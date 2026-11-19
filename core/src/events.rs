//! LISTEN / NOTIFY: how the system tells who wants to know that something
//! changed.
//!
//! A session listens to channels. A channel is a name: the name of a stored
//! table, whose every committed change is announced there by the system, or
//! any other name, on which someone sends with NOTIFY. As in PostgreSQL, an
//! event says little (the channel and a short text, such as `insert 4`); who
//! receives it reads the row with a query. Events are not kept for those
//! who were not listening.
//!
//! A session is one of the prompts: the screen, the serial port, or the
//! one another machine of the network types at (`remote.rs`).
//! Programs do not listen: those that run when something happens (`on`,
//! see `lang.rs`) are given everything that is sent, through a queue of
//! their own, with what happened in a form they can use.

use crate::shell::Text;
use crate::sql::Name;

/// The prompt on the screen.
pub const SCREEN: usize = 0;
/// The prompt on the serial port.
pub const SERIAL: usize = 1;
/// The prompt for the network.
pub const NETWORK: usize = 2;
pub const SESSIONS: usize = 3;
/// The number that stands for the programs that run by themselves, where a
/// session is asked for. It has names of its own, and no prompt.
pub const BACKGROUND: usize = 3;
/// Channels listened to, over all sessions.
pub const MAX_LISTENS: usize = 16;
/// Events that wait for one session; more are counted as lost.
pub const QUEUE: usize = 16;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Event {
    pub channel: Name,
    pub text: Text,
}

/// What an event is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum What {
    /// Someone sent it with NOTIFY.
    Said,
    /// A row of the table the channel is named after, by its id.
    Insert(u64),
    Update(u64),
    Delete(u64),
    /// Something else about that table: it was made, or removed.
    Table,
}

/// An event as the programs that wait for events get it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Happened {
    pub channel: Name,
    pub text: Text,
    pub what: What,
    /// How many programs, one started by the other, led to this event.
    pub depth: u8,
}

#[derive(Clone, Copy)]
struct Queue {
    events: [Event; QUEUE],
    first: usize,
    count: usize,
    lost: usize,
}

pub struct Events {
    /// Who listens to what; an empty name is a free slot.
    listens: [(usize, Name); MAX_LISTENS],
    queues: [Queue; SESSIONS],
    /// Events sent since the start, and the tick of the last one.
    pub sent: u64,
    pub sent_at: u64,
    /// Whether a program waits for events. Without one, nothing is kept
    /// for the programs.
    pub watched: bool,
    /// Set while a program started by an event runs: how deep it is in a
    /// chain of programs that start each other.
    pub depth: u8,
    happened: [Happened; QUEUE],
    happened_first: usize,
    happened_count: usize,
    unseen: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Listen {
    Started,
    Already,
    /// Every slot is taken.
    Full,
}

impl Events {
    pub const fn new() -> Events {
        const NONE: Event = Event { channel: Name::new(), text: Text::new() };
        Events {
            listens: [(0, Name::new()); MAX_LISTENS],
            queues: [Queue { events: [NONE; QUEUE], first: 0, count: 0, lost: 0 }; SESSIONS],
            sent: 0,
            sent_at: 0,
            watched: false,
            depth: 0,
            happened: [Happened { channel: Name::new(), text: Text::new(), what: What::Said, depth: 0 }; QUEUE],
            happened_first: 0,
            happened_count: 0,
            unseen: 0,
        }
    }

    fn is(&self, slot: usize, session: usize, channel: &[u8]) -> bool {
        let (who, name) = &self.listens[slot];
        *who == session && !name.is_empty() && name.bytes().eq_ignore_ascii_case(channel)
    }

    pub fn listen(&mut self, session: usize, channel: &[u8]) -> Listen {
        if (0..MAX_LISTENS).any(|slot| self.is(slot, session, channel)) {
            return Listen::Already;
        }
        match self.listens.iter_mut().find(|(_, name)| name.is_empty()) {
            Some(slot) => {
                let mut name = Name::new();
                for c in channel {
                    name.push(c.to_ascii_lowercase());
                }
                *slot = (session, name);
                Listen::Started
            }
            None => Listen::Full,
        }
    }

    /// Stops listening to a channel, or to all with `None`. Returns how many
    /// channels that was.
    pub fn unlisten(&mut self, session: usize, channel: Option<&[u8]>) -> usize {
        let mut stopped = 0;
        for slot in 0..MAX_LISTENS {
            let (who, name) = self.listens[slot];
            if who == session && !name.is_empty() && channel.is_none_or(|c| name.bytes().eq_ignore_ascii_case(c)) {
                self.listens[slot].1 = Name::new();
                stopped += 1;
            }
        }
        stopped
    }

    /// The channels a session listens to.
    pub fn channels(&self, session: usize) -> impl Iterator<Item = &Name> {
        self.listens.iter().filter(move |(who, name)| *who == session && !name.is_empty()).map(|(_, name)| name)
    }

    /// How many channels are listened to, over all sessions.
    pub fn listeners(&self) -> usize {
        self.listens.iter().filter(|(_, name)| !name.is_empty()).count()
    }

    /// Sends an event to the sessions that listen to the channel. Returns
    /// how many that is.
    pub fn notify(&mut self, channel: &[u8], text: &[u8], now: u64) -> usize {
        self.send(channel, text, What::Said, now)
    }

    /// Tells of a change to a table, on the channel of its name: `insert 4`
    /// for a row, or `text` for the table itself.
    pub fn changed(&mut self, table: &[u8], what: What, text: &[u8], now: u64) -> usize {
        use core::fmt::Write;
        let mut said = Text::new();
        let _ = match what {
            What::Insert(id) => write!(said, "insert {id}"),
            What::Update(id) => write!(said, "update {id}"),
            What::Delete(id) => write!(said, "delete {id}"),
            _ => {
                said.push_bytes(text);
                Ok(())
            }
        };
        self.send(table, said.bytes(), what, now)
    }

    fn send(&mut self, channel: &[u8], text: &[u8], what: What, now: u64) -> usize {
        if self.watched {
            if self.happened_count == QUEUE {
                self.unseen += 1;
            } else {
                let mut name = Name::new();
                for c in channel {
                    name.push(c.to_ascii_lowercase());
                }
                self.happened[(self.happened_first + self.happened_count) % QUEUE] = Happened { channel: name, text: Text::from_bytes(text), what, depth: self.depth };
                self.happened_count += 1;
            }
        }
        let mut reached = 0;
        for session in 0..SESSIONS {
            if !(0..MAX_LISTENS).any(|slot| self.is(slot, session, channel)) {
                continue;
            }
            reached += 1;
            let queue = &mut self.queues[session];
            if queue.count == QUEUE {
                queue.lost += 1;
                continue;
            }
            let mut name = Name::new();
            for c in channel {
                name.push(c.to_ascii_lowercase());
            }
            queue.events[(queue.first + queue.count) % QUEUE] = Event { channel: name, text: Text::from_bytes(text) };
            queue.count += 1;
        }
        if reached > 0 {
            self.sent += 1;
            self.sent_at = now.max(1);
        }
        reached
    }

    /// The next event that waits for a session.
    pub fn take(&mut self, session: usize) -> Option<Event> {
        let queue = &mut self.queues[session];
        if queue.count == 0 {
            return None;
        }
        let event = queue.events[queue.first];
        queue.first = (queue.first + 1) % QUEUE;
        queue.count -= 1;
        Some(event)
    }

    /// How many events a session missed because too many were waiting. Asking
    /// sets the count back to zero.
    pub fn take_lost(&mut self, session: usize) -> usize {
        core::mem::take(&mut self.queues[session].lost)
    }

    /// The next event that waits for the programs.
    pub fn take_happened(&mut self) -> Option<Happened> {
        if self.happened_count == 0 {
            return None;
        }
        let happened = self.happened[self.happened_first];
        self.happened_first = (self.happened_first + 1) % QUEUE;
        self.happened_count -= 1;
        Some(happened)
    }

    /// How many events the programs missed. Asking sets the count back to zero.
    pub fn take_unseen(&mut self) -> usize {
        core::mem::take(&mut self.unseen)
    }
}

impl Default for Events {
    fn default() -> Events {
        Events::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taken(events: &mut Events, session: usize) -> Vec<String> {
        let mut all = Vec::new();
        while let Some(event) = events.take(session) {
            all.push(format!("{}: {}", event.channel, event.text));
        }
        all
    }

    #[test]
    fn events_reach_those_who_listen() {
        let mut events = Events::new();
        assert_eq!(events.listen(SCREEN, b"Expenses"), Listen::Started);
        assert_eq!(events.listen(SCREEN, b"expenses"), Listen::Already);
        assert_eq!(events.listen(SERIAL, b"alerts"), Listen::Started);
        assert_eq!(events.notify(b"expenses", b"insert 4", 10), 1);
        assert_eq!(events.notify(b"ALERTS", b"disk is full", 20), 1);
        assert_eq!(events.notify(b"nobody", b"hello", 30), 0);
        assert_eq!((events.sent, events.sent_at), (2, 20));
        assert_eq!(taken(&mut events, SCREEN), ["expenses: insert 4"]);
        assert_eq!(taken(&mut events, SERIAL), ["alerts: disk is full"]);
        assert_eq!(events.take(SCREEN), None);
    }

    #[test]
    fn listening_stops() {
        let mut events = Events::new();
        events.listen(SCREEN, b"a");
        events.listen(SCREEN, b"b");
        events.listen(SERIAL, b"a");
        assert_eq!(events.channels(SCREEN).map(|n| n.bytes().to_vec()).collect::<Vec<_>>(), [b"a", b"b"]);
        assert_eq!(events.unlisten(SCREEN, Some(b"A")), 1);
        assert_eq!(events.unlisten(SCREEN, Some(b"a")), 0);
        assert_eq!(events.notify(b"a", b"", 1), 1);
        assert_eq!(events.unlisten(SERIAL, None), 1);
        assert_eq!(events.unlisten(SCREEN, None), 1);
        assert_eq!(events.listeners(), 0);
        assert_eq!(events.notify(b"a", b"", 1), 0);
    }

    #[test]
    fn too_many_are_counted_not_kept() {
        let mut events = Events::new();
        for i in 0..MAX_LISTENS {
            assert_eq!(events.listen(SERIAL, format!("c{i}").as_bytes()), Listen::Started);
        }
        assert_eq!(events.listen(SCREEN, b"one_more"), Listen::Full);
        for _ in 0..QUEUE + 3 {
            events.notify(b"c0", b"x", 1);
        }
        assert_eq!(taken(&mut events, SERIAL).len(), QUEUE);
        assert_eq!(events.take_lost(SERIAL), 3);
        assert_eq!(events.take_lost(SERIAL), 0);
        events.notify(b"c0", b"again", 1);
        assert_eq!(taken(&mut events, SERIAL), ["c0: again"]);
    }

    #[test]
    fn programs_are_given_everything_once_one_waits() {
        let mut events = Events::new();
        events.notify(b"alerts", b"nobody waits", 1);
        assert_eq!(events.take_happened(), None);
        events.watched = true;
        // nobody listens, and still the programs hear of it
        assert_eq!(events.notify(b"Alerts", b"fire", 2), 0);
        assert_eq!(events.changed(b"notes", What::Insert(4), b"", 3), 0);
        events.depth = 2;
        events.changed(b"notes", What::Table, b"drop", 4);
        events.depth = 0;
        let alerts = events.take_happened().unwrap();
        assert_eq!((alerts.channel.bytes(), alerts.text.bytes(), alerts.what, alerts.depth), (&b"alerts"[..], &b"fire"[..], What::Said, 0));
        let row = events.take_happened().unwrap();
        assert_eq!((row.channel.bytes(), row.text.bytes(), row.what), (&b"notes"[..], &b"insert 4"[..], What::Insert(4)));
        let table = events.take_happened().unwrap();
        assert_eq!((table.text.bytes(), table.what, table.depth), (&b"drop"[..], What::Table, 2));
        assert_eq!(events.take_happened(), None);
        // too many at once are counted
        for n in 0..QUEUE as u64 + 3 {
            events.changed(b"notes", What::Update(n), b"", 5);
        }
        assert_eq!(events.take_unseen(), 3);
        assert_eq!(events.take_unseen(), 0);
        assert_eq!(events.take_happened().unwrap().what, What::Update(0));
        // those who listen get the same texts as before
        events.listen(SCREEN, b"notes");
        assert_eq!(events.changed(b"notes", What::Delete(9), b"", 6), 1);
        assert_eq!(taken(&mut events, SCREEN), ["notes: delete 9"]);
    }
}
