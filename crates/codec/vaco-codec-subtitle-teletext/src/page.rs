//! A decoded Level 1 Teletext page: a 40-column by 25-row character grid
//! plus the page header's address and control bits (EN 300 706 §9.3, §12).
//!
//! Row 0 is the page header's display data (columns 8-39; columns 0-7 are
//! where a real decoder overlays the page number itself, which this crate
//! does not synthesise since it carries no text). Rows 1-24 are the packets
//! X/1-X/24 body text.

use crate::charset::latin_g0;
use crate::{hamming, parity};

/// One of the eight fixed Teletext colours (EN 300 706 §12.1, CLUT 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    Black,
    #[default]
    White,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
}

impl Color {
    const fn from_code(code: u8) -> Self {
        match code & 0x7 {
            0 => Self::Black,
            1 => Self::Red,
            2 => Self::Green,
            3 => Self::Yellow,
            4 => Self::Blue,
            5 => Self::Magenta,
            6 => Self::Cyan,
            _ => Self::White,
        }
    }
}

/// What a display cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// A resolved Latin G0 character (space for control-code positions).
    Text(char),
    /// A raw G1 mosaic code point (`0x20..=0x7F`, spacing attributes
    /// excluded): EN 300 706 §12.1/§15.7.1 pack the six sub-cell blocks into
    /// bits 0-4 and 6 of the code itself, which this type preserves
    /// unmodified rather than re-packing into a private bit order, since a
    /// renderer needs the spec's own layout to draw it.
    Mosaic { code: u8, separated: bool },
    /// An odd-parity check failed for this byte; see
    /// [`Page::corrupt_parity`].
    Corrupt,
}

/// One character cell: a glyph plus the spacing attributes in force when it
/// was transmitted (EN 300 706 §12.2, Table 26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent Table 26 spacing attribute, not a state machine"
)]
pub struct Cell {
    pub glyph: Glyph,
    pub fg: Color,
    pub bg: Color,
    pub flash: bool,
    pub conceal: bool,
    pub double_height: bool,
    pub double_width: bool,
    pub boxed: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            glyph: Glyph::Text(' '),
            fg: Color::White,
            bg: Color::Black,
            flash: false,
            conceal: false,
            double_height: false,
            double_width: false,
            boxed: false,
        }
    }
}

/// One display row: exactly 40 columns (EN 300 706 §7.1.4's fixed 40-byte
/// data field), never attacker-sized.
pub type Row = [Cell; 40];

/// The page header's control bits, `C4`-`C14` (EN 300 706 Table 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, spec-named control bit (Table 2), not a state machine"
)]
pub struct ControlBits {
    pub erase_page: bool,
    pub newsflash: bool,
    pub subtitle: bool,
    pub suppress_header: bool,
    pub update_indicator: bool,
    pub interrupted_sequence: bool,
    pub inhibit_display: bool,
    pub magazine_serial: bool,
    /// `C12`,`C13`,`C14` packed as a 3-bit value (Table 32); see
    /// [`crate::charset`]'s module docs for why this crate does not act on
    /// it beyond storing it.
    pub national_option: u8,
}

/// A fully assembled Level 1 page.
#[derive(Debug, Clone)]
pub struct Page {
    pub magazine: u8,
    /// Two hex digits, `page_tens << 4 | page_units` (EN 300 706 §9.3.1.1).
    pub page_number: u8,
    /// 13-bit subcode: `S1 | S2 << 4 | S3 << 7 | S4 << 11` (§9.3.1.2).
    pub subcode: u16,
    pub control: ControlBits,
    pub rows: [Row; 25],
    /// Odd-parity failures folded into this page, so a silently-dropped
    /// byte is at least countable (AGENT-CONSTRAINTS's "make the discard
    /// countable" rule).
    pub corrupt_parity: u32,
    /// Hamming 8/4 double-bit errors in the header's address/control/subcode
    /// bytes, folded the same way.
    pub corrupt_hamming: u32,
}

impl Page {
    fn blank(magazine: u8) -> Self {
        Self {
            magazine,
            page_number: 0,
            subcode: 0,
            control: ControlBits::default(),
            rows: [[Cell::default(); 40]; 25],
            corrupt_parity: 0,
            corrupt_hamming: 0,
        }
    }

    fn row_mut(&mut self, row: usize) -> Option<&mut Row> {
        self.rows.get_mut(row)
    }

    /// Parse a page header packet (X/0): `payload` is the 40 bytes
    /// following the magazine/packet address (EN 300 706 bytes 6-45).
    #[must_use]
    pub fn from_header(magazine: u8, payload: &[u8]) -> Self {
        let mut page = Self::blank(magazine);
        let mut corrupt_hamming = 0u32;

        let mut nibble = |byte: Option<&u8>| -> u8 {
            let Some(&b) = byte else { return 0 };
            let (n, correction) = hamming::decode8(b);
            if !correction.is_usable() {
                corrupt_hamming = corrupt_hamming.saturating_add(1);
            }
            n
        };

        let units = nibble(payload.first());
        let tens = nibble(payload.get(1));
        page.page_number = (tens << 4) | units;

        let s1 = nibble(payload.get(2));
        let byte9 = nibble(payload.get(3));
        let s2 = byte9 & 0x7;
        let c4 = (byte9 >> 3) & 1;
        let s3 = nibble(payload.get(4));
        let byte11 = nibble(payload.get(5));
        let s4 = byte11 & 0x3;
        let c5 = (byte11 >> 2) & 1;
        let c6 = (byte11 >> 3) & 1;
        let byte12 = nibble(payload.get(6));
        let byte13 = nibble(payload.get(7));
        page.corrupt_hamming = corrupt_hamming;

        page.subcode = u16::from(s1)
            | (u16::from(s2) << 4)
            | (u16::from(s3) << 7)
            | (u16::from(s4) << 11);

        page.control = ControlBits {
            erase_page: c4 != 0,
            newsflash: c5 != 0,
            subtitle: c6 != 0,
            suppress_header: (byte12 & 0x1) != 0,
            update_indicator: (byte12 & 0x2) != 0,
            interrupted_sequence: (byte12 & 0x4) != 0,
            inhibit_display: (byte12 & 0x8) != 0,
            magazine_serial: (byte13 & 0x1) != 0,
            national_option: (byte13 >> 1) & 0x7,
        };

        if let Some(text) = payload.get(8..) {
            page.fill_row(0, 8, text);
        }
        page
    }

    /// Parse a directly-displayable body packet (X/1 to X/24): `payload` is
    /// the 40 data bytes for `row` (1-24).
    pub fn fill_body_row(&mut self, row: u8, payload: &[u8]) {
        let row = usize::from(row);
        self.fill_row(row, 0, payload);
    }

    fn fill_row(&mut self, row: usize, start_col: usize, payload: &[u8]) {
        let mut fg = Color::White;
        let mut bg = Color::Black;
        let mut flash = false;
        let mut conceal = false;
        let mut double_height = false;
        let mut double_width = false;
        let mut boxed = false;
        let mut mosaic_mode = false;
        let mut mosaic_separated = false;
        let mut hold_mosaics = false;
        let mut held: Option<(u8, bool)> = None;

        for (i, &byte) in payload.iter().enumerate() {
            let col = start_col.saturating_add(i);
            if col >= 40 {
                break;
            }
            let Some(code) = parity::decode(byte) else {
                self.corrupt_parity = self.corrupt_parity.saturating_add(1);
                if let Some(cell) = self.row_mut(row).and_then(|r| r.get_mut(col)) {
                    *cell = Cell {
                        glyph: Glyph::Corrupt,
                        fg,
                        bg,
                        flash,
                        conceal,
                        double_height,
                        double_width,
                        boxed,
                    };
                }
                continue;
            };

            let glyph = if code <= 0x1F {
                apply_control(
                    code,
                    &mut fg,
                    &mut bg,
                    &mut flash,
                    &mut conceal,
                    &mut double_height,
                    &mut double_width,
                    &mut boxed,
                    &mut mosaic_mode,
                    &mut mosaic_separated,
                    &mut hold_mosaics,
                );
                if hold_mosaics && mosaic_mode {
                    held.map_or(Glyph::Text(' '), |(c, sep)| Glyph::Mosaic {
                        code: c,
                        separated: sep,
                    })
                } else {
                    Glyph::Text(' ')
                }
            } else if mosaic_mode && !(0x40..=0x5F).contains(&code) {
                // "Bit 6" in the spec's 1-indexed transmission numbering
                // (§12.2 code 1/E) is code's 0-indexed bit 5 (0x20): the
                // hold-mosaic eligibility marker, not one of the six block
                // bits (which are bits 1,2,3,4,5,7 = 0x01,0x02,0x04,0x08,
                // 0x10,0x40).
                if code & 0x20 != 0 {
                    held = Some((code, mosaic_separated));
                }
                Glyph::Mosaic {
                    code,
                    separated: mosaic_separated,
                }
            } else {
                Glyph::Text(latin_g0(code))
            };

            if let Some(cell) = self.row_mut(row).and_then(|r| r.get_mut(col)) {
                *cell = Cell {
                    glyph,
                    fg,
                    bg,
                    flash,
                    conceal,
                    double_height,
                    double_width,
                    boxed,
                };
            }
        }
    }
}

#[allow(clippy::too_many_arguments, reason = "one spacing-attribute state machine step")]
fn apply_control(
    code: u8,
    fg: &mut Color,
    bg: &mut Color,
    flash: &mut bool,
    conceal: &mut bool,
    double_height: &mut bool,
    double_width: &mut bool,
    boxed: &mut bool,
    mosaic_mode: &mut bool,
    mosaic_separated: &mut bool,
    hold_mosaics: &mut bool,
) {
    match code {
        0x00..=0x07 => {
            *fg = Color::from_code(code);
            *mosaic_mode = false;
        }
        0x08 => *flash = true,
        0x09 => *flash = false,
        0x0A => *boxed = false,
        0x0B => *boxed = true,
        0x0C => {
            *double_height = false;
            *double_width = false;
        }
        0x0D => *double_height = true,
        0x0E => *double_width = true,
        0x0F => {
            *double_height = true;
            *double_width = true;
        }
        0x10..=0x17 => {
            *fg = Color::from_code(code);
            *mosaic_mode = true;
        }
        0x18 => *conceal = true,
        0x19 => *mosaic_separated = false,
        0x1A => *mosaic_separated = true,
        0x1C => *bg = Color::Black,
        0x1D => *bg = *fg,
        0x1E => *hold_mosaics = true,
        0x1F => *hold_mosaics = false,
        // 0x1B (ESC/second-G0 toggle) is not implemented, see crate::charset;
        // every other value here is reserved or otherwise a no-op.
        _ => {}
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn parity_byte(data: u8) -> u8 {
        let d = data & 0x7F;
        if d.count_ones() % 2 == 1 {
            d
        } else {
            d | 0x80
        }
    }

    fn hamming_byte(nibble: u8) -> u8 {
        let d1 = nibble & 1;
        let d2 = (nibble >> 1) & 1;
        let d3 = (nibble >> 2) & 1;
        let d4 = (nibble >> 3) & 1;
        let p1 = 1 ^ d1 ^ d3 ^ d4;
        let p2 = 1 ^ d1 ^ d2 ^ d4;
        let p3 = 1 ^ d1 ^ d2 ^ d3;
        let p4 = 1 ^ p1 ^ d1 ^ p2 ^ d2 ^ p3 ^ d3 ^ d4;
        (p1 & 1)
            | ((d1 & 1) << 1)
            | ((p2 & 1) << 2)
            | ((d2 & 1) << 3)
            | ((p3 & 1) << 4)
            | ((d3 & 1) << 5)
            | ((p4 & 1) << 6)
            | ((d4 & 1) << 7)
    }

    #[test]
    fn header_decodes_page_number_and_text() {
        // Page 1/00, subcode 0, no control bits, text "HELLO" then spaces.
        let mut payload = vec![
            hamming_byte(0), // units
            hamming_byte(0), // tens
            hamming_byte(0), // S1
            hamming_byte(0), // S2 + C4
            hamming_byte(0), // S3
            hamming_byte(0), // S4 + C5,C6
            hamming_byte(0), // C7-C10
            hamming_byte(0), // C11-C14
        ];
        let text = "HELLO";
        for b in text.bytes() {
            payload.push(parity_byte(b));
        }
        while payload.len() < 8 + 32 {
            payload.push(parity_byte(b' '));
        }

        let page = Page::from_header(1, &payload);
        assert_eq!(page.magazine, 1);
        assert_eq!(page.page_number, 0x00);
        assert_eq!(page.corrupt_hamming, 0);
        assert_eq!(page.corrupt_parity, 0);
        for (i, ch) in text.chars().enumerate() {
            assert_eq!(page.rows[0][8 + i].glyph, Glyph::Text(ch));
        }
    }

    #[test]
    fn body_row_applies_alpha_colour_and_text() {
        let mut page = Page::blank_for_test(1);
        let mut payload = vec![0x01u8]; // Alpha Red (set-after)
        for b in "HI".bytes() {
            payload.push(b);
        }
        let payload: Vec<u8> = payload.into_iter().map(parity_byte).collect();
        page.fill_body_row(1, &payload);

        assert_eq!(page.rows[1][0].glyph, Glyph::Text(' '));
        assert_eq!(page.rows[1][1].fg, Color::Red);
        assert_eq!(page.rows[1][1].glyph, Glyph::Text('H'));
        assert_eq!(page.rows[1][2].glyph, Glyph::Text('I'));
    }

    #[test]
    fn a_bad_parity_byte_is_corrupt_and_counted() {
        let mut page = Page::blank_for_test(1);
        // 0x41 ('A') has even parity as-is: not a valid odd-parity byte.
        page.fill_body_row(1, &[0x41]);
        assert_eq!(page.corrupt_parity, 1);
        assert_eq!(page.rows[1][0].glyph, Glyph::Corrupt);
    }

    impl Page {
        fn blank_for_test(magazine: u8) -> Self {
            Self::blank(magazine)
        }
    }
}
