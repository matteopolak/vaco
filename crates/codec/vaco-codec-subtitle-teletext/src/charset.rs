//! Latin G0 primary character set (EN 300 706 §15.6.1, Table 35) and its
//! national-option substitution (§15.6.2, Table 36) at the thirteen
//! reserved code points.
//!
//! # Coverage
//!
//! A Level 1/1.5 page selects its national-option sub-set with the page
//! header's `C12`-`C14` control bits alone (§15.2: "At levels 1 and 1.5 the
//! national option sub-set in use on the page is defined by the C12, C13
//! and C14 control bits in the page header alone"), which — per Table 32's
//! first (default G0/G2 designation) row, the one that applies without a
//! packet X/28/M/29 this crate does not implement — selects one of eight
//! entries: `000` English, `001` German, `010` Swedish/Finnish/Hungarian,
//! `011` Italian, `100` French, `101` Portuguese/Spanish, `110`
//! Czech/Slovak, `111` unassigned in that row. [`latin_g0`] implements six
//! of the eight (English, German, Italian, French, Portuguese/Spanish,
//! Czech/Slovak); Swedish/Finnish/Hungarian (`010`) and the unassigned
//! value (`111`) fall back to English rather than guess. The other eleven
//! sub-sets Table 36 defines outside this default row's reach (Polish,
//! Turkish, Serbian/Croatian/Slovenian, Rumanian, Estonian,
//! Lettish/Lithuanian, ...) are not implemented for the same reason this
//! crate does not implement X/28 at all. This, along with G2 access via
//! packet X/26 beyond diacritical composition and the `ESC` second-G0-set
//! toggle, is this crate's stated Level 1.5 gap (see the crate's top-level
//! docs).
//!
//! Each sub-set below is transcribed directly from Table 36's own bitmap
//! figure (the standard ships the table as an image, not text) as a
//! complete 13-entry override list — including the handful of positions
//! where a sub-set's glyph happens to equal another's (e.g. Italian's
//! `5/D`-`5/F` are the same →/↑/# as English's) — rather than relying on
//! [`base`]'s fallback, since [`base`] does not reproduce those glyphs on
//! its own.

/// The base Latin G0 table (Table 35's "international reference" glyphs,
/// used at the thirteen national-option positions only when no
/// substitution applies) for code points `0x20..=0x7F`.
const fn base(code: u8) -> char {
    match code {
        0x24 => '\u{A4}', // international currency sign
        0x7F => '\u{2588}', // solid block (Table 35 note 4)
        c => c as char,   // every other position is plain ASCII
    }
}

type Overrides = [(u8, char); 13];

/// English (Table 36, `natopt_1`/`natopt_2` columns, "English" row).
const ENGLISH: Overrides = [
    (0x23, '\u{A3}'), // £
    (0x24, '$'),
    (0x40, '@'),
    (0x5B, '\u{2190}'), // ←
    (0x5C, '\u{BD}'),   // ½
    (0x5D, '\u{2192}'), // →
    (0x5E, '\u{2191}'), // ↑
    (0x5F, '#'),
    (0x60, '\u{2013}'), // –
    (0x7B, '\u{BC}'),   // ¼
    (0x7C, '\u{2016}'), // ‖
    (0x7D, '\u{BE}'),   // ¾
    (0x7E, '\u{F7}'),   // ÷
];

/// German ("German" row).
const GERMAN: Overrides = [
    (0x23, '#'),
    (0x24, '$'),
    (0x40, '\u{A7}'),   // §
    (0x5B, '\u{C4}'),   // Ä
    (0x5C, '\u{D6}'),   // Ö
    (0x5D, '\u{DC}'),   // Ü
    (0x5E, '^'),        // spacing circumflex
    (0x5F, '_'),        // spacing underscore
    (0x60, '\u{B0}'),   // °
    (0x7B, '\u{E4}'),   // ä
    (0x7C, '\u{F6}'),   // ö
    (0x7D, '\u{FC}'),   // ü
    (0x7E, '\u{DF}'),   // ß
];

/// Italian ("Italian" row) — note `5/D`-`5/F` coincide with English's
/// →/↑/#, transcribed explicitly rather than assumed.
const ITALIAN: Overrides = [
    (0x23, '\u{A3}'), // £
    (0x24, '$'),
    (0x40, '\u{E9}'),   // é
    (0x5B, '\u{B0}'),   // °
    (0x5C, '\u{E7}'),   // ç
    (0x5D, '\u{2192}'), // →
    (0x5E, '\u{2191}'), // ↑
    (0x5F, '#'),
    (0x60, '\u{F9}'),   // ù
    (0x7B, '\u{E0}'),   // à
    (0x7C, '\u{F2}'),   // ò
    (0x7D, '\u{E8}'),   // è
    (0x7E, '\u{EC}'),   // ì
];

/// French ("French" row).
const FRENCH: Overrides = [
    (0x23, '\u{E9}'), // é
    (0x24, '\u{EF}'), // ï
    (0x40, '\u{E0}'), // à
    (0x5B, '\u{EB}'), // ë
    (0x5C, '\u{EA}'), // ê
    (0x5D, '\u{F9}'), // ù
    (0x5E, '\u{EE}'), // î
    (0x5F, '#'),
    (0x60, '\u{E8}'), // è
    (0x7B, '\u{E2}'), // â
    (0x7C, '\u{F4}'), // ô
    (0x7D, '\u{FB}'), // û
    (0x7E, '\u{E7}'), // ç
];

/// Portuguese/Spanish ("Portuguese/Spanish" row) — `4/0` is genuinely a
/// plain, unaccented lowercase `i` in the spec's own figure, not a
/// transcription slip.
const PORTUGUESE_SPANISH: Overrides = [
    (0x23, '\u{E7}'), // ç
    (0x24, '$'),
    (0x40, 'i'),
    (0x5B, '\u{E1}'), // á
    (0x5C, '\u{E9}'), // é
    (0x5D, '\u{ED}'), // í
    (0x5E, '\u{F3}'), // ó
    (0x5F, '\u{FA}'), // ú
    (0x60, '\u{BF}'), // ¿
    (0x7B, '\u{FC}'), // ü
    (0x7C, '\u{F1}'), // ñ
    (0x7D, '\u{E8}'), // è
    (0x7E, '\u{E0}'), // à
];

/// Czech/Slovak ("Czech/Slovak" row).
const CZECH_SLOVAK: Overrides = [
    (0x23, '#'),
    (0x24, '\u{16F}'), // ů
    (0x40, '\u{10D}'), // č
    (0x5B, '\u{165}'), // ť
    (0x5C, '\u{17E}'), // ž
    (0x5D, '\u{FD}'),  // ý
    (0x5E, '\u{ED}'),  // í
    (0x5F, '\u{159}'), // ř
    (0x60, '\u{E9}'),  // é
    (0x7B, '\u{E1}'),  // á
    (0x7C, '\u{11B}'), // ě
    (0x7D, '\u{FA}'),  // ú
    (0x7E, '\u{161}'), // š
];

/// Resolve one odd-parity-decoded Latin G0 code point (`0x20..=0x7F`) to a
/// display character, under the national-option sub-set `national_option`
/// selects (`C12`-`C14`, 0-7; see the module docs for which eight this is).
///
/// `code`'s top bit must already be clear (a 7-bit value out of
/// [`crate::parity::decode`]); callers outside `0x20..=0x7F` get the
/// international-reference glyph at that position, since G0's alphanumeric
/// range is exactly `2/0` to `7/F`.
#[must_use]
#[allow(clippy::indexing_slicing, reason = "loop condition i < subset.len() keeps i in bounds; slice::get is not yet const-stable")]
pub const fn latin_g0(code: u8, national_option: u8) -> char {
    let c = code & 0x7F;
    let subset: &Overrides = match national_option & 0x7 {
        1 => &GERMAN,
        3 => &ITALIAN,
        4 => &FRENCH,
        5 => &PORTUGUESE_SPANISH,
        6 => &CZECH_SLOVAK,
        _ => &ENGLISH, // 0 (English), 2 (Swedish/Finnish/Hungarian, not transcribed) and 7 (unassigned)
    };
    let mut i = 0;
    while i < subset.len() {
        let (pos, ch) = subset[i];
        if pos == c {
            return ch;
        }
        i += 1;
    }
    base(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_positions_pass_through() {
        assert_eq!(latin_g0(b'A', 0), 'A');
        assert_eq!(latin_g0(b'0', 0), '0');
        assert_eq!(latin_g0(0x20, 0), ' ');
    }

    #[test]
    fn english_pound_sign_replaces_hash() {
        assert_eq!(latin_g0(0x23, 0), '\u{A3}');
    }

    #[test]
    fn dollar_sign_survives_the_currency_sign_default() {
        assert_eq!(latin_g0(0x24, 0), '$');
    }

    #[test]
    fn del_position_is_a_solid_block() {
        assert_eq!(latin_g0(0x7F, 0), '\u{2588}');
    }

    #[test]
    fn arrows_and_fractions_are_at_their_spec_positions() {
        assert_eq!(latin_g0(0x5B, 0), '\u{2190}');
        assert_eq!(latin_g0(0x5C, 0), '\u{BD}');
        assert_eq!(latin_g0(0x7E, 0), '\u{F7}');
    }

    #[test]
    fn german_selects_umlauts_and_eszett() {
        assert_eq!(latin_g0(0x5B, 1), '\u{C4}'); // Ä
        assert_eq!(latin_g0(0x7B, 1), '\u{E4}'); // ä
        assert_eq!(latin_g0(0x7E, 1), '\u{DF}'); // ß
        assert_eq!(latin_g0(0x40, 1), '\u{A7}'); // §
    }

    #[test]
    fn italian_selects_accented_vowels() {
        assert_eq!(latin_g0(0x40, 3), '\u{E9}'); // é
        assert_eq!(latin_g0(0x60, 3), '\u{F9}'); // ù
        assert_eq!(latin_g0(0x7E, 3), '\u{EC}'); // ì
        // Italian's 5/D coincides with English's own → at the same position.
        assert_eq!(latin_g0(0x5D, 3), '\u{2192}');
    }

    #[test]
    fn french_selects_accented_vowels_and_cedilla() {
        assert_eq!(latin_g0(0x23, 4), '\u{E9}'); // é
        assert_eq!(latin_g0(0x40, 4), '\u{E0}'); // à
        assert_eq!(latin_g0(0x7E, 4), '\u{E7}'); // ç
    }

    #[test]
    fn portuguese_spanish_selects_inverted_punctuation_and_tilde_n() {
        assert_eq!(latin_g0(0x60, 5), '\u{BF}'); // ¿
        assert_eq!(latin_g0(0x7C, 5), '\u{F1}'); // ñ
        assert_eq!(latin_g0(0x40, 5), 'i'); // plain, per the spec's own figure
    }

    #[test]
    fn czech_slovak_selects_caron_and_acute_letters() {
        assert_eq!(latin_g0(0x40, 6), '\u{10D}'); // č
        assert_eq!(latin_g0(0x5C, 6), '\u{17E}'); // ž
        assert_eq!(latin_g0(0x7E, 6), '\u{161}'); // š
    }

    #[test]
    fn unassigned_subsets_fall_back_to_english() {
        // 2 = Swedish/Finnish/Hungarian, 7 = unassigned in Table 32's
        // default row: neither is transcribed, so both render English's £.
        assert_eq!(latin_g0(0x23, 2), '\u{A3}');
        assert_eq!(latin_g0(0x23, 7), '\u{A3}');
    }

    #[test]
    fn every_subset_leaves_non_reserved_positions_at_the_base_glyph() {
        for national_option in 0u8..8 {
            assert_eq!(latin_g0(b'A', national_option), 'A');
            assert_eq!(latin_g0(b'z', national_option), 'z');
            assert_eq!(latin_g0(b'5', national_option), '5');
        }
    }
}
