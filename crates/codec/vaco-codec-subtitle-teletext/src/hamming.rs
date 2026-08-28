//! Hamming 8/4 and Hamming 24/18 forward error correction (EN 300 706 §8.2,
//! §8.3).
//!
//! Both decoders are derived directly from the specification's own encoding
//! equations rather than transcribed from its (scanned, hand-checkable-only-
//! by-eye) decision tables: every parity bit's defining equation names
//! exactly which data bits feed it, and a bit's membership pattern across
//! those equations is a standard extended-Hamming position number. That
//! position numbering was verified against the spec's tables (both bit
//! layout figures and the worked decision tables) before being trusted, but
//! the arithmetic below comes from the algebra, which is auditable, not
//! from reading dots off a page.

/// Outcome of correcting a Hamming-protected byte or triplet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Correction {
    /// No error detected.
    Clean,
    /// A single-bit error was found and corrected (in the payload or in a
    /// parity/check bit that does not affect the payload).
    Corrected,
    /// A double-bit error was detected. The spec's own decision table says
    /// to reject the data bits in this case; the returned payload should not
    /// be used.
    Uncorrectable,
}

impl Correction {
    /// Whether the returned payload is safe to use.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        !matches!(self, Self::Uncorrectable)
    }
}

fn bit(byte: u8, n: u32) -> u8 {
    (byte >> n) & 1
}

/// Hamming 8/4 decode: one byte in, a 4-bit data nibble (`D1` in bit 0,
/// `D4` in bit 3) and the correction outcome out.
///
/// Transmission order (EN 300 706 §8.2, bits 1-8 LSB first) is `P1 D1 P2 D2
/// P3 D3 P4 D4`, so byte bit 0 is `P1` and byte bit 7 is `D4`.
#[must_use]
pub fn decode8(byte: u8) -> (u8, Correction) {
    let p1 = bit(byte, 0);
    let d1 = bit(byte, 1);
    let p2 = bit(byte, 2);
    let d2 = bit(byte, 3);
    let p3 = bit(byte, 4);
    let d3 = bit(byte, 5);
    let p4 = bit(byte, 6);
    let d4 = bit(byte, 7);

    // Re-derive each parity bit from the received data and compare: zero
    // means that check is consistent with what was received.
    let s1 = p1 ^ d1 ^ d3 ^ d4 ^ 1;
    let s2 = p2 ^ d1 ^ d2 ^ d4 ^ 1;
    let s3 = p3 ^ d1 ^ d2 ^ d3 ^ 1;
    let syndrome = s1 | (s2 << 1) | (s3 << 2);
    // The whole byte has odd parity when every bit is correct (P4's own
    // defining equation is an odd-parity check over all seven other bits).
    let overall_odd = (p1 ^ d1 ^ p2 ^ d2 ^ p3 ^ d3 ^ p4 ^ d4) == 1;

    let (mut o1, mut o2, mut o3, mut o4) = (d1, d2, d3, d4);
    let correction = if syndrome == 0 && overall_odd {
        Correction::Clean
    } else if syndrome == 0 {
        Correction::Corrected // error confined to P4
    } else if overall_odd {
        Correction::Uncorrectable
    } else {
        // Standard extended-Hamming position numbering: 1=P1, 2=P2, 3=D4,
        // 4=P3, 5=D3, 6=D2, 7=D1 (verified against §8.2's own equations:
        // D1 feeds all three checks (7 = 1|2|4), D2 feeds P2/P3 (6), D3
        // feeds P1/P3 (5), D4 feeds P1/P2 (3)).
        match syndrome {
            3 => o4 ^= 1,
            5 => o3 ^= 1,
            6 => o2 ^= 1,
            7 => o1 ^= 1,
            _ => {} // 1, 2 or 4: a parity bit, payload unaffected
        }
        Correction::Corrected
    };

    let nibble = o1 | (o2 << 1) | (o3 << 2) | (o4 << 3);
    (nibble, correction)
}

fn bit32(word: u32, n: u32) -> u32 {
    (word >> n) & 1
}

fn xor_bits(word: u32, positions: &[u32]) -> u32 {
    positions.iter().fold(0u32, |acc, &p| acc ^ bit32(word, p))
}

/// Hamming 24/18 decode over one triplet of three consecutive bytes
/// (EN 300 706 §8.3): 18 data bits out (`D1` in bit 0, `D18` in bit 17) plus
/// the correction outcome.
///
/// `bytes[0]` carries transmission bits 1-8, `bytes[1]` bits 9-16,
/// `bytes[2]` bits 17-24, all LSB first, matching [`decode8`]'s convention.
#[must_use]
pub fn decode24(bytes: [u8; 3]) -> (u32, Correction) {
    let raw = u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16);

    // Position numbering follows the standard construction confirmed
    // against every one of §8.3's five encoding equations: parity bits sit
    // at positions 1, 2, 4, 8 and 16; data fills the rest in order.
    let p1 = bit32(raw, 0);
    let p2 = bit32(raw, 1);
    let p3 = bit32(raw, 3);
    let p4 = bit32(raw, 7);
    let p5 = bit32(raw, 15);

    let s1 = p1 ^ xor_bits(raw, &[2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22]) ^ 1;
    let s2 = p2 ^ xor_bits(raw, &[2, 5, 6, 9, 10, 13, 14, 17, 18, 21, 22]) ^ 1;
    let s3 = p3 ^ xor_bits(raw, &[4, 5, 6, 11, 12, 13, 14, 19, 20, 21, 22]) ^ 1;
    let s4 = p4 ^ xor_bits(raw, &[8, 9, 10, 11, 12, 13, 14]) ^ 1;
    let s5 = p5 ^ xor_bits(raw, &[16, 17, 18, 19, 20, 21, 22]) ^ 1;
    let syndrome = s1 | (s2 << 1) | (s3 << 2) | (s4 << 3) | (s5 << 4);

    let overall_odd = (0..24).fold(0u32, |acc, n| acc ^ bit32(raw, n)) == 1;

    let mut corrected = raw;
    let correction = if syndrome == 0 && overall_odd {
        Correction::Clean
    } else if syndrome == 0 {
        Correction::Corrected // error confined to P6 (position 24)
    } else if overall_odd {
        Correction::Uncorrectable
    } else {
        // `syndrome` (1-23) is the 1-indexed bit position of the single
        // error, in the same numbering used above.
        if let Some(shift) = syndrome.checked_sub(1) {
            corrected ^= 1u32.checked_shl(shift).unwrap_or(0);
        }
        Correction::Corrected
    };

    // Extract the 18 data-bit positions (everything but 1,2,4,8,16 and the
    // overall-parity bit 24) into a packed, contiguous D1..D18 value.
    let mut data = 0u32;
    let mut out_bit = 0u32;
    for pos in 0..23u32 {
        if matches!(pos, 0 | 1 | 3 | 7 | 15) {
            continue;
        }
        data |= bit32(corrected, pos) << out_bit;
        out_bit = out_bit.saturating_add(1);
    }
    (data, correction)
}

/// The 6-bit address field of a decoded X/26, X/27 or X/28 triplet
/// (EN 300 706 §12.3.1): bits 1-6 of the 18-bit payload.
#[must_use]
pub const fn triplet_address(data: u32) -> u8 {
    (data & 0x3F) as u8
}

/// The 5-bit mode field of a decoded triplet: bits 7-11.
#[must_use]
pub const fn triplet_mode(data: u32) -> u8 {
    ((data >> 6) & 0x1F) as u8
}

/// The 7-bit data field of a decoded triplet: bits 12-18.
#[must_use]
pub const fn triplet_data(data: u32) -> u8 {
    ((data >> 11) & 0x7F) as u8
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn encode8(d1: u8, d2: u8, d3: u8, d4: u8) -> u8 {
        let p1 = 1 ^ d1 ^ d3 ^ d4;
        let p2 = 1 ^ d1 ^ d2 ^ d4;
        let p3 = 1 ^ d1 ^ d2 ^ d3;
        let p4 = 1 ^ p1 ^ d1 ^ p2 ^ d2 ^ p3 ^ d3 ^ d4;
        (p1 & 1)
            | ((d1 & 1) << 1)
            | ((p2 & 1) << 2)
            | ((d2 & 1) << 3)
            | ((p3 & 1) << 4)
            | ((d3 & 1) << 5)
            | ((p4 & 1) << 6)
            | ((d4 & 1) << 7)
    }

    #[test]
    fn decode8_round_trips_every_nibble() {
        for n in 0u8..16 {
            let d1 = n & 1;
            let d2 = (n >> 1) & 1;
            let d3 = (n >> 2) & 1;
            let d4 = (n >> 3) & 1;
            let byte = encode8(d1, d2, d3, d4);
            let (nibble, correction) = decode8(byte);
            assert_eq!(correction, Correction::Clean);
            assert_eq!(nibble, n);
        }
    }

    #[test]
    fn decode8_corrects_every_single_bit_flip() {
        for n in 0u8..16 {
            let d1 = n & 1;
            let d2 = (n >> 1) & 1;
            let d3 = (n >> 2) & 1;
            let d4 = (n >> 3) & 1;
            let byte = encode8(d1, d2, d3, d4);
            for flip in 0u32..8 {
                let Some(shifted) = 1u8.checked_shl(flip) else {
                    continue;
                };
                let flipped = byte ^ shifted;
                let (nibble, correction) = decode8(flipped);
                assert_eq!(correction, Correction::Corrected, "flip bit {flip}");
                assert_eq!(nibble, n, "flip bit {flip}");
            }
        }
    }

    #[test]
    fn decode8_detects_a_double_bit_error() {
        let byte = encode8(0, 1, 0, 1);
        let flipped = byte ^ 0b0000_0011;
        let (_, correction) = decode8(flipped);
        assert_eq!(correction, Correction::Uncorrectable);
    }

    fn encode24(bits: [u8; 18]) -> [u8; 3] {
        let get = |positions: &[usize]| -> u32 {
            positions
                .iter()
                .fold(0u32, |acc, &p| acc ^ u32::from(bits.get(p).copied().unwrap_or(0)))
        };
        let p1 = 1 ^ get(&[0, 1, 3, 4, 6, 8, 10, 11, 13, 15, 17]);
        let p2 = 1 ^ get(&[0, 2, 3, 5, 6, 9, 10, 12, 13, 16, 17]);
        let p3 = 1 ^ get(&[1, 2, 3, 7, 8, 9, 10, 14, 15, 16, 17]);
        let p4 = 1 ^ get(&[4, 5, 6, 7, 8, 9, 10]);
        let p5 = 1 ^ get(&[11, 12, 13, 14, 15, 16, 17]);

        let mut raw = 0u32;
        raw |= p1 & 1;
        raw |= (p2 & 1) << 1;
        raw |= u32::from(bits[0]) << 2;
        raw |= (p3 & 1) << 3;
        raw |= u32::from(bits[1]) << 4;
        raw |= u32::from(bits[2]) << 5;
        raw |= u32::from(bits[3]) << 6;
        raw |= (p4 & 1) << 7;
        for (i, &d) in bits.iter().enumerate().skip(4).take(7) {
            raw |= u32::from(d) << (8 + (i - 4));
        }
        raw |= (p5 & 1) << 15;
        for (i, &d) in bits.iter().enumerate().skip(11) {
            raw |= u32::from(d) << (16 + (i - 11));
        }
        let p6 = 1 ^ (0..23).fold(0u32, |acc, n| acc ^ bit32(raw, n));
        raw |= (p6 & 1) << 23;
        [
            (raw & 0xFF) as u8,
            ((raw >> 8) & 0xFF) as u8,
            ((raw >> 16) & 0xFF) as u8,
        ]
    }

    #[test]
    fn decode24_round_trips_a_pattern() {
        let bits = [1, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 1];
        let bytes = encode24(bits);
        let (data, correction) = decode24(bytes);
        assert_eq!(correction, Correction::Clean);
        for (i, &b) in bits.iter().enumerate() {
            let got = (data >> i) & 1;
            assert_eq!(got, u32::from(b), "bit {i}");
        }
    }

    #[test]
    fn decode24_corrects_every_single_bit_flip() {
        let bits = [0, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 0, 1, 1, 0, 1];
        let bytes = encode24(bits);
        let raw = u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16);
        for flip in 0u32..24 {
            let Some(shifted) = raw.checked_shl(0).and_then(|_| 1u32.checked_shl(flip)) else {
                continue;
            };
            let flipped_raw = raw ^ shifted;
            let flipped = [
                (flipped_raw & 0xFF) as u8,
                ((flipped_raw >> 8) & 0xFF) as u8,
                ((flipped_raw >> 16) & 0xFF) as u8,
            ];
            let (data, correction) = decode24(flipped);
            assert_eq!(correction, Correction::Corrected, "flip bit {flip}");
            for (i, &b) in bits.iter().enumerate() {
                let got = (data >> i) & 1;
                assert_eq!(got, u32::from(b), "flip bit {flip}, data bit {i}");
            }
        }
    }

    #[test]
    fn decode24_detects_a_double_bit_error() {
        let bits = [1; 18];
        let bytes = encode24(bits);
        let raw = u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16);
        let flipped_raw = raw ^ 0b11;
        let flipped = [
            (flipped_raw & 0xFF) as u8,
            ((flipped_raw >> 8) & 0xFF) as u8,
            ((flipped_raw >> 16) & 0xFF) as u8,
        ];
        let (_, correction) = decode24(flipped);
        assert_eq!(correction, Correction::Uncorrectable);
    }

    #[test]
    fn triplet_fields_split_as_spec_figure_shows() {
        // Address=0b101010 (0x2A), Mode=0b10000 (0x10), Data=0b0101010 (0x2A).
        let value = 0x2A | (0x10 << 6) | (0x2A << 11);
        assert_eq!(triplet_address(value), 0x2A);
        assert_eq!(triplet_mode(value), 0x10);
        assert_eq!(triplet_data(value), 0x2A);
    }
}
