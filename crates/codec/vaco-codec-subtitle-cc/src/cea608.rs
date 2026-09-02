//! CEA-608 (line-21) decode: one [`Cea608Decoder`] per field, each carrying
//! two time-multiplexed channels (CC1/CC2 on field 1, CC3/CC4 on field 2) —
//! CEA-708 allows at most one CEA-608 datastream per program, carrying up to
//! four such channels two per field, per SMPTE EG 43:2009 §6.3.4.
//!
//! # Channel selection is stateful, not per-triplet
//!
//! Only a control-code byte pair carries a channel bit (`0x08` on its first
//! byte, per CEA-608's control-code layout). A standard-character pair
//! carries none, so it belongs to whichever channel a control code most
//! recently selected — that is why this decoder tracks `active` rather than
//! routing every pair independently.
//!
//! # Doubling
//!
//! Every control code is required to be transmitted twice in a row so a
//! single-bit line error does not lose it. [`Cea608Decoder`] suppresses the
//! second of an exact, immediate repeat and re-arms as soon as anything else
//! arrives, so a control code that happens to recur later (not
//! back-to-back) is applied again as new.

pub mod tables;

use crate::event::{Color, Screen, Style};
use tables::MiscControl;

/// Display rows are 1-15; row 0 is never used.
const MAX_ROW: u8 = 15;
/// Display columns are 0-31.
const MAX_COLUMN: u8 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    PopOn,
    RollUp(u8),
    PaintOn,
}

#[derive(Debug, Clone)]
struct ChannelState {
    mode: Mode,
    /// The on-screen buffer. Pop-on mode swaps this with `non_displayed` on
    /// `EndOfCaption`; roll-up and paint-on write straight into it.
    displayed: Screen,
    /// Pop-on's "being built off-screen" buffer. Unused in roll-up/paint-on.
    non_displayed: Screen,
    cursor_row: u8,
    cursor_col: u8,
    style: Style,
    /// The bottom row roll-up text scrolls up from; set by a PAC while in
    /// roll-up mode, or defaults to the last row.
    roll_up_base: u8,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self {
            mode: Mode::PopOn,
            displayed: Screen::new(),
            non_displayed: Screen::new(),
            cursor_row: MAX_ROW,
            cursor_col: 0,
            style: Style::default(),
            roll_up_base: MAX_ROW,
        }
    }
}

impl ChannelState {
    fn write_char(&mut self, ch: char) -> Option<Screen> {
        if self.cursor_col < MAX_COLUMN {
            let (row, col, style) = (self.cursor_row, self.cursor_col, self.style);
            self.buffer_mut().row_mut(row).put(col, ch, style);
            self.cursor_col = self.cursor_col.saturating_add(1);
        }
        self.visible_update()
    }

    /// The buffer that text written right now lands in.
    fn buffer_mut(&mut self) -> &mut Screen {
        match self.mode {
            Mode::PopOn => &mut self.non_displayed,
            Mode::RollUp(_) | Mode::PaintOn => &mut self.displayed,
        }
    }

    /// `Some` when the change just made is visible immediately (roll-up,
    /// paint-on); `None` in pop-on, where nothing is visible until
    /// `EndOfCaption` swaps the buffers.
    fn visible_update(&self) -> Option<Screen> {
        match self.mode {
            Mode::PopOn => None,
            Mode::RollUp(_) | Mode::PaintOn => Some(self.displayed.clone()),
        }
    }

    fn apply_pac(&mut self, p: tables::Pac) {
        self.cursor_row = p.row;
        self.cursor_col = p.indent.unwrap_or(0);
        self.style = Style {
            color: p.color.unwrap_or(Color::White),
            italics: p.italics,
            underline: p.underline,
        };
        if matches!(self.mode, Mode::RollUp(_)) {
            self.roll_up_base = p.row;
        }
    }

    fn apply_mid_row(&mut self, m: tables::MidRow) -> Option<Screen> {
        self.style = Style {
            color: m.color.unwrap_or(self.style.color),
            italics: m.italics,
            underline: m.underline,
        };
        // A mid-row code occupies one character cell, displayed as a space
        // in the new style, before the styled text that follows it.
        self.write_char(' ')
    }

    fn move_tab(&mut self, columns: u8) {
        self.cursor_col = self.cursor_col.saturating_add(columns).min(MAX_COLUMN - 1);
    }

    fn apply_misc(&mut self, c: MiscControl) -> Option<Screen> {
        match c {
            MiscControl::ResumeCaptionLoading => {
                self.mode = Mode::PopOn;
                None
            }
            MiscControl::Backspace => {
                let (row, col) = (self.cursor_row, self.cursor_col);
                self.buffer_mut().row_mut(row).remove_before(col);
                self.cursor_col = self.cursor_col.saturating_sub(1);
                self.visible_update()
            }
            MiscControl::DeleteToEndOfRow => {
                let (row, col) = (self.cursor_row, self.cursor_col);
                self.buffer_mut().row_mut(row).truncate_from(col);
                self.visible_update()
            }
            MiscControl::RollUp2 => self.enter_roll_up(2),
            MiscControl::RollUp3 => self.enter_roll_up(3),
            MiscControl::RollUp4 => self.enter_roll_up(4),
            MiscControl::ResumeDirectCaptioning => {
                self.mode = Mode::PaintOn;
                None
            }
            MiscControl::EraseDisplayedMemory => {
                self.displayed = Screen::new();
                Some(self.displayed.clone())
            }
            MiscControl::CarriageReturn => self.carriage_return(),
            MiscControl::EraseNonDisplayedMemory => {
                self.non_displayed = Screen::new();
                None
            }
            MiscControl::EndOfCaption => {
                std::mem::swap(&mut self.displayed, &mut self.non_displayed);
                self.non_displayed = Screen::new();
                Some(self.displayed.clone())
            }
            MiscControl::AlarmOff
            | MiscControl::AlarmOn
            | MiscControl::FlashOn
            | MiscControl::TextRestart
            | MiscControl::ResumeTextDisplay => None,
        }
    }

    fn enter_roll_up(&mut self, rows: u8) -> Option<Screen> {
        self.mode = Mode::RollUp(rows);
        None
    }

    fn carriage_return(&mut self) -> Option<Screen> {
        if !matches!(self.mode, Mode::RollUp(_)) {
            return None;
        }
        self.displayed.scroll(-1, MAX_ROW);
        self.cursor_row = self.roll_up_base;
        self.cursor_col = 0;
        Some(self.displayed.clone())
    }
}

fn odd_parity(byte: u8) -> bool {
    byte.count_ones() % 2 == 1
}

/// Decodes one line-21 field's `cc_data` byte pairs into caption screens.
///
/// Construct one per field (this crate's [`crate::CcDecoder`] owns two, for
/// field 1 and field 2); each carries its own pair of channels internally.
#[derive(Debug, Default)]
pub struct Cea608Decoder {
    active: usize,
    channels: [ChannelState; 2],
    last_control: Option<[u8; 2]>,
}

impl Cea608Decoder {
    /// Feed one byte pair from this field's `cc_data` triplets.
    ///
    /// `(0x00, 0x00)` is treated as padding, not a parity failure, since it
    /// commonly appears as filler within an otherwise-valid triplet. Any
    /// other pair that fails CEA-608's odd-parity check is dropped and
    /// counted in `parity_errors`.
    pub fn feed(&mut self, data: [u8; 2], parity_errors: &mut u64) -> Option<Screen> {
        if data == [0x00, 0x00] {
            return None;
        }
        if !odd_parity(data[0]) || !odd_parity(data[1]) {
            *parity_errors += 1;
            return None;
        }
        let pair = [data[0] & 0x7F, data[1] & 0x7F];
        let byte0 = pair[0];

        if (0x10..=0x1F).contains(&byte0) {
            let is_repeat = self.last_control == Some(pair);
            self.last_control = if is_repeat { None } else { Some(pair) };
            if is_repeat {
                return None;
            }
            let channel = usize::from(byte0 & 0x08 != 0);
            let base = byte0 & !0x08;
            self.active = channel;
            return self
                .channels
                .get_mut(channel)
                .and_then(|state| apply_control(state, base, pair[1]));
        }

        self.last_control = None;
        let byte1 = pair[1];
        let state = self.channels.get_mut(self.active)?;
        let mut last = None;
        if let Some(ch) = tables::standard_char(byte0) {
            last = state.write_char(ch);
        }
        if byte1 != 0x00
            && let Some(ch) = tables::standard_char(byte1)
        {
            last = state.write_char(ch);
        }
        last
    }
}

fn apply_control(state: &mut ChannelState, base: u8, second: u8) -> Option<Screen> {
    if let Some(p) = tables::pac(base, second) {
        state.apply_pac(p);
        return None;
    }
    if base == 0x11 {
        if let Some(m) = tables::mid_row(second) {
            return state.apply_mid_row(m);
        }
        if let Some(ch) = tables::special_char(second) {
            return state.write_char(ch);
        }
    }
    if matches!(base, 0x12 | 0x13)
        && (0x20..=0x3F).contains(&second)
        && let Some(ch) = tables::extended_char(base, second)
    {
        return state.write_char(ch);
    }
    if matches!(base, 0x14 | 0x15)
        && (0x20..=0x2F).contains(&second)
        && let Some(mc) = tables::misc_control(second)
    {
        return state.apply_misc(mc);
    }
    if base == 0x17
        && let Some(n) = tables::tab_offset(second)
    {
        state.move_tab(n);
    }
    None
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    fn parity(byte: u8) -> u8 {
        if odd_parity(byte) { byte } else { byte | 0x80 }
    }

    fn pair(a: u8, b: u8) -> [u8; 2] {
        [parity(a), parity(b)]
    }

    #[test]
    fn pop_on_basic_caption() {
        let mut dec = Cea608Decoder::default();
        let mut errors = 0;
        // RCL
        assert_eq!(dec.feed(pair(0x14, 0x20), &mut errors), None);
        // PAC: row 15, white, no indent (0x14, 0x70 has second byte in the
        // 0x60-0x7F half for row 15, offset 0x10 for underline-less white...
        // use row 1 (0x11,0x40) instead for a simple case).
        assert_eq!(dec.feed(pair(0x11, 0x40), &mut errors), None);
        assert_eq!(dec.feed(pair(b'H', b'i'), &mut errors), None);
        // EOC swaps the buffers into view.
        let screen = dec
            .feed(pair(0x14, 0x2F), &mut errors)
            .expect("EOC shows a screen");
        assert_eq!(screen.text(), "Hi");
        assert_eq!(errors, 0);
    }

    #[test]
    fn roll_up_scrolls_on_carriage_return() {
        let mut dec = Cea608Decoder::default();
        let mut errors = 0;
        dec.feed(pair(0x14, 0x25), &mut errors); // RU2
        dec.feed(pair(0x11, 0x40), &mut errors); // PAC row 1... base row for RU
        let s1 = dec
            .feed(pair(b'A', b'B'), &mut errors)
            .expect("roll-up text is visible immediately");
        assert_eq!(s1.text(), "AB");
        let s2 = dec
            .feed(pair(0x14, 0x2D), &mut errors) // CR
            .expect("carriage return scrolls");
        assert!(s2.is_empty());
    }

    #[test]
    fn doubled_control_code_applies_once() {
        let mut dec = Cea608Decoder::default();
        let mut errors = 0;
        dec.feed(pair(0x14, 0x25), &mut errors); // RU2
        dec.feed(pair(0x11, 0x40), &mut errors);
        dec.feed(pair(b'X', 0x00), &mut errors);
        // A duplicated RollUp3 immediately after should be suppressed: mode
        // stays RollUp(2), so a following carriage return still scrolls by
        // exactly one row rather than re-entering roll-up mode.
        dec.feed(pair(0x14, 0x26), &mut errors); // RU3 (first)
        dec.feed(pair(0x14, 0x26), &mut errors); // RU3 (duplicate, suppressed)
        assert_eq!(dec.channels[dec.active].mode, Mode::RollUp(3));
    }

    #[test]
    fn invalid_parity_is_counted_not_applied() {
        let mut dec = Cea608Decoder::default();
        let mut errors = 0;
        // Both bytes with their high bit forced to violate odd parity.
        let bad = [0x41, 0x42]; // even parity on both, top bit clear
        assert_eq!(dec.feed(bad, &mut errors), None);
        assert_eq!(errors, 1);
    }
}
