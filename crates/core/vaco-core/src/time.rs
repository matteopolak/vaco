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

    /// Convert to a microsecond [`Duration`], with round-to-nearest.
    #[must_use]
    pub fn to_duration(self, base: TimeBase) -> Option<Duration> {
        self.checked_rescale(base, TimeBase::MICROSECONDS, Rounding::default())
            .and_then(Timestamp::ticks)
            .map(Duration)
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

/// A span of time in ticks of a [`TimeBase`].
///
/// The `parse::duration` grammar and the `duration` option type both produce
/// **microseconds**, which is the unit this carries wherever a base is not
/// stated alongside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Duration(pub i64);

impl Duration {
    /// Zero length.
    pub const ZERO: Self = Self(0);

    /// From a microsecond count.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// The microsecond count.
    #[must_use]
    pub const fn as_micros(self) -> i64 {
        self.0
    }

    /// Seconds, for display and heuristics only.
    #[must_use]
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1e6
    }

    /// Reinterpret as a tick count in `base`, rounding to nearest.
    #[must_use]
    pub fn to_ticks(self, base: TimeBase) -> Option<i64> {
        Timestamp::new(self.0)
            .checked_rescale(TimeBase::MICROSECONDS, base, Rounding::default())
            .and_then(Timestamp::ticks)
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

/// The `(b, c)` of `ticks × b ÷ c` that converts `from` ticks into `to` ticks.
fn rescale_factors(from: TimeBase, to: TimeBase) -> Option<(i128, i128)> {
    let (fnum, fden) = finite_base(from)?;
    let (tnum, tden) = finite_base(to)?;
    Some((fnum * tden, fden * tnum))
}

/// `a × b ÷ c`, rounded. `None` when `c == 0`.
///
/// Callers pass values derived from an `i64` and two `i32`s, so `a * b` is at
/// most 2^125 and stays inside `i128`.
#[allow(
    clippy::integer_division,
    reason = "c is checked non-zero on the line above; this is the one division the module makes"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "a, b, c, q and r are the standard names for the operands and the quotient/remainder"
)]
fn muldiv_rnd(a: i128, b: i128, c: i128, rounding: Rounding) -> Option<i128> {
    if c == 0 {
        return None;
    }
    let n = a.checked_mul(b)?;
    // Normalise the divisor positive so the rounding cases only have to reason
    // about the sign of the dividend.
    let (n, d) = if c < 0 { (-n, -c) } else { (n, c) };
    let q = n / d;
    let r = n % d;
    if r == 0 {
        return Some(q);
    }
    let up = match rounding {
        Rounding::Zero => false,
        Rounding::Infinity => true,
        Rounding::Down => n < 0,
        Rounding::Up => n > 0,
        // `2 * |r| < d` cannot overflow: |r| < d <= 2^62 by construction.
        Rounding::NearestAwayFromZero => 2 * r.abs() >= d,
    };
    if up {
        Some(q + if n < 0 { -1 } else { 1 })
    } else {
        Some(q)
    }
}

/// Saturating narrowing to `i64`.
fn clamp_i64(v: i128) -> i64 {
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}
