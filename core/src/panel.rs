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
const FOOT: usize = 2;

/// A label, of a field or of a panel.
pub type Label = FixedStr<32>;

/// A column of the form: what it is called there, and whether it is typed.
#[derive(Clone, Copy)]
pub struct Field {
    pub column: Name,
    pub label: Label,
    /// Shown and not typed.
    pub shown: bool,
    /// The line it was written on, from 0: what it may hold (`among`) is
    /// read from there when it is needed.
    pub line: usize,
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
        let no_field = Field { column: Name::new(), label: Label::new(), shown: false, line: 0 };
        let mut lower = Name::from_bytes(name);
        lower.lower();
        let mut panel = Panel { name: lower, table: schema.name, title: Label::from_bytes(lower.bytes()), list: [Name::new(); sql::MAX_COLUMNS], listed: 0, fields: [no_field; COLUMNS], count: 0 };
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
                    // a table has seven columns but its id, and each has one field at most
                    panel.fields[panel.count] = Field { column: found, label, shown, line: at };
                    panel.count += 1;
                }
                _ => return Err(fault(b"A line of a panel starts with title, list or field.")),
            }
        }
        if panel.listed == 0 {
            // every column but the id, which says nothing to who reads
            for column in &schema.columns[1..schema.count.max(1)] {
                panel.list[panel.listed] = *column;
                panel.listed += 1;
            }
        }
        Ok(panel)
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
}

impl Page {
    pub const fn new() -> Page {
        Page { cursor: sql::Cursor::new(), cells: [[Text::new(); sql::MAX_COLUMNS]; ROWS + 1], ids: [0; ROWS + 1] }
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
    sys.panels.open[place] = Some(Open {
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
    });
    sys.screen = SCREENS.len() + place;
    true
}

/// Leaves the panel of a place: its screen is no more, and the overview
/// is shown if it was that one.
pub fn close(sys: &mut System, place: usize) {
    sys.panels.open[place] = None;
    if sys.screen == SCREENS.len() + place {
        sys.screen = 0;
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
    let (panel, found, after) = (open.panel, open.found, open.after);
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
    let space = COLS - 2 - 2 * listed.saturating_sub(1);
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
    if said && nowhere {
        open.message = FixedStr::from_bytes(b"No listed column is a text: there is nothing to look in.");
    } else if said && open.count == 0 && !open.found.is_empty() {
        open.message = FixedStr::from_bytes(b"No row holds: ").also(open.found.bytes());
    }
}

/// Handles a key on the screen of a panel.
pub fn key(sys: &mut System, place: usize, event: KeyEvent) {
    let Some(open) = sys.panels.open[place].as_mut() else {
        return;
    };
    // what could not be read stays said until it is read again
    if !open.failed {
        open.message.clear();
    }
    let say = |open: &mut Open, text: &[u8]| open.message = FixedStr::from_bytes(text);
    match event.key {
        Key::F(3) => return close(sys, place),
        // the overview, the panel staying open
        Key::Esc => {
            sys.screen = 0;
            return;
        }
        // the page that was asked for is not read yet: what these keys
        // do depends on it, and they wait for it
        Key::F(5) | Key::PageDown | Key::PageUp | Key::Enter if open.stale => return,
        Key::F(5) => {}
        Key::PageDown if !open.more => return say(open, b"This is the last page."),
        Key::PageDown => {
            if open.back == BACK {
                // the oldest way back is forgotten
                open.starts.copy_within(1.., 0);
                open.back -= 1;
            }
            open.starts[open.back] = open.after;
            (open.back, open.page) = (open.back + 1, open.page + 1);
            open.after = open.ids[open.count.saturating_sub(1)];
        }
        Key::PageUp if open.back == 0 && open.page > 0 => {
            // further back than is remembered: from the start again
            (open.after, open.page) = (0, 0);
        }
        Key::PageUp if open.back == 0 => return say(open, b"This is the first page."),
        Key::PageUp => {
            (open.back, open.page) = (open.back - 1, open.page - 1);
            open.after = open.starts[open.back];
        }
        // (a letter beyond ASCII is one as any other: tables hold them)
        Key::Char(typed) if typed >= 0x20 && typed != 0x7f => {
            open.find.push(typed);
            return;
        }
        Key::Backspace => {
            let shorter = open.find.len().saturating_sub(1);
            open.find = FixedStr::from_bytes(&open.find.bytes()[..shorter]);
            return;
        }
        Key::Enter => (open.found, open.after, open.back, open.page, open.said) = (open.find, 0, 0, 0, true),
        _ => return,
    }
    // read at the next tick of the interface, not from under this key
    open.stale = true;
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
    let Some(place) = place_of(sys, sys.screen) else {
        return false;
    };
    let now = sys.ticks;
    let stored = sys.stored_at;
    let Some(open) = sys.panels.open[place].as_mut() else {
        return false;
    };
    if !open.stale && (stored < open.read || now < open.looked + AGAIN) {
        return false;
    }
    open.looked = now;
    read(sys, place, room);
    true
}

/// Draws the panel of a place on the rows of the grid from `top` down.
pub fn draw(sys: &System, place: usize, grid: &mut Grid, top: usize) {
    let Some(open) = sys.panels.open[place].as_ref() else {
        return;
    };
    let bg = role::TERM_BASE;
    let blank = Cell::new(b' ', role::TERM_OUT, bg);
    for row in top..grid.rows {
        grid.fill(row, 0, COLS, blank);
    }
    grid.text(1, top + 1, open.panel.title.bytes(), role::TERM_PROMPT, bg);
    let mut page = FixedStr::<16>::new();
    let _ = write!(page, "page {}", open.page + 1);
    grid.text(COLS - 1 - page.len(), top + 1, page.bytes(), role::TERM_OUT, bg);
    let after = grid.text(1, top + 2, b"Find: ", role::TERM_OUT, bg);
    let end = grid.text(after, top + 2, open.find.bytes(), role::TERM_CMD, bg);
    grid.put(end, top + 2, Cell { ch: b' ', fg: role::TERM_CMD, bg, fx: fx::PULSE });
    grid.text(0, top + 4, open.header.bytes(), role::TERM_PROMPT, bg);
    let last = grid.rows.saturating_sub(1);
    for (at, line) in open.lines[..open.count].iter().enumerate() {
        if top + HEAD + at + FOOT > last {
            break;
        }
        grid.text(0, top + HEAD + at, line.bytes(), role::TERM_OUT, bg);
    }
    if open.count == 0 && !open.failed && open.message.is_empty() && !open.stale {
        grid.text(1, top + HEAD, b"(no rows)", role::TERM_OUT, bg);
    }
    grid.text(1, last.saturating_sub(1), open.message.bytes(), if open.failed { role::ERROR } else { role::TERM_CMD }, bg);
    grid.text(1, last, b"F3=Exit  F5=Refresh  PgUp/PgDn=Pages  Enter=Find  Esc=Overview", role::TERM_PROMPT, bg);
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
        assert_eq!((panel.title.bytes(), panel.listed, panel.count, panel.label(&panel.list[3]).bytes()), (&b"stock"[..], 4, 0, &b"qty"[..]));
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
            (&["say 1"], "A line of a panel starts with title, list or field."),
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
