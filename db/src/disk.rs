//! Where the pages of the database are kept.
//!
//! The engine reads and writes whole pages on a `Volume`. A volume is made
//! of disks; for now of exactly one. It is the place where a mirror over two
//! disks will go: the engine above only knows page numbers, never which disk
//! holds them.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;

/// Size of a page in bytes.
pub const PAGE: usize = 4096;
pub type Page = [u8; PAGE];

/// The device could not read or write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DiskError;

/// Something that keeps pages: a disk of the machine, or memory.
pub trait Disk {
    /// A short name for the user: `virtio`, `memory`.
    fn name(&self) -> &'static str;
    fn pages(&self) -> u64;
    /// Whether what is written is still there after the machine stops.
    fn keeps(&self) -> bool {
        true
    }
    fn read(&mut self, page: u64, into: &mut Page) -> Result<(), DiskError>;
    fn write(&mut self, page: u64, from: &Page) -> Result<(), DiskError>;
    /// Returns once everything written so far is safe from a power cut.
    fn flush(&mut self) -> Result<(), DiskError>;
    /// Says that a page holds nothing of use any more.
    fn discard(&mut self, _page: u64) {}
}

/// Pages kept in memory, for a machine without a disk and for tests. Only
/// the pages that were written take room.
pub struct Memory {
    pages: u64,
    held: BTreeMap<u64, Box<Page>>,
}

impl Memory {
    pub const fn new(pages: u64) -> Memory {
        Memory { pages, held: BTreeMap::new() }
    }
}

impl Disk for Memory {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn pages(&self) -> u64 {
        self.pages
    }

    fn keeps(&self) -> bool {
        false
    }

    fn read(&mut self, page: u64, into: &mut Page) -> Result<(), DiskError> {
        match self.held.get(&page) {
            Some(held) => into.copy_from_slice(&held[..]),
            None => into.fill(0),
        }
        Ok(())
    }

    fn write(&mut self, page: u64, from: &Page) -> Result<(), DiskError> {
        match self.held.get_mut(&page) {
            Some(held) => held.copy_from_slice(from),
            None => {
                self.held.insert(page, Box::new(*from));
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        Ok(())
    }

    fn discard(&mut self, page: u64) {
        self.held.remove(&page);
    }
}

/// The pages of the database, whatever holds them.
pub struct Volume {
    disk: Box<dyn Disk>,
}

impl Volume {
    /// A volume on one disk.
    pub fn single(disk: Box<dyn Disk>) -> Volume {
        Volume { disk }
    }

    pub fn name(&self) -> &'static str {
        self.disk.name()
    }

    pub fn pages(&self) -> u64 {
        self.disk.pages()
    }

    pub fn keeps(&self) -> bool {
        self.disk.keeps()
    }

    pub fn read(&mut self, page: u64, into: &mut Page) -> Result<(), DiskError> {
        self.disk.read(page, into)
    }

    pub fn write(&mut self, page: u64, from: &Page) -> Result<(), DiskError> {
        self.disk.write(page, from)
    }

    pub fn flush(&mut self) -> Result<(), DiskError> {
        self.disk.flush()
    }

    pub fn discard(&mut self, page: u64) {
        self.disk.discard(page)
    }
}
