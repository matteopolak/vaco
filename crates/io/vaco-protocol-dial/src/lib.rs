//! Dial helpers shared by protocols that must complete a duplex round trip
//! before `vaco_protocol_core::Protocol::open`/`create` has anything to hand
//! back.
//!
//! Those traits return either a source or a sink, never both mid-flight, so
//! a protocol that has to write a request and read its reply — or run a
//! handshake — before it has a stream worth returning cannot get there
//! through `ProtocolRegistry::resolve`. It dials its own transport instead,
//! calling `ProtocolEnv::check_scheme` by hand exactly where the registry
//! would have. This crate holds that dial step, in both its plain-TCP and
//! TLS-over-TCP forms, plus the header-block reader several of those
//! protocols also need.

#![forbid(unsafe_code)]

use std::io::Read;
use std::net::TcpStream;
use std::time::Duration;

use vaco_protocol_core::{ProtocolEnv, ProtocolError, Result};
use vaco_protocol_socket::url::HostPort;
use vaco_protocol_tls::TlsOptions;
use vaco_protocol_tls::connect::TlsStream;

/// Connect a plain TCP transport, checking `"tcp"` against `env` first.
///
/// # Errors
/// [`ProtocolError::Denied`] if `"tcp"` is not permitted by `env`; otherwise
/// whatever the connection attempt failed with.
pub fn dial_tcp(
    hp: &HostPort,
    timeout: Option<Duration>,
    env: &ProtocolEnv<'_>,
) -> Result<TcpStream> {
    env.check_scheme("tcp")?;
    vaco_protocol_socket::addr::connect(hp, timeout)
}

/// Connect and TLS-handshake a transport, checking `"tls"` against `env`
/// first, then reusing [`vaco_protocol_tls::connect`] for the TCP leg (which
/// checks `"tcp"` itself) and the handshake.
///
/// # Errors
/// [`ProtocolError::Denied`] if `"tls"` or `"tcp"` is not permitted by
/// `env`; otherwise whatever the connection or handshake failed with.
pub fn dial_tls(
    hp: &HostPort,
    timeout: Option<Duration>,
    env: &ProtocolEnv<'_>,
    opts: &TlsOptions,
) -> Result<TlsStream> {
    env.check_scheme("tls")?;
    let tcp = vaco_protocol_tls::connect::connect_tcp(hp, timeout, env)?;
    vaco_protocol_tls::connect::handshake(hp, tcp, opts, None)
}

/// Bytes a header block may reasonably use before a peer counts as hostile
/// rather than slow.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Read one header block (whatever precedes it, ending at `\r\n\r\n`) a byte
/// at a time, so a buffered reader never strands application bytes the
/// caller hands `stream` back for afterward.
///
/// `scheme` and `eof_detail` let each caller's error name its own protocol
/// and describe what an early close means there.
///
/// # Errors
/// [`ProtocolError::Malformed`] if the block exceeds [`MAX_HEADER_BYTES`] or
/// the peer closes before the terminator; propagates I/O failure otherwise.
pub fn read_header_block<S: Read>(
    stream: &mut S,
    scheme: &'static str,
    eof_detail: &'static str,
) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if buf.len() >= MAX_HEADER_BYTES {
            return Err(ProtocolError::Malformed {
                scheme,
                detail: "header block exceeded the size limit",
            });
        }
        if stream.read(&mut byte)? == 0 {
            return Err(ProtocolError::Malformed {
                scheme,
                detail: eof_detail,
            });
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    Ok(buf)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use std::io::Cursor;

    use vaco_io::CancelToken;
    use vaco_protocol_core::ProtocolRegistry;

    use super::*;

    #[test]
    fn dial_tcp_denied_without_tcp_on_the_whitelist() {
        let listener = TcpStream::connect("127.0.0.1:1");
        assert!(
            listener.is_err(),
            "port 1 must stay closed for this test to mean anything"
        );

        let registry = ProtocolRegistry::new();
        let cancel = CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["other"]);
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: 1,
        };
        let err = dial_tcp(&hp, None, &env).unwrap_err();
        assert!(matches!(err, ProtocolError::Denied { .. }));
    }

    #[test]
    fn dial_tcp_succeeds_once_granted() {
        let bound = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = bound.local_addr().unwrap();
        let registry = ProtocolRegistry::new();
        let cancel = CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["tcp"]);
        let hp = HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        };
        assert!(dial_tcp(&hp, Some(Duration::from_secs(2)), &env).is_ok());
    }

    #[test]
    fn read_header_block_stops_at_the_terminator_and_keeps_trailing_bytes_unread() {
        let mut cursor = Cursor::new(b"A: 1\r\nB: 2\r\n\r\ntrailing".to_vec());
        let block = read_header_block(&mut cursor, "test", "closed early").unwrap();
        assert_eq!(block, b"A: 1\r\nB: 2\r\n\r\n");
        let mut rest = Vec::new();
        std::io::Read::read_to_end(&mut cursor, &mut rest).unwrap();
        assert_eq!(rest, b"trailing");
    }

    #[test]
    fn read_header_block_reports_the_callers_eof_detail() {
        let mut cursor = Cursor::new(b"no terminator here".to_vec());
        let err = read_header_block(&mut cursor, "test", "closed early").unwrap_err();
        assert!(matches!(
            err,
            ProtocolError::Malformed {
                scheme: "test",
                detail: "closed early"
            }
        ));
    }

    #[test]
    fn read_header_block_bounds_an_unterminated_stream() {
        let mut cursor = Cursor::new(vec![b'x'; MAX_HEADER_BYTES + 16]);
        let err = read_header_block(&mut cursor, "test", "closed early").unwrap_err();
        assert!(matches!(
            err,
            ProtocolError::Malformed {
                scheme: "test",
                detail: "header block exceeded the size limit"
            }
        ));
    }
}
