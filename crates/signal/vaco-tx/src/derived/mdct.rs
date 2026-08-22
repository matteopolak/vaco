//! MDCT and IMDCT.
//!
//! ```text
//!   X[k] = Σ_{n<N} x[n]·cos(2π/N·(n + ½ + N/4)(k + ½)),   k ∈ [0, N/2)
//! ```
//!
//! # Fold, then DCT-IV
//!
//! Shifting the index by `N/4` turns the MDCT kernel into a DCT-IV kernel with
//! period `2N` and the reflection `g(2M−1−m) = −g(m)`, `M = N/2`. Folding the
//! `N` inputs into `M` by those two symmetries gives, exactly:
//!
//! ```text
//!   u[j] = −x[3N/4 − 1 − j] − x[3N/4 + j]      j ∈ [0, N/4)
//!   u[j] =  x[j − N/4]      − x[3N/4 − 1 − j]  j ∈ [N/4, N/2)
//!   X    = DCT-IV_M(u)
//! ```
//!
//! and the IMDCT is the transpose of the fold applied to `DCT-IV_M(X)`:
//!
//! ```text
//!   y[n] =  v[n + N/4]         n ∈ [0, N/4)
//!   y[n] = −v[3N/4 − 1 − n]    n ∈ [N/4, 3N/4)
//!   y[n] = −v[n − 3N/4]        n ∈ [3N/4, N)
//! ```
//!
//! The `N` IMDCT outputs carry the time-domain-aliasing structure
//! `y[N/2 + j] = y[N − 1 − j]`, so the unique half is `y[0 .. N/2)`. Without
//! [`crate::TxFlags::FULL_IMDCT`] that is all we emit; with it we fill the rest
//! from the same `v`, which is a copy with sign flips — "a fill, not a
//! transform", as plan 17 §C.4.1 puts it.
//!
//! # Windowing is not here
//!
//! Sine, KBD and Vorbis power-complementary windows are codec-specific and
//! belong in the codec DSP crate. `vaco-tx` transforms; it does not window.

use super::dct4::Dct4;
use crate::engine::Ctx;
use crate::num::Arith;

#[derive(Debug, Clone)]
pub(crate) struct Mdct<T: Arith> {
    n: usize,
    dct4: Dct4<T>,
}

impl<T: Arith> Mdct<T> {
    /// `None` unless `n` is a positive multiple of 4.
    #[allow(
        clippy::integer_division,
        reason = "n is a multiple of 4 by the guard immediately above"
    )]
    pub(crate) fn new(n: usize) -> Option<Self> {
        if n < 4 || !n.is_multiple_of(4) {
            return None;
        }
        Some(Self {
            n,
            dct4: Dct4::new(n / 2)?,
        })
    }

    #[allow(clippy::integer_division, reason = "n is a multiple of 4")]
    pub(crate) fn scratch_len(&self) -> usize {
        // The folded sequence, a scaled copy of the coefficients for the
        // inverse, and the DCT-IV's own working set.
        self.n + self.dct4.scratch_len()
    }

    pub(crate) fn describe(&self) -> crate::Decomposition {
        self.dct4.describe()
    }

    /// Forward: `n` real inputs to `n/2` coefficients.
    #[allow(
        clippy::indexing_slicing,
        reason = "every index is derived from j < n/2 and stays inside [0, n); buffers are length-checked on entry"
    )]
    #[allow(clippy::integer_division, reason = "n is a multiple of 4")]
    pub(crate) fn forward(&self, x: &[T], out: &mut [T], scratch: &mut [T], ctx: Ctx) {
        let n = self.n;
        let (q, h) = (n / 4, n / 2);
        if x.len() < n || out.len() < h || scratch.len() < self.scratch_len() {
            debug_assert!(false, "buffer too small for MDCT({n})");
            return;
        }
        // The layout matches `inverse`'s — fold buffer, coefficient buffer,
        // DCT-IV working set — so one `scratch_len` covers both directions.
        let (u, sub) = scratch.split_at_mut(h);
        let (_coeff_slot, sub) = sub.split_at_mut(h);
        // Fixed point halves here so |u| cannot leave Q31; combined with the
        // DCT-IV core's own 1/M that lands the output on exactly X/N.
        if T::STAGE_SCALED {
            for j in 0..q {
                u[j] = T::neg(T::half_sum(x[3 * q - 1 - j], x[3 * q + j]));
            }
            for j in q..h {
                u[j] = T::half_diff(x[j - q], x[3 * q - 1 - j]);
            }
        } else {
            for j in 0..q {
                u[j] = T::neg(T::add(x[3 * q - 1 - j], x[3 * q + j]));
            }
            for j in q..h {
                u[j] = T::sub(x[j - q], x[3 * q - 1 - j]);
            }
        }
        self.dct4.exec(u, out, sub, ctx);
    }

    /// Inverse: `n/2` coefficients to `n/2` samples, or `n` with `full`.
    #[allow(
        clippy::indexing_slicing,
        reason = "the output range is chosen by `full` and each branch indexes v with a value < n/2; buffers are length-checked on entry"
    )]
    #[allow(clippy::integer_division, reason = "n is a multiple of 4")]
    pub(crate) fn inverse(
        &self,
        coeffs: &[T],
        out: &mut [T],
        scratch: &mut [T],
        ctx: Ctx,
        full: bool,
    ) {
        let n = self.n;
        let (q, h) = (n / 4, n / 2);
        let want = if full { n } else { h };
        if coeffs.len() < h || out.len() < want || scratch.len() < self.scratch_len() {
            debug_assert!(false, "buffer too small for IMDCT({n})");
            return;
        }
        let (v, rest) = scratch.split_at_mut(h);
        let (tmp, sub) = rest.split_at_mut(h);
        if T::STAGE_SCALED {
            // Halving the coefficients puts the fixed-point output on 1/N, and
            // is also what stops the DCT-IV pack from saturating.
            for j in 0..h {
                tmp[j] = T::div_int(coeffs[j], 2);
            }
            self.dct4.exec(tmp, v, sub, ctx);
        } else {
            self.dct4.exec(coeffs, v, sub, ctx);
        }

        for (i, slot) in out.iter_mut().enumerate().take(want) {
            *slot = if i < q {
                v[i + q]
            } else if i < 3 * q {
                T::neg(v[3 * q - 1 - i])
            } else {
                T::neg(v[i - 3 * q])
            };
        }
    }
}
