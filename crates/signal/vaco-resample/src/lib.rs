//! Audio sample-format conversion, channel rematrixing and rate conversion.
//!
//! The `swresample` equivalent. Implements `planning/17-scale-resample-tx.md`
//! Part B.
//!
//! # What it is
//!
//! Three independent stages plus a composition of them:
//!
//! | Type | Job |
//! |---|---|
//! | [`convert::convert`] | sample-format conversion, packed ↔ planar |
//! | [`MixMatrix`] / [`Rematrix`] | channel layout remapping and mixing |
//! | [`RateConvert`] | polyphase sample-rate conversion |
//! | [`Dither`] | quantisation dither for the down-conversion path |
//! | [`Resampler`] | all four, wired together |
//!
//! Each stage is public and independently usable. A codec that only needs
//! `s16 → f32` calls [`convert::convert`]; a mixer that only needs 5.1 → stereo
//! builds a [`MixMatrix`]. The reference exposes only the fused context.
//!
//! # The numeric contract
//!
//! Every rounding decision in this crate is a *measured* property of the
//! reference binary, not a design choice. [`convert`] states each one with the
//! probe that established it. The two that matter most:
//!
//! * **float → `s16` rounds half **up** (toward +∞), not half-away-from-zero and
//!   not ties-to-even** — but only in the reference's vector kernel. See
//!   [`convert::F32_TO_S16_TAIL_DIVERGENCE`], which is a genuine D17.1 case: the
//!   reference's rounding is not a function of the sample value alone.
//! * **integer narrowing is an arithmetic shift**, not round-half-up. Plan 17
//!   §B.3.2 specifies round-half-up; the reference truncates. We follow the
//!   reference (D17).
//!
//! # Rate conversion is exact-rational
//!
//! 44100 → 48000 is 147/160. The phase accumulator is a pair of integers, never
//! a float, so a stream of any length cannot drift (§B.5.1). With
//! `exact_rational` (the default) and a reduced denominator ≤
//! [`rate::MAX_EXACT_PHASES`], there is no phase quantisation error at all.
//!
//! # What is not here
//!
//! Deliberately scoped out for this pass, each with a stated reason in
//! `docs/signal/vaco-resample.md`: noise-shaping dither curves (the option names
//! are accepted and aliased to triangular-highpass with a warning), and
//! timestamp compensation beyond [`Resampler::delay`] / [`Resampler::next_pts`].
//! The Dolby Pro Logic IIx/IIz/EX/Headphone `matrix_encoding` values are
//! implemented, in the sense that matters: the reference itself falls back to
//! an unencoded downmix for all four, and [`mix::build_matrix`] reproduces
//! that fallback rather than rejecting the option.

#![forbid(unsafe_code)]

pub mod buf;
pub mod convert;
pub mod design;
pub mod dither;
pub mod mix;
pub mod opts;
pub mod rate;
mod resampler;

pub use buf::{AudioMut, AudioRef, AudioSpec};
pub use dither::{Dither, DitherMethod};
pub use mix::{MatrixEncoding, MatrixShape, MixLevels, MixMatrix, Rematrix, build_matrix};
pub use opts::{Engine, FilterType, ResampleOptions};
pub use rate::RateConvert;
pub use resampler::{Resampler, default_layout};

/// The crate's error type is the shared taxonomy.
pub use vaco_core::Error;

/// `Result` with the shared error type.
pub type Result<T, E = Error> = core::result::Result<T, E>;
