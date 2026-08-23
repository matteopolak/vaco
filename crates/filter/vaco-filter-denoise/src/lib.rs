//! T4 video denoise filters (FT-4.6b, GitHub #469): `hqdn3d`, `atadenoise`,
//! `removegrain`, `nlmeans`, `owdenoise`, `dctdnoiz`, `fftdnoiz`,
//! `vaguedenoiser`. `bm3d` is named in the brief's group but is not
//! implemented here — see [`bm3d`]'s doc for why.
//!
//! # Membership, checked against the reference rather than assumed
//!
//! The brief's list was verified against `ffmpeg -hide_banner -filters`
//! (ffmpeg 8.1, 2026-08-23) rather than trusted, per this project's standing
//! practice (D17: measure, don't recall). Filtering that output to `V->V`
//! (or `N->V` for `bm3d`) rows whose description mentions denoising gives
//! exactly nine names: `atadenoise`, `bm3d`, `dctdnoiz`, `fftdnoiz`,
//! `hqdn3d`, `nlmeans`, `owdenoise`, `removegrain`, `vaguedenoiser`. That is
//! the brief's list precisely — nothing to add, nothing to drop. (`afftdn`
//! and `afwtdn` also match "denois" but are `A->A`; they belong to an audio
//! denoise/dynamics work package, not this one.)
//!
//! # Shape
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private `fn create`, aggregated by [`registry::DenoiseRegistry`] —
//! the same shape `vaco-filter-audio-eq` and `vaco-filter-video-geometry`
//! use. [`video`] is the shared plane-decode/encode helper every filter
//! module is built on; [`wavelet`] is the Haar transform and thresholding
//! shared by [`owdenoise`] and [`vaguedenoiser`].
//!
//! # Pixel format coverage
//!
//! Every filter here processes the planar `grayN`/`yuvN` family (any bit
//! depth up to 16, one component per plane) via [`video::sample_layout`].
//! Semi-planar (`nv12`) and packed (`rgb24`) formats are out of scope — see
//! that module's doc — and a filter asked to process one returns
//! [`vaco_core::Error::Unsupported`] rather than silently miscomputing.
//!
//! # What is verified versus structural
//!
//! None of these are checked byte-for-byte against the reference: every one
//! is a real denoising algorithm derived from the format's public option
//! semantics and, where a named public algorithm exists (Non-local Means,
//! wavelet shrinkage), from that algorithm's paper — never from `FFmpeg`'s
//! source (D7). Each is held to an *independent* oracle instead of a
//! byte-identity target: see the per-module doc for which one, and
//! `docs/filter/vaco-filter-denoise.md` for the full accounting, including
//! the one confirmed case (`hqdn3d`) where the reference's own option table
//! prints a default that measurably is not the default it uses.
#![forbid(unsafe_code)]

pub mod atadenoise;
mod bm3d;
pub mod dctdnoiz;
pub mod fftdnoiz;
pub mod hqdn3d;
pub mod nlmeans;
pub mod owdenoise;
pub mod registry;
pub mod removegrain;
pub mod vaguedenoiser;
mod video;
mod wavelet;

pub use registry::DenoiseRegistry;
