//! Small-radix DFT kernels, in split-complex form.
//!
//! Every kernel rewrites `re`/`im` in place with the radix-`R` forward DFT
//! `b[k] = Σ_j a[j] · exp(-2πi·j·k/R)`.
//!
//! # One implementation, two execution widths
//!
//! Kernels are generic over [`Lane`], which is implemented by `f32`, `f64`,
//! `i32` *and* by the SIMD vector wrapper in [`crate::simd`]. So the vectorised
//! `f32` butterfly is not a second implementation differentially tested against
//! the scalar one — it is the same source, monomorphised twice. The differential
//! tests still exist (they catch a wrong *load/store* index, which is where the
//! real risk lives), but the arithmetic cannot drift.
//!
//! # Why the odd radices share one kernel
//!
//! Plan 17 §C.3.1 asks for hardcoded straight-line butterflies per radix. The
//! power-of-two radices get exactly that ([`bf2`], [`bf4`], [`bf8`]): they are
//! the hot ones and are almost multiplication-free. The odd radices share
//! [`bf_odd`], which is straight-line after monomorphisation — `R` is a const
//! parameter, so both loops unroll fully — and has one implementation to test
//! rather than three. Its operation count is the symmetric-sum form a
//! hand-written radix-5 would use anyway.
//!
//! # Fixed point
//!
//! Nothing here rounds except through [`Lane`]. The caller divides the kernel's
//! inputs by `R` first (see [`crate::engine::stockham`]), which is what bounds
//! the accumulator: with `|a| ≤ 2^31/R` the widest partial sum in [`bf_odd`]
//! reaches `≈ 2^62.9`, inside `i64` — and every operation saturates regardless.

use crate::num::Lane;

/// Radix-2. `b0 = a0 + a1`, `b1 = a0 - a1`. No multiplies.
#[inline(always)]
pub(crate) fn bf2<L: Lane>(re: &mut [L; 2], im: &mut [L; 2]) {
    let (r0, r1) = (L::add(re[0], re[1]), L::sub(re[0], re[1]));
    let (i0, i1) = (L::add(im[0], im[1]), L::sub(im[0], im[1]));
    *re = [r0, r1];
    *im = [i0, i1];
}

/// Radix-4. Multiplication-free: the only twiddle is `-i`, a swap and a negate.
#[inline(always)]
pub(crate) fn bf4<L: Lane>(re: &mut [L; 4], im: &mut [L; 4]) {
    let t0r = L::add(re[0], re[2]);
    let t0i = L::add(im[0], im[2]);
    let t1r = L::sub(re[0], re[2]);
    let t1i = L::sub(im[0], im[2]);
    let t2r = L::add(re[1], re[3]);
    let t2i = L::add(im[1], im[3]);
    let t3r = L::sub(re[1], re[3]);
    let t3i = L::sub(im[1], im[3]);

    *re = [
        L::add(t0r, t2r),
        L::add(t1r, t3i),
        L::sub(t0r, t2r),
        L::sub(t1r, t3i),
    ];
    *im = [
        L::add(t0i, t2i),
        L::sub(t1i, t3r),
        L::sub(t0i, t2i),
        L::add(t1i, t3r),
    ];
}

/// Radix-8, as an even/odd split feeding two radix-4 kernels.
///
/// `c` is `√½` in the target representation, quantised once at plan time so the
/// fixed-point constant is identical in every plan of every length.
#[inline(always)]
#[allow(
    clippy::indexing_slicing,
    reason = "the arrays are `[L; 8]` and `[L; 4]` with concrete lengths; j < 4 and t < 4 by the loop bounds, so every index is in range and the checks fold away"
)]
pub(crate) fn bf8<L: Lane>(re: &mut [L; 8], im: &mut [L; 8], c: L::Const) {
    // b[2t]   = DFT4(e)[t]           e_j = a_j + a_{j+4}
    // b[2t+1] = DFT4(o · w^j)[t]     o_j = a_j - a_{j+4},  w = exp(-2πi/8)
    let mut er: [L; 4] = core::array::from_fn(|j| L::add(re[j], re[j + 4]));
    let mut ei: [L; 4] = core::array::from_fn(|j| L::add(im[j], im[j + 4]));
    let mut or: [L; 4] = core::array::from_fn(|j| L::sub(re[j], re[j + 4]));
    let mut oi: [L; 4] = core::array::from_fn(|j| L::sub(im[j], im[j + 4]));

    // o1 ·= w   = √½·(1 - i)
    let (o1r, o1i) = (or[1], oi[1]);
    or[1] = L::mul_c(L::add(o1r, o1i), c);
    oi[1] = L::mul_c(L::sub(o1i, o1r), c);
    // o2 ·= w^2 = -i
    let o2r = or[2];
    or[2] = oi[2];
    oi[2] = L::neg(o2r);
    // o3 ·= w^3 = -√½·(1 + i)
    let (o3r, o3i) = (or[3], oi[3]);
    or[3] = L::mul_c(L::sub(o3i, o3r), c);
    oi[3] = L::neg(L::mul_c(L::add(o3r, o3i), c));

    bf4(&mut er, &mut ei);
    bf4(&mut or, &mut oi);

    for t in 0..4 {
        re[2 * t] = er[t];
        im[2 * t] = ei[t];
        re[2 * t + 1] = or[t];
        im[2 * t + 1] = oi[t];
    }
}

/// Generic odd-radix DFT by symmetric sums.
///
/// `cos_tab[(k-1)·h + (j-1)] = cos(2πjk/R)`, likewise `sin_tab`, `h = (R-1)/2`.
/// Derived from
/// `a_j·e^{-iθ} + a_{R-j}·e^{+iθ} = (a_j + a_{R-j})·cos θ − i·(a_j − a_{R-j})·sin θ`,
/// which halves both the multiply count and the table.
#[inline(always)]
#[allow(
    clippy::indexing_slicing,
    reason = "R is a const parameter; every index is < R by construction (j ≤ h < R, R-j ≥ 1) or < h·h, the caller's table size. The checks fold away after monomorphisation."
)]
#[allow(
    clippy::integer_division,
    reason = "the divisor is the const parameter R, never untrusted input"
)]
pub(crate) fn bf_odd<L: Lane, const R: usize>(
    re: &mut [L; R],
    im: &mut [L; R],
    cos_tab: &[L::Const],
    sin_tab: &[L::Const],
) {
    let h = (R - 1) / 2;
    debug_assert!(R >= 3 && R % 2 == 1);
    debug_assert!(cos_tab.len() >= h * h && sin_tab.len() >= h * h);

    // S_j = a_j + a_{R-j}, D_j = a_j - a_{R-j}. Exact: no rounding, and after
    // the caller's divide-by-R the magnitudes cannot leave the sample range.
    // Index 0 is unused and holds a_0 so `from_fn` has something to return.
    let sr: [L; R] = core::array::from_fn(|j| {
        if j == 0 {
            re[0]
        } else {
            L::add(re[j], re[R - j])
        }
    });
    let si: [L; R] = core::array::from_fn(|j| {
        if j == 0 {
            im[0]
        } else {
            L::add(im[j], im[R - j])
        }
    });
    let dr: [L; R] = core::array::from_fn(|j| {
        if j == 0 {
            re[0]
        } else {
            L::sub(re[j], re[R - j])
        }
    });
    let di: [L; R] = core::array::from_fn(|j| {
        if j == 0 {
            im[0]
        } else {
            L::sub(im[j], im[R - j])
        }
    });

    let (a0r, a0i) = (re[0], im[0]);
    let mut zr = a0r;
    let mut zi = a0i;
    for j in 1..=h {
        zr = L::add(zr, sr[j]);
        zi = L::add(zi, si[j]);
    }

    for k in 1..=h {
        // j = 1 seeds the chains, so no accumulator ever needs a typed zero —
        // which matters because a vector zero cannot be produced without a
        // capability token.
        let c1 = cos_tab[(k - 1) * h];
        let s1 = sin_tab[(k - 1) * h];
        let mut cre = L::acc_mul_add(L::acc_of(a0r), sr[1], c1);
        let mut cim = L::acc_mul_add(L::acc_of(a0i), si[1], c1);
        let mut sre = L::acc_mul(di[1], s1);
        let mut sim = L::acc_neg(L::acc_mul(dr[1], s1));
        for j in 2..=h {
            let c = cos_tab[(k - 1) * h + (j - 1)];
            let s = sin_tab[(k - 1) * h + (j - 1)];
            cre = L::acc_mul_add(cre, sr[j], c);
            cim = L::acc_mul_add(cim, si[j], c);
            sre = L::acc_mul_add(sre, di[j], s);
            sim = L::acc_mul_sub(sim, dr[j], s);
        }
        re[k] = L::acc_finish(L::acc_sum(cre, sre));
        im[k] = L::acc_finish(L::acc_sum(cim, sim));
        re[R - k] = L::acc_finish(L::acc_diff(cre, sre));
        im[R - k] = L::acc_finish(L::acc_diff(cim, sim));
    }

    re[0] = zr;
    im[0] = zi;
}

/// The per-radix constants a stage needs, quantised once at plan time.
///
/// Shared verbatim between the scalar and vector paths: `L::Const` is the
/// element type in both cases, so there is exactly one table per radix and no
/// possibility of the two paths holding different constants.
#[derive(Debug, Clone)]
pub(crate) struct RadixConst<C> {
    /// `cos(2πjk/R)`, `h·h` entries; empty for radix 2, 4 and 8.
    pub cos: Vec<C>,
    /// `sin(2πjk/R)`, `h·h` entries; empty for radix 2, 4 and 8.
    pub sin: Vec<C>,
    /// `√½`. Used only by radix 8.
    pub c8: C,
}

/// A borrowed view of [`RadixConst`], so a kernel can be called from the vector
/// path without owning a `Vec`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KConst<'a, C> {
    pub cos: &'a [C],
    pub sin: &'a [C],
    pub c8: C,
}

impl<C: Copy> RadixConst<C> {
    pub(crate) fn borrow(&self) -> KConst<'_, C> {
        KConst {
            cos: &self.cos,
            sin: &self.sin,
            c8: self.c8,
        }
    }
}

impl<T: crate::num::Arith> RadixConst<T> {
    #[allow(
        clippy::integer_division,
        reason = "radix is one of the fixed kernel-set values, never untrusted input"
    )]
    pub(crate) fn new(radix: usize) -> Self {
        let mut cos = Vec::new();
        let mut sin = Vec::new();
        if radix % 2 == 1 && radix > 1 {
            let h = (radix - 1) / 2;
            for k in 1..=h {
                for j in 1..=h {
                    let theta = core::f64::consts::TAU * (j as f64) * (k as f64) / (radix as f64);
                    cos.push(T::from_f64(theta.cos()));
                    sin.push(T::from_f64(theta.sin()));
                }
            }
        }
        Self {
            cos,
            sin,
            c8: T::from_f64(core::f64::consts::FRAC_1_SQRT_2),
        }
    }
}

/// Uniform per-radix wrappers.
///
/// Every one has the shape `fn(&RadixConst<L::Const>, &mut [L; R], &mut [L; R])`,
/// which is what lets [`crate::engine::stockham`] generate one stage pass per
/// radix from a single macro. Array lengths are concrete literals — never a
/// const parameter on the array itself — so there is no index that is in bounds
/// for one monomorphisation and out of bounds for another.
pub(crate) mod kernels {
    use super::{KConst, Lane, bf_odd, bf2, bf4, bf8};

    #[inline(always)]
    pub(crate) fn k2<L: Lane>(_k: KConst<'_, L::Const>, re: &mut [L; 2], im: &mut [L; 2]) {
        bf2(re, im);
    }
    #[inline(always)]
    pub(crate) fn k3<L: Lane>(k: KConst<'_, L::Const>, re: &mut [L; 3], im: &mut [L; 3]) {
        bf_odd::<L, 3>(re, im, k.cos, k.sin);
    }
    #[inline(always)]
    pub(crate) fn k4<L: Lane>(_k: KConst<'_, L::Const>, re: &mut [L; 4], im: &mut [L; 4]) {
        bf4(re, im);
    }
    #[inline(always)]
    pub(crate) fn k5<L: Lane>(k: KConst<'_, L::Const>, re: &mut [L; 5], im: &mut [L; 5]) {
        bf_odd::<L, 5>(re, im, k.cos, k.sin);
    }
    #[inline(always)]
    pub(crate) fn k7<L: Lane>(k: KConst<'_, L::Const>, re: &mut [L; 7], im: &mut [L; 7]) {
        bf_odd::<L, 7>(re, im, k.cos, k.sin);
    }
    #[inline(always)]
    pub(crate) fn k8<L: Lane>(k: KConst<'_, L::Const>, re: &mut [L; 8], im: &mut [L; 8]) {
        bf8(re, im, k.c8);
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Directly evaluated `R`-point DFT in f64 — the oracle for every kernel.
    fn naive(re: &[f64], im: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let n = re.len();
        let mut out_r = vec![0.0; n];
        let mut out_i = vec![0.0; n];
        for k in 0..n {
            let (mut sr, mut si) = (0.0, 0.0);
            for j in 0..n {
                let th = -core::f64::consts::TAU * ((j * k) % n) as f64 / n as f64;
                let (s, c) = th.sin_cos();
                sr += re[j] * c - im[j] * s;
                si += re[j] * s + im[j] * c;
            }
            out_r[k] = sr;
            out_i[k] = si;
        }
        (out_r, out_i)
    }

    fn sample(n: usize) -> (Vec<f64>, Vec<f64>) {
        (
            (0..n)
                .map(|j| ((j * 37 % 19) as f64) / 19.0 - 0.5)
                .collect(),
            (0..n)
                .map(|j| ((j * 53 % 23) as f64) / 23.0 - 0.5)
                .collect(),
        )
    }

    macro_rules! check {
        ($n:literal, $f:path) => {{
            let (re, im) = sample($n);
            let (er, ei) = naive(&re, &im);
            let mut ar: [f64; $n] = re.try_into().unwrap();
            let mut ai: [f64; $n] = im.try_into().unwrap();
            let k = RadixConst::<f64>::new($n);
            $f(k.borrow(), &mut ar, &mut ai);
            for j in 0..$n {
                assert!(
                    (ar[j] - er[j]).abs() < 1e-12 && (ai[j] - ei[j]).abs() < 1e-12,
                    "radix {} lane {j}: got ({}, {}) want ({}, {})",
                    $n,
                    ar[j],
                    ai[j],
                    er[j],
                    ei[j]
                );
            }
        }};
    }

    #[test]
    fn every_kernel_matches_a_direct_dft() {
        check!(2, kernels::k2::<f64>);
        check!(3, kernels::k3::<f64>);
        check!(4, kernels::k4::<f64>);
        check!(5, kernels::k5::<f64>);
        check!(7, kernels::k7::<f64>);
        check!(8, kernels::k8::<f64>);
    }
}
