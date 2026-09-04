//! HLS muxer (RFC 8216): segments the input, writes the media playlist, and
//! optionally a master playlist and an fMP4 initialization segment.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`write_access`] | Re-export of `vaco_format_adaptive::WriteAccess` — moved there once `vaco-mux-dash` needed the identical shape |
//! | [`counting`] | [`counting::CountingSink`] — a byte-counting `MediaSink` wrapper, for `hls_flags single_file` |
//! | [`filename`] | `-hls_segment_filename`'s `%d`/`%0Nd` template expansion |
//!
//! Segment container writing (MPEG-TS/fMP4 box layout) is **not** here — see
//! `vaco_format_adaptive::provider` for why, and [`HlsMuxer::new`]'s
//! `segments` parameter for the seam.
//!
//! # How it works
//!
//! Every reference-stream (stream index 0) keyframe past `hls_time` seconds
//! since the current segment started triggers a rotation: the current
//! segment's muxer gets `write_trailer`, its `#EXTINF`/byte-range/
//! `#EXT-X-PROGRAM-DATE-TIME` entry is appended to the live window, the
//! window is trimmed to `hls_list_size` (deleting the dropped file when
//! `hls_flags delete_segments` is set), and the media playlist is rewritten
//! from scratch. Every packet not on the reference stream is routed into
//! whichever segment is currently open.
//!
//! `hls_flags single_file` is a genuinely different code path: rather than
//! opening a fresh nested muxer per segment, one muxer is opened once for
//! the whole session and each rotation just records a
//! [`vaco_format_adaptive::ByteRange`] via [`counting::CountingSink`] instead
//! of closing anything.
//!
//! ## The registered entry point cannot really mux HLS
//!
//! `MuxerDesc::open`'s frozen signature is `fn(Box<dyn MediaSink>) ->
//! Result<Box<dyn Muxer>>` — one sink, no filename, no protocol write
//! access. HLS fundamentally needs to create *more than one* output (the
//! playlist gets rewritten on every rotation — which needs a fresh,
//! truncating open, not a seek-and-overwrite of a handle that might now be
//! shorter than what it replaces — plus one file per segment). [`MUXER`]'s
//! registered constructor therefore degrades: it accepts the one sink as the
//! playlist destination, writes to it exactly once (at
//! [`vaco_format_core::Muxer::write_trailer`]), and fails
//! [`vaco_format_core::Muxer::write_packet`] with
//! [`vaco_core::Error::Unsupported`] the moment a segment file would need to
//! be created — since there is nowhere given to create one. [`HlsMuxer::new`]
//! is the real entry point: it takes the playlist's own URL and a
//! [`WriteAccess`] (re-exported from `vaco_format_adaptive`) instead of a pre-opened sink, and creates
//! every file itself, truncating, exactly the way the reference does.
//!
//! # How to change it
//!
//! The six `hls_flags` this crate implements
//! (`single_file`, `temp_file`, `delete_segments`, `append_list`,
//! `program_date_time`, `independent_segments`) are exactly the ones the
//! brief named; the other ten the reference has
//! (`round_durations`, `discont_start`, `omit_endlist`, `split_by_time`,
//! `second_level_segment_*`, `periodic_rekey`, `iframes_only`) are not
//! implemented — see `docs/format/vaco-mux-hls.md` for the full list and why
//! each was left out.
//!
//! # Configuration
//!
//! [`HlsMuxOptions`] — names, types and defaults measured against `ffmpeg -h
//! muxer=hls` (ffmpeg 8.1), not recalled.
//!
//! # Dependencies
//!
//! `vaco-format-adaptive`, `vaco-protocol-core` (never a concrete protocol
//! crate), `vaco-format-core`, `vaco-io`, `vaco-limits`, `vaco-packet`,
//! `vaco-codec-core`, `vaco-opts`. Reaches MPEG-TS/fMP4 muxers only through
//! `SegmentMuxerProvider`, never directly.

#![forbid(unsafe_code)]

pub mod counting;
pub mod filename;

use std::collections::VecDeque;
use std::fmt::Write as _;

use vaco_codec_core::CodecParameters;
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_format_adaptive::{
    ByteRange, SegmentContainerHint, SegmentMuxerProvider, WallClock,
    walltime::format_iso8601_datetime,
};
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_packet::Packet;

pub use counting::CountingSink;
/// Re-exported from `vaco-format-adaptive`, where this moved once
/// `vaco-mux-dash` needed the identical shape. Kept as `write_access::` too,
/// for existing call sites.
pub mod write_access {
    pub use vaco_format_adaptive::WriteAccess;
}
pub use vaco_format_adaptive::WriteAccess;

bitflags::bitflags! {
    /// `-hls_flags`. Six of the reference's sixteen constants — the ones the
    /// brief named; see the crate docs for the rest.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct HlsFlags: u32 {
        /// Every segment lives in one physical file, addressed by
        /// `#EXT-X-BYTERANGE`.
        const SINGLE_FILE = 1 << 0;
        /// Write to `<name>.tmp` and rename over `<name>` once complete.
        const TEMP_FILE = 1 << 1;
        /// Delete a segment's file once it falls out of the live window.
        const DELETE_SEGMENTS = 1 << 2;
        /// Continue an existing playlist's `#EXT-X-MEDIA-SEQUENCE`/segment
        /// numbering rather than starting over at zero.
        const APPEND_LIST = 1 << 3;
        /// Emit `#EXT-X-PROGRAM-DATE-TIME` before each segment.
        const PROGRAM_DATE_TIME = 1 << 4;
        /// Emit `#EXT-X-INDEPENDENT-SEGMENTS` once, at the top.
        const INDEPENDENT_SEGMENTS = 1 << 5;
    }
}

/// `-hls_segment_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HlsSegmentType {
    #[default]
    MpegTs,
    Fmp4,
}

/// `-hls_playlist_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HlsPlaylistType {
    #[default]
    None,
    Event,
    Vod,
}

/// Muxer-level options. Names, types and defaults measured against
/// `ffmpeg -h muxer=hls` (ffmpeg 8.1).
#[derive(Debug, Clone)]
pub struct HlsMuxOptions {
    /// `-hls_time`: target segment length, in seconds. Default `2`.
    pub hls_time: f64,
    /// `-hls_list_size`: entries kept in the live playlist window. `0`
    /// means unlimited (every segment ever written stays listed). Default
    /// `5`.
    pub hls_list_size: usize,
    /// `-hls_segment_filename`: a `%d`/`%0Nd` template. `None` falls back to
    /// `<playlist-stem><index>.<ext>`.
    pub hls_segment_filename: Option<String>,
    pub hls_flags: HlsFlags,
    pub hls_playlist_type: HlsPlaylistType,
    pub hls_segment_type: HlsSegmentType,
    /// `-master_pl_name`: when set, a trivial one-variant master playlist is
    /// written alongside the media playlist at `write_trailer`.
    pub master_pl_name: Option<String>,
}

impl Default for HlsMuxOptions {
    fn default() -> Self {
        Self {
            hls_time: 2.0,
            hls_list_size: 5,
            hls_segment_filename: None,
            hls_flags: HlsFlags::empty(),
            hls_playlist_type: HlsPlaylistType::default(),
            hls_segment_type: HlsSegmentType::default(),
            master_pl_name: None,
        }
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match MuxerDesc::open's fn pointer type"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(HlsMuxer {
        playlist_url: String::new(),
        write: None,
        playlist_sink: Some(sink),
        segments: Box::new(vaco_format_adaptive::NoSegmentMuxers),
        opts: HlsMuxOptions::default(),
        stream_params: Vec::new(),
        header_written: false,
        trailer_written: false,
        media_sequence_base: 0,
        next_index: 0,
        written: VecDeque::new(),
        current: None,
        single_file_muxer: None,
        single_file_handle: None,
        single_file_url: None,
        last_ref_dts: None,
        last_ref_duration_us: 0,
    }))
}

/// The descriptor a registry would hold.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "hls",
    long_name: "Apple HTTP Live Streaming",
    extensions: &["m3u8"],
    default_video: Some(vaco_codec_core::CodecId::H264),
    default_audio: Some(vaco_codec_core::CodecId::Aac),
    open: open_muxer,
};

/// One finished segment's playlist entry.
#[derive(Debug, Clone)]
struct WrittenSegment {
    /// The literal name written into the playlist — never the resolved,
    /// writable URL (which may be an absolute local path or carry a
    /// scheme): RFC 8216 entries are ordinarily relative to the playlist.
    uri: String,
    duration: Duration,
    byte_range: Option<ByteRange>,
    program_date_time: Option<WallClock>,
}

/// State for the segment currently receiving packets.
struct CurrentSegment {
    uri: String,
    time_base: Option<Rational>,
    start_dts: Option<i64>,
    program_date_time: Option<WallClock>,
    byte_start: u64,
    /// `None` in single-file mode, where packets go to
    /// [`HlsMuxer::single_file_muxer`] instead.
    file_muxer: Option<Box<dyn Muxer>>,
}

/// The HLS muxer.
pub struct HlsMuxer {
    playlist_url: String,
    write: Option<WriteAccess>,
    /// Only set by the registered [`MUXER`] entry point's degraded mode;
    /// see the crate docs.
    playlist_sink: Option<Box<dyn MediaSink>>,
    segments: Box<dyn SegmentMuxerProvider>,
    opts: HlsMuxOptions,
    stream_params: Vec<CodecParameters>,
    header_written: bool,
    trailer_written: bool,
    media_sequence_base: u64,
    next_index: u64,
    written: VecDeque<WrittenSegment>,
    current: Option<CurrentSegment>,
    single_file_muxer: Option<Box<dyn Muxer>>,
    single_file_handle: Option<counting::SharedPosition>,
    single_file_url: Option<String>,
    last_ref_dts: Option<i64>,
    /// The most recent *non-zero* reference-stream packet duration, in
    /// microseconds (`Packet::duration`'s own unit, independent of any
    /// stream time base — see `vaco_core::Duration`'s doc). `EXTINF` used to
    /// be derived from `last_ref_dts - start_dts` alone — the span *to* the
    /// last packet's own timestamp rather than *through* it — so every
    /// segment (not only the last) came up one packet short, worst on a
    /// segment holding a single packet, where that made the listed duration
    /// zero before the `.max(1)` floor. `finish_current_segment` now adds
    /// this to the last packet's own timestamp before computing the span.
    /// Seeded from the reference stream's declared frame rate at
    /// [`Muxer::write_header`] so a source whose packets never state a
    /// duration at all still gets a real value, then kept current from the
    /// last packet that did — mirrors `vaco-mux-avi`'s
    /// `last_video_duration_ticks`.
    last_ref_duration_us: u64,
}

impl core::fmt::Debug for HlsMuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HlsMuxer")
            .field("streams", &self.stream_params.len())
            .field("written", &self.written.len())
            .field("segment_type", &self.opts.hls_segment_type)
            .finish_non_exhaustive()
    }
}

impl HlsMuxer {
    /// The real entry point: `playlist_url` is where the media playlist will
    /// be written (and, resolved against it, where every segment file and
    /// the optional master playlist go); `write` is `None` only for a
    /// caller that genuinely cannot produce anything beyond option
    /// validation (every `write_packet` will then fail).
    #[must_use]
    pub fn new(
        playlist_url: String,
        write: Option<WriteAccess>,
        segments: Box<dyn SegmentMuxerProvider>,
        opts: HlsMuxOptions,
    ) -> Self {
        let (media_sequence_base, next_index) = if opts.hls_flags.contains(HlsFlags::APPEND_LIST) {
            recover_append_state(write.as_ref(), &playlist_url)
        } else {
            (0, 0)
        };
        Self {
            playlist_url,
            write,
            playlist_sink: None,
            segments,
            opts,
            stream_params: Vec::new(),
            header_written: false,
            trailer_written: false,
            media_sequence_base,
            next_index,
            written: VecDeque::new(),
            current: None,
            single_file_muxer: None,
            single_file_handle: None,
            single_file_url: None,
            last_ref_dts: None,
            last_ref_duration_us: 0,
        }
    }

    fn single_file(&self) -> bool {
        self.opts.hls_flags.contains(HlsFlags::SINGLE_FILE)
    }

    fn segment_extension(&self) -> &'static str {
        match self.opts.hls_segment_type {
            HlsSegmentType::MpegTs => "ts",
            HlsSegmentType::Fmp4 => "m4s",
        }
    }

    fn segment_hint(&self) -> SegmentContainerHint {
        match self.opts.hls_segment_type {
            HlsSegmentType::MpegTs => SegmentContainerHint::MpegTs,
            HlsSegmentType::Fmp4 => SegmentContainerHint::Fmp4,
        }
    }

    /// Write `content` to `name` (already resolved against `self.playlist_url`
    /// is the caller's job — `name` here is the final I/O URL), honouring
    /// `hls_flags temp_file`.
    fn write_text_file(&self, url: &str, content: &str) -> Result<()> {
        let Some(write) = &self.write else {
            return Err(Error::Unsupported(
                "HLS output needs protocol write access, and none was supplied",
            ));
        };
        if self.opts.hls_flags.contains(HlsFlags::TEMP_FILE) {
            let tmp = format!("{url}.tmp");
            let mut sink = write.create(&tmp)?;
            sink.write(content.as_bytes())?;
            sink.flush()?;
            drop(sink);
            write.rename(&tmp, url)
        } else {
            let mut sink = write.create(url)?;
            sink.write(content.as_bytes())?;
            sink.flush()
        }
    }

    /// Rewrite the media playlist from scratch. Only called when
    /// `self.write` is `Some`; the degraded (registered-ctor) mode writes
    /// its one sink directly from `write_trailer` instead, once, since it
    /// has no way to create a second, truncating open of the same URL.
    fn rewrite_media_playlist(&self) -> Result<()> {
        let text = self.render_media_playlist();
        self.write_text_file(&self.playlist_url, &text)
    }

    fn render_media_playlist(&self) -> String {
        let uses_fmp4 = matches!(self.opts.hls_segment_type, HlsSegmentType::Fmp4);
        let uses_byterange =
            self.single_file() || self.written.iter().any(|s| s.byte_range.is_some());
        let version = if uses_fmp4 {
            7
        } else if uses_byterange {
            4
        } else {
            3
        };

        let target = self
            .written
            .iter()
            .map(|s| s.duration.as_micros())
            .max()
            .unwrap_or((self.opts.hls_time * 1_000_000.0) as i64);
        let target_secs = u64::try_from(target.max(0))
            .unwrap_or(0)
            .div_ceil(1_000_000)
            .max(1);

        let mut out = String::new();
        out.push_str("#EXTM3U\n");
        let _ = writeln!(out, "#EXT-X-VERSION:{version}");
        if self.opts.hls_flags.contains(HlsFlags::INDEPENDENT_SEGMENTS) {
            out.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");
        }
        let _ = writeln!(out, "#EXT-X-TARGETDURATION:{target_secs}");
        let _ = writeln!(out, "#EXT-X-MEDIA-SEQUENCE:{}", self.media_sequence_base);
        match self.opts.hls_playlist_type {
            HlsPlaylistType::Vod => out.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n"),
            HlsPlaylistType::Event => out.push_str("#EXT-X-PLAYLIST-TYPE:EVENT\n"),
            HlsPlaylistType::None => {}
        }
        if uses_fmp4 {
            out.push_str("#EXT-X-MAP:URI=\"init.mp4\"\n");
        }
        for seg in &self.written {
            if self.opts.hls_flags.contains(HlsFlags::PROGRAM_DATE_TIME)
                && let Some(pdt) = seg.program_date_time
            {
                let _ = writeln!(
                    out,
                    "#EXT-X-PROGRAM-DATE-TIME:{}",
                    format_iso8601_datetime(pdt)
                );
            }
            if let Some(range) = seg.byte_range {
                let _ = writeln!(out, "#EXT-X-BYTERANGE:{}@{}", range.length, range.offset);
            }
            let secs = seg.duration.as_micros() as f64 / 1_000_000.0;
            let _ = writeln!(out, "#EXTINF:{secs:.3},");
            out.push_str(&seg.uri);
            out.push('\n');
        }
        if self.trailer_written {
            out.push_str("#EXT-X-ENDLIST\n");
        }
        out
    }

    fn render_master_playlist(&self, media_uri: &str) -> String {
        let bandwidth = self
            .stream_params
            .first()
            .and_then(|p| p.bit_rate)
            .unwrap_or(1_000_000);
        format!("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH={bandwidth}\n{media_uri}\n")
    }

    fn reference_stream_index(packet: &Packet) -> bool {
        packet.stream_index == 0
    }

    fn elapsed_since_segment_start(seg: &CurrentSegment, packet: &Packet) -> Option<i64> {
        let tb = seg.time_base?;
        let start = seg.start_dts?;
        let now = packet.dts.ticks().or_else(|| packet.pts.ticks())?;
        let delta = now.saturating_sub(start);
        Timestamp::new(delta)
            .to_duration(tb)
            .map(Duration::as_micros)
    }

    fn should_rotate(&self, packet: &Packet) -> bool {
        if !Self::reference_stream_index(packet) || !packet.is_key() {
            return false;
        }
        let Some(seg) = &self.current else {
            return true;
        };
        let target_us = (self.opts.hls_time * 1_000_000.0) as i64;
        Self::elapsed_since_segment_start(seg, packet).is_some_and(|us| us >= target_us)
    }

    fn open_new_segment(&mut self) -> Result<()> {
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        let ext = self.segment_extension();
        let name = self.opts.hls_segment_filename.as_ref().map_or_else(
            || filename::default_name(&self.playlist_url, index, ext),
            |t| filename::expand(t, index),
        );
        let pdt = self
            .opts
            .hls_flags
            .contains(HlsFlags::PROGRAM_DATE_TIME)
            .then(WallClock::now)
            .flatten();

        if self.single_file() {
            // Captured *before* `write_header` when this is the first
            // segment: the container header (PAT/PMT, `ftyp`, ...) is written
            // exactly once, right here, and belongs to segment 0's byte
            // range — reading it after `write_header` instead excluded those
            // bytes from every range and left a gap at the front of the
            // file, caught by this crate's own `single_file_segments_are_
            // contiguous_non_overlapping_byte_ranges` test.
            let byte_start = if self.single_file_muxer.is_none() {
                let Some(write) = &self.write else {
                    return Err(Error::Unsupported(
                        "HLS single_file output needs protocol write access",
                    ));
                };
                let full_url = vaco_format_adaptive::resolve(&self.playlist_url, &name);
                let raw = write.create(&full_url)?;
                let (counted, handle) = CountingSink::new(raw);
                let start = handle.get();
                let mut m = self.segments.open_segment(
                    self.segment_hint(),
                    Box::new(counted),
                    &self.stream_params,
                    false,
                )?;
                m.init()?;
                m.write_header()?;
                self.single_file_muxer = Some(m);
                self.single_file_handle = Some(handle);
                self.single_file_url = Some(name.clone());
                start
            } else {
                self.single_file_handle
                    .as_ref()
                    .map_or(0, counting::SharedPosition::get)
            };
            let time_base = self
                .single_file_muxer
                .as_ref()
                .and_then(|m| m.stream_time_base(0));
            self.current = Some(CurrentSegment {
                uri: self.single_file_url.clone().unwrap_or(name),
                time_base,
                start_dts: None,
                program_date_time: pdt,
                byte_start,
                file_muxer: None,
            });
        } else {
            let Some(write) = &self.write else {
                return Err(Error::Unsupported(
                    "HLS output needs protocol write access, and none was supplied",
                ));
            };
            let full_url = vaco_format_adaptive::resolve(&self.playlist_url, &name);
            let sink = write.create(&full_url)?;
            let mut m = self.segments.open_segment(
                self.segment_hint(),
                sink,
                &self.stream_params,
                false,
            )?;
            m.init()?;
            m.write_header()?;
            let time_base = m.stream_time_base(0);
            self.current = Some(CurrentSegment {
                uri: name,
                time_base,
                start_dts: None,
                program_date_time: pdt,
                byte_start: 0,
                file_muxer: Some(m),
            });
        }
        Ok(())
    }

    fn finish_current_segment(&mut self) -> Result<()> {
        let Some(seg) = self.current.take() else {
            return Ok(());
        };
        let duration_us = match (seg.time_base, seg.start_dts, self.last_ref_dts) {
            (Some(tb), Some(start), Some(last)) => {
                let start_us = Timestamp::new(start)
                    .to_duration(tb)
                    .map_or(0, Duration::as_micros);
                let last_us = Timestamp::new(last)
                    .to_duration(tb)
                    .map_or(0, Duration::as_micros);
                let extra_us = i64::try_from(self.last_ref_duration_us).unwrap_or(i64::MAX);
                last_us.saturating_add(extra_us).saturating_sub(start_us)
            }
            _ => 0,
        };
        let byte_range = if self.single_file() {
            let end = self
                .single_file_handle
                .as_ref()
                .map_or(seg.byte_start, counting::SharedPosition::get);
            Some(ByteRange {
                offset: seg.byte_start,
                length: end.saturating_sub(seg.byte_start),
            })
        } else {
            None
        };
        if let Some(mut m) = seg.file_muxer {
            m.write_trailer()?;
        }
        self.written.push_back(WrittenSegment {
            uri: seg.uri,
            duration: Duration::from_micros(duration_us.max(1)),
            byte_range,
            program_date_time: seg.program_date_time,
        });
        self.trim_window();
        Ok(())
    }

    fn trim_window(&mut self) {
        let keep = if self.opts.hls_list_size == 0 {
            usize::MAX
        } else {
            self.opts.hls_list_size
        };
        while self.written.len() > keep {
            if let Some(dropped) = self.written.pop_front() {
                self.media_sequence_base = self.media_sequence_base.saturating_add(1);
                if self.opts.hls_flags.contains(HlsFlags::DELETE_SEGMENTS)
                    && let Some(write) = &self.write
                {
                    let full = vaco_format_adaptive::resolve(&self.playlist_url, &dropped.uri);
                    let _ = write.delete(&full); // best-effort, per the crate docs
                }
            }
        }
    }
}

/// `-hls_flags append_list`: recover a continuing `#EXT-X-MEDIA-SEQUENCE`
/// and segment-numbering start from an existing playlist at `url`, when one
/// can be read. Best-effort by design (see the crate docs): this recovers
/// numbering only, not the prior segments' entries themselves.
fn recover_append_state(write: Option<&WriteAccess>, url: &str) -> (u64, u64) {
    let Some(write) = write else {
        return (0, 0);
    };
    let Some(text) = write.read_to_string(url) else {
        return (0, 0);
    };
    let sequence = text
        .lines()
        .find_map(|l| l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let count = text.lines().filter(|l| l.starts_with("#EXTINF:")).count() as u64;
    (sequence, sequence.saturating_add(count))
}

impl Muxer for HlsMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "streams must be added before the header is written",
            ));
        }
        let index = u32::try_from(self.stream_params.len())
            .map_err(|_| Error::InvalidData("too many streams"))?;
        self.stream_params.push(params.clone());
        Ok(index)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("header written twice"));
        }
        if self.stream_params.is_empty() {
            return Err(Error::InvalidData("HLS output needs at least one stream"));
        }
        self.header_written = true;
        // Seed the fallback from the reference stream's own declared frame
        // rate, not an invented constant — `0` (its default) only when the
        // stream states no frame rate either, in which case there is
        // nothing to derive from until a real packet duration arrives.
        // Audio has no samples-per-frame field to derive one from.
        if let Some(video) = self.stream_params.first().and_then(|p| p.video.as_ref())
            && video.frame_rate.num > 0
            && video.frame_rate.den > 0
            && let (Ok(num), Ok(den)) = (
                u64::try_from(video.frame_rate.num),
                u64::try_from(video.frame_rate.den),
            )
            && num > 0
        {
            self.last_ref_duration_us = 1_000_000u64
                .saturating_mul(den)
                .checked_div(num)
                .unwrap_or(0);
        }
        if matches!(self.opts.hls_segment_type, HlsSegmentType::Fmp4)
            && let Some(write) = &self.write
        {
            let init_url = vaco_format_adaptive::resolve(&self.playlist_url, "init.mp4");
            let sink = write.create(&init_url)?;
            let mut m = self.segments.open_segment(
                SegmentContainerHint::Fmp4,
                sink,
                &self.stream_params,
                true,
            )?;
            m.init()?;
            m.write_header()?;
            m.write_trailer()?; // zero packets: exactly an init segment.
        }
        // With no write access at all (the registered ctor's degraded mode)
        // the init segment simply is not written; `write_packet` will fail
        // before that becomes observable either way.
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("packet written before the header"));
        }
        if usize::try_from(packet.stream_index).is_ok_and(|i| i >= self.stream_params.len()) {
            return Err(Error::InvalidData("packet names an unknown stream"));
        }

        if self.current.is_none() || self.should_rotate(packet) {
            self.finish_current_segment()?;
            self.open_new_segment()?;
        }

        if self.single_file() {
            let Some(m) = self.single_file_muxer.as_mut() else {
                return Err(Error::InvalidData("single_file segment was not opened"));
            };
            m.write_packet(packet)?;
        } else {
            let Some(seg) = self.current.as_mut() else {
                return Err(Error::InvalidData("no active segment"));
            };
            let Some(m) = seg.file_muxer.as_mut() else {
                return Err(Error::InvalidData("segment has no muxer"));
            };
            m.write_packet(packet)?;
        }

        if Self::reference_stream_index(packet)
            && let Some(ts) = packet.dts.ticks().or_else(|| packet.pts.ticks())
        {
            if let Some(seg) = self.current.as_mut()
                && seg.start_dts.is_none()
            {
                seg.start_dts = Some(ts);
            }
            self.last_ref_dts = Some(ts);
            // A packet that states no duration leaves the running hint at
            // its last real value — see the field's own doc.
            let stated_us = packet.duration.as_micros().max(0);
            if stated_us > 0 {
                self.last_ref_duration_us = stated_us as u64;
            }
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        if self.trailer_written {
            return Err(Error::InvalidData("trailer written twice"));
        }
        self.finish_current_segment()?;
        if let Some(mut m) = self.single_file_muxer.take() {
            m.write_trailer()?;
        }
        self.trailer_written = true;

        if let Some(mut sink) = self.playlist_sink.take() {
            // Degraded registered-ctor mode: one shot, into the one sink we
            // were given.
            let text = self.render_media_playlist();
            sink.write(text.as_bytes())?;
            sink.flush()?;
            return Ok(());
        }

        self.rewrite_media_playlist()?;
        if let Some(name) = self.opts.master_pl_name.clone() {
            let master_url = vaco_format_adaptive::resolve(&self.playlist_url, &name);
            let playlist_name = self
                .playlist_url
                .rsplit('/')
                .next()
                .unwrap_or(&self.playlist_url);
            let text = self.render_master_playlist(playlist_name);
            self.write_text_file(&master_url, &text)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
