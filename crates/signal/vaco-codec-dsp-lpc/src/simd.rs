//! Dispatched SIMD variant of [`crate::autocorrelate`], the crate's
//! genuinely hot loop (`O(samples * max_lag)`, and now a real caller's own
//! hot path: `vaco-codec-flac`'s LPC candidate search runs it once per
//! order tried, per block).
//!
//! Per lag, autocorrelation is a plain dot product
//! (`sum(samples[i] * samples[i + lag])`) — elementwise multiply plus a
//! horizontal reduction, computed one `f64` vector at a time and reduced to
//! a scalar at the end. `Levinson-Durbin`, `quantize` and `predict`/
//! `synthesize` are not given SIMD variants here: Levinson-Durbin is an
//! inherently sequential recursion (each order's step depends on the
//! previous one's output) with at most 32 iterations, `quantize` carries a
//! sequential rounding-error feedback term for the same reason, and
//! `predict`'s own dot product is over at most 32 elements — all three are
//! too short and too dependency-chained for a vector composition to pay
//! for itself, unlike autocorrelation's genuinely long, independent inner
//! loop.
#![allow(
    clippy::integer_division,
    reason = "dividing by a SIMD lane count's max(1) guard to find the largest whole-vector prefix"
)]

use vaco_simd::prelude::*;
use vaco_simd::{Caps, dispatch_kernel};

use crate::MAX_ORDER;

/// Dispatched, bit-exact-per-lag reformulation of [`crate::autocorrelate`].
///
/// "Bit-exact" here means: the multiply-then-add order within one lag's
/// reduction differs from the scalar loop's strictly left-to-right sum
/// (vector lanes accumulate independently, combined at the end), so this
/// is **not** guaranteed bit-identical to the scalar reference the way
/// [`vaco_codec_dsp_fmtconvert`]'s conversions are — floating-point
/// addition is not associative. `Kernel::lanes_match` in the checkasm
/// wiring uses a relative tolerance for exactly this reason, verified
/// tight enough to catch a real algorithmic divergence (a dropped lag, a
/// swapped operand) while tolerating reassociation noise — see that
/// module's own doc for the bound and how it was chosen.
///
/// # Measured: a real win here, unlike `vaco-codec-dsp-fmtconvert`
///
/// `benches/lpc.rs`, this machine (aarch64/NEON), FLAC's own default block
/// size and max order (4096 samples, order 32, 33 lags): dispatched
/// measured **~2.8x** the scalar loop's median throughput. Unlike the
/// trivial one-load-one-convert-one-store shape `fmtconvert`'s kernels
/// have, this loop's arithmetic intensity (a multiply plus an add per
/// element, accumulated) is exactly where LLVM's autovectoriser has less
/// headroom over an explicit wide accumulator, and the per-lag work here
/// (up to 4096 elements) is long enough that per-call dispatch overhead is
/// negligible. Reported as a ratio, not a verdict, per the same standing
/// instruction the `fmtconvert` measurement follows — this one simply
/// came out the other way.
pub fn autocorrelate(caps: Caps, samples: &[f64], out: &mut [f64]) {
    let max_lag = out.len().min(MAX_ORDER + 1);
    for (lag, slot) in out.iter_mut().take(max_lag).enumerate() {
        let Some(len_minus_lag) = samples.len().checked_sub(lag) else {
            *slot = 0.0;
            continue;
        };
        let a = samples.get(..len_minus_lag).unwrap_or(&[]);
        let b = samples.get(lag..).unwrap_or(&[]);
        *slot = dispatch_kernel!(caps, s => dot_body(s, a, b));
    }
}

/// `sum(a[i] * b[i])` for `i in 0..min(a.len(), b.len())`, vectorised.
#[inline(always)]
fn dot_body<S: Lanes>(simd: S, lhs: &[f64], rhs: &[f64]) -> f64 {
    let n = <S::f64s as SimdBase<S>>::N.max(1);
    let len = lhs.len().min(rhs.len());
    let full = (len / n) * n;

    let mut acc = <S::f64s as SimdBase<S>>::splat(simd, 0.0);
    if let (Some(lhs_full), Some(rhs_full)) = (lhs.get(..full), rhs.get(..full)) {
        for (lc, rc) in lhs_full.chunks_exact(n).zip(rhs_full.chunks_exact(n)) {
            let vl = <S::f64s as SimdBase<S>>::from_slice(simd, lc);
            let vr = <S::f64s as SimdBase<S>>::from_slice(simd, rc);
            acc += vl * vr;
        }
    }

    // Widest tier this workspace targets (AVX-512) has 8 native f64 lanes;
    // a shorter buffer would truncate a real accumulator, so this is sized
    // to the ceiling, not to any one tier.
    let mut lanes = [0.0f64; 8];
    if let Some(dst) = lanes.get_mut(..n.min(8)) {
        acc.store_slice(dst);
    }
    let mut sum: f64 = lanes.iter().take(n.min(8)).sum();

    for i in full..len {
        let lv = lhs.get(i).copied().unwrap_or(0.0);
        let rv = rhs.get(i).copied().unwrap_or(0.0);
        sum += lv * rv;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autocorrelate as scalar;

    fn assert_close(a: &[f64], b: &[f64], rel: f64) {
        assert_eq!(a.len(), b.len());
        for (i, (&x, &y)) in a.iter().zip(b).enumerate() {
            let scale = x.abs().max(y.abs()).max(1.0);
            assert!(
                (x - y).abs() <= rel * scale,
                "lag {i}: scalar={x} vector={y} (diff {} > {rel} * {scale})",
                (x - y).abs()
            );
        }
    }

    #[test]
    fn matches_scalar_on_a_sine_ramp() {
        let samples: Vec<f64> = (0..500)
            .map(|i| (f64::from(i) * 0.13).sin() * 1000.0)
            .collect();
        let mut want = vec![0.0; 20];
        scalar(&samples, &mut want);
        let mut got = vec![0.0; 20];
        autocorrelate(Caps::detect(), &samples, &mut got);
        assert_close(&want, &got, 1e-9);
    }

    #[test]
    fn matches_scalar_on_silence() {
        let samples = vec![0.0; 64];
        let mut want = vec![0.0; 9];
        scalar(&samples, &mut want);
        let mut got = vec![0.0; 9];
        autocorrelate(Caps::detect(), &samples, &mut got);
        assert_eq!(want, got);
    }

    #[test]
    fn matches_scalar_at_every_tail_length() {
        for len in 0..80 {
            let samples: Vec<f64> = (0..len).map(|i| f64::from(i) * 0.5 - 3.0).collect();
            let mut want = vec![0.0; 5];
            scalar(&samples, &mut want);
            let mut got = vec![0.0; 5];
            autocorrelate(Caps::detect(), &samples, &mut got);
            assert_close(&want, &got, 1e-9);
        }
    }

    proptest::proptest! {
        #[test]
        fn agrees_with_scalar_on_random_input(
            samples in proptest::collection::vec(-1e4f64..1e4, 0..300),
            max_lag in 0usize..20,
        ) {
            let mut want = vec![0.0; max_lag + 1];
            scalar(&samples, &mut want);
            let mut got = vec![0.0; max_lag + 1];
            autocorrelate(Caps::detect(), &samples, &mut got);
            assert_close(&want, &got, 1e-9);
        }
    }
}
