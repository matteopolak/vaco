//! The clock, behind one door.
//!
//! # Why this crate exists
//!
//! Vaco must eventually compile for `wasm32`, where `std::time::Instant::now()`
//! and `SystemTime::now()` are not merely unavailable — on
//! `wasm32-unknown-unknown` they **panic at runtime**, which is the worst
//! possible failure mode for a workspace that `deny`s `unwrap_used` and `panic`
//! precisely to keep untrusted input from reaching a crash.
//!
//! Rather than sprinkle `#[cfg(target_family = "wasm")]` through every crate
//! that wants a deadline or a seed, the clock lives here and nowhere else. This
//! is the one-door adapter rule applied to a platform capability instead of to an
//! external crate: one door, so the port is one file.
//!
//! # The two clocks are deliberately different types
//!
//! [`Instant`] is monotonic and exists on every target. [`unix_nanos`] is wall
//! clock and returns `Option`, because a target genuinely may not have one.
//! Keeping them apart stops the common bug of using a wall clock for elapsed
//! time, and forces callers to say what they do without a calendar.
//!
//! # What happens on wasm today
//!
//! Without the `web` feature, [`Instant`] is a stopped clock and [`unix_nanos`]
//! returns `None`. Both are honest about it, and both are *total* — nothing
//! panics. The consequences are chosen so the failure is safe:
//!
//! - A deadline set *forward* from now never fires, because the clock never
//!   advances past it. (One set in the past still fires immediately — the
//!   comparison is honest, the clock is just frozen.) Deadlines are a
//!   last-resort guard, so a guard that does not trip is far better than a
//!   panic, and the byte and element budgets in `vaco-limits` are unaffected —
//!   those are the limits that actually bound an attacker.
//! - A seed falls back to a fixed constant. `parse::color("random")` is
//!   documented as carrying no statistical claim, so this weakens nothing that
//!   was promised.
//! - An expression's `time` builtin reads 0.
//!
//! Turning the `web` feature on is the intended fix, backing both with
//! `web-time` (`performance.now()` and `Date.now()`). It is declared and
//! documented but not yet wired, because adding a dependency before there is a
//! wasm build to test it against would be adopting a crate on faith — every
//! adoption is a reviewed decision. See `docs/core/vaco-time.md`.

#![forbid(unsafe_code)]
#![no_std]

pub use core::time::Duration;

/// A monotonic instant.
///
/// Mirrors the part of `std::time::Instant` Vaco actually uses. Deliberately
/// **not** `std::time::Instant`, so that a crate cannot accidentally depend on
/// the std type and lose portability without the compiler noticing.
///
/// On a target with no clock this is a *stopped* clock: every `now()` returns
/// the same value, so every elapsed duration is zero and a deadline set forward
/// from now never trips. It is still monotonic, still total, and never panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(Repr);

/// Nanoseconds since an unspecified origin. `u64` covers 584 years, which is
/// more than any process needs, and keeps the type `Copy` and cheap to compare.
type Repr = u64;

impl Instant {
    /// The current monotonic time.
    #[must_use]
    pub fn now() -> Self {
        Self(backend::monotonic_nanos())
    }

    /// Time elapsed since `earlier`, saturating at zero rather than panicking
    /// the way `std::time::Instant::sub` does on a non-monotonic reading.
    #[must_use]
    pub fn duration_since(self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    /// Time elapsed since this instant.
    #[must_use]
    pub fn elapsed(self) -> Duration {
        Self::now().duration_since(self)
    }

    /// This instant advanced by `d`, saturating at the representable maximum.
    ///
    /// Saturating rather than wrapping matters: a deadline built by adding a
    /// large duration must stay in the future, and a wrap would put it in the
    /// past and trip immediately.
    #[must_use]
    pub const fn saturating_add(self, d: Duration) -> Self {
        let ns = d.as_nanos();
        // `u64::MAX` nanoseconds is ~584 years; anything past it saturates.
        let ns = if ns > u64::MAX as u128 {
            u64::MAX
        } else {
            ns as u64
        };
        Self(self.0.saturating_add(ns))
    }

    /// This instant moved back by `d`, saturating at the origin.
    ///
    /// Saturating rather than wrapping for the mirror-image reason to
    /// [`saturating_add`](Self::saturating_add): a wrap would put a deliberately
    /// past deadline far in the future and stop it ever firing.
    #[must_use]
    pub const fn saturating_sub(self, d: Duration) -> Self {
        let ns = d.as_nanos();
        let ns = if ns > u64::MAX as u128 {
            u64::MAX
        } else {
            ns as u64
        };
        Self(self.0.saturating_sub(ns))
    }

    /// Whether this target has a real monotonic clock.
    ///
    /// Exposed so a caller can say "deadlines are unenforceable here" once, at
    /// startup, instead of silently doing nothing forever.
    #[must_use]
    pub const fn is_available() -> bool {
        backend::MONOTONIC_AVAILABLE
    }
}

/// Block the current thread for approximately `d`.
///
/// The third time door, and here for the same reason as the other two: sleeping
/// is an OS capability that `wasm32-unknown-unknown` does not have, and a crate
/// that reaches for `std::thread::sleep` directly has quietly become
/// non-portable without the compiler saying so.
///
/// # It does not sleep everywhere, so do not rely on it for correctness
///
/// Where there is no thread to block this returns immediately, and
/// [`can_sleep`] says so. A polling loop must therefore be bounded by an
/// iteration count as well as by a deadline — bounding it by the clock alone is
/// exactly the bug this crate makes visible, because [`Instant`] is also
/// stopped on such a target and `now() < deadline` stays true forever.
///
/// `vaco-protocol-file`'s `follow` read is the worked example.
pub fn sleep(d: Duration) {
    backend::sleep(d);
}

/// Whether [`sleep`] actually blocks on this target.
#[must_use]
pub const fn can_sleep() -> bool {
    backend::CAN_SLEEP
}

/// Nanoseconds since the Unix epoch, or `None` where there is no wall clock.
///
/// Separate from [`Instant`] because it answers a different question and has a
/// different availability story. Never use it to measure elapsed time — it can
/// jump backwards.
#[must_use]
pub fn unix_nanos() -> Option<u128> {
    backend::unix_nanos()
}

// ---------------------------------------------------------------- backends

#[cfg(not(target_family = "wasm"))]
mod backend {
    extern crate std;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub(crate) const MONOTONIC_AVAILABLE: bool = true;
    pub(crate) const CAN_SLEEP: bool = true;

    pub(crate) fn sleep(d: core::time::Duration) {
        std::thread::sleep(d);
    }

    /// A fixed origin, so `Instant` can be a plain integer. Taken once.
    fn origin() -> std::time::Instant {
        use std::sync::OnceLock;
        static ORIGIN: OnceLock<std::time::Instant> = OnceLock::new();
        *ORIGIN.get_or_init(std::time::Instant::now)
    }

    pub(crate) fn monotonic_nanos() -> u64 {
        // Saturates at ~584 years of uptime, which is not a case worth a branch
        // anywhere else in the codebase.
        u64::try_from(origin().elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    pub(crate) fn unix_nanos() -> Option<u128> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_nanos())
    }
}

#[cfg(target_family = "wasm")]
mod backend {
    // No clock. Both functions are total and constant — see the crate docs for
    // why each fallback is the safe direction, and turn on `web` to fix it.
    pub(crate) const MONOTONIC_AVAILABLE: bool = false;
    pub(crate) const CAN_SLEEP: bool = false;

    /// Returns immediately: there is no thread to block. Callers must bound
    /// their loop by a count, not only by a deadline — see [`super::sleep`].
    pub(crate) fn sleep(_d: core::time::Duration) {}

    pub(crate) fn monotonic_nanos() -> u64 {
        0
    }

    pub(crate) fn unix_nanos() -> Option<u128> {
        None
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn monotonic_never_goes_backwards() {
        let a = Instant::now();
        let b = Instant::now();
        assert!(b >= a);
        // Zero elapsed is legitimate — a coarse clock, or no clock at all.
        assert_eq!(a.duration_since(b), Duration::ZERO);
    }

    #[test]
    fn duration_since_saturates_instead_of_panicking() {
        let a = Instant::now();
        let later = a.saturating_add(Duration::from_secs(10));
        assert_eq!(later.duration_since(a), Duration::from_secs(10));
        // The argument order std would panic on.
        assert_eq!(a.duration_since(later), Duration::ZERO);
    }

    #[test]
    fn adding_an_absurd_duration_stays_in_the_future() {
        let a = Instant::now();
        let far = a.saturating_add(Duration::from_secs(u64::MAX));
        assert!(far >= a, "a saturating add must never wrap into the past");
    }

    #[test]
    fn sleeping_and_the_clock_agree() {
        // The dangerous combination is a target that can sleep but has no
        // clock, or vice versa: a polling loop bounded by a deadline would
        // then either spin without waiting or wait without ever expiring.
        // Neither exists today, and this test is where it would be noticed.
        assert_eq!(can_sleep(), Instant::is_available());
    }

    #[test]
    fn sleep_is_at_least_as_long_as_asked_where_it_sleeps() {
        let d = Duration::from_millis(2);
        let before = Instant::now();
        sleep(d);
        if can_sleep() {
            assert!(before.elapsed() >= d);
        }
    }

    #[test]
    fn availability_matches_the_wall_clock() {
        assert_eq!(Instant::is_available(), unix_nanos().is_some());
    }
}
