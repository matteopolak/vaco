//! Cyclic convolution by transform — the shared engine under Rader and
//! Bluestein.
//!
//! `c = IDFT(DFT(a) ⊙ DFT(b)) / L`. `DFT(b)` is precomputed at plan time, so
//! execution costs one forward transform, `L` complex multiplies and one
//! inverse.
//!
//! # The normalisation, stated once
//!
//! Our engines compute `E(x) = DFT(x)·k` and `E⁻¹(y) = IDFT_unnorm(y)·k`, with
//! `k = 1` for floats and `k = 1/L` for `i32` (per-stage scaling). So
//!
//! ```text
//!   C = E⁻¹(E(a) ⊙ E(b)) = c · L · k³
//! ```
//!
//! and the caller who wants `c / D` applies `C · 1/(L·k³·D)`:
//!
//! | precision | factor |
//! |---|---|
//! | `f32`/`f64` (`k = 1`, `D = 1`) | `1/L` |
//! | `i32` (`k = 1/L`) | `L²/D` |
//!
//! For `i32`, `D` is the transform length the caller is producing, because a
//! fixed-point transform of length `N` outputs `DFT/N`. That is also where the
//! precision goes: `C` sits at roughly `2^31·L/(L²·…)`, so the awkward-length
//! fixed-point paths carry materially less headroom than the staged ones. See
//! `docs/signal/vaco-tx.md`, "Precision of the awkward lengths".

use super::{Engine, scale_ratio};
use crate::num::Arith;
use super::Ctx;

#[derive(Debug, Clone)]
pub(crate) struct Conv<T: Arith> {
    l: usize,
    engine: Engine<T>,
    bf_re: Vec<T>,
    bf_im: Vec<T>,
}

impl<T: Arith> Conv<T> {
    /// `b_re`/`b_im` is the kernel in cyclic order, length `l`.
    pub(crate) fn new(l: usize, b_re: &[T], b_im: &[T], depth: u32) -> Self {
        let engine = Engine::build(l, depth + 1);
        let mut bf_re = b_re.to_vec();
        let mut bf_im = b_im.to_vec();
        bf_re.resize(l, T::ZERO);
        bf_im.resize(l, T::ZERO);
        let mut scratch = vec![T::ZERO; engine.scratch_len()];
        engine.exec(&mut bf_re, &mut bf_im, &mut scratch, Ctx::scalar_only());
        Self {
            l,
            engine,
            bf_re,
            bf_im,
        }
    }


    pub(crate) fn scratch_len(&self) -> usize {
        self.engine.scratch_len()
    }

    pub(crate) fn describe(&self) -> crate::Decomposition {
        self.engine.describe()
    }

    /// In-place cyclic convolution of `(ar, ai)` with the stored kernel,
    /// normalised so the result is `c / d_fixed` in fixed point and `c` in float.
    #[allow(
        clippy::indexing_slicing,
        reason = "the loop bound is `self.l`, and every buffer is length-checked against it on entry"
    )]
    pub(crate) fn exec(
        &self,
        ar: &mut [T],
        ai: &mut [T],
        scratch: &mut [T],
        ctx: Ctx,
        d_fixed: u64,
    ) {
        let l = self.l;
        if ar.len() < l || ai.len() < l {
            debug_assert!(false, "convolution buffer shorter than {l}");
            return;
        }
        self.engine.exec(ar, ai, scratch, ctx);
        for q in 0..l {
            let (x, y) = T::cmul_c(ar[q], ai[q], self.bf_re[q], self.bf_im[q]);
            ar[q] = x;
            ai[q] = y;
        }
        // Inverse by argument swap: IDFT(y) = swap(F(swap(y))).
        self.engine.exec(ai, ar, scratch, ctx);

        let (num, den) = if T::STAGE_SCALED {
            ((l as u64) * (l as u64), d_fixed)
        } else {
            (1, l as u64)
        };
        scale_ratio(&mut ar[..l], num, den);
        scale_ratio(&mut ai[..l], num, den);
    }
}
