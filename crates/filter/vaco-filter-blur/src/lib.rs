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
//! (eleven names). Nine are implemented here; see [`registry`]'s module
//! doc for which, and why `sab`/`smartblur` are a follow-up rather than a
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
//! algorithm, each with a measured refutation of the naive reading and an
//! independent algebraic invariant in place of a framecrc pin: `gblur` (IIR
//! impulse response, not FIR — see [`gblur`]'s doc), `cas` (published AMD
//! formula shape confirmed, exact constants not solved — see [`cas`]'s
//! doc), `dblur` (measured asymmetric/order-dependent response rules out a
//! plain symmetric kernel — see [`dblur`]'s doc), `yaepblur` (measured
//! sigma-dependent blend trend confirmed, exact weight formula not solved —
//! see [`yaepblur`]'s doc), `varblur` (two measured anomalies, including a
//! non-identity `radius=0` case — see [`varblur`]'s doc), `guided`
//! (`guidance=off` only, published He et al. formula implemented directly
//! but not probed against the reference — see [`guided`]'s doc). See
//! `docs/filter/vaco-filter-blur.md` for the full accounting.
//!
//! # Left for a follow-up (out of this brief's time budget)
//!
//! `sab` (shape-adaptive blur, a multi-pass per-pixel-adaptive-radius
//! algorithm) and `smartblur` (edge-aware blur) — the two filters this
//! crate's own roadmap row still does not implement. `guided=on` (a second,
//! external guide stream) and `guided`'s fast/subsampled mode are also
//! deliberately unimplemented — see [`guided`]'s doc — and rejected at
//! creation rather than silently downgraded to the self-guided case. None
//! of them block the nine filters that did land.

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
pub mod cas;
mod common;
pub mod dblur;
pub mod gblur;
pub mod guided;
pub mod registry;
pub mod unsharp;
pub mod varblur;
pub mod yaepblur;

#[cfg(test)]
mod tests_graph;

/// Benchmark-only window into this crate's internal box-average engine
/// (`common::box_pass`/`box_pass_naive`, both `pub(crate)` since they are
/// filter-internal machinery rather than this crate's own public surface).
/// `benches/box_pass.rs` is the only intended caller — nothing in the
/// filter graph should reach through here.
#[doc(hidden)]
pub mod bench_support {
    use crate::common::{self, Rounding};

    fn rounding(trunc: bool) -> Rounding {
        if trunc { Rounding::Trunc } else { Rounding::Nearest }
    }

    /// The `O(w*h)` sliding-window box average, as shipped.
    #[must_use]
    pub fn box_pass_fast(
        rows: &[&[u8]],
        w: i32,
        h: i32,
        rx: i32,
        ry: i32,
        trunc: bool,
    ) -> Vec<Vec<u8>> {
        common::box_pass(rows, w, h, rx, ry, rounding(trunc))
    }

    /// The `O(w*h*(2rx+1)*(2ry+1))` brute-force reference the fast path
    /// replaced, kept as the correctness oracle and the pre-optimisation
    /// baseline this benchmark compares against.
    #[must_use]
    pub fn box_pass_naive(
        rows: &[&[u8]],
        w: i32,
        h: i32,
        rx: i32,
        ry: i32,
        trunc: bool,
    ) -> Vec<Vec<u8>> {
        common::box_pass_naive(rows, w, h, rx, ry, rounding(trunc))
    }
}

pub use registry::BlurRegistry;
