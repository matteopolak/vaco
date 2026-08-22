//! Default header assembly.
//!
//! Portable: builds an ordered `(name, value)` list from [`HttpOptions`] and a
//! byte range, with no knowledge of `ureq` or sockets. A `fetch`-based sibling
//! would build the exact same list and hand it to `Headers::append` instead of
//! `http::request::Builder::header`.
//!
//! # Where the defaults came from
//!
//! Measured against the reference (`ffprobe -v debug http://127.0.0.1:PORT/x`
//! with a local `http.server` whose access log is readable; see
//! `docs/io/vaco-protocol-http.md`):
//!
//! ```text
//! GET /x HTTP/1.1
//! User-Agent: Lavf/62.12.100
//! Accept: */*
//! Range: bytes=0-
//! Connection: close
//! Host: 127.0.0.1:8123
//! Icy-MetaData: 1
//! ```
//!
//! `Host` is not built here — every HTTP/1.1 client, `ureq` included, derives
//! it from the request URI's authority, and setting it a second time here
//! would either duplicate it or race a caller who explicitly overrides it via
//! `-headers`. Header *order* is not treated as a contract (HTTP headers are
//! semantically unordered); the reference's own order is documented above only
//! to explain where the *set* of default headers came from.

use crate::options::HttpOptions;
use crate::parse::{cookie_header, parse_header_block};

/// The byte range for the request being built, half-open on the wire (`end`
/// is one past the last byte wanted) — the same convention `-end_offset`
/// uses, converted to the wire's inclusive form only inside
/// [`range_header_value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestRange {
    pub start: u64,
    /// `None` means "to the end".
    pub end_exclusive: Option<u64>,
}

/// The default `User-Agent`, when `-user_agent` is not set.
#[must_use]
pub fn default_user_agent() -> String {
    format!("Vaco/{}", env!("CARGO_PKG_VERSION"))
}

fn range_header_value(range: RequestRange) -> String {
    match range.end_exclusive {
        // `end_exclusive` is one past the last byte; the wire form is
        // inclusive, hence `- 1`. `end_exclusive` is only ever constructed
        // from `HttpOptions::end_offset`, which cannot be 0 with a positive
        // `start` in the caller's own construction (see `crate::source`), so
        // the subtraction cannot wrap here — but `saturating_sub` costs
        // nothing and removes the need to prove it at every call site.
        Some(end) => format!("bytes={}-{}", range.start, end.saturating_sub(1)),
        None => format!("bytes={}-", range.start),
    }
}

/// Build the header list for one request.
///
/// `range` is `None` when `-seekable 0` forces a plain forward read (no
/// `Range` header at all, matching the reference's own `-seekable 0`
/// behaviour of never sending one).
///
/// `credentials` is `(username, password)`, taken from the target URL's own
/// `user:pass@` userinfo (never from a `Location` header — see
/// `crate::protocol`). Sent whenever present, regardless of `-auth_type`: the
/// reference's "none" is autodetect, not "never send it", and a URL that
/// bothered to carry credentials wants them used.
#[must_use]
pub fn build(
    opts: &HttpOptions,
    range: Option<RequestRange>,
    credentials: Option<(&str, &str)>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();

    let ua = if opts.user_agent.is_empty() {
        default_user_agent()
    } else {
        opts.user_agent.clone()
    };
    out.push(("User-Agent".to_owned(), ua));
    out.push(("Accept".to_owned(), "*/*".to_owned()));

    if let Some(r) = range {
        out.push(("Range".to_owned(), range_header_value(r)));
    }

    out.push((
        "Connection".to_owned(),
        if opts.multiple_requests {
            "keep-alive".to_owned()
        } else {
            "close".to_owned()
        },
    ));

    if !opts.referer.is_empty() {
        out.push(("Referer".to_owned(), opts.referer.clone()));
    }

    if let Some(cookie) = cookie_header(&opts.cookies) {
        out.push(("Cookie".to_owned(), cookie));
    }

    if let Some((user, pass)) = credentials {
        out.push(("Authorization".to_owned(), basic_auth_value(user, pass)));
    }

    if opts.icy {
        out.push(("Icy-MetaData".to_owned(), "1".to_owned()));
    }

    for (name, value) in parse_header_block(&opts.headers) {
        if let Some(slot) = out.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&name)) {
            slot.1 = value;
        } else {
            out.push((name, value));
        }
    }

    out
}

/// `Authorization: Basic <base64(user:pass)>`.
#[must_use]
fn basic_auth_value(user: &str, pass: &str) -> String {
    format!(
        "Basic {}",
        base64_standard(format!("{user}:{pass}").as_bytes())
    )
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// RFC 4648 §4 standard base64, with padding.
///
/// Not a dependency-worthy amount of code (one alphabet table, one 3-bytes-in
/// / 4-chars-out loop) for the one thing this crate needs it for —
/// `Authorization: Basic`. Operates only on caller-supplied credentials
/// (never on server-controlled bytes), and is `O(input.len())` with no
/// allocation beyond the fixed 4/3 growth of the output `String`.
#[must_use]
fn base64_standard(input: &[u8]) -> String {
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        let c0 = b0 >> 2;
        let c1 = ((b0 & 0b0000_0011) << 4) | (b1 >> 4);
        let c2 = ((b1 & 0b0000_1111) << 2) | (b2 >> 6);
        let c3 = b2 & 0b0011_1111;

        let alphabet = |i: u8| {
            char::from(
                BASE64_ALPHABET
                    .get(usize::from(i & 0x3f))
                    .copied()
                    .unwrap_or(b'A'),
            )
        };
        out.push(alphabet(c0));
        out.push(alphabet(c1));
        out.push(if chunk.len() > 1 { alphabet(c2) } else { '=' });
        out.push(if chunk.len() > 2 { alphabet(c3) } else { '=' });
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    fn header<'a>(list: &'a [(String, String)], name: &str) -> Option<&'a str> {
        list.iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn defaults_match_the_measured_reference_set() {
        let opts = HttpOptions::default();
        let headers = build(
            &opts,
            Some(RequestRange {
                start: 0,
                end_exclusive: None,
            }),
            None,
        );
        assert!(header(&headers, "User-Agent").unwrap().starts_with("Vaco/"));
        assert_eq!(header(&headers, "Accept"), Some("*/*"));
        assert_eq!(header(&headers, "Range"), Some("bytes=0-"));
        assert_eq!(header(&headers, "Connection"), Some("close"));
        assert_eq!(header(&headers, "Icy-MetaData"), Some("1"));
        assert_eq!(header(&headers, "Referer"), None);
        assert_eq!(header(&headers, "Cookie"), None);
    }

    #[test]
    fn seekable_false_sends_no_range_header() {
        let opts = HttpOptions::default();
        let headers = build(&opts, None, None);
        assert_eq!(header(&headers, "Range"), None);
    }

    #[test]
    fn offset_and_end_offset_become_an_inclusive_wire_range() {
        let r = RequestRange {
            start: 100,
            end_exclusive: Some(200),
        };
        assert_eq!(range_header_value(r), "bytes=100-199");
    }

    #[test]
    fn multiple_requests_flips_the_connection_header() {
        let opts = HttpOptions {
            multiple_requests: true,
            ..HttpOptions::default()
        };
        let headers = build(&opts, None, None);
        assert_eq!(header(&headers, "Connection"), Some("keep-alive"));
    }

    #[test]
    fn custom_headers_override_a_default_by_name_case_insensitively() {
        let opts = HttpOptions {
            headers: "connection: KEEP-ALIVE\r\nX-Extra: 1".to_owned(),
            ..HttpOptions::default()
        };
        let headers = build(&opts, None, None);
        assert_eq!(header(&headers, "Connection"), Some("KEEP-ALIVE"));
        assert_eq!(header(&headers, "X-Extra"), Some("1"));
        // Overriding one default did not duplicate it.
        assert_eq!(
            headers
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case("connection"))
                .count(),
            1
        );
    }

    #[test]
    fn icy_false_omits_the_header() {
        let opts = HttpOptions {
            icy: false,
            ..HttpOptions::default()
        };
        let headers = build(&opts, None, None);
        assert_eq!(header(&headers, "Icy-MetaData"), None);
    }

    #[test]
    fn credentials_produce_an_authorization_header() {
        let opts = HttpOptions::default();
        let headers = build(&opts, None, Some(("alice", "s3cret")));
        assert_eq!(
            header(&headers, "Authorization"),
            Some(basic_auth_value("alice", "s3cret").as_str())
        );
    }

    #[test]
    fn no_credentials_means_no_authorization_header() {
        let opts = HttpOptions::default();
        let headers = build(&opts, None, None);
        assert_eq!(header(&headers, "Authorization"), None);
    }

    #[test]
    fn base64_matches_the_rfc_4648_worked_example() {
        // RFC 4648's own test vectors.
        assert_eq!(base64_standard(b""), "");
        assert_eq!(base64_standard(b"f"), "Zg==");
        assert_eq!(base64_standard(b"fo"), "Zm8=");
        assert_eq!(base64_standard(b"foo"), "Zm9v");
        assert_eq!(base64_standard(b"foob"), "Zm9vYg==");
        assert_eq!(base64_standard(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_standard(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn basic_auth_matches_the_textbook_example() {
        // The example from RFC 7617 §2.
        assert_eq!(
            basic_auth_value("Aladdin", "open sesame"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
    }
}
