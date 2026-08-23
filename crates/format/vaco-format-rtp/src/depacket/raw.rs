//! Payload types whose RTP framing is "one packet is one complete frame,
//! with no depacketiser-visible header at all".
//!
//! Covers PCMU (RFC 3551 PT 0), PCMA (PT 8), L16 (PT 10/11), and the dynamic
//! `opus` (RFC 7587 §4: "each RTP packet ... contains one or more Opus
//! frames" — this crate treats the whole payload as one Opus packet, which
//! is what every encoder that names itself `opus/48000/2` actually sends:
//! DTX/multi-frame packing is an encoder option this crate does not need to
//! unpack, since the Opus decoder itself parses a multi-frame packet),
//! `speex`, and octet-aligned single-frame `AMR`/`AMR-WB` (RFC 4867 §4.2:
//! this module does not implement the interleaved/bandwidth-efficient modes
//! or multi-frame-per-packet payloads, only the common
//! one-frame-per-packet, octet-aligned case most encoders default to).
//! `ac3` (RFC 4184) is included too: this module implements only its
//! un-fragmented case (RFC 4184 §4.1's optional fragmentation is not
//! implemented — a fragmented AC-3 frame is reported as
//! [`vaco_core::Error::Unsupported`] rather than silently truncated).

use vaco_core::{Error, Result};

use super::Depacketizer;

/// One RTP packet in, its payload out unchanged. Correct for every payload
/// type this module documents.
#[derive(Debug, Default)]
pub struct Identity;

impl Depacketizer for Identity {
    fn push(&mut self, _marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(Some(payload.to_vec()))
    }
}

/// RFC 4184 AC-3: a leading one-byte header (`fragment_type`:2 bits +
/// `packet_bytes_count`:6 bits) ahead of the frame. Only `fragment_type ==
/// 0` (a complete, unfragmented frame) is implemented.
#[derive(Debug, Default)]
pub struct Ac3;

impl Depacketizer for Ac3 {
    fn push(&mut self, _marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let first = *payload
            .first()
            .ok_or(Error::InvalidData("RTP AC-3 payload is empty"))?;
        let fragment_type = first >> 6;
        if fragment_type != 0 {
            return Err(Error::Unsupported(
                "fragmented RFC 4184 AC-3 payloads are not implemented",
            ));
        }
        let body = payload.get(1..).ok_or(Error::InvalidData(
            "RTP AC-3 payload has no frame after its header",
        ))?;
        Ok(Some(body.to_vec()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn identity_passes_through() {
        let mut d = Identity;
        assert_eq!(d.push(true, 0, b"abc").unwrap(), Some(b"abc".to_vec()));
    }

    #[test]
    fn ac3_strips_header_when_unfragmented() {
        let mut d = Ac3;
        let mut payload = vec![0x00u8];
        payload.extend_from_slice(b"frame-bytes");
        assert_eq!(
            d.push(true, 0, &payload).unwrap(),
            Some(b"frame-bytes".to_vec())
        );
    }

    #[test]
    fn ac3_rejects_fragmented() {
        let mut d = Ac3;
        assert!(d.push(true, 0, &[0x40]).is_err());
    }
}
