//! DASH demuxer (ISO/IEC 23009-1): MPD parsing with `quick-xml`,
//! representation selection, and segment-by-segment reading through a nested
//! fMP4 (or MPEG-TS) demuxer.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`tree`] | A generic, bounded XML tree — the one `quick-xml` pass every element parser walks afterwards |
//! | [`mpd`] | The MPD semantic model: `Period` > `AdaptationSet` > `Representation`, `$Number$`/`$Time$` substitution |
//! | [`segments`] | Turning one representation's addressing (`SegmentTemplate`/`SegmentList`/`SegmentBase`) into an ordered segment list |
//!
//! Segment container parsing (fMP4 box layout, MPEG-TS packetisation) is
//! **not** here — see `vaco_format_adaptive::provider`, and
//! [`DashDemuxer::open`]'s `segments` parameter for the seam.
//!
//! # How it works
//!
//! [`DashDemuxer::open`] reads the whole MPD (bounded —
//! [`vaco_format_adaptive::read_all_bounded`]), parses it with
//! [`tree::parse`] and [`mpd::interpret`], and — **only the first `Period`**
//! (see "What this crate does not do" below) — collects every
//! `Representation` across every `AdaptationSet` into
//! [`vaco_format_adaptive::select_variant`]'s candidate list, which picks one
//! (highest bandwidth under `DashOptions::max_bandwidth`, or highest
//! overall). [`segments::enumerate`] turns the chosen representation's
//! addressing into an ordered list of segments, each opened in turn through
//! `segments: &dyn SegmentDemuxerProvider`.
//!
//! Unlike `vaco-demux-hls`, packets are **not** re-timestamped across
//! segment boundaries: fMP4/CMAF fragments carry an absolute `tfdt` base
//! decode time each, so a correctly-authored DASH representation is already
//! continuous across its own segments without this crate doing anything —
//! there is no DASH equivalent of `#EXT-X-DISCONTINUITY` within one
//! representation's segment list.
//!
//! `ContentProtection` is detected and reported, never decrypted — see
//! [`mpd::ContentProtectionInfo`]; a representation whose `AdaptationSet`
//! carries one fails [`vaco_format_core::Demuxer::read_packet`] with
//! [`vaco_core::Error::Unsupported`] before any segment is opened.
//!
//! ## What this crate does not do
//!
//! - **Only the MPD's first `Period` is read.** A multi-period MPD (ad
//!   breaks, a live stream's stitched-together history) is common in
//!   practice; concatenating periods onto one continuous timeline needs each
//!   period's own `@start` applied as a presentation-time offset, which nets
//!   out to exactly the same "shift a nested demuxer's native timestamps
//!   onto a continuous outer timeline" problem `vaco-demux-hls` already
//!   solves for `#EXT-X-DISCONTINUITY` — reusing that shape across periods is
//!   the natural next step and is recorded here rather than attempted under
//!   this wave's time budget.
//! - **`SegmentBase`'s `sidx` is not parsed.** A `SegmentBase`-addressed
//!   representation (DASH's on-demand profile: one whole file, index-only
//!   `sidx` for seeking) is reported as a single segment covering the whole
//!   file rather than one segment per `sidx` entry. Still a correct read,
//!   just not byte-accurately sub-segmented.
//! - **Live (`dynamic`) MPDs with no `SegmentTimeline` and no period
//!   duration enumerate to zero segments.** Computing which segments exist
//!   *right now* needs `availabilityStartTime` compared against the wall
//!   clock — genuinely live behaviour, explicitly outside this project's
//!   byte-exact corpus (plan 13 §1b). `mpd::Mpd::presentation_type` is parsed
//!   correctly regardless.
//!
//! # How to change it
//!
//! `segments::enumerate` is the seam between "what the MPD says" and "what
//! gets read": all three addressing modes funnel through it, and it is what
//! a fourth addressing wrinkle (`SegmentTemplate` + `SegmentList` mixed —
//! not valid DASH, but seen from encoders that get it wrong) should extend.
//!
//! # Configuration
//!
//! [`DashOptions`].
//!
//! # Dependencies
//!
//! `quick-xml` (the only MPD dependency; all interpretation is this crate's
//! own — see `vaco-demux-hls`'s "a name in the reference is not a
//! specification" for why that distinction matters), `vaco-format-adaptive`,
//! `vaco-protocol-core` (never a concrete protocol crate), `vaco-format-core`,
//! `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-codec-core`. Reaches
//! fMP4/MPEG-TS demuxers only through `SegmentDemuxerProvider`, never
//! directly.

#![forbid(unsafe_code)]

pub mod mpd;
pub mod segments;
pub mod tree;

use vaco_core::{Duration, Error, Result};
use vaco_format_adaptive::{
    RemoteAccess, SegmentContainerHint, SegmentDemuxerProvider, Variant, select_variant,
};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, ProbeData, ProbeScore, SeekFlags,
    SeekTarget, Stream,
};
use vaco_io::MediaSource;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

pub use mpd::{ContentProtectionInfo, Mpd, PresentationType};
pub use segments::DashSegment;

/// Bytes an MPD document may occupy in memory.
pub const MAX_MANIFEST_BYTES: u64 = 16 << 20;
/// Bytes an initialization segment may occupy.
pub const MAX_INIT_BYTES: u64 = 64 << 20;

/// Flags this container declares. [`FormatFlags::GENERIC_INDEX`]: no index
/// of its own. [`FormatFlags::NOBINSEARCH`]/[`FormatFlags::NO_BYTE_SEEK`]:
/// byte position has no relationship to time across a segment boundary.
/// **Not** [`FormatFlags::TS_DISCONT`] — unlike HLS, a single representation's
/// segments are continuous by construction (see the module docs), so the
/// generic monotonic-DTS repair should stay active rather than be suppressed.
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX
    .union(FormatFlags::NOBINSEARCH)
    .union(FormatFlags::NO_BYTE_SEEK);

/// Content probe: an `<MPD` start tag, allowing for an XML declaration and/or
/// leading whitespace/BOM before it — a plain `starts_with` would miss the
/// overwhelmingly common `<?xml version="1.0"?>\n<MPD ...>` shape.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.find(b"<MPD", 0, 512).is_some() {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    // See the crate docs: `DemuxerDesc::open` carries no base URL and no
    // protocol registry, and DASH needs both to fetch anything beyond the
    // MPD bytes it was handed — the same gap `vaco-demux-hls` documents.
    Ok(Box::new(DashDemuxer::open(
        src,
        "",
        None,
        Box::new(vaco_format_core::discovery::NoParsers),
        Box::new(vaco_format_adaptive::NoSegmentDemuxers),
        &DashOptions::default(),
    )?))
}

/// The descriptor a registry would hold.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "dash",
    long_name: "Dynamic Adaptive Streaming over HTTP",
    extensions: &["mpd"],
    mime_types: &["application/dash+xml"],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

/// Demuxer-level options.
#[derive(Debug, Clone, Copy, Default)]
pub struct DashOptions {
    /// Cap on the representation chosen, in bits per second. `None` selects
    /// the highest-bandwidth representation unconditionally.
    pub max_bandwidth: Option<u64>,
}

/// The DASH demuxer.
pub struct DashDemuxer {
    streams: Vec<Stream>,
    mpd: Mpd,
    content_protection: Vec<ContentProtectionInfo>,
    init_bytes: Option<Vec<u8>>,
    hint: SegmentContainerHint,
    segment_list: Vec<DashSegment>,
    access: Option<RemoteAccess>,
    parsers: Box<dyn ParserProvider>,
    provider: Box<dyn SegmentDemuxerProvider>,
    segment_index: usize,
    current: Option<Box<dyn Demuxer>>,
    eof: bool,
}

impl core::fmt::Debug for DashDemuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DashDemuxer")
            .field("streams", &self.streams.len())
            .field("segments", &self.segment_list.len())
            .field("segment_index", &self.segment_index)
            .field("presentation_type", &self.mpd.presentation_type)
            .finish_non_exhaustive()
    }
}

fn representation_hint(rep: &mpd::Representation) -> SegmentContainerHint {
    let mime = rep.mime_type.as_deref().unwrap_or_default();
    if mime.contains("mp2t") {
        SegmentContainerHint::MpegTs
    } else {
        SegmentContainerHint::Fmp4
    }
}

fn representation_as_variant(
    rep: &mpd::Representation,
    period_index: usize,
    adaptation_index: usize,
    representation_index: usize,
) -> Variant {
    Variant {
        bandwidth: rep.bandwidth,
        average_bandwidth: None,
        width: rep.width,
        height: rep.height,
        frame_rate: rep.frame_rate,
        codecs: rep.codecs.clone(),
        uri: format!("{period_index}/{adaptation_index}/{representation_index}"),
        id: Some(rep.id.clone()),
    }
}

impl DashDemuxer {
    /// Open `source` — the bytes of an MPD already fetched from `url` — pick
    /// a representation, and enumerate its segments.
    ///
    /// `access = None` means "no way to fetch anything else": the MPD still
    /// parses, and [`DashDemuxer::mpd`] is inspectable, but reading any
    /// segment fails informatively.
    ///
    /// # Errors
    /// [`Error::InvalidData`] for a malformed MPD;
    /// [`Error::Unsupported`] when the MPD names no usable representation.
    pub fn open(
        source: Box<dyn MediaSource>,
        url: &str,
        access: Option<RemoteAccess>,
        parsers: Box<dyn ParserProvider>,
        provider: Box<dyn SegmentDemuxerProvider>,
        opts: &DashOptions,
    ) -> Result<Self> {
        let mut src = source;
        let mut budget = Budget::new(Limits::permissive());
        let bytes =
            vaco_format_adaptive::read_all_bounded(&mut *src, &mut budget, MAX_MANIFEST_BYTES)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidData("DASH MPD is not valid UTF-8"))?;
        let tree = tree::parse(text, &mut budget)?;
        let mpd = mpd::interpret(&tree)?;

        let period = mpd
            .periods
            .first()
            .ok_or(Error::Unsupported("DASH MPD names no Period"))?;

        let mut candidates: Vec<(usize, usize, Variant)> = Vec::new();
        for (ai, aset) in period.adaptation_sets.iter().enumerate() {
            for (ri, rep) in aset.representations.iter().enumerate() {
                candidates.push((ai, ri, representation_as_variant(rep, 0, ai, ri)));
            }
        }
        let variants: Vec<Variant> = candidates.iter().map(|(_, _, v)| v.clone()).collect();
        let chosen = select_variant(&variants, opts.max_bandwidth).ok_or(Error::Unsupported(
            "DASH MPD names no usable Representation",
        ))?;
        let chosen_uri = chosen.uri.clone();
        let (ai, ri) = candidates
            .iter()
            .find(|(_, _, v)| v.uri == chosen_uri)
            .map(|(ai, ri, _)| (*ai, *ri))
            .ok_or(Error::Unsupported("internal: selected variant vanished"))?;

        let aset = period
            .adaptation_sets
            .get(ai)
            .ok_or(Error::Unsupported("DASH adaptation set index out of range"))?;
        let rep = aset
            .representations
            .get(ri)
            .ok_or(Error::Unsupported("DASH representation index out of range"))?;

        let base_url = vaco_format_adaptive::resolve(url, mpd.base_url.as_deref().unwrap_or(""));
        let base_url =
            vaco_format_adaptive::resolve(&base_url, period.base_url.as_deref().unwrap_or(""));
        let base_url =
            vaco_format_adaptive::resolve(&base_url, aset.base_url.as_deref().unwrap_or(""));

        let period_end = period.duration.or(mpd.media_presentation_duration);

        let (init, seg_list) = segments::enumerate(rep, &base_url, period_end, &mut budget)?;
        let hint = representation_hint(rep);
        let content_protection = aset.content_protection.clone();

        let init_bytes = match (&access, &init) {
            (Some(access), Some(init)) => {
                let raw = access.open(&init.uri)?;
                let src: Box<dyn MediaSource> = match init.byte_range {
                    Some(range) => Box::new(vaco_format_adaptive::BoundedSource::new(raw, range)?),
                    None => raw,
                };
                let mut src = src;
                let mut b = Budget::new(Limits::permissive());
                Some(vaco_format_adaptive::read_all_bounded(
                    &mut *src,
                    &mut b,
                    MAX_INIT_BYTES,
                )?)
            }
            _ => None,
        };

        Ok(Self {
            streams: Vec::new(),
            mpd,
            content_protection,
            init_bytes,
            hint,
            segment_list: seg_list,
            access,
            parsers,
            provider,
            segment_index: 0,
            current: None,
            eof: false,
        })
    }

    /// The parsed MPD, for a caller wanting `presentation_type`,
    /// `availability_start_time`, or anything else beyond the trait-object
    /// [`Demuxer`] interface.
    #[must_use]
    pub const fn mpd(&self) -> &Mpd {
        &self.mpd
    }

    /// `ContentProtection` entries the chosen `AdaptationSet` declared, if
    /// any — see the module docs.
    #[must_use]
    pub fn content_protection(&self) -> &[ContentProtectionInfo] {
        &self.content_protection
    }

    fn open_next_segment(&mut self) -> Result<()> {
        if let Some(cp) = self.content_protection.first() {
            return Err(cp.unsupported_error());
        }
        let Some(seg) = self.segment_list.get(self.segment_index).cloned() else {
            self.eof = true;
            return Err(Error::Eof);
        };
        self.segment_index = self.segment_index.saturating_add(1);
        let Some(access) = &self.access else {
            return Err(Error::Unsupported(
                "DASH segment fetch needs protocol access, and none was supplied",
            ));
        };
        let raw = access.open(&seg.uri)?;
        let src: Box<dyn MediaSource> = match seg.byte_range {
            Some(range) => Box::new(vaco_format_adaptive::BoundedSource::new(raw, range)?),
            None => raw,
        };
        let nested = self.provider.open_segment(
            self.hint,
            self.init_bytes.as_deref(),
            src,
            self.parsers.as_ref(),
        )?;
        if self.streams.is_empty() && !nested.streams().is_empty() {
            self.streams = nested.streams().to_vec();
        }
        self.current = Some(nested);
        Ok(())
    }
}

impl Demuxer for DashDemuxer {
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
                Ok(pkt) => return Ok(pkt),
                Err(Error::Eof) => self.current = None,
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
        let mut landing = self.segment_list.len().saturating_sub(1);
        for (i, seg) in self.segment_list.iter().enumerate() {
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
        Ok(())
    }

    fn duration(&self) -> Option<Duration> {
        if matches!(self.mpd.presentation_type, PresentationType::Dynamic) {
            return None;
        }
        self.segment_list
            .iter()
            .try_fold(Duration::ZERO, |total, segment| {
                total.checked_add(segment.duration)
            })
    }
}
