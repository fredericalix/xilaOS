# Status and roadmap

XilaOS was built on 18 and 19 November 2026 and development stopped
there. It is a hobby project that reached a working first layer of a
much larger plan. This chapter says plainly what is done, what is not,
and what was designed on paper for whoever wants to take ideas from it.

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
| LISTEN / NOTIFY | yes | yes |
| XilaScript: variables, blocks, stored programs, `on` and `every` programs | yes | yes |
| Clock, `now` and `today`, `every day at` | yes | yes |
| Settings kept between starts | yes | yes |
| QR codes for output | yes, read back by another reader | yes |
| Ethernet (Intel e1000 family), ARP, IPv4, ICMP, UDP, DHCP | yes | yes, after five trials |
| TCP, both sides | yes | yes, through the web pages |
| Prompt over the network (`remote`), not encrypted | yes | yes |
| Tables in a browser (`web`), read only, not encrypted | yes | yes |
| `install from` (TFTP), `tools/netboot.py` | yes | yes |

Numbers at the end: about 25,000 lines of Rust, 229 unit tests, about
300 system checks.

## Known limits of what is done

These are not bugs to fix; they are where the first version stopped.

**SQL.** No joins, subqueries, `IN`, `BETWEEN` or `NOT LIKE`. No
indexes, keys or constraints; every query reads the whole table. No
`ALTER TABLE`. No `BEGIN` / `COMMIT`: one statement is one transaction.
Eight columns per table, 32 rows per result, 48 characters per text, no
date type. An aggregate cannot be part of a calculation
(`sum(a) / count(*)` is refused; `avg(a)` exists). Details in
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

**Network.** No DNS, no TLS, no IPv6, no fragments, eight TCP
conversations, no out-of-order reassembly. **Nothing is encrypted and
nothing is authenticated beyond a code sent in clear.** Details in
[Networking](10-networking.md).

**Hardware.** One real machine. PS/2 keyboard only, no USB, no NVMe, no
sound, no Wi-Fi. Details in [Hardware and graphics](11-hardware.md).

**Interface.** Two virtual screens (the overview and the boot log). The
`sound` and `irc` labels of the header come from the mock-up and have
nothing behind them.

## Small things that are off

Found while writing this documentation, and left as they are:

- Network commands that fail after being served (`net ping` with no
  answer, `net get` with a 404, `install from` a silent server) say so
  in words but still end with `#ok` on the serial port.
- `qr` with a command that says nothing prints a message and reports
  success.
- The rows that can be followed by typing their number are shared by
  all sessions: a list shown on the serial port replaces the one the
  screen could follow.
- The `about` column of `tables` is cut at 48 characters.
- The `frames sent` row of `net` can lag by up to a second.

## The plan that was not built

The system was meant to grow in layers. Layer 0 (the base) and layer 1
(the database) are mostly there, with a first part of the network.

| Layer | Content | State |
|---|---|---|
| 0. Base | Boot, memory, interrupts, tasks, screen, keyboard, interface | Done, without memory protection |
| 1. Database | Storage, tables, SQL, transactions, LISTEN / NOTIFY | Done in a first form; no typed large objects, no access rights |
| 2. Programs | A virtual machine, zones, public / private status, ACLs, installing applications, supervised services, a vault for secrets | Not started, except XilaScript and scheduled programs |
| 3. Network | Ethernet, TCP/IP, DNS, TLS, a stack per zone, webhooks and mail with a durable queue | Ethernet and TCP/IP only |
| 4. Sound | Audio driver, mixing, MP3, ambiences | Not started |
| 5. Applications | Spreadsheet, database front end, a HyperCard-like, mail, text web browser, IRC, HTTP server, a port of Doom | Not started |
| 6. Agents | AI companions with their own access rights to the data | Not started |
| 7. Later | Wi-Fi, USB, RISC-V | Not started |

### The intentions behind it

These were the author's decisions for the whole system. They explain
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
  machine itself was never chosen: WebAssembly, or one made for XilaOS
  around objects and tables.
- **Two levels of language.** Heavy programs (Doom, a spreadsheet
  engine) compiled for the virtual machine from an existing language;
  and the house language, XilaScript, interpreted, for everything
  written on the machine itself. Its words are English, one statement
  per line, blocks closed by `end`.
- **An application is a zone**, isolated from the others, in the manner
  of FreeBSD jails or illumos zones, with a public or private status
  and access control lists. Private by default. Isolation was to come
  from the virtual machine rather than from a hypervisor.
- **Events go out.** LISTEN / NOTIFY was meant to feed webhooks and
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
designed, the day before the project stopped, to replace the clear-text
`remote` prompt and to let one machine hand work to others:

- three kinds of parties speak it: a XilaOS instance, a hub on the
  Internet that keeps a directory and routes sealed envelopes, and a
  client application for other systems;
- every party opens one outgoing TCP link to the hub; two parties then
  open an end-to-end channel through it that the hub cannot read;
- at a first meeting a person compares a short code on both screens and
  accepts on the keyboard; afterwards the two recognise each other by
  their keys;
- the handshake follows the published Noise framework, with the
  algorithms taken from established Rust libraries (the first outside
  code the kernel would have had), `no_std` and without allocation, as
  a state machine fed with bytes like `tcp.rs`;
- a channel closes at the first message that does not verify; there is
  no mode without verification, not even for tests.

It was reviewed once by a second AI instance asked to break it, which
found one serious flaw (only one side confirmed the code) that the
design then fixed. It was never reviewed by a human specialist and none
of it was implemented. The steps after it (the hub, the remote
terminal, primary and secondary machines, distributed tasks, a XilaOS
without a screen) were only outlined.

### Where the original notes are

The project's working documents were written in French: the vision,
the reasoning behind each technical choice, the work plan with its
journal, the language reference, the hardware notes and the protocol
design. This English documentation replaces them and sums up what they
held: the intentions and the unbuilt designs above come from them. The
notes themselves, and the history of the two days of work, are not part
of this repository, which starts from the finished state.

## If you want to take it further

In rough order of how much each would unlock:

1. **A `devices`-driven driver model and device interrupts** (APIC,
   MSI): polling suits a disk, not a sound card.
2. **NVMe and USB** (xHCI, then keyboards and storage): what a PC made
   after 2018 needs to run it at all.
3. **Indexes and joins** in the SQL engine, and a `log` table.
4. **Tasks for programs**, so that a XilaScript program does not hold
   the system.
5. **The virtual machine**, which everything about applications, zones
   and rights depends on.

Each driver should be written from the standard or the maker's manual,
tested in QEMU first, and then tried on real hardware, where it will
fail in ways the emulator never shows.
