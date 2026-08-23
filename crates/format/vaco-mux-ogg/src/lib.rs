//! The Ogg muxer, and the `oga`/`ogv`/`opus`/`spx` alias registrations.
//!
//! One implementation, [`writer::OggMuxer`], behind five [`MuxerDesc`]
//! constants that differ only in their declared default codecs and file
//! extensions — measured against `ffmpeg -formats` / `ffmpeg -h muxer=<name>`
//! 8.1, not assumed:
//!
//! | Name | Extension | Default video | Default audio |
//! |---|---|---|---|
//! | [`MUXER_OGG`] | `ogg` | theora (no `CodecId`, see below) | flac |
//! | [`MUXER_OGA`] | `oga` | — | flac |
//! | [`MUXER_OGV`] | `ogv` | vp8 | — |
//! | [`MUXER_OPUS`] | `opus` | — | opus |
//! | [`MUXER_SPX`] | `spx` | — | speex (no `CodecId`) |
//!
//! `vaco_codec_core::CodecId` has no `Theora` or `Speex` variant (confirmed
//! by reading the enum, not assumed — the same gap
//! `vaco-demux-ogg::codec::OggCodec`'s docs record), so [`MUXER_OGG`] and
//! [`MUXER_SPX`] cannot declare their reference-measured default codec and
//! leave that field `None` instead of a wrong one.
//!
//! # What actually works end to end
//!
//! **Opus and FLAC round-trip through this crate's own sibling demuxer** —
//! see `tests/roundtrip.rs`, which muxes real packets and reads them back
//! with [`vaco_demux_ogg::OggDemuxer`] rather than asserting on bytes this
//! crate wrote itself. Both codecs need only one header packet's worth of
//! caller-supplied `extradata` (`OpusHead`, FLAC's raw `STREAMINFO`); this
//! crate synthesises the mandatory second (comment) packet each still needs
//! — see [`headers`].
//!
//! Vorbis, Theora and Speex do **not** round-trip: each needs a *setup*
//! header (codebooks or quantisation tables an encoder chose) that cannot be
//! synthesised generically, and no crate in this workspace yet produces one
//! or defines a convention for packing multiple header packets into one
//! `extradata` blob. A caller muxing one of these gets only its
//! identification packet written as a single header page; see
//! `docs/format/vaco-mux-ogg.md`.
//!
//! Page boundaries are this muxer's own policy (RFC 3533 does not dictate
//! them) and will not be byte-identical to the reference's; see
//! [`writer`]'s module docs.

#![forbid(unsafe_code)]

pub mod headers;
pub mod writer;

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::MediaSink;

pub use writer::OggMuxer;

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(OggMuxer::new(sink)?))
}

/// `ffmpeg -f ogg`: video and audio, defaulting to Theora and FLAC.
///
/// `default_video` is `None`, not `Some(CodecId::Theora)` — no such variant
/// exists; see the crate docs.
pub const MUXER_OGG: MuxerDesc = MuxerDesc {
    name: "ogg",
    long_name: "Ogg",
    extensions: &["ogg"],
    default_video: None,
    default_audio: Some(CodecId::Flac),
    open: open_muxer,
};

/// `ffmpeg -f oga`: audio only, defaulting to FLAC.
pub const MUXER_OGA: MuxerDesc = MuxerDesc {
    name: "oga",
    long_name: "Ogg Audio",
    extensions: &["oga"],
    default_video: None,
    default_audio: Some(CodecId::Flac),
    open: open_muxer,
};

/// `ffmpeg -f ogv`: video, defaulting to VP8.
pub const MUXER_OGV: MuxerDesc = MuxerDesc {
    name: "ogv",
    long_name: "Ogg Video",
    extensions: &["ogv"],
    default_video: Some(CodecId::Vp8),
    default_audio: None,
    open: open_muxer,
};

/// `ffmpeg -f opus`: audio, defaulting to Opus.
pub const MUXER_OPUS: MuxerDesc = MuxerDesc {
    name: "opus",
    long_name: "Ogg Opus",
    extensions: &["opus"],
    default_video: None,
    default_audio: Some(CodecId::Opus),
    open: open_muxer,
};

/// `ffmpeg -f spx`: audio, defaulting to Speex.
///
/// `default_audio` is `None`, not `Some(CodecId::Speex)` — no such variant
/// exists; see the crate docs.
pub const MUXER_SPX: MuxerDesc = MuxerDesc {
    name: "spx",
    long_name: "Ogg Speex",
    extensions: &["spx"],
    default_video: None,
    default_audio: None,
    open: open_muxer,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_five_descriptors_answer_to_their_own_name() {
        assert!(MUXER_OGG.matches_name("ogg"));
        assert!(MUXER_OGA.matches_name("oga"));
        assert!(MUXER_OGV.matches_name("ogv"));
        assert!(MUXER_OPUS.matches_name("opus"));
        assert!(MUXER_SPX.matches_name("spx"));
    }

    #[test]
    fn defaults_match_what_was_measured_against_the_reference() {
        assert_eq!(MUXER_OGG.default_audio, Some(CodecId::Flac));
        assert_eq!(MUXER_OGG.default_video, None);
        assert_eq!(MUXER_OGV.default_video, Some(CodecId::Vp8));
        assert_eq!(MUXER_OPUS.default_audio, Some(CodecId::Opus));
        assert_eq!(MUXER_SPX.default_audio, None);
    }
}
