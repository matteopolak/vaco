//! Mixed-radix Stockham autosort FFT — the workhorse.
//!
//! # Why Stockham rather than a bit-reversed in-place FFT
//!
//! Three properties, all of which this crate needs:
//!
//! 1. **No permutation pass.** Stockham is self-sorting: input and output are
//!    both in natural order, so there is no digit-reversal scatter to write, to
//!    vectorise, or to get wrong for a mixed-radix length.
//! 2. **Every stage is one radix.** That is what makes the fixed-point contract
//!    statable: "a radix-`r` stage divides by `r`" is a complete description of
//!    the scaling, and the total is exactly `1/n` for every decomposition. A
//!    split-radix flow graph, whose sub-blocks have different depths, has no
//!    such uniform rule — see `docs/signal/vaco-tx.md`, "What was deferred".
//! 3. **The inner loop is a batch of independent sub-transforms.** Plan 17
//!    §C.6.2's "vectorise across sub-transforms, not within butterflies" is not
//!    a transformation we apply to this loop; it *is* this loop.
//!
//! # The recurrence
//!
//! With `n_cur` the remaining transform size, `s` the number of sub-transforms
//! already built (starting at 1) and `m = n_cur / r`:
//!
//! ```text
//!   a[j]  = src[q + s·(p + j·m)]                       j ∈ [0, r)
//!   b     = DFT_r(a)
//!   dst[q + s·(r·p + k)] = b[k] · exp(-2πi·p·k/n_cur)   k ∈ [0, r)
//!   then n_cur ← m,  s ← s·r,  swap src and dst
//! ```
//!
//! `q + s·p` runs over `[0, s·m)` contiguously, so for a fixed leg `j` the input
//! is the contiguous block `[j·s·m, (j+1)·s·m)`. The output is contiguous in
//! runs of `s`. Once `s` reaches the vector width, both sides are unit stride and
//! the twiddle is a broadcast — which is the entire SIMD story for this crate.

use crate::butterfly::{RadixConst, kernels};
use crate::num::{Arith, StageView};
use super::Ctx;

/// One radix pass, with its twiddles in the order the kernel walks them.
#[derive(Debug, Clone)]
pub(crate) struct Stage<T> {
    pub(crate) radix: usize,
    /// `n_cur / radix` — the number of distinct twiddle groups.
    pub(crate) m: usize,
    /// Sub-transforms entering this stage. `1` for the first stage.
    pub(crate) s: usize,
    /// `w^{p·k}` for `p ∈ [0, m)`, `k ∈ [1, radix)`, stage-major, split-complex.
    /// `k = 0` is omitted because its twiddle is always 1.
    pub(crate) tw_re: Vec<T>,
    pub(crate) tw_im: Vec<T>,
    pub(crate) consts: RadixConst<T>,
}

impl<T: Arith> Stage<T> {
    fn view(&self) -> StageView<'_, T> {
        StageView {
            radix: self.radix,
            m: self.m,
            s: self.s,
            tw_re: &self.tw_re,
            tw_im: &self.tw_im,
            cos: &self.consts.cos,
            sin: &self.consts.sin,
            c8: self.consts.c8,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Stockham<T> {
    n: usize,
    stages: Vec<Stage<T>>,
}

impl<T: Arith> Stockham<T> {
    /// Build the stage list and its twiddle tables.
    ///
    /// Total twiddle storage is `≈ n` elements per component regardless of the
    /// radix mix, because `Σ_stages (n_cur/r)·(r-1) = n·(1 - 1/n)`.
    #[allow(
        clippy::integer_division,
        reason = "`radices` is a factorisation of `n`, so every division here is exact"
    )]
    pub(crate) fn new(n: usize, radices: &[usize]) -> Self {
        let mut stages = Vec::new();
        let mut n_cur = n;
        let mut s = 1usize;
        for &r in radices {
            debug_assert!(
                crate::factor::KERNEL_RADICES.contains(&r),
                "no kernel for radix {r}"
            );
            let m = n_cur / r;
            let mut tw_re = Vec::new();
            let mut tw_im = Vec::new();
            for p in 0..m {
                for k in 1..r {
                    // p·k < m·r = n_cur, so the angle needs no reduction.
                    let theta = -core::f64::consts::TAU * (p * k) as f64 / n_cur as f64;
                    let (sn, cs) = theta.sin_cos();
                    tw_re.push(T::from_f64(cs));
                    tw_im.push(T::from_f64(sn));
                }
            }
            stages.push(Stage {
                radix: r,
                m,
                s,
                tw_re,
                tw_im,
                consts: RadixConst::new(r),
            });
            n_cur = m;
            s *= r;
        }
        Self { n, stages }
    }


    /// Two ping-pong buffers, one per component.
    pub(crate) const fn scratch_len(&self) -> usize {
        2 * self.n
    }

    pub(crate) fn radices(&self) -> Vec<u32> {
        self.stages.iter().map(|st| st.radix as u32).collect()
    }

    pub(crate) fn exec(&self, re: &mut [T], im: &mut [T], scratch: &mut [T], ctx: Ctx) {
        debug_assert!(re.len() >= self.n && im.len() >= self.n);
        debug_assert!(scratch.len() >= self.scratch_len());
        let (tmp_re, rest) = scratch.split_at_mut(self.n);
        let (tmp_im, _) = rest.split_at_mut(self.n);

        let mut in_tmp = false;
        for st in &self.stages {
            if in_tmp {
                pass(st, tmp_re, tmp_im, re, im, ctx);
            } else {
                pass(st, re, im, tmp_re, tmp_im, ctx);
            }
            in_tmp = !in_tmp;
        }
        if in_tmp {
            super::copy_prefix(re, tmp_re, self.n);
            super::copy_prefix(im, tmp_im, self.n);
        }
    }
}

/// Generate one scalar stage pass per radix.
///
/// A macro rather than a const-generic function because the butterflies take
/// concrete `[L; 2]`, `[L; 4]`, … arrays: with `[L; R]` and a literal index,
/// `re[7]` would be a post-monomorphisation error for `R = 2` even in a `match`
/// arm that can never run.
macro_rules! scalar_pass {
    ($name:ident, $r:literal, $kernel:path) => {
        #[inline(always)]
        #[allow(
            clippy::indexing_slicing,
            reason = "every index is `q + s·(p + j·m)` or `q + s·(r·p + k)` with q<s, p<m, j,k<r, and s·m·r = n_cur ≤ buffer length — established by `Stockham::new`'s factorisation and asserted in debug"
        )]
        fn $name<T: Arith>(st: &Stage<T>, sr: &[T], si: &[T], dr: &mut [T], di: &mut [T]) {
            const R: usize = $r;
            let (m, s) = (st.m, st.s);
            let leg = s * m;
            for p in 0..m {
                let tb = p * (R - 1);
                for q in 0..s {
                    let base = q + s * p;
                    let mut ar: [T; R] = core::array::from_fn(|j| sr[base + j * leg]);
                    let mut ai: [T; R] = core::array::from_fn(|j| si[base + j * leg]);
                    if T::STAGE_SCALED {
                        for j in 0..R {
                            ar[j] = T::div_radix(ar[j], R as u32);
                            ai[j] = T::div_radix(ai[j], R as u32);
                        }
                    }
                    $kernel(st.consts.borrow(), &mut ar, &mut ai);
                    let ob = q + s * R * p;
                    dr[ob] = ar[0];
                    di[ob] = ai[0];
                    for k in 1..R {
                        let (x, y) =
                            T::cmul_c(ar[k], ai[k], st.tw_re[tb + k - 1], st.tw_im[tb + k - 1]);
                        dr[ob + s * k] = x;
                        di[ob + s * k] = y;
                    }
                }
            }
        }
    };
}

scalar_pass!(pass2, 2, kernels::k2);
scalar_pass!(pass3, 3, kernels::k3);
scalar_pass!(pass4, 4, kernels::k4);
scalar_pass!(pass5, 5, kernels::k5);
scalar_pass!(pass7, 7, kernels::k7);
scalar_pass!(pass8, 8, kernels::k8);

/// Run one stage: vectorised where the precision and the stride allow, scalar
/// otherwise. The scalar path is always compiled and is the reference the
/// vector path is differentially tested against.
fn pass<T: Arith>(st: &Stage<T>, sr: &[T], si: &[T], dr: &mut [T], di: &mut [T], ctx: Ctx) {
    if !ctx.scalar && T::simd_stockham_pass(ctx.caps, st.view(), sr, si, dr, di) {
        return;
    }
    match st.radix {
        2 => pass2(st, sr, si, dr, di),
        3 => pass3(st, sr, si, dr, di),
        4 => pass4(st, sr, si, dr, di),
        5 => pass5(st, sr, si, dr, di),
        7 => pass7(st, sr, si, dr, di),
        8 => pass8(st, sr, si, dr, di),
        // Unreachable: `Stockham::new` is only ever handed radices from
        // `factor::KERNEL_RADICES`. Leaving the data untouched rather than
        // panicking keeps the crate's no-panic property intact if that ever
        // stops being true; the round-trip tests would fail loudly.
        _ => debug_assert!(false, "unsupported radix {}", st.radix),
    }
}
