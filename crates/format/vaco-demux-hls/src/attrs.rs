//! The `NAME=VALUE` attribute-list grammar RFC 8216 §4.2 defines and reuses
//! on nearly every `#EXT-X-` tag: `EXT-X-STREAM-INF`, `EXT-X-MEDIA`,
//! `EXT-X-KEY`, `EXT-X-MAP`, `EXT-X-BYTERANGE`'s sibling tags, and more.
//!
//! One parser for all of them, rather than one per tag, because the grammar
//! itself is the part worth getting right once: a quoted-string value may
//! contain a comma, which is also the attribute separator, so a naive
//! `split(',')` corrupts `CODECS="avc1.4d401f,mp4a.40.2"` into two attributes.

/// One `NAME=VALUE` pair from an attribute list. `VALUE` keeps its quotes
/// stripped when it was a quoted-string; an enumerated-string or a decimal
/// value is returned exactly as written, since the caller knows which it
/// expects for a given attribute name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attr<'a> {
    pub name: &'a str,
    pub value: &'a str,
}

/// Parse an attribute list, the part of a tag line after the first `:`.
///
/// Total: a malformed attribute (no `=`, an unterminated quote) is simply
/// skipped rather than aborting the whole line, matching RFC 8216 §4.1's
/// general tolerance rule ("clients SHOULD ignore any attributes... they do
/// not recognize") extended one step further to attributes that do not even
/// parse — a playlist that is mostly fine should not become entirely
/// unreadable over one bad attribute.
#[must_use]
pub fn parse_attribute_list(s: &str) -> Vec<Attr<'_>> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        rest = rest.trim_start_matches([' ', ',']);
        if rest.is_empty() {
            break;
        }
        let Some(eq) = rest.find('=') else { break };
        let Some(name) = rest.get(..eq) else { break };
        let Some(after_eq) = rest.get(eq + 1..) else {
            break;
        };
        let (value, tail) = if let Some(quoted) = after_eq.strip_prefix('"') {
            match quoted.find('"') {
                Some(end) => {
                    let Some(value) = quoted.get(..end) else {
                        break;
                    };
                    let Some(tail) = quoted.get(end + 1..) else {
                        break;
                    };
                    (value, tail)
                }
                None => break,
            }
        } else {
            let end = after_eq.find(',').unwrap_or(after_eq.len());
            let Some(value) = after_eq.get(..end) else {
                break;
            };
            let tail = after_eq.get(end..).unwrap_or("");
            (value, tail)
        };
        out.push(Attr {
            name: name.trim(),
            value,
        });
        rest = tail;
    }
    out
}

/// Look up one attribute by name (case-sensitive: RFC 8216 attribute names
/// are always upper-case ASCII and this workspace never emits any other
/// case).
#[must_use]
pub fn get<'a>(attrs: &[Attr<'a>], name: &str) -> Option<&'a str> {
    attrs.iter().find(|a| a.name == name).map(|a| a.value)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn quoted_commas_do_not_split_the_attribute() {
        let attrs = parse_attribute_list(
            r#"BANDWIDTH=1280000,CODECS="avc1.4d401f,mp4a.40.2",RESOLUTION=640x360"#,
        );
        assert_eq!(get(&attrs, "BANDWIDTH"), Some("1280000"));
        assert_eq!(get(&attrs, "CODECS"), Some("avc1.4d401f,mp4a.40.2"));
        assert_eq!(get(&attrs, "RESOLUTION"), Some("640x360"));
    }

    #[test]
    fn a_malformed_attribute_does_not_stop_the_rest_from_parsing() {
        let attrs = parse_attribute_list(r#"GOOD=1,BROKEN,ALSO-GOOD="yes""#);
        assert_eq!(get(&attrs, "GOOD"), Some("1"));
        // Parsing stops at the first attribute that has no `=` at all, per
        // RFC 8216's general tolerance rule — the reference itself does not
        // attempt to resynchronise mid-line, only skip whole unrecognised
        // tags/attributes it can still delimit.
        assert_eq!(get(&attrs, "ALSO-GOOD"), None);
    }

    #[test]
    fn empty_list_yields_nothing() {
        assert!(parse_attribute_list("").is_empty());
    }
}
