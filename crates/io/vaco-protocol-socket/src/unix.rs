//! The `unix:` protocol.
//!
//! `AF_UNIX` sockets exist only on `unix`-family targets, so the real
//! implementation ([`native`]) is `#[cfg(unix)]`-only. Rather than leave the
//! whole module — and therefore `UNIX_PROTOCOL`, which
//! `vaco-component.toml`'s generated registry row references unconditionally
//! (see `xtask/src/registry.rs`'s `ctor_item_exists` check, which runs on
//! whatever platform builds this crate) — absent on every other target, a
//! small `#[cfg(not(unix))]` fallback registers the same scheme and reports
//! [`vaco_protocol_core::ProtocolError::Unsupported`] at open time. This
//! mirrors the reference's own build: `unix:` is a name every `ffmpeg` build
//! knows, whether or not the platform it was compiled on has the syscalls to
//! back it.

#[cfg(unix)]
mod native {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
    use std::path::Path;
    use std::time::Duration;

    use vaco_core::Error as CoreError;
    use vaco_io::{MediaSink, MediaSource, PeekSource, RawSource, Seekability};
    use vaco_opts::{ConstDesc, Dict, OptionsExt, Options, Schema, schema_of};
    use vaco_protocol_core::{
        Access, IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result,
        Url,
    };
    use vaco_time::{Instant, sleep};

    /// How often a bounded accept loop polls. Same value as `crate::tcp`'s, for
    /// the same reason.
    const ACCEPT_POLL: Duration = Duration::from_millis(10);

    /// `-type`'s named constants.
    const TYPE_CONSTS: &[ConstDesc] = &[
        ConstDesc::new("stream", "Stream (reliable stream-oriented)", "type", 1),
        ConstDesc::new("datagram", "Datagram (unreliable packet-oriented)", "type", 2),
        ConstDesc::new("seqpacket", "Seqpacket (reliable packet-oriented)", "type", 5),
    ];

    /// `-h protocol=unix`.
    #[derive(Debug, Clone, PartialEq, Options)]
    #[options(name = "unix", help = "Unix domain socket")]
    pub struct UnixOptions {
        /// Bind and accept rather than connect.
        #[opt(name = "listen", help = "Open socket for listening", default = false, flags(param))]
        pub listen: bool,

        /// In **milliseconds**, `-1` = wait indefinitely. Applied to `-listen`'s
        /// accept wait; a plain `connect()` to a local socket has no comparable
        /// wait worth bounding (see the crate docs).
        #[opt(
            name = "timeout",
            help = "Timeout in ms",
            default = -1,
            range = -1..=i32::MAX,
            flags(param)
        )]
        pub timeout: i32,

        /// `stream`, `datagram` or `seqpacket`. `seqpacket` is accepted and then
        /// refused at open time with [`ProtocolError::Unsupported`] — `std`
        /// exposes no stable `UnixSeqpacket`. See the crate docs.
        #[opt(
            name = "type",
            help = "Socket type",
            unit = "type",
            consts = TYPE_CONSTS,
            default = 1,
            default_repr = "stream",
            range = 1..=5,
            flags(param)
        )]
        pub kind: i32,

        /// Maximum datagram size. Unused for `stream`.
        #[opt(
            name = "pkt_size",
            help = "Maximum packet size",
            default = 0,
            range = 0..=i32::MAX,
            flags(param)
        )]
        pub pkt_size: i32,
    }

    fn options(opts: &Dict) -> Result<UnixOptions> {
        let mut parsed = UnixOptions::default();
        parsed
            .apply_dict(opts)
            .map_err(|_| ProtocolError::Malformed {
                scheme: "unix",
                detail: "bad option value",
            })?;
        Ok(parsed)
    }

    /// `unix:`'s `rest` is the literal path, unlike `tcp:`/`udp:`'s
    /// `//host:port`: a socket path is a filesystem path (rule U1's own domain)
    /// and may contain characters (`?`, `:`) that would be meaningful to
    /// [`crate::url::parse`]. No inline query options exist for this protocol
    /// (matching the reference: every `unix` option is an `AVOption`, none is
    /// documented as a URL query parameter).
    fn socket_path(url: &Url) -> &Path {
        Path::new(&url.rest)
    }

    fn listen_timeout(opts: &UnixOptions) -> Option<Duration> {
        (opts.timeout >= 0).then(|| Duration::from_millis(u64::try_from(opts.timeout).unwrap_or(0)))
    }

    fn accept_stream(listener: &UnixListener, timeout: Option<Duration>) -> Result<UnixStream> {
        listener.set_nonblocking(true).map_err(ProtocolError::from)?;
        let deadline = timeout.map(|t| Instant::now().saturating_add(t));
        let max_polls = timeout.map_or(usize::MAX, |t| {
            t.as_nanos()
                .div_ceil(ACCEPT_POLL.as_nanos().max(1))
                .try_into()
                .unwrap_or(usize::MAX)
        });
        for _ in 0..=max_polls {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).map_err(ProtocolError::from)?;
                    return Ok(stream);
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
            "timeout elapsed with no incoming connection",
        )))
    }

    /// A connected or accepted Unix stream socket, read side.
    #[derive(Debug)]
    pub struct UnixSource {
        stream: UnixStream,
        pos: u64,
    }

    impl UnixSource {
        #[must_use]
        pub const fn new(stream: UnixStream) -> Self {
            Self { stream, pos: 0 }
        }
    }

    impl RawSource for UnixSource {
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

    /// A connected or accepted Unix stream socket, write side.
    #[derive(Debug)]
    pub struct UnixSink {
        stream: UnixStream,
        pos: u64,
    }

    impl UnixSink {
        #[must_use]
        pub const fn new(stream: UnixStream) -> Self {
            Self { stream, pos: 0 }
        }
    }

    impl MediaSink for UnixSink {
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

    /// A bound or connected `SOCK_DGRAM` Unix socket, read side.
    #[derive(Debug)]
    pub struct UnixDatagramSource {
        socket: UnixDatagram,
        pos: u64,
    }

    impl RawSource for UnixDatagramSource {
        fn read(&mut self, buf: &mut [u8]) -> vaco_core::Result<usize> {
            loop {
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

    /// A bound or connected `SOCK_DGRAM` Unix socket, write side.
    #[derive(Debug)]
    pub struct UnixDatagramSink {
        socket: UnixDatagram,
        pos: u64,
    }

    impl MediaSink for UnixDatagramSink {
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

    /// The `unix:` protocol.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct UnixProtocol;

    impl Protocol for UnixProtocol {
        fn open(
            &self,
            url: &Url,
            flags: IoFlags,
            opts_dict: &Dict,
            _env: &ProtocolEnv<'_>,
        ) -> Result<Box<dyn MediaSource>> {
            let opts = options(opts_dict)?;
            let path = socket_path(url);
            match opts.kind {
                2 => {
                    let socket = if flags.listen || opts.listen {
                        let _ = std::fs::remove_file(path);
                        UnixDatagram::bind(path).map_err(ProtocolError::from)?
                    } else {
                        let socket = UnixDatagram::unbound().map_err(ProtocolError::from)?;
                        socket.connect(path).map_err(ProtocolError::from)?;
                        socket
                    };
                    Ok(Box::new(PeekSource::new(UnixDatagramSource {
                        socket,
                        pos: 0,
                    })))
                }
                5 => Err(ProtocolError::Unsupported {
                    scheme: "unix",
                    operation: "seqpacket (std::os::unix::net has no stable UnixSeqpacket)",
                }),
                _ => {
                    let stream = if flags.listen || opts.listen {
                        let _ = std::fs::remove_file(path);
                        let listener = UnixListener::bind(path).map_err(ProtocolError::from)?;
                        accept_stream(&listener, listen_timeout(&opts))?
                    } else {
                        UnixStream::connect(path).map_err(ProtocolError::from)?
                    };
                    Ok(Box::new(PeekSource::new(UnixSource::new(stream))))
                }
            }
        }

        fn create(
            &self,
            url: &Url,
            flags: IoFlags,
            opts_dict: &Dict,
            _env: &ProtocolEnv<'_>,
        ) -> Result<Box<dyn MediaSink>> {
            let opts = options(opts_dict)?;
            let path = socket_path(url);
            match opts.kind {
                2 => {
                    let socket = if flags.listen || opts.listen {
                        let _ = std::fs::remove_file(path);
                        UnixDatagram::bind(path).map_err(ProtocolError::from)?
                    } else {
                        let socket = UnixDatagram::unbound().map_err(ProtocolError::from)?;
                        socket.connect(path).map_err(ProtocolError::from)?;
                        socket
                    };
                    Ok(Box::new(UnixDatagramSink { socket, pos: 0 }))
                }
                5 => Err(ProtocolError::Unsupported {
                    scheme: "unix",
                    operation: "seqpacket (std::os::unix::net has no stable UnixSeqpacket)",
                }),
                _ => {
                    let stream = if flags.listen || opts.listen {
                        let _ = std::fs::remove_file(path);
                        let listener = UnixListener::bind(path).map_err(ProtocolError::from)?;
                        accept_stream(&listener, listen_timeout(&opts))?
                    } else {
                        UnixStream::connect(path).map_err(ProtocolError::from)?
                    };
                    Ok(Box::new(UnixSink::new(stream)))
                }
            }
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

    /// The registry entry for `unix:`.
    pub static UNIX_PROTOCOL: ProtocolDesc = ProtocolDesc {
        name: "unix",
        long_name: "Unix domain socket",
        flags: ProtocolFlags {
            network: false,
            nested_scheme: false,
            server_capable: true,
        readable: true,
        writable: true,
    },
        default_whitelist: &[],
        options: Some(unix_schema),
        proto: &UnixProtocol,
    };

    fn unix_schema() -> &'static Schema {
        schema_of::<UnixOptions>()
    }

}

#[cfg(unix)]
pub use native::{
    UNIX_PROTOCOL, UnixDatagramSink, UnixDatagramSource, UnixOptions, UnixProtocol, UnixSink,
    UnixSource,
};

/// The `unix:` registration on a target with no `AF_UNIX`. Every call fails
/// with [`vaco_protocol_core::ProtocolError::Unsupported`]; nothing here
/// touches the filesystem or a socket.
#[cfg(not(unix))]
mod fallback {
    use vaco_io::{MediaSink, MediaSource};
    use vaco_opts::{Dict, Options, Schema, schema_of};
    use vaco_protocol_core::{
        Access, IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags,
        Result, Url,
    };

    /// Empty on this target: there is nothing to configure for a protocol
    /// that cannot open anything.
    #[derive(Debug, Clone, PartialEq, Options)]
    #[options(name = "unix", help = "Unix domain socket (unavailable on this platform)")]
    pub struct UnixOptions {}

    /// The `unix:` protocol, unavailable form.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct UnixProtocol;

    const UNAVAILABLE: ProtocolError = ProtocolError::Unsupported {
        scheme: "unix",
        operation: "unix domain sockets are not available on this target",
    };

    impl Protocol for UnixProtocol {
        fn open(
            &self,
            _url: &Url,
            _flags: IoFlags,
            _opts: &Dict,
            _env: &ProtocolEnv<'_>,
        ) -> Result<Box<dyn MediaSource>> {
            Err(UNAVAILABLE)
        }

        fn create(
            &self,
            _url: &Url,
            _flags: IoFlags,
            _opts: &Dict,
            _env: &ProtocolEnv<'_>,
        ) -> Result<Box<dyn MediaSink>> {
            Err(UNAVAILABLE)
        }

        fn check(&self, _url: &Url, _env: &ProtocolEnv<'_>) -> Result<Access> {
            Ok(Access::default())
        }
    }

    /// The registry entry for `unix:` on this target.
    pub static UNIX_PROTOCOL: ProtocolDesc = ProtocolDesc {
        name: "unix",
        long_name: "Unix domain socket",
        flags: ProtocolFlags {
            network: false,
            nested_scheme: false,
            server_capable: false,
        readable: true,
        writable: true,
    },
        default_whitelist: &[],
        options: Some(unix_schema),
        proto: &UnixProtocol,
    };

    fn unix_schema() -> &'static Schema {
        schema_of::<UnixOptions>()
    }
}

#[cfg(not(unix))]
pub use fallback::{UNIX_PROTOCOL, UnixOptions, UnixProtocol};
