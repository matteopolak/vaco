//! T2 blur, sharpen and convolution video filters.
//!
//! FT-4.6a (GitHub #468). The reference's own grouping was probed directly
//! (`ffmpeg -filters`, `ffmpeg -h filter=<name>`, 2026-08-23) rather than
//! trusted from the brief, per this project's standing "measure, don't
//! recall" practice — see [`registry`]'s module doc for the final list and
//! the brief's discrepancies.
//!
//! Built against `vaco-filter-core` (the `Filter`/`FrameFilter` traits, the
//! `Simple` adapter) and, for the one three-input filter in this crate
//! (`maskedclamp`), `vaco-filter-framesync` (`FrameSyncFilter`, `Synced`).
//!
//! # Shape
//!
//! * [`common`] — 8-bit-only plane helpers shared by every filter here:
//!   format validation, frame metadata copying, the `planes` bitmask, and
//!   [`common::box_pass`], the clamp-bordered box average [`boxblur`],
//!   [`avgblur`] and [`unsharp`] all build on.
//! * [`convolution`] — the generic per-plane matrix engine, also the base
//!   [`edge`] reuses for `sobel`/`prewitt`/`scharr`.
//! * [`edge`] — the shared two-gradient (`Gx`/`Gy` magnitude) engine for
//!   `sobel`/`prewitt`/`scharr`. [`roberts`] and [`kirsch`] are separate
//!   modules: measured to have different border behaviour from the three
//!   that share this engine (see [`edge`]'s doc).
//! * [`morph`] — the shared dilation/erosion engine.
//! * One module per filter, each exposing `pub const DESC: FilterDesc` and
//!   `pub(crate) fn create`, aggregated by [`registry::BlurRegistry`].
//!
//! # What is verified versus structural
//!
//! Framecrc-level confidence (interior pixels, against small generated
//! inputs run through the reference binary directly): `boxblur`,
//! `avgblur`, `convolution`, `sobel`, `prewitt`, `scharr`, `dilation`,
//! `erosion`, `median`. Interior-verified with a documented border gap:
//! `unsharp` (analytic ramp invariant plus one measured off-by-one at the
//! very edge), `roberts` and `kirsch` (interior matches a measured probe;
//! the border does not fit any boundary model tried, and is flagged
//! unverified rather than guessed). Structural only, not compared against
//! the reference's actual algorithm: `gblur` (the reference's impulse
//! response is not a plain discrete Gaussian — see [`gblur`]'s doc for the
//! measurement that found this and the scope decision it led to) and
//! `maskedclamp` (a pure per-pixel formula read directly off the option
//! table, with no neighbourhood or border question to probe). See
//! `docs/filter/vaco-filter-blur.md` for the full accounting.
//!
//! # Left for a follow-up (out of this brief's time budget)
//!
//! `smartblur`, `bilateral`, `guided`, `sab`, `dblur`, `varblur`,
//! `yaepblur`, `cas`, `tmedian`, `xmedian`, `morpho`, `convolve`,
//! `deconvolve` — thirteen more filters this project's own roadmap
//! (`planning/16-filters.md` §8.4, "T2 blur/sharpen/convolve (~28)") counts
//! in the same family. `convolve`/`deconvolve` need an FFT matched
//! bit-exactly to the reference to be worth shipping at all; the rest are
//! each a genuinely different per-pixel algorithm (adaptive radius,
//! bilateral range weighting, a structuring-element second input) rather
//! than a variation on what this crate already built, and none of them
//! block the fourteen filters that did land.

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
pub mod convolution;
pub mod dilation;
mod edge;
pub mod erosion;
pub mod gblur;
pub mod kirsch;
pub mod maskedclamp;
pub mod median;
mod morph;
pub mod prewitt;
pub mod registry;
pub mod roberts;
pub mod scharr;
pub mod sobel;
pub mod unsharp;

pub use registry::BlurRegistry;
