//! A minimal, one-shot HTTP/1.1 client for the WHIP signalling exchange:
//! one `POST` with the SDP offer, an optional `PATCH`/`DELETE` against the
//! `Location` the server handed back.
//!
//! # Why this crate does not just call `vaco-protocol-http`
//!
//! That crate owns `ureq` (D11's single-owner rule: `cargo xtask owner-gate`
//! fails the build the moment a second crate declares it) and is built
//! around `vaco_io::MediaSource`/`MediaSink` — a byte *stream*, read or
//! written incrementally. WHIP's signalling exchange is the opposite shape:
//! one small, fully-buffered request and one fully-buffered response, read
//! synchronously before anything else can happen (the SDP answer has to be
//! parsed before ICE can even start). Reusing `vaco-protocol-http` would
//! mean depending on `ureq`'s own response type to read it back out, which
//! is exactly the dependency this crate avoids by writing the ~80 lines of
//! wire format itself. [`vaco_protocol_http::url::resolve_location`] *is*
//! reused directly — it is pure string manipulation with no `ureq` in its
//! signature, the one part of that crate built to be reused this way (see
//! its own module docs).
//!
//! # What is deliberately not implemented
//!
//! `https://` (would need a TLS handshake; every WHIP endpoint measured so
//! far runs its signalling over plain `http://` on a private/loopback
//! network path, with the media itself always DTLS/SRTP-encrypted
//! regardless — see the crate's top-level docs), chunked transfer-encoding
//! on the response (a WHIP answer is a short, `Content-Length`-declared SDP
//! body in every server measured), redirects, and connection reuse (one
//! TCP connection per request, closed after).

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use vaco_core::{Error, Result};
use vaco_protocol_socket::url::HostPort;

/// A fully-buffered HTTP response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The first header matching `name`, case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Split `http://host[:port]/path[?query]` into `(host, port, path)`.
///
/// # Errors
/// [`Error::Unsupported`] for any scheme other than `http`; [`Error::InvalidData`]
/// for a URL with no host.
fn split_url(url: &str) -> Result<(HostPort, String)> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        if url.starts_with("https://") {
            Error::Unsupported("https WHIP endpoints are not supported; use http://")
        } else {
            Error::InvalidData("WHIP endpoint is not an http:// URL")
        }
    })?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |i| {
        // `i` came from `rest.find`, so it is always a valid split point.
        rest.split_at_checked(i).unwrap_or((rest, "/"))
    });
    if authority.is_empty() {
        return Err(Error::InvalidData("WHIP endpoint names no host"));
    }
    let (host, port) = authority.split_once(':').map_or_else(
        || (authority.to_owned(), 80u16),
        |(h, p)| (h.to_owned(), p.parse().unwrap_or(80)),
    );
    let path = if path.is_empty() { "/" } else { path };
    Ok((HostPort { host, port }, path.to_owned()))
}

/// Perform one HTTP/1.1 request and return the fully-read response.
///
/// `body`/`content_type` are both `Some` or both `None` — a WHIP `POST`
/// carries `application/sdp`, a `DELETE` carries nothing.
///
/// # Errors
/// [`Error::Io`] for any socket failure; [`Error::InvalidData`] for a
/// response this parser cannot make sense of (no status line, a header
/// block that never ends, a declared `Content-Length` the connection closes
/// before delivering).
pub fn request(
    method: &str,
    url: &str,
    content_type: Option<&str>,
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<Response> {
    let (hp, path) = split_url(url)?;
    let mut stream = vaco_protocol_socket::addr::connect(&hp, Some(timeout))?;
    stream.set_read_timeout(Some(timeout)).map_err(Error::Io)?;
    stream.set_write_timeout(Some(timeout)).map_err(Error::Io)?;

    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: vaco\r\nAccept: */*\r\nConnection: close\r\n",
        host = hp.host,
    );
    if let (Some(ct), Some(b)) = (content_type, body) {
        let _ = write!(
            req,
            "Content-Type: {ct}\r\nContent-Length: {len}\r\n",
            len = b.len()
        );
    }
    req.push_str("\r\n");

    stream.write_all(req.as_bytes()).map_err(Error::Io)?;
    if let Some(b) = body {
        stream.write_all(b).map_err(Error::Io)?;
    }

    read_response(&mut stream)
}

/// Read a whole HTTP/1.1 response off `stream`: status line, headers, then
/// exactly `Content-Length` bytes of body (`0` if the header is absent).
fn read_response(stream: &mut TcpStream) -> Result<Response> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_header_end(&raw) {
            break pos;
        }
        let n = stream.read(&mut chunk).map_err(Error::Io)?;
        if n == 0 {
            return Err(Error::InvalidData(
                "connection closed before the response headers ended",
            ));
        }
        raw.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
        if raw.len() > 64 * 1024 {
            return Err(Error::InvalidData("response header block too large"));
        }
    };

    let head = String::from_utf8_lossy(raw.get(..header_end).unwrap_or(&[])).into_owned();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or(Error::InvalidData("malformed HTTP status line"))?;

    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }

    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);

    let already = raw.len().saturating_sub(header_end + 4);
    let mut body = raw.get(header_end + 4..).unwrap_or(&[]).to_vec();
    let mut remaining = content_length.saturating_sub(already);
    while remaining > 0 {
        let want = remaining.min(chunk.len());
        let Some(dst) = chunk.get_mut(..want) else {
            break;
        };
        let n = stream.read(dst).map_err(Error::Io)?;
        if n == 0 {
            break; // connection closed early; return what we have
        }
        body.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
        remaining = remaining.saturating_sub(n);
    }

    Ok(Response {
        status,
        headers,
        body,
    })
}

/// Byte offset of the `\r\n\r\n` that ends the header block, if `buf`
/// contains one yet.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;

    #[test]
    fn split_url_rejects_https() {
        assert!(matches!(
            split_url("https://host/whip"),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn split_url_defaults_port_and_path() {
        let (hp, path) = split_url("http://example.com").unwrap();
        assert_eq!(hp.host, "example.com");
        assert_eq!(hp.port, 80);
        assert_eq!(path, "/");
    }

    #[test]
    fn split_url_reads_explicit_port_and_path() {
        let (hp, path) = split_url("http://127.0.0.1:8889/room/whip").unwrap();
        assert_eq!(hp.host, "127.0.0.1");
        assert_eq!(hp.port, 8889);
        assert_eq!(path, "/room/whip");
    }

    #[test]
    fn round_trips_a_post_against_a_hand_built_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(sock.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            assert!(request_line.starts_with("POST /whip HTTP/1.1"));
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((k, v)) = line.split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    content_length = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; content_length];
            std::io::Read::read_exact(&mut reader, &mut body).unwrap();
            assert_eq!(body, b"v=0\r\n");
            let resp = b"HTTP/1.1 201 Created\r\nContent-Type: application/sdp\r\nLocation: /whip/1\r\nContent-Length: 4\r\n\r\nv=0\n";
            sock.write_all(resp).unwrap();
        });

        let resp = request(
            "POST",
            &format!("http://{addr}/whip"),
            Some("application/sdp"),
            Some(b"v=0\r\n"),
            Duration::from_secs(2),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(resp.status, 201);
        assert_eq!(resp.header("location"), Some("/whip/1"));
        assert_eq!(resp.body, b"v=0\n");
    }

    #[test]
    fn a_response_with_no_content_length_has_an_empty_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf);
            sock.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
        });
        let resp = request(
            "DELETE",
            &format!("http://{addr}/whip/1"),
            None,
            None,
            Duration::from_secs(2),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(resp.status, 204);
        assert!(resp.body.is_empty());
    }
}
