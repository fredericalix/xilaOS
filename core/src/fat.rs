//! The start-up partition: what the firmware of a PC reads to start XilaOS.
//!
//! A firmware (UEFI) starts a system from a file, `EFI\BOOT\BOOTX64.EFI`,
//! that it looks for in a partition laid out as FAT32. XilaOS keeps nothing
//! in files, but it has to be started: this module makes that partition,
//! with that one file in it, which is XilaOS itself.
//!
//! It only makes it, whole and new each time. Nothing here reads such a
//! partition or changes one: XilaOS never looks at it again.
//!
//! The partition is not built in memory. `Fat::sector` tells what any of
//! its sectors holds, so that whoever writes it needs no more room than
//! one sector.

use crate::gpt::{Partition, SECTOR};

/// Sectors before the tables: the description of the partition and its copy.
const RESERVED: u32 = 32;
/// Entries of a table in one sector: four bytes each.
const PER_SECTOR: u32 = (SECTOR / 4) as u32;
/// With fewer clusters than this a firmware reads the tables as FAT16.
const LEAST_CLUSTERS: u32 = 65_525;
const MOST_CLUSTERS: u32 = 0x0fff_fff4;
/// What a table says of the last cluster of a file.
const LAST: u32 = 0x0fff_ffff;

// The clusters, of one sector each: three directories, then the file.
const ROOT: u32 = 2;
const EFI: u32 = 3;
const BOOT: u32 = 4;
const FILE: u32 = 5;

const DIRECTORY: u8 = 0x10;
const ARCHIVE: u8 = 0x20;
const LABEL: u8 = 0x08;
/// The day everything in the partition is dated: 1 January 2026. XilaOS
/// does not know the date yet.
const DATE: u16 = (2026 - 1980) << 9 | 1 << 5 | 1;

/// A start-up partition to be written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fat {
    /// Where the partition starts on the disk, and its size.
    first: u32,
    sectors: u32,
    /// Sectors of one table; there are two.
    table: u32,
    /// Bytes and clusters of the file.
    file: u32,
    clusters: u32,
    /// A number that tells this partition from another one.
    serial: u32,
}

impl Fat {
    /// The layout of a partition that holds a file of `file` bytes. None
    /// if the partition cannot be laid out as FAT32, or is too small for
    /// the file.
    pub fn new(partition: Partition, file: usize, serial: u32) -> Option<Fat> {
        let first = u32::try_from(partition.first).ok()?;
        let sectors = u32::try_from(partition.sectors).ok()?;
        let file = u32::try_from(file).ok().filter(|bytes| *bytes > 0)?;
        // each cluster needs its sector and its entry in both tables
        let table = sectors.checked_sub(RESERVED - 2)?.div_ceil(PER_SECTOR + 2);
        let all = sectors.checked_sub(RESERVED + 2 * table)?;
        let clusters = file.div_ceil(SECTOR as u32);
        if !(LEAST_CLUSTERS..=MOST_CLUSTERS).contains(&all) || clusters > all - (FILE - ROOT) {
            return None;
        }
        Some(Fat { first, sectors, table, file, clusters, serial })
    }

    /// How many sectors, from the first of the partition, must be written.
    /// The others are clusters no file uses: what they hold does not matter.
    pub fn written(&self) -> u64 {
        (self.data() + (FILE - ROOT) + self.clusters) as u64
    }

    /// The sector of cluster 2, the first one.
    fn data(&self) -> u32 {
        RESERVED + 2 * self.table
    }

    /// Fills `out`, one sector, with what sector `at` of the partition
    /// holds. `file` is the file: as many bytes as `new` was told.
    pub fn sector(&self, at: u64, file: &[u8], out: &mut [u8]) {
        assert!(out.len() == SECTOR && file.len() == self.file as usize);
        out.fill(0);
        let Ok(at) = u32::try_from(at) else {
            return;
        };
        let data = self.data();
        match at {
            // the description of the partition, its notes, and a copy of both
            0 | 6 => self.description(out),
            1 | 7 => self.notes(out),
            _ if at < RESERVED => {}
            _ if at < data => {
                let first = (at - RESERVED) % self.table * PER_SECTOR;
                for (cluster, entry) in (first..).zip(out.chunks_exact_mut(4)) {
                    entry.copy_from_slice(&self.next(cluster).to_le_bytes());
                }
            }
            _ => match at - data + ROOT {
                ROOT => {
                    entry(&mut out[..32], b"XILAOS     ", LABEL, 0, 0);
                    entry(&mut out[32..64], b"EFI        ", DIRECTORY, EFI, 0);
                }
                EFI => {
                    // in a directory: itself, the one above (0 is the first one), and what it holds
                    entry(&mut out[..32], b".          ", DIRECTORY, EFI, 0);
                    entry(&mut out[32..64], b"..         ", DIRECTORY, 0, 0);
                    entry(&mut out[64..96], b"BOOT       ", DIRECTORY, BOOT, 0);
                }
                BOOT => {
                    entry(&mut out[..32], b".          ", DIRECTORY, BOOT, 0);
                    entry(&mut out[32..64], b"..         ", DIRECTORY, EFI, 0);
                    entry(&mut out[64..96], b"BOOTX64 EFI", ARCHIVE, FILE, self.file);
                }
                cluster if cluster < FILE + self.clusters => {
                    let from = (cluster - FILE) as usize * SECTOR;
                    let bytes = &file[from..file.len().min(from + SECTOR)];
                    out[..bytes.len()].copy_from_slice(bytes);
                }
                _ => {}
            },
        }
    }

    /// What the tables say of a cluster: the cluster that follows it in its
    /// file, or that it is the last one, or 0 if it is free.
    fn next(&self, cluster: u32) -> u32 {
        let end = FILE + self.clusters;
        match cluster {
            // the first two entries stand for no cluster
            0 => 0x0fff_fff8,
            1 => LAST,
            ROOT | EFI | BOOT => LAST,
            _ if cluster + 1 < end => cluster + 1,
            _ if cluster + 1 == end => LAST,
            _ => 0,
        }
    }

    fn description(&self, out: &mut [u8]) {
        // a jump over the description, to a loop: nothing starts from here
        out[..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
        out[0x5a..0x5c].copy_from_slice(&[0xeb, 0xfe]);
        out[3..11].copy_from_slice(b"XILAOS  ");
        out[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
        // sectors in a cluster
        out[13] = 1;
        out[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
        // tables
        out[16] = 2;
        // a disk that stays in its machine
        out[21] = 0xf8;
        // the shape disks said they had, which tools still expect to find
        out[24..26].copy_from_slice(&63u16.to_le_bytes());
        out[26..28].copy_from_slice(&255u16.to_le_bytes());
        out[28..32].copy_from_slice(&self.first.to_le_bytes());
        out[32..36].copy_from_slice(&self.sectors.to_le_bytes());
        out[36..40].copy_from_slice(&self.table.to_le_bytes());
        out[44..48].copy_from_slice(&ROOT.to_le_bytes());
        // where the notes are, and the copy of this sector
        out[48..50].copy_from_slice(&1u16.to_le_bytes());
        out[50..52].copy_from_slice(&6u16.to_le_bytes());
        out[64] = 0x80;
        out[66] = 0x29;
        out[67..71].copy_from_slice(&self.serial.to_le_bytes());
        out[71..82].copy_from_slice(b"XILAOS     ");
        out[82..90].copy_from_slice(b"FAT32   ");
        out[510..].copy_from_slice(&[0x55, 0xaa]);
    }

    /// The sector that says how many clusters are free and where to look
    /// for one.
    fn notes(&self, out: &mut [u8]) {
        let all = self.sectors - self.data();
        let used = (FILE - ROOT) + self.clusters;
        out[..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
        out[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
        out[488..492].copy_from_slice(&(all - used).to_le_bytes());
        out[492..496].copy_from_slice(&(ROOT + used).to_le_bytes());
        out[508..].copy_from_slice(&0xaa55_0000u32.to_le_bytes());
    }
}

/// Writes the entry of a directory: a name of eight characters and three,
/// what it is, its first cluster and its size.
fn entry(out: &mut [u8], name: &[u8; 11], kind: u8, cluster: u32, bytes: u32) {
    out[..11].copy_from_slice(name);
    out[11] = kind;
    // made, read and written on the same day, at midnight
    for date in [16, 18, 24] {
        out[date..date + 2].copy_from_slice(&DATE.to_le_bytes());
    }
    out[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    out[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    out[28..32].copy_from_slice(&bytes.to_le_bytes());
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn u16_at(bytes: &[u8], at: usize) -> u32 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]]) as u32
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    /// Reads a file of a FAT32 partition the way a firmware does, from the
    /// description in its first sector on: nothing here knows how `Fat`
    /// lays things out. `read` gives a sector of the partition.
    pub(crate) fn file(read: &mut dyn FnMut(u64) -> [u8; SECTOR], path: &[&[u8; 11]]) -> Option<Vec<u8>> {
        let boot = read(0);
        if boot[510..] != [0x55, 0xaa] || u16_at(&boot, 11) as usize != SECTOR || &boot[82..90] != b"FAT32   " {
            return None;
        }
        let (per_cluster, reserved, tables) = (boot[13] as u32, u16_at(&boot, 14), boot[16] as u32);
        let (sectors, table) = (u32_at(&boot, 32), u32_at(&boot, 36));
        let data = reserved + tables * table;
        // what makes it FAT32 for a firmware: the number of clusters
        if (sectors - data) / per_cluster < 65_525 {
            return None;
        }
        // all the clusters of what starts at a cluster, following a table
        let mut chain = |mut cluster: u32| {
            let mut bytes = Vec::new();
            while (2..0x0fff_fff8).contains(&cluster) {
                for sector in 0..per_cluster {
                    bytes.extend_from_slice(&read((data + (cluster - 2) * per_cluster + sector) as u64));
                }
                let entries = read((reserved + cluster / PER_SECTOR) as u64);
                cluster = u32_at(&entries, (cluster % PER_SECTOR) as usize * 4) & 0x0fff_ffff;
                assert!(bytes.len() < 64 << 20, "a chain that does not end");
            }
            bytes
        };
        let mut here = chain(u32_at(&boot, 44));
        for (depth, name) in path.iter().enumerate() {
            let found = here.chunks_exact(32).take_while(|entry| entry[0] != 0).find(|entry| entry[..11] == name[..])?;
            let (cluster, bytes) = (u16_at(found, 20) << 16 | u16_at(found, 26), u32_at(found, 28) as usize);
            let last = depth + 1 == path.len();
            if (found[11] & DIRECTORY != 0) == last {
                return None;
            }
            here = chain(cluster);
            if last {
                here.truncate(bytes);
            }
        }
        Some(here)
    }

    pub(crate) const PATH: [&[u8; 11]; 3] = [b"EFI        ", b"BOOT       ", b"BOOTX64 EFI"];

    fn partition() -> Partition {
        Partition { first: 2048, sectors: 100 << 11 }
    }

    fn sector(fat: &Fat, at: u64, file: &[u8]) -> [u8; SECTOR] {
        let mut out = [0xaa; SECTOR];
        fat.sector(at, file, &mut out);
        out
    }

    #[test]
    fn the_firmware_finds_the_file() {
        // a file that does not end on a sector, and one that does
        for length in [700_001, 1024, 1] {
            let system: Vec<u8> = (0..length).map(|i| (i * 7 + i / 512) as u8).collect();
            let fat = Fat::new(partition(), system.len(), 0x1234_5678).unwrap();
            let found = file(&mut |at| sector(&fat, at, &system), &PATH);
            assert!(found.as_deref() == Some(&system[..]), "a file of {length} bytes");
            // and nothing else is there
            assert_eq!(file(&mut |at| sector(&fat, at, &system), &[b"EFI        ", b"BOOTX64 EFI"]), None);
        }
    }

    #[test]
    fn the_partition_is_laid_out_as_tools_expect() {
        let system = vec![1u8; 3000];
        let fat = Fat::new(partition(), system.len(), 0x1234_5678).unwrap();
        let get = |at: u64| sector(&fat, at, &system);
        let boot = get(0);
        assert_eq!((u32_at(&boot, 28), u32_at(&boot, 32)), (2048, 100 << 11));
        // the tables have room for every cluster, and not a sector more
        let (table, data) = (fat.table, fat.data());
        let clusters = (100 << 11) - data;
        assert!(table * PER_SECTOR >= clusters + 2 && (table - 1) * PER_SECTOR < clusters + 2);
        assert!(clusters >= LEAST_CLUSTERS);
        // the copies: of the description, and of the table
        assert_eq!(get(6), get(0));
        assert_eq!(get(7), get(1));
        for at in 0..table as u64 {
            assert_eq!(get(RESERVED as u64 + at), get((RESERVED + table) as u64 + at));
        }
        // the first table: two entries for no cluster, three directories, six clusters of file
        let first = get(RESERVED as u64);
        let entries: Vec<u32> = first.chunks_exact(4).map(|e| u32_at(e, 0)).take(12).collect();
        assert_eq!(entries, [0x0fff_fff8, LAST, LAST, LAST, LAST, 6, 7, 8, 9, 10, LAST, 0]);
        // free clusters, and where the first one is
        let notes = get(1);
        assert_eq!((u32_at(&notes, 488), u32_at(&notes, 492)), (clusters - 9, 11));
        assert_eq!((u32_at(&notes, 0), u32_at(&notes, 484), u32_at(&notes, 508)), (0x4161_5252, 0x6141_7272, 0xaa55_0000));
        // what must be written stops with the file; beyond, nothing
        assert_eq!(fat.written(), (data + 9) as u64);
        assert_eq!(get(fat.written() - 1)[..3000 - 5 * SECTOR], [1u8; 3000 - 5 * SECTOR]);
        assert_eq!(get(fat.written() - 1)[3000 - 5 * SECTOR..], [0u8; 6 * SECTOR - 3000]);
        assert_eq!(get(fat.written()), [0; SECTOR]);
        assert_eq!(get(u64::MAX), [0; SECTOR]);
    }

    #[test]
    fn a_partition_that_cannot_hold_it_is_refused() {
        let room = |megabytes: u64, file: usize| Fat::new(Partition { first: 2048, sectors: megabytes << 11 }, file, 1).is_some();
        assert!(room(100, 600_000));
        // too small to be FAT32 at all
        assert!(!room(32, 600_000));
        assert!(room(33, 600_000));
        // no room for the file, or no file
        assert!(!room(100, 101 << 20));
        assert!(!room(100, 0));
        assert_eq!(Fat::new(Partition { first: 1 << 40, sectors: 100 << 11 }, 1, 1), None);
        assert_eq!(Fat::new(Partition { first: 0, sectors: 0 }, 1, 1), None);
    }
}
