//! Dispatched SIMD variant of [`crate::dc_predict`]'s reference-sample
//! summation (D-09, #126).
//!
//! `planar_predict` and `angular_project` are not given SIMD variants in
//! this pass: `planar_predict` writes one row at a time with a genuinely
//! vectorisable per-`x` formula (a linear ramp plus a per-column multiply),
//! but so does `add_pixels_clamped` in `vaco-codec-dsp-idct`, and that
//! kernel's own measurement (per-row `dispatch_kernel!` overhead, at block
//! sizes too small to amortise it — see that crate's `simd` module) argues
//! against assuming a win here without first measuring the row-dispatch
//! shape specifically, which this pass did not have time to do carefully.
//! `angular_project` produces one row of up to `size` elements per call
//! (same small-N concern) and its per-lane arithmetic (two multiplies, a
//! shift) is cheap enough relative to a dispatch call that the ratio is
//! unlikely to favour it either. `dc_predict`'s summation was picked
//! instead because it is the one part of this crate shaped like
//! `vaco-codec-dsp-lpc::autocorrelate` — a single reduction over a
//! caller-provided array, one dispatch per call, no per-row loop — which
//! is the shape that pass's own measurements showed actually winning.
//!
//! # Measured — a real, modest win, and a lesson about `black_box`
//!
//! `benches/intrapred.rs`, this machine (aarch64/NEON), size 32 (64
//! reference samples summed): dispatched runs at ~1.28x the scalar loop's
//! throughput. The first measurement of this benchmark (without
//! `divan::black_box` around `top`/`left`, both compile-time-constant
//! arrays) reported a misleading ~3.2x with the two paths' *fastest*
//! sample tying exactly — the signature of LLVM partially constant-folding
//! one path and not the other rather than measuring two real runtime
//! executions consistently. Adding `black_box` around the inputs (kept in
//! the committed benchmark) collapsed that gap to this smaller, trustworthy
//! number, where fastest and median now move together. Worth keeping in
//! mind for any future micro-benchmark in this workspace at a problem size
//! small enough that the whole computation is a candidate for folding.

use vaco_simd::prelude::*;
use vaco_simd::{Caps, dispatch_kernel};

use crate::dc::{mid_grey, round_div};

/// Dispatched, bit-exact [`crate::dc_predict`].
#[must_use]
pub fn dc_predict(caps: Caps, top: &[u16], left: &[u16], size: usize, bit_depth: u32) -> u16 {
    let top = if top.len() >= size {
        top.get(..size)
    } else {
        None
    };
    let left = if left.len() >= size {
        left.get(..size)
    } else {
        None
    };

    match (top, left) {
        (Some(t), Some(l)) => {
            let sum = sum_u16(caps, t) + sum_u16(caps, l);
            let count = u32::try_from(2 * size).unwrap_or(1).max(1);
            round_div(sum, count)
        }
        (Some(t), None) => average(caps, t),
        (None, Some(l)) => average(caps, l),
        (None, None) => mid_grey(bit_depth),
    }
}

fn average(caps: Caps, samples: &[u16]) -> u16 {
    let sum = sum_u16(caps, samples);
    let count = u32::try_from(samples.len()).unwrap_or(1).max(1);
    round_div(sum, count)
}

/// Dispatched `sum(values) as u32`, exact (no `u16` sum over any realistic
/// block can approach `u32::MAX`: even `size = 4096` at `u16::MAX` per
/// element is far short of overflowing).
fn sum_u16(caps: Caps, values: &[u16]) -> u32 {
    dispatch_kernel!(caps, s => sum_u16_body(s, values))
}

#[inline(always)]
#[allow(
    clippy::integer_division,
    reason = "dividing by a SIMD lane count's max(1) guard to find the largest whole-vector prefix"
)]
fn sum_u16_body<S: Lanes>(simd: S, values: &[u16]) -> u32 {
    let n = <S::u16s as SimdBase<S>>::N.max(1);
    let full = (values.len() / n) * n;

    let mut acc = <S::u32s as SimdBase<S>>::splat(simd, 0);
    if let Some(full_slice) = values.get(..full) {
        for chunk in full_slice.chunks_exact(n) {
            let v = <S::u16s as SimdBase<S>>::from_slice(simd, chunk);
            let (lo, hi) = v.widen();
            acc = acc + lo + hi;
        }
    }

    // Widest tier this workspace targets (AVX-512) has 16 native u32 lanes.
    let mut lanes = [0u32; 16];
    let m = <S::u32s as SimdBase<S>>::N.min(16);
    if let Some(dst) = lanes.get_mut(..m) {
        acc.store_slice(dst);
    }
    let mut sum: u32 = lanes.iter().take(m).sum();

    for &v in values.get(full..).unwrap_or(&[]) {
        sum += u32::from(v);
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dc_predict as scalar;

    #[test]
    fn matches_scalar_both_sides() {
        let top: Vec<u16> = (0..40).map(|i| i * 3 + 1).collect();
        let left: Vec<u16> = (0..40).map(|i| i * 7 + 2).collect();
        for size in [1usize, 4, 8, 16, 17, 32, 40] {
            let got = dc_predict(Caps::detect(), &top, &left, size, 8);
            let want = scalar(&top, &left, size, 8);
            assert_eq!(got, want, "size={size}");
        }
    }

    #[test]
    fn matches_scalar_one_side_and_neither() {
        let top: Vec<u16> = (0..40).map(|i| i * 3 + 1).collect();
        for size in [1usize, 4, 16, 40] {
            assert_eq!(
                dc_predict(Caps::detect(), &top, &[], size, 8),
                scalar(&top, &[], size, 8)
            );
            assert_eq!(
                dc_predict(Caps::detect(), &[], &top, size, 8),
                scalar(&[], &top, size, 8)
            );
        }
        assert_eq!(
            dc_predict(Caps::detect(), &[], &[], 8, 10),
            scalar(&[], &[], 8, 10)
        );
    }

    #[test]
    fn matches_scalar_at_extremes() {
        let top = vec![u16::MAX; 64];
        let left = vec![u16::MAX; 64];
        assert_eq!(
            dc_predict(Caps::detect(), &top, &left, 64, 12),
            scalar(&top, &left, 64, 12)
        );
    }

    proptest::proptest! {
        #[test]
        fn agrees_with_scalar_random(
            top in proptest::collection::vec(proptest::num::u16::ANY, 0..200),
            left in proptest::collection::vec(proptest::num::u16::ANY, 0..200),
            size in 0usize..200,
            bit_depth in 0u32..16,
        ) {
            let got = dc_predict(Caps::detect(), &top, &left, size, bit_depth);
            let want = scalar(&top, &left, size, bit_depth);
            proptest::prop_assert_eq!(got, want);
        }
    }
}
