//! RFC 7798: HEVC/H.265 over RTP.
//!
//! Mirrors [`crate::depacket::h264`]'s three cases with HEVC's 2-byte NAL
//! header (§1.1.4) instead of H.264's one byte: single NAL unit packets
//! (§4.4.1), Aggregation Packets — type 48, §4.4.2 — and Fragmentation
//! Units — type 49, §4.4.3. **Not implemented**: the `DONL`/`DOND` fields
//! (§4.4.2, only present when `sprop-max-don-diff` is negotiated, which this
//! crate's SDP layer never offers) and PACI packets (type 50, §4.4.4,
//! temporal scalability control information this crate has no use for).
//! Output is Annex B, matching [`crate::depacket::h264`]'s reasoning.

use vaco_core::{Error, Result};

use super::Depacketizer;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

fn nal_type(header: [u8; 2]) -> u8 {
    (header[0] >> 1) & 0x3F
}

/// HEVC/RTP depacketiser.
#[derive(Debug, Default)]
pub struct HevcDepacketizer {
    fu: Option<Vec<u8>>,
}

impl Depacketizer for HevcDepacketizer {
    fn push(&mut self, _marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let header: [u8; 2] =
            payload
                .get(0..2)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData(
                    "RTP HEVC payload shorter than its 2-byte header",
                ))?;
        match nal_type(header) {
            48 => {
                self.fu = None;
                Ok(Some(unpack_ap(payload)?))
            }
            49 => self.push_fu(payload, header),
            50 => Err(Error::Unsupported(
                "RFC 7798 PACI packets are not implemented",
            )),
            _ => {
                self.fu = None;
                let mut out = Vec::new();
                out.extend_from_slice(&START_CODE);
                out.extend_from_slice(payload);
                Ok(Some(out))
            }
        }
    }
}

impl HevcDepacketizer {
    fn push_fu(&mut self, payload: &[u8], header: [u8; 2]) -> Result<Option<Vec<u8>>> {
        let fu_header = *payload
            .get(2)
            .ok_or(Error::InvalidData("RTP HEVC FU payload has no FU header"))?;
        let start = fu_header & 0x80 != 0;
        let end = fu_header & 0x40 != 0;
        let original_type = fu_header & 0x3F;
        let body = payload.get(3..).ok_or(Error::InvalidData(
            "RTP HEVC FU payload has no fragment data",
        ))?;

        if start {
            // Rebuild the 2-byte NAL header with the original type, keeping
            // layer_id/tid from the FU's own PayloadHdr.
            let byte0 = (header[0] & 0x81) | (original_type << 1);
            let byte1 = header[1];
            let mut buf = Vec::new();
            buf.extend_from_slice(&START_CODE);
            buf.push(byte0);
            buf.push(byte1);
            buf.extend_from_slice(body);
            self.fu = Some(buf);
        } else {
            let buf = self.fu.as_mut().ok_or(Error::InvalidData(
                "RTP HEVC FU continuation with no start fragment",
            ))?;
            buf.extend_from_slice(body);
        }

        if end { Ok(self.fu.take()) } else { Ok(None) }
    }
}

fn unpack_ap(payload: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut nals = payload
        .get(2..)
        .ok_or(Error::InvalidData("RTP HEVC AP payload is empty"))?;
    while !nals.is_empty() {
        let size_bytes: [u8; 2] =
            nals.get(0..2)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData(
                    "RTP HEVC AP NALU size runs past the payload",
                ))?;
        let size = usize::from(u16::from_be_bytes(size_bytes));
        let nal = nals
            .get(2..2 + size)
            .ok_or(Error::InvalidData("RTP HEVC AP NALU runs past the payload"))?;
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
        nals = nals
            .get(2 + size..)
            .ok_or(Error::InvalidData("RTP HEVC AP arithmetic is inconsistent"))?;
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn single_nal_gets_a_start_code() {
        let mut d = HevcDepacketizer::default();
        // type 32 (VPS): byte0 = type<<1 = 0x40
        let nal = [0x40, 0x01, 1, 2, 3];
        let out = d.push(true, 0, &nal).unwrap().unwrap();
        assert_eq!(out, [&START_CODE[..], &nal[..]].concat());
    }

    #[test]
    fn fu_reassembles_two_fragments() {
        let mut d = HevcDepacketizer::default();
        // PayloadHdr: type=49 (0x62 => (49<<1)=98=0x62), layer/tid byte = 0x01
        let payload_hdr = [0x62u8, 0x01];
        let start = [payload_hdr[0], payload_hdr[1], 0x80 | 1, 0xAA]; // S=1, type=1 (TRAIL_R)
        let end = [payload_hdr[0], payload_hdr[1], 0x40 | 1, 0xBB]; // E=1
        assert_eq!(d.push(false, 0, &start).unwrap(), None);
        let out = d.push(true, 0, &end).unwrap().unwrap();
        let mut expect = Vec::new();
        expect.extend_from_slice(&START_CODE);
        expect.push(1 << 1); // reconstructed type=1, layer bit 0 preserved (0)
        expect.push(0x01);
        expect.extend_from_slice(&[0xAA, 0xBB]);
        assert_eq!(out, expect);
    }

    #[test]
    fn ap_unpacks_both_nals() {
        let mut d = HevcDepacketizer::default();
        let nal_a = [0x40, 0x01, 0xAA];
        let nal_b = [0x42, 0x01, 0xBB, 0xCC];
        let mut payload = vec![0x60u8, 0x01]; // type 48
        payload.extend_from_slice(&(nal_a.len() as u16).to_be_bytes());
        payload.extend_from_slice(&nal_a);
        payload.extend_from_slice(&(nal_b.len() as u16).to_be_bytes());
        payload.extend_from_slice(&nal_b);
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        let mut expect = Vec::new();
        expect.extend_from_slice(&START_CODE);
        expect.extend_from_slice(&nal_a);
        expect.extend_from_slice(&START_CODE);
        expect.extend_from_slice(&nal_b);
        assert_eq!(out, expect);
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let mut d = HevcDepacketizer::default();
            let _ = d.push(true, 0, &bytes);
        }
    }
}
