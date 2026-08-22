//! Span-anchored diagnostics.
//!
//! `vaco_core::Error` carries a `&'static str`, so it cannot hold the filter
//! name, the label or the caret a filtergraph diagnostic is worth having. The
//! same shape `vaco-filter-core` uses for `Conflict` applies here: a rich error
//! type of our own, convertible into the frozen one for the `Result` boundary,
//! and reachable separately for the message.

use core::fmt;

use crate::span::{Span, render_caret};

/// What went wrong, with enough detail to name it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The whole description contained no filter.
    EmptyGraph,
    /// A filter name was empty — `a;;b`, a leading `;`, or `a,,b`.
    EmptyFilterName,
    /// `[]`, or a label that was only whitespace.
    EmptyLabel,
    /// A `[` with no matching `]`.
    UnterminatedLabel,
    /// Something followed a filter that was neither `,` nor `;`.
    TrailingGarbage(String),
    /// No filter of this name is registered.
    UnknownFilter {
        name: String,
        suggestion: Option<String>,
    },
    /// More `[in]` labels than the filter has input pads.
    TooManyInputLabels {
        filter: String,
        given: usize,
        has: usize,
    },
    /// More `[out]` labels than the filter has output pads.
    TooManyOutputLabels {
        filter: String,
        given: usize,
        has: usize,
    },
    /// Two filters claimed the same output label.
    DuplicateOutputLabel { label: String, first: Span },
    /// Two filters in one graph share an explicit `@id`.
    DuplicateInstanceName(String),
    /// A positional argument came after a `key=value` one.
    PositionalAfterNamed(String),
    /// An input pad was left unconnected and cannot be exported.
    UnconnectedInput { filter: String, pad: usize },
    /// An output pad was left unconnected and was not labelled.
    UnconnectedOutput { filter: String, pad: usize },
    /// The two ends of a link carry different media types.
    MediaMismatch { src: String, dst: String },
    /// The graph is not a DAG.
    Cycle(Vec<String>),
    /// The filter rejected its arguments, or building it failed.
    Filter { filter: String, detail: String },
    /// The registry realised a pad count the descriptor cannot express.
    PadCountMismatch { filter: String, detail: String },
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyGraph => f.write_str("no filters specified in the graph description"),
            Self::EmptyFilterName => f.write_str("no such filter: ''"),
            Self::EmptyLabel => f.write_str("empty link label"),
            Self::UnterminatedLabel => f.write_str("mismatched '['"),
            Self::TrailingGarbage(t) => write!(f, "trailing garbage after a filter: {t}"),
            Self::UnknownFilter { name, suggestion } => match suggestion {
                Some(s) => write!(f, "no such filter: '{name}'; did you mean '{s}'?"),
                None => write!(f, "no such filter: '{name}'"),
            },
            Self::TooManyInputLabels { filter, given, has } => write!(
                f,
                "more input link labels specified for filter '{filter}' than it has inputs: {given} > {has}"
            ),
            Self::TooManyOutputLabels { filter, given, has } => write!(
                f,
                "more output link labels specified for filter '{filter}' than it has outputs: {given} > {has}"
            ),
            Self::DuplicateOutputLabel { label, .. } => {
                write!(f, "duplicate output label '{label}'")
            }
            Self::DuplicateInstanceName(n) => {
                write!(f, "duplicate filter instance name '{n}'")
            }
            Self::PositionalAfterNamed(v) => {
                write!(f, "positional argument '{v}' after a named argument")
            }
            Self::UnconnectedInput { filter, pad } => {
                write!(f, "input pad {pad} of {filter} is not connected")
            }
            Self::UnconnectedOutput { filter, pad } => write!(
                f,
                "output pad {pad} of {filter} is not connected; label it and map it"
            ),
            Self::MediaMismatch { src, dst } => {
                write!(f, "media type mismatch between {src} and {dst}")
            }
            Self::Cycle(nodes) => write!(f, "filtergraph contains a cycle: {}", nodes.join(" -> ")),
            Self::Filter { filter, detail } => write!(f, "filter '{filter}': {detail}"),
            Self::PadCountMismatch { filter, detail } => {
                write!(
                    f,
                    "filter '{filter}' realised pads it cannot declare: {detail}"
                )
            }
        }
    }
}

/// A diagnostic anchored to a span of the graph description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphError {
    pub kind: ErrorKind,
    pub span: Span,
}

impl GraphError {
    /// Build one.
    #[must_use]
    pub const fn new(kind: ErrorKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// The message with a caret under the offending span.
    ///
    /// Takes the source because the error does not own it — a graph string can
    /// be long and an error is usually rendered once.
    #[must_use]
    pub fn render(&self, src: &str) -> String {
        let mut out = format!("error: {}\n", self.kind);
        out.push_str(&render_caret(src, self.span, self.note()));
        if let ErrorKind::DuplicateOutputLabel { first, .. } = &self.kind {
            out.push_str("\nfirst defined here:\n");
            out.push_str(&render_caret(src, *first, "first definition"));
        }
        out
    }

    fn note(&self) -> &'static str {
        match self.kind {
            ErrorKind::EmptyFilterName => "a filter name is required here",
            ErrorKind::EmptyLabel => "a label needs a name",
            ErrorKind::UnterminatedLabel => "opened here, never closed",
            ErrorKind::TrailingGarbage(_) => "expected ',' or ';' after a filter",
            ErrorKind::UnknownFilter { .. } => "not a registered filter",
            ErrorKind::PositionalAfterNamed(_) => "named arguments must come last",
            ErrorKind::DuplicateOutputLabel { .. } => "defined a second time here",
            _ => "here",
        }
    }
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.kind, f)
    }
}

impl std::error::Error for GraphError {}

impl From<GraphError> for vaco_core::Error {
    fn from(e: GraphError) -> Self {
        match e.kind {
            ErrorKind::UnknownFilter { .. } => Self::Unsupported("no such filter"),
            ErrorKind::Filter { filter, detail } => Self::Option {
                name: filter,
                detail,
            },
            ErrorKind::EmptyGraph => Self::InvalidData("empty filtergraph description"),
            ErrorKind::Cycle(_) => Self::InvalidData("filtergraph contains a cycle"),
            ErrorKind::MediaMismatch { .. } => {
                Self::InvalidData("media type mismatch across a filter link")
            }
            _ => Self::InvalidData("invalid filtergraph description"),
        }
    }
}

/// Suggest the closest registered name, if one is close enough to help.
///
/// Levenshtein with a threshold of half the name's length, capped at three.
///
/// Half rather than a third because the commonest typo is a transposition,
/// which plain Levenshtein scores as two: `scael` -> `scale` has to survive.
/// The cap stops a long name matching an unrelated long name.
#[must_use]
pub fn suggest<'a, I: IntoIterator<Item = &'a str>>(name: &str, known: I) -> Option<String> {
    let budget = name.chars().count().div_euclid(2).clamp(1, 3);
    let mut best: Option<(usize, &str)> = None;
    for candidate in known {
        let d = edit_distance(name, candidate);
        if d <= budget && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, candidate));
        }
    }
    best.map(|(_, s)| s.to_owned())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len().saturating_add(1)];
    for (i, ca) in a.iter().enumerate() {
        if let Some(slot) = cur.first_mut() {
            *slot = i.saturating_add(1);
        }
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let del = prev.get(j.saturating_add(1)).copied().unwrap_or(usize::MAX);
            let ins = cur.get(j).copied().unwrap_or(usize::MAX);
            let sub = prev.get(j).copied().unwrap_or(usize::MAX);
            let best = del
                .saturating_add(1)
                .min(ins.saturating_add(1))
                .min(sub.saturating_add(cost));
            if let Some(slot) = cur.get_mut(j.saturating_add(1)) {
                *slot = best;
            }
        }
        core::mem::swap(&mut prev, &mut cur);
    }
    prev.last().copied().unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_near_miss_is_suggested() {
        assert_eq!(
            suggest("scael", ["scale", "crop", "overlay"]),
            Some("scale".to_owned())
        );
    }

    #[test]
    fn a_wild_guess_is_not() {
        assert_eq!(suggest("zzzzzzzz", ["scale", "crop"]), None);
    }

    #[test]
    fn rendering_names_the_span() {
        let e = GraphError::new(ErrorKind::EmptyFilterName, Span::new(2, 2));
        let out = e.render("a,,b");
        assert!(out.contains("no such filter: ''"), "{out}");
        assert!(out.contains("filtergraph:1:3"), "{out}");
    }

    #[test]
    fn edit_distance_is_symmetric_and_zero_on_equal() {
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", "abd"), edit_distance("abd", "abc"));
        assert_eq!(edit_distance("", "abc"), 3);
    }
}
