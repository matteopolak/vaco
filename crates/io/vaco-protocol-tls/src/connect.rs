//! Connecting the underlying TCP socket and driving the `rustls` handshake.
//!
//! # Why this crate does not open `tcp:` through the registry
//!
//! [`vaco_protocol_core::Protocol::open`] returns `Box<dyn
//! vaco_io::MediaSource>` — read-only, by design (D5: v0.1 has zero muxers,
//! so the trait was never asked to express a duplex transport). A TLS
//! handshake is inherently duplex: send a `ClientHello`, read a
//! `ServerHello`, and so on, all before there is anything to hand back to a
//! demuxer at all. There is no way to get both directions out of one
//! `Protocol::open` call as the trait is shaped today.
//!
//! Measured against the reference (`ffmpeg -v debug`, D17; see the crate
//! docs for the exact transcript): `tls.c` genuinely opens its TCP transport
//! as a nested `tcp:` URL, which works there because `URLContext` is duplex
//! in the C model. Rather than special-case `vaco-protocol-core`'s trait for
//! this one caller — a real gap, reported rather than worked around, per
//! this crate's brief — [`connect_tcp`] resolves and connects a
//! [`std::net::TcpStream`] directly, reusing
//! `vaco_protocol_socket::addr::connect` and
//! `vaco_protocol_socket::url::parse` (the exact same address-resolution and
//! `//host:port[?opt]` parsing `tcp:` itself uses — this crate depends on
//! `vaco-protocol-socket` for them rather than duplicating the logic) and
//! preserves the whitelist property by calling
//! [`vaco_protocol_core::ProtocolEnv::check_scheme`] with `"tcp"` **by
//! hand**, exactly where [`vaco_protocol_core::ProtocolRegistry::resolve`]
//! would have called it for a real nested open.
//!
//! This does mean a `tls:` open is one recursion level "flatter" than the
//! reference's own nested-`tcp:`-open shape (`env`'s `depth` is not
//! incremented a second time for the TCP leg, since [`connect_tcp`] never
//! calls [`vaco_protocol_core::ProtocolEnv::descend`]). That is a bookkeeping
//! difference in how close to [`vaco_protocol_core::DEFAULT_RECURSION_LIMIT`]
//! a very deeply nested URL gets, not a whitelist bypass: W1 (blacklist), W2/W3
//! (whitelist/default-grant) and W4 (the depth check itself, against the
//! depth already reached) are all still evaluated with the caller's real
//! `env`.

use std::net::TcpStream;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use vaco_protocol_core::{ProtocolEnv, ProtocolError, Result};
use vaco_protocol_socket::url::HostPort;

use crate::options::TlsOptions;

/// A connected, handshake-complete TLS stream.
pub type TlsStream = StreamOwned<ClientConnection, TcpStream>;

/// Resolve `url.rest` into a `host:port`.
///
/// # Errors
/// [`ProtocolError::Malformed`] if the URL names no parseable `host:port`.
pub fn host_port(rest: &str) -> Result<HostPort> {
    vaco_protocol_socket::url::parse(rest)
        .map(|(hp, _)| hp)
        .ok_or(ProtocolError::Malformed {
            scheme: "tls",
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
) -> Result<TcpStream> {
    env.check_scheme("tcp")?;
    vaco_protocol_socket::addr::connect(hp, timeout)
}

/// Perform the TLS handshake over `tcp`, returning a stream ready for
/// application data.
///
/// `verify_name` is the hostname used for both SNI and (when `opts.verify`)
/// certificate verification: `hp.host` unless `-verifyhost` overrides it.
///
/// # Errors
/// [`ProtocolError::Malformed`] for a host name `rustls` cannot use as an SNI
/// value, a config-building failure (see [`crate::verify::client_config`]),
/// or a handshake I/O failure.
pub fn handshake(
    hp: &HostPort,
    tcp: TcpStream,
    opts: &TlsOptions,
    ca_pem: Option<&str>,
) -> Result<TlsStream> {
    let verify_name = if opts.verifyhost.is_empty() {
        hp.host.as_str()
    } else {
        opts.verifyhost.as_str()
    };
    let config = crate::verify::client_config(opts, ca_pem)?;
    let name = ServerName::try_from(verify_name.to_owned()).map_err(|_| {
        ProtocolError::Malformed {
            scheme: "tls",
            detail: "host name is not a valid TLS server name",
        }
    })?;
    let mut conn = ClientConnection::new(config, name).map_err(|_| ProtocolError::Malformed {
        scheme: "tls",
        detail: "could not start a TLS client connection",
    })?;
    let mut sock = tcp;
    // Drive the handshake to completion now, so a certificate rejection or a
    // peer that is not speaking TLS at all is reported by `open()`/`create()`
    // itself, rather than surfacing lazily on the first demuxer read.
    while conn.is_handshaking() {
        conn.complete_io(&mut sock).map_err(ProtocolError::from)?;
    }
    Ok(StreamOwned::new(conn, sock))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use vaco_io::CancelToken;
    use vaco_protocol_core::ProtocolRegistry;

    use super::*;

    #[test]
    fn tcp_connect_needs_tcp_on_the_whitelist() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = ProtocolRegistry::new();
        let cancel = CancelToken::new();
        // Deliberately does not name "tcp" — only "tls" (matching a real
        // caller that granted this open the "tls" scheme but nothing else).
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["tls"]);
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        };
        let err = connect_tcp(&hp, Some(Duration::from_millis(500)), &env).unwrap_err();
        assert!(matches!(err, ProtocolError::Denied { .. }));
    }

    #[test]
    fn tcp_connect_succeeds_once_tcp_is_granted() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = ProtocolRegistry::new();
        let cancel = CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["tls", "tcp"]);
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        };
        assert!(connect_tcp(&hp, Some(Duration::from_secs(2)), &env).is_ok());
    }

    #[test]
    fn handshake_against_a_non_tls_peer_is_an_error_not_a_panic() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                // Send garbage that is not a TLS record and close.
                use std::io::Write;
                let mut stream = stream;
                let _ = stream.write_all(b"not tls at all, sixteen bytes plus");
            }
        });
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        };
        let tcp = TcpStream::connect(("127.0.0.1", addr.port())).unwrap();
        let opts = TlsOptions::default();
        assert!(handshake(&hp, tcp, &opts, None).is_err());
        let _ = server.join();
    }
}
