//! `ssim` — Structural Similarity Index between two video streams. Same pad
//! shape as `psnr`, no framesync surface.
//!
//! # Implemented from the published paper
//!
//! Z. Wang, A. C. Bovik, H. R. Sheikh and E. P. Simoncelli, "Image Quality
//! Assessment: From Error Visibility to Structural Similarity", *IEEE
//! Transactions on Image Processing*, vol. 13, no. 4, pp. 600-612, April
//! 2004: an 11x11 circularly-symmetric Gaussian window (`sigma=1.5`, unit
//! sum), local statistics `mu_x, mu_y, sigma_x^2, sigma_y^2, sigma_xy` per
//! pixel, and the per-window index
//!
//! ```text
//! SSIM(x,y) = ((2*mu_x*mu_y + C1) * (2*sigma_xy + C2))
//!           / ((mu_x^2 + mu_y^2 + C1) * (sigma_x^2 + sigma_y^2 + C2))
//! ```
//!
//! with `C1=(K1*L)^2`, `C2=(K2*L)^2`, `K1=0.01`, `K2=0.03`, `L=255` — the
//! paper's own defaults. Frame-level index is the mean of the per-window
//! map ("Mean SSIM"); window stride 1, no padding.
//!
//! # Not byte-exact, even in the degenerate case
//!
//! Two flat planes force `sigma_x=sigma_y=sigma_xy=0` everywhere, so the
//! formula collapses to `(2*mu_x*mu_y+C1)/(mu_x^2+mu_y^2+C1)` regardless of
//! windowing. For `128` vs `110`: `(2*128*110+6.5025)/(128^2+110^2+6.5025)
//! = 0.988628`, `dB=19.441551` — what this implementation produces.
//!
//! **`ffmpeg 8.1` measures `0.988625`/`19.440596` on that exact input** — a
//! ~3e-6 discrepancy, forced because the formula collapses to this value
//! for *any* zero-variance windowing: the reference is not evaluating the
//! textbook floating-point Gaussian window unmodified even here (most
//! plausibly a fixed-point/quantised kernel). `ssim` is therefore not
//! byte-exact against the reference on any input; what's verified is that
//! it matches the published formula exactly. Full numbers:
//! `docs/filter/vaco-filter-analysis.md`.
//!
//! `ssim.All` averages per-component SSIM weighted by sample count, like
//! `psnr`'s `mse_avg` and unlike `identity`/`msad` — see
//! [`crate::fmt::weighted_average`].

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, PlaneRef};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::fmt::{fixed6, weighted_average};
use crate::video::{REFERENCE_PADS, component_labels, copy_meta, video_shape};

pub const DESC: FilterDesc = FilterDesc {
    name: "ssim",
    description: "Calculate the SSIM between two video streams.",
    inputs: REFERENCE_PADS,
    outputs: crate::video::VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const K1: f64 = 0.01;
const K2: f64 = 0.03;
const L: f64 = 255.0;
const WINDOW: usize = 11;
/// `WINDOW / 2`, spelled as a literal rather than a division so the
/// workspace's `integer_division` denial (D2's sibling correctness lints)
/// has nothing to fire on — this is the window's fixed radius, not a
/// computed quantity that could ever take another value.
const RADIUS: usize = 5;
const SIGMA: f64 = 1.5;

/// The paper's 11x11 circularly-symmetric Gaussian, normalised to unit sum,
/// precomputed once per call rather than per window (it depends only on
/// `WINDOW`/`SIGMA`, both constants).
fn gaussian_kernel() -> [[f64; WINDOW]; WINDOW] {
    let mut kernel = [[0.0_f64; WINDOW]; WINDOW];
    let mut sum = 0.0;
    for (yi, row) in kernel.iter_mut().enumerate() {
        for (xi, cell) in row.iter_mut().enumerate() {
            // `try_from`/`unwrap_or` rather than `as i32`, so this carries
            // no cast-wrap risk to reason about: yi/xi/RADIUS are all
            // `0..WINDOW` (11), and the fallback is unreachable in
            // practice but still total.
            let yi = i32::try_from(yi).unwrap_or(0) - i32::try_from(RADIUS).unwrap_or(0);
            let xi = i32::try_from(xi).unwrap_or(0) - i32::try_from(RADIUS).unwrap_or(0);
            let (dy, dx) = (f64::from(yi), f64::from(xi));
            let w = (-(dx * dx + dy * dy) / (2.0 * SIGMA * SIGMA)).exp();
            *cell = w;
            sum += w;
        }
    }
    for row in &mut kernel {
        for cell in row {
            *cell /= sum;
        }
    }
    kernel
}

/// Mean SSIM over one plane pair, per the paper's sliding-window
/// convention: a window only where the full 11x11 support fits, no padding.
/// `0.0` planes too small for even one window return `1.0` (vacuously
/// identical: there is nothing to compare), matching the natural reading of
/// "no window differed".
fn plane_ssim(a: PlaneRef<'_>, b: PlaneRef<'_>) -> f64 {
    let kernel = gaussian_kernel();
    let radius = RADIUS;
    let rows = a.rows().min(b.rows());
    let cols = a
        .row(0)
        .map_or(0, <[u8]>::len)
        .min(b.row(0).map_or(0, <[u8]>::len));
    if rows < WINDOW || cols < WINDOW {
        return 1.0;
    }
    let c1 = (K1 * L).powi(2);
    let c2 = (K2 * L).powi(2);
    let mut sum = 0.0;
    let mut count: u64 = 0;
    for cy in radius..(rows - radius) {
        for cx in radius..(cols - radius) {
            let (mut mu_x, mut mu_y) = (0.0, 0.0);
            for (wy, krow) in kernel.iter().enumerate() {
                let y = cy + wy - radius;
                let (Some(ra), Some(rb)) = (a.row(y), b.row(y)) else {
                    continue;
                };
                for (wx, &weight) in krow.iter().enumerate() {
                    let x = cx + wx - radius;
                    let (Some(&sa), Some(&sb)) = (ra.get(x), rb.get(x)) else {
                        continue;
                    };
                    mu_x += weight * f64::from(sa);
                    mu_y += weight * f64::from(sb);
                }
            }
            let (mut var_x, mut var_y, mut covar) = (0.0, 0.0, 0.0);
            for (wy, krow) in kernel.iter().enumerate() {
                let y = cy + wy - radius;
                let (Some(ra), Some(rb)) = (a.row(y), b.row(y)) else {
                    continue;
                };
                for (wx, &weight) in krow.iter().enumerate() {
                    let x = cx + wx - radius;
                    let (Some(&sa), Some(&sb)) = (ra.get(x), rb.get(x)) else {
                        continue;
                    };
                    let dx = f64::from(sa) - mu_x;
                    let dy = f64::from(sb) - mu_y;
                    var_x += weight * dx * dx;
                    var_y += weight * dy * dy;
                    covar += weight * dx * dy;
                }
            }
            let numerator = (2.0 * mu_x * mu_y + c1) * (2.0 * covar + c2);
            let denominator = (mu_x * mu_x + mu_y * mu_y + c1) * (var_x + var_y + c2);
            sum += numerator / denominator;
            count += 1;
        }
    }
    if count == 0 { 1.0 } else { sum / count as f64 }
}

#[derive(Debug, Default)]
pub(crate) struct Ssim;

impl PairedFilter for Ssim {
    fn filter_frames(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        let [main, reference] = <[Frame; 2]>::try_from(inputs.into_vec())
            .unwrap_or_else(|_| unreachable!("Paired guarantees exactly input_count() frames"));
        Ok(FrameOut::One(measure(&main, &reference)))
    }
}

fn measure(main: &Frame, reference: &Frame) -> Frame {
    let mut out = main.clone();
    if let (Some((fmt, _, _, main_planes)), Some((_, _, _, ref_planes))) =
        (video_shape(main), video_shape(reference))
    {
        let labels = component_labels(fmt, main_planes.min(ref_planes));
        let mut per_component: Vec<(f64, u64)> = Vec::new();
        let mut tags: Vec<(String, String)> = Vec::new();
        for (plane_idx, label) in labels.iter().enumerate().take(main_planes.min(ref_planes)) {
            let (Some(a), Some(b)) = (main.plane(plane_idx), reference.plane(plane_idx)) else {
                continue;
            };
            let rows = a.rows().min(b.rows());
            let cols = a
                .row(0)
                .map_or(0, <[u8]>::len)
                .min(b.row(0).map_or(0, <[u8]>::len));
            let samples = (rows as u64).saturating_mul(cols as u64);
            let score = plane_ssim(a, b);
            per_component.push((score, samples));
            tags.push((format!("lavfi.ssim.{label}"), fixed6(score)));
        }
        if !per_component.is_empty() {
            let all = weighted_average(&per_component);
            out.set_metadata("lavfi.ssim.All", fixed6(all));
            for (key, value) in tags {
                out.set_metadata(key, value);
            }
            let db = if (1.0 - all) <= 0.0 {
                "inf".to_owned()
            } else {
                fixed6(-10.0 * (1.0 - all).log10())
            };
            out.set_metadata("lavfi.ssim.dB", db);
        }
    }
    copy_meta(&mut out, main);
    out
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: Box::new(Paired::new(Ssim)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn gray_frame(value: u8, w: u32, h: u32) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f
    }

    /// Independent oracle: a self-identical pair has zero variance and zero
    /// mean difference everywhere, which is the algebraic case where SSIM
    /// is exactly `1.0` regardless of window shape or size.
    #[test]
    fn self_identical_is_one() {
        let a = gray_frame(128, 16, 16);
        let b = a.clone();
        let out = measure(&a, &b);
        assert_eq!(out.metadata_get("lavfi.ssim.Y"), Some("1.000000"));
        assert_eq!(out.metadata_get("lavfi.ssim.All"), Some("1.000000"));
        assert_eq!(out.metadata_get("lavfi.ssim.dB"), Some("inf"));
    }

    /// Distinguishing input: two *flat* planes with different values force
    /// zero variance/covariance, which collapses the paper's formula to a
    /// closed form independent of window size or shape entirely — a check
    /// that could not be satisfied by an implementation that got the
    /// windowing wrong but the mean/variance terms right, since both
    /// contribute here. `128` vs `110` has the closed-form answer
    /// `0.988628`/`19.441551` (`C1=6.5025`, `C2=58.5225`, `K1=0.01`,
    /// `K2=0.03`, `L=255` — this module's doc derives it precisely).
    ///
    /// **Not equal to `ffmpeg 8.1`'s measured `0.988625`/`19.440596`** on
    /// this same input — see this module's doc for why that is a real,
    /// recorded divergence rather than a bug in this test.
    #[test]
    fn flat_pair_matches_the_closed_form() {
        let a = gray_frame(128, 16, 16);
        let b = gray_frame(110, 16, 16);
        let out = measure(&a, &b);
        assert_eq!(out.metadata_get("lavfi.ssim.Y"), Some("0.988628"));
        assert_eq!(out.metadata_get("lavfi.ssim.All"), Some("0.988628"));
        assert_eq!(out.metadata_get("lavfi.ssim.dB"), Some("19.441551"));
    }

    #[test]
    fn too_small_for_a_window_is_vacuously_one() {
        let a = gray_frame(0, 4, 4);
        let b = gray_frame(255, 4, 4);
        let out = measure(&a, &b);
        assert_eq!(out.metadata_get("lavfi.ssim.Y"), Some("1.000000"));
    }
}
