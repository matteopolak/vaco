//! DCT-II (forward) and DCT-III (inverse), by Makhoul's reduction to a real
//! DFT.
//!
//! ```text
//!   DCT-II :  X[k] = Σ_n x[n]·cos(π(2n+1)k/(2N))
//!   DCT-III:  y[k] = x[0]/2 + Σ_{n≥1} x[n]·cos(π(2k+1)n/(2N))
//! ```
//!
//! # Forward
//!
//! Permute `v[i] = x[2i]`, `v[N-1-i] = x[2i+1]`, take `V = DFT_N(v)` — which is
//! a real DFT, so it costs an `N/2`-point complex FFT — and read off
//! `X[k] = Re(e^{-iπk/(2N)}·V[k])`. Bins above `N/2` come from `V[k] =
//! conj(V[N-k])`, so the RDFT's `N/2+1` outputs are all that is needed.
//!
//! # Inverse
//!
//! DCT-III is exactly `(N/2)·(DCT-II)⁻¹`, so it is computed by running the
//! forward algorithm backwards rather than by a separate derivation. Inverting
//! `X[k] = Re(d_k V[k])` uses the companion identity `X[N-k] = −Im(d_k V[k])`,
//! giving `V[k] = conj(d_k)·(X[k] − i·X[N−k])` with `X[N] ≡ 0`; then an inverse
//! RDFT and the inverse permutation. Two transforms that share one set of
//! constants and one set of index maps cannot disagree about a sign convention.
//!
//! # Odd lengths
//!
//! The permutation needs `N` even. Odd `N` falls back to a direct `O(N²)`
//! evaluation against an `O(N)` cosine table indexed by `((2n+1)k) mod 4N`. No
//! codec asks for it; it exists so the transform is defined for every length.

use super::rdft::Rdft;
use crate::engine::Ctx;
use crate::num::Arith;

#[derive(Debug, Clone)]
pub(crate) struct Dct<T: Arith> {
    n: usize,
    /// `exp(-iπk/(2N))` for `k ∈ [0, N)`.
    d_re: Vec<T>,
    d_im: Vec<T>,
    /// Even-`n` fast path.
    rdft: Option<Rdft<T>>,
    /// Odd-`n` fallback: `cos(2πj/(4N))` for `j ∈ [0, 4N)`.
    table: Vec<T>,
}

impl<T: Arith> Dct<T> {
    pub(crate) fn new(n: usize) -> Option<Self> {
        if n < 1 {
            return None;
        }
        let mut d_re = Vec::new();
        let mut d_im = Vec::new();
        for k in 0..n {
            let theta = -core::f64::consts::PI * k as f64 / (2.0 * n as f64);
            let (s, c) = theta.sin_cos();
            d_re.push(T::from_f64(c));
            d_im.push(T::from_f64(s));
        }
        let (rdft, table) = if n.is_multiple_of(2) {
            (Rdft::new(n), Vec::new())
        } else {
            let mut t = Vec::new();
            for j in 0..4 * n {
                let theta = core::f64::consts::TAU * j as f64 / (4.0 * n as f64);
                t.push(T::from_f64(theta.cos()));
            }
            (None, t)
        };
        Some(Self {
            n,
            d_re,
            d_im,
            rdft,
            table,
        })
    }

    pub(crate) fn scratch_len(&self) -> usize {
        match &self.rdft {
            Some(r) => 3 * self.n + 2 + r.scratch_len(),
            None => self.n,
        }
    }

    pub(crate) fn describe(&self) -> crate::Decomposition {
        match &self.rdft {
            Some(r) => r.describe(),
            None => crate::Decomposition::Direct { n: self.n },
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "table indices are reduced mod 4n and loop bounds are n; buffers are length-checked on entry"
    )]
    fn naive(&self, x: &[T], out: &mut [T], inverse: bool) {
        let n = self.n;
        let four_n = 4 * n;
        let div = if T::STAGE_SCALED { n as u32 } else { 1 };
        // Index of the cosine for term j at output k: (2j+1)k for DCT-II,
        // (2k+1)j for DCT-III. `sample` also folds in the fixed-point 1/n and
        // DCT-III's halved DC, so the j = 0 seed and the loop agree.
        let sample = |j: usize| {
            let v = T::div_int(x[j], div);
            if inverse && j == 0 {
                T::div_int(v, 2)
            } else {
                v
            }
        };
        for (k, slot) in out.iter_mut().enumerate().take(n) {
            let idx0 = if inverse { 0 } else { k % four_n };
            let mut acc = T::acc_mul(sample(0), self.table[idx0]);
            for j in 1..n {
                let idx = if inverse {
                    ((2 * k + 1) * j) % four_n
                } else {
                    ((2 * j + 1) * k) % four_n
                };
                acc = T::acc_mul_add(acc, sample(j), self.table[idx]);
            }
            *slot = T::acc_finish(acc);
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "index maps are k, n-k and 2i / n-1-i with i < n/2, all inside the length-checked buffers"
    )]
    #[allow(clippy::integer_division, reason = "n is even on this path")]
    pub(crate) fn exec(&self, x: &[T], out: &mut [T], scratch: &mut [T], ctx: Ctx, inverse: bool) {
        let n = self.n;
        if x.len() < n || out.len() < n || scratch.len() < self.scratch_len() {
            debug_assert!(false, "buffer too small for DCT({n})");
            return;
        }
        let Some(rdft) = &self.rdft else {
            self.naive(x, out, inverse);
            return;
        };
        let m = n / 2;
        let (perm, rest) = scratch.split_at_mut(n);
        let (vr, rest) = rest.split_at_mut(m + 1);
        let (vi, sub) = rest.split_at_mut(m + 1);

        if inverse {
            // V[k] = conj(d_k)·(x[k] − i·x[N−k]), with x[N] ≡ 0.
            for k in 0..=m {
                let a = x[k];
                let b = if k == 0 { T::ZERO } else { x[n - k] };
                let (dr, di) = (self.d_re[k % n], self.d_im[k % n]);
                vr[k] = T::acc_finish(T::acc_mul_sub(T::acc_mul(a, dr), b, di));
                vi[k] = T::neg(T::acc_finish(T::acc_mul_add(T::acc_mul(b, dr), a, di)));
            }
            rdft.inverse_split(vr, vi, perm, sub, ctx);
            for i in 0..m {
                out[2 * i] = T::div_int(perm[i], 2);
                out[2 * i + 1] = T::div_int(perm[n - 1 - i], 2);
            }
        } else {
            for i in 0..m {
                perm[i] = x[2 * i];
                perm[n - 1 - i] = x[2 * i + 1];
            }
            rdft.forward_split(perm, vr, vi, sub, ctx);
            for k in 0..n {
                let (dr, di) = (self.d_re[k], self.d_im[k]);
                let (a, b, neg) = if k <= m {
                    (vr[k], vi[k], false)
                } else {
                    (vr[n - k], vi[n - k], true)
                };
                let acc = T::acc_mul(a, dr);
                out[k] = T::acc_finish(if neg {
                    T::acc_mul_add(acc, b, di)
                } else {
                    T::acc_mul_sub(acc, b, di)
                });
            }
        }
    }
}
