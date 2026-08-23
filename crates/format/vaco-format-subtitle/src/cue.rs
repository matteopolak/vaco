//! [`Cue`]: the one in-memory shape every text-subtitle format demuxes into.

use vaco_core::Duration;

/// One subtitle event: a span of time and the bytes shown during it.
///
/// This is deliberately the *only* shape in this crate. A format's own markup
/// — ASS override tags, `WebVTT` cue settings, SAMI's HTML fragments — is not
/// parsed here or anywhere in `vaco-subtitle-text`: that is rendering, and
/// rendering is a decoder's job (`crates/codec/`, a later wave). A demuxer's
/// entire contract is "recover the span and the bytes shown during it", and
/// `Cue` is exactly that contract.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cue {
    /// Presentation start, relative to the start of the file.
    pub start: Duration,
    /// Presentation end. Some formats (SAMI, `RealText`) state only a start time
    /// per event and imply the end from the *next* event's start; the parser
    /// that derives that is format-specific, but the derived value always ends
    /// up here so every consumer of `Cue` sees the same two-endpoint shape.
    pub end: Duration,
    /// The cue's payload, verbatim.
    ///
    /// Not a `String`. Measured against the reference: a demuxer passes text
    /// bytes through unchanged, including invalid UTF-8, and only a BOM at the
    /// very start of the *file* (handled once, before any cue exists — see
    /// [`crate::encoding`]) ever causes a conversion. Rejecting or replacing
    /// invalid UTF-8 inside a cue is the decoder's job, not the demuxer's.
    pub text: Vec<u8>,
}

impl Cue {
    /// A cue with `text` and the given endpoints.
    #[must_use]
    pub const fn new(start: Duration, end: Duration, text: Vec<u8>) -> Self {
        Self { start, end, text }
    }

    /// This cue's length. Negative or zero spans are not rejected here —
    /// a demuxer is lenient by design (see `planning/AGENT-CONSTRAINTS.md`
    /// "Detection and demuxing ask different questions") — but a caller
    /// computing a packet duration wants zero rather than a negative tick
    /// count, so this saturates.
    ///
    /// Fuzz-found (`fuzz/seeds/subtitle_text_demux/regression-cue-duration-subtract-overflow-*`):
    /// the doc comment above already claimed this saturates, but the
    /// subtraction was plain `-`, which panics under checked overflow
    /// arithmetic once a parser hands it two [`Duration`]s far enough apart
    /// (an out-of-range timestamp field saturated to near `i64::MIN`/`MAX`
    /// upstream is exactly such a pair). `saturating_sub` makes the doc
    /// comment true.
    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_micros(
            self.end
                .as_micros()
                .saturating_sub(self.start.as_micros())
                .max(0),
        )
    }

    /// A lossy `&str` view, for tests, tools and diagnostics. Never used by a
    /// parser or serialiser to decide behaviour — see the [`Cue::text`] doc.
    #[must_use]
    pub fn text_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.text)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn duration_saturates_on_extreme_endpoints_instead_of_overflowing() {
        // Regression for a fuzz-found panic
        // (`fuzz/seeds/subtitle_text_demux/regression-cue-duration-subtract-overflow-crash-f2255277`):
        // an `end` near `i64::MAX` and a `start` near `i64::MIN` used to
        // panic on `end - start` under checked overflow arithmetic, despite
        // this method's own doc comment already claiming it saturates.
        let c = Cue::new(
            Duration::from_micros(i64::MIN),
            Duration::from_micros(i64::MAX),
            Vec::new(),
        );
        assert_eq!(c.duration(), Duration::from_micros(i64::MAX));
    }

    #[test]
    fn duration_is_the_span_and_never_negative() {
        let c = Cue::new(
            Duration::from_micros(1_000),
            Duration::from_micros(2_500),
            Vec::new(),
        );
        assert_eq!(c.duration(), Duration::from_micros(1_500));
        let backwards = Cue::new(
            Duration::from_micros(5_000),
            Duration::from_micros(1_000),
            Vec::new(),
        );
        assert_eq!(backwards.duration(), Duration::ZERO);
    }

    #[test]
    fn text_lossy_does_not_panic_on_invalid_utf8() {
        let c = Cue::new(Duration::ZERO, Duration::ZERO, vec![0xFF, 0xFE, b'x']);
        assert!(c.text_lossy().contains('x'));
    }
}
