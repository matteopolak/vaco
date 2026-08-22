//! The vectorised `f32` Stockham stage.
//!
//! # What is vectorised, and why that is the whole design
//!
//! Plan 17 §C.6 names two levers. Both are here, and the second one falls out of
//! the first rather than needing separate work:
//!
//! **Split-complex layout.** `re` and `im` are separate arrays everywhere inside
//! the crate, so a complex multiply is `(ar·wr − ai·wi, ar·wi + ai·wr)` —
//! four multiplies, two adds, and *zero shuffles*, at any lane width on any
//! architecture. The interleaved `[re, im, …]` the public API takes is converted
//! once on the way in and once on the way out: `O(n)` against `O(n log n)`.
//!
//! **Vectorising across sub-transforms.** A Stockham stage runs `s·m`
//! independent radix-`R` butterflies, indexed by `q + s·p`. Taking `lanes`
//! consecutive `q` at a time gives contiguous loads, contiguous stores and a
//! *broadcast* twiddle, because every lane in the vector shares the same `p`.
//! There is no permute anywhere in this file.
//!
//! # The stages this does not take
//!
//! The condition is `s ≥ lanes`. `s` starts at 1 and multiplies by the radix
//! each stage, so with the planner emitting the largest radix first (see
//! [`crate::factor::smooth_radices`]) exactly **one** stage — the first — falls
//! below the vector width for every realistic length and lane count. That is
//! this crate's version of plan 17 §C.6.3's "last stages" question, and it lands
//! at the *start* of a Stockham flow rather than the end. The measurement is in
//! `docs/signal/vaco-tx.md`.
//!
//! # Why there is no separate vector butterfly
//!
//! [`Vf32`] implements [`Lane`], so [`crate::butterfly`]'s kernels monomorphise
//! over it directly. The vectorised radix-8 butterfly and the scalar one are the
//! same source text. What the differential tests are really checking, then, is
//! the load/store indexing in this file — which is where the risk actually is.

use core::marker::PhantomData;

use crate::butterfly::{KConst, kernels};
use crate::num::{Lane, StageView};
use vaco_simd::prelude::*;
use vaco_simd::{Caps, dispatch_kernel};

/// A native-width `f32` vector, wearing [`Lane`].
///
/// A newtype rather than an impl on the substrate's own vector types, because
/// `S::f32s` is an associated type and a blanket impl over it would overlap the
/// scalar impls as far as coherence is concerned.
pub(crate) struct Vf32<S, V> {
    v: V,
    _s: PhantomData<fn() -> S>,
}

impl<S, V: Copy> Clone for Vf32<S, V> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S, V: Copy> Copy for Vf32<S, V> {}

impl<S: Lanes, V: SimdFloat<S, Element = f32>> Vf32<S, V> {
    #[inline(always)]
    fn wrap(v: V) -> Self {
        Self {
            v,
            _s: PhantomData,
        }
    }
    #[inline(always)]
    fn load(simd: S, src: &[f32]) -> Self {
        Self::wrap(V::from_slice(simd, src))
    }
    #[inline(always)]
    fn store(self, dst: &mut [f32]) {
        self.v.store_slice(dst);
    }
}

impl<S: Lanes, V: SimdFloat<S, Element = f32>> Lane for Vf32<S, V> {
    type Const = f32;
    type Acc = Self;

    const STAGE_SCALED: bool = false;

    #[inline(always)]
    fn add(a: Self, b: Self) -> Self {
        Self::wrap(a.v + b.v)
    }
    #[inline(always)]
    fn sub(a: Self, b: Self) -> Self {
        Self::wrap(a.v - b.v)
    }
    #[inline(always)]
    fn neg(a: Self) -> Self {
        Self::wrap(-a.v)
    }
    #[inline(always)]
    fn mul_c(a: Self, c: f32) -> Self {
        Self::wrap(a.v * c)
    }
    #[inline(always)]
    fn div_radix(a: Self, _r: u32) -> Self {
        a
    }
    #[inline(always)]
    fn acc_of(x: Self) -> Self {
        x
    }
    #[inline(always)]
    fn acc_mul(x: Self, c: f32) -> Self {
        Self::wrap(x.v * c)
    }
    #[inline(always)]
    fn acc_neg(a: Self) -> Self {
        Self::wrap(-a.v)
    }
    // Plain multiply-then-add rather than `mul_add`. FMA would be permitted by
    // the crate's float reproducibility class, but keeping the scalar and vector
    // paths on the same rounding makes the differential tests exact-match rather
    // than tolerance-match, which is a far sharper instrument.
    #[inline(always)]
    fn acc_mul_add(acc: Self, x: Self, c: f32) -> Self {
        Self::wrap(acc.v + x.v * c)
    }
    #[inline(always)]
    fn acc_mul_sub(acc: Self, x: Self, c: f32) -> Self {
        Self::wrap(acc.v - x.v * c)
    }
    #[inline(always)]
    fn acc_sum(a: Self, b: Self) -> Self {
        Self::wrap(a.v + b.v)
    }
    #[inline(always)]
    fn acc_diff(a: Self, b: Self) -> Self {
        Self::wrap(a.v - b.v)
    }
    #[inline(always)]
    fn acc_finish(a: Self) -> Self {
        a
    }
    #[inline(always)]
    fn cmul_c(ar: Self, ai: Self, wr: f32, wi: f32) -> (Self, Self) {
        (
            Self::wrap(ar.v * wr - ai.v * wi),
            Self::wrap(ar.v * wi + ai.v * wr),
        )
    }
}

/// Generate one vectorised stage pass per radix.
///
/// Identical in structure to `engine::stockham`'s scalar macro; the only
/// differences are that the innermost index steps by `lanes` and that loads and
/// stores move a vector instead of an element.
macro_rules! vector_pass {
    ($name:ident, $r:literal, $kernel:path) => {
        #[inline(always)]
        #[allow(
            clippy::indexing_slicing,
            reason = "identical bounds to the scalar pass, plus `s % lanes == 0` checked by the caller, so every vector slice is fully inside the buffer"
        )]
        fn $name<S: Lanes>(
            simd: S,
            v: &StageView<'_, f32>,
            sr: &[f32],
            si: &[f32],
            dr: &mut [f32],
            di: &mut [f32],
        ) {
            const R: usize = $r;
            type L<S> = Vf32<S, <S as Lanes>::f32s>;
            let lanes = <S::f32s as SimdBase<S>>::N;
            let (m, s) = (v.m, v.s);
            let leg = s * m;
            let k = KConst {
                cos: v.cos,
                sin: v.sin,
                c8: v.c8,
            };
            for p in 0..m {
                let tb = p * (R - 1);
                let mut q = 0usize;
                while q < s {
                    let base = q + s * p;
                    let mut ar: [L<S>; R] = core::array::from_fn(|j| {
                        let lo = base + j * leg;
                        Vf32::load(simd, &sr[lo..lo + lanes])
                    });
                    let mut ai: [L<S>; R] = core::array::from_fn(|j| {
                        let lo = base + j * leg;
                        Vf32::load(simd, &si[lo..lo + lanes])
                    });
                    $kernel(k, &mut ar, &mut ai);
                    let ob = q + s * R * p;
                    ar[0].store(&mut dr[ob..ob + lanes]);
                    ai[0].store(&mut di[ob..ob + lanes]);
                    for kk in 1..R {
                        let (x, y) = Lane::cmul_c(ar[kk], ai[kk], v.tw_re[tb + kk - 1], v.tw_im[tb + kk - 1]);
                        let o = ob + s * kk;
                        x.store(&mut dr[o..o + lanes]);
                        y.store(&mut di[o..o + lanes]);
                    }
                    q += lanes;
                }
            }
        }
    };
}

vector_pass!(vpass2, 2, kernels::k2);
vector_pass!(vpass3, 3, kernels::k3);
vector_pass!(vpass4, 4, kernels::k4);
vector_pass!(vpass5, 5, kernels::k5);
vector_pass!(vpass7, 7, kernels::k7);
vector_pass!(vpass8, 8, kernels::k8);

#[inline(always)]
fn run<S: Lanes>(
    simd: S,
    v: StageView<'_, f32>,
    sr: &[f32],
    si: &[f32],
    dr: &mut [f32],
    di: &mut [f32],
) -> bool {
    let lanes = <S::f32s as SimdBase<S>>::N;
    // Below the vector width the sub-transform axis cannot fill a register and
    // the loads stop being contiguous. The scalar pass takes those stages.
    if v.s < lanes || !v.s.is_multiple_of(lanes) {
        return false;
    }
    match v.radix {
        2 => vpass2(simd, &v, sr, si, dr, di),
        3 => vpass3(simd, &v, sr, si, dr, di),
        4 => vpass4(simd, &v, sr, si, dr, di),
        5 => vpass5(simd, &v, sr, si, dr, di),
        7 => vpass7(simd, &v, sr, si, dr, di),
        8 => vpass8(simd, &v, sr, si, dr, di),
        _ => return false,
    }
    true
}

/// The hook [`crate::num::Arith::simd_stockham_pass`] routes `f32` through.
pub(crate) fn stockham_pass_f32(
    caps: Caps,
    v: StageView<'_, f32>,
    sr: &[f32],
    si: &[f32],
    dr: &mut [f32],
    di: &mut [f32],
) -> bool {
    dispatch_kernel!(caps, simd => run(simd, v, sr, si, dr, di))
}
