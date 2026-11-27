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

use crate::expr::Vars;
use crate::lang::Line;
use crate::machine::{self, Room, Turn};
use crate::shell::{Out, System, Text};
use crate::sql::Message;

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
}

impl Job {
    const fn new() -> Job {
        Job { number: 0, program: Text::new(), state: State::Done, by: 0, acting: false, watch: Watch::Nobody, began: None, statements: 0, vars: Vars::new(), own: false, taken: true, refused: false }
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
}

impl Jobs {
    pub const fn new() -> Jobs {
        Jobs { list: [const { Job::new() }; PLACES + 1], last: 0 }
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
    let came = machine::turn(sys, Room::Given(at), 0, out, spent);
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

/// Stops a job for good: its room is free.
pub fn end(sys: &mut System, number: u32) -> Result<(), Message> {
    let at = find(sys, number)?;
    if sys.jobs.list[at].ended() {
        return Err(about(number, "has ended."));
    }
    machine::stop(sys, Room::Given(at));
    sys.jobs.list[at].state = State::Stopped;
    Ok(())
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
}
