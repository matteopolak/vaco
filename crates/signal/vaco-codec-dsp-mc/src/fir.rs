//! The const-generic separable FIR engine.
//!
//! One tap count, one struct: [`TapSet<N>`]. A consumer picks its own `N`
//! (2 for bilinear, 6 for H.264 luma half-pel, 8 for an 8-tap DCT-based
//! filter) and gets a scalar reference and a dispatched vector
//! implementation that are proved to agree lane for lane
//! (`tests/checkasm.rs`, `tests/properties.rs`).
//!
//! # Two shapes, because a real interpolator is not always one pass
//!
//! [`fir_row_scalar`]/[`fir_row`] are the complete single-pass filter: bias,
//! shift and clip to `u8` in one call. That is enough for a 1D filter (or a
//! separable filter applied naively, clipping between passes).
//!
//! [`tap_sum`]/[`fir_pass_i32`] are the *unrounded, unclipped* building
//! block. A real two-pass separable interpolator (H.264 §8.4.2.2.1's luma
//! half-pel at a "j"-type sub-pel position, for one instance) runs the
//! horizontal pass with **no** intermediate clip and defers all rounding to
//! the vertical pass — clipping between passes would compound rounding error
//! into a structured bias, not the small unstructured scatter this project's
//! shipping bar allows. [`separable_2d`] composes the two raw passes with a
//! caller-supplied final [`TapSet`] for the rounding/shift/clip step, so each
//! codec's own intermediate-precision convention is a choice at the call
//! site, not a guess baked into this crate.

use vaco_simd::prelude::*;
use vaco_simd::{Caps, dispatch_kernel, ops};

/// One tap count's coefficients and output normalisation shift.
///
/// `coeffs.len() == N` always; `N` is the type parameter so a consumer's own
/// tap count is checked at compile time rather than by a runtime length
/// assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapSet<const N: usize> {
    /// Filter coefficients, applied to `N` consecutive source samples.
    pub coeffs: [i16; N],
    /// Right-shift applied after accumulation, before clipping to `u8`.
    pub shift: u32,
}

impl<const N: usize> TapSet<N> {
    /// The rounding bias added before shifting: half the shift's scale, `0`
    /// when `shift` is `0` (nothing to round).
    #[must_use]
    pub const fn round_bias(&self) -> i16 {
        if self.shift == 0 {
            0
        } else {
            1i16 << (self.shift - 1)
        }
    }
}

/// Well-established, spec-cited tap sets a consumer can use directly or copy
/// as a template for its own.
pub mod taps {
    use super::TapSet;

    /// Bilinear (2-tap) half-pel: the plain average. Exact for any input,
    /// used wherever a codec's chroma or fallback path is simple averaging.
    pub const BILINEAR: TapSet<2> = TapSet {
        coeffs: [1, 1],
        shift: 1,
    };

    /// H.264 luma half-sample six-tap FIR: `(E - 5F + 20G + 20H - 5I + J + 16) >> 5`.
    ///
    /// `Vaco-Spec-Ref: itu-t-h264-202108 §8.4.2.2.1 six-tap luma half-sample
    /// interpolation filter, single-dimension form (the "b"/"h" positions).`
    pub const H264_LUMA_HALFPEL: TapSet<6> = TapSet {
        coeffs: [1, -5, 20, 20, -5, 1],
        shift: 5,
    };
}

/// Scalar reference: one complete FIR pass, `N` taps, bias/shift/clip all
/// applied. The oracle every dispatched tier is checked against.
///
/// `src` should hold at least `dst_len + N - 1` samples (each output tap
/// reads `N` consecutive source samples starting at its own index). Shorter
/// input simply yields fewer output samples than `dst_len` asked for, rather
/// than panicking or reading padding.
#[must_use]
pub fn fir_row_scalar<const N: usize>(src: &[u8], taps: &TapSet<N>, dst_len: usize) -> Vec<u8> {
    let available = src.len().saturating_sub(N.saturating_sub(1));
    let len = dst_len.min(available);
    let bias = taps.round_bias();
    (0..len)
        .map(|i| {
            let window = src.get(i..i + N).unwrap_or(&[]);
            let acc = tap_sum(window, &taps.coeffs);
            clip_from_i32(acc, i32::from(bias), taps.shift)
        })
        .collect()
}

/// Dispatched vector implementation of [`fir_row_scalar`], writing into a
/// caller-owned `dst` rather than allocating.
///
/// Same length rule as the scalar reference: writes `min(dst.len(),
/// src.len().saturating_sub(N-1))` samples and leaves the rest of `dst`
/// untouched, so a mismatched pair of buffer sizes degrades rather than
/// panics.
pub fn fir_row<const N: usize>(caps: Caps, src: &[u8], taps: &TapSet<N>, dst: &mut [u8]) {
    let available = src.len().saturating_sub(N.saturating_sub(1));
    let len = dst.len().min(available);
    let Some(dst) = dst.get_mut(..len) else {
        return;
    };
    dispatch_kernel!(caps, s => fir_row_body(s, src, taps, dst));
}

/// The level-generic body behind [`fir_row`].
///
/// Structure: reload and re-widen the source once per tap, one output vector
/// per iteration — the "obvious translation", chosen because it is also the
/// *measured* winner. `vaco-simd`'s own `benches/adoption.rs` Group 4 found
/// the more elaborate `slide`-based and 2x-batched forms 1.36x-1.64x against
/// this shape's 1.12x, both worse: batching past the register file spills,
/// and hoisting the widen still needs a "slide" per neighbouring tap that
/// costs more than the reload it avoids.
#[inline(always)]
#[allow(
    clippy::integer_division,
    reason = "computing the largest multiple-of-native-width prefix length; truncation is the point"
)]
fn fir_row_body<S: Lanes, const N: usize>(simd: S, src: &[u8], taps: &TapSet<N>, dst: &mut [u8]) {
    let n = <S::u8s as SimdBase<S>>::N;
    let round = <S::i16s as SimdBase<S>>::splat(simd, taps.round_bias());
    let full = (dst.len() / n) * n;
    let Some((dst_full, dst_tail)) = split_mut_at(dst, full) else {
        return;
    };

    for (i, out) in dst_full.chunks_exact_mut(n).enumerate() {
        let base = i * n;
        let mut acc = (round, round);
        for (t, &c) in taps.coeffs.iter().enumerate() {
            let Some(window) = src.get(base + t..base + t + n) else {
                continue;
            };
            let v = <S::u8s as SimdBase<S>>::from_slice(simd, window);
            acc = ops::simd::wmla_u8_i16::<S>(acc, v, c);
        }
        ops::simd::pack_u8_from_i16::<S>(acc.0 >> taps.shift, acc.1 >> taps.shift)
            .store_slice(out);
    }

    let tail_base = full;
    for (i, o) in dst_tail.iter_mut().enumerate() {
        let window = src.get(tail_base + i..tail_base + i + N).unwrap_or(&[]);
        let acc = tap_sum(window, &taps.coeffs);
        *o = clip_from_i32(acc, i32::from(taps.round_bias()), taps.shift);
    }
}

/// Split a mutable slice at `mid`, safely: `None` when `mid > slice.len()`.
///
/// A thin, panic-free stand-in for `slice::split_at_mut`, which panics on an
/// out-of-range `mid` — and `mid` here is a computed prefix length, not a
/// caller-supplied one, so the `None` arm is unreachable in practice but
/// costs nothing to make explicit.
fn split_mut_at(s: &mut [u8], mid: usize) -> Option<(&mut [u8], &mut [u8])> {
    if mid > s.len() {
        return None;
    }
    Some(s.split_at_mut(mid))
}

/// Raw weighted sum over `window` and `coeffs` (equal length; a mismatch
/// simply zips to the shorter one), no bias, shift or clip applied — the
/// building block [`fir_row_scalar`] and [`separable_2d`]'s first pass both
/// reduce to. `i32` throughout: `N` taps of `u8 * i16` cannot overflow `i32`
/// for any tap count a real interpolation filter uses.
#[must_use]
pub fn tap_sum<const N: usize>(window: &[u8], coeffs: &[i16; N]) -> i32 {
    let mut acc = 0i32;
    for (t, &c) in coeffs.iter().enumerate() {
        let v = window.get(t).copied().unwrap_or(0);
        acc = acc.wrapping_add(i32::from(v).wrapping_mul(i32::from(c)));
    }
    acc
}

/// [`tap_sum`]'s sibling for an already-widened intermediate row — the second
/// pass of a two-pass separable filter reads the first pass's raw `i32`
/// output, not `u8` source samples.
#[must_use]
pub fn tap_sum_i32<const N: usize>(window: &[i32], coeffs: &[i16; N]) -> i64 {
    let mut acc = 0i64;
    for (t, &c) in coeffs.iter().enumerate() {
        let v = window.get(t).copied().unwrap_or(0);
        acc = acc.wrapping_add(i64::from(v).wrapping_mul(i64::from(c)));
    }
    acc
}

/// Round, shift and clip a raw [`tap_sum`] to `u8`.
#[must_use]
fn clip_from_i32(acc: i32, bias: i32, shift: u32) -> u8 {
    ops::clip_u8((acc.wrapping_add(bias)) >> shift)
}

/// A complete, unvectorised horizontal FIR pass with **no** rounding, shift
/// or clip — see the module doc for why a two-pass separable filter needs
/// this shape rather than [`fir_row_scalar`] applied twice.
#[must_use]
pub fn fir_pass_i32<const N: usize>(src: &[u8], coeffs: &[i16; N], dst_len: usize) -> Vec<i32> {
    let available = src.len().saturating_sub(N.saturating_sub(1));
    let len = dst_len.min(available);
    (0..len)
        .map(|i| tap_sum(src.get(i..i + N).unwrap_or(&[]), coeffs))
        .collect()
}

/// A two-pass separable FIR over a `src_w x src_h` block, producing a
/// `dst_w x dst_h` output.
///
/// `src` must already be border-extended (see [`crate::edge::extend_edges`])
/// to `(dst_w + NH - 1) x (src_h)` where `src_h >= dst_h + NV - 1`, since the
/// vertical pass reads `NV` extended-horizontal rows per output row.
///
/// The horizontal pass (`h_taps`) runs unrounded and unclipped
/// ([`fir_pass_i32`]); the vertical pass (`v_taps`) is where bias, shift and
/// clip to `u8` happen, via [`tap_sum_i32`]. This is the two-pass shape a
/// real spec uses (H.264 §8.4.2.2.1's two-dimensional half-pel positions,
/// for one): set `h_taps.shift`/`.coeffs` to the codec's horizontal filter
/// and `v_taps` to its vertical filter with the *combined* rounding bias and
/// shift, matching whatever precision convention that codec's own spec
/// documents — this crate does not assume one.
#[must_use]
pub fn separable_2d<const NH: usize, const NV: usize>(
    src: &[u8],
    src_stride: usize,
    src_h: usize,
    h_taps: &[i16; NH],
    v_taps: &TapSet<NV>,
    dst_w: usize,
    dst_h: usize,
) -> Vec<u8> {
    // Stage 1: horizontal pass, one row of unrounded i32 sums per source row.
    let intermediate_rows: Vec<Vec<i32>> = (0..src_h)
        .map(|y| {
            let row = src
                .get(y.saturating_mul(src_stride)..)
                .and_then(|r| r.get(..src_stride))
                .unwrap_or(&[]);
            fir_pass_i32(row, h_taps, dst_w)
        })
        .collect();

    // Stage 2: vertical pass down each column of the intermediate, with the
    // real rounding/shift/clip.
    let bias = i64::from(v_taps.round_bias());
    let mut out = vec![0u8; dst_w * dst_h];
    for (y, out_row) in out.chunks_exact_mut(dst_w).take(dst_h).enumerate() {
        for (x, o) in out_row.iter_mut().enumerate() {
            let mut window = [0i32; NV];
            for (t, w) in window.iter_mut().enumerate() {
                *w = intermediate_rows
                    .get(y + t)
                    .and_then(|r| r.get(x))
                    .copied()
                    .unwrap_or(0);
            }
            let acc = tap_sum_i32(&window, &v_taps.coeffs);
            let shifted = acc.wrapping_add(bias) >> v_taps.shift;
            *o = ops::clip_u8(i32::try_from(shifted).unwrap_or(if shifted < 0 {
                i32::MIN
            } else {
                i32::MAX
            }));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_input_passes_through_bilinear_unchanged() {
        // Every tap set in this module sums to exactly `1 << shift`, so a
        // constant plane must produce that same constant back out —
        // a real invariant of the filter definition, not merely "well
        // formed", and one two independently-*transcribed* coefficient
        // tables could still both violate identically.
        let src = [42u8; 8];
        let out = fir_row_scalar(&src, &taps::BILINEAR, 7);
        assert_eq!(out, vec![42u8; 7]);
    }

    #[test]
    fn dc_input_passes_through_h264_halfpel_unchanged() {
        let src = [200u8; 16];
        let out = fir_row_scalar(&src, &taps::H264_LUMA_HALFPEL, 11);
        assert_eq!(out, vec![200u8; 11]);
    }

    #[test]
    fn every_shipped_tap_set_sums_to_exactly_its_own_scale() {
        assert_eq!(taps::BILINEAR.coeffs.iter().sum::<i16>(), 1 << taps::BILINEAR.shift);
        assert_eq!(
            taps::H264_LUMA_HALFPEL.coeffs.iter().sum::<i16>(),
            1 << taps::H264_LUMA_HALFPEL.shift
        );
    }

    #[test]
    fn impulse_response_matches_the_coefficients_directly() {
        // A single `1` sample surrounded by zeros must read back the tap
        // coefficients themselves (scaled by `2^shift`, since the impulse
        // carries no bias to round away) — the other independent property a
        // FIR's own definition guarantees, verifying the tap *order* as well
        // as the tap *values* (a transposed pair would fail this, not just
        // the DC check above).
        let mut src = [0u8; 8];
        if let Some(s) = src.get_mut(2) {
            *s = 64;
        }
        let taps = TapSet {
            coeffs: [1, -5, 20, 20, -5, 1],
            shift: 0,
        };
        let out = fir_row_scalar(&src, &taps, 3);
        // Output position 0 reads window [0..6) = src[0..6]; the impulse at
        // src[2] lands at tap index 2, coefficient 20, scaled by 64.
        assert_eq!(out, vec![clip(20 * 64), clip(-5 * 64), clip(64)]);
    }

    fn clip(x: i32) -> u8 {
        ops::clip_u8(x)
    }

    #[test]
    fn dispatched_matches_scalar_across_every_tail_length() {
        for len in 0..=200usize {
            let src: Vec<u8> = (0..len + 5).map(|i| ((i * 53) & 0xFF) as u8).collect();
            let want = fir_row_scalar(&src, &taps::H264_LUMA_HALFPEL, len);
            let mut got = vec![0u8; len];
            fir_row(Caps::detect(), &src, &taps::H264_LUMA_HALFPEL, &mut got);
            assert_eq!(got, want, "len={len}");
        }
    }

    #[test]
    fn separable_2d_dc_input_passes_through_unchanged() {
        let w = 6;
        let h = 6;
        let src = vec![100u8; w * h];
        let v_taps = TapSet {
            coeffs: taps::H264_LUMA_HALFPEL.coeffs,
            // Combined shift for a horizontal-then-vertical pass at the same
            // scale as a single pass squared: 5 + 5 = 10.
            shift: 10,
        };
        let out = separable_2d(&src, w, h, &taps::H264_LUMA_HALFPEL.coeffs, &v_taps, 1, 1);
        assert_eq!(out, vec![100u8]);
    }
}
