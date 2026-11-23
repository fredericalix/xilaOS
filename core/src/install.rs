//! Putting XilaOS on a disk, so that the machine starts from it.
//!
//! `whole` takes a whole disk: it writes a partition table with two
//! partitions, the start-up partition with XilaOS in it (`fat`), and
//! leaves the other one empty for the tables. Whatever the disk held is
//! lost. `system` only replaces XilaOS in the start-up partition of a disk
//! that was installed before: the tables stay as they are.
//!
//! Both read back what they wrote before they say it is done. A disk that
//! does not keep what it is given must not be found out at the next start.

use crate::fat::Fat;
use crate::gpt::{self, Holds, Layout, Partition, SECTOR};

/// Most sectors read or written at once.
const CHUNK: usize = 8;
/// Sectors emptied at the start of the partition of the tables: where the
/// storage engine looks to tell a new place from one that holds something.
const EMPTIED: u64 = 128;

/// A disk, by whole sectors: eight at most at a time. Each function says
/// whether it could.
pub trait Sectors {
    fn read(&mut self, at: u64, into: &mut [u8]) -> bool;
    fn write(&mut self, at: u64, from: &[u8]) -> bool;
    /// Returns once the disk has written everything it was given.
    fn flush(&mut self) -> bool;
}

/// Why an installation stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failed {
    /// The disk is too small for the two partitions. Nothing was written.
    TooSmall,
    /// The start-up partition has no room for the system. Nothing was written.
    NoRoom,
    /// The disk refused to write this sector, or to read it back.
    Write(u64),
    Read(u64),
    /// This sector does not hold what was written in it.
    Differs(u64),
}

/// Told how far an installation is, as it goes: sectors done, of how
/// many. The start-up partition is written, then read back: twice its
/// sectors in all. The rest is too little to count.
pub type Progress<'a> = &'a mut dyn FnMut(u64, u64);

/// Writes the start-up partition (`check` false), or compares what it
/// holds with what should have been written.
fn start_up(disk: &mut dyn Sectors, start: Partition, fat: &Fat, file: &[u8], check: bool, progress: Progress) -> Result<(), Failed> {
    let (mut wanted, mut held) = ([0u8; CHUNK * SECTOR], [0u8; CHUNK * SECTOR]);
    let mut at = 0;
    while at < fat.written() {
        let count = (fat.written() - at).min(CHUNK as u64) as usize;
        for (i, sector) in wanted[..count * SECTOR].chunks_exact_mut(SECTOR).enumerate() {
            fat.sector(at + i as u64, file, sector);
        }
        let (wanted, held) = (&wanted[..count * SECTOR], &mut held[..count * SECTOR]);
        if !check {
            if !disk.write(start.first + at, wanted) {
                return Err(Failed::Write(start.first + at));
            }
        } else if !disk.read(start.first + at, held) {
            return Err(Failed::Read(start.first + at));
        } else if let Some(byte) = (0..wanted.len()).find(|byte| wanted[*byte] != held[*byte]) {
            return Err(Failed::Differs(start.first + at + (byte / SECTOR) as u64));
        }
        at += count as u64;
        progress(at + if check { fat.written() } else { 0 }, 2 * fat.written());
    }
    Ok(())
}

/// Takes a whole disk of `sectors` sectors for XilaOS. `file` is the
/// system as the firmware starts it; `seed` is a number that differs from
/// one installation to the next. Returns where the partitions are.
pub fn whole(disk: &mut dyn Sectors, sectors: u64, file: &[u8], seed: u64, progress: Progress) -> Result<Layout, Failed> {
    let layout = Layout::new(sectors, seed).ok_or(Failed::TooSmall)?;
    let fat = Fat::new(layout.start, file.len(), (seed >> 32 ^ seed) as u32).ok_or(Failed::NoRoom)?;
    // From here on the disk is written. The place of the tables first: if
    // the disk was installed before, they are where the old ones were, and
    // must not be taken for them.
    let nothing = [0u8; CHUNK * SECTOR];
    for at in (layout.data.first..layout.data.first + EMPTIED.min(layout.data.sectors)).step_by(CHUNK) {
        if !disk.write(at, &nothing) {
            return Err(Failed::Write(at));
        }
    }
    layout.write(&mut |at, sector| disk.write(at, sector)).map_err(Failed::Write)?;
    start_up(disk, layout.start, &fat, file, false, progress)?;
    if !disk.flush() {
        return Err(Failed::Write(0));
    }

    // read back: the table, by the reading that will look for it at start
    let mut unread = None;
    let found = gpt::describe(sectors, &mut |at, into| {
        let read = disk.read(at, into);
        if !read {
            unread.get_or_insert(at);
        }
        read
    });
    if let Some(at) = unread {
        return Err(Failed::Read(at));
    }
    if found != (Holds::Partitions { count: 2, system: Some((layout.start, layout.data)) }) {
        return Err(Failed::Differs(1));
    }
    start_up(disk, layout.start, &fat, file, true, progress)?;
    Ok(layout)
}

/// Replaces the system in the start-up partition of a disk XilaOS was
/// installed on. Nothing else on the disk is written.
pub fn system(disk: &mut dyn Sectors, start: Partition, file: &[u8], seed: u64, progress: Progress) -> Result<(), Failed> {
    let fat = Fat::new(start, file.len(), (seed >> 32 ^ seed) as u32).ok_or(Failed::NoRoom)?;
    start_up(disk, start, &fat, file, false, progress)?;
    if !disk.flush() {
        return Err(Failed::Write(0));
    }
    start_up(disk, start, &fat, file, true, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fat::tests::{file, PATH};
    use std::collections::HashMap;

    /// A disk in memory: only the sectors that are not empty take room.
    #[derive(Default)]
    struct Ram {
        sectors: u64,
        held: HashMap<u64, [u8; SECTOR]>,
        writes: Vec<u64>,
        flushed: usize,
        /// A sector that cannot be written, one that cannot be read, and
        /// one that forgets what it is given.
        no_write: Option<u64>,
        no_read: Option<u64>,
        forgets: Option<u64>,
    }

    impl Ram {
        fn new(megabytes: u64) -> Ram {
            Ram { sectors: megabytes << 11, ..Ram::default() }
        }

        fn sector(&self, at: u64) -> [u8; SECTOR] {
            self.held.get(&at).copied().unwrap_or([0; SECTOR])
        }
    }

    impl Sectors for Ram {
        fn read(&mut self, at: u64, into: &mut [u8]) -> bool {
            assert!(into.len() % SECTOR == 0 && into.len() <= CHUNK * SECTOR && at + (into.len() / SECTOR) as u64 <= self.sectors);
            for (i, sector) in into.chunks_exact_mut(SECTOR).enumerate() {
                if self.no_read == Some(at + i as u64) {
                    return false;
                }
                sector.copy_from_slice(&self.sector(at + i as u64));
            }
            true
        }

        fn write(&mut self, at: u64, from: &[u8]) -> bool {
            assert!(from.len() % SECTOR == 0 && from.len() <= CHUNK * SECTOR && at + (from.len() / SECTOR) as u64 <= self.sectors);
            for (i, sector) in from.chunks_exact(SECTOR).enumerate() {
                let at = at + i as u64;
                if self.no_write == Some(at) {
                    return false;
                }
                self.writes.push(at);
                if self.forgets != Some(at) {
                    self.held.insert(at, sector.try_into().unwrap());
                }
            }
            true
        }

        fn flush(&mut self) -> bool {
            self.flushed += 1;
            true
        }
    }

    fn take(disk: &mut Ram, file: &[u8], seed: u64) -> Result<Layout, Failed> {
        let sectors = disk.sectors;
        whole(disk, sectors, file, seed, &mut |_, _| {})
    }

    fn xila() -> Vec<u8> {
        (0..600_123).map(|i| (i * 31 + i / 700) as u8).collect()
    }

    fn started_from(disk: &Ram, start: Partition) -> Option<Vec<u8>> {
        file(&mut |at| disk.sector(start.first + at), &PATH)
    }

    #[test]
    fn a_whole_disk_is_taken() {
        let mut disk = Ram::new(500);
        // what the disk held: another system's table, and things all over
        for at in (0..disk.sectors).step_by(97) {
            disk.held.insert(at, [0x5a; SECTOR]);
        }
        let system = xila();
        let layout = take(&mut disk, &system, 42).unwrap();
        assert_eq!(layout.data, Partition { first: 101 << 11, sectors: 398 << 11 });
        assert!(disk.flushed > 0);
        // the firmware finds XilaOS, and XilaOS finds the place of its tables
        assert!(started_from(&disk, layout.start).as_deref() == Some(&system[..]));
        let mut read = |at: u64, into: &mut [u8; SECTOR]| {
            *into = disk.sector(at);
            true
        };
        assert_eq!(gpt::find(disk.sectors, &gpt::XILA, &mut read), Ok(layout.data));
        // which looks like a place nothing was ever written on
        assert!((0..EMPTIED).all(|at| disk.sector(layout.data.first + at) == [0; SECTOR]));
        // nothing was written but the tables at both ends, the start-up
        // partition as far as the file goes, and the start of the other one
        let fat = Fat::new(layout.start, system.len(), 1).unwrap();
        let expected = |at: u64| {
            at < 34
                || at >= disk.sectors - 33
                || (layout.start.first..layout.start.first + fat.written()).contains(&at)
                || (layout.data.first..layout.data.first + EMPTIED).contains(&at)
        };
        assert!(disk.writes.iter().all(|at| expected(*at)));
    }

    #[test]
    fn an_installation_tells_how_far_it_is() {
        let system = xila();
        let mut disk = Ram::new(500);
        let sectors = disk.sectors;
        let mut told = Vec::new();
        let layout = whole(&mut disk, sectors, &system, 42, &mut |done, total| told.push((done, total))).unwrap();
        // written, then read back: twice the sectors of the start-up partition that hold something
        let written = Fat::new(layout.start, system.len(), 1).unwrap().written();
        assert!(told.iter().all(|(_, total)| *total == 2 * written));
        assert!(told.windows(2).all(|pair| pair[0].0 < pair[1].0 && pair[1].0 - pair[0].0 <= CHUNK as u64));
        assert_eq!((told[0].0, told[told.len() - 1].0), (CHUNK as u64, 2 * written));
        // the system alone: the same road
        let mut again = Vec::new();
        assert_eq!(super::system(&mut disk, layout.start, &system, 43, &mut |done, total| again.push((done, total))), Ok(()));
        assert_eq!(again, told);
        // a disk that fails stops it where it is
        let mut failing = Ram { no_write: Some(layout.start.first + 80), ..Ram::new(500) };
        let mut last = 0;
        assert!(whole(&mut failing, sectors, &system, 42, &mut |done, _| last = done).is_err());
        assert_eq!(last, 80);
    }

    #[test]
    fn a_disk_that_cannot_take_it_is_left_alone() {
        let mut small = Ram::new(64);
        assert_eq!(take(&mut small, &xila(), 1), Err(Failed::TooSmall));
        let mut disk = Ram::new(500);
        assert_eq!(take(&mut disk, &vec![0; 101 << 20], 1), Err(Failed::NoRoom));
        assert_eq!(take(&mut disk, &[], 1), Err(Failed::NoRoom));
        assert!(small.writes.is_empty() && disk.writes.is_empty());
    }

    #[test]
    fn a_disk_that_fails_is_found_out() {
        let system = xila();
        let layout = Layout::new(500 << 11, 1).unwrap();
        let file_at = layout.start.first + Fat::new(layout.start, system.len(), 1).unwrap().written() - 3;
        for at in [0, 1, 2, (500 << 11) - 1, layout.start.first, file_at, layout.data.first] {
            let mut disk = Ram { no_write: Some(at), ..Ram::new(500) };
            let failed = take(&mut disk, &system, 1);
            assert!(matches!(failed, Err(Failed::Write(first)) if first <= at && at < first + CHUNK as u64), "sector {at}: {failed:?}");
        }
        for at in [1, 2, layout.start.first, file_at] {
            let mut disk = Ram { no_read: Some(at), ..Ram::new(500) };
            let failed = take(&mut disk, &system, 1);
            assert!(matches!(failed, Err(Failed::Read(first)) if first <= at && at < first + CHUNK as u64), "sector {at}: {failed:?}");
        }
        // a sector that says it was written and was not
        for at in [1, 2, layout.start.first, layout.start.first + 33, file_at] {
            let mut disk = Ram { forgets: Some(at), ..Ram::new(500) };
            disk.held.insert(at, [0x5a; SECTOR]);
            let failed = take(&mut disk, &system, 1);
            assert!(matches!(failed, Err(Failed::Differs(_))), "sector {at}: {failed:?}");
        }
    }

    #[test]
    fn the_system_alone_is_replaced() {
        let mut disk = Ram::new(500);
        let layout = take(&mut disk, &xila(), 42).unwrap();
        // the tables, and the time that passes
        disk.held.insert(layout.data.first, [7; SECTOR]);
        disk.writes.clear();
        let newer: Vec<u8> = xila().iter().rev().copied().chain([1, 2, 3]).collect();
        assert_eq!(system(&mut disk, layout.start, &newer, 43, &mut |_, _| {}), Ok(()));
        assert!(started_from(&disk, layout.start).as_deref() == Some(&newer[..]));
        // nothing was written outside the start-up partition
        assert!(disk.writes.iter().all(|at| (layout.start.first..layout.start.first + layout.start.sectors).contains(at)));
        assert_eq!(disk.sector(layout.data.first), [7; SECTOR]);
        // a shorter system leaves nothing of the longer one in the file
        assert_eq!(system(&mut disk, layout.start, &[9; 700], 44, &mut |_, _| {}), Ok(()));
        assert!(started_from(&disk, layout.start).as_deref() == Some(&[9; 700][..]));
        let mut failing = Ram { forgets: Some(layout.start.first), ..Ram::new(500) };
        assert_eq!(system(&mut failing, layout.start, &xila(), 1, &mut |_, _| {}), Err(Failed::Differs(layout.start.first)));
    }
}
