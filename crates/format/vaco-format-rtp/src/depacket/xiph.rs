//! RFC 5215 §3.1.1: Vorbis and Theora over RTP.
//!
//! Both codecs share one RTP payload framing (the RFC is explicitly written
//! to cover either): a 4-byte header — a 24-bit `Ident`, then one byte of
//! `F` (2-bit fragment type: 0 unfragmented, 1 start, 2 continuation, 3
//! end), `VDT` (2-bit data type, not interpreted by this module — a decoder
//! consuming the reassembled packet already knows a raw data packet from a
//! header packet by its own framing) and a 4-bit packet count — followed by
//! packet data.
//!
//! **Only `# pkts == 1` (the unfragmented, single-packet-per-RTP-payload
//! case) is implemented for `F == 0`.** `# pkts > 1` would aggregate
//! several complete codec packets into one RTP payload, each but the last
//! prefixed with a 16-bit length; this module's [`super::Depacketizer`]
//! trait returns one unit per `push`, so multi-packet aggregation is
//! reported as [`vaco_core::Error::Unsupported`] rather than silently
//! dropping every packet after the first. Fragmentation (`F` 1/2/3) *is*
//! implemented: a fragmented packet's raw bytes are concatenated across
//! calls with no further inner framing (RFC 5215 §3.1.1: "fragments ...
//! contain no...header of their own"), and the complete packet is returned
//! on the `F == 3` (end) call.

use vaco_core::{Error, Result};

use super::Depacketizer;

/// Vorbis/Theora RTP depacketiser (RFC 5215).
#[derive(Debug, Default)]
pub struct XiphDepacketizer {
    fragment: Vec<u8>,
    fragmenting: bool,
}

impl Depacketizer for XiphDepacketizer {
    fn push(&mut self, _marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let header: [u8; 4] =
            payload
                .get(0..4)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData(
                    "RTP Xiph payload shorter than its 4-byte header",
                ))?;
        let fragment_type = (header[3] >> 6) & 0x03;
        let num_packets = header[3] & 0x0F;
        let body = payload.get(4..).ok_or(Error::InvalidData(
            "RTP Xiph payload has no data after its header",
        ))?;

        match fragment_type {
            0 => {
                if self.fragmenting {
                    return Err(Error::InvalidData(
                        "RTP Xiph unfragmented packet arrived while a fragment was in progress",
                    ));
                }
                if num_packets != 1 {
                    return Err(Error::Unsupported(
                        "RTP Xiph packets aggregating more than one codec packet are not implemented",
                    ));
                }
                Ok(Some(body.to_vec()))
            }
            1 => {
                self.fragment.clear();
                self.fragment.extend_from_slice(body);
                self.fragmenting = true;
                Ok(None)
            }
            2 => {
                if !self.fragmenting {
                    return Err(Error::InvalidData(
                        "RTP Xiph continuation fragment with no start fragment",
                    ));
                }
                self.fragment.extend_from_slice(body);
                Ok(None)
            }
            3 => {
                if !self.fragmenting {
                    return Err(Error::InvalidData(
                        "RTP Xiph end fragment with no start fragment",
                    ));
                }
                self.fragment.extend_from_slice(body);
                self.fragmenting = false;
                Ok(Some(std::mem::take(&mut self.fragment)))
            }
            _ => Err(Error::InvalidData("RTP Xiph fragment type is out of range")),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn header(fragment_type: u8, num_packets: u8) -> [u8; 4] {
        [0, 0, 0, (fragment_type << 6) | (num_packets & 0x0F)]
    }

    #[test]
    fn unfragmented_single_packet() {
        let mut d = XiphDepacketizer::default();
        let mut payload = header(0, 1).to_vec();
        payload.extend_from_slice(b"vorbis-packet");
        assert_eq!(
            d.push(true, 0, &payload).unwrap(),
            Some(b"vorbis-packet".to_vec())
        );
    }

    #[test]
    fn rejects_aggregated_packets() {
        let mut d = XiphDepacketizer::default();
        let mut payload = header(0, 2).to_vec();
        payload.extend_from_slice(b"xx");
        assert!(d.push(true, 0, &payload).is_err());
    }

    #[test]
    fn reassembles_a_fragmented_packet() {
        let mut d = XiphDepacketizer::default();
        let mut start = header(1, 0).to_vec();
        start.extend_from_slice(b"AB");
        let mut end = header(3, 0).to_vec();
        end.extend_from_slice(b"CD");
        assert_eq!(d.push(false, 0, &start).unwrap(), None);
        assert_eq!(d.push(true, 0, &end).unwrap(), Some(b"ABCD".to_vec()));
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let mut d = XiphDepacketizer::default();
            let _ = d.push(true, 0, &bytes);
        }
    }
}
