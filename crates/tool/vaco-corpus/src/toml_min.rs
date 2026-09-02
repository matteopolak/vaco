//! A deliberately tiny TOML reader/writer for exactly this crate's two file
//! schemas (`vaco-media.lock` and a suite catalogue).
//!
//! # Why this exists
//!
//! No TOML crate is in `[workspace.dependencies]` (D10: an agent stops and
//! asks rather than adding a dependency silently), and `vaco-conformance`
//! already answered the same need with its own from-scratch reader for its
//! own file shapes. This crate does not depend on `vaco-conformance` — that
//! would pull a video/audio filter graph into a corpus-fetching tool for one
//! parser function — so it writes the same kind of small, purpose-built
//! reader again, restricted to what `lock.rs` actually needs.
//!
//! # The subset
//!
//! Top-level `schema = <int>` and other scalar keys, plus any number of
//! `[[entry]]`-style array-of-tables blocks. Values: basic strings (with `\"`,
//! `\\`, `\n`, `\t` escapes), integers, booleans, and flat string arrays
//! (`["a", "b"]`, no nesting). No inline tables, no dotted keys, no
//! multi-line strings, no floats, no nested arrays of tables. A file needing
//! more than this should not extend this parser — it should be the moment to
//! request a real TOML dependency, exactly as `vaco-conformance`'s copy says.
//!
//! # How to change it
//!
//! Add the construct to [`Parser::value`], and a case to the tests at the
//! bottom of this file.

use std::collections::BTreeMap;
use std::fmt;

/// A parsed scalar or array value.
#[derive(Debug, Clone, PartialEq)]
pub enum TomlValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    Array(Vec<TomlValue>),
}

impl TomlValue {
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(n) => Some(*n),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_array(&self) -> Option<&[TomlValue]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }
}

/// One `[key = value, ...]` table: the top level, or one array-of-tables entry.
pub type Table = BTreeMap<String, TomlValue>;

/// A parsed document: the top-level table, plus every named array-of-tables
/// section in file order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub top: Table,
    /// `(section name, entries in file order)`, one item per distinct
    /// `[[name]]` header encountered. Two headers with the same name append
    /// to the same `Vec`, matching TOML's own array-of-tables semantics.
    pub sections: Vec<(String, Vec<Table>)>,
}

impl Document {
    /// All entries under a named array-of-tables section, or an empty slice
    /// if the document has none.
    #[must_use]
    pub fn section(&self, name: &str) -> &[Table] {
        self.sections
            .iter()
            .find(|(n, _)| n == name)
            .map_or(&[], |(_, rows)| rows.as_slice())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TomlError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for TomlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for TomlError {}

/// Parse `src` into a [`Document`].
///
/// # Errors
/// A syntax error outside the supported subset, with the 1-based line number.
pub fn parse(src: &str) -> Result<Document, TomlError> {
    let mut doc = Document::default();
    let mut current: Option<(String, Table)> = None;

    for (idx, raw_line) in src.lines().enumerate() {
        let line_no = idx + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        if let Some(name) = line.strip_prefix("[[").and_then(|s| s.strip_suffix("]]")) {
            if let Some((name, table)) = current.take() {
                push_section(&mut doc, name, table);
            }
            current = Some((name.trim().to_owned(), Table::new()));
            continue;
        }

        let (key, value_src) = line.split_once('=').ok_or_else(|| TomlError {
            line: line_no,
            message: "expected `key = value`".to_owned(),
        })?;
        let key = key.trim();
        if key.is_empty() {
            return Err(TomlError {
                line: line_no,
                message: "empty key".to_owned(),
            });
        }
        let value = parse_value(value_src.trim(), line_no)?;

        match current.as_mut() {
            Some((_, table)) => {
                table.insert(key.to_owned(), value);
            }
            None => {
                doc.top.insert(key.to_owned(), value);
            }
        }
    }

    if let Some((name, table)) = current {
        push_section(&mut doc, name, table);
    }

    Ok(doc)
}

fn push_section(doc: &mut Document, name: String, table: Table) {
    if let Some((_, rows)) = doc.sections.iter_mut().find(|(n, _)| *n == name) {
        rows.push(table);
    } else {
        doc.sections.push((name, vec![table]));
    }
}

/// Strip a `#` comment that starts outside a quoted string.
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else if c == '#' {
            let Some(prefix) = line.get(..i) else {
                return line;
            };
            return prefix;
        }
    }
    line
}

fn parse_value(src: &str, line_no: usize) -> Result<TomlValue, TomlError> {
    if let Some(inner) = src.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return Ok(TomlValue::String(unescape(inner)));
    }
    if src == "true" {
        return Ok(TomlValue::Boolean(true));
    }
    if src == "false" {
        return Ok(TomlValue::Boolean(false));
    }
    if let Some(inner) = src.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let mut items = Vec::new();
        for part in split_top_level_commas(inner) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            items.push(parse_value(part, line_no)?);
        }
        return Ok(TomlValue::Array(items));
    }
    if let Ok(n) = src.replace('_', "").parse::<i64>() {
        return Ok(TomlValue::Integer(n));
    }
    Err(TomlError {
        line: line_no,
        message: format!("unsupported value: {src}"),
    })
}

fn split_top_level_commas(src: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut start = 0usize;
    for (i, c) in src.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            ',' => {
                if let Some(part) = src.get(start..i) {
                    parts.push(part);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if let Some(part) = src.get(start..) {
        parts.push(part);
    }
    parts
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') | None => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Render a string as a quoted TOML basic string.
#[must_use]
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render a flat string array as `["a", "b"]`.
#[must_use]
pub fn quote_array(items: &[String]) -> String {
    let rendered: Vec<String> = items.iter().map(|s| quote(s)).collect();
    format!("[{}]", rendered.join(", "))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{TomlValue, parse};

    #[test]
    fn top_level_scalars() {
        let doc = parse("schema = 1\nname = \"vaco\"\nok = true\n").expect("parses");
        assert_eq!(doc.top.get("schema"), Some(&TomlValue::Integer(1)));
        assert_eq!(
            doc.top.get("name"),
            Some(&TomlValue::String("vaco".to_owned()))
        );
        assert_eq!(doc.top.get("ok"), Some(&TomlValue::Boolean(true)));
    }

    #[test]
    fn array_of_tables() {
        let src = "schema = 1\n\n[[entry]]\nname = \"a\"\nsize = 10\n\n[[entry]]\nname = \"b\"\nsize = 20\n";
        let doc = parse(src).expect("parses");
        let rows = doc.section("entry");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.first().and_then(|t| t.get("name")),
            Some(&TomlValue::String("a".to_owned()))
        );
        assert_eq!(
            rows.get(1).and_then(|t| t.get("size")),
            Some(&TomlValue::Integer(20))
        );
    }

    #[test]
    fn string_arrays_and_escapes() {
        let src = "[[entry]]\ntargets = [\"a\", \"b\", \"c\"]\nnote = \"line\\nbreak\"\n";
        let doc = parse(src).expect("parses");
        let row = doc.section("entry").first().expect("one row");
        let Some(TomlValue::Array(items)) = row.get("targets") else {
            panic!("expected array");
        };
        assert_eq!(items.len(), 3);
        assert_eq!(
            row.get("note").and_then(TomlValue::as_str),
            Some("line\nbreak")
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let src = "# a comment\nschema = 1 # trailing\n\n";
        let doc = parse(src).expect("parses");
        assert_eq!(doc.top.get("schema"), Some(&TomlValue::Integer(1)));
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        let doc = parse("note = \"a # b\"\n").expect("parses");
        assert_eq!(
            doc.top.get("note").and_then(TomlValue::as_str),
            Some("a # b")
        );
    }

    #[test]
    fn malformed_line_is_rejected() {
        assert!(parse("not a key value line").is_err());
    }

    #[test]
    fn round_trip_through_quote() {
        let raw = "weird \"quoted\" \\ text";
        let quoted = super::quote(raw);
        let doc = parse(&format!("x = {quoted}\n")).expect("parses");
        assert_eq!(doc.top.get("x").and_then(TomlValue::as_str), Some(raw));
    }
}
