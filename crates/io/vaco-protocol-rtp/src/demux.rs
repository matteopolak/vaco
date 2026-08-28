//! Splitting a shared RTP/RTCP transport into its two logical streams —
//! RFC 5761's own scenario (`rtcp-mux`), and also what a bare pair of
//! sockets needs when the RTCP side has not been given its own port.

use vaco_rtp::{RtcpPacket, RtpPacket};

/// Which of the two RFC 3550 packet families one buffer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketKind {
    Rtp,
    Rtcp,
}

/// One arriving buffer, classified and parsed. Parsing failure is not
/// folded into `PacketKind` classification: [`crate::mux::classify`]'s
/// heuristic is cheap and needs no parse, so a caller that only wants to
/// route bytes to the right socket/handler can use it without paying for
/// a full parse first — this type is for a caller that wants both in one
/// step.
///
/// RTCP is almost always sent as a compound packet (RFC 3550 §6.1: an SR
/// or RR must be the first packet, typically followed by an SDES), so the
/// RTCP arm holds every packet [`vaco_rtp::rtcp::iter_compound`] found,
/// not just one.
#[derive(Debug)]
pub enum Demuxed<'a> {
    Rtp(vaco_core::Result<RtpPacket<'a>>),
    Rtcp(Vec<vaco_core::Result<RtcpPacket>>),
    /// `buf` was too short to even classify (see [`crate::mux::classify`]).
    Unclassifiable,
}

/// Classify and parse one buffer from a shared RTP/RTCP transport.
#[must_use]
pub fn demux(buf: &[u8]) -> Demuxed<'_> {
    match crate::mux::classify(buf) {
        Some(PacketKind::Rtp) => Demuxed::Rtp(RtpPacket::parse(buf)),
        Some(PacketKind::Rtcp) => Demuxed::Rtcp(vaco_rtp::rtcp::iter_compound(buf).collect()),
        None => Demuxed::Unclassifiable,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_rtp::{RTP_VERSION, RtpHeader};

    #[test]
    fn a_well_formed_rtp_packet_demuxes_and_parses_as_rtp() {
        let header = RtpHeader {
            version: RTP_VERSION,
            padding: false,
            extension: false,
            marker: false,
            payload_type: 96,
            sequence_number: 1,
            timestamp: 0,
            ssrc: 1,
            csrc_count: 0,
        };
        let built = vaco_rtp::rtp::build_basic(&header, b"x");
        match demux(&built) {
            Demuxed::Rtp(Ok(pkt)) => assert_eq!(pkt.header, header),
            other => panic!("expected Rtp(Ok(_)), got {other:?}"),
        }
    }

    #[test]
    fn a_buffer_with_an_rtcp_packet_type_demuxes_as_rtcp() {
        // Minimal RTCP-shaped buffer: V=2, PT=200 (SR) at byte 1, no
        // further structure needed — RtcpPacket::parse's own success/
        // failure is not what this test is checking, only that `demux`
        // routed to the Rtcp arm.
        let buf = vaco_rtp::rtcp::build_bye(&[1]);
        assert!(matches!(demux(&buf), Demuxed::Rtcp(_)));
    }

    #[test]
    fn empty_buffer_is_unclassifiable() {
        assert!(matches!(demux(&[]), Demuxed::Unclassifiable));
    }
}
