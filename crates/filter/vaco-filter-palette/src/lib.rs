//! Palette video filters: [`palettegen`], [`paletteuse`], and [`elbg`].
//! `latticepal` is unavailable because the installed reference has no such
//! filter to measure against.
//!
//! All filters use the original Heckbert (1982) median-cut quantizer in
//! [`quantize`], not a transcription of the reference implementation.
//!
//! - [`palettegen`] accumulates an 8-bit RGB histogram (ignoring alpha) for a
//!   whole stream and emits one `16x16` RGBA palette. Its `stats_mode=diff` and
//!   `single` options currently use full-stream accumulation.
//! - [`paletteuse`] chooses the nearest palette color by Euclidean RGB distance
//!   with no dithering. This is deliberately simpler than `sierra2_4a` error
//!   diffusion.
//! - [`elbg`] posterizes one frame with median cut, not iterative ELBG. Its
//!   `nb_steps` and `seed` options are accepted but do not affect deterministic
//!   median-cut output.
//!
//! Inputs must be addressable, non-hardware, non-palette RGBA. Each relevant
//! pad requests exact `Rgba`, so negotiation inserts conversion instead of
//! misreading another byte layout.

#![forbid(unsafe_code)]

pub mod elbg;
pub mod palettegen;
pub mod paletteuse;
pub mod quantize;
pub mod registry;

pub use registry::PaletteRegistry;
