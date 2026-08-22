//! The parse error taxonomy.
//!
//! The variants mirror the four rejection points the reference binary
//! distinguishes, because *which* string is rejected is observable on a command
//! line and the message is what a user sees when their filtergraph fails.

use core::fmt;

/// Where and why an expression failed to parse.
///
/// `offset` is a byte offset into the **whitespace-stripped** source, not into
/// the string the caller passed in. The reference strips whitespace before
/// parsing (see [`crate::lex::strip_whitespace`]), so there is no position in
/// the original string that corresponds to a parser state; reporting the
/// stripped offset at least lets a caller slice [`ParseError::source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// What went wrong.
    pub kind: ParseErrorKind,
    /// Byte offset into the whitespace-stripped source where it went wrong.
    pub offset: usize,
    /// The whitespace-stripped source, from `offset` to the end, truncated.
    ///
    /// Kept so the message can quote the offending tail the way the reference
    /// does (`Unknown function in 'nosuchfn(1)'`).
    pub tail: String,
}

/// The kind of parse failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseErrorKind {
    /// An identifier was neither a known constant/variable nor followed by `(`.
    UndefinedConstant,
    /// An identifier was followed by `(` but names no known function.
    UnknownFunction,
    /// A function was called with a number of arguments it does not accept.
    WrongArity,
    /// A `(` was never closed, or more than three arguments were supplied.
    MissingCloseParen,
    /// The expression parsed, but characters were left over.
    TrailingGarbage,
    /// Nesting exceeded the depth limit.
    ///
    /// Both limits live in [`crate::Limits`]; the defaults reproduce the
    /// reference's acceptance boundary exactly.
    TooDeep,
}

impl ParseErrorKind {
    #[must_use]
    const fn message(self) -> &'static str {
        match self {
            Self::UndefinedConstant => "undefined constant or missing '('",
            Self::UnknownFunction => "unknown function",
            Self::WrongArity => "wrong number of arguments",
            Self::MissingCloseParen => "missing ')' or too many args",
            Self::TrailingGarbage => "invalid chars at the end of expression",
            Self::TooDeep => "expression nests too deeply",
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} in '{}'", self.kind.message(), self.tail)
    }
}

impl core::error::Error for ParseError {}

impl From<ParseError> for vaco_core::Error {
    fn from(e: ParseError) -> Self {
        Self::Option {
            name: "expr".to_owned(),
            detail: e.to_string(),
        }
    }
}
