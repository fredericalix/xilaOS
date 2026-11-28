//! Panels: the screens of applications.
//!
//! ```text
//! panel articles on articles
//!     title 'Articles'
//!     list label, category, qty, place
//!     field label    'Label'
//!     field category 'Category' among select name from categories
//!     field qty      'In stock' shown
//! end
//! ```
//!
//! A panel is described on a stored table: which columns its list shows,
//! under what labels, what is typed in its form. The system gives the
//! rest: the list page by page, the line to find, the keys. It is an
//! object of the system as a program is: its lines are rows of the
//! stored table `panels` (name, line, text), line 0 saying which table
//! it is on; what is wrong with its form is told when it is kept, and
//! it is then not kept.
//!
//! Its name, typed at the prompt of the screen, opens it as a virtual
//! screen of its own (`System::panels`, four at one time), numbered
//! after the overview, the log and the companion's. `ui.rs` gives the
//! keys of that screen to `key` and its rows to `draw`.

use core::fmt::Write;

use xila_db::Field as Stored;

use crate::change::engine_error;
use crate::code::Fault;
use crate::expr::NO_VARS;
use crate::grid::{fx, Cell, Grid, COLS};
use crate::keys::{Key, KeyEvent};
use crate::lang::{self, word_of, Line, MAX_LINES};
use crate::shell::{self, Out, Style, System, Table, Text, SCREENS};
use crate::sql::{self, Message, Name, Schema, Source, Value};
use crate::text::{split_word, trim, FixedStr};
use crate::theme::role;

/// The table panels are kept in.
pub const PANELS: &[u8] = b"panels";
/// Columns a list shows, and fields a form has: what a table has but `id`.
pub const COLUMNS: usize = sql::MAX_COLUMNS - 1;

/// Panels open at one time, each a virtual screen.
pub const OPEN: usize = 4;
/// Rows a page of a list has at most: what the tallest screen has room for.
const ROWS: usize = 40;
/// Pages one can go back through; before those, `PgUp` stops.
const BACK: usize = 64;
/// Characters of the line to find, and the text columns it looks in.
const FIND: usize = 40;
const LOOKED: usize = 3;
/// Ticks between two looks at whether the list changed under who reads it.
const AGAIN: u64 = 100;
/// The first rows of a panel's screen, before those of its list: a blank
/// one, the title, the line to find, a blank one, the names of the
/// columns. And its last rows: what it has to say, and its keys.
const HEAD: usize = 5;
const FOOT: usize = 3;
/// The columns a list gives to what is typed before a row.
const OPT: usize = 4;

/// A label, of a field or of a panel.
pub type Label = FixedStr<32>;

/// A column of the form: what it is called there, and whether it is typed.
#[derive(Clone, Copy)]
pub struct Field {
    pub column: Name,
    pub label: Label,
    /// Shown and not typed.
    pub shown: bool,
    /// The line it was written on, from 0, and whether that line says
    /// what the field may hold (`among`): read from there when asked.
    pub line: usize,
    pub among: bool,
}

/// Options and keys a panel may have of its own.
pub const ACTIONS: usize = 8;

/// What starts something a panel has of its own: a digit typed before a
/// row (the character), or a function key (its number).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    Option(u8),
    Key(u8),
}

/// Something a panel has of its own: what starts it, what it is called,
/// and the line it was written on, where what it does is read from.
#[derive(Clone, Copy)]
pub struct Action {
    pub trigger: Trigger,
    pub label: FixedStr<12>,
    pub line: usize,
}

/// What an option or a key of a panel does.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Does<'a> {
    /// Opens another panel on the rows whose column holds the id of this row.
    Open { panel: &'a [u8], column: &'a [u8] },
    /// Runs a program, called as it is written.
    Run(&'a [u8]),
}

/// Reads what an option or a key does: `open moves where article = id`,
/// or `run count(id)`. None when it is written neither way.
fn does(written: &[u8]) -> Option<Does<'_>> {
    let (word, rest) = word_of(written);
    match word.bytes() {
        b"run" => {
            // a program by its name: not a command, nor a word of the language
            let (_, given) = lang::call_of(rest).filter(|(name, _)| lang::free_name(name))?;
            crate::code::arguments(given, &mut |_| Ok(())).ok()?;
            Some(Does::Run(rest))
        }
        b"open" => {
            let (panel, rest) = split_word(rest);
            let (word, rest) = word_of(rest);
            let equals = rest.iter().position(|byte| *byte == b'=')?;
            let (column, id) = (trim(&rest[..equals]), trim(&rest[equals + 1..]));
            (word.bytes() == b"where" && xila_db::good_name(panel) && xila_db::good_name(column) && id.eq_ignore_ascii_case(b"id")).then_some(Does::Open { panel, column })
        }
        _ => None,
    }
}

/// What follows the label on the line of an option or of a key: what it does.
fn after_label(line: &[u8]) -> &[u8] {
    let (_, rest) = word_of(trim(line));
    let (_, rest) = split_word(rest);
    text_then(trim(rest)).map_or(b"", |(_, rest)| rest)
}

/// A panel as it is described.
#[derive(Clone, Copy)]
pub struct Panel {
    pub name: Name,
    pub table: Name,
    pub title: Label,
    /// The columns of the list, in their order.
    pub list: [Name; sql::MAX_COLUMNS],
    pub listed: usize,
    pub fields: [Field; COLUMNS],
    pub count: usize,
    /// Its own options and keys.
    pub actions: [Action; ACTIONS],
    pub acts: usize,
}

/// The text between the quotes of `'a text'`, a quote written twice
/// being one. None for anything else.
fn quoted(written: &[u8]) -> Option<Label> {
    let inner = written.strip_prefix(b"'")?.strip_suffix(b"'")?;
    let mut text = Label::new();
    let mut bytes = inner.iter();
    while let Some(byte) = bytes.next() {
        // a quote alone would have ended the text
        if *byte == b'\'' && bytes.next() != Some(&b'\'') {
            return None;
        }
        if !text.push(*byte) {
            return None;
        }
    }
    (!text.is_empty()).then_some(text)
}

/// Where a text between quotes ends, when a line starts with one: the
/// text as written, and what follows it.
fn text_then(written: &[u8]) -> Option<(&[u8], &[u8])> {
    if written.first() != Some(&b'\'') {
        return None;
    }
    let mut at = 1;
    while at < written.len() {
        if written[at] == b'\'' {
            if written.get(at + 1) == Some(&b'\'') {
                at += 2;
                continue;
            }
            return Some((&written[..at + 1], trim(&written[at + 1..])));
        }
        at += 1;
    }
    None
}

impl Panel {
    /// The column of a table, by its name as it is written anywhere.
    fn column(schema: &Schema, name: &[u8]) -> Option<Name> {
        schema.columns[..schema.count].iter().find(|column| column.bytes().eq_ignore_ascii_case(name)).copied()
    }

    /// Reads the lines of a panel described on a table. What is wrong is
    /// told with its line, counted from 1.
    pub fn read(name: &[u8], schema: &Schema, lines: &[Line]) -> Result<Panel, Fault> {
        let no_field = Field { column: Name::new(), label: Label::new(), shown: false, line: 0, among: false };
        let mut lower = Name::from_bytes(name);
        lower.lower();
        let mut panel = Panel { name: lower, table: schema.name, title: Label::from_bytes(lower.bytes()), list: [Name::new(); sql::MAX_COLUMNS], listed: 0, fields: [no_field; COLUMNS], count: 0, actions: [Action { trigger: Trigger::Key(0), label: FixedStr::new(), line: 0 }; ACTIONS], acts: 0 };
        for (at, line) in lines.iter().enumerate() {
            let fault = |message: &[u8]| Fault { line: at + 1, message: Message::from_bytes(message) };
            let unknown = |column: &[u8]| Fault { line: at + 1, message: sql::error(b"Unknown column: ", column).also(b". The table ").also(schema.name.bytes()).also(b" has no such column.") };
            let text = trim(line.bytes());
            if text.is_empty() || text[0] == b'#' {
                continue;
            }
            let (word, rest) = word_of(text);
            match word.bytes() {
                b"title" => panel.title = quoted(rest).ok_or_else(|| fault(b"A title is a text between quotes, 32 characters at most: title 'Stock'"))?,
                b"list" => {
                    panel.listed = 0;
                    for column in rest.split(|byte| *byte == b',').map(trim) {
                        if column.is_empty() {
                            return Err(fault(b"A list names columns, with commas between them: list label, place"));
                        }
                        let column = Self::column(schema, column).ok_or_else(|| unknown(column))?;
                        if panel.listed == panel.list.len() {
                            return Err(fault(b"A list shows eight columns at most."));
                        }
                        panel.list[panel.listed] = column;
                        panel.listed += 1;
                    }
                }
                b"field" => {
                    let wrong = || fault(b"A field is a column and its label, 32 characters at most: field place 'Where'");
                    let (column, rest) = split_word(rest);
                    let found = Self::column(schema, column).ok_or_else(|| if column.is_empty() { wrong() } else { unknown(column) })?;
                    if found.bytes() == b"id" {
                        return Err(fault(b"The id of a row is given by the system: it is no field."));
                    }
                    if panel.fields[..panel.count].iter().any(|field| field.column == found) {
                        return Err(Fault { line: at + 1, message: sql::error(b"The column ", found.bytes()).also(b" has a field already.") });
                    }
                    let (label, rest) = text_then(trim(rest)).ok_or_else(wrong)?;
                    let label = quoted(label).ok_or_else(wrong)?;
                    let (next, after) = word_of(rest);
                    let (shown, rest) = if next.bytes() == b"shown" { (true, after) } else { (false, rest) };
                    let (next, after) = word_of(rest);
                    if !rest.is_empty() && (next.bytes() != b"among" || after.is_empty()) {
                        return Err(fault(b"After the label of a field: shown, or among and what it may hold."));
                    }
                    // values written there are read now; a select is read when they are asked for
                    if !rest.is_empty() && word_of(after).0.bytes() != b"select" && !literals(after, &mut |_| {}) {
                        return Err(fault(b"After among: values with commas between them ('a', 'b', 3), or a select."));
                    }
                    // a table has seven columns but its id, and each has one field at most
                    panel.fields[panel.count] = Field { column: found, label, shown, line: at, among: !rest.is_empty() };
                    panel.count += 1;
                }
                b"option" | b"key" => {
                    let (which, rest) = split_word(rest);
                    let trigger = if word.bytes() == b"option" {
                        match which {
                            [digit @ (b'1' | b'3' | b'6'..=b'9')] => Trigger::Option(*digit),
                            _ => return Err(fault(b"The digit of an option is 1, 3, 6, 7, 8 or 9: 2, 4 and 5 are the system's.")),
                        }
                    } else {
                        let number = which.strip_prefix(b"f").or_else(|| which.strip_prefix(b"F")).and_then(crate::text::parse_number);
                        match number {
                            Some(number @ (1 | 2 | 7..=11)) => Trigger::Key(number as u8),
                            _ => return Err(fault(b"A key of a panel is f1, f2 or f7 to f11: the others are the system's.")),
                        }
                    };
                    if panel.actions[..panel.acts].iter().any(|action| action.trigger == trigger) {
                        return Err(fault(if word.bytes() == b"option" { b"This digit has an option already." } else { b"This key is taken already." }));
                    }
                    let labelled = text_then(trim(rest)).and_then(|(label, rest)| Some((quoted(label).filter(|label| label.len() <= 12)?, rest)));
                    let Some((label, rest)) = labelled else {
                        return Err(fault(b"The label of an option or of a key is a text of 12 characters at most."));
                    };
                    if does(rest).is_none() {
                        return Err(fault(b"What it does: open moves where article = id, or run count(id)"));
                    }
                    if panel.acts == ACTIONS {
                        return Err(fault(b"A panel has eight options and keys of its own at most."));
                    }
                    panel.actions[panel.acts] = Action { trigger, label: FixedStr::from_bytes(label.bytes()), line: at };
                    panel.acts += 1;
                    // each must be shown on the row of the options: one that is not is a key nobody knows of
                    if panel.options().len() > COLS - 2 {
                        return Err(fault(b"Its options and keys do not fit on their row: shorten labels, or have fewer."));
                    }
                }
                _ => return Err(fault(b"A line of a panel starts with title, list, field, option or key.")),
            }
        }
        if panel.listed == 0 {
            // every column but the id, which says nothing to who reads
            for column in &schema.columns[1..schema.count.max(1)] {
                panel.list[panel.listed] = *column;
                panel.listed += 1;
            }
        }
        if panel.count == 0 {
            // with no field said, every column but the id is one, under its name
            for column in &schema.columns[1..schema.count.max(1)] {
                panel.fields[panel.count] = Field { column: *column, label: Label::from_bytes(column.bytes()), shown: false, line: 0, among: false };
                panel.count += 1;
            }
        }
        Ok(panel)
    }

    /// The row that says what is typed before a row and what its keys
    /// do: the system's options, then the panel's own.
    fn options(&self) -> FixedStr<128> {
        let mut options = FixedStr::from_bytes(b"2=Change  4=Remove  5=Display");
        for action in &self.actions[..self.acts] {
            let _ = match action.trigger {
                Trigger::Option(digit) => write!(options, "  {}={}", digit as char, action.label),
                Trigger::Key(number) => write!(options, "  F{number}={}", action.label),
            };
        }
        options
    }

    /// What a column is called on this panel: the label of its field, or
    /// its name.
    pub fn label(&self, column: &Name) -> Label {
        self.fields[..self.count].iter().find(|field| field.column == *column).map_or(Label::from_bytes(column.bytes()), |field| field.label)
    }
}

/// The table a panel is described on: stored, and there.
fn described_on(sys: &System, table: &[u8]) -> Result<Schema, Message> {
    match sys.schema(table) {
        Some(schema) if schema.stored => Ok(schema),
        Some(_) => Err(sql::error(b"A panel is described on a stored table: ", table).also(b" is a view of the system.")),
        None => Err(sql::error(b"Unknown table: ", table).also(b". Type tables to list them.")),
    }
}

/// `panel name on table ... end`: keeps the lines under the name.
pub fn define(sys: &mut System, written: &[u8], lines: &[Line], out: &mut dyn Out) -> bool {
    let fail = |out: &mut dyn Out, message: &[u8]| {
        out.line(Style::Error, message);
        false
    };
    let (name, rest) = split_word(written);
    let (on, table) = word_of(rest);
    if on.bytes() != b"on" || table.is_empty() || table.contains(&b' ') {
        return fail(out, b"A panel is written: panel stock on items");
    }
    if !lang::free_name(name) {
        return fail(out, b"A panel is named by a letter, then letters, digits or _, and not as a command.");
    }
    if lang::exists(&sys.db, name) {
        return fail(out, sql::error(name, b" is a program: a panel cannot have its name.").bytes());
    }
    if let Err(message) = lang::kept_in(&sys.db, PANELS) {
        return fail(out, message.bytes());
    }
    let schema = match described_on(sys, table) {
        Ok(schema) => schema,
        Err(message) => return fail(out, message.bytes()),
    };
    if let Err(fault) = Panel::read(name, &schema, lines) {
        out.line(Style::Error, fault.message.bytes());
        let mut place = FixedStr::<COLS>::new();
        let _ = write!(place, "Line {} is not right: the panel is not kept.", fault.line);
        return fail(out, place.bytes());
    }
    let mut lower = Name::from_bytes(name);
    lower.lower();
    let first = FixedStr::<{ xila_db::MAX_NAME + 4 }>::from_bytes(b"on ").also(schema.name.bytes());
    match lang::store_in(&mut sys.db, PANELS, lower.bytes(), Some(first.bytes()), lines) {
        Ok(replaced) => {
            let now = sys.ticks.max(1);
            sys.stored_at = now;
            sys.events.notify(PANELS, Text::from_bytes(b"panel ").also(lower.bytes()).bytes(), now);
            let mut message = FixedStr::<COLS>::new();
            let (what, count) = (if replaced { "replaced" } else { "kept" }, lines.len());
            let _ = write!(message, "Panel {lower} {what}: {count} line{}.", if count == 1 { "" } else { "s" });
            out.line(Style::Out, message.bytes());
            kept_again(sys, lower.bytes());
            true
        }
        Err(error) => {
            sys.db.rollback();
            fail(out, engine_error(error, None).bytes())
        }
    }
}

/// Whether a panel of that name is kept.
pub fn exists(sys: &System, name: &[u8]) -> bool {
    lang::exists_in(&sys.db, PANELS, name)
}

fn unknown(name: &[u8]) -> Message {
    sql::error(b"Unknown panel: ", name).also(b". Type panels to list them.")
}

/// Reads a panel that is kept, with its lines: as it is described today
/// on the table as it is today. None if there is no such panel.
pub fn load(sys: &System, name: &[u8], lines: &mut [Line; MAX_LINES]) -> Result<Option<(Panel, usize)>, Message> {
    let Some(loaded) = lang::load_from(&sys.db, PANELS, name, lines)? else {
        return Ok(None);
    };
    let table = word_of(loaded.first.bytes()).1;
    let schema = described_on(sys, table)?;
    match Panel::read(name, &schema, &lines[..loaded.count]) {
        Ok(panel) => Ok(Some((panel, loaded.count))),
        // its table changed since it was kept
        Err(fault) => {
            let mut message = fault.message;
            let _ = write!(message, " (line {} of the panel)", fault.line);
            Err(message)
        }
    }
}

/// `panels`: lists the panels, and the table each is described on.
pub fn list(sys: &mut System, out: &mut dyn Out) -> bool {
    let table = match lang::kept_in(&sys.db, PANELS) {
        Ok(table) => table,
        Err(message) => {
            out.line(Style::Error, message.bytes());
            return false;
        }
    };
    let mut listed = Table::new(PANELS, &[b"name", b"on", b"lines"]);
    let mut found = [(Text::new(), Text::new(), 0usize); shell::TABLE_ROWS];
    let (mut count, mut hidden) = (0, 0);
    if let Some(table) = table {
        let scanned = sys.db.scan(table, &mut |fields| {
            let (Stored::Text(name), Stored::Int(number), Stored::Text(text)) = (fields[1], fields[2], fields[3]) else {
                return;
            };
            let at = match found[..count].iter().position(|(known, ..)| known.bytes() == name) {
                Some(at) => at,
                None if count < found.len() => {
                    found[count].0 = Text::from_bytes(name);
                    count += 1;
                    count - 1
                }
                None => {
                    hidden += (number == 0) as usize;
                    return;
                }
            };
            match number {
                0 => found[at].1 = Text::from_bytes(word_of(text).1),
                _ if text.is_empty() => {}
                _ => found[at].2 += 1,
            }
        });
        if let Err(error) = scanned {
            out.line(Style::Error, engine_error(error, None).bytes());
            return false;
        }
    }
    if count == 0 {
        out.line(Style::Out, b"There is no panel yet. help l shows how to make one.");
        return true;
    }
    for (name, on, lines) in &found[..count] {
        let mut number = Text::new();
        let _ = write!(number, "{lines}");
        listed.row_to(&[name.bytes(), on.bytes(), number.bytes()], Text::from_bytes(b"show panel ").also(name.bytes()).bytes());
    }
    listed.more = hidden;
    shell::show(sys, out, &listed);
    true
}

/// `show panel name`: its lines, as they can be typed again.
pub fn show(sys: &mut System, name: &[u8], out: &mut dyn Out) -> bool {
    let mut lines = [Line::new(); MAX_LINES];
    let loaded = match lang::load_from(&sys.db, PANELS, name, &mut lines) {
        Ok(Some(loaded)) => loaded,
        Ok(None) => {
            out.line(Style::Error, unknown(name).bytes());
            return false;
        }
        Err(message) => {
            out.line(Style::Error, message.bytes());
            return false;
        }
    };
    let mut lower = Name::from_bytes(name);
    lower.lower();
    out.line(Style::Out, FixedStr::<COLS>::from_bytes(b"panel ").also(lower.bytes()).also(b" ").also(loaded.first.bytes()).bytes());
    for line in lines[..loaded.count].iter().filter(|line| !line.is_empty()) {
        out.line(Style::Out, FixedStr::<{ lang::LINE_MAX + 4 }>::from_bytes(b"    ").also(trim(line.bytes())).bytes());
    }
    out.line(Style::Out, b"end");
    true
}

/// `drop panel name`.
pub fn remove(sys: &mut System, name: &[u8], out: &mut dyn Out) -> bool {
    let table = match lang::kept_in(&sys.db, PANELS) {
        Ok(Some(table)) if exists(sys, name) => table,
        Ok(_) => {
            out.line(Style::Error, unknown(name).bytes());
            return false;
        }
        Err(message) => {
            out.line(Style::Error, message.bytes());
            return false;
        }
    };
    let removed = sys.db.delete(table, &mut |fields| matches!(fields[1], Stored::Text(of) if of.eq_ignore_ascii_case(name)));
    match removed.and_then(|_| sys.db.commit(&mut |_, _, _| {})) {
        Ok(()) => {
            let now = sys.ticks.max(1);
            sys.stored_at = now;
            let mut lower = Name::from_bytes(name);
            lower.lower();
            sys.events.notify(PANELS, Text::from_bytes(b"drop ").also(lower.bytes()).bytes(), now);
            out.line(Style::Out, FixedStr::<COLS>::from_bytes(b"Panel ").also(lower.bytes()).also(b" removed.").bytes());
            kept_again(sys, lower.bytes());
            true
        }
        Err(error) => {
            sys.db.rollback();
            out.line(Style::Error, engine_error(error, None).bytes());
            false
        }
    }
}

/// What is typed in a field of a form, and where in it.
#[derive(Clone, Copy)]
struct Input {
    text: Text,
    at: usize,
}

impl Input {
    const NONE: Input = Input { text: Text::new(), at: 0 };

    fn of(text: &[u8]) -> Input {
        let text = Text::from_bytes(text);
        Input { text, at: text.len() }
    }

    fn insert(&mut self, typed: u8) {
        let mut longer = Text::from_bytes(&self.text.bytes()[..self.at]);
        if self.text.len() < 48 && longer.push(typed) {
            longer.push_bytes(&self.text.bytes()[self.at..]);
            (self.text, self.at) = (longer, self.at + 1);
        }
    }

    /// Takes out the character at a place, if there is one.
    fn remove(&mut self, at: usize) {
        if at < self.text.len() {
            let mut shorter = Text::from_bytes(&self.text.bytes()[..at]);
            shorter.push_bytes(&self.text.bytes()[at + 1..]);
            self.text = shorter;
        }
    }
}

/// What the screen of a panel shows.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    List,
    /// The form, of the row `Open::id` or of a row to add: whether it is
    /// only shown, and the field that is typed.
    Form { shown: bool, at: usize },
    /// The row of that id is about to be removed: yes or no.
    Sure(u64),
    /// What a field may hold, to pick from: the field, and the one picked.
    Among { field: usize, at: usize },
}

/// What a key asked for, that the tick of the interface does: whatever
/// reads or writes tables.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Todo {
    Nothing,
    /// Read the row of that id into the form, to change it or to show it.
    Row(u64, bool),
    /// Write the form: a row more, or the row it was read from.
    Save,
    Remove(u64),
    /// Read what a field may hold.
    Among(usize),
    /// Do what an option or a key of the panel's own does: which one,
    /// and the id of the row it is for (0 for none).
    Act(usize, u64),
}

/// A panel that is open: what its list shows, and where in its table.
pub struct Open {
    pub panel: Panel,
    /// What is typed in the line to find, and what the list was read with.
    find: FixedStr<FIND>,
    found: FixedStr<FIND>,
    /// The id after which this page begins, and those of the pages
    /// before it, for the way back.
    after: u64,
    starts: [u64; BACK],
    back: usize,
    /// How many pages are before this one.
    page: usize,
    /// The page as it is shown: the names of the columns, then each row
    /// with its id.
    header: FixedStr<COLS>,
    ids: [u64; ROWS],
    lines: [FixedStr<COLS>; ROWS],
    count: usize,
    /// Whether rows follow this page.
    more: bool,
    /// What the panel has to say, and whether it is a failure.
    message: FixedStr<COLS>,
    failed: bool,
    /// The tick of the last reading, and of the last look at whether the
    /// tables changed since.
    read: u64,
    looked: u64,
    /// The page is to be read: by the interface at its next tick, not
    /// by the key or the command that asked, which are deep in the
    /// stack of their task already.
    stale: bool,
    /// Something was looked for with `Enter`: the reading that follows
    /// says so when no row holds it.
    said: bool,
    mode: Mode,
    todo: Todo,
    /// The row the form is of; none for a row that is added.
    id: Option<u64>,
    /// Whether the message is a refusal (a row the table would not
    /// take): said in red until the next key.
    refused: bool,
    /// In the list: the row the keys are on, or none for the line to
    /// find; and what was typed before a row, if something was.
    row: Option<usize>,
    option: Option<u8>,
    /// The id of the row the keys are on: a list that is read again
    /// keeps them on that row, wherever it is then.
    on: Option<u64>,
    /// What each field of the form holds, and what it held when the row
    /// was read: a change writes the fields that differ, and no other.
    inputs: [Input; COLUMNS],
    was: [Text; COLUMNS],
    /// What the field that was asked about may hold.
    choices: [Text; ROWS],
    choice_count: usize,
    /// The rows it is opened on: those whose column holds this value,
    /// which a row that is added takes. None for every row.
    only: Option<(Name, u64)>,
    /// The place of the panel it was opened from, to go back to.
    from: Option<usize>,
    /// The job that runs a program of this panel, until what it said is shown.
    job: Option<u32>,
}

impl Open {
    /// A panel just opened: its list is to be read.
    fn new(panel: Panel) -> Open {
        Open {
            panel,
            find: FixedStr::new(),
            found: FixedStr::new(),
            after: 0,
            starts: [0; BACK],
            back: 0,
            page: 0,
            header: FixedStr::new(),
            ids: [0; ROWS],
            lines: [FixedStr::new(); ROWS],
            count: 0,
            more: false,
            message: FixedStr::new(),
            failed: false,
            read: 0,
            looked: 0,
            stale: true,
            said: false,
            mode: Mode::List,
            todo: Todo::Nothing,
            id: None,
            refused: false,
            row: None,
            option: None,
            on: None,
            inputs: [Input::NONE; COLUMNS],
            was: [Text::new(); COLUMNS],
            choices: [Text::new(); ROWS],
            choice_count: 0,
            only: None,
            from: None,
            job: None,
        }
    }
}

/// What reading a page needs room for: the cursor that walks the table,
/// and the rows as values until their columns are laid out. It is the
/// interface's to keep (`Ui::page`), in memory that is no stack: 40 KB
/// that the stack of a task has not got to spare under a key or a
/// command.
pub struct Page {
    cursor: sql::Cursor,
    cells: [[Text; sql::MAX_COLUMNS]; ROWS + 1],
    ids: [u64; ROWS + 1],
    /// The lines of a panel, read again when what a field may hold is asked.
    lines: [Line; MAX_LINES],
}

impl Page {
    pub const fn new() -> Page {
        Page { cursor: sql::Cursor::new(), cells: [[Text::new(); sql::MAX_COLUMNS]; ROWS + 1], ids: [0; ROWS + 1], lines: [Line::new(); MAX_LINES] }
    }
}

impl Default for Page {
    fn default() -> Page {
        Page::new()
    }
}

/// The panels that are open, each at its place: the virtual screen
/// `SCREENS.len() + place`.
pub struct Panels {
    open: [Option<Open>; OPEN],
    /// Rows a list may show: what the screen has under the title bar,
    /// less the head and the foot of a panel. The interface says it.
    pub rows: usize,
}

impl Panels {
    pub const fn new() -> Panels {
        Panels { open: [const { None }; OPEN], rows: 20 }
    }
}

impl Default for Panels {
    fn default() -> Panels {
        Panels::new()
    }
}

/// The rows a list may show on a screen of so many rows, the first of
/// its panel being `top`.
pub fn rows_for(rows: usize, top: usize) -> usize {
    rows.saturating_sub(top + HEAD + FOOT).clamp(1, ROWS)
}

/// The place of the panel a screen shows, if it shows one that is open.
pub fn place_of(sys: &System, screen: usize) -> Option<usize> {
    screen.checked_sub(SCREENS.len()).filter(|place| sys.panels.open.get(*place).is_some_and(|open| open.is_some()))
}

/// Calls `visit` with every virtual screen there is, by its number and
/// its name: the three the system has, then the panels that are open.
pub fn screens(sys: &System, visit: &mut dyn FnMut(usize, &[u8])) {
    for (number, name) in SCREENS.iter().enumerate() {
        visit(number, name);
    }
    for (place, open) in sys.panels.open.iter().enumerate() {
        if let Some(open) = open {
            visit(SCREENS.len() + place, open.panel.name.bytes());
        }
    }
}

/// The name of the panel a screen shows, in the letters of a title.
pub fn title(sys: &System, screen: usize) -> Option<Name> {
    let mut name = sys.panels.open[place_of(sys, screen)?].as_ref()?.panel.name;
    name.upper();
    Some(name)
}

/// Opens a panel by its name, as a virtual screen of its own, and goes
/// to it. One that is open already is gone to. A panel opens for who
/// sits at the screen of the machine, and nobody else.
pub fn open(sys: &mut System, name: &[u8], out: &mut dyn Out) -> bool {
    let fail = |out: &mut dyn Out, message: &[u8]| {
        out.line(Style::Error, message);
        false
    };
    let seen = crate::jobs::in_turn(sys).is_none_or(|job| job.watch == crate::jobs::Watch::Screen);
    if sys.session != crate::events::SCREEN || sys.acting || !seen {
        return fail(out, sql::error(b"A panel opens at the screen of the machine: ", name).also(b".").bytes());
    }
    if let Some(place) = sys.panels.open.iter().position(|open| open.as_ref().is_some_and(|open| open.panel.name.bytes().eq_ignore_ascii_case(name))) {
        sys.screen = SCREENS.len() + place;
        return true;
    }
    let Some(place) = sys.panels.open.iter().position(|open| open.is_none()) else {
        return fail(out, b"Four panels are open: leave one with [F3].");
    };
    let mut lines = [Line::new(); MAX_LINES];
    let panel = match load(sys, name, &mut lines) {
        Ok(Some((panel, _))) => panel,
        Ok(None) => return fail(out, unknown(name).bytes()),
        Err(message) => return fail(out, message.bytes()),
    };
    sys.panels.open[place] = Some(Open::new(panel));
    sys.screen = SCREENS.len() + place;
    true
}

/// Leaves the panel of a place: its screen is no more, and the overview
/// is shown if it was that one.
pub fn close(sys: &mut System, place: usize) {
    // back to the panel it was opened from, if that one is still open
    let back = sys.panels.open[place].as_ref().and_then(|open| open.from).filter(|from| sys.panels.open[*from].is_some());
    sys.panels.open[place] = None;
    if sys.screen == SCREENS.len() + place {
        sys.screen = back.map_or(0, |from| SCREENS.len() + from);
    }
    for other in sys.panels.open.iter_mut().flatten() {
        if other.from == Some(place) {
            other.from = None;
        }
    }
}

/// A panel was kept again or removed: one that is open under that name
/// is read anew as it is described now, or left if it cannot be.
fn kept_again(sys: &mut System, name: &[u8]) {
    let Some(place) = sys.panels.open.iter().position(|open| open.as_ref().is_some_and(|open| open.panel.name.bytes().eq_ignore_ascii_case(name))) else {
        return;
    };
    let mut lines = [Line::new(); MAX_LINES];
    match load(sys, name, &mut lines) {
        Ok(Some((panel, _))) => {
            if let Some(open) = sys.panels.open[place].as_mut() {
                (open.panel, open.after, open.back, open.page, open.stale) = (panel, 0, 0, 0, true);
                (open.mode, open.todo, open.row, open.option) = (Mode::List, Todo::Nothing, None, None);
            }
        }
        _ => close(sys, place),
    }
}

/// Writes a value as a list shows it, in a width: a number to the right,
/// anything else to the left, cut when it is longer.
fn cell(line: &mut FixedStr<COLS>, value: &[u8], width: usize, number: bool) {
    let shown = &value[..value.len().min(width)];
    let pad = width - shown.len();
    for _ in 0..if number { pad } else { 0 } {
        line.push(b' ');
    }
    line.push_bytes(shown);
    for _ in 0..if number { 0 } else { pad } {
        line.push(b' ');
    }
}

/// Reads the page of a list from its table: the rows after `after`,
/// those that hold what is looked for when something is, as many as the
/// screen shows. What cannot be read is the panel's message, and its
/// list is then empty.
///
/// It keeps nothing large on the stack: the cursor and the rows are in
/// `room`. And it is called from the tick of the interface only
/// (`refresh`), never from under a key or a command.
#[inline(never)]
fn read(sys: &mut System, place: usize, room: &mut Page) {
    let Some(open) = sys.panels.open[place].as_ref() else {
        return;
    };
    let (panel, found, after, only) = (open.panel, open.found, open.after, open.only);
    let wanted = sys.panels.rows.clamp(1, ROWS);
    let listed = panel.listed;
    let mut numbers = [false; sql::MAX_COLUMNS];
    let mut count = 0;
    // something is looked for, and no listed column is a text to look in
    let mut nowhere = false;
    let fetched = (|| -> Result<(), Message> {
        let schema = described_on(sys, panel.table.bytes())?;
        // (room for names of 24 characters and a line to find made of quotes)
        let mut query = FixedStr::<720>::from_bytes(b"select id");
        // the id is asked for once, wherever the list shows it
        for column in panel.list[..listed].iter().filter(|column| column.bytes() != b"id") {
            query.push_bytes(b", ");
            query.push_bytes(column.bytes());
        }
        let _ = write!(query, " from {} where id > {}", panel.table, after);
        if let Some((column, value)) = only {
            let _ = write!(query, " and {column} = {value}");
        }
        // what is looked for: in the first text columns of the list, a quote written twice
        let mut looked = 0;
        for column in panel.list[..listed].iter().filter(|column| schema.columns[..schema.count].iter().zip(&schema.kinds).any(|(name, kind)| name == *column && *kind == xila_db::Kind::Text)) {
            if found.is_empty() || looked == LOOKED {
                break;
            }
            query.push_bytes(if looked == 0 { b" and (" } else { b" or " });
            query.push_bytes(column.bytes());
            query.push_bytes(b" like '%");
            for byte in found.bytes() {
                query.push_bytes(if *byte == b'\'' { b"''" } else { core::slice::from_ref(byte) });
            }
            query.push_bytes(b"%'");
            looked += 1;
        }
        if looked > 0 {
            query.push(b')');
        }
        nowhere = !found.is_empty() && looked == 0;
        room.cursor.open(&*sys, &NO_VARS, query.bytes())?;
        // one row more than the page shows, to know whether a page follows
        while count <= wanted {
            let Some(row) = room.cursor.next(&*sys, query.bytes())? else {
                break;
            };
            room.ids[count] = if let Value::Int(id) = row[0] { id as u64 } else { 0 };
            let mut next = 1;
            for at in 0..listed {
                let value = if panel.list[at].bytes() == b"id" {
                    row[0]
                } else {
                    next += 1;
                    row[next - 1]
                };
                room.cells[count][at] = value.shown();
                numbers[at] |= matches!(value, Value::Int(_) | Value::Dec(..));
            }
            count += 1;
        }
        Ok(())
    })();
    room.cursor.close();
    let now = sys.ticks;
    let Some(open) = sys.panels.open[place].as_mut() else {
        return;
    };
    (open.count, open.more, open.read, open.stale) = (0, false, now, false);
    let said = core::mem::take(&mut open.said);
    open.header.clear();
    if let Err(message) = fetched {
        open.message = FixedStr::from_bytes(message.bytes());
        open.failed = true;
        // no row is shown: the keys are on none
        (open.row, open.on, open.option) = (None, None, None);
        return;
    }
    // what was wrong the last time is no longer so
    if open.failed {
        open.message.clear();
        open.failed = false;
    }
    (open.more, open.count) = (count > wanted, count.min(wanted));
    // each column as wide as what it holds on this page and as its
    // name; the widest give way until the row fits
    let mut labels = [Label::new(); sql::MAX_COLUMNS];
    let mut widths = [0usize; sql::MAX_COLUMNS];
    for at in 0..listed {
        labels[at] = panel.label(&panel.list[at]);
        widths[at] = room.cells[..open.count].iter().map(|row| row[at].len()).max().unwrap_or(0).max(labels[at].len()).max(1);
    }
    // the widest texts give way first: a number that is cut is another number
    let space = COLS - 2 - OPT - 2 * listed.saturating_sub(1);
    while widths[..listed].iter().sum::<usize>() > space {
        let texts = (0..listed).filter(|at| !numbers[*at] && widths[*at] > 1).max_by_key(|at| widths[*at]);
        let Some(widest) = texts.or_else(|| (0..listed).filter(|at| widths[*at] > 1).max_by_key(|at| widths[*at])) else {
            break;
        };
        widths[widest] -= 1;
    }
    open.header.push(b' ');
    for at in 0..listed {
        open.header.push_bytes(if at > 0 { b"  " } else { b"" });
        cell(&mut open.header, labels[at].bytes(), widths[at], numbers[at]);
    }
    for row in 0..open.count {
        let mut line = FixedStr::from_bytes(b" ");
        for at in 0..listed {
            line.push_bytes(if at > 0 { b"  " } else { b"" });
            cell(&mut line, room.cells[row][at].bytes(), widths[at], numbers[at]);
        }
        (open.ids[row], open.lines[row]) = (room.ids[row], line);
    }
    // The keys stay on the row they were on, wherever it is now, with
    // what was typed before it. If it is gone they are on the row that
    // took its place, and what was typed is dropped: it was for another.
    let followed = open.on.and_then(|id| room.ids[..open.count].iter().position(|listed| *listed == id));
    if followed.is_none() {
        open.option = None;
    }
    open.row = followed.or(open.row.filter(|_| open.count > 0).map(|row| row.min(open.count - 1)));
    open.on = open.row.map(|row| room.ids[row]);
    if said && nowhere {
        open.message = FixedStr::from_bytes(b"No listed column is a text: there is nothing to look in.");
    } else if said && open.count == 0 && !open.found.is_empty() {
        open.message = FixedStr::from_bytes(b"No row holds: ").also(open.found.bytes());
    }
}

/// The field after one, or before it, among those that are typed: the
/// same when there is no other.
fn beside(panel: &Panel, at: usize, forward: bool) -> usize {
    let count = panel.count.max(1);
    let mut next = at;
    for _ in 0..count {
        next = if forward { (next + 1) % count } else { (next + count - 1) % count };
        if !panel.fields[next].shown {
            return next;
        }
    }
    at
}

/// Handles a key on the screen of a panel. A key asks; what reads or
/// writes tables is done at the next tick of the interface (`refresh`),
/// and until then the keys wait, but those that leave.
pub fn key(sys: &mut System, place: usize, event: KeyEvent) {
    let Some(open) = sys.panels.open[place].as_mut() else {
        return;
    };
    match (event.key, open.mode) {
        (Key::F(3), _) => return close(sys, place),
        // the overview, the panel staying as it is
        (Key::Esc, Mode::List | Mode::Form { .. }) => {
            sys.screen = 0;
            return;
        }
        _ => {}
    }
    if open.stale || open.todo != Todo::Nothing {
        return;
    }
    // what could not be read stays said until it is read again
    if !open.failed {
        open.message.clear();
    }
    open.refused = false;
    let say = |open: &mut Open, text: &[u8]| open.message = FixedStr::from_bytes(text);
    match open.mode {
        Mode::List => match event.key {
            Key::F(5) => open.stale = true,
            Key::F(6) => {
                open.inputs = [Input::NONE; COLUMNS];
                // a row added to a panel opened on some rows is one of them
                if let Some((column, value)) = open.only {
                    let mut held = Text::new();
                    let _ = write!(held, "{value}");
                    for (field, input) in open.panel.fields[..open.panel.count].iter().zip(open.inputs.iter_mut()) {
                        if field.column == column {
                            *input = Input::of(held.bytes());
                        }
                    }
                }
                (open.id, open.mode) = (None, Mode::Form { shown: false, at: beside(&open.panel, open.panel.count.max(1) - 1, true) });
            }
            Key::PageDown if !open.more => say(open, b"This is the last page."),
            Key::PageDown => {
                if open.back == BACK {
                    // the oldest way back is forgotten
                    open.starts.copy_within(1.., 0);
                    open.back -= 1;
                }
                open.starts[open.back] = open.after;
                (open.back, open.page) = (open.back + 1, open.page + 1);
                open.after = open.ids[open.count.saturating_sub(1)];
                (open.stale, open.option) = (true, None);
            }
            Key::PageUp if open.back == 0 && open.page > 0 => {
                // further back than is remembered: from the start again
                (open.after, open.page, open.stale, open.option) = (0, 0, true, None);
            }
            Key::PageUp if open.back == 0 => say(open, b"This is the first page."),
            Key::PageUp => {
                (open.back, open.page) = (open.back - 1, open.page - 1);
                (open.after, open.stale, open.option) = (open.starts[open.back], true, None);
            }
            // from the line to find down the rows, and back
            Key::Down | Key::Tab => {
                let next = match open.row {
                    None => 0,
                    Some(row) => row + 1,
                };
                if next < open.count {
                    (open.row, open.on, open.option) = (Some(next), Some(open.ids[next]), None);
                }
            }
            Key::Up => {
                let row = open.row.and_then(|row| row.checked_sub(1));
                (open.row, open.on, open.option) = (row, row.map(|row| open.ids[row]), None);
            }
            // (a letter beyond ASCII is one as any other: tables hold them)
            Key::Char(typed) if typed >= 0x20 && typed != 0x7f => match open.row {
                None => {
                    open.find.push(typed);
                }
                Some(_) if matches!(typed, b'2' | b'4' | b'5') || open.panel.actions[..open.panel.acts].iter().any(|action| action.trigger == Trigger::Option(typed)) => open.option = Some(typed),
                Some(_) if open.panel.acts == 0 => say(open, b"Before a row: 2 changes it, 4 removes it, 5 shows it."),
                Some(_) => say(open, b"Before a row: one of the digits of the line below."),
            },
            // a key of the panel's own: for the row the keys are on, if they are on one
            // (one program at a time: a key pressed again, or held, is not a program run twice)
            Key::F(number) if open.job.is_some() && open.panel.actions[..open.panel.acts].iter().any(|action| action.trigger == Trigger::Key(number)) => open.message = runs(open.job),
            Key::F(number) if open.panel.actions[..open.panel.acts].iter().any(|action| action.trigger == Trigger::Key(number)) => {
                let action = open.panel.actions[..open.panel.acts].iter().position(|action| action.trigger == Trigger::Key(number)).unwrap_or(0);
                open.todo = Todo::Act(action, open.row.filter(|row| *row < open.count).map_or(0, |row| open.ids[row]));
            }
            Key::Backspace if open.row.is_some() => open.option = None,
            Key::Backspace => {
                let shorter = open.find.len().saturating_sub(1);
                open.find = FixedStr::from_bytes(&open.find.bytes()[..shorter]);
            }
            Key::Enter => match open.row {
                None => (open.found, open.after, open.back, open.page, open.said, open.stale) = (open.find, 0, 0, 0, true, true),
                Some(row) if row < open.count => {
                    let id = open.ids[row];
                    match open.option.take() {
                        Some(b'2') => open.todo = Todo::Row(id, true),
                        Some(b'4') => {
                            open.mode = Mode::Sure(id);
                            open.message = sure(id);
                        }
                        Some(b'5') | None => open.todo = Todo::Row(id, false),
                        // an option of the panel's own
                        Some(digit) => match open.panel.actions[..open.panel.acts].iter().position(|action| action.trigger == Trigger::Option(digit)) {
                            Some(_) if open.job.is_some() => open.message = runs(open.job),
                            Some(action) => open.todo = Todo::Act(action, id),
                            None => open.todo = Todo::Row(id, false),
                        },
                    }
                }
                Some(_) => {}
            },
            _ => {}
        },
        Mode::Sure(id) => match event.key {
            Key::Enter => (open.mode, open.todo) = (Mode::List, Todo::Remove(id)),
            Key::F(12) | Key::Esc => {
                open.mode = Mode::List;
                say(open, b"Nothing was removed.");
            }
            _ => open.message = sure(id),
        },
        Mode::Form { shown, at } => {
            let field = open.panel.fields[at.min(COLUMNS - 1)];
            let input = &mut open.inputs[at.min(COLUMNS - 1)];
            match event.key {
                Key::F(12) => {
                    open.mode = Mode::List;
                    if !shown {
                        say(open, b"Nothing was changed.");
                    }
                }
                Key::Enter if shown => open.mode = Mode::List,
                Key::Enter => open.todo = Todo::Save,
                _ if shown => {}
                Key::Tab | Key::Down => open.mode = Mode::Form { shown, at: beside(&open.panel, at, true) },
                Key::Up => open.mode = Mode::Form { shown, at: beside(&open.panel, at, false) },
                // nothing is typed in what is only shown, nor in the column
                // a row to add is given by the panel it is added in
                _ if field.shown || (open.id.is_none() && open.only.is_some_and(|(column, _)| column == field.column)) => {}
                Key::F(4) if field.among => open.todo = Todo::Among(at),
                Key::F(4) => say(open, b"This field has no list of values to pick from."),
                Key::Char(typed) if typed >= 0x20 && typed != 0x7f => input.insert(typed),
                Key::Backspace if input.at > 0 => {
                    input.at -= 1;
                    input.remove(input.at);
                }
                Key::Delete => input.remove(input.at),
                Key::Left => input.at = input.at.saturating_sub(1),
                Key::Right => input.at = (input.at + 1).min(input.text.len()),
                Key::Home => input.at = 0,
                Key::End => input.at = input.text.len(),
                _ => {}
            }
        }
        Mode::Among { field, at } => match event.key {
            Key::Up => open.mode = Mode::Among { field, at: at.saturating_sub(1) },
            Key::Down => open.mode = Mode::Among { field, at: (at + 1).min(open.choice_count.saturating_sub(1)) },
            Key::Enter => {
                if at < open.choice_count {
                    open.inputs[field.min(COLUMNS - 1)] = Input::of(open.choices[at].bytes());
                }
                open.mode = Mode::Form { shown: false, at: field };
            }
            Key::F(12) | Key::Esc => open.mode = Mode::Form { shown: false, at: field },
            _ => {}
        },
    }
}

/// What is said of a key of a panel while the program of the one before runs.
fn runs(job: Option<u32>) -> FixedStr<COLS> {
    let mut message = FixedStr::new();
    let number = job.unwrap_or(0);
    let _ = write!(message, "Job {number} still runs: wait for it, or type job {number} end at the prompt.");
    message
}

/// What is asked before a row is removed, with the id of that row.
fn sure(id: u64) -> FixedStr<COLS> {
    let mut question = FixedStr::new();
    let _ = write!(question, "Remove row {id}? [Enter] removes it, [F12] keeps it.");
    question
}

/// What a line of a panel says a field may hold: what follows `among`.
fn among_of(line: &[u8]) -> Option<&[u8]> {
    let (_, rest) = word_of(trim(line));
    let (_, rest) = split_word(rest);
    let (_, rest) = text_then(trim(rest))?;
    let (next, after) = word_of(rest);
    let rest = if next.bytes() == b"shown" { after } else { rest };
    let (word, values) = word_of(rest);
    (word.bytes() == b"among" && !values.is_empty()).then_some(values)
}

/// Keeps the last thing a statement said, and whether it was a refusal.
struct Heard {
    text: FixedStr<COLS>,
    refused: bool,
}

impl Out for Heard {
    fn line(&mut self, style: Style, text: &[u8]) {
        (self.text, self.refused) = (FixedStr::from_bytes(text), style == Style::Error);
    }

    fn table(&mut self, _: &Table) {}
}

/// Writes a value of a form as SQL reads it, and says false when what
/// was typed is no value. A text goes between quotes, a quote in it
/// written twice: whatever is typed is the text. A number is worked out
/// here, with no column and no name to read, and only what it comes to
/// is written: a calculation may be typed (`6 * 2`), and nothing typed
/// there can be more than one value of one column (written as it was
/// typed, `1, label = 'x'` changed another column of the row). Nothing
/// typed is no value.
fn written(statement: &mut FixedStr<1024>, typed: &[u8], text: bool) -> bool {
    let typed = trim(typed);
    if typed.is_empty() {
        statement.push_bytes(b"null");
    } else if text {
        statement.push(b'\'');
        for byte in typed {
            statement.push_bytes(if *byte == b'\'' { b"''" } else { core::slice::from_ref(byte) });
        }
        statement.push(b'\'');
    } else {
        let mut lexer = sql::Lexer::new(typed);
        let worked = crate::expr::parse(&mut lexer, crate::expr::Names::without(&NO_VARS, b"")).and_then(|read| lexer.end().map(|_| read)).and_then(|read| read.eval(&[Value::Null; sql::MAX_COLUMNS]));
        match worked {
            Ok(number @ (Value::Int(_) | Value::Dec(..))) => statement.push_bytes(number.shown().bytes()),
            _ => return false,
        }
    }
    true
}

/// Does what a key asked of the tables: reads a row into the form,
/// writes the form, removes a row, reads what a field may hold. Called
/// at the tick of the interface, as a page is read, and for the same
/// reason. What is written goes through the statements of SQL, as the
/// screen of the machine: defaults and checks hold, and who listens to
/// the table hears of it.
#[inline(never)]
fn work(sys: &mut System, place: usize, room: &mut Page) {
    let Some(open) = sys.panels.open[place].as_mut() else {
        return;
    };
    let (todo, panel, id) = (core::mem::replace(&mut open.todo, Todo::Nothing), open.panel, open.id);
    let (inputs, was, only) = (open.inputs, open.was, open.only);
    let mut statement = FixedStr::<1024>::new();
    let text_of = |schema: &Schema, column: &Name| schema.columns[..schema.count].iter().zip(&schema.kinds).any(|(name, kind)| name == column && *kind == xila_db::Kind::Text);
    match todo {
        Todo::Nothing => {}
        Todo::Row(row, change) => {
            let _ = write!(statement, "select id");
            for field in &panel.fields[..panel.count] {
                statement.push_bytes(b", ");
                statement.push_bytes(field.column.bytes());
            }
            let _ = write!(statement, " from {} where id = {row}", panel.table);
            let read = room.cursor.open(&*sys, &NO_VARS, statement.bytes()).and_then(|_| room.cursor.next(&*sys, statement.bytes()));
            room.cursor.close();
            let Some(open) = sys.panels.open[place].as_mut() else {
                return;
            };
            match read {
                Ok(Some(values)) => {
                    for at in 0..panel.count {
                        open.was[at] = values[at + 1].shown();
                        open.inputs[at] = Input::of(open.was[at].bytes());
                    }
                    (open.id, open.mode) = (Some(row), Mode::Form { shown: !change, at: beside(&panel, panel.count.max(1) - 1, true) });
                }
                Ok(None) => (open.message, open.stale) = (FixedStr::from_bytes(b"This row is no longer there."), true),
                Err(message) => (open.message, open.refused) = (FixedStr::from_bytes(message.bytes()), true),
            }
        }
        Todo::Save | Todo::Remove(_) => {
            let schema = match described_on(sys, panel.table.bytes()) {
                Ok(schema) => schema,
                Err(message) => {
                    if let Some(open) = sys.panels.open[place].as_mut() {
                        (open.message, open.refused) = (FixedStr::from_bytes(message.bytes()), true);
                    }
                    return;
                }
            };
            let typed = || panel.fields[..panel.count].iter().zip(&inputs).filter(|(field, _)| !field.shown);
            let (mut nothing, mut same) = (false, false);
            // the first field that holds what is no value, if one does
            let mut wrong: Option<Label> = None;
            match (todo, id) {
                (Todo::Remove(row), _) => {
                    let _ = write!(statement, "delete from {} where id = {row}", panel.table);
                }
                (_, Some(row)) => {
                    // The fields that no longer hold what was read, and no
                    // other: a field that was not touched is not written
                    // back from what the form shows of it, which may be
                    // less than it holds (a text is shown by its first 48
                    // characters, an empty one as nothing), nor over what
                    // another wrote there meanwhile.
                    let changed = || panel.fields[..panel.count].iter().zip(&inputs).zip(&was).filter(|((field, input), was)| !field.shown && input.text.bytes() != was.bytes()).map(|(pair, _)| pair);
                    same = changed().next().is_none();
                    let _ = write!(statement, "update {} set ", panel.table);
                    for (at, (field, input)) in changed().enumerate() {
                        statement.push_bytes(if at > 0 { b", " } else { b"" });
                        statement.push_bytes(field.column.bytes());
                        statement.push_bytes(b" = ");
                        if !written(&mut statement, input.text.bytes(), text_of(&schema, &field.column)) {
                            wrong = wrong.or(Some(field.label));
                        }
                    }
                    let _ = write!(statement, " where id = {row}");
                }
                (_, None) => {
                    // a field where nothing was typed is left to the table:
                    // its value by default, or none
                    // (the column a panel is opened on is given by the panel, whatever the form holds there)
                    let given = || typed().filter(|(field, input)| !trim(input.text.bytes()).is_empty() && only.is_none_or(|(column, _)| column != field.column));
                    nothing = given().next().is_none();
                    let _ = write!(statement, "insert into {} (", panel.table);
                    if let Some((column, _)) = only {
                        let _ = write!(statement, "{column}, ");
                    }
                    for (at, (field, _)) in given().enumerate() {
                        statement.push_bytes(if at > 0 { b", " } else { b"" });
                        statement.push_bytes(field.column.bytes());
                    }
                    statement.push_bytes(b") values (");
                    if let Some((_, value)) = only {
                        let _ = write!(statement, "{value}, ");
                    }
                    for (at, (field, input)) in given().enumerate() {
                        statement.push_bytes(if at > 0 { b", " } else { b"" });
                        if !written(&mut statement, input.text.bytes(), text_of(&schema, &field.column)) {
                            wrong = wrong.or(Some(field.label));
                        }
                    }
                    statement.push(b')');
                }
            }
            let mut heard = Heard { text: FixedStr::new(), refused: false };
            if typed().next().is_none() && !matches!(todo, Todo::Remove(_)) {
                (heard.text, heard.refused) = (FixedStr::from_bytes(b"This panel has no field that is typed."), true);
            } else if same {
                heard.text = FixedStr::from_bytes(b"Nothing was changed.");
            } else if nothing {
                (heard.text, heard.refused) = (FixedStr::from_bytes(b"Nothing was typed: there is no row to add."), true);
            } else if let Some(label) = wrong {
                // nothing is run with a field that is not one value
                (heard.text, heard.refused) = (FixedStr::from_bytes(label.bytes()).also(b" takes a number, or a calculation."), true);
            } else if statement.len() == 1024 {
                // (it holds seven texts of 48 quotes and more; said rather than run cut)
                (heard.text, heard.refused) = (FixedStr::from_bytes(b"What is typed is too long to be written."), true);
            } else {
                // as who sits at the screen of the machine
                let session = core::mem::replace(&mut sys.session, crate::events::SCREEN);
                shell::change(sys, statement.bytes(), &mut heard);
                sys.session = session;
            }
            let Some(open) = sys.panels.open[place].as_mut() else {
                return;
            };
            // a row that went while its form was typed: nothing was written
            let gone = heard.text.bytes().starts_with(b"0 rows ");
            (open.message, open.refused) = (if gone { FixedStr::from_bytes(b"This row is no longer there.") } else { heard.text }, heard.refused);
            if !heard.refused {
                // written: the list again, read anew
                (open.mode, open.stale) = (Mode::List, true);
            }
        }
        Todo::Act(index, row) => {
            // what it does is read from its line, in the panel as it is kept now
            let action = panel.actions[index.min(ACTIONS - 1)];
            let line = match lang::load_from(&sys.db, PANELS, panel.name.bytes(), &mut room.lines) {
                Ok(Some(loaded)) if action.line < loaded.count => room.lines[action.line],
                _ => Line::new(),
            };
            let said = match does(after_label(line.bytes())) {
                Some(Does::Open { panel: name, column }) => opened(sys, place, Name::from_bytes(name), Name::from_bytes(column), row, room),
                Some(Does::Run(call)) => ran(sys, place, &panel, call, row, room),
                None => Err(Message::from_bytes(b"This panel was changed since it was opened: leave it and open it again.")),
            };
            if let (Err(message), Some(open)) = (said, sys.panels.open[place].as_mut()) {
                (open.message, open.refused) = (FixedStr::from_bytes(message.bytes()), true);
            }
        }
        Todo::Among(at) => {
            let field = panel.fields[at.min(COLUMNS - 1)];
            let mut choices = [Text::new(); ROWS];
            let (mut count, mut more) = (0, false);
            let found = (|| -> Result<(), Message> {
                let loaded = lang::load_from(&sys.db, PANELS, panel.name.bytes(), &mut room.lines)?.ok_or_else(|| unknown(panel.name.bytes()))?;
                let line = room.lines.get(field.line).filter(|_| field.line < loaded.count).copied().unwrap_or(Line::new());
                let Some(values) = among_of(line.bytes()) else {
                    return Ok(());
                };
                if word_of(values).0.bytes() == b"select" {
                    // the first column of what it selects; a select that sorts is
                    // worked out whole, and has no room for more than 32 rows
                    if room.cursor.open(&*sys, &NO_VARS, values).is_err_and(|message| message.bytes().starts_with(b"A loop takes")) {
                        return Err(Message::from_bytes(b"A list that is sorted proposes 32 values at most: this one has more."));
                    }
                    room.cursor.open(&*sys, &NO_VARS, values)?;
                    while let Some(row) = room.cursor.next(&*sys, values)? {
                        if count == ROWS {
                            more = true;
                            break;
                        }
                        choices[count] = row[0].shown();
                        count += 1;
                    }
                    return Ok(());
                }
                // values written there: read as they were when the panel was kept
                literals(values, &mut |value| {
                    if count < ROWS {
                        choices[count] = value;
                        count += 1;
                    } else {
                        more = true;
                    }
                });
                Ok(())
            })();
            room.cursor.close();
            let Some(open) = sys.panels.open[place].as_mut() else {
                return;
            };
            match found {
                Err(message) => (open.message, open.refused) = (FixedStr::from_bytes(message.bytes()), true),
                Ok(()) if count == 0 => open.message = FixedStr::from_bytes(b"There is nothing to pick from."),
                Ok(()) => {
                    if more {
                        open.message = FixedStr::from_bytes(b"The first 40 values are proposed: there are more.");
                    }
                    (open.choices, open.choice_count) = (choices, count);
                    // on the value the field holds, if it is one of them
                    let held = choices[..count].iter().position(|choice| choice.bytes() == trim(inputs[at.min(COLUMNS - 1)].text.bytes())).unwrap_or(0);
                    open.mode = Mode::Among { field: at, at: held };
                }
            }
        }
    }
}

/// Reads the values written after `among`: texts between quotes and
/// numbers, with commas between them. Hands each to `each`, and says
/// false when they are not written so.
fn literals(values: &[u8], each: &mut dyn FnMut(Text)) -> bool {
    let mut rest = trim(values);
    loop {
        let (value, after) = match text_then(rest) {
            Some((text, after)) => (unquoted(text), after),
            None => {
                let end = rest.iter().position(|byte| *byte == b',').unwrap_or(rest.len());
                let number = trim(&rest[..end]);
                if number.is_empty() || !number.iter().all(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'.')) {
                    return false;
                }
                (Text::from_bytes(number), trim(&rest[end..]))
            }
        };
        each(value);
        match after.strip_prefix(b",") {
            Some(more) if !trim(more).is_empty() => rest = trim(more),
            Some(_) => return false,
            None => return after.is_empty(),
        }
    }
}

/// Opens the panel an option names, on the rows that belong to a row
/// of this one: those whose column holds its id. Its screen is shown,
/// and leaving it goes back to this one.
fn opened(sys: &mut System, place: usize, name: Name, column: Name, row: u64, room: &mut Page) -> Result<(), Message> {
    if row == 0 {
        return Err(Message::from_bytes(b"Go down to a row first: this opens what belongs to one."));
    }
    let (panel, _) = load(sys, name.bytes(), &mut room.lines)?.ok_or_else(|| unknown(name.bytes()))?;
    let schema = described_on(sys, panel.table.bytes())?;
    let Some(found) = schema.columns[..schema.count].iter().position(|known| known.bytes().eq_ignore_ascii_case(column.bytes())) else {
        return Err(sql::error(b"The panel ", name.bytes()).also(b" is on a table with no column ").also(column.bytes()).also(b"."));
    };
    // the id of a row is a whole number, and no row is given its own id
    if found == 0 || schema.kinds[found] != xila_db::Kind::Int {
        return Err(sql::error(b"The column ", column.bytes()).also(b" of ").also(name.bytes()).also(b" holds no whole number: it cannot hold the id of a row."));
    }
    let column = schema.columns[found];
    // the one that is open under that name is opened on these rows; else a place that is free
    let known = sys.panels.open.iter().position(|open| open.as_ref().is_some_and(|open| open.panel.name == panel.name));
    if known == Some(place) {
        return Err(Message::from_bytes(b"A panel does not open itself."));
    }
    // opened on other rows, the one that is open is made new: not under what is being typed or done there
    if known.and_then(|at| sys.panels.open[at].as_ref()).is_some_and(|open| open.mode != Mode::List || open.todo != Todo::Nothing || open.job.is_some()) {
        return Err(sql::error(name.bytes(), b" is open on a form: finish it there, or leave it, first."));
    }
    let Some(at) = known.or_else(|| sys.panels.open.iter().position(|open| open.is_none())) else {
        return Err(Message::from_bytes(b"Four panels are open: leave one with [F3]."));
    };
    let mut open = Open::new(panel);
    (open.only, open.from) = (Some((column, row)), Some(place));
    sys.panels.open[at] = Some(open);
    sys.screen = SCREENS.len() + at;
    Ok(())
}

/// Runs the program an option or a key names, for who sits at the
/// screen of the machine. The values it is given are worked out with
/// the columns of the row as names, when it is for a row. It runs as a
/// job nobody waits for, whose last line the panel says when it has
/// ended (`refresh`); on a system with no rooms for jobs it runs at
/// once, to its end.
fn ran(sys: &mut System, place: usize, panel: &Panel, call: &[u8], row: u64, room: &mut Page) -> Result<(), Message> {
    let mut names = crate::expr::Vars::new();
    names.clock = sys.now();
    if row != 0 {
        let mut query = FixedStr::<96>::new();
        let _ = write!(query, "select * from {} where id = {row}", panel.table);
        let read = room.cursor.open(&*sys, &NO_VARS, query.bytes()).and_then(|_| room.cursor.next(&*sys, query.bytes()));
        let values = read.map(|values| values.map(|values| (values, room.cursor.headers().len())));
        let found = match values {
            Ok(Some((values, columns))) => {
                for (header, value) in room.cursor.headers().iter().zip(&values).take(columns) {
                    names.set(header.bytes(), *value);
                }
                Ok(())
            }
            Ok(None) => Err(Message::from_bytes(b"This row is no longer there.")),
            Err(message) => Err(message),
        };
        room.cursor.close();
        found?;
    }
    let call = Line::from_bytes(call);
    let mut heard = Heard { text: FixedStr::new(), refused: false };
    // as who sits at the screen of the machine, the columns of the row being its names for that time
    let session = core::mem::replace(&mut sys.session, crate::events::SCREEN);
    core::mem::swap(&mut sys.vars[crate::events::SCREEN], &mut names);
    let started = if crate::jobs::places(sys) > 0 {
        crate::jobs::begin(sys, crate::jobs::What::Program(call.bytes()), crate::jobs::Watch::Nobody, &mut heard).map(Some)
    } else {
        crate::machine::lines(sys, &[call], &mut heard);
        Ok(None)
    };
    core::mem::swap(&mut sys.vars[crate::events::SCREEN], &mut names);
    sys.session = session;
    // with the keys on no row, a value that is a column of the rows has no name: said as what is missing
    let named = |said: &[u8]| said.strip_prefix(b"Unknown name: ").map(Name::from_bytes);
    let missing = match (row, named(heard.text.bytes()).or(started.as_ref().err().and_then(|message| named(message.bytes())))) {
        (0, Some(name)) => sys.schema(panel.table.bytes()).is_some_and(|schema| schema.columns[..schema.count].contains(&name)),
        _ => false,
    };
    let Some(open) = sys.panels.open[place].as_mut() else {
        return Ok(());
    };
    if missing {
        (open.message, open.refused, open.stale) = (FixedStr::from_bytes(b"This key is for a row: go down to one first."), true, true);
        return Ok(());
    }
    match started {
        Ok(Some(number)) => {
            open.job = Some(number);
            open.message.clear();
            let _ = write!(open.message, "Job {number} runs: its last line will be said here.");
        }
        // run at once: what it said last, and the list read again
        Ok(None) => (open.message, open.refused, open.stale) = (heard.text, heard.refused, true),
        Err(message) => (open.message, open.refused) = (if message.is_empty() { heard.text } else { FixedStr::from_bytes(message.bytes()) }, true),
    }
    Ok(())
}

/// The text between the quotes of a text as it is written, of any
/// length a value has.
fn unquoted(written: &[u8]) -> Text {
    let inner = written.strip_prefix(b"'").and_then(|rest| rest.strip_suffix(b"'")).unwrap_or(written);
    let mut text = Text::new();
    let mut bytes = inner.iter();
    while let Some(byte) = bytes.next() {
        if *byte == b'\'' {
            bytes.next();
        }
        text.push(*byte);
    }
    text
}

/// Reads the list of the panel that is shown when it is to be read: it
/// was just opened, or something was stored since it was read. A second
/// apart at most for the second reason, so that a job that writes many
/// rows does not have it read as many times. Returns whether it was
/// read. The interface calls this at each of its ticks: the page of a
/// panel is read from there, where the stack of the task is shallow,
/// and never from under a key or a command. (A panel opened by its name
/// froze the system in QEMU, then one whose line to find was used: the
/// cursor, the page and the SELECT stood on top of frames that were
/// deep already.)
pub fn refresh(sys: &mut System, room: &mut Page) -> bool {
    // what a key asked of the tables is done, whichever screen is shown
    // by now: a row that was to be written is written
    let mut asked = false;
    for place in 0..OPEN {
        if sys.panels.open[place].as_ref().is_some_and(|open| open.todo != Todo::Nothing) {
            work(sys, place, room);
            asked = true;
        }
    }
    // a program a panel ran has ended: its last line is said there, and the list is read again
    for place in 0..OPEN {
        let Some(number) = sys.panels.open[place].as_ref().and_then(|open| open.job) else {
            continue;
        };
        // what the panel says meanwhile (a question, a form) is not written over: it waits for the list
        if sys.panels.open[place].as_ref().is_some_and(|open| open.mode != Mode::List) {
            continue;
        }
        let ended = match crate::jobs::find(sys, number) {
            Ok(at) => {
                let job = &sys.jobs.list()[at];
                job.ended().then(|| {
                    let failed = job.state != crate::jobs::State::Done;
                    let mut last = FixedStr::<COLS>::new();
                    // its last line; with the one before when it failed, which says why
                    let (mut before, mut end): (&[u8], &[u8]) = (b"", b"");
                    for line in job.said.lines() {
                        (before, end) = (end, line);
                    }
                    for line in [before, end].iter().filter(|line| !line.is_empty()).skip(if failed { 0 } else { 1 }.min([before, end].iter().filter(|line| !line.is_empty()).count().saturating_sub(1))) {
                        last.push_bytes(if last.is_empty() { b"" } else { b" " });
                        // (the cells of a table it showed were kept apart by tabs)
                        for byte in line.iter().take(COLS - 2 - last.len().min(COLS - 2)) {
                            last.push(if *byte == b'\t' { b' ' } else { *byte });
                        }
                    }
                    if last.is_empty() {
                        let _ = write!(last, "{}", crate::jobs::about(number, crate::jobs::told(job.state)));
                        last.push(b'.');
                    }
                    (last, failed)
                })
            }
            // no longer listed: eight others began since
            Err(_) => {
                let mut gone = FixedStr::<COLS>::new();
                let _ = write!(gone, "Job {number} is no longer listed: job_log says how it went.");
                Some((gone, false))
            }
        };
        if let (Some((last, failed)), Some(open)) = (ended, sys.panels.open[place].as_mut()) {
            (open.job, open.message, open.refused, open.stale) = (None, last, failed, true);
            asked = true;
        }
    }
    // then the list of the panel that is shown, if it is to be read
    let Some(place) = place_of(sys, sys.screen) else {
        return asked;
    };
    let now = sys.ticks;
    let stored = sys.stored_at;
    let Some(open) = sys.panels.open[place].as_mut() else {
        return asked;
    };
    // (a form that is being typed is not disturbed by what others store)
    if !open.stale && (open.mode != Mode::List || stored < open.read || now < open.looked + AGAIN) {
        return asked;
    }
    open.looked = now;
    read(sys, place, room);
    true
}

/// Draws the panel of a place on the rows of the grid from `top` down:
/// its list, its form, or what a field may hold.
pub fn draw(sys: &System, place: usize, grid: &mut Grid, top: usize) {
    let Some(open) = sys.panels.open[place].as_ref() else {
        return;
    };
    let bg = role::TERM_BASE;
    let blank = Cell::new(b' ', role::TERM_OUT, bg);
    for row in top..grid.rows {
        grid.fill(row, 0, COLS, blank);
    }
    let last = grid.rows.saturating_sub(1);
    let right = |grid: &mut Grid, text: &[u8]| {
        grid.text(COLS - 1 - text.len().min(COLS - 1), top + 1, text, role::TERM_OUT, bg);
    };
    let end = grid.text(1, top + 1, open.panel.title.bytes(), role::TERM_PROMPT, bg);
    if let Some((column, value)) = open.only {
        let mut rows = FixedStr::<40>::new();
        let _ = write!(rows, "  ({column} = {value})");
        grid.text(end, top + 1, rows.bytes(), role::TERM_OUT, bg);
    }
    grid.text(1, last.saturating_sub(2), open.message.bytes(), if open.failed || open.refused { role::ERROR } else { role::TERM_CMD }, bg);
    match open.mode {
        Mode::List | Mode::Sure(_) => {
            let mut page = FixedStr::<16>::new();
            let _ = write!(page, "page {}", open.page + 1);
            right(grid, page.bytes());
            let after = grid.text(1, top + 2, b"Find: ", role::TERM_OUT, bg);
            let end = grid.text(after, top + 2, open.find.bytes(), role::TERM_CMD, bg);
            if open.row.is_none() && open.mode == Mode::List {
                grid.put(end, top + 2, Cell { ch: b' ', fg: role::TERM_CMD, bg, fx: fx::PULSE });
            }
            if !open.header.is_empty() {
                grid.text(1, top + 4, b"Opt", role::TERM_PROMPT, bg);
            }
            grid.text(OPT, top + 4, open.header.bytes(), role::TERM_PROMPT, bg);
            for (at, line) in open.lines[..open.count].iter().enumerate() {
                if top + HEAD + at + FOOT > last {
                    break;
                }
                // what is typed before the row the keys are on, or its place
                let (mark, pulse) = match (open.row == Some(at), open.option) {
                    (true, Some(option)) => (option, fx::PULSE),
                    (true, None) => (b'_', fx::PULSE),
                    (false, _) => (b'_', 0),
                };
                grid.put(2, top + HEAD + at, Cell { ch: mark, fg: role::TERM_CMD, bg, fx: pulse });
                grid.text(OPT, top + HEAD + at, line.bytes(), role::TERM_OUT, bg);
            }
            if open.count == 0 && !open.failed && open.message.is_empty() && !open.stale {
                grid.text(1, top + HEAD, b"(no rows)", role::TERM_OUT, bg);
            }
            if matches!(open.mode, Mode::Sure(_)) {
                grid.text(1, last, b"Enter=Remove  F12=Keep", role::TERM_PROMPT, bg);
            } else {
                // the system's options, then the panel's own, and its keys
                grid.text(1, last - 1, open.panel.options().bytes(), role::TERM_PROMPT, bg);
                grid.text(1, last, b"F3=Exit  F5=Refresh  F6=Add  PgUp/PgDn=Pages  Enter=Go  Esc=Overview", role::TERM_PROMPT, bg);
            }
        }
        Mode::Form { shown, at } => {
            let mut what = FixedStr::<24>::new();
            let _ = match (open.id, shown) {
                (None, _) => write!(what, "a row to add"),
                (Some(id), true) => write!(what, "row {id}"),
                (Some(id), false) => write!(what, "row {id}, changed"),
            };
            right(grid, what.bytes());
            // the labels, then dots up to the values, which start in one column
            // (a label longer than 26 characters is cut there: a value has 48)
            let width = open.panel.fields[..open.panel.count].iter().map(|field| field.label.len()).max().unwrap_or(0).min(26) + 3;
            for (index, (field, input)) in open.panel.fields[..open.panel.count].iter().zip(&open.inputs).enumerate() {
                let row = top + 3 + index;
                if row + FOOT > last {
                    break;
                }
                let end = grid.text(1, row, &field.label.bytes()[..field.label.len().min(26)], role::TERM_OUT, bg);
                grid.fill(row, end + 1, 1 + width, Cell::new(b'.', role::TERM_OUT, bg));
                let given = open.id.is_none() && open.only.is_some_and(|(column, _)| column == field.column);
                let typed = !shown && !field.shown && !given;
                let from = grid.text(1 + width, row, b" : ", role::TERM_OUT, bg);
                grid.text(from, row, input.text.bytes(), if typed { role::TERM_CMD } else { role::TERM_OUT }, bg);
                if typed && index == at {
                    let under = input.text.bytes().get(input.at).copied().unwrap_or(b' ');
                    grid.put(from + input.at, row, Cell { ch: under, fg: role::TERM_CMD, bg, fx: fx::PULSE });
                }
            }
            grid.text(1, last, if shown { &b"Enter=Back  F12=Back  F3=Exit"[..] } else { b"Enter=Save  Tab=Next field  F4=Values  F12=Cancel  F3=Exit" }, role::TERM_PROMPT, bg);
        }
        Mode::Among { field, at } => {
            right(grid, open.panel.fields[field.min(COLUMNS - 1)].label.bytes());
            // the values around the one the keys are on, as many as the screen has rows for
            let room = last.saturating_sub(top + 3 + FOOT).max(1);
            let first = at.saturating_sub(room - 1);
            for (shown, choice) in open.choices[..open.choice_count].iter().enumerate().skip(first).take(room) {
                let row = top + 3 + shown - first;
                let (fg, back) = if shown == at { (role::TERM_BASE, role::TERM_CMD) } else { (role::TERM_OUT, bg) };
                grid.fill(row, 1, 52, Cell::new(b' ', fg, back));
                grid.text(2, row, choice.bytes(), fg, back);
            }
            grid.text(1, last, b"Enter=Pick  Up/Down=Move  F12=Back", role::TERM_PROMPT, bg);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shell::tests::Lines;

    /// Types lines at the prompt, and returns what the last one said.
    pub(crate) fn said(sys: &mut System, lines: &[&str]) -> Vec<String> {
        let mut last = Vec::new();
        for line in lines {
            let mut out = Lines::default();
            shell::exec(sys, line.as_bytes(), &mut out);
            last = out.lines;
        }
        last
    }

    /// A system with a table of things, and a few of them.
    pub(crate) fn stocked() -> Box<System> {
        let mut sys = Box::new(System::new());
        said(&mut sys, &["create table items (label text, category text, place text, qty int)", "insert into items values ('Oscilloscope', 'measure', 'bench 2', 1), ('Soldering iron', 'tools', 'bench 1', 2), ('Cat6 cable 5m', 'cables', 'shelf B', 12)"]);
        sys
    }

    #[test]
    fn a_panel_is_kept_shown_listed_and_dropped() {
        let mut sys = stocked();
        assert_eq!(said(&mut sys, &["panels"]), ["There is no panel yet. help l shows how to make one."]);
        let typed = ["panel Stock on Items", "title 'The stock'", "# what is where", "list label, place, qty", "field label 'Label'", "field place 'Where it''s kept' among 'bench 1', 'bench 2'", "field qty 'In stock' shown", "end"];
        assert_eq!(said(&mut sys, &typed), ["Panel stock kept: 6 lines."]);
        assert_eq!(
            said(&mut sys, &["show panel STOCK"]),
            ["panel stock on items", "    title 'The stock'", "    # what is where", "    list label, place, qty", "    field label 'Label'", "    field place 'Where it''s kept' among 'bench 1', 'bench 2'", "    field qty 'In stock' shown", "end"]
        );
        assert_eq!(said(&mut sys, &["panels"]), ["     name   on     lines", " [1] stock  items  6"]);
        assert_eq!(said(&mut sys, &["1"])[0], "panel stock on items");
        // it is read as it is described: its title, its list, its labels
        let mut lines = [Line::new(); MAX_LINES];
        let (panel, count) = load(&sys, b"stock", &mut lines).unwrap().unwrap();
        assert_eq!((count, panel.title.bytes(), panel.table.bytes(), panel.listed, panel.count), (6, &b"The stock"[..], &b"items"[..], 3, 3));
        assert_eq!(panel.list[..3].iter().map(|column| String::from_utf8_lossy(column.bytes()).into_owned()).collect::<Vec<_>>(), ["label", "place", "qty"]);
        assert_eq!((panel.label(&panel.list[1]).bytes(), panel.fields[1].shown, panel.fields[2].shown, panel.fields[1].line), (&b"Where it's kept"[..], false, true, 4));
        // typed again it is replaced; with nothing said, it lists every column but the id under its name
        assert_eq!(said(&mut sys, &["panel stock on items", "end"]), ["Panel stock replaced: 0 lines."]);
        let (panel, _) = load(&sys, b"stock", &mut lines).unwrap().unwrap();
        // ...and every column but the id is a field of its form
        assert_eq!((panel.title.bytes(), panel.listed, panel.count, panel.label(&panel.list[3]).bytes()), (&b"stock"[..], 4, 4, &b"qty"[..]));
        assert!(panel.fields[..4].iter().all(|field| !field.shown && !field.among));
        assert_eq!(said(&mut sys, &["select count(*) from panels"]), ["   count", "   2"]);
        // dropped
        assert_eq!(said(&mut sys, &["drop panel stock"]), ["Panel stock removed."]);
        assert_eq!(said(&mut sys, &["drop panel stock"]), ["Unknown panel: stock. Type panels to list them."]);
        assert_eq!(said(&mut sys, &["show panel stock"]), ["Unknown panel: stock. Type panels to list them."]);
        assert!(load(&sys, b"stock", &mut lines).unwrap().is_none());
    }

    #[test]
    fn what_is_wrong_with_a_panel_is_told_and_it_is_not_kept() {
        let mut sys = stocked();
        let not_kept = |line: usize| format!("Line {line} is not right: the panel is not kept.");
        let written = "A panel is written: panel stock on items";
        for first in ["panel stock", "panel stock items", "panel stock on", "panel stock on items now", "panel on items"] {
            assert_eq!(said(&mut sys, &[first, "end"]), [written], "{first}");
        }
        let named = "A panel is named by a letter, then letters, digits or _, and not as a command.";
        assert_eq!(said(&mut sys, &["panel 2stock on items", "end"]), [named]);
        assert_eq!(said(&mut sys, &["panel tables on items", "end"]), [named]);
        assert_eq!(said(&mut sys, &["panel while on items", "end"]), [named]);
        assert_eq!(said(&mut sys, &["panel stock on nothing", "end"]), ["Unknown table: nothing. Type tables to list them."]);
        assert_eq!(said(&mut sys, &["panel stock on tasks", "end"]), ["A panel is described on a stored table: tasks is a view of the system."]);
        // a program and a panel do not share a name
        said(&mut sys, &["program report", "say 1", "end"]);
        assert_eq!(said(&mut sys, &["panel report on items", "end"]), ["report is a program: a panel cannot have its name."]);
        said(&mut sys, &["panel things on items", "end"]);
        assert_eq!(said(&mut sys, &["program things", "say 1", "end"]), ["things is a panel: a program cannot have its name."]);
        // its lines
        let faults: [(&[&str], &str); 12] = [
            (&["say 1"], "A line of a panel starts with title, list, field, option or key."),
            (&["title Stock"], "A title is a text between quotes, 32 characters at most: title 'Stock'"),
            (&["title 'a title that is far too long to be the title of a panel'"], "A title is a text between quotes, 32 characters at most: title 'Stock'"),
            (&["title ''"], "A title is a text between quotes, 32 characters at most: title 'Stock'"),
            (&["list label, colour"], "Unknown column: colour. The table items has no such column."),
            (&["list label,"], "A list names columns, with commas between them: list label, place"),
            (&["field"], "A field is a column and its label, 32 characters at most: field place 'Where'"),
            (&["field place"], "A field is a column and its label, 32 characters at most: field place 'Where'"),
            (&["field colour 'Colour'"], "Unknown column: colour. The table items has no such column."),
            (&["field id 'Number'"], "The id of a row is given by the system: it is no field."),
            (&["field place 'Where' hidden"], "After the label of a field: shown, or among and what it may hold."),
            (&["field place 'Where' among"], "After the label of a field: shown, or among and what it may hold."),
        ];
        for (lines, message) in faults {
            let mut typed = vec!["panel stock on items", "title 'Stock'"];
            typed.extend_from_slice(lines);
            typed.push("end");
            assert_eq!(said(&mut sys, &typed), [message.to_string(), not_kept(2)], "{lines:?}");
        }
        assert_eq!(said(&mut sys, &["panel stock on items", "field place 'Where'", "field PLACE 'Again'", "end"]), ["The column place has a field already.".to_string(), not_kept(2)]);
        // what a field may hold: values with commas between them, or a select
        let among = "After among: values with commas between them ('a', 'b', 3), or a select.";
        for values in ["'a',, 'b'", "'a', 'b',", "'unclosed", "a word", "1 2"] {
            assert_eq!(said(&mut sys, &["panel stock on items", &format!("field place 'Where' among {values}"), "end"]), [among.to_string(), not_kept(1)], "{values}");
        }
        let listing = "A list names columns, with commas between them: list label, place";
        assert_eq!(said(&mut sys, &["panel stock on items", "list", "end"]), [listing.to_string(), not_kept(1)]);
        assert_eq!(said(&mut sys, &["panel stock on items", "list label,, place", "end"]), [listing.to_string(), not_kept(1)]);
        // what is asked for without its name
        assert_eq!(said(&mut sys, &["show panel"]), ["Type show panel and its name: show panel stock"]);
        assert_eq!(said(&mut sys, &["drop panel"]), ["Type drop panel and its name: drop panel stock"]);
        // a program that runs by itself is named after what starts it: not as a panel
        said(&mut sys, &["panel on_insert_items on items", "end"]);
        assert_eq!(said(&mut sys, &["on insert into items", "say 1", "end"]), ["on_insert_items is a panel: a program cannot have its name."]);
        said(&mut sys, &["drop panel on_insert_items"]);
        assert_eq!(said(&mut sys, &["show panel stock"]), ["Unknown panel: stock. Type panels to list them."]);
        // one that was kept stays as it was when it is typed again wrong
        said(&mut sys, &["panel stock on items", "title 'Good'", "end"]);
        said(&mut sys, &["panel stock on items", "title bad", "end"]);
        assert_eq!(said(&mut sys, &["show panel stock"])[1], "    title 'Good'");
        // made at the prompt, not inside a block
        assert_eq!(said(&mut sys, &["if 1 = 1", "panel inner on items", "end", "end"])[0], "A panel is made at the prompt, not inside a block or a program.");
        // a table of that name that is not the system's is left alone
        said(&mut sys, &["drop table panels", "create table panels (title text)"]);
        assert_eq!(said(&mut sys, &["panel stock on items", "end"]), ["The table panels is not the one the system keeps panels in."]);
        assert_eq!(said(&mut sys, &["panels"]), ["The table panels is not the one the system keeps panels in."]);
    }

    #[test]
    fn a_panel_has_options_and_keys_of_its_own() {
        let mut sys = stocked();
        let typed = ["panel stock on items", "option 8 'Moves' open moves where item = id", "option 9 'Zero' run zero(id)", "key f8 'Count' run recount(id, qty + 1)", "key F10 'Report' run report", "end"];
        assert_eq!(said(&mut sys, &typed), ["Panel stock kept: 4 lines."]);
        let mut lines = [Line::new(); MAX_LINES];
        let (panel, _) = load(&sys, b"stock", &mut lines).unwrap().unwrap();
        let actions: Vec<(Trigger, String, usize)> = panel.actions[..panel.acts].iter().map(|action| (action.trigger, String::from_utf8_lossy(action.label.bytes()).into_owned(), action.line)).collect();
        assert_eq!(actions, [(Trigger::Option(b'8'), "Moves".to_string(), 0), (Trigger::Option(b'9'), "Zero".to_string(), 1), (Trigger::Key(8), "Count".to_string(), 2), (Trigger::Key(10), "Report".to_string(), 3)]);
        // what each does is read from its line
        assert_eq!(does(after_label(lines[0].bytes())), Some(Does::Open { panel: b"moves", column: b"item" }));
        assert_eq!(does(after_label(lines[2].bytes())), Some(Does::Run(b"recount(id, qty + 1)")));
        assert_eq!(does(after_label(lines[3].bytes())), Some(Does::Run(b"report")));
        // what is wrong with one is told when the panel is kept
        let not_kept = "Line 1 is not right: the panel is not kept.";
        let faults = [
            ("option 2 'Mine' run x", "The digit of an option is 1, 3, 6, 7, 8 or 9: 2, 4 and 5 are the system's."),
            ("option 10 'Mine' run x", "The digit of an option is 1, 3, 6, 7, 8 or 9: 2, 4 and 5 are the system's."),
            ("option x 'Mine' run x", "The digit of an option is 1, 3, 6, 7, 8 or 9: 2, 4 and 5 are the system's."),
            ("key f3 'Mine' run x", "A key of a panel is f1, f2 or f7 to f11: the others are the system's."),
            ("key f12 'Mine' run x", "A key of a panel is f1, f2 or f7 to f11: the others are the system's."),
            ("key 8 'Mine' run x", "A key of a panel is f1, f2 or f7 to f11: the others are the system's."),
            ("option 8 Moves run x", "The label of an option or of a key is a text of 12 characters at most."),
            ("option 8 'A label too long' run x", "The label of an option or of a key is a text of 12 characters at most."),
            ("option 8 'Moves'", "What it does: open moves where article = id, or run count(id)"),
            ("option 8 'Moves' open moves", "What it does: open moves where article = id, or run count(id)"),
            ("option 8 'Moves' open moves where item = 3", "What it does: open moves where article = id, or run count(id)"),
            ("option 8 'Moves' open moves where item > id", "What it does: open moves where article = id, or run count(id)"),
            ("option 8 'Moves' run", "What it does: open moves where article = id, or run count(id)"),
            ("option 8 'Moves' run count(1, )", "What it does: open moves where article = id, or run count(id)"),
            ("option 8 'Moves' drop table items", "What it does: open moves where article = id, or run count(id)"),
        ];
        for (line, message) in faults {
            assert_eq!(said(&mut sys, &["panel wrong on items", line, "end"]), [message, not_kept], "{line}");
        }
        // a command or a word of the language is no program a panel runs
        for line in ["key f8 'Boom' run reboot", "option 8 'Say' run say", "option 8 'Tables' run tables"] {
            assert_eq!(said(&mut sys, &["panel wrong on items", line, "end"])[0], "What it does: open moves where article = id, or run count(id)", "{line}");
        }
        // what would not be shown on its row is not kept: a key nobody sees is not one to press
        let wide: Vec<String> = [1, 3, 6, 7].iter().map(|digit| format!("option {digit} 'Twelve chars' run x")).collect();
        let mut typed = vec!["panel wrong on items"];
        typed.extend(wide.iter().map(|line| line.as_str()));
        typed.push("end");
        assert_eq!(said(&mut sys, &typed), ["Its options and keys do not fit on their row: shorten labels, or have fewer.", "Line 4 is not right: the panel is not kept."]);
        assert_eq!(said(&mut sys, &["panel wrong on items", "option 8 'A' run x", "option 8 'B' run y", "end"])[0], "This digit has an option already.");
        assert_eq!(said(&mut sys, &["panel wrong on items", "key f8 'A' run x", "key F8 'B' run y", "end"])[0], "This key is taken already.");
        let mut many = vec!["panel wrong on items".to_string()];
        many.extend([1, 3, 6, 7, 8, 9].iter().map(|digit| format!("option {digit} 'O' run x")));
        many.extend([1, 2, 7].iter().map(|key| format!("key f{key} 'K' run x")));
        many.push("end".to_string());
        assert_eq!(said(&mut sys, &many.iter().map(|line| line.as_str()).collect::<Vec<_>>())[0], "A panel has eight options and keys of its own at most.");
    }

    #[test]
    fn a_panel_whose_table_changed_says_so_when_it_is_read() {
        let mut sys = stocked();
        said(&mut sys, &["panel stock on items", "list label, place", "field place 'Where'", "end"]);
        let mut lines = [Line::new(); MAX_LINES];
        said(&mut sys, &["alter table items drop column place"]);
        let gone = load(&sys, b"stock", &mut lines).err().unwrap();
        assert_eq!(String::from_utf8_lossy(gone.bytes()), "Unknown column: place. The table items has no such column. (line 1 of the panel)");
        said(&mut sys, &["drop table items"]);
        let gone = load(&sys, b"stock", &mut lines).err().unwrap();
        assert_eq!(String::from_utf8_lossy(gone.bytes()), "Unknown table: items. Type tables to list them.");
        // it is still shown and dropped
        assert_eq!(said(&mut sys, &["show panel stock"])[0], "panel stock on items");
        assert_eq!(said(&mut sys, &["drop panel stock"]), ["Panel stock removed."]);
    }
}
