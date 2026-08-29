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

/// Benchmark-only window into `nlmeans`'s internal plane filter
/// (`pub(crate)` since `PlaneBuf` itself is internal machinery, not this
/// crate's public surface). Takes/returns plain `Vec<f32>` planes rather
/// than `PlaneBuf` so nothing internal has to become actually `pub`.
/// `benches/nlmeans.rs` is the only intended caller.
#[doc(hidden)]
pub mod bench_support {
    use crate::nlmeans::{nlmeans_plane, nlmeans_plane_naive};
    use crate::video::PlaneBuf;

    fn to_buf(data: &[f32], width: usize, height: usize, max_val: f32) -> PlaneBuf {
        let mut buf = PlaneBuf::zeroed(width, height, max_val);
        for y in 0..height {
            for x in 0..width {
                if let Some(&v) = data.get(y * width + x) {
                    buf.set(x, y, v);
                }
            }
        }
        buf
    }

    /// The integral-image fast path, as shipped.
    #[must_use]
    pub fn nlmeans_fast(
        data: &[f32],
        width: usize,
        height: usize,
        max_val: f32,
        h: f32,
        pr: i64,
        rr: i64,
    ) -> Vec<f32> {
        nlmeans_plane(&to_buf(data, width, height, max_val), h, pr, rr)
            .as_slice()
            .to_vec()
    }

    /// The brute-force `O(w*h*(2rr+1)^2*(2pr+1)^2)` reference the fast path
    /// replaced.
    #[must_use]
    pub fn nlmeans_naive(
        data: &[f32],
        width: usize,
        height: usize,
        max_val: f32,
        h: f32,
        pr: i64,
        rr: i64,
    ) -> Vec<f32> {
        nlmeans_plane_naive(&to_buf(data, width, height, max_val), h, pr, rr)
            .as_slice()
            .to_vec()
    }
}

pub use registry::DenoiseRegistry;
