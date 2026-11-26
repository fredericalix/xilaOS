//! A disk whose power can be cut, and the tests that cut it.
//!
//! `Platter` models what a real disk promises: a write that was followed
//! by a finished flush is there after a power cut. Of the writes since the
//! last flush any may be there or not, in any combination, and the one
//! under way may be there in part.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::*;

#[derive(Clone)]
pub struct Platter {
    pages: u64,
    /// What a power cut cannot take away.
    safe: BTreeMap<u64, Box<Page>>,
    /// The writes since the last flush, in order.
    waiting: Vec<(u64, Box<Page>)>,
    /// The write that was under way when the power went.
    flying: Option<(u64, Box<Page>)>,
    /// Writes and flushes so far.
    pub ops: usize,
    /// The write or flush that fails; 0 for none.
    pub cut_at: usize,
    /// The failure is that one operation only: the disk goes on working.
    pub hiccup: bool,
    dead: bool,
}

/// How a power cut treats what was not flushed.
const MODES: u32 = 7;

impl Platter {
    pub fn new(pages: u64) -> Shared {
        Rc::new(RefCell::new(Platter {
            pages,
            safe: BTreeMap::new(),
            waiting: Vec::new(),
            flying: None,
            ops: 0,
            cut_at: 0,
            hiccup: false,
            dead: false,
        }))
    }

    fn current(&self, page: u64) -> Box<Page> {
        let waiting = self.waiting.iter().rev().find(|(at, _)| *at == page).map(|(_, data)| data);
        waiting.or(self.safe.get(&page)).cloned().unwrap_or(Box::new([0; PAGE]))
    }

    /// Counts an operation. False if it is the one that fails.
    fn step(&mut self) -> bool {
        self.ops += 1;
        if self.ops != self.cut_at {
            return true;
        }
        self.dead = !self.hiccup;
        false
    }

    /// The power comes back. `mode` picks what became of the writes that
    /// were not flushed.
    pub fn restart(&mut self, mode: u32) {
        let mut random = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(mode as u64 + self.ops as u64 + 1);
        for (page, data) in std::mem::take(&mut self.waiting) {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let kept = match mode {
                0 | 5 => false,
                1 | 2 | 6 => true,
                _ => random % 2 == 0,
            };
            if kept {
                self.safe.insert(page, data);
            }
        }
        if let Some((page, data)) = self.flying.take() {
            match mode {
                1 | 5 => {
                    self.safe.insert(page, data);
                }
                2 | 3 => {
                    let mut torn = self.current(page);
                    torn[..PAGE / 2].copy_from_slice(&data[..PAGE / 2]);
                    self.safe.insert(page, torn);
                }
                // some disks leave a page that reads as noise
                6 => {
                    self.safe.insert(page, Box::new([0x5a; PAGE]));
                }
                _ => {}
            }
        }
        (self.dead, self.cut_at, self.ops) = (false, 0, 0);
    }

    /// Changes one byte of a page behind the back of the database.
    pub fn damage(&mut self, page: u64, at: usize) {
        let mut data = self.current(page);
        data[at] ^= 0x40;
        self.safe.insert(page, data);
    }

    /// The pages that hold something, the headers left out.
    pub fn written(&self) -> Vec<u64> {
        self.safe.keys().copied().filter(|page| *page >= 2).collect()
    }
}

pub type Shared = Rc<RefCell<Platter>>;

struct Flaky(Shared);

impl Disk for Flaky {
    fn name(&self) -> &'static str {
        "platter"
    }

    fn pages(&self) -> u64 {
        self.0.borrow().pages
    }

    fn read(&mut self, page: u64, into: &mut Page) -> Result<(), DiskError> {
        let platter = self.0.borrow();
        if platter.dead {
            return Err(DiskError);
        }
        into.copy_from_slice(&platter.current(page)[..]);
        Ok(())
    }

    fn write(&mut self, page: u64, from: &Page) -> Result<(), DiskError> {
        let mut platter = self.0.borrow_mut();
        if platter.dead {
            return Err(DiskError);
        }
        if !platter.step() {
            if platter.dead {
                platter.flying = Some((page, Box::new(*from)));
            }
            return Err(DiskError);
        }
        platter.waiting.push((page, Box::new(*from)));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        let mut platter = self.0.borrow_mut();
        if platter.dead || !platter.step() {
            return Err(DiskError);
        }
        for (page, data) in std::mem::take(&mut platter.waiting) {
            platter.safe.insert(page, data);
        }
        Ok(())
    }
}

pub fn open(platter: &Shared) -> Result<Database, Error> {
    Database::open(Volume::single(Box::new(Flaky(platter.clone())))).map_err(|(error, _)| error)
}

fn copy(platter: &Shared) -> Shared {
    Rc::new(RefCell::new(platter.borrow().clone()))
}

fn commit(db: &mut Database) -> Result<(), Error> {
    db.commit(&mut |_, _, _| {})
}

/// Everything a database holds, as text.
pub fn dump(db: &Database) -> Vec<String> {
    let mut lines = Vec::new();
    for (index, table) in db.tables().iter().enumerate() {
        let columns: Vec<String> = table.columns.iter().map(|c| format!("{}:{:?}", String::from_utf8_lossy(&c.name), c.kind)).collect();
        lines.push(format!("table {} ({}) {} rows", String::from_utf8_lossy(&table.name), columns.join(", "), table.rows()));
        db.scan(index, &mut |fields| {
            let cells: Vec<String> = fields
                .iter()
                .map(|field| match field {
                    Field::Text(text) => format!("{}x{:?}", text.len(), text.first()),
                    other => format!("{other:?}"),
                })
                .collect();
            lines.push(cells.join(" "));
        })
        .unwrap();
    }
    lines
}

fn text(byte: u8, length: usize) -> Vec<u8> {
    vec![byte; length]
}

/// One transaction of a scenario: changes, then a commit.
type Step = fn(&mut Database) -> Result<(), Error>;

/// Cuts the power at every write and every flush of a scenario, in every
/// way the disk may then behave, and checks what the database is after the
/// restart: whole, and in the state of the last commit that returned, or of
/// the one that was under way.
///
/// `thorough` tries every behavior of the disk at every cut. Otherwise each
/// cut gets one, in turn: enough when a scenario is long to run.
fn cut_everywhere(base: &Shared, steps: &[Step], thorough: bool) {
    // without a cut: the states the database goes through
    let whole = copy(base);
    let mut states = vec![dump(&open(&whole).unwrap())];
    {
        let mut db = open(&whole).unwrap();
        for step in steps {
            step(&mut db).unwrap();
            states.push(dump(&open(&whole).unwrap()));
            assert_eq!(dump(&db), states[states.len() - 1]);
        }
        assert_eq!(db.verify(), Ok(()));
    }
    let ops = whole.borrow().ops;
    assert!(ops >= steps.len() * 4, "a commit is at least a page, a flush, the header, a flush");

    for cut in 1..=ops {
        let modes = if thorough { 0..MODES } else { cut as u32 % MODES..cut as u32 % MODES + 1 };
        for mode in modes {
            let platter = copy(base);
            platter.borrow_mut().cut_at = cut;
            let mut done = 0;
            {
                let mut db = open(&platter).unwrap();
                for step in steps {
                    match step(&mut db) {
                        Ok(()) => done += 1,
                        // the machine is off: nothing more happens
                        Err(_) => break,
                    }
                }
            }
            platter.borrow_mut().restart(mode);
            let what = format!("cut at operation {cut} of {ops}, mode {mode}, {done} commits returned");
            let mut db = open(&platter).unwrap_or_else(|error| panic!("{what}: does not open: {error:?}"));
            assert_eq!(db.verify(), Ok(()), "{what}");
            let found = dump(&db);
            let under_way = states.get(done + 1);
            assert!(found == states[done] || Some(&found) == under_way, "{what}: found a state that never was");

            // and it goes on working
            let more = db.create(b"after_the_cut", &[(b"x", Kind::Int)]).unwrap();
            db.insert(more, &[Field::Int(1)]).unwrap();
            commit(&mut db).unwrap_or_else(|error| panic!("{what}: no more commits: {error:?}"));
            let after = dump(&db);
            assert_eq!(after.len(), found.len() + 2);
            let db = open(&platter).unwrap();
            assert_eq!(db.verify(), Ok(()), "{what}");
            assert_eq!(dump(&db), after);
        }
    }
}

fn notes(db: &Database) -> usize {
    db.find(b"notes").unwrap()
}

/// A database with two tables, one of them over several pages.
fn office() -> Shared {
    let platter = Platter::new(512);
    let mut db = open(&platter).unwrap();
    assert!(db.fresh());
    let notes = db.create(b"notes", &[(b"title", Kind::Text), (b"n", Kind::Int)]).unwrap();
    for n in 0..40 {
        db.insert(notes, &[Field::Text(&text(b'a', 100 + n)), Field::Int(n as i64)]).unwrap();
    }
    commit(&mut db).unwrap();
    let small = db.create(b"small", &[(b"x", Kind::Dec(2))]).unwrap();
    for n in 0..3 {
        db.insert(small, &[Field::Dec(n)]).unwrap();
    }
    commit(&mut db).unwrap();
    platter
}

#[test]
fn a_power_cut_while_rows_change_leaves_the_old_state_or_the_new() {
    cut_everywhere(
        &office(),
        &[
            |db| {
                for n in 0..60 {
                    db.insert(notes(db), &[Field::Text(&text(b'b', 300)), Field::Int(n)])?;
                }
                commit(db)
            },
            |db| {
                let third = |fields: &[Field]| matches!(fields[0], Field::Int(id) if id % 3 == 0);
                db.update(notes(db), &mut { third }, &[(1, Field::Text(&text(b'c', 700)))])?;
                commit(db)
            },
            |db| {
                db.delete(notes(db), &mut |fields| matches!(fields[0], Field::Int(id) if id % 2 == 0))?;
                commit(db)
            },
            |db| {
                db.delete(db.find(b"small").unwrap(), &mut |_| true)?;
                db.insert(notes(db), &[Field::Null, Field::Int(-1)])?;
                commit(db)
            },
        ],
        true,
    );
}

#[test]
fn a_power_cut_while_tables_come_and_go_leaves_the_old_state_or_the_new() {
    cut_everywhere(
        &office(),
        &[
            |db| {
                let more = db.create(b"more", &[(b"a", Kind::Int), (b"b", Kind::Text)])?;
                for n in 0..5 {
                    db.insert(more, &[Field::Int(n), Field::Text(b"five")])?;
                }
                commit(db)
            },
            |db| {
                db.remove(notes(db))?;
                commit(db)
            },
            |db| {
                let notes = db.create(b"notes", &[(b"other", Kind::Dec(4))])?;
                db.insert(notes, &[Field::Dec(31416)])?;
                commit(db)
            },
            |db| {
                db.remove(db.find(b"small").unwrap())?;
                db.remove(db.find(b"more").unwrap())?;
                commit(db)
            },
        ],
        true,
    );
}

/// A table tall enough for its tree to have branches under the root.
fn tower() -> Shared {
    let platter = Platter::new(2048);
    let mut db = open(&platter).unwrap();
    let notes = db.create(b"notes", &[(b"title", Kind::Text)]).unwrap();
    for n in 0..1100 {
        db.insert(notes, &[Field::Text(&text(b'a' + (n % 20) as u8, 950))]).unwrap();
        if n % 100 == 99 {
            commit(&mut db).unwrap();
        }
    }
    commit(&mut db).unwrap();
    platter
}

#[test]
fn a_power_cut_in_a_tall_tree_leaves_the_old_state_or_the_new() {
    let platter = tower();
    // four rows to a page: more pages than one branch can hold
    assert!(open(&platter).unwrap().used() > 256 * PAGE);
    cut_everywhere(
        &platter,
        &[
            |db| {
                for _ in 0..10 {
                    db.insert(notes(db), &[Field::Text(&text(b'z', 950))])?;
                }
                commit(db)
            },
            |db| {
                db.delete(notes(db), &mut |fields| matches!(fields[0], Field::Int(id) if id % 50 < 4))?;
                commit(db)
            },
            |db| {
                db.update(notes(db), &mut |fields| matches!(fields[0], Field::Int(id) if id % 97 == 0), &[(1, Field::Text(b"short"))])?;
                commit(db)
            },
        ],
        false,
    );
}

#[test]
fn a_tall_tree_empties_and_gives_its_pages_back() {
    let platter = tower();
    let mut db = open(&platter).unwrap();
    assert_eq!(db.tables()[0].rows(), 1100);
    assert_eq!(db.verify(), Ok(()));
    assert_eq!(db.delete(0, &mut |fields| matches!(fields[0], Field::Int(id) if id > 3)), Ok(1097));
    commit(&mut db).unwrap();
    assert_eq!(db.verify(), Ok(()));
    // the catalog and one page of rows
    assert_eq!(db.used(), 2 * PAGE);
    let db = open(&platter).unwrap();
    assert_eq!(dump(&db).len(), 4);
    assert_eq!(db.used(), 2 * PAGE);
}

#[test]
fn what_was_committed_is_there_after_a_restart() {
    let platter = office();
    let before = {
        let mut db = open(&platter).unwrap();
        assert!(!db.fresh());
        let storage = db.storage();
        assert_eq!((storage.disk, storage.keeps, storage.pages), ("platter", true, 512));
        db.insert(notes(&db), &[Field::Text(b"kept"), Field::Int(7)]).unwrap();
        commit(&mut db).unwrap();
        // what was not committed is not kept
        db.insert(notes(&db), &[Field::Text(b"lost"), Field::Int(8)]).unwrap();
        db.rollback();
        db.insert(notes(&db), &[Field::Text(b"lost too"), Field::Int(9)]).unwrap();
        db.rollback();
        dump(&db)
    };
    let mut db = open(&platter).unwrap();
    assert_eq!(dump(&db), before);
    assert_eq!(db.tables()[0].rows(), 41);
    assert_eq!(db.verify(), Ok(()));
    // ids go on from where they were, even when the rows are gone
    db.delete(1, &mut |_| true).unwrap();
    commit(&mut db).unwrap();
    let mut db = open(&platter).unwrap();
    assert_eq!(db.insert(1, &[Field::Dec(5)]), Ok(4));
}

#[test]
fn what_the_columns_ask_is_there_after_a_restart() {
    let platter = Platter::new(64);
    {
        let mut db = open(&platter).unwrap();
        let label = Rule { not_null: true, unique: true, default: None };
        let count = Rule { default: Some(Field::Int(1)), ..Rule::default() };
        let parts = db.create_with(b"parts", &[(b"label", Kind::Text, label), (b"count", Kind::Int, count)], &[b"count >= 0"]).unwrap();
        db.insert(parts, &[Field::Text(b"bolt"), Field::Int(4)]).unwrap();
        commit(&mut db).unwrap();
    }
    let mut db = open(&platter).unwrap();
    assert_eq!(db.verify(), Ok(()));
    let table = &db.tables()[0];
    assert_eq!((table.columns[1].not_null, table.columns[1].unique, table.columns[2].default.clone()), (true, true, Some(Kept::Int(1))));
    assert_eq!(table.checks, [b"count >= 0".to_vec()]);
    // and is asked of the rows that come after it
    assert_eq!(db.insert(0, &[Field::Text(b"bolt"), Field::Int(1)]), Err(Error::Twice(1, 1)));
    assert_eq!(db.insert(0, &[Field::Null, Field::Int(1)]), Err(Error::Missing(1)));
}

/// Makes the headers of a volume say a version, as another XilaOS would
/// have written them. Returns how many headers there were.
fn dated(platter: &Shared, version: u32) -> usize {
    let mut platter = platter.borrow_mut();
    assert!(platter.waiting.is_empty());
    let mut headers = 0;
    for slot in 0..2 {
        let mut page = platter.current(slot);
        if crate::store::dated(&mut page, version) {
            platter.safe.insert(slot, page);
            headers += 1;
        }
    }
    headers
}

/// The versions the headers of a volume say, the newest commit first.
fn versions(platter: &Shared) -> Vec<u32> {
    let platter = platter.borrow();
    let mut found: Vec<(u64, u32)> = (0..2).map(|slot| platter.current(slot)).filter(|page| page[4..12] == *b"XilaOSdb").map(|page| (u64::from_le_bytes(page[20..28].try_into().unwrap()), u32::from_le_bytes(page[12..16].try_into().unwrap()))).collect();
    found.sort();
    found.iter().rev().map(|(_, version)| *version).collect()
}

#[test]
fn a_volume_of_version_1_is_read_and_is_of_version_2_once_it_is_written() {
    // tables without anything asked of their columns are, byte for byte,
    // what version 1 wrote: only the headers say the version
    let platter = office();
    assert_eq!(versions(&platter), [2, 2]);
    assert_eq!(dated(&platter, 1), 2);
    assert_eq!(versions(&platter), [1, 1]);
    let before = dump(&open(&platter).unwrap());
    // read as it is; reading writes nothing
    let mut db = open(&platter).unwrap();
    assert!(!db.fresh());
    assert_eq!(db.verify(), Ok(()));
    assert_eq!((db.tables().len(), db.tables()[0].rows(), db.tables()[0].checks.len()), (2, 40, 0));
    assert_eq!(versions(&platter), [1, 1]);
    // the first commit says the new version, the second leaves no header of the old
    db.insert(notes(&db), &[Field::Text(b"written by the new one"), Field::Int(41)]).unwrap();
    commit(&mut db).unwrap();
    assert_eq!(versions(&platter), [2, 1]);
    let made = db.create_with(b"ruled", &[(b"x", Kind::Int, Rule { not_null: true, ..Rule::default() })], &[]).unwrap();
    db.insert(made, &[Field::Int(1)]).unwrap();
    commit(&mut db).unwrap();
    assert_eq!(versions(&platter), [2, 2]);
    let db = open(&platter).unwrap();
    assert_eq!(db.verify(), Ok(()));
    assert_eq!(dump(&db)[..before.len() - 3].len(), before.len() - 3);
    assert_eq!((db.tables()[0].rows(), db.tables().len(), db.tables()[2].columns[1].not_null), (41, 3, true));
    // a version this code does not know is something else, and is left alone
    assert_eq!(dated(&platter, 3), 2);
    assert_eq!(open(&platter).err(), Some(Error::Foreign));
    assert_eq!(dated(&platter, 0), 0);
}

#[test]
fn a_disk_that_holds_something_else_is_left_alone() {
    let platter = Platter::new(64);
    platter.borrow_mut().damage(1, 510);
    assert_eq!(open(&platter).err(), Some(Error::Foreign));
    assert!(platter.borrow().written().is_empty());
    assert_eq!(platter.borrow().ops, 0);
    // and one that is too small is refused
    assert_eq!(open(&Platter::new(8)).err(), Some(Error::TooSmall));
}

#[test]
fn a_refused_disk_is_given_back_and_can_be_erased_on_purpose() {
    let platter = Platter::new(64);
    platter.borrow_mut().damage(1, 510);
    platter.borrow_mut().damage(9, 0);
    let Err((error, volume)) = Database::open(Volume::single(Box::new(Flaky(platter.clone())))) else {
        panic!("a disk that holds something else was opened");
    };
    assert_eq!((error, volume.pages()), (Error::Foreign, 64));
    let mut db = Database::format(volume).ok().unwrap();
    assert!(db.fresh() && db.tables().is_empty());
    let notes = db.create(b"notes", &[(b"title", Kind::Text)]).unwrap();
    db.insert(notes, &[Field::Text(b"kept")]).unwrap();
    commit(&mut db).unwrap();
    let db = open(&platter).unwrap();
    assert!(!db.fresh());
    assert_eq!(dump(&db).len(), 2);
    assert_eq!(db.verify(), Ok(()));
    // a database that is damaged is given back too
    let page = *platter.borrow().written().first().unwrap();
    platter.borrow_mut().damage(page, 100);
    let Err((error, volume)) = Database::open(Volume::single(Box::new(Flaky(platter.clone())))) else {
        panic!("a damaged database was opened");
    };
    assert_eq!(error, Error::Damaged(page));
    assert!(Database::format(volume).ok().unwrap().tables().is_empty());
    assert!(open(&platter).unwrap().tables().is_empty());
}

#[test]
fn a_damaged_page_is_noticed_when_it_is_read() {
    let platter = office();
    let pages = platter.borrow().written();
    let mut noticed = 0;
    for &page in &pages {
        let damaged = copy(&platter);
        damaged.borrow_mut().damage(page, 2000);
        // nothing wrong is ever returned: either the damage is in a page
        // the database does not use any more, or reading it fails
        match open(&damaged) {
            Err(error) => {
                assert_eq!(error, Error::Damaged(page));
                noticed += 1;
            }
            Ok(db) => {
                let mut failed = false;
                for table in 0..db.tables().len() {
                    match db.scan(table, &mut |_| {}) {
                        Ok(()) => {}
                        Err(error) => {
                            assert_eq!(error, Error::Damaged(page));
                            failed = true;
                        }
                    }
                }
                assert_eq!(failed, db.verify().is_err());
                noticed += failed as usize;
            }
        }
    }
    // the catalog, the branch and the leaves of notes, the leaf of small
    assert!(noticed >= 4, "{noticed} of {} pages", pages.len());
}

#[test]
fn a_damaged_header_brings_back_the_commit_before() {
    let platter = office();
    let mut db = open(&platter).unwrap();
    let before = dump(&db);
    let newest = db.storage().transaction % 2;
    db.insert(notes(&db), &[Field::Text(b"one more"), Field::Int(0)]).unwrap();
    commit(&mut db).unwrap();
    assert_ne!(dump(&db), before);
    platter.borrow_mut().damage(1 - newest, 30);
    let db = open(&platter).unwrap();
    assert_eq!(dump(&db), before);
    assert_eq!(db.verify(), Ok(()));
}

#[test]
fn a_write_that_fails_undoes_the_change_and_can_be_tried_again() {
    let platter = office();
    let mut db = open(&platter).unwrap();
    let before = dump(&db);
    let change = |db: &mut Database| {
        db.insert(notes(db), &[Field::Text(b"again"), Field::Int(1)])?;
        commit(db)
    };
    (platter.borrow_mut().cut_at, platter.borrow_mut().hiccup) = (platter.borrow().ops + 1, true);
    assert_eq!(change(&mut db), Err(Error::Disk));
    assert_eq!(dump(&db), before);
    assert!(!db.storage().stopped);
    assert_eq!(change(&mut db), Ok(()));
    assert_eq!(db.verify(), Ok(()));
    assert_eq!(dump(&open(&platter).unwrap()), dump(&db));
}

#[test]
fn a_header_that_fails_stops_the_changes_but_not_the_reading() {
    let platter = office();
    let mut db = open(&platter).unwrap();
    let before = dump(&db);
    let change = |db: &mut Database| {
        db.insert(notes(db), &[Field::Text(b"never"), Field::Int(1)]).unwrap();
        commit(db)
    };
    // a commit ends with the header and a flush: count on a copy
    let trial = copy(&platter);
    change(&mut open(&trial).unwrap()).unwrap();
    let header = trial.borrow().ops - 1;
    (platter.borrow_mut().cut_at, platter.borrow_mut().hiccup) = (header, true);
    assert_eq!(change(&mut db), Err(Error::Disk));
    assert!(db.storage().stopped);
    assert_eq!(dump(&db), before);
    assert_eq!(db.insert(notes(&db), &[Field::Text(b"no"), Field::Int(1)]), Err(Error::Stopped));
    db.rollback();
    assert_eq!(dump(&db), before);
    // the disk holds a whole state, whichever it is
    let db = open(&platter).unwrap();
    assert_eq!(db.verify(), Ok(()));
    assert_eq!(dump(&db), before);
}

/// Small numbers that look random and are the same at every run.
struct Dice(u64);

impl Dice {
    fn roll(&mut self, sides: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) % sides
    }
}

type Model = BTreeMap<String, BTreeMap<u64, (i64, Vec<u8>)>>;

fn dump_model(model: &Model, db: &Database) -> Vec<String> {
    // tables in the order the database lists them
    let mut lines = Vec::new();
    for table in db.tables() {
        let name = String::from_utf8_lossy(&table.name).into_owned();
        let rows = &model[&name];
        lines.push(format!("table {name} (id:Int, a:Int, b:Text) {} rows", rows.len()));
        for (id, (a, b)) in rows {
            lines.push(format!("Int({id}) Int({a}) {}x{:?}", b.len(), b.first()));
        }
    }
    lines
}

#[test]
fn many_changes_at_random_match_a_plain_map() {
    let platter = Platter::new(1024);
    let mut db = open(&platter).unwrap();
    let mut dice = Dice(0x2545_f491_4f6c_dd1d);
    let mut model = Model::new();
    let mut committed = model.clone();
    for round in 0..4000 {
        let name = format!("t{}", dice.roll(3));
        let table = db.find(name.as_bytes());
        let odd = dice.roll(4) as i64;
        let result = match (dice.roll(100), table) {
            (0..=5, None) => db.create(name.as_bytes(), &[(b"a", Kind::Int), (b"b", Kind::Text)]).map(|_| {
                model.insert(name, BTreeMap::new());
            }),
            (6, Some(table)) => db.remove(table).map(|_| {
                model.remove(&name);
            }),
            (7..=60, Some(table)) => {
                let (a, b) = (dice.roll(1000) as i64, text(b'a' + dice.roll(26) as u8, dice.roll(40) as usize * dice.roll(24) as usize));
                db.insert(table, &[Field::Int(a), Field::Text(&b)]).map(|id| {
                    assert!(model.get_mut(&name).unwrap().insert(id, (a, b)).is_none(), "id {id} given twice");
                })
            }
            (61..=75, Some(table)) => {
                let b = text(b'A' + dice.roll(26) as u8, dice.roll(900) as usize);
                let pick = |id: i64| id % 4 == odd;
                db.update(table, &mut |fields| matches!(fields[0], Field::Int(id) if pick(id)), &[(2, Field::Text(&b))]).map(|count| {
                    let rows = model.get_mut(&name).unwrap();
                    let picked: Vec<u64> = rows.keys().copied().filter(|id| pick(*id as i64)).collect();
                    assert_eq!(count, picked.len());
                    for id in picked {
                        rows.get_mut(&id).unwrap().1 = b.clone();
                    }
                })
            }
            (76..=88, Some(table)) => {
                let pick = |id: i64| id % 4 == odd || (odd == 0 && id % 3 == 0);
                db.delete(table, &mut |fields| matches!(fields[0], Field::Int(id) if pick(id))).map(|count| {
                    let rows = model.get_mut(&name).unwrap();
                    let before = rows.len();
                    rows.retain(|id, _| !pick(*id as i64));
                    assert_eq!(count, before - rows.len());
                })
            }
            _ => Ok(()),
        };
        let end = match result {
            // tables come and go only between transactions
            Err(Error::Busy) => 0,
            Err(Error::Full) => 9,
            Err(error) => panic!("round {round}: {error:?}"),
            Ok(()) => dice.roll(10),
        };
        match end {
            0..=5 => {
                commit(&mut db).unwrap_or_else(|error| panic!("round {round}: commit: {error:?}"));
                committed = model.clone();
                assert_eq!(db.verify(), Ok(()), "round {round}");
            }
            9 => {
                db.rollback();
                model = committed.clone();
            }
            _ => {}
        }
        if end == 9 || round % 7 == 0 {
            assert_eq!(dump(&db), dump_model(&model, &db), "round {round}");
        }
        if round % 200 == 199 {
            // a restart forgets what was not committed
            drop(db);
            db = open(&platter).unwrap();
            model = committed.clone();
            assert_eq!(dump(&db), dump_model(&model, &db), "round {round}, after a restart");
            assert_eq!(db.verify(), Ok(()));
        }
    }
    let rows: usize = model.values().map(|rows| rows.len()).sum();
    assert!(rows > 0 && db.storage().transaction > 1000, "{rows} rows, {:?}", db.storage());
}
