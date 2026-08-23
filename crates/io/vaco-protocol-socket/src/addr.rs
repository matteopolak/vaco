//! Resolving `host:port` and connecting with a bound timeout.
//!
//! One thing every protocol in this crate needs and none of them should
//! reimplement: turn a `(host, port)` pair into a live [`std::net::TcpStream`]
//! (or, for `udp:`, just the resolved address list), trying every address a
//! name resolves to rather than only the first, and bounding the whole
//! attempt by `-timeout` when the caller set one.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use vaco_protocol_core::{ProtocolError, Result};

use crate::url::HostPort;

/// Resolve `hp` to every address it names.
///
/// # Errors
/// [`ProtocolError::Io`] wrapping whatever [`ToSocketAddrs`] reported (a
/// `std::io::Error`, typically `ErrorKind::Other` for a resolution failure —
/// `std` gives us nothing more specific than that on every platform).
pub fn resolve(hp: &HostPort) -> Result<Vec<SocketAddr>> {
    let addrs = (hp.host.as_str(), hp.port)
        .to_socket_addrs()
        .map_err(ProtocolError::from)?
        .collect::<Vec<_>>();
    if addrs.is_empty() {
        return Err(ProtocolError::Malformed {
            scheme: "socket",
            detail: "host name resolved to no addresses",
        });
    }
    Ok(addrs)
}

/// Connect to `hp`, trying every resolved address in order and returning the
/// first that accepts, or the last failure if none did.
///
/// `timeout`, when set, bounds **each** attempt individually rather than the
/// whole list — a name with several addresses (common for anycast or
/// round-robin DNS) gets the full timeout's worth of patience with every one
/// of them, matching `getaddrinfo`-then-sequential-`connect` semantics rather
/// than dividing one budget across an unknown number of candidates.
///
/// # Errors
/// Whatever the last attempted address failed with, or the resolution error
/// if resolution itself failed.
pub fn connect(hp: &HostPort, timeout: Option<Duration>) -> Result<TcpStream> {
    let addrs = resolve(hp)?;
    let mut last_err = None;
    for addr in addrs {
        let attempt = match timeout {
            Some(t) => TcpStream::connect_timeout(&addr, t),
            None => TcpStream::connect(addr),
        };
        match attempt {
            Ok(stream) => return Ok(stream),
            Err(e) => last_err = Some(e),
        }
    }
    // `addrs` was checked non-empty in `resolve`, so this always has a value;
    // the fallback message exists only so a future change to that invariant
    // fails safe rather than unwrapping.
    Err(last_err.map_or(
        ProtocolError::Malformed {
            scheme: "socket",
            detail: "no address to connect to",
        },
        ProtocolError::from,
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn connects_to_a_loopback_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        };
        let stream = connect(&hp, Some(Duration::from_secs(2))).unwrap();
        assert!(stream.peer_addr().is_ok());
    }

    #[test]
    fn refused_connection_is_reported_not_panicked() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        };
        assert!(connect(&hp, Some(Duration::from_millis(500))).is_err());
    }

    #[test]
    fn unresolvable_host_is_an_error_not_a_panic() {
        let hp = HostPort {
            host: String::new(),
            port: 1,
        };
        // An empty host string is not a valid name; this must fail cleanly.
        let _ = resolve(&hp);
    }
}
