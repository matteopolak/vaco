//! Packetiser for payloads with no RTP-visible framing at all: PCMU, PCMA,
//! L16, Opus — the packetise-side mirror of
//! `vaco_format_rtp::depacket::raw::Identity`. Any byte boundary is a valid
//! split point, so a frame larger than the MTU is simply cut into
//! `mtu`-sized chunks; a real deployment would size its packetiser call
//! per-frame from the codec's own frame size rather than needing mid-frame
//! splits, but this is still correct RTP (RFC 3550 places no meaning on the
//! byte boundaries inside one PCM/Opus payload).

use super::Packetizer;

#[derive(Debug, Default)]
pub struct RawPacketizer;

impl Packetizer for RawPacketizer {
    fn packetize(&mut self, au: &[u8], mtu: usize) -> Vec<Vec<u8>> {
        if au.is_empty() {
            return Vec::new();
        }
        let mtu = mtu.max(1);
        au.chunks(mtu).map(<[u8]>::to_vec).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_into_mtu_sized_chunks() {
        let mut p = RawPacketizer;
        let out = p.packetize(&[0u8; 10], 4);
        assert_eq!(out.iter().map(Vec::len).collect::<Vec<_>>(), vec![4, 4, 2]);
    }

    #[test]
    fn empty_input_produces_no_packets() {
        let mut p = RawPacketizer;
        assert!(p.packetize(&[], 100).is_empty());
    }

    proptest::proptest! {
        #[test]
        fn every_byte_survives_in_order(au in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..300), mtu in 1usize..64) {
            let mut p = RawPacketizer;
            let out = p.packetize(&au, mtu);
            let joined: Vec<u8> = out.into_iter().flatten().collect();
            proptest::prop_assert_eq!(joined, au);
        }
    }
}
