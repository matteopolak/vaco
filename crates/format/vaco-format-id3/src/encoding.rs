//! The four text encodings `ID3v2` frames declare, and reading a
//! null-terminated string in whichever one is in effect.
//!
//! `ID3v2.4.0` §4: encoding byte `$00` ISO-8859-1, `$01` UTF-16 with BOM,
//! `$02` UTF-16BE without BOM, `$03` UTF-8. `ID3v2.3.0` §4 and `ID3v2.2.0` §3.3
//! define only `$00` and `$01` — `$02`/`$03` are a v2.4 addition — but this
//! crate accepts all four regardless of the tag's own version, matching
//! every lenient real-world reader: a byte value the spec does not define
//! for that version is not ambiguous, it just never occurs in a compliant
//! file of that version.
//!
//! Decoding never fails and never panics: ISO-8859-1 maps every byte to a
//! valid code point by definition, and invalid UTF-16 or UTF-8 sequences are
//! replaced with U+FFFD rather than rejected, the same lossy-recovery
//! posture `vaco-format-isom`'s text fields use for untrusted metadata.

/// One of the four `ID3v2` text encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// `$00`: ISO-8859-1, one byte per code point, `$00`-terminated.
    Latin1,
    /// `$01`: UTF-16, a byte-order mark first, `$00 $00`-terminated.
    Utf16Bom,
    /// `$02`: UTF-16BE, no byte-order mark, `$00 $00`-terminated.
    Utf16Be,
    /// `$03`: UTF-8, `$00`-terminated.
    Utf8,
}

impl Encoding {
    /// Decode an `ID3v2` encoding byte. `None` for any value other than
    /// `0..=3` — there is no fifth encoding to fall back to.
    #[must_use]
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Latin1),
            1 => Some(Self::Utf16Bom),
            2 => Some(Self::Utf16Be),
            3 => Some(Self::Utf8),
            _ => None,
        }
    }

    /// Bytes in this encoding's null terminator: 1 for the byte-oriented
    /// encodings, 2 for the UTF-16 ones.
    #[must_use]
    pub const fn terminator_width(self) -> usize {
        match self {
            Self::Latin1 | Self::Utf8 => 1,
            Self::Utf16Bom | Self::Utf16Be => 2,
        }
    }
}

/// Decode `bytes` (no terminator expected — the caller has already split it
/// off) as `encoding`. Never panics; invalid sequences are replaced with
/// U+FFFD.
#[must_use]
pub fn decode(encoding: Encoding, bytes: &[u8]) -> String {
    match encoding {
        Encoding::Latin1 => bytes.iter().map(|&b| char::from(b)).collect(),
        Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Encoding::Utf16Be => decode_utf16_from(bytes, u16::from_be_bytes),
        Encoding::Utf16Bom => match bytes.first_chunk::<2>() {
            Some(&[0xFF, 0xFE]) => {
                decode_utf16_from(bytes.get(2..).unwrap_or(&[]), u16::from_le_bytes)
            }
            Some(&[0xFE, 0xFF]) => {
                decode_utf16_from(bytes.get(2..).unwrap_or(&[]), u16::from_be_bytes)
            }
            // No BOM at all is malformed per spec, but every real reader
            // falls back to a definite endianness rather than rejecting the
            // frame outright; little-endian is the overwhelmingly common
            // case in the wild (Windows-authored tags).
            _ => decode_utf16_from(bytes, u16::from_le_bytes),
        },
    }
}

fn decode_utf16_from(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units = bytes.chunks_exact(2).map(|c| {
        // `chunks_exact(2)` guarantees exactly two bytes per chunk.
        let pair = <[u8; 2]>::try_from(c).unwrap_or([0, 0]);
        unit(pair)
    });
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Split `bytes` at the first occurrence of `encoding`'s null terminator,
/// decoding everything before it and returning what follows.
///
/// If no terminator is found, the whole slice is treated as the string (no
/// terminator) and the remainder is empty — the lenient behaviour a
/// truncated frame needs rather than an error, since a missing terminator on
/// the *last* field in a frame is valid by the grammar (see
/// [`read_to_end`]).
#[must_use]
pub fn read_terminated(encoding: Encoding, bytes: &[u8]) -> (String, &[u8]) {
    let width = encoding.terminator_width();
    let end = if width == 1 {
        bytes.iter().position(|&b| b == 0)
    } else {
        // A UTF-16 terminator must land on a two-byte boundary from the
        // start of this field; scanning by twos also means an odd stray
        // byte at the very end can never be mistaken for half of one.
        let mut i: usize = 0;
        let mut found = None;
        while let Some(pair) = bytes.get(i..i.saturating_add(2)) {
            if pair == [0, 0] {
                found = Some(i);
                break;
            }
            i = i.saturating_add(2);
        }
        found
    };
    match end {
        Some(i) => {
            let text = bytes.get(..i).unwrap_or(&[]);
            let rest = bytes.get(i.saturating_add(width)..).unwrap_or(&[]);
            (decode(encoding, text), rest)
        }
        None => (decode(encoding, bytes), &[]),
    }
}

/// Decode the rest of a frame with no terminator to look for — the last
/// field in most frames (a `T***` value, `COMM`'s text, `APIC`'s picture
/// data is *not* text and is never passed here).
#[must_use]
pub fn read_to_end(encoding: Encoding, bytes: &[u8]) -> String {
    decode(encoding, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_maps_every_byte_to_its_code_point() {
        assert_eq!(decode(Encoding::Latin1, &[0x41, 0xE9]), "A\u{e9}");
    }

    #[test]
    fn utf8_decodes_normally() {
        assert_eq!(decode(Encoding::Utf8, "héllo".as_bytes()), "héllo");
    }

    #[test]
    fn invalid_utf8_is_replaced_not_rejected() {
        let s = decode(Encoding::Utf8, &[0xFF, 0xFE, b'a']);
        assert!(s.contains('a'));
        assert!(s.contains('\u{fffd}'));
    }

    #[test]
    fn utf16_bom_le_round_trips() {
        // "Hi" as UTF-16LE with a little-endian BOM.
        let bytes = [0xFF, 0xFE, b'H', 0x00, b'i', 0x00];
        assert_eq!(decode(Encoding::Utf16Bom, &bytes), "Hi");
    }

    #[test]
    fn utf16_bom_be_round_trips() {
        let bytes = [0xFE, 0xFF, 0x00, b'H', 0x00, b'i'];
        assert_eq!(decode(Encoding::Utf16Bom, &bytes), "Hi");
    }

    #[test]
    fn utf16be_without_bom_round_trips() {
        let bytes = [0x00, b'H', 0x00, b'i'];
        assert_eq!(decode(Encoding::Utf16Be, &bytes), "Hi");
    }

    #[test]
    fn read_terminated_splits_at_the_null_and_returns_the_rest() {
        let (s, rest) = read_terminated(Encoding::Latin1, b"hello\x00world");
        assert_eq!(s, "hello");
        assert_eq!(rest, b"world");
    }

    #[test]
    fn read_terminated_utf16_splits_on_a_two_byte_boundary() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend_from_slice(&[b'H', 0x00, b'i', 0x00]); // "Hi"
        bytes.extend_from_slice(&[0x00, 0x00]); // terminator
        bytes.extend_from_slice(b"REST");
        let (s, rest) = read_terminated(Encoding::Utf16Bom, &bytes);
        assert_eq!(s, "Hi");
        assert_eq!(rest, b"REST");
    }

    #[test]
    fn read_terminated_with_no_terminator_consumes_everything() {
        let (s, rest) = read_terminated(Encoding::Latin1, b"no terminator here");
        assert_eq!(s, "no terminator here");
        assert!(rest.is_empty());
    }

    #[test]
    fn read_terminated_never_panics_on_a_lone_trailing_byte() {
        // An odd trailing byte for a UTF-16 field must not panic looking for
        // a two-byte terminator that cannot exist.
        let (_, rest) = read_terminated(Encoding::Utf16Be, &[0x00, b'H', 0xAB]);
        assert!(rest.is_empty());
    }

    #[test]
    fn an_unrecognised_encoding_byte_is_none() {
        assert_eq!(Encoding::from_byte(4), None);
        assert_eq!(Encoding::from_byte(255), None);
    }
}
