//! Keys: what opens a service of the Internet to this machine.
//!
//! A key is a secret: whoever reads it can spend what it pays for. There
//! is no vault yet, so this is the least that keeps one from being shown
//! by accident. Keys are rows of a stored table, `kept_keys`, that
//! nothing reads but this file:
//!
//! - SQL does not know the table (`catalog.rs`, `change.rs`): it is not
//!   listed, cannot be read, changed, made or dropped under that name.
//!   So neither a `select`, nor a program, nor the pages for browsers,
//!   nor the companion can show a key;
//! - no command shows one: `key` lists their names only;
//! - the line that sets one is not kept in the history of the prompt,
//!   and the screen shows stars in place of the key (`shell::typed`).
//!
//! (`keys.rs` is the keyboard.)
//!
//! What this does not do: the disk is not encrypted, so a key is on it
//! as it was typed, for whoever reads the disk from another system. And
//! the kernel has no memory protection: nothing here is a barrier
//! against code that runs in it.

use xila_db::{Database, Error, Field, Kind};

use crate::text::FixedStr;

/// The table keys are kept in. SQL refuses this name.
pub const TABLE: &[u8] = b"kept_keys";
/// The longest key.
pub const VALUE: usize = 200;

pub type Value = FixedStr<VALUE>;

/// Whether a text can be a key: no space, nothing a line could not hold.
pub fn good_value(value: &[u8]) -> bool {
    !value.is_empty() && value.len() <= VALUE && value.iter().all(|byte| (0x21..0x7f).contains(byte))
}

/// The table of keys, if it is there.
fn table(db: &Database) -> Option<usize> {
    let table = db.find(TABLE)?;
    let columns = &db.tables()[table].columns;
    let wanted: [(&[u8], Kind); 3] = [(b"id", Kind::Int), (b"name", Kind::Text), (b"value", Kind::Text)];
    (columns.len() == wanted.len() && columns.iter().zip(&wanted).all(|(column, (name, kind))| column.name == *name && column.kind == *kind)).then_some(table)
}

/// Calls `visit` with the name of each key that is kept.
pub fn names(db: &Database, visit: &mut dyn FnMut(&[u8])) {
    if let Some(table) = table(db) {
        let _ = db.scan(table, &mut |fields| {
            if let Field::Text(name) = fields[1] {
                visit(name);
            }
        });
    }
}

/// The key kept under a name, for whoever is about to use it.
pub fn get(db: &Database, name: &[u8]) -> Option<Value> {
    let mut found = None;
    let _ = db.scan(table(db)?, &mut |fields| {
        if let (Field::Text(of), Field::Text(value)) = (fields[1], fields[2]) {
            if of.eq_ignore_ascii_case(name) {
                found = Some(Value::from_bytes(value));
            }
        }
    });
    found
}

/// Keeps a key under a name, in place of the one that was there. Returns
/// whether there was one. It is on the volume when this returns.
pub fn set(db: &mut Database, name: &[u8], value: &[u8]) -> Result<bool, Error> {
    let done = (|| {
        let table = match db.find(TABLE) {
            // a table of that name that is not ours is not written to
            Some(_) => table(db).ok_or(Error::BadName)?,
            None => db.create(TABLE, &[(b"name", Kind::Text), (b"value", Kind::Text)])?,
        };
        let replaced = db.delete(table, &mut |fields| matches!(fields[1], Field::Text(of) if of.eq_ignore_ascii_case(name)))? > 0;
        db.insert(table, &[Field::Text(name), Field::Text(value)])?;
        Ok(replaced)
    })();
    match done {
        // nobody is told: an event would at least say that a key changed
        Ok(replaced) => db.commit(&mut |_, _, _| {}).map(|_| replaced),
        Err(why) => {
            db.rollback();
            Err(why)
        }
    }
}

/// Forgets the key kept under a name. Returns whether there was one.
pub fn forget(db: &mut Database, name: &[u8]) -> Result<bool, Error> {
    let Some(table) = table(db) else {
        return Ok(false);
    };
    match db.delete(table, &mut |fields| matches!(fields[1], Field::Text(of) if of.eq_ignore_ascii_case(name))) {
        Ok(count) => db.commit(&mut |_, _, _| {}).map(|_| count > 0),
        Err(why) => {
            db.rollback();
            Err(why)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_kept_replaced_and_forgotten() {
        let mut db = Database::new();
        assert_eq!((get(&db, b"openrouter"), forget(&mut db, b"openrouter")), (None, Ok(false)));
        assert_eq!(set(&mut db, b"openrouter", b"sk-or-v1-0123456789abcdef"), Ok(false));
        assert_eq!(get(&db, b"OpenRouter").unwrap().bytes(), b"sk-or-v1-0123456789abcdef");
        assert_eq!(get(&db, b"other"), None);
        assert_eq!(set(&mut db, b"other", b"x"), Ok(false));
        // a second one under the same name takes its place
        assert_eq!(set(&mut db, b"OPENROUTER", b"another"), Ok(true));
        assert_eq!(get(&db, b"openrouter").unwrap().bytes(), b"another");
        let mut all = Vec::new();
        names(&db, &mut |name| all.push(name.to_vec()));
        all.sort();
        assert_eq!(all, [b"OPENROUTER".to_vec(), b"other".to_vec()]);
        assert_eq!((forget(&mut db, b"openrouter"), forget(&mut db, b"openrouter")), (Ok(true), Ok(false)));
        assert_eq!((get(&db, b"openrouter"), get(&db, b"other").unwrap().bytes()), (None, &b"x"[..]));
        // the longest key is kept whole
        let long = [b'k'; VALUE];
        assert_eq!(set(&mut db, b"long", &long), Ok(false));
        assert_eq!(get(&db, b"long").unwrap().bytes(), long);
    }

    #[test]
    fn a_key_holds_no_space_and_nothing_a_line_could_not() {
        assert!(good_value(b"sk-or-v1-0123456789abcdef") && good_value(&[b'k'; VALUE]) && good_value(b"a"));
        for bad in [&b""[..], b"two words", b"line\nbreak", b"caf\xe9", b"tab\there", &[b'k'; VALUE + 1]] {
            assert!(!good_value(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_table_of_that_name_that_is_not_ours_is_left_alone() {
        let mut db = Database::new();
        let made = db.create(TABLE, &[(b"what", Kind::Int)]).unwrap();
        db.insert(made, &[Field::Int(7)]).unwrap();
        db.commit(&mut |_, _, _| {}).unwrap();
        assert_eq!(set(&mut db, b"openrouter", b"secret"), Err(Error::BadName));
        assert_eq!((get(&db, b"openrouter"), forget(&mut db, b"openrouter")), (None, Ok(false)));
        assert_eq!(db.tables()[made].rows(), 1);
    }
}
