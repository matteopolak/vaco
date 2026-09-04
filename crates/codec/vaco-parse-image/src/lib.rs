//! Still-image header parsing — **no decode**.
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
//! | [`still`] | PCX, TGA, SGI, XWD, XBM, QOI, PBM/PGM/PPM/PAM/PFM/PHM, JPEG-LS and `OpenEXR`, each forwarding to its own decoder crate's header reader |
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
//! None of these formats need boundary-finding the way a video elementary
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
pub mod still;
pub mod tiff;
pub mod webp;

pub use parser::ImageParser;

// Re-exported so a caller can describe a stream without also depending on
// `vaco-codec-core` directly.
pub use vaco_codec_core::CodecParameters;

/// The registry descriptor for the PNG parser. `vaco-component.toml` names
/// each of these consts, `cargo xtask gen-registry` puts them in
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

/// The registry descriptor for the PC Paintbrush PCX image parser. See [`PARSER_PNG`].
pub const PARSER_PCX: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "pcx",
    long_name: "PC Paintbrush PCX image",
    codecs: &[::vaco_codec_core::CodecId::Pcx],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Pcx>::new(limits)),
};

/// The registry descriptor for the Truevision Targa image parser. See [`PARSER_PNG`].
pub const PARSER_TARGA: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "targa",
    long_name: "Truevision Targa image",
    codecs: &[::vaco_codec_core::CodecId::Targa],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Targa>::new(limits)),
};

/// The registry descriptor for the SGI image parser. See [`PARSER_PNG`].
pub const PARSER_SGI: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "sgi",
    long_name: "SGI image",
    codecs: &[::vaco_codec_core::CodecId::Sgi],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Sgi>::new(limits)),
};

/// The registry descriptor for the XWD (X Window Dump) image parser. See [`PARSER_PNG`].
pub const PARSER_XWD: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "xwd",
    long_name: "XWD (X Window Dump) image",
    codecs: &[::vaco_codec_core::CodecId::Xwd],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Xwd>::new(limits)),
};

/// The registry descriptor for the XBM (X BitMap) image parser. See [`PARSER_PNG`].
pub const PARSER_XBM: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "xbm",
    long_name: "XBM (X BitMap) image",
    codecs: &[::vaco_codec_core::CodecId::Xbm],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Xbm>::new(limits)),
};

/// The registry descriptor for the QOI (Quite OK Image) parser. See [`PARSER_PNG`].
pub const PARSER_QOI: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "qoi",
    long_name: "QOI (Quite OK Image)",
    codecs: &[::vaco_codec_core::CodecId::Qoi],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Qoi>::new(limits)),
};

/// The registry descriptor for the PBM (Portable BitMap) image parser. See [`PARSER_PNG`].
pub const PARSER_PBM: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "pbm",
    long_name: "PBM (Portable BitMap) image",
    codecs: &[::vaco_codec_core::CodecId::Pbm],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Pbm>::new(limits)),
};

/// The registry descriptor for the PGM (Portable GrayMap) image parser. See [`PARSER_PNG`].
pub const PARSER_PGM: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "pgm",
    long_name: "PGM (Portable GrayMap) image",
    codecs: &[::vaco_codec_core::CodecId::Pgm],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Pgm>::new(limits)),
};

/// The registry descriptor for the PPM (Portable PixelMap) image parser. See [`PARSER_PNG`].
pub const PARSER_PPM: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "ppm",
    long_name: "PPM (Portable PixelMap) image",
    codecs: &[::vaco_codec_core::CodecId::Ppm],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Ppm>::new(limits)),
};

/// The registry descriptor for the PAM (Portable AnyMap) image parser. See [`PARSER_PNG`].
pub const PARSER_PAM: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "pam",
    long_name: "PAM (Portable AnyMap) image",
    codecs: &[::vaco_codec_core::CodecId::Pam],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Pam>::new(limits)),
};

/// The registry descriptor for the PFM (Portable FloatMap) image parser. See [`PARSER_PNG`].
pub const PARSER_PFM: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "pfm",
    long_name: "PFM (Portable FloatMap) image",
    codecs: &[::vaco_codec_core::CodecId::Pfm],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Pfm>::new(limits)),
};

/// The registry descriptor for the PHM (Portable HalfMap) image parser. See [`PARSER_PNG`].
pub const PARSER_PHM: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "phm",
    long_name: "PHM (Portable HalfMap) image",
    codecs: &[::vaco_codec_core::CodecId::Phm],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Phm>::new(limits)),
};

/// The registry descriptor for the JPEG-LS parser. See [`PARSER_PNG`].
pub const PARSER_JPEGLS: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "jpegls",
    long_name: "JPEG-LS",
    codecs: &[::vaco_codec_core::CodecId::JpegLs],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::JpegLs>::new(limits)),
};

/// The registry descriptor for the `OpenEXR` parser. See [`PARSER_PNG`].
pub const PARSER_EXR: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "exr",
    long_name: "OpenEXR image",
    codecs: &[::vaco_codec_core::CodecId::Exr],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(ImageParser::<still::Exr>::new(limits)),
};
