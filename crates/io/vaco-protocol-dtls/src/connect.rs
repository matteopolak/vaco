//! Connecting the underlying UDP socket and driving the client-side DTLS
//! handshake. See [`crate::listen`] for the server side.
//!
//! # Why this crate does not open `udp:` through the registry
//!
//! Same reason `vaco-protocol-tls` does not open `tcp:` through the registry
//! (see that crate's `connect` module docs for the full argument):
//! [`vaco_protocol_core::Protocol::open`] returns a read-only `Box<dyn
//! MediaSource>`, and a DTLS handshake needs both directions on one
//! connection before there is anything to hand back at all. This crate
//! resolves and connects its own [`std::net::UdpSocket`] directly, reusing
//! [`vaco_protocol_socket::addr::resolve`] for name resolution, and applies
//! the whitelist check by hand via
//! [`vaco_protocol_core::ProtocolEnv::check_scheme`] with `"udp"`, exactly
//! where [`vaco_protocol_core::ProtocolRegistry::resolve`] would have called
//! it for a real nested open.

use std::net::UdpSocket;
use std::time::Duration;

use openssl::ssl::{HandshakeError, Ssl, SslStream};
use vaco_protocol_core::{ProtocolEnv, ProtocolError, Result};
use vaco_protocol_socket::url::HostPort;

use crate::options::DtlsOptions;
use crate::transport::UdpTransport;

/// A connected, handshake-complete DTLS session.
pub type DtlsStream = SslStream<UdpTransport>;

/// RFC 5764 §4.2's fixed label for DTLS-SRTP keying-material export.
const SRTP_KEYING_MATERIAL_LABEL: &str = "EXTRACTOR-dtls_srtp";

/// Export the keying material an SRTP layer (`vaco-protocol-srtp`) derives
/// its master keys/salts from, once the handshake has negotiated a
/// `use_srtp` profile (`-use_srtp 1`; see [`crate::options::DtlsOptions`]).
///
/// `out.len()` decides how many bytes are exported — the caller sizes it per
/// RFC 5764 §4.1.2 for the profile actually negotiated (`selected profile`
/// is available via `stream.ssl().selected_srtp_profile()`), not this
/// crate's business to assume.
///
/// # Errors
/// [`ProtocolError::Malformed`] if the underlying OpenSSL export call fails
/// (typically: the handshake never negotiated `use_srtp` at all).
pub fn export_srtp_keying_material(stream: &DtlsStream, out: &mut [u8]) -> Result<()> {
    export_srtp_keying_material_from(stream, out)
}

/// As [`export_srtp_keying_material`], generic over the transport — the
/// [`handshake_over`] counterpart, for a caller whose completed handshake is
/// an `SslStream<S>` rather than the concrete [`DtlsStream`].
///
/// # Errors
/// As [`export_srtp_keying_material`].
pub fn export_srtp_keying_material_from<S>(
    stream: &openssl::ssl::SslStream<S>,
    out: &mut [u8],
) -> Result<()> {
    stream
        .ssl()
        .export_keying_material(out, SRTP_KEYING_MATERIAL_LABEL, None)
        .map_err(|_| ProtocolError::Malformed {
            scheme: "dtls",
            detail: "could not export SRTP keying material (was use_srtp negotiated?)",
        })
}

/// Resolve `url.rest` into a `host:port`. Discards any inline `?key=value`
/// query options — the same security property `vaco-protocol-tls`'s
/// `connect::host_port` keeps, for the same reason: `-ca_file`/`-cert_file`/
/// `-key_file` only ever come from the trusted `-opt`/`Dict` surface, never
/// from a URL's own query string.
///
/// # Errors
/// [`ProtocolError::Malformed`] if the URL names no parseable `host:port`.
pub fn host_port(rest: &str) -> Result<HostPort> {
    vaco_protocol_socket::url::parse(rest)
        .map(|(hp, _)| hp)
        .ok_or(ProtocolError::Malformed {
            scheme: "dtls",
            detail: "expected host:port",
        })
}

/// Connect a UDP socket to `hp`, applying the whitelist check by hand (see
/// the module docs for why this cannot go through the registry).
///
/// # Errors
/// [`ProtocolError::Denied`] if `"udp"` is not permitted by `env`; otherwise
/// whatever the connection attempt failed with.
pub fn connect_udp(
    hp: &HostPort,
    _timeout: Option<Duration>,
    env: &ProtocolEnv<'_>,
) -> Result<UdpSocket> {
    env.check_scheme("udp")?;
    let addrs = vaco_protocol_socket::addr::resolve(hp)?;
    let bind_addr = if addrs.first().is_some_and(std::net::SocketAddr::is_ipv6) {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let socket = UdpSocket::bind(bind_addr).map_err(ProtocolError::from)?;
    let mut last_err = None;
    for addr in addrs {
        match socket.connect(addr) {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        return Err(ProtocolError::from(e));
    }
    Ok(socket)
}

pub(crate) fn handshake_error<S>(err: &HandshakeError<S>) -> ProtocolError {
    let detail = match err {
        HandshakeError::SetupFailure(e) => e.to_string(),
        HandshakeError::Failure(s) | HandshakeError::WouldBlock(s) => s.error().to_string(),
    };
    ProtocolError::from(std::io::Error::other(format!(
        "DTLS handshake failed: {detail}"
    )))
}

/// Perform the client-side DTLS handshake, returning a stream ready for
/// application data.
///
/// # Errors
/// [`ProtocolError::Malformed`] for a bad certificate/key/`ca_file`
/// configuration (see [`crate::context::build`]), or a handshake I/O
/// failure.
pub fn handshake(
    socket: UdpSocket,
    opts: &DtlsOptions,
    cert_file_pem: Option<&str>,
    key_file_pem: Option<&str>,
    ca_file_pem: Option<&str>,
) -> Result<DtlsStream> {
    handshake_over(
        UdpTransport::new(socket),
        opts,
        cert_file_pem,
        key_file_pem,
        ca_file_pem,
    )
}

/// As [`handshake`], but over any caller-supplied [`Read`]/[`Write`]
/// transport rather than a bare [`UdpSocket`].
///
/// # Why this exists
///
/// A WebRTC-shaped caller's UDP socket carries three interleaved protocols
/// on one 5-tuple — STUN, DTLS and (once the handshake finishes) SRTP,
/// demultiplexed by RFC 7983's first-byte ranges. `vaco-mux-whip` (#619)
/// found this the hard way, against a real peer: `mediamtx` runs a full ICE
/// agent, not ICE-lite, and keeps sending its own STUN Binding Requests to
/// the publisher throughout the handshake window — requests this crate's
/// plain [`UdpTransport`] cannot tell apart from DTLS records, since it
/// hands every datagram straight to OpenSSL. A caller that must answer
/// those requests (or otherwise inspect/filter datagrams) needs its own
/// transport in the loop, which `handshake`'s fixed `UdpSocket` parameter
/// cannot express; this generic entry point is that seam, with `handshake`
/// itself now a thin wrapper reusing it.
///
/// `Read`/`Write` requires `'static` because the returned `SslStream` is
/// `'static`-independent of the borrow checker only when `S` itself is —
/// matching [`UdpTransport`]'s own owned-socket shape.
///
/// # Errors
/// As [`handshake`].
pub fn handshake_over<S: std::io::Read + std::io::Write>(
    transport: S,
    opts: &DtlsOptions,
    cert_file_pem: Option<&str>,
    key_file_pem: Option<&str>,
    ca_file_pem: Option<&str>,
) -> Result<openssl::ssl::SslStream<S>> {
    let ctx = crate::context::build(opts, cert_file_pem, key_file_pem, ca_file_pem)?;
    let mut ssl = Ssl::new(&ctx).map_err(|_| ProtocolError::Malformed {
        scheme: "dtls",
        detail: "could not start a DTLS client connection",
    })?;
    crate::context::apply_mtu(&mut ssl, opts.mtu)?;
    ssl.connect(transport).map_err(|e| handshake_error(&e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use vaco_io::CancelToken;
    use vaco_protocol_core::ProtocolRegistry;

    use super::*;

    #[test]
    fn udp_connect_needs_udp_on_the_whitelist() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = ProtocolRegistry::new();
        let cancel = CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["dtls"]);
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        };
        let err = connect_udp(&hp, Some(Duration::from_millis(500)), &env).unwrap_err();
        assert!(matches!(err, ProtocolError::Denied { .. }));
    }

    #[test]
    fn udp_connect_succeeds_once_udp_is_granted() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = ProtocolRegistry::new();
        let cancel = CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["dtls", "udp"]);
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        };
        assert!(connect_udp(&hp, Some(Duration::from_secs(2)), &env).is_ok());
    }

    #[test]
    fn handshake_against_a_non_dtls_peer_is_an_error_not_a_panic() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.connect(addr).unwrap();
        // Send garbage that is not a DTLS record from the "peer" side, then
        // let the handshake attempt read it back as a response.
        listener
            .send_to(
                b"not dtls at all, sixteen bytes plus",
                client.local_addr().unwrap(),
            )
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let opts = DtlsOptions::default();
        assert!(handshake(client, &opts, None, None, None).is_err());
    }
}
