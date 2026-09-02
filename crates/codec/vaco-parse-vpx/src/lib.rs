//! VP8 and VP9 uncompressed-header parsing — **no decode**.
//!
//! Closes P-06 (#276) and, with it, P-05 (#275): VP9 was the one codec in
//! the profile/level framework (`vaco_codec_core::params`) with no table
//! anywhere in the tree.
//!
//! # Parsing is not decoding
//!
//! Same line `vaco-parse-h264`, `vaco-parse-hevc` and `vaco-parse-av1` draw
//! (D5, D7, plan 15 §1.6): this crate reads frame-header syntax — `profile`,
//! `frame_type`, `color_config()`, `frame_size()` — and stops. No coefficient
//! is read, no motion vector, no sample of output.
//!
//! # What is here
//!
//! | Module | Syntax |
//! |---|---|
//! | [`vp8`] | RFC 6386 §9.1-9.2: the frame tag and key-frame dimensions |
//! | [`vp9`] | §6.2 `uncompressed_header()`, superframe-aware |
//! | [`superframe`] | Annex B's superframe index |
//! | [`profile`] | VP9 profile names and the Annex A level table |
//! | [`vpcc`] | MP4's `vpcC` — `VPCodecConfigurationRecord` |
//!
//! # The framing contract: one `parse` call, one already-framed sample
//!
//! Neither codec has a byte-stream elementary format anywhere in this
//! workspace — no `vaco-demux-raw` `BitstreamSpec` names either one, and
//! there is no IVF demuxer — and neither codec's syntax states a frame's
//! total byte length (VP8's `first_part_size` covers only the boolean-coded
//! header; VP9's tile data has no length field outside a superframe index).
//! So every container VP9/VP8 actually reach this crate through (`WebM`,
//! `AVI`, MP4) already delimits one sample as one frame, and both parsers
//! follow the identical contract `vaco-parse-opus` documents for the
//! identical reason: **the whole input is consumed, and it must already be
//! exactly one frame.** See [`vp8`]'s and [`vp9`]'s module docs.
//!
//! # Safety on untrusted input
//!
//! `unwrap`/`expect`/`panic`/`indexing_slicing` are denied workspace-wide;
//! every read here goes through [`vaco_bitstream::BitReader`]'s or
//! [`vaco_bitstream::ByteReader`]'s sticky-overrun model, which returns zeros
//! past a truncated buffer rather than panicking, and every header parser
//! checks for overrun once at the end rather than threading `Result` through
//! every field read.
//!
//! # Specification
//!
//! RFC 6386 (VP8) §9.1-9.2; the VP9 Bitstream & Decoding Process
//! Specification v0.6 (8 Dec 2016) §6.2 and Annex A/B; the `WebM` Project's
//! `VPCodecConfigurationBox` ISOBMFF binding. Nothing here was taken from any
//! implementation (D7) — see [`profile`]'s module doc for where the level
//! table's numbers were cross-checked instead, since `level` never surfaces
//! through `ffprobe` to probe against directly.
//!
//! # Dependencies
//!
//! `vaco-bitstream` for both reader shapes, `vaco-codec-core` for the
//! [`Parser`](vaco_codec_core::Parser) trait, [`CodecParameters`] and the
//! profile/level framework, `vaco-color` and `vaco-pixfmt` for the
//! signalling enums, `vaco-limits` for the budget, `vaco-packet` for the
//! emitted packets. No external runtime dependencies.

#![forbid(unsafe_code)]

pub mod profile;
pub mod superframe;
pub mod vp8;
pub mod vp9;
pub mod vpcc;

pub use vp8::{FrameTag, Vp8Parser, parse_frame_tag};
pub use vp9::{
    Vp9ColorConfig, Vp9Header, Vp9Parser, parse_display_header, parse_uncompressed_header,
};
pub use vpcc::{
    VpCodecConfigurationRecord, build as build_vpcc, from_vp9_header as vpcc_from_vp9_header,
    parse as parse_vpcc,
};

// Re-exported so a caller can describe a stream without also depending on
// `vaco-codec-core` directly.
pub use vaco_codec_core::CodecParameters;

/// The registry descriptor for the VP8 parser.
///
/// `vaco-component.toml` names this const, `cargo xtask gen-registry` puts it
/// in `vaco_registry::PARSERS`, and a demuxer reaches it through
/// `ParserProvider` without ever naming this crate (D14.1).
pub const PARSER: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "vp8",
    long_name: "On2 VP8",
    codecs: &[::vaco_codec_core::CodecId::Vp8],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(vp8::Vp8Parser::new(limits)),
};

/// The registry descriptor for the VP9 parser. See [`PARSER`].
pub const PARSER_VP9: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "vp9",
    long_name: "Google VP9",
    codecs: &[::vaco_codec_core::CodecId::Vp9],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(vp9::Vp9Parser::new(limits)),
};
