//! BER length coding (SMPTE ST 336 §7, restricted to *definite* lengths).
//!
//! KLV forbids the classic BER "indefinite length" (X.690 §8.1.3.6): every
//! value's length must be stated up front so a reader never has to scan for
//! an end-of-contents marker. That restriction is exactly what this module
//! enforces, and it is also the module's whole reason to exist separately
//! from `klv`: a BER length is the one field in this entire format that is
//! attacker-controlled *before* anything else about the input is known, so
//! its decoder gets checked in isolation, by proptest, with no I/O involved.
//!
//! # Shape
//!
//! - **Short form**: one byte, `0x00..=0x7F`, the value itself.
//! - **Long form**: one byte `0x80 | n` (`n` = 1..=8 following bytes), then
//!   `n` bytes, big-endian, holding the value.
//! - `n == 0` (a first byte of exactly `0x80`) is the indefinite-length
//!   marker BER allows elsewhere and KLV does not: rejected.
//! - `n > 8` cannot hold a value distinguishable from one that fits in a
//!   `u64` and is exactly the shape of a denial-of-service attempt — the
//!   canonical case is a first byte of `0xFF` (`n = 0x7F` = 127), which
//!   claims 127 more length bytes are coming. Rejected without reading past
//!   `n`'s own byte, so the attempt costs the parser one byte to refuse.

use vaco_core::{Error, Result};

/// Longest a BER length prefix can be before this decoder refuses it: one
/// marker byte plus eight value bytes, enough for any `u64`.
pub const MAX_ENCODED_LEN: usize = 9;

/// A definite-form BER length, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedLen {
    /// The value the length prefix encodes.
    pub value: u64,
    /// Bytes the prefix itself occupied (1..=9).
    pub consumed: usize,
}

/// Decode a BER length from the start of `bytes`.
///
/// Reads at most 9 bytes and never more than `bytes.len()`.
///
/// # Errors
///
/// [`Error::UnexpectedEof`] if `bytes` is empty, or shorter than the long
/// form it declares. [`Error::InvalidData`] for the indefinite-length marker
/// (`0x80`) or for a declared width over 8 bytes (`n > 8`, e.g. `0xFF`) —
/// see the module docs for why the latter is refused rather than merely slow.
pub fn decode(bytes: &[u8]) -> Result<DecodedLen> {
    let &b0 = bytes.first().ok_or(Error::UnexpectedEof)?;
    if b0 & 0x80 == 0 {
        return Ok(DecodedLen {
            value: u64::from(b0),
            consumed: 1,
        });
    }
    let n = usize::from(b0 & 0x7f);
    if n == 0 {
        return Err(Error::InvalidData(
            "indefinite-length BER form is not permitted in KLV",
        ));
    }
    if n > 8 {
        return Err(Error::InvalidData(
            "BER length prefix wider than 64 bits (declared width over 8 bytes)",
        ));
    }
    let width_bytes = bytes.get(1..1 + n).ok_or(Error::UnexpectedEof)?;
    let mut buf = [0u8; 8];
    // Right-align the n big-endian bytes into an 8-byte buffer.
    if let Some(dst) = buf.get_mut(8 - n..) {
        dst.copy_from_slice(width_bytes);
    }
    Ok(DecodedLen {
        value: u64::from_be_bytes(buf),
        consumed: 1 + n,
    })
}

/// A BER length prefix, encoded into a fixed, non-allocating buffer.
#[derive(Debug, Clone, Copy)]
pub struct EncodedLen {
    buf: [u8; MAX_ENCODED_LEN],
    len: u8,
}

impl EncodedLen {
    /// The encoded bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // `len` is always <= MAX_ENCODED_LEN, set only by `encode` below.
        self.buf.get(..usize::from(self.len)).unwrap_or(&[])
    }
}

/// Encode `value` in the shortest valid BER definite form.
///
/// Real MXF writers often pad to a fixed width instead (observed: `ffmpeg`
/// writes every partition pack's length as 4 bytes, `0x83` + 3, even when
/// the value would fit in one byte) so that a value can be rewritten in
/// place later without shifting the file. [`decode`] accepts any valid
/// width; this function only has to produce *a* correct one, and the
/// shortest is simplest to get right and cheapest to test.
#[must_use]
pub fn encode(value: u64) -> EncodedLen {
    if value < 0x80 {
        let mut buf = [0u8; MAX_ENCODED_LEN];
        buf[0] = value as u8;
        return EncodedLen { buf, len: 1 };
    }
    let be = value.to_be_bytes();
    // Minimal width: drop leading zero bytes, but keep at least 1.
    let first_nonzero = be.iter().position(|&b| b != 0).unwrap_or(7);
    let width = 8 - first_nonzero;
    let mut buf = [0u8; MAX_ENCODED_LEN];
    buf[0] = 0x80 | (width as u8);
    if let (Some(dst), Some(src)) = (buf.get_mut(1..1 + width), be.get(first_nonzero..)) {
        dst.copy_from_slice(src);
    }
    EncodedLen {
        buf,
        len: 1 + width as u8,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn short_form_is_the_byte_itself() {
        let d = decode(&[0x39]).unwrap();
        assert_eq!(d.value, 57);
        assert_eq!(d.consumed, 1);
    }

    #[test]
    fn long_form_matches_a_real_partition_pack_length() {
        // Measured: `out.mxf`'s header partition pack encodes its 104-byte
        // value as `0x83 0x00 0x00 0x68` — a 4-byte form for a value that
        // would fit in one byte, because the writer always pads to 4.
        let d = decode(&[0x83, 0x00, 0x00, 0x68]).unwrap();
        assert_eq!(d.value, 104);
        assert_eq!(d.consumed, 4);
    }

    #[test]
    fn long_form_eight_bytes_round_trips_u64_max() {
        let mut bytes = vec![0x88u8];
        bytes.extend_from_slice(&u64::MAX.to_be_bytes());
        let d = decode(&bytes).unwrap();
        assert_eq!(d.value, u64::MAX);
        assert_eq!(d.consumed, 9);
    }

    #[test]
    fn indefinite_length_marker_is_rejected() {
        assert!(matches!(decode(&[0x80]), Err(Error::InvalidData(_))));
    }

    #[test]
    fn all_ones_first_byte_is_rejected_without_reading_127_bytes() {
        // 0xFF -> n = 0x7F = 127. This is the edge the brief calls out by
        // name: refused on the marker byte alone, not by trying to read (and
        // possibly failing to find) 127 more bytes.
        assert!(matches!(decode(&[0xFF]), Err(Error::InvalidData(_))));
        // Confirmed even when the 127 bytes genuinely are NOT present: no
        // panic, no out-of-bounds read, just the same error.
        assert!(matches!(
            decode(&[0xFF, 0x01, 0x02]),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn a_2_to_the_56_length_is_a_real_finding_shape_not_noise() {
        // 2^56 is `0x01` followed by 7 zero bytes: width 8, the widest form
        // this decoder accepts. This must decode cleanly to a *value* —
        // bounding what a caller does with that value (never allocate it
        // directly) is `vaco-limits`' job, not this decoder's; see
        // `klv::read_value`.
        let d = decode(&[0x88, 0x01, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(d.value, 1u64 << 56);
        assert_eq!(d.consumed, 9);
    }

    #[test]
    fn declared_width_past_the_buffer_is_eof_not_a_panic() {
        assert!(matches!(
            decode(&[0x84, 0x01, 0x02]),
            Err(Error::UnexpectedEof)
        ));
    }

    #[test]
    fn empty_input_is_eof() {
        assert!(matches!(decode(&[]), Err(Error::UnexpectedEof)));
    }

    #[test]
    fn encode_short_values_uses_one_byte() {
        assert_eq!(encode(0).as_slice(), &[0x00]);
        assert_eq!(encode(127).as_slice(), &[0x7f]);
    }

    #[test]
    fn encode_uses_minimal_long_form() {
        assert_eq!(encode(128).as_slice(), &[0x81, 0x80]);
        assert_eq!(encode(256).as_slice(), &[0x82, 0x01, 0x00]);
    }

    proptest! {
        /// Every value round-trips through encode then decode.
        #[test]
        fn encode_decode_round_trip(value: u64) {
            let enc = encode(value);
            let dec = decode(enc.as_slice()).unwrap();
            prop_assert_eq!(dec.value, value);
            prop_assert_eq!(dec.consumed, enc.as_slice().len());
        }

        /// Decoding never panics on arbitrary bytes, of any length, and never
        /// reads past what it reports consuming.
        #[test]
        fn decode_never_panics_and_never_overreads(bytes: Vec<u8>) {
            if let Ok(d) = decode(&bytes) {
                prop_assert!(d.consumed <= bytes.len());
            }
        }

        /// A non-minimal (zero-padded) long-form encoding of a value that
        /// fits in fewer bytes still decodes to that value — real writers
        /// pad deliberately (see the partition-pack test above), so the
        /// decoder must accept it.
        #[test]
        fn padded_long_form_still_decodes(value in 0u64..0x7fff_ffff, extra_padding in 0u8..4) {
            let width = 4 + usize::from(extra_padding);
            let vbytes = value.to_be_bytes();
            let mut padded = vec![0u8; width];
            if let (Some(dst), Some(src)) = (padded.get_mut(width - 4..), vbytes.get(4..)) {
                dst.copy_from_slice(src);
            }
            let mut full = vec![0x80 | (width as u8)];
            full.extend_from_slice(&padded);
            let d = decode(&full).unwrap();
            prop_assert_eq!(d.value, value);
            prop_assert_eq!(d.consumed, full.len());
        }
    }
}
