# Architecture

XilaOS is about 36,000 lines of Rust in four crates, plus 3,200 lines
of Python and shell for building and testing. No boot loader and no C
are linked in. One crate, `xila-tls`, rests on libraries from the
registry, for the arithmetic of TLS and the checking of certificates;
nothing else does.

```text
 ┌──────────────────────────────────────────────────────────────────┐
 │ kernel/   xila-kernel   4,700 lines   x86_64-unknown-uefi only   │
 │ the machine: boot, interrupts, tasks, memory, drivers, screen    │
 └───────────────┬──────────────────────────────────┬───────────────┘
                 │ scancodes, bytes, frames, ticks  │ a Disk
                 ▼                                  │
 ┌────────────────────────────────────────────┐     │
 │ core/   xila-core   18,200 lines           │     │
 │ all the logic, no hardware, no heap:       │     │
 │ interface, shell, SQL, XilaScript, events, │     │
 │ network stack, TCP, partition tables, ...  │     │
 └───────────────┬────────────────────────────┘     │
                 │ tables, rows, transactions       │
                 ▼                                  ▼
 ┌──────────────────────────────────────────────────────────────────┐
 │ db/   xila-db   4,300 lines                                      │
 │ the storage engine: copy-on-write B-trees on pages of a volume   │
 └──────────────────────────────────────────────────────────────────┘
```

`core/` also uses `tls/` (`xila-tls`, 1,300 lines without its tests):
TLS 1.3 for the side that calls, with no hardware and no network under
it, as `tcp.rs` is. It is described with the network, in
[Networking](10-networking.md#tls).

| Crate | Builds for | Allocates | Tested by |
|---|---|---|---|
| `xila-db` | host and UEFI, `no_std` + `alloc` | yes | 36 unit tests, power cuts included |
| `xila-tls` | host and UEFI, `no_std` | not itself; the `rsa` library it uses does, while it checks a signature | 14 unit tests against a server played by the tests, then OpenSSL in `tools/check.py` |
| `xila-core` | host and UEFI, `no_std` outside tests | no: fixed-size buffers only (`System::db` aside) | more than 200 unit tests |
| `xila-kernel` | UEFI only | yes, through its own allocator | `tools/check.py`: more than 300 checks on the whole system in QEMU |

## The rule that shapes everything

**Whatever can be decided without touching the machine belongs in
`core/`, with unit tests.** The kernel is kept to what needs the
hardware: it reads a scancode, `core` decides what the key means; it
receives an Ethernet frame, `core` decides what to answer; it reads 32
registers of the graphics chip, `core` says what they mean.

This is what made it possible to build the system mostly by running
tests on the development machine. The network stack, TCP, the SQL
engine, the partition table and FAT code, the QR encoder, the decoding
of the Intel chip's registers: all of it runs under `cargo test` with
no emulator. Where `core` needs something from the machine it asks
through a small trait that the kernel implements and the tests fake:

| Trait | In | The kernel gives it | The tests give it |
|---|---|---|---|
| `xila_db::Disk` | `db/src/disk.rs` | a virtio disk, a SATA partition | memory; a disk whose power can be cut |
| `net::Wire` | `core/src/net.rs` | the e1000 card | a list of frames |
| `intel::Chip` | `core/src/intel.rs` | the chip's real registers | a chip made up from the register description |
| `install::Sectors` | `core/src/install.rs` | a SATA disk | a buffer |
| `shell::Out` | `core/src/shell.rs` | the screen's terminal, the serial port, the network answer | a list of lines |

The second rule is that `core` has no heap. Every buffer has a fixed
size: `FixedStr<N>` for text, arrays for tables and expression trees.
That is where most of the system's limits come from (48 characters per
text value, 32 rows per result, 8 columns), and it is also why nothing
in `core` can run out of memory half way through a command. Only the
storage engine allocates.

## From a key to the screen

```text
 keyboard controller ──scancode──► keys::Decoder ──KeyEvent──► Ui::key
                                                                  │
 serial port ──line──────────────────────────────────────────┐    │ Enter
 UDP port 4000 ──line (remote prompt)─────────────────────┐  │    │
                                                          ▼  ▼    ▼
                                             shell::exec(&mut System, line, &mut dyn Out)
                                                          │
                                 lang: let, say, if, while, for, program, on, every
                                                          │ anything else
                                                          ▼
                                                   shell::command
                                  ┌───────────────┬───────┴───────┬──────────────┐
                                  ▼               ▼               ▼              ▼
                              sql::select   change::change    a command     a Request
                              (read)        (one transaction) of the shell  for the kernel
                                  │               │               │              │
                                  └──── Out: lines and Tables ────┘        main.rs: serve
                                                  │
              screen: a Term (scrollback) ◄───────┴──────► serial: text, #table ... #end, #ok
                        │
                  Ui::render ──► Grid of 80x50 Cells ──► fb::Screen::compose ──► show
```

- **`shell::exec`** is the single entry point. Whoever calls it first
  sets `System::session` (screen, serial or network), because variables,
  open blocks and LISTEN subscriptions are per session.
- **`System`** (`core/src/shell.rs`) is the state commands may read and
  change: theme, mood, keyboard layout, the database (`System::db`), the
  network stack (`System::net`), events, variables, what the kernel
  copied in about tasks and memory, and the slot for a kernel request.
- **`Out`** is where output goes. A command never knows whether it
  writes to the screen or to a serial line; list-like results always go
  through `shell::Table`, which the screen aligns in columns and the
  serial port sends as tab separated rows.
- **`Ui`** (`core/src/ui.rs`) owns the character grid and one terminal
  per virtual screen. `Ui::render` rewrites the whole grid from the
  state on every change; the grid records which cells really changed,
  and only those are drawn.

## The modules of `core`

| Module | Lines | What it is |
|---|---|---|
| `shell.rs` | 3,167 | `System`, `exec`, every command, `Table`, the help texts. |
| `lang.rs` | 1,499 | XilaScript: blocks, the table of programs, `on` and `every`. |
| `code.rs` | 431 | XilaScript compiled: the instructions, the compiler, the faults of form. |
| `machine.rs` | 921 | The virtual machine that runs them: units, programs three deep, loops over rows; turns, and the rooms it runs in. |
| `jobs.rs` | 897 | Jobs: programs that run turn by turn as who started them, what they said, `job_log`. |
| `tcp.rs` | 1,305 | TCP as a state machine with no hardware. |
| `net.rs` | 1,015 | Ethernet, ARP, IPv4, ICMP, UDP, the DHCP client. |
| `sql.rs` | 1,523 | Lexer, `SELECT`, values, the `Source` trait, the cursor a loop reads its rows through. |
| `intel.rs` | 936 | The Intel graphics chip: what its registers say; page flipping; brightness. |
| `ui.rs` | 855 | The interface: header, title bar, screens, menu, terminal view. |
| `expr.rs` | 1,350 | Expressions: parsing into a small tree, evaluation per row; the pool where a compiled program keeps its own. |
| `qr.rs` | 673 | QR codes: encoding, error correction, painting. |
| `change.rs` | 1,550 | `CREATE TABLE` and what its columns ask, `ALTER TABLE`, `DROP TABLE`, indexes, `INSERT`, `UPDATE`, `DELETE`. |
| `city.rs` | 529 | The city: the scene from the system state, the isometric drawing. |
| `gpt.rs` | 529 | GPT partition tables: reading, describing, the layout `install` writes. |
| `companion.rs` | 900 | The companion: what is sent to its model, what is made of the answer, what it runs and what is refused to it, what is kept. (Its tests are 450 lines more.) |
| `watch.rs` | 145 | What happens to the machine, said as events on five channels. |
| `dns.rs` | 520 | Names: the question to a name server, its answer, the names kept. |
| `json.rs` | 250 | JSON written into a buffer and read where it lies, with no tree. |
| `secrets.rs` | 100 | Keys to services, in a table SQL does not know. |
| `web.rs` | 517 | The read-only pages of tables for a browser. |
| `remote.rs` | 464 | The prompt over the network: the code, the message format. |
| `grid.rs` | 406 | The grid of cells, change tracking, choosing a screen mode. |
| `tftp.rs` | 365 | Fetching a system from another machine. |
| `events.rs` | 355 | LISTEN / NOTIFY: sessions, channels, queues. |
| `mem.rs` | 333 | Free pages and the heap (used by the kernel). |
| `fat.rs` | 331 | The FAT32 start-up partition, computed sector by sector. |
| `install.rs` | 281 | Writing XilaOS on a disk, and reading it back. |
| `font.rs` | 263 | The 8x8 font of V2_OS. |
| `settings.rs` | 262 | The settings kept from one start to the next. |
| `clock.rs` | 251 | Dates and times from the battery clock. |
| `keys.rs` | 237 | Scancodes to key events, `us` and `fr` layouts. |
| `catalog.rs` | 290 | The system views: tables that show live state. |
| `ask.rs` | 658 | One HTTP question over TCP, plainly or through TLS, and its answer read whole. |
| `text.rs` | 189 | `FixedStr<N>` and helpers. |
| `line.rs` | 165 | The line editor. |
| `term.rs` | 110 | A terminal: a scrollback of rows. |
| `theme.rs` | 110 | Color roles and the themes. |

## How the pieces fit

**Text** is bytes in the character set of the V2_OS font (ASCII below
0x80), not UTF-8. It is `&[u8]` everywhere: `b"..."` literals and
`FixedStr<N>` buffers. Where text meets the rest of the world, which
writes UTF-8 (a model of the Internet, a browser), `chars.rs` is the
way between the two: the number Unicode gives a letter of the font,
and what the font shows for a character of Unicode, its own letter or
the nearest thing it has.

**Display.** A `Cell` stores a character, two color *roles* and effect
bits (big logo letters, rounded bar ends, pulsing cursor). Colors come
from the theme at drawing time, so changing the theme is only a
redraw. The kernel draws changed cells (16x16 pixels each at 1280x800)
into a picture in ordinary memory and copies the changed spans to the
screen. Details in [Hardware and graphics](11-hardware.md).

**The city.** `city::scene(&System)` turns the system state into six
plots of blocks with heights; `City` animates and draws them into a
640x176 picture, which the kernel enlarges over the top rows of the
overview screen. Only real state is shown: a block is added by adding
to `scene`. The `city` command returns the same scene as a table.

**SQL.** `sql::select(&dyn Source, query)` parses and runs a `SELECT`
and returns a `shell::Table`. A `Source` describes tables by name and
scans their rows; `System` is the only one. It has two kinds of tables:
views of live state (`catalog::TABLES`: `tasks`, `memory`, `city`...)
and the stored tables of `System::db`. `change::change` runs the
statements that write: one statement is one transaction. See
[The database and SQL](04-sql.md).

**The language.** Every line goes to `lang` first. A line that opens a
block is collected until its `end`. Lines are then compiled (`code.rs`)
and run by the virtual machine (`machine.rs`, whose room is `System::vm`):
`let`, `say`, the blocks, `run` and `return` are instructions of their
own; everything else is the instruction `command`, which goes to
`shell::command`, so every command is a statement. Programs are rows of
the stored table `programs`. A program runs as a job (`jobs.rs`): the
machine gives it turns in a room of its own, so that it holds nothing.
See [XilaScript](05-xilascript.md).

**Events.** A session listens to channels. `change` notifies the
channel named after the table for each committed change. `Ui::tick`
prints the screen's events in its terminal, the serial task writes its
own before the status line, and a second queue feeds the `on` programs.

**Kernel requests.** A command cannot call the kernel. It leaves a
`shell::Request` in `System`; the task that ran it calls `serve`, and
when there is an answer, `shell::served` words it. See
[The kernel](07-kernel.md).

**Shared state.** `Ui`, `System` and the screen live behind one lock.
A task locks it, does its work, unlocks, then sleeps one tick.

**Stack use.** Tables and query buffers are large values on the stack
(a `SELECT` takes about 50 KB). Task stacks are 256 KB for that reason.
A running program keeps its lines and its rows in the room of the
virtual machine, `System::vm`, not on the stack; what runs one
instruction stays small there, since every command of a program is
above it.

## Where to add things

| To add | Do this |
|---|---|
| A command | A function in `shell.rs`, an arm in `command`, its name in `NAMES` (kept sorted, for completion) and in `HELP_COMMANDS`, a unit test, then a check in `tools/check.py`. |
| A statement of the language | An instruction in `code.rs` (`Op`, and its line in `translate`) and what it does in `machine::step`, with its word in `shell::LANGUAGE` and its line in `HELP_LANGUAGE`. |
| A system view | A `TableDef` in `catalog::TABLES` and an arm in `scan`. |
| A block of the city | An entry in `city::scene`. |
| A setting kept between starts | A field of `settings::Kept`, a row in `rows`, an arm in `take`. |
| A driver | Read [Hardware and graphics](11-hardware.md) first. Decoding goes in `core/` behind a trait; the kernel finds the device on the PCI bus and gives the trait real registers. |

## What the design keeps out

XilaOS is not a Unix, on purpose. There are no files, no `fork`, no
POSIX, no Unix shell. The model is OS/400: a database at the core of
the system, with data shared between applications. Three more parts of
that model are designed and not built yet: programs compiled for a
virtual machine rather than to native code, applications as isolated
zones with a public or private status and access lists, and typed
objects larger than a row. See
[Status and roadmap](13-status-and-roadmap.md).
