//! The disk of the database: which one it is, and what the database finds
//! on it.
//!
//! - A virtio disk, as QEMU gives one for development, is taken whole.
//! - On a SATA disk, as real machines have, only a partition marked for
//!   XilaOS in the partition table is used (`xila_core::gpt`). The rest of
//!   the disk is never written.
//!
//! Whatever is found, the database itself refuses a place that holds
//! something it does not know, and only starts a new database where
//! nothing was ever written.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt::{self, Write};

use xila_core::fat::Fat;
use xila_core::gpt::{self, Holds, Layout};
use xila_core::install::{self, Failed, Sectors};
use xila_core::shell::{DiskInfo, InstallError, Installed, System, Text};
use xila_core::text::FixedStr;
use xila_db::{Database, Disk, DiskError, Error, Page, Volume, PAGE};

use crate::ahci::{self, Sata};
use crate::arch;
use crate::pci;
use crate::virtio::{self, VirtioBlk};

/// Calls `done` again and again until it says yes. Gives up after this
/// many ticks of the timer and returns false.
pub fn wait_for(ticks: u64, mut done: impl FnMut() -> bool) -> bool {
    let start = arch::ticks();
    // in case the timer does not run: interrupts off
    for _ in 0..1u64 << 31 {
        if done() {
            return true;
        }
        if arch::ticks() - start > ticks {
            return false;
        }
        core::hint::spin_loop();
    }
    false
}

const SECTORS_PER_PAGE: u64 = (PAGE / gpt::SECTOR) as u64;

/// A partition of a SATA disk, in pages.
struct Partition {
    disk: Sata,
    first: u64,
    pages: u64,
}

impl Partition {
    fn sector(&self, page: u64) -> Result<u64, DiskError> {
        if page < self.pages {
            Ok(self.first + page * SECTORS_PER_PAGE)
        } else {
            Err(DiskError)
        }
    }
}

impl Disk for Partition {
    fn name(&self) -> &'static str {
        "sata"
    }

    fn pages(&self) -> u64 {
        self.pages
    }

    fn read(&mut self, page: u64, into: &mut Page) -> Result<(), DiskError> {
        self.disk.read(self.sector(page)?, into)
    }

    fn write(&mut self, page: u64, from: &Page) -> Result<(), DiskError> {
        self.disk.write(self.sector(page)?, from)
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        self.disk.flush()
    }
}

/// The SATA disks of the machine, in the order of their ports: the order
/// `install` numbers them in.
pub struct Disks {
    pub list: Vec<Sata>,
    /// The disk with the partition the database took, or refused and kept
    /// aside for `disk erase`.
    taken: Option<usize>,
}

impl Disks {
    pub const fn new() -> Disks {
        Disks { list: Vec::new(), taken: None }
    }
}

impl Sectors for Sata {
    fn read(&mut self, at: u64, into: &mut [u8]) -> bool {
        Sata::read(self, at, into).is_ok()
    }

    fn write(&mut self, at: u64, from: &[u8]) -> bool {
        Sata::write(self, at, from).is_ok()
    }

    fn flush(&mut self) -> bool {
        Sata::flush(self).is_ok()
    }
}

/// A size as people say it.
struct Size(u64);

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.0 < 10 << 30 {
            write!(f, "{} MB", self.0 >> 20)
        } else {
            write!(f, "{} GB", self.0 >> 30)
        }
    }
}

/// What to say of the disk and of the database once the machine has been
/// looked at.
pub struct Found {
    /// For `about` and the `machine` table.
    pub disk: Text,
    /// For the boot log: the same, with the name of the disk.
    pub log: FixedStr<56>,
    pub database: Text,
    /// A disk that was refused and that `disk erase` may take: it holds
    /// something else, or a database that is damaged.
    pub spare: Option<Volume>,
}

/// Opens the database a disk holds, in place of `db`. When that cannot
/// be, says why in place of what `found` says of the disk; `short` is how
/// to name the disk then.
fn attach(db: &mut Database, disk: Box<dyn Disk>, found: &mut Found, short: &[u8]) {
    match Database::open(Volume::single(disk)) {
        Ok(opened) => {
            let tables = opened.tables().len();
            if opened.fresh() {
                found.database.push_bytes(b"new, on the disk");
            } else {
                let _ = write!(found.database, "on the disk, {tables} table{}", if tables == 1 { "" } else { "s" });
            }
            *db = opened;
        }
        Err((error, volume)) => {
            // the disk is left as it is: it may hold something of value
            if matches!(error, Error::Foreign | Error::Damaged(_)) {
                found.spare = Some(volume);
            }
            let mut why = Text::from_bytes(short);
            let _ = match error {
                Error::Foreign => write!(why, ", holds something else: not used"),
                Error::TooSmall => write!(why, ", too small: not used"),
                Error::Damaged(page) => write!(why, ", damaged at page {page}: not used"),
                _ => write!(why, ", does not answer: not used"),
            };
            found.disk = why;
            found.log = FixedStr::from_bytes(why.bytes());
            found.database.push_bytes(IN_MEMORY);
        }
    }
}

const IN_MEMORY: &[u8] = b"in memory, lost when the machine stops";

/// Looks for a disk and opens the database it holds, in place of `db`,
/// which stays as it is when there is none. The SATA disks that were found
/// are left in `disks`.
pub fn open(db: &mut Database, disks: &mut Disks) -> Found {
    let mut development = None;
    pci::scan(&mut |function, id, class| {
        if id == virtio::ID && development.is_none() {
            development = VirtioBlk::start(function);
        } else if class == ahci::CLASS {
            ahci::start(function, &mut disks.list);
        }
    });

    let mut found = Found { disk: Text::new(), log: FixedStr::new(), database: Text::new(), spare: None };
    if let Some(disk) = development {
        let _ = write!(found.disk, "virtio, {}", Size(disk.pages() * PAGE as u64));
        found.log = FixedStr::from_bytes(found.disk.bytes());
        let short = found.disk;
        attach(db, Box::new(disk), &mut found, short.bytes());
        return found;
    }

    let count = disks.list.len();
    let first = disks.list.first().map_or(0, |disk| disk.sectors * gpt::SECTOR as u64);
    for index in 0..count {
        let disk = &mut disks.list[index];
        let sectors = disk.sectors;
        let Ok(partition) = gpt::find(sectors, &gpt::XILA, &mut |at, into| disk.read(at, into).is_ok()) else {
            continue;
        };
        let pages = partition.sectors / SECTORS_PER_PAGE;
        let size = Size(pages * PAGE as u64);
        let _ = write!(found.disk, "sata, partition of {size}");
        let mut model = FixedStr::<24>::new();
        model.push_bytes(disk.model.bytes());
        let _ = write!(found.log, "sata, {model}, partition of {size}");
        attach(db, Box::new(Partition { disk: disk.clone(), first: partition.first, pages }), &mut found, b"sata partition");
        disks.taken = Some(index);
        return found;
    }
    let _ = match count {
        0 => write!(found.disk, "none"),
        1 => write!(found.disk, "sata, {}, no XilaOS partition: not used", Size(first)),
        _ => write!(found.disk, "{count} sata disks, no XilaOS partition: not used"),
    };
    found.log = FixedStr::from_bytes(found.disk.bytes());
    found.database.push_bytes(IN_MEMORY);
    found
}

/// Tells the commands what disks there are and what each one holds.
pub fn look(disks: &mut Disks, sys: &mut System) {
    let taken = disks.taken;
    for (index, slot) in sys.disks.iter_mut().enumerate() {
        *slot = disks.list.get_mut(index).map(|disk| DiskInfo {
            model: disk.model,
            sectors: disk.sectors,
            holds: gpt::describe(disk.sectors, &mut |at, into| disk.read(at, into).is_ok()),
            taken: taken == Some(index),
        });
    }
}

/// Puts the system, `file`, on a disk numbered from 1: on the whole disk,
/// which is then where the tables are kept, or only in place of the system
/// the disk holds.
pub fn install(sys: &mut System, disks: &mut Disks, number: usize, whole: bool, file: &[u8]) -> Result<Installed, InstallError> {
    let index = number.wrapping_sub(1);
    let Some(disk) = disks.list.get_mut(index) else {
        return Err(InstallError::Disk(Failed::TooSmall));
    };
    let sectors = disk.sectors;
    // the partitions get identifiers that no other disk has
    let seed = arch::counter() ^ sys.ticks.rotate_left(40) ^ sectors;
    if !whole {
        let Holds::Partitions { system: Some((start, _)), .. } = gpt::describe(sectors, &mut |at, into| disk.read(at, into).is_ok()) else {
            return Err(InstallError::Disk(Failed::NoRoom));
        };
        install::system(disk, start, file, seed).map_err(InstallError::Disk)?;
        return Ok(Installed { disk: number, whole });
    }

    // A disk that cannot take it is refused before anything is given up.
    let layout = Layout::new(sectors, seed).ok_or(InstallError::Disk(Failed::TooSmall))?;
    Fat::new(layout.start, file.len(), 0).ok_or(InstallError::Disk(Failed::NoRoom))?;
    // What the database holds of this disk, or what `disk erase` would
    // take, is a partition that is about to be gone.
    if disks.taken == Some(index) {
        disks.taken = None;
        if sys.spare.take().is_none() {
            sys.db = Database::new();
            sys.db.set_limit(sys.machine.mem_usable_kb as usize * 1024 / 4);
            sys.triggers.changed();
        }
        sys.machine.disk = Text::from_bytes(b"sata, installation not complete: not used");
    }
    let layout = install::whole(disk, sectors, file, seed).map_err(InstallError::Disk)?;

    // The tables are kept on the new partition from now on.
    let pages = layout.data.sectors / SECTORS_PER_PAGE;
    let partition = Partition { disk: disk.clone(), first: layout.data.first, pages };
    match Database::open(Volume::single(Box::new(partition))) {
        Ok(opened) => {
            sys.db = opened;
            sys.spare = None;
            // the settings in use go with the tables to their new place
            sys.kept = None;
            disks.taken = Some(index);
            sys.machine.disk = Text::new();
            let _ = write!(sys.machine.disk, "sata, partition of {}", Size(pages * PAGE as u64));
            sys.triggers.changed();
            sys.stored_at = sys.ticks.max(1);
            Ok(Installed { disk: number, whole })
        }
        Err((error, _)) => Err(InstallError::Database(error)),
    }
}
