//! The `tcp:` protocol.
//!
//! # Why `tls:` does not nest through this module
//!
//! [`Protocol::open`] returns `Box<dyn MediaSource>` and [`Protocol::create`]
//! returns `Box<dyn MediaSink>` — one direction each, because
//! `vaco-protocol-core`'s trait was designed for the demux-only shape D5
//! describes (see that crate's docs). A TLS handshake needs both directions on
//! the *same* connection: send a `ClientHello`, read a `ServerHello`, and so
//! on. Measured against the reference (`ffmpeg -v debug`, D17): `tls.c`
//! genuinely opens its underlying transport as a nested `tcp:` URL (a whitelist
//! naming only `tls` refuses the connection with `Protocol 'tcp' not on
//! whitelist 'tls'!`, and naming both proceeds to the handshake — see
//! `docs/io/vaco-protocol-tls.md`), which works there because `URLContext` is
//! duplex in the C model.
//!
//! Rather than widen `vaco-protocol-core`'s trait for one caller (out of scope
//! for this crate, and reported instead — see this crate's final report),
//! `vaco-protocol-tls` connects its own [`std::net::TcpStream`] directly,
//! independent of the [`TcpProtocol`] registered here. It still calls
//! [`vaco_protocol_core::ProtocolEnv::check_scheme`] with `"tcp"` first, so the
//! whitelist property — a `tls:` open needs `tcp` granted, exactly as
//! measured — holds even though the bytes never pass through this module.
//! `docs/io/vaco-protocol-tls.md` has the full argument.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use socket2::{Domain, Socket, TcpKeepalive, Type};
use vaco_core::Error as CoreError;
use vaco_io::{MediaSink, MediaSource, PeekSource, RawSource, Seekability};
use vaco_opts::{Dict, OptionsExt, Schema, schema_of};
use vaco_protocol_core::{
    Access, IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result,
    Url,
};
use vaco_time::{Instant, sleep};

use crate::addr;
use crate::options::TcpOptions;
use crate::url::HostPort;

/// How often a bounded (`listen_timeout`-limited) accept loop polls. Small
/// enough that a real connection is picked up promptly, large enough not to
/// spin.
const ACCEPT_POLL: Duration = Duration::from_millis(10);

fn options(opts: &Dict) -> Result<TcpOptions> {
    let mut parsed = TcpOptions::default();
    parsed
        .apply_dict(opts)
        .map_err(|_| ProtocolError::Malformed {
            scheme: "tcp",
            detail: "bad option value",
        })?;
    Ok(parsed)
}

/// `-timeout` (microseconds, `-1` = block) falling back to
/// [`ProtocolEnv::rw_timeout`] when left at its default.
fn effective_timeout(opts: &TcpOptions, env_timeout: Option<Duration>) -> Option<Duration> {
    if opts.timeout >= 0 {
        Some(Duration::from_micros(u64::try_from(opts.timeout).unwrap_or(0)))
    } else {
        env_timeout
    }
}

/// `local_addr`/`local_port` as a bind target for `domain`, or `None` when
/// neither was set.
fn local_bind_addr(opts: &TcpOptions, domain: Domain) -> Result<Option<SocketAddr>> {
    if opts.local_addr.is_empty() && opts.local_port.is_empty() {
        return Ok(None);
    }
    let ip = if opts.local_addr.is_empty() {
        if domain == Domain::IPV6 {
            std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
        } else {
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        }
    } else {
        opts.local_addr
            .parse()
            .map_err(|_| ProtocolError::Malformed {
                scheme: "tcp",
                detail: "local_addr is not a valid IP address",
            })?
    };
    let port: u16 = if opts.local_port.is_empty() {
        0
    } else {
        opts.local_port
            .parse()
            .map_err(|_| ProtocolError::Malformed {
                scheme: "tcp",
                detail: "local_port is not a valid port number",
            })?
    };
    Ok(Some(SocketAddr::new(ip, port)))
}

fn apply_connected_options(socket: &Socket, opts: &TcpOptions) -> Result<()> {
    if opts.tcp_nodelay {
        socket.set_nodelay(true).map_err(ProtocolError::from)?;
    }
    if opts.tcp_keepalive {
        socket
            .set_tcp_keepalive(&TcpKeepalive::new())
            .map_err(ProtocolError::from)?;
    }
    if opts.send_buffer_size >= 0 {
        socket
            .set_send_buffer_size(usize::try_from(opts.send_buffer_size).unwrap_or(0))
            .map_err(ProtocolError::from)?;
    }
    if opts.recv_buffer_size >= 0 {
        socket
            .set_recv_buffer_size(usize::try_from(opts.recv_buffer_size).unwrap_or(0))
            .map_err(ProtocolError::from)?;
    }
    // `tcp_mss` is deliberately not applied here; see the crate docs.
    Ok(())
}

/// Connect to `hp`, applying every pre- and post-connect option this crate
/// implements, trying each resolved address in turn.
///
/// # Errors
/// The last connection attempt's failure, or a malformed-option error.
pub fn connect(hp: &HostPort, opts: &TcpOptions, timeout: Option<Duration>) -> Result<TcpStream> {
    let addrs = addr::resolve(hp)?;
    let mut last_err: Option<ProtocolError> = None;
    for sockaddr in addrs {
        let domain = Domain::for_address(sockaddr);
        let socket =
            Socket::new(domain, Type::STREAM, Some(socket2::Protocol::TCP)).map_err(ProtocolError::from)?;
        if let Some(local) = local_bind_addr(opts, domain)?
            && let Err(e) = socket.bind(&local.into())
        {
            last_err = Some(ProtocolError::from(e));
            continue;
        }
        let connected = match timeout {
            Some(t) => socket.connect_timeout(&sockaddr.into(), t),
            None => socket.connect(&sockaddr.into()),
        };
        match connected {
            Ok(()) => {
                apply_connected_options(&socket, opts)?;
                return Ok(socket.into());
            }
            Err(e) => last_err = Some(ProtocolError::from(e)),
        }
    }
    Err(last_err.unwrap_or(ProtocolError::Malformed {
        scheme: "tcp",
        detail: "no address to connect to",
    }))
}

/// Bind to `hp` and accept exactly one connection, waiting up to
/// `listen_timeout` (`None` = forever).
///
/// # Errors
/// The bind failure, or [`ProtocolError::Io`] wrapping a timed-out wait
/// (reported as [`std::io::ErrorKind::TimedOut`]).
pub fn listen_accept(
    hp: &HostPort,
    opts: &TcpOptions,
    listen_timeout: Option<Duration>,
) -> Result<TcpStream> {
    let bind_addr = SocketAddr::new(
        if hp.host.is_empty() {
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        } else {
            hp.host.parse().map_err(|_| ProtocolError::Malformed {
                scheme: "tcp",
                detail: "listen address is not a valid IP address",
            })?
        },
        hp.port,
    );
    let listener = TcpListener::bind(bind_addr).map_err(ProtocolError::from)?;
    listener.set_nonblocking(true).map_err(ProtocolError::from)?;

    let deadline = listen_timeout.map(|t| Instant::now().saturating_add(t));
    // Bounded by iteration count too, per `vaco-time`'s own guidance: on a
    // target with a stopped clock (no monotonic source), `now() < deadline`
    // would never become false and this would spin forever.
    let max_polls = listen_timeout.map_or(usize::MAX, |t| {
        t.as_nanos()
            .div_ceil(ACCEPT_POLL.as_nanos().max(1))
            .try_into()
            .unwrap_or(usize::MAX)
    });

    for _ in 0..=max_polls {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).map_err(ProtocolError::from)?;
                let socket = Socket::from(stream);
                apply_connected_options(&socket, opts)?;
                return Ok(socket.into());
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
        "listen_timeout elapsed with no incoming connection",
    )))
}

/// A connected TCP socket, read side.
#[derive(Debug)]
pub struct TcpSource {
    stream: TcpStream,
    pos: u64,
}

impl TcpSource {
    #[must_use]
    pub const fn new(stream: TcpStream) -> Self {
        Self { stream, pos: 0 }
    }
}

impl RawSource for TcpSource {
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

/// A connected TCP socket, write side.
#[derive(Debug)]
pub struct TcpSink {
    stream: TcpStream,
    pos: u64,
}

impl TcpSink {
    #[must_use]
    pub const fn new(stream: TcpStream) -> Self {
        Self { stream, pos: 0 }
    }
}

impl MediaSink for TcpSink {
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

fn host_port(url: &Url) -> Result<HostPort> {
    crate::url::parse(&url.rest)
        .map(|(hp, _)| hp)
        .ok_or(ProtocolError::Malformed {
            scheme: "tcp",
            detail: "expected host:port",
        })
}

/// The `tcp:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct TcpProtocol;

impl Protocol for TcpProtocol {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts_dict: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let opts = options(opts_dict)?;
        let hp = host_port(url)?;
        let timeout = effective_timeout(&opts, env.rw_timeout);
        let stream = if flags.listen || opts.listen > 0 {
            let listen_timeout = (opts.listen_timeout >= 0)
                .then(|| Duration::from_millis(u64::try_from(opts.listen_timeout).unwrap_or(0)));
            listen_accept(&hp, &opts, listen_timeout)?
        } else {
            connect(&hp, &opts, timeout)?
        };
        if let Some(t) = timeout {
            stream.set_read_timeout(Some(t)).map_err(ProtocolError::from)?;
        }
        Ok(Box::new(PeekSource::new(TcpSource::new(stream))))
    }

    fn create(
        &self,
        url: &Url,
        flags: IoFlags,
        opts_dict: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let opts = options(opts_dict)?;
        let hp = host_port(url)?;
        let timeout = effective_timeout(&opts, env.rw_timeout);
        let stream = if flags.listen || opts.listen > 0 {
            let listen_timeout = (opts.listen_timeout >= 0)
                .then(|| Duration::from_millis(u64::try_from(opts.listen_timeout).unwrap_or(0)));
            listen_accept(&hp, &opts, listen_timeout)?
        } else {
            connect(&hp, &opts, timeout)?
        };
        if let Some(t) = timeout {
            stream.set_write_timeout(Some(t)).map_err(ProtocolError::from)?;
        }
        Ok(Box::new(TcpSink::new(stream)))
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

/// The registry entry for `tcp:`.
pub static TCP_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "tcp",
    long_name: "TCP",
    flags: ProtocolFlags {
        network: true,
        nested_scheme: false,
        server_capable: true,
        readable: true,
        writable: true,
    },
    // `tcp:` opens nothing nested — measured: `ffmpeg -v debug -i tcp://…`
    // logs "No default whitelist set" for `tcp`. See the crate docs.
    default_whitelist: &[],
    options: Some(tcp_schema),
    proto: &TcpProtocol,
};

fn tcp_schema() -> &'static Schema {
    schema_of::<TcpOptions>()
}
