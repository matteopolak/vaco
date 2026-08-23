//! `vaco-filter-color` — the colour/LUT family of `planning/16-filters.md`
//! §4.2's crate table.
//!
//! # Scope, honestly stated
//!
//! The plan's row for this crate is 29 filters, all verified against
//! `ffmpeg -filters`/`ffmpeg -h filter=<name>` (8.1) with no discrepancy
//! from the plan in either direction: `curves`, `colorbalance`,
//! `colorchannelmixer`, `colorcontrast`, `colorcorrect`, `colorize`,
//! `colorlevels`, `colortemperature`, `huesaturation`, `hue`, `vibrance`,
//! `exposure`, `selectivecolor`, `grayworld`, `greyedge`, `normalize`,
//! `monochrome`, `midequalizer`, `lut`, `lutrgb`, `lutyuv`, `lut2`, `geq`,
//! `pseudocolor`, `colormap`, `limitdiff`, `tonemap`, `eq`, `histeq`,
//! `colormatrix`.
//!
//! Seven are implemented: [`colorchannelmixer`], [`lut`] (which registers
//! `lut`, `lutrgb` and `lutyuv`), [`lut2`] and [`pseudocolor`], from a
//! prior (mis-scoped) brief for this crate; [`colorlevels`], added in
//! this pass. Each of the other 22 is a real GitHub-issue-sized unit of
//! work in its own right and none is silently stubbed here.
//!
//! Three filters were investigated and specifically **not** shipped,
//! because probing them surfaced real complexity rather than a formula a
//! couple of measurements could pin down — worth recording so the next
//! pass does not re-discover the same walls from nothing:
//!
//! - `colorbalance`: the per-channel shadows/midtones/highlights
//!   weighting is not a simple threshold or linear ramp. A sweep of
//!   `rs=1.0`'s effect on `R` found a **flat plateau** from `v=0` to
//!   `v≈24` (constant `delta=178`), then a non-linear falloff to `0` by
//!   `v=64` — ruled out a simple piecewise-linear or single-parameter
//!   curve, and reconstructing the real one needs more probing than this
//!   pass had time for. The scale-with-`rs` relationship *is* confirmed
//!   linear (`delta(v=0) ≈ 178 * rs`, checked at four values).
//! - `exposure`, `grayworld`: both force `gbrpf32le` output — planar
//!   32-bit float, not integer samples — which this crate's whole
//!   [`sample`] engine deliberately excludes
//!   ([`PixFmtFlags::FLOAT`](vaco_pixfmt::PixFmtFlags::FLOAT)). Needs a
//!   float-sample accessor this crate does not have, not just a new
//!   filter module.
//! - `geq`, `tonemap`: flagged in advance as the likely ones to leave (a
//!   full expression-evaluated generator, and dynamic-range conversion,
//!   respectively) — not investigated in this pass either, for the same
//!   reason.
//!
//! `lut3d`/`haldclut`/`lut1d`/`haldclutsrc` are **not** in this crate —
//! `planning/16-filters.md` §4.2 gives them their own row,
//! `vaco-filter-lut`, which closes all four.
//!
//! # Shape
//!
//! One module per filter (or filter family), each exposing `pub const
//! DESC: FilterDesc` and a crate-private `create`, dispatched by
//! [`registry::ColorRegistry`] — the same shape as the sibling filter
//! crates. See [`sample`] for the shared bit-depth-independent pixel
//! access this crate's filters are all written against.

#![forbid(unsafe_code)]

pub mod sample;

mod common;

pub mod colorchannelmixer;
pub mod colorlevels;
pub mod lut;
pub mod lut2;
pub mod pseudocolor;

pub mod registry;

pub use registry::ColorRegistry;
