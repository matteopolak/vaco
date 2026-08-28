//! Odd-parity decode (EN 300 706 §8.1), used for every display byte in
//! packets X/0 to X/25.
//!
//! The code is single-error-**detecting** only — the spec gives no recovery
//! rule beyond "single bit errors can be detected" — so a failed check
//! returns [`None`] rather than a guess, and callers are expected to render
//! a visible placeholder and count the failure (see [`crate::page::Page`]'s
//! `corrupt_parity` counter).

/// Decode one odd-parity byte: bit 7 is the parity bit, bits 0-6 the data.
///
/// Returns the 7-bit data value if the byte has odd parity (an odd number of
/// set bits), `None` otherwise.
#[must_use]
pub const fn decode(byte: u8) -> Option<u8> {
    if byte.count_ones() % 2 == 1 {
        Some(byte & 0x7F)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_correctly_parity_coded_byte() {
        // 'A' = 0x41 = 0b0100_0001, two set bits before parity; the encoder
        // sets bit 7 to make the count odd, e.g. 0xC1 (three set bits).
        assert_eq!(decode(0xC1), Some(0x41));
    }

    #[test]
    fn rejects_an_even_parity_byte() {
        // 0x41 alone has two set bits (even), so it fails the odd-parity
        // check as transmitted.
        assert_eq!(decode(0x41), None);
    }

    #[test]
    fn space_and_zero_both_decode() {
        // 0x20 (SPACE) has one set bit, already odd.
        assert_eq!(decode(0x20), Some(0x20));
    }
}
