//! The option-grammar escaping scheme.
//!
//! # Where this belongs
//!
//! Plan 11 §4.2 places `escape` in `vaco-core`. `vaco-core` is currently a stub
//! that does not have it, and `vaco-opts` cannot be written without it, so it
//! lives here for now. When `vaco-core::escape` lands, delete this module and
//! re-export that one — the API below is deliberately the same shape.
//!
//! # The scheme
//!
//! One level of the grammar has three ways to write a character:
//!
//! * bare, when it is not special at this level;
//! * backslash-escaped, `\:`;
//! * inside single quotes, `'a:b'`, where everything up to the next `'` is
//!   literal. A literal `'` is written by closing the quote, escaping it and
//!   reopening: `'a'\''b'` decodes to `a'b`.
//!
//! Levels nest: [`escape`] always escapes the backslash itself, so escaping an
//! already-escaped string and unescaping twice is the identity.

use std::borrow::Cow;

/// Characters that are special inside an option *value* at the `k=v:k=v` level.
pub const OPT_VALUE_SPECIAL: &str = ":=";

/// How [`escape`] should encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Leave the string alone when nothing needs escaping; otherwise
    /// [`Mode::Backslash`].
    #[default]
    Auto,
    /// Always backslash-escape.
    Backslash,
    /// Always wrap in single quotes.
    Quote,
}

/// A malformed escape sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscapeError {
    /// A `'` was opened and never closed.
    UnterminatedQuote,
    /// The string ends with a lone `\`.
    TrailingBackslash,
}

impl core::fmt::Display for EscapeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnterminatedQuote => f.write_str("unterminated single quote"),
            Self::TrailingBackslash => f.write_str("trailing backslash"),
        }
    }
}

impl std::error::Error for EscapeError {}

fn needs_escaping(s: &str, special: &str) -> bool {
    s.chars()
        .any(|c| c == '\\' || c == '\'' || special.contains(c))
}

/// Escape `special` (and always `\` and `'`) for one level of the grammar.
#[must_use]
pub fn escape(s: &str, special: &str, mode: Mode) -> String {
    match mode {
        Mode::Auto if !needs_escaping(s, special) => s.to_owned(),
        Mode::Auto | Mode::Backslash => {
            let mut out = String::with_capacity(s.len() + 4);
            for c in s.chars() {
                if c == '\\' || c == '\'' || special.contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
            out
        }
        Mode::Quote => {
            let mut out = String::with_capacity(s.len() + 4);
            out.push('\'');
            for c in s.chars() {
                if c == '\'' {
                    // close, escaped quote, reopen
                    out.push_str("'\\''");
                } else {
                    out.push(c);
                }
            }
            out.push('\'');
            out
        }
    }
}

/// Reverse one level of [`escape`].
///
/// # Errors
///
/// [`EscapeError`] for an unterminated quote or a trailing backslash.
pub fn unescape(s: &str) -> Result<String, EscapeError> {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    let mut in_quote = false;
    while let Some(c) = it.next() {
        match c {
            '\'' => in_quote = !in_quote,
            '\\' if !in_quote => match it.next() {
                Some(n) => out.push(n),
                None => return Err(EscapeError::TrailingBackslash),
            },
            _ => out.push(c),
        }
    }
    if in_quote {
        return Err(EscapeError::UnterminatedQuote);
    }
    Ok(out)
}

/// Split on any character in `seps`, honouring quotes and backslashes, and
/// return the pieces **still escaped**.
///
/// Splitting before unescaping is what makes nesting work: the outer level
/// finds its own separators without being confused by the inner level's, and
/// each piece is unescaped exactly once on the way down.
///
/// # Errors
///
/// [`EscapeError`] for an unterminated quote or a trailing backslash.
pub fn split_raw<'a>(s: &'a str, seps: &str) -> Result<Vec<&'a str>, EscapeError> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut in_quote = false;
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '\'' => in_quote = !in_quote,
            '\\' if !in_quote => {
                if it.next().is_none() {
                    return Err(EscapeError::TrailingBackslash);
                }
            }
            _ if !in_quote && seps.contains(c) => {
                out.push(s.get(start..i).unwrap_or_default());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    if in_quote {
        return Err(EscapeError::UnterminatedQuote);
    }
    out.push(s.get(start..).unwrap_or_default());
    Ok(out)
}

/// Split on `sep` and unescape each piece.
///
/// # Errors
///
/// [`EscapeError`] for an unterminated quote or a trailing backslash.
pub fn split(s: &str, sep: char) -> Result<Vec<Cow<'_, str>>, EscapeError> {
    let mut buf = [0u8; 4];
    let seps: &str = sep.encode_utf8(&mut buf);
    split_raw(s, seps)?
        .into_iter()
        .map(|p| {
            if needs_unescaping(p) {
                unescape(p).map(Cow::Owned)
            } else {
                Ok(Cow::Borrowed(p))
            }
        })
        .collect()
}

fn needs_unescaping(s: &str) -> bool {
    s.contains('\\') || s.contains('\'')
}

/// Split once at the first unescaped character in `seps`.
///
/// Returns `None` when no separator is present, which is how a positional
/// argument is recognised.
///
/// # Errors
///
/// [`EscapeError`] for an unterminated quote or a trailing backslash.
pub fn split_once_raw<'a>(
    s: &'a str,
    seps: &str,
) -> Result<Option<(&'a str, &'a str)>, EscapeError> {
    let mut in_quote = false;
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '\'' => in_quote = !in_quote,
            '\\' if !in_quote => {
                if it.next().is_none() {
                    return Err(EscapeError::TrailingBackslash);
                }
            }
            _ if !in_quote && seps.contains(c) => {
                let head = s.get(..i).unwrap_or_default();
                let tail = s.get(i + c.len_utf8()..).unwrap_or_default();
                return Ok(Some((head, tail)));
            }
            _ => {}
        }
    }
    if in_quote {
        return Err(EscapeError::UnterminatedQuote);
    }
    Ok(None)
}
