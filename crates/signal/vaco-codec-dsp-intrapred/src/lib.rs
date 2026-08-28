//! Generic intra prediction primitives shared across codec families.
//!
//! # What this is, and what it is not
//!
//! H.264, HEVC, VP8, VP9 and AV1 each define intra prediction, and each
//! already has (or, for HEVC/AV1, will have) its own crate — H.264/VP8/VP9
//! already ship complete, working, locally-implemented predictors in this
//! tree (`vaco-codec-h264::intra`, `vaco-codec-vp8::predict`,
//! `vaco-codec-vp9::predict`), not touched here. What HEVC's 35-mode
//! angular scheme and AV1's directional-prediction scheme share, and what
//! this crate provides, is the *underlying arithmetic* — DC averaging,
//! bilinear-corner "planar" prediction, and the "project each output
//! position along a signed angle and linearly interpolate between the two
//! nearest reference samples" technique both specifications parameterise
//! identically (a signed angle in 1/32-sample units, a 5-bit interpolation
//! weight). This crate implements that shared arithmetic once; each
//! format's own mode-to-angle table, reference-sample smoothing/filtering
//! and chroma cross-component prediction stay in that format's own crate,
//! since those genuinely differ between formats (HEVC has 33 angular modes
//! at one set of angles, AV1 a different set, and only HEVC has strong
//! intra smoothing).
//!
//! # Confidence level (see `AGENT-CONSTRAINTS.md`'s tiered-confidence
//! guidance, reproduced here because it is the operative fact about this
//! crate)
//!
//! [`angular_project`] implements the widely documented "linear projection
//! along a signed angle" technique, parameterised the way ITU-T H.265
//! §8.4.4.2.6 and `AOMedia` AV1 §7.11.2.4 both describe it — but this pass
//! did not check the *exact* indexing convention line-by-line against a
//! primary specification edition (tier 3 in the constraints document's
//! sense); it was checked against the properties the arithmetic must have
//! (zero-angle is an exact copy; a linear reference ramp interpolates
//! exactly; the interpolation weight halves at the exact midpoint) — tier 1
//! self-consistency, not tier 3 verification. A caller wiring this into a
//! real HEVC or AV1 decoder should re-derive the exact reference-array
//! indexing against the primary text for that format before trusting
//! byte-exact output, and this doc should be updated once that happens.
//! [`dc_predict`] and [`planar_predict`] are simple enough (an average; a
//! bilinear corner blend) that this caveat does not apply to them in the
//! same way — see their own docs.
//!
//! # No allocation
//!
//! Every function writes into a caller-provided output buffer.
#![forbid(unsafe_code)]
// `#[inline(always)]` on a SIMD kernel body is not a tuning knob in this
// crate: it is how the dispatched level's target-feature context reaches
// the body. A kernel that fails to inline is compiled at the ambient
// baseline -- still correct, silently slow, and invisible to every
// correctness test.
#![allow(
    clippy::inline_always,
    reason = "mandatory for target-feature propagation in vaco_simd kernel bodies"
)]

mod angular;
mod dc;
mod planar;
pub mod simd;

pub use angular::angular_project;
pub use dc::dc_predict;
pub use planar::planar_predict;
