//! XilaScript, the language of XilaOS: what the prompt understands beyond
//! one command.
//!
//! ```text
//! let total = select sum(amount) from expenses
//! say 'Total: ' + total
//!
//! if total > 100
//!     notify alerts, 'over budget'
//! else if total > 50
//!     say 'careful'
//! else
//!     say 'fine'
//! end
//!
//! for row in select label, amount from expenses where amount > 10
//!     say row.label + ': ' + row.amount
//! end
//!
//! program report
//!     let count = select count(*) from expenses
//!     say 'Expenses: ' + count
//! end
//!
//! program restock(item, count)
//!     update stock set qty = qty + count where label = item
//! end
//!
//! restock('cable', 5)
//! ```
//!
//! - One statement per line. A block starts with `if`, `while`, `for` or
//!   `program` and ends with `end`. A line that starts with `#` is a note.
//! - Every command of the prompt is a statement, SQL included, and the
//!   names made with `let` can be used wherever SQL takes a value.
//! - A value is an expression (`expr.rs`), or a SELECT: its first column of
//!   its first row.
//! - A program is lines kept under a name in the table `programs` of the
//!   database: an object of the system like any table, not a file. Its name
//!   then runs it, like a command.
//! - A program can run by itself: `on insert into expenses ... end` when a
//!   row is added (also `on update t`, `on delete from t`, `on notify
//!   channel`), `every 5 minutes ... end` again and again. It is kept like
//!   the others, under a name made from its first line, which is its line 0
//!   in the table.
//!
//! - A program takes values, eight at most, named between parentheses
//!   after its name and kept as its line 0 (`(item, count)`). A call
//!   gives one for each, worked out with the names of who calls. A
//!   program sees its own names only: those, and what it makes.
//!
//! Nothing here runs a line: a block typed at a prompt and the lines of a
//! program are compiled (`code.rs`) and run by the virtual machine
//! (`machine.rs`). What is wrong with the form of a program is told when
//! it is kept, and it is then not kept. This module collects the lines
//! of a block, keeps programs in their table, and starts those that run
//! by themselves. A program runs as the session that started it. Where
//! the system has rooms for jobs it runs as one (`jobs.rs`) and holds
//! nothing; what still runs at once is stopped after a number of
//! statements.

use core::fmt::Write;

use xila_db::{Database, Field, Kind};

use crate::change::engine_error;
use crate::events::{Happened, What, BACKGROUND};
use crate::expr::{Vars, NO_VARS, VAR_NAME};
use crate::grid::COLS;
use crate::shell::{self, Out, Style, System, Table};
use crate::sql::{self, Lexer, Message, Value};
use crate::text::{split_word, trim, FixedStr};

/// Lines of a block typed at the prompt, and of a program.
pub const MAX_LINES: usize = 48;
/// Longest line: what the serial port takes. The screen takes less.
pub const LINE_MAX: usize = 240;
pub type Line = FixedStr<LINE_MAX>;

/// Programs that wait for events, and programs that run again and again.
const MAX_ON: usize = 16;
const MAX_EVERY: usize = 8;
/// Events given to the programs each time `background` is called.
const PER_CALL: usize = 8;
/// Programs started by programs started by programs... through events.
const MAX_CHAIN: u8 = 4;
/// Timer ticks in a second.
const SECOND: u64 = 100;

/// The table programs are kept in.
const PROGRAMS: &[u8] = b"programs";

/// The words that start a block. `on` and `every` are kept for programs
/// that run by themselves.
const OPENERS: [&[u8]; 6] = [b"if", b"for", b"while", b"program", b"on", b"every"];
/// Words that cannot be the name of a value or of a program.
const WORDS: [&[u8]; 14] =
    [b"let", b"say", b"if", b"else", b"end", b"for", b"in", b"while", b"program", b"run", b"return", b"cancel", b"on", b"every"];

/// The first word of a line, in small letters, and what follows it.
pub(crate) fn word_of(line: &[u8]) -> (FixedStr<12>, &[u8]) {
    let (word, rest) = split_word(trim(line));
    let mut lower = FixedStr::<12>::new();
    // a word too long for this buffer is none of the words looked for
    if word.len() <= 11 {
        lower.push_bytes(word);
        lower.lower();
    }
    (lower, trim(rest))
}

/// Whether a line starts a block.
pub fn opens(line: &[u8]) -> bool {
    OPENERS.contains(&word_of(line).0.bytes())
}

/// Whether a name can be given to a value or to a program.
pub(crate) fn free_name(name: &[u8]) -> bool {
    let mut lower = FixedStr::<VAR_NAME>::from_bytes(name);
    lower.lower();
    xila_db::good_name(name) && !WORDS.contains(&lower.bytes()) && !shell::knows(lower.bytes())
}

/// Values a program takes at most.
pub(crate) const PARAMS: usize = 8;

/// A call as it is written: the name of a program, then nothing, or the
/// values it is given between parentheses, to the end of the text.
/// Gives the name and what is between the parentheses, empty for none.
/// None for anything else: `restock 5` is not a call.
pub(crate) fn call_of(text: &[u8]) -> Option<(&[u8], &[u8])> {
    let text = trim(text);
    let end = text.iter().position(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_')).unwrap_or(text.len());
    let (name, rest) = (&text[..end], trim(&text[end..]));
    if name.is_empty() {
        return None;
    }
    if rest.is_empty() {
        return Some((name, b""));
    }
    // the parenthesis that opens must be closed by the last character;
    // what a text holds between its quotes opens and closes nothing
    let (mut depth, mut quoted) = (0usize, false);
    for (at, byte) in rest.iter().enumerate() {
        match byte {
            b'\'' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return (at == rest.len() - 1).then(|| (name, trim(&rest[1..at])));
                }
            }
            _ if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// The names of the values a program takes, in their order.
pub(crate) struct Params {
    names: [FixedStr<VAR_NAME>; PARAMS],
    count: usize,
}

impl Params {
    /// Reads them as they are written between the parentheses of
    /// `program name(item, count)`. Nothing written is none.
    pub(crate) fn read(written: &[u8]) -> Result<Params, Message> {
        let mut params = Params { names: [FixedStr::new(); PARAMS], count: 0 };
        if trim(written).is_empty() {
            return Ok(params);
        }
        for name in written.split(|byte| *byte == b',').map(trim) {
            if !free_name(name) {
                return Err(Message::from_bytes(b"A parameter is named like a value: a letter, then letters, digits or _."));
            }
            if params.count == PARAMS {
                return Err(Message::from_bytes(b"A program takes eight values at most."));
            }
            let mut lower = FixedStr::<VAR_NAME>::from_bytes(name);
            lower.lower();
            if params.names().contains(&lower) {
                return Err(sql::error(b"Two parameters cannot share a name: ", lower.bytes()).also(b"."));
            }
            params.names[params.count] = lower;
            params.count += 1;
        }
        Ok(params)
    }

    pub(crate) fn names(&self) -> &[FixedStr<VAR_NAME>] {
        &self.names[..self.count]
    }

    /// As they are kept and shown: `(item, count)`. Nothing for none.
    pub(crate) fn written(&self) -> Line {
        let mut line = Line::new();
        for (at, name) in self.names().iter().enumerate() {
            line.push_bytes(if at == 0 { b"(" } else { b", " });
            line.push_bytes(name.bytes());
        }
        if self.count > 0 {
            line.push(b')');
        }
        line
    }
}

/// The lines typed since a block was opened.
pub struct Block {
    lines: [Line; MAX_LINES],
    count: usize,
    /// Blocks that are open and wait for their `end`.
    depth: usize,
}

impl Block {
    pub const fn new() -> Block {
        Block { lines: [FixedStr::new(); MAX_LINES], count: 0, depth: 0 }
    }

    /// Whether lines are being collected.
    pub fn open(&self) -> bool {
        self.depth > 0
    }
}

impl Default for Block {
    fn default() -> Block {
        Block::new()
    }
}

/// What to say when a calculation has a SELECT in it: a SELECT is a value
/// only when it is the whole value.
pub(crate) fn whole_select(text: &[u8]) -> Option<Message> {
    let mut lexer = Lexer::new(text);
    loop {
        match lexer.next() {
            sql::Token::End => return None,
            sql::Token::Word(word) if word.eq_ignore_ascii_case(b"select") => {
                return Some(Message::from_bytes(b"A select cannot be part of a calculation: keep it with let first."));
            }
            _ => {}
        }
    }
}

/// Runs one line typed at a prompt.
pub fn line(sys: &mut System, line: &[u8], out: &mut dyn Out) -> bool {
    if line.len() > LINE_MAX {
        out.line(Style::Error, b"A line takes 240 characters at most.");
        return false;
    }
    // a program that is called, by its name or after `run`, with or
    // without values, is a job where the prompt can wait for one
    let (word, rest) = word_of(line);
    let (ran, written) = if word.bytes() == b"run" { (true, rest) } else { (false, trim(line)) };
    let program = call_of(written).is_some_and(|(name, _)| {
        let mut lower = shell::Text::from_bytes(name);
        lower.lower();
        (ran || !shell::knows(lower.bytes())) && exists(&sys.db, name)
    });
    if let (true, Some(watch)) = (program, waits(sys)) {
        return begin(sys, crate::jobs::What::Program(written), watch, out);
    }
    crate::machine::lines(sys, &[Line::from_bytes(line)], out)
}

/// Who would wait for a job started now: the prompt of the screen or of
/// the serial port, on a system that has rooms for jobs, when it waits
/// for none already. Not the companion's lines, nor those a command of a
/// program or of a job types (`qr`), nor what comes from the network or
/// from a program that runs by itself: those run at once.
fn waits(sys: &System) -> Option<crate::jobs::Watch> {
    let watch = crate::jobs::watch_of(sys.session)?;
    (crate::jobs::places(sys) > 0 && !sys.acting && !sys.vm.busy() && crate::jobs::in_turn(sys).is_none() && crate::jobs::watched(sys, watch).is_none()).then_some(watch)
}

/// Starts a job the prompt waits for. True when it started: how it went
/// comes when it has ended (`jobs::outcome`).
fn begin(sys: &mut System, what: crate::jobs::What<'_>, watch: crate::jobs::Watch, out: &mut dyn Out) -> bool {
    match crate::jobs::begin(sys, what, watch, out) {
        Ok(_) => true,
        Err(message) => {
            // why it could not start may have been said already
            if !message.is_empty() {
                out.line(Style::Error, message.bytes());
            }
            false
        }
    }
}

/// Takes a line that opens a block, or that comes while one is open. The
/// lines are run, or kept as a program, when the last `end` comes.
pub fn typed(sys: &mut System, line: &[u8], out: &mut dyn Out) -> bool {
    let word = word_of(line).0;
    let block = &mut sys.blocks[sys.session];
    if block.open() && word.bytes() == b"cancel" {
        *block = Block::new();
        out.line(Style::Out, b"The lines are dropped.");
        return true;
    }
    if line.len() > LINE_MAX || block.count == MAX_LINES {
        *block = Block::new();
        out.line(Style::Error, b"A block takes 48 lines of 240 characters at most. The lines are dropped.");
        return false;
    }
    block.lines[block.count] = Line::from_bytes(line);
    block.count += 1;
    if OPENERS.contains(&word.bytes()) {
        block.depth += 1;
    } else if word.bytes() == b"end" {
        block.depth -= 1;
    }
    if block.depth > 0 {
        return true;
    }
    let block = core::mem::take(block);
    let lines = &block.lines[..block.count];
    let (first, rest) = word_of(lines[0].bytes());
    match first.bytes() {
        b"program" => define(sys, rest, &lines[1..lines.len() - 1], out),
        b"on" | b"every" => define_runs(sys, lines[0].bytes(), &lines[1..lines.len() - 1], out),
        _ => match waits(sys) {
            Some(watch) => begin(sys, crate::jobs::What::Lines(lines), watch, out),
            None => crate::machine::lines(sys, lines, out),
        },
    }
}

/// The table of programs, if it is there and is what it should be.
fn programs(db: &Database) -> Result<Option<usize>, Message> {
    let Some(table) = db.find(PROGRAMS) else {
        return Ok(None);
    };
    let columns = &db.tables()[table].columns;
    let wanted: [(&[u8], Kind); 4] = [(b"id", Kind::Int), (b"name", Kind::Text), (b"line", Kind::Int), (b"text", Kind::Text)];
    if columns.len() != wanted.len() || columns.iter().zip(&wanted).any(|(column, (name, kind))| column.name != *name || column.kind != *kind) {
        return Err(Message::from_bytes(b"The table programs is not the one the system keeps programs in."));
    }
    Ok(Some(table))
}

/// A program as it is read from its table: how many lines, and the
/// names of the values it takes, as written between its parentheses
/// (nothing when it takes none).
pub(crate) struct Loaded {
    pub count: usize,
    pub takes: Line,
}

/// Reads the lines of a program. `None` if there is no such program.
pub(crate) fn load(db: &Database, name: &[u8], into: &mut [Line; MAX_LINES]) -> Result<Option<Loaded>, Message> {
    let Some(table) = programs(db)? else {
        return Ok(None);
    };
    let (mut loaded, mut found) = (Loaded { count: 0, takes: Line::new() }, false);
    db.scan(table, &mut |fields| {
        if let (Field::Text(of), Field::Int(number), Field::Text(text)) = (fields[1], fields[2], fields[3]) {
            if of.eq_ignore_ascii_case(name) {
                found = true;
                // line 0 is not one of its lines: it says what values the
                // program takes, or when it runs by itself
                if number > 0 && loaded.count < MAX_LINES {
                    into[loaded.count] = Line::from_bytes(text);
                    loaded.count += 1;
                } else if number == 0 {
                    if let Some(takes) = text.strip_prefix(b"(").and_then(|rest| rest.strip_suffix(b")")) {
                        loaded.takes = Line::from_bytes(takes);
                    }
                }
            }
        }
    })
    .map_err(|error| engine_error(error, None))?;
    Ok(found.then_some(loaded))
}

/// Line 0 of a program, if it has one: the values it takes between
/// parentheses, or when it runs by itself.
fn header(db: &Database, name: &[u8]) -> Option<Line> {
    let table = programs(db).ok()??;
    let mut found = None;
    let _ = db.scan(table, &mut |fields| {
        if let (Field::Text(of), Field::Int(0), Field::Text(text)) = (fields[1], fields[2], fields[3]) {
            if of.eq_ignore_ascii_case(name) {
                found = Some(Line::from_bytes(text));
            }
        }
    });
    found
}

/// Whether a program of that name is kept.
pub fn exists(db: &Database, name: &[u8]) -> bool {
    let Ok(Some(table)) = programs(db) else {
        return false;
    };
    let mut found = false;
    let _ = db.scan(table, &mut |fields| found |= matches!(fields[1], Field::Text(of) if of.eq_ignore_ascii_case(name)));
    found
}

/// Puts the lines of a program in the database, in place of the ones it
/// had. `first` is its line 0, if it has one: the values it takes, or
/// when it runs by itself. Returns whether the program was there before.
fn store(db: &mut Database, name: &[u8], first: Option<&[u8]>, lines: &[Line]) -> Result<bool, xila_db::Error> {
    let table = match db.find(PROGRAMS) {
        Some(table) => table,
        None => db.create(PROGRAMS, &[(b"name", Kind::Text), (b"line", Kind::Int), (b"text", Kind::Text)])?,
    };
    let replaced = db.delete(table, &mut |fields| matches!(fields[1], Field::Text(of) if of.eq_ignore_ascii_case(name)))? > 0;
    if let Some(first) = first {
        db.insert(table, &[Field::Text(name), Field::Int(0), Field::Text(first)])?;
    }
    // an empty program still has a line, so that it exists
    let empty = [Line::new()];
    for (number, line) in (if lines.is_empty() { &empty[..] } else { lines }).iter().enumerate() {
        db.insert(table, &[Field::Text(name), Field::Int(number as i64 + 1), Field::Text(line.bytes())])?;
    }
    db.commit(&mut |_, _, _| {})?;
    Ok(replaced)
}

/// `program name ... end`, `program name(item, count) ... end`: keeps
/// the lines under the name, with the names of the values it takes.
fn define(sys: &mut System, written: &[u8], lines: &[Line], out: &mut dyn Out) -> bool {
    let Some((name, takes)) = call_of(written).filter(|(name, _)| free_name(name)) else {
        out.line(Style::Error, b"A program is named by a letter, then letters, digits or _, and not as a command.");
        return false;
    };
    let params = match Params::read(takes) {
        Ok(params) => params,
        Err(message) => {
            out.line(Style::Error, message.bytes());
            return false;
        }
    };
    let mut lower = shell::Text::from_bytes(name);
    lower.lower();
    keep(sys, lower.bytes(), None, params.written().bytes(), lines, out)
}

/// When a program runs by itself.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Runs {
    /// When this happens to this table, or when this channel is notified.
    On(When, sql::Name),
    /// Again and again: so many seconds, minutes, hours or days.
    Every(u64, &'static [u8]),
    /// Once a day, when the clock of the machine shows that time: so many
    /// minutes after midnight.
    Daily(u32),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum When {
    Insert,
    Update,
    Delete,
    Notify,
}

/// The units of `every`, and how many timer ticks each is.
const UNITS: [(&[u8], u64); 4] = [(b"second", SECOND), (b"minute", 60 * SECOND), (b"hour", 3600 * SECOND), (b"day", 86400 * SECOND)];

/// Reads the first line of a program that runs by itself.
fn runs(line: &[u8]) -> Result<Runs, Message> {
    let (first, rest) = word_of(line);
    if first.bytes() == b"every" {
        let wrong = || Message::from_bytes(b"Write: every 5 minutes (or seconds, hours, days), or every day at 8:00");
        let (count, unit) = split_word(rest);
        if count.eq_ignore_ascii_case(b"day") {
            // every day at 8:00
            let (at, time) = word_of(unit);
            let minutes = Some(time).filter(|_| at.bytes() == b"at").and_then(crate::clock::time_of_day).ok_or_else(wrong)?;
            return Ok(Runs::Daily(minutes));
        }
        let count = crate::text::parse_number(count).filter(|count| (1..=10_000).contains(count)).ok_or_else(wrong)?;
        let (unit, more) = word_of(unit);
        let unit = unit.bytes().strip_suffix(b"s").unwrap_or(unit.bytes());
        let found = UNITS.iter().find(|(name, _)| *name == unit).filter(|_| more.is_empty()).ok_or_else(wrong)?;
        return Ok(Runs::Every(count as u64, found.0));
    }
    let wrong = || Message::from_bytes(b"Write: on insert into t, on update t, on delete from t, or on notify channel");
    let (what, rest) = word_of(rest);
    let (when, rest) = match what.bytes() {
        b"insert" => (When::Insert, word_of(rest)),
        b"update" => (When::Update, (FixedStr::from_bytes(b"into"), rest)),
        b"delete" => (When::Delete, word_of(rest)),
        b"notify" => (When::Notify, (FixedStr::from_bytes(b"into"), rest)),
        _ => return Err(wrong()),
    };
    // the word SQL has there: insert INTO, delete FROM
    let wanted: &[u8] = if when == When::Delete { b"from" } else { b"into" };
    let (target, more) = split_word(rest.1);
    if rest.0.bytes() != wanted || !xila_db::good_name(target) || !more.is_empty() {
        return Err(wrong());
    }
    let mut name = sql::Name::from_bytes(target);
    name.lower();
    Ok(Runs::On(when, name))
}

impl Runs {
    /// The line as it is kept, and as `show` gives it back.
    fn line(&self) -> shell::Text {
        let mut line = shell::Text::new();
        let _ = match self {
            Runs::On(When::Insert, table) => write!(line, "on insert into {table}"),
            Runs::On(When::Update, table) => write!(line, "on update {table}"),
            Runs::On(When::Delete, table) => write!(line, "on delete from {table}"),
            Runs::On(When::Notify, channel) => write!(line, "on notify {channel}"),
            Runs::Every(count, unit) => write!(line, "every {count} {}{}", shell::Text::from_bytes(unit), if *count == 1 { "" } else { "s" }),
            Runs::Daily(minutes) => write!(line, "every day at {}:{:02}", minutes / 60, minutes % 60),
        };
        line
    }

    /// The name the program is kept under: its line, with `_` for spaces.
    fn name(&self) -> shell::Text {
        let mut name = shell::Text::new();
        for word in self.line().bytes().split(|b| *b == b' ').filter(|word| !matches!(*word, b"into" | b"from")) {
            if !name.is_empty() {
                name.push(b'_');
            }
            // a name has no colon: 8:00 is 8h00
            for byte in word {
                name.push(if *byte == b':' { b'h' } else { *byte });
            }
        }
        name
    }

    /// Timer ticks between two runs.
    fn period(&self) -> u64 {
        match self {
            Runs::Every(count, unit) => count * UNITS.iter().find(|(name, _)| name == unit).map_or(SECOND, |(_, ticks)| *ticks),
            Runs::On(..) | Runs::Daily(_) => 0,
        }
    }
}

/// `on ... end` and `every ... end`: keeps the lines, to be run by the system.
fn define_runs(sys: &mut System, first: &[u8], lines: &[Line], out: &mut dyn Out) -> bool {
    let runs = match runs(first) {
        Ok(runs) => runs,
        Err(message) => {
            out.line(Style::Error, message.bytes());
            return false;
        }
    };
    if let Runs::On(when, table) = runs {
        if when != When::Notify && sys.db.find(table.bytes()).is_none() {
            let message = if crate::catalog::find(table.bytes()).is_some() {
                sql::error(table.bytes(), b" is a view of the system: it does not say when it changes.")
            } else {
                sql::error(b"Unknown table: ", table.bytes()).also(b". Type tables to list them.")
            };
            out.line(Style::Error, message.bytes());
            return false;
        }
    }
    let name = runs.name();
    // one more must fit among those the system watches
    refresh(sys);
    let known = exists(&sys.db, name.bytes());
    let full = match runs {
        Runs::On(..) => sys.triggers.on_count == MAX_ON,
        Runs::Every(..) | Runs::Daily(_) => sys.triggers.every_count == MAX_EVERY,
    };
    if full && !known {
        out.line(Style::Error, b"There is room for 16 programs that wait for events and 8 that run again and again.");
        return false;
    }
    keep(sys, name.bytes(), Some(runs.line().bytes()), b"", lines, out)
}

/// Puts a program in the database and says so. `runs` is when it runs by
/// itself, `takes` the values it takes as they are kept: one or neither.
fn keep(sys: &mut System, name: &[u8], runs: Option<&[u8]>, takes: &[u8], lines: &[Line], out: &mut dyn Out) -> bool {
    if let Err(message) = programs(&sys.db) {
        out.line(Style::Error, message.bytes());
        return false;
    }
    // what is wrong with its form is told now, and the program of that
    // name that was kept before, if any, stays as it was
    if let Err(fault) = crate::machine::check(sys, lines) {
        out.line(Style::Error, fault.message.bytes());
        let mut place = FixedStr::<COLS>::new();
        let _ = write!(place, "Line {} is not right: the program is not kept.", fault.line);
        out.line(Style::Error, place.bytes());
        return false;
    }
    match store(&mut sys.db, name, runs.or(Some(takes).filter(|takes| !takes.is_empty())), lines) {
        Ok(replaced) => {
            let now = sys.ticks.max(1);
            sys.stored_at = now;
            // at once: the very next change may be one this program waits for
            refresh(sys);
            let mut text = shell::Text::from_bytes(b"program ");
            text.push_bytes(name);
            sys.events.notify(PROGRAMS, text.bytes(), now);
            let mut message = FixedStr::<COLS>::new();
            let (what, count) = (if replaced { "replaced" } else { "kept" }, lines.len());
            let _ = write!(message, "Program {} {what}: {count} line{}.", shell::Text::from_bytes(name), if count == 1 { "" } else { "s" });
            out.line(Style::Out, message.bytes());
            if let Some(runs) = runs {
                out.line(Style::Out, FixedStr::<COLS>::from_bytes(b"It runs ").also(runs).also(b". What it says goes to the log.").bytes());
                if runs.starts_with(b"every day at") && sys.now().is_none() {
                    out.line(Style::Out, b"The clock of this machine tells no time: until it does, it will not run.");
                }
            }
            true
        }
        Err(error) => {
            sys.db.rollback();
            out.line(Style::Error, engine_error(error, None).bytes());
            false
        }
    }
}

/// `programs`: lists the programs, how long they are, and when they run
/// by themselves.
pub fn list(sys: &mut System, out: &mut dyn Out) -> bool {
    let table = match programs(&sys.db) {
        Ok(table) => table,
        Err(message) => {
            out.line(Style::Error, message.bytes());
            return false;
        }
    };
    // name, lines, the line that says when it runs, and the values it takes
    let mut found = [(shell::Text::new(), 0usize, shell::Text::new(), shell::Text::new()); shell::TABLE_ROWS];
    let (mut count, mut hidden) = (0, 0);
    if let Some(table) = table {
        let scanned = sys.db.scan(table, &mut |fields| {
            let (Field::Text(name), Field::Int(number), Field::Text(text)) = (fields[1], fields[2], fields[3]) else {
                return;
            };
            let at = match found[..count].iter().position(|(known, ..)| known.bytes() == name) {
                Some(at) => at,
                None if count < found.len() => {
                    found[count].0 = shell::Text::from_bytes(name);
                    count += 1;
                    count - 1
                }
                None => {
                    hidden += (number <= 1) as usize;
                    return;
                }
            };
            match number {
                // a list too long to show beside the name is not cut inside a name
                0 if text.starts_with(b"(") => found[at].3 = shell::Text::from_bytes(if name.len() + text.len() <= 48 { text } else { b"(...)" }),
                0 => found[at].2 = shell::Text::from_bytes(text),
                // the single line of an empty program does not count
                _ if text.is_empty() && number == 1 => {}
                _ => found[at].1 += 1,
            }
        });
        if let Err(error) = scanned {
            out.line(Style::Error, engine_error(error, None).bytes());
            return false;
        }
    }
    if count == 0 {
        out.line(Style::Out, b"There is no program yet. help l shows how to make one.");
        return true;
    }
    let mut listed = Table::new(PROGRAMS, &[b"name", b"lines", b"runs"]);
    for (name, lines, runs, takes) in &found[..count] {
        let mut number = shell::Text::new();
        let _ = write!(number, "{lines}");
        let mut command = shell::Text::from_bytes(b"show ");
        command.push_bytes(name.bytes());
        let called = shell::Text::from_bytes(name.bytes()).also(takes.bytes());
        listed.row_to(&[called.bytes(), number.bytes(), if runs.is_empty() { b"when asked" } else { runs.bytes() }], command.bytes());
    }
    listed.more = hidden;
    shell::show(sys, out, &listed);
    true
}

/// `show name`: the lines of a program, as they can be typed again.
pub fn show(sys: &mut System, name: &[u8], out: &mut dyn Out) -> bool {
    let mut lines = [Line::new(); MAX_LINES];
    let count = match load(&sys.db, name, &mut lines) {
        Ok(Some(loaded)) => loaded.count,
        Ok(None) => {
            out.line(Style::Error, crate::sql::error(b"Unknown program: ", name).also(b". Type programs to list them.").bytes());
            return false;
        }
        Err(message) => {
            out.line(Style::Error, message.bytes());
            return false;
        }
    };
    let mut lower = shell::Text::from_bytes(name);
    lower.lower();
    let named = FixedStr::<{ LINE_MAX + 32 }>::from_bytes(b"program ").also(lower.bytes());
    match header(&sys.db, name) {
        Some(takes) if takes.bytes().starts_with(b"(") => out.line(Style::Out, named.also(takes.bytes()).bytes()),
        Some(runs) => out.line(Style::Out, runs.bytes()),
        None => out.line(Style::Out, named.bytes()),
    }
    let mut depth: usize = 1;
    for line in &lines[..count] {
        let word = word_of(line.bytes()).0;
        if matches!(word.bytes(), b"end" | b"else") {
            depth = depth.saturating_sub(1);
        }
        if !line.is_empty() {
            let mut shown = FixedStr::<{ LINE_MAX + 32 }>::new();
            for _ in 0..depth.min(8) * 4 {
                shown.push(b' ');
            }
            shown.push_bytes(line.bytes());
            out.line(Style::Out, shown.bytes());
        }
        if OPENERS.contains(&word.bytes()) || word.bytes() == b"else" {
            depth += 1;
        }
    }
    out.line(Style::Out, b"end");
    true
}

/// `drop program name`.
pub fn remove(sys: &mut System, name: &[u8], out: &mut dyn Out) -> bool {
    let table = match programs(&sys.db) {
        Ok(Some(table)) if exists(&sys.db, name) => table,
        Ok(_) => {
            out.line(Style::Error, crate::sql::error(b"Unknown program: ", name).also(b". Type programs to list them.").bytes());
            return false;
        }
        Err(message) => {
            out.line(Style::Error, message.bytes());
            return false;
        }
    };
    let removed = sys.db.delete(table, &mut |fields| matches!(fields[1], Field::Text(of) if of.eq_ignore_ascii_case(name)));
    match removed.and_then(|_| sys.db.commit(&mut |_, _, _| {})) {
        Ok(()) => {
            let now = sys.ticks.max(1);
            sys.stored_at = now;
            refresh(sys);
            let mut lower = shell::Text::from_bytes(name);
            lower.lower();
            let mut text = shell::Text::from_bytes(b"drop ");
            text.push_bytes(lower.bytes());
            sys.events.notify(PROGRAMS, text.bytes(), now);
            out.line(Style::Out, FixedStr::<COLS>::from_bytes(b"Program ").also(lower.bytes()).also(b" removed.").bytes());
            true
        }
        Err(error) => {
            sys.db.rollback();
            out.line(Style::Error, engine_error(error, None).bytes());
            false
        }
    }
}

/// `vars`: the names of the session and their values.
pub fn vars(sys: &mut System, out: &mut dyn Out) -> bool {
    let mut table = Table::new(b"vars", &[b"name", b"value"]);
    let mut hidden = 0;
    for (name, value) in sys.names().each() {
        if table.rows == shell::TABLE_ROWS {
            hidden += 1;
        }
        table.row(&[name, value.shown().bytes()]);
    }
    table.more = hidden;
    if table.rows == 0 {
        out.line(Style::Out, b"No name has a value yet. Give one with: let total = 3");
        return true;
    }
    shell::show(sys, out, &table);
    true
}

/// The programs that run by themselves, as the system watches them. It is
/// read again from the table of programs whenever that may have changed.
pub struct Triggers {
    stale: bool,
    /// What happens, to which table or channel, and the program to run.
    on: [(When, sql::Name, shell::Text); MAX_ON],
    on_count: usize,
    /// The program, the ticks between two runs, and the tick of the next.
    /// For a program that runs at a time of day: the minutes after
    /// midnight, and the second of the next run by the clock of the
    /// machine, since the year 2000.
    every: [(shell::Text, u64, u64, bool); MAX_EVERY],
    every_count: usize,
    /// As jobs: the first program by the clock to look at, and whether
    /// what happened is looked at before them, so that all take turns.
    every_from: usize,
    heard_next: bool,
}

impl Triggers {
    pub const fn new() -> Triggers {
        Triggers {
            stale: true,
            on: [(When::Notify, FixedStr::new(), FixedStr::new()); MAX_ON],
            on_count: 0,
            every: [(FixedStr::new(), 0, 0, false); MAX_EVERY],
            every_count: 0,
            every_from: 0,
            heard_next: false,
        }
    }

    /// Says that the programs may not be what they were.
    pub fn changed(&mut self) {
        self.stale = true;
    }
}

impl Default for Triggers {
    fn default() -> Triggers {
        Triggers::new()
    }
}

/// Reads from the table of programs which ones run by themselves.
fn refresh(sys: &mut System) {
    let before = core::mem::take(&mut sys.triggers);
    let clock = sys.now();
    let (triggers, now) = (&mut sys.triggers, sys.ticks);
    triggers.stale = false;
    triggers.heard_next = before.heard_next;
    if let Ok(Some(table)) = programs(&sys.db) {
        let _ = sys.db.scan(table, &mut |fields| {
            let (Field::Text(name), Field::Int(0), Field::Text(text)) = (fields[1], fields[2], fields[3]) else {
                return;
            };
            match runs(text) {
                Ok(Runs::On(when, target)) if triggers.on_count < MAX_ON => {
                    triggers.on[triggers.on_count] = (when, target, shell::Text::from_bytes(name));
                    triggers.on_count += 1;
                }
                Ok(every @ Runs::Every(..)) if triggers.every_count < MAX_EVERY => {
                    // a program that was already waiting keeps its turn
                    let waiting = before.every[..before.every_count].iter().find(|(known, period, _, daily)| known.bytes() == name && *period == every.period() && !daily);
                    let next = waiting.map_or(now + every.period(), |(_, _, next, _)| *next);
                    triggers.every[triggers.every_count] = (shell::Text::from_bytes(name), every.period(), next, false);
                    triggers.every_count += 1;
                }
                Ok(Runs::Daily(minutes)) if triggers.every_count < MAX_EVERY => {
                    // the next time the clock shows that hour; never, without a clock
                    let next = clock.map_or(u64::MAX, |clock| crate::clock::next_at(clock, minutes));
                    triggers.every[triggers.every_count] = (shell::Text::from_bytes(name), minutes as u64, next, true);
                    triggers.every_count += 1;
                }
                _ => {}
            }
        });
    }
    sys.events.watched = sys.triggers.on_count > 0;
}

/// Puts the name of the program before every line it says.
struct Named<'a> {
    out: &'a mut dyn Out,
    name: &'a [u8],
}

impl Out for Named<'_> {
    fn line(&mut self, style: Style, text: &[u8]) {
        let line = FixedStr::<{ LINE_MAX + 64 }>::from_bytes(self.name).also(b": ").also(text);
        self.out.line(style, line.bytes());
    }

    fn table(&mut self, table: &Table) {
        self.line(Style::Out, table.aligned(None).bytes());
        for row in 0..table.rows {
            self.line(Style::Out, table.aligned(Some(row)).bytes());
        }
    }
}

/// Gives a program the names that say what happened: `channel`, then
/// `text` for a notify, or `id` and the columns of the row as `new.column`.
/// False when the row is not there any more: nothing is left to run for.
fn describe(sys: &mut System, happened: &Happened) -> Result<bool, Message> {
    let no_room = || Message::from_bytes(b"There is no room for the names of what happened.");
    let mut names = Vars::new();
    let mut set = |name: &[u8], value: Value| if names.set(name, value) { Ok(()) } else { Err(no_room()) };
    set(b"channel", Value::text(happened.channel.bytes()))?;
    match happened.what {
        What::Said | What::Table => set(b"text", Value::Text(happened.text))?,
        What::Insert(id) | What::Update(id) | What::Delete(id) => {
            set(b"id", Value::Int(id as i64))?;
            if !matches!(happened.what, What::Delete(_)) {
                let mut query = FixedStr::<96>::new();
                let _ = write!(query, "select * from {} where id = {id}", happened.channel);
                let rows = sql::query(&*sys, &NO_VARS, query.bytes())?;
                if rows.count == 0 {
                    return Ok(false);
                }
                for (header, value) in rows.headers.iter().zip(&rows.rows[0]).take(rows.cols) {
                    set(FixedStr::<VAR_NAME>::from_bytes(b"new.").also(header.bytes()).bytes(), *value)?;
                }
            }
        }
    }
    sys.vars[BACKGROUND] = names;
    Ok(true)
}

/// Runs a program for the system, not for a prompt: with names of its own,
/// and with its name before what it says.
fn fire(sys: &mut System, program: &[u8], happened: Option<&Happened>, out: &mut dyn Out) {
    let session = core::mem::replace(&mut sys.session, BACKGROUND);
    // who asks, if this program asks the companion something
    sys.firing = crate::sql::Name::from_bytes(program);
    sys.vars[BACKGROUND] = Vars::new();
    // what this program causes is one step further down the chain
    sys.events.depth = happened.map_or(0, |happened| happened.depth) + 1;
    let mut named = Named { out, name: program };
    let ready = match happened {
        Some(happened) => describe(sys, happened),
        None => Ok(true),
    };
    // after the names of what happened, which are set as a whole
    sys.vars[BACKGROUND].clock = sys.now();
    match ready {
        Ok(true) => {
            // a failure was said where it happened
            crate::machine::program(sys, program, &mut named);
        }
        Ok(false) => {}
        Err(message) => named.line(Style::Error, message.bytes()),
    }
    sys.events.depth = 0;
    sys.firing.clear();
    sys.session = session;
}

/// Runs the programs whose time has come, and those that wait for what
/// just happened. The system calls this again and again; what the programs
/// say goes to `out`.
pub fn background(sys: &mut System, out: &mut dyn Out) {
    if sys.triggers.stale {
        refresh(sys);
    }
    match crate::jobs::system_place(sys) {
        // As jobs: this starts the next program when the one before has
        // ended, and the turns it is given run it. One at a time, in the
        // order a system without rooms runs them.
        Some(place) => {
            let mut looked = 0;
            while crate::jobs::free(sys, place) {
                let Some((program, happened)) = due(sys, out, &mut looked) else {
                    break;
                };
                start(sys, place, program.bytes(), happened.as_ref(), out);
            }
        }
        None => at_once(sys, out),
    }
    let unseen = sys.events.take_unseen();
    if unseen > 0 {
        let mut line = FixedStr::<COLS>::new();
        let _ = write!(line, "{unseen} events came too fast for the programs that wait for them.");
        out.line(Style::Error, line.bytes());
    }
}

/// Runs to the end, here and now, the programs whose time has come and
/// those that wait for what just happened: what a system does that was
/// given no rooms for jobs.
fn at_once(sys: &mut System, out: &mut dyn Out) {
    let clock = sys.now();
    for at in 0..sys.triggers.every_count.min(MAX_EVERY) {
        let (program, period, next, daily) = sys.triggers.every[at];
        if sys.triggers.stale {
            break;
        }
        match (daily, clock) {
            (false, _) if sys.ticks >= next => sys.triggers.every[at].2 = sys.ticks + period,
            // the same hour tomorrow, by the clock and not by a count
            (true, Some(clock)) if clock >= next => sys.triggers.every[at].2 = crate::clock::next_at(clock, period as u32),
            _ => continue,
        }
        fire(sys, program.bytes(), None, out);
    }
    for _ in 0..PER_CALL {
        let Some(happened) = sys.events.take_happened() else {
            break;
        };
        // a program may have changed the programs
        if sys.triggers.stale {
            refresh(sys);
        }
        for at in 0..sys.triggers.on_count {
            let (when, target, program) = sys.triggers.on[at];
            let is = matches!(
                (when, happened.what),
                (When::Insert, What::Insert(_)) | (When::Update, What::Update(_)) | (When::Delete, What::Delete(_)) | (When::Notify, What::Said)
            );
            if !is || target.bytes() != happened.channel.bytes() {
                continue;
            }
            if happened.depth >= MAX_CHAIN {
                let line = FixedStr::<LINE_MAX>::from_bytes(program.bytes()).also(b": not run, programs kept starting each other.");
                out.line(Style::Error, line.bytes());
                continue;
            }
            fire(sys, program.bytes(), Some(&happened), out);
        }
    }
}

/// The next program to run by itself, with what happened if something
/// did. The programs that run by the clock and those that hear something
/// take turns, and among the first each has its turn: one that takes
/// longer than its rhythm keeps no other from running. `looked` counts
/// the events taken in this call, which are so many at most.
fn due(sys: &mut System, out: &mut dyn Out, looked: &mut usize) -> Option<(shell::Text, Option<Happened>)> {
    let found = if sys.triggers.heard_next {
        due_event(sys, out, looked).or_else(|| due_every(sys))
    } else {
        due_every(sys).or_else(|| due_event(sys, out, looked))
    };
    sys.triggers.heard_next = matches!(found, Some((_, None)));
    found
}

/// A program whose time by the clock has come: the one after the last
/// that ran, round and round.
fn due_every(sys: &mut System) -> Option<(shell::Text, Option<Happened>)> {
    let clock = sys.now();
    let count = sys.triggers.every_count.min(MAX_EVERY);
    for step in 0..count {
        let at = (sys.triggers.every_from + step) % count;
        let (program, period, next, daily) = sys.triggers.every[at];
        match (daily, clock) {
            (false, _) if sys.ticks >= next => sys.triggers.every[at].2 = sys.ticks + period,
            // the same hour tomorrow, by the clock and not by a count
            (true, Some(clock)) if clock >= next => sys.triggers.every[at].2 = crate::clock::next_at(clock, period as u32),
            _ => continue,
        }
        sys.triggers.every_from = (at + 1) % count;
        return Some((program, None));
    }
    // none is late: the next time some are, they run in the order they were made
    sys.triggers.every_from = 0;
    None
}

/// The program that waits for the next thing that happened, if one does.
/// One at most: a program is named after what it waits for, so a second
/// one for the same thing replaces the first.
fn due_event(sys: &mut System, out: &mut dyn Out, looked: &mut usize) -> Option<(shell::Text, Option<Happened>)> {
    while *looked < PER_CALL {
        *looked += 1;
        let happened = sys.events.take_happened()?;
        // a program may have changed the programs
        if sys.triggers.stale {
            refresh(sys);
        }
        let waits = sys.triggers.on[..sys.triggers.on_count].iter().find(|(when, target, _)| {
            let is = matches!(
                (when, happened.what),
                (When::Insert, What::Insert(_)) | (When::Update, What::Update(_)) | (When::Delete, What::Delete(_)) | (When::Notify, What::Said)
            );
            is && target.bytes() == happened.channel.bytes()
        });
        let Some((_, _, program)) = waits.copied() else {
            continue;
        };
        if happened.depth >= MAX_CHAIN {
            let line = FixedStr::<LINE_MAX>::from_bytes(program.bytes()).also(b": not run, programs kept starting each other.");
            out.line(Style::Error, line.bytes());
            continue;
        }
        return Some((program, Some(happened)));
    }
    None
}

/// Starts a program for the system as a job: what `fire` does before it
/// runs one, the names of what happened among it.
fn start(sys: &mut System, place: usize, program: &[u8], happened: Option<&Happened>, out: &mut dyn Out) {
    let session = core::mem::replace(&mut sys.session, BACKGROUND);
    sys.vars[BACKGROUND] = Vars::new();
    // what this program causes is one step further down the chain
    let depth = happened.map_or(0, |happened| happened.depth) + 1;
    let mut named = Named { out, name: program };
    let ready = match happened {
        Some(happened) => describe(sys, happened),
        None => Ok(true),
    };
    sys.vars[BACKGROUND].clock = sys.now();
    match ready {
        // why it could not start, if it could not, was said
        Ok(true) => crate::jobs::begin_system(sys, place, program, depth, &mut named),
        Ok(false) => {}
        Err(message) => named.line(Style::Error, message.bytes()),
    }
    sys.session = session;
}

/// What a program that runs by itself says, with its name before it.
pub(crate) fn named<'a>(out: &'a mut dyn Out, name: &'a [u8]) -> impl Out + 'a {
    Named { out, name }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::Lines;

    /// Types lines at the prompt and returns what the last one showed. On a
    /// system with rooms the prompt waits for what became a job, as the
    /// screen does: one instruction a turn, until it has ended.
    fn type_lines(sys: &mut System, lines: &[&str]) -> (bool, Vec<String>) {
        let mut last = (true, Vec::new());
        for line in lines {
            let mut out = Lines::default();
            let mut ok = shell::exec(sys, line.as_bytes(), &mut out);
            if let Some(watch) = crate::jobs::watch_of(sys.session) {
                while let Some(at) = crate::jobs::watched(sys, watch) {
                    crate::jobs::turn(sys, at, &mut out, &mut || true);
                    if let Some(went) = crate::jobs::outcome(sys, watch) {
                        ok = went;
                    }
                }
            }
            last = (ok, out.lines);
        }
        last
    }

    fn said(sys: &mut System, lines: &[&str]) -> Vec<String> {
        type_lines(sys, lines).1
    }

    /// A system with a table of three expenses.
    fn books() -> System {
        let mut sys = System::new();
        type_lines(
            &mut sys,
            &[
                "create table expenses (label text, amount decimal)",
                "insert into expenses values ('coffee', 3.5), ('train', 42), ('bread', 1.2)",
            ],
        );
        sys
    }

    #[test]
    fn a_call_is_read_as_written() {
        let read = |text: &str| call_of(text.as_bytes()).map(|(name, given)| (String::from_utf8_lossy(name).into_owned(), String::from_utf8_lossy(given).into_owned()));
        let is = |name: &str, given: &str| Some((name.to_string(), given.to_string()));
        assert_eq!(read("report"), is("report", ""));
        assert_eq!(read("  report()  "), is("report", ""));
        assert_eq!(read("restock('cable', 5)"), is("restock", "'cable', 5"));
        // a text may hold what would end the call anywhere else
        assert_eq!(read("restock ('a, (b', round(2.5) )"), is("restock", "'a, (b', round(2.5)"));
        assert_eq!(read("restock 5"), None);
        assert_eq!(read("f(1) x"), None);
        assert_eq!(read("f(1"), None);
        assert_eq!(read("(1)"), None);
        assert_eq!(read(""), None);
    }

    #[test]
    fn parameters_are_kept_with_the_program_and_shown() {
        let mut sys = books();
        assert_eq!(said(&mut sys, &["program Restock( Item,count )", "say item", "end"]), ["Program restock kept: 1 line."]);
        assert_eq!(said(&mut sys, &["show restock"]), ["program restock(item, count)", "    say item", "end"]);
        assert_eq!(said(&mut sys, &["programs"]), ["     name                  lines  runs", " [1] restock(item, count)  1      when asked"]);
        assert_eq!(said(&mut sys, &["1"])[0], "program restock(item, count)");
        assert_eq!(said(&mut sys, &["select line, text from programs where name = 'restock' order by line"]), ["   line  text", "   0     (item, count)", "   1     say item"]);
        // typed again without any, it takes none: nothing is left of them
        type_lines(&mut sys, &["program restock()", "say 1", "end"]);
        assert_eq!(said(&mut sys, &["show restock"])[0], "program restock");
        assert_eq!(said(&mut sys, &["select count(*) from programs where line = 0"]), ["   count", "   0"]);
        // a long list is not cut in the middle of a name
        type_lines(&mut sys, &["program long(first_of_the_values, second_of_the_values, third)", "end"]);
        assert_eq!(said(&mut sys, &["programs"])[1..], [" [1] restock    1      when asked", " [2] long(...)  0      when asked"]);
        assert_eq!(said(&mut sys, &["show long"])[0], "program long(first_of_the_values, second_of_the_values, third)");
    }

    #[test]
    fn what_is_wrong_with_parameters_is_told_and_nothing_is_kept() {
        let mut sys = System::new();
        let named = "A parameter is named like a value: a letter, then letters, digits or _.";
        assert_eq!(said(&mut sys, &["program f(a, b, c, d, e, f, g, h, i)", "end"]), ["A program takes eight values at most."]);
        assert_eq!(said(&mut sys, &["program f(item, Item)", "end"]), ["Two parameters cannot share a name: item."]);
        assert_eq!(said(&mut sys, &["program f(1abc)", "end"]), [named]);
        assert_eq!(said(&mut sys, &["program f(a, )", "end"]), [named]);
        assert_eq!(said(&mut sys, &["program f(say)", "end"]), [named]);
        assert_eq!(said(&mut sys, &["program f(mood)", "end"]), [named]);
        assert_eq!(said(&mut sys, &["program f(a b)", "end"]), [named]);
        assert_eq!(said(&mut sys, &["program f(a", "end"]), ["A program is named by a letter, then letters, digits or _, and not as a command."]);
        assert_eq!(said(&mut sys, &["program f a", "end"]), ["A program is named by a letter, then letters, digits or _, and not as a command."]);
        assert_eq!(said(&mut sys, &["programs"]), ["There is no program yet. help l shows how to make one."]);
        // eight are taken
        assert_eq!(said(&mut sys, &["program f(a, b, c, d, e, f, g, h)", "end"]), ["Program f kept: 0 lines."]);
    }

    #[test]
    fn names_keep_values() {
        let mut sys = books();
        assert_eq!(type_lines(&mut sys, &["let total = 3 * 1.5"]), (true, vec![]));
        assert_eq!(said(&mut sys, &["say total"]), ["4.5"]);
        assert_eq!(said(&mut sys, &["let Total = total + 1", "say 'Total: ' + TOTAL"]), ["Total: 5.5"]);
        assert_eq!(said(&mut sys, &["let who = 'the ' || upper('cat')", "say who"]), ["the CAT"]);
        assert_eq!(said(&mut sys, &["say"]), [""]);
        // a select gives its first value; without a row, nothing
        assert_eq!(said(&mut sys, &["let sum = select sum(amount), count(*) from expenses", "say sum"]), ["46.70"]);
        assert_eq!(said(&mut sys, &["let none = select label from expenses where amount > 100", "say none is null"]), ["1"]);
        assert_eq!(said(&mut sys, &["say select count(*) from expenses"]), ["3"]);
        let (ok, out) = type_lines(&mut sys, &["vars"]);
        assert!(ok);
        let out: Vec<&str> = out.iter().map(|line| line.trim_end()).collect();
        assert_eq!(out, ["   name   value", "   total  5.5", "   who    the CAT", "   sum    46.70", "   none"]);
        assert_eq!(said(&mut sys, &["say 'Total: ' + select sum(amount) from expenses"]), ["A select cannot be part of a calculation: keep it with let first."]);
        assert_eq!(said(&mut sys, &["if 3 < select count(*) from expenses", "end"])[0], "A select cannot be part of a calculation: keep it with let first.");
    }

    #[test]
    fn names_are_values_in_sql() {
        let mut sys = books();
        type_lines(&mut sys, &["let limit = 3", "let what = 'tea'"]);
        assert_eq!(said(&mut sys, &["select label from expenses where amount > limit order by label"]), ["   label", "   coffee", "   train"]);
        assert_eq!(said(&mut sys, &["insert into expenses values (what, limit * 2)"]), ["1 row added to expenses, with id 4."]);
        assert_eq!(said(&mut sys, &["update expenses set amount = amount + limit where label = what"]), ["1 row changed in expenses."]);
        assert_eq!(said(&mut sys, &["delete from expenses where amount > limit * 10"]), ["1 row removed from expenses."]);
        assert_eq!(said(&mut sys, &["select label, amount from expenses where label = what"]), ["   label  amount", "   tea    9.00"]);
        // a column hides a name: here amount is the column
        type_lines(&mut sys, &["let amount = 1000"]);
        assert_eq!(said(&mut sys, &["select count(*) from expenses where amount < 100"]), ["   count", "   3"]);
        // each session has its own names
        sys.session = crate::events::SERIAL;
        assert_eq!(said(&mut sys, &["say limit"]), ["Unknown name: limit"]);
    }

    #[test]
    fn what_is_wrong_with_a_name_is_said() {
        let mut sys = System::new();
        assert_eq!(said(&mut sys, &["let x"]), ["A name gets a value like this: let total = 3"]);
        assert_eq!(said(&mut sys, &["let 2x = 1"]), ["A name is a letter, then letters, digits or _, and not a command."]);
        assert_eq!(said(&mut sys, &["let select = 1"]), ["A name is a letter, then letters, digits or _, and not a command."]);
        assert_eq!(said(&mut sys, &["let end = 1"]), ["A name is a letter, then letters, digits or _, and not a command."]);
        assert_eq!(said(&mut sys, &["let x = y + 1"]), ["Unknown name: y"]);
        assert_eq!(said(&mut sys, &["let x = 1 2"]), ["Expected the end of the query, found: 2"]);
        assert_eq!(type_lines(&mut sys, &["say nothing_here"]), (false, vec!["Unknown name: nothing_here".to_string()]));
        assert_eq!(said(&mut sys, &["vars"]), ["No name has a value yet. Give one with: let total = 3"]);
        for n in 0..32 {
            assert!(type_lines(&mut sys, &[&format!("let v{n} = {n}")]).0);
        }
        assert_eq!(said(&mut sys, &["let one_more = 1"]), ["There is no room for one more name: 32 are kept at most."]);
        assert_eq!(said(&mut sys, &["let v3 = v3 + v31", "say v3"]), ["34"]);
    }

    #[test]
    fn conditions_pick_the_lines_to_run() {
        let mut sys = System::new();
        let grade = |sys: &mut System, n: i64| {
            said(
                sys,
                &[
                    &format!("let n = {n}"),
                    "if n > 100",
                    "say 'large'",
                    "else if n > 10",
                    "    if n % 2 = 0",
                    "        say 'medium, even'",
                    "    else",
                    "        say 'medium, odd'",
                    "    end",
                    "else if n > 0",
                    "say 'small'",
                    "else",
                    "say 'none'",
                    "say 'at all'",
                    "end",
                ],
            )
        };
        assert_eq!(grade(&mut sys, 500), ["large"]);
        assert_eq!(grade(&mut sys, 12), ["medium, even"]);
        assert_eq!(grade(&mut sys, 13), ["medium, odd"]);
        assert_eq!(grade(&mut sys, 5), ["small"]);
        assert_eq!(grade(&mut sys, 0), ["none", "at all"]);
        // nothing is shown until the block ends, and nothing is run
        assert_eq!(type_lines(&mut sys, &["if n = 0", "say 'zero'"]), (true, vec![]));
        assert!(sys.blocks[0].open());
        assert_eq!(shell::prompt(&sys, 0), shell::CONTINUE);
        assert_eq!(said(&mut sys, &["end"]), ["zero"]);
        assert_eq!(shell::prompt(&sys, 0), shell::PROMPT);
        // a condition that is nothing does not hold
        assert_eq!(said(&mut sys, &["if null > 1", "say 'yes'", "else", "say 'no'", "end"]), ["no"]);
    }

    #[test]
    fn loops_run_lines_again() {
        let mut sys = books();
        assert_eq!(said(&mut sys, &["let n = 3", "let s = ''", "while n > 0", "let s = s || n", "let n = n - 1", "end", "say s"]), ["321"]);
        let (ok, out) = type_lines(
            &mut sys,
            &["for row in select label, amount from expenses where amount > 2 order by amount desc", "say row.label + ': ' + row.amount", "end"],
        );
        assert!(ok);
        assert_eq!(out, ["train: 42.00", "coffee: 3.50"]);
        // loops inside loops, and changes made from a loop
        type_lines(&mut sys, &["create table doubled (label text, amount decimal)"]);
        let (ok, out) = type_lines(
            &mut sys,
            &[
                "for row in select label, amount from expenses",
                "    let times = 2",
                "    while times > 0",
                "        insert into doubled values (row.label, row.amount * times)",
                "        let times = times - 1",
                "    end",
                "end",
            ],
        );
        assert!(ok, "{out:?}");
        // at the prompt each change is announced; in a program it is not
        assert_eq!(out.len(), 6);
        assert_eq!(out[5], "1 row added to doubled, with id 6.");
        type_lines(&mut sys, &["program quiet", "insert into doubled values ('x', 0)", "mood calm", "mood", "say 'done'", "delete from doubled where label = 'x'", "end"]);
        assert_eq!(said(&mut sys, &["quiet"]), ["Mood is calm.", "done"]);
        assert_eq!(said(&mut sys, &["select count(*), sum(amount) from doubled"]), ["   count  sum", "   6      140.10"]);
        // a loop that never ends is stopped
        let (ok, out) = type_lines(&mut sys, &["while 1 = 1", "let n = n + 1", "end"]);
        assert!(!ok);
        assert_eq!(out, ["This loop went round 10000 times and was stopped.", "Stopped at line 1."]);
        assert_eq!(said(&mut sys, &["say n"]), ["10000"]);
        // a loop reads every row of a stored table, however many
        type_lines(&mut sys, &["create table many (n int)"]);
        for _ in 0..5 {
            type_lines(&mut sys, &["insert into many values (1), (2), (3), (4), (5), (6), (7), (8)"]);
        }
        let (ok, out) = type_lines(&mut sys, &["for row in select n from many", "say row.n", "end"]);
        assert!(ok);
        assert_eq!((out.len(), out[39].as_str()), (40, "8"));
        // and one that would not see every row of what is sorted says so
        let (ok, out) = type_lines(&mut sys, &["for row in select n from many order by n", "say row.n", "end"]);
        assert!(!ok);
        assert_eq!(out, ["A loop takes 32 rows at most; this query has 40.", "Stopped at line 1."]);
        assert!(type_lines(&mut sys, &["for row in select n from many limit 3", "let n = row.n", "end"]).0);
    }

    #[test]
    fn a_line_that_fails_stops_the_block_and_says_where() {
        let mut sys = books();
        let (ok, out) = type_lines(&mut sys, &["if 1 = 1", "say 'before'", "let x = 1 / 0", "say 'after'", "end"]);
        assert!(!ok);
        assert_eq!(out, ["before", "Division by zero.", "Stopped at line 3."]);
        let (ok, out) = type_lines(&mut sys, &["if 1 = 1", "frobnicate", "end"]);
        assert!(!ok);
        assert_eq!(out, ["Unknown command: frobnicate", "Stopped at line 2."]);
        let (ok, out) = type_lines(&mut sys, &["if 1 = 1", "insert into expenses values (1, 2)", "end"]);
        assert!(!ok);
        assert_eq!(out, ["Column label takes a 'text' between quotes.", "Stopped at line 2."]);
        // a single line needs no place
        assert_eq!(said(&mut sys, &["let x = 1 / 0"]), ["Division by zero."]);
        assert_eq!(said(&mut sys, &["end"]), ["end closes if, while, for or program: none is open."]);
        assert_eq!(said(&mut sys, &["else"]), ["else belongs to an if."]);
        // what is wrong with the form of a block is told when it ends, whatever would have run
        for condition in ["if 1 = 1", "if 1 = 2"] {
            assert_eq!(
                type_lines(&mut sys, &[condition, "else", "else", "end"]),
                (false, vec!["After else comes nothing, or if and a condition; a plain else is last.".to_string(), "Line 3 is not right: nothing was run.".to_string()])
            );
        }
        assert_eq!(said(&mut sys, &["for x in tables", "end"]), ["A loop over rows is written: for row in select ...", "Line 1 is not right: nothing was run."]);
        // lines typed in a block can be dropped
        assert_eq!(said(&mut sys, &["while 1 = 1", "say 'forever'", "cancel"]), ["The lines are dropped."]);
        assert!(!sys.blocks[0].open());
        assert_eq!(said(&mut sys, &["cancel"]), ["Unknown command: cancel"]);
    }

    #[test]
    fn programs_are_kept_in_the_database_and_run_by_name() {
        let mut sys = books();
        assert_eq!(said(&mut sys, &["programs"]), ["There is no program yet. help l shows how to make one."]);
        let (ok, out) = type_lines(
            &mut sys,
            &[
                "program Report",
                "# what was spent, largest first",
                "for row in select label, amount from expenses order by amount desc",
                "    if row.amount > 10",
                "        say upper(row.label) + ' ' + row.amount",
                "    else",
                "        say row.label + ' ' + row.amount",
                "    end",
                "end",
                "let total = select sum(amount) from expenses",
                "say 'Total: ' + total",
                "end",
            ],
        );
        assert!(ok, "{out:?}");
        assert_eq!(out, ["Program report kept: 10 lines."]);
        let report = ["TRAIN 42.00", "coffee 3.50", "bread 1.20", "Total: 46.70"];
        assert_eq!(said(&mut sys, &["run report"]), report);
        assert_eq!(said(&mut sys, &["report"]), report);
        assert_eq!(said(&mut sys, &["REPORT"]), report);
        // it is a table like any other, and listed
        assert_eq!(said(&mut sys, &["select count(*) from programs where name = 'report'"]), ["   count", "   10"]);
        assert_eq!(said(&mut sys, &["programs"]), ["     name    lines  runs", " [1] report  10     when asked"]);
        let (ok, shown) = type_lines(&mut sys, &["1"]);
        assert!(ok);
        assert_eq!(shown[0], "program report");
        assert_eq!(shown[1], "    # what was spent, largest first");
        assert_eq!(shown[3], "        if row.amount > 10");
        assert_eq!(shown[4], "            say upper(row.label) + ' ' + row.amount");
        assert_eq!(shown[5], "        else");
        assert_eq!(shown[11], "end");
        assert_eq!(shown.len(), 12);
        // typing it again replaces it
        assert_eq!(said(&mut sys, &["program report", "say 'short'", "end"]), ["Program report replaced: 1 line."]);
        assert_eq!(said(&mut sys, &["report"]), ["short"]);
        assert_eq!(said(&mut sys, &["drop program report"]), ["Program report removed."]);
        assert_eq!(said(&mut sys, &["report"]), ["Unknown command: report"]);
        assert_eq!(said(&mut sys, &["run report"]), ["Unknown program: report. Type programs to list them."]);
        assert_eq!(said(&mut sys, &["drop program report"]), ["Unknown program: report. Type programs to list them."]);
        assert_eq!(said(&mut sys, &["show report"]), ["Unknown program: report. Type programs to list them."]);
        // names that would hide a command are refused
        assert_eq!(said(&mut sys, &["program tables", "end"]), ["A program is named by a letter, then letters, digits or _, and not as a command."]);
        assert_eq!(said(&mut sys, &["program if", "end"]), ["A program is named by a letter, then letters, digits or _, and not as a command."]);
        assert_eq!(said(&mut sys, &["program", "end"]), ["A program is named by a letter, then letters, digits or _, and not as a command."]);
        assert_eq!(said(&mut sys, &["program empty", "end"]), ["Program empty kept: 0 lines."]);
        assert_eq!(type_lines(&mut sys, &["empty"]), (true, vec![]));
    }

    #[test]
    fn programs_run_programs_and_say_where_they_fail() {
        let mut sys = System::new();
        type_lines(&mut sys, &["program inner(depth, zero)", "say 'in'", "if depth > 1", "return", "end", "let x = 1 / zero", "say 'not reached'", "end"]);
        type_lines(&mut sys, &["program outer(depth)", "say 'out'", "inner(depth, 0)", "say 'back'", "end"]);
        // `return` leaves the program it is in, not the one that ran it
        assert_eq!(type_lines(&mut sys, &["outer(2)"]), (true, vec!["out".to_string(), "in".to_string(), "back".to_string()]));
        let (ok, out) = type_lines(&mut sys, &["outer(0)"]);
        assert!(!ok);
        assert_eq!(out, ["out", "in", "Division by zero.", "Stopped at line 5 of inner.", "Stopped at line 2 of outer."]);
        // a program that runs itself is stopped
        type_lines(&mut sys, &["program again(depth)", "say depth", "run again(depth + 1)", "end"]);
        let (ok, out) = type_lines(&mut sys, &["again(1)"]);
        assert!(!ok);
        assert_eq!(out[..4], ["1", "2", "3", "Programs run programs three deep at most."]);
        assert_eq!(out.len(), 7);
        // and the prompt has its names, which no program saw
        assert_eq!(said(&mut sys, &["let depth = 9", "again(2)", "say depth"]), ["9"]);
        // programs are made at the prompt
        assert_eq!(said(&mut sys, &["if 1 = 1", "program nested", "end", "end"]), ["A program is made at the prompt, not inside another.", "Line 2 is not right: nothing was run."]);
    }

    thread_local! {
        /// Whether the tests of this thread give their systems rooms, so that
        /// the programs that run by themselves run as jobs.
        static AS_JOBS: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
    }

    /// Lets the system run what waits, and returns what the programs said.
    /// As jobs, they are given their turns until none is left to run.
    fn in_background(sys: &mut System) -> Vec<String> {
        let mut out = Lines::default();
        if !AS_JOBS.get() {
            background(sys, &mut out);
            return out.lines;
        }
        if sys.rooms.is_none() {
            crate::machine::tests::rooms(sys, 2);
        }
        for _ in 0..100_000 {
            background(sys, &mut out);
            if crate::jobs::idle(sys) {
                return out.lines;
            }
            // one instruction a turn: the worst case for what must be put back between two
            crate::jobs::work(sys, &mut out, &mut || true);
        }
        panic!("the programs never end: {:?}", out.lines);
    }

    #[test]
    fn the_programs_that_run_by_themselves_run_the_same_as_jobs() {
        AS_JOBS.set(true);
        // with rooms, the background starts a program and does not run it: its turns do
        let mut sys = books();
        crate::machine::tests::rooms(&mut sys, 2);
        type_lines(&mut sys, &["on notify alerts", "say 'heard ' + text", "say 'twice'", "end", "notify alerts, 'this'", "notify alerts, 'that'"]);
        let mut log = Lines::default();
        background(&mut sys, &mut log);
        background(&mut sys, &mut log);
        assert!(log.lines.is_empty(), "{:?}", log.lines);
        let job = &sys.jobs.list()[1];
        assert_eq!((job.program.bytes(), job.by, job.state, job.watch), (&b"on_notify_alerts"[..], BACKGROUND, crate::jobs::State::Ready, crate::jobs::Watch::Nobody));
        // one at a time: the second waits for the first to end
        crate::jobs::work(&mut sys, &mut log, &mut || true);
        background(&mut sys, &mut log);
        assert_eq!(log.lines, ["on_notify_alerts: heard this"]);
        crate::jobs::work(&mut sys, &mut log, &mut || false);
        assert_eq!(log.lines, ["on_notify_alerts: heard this", "on_notify_alerts: twice"]);
        assert!(sys.jobs.list()[1].ended() && sys.firing.is_empty() && sys.session == crate::events::SCREEN);
        background(&mut sys, &mut log);
        crate::jobs::work(&mut sys, &mut log, &mut || false);
        assert_eq!(log.lines[2..], ["on_notify_alerts: heard that", "on_notify_alerts: twice"]);
        // they are not written in the record of the jobs people start
        assert!(sys.db.find(crate::jobs::LOG).is_none());
        a_program_runs_when_a_row_is_added();
        programs_run_on_changes_removals_and_notifications();
        a_program_runs_again_and_again();
        what_goes_wrong_in_the_background_is_said_in_the_log();
        a_program_that_runs_by_itself_neither_takes_a_disk_nor_opens_the_machine();
        a_program_that_hears_something_knows_the_time();
        a_program_runs_every_day_at_its_time();
        the_first_line_of_a_program_that_runs_by_itself_is_checked();
        programs_that_start_each_other_keep_their_order_and_their_limit();
        a_program_that_runs_by_itself_gives_what_it_knows_to_another();
    }

    #[test]
    fn a_program_that_runs_by_itself_gives_what_it_knows_to_another() {
        let mut sys = books();
        type_lines(&mut sys, &["program audit(what, row)", "say 'audit ' + what + ' ' + row", "end"]);
        type_lines(&mut sys, &["on insert into expenses", "audit(new.label, id)", "say 'back ' + new.label", "end", "insert into expenses values ('milk', 1.5)"]);
        assert_eq!(in_background(&mut sys), ["on_insert_expenses: audit milk 4", "on_insert_expenses: back milk"]);
        // what the system gave the first is its own: a program it runs does not see it
        type_lines(&mut sys, &["program peeks", "say text", "end", "on notify peek", "peeks", "end", "notify peek, 'this'"]);
        assert_eq!(
            in_background(&mut sys),
            ["on_notify_peek: Unknown name: text", "on_notify_peek: Stopped at line 1 of peeks.", "on_notify_peek: Stopped at line 1 of on_notify_peek."]
        );
        // and the prompt's names are no more its own than before
        type_lines(&mut sys, &["let id = 'mine'", "insert into expenses values ('tea', 2)"]);
        assert_eq!(in_background(&mut sys), ["on_insert_expenses: audit tea 5", "on_insert_expenses: back tea"]);
        assert_eq!(said(&mut sys, &["say id"]), ["mine"]);
    }

    #[test]
    fn programs_that_start_each_other_keep_their_order_and_their_limit() {
        let mut sys = books();
        type_lines(&mut sys, &["create table a (n int)", "create table b (n int)", "create table c (n int)"]);
        // a row in a adds one in b and tells of it, b in c, c in a again: round and round, until the chain is cut
        type_lines(&mut sys, &["on insert into a", "say 'a ' + new.n", "insert into b values (new.n + 1)", "notify heard, 'from a'", "end"]);
        type_lines(&mut sys, &["on insert into b", "say 'b ' + new.n", "insert into c values (new.n + 1)", "end"]);
        type_lines(&mut sys, &["on insert into c", "say 'c ' + new.n", "insert into a values (new.n + 1)", "end"]);
        type_lines(&mut sys, &["on notify heard", "say 'heard ' + text", "end"]);
        // two programs for the same thing run in the order they were made
        type_lines(&mut sys, &["on update a", "say 'changed'", "end", "insert into a values (1)"]);
        let mut said = Vec::new();
        for _ in 0..12 {
            said.extend(in_background(&mut sys));
        }
        assert_eq!(
            said,
            [
                "on_insert_a: a 1",
                "on_insert_b: b 2",
                "on_notify_heard: heard from a",
                "on_insert_c: c 3",
                "on_insert_a: a 4",
                "on_insert_b: not run, programs kept starting each other.",
                "on_notify_heard: not run, programs kept starting each other.",
            ]
        );
        // the names of each run are its own: nothing of the last one is left for the next
        type_lines(&mut sys, &["on notify fresh", "say made", "let made = 1", "end", "notify fresh", "notify fresh"]);
        let mut said = Vec::new();
        for _ in 0..4 {
            said.extend(in_background(&mut sys));
        }
        assert_eq!(said, ["on_notify_fresh: Unknown name: made", "on_notify_fresh: Stopped at line 1 of on_notify_fresh."].repeat(2));
    }

    #[test]
    fn a_program_runs_when_a_row_is_added() {
        let mut sys = books();
        type_lines(&mut sys, &["create table alerts (label text, why text)"]);
        let (ok, out) = type_lines(
            &mut sys,
            &[
                "on insert into expenses",
                "    if new.amount > 50",
                "        say 'big expense: ' + new.label",
                "        insert into alerts values (new.label, 'row ' || id || ' of ' || channel)",
                "    end",
                "end",
            ],
        );
        assert!(ok);
        assert_eq!(out, ["Program on_insert_expenses kept: 4 lines.", "It runs on insert into expenses. What it says goes to the log."]);
        // nothing happens until something is added
        assert_eq!(in_background(&mut sys), Vec::<String>::new());
        type_lines(&mut sys, &["insert into expenses values ('tea', 2), ('rent', 800)"]);
        // the change is made first; the program runs after, for each row
        assert_eq!(said(&mut sys, &["select count(*) from alerts"]), ["   count", "   0"]);
        assert_eq!(in_background(&mut sys), ["on_insert_expenses: big expense: rent"]);
        assert_eq!(said(&mut sys, &["select label, why from alerts"]), ["   label  why", "   rent   row 5 of expenses"]);
        assert_eq!(in_background(&mut sys), Vec::<String>::new());
        // the names of what happened are the program's own
        assert_eq!(said(&mut sys, &["say id"]), ["Unknown name: id"]);
        // changes and removals are other things
        type_lines(&mut sys, &["update expenses set amount = 900 where label = 'tea'", "delete from expenses where label = 'rent'"]);
        assert_eq!(in_background(&mut sys), Vec::<String>::new());
        assert_eq!(said(&mut sys, &["programs"]), ["     name                lines  runs", " [1] on_insert_expenses  4      on insert into expenses"]);
        assert_eq!(said(&mut sys, &["show on_insert_expenses"])[..2], ["on insert into expenses", "    if new.amount > 50"]);
        assert_eq!(said(&mut sys, &["drop program on_insert_expenses"]), ["Program on_insert_expenses removed."]);
        type_lines(&mut sys, &["insert into expenses values ('car', 9000)"]);
        assert_eq!(in_background(&mut sys), Vec::<String>::new());
    }

    #[test]
    fn programs_run_on_changes_removals_and_notifications() {
        let mut sys = books();
        type_lines(&mut sys, &["on update expenses", "say 'now ' + new.amount + ' for ' + new.label", "end"]);
        type_lines(&mut sys, &["on delete from expenses", "say 'gone: ' + id", "end"]);
        type_lines(&mut sys, &["on notify Alerts", "say channel + ' says ' + text", "mood intense", "end"]);
        type_lines(&mut sys, &["update expenses set amount = amount * 2 where amount < 2", "delete from expenses where id = 1", "notify alerts 'fire'"]);
        assert_eq!(in_background(&mut sys), ["on_update_expenses: now 2.40 for bread", "on_delete_expenses: gone: 1", "on_notify_alerts: alerts says fire"]);
        // a program is a session without a prompt: it can do what a prompt does
        assert_eq!(said(&mut sys, &["mood"]), ["Mood is intense."]);
        // a notify with the name of a table is not a change to the table
        type_lines(&mut sys, &["notify expenses 'delete 2'"]);
        assert_eq!(in_background(&mut sys), Vec::<String>::new());
        // a row that is gone before its program runs has nothing to say
        type_lines(&mut sys, &["update expenses set amount = 1 where id = 2", "delete from expenses where id = 2"]);
        assert_eq!(in_background(&mut sys), ["on_delete_expenses: gone: 2"]);
        let listed = said(&mut sys, &["programs"]);
        assert_eq!(listed[3], " [3] on_notify_alerts    2      on notify alerts");
    }

    #[test]
    fn a_program_runs_again_and_again() {
        let mut sys = System::new();
        type_lines(&mut sys, &["create table samples (at int)"]);
        sys.ticks = 1000;
        let (_, out) = type_lines(&mut sys, &["every 2 Seconds", "insert into samples values (0)", "say 'sampled'", "end"]);
        assert_eq!(out, ["Program every_2_seconds kept: 2 lines.", "It runs every 2 seconds. What it says goes to the log."]);
        assert_eq!(in_background(&mut sys), Vec::<String>::new());
        sys.ticks = 1199;
        assert_eq!(in_background(&mut sys), Vec::<String>::new());
        sys.ticks = 1200;
        assert_eq!(in_background(&mut sys), ["every_2_seconds: sampled"]);
        assert_eq!(in_background(&mut sys), Vec::<String>::new());
        // a turn that was missed is not run twice
        sys.ticks = 2000;
        assert_eq!(in_background(&mut sys).len(), 1);
        assert_eq!(in_background(&mut sys).len(), 0);
        sys.ticks = 2200;
        assert_eq!(in_background(&mut sys).len(), 1);
        assert_eq!(said(&mut sys, &["select count(*) from samples"]), ["   count", "   3"]);
        // another program does not make it lose its turn
        sys.ticks = 2300;
        type_lines(&mut sys, &["every 1 hour", "say 'hourly'", "end"]);
        sys.ticks = 2400;
        assert_eq!(in_background(&mut sys), ["every_2_seconds: sampled"]);
        assert_eq!(said(&mut sys, &["programs"])[2], " [2] every_1_hour     1      every 1 hour");
        sys.ticks = 2300 + 3600 * 100;
        assert_eq!(in_background(&mut sys), ["every_2_seconds: sampled", "every_1_hour: hourly"]);
    }

    #[test]
    fn what_goes_wrong_in_the_background_is_said_in_the_log() {
        let mut sys = books();
        type_lines(&mut sys, &["on insert into expenses", "say 'seen'", "let x = new.amount / 0", "end"]);
        type_lines(&mut sys, &["insert into expenses values ('tea', 2)"]);
        assert_eq!(
            in_background(&mut sys),
            ["on_insert_expenses: seen", "on_insert_expenses: Division by zero.", "on_insert_expenses: Stopped at line 2 of on_insert_expenses."]
        );
        // the change that started it stays
        assert_eq!(said(&mut sys, &["select count(*) from expenses"]), ["   count", "   4"]);
        // a program that starts itself again and again is stopped
        type_lines(&mut sys, &["on insert into expenses", "insert into expenses values ('echo', new.amount + 1)", "end"]);
        type_lines(&mut sys, &["insert into expenses values ('first', 1)"]);
        let mut said_all = Vec::new();
        for _ in 0..10 {
            said_all.extend(in_background(&mut sys));
        }
        assert_eq!(said_all, ["on_insert_expenses: not run, programs kept starting each other."]);
        assert_eq!(type_lines(&mut sys, &["select count(*) from expenses where label = 'echo'"]).1, ["   count", "   4"]);
        // only a prompt listens
        type_lines(&mut sys, &["on notify ping", "listen ping", "end", "notify ping"]);
        assert_eq!(in_background(&mut sys)[0], "on_notify_ping: Only a prompt listens. A program waits for events with: on notify ...");
    }

    #[test]
    fn a_program_that_runs_by_itself_neither_takes_a_disk_nor_opens_the_machine() {
        let mut sys = System::new();
        // written over the network, where those commands are refused: a
        // program is not a way around
        sys.session = crate::events::NETWORK;
        let (ok, _) = type_lines(&mut sys, &["on notify door", "    install 1 yes", "    disk erase yes", "    web on", "    remote on", "    say 'after'", "end"]);
        assert!(ok);
        type_lines(&mut sys, &["notify door 'open'"]);
        sys.session = crate::events::SCREEN;
        // it stops at the first line that fails, and each of the four does
        for refused in ["install takes a disk", "disk erase takes a disk", "web on is typed", "remote on is typed"] {
            let said = in_background(&mut sys);
            assert!(said.len() == 2 && said[0].starts_with(&format!("on_notify_door: {refused}")) && said[0].ends_with("at the machine itself."), "{said:?}");
            assert!(sys.request.is_none() && sys.web.code().is_none() && sys.remote.code().is_none());
            // the next one, with the refused line taken out by who is at the machine
            type_lines(&mut sys, &["delete from programs where line = 1 and name = 'on_notify_door'", "update programs set line = line - 1 where line > 1", "notify door 'again'"]);
        }
        assert_eq!(in_background(&mut sys), ["on_notify_door: after"]);
        // run by who is at the machine, a program may
        type_lines(&mut sys, &["program doors", "    web on", "end"]);
        assert!(type_lines(&mut sys, &["run doors"]).0 && sys.web.code().is_some());
    }

    #[test]
    fn a_program_that_hears_something_knows_the_time() {
        use crate::clock::Moment;
        let mut sys = books();
        sys.clock = Some((Moment { year: 2026, month: 11, day: 27, hour: 15, minute: 4, second: 5 }.seconds(), sys.ticks));
        type_lines(&mut sys, &["create table heard (label text, at text, day text)"]);
        // what happened has names of its own: the time is known beside them
        type_lines(&mut sys, &["on insert into expenses", "insert into heard values (new.label, now, today)", "end"]);
        type_lines(&mut sys, &["on notify alerts", "say today + ': ' + text", "end"]);
        type_lines(&mut sys, &["insert into expenses values ('tea', 2)", "notify alerts 'look'"]);
        assert_eq!(in_background(&mut sys), ["on_notify_alerts: 2026-11-27: look"]);
        assert_eq!(said(&mut sys, &["select label, at, day from heard"]), ["   label  at                   day", "   tea    2026-11-27 15:04:05  2026-11-27"]);
    }

    #[test]
    fn a_program_runs_every_day_at_its_time() {
        use crate::clock::Moment;
        let mut sys = books();
        let at = |day: u32, hour: u32, minute: u32, second: u32| Moment { year: 2026, month: 11, day, hour, minute, second }.seconds();
        // a machine without a clock keeps the program and says it will wait
        let (ok, out) = type_lines(&mut sys, &["every day at 8:00", "say 'good morning, it is ' + now", "end"]);
        assert!(ok);
        assert_eq!(
            out,
            [
                "Program every_day_at_8h00 kept: 1 line.",
                "It runs every day at 8:00. What it says goes to the log.",
                "The clock of this machine tells no time: until it does, it will not run."
            ]
        );
        sys.ticks += 100 * 86_400 * 3;
        assert!(in_background(&mut sys).is_empty());

        // the clock is read: it is 7:59:58 on the 18th
        sys.clock = Some((at(18, 7, 59, 58), sys.ticks));
        sys.triggers.changed();
        assert!(in_background(&mut sys).is_empty());
        sys.ticks += 150;
        assert!(in_background(&mut sys).is_empty(), "7:59:59 is not 8:00");
        sys.ticks += 100;
        assert_eq!(in_background(&mut sys), ["every_day_at_8h00: good morning, it is 2026-11-18 08:00:00"]);
        // once, and not again that day
        sys.ticks += 100 * 3600 * 23;
        assert!(in_background(&mut sys).is_empty());
        sys.ticks += 100 * 3600;
        assert_eq!(in_background(&mut sys), ["every_day_at_8h00: good morning, it is 2026-11-19 08:00:00"]);
        // a machine that was off at 8:00 does not run it late: the next one is tomorrow's
        sys.clock = Some((at(21, 9, 30, 0), sys.ticks));
        sys.triggers.changed();
        assert!(in_background(&mut sys).is_empty());
        sys.ticks += 100 * 3600 * 22 + 100 * 60 * 30;
        assert_eq!(in_background(&mut sys), ["every_day_at_8h00: good morning, it is 2026-11-22 08:00:00"]);

        // it is listed with its time, shown, replaced and dropped like any other
        assert_eq!(said(&mut sys, &["programs"])[1], " [1] every_day_at_8h00  1      every day at 8:00");
        assert_eq!(said(&mut sys, &["show every_day_at_8h00"])[0], "every day at 8:00");
        let (_, out) = type_lines(&mut sys, &["every day at 08:00", "say 'again'", "end"]);
        assert_eq!(out[0], "Program every_day_at_8h00 replaced: 1 line.");
        assert_eq!(out.len(), 2, "the clock tells the time now");
        let (_, out) = type_lines(&mut sys, &["every day at 17:45", "say 'evening'", "end"]);
        assert_eq!(out[0], "Program every_day_at_17h45 kept: 1 line.");
        sys.ticks += 100 * 3600 * 9 + 100 * 60 * 45;
        assert_eq!(in_background(&mut sys), ["every_day_at_17h45: evening"]);
        assert!(type_lines(&mut sys, &["drop program every_day_at_17h45"]).0);
        sys.ticks += 100 * 86_400;
        assert_eq!(in_background(&mut sys), ["every_day_at_8h00: again"]);
    }

    #[test]
    fn the_first_line_of_a_program_that_runs_by_itself_is_checked() {
        let mut sys = books();
        let on = "Write: on insert into t, on update t, on delete from t, or on notify channel";
        let every = "Write: every 5 minutes (or seconds, hours, days), or every day at 8:00";
        for (first, wanted) in [
            ("on insert expenses", on),
            ("on insert into", on),
            ("on update of expenses", on),
            ("on delete expenses", on),
            ("on create expenses", on),
            ("on notify", on),
            ("on notify two words", on),
            ("every minute", every),
            ("every 0 minutes", every),
            ("every 5 weeks", every),
            ("every 5 minutes please", every),
            ("every day", every),
            ("every day at", every),
            ("every day at noon", every),
            ("every day at 25:00", every),
            ("every day at 8:00 sharp", every),
            ("every day by 8:00", every),
            ("on insert into shoes", "Unknown table: shoes. Type tables to list them."),
            ("on update tasks", "tasks is a view of the system: it does not say when it changes."),
        ] {
            assert_eq!(said(&mut sys, &[first, "end"]), [wanted], "{first}");
        }
        assert_eq!(runs(b"on INSERT into Expenses").map(|runs| runs.name()).ok().unwrap().bytes(), b"on_insert_expenses");
        assert_eq!(runs(b"every 1 hours").map(|runs| runs.line()).ok().unwrap().bytes(), b"every 1 hour");
        assert_eq!(runs(b"every 30 second").map(|runs| (runs.line(), runs.period())).ok().unwrap().1, 3000);
        assert_eq!(runs(b"every 2 days").map(|runs| runs.period()).ok().unwrap(), 2 * 86400 * 100);
        // typing one again replaces it
        type_lines(&mut sys, &["on delete from expenses", "say 'one'", "end"]);
        assert_eq!(said(&mut sys, &["on delete from EXPENSES", "say 'two'", "end"])[0], "Program on_delete_expenses replaced: 1 line.");
        type_lines(&mut sys, &["delete from expenses where id = 1"]);
        assert_eq!(in_background(&mut sys), ["on_delete_expenses: two"]);
        // there is room for eight that run again and again
        for n in 1..=8 {
            assert!(type_lines(&mut sys, &[&format!("every {n} days"), "end"]).0);
        }
        assert_eq!(said(&mut sys, &["every 9 days", "end"]), ["There is room for 16 programs that wait for events and 8 that run again and again."]);
        assert!(type_lines(&mut sys, &["every 8 days", "say 'replaced'", "end"]).0);
    }

    #[test]
    fn programs_that_run_by_themselves_are_found_again_after_a_restart() {
        let mut sys = books();
        type_lines(&mut sys, &["on insert into expenses", "say 'added ' + new.label", "end"]);
        // as after a start: the table of programs is all there is
        sys.triggers = Triggers::new();
        sys.events = crate::events::Events::new();
        assert_eq!(in_background(&mut sys), Vec::<String>::new());
        type_lines(&mut sys, &["insert into expenses values ('tea', 2)"]);
        assert_eq!(in_background(&mut sys), ["on_insert_expenses: added tea"]);
        // removing its table by hand stops it
        type_lines(&mut sys, &["drop table programs", "insert into expenses values ('milk', 1)"]);
        assert_eq!(in_background(&mut sys), Vec::<String>::new());
    }

    #[test]
    fn a_program_is_still_there_after_a_restart() {
        /// Memory that says it is a disk, and can be opened twice.
        struct Shared(std::rc::Rc<std::cell::RefCell<xila_db::Memory>>);

        impl xila_db::Disk for Shared {
            fn name(&self) -> &'static str {
                "shared"
            }
            fn pages(&self) -> u64 {
                self.0.borrow().pages()
            }
            fn read(&mut self, page: u64, into: &mut xila_db::Page) -> Result<(), xila_db::DiskError> {
                self.0.borrow_mut().read(page, into)
            }
            fn write(&mut self, page: u64, from: &xila_db::Page) -> Result<(), xila_db::DiskError> {
                self.0.borrow_mut().write(page, from)
            }
            fn flush(&mut self) -> Result<(), xila_db::DiskError> {
                Ok(())
            }
        }

        let disk = std::rc::Rc::new(std::cell::RefCell::new(xila_db::Memory::new(256)));
        let open = || xila_db::Database::open(xila_db::Volume::single(Box::new(Shared(disk.clone())))).ok().unwrap();
        // longer than the 48 characters a column of text shows in a result
        let long = "say select count(*) from programs where name = 'hello' and line > 0 and line < 1000";
        {
            let mut sys = System::new();
            sys.db = open();
            assert_eq!(said(&mut sys, &["program hello", "say 'hello'", long, "end"]), ["Program hello kept: 2 lines."]);
        }
        let mut sys = System::new();
        sys.db = open();
        assert_eq!(said(&mut sys, &["hello"]), ["hello", "2"]);
        assert_eq!(said(&mut sys, &["show hello"])[2], format!("    {long}"));
        // a table of that name that is something else is not taken for it
        type_lines(&mut sys, &["drop table programs", "create table programs (title text)"]);
        assert_eq!(said(&mut sys, &["program other", "end"]), ["The table programs is not the one the system keeps programs in."]);
        assert_eq!(said(&mut sys, &["hello"]), ["Unknown command: hello"]);
    }

    #[test]
    fn long_lines_are_run_whole_or_refused() {
        let mut sys = System::new();
        type_lines(&mut sys, &["create table t (n int)"]);
        // as long as the serial port takes them
        let values: Vec<String> = (0..8).map(|n| format!("({n} + 100000000000000000)")).collect();
        let long = format!("insert into t values {}", values.join(", "));
        assert!(long.len() > 200 && long.len() <= LINE_MAX);
        assert_eq!(said(&mut sys, &[&long]), ["8 rows added to t, with ids 1 to 8."]);
        assert_eq!(said(&mut sys, &["if 1 = 1", &long, "end"]), ["8 rows added to t, with ids 9 to 16."]);
        let longer = format!("select count(*) from t where n = 1{}", " or n = 1".repeat(30));
        assert_eq!(said(&mut sys, &[&longer]), ["A line takes 240 characters at most."]);
        assert_eq!(said(&mut sys, &["if 1 = 1", &longer]), ["A block takes 48 lines of 240 characters at most. The lines are dropped."]);
        assert!(!sys.blocks[0].open());
    }

    #[test]
    fn notify_takes_a_value() {
        let mut sys = System::new();
        type_lines(&mut sys, &["listen alerts", "let who = 'bread'"]);
        assert_eq!(said(&mut sys, &["notify alerts, 'big expense: ' + who"]), ["Sent to 1 listener."]);
        assert_eq!(sys.events.take(0).map(|event| String::from_utf8_lossy(event.text.bytes()).into_owned()), Some("big expense: bread".to_string()));
        assert_eq!(said(&mut sys, &["notify alerts 2 * 21"]), ["Sent to 1 listener."]);
        assert_eq!(sys.events.take(0).map(|event| String::from_utf8_lossy(event.text.bytes()).into_owned()), Some("42".to_string()));
        assert_eq!(said(&mut sys, &["notify alerts hello"]), ["Put the text between quotes, or give a name that has a value, not: hello"]);
    }

    #[test]
    fn a_program_with_a_fault_of_form_is_not_kept() {
        let mut sys = books();
        let (ok, out) = type_lines(&mut sys, &["program report", "say 'one'", "say 2 +", "end"]);
        assert!(!ok);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[1], "Line 2 is not right: the program is not kept.");
        assert_eq!(said(&mut sys, &["report"]), ["Unknown command: report"]);
        assert_eq!(said(&mut sys, &["programs"]), ["There is no program yet. help l shows how to make one."]);
        // one that was there stays as it was
        type_lines(&mut sys, &["program report", "say 'good'", "end"]);
        let (ok, out) = type_lines(&mut sys, &["program report", "say 'worse'", "else", "end"]);
        assert!(!ok);
        assert_eq!(out, ["else belongs to an if.", "Line 2 is not right: the program is not kept."]);
        assert_eq!(said(&mut sys, &["report"]), ["good"]);
        // and so does one that runs by itself, with what starts it
        type_lines(&mut sys, &["on insert into expenses", "say 'seen'", "end"]);
        let (ok, out) = type_lines(&mut sys, &["on insert into expenses", "for x in tables", "end", "end"]);
        assert!(!ok);
        assert_eq!(out, ["A loop over rows is written: for row in select ...", "Line 1 is not right: the program is not kept."]);
        type_lines(&mut sys, &["insert into expenses values ('tea', 2)"]);
        let mut log = Lines::default();
        background(&mut sys, &mut log);
        assert_eq!(log.lines, ["on_insert_expenses: seen"]);
        // a block that is run at once says that nothing was
        let (ok, out) = type_lines(&mut sys, &["if 1 = 1", "say 'ran'", "else", "else", "end"]);
        assert!(!ok);
        assert_eq!(out, ["After else comes nothing, or if and a condition; a plain else is last.", "Line 4 is not right: nothing was run."]);
    }

    #[test]
    fn a_program_kept_before_with_a_fault_of_form_says_it_when_run() {
        let mut sys = books();
        type_lines(&mut sys, &["program two", "say 1", "say 2", "end"]);
        // as an older system would have kept it: stored without being compiled
        store(&mut sys.db, b"old", None, &[Line::from_bytes(b"say 'first'"), Line::from_bytes(b"else")]).unwrap();
        let (ok, out) = type_lines(&mut sys, &["old"]);
        assert!(!ok);
        assert_eq!(out, ["else belongs to an if.", "Stopped at line 2 of old."]);
        // nothing of it runs, and who ran it says where it stopped
        assert_eq!(said(&mut sys, &["if 1 = 1", "say 1", "old", "end"]), ["1", "else belongs to an if.", "Stopped at line 2 of old.", "Stopped at line 3."]);
        // it is shown as any program of two lines is, and can be removed
        assert_eq!(said(&mut sys, &["show old"]).len(), said(&mut sys, &["show two"]).len());
        assert!(type_lines(&mut sys, &["drop program old"]).0);
    }

    #[test]
    fn a_program_that_runs_by_itself_types_nothing_at_a_prompt() {
        let mut sys = books();
        // qr shows a code on the screen of who asked for it: a program that runs by itself is nobody there
        type_lines(&mut sys, &["on insert into expenses", "say 'before'", "qr about", "say 'not here'", "end"]);
        type_lines(&mut sys, &["insert into expenses values ('tea', 2)"]);
        let mut log = Lines::default();
        background(&mut sys, &mut log);
        assert_eq!(
            log.lines,
            ["on_insert_expenses: before", "on_insert_expenses: qr is typed at a prompt: its code takes the screen of who asked for it.", "on_insert_expenses: Stopped at line 2 of on_insert_expenses."]
        );
        assert!(!sys.noting && sys.note.bytes().is_empty());
        // and whatever gives such a session a line to run, it has no block to keep lines in
        sys.session = BACKGROUND;
        let mut out = Lines::default();
        assert!(!shell::exec(&mut sys, b"if 1 = 1", &mut out));
        assert!(shell::exec(&mut sys, b"say 'one line'", &mut out) && shell::exec(&mut sys, b"", &mut out));
        assert_eq!(out.lines, ["A block is typed at a prompt, not by a program that runs by itself.", "one line"]);
        assert!(sys.blocks.iter().all(|block| !block.open()));
    }

    #[test]
    fn what_becomes_a_job_and_what_runs_at_once() {
        let made = || {
            let mut sys = Box::new(books());
            type_lines(&mut sys, &["program report", "say 'reported'", "end"]);
            crate::machine::tests::rooms(&mut sys, 3);
            sys
        };
        let waited = |sys: &System| crate::jobs::watched(sys, crate::jobs::Watch::Screen).is_some() || crate::jobs::watched(sys, crate::jobs::Watch::Serial).is_some();
        let exec = |sys: &mut System, line: &str| {
            let mut out = Lines::default();
            let ok = shell::exec(sys, line.as_bytes(), &mut out);
            (ok, out.lines)
        };
        // at the screen and at the serial port: the name of a program, `run`, and a block
        for session in [crate::events::SCREEN, crate::events::SERIAL] {
            for lines in [&["report"][..], &["run report"], &["if 1 = 1", "say 'in a block'", "end"]] {
                let mut sys = made();
                sys.session = session;
                let said: Vec<String> = lines.iter().flat_map(|line| exec(&mut sys, line).1).collect();
                assert!(said.is_empty() && waited(&sys), "{lines:?}");
                assert_eq!(sys.jobs.list()[0].by, session);
            }
        }
        // a single line that is no program runs at once
        let mut sys = made();
        assert_eq!(exec(&mut sys, "say 'now'"), (true, vec!["now".to_string()]));
        assert_eq!(exec(&mut sys, "select count(*) from expenses").1.len(), 2);
        assert!(!waited(&sys));
        // from the network prompt, from a program that runs by itself and for the companion: at once
        for session in [crate::events::NETWORK, BACKGROUND] {
            let mut sys = made();
            sys.session = session;
            assert_eq!(exec(&mut sys, "report"), (true, vec!["reported".to_string()]));
            assert!(!waited(&sys));
        }
        let mut sys = made();
        sys.acting = true;
        assert_eq!(exec(&mut sys, "report"), (true, vec!["reported".to_string()]));
        assert!(!waited(&sys));
        // while the prompt waits for one, a second runs at once: what is typed there meanwhile is not lost
        let mut sys = made();
        exec(&mut sys, "report");
        assert_eq!(exec(&mut sys, "report"), (true, vec!["reported".to_string()]));
        // a line typed by a command of a program that runs at once is not a job either
        let mut sys = made();
        sys.session = crate::events::NETWORK;
        type_lines(&mut sys, &["program outer", "qr report", "end"]);
        sys.session = crate::events::SCREEN;
        sys.acting = true;
        assert!(exec(&mut sys, "outer").0 && !waited(&sys));
        // with every place taken, the prompt says so
        let mut sys = made();
        type_lines(&mut sys, &["program forever", "while 1 = 1", "end", "end", "submit forever", "submit forever"]);
        assert_eq!(exec(&mut sys, "report"), (false, vec!["Eight jobs are running or held: end one, or wait.".to_string()]));
        // what cannot start says why, and nothing is waited for
        let mut sys = made();
        assert_eq!(exec(&mut sys, "run nowhere"), (false, vec!["Unknown program: nowhere. Type programs to list them.".to_string()]));
        assert!(!waited(&sys));
    }

    #[test]
    fn programs_that_start_each_other_through_jobs_are_cut_too() {
        AS_JOBS.set(true);
        let mut sys = books();
        crate::machine::tests::rooms(&mut sys, crate::jobs::PLACES + 1);
        type_lines(&mut sys, &["create table a (n int)", "program adder", "insert into a values (1)", "end"]);
        type_lines(&mut sys, &["on insert into a", "submit adder", "end", "insert into a values (0)"]);
        let mut said = Vec::new();
        for _ in 0..400 {
            said.extend(in_background(&mut sys));
            crate::jobs::work(&mut sys, &mut Lines::default(), &mut || false);
        }
        assert_eq!(said, ["on_insert_a: not run, programs kept starting each other."]);
        // as many rows as when the programs start each other without a job between them
        assert_eq!(type_lines(&mut sys, &["say select count(*) from a"]).1, ["5"]);
    }

    #[test]
    fn a_program_that_takes_longer_than_its_rhythm_starves_no_other() {
        let mut sys = books();
        crate::machine::tests::rooms(&mut sys, 2);
        type_lines(&mut sys, &["every 1 seconds", "let n = 0", "while n < 300", "let n = n + 1", "end", "end"]);
        type_lines(&mut sys, &["every 2 seconds", "say 'two'", "end", "on notify alerts", "say 'heard'", "end", "notify alerts"]);
        let mut log = Lines::default();
        // a tenth of a second for each instruction: the first program is always late
        for _ in 0..4000 {
            sys.ticks += 10;
            background(&mut sys, &mut log);
            crate::jobs::work(&mut sys, &mut log, &mut || true);
        }
        assert!(log.lines.contains(&"on_notify_alerts: heard".to_string()), "{:?}", log.lines);
        assert!(log.lines.iter().filter(|line| line.as_str() == "every_2_seconds: two").count() >= 2);
    }
}
