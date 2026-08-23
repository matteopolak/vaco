//! T2/T3 convolution and morphology video filters.
//!
//! Split out of `vaco-filter-blur` (GitHub issue #468/FT-4.6a) after the
//! orchestrator corrected the crate boundary against the authoritative
//! table in `planning/16-filters.md` §4.2: `vaco-filter-blur` owns
//! `unsharp, cas, avgblur, gblur, dblur, varblur, yaepblur, guided,
//! boxblur, smartblur, sab`, and this crate — `vaco-filter-convolve` —
//! owns `convolution, morpho, erosion, dilation, inflate, deflate, median,
//! sobel, prewitt, roberts, scharr, kirsch, edgedetect, blurdetect,
//! convolve, deconvolve, corr, xcorrelate`. The brief that spawned the
//! original crate had merged the two families; the code did not move
//! because it was wrong, only because it was filed under the wrong name.
//!
//! Built against `vaco-filter-core` (the `FrameFilter` trait, the `Simple`
//! adapter), exactly as `vaco-filter-blur` is, and now also against
//! `vaco-filter-framesync` (the `FrameSyncFilter` trait, the `Synced`
//! adapter) for [`morpho`]'s two video inputs.
//!
//! # Shape
//!
//! * [`common`] — 8-bit-only plane helpers: format validation, the
//!   `planes` bitmask, frame metadata copying, clamp-to-edge sampling. A
//!   deliberate fork of `vaco-filter-blur::common`'s non-`box_pass` half —
//!   see that module's doc for why it is not a shared dependency.
//! * [`convolution`] — the generic per-plane matrix engine, also the
//!   `convolution` filter itself, reused by [`edge`] for the two-gradient
//!   `sobel`/`prewitt`/`scharr` engine.
//! * [`morph`] — the shared dilation/erosion/inflate/deflate engine
//!   (fixed 3x3 neighbourhood), and, via `apply_structured`, the arbitrary
//!   structuring-element engine [`morpho`] uses.
//! * One module per filter, each exposing `pub const DESC: FilterDesc` and
//!   `pub(crate) fn create`, aggregated by
//!   [`registry::ConvolveRegistry`].
//!
//! # What is verified versus structural
//!
//! Framecrc-level confidence (interior pixels, against small generated
//! inputs run through the reference binary directly): `convolution`,
//! `sobel`, `prewitt`, `scharr`, `dilation`, `erosion`, `median`,
//! `inflate`, `deflate`. Interior-verified with a documented border gap:
//! `roberts`, `kirsch` (the border does not fit any boundary model tried,
//! and is flagged unverified rather than guessed — see [`edge`]'s and
//! [`kirsch`]'s docs for the measurements, including a wrong-mask/
//! wrong-divisor pair that cancelled into a false match on the first
//! pass). `morpho`'s `erode`/`dilate` core is measured directly against
//! the reference (see [`morpho`]'s doc); `open`/`close`/`gradient`/
//! `tophat`/`blackhat` are standard compositions of that measured core,
//! verified via the anti-extensive/extensive mathematical-morphology
//! invariants rather than probed individually. See
//! `docs/filter/vaco-filter-convolve.md` for the full accounting.
//!
//! # Left for a follow-up (out of this brief's time budget)
//!
//! `edgedetect`, `blurdetect`, `convolve`, `deconvolve`, `corr`,
//! `xcorrelate` — six more filters `planning/16-filters.md` §4.2 counts in
//! this crate. `edgedetect`'s own hysteresis/edge-tracing stage was
//! measured to behave in a way a plain Sobel-plus-double-threshold model
//! does not reproduce (see the crate's report for the probe); `convolve`/
//! `deconvolve`/`corr`/`xcorrelate` are frequency-domain, two-video-stream
//! operations that want `vaco-tx` matched carefully to the reference's
//! exact windowing and normalisation, and `blurdetect` is a
//! wavelet-decomposition-based metric — none were reached in the time this
//! pass had. None of them block the twelve filters that did land.

#![forbid(unsafe_code)]
#![allow(
    clippy::many_single_char_names,
    reason = "x/y/w/h/dx/dy are the natural names for pixel coordinates and \
              kernel offsets throughout this crate's image-processing math, \
              exactly as vaco-filter-video-geometry::crop allows the same \
              lint for the same reason"
)]

mod common;
pub mod convolution;
pub mod deflate;
pub mod dilation;
mod edge;
pub mod erosion;
pub mod inflate;
pub mod kirsch;
pub mod median;
mod morph;
pub mod morpho;
pub mod prewitt;
pub mod registry;
pub mod roberts;
pub mod scharr;
pub mod sobel;

#[cfg(test)]
mod tests_graph;

pub use registry::ConvolveRegistry;
