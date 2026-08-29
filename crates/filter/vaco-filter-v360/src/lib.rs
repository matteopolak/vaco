//! `v360` — 360-degree video projection conversion, converting between
//! projections of 360-degree ("spherical") video, such as extracting a
//! normal flat view from an equirectangular source.
//!
//! # Scope: two projections of the reference's twenty-five, plus rotation
//!
//! The reference's real `v360` (measured: `ffmpeg -h filter=v360`, this
//! environment's `ffmpeg` 9.0.1) supports 25 named projections
//! (`equirect`, `c3x2`/`c6x1`/`c1x6` cubemaps, `eac`, `dfisheye`, `flat`,
//! `barrel`, `sg`, `mercator`, `ball`, `hammer`, `sinusoidal`, `fisheye`,
//! `pannini`, `cylindrical`, `tetrahedron`, `barrelsplit`, `tsp`,
//! `hequirect`, `equisolid`, `og`, `octahedron`, `cylindricalea`), stereo
//! (`sbs`/`tb`) input/output, cubemap face order/rotation/padding, h/v/d
//! FOV in every combination, off-axis offsets, and per-axis flips — around
//! 5100 LOC upstream per this project's own plan doc. This crate ships
//! [`geometry::Projection::Equirect`] and [`geometry::Projection::Flat`]
//! (the reference's `flat`/`rectilinear`/`gnomonic`, a plain rectilinear
//! camera), in every direction (equirect->equirect re-orientation,
//! equirect->flat "extract a normal view from a 360 video", flat->equirect
//! "insert a normal video into a 360 canvas"), plus `yaw`/`pitch` together
//! and `roll` alone (**not** all three combined at once — see below) and
//! `h_flip`/`v_flip`. Every other named projection is rejected with a
//! clear [`vaco_core::Error::Unsupported`] naming it, rather than
//! silently misprojected.
//!
//! # Geometry is measured, not assumed — including the one place it did
//! # not fit
//!
//! The sign convention for each of `yaw`/`pitch`/`roll`, the equirect/
//! rectilinear formulas, and the `yaw`+`pitch` composition order were all
//! pinned down by probing the real reference binary with single-pixel
//! marker images and a stronger off-axis reverse check (place a marker at
//! a known world direction, find where the reference moved it to, and
//! confirm that pixel's own local ray reproduces the marker under the
//! candidate formula) — not from any specification or source. `yaw` turns
//! the view toward increasing longitude, `pitch` tilts up, `roll` rotates
//! the view counter-clockwise in on-screen (`x`-right, `y`-up) terms, and
//! `yaw`+`pitch` compose as `Yaw(Pitch(·))`, all confirmed off-axis.
//!
//! **Composing `roll` together with `yaw` and/or `pitch` was investigated
//! and does not fit any of the 6 possible orderings** of the three
//! rotations against that same off-axis check (best error ~10% of a unit
//! vector — tens of degrees, not a rounding gap). Rather than ship one
//! ordering as an unverified guess, [`v360::Filter::new`] refuses that
//! combination with a clear error; `roll` alone and `yaw`+`pitch` together
//! remain fully supported. See [`geometry`]'s own doc for the full
//! measurement, and `vaco-filter-color`'s `colorize`/`eq` for this
//! project's precedent of investigating a formula and not shipping it
//! when it does not fit.
//!
//! # What is not attempted
//!
//! Every projection but the two named above; stereo 3D; cubemap face
//! order/rotation/padding; `h_fov`/`v_fov`/`d_fov` FOV-linking (this crate
//! takes `h_fov`/`v_fov` independently, defaulting each to 90 degrees for
//! `flat` when left at the reference's own "auto" sentinel `0`, rather
//! than reproducing the reference's own cross-derivation between
//! `h_fov`/`v_fov`/`d_fov`/aspect ratio); off-axis `h_offset`/`v_offset`;
//! `alpha_mask`; non-default `rorder`; 9/10/12/16-bit sample depths (this
//! crate's per-plane sampling is byte-per-sample only, like
//! `vaco-filter-motion`'s warp).

#![forbid(unsafe_code)]

pub mod geometry;
pub mod registry;
pub mod v360;

pub use registry::V360Registry;
