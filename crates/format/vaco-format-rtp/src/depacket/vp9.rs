//! `draft-ietf-payload-vp9-16` §4.2: VP9 over RTP.
//!
//! Same reassembly shape as [`crate::depacket::vp8`] — concatenate stripped
//! payloads until the RTP marker bit — but the payload descriptor itself is
//! considerably larger: picture ID, layer indices, flexible-mode reference
//! indices (a variable-length chain, one byte per referenced frame) and an
//! optional scalability structure (SS) all have their own presence bits.
//! [`descriptor_len`] walks every one of them so the frame bytes handed back
//! never include a stray descriptor tail. **Not implemented**: interpreting
//! any of the fields once skipped (spatial/temporal layer selection) — this
//! crate always decodes every layer, which is the only sane default without
//! a caller-supplied layer preference.

use vaco_core::{Error, Result};

use super::Depacketizer;

/// VP9/RTP depacketiser.
#[derive(Debug, Default)]
pub struct Vp9Depacketizer {
    frame: Vec<u8>,
}

fn overflow() -> vaco_core::Error {
    Error::InvalidData("RTP VP9 descriptor overflows")
}

fn descriptor_len(payload: &[u8]) -> Result<usize> {
    let byte0 = *payload
        .first()
        .ok_or(Error::InvalidData("RTP VP9 payload is empty"))?;
    let has_picture_id = byte0 & 0x80 != 0; // I
    let inter_predicted = byte0 & 0x40 != 0; // P
    let has_layer_indices = byte0 & 0x20 != 0; // L
    let flexible = byte0 & 0x10 != 0; // F
    let has_scalability = byte0 & 0x02 != 0; // V

    let mut len = 1usize;

    if has_picture_id {
        let pid = *payload.get(len).ok_or(overflow())?;
        len = len.checked_add(1).ok_or(overflow())?;
        if pid & 0x80 != 0 {
            len = len.checked_add(1).ok_or(overflow())?;
        }
    }

    if has_layer_indices {
        len = len.checked_add(1).ok_or(overflow())?;
        if !flexible {
            len = len.checked_add(1).ok_or(overflow())?; // TL0PICIDX
        }
    }

    if flexible && inter_predicted {
        // Up to 3 reference-frame diffs, each 1 byte, N (bit0) = "more follow".
        for _ in 0..3 {
            let byte = *payload.get(len).ok_or(overflow())?;
            len = len.checked_add(1).ok_or(overflow())?;
            if byte & 0x01 == 0 {
                break;
            }
        }
    }

    if has_scalability {
        let ss = *payload.get(len).ok_or(overflow())?;
        len = len.checked_add(1).ok_or(overflow())?;
        let n_s = usize::from((ss >> 5) & 0x07)
            .checked_add(1)
            .ok_or(overflow())?;
        let has_resolutions = ss & 0x10 != 0; // Y
        let has_pg = ss & 0x08 != 0; // G

        if has_resolutions {
            let bytes = n_s.checked_mul(4).ok_or(overflow())?;
            len = len.checked_add(bytes).ok_or(overflow())?;
        }
        if has_pg {
            let n_g = usize::from(*payload.get(len).ok_or(overflow())?);
            len = len.checked_add(1).ok_or(overflow())?;
            for _ in 0..n_g {
                let g = *payload.get(len).ok_or(overflow())?;
                len = len.checked_add(1).ok_or(overflow())?;
                let r = usize::from((g >> 2) & 0x03);
                len = len.checked_add(r).ok_or(overflow())?;
            }
        }
    }

    Ok(len)
}

impl Depacketizer for Vp9Depacketizer {
    fn push(&mut self, marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let len = descriptor_len(payload)?;
        let body = payload.get(len..).ok_or(Error::InvalidData(
            "RTP VP9 descriptor runs past the payload",
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
    fn strips_minimal_descriptor() {
        let mut d = Vp9Depacketizer::default();
        let payload = [0x00u8, 1, 2, 3];
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(out, vec![1, 2, 3]);
    }

    #[test]
    fn strips_two_byte_picture_id() {
        let mut d = Vp9Depacketizer::default();
        // I=1, picture id byte has M bit set -> 2-byte picture id
        let payload = [0x80u8, 0x80, 0x01, 9, 9];
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(out, vec![9, 9]);
    }

    #[test]
    fn reassembles_until_marker() {
        let mut d = Vp9Depacketizer::default();
        assert_eq!(d.push(false, 0, &[0x00, 1, 2]).unwrap(), None);
        let out = d.push(true, 0, &[0x00, 3, 4]).unwrap().unwrap();
        assert_eq!(out, vec![1, 2, 3, 4]);
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let mut d = Vp9Depacketizer::default();
            let _ = d.push(true, 0, &bytes);
        }
    }
}
