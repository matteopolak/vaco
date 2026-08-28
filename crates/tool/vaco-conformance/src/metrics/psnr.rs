//! PSNR — peak signal-to-noise ratio, the standard definition:
//!
//! ```text
//! MSE  = mean((source[i] - distorted[i])^2)
//! PSNR = 10 * log10(MAX^2 / MSE)     (dB; +infinity when MSE == 0)
//! ```
//!
//! `MAX` is `2^depth - 1`. Higher is better, matching
//! [`crate::compare::quality::Metric`]'s convention directly — no negation
//! needed, unlike the audio spectral-distance metric.

use crate::compare::quality::{Metric, Signal};
use crate::metrics::sample::{geometry_matches, max_value, sample_at};

/// PSNR over one named plane, or averaged across every plane the two
/// signals share.
#[derive(Debug, Clone, Copy)]
pub enum Plane {
    /// Plane index `0` — the luma/only plane for the formats this project's
    /// `Signal` carries.
    Index(usize),
    /// Every plane the signals have in common, unweighted.
    Average,
}

/// The [`Metric`] implementation. `name` is the manifest-facing string
/// (`"psnr-y"`, `"psnr-avg"`, …); `plane` says what to average over.
#[derive(Debug, Clone)]
pub struct Psnr {
    name: &'static str,
    plane: Plane,
}

impl Psnr {
    /// PSNR of plane 0 (luma, for the video formats this harness handles;
    /// the only plane for audio).
    #[must_use]
    pub const fn y() -> Self {
        Self {
            name: "psnr-y",
            plane: Plane::Index(0),
        }
    }

    /// PSNR of a specific plane index, under a caller-chosen manifest name.
    #[must_use]
    pub const fn plane(name: &'static str, index: usize) -> Self {
        Self {
            name,
            plane: Plane::Index(index),
        }
    }

    /// PSNR averaged across every shared plane.
    #[must_use]
    pub const fn average() -> Self {
        Self {
            name: "psnr-avg",
            plane: Plane::Average,
        }
    }
}

/// MSE over one plane. `None` if the plane index is out of range for either
/// signal, or if no in-bounds sample pairs exist at all.
fn plane_mse(source: &Signal<'_>, distorted: &Signal<'_>, plane_idx: usize) -> Option<f64> {
    let src_plane = *source.planes.get(plane_idx)?;
    let dst_plane = *distorted.planes.get(plane_idx)?;
    let src_stride = *source.strides.get(plane_idx)?;
    let dst_stride = *distorted.strides.get(plane_idx)?;

    let mut sum_sq = 0.0_f64;
    let mut count: u64 = 0;
    for y in 0..source.height {
        for x in 0..source.width {
            let (Some(s), Some(d)) = (
                sample_at(src_plane, src_stride, x, y, source.depth),
                sample_at(dst_plane, dst_stride, x, y, distorted.depth),
            ) else {
                continue;
            };
            let diff = f64::from(s) - f64::from(d);
            sum_sq += diff * diff;
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    Some(sum_sq / count as f64)
}

impl Metric for Psnr {
    fn name(&self) -> &'static str {
        self.name
    }

    fn score(&self, source: &Signal<'_>, distorted: &Signal<'_>) -> Result<f64, String> {
        if !geometry_matches(source, distorted) {
            return Err(format!(
                "{}: geometry mismatch ({}x{}@{} vs {}x{}@{})",
                self.name,
                source.width,
                source.height,
                source.depth,
                distorted.width,
                distorted.height,
                distorted.depth
            ));
        }
        let max = max_value(source.depth);
        if max <= 0.0 {
            return Err(format!("{}: depth {} has no representable range", self.name, source.depth));
        }

        let mse = match self.plane {
            Plane::Index(idx) => plane_mse(source, distorted, idx)
                .ok_or_else(|| format!("{}: plane {idx} out of range or empty", self.name))?,
            Plane::Average => {
                let n = source.planes.len().min(distorted.planes.len());
                if n == 0 {
                    return Err(format!("{}: no planes to compare", self.name));
                }
                let mut total = 0.0_f64;
                let mut used = 0usize;
                for idx in 0..n {
                    if let Some(m) = plane_mse(source, distorted, idx) {
                        total += m;
                        used += 1;
                    }
                }
                if used == 0 {
                    return Err(format!("{}: every plane was empty", self.name));
                }
                total / used as f64
            }
        };

        if mse == 0.0 {
            return Ok(f64::INFINITY);
        }
        Ok(10.0 * (max * max / mse).log10())
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::Psnr;
    use crate::compare::quality::{Metric, Signal};

    fn signal(plane: &[u8], width: u32, height: u32, depth: u8) -> Signal<'_> {
        Signal {
            planes: vec![plane],
            strides: vec![width as usize],
            width,
            height,
            depth,
        }
    }

    #[test]
    fn identical_signals_score_infinite() {
        let data = [10_u8, 20, 30, 40, 50, 60, 70, 80, 90];
        let a = signal(&data, 3, 3, 8);
        let b = signal(&data, 3, 3, 8);
        let score = Psnr::y().score(&a, &b).unwrap();
        assert!(score.is_infinite() && score > 0.0);
    }

    #[test]
    fn a_larger_error_scores_lower() {
        let source = [100_u8; 16];
        let small_error = [101_u8; 16];
        let large_error = [150_u8; 16];

        let src = signal(&source, 4, 4, 8);
        let small = signal(&small_error, 4, 4, 8);
        let large = signal(&large_error, 4, 4, 8);

        let small_score = Psnr::y().score(&src, &small).unwrap();
        let large_score = Psnr::y().score(&src, &large).unwrap();
        assert!(
            small_score > large_score,
            "small error {small_score} should score higher than large error {large_score}"
        );
    }

    #[test]
    fn geometry_mismatch_is_an_error() {
        let a_data = [1_u8; 4];
        let b_data = [1_u8; 9];
        let a = signal(&a_data, 2, 2, 8);
        let b = signal(&b_data, 3, 3, 8);
        assert!(Psnr::y().score(&a, &b).is_err());
    }

    #[test]
    fn ten_bit_depth_uses_the_wider_range() {
        // Same relative error (1 code point) at a wider bit depth should
        // still register as a very high but finite PSNR, not panic or
        // silently truncate to the 8-bit range.
        let source: Vec<u8> = (0..8)
            .flat_map(|_| 500_u16.to_le_bytes())
            .collect();
        let distorted: Vec<u8> = (0..8)
            .flat_map(|_| 501_u16.to_le_bytes())
            .collect();
        let a = signal(&source, 4, 2, 10);
        let b = signal(&distorted, 4, 2, 10);
        let score = Psnr::y().score(&a, &b).unwrap();
        assert!(score.is_finite() && score > 40.0, "got {score}");
    }

    #[test]
    fn average_needs_shared_planes() {
        let y = [10_u8; 4];
        let u = [20_u8; 4];
        let mut a = signal(&y, 2, 2, 8);
        a.planes.push(&u);
        a.strides.push(2);
        let mut b = signal(&y, 2, 2, 8);
        b.planes.push(&u);
        b.strides.push(2);
        let score = Psnr::average().score(&a, &b).unwrap();
        assert!(score.is_infinite());
    }
}
