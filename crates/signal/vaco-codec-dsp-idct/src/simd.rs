//! Dispatched SIMD variant of [`crate::blockdsp::add_pixels_clamped`] (D-11,
//! #123) — every block-based codec's `Clip1(pred + residual)`
//! reconstruction step, and the natural target for a first SIMD pass here:
//! it runs once per reconstructed block, at every block size from 4×4 up to
//! 32×32 or 64×64.
//!
//! `get_pixels`/`diff_pixels` (the encoder-side counterparts) and the other
//! `blockdsp` functions are not given SIMD variants in this pass: `fill_block`
//! and `clear_block` are already a single `slice::fill`/loop of zero-cost
//! writes with nothing for a vector composition to add, and `get_pixels`/
//! `diff_pixels`/`put_pixels_clamped` share this file's exact shape closely
//! enough that a follow-up can copy this kernel's structure directly once a
//! second one is needed — kept to one worked example per this crate's own
//! "correctness first, breadth later" precedent (see the crate root doc's
//! note on why this crate has no SIMD path yet, which this file begins to
//! address).
//!
//! # Measured, not assumed: this does not win either
//!
//! `benches/idct.rs`, this machine (aarch64/NEON): at a 16x16 block (a
//! common macroblock size), dispatched measured **~0.9x** the scalar
//! loop's median throughput -- a small regression, not a win, and it does
//! not improve at 64x64 either (~0.84x, tested and reverted rather than
//! kept in the committed benchmark). Two likely contributors: the
//! per-row `dispatch_kernel!` call means dispatch overhead is paid `h`
//! times, not once, and LLVM already autovectorises the scalar
//! widen-add-clamp-narrow loop about as well as this explicit composition
//! does, similar to `vaco-codec-dsp-fmtconvert`'s own measurement.
//! Reported per the same standing instruction: shipped for correctness
//! and checkasm coverage (it also fixed a real overflow bug the scalar
//! path never had, which is worth having caught regardless of the speed
//! result), not for a measured win.
//!
//! # Gating: the public entry point is scalar-by-measurement
//!
//! A losing kernel behind a dispatch layer reads as an optimisation to
//! the next caller, and costs real throughput if they trust that
//! reading — this crate's own DC-mode callers (VP8, VP9, H.264) are
//! exactly the precedent for a caller arriving here next. So
//! [`add_pixels_clamped`] routes to [`crate::blockdsp::add_pixels_clamped`]
//! (the scalar reference) rather than [`add_pixels_clamped_vector`]. The
//! dispatched body stays wired into `vaco-checkasm` regardless, so it
//! cannot silently rot while it is not on the hot path.

use vaco_simd::prelude::*;
use vaco_simd::{Caps, dispatch_kernel, ops};

/// [`crate::blockdsp::add_pixels_clamped`]'s public SIMD-crate entry
/// point, gated to the scalar path. **Scalar-by-measurement, not
/// dispatched**: `benches/idct.rs` on aarch64/NEON measured the dispatched
/// path at ~0.9x the scalar loop's throughput at 16x16 and ~0.84x at
/// 64x64 (see this module's doc) — a pessimisation on the one target this
/// was measured on. `caps` is accepted and ignored so the signature does
/// not need to change if a wider target (AVX-512 is untested) inverts the
/// ratio; re-measure there before flipping this to call
/// [`add_pixels_clamped_vector`] instead.
pub fn add_pixels_clamped(
    caps: Caps,
    residual: &[i16],
    dst: &mut [u8],
    stride: usize,
    w: usize,
    h: usize,
) {
    let _ = caps;
    crate::blockdsp::add_pixels_clamped(residual, dst, stride, w, h);
}

/// The dispatched path [`add_pixels_clamped`] does not currently call —
/// see that function's doc for why. Exists so `vaco-checkasm` can keep
/// verifying it under `Differential` independently of which path
/// [`add_pixels_clamped`] routes through.
pub fn add_pixels_clamped_vector(
    caps: Caps,
    residual: &[i16],
    dst: &mut [u8],
    stride: usize,
    w: usize,
    h: usize,
) {
    for row in 0..h {
        let Some(res_row) = residual
            .get(row.saturating_mul(w)..)
            .and_then(|r| r.get(..w))
        else {
            return;
        };
        let Some(dst_row) = dst
            .get_mut(row.saturating_mul(stride)..)
            .and_then(|r| r.get_mut(..w))
        else {
            return;
        };
        dispatch_kernel!(caps, s => add_row_body(s, dst_row, res_row));
    }
}

/// One row: `dst[i] = Clip1(dst[i] + residual[i])`, native-width vectors at
/// a time, scalar for the tail.
///
/// **Widens all the way to `i32` before adding**, matching the scalar
/// reference's own `i32::from(*d) + i32::from(r)` exactly. An earlier
/// version of this function added directly on the `i16` halves
/// `ops::simd::widen_u8_i16` produces — correct for `dst` (`0..=255`
/// widened to `i16` cannot overflow) but wrong for `residual`, which is an
/// unrestricted `&[i16]`: a residual near `i16::MAX` (found by this
/// module's own proptest, not assumed) overflows a 16-bit add before the
/// clamp ever runs, wrapping to a negative number and clamping to `0`
/// instead of saturating to `255`. Widening the `i16` halves once more to
/// `i32`, adding there (where `0..=255 + i16::MIN..=i16::MAX` cannot
/// overflow), clamping to `0..=255`, and narrowing back down closes that
/// gap — the proptest below is the regression test.
#[inline(always)]
#[allow(
    clippy::integer_division,
    reason = "dividing by a SIMD lane count's max(1) guard, and halving it, to find whole-vector boundaries"
)]
fn add_row_body<S: Lanes>(simd: S, dst: &mut [u8], residual: &[i16]) {
    let n = <S::u8s as SimdBase<S>>::N.max(1);
    let half = n / 2;
    let len = dst.len().min(residual.len());
    let full = if half == 0 { 0 } else { (len / n) * n };

    let Some((dst_full, dst_tail)) = dst.split_at_mut_checked(full) else {
        return scalar_tail(dst, residual);
    };
    let Some((res_full, res_tail)) = residual.split_at_checked(full) else {
        return scalar_tail(dst, residual);
    };

    let zero = <S::i32s as SimdBase<S>>::splat(simd, 0);
    let max255 = <S::i32s as SimdBase<S>>::splat(simd, 255);

    for (d_chunk, r_chunk) in dst_full.chunks_exact_mut(n).zip(res_full.chunks_exact(n)) {
        let v = <S::u8s as SimdBase<S>>::from_slice(simd, d_chunk);
        let (dlo16, dhi16) = ops::simd::widen_u8_i16::<S>(v);
        let Some((r_lo, r_hi)) = r_chunk.split_at_checked(half) else {
            continue;
        };
        let rlo16 = <S::i16s as SimdBase<S>>::from_slice(simd, r_lo);
        let rhi16 = <S::i16s as SimdBase<S>>::from_slice(simd, r_hi);

        // Widen both i16 halves to i32, add there, clamp, narrow back.
        let (dlo_lo, dlo_hi) = dlo16.widen();
        let (rlo_lo, rlo_hi) = rlo16.widen();
        let sum_lo_lo = (dlo_lo + rlo_lo).max(zero).min(max255);
        let sum_lo_hi = (dlo_hi + rlo_hi).max(zero).min(max255);
        let new_lo16: S::i16s = sum_lo_lo.narrow(sum_lo_hi);

        let (dhi_lo, dhi_hi) = dhi16.widen();
        let (rhi_lo, rhi_hi) = rhi16.widen();
        let sum_hi_lo = (dhi_lo + rhi_lo).max(zero).min(max255);
        let sum_hi_hi = (dhi_hi + rhi_hi).max(zero).min(max255);
        let new_hi16: S::i16s = sum_hi_lo.narrow(sum_hi_hi);

        let packed = ops::simd::pack_u8_from_i16::<S>(new_lo16, new_hi16);
        packed.store_slice(d_chunk);
    }

    scalar_tail(dst_tail, res_tail);
}

fn scalar_tail(dst: &mut [u8], residual: &[i16]) {
    for (d, &r) in dst.iter_mut().zip(residual) {
        let sum = i32::from(*d) + i32::from(r);
        *d = sum.clamp(0, 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockdsp::add_pixels_clamped as scalar;

    #[test]
    fn matches_scalar_no_saturation() {
        let mut got = vec![100u8, 100, 100, 100, 100, 100, 100, 100];
        let mut want = got.clone();
        let residual = [10i16, -10, 0, 5, -100, 100, 1, -1];
        add_pixels_clamped_vector(Caps::detect(), &residual, &mut got, 8, 8, 1);
        scalar(&residual, &mut want, 8, 8, 1);
        assert_eq!(got, want);
    }

    #[test]
    fn matches_scalar_with_saturation_both_directions() {
        let mut got = vec![250u8, 5, 0, 255];
        let mut want = got.clone();
        let residual = [100i16, -100, -50, 50];
        add_pixels_clamped_vector(Caps::detect(), &residual, &mut got, 4, 4, 1);
        scalar(&residual, &mut want, 4, 4, 1);
        assert_eq!(got, want);
        assert_eq!(got, vec![255, 0, 0, 255]);
    }

    #[test]
    fn matches_scalar_at_every_width_up_to_64_and_multi_row() {
        for w in 0..64 {
            for h in [1usize, 2, 3] {
                let len = w * h;
                let base: Vec<u8> = (0..len).map(|i| ((i * 37) % 256) as u8).collect();
                let residual: Vec<i16> = (0..len)
                    .map(|i| i32::try_from((i * 91) % 400).unwrap_or(0) as i16 - 200)
                    .collect();
                let stride = w + 3; // stride wider than width, on purpose
                let mut got = vec![7u8; stride * h.max(1)];
                let mut want = got.clone();
                // Seed both buffers with the same starting pixel values in
                // their real (w-wide) positions.
                for row in 0..h {
                    let gslice = got.get_mut(row * stride..row * stride + w);
                    let wslice = want.get_mut(row * stride..row * stride + w);
                    let bslice = base.get(row * w..row * w + w);
                    if let (Some(g), Some(wv), Some(b)) = (gslice, wslice, bslice) {
                        g.copy_from_slice(b);
                        wv.copy_from_slice(b);
                    }
                }
                add_pixels_clamped_vector(Caps::detect(), &residual, &mut got, stride, w, h);
                scalar(&residual, &mut want, stride, w, h);
                assert_eq!(got, want, "w={w} h={h}");
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn agrees_with_scalar_random(
            dst_init in proptest::collection::vec(proptest::num::u8::ANY, 0..256),
            residual in proptest::collection::vec(proptest::num::i16::ANY, 0..256),
            stride in 1usize..32,
            w in 0usize..32,
            h in 0usize..16,
        ) {
            let mut got = dst_init.clone();
            let mut want = dst_init;
            add_pixels_clamped_vector(Caps::detect(), &residual, &mut got, stride, w, h);
            scalar(&residual, &mut want, stride, w, h);
            proptest::prop_assert_eq!(got, want);
        }
    }

    /// Pins the gating itself: the public `add_pixels_clamped` entry point
    /// must route to the scalar reference directly, not through
    /// `add_pixels_clamped_vector`. Guards against a future edit silently
    /// re-enabling dispatch here without re-measuring first (see this
    /// module's doc's "Gating" section for why that would be a regression on
    /// the one target this was measured on).
    #[test]
    fn public_entry_matches_scalar_directly() {
        let residual = [10i16, -10, 0, 5, -100, 100, 1, -1];
        let mut got = vec![100u8, 100, 100, 100, 100, 100, 100, 100];
        let mut want = got.clone();
        add_pixels_clamped(Caps::detect(), &residual, &mut got, 8, 8, 1);
        scalar(&residual, &mut want, 8, 8, 1);
        assert_eq!(got, want);
    }
}
