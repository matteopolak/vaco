//! RFC 5761 §4: telling an RTP packet from an RTCP packet when both share
//! one transport (one UDP socket, one TCP-interleaved channel).
//!
//! (RFC-vector-derived is not applicable here — RFC 5761 states a
//! reserved-range rule, not a worked numeric example — so this module's
//! own tests are draft-derived: checked directly against the range the RFC
//! states, boundaries included.)

/// RFC 5761 §4's heuristic, applied to the **raw second octet**, marker
/// bit included — not the masked-out 7-bit RTP payload type.
///
/// RTCP's second octet is entirely a packet type field (SR=200, RR=201,
/// SDES=202, BYE=203, APP=204, and the wider 192-223 reserved block RFC
/// 5761 itself carves out for future RTCP types). RTP's second octet is
/// `marker<<7 | payload_type`, so a marker bit of 1 combined with a
/// 7-bit payload type in 64-95 produces exactly the same raw byte range
/// (192-223) as RTCP's packet types — a genuine ambiguity RFC 5761 does
/// not resolve by inspection; instead it directs implementations not to
/// negotiate a *dynamic* RTP payload type in 64-95 when the two are
/// muxed, so that the raw-byte range 192-223 is unambiguously RTCP in
/// practice. This function implements exactly that convention: it looks
/// at the whole byte, not a masked 7-bit field, because that whole byte
/// is what the convention is defined in terms of.
#[must_use]
pub const fn is_rtcp_packet_type(second_octet: u8) -> bool {
    second_octet >= 192 && second_octet <= 223
}

/// Look at the leading bytes of one datagram / interleaved frame and
/// decide which of [`crate::demux::PacketKind`] it is, per
/// [`is_rtcp_packet_type`].
///
/// Returns `None` if `buf` is too short to contain even the second octet
/// — too short to be either kind, so this module declines to guess.
#[must_use]
pub fn classify(buf: &[u8]) -> Option<crate::demux::PacketKind> {
    let second = *buf.get(1)?;
    Some(if is_rtcp_packet_type(second) {
        crate::demux::PacketKind::Rtcp
    } else {
        crate::demux::PacketKind::Rtp
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_type_below_the_rtcp_range_is_rtp() {
        // RFC 3551 §6's static PT 0 (PCMU), unaffected by the marker bit.
        assert!(!is_rtcp_packet_type(0));
        assert!(!is_rtcp_packet_type(0x80)); // marker set, PT still 0
    }

    #[test]
    fn boundaries_of_the_rtcp_range_are_inclusive() {
        assert!(!is_rtcp_packet_type(191));
        assert!(is_rtcp_packet_type(192));
        assert!(is_rtcp_packet_type(223));
        assert!(!is_rtcp_packet_type(224));
    }

    #[test]
    fn rtcp_sr_and_rr_packet_types_classify_as_rtcp() {
        // RFC 3550's own SR=200, RR=201, SDES=202, BYE=203, APP=204 — all
        // inside RFC 5761's widened 192-223 range.
        for pt in [200u8, 201, 202, 203, 204] {
            assert!(is_rtcp_packet_type(pt), "PT {pt} should be RTCP");
        }
    }

    #[test]
    fn classify_reads_the_second_octet_not_the_first() {
        // first octet: version 2, no padding/extension, 0 CSRC (0x80);
        // second octet: marker + PT 96 (dynamic, RTP).
        assert_eq!(classify(&[0x80, 0xE0]), Some(crate::demux::PacketKind::Rtp));
        // second octet: PT 200 (SR) -> RTCP.
        assert_eq!(classify(&[0x80, 200]), Some(crate::demux::PacketKind::Rtcp));
    }

    #[test]
    fn too_short_to_classify_returns_none() {
        assert_eq!(classify(&[0x80]), None);
        assert_eq!(classify(&[]), None);
    }
}
