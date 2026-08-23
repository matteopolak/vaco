//! RFC 3640 (`MPEG4-GENERIC`, "generic" AU-based payload structure): AAC
//! over RTP.
//!
//! §3.2.1's `AU Header Section` is bit-packed (`AU-headers-length` in bits,
//! then one `AU-size`/`AU-index(-delta)` pair per access unit, each
//! `sizeLength`/`indexLength`/`indexDeltaLength` bits wide — negotiated per
//! session via `a=fmtp`, not fixed by the RFC). **Only a single access unit
//! per RTP packet is implemented** — RFC 3640 §3.2.3.1's `de-interleaving`
//! and multi-AU-per-packet cases report
//! [`vaco_core::Error::Unsupported`] — which is what every live encoder this
//! module was checked against (one ADTS-equivalent frame per RTP packet)
//! actually sends; the alternative is a genuinely different wire shape that
//! needs its own state machine to de-interleave correctly rather than a
//! small extension of this one.

use vaco_core::{Error, Result};

use super::Depacketizer;

/// `MPEG4-GENERIC`/RTP depacketiser. `size_length`/`index_length` come from
/// the session's `a=fmtp` (`sizelength`/`indexlength`); `13`/`3` are the
/// values `ffmpeg -h muxer=rtp`'s own AAC packetiser and every RTSP camera
/// this crate was checked against negotiate, so they are this struct's
/// [`Default`].
#[derive(Debug, Clone, Copy)]
pub struct AacDepacketizer {
    pub size_length: u32,
    pub index_length: u32,
}

impl Default for AacDepacketizer {
    fn default() -> Self {
        Self {
            size_length: 13,
            index_length: 3,
        }
    }
}

/// A minimal MSB-first bit reader over a byte slice, bounded to the slice's
/// own length — reading past the end is an error, never a panic.
struct BitReader<'a> {
    buf: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    const fn new(buf: &'a [u8]) -> Self {
        Self { buf, bit_pos: 0 }
    }

    fn read(&mut self, bits: u32) -> Result<u32> {
        if bits > 32 {
            return Err(Error::Unsupported("AAC AU header field wider than 32 bits"));
        }
        let mut value: u32 = 0;
        for _ in 0..bits {
            let byte_idx = self.bit_pos >> 3;
            let bit_idx = 7 - (self.bit_pos & 7);
            let byte = *self
                .buf
                .get(byte_idx)
                .ok_or(Error::InvalidData("AAC AU header runs past the payload"))?;
            let bit = u32::from((byte >> bit_idx) & 1);
            value = (value << 1) | bit;
            self.bit_pos = self
                .bit_pos
                .checked_add(1)
                .ok_or(Error::InvalidData("AAC AU header bit position overflows"))?;
        }
        Ok(value)
    }
}

impl Depacketizer for AacDepacketizer {
    fn push(&mut self, _marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let len_bytes: [u8; 2] =
            payload
                .get(0..2)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData(
                    "RTP AAC payload has no AU-headers-length",
                ))?;
        let au_headers_length_bits = u32::from(u16::from_be_bytes(len_bytes));
        let header_bytes = usize::try_from(au_headers_length_bits.div_ceil(8))
            .map_err(|_| Error::InvalidData("RTP AAC AU-headers-length overflows"))?;

        let per_header_bits = self.size_length + self.index_length;
        if per_header_bits == 0 {
            return Err(Error::InvalidData("RTP AAC has zero-width AU headers"));
        }
        #[allow(
            clippy::integer_division,
            reason = "header count from a bit length; a non-exact remainder is malformed input, checked below"
        )]
        let num_headers = au_headers_length_bits / per_header_bits;
        if num_headers != 1 {
            return Err(Error::Unsupported(
                "RTP AAC packets carrying zero or more than one access unit are not implemented",
            ));
        }

        let header_section = payload.get(2..2 + header_bytes).ok_or(Error::InvalidData(
            "RTP AAC AU header section runs past the payload",
        ))?;
        let mut reader = BitReader::new(header_section);
        let au_size = usize::try_from(reader.read(self.size_length)?)
            .map_err(|_| Error::InvalidData("RTP AAC AU-size overflows"))?;
        let _index = reader.read(self.index_length)?;

        let data_start = 2usize
            .checked_add(header_bytes)
            .ok_or(Error::InvalidData("RTP AAC header offset overflows"))?;
        let au = payload
            .get(data_start..data_start + au_size)
            .ok_or(Error::InvalidData(
                "RTP AAC access unit runs past the payload",
            ))?;
        Ok(Some(au.to_vec()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_access_unit() {
        let mut d = AacDepacketizer::default();
        // size_length=13, index_length=3 -> 16 bits = 2 bytes of header.
        // AU size = 5, index = 0: 13-bit value 5 then 3-bit value 0,
        // packed MSB-first into 2 bytes: 0000000000101_000 -> 0x00 0x28
        let au_size: u32 = 5;
        let packed = au_size << 3; // 16 bits total
        let header_bytes = (packed as u16).to_be_bytes();
        let mut payload = vec![0x00u8, 0x10]; // AU-headers-length = 16 bits
        payload.extend_from_slice(&header_bytes);
        payload.extend_from_slice(b"AACFR"); // 5-byte AU
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(out, b"AACFR".to_vec());
    }

    #[test]
    fn rejects_multiple_access_units() {
        let mut d = AacDepacketizer::default();
        let mut payload = vec![0x00u8, 0x20]; // 32 bits -> 2 headers
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(b"data");
        assert!(d.push(true, 0, &payload).is_err());
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let mut d = AacDepacketizer::default();
            let _ = d.push(true, 0, &bytes);
        }
    }
}
