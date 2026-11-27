//! Jobs: programs that run turn by turn, so that they hold nothing.
//!
//! A job is one run of the virtual machine in a room of its own
//! (`System::rooms`, which the kernel gives): some instructions at each
//! turn, then the system is given back. Who gives the turns is not this
//! module's to know: a prompt that waits for its job, or the task that
//! carries those nobody waits for.
//!
//! A job is who started it. Every turn runs with the session that began
//! it, with the companion's mark if the companion did, and with its
//! names; all are put back when the turn ends. So what is refused to
//! the prompt over the network, to a program that runs by itself and to
//! the companion is refused to their jobs.
//!
//! A system that was given no rooms has no jobs: everything runs at
//! once, in the machine it has of its own.

use core::fmt::Write;

use xila_db::{Field, Kind};

use crate::expr::Vars;
use crate::lang::Line;
use crate::machine::{self, Room, Turn};
use crate::shell::{self, Out, Style, System, Table, Text};
use crate::sql::Message;

/// The stored table that keeps the jobs nobody waited for, once they ended.
pub const LOG: &[u8] = b"job_log";
/// Its columns, after the `id` every table has.
const LOGGED: [(&[u8], Kind); 7] =
    [(b"job", Kind::Int), (b"program", Kind::Text), (b"by", Kind::Text), (b"began", Kind::Text), (b"ended", Kind::Text), (b"went", Kind::Text), (b"statements", Kind::Int)];
/// What a job said that is kept: its last lines, as far as they fit.
const SAID: usize = 2048;

/// The last lines a job said, for whoever asks later: a job nobody
/// waits for says them to nobody. A table is kept as a note keeps one,
/// its cells apart by tabs.
pub struct Said {
    bytes: [u8; SAID],
    len: usize,
    dropped: usize,
}

impl Said {
    pub const fn new() -> Said {
        Said { bytes: [0; SAID], len: 0, dropped: 0 }
    }

    /// The lines that are kept, the oldest first.
    pub fn lines(&self) -> impl Iterator<Item = &[u8]> {
        self.bytes[..self.len].split(|byte| *byte == b'\n').take(self.bytes[..self.len].iter().filter(|byte| **byte == b'\n').count())
    }

    /// How many lines were said and are no longer there.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    fn push(&mut self, line: &[u8]) {
        if line.len() + 1 > SAID {
            self.dropped += 1;
            return;
        }
        // the oldest lines leave until this one fits
        while self.len + line.len() + 1 > SAID {
            let first = self.bytes[..self.len].iter().position(|byte| *byte == b'\n').map_or(self.len, |end| end + 1);
            self.bytes.copy_within(first..self.len, 0);
            self.len -= first;
            self.dropped += 1;
        }
        self.bytes[self.len..self.len + line.len()].copy_from_slice(line);
        self.bytes[self.len + line.len()] = b'\n';
        self.len += line.len() + 1;
    }
}

impl Default for Said {
    fn default() -> Said {
        Said::new()
    }
}

impl Out for Said {
    fn line(&mut self, _style: Style, text: &[u8]) {
        self.push(text);
    }

    fn table(&mut self, table: &Table) {
        let mut row = crate::text::FixedStr::<{ crate::lang::LINE_MAX }>::new();
        for at in 0..=table.rows {
            row.clear();
            for column in 0..table.cols {
                if column > 0 {
                    row.push(b'\t');
                }
                row.push_bytes(if at == 0 { table.header[column].bytes() } else { table.cells[at - 1][column].bytes() });
            }
            self.push(row.bytes());
        }
    }
}

/// Jobs that people start, running or held, at one time. The system has
/// one more place, for the programs that run by themselves.
pub const PLACES: usize = 8;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    /// It runs when it is given a turn.
    Ready,
    /// One of its instructions asked the kernel something: it goes on
    /// once that is served.
    Waiting,
    Held,
    Done,
    Failed,
    Stopped,
}

/// Who is given what a job says, and waits for it to end.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Watch {
    Screen,
    Serial,
    Nobody,
}

/// What a job runs.
pub enum What<'a> {
    /// A block typed at a prompt.
    Lines(&'a [Line]),
    /// A program, by its name.
    Program(&'a [u8]),
}

pub struct Job {
    /// Counted from 1 since the machine started; 0 for a place nothing took yet.
    pub number: u32,
    /// The name of the program, or the first line of a block.
    pub program: Text,
    pub state: State,
    /// The session that started it, and whether the companion did.
    pub by: usize,
    pub acting: bool,
    pub watch: Watch,
    /// When it began, by the clock of the machine if it tells the time.
    pub began: Option<u64>,
    /// Statements it ran.
    pub statements: u64,
    /// Its names, when they are its own and not those of its session:
    /// they are in the session's place for the time of a turn.
    vars: Vars,
    own: bool,
    /// Whether the prompt that waited for it took how it went.
    taken: bool,
    /// Whether something the kernel answered it was a failure.
    refused: bool,
    /// A program that runs by itself, which the system started: how far
    /// down a chain of programs that start each other it is.
    pub system: bool,
    depth: u8,
    /// What it said, when nobody waits for it.
    pub said: Said,
}

impl Job {
    const fn new() -> Job {
        Job { number: 0, program: Text::new(), state: State::Done, by: 0, acting: false, watch: Watch::Nobody, began: None, statements: 0, vars: Vars::new(), own: false, taken: true, refused: false, system: false, depth: 0, said: Said::new() }
    }

    /// Whether it will run no more.
    pub fn ended(&self) -> bool {
        matches!(self.state, State::Done | State::Failed | State::Stopped)
    }
}

pub struct Jobs {
    /// A place for each room: those of people first, the system's last.
    list: [Job; PLACES + 1],
    /// The number of the last job that began.
    last: u32,
    /// The place to look at first when a turn is to be given: they come round.
    next: usize,
}

impl Jobs {
    pub const fn new() -> Jobs {
        Jobs { list: [const { Job::new() }; PLACES + 1], last: 0, next: 0 }
    }

    /// The places, used or not: one that nothing took has the number 0.
    pub fn list(&self) -> &[Job] {
        &self.list
    }
}

impl Default for Jobs {
    fn default() -> Jobs {
        Jobs::new()
    }
}

/// How many places people's jobs have on this system: a room each, the
/// last room being the system's.
pub fn places(sys: &System) -> usize {
    sys.rooms.as_ref().map_or(0, |rooms| rooms.len().saturating_sub(1)).min(PLACES)
}

/// The place of the programs that run by themselves, on a system that
/// was given rooms: the last one.
pub fn system_place(sys: &System) -> Option<usize> {
    sys.rooms.as_ref().and_then(|rooms| rooms.len().checked_sub(1)).filter(|place| *place <= PLACES)
}

/// Whether nothing runs at a place.
pub fn free(sys: &System, place: usize) -> bool {
    sys.jobs.list.get(place).is_some_and(|job| job.number == 0 || job.ended())
}

/// Starts a program that runs by itself as the system's job. The
/// session is the one such programs run as, and their names, already
/// those of what happened, become the job's. It keeps the limits of a
/// program nobody watches. Why it could not start is said.
pub(crate) fn begin_system(sys: &mut System, place: usize, program: &[u8], depth: u8, out: &mut dyn Out) {
    if machine::start_program(sys, Room::Given(place), program, out).is_none() {
        return;
    }
    if let Some(rooms) = &mut sys.rooms {
        rooms[place].unbound(false);
    }
    sys.jobs.last += 1;
    let names = sys.vars[sys.session].clone();
    let job = &mut sys.jobs.list[place];
    *job = Job::new();
    (job.number, job.program, job.state, job.by, job.watch) = (sys.jobs.last, Text::from_bytes(program), State::Ready, sys.session, Watch::Nobody);
    (job.began, job.own, job.system, job.depth, job.vars) = (names.clock, true, true, depth, names);
}

/// A state as it is shown.
pub fn word(state: State) -> &'static [u8] {
    match state {
        State::Ready => b"running",
        State::Waiting => b"waiting",
        State::Held => b"held",
        State::Done => b"done",
        State::Failed => b"failed",
        State::Stopped => b"stopped",
    }
}

/// The session a job was started from, as it is shown.
pub fn by(session: usize) -> &'static [u8] {
    match session {
        crate::events::SCREEN => b"screen",
        crate::events::SERIAL => b"serial",
        crate::events::NETWORK => b"network",
        _ => b"program",
    }
}

fn missing(number: u32) -> Message {
    let mut message = Message::new();
    let _ = write!(message, "There is no job {number}. Type jobs to list them.");
    message
}

fn about(number: u32, what: &str) -> Message {
    let mut message = Message::new();
    let _ = write!(message, "Job {number} {what}");
    message
}

/// The place of a job that is listed, by its number.
pub fn find(sys: &System, number: u32) -> Result<usize, Message> {
    sys.jobs.list.iter().position(|job| job.number == number && number != 0).ok_or_else(|| missing(number))
}

/// Starts a job, as the session of the moment and with the companion's
/// mark if it is set. Returns its number. An empty error means that why
/// it could not start was said already.
pub fn begin(sys: &mut System, what: What<'_>, watch: Watch, out: &mut dyn Out) -> Result<u32, Message> {
    let places = places(sys);
    if places == 0 {
        return Err(Message::from_bytes(b"This machine has no room for jobs."));
    }
    // a place nothing took, or that of the job that ended longest ago and
    // that no prompt still has to hear of
    let list = &sys.jobs.list[..places];
    let free = list.iter().position(|job| job.number == 0);
    let oldest = list.iter().enumerate().filter(|(_, job)| job.ended() && job.taken).min_by_key(|(_, job)| job.number).map(|(at, _)| at);
    let Some(at) = free.or(oldest) else {
        return Err(Message::from_bytes(b"Eight jobs are running or held: end one, or wait."));
    };
    // the job that was there stays listed if this one cannot start
    let room = Room::Given(at);
    let (started, program) = match what {
        What::Lines(lines) => (machine::start_lines(sys, room, lines, out), lines.first().map_or(Text::new(), |line| Text::from_bytes(crate::text::trim(line.bytes())))),
        What::Program(name) => (machine::start_program(sys, room, name, out), Text::from_bytes(name)),
    };
    if started.is_none() {
        return Err(Message::new());
    }
    // a job that is listed can be stopped: the limits of a program nobody watches are not its own
    if let Some(rooms) = &mut sys.rooms {
        rooms[at].unbound(true);
    }
    sys.jobs.last += 1;
    let own = watch == Watch::Nobody;
    let job = &mut sys.jobs.list[at];
    *job = Job::new();
    (job.number, job.program, job.state, job.by, job.acting, job.watch) = (sys.jobs.last, program, State::Ready, sys.session, sys.acting, watch);
    (job.began, job.own, job.taken) = (sys.clock.map(|(seconds, at)| seconds + sys.ticks.saturating_sub(at) / 100), own, own);
    job.vars.clock = job.began;
    Ok(job.number)
}

/// Gives the job at a place a turn: instructions until it ends or until
/// `spent` says so, as who started it. Nothing happens while the kernel
/// has something to serve, or when the job is not ready.
pub fn turn(sys: &mut System, at: usize, out: &mut dyn Out, spent: &mut dyn FnMut() -> bool) {
    if sys.request.is_some() || sys.jobs.list.get(at).is_none_or(|job| job.number == 0 || job.state != State::Ready) {
        return;
    }
    // it is who started it, with its names, for the time of the turn
    let (by, acting, own) = (sys.jobs.list[at].by, sys.jobs.list[at].acting, sys.jobs.list[at].own);
    let before = (core::mem::replace(&mut sys.session, by), core::mem::replace(&mut sys.acting, acting));
    if own {
        core::mem::swap(&mut sys.vars[by], &mut sys.jobs.list[at].vars);
    }
    // a program that runs by itself is known by its name to what it asks
    // of the companion, and what it causes is one step down its chain
    if sys.jobs.list[at].system {
        sys.firing = crate::sql::Name::from_bytes(sys.jobs.list[at].program.bytes());
        sys.events.depth = sys.jobs.list[at].depth;
    }
    let came = machine::turn(sys, Room::Given(at), 0, out, spent);
    if sys.jobs.list[at].system {
        sys.firing.clear();
        sys.events.depth = 0;
    }
    if own {
        core::mem::swap(&mut sys.vars[by], &mut sys.jobs.list[at].vars);
    }
    (sys.session, sys.acting) = before;
    let statements = sys.rooms.as_ref().map_or(0, |rooms| rooms[at].statements());
    let asked = sys.request.is_some();
    let job = &mut sys.jobs.list[at];
    job.statements = statements;
    job.state = match came {
        Turn::More if asked => State::Waiting,
        Turn::More => State::Ready,
        Turn::Ended(true) if !job.refused => State::Done,
        Turn::Ended(_) => State::Failed,
    };
}

/// The kernel served what a waiting job asked: it goes on. A failure is
/// remembered: the job will not have gone well.
pub fn answered(sys: &mut System, at: usize, ok: bool) {
    if let Some(job) = sys.jobs.list.get_mut(at).filter(|job| job.state == State::Waiting) {
        job.state = State::Ready;
        job.refused |= !ok;
    }
}

/// The place of the job a prompt waits for, ended or not, until how it
/// went is taken.
pub fn watched(sys: &System, watch: Watch) -> Option<usize> {
    sys.jobs.list.iter().position(|job| job.number != 0 && job.watch == watch && watch != Watch::Nobody && !job.taken)
}

/// How the job a prompt waited for went, once it has ended: taken once.
/// The job stays listed.
pub fn outcome(sys: &mut System, watch: Watch) -> Option<bool> {
    let job = &mut sys.jobs.list[watched(sys, watch)?];
    job.ended().then(|| {
        job.taken = true;
        job.state == State::Done
    })
}

/// The job a prompt waited for goes on with nobody waiting: it leaves
/// with a copy of the names of its session. Returns its number.
pub fn aside(sys: &mut System, watch: Watch) -> Option<u32> {
    let at = watched(sys, watch)?;
    let names = sys.vars[sys.jobs.list[at].by].clone();
    let job = &mut sys.jobs.list[at];
    if job.ended() {
        return None;
    }
    (job.watch, job.taken, job.own, job.vars) = (Watch::Nobody, true, true, names);
    Some(job.number)
}

/// Stops the job a prompt waits for, which a key asked for. Returns what
/// to say of where it was. The prompt then takes how it went, as for a
/// job that ended by itself.
pub fn stop_watched(sys: &mut System, watch: Watch) -> Option<Message> {
    let at = watched(sys, watch).filter(|at| !sys.jobs.list[*at].ended())?;
    let mut message = Message::from_bytes(b"Stopped.");
    if let Some((program, line)) = machine::at(sys, Room::Given(at)).filter(|(program, _)| !program.is_empty()) {
        message.clear();
        let _ = write!(message, "Stopped at line {line} of {program}.");
    }
    machine::stop(sys, Room::Given(at));
    sys.jobs.list[at].state = State::Stopped;
    Some(message)
}

/// What the prompt of a session waits for, if it is one that waits: the
/// screen and the serial port do.
pub fn watch_of(session: usize) -> Option<Watch> {
    match session {
        crate::events::SCREEN => Some(Watch::Screen),
        crate::events::SERIAL => Some(Watch::Serial),
        _ => None,
    }
}

/// Stops a job for good: its room is free.
pub fn end(sys: &mut System, number: u32) -> Result<(), Message> {
    let at = find(sys, number)?;
    if sys.jobs.list[at].ended() {
        return Err(about(number, "has ended."));
    }
    machine::stop(sys, Room::Given(at));
    sys.jobs.list[at].state = State::Stopped;
    if sys.jobs.list[at].watch == Watch::Nobody && !sys.jobs.list[at].system {
        record(sys, at);
    }
    Ok(())
}

/// Whether no job that nobody waits for is ready to run.
pub fn idle(sys: &System) -> bool {
    !sys.jobs.list.iter().any(|job| job.number != 0 && job.watch == Watch::Nobody && job.state == State::Ready)
}

/// Gives a turn to one of the jobs nobody waits for, each in its turn:
/// instructions until `spent`. What it says is kept with it. Returns the
/// place of a job that asked the kernel something and waits for it: the
/// caller serves it, then calls `served`. `log` is where the system says
/// what concerns nobody in particular.
pub fn work(sys: &mut System, log: &mut dyn Out, spent: &mut dyn FnMut() -> bool) -> Option<usize> {
    if sys.request.is_some() {
        return None;
    }
    let count = sys.jobs.list.len();
    let at = (0..count).map(|step| (sys.jobs.next + step) % count).find(|at| {
        let job = &sys.jobs.list[*at];
        job.number != 0 && job.watch == Watch::Nobody && job.state == State::Ready
    })?;
    sys.jobs.next = (at + 1) % count;
    if sys.jobs.list[at].system {
        // a program that runs by itself says in the log, under its name
        let name = sys.jobs.list[at].program;
        turn(sys, at, &mut crate::lang::named(log, name.bytes()), spent);
        return (sys.jobs.list[at].state == State::Waiting).then_some(at);
    }
    // what it says is kept with it: taken out while the system is its own to change
    let mut said = core::mem::take(&mut sys.jobs.list[at].said);
    turn(sys, at, &mut said, spent);
    sys.jobs.list[at].said = said;
    if sys.jobs.list[at].state == State::Waiting {
        return Some(at);
    }
    if sys.jobs.list[at].ended() && !record(sys, at) {
        log.line(Style::Error, b"The table job_log is not the one jobs are recorded in.");
    }
    None
}

/// The kernel served what the job at a place asked: what it answered is
/// said where the job says, and the job goes on.
pub fn served(sys: &mut System, at: usize, log: &mut dyn Out) -> bool {
    if sys.jobs.list[at].system {
        let name = sys.jobs.list[at].program;
        let ok = shell::served(sys, &mut crate::lang::named(log, name.bytes()));
        answered(sys, at, ok);
        return ok;
    }
    let mut said = core::mem::take(&mut sys.jobs.list[at].said);
    let ok = shell::served(sys, &mut said);
    sys.jobs.list[at].said = said;
    answered(sys, at, ok);
    ok
}

/// Writes a job that ended in the table of those nobody waited for.
/// False when a table of that name is there with other columns: it is
/// left alone. The row is written as the companion's record is: not
/// through SQL, and nobody is told of it, so that no program starts on
/// the record of programs.
fn record(sys: &mut System, at: usize) -> bool {
    let table = match sys.db.find(LOG) {
        Some(table) => {
            let columns = &sys.db.tables()[table].columns;
            if columns.len() != LOGGED.len() + 1 || !columns[1..].iter().zip(LOGGED).all(|(column, (name, kind))| column.name == name && column.kind == kind) {
                return false;
            }
            table
        }
        None => match sys.db.create(LOG, &LOGGED) {
            Ok(table) => table,
            Err(_) => {
                sys.db.rollback();
                return true;
            }
        },
    };
    let job = &sys.jobs.list[at];
    let by = by(job.by);
    // how it went: where it stopped is the last thing it said
    let mut went = Text::new();
    match job.state {
        State::Stopped => went.push_bytes(b"stopped"),
        State::Failed => {
            went.push_bytes(b"failed");
            if let Some(place) = job.said.lines().filter_map(|line| line.strip_prefix(b"Stopped ")).last() {
                went.push(b' ');
                went.push_bytes(place.strip_suffix(b".").unwrap_or(place));
            }
        }
        _ => went.push_bytes(b"done"),
    }
    let moment = |seconds: Option<u64>| seconds.map(|seconds| crate::clock::Moment::at(seconds).text());
    let now = sys.clock.map(|(seconds, at)| seconds + sys.ticks.saturating_sub(at) / 100);
    let (began, ended) = (moment(job.began), moment(now));
    fn text(text: &Option<Text>) -> Field<'_> {
        text.as_ref().map_or(Field::Null, |text| Field::Text(text.bytes()))
    }
    let row = [Field::Int(job.number as i64), Field::Text(job.program.bytes()), Field::Text(by), text(&began), text(&ended), Field::Text(went.bytes()), Field::Int(job.statements as i64)];
    match sys.db.insert(table, &row) {
        Ok(_) => {
            let _ = sys.db.commit(&mut |_, _, _| {});
            sys.stored_at = sys.ticks.max(1);
        }
        Err(_) => sys.db.rollback(),
    }
    true
}

/// Keeps a job from running until it is released.
pub fn hold(sys: &mut System, number: u32) -> Result<(), Message> {
    let job = &mut sys.jobs.list[find(sys, number)?];
    match job.state {
        State::Ready => {
            job.state = State::Held;
            Ok(())
        }
        State::Held => Err(about(number, "is held already.")),
        State::Waiting => Err(about(number, "waits for the kernel: try again.")),
        _ => Err(about(number, "has ended.")),
    }
}

pub fn release(sys: &mut System, number: u32) -> Result<(), Message> {
    let job = &mut sys.jobs.list[find(sys, number)?];
    match job.state {
        State::Held => {
            job.state = State::Ready;
            Ok(())
        }
        State::Done | State::Failed | State::Stopped => Err(about(number, "has ended.")),
        _ => Err(about(number, "is not held.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{NETWORK, SCREEN, SERIAL};
    use crate::machine::tests::rooms;
    use crate::shell::tests::Lines;

    fn typed(sys: &mut System, text: &[&str]) {
        for line in text {
            assert!(crate::shell::exec(sys, line.as_bytes(), &mut Lines::default()), "{line}");
        }
    }

    fn lines(text: &[&str]) -> Vec<Line> {
        text.iter().map(|line| Line::from_bytes(line.as_bytes())).collect()
    }

    /// A system with rooms for every job, and a program that counts to `to`.
    fn ready() -> Box<System> {
        let mut sys = Box::new(System::new());
        rooms(&mut sys, PLACES + 1);
        typed(&mut sys, &["program count", "let n = 0", "while n < to", "let n = n + 1", "end", "say n", "end", "let to = 5"]);
        sys
    }

    /// Gives a job turns of one instruction until it has ended. Returns what it said.
    fn finish(sys: &mut System, place: usize) -> Vec<String> {
        let mut out = Lines::default();
        for _ in 0..100_000 {
            if sys.jobs.list()[place].ended() {
                break;
            }
            turn(sys, place, &mut out, &mut || true);
        }
        assert!(sys.jobs.list()[place].ended());
        out.lines
    }

    fn place_of(sys: &System, number: u32) -> usize {
        sys.jobs.list().iter().position(|job| job.number == number).unwrap()
    }

    #[test]
    fn a_job_runs_as_who_started_it() {
        let mut sys = ready();
        // started from the network prompt: what is refused there is refused to it, whoever gives it its turns
        sys.session = NETWORK;
        let number = begin(&mut sys, What::Lines(&lines(&["say 'before'", "web on"])), Watch::Nobody, &mut Lines::default()).unwrap();
        sys.session = SCREEN;
        let place = place_of(&sys, number);
        let said = finish(&mut sys, place);
        assert_eq!(said, ["before", "web on is typed at the machine itself.", "Stopped at line 2."]);
        assert_eq!((sys.jobs.list()[place].state, sys.jobs.list()[place].by), (State::Failed, NETWORK));
        assert!(sys.web.code().is_none());
        assert_eq!((sys.session, sys.acting), (SCREEN, false));
        // started by the companion: it stays the companion's
        sys.acting = true;
        let number = begin(&mut sys, What::Lines(&lines(&["key set made up", "say 'not here'"])), Watch::Nobody, &mut Lines::default()).unwrap();
        sys.acting = false;
        let place = place_of(&sys, number);
        let said = finish(&mut sys, place);
        assert_eq!(said.len(), 2, "{said:?}");
        assert_eq!(said[1], "Stopped at line 1.");
        assert_eq!(sys.jobs.list()[place].state, State::Failed);
        assert!(crate::secrets::get(&sys.db, b"made").is_none());
        assert_eq!((sys.session, sys.acting), (SCREEN, false));
        // and at the machine, the same lines are not refused
        let number = begin(&mut sys, What::Lines(&lines(&["key set made up"])), Watch::Nobody, &mut Lines::default()).unwrap();
        let place = place_of(&sys, number);
        finish(&mut sys, place);
        assert_eq!(sys.jobs.list()[place].state, State::Done);
    }

    #[test]
    fn a_job_nobody_waits_for_has_names_of_its_own() {
        let mut sys = ready();
        // the program counts to `to`, a name of the session: a job nobody waits for does not see it
        let number = begin(&mut sys, What::Program(b"count"), Watch::Nobody, &mut Lines::default()).unwrap();
        let place = place_of(&sys, number);
        assert_eq!(finish(&mut sys, place), ["Unknown name: to", "Stopped at line 2 of count."]);
        // and what it names is not the session's
        let number = begin(&mut sys, What::Lines(&lines(&["let to = 99", "let made = 1", "say to"])), Watch::Nobody, &mut Lines::default()).unwrap();
        let place = place_of(&sys, number);
        assert_eq!(finish(&mut sys, place), ["99"]);
        let mut out = Lines::default();
        assert!(!crate::shell::exec(&mut sys, b"say made", &mut out));
        assert!(crate::shell::exec(&mut sys, b"say to", &mut out));
        assert_eq!(out.lines, ["Unknown name: made", "5"]);
        // a job the screen waits for sees the names of the screen, and changes them
        let number = begin(&mut sys, What::Program(b"count"), Watch::Screen, &mut Lines::default()).unwrap();
        let place = place_of(&sys, number);
        assert_eq!(watched(&sys, Watch::Screen), Some(place));
        assert_eq!(outcome(&mut sys, Watch::Screen), None, "it has not ended");
        assert_eq!(finish(&mut sys, place), ["5"]);
        assert_eq!(outcome(&mut sys, Watch::Screen), Some(true));
        assert_eq!((watched(&sys, Watch::Screen), outcome(&mut sys, Watch::Screen)), (None, None), "taken once");
        let mut out = Lines::default();
        crate::shell::exec(&mut sys, b"say n", &mut out);
        assert_eq!(out.lines, ["5"]);
        // sent aside, it goes on with a copy: the session's names are its own again
        typed(&mut sys, &["let to = 50"]);
        let number = begin(&mut sys, What::Program(b"count"), Watch::Screen, &mut Lines::default()).unwrap();
        let place = place_of(&sys, number);
        for _ in 0..20 {
            turn(&mut sys, place, &mut Lines::default(), &mut || true);
        }
        assert_eq!(aside(&mut sys, Watch::Screen), Some(number));
        assert_eq!(watched(&sys, Watch::Screen), None);
        typed(&mut sys, &["let to = 3", "let n = 1000"]);
        assert_eq!(finish(&mut sys, place), ["50"]);
        let mut out = Lines::default();
        crate::shell::exec(&mut sys, b"say n", &mut out);
        assert_eq!(out.lines, ["1000"]);
        // the serial port has its own
        sys.session = SERIAL;
        let number = begin(&mut sys, What::Lines(&lines(&["let mine = 7", "say mine"])), Watch::Serial, &mut Lines::default()).unwrap();
        sys.session = SCREEN;
        let place = place_of(&sys, number);
        assert_eq!(finish(&mut sys, place), ["7"]);
        assert_eq!(outcome(&mut sys, Watch::Serial), Some(true));
        assert!(!crate::shell::exec(&mut sys, b"say mine", &mut Lines::default()));
    }

    #[test]
    fn places_are_taken_and_given_back() {
        let mut sys = ready();
        let forever = lines(&["while 1 = 1", "end"]);
        let mut numbers = Vec::new();
        for _ in 0..PLACES {
            numbers.push(begin(&mut sys, What::Lines(&forever), Watch::Nobody, &mut Lines::default()).unwrap());
        }
        assert_eq!(numbers, (1..=PLACES as u32).collect::<Vec<_>>());
        // they run without the limits of a program nobody watches: 20 000 turns later they still do
        for _ in 0..20_000 {
            turn(&mut sys, 0, &mut Lines::default(), &mut || true);
        }
        assert_eq!(sys.jobs.list()[0].state, State::Ready);
        assert!(sys.jobs.list()[0].statements > 1);
        // a ninth is refused, and says why
        let refused = begin(&mut sys, What::Lines(&forever), Watch::Nobody, &mut Lines::default()).unwrap_err();
        assert_eq!(refused.bytes(), b"Eight jobs are running or held: end one, or wait.");
        // one ended: its place is the ninth's, and its number is not given again
        end(&mut sys, 3).unwrap();
        end(&mut sys, 5).unwrap();
        assert_eq!(sys.jobs.list()[place_of(&sys, 3)].state, State::Stopped);
        assert_eq!(begin(&mut sys, What::Lines(&forever), Watch::Nobody, &mut Lines::default()).unwrap(), 9);
        // the oldest ended job left: the other is still listed
        assert!(sys.jobs.list().iter().all(|job| job.number != 3));
        assert!(sys.jobs.list().iter().any(|job| job.number == 5 && job.state == State::Stopped));
        assert_eq!(begin(&mut sys, What::Lines(&forever), Watch::Nobody, &mut Lines::default()).unwrap(), 10);
        assert!(begin(&mut sys, What::Lines(&forever), Watch::Nobody, &mut Lines::default()).is_err());
        // what cannot start takes no place, and says why itself
        end(&mut sys, 10).unwrap();
        let mut out = Lines::default();
        assert!(begin(&mut sys, What::Program(b"nowhere"), Watch::Nobody, &mut out).unwrap_err().is_empty());
        assert_eq!(out.lines, ["Unknown program: nowhere. Type programs to list them."]);
        assert!(begin(&mut sys, What::Lines(&lines(&["say 1", "else"])), Watch::Nobody, &mut out).is_err());
        assert_eq!(begin(&mut sys, What::Lines(&forever), Watch::Nobody, &mut Lines::default()).unwrap(), 11);
        // a system that was given no rooms has no jobs
        let mut bare = Box::new(System::new());
        assert_eq!(begin(&mut bare, What::Lines(&forever), Watch::Nobody, &mut Lines::default()).unwrap_err().bytes(), b"This machine has no room for jobs.");
    }

    #[test]
    fn a_job_is_held_released_and_ended() {
        let mut sys = ready();
        typed(&mut sys, &["create table two (n int)", "insert into two values (1), (2)"]);
        typed(&mut sys, &["program deep", "for a in select n from two", "for b in select n from two", "while 1 = 1", "end", "end", "end", "end", "program outer", "deep", "end"]);
        let number = begin(&mut sys, What::Program(b"outer"), Watch::Nobody, &mut Lines::default()).unwrap();
        let place = place_of(&sys, number);
        for _ in 0..50 {
            turn(&mut sys, place, &mut Lines::default(), &mut || true);
        }
        let ran = sys.jobs.list()[place].statements;
        // held, turns do nothing
        hold(&mut sys, number).unwrap();
        assert_eq!(sys.jobs.list()[place].state, State::Held);
        for _ in 0..50 {
            turn(&mut sys, place, &mut Lines::default(), &mut || true);
        }
        assert_eq!(sys.jobs.list()[place].statements, ran);
        assert_eq!(hold(&mut sys, number).unwrap_err().bytes(), b"Job 1 is held already.");
        // released, it goes on
        release(&mut sys, number).unwrap();
        assert_eq!(release(&mut sys, number).unwrap_err().bytes(), b"Job 1 is not held.");
        for _ in 0..50 {
            turn(&mut sys, place, &mut Lines::default(), &mut || true);
        }
        assert!(sys.jobs.list()[place].statements > ran);
        // ended three units deep in two loops: its room is free, with its cursors
        end(&mut sys, number).unwrap();
        assert_eq!(sys.jobs.list()[place].state, State::Stopped);
        assert!(crate::machine::at(&sys, crate::machine::Room::Given(place)).is_none());
        assert_eq!(end(&mut sys, number).unwrap_err().bytes(), b"Job 1 has ended.");
        assert_eq!(hold(&mut sys, number).unwrap_err().bytes(), b"Job 1 has ended.");
        assert_eq!(end(&mut sys, 42).unwrap_err().bytes(), b"There is no job 42. Type jobs to list them.");
        // a turn does nothing while the kernel has something to serve
        let number = begin(&mut sys, What::Lines(&lines(&["say 'now'"])), Watch::Nobody, &mut Lines::default()).unwrap();
        let place = place_of(&sys, number);
        sys.request = Some(crate::shell::Request::Measure);
        let mut out = Lines::default();
        turn(&mut sys, place, &mut out, &mut || true);
        assert!(out.lines.is_empty());
        sys.request = None;
        assert_eq!(finish(&mut sys, place), ["now"]);
    }

    #[test]
    fn what_a_job_says_is_kept_its_last_lines() {
        let mut said = Said::new();
        assert_eq!((said.lines().count(), said.dropped()), (0, 0));
        for n in 0..300 {
            said.line(crate::shell::Style::Out, format!("line number {n}").as_bytes());
        }
        let kept: Vec<String> = said.lines().map(|line| String::from_utf8_lossy(line).into_owned()).collect();
        assert_eq!(kept.last().unwrap(), "line number 299");
        assert!(kept.len() > 100 && kept.len() < 300);
        assert_eq!(kept[0], format!("line number {}", 300 - kept.len()));
        assert_eq!(said.dropped(), 300 - kept.len());
        // a table is written as a note writes one: its cells apart by tabs
        let mut sys = ready();
        let mut said = Said::new();
        assert!(crate::shell::exec(&mut sys, b"select 1 as a, 'two' as b", &mut said));
        assert_eq!(said.lines().collect::<Vec<_>>(), [&b"a\tb"[..], b"1\ttwo"]);
        // a line longer than all the room there is does not stay, and breaks nothing
        said.line(crate::shell::Style::Out, &[b'x'; 5000]);
        said.line(crate::shell::Style::Out, b"after");
        assert_eq!(said.lines().last().unwrap(), b"after");
    }

    /// Gives the jobs nobody waits for their turns, one instruction each, until none is ready.
    fn work_all(sys: &mut System, log: &mut Lines) {
        for _ in 0..200_000 {
            if idle(sys) {
                return;
            }
            if let Some(place) = work(sys, log, &mut || true) {
                // what the kernel would do: serve, then say so
                sys.request = None;
                served(sys, place, log);
            }
        }
        panic!("the jobs never end");
    }

    #[test]
    fn jobs_nobody_waits_for_take_turns() {
        let mut sys = ready();
        let mut numbers = Vec::new();
        for to in [30, 60, 90] {
            let counts = lines(&["let n = 0", &format!("while n < {to}"), "let n = n + 1", "end", "say n"]);
            numbers.push(begin(&mut sys, What::Lines(&counts), Watch::Nobody, &mut Lines::default()).unwrap());
        }
        // one the screen waits for is not theirs to run
        let waited = begin(&mut sys, What::Lines(&lines(&["say 'mine'", "say 'still mine'"])), Watch::Screen, &mut Lines::default()).unwrap();
        // each turn is one job's, and they come round
        let mut log = Lines::default();
        let mut order = Vec::new();
        for _ in 0..6 {
            let before: Vec<u64> = sys.jobs.list().iter().map(|job| job.statements).collect();
            assert_eq!(work(&mut sys, &mut log, &mut || true), None);
            let moved: Vec<usize> = (0..before.len()).filter(|at| sys.jobs.list()[*at].statements != before[*at]).collect();
            assert_eq!(moved.len(), 1);
            order.push(moved[0]);
        }
        assert_eq!(order, [0, 1, 2, 0, 1, 2]);
        work_all(&mut sys, &mut log);
        for (number, to) in numbers.iter().zip(["30", "60", "90"]) {
            let job = &sys.jobs.list()[place_of(&sys, *number)];
            assert_eq!(job.state, State::Done);
            assert_eq!(job.said.lines().collect::<Vec<_>>(), [to.as_bytes()]);
        }
        assert!(log.lines.is_empty(), "{:?}", log.lines);
        assert_eq!(sys.jobs.list()[place_of(&sys, waited)].state, State::Ready);
        // one that asks the kernel something waits for it, and goes on when it is served
        let asks = begin(&mut sys, What::Lines(&lines(&["say 'before'", "spin", "say 'after'"])), Watch::Nobody, &mut Lines::default()).unwrap();
        let place = place_of(&sys, asks);
        assert_eq!(work(&mut sys, &mut log, &mut || false), Some(place));
        assert_eq!(sys.jobs.list()[place].state, State::Waiting);
        assert_eq!(sys.request, Some(crate::shell::Request::Spin));
        assert_eq!(work(&mut sys, &mut log, &mut || false), None, "nothing runs until the kernel has served");
        sys.request = None;
        served(&mut sys, place, &mut log);
        work_all(&mut sys, &mut log);
        assert_eq!(sys.jobs.list()[place].state, State::Done);
        assert_eq!(sys.jobs.list()[place].said.lines().last().unwrap(), b"after");
    }

    #[test]
    fn a_job_that_ends_is_written_in_job_log() {
        let mut sys = ready();
        let mut log = Lines::default();
        // a program that hears the log would start at each row: it must not
        typed(&mut sys, &["program fails", "say 'one'", "say 1 / 0", "end"]);
        begin(&mut sys, What::Lines(&lines(&["say 'done'"])), Watch::Nobody, &mut Lines::default()).unwrap();
        sys.session = NETWORK;
        begin(&mut sys, What::Program(b"fails"), Watch::Nobody, &mut Lines::default()).unwrap();
        sys.session = SCREEN;
        let stopped = begin(&mut sys, What::Lines(&lines(&["while 1 = 1", "end"])), Watch::Nobody, &mut Lines::default()).unwrap();
        // one the screen waits for is not recorded
        let waited = begin(&mut sys, What::Lines(&lines(&["say 'seen'"])), Watch::Screen, &mut Lines::default()).unwrap();
        let place = place_of(&sys, waited);
        finish(&mut sys, place);
        for _ in 0..50 {
            work(&mut sys, &mut log, &mut || true);
        }
        end(&mut sys, stopped).unwrap();
        typed(&mut sys, &["on insert into job_log", "say 'a row'", "end"]);
        let mut out = Lines::default();
        assert!(crate::shell::exec(&mut sys, b"select job, program, by, went, statements from job_log", &mut out));
        let rows: Vec<Vec<&str>> = out.lines[1..].iter().map(|row| row.split_whitespace().collect()).collect();
        assert_eq!(rows[0], ["1", "say", "'done'", "screen", "done", "1"], "{:?}", out.lines);
        assert_eq!(rows[1], ["2", "fails", "network", "failed", "at", "line", "2", "of", "fails", "2"]);
        assert_eq!(rows[2][rows[2].len() - 2], "stopped");
        assert_eq!(rows.len(), 3);
        // nobody is told of those rows
        crate::lang::background(&mut sys, &mut log);
        assert!(log.lines.is_empty(), "{:?}", log.lines);
        // a table of that name made by hand with other columns is left alone, and the log says so
        typed(&mut sys, &["drop program on_insert_job_log", "drop table job_log", "create table job_log (n int)"]);
        begin(&mut sys, What::Lines(&lines(&["say 'done'"])), Watch::Nobody, &mut Lines::default()).unwrap();
        work_all(&mut sys, &mut log);
        assert_eq!(log.lines, ["The table job_log is not the one jobs are recorded in."]);
        let mut out = Lines::default();
        crate::shell::exec(&mut sys, b"select count(*) from job_log", &mut out);
        assert_eq!(out.lines[1].trim(), "0");
    }
}
