//! RFC 4175: uncompressed video over RTP.
//!
//! §4.1's payload header is a 16-bit extended sequence number followed by
//! one or more scan-line headers (`Length`(16)+`Line Number`(15)+`C`(1,
//! continuation)+`Offset`(15)+`F`(1, field id)), each naming where its
//! pixel-data segment (which follows *after* the last header in the chain)
//! belongs in the frame. **This module concatenates the segments in wire
//! order rather than placing each one at its stated `Line Number`/`Offset`**
//! — correct for the overwhelmingly common case (segments arrive in raster
//! order, one per scan line, nothing dropped) and wrong for a genuinely
//! reordered or partial-update stream, which is a real gap: implementing
//! placement needs the frame's `width`/`sampling` (from `a=fmtp`, which
//! this depacketiser is not handed) to compute a byte offset, not just the
//! header's own fields.

use vaco_core::{Error, Result};

use super::Depacketizer;

/// RFC 4175 uncompressed-video depacketiser.
#[derive(Debug, Default)]
pub struct RawVideoDepacketizer {
    frame: Vec<u8>,
}

impl Depacketizer for RawVideoDepacketizer {
    fn push(&mut self, marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut headers = payload.get(2..).ok_or(Error::InvalidData(
            "RTP raw-video payload has no line headers",
        ))?;
        let mut lengths = Vec::new();
        loop {
            let hdr: [u8; 6] =
                headers
                    .get(0..6)
                    .and_then(|s| s.try_into().ok())
                    .ok_or(Error::InvalidData(
                        "RTP raw-video line header runs past the payload",
                    ))?;
            let length = usize::from(u16::from_be_bytes([hdr[0], hdr[1]]));
            let continuation = hdr[2] & 0x80 != 0;
            lengths.push(length);
            headers = headers.get(6..).ok_or(Error::InvalidData(
                "RTP raw-video line header arithmetic is inconsistent",
            ))?;
            if !continuation {
                break;
            }
        }

        let mut data = headers;
        for length in lengths {
            let segment = data.get(..length).ok_or(Error::InvalidData(
                "RTP raw-video pixel segment runs past the payload",
            ))?;
            self.frame.extend_from_slice(segment);
            data = data.get(length..).ok_or(Error::InvalidData(
                "RTP raw-video pixel segment arithmetic is inconsistent",
            ))?;
        }

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
    fn extracts_a_single_line_segment() {
        let mut d = RawVideoDepacketizer::default();
        let mut payload = vec![0u8, 0]; // extended sequence number
        payload.extend_from_slice(&4u16.to_be_bytes()); // length=4
        payload.extend_from_slice(&[0x00, 0x00]); // line number/C=0
        payload.extend_from_slice(&[0x00, 0x00]); // offset/F=0
        payload.extend_from_slice(b"abcd");
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(out, b"abcd".to_vec());
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let mut d = RawVideoDepacketizer::default();
            let _ = d.push(true, 0, &bytes);
        }
    }
}
