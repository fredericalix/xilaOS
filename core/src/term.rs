//! A scrolling text area with a fixed amount of history.

use crate::grid::{Cell, COLS};

/// Rows kept. The oldest are dropped when it is full.
pub const TERM_ROWS: usize = 256;

pub struct Term {
    rows: [[Cell; COLS]; TERM_ROWS],
    /// Index of the oldest row in `rows`.
    head: usize,
    len: usize,
    /// Next free column in the newest row.
    col: usize,
    /// Rows scrolled back; 0 shows the newest rows.
    pub scroll: usize,
    /// Background role of this terminal.
    pub bg: u8,
}

impl Term {
    pub const fn new(bg: u8) -> Term {
        Term { rows: [[Cell::BLANK; COLS]; TERM_ROWS], head: 0, len: 0, col: 0, scroll: 0, bg }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.col = 0;
        self.scroll = 0;
    }

    /// The `i`th row, 0 being the oldest.
    pub fn row(&self, i: usize) -> &[Cell; COLS] {
        &self.rows[(self.head + i) % TERM_ROWS]
    }

    /// Starts an empty row.
    pub fn new_row(&mut self) {
        if self.len == TERM_ROWS {
            self.head = (self.head + 1) % TERM_ROWS;
        } else {
            self.len += 1;
        }
        let blank = Cell::new(b' ', 0, self.bg);
        let at = (self.head + self.len - 1) % TERM_ROWS;
        self.rows[at] = [blank; COLS];
        self.col = 0;
    }

    /// Appends text to the newest row, continuing on new rows when it is full.
    pub fn put(&mut self, text: &[u8], fg: u8) {
        if self.len == 0 {
            self.new_row();
        }
        for &ch in text {
            if ch == b'\n' {
                self.new_row();
                continue;
            }
            if self.col == COLS {
                self.new_row();
            }
            let at = (self.head + self.len - 1) % TERM_ROWS;
            self.rows[at][self.col] = Cell::new(ch, fg, self.bg);
            self.col += 1;
        }
    }

    /// Writes text on a row of its own.
    pub fn line(&mut self, text: &[u8], fg: u8) {
        self.new_row();
        self.put(text, fg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &Term, i: usize) -> String {
        t.row(i).iter().map(|c| c.ch as char).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn lines_and_wrapping() {
        let mut t = Term::new(6);
        t.line(b"first", 7);
        t.line(&[b'x'; COLS + 3], 7);
        assert_eq!(t.len(), 3);
        assert_eq!(text(&t, 0), "first");
        assert_eq!(text(&t, 2), "xxx");
        assert_eq!(t.row(0)[0].bg, 6);
    }

    #[test]
    fn oldest_rows_are_dropped() {
        let mut t = Term::new(0);
        for i in 0..TERM_ROWS + 5 {
            t.line(format!("row {i}").as_bytes(), 1);
        }
        assert_eq!(t.len(), TERM_ROWS);
        assert_eq!(text(&t, 0), "row 5");
        assert_eq!(text(&t, TERM_ROWS - 1), format!("row {}", TERM_ROWS + 4));
    }
}
