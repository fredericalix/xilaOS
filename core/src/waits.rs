//! Waits: what a job asked of the network, going on while the system lives.
//!
//! `net ping`, `net name`, `net get`, `net post` and `install from` ask
//! another machine something and need its answer. A command cannot wait:
//! it leaves a `shell::Request`. Here that request becomes a *wait*,
//! kept at the place of the job that asked (`System::waits`): what was
//! asked, the machine that asks it (a ping, a name, a page over TCP, a
//! file over TFTP), then what came of it. The job stays `Waiting` and
//! the others run. The kernel moves every wait a step again and again
//! (`step`), which needs the card and the time and nothing else; when
//! one is done, who gives its job turns takes what came of it (`take`),
//! and `shell::served` says it where the job says.
//!
//! Waits go on side by side, each with its own conversation. One thing
//! is alone: there is one room for a system that is fetched, so a second
//! `install from` waits for the first.
//!
//! What runs at once (the prompt over the network, what the companion
//! runs, a system with no rooms) has a place of its own, `AT_ONCE`, and
//! the kernel steps it to its end before it does anything else.
//!
//! Two things a wait needs are the kernel's to keep, since they take
//! memory of its own: the TLS client of a page asked in secret, given to
//! `step` each time, and the room of a fetched system.

use crate::ask::{self, Ask};
use crate::dns;
use crate::jobs;
use crate::net::{Ip, Message, Wire};
use crate::shell::{Got, Request, System, Text};
use crate::text::FixedStr;
use crate::tftp;

/// A wait for each place of a job, and one for what runs at once.
pub const PLACES: usize = jobs::PLACES + 2;
/// The place of the wait of what runs at once.
pub const AT_ONCE: usize = jobs::PLACES + 1;
/// How long a ping waits for its answer, in microseconds.
const PING: u64 = 1_000_000;
/// How long a fetch may take, in microseconds: far more than a system
/// takes on a local network.
const FETCH: u64 = 60_000_000;
/// Bytes kept of what a page says: its start.
const START: usize = 640;

/// What the kernel could not give a wait.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trouble {
    /// No card to ask through.
    NoCard,
    /// Nothing to draw the numbers of a secret conversation from.
    NoChance,
    /// No clock to read the dates of certificates against.
    NoClock,
    /// No memory to keep a fetched system in.
    NoMemory,
}

/// What came of a wait, as `shell::served` will say it.
#[derive(Clone, Copy)]
enum Outcome {
    Pinged(Ip, Option<u32>),
    Named(Result<Ip, dns::Failed>),
    Got(u16, Result<Got, ask::Failed>),
    Fetched(Ip, Result<(usize, u32), tftp::Failed>),
}

#[derive(Clone, Copy)]
enum Stage {
    /// The number the ping is followed by, and when it left, once it did.
    Ping { to: Ip, sent: Option<(u16, u64)> },
    Name,
    /// A page: the address once it is known, then the conversation.
    /// `since` is the tick it began to wait for a conversation to be free.
    Page { to: Option<Ip>, port: u16, secret: bool, post: bool, ask: Option<Ask>, since: u64 },
    /// A fetch: going once the room for a system is its own.
    Fetch { server: Ip, fetch: Option<tftp::Fetch>, began: u64 },
    Done(Outcome),
}

/// One thing asked of the network, and how far it is.
#[derive(Clone, Copy)]
pub struct Wait {
    stage: Stage,
    /// The machine as it was written, the path and the text that goes
    /// along: what the command left in `System` when it asked.
    machine: dns::Name,
    path: FixedStr<200>,
    text: Text,
    kept: [u8; START],
}

pub struct Waits {
    list: [Option<Wait>; PLACES],
}

impl Waits {
    pub const fn new() -> Waits {
        Waits { list: [None; PLACES] }
    }
}

impl Default for Waits {
    fn default() -> Waits {
        Waits::new()
    }
}

/// Takes the request a command left, if it is one that waits for the
/// network, and makes it the wait of a place. False for any other
/// request, which is left where it is. `micros` is the fine clock.
pub fn begin(sys: &mut System, at: usize, micros: u64) -> bool {
    let stage = match sys.request {
        Some(Request::Ping(to)) => Stage::Ping { to, sent: None },
        Some(Request::Name) => Stage::Name,
        Some(Request::Ask { to, port, secret, post }) => Stage::Page { to, port, secret, post, ask: None, since: sys.ticks },
        Some(Request::Fetch(server)) => Stage::Fetch { server, fetch: None, began: micros },
        _ => return false,
    };
    sys.request = None;
    // one that was there is hung up: its place has another to ask
    drop(sys, at);
    sys.waits.list[at] = Some(Wait { stage, machine: sys.asked_machine, path: sys.asked_path, text: sys.asked_text, kept: [0; START] });
    true
}

/// Whether the wait of a place still asks: its outcome is not there yet.
pub fn going(sys: &System, at: usize) -> bool {
    sys.waits.list.get(at).is_some_and(|wait| wait.is_some_and(|wait| !matches!(wait.stage, Stage::Done(_))))
}

/// Whether the wait of a place has its outcome, to be taken.
pub fn done(sys: &System, at: usize) -> bool {
    sys.waits.list.get(at).is_some_and(|wait| wait.is_some_and(|wait| matches!(wait.stage, Stage::Done(_))))
}

/// Whether anything is being asked.
pub fn any(sys: &System) -> bool {
    (0..PLACES).any(|at| going(sys, at))
}

/// Whether a wait wants the card read without rest: a ping, whose time
/// is told, or a fetch, which goes a block at each answer.
pub fn hurried(sys: &System) -> bool {
    sys.waits.list.iter().flatten().any(|wait| matches!(wait.stage, Stage::Ping { .. } | Stage::Fetch { fetch: Some(_), .. }))
}

/// The machine a place asks a page of through TLS, as it was written:
/// the kernel keeps a client for it, and gives it to `step`.
pub fn secret(sys: &System, at: usize) -> Option<dns::Name> {
    let wait = sys.waits.list.get(at)?.as_ref()?;
    matches!(wait.stage, Stage::Page { secret: true, .. }).then_some(wait.machine)
}

/// Whether a system is to be fetched: the kernel keeps a room for it.
pub fn fetching(sys: &System) -> bool {
    sys.waits.list.iter().flatten().any(|wait| matches!(wait.stage, Stage::Fetch { .. }))
}

/// Whether a fetch has the room for a system: what was there is no
/// longer a system to install.
pub fn filling(sys: &System) -> bool {
    sys.waits.list.iter().flatten().any(|wait| matches!(wait.stage, Stage::Fetch { fetch: Some(_), .. }))
}

/// What the kernel could not give the wait of a place: it ends there,
/// with what its command says of such a failure.
pub fn fail(sys: &mut System, at: usize, trouble: Trouble) {
    let Some(wait) = sys.waits.list.get_mut(at).and_then(|wait| wait.as_mut()) else {
        return;
    };
    let outcome = match wait.stage {
        Stage::Ping { to, .. } => Outcome::Pinged(to, None),
        Stage::Name => Outcome::Named(Err(dns::Failed::NoServer)),
        Stage::Page { port, .. } => Outcome::Got(
            port,
            Err(match trouble {
                Trouble::NoChance => ask::Failed::NoChance,
                Trouble::NoClock => ask::Failed::NoClock,
                Trouble::NoCard | Trouble::NoMemory => ask::Failed::NoAnswer,
            }),
        ),
        Stage::Fetch { server, .. } => Outcome::Fetched(server, Err(if trouble == Trouble::NoMemory { tftp::Failed::NoMemory } else { tftp::Failed::NoWay })),
        Stage::Done(_) => return,
    };
    hang_up(sys, at);
    if let Some(wait) = sys.waits.list[at].as_mut() {
        wait.stage = Stage::Done(outcome);
    }
}

/// Lets go of what a wait holds in the network: its conversation, the
/// place where its name is asked about.
fn hang_up(sys: &mut System, at: usize) {
    let System { waits, net, .. } = sys;
    let Some(wait) = waits.list.get_mut(at).and_then(|wait| wait.as_mut()) else {
        return;
    };
    match &mut wait.stage {
        Stage::Page { ask: Some(ask), .. } => ask.drop_it(&mut net.tcp),
        Stage::Page { to: None, .. } | Stage::Name => net.names.give_up(wait.machine.bytes()),
        _ => {}
    }
}

/// The wait of a place is given up: who asked no longer wants the
/// answer. Its conversation is hung up, and nothing is said.
pub fn drop(sys: &mut System, at: usize) {
    hang_up(sys, at);
    if let Some(wait) = sys.waits.list.get_mut(at) {
        *wait = None;
    }
}

/// Takes what came of the wait of a place and puts it where
/// `shell::served` reads the kernel's answers, with the machine that was
/// asked. True when there was something: it is then to be said.
pub fn take(sys: &mut System, at: usize) -> bool {
    let Some(Wait { stage: Stage::Done(outcome), machine, .. }) = sys.waits.list.get(at).copied().flatten() else {
        return false;
    };
    sys.waits.list[at] = None;
    match outcome {
        Outcome::Pinged(to, took) => sys.pinged = Some((to, took)),
        Outcome::Named(found) => (sys.asked_machine, sys.named) = (machine, Some(found)),
        Outcome::Got(port, got) => (sys.asked_machine, sys.got) = (machine, Some((port, got))),
        Outcome::Fetched(server, fetched) => sys.fetched = Some((server, fetched)),
    }
    true
}

/// A message came for the port a fetch listens to: the fetch that is
/// going takes it, its bytes going into `room`. Returns the length of
/// the system when this ended the fetch well: the kernel makes it the
/// one `install` writes.
pub fn heard(sys: &mut System, message: &Message, room: &mut [u8], now: u64, wire: &mut dyn Wire) -> Option<usize> {
    let System { waits, net, .. } = sys;
    let wait = waits.list.iter_mut().flatten().find(|wait| matches!(wait.stage, Stage::Fetch { fetch: Some(_), .. }))?;
    let Stage::Fetch { server, fetch: Some(fetch), .. } = &mut wait.stage else {
        return None;
    };
    let (server, step) = (*server, fetch.message(message, room, now, net, wire));
    fetched(wait, server, step, room)
}

/// What a step of a fetch came to.
fn fetched(wait: &mut Wait, server: Ip, step: tftp::Step, room: &[u8]) -> Option<usize> {
    let (outcome, length) = match step {
        tftp::Step::Going => return None,
        tftp::Step::Failed(why) => (Err(why), None),
        tftp::Step::Done(length) => match room.get(..length).filter(|file| tftp::is_system(file)) {
            Some(file) => (Ok((length, crate::gpt::crc32(0, file))), Some(length)),
            None => (Err(tftp::Failed::NotASystem), None),
        },
    };
    wait.stage = Stage::Done(Outcome::Fetched(server, outcome));
    length
}

/// Moves the wait of a place one step. `now` is the time of the network
/// in ticks, `micros` the fine clock; `tls` is the client of a page
/// asked in secret, which waits until it is given one; `room` is where a
/// fetched system goes, empty while the kernel has none. Returns the
/// length of the system when a fetch just ended well.
pub fn step(sys: &mut System, at: usize, wire: &mut dyn Wire, now: u64, micros: u64, tls: Option<&mut xila_tls::Client>, room: &mut [u8]) -> Option<usize> {
    // one room for a system: a fetch waits while another has it
    let taken = filling(sys);
    let System { waits, net, .. } = sys;
    let wait = waits.list.get_mut(at)?.as_mut()?;
    let Wait { stage, machine, path, text, kept } = wait;
    let outcome = match stage {
        Stage::Done(_) => return None,
        Stage::Ping { to, sent: None } => {
            // a packet that waits for the address of a card would lose its place
            if net.holding() {
                return None;
            }
            match net.ping(*to, now, wire) {
                Some(number) => {
                    *stage = Stage::Ping { to: *to, sent: Some((number, micros)) };
                    return None;
                }
                None => Outcome::Pinged(*to, None),
            }
        }
        Stage::Ping { to, sent: Some((number, left)) } => match net.pinged(*number) {
            Some(Some(_)) => Outcome::Pinged(*to, Some(micros.saturating_sub(*left).min(u32::MAX as u64) as u32)),
            _ if micros.saturating_sub(*left) >= PING => Outcome::Pinged(*to, None),
            _ => return None,
        },
        Stage::Name => match net.name(machine.bytes(), now, wire) {
            dns::Lookup::Found(ip) => Outcome::Named(Ok(ip)),
            dns::Lookup::Failed(why) => Outcome::Named(Err(why)),
            dns::Lookup::Asking => return None,
        },
        Stage::Page { to, port, secret, post, ask, since } => {
            if *secret && tls.is_none() {
                return None;
            }
            let Some(address) = *to else {
                match net.name(machine.bytes(), now, wire) {
                    dns::Lookup::Found(ip) => *to = Some(ip),
                    dns::Lookup::Failed(why) => *stage = Stage::Done(Outcome::Got(*port, Err(ask::Failed::Name(why)))),
                    dns::Lookup::Asking => {}
                }
                return None;
            };
            let question = if *post { ask::Question::post(text.bytes()) } else { ask::Question::GET };
            let Some(asking) = ask else {
                // one more conversation than there are places waits for one to end
                if !net.tcp.has_room() && now < *since + ask::PATIENCE {
                    return None;
                }
                let place = ask::Place { machine: machine.bytes(), ip: Some(address), port: *port, path: path.bytes(), secret: *secret };
                match Ask::start(&mut net.tcp, address, &place, &question, now) {
                    Ok(started) => *ask = Some(started),
                    Err(why) => *stage = Stage::Done(Outcome::Got(*port, Err(why))),
                }
                return None;
            };
            match asking.step(&mut net.tcp, tls, question.bytes, kept, now) {
                ask::Step::Going => return None,
                ask::Step::Done(status) => Outcome::Got(*port, Ok(Got { status, start: FixedStr::from_bytes(&kept[..asking.kept]), total: asking.total })),
                ask::Step::Failed(why) => Outcome::Got(*port, Err(why)),
            }
        }
        Stage::Fetch { server, fetch: None, began } => {
            if taken || room.is_empty() {
                return None;
            }
            *stage = Stage::Fetch { server: *server, fetch: Some(tftp::Fetch::start(*server, now, net, wire)), began: *began };
            return None;
        }
        Stage::Fetch { server, fetch: Some(fetch), began } => {
            let (server, late) = (*server, micros.saturating_sub(*began) > FETCH);
            let step = match fetch.tick(now, net, wire) {
                tftp::Step::Going if late => tftp::Step::Failed(tftp::Failed::Silent),
                step => step,
            };
            return fetched(wait, server, step, room);
        }
    };
    *stage = Stage::Done(outcome);
    None
}

/// Whether a line typed alone is a command that waits for the network:
/// where a prompt can wait for a job, it becomes one.
pub fn asks(line: &[u8]) -> bool {
    let (first, rest) = crate::text::split_word(crate::text::trim(line));
    let (second, _) = crate::text::split_word(crate::text::trim(rest));
    let is = |word: &[u8], wanted: &[u8]| word.eq_ignore_ascii_case(wanted);
    (is(first, b"net") && [&b"ping"[..], b"name", b"get", b"post"].iter().any(|wanted| is(second, wanted))) || (is(first, b"install") && is(second, b"from"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::net::tests::{arp, echo, settled, Kept, BOX, THEIRS as CARD, US};
    use crate::shell::tests::Lines;
    use crate::tcp::tests::{Peer, THEIRS};
    use crate::tcp::{ACK, FIN, PUSH, SYN};

    /// A system with an address on the network, that knows the card of
    /// the machine that passes its packets on.
    pub(crate) fn networked() -> Box<System> {
        let mut sys = Box::new(System::new());
        sys.net = settled();
        sys.net.receive(&arp(false, CARD, BOX, US), 1, &mut Kept::default());
        sys.net_card = Some(crate::shell::NetCard { name: "Intel 82574L", link: Some((1000, true)), damaged: 0, missed: 0, sent: 0, refused: 0 });
        sys
    }

    /// Leaves the request a command typed would, and makes it the wait of a place.
    fn ask(sys: &mut System, at: usize, line: &str) {
        let mut out = Lines::default();
        assert!(crate::shell::command(sys, line.as_bytes(), &mut out), "{line}: {:?}", out.lines);
        assert!(begin(sys, at, 0), "{line}");
        assert!(going(sys, at) && sys.request.is_none());
    }

    /// One step of the wait of a place, with no secret and no room.
    fn pass(sys: &mut System, at: usize, wire: &mut Kept, now: u64, micros: u64) {
        assert_eq!(step(sys, at, wire, now, micros, None, &mut []), None);
    }

    /// What the wait of a place came to, as its command says it.
    fn said(sys: &mut System, at: usize) -> Vec<String> {
        assert!(take(sys, at));
        let mut out = Lines::default();
        crate::shell::served(sys, &mut out);
        out.lines
    }

    /// A machine that answers one conversation on a port: it takes our
    /// call, hears the question, and answers and hangs up when told to.
    struct Server {
        peer: Peer,
        open: bool,
        asked: Vec<u8>,
        answered: bool,
    }

    impl Server {
        fn new(port: u16) -> Server {
            Server { peer: Peer::new(port, 0), open: false, asked: Vec::new(), answered: false }
        }
    }

    /// One round of the machines of a test: each hears what we sent to
    /// its port, takes our call, and answers the question it heard whole
    /// if it has an answer to give.
    fn round(sys: &mut System, now: u64, servers: &mut [(&mut Server, Option<&[u8]>)]) {
        let mut segment = [0u8; crate::tcp::SEGMENT];
        while let Some((_, length)) = sys.net.tcp.output(crate::tcp::tests::OURS, now, &mut segment) {
            for (server, _) in servers.iter_mut() {
                let Some(got) = server.peer.takes(&segment[..length]) else {
                    continue;
                };
                if got.flags & SYN != 0 && !server.open {
                    (server.peer.to, server.open) = (got.from, true);
                    server.peer.say(&mut sys.net.tcp, SYN | ACK, b"", now);
                }
                server.asked.extend_from_slice(&got.bytes);
            }
        }
        for (server, answer) in servers.iter_mut() {
            let whole = server.asked.windows(4).any(|part| part == b"\r\n\r\n");
            if let (Some(answer), true, false) = (answer, server.open && whole, server.answered) {
                server.peer.say(&mut sys.net.tcp, ACK | PUSH, answer, now);
                server.peer.say(&mut sys.net.tcp, FIN | ACK, b"", now);
                server.answered = true;
            }
        }
    }

    const PAGE: &[u8] = b"HTTP/1.0 200 OK\r\nContent-Length: 6\r\n\r\nhello\n";

    #[test]
    fn a_ping_is_answered_or_given_a_second() {
        let mut sys = networked();
        let mut wire = Kept::default();
        ask(&mut sys, 0, "net ping 8.8.8.8");
        pass(&mut sys, 0, &mut wire, 100, 5_000);
        assert_eq!(wire.0.len(), 1, "the ping left");
        pass(&mut sys, 0, &mut wire, 101, 9_000);
        assert!(going(&sys, 0) && !done(&sys, 0) && hurried(&sys));
        // its answer: the time is counted from when it left
        let answer = echo(&wire.0[0], [8, 8, 8, 8]);
        sys.net.receive(&answer, 102, &mut wire);
        pass(&mut sys, 0, &mut wire, 102, 17_500);
        assert!(done(&sys, 0) && !going(&sys, 0) && !any(&sys));
        assert_eq!(said(&mut sys, 0), ["8.8.8.8 answered in 12.5 ms."]);
        // taken once
        assert!(!take(&mut sys, 0) && !done(&sys, 0));
        // one that nobody answers ends after a second
        ask(&mut sys, 3, "net ping 8.8.4.4");
        pass(&mut sys, 3, &mut wire, 200, 2_000_000);
        pass(&mut sys, 3, &mut wire, 250, 2_999_999);
        assert!(going(&sys, 3));
        pass(&mut sys, 3, &mut wire, 300, 3_000_000);
        assert_eq!(said(&mut sys, 3), ["8.8.4.4 did not answer within a second."]);
    }

    #[test]
    fn a_ping_waits_for_a_packet_that_waits_for_a_card() {
        let mut sys = networked();
        let mut wire = Kept::default();
        // two machines on the cable whose cards are not known
        ask(&mut sys, 0, "net ping 192.168.1.7");
        ask(&mut sys, 1, "net ping 192.168.1.8");
        pass(&mut sys, 0, &mut wire, 100, 0);
        pass(&mut sys, 1, &mut wire, 100, 0);
        // the first asked whose card it is; the second did not take its place
        assert_eq!(wire.0.len(), 1);
        sys.net.receive(&arp(false, [2, 0, 0, 0, 0, 7], [192, 168, 1, 7], US), 101, &mut wire);
        pass(&mut sys, 1, &mut wire, 102, 20_000);
        sys.net.receive(&arp(false, [2, 0, 0, 0, 0, 8], [192, 168, 1, 8], US), 103, &mut wire);
        // both pings left in the end, each to its machine
        let pings: Vec<&Vec<u8>> = wire.0.iter().filter(|frame| frame[12..14] == [8, 0] && frame[23] == 1).collect();
        assert_eq!(pings.iter().map(|frame| frame[33]).collect::<Vec<_>>(), [7, 8]);
        let (first, second) = (echo(pings[0], [192, 168, 1, 7]), echo(pings[1], [192, 168, 1, 8]));
        sys.net.receive(&second, 104, &mut wire);
        sys.net.receive(&first, 104, &mut wire);
        pass(&mut sys, 0, &mut wire, 104, 40_000);
        pass(&mut sys, 1, &mut wire, 104, 40_000);
        assert_eq!(said(&mut sys, 1), ["192.168.1.8 answered in 20.0 ms."]);
        assert_eq!(said(&mut sys, 0), ["192.168.1.7 answered in 40.0 ms."]);
    }

    /// Answers the question about a name that is on the wire, if one is.
    fn name_server(sys: &mut System, wire: &mut Kept, name: &str, address: Option<Ip>, now: u64) -> bool {
        let Some(frame) = wire.0.iter().rev().find(|frame| frame.len() > 54 && frame[23] == 17 && frame[36..38] == [0, 53] && frame.windows(name.split('.').next().unwrap().len()).any(|part| part.eq_ignore_ascii_case(name.split('.').next().unwrap().as_bytes()))) else {
            return false;
        };
        let length = u16::from_be_bytes([frame[38], frame[39]]) as usize;
        let (port, question) = (u16::from_be_bytes([frame[34], frame[35]]), &frame[42..34 + length]);
        let reply = match address {
            Some(ip) => crate::dns::tests::served(question, 0, &[(1, 30, &ip)]),
            None => crate::dns::tests::served(question, 3, &[]),
        };
        sys.net.names.answered(BOX, 53, port, &reply, now)
    }

    #[test]
    fn names_are_found_side_by_side() {
        let mut sys = networked();
        sys.net.names.told(BOX);
        let mut wire = Kept::default();
        ask(&mut sys, 0, "net name first.example");
        ask(&mut sys, 1, "net name none.example");
        pass(&mut sys, 0, &mut wire, 100, 0);
        pass(&mut sys, 1, &mut wire, 100, 0);
        assert!(going(&sys, 0) && going(&sys, 1) && !hurried(&sys));
        assert!(name_server(&mut sys, &mut wire, "none.example", None, 101));
        assert!(name_server(&mut sys, &mut wire, "first.example", Some([10, 0, 0, 1]), 101));
        pass(&mut sys, 0, &mut wire, 102, 0);
        pass(&mut sys, 1, &mut wire, 102, 0);
        // each is said with its own name, whichever command was typed last
        assert_eq!(said(&mut sys, 0), ["first.example is at 10.0.0.1."]);
        assert_eq!(said(&mut sys, 1).len(), 1);
        assert!(sys.asked_machine.bytes() == b"none.example");
    }

    #[test]
    fn pages_are_asked_side_by_side_and_each_is_said_with_its_machine() {
        let mut sys = networked();
        let mut wire = Kept::default();
        ask(&mut sys, 0, "net get 192.168.1.168:8000/first");
        ask(&mut sys, 1, "net post 192.168.1.168:8001/second 'a note'");
        let (mut first, mut second) = (Server::new(8000), Server::new(8001));
        for now in 10..40 {
            pass(&mut sys, 0, &mut wire, now, 0);
            pass(&mut sys, 1, &mut wire, now, 0);
            // the second answers at once, the first later
            let created: &[u8] = b"HTTP/1.0 201 Created\r\nContent-Length: 5\r\n\r\nkept\n";
            round(&mut sys, now, &mut [(&mut first, (now > 25).then_some(PAGE)), (&mut second, Some(created))]);
            if now <= 25 {
                assert!(going(&sys, 0));
            }
            if now == 25 {
                assert!(done(&sys, 1), "the second did not wait for the first");
            }
        }
        assert!(String::from_utf8_lossy(&first.asked).starts_with("GET /first HTTP/1.1\r\nHost: 192.168.1.168:8000\r\n"));
        assert!(String::from_utf8_lossy(&second.asked).starts_with("POST /second HTTP/1.1\r\n"));
        assert!(second.asked.ends_with(b"\r\n\r\na note"));
        assert_eq!(said(&mut sys, 1), ["192.168.1.168:8001 answered 201.", "kept"]);
        assert_eq!(said(&mut sys, 0), ["192.168.1.168:8000 answered 200.", "hello"]);
    }

    #[test]
    fn a_page_asked_by_a_name_finds_its_address_first() {
        let mut sys = networked();
        sys.net.names.told(BOX);
        let mut wire = Kept::default();
        ask(&mut sys, 0, "net get the-mac.example:8000/notes");
        ask(&mut sys, 1, "net get nowhere.example/");
        let mut server = Server::new(8000);
        for now in 10..40 {
            pass(&mut sys, 0, &mut wire, now, 0);
            pass(&mut sys, 1, &mut wire, now, 0);
            if now == 12 {
                assert!(name_server(&mut sys, &mut wire, "the-mac.example", Some(THEIRS), now));
                assert!(name_server(&mut sys, &mut wire, "nowhere.example", None, now));
            }
            round(&mut sys, now, &mut [(&mut server, Some(PAGE))]);
        }
        assert!(String::from_utf8_lossy(&server.asked).contains("\r\nHost: the-mac.example:8000\r\n"));
        assert_eq!(said(&mut sys, 0), ["the-mac.example:8000 answered 200.", "hello"]);
        // the one whose name nobody has asked nothing of anyone
        let failed = said(&mut sys, 1);
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(!failed[0].contains("answered"));
    }

    #[test]
    fn one_page_more_than_there_are_conversations_waits_for_one_to_end() {
        let mut sys = networked();
        let mut wire = Kept::default();
        // eight conversations are taken by others
        let held: Vec<_> = (0..crate::tcp::CONNECTIONS as u16).map(|n| sys.net.tcp.connect(THEIRS, 9000 + n, 5).unwrap()).collect();
        ask(&mut sys, 0, "net get 192.168.1.168:8000/");
        for now in 10..20 {
            pass(&mut sys, 0, &mut wire, now, 0);
        }
        assert!(going(&sys, 0), "it waits rather than fail");
        sys.net.tcp.abort(held[0]);
        let mut server = Server::new(8000);
        for now in 20..40 {
            pass(&mut sys, 0, &mut wire, now, 0);
            round(&mut sys, now, &mut [(&mut server, Some(PAGE))]);
        }
        assert_eq!(said(&mut sys, 0), ["192.168.1.168:8000 answered 200.", "hello"]);
        // with none that ends, it is given up after the silence a page is given
        ask(&mut sys, 1, "net get 192.168.1.168:8000/");
        while sys.net.tcp.has_room() {
            sys.net.tcp.connect(THEIRS, 9100, 40).unwrap();
        }
        let since = sys.ticks;
        pass(&mut sys, 1, &mut wire, since + crate::ask::PATIENCE - 1, 0);
        assert!(going(&sys, 1));
        pass(&mut sys, 1, &mut wire, since + crate::ask::PATIENCE, 0);
        assert_eq!(said(&mut sys, 1), ["192.168.1.168:8000 cannot be asked now: too many conversations at once."]);
    }

    #[test]
    fn a_wait_that_is_dropped_hangs_up() {
        let mut sys = networked();
        sys.net.names.told(BOX);
        let mut wire = Kept::default();
        // while its conversation goes on
        ask(&mut sys, 0, "net get 192.168.1.168:8000/");
        let mut server = Server::new(8000);
        for now in 10..13 {
            pass(&mut sys, 0, &mut wire, now, 0);
            // it takes the call and hears the question, and says nothing
            round(&mut sys, now, &mut [(&mut server, None)]);
        }
        assert!(server.asked.starts_with(b"GET / HTTP/1.1"));
        assert_eq!(sys.net.tcp.count(), 1);
        drop(&mut sys, 0);
        assert!(!going(&sys, 0) && !done(&sys, 0) && !take(&mut sys, 0));
        assert_eq!(sys.net.tcp.count(), 0, "the conversation is free again");
        // while its name is asked about: the four places are free for others
        for at in 0..crate::dns::ASKED {
            ask(&mut sys, at, &format!("net get gone{at}.example/"));
            pass(&mut sys, at, &mut wire, 20, 0);
            drop(&mut sys, at);
        }
        let asked = sys.net.names.asked;
        ask(&mut sys, 5, "net name here.example");
        pass(&mut sys, 5, &mut wire, 21, 0);
        assert_eq!(sys.net.names.asked, asked + 1, "its question left at once");
        // one that is begun where another was hangs the other up
        ask(&mut sys, 6, "net get 192.168.1.168:8002/");
        pass(&mut sys, 6, &mut wire, 30, 0);
        assert_eq!(sys.net.tcp.count(), 1);
        ask(&mut sys, 6, "net ping 8.8.8.8");
        assert_eq!(sys.net.tcp.count(), 0);
    }

    /// A file that has the shape of a system, of some length.
    fn system_file(length: usize) -> Vec<u8> {
        let mut file = vec![0u8; length];
        file[..2].copy_from_slice(b"MZ");
        file[0x3c] = 0x80;
        file[0x80..0x84].copy_from_slice(b"PE\0\0");
        file[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        file[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
        file[0x98 + 68..0x98 + 70].copy_from_slice(&10u16.to_le_bytes());
        file
    }

    /// Gives the fetch that is going a whole file, as a server would.
    fn serve_file(sys: &mut System, wire: &mut Kept, room: &mut [u8], file: &[u8], now: u64) -> Option<usize> {
        let from = |bytes: &[u8]| -> Vec<u8> { bytes.to_vec() };
        let mut last = None;
        for (number, part) in file.chunks(512).enumerate().chain(core::iter::once((file.len() / 512, &[][..])).filter(|_| file.len() % 512 == 0)) {
            let mut block = vec![0, 3, ((number + 1) >> 8) as u8, (number + 1) as u8];
            block.extend_from_slice(part);
            let bytes = from(&block);
            last = heard(sys, &Message { from: BOX, from_port: 5000, port: tftp::PORT, bytes: &bytes }, room, now, wire);
        }
        last
    }

    #[test]
    fn a_system_is_fetched_and_a_second_fetch_waits_for_the_room() {
        let mut sys = networked();
        let mut wire = Kept::default();
        let mut room = vec![0u8; 8192];
        ask(&mut sys, 0, "install from 192.168.1.1");
        ask(&mut sys, 1, "install from 192.168.1.1");
        assert!(fetching(&sys) && !filling(&sys));
        // without a room nothing is asked
        pass(&mut sys, 0, &mut wire, 10, 0);
        assert!(wire.0.is_empty() && !hurried(&sys));
        assert_eq!(step(&mut sys, 0, &mut wire, 11, 0, None, &mut room), None);
        assert_eq!(step(&mut sys, 1, &mut wire, 11, 0, None, &mut room), None);
        assert_eq!(wire.0.len(), 1, "one asks, the other waits");
        assert!(filling(&sys) && hurried(&sys));
        let file = system_file(1300);
        assert_eq!(serve_file(&mut sys, &mut wire, &mut room, &file, 12), Some(1300));
        assert_eq!(&room[..1300], &file[..]);
        assert!(done(&sys, 0) && going(&sys, 1) && !filling(&sys));
        let told = said(&mut sys, 0);
        assert!(told[0].starts_with("192.168.1.1 gave a system of 1300 bytes, check "), "{told:?}");
        // the second has the room now
        let before = wire.0.len();
        assert_eq!(step(&mut sys, 1, &mut wire, 13, 0, None, &mut room), None);
        assert_eq!(wire.0.len(), before + 1);
        // what it is given is no system: said so, and no length is returned
        assert_eq!(serve_file(&mut sys, &mut wire, &mut room, &[7u8; 700], 14), None);
        assert_eq!(said(&mut sys, 1), ["192.168.1.1 gave something that is not a system for this machine."]);
        // one that takes more than a minute is given up
        ask(&mut sys, 2, "install from 192.168.1.1");
        assert_eq!(step(&mut sys, 2, &mut wire, 15, 0, None, &mut room), None);
        assert_eq!(step(&mut sys, 2, &mut wire, 16, 60_000_001, None, &mut room), None);
        assert_eq!(said(&mut sys, 2), ["192.168.1.1 does not answer. Is tools/netboot.py running there?"]);
    }

    #[test]
    fn what_the_kernel_cannot_give_ends_a_wait_with_its_words() {
        let mut sys = networked();
        ask(&mut sys, 0, "net get https://example.org/");
        assert_eq!(secret(&sys, 0).unwrap().bytes(), b"example.org");
        // it waits for its client: nothing is asked without one
        let mut wire = Kept::default();
        pass(&mut sys, 0, &mut wire, 10, 0);
        assert!(wire.0.is_empty());
        fail(&mut sys, 0, Trouble::NoClock);
        assert!(done(&sys, 0));
        assert_eq!(said(&mut sys, 0).len(), 1);
        ask(&mut sys, 1, "install from 192.168.1.1");
        fail(&mut sys, 1, Trouble::NoMemory);
        assert_eq!(said(&mut sys, 1), ["192.168.1.1 was not asked: there is no memory to keep a system in."]);
        ask(&mut sys, 2, "net get 192.168.1.168/");
        assert_eq!(secret(&sys, 2), None);
        fail(&mut sys, 2, Trouble::NoCard);
        assert_eq!(said(&mut sys, 2), ["192.168.1.168:80 does not answer."]);
        // any other request is not a wait, and stays where it is
        sys.request = Some(Request::Spin);
        assert!(!begin(&mut sys, 0, 0) && sys.request == Some(Request::Spin));
    }

    #[test]
    fn the_commands_that_wait_for_the_network_are_told_from_the_others() {
        for line in ["net ping 10.0.2.2", "  NET  Get example.org/x", "net post a 'b'", "net name example.org", "install from 192.168.1.1"] {
            assert!(asks(line.as_bytes()), "{line}");
        }
        for line in ["net", "net restart", "net registers", "net names 1.1.1.1", "install", "install 1 yes", "install system", "say 'net ping'", "ask what is net get?"] {
            assert!(!asks(line.as_bytes()), "{line}");
        }
    }
}
