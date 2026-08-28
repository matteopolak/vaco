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
//! **An interop matrix — this or any package's own children running
//! against a real SRT peer in both directions, at every mode — is named
//! here as unreachable, not attempted, and not replaced with a
//! self-hosted look-alike.** Loss recovery (#556) and handshake
//! completion (#555) have meaningful self-hosted substitutes because both
//! are close to properties of a pair's own internal consistency; interop
//! is not — a matrix against this crate's own two implementations would
//! prove only that they agree with each other, which every other test in
//! this crate already establishes more directly.
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
//!   rather than presenting a guess as measured. **This is a real,
//!   separately-tracked functional gap** (issue #656), not merely the
//!   same specification gap stated twice: a real deployment over a real
//!   network needs some throttling regardless of whether it matches
//!   SRT's own named algorithms, which `arq`'s own tests (a simulated
//!   lossy but otherwise-instant link) cannot exercise.
//! - [`message`] (PR-10c / #557) — [`message::TransmissionMode`] (this
//!   crate's own reading of the `STREAM` `SRT Flags` bit — stated as an
//!   inference, not re-labeled draft-derived, since the fetched text names
//!   the bit without giving its semantics) and [`message::MessageReassembler`]
//!   (message-mode-only: groups `on_tick`-delivered packets by
//!   `msg_no`/`PacketPosition` into whole messages, all-or-nothing if any
//!   constituent packet is too-late-dropped).
//! - [`options`] (PR-10c / #557) — [`options::SrtOptions`], deliberately
//!   scoped to exactly the knobs this crate's own code reads
//!   (transmission mode, `latency_ms`, `rto_ms`) rather than reconstructing
//!   an `ffmpeg -h protocol=srt`-shaped table from general SRT knowledge
//!   this crate has no `libsrt` build to measure against — see that
//!   module's own docs.
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
pub mod message;
pub mod options;
pub mod packet;
pub mod session;

pub use vaco_protocol_core::{ProtocolError, Result};
