//! What happens to the system, said as events.
//!
//! A table that changes says so by itself (`events.rs`). The machine does
//! not: a cable that is pulled out, a disk that fills up, a task that
//! takes the whole processor happen without a statement. This file looks
//! at the state of the system once a second and, when something has
//! changed that is worth knowing, says it on a channel, as `notify`
//! would: who listens hears it (`listen network`), and a program can wait
//! for it (`on notify network`), which is how the companion is asked
//! about it.
//!
//! | Channel | What is said |
//! |---|---|
//! | `start` | the system has started: its version |
//! | `network` | `cable in`, `cable out`, `address 10.0.2.15`, `address lost`, `remote closed: wrong codes`, `web closed: wrong codes` |
//! | `disk` | `almost full: N of 10 pages used`, `stopped: a write failed`, `damaged page found` |
//! | `tasks` | `NAME has the processor to itself` |
//! | `memory` | `short: N KB free` |
//!
//! Each is said once, when it becomes true, not for as long as it is.
//! Nothing here touches the machine: it reads what `System` holds.

use core::fmt::Write;

use crate::net::Ip;
use crate::shell::{System, Text, MAX_TASKS};

/// Ticks between two looks.
const EVERY: u64 = 100;
/// Looks during which a task must keep nine tenths of the processor
/// before it is said to have it to itself.
const LONG: u8 = 10;

/// What was seen at the last look.
pub struct Watch {
    looked: Option<u64>,
    link: bool,
    ip: Option<Ip>,
    remote_closed: bool,
    web_closed: bool,
    full: bool,
    stopped: bool,
    damaged: u64,
    busy: [u8; MAX_TASKS],
    short: bool,
}

impl Watch {
    pub const fn new() -> Watch {
        Watch { looked: None, link: false, ip: None, remote_closed: false, web_closed: false, full: false, stopped: false, damaged: 0, busy: [0; MAX_TASKS], short: false }
    }
}

impl Default for Watch {
    fn default() -> Watch {
        Watch::new()
    }
}

/// Looks at the system, once a second, and says what has changed.
pub fn look(sys: &mut System) {
    let now = sys.ticks;
    let first = sys.watch.looked.is_none();
    if sys.watch.looked.is_some_and(|looked| now < looked + EVERY) {
        return;
    }
    sys.watch.looked = Some(now);
    let mut said = [(&b""[..], Text::new()); 12];
    let mut count = 0;
    let mut say = |channel: &'static [u8], text: Text| {
        if count < said.len() {
            said[count] = (channel, text);
            count += 1;
        }
    };
    let words = |text: &[u8]| Text::from_bytes(text);

    if first {
        say(b"start", Text::from_bytes(b"XilaOS ").also(crate::VERSION));
    }

    // the network: the cable, the address, the doors that close themselves
    let link = sys.net_card.is_some_and(|card| card.link.is_some());
    if link != sys.watch.link && !first {
        say(b"network", words(if link { b"cable in" } else { b"cable out" }));
    }
    if sys.net.ip != sys.watch.ip {
        match sys.net.ip {
            Some(ip) => say(b"network", Text::from_bytes(b"address ").also(crate::net::ip_text(ip).bytes())),
            None => say(b"network", words(b"address lost")),
        }
    }
    if sys.remote.gave_up && !sys.watch.remote_closed {
        say(b"network", words(b"remote closed: wrong codes"));
    }
    if sys.web.gave_up && !sys.watch.web_closed {
        say(b"network", words(b"web closed: wrong codes"));
    }
    (sys.watch.link, sys.watch.ip, sys.watch.remote_closed, sys.watch.web_closed) = (link, sys.net.ip, sys.remote.gave_up, sys.web.gave_up);

    // the disk and the tables on it
    let storage = sys.db.storage();
    let full = storage.pages > 0 && storage.used * 10 >= storage.pages * 9;
    if full && !sys.watch.full {
        let mut text = Text::new();
        let _ = write!(text, "almost full: {} of 10 pages used", storage.used * 10 / storage.pages);
        say(b"disk", text);
    }
    if storage.stopped && !sys.watch.stopped {
        say(b"disk", words(b"stopped: a write failed"));
    }
    let damaged = sys.damaged.get();
    if damaged > sys.watch.damaged {
        say(b"disk", words(b"damaged page found"));
    }
    (sys.watch.full, sys.watch.stopped, sys.watch.damaged) = (full, storage.stopped, damaged);

    // a task that leaves nothing to the others, for long enough to matter
    for id in 0..MAX_TASKS {
        let task = sys.tasks[id];
        // the idle task has the processor when nobody wants it: that is its work
        let greedy = task.used && sys.cpu_share[id] >= 900 && task.name.bytes() != b"idle";
        let seen = &mut sys.watch.busy[id];
        *seen = if greedy { seen.saturating_add(1) } else { 0 };
        if *seen == LONG {
            let mut text = Text::from_bytes(task.name.bytes());
            text.push_bytes(b" has the processor to itself");
            say(b"tasks", text);
        }
    }

    // the kernel's own memory
    let machine = &sys.machine;
    let short = machine.mem_managed_kb > 0 && machine.mem_free_kb * 10 < machine.mem_managed_kb;
    if short && !sys.watch.short {
        let mut text = Text::new();
        let _ = write!(text, "short: {} KB free", machine.mem_free_kb);
        say(b"memory", text);
    }
    sys.watch.short = short;

    for (channel, text) in &said[..count] {
        sys.events.notify(channel, text.bytes(), now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::run_line;
    use crate::shell::NetCard;

    /// What was said on the channels the screen listens to since the last time.
    fn heard(sys: &mut System) -> Vec<String> {
        let mut all = Vec::new();
        while let Some(event) = sys.events.take(crate::events::SCREEN) {
            all.push(format!("{}: {}", String::from_utf8_lossy(event.channel.bytes()), String::from_utf8_lossy(event.text.bytes())));
        }
        all
    }

    fn listening() -> System {
        let mut sys = System::new();
        for channel in ["start", "network", "disk", "tasks", "memory"] {
            assert!(run_line(&mut sys, &format!("listen {channel}")).0);
        }
        sys
    }

    /// One second later.
    fn later(sys: &mut System) -> Vec<String> {
        sys.ticks += 100;
        look(sys);
        heard(sys)
    }

    #[test]
    fn the_start_is_said_once_and_nothing_else_at_first() {
        let mut sys = listening();
        // a cable that is in at start is not news
        sys.net_card = Some(NetCard { name: "Intel 82574L", link: Some((1000, true)), damaged: 0, missed: 0, sent: 0, refused: 0 });
        look(&mut sys);
        assert_eq!(heard(&mut sys), ["start: XilaOS 0.1"]);
        // not before a second has passed, and then nothing has changed
        sys.ticks += 50;
        look(&mut sys);
        assert_eq!((heard(&mut sys), later(&mut sys)), (vec![], vec![]));
    }

    #[test]
    fn the_network_says_what_changes() {
        let mut sys = listening();
        look(&mut sys);
        heard(&mut sys);
        sys.net_card = Some(NetCard { name: "Intel 82574L", link: Some((1000, true)), damaged: 0, missed: 0, sent: 0, refused: 0 });
        assert_eq!(later(&mut sys), ["network: cable in"]);
        sys.net.ip = Some([10, 0, 2, 15]);
        assert_eq!(later(&mut sys), ["network: address 10.0.2.15"]);
        assert_eq!(later(&mut sys), Vec::<String>::new(), "said once");
        sys.net.ip = Some([192, 168, 100, 200]);
        assert_eq!(later(&mut sys), ["network: address 192.168.100.200"]);
        sys.net_card.as_mut().unwrap().link = None;
        sys.net.ip = None;
        assert_eq!(later(&mut sys), ["network: cable out", "network: address lost"]);
        // the doors that close themselves after twenty wrong codes
        (sys.remote.gave_up, sys.web.gave_up) = (true, true);
        assert_eq!(later(&mut sys), ["network: remote closed: wrong codes", "network: web closed: wrong codes"]);
        assert_eq!(later(&mut sys), Vec::<String>::new());
        // opened again and closed again: said again
        sys.web.gave_up = false;
        later(&mut sys);
        sys.web.gave_up = true;
        assert_eq!(later(&mut sys), ["network: web closed: wrong codes"]);
    }

    #[test]
    fn the_disk_says_when_it_is_almost_full_or_hurt() {
        let mut sys = listening();
        look(&mut sys);
        heard(&mut sys);
        // a database of four hundred pages, filled row by row (the engine
        // keeps thirty-two back, so a much smaller one is refused more
        // rows before it is nine tenths full)
        sys.db.set_limit(400 * 4096);
        run_line(&mut sys, "create table big (label text)");
        let mut said = Vec::new();
        let row = "('0123456789012345678901234567890123456789')";
        let some = format!("insert into big values {}", [row; 4].join(", "));
        let mut most = (0, 0);
        for _ in 0..12_000 {
            if !run_line(&mut sys, &some).0 {
                break;
            }
            let storage = sys.db.storage();
            most = most.max((storage.used, storage.pages));
            said.extend(later(&mut sys));
        }
        assert_eq!(said, ["disk: almost full: 9 of 10 pages used"], "said once, when it became true; most used: {most:?}");
        // a page that does not read as it was written
        sys.damaged.set(1);
        assert_eq!(later(&mut sys), ["disk: damaged page found"]);
        assert_eq!(later(&mut sys), Vec::<String>::new());
        sys.damaged.set(3);
        assert_eq!(later(&mut sys), ["disk: damaged page found"]);
    }

    #[test]
    fn a_task_that_leaves_nothing_to_the_others_is_named() {
        let mut sys = listening();
        look(&mut sys);
        heard(&mut sys);
        for (id, name) in [(0, "interface"), (1, "idle"), (2, "spin")] {
            sys.tasks[id].used = true;
            sys.tasks[id].name = crate::text::FixedStr::from_bytes(name.as_bytes());
        }
        // the idle task has it all: that is what it is for
        sys.cpu_share[1] = 1000;
        for _ in 0..30 {
            assert_eq!(later(&mut sys), Vec::<String>::new());
        }
        // a task that takes it for a moment is not news; for ten seconds, it is, once
        (sys.cpu_share[1], sys.cpu_share[2]) = (0, 950);
        for _ in 0..5 {
            assert_eq!(later(&mut sys), Vec::<String>::new());
        }
        sys.cpu_share[2] = 100;
        later(&mut sys);
        sys.cpu_share[2] = 950;
        let said: Vec<String> = (0..30).flat_map(|_| later(&mut sys)).collect();
        assert_eq!(said, ["tasks: spin has the processor to itself"]);
        // it lets go, and takes it again
        sys.cpu_share[2] = 0;
        later(&mut sys);
        sys.cpu_share[2] = 1000;
        let said: Vec<String> = (0..12).flat_map(|_| later(&mut sys)).collect();
        assert_eq!(said, ["tasks: spin has the processor to itself"]);
    }

    #[test]
    fn memory_that_runs_short_is_said() {
        let mut sys = listening();
        look(&mut sys);
        heard(&mut sys);
        (sys.machine.mem_managed_kb, sys.machine.mem_free_kb) = (200_000, 150_000);
        assert_eq!(later(&mut sys), Vec::<String>::new());
        sys.machine.mem_free_kb = 19_000;
        assert_eq!(later(&mut sys), ["memory: short: 19000 KB free"]);
        assert_eq!(later(&mut sys), Vec::<String>::new());
        sys.machine.mem_free_kb = 100_000;
        later(&mut sys);
        sys.machine.mem_free_kb = 1_000;
        assert_eq!(later(&mut sys), ["memory: short: 1000 KB free"]);
    }
}
