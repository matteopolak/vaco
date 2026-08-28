//! Fused scale/widen/narrow sample conversions for codec-internal hot loops.
//!
//! # What this is, and what it is not
//!
//! Every codec that produces PCM internally as a wider or different numeric
//! type than its output sample format needs a small, fixed set of
//! conversions applied per sample, often fused with a scalar gain (an
//! IMDCT's output normalisation constant, a fixed-point predictor's
//! dequantisation factor). This crate is that fixed set — nothing else.
//!
//! It deliberately does **not** overlap `vaco-resample`'s `SampleFmt`-driven
//! N×M conversion matrix (that crate's own `convert` module, D19): that
//! module walks a `Frame`-shaped buffer between any of twelve sample
//! formats, planar or packed, and is the right tool once a `Frame` exists.
//! This crate has no buffer abstraction and no format enum at all — it is
//! called *before* a `Frame` exists, on raw slices straight out of a
//! decoder's transform or predictor state, and every function takes a plain
//! scale factor because that is the shape those call sites actually have.
//!
//! # Rounding is a design choice here, not a measured contract
//!
//! Unlike `vaco-resample::convert`'s numeric rules — which are pinned to
//! `ffmpeg`'s observable `swresample` output because that output is exactly
//! what a byte-identical remux compares against — nothing this crate
//! computes is independently observable through the reference binary's
//! command-line surface: it runs entirely inside a decoder, before any
//! container or filter sees the samples. Round-half-away-from-zero with
//! saturation is used throughout because it is simple, standard, and
//! self-consistent; a codec crate that measures a different rounding rule
//! for its own format is free to not use this crate for that step (the
//! owner ruling in `AGENT-CONSTRAINTS.md` ships differences from the
//! reference when they do not sacrifice quality).
//!
//! # Mismatched lengths do not panic
//!
//! Every function processes `min(src.len(), dst.len())` elements (or, for
//! interleave/deinterleave, `min` across every channel slice too) rather
//! than asserting equal lengths. A codec that got its channel count wrong
//! is a real bug, but garbling or dropping trailing samples is a better
//! failure mode for a DSP primitive than a panic on attacker-reachable
//! sizes, and every caller can check `dst.len()` against what it expected
//! after the call.
//!
//! # No allocation
//!
//! Every function writes into a caller-provided output slice.
#![forbid(unsafe_code)]
// `#[inline(always)]` on a SIMD kernel body is not a tuning knob in this
// crate: it is how the dispatched level's target-feature context reaches
// the body (see `vaco-simd`'s own crate doc for the full reasoning). A
// kernel that fails to inline is compiled at the ambient baseline --
// still correct, silently slow, and invisible to every correctness test.
#![allow(
    clippy::inline_always,
    reason = "mandatory for target-feature propagation in vaco_simd kernel bodies"
)]

mod convert;
mod interleave;
pub mod simd;

pub use convert::{
    clip_i16, clip_i32, clip_u8, float_to_int16, float_to_int32, int16_to_float, int32_to_float,
    int32_to_float_fmul_scalar, scale_float,
};
pub use interleave::{deinterleave_f32, deinterleave_i16, interleave_f32, interleave_i16};
