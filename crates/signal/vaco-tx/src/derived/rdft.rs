//! Real-input DFT, and its inverse — the shared substrate under DCT-I, DST-I
//! and DCT-II/III.
//!
//! An `N`-point real DFT costs one `N/2`-point complex FFT plus an `O(N)`
//! split step: pack the even samples as real parts and the odd samples as
//! imaginary parts, transform, then separate the two interleaved spectra.
//!
//! ```text
//!   Z = FFT_{N/2}(x[0] + i·x[1], x[2] + i·x[3], …)
//!   A[k] = (Z[k] + conj(Z[M-k]))/2          (spectrum of the even samples)
//!   O[k] = (Z[k] − conj(Z[M-k]))/2          (i · spectrum of the odd samples)
//!   X[k] = A[k] − i·W_N^k·O[k],   k ∈ [0, M]
//! ```
//!
//! `k = M` reads `Z[0]` for both halves, which is correct because `Z[M] = Z[0]`.
//!
//! # Scaling
//!
//! The inner FFT is length `M = N/2`, so in fixed point it contributes `1/M`
//! where the contract wants `1/N`; the forward path therefore halves once at the
//! end and the float path does not. The inverse is the mirror image: the float
//! path doubles (so that `inverse(forward(x)) = N·x`) and the fixed path does
//! nothing, because `1/M · 1/2` from the `A`/`B` extraction already lands on
//! `1/N`. One `if`, in one place, and the two directions stay symmetric.

use crate::engine::Engine;
use crate::num::Arith;
use crate::engine::Ctx;

/// How the real transform is realised.
///
/// The packed form needs an even length. Rather than reject odd lengths — which
/// would put a hole in the crate's totality promise — an odd `n` runs a full
/// `n`-point complex transform with a zero imaginary part and keeps the unique
/// bins. Twice the work, never asked for by a codec, and total.
#[derive(Debug, Clone)]
enum Mode {
    Packed,
    Full,
}

#[derive(Debug, Clone)]
pub(crate) struct Rdft<T: Arith> {
    n: usize,
    m: usize,
    mode: Mode,
    /// `W_N^k = exp(-2πik/N)` for `k ∈ [0, M]`.
    tw_re: Vec<T>,
    tw_im: Vec<T>,
    fft: Engine<T>,
}

impl<T: Arith> Rdft<T> {
    /// `None` only for `n < 1`.
    #[allow(
        clippy::integer_division,
        reason = "the packed branch is guarded on n being even; the odd branch wants the floor"
    )]
    pub(crate) fn new(n: usize) -> Option<Self> {
        if n < 1 {
            return None;
        }
        if !n.is_multiple_of(2) {
            return Some(Self {
                n,
                m: n / 2,
                mode: Mode::Full,
                tw_re: Vec::new(),
                tw_im: Vec::new(),
                fft: Engine::new(n),
            });
        }
        let m = n / 2;
        let mut tw_re = Vec::new();
        let mut tw_im = Vec::new();
        for k in 0..=m {
            let theta = -core::f64::consts::TAU * k as f64 / n as f64;
            let (s, c) = theta.sin_cos();
            tw_re.push(T::from_f64(c));
            tw_im.push(T::from_f64(s));
        }
        Some(Self {
            n,
            m,
            mode: Mode::Packed,
            tw_re,
            tw_im,
            fft: Engine::new(m),
        })
    }

    /// Unique complex bins: `N/2 + 1`.
    pub(crate) const fn bins(&self) -> usize {
        self.m + 1
    }
    pub(crate) fn scratch_len(&self) -> usize {
        2 * self.n + self.fft.scratch_len()
    }

    pub(crate) fn describe(&self) -> crate::Decomposition {
        self.fft.describe()
    }

    /// Forward, writing split-complex output of [`Rdft::bins`] elements each.
    #[allow(
        clippy::indexing_slicing,
        reason = "loops run to m or m+1 and every buffer is length-checked against those on entry"
    )]
    pub(crate) fn forward_split(
        &self,
        x: &[T],
        out_re: &mut [T],
        out_im: &mut [T],
        scratch: &mut [T],
        ctx: Ctx,
    ) {
        let (n, m) = (self.n, self.m);
        if x.len() < n
            || out_re.len() < m + 1
            || out_im.len() < m + 1
            || scratch.len() < self.scratch_len()
        {
            debug_assert!(false, "buffer too small for RDFT({n})");
            return;
        }
        if matches!(self.mode, Mode::Full) {
            let (zr, rest) = scratch.split_at_mut(n);
            let (zi, sub) = rest.split_at_mut(n);
            for j in 0..n {
                zr[j] = x[j];
                zi[j] = T::ZERO;
            }
            self.fft.exec(zr, zi, sub, ctx);
            if let (Some(dr), Some(sr), Some(di), Some(si)) = (
                out_re.get_mut(..=m),
                zr.get(..=m),
                out_im.get_mut(..=m),
                zi.get(..=m),
            ) {
                dr.copy_from_slice(sr);
                di.copy_from_slice(si);
            }
            return;
        }
        let (zr, rest) = scratch.split_at_mut(m);
        let (zi, sub) = rest.split_at_mut(m);
        for j in 0..m {
            zr[j] = x[2 * j];
            zi[j] = x[2 * j + 1];
        }
        self.fft.exec(zr, zi, sub, ctx);

        let halve = T::STAGE_SCALED;
        for k in 0..=m {
            let a = if k == m { 0 } else { k };
            let b = if k == 0 || k == m { 0 } else { m - k };
            let (zkr, zki) = (zr[a], zi[a]);
            let (zmr, zmi) = (zr[b], T::neg(zi[b]));
            let er = T::half_sum(zkr, zmr);
            let ei = T::half_sum(zki, zmi);
            let orr = T::half_diff(zkr, zmr);
            let oii = T::half_diff(zki, zmi);
            // X[k] = E − i·W^k·O
            let (pr, pi) = T::cmul_c(orr, oii, self.tw_re[k], self.tw_im[k]);
            let mut xr = T::add(er, pi);
            let mut xi = T::sub(ei, pr);
            if halve {
                xr = T::div_int(xr, 2);
                xi = T::div_int(xi, 2);
            }
            out_re[k] = xr;
            out_im[k] = xi;
        }
    }

    /// Inverse, from split-complex bins to `n` real samples.
    ///
    /// Unnormalised in float, so `inverse(forward(x)) = N·x`; scaled by `1/N` in
    /// fixed point, so `inverse(forward(x)) = x/N`.
    #[allow(
        clippy::indexing_slicing,
        reason = "loops run to m and every buffer is length-checked against m+1 or n on entry"
    )]
    pub(crate) fn inverse_split(
        &self,
        in_re: &[T],
        in_im: &[T],
        out: &mut [T],
        scratch: &mut [T],
        ctx: Ctx,
    ) {
        let (n, m) = (self.n, self.m);
        if in_re.len() < m + 1
            || in_im.len() < m + 1
            || out.len() < n
            || scratch.len() < self.scratch_len()
        {
            debug_assert!(false, "buffer too small for inverse RDFT({n})");
            return;
        }
        if matches!(self.mode, Mode::Full) {
            let (zr, rest) = scratch.split_at_mut(n);
            let (zi, sub) = rest.split_at_mut(n);
            for k in 0..n {
                let (a, b) = if k <= m {
                    (in_re[k], in_im[k])
                } else {
                    (in_re[n - k], T::neg(in_im[n - k]))
                };
                zr[k] = a;
                zi[k] = b;
            }
            self.fft.exec(zi, zr, sub, ctx);
            if let (Some(d), Some(s)) = (out.get_mut(..n), zr.get(..n)) {
                d.copy_from_slice(s);
            }
            return;
        }
        let (zr, rest) = scratch.split_at_mut(m);
        let (zi, sub) = rest.split_at_mut(m);
        for k in 0..m {
            let (xr, xi) = (in_re[k], in_im[k]);
            let (yr, yi) = (in_re[m - k], T::neg(in_im[m - k]));
            let ar = T::half_sum(xr, yr);
            let ai = T::half_sum(xi, yi);
            let dr = T::half_diff(xr, yr);
            let di = T::half_diff(xi, yi);
            // B[k] = W^{-k}·(X[k] − conj(X[M−k]))/2, then Z[k] = A[k] + i·B[k].
            let (br, bi) = T::cmul_c(dr, di, self.tw_re[k], T::neg(self.tw_im[k]));
            zr[k] = T::sub(ar, bi);
            zi[k] = T::add(ai, br);
        }
        // Inverse complex transform by argument swap.
        self.fft.exec(zi, zr, sub, ctx);
        let double = !T::STAGE_SCALED;
        for j in 0..m {
            let (a, b) = if double {
                (T::mul_int(zr[j], 2), T::mul_int(zi[j], 2))
            } else {
                (zr[j], zi[j])
            };
            out[2 * j] = a;
            out[2 * j + 1] = b;
        }
    }
}
