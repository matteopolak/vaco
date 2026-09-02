//! Building our own SDP offer, and reading what a WHIP answer must carry.
//!
//! # Measured shape (D17), not assumed
//!
//! The offer format below matches `ffmpeg 9.0.1`'s own WHIP muxer output,
//! captured directly off the wire (a loopback HTTP server standing in for
//! the WHIP endpoint, `Vaco-Provenance: blackbox` in spirit — see
//! `docs/format/vaco-mux-whip.md` for the transcript): `a=setup:active` in
//! the offer, no `a=candidate` lines at all (this crate's answerer,
//! `mediamtx` 1.20.1, supplies its own candidates in the answer and accepts
//! ours arriving implicitly via the ICE connectivity check's source
//! address — "vanilla" non-trickle ICE with the client offering none of its
//! own). `a=setup:active` was chosen deliberately over the more common
//! `a=setup:actpass`: it pins *us* as the DTLS client, which is the well-
//! tested `vaco-protocol-dtls::connect` path rather than its `listen` path
//! — confirmed against a real `mediamtx` instance to elicit `a=setup:passive`
//! in the answer, satisfying RFC 5763 §5's "not both active" rule.

use std::fmt::Write as _;

use vaco_format_rtp::sdp::{MediaDescription, SessionDescription, parse as parse_sdp};

/// One stream's worth of the offer's `m=` block.
#[derive(Debug, Clone)]
pub struct MediaOffer {
    /// `video` or `audio` (RFC 4566 §5.14's vocabulary).
    pub kind: &'static str,
    pub payload_type: u8,
    /// `H264`, `opus`, ... (RFC 3551/the RTP payload registry's names).
    pub encoding_name: &'static str,
    pub clock_rate: u32,
    /// Extra `a=fmtp` parameters, already `key=value` joined by `;` — empty
    /// for a codec with none (Opus needs none for a basic publish).
    pub fmtp: String,
    pub ssrc: u32,
    pub mid: u32,
}

/// Build one SDP offer (RFC 4566, the WHIP-specific parts per
/// `draft-ietf-wish-whip` §4.1): one `m=` block per `media`, all under one
/// `BUNDLE` group, `a=sendonly` (this crate never receives media back).
#[must_use]
pub fn build_offer(
    local_ufrag: &str,
    local_pwd: &str,
    local_fingerprint: &str,
    media: &[MediaOffer],
) -> String {
    let mut s = String::new();
    s.push_str("v=0\r\n");
    s.push_str("o=- 0 0 IN IP4 0.0.0.0\r\n");
    s.push_str("s=-\r\n");
    s.push_str("t=0 0\r\n");
    let mids: Vec<String> = media.iter().map(|m| m.mid.to_string()).collect();
    let _ = writeln!(s, "a=group:BUNDLE {}\r", mids.join(" "));

    for m in media {
        let _ = writeln!(
            s,
            "m={kind} 9 UDP/TLS/RTP/SAVPF {pt}\r",
            kind = m.kind,
            pt = m.payload_type
        );
        s.push_str("c=IN IP4 0.0.0.0\r\n");
        let _ = writeln!(s, "a=ice-ufrag:{local_ufrag}\r");
        let _ = writeln!(s, "a=ice-pwd:{local_pwd}\r");
        let _ = writeln!(s, "a=fingerprint:sha-256 {local_fingerprint}\r");
        // We are always the DTLS client — see the module docs for why.
        s.push_str("a=setup:active\r\n");
        let _ = writeln!(s, "a=mid:{}\r", m.mid);
        s.push_str("a=sendonly\r\n");
        s.push_str("a=rtcp-mux\r\n");
        let _ = writeln!(
            s,
            "a=rtpmap:{pt} {name}/{rate}\r",
            pt = m.payload_type,
            name = m.encoding_name,
            rate = m.clock_rate
        );
        if !m.fmtp.is_empty() {
            let _ = writeln!(
                s,
                "a=fmtp:{pt} {fmtp}\r",
                pt = m.payload_type,
                fmtp = m.fmtp
            );
        }
        let _ = writeln!(s, "a=ssrc:{ssrc} cname:vaco\r", ssrc = m.ssrc);
    }
    s
}

/// What one negotiated media block needs from the answer.
#[derive(Debug, Clone)]
pub struct AnsweredMedia {
    pub mid: String,
    pub ice_ufrag: String,
    pub ice_pwd: String,
    /// Lower-case hex, no colons — normalised once here so every later
    /// comparison (the peer certificate check) is a plain string match.
    pub fingerprint: String,
    pub setup: String,
    pub candidates: Vec<String>,
}

/// Parse a WHIP SDP answer and pull out, per `m=` block, everything needed
/// to run ICE and DTLS: ICE credentials and `a=fingerprint`/`a=setup`
/// (media-level if present, else the session-level fallback BUNDLE uses),
/// and every `a=candidate` line, left as raw strings for
/// [`crate::candidate::parse`].
///
/// # Errors
/// [`vaco_core::Error::InvalidData`] if the body does not parse as SDP at
/// all, or names no media block.
pub fn parse_answer(body: &str) -> vaco_core::Result<Vec<AnsweredMedia>> {
    let session = parse_sdp(body)?;
    if session.media.is_empty() {
        return Err(vaco_core::Error::InvalidData(
            "WHIP answer names no media block",
        ));
    }
    let mut out = Vec::new();
    for m in &session.media {
        let ice_ufrag = attr_or_session(m, &session, "ice-ufrag")
            .ok_or(vaco_core::Error::InvalidData(
                "WHIP answer is missing ice-ufrag",
            ))?
            .to_owned();
        let ice_pwd = attr_or_session(m, &session, "ice-pwd")
            .ok_or(vaco_core::Error::InvalidData(
                "WHIP answer is missing ice-pwd",
            ))?
            .to_owned();
        let fingerprint_raw = attr_or_session(m, &session, "fingerprint").ok_or(
            vaco_core::Error::InvalidData("WHIP answer is missing a=fingerprint"),
        )?;
        // `a=fingerprint:sha-256 AB:CD:...` — keep only the hex, lower-cased,
        // colons stripped, so `crate::muxer`'s comparison against a freshly
        // computed digest is a plain equality.
        let fingerprint = fingerprint_raw
            .split_whitespace()
            .next_back()
            .unwrap_or_default()
            .replace(':', "")
            .to_ascii_lowercase();
        let setup = attr_or_session(m, &session, "setup")
            .unwrap_or_default()
            .to_owned();
        let mid = m.attr("mid").unwrap_or_default().to_owned();
        let candidates = m
            .attrs("candidate")
            .map(std::borrow::ToOwned::to_owned)
            .collect();
        out.push(AnsweredMedia {
            mid,
            ice_ufrag,
            ice_pwd,
            fingerprint,
            setup,
            candidates,
        });
    }
    Ok(out)
}

/// A media-level attribute if present, else the session-level one (BUNDLE
/// semantics: several real WHIP answers, `mediamtx` 1.20.1 among them, only
/// state `a=fingerprint` once, at the session level).
fn attr_or_session<'a>(
    media: &'a MediaDescription,
    session: &'a SessionDescription,
    name: &str,
) -> Option<&'a str> {
    media.attr(name).or_else(|| {
        session
            .attributes
            .iter()
            .find(|a| a.is(name))
            .and_then(|a| a.value.as_deref())
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    const REAL_MEDIAMTX_ANSWER: &str = "v=0\r\no=- 3339082891111356261 1787970802 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\na=msid-semantic:WMS *\r\na=fingerprint:sha-256 92:C4:68:A7:58:1C:02:7F:E2:B1:47:77:99:D9:5B:61:73:CB:75:74:97:27:95:A3:88:C5:2C:9F:B4:33:23:D4\r\na=group:BUNDLE 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=setup:passive\r\na=mid:0\r\na=ice-ufrag:dYsNaPLZkyMxTGpF\r\na=ice-pwd:atkfqKRUikvEunYBFrrDSAWyGkceQbnk\r\na=rtcp-mux\r\na=rtcp-rsize\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\na=recvonly\r\na=candidate:2878742611 1 udp 2130706431 127.0.0.1 8189 typ host ufrag dYsNaPLZkyMxTGpF\r\na=candidate:1224104489 1 udp 2130706431 192.168.2.63 8189 typ host ufrag dYsNaPLZkyMxTGpF\r\na=end-of-candidates\r\n";

    #[test]
    fn parses_a_real_mediamtx_answer() {
        let out = parse_answer(REAL_MEDIAMTX_ANSWER).unwrap();
        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!(m.ice_ufrag, "dYsNaPLZkyMxTGpF");
        assert_eq!(m.ice_pwd, "atkfqKRUikvEunYBFrrDSAWyGkceQbnk");
        assert_eq!(m.setup, "passive");
        assert_eq!(
            m.fingerprint,
            "92c468a7581c027fe2b1477799d95b6173cb7574972795a388c52c9fb43323d4"
        );
        assert_eq!(m.candidates.len(), 2);
    }

    #[test]
    fn build_offer_includes_every_stream() {
        let offer = build_offer(
            "locu",
            "locpwd0000000000000000",
            "AA:BB",
            &[MediaOffer {
                kind: "video",
                payload_type: 96,
                encoding_name: "H264",
                clock_rate: 90_000,
                fmtp: "packetization-mode=1".to_owned(),
                ssrc: 1234,
                mid: 0,
            }],
        );
        assert!(offer.contains("a=setup:active"));
        assert!(offer.contains("m=video 9 UDP/TLS/RTP/SAVPF 96"));
        assert!(offer.contains("a=ssrc:1234 cname:vaco"));
    }

    #[test]
    fn rejects_an_answer_with_no_media() {
        assert!(parse_answer("v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n").is_err());
    }

    #[test]
    fn rejects_an_answer_with_no_fingerprint() {
        let bad = "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=ice-ufrag:x\r\na=ice-pwd:y\r\n";
        assert!(parse_answer(bad).is_err());
    }
}
