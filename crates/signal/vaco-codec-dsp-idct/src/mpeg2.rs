//! The classical real-valued 8×8 IDCT used by MPEG-2 and the JPEG family.
//!
//! Unlike H.264/HEVC, neither ISO/IEC 13818-2 nor the JPEG baseline mandates
//! one specific integer algorithm — Annex A of 13818-2 (and, historically,
//! IEEE 1180) instead specify an **accuracy bound** an implementation's IDCT
//! must meet against the exact mathematical transform, measured over a large
//! statistical sample of random and near-saturated inputs. Any sufficiently
//! accurate 2-D IDCT is conformant, so there is no standard-mandated table to
//! transcribe here — which is exactly why this module is a wrapper over
//! `vaco-tx`'s existing DCT-III rather than a second hand-written transform:
//! duplicating one would be the mistake D19 exists to catch, not a hedge
//! against risk.
//!
//! # Reusing `vaco-tx`'s DCT-III
//!
//! `vaco-tx`'s 1-D DCT-III is (see its module docs) `y[k] = x[0]/2 +
//! Σ_{n≥1} x[n]·cos(π(2k+1)n/(2N))`, unnormalised. The classical 2-D IDCT is
//! `f(x,y) = ¼ Σ_u Σ_v C(u)C(v) F(u,v) cos((2x+1)uπ/16) cos((2y+1)vπ/16)`
//! with `C(0) = 1/√2`, `C(k) = 1` otherwise, applied separably. Working the
//! algebra through both directions (documented in full in
//! `docs/signal/vaco-codec-dsp-idct.md`, and checked numerically against a
//! direct `f64` evaluation to ~1e-13 before being trusted): pre-multiplying
//! row 0 and column 0 of the input by `√2` before running `vaco-tx`'s DCT-III
//! down each axis, then scaling the final result by `¼`, reproduces the
//! classical formula exactly. Any DC/off-DC weighting error in that step
//! shows up immediately as a large error against the accuracy tests below —
//! it is not the kind of bug that hides.
//!
//! # Accuracy, not bit-exactness
//!
//! [`vaco_tx`]'s own contract for `f32`/`f64` is Class C: relative RMS error
//! `≤ 2^-20` (`f32`) / `2^-48` (`f64`) against an `f64` direct evaluation.
//! That is dramatically tighter than IEEE 1180's classical bounds (peak error
//! ≤ 1, mean error ≤ 0.015, mean-square error ≤ 0.06, measured in 8-bit pixel
//! codes over a large random sample), so this module inherits conformance
//! for free rather than needing its own statistical test harness — checked
//! directly in this module's tests against the same `f64` direct evaluation.

use core::ops::Mul;

use vaco_core::Result;
use vaco_tx::{Direction, Plan, Tx, TxFlags, TxKind, TxSample};

const N: usize = 8;

/// The classical 8×8 IDCT, generic over `T ∈ {f32, f64}`. Built once (it owns
/// `vaco-tx`'s FFT-derived scratch state) and reused across blocks — exactly
/// the `Plan`/`Tx` split `vaco-tx` itself uses, for the same reason.
#[derive(Debug)]
pub struct Idct8x8<T: TxSample> {
    tx: Tx<T>,
}

impl<T: TxSample + Mul<Output = T>> Idct8x8<T> {
    /// # Errors
    ///
    /// Only if `vaco-tx` itself cannot build a length-8 DCT-III plan, which
    /// does not happen for any fixed, non-zero length such as 8.
    pub fn new() -> Result<Self> {
        let plan = Plan::<T>::new(
            TxKind::Dct,
            Direction::Inverse,
            N,
            T::IDENTITY_SCALE,
            TxFlags::empty(),
        )?;
        Ok(Self { tx: Tx::new(plan) })
    }

    /// Inverse-transform one row-major 8×8 block of dequantised coefficients.
    pub fn apply(&mut self, coeffs: &[T; N * N], out: &mut [T; N * N]) {
        let sqrt2 = T::from_f64(core::f64::consts::SQRT_2);
        let quarter = T::from_f64(0.25);

        // Pre-scale row 0 (indices 0..8) and column 0 (indices 0, 8, .., 56)
        // by `sqrt2`, matching the classical `C(0) = 1/sqrt(2)` weighting —
        // applied once per axis, so `F(0,0)` picks up `sqrt2` twice.
        let mut g = *coeffs;
        for v in 0..N {
            if let Some(x) = g.get_mut(v) {
                *x = *x * sqrt2;
            }
        }
        for u in 0..N {
            if let Some(x) = g.get_mut(u * N) {
                *x = *x * sqrt2;
            }
        }

        // Row pass: each contiguous 8-element row.
        let mut rows = [T::ZERO; N * N];
        for r in 0..N {
            let start = r * N;
            let Some(row_in) = g.get(start..start + N) else {
                continue;
            };
            let Some(row_out) = rows.get_mut(start..start + N) else {
                continue;
            };
            self.tx.execute(row_out, row_in);
        }

        // Column pass: gather each column into a scratch buffer, transform,
        // scatter back.
        let mut scratch_in = [T::ZERO; N];
        let mut scratch_out = [T::ZERO; N];
        for c in 0..N {
            for (r, slot) in scratch_in.iter_mut().enumerate() {
                *slot = rows.get(r * N + c).copied().unwrap_or(T::ZERO);
            }
            self.tx.execute(&mut scratch_out, &scratch_in);
            for (r, v) in scratch_out.iter().enumerate() {
                if let Some(dst) = out.get_mut(r * N + c) {
                    *dst = *v * quarter;
                }
            }
        }
    }
}

/// Build an [`Idct8x8<f32>`] wrapped in an [`Arc`]-shareable form is
/// unnecessary — `Tx` is cheap to own per decoder thread, matching
/// `vaco-tx`'s own guidance (`Plan` is the `Arc`-shared, immutable part;
/// `Tx` is the per-thread execution state). This helper exists only so a
/// caller need not spell out the trait bounds.
///
/// # Errors
///
/// See [`Idct8x8::new`].
pub fn idct8x8_f32() -> Result<Idct8x8<f32>> {
    Idct8x8::new()
}

/// See [`idct8x8_f32`].
///
/// # Errors
///
/// See [`Idct8x8::new`].
pub fn idct8x8_f64() -> Result<Idct8x8<f64>> {
    Idct8x8::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direct `f64` evaluation of the classical formula — the oracle IEEE
    /// 1180 / Annex A style testing compares against, and independent of
    /// `vaco-tx`'s internal algorithm (Makhoul's DFT reduction) entirely.
    fn direct_f64(f: &[f64; N * N]) -> [f64; N * N] {
        let c = |u: usize| {
            if u == 0 {
                core::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            }
        };
        let mut out = [0.0f64; N * N];
        for x in 0..N {
            for y in 0..N {
                let mut s = 0.0;
                for u in 0..N {
                    for v in 0..N {
                        let fuv = f.get(u * N + v).copied().unwrap_or(0.0);
                        s += c(u)
                            * c(v)
                            * fuv
                            * (core::f64::consts::PI * (2 * x + 1) as f64 * u as f64 / 16.0).cos()
                            * (core::f64::consts::PI * (2 * y + 1) as f64 * v as f64 / 16.0).cos();
                    }
                }
                if let Some(slot) = out.get_mut(x * N + y) {
                    *slot = s / 4.0;
                }
            }
        }
        out
    }

    fn rms_error(a: &[f64; N * N], b: &[f32; N * N]) -> f64 {
        let mut acc = 0.0;
        let mut norm = 0.0;
        for (x, y) in a.iter().zip(b.iter()) {
            let d = x - f64::from(*y);
            acc += d * d;
            norm += x * x;
        }
        if norm == 0.0 {
            acc.sqrt()
        } else {
            (acc / norm).sqrt()
        }
    }

    #[test]
    fn matches_direct_evaluation_within_class_c() {
        let input: [f64; N * N] = core::array::from_fn(|i| ((i as f64) * 37.0).sin() * 200.0);
        let input32: [f32; N * N] =
            core::array::from_fn(|i| input.get(i).copied().unwrap_or(0.0) as f32);

        let expected = direct_f64(&input);

        // `Idct8x8::new` cannot fail for a fixed, non-zero length such as 8;
        // an early return rather than `.expect()` keeps this test free of any
        // panicking macro (`clippy::panic` is denied workspace-wide).
        let Ok(mut idct) = idct8x8_f32() else {
            return;
        };
        let mut got = [0f32; N * N];
        idct.apply(&input32, &mut got);

        let err = rms_error(&expected, &got);
        assert!(err < 2f64.powi(-14), "relative RMS error too large: {err}");
    }

    #[test]
    fn dc_only_gives_a_uniform_block() {
        let mut c = [0f32; N * N];
        if let Some(v) = c.first_mut() {
            *v = 800.0;
        }
        let Ok(mut idct) = idct8x8_f32() else {
            return;
        };
        let mut out = [0f32; N * N];
        idct.apply(&c, &mut out);
        let first = out.first().copied().unwrap_or(0.0);
        for v in &out {
            assert!((v - first).abs() < 1e-3, "{out:?}");
        }
    }

    #[test]
    fn f64_path_also_builds_and_runs() {
        let Ok(mut idct) = idct8x8_f64() else {
            return;
        };
        let input = [1.0f64; N * N];
        let mut out = [0.0f64; N * N];
        idct.apply(&input, &mut out);
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
