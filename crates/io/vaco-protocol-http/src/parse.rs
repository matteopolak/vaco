//! Parsing of bytes a remote server controls.
//!
//! Nothing in this module touches a socket. That is the point: every function
//! here takes a `&str` (already lifted out of an HTTP header by `ureq`/the
//! `http` crate, which itself refuses non-visible-ASCII header values) and
//! returns a parsed value or `None`/an empty collection — never a panic, never
//! unbounded work, never an allocation sized directly from an attacker-chosen
//! number. This is the surface `fuzz/fuzz_targets/protocol_http_response.rs`
//! drives.
//!
//! Portable: no I/O, no `ureq` types, nothing OS-specific. A `vaco-protocol-fetch`
//! built on the browser `fetch` API would parse exactly the same header values
//! with exactly these functions.

/// A parsed `Content-Range: bytes <start>-<end>/<total>` response header.
///
/// `end` is **inclusive**, matching the wire format — the last byte position
/// actually present, not one past it. Contrast [`crate::options::HttpOptions::end_offset`],
/// which is exclusive on the *request* side; the asymmetry is the wire
/// format's, not ours, and converting between them is exactly where an
/// off-by-one hides, so the two are never silently substituted for each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: Option<u64>,
}

/// Parse a `Content-Range` value.
///
/// Accepts `bytes <start>-<end>/<total>` and `bytes <start>-<end>/*`. Rejects
/// (returns `None`) the unsatisfiable-range form `bytes */<total>`, which
/// carries no start/end and therefore tells a reader nothing about where the
/// bytes it is about to receive begin — a 416 response uses this form and a
/// reader should look at the status code, not this parser, for that case.
///
/// Total is bounded implicitly: every number here is parsed with
/// `str::parse::<u64>`, which rejects a value with more digits than fit rather
/// than looping or allocating per digit.
#[must_use]
pub fn parse_content_range(v: &str) -> Option<ContentRange> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end: u64 = end.trim().parse().ok()?;
    if end < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        digits => Some(digits.parse::<u64>().ok()?),
    };
    Some(ContentRange { start, end, total })
}

/// Parse a `Retry-After` value in the delay-seconds form (`Retry-After: 120`).
///
/// The HTTP-date form (`Retry-After: Wed, 21 Oct 2026 07:28:00 GMT`) is not
/// parsed — it would need a calendar and a clock (`vaco-time` deliberately
/// carries neither on a target with no wall clock, D18), and a server that
/// wants us to wait a specific number of seconds can just say so. A caller
/// that gets `None` back falls back to its own backoff schedule, which is
/// always safe: this header is an optimisation, never the sole termination
/// condition.
#[must_use]
pub fn parse_retry_after_secs(v: &str) -> Option<u64> {
    v.trim().parse::<u64>().ok()
}

/// Parse `-reconnect_on_http_error`'s comma-separated status code list.
///
/// Silently drops an entry that is not a bare 3-digit-shaped number, rather
/// than rejecting the whole list — this is a convenience list the *user*
/// wrote, not server-controlled input, so leniency costs nothing a caller
/// would not have typed by mistake anyway. Never allocates more than one
/// `u16` per comma-separated field.
#[must_use]
pub fn parse_reconnect_codes(list: &str) -> Vec<u16> {
    list.split(',')
        .filter_map(|s| s.trim().parse::<u16>().ok())
        .collect()
}

/// Build the `Cookie:` header value from `-cookies`'s
/// `Set-Cookie`-field-syntax lines.
///
/// Each non-empty line contributes the `name=value` pair before its first
/// `;` (the attributes — `path=`, `Secure`, `HttpOnly`, an expiry — describe
/// how a *browser* would store the cookie; we are not storing anything, only
/// relaying the pair on this and future requests to the same URL, so they are
/// read and discarded). Lines that do not contain `=` are skipped. This is
/// the caller's own configuration, not server-controlled, so it is not part
/// of the fuzz target — but it still never panics on malformed input.
#[must_use]
pub fn cookie_header(cookies_option: &str) -> Option<String> {
    let mut pairs: Vec<&str> = Vec::new();
    for raw_line in cookies_option.split(['\n', '\r']) {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let pair = line.split(';').next().unwrap_or("").trim();
        if pair.contains('=') {
            pairs.push(pair);
        }
    }
    if pairs.is_empty() {
        return None;
    }
    Some(pairs.join("; "))
}

/// Parse `-headers`'s raw `Key: Value\r\n`-separated block into pairs.
///
/// Accepts both `\r\n` and bare `\n` as the line separator (the reference
/// accepts a plain shell `$'...\n...'` in practice, not only literal `\r\n`),
/// trims surrounding whitespace from both the name and the value, and skips a
/// line with no `:` rather than erroring — a trailing blank line from a
/// shell heredoc is common and must not fail the whole option.
#[must_use]
pub fn parse_header_block(block: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw_line in block.split(['\n', '\r']) {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if !name.is_empty() {
                out.push((name.to_owned(), value.to_owned()));
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn content_range_parses_the_normal_form() {
        let cr = parse_content_range("bytes 1540439-1562813/1562814").unwrap();
        assert_eq!(cr.start, 1_540_439);
        assert_eq!(cr.end, 1_562_813);
        assert_eq!(cr.total, Some(1_562_814));
    }

    #[test]
    fn content_range_accepts_unknown_total() {
        let cr = parse_content_range("bytes 0-99/*").unwrap();
        assert_eq!(cr.total, None);
    }

    #[test]
    fn content_range_rejects_the_unsatisfiable_form() {
        assert!(parse_content_range("bytes */1562814").is_none());
    }

    #[test]
    fn content_range_rejects_garbage() {
        for s in ["", "bytes", "bytes -1-2/3", "bytes 5-2/9", "chickens 0-1/2"] {
            assert!(parse_content_range(s).is_none(), "{s:?}");
        }
    }

    #[test]
    fn retry_after_parses_seconds_only() {
        assert_eq!(parse_retry_after_secs("120"), Some(120));
        assert_eq!(parse_retry_after_secs(" 5 "), Some(5));
        assert_eq!(
            parse_retry_after_secs("Wed, 21 Oct 2026 07:28:00 GMT"),
            None
        );
        assert_eq!(parse_retry_after_secs(""), None);
    }

    #[test]
    fn reconnect_codes_skips_junk() {
        assert_eq!(
            parse_reconnect_codes("503, 504,,abc,599"),
            vec![503, 504, 599]
        );
        assert_eq!(parse_reconnect_codes(""), Vec::<u16>::new());
    }

    #[test]
    fn cookie_header_takes_the_pair_before_the_first_semicolon() {
        assert_eq!(
            cookie_header("sessionid=abc123; path=/\nfoo=bar; path=/"),
            Some("sessionid=abc123; foo=bar".to_owned())
        );
        assert_eq!(cookie_header(""), None);
        assert_eq!(cookie_header("garbage; no equals sign"), None);
    }

    #[test]
    fn header_block_overrides_are_parsed_permissively() {
        assert_eq!(
            parse_header_block("X-Custom: hi\r\nUser-Agent: X\n\n"),
            vec![
                ("X-Custom".to_owned(), "hi".to_owned()),
                ("User-Agent".to_owned(), "X".to_owned()),
            ]
        );
    }

    proptest::proptest! {
        /// Every parser in this module takes bytes a *server* controls, so
        /// "never panics on arbitrary input" is the property that matters
        /// most — this is the fuzz target's own claim, restated as a
        /// property test that runs on every `cargo test`.
        #[test]
        fn content_range_never_panics(s in ".*") {
            let _ = parse_content_range(&s);
        }

        #[test]
        fn retry_after_never_panics(s in ".*") {
            let _ = parse_retry_after_secs(&s);
        }

        #[test]
        fn reconnect_codes_never_panics(s in ".*") {
            let _ = parse_reconnect_codes(&s);
        }

        #[test]
        fn cookie_header_never_panics(s in ".*") {
            let _ = cookie_header(&s);
        }

        #[test]
        fn header_block_never_panics(s in ".*") {
            let _ = parse_header_block(&s);
        }

        /// A `Content-Range` this crate itself would have built (from
        /// [`crate::headers::range_header_value`]'s inverse — a valid
        /// `start-end/total` triple) always round-trips.
        #[test]
        fn content_range_round_trips_for_well_formed_input(start in 0u64..1_000_000, len in 1u64..1_000_000) {
            let end = start + len - 1;
            let total = end + 1 + 100;
            let v = format!("bytes {start}-{end}/{total}");
            let cr = parse_content_range(&v);
            proptest::prop_assert_eq!(cr, Some(ContentRange { start, end, total: Some(total) }));
        }
    }
}
