//! The RTMP chunk stream layer, AMF0, and the NetConnection/NetStream
//! command flow.
//!
//! # What this crate is (so far)
//!
//! The chunk stream, AMF0/command flow, and tunnelled variants all land in
//! this one crate. It is still a transport-framing library, not yet a
//! `vaco_protocol_core::Protocol` — there is no `rtmp:`/`rtmps:` registry
//! entry, because that needs socket ownership this crate does not have.
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
//! [`amf0`] encodes/decodes the marker-byte-tagged AMF0 value
//! format `adobe-amf0-spec` defines — Number, Boolean, String/Long
//! String, Object, Null, Undefined, ECMA Array, Strict Array, Date; see
//! that module's own docs for the six deliberately-unsupported types.
//! [`command`] builds and parses NetConnection/NetStream command
//! messages on top: `connect`/`createStream`/`publish`/`play` and their
//! named `_result`/`onStatus` responses (§7.2's own status codes,
//! `NetConnection.Connect.Success`/`NetStream.Publish.Start`/
//! `NetStream.Play.Start`).
//!
//! # No reference server on this machine
//!
//! No live `nginx-rtmp`/`srs`/Wowza-class RTMP server was reachable to
//! interoperate against, so the reference-server acceptance criterion ("publish and
//! play round-trip against a reference server with the same command
//! sequence the reference emits") is named unreachable in the reference-
//! peer sense, matching `vaco-protocol-srt`/`vaco-protocol-rist`'s own
//! replacement-bar pattern. The substitute built instead:
//! `tests/publish_play_flow.rs` drives the full `connect` ->
//! `createStream` -> `publish`/`play` -> `onStatus` sequence end to end
//! through this crate's own real chunk-stream transport (client and
//! server both this crate's own code), checked against the exact status
//! codes §7.2 itself names. Self-consistency evidence, not interop
//! evidence — stated as such, not as a substitute for the real thing.
//!
//! # `rtmps` and the tunnelled variants
//!
//! **`rtmps` needs no code here.** Every public entry point in this crate
//! (`Dechunker::feed`, `chunk_message`, `handshake::build_*`) takes or
//! returns a plain `&[u8]`/`Vec<u8>` — nothing assumes TCP specifically.
//! Wrapping the same bytes in a TLS session (`vaco-protocol-tls`, once a
//! `Protocol` exists here to own that composition) is exactly `rtmps:`,
//! with zero changes to this crate. This is stated as a finding, not
//! deferred as unbuilt work: there is nothing to build.
//!
//! **`rtmpt`/`ffrtmphttp` (HTTP-tunnelled RTMP) are not built.** Unlike
//! the plain handshake (Adobe's own specification) or the digest
//! handshake (cross-checked against two independent write-ups, see
//! [`handshake`]'s own docs), no D7/D15-clean primary source for the
//! HTTP-tunnel wire framing itself (the `/open`/`/idle`/`/send`/`/close`
//! URL scheme, the session-ID and per-request sequence-number
//! conventions, the response's own poll-interval byte encoding) could be
//! located and cross-checked in the time this batch allowed. Building it
//! from general recollection alone would risk exactly what
//! `vaco-protocol-srt`'s `options.rs` already named as the danger:
//! smuggling implementation-specific detail in under a
//! clean-room-derived label. Noted here rather than silently dropped.
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
//! - `src/amf0.rs`, `src/command.rs` — see those modules' own docs for
//!   what AMF0/command-message scope is deliberately cut.
//! - `rtmpt`/`ffrtmphttp` become buildable the day a citable primary
//!   source for their wire framing is located — do not reconstruct one
//!   from general knowledge before then.
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

pub mod amf0;
pub mod chunk;
pub mod command;
pub mod control;
mod crypto;
pub mod handshake;
pub mod message;
mod rng;

pub use vaco_protocol_core::{ProtocolError, Result};
