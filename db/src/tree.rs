//! A B-tree over the pages of a store: numbers as keys, bytes as values.
//!
//! A tree is known by the page of its root; 0 is the empty tree. Changing a
//! tree gives a new root, because the pages on the way down are written
//! again elsewhere (see `store`): whoever holds the root holds a state of
//! the whole tree.
//!
//! A page is a leaf, with keys and their values in order, or a branch, with
//! pages below it and the keys that separate them. Leaves are at level 0
//! and all at the same depth.
//!
//! ```text
//! every page   0  checksum (4)   4  kind (1)   5  level (1)   6  count (2)   8  transaction (8)
//! leaf         count times: key (8), length (2), value
//! branch       page (8), then count - 1 times: key (8), page (8)
//! ```
//!
//! Pages are not merged when rows go: a leaf is given back when it is
//! empty, and a root with a single page below it is dropped.

use alloc::vec;
use alloc::vec::Vec;

use crate::disk::{Page, PAGE};
use crate::store::Store;
use crate::Error;

const HEAD: usize = 16;
const BODY: usize = PAGE - HEAD;
const LEAF: u8 = 1;
const BRANCH: u8 = 2;
/// Longest value of a key. Four of them fit in a leaf.
pub(crate) const MAX_VALUE: usize = 1000;
/// Bytes of a leaf entry besides its value.
const ENTRY: usize = 10;
/// Pages below one branch.
const MAX_CHILDREN: usize = (BODY - 8) / 16 + 1;

pub(crate) enum Node {
    Leaf(Vec<(u64, Vec<u8>)>),
    /// Keys below `children[i]` are at least `keys[i - 1]` and less than
    /// `keys[i]`: there is one key less than there are children.
    Branch { level: u8, children: Vec<u64>, keys: Vec<u64> },
}

fn number(bytes: &[u8]) -> u64 {
    let mut eight = [0u8; 8];
    eight.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(eight)
}

fn ascending(keys: impl Iterator<Item = u64>) -> bool {
    let mut last = None;
    for key in keys {
        if last.is_some_and(|last| key <= last) {
            return false;
        }
        last = Some(key);
    }
    true
}

/// Reads a page as a node. None if it cannot be one.
fn decode(page: &Page) -> Option<Node> {
    let count = u16::from_le_bytes([page[6], page[7]]) as usize;
    let body = &page[HEAD..];
    match page[4] {
        LEAF if page[5] == 0 && count > 0 => {
            let mut entries = Vec::with_capacity(count);
            let mut at = 0;
            for _ in 0..count {
                let head = body.get(at..at + ENTRY)?;
                let length = u16::from_le_bytes([head[8], head[9]]) as usize;
                let value = body.get(at + ENTRY..at + ENTRY + length)?;
                entries.push((number(head), value.to_vec()));
                at += ENTRY + length;
            }
            ascending(entries.iter().map(|entry| entry.0)).then_some(Node::Leaf(entries))
        }
        BRANCH if page[5] > 0 && (1..=MAX_CHILDREN).contains(&count) => {
            let mut children = vec![number(body)];
            let mut keys = Vec::with_capacity(count - 1);
            for pair in body[8..].chunks_exact(16).take(count - 1) {
                keys.push(number(pair));
                children.push(number(&pair[8..]));
            }
            ascending(keys.iter().copied()).then_some(Node::Branch { level: page[5], children, keys })
        }
        _ => None,
    }
}

/// Writes a node into a page of zeros. The node must fit.
fn encode(node: &Node, page: &mut Page) {
    let (head, body) = page.split_at_mut(HEAD);
    match node {
        Node::Leaf(entries) => {
            head[4] = LEAF;
            head[6..8].copy_from_slice(&(entries.len() as u16).to_le_bytes());
            let mut at = 0;
            for (key, value) in entries {
                body[at..at + 8].copy_from_slice(&key.to_le_bytes());
                body[at + 8..at + ENTRY].copy_from_slice(&(value.len() as u16).to_le_bytes());
                body[at + ENTRY..at + ENTRY + value.len()].copy_from_slice(value);
                at += ENTRY + value.len();
            }
        }
        Node::Branch { level, children, keys } => {
            head[4] = BRANCH;
            head[5] = *level;
            head[6..8].copy_from_slice(&(children.len() as u16).to_le_bytes());
            body[..8].copy_from_slice(&children[0].to_le_bytes());
            for (i, (key, child)) in keys.iter().zip(&children[1..]).enumerate() {
                body[8 + i * 16..16 + i * 16].copy_from_slice(&key.to_le_bytes());
                body[16 + i * 16..24 + i * 16].copy_from_slice(&child.to_le_bytes());
            }
        }
    }
}

pub(crate) fn load(store: &mut Store, page: u64) -> Result<Node, Error> {
    store.read(page, decode)?.ok_or(Error::Damaged(page))
}

fn save(store: &mut Store, old: u64, node: &Node) -> Result<u64, Error> {
    store.write(old, |page| encode(node, page))
}

fn leaf_size(entries: &[(u64, Vec<u8>)]) -> usize {
    entries.iter().map(|entry| ENTRY + entry.1.len()).sum()
}

/// What became of a page after a change below it.
enum Grown {
    /// It is now this page; the same number when nothing had to move.
    Same(u64),
    /// It did not fit any more and is now two pages, with the smallest key
    /// of the second between them.
    Split(u64, u64, u64),
    /// Nothing is left in it and it was freed.
    Gone,
}

fn change(store: &mut Store, page: u64, key: u64, value: Option<&[u8]>, was: &mut bool) -> Result<Grown, Error> {
    match load(store, page)? {
        Node::Leaf(mut entries) => {
            let last = entries.last().is_none_or(|entry| key > entry.0);
            match (entries.binary_search_by_key(&key, |entry| entry.0), value) {
                (Ok(at), Some(value)) => {
                    *was = true;
                    entries[at].1 = value.to_vec();
                }
                (Ok(at), None) => {
                    *was = true;
                    entries.remove(at);
                }
                (Err(at), Some(value)) => entries.insert(at, (key, value.to_vec())),
                (Err(_), None) => return Ok(Grown::Same(page)),
            }
            if entries.is_empty() {
                store.free(page);
                return Ok(Grown::Gone);
            }
            if leaf_size(&entries) <= BODY {
                return Ok(Grown::Same(save(store, page, &Node::Leaf(entries))?));
            }
            // Keys mostly come in rising order: a key added at the end goes
            // alone to a new leaf and the full one stays full. Otherwise
            // the bytes are shared between the two.
            let mut cut = entries.len() - 1;
            if !last {
                let (mut size, half) = (0, leaf_size(&entries) / 2);
                let middle = entries.iter().position(|entry| {
                    size += ENTRY + entry.1.len();
                    size > half
                });
                cut = middle.unwrap_or(1).clamp(1, entries.len() - 1);
            }
            let right = entries.split_off(cut);
            let between = right[0].0;
            let left = save(store, page, &Node::Leaf(entries))?;
            Ok(Grown::Split(left, between, save(store, 0, &Node::Leaf(right))?))
        }
        Node::Branch { level, mut children, mut keys } => {
            let at = keys.partition_point(|k| *k <= key);
            match change(store, children[at], key, value, was)? {
                Grown::Same(child) if child == children[at] => return Ok(Grown::Same(page)),
                Grown::Same(child) => children[at] = child,
                Grown::Split(left, between, right) => {
                    children[at] = left;
                    children.insert(at + 1, right);
                    keys.insert(at, between);
                }
                Grown::Gone => {
                    children.remove(at);
                    if !keys.is_empty() {
                        keys.remove(at.saturating_sub(1));
                    }
                }
            }
            if children.is_empty() {
                store.free(page);
                return Ok(Grown::Gone);
            }
            if children.len() <= MAX_CHILDREN {
                return Ok(Grown::Same(save(store, page, &Node::Branch { level, children, keys })?));
            }
            // as for leaves: growth at the end leaves the full branch full
            let cut = if at + 2 == children.len() { children.len() - 1 } else { children.len() / 2 };
            let right_children = children.split_off(cut);
            let right_keys = keys.split_off(cut);
            let between = keys.pop().unwrap_or(0);
            let left = save(store, page, &Node::Branch { level, children, keys })?;
            let right = save(store, 0, &Node::Branch { level, children: right_children, keys: right_keys })?;
            Ok(Grown::Split(left, between, right))
        }
    }
}

/// Gives a key a value, or removes the key with `None`. Returns the new
/// root of the tree and whether the key was there.
pub(crate) fn set(store: &mut Store, root: u64, key: u64, value: Option<&[u8]>) -> Result<(u64, bool), Error> {
    if value.is_some_and(|value| value.len() > MAX_VALUE) {
        return Err(Error::TooLong);
    }
    if root == 0 {
        return match value {
            Some(value) => Ok((save(store, 0, &Node::Leaf(vec![(key, value.to_vec())]))?, false)),
            None => Ok((0, false)),
        };
    }
    let mut was = false;
    let mut root = match change(store, root, key, value, &mut was)? {
        Grown::Same(page) => page,
        Grown::Split(left, between, right) => {
            let level = match load(store, left)? {
                Node::Leaf(_) => 1,
                Node::Branch { level, .. } => level.checked_add(1).ok_or(Error::Damaged(left))?,
            };
            save(store, 0, &Node::Branch { level, children: vec![left, right], keys: vec![between] })?
        }
        Grown::Gone => 0,
    };
    // after a removal the top of the tree may have a single page below it
    while value.is_none() && root != 0 {
        match load(store, root)? {
            Node::Branch { children, .. } if children.len() == 1 => {
                store.free(root);
                root = children[0];
            }
            _ => break,
        }
    }
    Ok((root, was))
}

/// Calls `visit` with every key and its value, in the order of the keys.
pub(crate) fn walk(store: &mut Store, root: u64, visit: &mut dyn FnMut(u64, &[u8])) -> Result<(), Error> {
    range(store, root, 0, u64::MAX, visit)
}

/// Calls `visit` with the keys from `first` to `last`, both included, and
/// their values, in the order of the keys. Only the pages that can hold
/// such keys are read: one key costs one page for each level of the tree.
pub(crate) fn range(store: &mut Store, root: u64, first: u64, last: u64, visit: &mut dyn FnMut(u64, &[u8])) -> Result<(), Error> {
    if root == 0 || first > last {
        return Ok(());
    }
    match load(store, root)? {
        Node::Leaf(entries) => {
            for (key, value) in entries.iter().filter(|(key, _)| (first..=last).contains(key)) {
                visit(*key, value);
            }
        }
        Node::Branch { children, keys, .. } => {
            for (at, child) in children.iter().enumerate() {
                // what is below this page: from the key before it, to just before the next
                let from_here = at == 0 || keys[at - 1] <= last;
                let to_here = at + 1 == children.len() || keys[at] > first;
                if from_here && to_here {
                    range(store, *child, first, last, visit)?;
                }
            }
        }
    }
    Ok(())
}

/// Copies into `found` the keys from `first` to `last` of the first leaf
/// that has any, with their values, and nothing when no leaf has one.
/// Who reads a tree leaf by leaf, asking again from the key after the
/// last one it got, holds the store only while a leaf is read.
pub(crate) fn leaf(store: &mut Store, root: u64, first: u64, last: u64, found: &mut Vec<(u64, Vec<u8>)>) -> Result<(), Error> {
    if root == 0 || first > last {
        return Ok(());
    }
    match load(store, root)? {
        Node::Leaf(entries) => found.extend(entries.into_iter().filter(|(key, _)| (first..=last).contains(key))),
        Node::Branch { children, keys, .. } => {
            for (at, child) in children.iter().enumerate() {
                let from_here = at == 0 || keys[at - 1] <= last;
                let to_here = at + 1 == children.len() || keys[at] > first;
                if from_here && to_here {
                    leaf(store, *child, first, last, found)?;
                    if !found.is_empty() {
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Calls `each` with every page of a tree. Leaves are not read, except a
/// root that is one: finding the pages of a table costs a small part of
/// reading the table.
pub(crate) fn pages(store: &mut Store, root: u64, each: &mut dyn FnMut(&mut Store, u64) -> Result<(), Error>) -> Result<(), Error> {
    if root == 0 {
        return Ok(());
    }
    each(store, root)?;
    if let Node::Branch { level, children, .. } = load(store, root)? {
        for child in children {
            if level == 1 {
                each(store, child)?;
            } else {
                pages(store, child, each)?;
            }
        }
    }
    Ok(())
}

/// Frees every page of a tree.
pub(crate) fn release(store: &mut Store, root: u64) -> Result<(), Error> {
    let mut found = Vec::new();
    pages(store, root, &mut |_, page| {
        found.push(page);
        Ok(())
    })?;
    for page in found {
        store.free(page);
    }
    Ok(())
}

/// What `check` found in a tree.
pub(crate) struct Checked {
    pub(crate) keys: u64,
    pub(crate) last: Option<u64>,
}

/// Reads a whole tree and checks that it is well made: keys in order and
/// on the right side of the keys above them, leaves all at the same depth.
/// Calls `each` with every page.
pub(crate) fn check(store: &mut Store, root: u64, each: &mut dyn FnMut(u64) -> Result<(), Error>) -> Result<Checked, Error> {
    let mut checked = Checked { keys: 0, last: None };
    if root != 0 {
        check_page(store, root, None, (0, u64::MAX), each, &mut checked)?;
    }
    Ok(checked)
}

fn check_page(
    store: &mut Store,
    page: u64,
    level: Option<u8>,
    (low, high): (u64, u64),
    each: &mut dyn FnMut(u64) -> Result<(), Error>,
    checked: &mut Checked,
) -> Result<(), Error> {
    let bad = Err(Error::Damaged(page));
    each(page)?;
    match load(store, page)? {
        Node::Leaf(entries) => {
            if level.is_some_and(|level| level != 0) || leaf_size(&entries) > BODY {
                return bad;
            }
            for (key, value) in &entries {
                if *key < low || *key >= high || checked.last.is_some_and(|last| *key <= last) || value.len() > MAX_VALUE {
                    return bad;
                }
                checked.last = Some(*key);
                checked.keys += 1;
            }
        }
        Node::Branch { level: found, children, keys } => {
            if level.is_some_and(|level| level != found) || keys.len() + 1 != children.len() {
                return bad;
            }
            if keys.first().is_some_and(|key| *key < low) || keys.last().is_some_and(|key| *key >= high) {
                return bad;
            }
            for (i, child) in children.iter().enumerate() {
                let from = if i == 0 { low } else { keys[i - 1] };
                let to = keys.get(i).copied().unwrap_or(high);
                check_page(store, *child, Some(found - 1), (from, to), each, checked)?;
            }
        }
    }
    Ok(())
}
