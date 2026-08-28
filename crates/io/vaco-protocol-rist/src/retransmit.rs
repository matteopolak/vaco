//! §5.3.3 Retransmitted Packets — `VSF TR-06-1:2020`.
//!
//! A retransmitted packet is sent "using the same transmission method as
//! the other packets from this flow": same RTP sequence number, same
//! timestamp, same 31 high bits of SSRC, differing only in SSRC's least
//! significant bit, which the spec defines as the original/retransmission
//! flag. This module is that one bit's read/write side — nothing else
//! about a retransmitted packet's framing changes, so there is no new
//! packet type here, only a tag on the SSRC field of the RTP packet
//! `vaco_rtp::rtp::RtpPacket` already models.

/// §5.3.3's SSRC LSB tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// SSRC LSB = 0.
    Original,
    /// SSRC LSB = 1.
    Retransmission,
}

impl Origin {
    #[must_use]
    pub const fn from_ssrc(ssrc: u32) -> Self {
        if ssrc & 1 == 0 {
            Self::Original
        } else {
            Self::Retransmission
        }
    }

    /// Set this origin's LSB on `ssrc`, leaving the other 31 bits (the
    /// flow identity) untouched.
    #[must_use]
    pub const fn tag(self, ssrc: u32) -> u32 {
        let base = ssrc & !1;
        match self {
            Self::Original => base,
            Self::Retransmission => base | 1,
        }
    }
}

/// The 31-bit flow identity shared between an original packet and its
/// retransmission — `ssrc` with the origin bit masked off. Two packets
/// with the same [`flow_id`] and the same RTP sequence number are the
/// same logical packet, original or retransmitted.
#[must_use]
pub const fn flow_id(ssrc: u32) -> u32 {
    ssrc & !1
}

#[cfg(test)]
mod tests {
    use super::*;

    // draft-derived: §5.3.3's own two bullet points, "SSRC LSB=0: Original
    // packet" / "SSRC LSB=1: Retransmission packet", and "the remaining 31
    // bits of the SSRC shall be the same between the original and
    // retransmitted packets".

    #[test]
    fn even_ssrc_is_original() {
        assert_eq!(Origin::from_ssrc(0xAABB_CC00), Origin::Original);
    }

    #[test]
    fn odd_ssrc_is_retransmission() {
        assert_eq!(Origin::from_ssrc(0xAABB_CC01), Origin::Retransmission);
    }

    #[test]
    fn tagging_preserves_flow_id() {
        let original_ssrc = 0xAABB_CC00;
        let retransmitted_ssrc = Origin::Retransmission.tag(original_ssrc);
        assert_eq!(retransmitted_ssrc, 0xAABB_CC01);
        assert_eq!(flow_id(original_ssrc), flow_id(retransmitted_ssrc));
    }

    #[test]
    fn tagging_is_idempotent_on_already_tagged_ssrc() {
        let ssrc = 0xAABB_CC01;
        assert_eq!(Origin::Retransmission.tag(ssrc), ssrc);
        assert_eq!(Origin::Original.tag(ssrc), ssrc & !1);
    }
}
