//! SRT (Secure Reliable Transport), native, from `draft-sharabayko-srt-01`.
//!
//! Packet framing, the handshake state machine (caller, listener,
//! rendezvous), and the KM (key material) message *shape*. A transport
//! library, not yet a [`vaco_protocol_core::Protocol`] — no registry entry,
//! socket, or cipher yet; those land once the socket/timer seam and the
//! AES/CTR ownership question are settled.
//!
//! No `ffmpeg` build here carries `libsrt`, so every fact comes from the
//! draft. Tests are **draft-derived** (checked against the draft's worked
//! examples) or **self-consistency** (this crate's decoder agrees with its
//! own encoder — not proof it matches the draft). A real-peer interop
//! matrix is unreachable here and not replaced with a self-hosted
//! look-alike, which would only reprove self-consistency.
//!
//! # How it works
//!
//! - [`packet`] — common header, data/control framings (`draft` §3).
//! - [`handshake`] — Handshake CIF and extension blocks (`draft` §3.2.1).
//! - [`km`] — Key Material message shape (`draft` §3.2.2); the wrapped-key
//!   blob is parsed, not unwrapped — the cipher is deferred.
//! - [`cookie`] — rendezvous cookie (`draft` §4.3.2), MD5 of host/port/time.
//! - [`session`] — the three handshake state machines, sans-io.
//! - [`ack`] — ACK/NAK CIF shapes (`draft` §3.2.4-5); RTT/rate stats have
//!   no stated formula and are always zero.
//! - [`arq`] — retransmission engine, sans-io via `on_tick(now_ms)`. No
//!   `LiveCC`/`FileCC` (`draft` §5.1/§5.2 name them, not an algorithm);
//!   other constants are `IMPLEMENTATION-DEFINED`.
//! - [`pacing`] — [`pacing::Pacer`], a token-bucket rate ceiling
//!   `SendWindow::with_rate_limit` can attach.
//! - [`message`] — [`message::TransmissionMode`] (inferred from `STREAM`)
//!   and [`message::MessageReassembler`].
//! - [`options`] — [`options::SrtOptions`], scoped to the knobs this crate
//!   reads.
//!
//! No `Protocol` yet, no `-h protocol=srt` options. Depends on `vaco-core`,
//! `vaco-limits`, `vaco-protocol-core`, `vaco-time`; no crypto crate yet —
//! AES/CTR ownership is unresolved.

#![forbid(unsafe_code)]

pub mod ack;
pub mod arq;
pub mod cookie;
pub mod handshake;
pub mod km;
pub mod message;
pub mod options;
pub mod pacing;
pub mod packet;
pub mod session;

pub use vaco_protocol_core::{ProtocolError, Result};
