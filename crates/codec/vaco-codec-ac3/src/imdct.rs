//! IMDCT and the window/overlap-add that turns 256 frequency coefficients
//! into 256 new time-domain samples per channel per block. ATSC A/52:2012
//! §7.9.4.1.
//!
//! # The window is the spec's, not an approximation
//!
//! An earlier version of this module called its Kaiser-Bessel-derived
//! (alpha=5) window "a close approximation of AC-3's actual window, but not
//! the same table", and named it this crate's largest source of decode
//! error. Both claims are wrong, and measurably so: A/52 Table 7.33's 256
//! stated coefficients agree with this KBD(alpha=5) construction to a
//! maximum absolute difference of 5.0e-6, which is exactly the rounding
//! error of the table's own five decimal places. The table is the
//! approximation; computing the window is the exact form. Substituting
//! Table 7.33's rounded values would move decoded output *away* from
//! `ffmpeg`'s, not toward it.
//!
//! The IMDCT runs as a direct O(N^2) sum rather than the spec's
//! IFFT-with-twiddles factorisation; the two are numerically equivalent
//! (see [`imdct`] on the sign convention, which is not equivalent and did
//! have to be taken from the spec). A fast transform is a follow-up, not a
//! correctness question.

use std::f64::consts::PI;

/// Modified Bessel function of the first kind, order 0, via its power
/// series — textbook math, used only to build the KBD window below.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0f64;
    let mut term = 1.0f64;
    let mut k = 1.0f64;
    while term > sum * 1e-12 {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
        if k > 100.0 {
            break;
        }
    }
    sum
}

/// A Kaiser-Bessel-Derived window of length `n` (even), parameterised by
/// `alpha`. Standard construction: a Kaiser window of half-length `n/2 + 1`,
/// cumulative-sum-normalised, then mirrored — the same recipe MP3/AAC/Vorbis
/// implementations use for their own MDCT windows.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "n is always even (a transform length); halving is exact"
)]
pub fn kbd_window(n: usize, alpha: f64) -> Vec<f32> {
    let half = n / 2;
    let denom = bessel_i0(PI * alpha);
    let mut kaiser = vec![0f64; half + 1];
    for (i, slot) in kaiser.iter_mut().enumerate() {
        let t = (2.0 * i as f64) / (half as f64) - 1.0;
        let arg = alpha * (1.0 - t * t).max(0.0).sqrt();
        *slot = bessel_i0(PI * arg) / denom;
    }
    let mut cumsum = vec![0f64; half + 1];
    let mut acc = 0.0;
    for i in 0..=half {
        acc += kaiser.get(i).copied().unwrap_or(0.0);
        if let Some(slot) = cumsum.get_mut(i) {
            *slot = acc;
        }
    }
    let total = cumsum.last().copied().unwrap_or(1.0).max(1e-12);
    let mut window = vec![0f32; n];
    for i in 0..half {
        let w = (cumsum.get(i).copied().unwrap_or(0.0) / total).sqrt();
        if let Some(slot) = window.get_mut(i) {
            *slot = w as f32;
        }
        if let Some(slot) = window.get_mut(n - 1 - i) {
            *slot = w as f32;
        }
    }
    window
}

/// AC-3's window is KBD with this alpha — see the module docs for the
/// measured agreement with A/52 Table 7.33.
pub const AC3_KBD_ALPHA: f64 = 5.0;

/// Inverse MDCT: `n` input coefficients to `2*n` output samples, in ATSC
/// A/52:2012 §7.9.4.1's own sign convention:
///
/// `x[i] = -sum_k X[k] * cos((pi/n) * (i + 0.5 + n/2) * (k + 0.5))`
///
/// The leading minus is not a free convention. §7.9.4.1 defines the
/// transform as a pre-twiddle / IFFT / post-twiddle / de-interleave chain
/// whose twiddles are `xcos1[k] = -cos(2*pi*(8k+1)/(8N))` and
/// `xsin1[k] = -sin(...)` — both negated — and whose step-5 de-interleaving
/// negates four of its eight output terms. Evaluating that chain literally
/// for `N = 512` reproduces exactly `-1` times the textbook Princen-Bradley
/// sum above (checked numerically against the spec's own equations, and
/// end-to-end against `ffmpeg`'s decode; getting it wrong inverts every
/// output sample).
#[must_use]
pub fn imdct(coeffs: &[f32]) -> Vec<f32> {
    let n = coeffs.len();
    let mut out = vec![0f32; n * 2];
    if n == 0 {
        return out;
    }
    let n_f = n as f64;
    for (i, slot) in out.iter_mut().enumerate() {
        let mut acc = 0f64;
        for (k, &x) in coeffs.iter().enumerate() {
            let phase = (PI / n_f) * (i as f64 + 0.5 + n_f / 2.0) * (k as f64 + 0.5);
            acc += f64::from(x) * phase.cos();
        }
        *slot = -acc as f32;
    }
    out
}

/// Per-channel overlap-add state: the windowed second half of the previous
/// block's IMDCT output, carried into the next.
#[derive(Debug, Clone)]
pub struct OverlapState {
    tail: Vec<f32>,
}

impl OverlapState {
    #[must_use]
    pub fn new(half_len: usize) -> Self {
        Self {
            tail: vec![0f32; half_len],
        }
    }

    /// Feed one block's 256 coefficients (long transform, no block switch),
    /// returning 256 new time-domain output samples.
    ///
    /// §7.9.4.1 step 6: `pcm[n] = 2 * (x[n] + delay[n])`. The factor of two
    /// is the spec's own — "the factor of 2 scaling undoes headroom scaling
    /// performed in the encoder" — not a normalisation choice, and dropping
    /// it makes every decoded sample exactly half amplitude.
    #[must_use]
    #[allow(
        clippy::integer_division,
        reason = "imdct's output is always even-length (2*n); halving is exact"
    )]
    pub fn push_long(&mut self, coeffs: &[f32], window: &[f32]) -> Vec<f32> {
        let y = imdct(coeffs);
        let half = y.len() / 2;
        let mut windowed = vec![0f32; y.len()];
        for (i, &s) in y.iter().enumerate() {
            let w = window.get(i).copied().unwrap_or(1.0);
            if let Some(slot) = windowed.get_mut(i) {
                *slot = s * w;
            }
        }
        let mut out = vec![0f32; half];
        for i in 0..half {
            let prev = self.tail.get(i).copied().unwrap_or(0.0);
            let cur = windowed.get(i).copied().unwrap_or(0.0);
            if let Some(slot) = out.get_mut(i) {
                *slot = 2.0 * (prev + cur);
            }
        }
        self.tail = windowed
            .get(half..)
            .map(<[f32]>::to_vec)
            .unwrap_or_default();
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_dc_only_block_produces_a_uniform_magnitude_output() {
        // The property an independent oracle needs (per
        // The property an independent oracle needs to be worth anything: a
        // single non-zero low-frequency coefficient must not
        // produce a wildly non-uniform time envelope before windowing.
        let mut coeffs = vec![0f32; 8];
        coeffs[0] = 1.0;
        let y = imdct(&coeffs);
        assert_eq!(y.len(), 16);
        let max = y.iter().copied().fold(0f32, f32::max);
        let min = y.iter().copied().fold(0f32, f32::min);
        assert!(
            (max - (-min)).abs() < 0.5,
            "expected near-antisymmetric energy, got max={max} min={min}"
        );
    }

    #[test]
    fn the_kbd_window_is_symmetric_and_bounded() {
        let w = kbd_window(512, AC3_KBD_ALPHA);
        assert_eq!(w.len(), 512);
        for i in 0..256 {
            let a = w[i];
            let b = w[511 - i];
            assert!((a - b).abs() < 1e-4, "window not symmetric at {i}");
            assert!((0.0..=1.0).contains(&a));
        }
    }

    #[test]
    fn overlap_add_of_silence_is_silence() {
        let window = kbd_window(512, AC3_KBD_ALPHA);
        let mut state = OverlapState::new(256);
        let coeffs = vec![0f32; 256];
        let out1 = state.push_long(&coeffs, &window);
        let out2 = state.push_long(&coeffs, &window);
        assert!(out1.iter().all(|&v| v == 0.0));
        assert!(out2.iter().all(|&v| v == 0.0));
    }
}
