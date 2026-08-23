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
