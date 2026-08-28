//! Windowing (spec section 4.3.1) and the inverse MDCT (spec section 4.3.7),
//! built on `vaco-tx`'s general MDCT rather than a codec-local transform —
//! `vaco-tx::TxKind::Mdct` already covers exactly what Vorbis needs
//! (`FULL_IMDCT` for the untruncated `n`-sample output), so there is no
//! reason to duplicate it here.
//!
//! `Vaco-Spec-Ref: vorbis-i sections 4.3.1, 4.3.7 and 4.3.8`

use std::collections::HashMap;
use std::sync::Arc;

use vaco_core::{Error, Result};
use vaco_tx::{Direction, Plan, Tx, TxFlags, TxKind};

/// Window generation (spec section 4.3.1). `n` is the current block's size;
/// `blocksize_0` is the stream's short blocksize, needed to shape a long
/// window's hybrid taper when it laps against a short neighbor.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "spec 4.3.1's window geometry (n/2, n/4, 3n/4) is defined as exact floor division on block sizes that are always powers of two"
)]
pub(crate) fn window(
    n: usize,
    blocksize_0: usize,
    long_block: bool,
    previous_long: bool,
    next_long: bool,
) -> Vec<f32> {
    let center = n / 2;
    let (left_start, left_end, left_n) = if long_block && !previous_long {
        (
            n / 4 - blocksize_0 / 4,
            n / 4 + blocksize_0 / 4,
            blocksize_0 / 2,
        )
    } else {
        (0, center, n / 2)
    };
    let (right_start, right_end, right_n) = if long_block && !next_long {
        (
            n * 3 / 4 - blocksize_0 / 4,
            n * 3 / 4 + blocksize_0 / 4,
            blocksize_0 / 2,
        )
    } else {
        (center, n, n / 2)
    };

    let mut w = vec![0f32; n];
    let half_pi = std::f64::consts::FRAC_PI_2;
    for (i, slot) in w.iter_mut().enumerate().take(left_end).skip(left_start) {
        let t = half_pi * (i as f64 - left_start as f64 + 0.5) / left_n.max(1) as f64;
        *slot = (half_pi * t.sin().powi(2)).sin() as f32;
    }
    for slot in w.get_mut(left_end..right_start).unwrap_or(&mut []) {
        *slot = 1.0;
    }
    for (i, slot) in w.iter_mut().enumerate().take(right_end).skip(right_start) {
        let t = half_pi * (i as f64 - right_start as f64 + 0.5) / right_n.max(1) as f64 + half_pi;
        *slot = (half_pi * t.sin().powi(2)).sin() as f32;
    }
    w
}

/// Cached IMDCT plans, keyed by transform length — a Vorbis stream only ever
/// uses two (`blocksize_0`, `blocksize_1`).
#[derive(Debug)]
pub(crate) struct Imdct {
    plans: HashMap<usize, Arc<Plan<f32>>>,
}

impl Imdct {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            plans: HashMap::new(),
        }
    }

    fn plan_for(&mut self, n: usize) -> Result<Arc<Plan<f32>>> {
        if let Some(p) = self.plans.get(&n) {
            return Ok(Arc::clone(p));
        }
        // `vaco-tx`'s MDCT kernel (see its own module doc) is the unnormalized
        // Σ x[n]·cos(...) form on both the forward and inverse sides, and its
        // own doc notes the inverse is literally the transpose of the DCT-IV
        // the forward pass uses. Applying it with no extra scale here is what
        // matches a real Vorbis encoder's amplitude convention: an earlier
        // attempt at an explicit `2/n` decoder-side normalization measured
        // exactly `n/2` too quiet against real `ffmpeg`-encoded output (a
        // clean 1024x on a 2048-sample long block), which is precisely the
        // scale the unnormalized DCT-IV's own self-composition introduces —
        // confirming no extra factor belongs here at all.
        let plan = Plan::<f32>::new(
            TxKind::Mdct,
            Direction::Inverse,
            n,
            1.0f32,
            TxFlags::FULL_IMDCT,
        )
        .map_err(|_| Error::InvalidData("vorbis: invalid transform length"))?;
        self.plans.insert(n, Arc::clone(&plan));
        Ok(plan)
    }

    /// Inverse MDCT: `coeffs` (length `n/2`) to `n` time-domain samples.
    #[allow(
        clippy::integer_division,
        reason = "n is always even (a power of two, per the identification header)"
    )]
    pub(crate) fn transform(&mut self, coeffs: &[f32], n: usize) -> Result<Vec<f32>> {
        let plan = self.plan_for(n)?;
        let mut tx = Tx::new(plan);
        let mut output = vec![0f32; n];
        let mut input = vec![0f32; n / 2];
        let m = input.len().min(coeffs.len());
        if let (Some(dst), Some(src)) = (input.get_mut(..m), coeffs.get(..m)) {
            dst.copy_from_slice(src);
        }
        tx.execute(&mut output, &input);
        Ok(output)
    }
}

impl Default for Imdct {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::float_cmp,
    reason = "test code: comparing against literal 0.0 the window function assigns exactly"
)]
mod tests {
    use super::*;

    #[test]
    fn short_window_is_symmetric_and_bounded() {
        let w = window(256, 256, false, false, false);
        assert_eq!(w.len(), 256);
        for &v in &w {
            assert!((0.0..=1.0).contains(&v));
        }
        // Symmetric about the center.
        for i in 0..128 {
            assert!((w[i] - w[255 - i]).abs() < 1e-5);
        }
    }

    #[test]
    fn long_window_hybrid_taper_zero_pads_outside_short_region() {
        let w = window(2048, 256, true, false, true);
        // Outside the short-block-matched region the left half must be zero.
        assert_eq!(w[0], 0.0);
        assert_eq!(w[2048 / 4 - 256 / 4 - 1], 0.0);
    }

    #[test]
    fn imdct_of_silence_is_silence() {
        let mut imdct = Imdct::new();
        let coeffs = vec![0f32; 512];
        let out = imdct.transform(&coeffs, 1024).unwrap();
        assert_eq!(out.len(), 1024);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn imdct_of_finite_input_is_finite() {
        let mut imdct = Imdct::new();
        let coeffs: Vec<f32> = (0..512).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = imdct.transform(&coeffs, 1024).unwrap();
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
