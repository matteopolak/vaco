//! SWF (`ShockWave` Flash): the media tags, not a Flash player.
//!
//! # What this crate is and is not
//!
//! SWF's tag vocabulary covers an entire vector-graphics/animation/
//! `ActionScript` runtime — shapes, sprites, fonts, buttons, scripts, a
//! full display list.
//! This crate reads and writes exactly the tags that carry compressed media
//! (`DefineVideoStream`/`VideoFrame` for video, `SoundStreamHead(2)`/
//! `SoundStreamBlock` for audio) plus the handful of structural tags needed
//! to walk the file (`ShowFrame`, `End`) — everything else is skipped by its
//! own declared length, never interpreted. See [`demux`] for exactly which
//! tags are read and [`mux`] for which are written.
//!
//! Every field layout here was measured against a real `ffmpeg -f swf`
//! capture (see [`header`], [`demux`]) rather than assumed from the spec.
//!
//! One finding shaped this crate's mux-side scope: stripping every
//! `PlaceObject2` tag (the display-list placement of the video/sound
//! character) out of a real reference `.swf` and re-probing it still
//! reports the correct codec, dimensions, rates and packet count —
//! `PlaceObject2` is part of what the reference muxer writes, but it is not
//! needed to read the media back, so [`mux::SwfMuxer`] omits it (see its
//! module docs for the rest of what mux-side does not attempt).
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`header`] | fixed header, bit-packed `RECT`, frame rate/count |
//! | [`tags`] | the tag-header varint-length encoding every tag shares |
//! | [`demux`] | tag walk, video/audio packet extraction |
//! | [`mux`] | the inverse: header + minimal media-only tag stream |

#![forbid(unsafe_code)]

pub mod demux;
pub mod header;
pub mod mux;
pub mod tags;

use vaco_core::Result;
use vaco_format_core::{Demuxer, DemuxerDesc, MuxerDesc, ParserProvider};
use vaco_io::{MediaSink, MediaSource};

pub use demux::SwfDemuxer;
pub use mux::SwfMuxer;

/// Content sniff: the three-byte signature, uncompressed or not. Measured:
/// `ffprobe out.swf` (no `-f` override) reports `probe_score=51` — one
/// above `ProbeScore::EXTENSION` (50), not `CONTENT` (75) — for a real
/// reference file, which this reproduces literally rather than rounding up
/// to a "stronger" score that looks more confident than the reference's own
/// signature check apparently is.
#[must_use]
pub fn probe(data: &vaco_format_core::ProbeData<'_>) -> vaco_format_core::ProbeScore {
    let sig = [data.get(0), data.get(1), data.get(2)];
    let is_swf = matches!(sig, [Some(b'F' | b'C' | b'Z'), Some(b'W'), Some(b'S')]);
    if is_swf {
        vaco_format_core::ProbeScore(51)
    } else {
        vaco_format_core::ProbeScore::from_extension(data, &["swf"])
    }
}

/// The `swf` demuxer registry descriptor. No extensions, no MIME type —
/// measured: `ffmpeg -h demuxer=swf` prints neither.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "swf",
    long_name: "SWF (ShockWave Flash)",
    extensions: &[],
    mime_types: &[],
    flags: crate::demux::FLAGS,
    probe,
    open: open_demuxer,
};

/// The `swf` muxer registry descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "swf",
    long_name: "SWF (ShockWave Flash)",
    extensions: &["swf"],
    default_video: Some(vaco_codec_core::CodecId::Flv1),
    default_audio: Some(vaco_codec_core::CodecId::Mp3),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    // Sorenson H.263 (FLV1) and MP3 both carry their own frame headers;
    // nothing here needs an injected bitstream parser.
    Ok(Box::new(SwfDemuxer::open(src)?))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open; SwfMuxer::new cannot fail"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn vaco_format_core::Muxer>> {
    Ok(Box::new(SwfMuxer::new(sink)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demuxer_descriptor_answers_to_its_name_and_has_no_extension() {
        assert!(DEMUXER.matches_name("swf"));
        assert!(!DEMUXER.matches_extension("/tmp/x.swf"));
    }

    #[test]
    fn the_muxer_descriptor_answers_to_its_name_and_default_codecs() {
        assert!(MUXER.matches_name("swf"));
        assert_eq!(
            MUXER.default_codec(vaco_core::MediaType::Video),
            Some(vaco_codec_core::CodecId::Flv1)
        );
        assert_eq!(
            MUXER.default_codec(vaco_core::MediaType::Audio),
            Some(vaco_codec_core::CodecId::Mp3)
        );
    }

    #[test]
    fn the_probe_rejects_prose() {
        let data = vaco_format_core::ProbeData::new(b"The quick brown fox jumps over.");
        assert_eq!(probe(&data), vaco_format_core::ProbeScore::NONE);
    }

    #[test]
    fn the_probe_scores_a_real_signature_at_the_measured_value() {
        let data = vaco_format_core::ProbeData::new(b"FWS\x06\x17\x79\x00\x00");
        assert_eq!(probe(&data), vaco_format_core::ProbeScore(51));
    }
}
