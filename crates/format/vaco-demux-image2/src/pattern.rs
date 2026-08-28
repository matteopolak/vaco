//! Sequence filename patterns: `%d`, `%0Nd`, and `%%` for a literal percent.
//!
//! This is the `-pattern_type sequence` half of image2. It is pure string
//! arithmetic — turning an index into a filename and back — with no
//! filesystem access at all, so it is the half of this crate that still works
//! on `wasm32-unknown-unknown` at runtime (see the crate docs for the half
//! that does not).
//!
//! # Grammar, observed against ffmpeg 8.1
//!
//! `ffmpeg -f lavfi -i testsrc=d=1 -f image2 'out%03d.png'` writes
//! `out001.png` (`start_number` defaults to 1 on the mux side). The pattern
//! accepts exactly one numeric placeholder, spelled `%d` (unpadded) or
//! `%0Nd` (zero-padded to `N` digits, `N >= 1`); a literal `%` is written
//! `%%`. A pattern with zero or more than one placeholder is rejected —
//! measured via `ffmpeg -pattern_type sequence -i 'a.png'` and
//! `-i 'a%d_%d.png'`, both of which fail to open.

use vaco_core::{Error, Result};

/// A parsed `-pattern_type sequence` filename template.
///
/// `format` and `matches` are inverses of each other for any non-negative
/// index, which is the property the proptest in this module's tests checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequencePattern {
    prefix: String,
    /// Zero-padded width, or `0` for the unpadded `%d` form.
    width: usize,
    suffix: String,
}

impl SequencePattern {
    /// Parse a sequence pattern.
    ///
    /// # Errors
    /// [`Error::InvalidData`] when the pattern has no `%d`/`%0Nd` placeholder,
    /// or more than one.
    pub fn parse(pattern: &str) -> Result<Self> {
        let mut prefix = String::new();
        let mut suffix = String::new();
        let mut width: Option<usize> = None;
        let mut chars = pattern.char_indices().peekable();

        while let Some((_, c)) = chars.next() {
            if c != '%' {
                push(&mut prefix, &mut suffix, width, c);
                continue;
            }
            // Collect the run of ASCII digits right after '%', if any.
            let mut digits = String::new();
            while let Some(&(_, d)) = chars.peek() {
                if d.is_ascii_digit() {
                    digits.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            match chars.peek().copied() {
                Some((_, '%')) if digits.is_empty() => {
                    chars.next();
                    push(&mut prefix, &mut suffix, width, '%');
                }
                Some((_, 'd')) => {
                    chars.next();
                    if width.is_some() {
                        return Err(Error::InvalidData(
                            "sequence pattern has more than one %d placeholder",
                        ));
                    }
                    let w = if digits.is_empty() {
                        0
                    } else {
                        digits.parse().unwrap_or(0)
                    };
                    width = Some(w);
                }
                _ => {
                    // Not a recognised placeholder: put back the literal '%'
                    // and whatever digits we ate, verbatim.
                    push(&mut prefix, &mut suffix, width, '%');
                    for d in digits.chars() {
                        push(&mut prefix, &mut suffix, width, d);
                    }
                }
            }
        }

        let Some(width) = width else {
            return Err(Error::InvalidData(
                "sequence pattern has no %d or %0Nd placeholder",
            ));
        };
        Ok(Self {
            prefix,
            width,
            suffix,
        })
    }

    /// Whether `pattern` parses as a valid sequence pattern at all, without
    /// building one. Used by `-pattern_type`'s unset (auto) case to decide
    /// whether a filename should be treated as a sequence or as a literal.
    #[must_use]
    pub fn looks_like_one(pattern: &str) -> bool {
        Self::parse(pattern).is_ok()
    }

    /// Whether `pattern` contains at least one `%d`/`%0Nd` token, ignoring a
    /// doubled `%%`.
    ///
    /// [`SequencePattern::parse`] collapses "no placeholder" and "more than
    /// one placeholder" into the same `Err` variant, which is right for a
    /// caller that treats a pattern as a pattern or an error either way. A
    /// caller that instead wants to fall back to *literal filename* only when
    /// there is no placeholder at all — the mux side's bare-filename case —
    /// needs the two told apart, and matching `parse`'s error text would make
    /// that message an interface rather than prose.
    #[must_use]
    pub fn has_placeholder(pattern: &str) -> bool {
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                continue;
            }
            let mut saw_digit = false;
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
                saw_digit = true;
            }
            match chars.peek() {
                Some('%') if !saw_digit => {
                    chars.next();
                }
                Some('d') => return true,
                _ => {}
            }
        }
        false
    }

    /// The filename for `index`, e.g. `out%03d.png` at index 7 is `out007.png`.
    #[must_use]
    pub fn format(&self, index: i64) -> String {
        if self.width == 0 {
            format!("{}{}{}", self.prefix, index, self.suffix)
        } else {
            format!(
                "{}{:0width$}{}",
                self.prefix,
                index,
                self.suffix,
                width = self.width
            )
        }
    }

    /// Recover the index from a filename this pattern could have produced,
    /// for a non-negative index. `None` if `name` does not fit the template
    /// (wrong prefix/suffix, non-digit body, or — for the zero-padded form —
    /// the wrong digit count).
    ///
    /// Restricted to non-negative indices: a zero-padded negative number's
    /// sign eats one digit position (`%03d` of `-5` is `-05`, three
    /// characters, two of them digits), which would make this ambiguous with
    /// a positive index of different width. Nothing in this crate needs to
    /// recover a negative index from a filename, so the simpler rule is used.
    #[must_use]
    pub fn matches(&self, name: &str) -> Option<i64> {
        let rest = name.strip_prefix(self.prefix.as_str())?;
        let rest = rest.strip_suffix(self.suffix.as_str())?;
        if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        // `width` is a *minimum*: `%03d` of 1234 is "1234", four digits, not
        // truncated. A shorter digit run than the declared width, though, was
        // never produced by `format` and is rejected.
        if self.width > 0 && rest.len() < self.width {
            return None;
        }
        rest.parse().ok()
    }
}

fn push(prefix: &mut String, suffix: &mut String, width: Option<usize>, c: char) {
    if width.is_some() {
        suffix.push(c);
    } else {
        prefix.push(c);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rejects_missing_placeholder() {
        assert!(SequencePattern::parse("out.png").is_err());
    }

    #[test]
    fn has_placeholder_finds_a_bare_pattern() {
        assert!(!SequencePattern::has_placeholder("out.png"));
        assert!(SequencePattern::has_placeholder("out%03d.png"));
        assert!(SequencePattern::has_placeholder("out%d.png"));
        assert!(SequencePattern::has_placeholder("a%d_%d.png"));
        assert!(!SequencePattern::has_placeholder("100%%.png"));
    }

    #[test]
    fn rejects_duplicate_placeholder() {
        assert!(SequencePattern::parse("a%d_%d.png").is_err());
    }

    #[test]
    fn formats_zero_padded() {
        let p = SequencePattern::parse("out%03d.png").unwrap();
        assert_eq!(p.format(7), "out007.png");
        assert_eq!(p.format(1234), "out1234.png");
    }

    #[test]
    fn formats_unpadded() {
        let p = SequencePattern::parse("out%d.png").unwrap();
        assert_eq!(p.format(7), "out7.png");
    }

    #[test]
    fn literal_percent_escape() {
        let p = SequencePattern::parse("100%%_%03d.png").unwrap();
        assert_eq!(p.format(5), "100%_005.png");
    }

    #[test]
    fn matches_is_the_inverse_of_format() {
        let p = SequencePattern::parse("img_%04d.jpg").unwrap();
        let name = p.format(42);
        assert_eq!(p.matches(&name), Some(42));
        assert_eq!(p.matches("img_42.jpg"), None); // wrong width
        assert_eq!(p.matches("nope.jpg"), None);
    }

    proptest! {
        /// `format` then `matches` recovers the original index, for any
        /// prefix/suffix that contain no digits or `%` (which would make the
        /// split ambiguous) and any non-negative index.
        #[test]
        fn format_matches_roundtrip(
            prefix in "[a-zA-Z_/]{0,6}",
            suffix in "[a-zA-Z_.]{0,6}",
            width in 0usize..6,
            index in 0i64..1_000_000,
        ) {
            let pattern = if width == 0 {
                format!("{prefix}%d{suffix}")
            } else {
                format!("{prefix}%0{width}d{suffix}")
            };
            let parsed = SequencePattern::parse(&pattern);
            prop_assume!(parsed.is_ok());
            let Ok(p) = parsed else { unreachable!() };
            let name = p.format(index);
            prop_assert_eq!(p.matches(&name), Some(index));
        }

        /// Parsing never panics on arbitrary text.
        #[test]
        fn parse_never_panics(s in ".{0,64}") {
            let _ = SequencePattern::parse(&s);
        }
    }
}
