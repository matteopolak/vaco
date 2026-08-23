//! `vaco-filter-lut` — the 3D/Hald LUT family of `planning/16-filters.md`
//! §4.2's crate table.
//!
//! # Scope, honestly stated
//!
//! The plan's row for this crate is four filters — `lut1d`, `lut3d`,
//! `haldclut`, `haldclutsrc` — and all four are verified against
//! `ffmpeg -filters`/`ffmpeg -h filter=<name>` (8.1) with no discrepancy in
//! either direction. All four are now implemented: [`lut3d`] and
//! [`haldclut`] from a prior (mis-scoped) brief, [`lut1d`] and
//! [`haldclutsrc`] added in this pass.
//!
//! What is **not** implemented, honestly: `.3dl`/`.dat`/`.m3d` file
//! parsing for `lut3d`'s `file` option (only `.cube` is parsed — see
//! [`lut3d`]'s module doc's "Attempted and abandoned" section for the two
//! probes that failed to recover `.3dl`'s header/mesh syntax without
//! guessing); `lut1d`/`lut3d`'s `cubic`/`cosine`/`spline`/`tetrahedral`/
//! `pyramid`/`prism` interpolation modes (fall back to linear/trilinear);
//! `lut3d`/`haldclut`'s non-default `DOMAIN_MIN`/`DOMAIN_MAX`; `haldclut`'s
//! `clut=first` (always behaves like `clut=all`).
//!
//! # Shape
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private `create`, dispatched by [`registry::LutRegistry`]. See
//! [`sample`] for the shared bit-depth access this crate's filters are
//! written against, and [`lut3d::Cube3d`] for the shared trilinear/nearest
//! 3D-LUT sampler [`haldclut`] also uses.

#![forbid(unsafe_code)]

pub mod sample;

mod common;

pub mod haldclut;
pub mod haldclutsrc;
pub mod lut1d;
pub mod lut3d;

pub mod registry;

pub use registry::LutRegistry;
