//! Pages that are never overwritten while something still points at them.
//!
//! The store hands out pages of a volume and writes them in transactions:
//!
//! - A page that belongs to the last committed state is never written
//!   again. Changing it means writing a copy somewhere free; the old page
//!   becomes free once the transaction is committed.
//! - Pages 0 and 1 hold the header, which names the root of everything and
//!   the number of the transaction. A commit writes the new pages, waits
//!   until they are safe, then writes the header in the slot the previous
//!   commit did not use. That last write is the whole commit: before it the
//!   disk holds the old state, after it the new one.
//! - Opening reads both headers and takes the valid one with the highest
//!   number. There is nothing to replay.
//!
//! Which pages are free is not written anywhere: it is worked out when the
//! volume is opened, by following the trees (see `Database::open`). A page
//! nothing points at is free, so no crash can lose a page or free one that
//! is in use.
//!
//! Every page starts with the checksum of the rest of it, so a page the
//! disk damaged, or wrote only half of, is noticed when it is read.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use crate::disk::{Page, Volume, PAGE};
use crate::Error;

/// The first page that is not a header.
const FIRST: u64 = 2;
/// Smallest volume a database fits in.
pub(crate) const MIN_PAGES: u64 = 16;
/// Pages kept back for removing things: without them a full volume could
/// not even delete, since a delete writes copies before it frees anything.
const RESERVE: u64 = 32;
/// Pages read from a disk that are kept in memory.
const CACHE_PAGES: usize = 256;

const MAGIC: [u8; 8] = *b"XilaOSdb";
const VERSION: u32 = 1;

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x82f6_3b78 } else { crc >> 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC: [u32; 256] = crc_table();

/// CRC-32C of some bytes.
pub(crate) fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc = CRC[((crc ^ byte as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

/// Puts the checksum of a page in its first four bytes.
fn seal(page: &mut Page) {
    let sum = checksum(&page[4..]);
    page[..4].copy_from_slice(&sum.to_le_bytes());
}

fn sealed(page: &Page) -> bool {
    page[..4] == checksum(&page[4..]).to_le_bytes()
}

fn number(bytes: &[u8], at: usize) -> u64 {
    let mut eight = [0u8; 8];
    eight.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(eight)
}

/// What a header page says.
struct Header {
    txn: u64,
    pages: u64,
    catalog: u64,
    next_table: u64,
}

impl Header {
    fn page(&self) -> Box<Page> {
        let mut page = Box::new([0u8; PAGE]);
        page[4..12].copy_from_slice(&MAGIC);
        page[12..16].copy_from_slice(&VERSION.to_le_bytes());
        page[16..20].copy_from_slice(&(PAGE as u32).to_le_bytes());
        page[20..28].copy_from_slice(&self.txn.to_le_bytes());
        page[28..36].copy_from_slice(&self.pages.to_le_bytes());
        page[36..44].copy_from_slice(&self.catalog.to_le_bytes());
        page[44..52].copy_from_slice(&self.next_table.to_le_bytes());
        seal(&mut page);
        page
    }

    fn parse(page: &Page) -> Option<Header> {
        let good = sealed(page)
            && page[4..12] == MAGIC
            && page[12..16] == VERSION.to_le_bytes()
            && page[16..20] == (PAGE as u32).to_le_bytes();
        good.then(|| Header { txn: number(page, 20), pages: number(page, 28), catalog: number(page, 36), next_table: number(page, 44) })
    }
}

pub(crate) struct Store {
    volume: Volume,
    /// Pages of the volume the database may use.
    pages: u64,
    /// One bit per page: set when the committed state or the open
    /// transaction uses it.
    taken: Vec<u64>,
    taken_count: u64,
    /// No page below this one is free.
    hint: u64,
    /// The pages the open transaction wrote. They are not on the volume yet.
    dirty: BTreeMap<u64, Box<Page>>,
    /// Pages of the committed state the open transaction no longer uses.
    freed: Vec<u64>,
    cache: BTreeMap<u64, Box<Page>>,
    /// Number of the last committed transaction, and what its header says.
    txn: u64,
    catalog: u64,
    next_table: u64,
    /// A header could not be written: what the volume holds is not known
    /// for sure, so nothing more is written to it.
    stopped: bool,
    /// The change under way removes things and may use the reserve.
    pub(crate) spare: bool,
}

/// What `Store::open` found on a volume.
pub(crate) enum Found {
    Opened(Store),
    /// Nothing was ever written there.
    Blank(Volume),
}

impl Store {
    fn new(volume: Volume, header: &Header) -> Store {
        let mut store = Store {
            volume,
            pages: header.pages,
            taken: vec![0; header.pages.div_ceil(64) as usize],
            taken_count: 0,
            hint: FIRST,
            dirty: BTreeMap::new(),
            freed: Vec::new(),
            cache: BTreeMap::new(),
            txn: header.txn,
            catalog: header.catalog,
            next_table: header.next_table,
            stopped: false,
            spare: false,
        };
        for page in 0..FIRST {
            store.set(page);
        }
        store
    }

    /// Reads the headers of a volume. The pages in use are not known yet:
    /// the caller follows the trees and calls `mark` for each.
    ///
    /// A volume that cannot be opened is given back with the reason.
    pub(crate) fn open(mut volume: Volume) -> Result<Found, (Error, Volume)> {
        let mut best: Option<Header> = None;
        let (mut blank, mut readable) = (true, false);
        for slot in 0..FIRST {
            let mut page = Box::new([0u8; PAGE]);
            if volume.read(slot, &mut page).is_err() {
                blank = false;
                continue;
            }
            readable = true;
            blank &= page.iter().all(|byte| *byte == 0);
            if let Some(header) = Header::parse(&page) {
                if best.as_ref().is_none_or(|best| header.txn > best.txn) {
                    best = Some(header);
                }
            }
        }
        match best {
            Some(header) if header.pages < MIN_PAGES || header.pages > volume.pages() => Err((Error::Damaged(header.txn % 2), volume)),
            Some(header) => Ok(Found::Opened(Store::new(volume, &header))),
            None if !readable => Err((Error::Disk, volume)),
            None if blank => Ok(Found::Blank(volume)),
            None => Err((Error::Foreign, volume)),
        }
    }

    /// Starts an empty database on a volume, on its first `pages` pages.
    /// A volume that cannot take it is given back with the reason.
    pub(crate) fn format(mut volume: Volume, pages: u64) -> Result<Store, (Error, Volume)> {
        if pages < MIN_PAGES || pages > volume.pages() {
            return Err((Error::TooSmall, volume));
        }
        let header = Header { txn: 1, pages, catalog: 0, next_table: 1 };
        let write = |volume: &mut Volume, slot: u64, page: &Page| {
            volume.write(slot, page)?;
            volume.flush()
        };
        // whatever was in the other slot must not be taken for a header
        if write(&mut volume, 1, &header.page()).and_then(|_| write(&mut volume, 0, &[0u8; PAGE])).is_err() {
            return Err((Error::Disk, volume));
        }
        Ok(Store::new(volume, &header))
    }

    pub(crate) fn pages(&self) -> u64 {
        self.pages
    }

    /// Pages in use, the headers left out.
    pub(crate) fn used(&self) -> u64 {
        self.taken_count - FIRST
    }

    pub(crate) fn txn(&self) -> u64 {
        self.txn
    }

    pub(crate) fn catalog(&self) -> u64 {
        self.catalog
    }

    pub(crate) fn next_table(&self) -> u64 {
        self.next_table
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopped
    }

    pub(crate) fn volume(&self) -> &Volume {
        &self.volume
    }

    pub(crate) fn into_volume(self) -> Volume {
        self.volume
    }

    /// Whether the open transaction wrote or freed anything.
    pub(crate) fn changed(&self) -> bool {
        !self.dirty.is_empty() || !self.freed.is_empty()
    }

    fn is_taken(&self, page: u64) -> bool {
        self.taken[(page / 64) as usize] & (1 << (page % 64)) != 0
    }

    fn set(&mut self, page: u64) {
        self.taken[(page / 64) as usize] |= 1 << (page % 64);
        self.taken_count += 1;
    }

    fn release(&mut self, page: u64) {
        self.taken[(page / 64) as usize] &= !(1 << (page % 64));
        self.taken_count -= 1;
        self.hint = self.hint.min(page);
    }

    /// Says that the committed state uses a page. Fails if the page cannot
    /// be one, or if something else uses it already.
    pub(crate) fn mark(&mut self, page: u64) -> Result<(), Error> {
        if page < FIRST || page >= self.pages || self.is_taken(page) {
            return Err(Error::Damaged(page));
        }
        self.set(page);
        Ok(())
    }

    fn take(&mut self) -> Result<u64, Error> {
        if self.stopped {
            return Err(Error::Stopped);
        }
        let reserve = if self.spare { 0 } else { RESERVE.min(self.pages / 8) };
        if self.pages - self.taken_count <= reserve {
            return Err(Error::Full);
        }
        for index in (self.hint / 64) as usize..self.taken.len() {
            let word = self.taken[index];
            if word != u64::MAX {
                let page = index as u64 * 64 + (!word).trailing_zeros() as u64;
                if page >= self.pages {
                    break;
                }
                self.set(page);
                self.hint = page + 1;
                return Ok(page);
            }
        }
        Err(Error::Full)
    }

    fn keep(&mut self, page: u64, data: Box<Page>) {
        if !self.volume.keeps() {
            // the volume is memory already
            return;
        }
        if self.cache.len() >= CACHE_PAGES && !self.cache.contains_key(&page) {
            self.cache.pop_first();
        }
        self.cache.insert(page, data);
    }

    /// Gives a page to `with`: as the open transaction wrote it, or as the
    /// volume holds it.
    pub(crate) fn read<R>(&mut self, page: u64, with: impl FnOnce(&Page) -> R) -> Result<R, Error> {
        if page < FIRST || page >= self.pages {
            return Err(Error::Damaged(page));
        }
        if let Some(data) = self.dirty.get(&page).or_else(|| self.cache.get(&page)) {
            return Ok(with(data));
        }
        let mut data = Box::new([0u8; PAGE]);
        self.volume.read(page, &mut data).map_err(|_| Error::Disk)?;
        // a page written by a transaction that never committed is not part
        // of anything
        if !sealed(&data) || number(&data[..], 8) > self.txn {
            return Err(Error::Damaged(page));
        }
        let result = with(&data);
        self.keep(page, data);
        Ok(result)
    }

    /// Writes a page for the open transaction and returns where. `old` is
    /// the page this one replaces, or 0. When `old` was written by this
    /// same transaction it is written over; otherwise the new content goes
    /// to a free page and `old` is freed at the commit.
    ///
    /// `fill` gets a page of zeros and leaves the first 16 bytes alone: the
    /// store puts the checksum and the number of the transaction there.
    pub(crate) fn write(&mut self, old: u64, fill: impl FnOnce(&mut Page)) -> Result<u64, Error> {
        let page = if old != 0 && self.dirty.contains_key(&old) {
            old
        } else {
            let page = self.take()?;
            if old != 0 {
                self.freed.push(old);
            }
            page
        };
        let mut data = Box::new([0u8; PAGE]);
        fill(&mut data);
        data[8..16].copy_from_slice(&(self.txn + 1).to_le_bytes());
        seal(&mut data);
        self.dirty.insert(page, data);
        Ok(page)
    }

    /// Says that the open transaction no longer uses a page.
    pub(crate) fn free(&mut self, page: u64) {
        if self.dirty.remove(&page).is_some() {
            self.release(page);
        } else {
            self.freed.push(page);
        }
    }

    /// Makes the open transaction the committed state. After an error the
    /// caller must call `rollback`.
    pub(crate) fn commit(&mut self, catalog: u64, next_table: u64) -> Result<(), Error> {
        if !self.changed() && catalog == self.catalog && next_table == self.next_table {
            return Ok(());
        }
        if self.stopped {
            return Err(Error::Stopped);
        }
        // The new pages go where nothing of the committed state is, so
        // until the header is written a failure costs nothing.
        for (&page, data) in &self.dirty {
            self.volume.write(page, data).map_err(|_| Error::Disk)?;
        }
        self.volume.flush().map_err(|_| Error::Disk)?;
        let txn = self.txn + 1;
        let header = Header { txn, pages: self.pages, catalog, next_table }.page();
        if self.volume.write(txn % 2, &header).and_then(|_| self.volume.flush()).is_err() {
            // the header may or may not be there: writing anything more
            // could damage whichever state the next start will find
            self.stopped = true;
            return Err(Error::Disk);
        }
        (self.txn, self.catalog, self.next_table) = (txn, catalog, next_table);
        for page in core::mem::take(&mut self.freed) {
            self.release(page);
            self.cache.remove(&page);
            self.volume.discard(page);
        }
        for (page, data) in core::mem::take(&mut self.dirty) {
            self.keep(page, data);
        }
        self.spare = false;
        Ok(())
    }

    /// Forgets the open transaction.
    pub(crate) fn rollback(&mut self) {
        for (page, _) in core::mem::take(&mut self.dirty) {
            self.release(page);
        }
        self.freed.clear();
        self.spare = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_crc_32c() {
        assert_eq!(checksum(b"123456789"), 0xe306_9283);
        assert_ne!(checksum(&[0; PAGE - 4]), 0);
    }
}
