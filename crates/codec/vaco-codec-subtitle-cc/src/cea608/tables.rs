//! CEA-608 character and control-code tables.
//!
//! Every table here is keyed on the two data bytes of a byte pair *after*
//! the parity bit has been stripped and, for control codes, after the
//! channel-select bit (`0x08`) has been masked out of the first byte — see
//! [`super::strip_parity`] and [`super::CHANNEL_BIT`].

use crate::event::Color;

/// Decode a standard-character byte pair. `None` for a byte that is not a
/// standard character (a control code's first byte, or `0x00` padding).
///
/// The eleven substitutions below are where CEA-608's "basic North American"
/// character set diverges from ASCII in the same 0x20-0x7F range; every
/// other byte maps straight through.
#[must_use]
pub fn standard_char(byte: u8) -> Option<char> {
    let ch = match byte {
        0x27 => '\u{2019}', // right single quotation mark
        0x2A => '\u{00E1}', // á
        0x5C => '\u{00E9}', // é
        0x5E => '\u{00ED}', // í
        0x5F => '\u{00F3}', // ó
        0x60 => '\u{00FA}', // ú
        0x7B => '\u{00E7}', // ç
        0x7C => '\u{00F7}', // ÷
        0x7D => '\u{00D1}', // Ñ
        0x7E => '\u{00F1}', // ñ
        0x7F => '\u{2588}', // solid block
        0x20..=0x7A => char::from(byte),
        _ => return None,
    };
    Some(ch)
}

/// Decode a special-character code (first byte `0x11`/`0x19` after masking
/// the channel bit, second byte `0x30..=0x3F`).
#[must_use]
pub fn special_char(second: u8) -> Option<char> {
    let ch = match second {
        0x30 => '\u{00AE}', // ®
        0x31 => '\u{00B0}', // °
        0x32 => '\u{00BD}', // ½
        0x33 => '\u{00BF}', // ¿
        0x34 => '\u{2122}', // ™
        0x35 => '\u{00A2}', // ¢
        0x36 => '\u{00A3}', // £
        0x37 => '\u{266A}', // ♪
        0x38 => '\u{00E0}', // à
        0x39 => ' ',        // transparent space: rendered as a plain space
        0x3A => '\u{00E8}', // è
        0x3B => '\u{00E2}', // â
        0x3C => '\u{00EA}', // ê
        0x3D => '\u{00EE}', // î
        0x3E => '\u{00F4}', // ô
        0x3F => '\u{00FB}', // û
        _ => return None,
    };
    Some(ch)
}

/// Decode an extended-character code. `first` is the PAC-range first byte
/// with the channel bit already masked out (`0x12` or `0x13`); `second` is
/// `0x20..=0x3F`.
///
/// `0x12` is the Western-European accented set and `0x13` is the
/// French/German/Scandinavian set; each also redefines four ASCII
/// punctuation characters as "backspace over the basic-set glyph and draw
/// this instead", which real decoders treat as plain characters and this
/// one does too.
#[must_use]
pub fn extended_char(first: u8, second: u8) -> Option<char> {
    match first {
        0x12 => extended_char_1(second),
        0x13 => extended_char_2(second),
        _ => None,
    }
}

fn extended_char_1(second: u8) -> Option<char> {
    let ch = match second {
        0x20 => '\u{00C1}', // Á
        0x21 => '\u{00C9}', // É
        0x22 => '\u{00D3}', // Ó
        0x23 => '\u{00DA}', // Ú
        0x24 => '\u{00DC}', // Ü
        0x25 => '\u{00FC}', // ü
        0x26 => '\u{2018}', // '
        0x27 => '\u{00A1}', // ¡
        0x28 => '*',
        0x29 => '\'',
        0x2A => '\u{2014}', // em dash
        0x2B => '\u{00A9}', // (c)
        0x2C => '\u{2120}', // (sm)
        0x2D => '\u{2022}', // bullet
        0x2E => '\u{201C}', // "
        0x2F => '\u{201D}', // "
        0x30 => '\u{00C0}', // À
        0x31 => '\u{00C2}', // Â
        0x32 => '\u{00C7}', // Ç
        0x33 => '\u{00C8}', // È
        0x34 => '\u{00CA}', // Ê
        0x35 => '\u{00CB}', // Ë
        0x36 => '\u{00EB}', // ë
        0x37 => '\u{00CE}', // Î
        0x38 => '\u{00CF}', // Ï
        0x39 => '\u{00EF}', // ï
        0x3A => '\u{00D4}', // Ô
        0x3B => '\u{00D9}', // Ù
        0x3C => '\u{00F9}', // ù
        0x3D => '\u{00DB}', // Û
        0x3E => '\u{00AB}', // «
        0x3F => '\u{00BB}', // »
        _ => return None,
    };
    Some(ch)
}

fn extended_char_2(second: u8) -> Option<char> {
    let ch = match second {
        0x20 => '\u{00C3}', // Ã
        0x21 => '\u{00E3}', // ã
        0x22 => '\u{00CD}', // Í
        0x23 => '\u{00CC}', // Ì
        0x24 => '\u{00EC}', // ì
        0x25 => '\u{00D2}', // Ò
        0x26 => '\u{00F2}', // ò
        0x27 => '\u{00D5}', // Õ
        0x28 => '\u{00F5}', // õ
        0x29 => '{',
        0x2A => '}',
        0x2B => '\\',
        0x2C => '^',
        0x2D => '_',
        0x2E => '|',
        0x2F => '~',
        0x30 => '\u{00C4}', // Ä
        0x31 => '\u{00E4}', // ä
        0x32 => '\u{00D6}', // Ö
        0x33 => '\u{00F6}', // ö
        0x34 => '\u{00DF}', // ß
        0x35 => '\u{00A5}', // ¥
        0x36 => '\u{00A4}', // general currency sign
        0x37 => '\u{00A6}', // broken bar
        0x38 => '\u{00C5}', // Å
        0x39 => '\u{00E5}', // å
        0x3A => '\u{00D8}', // Ø
        0x3B => '\u{00F8}', // ø
        // 0x3C-0x3F are the box-drawing border corner characters; not
        // mapped to a character.
        _ => return None,
    };
    Some(ch)
}

/// The sixteen misc control codes, first byte `0x14`/`0x15`/`0x1C`/`0x1D`
/// after masking the channel bit (the `0x01` field-parity bit is masked too
/// since `cc_type` already tells the caller which field this is), second
/// byte `0x20..=0x2F`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiscControl {
    ResumeCaptionLoading,
    Backspace,
    AlarmOff,
    AlarmOn,
    DeleteToEndOfRow,
    RollUp2,
    RollUp3,
    RollUp4,
    FlashOn,
    ResumeDirectCaptioning,
    TextRestart,
    ResumeTextDisplay,
    EraseDisplayedMemory,
    CarriageReturn,
    EraseNonDisplayedMemory,
    EndOfCaption,
}

/// Decode a misc control code from its second byte.
#[must_use]
pub fn misc_control(second: u8) -> Option<MiscControl> {
    use MiscControl::{
        AlarmOff, AlarmOn, Backspace, CarriageReturn, DeleteToEndOfRow, EndOfCaption,
        EraseDisplayedMemory, EraseNonDisplayedMemory, FlashOn, ResumeCaptionLoading,
        ResumeDirectCaptioning, ResumeTextDisplay, RollUp2, RollUp3, RollUp4, TextRestart,
    };
    Some(match second {
        0x20 => ResumeCaptionLoading,
        0x21 => Backspace,
        0x22 => AlarmOff,
        0x23 => AlarmOn,
        0x24 => DeleteToEndOfRow,
        0x25 => RollUp2,
        0x26 => RollUp3,
        0x27 => RollUp4,
        0x28 => FlashOn,
        0x29 => ResumeDirectCaptioning,
        0x2A => TextRestart,
        0x2B => ResumeTextDisplay,
        0x2C => EraseDisplayedMemory,
        0x2D => CarriageReturn,
        0x2E => EraseNonDisplayedMemory,
        0x2F => EndOfCaption,
        _ => return None,
    })
}

/// A mid-row style-change code: first byte `0x11`/`0x19` after masking the
/// channel bit, second byte `0x20..=0x2F`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidRow {
    /// `None` for the "white italics" code, which changes style without
    /// changing color.
    pub color: Option<Color>,
    pub italics: bool,
    pub underline: bool,
}

/// Decode a mid-row style-change code.
#[must_use]
pub fn mid_row(second: u8) -> Option<MidRow> {
    let underline = second & 1 != 0;
    let (color, italics) = match second & 0xFE {
        0x20 => (Some(Color::White), false),
        0x22 => (Some(Color::Green), false),
        0x24 => (Some(Color::Blue), false),
        0x26 => (Some(Color::Cyan), false),
        0x28 => (Some(Color::Red), false),
        0x2A => (Some(Color::Yellow), false),
        0x2C => (Some(Color::Magenta), false),
        0x2E => (None, true),
        _ => return None,
    };
    Some(MidRow {
        color,
        italics,
        underline,
    })
}

/// Decode a tab-offset code (first byte `0x17`/`0x1F` after masking the
/// channel bit) into the number of columns to move right.
#[must_use]
pub fn tab_offset(second: u8) -> Option<u8> {
    match second {
        0x21 => Some(1),
        0x22 => Some(2),
        0x23 => Some(3),
        _ => None,
    }
}

/// A Preamble Address Code: sets the row, indent-or-color, and underline for
/// whatever text follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pac {
    /// Display row, 1-15.
    pub row: u8,
    /// `None` when this PAC sets an indent instead of a color.
    pub color: Option<Color>,
    pub italics: bool,
    /// 0, 4, 8, ..., 28 when this PAC sets an indent instead of a color.
    pub indent: Option<u8>,
    pub underline: bool,
}

/// Decode a Preamble Address Code. `first` is the first byte with the
/// channel bit already masked out.
#[must_use]
pub fn pac(first: u8, second: u8) -> Option<Pac> {
    let row = match (first, second) {
        (0x11, 0x40..=0x5F) => 1,
        (0x11, 0x60..=0x7F) => 2,
        (0x12, 0x40..=0x5F) => 3,
        (0x12, 0x60..=0x7F) => 4,
        (0x15, 0x40..=0x5F) => 5,
        (0x15, 0x60..=0x7F) => 6,
        (0x16, 0x40..=0x5F) => 7,
        (0x16, 0x60..=0x7F) => 8,
        (0x17, 0x40..=0x5F) => 9,
        (0x17, 0x60..=0x7F) => 10,
        (0x10, 0x40..=0x5F) => 11,
        (0x13, 0x40..=0x5F) => 12,
        (0x13, 0x60..=0x7F) => 13,
        (0x14, 0x40..=0x5F) => 14,
        (0x14, 0x60..=0x7F) => 15,
        _ => return None,
    };
    let underline = second & 1 != 0;
    let (color, italics, indent) = match second & 0x1E {
        0x00 => (Some(Color::White), false, None),
        0x02 => (Some(Color::Green), false, None),
        0x04 => (Some(Color::Blue), false, None),
        0x06 => (Some(Color::Cyan), false, None),
        0x08 => (Some(Color::Red), false, None),
        0x0A => (Some(Color::Yellow), false, None),
        0x0C => (Some(Color::Magenta), false, None),
        0x0E => (None, true, None),
        0x10 => (None, false, Some(0)),
        0x12 => (None, false, Some(4)),
        0x14 => (None, false, Some(8)),
        0x16 => (None, false, Some(12)),
        0x18 => (None, false, Some(16)),
        0x1A => (None, false, Some(20)),
        0x1C => (None, false, Some(24)),
        0x1E => (None, false, Some(28)),
        _ => return None,
    };
    Some(Pac {
        row,
        color,
        italics,
        indent,
        underline,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn standard_char_substitutions() {
        assert_eq!(standard_char(0x41), Some('A'));
        assert_eq!(standard_char(0x7F), Some('\u{2588}'));
        assert_eq!(standard_char(0x27), Some('\u{2019}'));
    }

    #[test]
    fn pac_row_table_covers_all_15_rows() {
        let mut rows: Vec<u8> = (0x40u8..=0x7F)
            .flat_map(|second| {
                [0x10u8, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]
                    .into_iter()
                    .filter_map(move |first| pac(first, second).map(|p| p.row))
            })
            .collect();
        rows.sort_unstable();
        rows.dedup();
        assert_eq!(rows, (1..=15).collect::<Vec<_>>());
    }

    #[test]
    fn pac_row1_white_no_indent() {
        let p = pac(0x11, 0x40).expect("row 1, white, no underline");
        assert_eq!(p.row, 1);
        assert_eq!(p.color, Some(Color::White));
        assert_eq!(p.indent, None);
        assert!(!p.underline);
    }

    #[test]
    fn pac_indent_and_underline() {
        let p = pac(0x14, 0x7F).expect("row 15, indent 28, underline");
        assert_eq!(p.row, 15);
        assert_eq!(p.indent, Some(28));
        assert!(p.underline);
    }

    #[test]
    fn misc_control_table_complete() {
        let found: Vec<_> = (0x20..=0x2Fu8).filter_map(misc_control).collect();
        assert_eq!(found.len(), 16);
    }
}
