//! The `dtls:` protocol: a DTLS-over-UDP transport, and the crate that owns
//! this workspace's one `openssl` dependency (D11).
//!
//! # What it is
//!
//! A [`vaco_protocol_core::Protocol`] implementation of `dtls:` — the scheme
//! WHIP/WebRTC-shaped callers open when they need a DTLS-secured datagram
//! stream. #562 (`PR-12b`) was blocked from the start of this project on "no
//! native Rust DTLS stack exists" (D14.2's zero-FFI Gate 1 admitted no
//! alternative): the 2026-08-28 owner amendment to Gate 1
//! (`planning/00-decisions.md`, "Gate 1 amendment") lifts that block for
//! transport security specifically, and this crate is the result.
//!
//! # How it works
//!
//! | Module | Job |
//! |---|---|
//! | [`options`] | `DtlsOptions` — `-h protocol=dtls`'s surface, the parts this crate implements. |
//! | [`cert`] | Loading a configured PEM certificate/key, or generating an ephemeral self-signed one when none is configured — see its module docs for why that default exists. |
//! | [`context`] | Building the `openssl::ssl::SslContext` `-verify`/`-use_srtp`/`-mtu` describe. |
//! | [`transport`] | [`transport::UdpTransport`] — a connected `UdpSocket` wearing the `Read`/`Write` coat `openssl::ssl::SslStream` needs. |
//! | [`connect`] | The client path: connects its own UDP socket, applies the whitelist check by hand, and drives the handshake. |
//! | [`listen`] | The server path (`-listen 1`): binds, waits for the first peer datagram, and drives the handshake. |
//! | [`protocol`] | [`protocol::DtlsProtocol`]: the `Protocol::open`/`create` entry points. |
//!
//! # Security: the whitelist, and `-verify`'s default
//!
//! `dtls:` grants nothing by default (`default_whitelist` is `&[]`) and needs
//! `udp` granted to it explicitly — the same shape `vaco-protocol-tls`'s
//! `tls:` uses for `tcp`, for the same reason (see that crate's docs for the
//! full measured argument for this class of protocol).
//!
//! `-verify`/`-tls_verify` default to `false`, matching the reference
//! (`ffmpeg -h protocol=dtls`). Unlike ordinary TLS, this is not merely
//! matching an existing default for its own sake: DTLS peers in this
//! protocol's actual use case (WebRTC/WHIP) authenticate via a fingerprint
//! exchanged over signalling, not a CA chain, so there is usually no chain to
//! check at all — see [`cert`]'s module docs. What still happens when
//! `verify` is left off: the handshake's cryptographic key exchange still
//! authenticates that the peer holds the private key matching whatever
//! certificate it presented; what does not happen: checking that
//! certificate's issuer or hostname against anything. `-verify 1` (or
//! `-tls_verify 1`) with `-ca_file` turns chain verification on, exactly as
//! `vaco-protocol-tls` does.
//!
//! # What is deliberately not implemented
//!
//! `-http_proxy` (matching `vaco-protocol-tls`'s and `vaco-protocol-http`'s
//! own scoping) and `-external_sock` (no already-connected-fd handoff exists
//! in this project's `IoFlags`/`ProtocolEnv` model to receive one through —
//! same blocker `vaco-protocol-tls` records for the same option). DTLS's own
//! retransmission timers for a lossy transport are not implemented either —
//! see [`transport`]'s module docs for the precise gap. `-listen`'s stateless
//! cookie exchange (RFC 6347's `HelloVerifyRequest`, `DTLSv1_listen` in
//! OpenSSL's own terms) is scoped out in favour of a simpler
//! connect-on-first-packet accept — see [`listen`]'s module docs for why that
//! is the right call for this protocol's actual callers.
//!
//! # Configuration
//!
//! [`options::DtlsOptions`] — see the crate docs above and that module's own
//! docs for the full, measured option table.
//!
//! # Dependencies (D11 — who owns `openssl`)
//!
//! This crate declares `openssl` (with its `vendored` feature, so no system
//! OpenSSL install is required at build time) directly, and nothing else in
//! this workspace does — `cargo xtask owner-gate`'s `MEDIA` list and `cargo
//! xtask dep-gate`'s scoped Gate 1 check both enforce this. See
//! `docs/dependencies.md` for the full Gate 2/3 assessment (openssl chosen
//! over boring/wolfssl) and the licence record (OpenSSL itself, as vendored
//! by `openssl-src`, is Apache-2.0 — checked directly, per D9's "check what
//! is actually linked, not what the wrapper declares", not assumed from the
//! `openssl`/`openssl-sys` crates' own declared licence alone, though those
//! also declare Apache-2.0).
//!
//! # wasm
//!
//! Native-only (`xtask/src/wasm.rs`'s `NATIVE_ONLY`): `openssl-sys`'s
//! vendored build compiles C via `cc`, which does not target
//! `wasm32-unknown-unknown`, and DTLS itself needs a real UDP socket, which
//! that target has none of either — the same two reasons
//! `vaco-protocol-socket` and `vaco-protocol-tls` are already on that list.

#![forbid(unsafe_code)]

pub mod cert;
pub mod connect;
pub mod context;
pub mod listen;
pub mod options;
pub mod protocol;
pub mod transport;

pub use options::DtlsOptions;
pub use protocol::{DTLS_PROTOCOL, DtlsProtocol, DtlsSink, DtlsSource};

use vaco_protocol_core::ProtocolRegistry;

/// Register `dtls:`.
pub fn register(registry: &mut ProtocolRegistry) {
    registry.register(&DTLS_PROTOCOL);
}
