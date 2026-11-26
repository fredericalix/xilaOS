//! The interface: header, title bar, terminal, menu.
//!
//! The layout follows the Trame 0.3 mockup, drawn with the characters of
//! V2_OS. `render` rewrites the whole grid from the state; the grid notices
//! which cells really changed, so only those reach the screen.

use core::fmt::Write;

use crate::grid::{fx, Cell, Grid, COLS};
use crate::keys::{Key, KeyEvent};
use crate::line::LineEditor;
use crate::events;
use crate::shell::{self, Out, Style, System, Table, FRAME_TICKS, SCREENS};
use crate::term::Term;
use crate::text::FixedStr;
use crate::theme::role;

/// Rows of the header, then one row of title bar, then the body.
pub const HEADER_ROWS: usize = 4;
pub const BAR_ROW: usize = HEADER_ROWS;
pub const BODY_TOP: usize = BAR_ROW + 1;
/// Rows the city takes at the top of the overview. The kernel draws it there
/// in pixels; the grid leaves those rows empty.
pub const CITY_ROWS: usize = crate::city::HEIGHT / 8;

/// First row of the terminal of a screen.
pub const fn term_top(screen: usize) -> usize {
    if screen == 0 {
        BODY_TOP + CITY_ROWS
    } else {
        BODY_TOP
    }
}

/// Lines of points, a point being a dot of a character, that a code may
/// take on a screen of `rows` rows: all that is below the title bar but
/// the last row, which says what to do.
pub const fn code_points(rows: usize) -> usize {
    (rows - BODY_TOP - 1) * 8
}

/// The rhythm of what moves by itself, when the screen gives it: its
/// chip counts the sweeps, and a picture stays a whole number of them.
/// Every picture is then seen for exactly as long as the others, which a
/// timer that knows nothing of the screen cannot promise.
pub struct Rhythm {
    /// The count of the chip when it was last read; None while the screen
    /// is not shown by a chip that counts.
    last: Option<u32>,
    /// Sweeps that have not made a picture yet.
    owed: u64,
    /// Sweeps a picture stays, and pictures since the start.
    every: u64,
    pictures: u64,
    /// The count and the tick at the start of the second being measured,
    /// and the sweeps per second found for the last one.
    since: (u32, u64),
    sweeps: u32,
}

impl Rhythm {
    pub const fn new() -> Rhythm {
        // until a second has been measured: the screens of most machines
        Rhythm { last: None, owed: 0, every: 2, pictures: 0, since: (0, 0), sweeps: 60 }
    }

    /// Takes the count of sweeps of the chip, or None if no chip shows the
    /// screen, and the ticks of the timer. Returns how many pictures became
    /// due; None when the timer must give the rhythm.
    pub fn advance(&mut self, count: Option<u32>, ticks: u64) -> Option<u64> {
        let Some(count) = count else {
            self.last = None;
            return None;
        };
        let Some(last) = self.last.replace(count) else {
            (self.owed, self.since) = (0, (count, ticks));
            return Some(0);
        };
        // the count goes round, the difference does not
        self.owed += count.wrapping_sub(last) as u64;
        if ticks >= self.since.1 + 100 {
            // As near to 25 pictures per second as whole sweeps allow.
            self.sweeps = (count.wrapping_sub(self.since.0) as u64 * 100 / (ticks - self.since.1)) as u32;
            self.every = ((self.sweeps as u64 + 12) / 25).max(1);
            self.since = (count, ticks);
        }
        let due = self.owed / self.every;
        self.owed %= self.every;
        self.pictures += due;
        Some(due)
    }

    /// Sweeps a picture stays, and sweeps per second.
    pub fn pace(&self) -> (u32, u32) {
        (self.every as u32, self.sweeps)
    }

    /// Pictures since the start.
    pub fn pictures(&self) -> u64 {
        self.pictures
    }
}

const LOGO: &[u8] = b"XILAOS";
const TITLES: [&[u8]; 2] = [b"OVERVIEW", b"BOOT LOG"];

pub struct Ui {
    pub grid: Grid,
    /// One terminal per screen: the prompt, and the boot log.
    terms: [Term; 2],
    editor: LineEditor,
    /// The selected line of the menu, when it is open.
    menu: Option<usize>,
    /// Something changed that `render` must show.
    pub dirty: bool,
    shown_meter: u16,
    /// What the title bar last said of the doors to the network, the
    /// prompt and the pages: open or not, and who asked last.
    shown_remote: (bool, Option<crate::net::Ip>, bool, Option<crate::net::Ip>),
}

/// Writes command output to a terminal.
struct TermOut<'a> {
    term: &'a mut Term,
}

impl Out for TermOut<'_> {
    fn line(&mut self, style: Style, text: &[u8]) {
        let fg = match style {
            Style::Out | Style::Done => role::TERM_OUT,
            Style::Error => role::ERROR,
        };
        self.term.line(text, fg);
    }

    fn table(&mut self, t: &Table) {
        self.term.line(t.aligned(None).bytes(), role::TERM_PROMPT);
        for r in 0..t.rows {
            self.term.line(t.aligned(Some(r)).bytes(), role::TERM_OUT);
        }
        if t.numbered {
            self.term.line(b"Type a number to follow a line.", role::TERM_OUT);
        }
    }

    fn clear(&mut self) {
        self.term.clear();
    }
}

impl Ui {
    pub const fn new() -> Ui {
        Ui {
            grid: Grid::new(),
            terms: [Term::new(role::TERM_BASE), Term::new(role::BOOT_BASE)],
            editor: LineEditor::new(),
            menu: None,
            dirty: true,
            shown_meter: 0,
            shown_remote: (false, None, false, None),
        }
    }

    /// Adds a line to the boot log screen.
    pub fn log(&mut self, text: &[u8]) {
        self.terms[1].line(text, role::BOOT_OUT);
        self.dirty = true;
    }

    /// Writes the first lines of the terminal.
    pub fn welcome(&mut self) {
        let term = &mut self.terms[0];
        term.line(b"X I L A O S", role::TERM_PROMPT);
        term.line(&[0xc4; COLS], role::TERM_OUT);
        let mut hello = FixedStr::<COLS>::from_bytes(b"XilaOS ");
        hello.push_bytes(crate::VERSION);
        hello.push_bytes(b". Type help to begin. [Alt]+[E] opens the menu of screens.");
        term.line(hello.bytes(), role::TERM_OUT);
        self.dirty = true;
    }

    fn term_rows(&self, screen: usize) -> usize {
        self.grid.rows - term_top(screen)
    }

    /// Handles a key.
    pub fn key(&mut self, sys: &mut System, ev: KeyEvent) {
        self.dirty = true;
        sys.nudge(30);
        // a code is on the screen: any key puts it away
        if sys.code.take().is_some() {
            return;
        }

        if ev.alt {
            if let Some(d) = ev.digit {
                if (d as usize) < SCREENS.len() {
                    sys.screen = d as usize;
                    self.menu = None;
                }
            } else if let Key::Char(c) = ev.key {
                match c.to_ascii_lowercase() {
                    b'e' => self.menu = if self.menu.is_some() { None } else { Some(sys.screen) },
                    b'x' => sys.mood = sys.mood.next(),
                    _ => {}
                }
            }
            return;
        }

        if let Some(selected) = self.menu {
            let n = SCREENS.len();
            match ev.key {
                Key::Up => self.menu = Some((selected + n - 1) % n),
                Key::Down => self.menu = Some((selected + 1) % n),
                Key::Enter => {
                    sys.screen = selected;
                    self.menu = None;
                }
                Key::Esc => self.menu = None,
                _ => {}
            }
            return;
        }

        let page = self.term_rows(sys.screen).saturating_sub(1);
        let term = &mut self.terms[sys.screen];
        let hidden = (term.len() + (sys.screen == 0) as usize).saturating_sub(page + 1);
        match ev.key {
            Key::PageUp => {
                term.scroll = (term.scroll + page).min(hidden);
                return;
            }
            Key::PageDown => {
                term.scroll = term.scroll.saturating_sub(page);
                return;
            }
            Key::Esc => {
                sys.screen = 0;
                return;
            }
            _ => {}
        }
        if sys.screen != 0 {
            return;
        }

        term.scroll = 0;
        match ev.key {
            Key::Char(c) => {
                self.editor.insert(c);
            }
            Key::Backspace => self.editor.backspace(),
            Key::Delete => self.editor.delete(),
            Key::Left => self.editor.left(),
            Key::Right => self.editor.right(),
            Key::Home => self.editor.home(),
            Key::End => self.editor.end(),
            Key::Up => self.editor.older(),
            Key::Down => self.editor.newer(),
            Key::Tab => self.complete(),
            Key::Enter => {
                let line = self.editor.take();
                if shell::typed(line.bytes()) != line {
                    // it holds a secret: the arrow keys do not bring it back
                    self.editor.forget();
                }
                self.run(sys, line.bytes());
            }
            _ => {}
        }
    }

    /// Runs a line as if it had been typed at the prompt of the screen.
    pub fn run(&mut self, sys: &mut System, line: &[u8]) {
        let term = &mut self.terms[0];
        term.scroll = 0;
        term.new_row();
        term.put(shell::prompt(sys, events::SCREEN), role::TERM_PROMPT);
        // as it may be shown: a key that is set is not
        term.put(shell::typed(line).bytes(), role::TERM_CMD);
        sys.session = events::SCREEN;
        shell::exec(sys, line, &mut TermOut { term });
        sys.nudge(50);
        self.dirty = true;
    }

    /// Shows on the last row how far a long piece of work is, written
    /// then read back, for the kernel to put on the screen while it works:
    /// the next `render` takes the row back. Returns how much of the bar
    /// is filled, so that only a bar that moved is shown.
    pub fn progress(&mut self, screen: usize, done: u64, total: u64) -> usize {
        const BAR: usize = 40;
        let filled = if total == 0 { BAR } else { (done.min(total) * BAR as u64 / total) as usize };
        let mut line = FixedStr::<COLS>::from_bytes(if done * 2 <= total && total > 0 { b" Writing the disk  [" } else { b" Reading it back   [" });
        for cell in 0..BAR {
            line.push(if cell < filled { b'#' } else { b'.' });
        }
        let _ = write!(line, "] {:3}%", filled * 100 / BAR);
        let (row, bg) = (self.grid.rows.saturating_sub(1), self.terms[screen.min(SCREENS.len() - 1)].bg);
        let end = self.grid.text(0, row, line.bytes(), role::TERM_OUT, bg);
        self.grid.fill(row, end, COLS, Cell::new(b' ', role::TERM_OUT, bg));
        filled
    }

    /// Shows, where the last command was typed, what the kernel answered to
    /// its request.
    pub fn served(&mut self, sys: &mut System) {
        let term = &mut self.terms[0];
        let rows = term.len();
        shell::served(sys, &mut TermOut { term });
        self.dirty |= term.len() != rows;
    }

    fn complete(&mut self) {
        let typed = FixedStr::<COLS>::from_bytes(self.editor.bytes());
        if typed.is_empty() || typed.bytes().contains(&b' ') {
            return;
        }
        let (matches, completed) = shell::complete(typed.bytes());
        if matches == 0 {
            return;
        }
        self.editor.set(completed.bytes());
        if matches > 1 {
            let mut list = FixedStr::<COLS>::new();
            for name in shell::NAMES.iter().filter(|n| n.starts_with(typed.bytes())) {
                list.push_bytes(name);
                list.push_bytes(b"  ");
            }
            self.terms[0].line(list.bytes(), role::TERM_OUT);
        }
    }

    /// Lets time pass: `ticks` timer ticks since the last call.
    pub fn tick(&mut self, sys: &mut System, ticks: u64) {
        sys.ticks += ticks;
        sys.settle(ticks);
        sys.sample();
        // What the user chose since the last tick is kept for the next start.
        crate::settings::keep(sys);
        // The programs that run by themselves; what they say goes to the log.
        let rows = self.terms[1].len();
        crate::lang::background(sys, &mut TermOut { term: &mut self.terms[1] });
        if self.terms[1].len() != rows {
            sys.log_rows = self.terms[1].len();
            self.dirty = true;
        }
        // What this prompt listens to is shown as it happens.
        while let Some(event) = sys.events.take(events::SCREEN) {
            let line = FixedStr::<COLS>::from_bytes(b"event ").also(event.channel.bytes()).also(b": ").also(event.text.bytes());
            self.terms[0].line(line.bytes(), role::TERM_PROMPT);
            self.dirty = true;
        }
        // The companion's answer to what was asked here, when it has come.
        if crate::companion::tell(sys, events::SCREEN, &mut TermOut { term: &mut self.terms[0] }).is_some() {
            self.terms[0].scroll = 0;
            self.dirty = true;
        }
        let lost = sys.events.take_lost(events::SCREEN);
        if lost > 0 {
            let mut line = FixedStr::<COLS>::new();
            let _ = write!(line, "{lost} more events came too fast to be shown.");
            self.terms[0].line(line.bytes(), role::TERM_PROMPT);
            self.dirty = true;
        }
        // The cursor fades in and out in 32 pictures, one step per picture:
        // 1.28 s by the timer, about as long by the sweeps of the screen.
        let phase = (sys.picture.unwrap_or(sys.ticks / FRAME_TICKS) * 4 % 128) as u32;
        let level = if phase < 64 { phase * 4 } else { (127 - phase) * 4 };
        self.grid.set_pulse(level as u8);
        let meter = (sys.energy + 50) / 100;
        if meter != self.shown_meter {
            self.shown_meter = meter;
            self.dirty = true;
        }
        // a door to the network opens, closes, or lets someone in
        let remote = (sys.remote.code().is_some(), sys.remote.from, sys.web.code().is_some(), sys.web.from);
        if remote != self.shown_remote {
            self.shown_remote = remote;
            self.dirty = true;
        }
    }

    /// Rewrites the grid from the current state.
    pub fn render(&mut self, sys: &mut System) {
        self.dirty = false;
        self.header(sys);
        self.bar(sys);
        self.body(sys);
        if let Some(selected) = self.menu {
            self.menu_box(selected, term_top(sys.screen));
        }
        sys.term_rows = self.terms[0].len();
        sys.log_rows = self.terms[1].len();
        sys.peek_rows = self.grid.rows;
        for row in 0..self.grid.rows {
            sys.peek[row] = self.grid.row_text(row);
        }
    }

    fn header(&mut self, sys: &System) {
        let g = &mut self.grid;
        let (ink, bg) = (role::MAST_INK, role::MAST_GRADIENT);
        for row in 0..HEADER_ROWS {
            g.fill(row, 0, COLS, Cell::new(b' ', ink, bg));
        }
        // The frame, with the dashes of V2_OS breaking its lines.
        g.fill(0, 1, COLS - 1, Cell::new(0xc4, ink, bg));
        g.fill(3, 1, COLS - 1, Cell::new(0xc4, ink, bg));
        for (row, left, right) in [(0, 0xda, 0xbf), (1, 0xb3, 0xb3), (2, 0xb3, 0xb3), (3, 0xc0, 0xd9)] {
            g.put(0, row, Cell::new(left, ink, bg));
            g.put(COLS - 1, row, Cell::new(right, ink, bg));
        }
        g.text(13, 0, b"--\xfa  \xfa--", ink, bg);
        g.text(48, 0, b"--\xfa \xfa-", ink, bg);
        g.text(4, 3, b"-\xc4-\xfa-", ink, bg);
        g.text(38, 3, b"--\xfa", ink, bg);
        g.text(56, 3, b"--\xfa     \xfa--", ink, bg);
        // Version, in a notch of the top line.
        let mut version = FixedStr::<24>::from_bytes(b"\xb4 Version ");
        version.push_bytes(crate::VERSION);
        version.push_bytes(b" \xc3");
        g.text(COLS - 3 - version.len(), 0, version.bytes(), ink, bg);
        // The logo: each letter four times larger, over two rows and two columns.
        for (i, &letter) in LOGO.iter().enumerate() {
            for quarter in 0..4u8 {
                let cell = Cell { ch: letter, fg: role::MAST_LOGO, bg, fx: fx::BIG | quarter };
                g.put(3 + 2 * i + (quarter & 1) as usize, 1 + (quarter >> 1) as usize, cell);
            }
        }
        // The state of the system, on the right.
        let mut knobs = FixedStr::<COLS>::from_bytes(b"mood [");
        knobs.push_bytes(sys.mood.name());
        knobs.push_bytes(b"] ");
        knobs.push_bytes(&sys.meter());
        knobs.push_bytes(b"   sound [off]   irc [0]");
        g.text(COLS - 3 - knobs.len(), 2, knobs.bytes(), ink, bg);
    }

    fn bar(&mut self, sys: &System) {
        let g = &mut self.grid;
        let (ink, bg) = (role::BAR_INK, role::BAR_BG);
        g.fill(BAR_ROW, 0, COLS, Cell::new(b' ', ink, bg));
        g.put(0, BAR_ROW, Cell { ch: b' ', fg: ink, bg, fx: fx::ROUND_L });
        g.put(COLS - 1, BAR_ROW, Cell { ch: b' ', fg: ink, bg, fx: fx::ROUND_R });
        g.text(2, BAR_ROW, if sys.code.is_some() { b"CODE" } else { TITLES[sys.screen] }, ink, bg);
        g.put(COLS - 3, BAR_ROW, Cell::new(b'=', role::BAR_ICON, bg));
        // Whoever sits at the machine sees which doors are open to the
        // network, the prompt and the pages, and who came through last,
        // on every screen.
        let mut doors = FixedStr::<64>::new();
        for (name, open, from) in [(&b"web: "[..], sys.web.code().is_some(), sys.web.from), (b"remote: ", sys.remote.code().is_some(), sys.remote.from)] {
            if open {
                if !doors.is_empty() {
                    doors.push_bytes(b"  ");
                }
                doors.push_bytes(name);
                match from {
                    Some(from) => doors.push_bytes(crate::net::ip_text(from).bytes()),
                    None => doors.push_bytes(b"open"),
                }
            }
        }
        g.text(COLS - 5 - doors.len(), BAR_ROW, doors.bytes(), role::BAR_ICON, bg);
    }

    fn body(&mut self, sys: &System) {
        if sys.code.is_some() {
            // the kernel paints the code over these rows; the last one is for words
            let blank = Cell::new(b' ', role::TERM_OUT, self.terms[0].bg);
            for row in BODY_TOP..self.grid.rows {
                self.grid.fill(row, 0, COLS, blank);
            }
            self.grid.text(0, self.grid.rows - 1, b"Read this code with the camera of a phone. Any key goes back.", role::TERM_OUT, blank.bg);
            return;
        }
        let top = term_top(sys.screen);
        let height = self.term_rows(sys.screen);
        let term = &self.terms[sys.screen];
        // The rows of the city, if any, belong to the kernel.
        for row in BODY_TOP..top {
            self.grid.fill(row, 0, COLS, Cell::new(b' ', role::TERM_OUT, term.bg));
        }
        let prompt = sys.screen == 0;
        let total = term.len() + prompt as usize;
        let first = total.saturating_sub(height);
        let first = first - term.scroll.min(first);
        let blank = Cell::new(b' ', role::TERM_OUT, term.bg);
        for r in 0..height {
            let row = top + r;
            let i = first + r;
            if i < term.len() {
                let cells = term.row(i);
                for (col, &cell) in cells.iter().enumerate() {
                    self.grid.put(col, row, cell);
                }
            } else if prompt && i == term.len() {
                let mut col = self.grid.text(0, row, shell::prompt(sys, events::SCREEN), role::TERM_PROMPT, term.bg);
                // a line longer than the row slides to the left under the
                // prompt, so that the cursor stays in sight
                let room = COLS - col;
                let typed = self.editor.bytes();
                let start = self.editor.cursor().saturating_sub(room - 1);
                let cursor = col + self.editor.cursor() - start;
                col = self.grid.text(col, row, &typed[start..typed.len().min(start + room)], role::TERM_CMD, term.bg);
                self.grid.fill(row, col, COLS, blank);
                let under = self.grid.get(cursor, row).ch;
                self.grid.put(cursor, row, Cell { ch: under, fg: role::TERM_CMD, bg: term.bg, fx: fx::PULSE });
            } else {
                self.grid.fill(row, 0, COLS, blank);
            }
        }
    }

    fn menu_box(&mut self, selected: usize, top: usize) {
        const WIDTH: usize = 26;
        let g = &mut self.grid;
        let left = COLS - WIDTH - 1;
        let (ink, bg) = (role::TERM_PROMPT, role::TERM_BASE);
        let bottom = top + SCREENS.len() + 1;
        g.fill(top, left, left + WIDTH, Cell::new(0xc4, ink, bg));
        g.fill(bottom, left, left + WIDTH, Cell::new(0xc4, ink, bg));
        for (row, l, r) in [(top, 0xda, 0xbf), (bottom, 0xc0, 0xd9)] {
            g.put(left, row, Cell::new(l, ink, bg));
            g.put(left + WIDTH - 1, row, Cell::new(r, ink, bg));
        }
        for (i, name) in SCREENS.iter().enumerate() {
            let row = top + 1 + i;
            let (fg, back) = if i == selected { (role::TERM_BASE, role::TERM_CMD) } else { (ink, bg) };
            g.put(left, row, Cell::new(0xb3, ink, bg));
            g.fill(row, left + 1, left + WIDTH - 1, Cell::new(b' ', fg, back));
            let mut item = FixedStr::<32>::from_bytes(b" [Alt]+[");
            item.push(b'0' + i as u8);
            item.push_bytes(b"]  ");
            item.push_bytes(name);
            g.text(left + 1, row, item.bytes(), fg, back);
            g.put(left + WIDTH - 1, row, Cell::new(0xb3, ink, bg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Layout;

    fn key(k: Key) -> KeyEvent {
        KeyEvent { key: k, alt: false, ctrl: false, digit: None }
    }

    fn alt(c: u8, digit: Option<u8>) -> KeyEvent {
        KeyEvent { key: Key::Char(c), alt: true, ctrl: false, digit }
    }

    fn type_text(ui: &mut Ui, sys: &mut System, text: &str) {
        for c in text.bytes() {
            ui.key(sys, key(Key::Char(c)));
        }
    }

    #[test]
    fn work_that_takes_time_shows_how_far_it_is() {
        let (mut ui, mut sys) = start();
        ui.render(&mut sys);
        let last = ui.grid.rows - 1;
        assert_eq!(ui.progress(0, 0, 4000), 0);
        assert_eq!(row(&ui, last), " Writing the disk  [........................................]   0%");
        assert_eq!(ui.progress(0, 1000, 4000), 10);
        assert_eq!(row(&ui, last), " Writing the disk  [##########..............................]  25%");
        assert_eq!(ui.progress(0, 3001, 4000), 30);
        assert_eq!(row(&ui, last), " Reading it back   [##############################..........]  75%");
        assert_eq!(ui.progress(0, 4000, 4000), 40);
        assert_eq!(row(&ui, last), " Reading it back   [########################################] 100%");
        // the next redraw takes the row back
        ui.render(&mut sys);
        assert!(!row(&ui, last).contains('['));
    }

    #[test]
    fn the_cursor_moves_one_step_per_picture() {
        let (mut ui, mut sys) = (Ui::new(), System::new());
        let (mut steps, mut level) = (0, ui.grid.pulse);
        for _ in 0..128 {
            ui.tick(&mut sys, 1);
            if ui.grid.pulse != level {
                level = ui.grid.pulse;
                steps += 1;
            }
        }
        assert_eq!(steps, 128 / FRAME_TICKS);
        // when the screen gives the rhythm, the timer no longer moves the cursor
        sys.picture = Some(7);
        ui.tick(&mut sys, 1);
        let level = ui.grid.pulse;
        for _ in 0..20 {
            ui.tick(&mut sys, 1);
        }
        assert_eq!(ui.grid.pulse, level);
        sys.picture = Some(8);
        ui.tick(&mut sys, 1);
        assert_ne!(ui.grid.pulse, level);
    }

    #[test]
    fn what_the_kernel_answers_is_shown_where_the_command_was_typed() {
        let (mut ui, mut sys) = (Ui::new(), System::new());
        type_text(&mut ui, &mut sys, "display test");
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(sys.request, Some(shell::Request::Measure));
        ui.dirty = false;
        ui.served(&mut sys);
        assert!(!ui.dirty, "nothing to show before the kernel answers");
        sys.measured = Some(shell::Measure { rounds: 5, drawn_us: 3000, shown_us: 1000 });
        ui.served(&mut sys);
        assert!(ui.dirty);
        ui.render(&mut sys);
        let last = (0..ui.grid.rows).rev().map(|r| row(&ui, r)).find(|text| text.starts_with("The whole"));
        assert_eq!(last.as_deref(), Some("The whole screen: drawn in 3.0 ms, shown in 1.0 ms (best of 5)."));
    }

    #[test]
    fn the_screen_gives_the_rhythm_of_what_moves() {
        let mut rhythm = Rhythm::new();
        assert_eq!(rhythm.advance(None, 0), None);
        // a screen swept 60 times per second, read every tick: 0.6 sweep per tick
        let count = |tick: u64| Some((0xffff_fff0u64 + tick * 6 / 10) as u32);
        assert_eq!(rhythm.advance(count(0), 0), Some(0));
        let mut pictures = 0;
        let mut gaps = Vec::new();
        let mut last = 0;
        for tick in 1..=1000 {
            let due = rhythm.advance(count(tick), tick).unwrap();
            assert!(due <= 1);
            if due == 1 {
                gaps.push(tick - last);
                last = tick;
            }
            pictures += due;
        }
        // thirty pictures per second, one every two sweeps, the count going round on the way
        assert_eq!((pictures, rhythm.pictures(), rhythm.pace()), (300, 300, (2, 60)));
        assert!(gaps[1..].iter().all(|gap| (3..=4).contains(gap)), "{gaps:?}");
        // other screens: as near to 25 pictures per second as whole sweeps allow
        for (sweeps, every) in [(50, 2), (75, 3), (120, 5), (30, 1), (144, 6), (24, 1)] {
            let mut rhythm = Rhythm::new();
            for tick in 0..=250u64 {
                rhythm.advance(Some((tick * sweeps / 100) as u32), tick);
            }
            assert_eq!(rhythm.pace(), (every, sweeps as u32), "{sweeps} sweeps per second");
        }
        // a system that was late owes the pictures it missed, all at once
        let mut rhythm = Rhythm::new();
        rhythm.advance(Some(10), 0);
        assert_eq!(rhythm.advance(Some(31), 35), Some(10));
        assert_eq!(rhythm.advance(Some(32), 37), Some(1));
        // back to the timer, then to the screen again: nothing is owed for the time between
        assert_eq!(rhythm.advance(None, 40), None);
        assert_eq!(rhythm.advance(Some(500), 900), Some(0));
        assert_eq!(rhythm.advance(Some(502), 903), Some(1));
    }

    #[test]
    fn a_code_takes_the_screen_until_a_key_is_pressed() {
        let (mut ui, mut sys) = (Ui::new(), System::new());
        ui.grid.rows = 48;
        ui.welcome();
        type_text(&mut ui, &mut sys, "qr about");
        ui.key(&mut sys, key(Key::Enter));
        ui.served(&mut sys);
        assert!(sys.code.is_some());
        ui.render(&mut sys);
        assert!(row(&ui, BAR_ROW).contains("CODE"));
        // the rows the kernel paints are left empty, the last one says what to do
        assert!((BODY_TOP..47).all(|r| row(&ui, r).is_empty()));
        assert_eq!(row(&ui, 47), "Read this code with the camera of a phone. Any key goes back.");
        // any key puts the code away and does nothing else
        ui.key(&mut sys, key(Key::Char(b'x')));
        assert!(sys.code.is_none() && ui.dirty);
        ui.render(&mut sys);
        assert!(row(&ui, BAR_ROW).contains("OVERVIEW"));
        let shown: Vec<String> = (0..48).map(|r| row(&ui, r)).collect();
        assert!(shown.iter().any(|text| text.starts_with("A code of ") && text.ends_with(" characters is on the screen.")), "{shown:?}");
        let prompt = shown.iter().rev().find(|text| !text.is_empty()).unwrap();
        assert!(prompt.ends_with(":/>"), "the key was not typed: {prompt}");
    }

    /// A row of the grid as text; characters outside ASCII become `~`.
    fn row(ui: &Ui, row: usize) -> String {
        let text: String = ui.grid.row_text(row).iter().map(|&c| if c < 0x80 { c as char } else { '~' }).collect();
        text.trim_end().to_string()
    }

    /// The rows of the body that are not empty.
    fn body(ui: &Ui) -> Vec<String> {
        (BODY_TOP..ui.grid.rows).map(|r| row(ui, r)).filter(|l| !l.is_empty()).collect()
    }

    /// First row of the terminal of the overview.
    const TOP: usize = term_top(0);

    fn start() -> (Box<Ui>, Box<System>) {
        let mut ui = Box::new(Ui::new());
        let mut sys = Box::new(System::new());
        sys.layout = Layout::Us;
        ui.render(&mut sys);
        (ui, sys)
    }

    #[test]
    fn header_and_title_bar() {
        let (ui, _) = start();
        assert!(row(&ui, 0).ends_with("~ Version 0.1 ~~~~"));
        // the logo is drawn in big cells, the state of the system on its right
        assert!(row(&ui, 1).contains("############"));
        assert!(row(&ui, 2).contains("mood [auto] [..........]   sound [off]   irc [0]"));
        assert_eq!(row(&ui, BAR_ROW), "  OVERVIEW                                                                   =");
        assert_eq!(ui.grid.get(0, BAR_ROW).fx, fx::ROUND_L);
    }

    #[test]
    fn the_title_bar_says_when_the_prompt_is_open_to_the_network() {
        let (mut ui, mut sys) = start();
        type_text(&mut ui, &mut sys, "remote on");
        ui.key(&mut sys, key(Key::Enter));
        ui.render(&mut sys);
        assert_eq!(row(&ui, BAR_ROW), "  OVERVIEW                                                     remote: open  =");
        // someone types there: who it is takes the place of "open", without a key being pressed here
        let code = crate::remote::written(sys.remote.code().unwrap());
        let mut request = code.bytes().to_vec();
        request.extend_from_slice(b" 1 uptime");
        sys.remote.asked([192, 168, 1, 168], true, &request);
        assert!(!ui.dirty);
        ui.tick(&mut sys, 1);
        assert!(ui.dirty);
        ui.render(&mut sys);
        assert_eq!(row(&ui, BAR_ROW), "  OVERVIEW                                            remote: 192.168.1.168  =");
        // on the other screen too
        sys.screen = 1;
        ui.render(&mut sys);
        assert!(row(&ui, BAR_ROW).starts_with("  BOOT LOG") && row(&ui, BAR_ROW).ends_with("remote: 192.168.1.168  ="));
        // the pages for browsers are another door, shown beside it
        sys.screen = 0;
        type_text(&mut ui, &mut sys, "web on");
        ui.key(&mut sys, key(Key::Enter));
        ui.render(&mut sys);
        assert!(row(&ui, BAR_ROW).ends_with("  web: open  remote: 192.168.1.168  ="), "{}", row(&ui, BAR_ROW));
        sys.web.close(&mut sys.net.tcp);
        sys.screen = 1;
        // closed, by a command or by itself, the mark goes
        sys.remote.close();
        ui.tick(&mut sys, 1);
        assert!(ui.dirty);
        ui.render(&mut sys);
        assert_eq!(row(&ui, BAR_ROW), "  BOOT LOG                                                                   =");
    }

    #[test]
    fn typing_a_command_shows_prompt_and_result() {
        let (mut ui, mut sys) = start();
        type_text(&mut ui, &mut sys, "mood calm");
        ui.render(&mut sys);
        assert_eq!(body(&ui), ["XilaOS|all:/>mood calm"]);
        ui.key(&mut sys, key(Key::Enter));
        ui.render(&mut sys);
        assert_eq!(body(&ui), ["XilaOS|all:/>mood calm", "Mood is calm.", "XilaOS|all:/>"]);
        assert!(row(&ui, 2).contains("mood [calm]"));
    }

    #[test]
    fn the_companion_answers_on_the_screen_when_the_model_has() {
        let (mut ui, mut sys) = start();
        sys.net.ip = Some([10, 0, 2, 15]);
        crate::secrets::set(&mut sys.db, b"openrouter", b"sk-or-v1-secret").unwrap();
        type_text(&mut ui, &mut sys, "ask are you there?");
        ui.key(&mut sys, key(Key::Enter));
        ui.tick(&mut sys, 1);
        ui.render(&mut sys);
        // asked: the prompt is free again, and nothing is said yet
        assert_eq!(body(&ui), ["XilaOS|all:/>ask are you there?", "XilaOS|all:/>"]);
        type_text(&mut ui, &mut sys, "mood calm");
        ui.key(&mut sys, key(Key::Enter));
        // the model answers: at the next tick it is on the screen
        crate::companion::heard(&mut sys, Ok((200, br#"{"choices":[{"message":{"content":"I am.\nAlways."}}]}"#)));
        ui.tick(&mut sys, 1);
        ui.render(&mut sys);
        assert_eq!(body(&ui), ["XilaOS|all:/>ask are you there?", "XilaOS|all:/>mood calm", "Mood is calm.", "I am.", "Always.", "XilaOS|all:/>"]);
        ui.tick(&mut sys, 1);
        ui.render(&mut sys);
        assert_eq!(body(&ui).len(), 6, "said once");
    }

    #[test]
    fn a_key_that_is_typed_is_not_shown_again() {
        let (mut ui, mut sys) = start();
        type_text(&mut ui, &mut sys, "key set openrouter sk-or-v1-secret");
        ui.key(&mut sys, key(Key::Enter));
        ui.render(&mut sys);
        let shown: Vec<String> = (0..ui.grid.rows).map(|at| row(&ui, at)).collect();
        // the screen keeps stars in place of the key
        assert!(shown.iter().any(|row| row.contains("key set openrouter ********")), "{shown:?}");
        assert!(shown.iter().any(|row| row.contains("The key openrouter is kept. It is never shown.")));
        assert!(!shown.iter().any(|row| row.contains("sk-or")), "{shown:?}");
        // and the arrow that brings lines back does not bring that one
        type_text(&mut ui, &mut sys, "mood calm");
        ui.key(&mut sys, key(Key::Enter));
        ui.key(&mut sys, key(Key::Up));
        assert_eq!(ui.editor.bytes(), b"mood calm");
        ui.key(&mut sys, key(Key::Up));
        assert_eq!(ui.editor.bytes(), b"mood calm", "nothing older to bring back");
        // the key itself is kept
        assert_eq!(crate::secrets::get(&sys.db, b"openrouter").unwrap().bytes(), b"sk-or-v1-secret");
    }

    #[test]
    fn a_long_line_slides_under_the_prompt() {
        let (mut ui, mut sys) = start();
        let statement = "insert into expenses values ('2026-11-18', 'a long label to pass the edge', 12.5)";
        assert!(shell::PROMPT.len() + statement.len() > COLS);
        type_text(&mut ui, &mut sys, "create table expenses (day text, label text, amount decimal)");
        ui.key(&mut sys, key(Key::Enter));
        type_text(&mut ui, &mut sys, statement);
        ui.render(&mut sys);
        let shown = body(&ui).last().cloned().unwrap();
        assert!(shown.starts_with("XilaOS|all:/>") && shown.ends_with("pass the edge', 12.5)"), "{shown}");
        assert_eq!(shown.len(), COLS - 1);
        // going back to the start shows the start again
        ui.key(&mut sys, key(Key::Home));
        ui.render(&mut sys);
        assert!(body(&ui).last().unwrap().starts_with("XilaOS|all:/>insert into expenses values ('2026-11-18',"));
        ui.key(&mut sys, key(Key::Enter));
        ui.render(&mut sys);
        let lines = body(&ui);
        assert_eq!(lines[lines.len() - 2], "1 row added to expenses, with id 1.");
    }

    #[test]
    fn events_show_at_the_prompt_that_listens() {
        let (mut ui, mut sys) = start();
        type_text(&mut ui, &mut sys, "listen alerts");
        ui.key(&mut sys, key(Key::Enter));
        sys.session = events::SERIAL;
        shell::exec(&mut sys, b"notify alerts 'the disk is full'", &mut shell::tests::Lines::default());
        ui.tick(&mut sys, 1);
        ui.render(&mut sys);
        assert_eq!(body(&ui)[1..], ["Listening to alerts.", "event alerts: the disk is full", "XilaOS|all:/>"]);
    }

    #[test]
    fn cursor_cell_pulses_where_the_next_character_goes() {
        let (mut ui, mut sys) = start();
        type_text(&mut ui, &mut sys, "ab");
        ui.key(&mut sys, key(Key::Left));
        ui.render(&mut sys);
        let cell = ui.grid.get(shell::PROMPT.len() + 1, TOP);
        assert_eq!((cell.ch, cell.fx), (b'b', fx::PULSE));
    }

    #[test]
    fn numbered_results_are_followed_by_typing_the_number() {
        let (mut ui, mut sys) = start();
        ui.run(&mut sys, b"screen");
        ui.run(&mut sys, b"2");
        ui.render(&mut sys);
        assert_eq!(sys.screen, 1);
        assert_eq!(row(&ui, BAR_ROW).trim(), "BOOT LOG                                                                   =");
    }

    #[test]
    fn tab_completes_and_lists() {
        let (mut ui, mut sys) = start();
        type_text(&mut ui, &mut sys, "upt");
        ui.key(&mut sys, key(Key::Tab));
        ui.render(&mut sys);
        assert_eq!(body(&ui), ["XilaOS|all:/>uptime"]);
        ui.key(&mut sys, key(Key::Enter));
        type_text(&mut ui, &mut sys, "m");
        ui.key(&mut sys, key(Key::Tab));
        ui.render(&mut sys);
        assert_eq!(body(&ui)[2..], ["mem  mood", "XilaOS|all:/>m"]);
    }

    #[test]
    fn menu_and_shortcuts_switch_screens() {
        let (mut ui, mut sys) = start();
        ui.log(b"first line of the log");
        ui.key(&mut sys, alt(b'e', None));
        ui.render(&mut sys);
        assert!(row(&ui, TOP + 1).ends_with("~ [Alt]+[0]  overview    ~"));
        ui.key(&mut sys, key(Key::Down));
        ui.key(&mut sys, key(Key::Enter));
        ui.render(&mut sys);
        assert_eq!(sys.screen, 1);
        assert_eq!(body(&ui), ["first line of the log"]);
        // typing does nothing on a screen without a prompt, Esc goes back
        type_text(&mut ui, &mut sys, "x");
        ui.key(&mut sys, key(Key::Esc));
        ui.render(&mut sys);
        assert_eq!(body(&ui), ["XilaOS|all:/>"]);
        ui.key(&mut sys, alt(b'&', Some(1)));
        assert_eq!(sys.screen, 1);
    }

    #[test]
    fn long_output_scrolls_and_page_keys_go_back() {
        let (mut ui, mut sys) = start();
        for _ in 0..30 {
            ui.run(&mut sys, b"uptime");
        }
        ui.render(&mut sys);
        let height = ui.grid.rows - TOP;
        assert_eq!(row(&ui, ui.grid.rows - 1), "XilaOS|all:/>");
        assert!((BODY_TOP..TOP).all(|r| row(&ui, r).is_empty()), "the rows of the city stay empty");
        assert_eq!(body(&ui).len(), height);
        ui.key(&mut sys, key(Key::PageUp));
        ui.render(&mut sys);
        assert_eq!(row(&ui, TOP), "XilaOS|all:/>uptime");
        assert_ne!(row(&ui, ui.grid.rows - 1), "XilaOS|all:/>");
        ui.key(&mut sys, key(Key::PageDown));
        ui.render(&mut sys);
        assert_eq!(row(&ui, ui.grid.rows - 1), "XilaOS|all:/>");
    }

    #[test]
    fn mood_gauge_follows_typing_and_time() {
        let (mut ui, mut sys) = start();
        type_text(&mut ui, &mut sys, "0123456789");
        ui.tick(&mut sys, 1);
        assert!(ui.dirty);
        ui.render(&mut sys);
        assert!(row(&ui, 2).contains("[###.......]"));
        ui.tick(&mut sys, 400);
        ui.render(&mut sys);
        assert!(row(&ui, 2).contains("[..........]"));
    }

    #[test]
    fn peek_copies_the_screen() {
        let (mut ui, mut sys) = start();
        ui.welcome();
        ui.render(&mut sys);
        assert_eq!(sys.peek_rows, ui.grid.rows);
        assert_eq!(&sys.peek[TOP][..11], b"X I L A O S");
        assert_eq!((sys.term_rows, sys.log_rows), (3, 0));
    }
}
