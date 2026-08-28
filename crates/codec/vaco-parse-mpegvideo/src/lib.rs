//! MPEG-1/2/4 part 2 video header parsing — **no decode**.
//!
//! Closes P-07 (#277).
//!
//! # What is here
//!
//! | Module | Syntax |
//! |---|---|
//! | [`mpeg12`] | MPEG-1/2 `sequence_header()`, `sequence_extension()`, `picture_header()` |
//! | [`mpeg4`] | MPEG-4 part 2 `VisualObjectSequence`, `VideoObjectLayer`, `VideoObjectPlane` |
//!
//! # Parsing is not decoding
//!
//! Same line every other `vaco-parse-*` crate draws (D5, D7, plan 15 §1.6):
//! header syntax and access-unit boundaries only, nothing that reconstructs
//! a sample.
//!
//! # Specification
//!
//! ISO/IEC 11172-2 (MPEG-1 video); ITU-T H.262 / ISO/IEC 13818-2 (MPEG-2
//! video), §6 for the syntax and Table 8-10/6-3/6-4/6-8 for the
//! profile/aspect-ratio/frame-rate/chroma-format tables. Nothing here was
//! taken from any implementation (D7); see [`mpeg12::profile_name`]'s doc for
//! where the profile table was measured against a real encoder rather than
//! transcribed from the standard's text.
//!
//! # Dependencies
//!
//! `vaco-bitstream` for the reader and the start-code primitive,
//! `vaco-codec-core` for the [`Parser`](vaco_codec_core::Parser) trait and
//! [`CodecParameters`](vaco_codec_core::CodecParameters), `vaco-pixfmt` for
//! the pixel-format enum, `vaco-limits` for the budget, `vaco-packet` for
//! the emitted packets. No external runtime dependencies.

#![forbid(unsafe_code)]

pub mod mpeg12;
pub mod mpeg4;

pub use mpeg12::{Mpeg12Parser, aspect_ratio, frame_rate, pixel_format, profile_name};
pub use mpeg4::Mpeg4Parser;

// Re-exported so a caller can describe a stream without also depending on
// `vaco-codec-core` directly.
pub use vaco_codec_core::CodecParameters;

/// The registry descriptor for the MPEG-1 video parser.
///
/// `vaco-component.toml` names this const, `cargo xtask gen-registry` puts it
/// in `vaco_registry::PARSERS`, and a demuxer reaches it through
/// `ParserProvider` without ever naming this crate (D14.1).
pub const PARSER_MPEG1: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "mpeg1video",
    long_name: "MPEG-1 video",
    codecs: &[::vaco_codec_core::CodecId::Mpeg1video],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(mpeg12::Mpeg12Parser::new(limits)),
};

/// The registry descriptor for the MPEG-2 video parser. See [`PARSER_MPEG1`].
pub const PARSER_MPEG2: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "mpeg2video",
    long_name: "MPEG-2 video",
    codecs: &[::vaco_codec_core::CodecId::Mpeg2video],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(mpeg12::Mpeg12Parser::new(limits)),
};

/// The registry descriptor for the MPEG-4 part 2 video parser. See
/// [`PARSER_MPEG1`].
pub const PARSER_MPEG4: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "mpeg4",
    long_name: "MPEG-4 part 2",
    codecs: &[::vaco_codec_core::CodecId::Mpeg4],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(mpeg4::Mpeg4Parser::new(limits)),
};
