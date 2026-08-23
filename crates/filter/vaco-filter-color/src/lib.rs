//! `vaco-filter-color` — the colour/LUT family of `planning/16-filters.md`
//! §4.2's crate table.
//!
//! # Scope, honestly stated
//!
//! The plan's row for this crate is 29 filters: `curves`, `colorbalance`,
//! `colorchannelmixer`, `colorcontrast`, `colorcorrect`, `colorize`,
//! `colorlevels`, `colortemperature`, `huesaturation`, `hue`, `vibrance`,
//! `exposure`, `selectivecolor`, `grayworld`, `greyedge`, `normalize`,
//! `monochrome`, `midequalizer`, `lut`, `lutrgb`, `lutyuv`, `lut2`, `geq`,
//! `pseudocolor`, `colormap`, `limitdiff`, `tonemap`, `eq`, `histeq`,
//! `colormatrix`. This pass implements six of them —
//! [`colorchannelmixer`], [`lut`] (which registers `lut`, `lutrgb` and
//! `lutyuv`), [`lut2`] and [`pseudocolor`] — because that is what a prior
//! (mis-scoped) brief for this crate covered before a correction placed it
//! on this row; the other 23 are not started. Each is a real GitHub-issue
//! sized unit of work in its own right and none is silently stubbed here.
//!
//! `lut3d`/`haldclut` are **not** in this crate — `planning/16-filters.md`
//! §4.2 gives them their own row, `vaco-filter-lut`, alongside `lut1d` and
//! `haldclutsrc`.
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
pub mod lut;
pub mod lut2;
pub mod pseudocolor;

pub mod registry;

pub use registry::ColorRegistry;
