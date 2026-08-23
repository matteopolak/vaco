//! The 80-bit IEEE 754 extended-precision float AIFF's `COMM` chunk stores
//! its sample rate in.
//!
//! This is the standard x87/IEEE-754-extended layout — a numerical standard,
//! not anyone's format-specific expression (D7): one sign bit, a 15-bit
//! biased exponent (bias 16383), and a 64-bit mantissa with an *explicit*
//! integer bit (unlike the implicit-leading-1 convention `f32`/`f64` use).
//!
//! ```text
//! byte  0-1   sign(1) + exponent(15), big-endian
//! byte  2-9   mantissa, 64 bits, big-endian, explicit integer bit at bit 63
//! ```

use vaco_bitstream::ByteReader;

/// Decode a 10-byte extended-precision value to the nearest `f64`.
///
/// Never panics on a short buffer: [`ByteReader`] zero-fills past the end,
/// which decodes to `0.0` — a wrong-but-safe answer for a truncated chunk,
/// consistent with the "clamp, never trust" policy the rest of this
/// workspace's container parsers use for attacker-controlled headers.
#[must_use]
pub fn to_f64(data: &[u8]) -> f64 {
    let mut r = ByteReader::new(data);
    let exp_field = r.be16();
    let mantissa = r.be64();
    let negative = exp_field & 0x8000 != 0;
    let exponent = i32::from(exp_field & 0x7fff);
    if exponent == 0 && mantissa == 0 {
        return 0.0;
    }
    // value = mantissa * 2^(exponent - 16383 - 63)
    let value = (mantissa as f64) * 2f64.powi(exponent - 16383 - 63);
    if negative { -value } else { value }
}

/// Encode `value` (assumed non-negative and finite — every sample rate this
/// crate writes is) as a 10-byte extended-precision value.
#[must_use]
pub fn from_f64(value: f64) -> [u8; 10] {
    if value <= 0.0 || !value.is_finite() {
        return [0; 10];
    }
    let exponent = value.log2().floor() as i32;
    let mantissa = (value / 2f64.powi(exponent - 63)).round();
    // A rounding step can push the mantissa to exactly 2^64; renormalise by
    // one exponent step rather than let the cast below wrap.
    let (exponent, mantissa) = if mantissa >= 2f64.powi(64) {
        (exponent + 1, mantissa / 2.0)
    } else {
        (exponent, mantissa)
    };
    let mantissa_bits = mantissa as u64;
    let exp_field = (u16::try_from(exponent + 16383).unwrap_or(0)) & 0x7fff;
    let eb = exp_field.to_be_bytes();
    let mb = mantissa_bits.to_be_bytes();
    let mut out = [0u8; 10];
    if let Some(head) = out.get_mut(0..2) {
        head.copy_from_slice(&eb);
    }
    if let Some(tail) = out.get_mut(2..10) {
        tail.copy_from_slice(&mb);
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "test code; these are exact-zero and exact-encoding checks, not tolerance comparisons"
)]
mod tests {
    use super::*;

    #[test]
    fn common_sample_rates_round_trip_exactly() {
        for &rate in &[
            8_000.0, 11_025.0, 22_050.0, 44_100.0, 48_000.0, 96_000.0, 192_000.0,
        ] {
            let bytes = from_f64(rate);
            let back = to_f64(&bytes);
            assert!((back - rate).abs() < 1e-6, "{rate} round-tripped to {back}");
        }
    }

    #[test]
    fn zero_round_trips_to_zero() {
        assert_eq!(to_f64(&from_f64(0.0)), 0.0);
    }

    #[test]
    fn a_short_buffer_decodes_to_zero_not_panicking() {
        assert_eq!(to_f64(&[]), 0.0);
        assert_eq!(to_f64(&[0x40]), 0.0);
    }

    #[test]
    fn a_known_encoding_matches_the_specification_bit_layout() {
        // 44100.0 = 1.34765625 * 2^15; mantissa (explicit-bit) = 0xAC44000000000000,
        // exponent field = 15 + 16383 = 16398 = 0x400E — a value this crate's own
        // riff sibling's AIFF-adjacent WAV sample rate field cites (44100 = 0xAC44)
        // as the visible mantissa top bytes, and independently reproducible by hand
        // from the IEEE 754 extended layout.
        let bytes = from_f64(44_100.0);
        assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 0x400E);
        assert_eq!(bytes[2], 0xAC);
        assert_eq!(bytes[3], 0x44);
    }
}
