//! The MPEG-2 section CRC-32 (ISO/IEC 13818-1 Annex B).
//!
//! One polynomial, and getting any of its four parameters wrong makes every
//! section look subtly broken rather than obviously broken — the reference
//! discards a failing section *silently*, so the symptom is a missing program,
//! not an error message.
//!
//! | Parameter | Value | Where the spec says so |
//! |---|---|---|
//! | polynomial | `0x04C1_1DB7` | Annex B gives the generator as `x^32 + x^26 + x^23 + x^22 + x^16 + x^12 + x^11 + x^10 + x^8 + x^7 + x^5 + x^4 + x^2 + x + 1` |
//! | initial value | `0xFFFF_FFFF` | Annex B: the register is preset to all ones |
//! | reflection | **none**, in or out | the decoder is defined over the bit stream in transmission order, most significant bit first |
//! | final XOR | none | Annex B has no output inversion |
//!
//! The table is spec-dictated: there is exactly one correct set of 256 values
//! for that generator, so it is merger material under D15 and is generated here
//! by a `const fn` rather than transcribed from anywhere.
//!
//! # The property that makes it cheap to use
//!
//! Running the CRC over the whole section *including* its own trailing four
//! CRC bytes yields zero for a valid section. [`section_crc_ok`] is that check
//! and is the only one a caller needs.

/// The generator polynomial, most-significant-bit-first.
pub const POLY: u32 = 0x04C1_1DB7;

/// The value the register is preset to.
pub const INIT: u32 = 0xFFFF_FFFF;

/// A valid section, CRC bytes included, reduces to this.
pub const RESIDUE: u32 = 0;

/// One byte's worth of the polynomial division, unreflected.
const fn table_entry(byte: u8) -> u32 {
    let mut crc = (byte as u32) << 24;
    let mut bit = 0;
    while bit < 8 {
        crc = if crc & 0x8000_0000 != 0 {
            (crc << 1) ^ POLY
        } else {
            crc << 1
        };
        bit += 1;
    }
    crc
}

#[allow(
    clippy::indexing_slicing,
    reason = "SAFETY-ARG: `i` is bounded by the loop condition `i < 256` and the \
              array is declared with exactly 256 elements, so the index is in \
              range at every iteration. `get_mut` is not available in a const fn."
)]
const fn build_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = table_entry(i as u8);
        i += 1;
    }
    t
}

/// Byte-wise division table for [`POLY`].
static TABLE: [u32; 256] = build_table();

/// Continue a CRC over `data`.
///
/// Exposed separately from [`crc32`] so a caller that has the section in two
/// pieces — which the section assembler does, before it copies — can avoid the
/// copy.
#[must_use]
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        let idx = ((crc >> 24) as u8 ^ b) as usize;
        // `idx` is a `u8` widened to `usize`, so it is in `0..256` by
        // construction; `get` keeps that fact local rather than asserting it.
        let entry = match TABLE.get(idx) {
            Some(v) => *v,
            None => 0,
        };
        crc = (crc << 8) ^ entry;
    }
    crc
}

/// The MPEG-2 CRC-32 of `data`.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(INIT, data)
}

/// Whether `section` — header, body and its own trailing four CRC bytes —
/// carries a valid CRC.
///
/// A section shorter than the four CRC bytes cannot be valid whatever it
/// contains, and is rejected without running the loop.
#[must_use]
pub fn section_crc_ok(section: &[u8]) -> bool {
    section.len() >= 4 && crc32(section) == RESIDUE
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// The bitwise definition, used as an independent oracle for the table.
    fn crc32_bitwise(data: &[u8]) -> u32 {
        let mut crc = INIT;
        for &b in data {
            for i in (0..8).rev() {
                let bit = (b >> i) & 1;
                let top = u32::from(crc >> 31 != 0);
                crc <<= 1;
                if top ^ u32::from(bit) != 0 {
                    crc ^= POLY;
                }
            }
        }
        crc
    }

    #[test]
    fn table_agrees_with_the_bitwise_definition() {
        for len in 0..40usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(crc32(&data), crc32_bitwise(&data), "len {len}");
        }
    }

    /// The published check value for CRC-32/MPEG-2 over `"123456789"`.
    #[test]
    fn published_check_value() {
        assert_eq!(crc32(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn appending_the_crc_makes_the_residue_zero() {
        let body = b"\x00\xb0\x0d\x00\x01\xc1\x00\x00\x00\x01\xe1\x00";
        let crc = crc32(body);
        let mut section = body.to_vec();
        section.extend_from_slice(&crc.to_be_bytes());
        assert!(section_crc_ok(&section));
        // One flipped bit anywhere must fail.
        for i in 0..section.len() {
            let mut bad = section.clone();
            bad[i] ^= 0x01;
            assert!(!section_crc_ok(&bad), "bit flip at {i} went undetected");
        }
    }

    #[test]
    fn a_short_section_is_never_valid() {
        assert!(!section_crc_ok(&[]));
        assert!(!section_crc_ok(&[0, 0, 0]));
    }

    #[test]
    fn update_is_associative_over_a_split() {
        let data: Vec<u8> = (0..100u8).collect();
        for split in 0..=data.len() {
            let (a, b) = data.split_at(split);
            assert_eq!(crc32_update(crc32_update(INIT, a), b), crc32(&data));
        }
    }
}
