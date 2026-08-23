//! RFC 6184: H.264 over RTP.
//!
//! Implements the three packetisation modes that matter in practice: single
//! NAL unit packets (§5.6), STAP-A aggregation (§5.7.1), and FU-A
//! fragmentation (§5.8). **Not implemented**: STAP-B/MTAP16/MTAP24 (§5.7.2,
//! rarely negotiated — they exist for packetisation-mode 2, which this
//! crate's SDP layer does not offer) and FU-B (§5.8, FU-A's
//! `dont-interleave`-only sibling). Both report
//! [`vaco_core::Error::Unsupported`] rather than silently dropping data.
//!
//! Output is Annex B (start-code-prefixed) NAL units, matching what
//! `vaco-format-nalu`'s Annex-B tooling and every raw H.264 demuxer in this
//! workspace already expects — a depacketiser producing length-prefixed
//! output would need every downstream consumer to special-case "this stream
//! came from RTP".

use vaco_core::{Error, Result};

use super::Depacketizer;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

fn nal_type(first_byte: u8) -> u8 {
    first_byte & 0x1F
}

/// H.264/RTP depacketiser. Holds in-progress FU-A reassembly state across
/// calls; a `push` for a NAL type it does not recognise as a continuation
/// resets that state rather than silently merging two unrelated NALs.
#[derive(Debug, Default)]
pub struct H264Depacketizer {
    fu: Option<Vec<u8>>,
}

impl Depacketizer for H264Depacketizer {
    fn push(&mut self, _marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let first = *payload
            .first()
            .ok_or(Error::InvalidData("RTP H.264 payload is empty"))?;
        match nal_type(first) {
            1..=23 => {
                self.fu = None;
                let mut out = Vec::new();
                out.extend_from_slice(&START_CODE);
                out.extend_from_slice(payload);
                Ok(Some(out))
            }
            24 => {
                self.fu = None;
                Ok(Some(unpack_stap_a(payload)?))
            }
            28 => self.push_fu_a(payload),
            25..=27 => Err(Error::Unsupported(
                "RFC 6184 STAP-B/MTAP16/MTAP24 are not implemented",
            )),
            29 => Err(Error::Unsupported("RFC 6184 FU-B is not implemented")),
            _ => Err(Error::InvalidData(
                "RTP H.264 payload names an unknown NAL type",
            )),
        }
    }
}

impl H264Depacketizer {
    fn push_fu_a(&mut self, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let indicator = *payload
            .first()
            .ok_or(Error::InvalidData("RTP H.264 FU-A payload is empty"))?;
        let fu_header = *payload.get(1).ok_or(Error::InvalidData(
            "RTP H.264 FU-A payload has no FU header",
        ))?;
        let start = fu_header & 0x80 != 0;
        let end = fu_header & 0x40 != 0;
        let original_nal_type = fu_header & 0x1F;
        let body = payload.get(2..).ok_or(Error::InvalidData(
            "RTP H.264 FU-A payload has no fragment data",
        ))?;

        if start {
            let reconstructed_header = (indicator & 0xE0) | original_nal_type;
            let mut buf = Vec::new();
            buf.extend_from_slice(&START_CODE);
            buf.push(reconstructed_header);
            buf.extend_from_slice(body);
            self.fu = Some(buf);
        } else {
            let buf = self.fu.as_mut().ok_or(Error::InvalidData(
                "RTP H.264 FU-A continuation with no start fragment",
            ))?;
            buf.extend_from_slice(body);
        }

        if end { Ok(self.fu.take()) } else { Ok(None) }
    }
}

fn unpack_stap_a(payload: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut nals = payload
        .get(1..)
        .ok_or(Error::InvalidData("RTP H.264 STAP-A payload is empty"))?;
    while !nals.is_empty() {
        let size_bytes: [u8; 2] =
            nals.get(0..2)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData(
                    "RTP H.264 STAP-A NALU size runs past the payload",
                ))?;
        let size = usize::from(u16::from_be_bytes(size_bytes));
        let nal = nals.get(2..2 + size).ok_or(Error::InvalidData(
            "RTP H.264 STAP-A NALU runs past the payload",
        ))?;
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
        nals = nals.get(2 + size..).ok_or(Error::InvalidData(
            "RTP H.264 STAP-A arithmetic is inconsistent",
        ))?;
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn single_nal_gets_a_start_code() {
        let mut d = H264Depacketizer::default();
        let nal = [0x67, 1, 2, 3]; // type 7 = SPS
        let out = d.push(true, 0, &nal).unwrap().unwrap();
        assert_eq!(out, [&START_CODE[..], &nal[..]].concat());
    }

    #[test]
    fn stap_a_unpacks_both_nals() {
        let mut d = H264Depacketizer::default();
        let nal_a = [0x67, 0xAA];
        let nal_b = [0x68, 0xBB, 0xCC];
        let mut payload = vec![24u8];
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

    #[test]
    fn fu_a_reassembles_three_fragments() {
        let mut d = H264Depacketizer::default();
        // Original NAL header: F=0 NRI=3 Type=5 (IDR) -> 0x65
        let indicator = 0x7C; // F=0 NRI=3 Type=28 (FU-A)
        let start = [indicator, 0x85, 1, 2]; // S=1 E=0 Type=5
        let mid = [indicator, 0x05, 3, 4]; // S=0 E=0
        let end = [indicator, 0x45, 5, 6]; // S=0 E=1
        assert_eq!(d.push(false, 0, &start).unwrap(), None);
        assert_eq!(d.push(false, 0, &mid).unwrap(), None);
        let out = d.push(true, 0, &end).unwrap().unwrap();
        let mut expect = Vec::new();
        expect.extend_from_slice(&START_CODE);
        expect.push(0x65); // reconstructed NRI=3 Type=5
        expect.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(out, expect);
    }

    #[test]
    fn fu_a_continuation_without_start_is_an_error() {
        let mut d = H264Depacketizer::default();
        let mid = [0x7C, 0x05, 1, 2];
        assert!(d.push(false, 0, &mid).is_err());
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let mut d = H264Depacketizer::default();
            let _ = d.push(true, 0, &bytes);
        }
    }
}
