//! Per-writer escaping.
//!
//! Every rule here was derived by pushing one torture string through the
//! reference binary and reading the bytes back with `od -c` — see
//! `tests/torture.rs`, which pins the results. The rules are not
//! self-consistent between writers and are not what a first-principles
//! implementation would choose; do not "fix" them.
//!
//! | | `\t` | `\n` | `\r` | other C0 | `\` | `"` |
//! |---|---|---|---|---|---|---|
//! | `default` | raw | raw | raw | raw | raw | raw |
//! | `compact` `e=c` | raw | `\n` | `\r` | `\b`/`\f` only | `\\` | raw |
//! | `compact` `e=csv` | raw | raw¹ | raw¹ | raw | raw | `""`¹ |
//! | `flat` | raw | `\n` | `\r` | raw | `\\` | `\"` |
//! | `ini` | `\t` | `\n` | `\r` | `\x00NN` | `\\` | raw |
//! | `json` | `\t` | `\n` | `\r` | `\u00NN` | `\\` | `\"` |
//! | `xml` | raw | raw | raw | U+FFFD² | raw | `&quot;` |
//!
//! ¹ inside RFC 4180 quotes, which the value's content triggers.
//! ² `xml` runs [`XmlValidation`] first; the replacement is configurable.

use std::fmt::Write as _;

/// `compact`/`csv` `escape` mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EscapeMode {
    /// Backslash escaping. The `compact` default.
    #[default]
    C,
    /// RFC 4180 quoting. The `csv` default.
    Csv,
    /// Verbatim.
    None,
}

impl EscapeMode {
    /// Parse the `escape`/`e` option value; [`None`] when unrecognised.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "c" => Some(Self::C),
            "csv" => Some(Self::Csv),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// Apply the mode to `v`, given the writer's item separator.
    #[must_use]
    pub fn apply(self, v: &str, sep: char) -> String {
        match self {
            Self::C => escape_c(v, sep),
            Self::Csv => escape_csv(v, sep),
            Self::None => v.to_owned(),
        }
    }
}

/// `compact`'s `escape=c`: backslash-escape the item separator, the backslash
/// itself, and exactly four control characters.
///
/// Tab and vertical tab are **not** escaped, which is the surprising part —
/// a tab reaches the output raw while a carriage return becomes `\r`.
#[must_use]
pub fn escape_c(v: &str, sep: char) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if c == sep => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// `compact`'s `escape=csv`: RFC 4180. Quote the field iff it contains the
/// separator, a double quote, LF or CR; inside the quotes, double the quotes.
#[must_use]
pub fn escape_csv(v: &str, sep: char) -> String {
    if !v.contains([sep, '"', '\n', '\r']) {
        return v.to_owned();
    }
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// `flat`'s value escaping, applied inside the surrounding double quotes.
///
/// Shell-ish: the four characters that would be interpreted inside a
/// double-quoted `sh` word, plus LF and CR. Nothing else — not tab, not the
/// separator character, not `<`, `>`, `&` or anything non-ASCII.
#[must_use]
pub fn escape_flat(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' | '"' | '$' | '`' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// `flat`'s key sanitisation: every character outside `[A-Za-z0-9_]` becomes
/// `_`, one underscore per character (not per run).
///
/// Case is preserved — a Matroska tag arrives already upper-cased by the
/// demuxer, and `WE-IRD_KEY.1` comes out `WE_IRD_KEY_1`.
#[must_use]
pub fn sanitise_flat_key(k: &str) -> String {
    k.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `ini`'s value escaping. Keys are **not** escaped, only values.
///
/// `:` and `#` are escaped but `;` is not, which is surprising for an INI
/// dialect; it is what the binary does.
#[must_use]
pub fn escape_ini(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' | '=' | ':' | '#' => {
                out.push('\\');
                out.push(c);
            }
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                // Four hex digits, lowercase: 0x01 renders `\x0001`.
                let _ = write!(out, "\\x{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// `json`'s string escaping.
///
/// Standard JSON minus the optional parts: `/` is left alone and non-ASCII is
/// emitted as raw UTF-8 rather than `\uXXXX`. DEL (0x7f) is not escaped either.
#[must_use]
pub fn escape_json(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// `xml`'s attribute-value escaping.
///
/// The four predefined entities that matter inside a double-quoted attribute.
/// `'` is left alone (the writer never single-quotes), and so are `\`, tab, LF
/// and CR — which makes the attribute value technically non-normalising, but
/// again, it is what the binary emits.
#[must_use]
pub fn escape_xml(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// Whether `c` may appear in a well-formed XML 1.0 document.
///
/// `Char ::= #x9 | #xA | #xD | [#x20-#xD7FF] | [#xE000-#xFFFD] |
/// [#x10000-#x10FFFF]` (XML 1.0 §2.2). Surrogates cannot occur in a Rust
/// `char`, so only the C0 controls and the two non-characters at the end of the
/// BMP are rejected in practice.
#[must_use]
pub const fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' '..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

/// What `string_validation`/`sv` does with a character the writer rejects.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StringValidation {
    /// Drop the whole field. Observed: the offending `<tag …/>` never appears.
    Fail,
    /// Emit the string unchanged.
    Ignore,
    /// Substitute `string_validation_replacement`. The default.
    #[default]
    Replace,
}

impl StringValidation {
    /// Parse the `string_validation`/`sv` option value.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fail" => Some(Self::Fail),
            "ignore" => Some(Self::Ignore),
            "replace" => Some(Self::Replace),
            _ => None,
        }
    }
}

/// The default `string_validation_replacement`.
///
/// The documented default is the empty string, but the binary substitutes
/// U+FFFD when the option is left alone — observed by feeding every C0 control
/// through the `xml` writer. Passing `svr=` explicitly does delete instead.
pub const DEFAULT_REPLACEMENT: &str = "\u{fffd}";

/// The `xml` writer's string-validation pass, run *before* escaping.
///
/// Returns [`None`] under [`StringValidation::Fail`] when the string contains a
/// character XML cannot represent; the caller then omits the field entirely.
#[must_use]
pub fn validate_xml(v: &str, mode: StringValidation, replacement: &str) -> Option<String> {
    if v.chars().all(is_xml_char) {
        return Some(v.to_owned());
    }
    match mode {
        StringValidation::Fail => None,
        StringValidation::Ignore => Some(v.to_owned()),
        StringValidation::Replace => Some(
            v.chars()
                .map(|c| {
                    if is_xml_char(c) {
                        std::borrow::Cow::Owned(c.to_string())
                    } else {
                        std::borrow::Cow::Borrowed(replacement)
                    }
                })
                .collect(),
        ),
    }
}

/// Invert [`escape_c`]. Used only by tests and by the fuzz target.
///
/// Returns [`None`] for input that [`escape_c`] could not have produced: a
/// trailing lone backslash, or a backslash before a character that is neither
/// an escape letter, the separator, nor a backslash.
#[must_use]
pub fn unescape_c(v: &str, sep: char) -> Option<String> {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            'b' => out.push('\u{8}'),
            'n' => out.push('\n'),
            'f' => out.push('\u{c}'),
            'r' => out.push('\r'),
            '\\' => out.push('\\'),
            c if c == sep => out.push(c),
            _ => return None,
        }
    }
    Some(out)
}

/// Invert [`escape_csv`].
#[must_use]
pub fn unescape_csv(v: &str) -> Option<String> {
    let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return if v.contains('"') {
            None
        } else {
            Some(v.to_owned())
        };
    };
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c == '"' {
            if it.next()? != '"' {
                return None;
            }
            out.push('"');
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Invert [`escape_ini`].
#[must_use]
pub fn unescape_ini(v: &str) -> Option<String> {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            'b' => out.push('\u{8}'),
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'f' => out.push('\u{c}'),
            'r' => out.push('\r'),
            'x' => {
                let hex: String = it.by_ref().take(4).collect();
                let n = u32::from_str_radix(&hex, 16).ok()?;
                out.push(char::from_u32(n)?);
            }
            c @ ('\\' | '=' | ':' | '#') => out.push(c),
            _ => return None,
        }
    }
    Some(out)
}

/// Invert [`escape_json`].
#[must_use]
pub fn unescape_json(v: &str) -> Option<String> {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            'b' => out.push('\u{8}'),
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'f' => out.push('\u{c}'),
            'r' => out.push('\r'),
            'u' => {
                let hex: String = it.by_ref().take(4).collect();
                let n = u32::from_str_radix(&hex, 16).ok()?;
                out.push(char::from_u32(n)?);
            }
            c @ ('"' | '\\') => out.push(c),
            _ => return None,
        }
    }
    Some(out)
}

/// Invert [`escape_flat`].
#[must_use]
pub fn unescape_flat(v: &str) -> Option<String> {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            c @ ('\\' | '"' | '$' | '`') => out.push(c),
            _ => return None,
        }
    }
    Some(out)
}

/// Invert [`escape_xml`]. Only the four entities the writer emits.
#[must_use]
pub fn unescape_xml(v: &str) -> Option<String> {
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    'outer: while let Some(i) = rest.find('&') {
        let (head, tail) = rest.split_at(i);
        out.push_str(head);
        for (ent, ch) in [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
        ] {
            if let Some(r) = tail.strip_prefix(ent) {
                out.push(ch);
                rest = r;
                continue 'outer;
            }
        }
        return None;
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    // The torture string from plan 14 §4.3, which is also what was pushed
    // through the reference binary.
    const NASTY: &str = "v=1,c:2|q\"3\\4;e[f]#g <&> ünï";

    #[test]
    fn observed_compact_c() {
        assert_eq!(escape_c(NASTY, '|'), "v=1,c:2\\|q\"3\\\\4;e[f]#g <&> ünï");
    }

    #[test]
    fn observed_compact_csv() {
        assert_eq!(
            escape_csv(NASTY, '|'),
            "\"v=1,c:2|q\"\"3\\4;e[f]#g <&> ünï\""
        );
    }

    #[test]
    fn observed_flat() {
        assert_eq!(escape_flat(NASTY), "v=1,c:2|q\\\"3\\\\4;e[f]#g <&> ünï");
        assert_eq!(sanitise_flat_key("WE-IRD_KEY.1"), "WE_IRD_KEY_1");
    }

    #[test]
    fn observed_ini() {
        assert_eq!(escape_ini(NASTY), "v\\=1,c\\:2|q\"3\\\\4;e[f]\\#g <&> ünï");
        assert_eq!(escape_ini("\u{1}"), "\\x0001");
        assert_eq!(escape_ini("\u{7f}"), "\u{7f}");
    }

    #[test]
    fn observed_json() {
        assert_eq!(escape_json(NASTY), "v=1,c:2|q\\\"3\\\\4;e[f]#g <&> ünï");
        assert_eq!(escape_json("a/b"), "a/b");
        assert_eq!(escape_json("\u{b}"), "\\u000b");
    }

    #[test]
    fn observed_xml() {
        assert_eq!(
            escape_xml(NASTY),
            "v=1,c:2|q&quot;3\\4;e[f]#g &lt;&amp;&gt; ünï"
        );
    }

    #[test]
    fn xml_validation_replaces_c0_with_replacement_char() {
        let v = "a\u{1}b";
        let out = validate_xml(v, StringValidation::Replace, DEFAULT_REPLACEMENT);
        assert_eq!(out.as_deref(), Some("a\u{fffd}b"));
        assert_eq!(
            validate_xml(v, StringValidation::Ignore, DEFAULT_REPLACEMENT).as_deref(),
            Some("a\u{1}b")
        );
        assert_eq!(
            validate_xml(v, StringValidation::Fail, DEFAULT_REPLACEMENT),
            None
        );
        // Tab, LF and CR survive.
        assert_eq!(
            validate_xml("\t\n\r", StringValidation::Replace, DEFAULT_REPLACEMENT).as_deref(),
            Some("\t\n\r")
        );
    }

    #[test]
    fn separator_dependent_escaping() {
        // Changing the separator moves which character gets a backslash.
        assert_eq!(escape_c("a|b@c", '@'), "a|b\\@c");
        assert_eq!(escape_csv("a|b@c", '@'), "\"a|b@c\"");
        assert_eq!(escape_csv("a|b@c", ','), "a|b@c");
    }
}
