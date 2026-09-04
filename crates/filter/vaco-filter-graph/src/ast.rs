//! The filtergraph description language: the syntax tree and the parser.
//!
//! # The grammar, as the reference implements it
//!
//! ```text
//! GRAPH       := WS? SWS_PREFIX? CHAIN (';' CHAIN)* WS?
//! SWS_PREFIX  := "sws_flags" '=' <token to ';'> ';'      -- literally first
//! CHAIN       := FILTER (',' FILTER)*
//! FILTER      := LABEL* NAME ('=' ARGS)? LABEL*
//! NAME        := <token stopping at '=' ',' ';' '['>     -- split at the first '@'
//! ARGS        := <token stopping at '[' ']' ',' ';'>
//! LABEL       := '[' <token stopping at ']'> ']'
//! ```
//!
//! Every rule below was measured against ffmpeg 8.1; the commands are in
//! `docs/filter/vaco-filter-graph.md`. Four of them contradict plan 16 §2.1 and
//! are called out there.
//!
//! * A **trailing** `,` or `;` is accepted (`hflip,` and `hflip;` both parse);
//!   a *leading* one is not, and `;;` is not, because each yields an empty
//!   filter name.
//! * The auto-assigned instance name is `Parsed_<name>_<N>` with `N` counting
//!   **every** filter in the graph, not filters of that name.
//! * The name token is not whitespace-trimmed *internally*, so `hflip @x`
//!   names a filter called `hflip ` and fails.
//! * `]` does not terminate a name: `hflip]x` is one unknown filter.

use crate::error::{ErrorKind, GraphError};
use crate::lex::{self, Quirk, StopSet, next_token, peek, skip_ws};
use crate::span::Span;

/// The `sws_flags=` prefix: one opaque flag string handed to every
/// auto-inserted `scale`.
///
/// Recognised **only** as the very first token of the whole description, and
/// only spelled exactly, with no space before the `=`. A `sws_flags=` anywhere
/// else is an ordinary (and unknown) filter name.
pub const SWS_PREFIX: &str = "sws_flags=";

/// A parsed filtergraph description.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ast {
    /// The `sws_flags=` prefix, decoded, if one was present.
    pub sws_flags: Option<String>,
    /// The chains, in source order.
    pub chains: Vec<Chain>,
    /// Leniencies the scanner applied. Empty for well-formed input.
    pub quirks: Vec<(Quirk, Span)>,
}

/// One `,`-separated run of filters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Chain {
    pub filters: Vec<FilterSpec>,
    pub span: Span,
}

/// One filter as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterSpec {
    /// The registered filter name, decoded.
    pub name: String,
    /// The explicit `@id`, if the user gave one.
    pub instance: Option<String>,
    /// The argument text, decoded once at the graph level. `None` when the
    /// filter was written without `=`; `Some("")` for a bare `name=`.
    pub args: Option<String>,
    /// Leading `[label]`s.
    pub inputs: Vec<Label>,
    /// Trailing `[label]`s.
    pub outputs: Vec<Label>,
    /// The whole filter, labels included.
    pub span: Span,
    /// Just the name (and `@id`).
    pub name_span: Span,
    /// Just the argument text.
    pub args_span: Span,
}

/// A link label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    pub span: Span,
}

/// One argument of a filter, still carrying option-level escaping.
///
/// The value is **not** unescaped here, because the correct unescaping depends
/// on the option's type: a list-valued option splits on `|` first and unescapes
/// each element after. Doing it eagerly is the classic source of "why does my
/// regex option need four backslashes".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arg {
    /// `Some` for `key=value`, `None` for a positional argument.
    pub key: Option<String>,
    /// The value, still escaped at the option level.
    pub raw_value: String,
    /// Where this argument sits **inside the decoded argument string**, not
    /// inside the original source. The graph level already removed one level of
    /// escaping, so the two do not line up; `FilterSpec::args_span` is the
    /// source anchor a diagnostic should use.
    pub span: Span,
}

impl Arg {
    /// The value with one level of escaping removed — what a scalar option
    /// receives.
    #[must_use]
    pub fn value(&self) -> String {
        lex::unescape(&self.raw_value)
    }

    /// The value split on unescaped `|` and unescaped — what a list-valued
    /// option receives.
    #[must_use]
    pub fn list_values(&self) -> Vec<String> {
        lex::split_raw(&self.raw_value, StopSet::LIST)
            .into_iter()
            .map(|(p, _)| lex::unescape(p))
            .collect()
    }
}

impl FilterSpec {
    /// The instance name this filter will be known by.
    ///
    /// `index` is its position among **all** filters in the graph, which is
    /// what the reference counts.
    #[must_use]
    pub fn instance_name(&self, index: usize) -> String {
        self.instance.as_ref().map_or_else(
            || format!("Parsed_{}_{index}", self.name),
            |id| format!("{}@{id}", self.name),
        )
    }

    /// Split the argument text into positional and `key=value` arguments.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::PositionalAfterNamed`] — mixing is legal, reversing the
    /// order is not.
    pub fn arguments(&self) -> Result<Vec<Arg>, GraphError> {
        let Some(args) = self.args.as_deref() else {
            return Ok(Vec::new());
        };
        if args.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut seen_named = false;
        for (piece, span) in lex::split_raw(args, StopSet::ARG) {
            if let Some((k, v)) = lex::split_once_raw(piece, StopSet::EQ) {
                seen_named = true;
                out.push(Arg {
                    key: Some(lex::unescape(k)),
                    raw_value: v.to_owned(),
                    span,
                });
            } else {
                if seen_named {
                    return Err(GraphError::new(
                        ErrorKind::PositionalAfterNamed(lex::unescape(piece)),
                        self.args_span,
                    ));
                }
                out.push(Arg {
                    key: None,
                    raw_value: piece.to_owned(),
                    span,
                });
            }
        }
        Ok(out)
    }
}

/// Parse a filtergraph description.
///
/// # Errors
///
/// A [`GraphError`] anchored to the offending span. Call
/// [`GraphError::render`] with the same source for a caret diagnostic.
pub fn parse(src: &str) -> Result<Ast, GraphError> {
    let mut at = 0usize;
    let mut ast = Ast::default();

    skip_ws(src, &mut at);
    if src.get(at..).is_some_and(|s| s.starts_with(SWS_PREFIX)) {
        at = at.saturating_add(SWS_PREFIX.len());
        let token = next_token(src, &mut at, StopSet::SEMI);
        ast.quirks.extend(token.quirks.iter().copied());
        ast.sws_flags = Some(token.text);
        // The `;` is required: `sws_flags=bicubic` alone is not a graph.
        if peek(src, at) == Some(';') {
            at = at.saturating_add(1);
        }
    }

    skip_ws(src, &mut at);
    if at >= src.len() {
        return Err(GraphError::new(ErrorKind::EmptyGraph, Span::at(at)));
    }

    loop {
        let chain = parse_chain(src, &mut at, &mut ast.quirks)?;
        ast.chains.push(chain);
        skip_ws(src, &mut at);
        if peek(src, at) != Some(';') {
            break;
        }
        at = at.saturating_add(1);
        skip_ws(src, &mut at);
        if at >= src.len() {
            break;
        }
    }

    skip_ws(src, &mut at);
    if at < src.len() {
        let rest = src.get(at..).unwrap_or_default().to_owned();
        return Err(GraphError::new(
            ErrorKind::TrailingGarbage(rest),
            Span::new(at, src.len()),
        ));
    }
    if ast.chains.is_empty() {
        return Err(GraphError::new(ErrorKind::EmptyGraph, Span::at(0)));
    }
    Ok(ast)
}

fn parse_chain(
    src: &str,
    at: &mut usize,
    quirks: &mut Vec<(Quirk, Span)>,
) -> Result<Chain, GraphError> {
    let start = *at;
    let mut filters = Vec::new();
    loop {
        filters.push(parse_filter(src, at, quirks)?);
        skip_ws(src, at);
        if peek(src, *at) != Some(',') {
            break;
        }
        *at = at.saturating_add(1);
        skip_ws(src, at);
        if *at >= src.len() {
            break;
        }
    }
    Ok(Chain {
        filters,
        span: Span::new(start, *at),
    })
}

fn parse_filter(
    src: &str,
    at: &mut usize,
    quirks: &mut Vec<(Quirk, Span)>,
) -> Result<FilterSpec, GraphError> {
    skip_ws(src, at);
    let start = *at;
    let inputs = parse_labels(src, at, quirks)?;

    let name_token = next_token(src, at, StopSet::NAME);
    quirks.extend(name_token.quirks.iter().copied());
    if name_token.is_empty() {
        return Err(GraphError::new(
            ErrorKind::EmptyFilterName,
            Span::at(name_token.span.start),
        ));
    }
    let (name, instance) = match name_token.text.split_once('@') {
        Some((n, i)) => (n.to_owned(), Some(i.to_owned())),
        None => (name_token.text.clone(), None),
    };
    // `@x` splits into an empty name and an instance tag. The reference reports
    // it the same way as a missing name — found by `graph_parse` at exec 667.
    //   ffmpeg -f lavfi -i "@x"  ->  No such filter: ''
    if name.is_empty() {
        return Err(GraphError::new(
            ErrorKind::EmptyFilterName,
            Span::at(name_token.span.start),
        ));
    }

    let mut args = None;
    let mut args_span = Span::at(*at);
    if peek(src, *at) == Some('=') {
        *at = at.saturating_add(1);
        let token = next_token(src, at, StopSet::GRAPH);
        quirks.extend(token.quirks.iter().copied());
        args_span = token.span;
        args = Some(token.text);
    }

    let outputs = parse_labels(src, at, quirks)?;
    skip_ws(src, at);
    match peek(src, *at) {
        None | Some(',' | ';') => {}
        Some(_) => {
            let rest = src.get(*at..).unwrap_or_default();
            let stop = rest
                .find([',', ';'])
                .map_or(src.len(), |i| at.saturating_add(i));
            return Err(GraphError::new(
                ErrorKind::TrailingGarbage(
                    src.get(*at..stop).unwrap_or_default().trim().to_owned(),
                ),
                Span::new(*at, stop),
            ));
        }
    }

    Ok(FilterSpec {
        name,
        instance,
        args,
        inputs,
        outputs,
        span: Span::new(start, *at),
        name_span: name_token.span,
        args_span,
    })
}

fn parse_labels(
    src: &str,
    at: &mut usize,
    quirks: &mut Vec<(Quirk, Span)>,
) -> Result<Vec<Label>, GraphError> {
    let mut out = Vec::new();
    loop {
        skip_ws(src, at);
        if peek(src, *at) != Some('[') {
            return Ok(out);
        }
        let open = *at;
        *at = at.saturating_add(1);
        let token = next_token(src, at, StopSet::LABEL);
        quirks.extend(token.quirks.iter().copied());
        if peek(src, *at) != Some(']') {
            return Err(GraphError::new(
                ErrorKind::UnterminatedLabel,
                Span::new(open, src.len()),
            ));
        }
        *at = at.saturating_add(1);
        if token.is_empty() {
            return Err(GraphError::new(ErrorKind::EmptyLabel, Span::new(open, *at)));
        }
        out.push(Label {
            name: token.text,
            span: Span::new(open, *at),
        });
    }
}

impl Ast {
    /// A copy with every span zeroed, for comparing two parses of differently
    /// spelled but equivalent descriptions.
    ///
    /// Printing normalises escaping, so `parse(print(a))` has the same
    /// structure as `a` but not the same offsets. This is what the round-trip
    /// property compares.
    #[must_use]
    pub fn without_spans(&self) -> Self {
        Self {
            sws_flags: self.sws_flags.clone(),
            quirks: Vec::new(),
            chains: self
                .chains
                .iter()
                .map(|c| Chain {
                    span: Span::default(),
                    filters: c
                        .filters
                        .iter()
                        .map(|f| FilterSpec {
                            name: f.name.clone(),
                            instance: f.instance.clone(),
                            args: f.args.clone(),
                            inputs: strip(&f.inputs),
                            outputs: strip(&f.outputs),
                            span: Span::default(),
                            name_span: Span::default(),
                            args_span: Span::default(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

fn strip(labels: &[Label]) -> Vec<Label> {
    labels
        .iter()
        .map(|l| Label {
            name: l.name.clone(),
            span: Span::default(),
        })
        .collect()
}

impl core::fmt::Display for Ast {
    /// Print a description that parses back to an equal [`Ast`].
    ///
    /// Not a reproduction of what the user typed — the escaping is normalised
    /// to backslashes — but `parse(print(parse(s))) == parse(s)`, which is what
    /// `-dumpgraph` and a future GUI need.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(flags) = &self.sws_flags {
            write!(f, "{}{};", SWS_PREFIX, lex::escape(flags, StopSet::SEMI))?;
        }
        for (i, chain) in self.chains.iter().enumerate() {
            if i > 0 {
                f.write_str(";")?;
            }
            for (j, filter) in chain.filters.iter().enumerate() {
                if j > 0 {
                    f.write_str(",")?;
                }
                for label in &filter.inputs {
                    write!(f, "[{}]", lex::escape(&label.name, StopSet::LABEL))?;
                }
                // A filter literally named `sws_flags`, written first with
                // arguments and no labels, would print as the prefix and parse
                // back as an empty graph. Found by `graph_hostile` on
                // `\sws_flags=x|y;`. One escaped byte keeps it a name.
                let collides_with_prefix = i == 0
                    && j == 0
                    && self.sws_flags.is_none()
                    && filter.inputs.is_empty()
                    && filter.instance.is_none()
                    && filter.args.is_some()
                    && SWS_PREFIX.strip_suffix('=') == Some(filter.name.as_str());
                if collides_with_prefix {
                    f.write_str("\\")?;
                }
                f.write_str(&lex::escape(&filter.name, StopSet::NAME))?;
                if let Some(id) = &filter.instance {
                    write!(f, "@{}", lex::escape(id, StopSet::NAME))?;
                }
                if let Some(args) = &filter.args {
                    write!(f, "={}", lex::escape(args, StopSet::GRAPH))?;
                }
                for label in &filter.outputs {
                    write!(f, "[{}]", lex::escape(&label.name, StopSet::LABEL))?;
                }
            }
        }
        Ok(())
    }
}
