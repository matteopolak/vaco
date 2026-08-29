//! `-listen 1`: bind and accept a single DTLS peer instead of connecting to
//! one. See [`crate::connect`] for the client side.
//!
//! # Scope: one peer per bind, no stateless cookie exchange
//!
//! [`bind_accept`] waits for the *first* datagram on the bound socket,
//! `connect`s the socket to whichever address sent it, and hands the
//! resulting connected socket to the DTLS handshake — the same
//! "connect-on-first-packet" shape `vaco-protocol-socket::tcp::listen_accept`
//! uses for `tcp:`/`unix:`, adapted to UDP's connectionless model. This
//! deliberately does not implement RFC 6347's stateless
//! `HelloVerifyRequest`/cookie exchange (`DTLSv1_listen`'s whole reason to
//! exist): that mechanism lets one *unconnected* socket serve many
//! prospective peers without committing per-client state until a cookie
//! round-trip proves the peer's address is not spoofed — valuable for a
//! server exposed to the open internet, not needed for `dtls:`'s own use
//! (one call opens one connection to one peer, mirroring every other
//! `-listen`-capable protocol in this workspace). Recorded here rather than
//! silently narrowed, per this project's own "measure the thing that can be
//! wrong" rule.

use std::net::UdpSocket;
use std::time::Duration;

use openssl::ssl::Ssl;
use vaco_protocol_core::ProtocolError;
use vaco_protocol_core::Result;
use vaco_protocol_socket::url::HostPort;
use vaco_time::{Instant, sleep};

use crate::connect::DtlsStream;
use crate::options::DtlsOptions;
use crate::transport::UdpTransport;

/// How often [`bind_accept`]'s wait loop polls the non-blocking socket.
/// Small enough that `rw_timeout` is honoured to within this granularity,
/// large enough not to spin a CPU core waiting for a peer that may never
/// come.
const ACCEPT_POLL: Duration = Duration::from_millis(20);

/// Bind to `hp` and wait for the first datagram, connecting the socket to
/// whoever sent it. `rw_timeout` bounds the wait (`None` waits indefinitely)
/// — see `crate::options`' module docs for why there is no separate
/// `-listen_timeout` here, unlike `tcp:`.
///
/// # Errors
/// The bind failure, or [`ProtocolError::Io`] wrapping a timed-out wait
/// (reported as [`std::io::ErrorKind::TimedOut`]).
pub fn bind_accept(hp: &HostPort, rw_timeout: Option<Duration>) -> Result<UdpSocket> {
    let bind_addr = if hp.host.is_empty() {
        format!("0.0.0.0:{}", hp.port)
    } else {
        format!("{}:{}", hp.host, hp.port)
    };
    let socket = UdpSocket::bind(&bind_addr).map_err(ProtocolError::from)?;
    socket.set_nonblocking(true).map_err(ProtocolError::from)?;

    let deadline = rw_timeout.map(|t| Instant::now().saturating_add(t));
    let max_polls = rw_timeout.map_or(usize::MAX, |t| {
        t.as_nanos()
            .div_ceil(ACCEPT_POLL.as_nanos().max(1))
            .try_into()
            .unwrap_or(usize::MAX)
    });

    let mut probe = [0_u8; 1];
    for _ in 0..=max_polls {
        match socket.peek_from(&mut probe) {
            Ok((_, peer)) => {
                socket.connect(peer).map_err(ProtocolError::from)?;
                socket.set_nonblocking(false).map_err(ProtocolError::from)?;
                return Ok(socket);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(ProtocolError::from(e)),
        }
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            break;
        }
        sleep(ACCEPT_POLL);
    }
    Err(ProtocolError::from(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "no peer datagram arrived while listening",
    )))
}

/// Perform the server-side DTLS handshake over an already-`connect`ed
/// socket (from [`bind_accept`]).
///
/// # Errors
/// [`ProtocolError::Malformed`] for a bad certificate/key/`ca_file`
/// configuration, or a handshake I/O failure.
pub fn handshake(
    socket: UdpSocket,
    opts: &DtlsOptions,
    cert_file_pem: Option<&str>,
    key_file_pem: Option<&str>,
    ca_file_pem: Option<&str>,
) -> Result<DtlsStream> {
    let ctx = crate::context::build(opts, cert_file_pem, key_file_pem, ca_file_pem)?;
    let mut ssl = Ssl::new(&ctx).map_err(|_| ProtocolError::Malformed {
        scheme: "dtls",
        detail: "could not start a DTLS server connection",
    })?;
    crate::context::apply_mtu(&mut ssl, opts.mtu)?;
    ssl.accept(UdpTransport::new(socket))
        .map_err(|e| crate::connect::handshake_error(&e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn bind_accept_times_out_with_no_peer() {
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: 0,
        };
        let err = bind_accept(&hp, Some(Duration::from_millis(100))).unwrap_err();
        assert!(matches!(err, ProtocolError::Io(_)));
    }

    #[test]
    fn bind_accept_connects_to_the_first_sender() {
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: 0,
        };
        // Bind first so we know the real port to send to.
        let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);
        let hp = HostPort { port: addr.port(), ..hp };

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client_addr = client.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            client.send_to(b"hi", (hp.host.as_str(), hp.port)).unwrap();
        });

        let server = bind_accept(
            &HostPort {
                host: "127.0.0.1".to_owned(),
                port: addr.port(),
            },
            Some(Duration::from_secs(2)),
        )
        .unwrap();
        assert_eq!(server.peer_addr().unwrap(), client_addr);
        sender.join().unwrap();
    }
}
