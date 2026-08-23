//! RTSP-over-HTTP tunnelling (Apple's scheme, undocumented by any RFC but
//! widely deployed and what `ffmpeg -rtsp_transport http` speaks): two HTTP
//! connections to the same host, one `GET` (the server-to-client leg) and
//! one `POST` (client-to-server), both carrying Base64-encoded RTSP
//! messages tied together by a shared `x-sessioncookie` header.
//!
//! # Why this exists instead of going through `vaco-protocol-http`
//!
//! `vaco-protocol-http` is built around `ureq`'s request/response model —
//! send a complete request, get a complete response back. RTSP-over-HTTP's
//! `GET` response body is unbounded and streamed live for the life of the
//! session (every RTSP response, and every interleaved RTP/RTCP frame,
//! arrives as more of that one body), and the `POST` body is written to
//! incrementally over the same lifetime — neither leg is a single
//! request/response round trip. This crate therefore opens its own raw
//! `tcp:` connections for both legs (through [`crate::connection::connect_tcp`],
//! the same whitelist-checked-by-hand path the plain TCP control connection
//! uses) and speaks just enough of HTTP/1.1 by hand to get the two
//! long-lived bodies going.
//!
//! [`HttpTunnelStream`] then presents both legs as one [`crate::connection::Duplex`],
//! transcoding to/from Base64 transparently, so `crate::connection::RtspConnection`
//! never has to know its underlying transport is two HTTP legs rather than
//! one bare socket.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use vaco_core::{Error, Result};
use vaco_protocol_core::ProtocolEnv;
use vaco_protocol_socket::url::HostPort;

use crate::base64;
use crate::connection::connect_tcp;

/// A pseudo-random session cookie, RFC 4122-adjacent but not a real UUID —
/// nothing in the tunnelling scheme requires cryptographic randomness, only
/// that the GET and POST legs agree on the same opaque string.
fn session_cookie() -> String {
    let nanos = vaco_time::Instant::now();
    let mixed = format!("{nanos:?}");
    base64::encode(mixed.as_bytes())
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(24)
        .collect()
}

fn http_request(method: &str, host: &str, path: &str, cookie: &str, extra: &str) -> Vec<u8> {
    format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         x-sessioncookie: {cookie}\r\n\
         Accept: application/x-rtsp-tunnelled\r\n\
         Pragma: no-cache\r\n\
         Cache-Control: no-cache\r\n\
         {extra}\
         Connection: keep-alive\r\n\r\n"
    )
    .into_bytes()
}

/// Read an HTTP response's status line and headers (up to the blank line),
/// returning the status code. Does not read any body — the caller decides
/// how much of it to consume, since the GET leg's body is unbounded.
fn read_http_status(stream: &mut TcpStream) -> Result<u16> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).map_err(Error::Io)?;
        if n == 0 {
            return Err(Error::UnexpectedEof);
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") || buf.ends_with(b"\n\n") {
            break;
        }
        if buf.len() > 16 * 1024 {
            return Err(Error::InvalidData("HTTP tunnel response head is too large"));
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let status_line = text.lines().next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    parts.next(); // HTTP/1.1
    let code: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or(Error::InvalidData(
            "HTTP tunnel response has no status code",
        ))?;
    Ok(code)
}

/// Both legs of an RTSP-over-HTTP tunnel, presented as one duplex byte
/// stream — reads decode Base64 off the `GET` leg, writes encode Base64
/// onto the `POST` leg.
#[derive(Debug)]
pub struct HttpTunnelStream {
    get: TcpStream,
    post: TcpStream,
    /// Base64 text read from `get` but not yet decoded (an incomplete
    /// 4-character group).
    text_buf: Vec<u8>,
    /// Decoded bytes ready to be handed to a `read()` caller.
    decoded_buf: Vec<u8>,
}

impl HttpTunnelStream {
    /// Perform the `GET`/`POST` handshake and return a stream ready for
    /// [`crate::connection::RtspConnection::from_stream`].
    ///
    /// Returns [`vaco_protocol_core::Result`], not [`vaco_core::Result`],
    /// specifically so [`vaco_protocol_core::ProtocolError::Denied`] (the
    /// whitelist gate refusing the nested `tcp` open) stays distinguishable
    /// from an ordinary transport failure all the way out of this
    /// function — collapsing it into a generic I/O error here would be
    /// exactly the kind of information loss the crate's security posture
    /// docs argue against.
    ///
    /// # Errors
    /// [`vaco_protocol_core::ProtocolError::Denied`] if `"tcp"` is not
    /// permitted by `env`; [`vaco_protocol_core::ProtocolError::Io`] for any
    /// other transport failure, including either leg not answering `200`.
    pub fn connect(
        hp: &HostPort,
        path: &str,
        timeout: Option<Duration>,
        env: &ProtocolEnv<'_>,
    ) -> vaco_protocol_core::Result<Self> {
        let cookie = session_cookie();
        let mut get = connect_tcp(hp, timeout, env)?;
        get.write_all(&http_request("GET", &hp.host, path, &cookie, ""))
            .map_err(|e| vaco_protocol_core::ProtocolError::Io(Error::Io(e)))?;
        let get_status =
            read_http_status(&mut get).map_err(vaco_protocol_core::ProtocolError::Io)?;
        if get_status != 200 {
            return Err(vaco_protocol_core::ProtocolError::Io(Error::InvalidData(
                "RTSP-over-HTTP GET leg did not answer 200",
            )));
        }

        let mut post = connect_tcp(hp, timeout, env)?;
        let extra = "Content-Type: application/x-rtsp-tunnelled\r\nContent-Length: 32767\r\n";
        post.write_all(&http_request("POST", &hp.host, path, &cookie, extra))
            .map_err(|e| vaco_protocol_core::ProtocolError::Io(Error::Io(e)))?;

        Ok(Self {
            get,
            post,
            text_buf: Vec::new(),
            decoded_buf: Vec::new(),
        })
    }
}

impl Read for HttpTunnelStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.decoded_buf.is_empty() {
            let mut chunk = [0u8; 512];
            let n = self.get.read(&mut chunk)?;
            if n == 0 {
                return Ok(0);
            }
            self.text_buf
                .extend_from_slice(chunk.get(..n).unwrap_or(&[]));
            // Decode whatever whole 4-character groups are available,
            // keeping any remainder for the next read.
            #[allow(
                clippy::integer_division,
                reason = "rounding down to a multiple of 4, not computing a quotient"
            )]
            let usable = (self.text_buf.len() / 4) * 4;
            if usable > 0 {
                let text = String::from_utf8_lossy(self.text_buf.get(..usable).unwrap_or(&[]))
                    .into_owned();
                self.decoded_buf.extend(base64::decode(&text));
                self.text_buf = self.text_buf.get(usable..).unwrap_or(&[]).to_vec();
            }
        }
        let n = out.len().min(self.decoded_buf.len());
        if let Some(src) = self.decoded_buf.get(..n)
            && let Some(dst) = out.get_mut(..n)
        {
            dst.copy_from_slice(src);
        }
        self.decoded_buf = self.decoded_buf.get(n..).unwrap_or(&[]).to_vec();
        Ok(n)
    }
}

impl Write for HttpTunnelStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let encoded = base64::encode(buf);
        self.post.write_all(encoded.as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.post.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn session_cookie_is_nonempty_and_alphanumeric() {
        let c = session_cookie();
        assert!(!c.is_empty());
        assert!(c.chars().all(|ch| ch.is_ascii_alphanumeric()));
    }
}
