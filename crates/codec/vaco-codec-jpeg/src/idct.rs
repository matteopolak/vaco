//! The "spec-exact" IDCT/FDCT mode, and the dequantize/level-shift step
//! around it.
//!
//! ITU-T T.81 Annex A.3.3 gives the inverse DCT an *accuracy* bound, not a
//! mandated algorithm — unlike H.264/HEVC, no bit pattern is normative. That
//! is exactly `vaco-codec-dsp-idct`'s [`mpeg2`](vaco_codec_dsp_idct::mpeg2)
//! module's own reason to exist (it already serves MPEG-2, which has the
//! identical accuracy-not-bit-exactness contract), so the inverse direction
//! here is that module reused, not reimplemented. What it does not provide
//! is a forward transform — its own docs describe it as inverse-only — so
//! [`Fdct8x8`] is new: the classical forward DCT, built the same way (one
//! `vaco_tx` DCT plan run twice, separably) but for encoding rather than
//! decoding.
//!
//! Calling this the "spec-exact" mode (plan 15's own term) means: the
//! literal `f64` evaluation of the classical cosine transform, to
//! `vaco-tx`'s Class C accuracy bound, with no fast integer approximation
//! substituted in. A decoder using a different (faster, less accurate) IDCT
//! algorithm is still T.81-conformant; this crate does not currently offer
//! one, so every decode uses this mode.

use vaco_codec_dsp_idct::mpeg2::Idct8x8;
use vaco_core::Result;
use vaco_tx::{Direction, Plan, Tx, TxFlags, TxKind};

const N: usize = 8;

/// One dequantized 8×8 block, reconstructed by the inverse transform and
/// level-shifted back into unsigned sample range.
pub(crate) struct SpecExactIdct {
    inner: Idct8x8<f64>,
}

impl SpecExactIdct {
    /// # Errors
    /// Only if `vaco-tx` cannot build a length-8 DCT-III plan, which does not
    /// happen for a fixed, non-zero length.
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            inner: Idct8x8::new()?,
        })
    }

    /// Dequantize `coeffs` (natural order) by `quant`, inverse-transform, and
    /// level-shift + clamp into `0..=(1 << precision) - 1`.
    pub(crate) fn apply(
        &mut self,
        coeffs: &[i32; 64],
        quant: &[u16; 64],
        precision: u8,
        out: &mut [i32; 64],
    ) {
        let mut f = [0.0f64; 64];
        for (dst, (&c, &q)) in f.iter_mut().zip(coeffs.iter().zip(quant.iter())) {
            *dst = f64::from(c) * f64::from(q);
        }
        let mut o = [0.0f64; 64];
        self.inner.apply(&f, &mut o);

        let level = 1i32 << precision.saturating_sub(1).min(30);
        let max = level.saturating_mul(2).saturating_sub(1);
        for (dst, &v) in out.iter_mut().zip(o.iter()) {
            let rounded = v.round();
            let shifted = if rounded.is_finite() {
                (rounded as i64).saturating_add(i64::from(level))
            } else {
                i64::from(level)
            };
            *dst = i32::try_from(shifted.clamp(0, i64::from(max))).unwrap_or(0);
        }
    }
}

/// The forward counterpart of [`vaco_codec_dsp_idct::mpeg2::Idct8x8`]: the
/// classical `F(u,v) = 1/4 * C(u) * C(v) * sum_x sum_y f(x,y) *
/// cos((2x+1)u*pi/16) * cos((2y+1)v*pi/16)`, `C(0) = 1/sqrt(2)` else `1`.
///
/// Built from `vaco_tx`'s DCT-II (`Direction::Forward`) the same separable
/// way the inverse is: a row pass, a column pass, and the `C(0)` weighting
/// applied where each pass's frequency-zero output lands. The inverse
/// applies its `C(0)` factor by *prescaling the input's* row/column zero
/// (since `vaco_tx`'s DCT-III already halves index zero on the way in); the
/// forward direction's DCT-II has no such built-in halving, so this
/// postscales each pass's *output* index zero instead — the two are
/// algebraic mirrors of the same weighting, verified against a direct `f64`
/// evaluation in this module's tests.
pub(crate) struct Fdct8x8 {
    tx: Tx<f64>,
}

impl Fdct8x8 {
    /// # Errors
    /// Only if `vaco-tx` cannot build a length-8 DCT-II plan.
    pub(crate) fn new() -> Result<Self> {
        let plan = Plan::<f64>::new(TxKind::Dct, Direction::Forward, N, 1.0, TxFlags::empty())?;
        Ok(Self { tx: Tx::new(plan) })
    }

    /// Forward-transform one row-major 8×8 block of centred samples
    /// (already level-shifted by the caller) into natural-order
    /// coefficients.
    pub(crate) fn apply(&mut self, samples: &[f64; N * N], out: &mut [f64; N * N]) {
        let inv_sqrt2 = core::f64::consts::FRAC_1_SQRT_2;
        let quarter = 0.25;

        let mut rows = [0.0f64; N * N];
        let mut row_out = [0.0f64; N];
        for r in 0..N {
            let start = r * N;
            let Some(row_in) = samples.get(start..start + N) else {
                continue;
            };
            self.tx.execute(&mut row_out, row_in);
            if let Some(v0) = row_out.first_mut() {
                *v0 *= inv_sqrt2;
            }
            if let Some(dst) = rows.get_mut(start..start + N) {
                dst.copy_from_slice(&row_out);
            }
        }

        let mut scratch_in = [0.0f64; N];
        let mut scratch_out = [0.0f64; N];
        for c in 0..N {
            for (r, slot) in scratch_in.iter_mut().enumerate() {
                *slot = rows.get(r * N + c).copied().unwrap_or(0.0);
            }
            self.tx.execute(&mut scratch_out, &scratch_in);
            if let Some(v0) = scratch_out.first_mut() {
                *v0 *= inv_sqrt2;
            }
            for (r, &v) in scratch_out.iter().enumerate() {
                if let Some(dst) = out.get_mut(r * N + c) {
                    *dst = v * quarter;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code exercising the transform, not the untrusted-input surface"
)]
mod tests {
    use super::*;

    fn direct_fdct_f64(f: &[f64; 64]) -> [f64; 64] {
        let c = |u: usize| {
            if u == 0 {
                core::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            }
        };
        let mut out = [0.0f64; 64];
        for u in 0..N {
            for v in 0..N {
                let mut s = 0.0;
                for x in 0..N {
                    for y in 0..N {
                        let fxy = f.get(x * N + y).copied().unwrap_or(0.0);
                        s += fxy
                            * (core::f64::consts::PI * (2 * x + 1) as f64 * u as f64 / 16.0).cos()
                            * (core::f64::consts::PI * (2 * y + 1) as f64 * v as f64 / 16.0).cos();
                    }
                }
                if let Some(slot) = out.get_mut(u * N + v) {
                    *slot = 0.25 * c(u) * c(v) * s;
                }
            }
        }
        out
    }

    #[test]
    fn fdct_matches_the_direct_classical_evaluation() {
        let mut fdct = Fdct8x8::new().unwrap();
        let input: [f64; 64] = core::array::from_fn(|i| ((i as f64) * 13.0).sin() * 100.0);
        let mut got = [0.0f64; 64];
        fdct.apply(&input, &mut got);
        let want = direct_fdct_f64(&input);
        for (g, w) in got.iter().zip(want.iter()) {
            assert!((g - w).abs() < 1e-6, "{g} vs {w}");
        }
    }

    #[test]
    fn fdct_then_idct_round_trips_a_dc_only_block() {
        let mut fdct = Fdct8x8::new().unwrap();
        let mut idct = SpecExactIdct::new().unwrap();
        let input = [50.0f64; 64];
        let mut freq = [0.0f64; 64];
        fdct.apply(&input, &mut freq);
        // A flat block has energy only at (0,0): DC = sum/8 * ... in the
        // classical normalization, everything else should be ~0.
        for (i, &v) in freq.iter().enumerate() {
            if i == 0 {
                assert!((v - 400.0).abs() < 1e-6, "DC={v}");
            } else {
                assert!(v.abs() < 1e-6, "AC[{i}]={v}");
            }
        }
        let coeffs: [i32; 64] =
            core::array::from_fn(|i| freq.get(i).copied().unwrap_or(0.0).round() as i32);
        let quant = [1u16; 64];
        let mut out = [0i32; 64];
        idct.apply(&coeffs, &quant, 8, &mut out);
        for &v in &out {
            assert_eq!(v, 178); // 50 + level(128) rounds to 178 for a flat block
        }
    }

    #[test]
    fn clamp_never_panics_on_extreme_quantized_values() {
        let mut idct = SpecExactIdct::new().unwrap();
        let mut coeffs = [0i32; 64];
        if let Some(a) = coeffs.first_mut() {
            *a = i32::MAX.checked_div(2).unwrap_or(0);
        }
        if let Some(b) = coeffs.get_mut(1) {
            *b = i32::MIN.checked_div(2).unwrap_or(0);
        }
        let quant = [255u16; 64];
        let mut out = [0i32; 64];
        idct.apply(&coeffs, &quant, 12, &mut out);
        for &v in &out {
            assert!((0..=4095).contains(&v));
        }
    }
}
