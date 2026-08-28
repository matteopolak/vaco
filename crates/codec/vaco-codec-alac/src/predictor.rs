//! The adaptive linear predictor.
//!
//! # Provenance
//!
//! This is this crate's **own design**, not a transcription of Apple's
//! `ALACSpecificConfig`-tuned predictor (see `provenance/vaco-codec-alac.toml`,
//! id `alac-payload-original`, and the crate doc's "what did not land"
//! section for why: there is no bitstream-syntax specification for ALAC's
//! actual prediction/entropy stage independent of Apple's reference source,
//! and reading that source to recover it would defeat the clean-room
//! boundary this crate exists to keep around the `alac` dev-dependency).
//!
//! A sign-sign LMS filter was chosen specifically because it needs **no
//! transmitted coefficients**: the encoder and decoder each start from the
//! same all-zero weights and apply the same deterministic update after every
//! sample, using only values already known to both sides (past
//! *reconstructed* samples, which are bit-exact because this codec is
//! lossless). That removes an entire class of possible encoder/decoder
//! mismatch — a coefficient quantised or transmitted inconsistently — at the
//! cost of slower convergence than a per-frame solved-and-transmitted FIR,
//! which is an efficiency trade, not a correctness one.

/// Weights are fixed-point with this many fractional bits.
const WEIGHT_SHIFT: u32 = 12;
/// Sign-sign LMS step size, in the same fixed-point units as the weights.
const MU: i64 = 2;
/// Largest predictor order a 5-bit header field can name.
pub(crate) const MAX_ORDER: usize = 32;

/// One channel's adaptive predictor state.
///
/// `history[0]` is the most recently seen reconstructed sample; `history[i]`
/// is `i` samples further back. Both encoder and decoder push exactly the
/// same values (the true, lossless sample, never the residual), so the two
/// stay in lock-step without any side channel.
#[derive(Debug, Clone)]
pub(crate) struct Predictor {
    order: usize,
    weights: [i64; MAX_ORDER],
    history: [i64; MAX_ORDER],
}

impl Predictor {
    /// A predictor of `order` taps (clamped to [`MAX_ORDER`]), all state
    /// zeroed — the same starting point the decoder always uses, so no
    /// warm-up transmission is needed: predictions are simply `0` (i.e. the
    /// coded value is the raw sample) until `order` real samples have been
    /// seen.
    #[must_use]
    pub(crate) fn new(order: usize) -> Self {
        Self {
            order: order.min(MAX_ORDER),
            weights: [0; MAX_ORDER],
            history: [0; MAX_ORDER],
        }
    }

    /// The fixed-point dot product of weights and history, in sample units
    /// (already shifted down by [`WEIGHT_SHIFT`]).
    fn predict(&self) -> i64 {
        let mut acc: i64 = 0;
        for i in 0..self.order {
            let w = self.weights.get(i).copied().unwrap_or(0);
            let h = self.history.get(i).copied().unwrap_or(0);
            acc = acc.wrapping_add(w.wrapping_mul(h));
        }
        acc >> WEIGHT_SHIFT
    }

    /// Sign-sign LMS update, then push `actual` onto the history.
    ///
    /// `error` is `actual - predicted`, computed by the caller (who needs it
    /// anyway, to code or apply the residual) so this never recomputes the
    /// prediction itself.
    fn adapt(&mut self, actual: i64, error: i64) {
        let step = match error.cmp(&0) {
            std::cmp::Ordering::Greater => MU,
            std::cmp::Ordering::Less => -MU,
            std::cmp::Ordering::Equal => 0,
        };
        if step != 0 {
            for i in 0..self.order {
                let h = self.history.get(i).copied().unwrap_or(0);
                let delta = match h.cmp(&0) {
                    std::cmp::Ordering::Greater => step,
                    std::cmp::Ordering::Less => -step,
                    std::cmp::Ordering::Equal => 0,
                };
                if let Some(w) = self.weights.get_mut(i) {
                    *w = w.wrapping_add(delta);
                }
            }
        }
        for i in (1..self.order).rev() {
            let prev = self.history.get(i - 1).copied().unwrap_or(0);
            if let Some(slot) = self.history.get_mut(i) {
                *slot = prev;
            }
        }
        if let Some(slot) = self.history.get_mut(0) {
            *slot = actual;
        }
    }

    /// Encode side: given the true sample, return the residual and update
    /// state exactly as [`Predictor::reconstruct`] will.
    pub(crate) fn residual(&mut self, actual: i64) -> i64 {
        let predicted = self.predict();
        let error = actual.wrapping_sub(predicted);
        self.adapt(actual, error);
        error
    }

    /// Decode side: given the coded residual, return the true sample and
    /// update state exactly as [`Predictor::residual`] did.
    pub(crate) fn reconstruct(&mut self, residual: i64) -> i64 {
        let predicted = self.predict();
        let actual = predicted.wrapping_add(residual);
        self.adapt(actual, residual);
        actual
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn residual_and_reconstruct_are_exact_inverses() {
        let signal: Vec<i64> = (0..500)
            .map(|i: i64| ((i * 37) % 251) - 125 + if i % 17 == 0 { 900 } else { 0 })
            .collect();
        for order in [0usize, 1, 2, 8, 31, 32, 64] {
            let mut enc = Predictor::new(order);
            let mut dec = Predictor::new(order);
            for &s in &signal {
                let r = enc.residual(s);
                let back = dec.reconstruct(r);
                assert_eq!(back, s, "order={order}");
            }
        }
    }

    #[test]
    fn zero_order_is_pass_through() {
        let mut enc = Predictor::new(0);
        assert_eq!(enc.residual(12345), 12345);
        assert_eq!(enc.residual(-999), -999);
    }
}
