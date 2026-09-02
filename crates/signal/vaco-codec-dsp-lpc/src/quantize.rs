//! Float LPC coefficients to the small-integer-plus-shift form every
//! bitstream format that stores them actually transmits.

use crate::MAX_ORDER;

/// A quantised predictor: `order` integer coefficients plus one shared
/// right-shift, in the shape RFC 9639 §9.2.6 defines for FLAC's `LPC`
/// subframe (and which any other fixed-point AR predictor needs in the
/// same shape).
#[derive(Clone, Copy, Debug)]
pub struct QuantizedLpc {
    coeffs: [i32; MAX_ORDER],
    /// Number of coefficients actually populated; the rest of `coeffs` is
    /// zero.
    pub order: usize,
    /// Right-shift applied after the coefficient/history multiply-sum, in
    /// [`predict`](crate::predict) and [`synthesize`](crate::synthesize).
    pub shift: u32,
}

impl QuantizedLpc {
    /// The quantised coefficients, most-recent-sample-first (see
    /// [`crate::LevinsonDurbin::coefficients`]'s doc for the ordering).
    #[must_use]
    pub fn coefficients(&self) -> &[i32] {
        self.coeffs.get(..self.order).unwrap_or(&[])
    }
}

/// Quantise `coeffs` to `precision`-bit signed integers plus a shift,
/// choosing the largest shift that keeps every coefficient representable in
/// `precision` bits, and carrying each coefficient's rounding error forward
/// into the next one (error-feedback / noise-shaped rounding) so the sum of
/// quantisation errors stays bounded rather than growing with `order` — a
/// standard scalar-quantiser technique, not specific to any one format.
///
/// `precision` is clamped to `1..=31` (a 0-bit or wider-than-`i32`
/// coefficient is meaningless) and `coeffs.len()` to [`MAX_ORDER`]. An empty
/// or all-zero `coeffs` returns `order: 0`.
#[must_use]
pub fn quantize(coeffs: &[f64], precision: u32) -> QuantizedLpc {
    let order = coeffs.len().min(MAX_ORDER);
    let precision = precision.clamp(1, 31);
    let mut out = QuantizedLpc {
        coeffs: [0; MAX_ORDER],
        order,
        shift: 0,
    };
    if order == 0 {
        return out;
    }

    let max_abs = coeffs
        .iter()
        .take(order)
        .fold(0.0f64, |m, &c| m.max(c.abs()));
    if max_abs <= 0.0 || !max_abs.is_finite() {
        out.order = 0;
        return out;
    }

    // Largest shift such that round(max_abs << shift) still fits in a
    // signed `precision`-bit integer: max_abs * 2^shift <= 2^(precision-1) - 1.
    let limit = ((1i64 << (precision - 1)) - 1) as f64;
    let mut shift: i32 = 0;
    while shift < 31 && max_abs * 2.0f64.powi(shift + 1) <= limit {
        shift += 1;
    }
    // `shift` may legitimately be 0 (a coefficient already close to the
    // representable limit); it is never negative here because the loop only
    // ever increments from 0, matching RFC 9639 Appendix B.4's restriction
    // that the reference encoder (and every conforming decoder that only
    // reads what it writes) never needs a negative shift.
    let scale = 2.0f64.powi(shift);

    let lo = -(1i64 << (precision - 1));
    let hi = (1i64 << (precision - 1)) - 1;
    let mut carry = 0.0f64;
    for (dst, &c) in out.coeffs.iter_mut().take(order).zip(coeffs) {
        let ideal = c * scale + carry;
        let rounded = ideal.round();
        carry = ideal - rounded;
        let clamped = (rounded as i64).clamp(lo, hi);
        *dst = i32::try_from(clamped).unwrap_or(0);
    }
    out.shift = u32::try_from(shift).unwrap_or(0);
    out
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    clippy::indexing_slicing,
    reason = "test assertions comparing exact hand-derived values; an out-of-range index is itself a test failure"
)]
mod tests {
    use super::*;

    #[test]
    fn exact_powers_of_two_round_trip_without_error() {
        // 0.5 and 0.25 are exactly representable at any binary shift, so
        // quantisation should recover them exactly (mod the chosen shift).
        let q = quantize(&[0.5, 0.25], 15);
        assert_eq!(q.order, 2);
        let scale = (1i64 << q.shift) as f64;
        let recovered: Vec<f64> = q
            .coefficients()
            .iter()
            .map(|&c| f64::from(c) / scale)
            .collect();
        assert!((recovered[0] - 0.5).abs() < 1e-9);
        assert!((recovered[1] - 0.25).abs() < 1e-9);
    }

    #[test]
    fn coefficients_stay_within_precision() {
        let q = quantize(&[1.9, -1.9, 0.001, -0.9999], 12);
        let bound = 1i64 << 11; // precision=12 -> [-2048, 2047]
        for &c in q.coefficients() {
            assert!(
                i64::from(c) >= -bound && i64::from(c) < bound,
                "{c} out of range"
            );
        }
    }

    #[test]
    fn all_zero_input_yields_empty_predictor() {
        let q = quantize(&[0.0, 0.0, 0.0], 15);
        assert_eq!(q.order, 0);
    }

    #[test]
    fn empty_input_yields_empty_predictor() {
        let q = quantize(&[], 15);
        assert_eq!(q.order, 0);
    }

    #[test]
    fn order_is_capped_at_max_order() {
        let coeffs = vec![0.1; crate::MAX_ORDER + 10];
        let q = quantize(&coeffs, 12);
        assert_eq!(q.order, crate::MAX_ORDER);
    }

    proptest::proptest! {
        #[test]
        fn quantize_never_panics(
            coeffs in proptest::collection::vec(proptest::num::f64::ANY, 0..40),
            precision in 0u32..40,
        ) {
            let _ = quantize(&coeffs, precision);
        }
    }
}
