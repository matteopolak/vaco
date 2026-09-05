//! The `tcp:`, `udp:`, `udplite:` and `unix:` protocols: bare OS sockets.
//!
//! Four [`vaco_protocol_core::Protocol`] implementations wrap standard-library
//! or `socket2` sockets. They are leaves of the protocol graph; `tls:` and
//! `http:`/`https:` build their own duplex transports above raw TCP.
//!
//! # How it works
//!
//! [`url`] parses the protocol-specific URL grammar, [`addr`] resolves network
//! addresses with bounded timeouts, and [`options`] mirrors the reference's
//! `ffmpeg -h protocol=<name>` option names. The protocol modules expose the
//! resulting sockets as [`vaco_io::RawSource`] and [`vaco_io::MediaSink`].
//!
//! # Security
//!
//! All four registrations have `nested_scheme: false` and an empty
//! `default_whitelist`. Measured against `ffmpeg 8.1` (D17), each reports
//! "No default whitelist set". A caller opening `tcp:`, `udp:`, `udplite:` or
//! `unix:` from a user URL or playlist must still whitelist that scheme (or
//! run unrestricted); these protocols grant no scheme implicitly.
//!
//! # Scope and configuration
//!
//! Unsupported options and platform constraints are recorded in each module's
//! docs and `docs/io/vaco-protocol-socket.md`. The option structs are
//! [`options::TcpOptions`], [`options::UdpOptions`] and
//! [`options::UnixOptions`]; `vaco-protocol-core` supplies the whitelist gate.
//!
//! `socket2` is needed for socket options absent from `std::net`, and is the
//! reason this crate is excluded from wasm builds: the dependency does not
//! compile for `wasm32-unknown-unknown` (measured, not assumed).

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
