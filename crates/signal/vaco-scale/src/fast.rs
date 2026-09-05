//! The kernel seam: vectorised bodies for the hot inner loops.
//!
//! # The rule this module exists to enforce
//!
//! **A kernel here may only make the generic path faster, never different.**
//! Every entry has a scalar reference in [`crate::exec`] that defines its
//! semantics, and the `kernels_agree` tests at the bottom of this file run both
//! over randomised input and require byte equality. Adding a kernel is therefore purely additive: if it
//! is wrong, a test says so, and if it is missing, the generic path already
//! works.
//!
//! # Authoring
//!
//! The pattern is `vaco-simd`'s (see its `example` module):
//!
//! 1. a scalar reference — here, the one already in `exec`;
//! 2. one `#[inline(always)]` body generic over `S: Lanes`;
//! 3. a dispatching wrapper;
//! 4. a [`KernelSet`] entry resolved once, at plan time.
//!
//! `#[inline(always)]` on step 2 is a correctness-of-codegen requirement, not a
//! tuning knob: it is how the dispatched level's target-feature context reaches
//! the body.
//!
//! # Why the affine row is the first kernel
//!
//! It is the one operation every colour conversion runs on every pixel, it is
//! branch-free, and it is lanewise with no shuffles — the best possible shape
//! for portable SIMD. The filters come second because their cost is dominated by
//! the gather on the source, not by the multiply.

use vaco_simd::prelude::*;
use vaco_simd::{Caps, KernelSet, Tier, dispatch_kernel};

use crate::colour::Affine;
use crate::exec::apply_affine;

/// Signature of the colour-matrix row kernel.
pub type AffineRowFn = fn(&Affine, &mut [i32], &mut [i32], &mut [i32]);

/// The kernels one plan resolves to.
#[derive(Clone, Copy, Debug)]
pub struct ScaleKernels {
    /// Apply a 3×3 affine to three component rows in place.
    pub affine_row: AffineRowFn,
}

impl KernelSet for ScaleKernels {
    fn for_tier(tier: Tier) -> Self {
        Self {
            affine_row: if tier.is_scalar() {
                apply_affine as AffineRowFn
            } else {
                affine_row_dispatched
            },
        }
    }

    fn kernel_names() -> &'static [&'static str] {
        &["affine_row"]
    }
}

impl Default for ScaleKernels {
    fn default() -> Self {
        Self::select()
    }
}

/// Whether the `i32` accumulator can hold every product this transform can
/// produce.
///
/// Not a guess: `Sum |m| x max_sample + |bias|` is the exact bound, so the
/// vector path is selected on a proof rather than on the depth happening to be
/// eight.
#[must_use]
pub fn fits_i32(a: &Affine) -> bool {
    let max = i64::from(a.max);
    a.m.iter().zip(a.bias.iter()).all(|(row, bias)| {
        // `i64::from(c).abs()` rather than `c.abs()`: `i32::MIN` is reachable
        // in a coefficient slot and `i32::MIN.abs()` overflows.
        let sum: i64 = row.iter().map(|c| i64::from(*c).abs()).sum();
        sum.saturating_mul(max)
            .saturating_add(bias.saturating_abs())
            < i64::from(i32::MAX)
    })
}

fn affine_row_dispatched(a: &Affine, r0: &mut [i32], r1: &mut [i32], r2: &mut [i32]) {
    if !fits_i32(a) {
        apply_affine(a, r0, r1, r2);
        return;
    }
    let caps = Caps::detect();
    dispatch_kernel!(caps, simd => affine_row_simd(simd, a, r0, r1, r2));
}

/// One generic body, monomorphised once per CPU level.
///
/// `#[inline(always)]` is a correctness-of-codegen requirement here, not a
/// tuning knob: it is how the dispatched level's target-feature context reaches
/// the body. A kernel that fails to inline compiles at the ambient baseline —
/// still correct, silently slow, and invisible to every test.
#[inline(always)]
#[allow(
    clippy::integer_division,
    clippy::inline_always,
    clippy::many_single_char_names,
    reason = "see above; the divisor is the lane count, a per-level constant"
)]
fn affine_row_simd<S: Lanes>(simd: S, a: &Affine, r0: &mut [i32], r1: &mut [i32], r2: &mut [i32]) {
    let lanes = <S::i32s as SimdBase<S>>::N;
    let len = r0.len().min(r1.len()).min(r2.len());
    let head = (len / lanes) * lanes;
    let (h0, t0) = r0.split_at_mut(head);
    let (h1, t1) = r1.split_at_mut(head);
    let (h2, t2) = r2.split_at_mut(head);

    let m = a.m;
    let b0 = a.bias.first().copied().unwrap_or(0) as i32;
    let b1 = a.bias.get(1).copied().unwrap_or(0) as i32;
    let b2 = a.bias.get(2).copied().unwrap_or(0) as i32;
    let shift = u32::from(a.shift);
    let lo = <S::i32s as SimdBase<S>>::splat(simd, 0);
    let hi = <S::i32s as SimdBase<S>>::splat(simd, a.max);

    let row = |i: usize, j: usize| m.get(i).and_then(|r| r.get(j)).copied().unwrap_or(0);

    for ((c0, c1), c2) in h0
        .chunks_exact_mut(lanes)
        .zip(h1.chunks_exact_mut(lanes))
        .zip(h2.chunks_exact_mut(lanes))
    {
        let x = <S::i32s as SimdBase<S>>::from_slice(simd, c0);
        let y = <S::i32s as SimdBase<S>>::from_slice(simd, c1);
        let z = <S::i32s as SimdBase<S>>::from_slice(simd, c2);

        let o0 = ((x * row(0, 0) + y * row(0, 1) + z * row(0, 2) + b0) >> shift)
            .max(lo)
            .min(hi);
        let o1 = ((x * row(1, 0) + y * row(1, 1) + z * row(1, 2) + b1) >> shift)
            .max(lo)
            .min(hi);
        let o2 = ((x * row(2, 0) + y * row(2, 1) + z * row(2, 2) + b2) >> shift)
            .max(lo)
            .min(hi);

        o0.store_slice(c0);
        o1.store_slice(c1);
        o2.store_slice(c2);
    }

    // The tail goes through the scalar reference, so there is no second edge
    // implementation that could disagree with the first.
    apply_affine(a, t0, t1, t2);
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use super::*;
    use crate::colour;
    use crate::spec::ImageSpec;
    use vaco_color::{ColorInfo, ColorRange, MatrixCoefficients};
    use vaco_pixfmt::PixFmt;

    fn affine() -> Affine {
        let s = ImageSpec {
            format: PixFmt::Yuv444p,
            width: 64,
            height: 1,
            color: ColorInfo {
                range: ColorRange::Limited,
                matrix: MatrixCoefficients::Bt709,
                ..ColorInfo::default()
            },
        };
        let d = ImageSpec {
            format: PixFmt::Rgb24,
            width: 64,
            height: 1,
            color: ColorInfo {
                range: ColorRange::Full,
                ..ColorInfo::default()
            },
        };
        match colour::build(&s, &d, 8) {
            colour::ColorStage::Affine(a) => a,
            colour::ColorStage::None => panic!("expected an affine stage"),
            colour::ColorStage::Float(_) => panic!("expected an affine stage"),
        }
    }

    #[test]
    fn vector_and_scalar_agree_at_every_length() {
        let a = affine();
        for len in 0..97usize {
            let mk = |seed: u32| -> Vec<i32> {
                (0..len)
                    .map(|i| ((i as u32).wrapping_mul(2654435761).wrapping_add(seed) % 256) as i32)
                    .collect()
            };
            let (mut a0, mut a1, mut a2) = (mk(1), mk(2), mk(3));
            let (mut b0, mut b1, mut b2) = (a0.clone(), a1.clone(), a2.clone());
            apply_affine(&a, &mut a0, &mut a1, &mut a2);
            affine_row_dispatched(&a, &mut b0, &mut b1, &mut b2);
            assert_eq!((a0, a1, a2), (b0, b1, b2), "length {len}");
        }
    }

    #[test]
    fn eight_bit_transforms_fit_a_thirty_two_bit_accumulator() {
        assert!(fits_i32(&affine()));
    }
}
