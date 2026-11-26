//! Asks another machine something over HTTP: one question, one answer.
//!
//! It is the other half of `web.rs`: there a browser calls us and gets a
//! page, here we call a machine that serves pages, say what we want in a
//! few lines of text, and read what it says back. `net get` and `net
//! post` use it to try the calling side of the network by hand, and it
//! is how XilaOS speaks to services of the Internet.
//!
//! A question is a line that says what is wanted (`GET /path`), the name
//! of the machine asked (as it was written: its address, or a name that
//! `dns.rs` found the address of), lines of the asker's own if it has
//! any (a key, the kind of what follows), and for a `POST` some bytes
//! that go with it.
//!
//! The answer starts with a line that says how it went, as a number: 200
//! is yes, 404 is "not found". Then come lines about what follows, then
//! what was asked for, which ends in one of three ways: after a length
//! that one of the lines said; in chunks, each after its own length, the
//! last one empty; or when the machine hangs up. `Ask` reads all three,
//! gives what was asked for without the lines and the lengths, and knows
//! an answer that was cut short from one that is whole. What does not
//! fit where it is to be kept is counted and let go.
//!
//! Asked plainly (`http://`), nothing is encrypted and nothing says who
//! answers. Asked through TLS (`https://`), the question and the answer
//! are sealed and the machine has proved it is the one that was named:
//! `step` is then given the `xila_tls::Client` that does it, and carries
//! its bytes over TCP. Either way, what comes back is never run.

use core::fmt::Write;

use crate::net::Ip;
use crate::tcp::{Id, Tcp};
use crate::text::FixedStr;

/// The most the first lines of a question take: what is asked, of whom,
/// and the asker's own lines.
pub const HEAD: usize = 1024;
/// The most an answer may be, kept or not: beyond that it is given up.
pub const MOST: usize = 4 << 20;
/// The longest line of an answer that is read whole; the lines that
/// matter are short, and the end of a longer one is let go.
const LINE: usize = 160;
/// The most lines an answer may have before what was asked for.
const LINES: u16 = 200;
/// Ticks without anything said or heard after which a machine is given up.
pub const PATIENCE: u64 = 500;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failed {
    /// No place for one more conversation, or a question that cannot be
    /// asked: too long, or with what would make it two.
    NoRoom,
    /// The machine refused the call, or never answered it.
    NoAnswer,
    /// It answered the call and then said nothing in time.
    Silent,
    /// What it said is not an answer to a question like ours.
    NotAnAnswer,
    /// It hung up before it had said all it announced.
    Cut,
    /// It said more than is listened to (`MOST`).
    TooLong,
    /// It was asked by its name, and the address of that name was not found.
    Name(crate::dns::Failed),
    /// It was to be asked through TLS, and that could not be: why.
    Secret(xila_tls::Error),
    /// It was to be asked through TLS, and this machine cannot: its
    /// processor has no source of chance, without which nothing is secret.
    NoChance,
    /// It was to be asked through TLS, and this machine does not know the
    /// date, without which no certificate can be checked.
    NoClock,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Going,
    /// Answered, whole: the number that says how it went.
    Done(u16),
    Failed(Failed),
}

/// What is asked: how (`GET` or `POST`), lines of the asker's own, each
/// ended by a carriage return and a line feed (`Authorization: ...`),
/// and the bytes that go along. Without a line that says what kind the
/// bytes are, they are said to be plain text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Question<'a> {
    pub how: &'a [u8],
    pub lines: &'a [u8],
    pub bytes: &'a [u8],
}

impl<'a> Question<'a> {
    /// The plainest question: give me that page.
    pub const GET: Question<'static> = Question { how: b"GET", lines: b"", bytes: b"" };

    /// Sends some text.
    pub fn post(bytes: &'a [u8]) -> Question<'a> {
        Question { how: b"POST", lines: b"", bytes }
    }
}

/// Where the reading of an answer has got.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// The first line: how it went.
    Status,
    /// The lines about what follows, until an empty one. True for those
    /// of an answer that only says "go on": the real one comes after.
    Lines(bool),
    /// What was asked for, of a length that was said: the bytes left.
    Counted(usize),
    /// In chunks: the line that says the length of the next one.
    Chunk,
    /// The bytes left of a chunk.
    Chunked(usize),
    /// The empty line that ends a chunk.
    ChunkEnd,
    /// The lines that may follow the last chunk, until an empty one.
    After,
    /// What was asked for, until the machine hangs up.
    ToTheEnd,
    /// All of it has come.
    Over,
}

pub struct Ask {
    id: Id,
    /// When something was last said or heard.
    since: u64,
    /// Ticks of silence that are waited through; `PATIENCE` unless set.
    pub patience: u64,
    head: FixedStr<HEAD>,
    /// How much of the head, then of the bytes that go along, is said.
    sent: usize,
    stage: Stage,
    line: FixedStr<LINE>,
    lines: u16,
    heard: bool,
    status: u16,
    length: Option<usize>,
    chunked: bool,
    /// Bytes of what was asked for that were kept, and that came in all.
    pub kept: usize,
    pub total: usize,
    step: Step,
}

/// Whether the asker's lines are lines: printable, each ended, none empty.
fn lines_are_lines(lines: &[u8]) -> bool {
    lines.is_empty() || (lines.ends_with(b"\r\n") && lines.split_inclusive(|byte| *byte == b'\n').all(|line| line.len() > 2 && line.ends_with(b"\r\n") && line[..line.len() - 2].iter().all(|byte| (0x20..0x7f).contains(byte))))
}

/// Whether one of the asker's lines is the one of that name.
fn has_line(lines: &[u8], name: &[u8]) -> bool {
    lines.split(|byte| *byte == b'\n').any(|line| line.len() > name.len() && line[..name.len()].eq_ignore_ascii_case(name) && line[name.len()] == b':')
}

impl Ask {
    /// Calls the machine of a place at the address `to` and asks it a
    /// question. The machine is told the name it is asked by, as it was
    /// written, since one address may serve the pages of several names.
    /// The bytes of the question are not kept: `step` is given them again.
    pub fn start(tcp: &mut Tcp, to: Ip, place: &Place, question: &Question, now: u64) -> Result<Ask, Failed> {
        let Question { how, lines, bytes } = *question;
        let mut head = FixedStr::<HEAD>::new();
        let path: &[u8] = if place.path.is_empty() { b"/" } else { place.path };
        // the first line, who is asked, and that one answer is all that is wanted
        let length = how.len() + path.len() + place.machine.len() + lines.len() + 140;
        if length > HEAD || how.is_empty() || path.iter().chain(how).chain(place.machine).any(|byte| *byte <= b' ' || *byte > 0x7e) || !lines_are_lines(lines) {
            return Err(Failed::NoRoom);
        }
        head.push_bytes(how);
        head.push(b' ');
        head.push_bytes(path);
        head.push_bytes(b" HTTP/1.1\r\nHost: ");
        head.push_bytes(place.machine);
        // the port, unless it is the usual one for the way it is asked
        if place.port != if place.secret { 443 } else { 80 } {
            let _ = write!(head, ":{}", place.port);
        }
        head.push_bytes(b"\r\nUser-Agent: XilaOS/");
        head.push_bytes(crate::VERSION);
        head.push_bytes(b"\r\nConnection: close\r\n");
        head.push_bytes(lines);
        if !bytes.is_empty() || how == b"POST" {
            if !has_line(lines, b"Content-Type") {
                head.push_bytes(b"Content-Type: text/plain\r\n");
            }
            let _ = write!(head, "Content-Length: {}\r\n", bytes.len());
        }
        head.push_bytes(b"\r\n");
        let id = tcp.connect(to, place.port, now).ok_or(Failed::NoRoom)?;
        Ok(Ask { id, ..Ask::unasked(head, now) })
    }

    /// Hangs up, whatever was being said: nobody wants the answer any more.
    pub fn drop_it(&mut self, tcp: &mut Tcp) {
        if self.step == Step::Going {
            self.step = Step::Failed(Failed::Silent);
            tcp.abort(self.id);
        }
    }

    /// Everything but the call itself.
    fn unasked(head: FixedStr<HEAD>, now: u64) -> Ask {
        Ask { id: 0, since: now, patience: PATIENCE, head, sent: 0, stage: Stage::Status, line: FixedStr::new(), lines: 0, heard: false, status: 0, length: None, chunked: false, kept: 0, total: 0, step: Step::Going }
    }

    /// What is left to say: the rest of the head, then the rest of the bytes.
    fn unsaid<'a>(&'a self, bytes: &'a [u8]) -> &'a [u8] {
        match self.sent.checked_sub(self.head.len()) {
            None => &self.head.bytes()[self.sent..],
            Some(sent) => bytes.get(sent..).unwrap_or(&[]),
        }
    }

    /// Bytes of what was asked for: kept in `into` as far as it has room,
    /// counted in any case.
    fn keep(&mut self, bytes: &[u8], into: &mut [u8]) {
        let count = bytes.len().min(into.len() - self.kept.min(into.len()));
        into[self.kept..self.kept + count].copy_from_slice(&bytes[..count]);
        self.kept += count;
        self.total += bytes.len();
        if self.total > MOST {
            self.step = Step::Failed(Failed::TooLong);
        }
    }

    /// One line of the answer that is not part of what was asked for.
    fn line(&mut self, line: &[u8]) -> Result<Stage, Failed> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        // the lines before what was asked for are not without end; those
        // between chunks are as many as there are chunks
        if matches!(self.stage, Stage::Lines(_) | Stage::After) {
            self.lines += 1;
            if self.lines > LINES {
                return Err(Failed::NotAnAnswer);
            }
        }
        Ok(match self.stage {
            // `HTTP/1.1 200 OK` says 200
            Stage::Status => {
                let mut words = line.split(|byte| *byte == b' ').filter(|word| !word.is_empty());
                if !words.next().is_some_and(|word| word.starts_with(b"HTTP/")) {
                    return Err(Failed::NotAnAnswer);
                }
                let number = words.next().and_then(crate::text::parse_number).filter(|number| (100..600).contains(number)).ok_or(Failed::NotAnAnswer)?;
                self.status = number as u16;
                // "go on" (100) is not the answer: the answer follows it
                Stage::Lines(number < 200)
            }
            Stage::Lines(true) if line.is_empty() => Stage::Status,
            // an empty line: what was asked for follows, in the way the lines said
            Stage::Lines(false) if line.is_empty() => match (self.status, self.chunked, self.length) {
                // "nothing to give" and "you have it already" are followed by nothing
                (204 | 304, _, _) | (_, false, Some(0)) => Stage::Over,
                (_, true, _) => Stage::Chunk,
                (_, false, Some(length)) => Stage::Counted(length),
                (_, false, None) => Stage::ToTheEnd,
            },
            Stage::Lines(go_on) => {
                let colon = line.iter().position(|byte| *byte == b':').unwrap_or(line.len());
                let (name, value) = (&line[..colon], line.get(colon + 1..).unwrap_or(b"").trim_ascii());
                if name.eq_ignore_ascii_case(b"Content-Length") {
                    self.length = Some(crate::text::parse_number(value).ok_or(Failed::NotAnAnswer)?);
                } else if name.eq_ignore_ascii_case(b"Transfer-Encoding") {
                    self.chunked = value.windows(7).any(|word| word.eq_ignore_ascii_case(b"chunked"));
                }
                Stage::Lines(go_on)
            }
            // the length of a chunk, in hexadecimal; what may follow a `;` is not read
            Stage::Chunk => {
                let digits = line.split(|byte| *byte == b';').next().unwrap_or(b"").trim_ascii();
                if digits.is_empty() || digits.len() > 6 {
                    return Err(Failed::NotAnAnswer);
                }
                let mut length = 0;
                for digit in digits {
                    length = length * 16 + (*digit as char).to_digit(16).ok_or(Failed::NotAnAnswer)? as usize;
                }
                match length {
                    0 => Stage::After,
                    length => Stage::Chunked(length),
                }
            }
            Stage::ChunkEnd if line.is_empty() => Stage::Chunk,
            Stage::After if line.is_empty() => Stage::Over,
            Stage::After => Stage::After,
            _ => return Err(Failed::NotAnAnswer),
        })
    }

    /// Bytes of the answer came, in the order they were said.
    fn take(&mut self, mut bytes: &[u8], into: &mut [u8]) {
        self.heard |= !bytes.is_empty();
        while !bytes.is_empty() && self.step == Step::Going {
            match self.stage {
                Stage::Counted(left) | Stage::Chunked(left) => {
                    let count = left.min(bytes.len());
                    self.keep(&bytes[..count], into);
                    bytes = &bytes[count..];
                    self.stage = match (self.stage, left - count) {
                        (Stage::Counted(_), 0) => Stage::Over,
                        (Stage::Counted(_), left) => Stage::Counted(left),
                        (_, 0) => Stage::ChunkEnd,
                        (_, left) => Stage::Chunked(left),
                    };
                }
                Stage::ToTheEnd => {
                    self.keep(bytes, into);
                    bytes = &[];
                }
                // what comes after the answer is not part of it
                Stage::Over => bytes = &[],
                _ => {
                    let byte = bytes[0];
                    bytes = &bytes[1..];
                    if byte != b'\n' {
                        // the end of a line too long is let go
                        self.line.push(byte);
                        continue;
                    }
                    let line = self.line;
                    self.line.clear();
                    match self.line(line.bytes()) {
                        Ok(stage) => self.stage = stage,
                        Err(why) => self.step = Step::Failed(why),
                    }
                }
            }
        }
    }

    /// What the answer comes to when the machine has no more to say.
    /// `sure` is whether that is known to be its own doing: always when
    /// asked plainly, and through TLS only when it said so under seal,
    /// since anyone on the way can cut a conversation.
    fn ended(&self, sure: bool, called: bool) -> Step {
        match self.stage {
            Stage::Over => Step::Done(self.status),
            Stage::ToTheEnd if sure => Step::Done(self.status),
            Stage::Status if !self.heard && !called => Step::Failed(Failed::NoAnswer),
            Stage::Status if !self.heard => Step::Failed(Failed::Silent),
            // not even a first line; unless they said "go on" and then nothing
            Stage::Status if self.status == 0 => Step::Failed(Failed::NotAnAnswer),
            _ => Step::Failed(Failed::Cut),
        }
    }

    /// Does a little of the asking: says what is not said yet, reads what
    /// has come, and keeps what was asked for in `into`. `said` is the
    /// bytes of the question, and `into` where the answer goes: the same
    /// two at every call (`kept` bytes of `into` are filled). To be called
    /// often, with `Net::speak` after it. With `secret`, a TLS client
    /// started for the machine that is asked, all of it goes through TLS.
    pub fn step(&mut self, tcp: &mut Tcp, mut secret: Option<&mut xila_tls::Client>, said: &[u8], into: &mut [u8], now: u64) -> Step {
        if self.step != Step::Going {
            return self.step;
        }
        let id = self.id;
        let mut bytes = [0u8; 1024];
        loop {
            let mut moved = 0;
            match secret.as_deref_mut() {
                None => {
                    if tcp.connected(id) {
                        let count = tcp.write(id, self.unsaid(said));
                        self.sent += count;
                        moved += count;
                    }
                    let count = tcp.read(id, &mut bytes);
                    self.take(&bytes[..count], into);
                    moved += count;
                }
                Some(tls) => {
                    use xila_tls::State;
                    // what came goes to TLS, as much as it can take now;
                    // what it opened is the answer
                    let wanted = tls.wants().min(bytes.len());
                    let count = tcp.read(id, &mut bytes[..wanted]);
                    tls.input(&bytes[..count]);
                    let read = tls.read(&mut bytes);
                    self.take(&bytes[..read], into);
                    // the question, once the handshake is over
                    let asked = if tls.state() == State::Open { tls.write(self.unsaid(said)) } else { 0 };
                    self.sent += asked;
                    // and what TLS has to send, as TCP has room for it
                    let room = if tcp.connected(id) { tcp.room(id).min(bytes.len()) } else { 0 };
                    let out = tls.output(&mut bytes[..room]);
                    tcp.write(id, &bytes[..out]);
                    moved += count + read + asked + out;
                    if let (State::Failed(why), Step::Going) = (tls.state(), self.step) {
                        self.step = Step::Failed(Failed::Secret(why));
                    }
                }
            }
            if moved == 0 || self.step != Step::Going {
                break;
            }
            self.since = now;
        }
        // through TLS, that they have finished is only sure when they said so under seal
        let sure = secret.as_deref().is_none_or(|tls| tls.state() == xila_tls::State::Ended);
        let over = tcp.ended(id) || (secret.is_some() && sure);
        if self.step != Step::Going {
            // given up already: nothing more to conclude
        } else if self.stage == Stage::Over || over {
            self.step = self.ended(sure, tcp.connected(id) || self.sent > 0);
        } else if now >= self.since + self.patience {
            self.step = Step::Failed(if tcp.connected(id) { Failed::Silent } else { Failed::NoAnswer });
        }
        if self.step != Step::Going {
            // done with them, whatever they still have to say; what TLS
            // tells a server it gives up on is let out first
            if tcp.alive(id) && !over && !matches!(self.step, Step::Failed(Failed::Secret(_))) {
                tcp.abort(id);
            } else {
                tcp.close(id);
            }
        }
        self.step
    }
}

/// Where to ask: the machine as it was written, its address if it was
/// written as one and not as a name, the port, the path, and whether
/// through TLS.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Place<'a> {
    pub machine: &'a [u8],
    pub ip: Option<Ip>,
    pub port: u16,
    pub path: &'a [u8],
    pub secret: bool,
}

/// Reads where to ask, written as `192.168.1.20`, `192.168.1.20:8000` or
/// `192.168.1.20:8000/some/path`, with or without `http://` before it,
/// and with a name in place of the address (`example.org/some/path`):
/// the machine, the port (80 if none is said) and the path. With
/// `https://` before it, it is asked through TLS, on port 443 if none
/// is said.
pub fn place(text: &[u8]) -> Option<Place<'_>> {
    let (text, secret) = match text.strip_prefix(b"https://") {
        Some(text) => (text, true),
        None => (text.strip_prefix(b"http://").unwrap_or(text), false),
    };
    let slash = text.iter().position(|byte| *byte == b'/').unwrap_or(text.len());
    let (machine, path) = text.split_at(slash);
    let (address, port) = match machine.iter().position(|byte| *byte == b':') {
        Some(colon) => (&machine[..colon], crate::text::parse_number(&machine[colon + 1..]).filter(|port| (1..65536).contains(port))? as u16),
        None => (machine, if secret { 443 } else { 80 }),
    };
    let ip = crate::net::ip_of(address);
    (ip.is_some() || crate::dns::valid(address)).then_some(Place { machine: address, ip, port, path, secret })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_to_ask_is_read_in_the_forms_people_write() {
        let at = |machine: &'static str, ip, port, path: &'static str| Some(Place { machine: machine.as_bytes(), ip, port, path: path.as_bytes(), secret: false });
        assert_eq!(place(b"192.168.1.20"), at("192.168.1.20", Some([192, 168, 1, 20]), 80, ""));
        assert_eq!(place(b"192.168.1.20:8000"), at("192.168.1.20", Some([192, 168, 1, 20]), 8000, ""));
        assert_eq!(place(b"http://192.168.1.20:8000/notes/1?x=2"), at("192.168.1.20", Some([192, 168, 1, 20]), 8000, "/notes/1?x=2"));
        assert_eq!(place(b"10.0.2.2/"), at("10.0.2.2", Some([10, 0, 2, 2]), 80, "/"));
        // a name in place of the address: its address is to be found
        assert_eq!(place(b"example.org"), at("example.org", None, 80, ""));
        assert_eq!(place(b"http://Example.org:8000/notes/1"), at("Example.org", None, 8000, "/notes/1"));
        assert_eq!(place(b"the-mac/"), at("the-mac", None, 80, "/"));
        // through TLS: another port when none is said
        let sealed = |place: Option<Place<'static>>| place.map(|place| Place { secret: true, ..place });
        assert_eq!(place(b"https://example.org"), sealed(at("example.org", None, 443, "")));
        assert_eq!(place(b"https://example.org:8443/v1/models"), sealed(at("example.org", None, 8443, "/v1/models")));
        assert_eq!(place(b"https://192.168.1.20/"), sealed(at("192.168.1.20", Some([192, 168, 1, 20]), 443, "/")));
        for wrong in ["", "192.168.1", "192.168.1.20:", "192.168.1.20:0", "192.168.1.20:70000", "192.168.1.20:80x", "https://", "ftp://192.168.1.20", ":80", "a b", "example..org", "example.org:x"] {
            assert_eq!(place(wrong.as_bytes()), None, "{wrong}");
        }
    }

    const TO: Ip = [192, 168, 1, 20];

    fn at(machine: &'static [u8], port: u16, path: &'static [u8], secret: bool) -> Place<'static> {
        Place { machine, ip: None, port, path, secret }
    }

    #[test]
    fn a_question_is_a_few_lines_of_text() {
        let mut tcp = Tcp::new();
        let said = |tcp: &mut Tcp, place: Place, question: Question| {
            let ask = Ask::start(tcp, TO, &place, &question, 0).unwrap_or_else(|_| panic!());
            // said in order: the head, then the bytes, wherever the saying has got
            let mut ask = ask;
            let mut all = Vec::new();
            while !ask.unsaid(question.bytes).is_empty() {
                let piece = &ask.unsaid(question.bytes)[..ask.unsaid(question.bytes).len().min(7)];
                all.extend_from_slice(piece);
                ask.sent += piece.len();
            }
            String::from_utf8(all).unwrap()
        };
        assert_eq!(said(&mut tcp, at(b"192.168.1.20", 8000, b"/notes", false), Question::GET), "GET /notes HTTP/1.1\r\nHost: 192.168.1.20:8000\r\nUser-Agent: XilaOS/0.1\r\nConnection: close\r\n\r\n");
        // asked by its name, the machine is told that name
        assert_eq!(said(&mut tcp, at(b"example.org", 80, b"", false), Question::GET), "GET / HTTP/1.1\r\nHost: example.org\r\nUser-Agent: XilaOS/0.1\r\nConnection: close\r\n\r\n");
        // through TLS the usual port is another one, and the question is the same
        assert!(said(&mut tcp, at(b"example.org", 443, b"/v1", true), Question::GET).contains("\r\nHost: example.org\r\n"));
        assert!(said(&mut tcp, at(b"example.org", 80, b"/v1", true), Question::GET).contains("\r\nHost: example.org:80\r\n"));
        // bytes that go along: how many is said, and that they are text unless the asker says what they are
        assert_eq!(
            said(&mut tcp, at(b"192.168.1.20", 80, b"/notes", false), Question::post(b"big expense: rent")),
            "POST /notes HTTP/1.1\r\nHost: 192.168.1.20\r\nUser-Agent: XilaOS/0.1\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: 17\r\n\r\nbig expense: rent"
        );
        assert!(said(&mut tcp, at(b"192.168.1.20", 80, b"/", false), Question::post(b"")).ends_with("Content-Length: 0\r\n\r\n"));
        // lines of the asker's own: a key, the kind of the bytes
        let mine = Question { how: b"POST", lines: b"Authorization: Bearer abc\r\ncontent-type: application/json\r\n", bytes: b"{}" };
        assert_eq!(
            said(&mut tcp, at(b"example.org", 443, b"/v1/chat", true), mine),
            "POST /v1/chat HTTP/1.1\r\nHost: example.org\r\nUser-Agent: XilaOS/0.1\r\nConnection: close\r\nAuthorization: Bearer abc\r\ncontent-type: application/json\r\nContent-Length: 2\r\n\r\n{}"
        );
        assert_eq!(tcp.count(), 7);
    }

    #[test]
    fn what_would_not_be_one_question_is_not_asked() {
        let mut tcp = Tcp::new();
        let refused = |tcp: &mut Tcp, place: Place, question: Question| matches!(Ask::start(tcp, TO, &place, &question, 0), Err(Failed::NoRoom));
        // a line break or a space where none may be
        assert!(refused(&mut tcp, at(b"192.168.1.20", 80, b"/a\r\nX: y", false), Question::GET));
        assert!(refused(&mut tcp, at(b"192.168.1.20", 80, b"/a b", false), Question::GET));
        assert!(refused(&mut tcp, at(b"a\r\nX: y", 80, b"/", false), Question::GET));
        assert!(refused(&mut tcp, at(b"192.168.1.20", 80, b"/", false), Question { how: b"GET /x", ..Question::GET }));
        assert!(refused(&mut tcp, at(b"192.168.1.20", 80, b"/", false), Question { how: b"", ..Question::GET }));
        // lines that are not lines: not ended, empty (which would end the head), or with what no line holds
        for lines in [&b"X: y"[..], b"X: y\n", b"\r\n", b"X: y\r\n\r\n", b"X: y\r\n\r\nGET / HTTP/1.1\r\n", b"X: \x07\r\n", b"X: caf\xe9\r\n"] {
            assert!(refused(&mut tcp, at(b"192.168.1.20", 80, b"/", false), Question { lines, ..Question::GET }), "{lines:?}");
        }
        // more lines than there is room for
        let long = [b"X: yyyyyyyyyyyyyyyy\r\n".as_slice(); 60].concat();
        assert!(refused(&mut tcp, at(b"192.168.1.20", 80, b"/", false), Question { lines: &long, ..Question::GET }));
        assert_eq!(tcp.count(), 0, "nobody was called");
        // the longest name with the longest path and a key still fit, whatever the bytes weigh
        static NAME: [u8; crate::dns::NAME] = [b'a'; crate::dns::NAME];
        static PATH: [u8; 200] = [b'p'; 200];
        let key = [b"Authorization: Bearer ".as_slice(), &[b'k'; 200], b"\r\n"].concat();
        let bytes = vec![b'x'; 100_000];
        assert!(Ask::start(&mut tcp, TO, &at(&NAME, 65000, &PATH, false), &Question { how: b"POST", lines: &key, bytes: &bytes }, 0).is_ok());
    }

    /// What an answer comes to: how it ends (as `Ask::step` would see it
    /// when the machine hangs up after saying it), what was kept and how
    /// many bytes there were in all. The answer is given in pieces of
    /// `piece` bytes, and kept in a buffer of `room`.
    fn heard(answer: &[u8], piece: usize, room: usize) -> (Step, Vec<u8>, usize) {
        let mut ask = Ask::unasked(FixedStr::new(), 0);
        let mut into = vec![0u8; room];
        for part in answer.chunks(piece) {
            ask.take(part, &mut into);
        }
        let step = if ask.step == Step::Going { ask.ended(true, true) } else { ask.step };
        (step, into[..ask.kept].to_vec(), ask.total)
    }

    fn all(answer: &[u8]) -> (Step, Vec<u8>, usize) {
        let whole = heard(answer, answer.len().max(1), 4096);
        // however the bytes arrive, the answer is the same
        for piece in [1, 2, 3, 7, 64] {
            assert_eq!(heard(answer, piece, 4096), whole, "{piece} bytes at a time");
        }
        whole
    }

    #[test]
    fn an_answer_of_a_said_length_is_read_to_that_length() {
        assert_eq!(all(b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 12\r\n\r\nhello\nthere\n"), (Step::Done(200), b"hello\nthere\n".to_vec(), 12));
        // the name of a line in any capitals, and lines ended the short way
        assert_eq!(all(b"HTTP/1.1 404 Not Found\ncontent-LENGTH:  3 \n\nno\n"), (Step::Done(404), b"no\n".to_vec(), 3));
        // what comes after the length is not part of it
        assert_eq!(all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokand more"), (Step::Done(200), b"ok".to_vec(), 2));
        // nothing to give
        assert_eq!(all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"), (Step::Done(200), vec![], 0));
        assert_eq!(all(b"HTTP/1.1 204 No Content\r\n\r\n"), (Step::Done(204), vec![], 0));
        assert_eq!(all(b"HTTP/1.1 304 Not Modified\r\nContent-Length: 10\r\n\r\n"), (Step::Done(304), vec![], 0));
        // hung up before the length was reached
        assert_eq!(all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\nhello").0, Step::Failed(Failed::Cut));
        assert_eq!(all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n").0, Step::Failed(Failed::Cut));
        // a length that is not one
        assert_eq!(all(b"HTTP/1.1 200 OK\r\nContent-Length: many\r\n\r\nhello").0, Step::Failed(Failed::NotAnAnswer));
    }

    #[test]
    fn an_answer_in_chunks_is_put_together() {
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n5\r\nhello\r\n1\r\n \r\nB;with=what-we-do-not-read\r\nfrom chunks\r\n0\r\n\r\n";
        assert_eq!(all(chunked), (Step::Done(200), b"hello from chunks".to_vec(), 17));
        // lines after the last chunk, and a chunk that holds line breaks and what looks like a length
        assert_eq!(all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: gzip, Chunked\r\n\r\n8\r\n0\r\n\r\n5\r\n\r\n0\r\nX-After: yes\r\n\r\n"), (Step::Done(200), b"0\r\n\r\n5\r\n".to_vec(), 8));
        // chunks win over a length, when both are said
        assert_eq!(all(b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n"), (Step::Done(200), b"ok".to_vec(), 2));
        // hung up before the empty chunk: cut, wherever
        for end in [chunked.len() - 1, chunked.len() - 5, chunked.len() - 8, 90, 85] {
            assert_eq!(all(&chunked[..end]).0, Step::Failed(Failed::Cut), "cut at {end}");
        }
        // what is not a length, and a chunk that does not end where it said
        assert_eq!(all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nfive\r\nhello\r\n0\r\n\r\n").0, Step::Failed(Failed::NotAnAnswer));
        assert_eq!(all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\r\n").0, Step::Failed(Failed::NotAnAnswer));
        assert_eq!(all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhello\r\n0\r\n\r\n").0, Step::Failed(Failed::NotAnAnswer));
    }

    #[test]
    fn an_answer_with_no_length_goes_on_until_they_hang_up() {
        assert_eq!(all(b"HTTP/1.0 200 OK\r\n\r\nall of\nthis"), (Step::Done(200), b"all of\nthis".to_vec(), 11));
        assert_eq!(all(b"HTTP/1.0 200 OK\r\n\r\n"), (Step::Done(200), vec![], 0));
        // through TLS, hanging up proves nothing unless it was said under seal: anyone can cut a cable
        let mut ask = Ask::unasked(FixedStr::new(), 0);
        ask.take(b"HTTP/1.0 200 OK\r\n\r\npay 10", &mut [0; 64]);
        assert_eq!((ask.ended(true, true), ask.ended(false, true)), (Step::Done(200), Step::Failed(Failed::Cut)));
        // with a length, the end is known without them saying anything
        let mut ask = Ask::unasked(FixedStr::new(), 0);
        ask.take(b"HTTP/1.0 200 OK\r\nContent-Length: 6\r\n\r\npay 10", &mut [0; 64]);
        assert_eq!((ask.stage, ask.ended(false, true)), (Stage::Over, Step::Done(200)));
    }

    #[test]
    fn go_on_is_not_the_answer() {
        assert_eq!(all(b"HTTP/1.1 100 Continue\r\nX: y\r\n\r\nHTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok"), (Step::Done(201), b"ok".to_vec(), 2));
        assert_eq!(all(b"HTTP/1.1 100 Continue\r\n\r\n").0, Step::Failed(Failed::Cut));
    }

    #[test]
    fn what_is_not_an_answer_is_said_so() {
        for other in [&b"SSH-2.0-OpenSSH\r\n"[..], b"HTTP/1.1 abc\r\n\r\n", b"HTTP/1.1 99 X\r\n\r\n", b"HTTP/1.1 600 X\r\n\r\n", b"HTTP/1.1\r\n\r\n", b"hello", b"\r\n\r\n"] {
            assert_eq!(all(other).0, Step::Failed(Failed::NotAnAnswer), "{other:?}");
        }
        // nothing at all: they never took the call, or took it and said nothing
        let ask = Ask::unasked(FixedStr::new(), 0);
        assert_eq!((ask.ended(true, false), ask.ended(true, true)), (Step::Failed(Failed::NoAnswer), Step::Failed(Failed::Silent)));
        // lines without end before what was asked for
        let endless = [b"HTTP/1.1 200 OK\r\n".as_slice(), &b"X: y\r\n".repeat(300)].concat();
        assert_eq!(all(&endless).0, Step::Failed(Failed::NotAnAnswer));
        // a line longer than is read whole: its end is let go, and the rest is read
        let long = [b"HTTP/1.1 200 OK\r\nSet-Cookie: ".as_slice(), &[b'c'; 5000], b"\r\nContent-Length: 2\r\n\r\nok"].concat();
        assert_eq!(all(&long), (Step::Done(200), b"ok".to_vec(), 2));
    }

    #[test]
    fn what_does_not_fit_is_counted_and_let_go() {
        let mut answer = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        let mut said = Vec::new();
        for n in 0..300u32 {
            let line = format!("line {n} of a long page\n");
            answer.extend_from_slice(format!("{:x}\r\n{line}\r\n", line.len()).as_bytes());
            said.extend_from_slice(line.as_bytes());
        }
        answer.extend_from_slice(b"0\r\n\r\n");
        for piece in [1, 100, 1460, answer.len()] {
            assert_eq!(heard(&answer, piece, 640), (Step::Done(200), said[..640].to_vec(), said.len()), "{piece} bytes at a time");
            assert_eq!(heard(&answer, piece, 0), (Step::Done(200), vec![], said.len()));
            assert_eq!(heard(&answer, piece, 100_000).1, said);
        }
        // more than is listened to
        let mut ask = Ask::unasked(FixedStr::new(), 0);
        ask.take(b"HTTP/1.0 200 OK\r\n\r\n", &mut []);
        let much = vec![0u8; 1 << 20];
        for _ in 0..4 {
            assert_eq!(ask.step, Step::Going);
            ask.take(&much, &mut []);
        }
        ask.take(b"x", &mut []);
        assert_eq!(ask.step, Step::Failed(Failed::TooLong));
    }
}
