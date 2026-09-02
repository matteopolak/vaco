//! A sans-io RTP sending session: the small piece of state RFC 3550 §5.1
//! actually requires a sender to keep between packets (the sequence
//! counter, which SSRC identifies this source) that [`vaco_rtp::rtp`]
//! itself, being a pure parse/build module with no notion of "the next
//! packet", deliberately does not hold.
//!
//! No socket, no clock reference beyond a caller-supplied RTP timestamp —
//! matching every other protocol crate at this layer
//! (`vaco-protocol-srt`/`vaco-protocol-rist`) in leaving transport and
//! wall-clock ownership to the caller.

use vaco_rtp::RtpHeader;

/// Sender-side session state for one SSRC.
#[derive(Debug, Clone)]
pub struct SendSession {
    ssrc: u32,
    payload_type: u8,
    sequence_number: u16,
}

impl SendSession {
    /// `initial_sequence_number` is a caller choice, not a default this
    /// crate invents: RFC 3550 §5.1 requires it be random (to make
    /// known-plaintext attacks on some encryption schemes harder), and
    /// "random" is not this sans-io crate's concern to generate.
    #[must_use]
    pub const fn new(ssrc: u32, payload_type: u8, initial_sequence_number: u16) -> Self {
        Self {
            ssrc,
            payload_type,
            sequence_number: initial_sequence_number,
        }
    }

    #[must_use]
    pub const fn ssrc(&self) -> u32 {
        self.ssrc
    }

    #[must_use]
    pub const fn sequence_number(&self) -> u16 {
        self.sequence_number
    }

    /// Build one RTP packet from `payload`, advancing the sequence counter
    /// (wrapping at 2^16, RFC 3550 §5.1's own field width).
    #[must_use]
    pub fn packetize(&mut self, timestamp: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let header = RtpHeader {
            version: vaco_rtp::RTP_VERSION,
            padding: false,
            extension: false,
            marker,
            payload_type: self.payload_type,
            sequence_number: self.sequence_number,
            timestamp,
            ssrc: self.ssrc,
            csrc_count: 0,
        };
        self.sequence_number = self.sequence_number.wrapping_add(1);
        vaco_rtp::rtp::build_basic(&header, payload)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_rtp::RtpPacket;

    #[test]
    fn packetize_advances_the_sequence_number_each_call() {
        let mut session = SendSession::new(0xAABB_CCDD, 96, 1000);
        let first = session.packetize(0, false, b"a");
        let second = session.packetize(160, false, b"b");
        assert_eq!(
            RtpPacket::parse(&first).unwrap().header.sequence_number,
            1000
        );
        assert_eq!(
            RtpPacket::parse(&second).unwrap().header.sequence_number,
            1001
        );
        assert_eq!(session.sequence_number(), 1002);
    }

    #[test]
    fn sequence_number_wraps_at_u16_max() {
        let mut session = SendSession::new(1, 0, u16::MAX);
        let built = session.packetize(0, false, b"");
        assert_eq!(
            RtpPacket::parse(&built).unwrap().header.sequence_number,
            u16::MAX
        );
        assert_eq!(session.sequence_number(), 0);
    }

    #[test]
    fn every_packet_from_one_session_carries_the_same_ssrc_and_payload_type() {
        let mut session = SendSession::new(42, 8, 0);
        for i in 0..5 {
            let built = session.packetize(i, false, b"x");
            let parsed = RtpPacket::parse(&built).unwrap();
            assert_eq!(parsed.header.ssrc, 42);
            assert_eq!(parsed.header.payload_type, 8);
        }
    }
}
