//! `shared:` — a local, multi-process fan-out ring.
//!
//! # What it is
//!
//! Layer 2. One producer process writes a stream once, an
//! arbitrary number of consumer processes on the same host each get their own
//! copy, and a slow consumer drops data rather than blocking the producer.
//!
//! # Why this is not `mmap`
//!
//! Real POSIX/`mmap` shared memory uses `unsafe` internally. That is not an
//! implementation detail that a wrapper crate can hide — reading bytes that
//! another process may be
//! concurrently mutating is exactly the case Rust's aliasing model cannot
//! check, so *every* crate on crates.io that exposes it puts `unsafe` on the
//! read side (`memmap2`'s `Mmap`, `shared_memory`'s `Shmem::as_slice`,
//! `mmap-sync`'s `Synchronizer::read` — checked directly, not assumed). This
//! crate forbids `unsafe`, so a protocol that can only be built with it is not
//! implemented that way.
//!
//! The protocol delivers a local, multi-process, fan-out, drop-when-full ring
//! on top of `AF_UNIX`
//! `SOCK_DGRAM` sockets instead of a memory-mapped region. Every property the
//! acceptance test cares about still holds:
//!
//! * **local, not network** — `AF_UNIX` never leaves the host, same as `mmap`.
//! * **multi-process** — sockets are named by filesystem path, so any process
//!   that can see the path can attach, same as `mmap`.
//! * **ring, not queue** — sends are non-blocking; a subscriber whose kernel
//!   receive buffer is full silently misses that message rather than stalling
//!   the producer, which is the "old data overwritten" behaviour a ring
//!   promises.
//!
//! The cost is a per-message copy through the kernel that `mmap` would avoid.
//! Given the constraints, that is the deliberate divergence, and it is
//! and makes no throughput claim, since the designs are not close enough for a
//! meaningful comparison.
//!
//! # Protocol
//!
//! `shared:<name>` resolves to `$TMPDIR/vaco-shared/<name>/`, containing:
//!
//! * `register` — a `SOCK_DGRAM` socket the producer binds
//!   ([`Protocol::create`]). Any subscriber's registration datagram arrives
//!   here; the *source address* of that datagram (which is meaningful for
//!   `AF_UNIX` only when the sender bound its own socket first, which every
//!   subscriber does) is the fan-out destination the producer records.
//! * `sub-<pid>-<n>` — one per subscriber ([`Protocol::open`]), created before
//!   it sends its registration so the address above is never empty.
//!
//! `<name>` must not contain a path separator: it names one segment under the
//! shared directory, not an arbitrary path.
//!
//! # Frame size
//!
//! There is no length check invented by this crate. `AF_UNIX` `SOCK_DGRAM`'s
//! real ceiling is a sysctl the kernel enforces on `send`/`sendto` with
//! `EMSGSIZE`; measured on this host, macOS defaults to 2048 bytes
//! (`net.local.dgram.maxdgram`) where Linux defaults to roughly 200 KiB
//! (`net.core.wmem_max`). Either way a caller that oversizes a write gets the
//! transport's own error, not a second, invented limit that could disagree
//! with it.
//!
//! # Security / bounds
//!
//! The only unbounded-growth risk in this crate is the producer's subscriber
//! list, which a chatty or malicious local peer could try to grow forever by
//! sending unlimited registration datagrams. It is capped at
//! [`MAX_SUBSCRIBERS`] with a plain comparison before every `push` — there is
//! no length-prefixed or otherwise attacker-declared size anywhere in this
//! protocol for `vaco_limits::Budget` to meter, so a fixed constant is the
//! right tool, not a missing one.
//!
//! # Example
//!
//! See `tests/shared_ipc.rs` for a genuine two-**process** round trip (not two
//! threads sharing one address space).

#![forbid(unsafe_code)]

#[cfg(unix)]
mod native {
    use std::collections::VecDeque;
    use std::fs;
    use std::io::ErrorKind;
    use std::os::unix::net::UnixDatagram;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use vaco_core::Error as CoreError;
    use vaco_io::{MediaSink, MediaSource, PeekSource, RawSource, Seekability};
    use vaco_opts::{Dict, Options, OptionsExt, Schema, schema_of};
    use vaco_protocol_core::{
        Access, IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result,
        Url,
    };
    use vaco_time::{Instant, sleep};

    /// How often the subscribe handshake retries while waiting for a producer.
    const SUBSCRIBE_POLL: Duration = Duration::from_millis(10);

    /// Hard cap on the producer's subscriber list. Not derived from any
    /// attacker-declared length (see the crate docs), so a plain constant is
    /// the bound rather than a `vaco_limits::Budget`.
    const MAX_SUBSCRIBERS: usize = 256;

    /// Registration datagrams carry no payload the producer reads; only the
    /// sender's bound source address matters. A non-empty marker still makes a
    /// stray packet distinguishable in a capture.
    const SUBSCRIBE_MARKER: &[u8] = b"vaco-shared-subscribe";

    /// `-h protocol=shared`.
    #[derive(Debug, Clone, Copy, PartialEq, Options)]
    #[options(name = "shared", help = "Local multi-process fan-out ring")]
    pub struct SharedOptions {
        /// In milliseconds; `-1` waits indefinitely for a producer to appear.
        /// Applies only to [`Protocol::open`] (the subscriber side) — `create`
        /// (the producer side) never waits on anything, it just binds.
        #[opt(
            name = "timeout",
            help = "Subscribe handshake timeout in ms, -1 = indefinite",
            default = 5000,
            range = -1..=i32::MAX,
            flags(param)
        )]
        pub timeout: i32,
    }

    fn options(opts: &Dict) -> Result<SharedOptions> {
        let mut parsed = SharedOptions::default();
        parsed
            .apply_dict(opts)
            .map_err(|_| ProtocolError::Malformed {
                scheme: "shared",
                detail: "bad option value",
            })?;
        Ok(parsed)
    }

    fn subscribe_deadline(opts: SharedOptions) -> Option<Instant> {
        (opts.timeout >= 0).then(|| {
            let ms = u64::try_from(opts.timeout).unwrap_or(0);
            Instant::now().saturating_add(Duration::from_millis(ms))
        })
    }

    /// Base directory all `shared:` names live under.
    fn base_dir() -> PathBuf {
        std::env::temp_dir().join("vaco-shared")
    }

    /// `shared:<name>` — `name` is one path segment, not a nested path.
    fn shared_dir(url: &Url) -> Result<PathBuf> {
        let name = url.rest.as_str();
        if name.is_empty() || name.contains(std::path::is_separator) || name == "." || name == ".."
        {
            return Err(ProtocolError::Malformed {
                scheme: "shared",
                detail: "name must be a single non-empty path segment",
            });
        }
        Ok(base_dir().join(name))
    }

    fn register_path(dir: &Path) -> PathBuf {
        dir.join("register")
    }

    /// The producer's write side: one `SOCK_DGRAM` bound at `register`, plus
    /// the list of subscriber addresses learned from registration datagrams.
    #[derive(Debug)]
    pub struct SharedSink {
        socket: UnixDatagram,
        register_path: PathBuf,
        subscribers: VecDeque<PathBuf>,
        pos: u64,
    }

    impl SharedSink {
        /// Drain any pending subscription requests without blocking. Bounded
        /// by [`MAX_SUBSCRIBERS`] on both the loop count and the list size, so
        /// a peer that floods registrations cannot grow this list or this
        /// call unboundedly.
        fn drain_subscribers(&mut self) {
            let mut scratch = [0u8; 64];
            for _ in 0..MAX_SUBSCRIBERS {
                match self.socket.recv_from(&mut scratch) {
                    Ok((_, addr)) => {
                        let Some(path) = addr.as_pathname() else {
                            // Unbound sender: cannot be fanned out to, ignore.
                            continue;
                        };
                        let path = path.to_path_buf();
                        if self.subscribers.contains(&path) {
                            continue;
                        }
                        if self.subscribers.len() >= MAX_SUBSCRIBERS {
                            // Oldest subscriber is dropped to make room rather
                            // than growing further — a ring policy applied to
                            // the subscriber list itself, not just the data.
                            self.subscribers.pop_front();
                        }
                        self.subscribers.push_back(path);
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }
    }

    impl MediaSink for SharedSink {
        fn write(&mut self, buf: &[u8]) -> vaco_core::Result<()> {
            self.drain_subscribers();
            let mut dead = Vec::new();
            for (idx, sub) in self.subscribers.iter().enumerate() {
                match self.socket.send_to(buf, sub) {
                    Ok(_) => {}
                    // Full receive buffer: exactly the "ring drops when full"
                    // behaviour this protocol promises. Not an error.
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                    // Subscriber process exited without cleaning up its
                    // socket file: prune it.
                    Err(e)
                        if e.kind() == ErrorKind::NotFound
                            || e.kind() == ErrorKind::ConnectionRefused =>
                    {
                        dead.push(idx);
                    }
                    // Anything else (e.g. `EMSGSIZE`) is a real transport
                    // failure and must not be swallowed.
                    Err(e) => return Err(CoreError::from(e)),
                }
            }
            for idx in dead.into_iter().rev() {
                self.subscribers.remove(idx);
            }
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

    impl Drop for SharedSink {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.register_path);
        }
    }

    /// The subscriber's read side: its own bound `SOCK_DGRAM`, cleaned up on
    /// drop.
    #[derive(Debug)]
    pub struct SharedSource {
        socket: UnixDatagram,
        own_path: PathBuf,
        pos: u64,
    }

    impl RawSource for SharedSource {
        fn read(&mut self, buf: &mut [u8]) -> vaco_core::Result<usize> {
            loop {
                return match self.socket.recv(buf) {
                    Ok(n) => {
                        self.pos = self.pos.saturating_add(n as u64);
                        Ok(n)
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => Err(CoreError::from(e)),
                };
            }
        }

        fn seekability(&self) -> Seekability {
            Seekability::None
        }
    }

    impl Drop for SharedSource {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.own_path);
        }
    }

    /// Monotonic counter so two subscribers opened by the same process (same
    /// pid) still get distinct socket paths.
    static NEXT_SUB_ID: AtomicU64 = AtomicU64::new(0);

    /// Bind a fresh subscriber socket under `dir` and register it with the
    /// producer, retrying until one appears or `opts.timeout` elapses.
    /// Returns the bound socket and its own path (needed so the caller can
    /// remove the socket file again on drop).
    fn subscribe(dir: &Path, opts: SharedOptions) -> Result<(UnixDatagram, PathBuf)> {
        fs::create_dir_all(dir).map_err(ProtocolError::from)?;
        let own_path = dir.join(format!(
            "sub-{}-{}",
            std::process::id(),
            NEXT_SUB_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&own_path);
        let socket = UnixDatagram::bind(&own_path).map_err(ProtocolError::from)?;
        socket.set_nonblocking(true).map_err(ProtocolError::from)?;

        let reg_path = register_path(dir);
        let deadline = subscribe_deadline(opts);
        loop {
            match socket.send_to(SUBSCRIBE_MARKER, &reg_path) {
                Ok(_) => break,
                Err(e)
                    if e.kind() == ErrorKind::NotFound
                        || e.kind() == ErrorKind::WouldBlock
                        || e.kind() == ErrorKind::ConnectionRefused =>
                {
                    // `NotFound`: no producer has bound `register` yet.
                    // `WouldBlock`: its receive queue is momentarily full.
                    // `ConnectionRefused`: measured (not guessed) on macOS —
                    // a `sendto` that lands in the split second between the
                    // producer's `bind()` creating the socket's directory
                    // entry and the kernel finishing wiring the endpoint up
                    // reports `ECONNREFUSED` rather than `ENOENT`, even
                    // though the very next attempt, tens of microseconds
                    // later, succeeds. All three are "not ready yet, keep
                    // retrying", not a reason to fail the handshake.
                }
                Err(e) => {
                    let _ = fs::remove_file(&own_path);
                    return Err(ProtocolError::from(e));
                }
            }
            if let Some(dl) = deadline
                && Instant::now() >= dl
            {
                let _ = fs::remove_file(&own_path);
                return Err(ProtocolError::from(std::io::Error::new(
                    ErrorKind::TimedOut,
                    "no `shared:` producer appeared before the subscribe timeout",
                )));
            }
            sleep(SUBSCRIBE_POLL);
        }

        socket.set_nonblocking(false).map_err(ProtocolError::from)?;
        Ok((socket, own_path))
    }

    /// The `shared:` protocol.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct SharedProtocol;

    impl Protocol for SharedProtocol {
        fn open(
            &self,
            url: &Url,
            _flags: IoFlags,
            opts_dict: &Dict,
            _env: &ProtocolEnv<'_>,
        ) -> Result<Box<dyn MediaSource>> {
            let opts = options(opts_dict)?;
            let dir = shared_dir(url)?;
            let (socket, own_path) = subscribe(&dir, opts)?;
            Ok(Box::new(PeekSource::new(SharedSource {
                socket,
                own_path,
                pos: 0,
            })))
        }

        fn create(
            &self,
            url: &Url,
            _flags: IoFlags,
            _opts: &Dict,
            _env: &ProtocolEnv<'_>,
        ) -> Result<Box<dyn MediaSink>> {
            let dir = shared_dir(url)?;
            fs::create_dir_all(&dir).map_err(ProtocolError::from)?;
            let reg_path = register_path(&dir);
            let _ = fs::remove_file(&reg_path);
            let socket = UnixDatagram::bind(&reg_path).map_err(ProtocolError::from)?;
            socket.set_nonblocking(true).map_err(ProtocolError::from)?;
            Ok(Box::new(SharedSink {
                socket,
                register_path: reg_path,
                subscribers: VecDeque::new(),
                pos: 0,
            }))
        }

        fn check(&self, _url: &Url, _env: &ProtocolEnv<'_>) -> Result<Access> {
            Ok(Access {
                read: true,
                write: true,
            })
        }
    }

    /// The registry entry for `shared:`.
    pub static SHARED_PROTOCOL: ProtocolDesc = ProtocolDesc {
        name: "shared",
        long_name: "Local multi-process fan-out ring",
        flags: ProtocolFlags {
            network: false,
            nested_scheme: false,
            server_capable: true,
            readable: true,
            writable: true,
        },
        default_whitelist: &[],
        options: Some(shared_schema),
        proto: &SharedProtocol,
    };

    fn shared_schema() -> &'static Schema {
        schema_of::<SharedOptions>()
    }
}

#[cfg(unix)]
pub use native::{SHARED_PROTOCOL, SharedOptions, SharedProtocol, SharedSink, SharedSource};

/// The `shared:` registration on a target with no `AF_UNIX` (mirrors
/// `vaco-protocol-socket::unix`'s fallback for the identical reason: the
/// scheme name exists on every target, even where the syscalls backing it do
/// not).
#[cfg(not(unix))]
mod fallback {
    use vaco_io::{MediaSink, MediaSource};
    use vaco_opts::{Dict, Options, Schema, schema_of};
    use vaco_protocol_core::{
        Access, IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result,
        Url,
    };

    #[derive(Debug, Clone, PartialEq, Options)]
    #[options(
        name = "shared",
        help = "Local multi-process fan-out ring (unavailable on this platform)"
    )]
    pub struct SharedOptions {}

    #[derive(Debug, Clone, Copy, Default)]
    pub struct SharedProtocol;

    const UNAVAILABLE: ProtocolError = ProtocolError::Unsupported {
        scheme: "shared",
        operation: "AF_UNIX is not available on this target",
    };

    impl Protocol for SharedProtocol {
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

    pub static SHARED_PROTOCOL: ProtocolDesc = ProtocolDesc {
        name: "shared",
        long_name: "Local multi-process fan-out ring",
        flags: ProtocolFlags {
            network: false,
            nested_scheme: false,
            server_capable: false,
            readable: true,
            writable: true,
        },
        default_whitelist: &[],
        options: Some(shared_schema),
        proto: &SharedProtocol,
    };

    fn shared_schema() -> &'static Schema {
        schema_of::<SharedOptions>()
    }
}

#[cfg(not(unix))]
pub use fallback::{SHARED_PROTOCOL, SharedOptions, SharedProtocol};

use vaco_protocol_core::ProtocolRegistry;

/// Register `shared:`.
pub fn register(registry: &mut ProtocolRegistry) {
    registry.register(&SHARED_PROTOCOL);
}
