//! Packet X/26: composite characters (EN 300 706 §12.3), the feature the
//! spec itself names as what turns a Level 1 page into a Level 1.5
//! transmission (§15.1: "additional packets X/26 may be introduced to form
//! Level 1.5 transmissions ... typically ... a few characters from the G2
//! supplementary set ... plus a few G0 characters with diacritical marks").
//!
//! # Scope: composition only, not the whole of X/26
//!
//! Table 27 (§12.3.1) lists thirty-two triplet modes split across a Row
//! Address group and a Column Address group; most of them are colours, PDC
//! time codes, DRCS invocation, font style or object linking — Level 2.5/
//! 3.5 machinery this crate has no page model for (no DRCS table, no
//! object store, no side panels). What this module implements is exactly
//! the subset that changes *text* on a Level 1.5 page:
//!
//! - Row group mode `00001` (Full Row Colour) and `00100` (Set Active
//!   Position): both set the Active Position's row (§12.3.2's row-address
//!   encoding: address 40 is row 24, 41-63 are rows 1-23); the colour part
//!   of Full Row Colour and the reserved/PDC row-group modes are ignored.
//! - Row group mode `00111` (Address Display Row 0): resets the Active
//!   Position to row 0, column 0.
//! - Column group mode `01111` (Character from the G2 Supplementary Set):
//!   sets the column, at present only meaningfully for a code point this
//!   crate already knows how to render — [`g2_char`] documents exactly how
//!   little of Table 37 that is today.
//! - Column group modes `10000`-`11111` (Characters Including Diacritical
//!   Marks): the composition machinery itself — [`compose`].
//!
//! Every other mode still moves the column (per §12.3.4: "For all mode
//! description values except all reserved values and the one used for
//! PDC, the address field sets the column co-ordinate") but overwrites
//! nothing, which is the spec-correct behaviour for a decoder that does
//! not implement that mode's actual function.
//!
//! # Why this reads triplets itself instead of reusing `hamming::decode24`'s caller
//!
//! [`crate::decoder`]'s `apply_record` already Hamming-24/18-decodes every
//! X/26 triplet to detect corruption (so a malformed enhancement packet is
//! never misread as page text); this module is what a magazine's packet 26
//! is routed to instead of that generic skip, and it re-decodes the same
//! bytes because the two need different things from the result — corruption
//! detection alone versus the address/mode/data fields themselves.

use crate::hamming::{self, triplet_address, triplet_data, triplet_mode, Correction};
use crate::page::{Glyph, Page};

/// Row Address group mode: Full Row Colour (§12.3.3, Table 28). Sets the
/// row and resets the column to 0; the colour data this triplet also
/// carries is out of scope (no colour model for non-spacing attributes).
const MODE_FULL_ROW_COLOUR: u8 = 0b00001;
/// Row Address group mode: Set Active Position. Sets the row from the
/// address field and, when the data field is `< 40`, the column too.
const MODE_SET_ACTIVE_POSITION: u8 = 0b00100;
/// Row Address group mode: Address Display Row 0. Resets to row 0, column 0.
const MODE_ADDRESS_ROW_0: u8 = 0b00111;
/// Column Address group mode: Character from the G2 Supplementary Set.
const MODE_G2_CHARACTER: u8 = 0b01111;
/// First of the sixteen Column Address group modes (`10000`-`11111`) that
/// compose a G0 character with a diacritical mark (§12.3.4).
const MODE_DIACRITICAL_BASE: u8 = 0b10000;
/// Column Address group modes that do *not* move the Active Position's
/// column, per Table 27 and §12.3.2's own statement of the exception:
/// "Apart from the one column address triplet used for PDC and those which
/// are reserved, the column co-ordinate is set by the address field of
/// column address triplets". Table 27 names `00100`/`00101`/`01010` as
/// "Reserved" and `00110` as the PDC triplet; every other column-group mode
/// value (including ones this crate does not otherwise act on, like
/// colours or DRCS) does move the column.
const COLUMN_MODES_WITHOUT_ADDRESS_EFFECT: [u8; 4] = [0b00100, 0b00101, 0b00110, 0b01010];

/// Apply one X/26 packet's thirteen triplets to `page`, updating its glyphs
/// in place.
///
/// `payload` is the packet's 40 data bytes (EN 300 706 bytes 6-45): one
/// Hamming 8/4 designation-code byte (which of up to sixteen X/26 "pages" a
/// row's enhancement data spans across, relevant only when a row needs more
/// than thirteen triplets — not tracked here, since ignoring it means at
/// worst a triplet from a rarely-used continuation is applied against the
/// wrong nominal "page", not that anything panics or over-allocates)
/// followed by thirteen Hamming 24/18 triplets, exactly [`crate::packet::
/// PACKET_LEN`] minus the two address bytes already consumed.
///
/// A triplet Hamming 24/18 decodes as [`Correction::Uncorrectable`] is
/// skipped, not guessed — the same "reject cleanly" rule
/// [`crate::decoder::validate_enhancement_packet`] documents for every
/// other enhancement packet.
pub(crate) fn apply(page: &mut Page, payload: &[u8]) {
    let Some(triplets) = payload.get(1..) else {
        return;
    };
    for chunk in triplets.chunks_exact(3) {
        let Ok(bytes) = <[u8; 3]>::try_from(chunk) else {
            break;
        };
        let (value, correction) = hamming::decode24(bytes);
        if correction == Correction::Uncorrectable {
            continue;
        }
        let address = triplet_address(value);
        let mode = triplet_mode(value);
        let data = triplet_data(value);

        if address >= 40 {
            apply_row_group(page, address, mode, data);
        } else {
            apply_column_group(page, address, mode, data);
        }
    }
}

/// §12.3.2: address 40 is row 24, addresses 41-63 are rows 1-23.
const fn row_from_address(address: u8) -> Option<usize> {
    match address {
        40 => Some(24),
        41..=63 => Some((address - 40) as usize),
        _ => None,
    }
}

fn apply_row_group(page: &mut Page, address: u8, mode: u8, data: u8) {
    match mode {
        MODE_FULL_ROW_COLOUR => {
            if let Some(row) = row_from_address(address) {
                page.x26_row = row;
                page.x26_col = 0;
            }
        }
        MODE_SET_ACTIVE_POSITION => {
            if let Some(row) = row_from_address(address) {
                page.x26_row = row;
            }
            if data < 40 {
                page.x26_col = usize::from(data);
            }
        }
        MODE_ADDRESS_ROW_0 => {
            page.x26_row = 0;
            page.x26_col = 0;
        }
        _ => {} // colours, PDC, reserved: no text-visible effect here
    }
}

fn apply_column_group(page: &mut Page, address: u8, mode: u8, data: u8) {
    // "For all mode description values except all reserved values and the
    // one used for PDC, the address field sets the column co-ordinate"
    // (§12.3.4) — every mode except the four in
    // `COLUMN_MODES_WITHOUT_ADDRESS_EFFECT` moves the column; the mode
    // itself then decides whether anything is drawn.
    if !COLUMN_MODES_WITHOUT_ADDRESS_EFFECT.contains(&mode) {
        page.x26_col = usize::from(address);
    }

    match mode {
        MODE_G2_CHARACTER => {
            if let Some(ch) = g2_char(data) {
                page.overwrite_glyph(page.x26_row, page.x26_col, Glyph::Text(ch));
            }
        }
        m if m >= MODE_DIACRITICAL_BASE => {
            let mark = m - MODE_DIACRITICAL_BASE;
            let ch = compose(page.control.national_option, mark, data);
            page.overwrite_glyph(page.x26_row, page.x26_col, Glyph::Text(ch));
        }
        _ => {} // G1/G3 graphics, colours, DRCS, font style, PDC, reserved
    }
}

/// A composed character: `base_code`'s G0 glyph (under `national_option`'s
/// sub-set, same as the Level 1 page uses) combined with the diacritical
/// mark at G2 column 4, row `mark` (`0`-`15`, EN 300 706 §12.3.4's mode
/// `10000`-`11111`, Table 37 note 2).
///
/// Mode `10000` (`mark == 0`) is the spec's own special case: "No
/// diacritical mark exists for mode description value 10000. An unmodified
/// G0 character is then displayed unless the 7 bits of the data field have
/// the value 0101010 (2/A) when the symbol \"@\" shall be displayed" — the
/// same `*`-becomes-`@` substitution Table 35 note 3 describes.
///
/// A `(base, mark)` pair this crate has not tabulated in [`diacritic`]
/// renders as the *unmodified* base character rather than a guess — a
/// visible, bounded gap (the accent is missing) rather than a wrong one.
fn compose(national_option: u8, mark: u8, base_code: u8) -> char {
    let base = crate::charset::latin_g0(base_code, national_option);
    if mark == 0 {
        return if base_code == 0x2A { '@' } else { base };
    }
    diacritic(base, mark).unwrap_or(base)
}

/// One `(base letter, mark index)` combination from G2 column 4, where a
/// precomposed Unicode scalar exists. Mark indices follow Table 37's own
/// row order in that column, cross-checked against the public ISO 6937/2
/// combining-diacritical-mark ordering the table's own note 3 cites as its
/// source: 1 grave, 2 acute, 3 circumflex, 4 tilde, 5 macron, 6 breve, 7 dot
/// above, 8 diaeresis, 9 (undefined), 10 ring above, 11 cedilla, 12
/// (undefined), 13 double acute, 14 ogonek, 15 caron.
///
/// Not exhaustive: only the Latin letters actually reachable through one of
/// this crate's eight national-option G0 sub-sets, plus the handful of
/// others common enough across Table 36's wider language set to be worth
/// composing correctly rather than leaving bare. A combination not listed
/// here falls back to the unmodified base letter (see [`compose`]).
#[allow(clippy::too_many_lines, reason = "one flat lookup table, not control flow")]
fn diacritic(base: char, mark: u8) -> Option<char> {
    Some(match (mark, base) {
        // 1: grave
        (1, 'a') => '\u{E0}',
        (1, 'e') => '\u{E8}',
        (1, 'i') => '\u{EC}',
        (1, 'o') => '\u{F2}',
        (1, 'u') => '\u{F9}',
        (1, 'A') => '\u{C0}',
        (1, 'E') => '\u{C8}',
        (1, 'I') => '\u{CC}',
        (1, 'O') => '\u{D2}',
        (1, 'U') => '\u{D9}',
        // 2: acute
        (2, 'a') => '\u{E1}',
        (2, 'e') => '\u{E9}',
        (2, 'i') => '\u{ED}',
        (2, 'o') => '\u{F3}',
        (2, 'u') => '\u{FA}',
        (2, 'y') => '\u{FD}',
        (2, 'c') => '\u{107}',
        (2, 'n') => '\u{144}',
        (2, 'r') => '\u{155}',
        (2, 's') => '\u{15B}',
        (2, 'z') => '\u{17A}',
        (2, 'l') => '\u{13A}',
        (2, 'A') => '\u{C1}',
        (2, 'E') => '\u{C9}',
        (2, 'I') => '\u{CD}',
        (2, 'O') => '\u{D3}',
        (2, 'U') => '\u{DA}',
        (2, 'Y') => '\u{DD}',
        (2, 'C') => '\u{106}',
        (2, 'N') => '\u{143}',
        (2, 'R') => '\u{154}',
        (2, 'S') => '\u{15A}',
        (2, 'Z') => '\u{179}',
        (2, 'L') => '\u{139}',
        // 3: circumflex
        (3, 'a') => '\u{E2}',
        (3, 'e') => '\u{EA}',
        (3, 'i') => '\u{EE}',
        (3, 'o') => '\u{F4}',
        (3, 'u') => '\u{FB}',
        (3, 'A') => '\u{C2}',
        (3, 'E') => '\u{CA}',
        (3, 'I') => '\u{CE}',
        (3, 'O') => '\u{D4}',
        (3, 'U') => '\u{DB}',
        // 4: tilde
        (4, 'a') => '\u{E3}',
        (4, 'e') => '\u{1EBD}',
        (4, 'i') => '\u{129}',
        (4, 'o') => '\u{F5}',
        (4, 'u') => '\u{169}',
        (4, 'n') => '\u{F1}',
        (4, 'A') => '\u{C3}',
        (4, 'O') => '\u{D5}',
        (4, 'N') => '\u{D1}',
        // 5: macron
        (5, 'a') => '\u{101}',
        (5, 'e') => '\u{113}',
        (5, 'i') => '\u{12B}',
        (5, 'o') => '\u{14D}',
        (5, 'u') => '\u{16B}',
        (5, 'A') => '\u{100}',
        (5, 'E') => '\u{112}',
        (5, 'I') => '\u{12A}',
        (5, 'O') => '\u{14C}',
        (5, 'U') => '\u{16A}',
        // 6: breve
        (6, 'a') => '\u{103}',
        (6, 'e') => '\u{115}',
        (6, 'g') => '\u{11F}',
        (6, 'i') => '\u{12D}',
        (6, 'o') => '\u{14F}',
        (6, 'u') => '\u{16D}',
        (6, 'A') => '\u{102}',
        (6, 'G') => '\u{11E}',
        // 7: dot above
        (7, 'c') => '\u{10B}',
        (7, 'e') => '\u{117}',
        (7, 'g') => '\u{121}',
        (7, 'z') => '\u{17C}',
        (7, 'C') => '\u{10A}',
        (7, 'E') => '\u{116}',
        (7, 'G') => '\u{120}',
        (7, 'Z') => '\u{17B}',
        (7, 'I') => '\u{130}',
        // 8: diaeresis
        (8, 'a') => '\u{E4}',
        (8, 'e') => '\u{EB}',
        (8, 'i') => '\u{EF}',
        (8, 'o') => '\u{F6}',
        (8, 'u') => '\u{FC}',
        (8, 'y') => '\u{FF}',
        (8, 'A') => '\u{C4}',
        (8, 'E') => '\u{CB}',
        (8, 'I') => '\u{CF}',
        (8, 'O') => '\u{D6}',
        (8, 'U') => '\u{DC}',
        // 10: ring above
        (10, 'a') => '\u{E5}',
        (10, 'u') => '\u{16F}',
        (10, 'A') => '\u{C5}',
        (10, 'U') => '\u{16E}',
        // 11: cedilla
        (11, 'c') => '\u{E7}',
        (11, 'g') => '\u{123}',
        (11, 'k') => '\u{137}',
        (11, 'l') => '\u{13C}',
        (11, 'n') => '\u{146}',
        (11, 'r') => '\u{157}',
        (11, 's') => '\u{15F}',
        (11, 't') => '\u{163}',
        (11, 'C') => '\u{C7}',
        (11, 'S') => '\u{15E}',
        // 13: double acute
        (13, 'o') => '\u{151}',
        (13, 'u') => '\u{171}',
        (13, 'O') => '\u{150}',
        (13, 'U') => '\u{170}',
        // 14: ogonek
        (14, 'a') => '\u{105}',
        (14, 'e') => '\u{119}',
        (14, 'i') => '\u{12F}',
        (14, 'u') => '\u{173}',
        (14, 'A') => '\u{104}',
        (14, 'E') => '\u{118}',
        // 15: caron
        (15, 'c') => '\u{10D}',
        (15, 'd') => '\u{10F}',
        (15, 'e') => '\u{11B}',
        (15, 'l') => '\u{13E}',
        (15, 'n') => '\u{148}',
        (15, 'r') => '\u{159}',
        (15, 's') => '\u{161}',
        (15, 't') => '\u{165}',
        (15, 'z') => '\u{17E}',
        (15, 'C') => '\u{10C}',
        (15, 'D') => '\u{10E}',
        (15, 'E') => '\u{11A}',
        (15, 'L') => '\u{13D}',
        (15, 'N') => '\u{147}',
        (15, 'R') => '\u{158}',
        (15, 'S') => '\u{160}',
        (15, 'T') => '\u{164}',
        (15, 'Z') => '\u{17D}',
        _ => return None,
    })
}

/// A code point from the Latin G2 supplementary set (Table 37), for the
/// "Character from the G2 Supplementary Set" column-group mode.
///
/// **Stated gap, not a placeholder for "coming later" without saying so**:
/// this crate has transcribed Table 37's column 4 (the diacritical marks
/// [`diacritic`] uses — the part of the table Level 1.5 depends on, per
/// §15.1) but not the other five columns, which hold a further ~80
/// symbols (ligatures, currency signs, typographic punctuation, IPA-style
/// extras) this crate has no cross-checked source for. Rather than guess,
/// the only code point resolved here is `2/0`, which Table 37 note 1 states
/// outright is SPACE — everything else leaves the Level 1 glyph already at
/// that cell in place (the column co-ordinate still moves, per
/// [`apply_column_group`]'s doc comment).
pub(crate) const fn g2_char(data: u8) -> Option<char> {
    match data {
        0x20 => Some(' '),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::page::Glyph;

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

    /// Encode one Column Address triplet (address < 40) into its Hamming
    /// 24/18-coded three bytes, from the algebraic encoder EN 300 706
    /// §8.3 defines (the same equations `hamming::decode24`'s own tests
    /// verify against, not a transcribed decision table).
    fn encode_column_triplet(address: u8, mode: u8, data: u8) -> [u8; 3] {
        let mut bits = [0u8; 18];
        for i in 0..6 {
            bits[i] = (address >> i) & 1;
        }
        for i in 0..5 {
            bits[6 + i] = (mode >> i) & 1;
        }
        for i in 0..7 {
            bits[11 + i] = (data >> i) & 1;
        }
        let get = |positions: &[usize]| -> u32 {
            positions.iter().fold(0u32, |acc, &p| acc ^ u32::from(bits[p]))
        };
        let p1 = 1 ^ get(&[0, 1, 3, 4, 6, 8, 10, 11, 13, 15, 17]);
        let p2 = 1 ^ get(&[0, 2, 3, 5, 6, 9, 10, 12, 13, 16, 17]);
        let p3 = 1 ^ get(&[1, 2, 3, 7, 8, 9, 10, 14, 15, 16, 17]);
        let p4 = 1 ^ get(&[4, 5, 6, 7, 8, 9, 10]);
        let p5 = 1 ^ get(&[11, 12, 13, 14, 15, 16, 17]);

        let mut raw = 0u32;
        raw |= p1 & 1;
        raw |= (p2 & 1) << 1;
        raw |= u32::from(bits[0]) << 2;
        raw |= (p3 & 1) << 3;
        raw |= u32::from(bits[1]) << 4;
        raw |= u32::from(bits[2]) << 5;
        raw |= u32::from(bits[3]) << 6;
        raw |= (p4 & 1) << 7;
        for (i, &d) in bits.iter().enumerate().skip(4).take(7) {
            raw |= u32::from(d) << (8 + (i - 4));
        }
        raw |= (p5 & 1) << 15;
        for (i, &d) in bits.iter().enumerate().skip(11) {
            raw |= u32::from(d) << (16 + (i - 11));
        }
        let p6 = 1 ^ (0..23).fold(0u32, |acc, n| acc ^ ((raw >> n) & 1));
        raw |= (p6 & 1) << 23;
        [(raw & 0xFF) as u8, ((raw >> 8) & 0xFF) as u8, ((raw >> 16) & 0xFF) as u8]
    }

    fn encode_row_triplet(address: u8, mode: u8, data: u8) -> [u8; 3] {
        // Row and Column triplets share one 6+5+7 layout; only the address
        // range (>= 40) distinguishes them, so the same encoder works.
        encode_column_triplet(address, mode, data)
    }

    fn packet_with(triplets: &[[u8; 3]]) -> Vec<u8> {
        let mut payload = vec![hamming_byte(0)]; // designation code, unused
        for t in triplets {
            payload.extend_from_slice(t);
        }
        while payload.len() < 40 {
            payload.push(hamming_byte(0));
        }
        payload
    }

    #[test]
    fn diacritical_mode_composes_an_acute_e() {
        let mut page = Page::blank_for_test(1);
        // Set Active Position: row 5, column 3.
        let set_pos = encode_row_triplet(41 + 4, 0b00100, 3); // address 45 -> row 5
        // Column 3, mode 10010 (diacritical index 2 = acute), data = 'e' (0x65).
        let compose_e = encode_column_triplet(3, 0b10010, b'e');
        let payload = packet_with(&[set_pos, compose_e]);
        apply(&mut page, &payload);
        assert_eq!(page.rows[5][3].glyph, Glyph::Text('\u{E9}')); // é
    }

    #[test]
    fn mode_10000_with_data_2a_displays_at_sign() {
        let mut page = Page::blank_for_test(2);
        let set_pos = encode_row_triplet(40, 0b00100, 0); // row 24, col 0
        let at_sign = encode_column_triplet(0, 0b10000, 0x2A);
        let payload = packet_with(&[set_pos, at_sign]);
        apply(&mut page, &payload);
        assert_eq!(page.rows[24][0].glyph, Glyph::Text('@'));
    }

    #[test]
    fn mode_10000_otherwise_displays_the_plain_g0_character() {
        let mut page = Page::blank_for_test(1);
        let plain_h = encode_column_triplet(7, 0b10000, b'H');
        let payload = packet_with(&[plain_h]);
        apply(&mut page, &payload);
        // Active Position starts at row 0 (page default), column moves with
        // the triplet's own address per §12.3.4.
        assert_eq!(page.rows[0][7].glyph, Glyph::Text('H'));
    }

    #[test]
    fn an_uncorrectable_triplet_is_skipped_not_misapplied() {
        let mut page = Page::blank_for_test(1);
        let mut garbage = encode_column_triplet(5, 0b10010, b'a');
        garbage[0] ^= 0b0000_0011; // flip two bits: uncorrectable
        let payload = packet_with(&[garbage]);
        apply(&mut page, &payload);
        assert_eq!(page.rows[0][5].glyph, Glyph::Text(' '));
    }

    #[test]
    fn active_position_persists_across_triplets_in_one_packet() {
        let mut page = Page::blank_for_test(1);
        let set_pos = encode_row_triplet(41, 0b00100, 10); // row 1, col 10
        let first = encode_column_triplet(10, 0b10010, b'a'); // column moves to 10 anyway
        let payload = packet_with(&[set_pos, first]);
        apply(&mut page, &payload);
        assert_eq!(page.rows[1][10].glyph, Glyph::Text('\u{E1}')); // á
    }

    #[test]
    fn address_display_row_0_resets_active_position() {
        let mut page = Page::blank_for_test(1);
        page.x26_row = 10;
        page.x26_col = 20;
        let reset = encode_row_triplet(63, 0b00111, 0);
        let payload = packet_with(&[reset]);
        apply(&mut page, &payload);
        assert_eq!((page.x26_row, page.x26_col), (0, 0));
    }
}
