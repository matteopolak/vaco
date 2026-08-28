//! Raw text (`AV_CODEC_ID_TEXT`, the reference's `text` decoder) to ASS.
//!
//! The whole codec is "these bytes are the subtitle" — no markup language, no
//! escapes, no tags. So the decode step is exactly [`crate::ass::escape_plain`]:
//! line breaks become `\N` and nothing else changes.
//!
//! This is also the right entry point for a packet carrying a bare ASS `Text`
//! field rather than the reference's nine-field dialogue chunk, which is what
//! this workspace's own `vaco-subtitle-text` ASS demuxer produces — see
//! [`crate::ass`]'s module docs for why the two shapes are kept apart instead
//! of sniffed between.

use crate::ass;

/// Convert a raw text cue to ASS dialogue text.
#[must_use]
pub fn to_ass(text: &str) -> String {
    let mut out = String::new();
    ass::escape_plain(&mut out, text);
    out
}

/// Convert a raw text cue from bytes, decoding UTF-8 lossily.
///
/// A demuxer in this workspace passes cue bytes through verbatim, including
/// invalid UTF-8 (`vaco_format_subtitle::Cue::text` is a `Vec<u8>` for
/// exactly that reason), so the byte-to-text decision lands here, on the
/// decode side, which is where that crate's own docs say it belongs.
#[must_use]
pub fn to_ass_bytes(bytes: &[u8]) -> String {
    to_ass(&String::from_utf8_lossy(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_breaks_become_backslash_n_and_nothing_else_changes() {
        assert_eq!(to_ass("a\nb"), "a\\Nb");
        assert_eq!(to_ass("<i>not markup</i>"), "<i>not markup</i>");
        assert_eq!(to_ass("&amp; stays"), "&amp; stays");
    }

    #[test]
    fn invalid_utf8_is_replaced_not_rejected() {
        assert_eq!(to_ass_bytes(&[b'a', 0xFF, b'b']), "a\u{FFFD}b");
    }
}
