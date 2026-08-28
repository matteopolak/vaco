//! Dispatched SIMD variants of the two exact widen-and-convert kernels:
//! [`int16_to_float`] and [`int32_to_float`].
//!
//! Both are bit-exact reformulations of their scalar counterparts, not
//! approximations: `i16 -> i32` is a sign-extending widen (exact, no
//! rounding), and `i32 -> f32` (with or without the fixed `1/2^31` scale
//! folded in as a single multiply) is one IEEE-754 round-to-nearest
//! operation — the same operation the scalar path performs, so there is no
//! daylight for the two to disagree. `float_to_int16`/`float_to_int32` are
//! deliberately **not** given a SIMD variant here: their scalar reference
//! rounds half-away-from-zero (matching `f32::round`), and `fearless_simd`
//! only exposes `round_ties_even` natively — reproducing half-away-from-zero
//! needs a `trunc(x + copysign(0.5, x))` composition whose edge-case
//! agreement (signed zeros, NaN, values near `i16`/`i32` saturation) was not
//! verified against the scalar reference in this pass. Per this crate's own
//! bar (a wrong fast kernel is worthless), an unverified composition is not
//! shipped; a future pass can add it once `Differential` confirms it agrees
//! at every edge case in `vaco_simd::edge`-equivalent coverage.
//!
//! # Measured, not assumed: this does not win on aarch64/NEON
//!
//! `benches/fmtconvert.rs`, this machine (aarch64/NEON), 1024 and 65536
//! `i16`/`i32` elements: `int16_to_float` dispatched measured **~0.65x**
//! the scalar loop's throughput at both sizes (not fixed dispatch
//! overhead amortising away — the ratio holds from 1024 to 65536
//! elements); `int32_to_float` measured **~0.96x**, a wash. LLVM already
//! autovectorises the scalar `.zip()` loop for this operation (one load,
//! one widen-or-convert, one store, no reduction) about as well as this
//! explicit composition does, and the composition's extra per-chunk
//! bookkeeping (a checked split every iteration) does not pay for itself.
//! Reported per this project's own standing instruction to report ratios,
//! not verdicts, rather than assumed or hidden: **shipped for
//! correctness and `vaco-checkasm` coverage, not for a measured win.** A
//! future pass attempting to actually beat the scalar path here should
//! start from disassembly of the scalar loop, not from adding more
//! composition.

// Every `n / 2` and `(len / n) * n` below divides by a SIMD native lane
// count (never zero for a real `S: Lanes`) or its `.max(1)` guard, to
// compute the largest whole-vector prefix -- truncation is the point, not
// a bug.
#![allow(
    clippy::integer_division,
    reason = "dividing by a SIMD lane count (or its max(1) guard) to find the largest whole-vector prefix"
)]

use vaco_simd::prelude::*;
use vaco_simd::{Caps, dispatch_kernel};

/// Dispatched, bit-exact [`crate::int16_to_float`]: writes `min(dst.len(),
/// src.len())` elements.
pub fn int16_to_float(caps: Caps, src: &[i16], dst: &mut [f32]) {
    let len = src.len().min(dst.len());
    let (Some(src), Some(dst)) = (src.get(..len), dst.get_mut(..len)) else {
        return;
    };
    dispatch_kernel!(caps, s => int16_to_float_body(s, src, dst));
}

/// Dispatched, bit-exact [`crate::int32_to_float`]: writes `min(dst.len(),
/// src.len())` elements.
pub fn int32_to_float(caps: Caps, src: &[i32], dst: &mut [f32]) {
    let len = src.len().min(dst.len());
    let (Some(src), Some(dst)) = (src.get(..len), dst.get_mut(..len)) else {
        return;
    };
    dispatch_kernel!(caps, s => int32_to_float_body(s, src, dst));
}

/// Dispatched, bit-exact [`crate::int32_to_float_fmul_scalar`]: writes
/// `min(dst.len(), src.len())` elements.
pub fn int32_to_float_fmul_scalar(caps: Caps, src: &[i32], mul: f32, dst: &mut [f32]) {
    let len = src.len().min(dst.len());
    let (Some(src), Some(dst)) = (src.get(..len), dst.get_mut(..len)) else {
        return;
    };
    dispatch_kernel!(caps, s => int32_to_float_fmul_body(s, src, mul, dst));
}

/// `i16 -> i32` (sign-extending widen, exact) `-> f32` (exact for the full
/// `i16` range): one IEEE-754 round-to-nearest float conversion, the same
/// operation [`crate::int16_to_float`]'s scalar loop performs per element.
#[inline(always)]
fn int16_to_float_body<S: Lanes>(simd: S, src: &[i16], dst: &mut [f32]) {
    let n = <S::i16s as SimdBase<S>>::N;
    let half = n / 2;
    if half == 0 {
        return scalar_tail_i16(src, dst);
    }
    let full = (src.len() / n) * n;
    let Some((src_full, src_tail)) = src.split_at_checked(full) else {
        return scalar_tail_i16(src, dst);
    };
    let Some((dst_full, dst_tail)) = dst.split_at_mut_checked(full) else {
        return scalar_tail_i16(src, dst);
    };

    for (s_chunk, d_chunk) in src_full.chunks_exact(n).zip(dst_full.chunks_exact_mut(n)) {
        let v = <S::i16s as SimdBase<S>>::from_slice(simd, s_chunk);
        let (lo, hi) = v.widen();
        let flo: S::f32s = SimdCvtFloat::float_from(lo);
        let fhi: S::f32s = SimdCvtFloat::float_from(hi);
        let Some((d_lo, d_hi)) = d_chunk.split_at_mut_checked(half) else {
            continue;
        };
        flo.store_slice(d_lo);
        fhi.store_slice(d_hi);
    }
    scalar_tail_i16(src_tail, dst_tail);
}

#[inline(always)]
fn int32_to_float_body<S: Lanes>(simd: S, src: &[i32], dst: &mut [f32]) {
    let n = <S::i32s as SimdBase<S>>::N;
    let scale = <S::f32s as SimdBase<S>>::splat(simd, crate::convert::INT32_TO_FLOAT_SCALE);
    let full = (src.len() / n.max(1)) * n.max(1);
    let Some((src_full, src_tail)) = src.split_at_checked(full) else {
        return scalar_tail_i32(src, dst, 1.0);
    };
    let Some((dst_full, dst_tail)) = dst.split_at_mut_checked(full) else {
        return scalar_tail_i32(src, dst, 1.0);
    };
    for (s_chunk, d_chunk) in src_full.chunks_exact(n.max(1)).zip(dst_full.chunks_exact_mut(n.max(1))) {
        let v = <S::i32s as SimdBase<S>>::from_slice(simd, s_chunk);
        let f: S::f32s = SimdCvtFloat::float_from(v);
        (f * scale).store_slice(d_chunk);
    }
    scalar_tail_i32(src_tail, dst_tail, 1.0);
}

#[inline(always)]
fn int32_to_float_fmul_body<S: Lanes>(simd: S, src: &[i32], mul: f32, dst: &mut [f32]) {
    let n = <S::i32s as SimdBase<S>>::N;
    let mulv = <S::f32s as SimdBase<S>>::splat(simd, mul);
    let full = (src.len() / n.max(1)) * n.max(1);
    let Some((src_full, src_tail)) = src.split_at_checked(full) else {
        return scalar_tail_fmul(src, dst, mul);
    };
    let Some((dst_full, dst_tail)) = dst.split_at_mut_checked(full) else {
        return scalar_tail_fmul(src, dst, mul);
    };
    for (s_chunk, d_chunk) in src_full.chunks_exact(n.max(1)).zip(dst_full.chunks_exact_mut(n.max(1))) {
        let v = <S::i32s as SimdBase<S>>::from_slice(simd, s_chunk);
        let f: S::f32s = SimdCvtFloat::float_from(v);
        (f * mulv).store_slice(d_chunk);
    }
    scalar_tail_fmul(src_tail, dst_tail, mul);
}

fn scalar_tail_i16(src: &[i16], dst: &mut [f32]) {
    for (d, &s) in dst.iter_mut().zip(src) {
        *d = f32::from(s);
    }
}

fn scalar_tail_i32(src: &[i32], dst: &mut [f32], _unused: f32) {
    crate::convert::int32_to_float(dst, src);
}

fn scalar_tail_fmul(src: &[i32], dst: &mut [f32], mul: f32) {
    crate::convert::int32_to_float_fmul_scalar(dst, src, mul);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{int16_to_float as scalar_i16, int32_to_float as scalar_i32};

    #[test]
    fn int16_to_float_matches_scalar_on_a_ramp() {
        let src: Vec<i16> = (0..300).map(|i| (i * 37 - 5000) as i16).collect();
        let mut got = vec![0.0f32; src.len()];
        int16_to_float(Caps::detect(), &src, &mut got);
        let mut want = vec![0.0f32; src.len()];
        scalar_i16(&mut want, &src);
        assert_eq!(got, want);
    }

    #[test]
    fn int16_to_float_matches_scalar_at_extremes() {
        let src = [i16::MIN, i16::MIN + 1, -1, 0, 1, i16::MAX - 1, i16::MAX];
        let mut got = vec![0.0f32; src.len()];
        int16_to_float(Caps::detect(), &src, &mut got);
        let mut want = vec![0.0f32; src.len()];
        scalar_i16(&mut want, &src);
        assert_eq!(got, want);
    }

    #[test]
    fn int32_to_float_matches_scalar_on_a_ramp() {
        let src: Vec<i32> = (0..300i64).map(|i| (i * 7_919_431 - 500_000_000) as i32).collect();
        let mut got = vec![0.0f32; src.len()];
        int32_to_float(Caps::detect(), &src, &mut got);
        let mut want = vec![0.0f32; src.len()];
        scalar_i32(&mut want, &src);
        assert_eq!(got, want);
    }

    #[test]
    fn int32_to_float_matches_scalar_at_extremes() {
        let src = [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX];
        let mut got = vec![0.0f32; src.len()];
        int32_to_float(Caps::detect(), &src, &mut got);
        let mut want = vec![0.0f32; src.len()];
        scalar_i32(&mut want, &src);
        assert_eq!(got, want);
    }

    #[test]
    fn every_tail_length_matches_scalar() {
        for len in 0..40 {
            let src: Vec<i16> = (0..len).map(|i| i16::try_from((i * 91) % 30000).unwrap_or(0)).collect();
            let mut got = vec![0.0f32; len];
            int16_to_float(Caps::detect(), &src, &mut got);
            let mut want = vec![0.0f32; len];
            scalar_i16(&mut want, &src);
            assert_eq!(got, want, "len={len}");
        }
    }

    proptest::proptest! {
        #[test]
        fn int16_to_float_agrees_with_scalar_random(src in proptest::collection::vec(proptest::num::i16::ANY, 0..512)) {
            let mut got = vec![0.0f32; src.len()];
            int16_to_float(Caps::detect(), &src, &mut got);
            let mut want = vec![0.0f32; src.len()];
            scalar_i16(&mut want, &src);
            proptest::prop_assert_eq!(got, want);
        }

        #[test]
        fn int32_to_float_agrees_with_scalar_random(src in proptest::collection::vec(proptest::num::i32::ANY, 0..512)) {
            let mut got = vec![0.0f32; src.len()];
            int32_to_float(Caps::detect(), &src, &mut got);
            let mut want = vec![0.0f32; src.len()];
            scalar_i32(&mut want, &src);
            proptest::prop_assert_eq!(got, want);
        }
    }
}
