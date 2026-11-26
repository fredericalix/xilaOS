//! The companion: a model of the Internet that lives in the system,
//! answers what it is asked, and reads the tables to do so.
//!
//! It thinks elsewhere. A question typed here (`ask how many tasks run?`)
//! is sent to a model over HTTPS with what was said before, and what the
//! model says back is the answer. What makes it a companion of *this*
//! system and not a chat window is what it is told and what it may do:
//! it is told what XilaOS is, and it is given a tool, `select`, which
//! reads the tables as the prompt does. The model does not answer at
//! once when it wants to look: it says which query to run, the query is
//! run here, its result is sent back, and so on until it has an answer.
//! One question is so several calls, `ROUNDS` at most.
//!
//! Everything said is kept in the stored table `talk`, one row for each
//! thing: what was asked (`you`), what the companion said (`xila`, and
//! `more` for what did not fit in one row), each query it asked for
//! (`select`) and what came of it (`result`), with the number of the
//! question they belong to (`turn`). The texts are longer
//! than SQL shows (48 characters): like the lines of programs they are
//! read through the storage engine. The conversation sent to the model
//! is those rows, the last few turns of them.
//!
//! Who the companion is, where its model is and which key opens it are a
//! row of the stored table `companions`, made at the first question and
//! changed with `update`. The key itself is kept apart (`secrets.rs`)
//! and goes out only through TLS.
//!
//! This file has no network in it. `ask` takes a question, `request`
//! writes what to send, `heard` takes what came back; the kernel carries
//! the bytes (`ask.rs`), a little at each tick, so that the machine goes
//! on living while the model thinks.

use core::fmt::Write;

use xila_db::{Database, Field, Kind};

use crate::json;
use crate::shell::{System, Text};
use crate::sql::Message;
use crate::text::FixedStr;

/// The table of what was said, and the one of who the companions are.
pub const TALK: &[u8] = b"talk";
pub const COMPANIONS: &[u8] = b"companions";
/// The longest text of one row of `talk`. A longer answer takes several.
pub const LINE: usize = 900;
/// The longest question.
pub const ASKED: usize = 150;
/// Calls to the model for one question, at most.
pub const ROUNDS: u8 = 6;
/// Queries of one answer of the model that are run; more are refused.
const CALLS: usize = 4;
/// Turns of the conversation that are sent along, at most.
const TURNS: i64 = 6;
/// Room the kernel gives for a question to the model, and for its answer.
pub const REQUEST: usize = 48 * 1024;
pub const ANSWER: usize = 64 * 1024;
/// Who asked, when it was asked on the companion's own screen: no
/// session of a prompt.
pub const HERE: usize = usize::MAX;
/// Questions of programs that wait their turn, at most.
const WAITING: usize = 8;
/// Ticks of silence a model is waited for: it may think for a while.
pub const PATIENCE: u64 = 9000;

/// Where the model is, unless the table says otherwise.
const PLACE: &[u8] = b"https://openrouter.ai/api/v1/chat/completions";
const MODEL: &[u8] = b"deepseek/deepseek-v4.1-flash-1017";
const KEY: &[u8] = b"openrouter";
const NAME: &[u8] = b"Xila";

/// What the model is told before anything else.
const TOLD: &[u8] = b"You live inside XilaOS, a small hobby operating system, and you are its companion: you talk with its one user, who types at its keyboard. \
XilaOS is not a Unix: there are no files, no shell of Unix, no processes. A database is the heart of the system and everything is a table. \
Its prompt understands SQL and a few commands.\n\
You can read the tables with the tool `select`, which runs one SQL SELECT and gives you its rows. Look before you answer: never guess what a table holds, and never invent a table. \
`select * from tables` lists every table (kind `system` for views of the live system, `stored` for the user's own). `select * from columns where table_name = 'x'` gives the columns of one. \
Views of the system: tasks (what runs), memory, machine, settings, screens, commands, city. \
The SQL is small: one statement, joins of two tables at most, no subqueries, 32 rows a result, texts cut at 48 characters; count(*), sum, min, max, avg, where, group by, order by, limit work.\n\
Answer in plain text of plain ASCII: no markdown, no emphasis marks, no lists with stars, no emoji. Be brief: a few short lines, 400 characters at most. \
If a query fails, read the error and try another. If you cannot know, say so.";

/// The one tool, as the model is told of it.
const TOOLS: &[u8] = br#"[{"type":"function","function":{"name":"select","description":"Reads rows of the tables of XilaOS with one SQL SELECT statement. Returns the rows, cells apart by tabs, the names of the columns first; or an error.","parameters":{"type":"object","properties":{"query":{"type":"string","description":"One SELECT statement."}},"required":["query"]}}}]"#;

/// Why a question got no answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trouble {
    /// No key is kept under the name the companion uses.
    NoKey,
    /// The place of the model is not a place.
    NoPlace,
    /// The model could not be reached, or what it said was cut.
    Ask(crate::ask::Failed),
    /// Its server refused, with that number.
    Refused(u16),
    /// What it said is not an answer as a model gives one.
    Lost,
    /// It asked for queries again and again without ever answering.
    TooMany,
    /// The conversation does not fit in what is sent, even one turn of it.
    TooLong,
    /// The tables could not be written: nothing can be kept.
    Engine,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    /// Nothing is asked.
    Idle,
    /// The model is to be called, or is being called: the kernel's work.
    Calling,
}

/// How a question ended, for whoever asked it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The answer is in `talk`, under that turn.
    Said(i64),
    Failed(Trouble),
}

/// The companion as the system holds it.
pub struct Companion {
    pub stage: Stage,
    /// The number of the question being answered.
    pub turn: i64,
    rounds: u8,
    /// The session that asked, which is shown the answer.
    pub asker: usize,
    /// How the last question ended, until the asker has been told.
    pub outcome: Option<Outcome>,
    /// When the question was asked, and what its server said if it refused.
    pub since: u64,
    pub refusal: FixedStr<120>,
    /// Calls made to the model since the start, and queries run for it.
    pub calls: u64,
    pub queries: u64,
    /// What programs asked while it was busy, and which program: asked in
    /// turn. And how many were let go because too many waited already.
    waiting: [(FixedStr<ASKED>, crate::sql::Name); WAITING],
    waiting_len: usize,
    pub dropped: u64,
}

impl Companion {
    pub const fn new() -> Companion {
        Companion { stage: Stage::Idle, turn: 0, rounds: 0, asker: 0, outcome: None, since: 0, refusal: FixedStr::new(), calls: 0, queries: 0, waiting: [(FixedStr::new(), FixedStr::new()); WAITING], waiting_len: 0, dropped: 0 }
    }

    /// Questions of programs that wait their turn.
    pub fn waiting(&self) -> usize {
        self.waiting_len
    }
}

impl Default for Companion {
    fn default() -> Companion {
        Companion::new()
    }
}

/// Who a companion is: a row of `companions`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Who {
    pub name: Text,
    pub place: Text,
    pub model: Text,
    pub key: Text,
    pub character: Text,
}

const WHO: [&[u8]; 5] = [b"name", b"place", b"model", b"key", b"character"];

/// The table of that name, if it has the columns it should: `id`, then
/// those, of those kinds.
fn table(db: &Database, name: &[u8], wanted: &[(&[u8], Kind)]) -> Option<usize> {
    let table = db.find(name)?;
    let columns = &db.tables()[table].columns;
    (columns.len() == wanted.len() + 1 && columns[1..].iter().zip(wanted).all(|(column, (name, kind))| column.name == *name && column.kind == *kind)).then_some(table)
}

fn talk(db: &Database) -> Option<usize> {
    table(db, TALK, &[(b"turn", Kind::Int), (b"who", Kind::Text), (b"text", Kind::Text)])
}

/// Who the companion is: the first row of `companions`, or what it is
/// until that table is made.
pub fn who(db: &Database) -> Who {
    let text = Text::from_bytes;
    let mut found = Who { name: text(NAME), place: text(PLACE), model: text(MODEL), key: text(KEY), character: Text::new() };
    let Some(table) = table(db, COMPANIONS, &WHO.map(|name| (name, Kind::Text))) else {
        return found;
    };
    let mut first = true;
    let _ = db.scan(table, &mut |fields| {
        if core::mem::take(&mut first) {
            let field = |at: usize| match fields[at] {
                Field::Text(value) => text(value),
                _ => Text::new(),
            };
            found = Who { name: field(1), place: field(2), model: field(3), key: field(4), character: field(5) };
        }
    });
    found
}

/// Makes the two tables if they are not there, and the row of the
/// companion. A table of one of those names that is something else is
/// left alone, and nothing can be asked.
fn tables(db: &mut Database) -> Result<usize, Message> {
    // one table at a time: the engine makes a table in a change of its own
    for name in [COMPANIONS, TALK] {
        if db.find(name).is_some() {
            continue;
        }
        let made = match name == COMPANIONS {
            true => db.create(name, &WHO.map(|name| (name, Kind::Text))).and_then(|table| db.insert(table, &[Field::Text(NAME), Field::Text(PLACE), Field::Text(MODEL), Field::Text(KEY), Field::Text(b"")])).map(|_| ()),
            false => db.create(name, &[(b"turn", Kind::Int), (b"who", Kind::Text), (b"text", Kind::Text)]).map(|_| ()),
        };
        match made {
            Ok(()) => db.commit(&mut |_, _, _| {}).map_err(|why| crate::change::engine_error(why, None))?,
            Err(why) => {
                db.rollback();
                return Err(crate::change::engine_error(why, None));
            }
        }
    }
    talk(db).ok_or(Message::from_bytes(b"The table talk is not the one the companion keeps what is said in."))
}

/// Writes one row of what was said. Returns its number.
fn say(db: &mut Database, turn: i64, who: &[u8], text: &[u8]) -> Option<u64> {
    let table = talk(db)?;
    match db.insert(table, &[Field::Int(turn), Field::Text(who), Field::Text(&text[..text.len().min(LINE)])]) {
        Ok(id) => db.commit(&mut |_, _, _| {}).ok().map(|_| id),
        Err(_) => {
            db.rollback();
            None
        }
    }
}

/// Takes a question, from a session of a prompt or from the companion's
/// screen (`HERE`). The answer comes later: `System::companion` says when
/// (`outcome`), and `tell` gives it to who asked. Returns why not, when
/// it cannot even be asked.
pub fn ask(sys: &mut System, text: &[u8], asker: usize) -> Result<(), Message> {
    let text = text.trim_ascii();
    if text.is_empty() || text.len() > ASKED {
        return Err(Message::from_bytes(b"Type ask and what you want to know: ask how many tasks are running?"));
    }
    if sys.companion.stage != Stage::Idle {
        return Err(Message::from_bytes(b"The companion is still answering the last question."));
    }
    let who = who(&sys.db);
    let place = crate::ask::place(who.place.bytes()).ok_or(Message::from_bytes(b"The place of the model, in the table companions, is not a place."))?;
    // the key only ever goes out sealed: a plain place gets none, and needs none
    if place.secret && crate::secrets::get(&sys.db, who.key.bytes()).is_none() {
        let mut message = Message::from_bytes(b"No key opens the model. Type key set ");
        message.push_bytes(who.key.bytes());
        message.push_bytes(b" and the key.");
        return Err(message);
    }
    if sys.net.ip.is_none() {
        return Err(Message::from_bytes(b"This machine has no address on the network yet. Type net to see why."));
    }
    let table = tables(&mut sys.db)?;
    // the number of this question: one more than the last
    let mut last = 0;
    let _ = sys.db.scan(table, &mut |fields| {
        if let Field::Int(turn) = fields[1] {
            last = last.max(turn);
        }
    });
    let turn = last + 1;
    say(&mut sys.db, turn, b"you", text).ok_or(Message::from_bytes(b"The question could not be kept: the tables cannot be written."))?;
    let ticks = sys.ticks;
    let companion = &mut sys.companion;
    (companion.stage, companion.turn, companion.rounds, companion.asker, companion.outcome, companion.since) = (Stage::Calling, turn, 0, asker, None, ticks);
    companion.refusal.clear();
    Ok(())
}

/// Takes a question from a program that runs by itself. Nobody waits
/// for the answer: the question takes its turn, and what is said is
/// written on the companion's screen. `who` is the name of the program.
pub fn ask_later(sys: &mut System, text: &[u8], who: &[u8]) -> Result<(), Message> {
    let text = text.trim_ascii();
    if text.is_empty() || text.len() > ASKED {
        return Err(Message::from_bytes(b"ask wants something to ask, of 150 characters at most."));
    }
    let companion = &mut sys.companion;
    if companion.waiting_len == WAITING {
        companion.dropped += 1;
        return Err(Message::from_bytes(b"Eight questions wait for the companion: this one is let go."));
    }
    companion.waiting[companion.waiting_len] = (FixedStr::from_bytes(text), crate::sql::Name::from_bytes(who));
    companion.waiting_len += 1;
    Ok(())
}

/// Asks the next question a program left, when nothing else is being
/// answered and whoever asked last has been told. Returns which program
/// asked, what, and why not if it could not be asked: for the
/// companion's screen, where all of it is written.
pub fn next(sys: &mut System) -> Option<(crate::sql::Name, FixedStr<ASKED>, Result<(), Message>)> {
    let companion = &mut sys.companion;
    if companion.stage != Stage::Idle || companion.outcome.is_some() || companion.waiting_len == 0 {
        return None;
    }
    let (text, who) = companion.waiting[0];
    companion.waiting.copy_within(1..companion.waiting_len, 0);
    companion.waiting_len -= 1;
    let asked = ask(sys, text.bytes(), HERE);
    Some((who, text, asked))
}

/// Gives up the question that is being answered.
pub fn give_up(sys: &mut System, why: Trouble) {
    (sys.companion.stage, sys.companion.outcome) = (Stage::Idle, Some(Outcome::Failed(why)));
}

/// The lines that go with a question to the model: the key, if the
/// place is reached through TLS, and what kind of bytes follow.
pub fn lines(sys: &System, secret: bool) -> FixedStr<300> {
    let mut lines = FixedStr::new();
    if let Some(key) = crate::secrets::get(&sys.db, who(&sys.db).key.bytes()).filter(|_| secret) {
        lines.push_bytes(b"Authorization: Bearer ");
        lines.push_bytes(key.bytes());
        lines.push_bytes(b"\r\n");
    }
    lines.push_bytes(b"Content-Type: application/json\r\n");
    lines
}

/// Writes the conversation from the turn `from` on, as the messages a
/// model is sent, each followed by a comma.
fn messages(db: &Database, table: usize, from: i64, w: &mut json::Writer) {
    // An answer of the model may be several rows: what it said, then the
    // queries it asked for, then their results. It is one message, with
    // the queries inside, followed by one message for each result.
    let (mut open, mut quoted, mut asked) = (false, false, false);
    let mut calls = [0u64; CALLS];
    let (mut count, mut answered) = (0, 0);
    let _ = db.scan(table, &mut |fields| {
        let (Field::Int(id), Field::Int(turn), Field::Text(who), Field::Text(text)) = (fields[0], fields[1], fields[2], fields[3]) else {
            return;
        };
        if turn < from {
            return;
        }
        // what the model said goes on in the next row when it was long
        if who == b"more" {
            if quoted && !asked {
                w.inside(text);
            }
            return;
        }
        let query = who == b"select";
        if !(query && open && count < CALLS) {
            // the message that was open ends here
            if quoted {
                w.raw(b"\"");
            }
            if asked {
                w.raw(b"]");
            }
            if open {
                w.raw(b"},");
            }
            // a query whose result was never kept still needs one: the model's server insists
            if who != b"result" || answered >= count {
                for call in &calls[answered.min(count)..count] {
                    w.raw(b"{\"role\":\"tool\",\"tool_call_id\":\"c");
                    w.number(*call);
                    w.raw(b"\",\"content\":\"(no result was kept)\"},");
                }
                (count, answered) = (if who == b"result" { count } else { 0 }, if who == b"result" { count } else { 0 });
            }
            (open, quoted, asked) = (false, false, false);
        }
        match who {
            b"you" => {
                w.raw(b"{\"role\":\"user\",\"content\":");
                w.text(text);
                w.raw(b"},");
            }
            b"xila" => {
                w.raw(b"{\"role\":\"assistant\",\"content\":\"");
                w.inside(text);
                (open, quoted) = (true, true);
            }
            b"select" => {
                if !open {
                    w.raw(b"{\"role\":\"assistant\",\"content\":null");
                    open = true;
                } else if quoted {
                    w.raw(b"\"");
                    quoted = false;
                }
                w.raw(if asked { b"," } else { b",\"tool_calls\":[" });
                asked = true;
                // what it asked for is itself JSON, written as a text
                let mut inner = [0u8; 2 * LINE + 32];
                let mut query = json::Writer::new(&mut inner);
                query.raw(b"{\"query\":");
                query.text(text);
                query.raw(b"}");
                let length = query.len();
                w.raw(b"{\"id\":\"c");
                w.number(id as u64);
                w.raw(b"\",\"type\":\"function\",\"function\":{\"name\":\"select\",\"arguments\":");
                w.text(&inner[..length]);
                w.raw(b"}}");
                if count < CALLS {
                    calls[count] = id as u64;
                    count += 1;
                }
            }
            b"result" if answered < count => {
                w.raw(b"{\"role\":\"tool\",\"tool_call_id\":\"c");
                w.number(calls[answered]);
                w.raw(b"\",\"content\":");
                w.text(text);
                w.raw(b"},");
                answered += 1;
            }
            // a row that is none of those is not part of the conversation
            _ => {}
        }
    });
    if quoted {
        w.raw(b"\"");
    }
    if asked {
        w.raw(b"]");
    }
    if open {
        w.raw(b"},");
    }
    for call in &calls[answered.min(count)..count] {
        w.raw(b"{\"role\":\"tool\",\"tool_call_id\":\"c");
        w.number(*call);
        w.raw(b"\",\"content\":\"(no result was kept)\"},");
    }
}

/// Writes what is sent to the model: who it is, the last turns of the
/// conversation, and the tool it may use. Returns its length, or None
/// when even the last turn does not fit.
pub fn request(sys: &System, into: &mut [u8]) -> Option<usize> {
    let table = talk(&sys.db)?;
    let who = who(&sys.db);
    let last = sys.companion.turn;
    let mut w = json::Writer::new(into);
    w.raw(b"{\"model\":");
    w.text(who.model.bytes());
    w.raw(b",\"max_tokens\":700,");
    // a model that thinks aloud first is slow and dear: not for a few lines
    if who.place.bytes().windows(10).any(|part| part == b"openrouter") {
        w.raw(b"\"reasoning\":{\"enabled\":false},");
    }
    w.raw(b"\"tools\":");
    w.raw(TOOLS);
    w.raw(b",\"messages\":[{\"role\":\"system\",\"content\":\"Your name is ");
    w.inside(who.name.bytes());
    w.raw(b". ");
    w.inside(TOLD);
    if !who.character.is_empty() {
        w.raw(b" Your character: ");
        w.inside(who.character.bytes());
    }
    w.raw(b"\"},");
    let head = w.len();
    // as many of the last turns as fit, the last one at least
    for from in (last - TURNS + 1).max(1)..=last {
        w.back_to(head);
        messages(&sys.db, table, from, &mut w);
        // the comma after the last message gives way to the end of the list
        w.back_to(w.len() - 1);
        w.raw(b"]}");
        if w.whole() {
            return Some(w.len());
        }
    }
    None
}

/// What a query gives, as the model is told: the names of the columns,
/// then the rows, cells apart by tabs; or the error.
fn selected(sys: &System, query: &[u8], into: &mut FixedStr<LINE>) {
    // only reading: this is the one way in, and `select` cannot change anything
    match crate::sql::select(sys, query) {
        Ok(table) => {
            for col in 0..table.cols {
                into.push_bytes(if col > 0 { b"\t" } else { b"" });
                into.push_bytes(table.header[col].bytes());
            }
            for row in 0..table.rows {
                into.push(b'\n');
                for col in 0..table.cols {
                    into.push_bytes(if col > 0 { b"\t" } else { b"" });
                    into.push_bytes(table.cells[row][col].bytes());
                }
            }
            if table.rows == 0 {
                into.push_bytes(b"\n(no rows)");
            }
            if table.more > 0 {
                let _ = write!(into, "\n({} more rows not shown)", table.more);
            }
        }
        Err(message) => {
            into.push_bytes(b"error: ");
            into.push_bytes(message.bytes());
        }
    }
}

/// What came back from the model: `Ok` with the number its server said
/// and what it said, or why nothing came. Keeps what it said, runs the
/// queries it asked for, and either asks again or ends the question.
pub fn heard(sys: &mut System, got: Result<(u16, &[u8]), crate::ask::Failed>) {
    if sys.companion.stage != Stage::Calling {
        return;
    }
    sys.companion.calls += 1;
    let turn = sys.companion.turn;
    let (status, answer) = match got {
        Ok(got) => got,
        Err(why) => return give_up(sys, Trouble::Ask(why)),
    };
    // a server that refuses says why in its own words
    if let Some(error) = json::get(answer, b"error").filter(|_| status != 200 || json::get(answer, b"choices").is_none()) {
        let mut said = [0u8; 120];
        let length = json::get(error, b"message").and_then(|message| json::text(message, &mut said)).unwrap_or(0);
        sys.companion.refusal = FixedStr::from_bytes(&said[..length]);
        return give_up(sys, Trouble::Refused(status));
    }
    let Some(message) = json::get(answer, b"choices").and_then(|choices| json::item(choices, 0)).and_then(|choice| json::get(choice, b"message")).filter(|_| status == 200) else {
        return give_up(sys, if status == 200 { Trouble::Lost } else { Trouble::Refused(status) });
    };
    // what it said, in rows of what a row holds
    let mut said = [0u8; 4 * LINE];
    let length = json::get(message, b"content").and_then(|content| json::text(content, &mut said)).unwrap_or(0);
    let text = said[..length].trim_ascii();
    for (at, part) in text.chunks(LINE).enumerate() {
        if say(&mut sys.db, turn, if at == 0 { b"xila" } else { b"more" }, part).is_none() {
            return give_up(sys, Trouble::Engine);
        }
    }
    // the queries it asks for: each is run here, and its result kept
    let calls = json::get(message, b"tool_calls").filter(|calls| json::item(calls, 0).is_some());
    let Some(calls) = calls else {
        // no query: what it said is the answer
        return match text.is_empty() {
            true => give_up(sys, Trouble::Lost),
            false => (sys.companion.stage, sys.companion.outcome) = (Stage::Idle, Some(Outcome::Said(turn))),
        };
    };
    let mut asked = [FixedStr::<LINE>::new(); CALLS];
    let mut count = 0;
    while let Some(call) = json::item(calls, count).filter(|_| count < CALLS) {
        let function = json::get(call, b"function");
        let named = function.and_then(|function| json::get(function, b"name")).is_some_and(|name| name == b"\"select\"");
        // what it wants is JSON written as a text: read once, it is JSON again
        let mut arguments = [0u8; 2 * LINE];
        let length = function.and_then(|function| json::get(function, b"arguments")).and_then(|arguments_| json::text(arguments_, &mut arguments)).unwrap_or(0);
        let mut query = [0u8; LINE];
        let length = json::get(&arguments[..length], b"query").and_then(|query_| json::text(query_, &mut query)).unwrap_or(0);
        if named && length > 0 {
            asked[count] = FixedStr::from_bytes(&query[..length]);
        }
        count += 1;
    }
    for query in &asked[..count] {
        if say(&mut sys.db, turn, b"select", query.bytes()).is_none() {
            return give_up(sys, Trouble::Engine);
        }
    }
    for query in &asked[..count] {
        let mut result = FixedStr::<LINE>::new();
        match query.is_empty() {
            true => result.push_bytes(b"error: the only tool is select, with one query."),
            false => {
                selected(sys, query.bytes(), &mut result);
                sys.companion.queries += 1;
            }
        }
        if say(&mut sys.db, turn, b"result", result.bytes()).is_none() {
            return give_up(sys, Trouble::Engine);
        }
    }
    sys.companion.rounds += 1;
    if sys.companion.rounds >= ROUNDS {
        give_up(sys, Trouble::TooMany);
    }
    // else the stage stays `Calling`: the kernel asks again, with the results
}

/// Calls `visit` with each row of what the companion said in a turn, and
/// whether the row goes on with what the row before was saying.
pub fn said(db: &Database, turn: i64, visit: &mut dyn FnMut(&[u8], bool)) {
    if let Some(table) = talk(db) {
        let _ = db.scan(table, &mut |fields| {
            if let (Field::Int(of), Field::Text(who @ (b"xila" | b"more")), Field::Text(text)) = (fields[1], fields[2], fields[3]) {
                if of == turn {
                    visit(text, who == b"more");
                }
            }
        });
    }
}

/// Why a question got no answer, for the user.
pub fn trouble(sys: &System, why: Trouble) -> FixedStr<160> {
    let mut line = FixedStr::new();
    match why {
        Trouble::NoKey => line.push_bytes(b"No key opens the model. Type key set, its name and the key."),
        Trouble::NoPlace => line.push_bytes(b"The place of the model, in the table companions, is not a place."),
        Trouble::Refused(status) => {
            let _ = write!(line, "The model's server refused ({status})");
            if !sys.companion.refusal.is_empty() {
                line.push_bytes(b": ");
                line.push_bytes(sys.companion.refusal.bytes());
            }
            line.push(b'.');
        }
        Trouble::Lost => line.push_bytes(b"What the model's server said is not an answer."),
        Trouble::TooMany => line.push_bytes(b"The companion looked again and again without answering: given up."),
        Trouble::TooLong => line.push_bytes(b"The conversation is too long to send, even its last turn."),
        Trouble::Engine => line.push_bytes(b"What is said cannot be kept: the tables cannot be written."),
        Trouble::Ask(why) => {
            use crate::ask::Failed;
            line.push_bytes(match why {
                Failed::Name(why) => crate::shell::unnamed(why),
                Failed::Secret(_) | Failed::NoChance | Failed::NoClock => return FixedStr::from_bytes(crate::shell::unsure(why).bytes()),
                Failed::NoRoom => b"The model cannot be asked now: too many conversations at once.",
                Failed::NoAnswer => b"The model's server does not answer.",
                Failed::Silent => b"The model's server took the call and then said nothing.",
                Failed::NotAnAnswer => b"The model's server answered something that is not an answer.",
                Failed::Cut => b"The model's server hung up before it had said everything.",
                Failed::TooLong => b"The model's server said more than XilaOS listens to.",
            });
        }
    }
    line
}

/// Whether a session waits for the companion: it asked, and has not been
/// told how that ended.
pub fn waits(sys: &System, session: usize) -> bool {
    sys.companion.asker == session && (sys.companion.stage == Stage::Calling || sys.companion.outcome.is_some())
}

/// Tells the session that asked how its question ended, once that is
/// known: the answer, in lines of `width` characters at most, or why
/// there is none. Returns whether there was an answer; None while there
/// is nothing to tell, or when it is another session that asked.
pub fn tell(sys: &mut System, session: usize, width: usize, out: &mut dyn crate::shell::Out) -> Option<bool> {
    use crate::shell::Style;
    if sys.companion.asker != session {
        return None;
    }
    match sys.companion.outcome.take()? {
        Outcome::Said(turn) => {
            // it may have spoken more than once (before it looked, and after),
            // and what it said at once may be several rows, which are one text
            let mut line = FixedStr::<{ crate::grid::COLS }>::new();
            said(&sys.db, turn, &mut |text, more| {
                if !more && !line.is_empty() {
                    out.line(Style::Out, line.bytes());
                    line.clear();
                }
                for &byte in text {
                    // a line ends where the text says, or at the last space that fits
                    if byte == b'\n' || line.len() >= width {
                        let cut = if byte == b'\n' { line.len() } else { line.bytes().iter().rposition(|byte| *byte == b' ').unwrap_or(line.len()) };
                        out.line(Style::Out, line.bytes()[..cut].trim_ascii_end());
                        line = FixedStr::from_bytes(line.bytes()[cut..].trim_ascii_start());
                    }
                    if byte != b'\n' {
                        line.push(if (0x20..0x7f).contains(&byte) { byte } else { b' ' });
                    }
                }
            });
            if !line.is_empty() {
                out.line(Style::Out, line.bytes());
            }
            Some(true)
        }
        Outcome::Failed(why) => {
            let words = trouble(sys, why);
            // a refusal in the server's own words may be longer than a line
            for part in words.bytes().chunks(width) {
                out.line(Style::Error, part);
            }
            Some(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::run_line;

    /// A system with an address, a key, and a table of its own.
    fn ready() -> System {
        let mut sys = System::new();
        sys.net.ip = Some([10, 0, 2, 15]);
        crate::secrets::set(&mut sys.db, b"openrouter", b"sk-or-v1-secret").unwrap();
        run_line(&mut sys, "create table notes (label text, stars int)");
        run_line(&mut sys, "insert into notes values ('rent', 3), ('a \"quoted\" one', 5)");
        sys
    }

    fn sent(sys: &System) -> Vec<u8> {
        let mut bytes = vec![0u8; REQUEST];
        let length = request(sys, &mut bytes).expect("a request that fits");
        bytes.truncate(length);
        // it is one whole value of JSON, with nothing after it
        assert_eq!(json::value(&bytes).map(|value| value.len()), Some(length), "{}", String::from_utf8_lossy(&bytes));
        bytes
    }

    fn text(value: Option<&[u8]>) -> String {
        let mut bytes = vec![0u8; REQUEST];
        let length = json::text(value.expect("a value"), &mut bytes).expect("a text");
        String::from_utf8(bytes[..length].to_vec()).unwrap()
    }

    /// The messages that were sent: who, what they say, and what else (queries, or whose result).
    fn messages_of(sent: &[u8]) -> Vec<(String, String, String)> {
        let list = json::get(sent, b"messages").unwrap();
        (0..)
            .map_while(|at| json::item(list, at))
            .map(|message| {
                let role = text(json::get(message, b"role"));
                let content = json::get(message, b"content").filter(|content| *content != b"null").map_or(String::new(), |content| text(Some(content)));
                let mut more = json::get(message, b"tool_call_id").map_or(String::new(), |id| text(Some(id)));
                if let Some(calls) = json::get(message, b"tool_calls") {
                    for call in (0..).map_while(|at| json::item(calls, at)) {
                        let function = json::get(call, b"function").unwrap();
                        assert_eq!((text(json::get(call, b"type")), text(json::get(function, b"name"))), ("function".to_string(), "select".to_string()));
                        let arguments = text(json::get(function, b"arguments"));
                        more += &format!("{}={};", text(json::get(call, b"id")), text(json::get(arguments.as_bytes(), b"query")));
                    }
                }
                (role, content, more)
            })
            .collect()
    }

    fn rows(sys: &mut System) -> Vec<String> {
        let table = talk(&sys.db).unwrap();
        let mut rows = Vec::new();
        sys.db.scan(table, &mut |fields| rows.push(format!("{:?}", &fields[1..]).replace("Int(", "").replace("Text(", "").replace(')', ""))).unwrap();
        rows
    }

    /// What a model answers: something said, and queries it wants run.
    fn answer(said: Option<&str>, queries: &[&str]) -> Vec<u8> {
        let mut w = String::from("\n   \n{\"id\":\"gen-1\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"message\":{\"role\":\"assistant\",\"content\":");
        w += &said.map_or("null".to_string(), |said| format!("{said:?}"));
        w += ",\"refusal\":null,\"reasoning\":null";
        if !queries.is_empty() {
            let calls: Vec<String> = queries.iter().enumerate().map(|(at, query)| format!("{{\"type\":\"function\",\"index\":{at},\"id\":\"call_0{at}\",\"function\":{{\"name\":\"select\",\"arguments\":{:?}}}}}", format!("{{\"query\":{query:?}}}"))).collect();
            w += &format!(",\"tool_calls\":[{}]", calls.join(","));
        }
        (w + "}}],\"usage\":{\"cost\":0.00002}}").into_bytes()
    }

    #[test]
    fn a_question_is_kept_and_sent_with_who_the_companion_is() {
        let mut sys = ready();
        assert_eq!((sys.companion.stage, talk(&sys.db)), (Stage::Idle, None));
        sys.ticks = 700;
        assert_eq!(ask(&mut sys, b"  how many notes do I have?  ", crate::events::SERIAL), Ok(()));
        let companion = &sys.companion;
        assert_eq!((companion.stage, companion.turn, companion.asker, companion.outcome, companion.since), (Stage::Calling, 1, crate::events::SERIAL, None, 700));
        assert_eq!(rows(&mut sys), ["[1, [121, 111, 117], [104, 111, 119, 32, 109, 97, 110, 121, 32, 110, 111, 116, 101, 115, 32, 100, 111, 32, 73, 32, 104, 97, 118, 101, 63]]"]);
        // who the companion is was written down, where SQL reads and changes it
        assert_eq!(run_line(&mut sys, "select name, model, key from companions").1.lines[1], "   Xila  deepseek/deepseek-v4.1-flash-1017  openrouter");
        assert_eq!(run_line(&mut sys, "select place from companions").1.lines[1], "   https://openrouter.ai/api/v1/chat/completions");
        let sent = sent(&sys);
        assert_eq!(text(json::get(&sent, b"model")), "deepseek/deepseek-v4.1-flash-1017");
        assert_eq!(json::path(&sent, &[b"reasoning", b"enabled"]), Some(&b"false"[..]));
        assert_eq!(text(json::item(json::get(&sent, b"tools").unwrap(), 0).and_then(|tool| json::path(tool, &[b"function", b"name"]))), "select");
        let messages = messages_of(&sent);
        assert_eq!(messages.len(), 2);
        assert!(messages[0].0 == "system" && messages[0].1.starts_with("Your name is Xila. You live inside XilaOS") && messages[0].1.contains("`select`"), "{:?}", messages[0]);
        assert_eq!(messages[1], ("user".to_string(), "how many notes do I have?".to_string(), String::new()));
        // the key goes with it only where the way is sealed
        assert_eq!(lines(&sys, true).bytes(), b"Authorization: Bearer sk-or-v1-secret\r\nContent-Type: application/json\r\n");
        assert_eq!(lines(&sys, false).bytes(), b"Content-Type: application/json\r\n");
        assert!(!sent.windows(5).any(|part| part == b"sk-or"), "never in what is said to the model");
        // another name, another model, a character: a row of a table
        run_line(&mut sys, "update companions set name = 'Vigie', model = 'some/other', character = 'You are wary.'");
        let sent = self::sent(&sys);
        assert_eq!(text(json::get(&sent, b"model")), "some/other");
        let told = &messages_of(&sent)[0].1;
        assert!(told.starts_with("Your name is Vigie. ") && told.ends_with(" Your character: You are wary."), "{told}");
    }

    #[test]
    fn the_model_looks_at_the_tables_before_it_answers() {
        let mut sys = ready();
        ask(&mut sys, b"how many notes do I have?", crate::events::SERIAL).unwrap();
        // it says it will look, and asks for two queries
        heard(&mut sys, Ok((200, &answer(Some("I'll check."), &["select count(*) from notes", "select label, stars from notes where stars > 4"]))));
        assert_eq!((sys.companion.stage, sys.companion.outcome, sys.companion.calls, sys.companion.queries), (Stage::Calling, None, 1, 2));
        // what it said, what it asked for and what came of it are all kept, and sent back to it
        let messages = messages_of(&sent(&sys));
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[2], ("assistant".to_string(), "I'll check.".to_string(), "c3=select count(*) from notes;c4=select label, stars from notes where stars > 4;".to_string()));
        assert_eq!(messages[3], ("tool".to_string(), "count\n2".to_string(), "c3".to_string()));
        assert_eq!(messages[4], ("tool".to_string(), "label\tstars\na \"quoted\" one\t5".to_string(), "c4".to_string()));
        // then it answers
        heard(&mut sys, Ok((200, &answer(Some("You have 2 notes.\nOne has 5 stars."), &[]))));
        assert_eq!((sys.companion.stage, sys.companion.outcome), (Stage::Idle, Some(Outcome::Said(1))));
        let mut all = Vec::new();
        said(&sys.db, 1, &mut |text, more| all.push((if more { "+" } else { "" }).to_string() + &String::from_utf8_lossy(text)));
        assert_eq!(all, ["I'll check.", "You have 2 notes.\nOne has 5 stars."]);
        // SQL sees the start of each thing that was said
        let seen = run_line(&mut sys, "select turn, who, text from talk order by id").1.lines;
        assert_eq!(seen[1..], ["   1     you     how many notes do I have?", "   1     xila    I'll check.", "   1     select  select count(*) from notes", "   1     select  select label, stars from notes where stars > 4", "   1     result  count 2", "   1     result  label stars a \"quoted\" one 5", "   1     xila    You have 2 notes. One has 5 stars."]);
        // the next question goes with what was said before
        ask(&mut sys, b"and the other one?", crate::events::SERIAL).unwrap();
        assert_eq!(sys.companion.turn, 2);
        let messages = messages_of(&sent(&sys));
        assert_eq!(messages.len(), 7);
        assert_eq!(messages[5], ("assistant".to_string(), "You have 2 notes.\nOne has 5 stars.".to_string(), String::new()));
        assert_eq!(messages[6], ("user".to_string(), "and the other one?".to_string(), String::new()));
        // late news of a call nobody waits for changes nothing
        heard(&mut sys, Ok((200, &answer(Some("The other is rent."), &[]))));
        heard(&mut sys, Ok((200, &answer(Some("Again."), &[]))));
        assert_eq!((sys.companion.outcome, rows(&mut sys).len()), (Some(Outcome::Said(2)), 9));
    }

    #[test]
    fn the_only_way_in_reads_and_reads_what_sql_reads() {
        let mut sys = ready();
        ask(&mut sys, b"clean up", crate::events::SERIAL).unwrap();
        // what is not a query is not run, a table that is not there is said not to be, and keys are nobody's
        let wanted = ["delete from notes", "select * from kept_keys", "drop table notes", "select * from nowhere"];
        heard(&mut sys, Ok((200, &answer(None, &wanted))));
        let messages = messages_of(&sent(&sys));
        assert_eq!(messages[2].1, "");
        for (message, query) in messages[3..].iter().zip(wanted) {
            assert!(message.0 == "tool" && message.1.starts_with("error: "), "{query}: {message:?}");
            assert!(!message.1.contains("sk-or"));
        }
        assert_eq!(run_line(&mut sys, "select count(*) from notes").1.lines[1], "   2");
        assert_eq!(crate::secrets::get(&sys.db, b"openrouter").unwrap().bytes(), b"sk-or-v1-secret");
        // more queries at once than are run, a tool that does not exist, a query that is not a text
        let many = ["select 1", "select 2", "select 3", "select 4", "select 5", "select 6"];
        heard(&mut sys, Ok((200, &answer(None, &many))));
        let broken = br#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"x","function":{"name":"run","arguments":"{\"command\":\"reboot\"}"}},{"id":"y","function":{"name":"select","arguments":"{\"query\":7}"}},{"id":"z"}]}}]}"#;
        heard(&mut sys, Ok((200, broken)));
        let messages = messages_of(&sent(&sys));
        let results: Vec<&str> = messages.iter().filter(|message| message.0 == "tool").map(|message| message.1.as_str()).collect();
        assert_eq!(results[4..8], ["1\n1", "2\n2", "3\n3", "4\n4"]);
        assert_eq!(results[8..], ["error: the only tool is select, with one query."; 3]);
        assert!(!sys.reboot);
        // every message of a result answers a query of the message before them
        let mut asked: Vec<String> = Vec::new();
        for message in &messages {
            match message.0.as_str() {
                "assistant" => asked = message.2.split(';').filter(|call| !call.is_empty()).map(|call| call.split('=').next().unwrap().to_string()).collect(),
                "tool" => assert_eq!(asked.remove(0), message.2),
                _ => assert!(asked.is_empty()),
            }
        }
    }

    #[test]
    fn a_model_that_never_answers_is_given_up() {
        let mut sys = ready();
        ask(&mut sys, b"how many notes?", crate::events::SERIAL).unwrap();
        for round in 1..=ROUNDS {
            assert_eq!(sys.companion.stage, Stage::Calling, "round {round}");
            heard(&mut sys, Ok((200, &answer(None, &["select count(*) from notes"]))));
        }
        assert_eq!((sys.companion.stage, sys.companion.outcome), (Stage::Idle, Some(Outcome::Failed(Trouble::TooMany))));
        // the next question can be asked, and what was left unfinished does not break it
        ask(&mut sys, b"well?", crate::events::SERIAL).unwrap();
        let messages = messages_of(&sent(&sys));
        assert_eq!(messages.last().unwrap().1, "well?");
        assert_eq!(messages.iter().filter(|message| message.0 == "tool").count(), ROUNDS as usize);
    }

    #[test]
    fn what_goes_wrong_is_said_in_words() {
        let mut sys = ready();
        // what cannot even be asked
        assert_eq!(ask(&mut sys, b"   ", crate::events::SERIAL).unwrap_err().bytes(), b"Type ask and what you want to know: ask how many tasks are running?");
        assert!(ask(&mut sys, &[b'x'; ASKED + 1], crate::events::SERIAL).is_err() && ask(&mut sys, &[b'x'; ASKED], crate::events::SERIAL).is_ok());
        assert_eq!(ask(&mut sys, b"again?", crate::events::SERIAL).unwrap_err().bytes(), b"The companion is still answering the last question.");
        // the server refuses, in its own words
        heard(&mut sys, Ok((401, br#"{"error":{"message":"No auth credentials found","code":401}}"#)));
        assert_eq!(sys.companion.outcome, Some(Outcome::Failed(Trouble::Refused(401))));
        assert_eq!(trouble(&sys, Trouble::Refused(401)).bytes(), b"The model's server refused (401): No auth credentials found.");
        for (status, said, why) in [
            (200, &br#"{"error":{"message":"Model overloaded"}}"#[..], Trouble::Refused(200)),
            (500, b"<html>oops</html>", Trouble::Refused(500)),
            (200, b"<html>hello</html>", Trouble::Lost),
            (200, br#"{"choices":[]}"#, Trouble::Lost),
            (200, br#"{"choices":[{"message":{"content":null}}]}"#, Trouble::Lost),
            (200, br#"{"choices":[{"message":{"content":"  "}}]}"#, Trouble::Lost),
            (200, b"", Trouble::Lost),
        ] {
            ask(&mut sys, b"hello?", crate::events::SERIAL).unwrap();
            heard(&mut sys, Ok((status, said)));
            assert_eq!((sys.companion.stage, sys.companion.outcome), (Stage::Idle, Some(Outcome::Failed(why))), "{status} {:?}", String::from_utf8_lossy(said));
            assert!(trouble(&sys, why).len() > 20);
        }
        ask(&mut sys, b"hello?", crate::events::SERIAL).unwrap();
        heard(&mut sys, Err(crate::ask::Failed::Silent));
        assert_eq!(sys.companion.outcome, Some(Outcome::Failed(Trouble::Ask(crate::ask::Failed::Silent))));
        // no key, no address, a place that is not one, a table in the way
        let mut sys = ready();
        crate::secrets::forget(&mut sys.db, b"openrouter").unwrap();
        assert_eq!(ask(&mut sys, b"hello?", crate::events::SERIAL).unwrap_err().bytes(), b"No key opens the model. Type key set openrouter and the key.");
        assert_eq!(talk(&sys.db), None, "nothing was made for a question that was not asked");
        let mut sys = ready();
        sys.net.ip = None;
        assert_eq!(ask(&mut sys, b"hello?", crate::events::SERIAL).unwrap_err().bytes(), b"This machine has no address on the network yet. Type net to see why.");
        let mut sys = ready();
        ask(&mut sys, b"hello?", crate::events::SERIAL).unwrap();
        heard(&mut sys, Ok((200, &answer(Some("Hello."), &[]))));
        run_line(&mut sys, "update companions set place = 'nowhere at all'");
        assert_eq!(ask(&mut sys, b"hello?", crate::events::SERIAL).unwrap_err().bytes(), b"The place of the model, in the table companions, is not a place.");
        // a plain place needs no key: none would go there anyway
        run_line(&mut sys, "update companions set place = 'http://10.0.2.2:8000/v1/chat'");
        crate::secrets::forget(&mut sys.db, b"openrouter").unwrap();
        assert_eq!(ask(&mut sys, b"hello?", crate::events::SERIAL), Ok(()));
        assert_eq!(json::get(&sent(&sys), b"reasoning"), None, "only said to the server that knows the word");
        let mut sys = ready();
        run_line(&mut sys, "create table talk (n int)");
        assert_eq!(ask(&mut sys, b"hello?", crate::events::SERIAL).unwrap_err().bytes(), b"The table talk is not the one the companion keeps what is said in.");
    }

    #[test]
    fn a_long_conversation_is_sent_by_its_last_turns() {
        let mut sys = ready();
        // an answer longer than a row holds is kept in several, and is one again when it is sent
        let long: String = (0..2000).map(|at| char::from(b'a' + (at % 26) as u8)).collect();
        for turn in 1..=9 {
            ask(&mut sys, format!("question {turn}").as_bytes(), crate::events::SERIAL).unwrap();
            heard(&mut sys, Ok((200, &answer(Some(&long), &[]))));
            assert_eq!(sys.companion.outcome, Some(Outcome::Said(turn)));
        }
        let mut all = String::new();
        let mut pieces = Vec::new();
        said(&sys.db, 9, &mut |text, more| {
            all += &String::from_utf8_lossy(text);
            pieces.push(more);
        });
        assert_eq!((all, pieces), (long.clone(), vec![false, true, true]));
        ask(&mut sys, b"question 10", crate::events::SERIAL).unwrap();
        let messages = messages_of(&sent(&sys));
        // who it is, five turns whole, and the question
        assert_eq!(messages.len(), 1 + 2 * 5 + 1);
        assert_eq!((messages[1].1.as_str(), messages[2].1.as_str(), messages[11].1.as_str()), ("question 5", long.as_str(), "question 10"));
        // with less room, fewer turns; with too little, nothing
        let whole = sent(&sys).len();
        let mut small = vec![0u8; whole - 1];
        let length = request(&sys, &mut small).unwrap();
        assert_eq!(messages_of(&small[..length]).len(), 1 + 2 * 4 + 1);
        assert_eq!(request(&sys, &mut small[..1500]), None);
    }
}
