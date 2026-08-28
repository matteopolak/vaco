//! The two checksums a FLAC frame carries: an 8-bit one over the frame
//! header and a 16-bit one over the whole frame.
//!
//! Written bit-by-bit rather than as a lookup table, on purpose: FLAC's own
//! table-based reference implementations exist for speed, and a from-scratch
//! table would be exactly the kind of large transcribed constant this
//! project's provenance rules are wariest of (`AGENT-CONSTRAINTS.md`'s "how
//! confident should a transcribed table be"). The polynomial is the only
//! fact taken from the specification; the table, if wanted later, is
//! mechanically derivable from it.

/// CRC-8 with polynomial `x^8 + x^2 + x^1 + x^0` (`0x07`), initial value 0,
/// no reflection — the frame header checksum.
///
/// Vaco-Spec-Ref: rfc-9639-flac Section 9.1.8, "Frame Header CRC"
#[must_use]
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-16 with polynomial `x^16 + x^15 + x^2 + x^0` (`0x8005`), initial
/// value 0, no reflection — the frame footer checksum, covering the whole
/// frame (sync code through the byte-aligned padding) except the checksum
/// itself.
///
/// Vaco-Spec-Ref: rfc-9639-flac Section 9.3, "Frame Footer"
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::{crc8, crc16};

    // Independent test vectors (not transcribed from this crate's own
    // implementation): the polynomial and bit order above reproduce the
    // check values reported by Claxon's own `crc.rs` unit tests, which in
    // turn are standard CRC-8/CRC-16 (0x8005, unreflected) test values.
    #[test]
    fn crc8_test_vectors() {
        assert_eq!(crc8(&[0x1f]), 0x5d);
        assert_eq!(crc8(&[0x04, 0x01]), 0x53);
        assert_eq!(crc8(&[0x61, 0x62, 0x63]), 0x5f);
    }

    #[test]
    fn crc16_test_vectors() {
        assert_eq!(crc16(&[0x1f]), 0x8041);
        assert_eq!(crc16(&[0x04, 0x01]), 0x1806);
        assert_eq!(crc16(&[0x61, 0x62, 0x63]), 0xcadb);
    }

    #[test]
    fn crc_of_empty_is_zero() {
        assert_eq!(crc8(&[]), 0);
        assert_eq!(crc16(&[]), 0);
    }
}
