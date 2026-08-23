//! RFC 6184 H.264 packetiser: single NAL unit packets (§5.6) and FU-A
//! fragmentation (§5.8) — the packetise-side mirror of
//! `vaco_format_rtp::depacket::h264`. **Not implemented**: STAP-A
//! aggregation — every NAL in the access unit is sent as its own packet (or
//! its own FU-A run) rather than bundling small ones together, which is
//! correct RTP and only costs a little header overhead compared to what
//! `ffmpeg`'s packetiser does for a run of tiny NALs.

use vaco_bitstream::annexb;

use super::Packetizer;

const FU_HEADER_OVERHEAD: usize = 2; // FU indicator + FU header

fn nal_type(first_byte: u8) -> u8 {
    first_byte & 0x1F
}

/// H.264/RTP packetiser (RFC 6184).
#[derive(Debug, Default)]
pub struct H264Packetizer;

impl Packetizer for H264Packetizer {
    fn packetize(&mut self, au: &[u8], mtu: usize) -> Vec<Vec<u8>> {
        let mtu = mtu.max(FU_HEADER_OVERHEAD + 1);
        let mut out = Vec::new();
        for nal in annexb::nal_units(au) {
            if nal.is_empty() {
                continue;
            }
            if nal.len() <= mtu {
                out.push(nal.to_vec());
                continue;
            }
            let Some(&first) = nal.first() else { continue };
            let nri = first & 0x60;
            let ty = nal_type(first);
            let indicator = nri | 0x1C; // FU-A
            let body_chunk = mtu - FU_HEADER_OVERHEAD;
            let body = nal.get(1..).unwrap_or(&[]);
            for (i, chunk) in body.chunks(body_chunk.max(1)).enumerate() {
                let is_first = i == 0;
                let is_last = (i + 1) * body_chunk >= body.len();
                let mut fu_header = ty;
                if is_first {
                    fu_header |= 0x80;
                }
                if is_last {
                    fu_header |= 0x40;
                }
                let mut packet = Vec::new();
                packet.push(indicator);
                packet.push(fu_header);
                packet.extend_from_slice(chunk);
                out.push(packet);
            }
        }
        out
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::default_constructed_unit_structs,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_format_rtp::Depacketizer;

    #[test]
    fn single_nal_fits_in_one_packet() {
        let mut p = H264Packetizer;
        let mut au = vec![0, 0, 0, 1];
        au.extend_from_slice(&[0x67, 1, 2, 3]);
        let out = p.packetize(&au, 1500);
        assert_eq!(out, vec![vec![0x67, 1, 2, 3]]);
    }

    #[test]
    fn large_nal_fragments_with_start_and_end_bits() {
        let mut p = H264Packetizer;
        let mut au = vec![0, 0, 0, 1];
        let mut nal = vec![0x65u8]; // NRI=3, type=5 (IDR)
        nal.extend(std::iter::repeat_n(0xAAu8, 100));
        au.extend_from_slice(&nal);
        let out = p.packetize(&au, 30);
        assert!(out.len() > 1);
        assert_eq!(out[0][0] & 0x1F, 28); // FU-A indicator
        assert!(out[0][1] & 0x80 != 0); // S bit on first
        assert_eq!(out[0][1] & 0x40, 0);
        assert!(out.last().unwrap()[1] & 0x40 != 0); // E bit on last

        // Reassembling with the depacketiser must recover the original NAL.
        let mut d = vaco_format_rtp::depacket::h264::H264Depacketizer::default();
        let mut reassembled = None;
        for (i, pkt) in out.iter().enumerate() {
            let marker = i + 1 == out.len();
            reassembled = d.push(marker, 0, pkt).unwrap();
        }
        let mut expect = vec![0, 0, 0, 1];
        expect.extend_from_slice(&nal);
        assert_eq!(reassembled.unwrap(), expect);
    }

    proptest::proptest! {
        #[test]
        fn packetize_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512), mtu in 3usize..64) {
            let mut p = H264Packetizer;
            let _ = p.packetize(&bytes, mtu);
        }
    }
}
