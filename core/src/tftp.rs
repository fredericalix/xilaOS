//! Fetches a file from another machine of the network (TFTP).
//!
//! It is how a machine gets a new system without a USB stick: the machine
//! that serves the kernel to those that start from the network
//! (`tools/netboot.py`) gives the same file to a XilaOS that asks for it.
//! `install from` and an address fetches it; `install system` then puts
//! it on the disk.
//!
//! The protocol is as small as protocols get. We ask for a file by name.
//! The other side sends it in numbered blocks, one message each, and waits
//! after each block for us to say its number back. A block shorter than
//! the others is the last. Before the first block the two sides may agree
//! on the size of the blocks; we ask for blocks that fill a frame.
//!
//! Nothing in it says who the other side is, nor that the file is the
//! right one. What is fetched is shown with its size and a check number
//! to compare with what the server says it gave, and nothing is written
//! to a disk before the user types `install system` at the machine.

use core::fmt::Write;

use crate::net::{Ip, Message, Net, Wire};
use crate::text::{parse_number, FixedStr};

/// The port a server waits on, and the one we ask from.
const SERVER: u16 = 69;
pub const PORT: u16 = 4069;
/// The name of the system, as a firmware asks for it.
pub const NAME: &[u8] = b"BOOTX64.EFI";
/// The block we ask for: what fits a frame with room to spare.
const ASKED: usize = 1400;
/// Ticks without news before the last message is sent again, and how
/// many times it is.
const PATIENCE: u64 = 100;
const TRIES: u32 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failed {
    /// This machine has no address, or no way to the other.
    NoWay,
    /// Nothing came, or nothing more.
    Silent,
    /// The other side said no: it has no such file.
    Refused,
    /// More came than there is room for.
    TooLarge,
    /// What came is not a system for this machine.
    NotASystem,
    /// No memory to keep it in.
    NoMemory,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Going,
    /// The whole file is there: so many bytes.
    Done(usize),
    Failed(Failed),
}

pub struct Fetch {
    server: Ip,
    /// The port the server answers from: it picks one for each file.
    port: Option<u16>,
    block: usize,
    /// The number of the block that must come next.
    next: u16,
    len: usize,
    /// When something last came, and how often we asked again since.
    heard: u64,
    tries: u32,
    /// The last message we sent, to send it again.
    last: FixedStr<64>,
    step: Step,
}

impl Fetch {
    /// Asks `server` for the system.
    pub fn start(server: Ip, now: u64, net: &mut Net, wire: &mut dyn Wire) -> Fetch {
        let mut ask = FixedStr::new();
        ask.push_bytes(&[0, 1]);
        ask.push_bytes(NAME);
        ask.push_bytes(b"\0octet\0blksize\0");
        let _ = write!(ask, "{ASKED}");
        ask.push_bytes(b"\0tsize\x000\0");
        let mut fetch = Fetch { server, port: None, block: 512, next: 1, len: 0, heard: now, tries: 0, last: ask, step: Step::Going };
        if !net.message(server, SERVER, PORT, ask.bytes(), now, wire) {
            fetch.step = Step::Failed(Failed::NoWay);
        }
        fetch
    }

    fn say(&mut self, bytes: &[u8], now: u64, net: &mut Net, wire: &mut dyn Wire) {
        self.last.clear();
        self.last.push_bytes(bytes);
        net.message(self.server, self.port.unwrap_or(SERVER), PORT, bytes, now, wire);
    }

    /// Takes a message that came to our port. The bytes of the file go
    /// into `into`.
    pub fn message(&mut self, message: &Message, into: &mut [u8], now: u64, net: &mut Net, wire: &mut dyn Wire) -> Step {
        if self.step != Step::Going || message.from != self.server || message.port != PORT || message.bytes.len() < 4 {
            return self.step;
        }
        // the first answer says which port the server talks from; then only that one
        if self.port.is_some_and(|port| port != message.from_port) {
            return self.step;
        }
        let (kind, number) = (u16::from_be_bytes([message.bytes[0], message.bytes[1]]), u16::from_be_bytes([message.bytes[2], message.bytes[3]]));
        match kind {
            // what the server agrees to, before any block
            6 if self.next == 1 && self.len == 0 => {
                let mut words = message.bytes[2..].split(|byte| *byte == 0);
                while let (Some(name), Some(value)) = (words.next(), words.next()) {
                    if name.eq_ignore_ascii_case(b"blksize") {
                        match parse_number(value) {
                            Some(block) if (8..=ASKED).contains(&block) => self.block = block,
                            _ => return self.fail(Failed::Refused),
                        }
                    }
                    if name.eq_ignore_ascii_case(b"tsize") && parse_number(value).map_or(true, |size| size > into.len()) {
                        return self.fail(Failed::TooLarge);
                    }
                }
                self.heard(message.from_port, now);
                self.say(&[0, 4, 0, 0], now, net, wire);
            }
            // a block
            3 => {
                let bytes = &message.bytes[4..];
                if number == self.next {
                    if bytes.len() > self.block || self.len + bytes.len() > into.len() {
                        return self.fail(Failed::TooLarge);
                    }
                    into[self.len..self.len + bytes.len()].copy_from_slice(bytes);
                    self.len += bytes.len();
                    self.next = self.next.wrapping_add(1);
                    self.heard(message.from_port, now);
                    self.say(&[0, 4, message.bytes[2], message.bytes[3]], now, net, wire);
                    if bytes.len() < self.block {
                        self.step = Step::Done(self.len);
                    }
                } else if number == self.next.wrapping_sub(1) && self.port.is_some() {
                    // the one before, again: our word for it was lost
                    self.heard(message.from_port, now);
                    self.say(&[0, 4, message.bytes[2], message.bytes[3]], now, net, wire);
                }
            }
            5 => return self.fail(Failed::Refused),
            _ => {}
        }
        self.step
    }

    fn heard(&mut self, port: u16, now: u64) {
        (self.port, self.heard, self.tries) = (Some(port), now, 0);
    }

    fn fail(&mut self, why: Failed) -> Step {
        self.step = Step::Failed(why);
        self.step
    }

    /// Lets the time pass: what got no answer is sent again, a few times.
    pub fn tick(&mut self, now: u64, net: &mut Net, wire: &mut dyn Wire) -> Step {
        if self.step == Step::Going && now >= self.heard + PATIENCE {
            if self.tries == TRIES {
                return self.fail(Failed::Silent);
            }
            self.tries += 1;
            self.heard = now;
            let last = self.last;
            net.message(self.server, self.port.unwrap_or(SERVER), PORT, last.bytes(), now, wire);
        }
        self.step
    }
}

/// Whether a file is a program a PC's firmware starts on a 64-bit
/// processor, which is what a XilaOS system is. It says nothing of who
/// made it.
pub fn is_system(file: &[u8]) -> bool {
    let word = |at: usize| file.get(at..at + 2).map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]));
    if file.len() < 0x40 || &file[..2] != b"MZ" {
        return false;
    }
    let at = u32::from_le_bytes([file[0x3c], file[0x3d], file[0x3e], file[0x3f]]) as usize;
    // "PE", for an x86-64 processor, with 64-bit addresses, for the firmware to run
    file.get(at..at + 4) == Some(b"PE\0\0") && word(at + 4) == Some(0x8664) && word(at + 24) == Some(0x20b) && word(at + 24 + 68) == Some(10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    const OURS: Ip = [192, 168, 1, 11];
    const THEIRS: Ip = [192, 168, 1, 168];

    /// A wire that keeps what is given to it.
    struct Kept(Vec<Vec<u8>>);

    impl Wire for Kept {
        fn send(&mut self, frame: &[u8]) {
            self.0.push(frame.to_vec());
        }
    }

    /// A network with an address, that knows the card of the server.
    fn network(wire: &mut Kept) -> Net {
        let mut net = Net::new();
        net.start([2, 0, 0, 0, 0, 1], 0);
        (net.ip, net.mask) = (Some(OURS), [255, 255, 255, 0]);
        // a message from the server teaches its card
        let mut frame = std::vec![0u8; 42 + 8];
        frame[..6].copy_from_slice(&[2, 0, 0, 0, 0, 1]);
        frame[6..12].copy_from_slice(&[2, 0, 0, 0, 0, 2]);
        frame[12..14].copy_from_slice(&[8, 0]);
        frame[14..34].copy_from_slice(&[0x45, 0, 0, 36, 0, 0, 0, 0, 64, 17, 0, 0, THEIRS[0], THEIRS[1], THEIRS[2], THEIRS[3], OURS[0], OURS[1], OURS[2], OURS[3]]);
        let mut sum = 0u32;
        for pair in frame[14..34].chunks(2) {
            sum += u16::from_be_bytes([pair[0], pair[1]]) as u32;
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        frame[24..26].copy_from_slice(&(!(sum as u16)).to_be_bytes());
        frame[34..42].copy_from_slice(&[0, 9, 0, 9, 0, 16, 0, 0]);
        assert!(net.receive(&frame, 0, wire).is_some());
        net
    }

    /// What we sent last: the port it went to, and its bytes.
    fn sent(wire: &Kept) -> (u16, Vec<u8>) {
        let frame = wire.0.last().unwrap();
        assert_eq!((&frame[30..34], &frame[34..36]), (&THEIRS[..], &PORT.to_be_bytes()[..]));
        // a short frame is filled up to the least a cable carries: the message says its own length
        let length = u16::from_be_bytes([frame[38], frame[39]]) as usize;
        (u16::from_be_bytes([frame[36], frame[37]]), frame[42..34 + length].to_vec())
    }

    fn from(port: u16, bytes: &[u8]) -> Message<'_> {
        Message { from: THEIRS, from_port: port, port: PORT, bytes }
    }

    fn block(number: u16, bytes: &[u8]) -> Vec<u8> {
        let mut message = std::vec![0, 3, (number >> 8) as u8, number as u8];
        message.extend_from_slice(bytes);
        message
    }

    #[test]
    fn a_file_is_fetched_block_by_block() {
        let mut wire = Kept(Vec::new());
        let mut net = network(&mut wire);
        let file: Vec<u8> = (0..3000u32).map(|at| (at * 7) as u8).collect();
        let mut into = [0u8; 4096];
        let mut fetch = Fetch::start(THEIRS, 10, &mut net, &mut wire);
        assert_eq!(sent(&wire), (69, b"\0\x01BOOTX64.EFI\0octet\0blksize\x001400\0tsize\x000\0".to_vec()));
        // the server agrees to smaller blocks than we asked for, from a port of its own
        assert_eq!(fetch.message(&from(5000, b"\0\x06blksize\x001000\0tsize\x003000\0"), &mut into, 11, &mut net, &mut wire), Step::Going);
        assert_eq!(sent(&wire), (5000, std::vec![0, 4, 0, 0]));
        for (number, part) in file.chunks(1000).enumerate() {
            let step = fetch.message(&from(5000, &block(number as u16 + 1, part)), &mut into, 12, &mut net, &mut wire);
            assert_eq!(sent(&wire), (5000, std::vec![0, 4, 0, number as u8 + 1]));
            // three full blocks: the file may go on
            assert_eq!(step, Step::Going);
        }
        // an empty block ends a file that is a whole number of blocks
        assert_eq!(fetch.message(&from(5000, &block(4, b"")), &mut into, 13, &mut net, &mut wire), Step::Done(3000));
        assert_eq!(sent(&wire), (5000, std::vec![0, 4, 0, 4]));
        assert_eq!(&into[..3000], &file[..]);
        // nothing more is taken
        assert_eq!(fetch.message(&from(5000, &block(5, b"more")), &mut into, 14, &mut net, &mut wire), Step::Done(3000));
    }

    #[test]
    fn a_server_that_agrees_to_nothing_sends_blocks_of_512() {
        let mut wire = Kept(Vec::new());
        let mut net = network(&mut wire);
        let mut into = [0u8; 2048];
        let mut fetch = Fetch::start(THEIRS, 0, &mut net, &mut wire);
        assert_eq!(fetch.message(&from(6000, &block(1, &[7; 512])), &mut into, 1, &mut net, &mut wire), Step::Going);
        // the same block again, because our word was lost: said again, not kept twice
        let before = wire.0.len();
        assert_eq!(fetch.message(&from(6000, &block(1, &[7; 512])), &mut into, 2, &mut net, &mut wire), Step::Going);
        assert_eq!((wire.0.len(), sent(&wire)), (before + 1, (6000, std::vec![0, 4, 0, 1])));
        // a message from another port, or a block out of turn, changes nothing
        assert_eq!(fetch.message(&from(6001, &block(2, b"no")), &mut into, 3, &mut net, &mut wire), Step::Going);
        assert_eq!(fetch.message(&from(6000, &block(3, b"no")), &mut into, 3, &mut net, &mut wire), Step::Going);
        assert_eq!(fetch.message(&from(6000, &block(2, &[9; 100])), &mut into, 4, &mut net, &mut wire), Step::Done(612));
        assert!(into[..512].iter().all(|byte| *byte == 7) && into[512..612].iter().all(|byte| *byte == 9));
    }

    #[test]
    fn what_fails_says_why() {
        let mut wire = Kept(Vec::new());
        let mut net = network(&mut wire);
        let mut into = [0u8; 1024];
        // the server has no such file
        let mut fetch = Fetch::start(THEIRS, 0, &mut net, &mut wire);
        assert_eq!(fetch.message(&from(7000, b"\0\x05\0\x01No such file\0"), &mut into, 1, &mut net, &mut wire), Step::Failed(Failed::Refused));
        // the file is larger than the room for it: said at once, or found on the way
        let mut fetch = Fetch::start(THEIRS, 0, &mut net, &mut wire);
        assert_eq!(fetch.message(&from(7000, b"\0\x06tsize\x002000\0"), &mut into, 1, &mut net, &mut wire), Step::Failed(Failed::TooLarge));
        let mut fetch = Fetch::start(THEIRS, 0, &mut net, &mut wire);
        for number in 1..=2 {
            assert_eq!(fetch.message(&from(7000, &block(number, &[1; 512])), &mut into, 1, &mut net, &mut wire), Step::Going);
        }
        assert_eq!(fetch.message(&from(7000, &block(3, &[1; 512])), &mut into, 1, &mut net, &mut wire), Step::Failed(Failed::TooLarge));
        // blocks larger than agreed are not a file
        let mut fetch = Fetch::start(THEIRS, 0, &mut net, &mut wire);
        assert_eq!(fetch.message(&from(7000, &block(1, &[1; 600])), &mut into, 1, &mut net, &mut wire), Step::Failed(Failed::TooLarge));
        // a machine without an address asks nobody
        let mut alone = Net::new();
        assert_eq!(Fetch::start(THEIRS, 0, &mut alone, &mut wire).tick(1, &mut alone, &mut wire), Step::Failed(Failed::NoWay));
    }

    #[test]
    fn silence_is_asked_again_then_given_up() {
        let mut wire = Kept(Vec::new());
        let mut net = network(&mut wire);
        let mut into = [0u8; 1024];
        let mut fetch = Fetch::start(THEIRS, 0, &mut net, &mut wire);
        let asked = sent(&wire);
        let before = wire.0.len();
        assert_eq!(fetch.tick(PATIENCE - 1, &mut net, &mut wire), Step::Going);
        assert_eq!(wire.0.len(), before);
        // the question again, to the port servers wait on
        assert_eq!(fetch.tick(PATIENCE, &mut net, &mut wire), Step::Going);
        assert_eq!((wire.0.len(), sent(&wire)), (before + 1, asked));
        // once the file has started, it is our last word that is said again, to the server's port
        assert_eq!(fetch.message(&from(8000, &block(1, &[3; 512])), &mut into, PATIENCE + 5, &mut net, &mut wire), Step::Going);
        let mut now = PATIENCE + 5;
        for _ in 0..TRIES {
            now += PATIENCE;
            assert_eq!(fetch.tick(now, &mut net, &mut wire), Step::Going);
            assert_eq!(sent(&wire), (8000, std::vec![0, 4, 0, 1]));
        }
        assert_eq!(fetch.tick(now + PATIENCE, &mut net, &mut wire), Step::Failed(Failed::Silent));
    }

    #[test]
    fn a_system_is_told_from_any_other_file() {
        // the start of a program for a PC's firmware, as a compiler writes it
        let mut file = std::vec![0u8; 0x200];
        file[..2].copy_from_slice(b"MZ");
        file[0x3c] = 0x80;
        file[0x80..0x84].copy_from_slice(b"PE\0\0");
        file[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        file[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
        file[0xdc..0xde].copy_from_slice(&10u16.to_le_bytes());
        assert!(is_system(&file));
        // for another processor, for Windows, cut short, or not a program at all
        for (at, value) in [(0x84, 0xaa64u16), (0xdc, 3), (0x98, 0x10b), (0x80, 0), (0, 0)] {
            let mut other = file.clone();
            other[at..at + 2].copy_from_slice(&value.to_le_bytes());
            assert!(!is_system(&other), "{at:x}");
        }
        assert!(!is_system(&file[..0x90]) && !is_system(b"MZ") && !is_system(b""));
        let mut far = file.clone();
        far[0x3c..0x40].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
        assert!(!is_system(&far));
    }
}
