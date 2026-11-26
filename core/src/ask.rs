//! Asks another machine something over HTTP: one question, one answer.
//!
//! It is the other half of `web.rs`: there a browser calls us and gets a
//! page, here we call a machine that serves pages, say what we want in a
//! few lines of text, and read what it says back. `net get` uses it to
//! try the calling side of the network by hand.
//!
//! A question is a line that says what is wanted (`GET /path`), the name
//! of the machine asked (as it was written: its address, or a name that
//! `dns.rs` found the address of), and for a `POST` some bytes that go
//! with it. The
//! answer starts with a line that says how it went, as a number: 200 is
//! yes, 404 is "not found". The start of the answer is kept, as much as
//! fits; the rest is let go.
//!
//! Nothing is encrypted and nothing says who answers. What comes back is
//! shown to the user and never run.

use core::fmt::Write;

use crate::net::Ip;
use crate::tcp::{Id, Tcp};
use crate::text::FixedStr;

/// The most that is kept of an answer, and of a question.
pub const ANSWER: usize = 1024;
const QUESTION: usize = 512;
/// Ticks after which a machine that has not finished answering is given up.
const PATIENCE: u64 = 500;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failed {
    /// No place for one more conversation, or a question too long.
    NoRoom,
    /// The machine refused the call, or never answered it.
    NoAnswer,
    /// It answered the call and then said nothing in time.
    Silent,
    /// What it said is not an answer to a question like ours.
    NotAnAnswer,
    /// It was asked by its name, and the address of that name was not found.
    Name(crate::dns::Failed),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Going,
    /// Answered: the number that says how it went.
    Done(u16),
    Failed(Failed),
}

pub struct Ask {
    id: Id,
    since: u64,
    question: FixedStr<QUESTION>,
    sent: usize,
    answer: [u8; ANSWER],
    answer_len: usize,
    step: Step,
}

impl Ask {
    /// Calls `to` on `port` and asks `how` (`GET` or `POST`) for `path`,
    /// with `bytes` going along for a `POST`. `machine` is how the machine
    /// was written, a name or an address: it is told so, since one
    /// address may serve the pages of several names.
    #[allow(clippy::too_many_arguments)]
    pub fn start(tcp: &mut Tcp, to: Ip, port: u16, machine: &[u8], how: &[u8], path: &[u8], bytes: &[u8], now: u64) -> Result<Ask, Failed> {
        let mut question = FixedStr::<QUESTION>::new();
        let path: &[u8] = if path.is_empty() { b"/" } else { path };
        // the first line, who is asked, and that one answer is all that is wanted
        let head = how.len() + path.len() + machine.len() + bytes.len() + 105;
        if head > QUESTION || path.iter().chain(how).chain(machine).any(|byte| *byte <= b' ' || *byte > 0x7e) {
            return Err(Failed::NoRoom);
        }
        question.push_bytes(how);
        question.push(b' ');
        question.push_bytes(path);
        question.push_bytes(b" HTTP/1.0\r\nHost: ");
        question.push_bytes(machine);
        if port != 80 {
            let _ = write!(question, ":{port}");
        }
        question.push_bytes(b"\r\nConnection: close\r\n");
        if !bytes.is_empty() || how == b"POST" {
            let _ = write!(question, "Content-Type: text/plain\r\nContent-Length: {}\r\n", bytes.len());
        }
        question.push_bytes(b"\r\n");
        question.push_bytes(bytes);
        let id = tcp.connect(to, port, now).ok_or(Failed::NoRoom)?;
        Ok(Ask { id, since: now, question, sent: 0, answer: [0; ANSWER], answer_len: 0, step: Step::Going })
    }

    /// Does a little of the asking: says what is not said yet, reads what
    /// has come. To be called often, with `Net::speak` after it.
    pub fn step(&mut self, tcp: &mut Tcp, now: u64) -> Step {
        if self.step != Step::Going {
            return self.step;
        }
        let id = self.id;
        if tcp.connected(id) && self.sent < self.question.len() {
            self.sent += tcp.write(id, &self.question.bytes()[self.sent..]);
        }
        let count = tcp.read(id, &mut self.answer[self.answer_len..]);
        self.answer_len += count;
        // what does not fit is let go, so that they can finish
        let mut rest = [0u8; 256];
        while self.answer_len == ANSWER && tcp.read(id, &mut rest) > 0 {}
        let over = tcp.ended(id);
        if over || self.answer_len == ANSWER {
            // all they had to say, or all we keep: that is the answer
            self.step = match self.status() {
                Some(status) => Step::Done(status),
                None if self.answer_len == 0 && !tcp.connected(id) && self.sent == 0 => Step::Failed(Failed::NoAnswer),
                None if self.answer_len == 0 => Step::Failed(Failed::Silent),
                None => Step::Failed(Failed::NotAnAnswer),
            };
        } else if now >= self.since + PATIENCE {
            self.step = Step::Failed(if tcp.connected(id) { Failed::Silent } else { Failed::NoAnswer });
        }
        if self.step != Step::Going {
            // done with them, whatever they still have to say
            if tcp.alive(id) && !over {
                tcp.abort(id);
            } else {
                tcp.close(id);
            }
        }
        self.step
    }

    /// The number in the first line of the answer: `HTTP/1.1 200 OK` says 200.
    fn status(&self) -> Option<u16> {
        let line = self.answer[..self.answer_len].split(|byte| *byte == b'\n').next()?;
        let mut words = line.split(|byte| *byte == b' ').filter(|word| !word.is_empty());
        if !words.next()?.starts_with(b"HTTP/") {
            return None;
        }
        let number = crate::text::parse_number(words.next()?)?;
        (100..600).contains(&number).then_some(number as u16)
    }

    /// What they said after the lines that come before it: the page, or
    /// as much of it as was kept.
    pub fn said(&self) -> &[u8] {
        let answer = &self.answer[..self.answer_len];
        let at = answer.windows(4).position(|four| four == b"\r\n\r\n").map(|at| at + 4);
        let at = at.or_else(|| answer.windows(2).position(|two| two == b"\n\n").map(|at| at + 2));
        &answer[at.unwrap_or(answer.len())..]
    }
}

/// Where to ask: the machine as it was written, its address if it was
/// written as one and not as a name, the port and the path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Place<'a> {
    pub machine: &'a [u8],
    pub ip: Option<Ip>,
    pub port: u16,
    pub path: &'a [u8],
}

/// Reads where to ask, written as `192.168.1.20`, `192.168.1.20:8000` or
/// `192.168.1.20:8000/some/path`, with or without `http://` before it,
/// and with a name in place of the address (`example.org/some/path`):
/// the machine, the port (80 if none is said) and the path.
pub fn place(text: &[u8]) -> Option<Place<'_>> {
    let text = text.strip_prefix(b"http://").unwrap_or(text);
    let slash = text.iter().position(|byte| *byte == b'/').unwrap_or(text.len());
    let (machine, path) = text.split_at(slash);
    let (address, port) = match machine.iter().position(|byte| *byte == b':') {
        Some(colon) => (&machine[..colon], crate::text::parse_number(&machine[colon + 1..]).filter(|port| (1..65536).contains(port))? as u16),
        None => (machine, 80),
    };
    let ip = crate::net::ip_of(address);
    (ip.is_some() || crate::dns::valid(address)).then_some(Place { machine: address, ip, port, path })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_to_ask_is_read_in_the_forms_people_write() {
        let at = |machine: &'static str, ip, port, path: &'static str| Some(Place { machine: machine.as_bytes(), ip, port, path: path.as_bytes() });
        assert_eq!(place(b"192.168.1.20"), at("192.168.1.20", Some([192, 168, 1, 20]), 80, ""));
        assert_eq!(place(b"192.168.1.20:8000"), at("192.168.1.20", Some([192, 168, 1, 20]), 8000, ""));
        assert_eq!(place(b"http://192.168.1.20:8000/notes/1?x=2"), at("192.168.1.20", Some([192, 168, 1, 20]), 8000, "/notes/1?x=2"));
        assert_eq!(place(b"10.0.2.2/"), at("10.0.2.2", Some([10, 0, 2, 2]), 80, "/"));
        // a name in place of the address: its address is to be found
        assert_eq!(place(b"example.org"), at("example.org", None, 80, ""));
        assert_eq!(place(b"http://Example.org:8000/notes/1"), at("Example.org", None, 8000, "/notes/1"));
        assert_eq!(place(b"the-mac/"), at("the-mac", None, 80, "/"));
        for wrong in ["", "192.168.1", "192.168.1.20:", "192.168.1.20:0", "192.168.1.20:70000", "192.168.1.20:80x", "https://192.168.1.20", ":80", "a b", "example..org", "example.org:x"] {
            assert_eq!(place(wrong.as_bytes()), None, "{wrong}");
        }
    }

    #[test]
    fn a_question_is_a_few_lines_of_text() {
        let mut tcp = Tcp::new();
        let ask = Ask::start(&mut tcp, [192, 168, 1, 20], 8000, b"192.168.1.20", b"GET", b"/notes", b"", 0).unwrap_or_else(|_| panic!());
        assert_eq!(ask.question.bytes(), b"GET /notes HTTP/1.0\r\nHost: 192.168.1.20:8000\r\nConnection: close\r\n\r\n");
        // asked by its name, the machine is told that name
        let ask = Ask::start(&mut tcp, [192, 168, 1, 20], 80, b"example.org", b"GET", b"/notes", b"", 0).unwrap_or_else(|_| panic!());
        assert_eq!(ask.question.bytes(), b"GET /notes HTTP/1.0\r\nHost: example.org\r\nConnection: close\r\n\r\n");
        let ask = Ask::start(&mut tcp, [192, 168, 1, 20], 80, b"192.168.1.20", b"POST", b"", b"big expense: rent", 0).unwrap_or_else(|_| panic!());
        assert_eq!(ask.question.bytes(), &b"POST / HTTP/1.0\r\nHost: 192.168.1.20\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: 17\r\n\r\nbig expense: rent"[..]);
        // what would not be one question is not sent: a line break in the path, or too much to say
        assert!(matches!(Ask::start(&mut tcp, [192, 168, 1, 20], 80, b"192.168.1.20", b"GET", b"/a\r\nX: y", b"", 0), Err(Failed::NoRoom)));
        assert!(matches!(Ask::start(&mut tcp, [192, 168, 1, 20], 80, b"192.168.1.20", b"GET", b"/a b", b"", 0), Err(Failed::NoRoom)));
        assert!(matches!(Ask::start(&mut tcp, [192, 168, 1, 20], 80, b"a\r\nX: y", b"GET", b"/", b"", 0), Err(Failed::NoRoom)));
        assert!(matches!(Ask::start(&mut tcp, [192, 168, 1, 20], 80, b"192.168.1.20", b"POST", b"/", &[b'x'; 500], 0), Err(Failed::NoRoom)));
        // the longest name with the longest path still fits
        let name = [b'a'; crate::dns::NAME];
        assert!(Ask::start(&mut tcp, [192, 168, 1, 20], 65000, &name, b"GET", &[b'p'; 200], b"", 0).is_ok());
        assert_eq!(tcp.count(), 4);
    }

    #[test]
    fn the_answer_says_how_it_went_and_what_was_said() {
        let mut ask = Ask { id: 0, since: 0, question: FixedStr::new(), sent: 0, answer: [0; ANSWER], answer_len: 0, step: Step::Going };
        let take = |ask: &mut Ask, text: &[u8]| {
            ask.answer[..text.len()].copy_from_slice(text);
            ask.answer_len = text.len();
        };
        take(&mut ask, b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nhello\nthere\n");
        assert_eq!((ask.status(), ask.said()), (Some(200), &b"hello\nthere\n"[..]));
        take(&mut ask, b"HTTP/1.1 404 Not Found\r\n\r\n");
        assert_eq!((ask.status(), ask.said()), (Some(404), &b""[..]));
        take(&mut ask, b"HTTP/1.1 204\n\n");
        assert_eq!((ask.status(), ask.said()), (Some(204), &b""[..]));
        for other in [&b"SSH-2.0-OpenSSH\r\n"[..], b"HTTP/1.1 abc\r\n\r\n", b"HTTP/1.1 99 X\r\n\r\n", b"hello", b""] {
            take(&mut ask, other);
            assert_eq!(ask.status(), None);
        }
    }
}
