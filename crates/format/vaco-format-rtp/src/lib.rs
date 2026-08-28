//! The RFC 4566 SDP parser, the RFC 3551 payload-type table, and the RTP
//! depacketisers.
//!
//! # What it is
//!
//! Layer 4 (`crates/format/`). [`sdp`] — RFC 4566 session descriptions,
//! which is what RTSP's `DESCRIBE` response negotiates and what
//! `vaco-demux-rtsp`'s `sdp:` demuxer reads directly from a `.sdp` file.
//!
//! [`rtp`]/[`rtcp`] are re-exported here unchanged from `vaco-rtp` (layer 1)
//! — see that crate's docs for why the RFC 3550 wire-format types moved
//! down a layer on 2026-08-28. This crate is still where you `use` them
//! from if you already depend on it; nothing downstream needed to change.
//!
//! [`payload`] is the RFC 3551 static payload-type table both depacketisers
//! and packetisers key off. [`depacket`] holds the depacketisers themselves
//! — see that module's docs for exactly which payload types are implemented
//! and which are not, and why.
//!
//! # Why this is one crate and not three
//!
//! `vaco-mux-rtp`'s packetisers and `vaco-demux-rtsp`'s depacketiser
//! selection both need the RTP header shape and the SDP grammar; RTSP
//! negotiates transport and payload types entirely in SDP. Splitting the
//! packet model from the parsers that need it would just be a second crate
//! boundary in the same dependency direction — D14.1's ban is on a
//! `vaco-format-*`/`vaco-demux-*` crate reaching into `vaco-parse-*` or
//! `vaco-registry`, not on format crates sharing a data model.
//!
//! # Security posture
//!
//! This crate never opens a socket — see `vaco-demux-rtsp`'s crate docs for
//! where the transport-security boundary actually lives (a server-supplied
//! `Transport:` port is untrusted input to *that* crate's whitelist gate,
//! not to this one). What this crate owns is the attacker-controlled *byte*
//! surface: header fields, extension lengths, SDP line lengths. Every parser
//! here takes a budget or a fixed-size input and returns `Result` rather
//! than indexing blindly.

#![forbid(unsafe_code)]

pub mod depacket;
pub mod payload;
pub mod sdp;

/// Re-exported from `vaco-rtp` (layer 1) unchanged — see the crate-level
/// docs above for why RFC 3550 framing lives one layer down now.
pub use vaco_rtp::rtcp;
/// Re-exported from `vaco-rtp` (layer 1) unchanged — see the crate-level
/// docs above for why RFC 3550 framing lives one layer down now.
pub use vaco_rtp::rtp;

pub use depacket::{Depacketizer, DepacketizerFactory, for_encoding};
pub use payload::{StaticPayload, static_payload};
pub use rtcp::{ReportBlock, RtcpPacket, SdesItem, SenderInfo};
pub use rtp::{RTP_VERSION, RtpHeader, RtpPacket};
pub use sdp::{Attribute, MediaDescription, SessionDescription};
