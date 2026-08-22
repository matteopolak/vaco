//! Synchsafe integers: 7 usable bits per byte, so an MPEG frame sync
//! (`0xFF` followed by a byte with its top three bits set) can never appear
//! inside a size field by construction.
//!
//! `ID3v2.3.0` §3.1 and `ID3v2.4.0` §3.1 both make the *header* size synchsafe.
//! Where the two versions genuinely differ — and getting this wrong silently
//! misparses every v2.3 tag — is the **frame** header size: `ID3v2.4.0` §4
//! makes it synchsafe too, but `ID3v2.3.0` §3.3 does not; a v2.3 frame size is
//! a plain 32-bit big-endian integer.
//!
//! Probed directly (`ffmpeg -metadata comment=<200 x 'A'> -id3v2_version 3|4
//! -c:a mp3 out.mp3`, then read the `COMM`/`TXXX` frame header bytes): a
//! content length of 210 bytes is written as `00 00 00 D2` under `-id3v2_version
//! 3` and as `00 00 01 52` under `-id3v2_version 4`. The first form has a byte
//! (`0xD2`) with its top bit set, which is not a legal synchsafe byte at
//! all — proof the v2.3 writer treated the field as plain binary, not proof
//! by absence. The second form is `210`'s synchsafe encoding
//! (`(1 << 7) | 0x52 = 210`), and is *not* the plain-binary encoding of 210
//! (which would be `00 00 00 D2`, identical to the v2.3 case) — proof the
//! v2.4 writer chose the synchsafe form specifically.

/// Decode four synchsafe bytes (as they appear in the tag/footer header, and
/// in a v2.4 frame header) into their 28-bit value.
///
/// Every byte's top bit is ignored, matching real encoders: a byte with the
/// top bit set is not valid synchsafe, but the informal standard does not
/// mandate rejecting a tag over it, and ffmpeg's own reader does not.
#[must_use]
pub const fn decode(bytes: [u8; 4]) -> u32 {
    let [a, b, c, d] = bytes;
    (((a & 0x7f) as u32) << 21)
        | (((b & 0x7f) as u32) << 14)
        | (((c & 0x7f) as u32) << 7)
        | ((d & 0x7f) as u32)
}

/// Whether every byte is a legal synchsafe byte (top bit clear).
///
/// Used to distinguish a v2.3 frame size (plain binary, so any byte pattern
/// is "valid" as a number) from a v2.4 one where a byte with the top bit set
/// indicates the value came from something other than a synchsafe writer —
/// not load-bearing for parsing (this crate trusts the declared version
/// unconditionally, matching the reference), but useful for a caller
/// diagnosing a mislabelled tag.
#[must_use]
pub const fn is_valid(bytes: [u8; 4]) -> bool {
    bytes[0] & 0x80 == 0 && bytes[1] & 0x80 == 0 && bytes[2] & 0x80 == 0 && bytes[3] & 0x80 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_probed_v24_frame_size() {
        // `00 00 01 52` -> 210, matching the 210-byte TXXX content ffmpeg
        // wrote under -id3v2_version 4.
        assert_eq!(decode([0x00, 0x00, 0x01, 0x52]), 210);
    }

    #[test]
    fn the_v23_encoding_of_the_same_value_is_not_valid_synchsafe() {
        // `00 00 00 D2` is what ffmpeg wrote for the same 210-byte content
        // under -id3v2_version 3: plain binary, and 0xD2's top bit is set.
        assert!(!is_valid([0x00, 0x00, 0x00, 0xD2]));
    }

    #[test]
    fn zero_and_max_round_trip() {
        assert_eq!(decode([0, 0, 0, 0]), 0);
        assert_eq!(decode([0x7f, 0x7f, 0x7f, 0x7f]), 0x0FFF_FFFF);
    }

    #[test]
    fn top_bits_are_ignored_not_rejected() {
        assert_eq!(decode([0xff, 0xff, 0xff, 0xff]), 0x0FFF_FFFF);
    }
}
