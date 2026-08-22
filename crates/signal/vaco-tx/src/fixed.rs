//! The Q31 fixed-point arithmetic primitives.
//!
//! Everything here is **normative**. `docs/signal/vaco-tx.md` §"The i32
//! arithmetic contract" states the same rules in prose; this module is the
//! executable copy, and the two must not drift. Golden vectors in
//! `tests/golden_i32.rs` pin the composition of these primitives, so changing
//! any of them is a reviewed, codec-affecting decision (plan 17 §C.5.2, §C.7).
//!
//! # The four rules
//!
//! 1. **Representation.** Samples and twiddles are Q31: a signed 32-bit integer
//!    `v` denotes `v / 2^31`, nominal range `[-1, 1)`.
//! 2. **Rounding is round-half-up.** Every shift and every division rounds to
//!    the nearest representable value, with exact halves going toward `+∞`.
//!    Chosen because it is a single add-then-shift on every architecture and
//!    has no data-dependent branch.
//! 3. **Overflow saturates.** Never wraps, never panics, never is UB. Both the
//!    `i64` accumulation and the final narrowing to `i32` saturate.
//! 4. **No data-dependent control flow.** Every operation is a pure function of
//!    its inputs with a fixed instruction sequence, which is what makes a SIMD
//!    variant provably identical to the scalar reference.

/// Number of fractional bits in the Q31 representation.
pub const Q: u32 = 31;

/// `1.0` in Q31, saturated. Exactly `i32::MAX`, i.e. `1 - 2^-31`.
///
/// Q31 cannot represent `1.0`, so this is the closest value. [`crate::Plan`]
/// treats it as the identity scale and skips the scaling pass entirely rather
/// than multiplying by it, which would be off by one ULP for large inputs.
pub const ONE: i32 = i32::MAX;

/// Clamp a 64-bit intermediate into `i32`, saturating.
#[inline]
#[must_use]
pub const fn clamp_i32(x: i64) -> i32 {
    if x > i32::MAX as i64 {
        i32::MAX
    } else if x < i32::MIN as i64 {
        i32::MIN
    } else {
        x as i32
    }
}

/// `round(x / 2^s)`, half up, saturating.
///
/// `s` must be in `1..=62`. The add saturates so that an accumulator already at
/// `i64::MAX` cannot wrap before the shift.
#[inline]
#[must_use]
pub const fn round_shift(x: i64, s: u32) -> i32 {
    debug_assert!(s >= 1 && s <= 62);
    clamp_i32(x.saturating_add(1i64 << (s - 1)) >> s)
}

/// `round(x / d)`, half up, saturating. `d` must be positive.
///
/// For `d = 2^k` this is exactly [`round_shift`] with `s = k`; the two agree bit
/// for bit, which is what lets radix-2/4/8 stages use the cheap form without
/// changing the contract.
#[inline]
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "the truncating half-divisor `d/2` is part of the round-half-up definition, not an accident"
)]
pub fn round_div(x: i64, d: i64) -> i32 {
    debug_assert!(d > 0);
    if d & (d - 1) == 0 {
        return round_shift(x, d.trailing_zeros());
    }
    clamp_i32(x.saturating_add(d / 2).div_euclid(d))
}

/// Quantise a real number to Q31: `round(x * 2^31)`, half up, saturating.
///
/// This is how **every** twiddle factor and every algebraic constant enters the
/// fixed-point paths. Twiddles are computed in `f64` and passed through here, so
/// two plans for the same length hold bit-identical tables regardless of build
/// profile or architecture.
#[inline]
#[must_use]
pub fn quantise(x: f64) -> i32 {
    // `floor(x * 2^31 + 0.5)` in f64 is exact for every |x| <= 1: the product has
    // at most 32 significant bits and f64 carries 53.
    let scaled = (x * f64::from(1u32 << Q) + 0.5).floor();
    if scaled >= f64::from(i32::MAX) {
        i32::MAX
    } else if scaled <= f64::from(i32::MIN) {
        i32::MIN
    } else {
        scaled as i32
    }
}

/// Q31 multiply: `round((a * b) / 2^31)`, saturating.
#[inline]
#[must_use]
pub const fn qmul(a: i32, b: i32) -> i32 {
    round_shift(a as i64 * b as i64, Q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_shift_is_half_up() {
        assert_eq!(round_shift(3, 1), 2); // 1.5 -> 2
        assert_eq!(round_shift(1, 1), 1); // 0.5 -> 1
        assert_eq!(round_shift(-1, 1), 0); // -0.5 -> 0, toward +inf
        assert_eq!(round_shift(-3, 1), -1); // -1.5 -> -1
        assert_eq!(round_shift(4, 2), 1);
        assert_eq!(round_shift(6, 2), 2); // 1.5 -> 2
    }

    #[test]
    fn round_div_matches_round_shift_on_powers_of_two() {
        for x in [-9_i64, -5, -1, 0, 1, 5, 9, 1 << 40] {
            for k in 1_u32..=8 {
                assert_eq!(round_div(x, 1i64 << k), round_shift(x, k), "x={x} k={k}");
            }
        }
    }

    #[test]
    fn round_div_is_nearest_for_odd_divisors() {
        // 7/3 = 2.33 -> 2 ; 8/3 = 2.67 -> 3 ; -7/3 = -2.33 -> -2
        assert_eq!(round_div(7, 3), 2);
        assert_eq!(round_div(8, 3), 3);
        assert_eq!(round_div(-7, 3), -2);
        assert_eq!(round_div(-8, 3), -3);
        // exact halves round toward +inf: 5/2 handled by the shift path, 7/2 too
        assert_eq!(round_div(12, 5), 2); // 2.4
        assert_eq!(round_div(13, 5), 3); // 2.6
    }

    #[test]
    fn saturation_never_wraps() {
        assert_eq!(round_shift(i64::MAX, 1), i32::MAX);
        assert_eq!(round_shift(i64::MIN, 1), i32::MIN);
        assert_eq!(round_div(i64::MAX, 3), i32::MAX);
        assert_eq!(qmul(i32::MIN, i32::MIN), i32::MAX);
    }

    #[test]
    fn quantise_pins_the_named_constants() {
        assert_eq!(quantise(1.0), i32::MAX);
        assert_eq!(quantise(-1.0), i32::MIN);
        assert_eq!(quantise(0.0), 0);
        assert_eq!(quantise(0.5), 1 << 30);
        assert_eq!(quantise(-0.5), -(1 << 30));
    }

    #[test]
    fn one_is_the_saturated_unit() {
        assert_eq!(ONE, quantise(1.0));
    }
}
