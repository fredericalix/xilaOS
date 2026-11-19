//! The line being typed at the prompt, with its history.

use crate::text::FixedStr;

/// Longest line. Prompt and line fit in one row of the grid.
pub const LINE_MAX: usize = 160;
const HISTORY: usize = 16;

pub type Line = FixedStr<LINE_MAX>;

pub struct LineEditor {
    buf: Line,
    cursor: usize,
    history: [Line; HISTORY],
    history_len: usize,
    /// Which history entry is shown: 0 is the most recent, None the line being typed.
    recalled: Option<usize>,
}

impl LineEditor {
    pub const fn new() -> LineEditor {
        LineEditor { buf: Line::new(), cursor: 0, history: [Line::new(); HISTORY], history_len: 0, recalled: None }
    }

    pub fn bytes(&self) -> &[u8] {
        self.buf.bytes()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn insert(&mut self, ch: u8) {
        if self.buf.insert(self.cursor, ch) {
            self.cursor += 1;
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buf.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        self.buf.remove(self.cursor);
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.buf.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.buf.len();
    }

    /// Replaces the line and puts the cursor at its end.
    pub fn set(&mut self, bytes: &[u8]) {
        self.buf = Line::from_bytes(bytes);
        self.cursor = self.buf.len();
    }

    /// Returns the line, remembers it, and starts an empty one.
    pub fn take(&mut self) -> Line {
        let line = self.buf;
        let repeated = self.history_len > 0 && self.history[0].bytes() == line.bytes();
        if !line.is_empty() && !repeated {
            self.history.copy_within(0..HISTORY - 1, 1);
            self.history[0] = line;
            self.history_len = (self.history_len + 1).min(HISTORY);
        }
        self.set(b"");
        self.recalled = None;
        line
    }

    /// Shows the previous line of the history.
    pub fn older(&mut self) {
        let next = self.recalled.map_or(0, |i| i + 1);
        if next < self.history_len {
            self.recalled = Some(next);
            let line = self.history[next];
            self.set(line.bytes());
        }
    }

    /// Shows the next line of the history, then an empty line.
    pub fn newer(&mut self) {
        match self.recalled {
            Some(0) | None => {
                self.recalled = None;
                self.set(b"");
            }
            Some(i) => {
                self.recalled = Some(i - 1);
                let line = self.history[i - 1];
                self.set(line.bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(ed: &mut LineEditor, text: &[u8]) {
        for &c in text {
            ed.insert(c);
        }
    }

    #[test]
    fn editing_in_the_middle() {
        let mut ed = LineEditor::new();
        typed(&mut ed, b"hlp");
        ed.left();
        ed.left();
        ed.insert(b'e');
        assert_eq!(ed.bytes(), b"help");
        ed.end();
        ed.backspace();
        ed.home();
        ed.delete();
        assert_eq!(ed.bytes(), b"el");
    }

    #[test]
    fn history_goes_back_and_forth() {
        let mut ed = LineEditor::new();
        typed(&mut ed, b"one");
        assert_eq!(ed.take().bytes(), b"one");
        typed(&mut ed, b"two");
        ed.take();
        typed(&mut ed, b"two");
        ed.take(); // repeated: stored once
        ed.older();
        assert_eq!(ed.bytes(), b"two");
        ed.older();
        assert_eq!(ed.bytes(), b"one");
        ed.older();
        assert_eq!(ed.bytes(), b"one");
        ed.newer();
        assert_eq!(ed.bytes(), b"two");
        ed.newer();
        assert_eq!(ed.bytes(), b"");
    }

    #[test]
    fn empty_lines_are_not_remembered() {
        let mut ed = LineEditor::new();
        ed.take();
        ed.older();
        assert_eq!(ed.bytes(), b"");
    }
}
