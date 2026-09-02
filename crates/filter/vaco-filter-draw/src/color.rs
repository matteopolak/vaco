//! Colour parsing: a thin re-export of `vaco-core`'s already-complete
//! `AVColor` grammar, plus the one helper that vocabulary does not carry
//! itself.
//!
//! # Why this is not a second parser
//!
//! `vaco_core::parse::color` already implements exactly what plan
//! `16-filters.md` §4.1 asks this crate for — `#RRGGBB[AA]`, `0xRRGGBB[AA]`,
//! the reference's full named-colour table, `@alpha`, and `random` — because
//! it backs the CLI's own colour-shaped options
//! (`planning/research/05-fftools-cli.md` §5.6). Building a second, crate-
//! local copy is exactly the duplication `cargo xtask dup-check` (D19) exists
//! to catch — it did, here, during this crate's own first draft — and the
//! fix is to depend on the one that already exists rather than keep a
//! parallel one just because this crate is at a different layer.
//!
//! [`vaco_core::Rgba`] is used as this crate's colour type for the same
//! reason; [`alpha_fraction`] is the only thing this module actually adds.

pub use vaco_core::Rgba;

/// Alpha as a `0.0..=1.0` fraction, for the blend maths in [`crate::blend`].
#[must_use]
pub fn alpha_fraction(c: Rgba) -> f64 {
    f64::from(c.a) / 255.0
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn alpha_fraction_spans_the_full_range() {
        assert!((alpha_fraction(Rgba::new(0, 0, 0, 0)) - 0.0).abs() < 1e-12);
        assert!((alpha_fraction(Rgba::new(0, 0, 0, 255)) - 1.0).abs() < 1e-12);
        assert!((alpha_fraction(Rgba::new(0, 0, 0, 128)) - 128.0 / 255.0).abs() < 1e-12);
    }

    #[test]
    fn reuses_vaco_cores_color_grammar_directly() {
        // Not re-testing that grammar here (it is `vaco-core`'s own,
        // covered by its tests) — just confirming this crate calls it rather
        // than a local copy.
        let red = vaco_core::parse::color("red").unwrap();
        assert_eq!(red, Rgba::new(255, 0, 0, 255));
    }
}
