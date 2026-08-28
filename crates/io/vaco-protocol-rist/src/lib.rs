//! RIST (Reliable Internet Stream Transport) Simple and Main Profile,
//! native, from VSF `TR-06-1:2020` and `TR-06-2:2022`.
//!
//! # What this package is (PR-11a / #558, PR-11b / #559)
//!
//! Simple Profile (#558): RTP/RTCP framing (reused from `vaco-rtp`, not
//! reimplemented — see `docs/model/vaco-rtp.md` for why RTP/RTCP moved to
//! layer 1), the RIST-specific RTCP messages Simple Profile adds on top
//! (bitmask/range retransmission requests, the optional RTT-echo
//! message), retransmitted-packet matching, and the receiver's
//! two-section reorder/retransmission buffer.
//!
//! Main Profile (#559): GRE tunnelling ([`gre`], [`keepalive`]) and
//! Pre-Shared Key encryption ([`psk`]). §6's DTLS/certificate path is
//! **named blocked and deferred to PR-12** — the same T4-tiered native
//! DTLS gap already blocking WHIP (#619); `rustls` has no DTLS, and this
//! is not a fresh finding, just the second package to hit the same one.
//! §7.5's PSK authentication points to Annex D, a **Normative, full
//! EAP-SHA256-SRP6a implementation** (2048-bit safe-prime modular
//! exponentiation, its own multi-message challenge/response, its own
//! session-key derivation) — genuinely comparable in size to the
//! AES-ownership question, not a wiring detail, so it is **filed
//! separately as #657** rather than built here. This is a legitimate
//! split, not a shortcut: §7 itself states PSK's intrinsic
//! authentication (knowledge of the passphrase) "may be sufficient for
//! some applications", naming Annex D as an *additional* level on top.
//!
//! Like `vaco-protocol-srt`'s PR-10a, this is a framing/state library,
//! not yet a [`vaco_protocol_core::Protocol`] — no `rist:` registry
//! entry, no socket.
//!
//! Bonding and the statistics surface (#560): §5.4/§5.5's multi-link
//! replication and combining ([`bonding`]) and this crate's own
//! statistics surface ([`stats`]) — neither profile names a required
//! statistics API, so [`stats`] is this crate's own choice of what to
//! expose, not a spec-mandated shape. #560's own interop-matrix clause is
//! **named unreachable up front, before implementation, the same way
//! #557-#559 named their own reference-peer requirements unreachable**:
//! there is no `librist` build on this machine (see "No reference
//! implementation" below), so no cross-implementation matrix can be run.
//! The replacement bar actually built against instead: *a bonded
//! two-link session survives the loss of either link with no
//! delivered-packet loss* — #560's own Acceptance Criterion, exercised
//! directly by [`bonding`]'s own tests.
//!
//! # No reference implementation on this machine
//!
//! No `ffmpeg` build available here carries `librist` (`ffmpeg -protocols`
//! / `ffmpeg -buildconf` list no `rist` entry). Every fact in this crate
//! comes from `VSF TR-06-1:2020`/`TR-06-2:2022` (freely published
//! Technical Recommendations, CC BY-ND 4.0 — D7/D15-clean the same way
//! the SRT draft was) rather than a differential check.
//!
//! Tests carry **three** evidence-class labels, not two:
//! - **RFC-vector-derived** — checked against a published RFC's own
//!   numeric test vectors (genuinely independent of this crate's own
//!   code). Used in `vaco-crypto` (RFC 3686's AES-CTR vectors, RFC 7914's
//!   PBKDF2-HMAC-SHA256 vectors) — this crate's own `gre`/`keepalive`/
//!   `psk` modules build on those without re-deriving them.
//! - **draft-derived** — checked against `TR-06-1`/`TR-06-2`'s own worked
//!   field layouts, tables, and numeric examples (Appendix A's
//!   retransmission-request scenario, Appendix B's PBKDF2 key-derivation
//!   example — the latter independently re-derived via Python's stdlib
//!   `hashlib.pbkdf2_hmac` before being trusted, not merely read off the
//!   page).
//! - **self-consistency** — this crate's own two sides agreeing (a fake
//!   sender/receiver pair completing a session), real evidence of
//!   internal consistency but not of spec conformance.
//!
//! **Patent posture (D4).** TR-06-1's IPR notice claims a patent
//! ("IPR") over §4, 5, 5.1 (excl. 5.1.2), 5.2, 5.3 and sub-sections, and
//! 5.4 — essentially all of Simple Profile's substantive operation — held
//! by Video-Flow Ltd, with an assurance to license to any implementer who
//! asks. That is not absence of encumbrance (`planning/00-decisions.md`
//! D4's 2026-08-28 amendment). This crate is not "in the published build"
//! today in D4's sense — it has no `vaco-component.toml` fragment and
//! nothing links it into `vaco-cli` — but **the moment it is registered,
//! the fragment must set `encumbered = true` behind
//! `patent-encumbered-rist`, `default = false`.** Do not register it
//! without that flag.
//!
//! # How it works
//!
//! - [`rtcp`] — the RIST-specific RTCP messages `TR-06-1` §5.2/§5.3 add:
//!   [`rtcp::RttEcho`] (the optional §5.2.6 RTT-echo `APP` message),
//!   [`rtcp::GenericNack`] (the §5.3.2.1 bitmask retransmission request,
//!   RFC 4585's own Generic NACK FCI shape), and [`rtcp::RangeNack`] (the
//!   §5.3.2.2 range retransmission request, a RIST-specific `APP`
//!   message). Plain SR/RR/SDES/BYE are `vaco_rtp::rtcp` types directly,
//!   reached the same way any other unrecognised payload type is —
//!   through [`vaco_rtp::rtcp::RtcpPacket::Other`]'s `count_or_fmt` field
//!   (added alongside this crate, since RIST is the first consumer that
//!   needs the `APP` subtype / FB `FMT` bits `vaco-rtp` itself does not
//!   interpret).
//! - [`retransmit`] — §5.3.3's SSRC-LSB tagging (`0` = original, `1` =
//!   retransmission; the remaining 31 bits identify the flow).
//! - [`buffer`] — §5.3.1's two-section receiver buffer (Figure 1: a
//!   Reorder Section feeding a Retransmission Reassembly Section, loss
//!   detected at the boundary between them, no recovery past the far
//!   end). Sans-io, the same shape as `vaco-protocol-srt::arq`: driven by
//!   `on_packet(seq, payload, now_ms)`/`on_tick(now_ms)`, nothing owns a
//!   socket or a clock. Appendix B's suggested defaults (1000 ms total
//!   buffer, 70 ms reorder section) are **informative, not normative** —
//!   marked `IMPLEMENTATION-DEFINED` at the point of declaration rather
//!   than presented as required values.
//! - [`gre`] (#559) — the GRE-over-UDP tunnel header (`TR-06-2` §5.1
//!   `Fig. 1/2`, RFC 8086/2890's `C`/`K`/`S` flags plus the `H`/`RV` bits
//!   RIST carves out of `Reserved0`), the VSF Packet Header (§5.2
//!   `Fig. 3`), and Reduced Overhead Mode's own header (§5.3.2 `Fig. 5`).
//!   Full Datagram Mode's IP-packet payload and the JSON Keep-Alive
//!   payload are both carried as opaque bytes deliberately — see the
//!   module's own docs for why.
//! - [`keepalive`] (#559) — the Keep-Alive message (§5.6.3/§5.6.4
//!   `Fig. 8`): the 48-bit MAC address and the thirteen named capability
//!   flags (including `D`/`T`, which the spec overloads onto the same
//!   message to mean Disconnect/Reconnect — §5.6.5/§5.6.6). The JSON
//!   payload itself is opaque bytes, not parsed (see the module docs).
//! - [`psk`] (#559) — §7.1-7.4's Pre-Shared Key encryption: key
//!   derivation (§7.3's PBKDF2-HMAC-SHA256, 1024 iterations, the nonce as
//!   salt — built on `vaco-crypto`, checked against `TR-06-2` Annex B's
//!   own worked example there) and the IV construction (§7.2: the
//!   sequence number as the counter block's high 4 bytes). §7.4's
//!   passphrase-rotation policy and §7.6's Future Nonce Announcement are
//!   not built — see the module's own docs on why.
//! - [`bonding`] (#560) — §5.4 (Simple Profile: bonding across raw
//!   network connections) and §5.5 (Main Profile: tunnel-level
//!   multi-path over GRE paths) are one mechanism at this crate's level
//!   of abstraction, not two: [`bonding::BondedReceiver`] is a thin
//!   wrapper over [`buffer::ReceiveBuffer`] that adds only the one thing
//!   the buffer does not already track — which link an arrival came in
//!   on. Deduplication of replicated copies falls out of
//!   [`buffer::ReceiveBuffer`]'s existing sequence-number keying with no
//!   new logic, per §5.4's own requirement that replicated copies "shall
//!   have the same RTP sequence number and timestamp".
//! - [`stats`] (#560) — a small statistics surface, not a spec-mandated
//!   one. Counters are labelled **independently-computed**
//!   ([`stats::SessionStats::total_accounted_for`], checked in its own
//!   test against a total the test itself derives separately) versus
//!   **merely-reported** ([`stats::SessionStats::packets_delivered`]/
//!   [`stats::SessionStats::packets_dropped`], read straight off
//!   [`buffer::BufferEvent`]) — the same distinction
//!   `vaco-protocol-srt`'s PR-10c stats module drew.
//!
//! # What is not verified
//!
//! No interop — see above, and #560's interop-matrix clause specifically:
//! no `librist` build exists on this machine, so it is named unreachable
//! rather than attempted, with the Acceptance Criterion itself (bonded
//! two-link survival) built and tested as the replacement bar instead.
//! §5.1's port-assignment rules (unicast/ multicast, NAT firewall
//! interaction) are socket/deployment concerns with nothing to unit-test
//! at this layer; they are documented, not coded. §5.3.4/§5.3.5 (burst
//! control, SSRC filtering) are explicitly informative in the spec itself
//! ("details... left to the discretion of the implementer") and are not
//! built as a result — there is nothing normative to conform to. §6
//! (DTLS) and Annex D (EAP-SHA256-SRP6a) are named blocked/deferred
//! above, not attempted.
//!
//! # Configuration
//!
//! None yet — no `Protocol`, no `-h protocol=rist` options.
//!
//! # Dependencies
//!
//! `vaco-core`, `vaco-crypto` (layer 0 — AES-CTR and PBKDF2-HMAC-SHA256,
//! not duplicated), `vaco-limits` (bounded parsing), `vaco-protocol-core`
//! (`ProtocolError`/`Result`, reused ahead of any `Protocol` impl exactly
//! as `vaco-protocol-srt` does), `vaco-rtp` (layer 1 — RFC 3550 RTP/RTCP
//! framing, not duplicated), `vaco-time`.

#![forbid(unsafe_code)]

pub mod bonding;
pub mod buffer;
pub mod gre;
pub mod keepalive;
pub mod psk;
pub mod retransmit;
pub mod rtcp;
pub mod stats;

pub use vaco_protocol_core::{ProtocolError, Result};
