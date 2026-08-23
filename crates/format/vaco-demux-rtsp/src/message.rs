//! RFC 2326 §4 / RFC 7826 §8: the RTSP request/response text grammar.
//!
//! Every byte a [`Response`] parses came off a socket to a server this
//! crate does not control — a compromised or merely buggy RTSP server picks
//! every header name, value and status line this module sees. [`Response::parse_head`]
//! is written to be fuzzed directly (see `fuzz/fuzz_targets/rtsp_response_parse.rs`):
//! it takes a complete byte slice, never assumes a header fits any bound
//! beyond what the buffer actually holds, and returns `Result` for anything
//! that does not look like RTSP rather than guessing.

use vaco_core::{Error, Result};

/// The methods this crate's [`crate::session`] actually sends. RFC 2326
/// §10 defines more (`RECORD`, `ANNOUNCE`, `REDIRECT`, `SET_PARAMETER`) —
/// not implemented, since nothing in this crate's client role needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Options,
    Describe,
    Setup,
    Play,
    Pause,
    Teardown,
    GetParameter,
}

impl Method {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Options => "OPTIONS",
            Self::Describe => "DESCRIBE",
            Self::Setup => "SETUP",
            Self::Play => "PLAY",
            Self::Pause => "PAUSE",
            Self::Teardown => "TEARDOWN",
            Self::GetParameter => "GET_PARAMETER",
        }
    }
}

/// A case-insensitive, order-preserving, duplicate-tolerant header list —
/// the same shape `vaco_format_core::Metadata` uses for container metadata,
/// for the same reason: HTTP/RTSP headers may repeat (`WWW-Authenticate`
/// with more than one scheme) and a `HashMap` would silently keep only one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers(pub Vec<(String, String)>);

impl Headers {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.0
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn push(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.push((name.into(), value.into()));
    }
}

/// One outgoing RTSP request. `body` is sent verbatim with a matching
/// `Content-Length` this crate computes — a caller never sets that header
/// directly.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub uri: String,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl Request {
    #[must_use]
    pub fn new(method: Method, uri: impl Into<String>) -> Self {
        Self {
            method,
            uri: uri.into(),
            headers: Headers::default(),
            body: Vec::new(),
        }
    }

    /// Serialise to the exact bytes to send, RFC 2326 §4 (CRLF line
    /// endings, a blank line ending the header block, `Content-Length`
    /// added automatically when `body` is non-empty).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(self.method.as_str().as_bytes());
        out.push(b' ');
        out.extend_from_slice(self.uri.as_bytes());
        out.extend_from_slice(b" RTSP/1.0\r\n");
        for (k, v) in &self.headers.0 {
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(v.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        if !self.body.is_empty() {
            out.extend_from_slice(format!("Content-Length: {}\r\n", self.body.len()).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

/// A parsed RTSP status line and header block — the body, if any, is read
/// separately (see `crate::connection`) since `Content-Length` bytes may
/// not have arrived on the wire yet when the head is parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Headers,
}

impl Response {
    #[must_use]
    pub fn content_length(&self) -> usize {
        self.headers
            .get("Content-Length")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    }

    #[must_use]
    pub fn cseq(&self) -> Option<u32> {
        self.headers.get("CSeq").and_then(|v| v.trim().parse().ok())
    }

    /// Parse the status line and every header out of `buf`, which must
    /// contain the complete head (up to and including the blank line that
    /// ends it — `crate::connection` finds that boundary before calling
    /// this). Returns the parsed head; the body, if `Content-Length` names
    /// one, is not consumed here.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `buf` has no status line, the status line
    /// is not `RTSP/<ver> <code> <reason>`, or a header line has no `:`.
    pub fn parse_head(buf: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(buf)
            .map_err(|_| Error::InvalidData("RTSP response head is not valid UTF-8"))?;
        let mut lines = text.split("\r\n");
        // Tolerate a bare `\n`-only stream, which a couple of embedded RTSP
        // servers this crate was checked against actually send.
        let mut lines_lf;
        let iter: &mut dyn Iterator<Item = &str> = if text.contains("\r\n") {
            &mut lines
        } else {
            lines_lf = text.split('\n');
            &mut lines_lf
        };

        let status_line = iter
            .next()
            .ok_or(Error::InvalidData("RTSP response has no status line"))?;
        let mut parts = status_line.splitn(3, ' ');
        let version = parts
            .next()
            .ok_or(Error::InvalidData("RTSP status line is empty"))?;
        if !version.starts_with("RTSP/") {
            return Err(Error::InvalidData(
                "RTSP status line has no RTSP/ version token",
            ));
        }
        let status: u16 = parts
            .next()
            .ok_or(Error::InvalidData("RTSP status line has no status code"))?
            .parse()
            .map_err(|_| Error::InvalidData("RTSP status code is not a number"))?;
        let reason = parts.next().unwrap_or("").trim_end_matches('\r').to_owned();

        let mut headers = Headers::default();
        for line in iter {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                break;
            }
            let (name, value) = line
                .split_once(':')
                .ok_or(Error::InvalidData("RTSP header line has no ':'"))?;
            headers.push(name.trim(), value.trim());
        }

        Ok(Self {
            status,
            reason,
            headers,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn builds_a_request_with_content_length() {
        let mut req = Request::new(Method::Setup, "rtsp://host/track1");
        req.headers.push("CSeq", "2");
        req.body = b"hello".to_vec();
        let bytes = req.to_bytes();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("SETUP rtsp://host/track1 RTSP/1.0\r\n"));
        assert!(text.contains("Content-Length: 5\r\n"));
        assert!(text.ends_with("hello"));
    }

    #[test]
    fn parses_a_response_head() {
        let raw = b"RTSP/1.0 200 OK\r\nCSeq: 3\r\nSession: ABC123\r\n\r\n";
        let resp = Response::parse_head(raw).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.reason, "OK");
        assert_eq!(resp.cseq(), Some(3));
        assert_eq!(resp.headers.get("session"), Some("ABC123"));
    }

    #[test]
    fn tolerates_bare_lf() {
        let raw = b"RTSP/1.0 200 OK\nCSeq: 1\n\n";
        let resp = Response::parse_head(raw).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.cseq(), Some(1));
    }

    #[test]
    fn rejects_missing_version_token() {
        assert!(Response::parse_head(b"200 OK\r\n\r\n").is_err());
    }

    #[test]
    fn rejects_header_with_no_colon() {
        assert!(Response::parse_head(b"RTSP/1.0 200 OK\r\nbroken-header\r\n\r\n").is_err());
    }

    #[test]
    fn content_length_defaults_to_zero() {
        let resp = Response::parse_head(b"RTSP/1.0 200 OK\r\n\r\n").unwrap();
        assert_eq!(resp.content_length(), 0);
    }

    proptest::proptest! {
        #[test]
        fn parse_head_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
            let _ = Response::parse_head(&bytes);
        }
    }
}
