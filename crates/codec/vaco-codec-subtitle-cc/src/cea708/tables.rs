//! CEA-708 code-space length table, `G0`/`G1` character sets, and the
//! window/pen command argument layouts.

use crate::event::Color;

/// The byte length (including the opcode byte itself) of the code starting
/// at `bytes[0]`, per ANSI/CTA-708's code-space table: `C0` (`0x00-0x1F`,
/// with `0x10` escaping into the extended `C2`/`C3`/`G2`/`G3` space),
/// printable `G0` (`0x20-0x7F`), `C1` window/pen commands (`0x80-0x9F`, each
/// with its own fixed argument count), and `G1` (`0xA0-0xFF`).
///
/// Never returns 0, so a caller advancing by this amount always makes
/// progress even on a code this crate does not otherwise interpret.
#[must_use]
pub fn code_len(bytes: &[u8]) -> usize {
    let Some(&b0) = bytes.first() else { return 1 };
    match b0 {
        0x00..=0x0F | 0x20..=0x7F | 0x80..=0x87 | 0x8E | 0x8F | 0x93..=0x96 | 0xA0..=0xFF => 1,
        0x10 => 1 + ext1_len(bytes.get(1..).unwrap_or(&[])),
        0x11..=0x17 | 0x88..=0x8C | 0x8D => 2,
        0x18..=0x1F | 0x90 | 0x92 => 3,
        0x91 => 4,
        0x97 => 5,
        0x98..=0x9F => 7,
    }
}

fn ext1_len(bytes: &[u8]) -> usize {
    let Some(&b0) = bytes.first() else { return 1 };
    match b0 {
        0x00..=0x07 | 0x20..=0x7F | 0xA0..=0xFF => 1,
        0x08..=0x0F => 2,
        0x10..=0x17 => 3,
        0x18..=0x1F => 4,
        0x80..=0x87 => 5,
        0x88..=0x8F => 6,
        0x90..=0x9F => usize::from(bytes.get(1).map_or(0, |b| b & 0x3F)) + 1,
    }
}

/// Decode a `G0` (standard) character. Identical to ASCII except `0x7F`,
/// which CEA-708 redefines as a musical eighth note rather than ASCII DEL.
#[must_use]
pub fn decode_g0(byte: u8) -> Option<char> {
    match byte {
        0x7F => Some('\u{266A}'),
        0x20..=0x7E => Some(char::from(byte)),
        _ => None,
    }
}

/// Decode a `G1` character: CEA-708's `0xA0-0xFF` range is ISO-8859-1
/// (Latin-1), so the byte value is already the Unicode code point.
#[must_use]
pub fn decode_g1(byte: u8) -> Option<char> {
    if byte < 0xA0 {
        return None;
    }
    char::from_u32(u32::from(byte))
}

/// The geometry fields of a `DefineWindow` (`0x98-0x9F`) command's 6
/// argument bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    /// 0-8: which corner/edge/center of the window the anchor point sits at.
    pub anchor_point: u8,
    /// When set, `anchor_vertical`/`anchor_horizontal` are percentages of
    /// the screen rather than absolute row/column-like units.
    pub relative: bool,
    pub anchor_vertical: u8,
    pub anchor_horizontal: u8,
    /// 0-15.
    pub row_count: u8,
    /// 0-63.
    pub column_count: u8,
    pub visible: bool,
}

/// Decode `DefineWindow`'s 6 argument bytes (the opcode's low 3 bits are the
/// window ID and are read by the caller, not here).
#[must_use]
pub fn define_window(args: [u8; 6]) -> WindowGeometry {
    let [byte0, byte1, byte2, byte3, byte4, _pen_style] = args;
    WindowGeometry {
        visible: (byte0 & 0x20) != 0,
        relative: (byte1 & 0x80) != 0,
        anchor_vertical: byte1 & 0x7F,
        anchor_horizontal: byte2,
        anchor_point: (byte3 & 0xF0) >> 4,
        row_count: byte3 & 0x0F,
        column_count: byte4 & 0x3F,
    }
}

/// `SetPenAttributes`' two argument bytes, reduced to the two flags this
/// crate models: italics and underline. Pen size, font style, text tag and
/// offset are parsed-and-discarded (a documented scope reduction, not a
/// parsing gap: consuming both bytes keeps the service block's byte offset
/// correct for what follows).
#[must_use]
pub const fn pen_attributes(args: [u8; 2]) -> (bool, bool) {
    let [_size_font_tag, style] = args;
    let italics = (style & 0x80) != 0;
    let underline = (style & 0x40) != 0;
    (italics, underline)
}

/// `SetPenColor`'s foreground color, from its first argument byte. The
/// other two bytes (background and edge color) are consumed by the caller
/// for byte-offset purposes but not modeled.
///
/// CEA-708 packs color as 2 bits each of red/green/blue in the low 6 bits;
/// this reduces that 64-value space to the nearest of this crate's 8 named
/// [`Color`]s by treating each channel as present when either of its two
/// bits is set, which is exact for the fully-saturated colors a caption
/// author actually picks and approximate for intermediate levels.
#[must_use]
pub const fn pen_color(foreground_byte: u8) -> Color {
    let r = foreground_byte & 0x30 != 0;
    let g = foreground_byte & 0x0C != 0;
    let b = foreground_byte & 0x03 != 0;
    match (r, g, b) {
        (true, true, true) => Color::White,
        (false, false, false) => Color::Black,
        (true, false, false) => Color::Red,
        (false, true, false) => Color::Green,
        (false, false, true) => Color::Blue,
        (true, true, false) => Color::Yellow,
        (false, true, true) => Color::Cyan,
        (true, false, true) => Color::Magenta,
    }
}

/// `SetPenLocation`'s two argument bytes: (row, column).
#[must_use]
pub const fn pen_location(args: [u8; 2]) -> (u8, u8) {
    let [row, col] = args;
    (row & 0x0F, col & 0x3F)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_len_matches_known_opcodes() {
        assert_eq!(code_len(&[0x41]), 1); // G0 'A'
        assert_eq!(code_len(&[0xA0]), 1); // G1 NBSP
        assert_eq!(code_len(&[0x90, 0, 0]), 3); // SetPenAttributes
        assert_eq!(code_len(&[0x91, 0, 0, 0]), 4); // SetPenColor
        assert_eq!(code_len(&[0x92, 0, 0]), 3); // SetPenLocation
        assert_eq!(code_len(&[0x97, 0, 0, 0, 0]), 5); // SetWindowAttributes
        assert_eq!(code_len(&[0x98, 0, 0, 0, 0, 0, 0]), 7); // DefineWindow
        assert_eq!(code_len(&[0x0D]), 1); // CR
    }

    #[test]
    fn ext1_p16_length_from_second_byte() {
        // 0x10 (EXT1) followed by 0x90..=0x9F carries its own length in the
        // low 6 bits of the byte after that.
        assert_eq!(code_len(&[0x10, 0x90, 0x05]), 1 + 6);
    }

    #[test]
    fn g0_musical_note_deviation() {
        assert_eq!(decode_g0(0x7F), Some('\u{266A}'));
        assert_eq!(decode_g0(0x41), Some('A'));
    }

    #[test]
    fn define_window_geometry_bits() {
        // visible=1 (0x20), relative=1 anchor_vertical=50 (0xB2 = 0x80|50),
        // anchor_horizontal=99, anchor_point=3 row_count=5 (0x35),
        // column_count=32 (0x20).
        let geometry = define_window([0x20, 0xB2, 99, 0x35, 0x20, 0]);
        assert!(geometry.visible);
        assert!(geometry.relative);
        assert_eq!(geometry.anchor_vertical, 50);
        assert_eq!(geometry.anchor_horizontal, 99);
        assert_eq!(geometry.anchor_point, 3);
        assert_eq!(geometry.row_count, 5);
        assert_eq!(geometry.column_count, 32);
    }
}
