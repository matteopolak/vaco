//! The `tcp:`, `udp:`, `udplite:` and `unix:` protocols: bare OS sockets.
//!
//! # What it is
//!
//! Four [`vaco_protocol_core::Protocol`] implementations, each a thin,
//! mechanical wrapper over a socket type the standard library or `socket2`
//! already provides. None of them opens another URL — see "Security" below —
//! so this crate is the leaf of the protocol graph: `tls:`
//! (`vaco-protocol-tls`) and `http:`/`https:` (`vaco-protocol-http`) both sit
//! on top of a raw TCP connection, but neither reaches it through this crate;
//! see [`crate::tcp`]'s module docs for why nesting through the registry does
//! not work for a duplex transport, and how `vaco-protocol-tls` instead
//! reimplements the same handful of syscalls independently.
//!
//! | Module | Protocol | Transport |
//! |---|---|---|
//! | [`tcp`] | `tcp:` | [`std::net::TcpStream`] / [`std::net::TcpListener`] |
//! | [`udp`] | `udp:` | [`socket2::Socket`], `SOCK_DGRAM` |
//! | [`udp`] | `udplite:` | [`socket2::Socket`], `SOCK_DGRAM` over `IPPROTO_UDPLITE` |
//! | [`unix`] | `unix:` | [`std::os::unix::net::UnixStream`] / `UnixListener` on `unix`; a stub that reports `Unsupported` elsewhere |
//!
//! # How it works
//!
//! [`url`] parses the `//host:port[?opt=val&...]` (`tcp:`/`udp:`/`udplite:`) or
//! bare-path (`unix:`) grammar out of [`vaco_protocol_core::Url::rest`].
//! [`addr`] resolves `host:port` to a [`std::net::SocketAddr`] list and
//! connects with a bound timeout. [`options`] holds one `Options` struct per
//! protocol, matching `ffmpeg -h protocol=<name>`'s own option names (D9).
//! Each protocol module then does the minimum work to turn a connected or
//! bound socket into a [`vaco_io::RawSource`]/[`vaco_io::MediaSink`] pair,
//! exactly as [`vaco_protocol_file::FileProtocol`] does for a local file.
//!
//! # Security: every protocol here grants nothing, and needs nothing granted
//!
//! [`vaco_protocol_core::ProtocolFlags::nested_scheme`] is `false` for all
//! four registrations, and every [`vaco_protocol_core::ProtocolDesc::default_whitelist`]
//! is empty. Measured directly (`ffmpeg 8.1`, D17): `ffmpeg -v debug -i
//! tcp://127.0.0.1:<port>` prints `Setting default whitelist` for **no**
//! protocol here — the reference's own debug log names the line **"No
//! default whitelist set"** for `tcp`, matching `tls`'s own (see
//! `docs/io/vaco-protocol-tls.md`). None of these four protocols opens a
//! nested URL, so there is nothing for a default grant to apply to — the
//! empty list is not a conservative choice, it is the accurate one.
//!
//! What *is* a real, measured security property: a caller that wants to open
//! a bare `tcp://…`/`udp://…`/`unix:…` URL — whether typed by a user or named
//! by a playlist an `hls:`/`dash:`/`concat:`/`sdp:` document controls — must
//! have the scheme itself on its whitelist (or be unrestricted). Nothing
//! here grants that implicitly; see `vaco-protocol-wrap`'s crate docs for the
//! general shape of this rule ("none of these protocols grants any default
//! whitelist to what they open") and `vaco-protocol-tls`'s for the one
//! wrinkle specific to a protocol that *does* nest (`tls:` opening a raw
//! socket the way this crate's own `tcp:` does).
//!
//! # What is deliberately not implemented
//!
//! See each protocol module's own docs for the option-by-option detail. In
//! summary: TCP's `-tcp_mss` (no cross-platform safe syscall for it in
//! `socket2`); UDP-Lite's `-udplite_coverage` (same reason — the checksum
//! coverage length has no `socket2` accessor and a raw `setsockopt` would need
//! `unsafe`, which `#![forbid(unsafe_code)]` rules out with no exception
//! outside `vaco-hw-*`); UDP's `-bitrate`/`-burst_bits` (write-side pacing —
//! D5 has zero muxers, so nothing calls
//! [`Protocol::create`](vaco_protocol_core::Protocol::create) with a bitrate
//! yet); UDP's `-fifo_size`/`-overrun_nonfatal` (accepted for interface parity
//! but not wired to a background-thread circular buffer — every read here is
//! synchronous, one `recv` per demuxer read); UDP's `-sources`/`-block`
//! (source-specific multicast filtering — group `join`/`leave` is
//! implemented via `socket2`, per-source filtering is not); `unix:`'s
//! `seqpacket` socket type (`std::os::unix::net` has no stable
//! `UnixSeqpacket`); and `-listen 2` (accept-then-keep-listening) is treated
//! identically to `-listen 1` (accept exactly one connection), since a
//! faithful re-listen loop needs the same background-driver question
//! `vaco-sched::Driver` answers and no protocol crate should invent its own
//! answer to it (see `xtask/src/time_gate.rs`'s note on `std::thread::spawn`).
//!
//! # Configuration
//!
//! [`options::TcpOptions`], [`options::UdpOptions`] (shared by `udp:` and
//! `udplite:`), [`options::UnixOptions`] — one field per option this crate
//! implements, named and defaulted from `ffmpeg -h protocol=<name>` (D9/D17).
//! Also honours [`vaco_protocol_core::ProtocolEnv::rw_timeout`] as the
//! fallback when a protocol's own `-timeout` was left at its default.
//!
//! # Dependencies
//!
//! `socket2` (D14.3) for the options `std::net` has no portable accessor for
//! (buffer sizes, keepalive tuning, multicast group membership, `TOS`/DSCP).
//! Nothing else new: no PEM, no TLS, no HTTP — those belong to
//! `vaco-protocol-tls` and `vaco-protocol-http` respectively.
//!
//! # wasm
//!
//! Exempted from `cargo xtask wasm-check` (see `xtask/src/wasm.rs`'s
//! `NATIVE_ONLY` list) — measured, not assumed: a throwaway crate depending on
//! `socket2` alone fails to build for `wasm32-unknown-unknown` with nine
//! `E0308`/`E0061`/`E0583` errors inside `socket2` itself (it assumes
//! `std::net::{TcpStream,TcpListener,UdpSocket}` exist, which they do not on
//! that target). `std::net` alone *does* compile there (it is a stub that
//! returns `io::Error`, not a `compile_error!`), but `socket2` does not, so
//! this crate cannot either. A wasm build reaches a socket through the host
//! runtime's own APIs (`WebSocket`, `WebTransport`), which is a different
//! protocol behind the same [`vaco_protocol_core::Protocol`] trait — the same
//! D11 adapter-rule argument `vaco-protocol-http`'s exemption already makes.

#![forbid(unsafe_code)]

pub mod addr;
pub mod options;
pub mod tcp;
pub mod udp;
pub mod unix;
pub mod url;

pub use options::{TcpOptions, UdpOptions};
pub use tcp::{TCP_PROTOCOL, TcpProtocol, TcpSink, TcpSource};
pub use udp::{UDP_PROTOCOL, UDPLITE_PROTOCOL, UdpProtocol, UdpSink, UdpSource};
// `unix:` always registers (see `unix`'s module docs): on a non-`unix` target
// this is the `#[cfg(not(unix))]` fallback that reports `Unsupported` at open
// time, not a missing item, so `UNIX_PROTOCOL`'s ctor path — named
// unconditionally by `vaco-component.toml` — resolves on every platform.
pub use unix::{UNIX_PROTOCOL, UnixOptions, UnixProtocol};
#[cfg(unix)]
pub use unix::{UnixDatagramSink, UnixDatagramSource, UnixSink, UnixSource};

use vaco_protocol_core::ProtocolRegistry;

/// Register every protocol this crate provides.
///
/// `unix:` registers on every platform — see [`unix`]'s module docs for why
/// that is correct even where `AF_UNIX` does not exist.
pub fn register(registry: &mut ProtocolRegistry) {
    registry.register(&TCP_PROTOCOL);
    registry.register(&UDP_PROTOCOL);
    registry.register(&UDPLITE_PROTOCOL);
    registry.register(&UNIX_PROTOCOL);
}
