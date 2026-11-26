//! Names: the address of a machine that is known by its name.
//!
//! People and programs say `openrouter.ai`; packets go to `104.18.3.115`.
//! A name server knows which address a name has. The network says where
//! one is when it gives this machine its own address (DHCP), and XilaOS
//! asks it one question at a time, in a UDP message to its port 53: "the
//! IPv4 address of this name?". The answer says for how long it may be
//! kept, and it is kept that long, so the same name is not asked twice in
//! a row.
//!
//! This is the part without hardware and without the network under it:
//! `Names::look` says what to send and `Names::answered` takes what came.
//! `Net::name` carries both.
//!
//! The server does the real work of going from server to server (the
//! question asks it to). Only addresses of four bytes are asked for. An
//! answer too long for one UDP message is not asked again over TCP.
//!
//! Nothing says who answers: whoever can put a message on the cable
//! with the right two numbers can give a wrong address. What makes sure
//! of the machine at that address is what is spoken with it afterwards.

use crate::net::Ip;
use crate::text::FixedStr;

/// The longest name that is taken. The protocol allows 253; the names of
/// machines that people type are much shorter, and this one fits in a
/// line of the screen with what is said about it (`a name is at
/// 255.255.255.255.`).
pub const NAME: usize = 56;
/// The port a name server listens on.
pub const PORT: u16 = 53;
/// The longest question: the twelve bytes of the start, the name, four
/// bytes of what is asked of it.
pub const QUESTION: usize = 12 + NAME + 2 + 4;

/// Ticks of the timer in a second.
const SECOND: u64 = 100;
/// How many times a question is sent before the server is given up, one
/// second apart.
const TRIES: u8 = 3;
/// An address is kept for as long as its answer says, within these.
const KEPT: (u64, u64) = (10, 86_400);
const KNOWN: usize = 8;

pub type Name = FixedStr<NAME>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failed {
    /// The network named no server to ask.
    NoServer,
    /// The server did not answer.
    Silent,
    /// The server says no machine has that name.
    NoSuchName,
    /// The name exists and has no address of four bytes, or the server
    /// could not tell.
    NoAddress,
}

/// How far the looking for a name has got.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lookup {
    Found(Ip),
    Asking,
    Failed(Failed),
}

/// A message to send: to that address and port, from that port of ours,
/// that many bytes of the question.
pub type Send = (Ip, u16, u16, usize);

/// Whether a text can be the name of a machine: words of letters,
/// digits, `-` and `_` between dots, with a letter somewhere (four
/// numbers between dots are an address, and three are a mistake).
pub fn valid(name: &[u8]) -> bool {
    let name = name.strip_suffix(b".").unwrap_or(name);
    !name.is_empty()
        && name.len() <= NAME
        && name.iter().any(|byte| byte.is_ascii_alphabetic())
        && name.split(|byte| *byte == b'.').all(|word| !word.is_empty() && word.len() <= 63 && word.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-' || *byte == b'_'))
}

/// A name as it is kept and compared: small letters, no dot at the end.
fn kept(name: &[u8]) -> Name {
    let mut kept = Name::from_bytes(name.strip_suffix(b".").unwrap_or(name));
    kept.lower();
    kept
}

/// Writes the question "the IPv4 address of `name`?", told from others
/// by `number`. Returns its length.
pub fn question(name: &[u8], number: u16, into: &mut [u8; QUESTION]) -> usize {
    into.fill(0);
    into[..2].copy_from_slice(&number.to_be_bytes());
    // a question, and the server is asked to go and find the answer itself
    into[2] = 0x01;
    // one question
    into[5] = 1;
    let mut at = 12;
    // the name, word by word, each after its length; an empty word ends it
    for word in name.split(|byte| *byte == b'.').filter(|word| !word.is_empty()) {
        into[at] = word.len() as u8;
        into[at + 1..at + 1 + word.len()].copy_from_slice(word);
        at += 1 + word.len();
    }
    at += 1;
    // an address of four bytes (1), of the Internet (1)
    into[at..at + 4].copy_from_slice(&[0, 1, 0, 1]);
    at + 4
}

/// Where the name that starts at `at` ends. A name is words, each after
/// its length, and may end by pointing back into the message (two bytes
/// whose first two bits are set) instead of by an empty word.
fn after_name(bytes: &[u8], mut at: usize) -> Option<usize> {
    loop {
        match *bytes.get(at)? {
            0 => return Some(at + 1),
            length if length & 0xc0 == 0xc0 => return (at + 2 <= bytes.len()).then_some(at + 2),
            length if length > 63 => return None,
            length => at += 1 + length as usize,
        }
    }
}

/// Reads an answer to the question `number` about `name`. None if it is
/// not one: another number, another name, not an answer at all. Else the
/// address and for how many seconds it may be kept, or why there is none.
pub fn answer(bytes: &[u8], number: u16, name: &[u8]) -> Option<Result<(Ip, u32), Failed>> {
    // an answer (the first bit) to an ordinary question, with our number
    if bytes.len() < 12 || bytes[..2] != number.to_be_bytes() || bytes[2] & 0xf8 != 0x80 {
        return None;
    }
    // the question comes back before the answers: it must be ours
    if bytes[4..6] != [0, 1] {
        return None;
    }
    let mut at = 12;
    for word in name.split(|byte| *byte == b'.').filter(|word| !word.is_empty()) {
        let theirs = bytes.get(at + 1..at + 1 + word.len())?;
        if bytes[at] as usize != word.len() || !theirs.eq_ignore_ascii_case(word) {
            return None;
        }
        at += 1 + word.len();
    }
    if bytes.get(at..at + 5)? != [0, 0, 1, 0, 1] {
        return None;
    }
    at += 5;
    match bytes[3] & 0x0f {
        0 => {}
        3 => return Some(Err(Failed::NoSuchName)),
        _ => return Some(Err(Failed::NoAddress)),
    }
    // The answers: the name may be another name for a second one, which
    // the server follows, so the address is the first line that is one.
    // Whose address each line gives is not compared with the question:
    // the server is trusted with that, as it is with the address.
    for _ in 0..u16::from_be_bytes([bytes[6], bytes[7]]) {
        let Some(line) = after_name(bytes, at) else {
            break;
        };
        let Some(head) = bytes.get(line..line + 10) else {
            break;
        };
        let length = u16::from_be_bytes([head[8], head[9]]) as usize;
        let Some(value) = bytes.get(line + 10..line + 10 + length) else {
            break;
        };
        if head[..4] == [0, 1, 0, 1] && length == 4 {
            let seconds = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
            return Some(Ok(([value[0], value[1], value[2], value[3]], seconds)));
        }
        at = line + 10 + length;
    }
    // no address among them, or the answer was cut for being too long
    Some(Err(Failed::NoAddress))
}

#[derive(Clone, Copy)]
struct Known {
    name: Name,
    ip: Ip,
    /// The tick until which it is kept; 0 for an empty place.
    until: u64,
}

#[derive(Clone, Copy)]
struct Asking {
    name: Name,
    number: u16,
    port: u16,
    again: u64,
    tries: u8,
}

/// The names this machine knows, and the one it is asking about.
pub struct Names {
    /// Who is asked: what the network said, or what `net names` set.
    pub server: Option<(Ip, u16)>,
    /// Set by hand: what the network says no longer replaces it.
    pub by_hand: bool,
    known: [Known; KNOWN],
    asking: Option<Asking>,
    failed: Option<(Name, Failed)>,
    chance: u64,
    /// Questions sent since the start, for `net`.
    pub asked: u64,
}

impl Names {
    pub const fn new() -> Names {
        Names { server: None, by_hand: false, known: [Known { name: Name::new(), ip: [0; 4], until: 0 }; KNOWN], asking: None, failed: None, chance: 0x2545_f491_4f6c_dd1d, asked: 0 }
    }

    /// Mixes in a number nobody outside can guess: the numbers of the
    /// questions are drawn from it.
    pub fn stir(&mut self, chance: u64) {
        self.chance = self.chance.rotate_left(17) ^ chance;
    }

    /// Forgets everything: the card was set up again, perhaps on another
    /// network.
    pub fn forget(&mut self) {
        let (chance, asked) = (self.chance, self.asked);
        *self = Names::new();
        (self.chance, self.asked) = (chance, asked);
    }

    /// The server the network named, unless one was set by hand.
    pub fn told(&mut self, server: Ip) {
        if !self.by_hand && server != [0; 4] {
            self.server = Some((server, PORT));
        }
    }

    /// Sets by hand who is asked. What was known came from another server
    /// and is forgotten.
    pub fn set(&mut self, server: Ip, port: u16) {
        self.forget();
        (self.server, self.by_hand) = (Some((server, port)), true);
    }

    fn draw(&mut self) -> u32 {
        self.chance = self.chance.wrapping_mul(0x5851_f42d_4c95_7f2d).wrapping_add(0x1405_7b7e_f767_814f);
        ((self.chance ^ self.chance >> 29).wrapping_mul(0xbf58_476d_1ce4_e5b9) >> 32) as u32
    }

    /// Looks for the address of a name, which must be `valid`. To be
    /// called again and again until it says something else than
    /// `Asking`. When there is a message to send, it is written in
    /// `message` and what is returned with the state says where it goes.
    pub fn look(&mut self, name: &[u8], now: u64, message: &mut [u8; QUESTION]) -> (Lookup, Option<Send>) {
        let name = kept(name);
        if let Some(known) = self.known.iter().find(|known| known.until > now && known.name == name) {
            return (Lookup::Found(known.ip), None);
        }
        if let Some((_, why)) = self.failed.take_if(|(failed, _)| *failed == name) {
            return (Lookup::Failed(why), None);
        }
        let Some((server, port)) = self.server else {
            return (Lookup::Failed(Failed::NoServer), None);
        };
        let asking = match self.asking {
            Some(asking) if asking.name == name => {
                if now < asking.again {
                    return (Lookup::Asking, None);
                }
                if asking.tries >= TRIES {
                    self.asking = None;
                    return (Lookup::Failed(Failed::Silent), None);
                }
                // the same question again: the first may have been lost
                Asking { again: now + SECOND, tries: asking.tries + 1, ..asking }
            }
            // one question at a time: this one takes the place of any other
            _ => {
                let drawn = self.draw();
                // a port of ours among those kept for such uses
                Asking { name, number: drawn as u16, port: 49152 + (drawn >> 16) as u16 % 16384, again: now + SECOND, tries: 1 }
            }
        };
        (self.asking, self.failed) = (Some(asking), None);
        self.asked += 1;
        let length = question(name.bytes(), asking.number, message);
        (Lookup::Asking, Some((server, port, asking.port, length)))
    }

    /// The question could not be sent: nothing is being asked.
    pub fn give_up(&mut self) {
        self.asking = None;
    }

    /// A UDP message came for a port of ours. Returns true if it was for
    /// the port a question was asked from, whatever it holds.
    pub fn answered(&mut self, from: Ip, from_port: u16, port: u16, bytes: &[u8], now: u64) -> bool {
        let Some(asking) = self.asking.filter(|asking| asking.port == port) else {
            return false;
        };
        if self.server != Some((from, from_port)) {
            return true;
        }
        match answer(bytes, asking.number, asking.name.bytes()) {
            None => {}
            Some(Ok((ip, seconds))) => {
                // in the place of what is no longer kept, or of what ends first
                let at = (0..KNOWN).min_by_key(|at| if self.known[*at].until <= now { 0 } else { self.known[*at].until }).unwrap_or(0);
                self.known[at] = Known { name: asking.name, ip, until: now + (seconds as u64).clamp(KEPT.0, KEPT.1) * SECOND };
                self.asking = None;
            }
            Some(Err(why)) => {
                self.failed = Some((asking.name, why));
                self.asking = None;
            }
        }
        true
    }
}

impl Default for Names {
    fn default() -> Names {
        Names::new()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// What a server answers: the question back, then lines. A line is
    /// (kind, seconds, value); every line names its machine by pointing
    /// back at the name of the question, as servers do.
    pub fn served(asked: &[u8], code: u8, lines: &[(u16, u32, &[u8])]) -> Vec<u8> {
        let end = after_name(asked, 12).unwrap() + 4;
        let mut bytes = asked[..end].to_vec();
        (bytes[2], bytes[3]) = (0x81, 0x80 | code);
        bytes[6..8].copy_from_slice(&(lines.len() as u16).to_be_bytes());
        for (kind, seconds, value) in lines {
            bytes.extend_from_slice(&[0xc0, 12]);
            bytes.extend_from_slice(&kind.to_be_bytes());
            bytes.extend_from_slice(&[0, 1]);
            bytes.extend_from_slice(&seconds.to_be_bytes());
            bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
            bytes.extend_from_slice(value);
        }
        bytes
    }

    #[test]
    fn a_name_is_words_between_dots_with_a_letter_somewhere() {
        for name in ["openrouter.ai", "a", "the-mac", "xn--caf-dma.fr", "_dmarc.example.org", "example.org.", "3com.com", "A.B"] {
            assert!(valid(name.as_bytes()), "{name}");
        }
        let long = "a".repeat(64) + ".fr";
        let longest = "a.".repeat(NAME / 2 - 1) + "fr";
        assert!(valid(longest.as_bytes()) && longest.len() == NAME);
        for name in ["", ".", "192.168.1.20", "192.168.1", "a..b", ".a", "a b", "caf\u{e9}.fr", "a/b", "a:80", long.as_str(), &(longest + "x")] {
            assert!(!valid(name.as_bytes()), "{name}");
        }
    }

    #[test]
    fn a_question_is_the_name_word_by_word() {
        let mut message = [0xffu8; QUESTION];
        let length = question(b"www.example.org", 0x1234, &mut message);
        assert_eq!(&message[..length], b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x03www\x07example\x03org\x00\x00\x01\x00\x01");
        // the longest name fits
        let name = "a.".repeat(NAME / 2 - 1) + "fr";
        assert_eq!(question(name.as_bytes(), 1, &mut message), QUESTION);
    }

    #[test]
    fn an_answer_gives_the_address_and_how_long_to_keep_it() {
        let mut asked = [0u8; QUESTION];
        let length = question(b"www.example.org", 7, &mut asked);
        let asked = &asked[..length];
        let plain = served(asked, 0, &[(1, 300, &[93, 184, 216, 34])]);
        assert_eq!(answer(&plain, 7, b"www.example.org"), Some(Ok(([93, 184, 216, 34], 300))));
        // capitals in the name do not matter, in the question or in what comes back
        assert_eq!(answer(&plain, 7, b"WWW.Example.org"), Some(Ok(([93, 184, 216, 34], 300))));
        // another name for a second machine first (5), an address of sixteen bytes (28): the first of four bytes counts
        let other = served(asked, 0, &[(5, 60, b"\x03cdn\xc0\x10"), (28, 60, &[0; 16]), (1, 20, &[10, 0, 0, 1]), (1, 20, &[10, 0, 0, 2])]);
        assert_eq!(answer(&other, 7, b"www.example.org"), Some(Ok(([10, 0, 0, 1], 20))));
        // no such name, a server that could not tell, a name with no address
        assert_eq!(answer(&served(asked, 3, &[]), 7, b"www.example.org"), Some(Err(Failed::NoSuchName)));
        assert_eq!(answer(&served(asked, 2, &[]), 7, b"www.example.org"), Some(Err(Failed::NoAddress)));
        assert_eq!(answer(&served(asked, 0, &[(28, 60, &[0; 16])]), 7, b"www.example.org"), Some(Err(Failed::NoAddress)));
        assert_eq!(answer(&served(asked, 0, &[]), 7, b"www.example.org"), Some(Err(Failed::NoAddress)));
        // not ours: another number, another name, a question instead of an answer, a piece of one
        assert_eq!(answer(&plain, 8, b"www.example.org"), None);
        assert_eq!(answer(&plain, 7, b"www.example.com"), None);
        assert_eq!(answer(&plain, 7, b"example.org"), None);
        assert_eq!(answer(asked, 7, b"www.example.org"), None);
        assert_eq!(answer(&plain[..20], 7, b"www.example.org"), None);
        assert_eq!(answer(&[], 7, b"www.example.org"), None);
        // an answer cut anywhere is never read past its end
        for cut in 0..other.len() {
            let _ = answer(&other[..cut], 7, b"www.example.org");
        }
        let mut lying = plain.clone();
        let at = lying.len() - 5;
        lying[at] = 200;
        assert_eq!(answer(&lying, 7, b"www.example.org"), Some(Err(Failed::NoAddress)));
    }

    const SERVER: Ip = [10, 0, 2, 3];

    #[test]
    fn a_name_is_asked_once_then_known_for_as_long_as_was_said() {
        let mut names = Names::new();
        let mut message = [0u8; QUESTION];
        // nobody to ask
        assert_eq!(names.look(b"example.org", 0, &mut message), (Lookup::Failed(Failed::NoServer), None));
        names.told(SERVER);
        let (state, sent) = names.look(b"Example.org", 0, &mut message);
        let (to, to_port, from_port, length) = sent.unwrap();
        assert_eq!((state, to, to_port, length), (Lookup::Asking, SERVER, 53, 29));
        assert!(from_port >= 49152);
        // nothing more is sent while the answer may still come
        assert_eq!(names.look(b"example.org", 50, &mut message), (Lookup::Asking, None));
        let reply = served(&message[..length], 0, &[(1, 30, &[93, 184, 216, 34])]);
        // what comes from elsewhere, or to another port, changes nothing
        assert!(names.answered([10, 0, 2, 99], 53, from_port, &reply, 60));
        assert!(names.answered(SERVER, 5353, from_port, &reply, 60));
        assert!(!names.answered(SERVER, 53, from_port ^ 1, &reply, 60));
        assert_eq!(names.look(b"example.org", 60, &mut message), (Lookup::Asking, None));
        assert!(names.answered(SERVER, 53, from_port, &reply, 60));
        // known, whatever the capitals, for thirty seconds
        assert_eq!(names.look(b"EXAMPLE.ORG.", 61, &mut message), (Lookup::Found([93, 184, 216, 34]), None));
        assert_eq!(names.look(b"example.org", 60 + 2999, &mut message), (Lookup::Found([93, 184, 216, 34]), None));
        assert_eq!(names.asked, 1);
        assert!(matches!(names.look(b"example.org", 60 + 3000, &mut message), (Lookup::Asking, Some(_))));
        // a late answer to the first question is not one to the second
        names.answered(SERVER, 53, from_port, &reply, 3100);
        assert_eq!(names.look(b"example.org", 3100, &mut message).0, Lookup::Asking);
    }

    #[test]
    fn a_question_is_sent_three_times_before_the_server_is_given_up() {
        let mut names = Names::new();
        names.told(SERVER);
        let mut message = [0u8; QUESTION];
        let mut sent = Vec::new();
        let mut state = Lookup::Asking;
        let mut now = 0;
        while state == Lookup::Asking {
            let (next, send) = names.look(b"mute.example", now, &mut message);
            if let Some(send) = send {
                sent.push((now, send, message[..2].to_vec()));
            }
            state = next;
            now += 10;
        }
        assert_eq!(state, Lookup::Failed(Failed::Silent));
        // one second apart, the same question from the same port
        assert_eq!(sent.iter().map(|(at, ..)| *at).collect::<Vec<_>>(), [0, 100, 200]);
        assert!(sent.iter().all(|(_, send, number)| *send == sent[0].1 && *number == sent[0].2));
        assert_eq!(now, 310);
        // and asking again starts anew, with another number
        let (state, send) = names.look(b"mute.example", now, &mut message);
        assert!(state == Lookup::Asking && send.is_some() && message[..2] != sent[0].2[..]);
    }

    #[test]
    fn a_name_nobody_has_is_said_once_and_not_kept() {
        let mut names = Names::new();
        names.told(SERVER);
        let mut message = [0u8; QUESTION];
        let (_, sent) = names.look(b"none.example", 0, &mut message);
        let (_, _, port, length) = sent.unwrap();
        assert!(names.answered(SERVER, 53, port, &served(&message[..length], 3, &[]), 5));
        assert_eq!(names.look(b"none.example", 6, &mut message), (Lookup::Failed(Failed::NoSuchName), None));
        // asked again the next time: the name may exist by then
        assert!(matches!(names.look(b"none.example", 7, &mut message), (Lookup::Asking, Some(_))));
    }

    #[test]
    fn the_server_set_by_hand_stays_and_what_was_known_goes() {
        let mut names = Names::new();
        names.told(SERVER);
        let mut message = [0u8; QUESTION];
        let (_, sent) = names.look(b"example.org", 0, &mut message);
        let (_, _, port, length) = sent.unwrap();
        names.answered(SERVER, 53, port, &served(&message[..length], 0, &[(1, 600, &[1, 2, 3, 4])]), 1);
        assert_eq!(names.look(b"example.org", 2, &mut message).0, Lookup::Found([1, 2, 3, 4]));
        names.set([10, 0, 2, 2], 5300);
        // what the network says later does not replace it
        names.told(SERVER);
        let (state, sent) = names.look(b"example.org", 3, &mut message);
        assert_eq!((state, sent.map(|sent| (sent.0, sent.1))), (Lookup::Asking, Some(([10, 0, 2, 2], 5300))));
        // until the card is set up again
        names.forget();
        assert_eq!((names.server, names.asked), (None, 2));
        names.told(SERVER);
        assert_eq!(names.server, Some((SERVER, 53)));
        // no address at all is not a server
        names.forget();
        names.told([0; 4]);
        assert_eq!(names.server, None);
    }

    #[test]
    fn more_names_than_places_push_out_what_ends_first() {
        let mut names = Names::new();
        names.told(SERVER);
        let mut message = [0u8; QUESTION];
        for n in 0..KNOWN as u8 + 1 {
            let name = format!("m{n}.example");
            let (_, sent) = names.look(name.as_bytes(), 0, &mut message);
            let (_, _, port, length) = sent.unwrap();
            // the first is kept the shortest
            names.answered(SERVER, 53, port, &served(&message[..length], 0, &[(1, 100 + n as u32, &[10, 0, 0, n])]), 0);
        }
        assert!(matches!(names.look(b"m0.example", 1, &mut message), (Lookup::Asking, Some(_))));
        for n in 2..KNOWN as u8 + 1 {
            assert_eq!(names.look(format!("m{n}.example").as_bytes(), 1, &mut message), (Lookup::Found([10, 0, 0, n]), None));
        }
    }
}
