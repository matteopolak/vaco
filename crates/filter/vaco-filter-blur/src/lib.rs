//! T2 blur and sharpen video filters.
//!
//! FT-4.6a (GitHub #468). The reference's own grouping for "blur, sharpen
//! and convolution" was probed directly (`ffmpeg -filters`, `ffmpeg -h
//! filter=<name>`, 2026-08-23) rather than trusted from the brief that
//! requested this crate — but the brief's own group boundary was also
//! wrong, and the authority for it turned out to be a plan document rather
//! than the reference binary: `planning/16-filters.md` §4.2 puts
//! convolution and morphology (`convolution`, `sobel`, `prewitt`,
//! `roberts`, `scharr`, `kirsch`, `dilation`, `erosion`, `median`, and
//! more) in a *separate* crate, `vaco-filter-convolve`, which is where that
//! code now lives — this crate shipped it first under the wrong name,
//! caught by the orchestrator reading the plan's own crate-decomposition
//! table before this issue closed. `maskedclamp` turned out to belong to a
//! third crate again (`vaco-filter-key`) and was dropped entirely from
//! both.
//!
//! `vaco-filter-blur` itself owns: `unsharp`, `cas`, `avgblur`, `gblur`,
//! `dblur`, `varblur`, `yaepblur`, `guided`, `boxblur`, `smartblur`, `sab`
//! (eleven names). Four are implemented here; see [`registry`]'s module
//! doc for which, and why the other seven are a follow-up rather than a
//! silent gap.
//!
//! Built against `vaco-filter-core` (the `FrameFilter` trait, the `Simple`
//! adapter), exactly as `vaco-filter-convolve` is.
//!
//! # Shape
//!
//! * [`common`] — 8-bit-only plane helpers shared by every filter here:
//!   format validation, frame metadata copying, the `planes` bitmask, and
//!   [`common::box_pass`], the clamp-bordered box average [`boxblur`],
//!   [`avgblur`] and [`unsharp`] all build on.
//! * One module per filter, each exposing `pub const DESC: FilterDesc` and
//!   `pub(crate) fn create`, aggregated by [`registry::BlurRegistry`].
//!
//! # What is verified versus structural
//!
//! Framecrc-level confidence (interior pixels, against small generated
//! inputs run through the reference binary directly): `boxblur`,
//! `avgblur`. Interior-verified with a documented border gap: `unsharp`
//! (analytic ramp invariant plus one measured off-by-one at the very
//! edge). Structural only, not compared against the reference's actual
//! algorithm: `gblur` (the reference's impulse response is not a plain
//! discrete Gaussian — see [`gblur`]'s doc for the measurement that found
//! this and the scope decision it led to). See
//! `docs/filter/vaco-filter-blur.md` for the full accounting.
//!
//! # Left for a follow-up (out of this brief's time budget)
//!
//! `cas`, `dblur`, `varblur`, `yaepblur`, `guided`, `sab`, `smartblur` —
//! seven more filters this crate's own roadmap row names. Each is a
//! genuinely different algorithm from what is implemented here (AMD's
//! published Contrast Adaptive Sharpen formula, a directional/rotated
//! blur, a per-pixel radius driven by a second video stream, an
//! edge-preserving variance-gated blend, the He et al. guided filter, a
//! shape-adaptive multi-pass blur) rather than a variation on
//! `common::box_pass`, and none of them were reached in this pass. None of
//! them block the four filters that did land.

#![forbid(unsafe_code)]
#![allow(
    clippy::many_single_char_names,
    reason = "x/y/w/h/dx/dy are the natural names for pixel coordinates and \
              kernel offsets throughout this crate's image-processing math, \
              exactly as vaco-filter-video-geometry::crop allows the same \
              lint for the same reason"
)]

pub mod avgblur;
pub mod boxblur;
mod common;
pub mod gblur;
pub mod registry;
pub mod unsharp;

pub use registry::BlurRegistry;
