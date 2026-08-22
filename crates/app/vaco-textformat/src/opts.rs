//! Run-wide formatting switches and the `-of name=k=v:k=v` grammar.

use vaco_core::{Error, Result};

use crate::escape::{DEFAULT_REPLACEMENT, StringValidation};
use crate::num::Pretty;

/// `-show_optional_fields`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OptionalFields {
    /// `auto` / `-1`. `json` and `xml` omit unavailable optional fields; every
    /// other writer prints `N/A`. The default.
    #[default]
    Auto,
    /// `always` / `1`. Every writer prints the field, `json` and `xml` as the
    /// literal string `"N/A"`.
    Always,
    /// `never` / `0`. Every writer omits it.
    Never,
}

impl OptionalFields {
    /// Parse the `-show_optional_fields` argument.
    ///
    /// # Errors
    /// [`Error::Option`] for anything else.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "auto" | "-1" => Ok(Self::Auto),
            "always" | "1" => Ok(Self::Always),
            "never" | "0" => Ok(Self::Never),
            other => Err(Error::Option {
                name: "show_optional_fields".into(),
                detail: format!("unknown value {other:?}"),
            }),
        }
    }
}

/// Everything outside the writer that changes an output byte.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FormatOpts {
    /// `-unit`, `-prefix`, `-byte_binary_prefix`, `-sexagesimal`.
    pub pretty: Pretty,
    /// `-show_optional_fields`.
    pub show_optional_fields: OptionalFields,
}

impl FormatOpts {
    /// `-pretty`: `-unit -prefix -byte_binary_prefix -sexagesimal`.
    #[must_use]
    pub fn pretty() -> Self {
        Self {
            pretty: Pretty {
                unit: true,
                prefix: true,
                byte_binary_prefix: true,
                sexagesimal: true,
            },
            show_optional_fields: OptionalFields::Auto,
        }
    }
}

/// The options every writer accepts.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CommonOpts {
    /// `string_validation`/`sv`.
    pub validation: StringValidation,
    /// `string_validation_replacement`/`svr`.
    pub replacement: String,
}

impl Default for CommonOpts {
    fn default() -> Self {
        Self {
            validation: StringValidation::Replace,
            replacement: DEFAULT_REPLACEMENT.to_owned(),
        }
    }
}

impl CommonOpts {
    /// Consume the two shared keys; returns `false` when `key` is not one.
    ///
    /// # Errors
    /// [`Error::Option`] when the value does not parse.
    pub fn set(&mut self, key: &str, value: &str) -> Result<bool> {
        match key {
            "string_validation" | "sv" => {
                self.validation = StringValidation::parse(value).ok_or_else(|| Error::Option {
                    name: key.to_owned(),
                    detail: format!("unknown value {value:?}"),
                })?;
                Ok(true)
            }
            "string_validation_replacement" | "svr" => {
                value.clone_into(&mut self.replacement);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// A parsed `-of writer[=key=value[:key=value…]]` argument.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WriterSpec {
    /// The writer name, before the first `=`.
    pub name: String,
    /// The `key=value` pairs, in the order given.
    pub options: Vec<(String, String)>,
}

impl WriterSpec {
    /// Parse `-of` syntax.
    ///
    /// Pairs are separated by `:` and a `\` escapes the next character, so
    /// `-of 'compact=s=\:'` sets the item separator to a colon.
    ///
    /// # Errors
    /// [`Error::Option`] when a pair has no `=`.
    pub fn parse(spec: &str) -> Result<Self> {
        let (name, rest) = match spec.split_once('=') {
            None => (spec, ""),
            Some((n, r)) => (n, r),
        };
        let mut options = Vec::new();
        for pair in split_unescaped(rest, ':') {
            if pair.is_empty() {
                continue;
            }
            let Some((k, v)) = pair.split_once('=') else {
                return Err(Error::Option {
                    name: name.to_owned(),
                    detail: format!("option {pair:?} has no value"),
                });
            };
            options.push((k.to_owned(), v.to_owned()));
        }
        Ok(Self {
            name: name.to_owned(),
            options,
        })
    }
}

/// Split on unescaped `sep`, removing one level of `\` escaping.
fn split_unescaped(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == sep {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if escaped {
        cur.push('\\');
    }
    out.push(cur);
    out
}

/// Parse a boolean writer option. ffprobe accepts `0`/`1` here, and also the
/// spellings `av_opt` would take.
///
/// # Errors
/// [`Error::Option`] for anything else.
pub fn parse_bool(key: &str, value: &str) -> Result<bool> {
    match value {
        "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        other => Err(Error::Option {
            name: key.to_owned(),
            detail: format!("expected a boolean, got {other:?}"),
        }),
    }
}

/// Parse a single-character writer option (`item_sep`, `sep_char`).
///
/// # Errors
/// [`Error::Option`] when the value is not exactly one character.
pub fn parse_char(key: &str, value: &str) -> Result<char> {
    let mut it = value.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(Error::Option {
            name: key.to_owned(),
            detail: format!("expected a single character, got {value:?}"),
        }),
    }
}

/// The error a writer raises for a key it does not know.
#[must_use]
pub fn unknown_option(writer: &str, key: &str) -> Error {
    Error::Option {
        name: writer.to_owned(),
        detail: format!("unknown option {key:?}"),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn plain_name() {
        let s = WriterSpec::parse("json").expect("parse");
        assert_eq!(s.name, "json");
        assert!(s.options.is_empty());
    }

    #[test]
    fn one_option() {
        let s = WriterSpec::parse("ini=h=0").expect("parse");
        assert_eq!(s.name, "ini");
        assert_eq!(s.options, [("h".to_owned(), "0".to_owned())]);
    }

    #[test]
    fn several_options() {
        let s = WriterSpec::parse("xml=sv=replace:svr=?").expect("parse");
        assert_eq!(
            s.options,
            [
                ("sv".to_owned(), "replace".to_owned()),
                ("svr".to_owned(), "?".to_owned())
            ]
        );
    }

    #[test]
    fn escaped_separator_in_a_value() {
        let s = WriterSpec::parse("compact=s=\\:").expect("parse");
        assert_eq!(s.options, [("s".to_owned(), ":".to_owned())]);
    }

    #[test]
    fn empty_value_is_allowed() {
        let s = WriterSpec::parse("xml=svr=").expect("parse");
        assert_eq!(s.options, [("svr".to_owned(), String::new())]);
    }

    #[test]
    fn missing_value_is_an_error() {
        assert!(WriterSpec::parse("compact=nokey").is_err());
    }

    #[test]
    fn optional_fields_spellings() {
        for (text, want) in [
            ("auto", OptionalFields::Auto),
            ("-1", OptionalFields::Auto),
            ("always", OptionalFields::Always),
            ("1", OptionalFields::Always),
            ("never", OptionalFields::Never),
            ("0", OptionalFields::Never),
        ] {
            assert!(
                matches!(OptionalFields::parse(text), Ok(v) if v == want),
                "{text}"
            );
        }
        assert!(OptionalFields::parse("maybe").is_err());
    }
}
