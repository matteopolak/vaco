//! RFC 3640 `MPEG4-GENERIC` packetiser: one access unit per RTP packet,
//! the mirror image of `vaco_format_rtp::depacket::aac`'s single-AU
//! limitation. `size_length`/`index_length` default to `13`/`3`, matching
//! that module's own defaults so the two interoperate without extra
//! configuration.
//!
//! **Not implemented**: splitting an access unit across packets when it
//! does not fit the MTU. AAC frames are small enough (a few hundred bytes
//! at typical bitrates) that this essentially never triggers in practice;
//! when it would, this packetiser sends the oversized packet anyway rather
//! than silently dropping audio — a caller that hits this in practice needs
//! RFC 3640's `fragment` AU-header extension, which is a real gap, not
//! this crate pretending the limit does not exist.

use super::Packetizer;

#[derive(Debug, Clone, Copy)]
pub struct AacPacketizer {
    pub size_length: u32,
    pub index_length: u32,
}

impl Default for AacPacketizer {
    fn default() -> Self {
        Self {
            size_length: 13,
            index_length: 3,
        }
    }
}

impl Packetizer for AacPacketizer {
    fn packetize(&mut self, au: &[u8], _mtu: usize) -> Vec<Vec<u8>> {
        if au.is_empty() {
            return Vec::new();
        }
        let header_bits = self.size_length + self.index_length;
        // Only `size_length == 13, index_length == 3` (16 bits = 2 bytes)
        // is exercised by this crate's own tests; the general bit-packing
        // below still handles any byte-aligned combination correctly.
        let header_bytes = usize::try_from(header_bits.div_ceil(8)).unwrap_or(2);
        let mut au_size_bits = vec![false; usize::try_from(self.size_length).unwrap_or(13)];
        let mut size = au.len();
        for bit in au_size_bits.iter_mut().rev() {
            *bit = size & 1 != 0;
            size >>= 1;
        }
        let index_bits = vec![false; usize::try_from(self.index_length).unwrap_or(3)];
        let mut bits: Vec<bool> = au_size_bits;
        bits.extend(index_bits);

        let mut header_section = vec![0u8; header_bytes];
        for (i, bit) in bits.iter().enumerate() {
            if *bit {
                let byte_idx = i >> 3;
                let bit_idx = 7 - (i & 7);
                if let Some(b) = header_section.get_mut(byte_idx) {
                    *b |= 1 << bit_idx;
                }
            }
        }

        let mut packet = Vec::new();
        packet.extend_from_slice(&(header_bits as u16).to_be_bytes());
        packet.extend_from_slice(&header_section);
        packet.extend_from_slice(au);
        vec![packet]
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_rtp::depacket::Depacketizer;
    use vaco_format_rtp::depacket::aac::AacDepacketizer;

    #[test]
    fn round_trips_through_the_depacketiser() {
        let mut p = AacPacketizer::default();
        let au = b"an-aac-access-unit".to_vec();
        let packets = p.packetize(&au, 1500);
        assert_eq!(packets.len(), 1);

        let mut d = AacDepacketizer::default();
        let out = d.push(true, 0, &packets[0]).unwrap();
        assert_eq!(out, Some(au));
    }

    proptest::proptest! {
        #[test]
        fn round_trips_arbitrary_access_units(au in proptest::collection::vec(proptest::prelude::any::<u8>(), 1..500)) {
            let mut p = AacPacketizer::default();
            let packets = p.packetize(&au, 1500);
            proptest::prop_assert_eq!(packets.len(), 1);
            let mut d = AacDepacketizer::default();
            let out = d.push(true, 0, &packets[0]).unwrap();
            proptest::prop_assert_eq!(out, Some(au));
        }
    }
}
