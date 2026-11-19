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
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    Insert,
    Update,
    Delete,
}

#[derive(Clone)]
pub struct Column {
    pub name: Vec<u8>,
    pub kind: Kind,
}

#[derive(Clone)]
pub struct Table {
    pub name: Vec<u8>,
    /// Column 0 is `id`.
    pub columns: Vec<Column>,
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
    /// the columns after `id` with their type.
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
        columns.push(Column { name: b"id".to_vec(), kind: Kind::Int });
        for _ in 0..count {
            let kind = match (reader.byte()?, reader.byte()?) {
                (KIND_INT, _) => Kind::Int,
                (KIND_DEC, scale) => Kind::Dec(scale),
                (KIND_TEXT, _) => Kind::Text,
                _ => return None,
            };
            columns.push(Column { name: reader.name()?, kind });
        }
        Some(Table { name, columns, key, root, rows, next_id, changed: false, read_at: 0, written_at: 0 })
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
        let none = Storage { disk: "memory", keeps: false, pages: (self.limit / PAGE) as u64, used: 0, transaction: 0, stopped: false };
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
        if !self.log.is_empty() {
            return Err(Error::Busy);
        }
        if !good_name(name) || columns.iter().any(|(column, _)| !good_name(column)) {
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
        let mut made = Vec::new();
        made.push(Column { name: b"id".to_vec(), kind: Kind::Int });
        for (column, kind) in columns {
            if made.iter().any(|c: &Column| c.name.eq_ignore_ascii_case(column)) {
                return Err(Error::DuplicateColumn);
            }
            made.push(Column { name: column.to_ascii_lowercase(), kind: *kind });
        }
        self.begin();
        let key = self.next_table;
        self.next_table += 1;
        self.tables.push(Table {
            name: name.to_ascii_lowercase(),
            columns: made,
            key,
            root: 0,
            rows: 0,
            next_id: 1,
            changed: true,
            read_at: 0,
            written_at: 0,
        });
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

    /// Calls `visit` with every row, in the order of their ids. The first
    /// field is the id.
    pub fn scan(&self, table: usize, visit: &mut dyn FnMut(&[Field])) -> Result<(), Error> {
        let table = &self.tables[table];
        let count = table.columns.len();
        let mut slot = self.store.try_borrow_mut().map_err(|_| Error::Busy)?;
        let Some(store) = slot.as_mut() else {
            return Ok(());
        };
        tree::walk(store, table.root, &mut |id, record| {
            let mut fields = [Field::Null; MAX_COLUMNS];
            fields[0] = Field::Int(id as i64);
            decode(record, &mut fields[1..count]);
            visit(&fields[..count]);
        })
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
        let kinds: Vec<Kind> = self.tables[table].columns.iter().map(|column| column.kind).collect();
        // the new records are worked out first: the tree is not changed
        // while it is read
        let mut records: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut failed = None;
        self.scan(table, &mut |fields| {
            let Field::Int(id) = fields[0] else {
                return;
            };
            if failed.is_some() {
                return;
            }
            let mut put = |new: &[Field]| {
                let wrong = new.iter().zip(&kinds).position(|(field, kind)| !fits(*kind, field));
                let record = match wrong {
                    _ if new.len() != fields.len() => Err(Error::ColumnCount),
                    _ if new[0] != fields[0] => Err(Error::Key),
                    Some(column) => Err(Error::WrongKind(column)),
                    None => encode(&new[1..]),
                };
                match record {
                    Ok(record) => records.push((id as u64, record)),
                    Err(error) => failed = Some(error),
                }
            };
            each(fields, &mut put);
        })?;
        if let Some(error) = failed {
            return Err(error);
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
        let mut ids = Vec::new();
        self.scan(table, &mut |fields| {
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
