//! The `rtp:`/`rtcp:` protocol scheme — RFC 5761 multiplexing and a
//! sans-io sending session — issue #551, PR-08 (`rtp`, `srtp`, `prompeg`).
//!
//! # What this package is
//!
//! `rtp:`/`rtcp:` in the reference tool is a thin protocol-layer wrapper
//! around already-framed RTP/RTCP bytes ([`vaco_rtp`], layer 1, extracted
//! in this session for exactly this reason): the protocol's own job is
//! deciding *which stream* a buffer belongs to when RTP and RTCP share one
//! transport (RFC 5761, `rtcp-mux`), and giving a sender the small piece
//! of per-source state ([`session::SendSession`]'s sequence counter) that
//! a pure parse/build module has no reason to hold.
//!
//! **Deliberately not built: a URL option table.** `vaco-protocol-srt`'s
//! own `options.rs` names the reason this crate follows: without a
//! `librtp`-carrying `ffmpeg` build to measure `-h protocol=rtp` against,
//! reconstructing option names (`ttl`, `buffer_size`, `localrtpport`, ...)
//! from general knowledge of `rtpproto.c` would smuggle
//! implementation-specific detail in under a clean-room-derived label.
//! Nothing here needed one, so none was invented.
//!
//! # How it works
//!
//! - [`mux::is_rtcp_packet_type`]/[`mux::classify`] — RFC 5761 §4's
//!   reserved-range rule (packet types 192-223 are always RTCP,
//!   independent of RFC 3551's specific static/dynamic payload-type
//!   allocation below that range).
//! - [`demux::demux`] — classifies and parses one buffer in a single call,
//!   for a caller that does not want the two steps separately.
//! - [`session::SendSession`] — sequence-number bookkeeping for one SSRC.
//!   No socket, no clock: the caller supplies the RTP timestamp and reads
//!   the built bytes back out, the same shape `vaco-protocol-srt`/
//!   `vaco-protocol-rist` use at this layer.
//!
//! # What is not built (`prompeg`)
//!
//! `prompeg` (Pro-MPEG Code of Practice #3 FEC) is named in #551's own
//! scope but is **not built**: no D7/D15-clean primary source for the
//! COP3 FEC header's exact bit layout (`SNBase`, length-recovery, mask,
//! `X`/`D`/type/index/offset, `NA`/SNBase-extension fields) could be
//! located and cross-checked in the time this batch allowed — general
//! recollection of the byte layout was available but not independently
//! verifiable against a citable document, and this crate does not label
//! anything draft-derived that it cannot actually cite. Noted here rather
//! than silently dropped; see #551's closing comment for the same note.
//!
//! # Evidence
//!
//! RFC 5761 states a reserved-range rule, not a worked numeric example, so
//! [`mux`]'s tests are draft-derived (checked directly against the range
//! stated in the RFC text, boundaries included) rather than
//! RFC-vector-derived. [`session`]'s tests are self-consistency (this
//! crate's own packetizer output re-parsed by [`vaco_rtp::rtp::RtpPacket`]).

#![forbid(unsafe_code)]

pub mod demux;
pub mod mux;
pub mod session;

pub use demux::{Demuxed, PacketKind, demux};
pub use session::SendSession;
