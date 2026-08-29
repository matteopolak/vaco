//! The `whip` muxer (#619): WHIP — WebRTC-HTTP Ingestion Protocol
//! (`draft-ietf-wish-whip`) — publish.
//!
//! # What it is
//!
//! A `Muxer` that turns a media stream into SRTP-protected RTP and sends it
//! to a WHIP endpoint: one HTTP `POST` carrying an SDP offer, an SDP answer
//! back, an ICE connectivity check, a DTLS handshake, and then packets. It
//! is the first muxer in this tree that needs a network *negotiation*
//! before any byte-oriented sink exists — see [`crate::muxer`]'s module docs
//! for how it fits `vaco-format-core`'s existing `Muxer` trait without
//! changing it, and `docs/format/vaco-mux-whip.md` for the design writeup.
//!
//! # How it works
//!
//! | Module | Job |
//! |---|---|
//! | [`sdp`] | Building our own SDP offer and reading the fields a WHIP answer must carry (ICE credentials, DTLS fingerprint, `setup`, candidates). |
//! | [`candidate`] | RFC 8839 `a=candidate` line parsing, host/srflx only. |
//! | [`http`] | A minimal one-shot HTTP/1.1 client (`http://` only — see its docs) for the WHIP POST/PATCH/DELETE exchange, built on `vaco-protocol-socket`'s TCP connect rather than declaring `ureq` a second time (D11: `vaco-protocol-http` alone owns it). |
//! | [`muxer`] | [`muxer::WhipMuxer`], the registered `whip` muxer. |
//!
//! # Security
//!
//! DTLS verification is never weakened to make a handshake succeed:
//! `-verify` stays off (matching the reference and the whole WebRTC identity
//! model, which authenticates via a fingerprint instead of a CA chain — see
//! `vaco-protocol-dtls::cert`'s own docs), but this crate checks the peer's
//! *actual* certificate fingerprint against the one the SDP answer promised,
//! and refuses the connection on a mismatch (`muxer::verify_peer_fingerprint`).
//! ICE's `MESSAGE-INTEGRITY` is checked too, by `vaco-protocol-ice`. Neither
//! check is a formality: both reject a wrong answer, proven by tests in
//! their respective crates.
//!
//! Only ICE/STUN and DTLS/SRTP against the WHIP endpoint itself are ever
//! contacted — no other host is probed, enumerated or scanned.
//!
//! # What is deliberately not implemented
//!
//! `https://` WHIP endpoints (signalling only — the media path is always
//! SRTP-encrypted regardless); trickle ICE (`PATCH` with
//! `application/trickle-ice-sdpfrag`) and server-reflexive/relay candidates
//! (TURN) — every WHIP endpoint measured so far (`mediamtx` 1.20.1)
//! publishes reachable host candidates directly in the answer, non-trickled,
//! matching real `ffmpeg 9.0.1`'s own WHIP client behaviour observed the
//! same way; responding to a peer-initiated STUN Binding Request (RFC 7675
//! consent freshness) once the session is established; and RTCP receiver
//! reports (send-only, matching `a=sendonly` in the offer). See
//! `docs/format/vaco-mux-whip.md` for the full list and why each is safe to
//! cut for a first, real, working publish path.

#![forbid(unsafe_code)]

pub mod candidate;
pub mod http;
pub mod muxer;
pub mod sdp;

pub use muxer::{MUXER, WhipMuxer};
