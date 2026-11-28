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
const TITLES: [&[u8]; 3] = [b"OVERVIEW", b"BOOT LOG", b"COMPANION"];
/// The screens by their number: the boot log has no line to type on,
/// and the companion's is a conversation.
const LOG: usize = 1;
const COMPANION: usize = 2;

pub struct Ui {
    pub grid: Grid,
    /// One terminal per screen: the prompt, the boot log, and what is
    /// said with the companion.
    terms: [Term; 3],
    editor: LineEditor,
    /// The line being typed for the companion, on its screen.
    chat: LineEditor,
    /// The selected line of the menu, when it is open.
    menu: Option<usize>,
    /// Something changed that `render` must show.
    pub dirty: bool,
    shown_meter: u16,
    /// What the title bar last said of the doors to the network, the
    /// prompt and the pages: open or not, and who asked last.
    shown_remote: (bool, Option<crate::net::Ip>, bool, Option<crate::net::Ip>),
    /// The room a page of a panel is read in: kept here, not on a stack.
    page: crate::panel::Page,
}

/// Writes what is said on the companion's screen, as a conversation is
/// written: who speaks before the first line, the others under it.
struct ChatOut<'a> {
    term: &'a mut Term,
    head: FixedStr<64>,
    first: bool,
    ink: u8,
}

impl Out for ChatOut<'_> {
    fn line(&mut self, style: Style, text: &[u8]) {
        self.term.new_row();
        match core::mem::take(&mut self.first) {
            true => self.term.put(self.head.bytes(), role::TERM_PROMPT),
            false => self.term.put(&[b' '; 64][..self.head.len()], role::TERM_PROMPT),
        }
        self.term.put(text, if style == Style::Error { role::ERROR } else { self.ink });
    }

    fn table(&mut self, _: &Table) {}
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

/// What is said on a terminal, with whether anything was: a terminal
/// that is full keeps its length when a row is added, so its length
/// does not tell that it must be drawn again.
struct Told<'a> {
    out: TermOut<'a>,
    any: bool,
}

impl Out for Told<'_> {
    fn line(&mut self, style: Style, text: &[u8]) {
        self.any = true;
        self.out.line(style, text);
    }

    fn table(&mut self, table: &Table) {
        self.any = true;
        self.out.table(table);
    }

    fn clear(&mut self) {
        self.any = true;
        self.out.clear();
    }
}

impl Ui {
    pub const fn new() -> Ui {
        Ui {
            grid: Grid::new(),
            terms: [Term::new(role::TERM_BASE), Term::new(role::BOOT_BASE), Term::new(role::TERM_BASE)],
            editor: LineEditor::new(),
            chat: LineEditor::new(),
            menu: None,
            dirty: true,
            shown_meter: 0,
            shown_remote: (false, None, false, None),
            page: crate::panel::Page::new(),
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
        self.terms[COMPANION].line(b"The companion is here. Ask what you want to know: [Enter] sends, [Esc] leaves.", role::TERM_OUT);
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
                if (d as usize) < SCREENS.len() || crate::panel::place_of(sys, d as usize).is_some() {
                    sys.screen = d as usize;
                    self.menu = None;
                }
            } else if let Key::Char(c) = ev.key {
                match c.to_ascii_lowercase() {
                    b'e' => self.menu = if self.menu.is_some() { None } else { Some(Self::listed(sys).0.iter().position(|screen| *screen == sys.screen).unwrap_or(0)) },
                    b'x' => sys.mood = sys.mood.next(),
                    // the program the prompt waits for goes on without it
                    b'b' => {
                        if let Some(number) = crate::jobs::aside(sys, crate::jobs::Watch::Screen) {
                            let mut line = FixedStr::<COLS>::new();
                            let _ = write!(line, "Job {number} goes on in the background: job {number} shows it, job {number} end stops it.");
                            self.terms[0].line(line.bytes(), role::TERM_OUT);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        if let Some(selected) = self.menu {
            let (screens, n) = Self::listed(sys);
            let selected = selected.min(n - 1);
            match ev.key {
                Key::Up => self.menu = Some((selected + n - 1) % n),
                Key::Down => self.menu = Some((selected + 1) % n),
                Key::Enter => {
                    sys.screen = screens[selected];
                    self.menu = None;
                }
                Key::Esc => self.menu = None,
                _ => {}
            }
            return;
        }

        // the screen of a panel: its keys are its own
        if sys.screen >= SCREENS.len() {
            match crate::panel::place_of(sys, sys.screen) {
                Some(place) => crate::panel::key(sys, place, ev),
                None => sys.screen = 0,
            }
            return;
        }

        let page = self.term_rows(sys.screen).saturating_sub(1);
        // whether the prompt waits for a program, which then has the keyboard's Esc
        let waits = sys.screen == 0 && self.waits(sys);
        let term = &mut self.terms[sys.screen];
        let hidden = (term.len() + (sys.screen != LOG) as usize).saturating_sub(page + 1);
        match ev.key {
            Key::PageUp => {
                term.scroll = (term.scroll + page).min(hidden);
                return;
            }
            Key::PageDown => {
                term.scroll = term.scroll.saturating_sub(page);
                return;
            }
            // at the prompt, Esc stops the program it waits for
            Key::Esc if waits => {
                if let Some(message) = crate::jobs::stop_watched(sys, crate::jobs::Watch::Screen) {
                    term.line(message.bytes(), role::ERROR);
                }
                return;
            }
            Key::Esc => {
                sys.screen = 0;
                return;
            }
            _ => {}
        }
        // nothing is typed at a prompt that waits, nor on the log
        if sys.screen == LOG || waits {
            return;
        }

        term.scroll = 0;
        // the line that is typed: a command at the prompt, or something to ask the companion
        let talking = sys.screen == COMPANION;
        let editor = if talking { &mut self.chat } else { &mut self.editor };
        match ev.key {
            Key::Char(c) => editor.insert(c),
            Key::Backspace => editor.backspace(),
            Key::Delete => editor.delete(),
            Key::Left => editor.left(),
            Key::Right => editor.right(),
            Key::Home => editor.home(),
            Key::End => editor.end(),
            Key::Up => editor.older(),
            Key::Down => editor.newer(),
            Key::Tab if !talking => self.complete(),
            Key::Enter if talking => {
                let line = editor.take();
                self.say(sys, line.bytes());
            }
            Key::Enter => {
                let line = editor.take();
                if shell::typed(line.bytes()) != line {
                    // it holds a secret: the arrow keys do not bring it back
                    self.editor.forget();
                }
                self.run(sys, line.bytes());
            }
            _ => {}
        }
    }

    /// What starts a line of the conversation: the time, if the machine
    /// knows it, and who speaks.
    fn speaker(sys: &System, name: &[u8]) -> FixedStr<64> {
        let mut head = FixedStr::new();
        if let Some(now) = sys.now() {
            // `2026-11-26 16:34:58`: the hour and the minute of it
            head.push_bytes(&crate::clock::Moment::at(now).text().bytes()[11..16]);
            head.push(b' ');
        }
        head.push(b'<');
        head.push_bytes(name);
        head.push_bytes(b"> ");
        head.lower();
        head
    }

    /// Asks the companion what was typed on its screen.
    fn say(&mut self, sys: &mut System, line: &[u8]) {
        let line = crate::text::trim(line);
        if line.is_empty() {
            return;
        }
        let term = &mut self.terms[COMPANION];
        let head = Ui::speaker(sys, b"you");
        let mut said = ChatOut { term, head, first: true, ink: role::TERM_CMD };
        crate::text::wrap(line, COLS - 1 - head.len(), &mut |part| said.line(Style::Out, part));
        if let Err(message) = crate::companion::ask(sys, line, crate::companion::HERE) {
            ChatOut { term, head: FixedStr::from_bytes(b"      * "), first: true, ink: role::ERROR }.line(Style::Error, message.bytes());
        }
        sys.nudge(50);
        self.dirty = true;
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

    /// Whether the prompt of the screen waits for a job that has not ended.
    fn waits(&self, sys: &System) -> bool {
        crate::jobs::watched(sys, crate::jobs::Watch::Screen).is_some_and(|at| !sys.jobs.list()[at].ended())
    }

    /// Gives a turn to the job the prompt of the screen waits for: what
    /// it says goes where its command was typed. When it has ended the
    /// prompt is back.
    pub fn work(&mut self, sys: &mut System, spent: &mut dyn FnMut() -> bool) {
        let Some(at) = crate::jobs::watched(sys, crate::jobs::Watch::Screen) else {
            return;
        };
        let mut told = Told { out: TermOut { term: &mut self.terms[0] }, any: false };
        crate::jobs::turn(sys, at, &mut told, spent);
        if crate::jobs::outcome(sys, crate::jobs::Watch::Screen).is_some() || told.any {
            self.dirty = true;
        }
    }

    /// Gives a turn to one of the jobs nobody waits for. What a program
    /// that runs by itself says goes to the log screen. Returns the place
    /// of a job that asked the kernel something: once that is served,
    /// `carried` tells it.
    pub fn carry(&mut self, sys: &mut System, spent: &mut dyn FnMut() -> bool) -> Option<usize> {
        let mut told = Told { out: TermOut { term: &mut self.terms[LOG] }, any: false };
        let asked = crate::jobs::work(sys, &mut told, spent);
        let any = told.any;
        self.logged(sys, any);
        asked
    }

    /// The kernel served what the job at a place asked: its answer is
    /// said where that job says, and the job goes on.
    pub fn carried(&mut self, sys: &mut System, at: usize) {
        let mut told = Told { out: TermOut { term: &mut self.terms[LOG] }, any: false };
        crate::jobs::served(sys, at, &mut told);
        let any = told.any;
        self.logged(sys, any);
    }

    /// Something was said in the log: the system knows how long it is, and
    /// the screen that shows it is drawn again. (Its length alone does not
    /// tell: a terminal that is full no longer grows.)
    fn logged(&mut self, sys: &mut System, any: bool) {
        if any {
            sys.log_rows = self.terms[LOG].len();
            self.dirty = true;
        }
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
        let ok = shell::served(sys, &mut TermOut { term });
        self.dirty |= term.len() != rows;
        // the program the prompt waits for may have asked: it goes on
        if let Some(at) = crate::jobs::watched(sys, crate::jobs::Watch::Screen) {
            crate::jobs::answered(sys, at, ok);
        }
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
        // What changed in the state of the system is said, for who listens
        // and for the programs that wait for it. After the programs had
        // their turn: at the first tick they have just been read from the
        // tables, and only then can one of them hear that the system started.
        crate::watch::look(sys);
        if self.terms[1].len() != rows {
            sys.log_rows = self.terms[1].len();
            self.dirty = true;
        }
        // The list of the panel that is shown follows its table.
        if crate::panel::refresh(sys, &mut self.page) {
            self.dirty = true;
        }
        // What this prompt listens to is shown as it happens.
        while let Some(event) = sys.events.take(events::SCREEN) {
            let line = FixedStr::<COLS>::from_bytes(b"event ").also(event.channel.bytes()).also(b": ").also(event.text.bytes());
            self.terms[0].line(line.bytes(), role::TERM_PROMPT);
            self.dirty = true;
        }
        // The companion's answer, when it has come: at the prompt if it
        // was asked there, in the conversation if it was asked on its screen.
        if crate::companion::tell(sys, events::SCREEN, COLS - 1, &mut TermOut { term: &mut self.terms[0] }).is_some() {
            self.terms[0].scroll = 0;
            self.dirty = true;
        }
        if sys.companion.asker == crate::companion::HERE && sys.companion.outcome.is_some() {
            let head = Ui::speaker(sys, crate::companion::who(&sys.db).name.bytes());
            let term = &mut self.terms[COMPANION];
            term.scroll = 0;
            crate::companion::tell(sys, crate::companion::HERE, COLS - 1 - head.len(), &mut ChatOut { term, head, first: true, ink: role::TERM_OUT });
            self.dirty = true;
        }
        // What a program that runs by itself asks the companion, when its
        // turn has come: written in the conversation, under the program's name.
        if let Some((who, text, asked)) = crate::companion::next(sys) {
            let term = &mut self.terms[COMPANION];
            let head = Ui::speaker(sys, who.bytes());
            let mut said = ChatOut { term, head, first: true, ink: role::TERM_CMD };
            crate::text::wrap(text.bytes(), COLS - 1 - head.len(), &mut |part| said.line(Style::Out, part));
            if let Err(message) = asked {
                ChatOut { term, head: FixedStr::from_bytes(b"      * "), first: true, ink: role::ERROR }.line(Style::Error, message.bytes());
            }
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

    /// The virtual screens there are, by their numbers, and how many: the
    /// three of the system, then the panels that are open.
    fn listed(sys: &System) -> ([usize; SCREENS.len() + crate::panel::OPEN], usize) {
        let (mut screens, mut count) = ([0; SCREENS.len() + crate::panel::OPEN], 0);
        crate::panel::screens(sys, &mut |number, _| {
            screens[count] = number;
            count += 1;
        });
        (screens, count)
    }

    /// Rewrites the grid from the current state.
    pub fn render(&mut self, sys: &mut System) {
        self.dirty = false;
        // a list shows as many rows as this screen has for it
        sys.panels.rows = crate::panel::rows_for(self.grid.rows, BODY_TOP);
        // the panel that was shown is no longer open: the overview
        if sys.screen >= SCREENS.len() && crate::panel::place_of(sys, sys.screen).is_none() {
            sys.screen = 0;
        }
        self.header(sys);
        self.bar(sys);
        self.body(sys);
        if let Some(selected) = self.menu {
            self.menu_box(sys, selected, term_top(sys.screen));
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
        let panel = crate::panel::title(sys, sys.screen);
        let title: &[u8] = match (&sys.code, &panel) {
            (Some(_), _) => b"CODE",
            (None, Some(name)) => name.bytes(),
            (None, None) => TITLES[sys.screen.min(TITLES.len() - 1)],
        };
        g.text(2, BAR_ROW, title, ink, bg);
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
        if let Some(place) = crate::panel::place_of(sys, sys.screen) {
            return crate::panel::draw(sys, place, &mut self.grid, BODY_TOP);
        }
        let top = term_top(sys.screen);
        let height = self.term_rows(sys.screen);
        let term = &self.terms[sys.screen.min(SCREENS.len() - 1)];
        // The rows of the city, if any, belong to the kernel.
        for row in BODY_TOP..top {
            self.grid.fill(row, 0, COLS, Cell::new(b' ', role::TERM_OUT, term.bg));
        }
        let prompt = sys.screen != LOG;
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
                // at the prompt, a command; on the companion's screen, something to ask it
                // the prompt that waits for a program says so, and how to have it back
                let mut running = FixedStr::<COLS>::new();
                if let Some(job) = crate::jobs::watched(sys, crate::jobs::Watch::Screen).map(|at| &sys.jobs.list()[at]).filter(|job| !job.ended()) {
                    const HOW: &[u8] = b". [Esc] stops it, [Alt]+[B] sends it to the background.";
                    let (name, room) = (job.program.bytes(), COLS - 1 - 8 - HOW.len());
                    running.push_bytes(b"Running ");
                    // a call too long for the row is shown by its name, its
                    // values left out; the first line of a block is cut
                    let bare = name.split(|byte| *byte == b'(').next().unwrap_or(name);
                    let called = bare.len() < name.len() && xila_db::good_name(bare);
                    if name.len() <= room {
                        running.push_bytes(name);
                    } else if called && bare.len() + 5 <= room {
                        running.push_bytes(bare);
                        running.push_bytes(b"(...)");
                    } else {
                        running.push_bytes(&name[..room]);
                    }
                    running.push_bytes(HOW);
                }
                let (mark, editor): (&[u8], &LineEditor) = match sys.screen {
                    COMPANION if sys.companion.stage == crate::companion::Stage::Calling && sys.companion.asker == crate::companion::HERE => (b"(thinking) ", &self.chat),
                    COMPANION => (b"<you> ", &self.chat),
                    _ if !running.is_empty() => (running.bytes(), &self.editor),
                    _ => (shell::prompt(sys, events::SCREEN), &self.editor),
                };
                let mut col = self.grid.text(0, row, mark, role::TERM_PROMPT, term.bg);
                // a line longer than the row slides to the left under the
                // prompt, so that the cursor stays in sight
                let room = COLS - col;
                let typed = editor.bytes();
                let start = editor.cursor().saturating_sub(room - 1);
                let cursor = col + editor.cursor() - start;
                col = self.grid.text(col, row, &typed[start..typed.len().min(start + room)], role::TERM_CMD, term.bg);
                self.grid.fill(row, col, COLS, blank);
                let under = self.grid.get(cursor, row).ch;
                self.grid.put(cursor, row, Cell { ch: under, fg: role::TERM_CMD, bg: term.bg, fx: fx::PULSE });
            } else {
                self.grid.fill(row, 0, COLS, blank);
            }
        }
    }

    fn menu_box(&mut self, sys: &System, selected: usize, top: usize) {
        const WIDTH: usize = 26;
        let (_, count) = Self::listed(sys);
        let g = &mut self.grid;
        let left = COLS - WIDTH - 1;
        let (ink, bg) = (role::TERM_PROMPT, role::TERM_BASE);
        let bottom = top + count + 1;
        g.fill(top, left, left + WIDTH, Cell::new(0xc4, ink, bg));
        g.fill(bottom, left, left + WIDTH, Cell::new(0xc4, ink, bg));
        for (row, l, r) in [(top, 0xda, 0xbf), (bottom, 0xc0, 0xd9)] {
            g.put(left, row, Cell::new(l, ink, bg));
            g.put(left + WIDTH - 1, row, Cell::new(r, ink, bg));
        }
        let mut i = 0;
        crate::panel::screens(sys, &mut |number, name| {
            let row = top + 1 + i;
            let (fg, back) = if i == selected.min(count - 1) { (role::TERM_BASE, role::TERM_CMD) } else { (ink, bg) };
            g.put(left, row, Cell::new(0xb3, ink, bg));
            g.fill(row, left + 1, left + WIDTH - 1, Cell::new(b' ', fg, back));
            // (a long name of a panel is cut where the box ends)
            let mut item = FixedStr::<{ WIDTH - 2 }>::from_bytes(b" [Alt]+[");
            item.push(b'0' + number as u8);
            item.push_bytes(b"]  ");
            item.push_bytes(name);
            g.text(left + 1, row, item.bytes(), fg, back);
            g.put(left + WIDTH - 1, row, Cell::new(0xb3, ink, bg));
            i += 1;
        });
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

    fn alt_digit(digit: u8) -> KeyEvent {
        alt(b'0' + digit, Some(digit))
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
        let (mut ui, mut sys) = (Box::new(Ui::new()), Box::new(System::new()));
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
        let (mut ui, mut sys) = (Box::new(Ui::new()), Box::new(System::new()));
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
        let (mut ui, mut sys) = (Box::new(Ui::new()), Box::new(System::new()));
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
    fn the_companion_has_a_screen_where_one_talks_with_it() {
        let (mut ui, mut sys) = start();
        ui.welcome();
        sys.net.ip = Some([10, 0, 2, 15]);
        // 26 November 2026, 16:34:58
        sys.clock = Some((849_026_098, 0));
        ui.key(&mut sys, alt_digit(2));
        ui.render(&mut sys);
        assert!(row(&ui, 4).contains("COMPANION"));
        assert_eq!(body(&ui), ["The companion is here. Ask what you want to know: [Enter] sends, [Esc] leaves.", "<you>"]);
        // what is typed here is for the companion, not for the prompt
        type_text(&mut ui, &mut sys, "mood calm");
        ui.render(&mut sys);
        assert_eq!(body(&ui)[1], "<you> mood calm");
        ui.key(&mut sys, key(Key::Enter));
        ui.render(&mut sys);
        // no key yet: said in the conversation
        assert_eq!(body(&ui)[1..], ["16:34 <you> mood calm", "      * No key opens the model. Type key set openrouter and the key.", "<you>"]);
        assert_eq!(sys.mood.name(), b"auto", "nothing was run");
        crate::secrets::set(&mut sys.db, b"openrouter", b"sk-or-v1-secret").unwrap();
        // a long question is written under itself
        let question = "which of my notes has the most stars, and how many stars is that, compared with the others?";
        type_text(&mut ui, &mut sys, question);
        ui.key(&mut sys, key(Key::Enter));
        ui.tick(&mut sys, 100);
        ui.render(&mut sys);
        assert_eq!(body(&ui)[3..], ["16:34 <you> which of my notes has the most stars, and how many stars is that,", "            compared with the others?", "(thinking)"]);
        assert_eq!((sys.companion.asker, sys.companion.stage), (crate::companion::HERE, crate::companion::Stage::Calling));
        // one can go and type at the prompt meanwhile; what was being typed here stays
        type_text(&mut ui, &mut sys, "and");
        ui.key(&mut sys, alt_digit(0));
        type_text(&mut ui, &mut sys, "mood calm");
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(sys.mood.name(), b"calm");
        // the answer comes into the conversation, not at the prompt
        crate::companion::heard(&mut sys, Ok((200, br#"{"choices":[{"message":{"content":"\"holidays\" has the most stars: 5. That is two more than rent, and four more than taxes.\nAsk me again."}}]}"#)));
        ui.tick(&mut sys, 6000);
        ui.render(&mut sys);
        assert!(!body(&ui).iter().any(|row| row.contains("holidays")));
        ui.key(&mut sys, alt_digit(2));
        ui.render(&mut sys);
        assert_eq!(body(&ui)[5..], ["16:35 <xila> \"holidays\" has the most stars: 5. That is two more than rent, and", "             four more than taxes.", "             Ask me again.", "<you> and"]);
        // Esc goes back to the overview, as on the other screens
        ui.key(&mut sys, key(Key::Esc));
        assert_eq!(sys.screen, 0);
    }

    #[test]
    fn a_program_asks_the_companion_and_the_answer_is_on_its_screen() {
        let (mut ui, mut sys) = start();
        sys.net.ip = Some([10, 0, 2, 15]);
        crate::secrets::set(&mut sys.db, b"openrouter", b"sk-or-v1-secret").unwrap();
        for line in ["create table expenses (label text, amount int)", "on insert into expenses", "if new.amount > 50", "ask is {new.label} at {new.amount} unusual?", "end", "end", "on notify alerts", "ask what does '{text}' mean?", "end"] {
            ui.run(&mut sys, line.as_bytes());
        }
        // a small expense: nothing is asked
        ui.run(&mut sys, b"insert into expenses values ('coffee', 3)");
        ui.tick(&mut sys, 1);
        assert_eq!((sys.companion.stage, sys.companion.waiting()), (crate::companion::Stage::Idle, 0));
        // a large one, and an alert right after: asked one after the other, each under the name of its program
        ui.run(&mut sys, b"insert into expenses values ('a boat', 9000)");
        ui.run(&mut sys, b"notify alerts 'the disk is full'");
        ui.tick(&mut sys, 1);
        assert_eq!((sys.companion.stage, sys.companion.asker, sys.companion.waiting()), (crate::companion::Stage::Calling, crate::companion::HERE, 1));
        ui.key(&mut sys, alt_digit(2));
        ui.render(&mut sys);
        assert_eq!(body(&ui), ["<on_insert_expenses> is a boat at 9000 unusual?", "(thinking)"]);
        crate::companion::heard(&mut sys, Ok((200, br#"{"choices":[{"message":{"content":"Yes: the others are under ten."}}]}"#)));
        ui.tick(&mut sys, 1);
        ui.render(&mut sys);
        // the answer, then the next question, which was waiting
        assert_eq!(body(&ui)[1..], ["<xila> Yes: the others are under ten.", "<on_notify_alerts> what does 'the disk is full' mean?", "(thinking)"]);
        assert_eq!(sys.companion.waiting(), 0);
        crate::companion::heard(&mut sys, Ok((200, br#"{"choices":[{"message":{"content":"That no more can be kept."}}]}"#)));
        ui.tick(&mut sys, 1);
        ui.render(&mut sys);
        assert_eq!(body(&ui)[3..], ["<xila> That no more can be kept.", "<you>"]);
        // nothing of it went to the prompt's screen, but for what the commands said there
        ui.key(&mut sys, alt_digit(0));
        ui.render(&mut sys);
        assert!(!body(&ui).iter().any(|row| row.contains("under ten") || row.contains("no more can be kept")), "{:?}", body(&ui));
        // more questions than can wait: the ninth is let go, and said so in the log
        for _ in 0..12 {
            ui.run(&mut sys, b"notify alerts 'again'");
        }
        ui.tick(&mut sys, 1);
        ui.tick(&mut sys, 1);
        assert_eq!((sys.companion.stage, sys.companion.waiting()), (crate::companion::Stage::Calling, 8));
        assert_eq!(sys.companion.dropped, 3);
        ui.key(&mut sys, alt_digit(1));
        ui.render(&mut sys);
        assert!(body(&ui).iter().any(|row| row == "on_notify_alerts: Eight questions wait for the companion: this one is let go."), "{:?}", body(&ui));
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

    /// A system with a table of so many things and a panel on it.
    fn with_panel(count: usize) -> (Box<Ui>, Box<System>) {
        let (mut ui, mut sys) = start();
        ui.run(&mut sys, b"create table items (label text, place text, qty int)");
        for first in (0..count).step_by(4) {
            let values: Vec<String> = (first..(first + 4).min(count)).map(|n| format!("('thing {n}', 'shelf {}', {})", n % 4, n * 10)).collect();
            ui.run(&mut sys, format!("insert into items values {}", values.join(", ")).as_bytes());
        }
        for line in ["panel stock on items", "title 'The stock'", "list label, place, qty", "field place 'Where it is'", "field qty 'In stock'", "end"] {
            ui.run(&mut sys, line.as_bytes());
        }
        (ui, sys)
    }

    /// The rows of the list a panel shows, and what it has to say, once
    /// the interface had a tick to read what is to be read.
    fn listed(ui: &mut Ui, sys: &mut System) -> (Vec<String>, String) {
        ui.tick(sys, 0);
        ui.render(sys);
        let last = ui.grid.rows - 1;
        // (without the columns before each row, where an option is typed)
        let rows = (BODY_TOP + 5..last - 2).map(|at| row(ui, at)).filter(|text| !text.is_empty()).map(|text| if text.starts_with(" (") { text } else { text.get(4..).unwrap_or("").to_string() });
        (rows.collect(), row(ui, last - 2).trim().to_string())
    }

    fn things(numbers: core::ops::Range<usize>) -> Vec<String> {
        numbers.map(|n| format!("thing {n}")).collect()
    }

    fn labels(rows: &[String]) -> Vec<String> {
        rows.iter().map(|text| text.trim().split("  ").next().unwrap().to_string()).collect()
    }

    #[test]
    fn a_panel_opens_as_a_screen_of_its_own_and_lists_its_table() {
        let (mut ui, mut sys) = with_panel(3);
        ui.run(&mut sys, b"stock");
        assert_eq!(sys.screen, 3);
        // the screen is the panel's at once; its rows are read at the next tick of the interface
        ui.render(&mut sys);
        assert!(row(&ui, BAR_ROW).contains("STOCK") && row(&ui, BODY_TOP + 4).is_empty());
        ui.tick(&mut sys, 0);
        ui.render(&mut sys);
        let title = row(&ui, BODY_TOP + 1);
        assert!(title.starts_with(" The stock") && title.ends_with("page 1"), "{title}");
        assert_eq!(row(&ui, BODY_TOP + 2), " Find:");
        // the columns under their labels, numbers to the right
        assert_eq!(row(&ui, BODY_TOP + 4), " Opt label    Where it is  In stock");
        assert_eq!(listed(&mut ui, &mut sys).0, [" thing 0  shelf 0             0", " thing 1  shelf 1            10", " thing 2  shelf 2            20"]);
        assert_eq!(row(&ui, ui.grid.rows - 2), " 2=Change  4=Remove  5=Display");
        assert_eq!(row(&ui, ui.grid.rows - 1), " F3=Exit  F5=Refresh  F6=Add  PgUp/PgDn=Pages  Enter=Go  Esc=Overview");
        // Esc shows the overview and leaves the panel open: its name, or its key, goes back to it
        ui.key(&mut sys, key(Key::Esc));
        assert_eq!((sys.screen, crate::panel::place_of(&sys, 3)), (0, Some(0)));
        ui.key(&mut sys, alt_digit(3));
        assert_eq!(sys.screen, 3);
        ui.key(&mut sys, key(Key::Esc));
        ui.run(&mut sys, b"Stock");
        assert_eq!((sys.screen, crate::panel::place_of(&sys, 4)), (3, None));
        // F3 leaves it: its screen is no more
        ui.key(&mut sys, key(Key::F(3)));
        assert_eq!((sys.screen, crate::panel::place_of(&sys, 3)), (0, None));
        ui.key(&mut sys, alt_digit(3));
        assert_eq!(sys.screen, 0);
        ui.render(&mut sys);
        assert!(row(&ui, BAR_ROW).contains("OVERVIEW"));
        // a table with no row, and a panel that says nothing of its columns
        ui.run(&mut sys, b"delete from items");
        ui.run(&mut sys, b"panel plain on items");
        ui.run(&mut sys, b"end");
        ui.run(&mut sys, b"plain");
        ui.tick(&mut sys, 0);
        ui.render(&mut sys);
        assert_eq!((row(&ui, BODY_TOP + 1).split_whitespace().next(), row(&ui, BODY_TOP + 4).as_str()), (Some("plain"), " Opt label  place  qty"));
        assert_eq!(listed(&mut ui, &mut sys), (vec![" (no rows)".to_string()], String::new()));
    }

    #[test]
    fn a_list_is_read_page_by_page_both_ways() {
        let (mut ui, mut sys) = with_panel(100);
        ui.run(&mut sys, b"stock");
        let page = ui.grid.rows - BODY_TOP - 8;
        assert_eq!(labels(&listed(&mut ui, &mut sys).0), things(0..page));
        ui.key(&mut sys, key(Key::PageDown));
        assert_eq!(labels(&listed(&mut ui, &mut sys).0), things(page..2 * page));
        assert!(row(&ui, BODY_TOP + 1).ends_with("page 2"));
        // to the last page, which is not full, and no further
        let mut pages = 2;
        while listed(&mut ui, &mut sys).0.len() == page {
            ui.key(&mut sys, key(Key::PageDown));
            pages += 1;
        }
        assert_eq!(labels(&listed(&mut ui, &mut sys).0), things((pages - 1) * page..100));
        ui.key(&mut sys, key(Key::PageDown));
        let (rows, message) = listed(&mut ui, &mut sys);
        assert_eq!((labels(&rows), message.as_str()), (things((pages - 1) * page..100), "This is the last page."));
        // and back, page by page, to the first
        for back in (0..pages - 1).rev() {
            ui.key(&mut sys, key(Key::PageUp));
            assert_eq!(labels(&listed(&mut ui, &mut sys).0), things(back * page..(back + 1) * page), "page {}", back + 1);
        }
        assert!(row(&ui, BODY_TOP + 1).ends_with("page 1"));
        ui.key(&mut sys, key(Key::PageUp));
        assert_eq!(listed(&mut ui, &mut sys).1, "This is the first page.");
        // the message goes at the next key
        ui.key(&mut sys, key(Key::F(5)));
        assert_eq!(listed(&mut ui, &mut sys).1, "");
        // keys that turn pages wait for the page to be read: two at once are one page
        ui.key(&mut sys, key(Key::PageDown));
        ui.key(&mut sys, key(Key::PageDown));
        ui.key(&mut sys, key(Key::PageUp));
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(labels(&listed(&mut ui, &mut sys).0), things(page..2 * page));
        assert!(row(&ui, BODY_TOP + 1).ends_with("page 2"));
    }

    #[test]
    fn a_list_shows_every_column_of_its_table_and_keeps_numbers_whole() {
        let (mut ui, mut sys) = start();
        ui.run(&mut sys, b"create table wide (a text, b text, c text, d text, e int, f int, g int)");
        let long = "a text of forty-eight characters, as long as one";
        ui.run(&mut sys, format!("insert into wide values ('{long}', '{long}', '{long}', 'short', 123456789012, -7, 0)").as_bytes());
        // the id and the seven others: eight columns, the id read once
        for line in ["panel wide on wide", "list id, a, b, c, d, e, f, g", "end", "wide"] {
            ui.run(&mut sys, line.as_bytes());
        }
        let (rows, message) = listed(&mut ui, &mut sys);
        assert_eq!((rows.len(), message.as_str()), (1, ""));
        // texts give way, numbers are never cut
        let cells: Vec<&str> = rows[0].split_whitespace().collect();
        assert_eq!((cells[0], &cells[cells.len() - 3..]), ("1", &["123456789012", "-7", "0"][..]));
        assert!(rows[0].len() <= COLS - 1);
        // a list of numbers has nothing for the line to find to look in, and says so
        for line in ["panel sums on wide", "list e, f, g", "end"] {
            ui.key(&mut sys, key(Key::Esc));
            ui.run(&mut sys, line.as_bytes());
        }
        ui.run(&mut sys, b"sums");
        listed(&mut ui, &mut sys);
        type_text(&mut ui, &mut sys, "5");
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(listed(&mut ui, &mut sys), (vec!["            e   f  g".to_string(), " 123456789012  -7  0".to_string()][1..].to_vec(), "No listed column is a text: there is nothing to look in.".to_string()));
    }

    #[test]
    fn the_line_to_find_keeps_the_rows_that_hold_what_is_typed() {
        let (mut ui, mut sys) = with_panel(100);
        ui.run(&mut sys, b"stock");
        listed(&mut ui, &mut sys);
        type_text(&mut ui, &mut sys, "thing 7x");
        ui.key(&mut sys, key(Key::Backspace));
        ui.render(&mut sys);
        assert_eq!(row(&ui, BODY_TOP + 2), " Find: thing 7");
        // nothing is looked for until Enter
        assert_eq!(listed(&mut ui, &mut sys).0.len(), ui.grid.rows - BODY_TOP - 8);
        ui.key(&mut sys, key(Key::Enter));
        let mut wanted = vec!["thing 7".to_string()];
        wanted.extend(things(70..80));
        assert_eq!(labels(&listed(&mut ui, &mut sys).0), wanted);
        // in any text column of the list: the places too
        for _ in 0..7 {
            ui.key(&mut sys, key(Key::Backspace));
        }
        type_text(&mut ui, &mut sys, "shelf 3");
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(labels(&listed(&mut ui, &mut sys).0), ["thing 3", "thing 7", "thing 11"].iter().map(|text| text.to_string()).chain((15..100).step_by(4).map(|n| format!("thing {n}"))).take(ui.grid.rows - BODY_TOP - 8).collect::<Vec<_>>());
        // letters beyond ASCII are typed and found as any other
        for _ in 0..7 {
            ui.key(&mut sys, key(Key::Backspace));
        }
        shell::exec(&mut sys, b"insert into items values ('C\x83ble', 'door', 1)", &mut shell::tests::Lines::default());
        type_text(&mut ui, &mut sys, "c");
        ui.key(&mut sys, key(Key::Char(0x83)));
        type_text(&mut ui, &mut sys, "b");
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(listed(&mut ui, &mut sys).0.len(), 1);
        for _ in 0..3 {
            ui.key(&mut sys, key(Key::Backspace));
        }
        type_text(&mut ui, &mut sys, "shelf 3");
        // what no row holds, a quote among it
        for _ in 0..7 {
            ui.key(&mut sys, key(Key::Backspace));
        }
        type_text(&mut ui, &mut sys, "it's 100%");
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(listed(&mut ui, &mut sys), (vec![], "No row holds: it's 100%".to_string()));
        // as much as the line takes, and no more
        type_text(&mut ui, &mut sys, &"x".repeat(60));
        ui.key(&mut sys, key(Key::Enter));
        listed(&mut ui, &mut sys);
        assert_eq!(row(&ui, BODY_TOP + 2).len(), " Find: ".len() + 40);
        // emptied, it shows everything again from the first page
        for _ in 0..40 {
            ui.key(&mut sys, key(Key::Backspace));
        }
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(labels(&listed(&mut ui, &mut sys).0)[0], "thing 0");
    }

    #[test]
    fn a_list_follows_its_table() {
        let (mut ui, mut sys) = with_panel(2);
        ui.run(&mut sys, b"stock");
        ui.tick(&mut sys, 150);
        assert_eq!(listed(&mut ui, &mut sys).0.len(), 2);
        // a row added from the serial port: shown within a second, without a key
        sys.session = events::SERIAL;
        shell::exec(&mut sys, b"insert into items values ('from afar', 'door', 1)", &mut shell::tests::Lines::default());
        sys.session = events::SCREEN;
        ui.tick(&mut sys, 1);
        ui.tick(&mut sys, 120);
        assert_eq!(labels(&listed(&mut ui, &mut sys).0), ["thing 0", "thing 1", "from afar"]);
        // its table changes under it: a listed column goes, then the table
        sys.session = events::SERIAL;
        shell::exec(&mut sys, b"alter table items drop column place", &mut shell::tests::Lines::default());
        ui.key(&mut sys, key(Key::F(5)));
        assert_eq!(listed(&mut ui, &mut sys), (vec![], "Unknown column: place".to_string()));
        shell::exec(&mut sys, b"drop table items", &mut shell::tests::Lines::default());
        sys.session = events::SCREEN;
        ui.key(&mut sys, key(Key::F(5)));
        let gone = (vec![], "Unknown table: items. Type tables to list them.".to_string());
        assert_eq!(listed(&mut ui, &mut sys), gone);
        // a key that reads nothing leaves what is wrong said
        ui.key(&mut sys, key(Key::Left));
        type_text(&mut ui, &mut sys, "x");
        ui.key(&mut sys, key(Key::Backspace));
        assert_eq!(listed(&mut ui, &mut sys), gone);
        // the table is made again: the list comes back, and nothing more is said of the failure
        ui.run(&mut sys, b"create table items (label text, place text, qty int)");
        ui.run(&mut sys, b"insert into items values ('back', 'here', 1)");
        ui.tick(&mut sys, 150);
        assert_eq!(listed(&mut ui, &mut sys), (vec![" back   here                1".to_string()], String::new()));
        ui.key(&mut sys, key(Key::PageDown));
        ui.key(&mut sys, key(Key::F(3)));
        assert_eq!(sys.screen, 0);
    }

    /// A system with a table that asks things of its rows, and a panel
    /// on it that is open, its list read.
    fn with_form() -> (Box<Ui>, Box<System>) {
        let (mut ui, mut sys) = start();
        for line in [
            "create table kinds (name text)",
            "insert into kinds values ('cables'), ('tools'), ('measure')",
            "create table things (label text not null unique, kind text, state text default 'good' check (state = 'good' or state = 'worn'), qty int default 1, seen text)",
            "insert into things values ('Oscilloscope', 'measure', 'good', 1, '2026-11-01'), ('Soldering iron', 'tools', 'worn', 2, '2026-11-02')",
            "panel things on things",
            "title 'Things'",
            "list label, kind, state, qty",
            "field label 'Label'",
            "field kind 'Kind' among select name from kinds",
            "field state 'State' among 'good', 'worn', 'it''s gone'",
            "field qty 'In stock'",
            "field seen 'Last seen' shown",
            "end",
            "things",
        ] {
            ui.run(&mut sys, line.as_bytes());
        }
        listed(&mut ui, &mut sys);
        (ui, sys)
    }

    /// The fields of the form a panel shows, as `label: value`, and what it says.
    fn form(ui: &mut Ui, sys: &mut System) -> (Vec<String>, String) {
        ui.tick(sys, 0);
        ui.render(sys);
        let last = ui.grid.rows - 1;
        let fields = (BODY_TOP + 3..last - 2).map(|at| row(ui, at)).filter(|text| !text.is_empty());
        let field = |text: String| {
            let (label, value) = text.split_once(" :").unwrap_or((&text, ""));
            format!("{}: {}", label.trim().trim_end_matches(['.', ' ']), value.trim()).trim_end().to_string()
        };
        (fields.map(field).collect(), row(ui, last - 2).trim().to_string())
    }

    /// The rows of a list with one space between their columns.
    fn squeezed(listed: (Vec<String>, String)) -> (Vec<String>, String) {
        (listed.0.iter().map(|text| text.split_whitespace().collect::<Vec<_>>().join(" ")).collect(), listed.1)
    }

    fn keys(ui: &mut Ui, sys: &mut System, pressed: &[Key]) {
        for pressed in pressed {
            ui.key(sys, key(*pressed));
            ui.tick(sys, 0);
        }
    }

    fn typed(ui: &mut Ui, sys: &mut System, line: &str) -> Vec<String> {
        let mut out = shell::tests::Lines::default();
        shell::exec(sys, line.as_bytes(), &mut out);
        ui.dirty = true;
        out.lines
    }

    #[test]
    fn a_row_is_added_through_the_form() {
        let (mut ui, mut sys) = with_form();
        keys(&mut ui, &mut sys, &[Key::F(6)]);
        ui.render(&mut sys);
        assert!(row(&ui, BODY_TOP + 1).ends_with("a row to add"));
        assert_eq!(form(&mut ui, &mut sys), (vec!["Label:".to_string(), "Kind:".into(), "State:".into(), "In stock:".into(), "Last seen:".into()], String::new()));
        assert_eq!(row(&ui, ui.grid.rows - 1), " Enter=Save  Tab=Next field  F4=Values  F12=Cancel  F3=Exit");
        // typed field by field; a field that is only shown is passed over
        type_text(&mut ui, &mut sys, "Cat6 cable 5m");
        keys(&mut ui, &mut sys, &[Key::Tab]);
        type_text(&mut ui, &mut sys, "cables");
        keys(&mut ui, &mut sys, &[Key::Tab, Key::Tab]);
        type_text(&mut ui, &mut sys, "6 * 2");
        keys(&mut ui, &mut sys, &[Key::Tab]);
        type_text(&mut ui, &mut sys, "!");
        // (round to the first field again, where the cursor is where it was left)
        assert_eq!(form(&mut ui, &mut sys).0, ["Label: Cat6 cable 5m!", "Kind: cables", "State:", "In stock: 6 * 2", "Last seen:"]);
        // editing in a field: the keys of a line
        keys(&mut ui, &mut sys, &[Key::Backspace, Key::End, Key::Left, Key::Delete, Key::Home, Key::Right]);
        type_text(&mut ui, &mut sys, "+");
        assert_eq!(form(&mut ui, &mut sys).0[0], "Label: C+at6 cable 5");
        keys(&mut ui, &mut sys, &[Key::Backspace, Key::End]);
        type_text(&mut ui, &mut sys, "m");
        // Enter writes it: what was not typed takes the value by default, a calculation is worked out
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(squeezed(listed(&mut ui, &mut sys)), (vec!["Oscilloscope measure good 1".to_string(), "Soldering iron tools worn 2".into(), "Cat6 cable 5m cables good 12".into()], "1 row added to things, with id 3.".to_string()));
        assert_eq!(typed(&mut ui, &mut sys, "select state, qty, seen is null from things where id = 3"), ["   state  qty  seenisnull", "   good   12   1"]);
        // what the table refuses is said in its words, and the form stays as it was typed
        keys(&mut ui, &mut sys, &[Key::F(6)]);
        type_text(&mut ui, &mut sys, "Oscilloscope");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        let (fields, message) = form(&mut ui, &mut sys);
        assert_eq!((fields[0].as_str(), message.as_str()), ("Label: Oscilloscope", "Column label takes no value twice: row 1 has that one."));
        for _ in 0..12 {
            keys(&mut ui, &mut sys, &[Key::Backspace]);
        }
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(form(&mut ui, &mut sys).1, "Nothing was typed: there is no row to add.");
        keys(&mut ui, &mut sys, &[Key::Tab]);
        type_text(&mut ui, &mut sys, "tools");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(form(&mut ui, &mut sys).1, "Column label must have a value.");
        keys(&mut ui, &mut sys, &[Key::Up]);
        type_text(&mut ui, &mut sys, "Tape");
        keys(&mut ui, &mut sys, &[Key::Tab, Key::Tab]);
        type_text(&mut ui, &mut sys, "lost");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert!(form(&mut ui, &mut sys).1.contains("state = 'good' or state = 'worn'"));
        keys(&mut ui, &mut sys, &[Key::Tab]);
        type_text(&mut ui, &mut sys, "many");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(form(&mut ui, &mut sys).1, "In stock takes a number, or a calculation.");
        // F12 gives it up: nothing was added
        keys(&mut ui, &mut sys, &[Key::F(12)]);
        assert_eq!(squeezed(listed(&mut ui, &mut sys)), (vec!["Oscilloscope measure good 1".to_string(), "Soldering iron tools worn 2".into(), "Cat6 cable 5m cables good 12".into()], "Nothing was changed.".to_string()));
    }

    #[test]
    fn a_row_is_shown_changed_and_removed_from_the_list() {
        let (mut ui, mut sys) = with_form();
        // down from the line to find to the rows; Enter alone shows the row
        keys(&mut ui, &mut sys, &[Key::Down, Key::Down, Key::Enter]);
        ui.render(&mut sys);
        assert!(row(&ui, BODY_TOP + 1).ends_with("row 2"));
        assert_eq!(form(&mut ui, &mut sys).0, ["Label: Soldering iron", "Kind: tools", "State: worn", "In stock: 2", "Last seen: 2026-11-02"]);
        assert_eq!(row(&ui, ui.grid.rows - 1), " Enter=Back  F12=Back  F3=Exit");
        // nothing is typed in a row that is shown
        type_text(&mut ui, &mut sys, "x");
        assert_eq!(form(&mut ui, &mut sys).0[0], "Label: Soldering iron");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        // 5 does the same; 2 changes; anything else says what can be typed there
        type_text(&mut ui, &mut sys, "9");
        assert_eq!(listed(&mut ui, &mut sys).1, "Before a row: 2 changes it, 4 removes it, 5 shows it.");
        type_text(&mut ui, &mut sys, "2");
        ui.render(&mut sys);
        assert_eq!(&row(&ui, BODY_TOP + 6)[..4], "  2 ");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        ui.render(&mut sys);
        assert!(row(&ui, BODY_TOP + 1).ends_with("row 2, changed"));
        keys(&mut ui, &mut sys, &[Key::Tab, Key::Tab, Key::Tab]);
        keys(&mut ui, &mut sys, &[Key::Backspace]);
        type_text(&mut ui, &mut sys, "7");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(squeezed(listed(&mut ui, &mut sys)), (vec!["Oscilloscope measure good 1".to_string(), "Soldering iron tools worn 7".into()], "1 row changed in things.".to_string()));
        // what is only shown was not written over
        assert_eq!(typed(&mut ui, &mut sys, "select seen from things where id = 2")[1].trim(), "2026-11-02");
        // 4 asks before it removes; F12 keeps the row
        type_text(&mut ui, &mut sys, "4");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(listed(&mut ui, &mut sys).1, "Remove row 2? [Enter] removes it, [F12] keeps it.");
        assert_eq!(row(&ui, ui.grid.rows - 1), " Enter=Remove  F12=Keep");
        keys(&mut ui, &mut sys, &[Key::F(12)]);
        assert_eq!(squeezed(listed(&mut ui, &mut sys)), (vec!["Oscilloscope measure good 1".to_string(), "Soldering iron tools worn 7".into()], "Nothing was removed.".to_string()));
        type_text(&mut ui, &mut sys, "4");
        keys(&mut ui, &mut sys, &[Key::Enter, Key::Enter]);
        assert_eq!(squeezed(listed(&mut ui, &mut sys)), (vec!["Oscilloscope measure good 1".to_string()], "1 row removed from things.".to_string()));
        // the keys stay on a row that is there; Up goes back to the line to find
        keys(&mut ui, &mut sys, &[Key::Up, Key::Up]);
        type_text(&mut ui, &mut sys, "osc");
        ui.render(&mut sys);
        assert_eq!(row(&ui, BODY_TOP + 2), " Find: osc");
        // a row that went while its form was typed: said, and nothing is written
        keys(&mut ui, &mut sys, &[Key::Down]);
        type_text(&mut ui, &mut sys, "2");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        typed(&mut ui, &mut sys, "delete from things");
        type_text(&mut ui, &mut sys, "!");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(squeezed(listed(&mut ui, &mut sys)), (vec![], "This row is no longer there.".to_string()));
    }

    #[test]
    fn what_is_typed_in_a_field_is_one_value_and_nothing_else() {
        let (mut ui, mut sys) = with_form();
        typed(&mut ui, &mut sys, "let qty = 500");
        let untouched = |ui: &mut Ui, sys: &mut System| typed(ui, sys, "select label, state, qty, seen from things order by id");
        let before = untouched(&mut ui, &mut sys);
        // in a number: no second column, no condition, no name of the prompt, no column of the row
        for attempt in ["1, label = 'hacked'", "1 where 1 = 1", "qty", "qty + 1", "1); drop table things", "(select 1)", "'12'", "1 2"] {
            keys(&mut ui, &mut sys, &[Key::Down, Key::Down]);
            type_text(&mut ui, &mut sys, "2");
            keys(&mut ui, &mut sys, &[Key::Enter, Key::Tab, Key::Tab, Key::Tab, Key::Backspace]);
            type_text(&mut ui, &mut sys, attempt);
            keys(&mut ui, &mut sys, &[Key::Enter]);
            assert_eq!(form(&mut ui, &mut sys).1, "In stock takes a number, or a calculation.", "{attempt}");
            keys(&mut ui, &mut sys, &[Key::F(12), Key::Up, Key::Up]);
            assert_eq!(untouched(&mut ui, &mut sys), before, "{attempt}");
        }
        // in a text: whatever is typed is the text
        for (attempt, kept) in [("it's, seen = 'x'", "it's, seen = 'x'"), ("a' where 1 = 1 --", "a' where 1 = 1 --"), ("'); drop table things", "'); drop table things"), ("  spaced  ", "spaced")] {
            keys(&mut ui, &mut sys, &[Key::Down, Key::Down]);
            type_text(&mut ui, &mut sys, "2");
            keys(&mut ui, &mut sys, &[Key::Enter, Key::Tab]);
            for _ in 0..40 {
                keys(&mut ui, &mut sys, &[Key::Backspace]);
            }
            type_text(&mut ui, &mut sys, attempt);
            keys(&mut ui, &mut sys, &[Key::Enter]);
            assert_eq!(listed(&mut ui, &mut sys).1, "1 row changed in things.", "{attempt}");
            let row = typed(&mut ui, &mut sys, "select kind = 'x', seen from things where id = 2");
            assert_eq!(row[1].split_whitespace().last(), Some("2026-11-02"), "{attempt}");
            sys.session = events::SERIAL;
            sys.vars[events::SERIAL].set(b"wanted", crate::sql::Value::text(kept.as_bytes()));
            assert_eq!(typed(&mut ui, &mut sys, "select count(*) from things where id = 2 and kind = wanted")[1].trim(), "1", "{attempt}");
            sys.session = events::SCREEN;
            keys(&mut ui, &mut sys, &[Key::Up, Key::Up]);
        }
        assert_eq!(typed(&mut ui, &mut sys, "select count(*) from things")[1].trim(), "2");
        // a decimal comes back as it was written, and a calculation is worked out by the form
        typed(&mut ui, &mut sys, "create table prices (label text, price decimal(8,2))");
        typed(&mut ui, &mut sys, "insert into prices values ('cable', 12.50)");
        for line in ["panel prices on prices", "end"] {
            ui.key(&mut sys, key(Key::Esc));
            ui.run(&mut sys, line.as_bytes());
        }
        ui.run(&mut sys, b"prices");
        listed(&mut ui, &mut sys);
        keys(&mut ui, &mut sys, &[Key::Down]);
        type_text(&mut ui, &mut sys, "2");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(form(&mut ui, &mut sys).0, ["label: cable", "price: 12.50"]);
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(typed(&mut ui, &mut sys, "select price from prices")[1].trim(), "12.50");
        type_text(&mut ui, &mut sys, "2");
        keys(&mut ui, &mut sys, &[Key::Enter, Key::Tab]);
        type_text(&mut ui, &mut sys, " * 2 + 0.25");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(typed(&mut ui, &mut sys, "select price from prices")[1].trim(), "25.25");
    }

    #[test]
    fn a_change_writes_what_was_changed_and_nothing_else() {
        let (mut ui, mut sys) = start();
        for line in ["create table notes (label text, note text, lead text, qty int)", "insert into notes values ('one', '', '  lead', 1), ('two', '', 'x', 2), ('three', '', 'x', 3)", "panel notes on notes", "end", "notes"] {
            ui.run(&mut sys, line.as_bytes());
        }
        listed(&mut ui, &mut sys);
        let held = |ui: &mut Ui, sys: &mut System| typed(ui, sys, "select note = '', length(lead), qty from notes where id = 1")[1].split_whitespace().map(String::from).collect::<Vec<_>>();
        // Enter with nothing typed writes nothing, and says so
        keys(&mut ui, &mut sys, &[Key::Down]);
        type_text(&mut ui, &mut sys, "2");
        keys(&mut ui, &mut sys, &[Key::Enter, Key::Enter]);
        assert_eq!(listed(&mut ui, &mut sys).1, "Nothing was changed.");
        // one field is changed: an empty text stays one, spaces stay, and so does what another wrote meanwhile
        type_text(&mut ui, &mut sys, "2");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        form(&mut ui, &mut sys);
        typed(&mut ui, &mut sys, "update notes set label = 'ONE' where id = 1");
        keys(&mut ui, &mut sys, &[Key::Up]);
        type_text(&mut ui, &mut sys, "0");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(listed(&mut ui, &mut sys).1, "1 row changed in notes.");
        assert_eq!(held(&mut ui, &mut sys), ["1", "6", "10"]);
        assert_eq!(typed(&mut ui, &mut sys, "select label from notes where id = 1")[1].trim(), "ONE");
        // a field that is emptied is emptied
        type_text(&mut ui, &mut sys, "2");
        keys(&mut ui, &mut sys, &[Key::Enter, Key::Tab, Key::Tab]);
        for _ in 0..6 {
            keys(&mut ui, &mut sys, &[Key::Backspace]);
        }
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(typed(&mut ui, &mut sys, "select lead is null from notes where id = 1")[1].trim(), "1");
        // what is typed before a row stays with that row when the rows above it go
        keys(&mut ui, &mut sys, &[Key::Down]);
        type_text(&mut ui, &mut sys, "4");
        typed(&mut ui, &mut sys, "delete from notes where id = 1");
        ui.tick(&mut sys, 150);
        ui.render(&mut sys);
        assert_eq!((row(&ui, BODY_TOP + 5)[..4].to_string(), squeezed(listed(&mut ui, &mut sys)).0), ("  4 ".to_string(), vec!["two x 2".to_string(), "three x 3".into()]));
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(listed(&mut ui, &mut sys).1, "Remove row 2? [Enter] removes it, [F12] keeps it.");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(squeezed(listed(&mut ui, &mut sys)), (vec!["three x 3".to_string()], "1 row removed from notes.".to_string()));
        // ...and goes when that row does
        type_text(&mut ui, &mut sys, "4");
        typed(&mut ui, &mut sys, "delete from notes");
        ui.tick(&mut sys, 150);
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(typed(&mut ui, &mut sys, "insert into notes values ('four', '', '', 4)").len(), 1);
        ui.tick(&mut sys, 150);
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(typed(&mut ui, &mut sys, "select count(*) from notes")[1].trim(), "1");
        // a save that was asked for is done though another screen is shown before the tick
        keys(&mut ui, &mut sys, &[Key::F(12), Key::F(6)]);
        type_text(&mut ui, &mut sys, "five");
        ui.key(&mut sys, key(Key::Enter));
        ui.key(&mut sys, key(Key::Esc));
        ui.tick(&mut sys, 0);
        assert_eq!((sys.screen, typed(&mut ui, &mut sys, "select count(*) from notes where label = 'five'")[1].trim().to_string()), (0, "1".to_string()));
        // a table that cannot be read leaves the keys on no row
        ui.key(&mut sys, alt_digit(3));
        keys(&mut ui, &mut sys, &[Key::Down]);
        typed(&mut ui, &mut sys, "drop table notes");
        keys(&mut ui, &mut sys, &[Key::F(5)]);
        type_text(&mut ui, &mut sys, "4");
        ui.render(&mut sys);
        assert_eq!(row(&ui, BODY_TOP + 2), " Find: 4");
    }

    #[test]
    fn a_panel_that_only_shows_takes_nothing_typed() {
        let (mut ui, mut sys) = start();
        for line in ["create table seen (label text, qty int)", "insert into seen values ('one', 1)", "panel seen on seen", "field label 'A label of thirty-two characters' shown", "field qty 'N' shown", "end", "seen"] {
            ui.run(&mut sys, line.as_bytes());
        }
        listed(&mut ui, &mut sys);
        keys(&mut ui, &mut sys, &[Key::Down]);
        type_text(&mut ui, &mut sys, "2");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        type_text(&mut ui, &mut sys, "zz");
        keys(&mut ui, &mut sys, &[Key::F(4)]);
        let (fields, message) = form(&mut ui, &mut sys);
        assert_eq!((fields[1].as_str(), message.as_str()), ("N: 1", ""));
        // a long label leaves the value its room on the row
        assert!(row(&ui, BODY_TOP + 3).ends_with(" : one") && row(&ui, BODY_TOP + 3).len() <= 40);
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(form(&mut ui, &mut sys).1, "This panel has no field that is typed.");
    }

    #[test]
    fn what_a_field_may_hold_is_picked_with_f4() {
        let (mut ui, mut sys) = with_form();
        keys(&mut ui, &mut sys, &[Key::F(6), Key::F(4)]);
        assert_eq!(form(&mut ui, &mut sys).1, "This field has no list of values to pick from.");
        // from a table: the values of a column
        keys(&mut ui, &mut sys, &[Key::Tab, Key::F(4)]);
        ui.render(&mut sys);
        assert!(row(&ui, BODY_TOP + 1).ends_with("Kind"));
        assert_eq!((BODY_TOP + 3..BODY_TOP + 6).map(|at| row(&ui, at).trim().to_string()).collect::<Vec<_>>(), ["cables", "tools", "measure"]);
        assert_eq!(row(&ui, ui.grid.rows - 1), " Enter=Pick  Up/Down=Move  F12=Back");
        keys(&mut ui, &mut sys, &[Key::Down, Key::Down, Key::Down, Key::Up, Key::Enter]);
        assert_eq!(form(&mut ui, &mut sys).0[1], "Kind: tools");
        // written there: texts between quotes, a quote among them
        keys(&mut ui, &mut sys, &[Key::Tab, Key::F(4), Key::Down, Key::Down, Key::Enter]);
        assert_eq!(form(&mut ui, &mut sys).0[2], "State: it's gone");
        // asked again, the keys are on the value the field holds; F12 leaves it as it is
        keys(&mut ui, &mut sys, &[Key::F(4), Key::Up, Key::F(12)]);
        assert_eq!(form(&mut ui, &mut sys).0[2], "State: it's gone");
        keys(&mut ui, &mut sys, &[Key::F(4), Key::Up, Key::Enter]);
        assert_eq!(form(&mut ui, &mut sys).0[2], "State: worn");
        // the table still decides: what is picked is written as what is typed
        keys(&mut ui, &mut sys, &[Key::Up, Key::Up]);
        type_text(&mut ui, &mut sys, "Pliers");
        keys(&mut ui, &mut sys, &[Key::Enter]);
        assert_eq!(listed(&mut ui, &mut sys).1, "1 row added to things, with id 3.");
        assert_eq!(typed(&mut ui, &mut sys, "select kind, state from things where id = 3")[1].split_whitespace().collect::<Vec<_>>(), ["tools", "worn"]);
        // more values than are proposed: said, sorted or not
        keys(&mut ui, &mut sys, &[Key::F(12)]);
        for first in (0..48).step_by(8) {
            let values: Vec<String> = (first..first + 8).map(|n| format!("('kind {n:02}')")).collect();
            typed(&mut ui, &mut sys, &format!("insert into kinds values {}", values.join(", ")));
        }
        keys(&mut ui, &mut sys, &[Key::F(6), Key::Tab, Key::F(4)]);
        assert_eq!(form(&mut ui, &mut sys).1, "The first 40 values are proposed: there are more.");
        keys(&mut ui, &mut sys, &[Key::F(12), Key::F(12)]);
        for line in ["panel sorted on things", "field kind 'Kind' among select name from kinds order by name", "end"] {
            typed(&mut ui, &mut sys, line);
        }
        ui.run(&mut sys, b"sorted");
        listed(&mut ui, &mut sys);
        keys(&mut ui, &mut sys, &[Key::F(6), Key::F(4)]);
        assert_eq!(form(&mut ui, &mut sys).1, "A list that is sorted proposes 32 values at most: this one has more.");
        keys(&mut ui, &mut sys, &[Key::F(3)]);
        ui.key(&mut sys, alt_digit(3));
        // a list of values that is gone says so
        typed(&mut ui, &mut sys, "drop table kinds");
        keys(&mut ui, &mut sys, &[Key::F(6), Key::Tab, Key::F(4)]);
        assert_eq!(form(&mut ui, &mut sys).1, "Unknown table: kinds. Type tables to list them.");
    }

    #[test]
    fn four_panels_are_open_at_one_time_each_a_screen() {
        let (mut ui, mut sys) = with_panel(3);
        for name in ["second", "third", "fourth", "fifth"] {
            ui.run(&mut sys, format!("panel {name} on items").as_bytes());
            ui.run(&mut sys, b"end");
        }
        for (name, screen) in [("stock", 3), ("second", 4), ("third", 5), ("fourth", 6)] {
            ui.key(&mut sys, key(Key::Esc));
            ui.run(&mut sys, name.as_bytes());
            assert_eq!(sys.screen, screen, "{name}");
        }
        ui.key(&mut sys, key(Key::Esc));
        ui.run(&mut sys, b"fifth");
        ui.render(&mut sys);
        assert_eq!((sys.screen, body(&ui).iter().rev().nth(1).unwrap().as_str()), (0, "Four panels are open: leave one with [F3]."));
        // the menu and the commands know them
        ui.key(&mut sys, alt(b'e', None));
        ui.render(&mut sys);
        let menu: Vec<String> = (TOP + 1..TOP + 8).map(|at| row(&ui, at).rsplit("[Alt]+").next().unwrap().trim_end_matches('~').trim().to_string()).collect();
        assert_eq!(menu, ["[0]  overview", "[1]  boot log", "[2]  companion", "[3]  stock", "[4]  second", "[5]  third", "[6]  fourth"]);
        for _ in 0..5 {
            ui.key(&mut sys, key(Key::Down));
        }
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(sys.screen, 5);
        ui.key(&mut sys, key(Key::Esc));
        ui.run(&mut sys, b"select id, name, shown from screens where id > 2");
        ui.render(&mut sys);
        let shown = body(&ui);
        assert_eq!(shown[shown.len() - 5..shown.len() - 1].iter().map(|text| text.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>(), ["3 stock", "4 second", "5 third", "6 fourth"]);
        ui.run(&mut sys, b"screen second");
        assert_eq!(sys.screen, 4);
        // one is left: its place is free, the others keep theirs
        ui.key(&mut sys, key(Key::F(3)));
        ui.key(&mut sys, alt_digit(4));
        assert_eq!(sys.screen, 0);
        ui.key(&mut sys, alt_digit(6));
        assert_eq!(sys.screen, 6);
        ui.key(&mut sys, key(Key::Esc));
        ui.run(&mut sys, b"fifth");
        assert_eq!(sys.screen, 4);
        // a panel that is dropped while it is open is left; kept again, it is read anew
        ui.key(&mut sys, key(Key::Esc));
        ui.run(&mut sys, b"drop panel fifth");
        assert_eq!(crate::panel::place_of(&sys, 4), None);
        for line in ["panel stock on items", "title 'Renamed'", "end"] {
            ui.run(&mut sys, line.as_bytes());
        }
        ui.key(&mut sys, alt_digit(3));
        ui.render(&mut sys);
        assert!(row(&ui, BODY_TOP + 1).starts_with(" Renamed"));
    }

    #[test]
    fn a_panel_opens_at_the_screen_of_the_machine_only() {
        let (mut ui, mut sys) = with_panel(3);
        let refused = |sys: &mut System| {
            let mut out = shell::tests::Lines::default();
            let ok = shell::exec(sys, b"stock", &mut out);
            (ok, out.lines)
        };
        let nowhere = (false, vec!["A panel opens at the screen of the machine: stock.".to_string()]);
        for session in [events::SERIAL, events::NETWORK, events::BACKGROUND] {
            sys.session = session;
            assert_eq!(refused(&mut sys), nowhere, "session {session}");
        }
        sys.session = events::SCREEN;
        sys.acting = true;
        assert_eq!(refused(&mut sys), nowhere);
        sys.acting = false;
        assert_eq!((sys.screen, crate::panel::place_of(&sys, 3)), (0, None));
        // a name that is no panel is still an unknown command, and a panel takes no word after it
        ui.run(&mut sys, b"stocks");
        ui.run(&mut sys, b"stock now");
        ui.render(&mut sys);
        assert_eq!(body(&ui).iter().rev().nth(1).unwrap(), "Unknown command: stock");
        // a code over a panel: any key puts it away, and the panel has not moved
        ui.run(&mut sys, b"stock");
        ui.key(&mut sys, key(Key::PageDown));
        sys.code = crate::qr::encode(b"x", 5);
        ui.key(&mut sys, key(Key::F(3)));
        assert_eq!(sys.screen, 3);
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

    /// An interface and a system with rooms for jobs, and a program that counts to `to`.
    fn with_jobs() -> (Box<Ui>, Box<System>) {
        let (mut ui, mut sys) = (Box::new(Ui::new()), Box::new(System::new()));
        crate::machine::tests::rooms(&mut sys, 3);
        for line in ["program count(to)", "let n = 0", "while n < to", "let n = n + 1", "if n % 100 = 0", "say n", "end", "end", "say 'counted'", "end", "let to = 300"] {
            ui.run(&mut sys, line.as_bytes());
        }
        (ui, sys)
    }

    /// The rows of the screen that hold something, as text.
    fn rows(ui: &mut Ui, sys: &mut System) -> Vec<String> {
        ui.render(sys);
        (0..ui.grid.rows).map(|at| row(ui, at)).filter(|text| !text.trim().is_empty()).collect()
    }

    #[test]
    fn the_screen_waits_for_a_program_and_lives() {
        let (mut ui, mut sys) = with_jobs();
        type_text(&mut ui, &mut sys, "count(to)");
        ui.key(&mut sys, key(Key::Enter));
        // it has not run: it is the job the screen waits for
        assert!(crate::jobs::watched(&sys, crate::jobs::Watch::Screen).is_some());
        let shown = rows(&mut ui, &mut sys);
        assert_eq!(shown.last().unwrap().trim_end(), "Running count(300). [Esc] stops it, [Alt]+[B] sends it to the background.");
        assert!(!shown.iter().any(|text| text.trim() == "100"));
        // what is typed meanwhile is not taken, and nothing is run twice
        type_text(&mut ui, &mut sys, "say 'typed too soon'");
        ui.key(&mut sys, key(Key::Enter));
        assert!(ui.editor.bytes().is_empty());
        // it runs by turns while the ticks pass, and what it says is shown as it comes
        let level = ui.grid.pulse;
        let mut seen = Vec::new();
        for _ in 0..5000 {
            ui.tick(&mut sys, 1);
            ui.work(&mut sys, &mut || true);
            let now = rows(&mut ui, &mut sys);
            if now.iter().any(|text| text.trim() == "100") && !now.iter().any(|text| text.trim() == "300") && seen.is_empty() {
                seen = now;
            }
            if crate::jobs::watched(&sys, crate::jobs::Watch::Screen).is_none() {
                break;
            }
        }
        assert!(!seen.is_empty(), "a line it said was on the screen before it ended");
        assert!(seen.last().unwrap().starts_with("Running count(300)."));
        assert_ne!(ui.grid.pulse, level, "the cursor still moved");
        // ended: the prompt is back, and the keyboard is the prompt's again
        let shown = rows(&mut ui, &mut sys);
        assert_eq!(shown[shown.len() - 2].trim(), "counted");
        assert!(shown.last().unwrap().starts_with(core::str::from_utf8(shell::PROMPT).unwrap()));
        assert!(!shown.iter().any(|text| text.contains("typed too soon")));
        // what it counted with was its own: the prompt never had that name
        type_text(&mut ui, &mut sys, "say n");
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(rows(&mut ui, &mut sys).iter().rev().nth(1).unwrap().trim(), "Unknown name: n");
        type_text(&mut ui, &mut sys, "say to");
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(rows(&mut ui, &mut sys).iter().rev().nth(1).unwrap().trim(), "300");
        ui.run(&mut sys, b"let n = 300");
        // a single line that is no program is not a job: it has run when Enter comes back
        assert!(crate::jobs::watched(&sys, crate::jobs::Watch::Screen).is_none());
        // a block typed at the prompt is one
        for line in ["while n < 400", "let n = n + 1", "end"] {
            type_text(&mut ui, &mut sys, line);
            ui.key(&mut sys, key(Key::Enter));
        }
        assert!(rows(&mut ui, &mut sys).last().unwrap().starts_with("Running while n < 400."));
        for _ in 0..5000 {
            ui.work(&mut sys, &mut || true);
        }
        type_text(&mut ui, &mut sys, "say n");
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(rows(&mut ui, &mut sys).iter().rev().nth(1).unwrap().trim(), "400");
    }

    #[test]
    fn a_call_too_long_for_the_edit_row_is_shown_by_its_name() {
        let (mut ui, mut sys) = with_jobs();
        type_text(&mut ui, &mut sys, "count(1000000000000)");
        ui.key(&mut sys, key(Key::Enter));
        let shown = rows(&mut ui, &mut sys);
        assert_eq!(shown.last().unwrap().trim_end(), "Running count(...). [Esc] stops it, [Alt]+[B] sends it to the background.");
        ui.key(&mut sys, key(Key::Esc));
        ui.work(&mut sys, &mut || true);
        assert!(rows(&mut ui, &mut sys).last().unwrap().starts_with(core::str::from_utf8(shell::PROMPT).unwrap()));
        // the first line of a block typed is no call: it is cut where the row ends
        ui.run(&mut sys, b"let n = 0");
        for line in ["while n < (1 + 2) * 1000000", "let n = n + 1", "end"] {
            type_text(&mut ui, &mut sys, line);
            ui.key(&mut sys, key(Key::Enter));
        }
        assert!(rows(&mut ui, &mut sys).last().unwrap().starts_with("Running while n < (1 + 2. [Esc] stops it"));
        ui.key(&mut sys, key(Key::Esc));
    }

    #[test]
    fn esc_stops_it_and_alt_b_sends_it_aside() {
        let (mut ui, mut sys) = with_jobs();
        ui.run(&mut sys, b"let to = 1000000");
        type_text(&mut ui, &mut sys, "count(to)");
        ui.key(&mut sys, key(Key::Enter));
        for _ in 0..50 {
            ui.work(&mut sys, &mut || true);
        }
        // Esc stops it, and says where it was
        ui.key(&mut sys, key(Key::Esc));
        ui.work(&mut sys, &mut || true);
        let shown = rows(&mut ui, &mut sys);
        let said = shown[shown.len() - 2].trim();
        assert!(said.starts_with("Stopped at line ") && said.ends_with(" of count."), "{said}");
        assert!(shown.last().unwrap().starts_with(core::str::from_utf8(shell::PROMPT).unwrap()));
        assert_eq!(sys.jobs.list()[0].state, crate::jobs::State::Stopped);
        assert_eq!(sys.screen, 0);
        // Alt+B sends it to the background: the prompt is back and the job goes on
        type_text(&mut ui, &mut sys, "count(to)");
        ui.key(&mut sys, key(Key::Enter));
        ui.work(&mut sys, &mut || true);
        ui.key(&mut sys, alt(b'b', None));
        let shown = rows(&mut ui, &mut sys);
        assert_eq!(shown[shown.len() - 2].trim(), "Job 2 goes on in the background: job 2 shows it, job 2 end stops it.");
        assert!(shown.last().unwrap().starts_with(core::str::from_utf8(shell::PROMPT).unwrap()));
        assert!(crate::jobs::watched(&sys, crate::jobs::Watch::Screen).is_none());
        let before = sys.jobs.list()[1].statements;
        for _ in 0..50 {
            crate::jobs::work(&mut sys, &mut shell::tests::Lines::default(), &mut || true);
        }
        assert!(sys.jobs.list()[1].statements > before && sys.jobs.list()[1].state == crate::jobs::State::Ready);
        // the prompt is the user's meanwhile, and its names are its own again
        type_text(&mut ui, &mut sys, "say to");
        ui.key(&mut sys, key(Key::Enter));
        assert_eq!(rows(&mut ui, &mut sys).iter().rev().nth(1).unwrap().trim(), "1000000");
        // with nothing to wait for, Alt+B does nothing and Esc goes to the first screen as before
        ui.key(&mut sys, alt(b'b', None));
        sys.screen = LOG;
        ui.key(&mut sys, key(Key::Esc));
        assert_eq!(sys.screen, 0);
        assert_eq!(sys.jobs.list()[1].state, crate::jobs::State::Ready);
    }

    #[test]
    fn a_program_the_screen_waits_for_asks_the_kernel_and_goes_on() {
        let (mut ui, mut sys) = with_jobs();
        for line in ["program asks", "say 'before'", "spin", "say 'after'", "end"] {
            ui.run(&mut sys, line.as_bytes());
        }
        type_text(&mut ui, &mut sys, "asks");
        ui.key(&mut sys, key(Key::Enter));
        // it stops where it asked: the kernel serves, as after any command typed here
        ui.work(&mut sys, &mut || false);
        assert_eq!(sys.request, Some(shell::Request::Spin));
        ui.work(&mut sys, &mut || false);
        assert!(!rows(&mut ui, &mut sys).iter().any(|text| text.trim() == "after"), "it waits for the kernel");
        sys.request = None;
        ui.served(&mut sys);
        ui.work(&mut sys, &mut || false);
        let shown = rows(&mut ui, &mut sys);
        assert_eq!(shown[shown.len() - 2].trim(), "after");
        assert!(shown.last().unwrap().starts_with(core::str::from_utf8(shell::PROMPT).unwrap()));
    }

    #[test]
    fn the_jobs_nobody_waits_for_are_carried_and_the_log_shows_what_they_say() {
        let (mut ui, mut sys) = with_jobs();
        // (a job that is submitted has names of its own: one that counts to a name of the screen would stop)
        for line in ["on notify alerts", "say 'heard ' + text", "spin", "say 'and went on'", "end", "program plain", "say 'counted'", "end", "notify alerts, 'this'", "submit plain"] {
            ui.run(&mut sys, line.as_bytes());
        }
        // the tick starts the program that runs by itself; carrying runs it, and the one that was submitted
        ui.tick(&mut sys, 1);
        let logged = ui.terms[LOG].len();
        let mut asked = None;
        for _ in 0..3000 {
            asked = asked.or(ui.carry(&mut sys, &mut || true));
            if asked.is_some() {
                break;
            }
        }
        // the program asked the kernel something: it is served, and what was answered is in the log too
        assert_eq!(sys.request, Some(shell::Request::Spin));
        sys.request = None;
        ui.carried(&mut sys, asked.unwrap());
        for _ in 0..3000 {
            ui.carry(&mut sys, &mut || true);
        }
        assert!(crate::jobs::idle(&sys));
        sys.screen = LOG;
        let shown = rows(&mut ui, &mut sys);
        assert!(shown.iter().any(|text| text.trim() == "on_notify_alerts: heard this"), "{shown:?}");
        assert!(shown.iter().any(|text| text.trim() == "on_notify_alerts: and went on"));
        assert_eq!(sys.log_rows, ui.terms[LOG].len());
        assert!(ui.terms[LOG].len() > logged);
        // what the submitted job said is with it, not in the log
        assert!(!shown.iter().any(|text| text.contains("counted")));
        assert_eq!(sys.jobs.list()[0].said.lines().last().unwrap(), b"counted");
    }

    #[test]
    fn what_a_program_says_is_drawn_even_when_the_terminal_is_full() {
        let (mut ui, mut sys) = with_jobs();
        // more rows than a terminal keeps: its length no longer tells that something was said
        for n in 0..crate::term::TERM_ROWS + 50 {
            ui.terms[0].line(format!("old {n}").as_bytes(), role::TERM_OUT);
            ui.terms[LOG].line(format!("old {n}").as_bytes(), role::TERM_OUT);
        }
        for line in ["program talks", "say 'now'", "end", "on notify alerts", "say 'heard'", "end", "notify alerts"] {
            ui.run(&mut sys, line.as_bytes());
        }
        type_text(&mut ui, &mut sys, "talks");
        ui.key(&mut sys, key(Key::Enter));
        ui.render(&mut sys);
        assert!(!ui.dirty);
        ui.work(&mut sys, &mut || false);
        assert!(ui.dirty, "the screen is drawn again when the program said something");
        assert!(rows(&mut ui, &mut sys).iter().any(|text| text.trim() == "now"));
        // the same for the log
        ui.tick(&mut sys, 1);
        ui.render(&mut sys);
        ui.dirty = false;
        ui.carry(&mut sys, &mut || false);
        assert!(ui.dirty);
    }
}
