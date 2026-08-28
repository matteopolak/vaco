//! ITU-T H.261 and baseline ITU-T H.263 video decode.
//!
//! `Vaco-Spec-Ref: itu-t-h261` (03/93, the free base text) and
//! `itu-t-h263` (03/96, the free base text, before any Annex D-U
//! extension — see "Scope" below).
//!
//! # Scope
//!
//! H.261: picture/GOB/macroblock/block layers, all mandatory features.
//! H.263: the mandatory baseline syntax only — Unrestricted Motion Vector,
//! Syntax-based Arithmetic Coding, Advanced Prediction and PB-frames
//! (Annexes D, E, F, G, all optional per `PTYPE`'s own mode bits) are a
//! later extensions pass, not this crate. Encode is out of scope entirely.
//!
//! # Modules
//!
//! [`tables`]: every VLC/constant table for both formats, cited in
//! `provenance/vaco-codec-h263.toml`. [`vlc`]: the generic prefix-code
//! decoder. [`block`]: coefficient decode, dequantisation, IDCT — shared
//! shape, format-specific formulas. [`motion`]: half-pel interpolation
//! (shared) and vector prediction (format-specific: H.261's previous-
//! macroblock DPCM vs. H.263's median-of-three). [`picture`]: the
//! reference-frame the decoder reads predictions from. [`h261`]/[`h263`]:
//! each format's own `Decoder` trait implementation.

#![forbid(unsafe_code)]

mod block;
mod deblock;
mod h261;
mod h263;
mod motion;
mod picture;
mod plus;
pub mod tables;
mod vlc;

pub use h261::H261Decoder;
pub use h263::H263Decoder;

use vaco_codec_core::{Caps, CodecId, DecoderDesc};
use vaco_core::MediaType;
use vaco_limits::Limits;

fn make_h261(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(H261Decoder::new(limits))
}

fn make_h263(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(H263Decoder::new(limits))
}

/// The registry descriptor for the H.261 decoder.
pub const DECODER_H261: DecoderDesc = DecoderDesc {
    name: "h261",
    long_name: "H.261",
    id: CodecId::H261,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_h261,
};

/// The registry descriptor for the H.263 decoder (baseline syntax only —
/// see this crate's own module docs for what that excludes).
pub const DECODER_H263: DecoderDesc = DecoderDesc {
    name: "h263",
    long_name: "H.263 / H.263-1996, H.263+ / H.263-1998 / H.263 version 2",
    id: CodecId::H263,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_h263,
};
