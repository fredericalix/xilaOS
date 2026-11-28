//! The network: what goes over an Ethernet cable, and what XilaOS answers.
//!
//! This is the part without hardware. A driver gives it the frames that
//! arrive (`receive`) and the time that passes (`tick`), and sends the
//! frames it is handed (`Wire`). Everything in between is here:
//!
//! - **Ethernet**: a frame goes to a card by its address, six bytes that
//!   every card has;
//! - **ARP**: asks all the cards of the cable which one has an IP
//!   address, and answers when ours is asked for;
//! - **IPv4**: packets between machines by their IP address, directly on
//!   the cable or through the gateway. Not the pieces of packets cut
//!   on the way: nothing XilaOS exchanges is that large;
//! - **ICMP**: answers a `ping`, and sends one;
//! - **UDP**: small messages to a numbered port of a machine;
//! - **DHCP**: asks the network for an address, a mask, a gateway and a
//!   name server, as a machine does when its cable is plugged in, and
//!   asks again before the lease runs out.
//!
//! - **TCP**: conversations in which every byte arrives, in order. The
//!   protocol itself is in `tcp.rs`; this carries its segments.
//!
//! - **Names** (DNS): the address of a machine known by its name, asked
//!   of the server the network named. The questions and answers are in
//!   `dns.rs`; this carries them.
//!
//! Numbers of several bytes are written with the most significant byte
//! first, the reverse of the processor: `be` below.

use core::fmt::Write;

use crate::shell::Text;

pub type Mac = [u8; 6];
pub type Ip = [u8; 4];

const EVERYONE: Mac = [0xff; 6];
const NOWHERE: Ip = [0; 4];
const EVERY_IP: Ip = [255; 4];

/// The longest frame, and the shortest a card may send.
pub const FRAME: usize = 1514;
const SHORTEST: usize = 60;

const ARP: u16 = 0x0806;
const IPV4: u16 = 0x0800;
const ICMP: u8 = 1;
const TCP: u8 = 6;
const UDP: u8 = 17;

/// Ticks of the timer: a second, and how long things may take.
const SECOND: u64 = 100;
/// An address learnt by ARP is asked again after a while.
const KNOWN_FOR: u64 = 300 * SECOND;

/// Where frames go out: the driver of a card.
pub trait Wire {
    fn send(&mut self, frame: &[u8]);
}

fn be16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The checksum of the internet protocols over some bytes, continued from
/// `sum`: the bytes two by two as numbers, added with the carries.
fn add(mut sum: u32, bytes: &[u8]) -> u32 {
    for pair in bytes.chunks(2) {
        sum += (pair[0] as u32) << 8 | pair.get(1).copied().unwrap_or(0) as u32;
    }
    sum
}

fn checksum(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// An IP address as people write it: `192.168.1.20`.
pub fn ip_text(ip: Ip) -> Text {
    let mut text = Text::new();
    let _ = write!(text, "{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
    text
}

/// Reads an IP address written `192.168.1.20`.
pub fn ip_of(text: &[u8]) -> Option<Ip> {
    let mut ip = [0u8; 4];
    let mut parts = text.split(|byte| *byte == b'.');
    for part in &mut ip {
        let number = parts.next().filter(|part| !part.is_empty() && part.len() <= 3)?;
        *part = u8::try_from(crate::text::parse_number(number)?).ok()?;
    }
    parts.next().is_none().then_some(ip)
}

/// The address of a card as it is written: `52:54:00:12:34:56`.
pub fn mac_text(mac: Mac) -> Text {
    let mut text = Text::new();
    for (at, byte) in mac.iter().enumerate() {
        let _ = write!(text, "{}{byte:02x}", if at > 0 { ":" } else { "" });
    }
    text
}

/// How far the asking for an address has got.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lease {
    /// Asking who offers one; again at that tick.
    Asking { again: u64, tries: u32 },
    /// Offered this one by that server, and asking to have it.
    Taking { ip: Ip, server: Ip, again: u64, tries: u32 },
    /// It is ours until `until`, and asked for again from `renew` on.
    Held { until: u64, renew: u64 },
}

/// A ping that was sent, and its answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ping {
    pub to: Ip,
    number: u16,
    sent: u64,
    /// Ticks it took, once the answer has come.
    pub took: Option<u64>,
}

/// A message for a port of this machine, as `receive` returns it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Message<'a> {
    pub from: Ip,
    pub from_port: u16,
    pub port: u16,
    pub bytes: &'a [u8],
}

const KNOWN: usize = 8;
/// Packets that wait at one time for the address of a card.
pub const HELD: usize = 4;
/// Pings whose answers are waited for at one time.
pub const PINGS: usize = 8;

/// The network as this machine sees it.
pub struct Net {
    /// The address of the card; None while there is no card XilaOS drives.
    pub mac: Option<Mac>,
    /// Our address, the mask that says which addresses are on the cable,
    /// and the machine that passes packets to the others.
    pub ip: Option<Ip>,
    pub mask: Ip,
    pub gateway: Ip,
    lease: Lease,
    /// The number that tells our exchange with the server from others.
    exchange: u32,
    /// The cards known by their IP address, and when each was learnt.
    known: [(Ip, Mac, u64); KNOWN],
    /// The packets that wait for the address of a card: whose, how long
    /// each is and since when.
    waiting: [Option<(Ip, usize, u64)>; HELD],
    held: [[u8; FRAME]; HELD],
    /// The pings that are followed: the last that were sent.
    followed: [Option<Ping>; PINGS],
    pings: u16,
    /// The conversations with other machines.
    pub tcp: crate::tcp::Tcp,
    /// The machines known by their name, and who is asked for the others.
    pub names: crate::dns::Names,
    /// Frames taken from the cable and given to it.
    pub received: u64,
    pub sent: u64,
}

impl Net {
    pub const fn new() -> Net {
        Net {
            mac: None,
            ip: None,
            mask: NOWHERE,
            gateway: NOWHERE,
            lease: Lease::Asking { again: 0, tries: 0 },
            exchange: 0,
            known: [(NOWHERE, [0; 6], 0); KNOWN],
            waiting: [None; HELD],
            held: [[0; FRAME]; HELD],
            followed: [None; PINGS],
            pings: 0,
            tcp: crate::tcp::Tcp::new(),
            names: crate::dns::Names::new(),
            received: 0,
            sent: 0,
        }
    }

    /// A card was found, or set up again: its address. Whatever address
    /// the network had given is asked for anew. The number of the exchange
    /// with the server of addresses comes from the card and from the time.
    pub fn start(&mut self, mac: Mac, now: u64) {
        self.mac = Some(mac);
        (self.ip, self.waiting) = (None, [None; HELD]);
        self.exchange = u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]) ^ (now as u32).wrapping_mul(0x9e37_79b9);
        self.lease = Lease::Asking { again: now, tries: 0 };
        // the names were those of the network the card was on
        self.names.forget();
    }

    /// Whether an address was given, and until which tick.
    pub fn held_until(&self) -> Option<u64> {
        match self.lease {
            Lease::Held { until, .. } => Some(until),
            _ => None,
        }
    }

    fn put(&mut self, frame: &mut [u8], length: usize, wire: &mut dyn Wire) {
        // a frame shorter than the shortest is filled with nothing
        let length = length.max(SHORTEST);
        self.sent += 1;
        wire.send(&frame[..length]);
    }

    /// Writes the start of a frame: to whom, from us, and of what kind.
    fn frame(&self, frame: &mut [u8], to: Mac, kind: u16) {
        frame[..SHORTEST].fill(0);
        frame[..6].copy_from_slice(&to);
        frame[6..12].copy_from_slice(&self.mac.unwrap_or([0; 6]));
        frame[12..14].copy_from_slice(&kind.to_be_bytes());
    }

    fn learn(&mut self, ip: Ip, mac: Mac, now: u64) {
        if ip == NOWHERE || ip == EVERY_IP {
            return;
        }
        // in place of what was known of it, or of the oldest
        let at = self.known.iter().position(|known| known.0 == ip).unwrap_or_else(|| (0..KNOWN).min_by_key(|at| self.known[*at].2).unwrap_or(0));
        self.known[at] = (ip, mac, now.max(1));
    }

    fn card_of(&self, ip: Ip, now: u64) -> Option<Mac> {
        if ip == EVERY_IP {
            return Some(EVERYONE);
        }
        self.known.iter().find(|known| known.0 == ip && known.2 != 0 && now < known.2 + KNOWN_FOR).map(|known| known.1)
    }

    fn arp(&mut self, asking: bool, to: Mac, their_mac: Mac, their_ip: Ip, wire: &mut dyn Wire) {
        let mut frame = [0u8; SHORTEST];
        self.frame(&mut frame, to, ARP);
        // Ethernet and IPv4, addresses of six and four bytes; 1 asks, 2 answers
        frame[14..22].copy_from_slice(&[0, 1, 8, 0, 6, 4, 0, if asking { 1 } else { 2 }]);
        frame[22..28].copy_from_slice(&self.mac.unwrap_or([0; 6]));
        frame[28..32].copy_from_slice(&self.ip.unwrap_or(NOWHERE));
        frame[32..38].copy_from_slice(&their_mac);
        frame[38..42].copy_from_slice(&their_ip);
        self.put(&mut frame, 42, wire);
    }

    /// Sends what is in `frame` from byte 34 on, `length` bytes of it, as
    /// an IP packet of a kind to an address. Returns false if nobody can
    /// take it: no address of our own, or no gateway to somewhere else.
    fn packet(&mut self, frame: &mut [u8; FRAME], to: Ip, kind: u8, length: usize, from: Ip, now: u64, wire: &mut dyn Wire) -> bool {
        let total = 20 + length;
        frame[14..34].copy_from_slice(&[0x45, 0, (total >> 8) as u8, total as u8, 0, 0, 0x40, 0, 64, kind, 0, 0, from[0], from[1], from[2], from[3], to[0], to[1], to[2], to[3]]);
        let sum = checksum(add(0, &frame[14..34]));
        frame[24..26].copy_from_slice(&sum.to_be_bytes());
        // on the cable, the card of that address; elsewhere, the gateway
        let near = to == EVERY_IP || (0..4).all(|at| (to[at] ^ from[at]) & self.mask[at] == 0);
        let next = if near { to } else { self.gateway };
        if next == NOWHERE {
            return false;
        }
        match self.card_of(next, now) {
            Some(card) => {
                self.frame_keeping(frame, card);
                self.put(frame, 14 + total, wire);
            }
            None => {
                // ask whose address it is, and keep the packet until the answer:
                // whole, but for the card it will go to
                // in a place that is free, or in that of the one that waited longest
                self.frame_keeping(frame, [0; 6]);
                let at = (0..HELD).min_by_key(|at| self.waiting[*at].map_or(0, |(_, _, since)| since + 1)).unwrap_or(0);
                self.held[at][..14 + total].copy_from_slice(&frame[..14 + total]);
                self.waiting[at] = Some((next, 14 + total, now));
                self.arp(true, EVERYONE, [0; 6], next, wire);
            }
        }
        true
    }

    /// Writes the start of a frame without touching what follows it.
    fn frame_keeping(&self, frame: &mut [u8], to: Mac) {
        frame[..6].copy_from_slice(&to);
        frame[6..12].copy_from_slice(&self.mac.unwrap_or([0; 6]));
        frame[12..14].copy_from_slice(&IPV4.to_be_bytes());
    }

    /// Whether an address is on this machine's own network: on the same
    /// cable, not beyond the gateway.
    pub fn local(&self, ip: Ip) -> bool {
        self.ip.is_some_and(|ours| self.mask != NOWHERE && (0..4).all(|at| (ip[at] ^ ours[at]) & self.mask[at] == 0))
    }

    /// Sends a message to a port of a machine, from one of ours.
    pub fn message(&mut self, to: Ip, to_port: u16, from_port: u16, bytes: &[u8], now: u64, wire: &mut dyn Wire) -> bool {
        let Some(from) = self.ip else {
            return false;
        };
        if bytes.len() > FRAME - 42 {
            return false;
        }
        let mut frame = [0u8; FRAME];
        self.udp(&mut frame, from, to, from_port, to_port, bytes);
        self.packet(&mut frame, to, UDP, 8 + bytes.len(), from, now, wire)
    }

    /// Writes a UDP message at its place in a frame, with its checksum.
    fn udp(&self, frame: &mut [u8; FRAME], from: Ip, to: Ip, from_port: u16, to_port: u16, bytes: &[u8]) {
        let length = 8 + bytes.len();
        frame[34..36].copy_from_slice(&from_port.to_be_bytes());
        frame[36..38].copy_from_slice(&to_port.to_be_bytes());
        frame[38..40].copy_from_slice(&(length as u16).to_be_bytes());
        frame[40..42].fill(0);
        frame[42..42 + bytes.len()].copy_from_slice(bytes);
        // the checksum also covers the two addresses, the kind and the length
        let mut sum = add(0, &from);
        sum = add(sum, &to);
        sum += UDP as u32 + length as u32;
        let sum = match checksum(add(sum, &frame[34..34 + length])) {
            // 0 would say "no checksum"
            0 => 0xffff,
            sum => sum,
        };
        frame[40..42].copy_from_slice(&sum.to_be_bytes());
    }

    /// Looks for the address of a machine by its name (`dns::valid`), in
    /// what is known or by asking the name server. To be called again and
    /// again, with `receive` in between, until it says something else
    /// than `Asking`: three seconds at most.
    pub fn name(&mut self, name: &[u8], now: u64, wire: &mut dyn Wire) -> crate::dns::Lookup {
        let mut question = [0u8; crate::dns::QUESTION];
        let (state, send) = self.names.look(name, now, &mut question);
        if let Some((to, to_port, from_port, length)) = send {
            if !self.message(to, to_port, from_port, &question[..length], now, wire) {
                // no address of our own, or no way to the server
                self.names.give_up(name);
                return crate::dns::Lookup::Failed(crate::dns::Failed::NoServer);
            }
        }
        state
    }

    /// Sends a ping, and returns the number it is followed by, or None
    /// when it could not be sent. `pinged` then says whether its answer
    /// came. Several are followed side by side; one more takes the place
    /// of the oldest.
    pub fn ping(&mut self, to: Ip, now: u64, wire: &mut dyn Wire) -> Option<u16> {
        let from = self.ip?;
        self.pings = self.pings.wrapping_add(1);
        let mut frame = [0u8; FRAME];
        // an echo request, ours by the two numbers that follow the checksum
        frame[34..42].copy_from_slice(&[8, 0, 0, 0, 0x58, 0x69, (self.pings >> 8) as u8, self.pings as u8]);
        frame[42..58].copy_from_slice(b"XilaOS says ping");
        let sum = checksum(add(0, &frame[34..58]));
        frame[36..38].copy_from_slice(&sum.to_be_bytes());
        let at = (0..PINGS).min_by_key(|at| self.followed[*at].map_or(0, |ping| ping.sent + 1)).unwrap_or(0);
        self.followed[at] = Some(Ping { to, number: self.pings, sent: now, took: None });
        self.packet(&mut frame, to, ICMP, 24, from, now, wire).then_some(self.pings)
    }

    /// What became of a ping: None when it is no longer followed, else
    /// the ticks its answer took, if it came.
    pub fn pinged(&self, number: u16) -> Option<Option<u64>> {
        self.followed.iter().flatten().find(|ping| ping.number == number).map(|ping| ping.took)
    }

    /// Whether as many packets wait for the address of a card as can:
    /// one more that had to wait would take the place of one. A packet
    /// counts for a second: a card that has not said its address by then
    /// will not, and its place is anyone's.
    pub fn holding(&self, now: u64) -> bool {
        self.waiting.iter().all(|waiting| waiting.is_some_and(|(_, _, since)| now < since + 100))
    }

    /// Asks the network for an address, or to have the one it offered.
    fn ask(&mut self, taking: Option<(Ip, Ip)>, wire: &mut dyn Wire) {
        let mut frame = [0u8; FRAME];
        let mac = self.mac.unwrap_or([0; 6]);
        let body = &mut frame[42..];
        // a request, on Ethernet, with addresses of six bytes
        body[..4].copy_from_slice(&[1, 1, 6, 0]);
        body[4..8].copy_from_slice(&self.exchange.to_be_bytes());
        // answer to everyone: we have no address to be answered at
        body[10] = 0x80;
        if let (None, Some(ours)) = (taking, self.ip) {
            // asking again for the one we hold
            body[12..16].copy_from_slice(&ours);
        }
        body[28..34].copy_from_slice(&mac);
        body[236..240].copy_from_slice(&[99, 130, 83, 99]);
        let mut at = 240;
        let mut option = |code: u8, value: &[u8]| {
            body[at] = code;
            body[at + 1] = value.len() as u8;
            body[at + 2..at + 2 + value.len()].copy_from_slice(value);
            at += 2 + value.len();
        };
        // 53: what this is, 1 to ask who offers, 3 to ask for an address
        let held = self.ip.is_some();
        option(53, &[if taking.is_some() || held { 3 } else { 1 }]);
        if let Some((ip, server)) = taking {
            option(50, &ip);
            option(54, &server);
        }
        option(12, b"xilaos");
        // 55: what we want to be told: the mask, the gateway, the name
        // server and the lease
        option(55, &[1, 3, 6, 51]);
        body[at] = 255;
        let length = (at + 1).max(300);
        self.udp(&mut frame, NOWHERE, EVERY_IP, 68, 67, &[]);
        // `udp` wrote an empty message: now with its real length and checksum
        let total = 8 + length;
        frame[38..40].copy_from_slice(&(total as u16).to_be_bytes());
        frame[40..42].fill(0);
        let mut sum = add(0, &EVERY_IP);
        sum += UDP as u32 + total as u32;
        let sum = checksum(add(sum, &frame[34..34 + total]));
        frame[40..42].copy_from_slice(&(if sum == 0 { 0xffff } else { sum }).to_be_bytes());
        let from = NOWHERE;
        frame[14..34].copy_from_slice(&[0x45, 0, ((20 + total) >> 8) as u8, (20 + total) as u8, 0, 0, 0x40, 0, 64, UDP, 0, 0, from[0], from[1], from[2], from[3], 255, 255, 255, 255]);
        let sum = checksum(add(0, &frame[14..34]));
        frame[24..26].copy_from_slice(&sum.to_be_bytes());
        self.frame_keeping(&mut frame, EVERYONE);
        self.put(&mut frame, 34 + total, wire);
    }

    /// What the server of addresses answered.
    fn answered(&mut self, body: &[u8], now: u64, wire: &mut dyn Wire) {
        if body.len() < 244 || body[0] != 2 || be32(body, 4) != self.exchange || body[236..240] != [99, 130, 83, 99] {
            return;
        }
        if body[28..34] != self.mac.unwrap_or([0; 6]) {
            return;
        }
        let offered: Ip = [body[16], body[17], body[18], body[19]];
        let (mut kind, mut mask, mut gateway, mut server, mut names, mut seconds) = (0, NOWHERE, NOWHERE, NOWHERE, NOWHERE, 3600u64);
        let mut at = 240;
        while at + 1 < body.len() && body[at] != 255 {
            if body[at] == 0 {
                at += 1;
                continue;
            }
            let (code, length) = (body[at], body[at + 1] as usize);
            let Some(value) = body.get(at + 2..at + 2 + length) else {
                return;
            };
            match (code, length) {
                (53, 1) => kind = value[0],
                (1, 4) => mask.copy_from_slice(value),
                (3, 4..) => gateway.copy_from_slice(&value[..4]),
                // the first of the name servers, if several are named
                (6, 4..) => names.copy_from_slice(&value[..4]),
                (54, 4) => server.copy_from_slice(value),
                (51, 4) => seconds = be32(value, 0) as u64,
                _ => {}
            }
            at += 2 + length;
        }
        match (kind, self.lease) {
            // an offer, while asking: take it
            (2, Lease::Asking { .. }) if offered != NOWHERE => {
                self.lease = Lease::Taking { ip: offered, server, again: now + 2 * SECOND, tries: 1 };
                self.ask(Some((offered, server)), wire);
            }
            // it is ours
            (5, Lease::Taking { .. } | Lease::Held { .. }) if offered != NOWHERE => {
                (self.ip, self.mask, self.gateway) = (Some(offered), mask, gateway);
                self.names.told(names);
                let ticks = seconds.clamp(60, 30 * 86_400) * SECOND;
                self.lease = Lease::Held { until: now + ticks, renew: now + ticks / 2 };
            }
            // refused: start again
            (6, _) => {
                self.ip = None;
                self.lease = Lease::Asking { again: now + SECOND, tries: 0 };
            }
            _ => {}
        }
    }

    /// The time that passes: asks for an address, or again for the one held.
    pub fn tick(&mut self, now: u64, wire: &mut dyn Wire) {
        if self.mac.is_none() {
            return;
        }
        match self.lease {
            Lease::Asking { again, tries } if now >= again => {
                // sooner at first, then every eight seconds
                self.lease = Lease::Asking { again: now + (2u64 << tries.min(2)) * SECOND, tries: tries + 1 };
                self.ask(None, wire);
            }
            Lease::Taking { ip, server, again, tries } if now >= again => {
                if tries >= 4 {
                    self.lease = Lease::Asking { again: now, tries: 0 };
                } else {
                    self.lease = Lease::Taking { ip, server, again: now + 2 * SECOND, tries: tries + 1 };
                    self.ask(Some((ip, server)), wire);
                }
            }
            Lease::Held { until, .. } if now >= until => {
                // nobody renewed it: it is no longer ours
                self.ip = None;
                self.lease = Lease::Asking { again: now, tries: 0 };
            }
            Lease::Held { until, renew } if now >= renew => {
                // again a minute later, if this one is not answered
                self.lease = Lease::Held { until, renew: now + 60 * SECOND };
                self.ask(None, wire);
            }
            _ => {}
        }
        self.tcp.tick(now);
        self.speak(now, wire);
    }

    /// Sends what the conversations have to say: after a program wrote to
    /// one, and whenever something came or time passed. No more at once
    /// than a card takes; the rest goes the next time.
    pub fn speak(&mut self, now: u64, wire: &mut dyn Wire) {
        let Some(ours) = self.ip else {
            return;
        };
        let mut segment = [0u8; crate::tcp::SEGMENT];
        for _ in 0..12 {
            let Some((to, length)) = self.tcp.output(ours, now, &mut segment) else {
                break;
            };
            let mut frame = [0u8; FRAME];
            frame[34..34 + length].copy_from_slice(&segment[..length]);
            self.packet(&mut frame, to, TCP, length, ours, now, wire);
        }
    }

    /// A frame came from the cable. Answers what the network asks of any
    /// machine, and returns a message if the frame carries one for a port
    /// of this machine.
    pub fn receive<'a>(&mut self, frame: &'a [u8], now: u64, wire: &mut dyn Wire) -> Option<Message<'a>> {
        let ours = self.mac?;
        if frame.len() < 42 || (frame[..6] != ours && frame[..6] != EVERYONE) {
            return None;
        }
        self.received += 1;
        let their_card: Mac = [frame[6], frame[7], frame[8], frame[9], frame[10], frame[11]];
        match be16(frame, 12) {
            ARP => {
                // Ethernet and IPv4 only
                if frame[14..20] != [0, 1, 8, 0, 6, 4] {
                    return None;
                }
                let (asking, their_ip, wanted): (bool, Ip, Ip) = (frame[21] == 1, [frame[28], frame[29], frame[30], frame[31]], [frame[38], frame[39], frame[40], frame[41]]);
                if self.ip.is_some_and(|ip| ip == wanted) {
                    self.learn(their_ip, their_card, now);
                    if asking {
                        self.arp(false, their_card, their_card, their_ip, wire);
                    }
                } else if self.card_of(their_ip, now).is_some() || self.waiting.iter().flatten().any(|(ip, ..)| *ip == their_ip) {
                    // news of a card we talk to
                    self.learn(their_ip, their_card, now);
                }
                // the packets that waited for this address can go
                for at in 0..HELD {
                    if let Some((_, length, _)) = self.waiting[at].filter(|(ip, ..)| *ip == their_ip) {
                        self.waiting[at] = None;
                        let mut held = self.held[at];
                        held[..6].copy_from_slice(&their_card);
                        self.put(&mut held, length, wire);
                    }
                }
                None
            }
            IPV4 => {
                let header = (frame[14] & 0x0f) as usize * 4;
                let total = be16(frame, 16) as usize;
                // version 4, a whole header, a whole packet, and not a piece of one
                if frame[14] >> 4 != 4 || header < 20 || total < header + 8 || frame.len() < 14 + total || be16(frame, 20) & 0x3fff != 0 {
                    return None;
                }
                if checksum(add(0, &frame[14..14 + header])) != 0 {
                    return None;
                }
                let (from, to): (Ip, Ip) = ([frame[26], frame[27], frame[28], frame[29]], [frame[30], frame[31], frame[32], frame[33]]);
                let data = &frame[14 + header..14 + total];
                match frame[23] {
                    UDP => {
                        let length = be16(data, 4) as usize;
                        if length < 8 || length > data.len() {
                            return None;
                        }
                        // 0 says the sender made no checksum
                        if be16(data, 6) != 0 {
                            let mut sum = add(0, &from);
                            sum = add(sum, &to);
                            sum += UDP as u32 + length as u32;
                            if checksum(add(sum, &data[..length])) != 0 {
                                return None;
                            }
                        }
                        let (from_port, port, bytes) = (be16(data, 0), be16(data, 2), &data[8..length]);
                        if port == 68 && from_port == 67 {
                            // the server of addresses: also before we have one
                            self.answered(bytes, now, wire);
                            return None;
                        }
                        if self.ip != Some(to) {
                            return None;
                        }
                        self.learn(from, their_card, now);
                        if self.names.answered(from, from_port, port, bytes, now) {
                            // what a name server says of the name we asked about
                            return None;
                        }
                        Some(Message { from, from_port, port, bytes })
                    }
                    ICMP if self.ip == Some(to) && checksum(add(0, data)) == 0 => {
                        self.learn(from, their_card, now);
                        match data[0] {
                            // a ping: the same back, as an answer
                            8 if total <= FRAME - 14 => {
                                let mut answer = [0u8; FRAME];
                                answer[34..34 + data.len()].copy_from_slice(data);
                                answer[34] = 0;
                                answer[36..38].fill(0);
                                let sum = checksum(add(0, &answer[34..34 + data.len()]));
                                answer[36..38].copy_from_slice(&sum.to_be_bytes());
                                self.packet(&mut answer, from, ICMP, data.len(), to, now, wire);
                            }
                            // the answer to ours
                            0 => {
                                for ping in self.followed.iter_mut().flatten() {
                                    if ping.took.is_none() && ping.to == from && be16(data, 4) == 0x5869 && be16(data, 6) == ping.number {
                                        ping.took = Some(now.saturating_sub(ping.sent));
                                    }
                                }
                            }
                            _ => {}
                        }
                        None
                    }
                    TCP if self.ip == Some(to) => {
                        self.learn(from, their_card, now);
                        self.tcp.input(from, to, data, now);
                        self.speak(now, wire);
                        None
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

impl Default for Net {
    fn default() -> Net {
        Net::new()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A cable that keeps what is sent on it.
    #[derive(Default)]
    pub(crate) struct Kept(pub Vec<Vec<u8>>);

    impl Wire for Kept {
        fn send(&mut self, frame: &[u8]) {
            self.0.push(frame.to_vec());
        }
    }

    pub(crate) const OURS: Mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
    pub(crate) const THEIRS: Mac = [0x52, 0x55, 0x0a, 0, 2, 2];
    pub(crate) const BOX: Ip = [192, 168, 1, 1];
    pub(crate) const US: Ip = [192, 168, 1, 20];

    /// A frame as another machine would send it, built by other code than
    /// the one above: a packet of a kind with its data.
    pub(crate) fn packet(to: Mac, from_card: Mac, from: Ip, to_ip: Ip, kind: u8, data: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&to);
        frame.extend_from_slice(&from_card);
        frame.extend_from_slice(&[8, 0]);
        let total = (20 + data.len()) as u16;
        let mut header = vec![0x45, 0, (total >> 8) as u8, total as u8, 0x12, 0x34, 0, 0, 64, kind, 0, 0];
        header.extend_from_slice(&from);
        header.extend_from_slice(&to_ip);
        let sum = sum_of(&header);
        header[10..12].copy_from_slice(&sum.to_be_bytes());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(data);
        frame
    }

    /// The checksum, the slow and plain way.
    pub(crate) fn sum_of(bytes: &[u8]) -> u16 {
        let mut sum = 0u64;
        for at in (0..bytes.len()).step_by(2) {
            sum += (bytes[at] as u64) << 8 | *bytes.get(at + 1).unwrap_or(&0) as u64;
        }
        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        !(sum as u16)
    }

    pub(crate) fn udp(from: Ip, to: Ip, from_port: u16, to_port: u16, bytes: &[u8]) -> Vec<u8> {
        let length = (8 + bytes.len()) as u16;
        let mut data = Vec::new();
        data.extend_from_slice(&from_port.to_be_bytes());
        data.extend_from_slice(&to_port.to_be_bytes());
        data.extend_from_slice(&length.to_be_bytes());
        data.extend_from_slice(&[0, 0]);
        data.extend_from_slice(bytes);
        let mut all = Vec::new();
        all.extend_from_slice(&from);
        all.extend_from_slice(&to);
        all.extend_from_slice(&[0, 17]);
        all.extend_from_slice(&length.to_be_bytes());
        all.extend_from_slice(&data);
        let sum = sum_of(&all);
        data[6..8].copy_from_slice(&sum.to_be_bytes());
        data
    }

    /// A network that has its address already.
    pub(crate) fn settled() -> Net {
        let mut net = Net::new();
        net.start(OURS, 0);
        (net.ip, net.mask, net.gateway) = (Some(US), [255, 255, 255, 0], BOX);
        net.lease = Lease::Held { until: 1_000_000, renew: 500_000 };
        net
    }

    pub(crate) fn arp(asking: bool, from_card: Mac, from: Ip, wanted: Ip) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(if asking { &EVERYONE } else { &OURS });
        frame.extend_from_slice(&from_card);
        frame.extend_from_slice(&[8, 6, 0, 1, 8, 0, 6, 4, 0, if asking { 1 } else { 2 }]);
        frame.extend_from_slice(&from_card);
        frame.extend_from_slice(&from);
        frame.extend_from_slice(&[0; 6]);
        frame.extend_from_slice(&wanted);
        frame.resize(60, 0);
        frame
    }

    #[test]
    fn addresses_are_read_and_written() {
        assert_eq!(ip_of(b"192.168.1.20"), Some([192, 168, 1, 20]));
        assert_eq!(ip_of(b"10.0.2.2"), Some([10, 0, 2, 2]));
        for bad in ["192.168.1", "192.168.1.256", "1.2.3.4.5", "a.b.c.d", "1..2.3", "", "1.2.3.", "1.2.3.0004"] {
            assert_eq!(ip_of(bad.as_bytes()), None, "{bad}");
        }
        assert_eq!(ip_text([10, 0, 2, 15]).bytes(), b"10.0.2.15");
        assert_eq!(mac_text(OURS).bytes(), b"52:54:00:12:34:56");
        // the checksum of the documents that define it: RFC 1071
        assert_eq!(checksum(add(0, &[0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7])), !0xddf2);
        assert_eq!(checksum(add(0, &[1, 2, 3])), sum_of(&[1, 2, 3]));
    }

    #[test]
    fn the_card_answers_who_asks_for_its_address() {
        let mut net = settled();
        let mut wire = Kept::default();
        assert_eq!(net.receive(&arp(true, THEIRS, BOX, US), 10, &mut wire), None);
        assert_eq!(wire.0.len(), 1);
        let answer = &wire.0[0];
        assert_eq!(answer.len(), 60);
        assert_eq!((&answer[..6], &answer[6..12], &answer[12..14]), (&THEIRS[..], &OURS[..], &[8u8, 6][..]));
        // an answer: our card and address, then theirs
        assert_eq!(answer[20..22], [0, 2]);
        assert_eq!((&answer[22..28], &answer[28..32], &answer[32..38], &answer[38..42]), (&OURS[..], &US[..], &THEIRS[..], &BOX[..]));
        // not for an address that is not ours, nor without one
        net.receive(&arp(true, THEIRS, BOX, [192, 168, 1, 21]), 10, &mut wire);
        let mut new = Net::new();
        new.start(OURS, 0);
        new.receive(&arp(true, THEIRS, BOX, NOWHERE), 10, &mut wire);
        assert_eq!(wire.0.len(), 1);
        // nor a frame for another card
        let mut other = arp(true, THEIRS, BOX, US);
        other[..6].copy_from_slice(&[2, 2, 2, 2, 2, 2]);
        net.receive(&other, 10, &mut wire);
        assert_eq!((wire.0.len(), net.received), (1, 2));
    }

    #[test]
    fn a_ping_is_answered() {
        let mut net = settled();
        let mut wire = Kept::default();
        let mut ping = vec![8, 0, 0, 0, 0xab, 0xcd, 0, 7];
        ping.extend_from_slice(b"abcdefghijklmnopqrstuvwabcdefghi");
        let sum = sum_of(&ping);
        ping[2..4].copy_from_slice(&sum.to_be_bytes());
        net.receive(&packet(OURS, THEIRS, BOX, US, 1, &ping), 10, &mut wire);
        // the card that asked is known by now: the answer goes straight to it
        assert_eq!(wire.0.len(), 1);
        let answer = &wire.0[0];
        assert_eq!((&answer[..6], &answer[6..12]), (&THEIRS[..], &OURS[..]));
        assert_eq!(sum_of(&answer[14..34]), 0, "the header is whole");
        assert_eq!((&answer[26..30], &answer[30..34], answer[23]), (&US[..], &BOX[..], 1));
        let data = &answer[34..34 + ping.len()];
        assert_eq!((data[0], sum_of(data)), (0, 0));
        assert_eq!(data[4..], ping[4..]);
        // a ping that was damaged on the way is not
        let mut bad = packet(OURS, THEIRS, BOX, US, 1, &ping);
        bad[40] ^= 1;
        net.receive(&bad, 10, &mut wire);
        let mut bad = packet(OURS, THEIRS, BOX, US, 1, &ping);
        bad[15] ^= 1;
        net.receive(&bad, 10, &mut wire);
        // nor one for another address, nor a piece of a packet
        net.receive(&packet(OURS, THEIRS, BOX, [192, 168, 1, 21], 1, &ping), 10, &mut wire);
        let mut piece = packet(OURS, THEIRS, BOX, US, 1, &ping);
        piece[20] = 0x20;
        net.receive(&piece, 10, &mut wire);
        assert_eq!(wire.0.len(), 1);
    }

    /// The answer to a ping that was sent: the same back, as type 0.
    pub(crate) fn echo(sent: &[u8], from: Ip) -> Vec<u8> {
        let mut data = sent[34..58].to_vec();
        data[0] = 0;
        data[2..4].fill(0);
        let sum = sum_of(&data);
        data[2..4].copy_from_slice(&sum.to_be_bytes());
        packet(OURS, THEIRS, from, US, 1, &data)
    }

    #[test]
    fn pings_are_followed_side_by_side() {
        let mut net = settled();
        let mut wire = Kept::default();
        // the card of the gateway is known: nothing waits for it
        net.receive(&arp(false, THEIRS, BOX, US), 1, &mut wire);
        assert!(!net.holding(1));
        let first = net.ping([8, 8, 8, 8], 100, &mut wire).unwrap();
        let second = net.ping([9, 9, 9, 9], 105, &mut wire).unwrap();
        assert_ne!(first, second);
        let sent: Vec<Vec<u8>> = wire.0.clone();
        // the answers cross: each ping has its own, counted from when it left
        net.receive(&echo(&sent[1], [9, 9, 9, 9]), 110, &mut wire);
        assert_eq!((net.pinged(first), net.pinged(second)), (Some(None), Some(Some(5))));
        // an answer from another machine than the one that was asked is none
        net.receive(&echo(&sent[0], [9, 9, 9, 9]), 115, &mut wire);
        assert_eq!(net.pinged(first), Some(None));
        net.receive(&echo(&sent[0], [8, 8, 8, 8]), 120, &mut wire);
        assert_eq!(net.pinged(first), Some(Some(20)));
        // eight are followed: one more takes the place of the oldest
        for n in 0..PINGS as u64 - 1 {
            net.ping([8, 8, 4, 4], 200 + n, &mut wire).unwrap();
        }
        assert_eq!((net.pinged(first), net.pinged(second)), (None, Some(Some(5))));
        // packets wait for the address of a card side by side, four of them
        let before = wire.0.len();
        for last in 7..7 + HELD as u8 {
            assert!(!net.holding(300));
            net.ping([192, 168, 1, last], 300, &mut wire);
        }
        assert_eq!(wire.0.len(), before + HELD, "each asked whose card it is");
        // with every place taken it is said, so that one more does not push one out...
        assert!(net.holding(300) && net.holding(399));
        // ...for a second: a card that has not answered by then will not
        assert!(!net.holding(400));
        // the cards answer in another order: each packet goes to its own
        let sent = wire.0.len();
        for last in [9u8, 7, 10, 8] {
            net.receive(&arp(false, [2, 0, 0, 0, 0, last], [192, 168, 1, last], US), 401, &mut wire);
        }
        let went: Vec<(u8, u8)> = wire.0[sent..].iter().map(|frame| (frame[5], frame[33])).collect();
        assert_eq!(went, [(9, 9), (7, 7), (10, 10), (8, 8)]);
        assert!(!net.holding(402));
    }

    #[test]
    fn a_ping_is_sent_through_the_gateway_once_its_card_is_known() {
        let mut net = settled();
        let mut wire = Kept::default();
        let number = net.ping([8, 8, 8, 8], 100, &mut wire).unwrap();
        // elsewhere: whose card is the gateway? The ping waits for the answer
        assert_eq!(wire.0.len(), 1);
        assert_eq!((&wire.0[0][..6], &wire.0[0][12..14], &wire.0[0][20..22], &wire.0[0][38..42]), (&EVERYONE[..], &[8u8, 6][..], &[0u8, 1][..], &BOX[..]));
        net.receive(&arp(false, THEIRS, BOX, US), 101, &mut wire);
        assert_eq!(wire.0.len(), 2);
        let sent = wire.0[1].clone();
        assert_eq!((&sent[..6], &sent[30..34], sent[23], sent[34]), (&THEIRS[..], &[8u8, 8, 8, 8][..], 1, 8));
        // a whole frame: from our card, and an IP packet
        assert_eq!((&sent[6..12], &sent[12..14]), (&OURS[..], &[8u8, 0][..]));
        assert_eq!((sum_of(&sent[14..34]), sum_of(&sent[34..58])), (0, 0));
        assert_eq!(net.pinged(number), Some(None));
        // the answer: the same back, as type 0
        let mut data = sent[34..58].to_vec();
        data[0] = 0;
        data[2..4].fill(0);
        let sum = sum_of(&data);
        data[2..4].copy_from_slice(&sum.to_be_bytes());
        // one for another ping does not count
        let mut other = data.clone();
        other[7] ^= 1;
        other[2..4].fill(0);
        let sum = sum_of(&other);
        other[2..4].copy_from_slice(&sum.to_be_bytes());
        net.receive(&packet(OURS, THEIRS, [8, 8, 8, 8], US, 1, &other), 130, &mut wire);
        assert_eq!(net.pinged(number), Some(None));
        net.receive(&packet(OURS, THEIRS, [8, 8, 8, 8], US, 1, &data), 131, &mut wire);
        assert_eq!(net.pinged(number), Some(Some(31)));
        // a machine on the cable is asked for directly, and the next ping needs no asking
        net.ping([192, 168, 1, 7], 200, &mut wire);
        assert_eq!(wire.0[2][38..42], [192, 168, 1, 7]);
        net.ping([8, 8, 4, 4], 201, &mut wire);
        assert_eq!((wire.0.len(), &wire.0[3][..6]), (4, &THEIRS[..]));
        // without an address, or a gateway, nothing leaves
        let mut new = Net::new();
        new.start(OURS, 0);
        assert!(new.ping([8, 8, 8, 8], 0, &mut Kept::default()).is_none());
        net.gateway = NOWHERE;
        assert!(net.ping([8, 8, 8, 8], 300, &mut wire).is_none());
    }

    /// What a server of addresses says: an offer (2), a yes (5) or a no (6).
    fn served(kind: u8, exchange: u32, offered: Ip, seconds: u32) -> Vec<u8> {
        let mut body = vec![0u8; 240];
        body[..4].copy_from_slice(&[2, 1, 6, 0]);
        body[4..8].copy_from_slice(&exchange.to_be_bytes());
        body[16..20].copy_from_slice(&offered);
        body[28..34].copy_from_slice(&OURS);
        body[236..240].copy_from_slice(&[99, 130, 83, 99]);
        body.extend_from_slice(&[53, 1, kind, 54, 4, 192, 168, 1, 1, 51, 4]);
        body.extend_from_slice(&seconds.to_be_bytes());
        body.extend_from_slice(&[1, 4, 255, 255, 255, 0, 0, 3, 8, 192, 168, 1, 1, 192, 168, 1, 2, 6, 4, 9, 9, 9, 9, 255]);
        packet(EVERYONE, THEIRS, BOX, EVERY_IP, 17, &udp(BOX, EVERY_IP, 67, 68, &body))
    }

    /// The options of what a client asked, by their number.
    fn asked(frame: &[u8]) -> (Vec<(u8, Vec<u8>)>, u32) {
        assert_eq!((&frame[..6], &frame[26..30], &frame[30..34], frame[23]), (&EVERYONE[..], &NOWHERE[..], &EVERY_IP[..], 17));
        assert_eq!((be16(frame, 34), be16(frame, 36)), (68, 67));
        assert_eq!(sum_of(&frame[14..34]), 0);
        let length = be16(frame, 38) as usize;
        let mut all = vec![0, 0, 0, 0, 255, 255, 255, 255, 0, 17, frame[38], frame[39]];
        all.extend_from_slice(&frame[34..34 + length]);
        assert_eq!(sum_of(&all), 0, "the checksum of the message");
        let body = &frame[42..];
        assert_eq!((&body[..4], &body[28..34], &body[236..240]), (&[1u8, 1, 6, 0][..], &OURS[..], &[99u8, 130, 83, 99][..]));
        let mut options = Vec::new();
        let mut at = 240;
        while body[at] != 255 {
            options.push((body[at], body[at + 2..at + 2 + body[at + 1] as usize].to_vec()));
            at += 2 + body[at + 1] as usize;
        }
        (options, be32(body, 4))
    }

    #[test]
    fn an_address_is_asked_for_taken_and_asked_again() {
        let mut net = Net::new();
        let mut wire = Kept::default();
        net.tick(0, &mut wire);
        assert!(wire.0.is_empty(), "no card, nothing to ask");
        net.start(OURS, 50);
        net.tick(50, &mut wire);
        let (options, exchange) = asked(&wire.0[0]);
        assert_eq!(options[0], (53, vec![1]));
        assert!(options.contains(&(55, vec![1, 3, 6, 51])) && options.contains(&(12, b"xilaos".to_vec())));
        // nobody answers: again after two seconds, then four, then eight and eight
        for tick in 51..2500 {
            net.tick(tick, &mut wire);
        }
        assert_eq!(wire.0.len(), 5);
        // an offer for somebody else's exchange is not ours
        net.receive(&served(2, exchange ^ 1, US, 600), 2500, &mut wire);
        assert_eq!(wire.0.len(), 5);
        net.receive(&served(2, exchange, US, 600), 2500, &mut wire);
        let (options, same) = asked(&wire.0[5]);
        assert_eq!(same, exchange);
        assert_eq!(options[..3], [(53, vec![3]), (50, US.to_vec()), (54, BOX.to_vec())]);
        assert_eq!(net.ip, None, "not ours until the server says yes");
        // yes: for 600 seconds
        net.receive(&served(5, exchange, US, 600), 2510, &mut wire);
        assert_eq!((net.ip, net.mask, net.gateway), (Some(US), [255, 255, 255, 0], BOX));
        assert_eq!(net.names.server, Some(([9, 9, 9, 9], 53)), "the name server came with the address");
        assert_eq!(net.held_until(), Some(2510 + 60_000));
        // at half that time it is asked for again, with the address in the request
        for tick in 2511..=2510 + 30_000 {
            net.tick(tick, &mut wire);
        }
        assert_eq!(wire.0.len(), 7);
        let (options, _) = asked(&wire.0[6]);
        assert_eq!(options[0], (53, vec![3]));
        assert_eq!(wire.0[6][42 + 12..42 + 16], US);
        net.receive(&served(5, exchange, US, 600), 32_600, &mut wire);
        assert_eq!(net.held_until(), Some(32_600 + 60_000));
        // a server that stops answering: asked every minute, and at the end the address is given up
        for tick in 32_601..=32_600 + 60_000 {
            net.tick(tick, &mut wire);
        }
        assert_eq!(net.ip, None);
        // five times in the second half of the lease
        assert_eq!(wire.0.len(), 12);
        // a no starts everything again
        let mut net = settled();
        net.exchange = 7;
        net.receive(&served(6, 7, NOWHERE, 0), 10, &mut wire);
        assert_eq!(net.ip, None);
    }

    #[test]
    fn a_name_is_asked_of_the_server_the_network_named() {
        use crate::dns::{Failed, Lookup};
        const SERVER: Ip = [192, 168, 1, 3];
        let mut net = settled();
        let mut wire = Kept::default();
        // the network named nobody to ask
        assert_eq!(net.name(b"example.org", 10, &mut wire), Lookup::Failed(Failed::NoServer));
        net.names.told(SERVER);
        net.learn(SERVER, THEIRS, 1);
        assert_eq!(net.name(b"example.org", 10, &mut wire), Lookup::Asking);
        let sent = wire.0[0].clone();
        let (port, length) = (be16(&sent, 34), be16(&sent, 38) as usize - 8);
        assert_eq!((&sent[..6], &sent[30..34], be16(&sent, 36)), (&THEIRS[..], &SERVER[..], 53));
        let reply = crate::dns::tests::served(&sent[42..42 + length], 0, &[(1, 60, &[93, 184, 216, 34])]);
        // what comes to that port from somebody else is not an answer, and is nobody's message either
        let frame = packet(OURS, THEIRS, [192, 168, 1, 7], US, 17, &udp([192, 168, 1, 7], US, 53, port, &reply));
        assert_eq!(net.receive(&frame, 20, &mut wire), None);
        assert_eq!(net.name(b"example.org", 20, &mut wire), Lookup::Asking);
        let frame = packet(OURS, THEIRS, SERVER, US, 17, &udp(SERVER, US, 53, port, &reply));
        assert_eq!(net.receive(&frame, 30, &mut wire), None);
        assert_eq!(net.name(b"example.org", 31, &mut wire), Lookup::Found([93, 184, 216, 34]));
        assert_eq!(wire.0.len(), 1, "asked once");
        // once the answer has come, that port is like any other
        assert!(net.receive(&frame, 32, &mut wire).is_some());
        // without an address of our own nothing can be asked, and nothing stays half asked
        net.ip = None;
        assert_eq!(net.name(b"other.example", 40, &mut wire), Lookup::Failed(Failed::NoServer));
        net.ip = Some(US);
        assert_eq!(net.name(b"other.example", 41, &mut wire), Lookup::Asking);
        assert_eq!(wire.0.len(), 2);
        // a card that is set up again forgets the names and who gave them
        net.start(OURS, 50);
        assert_eq!(net.names.server, None);
    }

    #[test]
    fn a_message_reaches_a_port_and_one_is_sent() {
        let mut net = settled();
        let mut wire = Kept::default();
        let frame = packet(OURS, THEIRS, [192, 168, 1, 7], US, 17, &udp([192, 168, 1, 7], US, 40_000, 2600, b"hello"));
        assert_eq!(net.receive(&frame, 10, &mut wire), Some(Message { from: [192, 168, 1, 7], from_port: 40_000, port: 2600, bytes: b"hello" }));
        // a damaged one does not, nor one for another address
        let mut bad = frame.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(net.receive(&bad, 10, &mut wire), None);
        let other = packet(OURS, THEIRS, [192, 168, 1, 7], [192, 168, 1, 21], 17, &udp([192, 168, 1, 7], [192, 168, 1, 21], 40_000, 2600, b"hello"));
        assert_eq!(net.receive(&other, 10, &mut wire), None);
        // the answer goes straight back: the card was learnt from the message
        assert!(net.message([192, 168, 1, 7], 40_000, 2600, b"hello yourself", 11, &mut wire));
        assert_eq!(wire.0.len(), 1);
        let sent = &wire.0[0];
        assert_eq!((&sent[..6], be16(sent, 34), be16(sent, 36), &sent[42..56]), (&THEIRS[..], 2600, 40_000, &b"hello yourself"[..]));
        let mut all = Vec::new();
        all.extend_from_slice(&sent[26..34]);
        all.extend_from_slice(&[0, 17, sent[38], sent[39]]);
        all.extend_from_slice(&sent[34..56]);
        assert_eq!((sum_of(&all), sum_of(&sent[14..34])), (0, 0));
        // too long for one frame
        assert!(!net.message([192, 168, 1, 7], 1, 2, &[0; 1473], 11, &mut wire));
        assert!(net.message([192, 168, 1, 7], 1, 2, &[0; 1472], 11, &mut wire));
        assert_eq!(wire.0[1].len(), 1514);
    }

    /// A TCP segment as another machine would write it, with its checksum
    /// made the slow way.
    fn segment(from: Ip, to: Ip, ports: (u16, u16), number: u32, acknowledged: u32, flags: u8, bytes: &[u8]) -> Vec<u8> {
        let mut segment = Vec::new();
        segment.extend_from_slice(&ports.0.to_be_bytes());
        segment.extend_from_slice(&ports.1.to_be_bytes());
        segment.extend_from_slice(&number.to_be_bytes());
        segment.extend_from_slice(&acknowledged.to_be_bytes());
        segment.extend_from_slice(&[5 << 4, flags, 0x10, 0, 0, 0, 0, 0]);
        segment.extend_from_slice(bytes);
        let mut summed = Vec::new();
        summed.extend_from_slice(&from);
        summed.extend_from_slice(&to);
        summed.extend_from_slice(&[0, 6, (segment.len() >> 8) as u8, segment.len() as u8]);
        summed.extend_from_slice(&segment);
        let sum = sum_of(&summed);
        segment[16..18].copy_from_slice(&sum.to_be_bytes());
        segment
    }

    #[test]
    fn a_conversation_goes_over_the_cable() {
        const MAC_ROOM: Ip = [192, 168, 1, 30];
        let mut net = Net::new();
        let mut wire = Kept::default();
        net.start(OURS, 0);
        (net.ip, net.mask) = (Some(US), [255, 255, 255, 0]);
        assert!(net.tcp.listen(80));
        let ports = (40000, 80);
        // what we put on the cable for them: a frame for their card, a packet of kind 6 whose sums are right
        let ours = |frame: &[u8]| -> (u8, u32, u32, Vec<u8>) {
            assert_eq!((&frame[..6], &frame[6..12], &frame[12..14], frame[23]), (&THEIRS[..], &OURS[..], &[8u8, 0][..], 6));
            assert_eq!((&frame[26..30], &frame[30..34]), (&US[..], &MAC_ROOM[..]));
            assert_eq!(sum_of(&frame[14..34]), 0);
            let total = be16(frame, 16) as usize;
            let segment = &frame[34..14 + total];
            let mut summed = Vec::new();
            summed.extend_from_slice(&frame[26..34]);
            summed.extend_from_slice(&[0, 6, (segment.len() >> 8) as u8, segment.len() as u8]);
            summed.extend_from_slice(segment);
            assert_eq!(sum_of(&summed), 0, "the checksum of a segment");
            assert_eq!((be16(segment, 0), be16(segment, 2)), (80, 40000));
            let header = (segment[12] >> 4) as usize * 4;
            (segment[13], be32(segment, 4), be32(segment, 8), segment[header..].to_vec())
        };
        // they call: we answer at once, to the card the call came from
        net.receive(&packet(OURS, THEIRS, MAC_ROOM, US, 6, &segment(MAC_ROOM, US, ports, 5000, 0, 0x02, b"")), 1, &mut wire);
        assert_eq!(wire.0.len(), 1);
        let (flags, first, acknowledged, _) = ours(&wire.0[0]);
        assert_eq!((flags, acknowledged), (0x12, 5001));
        net.receive(&packet(OURS, THEIRS, MAC_ROOM, US, 6, &segment(MAC_ROOM, US, ports, 5001, first + 1, 0x10, b"")), 2, &mut wire);
        let id = net.tcp.accept(80).unwrap();
        // they speak; it is acknowledged without the program doing anything
        net.receive(&packet(OURS, THEIRS, MAC_ROOM, US, 6, &segment(MAC_ROOM, US, ports, 5001, first + 1, 0x18, b"hello")), 3, &mut wire);
        assert_eq!(wire.0.len(), 2);
        assert_eq!(ours(&wire.0[1]), (0x10, first + 1, 5006, Vec::new()));
        let mut read = [0u8; 16];
        assert_eq!(net.tcp.read(id, &mut read), 5);
        // the program answers: it goes when the network is told to speak
        net.tcp.write(id, b"hello yourself");
        assert_eq!(wire.0.len(), 2);
        net.speak(4, &mut wire);
        assert_eq!(ours(&wire.0[2]), (0x18, first + 1, 5006, b"hello yourself".to_vec()));
        // not acknowledged: the time that passes sends it again
        for now in 5..60 {
            net.tick(now, &mut wire);
        }
        // (the ticks also ask for an address, which this test gave by hand: only what is for them counts)
        let again: Vec<_> = wire.0[3..].iter().filter(|frame| frame[12..14] == [8, 0] && frame[23] == 6).collect();
        assert_eq!(again.len(), 1);
        assert_eq!(ours(again[0]), (0x18, first + 1, 5006, b"hello yourself".to_vec()));
        // a segment for another address than ours, or damaged, is not ours to answer
        let before = wire.0.len();
        net.receive(&packet(OURS, THEIRS, MAC_ROOM, [192, 168, 1, 21], 6, &segment(MAC_ROOM, [192, 168, 1, 21], ports, 1, 0, 0x02, b"")), 61, &mut wire);
        let mut damaged = packet(OURS, THEIRS, MAC_ROOM, US, 6, &segment(MAC_ROOM, US, (40001, 80), 1, 0, 0x02, b""));
        damaged[40] ^= 0x40;
        net.receive(&damaged, 61, &mut wire);
        assert_eq!(wire.0.len(), before);
    }
}
