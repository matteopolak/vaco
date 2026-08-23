//! RFC 4629: H.263 (H.263-1998/H.263-2000) over RTP.
//!
//! §5.1's 2-byte mandatory header — `RR`(5)+`P`(1)+`V`(1)+`PLEN`(6)+`PEBIT`(3)
//! — carries no fragmentation-unit state machine at all: a picture is simply
//! every RTP packet from one with a fresh accumulator to the one with the
//! marker bit set, in sequence-number order. When `P` is set the two H.263
//! picture/GOB/slice start-code zero bytes it elides are reconstructed.
//! `V` (a following VRC byte) and `PLEN` (a following extra picture header)
//! are skipped rather than interpreted, matching what a decoder consuming
//! the reconstructed bitstream needs. **Not implemented**: RFC 2190's older
//! "H263" mode (RFC 3551's *static* PT 34 assignment) — this module only
//! implements the dynamic `H263-1998`/`H263-2000` `a=rtpmap` framing, which
//! is what `ffmpeg -h muxer=rtp`'s default (`rfc2190` is an opt-in flag) and
//! every modern RTSP camera this crate was checked against actually sends.

use vaco_core::{Error, Result};

use super::Depacketizer;

/// H.263/RTP depacketiser (RFC 4629 dynamic mode).
#[derive(Debug, Default)]
pub struct H263Depacketizer {
    picture: Vec<u8>,
}

impl Depacketizer for H263Depacketizer {
    fn push(&mut self, marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let header: [u8; 2] =
            payload
                .get(0..2)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData(
                    "RTP H.263 payload shorter than its 2-byte header",
                ))?;
        let has_picture_start = header[0] & 0x04 != 0; // P
        let has_vrc = header[0] & 0x02 != 0; // V
        let extra_len = usize::from(((header[0] & 0x01) << 5) | (header[1] >> 3)); // PLEN

        let mut skip = 2usize;
        if has_vrc {
            skip = skip
                .checked_add(1)
                .ok_or(Error::InvalidData("RTP H.263 header overflows"))?;
        }
        skip = skip
            .checked_add(extra_len)
            .ok_or(Error::InvalidData("RTP H.263 PLEN overflows"))?;
        let body = payload
            .get(skip..)
            .ok_or(Error::InvalidData("RTP H.263 header runs past the payload"))?;

        if has_picture_start {
            self.picture.extend_from_slice(&[0, 0]);
        }
        self.picture.extend_from_slice(body);

        if marker {
            let out = std::mem::take(&mut self.picture);
            Ok(Some(out))
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
    fn reassembles_a_picture_start_across_two_packets() {
        let mut d = H263Depacketizer::default();
        let first = [0x04u8, 0x00, 0x80, 1, 2]; // P=1
        let last = [0x00u8, 0x00, 3, 4]; // continuation
        assert_eq!(d.push(false, 0, &first).unwrap(), None);
        let out = d.push(true, 0, &last).unwrap().unwrap();
        assert_eq!(out, vec![0, 0, 0x80, 1, 2, 3, 4]);
    }

    #[test]
    fn skips_vrc_byte_when_v_is_set() {
        let mut d = H263Depacketizer::default();
        let payload = [0x02u8, 0x00, 0xFF /* VRC */, 1, 2];
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(out, vec![1, 2]);
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let mut d = H263Depacketizer::default();
            let _ = d.push(true, 0, &bytes);
        }
    }
}
