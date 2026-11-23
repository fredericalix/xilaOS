# The storage engine

XilaOS has no file system. The disk belongs to a database, and everything
that lasts is a row of a table: the user's tables, the programs of
[XilaScript](05-xilascript.md) (rows of the table `programs`), the settings
kept from one start to the next (rows of `kept_settings`).

This chapter describes the engine that keeps those tables: the crate
`xila-db`, in `db/`. It is about 1,700 lines of Rust and 900 lines of
tests, has no dependency, and builds without the standard library
(`no_std` with `alloc`). It knows nothing of SQL, of the screen or of the
machine. The query language lives in `xila-core` and calls it; the kernel
gives it a disk.

What it promises:

- A change is a transaction. It is applied whole or not at all.
- When `commit` returns, the change is safe on the disk.
- A power cut at any moment leaves the database as it was before the
  transaction or as it is after, never in between. There is no journal
  and nothing to replay at the next start.
- A page the disk damaged, or wrote only half of, is noticed when it is
  read, and reported with its number.

## The layers

```text
   xila-core        sql.rs, change.rs, lang.rs, settings.rs
                    ------------------------------------------------
   lib.rs           Database: tables, records, transactions, catalog
   tree.rs          B-trees: a number -> some bytes
   store.rs         pages that are never overwritten; the two headers
   disk.rs          Volume: pages of 4 KB on a Disk
                    ------------------------------------------------
   kernel           virtio.rs (QEMU), ahci.rs + a GPT partition (SATA),
                    or Memory when the machine has no disk for XilaOS
```

| File | Lines | What it holds |
|---|---|---|
| `db/src/disk.rs` | 127 | The `Disk` trait, `Memory`, and `Volume`. |
| `db/src/store.rs` | 430 | Copy-on-write pages, checksums, headers, commit and rollback. |
| `db/src/tree.rs` | 377 | The B-tree: `set`, `walk`, `range`, `pages`, `release`, `check`. |
| `db/src/lib.rs` | 990 | `Database`: tables, records, the catalog, transactions. |
| `db/src/testing.rs` | 674 | A disk whose power can be cut, and the tests that cut it. |

## The volume

The engine reads and writes whole pages of 4,096 bytes on a `Volume`. A
volume sits on a `Disk`:

```rust
pub trait Disk {
    fn name(&self) -> &'static str;          // "virtio", "sata", "memory"
    fn pages(&self) -> u64;
    fn keeps(&self) -> bool { true }         // still there after a stop?
    fn read(&mut self, page: u64, into: &mut Page) -> Result<(), DiskError>;
    fn write(&mut self, page: u64, from: &Page) -> Result<(), DiskError>;
    fn flush(&mut self) -> Result<(), DiskError>;   // safe from a power cut
    fn discard(&mut self, _page: u64) {}
}
```

`flush` is the one promise the engine needs from a disk: when it returns,
everything written before it survives a power cut. The virtio driver
sends a flush request when the device offers one; the SATA driver sends
`FLUSH CACHE` or `FLUSH CACHE EXT`, whichever the disk says it knows.

There are three disks:

| Disk | Where | Page *n* is |
|---|---|---|
| `VirtioBlk` | `kernel/src/virtio.rs` | sectors 8*n* to 8*n*+7 of the whole virtio disk |
| `Partition` | `kernel/src/disk.rs` | sectors `first` + 8*n* of a SATA disk, inside the GPT partition of XilaOS's type |
| `Memory` | `db/src/disk.rs` | a map from page number to page, in the kernel's heap; only written pages take room |

A volume has exactly one disk today. It is the place where a mirror over
two disks would go: the engine above only knows page numbers.

Which disk the kernel picks at boot, and what `install` writes around
the database's partition, is in
[Disk layout and installation](09-disk-layout-and-install.md).

## What is on the disk

```text
 page 0      header, slot 0  ┐ written in turn: the commit of transaction
 page 1      header, slot 1  ┘ N goes to slot N mod 2
 page 2..    tree pages: leaves and branches of the catalog and of
             every table, in no particular order; and free pages
```

### Every page

Every page starts with the same 16 bytes:

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | CRC-32C of bytes 4 to 4095 |
| 4 | 1 | kind: 1 leaf, 2 branch (a header page has its magic here) |
| 5 | 1 | level: 0 for a leaf, 1 for a branch right above leaves... |
| 6 | 2 | count: entries of a leaf, or pages below a branch |
| 8 | 8 | number of the transaction that wrote the page |

All numbers are little endian. The checksum is computed by the store
when a page is written (`seal`) and verified when it is read from the
disk (`sealed`).

### The header

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | CRC-32C of the rest |
| 4 | 8 | magic: `XilaOSdb` |
| 12 | 4 | format version: 1 |
| 16 | 4 | page size: 4096 |
| 20 | 8 | `txn`: number of the transaction this header commits |
| 28 | 8 | `pages`: pages of the volume the database may use |
| 36 | 8 | `catalog`: page of the root of the catalog tree; 0 when empty |
| 44 | 8 | `next_table`: the key the next table will get in the catalog |

The header is the root of everything: it names the catalog, the catalog
names the tables, each table names the root of its tree.

### A leaf

After the 16 bytes, `count` entries, in the order of their keys:

```text
 key (8)   length (2)   value (length bytes)
```

A value is at most 1,000 bytes, so at least four entries fit in a leaf.

### A branch

After the 16 bytes:

```text
 page (8)   then count - 1 times:  key (8)  page (8)
```

Keys below the first page are less than the first key; keys below the
page that follows key *k* are at least *k*. A branch holds up to 255
pages. Leaves are all at the same depth.

## Pages are never overwritten

The rule of `store.rs`: a page that belongs to the last committed state
is never written again. Changing it means writing a copy somewhere free.

```text
 before                          after changing a row in leaf C

   header ──► root A               header' ──► root A'     (A, C: freed
              ├─ leaf B                        ├─ leaf B     at the commit)
              └─ leaf C                        └─ leaf C'
```

Changing leaf C writes C' in a free page; its parent must now point at
C', so the parent is copied too, and so on up to the root. The new root
is a new state of the whole tree, and the old root still describes the
old state, untouched, until the header changes.

Inside one transaction a page is copied once: `Store::write(old, fill)`
writes over `old` when the open transaction already wrote it, and takes
a free page otherwise, noting `old` in a list of pages to free at the
commit.

The pages a transaction writes are not on the disk yet. They are kept in
memory (`dirty`) until the commit. A transaction is therefore bounded by
memory; one SQL statement is one transaction, so they are small.

### The commit

`Store::commit(catalog, next_table)` does four things, in this order:

1. Writes every dirty page to its place. These places hold nothing of
   the committed state, so a failure here costs nothing.
2. `flush`: waits until those pages are safe.
3. Writes the header, with the number `txn + 1`, in slot
   `(txn + 1) mod 2`: the slot the previous commit did not use.
4. `flush` again.

Step 3 is the whole commit. Before that one page is on the disk, the
newest valid header is the old one and the disk holds the old state.
After it, the disk holds the new state. A header written by half fails
its checksum and is not a header: the old one still wins.

After the commit the freed pages become free for the next transaction
(never before: until the header is safe, the old state still needs
them), and the dirty pages move to a small cache.

If step 3 or 4 fails, the engine cannot know which of the two states
the disk holds. Both are whole, but writing anything more could damage
whichever one the next start will find. The store sets `stopped`:
every later change is refused with `Error::Stopped`, and reading goes
on. The `disk` command says so, and a restart clears it.

### The rollback

`Store::rollback` forgets the dirty pages and the list of pages to
free. Nothing was written, so there is nothing to undo on the disk.

### Opening

`Store::open` reads pages 0 and 1 and takes the valid header with the
highest transaction number. Then:

| What pages 0 and 1 hold | Result |
|---|---|
| A valid header | The database is opened. |
| Only zeros | `Blank`: the volume gets a new, empty database. |
| Anything else | `Error::Foreign`: the volume is given back, untouched. |
| Neither can be read | `Error::Disk`. |
| A header whose `pages` cannot be right | `Error::Damaged`. |

A disk that holds something else is never written. The kernel keeps it
aside, starts with the tables in memory, says so in the boot log, and
only `disk erase yes` (which calls `Database::format`) takes it.

A page read from the disk is refused, as `Error::Damaged(page)`, when
its checksum is wrong or when the transaction number in it is higher
than the header's: such a page was written by a transaction that never
committed, and is part of nothing.

### Which pages are free

The list of free pages is written nowhere. When a volume is opened,
`Database::tables_of` follows the trees and marks each page they use in
a bitmap in memory (`Store::taken`, one bit per page):

1. the pages of the catalog tree;
2. for each table the catalog describes, the pages of its tree.

Every other page is free. So no crash can lose a page, and none can
free a page that is in use: there is no free list to get out of step
with the trees. A page reached twice, or outside the volume, is
`Error::Damaged`.

The walk does not read the leaves (`tree::pages` stops at the branches
of level 1, which name their leaves), so its cost is a small part of
the size of the database: one page in about 255.

A free page is found by scanning the bitmap from a hint, the lowest
page that may be free.

### The reserve

Removing rows writes copies before it frees anything. A volume that is
completely full could therefore not even delete. The store keeps back
32 pages (or an eighth of a small volume): an insert or an update is
refused with `Error::Full` when only the reserve is left, while a
delete, and the small write of the catalog at each commit, may use it.

### The cache

Up to 256 pages read from a disk (1 MB) are kept in memory. A volume in
memory needs no cache. The cache is a plain map; when it is full, the
page with the lowest number leaves.

## The B-tree

`tree.rs` is a B-tree from a number (`u64`) to bytes. A tree is known
by the page of its root; 0 is the empty tree. Because pages are copied
on the way down, every change returns a new root.

| Function | What it does |
|---|---|
| `set(store, root, key, Some(value))` | Gives a key a value; returns the new root. |
| `set(store, root, key, None)` | Removes a key. |
| `walk(store, root, visit)` | Calls `visit` with every key and value, in order. |
| `range(store, root, first, last, visit)` | The same for the keys from `first` to `last`: only the pages that can hold them are read, so one key costs one page per level. |
| `pages(store, root, each)` | Calls `each` with every page of the tree, without reading the leaves. |
| `release(store, root)` | Frees every page of the tree (`drop table`). |
| `check(store, root, each)` | Reads everything and checks that the tree is well made. |

Three choices keep it short:

- **Splits lean to the right.** Keys are row ids and mostly arrive in
  rising order. When a leaf overflows because of a key added at its
  end, that key goes alone to a new leaf and the full one stays full;
  only an insertion in the middle shares the bytes in two halves.
  Branches do the same. A table filled in order has full pages.
- **Pages are not merged.** A leaf is given back when it is empty, and
  a root with a single page below it is dropped. A table from which
  many rows were removed keeps pages that are partly empty.
- **There is one tree per table, by id.** No secondary index exists:
  a query reads the whole table, unless it says where the id must be
  (`range`).

`check` verifies, for a whole tree, that keys rise, that every key is
on the right side of the keys above it, that leaves are all at the same
depth and that no entry is too long. `Database::verify` runs it on the
catalog and on every table, and also checks that every page is used
exactly once and that the row counts are right. The tests call it after
every simulated power cut.

## Tables, records and the catalog

`lib.rs` turns trees into tables.

**A table** has a name and up to 8 columns. Column 0 is always `id`: a
number the engine gives to each row, starting at 1, never used twice in
a table while the machine runs. It is the key of the row in the tree,
and what change events carry. Names are kept in lower case, 24
characters at most, letters, digits and `_`.

**A column** has one of three types:

| `Kind` | Stored as |
|---|---|
| `Int` | a signed 64 bit number |
| `Dec(scale)` | a signed 64 bit number of the smallest unit: 12.50 with scale 2 is 1250 |
| `Text` | bytes |

**A row** is stored as a record, the value of its key in the tree. The
id is not in the record: it is the key. Each field is one tag byte
followed by its value:

```text
 0                        null
 1  number (8 bytes)      int
 2  number (8 bytes)      decimal, in the smallest unit of its column
 3  length (2)  bytes     text
```

Fields a record does not have read as null. A record is at most 1,000
bytes; larger objects (pictures, music) need a form of their own,
which is not built yet.

**The catalog** is one more tree. Its keys are table keys (given from
`next_table`, never used twice) and its values describe a table:

```text
 name length (1)  name
 root (8)  next id (8)  rows (8)
 columns after id (1)
 then for each:  kind (1)  scale (1)  name length (1)  name
```

So the root of a table's tree lives in the catalog, and the root of the
catalog lives in the header. A commit that changed one row of one table
writes: the leaf, the branches above it, the leaf of the catalog that
holds the table's record (its root changed), the branches above that,
and one header.

## Transactions

The `Database` API is small:

| Call | Effect |
|---|---|
| `create(name, columns)` | Makes a table. |
| `remove(table)` | Removes a table and frees its tree. |
| `insert(table, fields)` | Adds a row, returns its id. |
| `scan(table, visit)` | Calls `visit` with every row, by id. |
| `scan_ids(table, ids, visit)` | The same for the rows whose ids are in `ids` (an `Ids`: first and last), read through `tree::range`. `update_each_ids` and `delete_ids` change and remove among those rows only. |
| `update(table, pick, set)` | Sets columns of the rows `pick` accepts. |
| `update_each(table, each)` | Changes rows one by one, each with its own new values. |
| `delete(table, pick)` | Removes the rows `pick` accepts. |
| `commit(each)` | Makes everything since the last commit final. |
| `rollback()` | Undoes everything since the last commit. |
| `verify()` | Reads the whole database and checks it. |

The first change opens a transaction: the engine keeps a copy of the
table descriptions as they were (`saved`), and a log of what changes
(table, kind of change, id). There is one transaction at a time and one
writer; the kernel guarantees it by using the database only under the
lock of its shared state.

`commit` writes the changed table records into the catalog, commits the
store, and only then calls `each` for every logged change. That
callback is where LISTEN / NOTIFY gets its events: an event is never
sent for a change that did not commit. On any error the transaction is
rolled back and the error returned.

`rollback` puts the saved descriptions back, with one exception: the
next id of each table keeps its highest value, so an id that was handed
out is not handed out again.

`xila-core` never leaves a transaction open: `change::change` runs one
SQL statement, then commits on success or rolls back on any error. That
is why a statement is applied whole or not at all.

### Without a disk

`Database::new()` makes a database that has no store yet. The first
write makes one on a `Memory` disk, sized to the limit the kernel set:
a quarter of the usable memory. The same engine, the same pages and the
same commits run in memory; only `flush` has nothing to wait for. The
`disk` command and the boot log say that the tables are lost when the
machine stops.

## How it is tested

`db/src/testing.rs` has a disk called `Platter` that models what a real
disk promises, and no more: a write followed by a finished flush is
there after a power cut; of the writes since the last flush any may be
there or not; the write under way may be there in part.

A scenario is a list of transactions. The test (`cut_everywhere`) runs
it once to record the state after each commit and to count the disk
operations, then runs it again as many times as there are operations,
cutting the power at each write and each flush in turn. After each cut
the power "comes back" in one of seven ways:

| Mode | Unflushed writes | The write under way |
|---|---|---|
| 0 | all lost | lost |
| 1 | all kept | written whole |
| 2 | all kept | first half written |
| 3 | some kept, at random | first half written |
| 4 | some kept, at random | lost |
| 5 | all lost | written whole |
| 6 | all kept | the page reads as noise |

Each time the database must open, pass `verify`, and hold exactly the
state of the last commit that returned, or of the one that was under
way; then it must accept one more transaction and still be whole after
another restart. The scenarios cover rows changing, tables being
created and dropped, and a tall tree.

Other tests in the same file:

- what was committed is there after a restart;
- a disk that holds something else is left alone, byte for byte, and
  can be erased on purpose;
- one changed byte in any page is noticed when the page is read;
- a damaged header brings back the commit before it;
- a write that fails undoes the change, which can be tried again;
- a header that fails stops the changes but not the reading;
- 4,000 changes drawn at random, restarts included, give the same rows
  as a plain map in memory.

Four faults put into the engine on purpose while it was written (no
flush before the header, freed pages reused too early, checksum not
verified, a single header) were each caught by these tests.

In QEMU, `tools/check.py` stops and restarts the machine and finds the
tables again. Then, several times in a row, on a virtio disk and on a
SATA partition, it sends a statement that adds three rows and kills the
virtual machine a moment later (`Xila.cut()`), with a different pause
each time. After each restart the table must hold all three rows of the
statement or none of them, and the rows from before.

Run the engine's tests on the development machine with:

```sh
cargo test -p xila-db
```

Any change to `store.rs` or `tree.rs` must keep them passing, and a new
kind of write needs a scenario there.

## Limits

What the engine does not do:

- **Disk failure.** A damaged page is detected, not repaired. The
  planned answer is a mirror (two disks, each page written on both,
  the checksum telling which copy is good). The `Volume` layer is where
  it will go; it is not built yet.
- **A write the disk loses silently.** If a disk answers "written" and
  keeps the old page, that old page is still valid by its own checksum.
  Catching it needs the checksum of a page kept in the page that points
  at it, as ZFS does.
- **A disk that lies about `flush`.**
- **Indexes.** One tree per table, by id. Every query reads the whole
  table, and `where id = 5` does too.
- **Snapshots.** Keeping an old root would be enough to see the
  database as it was. The format allows it; nothing uses it.
- **Concurrent transactions.** One writer, and readers wait for it.
- **Large values.** A row fits in 1,000 bytes.
- **Sector sizes.** 512-byte sectors only; no NVMe driver; MBR
  partition tables are not read.

Limits that come from the fixed-size buffers of `xila-core`, not from
this engine (48 characters per text, 32 rows per result), are listed
in [The database and SQL](04-sql.md).
