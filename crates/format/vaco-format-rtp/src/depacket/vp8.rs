//! RFC 7741 §4.2: VP8 over RTP.
//!
//! The payload descriptor is variable-length depending on which extension
//! flags are set; this module's only job is walking it correctly so the
//! encoded VP8 payload after it can be concatenated across packets until
//! the marker bit, which is what a complete VP8 frame boundary is (RFC 7741
//! has no fragmentation-unit header the way H.264/HEVC do — a frame that
//! does not fit one packet is simply split across consecutive packets with
//! no extra framing at all).

use vaco_core::{Error, Result};

use super::Depacketizer;

/// VP8/RTP depacketiser.
#[derive(Debug, Default)]
pub struct Vp8Depacketizer {
    frame: Vec<u8>,
}

fn descriptor_len(payload: &[u8]) -> Result<usize> {
    let byte0 = *payload
        .first()
        .ok_or(Error::InvalidData("RTP VP8 payload is empty"))?;
    let extended = byte0 & 0x80 != 0; // X
    if !extended {
        return Ok(1);
    }
    let byte1 = *payload.get(1).ok_or(Error::InvalidData(
        "RTP VP8 extension flags byte is missing",
    ))?;
    let has_picture_id = byte1 & 0x80 != 0; // I
    let has_tl0_idx = byte1 & 0x40 != 0; // L
    let has_tid_or_key = byte1 & 0x20 != 0 || byte1 & 0x10 != 0; // T | K

    let mut len = 2usize;
    if has_picture_id {
        let pid_byte = *payload
            .get(len)
            .ok_or(Error::InvalidData("RTP VP8 PictureID byte is missing"))?;
        len = len
            .checked_add(1)
            .ok_or(Error::InvalidData("RTP VP8 descriptor overflows"))?;
        if pid_byte & 0x80 != 0 {
            // 2-byte (15-bit) PictureID.
            len = len
                .checked_add(1)
                .ok_or(Error::InvalidData("RTP VP8 descriptor overflows"))?;
        }
    }
    if has_tl0_idx {
        len = len
            .checked_add(1)
            .ok_or(Error::InvalidData("RTP VP8 descriptor overflows"))?;
    }
    if has_tid_or_key {
        len = len
            .checked_add(1)
            .ok_or(Error::InvalidData("RTP VP8 descriptor overflows"))?;
    }
    Ok(len)
}

impl Depacketizer for Vp8Depacketizer {
    fn push(&mut self, marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let len = descriptor_len(payload)?;
        let body = payload.get(len..).ok_or(Error::InvalidData(
            "RTP VP8 descriptor runs past the payload",
        ))?;
        self.frame.extend_from_slice(body);
        if marker {
            Ok(Some(std::mem::take(&mut self.frame)))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn strips_minimal_one_byte_descriptor() {
        let mut d = Vp8Depacketizer::default();
        let payload = [0x10u8, 1, 2, 3]; // X=0
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(out, vec![1, 2, 3]);
    }

    #[test]
    fn strips_extended_descriptor_with_short_picture_id() {
        let mut d = Vp8Depacketizer::default();
        // X=1, byte1: I=1 -> one more byte (short PictureID, top bit clear)
        let payload = [0x80u8, 0x80, 0x05, 9, 9];
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(out, vec![9, 9]);
    }

    #[test]
    fn reassembles_across_two_packets_until_marker() {
        let mut d = Vp8Depacketizer::default();
        assert_eq!(d.push(false, 0, &[0x00, 1, 2]).unwrap(), None);
        let out = d.push(true, 0, &[0x00, 3, 4]).unwrap().unwrap();
        assert_eq!(out, vec![1, 2, 3, 4]);
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let mut d = Vp8Depacketizer::default();
            let _ = d.push(true, 0, &bytes);
        }
    }
}
