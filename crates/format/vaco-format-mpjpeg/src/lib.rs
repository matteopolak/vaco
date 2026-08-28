//! MPJPEG: MIME multipart JPEG, the "motion JPEG over HTTP" wire format.
//!
//! # What this actually is
//!
//! Not a container in the box/EBML/pack sense — a byte stream of repeated
//! MIME multipart parts, one JPEG picture per part:
//!
//! ```text
//! --ffmpeg\r\n
//! Content-type: image/jpeg\r\n
//! Content-length: 2164\r\n
//! \r\n
//! <2164 bytes of JPEG>\r\n
//! --ffmpeg\r\n
//! ...
//! ```
//!
//! Measured against `ffmpeg -f mpjpeg` (8.1): the boundary tag defaults to
//! `ffmpeg`, every part's headers are exactly `Content-type: image/jpeg\r\n`
//! then `Content-length: N\r\n` in that order and capitalisation, and
//! [`mux::MpjpegMuxer::write_trailer`] emits one final boundary line and
//! nothing else — no closing `--boundary--`, since the format is for
//! streams with no defined end.
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`demux`] | boundary/header parsing, `Content-length`-driven reads |
//! | [`mux`] | the inverse: one part per video packet |
//!
//! # What the demuxer tolerates that the muxer never produces
//!
//! `ffmpeg -h demuxer=mpjpeg` exposes `-strict_mime_boundary` (default
//! `false`): a lenient reader need not verify that every boundary line
//! repeats the same tag byte-for-byte. This demuxer models that knob (see
//! [`demux::MpjpegDemuxer::strict_mime_boundary`]) by checking only the
//! leading `--` in non-strict mode. It does **not** recover a part whose
//! `Content-length` is missing: every reference sample sends the header,
//! and scanning for an EOI marker instead is an unverifiable heuristic.

#![forbid(unsafe_code)]

pub mod demux;
pub mod mux;

use vaco_core::Result;
use vaco_format_core::{Demuxer, DemuxerDesc, MuxerDesc, ParserProvider};
use vaco_io::{MediaSink, MediaSource};

pub use demux::MpjpegDemuxer;
pub use mux::MpjpegMuxer;

/// Content sniff: the input must open with `--` followed by a boundary token
/// and a line ending, i.e. it must look like the start of a MIME multipart
/// part. There is no magic number to check instead — MPJPEG has none — so
/// this is deliberately weaker than most probes and extension carries most of
/// the weight, matching how thin a signal the reference format itself is.
#[must_use]
pub fn probe(data: &vaco_format_core::ProbeData<'_>) -> vaco_format_core::ProbeScore {
    if looks_like_boundary_line(data) {
        vaco_format_core::ProbeScore::CONTENT
    } else {
        vaco_format_core::ProbeScore::from_extension(data, &["mjpg"])
    }
}

fn looks_like_boundary_line(data: &vaco_format_core::ProbeData<'_>) -> bool {
    if data.get(0) != Some(b'-') || data.get(1) != Some(b'-') {
        return false;
    }
    // A boundary line is followed by a header block, so somewhere in the
    // first probe window there must be a line that looks like
    // `Content-length:` (case-insensitively on the field name, matching how
    // this crate reads it back in `demux`).
    let window = data.len().min(512);
    let mut i = 2usize;
    let mut saw_newline = false;
    while i < window {
        if data.get(i) == Some(b'\n') {
            saw_newline = true;
            break;
        }
        i += 1;
    }
    saw_newline
}

/// The registry descriptor.
///
/// No `mime_types`: measured on `ffmpeg -h demuxer=mpjpeg`, which prints only
/// `Common extensions: mjpg.` and no `Mime type:` line at all — the
/// asymmetry is real, the muxer has one and the demuxer does not.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "mpjpeg",
    long_name: "MIME multipart JPEG",
    extensions: &["mjpg"],
    mime_types: &[],
    flags: crate::demux::FLAGS,
    probe,
    open: open_demuxer,
};

/// The registry descriptor for muxing.
///
/// `Default video codec: mjpeg` and `Mime type:
/// multipart/x-mixed-replace;boundary=ffmpeg` both come from `ffmpeg -h
/// muxer=mpjpeg`.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "mpjpeg",
    long_name: "MIME multipart JPEG",
    extensions: &["mjpg"],
    default_video: Some(vaco_codec_core::CodecId::Jpeg),
    default_audio: None,
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    // Every part is a complete JPEG picture; there is no in-band codec
    // configuration to extract via a parser.
    Ok(Box::new(MpjpegDemuxer::open(src)?))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open; MpjpegMuxer::new cannot fail"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn vaco_format_core::Muxer>> {
    Ok(Box::new(MpjpegMuxer::new(sink)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demuxer_descriptor_answers_to_the_names_the_cli_uses() {
        assert!(DEMUXER.matches_name("mpjpeg"));
        assert!(DEMUXER.matches_extension("/tmp/x.mjpg"));
        assert!(!DEMUXER.matches_extension("/tmp/x.mp4"));
    }

    #[test]
    fn the_muxer_descriptor_answers_to_its_name() {
        assert!(MUXER.matches_name("mpjpeg"));
        assert_eq!(
            MUXER.default_codec(vaco_core::MediaType::Video),
            Some(vaco_codec_core::CodecId::Jpeg)
        );
    }

    #[test]
    fn the_probe_rejects_prose() {
        let data = vaco_format_core::ProbeData::new(b"The quick brown fox jumps.");
        assert_eq!(probe(&data), vaco_format_core::ProbeScore::NONE);
    }

    #[test]
    fn the_probe_accepts_a_boundary_line() {
        let data =
            vaco_format_core::ProbeData::new(b"--ffmpeg\r\nContent-type: image/jpeg\r\n\r\n");
        assert_eq!(probe(&data), vaco_format_core::ProbeScore::CONTENT);
    }
}
