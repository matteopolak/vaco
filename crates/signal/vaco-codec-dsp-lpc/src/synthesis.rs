//! Fixed-point AR reconstruction: `sample = residual + (sum(coeff *
//! history) >> shift)`.
//!
//! Transcribed from IETF RFC 9639 §9.2.6: "each coefficient is multiplied
//! by its corresponding past sample, the results are summed, and this sum
//! is then shifted... the first predictor coefficient has to be multiplied
//! with the sample directly before the sample that is being predicted."

use crate::MAX_ORDER;

/// One prediction: `(sum(qcoeffs[i] * history[i]) ) >> shift`.
///
/// `history[0]` is the sample directly before the one being predicted,
/// `history[1]` the one before that, and so on — the ordering RFC 9639
/// §9.2.6 specifies. `shift` is clamped to `0..64` so an out-of-range value
/// (never produced by [`crate::quantize`], but this function does not
/// assume its caller used that function) narrows the shift instead of
/// panicking on an out-of-range shift amount.
///
/// The multiply-accumulate widens to `i128`: a real codec's coefficients
/// and history samples are both far inside `i32`'s useful range (FLAC caps
/// coefficient precision at 15 bits and PCM at 32), but this function takes
/// plain `i32` slices with no such promise attached, so it must also be
/// defined for `history`/`qcoeffs` full of `i32::MIN`/`i32::MAX` — exactly
/// what a fuzzer handed it. `32 * i32::MAX * i32::MAX` (`order <=
/// `[`crate::MAX_ORDER`]) is about 1.5e20, which overflows `i64` (max
/// ~9.2e18) but not `i128` (max ~1.7e38); the final result then saturates
/// into `i64` rather than wrapping, since a saturated-but-wrong prediction
/// on adversarial input is a far better failure mode than either a panic
/// or a silently wrapped value that looks like a plausible sample.
#[must_use]
pub fn predict(history: &[i32], qcoeffs: &[i32], shift: u32) -> i64 {
    let sum: i128 = qcoeffs
        .iter()
        .zip(history)
        .map(|(&c, &h)| i128::from(c) * i128::from(h))
        .sum();
    let shifted = sum >> shift.min(63);
    shifted.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Reconstructs a full block from `warmup` (the subframe's unencoded
/// leading samples, stored verbatim in the bitstream ahead of any residual)
/// and `residual` (one value per sample after the warm-up), writing
/// `warmup.len() + residual.len()` samples into `out` (truncated to
/// `out.len()` if shorter).
///
/// `qcoeffs.len()` is the predictor order and must be `<= warmup.len()`
/// for every output past the warm-up to have enough history; if it is not,
/// the missing history positions are treated as `0` rather than panicking
/// (matching "the bitstream shall not contain data" that violates this —
/// RFC 9639 does not define behaviour there either, so this is a
/// deliberate, documented choice rather than an attempt at conformance).
pub fn synthesize(warmup: &[i32], residual: &[i64], qcoeffs: &[i32], shift: u32, out: &mut [i64]) {
    let order = qcoeffs.len().min(MAX_ORDER);
    let mut out_iter = out.iter_mut();

    for &w in warmup {
        let Some(slot) = out_iter.next() else {
            return;
        };
        *slot = i64::from(w);
    }

    // `history[0]` tracks the most recently produced sample; shifted down
    // by one position after every step, oldest sample falling off the end.
    // `MAX_ORDER` is small (32) and this is the scalar reference, so a
    // full shift per sample is the right trade of simplicity against
    // speed — see the crate root doc on why this crate has no SIMD variant
    // yet.
    let mut history = [0i64; MAX_ORDER];
    for (slot, &w) in history.iter_mut().zip(warmup.iter().rev()) {
        *slot = i64::from(w);
    }

    for &r in residual {
        let Some(slot) = out_iter.next() else {
            return;
        };
        let mut hist32 = [0i32; MAX_ORDER];
        for (h32, h64) in hist32.iter_mut().zip(history.iter()).take(order) {
            *h32 = i32::try_from(*h64).unwrap_or(if *h64 > 0 { i32::MAX } else { i32::MIN });
        }
        let pred = predict(hist32.get(..order).unwrap_or(&[]), qcoeffs, shift);
        // Saturating, not wrapping, for the same reason `predict` saturates
        // its own internal sum: adversarial `residual` plus an
        // already-saturated `pred` must not panic under the fuzz profile's
        // overflow checks (see `predict`'s doc).
        let sample = r.saturating_add(pred);
        *slot = sample;

        for i in (1..order).rev() {
            if let (Some(prev), Some(cur)) = (
                history.get(i - 1).copied(),
                history.get_mut(i),
            ) {
                *cur = prev;
            }
        }
        if let Some(h0) = history.first_mut() {
            *h0 = sample;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predict_matches_hand_computed_dot_product() {
        // history[0]=10 (most recent), history[1]=20; coeffs [3, -2];
        // sum = 3*10 + (-2)*20 = -10; shift 0 -> -10.
        assert_eq!(predict(&[10, 20], &[3, -2], 0), -10);
    }

    #[test]
    fn predict_applies_the_shift() {
        // sum = 1*8 = 8; >> 2 = 2.
        assert_eq!(predict(&[8], &[1], 2), 2);
    }

    #[test]
    fn predict_shift_is_clamped_rather_than_panicking() {
        // Would panic on a bare `>>` for shift >= 64; must not here.
        assert_eq!(predict(&[1], &[1], 1000), 0);
    }

    #[test]
    fn synthesize_order_zero_is_pass_through_plus_residual() {
        // No coefficients: prediction is always 0, so output == residual
        // (after the warm-up, which there is none of here).
        let mut out = [0i64; 4];
        synthesize(&[], &[1, 2, 3, 4], &[], 0, &mut out);
        assert_eq!(out, [1, 2, 3, 4]);
    }

    #[test]
    fn synthesize_first_order_matches_hand_trace() {
        // order-1 predictor, coeff=1 (i.e. "predict the previous sample
        // exactly"), shift=0: warmup=[5], then each residual adds onto the
        // running prediction, i.e. this is a running sum starting at 5.
        let mut out = [0i64; 4];
        synthesize(&[5], &[1, 1, 1], &[1], 0, &mut out);
        assert_eq!(out, [5, 6, 7, 8]);
    }

    #[test]
    fn synthesize_second_order_matches_hand_trace() {
        // warmup = [1, 2]; coeffs = [2, -1], shift 0:
        //   pred(sample 2) = 2*history[0] - 1*history[1] = 2*2 - 1*1 = 3;
        //   sample 2 = residual[0] + 3.
        // With residual = [0, 0]: sample2 = 3, then
        //   pred(sample3) = 2*3 - 1*2 = 4; sample3 = 4.
        let mut out = [0i64; 4];
        synthesize(&[1, 2], &[0, 0], &[2, -1], 0, &mut out);
        assert_eq!(out, [1, 2, 3, 4]);
    }

    #[test]
    fn synthesize_never_panics_on_undersized_out() {
        let mut out = [0i64; 1];
        synthesize(&[1, 2, 3], &[4, 5, 6], &[1, 1, 1], 0, &mut out);
    }

    proptest::proptest! {
        #[test]
        fn synthesize_never_panics(
            warmup in proptest::collection::vec(proptest::num::i32::ANY, 0..8),
            residual in proptest::collection::vec(-1_000_000i64..1_000_000, 0..8),
            coeffs in proptest::collection::vec(proptest::num::i32::ANY, 0..8),
            shift in 0u32..40,
        ) {
            let mut out = vec![0i64; warmup.len() + residual.len()];
            synthesize(&warmup, &residual, &coeffs, shift, &mut out);
        }

        #[test]
        fn predict_never_panics(
            history in proptest::collection::vec(proptest::num::i32::ANY, 0..40),
            coeffs in proptest::collection::vec(proptest::num::i32::ANY, 0..40),
            shift in 0u32..1000,
        ) {
            let _ = predict(&history, &coeffs, shift);
        }
    }
}
