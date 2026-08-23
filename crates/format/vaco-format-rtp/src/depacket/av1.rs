//! AOM's "RTP Payload Format For AV1", v1.0 §5: AV1 over RTP.
//!
//! Not an IETF RFC — a public specification published by the Alliance for
//! Open Media (the same body that publishes the AV1 bitstream spec this
//! workspace already cites elsewhere), so citing it is the same kind of
//! clean-room source as an RFC (D7): a functional specification, not
//! `FFmpeg`'s implementation of one.
//!
//! Each packet starts with a 1-byte aggregation header (`Z`|`Y`|`W`(2
//! bits)|`N`|reserved(3)) naming how many OBU elements follow: all but the
//! last are LEB128-length-prefixed, the last runs to the end of the
//! payload. **Not implemented**: `Z`/`Y` (an OBU element fragmented across
//! packet boundaries) — this module requires every OBU element to be
//! complete within one packet and reports [`vaco_core::Error::Unsupported`]
//! otherwise, and `W == 0` (element count left for the receiver to infer
//! from the bitstream) is treated as exactly one element filling the
//! payload, which is what every encoder this crate was checked against
//! actually emits for `W == 0`.

use vaco_core::{Error, Result};

use super::Depacketizer;

/// AV1/RTP depacketiser. Accumulates OBUs across packets that share one
/// temporal unit (RFC-analogous "marker bit ends the unit" convention, the
/// spec's §5 "the RTP marker bit MUST be set ... for the last packet ... of
/// a temporal unit").
#[derive(Debug, Default)]
pub struct Av1Depacketizer {
    unit: Vec<u8>,
}

/// Read a LEB128-encoded length, per the AV1 spec's own `leb128()` (used
/// identically for OBU sizes in the bitstream itself). Returns the decoded
/// value and the number of bytes consumed. Bounded to 4 bytes / 28 bits,
/// which is far more than any single RTP payload could need to express.
fn read_leb128(buf: &[u8]) -> Result<(u64, usize)> {
    let mut value: u64 = 0;
    for i in 0..4usize {
        let byte = *buf.get(i).ok_or(Error::InvalidData(
            "AV1 RTP OBU element length runs past the payload",
        ))?;
        value |= u64::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok((value, i + 1));
        }
    }
    Err(Error::InvalidData(
        "AV1 RTP OBU element LEB128 length is too long",
    ))
}

impl Depacketizer for Av1Depacketizer {
    fn push(&mut self, marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let header = *payload
            .first()
            .ok_or(Error::InvalidData("RTP AV1 payload is empty"))?;
        let z = header & 0x80 != 0;
        let y = header & 0x40 != 0;
        let w = (header >> 4) & 0x03;
        if z || y {
            return Err(Error::Unsupported(
                "AV1 RTP OBU elements fragmented across packets are not implemented",
            ));
        }

        let mut rest = payload.get(1..).ok_or(Error::InvalidData(
            "RTP AV1 payload has no data after its header",
        ))?;
        let element_count = if w == 0 { 1 } else { w };
        for i in 0..element_count {
            let is_last = i + 1 == element_count;
            let element = if is_last {
                let e = rest;
                rest = &[];
                e
            } else {
                let (len, used) = read_leb128(rest)?;
                let len = usize::try_from(len)
                    .map_err(|_| Error::InvalidData("AV1 RTP OBU element length overflows"))?;
                let start = rest.get(used..).ok_or(Error::InvalidData(
                    "AV1 RTP OBU element length prefix runs past the payload",
                ))?;
                let element = start.get(..len).ok_or(Error::InvalidData(
                    "AV1 RTP OBU element runs past the payload",
                ))?;
                rest = start.get(len..).ok_or(Error::InvalidData(
                    "AV1 RTP OBU element arithmetic is inconsistent",
                ))?;
                element
            };
            self.unit.extend_from_slice(element);
        }

        if marker {
            Ok(Some(std::mem::take(&mut self.unit)))
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
    fn w_zero_is_one_element_filling_the_payload() {
        let mut d = Av1Depacketizer::default();
        let payload = [0x00u8, 1, 2, 3];
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(out, vec![1, 2, 3]);
    }

    #[test]
    fn w_two_splits_into_two_elements() {
        let mut d = Av1Depacketizer::default();
        // W=2 (bits 4-5 = 10 -> 0x20), first element length=2 (leb128 single byte 0x02)
        let payload = [0x20u8, 0x02, 0xAA, 0xBB, 0xCC, 0xDD];
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(out, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn rejects_fragmentation_flags() {
        let mut d = Av1Depacketizer::default();
        assert!(d.push(true, 0, &[0x80, 1, 2]).is_err());
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let mut d = Av1Depacketizer::default();
            let _ = d.push(true, 0, &bytes);
        }
    }
}
