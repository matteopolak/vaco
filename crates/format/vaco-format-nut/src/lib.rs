//! NUT: the ffmpeg/mplayer container, from the frozen 2008-02-02
//! specification.
//!
//! # Why this is the format in this batch where byte-identity was worth
//! chasing
//!
//! NUT is fully, publicly specified — every field's encoding is written
//! down, not reverse-engineered from a muxer's behaviour. That does not
//! mean every *choice* a real muxer makes is specified: the exact
//! frame-code table layout, which frames get elision headers, and the
//! precise `back_ptr` placement are muxer heuristics the spec deliberately
//! leaves open. See [`header`] and [`mux`] for exactly which of those this
//! crate's own muxer does not attempt to match, and why that is a
//! documented scope boundary rather than an accident.
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`vlc`] | `v`/`s`/`vb`/`t` variable-length coding, `convert_ts` |
//! | [`startcode`] | the five startcodes and the file signature |
//! | [`header`] | `main_header`'s frame-code table, `stream_header` |
//! | [`codecs`] | `fourcc` <-> [`vaco_codec_core::CodecId`] |
//! | [`demux`] | packet framing, timestamp reconstruction |
//! | [`mux`] | the inverse |
//!
//! # A real measured checksum bug this crate's development caught early
//!
//! The specification says only "Generator polynomial is 0x104C11DB7.
//! Starting value is zero" for `checksum`/`header_checksum` — silent about
//! bit order, reflection and final XOR. `vaco-hash::crc32` (the ordinary
//! reflected `CRC-32/ISO-HDLC`) does not produce the value a real `ffmpeg
//! -f nut` file's own `main_header` checksum contains; `vaco_hash::crc32_nut`
//! does, verified against those exact bytes — see that function's docs for
//! the derivation and `vaco-hash`'s test suite for the reproducible check.

#![forbid(unsafe_code)]

pub mod codecs;
pub mod demux;
pub mod header;
pub mod mux;
pub mod startcode;
pub mod vlc;

use vaco_core::Result;
use vaco_format_core::{Demuxer, DemuxerDesc, MuxerDesc, ParserProvider};
use vaco_io::{MediaSink, MediaSource};

pub use demux::NutDemuxer;
pub use mux::NutMuxer;
pub use startcode::FILE_ID_STRING;

/// Content sniff: the literal file signature. Unambiguous by construction —
/// no other format's magic collides with `"nut/multimedia container\0"`.
#[must_use]
pub fn probe(data: &vaco_format_core::ProbeData<'_>) -> vaco_format_core::ProbeScore {
    let matches = (0..FILE_ID_STRING.len()).all(|i| data.get(i) == FILE_ID_STRING.get(i).copied());
    if matches {
        vaco_format_core::ProbeScore::MAX
    } else {
        vaco_format_core::ProbeScore::from_extension(data, &["nut"])
    }
}

/// The `nut` demuxer registry descriptor. Measured: `ffmpeg -h
/// demuxer=nut` lists `Common extensions: nut.` and no `Mime type:` line.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "nut",
    long_name: "NUT",
    extensions: &["nut"],
    mime_types: &[],
    flags: crate::demux::FLAGS,
    probe,
    open: open_demuxer,
};

/// The `nut` muxer registry descriptor. Measured: `ffmpeg -h muxer=nut` ->
/// `Mime type: video/x-nut`, `Default video codec: mpeg4`, `Default audio
/// codec: mp3`.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "nut",
    long_name: "NUT",
    extensions: &["nut"],
    default_video: Some(vaco_codec_core::CodecId::Mpeg4),
    default_audio: Some(vaco_codec_core::CodecId::Mp3),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    // MPEG-4/H.264 video and MP3/PCM audio all carry their own in-band
    // configuration (or, for MPEG-4, `codec_specific_data` straight off the
    // wire) — nothing here needs an injected bitstream parser.
    Ok(Box::new(NutDemuxer::open(src)?))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open; NutMuxer::new cannot fail"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn vaco_format_core::Muxer>> {
    Ok(Box::new(NutMuxer::new(sink)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demuxer_descriptor_answers_to_its_name_and_extension() {
        assert!(DEMUXER.matches_name("nut"));
        assert!(DEMUXER.matches_extension("/tmp/x.nut"));
        assert!(!DEMUXER.matches_extension("/tmp/x.mp4"));
    }

    #[test]
    fn the_muxer_descriptor_answers_to_its_name_and_default_codecs() {
        assert!(MUXER.matches_name("nut"));
        assert_eq!(
            MUXER.default_codec(vaco_core::MediaType::Video),
            Some(vaco_codec_core::CodecId::Mpeg4)
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
    fn the_probe_accepts_the_real_signature() {
        let data = vaco_format_core::ProbeData::new(FILE_ID_STRING);
        assert_eq!(probe(&data), vaco_format_core::ProbeScore::MAX);
    }
}
