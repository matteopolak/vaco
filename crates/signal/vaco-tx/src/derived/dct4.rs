//! DCT-IV via a quarter-length complex FFT — the core the MDCT is built on.
//!
//! ```text
//!   C4[k] = Σ_{n<M} u[n]·cos(π/M·(n+½)(k+½))
//! ```
//!
//! # Derivation, because the constants are easy to get wrong
//!
//! Split `n` into evens `2r` and odds `M−1−2r`, `r ∈ [0, L)`, `L = M/2`, and set
//! `t_r = u[2r] + i·u[M−1−2r]`. Writing `β_r = π(4r+1)(4k+1)/(4M)`, the two
//! output halves fall out of one complex sum:
//!
//! ```text
//!   C4[2k]     =  Re Σ_r t_r·e^{-iβ_r}
//!   C4[M-1-2k] = −Im Σ_r t_r·e^{-iβ_r}
//! ```
//!
//! and `β_r` factors exactly into an `L`-point DFT kernel plus two rotations:
//!
//! ```text
//!   e^{-iβ_r} = e^{-2πi·rk/L} · e^{-iπ(r+⅛)/M} · e^{-iπ(k+⅛)/M}
//! ```
//!
//! So: pack, pre-rotate, `L`-point complex FFT, post-rotate, unpack. For an
//! MDCT of length `N` this `L` is `N/4` — the factor-of-four reduction that
//! makes MDCT codecs practical, and the thing a naive implementation loses by
//! reaching for an `N/2`-point FFT.
//!
//! # Scaling
//!
//! Fixed point halves at the pack step, which is needed anyway: `|t_r|` reaches
//! `√2·2^31` and would saturate the pre-rotation otherwise. Combined with the
//! inner FFT's `1/L`, the output is exactly `C4/M` — the crate's uniform
//! "transform of length `M` is scaled by `1/M`" rule, with no separate
//! correction pass.

use crate::engine::Ctx;
use crate::engine::Engine;
use crate::num::Arith;

#[derive(Debug, Clone)]
pub(crate) struct Dct4<T: Arith> {
    m: usize,
    l: usize,
    pre_re: Vec<T>,
    pre_im: Vec<T>,
    post_re: Vec<T>,
    post_im: Vec<T>,
    fft: Engine<T>,
}

impl<T: Arith> Dct4<T> {
    /// `None` unless `m` is even and at least 2.
    #[allow(
        clippy::integer_division,
        reason = "m is even by the guard immediately above"
    )]
    pub(crate) fn new(m: usize) -> Option<Self> {
        if m < 2 || !m.is_multiple_of(2) {
            return None;
        }
        let l = m / 2;
        let rot = |j: usize| {
            let theta = -core::f64::consts::PI * (j as f64 + 0.125) / m as f64;
            theta.sin_cos()
        };
        let mut pre_re = Vec::new();
        let mut pre_im = Vec::new();
        let mut post_re = Vec::new();
        let mut post_im = Vec::new();
        for j in 0..l {
            let (s, c) = rot(j);
            pre_re.push(T::from_f64(c));
            pre_im.push(T::from_f64(s));
            post_re.push(T::from_f64(c));
            post_im.push(T::from_f64(s));
        }
        Some(Self {
            m,
            l,
            pre_re,
            pre_im,
            post_re,
            post_im,
            fft: Engine::new(l),
        })
    }

    pub(crate) fn scratch_len(&self) -> usize {
        2 * self.l + self.fft.scratch_len()
    }

    pub(crate) fn describe(&self) -> crate::Decomposition {
        self.fft.describe()
    }

    /// `out = C4(u)` (float) or `C4(u)/M` (fixed). `u` and `out` must not alias.
    #[allow(
        clippy::indexing_slicing,
        reason = "2r and M-1-2r are < m for r < l = m/2, and every buffer is length-checked against m on entry"
    )]
    pub(crate) fn exec(&self, u: &[T], out: &mut [T], scratch: &mut [T], ctx: Ctx) {
        let (m, l) = (self.m, self.l);
        if u.len() < m || out.len() < m || scratch.len() < self.scratch_len() {
            debug_assert!(false, "buffer too small for DCT-IV({m})");
            return;
        }
        let (yr, rest) = scratch.split_at_mut(l);
        let (yi, sub) = rest.split_at_mut(l);

        let div = if T::STAGE_SCALED { 2 } else { 1 };
        for r in 0..l {
            let a = T::div_int(u[2 * r], div);
            let b = T::div_int(u[m - 1 - 2 * r], div);
            let (x, y) = T::cmul_c(a, b, self.pre_re[r], self.pre_im[r]);
            yr[r] = x;
            yi[r] = y;
        }
        self.fft.exec(yr, yi, sub, ctx);
        for k in 0..l {
            let (x, y) = T::cmul_c(yr[k], yi[k], self.post_re[k], self.post_im[k]);
            out[2 * k] = x;
            out[m - 1 - 2 * k] = T::neg(y);
        }
    }
}
