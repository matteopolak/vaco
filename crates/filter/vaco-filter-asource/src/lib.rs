//! Audio test-signal and FIR-coefficient generator sources: plan 16 §4.3's
//! `vaco-filter-asource` row.
//!
//! FT-4.13a (GitHub #481). `anullsrc` and `anullsink` in that row are
//! **not** registered here — they already ship from `vaco-filter-plumbing`
//! (FT-4.3 / GitHub #467). Re-registering either name would be a second,
//! competing `[[component]]` row for the same `ctor`, which `cargo xtask
//! dup-check` exists to catch.
//!
//! `afirsrc` and `afireqsrc` are not implemented: both need the
//! frequency-sampling FIR design method (interpolate a frequency response
//! onto a bin grid, inverse-FFT, window), which needs `vaco-tx` wired up
//! correctly (bin layout, conjugate symmetry for a real output, the
//! circular shift for linear phase) — real signal-processing surface area
//! this crate did not have time to implement and verify correctly. See
//! this crate's closing report (GitHub #481) and
//! `docs/filter/vaco-filter-asource.md` for what that would take.
//!
//! # Shape
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private `create`, dispatched by [`registry::AsourceRegistry`].
//! Same pattern as every sibling filter crate.
#![forbid(unsafe_code)]
// `n`, `t`, `x` are this domain's own names -- sample index, time in
// seconds, and the FIR/window design literature's own variable. Same
// reasoning `vaco-tx` gives for its own single-letter names.
#![allow(
    clippy::many_single_char_names,
    reason = "sample index, time, and DSP/window design literature's own variable names"
)]

pub mod aevalsrc;
pub mod afdelaysrc;
pub mod anoisesrc;
pub mod hilbert;
pub mod sinc;
pub mod sine;

mod rng;
mod window;

pub mod registry;

pub use registry::AsourceRegistry;

/// Convert a finite media duration into source samples without floating-point
/// clock arithmetic.
pub(crate) fn sample_budget(duration: vaco_core::Duration, sample_rate: u32) -> u64 {
    let rate = i32::try_from(sample_rate.max(1)).unwrap_or(1);
    duration
        .to_ticks_rounding(
            vaco_core::Rational::new(1, rate),
            vaco_core::Rounding::NearestAwayFromZero,
        )
        .and_then(|samples| u64::try_from(samples.max(0)).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use vaco_core::{Duration, Rational};

    #[test]
    fn sample_budget_retains_a_large_awkward_clock_duration() {
        let samples = 9_007_199_254_740_993_i64;
        let duration = Duration::from_ticks(samples, Rational::new(1, 48_000))
            .unwrap_or(Duration::ZERO);
        assert_eq!(super::sample_budget(duration, 48_000), samples as u64);
    }
}
