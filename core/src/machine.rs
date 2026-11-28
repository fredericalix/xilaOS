//! The machine that runs compiled XilaScript.
//!
//! Lines typed at a prompt and the lines of programs are compiled
//! (`code.rs`) into *units*, and the machine runs the unit at the top:
//! one instruction, then the next. A program that is run is one more
//! unit above the one that runs it. Everything the machine knows of what
//! it is doing is in `Machine`, the field `vm` of `System`: which units are in
//! use, where each is, how many rounds its loops went. Nothing of a run
//! is on the stack of the task, so a run is left between two
//! instructions and taken up again: `turn` runs some of them, and that
//! is how a job runs (`jobs.rs`). `lines` and `program` are a start and
//! turns to the end, for what still runs at once. One case is not left
//! in the middle: a command that types a line (`qr`) runs it to its end
//! from inside its own instruction, at once, in the system's own room.
//!
//! The room is fixed: four units (lines typed, then programs three
//! deep) and four cursors for loops over rows. A command that types at
//! the prompt from inside a program (`qr`) enters the machine again: it
//! takes the next unit and gives it back.
//!
//! A program has names of its own: its parameters, with the values the
//! call gave them, and what it makes. They are the names in use while
//! it runs (`System::names`), and those of who called it wait in the
//! room (`aside`) until it leaves, however it leaves. Lines typed have
//! none of their own: they use the names in use where they run.
//!
//! An instruction copies what it needs out of the machine (it is a few
//! numbers, its line 240 bytes) before it calls anything that takes the
//! whole `System`, a command above all.

use core::fmt::Write;

use crate::code::{self, Code, Instr, Op, What, LOOPS, WHILES};
use crate::expr::{Vars, VAR_NAME};
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
const MAX_STEPS: u64 = 100_000;

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
    /// Statements run since the first unit was taken: what the limit of
    /// a program nobody watches counts.
    steps: u64,
    /// Those, and the rounds of the loops: what is shown of a job, so
    /// that a loop that does nothing else is seen to run.
    ran: u64,
    /// Whether the limits of a program nobody watches are lifted: the
    /// rounds of a `while`, the statements in all. They hold unless a
    /// job that can be stopped says otherwise.
    unbound: bool,
    /// The loops over rows that are open, in all the units together.
    cursors: [Cursor; LOOPS],
    /// For each unit that is a program, the names of who called it, put
    /// aside while it runs with its own: they come back when it leaves.
    aside: [Vars; UNITS],
}

impl Machine {
    pub const fn new() -> Machine {
        Machine { units: [const { Code::new() }; UNITS], calls: [Call::NONE; UNITS], depth: 0, programs: 0, steps: 0, ran: 0, unbound: false, cursors: [const { Cursor::new() }; LOOPS], aside: [const { Vars::new() }; UNITS] }
    }
}

impl Machine {
    /// Lifts the limits of a program nobody watches, or sets them again.
    pub fn unbound(&mut self, lifted: bool) {
        self.unbound = lifted;
    }

    /// Statements run since what runs here was started.
    pub fn statements(&self) -> u64 {
        self.ran
    }

    /// Whether something runs here.
    pub fn busy(&self) -> bool {
        self.depth > 0
    }

    /// The names of who started the lowest program that runs here, which
    /// wait for it to end. None when no program runs here.
    pub(crate) fn outer(&self) -> Option<&Vars> {
        (0..self.depth).find(|unit| !self.calls[*unit].program.is_empty()).map(|unit| &self.aside[unit])
    }
}

/// The names a program starts with.
#[derive(Clone, Copy)]
pub(crate) enum Given<'a> {
    /// The values of a call as they are written between its parentheses:
    /// worked out with the names of who calls, one for each parameter.
    Written(&'a [u8]),
    /// The names in use as they stand: what the system gives a program
    /// that runs by itself.
    Ready,
}

/// Where a machine is: the one a system has of its own, in which what is
/// typed runs at once, or one of the rooms it was given for its jobs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Room {
    Own,
    Given(usize),
}

/// What a turn came to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Turn {
    /// There is more to run.
    More,
    /// It ended, well or not: what failed was said.
    Ended(bool),
}

/// The machine of a room. A room that was not given is the system's own.
fn vm(sys: &System, room: Room) -> &Machine {
    match (&sys.rooms, room) {
        (Some(rooms), Room::Given(at)) if at < rooms.len() => &rooms[at],
        _ => &sys.vm,
    }
}

fn vm_mut(sys: &mut System, room: Room) -> &mut Machine {
    match (&mut sys.rooms, room) {
        (Some(rooms), Room::Given(at)) if at < rooms.len() => &mut rooms[at],
        _ => &mut sys.vm,
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
    match start_lines(sys, Room::Own, lines, out) {
        Some(base) => turn(sys, Room::Own, base, out, &mut || false) == Turn::Ended(true),
        None => false,
    }
}

/// Compiles lines into a room and makes them what runs there. Returns
/// the unit they are, to give to `turn`; None when they cannot start,
/// with why said.
pub fn start_lines(sys: &mut System, room: Room, lines: &[Line], out: &mut dyn Out) -> Option<usize> {
    let unit = vm(sys, room).depth;
    if unit == UNITS {
        out.line(Style::Error, b"Programs run programs three deep at most.");
        return None;
    }
    if let Err(fault) = code::compile(lines, &mut vm_mut(sys, room).units[unit]) {
        out.line(Style::Error, fault.message.bytes());
        if lines.len() > 1 {
            let mut place = FixedStr::<COLS>::new();
            let _ = write!(place, "Line {} is not right: nothing was run.", fault.line);
            out.line(Style::Error, place.bytes());
        }
        return None;
    }
    enter(sys, room, Text::new(), lines.len(), None);
    Some(unit)
}

/// Compiles lines without running them, to learn whether their form is
/// right: what a program goes through before it is kept. The next unit
/// is where it is done, and is left free.
pub fn check(sys: &mut System, lines: &[Line]) -> Result<(), code::Fault> {
    let free = sys.vm.depth;
    match sys.vm.units.get_mut(free) {
        Some(unit) => code::compile(lines, unit),
        // no unit is free to try them in: they are told when they run
        None => Ok(()),
    }
}

/// Runs a program kept in the database to the end, as the system does
/// for one that runs by itself: the names in use are the ones it starts
/// with. Says what failed, and returns whether all went well.
pub fn program(sys: &mut System, name: &[u8], out: &mut dyn Out) -> bool {
    match start_program(sys, Room::Own, name, Given::Ready, out) {
        Some(base) => turn(sys, Room::Own, base, out, &mut || false) == Turn::Ended(true),
        None => false,
    }
}

/// Makes a program kept in the database what runs in a room, with the
/// names it is given. Returns the unit it is, to give to `turn`; None
/// when it cannot start, with why said.
pub(crate) fn start_program(sys: &mut System, room: Room, name: &[u8], given: Given<'_>, out: &mut dyn Out) -> Option<usize> {
    let base = vm(sys, room).depth;
    match call(sys, room, name, given, out) {
        Ok(()) => Some(base),
        Err(message) => {
            if !message.is_empty() {
                out.line(Style::Error, message.bytes());
            }
            None
        }
    }
}

/// Gives back every unit and every cursor of a room: what ran there is
/// gone, and nothing is said.
pub fn stop(sys: &mut System, room: Room) {
    while vm(sys, room).depth > 0 {
        leave(sys, room);
    }
}

/// Where a room is: the program at the top (nothing for lines typed) and
/// its line, from 1. None when nothing runs there.
pub fn at(sys: &System, room: Room) -> Option<(Text, usize)> {
    let machine = vm(sys, room);
    let call = machine.calls[machine.depth.checked_sub(1)?];
    let code = &machine.units[machine.depth - 1];
    let line = if code.is_empty() { 1 } else { code.at(call.next.min(code.len() - 1)).line as usize + 1 };
    Some((call.program, line))
}

/// Starts a program: its lines are read from the database and compiled
/// into the next unit, which is then the one that runs, with names of
/// its own made of what it is given. Programs are three deep at most,
/// whoever started the first.
///
/// Never part of `step`: the lines it reads are 12 KB on the stack, and
/// `step` is below every command a program runs.
#[inline(never)]
fn call(sys: &mut System, room: Room, name: &[u8], given: Given<'_>, out: &mut dyn Out) -> Result<(), Message> {
    if vm(sys, room).programs == MAX_LEVELS || vm(sys, room).depth == UNITS {
        return Err(Message::from_bytes(b"Programs run programs three deep at most."));
    }
    // read here and given to the compiler, which keeps them: not held while the program runs
    let mut lines = [Line::new(); MAX_LINES];
    let Some(loaded) = lang::load(&sys.db, name, &mut lines)? else {
        return Err(crate::sql::error(b"Unknown program: ", name).also(b". Type programs to list them."));
    };
    // its names, worked out with those of who calls, before anything of it is there
    let names = match given {
        Given::Written(written) => bind(sys, name, loaded.takes.bytes(), written)?,
        Given::Ready => sys.names().clone(),
    };
    let unit = vm(sys, room).depth;
    if let Err(fault) = code::compile(&lines[..loaded.count], &mut vm_mut(sys, room).units[unit]) {
        // kept by a system that did not compile what it kept: nothing of it runs
        out.line(Style::Error, fault.message.bytes());
        let mut place = FixedStr::<COLS>::new();
        let _ = write!(place, "Stopped at line {} of {}.", fault.line, Text::from_bytes(name));
        out.line(Style::Error, place.bytes());
        return Err(said());
    }
    enter(sys, room, Text::from_bytes(name), loaded.count, Some(names));
    Ok(())
}

/// What to say of a call that does not give a program as many values as
/// it takes.
fn takes(name: &[u8], params: &lang::Params) -> Message {
    let mut lower = Text::from_bytes(name);
    lower.lower();
    let mut message = Message::new();
    let _ = match params.names().len() {
        0 => write!(message, "{lower} takes no value."),
        1 => write!(message, "{lower} takes 1 value: "),
        count => write!(message, "{lower} takes {count} values: "),
    };
    for (at, param) in params.names().iter().enumerate() {
        message.push_bytes(if at > 0 { b", " } else { b"" });
        message.push_bytes(param.bytes());
    }
    if !params.names().is_empty() {
        message.push(b'.');
    }
    message
}

/// The names a program starts with: each of its parameters (`takes`, as
/// they are kept) with the value the call gives it (`written`), worked
/// out with the names of who calls, and the moment that caller knows as
/// `now`. A call that does not give as many values as the program takes
/// is refused before any of them is worked out.
#[inline(never)]
fn bind(sys: &System, name: &[u8], takes_these: &[u8], written: &[u8]) -> Result<Vars, Message> {
    // kept right, unless its row was changed by hand: then this says how
    let params = lang::Params::read(takes_these)?;
    if code::arguments(written, &mut |_| Ok(()))? != params.names().len() {
        return Err(takes(name, &params));
    }
    let caller = sys.names();
    let mut names = Vars::new();
    names.clock = caller.clock;
    let mut at = 0;
    code::arguments(written, &mut |value| {
        names.set(params.names()[at].bytes(), value.eval_with(&[], caller)?);
        at += 1;
        Ok(())
    })?;
    Ok(names)
}

/// Takes the next unit, whose code is ready, for a program or for lines.
/// A program comes with its names, which become the ones in use: those
/// of who called it wait in the room. Lines typed have none of their own.
fn enter(sys: &mut System, room: Room, program: Text, lines: usize, names: Option<Vars>) {
    let caller = names.map(|names| core::mem::replace(sys.names_mut(), names));
    let machine = vm_mut(sys, room);
    if machine.depth == 0 {
        (machine.steps, machine.ran) = (0, 0);
    }
    // its cursors come after those of who called it
    let cursors = machine.depth.checked_sub(1).map_or(0, |caller| machine.calls[caller].cursors + machine.calls[caller].open);
    machine.calls[machine.depth] = Call { program, lines, cursors, ..Call::NONE };
    if let Some(caller) = caller {
        machine.aside[machine.depth] = caller;
    }
    machine.depth += 1;
    if !program.is_empty() {
        machine.programs += 1;
    }
}

/// Gives the unit at the top back, and the cursors it left open. When it
/// is a program, the names of who called it are the ones in use again.
fn leave(sys: &mut System, room: Room) {
    let machine = vm_mut(sys, room);
    machine.depth -= 1;
    let call = machine.calls[machine.depth];
    let mut caller = None;
    if !call.program.is_empty() {
        machine.programs -= 1;
        caller = Some(core::mem::take(&mut machine.aside[machine.depth]));
    }
    for cursor in machine.cursors.iter_mut().skip(call.cursors).take(call.open) {
        cursor.close();
    }
    if let Some(caller) = caller {
        *sys.names_mut() = caller;
    }
}

/// Runs the unit `base` of a room, and those it calls, until it ends or
/// until `spent` says the time of this turn is over. `spent` is asked
/// after each instruction, never inside one: an instruction is whole.
/// What failed is said, with where each unit stopped.
pub fn turn(sys: &mut System, room: Room, base: usize, out: &mut dyn Out, spent: &mut dyn FnMut() -> bool) -> Turn {
    loop {
        let Some(top) = vm(sys, room).depth.checked_sub(1).filter(|top| *top >= base) else {
            // nothing of it is left to run
            return Turn::Ended(true);
        };
        let at = vm(sys, room).calls[top].next;
        if at >= vm(sys, room).units[top].len() {
            // the end of the unit: who called it goes on
            leave(sys, room);
            if top == base {
                return Turn::Ended(true);
            }
            continue;
        }
        let instr = vm(sys, room).units[top].at(at);
        vm_mut(sys, room).calls[top].next = at + 1;
        let Err(mut message) = step(sys, room, top, instr, out) else {
            // A job stops where an instruction asked the kernel something,
            // to go on once that is served. What runs at once goes on, as it
            // always did: its request is served when it has ended.
            if spent() || (room != Room::Own && sys.request.is_some()) {
                return Turn::More;
            }
            continue;
        };
        // It failed: each unit that was running says where it stopped, down
        // to the one this run was for. A unit above may have been entered
        // by the instruction and have said its own.
        loop {
            // (nothing is left when the instruction emptied its own room)
            let Some(top) = vm(sys, room).depth.checked_sub(1) else {
                if !message.is_empty() {
                    out.line(Style::Error, message.bytes());
                }
                return Turn::Ended(false);
            };
            let call = vm(sys, room).calls[top];
            let line = vm(sys, room).units[top].at(call.next.saturating_sub(1)).line as usize + 1;
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
            leave(sys, room);
            if top == base {
                return Turn::Ended(false);
            }
            message = said();
        }
    }
}

/// Works out the value an instruction asks for.
fn value(sys: &System, room: Room, unit: usize, what: What, text: &[u8]) -> Result<Value, Message> {
    match what {
        What::Value(kept) => vm(sys, room).units[unit].pool().eval(kept, sys.names()),
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
    let rows = sql::query(sys, sys.names(), text)?;
    Ok(if rows.count > 0 && rows.cols > 0 { rows.rows[0][0] } else { Value::Null })
}

/// Opens the cursor `at` on a SELECT. A cursor reads the tables of the
/// system it is kept in: it is taken out of it for that time.
#[inline(never)]
fn open(sys: &mut System, room: Room, at: usize, query: &[u8]) -> Result<(), Message> {
    let mut cursor = Cursor::new();
    core::mem::swap(&mut cursor, &mut vm_mut(sys, room).cursors[at]);
    let opened = cursor.open(&*sys, sys.names(), query);
    core::mem::swap(&mut cursor, &mut vm_mut(sys, room).cursors[at]);
    opened
}

/// The next row of the cursor `at` when it must be read from the tables.
#[inline(never)]
fn read(sys: &mut System, room: Room, at: usize, query: &[u8]) -> Result<Option<[Value; MAX_COLUMNS]>, Message> {
    let mut cursor = Cursor::new();
    core::mem::swap(&mut cursor, &mut vm_mut(sys, room).cursors[at]);
    let next = cursor.next(&*sys, query);
    core::mem::swap(&mut cursor, &mut vm_mut(sys, room).cursors[at]);
    next
}

/// Runs one instruction of the unit `unit`, which is at the top.
fn step(sys: &mut System, room: Room, unit: usize, instr: Instr, out: &mut dyn Out) -> Result<(), Message> {
    if instr.begins || matches!(instr.op, Op::Round(_)) {
        vm_mut(sys, room).ran += 1;
    }
    if instr.begins {
        let machine = vm_mut(sys, room);
        machine.steps += 1;
        if machine.steps > MAX_STEPS && !machine.unbound {
            return Err(Message::from_bytes(b"This ran 100000 statements and was stopped."));
        }
    }
    // the texts an instruction needs are those of its line
    let line: Line = *vm(sys, room).units[unit].line(instr.line as usize);
    let text = trim(line.bytes());
    match instr.op {
        Op::Let(what) => {
            let (name, written) = code::let_parts(text).unwrap_or((b"", b""));
            let value = value(sys, room, unit, what, written)?;
            if !sys.names_mut().set(name, value) {
                return Err(Message::from_bytes(b"There is no room for one more name: 32 are kept at most."));
            }
        }
        Op::Say(what) => {
            let shown = match what {
                Some(what) => value(sys, room, unit, what, code::rest(text))?,
                None => Value::Null,
            };
            out.line(Style::Out, shown.shown().bytes());
        }
        Op::Jump(to) => vm_mut(sys, room).calls[unit].next = to as usize,
        Op::JumpUnless(condition, to) => {
            if !vm(sys, room).units[unit].pool().holds(condition, sys.names())? {
                vm_mut(sys, room).calls[unit].next = to as usize;
            }
        }
        Op::Enter(which) => vm_mut(sys, room).calls[unit].rounds[which as usize] = 0,
        Op::Round(which) => {
            let machine = vm_mut(sys, room);
            let rounds = &mut machine.calls[unit].rounds[which as usize];
            *rounds += 1;
            if *rounds > MAX_ROUNDS && !machine.unbound {
                return Err(Message::from_bytes(b"This loop went round 10000 times and was stopped."));
            }
        }
        Op::Open(which) => {
            let at = vm(sys, room).calls[unit].cursors + which as usize;
            if at >= LOOPS {
                return Err(Message::from_bytes(b"Loops over rows go four deep at most."));
            }
            let (_, query) = code::for_parts(text).unwrap_or((b"", b""));
            open(sys, room, at, query)?;
            vm_mut(sys, room).calls[unit].open = which as usize + 1;
        }
        Op::Next(which, done) => {
            let at = vm(sys, room).calls[unit].cursors + which as usize;
            let (name, query) = code::for_parts(text).unwrap_or((b"", b""));
            let mut row = vm_mut(sys, room).cursors[at].ready();
            if row.is_none() && vm(sys, room).cursors[at].more() {
                row = read(sys, room, at, query)?;
            }
            let Some(row) = row else {
                vm_mut(sys, room).calls[unit].next = done as usize;
                return Ok(());
            };
            // the columns of the row, as name.column
            let mut headers = [sql::Name::new(); MAX_COLUMNS];
            let columns = vm(sys, room).cursors[at].headers().len();
            headers[..columns].copy_from_slice(vm(sys, room).cursors[at].headers());
            for (header, value) in headers[..columns].iter().zip(row) {
                let mut column = FixedStr::<VAR_NAME>::from_bytes(name);
                if column.len() + 1 + header.len() > VAR_NAME {
                    return Err(crate::sql::error(b"This name is too long: ", name).also(b".").also(header.bytes()));
                }
                column.push(b'.');
                column.push_bytes(header.bytes());
                if !sys.names_mut().set(column.bytes(), value) {
                    return Err(Message::from_bytes(b"There is no room for one more name: 32 are kept at most."));
                }
            }
        }
        Op::Close(which) => {
            let machine = vm_mut(sys, room);
            machine.calls[unit].open = which as usize;
            let at = machine.calls[unit].cursors + which as usize;
            machine.cursors[at].close();
        }
        Op::Call => {
            let (name, written) = code::call_parts(text);
            call(sys, room, name, Given::Written(written), out)?;
        }
        Op::Command => {
            let (name, arguments) = split_word(text);
            let mut lower = Text::from_bytes(name);
            lower.lower();
            if !shell::knows(lower.bytes()) && arguments.is_empty() && lang::exists(&sys.db, name) {
                // the name of a program is a command
                call(sys, room, name, Given::Written(b""), out)?;
            } else {
                // In a program, what a command did is not announced. Lines
                // typed say it, those included that a command of a program
                // types (`qr`), which are a unit of their own.
                let quiet = !vm(sys, room).calls[unit].program.is_empty();
                let done = if quiet { shell::command(sys, text, &mut Quiet(out)) } else { shell::command(sys, text, out) };
                if !done {
                    return Err(said());
                }
            }
        }
        Op::Return => vm_mut(sys, room).calls[unit].next = usize::MAX,
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
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
    fn a_program_is_given_its_values() {
        let mut sys = System::new();
        typed(&mut sys, &["program add(a, b)", "say a + b", "end"]);
        assert_eq!(each(&mut sys, &["add(2, 3)"]), ["5"]);
        assert_eq!(each(&mut sys, &["run add(2, 3)"]), ["5"]);
        assert_eq!(each(&mut sys, &["ADD( 2 , 3 )"]), ["5"]);
        // worked out with the names of who calls, when the call is made
        assert_eq!(each(&mut sys, &["let n = 4", "add(n, n + 1)"]), ["9"]);
        assert_eq!(block(&mut sys, &["let k = 1", "while k < 3", "add(k, 10)", "let k = k + 1", "end"]).1, ["11", "12"]);
        // a text may hold a comma or a parenthesis, and a function has its own
        typed(&mut sys, &["program tell(what, n)", "say what + n", "end"]);
        assert_eq!(each(&mut sys, &["tell('a, (b', round(2.4))"]), ["a, (b2"]);
        typed(&mut sys, &["program nothing(x)", "say x is null", "end"]);
        assert_eq!(each(&mut sys, &["nothing(null)"]), ["1"]);
        assert_eq!(each(&mut sys, &["nothing(3)"]), ["0"]);
    }

    #[test]
    fn a_program_sees_its_own_names_only() {
        let mut sys = books();
        each(&mut sys, &["let floor = 10"]);
        typed(&mut sys, &["program cheap", "say floor", "end"]);
        assert_eq!(each(&mut sys, &["cheap"]), ["Unknown name: floor", "Stopped at line 1 of cheap."]);
        // what it makes is its own, and is gone when it ends
        typed(&mut sys, &["program makes(floor)", "let floor = floor + 1", "let made = 3", "for row in select label from expenses", "end", "say floor + made", "end"]);
        assert_eq!(each(&mut sys, &["makes(1)"]), ["5"]);
        assert_eq!(each(&mut sys, &["say floor"]), ["10"]);
        assert_eq!(each(&mut sys, &["say made"]), ["Unknown name: made"]);
        assert_eq!(each(&mut sys, &["say row.label"]), ["Unknown name: row.label"]);
        // a parameter is a name in SQL as any other...
        typed(&mut sys, &["program over(least)", "say select count(*) from expenses where amount > least", "end"]);
        assert_eq!(each(&mut sys, &["over(3)"]), ["2"]);
        // ...so a column of the table hides it
        typed(&mut sys, &["program named(label)", "say select count(*) from expenses where label = label", "end"]);
        assert_eq!(each(&mut sys, &["named('tea')"]), ["3"]);
        // the names a program lists are its own
        typed(&mut sys, &["program lists(one)", "vars", "end"]);
        let listed = each(&mut sys, &["lists(7)"]);
        assert_eq!(listed.iter().map(|line| line.trim_end()).collect::<Vec<_>>(), ["   name  value", "   one   7"]);
    }

    #[test]
    fn the_names_of_who_calls_come_back_however_it_ends() {
        let mut sys = System::new();
        typed(&mut sys, &["program inner(x)", "say x", "if x > 5", "return", "end", "if x > 2", "let y = 1 / (x - x)", "end", "end"]);
        typed(&mut sys, &["program outer(x)", "inner(x + 1)", "say x", "end"]);
        assert_eq!(each(&mut sys, &["let x = 100", "outer(1)"]), ["2", "1"]);
        assert_eq!(each(&mut sys, &["say x"]), ["100"]);
        // left by return
        assert_eq!(each(&mut sys, &["outer(5)"]), ["6", "5"]);
        // left by a failure: every level says where it stopped, and the prompt has its names
        assert_eq!(each(&mut sys, &["outer(2)"]), ["3", "Division by zero.", "Stopped at line 6 of inner.", "Stopped at line 1 of outer."]);
        assert_eq!(each(&mut sys, &["say x"]), ["100"]);
        // a block typed uses the names of its prompt, and a call from it changes none
        assert_eq!(block(&mut sys, &["let x = x + 1", "inner(0)", "say x"]).1, ["0", "101"]);
        assert_eq!(each(&mut sys, &["say x"]), ["101"]);
    }

    #[test]
    fn a_call_with_the_wrong_number_of_values_is_refused_before_it_runs() {
        let mut sys = System::new();
        typed(&mut sys, &["program two(item, count)", "say 'ran'", "end", "program one(item)", "say 'ran'", "end", "program plain", "say 'ran'", "end"]);
        let two = ["two takes 2 values: item, count."];
        assert_eq!(each(&mut sys, &["two(1)"]), two);
        assert_eq!(each(&mut sys, &["two(1, 2, 3)"]), two);
        assert_eq!(each(&mut sys, &["two"]), two);
        assert_eq!(each(&mut sys, &["run two"]), two);
        assert_eq!(each(&mut sys, &["TWO(1)"]), two);
        // said before a value that cannot be worked out
        assert_eq!(each(&mut sys, &["two(missing)"]), two);
        assert_eq!(each(&mut sys, &["one()"]), ["one takes 1 value: item."]);
        assert_eq!(each(&mut sys, &["plain(1)"]), ["plain takes no value."]);
        assert_eq!(each(&mut sys, &["plain()"]), ["ran"]);
        assert_eq!(each(&mut sys, &["plain"]), ["ran"]);
        assert_eq!(each(&mut sys, &["run plain"]), ["ran"]);
        // a value that cannot be worked out: nothing of the program runs
        assert_eq!(each(&mut sys, &["one(missing)"]), ["Unknown name: missing"]);
        // in a program, the line of the call is told
        typed(&mut sys, &["program calls", "say 'before'", "two(1)", "end"]);
        assert_eq!(each(&mut sys, &["calls"]), ["before", "two takes 2 values: item, count.", "Stopped at line 2 of calls."]);
        assert_eq!(each(&mut sys, &["nowhere(1)"]), ["Unknown program: nowhere. Type programs to list them."]);
    }

    #[test]
    fn what_is_wrong_with_a_call_is_told_before_anything_runs() {
        let mut sys = System::new();
        let not_run = "Line 2 is not right: nothing was run.";
        assert_eq!(block(&mut sys, &["say 1", "f(1, )"]), (false, vec!["A value is missing between the parentheses.".to_string(), not_run.to_string()]));
        assert_eq!(block(&mut sys, &["say 1", "f(, 1)"]).1, ["A value is missing between the parentheses.", not_run]);
        assert_eq!(block(&mut sys, &["say 1", "run f(1) 2"]).1, ["A program is run like this: run report, or run restock('cable', 5)", not_run]);
        assert_eq!(block(&mut sys, &["say 1", "run"]).1, ["A program is run like this: run report, or run restock('cable', 5)", not_run]);
        assert_eq!(block(&mut sys, &["say 1", "f(select 1)"]).1, ["A select cannot be part of a calculation: keep it with let first.", not_run]);
        assert_eq!(block(&mut sys, &["say 1", "run f(1 +)"]).1[1], not_run);
        assert_eq!(block(&mut sys, &["say 1", "f(1 2)"]).1[1], not_run);
        assert_eq!(block(&mut sys, &["say 1", "f(1, 2, 3, 4, 5, 6, 7, 8, 9)"]).1, ["A program takes eight values at most.", not_run]);
        // a program is not kept with such a line
        let mut out = Lines::default();
        for line in ["program wrong", "say 1", "f(1, )", "end"] {
            shell::exec(&mut sys, line.as_bytes(), &mut out);
        }
        assert_eq!(out.lines, ["A value is missing between the parentheses.", "Line 2 is not right: the program is not kept."]);
        // what the prompt has a command or a word for is not a call
        assert_eq!(each(&mut sys, &["say (1 + 2) * 3"]), ["9"]);
        assert_eq!(each(&mut sys, &["select (1 + 2) * 3 as n from tables limit 1"])[1].trim(), "9");
    }

    #[test]
    fn a_program_calls_itself_with_values_of_its_own() {
        let mut sys = System::new();
        typed(&mut sys, &["program down(n)", "say n", "if n > 1", "down(n - 1)", "end", "say n", "end"]);
        assert_eq!(each(&mut sys, &["down(3)"]), ["3", "2", "1", "1", "2", "3"]);
        let stopped = "Stopped at line 3 of down.";
        assert_eq!(each(&mut sys, &["down(4)"]), ["4", "3", "2", "Programs run programs three deep at most.", stopped, stopped, stopped]);
        // every level gave its names back
        assert_eq!(each(&mut sys, &["let n = 9", "down(1)", "say n"]), ["9"]);
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
        typed(&mut sys, &["program inner(depth, zero)", "say 'in'", "if depth > 1", "return", "end", "let x = 1 / zero", "say 'not reached'", "end"]);
        typed(&mut sys, &["program outer(depth)", "say 'out'", "inner(depth, 0)", "say 'back'", "end"]);
        // `return` leaves the program it is in, not the one that ran it
        assert_eq!(block(&mut sys, &["outer(2)"]), (true, texts(&["out", "in", "back"])));
        assert_eq!(block(&mut sys, &["outer(0)"]), (false, texts(&["out", "in", "Division by zero.", "Stopped at line 5 of inner.", "Stopped at line 2 of outer."])));
        // run by `run`, from lines that are more than one
        assert_eq!(
            block(&mut sys, &["say 'first'", "run outer(0)", "say 'not here'"]),
            (false, texts(&["first", "out", "in", "Division by zero.", "Stopped at line 5 of inner.", "Stopped at line 2 of outer.", "Stopped at line 2."]))
        );
        // a program that runs itself is stopped
        typed(&mut sys, &["program again(depth)", "say depth", "run again(depth + 1)", "end"]);
        let (ok, out) = block(&mut sys, &["again(1)"]);
        assert!(!ok);
        assert_eq!(out, ["1", "2", "3", "Programs run programs three deep at most.", "Stopped at line 2 of again.", "Stopped at line 2 of again.", "Stopped at line 2 of again."]);
        // and is three deep whoever started the first: started as the system
        // starts one, the names in use are the ones it begins with
        let mut out = Lines::default();
        each(&mut sys, &["let depth = 0"]);
        assert!(!program(&mut sys, b"again", &mut out));
        assert_eq!(out.lines[..4], ["0", "1", "2", "Programs run programs three deep at most."]);
        assert_eq!(out.lines.len(), 7);
        assert_eq!(each(&mut sys, &["say depth"]), ["0"]);
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

    #[test]
    fn a_line_typed_by_a_command_of_a_program_says_what_it_does() {
        let mut sys = books();
        // qr keeps what the line it types says: all of it, as when it is typed at the prompt
        typed(&mut sys, &["program noted", "qr insert into expenses values ('x', 1)", "end"]);
        assert_eq!(block(&mut sys, &["noted"]), (true, vec![]));
        assert_eq!(String::from_utf8_lossy(sys.note.bytes()), "1 row added to expenses, with id 4.\n");
        // a program run that way is quiet as any program
        typed(&mut sys, &["program quiet", "insert into expenses values ('y', 2)", "say 'done'", "end", "program outer", "qr quiet", "end"]);
        assert!(block(&mut sys, &["outer"]).0);
        assert_eq!(String::from_utf8_lossy(sys.note.bytes()), "done\n");
    }

    /// Gives a system rooms for jobs, as the kernel does.
    pub(crate) fn rooms(sys: &mut System, count: usize) {
        let made: Vec<Machine> = (0..count).map(|_| Machine::new()).collect();
        sys.rooms = Some(Box::leak(made.into_boxed_slice()));
    }

    /// A system with a table of 100 rows, programs three deep whose last fails, and a name.
    fn stocked() -> Box<System> {
        let mut sys = Box::new(System::new());
        typed(&mut sys, &["create table hundred (n int)"]);
        for start in (0..100).step_by(8) {
            let values: Vec<String> = (start..(start + 8).min(100)).map(|n| format!("({n})")).collect();
            typed(&mut sys, &[&format!("insert into hundred values {}", values.join(", "))]);
        }
        typed(&mut sys, &["program top", "say 'top'", "middle", "say 'not here'", "end"]);
        typed(&mut sys, &["program middle", "say 'middle'", "run bottom", "end"]);
        typed(&mut sys, &["program bottom", "say 'bottom'", "let x = 1 / zero", "end", "let zero = 0"]);
        sys
    }

    /// Runs lines in a room it was given, one instruction a turn. Returns how it went, what it said, and how many turns it took.
    fn in_turns(sys: &mut System, room: Room, text: &[&str]) -> (bool, Vec<String>, usize) {
        let given: Vec<Line> = text.iter().map(|line| Line::from_bytes(line.as_bytes())).collect();
        let mut out = Lines::default();
        let Some(base) = start_lines(sys, room, &given, &mut out) else {
            return (false, out.lines, 0);
        };
        let mut turns = 0;
        loop {
            turns += 1;
            match turn(sys, room, base, &mut out, &mut || true) {
                Turn::More => assert!(turns < 1_000_000, "it never ends"),
                Turn::Ended(ok) => return (ok, out.lines, turns),
            }
        }
    }

    #[test]
    fn a_program_run_in_turns_says_what_it_says_at_once() {
        let blocks: [&[&str]; 6] = [
            &["let n = 0", "while n < 50", "let n = n + 1", "end", "say n"],
            &["let sum = 0", "for row in select n from hundred", "let sum = sum + row.n", "end", "say sum"],
            &["say 'first'", "top", "say 'not here'"],
            &["say 'ran'", "else"],
            &["for a in select n from hundred where n < 3", "for b in select n from hundred where n < 3", "if b.n = 1", "return", "end", "say a.n + b.n", "end", "end"],
            &["run nowhere"],
        ];
        for block_lines in blocks {
            let (mut at_once, mut by_turns) = (stocked(), stocked());
            rooms(&mut by_turns, 2);
            let once = block(&mut at_once, block_lines);
            let (ok, out, turns) = in_turns(&mut by_turns, Room::Given(1), block_lines);
            assert_eq!((ok, &out), (once.0, &once.1), "{block_lines:?}");
            assert_eq!(each(&mut at_once, &["vars"]), each(&mut by_turns, &["vars"]), "{block_lines:?}");
            // one instruction a turn: the first block alone is more than a hundred of them
            assert!(turns >= 1 || !ok);
            // and the room is free again, as is the machine's own
            assert!(at(&by_turns, Room::Given(1)).is_none() && at(&by_turns, Room::Own).is_none());
        }
        let mut sys = stocked();
        rooms(&mut sys, 1);
        assert!(in_turns(&mut sys, Room::Given(0), blocks[0]).2 > 100);
    }

    #[test]
    fn two_rooms_take_turns_without_touching_each_other() {
        let mut sys = stocked();
        rooms(&mut sys, 2);
        // the two run in two sessions, each with its names
        let adds = [Line::from_bytes(b"for row in select id from hundred"), Line::from_bytes(b"update hundred set n = n + 1 where id = row.id"), Line::from_bytes(b"end")];
        let doubles = [Line::from_bytes(b"for row in select id from hundred"), Line::from_bytes(b"update hundred set n = n * 2 where id = row.id"), Line::from_bytes(b"end")];
        let mut out = Lines::default();
        sys.session = crate::events::SCREEN;
        let first = start_lines(&mut sys, Room::Given(0), &adds, &mut out).unwrap();
        sys.session = crate::events::SERIAL;
        let second = start_lines(&mut sys, Room::Given(1), &doubles, &mut out).unwrap();
        let (mut one, mut two) = (Turn::More, Turn::More);
        while one == Turn::More || two == Turn::More {
            if one == Turn::More {
                sys.session = crate::events::SCREEN;
                one = turn(&mut sys, Room::Given(0), first, &mut out, &mut || true);
            }
            if two == Turn::More {
                sys.session = crate::events::SERIAL;
                two = turn(&mut sys, Room::Given(1), second, &mut out, &mut || true);
            }
        }
        assert_eq!((one, two), (Turn::Ended(true), Turn::Ended(true)), "{:?}", out.lines);
        // every row was changed by both, in one order or the other: no change is lost
        sys.session = crate::events::SCREEN;
        assert_eq!(each(&mut sys, &["say select count(*) from hundred"]), ["100"]);
        for id in [1, 37, 100] {
            let n = id - 1;
            let now: i64 = each(&mut sys, &[&format!("say select n from hundred where id = {id}")])[0].parse().unwrap();
            assert!(now == (n + 1) * 2 || now == n * 2 + 1, "{id}: {now}");
        }
    }

    #[test]
    fn a_room_that_is_stopped_is_free_again() {
        let mut sys = stocked();
        rooms(&mut sys, 1);
        typed(&mut sys, &["program deep", "for a in select n from hundred", "for b in select n from hundred", "let seen = b.n", "end", "end", "end"]);
        typed(&mut sys, &["program outer", "say 'out'", "deep", "end"]);
        let mut out = Lines::default();
        let base = start_program(&mut sys, Room::Given(0), b"outer", Given::Written(b""), &mut out).unwrap();
        for _ in 0..40 {
            assert_eq!(turn(&mut sys, Room::Given(0), base, &mut out, &mut || true), Turn::More);
        }
        // where it is: the program at the top, and its line
        let (program, line) = at(&sys, Room::Given(0)).unwrap();
        assert_eq!(program.bytes(), b"deep");
        assert!((1..=5).contains(&line));
        stop(&mut sys, Room::Given(0));
        assert!(at(&sys, Room::Given(0)).is_none());
        assert_eq!(out.lines, ["out"]);
        // the room runs something else at once, with all its cursors
        let four = ["for a in select 1", "for b in select 1", "for c in select 1", "for d in select 1", "say 'four deep'", "end", "end", "end", "end"];
        assert_eq!(in_turns(&mut sys, Room::Given(0), &four).1, ["four deep"]);
        // a program that is not there starts nothing, and says so
        let mut out = Lines::default();
        assert!(start_program(&mut sys, Room::Given(0), b"nowhere", Given::Written(b""), &mut out).is_none());
        assert_eq!(out.lines, ["Unknown program: nowhere. Type programs to list them."]);
    }

    #[test]
    fn without_limits_a_loop_goes_on() {
        let mut sys = Box::new(System::new());
        rooms(&mut sys, 1);
        let long = ["let n = 0", "while n < 20001", "let n = n + 1", "end", "say n"];
        // with them, as in the machine's own room
        let (ok, out, _) = in_turns(&mut sys, Room::Given(0), &long);
        assert!(!ok && out[0] == "This loop went round 10000 times and was stopped.");
        sys.rooms.as_mut().unwrap()[0].unbound(true);
        assert_eq!(in_turns(&mut sys, Room::Given(0), &long).1, ["20001"]);
        let longer = ["let n = 0", "while n < 9", "let m = 0", "while m < 9000", "let m = m + 1", "end", "let n = n + 1", "end", "say n"];
        assert_eq!(in_turns(&mut sys, Room::Given(0), &longer).1, ["9"]);
        // a turn that is given no end runs to the end
        let given: Vec<Line> = long.iter().map(|line| Line::from_bytes(line.as_bytes())).collect();
        let mut out = Lines::default();
        let base = start_lines(&mut sys, Room::Given(0), &given, &mut out).unwrap();
        assert_eq!(turn(&mut sys, Room::Given(0), base, &mut out, &mut || false), Turn::Ended(true));
    }

    #[test]
    fn the_rounds_of_a_loop_are_seen_but_do_not_count_against_the_limit() {
        let mut sys = Box::new(System::new());
        // ten times nine thousand rounds of one statement: under 100 000 statements, as it always was
        let nested = ["let n = 0", "while n < 10", "let m = 0", "while m < 9000", "let m = m + 1", "end", "let n = n + 1", "end", "say n"];
        assert_eq!(block(&mut sys, &nested), (true, texts(&["10"])));
        // and a loop that does nothing else is still seen to run
        rooms(&mut sys, 1);
        let given: Vec<Line> = ["while 1 = 1", "end"].iter().map(|line| Line::from_bytes(line.as_bytes())).collect();
        sys.rooms.as_mut().unwrap()[0].unbound(true);
        let base = start_lines(&mut sys, Room::Given(0), &given, &mut Lines::default()).unwrap();
        for _ in 0..3000 {
            turn(&mut sys, Room::Given(0), base, &mut Lines::default(), &mut || true);
        }
        assert!(sys.rooms.as_ref().unwrap()[0].statements() > 500);
    }
}
