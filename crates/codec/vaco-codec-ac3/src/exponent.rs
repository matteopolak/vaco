//! Exponent strategies and grouped differential exponent decode.
//! ATSC A/52:2012 §7.1.3, transcribed from the pseudocode directly.

use vaco_bitstream::BitReader;

/// Per-block exponent strategy. `Reuse` means "same as the previous block
/// carrying this channel" — the caller supplies the previous exponents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpStrategy {
    Reuse,
    D15,
    D25,
    D45,
}

impl ExpStrategy {
    #[must_use]
    pub const fn from_bits(code: u32) -> Self {
        match code {
            1 => Self::D15,
            2 => Self::D25,
            3 => Self::D45,
            _ => Self::Reuse,
        }
    }

    /// `grpsize`: coefficients each decoded absolute exponent (`aexp`) is
    /// copied to. §7.1.3.
    const fn grpsize(self) -> usize {
        match self {
            Self::D25 => 2,
            Self::D45 => 4,
            _ => 1,
        }
    }
}

/// Decode one channel's exponents for `1 + ncodes*3*grpsize` coefficients:
/// `exp[0]` is the raw 4-bit `absexp` (already shifted, e.g. `cplabsexp<<1`,
/// if the caller's channel needs that); each of the `ncodes` 7-bit `gexp`
/// codes then unpacks to 3 differential values which accumulate into 3
/// absolute exponents, each copied to `grpsize` coefficients.
///
/// This is deliberately not derived from a bin count internally — the three
/// channel kinds (full-bandwidth, coupling, LFE) compute `ncodes` by three
/// different spec formulas from their own bin range, so the caller passes it
/// in explicitly rather than this function guessing it back out of `n`.
#[must_use]
pub fn decode(r: &mut BitReader<'_>, absexp: u8, ncodes: usize, strategy: ExpStrategy) -> Vec<u8> {
    let grpsize = strategy.grpsize();
    let mut out = vec![absexp];
    let mut prev = absexp;
    for _ in 0..ncodes {
        let code = r.get(7);
        for m in [decode_digit(code, 25), decode_digit(code % 25, 5), code % 25 % 5] {
            let dexp = i32::from(u8::try_from(m).unwrap_or(0)) - 2;
            let next = (i64::from(prev) + i64::from(dexp)).clamp(0, 24);
            prev = u8::try_from(next).unwrap_or(prev);
            for _ in 0..grpsize {
                out.push(prev);
            }
        }
    }
    out
}

#[allow(
    clippy::integer_division,
    reason = "base-25/5 digit extraction from a packed 7-bit code, not a precision loss"
)]
const fn decode_digit(value: u32, base: u32) -> u32 {
    value / base
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_exponent_run_decodes_to_a_constant_array() {
        // dexp code for delta {0,0,0} is (0+2)*25 + (0+2)*5 + (0+2) = 62.
        let code = 62u32;
        let mut bits = Vec::new();
        for _ in 0..10 {
            for b in (0..7).rev() {
                bits.push((code >> b) & 1 != 0);
            }
        }
        let mut buf = vec![0u8; bits.len().div_ceil(8)];
        for (i, &b) in bits.iter().enumerate() {
            if b {
                buf[i / 8] |= 0x80 >> (i % 8);
            }
        }
        let mut r = BitReader::new(&buf);
        let out = decode(&mut r, 0, 8, ExpStrategy::D15);
        assert!(out.iter().all(|&e| e == 0));
        assert_eq!(out.len(), 1 + 8 * 3);
    }

    #[test]
    fn grpsize_expands_each_decoded_value() {
        let mut r = BitReader::new(&[0u8; 8]);
        let out = decode(&mut r, 5, 2, ExpStrategy::D45);
        assert_eq!(out.len(), 1 + 2 * 3 * 4);
    }

    #[test]
    fn never_panics_on_a_truncated_buffer() {
        let mut r = BitReader::new(&[0u8; 1]);
        let _ = decode(&mut r, 0, 20, ExpStrategy::D45);
    }
}
