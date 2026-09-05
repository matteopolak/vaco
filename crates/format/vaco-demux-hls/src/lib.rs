//! HLS demuxer (RFC 8216): master and media playlist parsing, variant
//! selection, and segment-by-segment reading through a nested MPEG-TS or
//! fragmented-MP4 demuxer.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`attrs`] | The `NAME=VALUE` attribute-list grammar every `#EXT-X-` tag with parameters uses |
//! | [`master`] | `#EXT-X-STREAM-INF` variants and `#EXT-X-MEDIA` alternate renditions |
//! | [`media`] | The segment list itself: `#EXTINF`, `#EXT-X-BYTERANGE`, `#EXT-X-MAP`, `#EXT-X-DISCONTINUITY`, `#EXT-X-PROGRAM-DATE-TIME`, `#EXT-X-PLAYLIST-TYPE`/`#EXT-X-ENDLIST` |
//! | [`key`] | `#EXT-X-KEY` — detected and reported, never decrypted |
//! | [`access`] | Re-export of `vaco_format_adaptive::RemoteAccess` — moved there once `vaco-demux-dash` needed the identical shape |
//!
//! Segment framing (MPEG-TS packetisation, fMP4 box parsing) is **not**
//! here — see [`vaco_format_adaptive::provider`] for why, and
//! [`HlsDemuxer::open`]'s `segments` parameter for the seam.
//!
//! # How it works
//!
//! [`HlsDemuxer::open`] reads the whole playlist (bounded — see
//! [`vaco_format_adaptive::read_all_bounded`]), and:
//!
//! * If it is a **master** playlist, [`master::parse`] extracts every variant
//!   and rendition, [`vaco_format_adaptive::select_variant`] picks one
//!   (highest bandwidth under `opts.max_bandwidth`, or the highest overall),
//!   and its media playlist is fetched and parsed in turn.
//! * A **media** playlist's segments are opened one at a time, in order, each
//!   through `segments: &dyn SegmentDemuxerProvider` — MPEG-TS when the
//!   segment carries no `#EXT-X-MAP`, fragmented MP4 when it does.
//!
//! Packets are re-timestamped onto one continuous per-stream timeline: each
//! nested demuxer restarts near whatever timestamps its own segment happens
//! to carry, and RFC 8216 promises continuity only *within* a
//! `#EXT-X-DISCONTINUITY`-delimited run. [`HlsDemuxer`] tracks, per stream
//! index, the end (last emitted `dts` plus that packet's own duration,
//! rescaled into the stream's time base) of the highest-timestamped packet
//! emitted so far, and — on the first packet following a discontinuity — an
//! offset that makes the new run continue exactly from there, with no
//! repeated or skipped tick at the boundary. See
//! `docs/format/vaco-demux-hls.md` for what this still does not handle (a
//! stream whose index/order actually changes across the discontinuity) and
//! why that is an acceptable gap for this phase.
//!
//! `#EXT-X-KEY` is detected and reported, never decrypted — see [`key`].
//!
//! # How to change it
//!
//! Add a new tag by extending [`master::parse`] or [`media::parse`]; both
//! already tolerate unrecognised tags (RFC 8216 §4.1), so a gap there is
//! silent unless a test catches it — which is exactly why both parsers are
//! tested against a playlist naming every tag they claim to support.
//!
//! # Configuration
//!
//! [`HlsOptions`]. Live playlist reloading (`-live_start_index`,
//! `-max_reload`, `-m3u8_hold_counters`) is **not implemented**: this crate
//! reads the segments a playlist lists at the moment it is opened and does
//! not poll for more. A live playlist parses correctly and
//! [`media::MediaPlaylist::is_live`] reports it, but `read_packet` simply
//! reaches `Eof` once the currently-known segments are exhausted, rather than
//! reloading. Recorded as deferred, not silently absent — see this crate's
//! docs file.
//!
//! # Dependencies
//!
//! `vaco-format-adaptive` (segment timeline model — used only where HLS's own
//! `EXTINF` sum needs a duration, not [`vaco_format_adaptive::timeline`]
//! itself, since HLS states no run-length encoding to expand), `vaco-protocol-core`
//! (W2: never a concrete protocol crate — see [`RemoteAccess`], re-exported from `vaco_format_adaptive`),
//! `vaco-format-core`, `vaco-io`, `vaco-limits`, `vaco-packet`,
//! `vaco-codec-core`. Reaches MPEG-TS/fMP4 demuxers only through
//! `SegmentDemuxerProvider`, never directly (see the crate-level report for
//! why `vaco-registry` cannot be a dependency here).

#![forbid(unsafe_code)]

pub mod attrs;
pub mod key;
pub mod master;
pub mod media;

use vaco_core::{Duration, Error, Result, Timestamp};
use vaco_format_adaptive::{
    BoundedSource, SegmentContainerHint, SegmentDemuxerProvider, select_variant,
};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, ProbeData, ProbeScore, SeekFlags,
    SeekTarget, Stream,
};
use vaco_io::MediaSource;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// Re-exported from `vaco-format-adaptive`, where this moved once
/// `vaco-demux-dash` needed the identical "owned, whitelist-gated URL
/// access kept alive for the demuxer's lifetime" shape. Kept as `access::`
/// too, for existing call sites.
pub mod access {
    pub use vaco_format_adaptive::RemoteAccess;
}
pub use key::KeyInfo;
pub use master::MasterPlaylist;
pub use media::{MapInfo, MediaPlaylist, MediaSegment, PlaylistType};
pub use vaco_format_adaptive::RemoteAccess;

/// Bytes a single playlist document may occupy in memory.
///
/// A media playlist naming a day of two-second segments is under 200 KiB of
/// text; 16 MiB is generous headroom above any real playlist while still
/// bounding a hostile server's `Content-Length` lie.
pub const MAX_MANIFEST_BYTES: u64 = 16 << 20;

/// Bytes an `#EXT-X-MAP` initialization segment may occupy.
pub const MAX_INIT_BYTES: u64 = 64 << 20;

/// Flags this container declares (see `vaco-format-core::flags`).
///
/// [`FormatFlags::TS_DISCONT`] because `#EXT-X-DISCONTINUITY` is a genuine,
/// named discontinuity — the exact case that flag exists to suppress the
/// generic monotonic-DTS repair for, since this crate already re-times across
/// one deliberately (see the module docs). [`FormatFlags::GENERIC_INDEX`]:
/// this crate builds no index of its own; [`FormatFlags::NOBINSEARCH`]: byte
/// position has no relationship to time across a segment boundary, so
/// bisection cannot work. [`FormatFlags::SHOW_IDS`] is not set: HLS has no
/// container-level stream id of its own (the nested demuxer's, if any, is
/// what would be shown).
pub const FLAGS: FormatFlags = FormatFlags::TS_DISCONT
    .union(FormatFlags::GENERIC_INDEX)
    .union(FormatFlags::NOBINSEARCH);

/// Content probe: an unambiguous `#EXTM3U` magic at the very start.
///
/// Deliberately not upgraded to [`ProbeScore::MAGIC_CHECKED`] on finding a
/// following `#EXT-X-` tag: a bare `.m3u`/`.m3u8` audio playlist (no HLS tags
/// at all) is a real, different, unregistered-here format, and claiming
/// [`ProbeScore::MAX`] on magic alone would make this demuxer's probe
/// indistinguishable from a confirmed one. [`ProbeScore::MAGIC`] already
/// outranks every non-magic-bearing competitor.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(b"#EXTM3U") {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

/// The registered entry point. See the crate docs' "Configuration" section
/// and the top-level report: `DemuxerDesc::open` carries no base URL and no
/// protocol registry, both of which HLS genuinely needs to open anything
/// beyond the bytes it was handed, so this degrades to "parse what was
/// given, fail informatively the moment more must be fetched" — see
/// [`HlsDemuxer::open`]'s `access: None` case — rather than a panic or a
/// silent wrong answer.
///
/// It also cannot forward the caller's `parsers`: `DemuxerDesc::open`'s
/// `&dyn ParserProvider` is borrowed for one call, but [`HlsDemuxer`] must
/// own a provider it can use across many later `read_packet` calls (a fresh
/// nested demuxer per segment), and there is no safe way to turn a borrow
/// into an owned `Box` here. [`vaco_format_core::discovery::NoParsers`]
/// stands in, so a demuxer opened through this path never gets
/// bitstream-parsed profile/`pix_fmt` information the way one opened through
/// [`HlsDemuxer::open`] with a real provider would.
fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(HlsDemuxer::open(
        src,
        "",
        None,
        Box::new(vaco_format_core::discovery::NoParsers),
        Box::new(vaco_format_adaptive::NoSegmentDemuxers),
        &HlsOptions::default(),
    )?))
}

/// The descriptor a registry would hold.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "hls",
    long_name: "Apple HTTP Live Streaming",
    extensions: &["m3u8", "m3u"],
    mime_types: &["application/vnd.apple.mpegurl", "application/x-mpegurl"],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

/// Demuxer-level options.
#[derive(Debug, Clone, Copy, Default)]
pub struct HlsOptions {
    /// Cap on the variant chosen from a master playlist, in bits per second.
    /// `None` selects the highest-bandwidth variant unconditionally —
    /// [`vaco_format_adaptive::select_variant`]'s own default.
    pub max_bandwidth: Option<u64>,
}

/// The HLS demuxer.
pub struct HlsDemuxer {
    streams: Vec<Stream>,
    playlist: MediaPlaylist,
    access: Option<RemoteAccess>,
    parsers: Box<dyn ParserProvider>,
    segments: Box<dyn SegmentDemuxerProvider>,
    segment_index: usize,
    current: Option<Box<dyn Demuxer>>,
    /// Last emitted (already-adjusted) `dts`, per stream index.
    last_dts: Vec<i64>,
    /// Last observed positive `dts` delta between consecutive packets on
    /// that stream — an estimate of "one packet's worth of time", since raw
    /// MPEG-TS packets carry no explicit duration field for this crate to
    /// read back. Used only to extrapolate where a following discontinuity's
    /// run should continue from.
    interval: Vec<i64>,
    /// Whether `last_dts`/`interval` hold a real value yet, per stream.
    seen: Vec<bool>,
    /// Per-stream additive offset applied to every emitted timestamp.
    offset_ticks: Vec<i64>,
    /// Set on every stream when a `#EXT-X-DISCONTINUITY` is crossed; cleared,
    /// per stream, the first time that stream's next packet is seen (which is
    /// when there is finally a raw timestamp to compute an offset from).
    reset_needed: Vec<bool>,
    eof: bool,
}

impl core::fmt::Debug for HlsDemuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HlsDemuxer")
            .field("streams", &self.streams.len())
            .field("segments", &self.playlist.segments.len())
            .field("segment_index", &self.segment_index)
            .field("live", &self.playlist.is_live())
            .finish_non_exhaustive()
    }
}

impl HlsDemuxer {
    /// Open `source` — the bytes of a master or media playlist already
    /// fetched from `url` — and, for a master playlist, fetch and parse the
    /// selected variant's media playlist too.
    ///
    /// `access = None` means "no way to fetch anything else": parsing a
    /// self-contained media playlist still succeeds, but a master playlist
    /// cannot be resolved (there is nowhere to fetch the chosen variant from)
    /// and reading any segment fails the moment one is needed. This is what
    /// [`DEMUXER`]'s registered entry point uses; see this crate's top-level
    /// report for why that gap exists in the frozen [`DemuxerDesc`] shape.
    ///
    /// # Errors
    /// [`Error::InvalidData`] for a malformed playlist; whatever `access`
    /// reports for a master playlist it cannot resolve.
    pub fn open(
        source: Box<dyn MediaSource>,
        url: &str,
        access: Option<RemoteAccess>,
        parsers: Box<dyn ParserProvider>,
        segments: Box<dyn SegmentDemuxerProvider>,
        opts: &HlsOptions,
    ) -> Result<Self> {
        let mut src = source;
        let mut budget = Budget::new(Limits::permissive());
        let bytes =
            vaco_format_adaptive::read_all_bounded(&mut *src, &mut budget, MAX_MANIFEST_BYTES)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidData("HLS playlist is not valid UTF-8"))?;

        let playlist = if text.contains("#EXT-X-STREAM-INF") {
            let masterlist = master::parse(text, url)?;
            let variant = select_variant(&masterlist.variants, opts.max_bandwidth).ok_or(
                Error::InvalidData("HLS master playlist names no usable variant"),
            )?;
            let Some(access) = &access else {
                return Err(Error::Unsupported(
                    "HLS master playlist needs a variant fetched, and no protocol access was supplied",
                ));
            };
            let mut sub = access.open(&variant.uri)?;
            let sub_bytes =
                vaco_format_adaptive::read_all_bounded(&mut *sub, &mut budget, MAX_MANIFEST_BYTES)?;
            let sub_text = std::str::from_utf8(&sub_bytes)
                .map_err(|_| Error::InvalidData("HLS media playlist is not valid UTF-8"))?;
            media::parse(sub_text, &variant.uri)?
        } else {
            media::parse(text, url)?
        };

        Ok(Self {
            streams: Vec::new(),
            playlist,
            access,
            parsers,
            segments,
            segment_index: 0,
            current: None,
            last_dts: Vec::new(),
            interval: Vec::new(),
            seen: Vec::new(),
            offset_ticks: Vec::new(),
            reset_needed: Vec::new(),
            eof: false,
        })
    }

    /// The parsed media playlist this demuxer is reading — for a caller that
    /// wants `is_live`, segment metadata, or the key table without going
    /// through the trait-object [`Demuxer`] interface.
    #[must_use]
    pub const fn playlist(&self) -> &MediaPlaylist {
        &self.playlist
    }

    fn open_next_segment(&mut self) -> Result<()> {
        let Some(seg) = self.playlist.segments.get(self.segment_index).cloned() else {
            self.eof = true;
            return Err(Error::Eof);
        };
        self.segment_index = self.segment_index.saturating_add(1);

        if let Some(key_idx) = seg.key {
            let key = self
                .playlist
                .keys
                .get(key_idx)
                .ok_or(Error::InvalidData("segment names an unknown #EXT-X-KEY"))?;
            return Err(key.unsupported_error());
        }

        let Some(access) = &self.access else {
            return Err(Error::Unsupported(
                "HLS segment fetch needs protocol access, and none was supplied",
            ));
        };

        let raw = access.open(&seg.uri)?;
        let src: Box<dyn MediaSource> = match seg.byte_range {
            Some(range) => Box::new(BoundedSource::new(raw, range)?),
            None => raw,
        };

        let init = match seg.map {
            Some(map_idx) => {
                let map = self
                    .playlist
                    .maps
                    .get(map_idx)
                    .ok_or(Error::InvalidData("segment names an unknown #EXT-X-MAP"))?
                    .clone();
                let mraw = access.open(&map.uri)?;
                let mut msrc: Box<dyn MediaSource> = match map.byte_range {
                    Some(range) => Box::new(BoundedSource::new(mraw, range)?),
                    None => mraw,
                };
                let mut init_budget = Budget::new(Limits::permissive());
                Some(vaco_format_adaptive::read_all_bounded(
                    &mut *msrc,
                    &mut init_budget,
                    MAX_INIT_BYTES,
                )?)
            }
            None => None,
        };

        let hint = if seg.map.is_some() {
            SegmentContainerHint::Fmp4
        } else {
            SegmentContainerHint::MpegTs
        };
        let nested =
            self.segments
                .open_segment(hint, init.as_deref(), src, self.parsers.as_ref())?;

        if self.streams.is_empty() && !nested.streams().is_empty() {
            self.streams = nested.streams().to_vec();
            self.last_dts = vec![0i64; self.streams.len()];
            self.interval = vec![0i64; self.streams.len()];
            self.seen = vec![false; self.streams.len()];
            self.offset_ticks = vec![0i64; self.streams.len()];
            self.reset_needed = vec![false; self.streams.len()];
        }
        if seg.discontinuity {
            for flag in &mut self.reset_needed {
                *flag = true;
            }
        }
        self.current = Some(nested);
        Ok(())
    }

    /// Rebase one packet onto the continuous per-stream timeline, per the
    /// module docs' "how it works" section.
    ///
    /// Extrapolates "where the next packet should land" from the last
    /// observed `dts` delta on that stream, rather than from
    /// [`Packet::duration`]: a raw MPEG-TS packet carries no explicit
    /// duration for [`vaco_demux_mpegts::MpegTsDemuxer`] to report back, so
    /// that field reads zero in practice and using it produced an exact
    /// duplicate timestamp at every discontinuity boundary — caught by this
    /// crate's own integration test, which is why this is the shape it is
    /// rather than the more obviously "correct"-looking one.
    fn apply_continuity(&mut self, pkt: &mut Packet) {
        let Ok(idx) = usize::try_from(pkt.stream_index) else {
            return;
        };
        if idx >= self.offset_ticks.len() {
            return; // A stream this demuxer never declared; leave untouched.
        }
        if self.reset_needed.get(idx).copied().unwrap_or(false) {
            let raw = pkt.dts.ticks().or_else(|| pkt.pts.ticks());
            if let Some(raw) = raw {
                let last = self.last_dts.get(idx).copied().unwrap_or(0);
                let interval = self.interval.get(idx).copied().unwrap_or(0);
                let target = last.saturating_add(interval);
                if let Some(slot) = self.offset_ticks.get_mut(idx) {
                    *slot = target.saturating_sub(raw);
                }
                if let Some(slot) = self.reset_needed.get_mut(idx) {
                    *slot = false;
                }
            }
        }
        let offset = self.offset_ticks.get(idx).copied().unwrap_or(0);
        if let Some(t) = pkt.dts.ticks() {
            let adjusted = t.saturating_add(offset);
            pkt.dts = Timestamp::new(adjusted);
            if self.seen.get(idx).copied().unwrap_or(false) {
                let prev = self.last_dts.get(idx).copied().unwrap_or(adjusted);
                let delta = adjusted.saturating_sub(prev);
                if delta > 0
                    && let Some(slot) = self.interval.get_mut(idx)
                {
                    *slot = delta;
                }
            } else if let Some(slot) = self.seen.get_mut(idx) {
                *slot = true;
            }
            if let Some(slot) = self.last_dts.get_mut(idx) {
                *slot = adjusted;
            }
        }
        if let Some(t) = pkt.pts.ticks() {
            pkt.pts = Timestamp::new(t.saturating_add(offset));
        }
    }
}

impl Demuxer for HlsDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        loop {
            if self.current.is_none() {
                if self.eof {
                    return Err(Error::Eof);
                }
                self.open_next_segment()?;
            }
            let Some(cur) = self.current.as_mut() else {
                return Err(Error::Eof);
            };
            match cur.read_packet() {
                Ok(mut pkt) => {
                    self.apply_continuity(&mut pkt);
                    return Ok(pkt);
                }
                Err(Error::Eof) => {
                    self.current = None;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let _ = flags;
        let SeekTarget::Timestamp { stream_index, ts } = target else {
            return Err(Error::NotSeekable);
        };
        let stream = usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.streams.get(i))
            .ok_or(Error::InvalidData("seek names an unknown stream"))?;
        let target = ts.to_duration(stream.time_base).ok_or(Error::NotSeekable)?;

        let mut cursor = Duration::ZERO;
        let mut landing = self.playlist.segments.len().saturating_sub(1);
        for (i, seg) in self.playlist.segments.iter().enumerate() {
            let end = cursor.checked_add(seg.duration).ok_or(Error::NotSeekable)?;
            if target < end {
                landing = i;
                break;
            }
            cursor = end;
        }

        self.segment_index = landing;
        self.current = None;
        self.eof = false;
        for flag in &mut self.reset_needed {
            *flag = true;
        }
        for e in &mut self.last_dts {
            *e = 0;
        }
        for s in &mut self.seen {
            *s = false;
        }
        Ok(())
    }

    fn duration(&self) -> Option<Duration> {
        if self.playlist.is_live() {
            return None;
        }
        self.playlist
            .segments
            .iter()
            .try_fold(Duration::ZERO, |total, segment| {
                total.checked_add(segment.duration)
            })
    }
}
