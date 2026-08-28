//! Shared drawing kernels that cross filter-crate boundaries.
//!
//! Plan `16-filters.md` §4.1's `vaco-filter-draw` row (GitHub #458, FT-3.1):
//! the `drawutils` equivalent — format-aware colour parsing, and plane-correct
//! fill/blend/box operations that work across subsampled chroma and
//! 8-through-16-bit depths, rather than being re-derived per caller.
//!
//! # Why this needs to be its own crate
//!
//! `vaco-filter-draw-vf`'s `drawbox`/`drawgrid` already ship a colour parser
//! and a blend routine, but both are `pub(crate)`, and both are scoped to
//! exactly the case that filter needed: planar RGB, 8-bit, no alpha (see that
//! crate's own doc for why converting an arbitrary colour into a YUV frame's
//! colour model was out of scope there). `overlay`, `drawtext`'s box
//! background, and any filter that composites over a real decoded frame need
//! the general case — YUV as well as RGB, subsampled chroma, 9/10/12/16-bit
//! depths — and per D19 that belongs in one shared crate rather than being
//! re-derived (and re-narrowed) by the next caller. This crate is that home.
//! It does not replace `vaco-filter-draw-vf`'s own narrower copy; that is a
//! migration for whoever next touches that crate, noted rather than done here
//! since this pass does not own it.
//!
//! # What is here
//!
//! - [`color`]: `AVColor`-grammar parsing (`#RRGGBB[AA]`, `0xRRGGBB[AA]`, the
//!   reference's full named-colour table, and an `@alpha` suffix) into
//!   [`color::Rgba`].
//! - [`sample`]: generic component pack/unpack against a
//!   [`vaco_pixfmt::PixFmtDescriptor`] — the piece that makes the rest of this
//!   crate work on any packed-or-planar, 8-or-16-bit format without a
//!   per-format match arm.
//! - [`solid`]: resolves an [`color::Rgba`] into the destination format's own
//!   native code values (RGB channels directly; YUV via
//!   [`vaco_color::MatrixCoefficients`], defaulting to BT.601/limited-range —
//!   see that module's doc for the measurement pinning that default).
//! - [`fill`]: writes a resolved colour into every sample of a region of a
//!   [`vaco_frame::Frame`], chroma-subsampling and bit-depth aware.
//! - [`blend`]: the same region, alpha-composited over the existing content
//!   instead of overwriting it.
//! - [`rect`]: clips an `(x, y, w, h)` rectangle to the frame and to each
//!   plane's own chroma-decimated geometry, and derives a border-only ring
//!   for `thickness`-style box drawing.
//!
//! # What is out of scope
//!
//! Palette, bitstream-packed, hardware-surface and floating-point formats
//! ([`vaco_pixfmt::PixFmtFlags::PALETTE`], `BITSTREAM`, `HW_ACCEL`, `FLOAT`)
//! are rejected with [`vaco_core::Error::Unsupported`] rather than silently
//! misinterpreted — none of this crate's callers need them yet, and guessing
//! a byte layout for them would be exactly the "plausible, wrong frame with
//! no signal anything happened" failure this project's own history warns
//! against.
#![forbid(unsafe_code)]

pub mod blend;
pub mod color;
pub mod fill;
pub mod rect;
pub mod sample;
pub mod solid;

pub use color::Rgba;
pub use rect::Rect;
pub use solid::Solid;
