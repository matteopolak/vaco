//! Filename pattern expansion for `segment`/`stream_segment`: the `%d`-style
//! numbered form, and (`-strftime`) a small `strftime` subset.
//!
//! # Why this is not `vaco-demux-image2`'s `SequencePattern`
//!
//! That type solves the identical `%0Nd` problem and lives in this same
//! `crates/format/` layer, but pulling in a sibling format crate for twenty
//! lines of printf-style substitution is a heavier dependency edge than the
//! problem justifies, and this crate's number-pattern needs are a strict
//! subset (segment indices only count up from zero, never need glob
//! matching or a filesystem probe). Reimplemented small and self-contained
//! instead.

use core::fmt::Write as _;

/// The largest zero-padding width honoured — see
/// `vaco-mux-hls::filename::MAX_PAD_WIDTH`'s doc for why: `%0Nd` feeds `N`
/// straight into a `format!`/`write!` width specifier, which allocates a
/// string that many bytes long, so an uncapped width turns a few bytes of
/// pattern into a multi-gigabyte allocation from one `%0<huge>d`.
const MAX_PAD_WIDTH: usize = 64;

/// Expand every `%d`/`%0Nd`/`%%` in `pattern` for segment index `n`.
///
/// `%%` is a literal `%`. `%d` and `%0Nd` (`N` one or two digits) are `n`
/// formatted as decimal, zero-padded to `N` digits for the padded form. An
/// unrecognised `%x` is passed through literally (both characters) — this is
/// a lenient expander, not a validator; a script author who wrote a typo
/// gets a filename with a stray `%x` in it, not a hard failure mid-segment.
#[must_use]
pub fn expand_index(pattern: &str, n: u64) -> String {
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some('d') => {
                chars.next();
                out.push_str(&n.to_string());
            }
            Some('0') => {
                chars.next();
                let mut width = String::new();
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    if let Some(d) = chars.next() {
                        width.push(d);
                    }
                }
                if chars.peek() == Some(&'d') {
                    chars.next();
                    let width: usize = width.parse().unwrap_or(0).min(MAX_PAD_WIDTH);
                    let _ = write!(out, "{n:0width$}");
                } else {
                    // Not actually a numbered placeholder: keep it literal.
                    out.push('%');
                    out.push('0');
                    out.push_str(&width);
                }
            }
            _ => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_percent_d() {
        assert_eq!(expand_index("out%d.ts", 3), "out3.ts");
    }

    #[test]
    fn zero_padded() {
        assert_eq!(expand_index("out%03d.ts", 7), "out007.ts");
        assert_eq!(expand_index("out%05d.ts", 42), "out00042.ts");
    }

    #[test]
    fn literal_percent_percent() {
        assert_eq!(expand_index("100%%done%d.ts", 1), "100%done1.ts");
    }

    #[test]
    fn no_placeholder_is_unchanged() {
        assert_eq!(expand_index("out.ts", 5), "out.ts");
    }

    /// A short pattern must never turn into a huge allocation: before the
    /// `MAX_PAD_WIDTH` cap, `%09999999999d` asked for a ten-billion-byte
    /// pad from thirteen bytes of input.
    #[test]
    fn an_absurd_pad_width_is_capped_not_honoured() {
        let out = expand_index("out%09999999999d.ts", 3);
        assert!(out.len() < 200, "got {} bytes", out.len());
        assert!(out.starts_with("out"));
        assert!(out.ends_with("3.ts"));
    }

    #[test]
    fn an_unrecognised_specifier_is_kept_literally() {
        assert_eq!(expand_index("out%xyz.ts", 1), "out%xyz.ts");
    }
}
