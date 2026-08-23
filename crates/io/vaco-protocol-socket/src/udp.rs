//! The `udp:` and `udplite:` protocols.
//!
//! One implementation for both — [`UdpProtocol`] is parameterised by
//! `lite: bool`, which only decides the wire protocol number
//! (`socket2::Protocol::UDP` vs `UDPLITE`) a new socket is created with.
//! Every other behaviour (options, framing, multicast) is identical, which
//! matches the reference: `ffmpeg -h protocol=udp` and `-h protocol=udplite`
//! print the same option table (see `crate::options::UdpOptions`'s docs).
//!
//! # Framing
//!
//! One [`vaco_io::RawSource::read`] call is one `recv`/`recv_from`: UDP has no
//! stream to reassemble, so a demuxer's read buffer size already *is* the
//! packet boundary. A datagram larger than the caller's buffer is truncated by
//! the kernel exactly as a raw `recvfrom` would truncate it — this crate adds
//! no additional buffering that could either merge or further split a
//! datagram.
//!
//! # Why setup goes through `socket2` but I/O does not
//!
//! `socket2::Socket::recv`/`recv_from` take `&mut [MaybeUninit<u8>]`, and
//! turning a `&mut [u8]` into that safely needs the exact pointer cast
//! `socket2`'s own `Read` impl performs — inside `socket2`, which is allowed
//! its own `unsafe`; this crate is `#![forbid(unsafe_code)]` with no
//! exception. So every socket here is *built* with `socket2` (for the options
//! `std::net` has no accessor for: buffer sizes, multicast group membership,
//! TTL, TOS, `SO_REUSEADDR`) and then converted with `Into<std::net::UdpSocket>`
//! before a single byte is sent or received, so all I/O uses `std::net`'s own
//! safe, plain-`&mut [u8]` `recv`/`recv_from`/`send`/`send_to`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use socket2::{Domain, Socket, Type};
use vaco_core::Error as CoreError;
use vaco_io::{MediaSink, MediaSource, PeekSource, RawSource, Seekability};
use vaco_opts::{Dict, OptionsExt, Schema, schema_of};
use vaco_protocol_core::{
    Access, IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result,
    Url,
};

use crate::addr;
use crate::options::UdpOptions;
use crate::url::HostPort;

fn options(opts: &Dict, scheme: &'static str) -> Result<UdpOptions> {
    let mut parsed = UdpOptions::default();
    parsed
        .apply_dict(opts)
        .map_err(|_| ProtocolError::Malformed {
            scheme,
            detail: "bad option value",
        })?;
    Ok(parsed)
}

fn host_port(url: &Url, scheme: &'static str) -> Result<HostPort> {
    crate::url::parse(&url.rest)
        .map(|(hp, _)| hp)
        .ok_or(ProtocolError::Malformed {
            scheme,
            detail: "expected host:port",
        })
}

fn unspecified(domain: Domain) -> IpAddr {
    if domain == Domain::IPV6 {
        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    }
}

fn local_addr(opts: &UdpOptions, domain: Domain) -> Result<SocketAddr, ProtocolError> {
    let ip = if opts.localaddr.is_empty() {
        unspecified(domain)
    } else {
        opts.localaddr
            .parse()
            .map_err(|_| ProtocolError::Malformed {
                scheme: "udp",
                detail: "localaddr is not a valid IP address",
            })?
    };
    let port = if opts.localport < 0 {
        0
    } else {
        u16::try_from(opts.localport).unwrap_or(0)
    };
    Ok(SocketAddr::new(ip, port))
}

fn apply_common_options(socket: &Socket, opts: &UdpOptions) -> Result<()> {
    if opts.reuse_address() {
        socket.set_reuse_address(true).map_err(ProtocolError::from)?;
    }
    if opts.buffer_size >= 0 {
        let size = usize::try_from(opts.buffer_size).unwrap_or(0);
        socket.set_send_buffer_size(size).map_err(ProtocolError::from)?;
        socket.set_recv_buffer_size(size).map_err(ProtocolError::from)?;
    }
    if opts.broadcast {
        socket.set_broadcast(true).map_err(ProtocolError::from)?;
    }
    #[cfg(unix)]
    if opts.dscp >= 0 {
        // IP_TOS carries the DSCP value in its top six bits.
        let tos = u32::try_from(opts.dscp).unwrap_or(0) << 2;
        socket.set_tos(tos).map_err(ProtocolError::from)?;
    }
    Ok(())
}

/// Join the multicast group named by `group`, if it is one. A no-op for a
/// unicast address, so callers can call this unconditionally.
fn maybe_join_multicast(socket: &Socket, group: IpAddr) -> Result<()> {
    if !group.is_multicast() {
        return Ok(());
    }
    match group {
        IpAddr::V4(g) => socket
            .join_multicast_v4(&g, &Ipv4Addr::UNSPECIFIED)
            .map_err(ProtocolError::from),
        IpAddr::V6(g) => socket
            .join_multicast_v6(&g, 0)
            .map_err(ProtocolError::from),
    }
}

/// `IPPROTO_UDPLITE`, IANA-assigned as 136. `socket2::Protocol::UDPLITE` only
/// exists on the handful of `target_os`es that define the OS constant
/// (Linux, Android, FreeBSD, Fuchsia — measured: it does not exist on this
/// crate's own macOS development target, an `E0599` at first `cargo check`).
/// Using the numeric assignment directly, via `Protocol`'s `From<c_int>`,
/// keeps this crate portable to every `unix`-family build target; a kernel
/// that genuinely lacks UDP-Lite support (macOS has none — Apple's TCP/IP
/// stack never implemented RFC 3828) reports that at `socket()`/`connect()`
/// time as an ordinary `EPROTONOSUPPORT`-shaped [`ProtocolError::Io`], not a
/// panic and not a silent fallback to plain UDP.
const IPPROTO_UDPLITE: i32 = 136;

fn new_socket(domain: Domain, lite: bool) -> Result<Socket> {
    let proto = if lite {
        socket2::Protocol::from(IPPROTO_UDPLITE)
    } else {
        socket2::Protocol::UDP
    };
    Socket::new(domain, Type::DGRAM, Some(proto)).map_err(ProtocolError::from)
}

/// Bind for reading: binds to `hp` directly (joining its multicast group if
/// it names one), matching the reference's own approach of binding the
/// receiving socket to the address named in the URL rather than always
/// binding `0.0.0.0`.
fn bind_for_read(hp: &HostPort, opts: &UdpOptions, lite: bool) -> Result<UdpSocket> {
    let addrs = addr::resolve(hp)?;
    let target = addrs
        .into_iter()
        .next()
        .ok_or(ProtocolError::Malformed {
            scheme: "udp",
            detail: "host name resolved to no addresses",
        })?;
    let domain = Domain::for_address(target);
    let socket = new_socket(domain, lite)?;
    apply_common_options(&socket, opts)?;
    socket.bind(&target.into()).map_err(ProtocolError::from)?;
    maybe_join_multicast(&socket, target.ip())?;
    if opts.connect {
        socket.connect(&target.into()).map_err(ProtocolError::from)?;
    }
    Ok(socket.into())
}

/// Bind (to `localaddr`/`localport`, or unspecified) then connect for
/// writing: the reference always treats `create()`'s destination as the peer
/// to send to.
fn bind_for_write(hp: &HostPort, opts: &UdpOptions, lite: bool) -> Result<UdpSocket> {
    let addrs = addr::resolve(hp)?;
    let target = addrs
        .into_iter()
        .next()
        .ok_or(ProtocolError::Malformed {
            scheme: "udp",
            detail: "host name resolved to no addresses",
        })?;
    let domain = Domain::for_address(target);
    let socket = new_socket(domain, lite)?;
    apply_common_options(&socket, opts)?;
    socket
        .bind(&local_addr(opts, domain)?.into())
        .map_err(ProtocolError::from)?;
    if let IpAddr::V4(g) = target.ip()
        && g.is_multicast()
    {
        socket.set_multicast_ttl_v4(u32::try_from(opts.ttl).unwrap_or(16))
            .map_err(ProtocolError::from)?;
    }
    socket.connect(&target.into()).map_err(ProtocolError::from)?;
    Ok(socket.into())
}

fn apply_timeout(socket: &UdpSocket, opts: &UdpOptions) -> Result<()> {
    if opts.timeout > 0 {
        let d = Duration::from_micros(u64::try_from(opts.timeout).unwrap_or(0));
        socket.set_read_timeout(Some(d)).map_err(ProtocolError::from)?;
    }
    Ok(())
}

/// A UDP/UDP-Lite socket, read side.
#[derive(Debug)]
pub struct UdpSource {
    socket: UdpSocket,
    pos: u64,
}

impl UdpSource {
    #[must_use]
    pub const fn new(socket: UdpSocket) -> Self {
        Self { socket, pos: 0 }
    }
}

impl RawSource for UdpSource {
    fn read(&mut self, buf: &mut [u8]) -> vaco_core::Result<usize> {
        loop {
            // `recv` works whether or not the socket was `connect()`ed to a
            // single peer (the common case here, since `bind_for_read` always
            // binds); a socket that additionally called `connect()` (`-connect
            // 1`) restricts *which* peer's datagrams arrive at all, which
            // `recv` alone cannot express, so this uses `recv` either way and
            // relies on the kernel to have already filtered by the earlier
            // `connect()` call when one was made.
            return match self.socket.recv(buf) {
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

/// A UDP/UDP-Lite socket, write side.
#[derive(Debug)]
pub struct UdpSink {
    socket: UdpSocket,
    pos: u64,
}

impl UdpSink {
    #[must_use]
    pub const fn new(socket: UdpSocket) -> Self {
        Self { socket, pos: 0 }
    }
}

impl MediaSink for UdpSink {
    fn write(&mut self, buf: &[u8]) -> vaco_core::Result<()> {
        self.socket.send(buf)?;
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
        Ok(())
    }
}

/// The `udp:`/`udplite:` protocol. `lite` selects the wire protocol number a
/// new socket is created with; nothing else differs.
#[derive(Debug, Clone, Copy)]
pub struct UdpProtocol {
    lite: bool,
}

impl UdpProtocol {
    const fn scheme(self) -> &'static str {
        if self.lite { "udplite" } else { "udp" }
    }
}

impl Protocol for UdpProtocol {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        opts_dict: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let opts = options(opts_dict, self.scheme())?;
        let hp = host_port(url, self.scheme())?;
        let socket = bind_for_read(&hp, &opts, self.lite)?;
        apply_timeout(&socket, &opts)?;
        Ok(Box::new(PeekSource::new(UdpSource::new(socket))))
    }

    fn create(
        &self,
        url: &Url,
        _flags: IoFlags,
        opts_dict: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let opts = options(opts_dict, self.scheme())?;
        let hp = host_port(url, self.scheme())?;
        let socket = bind_for_write(&hp, &opts, self.lite)?;
        Ok(Box::new(UdpSink::new(socket)))
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

/// The registry entry for `udp:`.
pub static UDP_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "udp",
    long_name: "UDP",
    flags: ProtocolFlags {
        network: true,
        nested_scheme: false,
        server_capable: false,
        readable: true,
        writable: true,
    },
    // Measured (matching `tcp:`'s own): opens nothing nested.
    default_whitelist: &[],
    options: Some(udp_schema),
    proto: &UdpProtocol { lite: false },
};

/// The registry entry for `udplite:`.
pub static UDPLITE_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "udplite",
    long_name: "UDP-Lite",
    flags: ProtocolFlags {
        network: true,
        nested_scheme: false,
        server_capable: false,
        readable: true,
        writable: true,
    },
    default_whitelist: &[],
    options: Some(udp_schema),
    proto: &UdpProtocol { lite: true },
};

fn udp_schema() -> &'static Schema {
    schema_of::<UdpOptions>()
}
