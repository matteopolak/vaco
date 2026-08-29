//! [`DtlsProtocol`]: the `Protocol::open`/`create` entry points.

use std::io::{Read, Write};

use vaco_core::Error as CoreError;
use vaco_io::{MediaSink, MediaSource, PeekSource, RawSource, Seekability};
use vaco_opts::{Dict, OptionsExt, Schema, schema_of};
use vaco_protocol_core::{
    Access, IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result,
    Url,
};

use crate::connect::{self, DtlsStream};
use crate::listen;
use crate::options::DtlsOptions;

fn options(opts: &Dict) -> Result<DtlsOptions> {
    let mut parsed = DtlsOptions::default();
    parsed
        .apply_dict(opts)
        .map_err(|_| ProtocolError::Malformed {
            scheme: "dtls",
            detail: "bad option value",
        })?;
    Ok(parsed)
}

/// A local file's contents, read directly from the filesystem.
///
/// **Deliberately not routed through `env`/the `file:` protocol's
/// whitelist** — same security property as `vaco-protocol-tls`'s
/// `read_ca_file`: `cert_file`/`key_file`/`ca_file` only ever come from the
/// trusted `-opt`/`Dict` surface, never from a URL's own query string
/// ([`connect::host_port`] discards whatever
/// `vaco_protocol_socket::url::parse` recovered as inline `?key=value`
/// options). That is what stops a hostile playlist entry shaped like
/// `dtls://evil.example:443?cert_file=/etc/passwd` from ever reaching this
/// function.
///
/// # Errors
/// [`ProtocolError::Io`] if the path cannot be read.
fn read_file(path: &str) -> Result<String> {
    std::fs::read_to_string(path).map_err(ProtocolError::from)
}

fn cert_file_pem(opts: &DtlsOptions) -> Result<Option<String>> {
    if opts.cert_file.is_empty() {
        Ok(None)
    } else {
        Ok(Some(read_file(&opts.cert_file)?))
    }
}

fn key_file_pem(opts: &DtlsOptions) -> Result<Option<String>> {
    if opts.key_file.is_empty() {
        Ok(None)
    } else {
        Ok(Some(read_file(&opts.key_file)?))
    }
}

fn ca_file_pem(opts: &DtlsOptions) -> Result<Option<String>> {
    if opts.ca_file.is_empty() {
        Ok(None)
    } else {
        Ok(Some(read_file(&opts.ca_file)?))
    }
}

/// A DTLS connection, read side.
#[derive(Debug)]
pub struct DtlsSource {
    stream: DtlsStream,
    pos: u64,
}

impl DtlsSource {
    #[must_use]
    pub const fn new(stream: DtlsStream) -> Self {
        Self { stream, pos: 0 }
    }
}

impl RawSource for DtlsSource {
    fn read(&mut self, buf: &mut [u8]) -> vaco_core::Result<usize> {
        loop {
            return match self.stream.read(buf) {
                Ok(n) => {
                    self.pos = self.pos.saturating_add(n as u64);
                    Ok(n)
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => Err(CoreError::from(e)),
            };
        }
    }

    fn seekability(&self) -> Seekability {
        Seekability::None
    }
}

/// A DTLS connection, write side.
#[derive(Debug)]
pub struct DtlsSink {
    stream: DtlsStream,
    pos: u64,
}

impl DtlsSink {
    #[must_use]
    pub const fn new(stream: DtlsStream) -> Self {
        Self { stream, pos: 0 }
    }
}

impl MediaSink for DtlsSink {
    fn write(&mut self, buf: &[u8]) -> vaco_core::Result<()> {
        self.stream.write_all(buf)?;
        self.pos = self.pos.saturating_add(buf.len() as u64);
        Ok(())
    }

    fn seek(&mut self, _pos: u64) -> vaco_core::Result<u64> {
        Err(CoreError::NotSeekable)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn is_seekable(&self) -> bool {
        false
    }

    fn flush(&mut self) -> vaco_core::Result<()> {
        self.stream.flush()?;
        Ok(())
    }
}

/// The `dtls:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct DtlsProtocol;

impl DtlsProtocol {
    fn dial(
        url: &Url,
        flags: IoFlags,
        opts: &DtlsOptions,
        env: &ProtocolEnv<'_>,
    ) -> Result<DtlsStream> {
        let hp = connect::host_port(&url.rest)?;
        let cert_pem = cert_file_pem(opts)?;
        let key_pem = key_file_pem(opts)?;
        let ca_pem = ca_file_pem(opts)?;
        if flags.listen || opts.listen > 0 {
            let socket = listen::bind_accept(&hp, env.rw_timeout)?;
            listen::handshake(socket, opts, cert_pem.as_deref(), key_pem.as_deref(), ca_pem.as_deref())
        } else {
            let socket = connect::connect_udp(&hp, env.rw_timeout, env)?;
            connect::handshake(socket, opts, cert_pem.as_deref(), key_pem.as_deref(), ca_pem.as_deref())
        }
    }
}

impl Protocol for DtlsProtocol {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts_dict: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let opts = options(opts_dict)?;
        let stream = Self::dial(url, flags, &opts, env)?;
        Ok(Box::new(PeekSource::new(DtlsSource::new(stream))))
    }

    fn create(
        &self,
        url: &Url,
        flags: IoFlags,
        opts_dict: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let opts = options(opts_dict)?;
        let stream = Self::dial(url, flags, &opts, env)?;
        Ok(Box::new(DtlsSink::new(stream)))
    }

    fn check(&self, url: &Url, env: &ProtocolEnv<'_>) -> Result<Access> {
        match self.open(url, IoFlags::READ, &Dict::new(), env) {
            Ok(_) => Ok(Access {
                read: true,
                write: true,
            }),
            Err(_) => Ok(Access::default()),
        }
    }
}

/// The registry entry for `dtls:`.
pub static DTLS_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "dtls",
    long_name: "DTLS",
    flags: ProtocolFlags {
        network: true,
        // `dtls:` opens a nested UDP connection, but not through the
        // registry — see `crate::connect`'s module docs, same argument as
        // `vaco-protocol-tls`'s `tls:`/`tcp:` relationship.
        nested_scheme: true,
        server_capable: true,
        readable: true,
        writable: true,
    },
    // No probe was needed to justify `&[]` here beyond what `tls:` already
    // established for the same shape of protocol (a transport that opens a
    // nested connection of its own): every such protocol in this workspace
    // grants nothing by default (`vaco-protocol-wrap`'s crate docs has the
    // general argument; `hls:`'s curated grant is the one deliberate
    // exception). A caller needs `udp` granted alongside `dtls` explicitly.
    default_whitelist: &[],
    options: Some(dtls_schema),
    proto: &DtlsProtocol,
};

fn dtls_schema() -> &'static Schema {
    schema_of::<DtlsOptions>()
}
