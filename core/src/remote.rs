//! The prompt, from another machine of the local network.
//!
//! It is closed when the machine starts, and nothing is kept of it from
//! one start to the next. `remote on`, typed at the machine itself, opens
//! it and shows a code; whoever has the code can then send commands, one
//! per message, to a port of this machine, and gets back what the prompt
//! of the serial port would have written. `remote off` closes it.
//!
//! What protects it, and what does not:
//! - only machines of the same local network are answered, and only if
//!   their message starts with the code; the others get no answer at all,
//!   so that nothing tells them a prompt is there;
//! - after twenty messages with a wrong code it closes itself;
//! - `install` and `disk erase`, which take a disk, and `remote on` are
//!   refused there: they are typed at the machine;
//! - but nothing is encrypted. The code and the commands cross the
//!   network as they are, and a machine of that network that can listen
//!   to it can read the code and use it. It is a door for a network one
//!   trusts, kept shut the rest of the time.
//!
//! A message is text: the code, a number, and a line for the prompt, as
//! in `K7QD-2MXP 12 select * from tasks`. The number tells one request
//! from the next. The network may lose a message or deliver it twice, so
//! the answer to the last request is kept: the same number from the same
//! machine gets the same answer again and the line is not run twice. An
//! answer comes back in parts of at most `PART` bytes, each starting
//! with a line that says the number, which part it is, and how many
//! there are.

use core::fmt::Write;

use crate::net::Ip;
use crate::shell::{Out, Style, System, Table};
use crate::text::{parse_number, FixedStr};

/// The port the prompt listens on.
pub const PORT: u16 = 4000;
/// Bytes of an answer in one message, and the most messages of one answer.
pub const PART: usize = 1200;
pub const PARTS: usize = 12;
/// One message with its first line.
pub const MESSAGE: usize = PART + 40;
const MOST: usize = PART * PARTS;
/// Room kept at the end of an answer for its last lines.
const ENDING: usize = 48;
/// The longest line taken.
pub const LINE: usize = 240;
/// Wrong codes after which the prompt closes itself.
pub const WRONG: u32 = 20;

/// Letters and digits that are not taken for one another when read aloud
/// or copied by hand: no I, O, 0 or 1.
const LETTERS: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub type Code = [u8; 8];

/// Draws a code from `chance`, which must be a number nobody else can guess.
pub fn draw(chance: u64) -> Code {
    let mut state = chance;
    let mut code = [0; 8];
    for letter in &mut code {
        // a step of a generator that spreads every bit over all the others
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = state;
        mixed = (mixed ^ mixed >> 30).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ mixed >> 27).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^= mixed >> 31;
        *letter = LETTERS[(mixed >> 59) as usize];
    }
    code
}

/// Whether what was given is the code: in letters of either size, with or
/// without its dash. Every letter is compared whatever the ones before:
/// how long this takes tells nothing of how much of a guess was right.
pub fn same(code: Code, given: &[u8]) -> bool {
    let (mut count, mut differs) = (0, 0u8);
    for &byte in given {
        if byte == b'-' {
            continue;
        }
        differs |= if count < code.len() { byte.to_ascii_uppercase() ^ code[count] } else { 1 };
        count += 1;
    }
    count == code.len() && differs == 0
}

/// The code as it is shown: in two halves.
pub fn written(code: Code) -> FixedStr<9> {
    let mut text = FixedStr::new();
    text.push_bytes(&code[..4]);
    text.push(b'-');
    text.push_bytes(&code[4..]);
    text
}

/// What a command says, as the prompt of the serial port writes it: a
/// line per line, a table between `#table` and `#end` with its cells
/// apart by tabs, and at the end `#ok` or `#error`.
pub struct Answer {
    bytes: [u8; MOST],
    len: usize,
    /// Bytes that did not fit.
    lost: usize,
}

impl Answer {
    pub const fn new() -> Answer {
        Answer { bytes: [0; MOST], len: 0, lost: 0 }
    }

    pub fn clear(&mut self) {
        (self.len, self.lost) = (0, 0);
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn push(&mut self, text: &[u8]) {
        let room = (MOST - ENDING).saturating_sub(self.len).min(text.len());
        self.bytes[self.len..self.len + room].copy_from_slice(&text[..room]);
        self.len += room;
        self.lost += text.len() - room;
    }

    /// The events the session listens to, as the serial prompt gives
    /// them: with the command that follows them.
    pub fn events(&mut self, sys: &mut System) {
        while let Some(event) = sys.events.take(crate::events::NETWORK) {
            self.push(b"#event ");
            self.push(event.channel.bytes());
            self.push(b"\t");
            self.push(event.text.bytes());
            self.push(b"\n");
        }
        let lost = sys.events.take_lost(crate::events::NETWORK);
        if lost > 0 {
            let mut note = FixedStr::<32>::new();
            let _ = write!(note, "#lost {lost}\n");
            self.push(note.bytes());
        }
    }

    /// The last lines: what did not fit, whether a block is open and more
    /// lines are expected, and how the command went.
    pub fn end(&mut self, more: bool, ok: bool) {
        let mut ending = FixedStr::<ENDING>::new();
        if self.lost > 0 {
            // what was cut may have been the middle of a line
            if self.len > 0 && self.bytes[self.len - 1] != b'\n' {
                ending.push(b'\n');
            }
            let _ = write!(ending, "#cut {}\n", self.lost);
        }
        if more {
            ending.push_bytes(b"#more\n");
        }
        ending.push_bytes(if ok { b"#ok\n" } else { b"#error\n" });
        let room = ending.len().min(MOST - self.len);
        self.bytes[self.len..self.len + room].copy_from_slice(&ending.bytes()[..room]);
        self.len += room;
    }
}

impl Out for Answer {
    fn line(&mut self, _style: Style, text: &[u8]) {
        self.push(text);
        self.push(b"\n");
    }

    fn table(&mut self, table: &Table) {
        self.push(b"#table ");
        self.push(table.name.bytes());
        for col in 0..table.cols {
            self.push(b"\t");
            self.push(table.header[col].bytes());
        }
        self.push(b"\n");
        for row in 0..table.rows {
            for col in 0..table.cols {
                if col > 0 {
                    self.push(b"\t");
                }
                self.push(table.cells[row][col].bytes());
            }
            self.push(b"\n");
        }
        self.push(b"#end\n");
    }
}

/// How many messages an answer takes: one at least.
pub fn parts(answer: &Answer) -> usize {
    answer.len.div_ceil(PART).max(1)
}

/// Writes one message of an answer: a line with the number of the
/// request, which part this is from 1 and how many there are, then the
/// bytes of that part. Returns its length.
pub fn part(answer: &Answer, number: u32, index: usize, into: &mut [u8; MESSAGE]) -> usize {
    let count = parts(answer);
    let mut first = FixedStr::<40>::new();
    let _ = write!(first, "{number} {} {count}\n", index + 1);
    let from = (index * PART).min(answer.len);
    let bytes = &answer.bytes[from..(from + PART).min(answer.len)];
    into[..first.len()].copy_from_slice(first.bytes());
    into[first.len()..first.len() + bytes.len()].copy_from_slice(bytes);
    first.len() + bytes.len()
}

/// What to do with a message that came to the port.
#[derive(PartialEq, Eq, Debug)]
pub enum Asked<'a> {
    /// No answer: closed, not from this network, not the code, not a request.
    Nothing,
    /// The request answered last, again: send the same answer.
    Again(u32),
    /// A new request, with its number: run the line and answer.
    Run(u32, &'a [u8]),
}

/// The state of the prompt for the network.
pub struct Remote {
    code: Option<Code>,
    /// Who asked last, and the number of that request.
    last: Option<(Ip, u32)>,
    /// Requests run since it was opened, wrong codes since then, and who
    /// asked last.
    pub served: u32,
    pub wrong: u32,
    pub from: Option<Ip>,
    /// It closed itself after too many wrong codes.
    pub gave_up: bool,
}

impl Remote {
    pub const fn new() -> Remote {
        Remote { code: None, last: None, served: 0, wrong: 0, from: None, gave_up: false }
    }

    /// Opens the prompt with a new code drawn from `chance`, which must
    /// be a number nobody else can guess.
    pub fn open(&mut self, chance: u64) -> Code {
        let code = draw(chance);
        *self = Remote { code: Some(code), ..Remote::new() };
        code
    }

    pub fn close(&mut self) {
        self.code = None;
        self.last = None;
    }

    /// The code, while the prompt is open.
    pub fn code(&self) -> Option<Code> {
        self.code
    }

    /// Reads a message that came to the port from `from`; `local` says
    /// whether that address is on this machine's own network.
    pub fn asked<'a>(&mut self, from: Ip, local: bool, message: &'a [u8]) -> Asked<'a> {
        let Some(code) = self.code else {
            return Asked::Nothing;
        };
        if !local {
            return Asked::Nothing;
        }
        let message = crate::text::trim(message);
        let end = message.iter().position(|byte| *byte == b' ').unwrap_or(message.len());
        if !same(code, &message[..end]) {
            self.wrong += 1;
            if self.wrong >= WRONG {
                self.close();
                self.gave_up = true;
            }
            return Asked::Nothing;
        }
        let rest = crate::text::trim(&message[end..]);
        let end = rest.iter().position(|byte| *byte == b' ').unwrap_or(rest.len());
        let Some(number) = parse_number(&rest[..end]) else {
            return Asked::Nothing;
        };
        let line = crate::text::trim(&rest[end..]);
        if line.len() > LINE {
            return Asked::Nothing;
        }
        let number = number as u32;
        self.from = Some(from);
        if self.last == Some((from, number)) {
            return Asked::Again(number);
        }
        self.last = Some((from, number));
        self.served += 1;
        Asked::Run(number, line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC_ROOM: Ip = [192, 168, 1, 20];
    const OTHER: Ip = [192, 168, 1, 30];

    fn opened() -> (Remote, Code) {
        let mut remote = Remote::new();
        let code = remote.open(0x1234_5678_9abc_def0);
        (remote, code)
    }

    fn request(code: Code, rest: &str) -> std::vec::Vec<u8> {
        let mut message = written(code).bytes().to_vec();
        message.extend_from_slice(rest.as_bytes());
        message
    }

    #[test]
    fn it_is_closed_until_it_is_opened() {
        let mut remote = Remote::new();
        assert_eq!(remote.code(), None);
        assert_eq!(remote.asked(MAC_ROOM, true, b"ABCDEFGH 1 help"), Asked::Nothing);
        // nothing was counted either: there is no code to be wrong about
        assert_eq!((remote.wrong, remote.served), (0, 0));
        let code = remote.open(1);
        assert_eq!(remote.code(), Some(code));
        remote.close();
        assert_eq!(remote.asked(MAC_ROOM, true, &request(code, " 1 help")), Asked::Nothing);
    }

    #[test]
    fn a_code_is_eight_letters_one_cannot_misread() {
        let mut remote = Remote::new();
        let mut seen = std::collections::HashSet::new();
        for chance in 0..2000u64 {
            let code = remote.open(chance);
            assert!(code.iter().all(|letter| LETTERS.contains(letter)), "{code:?}");
            seen.insert(code);
        }
        // numbers that follow one another give codes that have nothing in common
        assert_eq!(seen.len(), 2000);
        let (first, second) = (remote.open(7), remote.open(8));
        assert!(first.iter().zip(second).filter(|(a, b)| **a == *b).count() < 5);
        assert_eq!(written(*b"K7QD2MXP").bytes(), b"K7QD-2MXP");
    }

    #[test]
    fn a_request_with_the_code_is_run_once() {
        let (mut remote, code) = opened();
        let message = request(code, " 12 select * from tasks");
        assert_eq!(remote.asked(MAC_ROOM, true, &message), Asked::Run(12, b"select * from tasks"));
        // the same again: the network lost the answer, or the message came twice
        assert_eq!(remote.asked(MAC_ROOM, true, &message), Asked::Again(12));
        assert_eq!(remote.asked(MAC_ROOM, true, &request(code, " 13 uptime")), Asked::Run(13, b"uptime"));
        // the same number from another machine is another request
        assert_eq!(remote.asked(OTHER, true, &request(code, " 13 uptime")), Asked::Run(13, b"uptime"));
        assert_eq!((remote.served, remote.wrong, remote.from), (3, 0, Some(OTHER)));
        // the code without its dash, in small letters; an empty line, as at any prompt
        let mut plain: std::vec::Vec<u8> = code.to_ascii_lowercase().to_vec();
        plain.extend_from_slice(b" 14");
        assert_eq!(remote.asked(MAC_ROOM, true, &plain), Asked::Run(14, b""));
        plain.extend_from_slice(b"   \n");
        assert_eq!(remote.asked(MAC_ROOM, true, &plain), Asked::Again(14));
    }

    #[test]
    fn what_is_not_a_request_gets_no_answer() {
        let (mut remote, code) = opened();
        // from beyond this network, even with the code
        assert_eq!(remote.asked([8, 8, 8, 8], false, &request(code, " 1 help")), Asked::Nothing);
        assert_eq!(remote.wrong, 0);
        // no number, a number that is none, a line too long
        for rest in ["", " help", " -1 help", " 1x help"] {
            assert_eq!(remote.asked(MAC_ROOM, true, &request(code, rest)), Asked::Nothing, "{rest}");
        }
        let long = std::format!(" 2 {}", "x".repeat(LINE + 1));
        assert_eq!(remote.asked(MAC_ROOM, true, &request(code, &long)), Asked::Nothing);
        assert_eq!((remote.wrong, remote.served), (0, 0));
    }

    #[test]
    fn it_closes_itself_after_too_many_wrong_codes() {
        let (mut remote, code) = opened();
        let mut near = code;
        near[7] = if near[7] == b'A' { b'B' } else { b'A' };
        for wrong in [&b"AAAAAAAA 1 help"[..], b"", b"help", &request(near, " 1 help"), &request(code, "X 1 help"), &code[..7]] {
            assert_eq!(remote.asked(MAC_ROOM, true, wrong), Asked::Nothing);
        }
        assert_eq!(remote.wrong, 6);
        assert_eq!(remote.asked(MAC_ROOM, true, &request(code, " 1 help")), Asked::Run(1, b"help"));
        for _ in 6..WRONG {
            assert!(!remote.gave_up);
            remote.asked(OTHER, true, b"guess 1 help");
        }
        assert!(remote.gave_up && remote.code().is_none());
        // the right code comes too late
        assert_eq!(remote.asked(MAC_ROOM, true, &request(code, " 2 help")), Asked::Nothing);
        // opened again, it has a new code and counts from nothing
        let again = remote.open(99);
        assert!(again != code && !remote.gave_up && remote.wrong == 0);
    }

    #[test]
    fn an_answer_is_what_the_serial_prompt_writes() {
        let mut answer = Answer::new();
        answer.line(Style::Out, b"Hello.");
        let mut table = Table::new(b"things", &[b"name", b"size"]);
        table.row(&[b"one", b"1"]);
        table.row(&[b"two", b"22"]);
        answer.table(&table);
        answer.end(false, true);
        assert_eq!(answer.bytes(), b"Hello.\n#table things\tname\tsize\none\t1\ntwo\t22\n#end\n#ok\n");
        answer.clear();
        answer.line(Style::Error, b"No.");
        answer.end(true, false);
        assert_eq!(answer.bytes(), b"No.\n#more\n#error\n");
    }

    #[test]
    fn an_answer_goes_in_parts() {
        let mut answer = Answer::new();
        answer.end(false, true);
        let mut message = [0u8; MESSAGE];
        assert_eq!(parts(&answer), 1);
        let length = part(&answer, 7, 0, &mut message);
        assert_eq!(&message[..length], b"7 1 1\n#ok\n");

        answer.clear();
        for row in 0..100 {
            let mut line = FixedStr::<40>::new();
            let _ = write!(line, "row {row:03} of a long answer, to the end");
            answer.line(Style::Out, line.bytes());
        }
        answer.end(false, true);
        let whole = answer.bytes().to_vec();
        assert_eq!(parts(&answer), whole.len().div_ceil(PART));
        let mut back = std::vec::Vec::new();
        for index in 0..parts(&answer) {
            let length = part(&answer, 999_999_999, index, &mut message);
            let first = std::format!("999999999 {} {}\n", index + 1, parts(&answer));
            assert!(message[..length].starts_with(first.as_bytes()));
            assert!(length - first.len() <= PART);
            back.extend_from_slice(&message[first.len()..length]);
        }
        assert_eq!(back, whole);
    }

    #[test]
    fn an_answer_too_long_says_what_it_left_out() {
        let mut answer = Answer::new();
        let line = [b'x'; 99];
        for _ in 0..200 {
            answer.line(Style::Out, &line);
        }
        answer.end(true, false);
        assert!(parts(&answer) <= PARTS);
        let text = core::str::from_utf8(answer.bytes()).unwrap();
        let cut = 200 * 100 - (MOST - ENDING);
        assert!(text.ends_with(&std::format!("x\n#cut {cut}\n#more\n#error\n")), "{}", &text[text.len() - 60..]);
        // every part fits a message
        let mut message = [0u8; MESSAGE];
        assert!(part(&answer, 999_999_999, PARTS - 1, &mut message) <= MESSAGE);
    }
}
