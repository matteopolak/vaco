//! BER length encoding (SMPTE ST 336 §7), the write-side counterpart of
//! `vaco-demux-mxf::ber`'s decoder. Definite forms only, same as that crate
//! reads.

/// Longest a BER length prefix this crate writes can be: one marker byte
/// plus eight value bytes (enough for any `u64`, matching the read side's
/// own `MAX_ENCODED_LEN`).
const MAX_ENCODED_LEN: usize = 9;

/// A definite-form BER length, encoded into a fixed, non-allocating buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EncodedLen {
    buf: [u8; MAX_ENCODED_LEN],
    len: u8,
}

impl EncodedLen {
    #[must_use]
    pub(crate) fn as_slice(&self) -> &[u8] {
        self.buf.get(..usize::from(self.len)).unwrap_or(&[])
    }
}

/// Encode `value` as a fixed-width, 4-value-byte long form (`0x83` + 3
/// bytes) when it fits (up to 16 MiB minus one), and an 8-value-byte long
/// form (`0x88` + 8 bytes) otherwise.
///
/// A real `ffmpeg` partition/primer pack pads its length to 4 bytes even
/// when the value would fit in fewer (`vaco-demux-mxf::ber`'s doc comment
/// notes this, measured); every metadata KLV this crate writes follows the
/// same convention. Essence elements can legitimately exceed 16 MiB (a
/// large uncompressed frame), so those widen to the 8-byte form rather than
/// silently truncating — [`vaco-demux-mxf::ber::decode`] accepts either
/// width.
#[must_use]
pub(crate) fn encode(value: u64) -> EncodedLen {
    if value < 0x0100_0000 {
        let be = (value as u32).to_be_bytes();
        let mut buf = [0u8; MAX_ENCODED_LEN];
        buf[0] = 0x83;
        buf[1] = be[1];
        buf[2] = be[2];
        buf[3] = be[3];
        return EncodedLen { buf, len: 4 };
    }
    let be = value.to_be_bytes();
    let mut buf = [0u8; MAX_ENCODED_LEN];
    buf[0] = 0x88; // 8 more bytes follow.
    buf[1..9].copy_from_slice(&be);
    EncodedLen { buf, len: 9 }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_small_value_in_the_fixed_four_byte_long_form() {
        assert_eq!(encode(104).as_slice(), &[0x83, 0x00, 0x00, 0x68]);
    }

    #[test]
    fn widens_to_eight_bytes_past_sixteen_mebibytes() {
        let enc = encode(0x0100_0000);
        assert_eq!(enc.as_slice()[0], 0x88);
        assert_eq!(enc.as_slice().len(), 9);
    }

    #[test]
    fn round_trips_through_a_reimplementation_of_the_decode_side_rule() {
        for v in [0u64, 1, 127, 128, 255, 65535, 1 << 20, 1 << 25] {
            let enc = encode(v);
            assert_eq!(decode_shim(enc.as_slice()), v);
        }
    }

    // A from-scratch reimplementation of BER's own decode rule (not a
    // dependency on the sibling crate's private `ber` module from a unit
    // test), just enough to prove the encoder's bytes decode back to the
    // same value.
    fn decode_shim(bytes: &[u8]) -> u64 {
        let b0 = bytes[0];
        assert!(b0 & 0x80 != 0, "this encoder always uses the long form");
        let n = usize::from(b0 & 0x7f);
        let mut buf = [0u8; 8];
        buf[8 - n..].copy_from_slice(&bytes[1..=n]);
        u64::from_be_bytes(buf)
    }
}
