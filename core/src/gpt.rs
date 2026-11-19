//! Finding the partition of XilaOS on a disk.
//!
//! A disk of a real machine holds other systems and their data. XilaOS
//! never takes a whole disk: it takes one partition, marked for it in the
//! partition table (GPT) by its type. Everything else on the disk is left
//! alone, whatever it looks like.
//!
//! The table is trusted only when it is whole: the right signature, and
//! the checksums of its header and of its entries. The copy at the end of
//! the disk is not used; a disk whose first table is damaged is not used
//! either.

/// Bytes in a sector, the unit a partition table counts in.
pub const SECTOR: usize = 512;

/// The type of a partition that holds a XilaOS database, as tools write
/// it: `04DA9601-6187-4182-88FE-27F226C8DEE9`.
pub const XILA: [u8; 16] = guid(b"04DA9601-6187-4182-88FE-27F226C8DEE9");

const fn hex(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        b'A'..=b'F' => digit - b'A' + 10,
        _ => panic!("not a hexadecimal digit"),
    }
}

/// An identifier as it is on disk, from its text. The first three groups
/// are stored with their bytes in the reverse order.
pub const fn guid(text: &[u8; 36]) -> [u8; 16] {
    // where each byte of the result is in the text
    const AT: [usize; 16] = [6, 4, 2, 0, 11, 9, 16, 14, 19, 21, 24, 26, 28, 30, 32, 34];
    let mut bytes = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        bytes[i] = hex(text[AT[i]]) << 4 | hex(text[AT[i] + 1]);
        i += 1;
    }
    bytes
}

/// CRC-32 as the partition table uses it, continued from `crc` (start with 0).
pub fn crc32(crc: u32, bytes: &[u8]) -> u32 {
    let mut crc = !crc;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u32_at(bytes, at) as u64 | (u32_at(bytes, at + 4) as u64) << 32
}

/// A range of sectors of a disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Partition {
    pub first: u64,
    pub sectors: u64,
}

/// Why a disk has no partition to use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    /// A sector could not be read.
    Unreadable,
    /// The disk has no partition table of the kind XilaOS reads.
    NoTable,
    /// The partition table does not match its checksums.
    Damaged,
    /// The table is fine and has no partition of that type.
    NoPartition,
}

/// Reads the partition table of a disk of `sectors` sectors and gives
/// `each` every entry of it, with its range when that range is one a
/// partition may have. `read` reads one sector and says if it could.
///
/// The checksum of the entries is only known at the end: what `each` saw
/// counts only if this returns `Ok`.
fn entries(
    sectors: u64,
    read: &mut dyn FnMut(u64, &mut [u8; SECTOR]) -> bool,
    each: &mut dyn FnMut(&[u8], Option<Partition>),
) -> Result<(), Why> {
    let mut sector = [0u8; SECTOR];
    if !read(1, &mut sector) {
        return Err(Why::Unreadable);
    }
    if &sector[..8] != b"EFI PART" {
        return Err(Why::NoTable);
    }
    let header_size = u32_at(&sector, 12) as usize;
    if !(92..=SECTOR).contains(&header_size) {
        return Err(Why::Damaged);
    }
    let said = u32_at(&sector, 16);
    let mut header = sector;
    header[16..20].fill(0);
    if crc32(0, &header[..header_size]) != said || u64_at(&sector, 24) != 1 {
        return Err(Why::Damaged);
    }
    let (entries_at, count, size) = (u64_at(&sector, 72), u32_at(&sector, 80) as usize, u32_at(&sector, 84) as usize);
    let entries_crc = u32_at(&sector, 88);
    // where the table says partitions may be
    let (usable_first, usable_last) = (u64_at(&sector, 40), u64_at(&sector, 48));
    // entries of 128 bytes in every table seen so far; larger ones are
    // allowed, as long as a whole number of them fits a sector
    if !(128..=SECTOR).contains(&size) || !size.is_power_of_two() || count > 4096 || entries_at < 2 {
        return Err(Why::Damaged);
    }

    let mut crc = 0;
    let mut left = count;
    let mut at = entries_at;
    while left > 0 {
        if at >= sectors || !read(at, &mut sector) {
            return Err(Why::Unreadable);
        }
        let here = left.min(SECTOR / size);
        crc = crc32(crc, &sector[..here * size]);
        for entry in sector[..here * size].chunks_exact(size) {
            let (first, last) = (u64_at(entry, 32), u64_at(entry, 40));
            let inside = first >= usable_first.max(2) && first <= last && last <= usable_last && last < sectors;
            each(entry, inside.then(|| Partition { first, sectors: last - first + 1 }));
        }
        left -= here;
        at += 1;
    }
    if crc != entries_crc {
        return Err(Why::Damaged);
    }
    Ok(())
}

/// Looks in the partition table of a disk for the first partition of a
/// type.
pub fn find(sectors: u64, kind: &[u8; 16], read: &mut dyn FnMut(u64, &mut [u8; SECTOR]) -> bool) -> Result<Partition, Why> {
    let mut found = None;
    entries(sectors, read, &mut |entry, range| {
        if found.is_none() && entry[..16] == kind[..] {
            found = range;
        }
    })?;
    found.ok_or(Why::NoPartition)
}

/// The type of the partition a firmware starts a system from.
pub const START: [u8; 16] = guid(b"C12A7328-F81F-11D2-BA4B-00A0C93EC93B");

/// The names XilaOS gives its two partitions. The first one tells that a
/// start-up partition was made by XilaOS and holds nothing else.
const START_NAME: &[u8] = b"XilaOS start";
const DATA_NAME: &[u8] = b"XilaOS";

/// Where the name is in an entry: two bytes per character.
const NAME_AT: usize = 56;

fn named(entry: &[u8], name: &[u8]) -> bool {
    let stored = entry[NAME_AT..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]));
    stored.take_while(|c| *c != 0).eq(name.iter().map(|b| *b as u16))
}

/// What a disk holds, as far as its first sectors tell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Holds {
    Unreadable,
    /// Its first sectors were never written.
    Nothing,
    /// A partition table of the kind PCs had before: XilaOS does not read it.
    OldTable,
    /// Something, and no partition table.
    Unknown,
    /// A partition table that does not match its checksums.
    Damaged,
    /// A partition table with this many partitions. `system` is there when
    /// XilaOS was installed on the disk: its start-up partition and the
    /// partition of its tables.
    Partitions { count: u32, system: Option<(Partition, Partition)> },
}

/// Tells what a disk holds, reading its partition table.
pub fn describe(sectors: u64, read: &mut dyn FnMut(u64, &mut [u8; SECTOR]) -> bool) -> Holds {
    let (mut count, mut start, mut data) = (0, None, None);
    let table = entries(sectors, read, &mut |entry, range| {
        if entry[..16] == [0; 16] {
            return;
        }
        count += 1;
        if start.is_none() && entry[..16] == START && named(entry, START_NAME) {
            start = range;
        }
        if data.is_none() && entry[..16] == XILA {
            data = range;
        }
    });
    match table {
        Ok(()) => Holds::Partitions { count, system: start.zip(data) },
        Err(Why::Unreadable) => Holds::Unreadable,
        Err(Why::NoTable) => {
            let (mut first, mut second) = ([0u8; SECTOR], [0u8; SECTOR]);
            if !read(0, &mut first) || !read(1, &mut second) {
                Holds::Unreadable
            } else if first[510..] == [0x55, 0xaa] {
                Holds::OldTable
            } else if first.iter().chain(second.iter()).all(|byte| *byte == 0) {
                Holds::Nothing
            } else {
                Holds::Unknown
            }
        }
        Err(_) => Holds::Damaged,
    }
}

/// Sectors of the start-up partition: 100 MB, as other systems make it.
pub const START_SECTORS: u64 = 100 << 11;
/// Partitions start on a megabyte, which suits every kind of disk.
const ALIGN: u64 = 1 << 11;
/// The least room for the tables that is worth a disk.
const DATA_LEAST: u64 = 16 << 11;
/// The table has 128 entries of 128 bytes: 32 sectors.
const ENTRIES: u32 = 128;
const ENTRY: usize = 128;
const TABLE_SECTORS: u64 = 32;

/// A whole disk for XilaOS: a start-up partition, and the rest for the
/// tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    sectors: u64,
    pub start: Partition,
    pub data: Partition,
    /// Identifiers of the disk and of the two partitions.
    identifiers: [[u8; 16]; 3],
}

impl Layout {
    /// The layout for a disk of `sectors` sectors, or None if it is too
    /// small. The identifiers a table must have are drawn from `seed`.
    pub fn new(sectors: u64, seed: u64) -> Option<Layout> {
        let first = ALIGN + START_SECTORS;
        // the last sectors hold the second copy of the table
        let end = sectors.checked_sub(TABLE_SECTORS + 1)? / ALIGN * ALIGN;
        if end < first + DATA_LEAST {
            return None;
        }
        // identifiers of the random kind (version 4), as tools make them
        let mut state = seed;
        let mut identifiers = [[0u8; 16]; 3];
        for identifier in &mut identifiers {
            for half in identifier.chunks_exact_mut(8) {
                state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut mixed = state;
                mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                half.copy_from_slice(&(mixed ^ (mixed >> 31)).to_le_bytes());
            }
            identifier[7] = identifier[7] & 0x0f | 0x40;
            identifier[8] = identifier[8] & 0x3f | 0x80;
        }
        Some(Layout {
            sectors,
            start: Partition { first: ALIGN, sectors: START_SECTORS },
            data: Partition { first, sectors: end - first },
            identifiers,
        })
    }

    /// The first sector of the entries; the 31 others are empty.
    fn entries(&self) -> [u8; SECTOR] {
        let mut sector = [0u8; SECTOR];
        let partitions = [(START, self.start, START_NAME), (XILA, self.data, DATA_NAME)];
        for (i, (kind, range, name)) in partitions.iter().enumerate() {
            let entry = &mut sector[i * ENTRY..][..ENTRY];
            entry[..16].copy_from_slice(kind);
            entry[16..32].copy_from_slice(&self.identifiers[i + 1]);
            entry[32..40].copy_from_slice(&range.first.to_le_bytes());
            entry[40..48].copy_from_slice(&(range.first + range.sectors - 1).to_le_bytes());
            for (at, byte) in name.iter().enumerate() {
                entry[NAME_AT + 2 * at] = *byte;
            }
        }
        sector
    }

    /// The header of the table: the one at sector `here`, which says where
    /// the other one is and where its own entries are.
    fn header(&self, here: u64, other: u64, entries_at: u64) -> [u8; SECTOR] {
        let mut entries_crc = crc32(0, &self.entries());
        for _ in 1..TABLE_SECTORS {
            entries_crc = crc32(entries_crc, &[0; SECTOR]);
        }
        let mut sector = [0u8; SECTOR];
        sector[..8].copy_from_slice(b"EFI PART");
        sector[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        sector[12..16].copy_from_slice(&92u32.to_le_bytes());
        sector[24..32].copy_from_slice(&here.to_le_bytes());
        sector[32..40].copy_from_slice(&other.to_le_bytes());
        sector[40..48].copy_from_slice(&(2 + TABLE_SECTORS).to_le_bytes());
        sector[48..56].copy_from_slice(&(self.sectors - TABLE_SECTORS - 2).to_le_bytes());
        sector[56..72].copy_from_slice(&self.identifiers[0]);
        sector[72..80].copy_from_slice(&entries_at.to_le_bytes());
        sector[80..84].copy_from_slice(&ENTRIES.to_le_bytes());
        sector[84..88].copy_from_slice(&(ENTRY as u32).to_le_bytes());
        sector[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let sum = crc32(0, &sector[..92]);
        sector[16..20].copy_from_slice(&sum.to_le_bytes());
        sector
    }

    /// Writes the partition table on the disk, in place of whatever table
    /// it had: the sector that tells old tools the whole disk is taken, the
    /// table, and its second copy at the end of the disk. `write` writes
    /// one sector and says if it could; the sector that could not be
    /// written is the error.
    pub fn write(&self, write: &mut dyn FnMut(u64, &[u8; SECTOR]) -> bool) -> Result<(), u64> {
        let last = self.sectors - 1;
        let mut put = |at: u64, sector: &[u8; SECTOR]| if write(at, sector) { Ok(()) } else { Err(at) };
        // the copy first, and the header of the table last: the table
        // counts from the moment its header is there
        for (entries_at, here, other) in [(last - TABLE_SECTORS, last, 1), (2, 1, last)] {
            put(entries_at, &self.entries())?;
            for sector in 1..TABLE_SECTORS {
                put(entries_at + sector, &[0; SECTOR])?;
            }
            if here == 1 {
                let mut guard = [0u8; SECTOR];
                // one partition of the kind "see the other table", from sector 1 on
                guard[446..454].copy_from_slice(&[0, 0, 2, 0, 0xee, 0xff, 0xff, 0xff]);
                guard[454..458].copy_from_slice(&1u32.to_le_bytes());
                guard[458..462].copy_from_slice(&(last.min(u32::MAX as u64) as u32).to_le_bytes());
                guard[510..].copy_from_slice(&[0x55, 0xaa]);
                put(0, &guard)?;
            }
            put(here, &self.header(here, other, entries_at))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: [u8; 16] = guid(b"0FC63DAF-8483-4772-8E79-3D69D8477DE4");

    /// A disk with a partition table, built the way the specification says.
    fn disk(sectors: u64, partitions: &[([u8; 16], u64, u64)]) -> Vec<[u8; SECTOR]> {
        let mut disk = vec![[0u8; SECTOR]; sectors as usize];
        let mut entries = vec![0u8; 128 * 128];
        for (i, (kind, first, last)) in partitions.iter().enumerate() {
            let entry = &mut entries[i * 128..][..128];
            entry[..16].copy_from_slice(kind);
            entry[16] = i as u8 + 1;
            entry[32..40].copy_from_slice(&first.to_le_bytes());
            entry[40..48].copy_from_slice(&last.to_le_bytes());
        }
        for (i, chunk) in entries.chunks(SECTOR).enumerate() {
            disk[2 + i].copy_from_slice(chunk);
        }
        let header = &mut disk[1];
        header[..8].copy_from_slice(b"EFI PART");
        header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        header[12..16].copy_from_slice(&92u32.to_le_bytes());
        header[24..32].copy_from_slice(&1u64.to_le_bytes());
        header[40..48].copy_from_slice(&34u64.to_le_bytes());
        header[48..56].copy_from_slice(&(sectors - 34).to_le_bytes());
        header[72..80].copy_from_slice(&2u64.to_le_bytes());
        header[80..84].copy_from_slice(&128u32.to_le_bytes());
        header[84..88].copy_from_slice(&128u32.to_le_bytes());
        header[88..92].copy_from_slice(&crc32(0, &entries).to_le_bytes());
        let sum = crc32(0, &header[..92]);
        header[16..20].copy_from_slice(&sum.to_le_bytes());
        disk
    }

    fn look(disk: &[[u8; SECTOR]]) -> Result<Partition, Why> {
        find(disk.len() as u64, &XILA, &mut |at, into| {
            *into = disk[at as usize];
            true
        })
    }

    #[test]
    fn identifiers_are_stored_as_tools_store_them() {
        // the type of an EFI system partition, as every disk has it
        let efi = guid(b"C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
        assert_eq!(efi, [0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b]);
        assert_eq!(guid(b"c12a7328-f81f-11d2-ba4b-00a0c93ec93b"), efi);
        assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(crc32(0, b"1234"), b"56789"), 0xcbf4_3926);
    }

    #[test]
    fn the_partition_of_xilaos_is_found_among_others() {
        let disk = disk(4096, &[(LINUX, 64, 1023), (XILA, 1024, 3071), (XILA, 3072, 4000)]);
        assert_eq!(look(&disk), Ok(Partition { first: 1024, sectors: 2048 }));
    }

    #[test]
    fn a_disk_without_one_is_not_used() {
        assert_eq!(look(&disk(4096, &[(LINUX, 64, 4000)])), Err(Why::NoPartition));
        assert_eq!(look(&disk(4096, &[])), Err(Why::NoPartition));
        assert_eq!(look(&vec![[0u8; SECTOR]; 64]), Err(Why::NoTable));
        // a partition that does not fit the disk is not one
        assert_eq!(look(&disk(4096, &[(XILA, 1024, 4096)])), Err(Why::NoPartition));
        assert_eq!(look(&disk(4096, &[(XILA, 2000, 1999)])), Err(Why::NoPartition));
        assert_eq!(look(&disk(4096, &[(XILA, 33, 100)])), Err(Why::NoPartition));
        assert_eq!(look(&disk(4096, &[(XILA, 1024, 4063)])), Err(Why::NoPartition));
        assert!(look(&disk(4096, &[(XILA, 34, 4062)])).is_ok());
    }

    #[test]
    fn a_damaged_table_is_not_trusted() {
        let good = disk(4096, &[(XILA, 1024, 3071)]);
        assert!(look(&good).is_ok());
        // any byte of the header or of the entries
        for (sector, byte) in [(1, 40), (1, 80), (1, 91), (2, 0), (2, 33), (2, 300), (20, 7), (33, 511)] {
            let mut bad = good.clone();
            bad[sector][byte] ^= 1;
            assert_eq!(look(&bad), Err(Why::Damaged), "sector {sector}, byte {byte}");
        }
        let mut bad = good.clone();
        bad[1][0] = b'X';
        assert_eq!(look(&bad), Err(Why::NoTable));
        // a sector that cannot be read
        let unreadable = |fails: u64| {
            find(4096, &XILA, &mut |at, into| {
                *into = good[at as usize];
                at != fails
            })
        };
        assert_eq!(unreadable(1), Err(Why::Unreadable));
        assert_eq!(unreadable(17), Err(Why::Unreadable));
        assert!(unreadable(34).is_ok());
    }

    /// A disk kept in memory, of which only the sectors written exist.
    fn written(layout: &Layout) -> std::collections::HashMap<u64, [u8; SECTOR]> {
        let mut disk = std::collections::HashMap::new();
        layout
            .write(&mut |at, sector| {
                disk.insert(at, *sector);
                true
            })
            .unwrap();
        disk
    }

    #[test]
    fn a_whole_disk_is_laid_out_for_xilaos() {
        let sectors = 500 << 11;
        let layout = Layout::new(sectors, 7).unwrap();
        assert_eq!(layout.start, Partition { first: 2048, sectors: 100 << 11 });
        assert_eq!(layout.data, Partition { first: 101 << 11, sectors: 398 << 11 });
        // too small for the tables to be worth it
        assert_eq!(Layout::new(117 << 11, 7), None);
        assert_eq!(Layout::new(10, 7), None);
        assert!(Layout::new(118 << 11, 7).is_some());

        let disk = written(&layout);
        let mut read = |at: u64, into: &mut [u8; SECTOR]| {
            *into = disk.get(&at).copied().unwrap_or([0; SECTOR]);
            true
        };
        // the table reads back, by the same reading that finds the partition at start
        assert_eq!(find(sectors, &XILA, &mut read), Ok(layout.data));
        assert_eq!(find(sectors, &START, &mut read), Ok(layout.start));
        assert_eq!(describe(sectors, &mut read), Holds::Partitions { count: 2, system: Some((layout.start, layout.data)) });
        // old tools see a disk that is taken
        assert_eq!(disk[&0][446..462], [0, 0, 2, 0, 0xee, 0xff, 0xff, 0xff, 1, 0, 0, 0, 0xff, 0x9f, 0x0f, 0]);
        assert_eq!(disk[&0][510..], [0x55, 0xaa]);
        // the second copy, at the end of the disk, is a table too: it says
        // where it is, where the first one is, and has the same entries
        let copy = disk[&(sectors - 1)];
        assert_eq!((u64_at(&copy, 24), u64_at(&copy, 32), u64_at(&copy, 72)), (sectors - 1, 1, sectors - 33));
        let mut header = copy;
        header[16..20].fill(0);
        assert_eq!(crc32(0, &header[..92]), u32_at(&copy, 16));
        assert_eq!(disk[&(sectors - 33)], disk[&2]);
        assert_eq!(u32_at(&copy, 88), u32_at(&disk[&1], 88));
        assert_eq!((u64_at(&disk[&1], 40), u64_at(&disk[&1], 48)), (34, sectors - 34));
    }

    #[test]
    fn identifiers_differ_from_one_installation_to_the_next() {
        let (one, other) = (Layout::new(1 << 20, 1).unwrap(), Layout::new(1 << 20, 2).unwrap());
        let mut all: Vec<[u8; 16]> = one.identifiers.iter().chain(other.identifiers.iter()).copied().collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 6);
        // of the random kind
        assert!(all.iter().all(|id| id[7] >> 4 == 4 && id[8] >> 6 == 2));
    }

    #[test]
    fn what_a_disk_holds_is_told() {
        let tell = |disk: &[[u8; SECTOR]]| {
            describe(disk.len() as u64, &mut |at, into| {
                *into = disk[at as usize];
                true
            })
        };
        assert_eq!(tell(&vec![[0u8; SECTOR]; 64]), Holds::Nothing);
        let mut old = vec![[0u8; SECTOR]; 64];
        old[0][510..].copy_from_slice(&[0x55, 0xaa]);
        assert_eq!(tell(&old), Holds::OldTable);
        let mut unknown = vec![[0u8; SECTOR]; 64];
        unknown[1][3] = 9;
        assert_eq!(tell(&unknown), Holds::Unknown);
        assert_eq!(tell(&disk(4096, &[])), Holds::Partitions { count: 0, system: None });
        // a start-up partition somebody else made is not the one of XilaOS
        let others = disk(4096, &[(START, 64, 1023), (LINUX, 1024, 2047), (XILA, 2048, 4000)]);
        assert_eq!(tell(&others), Holds::Partitions { count: 3, system: None });
        let mut damaged = others.clone();
        damaged[2][40] ^= 1;
        assert_eq!(tell(&damaged), Holds::Damaged);
        assert_eq!(describe(4096, &mut |_, _| false), Holds::Unreadable);
    }
}
