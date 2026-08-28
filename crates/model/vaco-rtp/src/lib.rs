//! The RTP/RTCP wire-format model — RFC 3550 header parsing and building,
//! with nothing above it.
//!
//! # What it is
//!
//! Layer 1 (`crates/model/`). Just [`rtp`] (the RTP header and packet) and
//! [`rtcp`] (sender/receiver reports, SDES, `BYE`, and compound-packet
//! iteration) — the two RFC 3550 wire shapes, and nothing that needs a
//! format-layer or protocol-layer concept to make sense (no SDP, no
//! payload-type table, no depacketisers).
//!
//! # Why this crate exists (2026-08-28 extraction)
//!
//! Originally these two modules lived in `vaco-format-rtp` (layer 4). That
//! was fine while the only consumers were format-layer crates
//! (`vaco-demux-rtsp`, `vaco-mux-rtp`). It stopped being fine once a
//! layer-2 protocol crate needed the same RTP/RTCP framing: RIST's Simple
//! Profile (VSF TR-06-1) *is* RTP/RTCP on the wire, and `xtask/src/layers.rs`
//! requires every dependency edge to point downward — a layer-2 `io` crate
//! cannot depend on a layer-4 `format` crate. Reimplementing the same
//! RFC 3550 structs a second time inside the protocol crate was rejected:
//! `dup-check` (D19, one definition per concept) would have forced either a
//! name collision, a renamed-but-identical duplicate (worse), or a
//! `DISTINCT` exception papering over real duplication.
//!
//! So the wire-format types moved down to their own layer-1 crate, the same
//! shape as the `vaco-hash` extraction: one owner, at the lowest layer that
//! needs it, everyone else depends downward or re-exports.
//! `vaco-format-rtp` re-exports [`rtp`] and [`rtcp`] from here unchanged, so
//! nothing in `vaco-demux-rtsp` or `vaco-mux-rtp` had to change.
//!
//! # Security posture
//!
//! Every field here is attacker-controlled input (a hostile RTSP/RIST peer
//! chooses every header byte), so both modules are `#![forbid(unsafe_code)]`
//! and reject rather than panic on a malformed header.

#![forbid(unsafe_code)]

pub mod rtcp;
pub mod rtp;

pub use rtcp::{ReportBlock, RtcpPacket, SdesItem, SenderInfo};
pub use rtp::{RTP_VERSION, RtpHeader, RtpPacket};
