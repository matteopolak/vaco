//! RFC 4960 §3.1 — the SCTP common header, and packet-level `CRC32c`
//! validation (Appendix B).

use vaco_hash::crc32c;
use vaco_protocol_core::{ProtocolError, Result};

const SCHEME: &str = "sctp";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed { scheme: SCHEME, detail }
}

pub const COMMON_HEADER_LEN: usize = 12;

/// RFC 4960 §3.1's 12-byte common header, present once at the start of
/// every SCTP packet, ahead of one or more chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommonHeader {
    pub source_port: u16,
    pub destination_port: u16,
    pub verification_tag: u32,
    /// The `CRC32c` checksum as read off (or computed for) the wire. Not
    /// re-derived on every field access — see [`verify_checksum`]/
    /// [`build_with_checksum`] for when it is actually checked/computed.
    pub checksum: u32,
}

impl CommonHeader {
    /// Parse the fixed 12-byte header from the front of a packet. Does
    /// not verify the checksum — see [`verify_checksum`], which needs the
    /// whole packet, not just the header.
    ///
    /// # Errors
    /// [`ProtocolError::Malformed`] if `buf` is shorter than 12 bytes.
    pub fn parse(buf: &[u8]) -> Result<Self> {
        let b: [u8; COMMON_HEADER_LEN] = buf.get(..COMMON_HEADER_LEN).ok_or_else(|| malformed("SCTP common header is truncated"))?.try_into().unwrap_or([0; COMMON_HEADER_LEN]);
        Ok(Self {
            source_port: u16::from_be_bytes([b[0], b[1]]),
            destination_port: u16::from_be_bytes([b[2], b[3]]),
            verification_tag: u32::from_be_bytes([b[4], b[5], b[6], b[7]]),
            checksum: u32::from_be_bytes([b[8], b[9], b[10], b[11]]),
        })
    }

    /// Serialize the header with `self.checksum` written verbatim
    /// (usually 0 while assembling a packet whose checksum is filled in
    /// afterwards by [`build_with_checksum`]).
    #[must_use]
    pub fn build(&self) -> [u8; COMMON_HEADER_LEN] {
        let mut out = [0u8; COMMON_HEADER_LEN];
        out[0..2].copy_from_slice(&self.source_port.to_be_bytes());
        out[2..4].copy_from_slice(&self.destination_port.to_be_bytes());
        out[4..8].copy_from_slice(&self.verification_tag.to_be_bytes());
        out[8..12].copy_from_slice(&self.checksum.to_be_bytes());
        out
    }
}

/// Compute Appendix B's `CRC32c` over a whole packet with the checksum
/// field treated as zero, per RFC 4960 §6.8's own procedure (the sender
/// zeroes the field, computes the CRC over the full packet including the
/// zeroed field, then writes the result into that field).
#[must_use]
pub fn compute_checksum(header: &CommonHeader, chunks: &[u8]) -> u32 {
    let zeroed = CommonHeader { checksum: 0, ..*header };
    let mut buf = Vec::new();
    buf.extend_from_slice(&zeroed.build());
    buf.extend_from_slice(chunks);
    crc32c(&buf)
}

/// Assemble a whole packet: header (checksum filled in) followed by
/// `chunks` (already padded to 4-byte boundaries by the caller — see
/// [`crate::chunk::pad_to_4`]).
#[must_use]
pub fn build_with_checksum(header: &CommonHeader, chunks: &[u8]) -> Vec<u8> {
    let checksum = compute_checksum(header, chunks);
    let final_header = CommonHeader { checksum, ..*header };
    let mut out = Vec::new();
    out.extend_from_slice(&final_header.build());
    out.extend_from_slice(chunks);
    out
}

/// Verify a whole received packet's checksum.
#[must_use]
pub fn verify_checksum(packet: &[u8]) -> bool {
    let Ok(header) = CommonHeader::parse(packet) else {
        return false;
    };
    let Some(rest) = packet.get(COMMON_HEADER_LEN..) else {
        return false;
    };
    compute_checksum(&header, rest) == header.checksum
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn sample_header() -> CommonHeader {
        CommonHeader { source_port: 1000, destination_port: 2000, verification_tag: 0xDEAD_BEEF, checksum: 0 }
    }

    #[test]
    fn header_round_trips() {
        let h = sample_header();
        let built = h.build();
        assert_eq!(CommonHeader::parse(&built).unwrap(), h);
    }

    #[test]
    fn a_built_packet_verifies_its_own_checksum() {
        let packet = build_with_checksum(&sample_header(), b"fake chunk bytes");
        assert!(verify_checksum(&packet));
    }

    #[test]
    fn a_tampered_packet_fails_checksum_verification() {
        let mut packet = build_with_checksum(&sample_header(), b"fake chunk bytes");
        let last = packet.len() - 1;
        packet[last] ^= 0xFF;
        assert!(!verify_checksum(&packet));
    }

    #[test]
    fn truncated_packet_is_not_verified_not_panicking() {
        assert!(!verify_checksum(&[0u8; 4]));
        assert!(!verify_checksum(&[]));
    }
}
