# Status and roadmap

The first version of XilaOS was built on 18 and 19 November 2026 and
published as XilaOS 0.1: a working first layer of a much larger plan.
Development resumed on 23 November 2026. It is a hobby project with no
schedule, and nothing in the plan is a promise. This chapter says
plainly what is done, what is not, what is designed on paper, and what
is left to do.

Nobody should run it on a machine whose data matters, or on a network
they do not trust.

## What is done

Each line was verified in QEMU by `tools/check.py`; those marked T430
were also tried on the real machine.

| Area | Done | T430 |
|---|---|---|
| Boot as a UEFI application, from a stick, a disk or the network | yes | yes |
| Character interface, virtual screens, menu, themes, `us` / `fr` keyboards | yes | yes |
| Serial shell with machine-readable output | yes | no serial port in use |
| Preemptive kernel tasks, page allocator, heap, own page tables | yes | yes |
| The city, drawn from live state | yes | yes |
| Prepared picture, block copies, write combining | yes | yes: a whole screen in 0.3 ms instead of 9.4 |
| Intel HD Graphics: reading the chip, two pictures changed between sweeps, brightness | unit tests only (QEMU has no such chip) | yes |
| Storage engine: copy-on-write B-trees, checksums, atomic commits | yes, power cuts included | yes |
| virtio disk | yes | not applicable |
| SATA (AHCI), GPT partition of XilaOS's type | yes | yes |
| `install`, `install system`, the USB stick image | yes | yes |
| SQL: `SELECT`, `CREATE TABLE`, `DROP TABLE`, `INSERT`, `UPDATE`, `DELETE`, expressions | yes | yes |
| `disk check`: every page of the tables read and checked | yes | not tried |
| SQL: `IN`, `BETWEEN`, `NOT LIKE`, calculations on functions over rows; rows reached by their `id`; `JOIN` and `LEFT JOIN` of two tables | yes | not tried |
| `install` shows how far it is; dead keys of the `fr` layout; a quote inside a text | yes | not tried |
| LISTEN / NOTIFY | yes | yes |
| XilaScript: variables, blocks, stored programs, `on` and `every` programs | yes | yes |
| Clock, `now` and `today`, `every day at` | yes | yes |
| Settings kept between starts | yes | yes |
| QR codes for output | yes, read back by another reader | yes |
| Ethernet (Intel e1000 family), ARP, IPv4, ICMP, UDP, DHCP | yes | yes, after five trials |
| Names (DNS): `net name`, `net names`, `net get` by name | yes | not tried |
| TLS 1.3, the side that calls: `net get https://`, checked against OpenSSL and servers of the Internet | yes | not tried |
| HTTP questions with lines and bytes of their own (`net post`), answers read whole: a said length, chunks, to the end | yes | not tried |
| Keys to services (`key`): kept on the disk in a table SQL does not know, shown by nothing | yes | not tried |
| TCP, both sides | yes | yes, through the web pages |
| Prompt over the network (`remote`), not encrypted | yes | yes |
| Tables in a browser (`web`), read only, not encrypted | yes | yes |
| `install from` (TFTP), `tools/netboot.py` | yes | yes |

Numbers at version 0.1: about 25,000 lines of Rust, 229 unit tests,
about 300 system checks. On 26 November 2026: about 29,500 lines, 275
unit tests, 415 system checks.

## Known limits of what is done

These are not bugs; they are where the first version stopped. What is
meant to change is in the [work plan](#work-plan) below.

**SQL.** Two tables in a join at most, no subqueries. No indexes, keys or constraints: a
query reads the whole table unless it says where the `id` must be. No
`ALTER TABLE`. No `BEGIN` / `COMMIT`: one statement is one transaction.
Eight columns per table, 32 rows per result, 48 characters per text, no
date type. Details in
[The database and SQL](04-sql.md).

**XilaScript.** No parameters, no editor (a program is retyped to be
changed), no `insert ... select`, loops over 32 rows at most. A program
holds the whole system while it runs. Details in
[XilaScript](05-xilascript.md).

**Storage.** A damaged page is detected, not repaired. A write the disk
loses silently is not caught. No mirror, no snapshots. Details in
[The storage engine](08-storage.md).

**Kernel.** One processor core. No user mode. No APIC, no ACPI, so no
clean power-off: `reboot` or the power button. A fatal error is only
reported on the serial port. Details in [The kernel](07-kernel.md).

**Network.** No IPv6, no fragments, eight TCP conversations, no
out-of-order reassembly. Names: IPv4 addresses only, and nothing
proves who answered. TLS only where XilaOS calls (`net get https://`):
version 1.3 only, one cipher, no asking whether a certificate was
revoked, and a protocol read by no specialist. **What XilaOS opens to
the network (`remote`, `web`) is not encrypted, and authenticated by
nothing beyond a code sent in clear.** Details in
[Networking](10-networking.md).

**Hardware.** One real machine. PS/2 keyboard only, no USB, no NVMe, no
sound, no Wi-Fi. Details in [Hardware and graphics](11-hardware.md).

**Interface.** Two virtual screens (the overview and the boot log). The
`sound` and `irc` labels of the header come from the mock-up and have
nothing behind them.

## The larger plan

The system is meant to grow in layers. Layer 0 (the base) and layer 1
(the database) are mostly there, with a first part of the network.

| Layer | Content | State |
|---|---|---|
| 0. Base | Boot, memory, interrupts, tasks, screen, keyboard, interface | Done, without memory protection |
| 1. Database | Storage, tables, SQL, transactions, LISTEN / NOTIFY | Done in a first form; no typed large objects, no access rights |
| 2. Programs | A virtual machine, zones, public / private status, ACLs, installing applications, supervised services, a vault for secrets | Not started, except XilaScript and scheduled programs |
| 3. Network | Ethernet, TCP/IP, DNS, TLS, a stack per zone, webhooks and mail with a durable queue | Ethernet, TCP/IP, DNS and TLS for the side that calls |
| 4. Sound | Audio driver, mixing, MP3, ambiences | Not started |
| 5. Applications | Spreadsheet, database front end, a HyperCard-like, mail, text web browser, IRC, HTTP server, a port of Doom | Not started |
| 6. Agents | AI companions with their own access rights to the data | A first companion is under way, without access rights |
| 7. Later | Wi-Fi, USB, RISC-V | Not started |

### The intentions behind it

These are the author's decisions for the whole system. They explain
choices that are visible in the code.

- **Not a Unix.** What was held against Unix: too much freedom, which
  opens holes; an unfriendly shell; applications that are hard to
  install. The model is OS/400: a database in the heart of the system,
  lighter, with data shared between applications under control.
- **No file system.** Everything that lasts is an object of the
  database: tables, programs, music, pictures, settings, zones, rights.
- **Programs are compiled for a virtual machine**, not to native code,
  as on OS/400. It gives independence from the processor, isolation by
  construction, and an application that installs as one object. The
  machine itself has not been chosen: WebAssembly, or one made for
  XilaOS around objects and tables.
- **Two levels of language.** Heavy programs (Doom, a spreadsheet
  engine) compiled for the virtual machine from an existing language;
  and the house language, XilaScript, interpreted, for everything
  written on the machine itself. Its words are English, one statement
  per line, blocks closed by `end`.
- **An application is a zone**, isolated from the others, in the manner
  of FreeBSD jails or illumos zones, with a public or private status
  and access control lists. Private by default. Isolation is to come
  from the virtual machine rather than from a hypervisor.
- **Events go out.** LISTEN / NOTIFY is meant to feed webhooks and
  mail through a durable queue written in the same transaction as the
  change: a companion raises an alert and a message leaves for a phone;
  a disk fails and a pager rings.
- **Characters and the keyboard.** The interface is a grid of
  characters with the V2_OS font, drawn in a graphics mode so that a
  screen could switch to pixels for a game. No mouse.
- **The city shows real state**, not decoration: a block per object, a
  light when something is read or written.
- **It must work in QEMU with the serial shell**, so that an AI can run
  commands, develop and test.

### Designs that exist on paper

**The storage mirror.** Two disks, each page written on both. On a
read, the checksum says which copy is good and the bad one is repaired
from the other. It needs a degraded mode, rebuilding onto a new disk, a
periodic read of every page, and an event when a disk fails. It would
live in `db/src/disk.rs`, under `Volume`. No RAID 0: it tolerates
nothing.

**Access rights.** Every access to the data would go through kernel
calls of the virtual machine, which know the calling zone and check the
object's access list. Since programs could reach data in no other way,
there would be no way around. An event carries little (table, kind of
change, row id) so that listening cannot leak data: the listener reads
the row again with its own rights.

**Graphics, further.** Hardware planes of the Intel chip for sprites
over the character grid, hardware scaling for a low-resolution game,
and at the far end the chip's 3D engine to render the city with a
moving camera. See [Hardware and graphics](11-hardware.md).

**An encrypted protocol between XilaOS machines.** The last thing
designed before version 0.1, to replace the clear-text `remote` prompt
and to let one machine hand work to others:

- three kinds of parties speak it: a XilaOS instance, a hub on the
  Internet that keeps a directory and routes sealed envelopes, and a
  client application for other systems;
- every party opens one outgoing TCP link to the hub; two parties then
  open an end-to-end channel through it that the hub cannot read;
- at a first meeting a person compares a short code on both screens and
  accepts on the keyboard; afterwards the two recognise each other by
  their keys;
- the handshake follows the published Noise framework, with the
  algorithms taken from established Rust libraries (the ones TLS
  brought in since: X25519, ChaCha20-Poly1305, SHA-256), `no_std` and
  without allocation, as a state machine fed with bytes like `tcp.rs`;
- a channel closes at the first message that does not verify; there is
  no mode without verification, not even for tests.

It was reviewed once by a second AI instance asked to break it, which
found one serious flaw (only one side confirmed the code) that the
design then fixed. It has not been reviewed by a human specialist and
none of it is implemented. The steps after it (the hub, the remote
terminal, primary and secondary machines, distributed tasks, a XilaOS
without a screen) are only outlined.

### Where the original notes are

The working documents of the first two days were written in French:
the vision, the reasoning behind each technical choice, the first work
plan with its journal, the language reference, the hardware notes and
the protocol design. This English documentation replaces them and sums
up what they held: the intentions and the unbuilt designs above, and
the work plan below, come from them. The notes themselves, and the
history of those two days, are not part of this repository, which
starts from version 0.1.

## Work plan

This is what is left to do, kept up to date as the work goes. It is a
list, not a schedule: inside a group the order is roughly the order in
which things depend on each other, and nothing here is promised. A
step that is finished and verified leaves this plan and enters the
table of what is done, at the top of this chapter. A larger step is
designed with the author before it is built.

### The phases under way

Chosen by the author on 23 November 2026. The T430 is out of reach for
now, so these phases are the ones that can be built and verified in
QEMU alone; device interrupts and the drivers for recent PCs wait for
a real machine. Each numbered step is merged once its unit tests and
its checks in `tools/check.py` pass.

On 26 November 2026 the author put a first AI companion ahead of the
rest: it is the phase under way, and what is left of phase 2 waits for
it. Its steps are [below](#the-companion).

| Phase | Content | State |
|---|---|---|
| 1. Loose ends | The small things that were off; an apostrophe in a text; the dead keys of `fr`; a command that reads the whole database; `install` that shows it is writing | Done on 23 November 2026 |
| The companion | A first AI companion: a screen to write to it, and its reactions to what happens in the system. Names (DNS), TLS and HTTPS first, since the model it speaks through is reached over the Internet | In progress |
| 2. SQL | Done: `IN`, `BETWEEN`, `NOT LIKE`, calculations on functions over rows; a row reached by its `id` without reading the table; joins of two tables. Left: constraints and indexes, which change what is written on the disk; `BEGIN` / `COMMIT`; a view of the log, which waits for texts longer than 48 characters | Waits for the companion |
| 3. XilaScript | Parameters and names of a program's own; a task of its own for a program, and for the commands that wait (`net ping`, `net get`, `install from`); an editor; `insert ... select`, the values of before, more rhythms | After 2 |
| 4. The encrypted channel | Step 1 of the protocol, as a library tested on the development machine | Waits for the author's agreement on the design. Outside code is there since TLS, with the algorithms the channel needs |
| 5. The virtual machine | The choice of the machine, then a first program run in it | Starts with a design session |
| 6. The mirror | Two disks, repair by the checksum, degraded mode, rebuilding | After 5, or earlier if a decision is awaited |

**Waiting for the T430.** What these phases build is only verified in
QEMU until it is tried there: the dead keys on the real keyboard,
`install` showing its progress on a real disk, `disk check` on a
real disk, names asked of the name server of a real network, and TLS
with the keys its processor draws and the time its clock tells.

### The companion

What the author decided on 26 November 2026:

- **One companion to begin with**, with no narrow role, described by a
  row of a table (its name, its character, what it is told). The four
  of the first notes (a lookout, an archivist, a luthier, a doorkeeper)
  would be more rows.
- **It speaks through a model reached over the Internet**, by HTTPS.
  So names and TLS are built into XilaOS itself, not left to another
  machine on the way.
- **It reads the tables and acts by itself**: it runs commands and
  changes data without asking. Three things hold it: every action it
  takes is a row of a table; a command stops it; `install`,
  `disk erase`, `remote on` and `web on` are refused to it. Nothing
  caps what it spends on the model.
- **It reacts by itself** to changes in a table, to the state of the
  system, to the clock, and when a XilaScript program calls it.
- **It remembers**: its conversations and notes of its own are kept in
  the database, as rows that SQL reads. No index of vectors: at the
  size of one machine's notes it reads them, or searches them with
  `like`. A column of vectors and a distance in SQL could come later.

What has to be said with it: a companion that acts by itself follows
what it reads, and any text of a table can steer it. Access lists do
not exist yet to bound it; the three guards above are all there is.

The steps, in order. Each is merged once verified. Three were done on
26 November 2026:

- **Names**: DHCP asks for the name server, a question over UDP gives
  the address of a name, `net get` takes a name.
- **TLS**: a client for TLS 1.3 only, written here as a machine fed
  with bytes like `tcp.rs`, in a crate of its own (`tls/`) which holds
  the first outside code of XilaOS: the algorithms and the checking of
  certificates. `net get https://` asks a page through it. A machine
  whose processor has no source of chance of its own asks nothing
  through TLS. What it does and does not make sure of is in
  [Networking](10-networking.md#security).
- **HTTP as a service of the Internet speaks it**: a question with
  lines of its own (a key) and bytes that go along, answers that come
  in chunks, answers of any length up to 4 MB, and an answer that was
  cut short known from a whole one. `net post` tries it by hand; a
  megabyte from the model's server is read in a quarter of a second.

What the author decided for the companion itself, on 26 November 2026:
its key is kept apart and never shown (`key`, done that day; the disk
is not encrypted, so this is not a vault); one talks to it on a screen
of its own and with a command, `ask`; what is said is kept as long
lines of a table, of which SQL shows the first 48 characters, as for
`programs`; and it is called Xila.

What is left:

4. **Longer texts, and waiting without holding the system.** A text
   holds 48 characters today, and a command that waits for the network
   holds everything: a model that thinks for half a minute would stop
   the machine for half a minute.
5. **The companion itself**: its screen, the conversation, reading the
   tables.
6. **Acting, notes, events.**

A trial of TLS, thrown away afterwards, came before it was built: a
real handshake with the model's server and one question to the model
through it, and the cost for the kernel (some fifty outside crates,
400 KB, 7 to 26 ms of calculation for a handshake in QEMU).

### Loose ends of what exists

- Characters outside ASCII leave as `?` on the serial port and in the
  web pages.
- `net ping`, `net get` and `install from` hold the system while they
  wait: phase 3.
- A development mode. The serial shell is always open and may do
  everything; it is meant to be open in QEMU, or on request at start,
  and closed otherwise.
- A close comparison of the screens with the mock-up, and the
  touch-ups it calls for.

### Kernel

- Device interrupts (APIC, then MSI) and the APIC timer in place of
  the old one. Polling suits a disk, not a sound card.
- Waiting for an event (a key, a byte, a device) instead of waking at
  every tick.
- Reading the simple ACPI tables (processors, interrupts, the whole
  PCI bus); then power-off, battery and sleep, which need an ACPI
  interpreter.
- One shape for all drivers, attached by class of device, and
  `devices` as a system view.
- More than eight tasks; priorities, or at least a larger share for
  the interface.
- A fatal error shown on the screen, not only on the serial port.
- The serial port is tested by a byte that must come back, read at
  once: an imitated chip hands it back at once, a real one takes the
  time of one character. No real serial port has been tried.
- A log kept in the database and read after a restart, for trials
  that can leave the screen dark.
- The memory of the firmware's boot services taken back: the kernel
  only uses what was free at start (205 MB of 250 in QEMU).
- Memory protection: page zero and the kernel's code first, then user
  mode and a memory of its own for each program.
- Setting the machine's clock; time zone and summer time.
- More than one processor core.

### Drivers and hardware

- NVMe, and disks with 4 KB sectors: the disk of a recent PC.
- USB: the xHCI controller, then keyboards, sticks and Ethernet
  adapters. With NVMe, this is what a PC made after 2018 needs to run
  XilaOS at all.
- virtio-net and virtio-gpu, for virtual machines.
- Sound: Intel HD Audio.
- An interrupt for the network card: a frame waits up to a hundredth
  of a second today.
- On the T430: the brightness keys (they go through a circuit XilaOS
  does not drive) and turning the screen off.
- On the T430: the link stays at 100 Mb/s on a gigabit switch. Set
  aside by the author; to take it up again, show in `net registers`
  what the chip near the socket announces and what the switch
  announces to it (its registers 4, 5, 9 and 10).
- Far away: Wi-Fi, and a second processor family (RISC-V).

Each driver is written from the standard or the maker's manual, tested
in QEMU first, and then tried on real hardware, where it will fail in
ways the emulator never shows.

### Graphics and the interface

- The city seen in perspective with a camera that moves, as in the
  mock-up, instead of the fixed isometric view; commands and query
  results that move the camera and light the blocks they speak of.
- Movement in the interface outside the city (header, changes of
  screen).
- A virtual screen for each application, with its own theme; a screen
  that switches to pixels for a game. Something behind the `sound` and
  `irc` labels of the header.
- On the Intel chip: a hardware plane for sprites over the character
  grid, and hardware scaling for a low-resolution screen.
- Further on the chip: its 2D engine (little to gain at this size of
  screen: measure first), and its 3D engine for the city, which is a
  project of its own.

### Storage

- The mirror on two disks, as designed above; then four.
- A read of every page at regular intervals: `disk check` does it on
  demand, and holds the system while it reads.
- The checksum of a page kept in the page that points to it, against
  a write the disk loses silently.
- Snapshots: a table as it was at a past date.
- Pages merged when rows leave; today a leaf is given back only when
  it is empty.
- Large typed objects (music, pictures): a row holds 1000 bytes at
  most.
- Installing beside another system, in the free space of a disk.

### SQL

- Keys, indexes, constraints (`NOT NULL`, `UNIQUE`), `ALTER TABLE`.
  Only the `id` leads straight to a row today.
- Joins of more than two tables, and subqueries.
- `BEGIN`, `COMMIT`, `ROLLBACK` at the prompt.
- Date and time types.
- The fixed sizes: eight columns, 32 rows per result, 48 characters
  per text, 24 elements per calculation, one column in `GROUP BY`, one
  criterion in `ORDER BY`, eight rows per `INSERT`.
- A system view of the log: a line of the log is 80 characters and a
  text holds 48, so it waits for longer texts.
- Changing a setting with `update settings`.

### XilaScript

- A task of its own for a program, so that it does not hold the
  system.
- Parameters, and names that belong to a program.
- An editor, to change a program without typing it again.
- `insert into t select ...`, a `select` inside a calculation, loops
  over more than 32 rows.
- The values of before in `on update` and `on delete`.
- More rhythms from the clock: `every monday at 9:00`, `every month`.
- Screens in characters to enter data.

### Network

- TLS where XilaOS is called: `remote` and `web` are still in clear.
  TLS 1.2, for servers that have nothing newer; a session taken up
  again; asking whether a certificate was revoked.
- Names: addresses of sixteen bytes, an answer too long for UDP asked
  again over TCP, `net ping` and `install from` by name.
- Fragments, segments that arrive out of order, more than eight
  conversations, IPv6.
- The web pages: sorting, filtering, the rest of a long table, a page
  that brings itself up to date when the table changes; then an HTTP
  server for content kept in the database, perhaps a REST API.
- Events that go out: a durable queue written in the transaction of
  the change, sending rules kept in a table, webhooks and mail.
- A way for programs to use the network, and a stack for each zone.

### The encrypted protocol

Designed above, not started; the design of the channel waits for the
author's agreement. The order that was agreed:

1. the encrypted channel, in a library that every party shares;
2. the hub, first as a small program for another system built from
   that library, and the command that registers a machine with it;
3. the remote terminal, a client application for other systems, which
   replaces `remote`;
4. primary and secondary machines: the primary asks, a code is
   compared, a person accepts;
5. tasks handed by a primary to its secondaries, and their results;
6. a XilaOS without a screen, then the hub on it.

### The layers above

- **Programs.** The virtual machine, then zones with their public or
  private status and access lists, rights to listen, installing an
  application, supervised services, a vault for secrets.
- **Sound.** Mixing, MP3, ambiences that follow the mood.
- **Applications.** A spreadsheet, a database front end, a
  HyperCard-like, mail, a text web browser, IRC, a port of Doom.
- **Agents.** Companions that are written to and answer in writing,
  each a zone with its own rights on the data, drawn as pixel-art
  characters.

### Decisions still to take

- Which virtual machine.
- How an application is installed, and where it comes from.
- What exactly is taken from OS/400: typed objects, single-level
  storage, or both.
- One user, or several.
- Which states of the system are events the companion hears.
- How a HyperCard-like is driven with the keyboard alone.
- Where a tenth system view goes: the `data` plot of the city has
  room for nine.

### Set aside

- MBR partition tables: `install` writes a GPT.
- A client of the protocol inside a web browser.
- Anything between XilaOS machines over HTTP: that is the protocol's
  work.
- RAID 0.
