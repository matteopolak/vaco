//! MPEG-1 (ISO/IEC 11172-2) and MPEG-2 (ITU-T H.262 / ISO/IEC 13818-2) video
//! decode.
//!
//! `Vaco-Spec-Ref: itu-t-h262` — the free 1995 base text (ISO/IEC 13818-2 :
//! 1995 (E) / ITU-T H.262 (1995 E)), downloaded directly from
//! `https://www.itu.int/rec/dologin_pub.asp?lang=e&id=T-REC-H.262-199507-S!!PDF-E&type=items`.
//! The later consolidated edition (02/2012) sits behind a TIES login wall;
//! the 1995 base text is the free one and it carries every non-scalable
//! clause this crate implements (§6.2-6.3 syntax, §7.1-7.6 semantics,
//! Annex A/B). See `docs/codec/vaco-codec-mpeg12.md` for what that leaves
//! out and for measured accuracy.
//!
//! # Scope
//!
//! Sequence/GOP/picture headers and their extensions, slice and macroblock
//! layer, intra and inter (P/B) macroblocks, frame pictures in both
//! frame-DCT/frame-MC and field-DCT/field-MC (interlaced-in-a-frame-picture)
//! modes, 4:2:0 chroma. **Not implemented**: separate field pictures
//! (`picture_structure` != frame), dual-prime prediction, 16x8 motion
//! compensation, 4:2:2/4:4:4 chroma, spatial/SNR/temporal scalability, and
//! the intra VLC table / alternate scan combinations that belong to a
//! lower-priority extensions pass rather than this crate's core decode
//! path.
//!
//! # Modules
//!
//! [`tables`]: every VLC/constant table, each cited in
//! `provenance/vaco-codec-mpeg12.toml`. [`vlc`]: the one generic
//! prefix-code decoder every table in [`tables`] is read through.
//! [`headers`]: `sequence_header()` through `picture_coding_extension()`
//! parsing. [`block`]: one block's entropy-coded coefficients to a
//! dequantised, inverse-transformed 8x8 residual. [`motion`]:
//! motion-vector prediction and half-pel interpolation. [`picture`]: the
//! reference-frame ring predictions read from. [`decoder`]: the `Decoder`
//! trait implementation driving all of the above.

#![forbid(unsafe_code)]

mod block;
mod decoder;
mod headers;
mod macroblock;
mod motion;
mod picture;
pub mod tables;
mod vlc;

pub use decoder::Mpeg12Decoder;

use vaco_codec_core::{Caps, CodecId, DecoderDesc};
use vaco_core::MediaType;
use vaco_limits::Limits;

fn make(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(Mpeg12Decoder::new(limits))
}

/// The registry descriptor for the MPEG-1 video decoder. MPEG-1 and MPEG-2
/// share one decoder ([`Mpeg12Decoder`]) because they share the entire
/// syntax except `sequence_extension()`'s presence — see [`headers`]'s docs.
pub const DECODER_MPEG1: DecoderDesc = DecoderDesc {
    name: "mpeg1video",
    long_name: "MPEG-1 video",
    id: CodecId::Mpeg1video,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make,
};

/// The registry descriptor for the MPEG-2 video decoder. See
/// [`DECODER_MPEG1`].
pub const DECODER_MPEG2: DecoderDesc = DecoderDesc {
    name: "mpeg2video",
    long_name: "MPEG-2 video",
    id: CodecId::Mpeg2video,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make,
};
