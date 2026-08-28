//! RIST (Reliable Internet Stream Transport) Simple Profile, native, from
//! VSF `TR-06-1:2020`.
//!
//! # What this package is (PR-11a / #558)
//!
//! RTP/RTCP framing (reused from `vaco-rtp`, not reimplemented — see
//! `docs/model/vaco-rtp.md` for why RTP/RTCP moved to layer 1), the
//! RIST-specific RTCP messages Simple Profile adds on top (bitmask/range
//! retransmission requests, the optional RTT-echo message), retransmitted-
//! packet matching, and the receiver's two-section reorder/retransmission
//! buffer. Like `vaco-protocol-srt`'s PR-10a, this is a framing/state
//! library, not yet a [`vaco_protocol_core::Protocol`] — no `rist:`
//! registry entry, no socket. GRE tunnelling, DTLS/PSK encryption and
//! authentication (Main Profile, TR-06-2) are #559; bonding, the
//! statistics surface and the interop matrix are #560.
//!
//! # No reference implementation on this machine
//!
//! No `ffmpeg` build available here carries `librist` (`ffmpeg -protocols`
//! / `ffmpeg -buildconf` list no `rist` entry). Every fact in this crate
//! comes from `VSF TR-06-1:2020` (a freely published Technical
//! Recommendation, CC BY-ND 4.0 — D7/D15-clean the same way the SRT
//! draft was) rather than a differential check. As with
//! `vaco-protocol-srt`, tests are labelled **draft-derived** (checked
//! against the spec's own worked field layouts, tables, and Appendix A's
//! numeric example) or **self-consistency** (this crate's own two sides
//! agreeing, which is real evidence of internal consistency but not of
//! spec conformance).
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
//!
//! # What is not verified
//!
//! No interop — see above. §5.1's port-assignment rules (unicast/
//! multicast, NAT firewall interaction) are socket/deployment concerns
//! with nothing to unit-test at this layer; they are documented, not
//! coded. §5.3.4/§5.3.5 (burst control, SSRC filtering) are explicitly
//! informative in the spec itself ("details... left to the discretion of
//! the implementer") and are not built as a result — there is nothing
//! normative to conform to.
//!
//! # Configuration
//!
//! None yet — no `Protocol`, no `-h protocol=rist` options.
//!
//! # Dependencies
//!
//! `vaco-core`, `vaco-limits` (bounded parsing), `vaco-protocol-core`
//! (`ProtocolError`/`Result`, reused ahead of any `Protocol` impl exactly
//! as `vaco-protocol-srt` does), `vaco-rtp` (layer 1 — RFC 3550 RTP/RTCP
//! framing, not duplicated), `vaco-time`.

#![forbid(unsafe_code)]

pub mod buffer;
pub mod retransmit;
pub mod rtcp;

pub use vaco_protocol_core::{ProtocolError, Result};
