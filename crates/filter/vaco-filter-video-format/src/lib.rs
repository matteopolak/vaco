//! Video format/metadata filters: `format`, `noformat`, `setsar`, `setdar`,
//! `setparams`, `setfield`, `setrange`, `fps`. Covers most of
//! GitHub epic #54's real child issue #463 (`vaco-filter-scale`: `scale`,
//! `format`, `noformat`, `setsar`, `setdar`, `setparams` — `scale` itself
//! lives in the sibling `vaco-filter-video-geometry` crate) plus `fps` from
//! #465. See `lib.rs`'s own crate-doc note in that sibling crate, and this
//! crate's `docs/filter/vaco-filter-video-format.md`, for the full
//! naming/scope reconciliation against the plan.
//!
//! # Shape
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private `create(&Instantiate) -> Result<Instance, String>`,
//! dispatched by [`registry::FormatRegistry`] — the same shape as
//! `vaco-filter-audio`, `vaco-filter-plumbing` and `vaco-filter-video-geometry`.
//! Every per-filter `Options`/`State` type is `pub(crate)`, for the same
//! `dup-check` reason those crates give.
//!
//! # What is real versus what is structural
//!
//! `format`'s negotiation constraint, `setsar`/`setdar`'s SAR-only-ever-
//! overwrites-SAR behaviour and `fps`'s hold/duplicate/drop algorithm are
//! measured against `ffmpeg 8.1` (`docs/filter/vaco-filter-video-format.md`
//! has the full table).
//!
//! # `framerate` moved out (2026-08-28)
//!
//! This crate used to register a **structural-only** `framerate` stand-in
//! (`fps`-style duplication, no blending) with a doc note saying the real
//! implementation belongs in `vaco-filter-motion` "once `vaco-filter-vdsp`
//! exists" (plan 16 §4.1/§4.2). `vaco-filter-vdsp` now exists and
//! `vaco-filter-motion` now implements `framerate` for real (measured
//! per-pixel cross-fade blend plus a whole-frame scene-cut gate — see that
//! crate's own doc). `cargo xtask gen-registry` refuses two crates
//! registering the same filter name, so the stand-in is removed here
//! rather than left to collide; nothing else in this crate depended on it.
#![forbid(unsafe_code)]

pub mod format;
pub mod fps;
pub mod noformat;
pub mod setdar;
pub mod setfield;
pub mod setparams;
pub mod setrange;
pub mod setsar;

pub mod registry;

pub use registry::FormatRegistry;
