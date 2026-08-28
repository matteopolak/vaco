//! Render a decoded [`Screen`] as SRT-like text, for fixture verification.
//!
//! This is not a muxer: it has no timestamps of its own (this crate has none
//! to give it — see the crate-level doc's timing note) and produces only the
//! cue-text portion of an SRT block, one line per on-screen row, in row
//! order. It exists so a test can compare a decoded caption against a
//! plain-text expectation without reaching into [`Screen`]'s structure.

use crate::event::Screen;

/// Render every non-empty row of `screen`, top to bottom, one line each,
/// joined with `\n` (no trailing newline).
#[must_use]
pub fn render(screen: &Screen) -> String {
    screen.text()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Cell, Row, Style};

    #[test]
    fn renders_rows_in_order() {
        let mut screen = Screen::new();
        screen.rows.push(Row {
            row: 2,
            cells: vec![Cell {
                column: 0,
                ch: 'b',
                style: Style::default(),
            }],
        });
        screen.rows.push(Row {
            row: 1,
            cells: vec![Cell {
                column: 0,
                ch: 'a',
                style: Style::default(),
            }],
        });
        // Screen::rows is documented row-sorted; render() trusts that rather
        // than re-sorting, so this fixture keeps them in order too.
        screen.rows.sort_by_key(|r| r.row);
        assert_eq!(render(&screen), "a\nb");
    }
}
