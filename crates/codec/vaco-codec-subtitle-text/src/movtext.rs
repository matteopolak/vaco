//! 3GPP timed text (`mov_text` / `tx3g`) to ASS override markup.
//!
//! # The wire format, measured from real samples
//!
//! Unlike every other format in this crate, `mov_text` is binary. The layout
//! below was read off actual bytes produced by the reference
//! (`ffmpeg -i in.srt -c:s mov_text out.mp4`, then
//! `ffmpeg -i out.mp4 -c:s copy -f data -` and `xxd`), then cross-checked
//! against 3GPP TS 26.245:
//!
//! ```text
//! sample := u16be text_len, text_len bytes of UTF-8 text, then zero or more boxes
//! box    := u32be size (header included), 4-byte type, payload
//! styl   := u16be entry_count, entry_count * 12-byte entries
//! entry  := u16be start_char, u16be end_char, u16be font_id,
//!           u8 face_flags, u8 font_size, u32 rgba
//! ```
//!
//! Two facts here are measurements that a from-the-spec implementation gets
//! wrong, and both were confirmed by constructing the input deliberately:
//!
//! - **`text_len` counts bytes, but `styl` offsets count characters.** For
//!   `"ééé ital end"` the reference wrote `text_len = 15` (the UTF-8 byte
//!   count) and `start_char = 4`, `end_char = 8` — the character indices of
//!   `"ital"`, whose *byte* range is 7..11. Treating the offsets as byte
//!   offsets styles the wrong span on any non-ASCII line.
//! - **A character is a Unicode scalar, not a UTF-16 code unit.** For
//!   `"😀 ital end"` the reference wrote `start_char = 2`. UTF-16 counting
//!   would make the astral emoji two units and give 3. This is observable
//!   reference behaviour (D17) and is what this decoder reproduces; where the
//!   specification's own wording disagrees, the behaviour is the fact.
//!
//! # Closing tag
//!
//! A styled span ends with `{\r}` — a full style reset — not with the
//! `{\i0}`-style per-attribute closer [`crate::srt`] emits. Measured:
//! `<i>world</i>` through an `mov_text` round trip comes back as
//! `{\i1}world{\r}`, where the same cue read straight from `.srt` gives
//! `{\i1}world{\i0}`. Same markup language, two decoders, two conventions.

use crate::ass;

/// `face_style_flags` bit 0: bold.
pub const FACE_BOLD: u8 = 0x01;
/// `face_style_flags` bit 1: italic.
pub const FACE_ITALIC: u8 = 0x02;
/// `face_style_flags` bit 2: underline.
pub const FACE_UNDERLINE: u8 = 0x04;

/// One `styl` record: a half-open character range and the face to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleRecord {
    pub start_char: u16,
    pub end_char: u16,
    pub font_id: u16,
    pub face_flags: u8,
    pub font_size: u8,
    pub rgba: u32,
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    let hi = *data.get(at)?;
    let lo = *data.get(at.saturating_add(1))?;
    Some(u16::from(hi) << 8 | u16::from(lo))
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    let mut v = 0u32;
    for i in 0..4 {
        v = (v << 8) | u32::from(*data.get(at.saturating_add(i))?);
    }
    Some(v)
}

/// A parsed text sample: the text, and whatever style records came with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSample {
    pub text: String,
    pub styles: Vec<StyleRecord>,
}

/// Parse one `mov_text` sample.
///
/// Returns `None` only when the sample is too short to hold its own length
/// prefix. A sample whose declared `text_len` overruns the buffer is
/// truncated to what is actually present rather than rejected — a demuxer
/// handing over a short final sample is a real thing, and the reference
/// renders what it has.
///
/// Text that is not valid UTF-8 is decoded lossily; 3GPP requires UTF-8 (or
/// UTF-16 behind a BOM, which this decoder does not implement — see the
/// crate docs), so invalid bytes here are corruption rather than a coding
/// this decoder should try to honour.
#[must_use]
pub fn parse_sample(data: &[u8]) -> Option<TextSample> {
    let declared = usize::from(be16(data, 0)?);
    let body = data.get(2..)?;
    let take = declared.min(body.len());
    let text_bytes = body.get(..take).unwrap_or(&[]);
    let text = String::from_utf8_lossy(text_bytes).into_owned();

    let mut styles = Vec::new();
    let mut at = take;
    // Walk the trailing box list. Every step consumes at least 8 bytes, so
    // this cannot spin; a malformed size just ends the walk.
    while let Some(size) = be32(body, at) {
        let Some(kind) = body.get(at.saturating_add(4)..at.saturating_add(8)) else {
            break;
        };
        let size = size as usize;
        if size < 8 || at.saturating_add(size) > body.len() {
            break;
        }
        if kind == b"styl" {
            let payload_at = at.saturating_add(8);
            if let Some(count) = be16(body, payload_at) {
                for i in 0..usize::from(count) {
                    let e = payload_at
                        .saturating_add(2)
                        .saturating_add(i.saturating_mul(12));
                    // Stop at the first entry that does not fit: a declared
                    // count larger than the box is truncation, not a reason
                    // to allocate for 65535 entries.
                    let (Some(start_char), Some(end_char), Some(font_id)) = (
                        be16(body, e),
                        be16(body, e.saturating_add(2)),
                        be16(body, e.saturating_add(4)),
                    ) else {
                        break;
                    };
                    let (Some(&face_flags), Some(&font_size), Some(rgba)) = (
                        body.get(e.saturating_add(6)),
                        body.get(e.saturating_add(7)),
                        be32(body, e.saturating_add(8)),
                    ) else {
                        break;
                    };
                    styles.push(StyleRecord {
                        start_char,
                        end_char,
                        font_id,
                        face_flags,
                        font_size,
                        rgba,
                    });
                }
            }
        }
        at = at.saturating_add(size);
    }
    Some(TextSample { text, styles })
}

/// Decode one `mov_text` sample to ASS dialogue text.
///
/// Returns `None` for a sample carrying no text — the reference writes these
/// as gap fillers between cues (a bare `0x0000` length prefix, two bytes
/// total) and produces no dialogue line for them.
#[must_use]
pub fn to_ass(data: &[u8]) -> Option<String> {
    let sample = parse_sample(data)?;
    if sample.text.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (index, ch) in sample.text.chars().enumerate() {
        let index = u16::try_from(index).unwrap_or(u16::MAX);
        // Close first, so a span ending where the next begins does not get
        // its reset applied after the following span's opener.
        if sample
            .styles
            .iter()
            .any(|s| s.end_char == index && s.face_flags != 0 && s.start_char < s.end_char)
        {
            out.push_str("{\\r}");
        }
        for s in sample.styles.iter().filter(|s| s.start_char == index) {
            if s.face_flags & FACE_BOLD != 0 {
                out.push_str("{\\b1}");
            }
            if s.face_flags & FACE_ITALIC != 0 {
                out.push_str("{\\i1}");
            }
            if s.face_flags & FACE_UNDERLINE != 0 {
                out.push_str("{\\u1}");
            }
        }
        let mut buf = [0u8; 4];
        ass::escape_plain(&mut out, ch.encode_utf8(&mut buf));
    }
    let end = u16::try_from(sample.text.chars().count()).unwrap_or(u16::MAX);
    if sample
        .styles
        .iter()
        .any(|s| s.end_char >= end && s.face_flags != 0 && s.start_char < s.end_char)
    {
        out.push_str("{\\r}");
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Build a sample the way the reference lays one out.
    fn sample(text: &str, styles: &[(u16, u16, u8)]) -> Vec<u8> {
        let mut v = Vec::new();
        let bytes = text.as_bytes();
        v.extend_from_slice(&u16::try_from(bytes.len()).unwrap().to_be_bytes());
        v.extend_from_slice(bytes);
        if !styles.is_empty() {
            let size = 8 + 2 + styles.len() * 12;
            v.extend_from_slice(&u32::try_from(size).unwrap().to_be_bytes());
            v.extend_from_slice(b"styl");
            v.extend_from_slice(&u16::try_from(styles.len()).unwrap().to_be_bytes());
            for &(s, e, face) in styles {
                v.extend_from_slice(&s.to_be_bytes());
                v.extend_from_slice(&e.to_be_bytes());
                v.extend_from_slice(&1u16.to_be_bytes()); // font id
                v.push(face);
                v.push(0x10); // font size
                v.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
            }
        }
        v
    }

    #[test]
    fn plain_sample_round_trips() {
        let s = sample("Hello world", &[]);
        assert_eq!(to_ass(&s).unwrap(), "Hello world");
    }

    #[test]
    fn italic_span_matches_the_measured_reference_bytes() {
        // The exact bytes the reference wrote for "Hello world\nsecond line"
        // with <i>world</i>: styl start=6 end=11 face=0x02.
        let s = sample("Hello world\nsecond line", &[(6, 11, FACE_ITALIC)]);
        assert_eq!(to_ass(&s).unwrap(), "Hello {\\i1}world{\\r}\\Nsecond line");
    }

    #[test]
    fn offsets_are_characters_not_bytes() {
        // "ééé ital end": "ital" is chars 4..8, bytes 7..11. The reference
        // wrote 4/8, so byte-offset handling would style " ita" instead.
        let s = sample("ééé ital end", &[(4, 8, FACE_ITALIC)]);
        assert_eq!(to_ass(&s).unwrap(), "ééé {\\i1}ital{\\r} end");
    }

    #[test]
    fn a_character_is_a_scalar_not_a_utf16_unit() {
        // "😀 ital end": the reference wrote start=2, which is scalar
        // counting; UTF-16 counting would have made it 3.
        let s = sample("😀 ital end", &[(2, 6, FACE_ITALIC)]);
        assert_eq!(to_ass(&s).unwrap(), "😀 {\\i1}ital{\\r} end");
    }

    #[test]
    fn an_empty_gap_sample_produces_no_event() {
        assert_eq!(to_ass(&[0x00, 0x00]), None);
    }

    #[test]
    fn bold_and_underline_flags_are_honoured() {
        let s = sample("abc", &[(0, 3, FACE_BOLD | FACE_UNDERLINE)]);
        assert_eq!(to_ass(&s).unwrap(), "{\\b1}{\\u1}abc{\\r}");
    }

    #[test]
    fn a_declared_length_past_the_end_is_truncated_not_rejected() {
        let s = [0x00, 0xFF, b'h', b'i'];
        assert_eq!(to_ass(&s).unwrap(), "hi");
    }

    #[test]
    fn a_styl_count_larger_than_its_box_stops_at_the_last_whole_entry() {
        let mut v = Vec::new();
        v.extend_from_slice(&3u16.to_be_bytes());
        v.extend_from_slice(b"abc");
        v.extend_from_slice(&(8u32 + 2 + 12).to_be_bytes());
        v.extend_from_slice(b"styl");
        v.extend_from_slice(&9u16.to_be_bytes()); // claims nine, carries one
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&3u16.to_be_bytes());
        v.extend_from_slice(&1u16.to_be_bytes());
        v.push(FACE_BOLD);
        v.push(0x10);
        v.extend_from_slice(&0u32.to_be_bytes());
        let parsed = parse_sample(&v).unwrap();
        assert_eq!(parsed.styles.len(), 1);
        assert_eq!(to_ass(&v).unwrap(), "{\\b1}abc{\\r}");
    }

    #[test]
    fn a_truncated_sample_is_none_not_a_panic() {
        assert_eq!(to_ass(&[]), None);
        assert_eq!(to_ass(&[0x00]), None);
    }
}
