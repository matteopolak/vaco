//! `mkvtimestamp_v2`: a plain-text timestamp dump, one line per frame.
//!
//! # Measured against the reference
//!
//! `ffmpeg -f lavfi -i testsrc=r=25:d=1 -f mkvtimestamp_v2 -`, byte-inspected
//! with `od -c` (ffmpeg 8.1, `LC_ALL=C`):
//!
//! ```text
//! # timecode format v2
//! 0
//! 40
//! 80
//! ⋮
//! 960
//! ```
//!
//! * The header is exactly `"# timecode format v2\n"`, written once, even if
//!   zero frames follow.
//! * Every following line is one decimal integer: the frame's presentation
//!   timestamp in **milliseconds**, LF-terminated, no padding, no sign for a
//!   non-negative value.
//! * Rounding is to the nearest millisecond. Confirmed against a
//!   `24000/1001` stream (`0, 42, 83, 125, 167, …`): frame 1's exact time is
//!   41.7083ms → 42; frame 3's is 125.125ms → 125. Both round to the nearest
//!   integer, not truncate.
//!
//! This crate does not reimplement that rounding. [`MkvTimestampV2Muxer`]
//! declares [`Muxer::stream_time_base`] as `1/1000`, and
//! [`vaco_format_core::mux`]'s M1 rescale step
//! ([`vaco_format_core::interleave::MuxTimestamps::apply`]) converts every
//! incoming packet's `pts` into that base with
//! [`vaco_core::Rounding::NearestAwayFromZero`] (the framework default)
//! *before* [`Muxer::write_packet`] ever sees it — so the value already
//! arriving in `packet.pts` **is** the millisecond count to print, and a
//! second rounding pass in this crate would only be a second place for the
//! same rounding to disagree.
//!
//! # Single video stream only
//!
//! Measured: muxing a lone `sine` (audio) stream produces
//! `Output file does not contain any stream` — an audio-only file is
//! rejected outright. Muxing one video **and** one audio stream together
//! produces `More than one stream unsupported` for the packets belonging to
//! whichever stream did not win the muxer's single internal slot, repeated
//! once per rejected packet; the *file's own content* in that probe did not
//! resolve into a single unambiguous "which stream wins" rule under
//! `-map 0:v -map 1:a` ordering, and two video streams together produced a
//! header-only file with **no** data lines at all — a third, still different
//! outcome. Rather than encode a guess at whichever tie-break explains all
//! three, this crate takes the conservative, unambiguous reading the
//! single-stream case supports outright: [`MkvTimestampV2Muxer::add_stream`]
//! accepts exactly one stream, and it must be video; every other case is
//! [`vaco_core::Error::Unsupported`] at `add_stream` time rather than a
//! per-packet warning. This is a deliberate, documented divergence from the
//! reference's more lenient (and, on this measurement, inconsistent) runtime
//! behaviour — see `docs/format/vaco-mux-utility.md`.

use core::fmt::Write as _;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

/// The header line, written once by [`Muxer::write_header`].
pub const HEADER_LINE: &str = "# timecode format v2\n";

/// The millisecond time base every accepted stream is forced into.
const MS_TIME_BASE: Rational = Rational::new(1, 1000);

/// `mkvtimestamp_v2`: one line per video frame, the frame's PTS in
/// milliseconds.
#[derive(Debug)]
pub struct MkvTimestampV2Muxer {
    out: IoWriter,
    has_stream: bool,
}

impl MkvTimestampV2Muxer {
    /// # Errors
    /// As [`IoWriter::new`].
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            has_stream: false,
        })
    }
}

impl Muxer for MkvTimestampV2Muxer {
    fn flags(&self) -> FormatFlags {
        FLAGS
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.has_stream {
            return Err(Error::Unsupported(
                "mkvtimestamp_v2: more than one stream unsupported",
            ));
        }
        if params.media_type != Some(MediaType::Video) {
            return Err(Error::Unsupported(
                "mkvtimestamp_v2: only a single video stream can be muxed",
            ));
        }
        self.has_stream = true;
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        self.out.write(HEADER_LINE.as_bytes())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if packet.stream_index != 0 {
            // No second stream was ever accepted by `add_stream`, so this can
            // only be a caller driving the trait directly and out of the
            // contract `MuxBuilder`/`MuxWriter` enforce. Dropping rather than
            // panicking (`indexing_slicing`/`panic` are denied workspace-wide).
            return Ok(());
        }
        let Some(ms) = packet.pts.ticks() else {
            return Err(Error::Unsupported(
                "mkvtimestamp_v2: a packet with no pts cannot be timestamped",
            ));
        };
        let mut line = String::with_capacity(16);
        let _ = writeln!(line, "{ms}");
        self.out.write(line.as_bytes())
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.out.flush()
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        (stream_index == 0 && self.has_stream).then_some(MS_TIME_BASE)
    }
}

/// The container flags `-f mkvtimestamp_v2` is declared with.
///
/// `NOSTREAMS` is deliberately absent: measured, a file with zero video
/// streams is rejected (`Output file does not contain any stream`), not
/// accepted as an empty-but-valid file. `TS_NONSTRICT` is set because the
/// format is a monotonically-increasing dump, not a codec bitstream with its
/// own DTS discipline — same reasoning `vaco-mux-hash`'s frame-per-line
/// muxers use.
pub const FLAGS: FormatFlags = FormatFlags::NOFILE
    .union(FormatFlags::TS_NONSTRICT)
    .union(FormatFlags::TS_NEGATIVE);

/// # Errors
/// As [`MkvTimestampV2Muxer::new`].
fn open_mkvtimestamp(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(MkvTimestampV2Muxer::new(sink)?))
}

/// `mkvtimestamp_v2`: `ffmpeg -h muxer=mkvtimestamp_v2` names it "extract pts
/// as timecode v2 format, as defined by mkvtoolnix" and declares only a
/// default video codec (`rawvideo`) — no default audio, matching this
/// muxer's video-only contract.
pub static MUXER_MKVTIMESTAMP_V2: MuxerDesc = MuxerDesc {
    name: "mkvtimestamp_v2",
    long_name: "extract pts as timecode v2 format, as defined by mkvtoolnix",
    extensions: &[],
    default_video: Some(CodecId::Rawvideo),
    default_audio: None,
    open: open_mkvtimestamp,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::Timestamp;
    use vaco_format_core::vacoraw::{MemorySink, SharedBytes};
    use vaco_limits::{Budget, Limits};

    fn params(media: MediaType) -> CodecParameters {
        CodecParameters::new(media)
    }

    fn packet_at(pts_ms: i64) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        let mut p = Packet::from_slice(&mut budget, b"x").unwrap();
        p.stream_index = 0;
        p.pts = Timestamp::new(pts_ms);
        p
    }

    fn text(shared: &SharedBytes) -> String {
        String::from_utf8(shared.snapshot()).unwrap()
    }

    #[test]
    fn header_is_written_even_with_zero_frames() {
        let sink = MemorySink::new();
        let snapshot = sink.shared();
        let mut m = MkvTimestampV2Muxer::new(Box::new(sink)).unwrap();
        m.add_stream(&params(MediaType::Video)).unwrap();
        m.write_header().unwrap();
        m.write_trailer().unwrap();
        assert_eq!(text(&snapshot), HEADER_LINE);
    }

    #[test]
    fn one_line_per_frame_in_milliseconds() {
        let sink = MemorySink::new();
        let snapshot = sink.shared();
        let mut m = MkvTimestampV2Muxer::new(Box::new(sink)).unwrap();
        m.add_stream(&params(MediaType::Video)).unwrap();
        m.write_header().unwrap();
        for ms in [0, 40, 80, 960] {
            m.write_packet(&packet_at(ms)).unwrap();
        }
        m.write_trailer().unwrap();
        assert_eq!(text(&snapshot), "# timecode format v2\n0\n40\n80\n960\n");
    }

    #[test]
    fn a_second_stream_is_rejected_regardless_of_media_type() {
        let sink = MemorySink::new();
        let mut m = MkvTimestampV2Muxer::new(Box::new(sink)).unwrap();
        m.add_stream(&params(MediaType::Video)).unwrap();
        assert!(m.add_stream(&params(MediaType::Video)).is_err());
        let sink2 = MemorySink::new();
        let mut m2 = MkvTimestampV2Muxer::new(Box::new(sink2)).unwrap();
        m2.add_stream(&params(MediaType::Video)).unwrap();
        assert!(m2.add_stream(&params(MediaType::Audio)).is_err());
    }

    #[test]
    fn an_audio_only_file_is_rejected_outright() {
        let sink = MemorySink::new();
        let mut m = MkvTimestampV2Muxer::new(Box::new(sink)).unwrap();
        assert!(m.add_stream(&params(MediaType::Audio)).is_err());
    }

    #[test]
    fn a_packet_with_no_pts_is_an_error_not_a_panic() {
        let sink = MemorySink::new();
        let mut m = MkvTimestampV2Muxer::new(Box::new(sink)).unwrap();
        m.add_stream(&params(MediaType::Video)).unwrap();
        m.write_header().unwrap();
        let mut p = packet_at(0);
        p.pts = Timestamp::NONE;
        assert!(m.write_packet(&p).is_err());
    }

    #[test]
    fn declares_ms_time_base_only_once_a_stream_exists() {
        let sink = MemorySink::new();
        let mut m = MkvTimestampV2Muxer::new(Box::new(sink)).unwrap();
        assert_eq!(m.stream_time_base(0), None);
        m.add_stream(&params(MediaType::Video)).unwrap();
        assert_eq!(m.stream_time_base(0), Some(MS_TIME_BASE));
    }

    #[test]
    fn descriptor_declares_video_only_defaults() {
        assert!(MUXER_MKVTIMESTAMP_V2.matches_name("mkvtimestamp_v2"));
        assert_eq!(
            MUXER_MKVTIMESTAMP_V2.default_codec(MediaType::Video),
            Some(CodecId::Rawvideo)
        );
        assert_eq!(MUXER_MKVTIMESTAMP_V2.default_codec(MediaType::Audio), None);
    }
}
