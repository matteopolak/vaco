//! [`GopherProtocol`]/[`GophersProtocol`] — the `gopher:`/`gophers:`
//! schemes, and the registry entries.

use std::io::{Read, Write};
use std::net::TcpStream;

use vaco_io::{MediaSink, MediaSource, PeekSource, ReaderSource, WriterSink};
use vaco_opts::Dict;
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};
use vaco_protocol_socket::url::HostPort;
use vaco_protocol_tls::TlsOptions;
use vaco_protocol_tls::connect::TlsStream;

use crate::selector;

/// Default gopher port (RFC 1436).
const DEFAULT_PORT: u16 = 70;

fn parse_host_port(authority: &str) -> HostPort {
    match authority.rsplit_once(':') {
        Some((h, p)) => HostPort {
            host: h.to_owned(),
            port: p.parse().unwrap_or(DEFAULT_PORT),
        },
        None => HostPort {
            host: authority.to_owned(),
            port: DEFAULT_PORT,
        },
    }
}

/// Send `<selector>\r\n` for `path`, per [`selector::parse`]'s measured
/// algorithm, after validating the type character.
///
/// # Errors
/// [`ProtocolError::Malformed`] if `path` names no type character at all;
/// wraps [`vaco_core::Error::Option`] (via `crate::selector::check_type`) for
/// an unsupported type; propagates I/O failure otherwise.
fn send_selector<S: Write>(stream: &mut S, path: &str) -> Result<()> {
    let (ty, sel) = selector::parse(path).ok_or(ProtocolError::Malformed {
        scheme: "gopher",
        detail: "expected gopher://host[:port]/<type><selector>",
    })?;
    selector::check_type(ty).map_err(ProtocolError::Io)?;
    stream.write_all(sel.as_bytes())?;
    stream.write_all(b"\r\n")?;
    Ok(())
}

fn open_generic<S: Read + Write + Send + 'static>(
    mut stream: S,
    path: &str,
) -> Result<Box<dyn MediaSource>> {
    send_selector(&mut stream, path)?;
    Ok(Box::new(PeekSource::new(ReaderSource::new(stream))))
}

fn create_generic<S: Write + Send + 'static>(mut stream: S, path: &str) -> Result<Box<dyn MediaSink>> {
    send_selector(&mut stream, path)?;
    Ok(Box::new(WriterSink::new(stream)))
}

/// Dial the raw TCP transport `gopher:` uses, applying the whitelist check
/// by hand — the selector round trip is inherently duplex (write, then
/// treat the connection as one direction), which
/// `vaco_protocol_core::Protocol::open`/`create`'s one-direction-each shape
/// cannot express, the same reasoning as `vaco-protocol-tls`/
/// `-httpproxy`/`-ftp` in this workspace.
fn dial_tcp(hp: &HostPort, env: &ProtocolEnv<'_>) -> Result<TcpStream> {
    env.check_scheme("tcp")?;
    vaco_protocol_socket::addr::connect(hp, None)
}

/// Dial and TLS-handshake the transport `gophers:` uses, reusing
/// `vaco-protocol-tls`'s own connect/handshake rather than duplicating TLS
/// handling — see that crate's docs for the handshake itself.
fn dial_tls(hp: &HostPort, env: &ProtocolEnv<'_>) -> Result<TlsStream> {
    env.check_scheme("tls")?;
    let tcp = vaco_protocol_tls::connect::connect_tcp(hp, None, env)?;
    let opts = TlsOptions::default();
    vaco_protocol_tls::connect::handshake(hp, tcp, &opts, None)
}

/// The `gopher:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct GopherProtocol;

impl Protocol for GopherProtocol {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let (authority, path) = selector::split_authority(&url.rest);
        let hp = parse_host_port(authority);
        let stream = dial_tcp(&hp, env)?;
        open_generic(stream, path)
    }

    fn create(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let (authority, path) = selector::split_authority(&url.rest);
        let hp = parse_host_port(authority);
        let stream = dial_tcp(&hp, env)?;
        create_generic(stream, path)
    }
}

/// The `gophers:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct GophersProtocol;

impl Protocol for GophersProtocol {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let (authority, path) = selector::split_authority(&url.rest);
        let hp = parse_host_port(authority);
        let stream = dial_tls(&hp, env)?;
        open_generic(stream, path)
    }

    fn create(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let (authority, path) = selector::split_authority(&url.rest);
        let hp = parse_host_port(authority);
        let stream = dial_tls(&hp, env)?;
        create_generic(stream, path)
    }
}

/// The registry entry for `gopher:`.
pub static GOPHER_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "gopher",
    long_name: "Gopher (RFC 1436)",
    // `-protocols` lists `gopher` under both `Input:` and `Output:`.
    flags: ProtocolFlags {
        network: true,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: true,
    },
    // Measured non-empty — the first protocol found so far in this
    // workspace where that is true. `ffmpeg -v debug`:
    // "[gopher @ ...] Setting default whitelist 'gopher,tcp'". See the
    // crate docs for the full transcript and why this makes sense here
    // (a gopher menu links to further gopher/plain resources).
    default_whitelist: &["gopher", "tcp"],
    // `-h protocol=gopher` reports "Unknown protocol": no private
    // AVOptions.
    options: None,
    proto: &GopherProtocol,
};

/// The registry entry for `gophers:`.
pub static GOPHERS_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "gophers",
    long_name: "Gopher over TLS",
    flags: ProtocolFlags {
        network: true,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: true,
    },
    // Measured: "[gophers @ ...] Setting default whitelist
    // 'gopher,gophers,tcp,tls'".
    default_whitelist: &["gopher", "gophers", "tcp", "tls"],
    options: None,
    proto: &GophersProtocol,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    #[test]
    fn gopher_default_whitelist_matches_measurement() {
        assert_eq!(GOPHER_PROTOCOL.default_whitelist, &["gopher", "tcp"]);
    }

    #[test]
    fn gophers_default_whitelist_matches_measurement() {
        assert_eq!(
            GOPHERS_PROTOCOL.default_whitelist,
            &["gopher", "gophers", "tcp", "tls"]
        );
    }

    #[test]
    fn neither_protocol_has_an_option_schema() {
        assert!(GOPHER_PROTOCOL.options.is_none());
        assert!(GOPHERS_PROTOCOL.options.is_none());
    }

    #[test]
    fn parse_host_port_defaults_to_70() {
        let hp = parse_host_port("example.com");
        assert_eq!(hp.port, 70);
        let hp2 = parse_host_port("example.com:7070");
        assert_eq!(hp2.port, 7070);
    }
}
