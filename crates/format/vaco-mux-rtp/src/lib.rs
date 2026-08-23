//! The RTP packetisers, and `rtp_mpegts`.
//!
//! # What it is
//!
//! Layer 4 (`crates/format/`). [`RtpMuxer`] is the registered `rtp` muxer
//! (`ffmpeg -h muxer=rtp`'s counterpart): one stream, packetised per
//! [`Packetizer`] and written to whatever [`vaco_io::MediaSink`] the caller
//! already opened (a `udp:` sink in the ordinary case — this crate never
//! opens a socket itself, since [`vaco_format_core::MuxerDesc::open`]'s
//! signature is exactly "here is your sink", nothing more, which is the
//! right shape for a muxer and means this crate has no transport-security
//! surface of its own: whatever opened the sink already went through
//! `vaco-protocol-core`'s whitelist gate).
//!
//! # `rtp_mpegts` — what is actually implemented
//!
//! `ffmpeg`'s `rtp_mpegts` muxer instantiates a private MPEG-TS muxer
//! internally and packs its output into RTP. `vaco_format_core::MuxerDesc`
//! has no seam for a muxer to depend on *another* muxer's implementation —
//! there is no `MuxerProvider` the way `ParserProvider` exists for
//! demuxers-needing-parsers, and inventing one is out of this crate's scope
//! (it would need `vaco-format-core` or `vaco-registry` changes this crate
//! does not own). [`rtp_mpegts::pack`] therefore implements only the
//! RTP-framing half — splitting a run of already-muxed 188-byte MPEG-TS
//! packets into MTU-sized RTP payloads, RFC 2250 §2's "MP2T" framing — and
//! [`RtpMpegtsMuxer`] is a real, registered, working muxer built on it, but
//! one that expects **already-muxed TS bytes** as its single stream's
//! packet payload (a caller runs `vaco-mux-mpegts` itself and feeds this
//! muxer its output, rather than this muxer running a nested one). This is
//! reported as a scope decision, not hidden: `docs/format/vaco-mux-rtp.md`
//! spells out exactly what a caller must do differently from
//! `ffmpeg -f rtp_mpegts` to get the same result.

#![forbid(unsafe_code)]

pub mod aac;
pub mod h264;
pub mod muxer;
pub mod raw;
pub mod registry;
pub mod rtp_mpegts;

pub use muxer::{MUXER, RtpMuxer};
pub use registry::{Packetizer, PacketizerFactory, packetizer_for};
pub use rtp_mpegts::{MUXER_RTP_MPEGTS, RtpMpegtsMuxer};
