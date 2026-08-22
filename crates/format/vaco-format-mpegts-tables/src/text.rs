//! DVB text decoding (ETSI EN 300 468 Annex A).
//!
//! Every string in DVB SI — service names, provider names, event titles — is a
//! byte string whose *first byte* may select a character table. The default,
//! when the first byte is a printable character, is ISO/IEC 6937, which is not
//! Latin-1 and differs from it in about sixty positions. Treating it as Latin-1
//! is the usual mistake and gives visibly wrong text for any accented name.
//!
//! # Coverage, stated plainly
//!
//! | Selector | Table | Status |
//! |---|---|---|
//! | (none) | ISO/IEC 6937 with the DVB Euro at `0xA4` | **implemented**, including combining diacritics |
//! | `0x10 0x00 0x01` | ISO 8859-1 | implemented |
//! | `0x10 0x00 0x09` / `0x05` | ISO 8859-9 | implemented |
//! | `0x10 0x00 0x0F` / `0x0B` | ISO 8859-15 | implemented |
//! | `0x11` | ISO 10646 UTF-16BE | implemented |
//! | `0x15` | UTF-8 | implemented |
//! | `0x01`-`0x0B`, other `0x10` | ISO 8859-5…-14 | **not decoded**: bytes ≥ `0xA0` become `U+FFFD` |
//! | `0x12`-`0x14`, `0x1F` | KSX1001, GB2312, Big5, private | **not decoded**, same rule |
//!
//! An undecoded table produces replacement characters rather than plausible
//! rubbish, deliberately: a name that is visibly broken gets reported, and a
//! name that is quietly wrong does not. [`decode_with_charset`] returns which
//! table was selected so a caller can say so.
//!
//! Combining diacritics (`0xC1`-`0xCF` in ISO 6937) are emitted as Unicode
//! combining marks after their base character rather than composed into
//! precomposed code points, because composition needs a normalisation table we
//! do not carry. The result is canonically equivalent but not byte-identical to
//! an NFC renderer's.

/// Which character table a string selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    /// The default when no selector byte is present.
    Iso6937,
    /// ISO 8859-`n`.
    Iso8859(u8),
    /// ISO 10646 in UTF-16BE.
    Utf16Be,
    Utf8,
    /// A table we do not decode. The value is the selector byte.
    Unsupported(u8),
}

impl Charset {
    /// A name for diagnostics.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Iso6937 => "iso6937",
            Self::Iso8859(_) => "iso8859",
            Self::Utf16Be => "utf16be",
            Self::Utf8 => "utf8",
            Self::Unsupported(_) => "unsupported",
        }
    }
}

/// ISO/IEC 6937 code points for `0xA0`-`0xFF`.
///
/// `0` marks a position that is either unassigned or a combining diacritic;
/// both are handled by the decoder rather than by this table. `0xA4` carries
/// the Euro sign per EN 300 468 Annex A, which is DVB's one amendment to the
/// underlying standard.
const ISO6937_HIGH: [u16; 96] = [
    0x00A0, 0x00A1, 0x00A2, 0x00A3, 0x20AC, 0x00A5, 0x0000, 0x00A7, // A0
    0x00A4, 0x2018, 0x201C, 0x00AB, 0x2190, 0x2191, 0x2192, 0x2193, // A8
    0x00B0, 0x00B1, 0x00B2, 0x00B3, 0x00D7, 0x00B5, 0x00B6, 0x00B7, // B0
    0x00F7, 0x2019, 0x201D, 0x00BB, 0x00BC, 0x00BD, 0x00BE, 0x00BF, // B8
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, // C0 diacritics
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, // C8 diacritics
    0x2015, 0x00B9, 0x00AE, 0x00A9, 0x2122, 0x266A, 0x00AC, 0x00A6, // D0
    0x0000, 0x0000, 0x0000, 0x0000, 0x215B, 0x215C, 0x215D, 0x215E, // D8
    0x2126, 0x00C6, 0x0110, 0x00AA, 0x0126, 0x0000, 0x0132, 0x013F, // E0
    0x0141, 0x00D8, 0x0152, 0x00BA, 0x00DE, 0x0166, 0x014A, 0x0149, // E8
    0x0138, 0x00E6, 0x0111, 0x00F0, 0x0127, 0x0131, 0x0133, 0x0140, // F0
    0x0142, 0x00F8, 0x0153, 0x00DF, 0x00FE, 0x0167, 0x014B, 0x00AD, // F8
];

/// The combining mark each ISO 6937 diacritic prefix introduces.
///
/// Index is `byte - 0xC0`; `0` means the position is unassigned.
const ISO6937_DIACRITIC: [u16; 16] = [
    0x0000, // C0 unassigned
    0x0300, // C1 grave
    0x0301, // C2 acute
    0x0302, // C3 circumflex
    0x0303, // C4 tilde
    0x0304, // C5 macron
    0x0306, // C6 breve
    0x0307, // C7 dot above
    0x0308, // C8 diaeresis
    0x0000, // C9 unassigned
    0x030A, // CA ring above
    0x0327, // CB cedilla
    0x0332, // CC underline
    0x030B, // CD double acute
    0x0328, // CE ogonek
    0x030C, // CF caron
];

/// Positions where ISO 8859-15 differs from ISO 8859-1.
const LATIN9_DIFF: [(u8, u16); 8] = [
    (0xA4, 0x20AC),
    (0xA6, 0x0160),
    (0xA8, 0x0161),
    (0xB4, 0x017D),
    (0xB8, 0x017E),
    (0xBC, 0x0152),
    (0xBD, 0x0153),
    (0xBE, 0x0178),
];

/// Positions where ISO 8859-9 differs from ISO 8859-1.
const LATIN5_DIFF: [(u8, u16); 6] = [
    (0xD0, 0x011E),
    (0xDD, 0x0130),
    (0xDE, 0x015E),
    (0xF0, 0x011F),
    (0xFD, 0x0131),
    (0xFE, 0x015F),
];

/// The character table a string selects, and the bytes after the selector.
#[must_use]
fn select(raw: &[u8]) -> (Charset, &[u8]) {
    let Some(&first) = raw.first() else {
        return (Charset::Iso6937, raw);
    };
    let rest = raw.get(1..).unwrap_or(&[]);
    match first {
        // A.3: selector n selects ISO 8859-(n+4), for n in 1..=11 with 8
        // reserved (there is no ISO 8859-12).
        0x01..=0x07 | 0x09..=0x0B => (Charset::Iso8859(first.saturating_add(4)), rest),
        0x10 => {
            // Three-byte selector: 0x10, then a sixteen-bit table number.
            let n = raw
                .get(1..3)
                .and_then(|b| Some(u16::from_be_bytes([*b.first()?, *b.get(1)?])));
            match n {
                Some(n) if n <= 15 => (Charset::Iso8859(n as u8), raw.get(3..).unwrap_or(&[])),
                _ => (Charset::Unsupported(0x10), raw.get(3..).unwrap_or(&[])),
            }
        }
        0x11 => (Charset::Utf16Be, rest),
        0x15 => (Charset::Utf8, rest),
        0x08 | 0x12..=0x14 | 0x16..=0x1F => (Charset::Unsupported(first), rest),
        // 0x00 is not a legal selector and is not a printable character
        // either; treating it as the default table is what makes an all-zero
        // padding field decode to an empty string rather than an error.
        _ => (Charset::Iso6937, raw),
    }
}

/// Decode a DVB text string.
#[must_use]
pub fn decode(raw: &[u8]) -> String {
    decode_with_charset(raw).0
}

/// Decode, and report which table was used.
#[must_use]
pub fn decode_with_charset(raw: &[u8]) -> (String, Charset) {
    let (charset, body) = select(raw);
    let text = match charset {
        Charset::Iso6937 => decode_6937(body),
        Charset::Iso8859(n) => decode_8859(body, n),
        Charset::Utf16Be => decode_utf16be(body),
        Charset::Utf8 => String::from_utf8_lossy(body).into_owned(),
        Charset::Unsupported(_) => decode_ascii_only(body),
    };
    (text, charset)
}

/// What a byte in the `0x80`-`0x9F` control range means.
enum Control {
    /// Not a control code at all.
    None,
    /// Consumed, emitting nothing: the emphasis selectors.
    Drop,
    /// `0x8A`, the only control code that produces a character.
    LineBreak,
}

const fn control(byte: u8) -> Control {
    match byte {
        0x8A => Control::LineBreak,
        0x80..=0x89 | 0x8B..=0x9F => Control::Drop,
        _ => Control::None,
    }
}

/// Handle `byte` if it is a control code. Returns whether it was consumed.
fn push_control(out: &mut String, byte: u8) -> bool {
    match control(byte) {
        Control::LineBreak => {
            out.push('\n');
            true
        }
        Control::Drop => true,
        Control::None => false,
    }
}

fn decode_6937(body: &[u8]) -> String {
    let mut out = String::new();
    let mut pending: Option<char> = None;
    for &b in body {
        if push_control(&mut out, b) {
            continue;
        }
        match b {
            0x00..=0x7F => {
                out.push(b as char);
                if let Some(mark) = pending.take() {
                    out.push(mark);
                }
            }
            0xC0..=0xCF => {
                // A diacritic prefix: the mark applies to the *next*
                // character, so it is held and emitted after it.
                let idx = usize::from(b - 0xC0);
                let mark = ISO6937_DIACRITIC.get(idx).copied().unwrap_or(0);
                // Zero is "unassigned", not U+0000: `from_u32(0)` succeeds and
                // would silently plant a NUL in the middle of a service name.
                pending = if mark == 0 {
                    None
                } else {
                    char::from_u32(u32::from(mark))
                };
                if pending.is_none() {
                    out.push('\u{FFFD}');
                }
            }
            0xA0..=0xFF => {
                let idx = usize::from(b - 0xA0);
                let cp = ISO6937_HIGH.get(idx).copied().unwrap_or(0);
                out.push(char::from_u32(u32::from(cp)).unwrap_or('\u{FFFD}'));
                if let Some(mark) = pending.take() {
                    out.push(mark);
                }
            }
            _ => out.push('\u{FFFD}'),
        }
    }
    // A trailing diacritic with nothing to attach to.
    if pending.is_some() {
        out.push('\u{FFFD}');
    }
    out
}

fn decode_8859(body: &[u8], n: u8) -> String {
    let supported = matches!(n, 1 | 9 | 15);
    let mut out = String::new();
    for &b in body {
        if push_control(&mut out, b) {
            continue;
        }
        if b < 0xA0 {
            out.push(b as char);
            continue;
        }
        if !supported {
            out.push('\u{FFFD}');
            continue;
        }
        let diff = match n {
            15 => LATIN9_DIFF.iter().find(|&&(k, _)| k == b).map(|&(_, v)| v),
            9 => LATIN5_DIFF.iter().find(|&&(k, _)| k == b).map(|&(_, v)| v),
            _ => None,
        };
        let cp = u32::from(diff.unwrap_or(u16::from(b)));
        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
    }
    out
}

fn decode_utf16be(body: &[u8]) -> String {
    let units: Vec<u16> = body
        .chunks_exact(2)
        .filter_map(|c| Some(u16::from_be_bytes([*c.first()?, *c.get(1)?])))
        .collect();
    String::from_utf16_lossy(&units)
}

fn decode_ascii_only(body: &[u8]) -> String {
    let mut out = String::new();
    for &b in body {
        if push_control(&mut out, b) {
            continue;
        }
        if b < 0x80 {
            out.push(b as char);
        } else {
            out.push('\u{FFFD}');
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_passes_through() {
        assert_eq!(decode(b"Service01"), "Service01");
        assert_eq!(decode(b"FFmpeg"), "FFmpeg");
        assert_eq!(decode(b""), "");
    }

    #[test]
    fn the_dvb_euro_sits_where_latin1_has_a_currency_sign() {
        assert_eq!(decode(&[0xA4]), "\u{20AC}");
        // Latin-1 selected explicitly keeps the currency sign.
        assert_eq!(decode(&[0x10, 0x00, 0x01, 0xA4]), "\u{00A4}");
    }

    #[test]
    fn iso6937_is_not_latin1() {
        // 0xA9 is an opening quote in 6937 and a copyright sign in Latin-1.
        assert_eq!(decode(&[0xA9]), "\u{2018}");
        assert_eq!(decode(&[0x10, 0x00, 0x01, 0xA9]), "\u{00A9}");
        // 0xD3 is the copyright sign in 6937.
        assert_eq!(decode(&[0xD3]), "\u{00A9}");
    }

    #[test]
    fn a_diacritic_prefix_follows_its_base_character() {
        // acute + e
        assert_eq!(decode(&[0xC2, b'e']), "e\u{0301}");
        // caron + c, inside a word
        assert_eq!(decode(b"\xCFCesk\xC2y"), "C\u{030C}esky\u{0301}");
    }

    #[test]
    fn a_dangling_diacritic_becomes_a_replacement_character() {
        assert_eq!(decode(&[0xC2]), "\u{FFFD}");
        assert_eq!(decode(&[0xC0, b'x']), "\u{FFFD}x");
    }

    #[test]
    fn utf8_and_utf16_selectors() {
        let mut s = vec![0x15];
        s.extend_from_slice("héllo".as_bytes());
        assert_eq!(decode(&s), "héllo");
        let mut s = vec![0x11];
        for u in "héllo".encode_utf16() {
            s.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(decode(&s), "héllo");
    }

    #[test]
    fn latin15_and_latin9_differences() {
        assert_eq!(decode(&[0x0B, 0xA4]), "\u{20AC}");
        assert_eq!(decode(&[0x05, 0xDD]), "\u{0130}");
        assert_eq!(decode(&[0x10, 0x00, 0x0F, 0xBD]), "\u{0153}");
    }

    #[test]
    fn an_undecoded_table_produces_replacement_characters_not_rubbish() {
        // ISO 8859-5, Cyrillic: selector 0x01.
        let (text, charset) = decode_with_charset(&[0x01, b'A', 0xC0]);
        assert_eq!(charset, Charset::Iso8859(5));
        assert_eq!(text, "A\u{FFFD}");
    }

    #[test]
    fn control_codes_are_dropped_and_the_line_break_kept() {
        assert_eq!(decode(b"a\x86b\x87c"), "abc");
        assert_eq!(decode(b"a\x8Ab"), "a\nb");
    }

    #[test]
    fn every_byte_sequence_decodes_without_panicking() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let _ = decode(&[a, b]);
                let _ = decode(&[a, b, 0x41]);
            }
        }
    }

    #[test]
    fn a_truncated_selector_does_not_read_past_the_end() {
        assert_eq!(decode(&[0x10]), "");
        assert_eq!(decode(&[0x10, 0x00]), "");
        assert_eq!(decode(&[0x11, 0x00]), "");
    }
}
