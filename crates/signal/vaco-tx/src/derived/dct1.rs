//! DCT-I and DST-I, by symmetric extension into a real DFT.
//!
//! ```text
//!   DCT-I: X[k] = (x[0] + (−1)^k·x[N−1])/2 + Σ_{n=1}^{N−2} x[n]·cos(πnk/(N−1))
//!   DST-I: X[k] = Σ_{n=0}^{N−1} x[n]·sin(π(n+1)(k+1)/(N+1))
//! ```
//!
//! Even-extend a length-`N` DCT-I input to length `2(N−1)` and its real DFT is
//! `2·X`, with the `N` unique bins landing exactly on the `N` outputs. Odd-extend
//! a DST-I input to length `2(N+1)` and the DFT is purely imaginary, with
//! `X[k] = −Im(E[k+1])/2`.
//!
//! Both are self-inverse up to a constant, so [`crate::Direction`] selects only
//! a normalisation — `2/(N−1)` for DCT-I, `2/(N+1)` for DST-I — chosen so that
//! `inverse(forward(x)) = x` in float.
//!
//! Plan 17 §C.4.4 is explicit that these are the least-used transforms in the
//! set and get the straightforward extension-based implementation and no
//! specialised optimisation. They do.

use super::rdft::Rdft;
use crate::engine::scale_ratio;
use crate::num::Arith;
use crate::engine::Ctx;

#[derive(Debug, Clone)]
pub(crate) struct SymTx<T: Arith> {
    n: usize,
    /// Extension length: `2(n−1)` for DCT-I, `2(n+1)` for DST-I.
    ext: usize,
    sine: bool,
    rdft: Rdft<T>,
}

impl<T: Arith> SymTx<T> {
    /// `sine = false` builds DCT-I (needs `n ≥ 2`), `true` builds DST-I
    /// (needs `n ≥ 1`).
    pub(crate) fn new(n: usize, sine: bool) -> Option<Self> {
        if sine {
            if n < 1 {
                return None;
            }
        } else if n < 2 {
            return None;
        }
        let ext = if sine { 2 * (n + 1) } else { 2 * (n - 1) };
        Some(Self {
            n,
            ext,
            sine,
            rdft: Rdft::new(ext)?,
        })
    }


    #[allow(clippy::integer_division, reason = "ext is even by construction")]
    pub(crate) fn scratch_len(&self) -> usize {
        self.ext + 2 * (self.ext / 2 + 1) + self.rdft.scratch_len()
    }

    pub(crate) fn describe(&self) -> crate::Decomposition {
        self.rdft.describe()
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "extension indices are < ext and bin indices are ≤ ext/2, both inside buffers sized by scratch_len"
    )]
    #[allow(clippy::integer_division, reason = "ext is even by construction")]
    pub(crate) fn exec(
        &self,
        x: &[T],
        out: &mut [T],
        scratch: &mut [T],
        ctx: Ctx,
        inverse: bool,
    ) {
        let (n, ext) = (self.n, self.ext);
        let bins = ext / 2 + 1;
        if x.len() < n || out.len() < n || scratch.len() < self.scratch_len() {
            debug_assert!(false, "buffer too small for symmetric transform({n})");
            return;
        }
        let (e, rest) = scratch.split_at_mut(ext);
        let (er, rest) = rest.split_at_mut(bins);
        let (ei, sub) = rest.split_at_mut(bins);

        for slot in e.iter_mut() {
            *slot = T::ZERO;
        }
        if self.sine {
            // Odd extension: 0, x[0..n], 0, −x[n−1..0].
            for j in 0..n {
                e[j + 1] = x[j];
                e[ext - 1 - j] = T::neg(x[j]);
            }
        } else {
            // Even extension: x[0..n], then x[n−2..1] mirrored.
            if let (Some(head), Some(src)) = (e.get_mut(..n), x.get(..n)) {
                head.copy_from_slice(src);
            }
            for j in 1..n - 1 {
                e[ext - j] = x[j];
            }
        }

        self.rdft.forward_split(e, er, ei, sub, ctx);

        if self.sine {
            for k in 0..n {
                out[k] = T::neg(T::div_int(ei[k + 1], 2));
            }
        } else {
            for k in 0..n {
                out[k] = T::div_int(er[k], 2);
            }
        }

        if inverse {
            let den = if self.sine { n + 1 } else { n - 1 };
            scale_ratio(&mut out[..n], 2, den as u64);
        }
    }
}
