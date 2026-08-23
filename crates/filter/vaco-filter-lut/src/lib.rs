//! `vaco-filter-lut` — the 3D/Hald LUT family of `planning/16-filters.md`
//! §4.2's crate table.
//!
//! # Scope, honestly stated
//!
//! The plan's row for this crate is four filters: `lut1d`, `lut3d`,
//! `haldclut`, `haldclutsrc`. This pass implements [`lut3d`] and
//! [`haldclut`], carried over from a prior (mis-scoped) brief; `lut1d` and
//! `haldclutsrc` are not started.
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
pub mod lut3d;

pub mod registry;

pub use registry::LutRegistry;
