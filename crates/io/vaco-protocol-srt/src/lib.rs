//! SRT (Secure Reliable Transport), native, from `draft-sharabayko-srt-01`.
//!
//! # What this package is (PR-10a / #555)
//!
//! Packet framing, the handshake state machine across all three modes
//! (caller, listener, rendezvous), and the KM (key material) message
//! *shape*. Like `vaco-protocol-rtmp`'s own PR-09a, this is a transport
//! library, not yet a [`vaco_protocol_core::Protocol`] — there is no
//! `srt:`/`srts:` registry entry here, no socket, and no cipher. Those
//! land in later packages once the socket/timer seam
//! (`planning/INTERFACE-GAPS.md` gap 28) and the AES/CTR ownership question
//! (deferred per dispatch) are both settled.
//!
//! # No reference implementation on this machine
//!
//! No `ffmpeg` build available here carries `libsrt` (`ffmpeg -protocols`
//! lists no `srt` entry). Every fact in this crate comes from
//! `draft-sharabayko-srt-01` (an IETF Internet-Draft, freely published, no
//! clean-room blocker) rather than a differential check, and this crate's
//! own tests are explicitly split into **draft-derived** (checked against
//! the draft's own worked field layouts and numeric tables, independent of
//! this crate's own encoder) and **self-consistency** (this crate's decoder
//! agrees with its own encoder — real evidence of internal consistency, but
//! not evidence either one matches the draft, since a shared misreading
//! would pass both sides of a round trip identically). Every test module's
//! doc comment says which it is.
//!
//! # How it works
//!
//! - [`packet`] — the common 16-byte header and the data/control packet
//!   framings built on it (`draft` §3).
//! - [`handshake`] — the Handshake control packet's CIF, its
//!   HSREQ/HSRSP/SID/... extension blocks, and the numeric constants for
//!   handshake type, encryption field and rejection reason (`draft` §3.2.1,
//!   Tables 4-7).
//! - [`km`] — the Key Material message *shape* (`draft` §3.2.2): parses and
//!   serializes every field, including the wrapped-key blob, without
//!   unwrapping it — the actual AES-CTR cipher is deferred to whichever
//!   crate ends up owning it (`planning/INTERFACE-GAPS.md`; not `ctr`
//!   claimed directly, not hand-rolled — see that gap's crypto-ownership
//!   note).
//! - [`cookie`] — the rendezvous cookie computation (`draft` §4.3.2): a
//!   32-bit value from host/port/time-with-one-minute-accuracy, scrambled
//!   through MD5, compared between peers to decide Initiator/Responder.
//! - [`session`] — the three handshake state machines (caller, listener,
//!   rendezvous), sans-io: each takes "a packet arrived" or "start" as
//!   input and returns "send this packet" / "connected" / "rejected" as
//!   output, driven by a caller that owns the actual socket (a later
//!   package).
//! - [`ack`] — the ACK/NAK control packets' CIF shapes (`draft` §3.2.4,
//!   §3.2.5). The field *layout* is draft-derived; the ACK stats fields
//!   (RTT and the three rate estimates) have no stated formula and are
//!   always zero, documented as such rather than guessed.
//! - [`arq`] (PR-10b / #556) — the retransmission engine: [`arq::SendWindow`]
//!   (buffer, NAK-triggered and RTO-triggered resend) and
//!   [`arq::ReceiveWindow`] (loss detection, in-order delivery, TSBPD-ish
//!   too-late drop). Sans-io via an explicit `on_tick(now_ms)`
//!   (`planning/INTERFACE-GAPS.md` gap 28's addendum) rather than owning a
//!   socket or a clock. **No congestion control / rate limiting is
//!   implemented** — `draft` §5.1/§5.2 name LiveCC/FileCC but do not give
//!   their algorithms in the fetched text, and `arq`'s own module docs
//!   name every other constant this module needed a number for and could
//!   not get from the draft (RTO, the too-late-drop threshold, NAK
//!   re-announcement policy) as `IMPLEMENTATION-DEFINED`, with reasoning,
//!   rather than presenting a guess as measured.
//!
//! # Configuration
//!
//! None yet — no `Protocol`, no `-h protocol=srt` options (matching
//! `vaco-protocol-rtmp`'s own PR-09a).
//!
//! # Dependencies
//!
//! `vaco-core`, `vaco-limits` (bounded parsing), `vaco-protocol-core`
//! (`ProtocolError`/`Result`, reused ahead of any `Protocol` impl exactly
//! as `vaco-protocol-rtmp` does), `vaco-time` (non-cryptographic seeding
//! and, later, the connection's own timestamp base). No crypto crate
//! dependency (D10 — deferred, see `planning/INTERFACE-GAPS.md`).

#![forbid(unsafe_code)]

pub mod ack;
pub mod arq;
pub mod cookie;
pub mod handshake;
pub mod km;
pub mod packet;
pub mod session;

pub use vaco_protocol_core::{ProtocolError, Result};
