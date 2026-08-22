//! A deliberately small TOML reader.
//!
//! # Why this exists
//!
//! Plan 13 §1.5.1 specifies the manifest format as TOML, and §1.4.1 specifies
//! the divergence register as TOML. The workspace dependency list (D10) does
//! not declare a TOML crate, and adding one is a reviewed decision that is not
//! this crate's to make — so the harness reads the subset of TOML its own file
//! formats use, and nothing else.
//!
//! # The subset
//!
//! Supported: comments, bare and quoted keys, dotted keys in headers,
//! `[table]`, `[[array-of-table]]`, basic strings with the standard escapes,
//! multi-line basic strings (`"""`), literal strings (`'`), integers with `_`
//! separators, floats, booleans, arrays (including multi-line and trailing
//! commas), inline tables, and bare local dates (`2027-09-04`), which are
//! retained as [`Value::Date`] strings.
//!
//! Not supported, and rejected with an error rather than silently mis-parsed:
//! dotted keys on the left of an assignment, multi-line literal strings,
//! date-times with a time component, hex/octal/binary integers, and `inf`/`nan`.
//! Every one of those is a parse error, so a manifest that uses them fails
//! loudly the first time anyone writes it.
//!
//! # How to change it
//!
//! Add the construct to [`Parser::value`] (for a value form) or to
//! [`Parser::header`] (for a table form), and add a case to the tests at the
//! bottom of this file. Resist growing it into a general TOML implementation:
//! the moment the manifests need more than this, the right move is to request
//! a workspace dependency, not to keep extending a bespoke parser.

use std::collections::BTreeMap;
use std::fmt;

/// A parsed TOML value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A string, with escapes already resolved.
    String(String),
    /// A signed integer.
    Integer(i64),
    /// A floating-point number.
    Float(f64),
    /// `true` or `false`.
    Boolean(bool),
    /// A bare local date, kept in `YYYY-MM-DD` form.
    Date(String),
    /// An array. Heterogeneous arrays are accepted; callers type-check.
    Array(Vec<Value>),
    /// A table, ordered by key so that reports are deterministic.
    Table(Table),
}

/// A TOML table.
pub type Table = BTreeMap<String, Value>;

/// A parse failure, with the line it occurred on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TomlError {
    /// 1-based line number.
    pub line: usize,
    /// What went wrong.
    pub message: String,
}

impl fmt::Display for TomlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for TomlError {}

impl Value {
    /// The string inside, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) | Self::Date(s) => Some(s),
            _ => None,
        }
    }

    /// The integer inside, if this is one.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// The number inside as `f64`, accepting an integer.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            Self::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// The boolean inside, if this is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    /// The array inside, if this is one.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The table inside, if this is one.
    #[must_use]
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Self::Table(t) => Some(t),
            _ => None,
        }
    }

    /// An array of strings, rejecting any non-string element.
    #[must_use]
    pub fn as_str_array(&self) -> Option<Vec<String>> {
        self.as_array()?
            .iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect()
    }

    /// A short name for the value's kind, for error messages.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::String(_) => "string",
            Self::Integer(_) => "integer",
            Self::Float(_) => "float",
            Self::Boolean(_) => "boolean",
            Self::Date(_) => "date",
            Self::Array(_) => "array",
            Self::Table(_) => "table",
        }
    }
}

/// Parse a whole document.
///
/// # Errors
/// [`TomlError`] on any construct outside the supported subset, on a duplicate
/// key, or on malformed syntax.
pub fn parse(input: &str) -> Result<Table, TomlError> {
    let mut p = Parser {
        bytes: input.as_bytes(),
        pos: 0,
        line: 1,
    };
    p.document()
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    line: usize,
}

impl Parser<'_> {
    fn err<T>(&self, message: impl Into<String>) -> Result<T, TomlError> {
        Err(TomlError {
            line: self.line,
            message: message.into(),
        })
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.pos + n).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
        }
        Some(b)
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            if b == b'\n' {
                self.line += 1;
            }
            true
        } else {
            false
        }
    }

    /// Horizontal whitespace only.
    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    /// Whitespace, newlines and comments — everything with no semantic weight.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => {
                    self.bump();
                }
                Some(b'#') => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// After a value: inline whitespace, an optional comment, then end of line.
    fn end_of_line(&mut self) -> Result<(), TomlError> {
        self.skip_inline_ws();
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.pos += 1;
            }
        }
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.bump();
                Ok(())
            }
            Some(b'\r') if self.peek_at(1) == Some(b'\n') => {
                self.pos += 1;
                self.bump();
                Ok(())
            }
            Some(b) => self.err(format!("unexpected `{}` after value", b as char)),
        }
    }

    fn document(&mut self) -> Result<Table, TomlError> {
        let mut root = Table::new();
        // The table that bare `key = value` lines currently land in.
        let mut path: Vec<String> = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                None => return Ok(root),
                Some(b'[') => path = self.header(&mut root)?,
                _ => {
                    let (key, value) = self.assignment()?;
                    let target = descend(&mut root, &path, self.line)?;
                    if target.insert(key.clone(), value).is_some() {
                        return self.err(format!("duplicate key `{key}`"));
                    }
                }
            }
        }
    }

    /// `[a.b]` or `[[a.b]]`. Returns the new current path.
    fn header(&mut self, root: &mut Table) -> Result<Vec<String>, TomlError> {
        self.bump(); // '['
        let array = self.eat(b'[');
        let mut path = Vec::new();
        loop {
            self.skip_inline_ws();
            path.push(self.key()?);
            self.skip_inline_ws();
            if self.eat(b'.') {
                continue;
            }
            break;
        }
        if !self.eat(b']') {
            return self.err("expected `]`");
        }
        if array && !self.eat(b']') {
            return self.err("expected `]]`");
        }
        self.end_of_line()?;

        if array {
            let Some((last, parents)) = path.split_last() else {
                return self.err("empty table header");
            };
            let parent = descend(root, parents, self.line)?;
            let slot = parent
                .entry(last.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            match slot {
                Value::Array(items) => items.push(Value::Table(Table::new())),
                other => {
                    return self.err(format!(
                        "`{last}` was already a {}, cannot be an array of tables",
                        other.kind()
                    ));
                }
            }
        } else {
            // Creating it is enough; `descend` walks arrays-of-tables by
            // taking their last element, which is what TOML requires.
            descend(root, &path, self.line)?;
        }
        Ok(path)
    }

    fn assignment(&mut self) -> Result<(String, Value), TomlError> {
        let key = self.key()?;
        self.skip_inline_ws();
        if !self.eat(b'=') {
            return self.err(format!("expected `=` after key `{key}`"));
        }
        self.skip_inline_ws();
        let value = self.value()?;
        self.end_of_line()?;
        Ok((key, value))
    }

    fn key(&mut self) -> Result<String, TomlError> {
        match self.peek() {
            Some(b'"') => self.basic_string(),
            Some(b'\'') => self.literal_string(),
            _ => {
                let start = self.pos;
                while matches!(self.peek(), Some(b) if b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    self.pos += 1;
                }
                if self.pos == start {
                    return self.err("expected a key");
                }
                let slice = self.bytes.get(start..self.pos).unwrap_or_default();
                String::from_utf8(slice.to_vec()).map_err(|_| TomlError {
                    line: self.line,
                    message: "key is not UTF-8".into(),
                })
            }
        }
    }

    fn value(&mut self) -> Result<Value, TomlError> {
        match self.peek() {
            Some(b'"') => {
                if self.peek_at(1) == Some(b'"') && self.peek_at(2) == Some(b'"') {
                    self.multiline_string()
                } else {
                    self.basic_string().map(Value::String)
                }
            }
            Some(b'\'') => self.literal_string().map(Value::String),
            Some(b'[') => self.array(),
            Some(b'{') => self.inline_table(),
            Some(b't' | b'f') => self.boolean(),
            Some(b) if b == b'-' || b == b'+' || b.is_ascii_digit() => self.number_or_date(),
            Some(b) => self.err(format!("unexpected `{}` at start of value", b as char)),
            None => self.err("unexpected end of input"),
        }
    }

    fn boolean(&mut self) -> Result<Value, TomlError> {
        for (word, val) in [("true", true), ("false", false)] {
            let end = self.pos + word.len();
            if self.bytes.get(self.pos..end) == Some(word.as_bytes()) {
                self.pos = end;
                return Ok(Value::Boolean(val));
            }
        }
        self.err("expected `true` or `false`")
    }

    fn number_or_date(&mut self) -> Result<Value, TomlError> {
        let start = self.pos;
        while matches!(self.peek(), Some(b) if b.is_ascii_digit()
            || b == b'-' || b == b'+' || b == b'_' || b == b'.' || b == b'e' || b == b'E')
        {
            self.pos += 1;
        }
        let raw = self
            .bytes
            .get(start..self.pos)
            .and_then(|s| std::str::from_utf8(s).ok())
            .unwrap_or_default()
            .to_owned();
        if raw.is_empty() {
            return self.err("expected a number");
        }
        // A bare local date: `YYYY-MM-DD`, with the dashes in the interior.
        if raw.len() == 10 && is_local_date(&raw) {
            return Ok(Value::Date(raw));
        }
        let cleaned = raw.replace('_', "");
        if cleaned.contains(['.', 'e', 'E']) {
            return cleaned
                .parse::<f64>()
                .map(Value::Float)
                .map_or_else(|_| self.err(format!("`{raw}` is not a number")), Ok);
        }
        cleaned
            .parse::<i64>()
            .map(Value::Integer)
            .map_or_else(|_| self.err(format!("`{raw}` is not an integer")), Ok)
    }

    fn array(&mut self) -> Result<Value, TomlError> {
        self.bump(); // '['
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            if self.eat(b']') {
                return Ok(Value::Array(items));
            }
            items.push(self.value()?);
            self.skip_trivia();
            if self.eat(b',') {
                continue;
            }
            self.skip_trivia();
            if self.eat(b']') {
                return Ok(Value::Array(items));
            }
            return self.err("expected `,` or `]` in array");
        }
    }

    fn inline_table(&mut self) -> Result<Value, TomlError> {
        self.bump(); // '{'
        let mut table = Table::new();
        loop {
            self.skip_trivia();
            if self.eat(b'}') {
                return Ok(Value::Table(table));
            }
            let key = self.key()?;
            self.skip_trivia();
            if !self.eat(b'=') {
                return self.err(format!("expected `=` after key `{key}`"));
            }
            self.skip_trivia();
            let value = self.value()?;
            if table.insert(key.clone(), value).is_some() {
                return self.err(format!("duplicate key `{key}` in inline table"));
            }
            self.skip_trivia();
            if self.eat(b',') {
                continue;
            }
            self.skip_trivia();
            if self.eat(b'}') {
                return Ok(Value::Table(table));
            }
            return self.err("expected `,` or `}` in inline table");
        }
    }

    fn basic_string(&mut self) -> Result<String, TomlError> {
        self.bump(); // '"'
        let mut out = String::new();
        loop {
            match self.bump() {
                None | Some(b'\n') => return self.err("unterminated string"),
                Some(b'"') => return Ok(out),
                Some(b'\\') => self.escape(&mut out)?,
                Some(b) => push_byte(&mut out, b, self.bytes, &mut self.pos),
            }
        }
    }

    fn multiline_string(&mut self) -> Result<Value, TomlError> {
        self.pos += 3;
        // A newline immediately after the opening delimiter is trimmed.
        if self.peek() == Some(b'\r') && self.peek_at(1) == Some(b'\n') {
            self.pos += 1;
        }
        self.eat(b'\n');
        let mut out = String::new();
        loop {
            if self.peek() == Some(b'"')
                && self.peek_at(1) == Some(b'"')
                && self.peek_at(2) == Some(b'"')
            {
                self.pos += 3;
                return Ok(Value::String(out));
            }
            match self.bump() {
                None => return self.err("unterminated multi-line string"),
                Some(b'\\') => {
                    // A backslash at end of line swallows the newline and the
                    // leading whitespace of the next line.
                    self.skip_inline_ws();
                    if matches!(self.peek(), Some(b'\n' | b'\r')) {
                        self.skip_trivia();
                    } else {
                        self.escape(&mut out)?;
                    }
                }
                Some(b'\r') if self.peek() == Some(b'\n') => {}
                Some(b) => push_byte(&mut out, b, self.bytes, &mut self.pos),
            }
        }
    }

    fn literal_string(&mut self) -> Result<String, TomlError> {
        self.bump(); // '\''
        let mut out = String::new();
        loop {
            match self.bump() {
                None | Some(b'\n') => return self.err("unterminated literal string"),
                Some(b'\'') => return Ok(out),
                Some(b) => push_byte(&mut out, b, self.bytes, &mut self.pos),
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), TomlError> {
        let Some(b) = self.bump() else {
            return self.err("unterminated escape");
        };
        let ch = match b {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'"' => '"',
            b'\\' => '\\',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'0' => '\0',
            b'u' | b'U' => {
                let width = if b == b'u' { 4 } else { 8 };
                let mut code: u32 = 0;
                for _ in 0..width {
                    let Some(d) = self.bump().and_then(|d| (d as char).to_digit(16)) else {
                        return self.err("bad unicode escape");
                    };
                    code = code * 16 + d;
                }
                let Some(ch) = char::from_u32(code) else {
                    return self.err("unicode escape is not a scalar value");
                };
                out.push(ch);
                return Ok(());
            }
            other => return self.err(format!("unknown escape `\\{}`", other as char)),
        };
        out.push(ch);
        Ok(())
    }
}

/// Push one byte of a UTF-8 stream, pulling in the continuation bytes of a
/// multi-byte sequence so that non-ASCII text survives.
fn push_byte(out: &mut String, first: u8, bytes: &[u8], pos: &mut usize) {
    if first.is_ascii() {
        out.push(first as char);
        return;
    }
    let extra = match first {
        0xC2..=0xDF => 1,
        0xE0..=0xEF => 2,
        0xF0..=0xF4 => 3,
        _ => 0,
    };
    let start = pos.saturating_sub(1);
    let end = (start + 1 + extra).min(bytes.len());
    if let Some(slice) = bytes.get(start..end)
        && let Ok(s) = std::str::from_utf8(slice)
    {
        out.push_str(s);
        *pos = end;
    } else {
        out.push(char::REPLACEMENT_CHARACTER);
    }
}

fn is_local_date(raw: &str) -> bool {
    let b = raw.as_bytes();
    let digits_at = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let dash_at = |i: usize| b.get(i) == Some(&b'-');
    (0..4).all(digits_at)
        && dash_at(4)
        && (5..7).all(digits_at)
        && dash_at(7)
        && (8..10).all(digits_at)
}

/// Walk (creating as needed) to the table `path` names, following the last
/// element of any array-of-tables on the way down.
fn descend<'t>(
    root: &'t mut Table,
    path: &[String],
    line: usize,
) -> Result<&'t mut Table, TomlError> {
    let mut cur = root;
    for key in path {
        let slot = cur
            .entry(key.clone())
            .or_insert_with(|| Value::Table(Table::new()));
        cur = match slot {
            Value::Table(t) => t,
            Value::Array(items) => match items.last_mut() {
                Some(Value::Table(t)) => t,
                _ => {
                    return Err(TomlError {
                        line,
                        message: format!("`{key}` is an array of non-tables"),
                    });
                }
            },
            other => {
                return Err(TomlError {
                    line,
                    message: format!("`{key}` is a {}, not a table", other.kind()),
                });
            }
        };
    }
    Ok(cur)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "an out-of-range index or a None in a test is a failing test, \
              which is the correct outcome"
)]
mod tests {
    use super::{Value, parse};

    #[test]
    fn scalars_and_comments() {
        let t = parse(
            r#"
            # a comment
            schema = 1        # trailing comment
            name   = "probe"
            ratio  = 1.5
            neg    = -7
            on     = true
            when   = 2027-09-04
            sep    = 1_000
            "#,
        )
        .expect("parses");
        assert_eq!(t["schema"], Value::Integer(1));
        assert_eq!(t["name"], Value::String("probe".into()));
        assert_eq!(t["ratio"], Value::Float(1.5));
        assert_eq!(t["neg"], Value::Integer(-7));
        assert_eq!(t["on"], Value::Boolean(true));
        assert_eq!(t["when"], Value::Date("2027-09-04".into()));
        assert_eq!(t["sep"], Value::Integer(1000));
    }

    #[test]
    fn arrays_of_tables_and_inline_tables() {
        let t = parse(
            r#"
            [[media]]
            id = "a"
            tags = ["video", "audio"]

            [[media]]
            id = "b"
            scope = { suite = "probe-*", field = "format_long_name" }
            "#,
        )
        .expect("parses");
        let media = t["media"].as_array().expect("array");
        assert_eq!(media.len(), 2);
        let first = media[0].as_table().expect("table");
        assert_eq!(first["id"], Value::String("a".into()));
        assert_eq!(
            first["tags"].as_str_array().expect("strings"),
            vec!["video".to_owned(), "audio".to_owned()]
        );
        let scope = media[1].as_table().expect("table")["scope"]
            .as_table()
            .expect("inline table");
        assert_eq!(scope["suite"], Value::String("probe-*".into()));
    }

    #[test]
    fn multiline_strings_join_and_trim() {
        let t = parse("text = \"\"\"\nline one\nline two\n\"\"\"\n").expect("parses");
        assert_eq!(t["text"], Value::String("line one\nline two\n".into()));
    }

    #[test]
    fn nested_headers() {
        let t =
            parse("[compare]\nmode = \"exact-bytes\"\n[normalise]\noutput = []\n").expect("parses");
        assert_eq!(
            t["compare"].as_table().expect("table")["mode"],
            Value::String("exact-bytes".into())
        );
        assert!(
            t["normalise"].as_table().expect("table")["output"]
                .as_array()
                .expect("array")
                .is_empty()
        );
    }

    #[test]
    fn duplicate_key_is_an_error() {
        assert!(parse("a = 1\na = 2\n").is_err());
    }

    #[test]
    fn multiline_arrays_with_trailing_comma() {
        let t = parse("v = [\n 1,\n 2,\n 3,\n]\n").expect("parses");
        assert_eq!(t["v"].as_array().expect("array").len(), 3);
    }

    #[test]
    fn unsupported_constructs_fail_loudly() {
        // Hex integers are outside the subset and must not parse as 0.
        assert!(parse("a = 0xff\n").is_err());
    }

    #[test]
    fn non_ascii_survives() {
        let t = parse("a = \"café\"\n").expect("parses");
        assert_eq!(t["a"], Value::String("café".into()));
    }
}
