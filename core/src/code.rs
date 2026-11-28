//! XilaScript compiled: the instructions the machine runs.
//!
//! A block typed at a prompt, or the lines of a program, are translated
//! once into instructions, and what is wrong with their form is told
//! then, before anything runs: a block that is not closed, an `else`
//! without its `if`, a calculation that cannot be read. What cannot be
//! known yet is not looked at: whether a name has a value, whether a
//! table or a program is there, what a command or a SELECT says. That
//! is told when the instruction runs, with its line.
//!
//! ```text
//! program restock                     0  open   c0
//!   for item in select name, qty      1  next   c0, else 4
//!       from stock where qty < 5      2  say    item.name
//!     say item.name                   3  jump   1
//!   end                               4  close  c0
//! end
//! ```
//!
//! An instruction holds numbers only: the line it came from, where to
//! jump, where its calculation is kept (`expr::Pool`). The texts it
//! needs (the name of a `let`, a SELECT, a command) are read from its
//! line, which the code keeps.

use crate::expr::{self, Expr, Kept, Names, Pool};
use crate::lang::{call_of, free_name, whole_select, word_of, Line, MAX_LINES, PARAMS};
use crate::sql::{Lexer, Message, Token};
use crate::text::{split_word, trim};

/// Loops over rows open one inside the other.
pub(crate) const LOOPS: usize = 4;
/// The `while` loops of one unit: each has its count of rounds.
pub(crate) const WHILES: usize = MAX_LINES / 2;
/// Instructions of one unit. No line makes more than three (a `while`),
/// so there is room for any 48 lines, those included that the prompt
/// cannot type but a row of the table of programs can hold: blocks
/// opened again and again and never closed. A jump still fits a byte.
const INSTRS: usize = 3 * MAX_LINES;
/// A jump that is not yet known to lead anywhere.
const NOWHERE: u8 = u8::MAX;

/// Where a value comes from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum What {
    /// A calculation, kept read.
    Value(Kept),
    /// A SELECT, read when it runs: its first column of its first row.
    Select,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Op {
    /// `let name = value`: the name is read from the line (`let_parts`).
    Let(What),
    /// `say value`, or `say` alone.
    Say(Option<What>),
    Jump(u8),
    /// Goes on when the condition holds, jumps when it does not.
    JumpUnless(Kept, u8),
    /// A `while` is entered: its count of rounds starts again.
    Enter(u8),
    /// One more round of that `while`.
    Round(u8),
    /// `for name in select ...`: opens this cursor on the SELECT of the
    /// line (`for_parts`).
    Open(u8),
    /// The next row of the cursor under the names `name.column`, or a
    /// jump when there is none.
    Next(u8, u8),
    Close(u8),
    /// `run name`, `run name(values)`, `name(values)`: the name and the
    /// values are read from the line (`call_parts`) when it runs.
    Call,
    /// Any other line: a command of the prompt, or the name of a program
    /// alone.
    Command,
    Return,
}

/// One instruction: what to do, the line it came from (from 0), and
/// whether a statement begins with it (statements are counted).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Instr {
    pub op: Op,
    pub line: u8,
    pub begins: bool,
}

/// What is wrong with the form of some lines, and where: `line` is
/// counted from 1 among the lines given.
#[derive(Debug)]
pub struct Fault {
    pub line: usize,
    pub message: Message,
}

/// The name and the value of `let name = value`, as written.
pub(crate) fn let_parts(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = word_of(line).1;
    let equals = rest.iter().position(|byte| *byte == b'=')?;
    Some((trim(&rest[..equals]), trim(&rest[equals + 1..])))
}

/// The name and the SELECT of `for name in select ...`.
pub(crate) fn for_parts(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let (name, after) = split_word(word_of(line).1);
    let (word_in, query) = word_of(after);
    (word_in.bytes() == b"in" && word_of(query).0.bytes() == b"select").then_some((name, query))
}

/// What follows the first word of a line: the value of `say`, the name
/// of `run`.
pub(crate) fn rest(line: &[u8]) -> &[u8] {
    word_of(line).1
}

/// The name of the program a line calls, and the values it gives it as
/// they are written between the parentheses (nothing for none). Empty
/// names for a line that calls nothing.
pub(crate) fn call_parts(line: &[u8]) -> (&[u8], &[u8]) {
    let (word, after) = word_of(line);
    call_of(if word.bytes() == b"run" { after } else { line }).unwrap_or((b"", b""))
}

/// Reads the values a call gives, calculations with commas between
/// them, and hands each to `each` as it is read. Returns how many there
/// are. A name in them is looked up when the calculation is worked out.
pub(crate) fn arguments(written: &[u8], each: &mut dyn FnMut(&Expr) -> Result<(), Message>) -> Result<usize, Message> {
    let mut lexer = Lexer::new(written);
    let mut count = 0;
    if lexer.peek() == Token::End {
        return Ok(0);
    }
    loop {
        if matches!(lexer.peek(), Token::End | Token::Symbol(b",")) {
            return Err(Message::from_bytes(b"A value is missing between the parentheses."));
        }
        if count == PARAMS {
            return Err(Message::from_bytes(b"A program takes eight values at most."));
        }
        // a SELECT there reads as a name with something after it
        let select = |message| whole_select(written).unwrap_or(message);
        let read = expr::parse(&mut lexer, Names::late()).map_err(select)?;
        count += 1;
        each(&read)?;
        if lexer.peek() == Token::End {
            return Ok(count);
        }
        lexer.expect(b",").map_err(select)?;
    }
}

/// Lines compiled: one unit of what the machine runs.
pub struct Code {
    lines: [Line; MAX_LINES],
    count: usize,
    instrs: [Instr; INSTRS],
    len: usize,
    pool: Pool,
}

/// A block that waits for its `end` while the lines are compiled.
#[derive(Clone, Copy)]
enum Open {
    /// The jump of the condition being read, which leads to the next
    /// branch; the last of the jumps that leave a branch for the end,
    /// each holding the one before it until the end is known; and
    /// whether a plain `else` came.
    If(Option<u8>, u8, bool),
    /// Where the condition is, and the jump that leaves the loop.
    While(u8, u8),
    /// Where the next row is asked for, and the cursor.
    For(u8, u8),
}

impl Code {
    pub const fn new() -> Code {
        Code { lines: [Line::new(); MAX_LINES], count: 0, instrs: [Instr { op: Op::Return, line: 0, begins: false }; INSTRS], len: 0, pool: Pool::new() }
    }

    /// How many instructions there are.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn at(&self, at: usize) -> Instr {
        self.instrs[at]
    }

    /// A line the code was made from, counted from 0.
    pub fn line(&self, at: usize) -> &Line {
        &self.lines[at]
    }

    pub(crate) fn pool(&self) -> &Pool {
        &self.pool
    }

    fn clear(&mut self) {
        (self.count, self.len) = (0, 0);
        self.pool.clear();
    }

    fn emit(&mut self, op: Op, line: usize, begins: bool) -> u8 {
        self.instrs[self.len] = Instr { op, line: line as u8, begins };
        self.len += 1;
        (self.len - 1) as u8
    }

    /// Makes a jump that was left open lead to the next instruction.
    fn land(&mut self, jump: u8) {
        let here = self.len as u8;
        match &mut self.instrs[jump as usize].op {
            Op::Jump(to) | Op::JumpUnless(_, to) | Op::Next(_, to) => *to = here,
            _ => {}
        }
    }

    /// Reads a calculation and keeps it.
    fn kept(&mut self, text: &[u8]) -> Result<Kept, Message> {
        let mut lexer = Lexer::new(text);
        // a SELECT in a calculation reads as a name with something after it
        let read = expr::parse(&mut lexer, Names::late()).and_then(|read| lexer.end().map(|_| read)).map_err(|message| whole_select(text).unwrap_or(message))?;
        self.pool.keep(&read).ok_or_else(|| Message::from_bytes(b"Too many calculations for one program: split it in two."))
    }

    /// Reads a value: a SELECT as a whole, or a calculation.
    fn what(&mut self, text: &[u8]) -> Result<What, Message> {
        if word_of(text).0.bytes() == b"select" {
            Ok(What::Select)
        } else {
            self.kept(text).map(What::Value)
        }
    }
}

impl Default for Code {
    fn default() -> Code {
        Code::new()
    }
}

/// Compiles lines into `into`, which holds nothing of what it held. On a
/// fault it holds nothing that can be run.
pub fn compile(lines: &[Line], into: &mut Code) -> Result<(), Fault> {
    into.clear();
    let made = translate(lines, into);
    if made.is_err() {
        into.clear();
    }
    made
}

fn translate(lines: &[Line], code: &mut Code) -> Result<(), Fault> {
    if lines.len() > MAX_LINES {
        return Err(Fault { line: MAX_LINES + 1, message: Message::from_bytes(b"A block takes 48 lines of 240 characters at most.") });
    }
    code.lines[..lines.len()].copy_from_slice(lines);
    code.count = lines.len();
    // the blocks that are open, with the line that opened each
    let mut open = [(Open::While(0, 0), 0usize); MAX_LINES];
    let (mut depth, mut whiles) = (0, 0u8);
    for (at, line) in lines.iter().enumerate() {
        let fault = |message: &[u8]| Fault { line: at + 1, message: Message::from_bytes(message) };
        let told = |message: Message| Fault { line: at + 1, message };
        let text = trim(line.bytes());
        if text.is_empty() || text[0] == b'#' {
            continue;
        }
        let (word, after) = word_of(text);
        match word.bytes() {
            b"let" => {
                let Some((name, value)) = let_parts(text) else {
                    return Err(fault(b"A name gets a value like this: let total = 3"));
                };
                if !free_name(name) {
                    return Err(fault(b"A name is a letter, then letters, digits or _, and not a command."));
                }
                let what = code.what(value).map_err(told)?;
                code.emit(Op::Let(what), at, true);
            }
            b"say" => {
                let what = if after.is_empty() { None } else { Some(code.what(after).map_err(told)?) };
                code.emit(Op::Say(what), at, true);
            }
            b"if" => {
                let condition = code.kept(after).map_err(told)?;
                let jump = code.emit(Op::JumpUnless(condition, NOWHERE), at, true);
                open[depth] = (Open::If(Some(jump), NOWHERE, false), at);
                depth += 1;
            }
            b"else" => {
                let Some((Open::If(pending, leaves, last), opened)) = depth.checked_sub(1).map(|top| open[top]) else {
                    return Err(fault(b"else belongs to an if."));
                };
                let (next, more) = word_of(after);
                let another = next.bytes() == b"if";
                if last || (!another && !after.is_empty()) {
                    return Err(fault(b"After else comes nothing, or if and a condition; a plain else is last."));
                }
                // the branch before ends here: it leaves for the end, and
                // its condition, when it does not hold, leads here
                let leaves = code.emit(Op::Jump(leaves), at, false);
                if let Some(pending) = pending {
                    code.land(pending);
                }
                let pending = if another {
                    let condition = code.kept(more).map_err(told)?;
                    Some(code.emit(Op::JumpUnless(condition, NOWHERE), at, false))
                } else {
                    None
                };
                open[depth - 1] = (Open::If(pending, leaves, !another), opened);
            }
            b"while" => {
                let condition = code.kept(after).map_err(told)?;
                code.emit(Op::Enter(whiles), at, true);
                let leaves = code.emit(Op::JumpUnless(condition, NOWHERE), at, false);
                code.emit(Op::Round(whiles), at, false);
                open[depth] = (Open::While(leaves, leaves), at);
                (depth, whiles) = (depth + 1, whiles + 1);
            }
            b"for" => {
                if !for_parts(text).is_some_and(|(name, _)| free_name(name)) {
                    return Err(fault(b"A loop over rows is written: for row in select ..."));
                }
                let cursor = open[..depth].iter().filter(|(block, _)| matches!(block, Open::For(..))).count();
                if cursor == LOOPS {
                    return Err(fault(b"Loops over rows go four deep at most."));
                }
                code.emit(Op::Open(cursor as u8), at, true);
                let next = code.emit(Op::Next(cursor as u8, NOWHERE), at, false);
                open[depth] = (Open::For(next, cursor as u8), at);
                depth += 1;
            }
            b"end" => {
                let Some(top) = depth.checked_sub(1) else {
                    return Err(fault(b"end closes if, while, for or program: none is open."));
                };
                depth = top;
                match open[top].0 {
                    Open::If(pending, mut leaves, _) => {
                        if let Some(pending) = pending {
                            code.land(pending);
                        }
                        // every branch that left for the end comes here
                        while leaves != NOWHERE {
                            let Op::Jump(before) = code.instrs[leaves as usize].op else {
                                break;
                            };
                            code.land(leaves);
                            leaves = before;
                        }
                    }
                    Open::While(condition, leaves) => {
                        code.emit(Op::Jump(condition), at, false);
                        code.land(leaves);
                    }
                    Open::For(next, cursor) => {
                        code.emit(Op::Jump(next), at, false);
                        code.land(next);
                        code.emit(Op::Close(cursor), at, false);
                    }
                }
            }
            b"return" => {
                code.emit(Op::Return, at, true);
            }
            b"run" => {
                let Some((_, given)) = call_of(after).filter(|_| !after.is_empty()) else {
                    return Err(fault(b"A program is run like this: run report, or run restock('cable', 5)"));
                };
                arguments(given, &mut |_| Ok(())).map_err(told)?;
                code.emit(Op::Call, at, true);
            }
            b"program" | b"on" | b"every" => return Err(fault(b"A program is made at the prompt, not inside another.")),
            b"panel" => return Err(fault(b"A panel is made at the prompt, not inside a block or a program.")),
            _ => match call_of(text) {
                // values between parentheses after a name that is no
                // command and no word of the language: a program is called
                Some((name, given)) if text.ends_with(b")") && free_name(name) => {
                    arguments(given, &mut |_| Ok(())).map_err(told)?;
                    code.emit(Op::Call, at, true);
                }
                _ => {
                    code.emit(Op::Command, at, true);
                }
            },
        }
    }
    match depth.checked_sub(1) {
        Some(top) => Err(Fault { line: open[top].1 + 1, message: Message::from_bytes(b"This block has no end.") }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<Line> {
        text.iter().map(|line| Line::from_bytes(line.as_bytes())).collect()
    }

    fn fault(text: &[&str]) -> (usize, String) {
        let mut code = Code::new();
        let fault = compile(&lines(text), &mut code).unwrap_err();
        assert_eq!(code.len(), 0);
        (fault.line, String::from_utf8_lossy(fault.message.bytes()).into_owned())
    }

    #[test]
    fn what_is_wrong_with_the_form_is_told_with_its_line() {
        assert_eq!(fault(&["let a = 1", "else"]), (2, "else belongs to an if.".into()));
        assert_eq!(fault(&["if 1 = 1", "else", "else", "end"]), (3, "After else comes nothing, or if and a condition; a plain else is last.".into()));
        assert_eq!(fault(&["if 1 = 1", "else", "else if 2 = 2", "end"]).0, 3);
        assert_eq!(fault(&["if 1 = 1", "else maybe", "end"]), (2, "After else comes nothing, or if and a condition; a plain else is last.".into()));
        assert_eq!(fault(&["say 1", "for x in tables", "end"]), (2, "A loop over rows is written: for row in select ...".into()));
        assert_eq!(fault(&["let total"]).1, "A name gets a value like this: let total = 3");
        assert_eq!(fault(&["let = 3"]).1, "A name is a letter, then letters, digits or _, and not a command.");
        assert_eq!(fault(&["let say = 3"]).1, "A name is a letter, then letters, digits or _, and not a command.");
        assert_eq!(fault(&["if 1 = 1", "program nested", "end", "end"]), (2, "A program is made at the prompt, not inside another.".into()));
        assert_eq!(fault(&["let a = 1 + select 2"]).1, "A select cannot be part of a calculation: keep it with let first.");
        assert_eq!(fault(&["say 1", "say 2 +"]).0, 2);
        assert_eq!(fault(&["say 1 2"]).0, 1);
        assert_eq!(fault(&["say 1", "while 1 = 1", "say 1"]), (2, "This block has no end.".into()));
        assert_eq!(fault(&["end"]).1, "end closes if, while, for or program: none is open.");
        // five loops over rows, one in the other
        let mut deep: Vec<&str> = vec!["for r in select 1"; 5];
        deep.extend(["end"; 5]);
        assert_eq!(fault(&deep), (5, "Loops over rows go four deep at most.".into()));
    }

    #[test]
    fn what_cannot_be_known_yet_is_not_a_fault() {
        let mut code = Code::new();
        // a name with no value, a table and a program that are not there, a command nobody knows
        let text = ["let a = nothing_yet + 1", "for r in select n from no_such_table", "say r.n", "end", "frobnicate", "run later", "# a note", "", "say", "let b = select 1"];
        compile(&lines(&text), &mut code).unwrap();
        assert!(code.len() >= 8);
        // every instruction knows its line, and the lines are kept
        assert!((0..code.len()).all(|at| (code.at(at).line as usize) < text.len()));
        assert_eq!(code.line(4).bytes(), b"frobnicate");
        // made again, it holds the new lines only
        compile(&lines(&["say 1"]), &mut code).unwrap();
        assert_eq!(code.len(), 1);
    }

    #[test]
    fn the_longest_program_fits_or_is_refused_whole() {
        let mut code = Code::new();
        // 48 lines that take the most instructions: loops closed at once
        let mut most = Vec::new();
        for _ in 0..24 {
            most.extend(["while 1 = 0", "end"]);
        }
        compile(&lines(&most), &mut code).unwrap();
        let mut mixed = Vec::new();
        for _ in 0..8 {
            mixed.extend(["if 1 = 1", "else if 2 = 2", "else if 3 = 3", "else", "end", "while 1 = 0"]);
        }
        // six of those, each leaving its while open, then an if left open too
        mixed.truncate(40);
        mixed.extend(["end"; 7]);
        mixed.push("say 1");
        assert_eq!(mixed.len(), 48);
        compile(&lines(&mixed), &mut code).unwrap();
        // 48 lines of the longest calculation one line may hold (21 nodes each) do not fit the pool: told when compiled
        let long = format!("let a = {}", (0..11).map(|n| n.to_string()).collect::<Vec<_>>().join(" + "));
        let many: Vec<&str> = (0..48).map(|_| long.as_str()).collect();
        assert_eq!(fault(&many).1, "Too many calculations for one program: split it in two.");
        // and 48 lines as programs have them do (nine nodes each)
        let plain: Vec<&str> = (0..48).map(|_| "say 'a: ' + a + ', b: ' + b + '.'").collect();
        compile(&lines(&plain), &mut code).unwrap();
        // more lines than a block takes
        let over: Vec<&str> = (0..49).map(|_| "say 1").collect();
        assert_eq!(fault(&over).0, 49);
    }

    #[test]
    fn lines_that_open_blocks_and_never_close_them_are_a_fault_not_a_crash() {
        // what the prompt cannot type, but a row of the table of programs can hold
        for opener in ["while 1 = 1", "if 1 = 1", "for r in select 1"] {
            let many: Vec<&str> = (0..48).map(|_| opener).collect();
            let (line, message) = fault(&many);
            assert!(line >= 1 && line <= 48, "{opener}");
            assert!(message == "This block has no end." || message == "Loops over rows go four deep at most.", "{opener}: {message}");
        }
        let mut mixed: Vec<&str> = (0..47).map(|_| "while 1 = 1").collect();
        mixed.push("else if 1 = 1");
        assert_eq!(fault(&mixed).1, "else belongs to an if.");
    }
}
