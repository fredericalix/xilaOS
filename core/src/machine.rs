//! The machine that runs compiled XilaScript.
//!
//! Lines typed at a prompt and the lines of programs are compiled
//! (`code.rs`) into *units*, and the machine runs the unit at the top:
//! one instruction, then the next. A program that is run is one more
//! unit above the one that runs it. Everything the machine knows of what
//! it is doing is in `Machine`, the field `vm` of `System`: which units are in
//! use, where each is, how many rounds its loops went. Nothing of a run
//! is on the stack of the task, so a run could be left between two
//! instructions and taken up again; for now `lines` and `program` run
//! to the end.
//!
//! The room is fixed: four units (lines typed, then programs three
//! deep) and four cursors for loops over rows. A command that types at
//! the prompt from inside a program (`qr`) enters the machine again: it
//! takes the next unit and gives it back.
//!
//! An instruction copies what it needs out of the machine (it is a few
//! numbers, its line 240 bytes) before it calls anything that takes the
//! whole `System`, a command above all.

use core::fmt::Write;

use crate::code::{self, Code, Instr, Op, What, LOOPS, WHILES};
use crate::expr::VAR_NAME;
use crate::grid::COLS;
use crate::lang::{self, Line, MAX_LINES};
use crate::shell::{self, Out, Style, System, Table, Text};
use crate::sql::{self, Cursor, Message, Value, MAX_COLUMNS};
use crate::text::{split_word, trim, FixedStr};

/// Units of code: lines typed, then programs three deep.
pub const UNITS: usize = 4;
/// Programs that run programs: how deep.
const MAX_LEVELS: usize = 3;
/// Times a `while` goes round before it is stopped.
const MAX_ROUNDS: u32 = 10_000;
/// Statements one line typed at the prompt may run in all.
const MAX_STEPS: u32 = 100_000;

/// A unit that is running: where it is.
#[derive(Clone, Copy)]
struct Call {
    /// The next instruction.
    next: usize,
    /// How many times each `while` went round since it was entered.
    rounds: [u32; WHILES],
    /// The name of the program, or nothing for lines typed.
    program: Text,
    /// How many lines were given, for lines typed: one alone has no place to tell.
    lines: usize,
    /// The first of the machine's cursors that is this unit's, and how
    /// many it has open: those of who called it are before.
    cursors: usize,
    open: usize,
}

impl Call {
    const NONE: Call = Call { next: 0, rounds: [0; WHILES], program: Text::new(), lines: 0, cursors: 0, open: 0 };
}

pub struct Machine {
    units: [Code; UNITS],
    calls: [Call; UNITS],
    /// Units in use.
    depth: usize,
    /// Programs running, one inside the other.
    programs: usize,
    /// Statements run since the first unit was taken.
    steps: u32,
    /// The loops over rows that are open, in all the units together.
    cursors: [Cursor; LOOPS],
}

impl Machine {
    pub const fn new() -> Machine {
        Machine { units: [const { Code::new() }; UNITS], calls: [Call::NONE; UNITS], depth: 0, programs: 0, steps: 0, cursors: [const { Cursor::new() }; LOOPS] }
    }
}

impl Default for Machine {
    fn default() -> Machine {
        Machine::new()
    }
}

/// A failure that was already told to the user.
fn said() -> Message {
    Message::new()
}

/// Lets through what a command says, but not that it is done.
struct Quiet<'a>(&'a mut dyn Out);

impl Out for Quiet<'_> {
    fn line(&mut self, style: Style, text: &[u8]) {
        if style != Style::Done {
            self.0.line(style, text);
        }
    }

    fn table(&mut self, table: &Table) {
        self.0.table(table);
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

/// Compiles lines and runs them to the end, for the session that gave
/// them and with its names. Says what is wrong and returns false when
/// their form is, or when one of them fails.
pub fn lines(sys: &mut System, lines: &[Line], out: &mut dyn Out) -> bool {
    let unit = sys.vm.depth;
    if unit == UNITS {
        out.line(Style::Error, b"Programs run programs three deep at most.");
        return false;
    }
    if let Err(fault) = code::compile(lines, &mut sys.vm.units[unit]) {
        out.line(Style::Error, fault.message.bytes());
        if lines.len() > 1 {
            let mut place = FixedStr::<COLS>::new();
            let _ = write!(place, "Line {} is not right: nothing was run.", fault.line);
            out.line(Style::Error, place.bytes());
        }
        return false;
    }
    enter(sys, Text::new(), lines.len());
    run(sys, unit, out)
}

/// Compiles lines without running them, to learn whether their form is
/// right: what a program goes through before it is kept. The next unit
/// is where it is done, and is left free.
pub fn check(sys: &mut System, lines: &[Line]) -> Result<(), code::Fault> {
    match sys.vm.units.get_mut(sys.vm.depth) {
        Some(unit) => code::compile(lines, unit),
        // no unit is free to try them in: they are told when they run
        None => Ok(()),
    }
}

/// Runs a program kept in the database to the end, as the system does
/// for one that runs by itself. Says what failed, and returns whether
/// all went well.
pub fn program(sys: &mut System, name: &[u8], out: &mut dyn Out) -> bool {
    let base = sys.vm.depth;
    match call(sys, name, out) {
        Ok(()) => run(sys, base, out),
        Err(message) => {
            if !message.is_empty() {
                out.line(Style::Error, message.bytes());
            }
            false
        }
    }
}

/// Starts a program: its lines are read from the database and compiled
/// into the next unit, which is then the one that runs. Programs are
/// three deep at most, whoever started the first.
///
/// Never part of `step`: the lines it reads are 12 KB on the stack, and
/// `step` is below every command a program runs.
#[inline(never)]
fn call(sys: &mut System, name: &[u8], out: &mut dyn Out) -> Result<(), Message> {
    if sys.vm.programs == MAX_LEVELS || sys.vm.depth == UNITS {
        return Err(Message::from_bytes(b"Programs run programs three deep at most."));
    }
    // read here and given to the compiler, which keeps them: not held while the program runs
    let mut lines = [Line::new(); MAX_LINES];
    let Some(count) = lang::load(&sys.db, name, &mut lines)? else {
        return Err(crate::sql::error(b"Unknown program: ", name).also(b". Type programs to list them."));
    };
    let unit = sys.vm.depth;
    if let Err(fault) = code::compile(&lines[..count], &mut sys.vm.units[unit]) {
        // kept by a system that did not compile what it kept: nothing of it runs
        out.line(Style::Error, fault.message.bytes());
        let mut place = FixedStr::<COLS>::new();
        let _ = write!(place, "Stopped at line {} of {}.", fault.line, Text::from_bytes(name));
        out.line(Style::Error, place.bytes());
        return Err(said());
    }
    enter(sys, Text::from_bytes(name), count);
    Ok(())
}

/// Takes the next unit, whose code is ready, for a program or for lines.
fn enter(sys: &mut System, program: Text, lines: usize) {
    let machine = &mut sys.vm;
    if machine.depth == 0 {
        machine.steps = 0;
    }
    // its cursors come after those of who called it
    let cursors = machine.depth.checked_sub(1).map_or(0, |caller| machine.calls[caller].cursors + machine.calls[caller].open);
    machine.calls[machine.depth] = Call { program, lines, cursors, ..Call::NONE };
    machine.depth += 1;
    if !program.is_empty() {
        machine.programs += 1;
    }
}

/// Gives the unit at the top back, and the cursors it left open.
fn leave(sys: &mut System) {
    let machine = &mut sys.vm;
    machine.depth -= 1;
    let call = machine.calls[machine.depth];
    if !call.program.is_empty() {
        machine.programs -= 1;
    }
    for cursor in machine.cursors.iter_mut().skip(call.cursors).take(call.open) {
        cursor.close();
    }
}

/// Runs the unit `base`, and those it calls, to the end. Returns whether
/// all went well; what did not was said.
fn run(sys: &mut System, base: usize, out: &mut dyn Out) -> bool {
    loop {
        let top = sys.vm.depth - 1;
        let at = sys.vm.calls[top].next;
        if at >= sys.vm.units[top].len() {
            // the end of the unit: who called it goes on
            leave(sys);
            if top == base {
                return true;
            }
            continue;
        }
        let instr = sys.vm.units[top].at(at);
        sys.vm.calls[top].next = at + 1;
        let Err(mut message) = step(sys, top, instr, out) else {
            continue;
        };
        // It failed: each unit that was running says where it stopped, down
        // to the one this run was for. A unit above may have been entered
        // by the instruction and have said its own.
        loop {
            let top = sys.vm.depth - 1;
            let call = sys.vm.calls[top];
            let line = sys.vm.units[top].at(call.next.saturating_sub(1)).line as usize + 1;
            if !message.is_empty() {
                out.line(Style::Error, message.bytes());
            }
            let mut place = FixedStr::<COLS>::new();
            if !call.program.is_empty() {
                let _ = write!(place, "Stopped at line {line} of {}.", call.program);
            } else if call.lines > 1 {
                let _ = write!(place, "Stopped at line {line}.");
            }
            if !place.is_empty() {
                out.line(Style::Error, place.bytes());
            }
            leave(sys);
            if top == base {
                return false;
            }
            message = said();
        }
    }
}

/// Works out the value an instruction asks for.
fn value(sys: &System, unit: usize, what: What, text: &[u8]) -> Result<Value, Message> {
    match what {
        What::Value(kept) => sys.vm.units[unit].pool().eval(kept, &sys.vars[sys.session]),
        What::Select => selected(sys, text),
    }
}

// The three that follow hold a result or a cursor, 17 to 20 KB, while
// they work. They are never part of `step`, which is below every command
// a program runs: a SELECT typed at the screen needs the stack of its
// task for itself.

/// What a SELECT gives as a value: its first column of its first row.
#[inline(never)]
fn selected(sys: &System, text: &[u8]) -> Result<Value, Message> {
    let rows = sql::query(sys, &sys.vars[sys.session], text)?;
    Ok(if rows.count > 0 && rows.cols > 0 { rows.rows[0][0] } else { Value::Null })
}

/// Opens the cursor `at` on a SELECT. A cursor reads the tables of the
/// system it is kept in: it is taken out of it for that time.
#[inline(never)]
fn open(sys: &mut System, at: usize, query: &[u8]) -> Result<(), Message> {
    let mut cursor = Cursor::new();
    core::mem::swap(&mut cursor, &mut sys.vm.cursors[at]);
    let opened = cursor.open(&*sys, &sys.vars[sys.session], query);
    core::mem::swap(&mut cursor, &mut sys.vm.cursors[at]);
    opened
}

/// The next row of the cursor `at` when it must be read from the tables.
#[inline(never)]
fn read(sys: &mut System, at: usize, query: &[u8]) -> Result<Option<[Value; MAX_COLUMNS]>, Message> {
    let mut cursor = Cursor::new();
    core::mem::swap(&mut cursor, &mut sys.vm.cursors[at]);
    let next = cursor.next(&*sys, query);
    core::mem::swap(&mut cursor, &mut sys.vm.cursors[at]);
    next
}

/// Runs one instruction of the unit `unit`, which is at the top.
fn step(sys: &mut System, unit: usize, instr: Instr, out: &mut dyn Out) -> Result<(), Message> {
    if instr.begins {
        sys.vm.steps += 1;
        if sys.vm.steps > MAX_STEPS {
            return Err(Message::from_bytes(b"This ran 100000 statements and was stopped."));
        }
    }
    // the texts an instruction needs are those of its line
    let line: Line = *sys.vm.units[unit].line(instr.line as usize);
    let text = trim(line.bytes());
    match instr.op {
        Op::Let(what) => {
            let (name, written) = code::let_parts(text).unwrap_or((b"", b""));
            let value = value(sys, unit, what, written)?;
            if !sys.vars[sys.session].set(name, value) {
                return Err(Message::from_bytes(b"There is no room for one more name: 32 are kept at most."));
            }
        }
        Op::Say(what) => {
            let shown = match what {
                Some(what) => value(sys, unit, what, code::rest(text))?,
                None => Value::Null,
            };
            out.line(Style::Out, shown.shown().bytes());
        }
        Op::Jump(to) => sys.vm.calls[unit].next = to as usize,
        Op::JumpUnless(condition, to) => {
            if !sys.vm.units[unit].pool().holds(condition, &sys.vars[sys.session])? {
                sys.vm.calls[unit].next = to as usize;
            }
        }
        Op::Enter(which) => sys.vm.calls[unit].rounds[which as usize] = 0,
        Op::Round(which) => {
            let rounds = &mut sys.vm.calls[unit].rounds[which as usize];
            *rounds += 1;
            if *rounds > MAX_ROUNDS {
                return Err(Message::from_bytes(b"This loop went round 10000 times and was stopped."));
            }
        }
        Op::Open(which) => {
            let at = sys.vm.calls[unit].cursors + which as usize;
            if at >= LOOPS {
                return Err(Message::from_bytes(b"Loops over rows go four deep at most."));
            }
            let (_, query) = code::for_parts(text).unwrap_or((b"", b""));
            open(sys, at, query)?;
            sys.vm.calls[unit].open = which as usize + 1;
        }
        Op::Next(which, done) => {
            let at = sys.vm.calls[unit].cursors + which as usize;
            let (name, query) = code::for_parts(text).unwrap_or((b"", b""));
            let mut row = sys.vm.cursors[at].ready();
            if row.is_none() && sys.vm.cursors[at].more() {
                row = read(sys, at, query)?;
            }
            let Some(row) = row else {
                sys.vm.calls[unit].next = done as usize;
                return Ok(());
            };
            // the columns of the row, as name.column
            let mut headers = [sql::Name::new(); MAX_COLUMNS];
            let columns = sys.vm.cursors[at].headers().len();
            headers[..columns].copy_from_slice(sys.vm.cursors[at].headers());
            for (header, value) in headers[..columns].iter().zip(row) {
                let mut column = FixedStr::<VAR_NAME>::from_bytes(name);
                if column.len() + 1 + header.len() > VAR_NAME {
                    return Err(crate::sql::error(b"This name is too long: ", name).also(b".").also(header.bytes()));
                }
                column.push(b'.');
                column.push_bytes(header.bytes());
                if !sys.vars[sys.session].set(column.bytes(), value) {
                    return Err(Message::from_bytes(b"There is no room for one more name: 32 are kept at most."));
                }
            }
        }
        Op::Close(which) => {
            let call = &mut sys.vm.calls[unit];
            call.open = which as usize;
            let at = call.cursors + which as usize;
            sys.vm.cursors[at].close();
        }
        Op::Call => call(sys, code::rest(text), out)?,
        Op::Command => {
            let (name, arguments) = split_word(text);
            let mut lower = Text::from_bytes(name);
            lower.lower();
            if !shell::knows(lower.bytes()) && arguments.is_empty() && lang::exists(&sys.db, name) {
                // the name of a program is a command
                call(sys, name, out)?;
            } else {
                // inside a program, what a command did is not announced
                let done = if sys.vm.programs > 0 { shell::command(sys, text, &mut Quiet(out)) } else { shell::command(sys, text, out) };
                if !done {
                    return Err(said());
                }
            }
        }
        Op::Return => sys.vm.calls[unit].next = usize::MAX,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::Lines;

    /// Gives lines to the machine as one block. Returns how it went and what it said.
    fn block(sys: &mut System, text: &[&str]) -> (bool, Vec<String>) {
        let given: Vec<Line> = text.iter().map(|line| Line::from_bytes(line.as_bytes())).collect();
        let mut out = Lines::default();
        let ok = lines(sys, &given, &mut out);
        (ok, out.lines)
    }

    /// Gives lines to the machine one by one, as a prompt would. Returns what the last one said.
    fn each(sys: &mut System, text: &[&str]) -> Vec<String> {
        text.iter().map(|line| block(sys, &[line]).1).last().unwrap()
    }

    /// A system with a table of three expenses.
    fn books() -> System {
        let mut sys = System::new();
        each(&mut sys, &["create table expenses (label text, amount decimal)", "insert into expenses values ('coffee', 3.5), ('train', 42), ('bread', 1.2)"]);
        sys
    }

    #[test]
    fn conditions_pick_the_lines_to_run() {
        let mut sys = System::new();
        let grade = |sys: &mut System, n: i64| {
            block(
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
                    "# a note, and a line with nothing",
                    "",
                ],
            )
            .1
        };
        assert_eq!(grade(&mut sys, 500), ["large"]);
        assert_eq!(grade(&mut sys, 12), ["medium, even"]);
        assert_eq!(grade(&mut sys, 13), ["medium, odd"]);
        assert_eq!(grade(&mut sys, 5), ["small"]);
        assert_eq!(grade(&mut sys, 0), ["none", "at all"]);
        // a condition that is nothing does not hold
        assert_eq!(block(&mut sys, &["if null > 1", "say 'yes'", "else", "say 'no'", "end"]).1, ["no"]);
        // return ends the lines
        assert_eq!(block(&mut sys, &["say 1", "if 1 = 1", "return", "end", "say 2"]), (true, vec!["1".to_string()]));
    }

    #[test]
    fn loops_run_lines_again() {
        let mut sys = System::new();
        assert_eq!(block(&mut sys, &["let n = 3", "let s = ''", "while n > 0", "let s = s || n", "let n = n - 1", "end", "say s"]).1, ["321"]);
        // a loop in a loop starts its count again each time
        assert_eq!(block(&mut sys, &["let a = 0", "let all = 0", "while a < 3", "let b = 0", "while b < 4", "let b = b + 1", "let all = all + 1", "end", "let a = a + 1", "end", "say all"]).1, ["12"]);
        // a loop that never ends is stopped
        let (ok, out) = block(&mut sys, &["while 1 = 1", "let n = n + 1", "end"]);
        assert!(!ok);
        assert_eq!(out, ["This loop went round 10000 times and was stopped.", "Stopped at line 1."]);
        assert_eq!(each(&mut sys, &["say n"]), ["10000"]);
        // each time it is entered, not in all
        let (ok, out) = block(&mut sys, &["let n = 0", "while n < 3", "let m = 0", "while m < 9000", "let m = m + 1", "end", "let n = n + 1", "end", "say n"]);
        assert_eq!((ok, out), (true, vec!["3".to_string()]));
        // and so are lines that run too many statements
        let (ok, out) = block(&mut sys, &["let n = 0", "while n < 20", "let m = 0", "while m < 9000", "let m = m + 1", "end", "let n = n + 1", "end"]);
        assert!(!ok);
        assert_eq!(out[0], "This ran 100000 statements and was stopped.");
    }

    #[test]
    fn a_fault_of_form_is_told_before_anything_runs() {
        let mut sys = System::new();
        let (ok, out) = block(&mut sys, &["if 1 = 1", "say 'ran'", "else", "else", "end"]);
        assert!(!ok);
        assert_eq!(out, ["After else comes nothing, or if and a condition; a plain else is last.", "Line 4 is not right: nothing was run."]);
        let (ok, out) = block(&mut sys, &["let made = 1", "for x in tables", "end"]);
        assert!(!ok);
        assert_eq!(out, ["A loop over rows is written: for row in select ...", "Line 2 is not right: nothing was run."]);
        assert_eq!(each(&mut sys, &["say made"]), ["Unknown name: made"]);
        // what goes wrong while it runs is told as before, after what ran
        assert_eq!(block(&mut sys, &["say 'before'", "say 1 / 0"]), (false, vec!["before".to_string(), "Division by zero.".to_string(), "Stopped at line 2.".to_string()]));
        // and the machine is whole after each
        assert_eq!(block(&mut sys, &["say 'still here'"]), (true, vec!["still here".to_string()]));
    }

    /// Types lines at the prompt, which keeps programs: all must go well.
    fn typed(sys: &mut System, text: &[&str]) {
        for line in text {
            assert!(shell::exec(sys, line.as_bytes(), &mut Lines::default()), "{line}");
        }
    }

    fn texts(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn programs_run_programs_and_say_where_they_fail() {
        let mut sys = System::new();
        typed(&mut sys, &["program inner", "say 'in'", "if depth > 1", "return", "end", "let x = 1 / zero", "say 'not reached'", "end"]);
        typed(&mut sys, &["program outer", "say 'out'", "inner", "say 'back'", "end"]);
        each(&mut sys, &["let depth = 2", "let zero = 0"]);
        // `return` leaves the program it is in, not the one that ran it
        assert_eq!(block(&mut sys, &["outer"]), (true, texts(&["out", "in", "back"])));
        each(&mut sys, &["let depth = 0"]);
        assert_eq!(block(&mut sys, &["outer"]), (false, texts(&["out", "in", "Division by zero.", "Stopped at line 5 of inner.", "Stopped at line 2 of outer."])));
        // run by `run`, from lines that are more than one
        assert_eq!(
            block(&mut sys, &["say 'first'", "run outer", "say 'not here'"]),
            (false, texts(&["first", "out", "in", "Division by zero.", "Stopped at line 5 of inner.", "Stopped at line 2 of outer.", "Stopped at line 2."]))
        );
        // a program that runs itself is stopped
        typed(&mut sys, &["program again", "let depth = depth + 1", "run again", "end"]);
        let (ok, out) = block(&mut sys, &["again"]);
        assert!(!ok);
        assert_eq!(out, ["Programs run programs three deep at most.", "Stopped at line 2 of again.", "Stopped at line 2 of again.", "Stopped at line 2 of again."]);
        assert_eq!(each(&mut sys, &["say depth"]), ["3"]);
        // and is three deep whoever started the first
        let mut out = Lines::default();
        each(&mut sys, &["let depth = 0"]);
        assert!(!program(&mut sys, b"again", &mut out));
        assert_eq!(out.lines.len(), 4);
        assert_eq!(each(&mut sys, &["say depth"]), ["3"]);
        // one that is not there
        assert_eq!(block(&mut sys, &["run nowhere"]), (false, texts(&["Unknown program: nowhere. Type programs to list them."])));
        let mut out = Lines::default();
        assert!(!program(&mut sys, b"nowhere", &mut out));
        assert_eq!(out.lines, ["Unknown program: nowhere. Type programs to list them."]);
        // inside a program, what a command did is not announced
        typed(&mut sys, &["create table doubled (label text, amount decimal)"]);
        typed(&mut sys, &["program quiet", "insert into doubled values ('x', 0)", "mood calm", "mood", "say 'done'", "delete from doubled where label = 'x'", "end"]);
        assert_eq!(block(&mut sys, &["quiet"]), (true, texts(&["Mood is calm.", "done"])));
        let mut out = Lines::default();
        assert!(program(&mut sys, b"quiet", &mut out));
        assert_eq!(out.lines, ["Mood is calm.", "done"]);
        // the machine is whole after all that
        assert_eq!(block(&mut sys, &["quiet"]), (true, texts(&["Mood is calm.", "done"])));
    }

    #[test]
    fn loops_over_rows_read_them_one_by_one() {
        let mut sys = books();
        assert_eq!(
            block(&mut sys, &["for row in select label, amount from expenses where amount > 2 order by amount desc", "say row.label + ': ' + row.amount", "end"]),
            (true, texts(&["train: 42.00", "coffee: 3.50"]))
        );
        // loops inside loops, and changes made from a loop
        each(&mut sys, &["create table doubled (label text, amount decimal)"]);
        let (ok, out) = block(
            &mut sys,
            &["for row in select label, amount from expenses", "    let times = 2", "    while times > 0", "        insert into doubled values (row.label, row.amount * times)", "        let times = times - 1", "    end", "end"],
        );
        assert!(ok, "{out:?}");
        assert_eq!(out.len(), 6);
        assert_eq!(out[5], "1 row added to doubled, with id 6.");
        assert_eq!(each(&mut sys, &["select count(*), sum(amount) from doubled"]), ["   count  sum", "   6      140.10"]);
        // the names of the SELECT are those of the moment the loop began
        assert_eq!(block(&mut sys, &["let least = 2", "for row in select label from expenses where amount > least", "let least = 100", "say row.label", "end"]).1, ["coffee", "train"]);
        // a name too long to be kept
        let (ok, out) = block(&mut sys, &["for quite_a_long_name_here in select label as a_long_header_too from expenses", "end"]);
        assert!(!ok);
        assert_eq!(out, ["This name is too long: quite_a_long_name_here.a_long_header_too", "Stopped at line 1."]);
        // a table that is not there
        assert_eq!(block(&mut sys, &["say 1", "for row in select n from nowhere", "end"]).1, ["1", "Unknown table: nowhere. Type tables to list them.", "Stopped at line 2."]);
    }

    #[test]
    fn a_loop_over_rows_has_no_limit_and_ends() {
        let mut sys = System::new();
        each(&mut sys, &["create table many (n int)"]);
        for _ in 0..25 {
            each(&mut sys, &["insert into many values (1), (1), (1), (1), (1), (1), (1), (1)"]);
        }
        // 200 rows, where 32 was the most
        assert_eq!(block(&mut sys, &["let sum = 0", "for row in select n from many", "let sum = sum + row.n", "end", "say sum"]), (true, texts(&["200"])));
        // the last row stays under its names after the loop, as it did
        assert_eq!(each(&mut sys, &["say row.n"]), ["1"]);
        // a loop that adds to the table it reads ends: it sees the rows that were there
        let (ok, out) = block(&mut sys, &["let seen = 0", "for row in select n from many", "insert into many values (2)", "let seen = seen + 1", "end", "say seen"]);
        assert!(ok);
        assert_eq!(out.last().unwrap(), "200");
        // a loop that removes the row it is at, and the one after it
        assert!(block(&mut sys, &["for row in select id from many", "delete from many where id = row.id or id = row.id + 1", "end"]).0);
        assert_eq!(each(&mut sys, &["say select count(*) from many"]), ["0"]);
        // what is worked out whole keeps its limit and says so
        for _ in 0..5 {
            each(&mut sys, &["insert into many values (1), (2), (3), (4), (5), (6), (7), (8)"]);
        }
        assert_eq!(
            block(&mut sys, &["for row in select n from many order by n", "say row.n", "end"]),
            (false, texts(&["A loop takes 32 rows at most; this query has 40.", "Stopped at line 1."]))
        );
        assert!(block(&mut sys, &["for row in select n from many limit 3", "let n = row.n", "end"]).0);
        // the table goes under the loop: said when the next rows are asked for, at the line of the loop
        let (ok, out) = block(&mut sys, &["let gone = 0", "for row in select n from many", "if gone = 0", "drop table many", "let gone = 1", "end", "end"]);
        assert!(!ok, "{out:?}");
        assert!(out[out.len() - 2].starts_with("Unknown table: many") && out[out.len() - 1] == "Stopped at line 2.", "{out:?}");
        // loops in loops, and return from inside them: the cursors are given back
        each(&mut sys, &["create table two (n int)", "insert into two values (1), (2)"]);
        for _ in 0..3 {
            let deep = ["for a in select n from two", "for b in select n from two", "for c in select n from two", "for d in select n from two", "if d.n = 2", "return", "end", "end", "end", "end", "end", "say 'not here'"];
            assert_eq!(block(&mut sys, &deep), (true, vec![]));
        }
        // the four cursors are shared: a program run from inside two loops has two left
        typed(&mut sys, &["program three", "for x in select n from two", "for y in select n from two", "for z in select n from two", "say 'deep'", "end", "end", "end", "end"]);
        let (ok, out) = block(&mut sys, &["for a in select n from two", "for b in select n from two", "three", "end", "end"]);
        assert!(!ok);
        assert_eq!(out, ["Loops over rows go four deep at most.", "Stopped at line 3 of three.", "Stopped at line 3."]);
        typed(&mut sys, &["program one", "for x in select n from two", "say x.n", "end", "end"]);
        assert_eq!(block(&mut sys, &["for a in select n from two", "for b in select n from two", "for c in select n from two", "one", "end", "end", "end"]).1.len(), 16);
    }

    #[test]
    fn the_machine_is_entered_again_by_a_command_it_runs() {
        let mut sys = System::new();
        typed(&mut sys, &["program inner", "say 'in'", "end", "program outer", "qr inner", "say 'after'", "end"]);
        let mut out = Lines::default();
        assert!(program(&mut sys, b"outer", &mut out), "{:?}", out.lines);
        // what inner said went to the code, not here
        assert_eq!(out.lines, ["after"]);
        // with every unit taken, one more is refused, and the machine is whole after
        typed(&mut sys, &["program a", "qr b", "say 'a went on'", "end", "program b", "qr c", "end", "program c", "qr d", "end", "program d", "say 'too deep'", "end"]);
        let mut out = Lines::default();
        program(&mut sys, b"a", &mut out);
        let mut out = Lines::default();
        assert!(program(&mut sys, b"inner", &mut out));
        assert_eq!(out.lines, ["in"]);
        assert_eq!(block(&mut sys, &["outer"]), (true, texts(&["after"])));
    }
}
