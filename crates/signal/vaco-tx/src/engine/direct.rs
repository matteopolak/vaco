//! Directly evaluated `O(n²)` DFT.
//!
//! Two jobs, and they are different jobs:
//!
//! * **Small awkward lengths, every precision.** For a prime below ~32 this
//!   beats setting up Rader's convolution outright.
//! * **Fixed-point awkward lengths.** This is the only awkward-length path whose
//!   `i32` precision is good: one rounding per output, against Bluestein's two
//!   full transforms. That is why [`super::DIRECT_MAX_FIXED`] is two orders of
//!   magnitude above the float threshold.
//!
//! The twiddle table is `O(n)`, not `O(n²)`: `W^{jk}` depends only on
//! `(j·k) mod n`.

use crate::num::Arith;

#[derive(Debug, Clone)]
pub(crate) struct Direct<T> {
    n: usize,
    tw_re: Vec<T>,
    tw_im: Vec<T>,
}

impl<T: Arith> Direct<T> {
    pub(crate) fn new(n: usize) -> Self {
        let mut tw_re = Vec::new();
        let mut tw_im = Vec::new();
        for j in 0..n {
            let theta = -core::f64::consts::TAU * j as f64 / n as f64;
            let (s, c) = theta.sin_cos();
            tw_re.push(T::from_f64(c));
            tw_im.push(T::from_f64(s));
        }
        Self { n, tw_re, tw_im }
    }

    pub(crate) const fn len(&self) -> usize {
        self.n
    }

    /// One copy of the input, per component.
    pub(crate) const fn scratch_len(&self) -> usize {
        2 * self.n
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "every index is < n: `(j*k) % n` by construction, and the buffers are checked against n on entry"
    )]
    pub(crate) fn exec(&self, re: &mut [T], im: &mut [T], scratch: &mut [T]) {
        let n = self.n;
        if re.len() < n || im.len() < n || scratch.len() < 2 * n {
            debug_assert!(false, "buffer too small for a direct DFT of {n}");
            return;
        }
        let (xr, rest) = scratch.split_at_mut(n);
        let (xi, _) = rest.split_at_mut(n);

        // Fixed point divides the input by n up front. That is the same
        // "divide by the radix" rule the staged engines apply, collapsed into a
        // single stage, so a `Direct` plan and a `Stockham` plan of the same
        // length agree on the output scale.
        let div = if T::STAGE_SCALED { n as u32 } else { 1 };
        for j in 0..n {
            xr[j] = T::div_int(re[j], div);
            xi[j] = T::div_int(im[j], div);
        }

        for k in 0..n {
            let mut ar = T::acc_of(xr[0]);
            let mut ai = T::acc_of(xi[0]);
            let mut idx = 0usize;
            for j in 1..n {
                idx += k;
                if idx >= n {
                    idx -= n;
                }
                let (wr, wi) = (self.tw_re[idx], self.tw_im[idx]);
                ar = T::acc_mul_add(ar, xr[j], wr);
                ar = T::acc_mul_sub(ar, xi[j], wi);
                ai = T::acc_mul_add(ai, xr[j], wi);
                ai = T::acc_mul_add(ai, xi[j], wr);
            }
            re[k] = T::acc_finish(ar);
            im[k] = T::acc_finish(ai);
        }
    }
}
