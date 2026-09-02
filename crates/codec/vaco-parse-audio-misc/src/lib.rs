//! Vorbis, FLAC and ALAC header parsing (no decode).
//!
//! # What is in here
//!
//! | Module | Syntax | Specification |
//! |---|---|---|
//! | [`vorbis`] | identification header, Xiph header packing | Xiph Vorbis I §4.2.2 |
//! | [`flac`] | `STREAMINFO` | the FLAC format document |
//! | [`alac`] | `ALACSpecificConfig` | none published; measured directly |
//!
//! # Comment and picture parsing lives one crate over
//!
//! Vorbis comment tags and FLAC's `METADATA_BLOCK_PICTURE` are
//! `vaco-format-vorbiscomment`'s job (work package `#540`), not this
//! crate's: both formats' setup headers are container-consumed metadata
//! rather than a codec parameter this crate's `Parser`s report, and the two
//! packages deliberately share that one reader rather than each parsing the
//! same vendor-plus-tag-list shape independently. See that crate's docs.
//!
//! # Parsing is not decoding
//!
//! As with `vaco-parse-aac` and `vaco-parse-opus`: nothing here reconstructs
//! spectra, runs Rice decoding or produces PCM. Every `Parser` here treats
//! its input as an already-framed packet the container delivered — see each
//! module's own doc for exactly what that assumes and what it does not
//! attempt (a raw, non-containerized `.flac` elementary stream's own frame
//! sync, for one).

#![forbid(unsafe_code)]

pub mod alac;
pub mod flac;
pub mod vorbis;

pub use alac::{AlacChannelLayoutInfo, AlacCookie, AlacParser, AlacSpecificConfig};
pub use flac::{FlacParser, StreamInfo};
pub use vorbis::{IdentificationHeader, VorbisParser, unpack_headers};

/// The Vorbis descriptor.
pub const PARSER_VORBIS: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "vorbis",
    long_name: "Vorbis",
    codecs: &[::vaco_codec_core::CodecId::Vorbis],
    media_type: ::vaco_core::MediaType::Audio,
    make: |limits| ::std::boxed::Box::new(vorbis::VorbisParser::new(limits)),
};

/// The FLAC descriptor.
pub const PARSER_FLAC: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "flac",
    long_name: "FLAC (Free Lossless Audio Codec)",
    codecs: &[::vaco_codec_core::CodecId::Flac],
    media_type: ::vaco_core::MediaType::Audio,
    make: |limits| ::std::boxed::Box::new(flac::FlacParser::new(limits)),
};

/// The ALAC descriptor.
pub const PARSER_ALAC: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "alac",
    long_name: "ALAC (Apple Lossless Audio Codec)",
    codecs: &[::vaco_codec_core::CodecId::Alac],
    media_type: ::vaco_core::MediaType::Audio,
    make: |limits| ::std::boxed::Box::new(alac::AlacParser::new(limits)),
};
