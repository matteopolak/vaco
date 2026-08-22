//! Bluestein's chirp-z transform — the safety net that makes `Plan::new` total.
//!
//! Using `nk = (n² + k² − (k−n)²)/2`:
//!
//! ```text
//!   X[k] = w[k] · Σ_n (x[n]·w[n]) · v[k−n],   w[j] = e^{-iπj²/N},  v[j] = e^{+iπj²/N}
//! ```
//!
//! The sum is a linear convolution, computed cyclically at length
//! `M = next_pow2(2N−1)` with the kernel wrapped so no aliasing reaches
//! `k ∈ [0, N)`. `M` is always a power of two, so the inner transform is always
//! plain mixed-radix and this rule never recurses.
//!
//! Cost is roughly three power-of-two transforms of length `≈ 2N`. That is the
//! price of "there is no length we cannot transform", and it is worth paying:
//! the alternative is every codec discovering, late, that its bitstream asked
//! for 1381 points.

use super::conv::Conv;
use crate::num::Arith;
use super::Ctx;

#[derive(Debug, Clone)]
pub(crate) struct Bluestein<T: Arith> {
    n: usize,
    m: usize,
    /// `w[j] = e^{-iπj²/N}`, the pre- and post-rotation chirp.
    w_re: Vec<T>,
    w_im: Vec<T>,
    conv: Conv<T>,
}

impl<T: Arith> Bluestein<T> {
    pub(crate) fn new(n: usize) -> Self {
        let m = (2 * n - 1).next_power_of_two();
        let two_n = 2 * n as u64;

        // The angle uses j² reduced mod 2N. Without the reduction, j² for
        // j ≈ 2^24 exceeds f64's exact-integer range and the chirp drifts.
        let chirp = |j: usize, sign: f64| -> (f64, f64) {
            let jj = (j as u64 % two_n) * (j as u64 % two_n) % two_n;
            let theta = sign * core::f64::consts::PI * jj as f64 / n as f64;
            theta.sin_cos()
        };

        let mut w_re = Vec::new();
        let mut w_im = Vec::new();
        for j in 0..n {
            let (s, c) = chirp(j, -1.0);
            w_re.push(T::from_f64(c));
            w_im.push(T::from_f64(s));
        }

        let mut b_re = vec![T::ZERO; m];
        let mut b_im = vec![T::ZERO; m];
        for j in 0..n {
            let (s, c) = chirp(j, 1.0);
            let (vr, vi) = (T::from_f64(c), T::from_f64(s));
            if let Some(slot) = b_re.get_mut(j) {
                *slot = vr;
            }
            if let Some(slot) = b_im.get_mut(j) {
                *slot = vi;
            }
            if j > 0 {
                if let Some(slot) = b_re.get_mut(m - j) {
                    *slot = vr;
                }
                if let Some(slot) = b_im.get_mut(m - j) {
                    *slot = vi;
                }
            }
        }

        Self {
            n,
            m,
            w_re,
            w_im,
            conv: Conv::new(m, &b_re, &b_im, 0),
        }
    }


    pub(crate) fn scratch_len(&self) -> usize {
        2 * self.m + self.conv.scratch_len()
    }

    pub(crate) fn describe(&self) -> crate::Decomposition {
        crate::Decomposition::Bluestein {
            m: self.m,
            inner: Box::new(self.conv.describe()),
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "loops are bounded by n ≤ m and every buffer is length-checked against n and m on entry"
    )]
    pub(crate) fn exec(&self, re: &mut [T], im: &mut [T], scratch: &mut [T], ctx: Ctx) {
        let (n, m) = (self.n, self.m);
        if re.len() < n || im.len() < n || scratch.len() < self.scratch_len() {
            debug_assert!(false, "buffer too small for Bluestein({n})");
            return;
        }
        let (ar, rest) = scratch.split_at_mut(m);
        let (ai, sub) = rest.split_at_mut(m);

        for j in 0..n {
            let (x, y) = T::cmul_c(re[j], im[j], self.w_re[j], self.w_im[j]);
            ar[j] = x;
            ai[j] = y;
        }
        for j in n..m {
            ar[j] = T::ZERO;
            ai[j] = T::ZERO;
        }

        self.conv.exec(ar, ai, sub, ctx, n as u64);

        for k in 0..n {
            let (x, y) = T::cmul_c(ar[k], ai[k], self.w_re[k], self.w_im[k]);
            re[k] = x;
            im[k] = y;
        }
    }
}
