//! Source spans and the caret renderer every diagnostic uses.

use core::fmt;

/// A byte range into the filtergraph source string.
///
/// Half-open, and always on `char` boundaries because every producer advances
/// by whole characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    /// A span covering `start..end`, normalised so `end >= start`.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        if end < start {
            Self { start, end: start }
        } else {
            Self { start, end }
        }
    }

    /// An empty span at `at`.
    #[must_use]
    pub const fn at(at: usize) -> Self {
        Self { start: at, end: at }
    }

    /// How many bytes the span covers.
    #[must_use]
    pub const fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span covers nothing.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// The smallest span containing both.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        Self::new(self.start.min(other.start), self.end.max(other.end))
    }

    /// The text this span covers, or `""` if it does not land on character
    /// boundaries of `src`.
    #[must_use]
    pub fn slice(self, src: &str) -> &str {
        src.get(self.start..self.end).unwrap_or_default()
    }
}

/// Render `src` with a caret under `span` and `note` beside it.
///
/// A filtergraph is conventionally one line, but it may legitimately contain
/// newlines (whitespace is skipped around the structural characters), so the
/// renderer finds the line containing the span rather than assuming line 1.
#[must_use]
pub fn render_caret(src: &str, span: Span, note: &str) -> String {
    let start = span.start.min(src.len());
    let line_start = src
        .get(..start)
        .and_then(|s| s.rfind('\n'))
        .map_or(0, |i| i.saturating_add(1));
    let line_end = src
        .get(start..)
        .and_then(|s| s.find('\n'))
        .map_or(src.len(), |i| start.saturating_add(i));
    let line = src.get(line_start..line_end).unwrap_or_default();
    let line_no = src
        .get(..line_start)
        .map_or(1, |s| s.matches('\n').count().saturating_add(1));
    let col = src.get(line_start..start).map_or(0, |s| s.chars().count());
    let width = span
        .slice(src)
        .chars()
        .count()
        .max(1)
        .min(line.chars().count().saturating_sub(col).max(1));

    let gutter = line_no.to_string();
    let pad = " ".repeat(gutter.len());
    let mut out = String::new();
    let _ = fmt::Write::write_fmt(
        &mut out,
        format_args!(
            "  --> filtergraph:{}:{}\n{pad} |\n{gutter} | {line}\n{pad} | {}{} {note}\n",
            line_no,
            col.saturating_add(1),
            " ".repeat(col),
            "^".repeat(width),
        ),
    );
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn caret_points_at_the_span() {
        let src = "[0:v]drawtext=text=hello";
        let out = render_caret(src, Span::new(5, 13), "here");
        assert!(out.contains("filtergraph:1:6"), "{out}");
        assert!(out.contains("^^^^^^^^ here"), "{out}");
    }

    #[test]
    fn caret_finds_the_right_line() {
        let src = "a,\nb,\nc";
        let out = render_caret(src, Span::new(6, 7), "third");
        assert!(out.contains("filtergraph:3:1"), "{out}");
    }

    #[test]
    fn an_empty_span_still_renders_one_caret() {
        let out = render_caret("ab", Span::at(2), "end");
        assert!(out.contains('^'), "{out}");
    }

    #[test]
    fn a_span_past_the_end_does_not_panic() {
        let _ = render_caret("ab", Span::new(99, 120), "x");
    }
}
