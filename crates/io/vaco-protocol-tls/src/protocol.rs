//! [`TlsProtocol`]: the `Protocol::open`/`create` entry points.

use std::io::{Read, Write};

use vaco_core::Error as CoreError;
use vaco_io::{MediaSink, MediaSource, PeekSource, RawSource, Seekability};
use vaco_opts::{Dict, OptionsExt, Schema, schema_of};
use vaco_protocol_core::{
    Access, IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};

use crate::connect::{self, TlsStream};
use crate::options::TlsOptions;

fn options(opts: &Dict) -> Result<TlsOptions> {
    let mut parsed = TlsOptions::default();
    parsed
        .apply_dict(opts)
        .map_err(|_| ProtocolError::Malformed {
            scheme: "tls",
            detail: "bad option value",
        })?;
    Ok(parsed)
}

/// `-ca_file`'s contents, read directly from the local filesystem.
///
/// **Deliberately not routed through `env`/the `file:` protocol's whitelist**
/// — `ca_file` only ever comes from the trusted `-opt`/`Dict` surface a local
/// caller controls (the CLI, or an embedder's own code), never from a URL's
/// own query string: [`connect::host_port`] discards whatever
/// `vaco_protocol_socket::url::parse` recovered as inline `?key=value`
/// options, using only the `host:port` pair from it. That is what stops a
/// hostile playlist entry shaped like `tls://evil.example:443?ca_file=/etc/passwd`
/// from ever reaching this function — the query portion of a `tls:` URL is
/// simply never consulted for options at all.
///
/// # Errors
/// [`ProtocolError::Io`] if the path cannot be read.
fn read_ca_file(path: &str) -> Result<String> {
    std::fs::read_to_string(path).map_err(ProtocolError::from)
}

/// A TLS connection, read side.
#[derive(Debug)]
pub struct TlsSource {
    stream: TlsStream,
    pos: u64,
}

impl TlsSource {
    #[must_use]
    pub const fn new(stream: TlsStream) -> Self {
        Self { stream, pos: 0 }
    }
}

impl RawSource for TlsSource {
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

/// A TLS connection, write side.
#[derive(Debug)]
pub struct TlsSink {
    stream: TlsStream,
    pos: u64,
}

impl TlsSink {
    #[must_use]
    pub const fn new(stream: TlsStream) -> Self {
        Self { stream, pos: 0 }
    }
}

impl MediaSink for TlsSink {
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

/// The `tls:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct TlsProtocol;

impl TlsProtocol {
    fn dial(url: &Url, opts: &TlsOptions, env: &ProtocolEnv<'_>) -> Result<TlsStream> {
        let hp = connect::host_port(&url.rest)?;
        let tcp = connect::connect_tcp(&hp, env.rw_timeout, env)?;
        let ca_pem = if opts.ca_file.is_empty() {
            None
        } else {
            Some(read_ca_file(&opts.ca_file)?)
        };
        connect::handshake(&hp, tcp, opts, ca_pem.as_deref())
    }
}

impl Protocol for TlsProtocol {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        opts_dict: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let opts = options(opts_dict)?;
        let stream = Self::dial(url, &opts, env)?;
        Ok(Box::new(PeekSource::new(TlsSource::new(stream))))
    }

    fn create(
        &self,
        url: &Url,
        _flags: IoFlags,
        opts_dict: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let opts = options(opts_dict)?;
        let stream = Self::dial(url, &opts, env)?;
        Ok(Box::new(TlsSink::new(stream)))
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

/// The registry entry for `tls:`.
pub static TLS_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "tls",
    long_name: "TLS",
    flags: ProtocolFlags {
        network: true,
        // `tls:` opens a nested TCP connection, but not through the
        // registry — see `crate::connect`'s module docs. `nested_scheme` is
        // still `true`: it describes whether this protocol's own
        // `default_whitelist` is a meaningful grant to *something* it opens,
        // which is true here even though the "something" is not itself a
        // `Protocol::open` call.
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: true,
    },
    // Measured: `ffmpeg -v debug -i tls://…` logs "No default whitelist set"
    // — see the crate docs for the full transcript, including the whitelist
    // refusal this produces when only "tls" (not "tcp") is granted.
    default_whitelist: &[],
    options: Some(tls_schema),
    proto: &TlsProtocol,
};

fn tls_schema() -> &'static Schema {
    schema_of::<TlsOptions>()
}
