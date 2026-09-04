//! Geometry video filters dispatched by [`registry::T2GeometryRegistry`].
//!
//! Registered filters are [`scroll`], [`field`], [`il`], [`tile`], [`untile`],
//! [`fillborders`], [`swaprect`], [`swapuv`], [`shuffleframes`],
//! [`shuffleplanes`], [`alphaextract`], [`pixelize`], [`perspective`],
//! [`framepack`], [`mergeplanes`], [`alphamerge`], and [`extractplanes`].
//! `crop`, `pad`, `transpose`, `hflip`, and `vflip` belong to
//! `vaco-filter-video-geometry`; `rotate` belongs to
//! `vaco-filter-video-composite`.
//!
//! [`framepack`] and [`mergeplanes`] use [`Paired`](vaco_filter_core::adapt::Paired),
//! [`extractplanes`] uses [`Fanout`](vaco_filter_core::adapt::Fanout), and
//! [`alphamerge`] uses the framesync adapter because its `eof_action`,
//! `shortest`, `repeatlast`, and `ts_sync_mode` behavior requires it.
//!
//! Deliberately unsupported filters retain these boundaries:
//!
//! - `shear`: a centered formula matched only two of four measured rows.
//! - `lenscorrection`: the radial-distortion normalization is unmeasured.
//! - `shufflepixels`: its seeded PRNG sequence is unidentified.
//! - `addroi`: `vaco_frame::FrameSideData` has no ROI representation.
//! - `ccrepack`: CEA-708 repacking has distinct byte-packing rules.
//! - `stereo3d` and `tiltandshift`: respectively require broad stereo/color
//!   processing and a whole-stream slit-scan buffer.
//!
//! Each filter exposes `DESC` and a crate-private constructor. [`geom`] owns
//! byte-level plane addressing, [`fill`] performs limited-range-correct fills,
//! and [`warp`] with [`sample`] implement `perspective`'s projective sampling.
#![forbid(unsafe_code)]

pub mod alphaextract;
pub mod alphamerge;
pub mod extractplanes;
pub mod field;
pub mod fill;
pub mod fillborders;
pub mod framepack;
mod geom;
pub mod il;
pub mod mergeplanes;
pub mod perspective;
pub mod pixelize;
mod sample;
pub mod scroll;
pub mod shuffleframes;
pub mod shuffleplanes;
pub mod swaprect;
pub mod swapuv;
pub mod tile;
pub mod untile;
mod warp;

pub mod registry;

pub use registry::T2GeometryRegistry;
