//! Indexes: from a value of a column to the rows that have it.
//!
//! An index is one more tree of the store. Those trees go from a number
//! to bytes, so a value is first turned into a number, its *mark*, which
//! keeps the order of the values:
//!
//! - a whole number or a decimal (in the units of its column): the number
//!   itself, moved so that the negative ones come first. The order of the
//!   marks is the order of the numbers: an index finds a span of them;
//! - a text: a number worked out from all of its bytes (FNV-1a). Texts
//!   that start alike, as addresses and paths do, have marks that have
//!   nothing in common; two different texts may have the same mark, but
//!   seldom. So an index finds the rows that have one text, not those
//!   between two texts, and what it gives is rows that *may* have the
//!   value: whoever asks looks at each row.
//!
//! A row without a value in the column is not in the index.
//!
//! Under a mark, the record says which rows have it: their ids, up to
//! `INLINE` of them in the record itself; beyond, the record names the
//! root of a tree of its own, whose keys are the ids. A column where
//! every row has its own value costs one small record per row; a column
//! where a thousand rows share a value costs one tree for that value.
//!
//! Every change goes through `tree::set` inside the open transaction of
//! the store, like the rows themselves: a row and its marks are committed
//! together or not at all.

use alloc::vec::Vec;

use crate::store::Store;
use crate::{tree, Error, Field};

/// Ids kept in the record of a mark before they get a tree of their own.
pub(crate) const INLINE: usize = 32;
const LIST: u8 = 1;
const TREE: u8 = 2;

/// The number an index knows a value by. None for no value.
pub fn mark(field: &Field) -> Option<u64> {
    match *field {
        Field::Null => None,
        Field::Int(n) | Field::Dec(n) => Some((n as u64) ^ (1 << 63)),
        Field::Text(text) => Some(text.iter().fold(0xcbf2_9ce4_8422_2325, |hash: u64, byte| (hash ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3))),
    }
}

/// What the record of a mark holds.
enum Rows {
    List(Vec<u64>),
    /// The root of the tree of the ids, and how many there are.
    Tree(u64, u64),
}

fn number(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]))
}

fn read(record: &[u8], page: u64) -> Result<Rows, Error> {
    match record.split_first() {
        Some((&LIST, ids)) if ids.len() % 8 == 0 && !ids.is_empty() && ids.len() <= INLINE * 8 => Ok(Rows::List(ids.chunks(8).map(number).collect())),
        Some((&TREE, rest)) if rest.len() == 16 => Ok(Rows::Tree(number(&rest[..8]), number(&rest[8..]))),
        _ => Err(Error::Damaged(page)),
    }
}

fn written(rows: &Rows) -> Vec<u8> {
    let mut record = Vec::new();
    match rows {
        Rows::List(ids) => {
            record.push(LIST);
            for id in ids {
                record.extend_from_slice(&id.to_le_bytes());
            }
        }
        Rows::Tree(root, count) => {
            record.push(TREE);
            record.extend_from_slice(&root.to_le_bytes());
            record.extend_from_slice(&count.to_le_bytes());
        }
    }
    record
}

/// The record of one mark, if the index has it.
fn get(store: &mut Store, root: u64, mark: u64) -> Result<Option<Rows>, Error> {
    let mut found = None;
    tree::range(store, root, mark, mark, &mut |_, record| found = Some(read(record, root)))?;
    found.transpose()
}

/// Says that a row has a value. Returns the new root of the index.
pub(crate) fn add(store: &mut Store, root: u64, mark: u64, id: u64) -> Result<u64, Error> {
    let rows = match get(store, root, mark)? {
        None => Rows::List([id].to_vec()),
        Some(Rows::List(mut ids)) => {
            match ids.binary_search(&id) {
                Ok(_) => return Ok(root),
                Err(at) => ids.insert(at, id),
            }
            if ids.len() <= INLINE {
                Rows::List(ids)
            } else {
                // too many for a record: they get a tree of their own
                let mut own = 0;
                for id in &ids {
                    own = tree::set(store, own, *id, Some(&[]))?.0;
                }
                Rows::Tree(own, ids.len() as u64)
            }
        }
        Some(Rows::Tree(own, count)) => {
            let (own, was) = tree::set(store, own, id, Some(&[]))?;
            Rows::Tree(own, count + u64::from(!was))
        }
    };
    Ok(tree::set(store, root, mark, Some(&written(&rows)))?.0)
}

/// Says that a row no longer has a value. Returns the new root.
pub(crate) fn remove(store: &mut Store, root: u64, mark: u64, id: u64) -> Result<u64, Error> {
    let rows = match get(store, root, mark)? {
        None => return Ok(root),
        Some(Rows::List(mut ids)) => {
            match ids.binary_search(&id) {
                Ok(at) => ids.remove(at),
                Err(_) => return Ok(root),
            };
            (!ids.is_empty()).then_some(Rows::List(ids))
        }
        Some(Rows::Tree(own, count)) => {
            let (own, was) = tree::set(store, own, id, None)?;
            let count = count - u64::from(was);
            // a tree stays a tree until it is empty: it may fill again
            (own != 0).then_some(Rows::Tree(own, count))
        }
    };
    Ok(tree::set(store, root, mark, rows.as_ref().map(written).as_deref())?.0)
}

/// Puts in `into` the ids of the rows whose mark is from `first` to
/// `last`, both included, in no order that matters. When there are more
/// than `most` it says false and `into` is of no use: so many rows are
/// read faster from the table itself, and the trees of ids are not even
/// opened to know it.
pub(crate) fn ids(store: &mut Store, root: u64, first: u64, last: u64, most: usize, into: &mut Vec<u64>) -> Result<bool, Error> {
    let mut own = Vec::new();
    let mut bad = false;
    let mut count = 0u64;
    tree::range(store, root, first, last, &mut |_, record| match read(record, root) {
        Ok(Rows::List(ids)) => {
            count += ids.len() as u64;
            if count <= most as u64 {
                into.extend_from_slice(&ids);
            }
        }
        Ok(Rows::Tree(root, held)) => {
            count += held;
            own.push(root);
        }
        Err(_) => bad = true,
    })?;
    if bad {
        return Err(Error::Damaged(root));
    }
    if count > most as u64 {
        return Ok(false);
    }
    for root in own {
        tree::walk(store, root, &mut |id, _| into.push(id))?;
    }
    Ok(true)
}

/// The roots of the trees of ids an index has, besides its own.
fn own(store: &mut Store, root: u64) -> Result<Vec<u64>, Error> {
    let mut own = Vec::new();
    let mut bad = false;
    tree::walk(store, root, &mut |_, record| match read(record, root) {
        Ok(Rows::Tree(root, _)) => own.push(root),
        Ok(Rows::List(_)) => {}
        Err(_) => bad = true,
    })?;
    if bad {
        return Err(Error::Damaged(root));
    }
    Ok(own)
}

/// Calls `each` with every page of an index: its tree, and the trees of
/// ids below it. Unlike the pages of a table this reads the leaves of the
/// index, which is where those trees are named.
pub(crate) fn pages(store: &mut Store, root: u64, each: &mut dyn FnMut(&mut Store, u64) -> Result<(), Error>) -> Result<(), Error> {
    tree::pages(store, root, each)?;
    for root in own(store, root)? {
        tree::pages(store, root, each)?;
    }
    Ok(())
}

/// Frees every page of an index.
pub(crate) fn release(store: &mut Store, root: u64) -> Result<(), Error> {
    for root in own(store, root)? {
        tree::release(store, root)?;
    }
    tree::release(store, root)
}

/// Reads a whole index and checks that it is well made. Calls `each` with
/// every page, and returns every (mark, id) it holds, in order.
pub(crate) fn check(store: &mut Store, root: u64, each: &mut dyn FnMut(u64) -> Result<(), Error>) -> Result<Vec<(u64, u64)>, Error> {
    tree::check(store, root, each)?;
    let mut records = Vec::new();
    tree::walk(store, root, &mut |mark, record| records.push((mark, read(record, root))))?;
    let mut all = Vec::new();
    for (mark, rows) in records {
        match rows? {
            Rows::List(ids) => {
                if !ids.windows(2).all(|pair| pair[0] < pair[1]) {
                    return Err(Error::Damaged(root));
                }
                all.extend(ids.iter().map(|id| (mark, *id)));
            }
            Rows::Tree(own, count) => {
                if tree::check(store, own, each)?.keys != count || count == 0 {
                    return Err(Error::Damaged(own));
                }
                tree::walk(store, own, &mut |id, _| all.push((mark, id)))?;
            }
        }
    }
    Ok(all)
}
