//! Timestamps and time bases.
//!
//! Every timestamp in Vaco is an integer count of ticks in an explicit
//! [`TimeBase`]. There is no ambient "seconds" representation, because the
//! commonest class of bug in a media tool is a timestamp interpreted in the
//! wrong base, and a type that carries its own base makes that a compile error
//! rather than a silent desync.
//!
//! # Rescaling
//!
//! Converting a tick count from one base to another is `ticks × from ÷ to`,
//! which is `ticks × from.num × to.den ÷ (from.den × to.num)`. Every one of
//! those operands is at most 63 bits, so the product is computed in `i128`
//! where it cannot overflow, and the single division at the end is the only
//! place precision is lost. That is why [`Rounding`] is a required argument
//! rather than a default: a muxer placing a chunk boundary and a decoder
//! reporting a presentation time want different answers from the same input.
//!
//! Nothing in this module goes through `f64`. Comparing two timestamps in
//! different bases cross-multiplies in `i128` instead, so `1/90000` and
//! `1/1001` streams order exactly rather than nearly.

use std::cmp::Ordering;
use std::fmt;

use crate::Rational;

/// The unit one timestamp tick represents, in seconds.
///
/// A stream at 90 kHz has a time base of `1/90000`; a 25 fps video track often
/// uses `1/25`. This is a [`Rational`], so [`TimeBase::MICROSECONDS`] and the
/// rest of that type's vocabulary apply.
pub type TimeBase = Rational;

/// A point in time, in ticks of some [`TimeBase`] tracked by the owning stream.
///
/// `None` models an absent timestamp — genuinely common in real media, and the
/// reason this is an `Option` newtype rather than a sentinel value. Sentinels
/// get compared, printed and arithmetic'd by accident; `None` cannot be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Timestamp(Option<i64>);

impl Timestamp {
    /// The absent timestamp, and the [`Default`].
    pub const NONE: Self = Self(None);
    /// Tick zero.
    pub const ZERO: Self = Self(Some(0));

    #[must_use]
    pub const fn new(ticks: i64) -> Self {
        Self(Some(ticks))
    }

    #[must_use]
    pub const fn ticks(self) -> Option<i64> {
        self.0
    }

    #[must_use]
    pub const fn is_some(self) -> bool {
        self.0.is_some()
    }

    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0.is_none()
    }

    /// Offset by a tick count in the same base. Absent stays absent; overflow
    /// saturates rather than wrapping.
    #[must_use]
    pub const fn offset(self, delta: i64) -> Self {
        match self.0 {
            Some(t) => Self(Some(t.saturating_add(delta))),
            None => Self::NONE,
        }
    }

    /// Convert between time bases with explicit rounding.
    ///
    /// Rescaling is where precision is lost, so the rounding mode is a required
    /// argument: a muxer writing a chunk boundary and a decoder reporting a
    /// presentation time want different answers.
    ///
    /// Returns [`Timestamp::NONE`] when this timestamp is absent or when either
    /// base is unusable — undefined, infinite, or a `to` base with a zero
    /// numerator, none of which name a tick length. A result that does not fit
    /// in `i64` saturates; use [`Timestamp::checked_rescale`] to observe that.
    #[must_use]
    pub fn rescale(self, from: TimeBase, to: TimeBase, rounding: Rounding) -> Self {
        let Some(ticks) = self.0 else {
            return Self::NONE;
        };
        let Some((b, c)) = rescale_factors(from, to) else {
            return Self::NONE;
        };
        match muldiv_rnd(i128::from(ticks), b, c, rounding) {
            Some(v) => Self(Some(clamp_i64(v))),
            None => Self::NONE,
        }
    }

    /// [`Timestamp::rescale`], but `None` when the result does not fit in `i64`.
    ///
    /// The distinction matters to a muxer: a saturated timestamp is a plausible
    /// number that is wrong, where `None` is a fact you can act on.
    #[must_use]
    pub fn checked_rescale(self, from: TimeBase, to: TimeBase, rounding: Rounding) -> Option<Self> {
        let ticks = self.0?;
        let (b, c) = rescale_factors(from, to)?;
        let v = muldiv_rnd(i128::from(ticks), b, c, rounding)?;
        i64::try_from(v).ok().map(Self::new)
    }

    /// Compare two timestamps that may be in different bases.
    ///
    /// `None` when either timestamp is absent or either base is unusable.
    /// Exact: the comparison cross-multiplies in `i128` and never converts to
    /// seconds, so no rounding decision is embedded in the answer.
    #[must_use]
    pub fn compare(
        self,
        self_base: TimeBase,
        other: Self,
        other_base: TimeBase,
    ) -> Option<Ordering> {
        let (a, b) = (self.0?, other.0?);
        let (an, ad) = finite_base(self_base)?;
        let (bn, bd) = finite_base(other_base)?;
        // Seconds are a*an/ad and b*bn/bd with ad, bd > 0; multiplying both
        // sides by ad*bd preserves order. |a| < 2^63, |an| <= 2^31, bd <= 2^31,
        // so each side is below 2^125 and i128 has room to spare.
        let lhs = i128::from(a) * an * bd;
        let rhs = i128::from(b) * bn * ad;
        Some(lhs.cmp(&rhs))
    }

    /// Seconds, for display and heuristics only. `None` when absent or when the
    /// base is unusable.
    #[must_use]
    pub fn to_seconds(self, base: TimeBase) -> Option<f64> {
        let ticks = self.0?;
        if !base.is_defined() {
            return None;
        }
        Some(ticks as f64 * base.to_f64())
    }

    /// Convert native ticks to an exact seconds duration without rounding.
    #[must_use]
    pub fn to_duration(self, base: TimeBase) -> Option<Duration> {
        Duration::from_ticks(self.0?, base)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(t) => write!(f, "{t}"),
            None => f.write_str("N/A"),
        }
    }
}

impl From<i64> for Timestamp {
    fn from(ticks: i64) -> Self {
        Self::new(ticks)
    }
}

impl From<Option<i64>> for Timestamp {
    fn from(ticks: Option<i64>) -> Self {
        Self(ticks)
    }
}

/// How a rescale that cannot be represented exactly should round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rounding {
    /// Truncate towards zero.
    Zero,
    /// Away from zero.
    Infinity,
    /// Towards negative infinity (floor).
    Down,
    /// Towards positive infinity (ceiling).
    Up,
    /// Round to nearest, ties away from zero. The usual choice for presentation
    /// timestamps.
    #[default]
    NearestAwayFromZero,
}

impl Rounding {
    /// Every mode, for exhaustive testing.
    pub const ALL: [Self; 5] = [
        Self::Zero,
        Self::Infinity,
        Self::Down,
        Self::Up,
        Self::NearestAwayFromZero,
    ];

    /// The largest possible `|exact - rounded|`, in units of the output tick.
    ///
    /// One tick for the directed modes, half a tick for round-to-nearest. The
    /// round-trip property tests are stated against this number.
    #[must_use]
    pub const fn max_error_ticks(self) -> f64 {
        match self {
            Self::NearestAwayFromZero => 0.5,
            _ => 1.0,
        }
    }
}

/// Exact `a × b ÷ c` with `i128` intermediates and the requested rounding.
///
/// `None` when `c == 0` or when the result does not fit in `i64`. This is the
/// primitive [`Timestamp::rescale`] is built from, exposed because format code
/// rescales things that are not timestamps — durations, byte positions, and
/// chunk offsets.
#[must_use]
pub fn rescale_rnd(a: i64, b: i64, c: i64, rounding: Rounding) -> Option<i64> {
    let v = muldiv_rnd(i128::from(a), i128::from(b), i128::from(c), rounding)?;
    i64::try_from(v).ok()
}

/// An exact span of time, stored as reduced rational seconds.
///
/// Native ticks retain their time base through `from_ticks`. Rounding happens
/// only at explicit integer-tick or microsecond conversion boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Duration {
    numerator: i128,
    denominator: i128,
}

/// Compatibility name for the exact representation now used by `Duration`.
pub type ExactDuration = Duration;

impl Duration {
    /// Zero length.
    pub const ZERO: Self = Self {
        numerator: 0,
        denominator: 1,
    };

    /// From a microsecond count, without loss.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self::from_ratio(micros as i128, 1_000_000)
    }

    /// Parse a base-10 seconds value without passing through binary floating point.
    ///
    /// Playlist syntaxes use decimal seconds; preserving their written digits
    /// avoids silently collapsing sub-microsecond segment boundaries.
    #[must_use]
    pub fn from_decimal_seconds(value: &str) -> Option<Self> {
        let (negative, value) = value
            .strip_prefix('-')
            .map_or((false, value), |rest| (true, rest));
        let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
        if whole.is_empty() && fraction.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let whole = if whole.is_empty() {
            0
        } else {
            whole.parse::<i128>().ok()?
        };
        let fraction_digits = fraction.len();
        let fraction = if fraction.is_empty() {
            0
        } else {
            fraction.parse::<i128>().ok()?
        };
        let scale = (0..fraction_digits).try_fold(1_i128, |scale, _| scale.checked_mul(10))?;
        let numerator = whole.checked_mul(scale)?.checked_add(fraction)?;
        Some(Self::from_ratio(
            if negative {
                numerator.checked_neg()?
            } else {
                numerator
            },
            scale,
        ))
    }

    /// Build an exact duration from ticks in `time_base`.
    #[must_use]
    pub fn from_ticks(ticks: i64, time_base: TimeBase) -> Option<Self> {
        let (num, den) = finite_base(time_base)?;
        Some(Self::from_ratio(i128::from(ticks) * num, den))
    }

    /// Build an exact seconds fraction when the denominator is positive.
    ///
    /// Container timescales may exceed [`TimeBase`]'s `i32` storage while
    /// still fitting the duration representation, so they use this path.
    #[must_use]
    pub fn from_fraction(numerator: i128, denominator: i128) -> Option<Self> {
        (denominator > 0).then(|| Self::from_ratio(numerator, denominator))
    }

    /// Preserve a duration. Kept for callers of the former `ExactDuration` type.
    #[must_use]
    pub const fn from_duration(duration: Self) -> Self {
        duration
    }

    /// Return the reduced seconds ratio as `(numerator, denominator)`.
    #[must_use]
    pub const fn as_ratio(self) -> (i128, i128) {
        (self.numerator, self.denominator)
    }

    /// Microseconds, rounded to nearest and saturating at the `i64` bounds.
    ///
    /// This is a legacy/display boundary, not an intermediate representation.
    /// Use `checked_micros` when overflow or the rounding direction matters.
    #[must_use]
    pub fn as_micros(self) -> i64 {
        muldiv_rnd(
            self.numerator,
            1_000_000,
            self.denominator,
            Rounding::default(),
        )
        .map_or_else(
            || {
                if self.numerator < 0 {
                    i64::MIN
                } else {
                    i64::MAX
                }
            },
            clamp_i64,
        )
    }

    /// Convert to microseconds with explicit rounding, refusing overflow.
    #[must_use]
    pub fn checked_micros(self, rounding: Rounding) -> Option<i64> {
        muldiv_rnd(self.numerator, 1_000_000, self.denominator, rounding)
            .and_then(|micros| i64::try_from(micros).ok())
    }

    /// Seconds, for display and heuristics only.
    #[must_use]
    pub fn as_secs_f64(self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }

    /// Convert to ticks in `base`, rounding to nearest and refusing overflow.
    #[must_use]
    pub fn to_ticks(self, base: TimeBase) -> Option<i64> {
        self.to_ticks_rounding(base, Rounding::default())
    }

    /// Convert to ticks with explicit rounding, refusing invalid bases or overflow.
    #[must_use]
    pub fn to_ticks_rounding(self, base: TimeBase, rounding: Rounding) -> Option<i64> {
        let (num, den) = finite_base(base)?;
        muldiv_rnd(
            self.numerator,
            den,
            self.denominator.checked_mul(num)?,
            rounding,
        )
        .and_then(|ticks| i64::try_from(ticks).ok())
    }

    /// Round to a microsecond-valued duration.
    ///
    /// Compatibility with the former `ExactDuration` conversion; new callers
    /// should retain this exact value or request `checked_micros` at their sink.
    #[must_use]
    pub fn to_duration(self, rounding: Rounding) -> Option<Self> {
        self.checked_micros(rounding).map(Self::from_micros)
    }

    /// Add exact seconds, refusing an intermediate or result outside `i128`.
    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.checked_combine(other, false)
    }

    /// Subtract exact seconds, refusing an intermediate or result outside `i128`.
    #[must_use]
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.checked_combine(other, true)
    }

    #[allow(
        clippy::integer_division,
        reason = "gcd factors divide canonical denominators and numerators exactly"
    )]
    fn checked_combine(self, other: Self, subtract: bool) -> Option<Self> {
        let gcd = gcd_i128(self.denominator, other.denominator);
        let lhs = self.numerator.checked_mul(other.denominator / gcd)?;
        let rhs = other.numerator.checked_mul(self.denominator / gcd)?;
        let numerator = if subtract {
            lhs.checked_sub(rhs)?
        } else {
            lhs.checked_add(rhs)?
        };
        if numerator == 0 {
            return Some(Self::ZERO);
        }
        let reduction = gcd_i128((numerator % gcd).abs(), gcd);
        let denominator = (self.denominator / gcd).checked_mul(other.denominator / reduction)?;
        Some(Self::from_ratio(numerator / reduction, denominator))
    }

    #[allow(
        clippy::integer_division,
        reason = "canonical rational reduction divides both terms by their gcd"
    )]
    const fn from_ratio(numerator: i128, denominator: i128) -> Self {
        debug_assert!(denominator > 0);
        if numerator == 0 {
            return Self::ZERO;
        }
        // Reducing the remainder avoids taking abs(i128::MIN).
        let gcd = gcd_i128((numerator % denominator).abs(), denominator);
        Self {
            numerator: numerator / gcd,
            denominator: denominator / gcd,
        }
    }
}

impl Default for Duration {
    fn default() -> Self {
        Self::ZERO
    }
}

impl PartialOrd for Duration {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Duration {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.numerator.signum(), other.numerator.signum()) {
            (a, b) if a != b => a.cmp(&b),
            (0, 0) => Ordering::Equal,
            (1, 1) => cmp_nonnegative_ratios(
                self.numerator.unsigned_abs(),
                self.denominator as u128,
                other.numerator.unsigned_abs(),
                other.denominator as u128,
            ),
            (-1, -1) => cmp_nonnegative_ratios(
                other.numerator.unsigned_abs(),
                other.denominator as u128,
                self.numerator.unsigned_abs(),
                self.denominator as u128,
            ),
            _ => unreachable!("signs were checked above"),
        }
    }
}

/// The canonical rendering — signed seconds with exactly six decimals — the
/// same text [`crate::parse::format_duration`] produces.
impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::parse::format_duration(*self))
    }
}

// ---------------------------------------------------------------- internals

/// `(num, den)` of a base that actually names a tick length, with `den > 0`.
///
/// Rejects undefined and infinite bases, and a zero numerator, none of which
/// do. Returned widened so that the caller's products cannot overflow.
fn finite_base(tb: TimeBase) -> Option<(i128, i128)> {
    if tb.den == 0 || tb.num == 0 {
        return None;
    }
    let (n, d) = (i128::from(tb.num), i128::from(tb.den));
    if d < 0 { Some((-n, -d)) } else { Some((n, d)) }
}

/// Compare non-negative fractions without cross-multiplication.
///
/// The continued-fraction form keeps every intermediate below `u128`, so an
/// exact value remains orderable even if a future constructor permits wider
/// numerators than the current tick-based constructors do.
#[allow(
    clippy::integer_division,
    reason = "continued-fraction comparison intentionally divides integers without rounding"
)]
fn cmp_nonnegative_ratios(mut a: u128, mut b: u128, mut c: u128, mut d: u128) -> Ordering {
    let mut reverse = false;
    loop {
        let left = a / b;
        let right = c / d;
        if left != right {
            return if reverse {
                right.cmp(&left)
            } else {
                left.cmp(&right)
            };
        }

        let left_rem = a % b;
        let right_rem = c % d;
        match (left_rem == 0, right_rem == 0) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if reverse {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if reverse {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (false, false) => {
                (a, b, c, d) = (b, left_rem, d, right_rem);
                reverse = !reverse;
            }
        }
    }
}

/// The `(b, c)` of `ticks × b ÷ c` that converts `from` ticks into `to` ticks.
fn rescale_factors(from: TimeBase, to: TimeBase) -> Option<(i128, i128)> {
    let (fnum, fden) = finite_base(from)?;
    let (tnum, tden) = finite_base(to)?;
    Some((fnum * tden, fden * tnum))
}

/// `a × b ÷ c`, rounded. `None` for zero divisors or unrepresentable results.
///
/// Rational aggregates can have products wider than the result's storage;
/// modular multiplication retains their quotient and remainder without overflow.
#[allow(
    clippy::integer_division,
    reason = "nonzero divisors are checked before quotient/remainder arithmetic"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "a, b, c, q and r are the standard names for the operands and the quotient/remainder"
)]
fn muldiv_rnd(a: i128, b: i128, c: i128, rounding: Rounding) -> Option<i128> {
    if c == 0 {
        return None;
    }
    let negative = (a < 0) ^ (b < 0) ^ (c < 0);
    let denominator = c.unsigned_abs();
    let (mut quotient, remainder) =
        unsigned_muldiv(a.unsigned_abs(), b.unsigned_abs(), denominator)?;
    if remainder != 0 {
        let away = match rounding {
            Rounding::Zero => false,
            Rounding::Infinity => true,
            Rounding::Down => negative,
            Rounding::Up => !negative,
            Rounding::NearestAwayFromZero => remainder >= denominator - remainder,
        };
        if away {
            quotient = quotient.checked_add(1)?;
        }
    }
    if negative && quotient == i128::MIN.unsigned_abs() {
        Some(i128::MIN)
    } else {
        let result = i128::try_from(quotient).ok()?;
        Some(if negative { -result } else { result })
    }
}

/// Divide a product without requiring that the product itself fit in u128.
#[allow(
    clippy::integer_division,
    reason = "quotient and remainder are retained separately for explicit rounding"
)]
fn unsigned_muldiv(a: u128, b: u128, denominator: u128) -> Option<(u128, u128)> {
    if let Some(product) = a.checked_mul(b) {
        return Some((product / denominator, product % denominator));
    }
    let whole = (a / denominator).checked_mul(b)?;
    let addend = a % denominator;
    let mut quotient = 0_u128;
    let mut remainder = 0_u128;
    for bit in (0..u128::BITS).rev() {
        quotient = quotient.checked_mul(2)?;
        // Each modular addition stays below denominator, even when doubling
        // a remainder would overflow the storage type.
        if remainder >= denominator - remainder {
            remainder -= denominator - remainder;
            quotient = quotient.checked_add(1)?;
        } else {
            remainder += remainder;
        }
        if (b >> bit) & 1 != 0 {
            if remainder >= denominator - addend {
                remainder -= denominator - addend;
                quotient = quotient.checked_add(1)?;
            } else {
                remainder += addend;
            }
        }
    }
    Some((whole.checked_add(quotient)?, remainder))
}

/// Saturating narrowing to `i64`.
fn clamp_i64(v: i128) -> i64 {
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

const fn gcd_i128(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    if a > 1 { a } else { 1 }
}
