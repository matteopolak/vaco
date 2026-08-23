//! RFC 2435: JPEG over RTP.
//!
//! RTP/JPEG never transmits Huffman tables (§3.1.5: "the Huffman coding
//! ... uses the tables ... specified in \[the RFC\]") and, for `Q < 128`,
//! never transmits quantization tables either — both are the *default*
//! ITU-T T.81 Annex K tables, scaled from `Q` by the algorithm in RFC 2435
//! Appendix A. Reconstructing a standalone JPEG file a generic decoder can
//! read means synthesising `DQT`/`DHT`/`SOF0`/`SOS` markers this module
//! never receives over the wire — [`DEFAULT_LUMA_QUANTIZER`],
//! [`DEFAULT_CHROMA_QUANTIZER`] and the four `DEFAULT_*_HUFFMAN_*` tables
//! below are transcribed from RFC 2435 Appendices A and B (a public
//! specification, not `FFmpeg` source — D7). **They have not been verified
//! byte-for-byte against a real JPEG decoder's output**; [`tests`] checks
//! internal consistency (every Huffman table's code-length counts sum to
//! its value-table length) rather than decoded-pixel correctness, so this
//! is reported as structurally complete and behaviourally unverified,
//! rather than claiming more than has actually been checked.
//!
//! **Implemented**: `Type` 0 and 1 (4:2:2 and 4:2:0 non-interleaved
//! sampling, §3.1.3), fragment reassembly by `Fragment Offset`, and an
//! explicit quantization table (`Q >= 128`, §3.1.8) when present on the
//! first fragment. **Not implemented**: restart markers (`Type` 64..=127,
//! §3.1.7) and `Type` values above 1 other than the restart-marker range.

use vaco_core::{Error, Result};

use super::Depacketizer;

/// RFC 2435 Appendix A's zig-zag scan order, used to lay out a quantization
/// table into natural (row-major) order for the `DQT` marker.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const DEFAULT_LUMA_QUANTIZER: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

const DEFAULT_CHROMA_QUANTIZER: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_LUMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_CHROMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D];
#[rustfmt::skip]
const AC_LUMA_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
#[rustfmt::skip]
const AC_CHROMA_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26,
    0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
    0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
    0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
    0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// RFC 2435 Appendix A's exact scaling algorithm for `Q < 128` (`Q == 0` is
/// treated as `1`, matching the RFC's own note that `Q` of 0 is nonsensical
/// but should not divide by zero).
fn scale_quantizer(base: &[u16; 64], q: u8) -> [u8; 64] {
    let q = q.max(1);
    #[allow(
        clippy::integer_division,
        reason = "RFC 2435 Appendix A's scaling formula is defined as integer division"
    )]
    let factor: i32 = if q < 50 {
        5000 / i32::from(q)
    } else {
        200 - 2 * i32::from(q)
    };
    let mut out = [0u8; 64];
    for (dst, &src) in out.iter_mut().zip(base.iter()) {
        #[allow(
            clippy::integer_division,
            reason = "RFC 2435 Appendix A's scaling formula is defined as integer division"
        )]
        let scaled = (i32::from(src) * factor + 50) / 100;
        *dst = u8::try_from(scaled.clamp(1, 255)).unwrap_or(255);
    }
    out
}

fn write_dqt(out: &mut Vec<u8>, table_id: u8, table: &[u8; 64]) {
    out.extend_from_slice(&[0xFF, 0xDB, 0x00, 67, table_id]);
    for &zz in &ZIGZAG {
        out.push(table.get(zz).copied().unwrap_or(0));
    }
}

fn write_dht(out: &mut Vec<u8>, class_and_id: u8, bits: &[u8; 16], values: &[u8]) {
    let len = 2 + 1 + 16 + values.len();
    out.extend_from_slice(&[0xFF, 0xC4]);
    out.extend_from_slice(&(u16::try_from(len).unwrap_or(u16::MAX)).to_be_bytes());
    out.push(class_and_id);
    out.extend_from_slice(bits);
    out.extend_from_slice(values);
}

/// Build a complete JFIF file from a reassembled RFC 2435 scan and its
/// header fields.
fn build_jpeg(
    width_px: u16,
    height_px: u16,
    type_specific: u8,
    luma: &[u8; 64],
    chroma: &[u8; 64],
    scan: &[u8],
) -> Result<Vec<u8>> {
    if type_specific > 1 {
        return Err(Error::Unsupported(
            "RTP JPEG restart-marker types (64..=127) are not implemented",
        ));
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0xFF, 0xD8]); // SOI
    write_dqt(&mut out, 0, luma);
    write_dqt(&mut out, 1, chroma);

    // SOF0 (baseline), 3 components: Y, Cb, Cr.
    let (h_y, v_y) = if type_specific == 0 { (2, 1) } else { (2, 2) };
    out.extend_from_slice(&[0xFF, 0xC0, 0x00, 17, 0x08]);
    out.extend_from_slice(&height_px.to_be_bytes());
    out.extend_from_slice(&width_px.to_be_bytes());
    out.push(3);
    out.extend_from_slice(&[1, (h_y << 4) | v_y, 0]);
    out.extend_from_slice(&[2, 0x11, 1]);
    out.extend_from_slice(&[3, 0x11, 1]);

    write_dht(&mut out, 0x00, &DC_LUMA_BITS, &DC_LUMA_VALUES);
    write_dht(&mut out, 0x10, &AC_LUMA_BITS, &AC_LUMA_VALUES);
    write_dht(&mut out, 0x01, &DC_CHROMA_BITS, &DC_CHROMA_VALUES);
    write_dht(&mut out, 0x11, &AC_CHROMA_BITS, &AC_CHROMA_VALUES);

    // SOS.
    out.extend_from_slice(&[0xFF, 0xDA, 0x00, 12, 3]);
    out.extend_from_slice(&[1, 0x00]);
    out.extend_from_slice(&[2, 0x11]);
    out.extend_from_slice(&[3, 0x11]);
    out.extend_from_slice(&[0, 63, 0]);

    out.extend_from_slice(scan);
    out.extend_from_slice(&[0xFF, 0xD9]); // EOI
    Ok(out)
}

/// RFC 2435 JPEG/RTP depacketiser.
#[derive(Debug, Default)]
pub struct JpegDepacketizer {
    scan: Vec<u8>,
    expected_offset: u32,
    type_specific: u8,
    width_px: u16,
    height_px: u16,
    q: u8,
    explicit_luma: Option<[u8; 64]>,
    explicit_chroma: Option<[u8; 64]>,
    started: bool,
}

impl Depacketizer for JpegDepacketizer {
    fn push(&mut self, marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let header: [u8; 8] =
            payload
                .get(0..8)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData(
                    "RTP JPEG payload shorter than its 8-byte header",
                ))?;
        let fragment_offset =
            (u32::from(header[1]) << 16) | (u32::from(header[2]) << 8) | u32::from(header[3]);
        let jpeg_type = header[4];
        let q = header[5];
        let width_px = u16::from(header[6]) * 8;
        let height_px = u16::from(header[7]) * 8;

        let mut cursor = 8usize;
        if fragment_offset == 0 {
            self.scan.clear();
            self.expected_offset = 0;
            self.type_specific = jpeg_type;
            self.width_px = width_px;
            self.height_px = height_px;
            self.q = q;
            self.explicit_luma = None;
            self.explicit_chroma = None;
            self.started = true;

            if q >= 128 {
                let qt: [u8; 4] = payload.get(8..12).and_then(|s| s.try_into().ok()).ok_or(
                    Error::InvalidData("RTP JPEG quantization header runs past the payload"),
                )?;
                let precision = qt[1];
                if precision != 0 {
                    return Err(Error::Unsupported(
                        "RTP JPEG 16-bit quantization table precision is not implemented",
                    ));
                }
                let len = usize::from(u16::from_be_bytes([qt[2], qt[3]]));
                let tables = payload.get(12..12 + len).ok_or(Error::InvalidData(
                    "RTP JPEG quantization tables run past the payload",
                ))?;
                let luma_bytes: [u8; 64] = tables
                    .get(0..64)
                    .and_then(|s| s.try_into().ok())
                    .ok_or(Error::InvalidData(
                        "RTP JPEG quantization tables are too short",
                    ))?;
                let chroma_bytes: [u8; 64] = tables
                    .get(64..128)
                    .and_then(|s| s.try_into().ok())
                    .unwrap_or(luma_bytes);
                self.explicit_luma = Some(luma_bytes);
                self.explicit_chroma = Some(chroma_bytes);
                cursor = 12usize.checked_add(len).ok_or(Error::InvalidData(
                    "RTP JPEG quantization header length overflows",
                ))?;
            }
        } else {
            if !self.started {
                return Err(Error::InvalidData(
                    "RTP JPEG fragment with a nonzero offset before any start-of-frame fragment",
                ));
            }
            if fragment_offset != self.expected_offset {
                return Err(Error::InvalidData(
                    "RTP JPEG fragment offset does not match the bytes received so far",
                ));
            }
        }

        let data = payload
            .get(cursor..)
            .ok_or(Error::InvalidData("RTP JPEG header runs past the payload"))?;
        self.scan.extend_from_slice(data);
        self.expected_offset = self
            .expected_offset
            .checked_add(u32::try_from(data.len()).unwrap_or(u32::MAX))
            .ok_or(Error::InvalidData("RTP JPEG fragment offset overflows"))?;

        if !marker {
            return Ok(None);
        }

        let luma = self
            .explicit_luma
            .unwrap_or_else(|| scale_quantizer(&DEFAULT_LUMA_QUANTIZER, self.q));
        let chroma = self
            .explicit_chroma
            .unwrap_or_else(|| scale_quantizer(&DEFAULT_CHROMA_QUANTIZER, self.q));
        let jpeg = build_jpeg(
            self.width_px,
            self.height_px,
            self.type_specific,
            &luma,
            &chroma,
            &self.scan,
        )?;
        self.started = false;
        Ok(Some(jpeg))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn main_header(offset: u32, jpeg_type: u8, q: u8, width8: u8, height8: u8) -> Vec<u8> {
        let mut h = vec![0u8];
        h.extend_from_slice(&offset.to_be_bytes()[1..]); // 24-bit
        h.push(jpeg_type);
        h.push(q);
        h.push(width8);
        h.push(height8);
        h
    }

    #[test]
    fn huffman_table_lengths_match_their_bit_counts() {
        assert_eq!(
            usize::try_from(AC_LUMA_BITS.iter().map(|&b| u32::from(b)).sum::<u32>()).unwrap(),
            AC_LUMA_VALUES.len()
        );
        assert_eq!(
            usize::try_from(AC_CHROMA_BITS.iter().map(|&b| u32::from(b)).sum::<u32>()).unwrap(),
            AC_CHROMA_VALUES.len()
        );
        assert_eq!(
            usize::try_from(DC_LUMA_BITS.iter().map(|&b| u32::from(b)).sum::<u32>()).unwrap(),
            DC_LUMA_VALUES.len()
        );
        assert_eq!(
            usize::try_from(DC_CHROMA_BITS.iter().map(|&b| u32::from(b)).sum::<u32>()).unwrap(),
            DC_CHROMA_VALUES.len()
        );
    }

    #[test]
    fn single_packet_frame_produces_a_valid_jfif_shell() {
        let mut d = JpegDepacketizer::default();
        let mut payload = main_header(0, 0, 50, 10, 8); // 80x64
        payload.extend_from_slice(b"scan-data-bytes");
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(&out[0..2], &[0xFF, 0xD8]);
        assert_eq!(&out[out.len() - 2..], &[0xFF, 0xD9]);
        assert!(
            out.windows(b"scan-data-bytes".len())
                .any(|w| w == b"scan-data-bytes")
        );
    }

    #[test]
    fn rejects_offset_mismatch() {
        let mut d = JpegDepacketizer::default();
        let mut first = main_header(0, 0, 50, 1, 1);
        first.extend_from_slice(b"abcd");
        d.push(false, 0, &first).unwrap();
        let mut second = main_header(999, 0, 50, 1, 1);
        second.extend_from_slice(b"efgh");
        assert!(d.push(true, 0, &second).is_err());
    }

    #[test]
    fn reassembles_two_fragments() {
        let mut d = JpegDepacketizer::default();
        let mut first = main_header(0, 0, 50, 1, 1);
        first.extend_from_slice(b"ABCD");
        assert_eq!(d.push(false, 0, &first).unwrap(), None);
        let mut second = main_header(4, 0, 50, 1, 1);
        second.extend_from_slice(b"EFGH");
        let out = d.push(true, 0, &second).unwrap().unwrap();
        assert!(out.windows(8).any(|w| w == b"ABCDEFGH"));
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..300)) {
            let mut d = JpegDepacketizer::default();
            let _ = d.push(true, 0, &bytes);
        }
    }
}
