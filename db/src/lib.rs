//! The storage engine of XilaOS.
//!
//! It keeps tables of typed records and changes them in transactions. It
//! knows nothing of SQL, of the screen or of the machine: the query language
//! lives in `xila-core` and calls this.
//!
//! - A table has up to `MAX_COLUMNS` columns. Column 0 is always `id`: a
//!   number the engine gives to each row, never used twice in a table. It is
//!   the key of the row.
//! - A row is stored as a record: the bytes of its fields one after the
//!   other (see `encode`).
//! - Every change is part of a transaction that ends with `commit`, which
//!   makes it final and reports it (this is where LISTEN / NOTIFY gets its
//!   events), or with `rollback`, which undoes it.
//!
//! The rows of a table are in a B-tree, by id (`tree`). The trees are made
//! of pages that are never overwritten in place (`store`), on a volume
//! (`disk`): a disk of the machine, or memory when there is none. A commit
//! is one write, so a power cut leaves the database as it was before the
//! transaction or as it is after, never in between.
//!
//! One more tree, the catalog, describes the tables: for each its name, its
//! columns, the root of its tree, how many rows it has and the next id.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod disk;
mod store;
#[cfg(test)]
mod testing;
mod tree;

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::cell::RefCell;

pub use disk::{Disk, DiskError, Memory, Page, Volume, PAGE};
use store::{Found, Store};

/// Columns of a table, `id` included.
pub const MAX_COLUMNS: usize = 8;
pub const MAX_TABLES: usize = 64;
/// Length of the name of a table or of a column.
pub const MAX_NAME: usize = 24;

/// The type of a column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Int,
    /// An exact decimal number with this many digits after the point. It is
    /// stored as a whole number of the smallest unit: 12.50 with two digits
    /// is 1250.
    Dec(u8),
    Text,
}

/// The value of one column of one row. `Dec` is in the smallest unit of its
/// column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field<'a> {
    Null,
    Int(i64),
    Dec(i64),
    Text(&'a [u8]),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The database has reached its size limit.
    Full,
    /// A table of that name exists.
    Exists,
    TooManyTables,
    /// No column besides `id`, or more than `MAX_COLUMNS`.
    ColumnCount,
    /// A name is empty, too long, or has characters other than letters,
    /// digits and `_`, or does not start with a letter.
    BadName,
    /// Two columns have the same name, or one is called `id`.
    DuplicateColumn,
    /// The value given for this column is not of its type.
    WrongKind(usize),
    /// `id` is given by the engine and cannot be set.
    Key,
    /// A row is longer than a record can hold.
    TooLong,
    /// Changes are waiting for `commit` or `rollback`.
    Busy,
    /// The disk could not read or write.
    Disk,
    /// A page of the database is not what it should be: its number.
    Damaged(u64),
    /// A commit failed half way. What the disk holds is still one whole
    /// state, but nothing more is written until the machine restarts.
    Stopped,
    /// The disk holds something that is not a XilaOS database.
    Foreign,
    /// The disk is too small for a database.
    TooSmall,
    /// This column must have a value, and the row has none.
    Missing(usize),
    /// This column has no value twice, and the row with this id has it.
    Twice(usize, u64),
    /// More conditions than a table keeps, or one that is too long.
    Checks,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    Insert,
    Update,
    Delete,
}

/// A value that is kept: what a column says a row takes when none is given.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Kept {
    Null,
    Int(i64),
    Dec(i64),
    Text(Vec<u8>),
}

impl Kept {
    fn of(field: &Field) -> Kept {
        match *field {
            Field::Null => Kept::Null,
            Field::Int(n) => Kept::Int(n),
            Field::Dec(n) => Kept::Dec(n),
            Field::Text(text) => Kept::Text(text.to_vec()),
        }
    }

    pub fn field(&self) -> Field<'_> {
        match self {
            Kept::Null => Field::Null,
            Kept::Int(n) => Field::Int(*n),
            Kept::Dec(n) => Field::Dec(*n),
            Kept::Text(text) => Field::Text(text),
        }
    }
}

/// What a column asks of its values, besides their type, as it is given
/// when a table is made.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rule<'a> {
    /// Every row has a value here. The engine refuses a row that has none.
    pub not_null: bool,
    /// No two rows have the same value here; rows without a value are not
    /// compared. The engine refuses the second one. With no index it
    /// reads the table to know.
    pub unique: bool,
    /// The value a row takes when none is given. The engine keeps it and
    /// does not use it: whoever makes the row does.
    pub default: Option<Field<'a>>,
}

#[derive(Clone)]
pub struct Column {
    pub name: Vec<u8>,
    pub kind: Kind,
    pub not_null: bool,
    pub unique: bool,
    pub default: Option<Kept>,
}

impl Column {
    fn plain(name: Vec<u8>, kind: Kind) -> Column {
        Column { name, kind, not_null: false, unique: false, default: None }
    }

    fn ruled(&self) -> bool {
        self.not_null || self.unique || self.default.is_some()
    }
}

const RULE_NOT_NULL: u8 = 1;
const RULE_UNIQUE: u8 = 2;
const RULE_DEFAULT: u8 = 4;
/// Conditions a table keeps, at most, and the longest one.
pub const MAX_CHECKS: usize = 4;
pub const MAX_CHECK: usize = 60;

#[derive(Clone)]
pub struct Table {
    pub name: Vec<u8>,
    /// Column 0 is `id`.
    pub columns: Vec<Column>,
    /// Conditions every row holds, as whoever made the table wrote them.
    /// The engine keeps them and does not read them.
    pub checks: Vec<Vec<u8>>,
    /// Key of the table in the catalog. Never used twice.
    key: u64,
    /// Root of the tree of the rows; 0 when there are none.
    root: u64,
    rows: u64,
    next_id: u64,
    /// The catalog on the volume does not say this yet.
    changed: bool,
    /// For whoever watches the table: when it was last read and last
    /// changed, on a clock of the caller's choice. The engine only keeps them.
    pub read_at: u64,
    pub written_at: u64,
}

const KIND_INT: u8 = 1;
const KIND_DEC: u8 = 2;
const KIND_TEXT: u8 = 3;

/// Takes bytes from the front of a record.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn bytes(&mut self, count: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(count)?;
        self.0 = rest;
        Some(taken)
    }

    fn byte(&mut self) -> Option<u8> {
        self.bytes(1).map(|bytes| bytes[0])
    }

    fn number(&mut self) -> Option<u64> {
        self.bytes(8).map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8])))
    }

    fn name(&mut self) -> Option<Vec<u8>> {
        let length = self.byte()? as usize;
        let name = self.bytes(length)?;
        good_name(name).then(|| name.to_vec())
    }

    /// A value, written as a field of a record is.
    fn kept(&mut self) -> Option<Kept> {
        let number = |reader: &mut Reader| reader.number().map(|n| n as i64);
        Some(match self.byte()? {
            TAG_NULL => Kept::Null,
            TAG_INT => Kept::Int(number(self)?),
            TAG_DEC => Kept::Dec(number(self)?),
            TAG_TEXT => {
                let length = self.bytes(2)?;
                Kept::Text(self.bytes(u16::from_le_bytes([length[0], length[1]]) as usize)?.to_vec())
            }
            _ => return None,
        })
    }
}

impl Table {
    pub fn rows(&self) -> usize {
        self.rows as usize
    }

    /// Index of a column by name, whatever the case of the letters.
    pub fn column(&self, name: &[u8]) -> Option<usize> {
        self.columns.iter().position(|column| column.name.eq_ignore_ascii_case(name))
    }

    /// The table as the catalog keeps it: name, root, next id, rows, then
    /// the columns after `id` with their type. When a column has a rule or
    /// the table a condition, more follows: for each column after `id` a
    /// byte of rules and, if it says so, the value by default; then the
    /// number of conditions, and each after its length. A table that has
    /// none of this is described as version 1 of the format did.
    fn describe(&self) -> Vec<u8> {
        let mut record = Vec::new();
        record.push(self.name.len() as u8);
        record.extend_from_slice(&self.name);
        for number in [self.root, self.next_id, self.rows] {
            record.extend_from_slice(&number.to_le_bytes());
        }
        record.push(self.columns.len() as u8 - 1);
        for column in &self.columns[1..] {
            let (kind, scale) = match column.kind {
                Kind::Int => (KIND_INT, 0),
                Kind::Dec(scale) => (KIND_DEC, scale),
                Kind::Text => (KIND_TEXT, 0),
            };
            record.extend_from_slice(&[kind, scale, column.name.len() as u8]);
            record.extend_from_slice(&column.name);
        }
        if self.checks.is_empty() && !self.columns.iter().any(Column::ruled) {
            return record;
        }
        for column in &self.columns[1..] {
            let rules = [(column.not_null, RULE_NOT_NULL), (column.unique, RULE_UNIQUE), (column.default.is_some(), RULE_DEFAULT)];
            record.push(rules.iter().filter(|(set, _)| *set).map(|(_, bit)| bit).sum());
            if let Some(default) = &column.default {
                // a value that is too long was refused when the table was made
                record.extend_from_slice(&encode(&[default.field()]).unwrap_or_else(|_| [TAG_NULL].to_vec()));
            }
        }
        record.push(self.checks.len() as u8);
        for check in &self.checks {
            record.push(check.len() as u8);
            record.extend_from_slice(check);
        }
        record
    }

    fn described(key: u64, record: &[u8]) -> Option<Table> {
        let mut reader = Reader(record);
        let name = reader.name()?;
        let (root, next_id, rows) = (reader.number()?, reader.number()?, reader.number()?);
        let count = reader.byte()? as usize;
        if count == 0 || count >= MAX_COLUMNS {
            return None;
        }
        let mut columns = Vec::new();
        columns.push(Column::plain(b"id".to_vec(), Kind::Int));
        for _ in 0..count {
            let kind = match (reader.byte()?, reader.byte()?) {
                (KIND_INT, _) => Kind::Int,
                (KIND_DEC, scale) => Kind::Dec(scale),
                (KIND_TEXT, _) => Kind::Text,
                _ => return None,
            };
            columns.push(Column::plain(reader.name()?, kind));
        }
        let mut checks = Vec::new();
        // what version 1 did not have: there or not, never in part
        if !reader.0.is_empty() {
            for column in &mut columns[1..] {
                let rules = reader.byte()?;
                if rules & !(RULE_NOT_NULL | RULE_UNIQUE | RULE_DEFAULT) != 0 {
                    return None;
                }
                (column.not_null, column.unique) = (rules & RULE_NOT_NULL != 0, rules & RULE_UNIQUE != 0);
                if rules & RULE_DEFAULT != 0 {
                    column.default = Some(reader.kept().filter(|default| fits(column.kind, &default.field()))?);
                }
            }
            let count = reader.byte()? as usize;
            if count > MAX_CHECKS {
                return None;
            }
            for _ in 0..count {
                let length = reader.byte()? as usize;
                checks.push(reader.bytes(length).filter(|_| length <= MAX_CHECK)?.to_vec());
            }
            if !reader.0.is_empty() {
                return None;
            }
        }
        Some(Table { name, columns, checks, key, root, rows, next_id, changed: false, read_at: 0, written_at: 0 })
    }
}

struct Logged {
    table: usize,
    id: u64,
    change: Change,
}

/// What a database is kept on, and how full it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Storage {
    /// `memory`, or the name of the disk.
    pub disk: &'static str,
    /// Whether the tables are still there after the machine stops.
    pub keeps: bool,
    pub pages: u64,
    pub used: u64,
    /// Number of the last transaction that was committed.
    pub transaction: u64,
    /// See `Error::Stopped`.
    pub stopped: bool,
    /// Pages that reading and changing the tables went through since the
    /// database was opened: what a query costs.
    pub looked: u64,
}

/// The rows of a table whose ids go from `first` to `last`, both
/// included. Reading them goes down the tree to them: it costs their
/// pages, not those of the table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ids {
    pub first: u64,
    pub last: u64,
}

impl Ids {
    pub const ALL: Ids = Ids { first: 0, last: u64::MAX };

    pub fn one(id: u64) -> Ids {
        Ids { first: id, last: id }
    }
}

pub struct Database {
    tables: Vec<Table>,
    /// Made when first needed, in memory, unless `open` gave a volume.
    store: RefCell<Option<Store>>,
    /// Bytes a database in memory may take.
    limit: usize,
    next_table: u64,
    /// The tables as they were when the open transaction started.
    saved: Option<Vec<Table>>,
    /// Catalog keys of the tables the open transaction removed.
    removed: Vec<u64>,
    log: Vec<Logged>,
    fresh: bool,
}

const TAG_NULL: u8 = 0;
const TAG_INT: u8 = 1;
const TAG_DEC: u8 = 2;
const TAG_TEXT: u8 = 3;

/// Writes fields as a record: one tag byte per field, then 8 bytes for a
/// number (little endian) or a 2 byte length and the bytes for a text.
fn encode(fields: &[Field]) -> Result<Vec<u8>, Error> {
    let mut record = Vec::new();
    for field in fields {
        match *field {
            Field::Null => record.push(TAG_NULL),
            Field::Int(n) => {
                record.push(TAG_INT);
                record.extend_from_slice(&n.to_le_bytes());
            }
            Field::Dec(n) => {
                record.push(TAG_DEC);
                record.extend_from_slice(&n.to_le_bytes());
            }
            Field::Text(text) => {
                let length = u16::try_from(text.len()).map_err(|_| Error::TooLong)?;
                record.push(TAG_TEXT);
                record.extend_from_slice(&length.to_le_bytes());
                record.extend_from_slice(text);
            }
        }
    }
    if record.len() > tree::MAX_VALUE {
        return Err(Error::TooLong);
    }
    Ok(record)
}

/// Reads a record into `fields`. Fields the record does not have stay null.
fn decode<'a>(record: &'a [u8], fields: &mut [Field<'a>]) {
    let mut at = 0;
    for field in fields.iter_mut() {
        let Some(&tag) = record.get(at) else {
            return;
        };
        at += 1;
        let number = |at: usize| record.get(at..at + 8).map(|b| i64::from_le_bytes(b.try_into().unwrap_or([0; 8])));
        match tag {
            TAG_INT | TAG_DEC => {
                let Some(n) = number(at) else {
                    return;
                };
                *field = if tag == TAG_INT { Field::Int(n) } else { Field::Dec(n) };
                at += 8;
            }
            TAG_TEXT => {
                let Some(length) = record.get(at..at + 2) else {
                    return;
                };
                let length = u16::from_le_bytes([length[0], length[1]]) as usize;
                let Some(text) = record.get(at + 2..at + 2 + length) else {
                    return;
                };
                *field = Field::Text(text);
                at += 2 + length;
            }
            _ => {}
        }
    }
}

fn fits(kind: Kind, field: &Field) -> bool {
    matches!((kind, field), (_, Field::Null) | (Kind::Int, Field::Int(_)) | (Kind::Dec(_), Field::Dec(_)) | (Kind::Text, Field::Text(_)))
}

/// Whether a table or a column can have this name.
pub fn good_name(name: &[u8]) -> bool {
    name.len() <= MAX_NAME
        && name.first().is_some_and(|c| c.is_ascii_alphabetic())
        && name.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// The store of a database, made in memory if there is none yet.
fn ready(slot: &mut Option<Store>, limit: usize) -> Result<&mut Store, Error> {
    if slot.is_none() {
        let pages = (limit / PAGE) as u64;
        let volume = Volume::single(Box::new(Memory::new(pages)));
        *slot = Some(Store::format(volume, pages).map_err(|_| Error::Full)?);
    }
    slot.as_mut().ok_or(Error::Full)
}

impl Database {
    /// A database in memory: its tables are lost when the machine stops.
    pub const fn new() -> Database {
        Database {
            tables: Vec::new(),
            store: RefCell::new(None),
            limit: 8 << 20,
            next_table: 1,
            saved: None,
            removed: Vec::new(),
            log: Vec::new(),
            fresh: true,
        }
    }

    /// The database a volume holds. A volume nothing was ever written on
    /// gets an empty database; one that holds something else is refused.
    ///
    /// This reads the catalog and the branches of every tree, to find which
    /// pages are in use. The rows themselves are not read.
    ///
    /// A volume that is refused is given back, with the reason: nothing
    /// was written on it.
    pub fn open(volume: Volume) -> Result<Database, (Error, Volume)> {
        if volume.pages() < store::MIN_PAGES {
            return Err((Error::TooSmall, volume));
        }
        let mut store = match Store::open(volume)? {
            Found::Opened(store) => store,
            Found::Blank(volume) => return Database::format(volume),
        };
        match Database::tables_of(&mut store) {
            Ok(tables) => Ok(Database::on(store, tables, false)),
            Err(error) => Err((error, store.into_volume())),
        }
    }

    /// Starts an empty database on a volume, whatever it holds. What was
    /// there is lost.
    pub fn format(volume: Volume) -> Result<Database, (Error, Volume)> {
        let pages = volume.pages();
        Ok(Database::on(Store::format(volume, pages)?, Vec::new(), true))
    }

    fn on(store: Store, tables: Vec<Table>, fresh: bool) -> Database {
        let next_table = store.next_table();
        Database { tables, store: RefCell::new(Some(store)), limit: 0, next_table, saved: None, removed: Vec::new(), log: Vec::new(), fresh }
    }

    /// Reads the catalog of a store that was just opened, and tells the
    /// store which pages the trees use.
    fn tables_of(store: &mut Store) -> Result<Vec<Table>, Error> {
        let catalog = store.catalog();
        tree::pages(store, catalog, &mut |store, page| store.mark(page))?;
        let mut tables = Vec::new();
        let mut bad = false;
        tree::walk(store, catalog, &mut |key, record| match Table::described(key, record) {
            Some(table) => tables.push(table),
            None => bad = true,
        })?;
        if bad || tables.len() > MAX_TABLES {
            return Err(Error::Damaged(catalog));
        }
        for table in &tables {
            tree::pages(store, table.root, &mut |store, page| store.mark(page))?;
        }
        Ok(tables)
    }

    /// Whether the database was empty when it was made or opened: nothing
    /// had ever been written on its volume.
    pub fn fresh(&self) -> bool {
        self.fresh
    }

    /// Bytes a database in memory may take. It counts when the first table
    /// is made, not after.
    pub fn set_limit(&mut self, bytes: usize) {
        self.limit = bytes;
    }

    pub fn storage(&self) -> Storage {
        let none = Storage { disk: "memory", keeps: false, pages: (self.limit / PAGE) as u64, used: 0, transaction: 0, stopped: false, looked: 0 };
        let Ok(slot) = self.store.try_borrow() else {
            return none;
        };
        slot.as_ref().map_or(none, |store| Storage {
            disk: store.volume().name(),
            keeps: store.volume().keeps(),
            pages: store.pages(),
            used: store.used(),
            transaction: store.txn(),
            stopped: store.stopped(),
            looked: store.looked(),
        })
    }

    /// Bytes the database may take in all.
    pub fn limit(&self) -> usize {
        self.storage().pages as usize * PAGE
    }

    /// Bytes of the pages in use.
    pub fn used(&self) -> usize {
        self.storage().used as usize * PAGE
    }

    pub fn tables(&self) -> &[Table] {
        &self.tables
    }

    pub fn table_mut(&mut self, table: usize) -> &mut Table {
        &mut self.tables[table]
    }

    /// Index of a table by name, whatever the case of the letters.
    pub fn find(&self, name: &[u8]) -> Option<usize> {
        self.tables.iter().position(|table| table.name.eq_ignore_ascii_case(name))
    }

    /// Remembers the tables as they are, if no transaction is open yet.
    fn begin(&mut self) {
        if self.saved.is_none() {
            self.saved = Some(self.tables.clone());
        }
    }

    /// Creates a table with these columns after `id`. Names are kept in
    /// lower case.
    pub fn create(&mut self, name: &[u8], columns: &[(&[u8], Kind)]) -> Result<usize, Error> {
        let mut plain = [(&b""[..], Kind::Int, Rule::default()); MAX_COLUMNS];
        for (plain, (column, kind)) in plain.iter_mut().zip(columns) {
            *plain = (column, *kind, Rule::default());
        }
        if columns.len() >= MAX_COLUMNS {
            return Err(Error::ColumnCount);
        }
        self.create_with(name, &plain[..columns.len()], &[])
    }

    /// Makes a table whose columns have rules, and which keeps conditions
    /// (`checks`, at most `MAX_CHECKS` of `MAX_CHECK` bytes: texts for
    /// whoever writes rows, which the engine does not read).
    pub fn create_with(&mut self, name: &[u8], columns: &[(&[u8], Kind, Rule)], checks: &[&[u8]]) -> Result<usize, Error> {
        if !self.log.is_empty() {
            return Err(Error::Busy);
        }
        if !good_name(name) || columns.iter().any(|(column, ..)| !good_name(column)) {
            return Err(Error::BadName);
        }
        if self.find(name).is_some() {
            return Err(Error::Exists);
        }
        if self.tables.len() == MAX_TABLES {
            return Err(Error::TooManyTables);
        }
        if columns.is_empty() || columns.len() >= MAX_COLUMNS {
            return Err(Error::ColumnCount);
        }
        if checks.len() > MAX_CHECKS || checks.iter().any(|check| check.is_empty() || check.len() > MAX_CHECK) {
            return Err(Error::Checks);
        }
        let mut made = Vec::new();
        made.push(Column::plain(b"id".to_vec(), Kind::Int));
        for (at, (column, kind, rule)) in columns.iter().enumerate() {
            if made.iter().any(|c: &Column| c.name.eq_ignore_ascii_case(column)) {
                return Err(Error::DuplicateColumn);
            }
            if let Some(default) = &rule.default {
                if !fits(*kind, default) {
                    return Err(Error::WrongKind(at + 1));
                }
                // it must fit in the catalog, and in a row
                encode(&[*default])?;
            }
            made.push(Column { name: column.to_ascii_lowercase(), kind: *kind, not_null: rule.not_null, unique: rule.unique, default: rule.default.as_ref().map(Kept::of) });
        }
        let table = Table {
            name: name.to_ascii_lowercase(),
            columns: made,
            checks: checks.iter().map(|check| check.to_vec()).collect(),
            key: self.next_table,
            root: 0,
            rows: 0,
            next_id: 1,
            changed: true,
            read_at: 0,
            written_at: 0,
        };
        if table.describe().len() > tree::MAX_VALUE {
            return Err(Error::TooLong);
        }
        self.begin();
        self.next_table += 1;
        self.tables.push(table);
        Ok(self.tables.len() - 1)
    }

    /// Removes a table and its rows. The tables after it move down by one.
    pub fn remove(&mut self, table: usize) -> Result<(), Error> {
        if !self.log.is_empty() {
            return Err(Error::Busy);
        }
        self.begin();
        let store = ready(self.store.get_mut(), self.limit)?;
        tree::release(store, self.tables[table].root)?;
        let gone = self.tables.remove(table);
        self.removed.push(gone.key);
        Ok(())
    }

    /// Adds a row with these values for the columns after `id`; columns left
    /// out are null. Returns the id of the row.
    pub fn insert(&mut self, table: usize, fields: &[Field]) -> Result<u64, Error> {
        let columns = &self.tables[table].columns;
        if fields.len() > columns.len() - 1 {
            return Err(Error::ColumnCount);
        }
        if let Some(wrong) = fields.iter().zip(&columns[1..]).position(|(field, column)| !fits(column.kind, field)) {
            return Err(Error::WrongKind(wrong + 1));
        }
        // what the columns ask: a field that is not given is no value
        for (at, column) in columns.iter().enumerate().skip(1) {
            let field = fields.get(at - 1).copied().unwrap_or(Field::Null);
            if field == Field::Null {
                if column.not_null {
                    return Err(Error::Missing(at));
                }
            } else if column.unique {
                if let Some(id) = self.holder(table, at, &field)? {
                    return Err(Error::Twice(at, id));
                }
            }
        }
        let record = encode(fields)?;
        self.begin();
        let store = ready(self.store.get_mut(), self.limit)?;
        let found = &mut self.tables[table];
        let id = found.next_id;
        found.root = tree::set(store, found.root, id, Some(&record))?.0;
        found.next_id += 1;
        found.rows += 1;
        found.changed = true;
        self.log.push(Logged { table, id, change: Change::Insert });
        Ok(id)
    }

    /// The row that has this value in this column, if one does. It reads
    /// the whole table: no index says it yet.
    fn holder(&self, table: usize, column: usize, value: &Field) -> Result<Option<u64>, Error> {
        let mut found = None;
        self.scan(table, &mut |fields| {
            if let (true, None, Field::Int(id)) = (fields[column] == *value, found, fields[0]) {
                found = Some(id as u64);
            }
        })?;
        Ok(found)
    }

    /// Calls `visit` with every row, in the order of their ids. The first
    /// field is the id.
    pub fn scan(&self, table: usize, visit: &mut dyn FnMut(&[Field])) -> Result<(), Error> {
        self.scan_ids(table, Ids::ALL, visit)
    }

    /// As `scan`, for the rows whose ids are among `ids` only.
    pub fn scan_ids(&self, table: usize, ids: Ids, visit: &mut dyn FnMut(&[Field])) -> Result<(), Error> {
        let table = &self.tables[table];
        let count = table.columns.len();
        let mut slot = self.store.try_borrow_mut().map_err(|_| Error::Busy)?;
        let Some(store) = slot.as_mut() else {
            return Ok(());
        };
        tree::range(store, table.root, ids.first, ids.last, &mut |id, record| {
            let mut fields = [Field::Null; MAX_COLUMNS];
            fields[0] = Field::Int(id as i64);
            decode(record, &mut fields[1..count]);
            visit(&fields[..count]);
        })
    }

    /// As `scan_ids`, for a `visit` that reads the database itself: the
    /// table is read leaf by leaf, and nothing is held while `visit` is
    /// called. It costs the way down to each leaf.
    pub fn scan_apart(&self, table: usize, ids: Ids, visit: &mut dyn FnMut(&[Field])) -> Result<(), Error> {
        let table = &self.tables[table];
        let count = table.columns.len();
        let mut next = ids.first;
        loop {
            let mut found = Vec::new();
            {
                let mut slot = self.store.try_borrow_mut().map_err(|_| Error::Busy)?;
                let Some(store) = slot.as_mut() else {
                    return Ok(());
                };
                tree::leaf(store, table.root, next, ids.last, &mut found)?;
            }
            let Some(&(last, _)) = found.last() else {
                return Ok(());
            };
            for (id, record) in &found {
                let mut fields = [Field::Null; MAX_COLUMNS];
                fields[0] = Field::Int(*id as i64);
                decode(record, &mut fields[1..count]);
                visit(&fields[..count]);
            }
            if last >= ids.last {
                return Ok(());
            }
            next = last + 1;
        }
    }

    /// Gives new values to some columns of the rows `pick` accepts. `set`
    /// holds column indexes and values. Returns how many rows changed.
    pub fn update(&mut self, table: usize, pick: &mut dyn FnMut(&[Field]) -> bool, set: &[(usize, Field)]) -> Result<usize, Error> {
        let columns = &self.tables[table].columns;
        for (column, field) in set {
            if *column == 0 {
                return Err(Error::Key);
            }
            if !columns.get(*column).is_some_and(|c| fits(c.kind, field)) {
                return Err(Error::WrongKind(*column));
            }
        }
        self.update_each(table, &mut |fields, put| {
            if pick(fields) {
                let mut new = [Field::Null; MAX_COLUMNS];
                new[..fields.len()].copy_from_slice(fields);
                for (column, field) in set {
                    new[*column] = *field;
                }
                put(&new[..fields.len()]);
            }
        })
    }

    /// Changes rows one by one. `each` is called with every row, and with a
    /// function it calls to give the row its new fields: all of them, the
    /// id first and unchanged. A row it does not call the function for
    /// stays as it is. Returns how many rows changed.
    pub fn update_each(&mut self, table: usize, each: &mut dyn FnMut(&[Field], &mut dyn FnMut(&[Field]))) -> Result<usize, Error> {
        self.update_each_ids(table, Ids::ALL, each)
    }

    /// As `update_each`, for the rows whose ids are among `ids` only.
    pub fn update_each_ids(&mut self, table: usize, ids: Ids, each: &mut dyn FnMut(&[Field], &mut dyn FnMut(&[Field]))) -> Result<usize, Error> {
        let kinds: Vec<Kind> = self.tables[table].columns.iter().map(|column| column.kind).collect();
        let not_null: Vec<bool> = self.tables[table].columns.iter().map(|column| column.not_null).collect();
        let unique: Vec<usize> = (1..kinds.len()).filter(|column| self.tables[table].columns[*column].unique).collect();
        // what each changed row has in the columns that take no value
        // twice, and whether any of those values is a new one
        let mut values: Vec<(usize, Kept, u64)> = Vec::new();
        let mut moved = false;
        // the new records are worked out first: the tree is not changed
        // while it is read
        let mut records: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut failed = None;
        self.scan_ids(table, ids, &mut |fields| {
            let Field::Int(id) = fields[0] else {
                return;
            };
            if failed.is_some() {
                return;
            }
            let mut put = |new: &[Field]| {
                let wrong = new.iter().zip(&kinds).position(|(field, kind)| !fits(*kind, field));
                let empty = new.iter().zip(&not_null).position(|(field, not_null)| *not_null && *field == Field::Null);
                let record = match (wrong, empty) {
                    _ if new.len() != fields.len() => Err(Error::ColumnCount),
                    _ if new[0] != fields[0] => Err(Error::Key),
                    (Some(column), _) => Err(Error::WrongKind(column)),
                    (None, Some(column)) => Err(Error::Missing(column)),
                    (None, None) => encode(&new[1..]),
                };
                match record {
                    Ok(record) => {
                        for &column in &unique {
                            if new[column] != Field::Null {
                                values.push((column, Kept::of(&new[column]), id as u64));
                                moved |= new[column] != fields[column];
                            }
                        }
                        records.push((id as u64, record));
                    }
                    Err(error) => failed = Some(error),
                }
            };
            each(fields, &mut put);
        })?;
        if let Some(error) = failed {
            return Err(error);
        }
        // No value twice, in the table as it will be once every row has
        // changed: rows may exchange their values, or all move by one.
        if moved {
            values.sort();
            if let Some(pair) = values.windows(2).find(|pair| pair[0].0 == pair[1].0 && pair[0].1 == pair[1].1) {
                return Err(Error::Twice(pair[0].0, pair[0].2));
            }
            let mut twice = None;
            self.scan(table, &mut |fields| {
                let Field::Int(id) = fields[0] else {
                    return;
                };
                // a row that changes is among the values already
                if twice.is_some() || records.binary_search_by_key(&(id as u64), |(id, _)| *id).is_ok() {
                    return;
                }
                for &column in &unique {
                    if fields[column] != Field::Null {
                        let held = Kept::of(&fields[column]);
                        if values.binary_search_by(|(other, value, _)| (*other, value).cmp(&(column, &held))).is_ok() {
                            twice = Some(Error::Twice(column, id as u64));
                        }
                    }
                }
            })?;
            if let Some(error) = twice {
                return Err(error);
            }
        }
        self.begin();
        let store = ready(self.store.get_mut(), self.limit)?;
        let found = &mut self.tables[table];
        for (id, record) in &records {
            found.root = tree::set(store, found.root, *id, Some(record))?.0;
            found.changed = true;
            self.log.push(Logged { table, id: *id, change: Change::Update });
        }
        Ok(records.len())
    }

    /// Removes the rows `pick` accepts. Returns how many.
    pub fn delete(&mut self, table: usize, pick: &mut dyn FnMut(&[Field]) -> bool) -> Result<usize, Error> {
        self.delete_ids(table, Ids::ALL, pick)
    }

    /// As `delete`, for the rows whose ids are among `among` only.
    pub fn delete_ids(&mut self, table: usize, among: Ids, pick: &mut dyn FnMut(&[Field]) -> bool) -> Result<usize, Error> {
        let mut ids = Vec::new();
        self.scan_ids(table, among, &mut |fields| {
            if let (true, Field::Int(id)) = (pick(fields), fields[0]) {
                ids.push(id as u64);
            }
        })?;
        self.begin();
        let store = ready(self.store.get_mut(), self.limit)?;
        let found = &mut self.tables[table];
        // removing writes copies of pages before it frees any
        store.spare = true;
        let mut result = Ok(ids.len());
        for &id in &ids {
            match tree::set(store, found.root, id, None) {
                Ok((root, _)) => {
                    found.root = root;
                    found.rows -= 1;
                    found.changed = true;
                    self.log.push(Logged { table, id, change: Change::Delete });
                }
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        store.spare = false;
        result
    }

    /// Writes the catalog and the pages of the open transaction.
    fn write(&mut self) -> Result<(), Error> {
        let store = ready(self.store.get_mut(), self.limit)?;
        // the catalog is small, and without it nothing could be removed
        store.spare = true;
        let mut catalog = store.catalog();
        for key in &self.removed {
            catalog = tree::set(store, catalog, *key, None)?.0;
        }
        for table in self.tables.iter().filter(|table| table.changed) {
            catalog = tree::set(store, catalog, table.key, Some(&table.describe()))?.0;
        }
        store.commit(catalog, self.next_table)
    }

    /// Makes the changes since the last commit final, and calls `each` with
    /// every one of them: table, kind of change, id of the row. When this
    /// returns without an error the changes are safe on the volume. After an
    /// error they are undone, as with `rollback`.
    pub fn commit(&mut self, each: &mut dyn FnMut(usize, Change, u64)) -> Result<(), Error> {
        if self.saved.is_none() {
            return Ok(());
        }
        if let Err(error) = self.write() {
            self.rollback();
            return Err(error);
        }
        self.saved = None;
        self.removed.clear();
        for table in &mut self.tables {
            table.changed = false;
        }
        for logged in self.log.drain(..) {
            each(logged.table, logged.change, logged.id);
        }
        Ok(())
    }

    /// Undoes the changes since the last commit. Ids that were given are not
    /// given again.
    pub fn rollback(&mut self) {
        if let Some(saved) = self.saved.take() {
            let undone = core::mem::replace(&mut self.tables, saved);
            for table in &mut self.tables {
                if let Some(was) = undone.iter().find(|was| was.key == table.key) {
                    if was.next_id > table.next_id {
                        table.next_id = was.next_id;
                        table.changed = true;
                    }
                    (table.read_at, table.written_at) = (was.read_at, was.written_at);
                }
            }
        }
        self.removed.clear();
        self.log.clear();
        if let Some(store) = self.store.get_mut() {
            store.rollback();
        }
    }

    /// Reads the whole database and checks that it is well made: every
    /// tree in order, every page used once, the row counts right.
    pub fn verify(&self) -> Result<(), Error> {
        if self.saved.is_some() {
            return Err(Error::Busy);
        }
        let mut slot = self.store.try_borrow_mut().map_err(|_| Error::Busy)?;
        let Some(store) = slot.as_mut() else {
            return Ok(());
        };
        // every page is read from the volume, with its checksum: a page
        // kept in memory from an earlier read would say nothing of what
        // the volume holds now
        store.forget();
        let mut seen = BTreeSet::new();
        let mut each = |page: u64| if seen.insert(page) { Ok(()) } else { Err(Error::Damaged(page)) };
        let catalog = store.catalog();
        if tree::check(store, catalog, &mut each)?.keys != self.tables.len() as u64 {
            return Err(Error::Damaged(catalog));
        }
        for table in &self.tables {
            let found = tree::check(store, table.root, &mut each)?;
            if found.keys != table.rows || found.last.is_some_and(|last| last >= table.next_id) {
                return Err(Error::Damaged(table.root));
            }
        }
        if seen.len() as u64 != store.used() {
            return Err(Error::Damaged(0));
        }
        Ok(())
    }
}

impl Default for Database {
    fn default() -> Database {
        Database::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shop() -> (Database, usize) {
        let mut db = Database::new();
        let fruit = db.create(b"Fruit", &[(b"name", Kind::Text), (b"price", Kind::Dec(2)), (b"stock", Kind::Int)]).unwrap();
        db.insert(fruit, &[Field::Text(b"apple"), Field::Dec(30), Field::Int(12)]).unwrap();
        db.insert(fruit, &[Field::Text(b"banana"), Field::Dec(125), Field::Int(0)]).unwrap();
        db.insert(fruit, &[Field::Text(b"cherry"), Field::Null]).unwrap();
        db.commit(&mut |_, _, _| {}).unwrap();
        (db, fruit)
    }

    /// The rows as text, one per row.
    fn rows(db: &Database, table: usize) -> Vec<String> {
        let mut rows = Vec::new();
        db.scan(table, &mut |fields| {
            let cells: Vec<String> = fields
                .iter()
                .map(|field| match field {
                    Field::Null => "-".to_string(),
                    Field::Int(n) => n.to_string(),
                    Field::Dec(n) => format!("d{n}"),
                    Field::Text(t) => String::from_utf8_lossy(t).into_owned(),
                })
                .collect();
            rows.push(cells.join(" "));
        })
        .unwrap();
        rows
    }

    #[test]
    fn rows_come_back_with_their_ids() {
        let (db, fruit) = shop();
        assert_eq!(rows(&db, fruit), ["1 apple d30 12", "2 banana d125 0", "3 cherry - -"]);
        assert_eq!(db.tables()[fruit].rows(), 3);
        assert_eq!(db.tables()[fruit].name, b"fruit");
        assert_eq!(db.find(b"FRUIT"), Some(fruit));
        assert_eq!(db.tables()[fruit].column(b"Price"), Some(2));
        assert_eq!(db.verify(), Ok(()));
    }

    #[test]
    fn creating_checks_names_and_columns() {
        let (mut db, _) = shop();
        assert_eq!(db.create(b"fruit", &[(b"x", Kind::Int)]), Err(Error::Exists));
        assert_eq!(db.create(b"2fast", &[(b"x", Kind::Int)]), Err(Error::BadName));
        assert_eq!(db.create(b"t", &[(b"a b", Kind::Int)]), Err(Error::BadName));
        assert_eq!(db.create(b"t", &[]), Err(Error::ColumnCount));
        assert_eq!(db.create(b"t", &[(b"x" as &[u8], Kind::Int); MAX_COLUMNS]), Err(Error::ColumnCount));
        assert_eq!(db.create(b"t", &[(b"x", Kind::Int), (b"X", Kind::Text)]), Err(Error::DuplicateColumn));
        assert_eq!(db.create(b"t", &[(b"id", Kind::Int)]), Err(Error::DuplicateColumn));
        assert!(db.create(b"t", &[(b"x" as &[u8], Kind::Int); 1]).is_ok());
    }

    #[test]
    fn values_must_be_of_the_type_of_their_column() {
        let (mut db, fruit) = shop();
        assert_eq!(db.insert(fruit, &[Field::Int(1)]), Err(Error::WrongKind(1)));
        assert_eq!(db.insert(fruit, &[Field::Null, Field::Int(1)]), Err(Error::WrongKind(2)));
        assert_eq!(db.insert(fruit, &[Field::Null; 4]), Err(Error::ColumnCount));
        assert_eq!(db.update(fruit, &mut |_| true, &[(3, Field::Text(b"many"))]), Err(Error::WrongKind(3)));
        assert_eq!(db.update(fruit, &mut |_| true, &[(0, Field::Int(9))]), Err(Error::Key));
        assert_eq!(rows(&db, fruit).len(), 3);
    }

    #[test]
    fn rows_are_changed_one_by_one() {
        let (mut db, fruit) = shop();
        // every stock doubled, where there is one; the names untouched
        let doubled = db.update_each(fruit, &mut |fields, put| {
            if let Field::Int(stock) = fields[3] {
                let mut new = [Field::Null; 4];
                new.copy_from_slice(fields);
                new[3] = Field::Int(stock * 2);
                put(&new);
            }
        });
        assert_eq!(doubled, Ok(2));
        assert_eq!(rows(&db, fruit), ["1 apple d30 24", "2 banana d125 0", "3 cherry - -"]);
        // what is given must be a whole row of the table, with its id
        fn with<'a>(fields: &[Field<'a>], column: usize, field: Field<'a>) -> [Field<'a>; 4] {
            let mut new = [Field::Null; 4];
            new.copy_from_slice(fields);
            new[column] = field;
            new
        }
        assert_eq!(db.update_each(fruit, &mut |fields, put| put(&with(fields, 0, Field::Int(9)))), Err(Error::Key));
        assert_eq!(db.update_each(fruit, &mut |fields, put| put(&with(fields, 1, Field::Int(9)))), Err(Error::WrongKind(1)));
        assert_eq!(db.update_each(fruit, &mut |fields, put| put(&fields[..3])), Err(Error::ColumnCount));
        assert_eq!(rows(&db, fruit), ["1 apple d30 24", "2 banana d125 0", "3 cherry - -"]);
    }

    #[test]
    fn update_and_delete_touch_the_picked_rows() {
        let (mut db, fruit) = shop();
        let red = |fields: &[Field]| fields[1] != Field::Text(b"banana");
        assert_eq!(db.update(fruit, &mut { red }, &[(3, Field::Int(7)), (2, Field::Null)]), Ok(2));
        assert_eq!(rows(&db, fruit), ["1 apple - 7", "2 banana d125 0", "3 cherry - 7"]);
        assert_eq!(db.delete(fruit, &mut |fields| fields[0] == Field::Int(2)), Ok(1));
        assert_eq!(rows(&db, fruit), ["1 apple - 7", "3 cherry - 7"]);
        // an id is not given twice
        assert_eq!(db.insert(fruit, &[Field::Text(b"plum")]), Ok(4));
        assert_eq!(db.tables()[fruit].rows(), 3);
    }

    #[test]
    fn commit_reports_and_rollback_undoes() {
        let (mut db, fruit) = shop();
        let used = db.used();
        db.insert(fruit, &[Field::Text(b"plum")]).unwrap();
        db.update(fruit, &mut |fields| fields[0] == Field::Int(1), &[(1, Field::Text(b"a much longer name"))]).unwrap();
        db.delete(fruit, &mut |fields| fields[0] == Field::Int(2)).unwrap();
        db.rollback();
        assert_eq!(rows(&db, fruit), ["1 apple d30 12", "2 banana d125 0", "3 cherry - -"]);
        assert_eq!(db.tables()[fruit].rows(), 3);
        assert_eq!(db.used(), used);
        assert_eq!(db.verify(), Ok(()));

        db.insert(fruit, &[Field::Text(b"plum")]).unwrap();
        db.delete(fruit, &mut |fields| fields[0] == Field::Int(1)).unwrap();
        let mut seen = Vec::new();
        db.commit(&mut |table, change, id| seen.push((table, change, id))).unwrap();
        assert_eq!(seen, [(fruit, Change::Insert, 5), (fruit, Change::Delete, 1)]);
        db.commit(&mut |_, _, _| panic!("nothing is left to report")).unwrap();
        db.rollback();
        assert_eq!(rows(&db, fruit).len(), 3);
        assert_eq!(db.verify(), Ok(()));
    }

    #[test]
    fn a_full_database_refuses_and_stays_as_it_was() {
        // 24 pages, of which two are headers and three are kept back
        let mut db = Database::open(Volume::single(Box::new(Memory::new(24)))).ok().unwrap();
        let notes = db.create(b"notes", &[(b"body", Kind::Text)]).unwrap();
        db.commit(&mut |_, _, _| {}).unwrap();
        let mut kept = 0;
        let full = loop {
            match db.insert(notes, &[Field::Text(&[b'x'; 900])]).and_then(|_| db.commit(&mut |_, _, _| {})) {
                Ok(()) => kept += 1,
                Err(error) => break error,
            }
        };
        assert_eq!(full, Error::Full);
        db.rollback();
        assert!(kept > 40, "{kept} rows fit");
        assert_eq!(db.tables()[notes].rows(), kept);
        assert_eq!(db.verify(), Ok(()));
        // a change that needs more room is refused whole
        assert_eq!(db.update(notes, &mut |_| true, &[(1, Field::Text(&[b'y'; 990]))]), Err(Error::Full));
        db.rollback();
        assert_eq!(rows(&db, notes).len(), kept);
        assert!(rows(&db, notes).iter().all(|row| row.ends_with("xxx")));
        // removing rows always works, and makes room
        assert_eq!(db.delete(notes, &mut |fields| fields[0] != Field::Int(1)), Ok(kept - 1));
        db.commit(&mut |_, _, _| {}).unwrap();
        assert_eq!(db.used(), 2 * PAGE);
        assert!(db.insert(notes, &[Field::Text(&[b'x'; 900])]).is_ok());
        db.commit(&mut |_, _, _| {}).unwrap();
        assert_eq!(db.verify(), Ok(()));
        // and so does removing a table when nothing else fits
        while db.insert(notes, &[Field::Text(&[b'x'; 900])]).is_ok() {}
        db.rollback();
        assert_eq!(db.remove(notes), Ok(()));
        db.commit(&mut |_, _, _| {}).unwrap();
        assert_eq!(db.used(), 0);
    }

    #[test]
    fn tables_are_removed_unless_changes_are_pending() {
        let (mut db, fruit) = shop();
        let other = db.create(b"other", &[(b"x", Kind::Int)]).unwrap();
        db.insert(other, &[Field::Int(1)]).unwrap();
        assert_eq!(db.remove(fruit), Err(Error::Busy));
        assert_eq!(db.create(b"more", &[(b"x", Kind::Int)]), Err(Error::Busy));
        db.commit(&mut |_, _, _| {}).unwrap();
        let used = db.used();
        assert_eq!(db.remove(fruit), Ok(()));
        assert_eq!(db.find(b"other"), Some(0));
        db.commit(&mut |_, _, _| {}).unwrap();
        assert!(db.used() < used && db.used() > 0);
        assert_eq!(db.verify(), Ok(()));
        // a removal can be undone like any change
        db.remove(0).unwrap();
        assert!(db.tables().is_empty());
        db.rollback();
        assert_eq!(rows(&db, 0), ["1 1"]);
    }

    #[test]
    fn rows_are_reached_by_their_ids_without_reading_the_table() {
        let mut db = Database::new();
        db.set_limit(64 << 20);
        let notes = db.create(b"notes", &[(b"n", Kind::Int), (b"text", Kind::Text)]).unwrap();
        // enough rows for a tree of three levels: the ids are 1 to 6000, without those that leave below
        for n in 1..=6000 {
            db.insert(notes, &[Field::Int(n * 2), Field::Text(&[b'x'; 300])]).unwrap();
        }
        db.delete(notes, &mut |fields| matches!(fields[0], Field::Int(id) if id % 7 == 0)).unwrap();
        db.commit(&mut |_, _, _| {}).unwrap();
        let ids = |db: &Database, among: Ids| {
            let mut found = Vec::new();
            db.scan_ids(notes, among, &mut |fields| {
                let (Field::Int(id), Field::Int(n)) = (fields[0], fields[1]) else {
                    panic!("not a row: {fields:?}");
                };
                assert_eq!(n, id * 2);
                found.push(id as u64);
            })
            .unwrap();
            found
        };
        let all = ids(&db, Ids::ALL);
        assert_eq!(all.len(), 6000 - 6000 / 7);
        let whole = db.storage().looked;
        // a range gives what reading everything and keeping the range gives
        for (first, last) in [(1, 1), (7, 7), (6000, 6000), (6001, 9000), (0, 0), (5, 3), (1, 20), (2990, 3010), (5999, u64::MAX), (0, u64::MAX), (4321, 4321)] {
            let wanted: Vec<u64> = all.iter().copied().filter(|id| (first..=last).contains(id)).collect();
            assert_eq!(ids(&db, Ids { first, last }), wanted, "{first} to {last}");
        }
        // and one row costs a page for each level of the tree, not the pages of the table
        let before = db.storage().looked;
        assert_eq!(ids(&db, Ids::one(4321)), [4321]);
        let one = db.storage().looked - before;
        let before = db.storage().looked;
        ids(&db, Ids::ALL);
        let every = db.storage().looked - before;
        assert!(one <= 3 && every > 400 && whole >= every, "one row: {one} pages, all of them: {every}");

        // changing and removing rows by their ids touch no other row
        let changed = db.update_each_ids(notes, Ids { first: 100, last: 110 }, &mut |fields, put| {
            let mut new = [Field::Null; 3];
            new.copy_from_slice(fields);
            new[2] = Field::Text(b"changed");
            put(&new);
        });
        // 105 left with the multiples of seven
        assert_eq!(changed, Ok(10));
        assert_eq!(db.delete_ids(notes, Ids { first: 200, last: 202 }, &mut |fields| fields[0] != Field::Int(201)), Ok(2));
        db.commit(&mut |_, _, _| {}).unwrap();
        let mut texts = Vec::new();
        db.scan(notes, &mut |fields| {
            if let (Field::Int(id), Field::Text(text)) = (fields[0], fields[2]) {
                if text == b"changed" {
                    texts.push(id);
                }
            }
        })
        .unwrap();
        assert_eq!(texts, [100, 101, 102, 103, 104, 106, 107, 108, 109, 110]);
        assert_eq!(ids(&db, Ids { first: 199, last: 204 }), [199, 201, 204]);
        assert_eq!(db.verify(), Ok(()));

        // read leaf by leaf, the rows are the same, and each one can read the database again
        let all = ids(&db, Ids::ALL);
        for among in [Ids::ALL, Ids::one(4321), Ids { first: 2990, last: 3010 }, Ids { first: 5999, last: u64::MAX }, Ids { first: 7, last: 7 }, Ids { first: 9, last: 2 }] {
            let mut apart = Vec::new();
            db.scan_apart(notes, among, &mut |fields| {
                let Field::Int(id) = fields[0] else {
                    panic!("not a row: {fields:?}");
                };
                // the row after this one, read while this one is visited
                if apart.len() % 97 == 0 {
                    let mut after = 0;
                    db.scan_ids(notes, Ids { first: id as u64 + 1, last: id as u64 + 3 }, &mut |_| after += 1).unwrap();
                    assert!(after <= 3);
                }
                apart.push(id as u64);
            })
            .unwrap();
            assert_eq!(apart, ids(&db, among), "{among:?}");
        }
        // which the plain reading does not allow
        let mut refused = None;
        db.scan_ids(notes, Ids::one(all[0]), &mut |_| refused = db.scan(notes, &mut |_| {}).err()).unwrap();
        assert_eq!(refused, Some(Error::Busy));
    }

    #[test]
    fn records_keep_every_kind_of_value() {
        let fields = [Field::Int(-5), Field::Null, Field::Text(b""), Field::Dec(i64::MIN), Field::Text(b"caf\x82")];
        let record = encode(&fields).unwrap();
        let mut back = [Field::Null; 6];
        decode(&record, &mut back);
        assert_eq!(&back[..5], &fields);
        assert_eq!(back[5], Field::Null);
        assert_eq!(encode(&[Field::Text(&[0; 70000])]), Err(Error::TooLong));
        assert_eq!(encode(&[Field::Text(&[0; 998])]), Err(Error::TooLong));
        assert!(encode(&[Field::Text(&[0; 997])]).is_ok());
    }

    #[test]
    fn the_catalog_describes_a_table_whole() {
        let (db, fruit) = shop();
        let table = &db.tables()[fruit];
        let back = Table::described(table.key, &table.describe()).unwrap();
        assert_eq!(back.name, b"fruit");
        let columns: Vec<(&[u8], Kind)> = back.columns.iter().map(|column| (&column.name[..], column.kind)).collect();
        assert_eq!(columns, [(&b"id"[..], Kind::Int), (b"name", Kind::Text), (b"price", Kind::Dec(2)), (b"stock", Kind::Int)]);
        assert_eq!((back.rows, back.next_id, back.root), (3, 4, table.root));
        assert!(Table::described(1, &table.describe()[..10]).is_none());
    }

    /// A table whose columns ask something: a label every row has and
    /// none has twice, a code none has twice, a count that is 1 when
    /// nothing is said.
    fn ruled() -> (Database, usize) {
        let mut db = Database::new();
        let label = Rule { not_null: true, unique: true, default: None };
        let code = Rule { unique: true, ..Rule::default() };
        let count = Rule { default: Some(Field::Int(1)), ..Rule::default() };
        let parts = db.create_with(b"parts", &[(b"label", Kind::Text, label), (b"code", Kind::Int, code), (b"count", Kind::Int, count)], &[&b"count >= 0"[..], b"code is null or code > 99"]).unwrap();
        db.insert(parts, &[Field::Text(b"bolt"), Field::Int(100), Field::Int(4)]).unwrap();
        db.insert(parts, &[Field::Text(b"nut"), Field::Int(200), Field::Int(4)]).unwrap();
        db.insert(parts, &[Field::Text(b"washer"), Field::Null, Field::Int(9)]).unwrap();
        db.commit(&mut |_, _, _| {}).unwrap();
        (db, parts)
    }

    #[test]
    fn a_column_that_must_have_a_value_refuses_a_row_without_one() {
        let (mut db, parts) = ruled();
        assert_eq!(db.insert(parts, &[Field::Null, Field::Int(300)]), Err(Error::Missing(1)));
        // a field that is not given is no value either
        assert_eq!(db.insert(parts, &[]), Err(Error::Missing(1)));
        assert_eq!(db.update(parts, &mut |fields| fields[0] == Field::Int(2), &[(1, Field::Null)]), Err(Error::Missing(1)));
        // nothing of it was done: there is nothing to undo
        assert_eq!(db.create(b"more", &[(b"x", Kind::Int)]).map(|_| ()), Ok(()));
        db.rollback();
        // the other columns take no value as before
        assert_eq!(db.insert(parts, &[Field::Text(b"pin")]), Ok(4));
        assert_eq!(db.update(parts, &mut |_| true, &[(3, Field::Null)]), Ok(4));
        assert_eq!(rows(&db, parts)[3], "4 pin - -");
    }

    #[test]
    fn a_column_that_takes_no_value_twice_refuses_the_second() {
        let (mut db, parts) = ruled();
        assert_eq!(db.insert(parts, &[Field::Text(b"nut"), Field::Int(300)]), Err(Error::Twice(1, 2)));
        assert_eq!(db.insert(parts, &[Field::Text(b"rivet"), Field::Int(100)]), Err(Error::Twice(2, 1)));
        // letters as they are: another case is another value
        assert_eq!(db.insert(parts, &[Field::Text(b"Nut"), Field::Int(300)]), Ok(4));
        // no value is not a value: any number of rows may have none
        assert_eq!(db.insert(parts, &[Field::Text(b"rivet"), Field::Null]), Ok(5));
        // a row added in this change counts, before anything is committed
        assert_eq!(db.insert(parts, &[Field::Text(b"rivet"), Field::Null]), Err(Error::Twice(1, 5)));
        db.commit(&mut |_, _, _| {}).unwrap();
        // changing a row to what another has
        assert_eq!(db.update(parts, &mut |fields| fields[0] == Field::Int(4), &[(2, Field::Int(200))]), Err(Error::Twice(2, 2)));
        assert_eq!(db.update(parts, &mut |fields| fields[0] == Field::Int(5), &[(1, Field::Text(b"bolt"))]), Err(Error::Twice(1, 1)));
        // two rows changed to the same value: neither has it yet
        assert_eq!(db.update(parts, &mut |fields| matches!(fields[0], Field::Int(4 | 5)), &[(2, Field::Int(777))]), Err(Error::Twice(2, 4)));
        // a row keeps what it has, and other columns change freely
        assert_eq!(db.update(parts, &mut |_| true, &[(3, Field::Int(0))]), Ok(5));
        assert_eq!(db.update(parts, &mut |fields| fields[0] == Field::Int(1), &[(1, Field::Text(b"bolt")), (2, Field::Int(100))]), Ok(1));
        // every code one hundred more: 100 becomes 200 while another row
        // still has 200, and the table as it ends has none twice
        let moved = db.update_each(parts, &mut |fields, put| {
            if let Field::Int(code) = fields[2] {
                let mut new = [Field::Null; 4];
                new.copy_from_slice(fields);
                new[2] = Field::Int(code + 100);
                put(&new);
            }
        });
        assert_eq!(moved, Ok(3));
        // two rows exchange their labels
        let swapped = db.update_each(parts, &mut |fields, put| {
            let mut new = [Field::Null; 4];
            new.copy_from_slice(fields);
            match fields[1] {
                Field::Text(b"bolt") => new[1] = Field::Text(b"nut"),
                Field::Text(b"nut") => new[1] = Field::Text(b"bolt"),
                _ => return,
            }
            put(&new);
        });
        assert_eq!(swapped, Ok(2));
        db.commit(&mut |_, _, _| {}).unwrap();
        assert_eq!(rows(&db, parts), ["1 nut 200 0", "2 bolt 300 0", "3 washer - 0", "4 Nut 400 0", "5 rivet - 0"]);
        assert_eq!(db.verify(), Ok(()));
    }

    #[test]
    fn the_catalog_keeps_what_the_columns_ask_and_the_conditions() {
        let (db, parts) = ruled();
        let table = &db.tables()[parts];
        let back = Table::described(table.key, &table.describe()).unwrap();
        let asked: Vec<(bool, bool, Option<Kept>)> = back.columns.iter().map(|column| (column.not_null, column.unique, column.default.clone())).collect();
        assert_eq!(asked, [(false, false, None), (true, true, None), (false, true, None), (false, false, Some(Kept::Int(1)))]);
        assert_eq!(back.checks, [b"count >= 0".to_vec(), b"code is null or code > 99".to_vec()]);
        assert_eq!((back.rows, back.next_id, back.root), (3, 4, table.root));
        // cut anywhere, or with something after it, it describes nothing
        let record = table.describe();
        assert!((0..record.len()).all(|length| Table::described(1, &record[..length]).is_none_or(|table| table.checks.is_empty() && length < record.len() - 30)));
        assert!(Table::described(1, &[&record[..], &[0]].concat()).is_none());
        // a table that asks nothing is described as it always was
        let (plain, fruit) = shop();
        let plain = plain.tables()[fruit].describe();
        assert_eq!(plain.len(), 1 + 5 + 24 + 1 + (3 + 4) + (3 + 5) + (3 + 5));
        // every kind of value by default, and a text of the longest
        let mut db = Database::new();
        let long = [b'x'; 48];
        let defaults = [Field::Text(&long), Field::Dec(-250), Field::Null, Field::Int(i64::MIN)];
        let columns: Vec<(&[u8], Kind, Rule)> = [(&b"a"[..], Kind::Text), (b"b", Kind::Dec(2)), (b"c", Kind::Int), (b"d", Kind::Int)].iter().zip(defaults).map(|((name, kind), default)| (*name, *kind, Rule { default: Some(default), ..Rule::default() })).collect();
        let all = db.create_with(b"all", &columns, &[&[b'c'; MAX_CHECK][..]; MAX_CHECKS]).unwrap();
        let back = Table::described(9, &db.tables()[all].describe()).unwrap();
        assert_eq!(back.columns[1..].iter().map(|column| column.default.as_ref().map(Kept::field)).collect::<Vec<_>>(), defaults.map(Some));
        assert_eq!(back.checks.len(), MAX_CHECKS);
    }

    #[test]
    fn a_table_that_asks_too_much_is_not_made() {
        let mut db = Database::new();
        let wrong = Rule { default: Some(Field::Text(b"one")), ..Rule::default() };
        assert_eq!(db.create_with(b"t", &[(b"a", Kind::Text, Rule::default()), (b"n", Kind::Int, wrong)], &[]), Err(Error::WrongKind(2)));
        let plain = [(&b"a"[..], Kind::Int, Rule::default())];
        assert_eq!(db.create_with(b"t", &plain, &[&b"a > 0"[..]; MAX_CHECKS + 1]), Err(Error::Checks));
        assert_eq!(db.create_with(b"t", &plain, &[&[b'a'; MAX_CHECK + 1][..]]), Err(Error::Checks));
        assert_eq!(db.create_with(b"t", &plain, &[b""]), Err(Error::Checks));
        // nothing was started: a table can be made right after
        assert!(db.create_with(b"t", &plain, &[b"a > 0"]).is_ok());
    }

    #[test]
    fn a_database_in_memory_says_so() {
        let (db, _) = shop();
        let storage = db.storage();
        assert_eq!((storage.disk, storage.keeps, storage.stopped), ("memory", false, false));
        assert_eq!(storage.pages, 2048);
        assert_eq!(db.limit(), 8 << 20);
        // the catalog and one page of rows
        assert_eq!(db.used(), 2 * PAGE);
    }
}
