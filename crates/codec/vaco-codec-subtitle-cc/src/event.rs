//! Shared output types: a decoded screen is a sparse set of styled cells.
//!
//! A fixed 2D grid (15 rows x 32 columns for CEA-608, up to 16 rows x 42
//! columns per CEA-708 window) would need indexed access everywhere, which
//! this workspace's `indexing_slicing = "deny"` lint forbids outside tests.
//! A sparse list of cells, found and replaced by linear scan, sidesteps that
//! entirely and is not a performance concern at these sizes (at most a few
//! hundred cells).

/// The eight colors CEA-608 and CEA-708's `G0`/pen commands can select.
///
/// CEA-608 has no black foreground option (mid-row and PAC codes only ever
/// select one of these seven plus white), so [`Color::Black`] only ever
/// appears from CEA-708 pen/window color commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// The default foreground color for both formats.
    #[default]
    White,
    /// CEA-708 background/edge color; never a CEA-608 foreground.
    Black,
    Green,
    Blue,
    Cyan,
    Red,
    Yellow,
    Magenta,
}

/// Character-level styling in effect when a [`Cell`] was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// Foreground color.
    pub color: Color,
    /// Set by a mid-row/PAC "white italics" code or a CEA-708 pen attribute.
    pub italics: bool,
    /// Set by the low bit of a mid-row or PAC code, or a CEA-708 pen
    /// attribute.
    pub underline: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: Color::White,
            italics: false,
            underline: false,
        }
    }
}

/// One character at a column within a [`Row`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// 0-based column.
    pub column: u8,
    /// The decoded character.
    pub ch: char,
    /// Styling in effect when this character was written.
    pub style: Style,
}

/// One row of a [`Screen`], as a sparse, column-sorted list of cells.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    /// 1-based row number (CEA-608: 1-15; CEA-708: 1 plus the window's row
    /// count).
    pub row: u8,
    /// Cells present in this row, sorted by [`Cell::column`] with no two
    /// cells sharing a column.
    pub cells: Vec<Cell>,
}

impl Row {
    fn new(row: u8) -> Self {
        Self {
            row,
            cells: Vec::new(),
        }
    }

    /// Insert or replace the cell at `column`, keeping cells column-sorted.
    pub fn put(&mut self, column: u8, ch: char, style: Style) {
        self.cells.retain(|c| c.column != column);
        let pos = self
            .cells
            .iter()
            .position(|c| c.column > column)
            .unwrap_or(self.cells.len());
        self.cells.insert(pos, Cell { column, ch, style });
    }

    /// Remove every cell at or after `column` (CEA-608 "delete to end of
    /// row").
    pub fn truncate_from(&mut self, column: u8) {
        self.cells.retain(|c| c.column < column);
    }

    /// Remove the last cell strictly before `column`, if any (backspace).
    pub fn remove_before(&mut self, column: u8) {
        if let Some(pos) = self.cells.iter().rposition(|c| c.column < column) {
            self.cells.remove(pos);
        }
    }

    /// This row's text, columns rendered as single spaces between cells.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        let mut last_col: Option<u8> = None;
        for cell in &self.cells {
            if let Some(last) = last_col {
                for _ in 0..cell.column.saturating_sub(last).saturating_sub(1) {
                    out.push(' ');
                }
            }
            out.push(cell.ch);
            last_col = Some(cell.column);
        }
        out
    }
}

/// A decoded, displayable screen: a sparse, row-sorted set of [`Row`]s.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Screen {
    /// Rows present on screen, sorted by [`Row::row`] with no two rows
    /// sharing a row number.
    pub rows: Vec<Row>,
}

impl Screen {
    /// An empty screen (no rows).
    #[must_use]
    pub const fn new() -> Self {
        Self { rows: Vec::new() }
    }

    /// Whether this screen has no visible text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(|r| r.cells.is_empty())
    }

    /// The row-mutable reference at `row`, inserting an empty one in
    /// row-sorted position if it does not exist yet.
    pub fn row_mut(&mut self, row: u8) -> &mut Row {
        if self.rows.iter().all(|r| r.row != row) {
            let pos = self
                .rows
                .iter()
                .position(|r| r.row > row)
                .unwrap_or(self.rows.len());
            self.rows.insert(pos, Row::new(row));
        }
        self.rows
            .iter_mut()
            .find(|r| r.row == row)
            .unwrap_or_else(|| {
                unreachable!("row was just found present, or inserted immediately above")
            })
    }

    /// Remove `row` entirely, if present.
    pub fn remove_row(&mut self, row: u8) {
        self.rows.retain(|r| r.row != row);
    }

    /// Shift every row's number by `delta` (negative scrolls up), dropping
    /// any row that lands outside `1..=max_row`.
    pub fn scroll(&mut self, delta: i16, max_row: u8) {
        for row in &mut self.rows {
            let shifted = i16::from(row.row) + delta;
            row.row = u8::try_from(shifted.clamp(0, i16::from(max_row))).unwrap_or(0);
        }
        self.rows.retain(|r| r.row >= 1);
    }

    /// Render every row's text, top to bottom, one line each.
    #[must_use]
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn put_keeps_columns_sorted() {
        let mut row = Row::new(1);
        row.put(5, 'b', Style::default());
        row.put(0, 'a', Style::default());
        row.put(2, 'x', Style::default());
        row.put(2, 'c', Style::default()); // replaces the 'x'
        let cols: Vec<u8> = row.cells.iter().map(|c| c.column).collect();
        assert_eq!(cols, [0, 2, 5]);
        assert_eq!(row.text(), "a c  b");
    }

    #[test]
    fn scroll_drops_rows_off_top() {
        let mut screen = Screen::new();
        screen.row_mut(1).put(0, 'a', Style::default());
        screen.row_mut(2).put(0, 'b', Style::default());
        screen.scroll(-1, 15);
        assert_eq!(screen.rows.len(), 1);
        assert_eq!(screen.rows[0].row, 1);
        assert_eq!(screen.rows[0].text(), "b");
    }
}
