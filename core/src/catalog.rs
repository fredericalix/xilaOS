//! The tables of the system.
//!
//! Some are views of the live state: reading `tasks` shows the tasks as they
//! are at that moment. They are what makes "everything is a table" true: the
//! commands `tasks`, `mem` or `city` and a SELECT on the table of the same
//! name read the same thing. The others are the stored tables of the engine
//! (`System::db`), made with CREATE TABLE. A query sees both the same way.

use core::fmt::Write;

use crate::city;
use crate::shell::{System, Text, NAMES, SCREENS};
use crate::change::engine_error;
use crate::sql::{row_from, Kind, Message, Row, Schema, Source, TableDef, Value, MAX_COLUMNS};
use crate::theme::THEMES;

/// The tables that are views of the live state of the system.
pub const TABLES: [TableDef; 9] = [
    TableDef { name: b"tables", columns: &[b"name", b"kind", b"columns", b"rows", b"about"], kinds: b"ttiit", about: b"all tables: views of the system, stored tables" },
    TableDef { name: b"columns", columns: &[b"table_name", b"position", b"name", b"type"], kinds: b"titt", about: b"the columns of every table" },
    TableDef { name: b"tasks", columns: &[b"id", b"name", b"state", b"ran_ms", b"share"], kinds: b"ittii", about: b"what runs, and its share of the processor in %" },
    TableDef { name: b"settings", columns: &[b"name", b"value"], kinds: b"tt", about: b"mood, theme, keyboard, screen" },
    TableDef { name: b"screens", columns: &[b"id", b"name", b"shown"], kinds: b"itt", about: b"the virtual screens" },
    TableDef { name: b"memory", columns: &[b"kind", b"kb"], kinds: b"ti", about: b"memory: the machine, the kernel, the database" },
    TableDef { name: b"city", columns: &[b"plot", b"block", b"height", b"lit"], kinds: b"ttit", about: b"what the city shows" },
    TableDef { name: b"commands", columns: &[b"name"], kinds: b"t", about: b"the commands of the prompt" },
    TableDef { name: b"machine", columns: &[b"item", b"value"], kinds: b"tt", about: b"processor, firmware, screen, disk" },
];

// A cell of a result holds a `Text`: what says what a view is must fit.
const _: () = {
    let mut index = 0;
    while index < TABLES.len() {
        assert!(TABLES[index].about.len() <= crate::change::MAX_TEXT);
        index += 1;
    }
};

/// Index of a table of the system by name.
pub fn find(name: &[u8]) -> Option<usize> {
    TABLES.iter().position(|table| table.name.eq_ignore_ascii_case(name))
}

/// The name of a type, as CREATE TABLE takes it.
pub fn kind_name(kind: Kind) -> Text {
    let mut name = Text::new();
    match kind {
        Kind::Int => name.push_bytes(b"int"),
        Kind::Text => name.push_bytes(b"text"),
        Kind::Dec(scale) => {
            let _ = write!(name, "decimal(18,{scale})");
        }
    }
    name
}

/// Describes a stored table. Stored tables are numbered after the views.
pub fn stored_schema(table: &xila_db::Table, index: usize) -> Schema {
    let mut schema = Schema::new(TABLES.len() + index, &table.name, true);
    for column in &table.columns {
        schema.add(&column.name, column.kind);
    }
    schema
}

/// Where the stored tables are kept, in a few words.
pub fn kept(sys: &System) -> Text {
    let storage = sys.db.storage();
    let mut text = Text::new();
    if !storage.keeps {
        text.push_bytes(b"in memory, lost when the machine stops");
    } else if storage.stopped {
        text.push_bytes(b"on disk, stopped after a failed write");
    } else {
        let _ = write!(text, "on disk, transaction {}", storage.transaction);
    }
    text
}

/// The ids of the engine among these whole numbers; None when there are none.
pub(crate) fn ids(first: i64, last: i64) -> Option<xila_db::Ids> {
    (first <= last && last >= 0).then(|| xila_db::Ids { first: first.max(0) as u64, last: last as u64 })
}

fn row(values: &[Value]) -> Row {
    let mut row = [Value::Null; MAX_COLUMNS];
    row[..values.len()].copy_from_slice(values);
    row
}

fn number(n: u64) -> Value {
    Value::Int(n as i64)
}

impl Source for System {
    fn schema(&self, name: &[u8]) -> Option<Schema> {
        if let Some(index) = find(name) {
            return Some(TABLES[index].schema(index));
        }
        let index = self.db.find(name)?;
        Some(stored_schema(&self.db.tables()[index], index))
    }

    fn scan(&self, table: usize, visit: &mut dyn FnMut(&Row)) -> Result<(), Message> {
        self.scan_among(table, i64::MIN, i64::MAX, visit)
    }

    fn scan_among(&self, table: usize, first: i64, last: i64, visit: &mut dyn FnMut(&Row)) -> Result<(), Message> {
        let text = Value::text;
        let stored = self.db.tables();
        if let Some(index) = table.checked_sub(TABLES.len()) {
            if let Some(table) = stored.get(index) {
                let mut kinds = [Kind::Int; MAX_COLUMNS];
                for (kind, column) in kinds.iter_mut().zip(&table.columns) {
                    *kind = column.kind;
                }
                // the first column of a stored table is its id, which the engine finds rows by
                if let Some(ids) = ids(first, last) {
                    self.db.scan_ids(index, ids, &mut |fields| visit(&row_from(fields, &kinds))).map_err(|error| engine_error(error, None))?;
                }
            }
            return Ok(());
        }
        match table {
            0 => {
                for (i, def) in TABLES.iter().enumerate() {
                    let mut rows = 0;
                    if i == 0 {
                        rows = TABLES.len() + stored.len();
                    } else {
                        self.scan(i, &mut |_| rows += 1)?;
                    }
                    visit(&row(&[text(def.name), text(b"system"), number(def.columns.len() as u64), number(rows as u64), text(def.about)]));
                }
                for table in stored {
                    visit(&row(&[text(&table.name), text(b"stored"), number(table.columns.len() as u64), number(table.rows() as u64), Value::Null]));
                }
            }
            1 => {
                for def in TABLES.iter() {
                    for (position, (name, kind)) in def.columns.iter().zip(def.kinds).enumerate() {
                        let kind: &[u8] = if *kind == b'i' { b"int" } else { b"text" };
                        visit(&row(&[text(def.name), number(position as u64 + 1), text(name), text(kind)]));
                    }
                }
                for table in stored {
                    for (position, column) in table.columns.iter().enumerate() {
                        visit(&row(&[text(&table.name), number(position as u64 + 1), text(&column.name), Value::Text(kind_name(column.kind))]));
                    }
                }
            }
            2 => {
                for (id, task) in self.tasks.iter().enumerate().filter(|(_, task)| task.used) {
                    visit(&row(&[
                        number(id as u64),
                        text(task.name.bytes()),
                        text(task.state),
                        number(task.ticks * 10),
                        number(self.cpu_share[id] as u64 / 10),
                    ]));
                }
            }
            3 => {
                let mut energy = Text::new();
                let _ = write!(energy, "{}", self.energy);
                visit(&row(&[text(b"mood"), text(self.mood.name())]));
                visit(&row(&[text(b"energy"), Value::Text(energy)]));
                visit(&row(&[text(b"theme"), text(THEMES[self.theme].name)]));
                visit(&row(&[text(b"keymap"), text(self.layout.name())]));
                visit(&row(&[text(b"screen"), text(SCREENS[self.screen])]));
            }
            4 => {
                for (id, name) in SCREENS.iter().enumerate() {
                    visit(&row(&[number(id as u64), text(name), text(if id == self.screen { b"yes" } else { b"" })]));
                }
            }
            5 => {
                let m = &self.machine;
                visit(&row(&[text(b"usable"), number(m.mem_usable_kb)]));
                visit(&row(&[text(b"kept by the firmware"), number(m.mem_firmware_kb)]));
                visit(&row(&[text(b"managed by the kernel"), number(m.mem_managed_kb)]));
                visit(&row(&[text(b"free"), number(m.mem_free_kb)]));
                visit(&row(&[text(b"kernel heap in use"), number(m.heap_used / 1024)]));
                visit(&row(&[text(b"stored tables"), number(self.db.used() as u64 / 1024)]));
                visit(&row(&[text(b"room for stored tables"), number(self.db.limit() as u64 / 1024)]));
            }
            6 => {
                for plot in city::scene(self).iter() {
                    for block in plot.blocks.iter().filter(|block| block.used) {
                        visit(&row(&[
                            text(plot.name),
                            text(block.name.bytes()),
                            number(block.height as u64),
                            text(if block.lit { b"yes" } else { b"" }),
                        ]));
                    }
                }
            }
            7 => {
                for name in NAMES {
                    visit(&row(&[text(name)]));
                }
            }
            8 => {
                let m = &self.machine;
                let mut system = Text::from_bytes(b"XilaOS ");
                system.push_bytes(crate::VERSION);
                let mut screen = Text::new();
                let _ = write!(screen, "{}x{}", m.screen_width, m.screen_height);
                visit(&row(&[text(b"system"), Value::Text(system)]));
                visit(&row(&[text(b"processor"), text(m.cpu.bytes())]));
                visit(&row(&[text(b"firmware"), text(m.firmware.bytes())]));
                visit(&row(&[text(b"screen"), Value::Text(screen)]));
                visit(&row(&[text(b"screen memory"), text(if self.display.combining { b"write combining" } else { b"written piece by piece" })]));
                let mut pictures = Text::new();
                let _ = write!(pictures, "{}", self.display.pictures);
                visit(&row(&[text(b"pictures per second"), Value::Text(pictures)]));
                visit(&row(&[text(b"serial port"), text(if m.serial { b"yes" } else { b"no" })]));
                visit(&row(&[text(b"graphics chip"), text(m.graphics.bytes())]));
                let clock = self.now().map_or(Text::from_bytes(b"tells no time"), |now| crate::clock::Moment::at(now).text());
                visit(&row(&[text(b"clock"), Value::Text(clock)]));
                visit(&row(&[text(b"disk"), text(m.disk.bytes())]));
                visit(&row(&[text(b"stored tables"), Value::Text(kept(self))]));
            }
            _ => {}
        }
        Ok(())
    }
}
