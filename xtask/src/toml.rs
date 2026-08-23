//! The array-of-tables subset of TOML that this repository's own metadata uses.
//!
//! Two schemas are read with it: `vaco-component.toml` fragments (plan 19 §3.4)
//! and `provenance/*.toml` (plan 13 §6). Both are hand-written, both are small,
//! and both are read by a binary that must compile before anything else — so
//! xtask stays dependency-free and reads the subset it actually needs rather
//! than taking a general TOML crate.
//!
//! The subset: `# comments`, `[[table]]` headers, and `key = value` where value
//! is a double-quoted basic string or a bare `true`/`false`. Anything outside
//! that is an error naming the line, because a metadata file that parses
//! *approximately* is worse than one that does not parse at all.
//!
//! Plan 13 §6.4 wrote `provenance/*.yaml`. It is TOML here, deliberately: the
//! parser for this subset already existed for the component fragments, and one
//! metadata dialect in the repository beats two (D19). The evidence the section
//! asks for is unchanged; only the punctuation is.

use crate::Map;

/// One array-of-tables entry: its keys, and the line its header was on.
#[derive(Debug, Default)]
pub struct Table {
    map: Map<String, String>,
    pub name: String,
    pub origin_line: usize,
}

impl Table {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.map.keys()
    }

    /// A required key, or a message naming the file's line.
    ///
    /// # Errors
    /// When the key is absent.
    pub fn need(&self, key: &str) -> Result<&str, String> {
        self.get(key)
            .ok_or_else(|| format!("line {}: `{key}` is required", self.origin_line))
    }
}

/// Parse every array-of-tables entry in `text` whose header is in `allowed`.
///
/// # Errors
/// A message naming the line, for anything outside the subset's grammar or any
/// header not in `allowed`.
pub fn tables(text: &str, allowed: &[&str]) -> Result<Vec<Table>, String> {
    let mut tables: Vec<Table> = Vec::new();

    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let s = strip_comment(raw).trim();
        if s.is_empty() {
            continue;
        }

        if let Some(rest) = s.strip_prefix("[[") {
            let name = rest
                .strip_suffix("]]")
                .ok_or_else(|| format!("line {line}: unterminated `[[` header"))?
                .trim();
            if !allowed.contains(&name) {
                return Err(format!(
                    "line {line}: `[[{name}]]` — this schema defines {}",
                    list(allowed)
                ));
            }
            tables.push(Table {
                map: Map::new(),
                name: name.to_owned(),
                origin_line: line,
            });
            continue;
        }
        if s.starts_with('[') {
            return Err(format!(
                "line {line}: `{s}` — this schema holds only {}",
                list(allowed)
            ));
        }

        let (key, value) = s
            .split_once('=')
            .ok_or_else(|| format!("line {line}: `{s}` is not `key = value`"))?;
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("line {line}: `{key}` is not a bare key"));
        }
        let value = scalar(value.trim(), line)?;

        let Some(t) = tables.last_mut() else {
            return Err(format!(
                "line {line}: `{key}` appears before any {} header",
                list(allowed)
            ));
        };
        if t.map.insert(key.to_owned(), value).is_some() {
            return Err(format!("line {line}: `{key}` is set twice"));
        }
    }
    Ok(tables)
}

/// `[[a]]`, or `[[a]] and [[b]]`, or `[[a]], [[b]] and [[c]]`.
fn list(names: &[&str]) -> String {
    let quoted: Vec<String> = names.iter().map(|n| format!("`[[{n}]]`")).collect();
    match quoted.split_last() {
        None => "no tables at all".to_owned(),
        Some((last, [])) => last.clone(),
        Some((last, head)) => format!("{} and {last}", head.join(", ")),
    }
}

/// Remove a trailing `#` comment, respecting a quoted `#`.
fn strip_comment(s: &str) -> &str {
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
        } else if in_str && c == '\\' {
            escaped = true;
        } else if c == '"' {
            in_str = !in_str;
        } else if c == '#' && !in_str {
            return s.get(..i).unwrap_or(s);
        }
    }
    s
}

/// A basic string, or a bare `true`/`false`.
fn scalar(s: &str, line: usize) -> Result<String, String> {
    if s == "true" || s == "false" {
        return Ok(s.to_owned());
    }
    let inner = s
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .ok_or_else(|| {
            format!("line {line}: {s:?} — values are double-quoted strings, or `true`/`false`")
        })?;

    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            if c == '"' {
                return Err(format!("line {line}: unescaped `\"` inside a string"));
            }
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => return Err(format!("line {line}: unsupported escape `\\{other}`")),
            None => return Err(format!("line {line}: string ends in a backslash")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_two_table_kinds_in_one_file() {
        let t = tables(
            "[[source]]\nid = \"a\"\n\n[[table]]\nname = \"T\"\nsource = \"a\"\n",
            &["source", "table"],
        )
        .expect("parse");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].name, "source");
        assert_eq!(t[1].get("source"), Some("a"));
    }

    #[test]
    fn rejects_a_header_the_schema_does_not_define() {
        let e = tables("[[nope]]\n", &["source"]).expect_err("must reject");
        assert!(e.contains("`[[source]]`"), "{e}");
    }

    #[test]
    fn a_quoted_hash_is_not_a_comment() {
        let t = tables("[[s]]\nid = \"a#b\"\n", &["s"]).expect("parse");
        assert_eq!(t[0].get("id"), Some("a#b"));
    }
}
