//! RFC 4566 (SDP) and the offer/answer-adjacent bits of RFC 3264 that RTSP's
//! `DESCRIBE`/`SETUP` exchange actually uses.
//!
//! `vaco-demux-rtsp` reads a [`SessionDescription`] to decide, per
//! [`MediaDescription`]: which transport to `SETUP` (`m=<media> <port>
//! RTP/AVP <fmt>...`), which `a=control` URL identifies the track, and which
//! depacketiser to hand payloads to (`a=rtpmap`/`a=fmtp`, resolved through
//! `crate::depacket::for_media`).

mod parse;
mod types;

pub use parse::parse;
pub use types::{
    Attribute, Connection, MediaDescription, Origin, RtpMap, SessionDescription, fmtp_params,
    parse_fmtp, parse_rtpmap,
};
