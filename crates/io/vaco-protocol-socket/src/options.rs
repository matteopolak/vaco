//! Option surfaces for `tcp:`, `udp:`/`udplite:` and `unix:`.
//!
//! Names, defaults and ranges are copied from `ffmpeg -h protocol=<name>`
//! (8.1), read as observed behaviour of a shipped binary (D6/D7/D17) — not
//! from memory and not from a plan. See the crate docs for which options are
//! accepted here but not wired to a real syscall, and why.

use vaco_opts::Options;

/// `-h protocol=tcp`.
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "tcp", help = "TCP transport")]
pub struct TcpOptions {
    /// `0`: connect. `1`/`2`: bind and accept. We treat `2` the same as `1`
    /// (accept exactly one connection) — see the crate docs' "deliberately
    /// not implemented" section.
    #[opt(
        name = "listen",
        help = "Listen for incoming connections",
        default = 0,
        range = 0..=2,
        flags(param)
    )]
    pub listen: i32,

    /// Local port to bind before connecting, or to listen on. Empty means "any".
    #[opt(
        name = "local_port",
        help = "Local port",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub local_port: String,

    /// Local address to bind before connecting, or to listen on.
    #[opt(
        name = "local_addr",
        help = "Local address",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub local_addr: String,

    /// Socket I/O timeout, in **microseconds**. `-1` means block indefinitely.
    /// Also governs the `connect()` call itself, matching the reference.
    #[opt(
        name = "timeout",
        help = "set timeout (in microseconds) of socket I/O operations",
        default = -1,
        range = -1..=i32::MAX,
        flags(param)
    )]
    pub timeout: i32,

    /// How long `-listen` waits for a connection, in **milliseconds**. `-1`
    /// means wait indefinitely.
    #[opt(
        name = "listen_timeout",
        help = "Connection awaiting timeout (in milliseconds)",
        default = -1,
        range = -1..=i32::MAX,
        flags(param)
    )]
    pub listen_timeout: i32,

    /// `SO_SNDBUF`. `-1` leaves the OS default.
    #[opt(
        name = "send_buffer_size",
        help = "Socket send buffer size (in bytes)",
        default = -1,
        range = -1..=i32::MAX,
        flags(param)
    )]
    pub send_buffer_size: i32,

    /// `SO_RCVBUF`. `-1` leaves the OS default.
    #[opt(
        name = "recv_buffer_size",
        help = "Socket receive buffer size (in bytes)",
        default = -1,
        range = -1..=i32::MAX,
        flags(param)
    )]
    pub recv_buffer_size: i32,

    /// `TCP_NODELAY`.
    #[opt(
        name = "tcp_nodelay",
        help = "Use TCP_NODELAY to disable nagle's algorithm",
        default = false,
        flags(param)
    )]
    pub tcp_nodelay: bool,

    /// `SO_KEEPALIVE`, with the OS default probe interval.
    #[opt(
        name = "tcp_keepalive",
        help = "Use TCP keepalive to detect dead connections",
        default = false,
        flags(param)
    )]
    pub tcp_keepalive: bool,

    /// `TCP_MAXSEG`. Accepted for interface parity; not wired to a syscall —
    /// `socket2` has no cross-platform accessor, and a raw `setsockopt` would
    /// need `unsafe`. See the crate docs.
    #[opt(
        name = "tcp_mss",
        help = "Maximum segment size for outgoing TCP packets",
        default = -1,
        range = -1..=i32::MAX,
        flags(param)
    )]
    pub tcp_mss: i32,
}

/// `-h protocol=udp` / `-h protocol=udplite`. One struct for both: every
/// option the reference documents for either is identical, `udplite`
/// additionally exposing `udplite_coverage` (which `udp` also technically
/// accepts and ignores, matching the measured `-h protocol=udp` listing that
/// already shows it).
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "udp", help = "UDP / UDP-Lite transport")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per reference option name"
)]
pub struct UdpOptions {
    /// `SO_SNDBUF`/`SO_RCVBUF` (the reference uses one option for both).
    #[opt(
        name = "buffer_size",
        help = "System data size (in bytes)",
        default = -1,
        range = -1..=i32::MAX,
        flags(param)
    )]
    pub buffer_size: i32,

    /// Local port to bind before sending/receiving. `-1` means "any".
    #[opt(
        name = "localport",
        alias = "local_port",
        help = "Local port",
        default = -1,
        range = -1..=i32::MAX,
        flags(param)
    )]
    pub localport: i32,

    /// Local address to bind before sending/receiving.
    #[opt(
        name = "localaddr",
        help = "Local address",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub localaddr: String,

    /// UDP-Lite checksum coverage length. Accepted for interface parity; not
    /// wired to a syscall (`UDPLITE_SEND_CSCOV`/`UDPLITE_RECV_CSCOV` have no
    /// `socket2` accessor). See the crate docs.
    #[opt(
        name = "udplite_coverage",
        help = "choose UDPLite head size which should be validated by checksum",
        default = 0,
        range = 0..=i32::MAX,
        flags(param)
    )]
    pub udplite_coverage: i32,

    /// Maximum size of one packet, both for the send-side cap and the
    /// receive-side buffer.
    #[opt(
        name = "pkt_size",
        help = "Maximum UDP packet size",
        default = 1472,
        range = -1..=i32::MAX,
        flags(param)
    )]
    pub pkt_size: i32,

    /// `SO_REUSEADDR`. `-1` (`auto`, matching the reference's own tri-state
    /// default) behaves as `false` here: we have no "was this address
    /// already bound by us" state to make `auto` meaningfully different.
    #[opt(
        name = "reuse",
        alias = "reuse_socket",
        help = "explicitly allow reusing UDP sockets",
        default = -1,
        range = -1..=1,
        flags(param)
    )]
    pub reuse: i32,

    /// `SO_BROADCAST`.
    #[opt(
        name = "broadcast",
        help = "explicitly allow or disallow broadcast destination",
        default = false,
        flags(encoding)
    )]
    pub broadcast: bool,

    /// Multicast TTL / hop limit.
    #[opt(
        name = "ttl",
        help = "Time to live (multicast only)",
        default = 16,
        range = 0..=255,
        flags(encoding)
    )]
    pub ttl: i32,

    /// `IP_TOS` / DSCP class. `-1` leaves the OS default. Not applied on
    /// Windows (`socket2::Socket::set_tos` is unix-only; see the crate docs).
    #[opt(
        name = "dscp",
        help = "DSCP class for outgoing packets",
        default = -1,
        range = -1..=63,
        flags(encoding)
    )]
    pub dscp: i32,

    /// Call `connect()` on the socket (restricts the peer and lets us use
    /// `send`/`recv` instead of `sendto`/`recvfrom`), rather than binding
    /// only.
    #[opt(
        name = "connect",
        help = "set if connect() should be called on socket",
        default = false,
        flags(param)
    )]
    pub connect: bool,

    /// Read-side circular buffer size, in 188-byte (MPEG-TS packet) units.
    /// Accepted for interface parity; not wired to a background-thread
    /// prefetcher. See the crate docs.
    #[opt(
        name = "fifo_size",
        help = "set the UDP circular buffer size (in 188-byte packets)",
        default = 28672,
        range = 0..=i32::MAX,
        flags(decoding)
    )]
    pub fifo_size: i32,

    /// Survive a fifo overrun instead of erroring. Meaningless while
    /// `fifo_size` is unwired; accepted anyway so a caller need not special-case
    /// this crate.
    #[opt(
        name = "overrun_nonfatal",
        help = "survive in case of UDP receiving circular buffer overrun",
        default = false,
        flags(decoding)
    )]
    pub overrun_nonfatal: bool,

    /// Read-side receive timeout, in **microseconds**. `0` (the reference
    /// default) means block indefinitely.
    #[opt(
        name = "timeout",
        help = "set raise error timeout, in microseconds (only in read mode)",
        default = 0,
        range = 0..=i32::MAX,
        flags(decoding)
    )]
    pub timeout: i32,

    /// Source-specific multicast allow-list. Accepted for interface parity;
    /// not wired (see the crate docs — `socket2` has no source-filtered
    /// multicast join).
    #[opt(
        name = "sources",
        help = "Source list",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub sources: String,

    /// Source-specific multicast block-list. Same deferral as `sources`.
    #[opt(
        name = "block",
        help = "Block list",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub block: String,
}

impl UdpOptions {
    /// `-reuse`'s tri-state, collapsed to a bool per this crate's `auto ==
    /// false` policy (see the field doc).
    #[must_use]
    pub const fn reuse_address(&self) -> bool {
        self.reuse == 1
    }
}
