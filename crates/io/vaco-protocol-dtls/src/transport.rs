//! [`UdpTransport`]: a connected [`UdpSocket`] wearing a [`Read`]/[`Write`]
//! coat, which is all `openssl::ssl::SslStream` needs from its underlying
//! transport.
//!
//! `std::net::UdpSocket` does not implement `Read`/`Write` itself — datagram
//! semantics do not fit the streaming-byte model those traits describe — but
//! a socket already [`connect`](UdpSocket::connect)ed to exactly one peer
//! behaves enough like a stream for DTLS's purposes: `openssl` reads and
//! writes whole records, never partial ones, so one `read`/`write` call here
//! is one `recv`/`send` there, with no reassembly needed on either side.
//!
//! # What this does not do
//!
//! DTLS's own retransmission timers (RFC 6347 §4.2.4: resend a handshake
//! flight if the peer's response does not arrive in time) are not
//! implemented here — this is a plain blocking transport, not one that
//! drives `openssl`'s `DTLSv1_get_timeout`/`DTLSv1_handle_timeout` pair on a
//! schedule. On a lossy link, a lost handshake packet stalls rather than
//! retries. Every test in this crate runs over loopback, where that gap does
//! not show up; a real deployment across the open internet would want this
//! filled in before depending on it. Recorded here rather than silently
//! shipped, per this project's own "measure the thing that can be wrong"
//! rule.

use std::io::{self, Read, Write};
use std::net::UdpSocket;

/// A [`UdpSocket`] already `connect`ed to exactly one peer, read/written a
/// whole datagram at a time.
#[derive(Debug)]
pub struct UdpTransport {
    socket: UdpSocket,
}

impl UdpTransport {
    #[must_use]
    pub const fn new(socket: UdpSocket) -> Self {
        Self { socket }
    }

    /// Hand back the underlying socket, e.g. to set options `openssl` has no
    /// opinion about.
    #[must_use]
    pub const fn socket(&self) -> &UdpSocket {
        &self.socket
    }
}

impl Read for UdpTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.socket.recv(buf)
    }
}

impl Write for UdpTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.socket.send(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        // UDP has no buffering to flush: every `send` above is already one
        // whole, immediately-transmitted datagram.
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_datagram_reads_back_whole() {
        let a = UdpSocket::bind("127.0.0.1:0").unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").unwrap();
        a.connect(b.local_addr().unwrap()).unwrap();
        b.connect(a.local_addr().unwrap()).unwrap();
        let mut ta = UdpTransport::new(a);
        let mut tb = UdpTransport::new(b);

        ta.write_all(b"hello dtls transport").unwrap();
        let mut buf = [0u8; 64];
        let n = tb.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello dtls transport");
    }
}
