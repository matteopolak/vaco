//! Generic separable FIR motion compensation (D-08a).
//!
//! One const-generic engine for the horizontal/vertical FIR pass every
//! block-based codec's sub-pixel interpolation reduces to, plus the border
//! replication ("edge emulation") a motion vector needs whenever a block
//! reaches past the visible picture. Consumers (H.264, HEVC, VP8, VP9, AV1)
//! supply their own [`fir::TapSet`] and pixel source; this crate does not
//! parse a bitstream or know about any one codec's coefficient tables beyond
//! the two worked, spec-cited examples in [`fir::taps`].
//!
//! See `docs/signal/vaco-codec-dsp-mc.md` for the design rationale, in
//! particular why the tap loop uses the "reload and re-widen" structure
//! rather than the more clever `slide`/batched forms — that choice is a
//! measured result, not a guess (`vaco-simd`'s own
//! `benches/adoption.rs` Group 4 measured the alternatives directly).

#![forbid(unsafe_code)]
// Mandatory on the tap-loop body in `fir.rs`, not a tuning knob: it is how a
// dispatched level's target-feature context reaches the function. See
// `vaco_simd`'s own crate docs for the full rationale; turned off once here
// rather than annotated per function.
#![allow(
    clippy::inline_always,
    reason = "mandatory for target-feature propagation in the dispatched FIR body"
)]

pub mod edge;
pub mod fir;
