//! Linear predictive coding: analysis and synthesis.
//!
//! # What this is
//!
//! Three formats in this tree code audio as an autoregressive predictor plus
//! a residual: FLAC's `LPC` subframe type, ALAC's adaptive predictor, and
//! the LPC half of Opus SILK. Each is a genuinely different algorithm
//! (FLAC: classic Levinson-Durbin from windowed autocorrelation, fixed
//! integer coefficients and shift; ALAC: a proprietary sign-sign adaptive
//! filter with no analysis step at all; SILK: NLSF-quantised coefficients
//! recovered by a step-up recursion, not autocorrelation), so this crate
//! does not attempt one function that serves all three. What genuinely is
//! shared, and is what this crate provides:
//!
//! - [`autocorrelate`] and [`levinson_durbin`] — the textbook analysis pair
//!   (Rabiner & Schafer, *Digital Processing of Speech Signals*, and
//!   equivalently Makhoul 1975) that FLAC-style encoders use to turn a
//!   windowed sample block into predictor coefficients.
//! - [`quantize`] — turning float coefficients into the small integer +
//!   shift pair every bitstream format that stores LPC coefficients
//!   actually transmits.
//! - [`predict`] and [`synthesize`] — the fixed-point AR reconstruction
//!   step (`sample = residual + (sum(coeff * history) >> shift)`), which is
//!   the exact arithmetic IETF RFC 9639 (FLAC, 2024) §9.2.6 defines for its
//!   `LPC` subframe and which every other fixed-point linear predictor
//!   (this crate's own [`synthesize`] doc, and SILK's local
//!   `vaco-codec-opus::silk::decode` synthesis loop) shares the *shape* of,
//!   even where the surrounding format differs (SILK works in Q12 with an
//!   `f32` accumulator rather than FLAC's plain integer shift).
//!
//! **Not implemented here, on purpose**: FLAC's own windowing (Welch/Tukey
//! or otherwise — that choice is an encoder policy, not shared math), NLSF
//! quantisation and the reflection-coefficient step-up recursion SILK's
//! `nlsf_to_lpc` performs (a different derivation from the same "reflection
//! coefficients determine an LPC filter" fact, not sharable without forcing
//! SILK's already-working decoder onto a new dependency it does not need),
//! and coefficient stabilisation. `vaco-codec-flac`'s encoder and
//! `vaco-codec-simple-audio`'s comfort-noise analysis call this crate;
//! ALAC/SILK each already ship a complete, working, locally-implemented
//! predictor. This crate keeps classic LPC in one place, per D19, rather
//! than a fourth local copy.
//!
//! # Provenance
//!
//! Autocorrelation and Levinson-Durbin are textbook signal processing
//! (Levinson 1947, Durbin 1960; the standard modern presentation is
//! Rabiner & Schafer §8.3) with no single implementation to be derived
//! from — every DSP textbook states the same recursion. The fixed-point
//! synthesis arithmetic is transcribed from IETF RFC 9639 §9.2.6, a
//! published IETF standard, fetched directly
//! (`https://www.rfc-editor.org/rfc/rfc9639.txt`) and never from any
//! decoder's source.
//!
//! # No allocation
//!
//! The LPC order this crate supports is capped at [`MAX_ORDER`] (32, the
//! largest order any format in this tree's roadmap uses — FLAC's own limit
//! is exactly 32), so every coefficient buffer is a fixed-size array on the
//! stack rather than a `Vec`. A caller that has already validated order
//! from an untrusted bitstream should clamp it to `MAX_ORDER` before
//! calling in; every function here also clamps defensively so an
//! out-of-range order degrades to the largest order actually computed
//! rather than panicking.
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

mod analysis;
mod quantize;
pub mod simd;
mod synthesis;

pub use analysis::{LevinsonDurbin, autocorrelate, levinson_durbin};
pub use quantize::{QuantizedLpc, quantize};
pub use synthesis::{predict, synthesize};

/// Largest predictor order this crate computes. FLAC's own subframe format
/// caps `LPC order` at 32 (a 5-bit field storing `order - 1`), which is also
/// the largest order any codec in this tree's roadmap uses.
pub const MAX_ORDER: usize = 32;

/// Runtime-dispatched autocorrelation for encoder analysis.
///
/// Unlike [`autocorrelate`], this may reassociate floating-point additions
/// within a lag's dot product. It is appropriate where LPC analysis chooses
/// an encoder candidate but does not define decoded sample arithmetic; use
/// [`autocorrelate`] when a strict scalar reduction order is required.
pub fn autocorrelate_dispatched(samples: &[f64], out: &mut [f64]) {
    simd::autocorrelate(vaco_simd::Caps::detect(), samples, out);
}
