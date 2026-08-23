//! The shared RTP/RTCP packet model and the SDP parser.
//!
//! # What it is
//!
//! Layer 4 (`crates/format/`). Two things every RTP-adjacent crate needs and
//! neither should duplicate:
//!
//! 1. [`rtp`]/[`rtcp`] — RFC 3550 packet parsing and building, over
//!    attacker-controlled bytes. Every field of an RTP header is untrusted
//!    input (a hostile RTSP server chooses every byte a `PLAY` response
//!    starts streaming), so both modules are `#![forbid(unsafe_code)]` and
//!    reject rather than panic on a malformed header — an RTP packet
//!    claiming 15 CSRCs in a 12-byte buffer is refused, not indexed into.
//! 2. [`sdp`] — RFC 4566 session descriptions, which is what RTSP's
//!    `DESCRIBE` response negotiates and what `vaco-demux-rtsp`'s `sdp:`
//!    demuxer reads directly from a `.sdp` file.
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
pub mod rtcp;
pub mod rtp;
pub mod sdp;

pub use depacket::{Depacketizer, DepacketizerFactory, for_encoding};
pub use payload::{StaticPayload, static_payload};
pub use rtcp::{ReportBlock, RtcpPacket, SdesItem, SenderInfo};
pub use rtp::{RTP_VERSION, RtpHeader, RtpPacket};
pub use sdp::{Attribute, MediaDescription, SessionDescription};
