//! Rader's algorithm: a prime-length DFT as a cyclic convolution.
//!
//! For prime `p` with primitive root `g`, the non-zero indices form a cyclic
//! group under multiplication, so with `n = g^q` and `k = g^{-m}`:
//!
//! ```text
//!   X[g^{-m}] = x[0] + Σ_q x[g^q] · W^{g^{q-m}}
//! ```
//!
//! which is a length-`(p-1)` cyclic convolution of `a[q] = x[g^q]` with
//! `bb[q] = W^{g^{-q}}`. `p-1` is even and usually smooth, so the inner
//! transform is normally a plain mixed-radix one.
//!
//! **Why the convolution is length `p-1` and not a zero-padded power of two.**
//! Padding to `≥ 2(p-1)-1` would make Rader cost the same as Bluestein and
//! there would be no reason to have both. Using the exact cyclic length is the
//! whole win: roughly half the work.

use super::conv::Conv;
use crate::factor;
use crate::num::Arith;
use super::Ctx;

#[derive(Debug, Clone)]
pub(crate) struct Rader<T: Arith> {
    p: usize,
    /// `g^q mod p` for `q ∈ [0, p-1)` — where the input for slot `q` comes from.
    in_idx: Vec<u32>,
    /// `g^{-m} mod p` — where output slot `m` goes.
    out_idx: Vec<u32>,
    conv: Conv<T>,
}

impl<T: Arith> Rader<T> {
    /// `None` when `p` is not an odd prime or has no computable primitive root,
    /// so the planner falls through to Bluestein rather than asserting.
    pub(crate) fn new(p: usize, depth: u32) -> Option<Self> {
        if p < 3 || !factor::is_prime(p) {
            return None;
        }
        let g = factor::primitive_root(p)?;
        let g_inv = factor::mod_inverse(g, p)?;
        let l = p - 1;

        let mut in_idx = Vec::new();
        let mut out_idx = Vec::new();
        let mut b_re = Vec::new();
        let mut b_im = Vec::new();
        let mut gq = 1usize;
        let mut giq = 1usize;
        for _ in 0..l {
            in_idx.push(gq as u32);
            out_idx.push(giq as u32);
            // bb[q] = W_p^{g^{-q}}
            let theta = -core::f64::consts::TAU * giq as f64 / p as f64;
            let (s, c) = theta.sin_cos();
            b_re.push(T::from_f64(c));
            b_im.push(T::from_f64(s));
            gq = gq * g % p;
            giq = giq * g_inv % p;
        }

        Some(Self {
            p,
            in_idx,
            out_idx,
            conv: Conv::new(l, &b_re, &b_im, depth),
        })
    }


    pub(crate) fn scratch_len(&self) -> usize {
        2 * (self.p - 1) + self.conv.scratch_len()
    }

    pub(crate) fn describe(&self) -> crate::Decomposition {
        crate::Decomposition::Rader {
            p: self.p,
            inner: Box::new(self.conv.describe()),
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "in_idx and out_idx hold residues mod p, so every index is < p; the buffers are checked against p on entry"
    )]
    pub(crate) fn exec(&self, re: &mut [T], im: &mut [T], scratch: &mut [T], ctx: Ctx) {
        let p = self.p;
        let l = p - 1;
        if re.len() < p || im.len() < p || scratch.len() < self.scratch_len() {
            debug_assert!(false, "buffer too small for Rader({p})");
            return;
        }
        let (ar, rest) = scratch.split_at_mut(l);
        let (ai, sub) = rest.split_at_mut(l);

        // Fixed point produces DFT/p, so the parts that bypass the convolution
        // — X[0] and the x[0] term — are divided by p here. `div_int(·, 1)` is
        // the exact identity, so the float path shares the code.
        let div = if T::STAGE_SCALED { p as u32 } else { 1 };
        let x0r = T::div_int(re[0], div);
        let x0i = T::div_int(im[0], div);
        let mut s_r = x0r;
        let mut s_i = x0i;
        for j in 1..p {
            s_r = T::add(s_r, T::div_int(re[j], div));
            s_i = T::add(s_i, T::div_int(im[j], div));
        }

        for q in 0..l {
            let src = self.in_idx[q] as usize;
            ar[q] = re[src];
            ai[q] = im[src];
        }
        self.conv.exec(ar, ai, sub, ctx, p as u64);

        re[0] = s_r;
        im[0] = s_i;
        for m in 0..l {
            let dst = self.out_idx[m] as usize;
            re[dst] = T::add(x0r, ar[m]);
            im[dst] = T::add(x0i, ai[m]);
        }
    }
}
