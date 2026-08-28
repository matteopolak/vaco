//! Two guards against the two shapes of non-termination plan 13 §2.2.4
//! describes: a component that stops making progress, and one that simply
//! runs too long.
//!
//! # Why two, not one
//!
//! `vaco_limits::ProgressGuard` (re-exported here as
//! [`crate::ProgressGuard`] — see this crate's `lib.rs`) converts a
//! *specific, documented* contract violation — "this stepping call did not
//! advance anything" — into an immediate `LimitError::NoProgress` at a known
//! call site. It needs the caller to know what "progress" means for their
//! API (consumed a packet, produced a frame, advanced the read position), so
//! it is meaningless without that per-call boolean.
//!
//! **This crate does not redefine `ProgressGuard`.** `vaco-limits` (layer 0)
//! already implements plan 13 §2.2.4(a) exactly — it is not fuzz-only, it is
//! the same guard real parsers use in production, which is a stronger
//! guarantee than a fuzz-only reimplementation could ever be, and
//! `cargo xtask dup-check` (D19) rightly refuses two independent copies of
//! the same stepping-contract check drifting apart under one name. A fuzz
//! target reaches it as `vaco_fuzz_support::ProgressGuard`, one import
//! instead of two.
//!
//! [`Guard`], defined here, is the coarser, contract-free fallback: a
//! wall-clock budget on one fuzz iteration, for code that has no notion of
//! "a step" at all (a one-shot parse, a whole-buffer transform). Plan 13
//! §2.2.4(c) is explicit that a wall-clock deadline is "the fallback, never
//! the primary" — reproducible fuel/progress counting should be tried first
//! — so [`Guard`] exists for exactly the fuzz targets that have nothing more
//! structured to check, and has no equivalent elsewhere in the tree.

use std::time::{Duration, Instant};

/// A coarse per-iteration deadline for a fuzz target with no finer-grained
/// progress contract to check.
///
/// `libFuzzer`'s own `-timeout` flag already kills a hung process, but that
/// is an external signal with no information about *where* the target was
/// stuck — plan 13 §2.2.4's whole point is that a `-timeout` firing is a
/// **bug**, and this guard turns it into a panic with a location and a label
/// before libFuzzer's coarser hammer falls.
#[derive(Debug)]
pub struct Guard {
    label: &'static str,
    start: Instant,
    budget: Duration,
}

impl Guard {
    /// Start a guard for one fuzz iteration, allowed to run for `budget`
    /// before [`Guard::check`] panics.
    #[must_use]
    pub fn new(label: &'static str, budget: Duration) -> Self {
        Self {
            label,
            start: Instant::now(),
            budget,
        }
    }

    /// Panics if more than `budget` has elapsed since [`Guard::new`].
    ///
    /// Call this at whatever points in the target body are cheap and
    /// frequent — the top of a decode loop, once per demuxed packet — so a
    /// hang is caught close to where it started rather than only at the
    /// call's very end (which, for a genuine infinite loop, it would never
    /// reach).
    ///
    /// # Panics
    /// When the elapsed time exceeds the configured budget.
    pub fn check(&self) {
        let elapsed = self.start.elapsed();
        assert!(
            elapsed <= self.budget,
            "Guard[{}] exceeded its {:?} budget after {elapsed:?} — this is a hang, \
             not a slow input: minimise it and file a fuzz regression (see \
             AGENT-CONSTRAINTS.md's Fuzzing section)",
            self.label,
            self.budget,
        );
    }

    /// Elapsed time since [`Guard::new`], for a caller that wants to log or
    /// bucket it (e.g. slow-unit detection, plan 13 §2.2.4(d)) rather than
    /// panic.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use std::time::Duration;

    use super::Guard;

    #[test]
    fn guard_does_not_panic_within_budget() {
        let g = Guard::new("t", Duration::from_secs(60));
        g.check();
    }

    #[test]
    #[should_panic(expected = "exceeded its")]
    fn guard_panics_past_budget() {
        let g = Guard::new("t", Duration::from_nanos(1));
        std::thread::sleep(Duration::from_millis(5));
        g.check();
    }

    #[test]
    fn progress_guard_is_vaco_limits_own_type_not_a_second_copy() {
        // Re-export smoke test: this is `vaco_limits::ProgressGuard`'s own
        // documented example (its doc comment), proved reachable through
        // this crate's path instead of a second implementation.
        let mut guard = crate::ProgressGuard::new();
        for _ in 0..1000 {
            guard.tick(true).expect("progress resets the stall count");
        }
        for _ in 0..64 {
            guard.tick(false).expect("64 stalls is still within tolerance");
        }
        assert!(guard.tick(false).is_err(), "the 65th consecutive stall must fail");
    }
}
