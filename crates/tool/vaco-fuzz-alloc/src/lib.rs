//! TEST-ONLY. A counting `GlobalAlloc` that aborts above a ceiling. Never in
//! a shipped artifact. See `planning/13-correctness.md` §2.2.3.
//!
//! # What it is
//!
//! The belt-and-braces safety net behind `vaco-limits`: every allocation
//! this project's own parsers make is supposed to be charged against a
//! `Limits` budget first, but "supposed to" is a design discipline, not a
//! proof. `vaco_fuzz_alloc::Counting` wraps [`std::alloc::System`], counts
//! live bytes across the whole process, and aborts with a distinctive
//! message the moment that count crosses a ceiling — catching an allocation
//! that slipped past `Limits` regardless of which crate made it.
//!
//! A fuzz target opts in with:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: vaco_fuzz_alloc::Counting = vaco_fuzz_alloc::Counting;
//! ```
//!
//! This crate never sets `#[global_allocator]` itself — only a binary may
//! have one, and it must be the fuzz target's own choice, not something a
//! library dependency imposes on every consumer.
//!
//! # Why this crate is allowed `unsafe` at all
//!
//! `GlobalAlloc` cannot be implemented in safe Rust — the trait itself is
//! `unsafe fn alloc`/`unsafe fn dealloc`. D2
//! (`planning/00-decisions.md`) forbids `unsafe` everywhere except a closed
//! allowlist (`vaco-hwaccel-*`, `vaco-io-mmap`, `vaco-play-backend-*`,
//! optional `-sys` wrapper crates), and this crate is not on it — the D2
//! text predates this crate. Work package QA-05 (`#176`) names exactly this
//! as part of its own scope: "`vaco-fuzz-alloc` (counting `GlobalAlloc`, D2
//! allowlist entry + the CI assertion it never reaches a shipped artifact)".
//!
//! **What is actually done here**: the crate itself, built exactly to plan
//! 13 §2.2.3's design, with `[lints]` in `Cargo.toml` deliberately *not*
//! inheriting `unsafe_code = "forbid"` (the same shape the D2-allowlisted
//! `vaco-hw-*` crates use) and a `// SAFETY:` comment on the one `unsafe impl`
//! justifying it line by line.
//!
//! **What is not done, and why**: `cargo xtask unsafe-audit`
//! (`xtask/src/unsafe_audit.rs`) currently recognises exactly one exemption —
//! the literal prefix `vaco-hw-` — so it will flag this crate until that
//! list also names `vaco-fuzz-alloc`. `xtask` was under another agent's
//! active ownership (`agent:codec-path`, `#652`, per `planning/ASSIGNMENTS.md`)
//! for the whole of this session, and this batch's own constraints are
//! explicit: "If you need a change in a crate you do not own, stop and
//! report — do not work around it." So that one-line addition is reported
//! here rather than made. Plan 13 §2.2.3's other half — a
//! `tools/unsafe-allowlist.toml` entry recording the review — is new
//! infrastructure this crate does not invent unilaterally either; the
//! module-doc table below stands in for it until that file exists.
//!
//! | Field | Value |
//! |---|---|
//! | `name` | `vaco-fuzz-alloc` |
//! | `reason` | `GlobalAlloc` cannot be implemented in safe Rust; needed as the fuzzing allocation backstop |
//! | `justification_doc` | `planning/13-correctness.md` §2.2.3 |
//! | `in_default_build` | `false` (also recorded in this crate's own `Cargo.toml` under `[package.metadata.vaco]`) |
//! | `test_only` | `true` — CI should assert it appears in no binary's dependency graph, per the same metadata key |
//!
//! # Configuration
//!
//! [`Counting::set_ceiling`] — the default is 256 MiB, matching plan
//! 13 §2.2.3's own number.

#![allow(
    unsafe_code,
    reason = "GlobalAlloc's methods are unsafe fn by definition; see this module's docs for the full D2 accounting"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static CEILING: AtomicUsize = AtomicUsize::new(256 * 1024 * 1024);

/// The distinctive message printed to stderr before the process aborts, so a
/// fuzzer log or crash report is unambiguous about which safety net fired.
pub const CEILING_MESSAGE_PREFIX: &str = "VACO-FUZZ-ALLOC-CEILING";

/// Pure decision logic: would accepting one more allocation of `requested`
/// bytes, on top of `live_before` already-live bytes, cross `ceiling`?
///
/// Split out from [`Counting::alloc`] specifically so it is unit-testable
/// without touching a real allocator or aborting a test process — the
/// abort itself cannot be exercised by a normal `#[test]` (that is the
/// entire point of `std::process::abort`), so this is the boundary the
/// tests below actually check.
#[must_use]
pub const fn would_exceed(live_before: usize, requested: usize, ceiling: usize) -> bool {
    match live_before.checked_add(requested) {
        Some(total) => total > ceiling,
        // An addition that overflows `usize` is certainly over any sane
        // ceiling — treat it as exceeding rather than wrapping past the
        // check, which would be the one way this "backstop" could itself
        // be bypassed by a single enormous request.
        None => true,
    }
}

/// The counting allocator. Zero-sized: every instance shares the same
/// process-wide atomics, which is what makes it valid as a
/// `#[global_allocator]` — there is exactly one per process by construction.
#[derive(Debug, Clone, Copy, Default)]
pub struct Counting;

impl Counting {
    /// Bytes currently counted as live (allocated minus deallocated through
    /// this allocator since the process started).
    #[must_use]
    pub fn live_bytes() -> usize {
        LIVE.load(Ordering::Relaxed)
    }

    /// Change the abort ceiling. Fuzz targets with a legitimately larger
    /// working set (a big raw-video frame buffer, decoded intentionally)
    /// should call this once at start-up rather than disabling the guard.
    pub fn set_ceiling(bytes: usize) {
        CEILING.store(bytes, Ordering::Relaxed);
    }

    /// The current ceiling.
    #[must_use]
    pub fn ceiling() -> usize {
        CEILING.load(Ordering::Relaxed)
    }
}

// SAFETY: every method delegates unchanged to `System` for the actual
// allocation/deallocation — no pointer arithmetic and no aliasing claim of
// this crate's own. The only added behaviour is an atomic counter update
// around each call and, in `alloc`/`alloc_zeroed`, a `process::abort()`
// (never a panic, which could be caught and misreported as normal execution
// by a fuzz harness) when the running total would cross `CEILING`. Ordering
// is `Relaxed` throughout because this is an approximate, best-effort
// backstop, not a precise accounting system — a lost update under race
// would only ever make the ceiling fire a few bytes later or earlier than
// exact, never unsound.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live_before = LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        if would_exceed(live_before, layout.size(), CEILING.load(Ordering::Relaxed)) {
            eprintln!(
                "{CEILING_MESSAGE_PREFIX} live={} req={} ceiling={}",
                live_before + layout.size(),
                layout.size(),
                CEILING.load(Ordering::Relaxed)
            );
            std::process::abort();
        }
        // SAFETY: `layout` is exactly the caller's layout, forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr`/`layout` are exactly the caller's, forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let live_before = LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        if would_exceed(live_before, layout.size(), CEILING.load(Ordering::Relaxed)) {
            eprintln!(
                "{CEILING_MESSAGE_PREFIX} live={} req={} ceiling={}",
                live_before + layout.size(),
                layout.size(),
                CEILING.load(Ordering::Relaxed)
            );
            std::process::abort();
        }
        // SAFETY: `layout` is exactly the caller's layout, forwarded unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_size = layout.size();
        let live_before = LIVE.load(Ordering::Relaxed);
        let live_without_old = live_before.saturating_sub(old_size);
        if new_size > old_size
            && would_exceed(
                live_without_old,
                new_size,
                CEILING.load(Ordering::Relaxed),
            )
        {
            eprintln!(
                "{CEILING_MESSAGE_PREFIX} live={} req={} ceiling={}",
                live_without_old + new_size,
                new_size,
                CEILING.load(Ordering::Relaxed)
            );
            std::process::abort();
        }
        // SAFETY: `ptr`/`layout`/`new_size` are exactly the caller's,
        // forwarded unchanged; `System.realloc` has the same safety
        // contract as this function.
        let result = unsafe { System.realloc(ptr, layout, new_size) };
        if !result.is_null() {
            LIVE.fetch_add(new_size, Ordering::Relaxed);
            LIVE.fetch_sub(old_size, Ordering::Relaxed);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::would_exceed;

    #[test]
    fn under_ceiling_does_not_exceed() {
        assert!(!would_exceed(0, 100, 1000));
        assert!(!would_exceed(500, 400, 1000));
    }

    #[test]
    fn exactly_at_ceiling_does_not_exceed() {
        assert!(!would_exceed(0, 1000, 1000));
    }

    #[test]
    fn one_byte_over_exceeds() {
        assert!(would_exceed(0, 1001, 1000));
        assert!(would_exceed(999, 2, 1000));
    }

    #[test]
    fn an_overflowing_request_always_exceeds() {
        assert!(would_exceed(usize::MAX - 5, 10, usize::MAX));
    }

    #[test]
    fn default_ceiling_matches_the_plans_own_number() {
        // 256 MiB, per plan 13 §2.2.3's `Counting` example.
        assert_eq!(super::Counting::ceiling(), 256 * 1024 * 1024);
    }
}
