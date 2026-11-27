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
use crate::grid::COLS;
use crate::lang::Line;
use crate::shell::{self, Out, Style, System, Table};
use crate::sql::{self, Cursor, Message, Name, Value};
use crate::text::{trim, FixedStr};

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
    program: Name,
    /// How many lines were given, for lines typed: one alone has no place to tell.
    lines: usize,
}

impl Call {
    const NONE: Call = Call { next: 0, rounds: [0; WHILES], program: Name::new(), lines: 0 };
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
    #[allow(dead_code)]
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
    enter(sys, Name::new(), lines.len());
    run(sys, unit, out)
}

/// Takes the next unit, whose code is ready, for a program or for lines.
fn enter(sys: &mut System, program: Name, lines: usize) {
    let machine = &mut sys.vm;
    if machine.depth == 0 {
        machine.steps = 0;
    }
    machine.calls[machine.depth] = Call { program, lines, ..Call::NONE };
    machine.depth += 1;
    if !program.is_empty() {
        machine.programs += 1;
    }
}

/// Gives the unit at the top back.
fn leave(sys: &mut System) {
    let machine = &mut sys.vm;
    machine.depth -= 1;
    if !machine.calls[machine.depth].program.is_empty() {
        machine.programs -= 1;
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
                let _ = write!(place, "Stopped at line {line} of {}.", shell::Text::from_bytes(call.program.bytes()));
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
    let vars = &sys.vars[sys.session];
    match what {
        What::Value(kept) => sys.vm.units[unit].pool().eval(kept, vars),
        // a SELECT gives its first column of its first row
        What::Select => {
            let rows = sql::query(sys, vars, text)?;
            Ok(if rows.count > 0 && rows.cols > 0 { rows.rows[0][0] } else { Value::Null })
        }
    }
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
        Op::Open(_) | Op::Next(..) | Op::Close(_) | Op::Call => return Err(Message::from_bytes(b"Not built yet.")),
        Op::Command => {
            // inside a program, what a command did is not announced
            let done = if sys.vm.programs > 0 { shell::command(sys, text, &mut Quiet(out)) } else { shell::command(sys, text, out) };
            if !done {
                return Err(said());
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
    fn names_keep_values() {
        let mut sys = books();
        assert_eq!(block(&mut sys, &["let total = 3 * 1.5"]), (true, vec![]));
        assert_eq!(each(&mut sys, &["say total"]), ["4.5"]);
        assert_eq!(each(&mut sys, &["let Total = total + 1", "say 'Total: ' + TOTAL"]), ["Total: 5.5"]);
        assert_eq!(each(&mut sys, &["let who = 'the ' || upper('cat')", "say who"]), ["the CAT"]);
        assert_eq!(each(&mut sys, &["say"]), [""]);
        // a select gives its first value; without a row, nothing
        assert_eq!(each(&mut sys, &["let sum = select sum(amount), count(*) from expenses", "say sum"]), ["46.70"]);
        assert_eq!(each(&mut sys, &["let none = select label from expenses where amount > 100", "say none is null"]), ["1"]);
        assert_eq!(each(&mut sys, &["say select count(*) from expenses"]), ["3"]);
        assert_eq!(each(&mut sys, &["say 'Total: ' + select sum(amount) from expenses"]), ["A select cannot be part of a calculation: keep it with let first."]);
        // names are values in the statements of SQL, and a command is a statement
        each(&mut sys, &["let limit = 3", "let what = 'tea'"]);
        assert_eq!(each(&mut sys, &["insert into expenses values (what, limit * 2)"]), ["1 row added to expenses, with id 4."]);
        assert_eq!(each(&mut sys, &["select label, amount from expenses where label = what"]), ["   label  amount", "   tea    6.00"]);
        // each session has its own names
        sys.session = crate::events::SERIAL;
        assert_eq!(block(&mut sys, &["say limit"]), (false, vec!["Unknown name: limit".to_string()]));
    }

    #[test]
    fn what_is_wrong_with_a_name_is_said() {
        let mut sys = System::new();
        assert_eq!(each(&mut sys, &["let x"]), ["A name gets a value like this: let total = 3"]);
        assert_eq!(each(&mut sys, &["let 2x = 1"]), ["A name is a letter, then letters, digits or _, and not a command."]);
        assert_eq!(each(&mut sys, &["let select = 1"]), ["A name is a letter, then letters, digits or _, and not a command."]);
        assert_eq!(each(&mut sys, &["let x = y + 1"]), ["Unknown name: y"]);
        assert_eq!(each(&mut sys, &["let x = 1 2"]), ["Expected the end of the query, found: 2"]);
        for n in 0..32 {
            assert!(block(&mut sys, &[&format!("let v{n} = {n}")]).0);
        }
        assert_eq!(each(&mut sys, &["let one_more = 1"]), ["There is no room for one more name: 32 are kept at most."]);
        assert_eq!(each(&mut sys, &["let v3 = v3 + v31", "say v3"]), ["34"]);
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
    fn a_line_that_fails_stops_the_block_and_says_where() {
        let mut sys = books();
        assert_eq!(block(&mut sys, &["if 1 = 1", "say 'before'", "let x = 1 / 0", "say 'after'", "end"]), (false, vec!["before".to_string(), "Division by zero.".to_string(), "Stopped at line 3.".to_string()]));
        assert_eq!(block(&mut sys, &["if 1 = 1", "frobnicate", "end"]).1, ["Unknown command: frobnicate", "Stopped at line 2."]);
        assert_eq!(block(&mut sys, &["if 1 = 1", "insert into expenses values (1, 2)", "end"]).1, ["Column label takes a 'text' between quotes.", "Stopped at line 2."]);
        // a single line needs no place
        assert_eq!(each(&mut sys, &["let x = 1 / 0"]), ["Division by zero."]);
        assert_eq!(each(&mut sys, &["end"]), ["end closes if, while, for or program: none is open."]);
        assert_eq!(each(&mut sys, &["else"]), ["else belongs to an if."]);
        // lines as long as the serial port takes them are run whole
        each(&mut sys, &["create table t (n int)"]);
        let values: Vec<String> = (0..8).map(|n| format!("({n} + 100000000000000000)")).collect();
        let long = format!("insert into t values {}", values.join(", "));
        assert!(long.len() > 200 && long.len() <= crate::lang::LINE_MAX);
        assert_eq!(block(&mut sys, &["if 1 = 1", &long, "end"]).1, ["8 rows added to t, with ids 1 to 8."]);
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
}
