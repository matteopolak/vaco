//! Video format/metadata filters: `format`, `noformat`, `setsar`, `setdar`,
//! `setparams`, `setfield`, `setrange`, `fps`, `framerate`. Covers most of
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
//! has the full table). `framerate` is **structural only**: it registers and
//! runs, but performs `fps`-style nearest-frame duplication rather than the
//! reference's motion-compensated blending — see `framerate.rs`'s doc for
//! why that line was drawn where it was.
#![forbid(unsafe_code)]

pub mod format;
pub mod fps;
pub mod framerate;
pub mod noformat;
pub mod setdar;
pub mod setfield;
pub mod setparams;
pub mod setrange;
pub mod setsar;

pub mod registry;

pub use registry::FormatRegistry;
