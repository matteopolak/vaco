//! Video test-pattern sources: `pal100bars`, `pal75bars`.
//!
//! Plan 16 §4.2's `vaco-filter-source` row lists `color`, `testsrc`,
//! `testsrc2`, `smptebars`, `nullsrc` and a dozen more under one crate. This
//! crate is FT-4.4's (GitHub epic #54) child issue for that group, and its
//! actual scope is narrower than the row for two independent reasons, laid
//! out here once rather than in every module:
//!
//! 1. **`color`, `nullsrc`, `anullsrc`, `nullsink`, `anullsink` are already
//!    shipped**, in `vaco-filter-plumbing` (FT-4.3 / GitHub #467) — see that
//!    crate's `lib.rs` doc. Re-registering any of those names here would be
//!    a second, competing `[[component]]` row for the same `ctor` name,
//!    which `cargo xtask gen-registry` and `dup-check` both exist to catch.
//!    `buffer`/`abuffer`/`buffersink`/`abuffersink` are `vaco-filter-core`'s
//!    own privileged `Graph` I/O API, per that crate's `lib.rs` doc, not a
//!    leaf filter at all.
//! 2. **`testsrc`, `testsrc2` and `smptebars` need a pattern this crate has
//!    not measured precisely enough to implement without guessing.**
//!    `testsrc`/`testsrc2` draw a moving gradient, a checkerboard, a clock
//!    hand and rendered text — text rendering is `vaco-filter-text`'s
//!    dependency footprint (a font rasteriser), outside this crate's scope,
//!    and the non-text part of the pattern was not reverse-engineered to the
//!    pixel in the time available. `smptebars` is a three-row layout (top
//!    colour bars, a middle reversal row, a bottom PLUGE/black row) whose
//!    exact proportions did not resolve to a clean formula from a single
//!    probe — see this crate's closing report for the actual measurement
//!    and why it was inconclusive. Shipping a guessed pixel layout under a
//!    name that claims to be a broadcast standard is worse than not shipping
//!    it, so it is left out rather than approximated.
//!
//! What *is* here: [`bars`], the EBU/PAL colour-bar family, which resolved
//! to a clean, fully measured 8-equal-segment layout (see that module's
//! doc) and is registered as `pal100bars` and `pal75bars`.
//!
//! # Shape
//!
//! Same as the sibling filter crates: one module per filter (or, for the
//! two bar filters, one shared module), each exposing `pub const DESC:
//! FilterDesc` and a crate-private `create`, dispatched by
//! [`registry::SourceRegistry`].
#![forbid(unsafe_code)]

pub mod bars;

pub mod registry;

pub use registry::SourceRegistry;

/// Convert a finite media duration into source frames without floating-point
/// clock arithmetic.
pub(crate) fn frame_budget(duration: vaco_core::Duration, rate: vaco_core::Rational) -> u64 {
    duration
        .to_ticks_rounding(rate.inverse(), vaco_core::Rounding::NearestAwayFromZero)
        .and_then(|frames| u64::try_from(frames.max(0)).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use vaco_core::{Duration, Rational};

    #[test]
    fn frame_budget_retains_a_large_awkward_clock_duration() {
        let frames = 9_007_199_254_740_993_i64;
        let duration = Duration::from_ticks(frames, Rational::new(1_001, 30_000))
            .unwrap_or(Duration::ZERO);
        assert_eq!(super::frame_budget(duration, Rational::new(30_000, 1_001)), frames as u64);
    }
}
