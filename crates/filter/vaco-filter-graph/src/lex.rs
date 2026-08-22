//! The one escaping-aware scan every level of the grammar shares.
//!
//! # Why this is not `vaco_core::escape`
//!
//! The shared module in `vaco-core` implements the same *scheme* — backslash
//! escapes, single-quoted runs, split-before-unescape — but it **rejects** two
//! inputs the reference accepts, and a filtergraph string is a user-facing
//! contract:
//!
//! ```text
//! ffmpeg -f lavfi -i "movie='ab"    ->  filename 'ab'   (unterminated quote runs to the end)
//! ffmpeg -f lavfi -i 'movie=ab\'    ->  filename 'ab\'  (a lone trailing backslash is literal)
//! ```
//!
//! `vaco_core::escape::unescape` returns `UnterminatedQuote` and
//! `TrailingBackslash` for those. Erroring would fail command lines that work
//! today, so this scanner is lenient in exactly those two places and records
//! each as a [`Quirk`] instead, which `Ast::warnings` surfaces.
//!
//! # What one scan does
//!
//! [`next_token`] walks bytes, tracking whether it is inside a quoted run,
//! honouring `\`, and stopping only on an **unescaped, unquoted** character in
//! the stop set. That single function is what makes the levels compose: `\[`
//! inside an argument never starts a link label, because the graph scan asked
//! for [`StopSet::GRAPH`] and the backslash suppressed it.
//!
//! Whitespace before a token is skipped and trailing whitespace is trimmed —
//! but only whitespace written *bare*. A space that arrived through a quote or
//! a backslash is data and survives, which is the only way to write a value
//! that ends in a space.

use crate::span::Span;

/// Which characters end a token at this level.
///
/// The flags are deliberately named after the *level*, not the character, so a
/// call site reads as the grammar rule it implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StopSet(u8);

impl StopSet {
    /// Nothing stops the scan; it runs to the end of the string.
    pub const NONE: Self = Self(0);
    /// `:` — the argument separator inside a filter's argument list.
    pub const ARG: Self = Self(1);
    /// `=` — what separates a key from its value, and a name from its
    /// arguments.
    pub const EQ: Self = Self(2);
    /// `,` and `;` — what separates filters and chains.
    pub const CHAIN: Self = Self(4);
    /// `[` — what opens a link label.
    pub const OPEN: Self = Self(8);
    /// `]` — what closes one.
    pub const CLOSE: Self = Self(16);
    /// `|` — the separator inside a list-valued option. Option-internal, and
    /// named here so it is not mistaken for a graph-level rule.
    pub const LIST: Self = Self(32);
    /// `;` alone — what terminates the `sws_flags=` prefix.
    pub const SEMI: Self = Self(64);

    /// `[ ] , ;` — what terminates a filter description.
    pub const GRAPH: Self = Self(4 | 8 | 16);
    /// `= , ; [` — what terminates a filter *name*. Note `]` is absent: the
    /// reference reads `hflip]x` as one (unknown) filter name.
    pub const NAME: Self = Self(2 | 4 | 8);
    /// `]` — what terminates a link label.
    pub const LABEL: Self = Self(16);

    /// The union of two stop sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn has(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Whether `c` ends a token at this level.
    #[must_use]
    pub const fn stops(self, c: char) -> bool {
        match c {
            ':' => self.has(Self::ARG),
            '|' => self.has(Self::LIST),
            '=' => self.has(Self::EQ),
            '[' => self.has(Self::OPEN),
            ']' => self.has(Self::CLOSE),
            ',' => self.has(Self::CHAIN),
            ';' => self.has(Self::CHAIN) || self.has(Self::SEMI),
            _ => false,
        }
    }

    /// The characters this set stops on, for a re-escaping printer.
    #[must_use]
    pub fn chars(self) -> Vec<char> {
        [':', '|', '=', '[', ']', ',', ';']
            .into_iter()
            .filter(|&c| self.stops(c))
            .collect()
    }
}

/// Something the reference tolerates that a stricter reading would reject.
///
/// Recorded rather than raised, because rejecting either would fail command
/// lines that work against the reference today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Quirk {
    /// A `'` was opened and never closed; the quoted run reached the end of the
    /// string.
    UnterminatedQuote,
    /// The string ended with a lone `\`, which is taken as a literal backslash.
    TrailingBackslash,
}

impl Quirk {
    /// A one-line explanation for a verbose log.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::UnterminatedQuote => {
                "unterminated quoted run; it was taken as reaching the end of the token"
            }
            Self::TrailingBackslash => {
                "the token ends with a lone backslash; it was taken literally"
            }
        }
    }
}

/// One scanned token: the decoded text, where it came from, and anything odd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The text with one level of escaping removed.
    pub text: String,
    /// The bytes of the source this token was scanned from, whitespace
    /// included.
    pub span: Span,
    /// Leniencies applied while scanning.
    pub quirks: Vec<(Quirk, Span)>,
}

impl Token {
    /// Whether the decoded text is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

const fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Advance `at` past any whitespace.
pub fn skip_ws(src: &str, at: &mut usize) {
    while let Some(c) = peek(src, *at) {
        if is_ws(c) {
            *at = at.saturating_add(c.len_utf8());
        } else {
            break;
        }
    }
}

/// The character at `at`, if `at` is a boundary inside `src`.
#[must_use]
pub fn peek(src: &str, at: usize) -> Option<char> {
    src.get(at..).and_then(|s| s.chars().next())
}

/// Scan one token, removing exactly one level of escaping.
///
/// Stops *before* the terminator, so the caller decides what to do with it.
/// Never fails: the two malformed shapes are recorded as [`Quirk`]s, matching
/// the reference (see the module documentation).
pub fn next_token(src: &str, at: &mut usize, stop: StopSet) -> Token {
    skip_ws(src, at);
    let start = *at;
    let mut text = String::new();
    // Bytes of `text` up to and including the last character that must survive
    // the trailing trim: everything except bare whitespace.
    let mut keep = 0usize;
    let mut in_quote = false;
    let mut quirks = Vec::new();

    while let Some(c) = peek(src, *at) {
        let here = *at;
        if in_quote {
            *at = at.saturating_add(c.len_utf8());
            if c == '\'' {
                in_quote = false;
            } else {
                text.push(c);
                keep = text.len();
            }
            continue;
        }
        if stop.stops(c) {
            break;
        }
        *at = at.saturating_add(c.len_utf8());
        match c {
            '\'' => in_quote = true,
            '\\' => {
                if let Some(n) = peek(src, *at) {
                    *at = at.saturating_add(n.len_utf8());
                    text.push(n);
                } else {
                    // The reference keeps a lone trailing backslash.
                    text.push('\\');
                    quirks.push((Quirk::TrailingBackslash, Span::new(here, *at)));
                }
                keep = text.len();
            }
            _ => {
                text.push(c);
                if !is_ws(c) {
                    keep = text.len();
                }
            }
        }
    }
    if in_quote {
        quirks.push((Quirk::UnterminatedQuote, Span::new(start, *at)));
    }
    text.truncate(keep);
    Token {
        text,
        span: Span::new(start, *at),
        quirks,
    }
}

/// Split `src` at every unescaped, unquoted character in `stop`, returning the
/// pieces **still escaped** and their spans.
///
/// Splitting before unescaping is what makes nesting work: the outer level
/// finds its own separators without being confused by the inner level's, and
/// each piece is unescaped exactly once on the way down. Unescaping first and
/// splitting after is the classic "why does my regex option need four
/// backslashes" bug.
#[must_use]
pub fn split_raw(src: &str, stop: StopSet) -> Vec<(&str, Span)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut at = 0usize;
    let mut in_quote = false;
    while let Some(c) = peek(src, at) {
        let here = at;
        at = at.saturating_add(c.len_utf8());
        if in_quote {
            if c == '\'' {
                in_quote = false;
            }
            continue;
        }
        match c {
            '\'' => in_quote = true,
            '\\' => {
                if let Some(n) = peek(src, at) {
                    at = at.saturating_add(n.len_utf8());
                }
            }
            _ if stop.stops(c) => {
                out.push((
                    src.get(start..here).unwrap_or_default(),
                    Span::new(start, here),
                ));
                start = at;
            }
            _ => {}
        }
    }
    out.push((
        src.get(start..).unwrap_or_default(),
        Span::new(start, src.len()),
    ));
    out
}

/// Split once at the first unescaped character in `stop`.
///
/// `None` means the separator is absent, which is how a positional argument is
/// told from a `key=value` one.
#[must_use]
pub fn split_once_raw(src: &str, stop: StopSet) -> Option<(&str, &str)> {
    let mut at = 0usize;
    let mut in_quote = false;
    while let Some(c) = peek(src, at) {
        let here = at;
        at = at.saturating_add(c.len_utf8());
        if in_quote {
            if c == '\'' {
                in_quote = false;
            }
            continue;
        }
        match c {
            '\'' => in_quote = true,
            '\\' => {
                if let Some(n) = peek(src, at) {
                    at = at.saturating_add(n.len_utf8());
                }
            }
            _ if stop.stops(c) => {
                return Some((
                    src.get(..here).unwrap_or_default(),
                    src.get(at..).unwrap_or_default(),
                ));
            }
            _ => {}
        }
    }
    None
}

/// Remove one level of escaping from a whole string.
///
/// The lenient counterpart of `vaco_core::escape::unescape`: the two shapes
/// that module rejects are accepted here, because the reference accepts them.
#[must_use]
pub fn unescape(src: &str) -> String {
    let mut at = 0usize;
    next_token(src, &mut at, StopSet::NONE).text
}

/// Escape `text` so that scanning it with `stop` yields `text` back.
///
/// Leading and trailing whitespace is escaped too — without that the trailing
/// trim in [`next_token`] would eat it, and the round trip would not hold for
/// `drawtext=text=hello ` and friends.
#[must_use]
pub fn escape(text: &str, stop: StopSet) -> String {
    let mut out = String::new();
    let n = text.chars().count();
    for (i, c) in text.chars().enumerate() {
        let edge = i == 0 || i.saturating_add(1) == n;
        if c == '\\' || c == '\'' || stop.stops(c) || (is_ws(c) && edge) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn scan(src: &str, stop: StopSet) -> (String, usize) {
        let mut at = 0;
        let t = next_token(src, &mut at, stop);
        (t.text, at)
    }

    #[test]
    fn a_bare_token_runs_to_the_stop_character() {
        assert_eq!(scan("ab,cd", StopSet::GRAPH), ("ab".into(), 2));
    }

    #[test]
    fn a_backslash_escapes_the_next_character() {
        assert_eq!(scan(r"a\,b,c", StopSet::GRAPH).0, "a,b");
    }

    #[test]
    fn a_quoted_run_is_literal_including_backslashes() {
        // Measured: `movie='a\\b'` yields `a\b` after both levels, which is
        // only consistent with `\` being data inside quotes.
        assert_eq!(scan(r"'a\\b'", StopSet::GRAPH).0, r"a\\b");
        assert_eq!(scan("'a:b,c'", StopSet::GRAPH).0, "a:b,c");
    }

    #[test]
    fn quotes_do_not_nest_and_adjacent_runs_concatenate() {
        assert_eq!(scan("'a''b'", StopSet::GRAPH).0, "ab");
    }

    #[test]
    fn leading_whitespace_is_skipped_and_trailing_is_trimmed() {
        assert_eq!(scan("  a b  ,x", StopSet::GRAPH).0, "a b");
    }

    #[test]
    fn escaped_or_quoted_whitespace_survives_the_trim() {
        assert_eq!(scan(r"a\ ", StopSet::GRAPH).0, "a ");
        assert_eq!(scan("'a '", StopSet::GRAPH).0, "a ");
        assert_eq!(scan(r"\ a", StopSet::GRAPH).0, " a");
    }

    #[test]
    fn an_unterminated_quote_reaches_the_end_and_is_recorded() {
        let mut at = 0;
        let t = next_token("'ab", &mut at, StopSet::GRAPH);
        assert_eq!(t.text, "ab");
        assert_eq!(
            t.quirks.first().map(|q| q.0),
            Some(Quirk::UnterminatedQuote)
        );
    }

    #[test]
    fn a_lone_trailing_backslash_is_literal_and_recorded() {
        let mut at = 0;
        let t = next_token(r"ab\", &mut at, StopSet::GRAPH);
        assert_eq!(t.text, r"ab\");
        assert_eq!(
            t.quirks.first().map(|q| q.0),
            Some(Quirk::TrailingBackslash)
        );
    }

    #[test]
    fn the_name_stop_set_does_not_include_a_close_bracket() {
        assert_eq!(scan("hflip]x", StopSet::NAME).0, "hflip]x");
        assert_eq!(scan("hflip=1", StopSet::NAME).0, "hflip");
    }

    #[test]
    fn the_graph_stop_set_does_not_include_colon_or_equals() {
        assert_eq!(scan("w=1:h=2,next", StopSet::GRAPH).0, "w=1:h=2");
    }

    #[test]
    fn split_raw_keeps_the_pieces_escaped() {
        let pieces = split_raw(r"a\:b:c", StopSet::ARG);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].0, r"a\:b");
        assert_eq!(pieces[1].0, "c");
    }

    #[test]
    fn escape_round_trips_through_the_scanner() {
        for s in [
            "plain",
            "a,b",
            "a;b",
            "a[b]c",
            r"back\slash",
            "quo'te",
            " leading",
            "trailing ",
            "  ",
            "",
        ] {
            let enc = escape(s, StopSet::GRAPH);
            let mut at = 0;
            assert_eq!(next_token(&enc, &mut at, StopSet::GRAPH).text, s, "{s:?}");
        }
    }

    #[test]
    fn scanning_never_panics_on_multibyte_input() {
        for s in ["é", "'é", r"é\", "日本語:x", "\u{1f600},a"] {
            let _ = unescape(s);
            let _ = split_raw(s, StopSet::ARG);
        }
    }
}
