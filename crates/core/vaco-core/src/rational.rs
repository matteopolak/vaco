//! Exact rational arithmetic.
//!
//! Time bases, frame rates and aspect ratios are all rationals, and they must be
//! exact: 30000/1001 is not 29.97, and accumulating a float error across a
//! two-hour timeline produces drift that shows up as audio/video desync.

use std::cmp::Ordering;

/// An exact rational number.
///
/// A zero denominator is permitted and meaningful — `1/0` is how an undefined or
/// infinite frame rate is expressed, matching what containers actually store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}

impl Rational {
    pub const ZERO: Self = Self { num: 0, den: 1 };
    pub const ONE: Self = Self { num: 1, den: 1 };
    /// Undefined: used for an unknown frame rate or aspect ratio.
    pub const UNDEFINED: Self = Self { num: 0, den: 0 };

    #[must_use]
    pub const fn new(num: i32, den: i32) -> Self {
        Self { num, den }
    }

    /// Reduce to lowest terms, normalising the sign onto the numerator.
    #[must_use]
    pub fn reduced(self) -> Self {
        todo!("P0-03 freeze: gcd reduction with sign normalisation")
    }

    #[must_use]
    pub fn is_defined(self) -> bool {
        self.den != 0
    }

    /// Lossy conversion, for display and heuristics only. Never for timestamps.
    #[must_use]
    pub fn to_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// Best rational approximation of `value` with denominator at most `max_den`.
    #[must_use]
    pub fn approximate(value: f64, max_den: i32) -> Self {
        let _ = (value, max_den);
        todo!("P0-03 freeze: continued-fraction approximation")
    }

    #[must_use]
    pub fn inverse(self) -> Self {
        Self {
            num: self.den,
            den: self.num,
        }
    }
}

/// Defaults to [`Rational::UNDEFINED`] (`0/0`), not zero.
///
/// A frame rate or aspect ratio that has not been set is *unknown*, which is a
/// different statement from "zero" — and `0/1` is a perfectly valid number that
/// would silently propagate through arithmetic. `0/0` cannot be mistaken for one.
impl Default for Rational {
    fn default() -> Self {
        Self::UNDEFINED
    }
}

impl std::ops::Mul for Rational {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        let _ = rhs;
        todo!("P0-03 freeze: reduce before multiplying to avoid overflow")
    }
}

impl std::ops::Div for Rational {
    type Output = Self;
    #[allow(
        clippy::suspicious_arithmetic_impl,
        reason = "division by a rational IS multiplication by its inverse"
    )]
    fn div(self, rhs: Self) -> Self {
        self * rhs.inverse()
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let _ = other;
        todo!("P0-03 freeze: compare via widening cross-multiplication, never floats")
    }
}
