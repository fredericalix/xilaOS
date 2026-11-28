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
| `disk check`: every page of the tables read from the disk and checked (until 26 November 2026 a page already in memory was not read again) | yes | yes, on 27 November 2026: 64 pages of tables and indexes |
| SQL: `IN`, `BETWEEN`, `NOT LIKE`, calculations on functions over rows; rows reached by their `id`; `JOIN` and `LEFT JOIN` of two tables | yes | yes, on 27 November 2026 |
| What a column asks: `NOT NULL`, `UNIQUE`, `DEFAULT`, `CHECK`; `show table`; the format of the database in version 2, version 1 read as it is | yes, a power cut and a disk of version 1 included | yes, on 27 November 2026, on a volume made in version 3; no power cut |
| Indexes on one column: `CREATE INDEX ON t (c)`, used by `SELECT`, by joins and by `UNIQUE`; the format of the database in version 3 | yes, power cuts included | yes, on 27 November 2026: the same pages read as in QEMU (30 for the table, 11 by the index); no power cut |
| `ALTER TABLE`: a column added, dropped or renamed, a table renamed, what a column asks changed, checks added and dropped | yes, power cuts included | yes, on 27 November 2026; no power cut |
| `install` shows how far it is; dead keys of the `fr` layout; a quote inside a text | yes | yes, on 27 November 2026, but for the dead keys |
| LISTEN / NOTIFY | yes | yes |
| XilaScript: variables, blocks, stored programs, `on` and `every` programs | yes | yes |
| XilaScript compiled for the virtual machine, which runs it; what is wrong with the form of a program told when it is kept; loops over the rows of a stored table without a limit | yes | not tried |
| Programs as jobs that hold nothing: the screen and the serial port wait for a program while the system lives (`Esc`, `Alt`+`B`, Ctrl+C); `submit`, `jobs`, `job`; the programs that run by themselves as jobs; `job_log` | yes | not tried |
| Programs that take values (`program restock(item, count)`, `restock('cable', 5)`, `submit restock(...)`) and have names of their own | yes | not tried |
| Commands that wait for the network (`net ping`, `net name`, `net get`, `net post`, `install from`) as waits of a job: the system lives, several go on side by side | yes | not tried |
| Panels, the screens of applications: described on a table, kept and listed; opened as virtual screens that list their rows page by page, with a line to find; a form to add, change, display and remove a row through SQL, values to pick at `F4`; panels opened from a row on the rows that belong to it; programs run from a key or an option with the values of the row; function keys | yes | not tried |
| Clock, `now` and `today`, `every day at` | yes | yes |
| Settings kept between starts | yes | yes |
| QR codes for output | yes, read back by another reader | yes |
| Ethernet (Intel e1000 family), ARP, IPv4, ICMP, UDP, DHCP | yes | yes, after five trials |
| Names (DNS): `net name`, `net names`, `net get` by name | yes | yes, on 27 November 2026, asked of the name server the home router named; `net names` not tried |
| TLS 1.3, the side that calls: `net get https://`, checked against OpenSSL and servers of the Internet | yes | yes, on 27 November 2026: three servers of the Internet, with what its processor draws and the time its clock tells |
| HTTP questions with lines and bytes of their own (`net post`), answers read whole: a said length, chunks, to the end | yes | `net get` only, on 27 November 2026; `net post` not tried |
| Keys to services (`key`): kept on the disk in a table SQL does not know, shown by nothing | yes | yes, on 27 November 2026: a key kept, used, and not among the tables the companion listed; still there after `install system` and a restart |
| What happens to the machine said as events: `start`, `network`, `disk`, `tasks`, `memory` | yes; `disk` and `memory` by unit tests only | not tried |
| The companion asked by programs (`ask` in an `on` or `every` program), which is how it reacts to what happens | yes | yes, on 27 November 2026: an `on notify` program |
| The companion, first form: `ask` and a screen of its own, a model of the Internet that answers and reads the tables (`select`), what is said kept in `talk` | yes, against a made-up model and a real one | yes, on 27 November 2026, asked by a program: through a relay first, then with a key kept there, through TLS (see below); `ask` typed there with an answer not tried |
| The companion acts (`run`: one line at the prompt, written in `acts`), remembers (`remember`, the table `remembered`) and is stopped (`companion off`, kept between starts); what is refused to it | yes, against a made-up model and a real one; the refusals against the made-up one | yes, on 27 November 2026, through the relay; of the refusals only `web on`, which the real model tried; kept between starts not tried |
| `install`, `disk erase`, `remote on`, `web on` refused to a program that runs by itself; `disk erase yes` refused to the network session | yes | not tried |
| The table `companions`, which says where the key goes, changed at the machine only: not from the network prompt, not by a program that runs by itself | yes | yes, on 27 November 2026, for the network prompt |
| Accented letters between the font and the rest of the world (UTF-8): what is typed to the companion, what it answers, the web pages | yes | yes, on 27 November 2026, for what it answers (see below) |
| TCP, both sides | yes | yes, through the web pages |
| Prompt over the network (`remote`), not encrypted | yes | yes |
| Tables in a browser (`web`), read only, not encrypted | yes | yes |
| `install from` (TFTP), `tools/netboot.py` | yes | yes, again on 27 November 2026 with the kernel of that day |

Numbers at version 0.1: about 25,000 lines of Rust, 229 unit tests,
about 300 system checks. On 28 November 2026: about 44,000 lines, 428
unit tests, 580 system checks.

## Known limits of what is done

These are not bugs; they are where the first version stopped. What is
meant to change is in the [work plan](#work-plan) below.

**SQL.** Two tables in a join at most, no subqueries. No keys but `id`. An index is on one
column, finds one text or decimal but a span of whole numbers only, and is
not used by `update` and `delete`. The type of a column does not change. No `BEGIN` / `COMMIT`: one statement is one transaction.
Eight columns per table, 32 rows per result, 48 characters per text, no
date type. Details in
[The database and SQL](04-sql.md).

**XilaScript.** No editor (a program is retyped to be changed), no
`insert ... select`. A program takes values and gives none back: what
it makes goes into a table. A loop reads every row of a stored
table, but 32 at most of what is sorted, grouped or joined. A program
runs as a job that holds nothing, at the screen, at the serial port
and when it is submitted; one long instruction still holds the
system, and so does a program run from the prompt over the network or
for the companion. Details in
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

**Interface.** Three virtual screens (the overview, the boot log and the companion's), and one for each panel that is open, four at most. A panel lists the rows of its table and adds, changes and removes them through a form. The
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
| 5. Applications | Spreadsheet, database front end, a HyperCard-like, mail, text web browser, IRC, HTTP server | Not started |
| 6. Agents | AI companions with their own access rights to the data | A first companion answers, reads the tables, acts and remembers; it has no access rights |
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
  machine was chosen on 27 November 2026: one made for XilaOS around
  values and tables, and no other
  ([below](#the-virtual-machine-and-the-jobs)).
- **One language of the house.** XilaScript, for everything written
  on the machine itself: its words are English, one statement per
  line, blocks closed by `end`. It was interpreted until 27 November
  2026 and is compiled for the virtual machine since. The first notes had a second
  level, heavy programs (Doom, a spreadsheet engine) compiled from an
  existing language: with a machine of the house alone, that level is
  given up.
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

Chosen by the author on 23 November 2026. The T430 was out of reach
then, so these phases are the ones that can be built and verified in
QEMU alone; device interrupts and the drivers for recent PCs wait for
a real machine. Each numbered step is merged once its unit tests and
its checks in `tools/check.py` pass.

On 26 November 2026 the author put a first AI companion ahead of the
rest, and it was built that day; what is left of phase 2 waited for
it. Its steps are [below](#the-companion).

| Phase | Content | State |
|---|---|---|
| 1. Loose ends | The small things that were off; an apostrophe in a text; the dead keys of `fr`; a command that reads the whole database; `install` that shows it is writing | Done on 23 November 2026 |
| The companion | A first AI companion: a screen to write to it, and its reactions to what happens in the system. Names (DNS), TLS and HTTPS first, since the model it speaks through is reached over the Internet | Done on 26 November 2026, in QEMU; not tried on the T430 |
| 2. SQL | Done: `IN`, `BETWEEN`, `NOT LIKE`, calculations on functions over rows; a row reached by its `id` without reading the table; joins of two tables. Resumed on 27 November 2026 with what a column asks: `NOT NULL`, `UNIQUE`, `DEFAULT`, `CHECK`, kept in the catalog (format version 2; version 1 is read). Then indexes on one column, the same day (`CREATE INDEX ON t (c)`; format version 3), which `SELECT`, joins and `UNIQUE` use. Then `ALTER TABLE` (columns added, dropped, renamed; what they ask changed). Left: `BEGIN` / `COMMIT`; indexes for `UPDATE` and `DELETE`, on several columns, and a choice between two; a view of the log, which waits for texts longer than 48 characters | Under way |
| 3. XilaScript | An editor; `insert ... select`, the values of before, more rhythms. Parameters, names of a program's own and a program that holds nothing went to phase 5 on 27 November 2026 | After 5 |
| 4. The encrypted channel | Step 1 of the protocol, as a library tested on the development machine | Waits for the author's agreement on the design. Outside code is there since TLS, with the algorithms the channel needs |
| 5. The virtual machine | The machine, XilaScript compiled for it, and programs as jobs that hold nothing; with it what phase 3 had of parameters, names of a program's own and commands that wait | Designed on 27 November 2026 ([below](#the-virtual-machine-and-the-jobs)), put ahead by the author. The machine and the compiler, then jobs, were built that day, parameters and names of a program's own on 28 November; the commands that wait for the network on 28 November too: the phase is built. Then application screens, batch work and the scheduling of jobs, each with a design of its own |
| 6. The mirror | Two disks, repair by the checksum, degraded mode, rebuilding | After 5, or earlier if a decision is awaited |
| 7. A memory by meaning for the companion | What it remembers found by what it means and not only by its last lines: vectors kept with the facts, a search by distance | Asked for by the author on 28 November 2026, as the piece of work that follows the panels ([below](#a-memory-by-meaning-for-the-companion)). Not designed |

**Tried on the T430, and what still waits.** On 27 November 2026 the
T430 was started from its network cable with the kernel of that day.
That kernel found the tables its disk held, written in version 1 of
the format. Its system was replaced with the tables kept (`install
from`, `install system`), then its whole disk was taken again
(`install from`, `install n yes`, which showed how far it was), and
the machine started from that disk: the volume it holds since was
made in version 3.

The same day a script on the development machine typed at its prompt
over the network (`remote on`), with the statements and the answers
of `tools/check.py`: what a column asks, indexes (two tables of 800
rows, the pages read counted), `ALTER TABLE`, joins, programs,
`disk check` on the real disk, a ping, names asked of the home
router, and pages of three servers of the Internet through TLS. 48
of its 49 checks passed. The one that failed was a fault of XilaOS,
which no check had met: in a program that runs when something
happens (`on insert`, `on notify`...), the names `now` and `today`
were unknown. It was corrected that day.

The companion was tried there too, the same day. `key set` is refused
to the network prompt, since the key would cross the network as it
is, and the author chose not to type the key at the machine: so the
row of `companions` was pointed at a relay on the development machine,
in plain HTTP, which added the key and passed the question on to the
model through HTTPS. And `ask` is refused to the network prompt, so
the questions were asked by a program (`on notify`). The real model
read a table (three `select`, the sum right), made a table and filled
it (`run`, written in `acts`), kept a fact and said it again at the
next question, tried `web on` when told to and was refused, declined
to empty `acts`, and was stopped and had back (`companion off`, `on`).
Seventeen calls in all. Then the author had the key put on the T430
after all, from the development machine: a program written through
the network prompt ran `key set`, which the prompt itself refuses, so
the key crossed the local network in clear once. With `companions`
pointing at the model's own server again, a question went from the
T430 through TLS with the key kept there, and was answered (three
calls, a `select` among them).

Doing that showed two ways around what guards the key, both from the
network prompt: a program written there may run `key set`, and the
row of `companions`, which says where the key is sent, could be
changed there. The author chose to close the second and keep the
first: `companions` is now changed at the machine only.

At the end of that day the system on the T430's disk was replaced
once more (`install from`, `install system`) by the one that holds
those two corrections and the accented letters. After the restart
the key was still there, and a change to `companions` was refused to
the network prompt. The accents were not there at first: the author,
writing to the companion in French, still read `t?che`. The system
was not at fault. The rows of `talk` written before it held `?` where
accents had been lost, the last six turns go to the model with every
question, and the model wrote as it saw itself write: the same rows
copied into QEMU gave the same `?`, and a `talk` with nothing lost
gave accents. Each answer written so kept the fault going. With
`talk` emptied on the T430 the next answer came with its accents, on
the screen and in the table.

What still waits for the T430, since it cannot be done over the
network or was not tried: `ask` typed at the prompt (the author wrote
on the companion's screen and was answered there), accented letters
typed to the companion (`keymap fr`) and in the web pages, `now` in a
program that hears something, what happens to the machine said as
events, `net post`, the dead keys on the real keyboard, and a power
cut while tables with indexes change.

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

A fourth came the same day, the companion in its first form: `ask`,
at the screen or on the serial port; a model that reads the tables
with one tool, `select`, and cannot change anything; what is said kept
in the table `talk`, as long lines; who it is in the table
`companions`. The model is called a little at each tick, so nothing
waits for it but who asked. It is described in
[The companion](14-the-companion.md).

Its screen came next, the same day: `Alt`+`2`, where what is typed is
for the companion and what is said is written as a conversation.

Reacting came after: a program that runs by itself can ask the
companion (`ask`, with the values of what happened between braces),
and what happens to the machine is said as events that such a program
hears: its start, the network, the disk, the tasks, the memory.

Acting and remembering came last, the same day. The model has two
more tools: `run`, one line at the prompt, and `remember`, a sentence
kept in the table `remembered` and told to it again with every
question. Every line it runs is a row of the table `acts`;
`companion off` stops it, and stays in force through a restart;
`install`, `disk erase`, `remote on` and `web on` are refused to it.

Three things were added to what the author had decided, because the
guards did not hold without them. They are his to undo:

- More is refused to the companion than the four commands: `key`,
  `reboot`, `ask`, `companion on`, blocks, what needs the kernel while
  it is served, and any change to the tables `programs`, `acts` and
  `companions`. A program the companion wrote would have run without
  its refusals and could have turned it on again; the row of
  `companions` says where its key is sent; `acts` is the record.
- `install`, `disk erase`, `remote on` and `web on` are now refused to
  every program that runs by itself, whoever wrote it. Before, a
  program written at the prompt over the network was a way around the
  refusals of that prompt. A user's own `on` or `every` program can no
  longer open the pages for browsers by itself.
- `disk erase yes` was not refused to the prompt over the network,
  though `disk erase` was. It is.

What it is not: bounded. It may empty or drop any table of the user's
without asking, and nothing undoes it. See
[what holds it](14-the-companion.md#what-holds-it).

Two things of the plan were not needed in the form they had: texts
longer than 48 characters in SQL (what is said is kept as long lines
that the storage engine reads, as for programs; SQL still shows 48),
and a task of its own for what waits (the call to the model advances
at each tick of the interface task). Both stay in the lists below for
what else needs them.

A trial of TLS, thrown away afterwards, came before it was built: a
real handshake with the model's server and one question to the model
through it, and the cost for the kernel (some fifty outside crates,
400 KB, 7 to 26 ms of calculation for a handshake in QEMU).

### The virtual machine and the jobs

What the author decided on 27 November 2026:

- **Closer to an IBM mainframe and an AS/400.** Jobs that are
  scheduled; data imported, sorted and processed in batches;
  application screens to work on the data, as RPG has them. Four
  pieces of work, in this order: programs that hold nothing (jobs);
  application screens, the first of them to manage a stock of
  equipment; batch work; the scheduling of jobs. Each gets its own
  design; this one is the first.
- **A XilaScript program must no longer hold the system** while it
  runs.
- **At the prompt, a program that is run is waited for while the rest
  of the system lives**, as an interactive call on an AS/400: what it
  says is shown as it runs, a key stops it, another sends it to the
  background. `submit name` runs it as a batch job without waiting.
- **The virtual machine first**, rather than the interpreter made to
  run in steps.
- **The machine is one made for XilaOS, and the only one.** No
  WebAssembly. So no program compiled from C or Rust: a port of Doom
  has no path any more.
- **XilaScript is compiled for it.** It is still typed line by line
  at the prompt; a program that is kept is translated once.

The design, agreed in outline that day. Its first step, the machine
and the compiler, was built the same day (`core/src/code.rs`,
`machine.rs`, [XilaScript](05-xilascript.md#how-it-is-implemented));
the rest is not built:

**What the machine works on.** The values of the database as they are
(`Null`, a whole number, an exact decimal, a text), and three kinds of
objects: a *cursor* over a `select`, a *call* of a program with names
of its own, and later the *format* of a screen.

**Its instructions** speak of what the system has, not of what a
processor has. A program and what it becomes:

```
program restock                     0  open   c0
  for item in select name, qty      1  next   c0, else 4
      from stock where qty < 5      2  say    item.name
    say item.name                   3  jump   1
  end                               4  close  c0
end
```

(As built, an instruction holds numbers only, and reads the texts it
needs from its line: the SELECT of `open`, the name of the loop.)

The set, to begin with: keep a value under a name; say one; jump,
always or unless a condition holds; open a cursor, take its next row,
close it; call a program and come back; and `command`, which gives a line to the prompt's own
commands, the statements of SQL that change tables among them. Every
instruction keeps the number of the line it came from, so that a
failure still says `Stopped at line 3 of restock.`

- **Values are worked out by `expr.rs`**, which SQL uses too: the
  machine does not get an arithmetic of its own. An expression is
  read once, when its program is compiled, and the instruction keeps
  it read. That asks one new thing of `expr.rs`, which today takes
  the value of a name while it reads: a name that is looked up when
  the expression is worked out. The text of a statement of SQL is
  still read each time it runs.
- **One door to the system.** Whatever touches a table, an event or,
  later, a screen goes through one function that knows which job
  asks. The rights of zones will be checked there.
- **A `for` is a cursor**, read row by row, so its limit of 32 rows
  goes, and a job can be left between two rows of a large table. A
  cursor over a plain `select` of one table walks it by `id`, a few
  rows at a time, and keeps only the last `id` it gave: it sees rows
  as they are when it reaches them, changes made by others meanwhile
  included, but none added after it opened, so that a loop that adds
  to the table it reads ends. A `select`
  that sorts, groups or joins is worked out whole when the cursor
  opens, within the limits a result has today, until batch work gives
  sorting its room. The columns of the row stay names (`item.name`),
  which count among the 32 and keep the last row after the loop, as
  today.

**Compiling.** A program is translated when it is kept (the last
`end` of `program`, `on`, `every`), and the faults of its form are
told then: a block that is not closed, an `else` without its `if`, a
line that is no statement, an expression that cannot be read. What
cannot be known then (a name with no value yet, a table that is not
there) is told when it runs, as today. The lines stay in the table
`programs` and `show name` gives them back. The code is not kept on
the disk for now: it is made again when the job starts. What is typed
at a prompt goes the same way, so there is one way to run and the
interpreter of `lang.rs` leaves: a single line is one instruction,
run at once in the task of who typed it, as today, and takes no place
among the jobs (so `jobs` and `job 3 end` can always be typed); a
block typed there, and a program run by its name, are jobs the prompt
waits for.

**A job** is one run of the machine: its code, its calls, its names,
its cursors, who started it, and a state: ready, running, waiting
(for the kernel to serve a request; later for a screen), held, ended
(done, failed at a line, stopped).

- **In `core/`, with no heap**: `jobs::step` runs instructions of the
  ready jobs in turn and returns. Eight places for the jobs people
  start, each with a fixed room, and one more for those the system
  starts; with the eight taken by jobs that have not ended, one more
  is refused and the answer says so. A job has a number that counts
  up from the start of the machine, and stays in the list once ended
  until its place is needed, the oldest first. One instruction is
  whole: a statement is a
  transaction, which is what keeps a job from seeing half of what
  another changes.
- **A kernel task carries it**, as one carries the companion's bytes:
  it takes the lock, lets the jobs run for a slice of a few
  milliseconds, serves the request a job may have left, gives the
  lock back. The interface is never kept longer than a slice and one
  instruction. An instruction can be long (a `select` over a large
  table, `disk check`, `install`, `net get`): it still holds the
  system for that time, and the documentation says so.
- **Who started it stays with it.** Every instruction of a job runs
  as the session that started it, and with `System::acting` set if
  the companion did; a job submitted by a job gets the same. So what
  is refused to the network prompt, to a program that runs by itself
  and to the companion is refused to their jobs, and `submit` is no
  way around.
- **The prompt waits as the serial port waits for the companion
  today**: lock given back, asking at each tick what was said and
  whether it ended. The screen shows the lines as they come; Esc
  stops the job, Alt+B sends it to the background. The serial port
  writes them and ends with its status line when the job ends; a
  Ctrl+C byte stops it. The prompt over the network answers when the
  job ends, and its protocol does not change: a job waited for from
  there keeps the limits of a program nobody watches, so that it
  ends, and a line that comes from there meanwhile is refused with
  the number of the job. For long work there, `submit`, then `job n`.
- **Names.** As step 2 built it, a job had one set of names, which
  the programs it called shared, and a job that a prompt waited for
  saw and changed the names of that session. Step 3 changed that
  (below): a program has its own. What stays: a block typed at a
  prompt uses the names of that prompt, and sent to the background
  it leaves with a copy of them.
- **The programs that run by themselves become jobs** (`on`,
  `every`): made when their time comes, run one after the other in
  the order of what happened, as today, beside the jobs that people
  start. What they say still goes to the log screen under their name.
  They keep the limits that stop a program nobody watches (100 000
  statements, 10 000 rounds of a `while`); a job a person started
  loses them, since it can be stopped.
- **For a first form** (the author's choice when step 2 was planned,
  27 November 2026): what the prompt over the network and the
  companion run stays as it was after step 1, run at once to its
  end with the limits, the system held meanwhile. For long work from
  the network: `submit`. What follows of them is for later.
- **What the companion runs** (`run`) is a job it waits for before it
  answers the model, recorded in `acts` as today. What holds it stays
  whole: its job keeps the limits of a program nobody watches; a
  command that leaves a request for the kernel stays refused to it,
  at the door, though a job could now wait for the answer (it would
  give it `net get` and `net post`); and `submit`, `job n end`,
  `hold` and `release` are refused to it for now, so that nothing it
  started outlives its answer and `companion off` has nothing of its
  own to stop. Each gets its line in the tests of what is refused to
  it and its row in the table of `docs/14-the-companion.md`.
- **What a job nobody waits for says** is kept with it, its last
  lines, for as long as it stays in the list.

**Commands.** `submit name`; `jobs`, also a view for SQL (job,
program, state, started by, since, statements run); `job n` (its
state and what it said); `job n end`, `job n hold`, `job n release`.
A row is added to a stored table `job_log` when a job that nobody
waited for ends (program, started by, began, ended, how it went,
statements run): what ran last night is the first thing asked of a
machine that works in batches. Only the jobs people submit or send to
the background for now: a row for each run of an `on insert` program
would be a commit for each row added. Which runs of the programs that
run by themselves are recorded is for the design of scheduling. The
rows are written as those of `acts` are, not through `change.rs`, and
nobody is told of them, so that no program starts on its own record.
`jobs` is a tenth view, and the `data` plot of the city has room for
nine: the author decided on 27 November 2026 that it takes no block
there. A job is a block of the `tasks` plot, since it is what runs,
as high as what it has run.

**Parameters, and names of a program's own** (step 3; the author's
choices of 28 November 2026).

```
program restock(item, count)
    update stock set qty = qty + count where label = item
end

restock('cable', n + 1)
run restock('cable', 5)
submit restock('cable', 5)
```

- **Between parentheses**, as the functions of a calculation are
  written. A program that takes nothing is written and typed as
  before (`program report`, `report`); `report()` says the same.
- **Every parameter gets a value**: eight at most, none by default
  for now (that can come later without a kept program changing). A
  call that gives too few or too many is refused before any line of
  the program runs: `restock takes 2 values: item, count.`
- **A value given is a calculation**, worked out with the names of
  who calls, when the call is made: a job that is submitted leaves
  with its values. A `select` is kept with `let` first, as in any
  calculation.
- **A program gives nothing back.** What it makes goes into a table,
  or is said; `return` takes no value.
- **A program sees its own names only**: its parameters and what it
  makes with `let` or `for`. It neither reads nor changes the names
  of the prompt or of the program that called it, and its names are
  gone when it ends. So a program behaves the same typed at a
  prompt, submitted, started by another and, later, by a screen. The
  lines of a block typed at a prompt (`if`, `while`, `for` outside a
  program) still use the names of that prompt.
- **A program that runs by itself** is given its names by the system
  as before (`channel`, `id`, `new.column`, `text`): they are its
  parameters.
- **In a statement of SQL a column still wins over a name**, so a
  parameter named like a column of the table is not seen there.
- **What changes for what exists**: a program kept before that read a
  name of the prompt stops with `Unknown name`, and is typed again
  with a parameter; the row of a `for` in a program no longer stays
  at the prompt after it.
- **Kept and shown**: the parameters are line 0 of the program in the
  table `programs`, where a program that runs by itself has the line
  that says when. `programs` and `show` give them back, and a job is
  listed with the values it was given (`restock('cable', 6)`), in
  `jobs`, `job n` and `job_log`.
- **In the machine**: when a program is entered, the names of who
  called it are put aside in the room of the machine, and come back
  when it leaves, however it leaves.

**The commands that wait for the network** (step 4; the author's
choices of 28 November 2026). `net ping`, `net name`, `net get`,
`net post` and `install from` each held the whole system in a loop of
the kernel until their answer: a second, three, five of silence, a
minute.

- **A wait belongs to its job.** What was asked, how far it is and
  what came back are kept with the job that asked, not in the one
  place the kernel has for a request: the job is `waiting`, the
  others run. When the answer is there, who gives the job its turns
  says it where the job says, and the job goes on.
- **A line typed alone waits the same way** at the screen and at the
  serial port: it becomes a job that prompt waits for, so the system
  lives and `Esc` or Ctrl+C gives the question up. With no place left
  among the jobs it runs at once, as before, rather than be refused.
- **Really side by side.** Each job has its own conversation: pages
  (`net get`, `net post`) over TCP, which holds eight, one of them
  for the pages of browsers and one for the companion, so a ninth
  waits for one to end; pings, of which the network follows eight;
  names, of which it asks four at a time. `install from` is the one
  that is alone: there is one room in memory for a system that was
  fetched, and a second waits for it.
- **A task of the kernel carries the network** while something is
  asked: it reads the card and moves every question one step, then
  gives the lock back. At one step a tick `install from` would take
  most of a minute; so it does not rest while frames come.
- **A job that is stopped while it waits** hangs up.
- **What still runs at once** (the prompt over the network, a system
  with no rooms; the companion is given no command that needs the
  kernel) goes through the same
  questions, carried to their end without the lock being given back:
  the author's choice of 27 November for those two stands.
- **Not this step**: `install` writing a disk, `disk check`, a
  `select` over a large table. One instruction is still whole.
- The questions are in `core/` with their tests; the kernel reads the
  card, tells the time and keeps what needs its memory (a TLS client
  for each page asked in secret, the room of a fetched system).

**The steps.** Each is merged when its unit tests and its checks
pass.

1. Done on 27 November 2026: the machine and the compiler, for the
   XilaScript of that day, run to the end where the interpreter ran,
   with its limits. What it showed for the next step: the room of the
   machine (four units, four cursors, 250 KB) is inside `System`,
   which nine jobs could not be, so the rooms of jobs must come from
   the kernel; and the stack of the interface task has little to
   spare under a `SELECT`, so nothing large may stand on it while
   instructions run (a `SELECT` typed at the screen froze the system
   until that was seen to).
2. Done on 27 November 2026: jobs. The machine runs in turns, in
   rooms the kernel makes in its memory; the screen and the serial
   port wait for a program while the system lives (`Esc`, `Alt`+`B`,
   Ctrl+C); `submit`, `jobs`, `job`; the programs that run by
   themselves are jobs, one after the other; `job_log`. In QEMU a
   `while` goes round about 440 000 times a second. Left as they
   were, by the author's choice: what the prompt over the network and
   the companion run, at once and with the limits. What it showed for
   what comes next: a submitted program has names of its own and no
   way to be given any, so parameters (step 3) are what makes
   `submit` useful for more than programs that read everything from
   tables; a line that a command of a job types (`qr`) still runs to
   its end inside that instruction. A review of the whole found, and
   it was closed, that a job could stop itself from one of its own
   lines and stop the kernel with it, that the chain of programs that
   start each other was not cut through `submit`, and that a program
   by the clock that took longer than its rhythm kept the others from
   running.
3. Done on 28 November 2026: parameters, and names that belong to
   one program, as chosen with the author that day (above). The
   names in use are read through one door (`System::names`); the
   machine keeps those of who called a program aside in its room,
   and a job nobody waits for keeps its own with it, so that it
   never touches those of a prompt. What it showed: with those names
   a `System` is 454 KB, more than the unit tests could stand a few
   of on a stack of 2 MB, so they are given 8 MB; and a program that
   gives nothing back says what it found only through a table or
   `say`, which the screens will have to live with or change.
4. Done on 28 November 2026: the commands that wait for the network
   as waits of a job, as chosen with the author that day (above).
   The questions are in `core/src/waits.rs`; the task `jobs` reads
   the card while something is asked. What it showed, in the checks in
   QEMU and not in the unit tests: the network kept one packet
   waiting for the address of a card, for ever when no card answered,
   which pings side by side could not live with (it keeps four now,
   a second each); and a job must wait until its answer is said, not
   only until it has come.

Then the screens, and the stock of equipment: [below](#application-screens-panels).

### Application screens: panels

What the author decided on 28 November 2026, the virtual machine
being built:

- **A screen of an application is described on a table**, and
  programs are hooked to it where something of its own is to happen.
  The system gives the list, the search, the form to add and change,
  the removal. Not a format that a program drives line by line, as
  RPG does: that may come later, for what a table does not describe.
- **The word is `panel`** (`screen` is the command of the virtual
  screens, and `screens` their view): `panel NAME on TABLE ... end`,
  kept in the stored table `panels`; `panels` lists them, `show panel
  NAME` gives one back, `drop panel NAME` removes it, and its name
  opens it.
- **A panel that is open is a virtual screen of its own**, beside the
  overview, the log and the companion's: `Alt` and its digit, and the
  menu, lead to it. Four may be open at one time.
- **Function keys, as on a 5250**, recalled on the last rows: `F3`
  leaves, `F6` adds, `F12` cancels, `Enter` goes, `PgUp` and `PgDn`
  turn the pages; in a list a digit typed before a row acts on it
  (`2` change, `4` delete, `5` display). `F4` shows the values a
  field may take.
- **A list is in the order its rows were made**, page by page, with
  no limit, and a line to find: what is typed there keeps the rows
  that hold it. Sorting waits for batch work.
- **The first application is a stock**: things followed one by one
  (`items`: label, category, serial, place, state, since, note) and
  articles followed by their quantity (`articles`: label, category,
  place, qty, least, note), whose entries and exits are rows of
  `movements` (article, day, qty, why) typed in a panel opened from
  the article; a program `on insert into movements` keeps the
  quantity. `categories` (name) says what a category may be.
- **The application is a text of the repository**, statements that
  make its tables, panels and programs, sent to a machine through its
  prompt over the network and played by `tools/check.py`. Nothing of
  it is in the kernel.

The design:

```
panel articles on articles
    title 'Articles'
    list label, category, qty, place
    field label    'Label'
    field category 'Category' among select name from categories
    field qty      'In stock' shown
    field place    'Where it is'
    option 8 'Moves' open moves where article = id
    key f8 'Count' run recount(id)
end
```

- **A panel is an object of the system as a program is**: lines kept
  in a table, typed at a prompt as a block, checked when its `end`
  comes and not kept when its form is wrong. The companion may not
  change the table `panels`, no more than `programs`. A panel opens
  at the screen of the machine only: not at the serial port, not at
  the prompt over the network.
- **`list`** names the columns of the list; **`field`** a column of
  the form, with its label; `shown` makes it one that is not typed;
  `among` says what it may hold, values written there or a `select`
  of one column. Without any `field`, every column is one, under its
  name.
- **What is written goes through the statements of SQL**
  (`change.rs`): values by default, checks and what a column asks
  hold, those who listen to the table hear of it and the programs
  `on insert` run. What the table refuses is said on the message
  line, in its own words.
- **`option N 'label' open PANEL where COLUMN = id`** opens another
  panel on the rows that belong to this one, where a row that is
  added gets that value; **`option N 'label' run PROGRAM(values)`**
  and **`key fN 'label' run PROGRAM(values)`** run a program as a job
  the panel waits for, with values of the row; the last line it says
  is the message.
- **Limits said beforehand**: seven fields (a table has eight
  columns), 48 characters a text, no sorting, four panels open.

In four steps, each merged when its unit tests and its checks pass:

1. Done on 28 November 2026: the panel as an object, the function
   keys, the list in its virtual screen, the line to find
   ([Panels](15-panels.md)). What it showed for the steps to come:
   the stack of the interface task has nothing to spare under a key
   or a command (a page read from there froze the system in QEMU,
   twice, and no unit test saw it), so what a panel reads or writes
   is asked for by a key and done at the tick of the interface, with
   what is large kept in the interface's own memory; and a panel that
   will run programs at a key runs them for who sits at the machine,
   whoever wrote the panel, the prompt over the network included.
2. Done on 28 November 2026: the form: display, add, change, remove;
   `F4` and `among`. What is written goes through the statements of
   SQL, at the tick of the interface. What it showed: what is typed
   in a field must be made one value by the form itself before it is
   put in a statement (a number written as it was typed changed
   another column, and with an unclosed quote every row), and a
   change must write only what was changed, since a form shows less
   of a row than the row holds.
3. Done on 28 November 2026: panels opened from a row, and programs
   on a key or an option, run as jobs for who sits at the machine.
4. The stock: its text in the repository, the tool that sends it, its
   chapter of the documentation.

### A memory by meaning for the companion

Asked for by the author on 28 November 2026: the piece of work that
follows the panels, before batch work and the scheduling of jobs. It
gets a design of its own, with the author, before anything is built.

**What is there today.** The companion remembers through three plain
tables that are read again at every question: `remembered` (a fact of
200 characters at most, added by its tool `remember`), of which the
last twenty are told to the model; `talk`, of which the last six turns
are sent; `acts`, the record of what it ran. What is older is not
lost, but the model sees it only if it thinks of asking for it with
`select`, and it then looks for words (`like`), not for what is meant.
There is no vector and no search by distance anywhere in the system.

**What is asked.** That what the companion is told of its memory be
what bears on the question, however old, found by its meaning.

**What the design has to settle**, none of it decided:

- **Who makes the vectors.** A model of the Internet, reached as the
  companion's own is (one more call for each fact that is kept and
  for each question, each with its cost, and the text of the fact
  leaves the machine once more), or something small enough to run on
  the machine itself, which has no vector instructions in its kernel
  today.
- **Where a vector is kept.** A value of a table is a number or a
  text of 48 characters and a table has eight columns: a vector of a
  few hundred numbers is neither. A kind of column of its own in the
  storage engine, or a table apart that only the companion reads, as
  `kept_keys` is apart.
- **How it is searched.** By reading them all, which a few thousand
  facts allow, or through an index made for distances.
- **What is given a vector**: the facts of `remembered` only, or the
  turns of `talk` too.
- **What the model is then sent**: the nearest facts beside the last
  ones, how many, and what it is told of where they come from.
- **What the user sees and holds**: the tables stay the user's to
  read and to empty; a fact that is removed must take its vector with
  it; and what leaves the machine to become a vector is said, as what
  is not encrypted is said.

### Loose ends of what exists

- Characters outside ASCII leave as `?` on the serial port and in QR
  codes. (The companion and the web pages read and write them as
  UTF-8 since 27 November 2026.)
- What the prompt over the network and the companion run still holds
  the system, and so does what the first asks of the network.
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

- References from a table to another, a change of the type of a
  column, indexes and `UNIQUE` over several columns, an index that finds the texts that
  start with something, a choice between two indexes, indexes used by
  `UPDATE` and `DELETE`.
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
  over more than 32 rows of what is sorted, grouped or joined; a loop
  that goes by an index when its `select` names one (it reads the
  whole table by `id` today): with batch work, the author decided,
  when large tables come and are measured.
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
  HyperCard-like, mail, a text web browser, IRC.
- **Agents.** Companions that are written to and answer in writing,
  each a zone with its own rights on the data, drawn as pixel-art
  characters.

### Decisions still to take

- How an application is installed, and where it comes from.
- What exactly is taken from OS/400: typed objects, single-level
  storage, or both.
- One user, or several.
- How a HyperCard-like is driven with the keyboard alone.
- Where a system view goes that is not `jobs` (which shows in the
  `tasks` plot): the `data` plot of the city has room for nine.

### Set aside

- MBR partition tables: `install` writes a GPT.
- A client of the protocol inside a web browser.
- Anything between XilaOS machines over HTTP: that is the protocol's
  work.
- RAID 0.
