//! Exact rational arithmetic.
//!
//! Time bases, frame rates and aspect ratios are all rationals, and they must be
//! exact: 30000/1001 is not 29.97, and accumulating a float error across a
//! two-hour timeline produces drift that shows up as audio/video desync.
//!
//! # The value model
//!
//! A [`Rational`] is a raw `i32/i32` pair, deliberately *not* kept in lowest
//! terms. The pair a container wrote is the pair we hand back, because a muxer
//! that has to reproduce a file byte for byte (D5) needs the authored `1001`,
//! not a helpfully reduced equivalent.
//!
//! Three classes of value exist, and every operation here handles all three
//! without panicking:
//!
//! | Class | Shape | Example | Meaning |
//! |---|---|---|---|
//! | finite | `den != 0` | `30000/1001` | the number `num / den` |
//! | infinite | `den == 0`, `num != 0` | `1/0` | ±∞ — a real frame rate that containers store |
//! | undefined | `0/0` | [`Rational::UNDEFINED`] | not known; also the [`Default`] |
//!
//! # Equality is by value, ordering is partial
//!
//! `1/2 == 2/4` and `1/0 == 7/0`, because equality compares the *number*, not
//! the field pair. [`Hash`] hashes the canonical reduced form, so the two
//! agree. To ask whether two rationals are the same literal pair, compare
//! `num` and `den` directly.
//!
//! Ordering is [`PartialOrd`] and not [`Ord`] because [`Rational::UNDEFINED`]
//! genuinely has no place on the number line: any comparison involving it
//! yields `None`. Where a total order is required — sorting, `BTreeMap` — use
//! [`Rational::cmp_exact`], which orders undefined below −∞.
//!
//! # Overflow
//!
//! Arithmetic runs in `i128` and reduces; when an exact result does not fit in
//! `i32/i32` the operator forms ([`Mul`](std::ops::Mul), [`Div`](std::ops::Div),
//! …) fall back to the closest representable rational, and the
//! `checked_*` forms return `None` instead. Nothing here panics and nothing
//! wraps.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use crate::Error;

/// An exact rational number.
///
/// A zero denominator is permitted and meaningful — `1/0` is how an infinite
/// frame rate is expressed, matching what containers actually store, and `0/0`
/// is "unknown". See the [module documentation](self) for the value model.
#[derive(Debug, Clone, Copy)]
pub struct Rational {
    /// Numerator, as authored. Not normalised.
    pub num: i32,
    /// Denominator, as authored. May be zero, and may be negative.
    pub den: i32,
}

impl Rational {
    /// `0/1`.
    pub const ZERO: Self = Self { num: 0, den: 1 };
    /// `1/1`.
    pub const ONE: Self = Self { num: 1, den: 1 };
    /// Undefined: used for an unknown frame rate or aspect ratio.
    pub const UNDEFINED: Self = Self { num: 0, den: 0 };
    /// `1/0` — positive infinity, the canonical "infinite frame rate".
    pub const INFINITY: Self = Self { num: 1, den: 0 };
    /// `-1/0` — negative infinity.
    pub const NEG_INFINITY: Self = Self { num: -1, den: 0 };
    /// `1/1000000`, the microsecond time base — the `AV_TIME_BASE` role.
    ///
    /// Spelled `TimeBase::MICROSECONDS` at every use site, via the
    /// [`TimeBase`](crate::TimeBase) alias.
    pub const MICROSECONDS: Self = Self {
        num: 1,
        den: 1_000_000,
    };

    /// Construct from a raw pair. Does **not** reduce and never panics.
    #[must_use]
    pub const fn new(num: i32, den: i32) -> Self {
        Self { num, den }
    }

    /// Reduce to lowest terms, normalising the sign onto the numerator.
    ///
    /// Undefined stays undefined; an infinity collapses to `±1/0`; zero becomes
    /// `0/1`.
    ///
    /// **Reduction never changes the value.** On the boundary inputs whose
    /// reduced form does not fit in `i32/i32` — those involving `i32::MIN`
    /// against a negative denominator — this returns `self` unchanged rather
    /// than the nearest representable rational.
    ///
    /// That matters more than it looks. This method previously approximated, on
    /// the reasoning that it cannot fail and something must be returned. A fuzz
    /// target found the consequence: `-1/i32::MIN` is exactly `1/2147483648`,
    /// which no `i32` denominator can hold, so it saturated to `1/2147483647`
    /// and became **equal to a genuinely different rational**. Any code that
    /// reduced before comparing then got the wrong answer, silently. Returning
    /// a not-fully-reduced but exact value is strictly safer: reduction is an
    /// optimisation, not a change of meaning.
    ///
    /// Use [`Rational::checked_reduced`] to detect the case explicitly.
    #[must_use]
    pub fn reduced(self) -> Self {
        let (n, d) = self.canonical();
        exact_from_canonical(n, d).unwrap_or(self)
    }

    /// [`Rational::reduced`], but `None` when the reduced value is not exactly
    /// representable as `i32/i32`.
    #[must_use]
    pub fn checked_reduced(self) -> Option<Self> {
        let (n, d) = self.canonical();
        exact_from_canonical(n, d)
    }

    /// Whether this is a finite number (`den != 0`).
    #[must_use]
    pub const fn is_defined(self) -> bool {
        self.den != 0
    }

    /// Whether this is [`Rational::UNDEFINED`] — `0/0`, in any spelling.
    #[must_use]
    pub const fn is_undefined(self) -> bool {
        self.den == 0 && self.num == 0
    }

    /// Whether this is ±∞ — a zero denominator with a non-zero numerator.
    #[must_use]
    pub const fn is_infinite(self) -> bool {
        self.den == 0 && self.num != 0
    }

    /// Whether this is exactly zero. `0/0` is not zero, it is unknown.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.num == 0 && self.den != 0
    }

    /// `-1`, `0` or `1`. Undefined has sign `0`.
    #[must_use]
    pub const fn signum(self) -> i32 {
        if self.den == 0 {
            self.num.signum()
        } else {
            self.num.signum() * self.den.signum()
        }
    }

    /// Lossy conversion, for display and heuristics only. Never for timestamps.
    ///
    /// `0/0` converts to `NaN` and `±n/0` to `±f64::INFINITY`, which is the
    /// arithmetic those values already mean.
    #[must_use]
    pub fn to_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// Best rational approximation of `value` with denominator at most
    /// `max_den`, by continued fractions.
    ///
    /// `NaN` maps to [`Rational::UNDEFINED`] and `±inf` to `±1/0`. `max_den` is
    /// clamped to at least 1. Magnitudes beyond `i32::MAX` saturate rather than
    /// wrap.
    #[must_use]
    pub fn approximate(value: f64, max_den: i32) -> Self {
        let max_den = i128::from(max_den.max(1));
        if value.is_nan() {
            return Self::UNDEFINED;
        }
        if value.is_infinite() {
            return if value > 0.0 {
                Self::INFINITY
            } else {
                Self::NEG_INFINITY
            };
        }
        let Some((n, d)) = f64_to_ratio(value, max_den) else {
            // Beyond the representable magnitude: saturate, do not wrap.
            return Self {
                num: if value < 0.0 { -i32::MAX } else { i32::MAX },
                den: 1,
            };
        };
        let (n, d) = approx_canonical(n, d, max_den);
        from_canonical(n, d)
    }

    /// Reduce `num/den` so that both fit in `i32` and the denominator is at
    /// most `max`.
    ///
    /// The `bool` is `true` when the result is exact. This is the workhorse
    /// behind every operator on this type, exposed because demuxers reducing a
    /// container-supplied 64-bit pair need exactly it.
    #[must_use]
    pub fn reduce(num: i64, den: i64, max: i64) -> (Self, bool) {
        let max = i128::from(max.clamp(1, i64::from(i32::MAX)));
        let (n, d) = canonicalise(i128::from(num), i128::from(den));
        if d != 0
            && d <= max
            && let Some(exact) = exact_from_canonical(n, d)
        {
            return (exact, true);
        }
        if d == 0 {
            return (from_canonical(n, d), true);
        }
        let (an, ad) = approx_canonical(n, d, max);
        (from_canonical(an, ad), an == n && ad == d)
    }

    /// The reciprocal, as a raw field swap. `0/0` stays undefined, `n/0`
    /// becomes `0/n` — zero — and zero becomes an infinity.
    #[must_use]
    pub const fn inverse(self) -> Self {
        Self {
            num: self.den,
            den: self.num,
        }
    }

    /// The reciprocal, or `None` for undefined and for zero.
    #[must_use]
    pub const fn checked_inverse(self) -> Option<Self> {
        if self.num == 0 {
            None
        } else {
            Some(self.inverse())
        }
    }

    /// Exact total order: undefined < −∞ < finite values < +∞.
    ///
    /// Total, and therefore usable as a `BTreeMap` key or a sort comparator,
    /// where [`PartialOrd`] cannot be because undefined has no position on the
    /// number line.
    #[must_use]
    pub fn cmp_exact(self, other: Self) -> Ordering {
        let (an, ad) = self.canonical();
        let (bn, bd) = other.canonical();
        match class(an, ad).cmp(&class(bn, bd)) {
            Ordering::Equal => {}
            other => return other,
        }
        if ad == 0 {
            // Same class and both non-finite: equal (±∞ or undefined).
            return Ordering::Equal;
        }
        // Both denominators are positive here, so cross-multiplication in i128
        // is order-preserving and cannot overflow: |n| <= 2^31, d <= 2^31.
        (an * bd).cmp(&(bn * ad))
    }

    /// Exact addition, or `None` when the result is not representable.
    #[must_use]
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        let (n, d) = add_canonical(self, rhs)?;
        exact_from_canonical(n, d)
    }

    /// Exact subtraction, or `None` when the result is not representable.
    #[must_use]
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.checked_add(rhs.neg_value())
    }

    /// Exact multiplication, or `None` when the result is not representable.
    #[must_use]
    pub fn checked_mul(self, rhs: Self) -> Option<Self> {
        let (n, d) = mul_canonical(self, rhs)?;
        exact_from_canonical(n, d)
    }

    /// Exact division, or `None` when the result is not representable.
    ///
    /// Dividing by zero yields an infinity, not `None`; dividing by undefined
    /// yields undefined. `None` means "the exact answer does not fit".
    #[must_use]
    pub fn checked_div(self, rhs: Self) -> Option<Self> {
        let (n, d) = mul_canonical(self, rhs.inverse())?;
        exact_from_canonical(n, d)
    }

    /// Negation that cannot overflow, used internally by subtraction.
    ///
    /// `-i32::MIN` would wrap, so when the numerator is `i32::MIN` the sign
    /// moves to the denominator instead — and when *both* are `i32::MIN` the
    /// value is exactly 1 and the answer is spelled out, because flipping
    /// either field would wrap back onto itself.
    const fn neg_value(self) -> Self {
        if self.num != i32::MIN {
            Self {
                num: -self.num,
                den: self.den,
            }
        } else if self.den != i32::MIN {
            Self {
                num: self.num,
                den: -self.den,
            }
        } else {
            Self { num: -1, den: 1 }
        }
    }

    /// Canonical `(num, den)` in `i128`: `den > 0` for a finite value, or
    /// `den == 0` with `num` in `{-1, 0, 1}` for ±∞ and undefined.
    fn canonical(self) -> (i128, i128) {
        canonicalise(i128::from(self.num), i128::from(self.den))
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

// ---------------------------------------------------------------- equality

/// Value equality, not field equality: `1/2 == 2/4`.
///
/// The alternative — comparing the raw pair — would make [`PartialOrd`]
/// inconsistent with [`PartialEq`], since cross-multiplication says those two
/// are the same number. Compare `num` and `den` yourself when you mean "the
/// same literal pair".
impl PartialEq for Rational {
    fn eq(&self, other: &Self) -> bool {
        self.cmp_exact(*other) == Ordering::Equal
    }
}

impl Eq for Rational {}

/// Hashes the canonical reduced form, so that `a == b` implies equal hashes.
impl Hash for Rational {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.canonical().hash(state);
    }
}

/// `None` exactly when either side is [`Rational::UNDEFINED`].
impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.is_undefined() || other.is_undefined() {
            return None;
        }
        Some(self.cmp_exact(*other))
    }
}

// -------------------------------------------------------------- arithmetic

impl std::ops::Neg for Rational {
    type Output = Self;
    fn neg(self) -> Self {
        self.neg_value()
    }
}

impl std::ops::Add for Rational {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        match add_canonical(self, rhs) {
            Some((n, d)) => from_canonical(n, d),
            None => Self::UNDEFINED,
        }
    }
}

impl std::ops::Sub for Rational {
    type Output = Self;
    #[allow(
        clippy::suspicious_arithmetic_impl,
        reason = "subtraction IS addition of the negation"
    )]
    fn sub(self, rhs: Self) -> Self {
        self + rhs.neg_value()
    }
}

impl std::ops::Mul for Rational {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        match mul_canonical(self, rhs) {
            Some((n, d)) => from_canonical(n, d),
            None => Self::UNDEFINED,
        }
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

// ----------------------------------------------------------- text formats

/// `num/den`, always both halves, always as stored.
impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

/// `"30000/1001"`, `"30000:1001"` or `"25"` — the **stored pair**, exactly.
///
/// # Not the same grammar as [`parse::rational`](crate::parse::rational)
///
/// This is a literal parser and the inverse of [`Display`](fmt::Display): what
/// you write is what you get, unreduced, so `"6/4"` is 6/4 and `1/0` is the
/// stored infinity. It is what you want for a config file, a test fixture, or
/// anything round-tripping through text.
///
/// `parse::rational` is the *CLI option* grammar, and it is a different thing:
/// it evaluates the whole string as an expression and then approximates, so
/// there `"6/4"` is 3/2 — matching the reference, where `-aspect 6/4` yields
/// 3:2. The two were the same function until the ratio grammar was found to be
/// expression-backed, at which point sharing one implementation meant either
/// this type stopped round-tripping or the CLI stopped matching.
impl FromStr for Rational {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let bad = || Error::InvalidData("rational");
        if let Some((n, d)) = s.split_once('/').or_else(|| s.split_once(':')) {
            let n = n.trim().parse().map_err(|_| bad())?;
            let d = d.trim().parse().map_err(|_| bad())?;
            return Ok(Self::new(n, d));
        }
        s.parse().map(|n| Self::new(n, 1)).map_err(|_| bad())
    }
}

// ---------------------------------------------------------------- internals

/// `0` for a finite value; the order key places undefined below −∞.
fn class(num: i128, den: i128) -> i32 {
    if den != 0 {
        0
    } else if num == 0 {
        -2
    } else if num > 0 {
        1
    } else {
        -1
    }
}

/// Greatest common divisor. `gcd(0, n) == n`.
#[must_use]
pub(crate) const fn gcd_u128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Put `(n, d)` in canonical form: `den > 0`, `gcd == 1`, or `den == 0` with
/// `num` in `{-1, 0, 1}`.
#[allow(
    clippy::integer_division,
    reason = "the divisor is a gcd of non-zero operands and therefore >= 1"
)]
fn canonicalise(n: i128, d: i128) -> (i128, i128) {
    if d == 0 {
        return (n.signum(), 0);
    }
    if n == 0 {
        return (0, 1);
    }
    let g = gcd_u128(n.unsigned_abs(), d.unsigned_abs()).cast_signed();
    let (mut n, mut d) = (n / g, d / g);
    if d < 0 {
        n = -n;
        d = -d;
    }
    (n, d)
}

/// Canonical pair back to a `Rational`, exactly or `None`.
fn exact_from_canonical(n: i128, d: i128) -> Option<Rational> {
    if d == 0 {
        return Some(Rational {
            num: n.signum() as i32,
            den: 0,
        });
    }
    Some(Rational {
        num: i32::try_from(n).ok()?,
        den: i32::try_from(d).ok()?,
    })
}

/// Canonical pair back to a `Rational`, approximating when it does not fit.
fn from_canonical(n: i128, d: i128) -> Rational {
    if let Some(exact) = exact_from_canonical(n, d) {
        return exact;
    }
    let (an, ad) = approx_canonical(n, d, i128::from(i32::MAX));
    Rational {
        num: an as i32,
        den: ad as i32,
    }
}

/// Multiply, cross-reducing first so that the exact product survives in `i128`
/// for the widest possible range of inputs.
///
/// `None` means the product is undefined: `0 × ∞`, or either side undefined.
fn mul_canonical(a: Rational, b: Rational) -> Option<(i128, i128)> {
    let (an, ad) = a.canonical();
    let (bn, bd) = b.canonical();
    if (an == 0 && ad == 0) || (bn == 0 && bd == 0) {
        return None; // undefined operand
    }
    if ad == 0 || bd == 0 {
        // An infinity times zero has no value; otherwise the sign carries.
        if an == 0 || bn == 0 {
            return None;
        }
        return Some(((an.signum() * bn.signum()), 0));
    }
    // Cross-reduce before multiplying: this is what keeps `1/1001 * 1001/1`
    // exact instead of routing it through a 62-bit intermediate.
    let (an, bd) = cross_reduce(an, bd);
    let (bn, ad) = cross_reduce(bn, ad);
    Some(canonicalise(an * bn, ad * bd))
}

/// Divide both by their gcd. Magnitudes here are at most `2^31`, so the
/// products the caller then forms cannot leave `i128`.
#[allow(
    clippy::integer_division,
    reason = "the divisor is a gcd of the operands and is at least 1"
)]
fn cross_reduce(a: i128, b: i128) -> (i128, i128) {
    if a == 0 || b == 0 {
        return (a, b);
    }
    let g = gcd_u128(a.unsigned_abs(), b.unsigned_abs()).cast_signed();
    (a / g, b / g)
}

/// `None` means the sum is undefined: an undefined operand, or `∞ + -∞`.
fn add_canonical(a: Rational, b: Rational) -> Option<(i128, i128)> {
    let (an, ad) = a.canonical();
    let (bn, bd) = b.canonical();
    if (an == 0 && ad == 0) || (bn == 0 && bd == 0) {
        return None;
    }
    match (ad, bd) {
        (0, 0) => {
            if an == bn {
                Some((an, 0))
            } else {
                None // +inf + -inf
            }
        }
        (0, _) => Some((an, 0)),
        (_, 0) => Some((bn, 0)),
        _ => {
            // |n| <= 2^31 and d <= 2^31, so both products are below 2^62 and
            // the sum below 2^63 — nowhere near the i128 ceiling.
            let (an, bn) = (an * bd, bn * ad);
            Some(canonicalise(an + bn, ad * bd))
        }
    }
}

/// Best rational approximation of the exact ratio `n/d` (with `d > 0`) subject
/// to `den <= max_den` and `|num| <= i32::MAX`, by continued fractions with
/// semiconvergent refinement.
///
/// Returned in canonical form. Callers guarantee `d > 0`.
#[allow(
    clippy::integer_division,
    reason = "every divisor is the strictly positive remainder of the previous step"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "the single letters are the standard notation of the continued-fraction recurrence"
)]
fn approx_canonical(n: i128, d: i128, max_den: i128) -> (i128, i128) {
    debug_assert!(d > 0);
    let max_num = i128::from(i32::MAX);
    let neg = n < 0;
    let (mut x, mut y) = (n.abs(), d);

    // h/k are the convergents; index 1 is the most recent accepted one.
    let (mut h0, mut h1) = (0i128, 1i128);
    let (mut k0, mut k1) = (1i128, 0i128);
    let mut accepted = false;

    while y != 0 {
        let a = x / y;
        let h = a * h1 + h0;
        let k = a * k1 + k0;
        if h > max_num || k > max_den {
            // The full convergent is out of budget. The best rational under the
            // budget is then a *semiconvergent*: the same step taken only `t`
            // times, for the largest `t` that still fits.
            if accepted {
                let t_den = if k1 == 0 {
                    a
                } else {
                    ((max_den - k0) / k1).min(a)
                };
                let t_num = if h1 == 0 {
                    t_den
                } else {
                    ((max_num - h0) / h1).min(t_den)
                };
                let t = t_num.max(0);
                if t > 0 {
                    let hs = h0 + t * h1;
                    let ks = k0 + t * k1;
                    if closer(n.abs(), d, hs, ks, h1, k1) {
                        h1 = hs;
                        k1 = ks;
                    }
                }
            }
            break;
        }
        h0 = h1;
        h1 = h;
        k0 = k1;
        k1 = k;
        accepted = true;
        let r = x - a * y;
        x = y;
        y = r;
    }

    if !accepted || k1 == 0 {
        // The value's integer part alone exceeds the numerator budget.
        return (if neg { -max_num } else { max_num }, 1);
    }
    (if neg { -h1 } else { h1 }, k1)
}

/// Is `p/q` a closer approximation of `n/d` than `r/s` is? All non-negative,
/// `d`, `q`, `s` positive. Exact — no floats.
#[allow(
    clippy::many_single_char_names,
    reason = "the single letters are the standard notation of the continued-fraction recurrence"
)]
fn closer(n: i128, d: i128, p: i128, q: i128, r: i128, s: i128) -> bool {
    // |n/d - p/q| < |n/d - r/s|  <=>  |n*q - p*d| * s < |n*s - r*d| * q,
    // after multiplying both sides by the positive d*q*s.
    let ea = (n * q - p * d).abs();
    let eb = (n * s - r * d).abs();
    match ea.checked_mul(s).zip(eb.checked_mul(q)) {
        Some((la, lb)) => la < lb,
        None => false,
    }
}

/// Decompose a finite `f64` into an exact ratio `n/d` with `d` a power of two.
///
/// `None` when the magnitude is beyond what a `Rational` can hold, or so small
/// that no fraction with `den <= max_den` is nearer to it than zero — the
/// caller turns the first into a saturation and the second never reaches here,
/// because the returned ratio is `0/1`.
fn f64_to_ratio(value: f64, max_den: i128) -> Option<(i128, i128)> {
    if value == 0.0 {
        return Some((0, 1));
    }
    let mag = value.abs();
    if mag > f64::from(i32::MAX) {
        return None;
    }
    // Below half a step of the finest grid the bound allows, the answer is zero
    // and the exponent would not fit anyway.
    if mag * (max_den as f64) < 0.5 {
        return Some((0, 1));
    }
    let bits = value.to_bits();
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (mantissa, exp) = if raw_exp == 0 {
        (frac, -1074i32)
    } else {
        (frac | (1u64 << 52), raw_exp - 1075)
    };
    let m = i128::from(mantissa);
    let signed = if value < 0.0 { -m } else { m };
    if exp >= 0 {
        if exp > 100 {
            return None;
        }
        Some((signed << exp, 1))
    } else {
        let shift = -exp;
        if shift > 110 {
            // Defensive: excluded by the magnitude test above.
            return Some((0, 1));
        }
        Some((signed, 1i128 << shift))
    }
}
