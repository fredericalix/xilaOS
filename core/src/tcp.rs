//! TCP: a conversation between two machines, in which every byte arrives,
//! once, in order.
//!
//! The network below loses packets, repeats them and delivers them out of
//! order. TCP numbers every byte. Each side says how far it has received
//! (the acknowledgement), and what was sent and not acknowledged in time
//! is sent again. A conversation opens with three packets (SYN, SYN and
//! ACK, ACK), and each side closes its half with a FIN, which is
//! acknowledged like a byte.
//!
//! Either side may open it: a program listens on a port and another
//! machine connects to it (`listen`, `accept`), or a program calls a port
//! of another machine (`connect`).
//!
//! It is a state machine with no hardware and no time of its own: `input`
//! is given the segments that arrive, `tick` the time that passes, and
//! `output` hands out the segments to send, one at a time, until it has
//! none. `Net` moves them to and from the cable.
//!
//! What it is, and is not:
//! - a few conversations at once (`CONNECTIONS`), each with a small
//!   memory for what arrives and for what is not acknowledged yet; a
//!   program that has more to say gives it as room is made;
//! - what arrives out of order is dropped and asked for again, which
//!   costs time on a bad network and nothing on a good one;
//! - no more is sent unacknowledged than the other side says it has room
//!   for, and never more than our own small memory: that is all the care
//!   taken not to flood a network, and it is enough at this size;
//! - a conversation that is silent for a minute is ended, so that a peer
//!   that vanished does not keep a place forever;
//! - after both sides have closed, the place lingers two seconds to
//!   answer a last packet that comes again, unless a new caller needs it.

use crate::net::Ip;

/// Conversations at once, and ports listened on.
pub const CONNECTIONS: usize = 8;
const PORTS: usize = 4;
/// Bytes kept: received and not read yet, and written and not
/// acknowledged yet.
const IN: usize = 1024;
const OUT: usize = 4096;
/// The most bytes in one segment: what fits a frame.
const MOST: usize = 1460;
/// The header of a segment, and with the option that says `MOST`.
const HEADER: usize = 20;
/// The longest segment `output` writes.
pub const SEGMENT: usize = HEADER + MOST;

const SECOND: u64 = 100;
/// How long an acknowledgement is waited for before sending again; the
/// wait doubles each time, up to a limit, and after `TRIES` it is over.
const PATIENCE: u64 = SECOND / 2;
const PATIENCE_MOST: u64 = 8 * SECOND;
const TRIES: u32 = 6;
/// A conversation in which nothing came for that long is ended.
const SILENCE: u64 = 60 * SECOND;
/// After both sides have closed, the place is kept a moment to answer a
/// last packet that comes again.
const LINGER: u64 = 2 * SECOND;

const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
const RST: u8 = 0x04;
const PUSH: u8 = 0x08;
const ACK: u8 = 0x10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Free,
    /// We call: our first packet is sent, their answer is awaited.
    Calling,
    /// Called: our answer is sent, theirs is awaited.
    Called,
    Open,
    /// They have closed their half; ours is still open.
    TheyClosed,
    /// We have closed ours, and wait for it to be acknowledged.
    WeClosed,
    /// Ours is closed and acknowledged; theirs is still open.
    WeClosedHeard,
    /// Both closed at the same moment; ours is not acknowledged yet.
    BothClosing,
    /// They closed, then we did; ours is not acknowledged yet.
    LastWord,
    /// All is said; the place lingers.
    Lingering,
    /// Over, but the program that had it does not know yet.
    Dead,
}

#[derive(Clone, Copy)]
struct Connection {
    state: State,
    their_ip: Ip,
    their_port: u16,
    port: u16,
    /// The number of the next byte we expect from them.
    expected: u32,
    received: [u8; IN],
    received_len: usize,
    /// They said they will send no more.
    they_ended: bool,
    /// The number of our first byte not acknowledged, and of the next to send.
    unheard: u32,
    next: u32,
    /// How many bytes they say they have room for, and their largest segment.
    their_room: usize,
    their_most: usize,
    /// What the program wrote and they have not acknowledged, from `unheard`.
    written: [u8; OUT],
    written_len: usize,
    /// The program has closed; our FIN is sent.
    closing: bool,
    closed: bool,
    answered: bool,
    /// An acknowledgement is owed, with nothing else to carry it.
    owes: bool,
    /// When to send again what is not acknowledged; 0 if nothing is.
    again: u64,
    patience: u64,
    tries: u32,
    /// One byte may go although they say they have no room: to hear from
    /// them when they have some again.
    probe: bool,
    heard: u64,
    until: u64,
    /// A program took it with `accept`, and it is done with it.
    taken: bool,
    done: bool,
}

const FREE: Connection = Connection {
    state: State::Free,
    their_ip: [0; 4],
    their_port: 0,
    port: 0,
    expected: 0,
    received: [0; IN],
    received_len: 0,
    they_ended: false,
    unheard: 0,
    next: 0,
    their_room: 0,
    their_most: 536,
    written: [0; OUT],
    written_len: 0,
    closing: false,
    closed: false,
    answered: false,
    owes: false,
    again: 0,
    patience: PATIENCE,
    tries: 0,
    probe: false,
    heard: 0,
    until: 0,
    taken: false,
    done: false,
};

impl Connection {
    /// The conversation is over: the place is free, unless a program
    /// still holds it and must learn of it first.
    fn end(&mut self) {
        self.state = if self.taken && !self.done { State::Dead } else { State::Free };
        self.again = 0;
    }

    fn live(&self) -> bool {
        !matches!(self.state, State::Free | State::Dead)
    }

    /// Whether our half is open or still has something to send again.
    fn sends(&self) -> bool {
        matches!(self.state, State::Open | State::TheyClosed | State::WeClosed | State::BothClosing | State::LastWord)
    }
}

/// A refusal to send: to a segment that belongs to no conversation.
#[derive(Clone, Copy)]
struct Refusal {
    to: Ip,
    their_port: u16,
    port: u16,
    number: u32,
    acknowledges: Option<u32>,
}

/// A conversation, for the program that has it.
pub type Id = usize;

pub struct Tcp {
    connections: [Connection; CONNECTIONS],
    ports: [u16; PORTS],
    refusal: Option<Refusal>,
    /// Where `output` looks first: each conversation in its turn.
    turn: usize,
    /// From which the number of our first byte is drawn: nobody outside
    /// should be able to guess it.
    chance: u64,
    /// The port our last call went out from.
    last_port: u16,
    /// Counted for whoever wants to know: opened, ended by silence or by
    /// the other side, and segments sent again.
    pub opened: u32,
    pub lost: u32,
    pub again: u32,
}

fn be16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The checksum of a segment: over it and over the two addresses, the
/// kind of packet and the length, as if they were written before it.
fn checksum(from: Ip, to: Ip, segment: &[u8]) -> u16 {
    let mut sum = 6 + segment.len() as u32;
    for bytes in [&from[..], &to[..], segment] {
        for pair in bytes.chunks(2) {
            sum += (pair[0] as u32) << 8 | pair.get(1).copied().unwrap_or(0) as u32;
        }
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// The largest segment the other side takes, as the packet that opens a
/// conversation says it in its options; a small one if it says nothing.
fn their_most(segment: &[u8], header: usize) -> usize {
    let mut most = 536;
    let mut at = HEADER;
    while at + 1 < header {
        match segment[at] {
            0 => break,
            1 => at += 1,
            2 if segment[at + 1] == 4 && at + 4 <= header => {
                most = be16(segment, at + 2) as usize;
                at += 4;
            }
            _ => at += (segment[at + 1] as usize).max(2),
        }
    }
    most.clamp(64, MOST)
}

impl Tcp {
    pub const fn new() -> Tcp {
        Tcp { connections: [FREE; CONNECTIONS], ports: [0; PORTS], refusal: None, turn: 0, chance: 0x9e37_79b9_7f4a_7c15, last_port: 0, opened: 0, lost: 0, again: 0 }
    }

    /// Mixes in a number nobody outside can guess.
    pub fn stir(&mut self, chance: u64) {
        self.chance = self.chance.rotate_left(13) ^ chance;
    }

    /// Starts taking calls on a port. False if no more ports can be.
    pub fn listen(&mut self, port: u16) -> bool {
        if port == 0 {
            return false;
        }
        if self.ports.contains(&port) {
            return true;
        }
        match self.ports.iter_mut().find(|slot| **slot == 0) {
            Some(slot) => {
                *slot = port;
                true
            }
            None => false,
        }
    }

    /// Stops taking calls on a port. The conversations no program had
    /// taken are refused; the others go on.
    pub fn unlisten(&mut self, port: u16) {
        for slot in &mut self.ports {
            if *slot == port {
                *slot = 0;
            }
        }
        for id in 0..CONNECTIONS {
            if self.connections[id].port == port && self.connections[id].live() && !self.connections[id].taken {
                self.abort(id);
            }
        }
    }

    pub fn listens(&self, port: u16) -> bool {
        port != 0 && self.ports.contains(&port)
    }

    /// The number of our first byte in a new conversation.
    fn first(&mut self, with: u64) -> u32 {
        self.stir(with);
        ((self.chance ^ self.chance >> 29).wrapping_mul(0xbf58_476d_1ce4_e5b9) >> 32) as u32
    }

    /// A place for a new conversation: a free one, or one that only
    /// lingers after all was said. A new conversation matters more than
    /// a last packet that may come again.
    fn place(&self) -> Option<Id> {
        let free = self.connections.iter().position(|c| c.state == State::Free);
        free.or_else(|| self.connections.iter().position(|c| c.state == State::Lingering))
    }

    /// Calls a port of another machine. None if there is no place for
    /// one more conversation. The call is not answered yet: `connected`
    /// says when it is, and `alive` turns false if it never is.
    pub fn connect(&mut self, to: Ip, port: u16, now: u64) -> Option<Id> {
        let id = self.place()?;
        // from a port of ours that no conversation with them uses: one of
        // those kept for calls, the next each time
        let mut ours = self.last_port;
        loop {
            ours = if (49152..65535).contains(&ours) { ours + 1 } else { 49152 + (self.first(now) & 0x1fff) as u16 };
            if !self.connections.iter().any(|c| c.live() && c.port == ours) {
                break;
            }
        }
        self.last_port = ours;
        let first = self.first(now ^ (port as u64) << 32 ^ u32::from_be_bytes(to) as u64);
        self.connections[id] = Connection { state: State::Calling, their_ip: to, their_port: port, port: ours, unheard: first, next: first, heard: now, taken: true, ..FREE };
        Some(id)
    }

    /// Whether the other side has answered a call, or made one that was
    /// answered: the two can talk.
    pub fn connected(&self, id: Id) -> bool {
        let c = &self.connections[id];
        c.live() && !matches!(c.state, State::Calling | State::Called)
    }

    /// A conversation that opened on a port and that no program has yet.
    pub fn accept(&mut self, port: u16) -> Option<Id> {
        let id = self.connections.iter().position(|c| c.port == port && !c.taken && matches!(c.state, State::Open | State::TheyClosed))?;
        self.connections[id].taken = true;
        Some(id)
    }

    /// Whether the conversation still goes on. If not, `close` frees it.
    pub fn alive(&self, id: Id) -> bool {
        self.connections[id].live()
    }

    /// Who is at the other end.
    pub fn peer(&self, id: Id) -> (Ip, u16) {
        (self.connections[id].their_ip, self.connections[id].their_port)
    }

    /// Takes what has arrived, as much as fits. Returns how many bytes.
    pub fn read(&mut self, id: Id, into: &mut [u8]) -> usize {
        let c = &mut self.connections[id];
        let count = c.received_len.min(into.len());
        into[..count].copy_from_slice(&c.received[..count]);
        c.received.copy_within(count..c.received_len, 0);
        // they were told there was no room: now there is
        if c.received_len == IN && count > 0 && c.live() {
            c.owes = true;
        }
        c.received_len -= count;
        count
    }

    /// Whether they have said all they had to say, and it has been read.
    pub fn ended(&self, id: Id) -> bool {
        let c = &self.connections[id];
        c.received_len == 0 && (c.they_ended || !c.live())
    }

    /// How many bytes `write` would take now.
    pub fn room(&self, id: Id) -> usize {
        let c = &self.connections[id];
        if matches!(c.state, State::Open | State::TheyClosed) && !c.closing {
            OUT - c.written_len
        } else {
            0
        }
    }

    /// Gives bytes to send. Returns how many were taken: those that fit.
    pub fn write(&mut self, id: Id, bytes: &[u8]) -> usize {
        let count = self.room(id).min(bytes.len());
        let c = &mut self.connections[id];
        c.written[c.written_len..c.written_len + count].copy_from_slice(&bytes[..count]);
        c.written_len += count;
        count
    }

    /// The program is done: what it wrote is still sent, then our half is
    /// closed, and the place is freed when the other side has closed too.
    /// The id must not be used after this.
    pub fn close(&mut self, id: Id) {
        let c = &mut self.connections[id];
        c.done = true;
        match c.state {
            State::Dead => c.state = State::Free,
            State::Free => {}
            _ => c.closing = true,
        }
    }

    /// Ends a conversation at once, and tells the other side.
    pub fn abort(&mut self, id: Id) {
        let c = &mut self.connections[id];
        if c.live() {
            self.refusal = Some(Refusal { to: c.their_ip, their_port: c.their_port, port: c.port, number: c.next, acknowledges: None });
        }
        c.state = State::Free;
        c.again = 0;
    }

    /// How many conversations are going on.
    pub fn count(&self) -> usize {
        self.connections.iter().filter(|c| c.live()).count()
    }

    /// Takes a segment that came from `from` for our address `to`.
    pub fn input(&mut self, from: Ip, to: Ip, segment: &[u8], now: u64) {
        if segment.len() < HEADER || checksum(from, to, segment) != 0 {
            return;
        }
        let (their_port, port, number, acknowledged, flags, room) = (be16(segment, 0), be16(segment, 2), be32(segment, 4), be32(segment, 8), segment[13], be16(segment, 14) as usize);
        let header = (segment[12] >> 4) as usize * 4;
        if header < HEADER || header > segment.len() {
            return;
        }
        let bytes = &segment[header..];
        let found = self.connections.iter().position(|c| c.live() && c.their_ip == from && c.their_port == their_port && c.port == port);
        let Some(id) = found else {
            if flags & RST != 0 {
                return;
            }
            if flags & (SYN | ACK) == SYN && self.listens(port) {
                // a call: a place for it, if there is one; if not, they will call again
                if let Some(id) = self.place() {
                    let first = self.first(now ^ (number as u64) << 20 ^ (their_port as u64) << 48);
                    self.connections[id] = Connection {
                        state: State::Called,
                        their_ip: from,
                        their_port,
                        port,
                        expected: number.wrapping_add(1),
                        unheard: first,
                        next: first,
                        their_room: room,
                        their_most: their_most(segment, header),
                        heard: now,
                        ..FREE
                    };
                }
                return;
            }
            // nobody here for that: say so, in the terms of what came
            let length = bytes.len() as u32 + (flags & SYN != 0) as u32 + (flags & FIN != 0) as u32;
            self.refusal = Some(if flags & ACK != 0 {
                Refusal { to: from, their_port, port, number: acknowledged, acknowledges: None }
            } else {
                Refusal { to: from, their_port, port, number: 0, acknowledges: Some(number.wrapping_add(length)) }
            });
            return;
        };

        let c = &mut self.connections[id];
        c.heard = now;
        if c.state == State::Calling {
            // only an answer to our call counts: one that acknowledges its number
            let ours = c.answered && flags & ACK != 0 && acknowledged == c.unheard.wrapping_add(1);
            if flags & RST != 0 {
                if ours {
                    // nobody listens there
                    c.end();
                    self.lost += 1;
                }
            } else if !ours {
                if flags & ACK != 0 {
                    self.refusal = Some(Refusal { to: from, their_port, port, number: acknowledged, acknowledges: None });
                }
            } else if flags & SYN != 0 {
                // yes: their first number, and ours acknowledged
                c.state = State::Open;
                c.expected = number.wrapping_add(1);
                c.unheard = acknowledged;
                (c.again, c.tries, c.patience) = (0, 0, PATIENCE);
                (c.their_room, c.their_most) = (room, their_most(segment, header));
                c.owes = true;
                self.opened += 1;
            }
            return;
        }
        if flags & RST != 0 {
            // only a refusal that knows where the conversation is counts
            if number == c.expected {
                // after all was said it loses nothing: some close that way
                if c.state != State::Lingering {
                    self.lost += 1;
                }
                c.end();
            }
            return;
        }
        if flags & SYN != 0 {
            // their call again: our answer was lost
            if c.state == State::Called && number.wrapping_add(1) == c.expected {
                c.answered = false;
            } else {
                c.owes = true;
            }
            return;
        }
        if flags & ACK == 0 {
            return;
        }
        let (heard, sent) = (acknowledged.wrapping_sub(c.unheard), c.next.wrapping_sub(c.unheard));
        if c.state == State::Called {
            if !c.answered || heard != 1 {
                self.refusal = Some(Refusal { to: from, their_port, port, number: acknowledged, acknowledges: None });
                return;
            }
            c.state = State::Open;
            c.unheard = acknowledged;
            (c.again, c.tries, c.patience) = (0, 0, PATIENCE);
            c.their_room = room;
            self.opened += 1;
        } else if heard <= sent {
            if heard > 0 {
                // so many of our bytes arrived; one more than we wrote is our FIN
                let bytes_heard = (heard as usize).min(c.written_len);
                c.written.copy_within(bytes_heard..c.written_len, 0);
                c.written_len -= bytes_heard;
                c.unheard = acknowledged;
                (c.tries, c.patience, c.probe) = (0, PATIENCE, false);
                c.again = if c.next != c.unheard { now + c.patience } else { 0 };
                if c.closed && heard as usize > bytes_heard {
                    match c.state {
                        State::WeClosed => c.state = State::WeClosedHeard,
                        State::BothClosing => (c.state, c.until) = (State::Lingering, now + LINGER),
                        State::LastWord => {
                            c.end();
                            return;
                        }
                        _ => {}
                    }
                }
            }
            c.their_room = room;
        }

        let hears = matches!(c.state, State::Open | State::WeClosed | State::WeClosedHeard);
        if number == c.expected && hears {
            let count = bytes.len().min(IN - c.received_len);
            c.received[c.received_len..c.received_len + count].copy_from_slice(&bytes[..count]);
            c.received_len += count;
            c.expected = c.expected.wrapping_add(count as u32);
            if !bytes.is_empty() {
                c.owes = true;
            }
            if flags & FIN != 0 && count == bytes.len() {
                c.expected = c.expected.wrapping_add(1);
                (c.they_ended, c.owes) = (true, true);
                match c.state {
                    State::Open => c.state = State::TheyClosed,
                    State::WeClosed => c.state = State::BothClosing,
                    _ => (c.state, c.until) = (State::Lingering, now + LINGER),
                }
            }
        } else if !bytes.is_empty() || flags & FIN != 0 {
            // not what comes next, or said again: they are told where we are
            c.owes = true;
        }
    }

    /// Lets the time pass: what was not acknowledged is sent again, and
    /// what has been silent too long is ended.
    pub fn tick(&mut self, now: u64) {
        for id in 0..CONNECTIONS {
            let c = &mut self.connections[id];
            if !c.live() {
                continue;
            }
            if now >= c.heard + SILENCE {
                self.lost += 1;
                let taken = c.taken && !c.done;
                self.abort(id);
                if taken {
                    self.connections[id].state = State::Dead;
                    self.connections[id].taken = true;
                }
                continue;
            }
            if c.state == State::Lingering {
                if now >= c.until {
                    c.end();
                }
                continue;
            }
            // they have no room and nothing is on its way: a byte will ask them
            if c.sends() && c.again == 0 && c.their_room == 0 && c.written_len > 0 {
                c.again = now + c.patience;
            }
            if c.again != 0 && now >= c.again {
                if c.tries == TRIES {
                    self.lost += 1;
                    c.end();
                    continue;
                }
                c.tries += 1;
                c.patience = (c.patience * 2).min(PATIENCE_MOST);
                c.again = now + c.patience;
                self.again += 1;
                // from the first byte not acknowledged, everything again
                if matches!(c.state, State::Called | State::Calling) {
                    c.answered = false;
                } else {
                    c.next = c.unheard;
                    c.closed = false;
                    c.probe = c.their_room == 0;
                }
            }
        }
    }

    /// Writes the start of a segment. Returns the length of its header.
    fn header(into: &mut [u8], c: &Connection, number: u32, flags: u8, most: bool) -> usize {
        let length = if most { HEADER + 4 } else { HEADER };
        into[..2].copy_from_slice(&c.port.to_be_bytes());
        into[2..4].copy_from_slice(&c.their_port.to_be_bytes());
        into[4..8].copy_from_slice(&number.to_be_bytes());
        into[8..12].copy_from_slice(&c.expected.to_be_bytes());
        into[12] = (length as u8 / 4) << 4;
        into[13] = flags;
        into[14..16].copy_from_slice(&((IN - c.received_len) as u16).to_be_bytes());
        into[16..20].fill(0);
        if most {
            // the largest segment we take
            into[20..24].copy_from_slice(&[2, 4, (MOST >> 8) as u8, MOST as u8]);
        }
        length
    }

    /// The next segment to send, written into `into`, from our address
    /// `ours`: who it is for and how long it is. None when there is
    /// nothing to send now. Call it until it says so.
    pub fn output(&mut self, ours: Ip, now: u64, into: &mut [u8; SEGMENT]) -> Option<(Ip, usize)> {
        if let Some(refusal) = self.refusal.take() {
            let from = Connection { port: refusal.port, their_port: refusal.their_port, expected: refusal.acknowledges.unwrap_or(0), received_len: IN, ..FREE };
            let flags = if refusal.acknowledges.is_some() { RST | ACK } else { RST };
            let length = Self::header(into, &from, refusal.number, flags, false);
            let sum = checksum(ours, refusal.to, &into[..length]);
            into[16..18].copy_from_slice(&sum.to_be_bytes());
            return Some((refusal.to, length));
        }
        for step in 0..CONNECTIONS {
            let id = (self.turn + step) % CONNECTIONS;
            let c = &mut self.connections[id];
            if !c.live() {
                continue;
            }
            let length = if matches!(c.state, State::Called | State::Calling) {
                if c.answered {
                    continue;
                }
                // our first number: with theirs acknowledged if they called
                c.answered = true;
                c.next = c.unheard.wrapping_add(1);
                if c.again == 0 {
                    c.again = now + c.patience;
                }
                Self::header(into, c, c.unheard, if c.state == State::Called { SYN | ACK } else { SYN }, true)
            } else {
                let sent = c.next.wrapping_sub(c.unheard) as usize;
                let sent_bytes = sent.min(c.written_len);
                let room = if c.probe { c.their_room.max(1) } else { c.their_room };
                let count = (c.written_len - sent_bytes).min(c.their_most).min(room.saturating_sub(sent_bytes));
                if c.sends() && count > 0 {
                    // the next bytes, as many as fit and as they have room for
                    let last = sent_bytes + count == c.written_len;
                    let header = Self::header(into, c, c.unheard.wrapping_add(sent_bytes as u32), if last { ACK | PUSH } else { ACK }, false);
                    into[header..header + count].copy_from_slice(&c.written[sent_bytes..sent_bytes + count]);
                    c.next = c.next.wrapping_add(count as u32);
                    c.probe = false;
                    header + count
                } else if c.sends() && c.closing && !c.closed && sent_bytes == c.written_len {
                    // all is sent: our half closes
                    c.closed = true;
                    let header = Self::header(into, c, c.next, FIN | ACK, false);
                    c.next = c.next.wrapping_add(1);
                    match c.state {
                        State::Open => c.state = State::WeClosed,
                        State::TheyClosed => c.state = State::LastWord,
                        _ => {}
                    }
                    header
                } else if c.owes {
                    Self::header(into, c, c.next, ACK, false)
                } else {
                    continue;
                }
            };
            c.owes = false;
            if c.next != c.unheard && c.again == 0 {
                c.again = now + c.patience;
            }
            let to = c.their_ip;
            let sum = checksum(ours, to, &into[..length]);
            into[16..18].copy_from_slice(&sum.to_be_bytes());
            // the next one first, next time: each in its turn
            self.turn = (id + 1) % CONNECTIONS;
            return Some((to, length));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    const OURS: Ip = [192, 168, 1, 11];
    const THEIRS: Ip = [192, 168, 1, 168];

    /// The other side of a conversation, played by the test.
    struct Peer {
        port: u16,
        to: u16,
        /// The number of its next byte, and of the next it expects of ours.
        next: u32,
        expected: u32,
        room: u16,
    }

    /// A segment of ours, as the other side reads it.
    #[derive(Debug, PartialEq)]
    struct Got {
        flags: u8,
        number: u32,
        acknowledged: u32,
        room: u16,
        bytes: Vec<u8>,
    }

    impl Peer {
        fn new(port: u16, to: u16) -> Peer {
            Peer { port, to, next: 1000, expected: 0, room: 65535 }
        }

        fn segment(&self, flags: u8, number: u32, bytes: &[u8]) -> Vec<u8> {
            let mut segment = std::vec![0u8; HEADER];
            segment[..2].copy_from_slice(&self.port.to_be_bytes());
            segment[2..4].copy_from_slice(&self.to.to_be_bytes());
            segment[4..8].copy_from_slice(&number.to_be_bytes());
            segment[8..12].copy_from_slice(&self.expected.to_be_bytes());
            segment[12] = 5 << 4;
            segment[13] = flags;
            segment[14..16].copy_from_slice(&self.room.to_be_bytes());
            segment.extend_from_slice(bytes);
            let sum = checksum(THEIRS, OURS, &segment);
            segment[16..18].copy_from_slice(&sum.to_be_bytes());
            segment
        }

        /// Sends a segment with its next number, and counts what it sent.
        fn say(&mut self, tcp: &mut Tcp, flags: u8, bytes: &[u8], now: u64) {
            let segment = self.segment(flags, self.next, bytes);
            tcp.input(THEIRS, OURS, &segment, now);
            self.next = self.next.wrapping_add(bytes.len() as u32 + (flags & (SYN | FIN) != 0) as u32);
        }

        /// Reads every segment we have to send, and acknowledges in its
        /// head what is for it and in order.
        fn hear(&mut self, tcp: &mut Tcp, now: u64) -> Vec<Got> {
            let mut all = Vec::new();
            let mut segment = [0u8; SEGMENT];
            while let Some((to, length)) = tcp.output(OURS, now, &mut segment) {
                assert_eq!(to, THEIRS);
                let segment = &segment[..length];
                assert_eq!(checksum(OURS, THEIRS, segment), 0, "the checksum of what we send");
                if be16(segment, 2) != self.port {
                    continue;
                }
                let header = (segment[12] >> 4) as usize * 4;
                let got = Got { flags: segment[13], number: be32(segment, 4), acknowledged: be32(segment, 8), room: be16(segment, 14), bytes: segment[header..].to_vec() };
                if got.flags & SYN != 0 {
                    self.expected = got.number.wrapping_add(1);
                } else if got.number == self.expected {
                    self.expected = self.expected.wrapping_add(got.bytes.len() as u32 + (got.flags & FIN != 0) as u32);
                }
                all.push(got);
                assert!(all.len() < 100, "it never stops sending");
            }
            all
        }

        /// Calls, and is answered: the three packets that open a conversation.
        fn call(tcp: &mut Tcp, port: u16, to: u16, now: u64) -> Peer {
            let mut peer = Peer::new(port, to);
            peer.say(tcp, SYN, b"", now);
            let got = peer.hear(tcp, now);
            assert_eq!(got.len(), 1);
            assert_eq!((got[0].flags, got[0].acknowledged, got[0].room as usize), (SYN | ACK, 1001, IN));
            // it says the largest segment it takes
            peer.say(tcp, ACK, b"", now);
            assert!(peer.hear(tcp, now).is_empty());
            peer
        }
    }

    fn listening() -> Tcp {
        let mut tcp = Tcp::new();
        assert!(tcp.listen(80));
        tcp
    }

    #[test]
    fn a_conversation_opens_says_things_and_closes() {
        let mut tcp = listening();
        assert_eq!(tcp.accept(80), None);
        let mut peer = Peer::call(&mut tcp, 50000, 80, 10);
        let id = tcp.accept(80).unwrap();
        assert_eq!(tcp.accept(80), None);
        assert_eq!((tcp.peer(id), tcp.alive(id), tcp.count(), tcp.opened), ((THEIRS, 50000), true, 1, 1));

        // they ask; it is acknowledged at once and read by the program
        peer.say(&mut tcp, ACK | PUSH, b"GET / HTTP/1.0\r\n\r\n", 11);
        let got = peer.hear(&mut tcp, 11);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].flags, got[0].acknowledged, got[0].room as usize, got[0].bytes.len()), (ACK, peer.next, IN - 18, 0));
        let mut asked = [0u8; 64];
        let count = tcp.read(id, &mut asked);
        assert_eq!(&asked[..count], b"GET / HTTP/1.0\r\n\r\n");
        assert!(!tcp.ended(id));

        // we answer with more than one segment holds, and close
        let page: Vec<u8> = (0..3000u32).map(|at| (at % 251) as u8).collect();
        assert_eq!(tcp.room(id), OUT);
        assert_eq!(tcp.write(id, &page), 3000);
        tcp.close(id);
        let got = peer.hear(&mut tcp, 12);
        assert_eq!(got.iter().map(|g| (g.flags, g.bytes.len())).collect::<Vec<_>>(), [(ACK, 536), (ACK, 536), (ACK, 536), (ACK, 536), (ACK, 536), (ACK | PUSH, 320), (FIN | ACK, 0)]);
        let sent: Vec<u8> = got.iter().flat_map(|g| g.bytes.clone()).collect();
        assert_eq!(sent, page);
        // nothing more until they acknowledge
        assert!(peer.hear(&mut tcp, 13).is_empty());

        // they acknowledge everything and close their half: the place is free after a moment
        peer.say(&mut tcp, FIN | ACK, b"", 14);
        let got = peer.hear(&mut tcp, 14);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].flags, got[0].acknowledged), (ACK, peer.next));
        assert_eq!(tcp.count(), 1);
        tcp.tick(14 + LINGER);
        assert_eq!(tcp.count(), 0);
        assert!(peer.hear(&mut tcp, 300).is_empty());
    }

    #[test]
    fn the_largest_segment_is_the_one_they_say() {
        let mut tcp = listening();
        let mut peer = Peer::new(50001, 80);
        // their call says they take segments of 1460 bytes
        let mut call = peer.segment(SYN, peer.next, b"");
        call[12] = 6 << 4;
        call.extend_from_slice(&[2, 4, 0x05, 0xb4]);
        call[16..18].fill(0);
        let sum = checksum(THEIRS, OURS, &call);
        call[16..18].copy_from_slice(&sum.to_be_bytes());
        tcp.input(THEIRS, OURS, &call, 1);
        peer.next += 1;
        let mut segment = [0u8; SEGMENT];
        let (_, length) = tcp.output(OURS, 1, &mut segment).unwrap();
        // our answer says the same of us
        assert_eq!((length, &segment[20..24]), (24, &[2u8, 4, 0x05, 0xb4][..]));
        peer.expected = be32(&segment, 4) + 1;
        peer.say(&mut tcp, ACK, b"", 1);
        let id = tcp.accept(80).unwrap();
        assert_eq!(tcp.write(id, &[7; 4000]), 4000);
        let got = peer.hear(&mut tcp, 2);
        assert_eq!(got.iter().map(|g| g.bytes.len()).collect::<Vec<_>>(), [1460, 1460, 1080]);
    }

    #[test]
    fn what_is_not_acknowledged_is_sent_again() {
        let mut tcp = listening();
        let mut peer = Peer::call(&mut tcp, 50002, 80, 0);
        let id = tcp.accept(80).unwrap();
        tcp.write(id, b"hello");
        let first = peer.hear(&mut tcp, 0);
        assert_eq!(first[0].bytes, b"hello");
        // lost on the way: the peer never saw it
        peer.expected = first[0].number;
        tcp.tick(PATIENCE - 1);
        assert!(peer.hear(&mut tcp, PATIENCE - 1).is_empty());
        tcp.tick(PATIENCE);
        let again = peer.hear(&mut tcp, PATIENCE);
        assert_eq!((again.len(), &again[0].bytes[..], again[0].number, tcp.again), (1, &b"hello"[..], first[0].number, 1));
        // the wait doubles
        peer.expected = first[0].number;
        tcp.tick(PATIENCE * 2);
        assert!(peer.hear(&mut tcp, PATIENCE * 2).is_empty());
        tcp.tick(PATIENCE * 3);
        assert_eq!(peer.hear(&mut tcp, PATIENCE * 3).len(), 1);
        // acknowledged at last: nothing more is sent, ever
        peer.say(&mut tcp, ACK, b"", PATIENCE * 3 + 1);
        tcp.tick(PATIENCE * 100 / 2);
        assert!(peer.hear(&mut tcp, PATIENCE * 50).is_empty() && tcp.alive(id));
        assert_eq!(tcp.room(id), OUT);
    }

    #[test]
    fn a_peer_that_stops_answering_is_given_up() {
        let mut tcp = listening();
        let mut peer = Peer::call(&mut tcp, 50003, 80, 0);
        let id = tcp.accept(80).unwrap();
        tcp.write(id, b"anyone?");
        peer.hear(&mut tcp, 0);
        let mut now = 0;
        let mut times = 0;
        while tcp.alive(id) {
            now += 10;
            assert!(now < SILENCE, "it never gives up");
            tcp.tick(now);
            times += peer.hear(&mut tcp, now).len();
        }
        assert_eq!((times, tcp.lost), (TRIES as usize, 1));
        // the program learns of it, and frees the place
        assert!(tcp.ended(id) && tcp.room(id) == 0 && tcp.count() == 0);
        assert_eq!(tcp.accept(80), None);
        tcp.close(id);
        // a call that was never answered to the end is given up too
        let mut silent = Peer::new(50004, 80);
        silent.say(&mut tcp, SYN, b"", now);
        assert_eq!(silent.hear(&mut tcp, now).len(), 1);
        let start = now;
        while tcp.count() > 0 {
            now += 10;
            assert!(now < start + SILENCE, "it never gives up");
            tcp.tick(now);
            silent.hear(&mut tcp, now);
        }
        assert_eq!(tcp.accept(80), None);
    }

    #[test]
    fn what_comes_out_of_order_is_asked_for_again() {
        let mut tcp = listening();
        let mut peer = Peer::call(&mut tcp, 50005, 80, 0);
        let id = tcp.accept(80).unwrap();
        // the second segment arrives first
        let second = peer.segment(ACK, peer.next + 5, b"world");
        tcp.input(THEIRS, OURS, &second, 1);
        let got = peer.hear(&mut tcp, 1);
        assert_eq!((got.len(), got[0].acknowledged), (1, peer.next));
        let mut read = [0u8; 16];
        assert_eq!(tcp.read(id, &mut read), 0);
        // then the first, then the second again
        peer.say(&mut tcp, ACK, b"hello", 2);
        peer.say(&mut tcp, ACK, b"world", 2);
        assert_eq!(tcp.read(id, &mut read), 10);
        assert_eq!(&read[..10], b"helloworld");
        // one that comes twice is acknowledged and not kept twice
        peer.hear(&mut tcp, 2);
        let twice = peer.segment(ACK, peer.next - 5, b"world");
        tcp.input(THEIRS, OURS, &twice, 3);
        assert_eq!(peer.hear(&mut tcp, 3)[0].acknowledged, peer.next);
        assert_eq!(tcp.read(id, &mut read), 0);
        // a segment damaged on the way is as if it never came
        let mut damaged = peer.segment(ACK, peer.next, b"oops");
        damaged[21] ^= 1;
        tcp.input(THEIRS, OURS, &damaged, 4);
        assert!(peer.hear(&mut tcp, 4).is_empty() && tcp.read(id, &mut read) == 0);
    }

    #[test]
    fn no_more_is_sent_than_they_have_room_for() {
        let mut tcp = listening();
        let mut peer = Peer::call(&mut tcp, 50006, 80, 0);
        let id = tcp.accept(80).unwrap();
        // they have room for 700 bytes
        peer.room = 700;
        peer.say(&mut tcp, ACK, b"", 1);
        assert_eq!(tcp.write(id, &[1; 2000]), 2000);
        let got = peer.hear(&mut tcp, 1);
        assert_eq!(got.iter().map(|g| g.bytes.len()).collect::<Vec<_>>(), [536, 164]);
        assert!(peer.hear(&mut tcp, 2).is_empty());
        // acknowledged, and room for nothing: we wait, then ask with one byte
        peer.room = 0;
        peer.say(&mut tcp, ACK, b"", 2);
        assert!(peer.hear(&mut tcp, 2).is_empty());
        tcp.tick(3);
        tcp.tick(3 + PATIENCE);
        let probe = peer.hear(&mut tcp, 3 + PATIENCE);
        assert_eq!(probe.iter().map(|g| g.bytes.len()).collect::<Vec<_>>(), [1]);
        // they have room again: the rest follows
        peer.room = 5000;
        peer.say(&mut tcp, ACK, b"", 60);
        let got = peer.hear(&mut tcp, 60);
        assert_eq!(got.iter().map(|g| g.bytes.len()).sum::<usize>(), 2000 - 700 - 1);
        peer.say(&mut tcp, ACK, b"", 61);
        assert_eq!(tcp.room(id), OUT);
    }

    #[test]
    fn we_say_how_much_room_we_have() {
        let mut tcp = listening();
        let mut peer = Peer::call(&mut tcp, 50007, 80, 0);
        let id = tcp.accept(80).unwrap();
        // more than we keep: what fits is taken and acknowledged, the rest is not
        let start = peer.next;
        peer.say(&mut tcp, ACK, &[5; 600], 1);
        peer.say(&mut tcp, ACK, &[6; 600], 1);
        let got = peer.hear(&mut tcp, 1);
        assert_eq!((got[0].acknowledged, got[0].room), (start + IN as u32, 0));
        // the program reads: they are told there is room again
        let mut read = [0u8; 2000];
        assert_eq!(tcp.read(id, &mut read), IN);
        assert!(read[..600].iter().all(|b| *b == 5) && read[600..IN].iter().all(|b| *b == 6));
        let got = peer.hear(&mut tcp, 2);
        assert_eq!((got.len(), got[0].room as usize), (1, IN));
    }

    #[test]
    fn they_may_close_first_or_at_the_same_time() {
        // they close first: we can still answer, then close
        let mut tcp = listening();
        let mut peer = Peer::call(&mut tcp, 50008, 80, 0);
        let id = tcp.accept(80).unwrap();
        peer.say(&mut tcp, FIN | ACK, b"bye", 1);
        assert_eq!(peer.hear(&mut tcp, 1)[0].acknowledged, peer.next);
        let mut read = [0u8; 8];
        assert!(!tcp.ended(id));
        assert_eq!(tcp.read(id, &mut read), 3);
        assert!(tcp.ended(id) && tcp.alive(id));
        assert_eq!(tcp.write(id, b"bye to you"), 10);
        tcp.close(id);
        let got = peer.hear(&mut tcp, 2);
        assert_eq!(got.iter().map(|g| (g.flags, g.bytes.len())).collect::<Vec<_>>(), [(ACK | PUSH, 10), (FIN | ACK, 0)]);
        assert_eq!(tcp.count(), 1);
        peer.say(&mut tcp, ACK, b"", 3);
        assert_eq!(tcp.count(), 0);

        // both at the same time: each FIN crosses the other
        let mut peer = Peer::call(&mut tcp, 50009, 80, 10);
        let id = tcp.accept(80).unwrap();
        tcp.close(id);
        let ours = peer.hear(&mut tcp, 11);
        assert_eq!(ours[0].flags, FIN | ACK);
        // theirs was sent before they saw ours
        peer.expected -= 1;
        peer.say(&mut tcp, FIN | ACK, b"", 11);
        assert_eq!(peer.hear(&mut tcp, 11)[0].acknowledged, peer.next);
        peer.expected += 1;
        peer.say(&mut tcp, ACK, b"", 12);
        tcp.tick(12 + LINGER);
        assert_eq!(tcp.count(), 0);
    }

    #[test]
    fn who_is_not_expected_is_refused() {
        let mut tcp = listening();
        // a call to a port nobody listens on
        let mut peer = Peer::new(50010, 81);
        peer.say(&mut tcp, SYN, b"", 0);
        let got = peer.hear(&mut tcp, 0);
        assert_eq!((got.len(), got[0].flags, got[0].acknowledged), (1, RST | ACK, 1001));
        // a segment of a conversation we know nothing of
        let mut peer = Peer::new(50011, 80);
        peer.expected = 777;
        peer.say(&mut tcp, ACK, b"hello?", 0);
        let got = peer.hear(&mut tcp, 0);
        assert_eq!((got.len(), got[0].flags, got[0].number), (1, RST, 777));
        // a refusal is not answered by a refusal
        peer.say(&mut tcp, RST, b"", 0);
        assert!(peer.hear(&mut tcp, 0).is_empty());
        // an answer to our answer that acknowledges the wrong number
        let mut peer = Peer::new(50012, 80);
        peer.say(&mut tcp, SYN, b"", 0);
        peer.hear(&mut tcp, 0);
        peer.expected += 5;
        peer.say(&mut tcp, ACK, b"", 0);
        assert_eq!(peer.hear(&mut tcp, 0)[0].flags, RST);
        assert_eq!(tcp.accept(80), None);
    }

    #[test]
    fn they_may_end_it_at_once() {
        let mut tcp = listening();
        let mut peer = Peer::call(&mut tcp, 50013, 80, 0);
        let id = tcp.accept(80).unwrap();
        // a refusal that does not know where the conversation is changes nothing
        let stray = peer.segment(RST, peer.next + 99, b"");
        tcp.input(THEIRS, OURS, &stray, 1);
        assert!(tcp.alive(id));
        peer.say(&mut tcp, RST, b"", 1);
        assert!(!tcp.alive(id) && tcp.ended(id) && tcp.lost == 1);
        assert_eq!(tcp.write(id, b"too late"), 0);
        assert!(peer.hear(&mut tcp, 1).is_empty());
        tcp.close(id);
        // and so may we
        let mut peer = Peer::call(&mut tcp, 50014, 80, 2);
        let id = tcp.accept(80).unwrap();
        tcp.abort(id);
        let got = peer.hear(&mut tcp, 2);
        assert_eq!((got.len(), got[0].flags, tcp.count()), (1, RST, 0));
    }

    #[test]
    fn there_are_only_so_many_places() {
        let mut tcp = listening();
        let mut peers = Vec::new();
        for at in 0..CONNECTIONS as u16 {
            peers.push(Peer::call(&mut tcp, 51000 + at, 80, 0));
        }
        assert_eq!(tcp.count(), CONNECTIONS);
        // one more gets no answer: it will call again
        let mut late = Peer::new(52000, 80);
        late.say(&mut tcp, SYN, b"", 1);
        assert!(late.hear(&mut tcp, 1).is_empty());
        // each is its own conversation
        let mut ids = Vec::new();
        while let Some(id) = tcp.accept(80) {
            ids.push(id);
        }
        assert_eq!(ids.len(), CONNECTIONS);
        for (at, peer) in peers.iter_mut().enumerate() {
            peer.say(&mut tcp, ACK, &[at as u8; 3], 2);
        }
        for id in &ids {
            let mut read = [0u8; 8];
            assert_eq!(tcp.read(*id, &mut read), 3);
            let from = tcp.peer(*id).1 - 51000;
            assert_eq!(read[..3], [from as u8; 3]);
        }
        // a place is freed, and the late one gets in
        tcp.abort(ids[3]);
        late.next = 1000;
        late.say(&mut tcp, SYN, b"", 3);
        assert_eq!(late.hear(&mut tcp, 3).len(), 1);
    }

    #[test]
    fn we_may_call_too() {
        let mut tcp = Tcp::new();
        let id = tcp.connect(THEIRS, 8000, 5).unwrap();
        assert!(tcp.alive(id) && !tcp.connected(id) && tcp.peer(id) == (THEIRS, 8000));
        // our call: a first number, nothing acknowledged, and the largest segment we take
        let mut segment = [0u8; SEGMENT];
        let (to, length) = tcp.output(OURS, 5, &mut segment).unwrap();
        let (port, first) = (be16(&segment, 0), be32(&segment, 4));
        assert_eq!((to, length, segment[13], be16(&segment, 2), &segment[20..24]), (THEIRS, 24, SYN, 8000, &[2u8, 4, 0x05, 0xb4][..]));
        assert!(port >= 49152 && checksum(OURS, THEIRS, &segment[..length]) == 0);
        assert!(tcp.output(OURS, 5, &mut segment).is_none());
        // what can be written before they answer waits
        assert_eq!(tcp.write(id, b"too early"), 0);
        // an answer that acknowledges another number is not for this call
        let mut peer = Peer::new(8000, port);
        peer.expected = first.wrapping_add(7);
        peer.say(&mut tcp, SYN | ACK, b"", 6);
        assert!(!tcp.connected(id));
        assert_eq!(peer.hear(&mut tcp, 6).iter().map(|g| g.flags).collect::<Vec<_>>(), [RST]);
        // they answer: we acknowledge, and the two can talk
        let mut peer = Peer::new(8000, port);
        peer.expected = first.wrapping_add(1);
        peer.say(&mut tcp, SYN | ACK, b"", 7);
        assert!(tcp.connected(id) && tcp.opened == 1);
        let got = peer.hear(&mut tcp, 7);
        assert_eq!((got.len(), got[0].flags, got[0].number, got[0].acknowledged), (1, ACK, first.wrapping_add(1), 1001));
        assert_eq!(tcp.write(id, b"GET / HTTP/1.0\r\n\r\n"), 18);
        assert_eq!(peer.hear(&mut tcp, 8)[0].bytes, b"GET / HTTP/1.0\r\n\r\n");
        peer.say(&mut tcp, ACK | PUSH, b"HTTP/1.0 200 OK\r\n\r\nhello", 9);
        peer.say(&mut tcp, FIN | ACK, b"", 9);
        let mut read = [0u8; 64];
        let count = tcp.read(id, &mut read);
        assert_eq!(&read[..count], b"HTTP/1.0 200 OK\r\n\r\nhello");
        assert!(tcp.ended(id));
        tcp.close(id);
        let got = peer.hear(&mut tcp, 10);
        assert_eq!(got.last().unwrap().flags, FIN | ACK);
        peer.say(&mut tcp, ACK, b"", 11);
        assert_eq!(tcp.count(), 0);
    }

    #[test]
    fn a_call_nobody_answers_is_given_up_and_one_that_is_refused_ends_at_once() {
        let mut tcp = Tcp::new();
        let id = tcp.connect(THEIRS, 8000, 0).unwrap();
        let mut segment = [0u8; SEGMENT];
        let mut calls = 0;
        let mut now = 0;
        while tcp.alive(id) {
            while tcp.output(OURS, now, &mut segment).is_some() {
                assert_eq!(segment[13], SYN);
                calls += 1;
            }
            now += 10;
            assert!(now < SILENCE, "it never gives up");
            tcp.tick(now);
        }
        assert_eq!((calls, tcp.connected(id), tcp.lost), (1 + TRIES, false, 1));
        tcp.close(id);
        assert_eq!(tcp.count(), 0);
        // nobody listens on that port: they say so, and it is over at once
        let id = tcp.connect(THEIRS, 8001, now).unwrap();
        let (_, length) = tcp.output(OURS, now, &mut segment).unwrap();
        let (port, first) = (be16(&segment[..length], 0), be32(&segment, 4));
        let mut peer = Peer::new(8001, port);
        // a refusal that does not acknowledge our call is not believed
        peer.expected = first;
        peer.say(&mut tcp, RST | ACK, b"", now);
        assert!(tcp.alive(id));
        peer.expected = first.wrapping_add(1);
        peer.say(&mut tcp, RST | ACK, b"", now);
        assert!(!tcp.alive(id) && tcp.lost == 2);
        tcp.close(id);
        // each call goes out from a port of its own
        let mut ports = Vec::new();
        for _ in 0..CONNECTIONS {
            let id = tcp.connect(THEIRS, 8002, now).unwrap();
            ports.push(tcp.connections[id].port);
        }
        ports.sort();
        ports.dedup();
        assert_eq!(ports.len(), CONNECTIONS);
        // and there are only so many places
        assert_eq!(tcp.connect(THEIRS, 8002, now), None);
    }

    #[test]
    fn a_place_that_lingers_is_given_to_a_new_caller() {
        let mut tcp = listening();
        // every place is taken by a conversation that is over on both sides
        for at in 0..CONNECTIONS as u16 {
            let mut peer = Peer::call(&mut tcp, 54000 + at, 80, 0);
            let id = tcp.accept(80).unwrap();
            tcp.close(id);
            peer.hear(&mut tcp, 1);
            peer.say(&mut tcp, FIN | ACK, b"", 1);
            peer.hear(&mut tcp, 1);
        }
        assert_eq!(tcp.count(), CONNECTIONS);
        // callers come before the places are freed by time: each gets one
        for at in 0..CONNECTIONS as u16 {
            let mut peer = Peer::call(&mut tcp, 55000 + at, 80, 2);
            let id = tcp.accept(80).unwrap();
            peer.say(&mut tcp, ACK, b"hi", 2);
            let mut read = [0u8; 4];
            assert_eq!((tcp.read(id, &mut read), tcp.peer(id).1), (2, 55000 + at));
        }
        // those are in use: one more waits
        let mut late = Peer::new(56000, 80);
        late.say(&mut tcp, SYN, b"", 3);
        assert!(late.hear(&mut tcp, 3).is_empty());
    }

    #[test]
    fn a_silent_conversation_is_ended() {
        let mut tcp = listening();
        let mut peer = Peer::call(&mut tcp, 50015, 80, 0);
        let id = tcp.accept(80).unwrap();
        tcp.tick(SILENCE - 1);
        assert!(tcp.alive(id));
        tcp.tick(SILENCE);
        assert!(!tcp.alive(id) && tcp.count() == 0);
        // they are told, in case they are still there
        assert_eq!(peer.hear(&mut tcp, SILENCE)[0].flags, RST);
        tcp.close(id);
        // one that nobody took is simply freed
        let mut peer = Peer::call(&mut tcp, 50016, 80, 7000);
        tcp.tick(7000 + SILENCE);
        assert_eq!((tcp.count(), tcp.accept(80)), (0, None));
        peer.hear(&mut tcp, 7000 + SILENCE);
        // no longer listening: who is waiting is refused, who is being served goes on
        let mut served = Peer::call(&mut tcp, 50017, 80, 20000);
        let id = tcp.accept(80).unwrap();
        let mut waiting = Peer::call(&mut tcp, 50018, 80, 20000);
        tcp.unlisten(80);
        assert!(!tcp.listens(80) && tcp.alive(id));
        assert_eq!(waiting.hear(&mut tcp, 20000)[0].flags, RST);
        served.say(&mut tcp, ACK, b"still here", 20001);
        let mut read = [0u8; 16];
        assert_eq!(tcp.read(id, &mut read), 10);
    }

    #[test]
    fn our_first_number_cannot_be_guessed_from_the_one_before() {
        let mut tcp = listening();
        let mut seen = Vec::new();
        for at in 0..6u16 {
            let mut peer = Peer::new(53000 + at, 80);
            peer.say(&mut tcp, SYN, b"", at as u64);
            seen.push(peer.hear(&mut tcp, at as u64)[0].number);
        }
        let steps: Vec<u32> = seen.windows(2).map(|pair| pair[1].wrapping_sub(pair[0])).collect();
        assert!(steps.windows(2).all(|pair| pair[0] != pair[1]), "{steps:?}");
        // only so many ports can be listened on
        let mut tcp = Tcp::new();
        assert!((1..=PORTS as u16).all(|port| tcp.listen(port)) && !tcp.listen(99) && tcp.listen(2) && !tcp.listen(0));
    }
}
