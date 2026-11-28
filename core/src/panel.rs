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
use crate::grid::COLS;
use crate::lang::{self, word_of, Line, MAX_LINES};
use crate::shell::{self, Out, Style, System, Table, Text};
use crate::sql::{self, Message, Name, Schema, Source};
use crate::text::{split_word, trim, FixedStr};

/// The table panels are kept in.
pub const PANELS: &[u8] = b"panels";
/// Columns a list shows, and fields a form has: what a table has but `id`.
pub const COLUMNS: usize = sql::MAX_COLUMNS - 1;

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
                        let column = Self::column(schema, column).ok_or_else(|| unknown(column))?;
                        if panel.listed == panel.list.len() {
                            return Err(fault(b"A list shows eight columns at most."));
                        }
                        panel.list[panel.listed] = column;
                        panel.listed += 1;
                    }
                }
                b"field" => {
                    let wrong = || fault(b"A field is a column and its label: field place 'Where it is'");
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
            true
        }
        Err(error) => {
            sys.db.rollback();
            out.line(Style::Error, engine_error(error, None).bytes());
            false
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
            (&["list label,"], "Unknown column: . The table items has no such column."),
            (&["field"], "A field is a column and its label: field place 'Where it is'"),
            (&["field place"], "A field is a column and its label: field place 'Where it is'"),
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
