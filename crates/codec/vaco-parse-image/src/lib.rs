//! PNG/JPEG/GIF/BMP/TIFF/WebP header parsing — **no decode**.
//!
//! Closes P-08 (#278).
//!
//! # What is here
//!
//! | Module | Format |
//! |---|---|
//! | [`png`] | PNG signature + `IHDR` |
//! | [`jpeg`] | JPEG (read as Motion JPEG): `SOI` through the first `SOF` |
//! | [`gif`] | GIF signature + Logical Screen Descriptor |
//! | [`bmp`] | `BITMAPFILEHEADER` + `BITMAPINFOHEADER`'s leading fields |
//! | [`tiff`] | The byte-order header + IFD 0's baseline tags |
//! | [`webp`] | RIFF/`WEBP`: `VP8 ` (lossy), `VP8L` (lossless), `VP8X` (extended) |
//! | [`parser`] | [`parser::ImageParser`], the shared "whole file is one image" `Parser` wrapper every format above plugs into |
//!
//! # Parsing is not decoding
//!
//! Same line every other `vaco-parse-*` crate draws (D5, D7, plan 15 §1.6):
//! header syntax only — dimensions, sample layout, the handful of fields
//! `CodecParameters` needs — never a decoded pixel.
//!
//! # Framing: the whole file is one image
//!
//! None of the six formats need boundary-finding the way a video elementary
//! stream does: `vaco-demux-image2` already hands this crate one whole file
//! as one packet, whether through `image2`'s pattern match or one of its 37
//! `*_pipe` splitters. See [`parser`]'s module doc for the full argument,
//! which is the same one `vaco-parse-opus` and `vaco-parse-vpx` make for
//! their own self-contained formats.
//!
//! # Specification
//!
//! ISO/IEC 15948 (PNG); ITU-T T.81 / ISO/IEC 10918-1 (JPEG); the `CompuServe`
//! `GIF89a` specification; Microsoft's `BITMAPFILEHEADER`/`BITMAPINFOHEADER`
//! documentation (BMP); Adobe TIFF Revision 6.0; Google's WebP Container and
//! WebP Lossless Bitstream specifications. Nothing here was taken from any
//! implementation (D7); every pixel-format mapping was measured against a
//! real encoded file rather than read off a table, and each module's doc
//! comment says which combinations were probed and which are a same-pattern
//! extrapolation.
//!
//! # Dependencies
//!
//! `vaco-bitstream` for the byte reader, `vaco-codec-core` for the
//! [`Parser`](vaco_codec_core::Parser) trait and
//! [`CodecParameters`](vaco_codec_core::CodecParameters), `vaco-pixfmt` for
//! the pixel-format enum, `vaco-limits` for the budget, `vaco-packet` for
//! the emitted packets, `vaco-parse-vpx` for the VP8 frame-tag reader WebP's
//! lossy sub-format reuses. No external runtime dependencies.

#![forbid(unsafe_code)]

pub mod bmp;
pub mod gif;
pub mod jpeg;
pub mod parser;
pub mod png;
pub mod tiff;
pub mod webp;

pub use parser::ImageParser;

// Re-exported so a caller can describe a stream without also depending on
// `vaco-codec-core` directly.
pub use vaco_codec_core::CodecParameters;

/// The registry descriptor for the PNG parser. `vaco-component.toml` names
/// each of these six consts, `cargo xtask gen-registry` puts them in
/// `vaco_registry::PARSERS`, and a demuxer reaches one through
/// `ParserProvider` without ever naming this crate (D14.1).
pub const PARSER_PNG: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "png",
    long_name: "PNG (Portable Network Graphics) image",
    codecs: &[::vaco_codec_core::CodecId::Png],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<png::Png>::new(limits)),
};

/// The registry descriptor for the JPEG parser. See [`PARSER_PNG`].
pub const PARSER_JPEG: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "mjpeg",
    long_name: "Motion JPEG",
    codecs: &[::vaco_codec_core::CodecId::Jpeg],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<jpeg::Jpeg>::new(limits)),
};

/// The registry descriptor for the GIF parser. See [`PARSER_PNG`].
pub const PARSER_GIF: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "gif",
    long_name: "CompuServe GIF (Graphics Interchange Format)",
    codecs: &[::vaco_codec_core::CodecId::Gif],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<gif::Gif>::new(limits)),
};

/// The registry descriptor for the BMP parser. See [`PARSER_PNG`].
pub const PARSER_BMP: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "bmp",
    long_name: "BMP (Windows and OS/2 bitmap)",
    codecs: &[::vaco_codec_core::CodecId::Bmp],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<bmp::Bmp>::new(limits)),
};

/// The registry descriptor for the TIFF parser. See [`PARSER_PNG`].
pub const PARSER_TIFF: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "tiff",
    long_name: "TIFF image",
    codecs: &[::vaco_codec_core::CodecId::Tiff],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<tiff::Tiff>::new(limits)),
};

/// The registry descriptor for the WebP parser. See [`PARSER_PNG`].
pub const PARSER_WEBP: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "webp",
    long_name: "WebP",
    codecs: &[::vaco_codec_core::CodecId::Webp],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<webp::Webp>::new(limits)),
};
