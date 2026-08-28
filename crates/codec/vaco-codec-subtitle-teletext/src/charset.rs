//! Latin G0 primary character set (EN 300 706 §15.6.1, Table 35) and its
//! English national-option substitution (§15.6.2, Table 36).
//!
//! # Coverage
//!
//! [`latin_g0`] renders the **English** national-option sub-set
//! unconditionally, regardless of a page's `C12`-`C14` control bits — see
//! [`crate::page::ControlBits::national_option`], which still records the
//! raw 3-bit value for a caller that wants to do better. The other twelve
//! sub-sets Table 36 defines (German, Swedish/Finnish/Hungarian, Italian,
//! French, Portuguese/Spanish, Czech/Slovak, Polish, Turkish,
//! Serbian/Croatian/Slovenian, Rumanian, Estonian, Lettish/Lithuanian) are
//! **not implemented**; a page that selects one of them still renders with
//! the English glyphs at the thirteen national-option code points. This,
//! along with G2 access via packet X/26 and the `ESC` second-G0-set toggle,
//! is this crate's stated Level 1.5 gap (see the crate's top-level docs).

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

/// English national-option substitution (Table 36, `natopt_1`/`natopt_2`
/// columns), applied over the thirteen reserved G0 positions.
const fn english_override(code: u8) -> Option<char> {
    match code {
        0x23 => Some('\u{A3}'),   // £
        0x24 => Some('$'),
        0x40 => Some('@'),
        0x5B => Some('\u{2190}'), // ←
        0x5C => Some('\u{BD}'),   // ½
        0x5D => Some('\u{2192}'), // →
        0x5E => Some('\u{2191}'), // ↑
        0x5F => Some('#'),
        0x60 => Some('\u{2013}'), // –
        0x7B => Some('\u{BC}'),   // ¼
        0x7C => Some('\u{2016}'), // ‖
        0x7D => Some('\u{BE}'),   // ¾
        0x7E => Some('\u{F7}'),   // ÷
        _ => None,
    }
}

/// Resolve one odd-parity-decoded Latin G0 code point (`0x20..=0x7F`) to a
/// display character.
///
/// `code`'s top bit must already be clear (a 7-bit value out of
/// [`crate::parity::decode`]); callers outside `0x20..=0x7F` get the
/// international-reference glyph at that position, since G0's alphanumeric
/// range is exactly `2/0` to `7/F`.
#[must_use]
pub const fn latin_g0(code: u8) -> char {
    let c = code & 0x7F;
    if let Some(over) = english_override(c) {
        over
    } else {
        base(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_positions_pass_through() {
        assert_eq!(latin_g0(b'A'), 'A');
        assert_eq!(latin_g0(b'0'), '0');
        assert_eq!(latin_g0(0x20), ' ');
    }

    #[test]
    fn english_pound_sign_replaces_hash() {
        assert_eq!(latin_g0(0x23), '\u{A3}');
    }

    #[test]
    fn dollar_sign_survives_the_currency_sign_default() {
        assert_eq!(latin_g0(0x24), '$');
    }

    #[test]
    fn del_position_is_a_solid_block() {
        assert_eq!(latin_g0(0x7F), '\u{2588}');
    }

    #[test]
    fn arrows_and_fractions_are_at_their_spec_positions() {
        assert_eq!(latin_g0(0x5B), '\u{2190}');
        assert_eq!(latin_g0(0x5C), '\u{BD}');
        assert_eq!(latin_g0(0x7E), '\u{F7}');
    }
}
