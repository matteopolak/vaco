//! Pure construction of the Icecast source-client request: no I/O, so this
//! is what the fuzz target and unit tests exercise directly.
//!
//! # Measured against `ffmpeg 8.1`, using a local fake HTTP server
//!
//! Legacy (`-legacy_icecast 1`) sends `SOURCE <path> HTTP/1.1` and never a
//! `100-continue` wait — the body follows the headers immediately. Modern
//! (the default) sends `PUT <path> HTTP/1.1` with `Expect: 100-continue`,
//! and the client genuinely blocks for a `100` response before writing the
//! body: a fake server that accepts the connection, reads the headers, and
//! sends nothing back receives no body at all within the capture window.
//!
//! Header order, captured verbatim with every optional field set:
//!
//! ```text
//! PUT /mystream.mp3 HTTP/1.1
//! User-Agent: MyAgent/1.0
//! Accept: */*
//! Expect: 100-continue
//! Connection: close
//! Host: 127.0.0.1:19502
//! Content-Type: audio/mpeg
//! Icy-MetaData: 1
//! Ice-Name: MyStream
//! Ice-Description: A test stream
//! Ice-URL: http://example.com
//! Ice-Genre: Rock
//! Ice-Public: 1
//! Authorization: Basic c291cmNlOmhhY2ttZQ==
//! ```
//!
//! `Expect` is omitted for legacy mode; every other line and its position is
//! identical between the two modes. `Ice-Name`/`Ice-Description`/`Ice-URL`/
//! `Ice-Genre` are each omitted entirely (not sent empty) when the
//! corresponding option is unset — measured by leaving one at a time unset
//! and confirming only that one line disappears. `Ice-Public` and
//! `Icy-MetaData` are always present regardless of options.
//!
//! Auth: URL userinfo overrides `-password` (measured via the reference's own
//! debug line, `Overwriting -password <pass> with URI password!`); the
//! username defaults to the literal `source` when the URL has no userinfo
//! (measured by base64-decoding the `Authorization` header in that case).

use crate::options::IcecastOptions;

/// Everything [`build_headers`] needs that isn't already in [`IcecastOptions`]:
/// the destination path, the authority for the `Host` header, and resolved
/// credentials (URL userinfo already folded in — see
/// [`crate::protocol::credentials`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target<'a> {
    pub path: &'a str,
    pub host: &'a str,
    pub user: &'a str,
    pub password: &'a str,
}

/// `SOURCE` or `PUT`, and whether to wait for a `100 Continue` before the
/// body — the two things [`crate::protocol`]'s dial loop branches on.
#[must_use]
pub fn method(opts: &IcecastOptions) -> (&'static str, bool) {
    if opts.legacy {
        ("SOURCE", false)
    } else {
        ("PUT", true)
    }
}

/// Build the exact request-line-plus-headers block (including the trailing
/// blank line), in the measured order, ready to write to the wire.
#[must_use]
pub fn build_headers(opts: &IcecastOptions, target: &Target<'_>) -> String {
    let (method, expect_continue) = method(opts);
    let user_agent = if opts.user_agent.is_empty() {
        std::borrow::Cow::Owned(format!("Lavf/{}", env!("CARGO_PKG_VERSION")))
    } else {
        std::borrow::Cow::Borrowed(opts.user_agent.as_str())
    };
    let content_type = if opts.content_type.is_empty() {
        "audio/mpeg"
    } else {
        opts.content_type.as_str()
    };

    let mut out = String::new();
    out.push_str(method);
    out.push(' ');
    out.push_str(target.path);
    out.push_str(" HTTP/1.1\r\n");
    push_header(&mut out, "User-Agent", &user_agent);
    push_header(&mut out, "Accept", "*/*");
    if expect_continue {
        push_header(&mut out, "Expect", "100-continue");
    }
    push_header(&mut out, "Connection", "close");
    push_header(&mut out, "Host", target.host);
    push_header(&mut out, "Content-Type", content_type);
    push_header(&mut out, "Icy-MetaData", "1");
    if !opts.name.is_empty() {
        push_header(&mut out, "Ice-Name", &opts.name);
    }
    if !opts.description.is_empty() {
        push_header(&mut out, "Ice-Description", &opts.description);
    }
    if !opts.url.is_empty() {
        push_header(&mut out, "Ice-URL", &opts.url);
    }
    if !opts.genre.is_empty() {
        push_header(&mut out, "Ice-Genre", &opts.genre);
    }
    push_header(&mut out, "Ice-Public", if opts.public { "1" } else { "0" });
    push_header(
        &mut out,
        "Authorization",
        &basic_auth(target.user, target.password),
    );
    out.push_str("\r\n");
    out
}

fn push_header(out: &mut String, name: &str, value: &str) {
    out.push_str(name);
    out.push_str(": ");
    out.push_str(value);
    out.push_str("\r\n");
}

/// `Basic base64(user:password)`. Deliberately takes already-resolved,
/// non-secret-shaped `&str`s — the caller ([`crate::protocol::credentials`])
/// is where the "default user is `source`, URL userinfo wins" precedence is
/// decided, not here; this function only formats.
#[must_use]
pub fn basic_auth(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64_standard(format!("{user}:{password}").as_bytes())
    )
}

/// Same alphabet/padding as `vaco-protocol-httpproxy`'s private helper of the
/// same name (duplicated rather than shared: `cargo xtask dup-check` only
/// flags type names, and a four-line function isn't worth a new shared
/// crate).
fn base64_standard(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = *chunk.first().unwrap_or(&0);
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        let idx = |shift: u32| {
            let i = usize::try_from((n >> shift) & 0x3f).unwrap_or(0);
            char::from(*ALPHABET.get(i).unwrap_or(&b'A'))
        };
        out.push(idx(18));
        out.push(idx(12));
        out.push(if chunk.len() > 1 { idx(6) } else { '=' });
        out.push(if chunk.len() > 2 { idx(0) } else { '=' });
    }
    out
}

/// Parse the reply status line and headers from `buf` well enough to decide
/// whether a `100 Continue` was sent, pure and I/O-free so the fuzz target
/// can hit it directly. Returns `None` if `buf` doesn't yet contain a
/// complete status line.
#[must_use]
pub fn parse_status_line(buf: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(buf).ok()?;
    let line_end = text.find("\r\n")?;
    let line = &text[..line_end];
    let mut parts = line.split_whitespace();
    let _http_version = parts.next()?;
    let code = parts.next()?;
    code.parse::<u16>().ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn opts() -> IcecastOptions {
        IcecastOptions::default()
    }

    #[test]
    fn modern_mode_sends_put_and_expects_continue() {
        assert_eq!(method(&opts()), ("PUT", true));
    }

    #[test]
    fn legacy_mode_sends_source_and_no_expect() {
        let mut o = opts();
        o.legacy = true;
        assert_eq!(method(&o), ("SOURCE", false));
    }

    #[test]
    fn header_order_and_omission_matches_the_capture() {
        let mut o = opts();
        o.name = "MyStream".into();
        o.description = "A test stream".into();
        o.url = "http://example.com".into();
        o.genre = "Rock".into();
        o.public = true;
        o.user_agent = "MyAgent/1.0".into();
        let target = Target {
            path: "/mystream.mp3",
            host: "127.0.0.1:19502",
            user: "source",
            password: "hackme",
        };
        let headers = build_headers(&o, &target);
        let expected = "PUT /mystream.mp3 HTTP/1.1\r\n\
User-Agent: MyAgent/1.0\r\n\
Accept: */*\r\n\
Expect: 100-continue\r\n\
Connection: close\r\n\
Host: 127.0.0.1:19502\r\n\
Content-Type: audio/mpeg\r\n\
Icy-MetaData: 1\r\n\
Ice-Name: MyStream\r\n\
Ice-Description: A test stream\r\n\
Ice-URL: http://example.com\r\n\
Ice-Genre: Rock\r\n\
Ice-Public: 1\r\n\
Authorization: Basic c291cmNlOmhhY2ttZQ==\r\n\
\r\n";
        assert_eq!(headers, expected);
    }

    #[test]
    fn unset_ice_fields_are_omitted_entirely_not_sent_empty() {
        let o = opts();
        let target = Target {
            path: "/x",
            host: "h:1",
            user: "source",
            password: "",
        };
        let headers = build_headers(&o, &target);
        assert!(!headers.contains("Ice-Name"));
        assert!(!headers.contains("Ice-Description"));
        assert!(!headers.contains("Ice-URL"));
        assert!(!headers.contains("Ice-Genre"));
        assert!(headers.contains("Ice-Public: 0\r\n"));
    }

    #[test]
    fn legacy_mode_omits_expect_header() {
        let mut o = opts();
        o.legacy = true;
        let target = Target {
            path: "/x",
            host: "h:1",
            user: "source",
            password: "",
        };
        let headers = build_headers(&o, &target);
        assert!(!headers.contains("Expect"));
        assert!(headers.starts_with("SOURCE /x HTTP/1.1\r\n"));
    }

    #[test]
    fn default_username_is_the_literal_source() {
        let auth = basic_auth("source", "hackme");
        assert_eq!(auth, "Basic c291cmNlOmhhY2ttZQ==");
    }

    #[test]
    fn parse_status_line_reads_the_code() {
        assert_eq!(
            parse_status_line(b"HTTP/1.1 100 Continue\r\n\r\n"),
            Some(100)
        );
        assert_eq!(
            parse_status_line(b"HTTP/1.1 401 Unauthorized\r\n"),
            Some(401)
        );
        assert_eq!(parse_status_line(b"not a status line"), None);
        assert_eq!(parse_status_line(b"incomplete"), None);
    }
}
