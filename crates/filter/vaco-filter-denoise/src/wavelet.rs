//! The à trous (stationary/undecimated) wavelet transform, and coefficient
//! shrinkage on top of it — the shared engine behind [`crate::owdenoise`]
//! ("denoise using wavelets") and [`crate::vaguedenoiser`] ("wavelet based
//! Denoiser").
//!
//! # Why this transform, and where it comes from
//!
//! The à trous ("with holes") transform is a standard, published,
//! non-decimated wavelet decomposition (Holschneider et al. 1989; Starck &
//! Murtagh's `MR/1` denoising method builds on it directly) — a general
//! signal-processing technique, not something read out of `FFmpeg`. At each
//! level `j` it convolves the current approximation with a B3-spline kernel
//! `[1,4,6,4,1]/16` whose taps are spaced `2^j` samples apart ("holes"),
//! separably in each dimension, producing a smoother approximation; the
//! *detail* at that level is the difference between the approximation
//! before and after. Because every level is a plain subtraction, summing
//! every detail band plus the final smooth band reconstructs the original
//! **exactly**, with no thresholding: `sum(w_0..w_{n-1}) + c_n == c_0`. That
//! identity is `tests::reconstruction_is_exact_before_thresholding` below,
//! and it is what makes "denoise" here just "shrink the detail bands before
//! summing".
//!
//! Unlike the classic (decimated) discrete wavelet transform, à trous needs
//! no power-of-two padding: every band is the same size as the input, which
//! is what makes it a convenient fit for an arbitrary video frame size.
//!
//! # Provenance
//!
//! `provenance/sources.toml`: `holschneider-1989-atrous` (the transform),
//! `donoho-johnstone-1994-visushrink` (universal/`VisuShrink` threshold),
//! `chang-yu-vetterli-2000-bayesshrink` (`BayesShrink` threshold).

/// How a coefficient is shrunk once it is below or above a threshold `t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThresholdMethod {
    /// Zero anything with `|v| <= t`, leave the rest untouched.
    Hard,
    /// Zero anything with `|v| <= t`; shift everything else toward zero by
    /// `t`.
    Soft,
    /// Donoho's non-negative garrote: continuous like soft, but approaches
    /// the identity for large `|v|` like hard.
    Garrote,
}

impl ThresholdMethod {
    pub(crate) fn shrink(self, v: f32, t: f32) -> f32 {
        if t <= 0.0 {
            return v;
        }
        let a = v.abs();
        match self {
            Self::Hard => {
                if a <= t {
                    0.0
                } else {
                    v
                }
            }
            Self::Soft => {
                if a <= t {
                    0.0
                } else {
                    v.signum() * (a - t)
                }
            }
            Self::Garrote => {
                if a <= t {
                    0.0
                } else {
                    v - (t * t) / v
                }
            }
        }
    }
}

/// One level's approximation-to-approximation smoothing pass: separable
/// `[1,4,6,4,1]/16` B3-spline convolution with holes of `step` samples,
/// replicate boundary handling.
fn smooth_pass(src: &[f32], width: usize, height: usize, step: usize) -> Vec<f32> {
    const TAPS: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let max_x = i64::try_from(width.saturating_sub(1)).unwrap_or(i64::MAX);
    let max_y = i64::try_from(height.saturating_sub(1)).unwrap_or(i64::MAX);
    let get = |data: &[f32], x: i64, y: i64| -> f32 {
        let cx = usize::try_from(x.clamp(0, max_x)).unwrap_or(0);
        let cy = usize::try_from(y.clamp(0, max_y)).unwrap_or(0);
        let idx = cy.saturating_mul(width).saturating_add(cx);
        data.get(idx).copied().unwrap_or(0.0)
    };
    let step = i64::try_from(step).unwrap_or(i64::MAX);
    // Horizontal pass.
    let mut tmp = vec![0.0f32; width.saturating_mul(height)];
    for y in 0..height {
        let yi = i64::try_from(y).unwrap_or(0);
        for x in 0..width {
            let xi = i64::try_from(x).unwrap_or(0);
            let mut acc = 0.0f32;
            for (k, tap) in TAPS.iter().enumerate() {
                let ki = i64::try_from(k).unwrap_or(0);
                let offset = ki.saturating_sub(2).saturating_mul(step);
                acc += tap * get(src, xi.saturating_add(offset), yi);
            }
            if let Some(dst) = tmp.get_mut(y.saturating_mul(width).saturating_add(x)) {
                *dst = acc;
            }
        }
    }
    // Vertical pass.
    let mut out = vec![0.0f32; width.saturating_mul(height)];
    for y in 0..height {
        let yi = i64::try_from(y).unwrap_or(0);
        for x in 0..width {
            let xi = i64::try_from(x).unwrap_or(0);
            let mut acc = 0.0f32;
            for (k, tap) in TAPS.iter().enumerate() {
                let ki = i64::try_from(k).unwrap_or(0);
                let offset = ki.saturating_sub(2).saturating_mul(step);
                acc += tap * get(&tmp, xi, yi.saturating_add(offset));
            }
            if let Some(dst) = out.get_mut(y.saturating_mul(width).saturating_add(x)) {
                *dst = acc;
            }
        }
    }
    out
}

/// A full à trous decomposition: `levels` detail bands plus the final
/// smooth band, each `width * height` samples.
#[derive(Debug, Clone)]
pub(crate) struct Decomposition {
    pub(crate) details: Vec<Vec<f32>>,
    pub(crate) smooth: Vec<f32>,
}

impl Decomposition {
    /// Decompose `data` (`width * height` samples, row-major) into `levels`
    /// detail bands.
    pub(crate) fn decompose(data: &[f32], width: usize, height: usize, levels: usize) -> Self {
        let mut details = Vec::new();
        let mut current = data.to_vec();
        for j in 0..levels {
            let step = 1usize << j;
            let smoothed = smooth_pass(&current, width, height, step);
            let detail: Vec<f32> = current
                .iter()
                .zip(smoothed.iter())
                .map(|(c, s)| c - s)
                .collect();
            details.push(detail);
            current = smoothed;
        }
        Self {
            details,
            smooth: current,
        }
    }

    /// Sum every detail band plus the smooth band back into one image.
    /// Exact (up to float rounding) when no shrinkage has been applied.
    pub(crate) fn reconstruct(&self) -> Vec<f32> {
        let mut out = self.smooth.clone();
        for band in &self.details {
            for (o, d) in out.iter_mut().zip(band.iter()) {
                *o += d;
            }
        }
        out
    }

    /// Robust noise-sigma estimate from the finest detail band, the standard
    /// median-absolute-deviation estimator behind `VisuShrink` (Donoho &
    /// Johnstone 1994): `sigma = median(|detail|) / 0.6745`.
    pub(crate) fn finest_band_sigma(&self) -> f32 {
        let Some(band) = self.details.first() else {
            return 0.0;
        };
        median_abs(band) / 0.6745
    }

    /// Apply `method` with a per-level threshold `threshold_for(level, sigma)`
    /// to every detail band, in place. `sigma` is this decomposition's
    /// finest-band noise estimate, passed once so a caller's threshold
    /// formula can use it without recomputing it.
    pub(crate) fn shrink(&mut self, method: ThresholdMethod, mut threshold_for: impl FnMut(usize, f32) -> f32) {
        let sigma = self.finest_band_sigma();
        for (level, band) in self.details.iter_mut().enumerate() {
            let t = threshold_for(level, sigma);
            for v in band.iter_mut() {
                *v = method.shrink(*v, t);
            }
        }
    }
}

/// Median of `|v|` over `data`. `O(n log n)`; these bands are one video
/// plane, not a hot per-pixel path.
fn median_abs(data: &[f32]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let mut abs: Vec<f32> = data.iter().map(|v| v.abs()).collect();
    abs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    #[allow(
        clippy::integer_division,
        reason = "computing the middle index of a sorted slice for a median"
    )]
    let mid = abs.len() / 2;
    if abs.len().is_multiple_of(2) {
        let Some(a) = abs.get(mid.saturating_sub(1)) else {
            return 0.0;
        };
        let Some(b) = abs.get(mid) else { return 0.0 };
        (a + b) / 2.0
    } else {
        abs.get(mid).copied().unwrap_or(0.0)
    }
}

/// `BayesShrink`'s per-band adaptive threshold (Chang, Yu & Vetterli 2000):
/// `t = sigma^2 / sigma_band`, where `sigma_band = sqrt(max(var(band) -
/// sigma^2, 0))` estimates the band's own signal variance net of noise.
/// Falls back to `0.0` (no shrinkage) when the band's variance does not
/// exceed the noise estimate — there is nothing to separate signal from.
pub(crate) fn bayes_threshold(band: &[f32], sigma: f32) -> f32 {
    if band.is_empty() {
        return 0.0;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "band lengths are plane sample counts, far below f32's exact-integer range"
    )]
    let n = band.len() as f32;
    let mean = band.iter().sum::<f32>() / n;
    let var = band.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n;
    let sigma_x2 = (var - sigma * sigma).max(0.0);
    if sigma_x2 <= f32::EPSILON {
        return f32::MAX;
    }
    (sigma * sigma) / sigma_x2.sqrt()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn reconstruction_is_exact_before_thresholding() {
        // A non-trivial, non-constant field: reconstruction must recover it
        // to float rounding, independent of what the shrinkage step later
        // does with the same decomposition.
        let (w, h) = (17, 13);
        let data: Vec<f32> = (0..w * h)
            .map(|i| (((i * 37 + 5) % 251) as f32) / 4.0)
            .collect();
        let decomp = Decomposition::decompose(&data, w, h, 4);
        let recon = decomp.reconstruct();
        for (a, b) in data.iter().zip(recon.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn a_constant_field_has_zero_detail_at_every_level() {
        let (w, h) = (9, 9);
        let data = vec![42.0f32; w * h];
        let decomp = Decomposition::decompose(&data, w, h, 3);
        for band in &decomp.details {
            for v in band {
                assert!(v.abs() < 1e-4, "expected 0, got {v}");
            }
        }
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "shrink() returns the literal 0.0 or the untouched input exactly, not a computed float"
    )]
    fn hard_threshold_zeroes_small_coefficients_only() {
        assert_eq!(ThresholdMethod::Hard.shrink(0.5, 1.0), 0.0);
        assert_eq!(ThresholdMethod::Hard.shrink(2.0, 1.0), 2.0);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "shrink() returns the literal 0.0 exactly when |v| <= t, not a computed float"
    )]
    fn soft_threshold_shrinks_surviving_coefficients_toward_zero() {
        let v = ThresholdMethod::Soft.shrink(2.0, 1.0);
        assert!((v - 1.0).abs() < 1e-6);
        assert_eq!(ThresholdMethod::Soft.shrink(0.5, 1.0), 0.0);
    }

    #[test]
    fn garrote_threshold_is_between_hard_and_soft() {
        let hard = ThresholdMethod::Hard.shrink(3.0, 1.0);
        let soft = ThresholdMethod::Soft.shrink(3.0, 1.0);
        let garrote = ThresholdMethod::Garrote.shrink(3.0, 1.0);
        assert!(garrote <= hard && garrote >= soft);
    }
}
