//! The duplex RTSP control connection: connect, send a [`Request`], read
//! back either a [`Response`] or an interleaved `$`-framed RTP/RTCP chunk.
//!
//! # Why this crate connects its own `tcp:` rather than going through the registry
//!
//! Exactly [`vaco_protocol_tls`](../../vaco_protocol_tls/index.html)'s
//! reasoning, restated for RTSP: [`vaco_protocol_core::Protocol::open`]
//! returns a read-only `Box<dyn MediaSource>`, and an RTSP session is
//! inherently a duplex conversation (send `DESCRIBE`, read the response;
//! send `SETUP`, read the response; ...) before there is anything to hand a
//! caller at all. [`connect_tcp`] resolves and connects a
//! [`std::net::TcpStream`] directly, reusing `vaco_protocol_socket::addr::connect`
//! and `vaco_protocol_socket::url::parse`, and calls
//! [`vaco_protocol_core::ProtocolEnv::check_scheme`] with `"tcp"` **by
//! hand** — exactly where [`vaco_protocol_core::ProtocolRegistry::resolve`]
//! would call it for a real nested open. This is a reported gap in
//! `vaco-protocol-core`'s trait, not a workaround for it.
//!
//! # Interleaved framing (RFC 2326 §10.12)
//!
//! When the negotiated transport is TCP-interleaved or HTTP-tunnelled, RTP
//! and RTCP packets travel over this same connection as `$`-prefixed binary
//! chunks: `'$'`(1 byte) + channel(1 byte) + length(2 bytes, big-endian) +
//! that many bytes of RTP/RTCP payload. [`RtspConnection::read_message`]
//! tells the two apart by the first byte — `$` (0x24) starts a binary
//! frame, anything else starts an RTSP response's status line — so a
//! caller reading the control connection in a loop transparently receives
//! both kinds of message on one socket, which is what interleaved mode
//! actually is.

use std::io::{Read, Write};
use std::time::Duration;

use vaco_core::{Error, Result};
use vaco_protocol_core::{ProtocolEnv, ProtocolError};
use vaco_protocol_socket::url::HostPort;

use crate::message::{Request, Response};

/// Anything this connection can read from and write to: a real
/// [`std::net::TcpStream`] in production, an in-memory loopback stream (or
/// a `std::net::TcpStream` from a `TcpListener::bind("127.0.0.1:0")`) in
/// tests.
pub trait Duplex: Read + Write + Send {}
impl<T: Read + Write + Send> Duplex for T {}

/// One message read off the control connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnMessage {
    Response(Response, Vec<u8>),
    /// An RFC 2326 §10.12 interleaved frame: `(channel, payload)`.
    Interleaved(u8, Vec<u8>),
}

/// Resolve `host:port` into a `HostPort`, sharing `tcp:`'s own grammar.
///
/// # Errors
/// [`ProtocolError::Malformed`] if `rest` names no parseable `host:port`.
pub fn host_port(rest: &str) -> vaco_protocol_core::Result<HostPort> {
    vaco_protocol_socket::url::parse(rest)
        .map(|(hp, _)| hp)
        .ok_or(ProtocolError::Malformed {
            scheme: "rtsp",
            detail: "expected host:port",
        })
}

/// Connect the raw TCP transport, applying the whitelist check by hand (see
/// the module docs for why this cannot go through the registry).
///
/// # Errors
/// [`ProtocolError::Denied`] if `"tcp"` is not permitted by `env`; otherwise
/// whatever the connection attempt failed with.
pub fn connect_tcp(
    hp: &HostPort,
    timeout: Option<Duration>,
    env: &ProtocolEnv<'_>,
) -> vaco_protocol_core::Result<std::net::TcpStream> {
    env.check_scheme("tcp")?;
    vaco_protocol_socket::addr::connect(hp, timeout)
}

/// The RTSP control connection.
pub struct RtspConnection {
    stream: Box<dyn Duplex>,
    /// Bytes already read off the socket but not yet consumed by a caller —
    /// a message boundary rarely lines up with a `read()` call's return.
    buf: Vec<u8>,
}

impl std::fmt::Debug for RtspConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtspConnection")
            .field("buffered", &self.buf.len())
            .finish_non_exhaustive()
    }
}

impl RtspConnection {
    #[must_use]
    pub fn from_stream(stream: impl Duplex + 'static) -> Self {
        Self {
            stream: Box::new(stream),
            buf: Vec::new(),
        }
    }

    fn fill(&mut self, min_extra: usize) -> Result<()> {
        let mut tmp = [0u8; 4096];
        let mut got = 0usize;
        while got < min_extra {
            let n = self.stream.read(&mut tmp).map_err(Error::Io)?;
            if n == 0 {
                return Err(Error::UnexpectedEof);
            }
            self.buf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
            got += n;
        }
        Ok(())
    }

    /// Find the end of the head block (`\r\n\r\n` or a bare `\n\n`),
    /// reading more from the socket until one is found.
    fn read_head(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(pos) = find_subslice(&self.buf, b"\r\n\r\n") {
                let head = self.buf.get(..pos + 4).unwrap_or(&[]).to_vec();
                self.buf = self.buf.get(pos + 4..).unwrap_or(&[]).to_vec();
                return Ok(head);
            }
            if let Some(pos) = find_subslice(&self.buf, b"\n\n") {
                let head = self.buf.get(..pos + 2).unwrap_or(&[]).to_vec();
                self.buf = self.buf.get(pos + 2..).unwrap_or(&[]).to_vec();
                return Ok(head);
            }
            self.fill(1)?;
        }
    }

    fn read_exact_n(&mut self, n: usize) -> Result<Vec<u8>> {
        while self.buf.len() < n {
            self.fill(n - self.buf.len())?;
        }
        let out = self.buf.get(..n).unwrap_or(&[]).to_vec();
        self.buf = self.buf.get(n..).unwrap_or(&[]).to_vec();
        Ok(out)
    }

    fn peek_byte(&mut self) -> Result<u8> {
        if self.buf.is_empty() {
            self.fill(1)?;
        }
        self.buf
            .first()
            .copied()
            .ok_or(Error::InvalidData("RTSP connection produced no data"))
    }

    /// Read one message: either a complete RTSP response (head + body, if
    /// `Content-Length` names one) or one interleaved binary frame.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] if the peer closes mid-message;
    /// [`Error::InvalidData`] for a head that does not parse as RTSP.
    pub fn read_message(&mut self) -> Result<ConnMessage> {
        if self.peek_byte()? == b'$' {
            let frame_header = self.read_exact_n(4)?;
            let channel = *frame_header.get(1).unwrap_or(&0);
            let len_bytes: [u8; 2] = frame_header
                .get(2..4)
                .and_then(|s| s.try_into().ok())
                .unwrap_or([0, 0]);
            let len = usize::from(u16::from_be_bytes(len_bytes));
            let payload = self.read_exact_n(len)?;
            return Ok(ConnMessage::Interleaved(channel, payload));
        }
        let head = self.read_head()?;
        let resp = Response::parse_head(&head)?;
        let body = self.read_exact_n(resp.content_length())?;
        Ok(ConnMessage::Response(resp, body))
    }

    /// Write a request's bytes to the connection.
    ///
    /// # Errors
    /// Propagates a transport write failure.
    pub fn send(&mut self, req: &Request) -> Result<()> {
        self.stream.write_all(&req.to_bytes()).map_err(Error::Io)
    }

    /// Write a raw interleaved frame (used to send RTCP over TCP/HTTP
    /// interleaved transports).
    ///
    /// # Errors
    /// Propagates a transport write failure.
    pub fn send_interleaved(&mut self, channel: u8, data: &[u8]) -> Result<()> {
        let len = u16::try_from(data.len()).map_err(|_| {
            Error::InvalidData("interleaved frame is too large for a 16-bit length")
        })?;
        let mut out = Vec::new();
        out.push(b'$');
        out.push(channel);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(data);
        self.stream.write_all(&out).map_err(Error::Io)
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack.get(i..i + needle.len()) == Some(needle))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    #[test]
    fn reads_a_response_with_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            sock.write_all(b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: 5\r\n\r\nhello")
                .unwrap();
        });
        let stream = TcpStream::connect(addr).unwrap();
        let mut conn = RtspConnection::from_stream(stream);
        let msg = conn.read_message().unwrap();
        match msg {
            ConnMessage::Response(resp, body) => {
                assert_eq!(resp.status, 200);
                assert_eq!(body, b"hello");
            }
            ConnMessage::Interleaved(..) => panic!("expected a Response"),
        }
        server.join().unwrap();
    }

    #[test]
    fn reads_an_interleaved_frame_between_responses() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut frame = vec![b'$', 0u8];
            frame.extend_from_slice(&4u16.to_be_bytes());
            frame.extend_from_slice(b"data");
            sock.write_all(&frame).unwrap();
            sock.write_all(b"RTSP/1.0 200 OK\r\nCSeq: 2\r\n\r\n")
                .unwrap();
        });
        let stream = TcpStream::connect(addr).unwrap();
        let mut conn = RtspConnection::from_stream(stream);
        match conn.read_message().unwrap() {
            ConnMessage::Interleaved(ch, data) => {
                assert_eq!(ch, 0);
                assert_eq!(data, b"data");
            }
            ConnMessage::Response(..) => panic!("expected an interleaved frame"),
        }
        match conn.read_message().unwrap() {
            ConnMessage::Response(resp, _) => assert_eq!(resp.cseq(), Some(2)),
            ConnMessage::Interleaved(..) => panic!("expected a Response"),
        }
        server.join().unwrap();
    }

    #[test]
    fn tcp_connect_needs_tcp_on_the_whitelist() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["rtsp"]);
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        };
        let err = connect_tcp(&hp, Some(Duration::from_millis(500)), &env).unwrap_err();
        assert!(matches!(err, ProtocolError::Denied { .. }));
    }
}
