//! SSIM — the structural similarity index of Wang, Bovik, Sheikh &
//! Simoncelli, *IEEE Transactions on Image Processing* 13(4), 2004,
//! "Image Quality Assessment: From Error Visibility to Structural
//! Similarity" (equations 12–14 in that paper, cited by that reference —
//! **never** transcribed from `tests/tiny_ssim.c`, which is GPL and on the
//! project's hard do-not-reuse list per plan 13 §0.1/§1.11.2).
//!
//! # The definition implemented here
//!
//! An 11×11 circularly-symmetric Gaussian weighting window (σ = 1.5,
//! normalised to sum to 1 — the paper's own choice, §III.A) slides over both
//! signals in valid (non-padded) positions. At each position:
//!
//! ```text
//! SSIM(x, y) = (2 μx μy + C1)(2 σxy + C2)
//!              ────────────────────────────
//!              (μx² + μy² + C1)(σx² + σy² + C2)
//!
//! C1 = (K1 L)²,  K1 = 0.01
//! C2 = (K2 L)²,  K2 = 0.03
//! ```
//!
//! where `μ`, `σ²`, `σxy` are the Gaussian-weighted local mean, variance and
//! covariance, and `L` is the dynamic range (`2^depth - 1`). The overall
//! score is the mean of the per-window SSIM over every valid window
//! position (the paper's "Mean SSIM", §IV). Higher is better; the range is
//! `(-1, 1]`, with `1` only at identity.
//!
//! # Scope
//!
//! Operates on plane 0 only (luma, for the video formats this harness
//! handles). Chroma SSIM and any multi-scale variant (MS-SSIM) are not
//! implemented — a documented cut, not an oversight; extending
//! [`Ssim::score`] to another plane index is the same shape as
//! [`super::psnr::Psnr::plane`] if that is ever needed.
//!
//! # Performance, honestly
//!
//! This is the direct `O(window_area)` per pixel definition with no separable-
//! filter optimisation, which is the right trade for conformance-scale
//! fixtures and the wrong one for a full-resolution frame — a naive 1080p
//! frame is ~2M windows × 121 taps, which is too slow for a CI gate run
//! per-frame. A production encoder-quality gate should downsample or use a
//! separable two-pass Gaussian; that optimisation is not implemented here.

use crate::compare::quality::{Metric, Signal};
use crate::metrics::sample::{geometry_matches, max_value, sample_at};

const WINDOW: usize = 11;
const SIGMA: f64 = 1.5;
const K1: f64 = 0.01;
const K2: f64 = 0.03;

/// The paper's Gaussian window, built once per call. `WINDOW` is small and
/// fixed, so this costs nothing measurable next to the O(pixels × 121) score
/// loop it feeds.
fn gaussian_window() -> [[f64; WINDOW]; WINDOW] {
    let center = (WINDOW as f64 - 1.0) / 2.0;
    let mut w = [[0.0_f64; WINDOW]; WINDOW];
    let mut sum = 0.0_f64;
    for (yi, row) in w.iter_mut().enumerate() {
        for (xi, cell) in row.iter_mut().enumerate() {
            let dx = xi as f64 - center;
            let dy = yi as f64 - center;
            let g = (-(dx * dx + dy * dy) / (2.0 * SIGMA * SIGMA)).exp();
            *cell = g;
            sum += g;
        }
    }
    if sum > 0.0 {
        for row in &mut w {
            for cell in row {
                *cell /= sum;
            }
        }
    }
    w
}

/// The SSIM [`Metric`], luma-plane only.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ssim;

impl Metric for Ssim {
    fn name(&self) -> &'static str {
        "ssim"
    }

    fn score(&self, source: &Signal<'_>, distorted: &Signal<'_>) -> Result<f64, String> {
        if !geometry_matches(source, distorted) {
            return Err(format!(
                "ssim: geometry mismatch ({}x{}@{} vs {}x{}@{})",
                source.width,
                source.height,
                source.depth,
                distorted.width,
                distorted.height,
                distorted.depth
            ));
        }
        let (Some(&src_plane), Some(&dst_plane)) =
            (source.planes.first(), distorted.planes.first())
        else {
            return Err("ssim: no plane 0 to compare".to_owned());
        };
        let (Some(&src_stride), Some(&dst_stride)) =
            (source.strides.first(), distorted.strides.first())
        else {
            return Err("ssim: no stride recorded for plane 0".to_owned());
        };

        let width = source.width;
        let height = source.height;
        if (width as usize) < WINDOW || (height as usize) < WINDOW {
            return Err(format!(
                "ssim: {width}x{height} is smaller than the {WINDOW}x{WINDOW} window"
            ));
        }

        let l = max_value(source.depth);
        if l <= 0.0 {
            return Err(format!(
                "ssim: depth {} has no representable range",
                source.depth
            ));
        }
        let c1 = (K1 * l) * (K1 * l);
        let c2 = (K2 * l) * (K2 * l);
        let window = gaussian_window();

        let last_x = width - WINDOW as u32 + 1;
        let last_y = height - WINDOW as u32 + 1;

        let mut total = 0.0_f64;
        let mut windows = 0u64;

        for top in 0..last_y {
            for left in 0..last_x {
                let Some(local) = window_ssim(
                    src_plane,
                    src_stride,
                    source.depth,
                    dst_plane,
                    dst_stride,
                    distorted.depth,
                    left,
                    top,
                    &window,
                    c1,
                    c2,
                ) else {
                    continue;
                };
                total += local;
                windows += 1;
            }
        }

        if windows == 0 {
            return Err("ssim: no complete window fit inside the frame".to_owned());
        }
        Ok(total / windows as f64)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one window comparison genuinely has this many independent inputs; grouping them into a struct would not make the arithmetic below clearer"
)]
fn window_ssim(
    src_plane: &[u8],
    src_stride: usize,
    src_depth: u8,
    dst_plane: &[u8],
    dst_stride: usize,
    dst_depth: u8,
    left: u32,
    top: u32,
    window: &[[f64; WINDOW]; WINDOW],
    c1: f64,
    c2: f64,
) -> Option<f64> {
    let mut mu_x = 0.0_f64;
    let mut mu_y = 0.0_f64;
    for (dy, row) in window.iter().enumerate() {
        for (dx, &w) in row.iter().enumerate() {
            let x = left + dx as u32;
            let y = top + dy as u32;
            let sx = f64::from(sample_at(src_plane, src_stride, x, y, src_depth)?);
            let sy = f64::from(sample_at(dst_plane, dst_stride, x, y, dst_depth)?);
            mu_x += w * sx;
            mu_y += w * sy;
        }
    }

    let mut var_x = 0.0_f64;
    let mut var_y = 0.0_f64;
    let mut cov_xy = 0.0_f64;
    for (dy, row) in window.iter().enumerate() {
        for (dx, &w) in row.iter().enumerate() {
            let x = left + dx as u32;
            let y = top + dy as u32;
            let sx = f64::from(sample_at(src_plane, src_stride, x, y, src_depth)?);
            let sy = f64::from(sample_at(dst_plane, dst_stride, x, y, dst_depth)?);
            let ex = sx - mu_x;
            let ey = sy - mu_y;
            var_x += w * ex * ex;
            var_y += w * ey * ey;
            cov_xy += w * ex * ey;
        }
    }

    let numerator = (2.0 * mu_x * mu_y + c1) * (2.0 * cov_xy + c2);
    let denominator = (mu_x * mu_x + mu_y * mu_y + c1) * (var_x + var_y + c2);
    if denominator == 0.0 {
        return Some(1.0);
    }
    Some(numerator / denominator)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::Ssim;
    use crate::compare::quality::{Metric, Signal};

    fn signal(plane: &[u8], width: u32, height: u32) -> Signal<'_> {
        Signal {
            planes: vec![plane],
            strides: vec![width as usize],
            width,
            height,
            depth: 8,
        }
    }

    fn checkerboard(w: u32, h: u32) -> Vec<u8> {
        (0..h)
            .flat_map(|y| (0..w).map(move |x| if (x + y) % 2 == 0 { 20_u8 } else { 220 }))
            .collect()
    }

    #[test]
    fn identical_signals_score_one() {
        let data = checkerboard(16, 16);
        let a = signal(&data, 16, 16);
        let b = signal(&data, 16, 16);
        let score = Ssim.score(&a, &b).unwrap();
        assert!((score - 1.0).abs() < 1e-9, "got {score}");
    }

    #[test]
    fn a_noisier_signal_scores_lower_than_a_barely_different_one() {
        let source = checkerboard(16, 16);
        let mut slightly_off = source.clone();
        for v in &mut slightly_off {
            *v = v.saturating_add(2);
        }
        let mut very_off = source.clone();
        for (i, v) in very_off.iter_mut().enumerate() {
            *v = if i % 2 == 0 { 0 } else { 255 };
        }

        let src = signal(&source, 16, 16);
        let close = signal(&slightly_off, 16, 16);
        let far = signal(&very_off, 16, 16);

        let close_score = Ssim.score(&src, &close).unwrap();
        let far_score = Ssim.score(&src, &far).unwrap();
        assert!(
            close_score > far_score,
            "close {close_score} should score higher than far {far_score}"
        );
    }

    #[test]
    fn too_small_a_frame_is_an_error() {
        let data = [1_u8; 25];
        let a = signal(&data, 5, 5);
        let b = signal(&data, 5, 5);
        assert!(Ssim.score(&a, &b).is_err());
    }

    #[test]
    fn geometry_mismatch_is_an_error() {
        let a_data = checkerboard(16, 16);
        let b_data = checkerboard(20, 20);
        let a = signal(&a_data, 16, 16);
        let b = signal(&b_data, 20, 20);
        assert!(Ssim.score(&a, &b).is_err());
    }

    #[test]
    fn score_never_exceeds_one_on_random_looking_content() {
        let a_data: Vec<u8> = (0_u32..(20 * 20)).map(|i| (i * 37 % 256) as u8).collect();
        let b_data: Vec<u8> = (0_u32..(20 * 20)).map(|i| (i * 53 % 256) as u8).collect();
        let a = signal(&a_data, 20, 20);
        let b = signal(&b_data, 20, 20);
        let score = Ssim.score(&a, &b).unwrap();
        assert!(score <= 1.0 + 1e-9, "got {score}");
    }
}
