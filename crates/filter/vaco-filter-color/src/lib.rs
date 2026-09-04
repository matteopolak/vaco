//! Colour correction and lookup-table filters.
//!
//! Implemented names are `colorchannelmixer`, `colorlevels`, `colormatrix`,
//! `exposure`, `hue`, `limitdiff`, `lut`, `lutrgb`, `lutyuv`, `lut2`, and
//! `pseudocolor`. Each module exposes a descriptor and constructor through
//! [`registry::ColorRegistry`]; unsupported names are not silently stubbed.
//!
//! [`sample`] provides shared integer access up to 16 bits and separate
//! IEEE-754 `f32` access. [`exposure`] uses the float path with `gbrpf32le`;
//! its module documents the measured arithmetic order required for bit-exact
//! integer-exposure cases. [`hue`] intentionally accepts constant `h` and `s`
//! values rather than the reference's time-varying expression language, and
//! leaves brightness parsed but inert because its measured response is not a
//! single linear term.
//!
//! Several absent filters require distinct algorithms rather than registry
//! work. `grayworld` needs a measured LAB-space global-average algorithm.
//! `geq` needs a full expression-driven generator, while `tonemap` needs
//! dynamic-range conversion. A `colorbalance` probe found `rs=1.0` produced a
//! flat `delta=178` plateau for inputs 0 through about 24, followed by a
//! nonlinear falloff to zero by 64; four `rs` probes confirmed the plateau
//! scales linearly, but did not determine the falloff curve.
//!
//! The `lut3d`, `haldclut`, `lut1d`, and `haldclutsrc` filters belong to the
//! separate `vaco-filter-lut` crate.

#![forbid(unsafe_code)]

pub mod sample;

mod common;

pub mod colorchannelmixer;
pub mod colorlevels;
pub mod colormatrix;
pub mod exposure;
pub mod hue;
pub mod limitdiff;
pub mod lut;
pub mod lut2;
pub mod pseudocolor;

pub mod registry;

pub use registry::ColorRegistry;
