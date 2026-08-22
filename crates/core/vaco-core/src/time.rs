//! Timestamps and time bases.
//!
//! Every timestamp in Vaco is an integer count of ticks in an explicit
//! [`TimeBase`]. There is no ambient "seconds" representation, because the
//! commonest class of bug in a media tool is a timestamp interpreted in the
//! wrong base, and a type that carries its own base makes that a compile error
//! rather than a silent desync.

use crate::Rational;

/// The unit one timestamp tick represents, in seconds.
///
/// A stream at 90 kHz has a time base of `1/90000`; a 25 fps video track often
/// uses `1/25`.
pub type TimeBase = Rational;

/// A point in time, in ticks of some [`TimeBase`] tracked by the owning stream.
///
/// `None` models an absent timestamp — genuinely common in real media, and the
/// reason this is an `Option` newtype rather than a sentinel value. Sentinels
/// get compared, printed and arithmetic'd by accident; `None` cannot be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Timestamp(Option<i64>);

impl Timestamp {
    pub const NONE: Self = Self(None);

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

    /// Convert between time bases with explicit rounding.
    ///
    /// Rescaling is where precision is lost, so the rounding mode is a required
    /// argument: a muxer writing a chunk boundary and a decoder reporting a
    /// presentation time want different answers.
    #[must_use]
    pub fn rescale(self, from: TimeBase, to: TimeBase, rounding: Rounding) -> Self {
        let _ = (from, to, rounding);
        todo!("P0-03 freeze: 128-bit intermediate, then round per `rounding`")
    }

    /// Compare two timestamps that may be in different bases.
    #[must_use]
    pub fn compare(
        self,
        self_base: TimeBase,
        other: Self,
        other_base: TimeBase,
    ) -> Option<std::cmp::Ordering> {
        let _ = (self_base, other, other_base);
        todo!("P0-03 freeze: widening cross-multiplication, no float")
    }
}

/// How a rescale that cannot be represented exactly should round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rounding {
    Zero,
    Infinity,
    Down,
    Up,
    /// Round to nearest, ties away from zero. The usual choice for presentation
    /// timestamps.
    #[default]
    NearestAwayFromZero,
}

/// A span of time in ticks of a [`TimeBase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Duration(pub i64);
