//! SCTP (RFC 4960) — issue #561, PR-12a.
//!
//! # What this package is
//!
//! Framing ([`packet`], [`chunk`]) and a sans-io association state
//! machine ([`association`]) for RFC 4960's four-way handshake and basic
//! reliable data transfer, at the same "framing/state library, no
//! `Protocol` yet" stage every other new protocol crate in this project
//! has started at (`vaco-protocol-srt`/`vaco-protocol-rist`/
//! `vaco-protocol-rtmp`).
//!
//! # Scope, stated up front
//!
//! - Twelve chunk types are built (`DATA`/`INIT`/`INIT ACK`/`SACK`/
//!   `HEARTBEAT`/`HEARTBEAT ACK`/`ABORT`/`SHUTDOWN`/`SHUTDOWN ACK`/
//!   `ERROR`/`COOKIE ECHO`/`COOKIE ACK`/`SHUTDOWN COMPLETE`) — their fixed
//!   fields, not their optional variable-length parameters (except INIT
//!   ACK's mandatory State Cookie, which the handshake cannot work
//!   without). See [`chunk`]'s own docs.
//! - [`association::Association`] drives §5's four-way handshake
//!   (`INIT` -> `INIT ACK` -> `COOKIE ECHO` -> `COOKIE ACK`) and a basic
//!   `DATA`/`SACK` exchange: one sender queues data by TSN, the receiver
//!   acknowledges the cumulative TSN it has seen contiguously. **Not
//!   built**: multi-homing (one association across several IP addresses),
//!   PR-SCTP partial reliability, stream-level ordering guarantees beyond
//!   what `stream_sequence_number` merely carries, congestion control,
//!   and the shutdown sequence past `SHUTDOWN`/`SHUTDOWN ACK`/
//!   `SHUTDOWN COMPLETE`'s three fixed messages (no T2-shutdown timer,
//!   no retransmission backoff).
//!
//! # No reference peer, and no 24-hour fuzz run, from here
//!
//! #561's own Acceptance Criterion ("a session round-trips against a
//! reference peer and the fuzz target is green for 24 h") names two
//! things unreachable from this environment: no SCTP-speaking reference
//! peer is installed here, and this batch has no 24-hour window to run a
//! fuzz target in. Both are named rather than silently skipped, per the
//! owner's own ruling on replacement bars (`planning/AGENT-CONSTRAINTS.md`,
//! `705779d`): demonstrably-not-broken, structurally-correct,
//! deviation-named is the bar, not byte-exact agreement with an
//! unavailable oracle. The substitute actually built:
//! `tests/association_handshake.rs` drives two `Association`s (client and
//! server, this crate's own code on both sides) through the full
//! four-way handshake to `Established`, then a `DATA`/`SACK` exchange —
//! and the fuzz target ([`packet`]/[`chunk`] parsers over arbitrary bytes)
//! ran clean for a short smoke window instead of 24 hours, which is
//! disclosed as exactly that, not represented as the full 24-hour run.
//!
//! # How it works
//!
//! - [`packet`] — RFC 4960 §3.1's 12-byte common header and Appendix B's
//!   `CRC32c` checksum (`vaco_hash::crc32c`, D11 — not a new `crc`
//!   dependency here).
//! - [`chunk`] — the generic chunk header (§3.2) and the twelve chunk
//!   types above.
//! - [`association`] — the sans-io handshake/data-transfer state machine.

#![forbid(unsafe_code)]

pub mod association;
pub mod chunk;
pub mod packet;

pub use vaco_protocol_core::{ProtocolError, Result};
