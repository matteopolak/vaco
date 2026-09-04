//! Video test-pattern and procedural generator sources.
//!
//! `nullsrc`, `color`, and `nullsink` are **not** registered here because they
//! already ship from `vaco-filter-plumbing`; `pal100bars`/`pal75bars` already
//! ship from `vaco-filter-video-source`. Re-registering any of those five
//! names would be a second, competing `[[component]]` row for the same `ctor`,
//! which `cargo xtask dup-check` exists to catch.
//!
//! `testsrc` is left out for the same reason `vaco-filter-video-source`
//! left it out: the reference overlays a rendered timestamp using its own
//! bitmap glyph table, which needs a font/glyph rasteriser this crate does
//! not have.
//!
//! `testsrc2` is also left out, for a related but distinct reason: unlike
//! `testsrc`, `-h filter=testsrc2` has no text-related option at all, so no
//! glyph rasteriser is needed — but probing its animated moving-checker
//! pattern (`ffmpeg -f lavfi -i testsrc2=size=32x24 -f rawvideo -pix_fmt
//! yuv420p -frames:v N -`, compared frame to frame) did not resolve to a
//! formula this crate could verify with confidence in the time available:
//! fitting the animation's period and the header gradient bands' exact
//! boundaries left unexplained residuals. Shipping a guessed pixel pattern
//! under a name the project explicitly calls out as an oracle for other
//! filters' conformance tests is worse than not shipping it — the same call
//! `vaco-filter-video-source` made for `testsrc`/`smptebars` in its own
//! row. See `docs/filter/vaco-filter-source.md` for the full list of what
//! is and is not implemented, and the per-generator exactness table.
//!
//! # Shape
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private `create`, dispatched by [`registry::GeneratorRegistry`]. Same
//! pattern as every sibling filter crate.
#![forbid(unsafe_code)]
// `x`, `y`, `w`, `h`, `r`, `g`, `b` are this domain's own names: pixel
// coordinates, frame dimensions and colour channels. Renaming them to
// `horizontal_position` etc. would make the per-pixel formulas above harder
// to check against the module docs' own math, not easier — the same
// reasoning `vaco-tx` gives for keeping FFT literature's single-letter names.
#![allow(
    clippy::many_single_char_names,
    reason = "pixel coordinates, frame dimensions and colour channels are this domain's own names"
)]

pub mod allrgb;
pub mod allyuv;
pub mod bars;
pub mod cellauto;
pub mod colorchart;
pub mod colorspectrum;
pub mod gradients;
pub mod life;
pub mod mandelbrot;
pub mod perlin;
pub mod rgbtestsrc;
pub mod sierpinski;
pub mod yuvtestsrc;
pub mod zoneplate;

mod rng;

pub mod registry;

pub use registry::GeneratorRegistry;
