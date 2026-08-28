//! The RTMP chunk stream layer: handshake, chunking/dechunking, and the six
//! protocol control messages.
//!
//! # What this crate is (so far)
//!
//! This is PR-09a of three. It is a transport-framing library, not yet a
//! `vaco_protocol_core::Protocol` — there is no `rtmp:`/`rtmps:` registry
//! entry here, because a session is not useful without AMF0/AMF3 and the
//! NetConnection/NetStream command flow that PR-09b adds on top of this.
//! `Client`, [`chunk`], [`message`] and [`control`] are the building blocks
//! that package will call.
//!
//! # How it works
//!
//! [`handshake`] runs the byte exchange (C0/C1/C2 against S0/S1/S2) that
//! must complete before either side has a chunk stream at all, in both the
//! plain form Adobe's own specification documents and the HMAC-SHA256
//! digest form real deployments actually negotiate. [`chunk`] encodes and
//! decodes one chunk's basic header, message header and extended timestamp.
//! [`message`] turns a chunk stream in each direction into whole messages
//! and back: [`message::Dechunker`] holds one [`chunk::ChunkStreamState`]
//! per chunk stream ID (for type-1/2/3 header delta compression) and hands
//! back a complete [`message::RtmpMessage`] once one arrives, however many
//! chunks it took; [`message::chunk_message`] does the reverse. [`control`]
//! encodes and decodes the six message-type-1..6 payloads every RTMP
//! session exchanges regardless of what is layered on top.
//!
//! # How to change it
//!
//! - `src/crypto.rs` — SHA-256 and HMAC-SHA256, hand-rolled rather than a
//!   new dependency (see that module's docs for why, and for the FIPS
//!   180-4/RFC 4231 test vectors it is checked against).
//! - `src/rng.rs` — the handshake's non-cryptographic filler bytes.
//! - `src/handshake.rs` — see its own docs for the two schemes and what is
//!   unverified about the digest one.
//! - `src/chunk.rs`, `src/message.rs`, `src/control.rs` — the framing.
//!
//! # Configuration
//!
//! None yet — no `Protocol`, no `-h protocol=rtmp` options.
//!
//! # Dependencies
//!
//! `vaco-protocol-core` for [`Result`]/`ProtocolError` (this crate reuses the
//! protocol layer's error type rather than inventing its own, so a future
//! `Protocol` impl over it does not need a translation layer). `vaco-limits`
//! for [`vaco_limits::Budget`], required wherever a message body is sized
//! from the wire's 3-byte, peer-controlled length field. `vaco-time` for a
//! wasm-safe clock, used only to seed [`rng`] — never `Instant::now()`
//! directly.

#![forbid(unsafe_code)]

pub mod chunk;
pub mod control;
mod crypto;
pub mod handshake;
pub mod message;
mod rng;

pub use vaco_protocol_core::{ProtocolError, Result};
