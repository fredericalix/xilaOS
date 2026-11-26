# Development and testing

XilaOS was developed almost entirely without looking at it. The same
shell runs on the screen and on the serial port, the serial port
answers in a format a program can read, and a small Python class drives
QEMU: it sends commands, presses keys, reads the characters on the
screen and takes screenshots. Every behavior that was added got a check
that way.

This chapter describes that loop. It is the part of the project most
worth reusing.

## The commands

```sh
cargo test -p xila-core -p xila-db           # unit tests of the logic and of the storage engine, on the host
cargo test -p xila-core ui::tests::menu      # one test, or a group, by name
tools/build.sh                               # kernel -> build/esp/EFI/BOOT/BOOTX64.EFI
python3 tools/check.py                       # the whole system in QEMU (after build.sh)
tools/run.sh                                 # build and open QEMU with a window; serial port on the terminal
tools/usb.sh                                 # build, then the image of a USB stick: build/xilaos-usb.img
python3 tools/netboot.py                     # serve the kernel to a PC that starts from its network cable
python3 tools/remote.py ADDRESS CODE [cmd]   # the prompt of a running machine, over the network
python3 tools/xq.py cmd:about type:help key:ret peek shot:build/shots/x   # drive it by hand
python3 tools/docs.py                        # this documentation as one PDF: docs/xilaos.pdf
```

| Tool | What it is |
|---|---|
| `tools/build.sh` | Builds the kernel with the local cargo when it has the UEFI target, else in the Docker image `xilaos-build`, which it makes if missing. |
| `tools/run.sh` | Builds, then QEMU with a window, 256 MB, a virtio disk on `build/data.img`, the serial port on the terminal. |
| `tools/xq.py` | The `Xila` class and a small command line over it. |
| `tools/check.py` | About 300 checks of the whole system, in more than thirty starts of QEMU. |
| `tools/usb.sh` | The image of a USB stick, made on the host by the code of `install` itself (`core/examples/usb.rs`). |
| `tools/netboot.py` | A proxy DHCP and TFTP server that hands the kernel to a PC starting from the network, and to `install from`. |
| `tools/remote.py` | The client of the prompt over the network. |
| `tools/qr.swift` | Reads a QR code in a screenshot with the reader of macOS, to check the `qr` command against code that did not draw it. |
| `tools/docs.py` | Assembles the chapters of `docs/` into `docs/xilaos.pdf` (needs pandoc and weasyprint). |

Notes on building:

- The kernel target is `x86_64-unknown-uefi`. `cargo build` for the
  whole workspace on the host does not work; always name the package.
- With Docker on macOS, a file written on the host can take a moment to
  appear in the container. `build.sh` runs `sync` first; if a build
  fails on a truncated file, build again.
- QEMU needs the UEFI firmware `edk2-x86_64-code.fd` that Homebrew's
  QEMU ships; `XILA_FIRMWARE` overrides the path.

## Three levels of tests

| Level | Where | What it proves |
|---|---|---|
| Unit tests of `xila-core` (more than 200) | next to the code, `#[cfg(test)]` | The logic: parsing, SQL results, the language, the interface's grid after a key, TCP against a simulated peer, partition tables, the QR encoder... |
| Unit tests of `xila-db` (24) | `db/src/lib.rs`, `db/src/testing.rs` | The storage engine, with the power cut at every write. See [The storage engine](08-storage.md). |
| System checks (more than 300) | `tools/check.py` | The built kernel, in QEMU with a real firmware: boot, keyboard, screen, disks, power cuts, network, install. |

The unit tests run in a few seconds on the host. They cover everything
that does not need the machine, which by design is most of the system
(see [Architecture](06-architecture.md)).

## The serial protocol

The serial port (COM1, 115,200 baud) carries the same shell as the
screen. What it writes is made for a program:

- Every command ends with a status line, `#ok` or `#error`, then the
  prompt `XilaOS|all:/>`. A command whose work the kernel does (`net
  ping`, `net get`, `install`, `qr`...) ends with `#error` when the
  kernel's answer is a failure: no answer to a ping, a page that is not
  there, a disk that refused.
- A table is `#table name<TAB>column<TAB>column...`, then one line per
  row with tabs between cells, then `#end`.
- Events the serial session listens to arrive as
  `#event channel<TAB>text` lines before the status line of its next
  command (an empty line is enough to ask). `#lost N` says that N
  events did not fit in the queue.
- While a block of the language is open, each line is answered by
  `#more` then `#ok`. Nothing runs until the last `end`.
- A fatal error of the kernel is `#fatal` and a message.

```text
XilaOS|all:/>tasks
#table tasks	id	name	state	ran
0	interface	ready	0.08 s
1	idle	ready	0.02 s
2	serial shell	running	0.00 s
#end
#ok
XilaOS|all:/>frobnicate
Unknown command: frobnicate
#error
XilaOS|all:/>
```

Two commands exist for the tools:

- **`peek`** returns the characters currently on the screen, one line
  per row. The serial task brings the grid up to date before each
  command, so `peek` right after a command shows its effect. It is how
  a test asserts what the user sees.
- **`city`** returns the scene the city draws, as a table.

The prompt over the network (`remote on`) answers in the same format,
so `remote.Machine.cmd` returns what `Xila.cmd` does.

## Driving QEMU: the `Xila` class

`tools/xq.py` starts QEMU without a window, with the serial port and
the QEMU monitor on two Unix sockets.

```python
import sys
sys.path.insert(0, "tools")
from xq import Xila, blank_disk

with Xila(disk=blank_disk("build/try.img")) as x:
    print("\n".join(x.boot_log))
    ok, lines = x.cmd("create table notes (title text)")
    rows = x.table("select name, share from tasks")   # a list of dicts
    x.type("help")                                    # keys, as on a US keyboard
    x.key("ret")
    print("\n".join(x.peek()))                        # the 50 rows of the screen
    x.shot("build/shots/help")                        # build/shots/help.png
```

| Method | What it does |
|---|---|
| `cmd(line)` | Runs a command on the serial shell. Returns `(ok, lines)`. Events go to `x.events`, lost counts to `x.lost`, and `x.more` says whether a block is open. |
| `table(line)` | Runs a command and returns the rows of its first table as dicts. |
| `peek()` | The characters on the screen. |
| `key(name)` | Presses a key through the QEMU monitor: `ret`, `tab`, `esc`, `up`, `pgup`, `alt-e`... |
| `type(text)` | Types text, key by key. |
| `shot(path)` | Saves a screenshot as PNG. |
| `cut()` | Kills the machine at once, as a power cut would. |

QEMU key names are positions on a US keyboard, which is the layout the
system starts with.

The machine is chosen by the arguments of `Xila(...)`:

| Argument | The machine has |
|---|---|
| `disk=path` | A virtio disk on that file; the database takes it whole. `blank_disk(path)` makes an empty one. |
| `sata=path` | A SATA disk; the database takes only the partition of XilaOS's type. `partitioned_disk(path, megabytes, partitions)` makes one with a GPT. |
| neither | No disk for the tables: they live in memory. |
| `cpu="max"` | A processor with 1 GB pages. |
| `stick=image` | Starts from the image of a USB stick, which the kernel cannot see, as on a real machine. |
| `bare=True` | Starts from whatever its other disks hold, as a machine does once XilaOS is installed. |
| `net="e1000e"` | A network card on QEMU's own small network: the guest gets 10.0.2.15, and 10.0.2.2 answers a ping. Every frame is kept in `build/net.pcap`. |
| `forward=port` | A UDP port of the host that leads to the guest's network prompt. |
| `web=port` | A TCP port of the host that leads to the guest's web pages. |
| `tftp=folder` | The folder's files are served at 10.0.2.2, for `install from`. |
| `more=[...]` | More arguments for QEMU, for example `["-rtc", "base=2026-03-04T23:59:45"]` to set the clock. |

Without `stick` or `bare`, the machine starts from the folder
`build/esp`, which QEMU shows it as a small SATA disk without a XilaOS
partition.

## What `check.py` goes through

In order, each part with its own start of the machine where the
hardware differs:

1. the boot log, `about`, the timer;
2. the screen and the keyboard: typing, shifted characters, history,
   completion, numbered rows, the menu, scrolling, the French layout;
3. tasks: a task that never rests gets the processor, the interface
   still answers, `stop` ends it, the processor idles otherwise;
4. the city and the display: a busy task stands tall, the picture is on
   the screen, 25 pictures per second, a typed character redraws its
   cell and nothing else;
5. SQL, stored tables, events, XilaScript, programs that run by
   themselves;
6. the disk: stop and start again; power cuts in the middle of a
   statement; no disk; a disk that holds something else; a SATA disk
   with another system on it, compared byte for byte outside the
   partition before and after; `disk erase`;
7. the network: DHCP, ping, the captured frames re-read by other code
   for lengths and checksums; the remote prompt; the web pages fetched
   with `urllib`; `net get` against a small HTTP server on the host;
   names against a small name server on the host;
8. the clock a quarter of a minute before midnight; kept settings;
9. two pictures shown in turn (the trial kind, in memory);
10. QR codes, read back by the reader of macOS;
11. installing from a USB stick image onto a disk that holds another
    system; what is on the disk afterwards read by Python and by
    `fsck_msdos`; `install system`; `install from`; a disk too small.

Add a check there for every behavior that is added.

## Things QEMU does not tell

Working in an emulator has blind spots. Those met here:

- **QEMU forgives what real hardware does not.** The network driver
  worked in QEMU from the first try and sent nothing on the T430 for
  five trials, because a register was written whole instead of bit by
  bit (the story is in [Networking](10-networking.md)). When a driver
  is changed, keep to what the specification says rather than to what
  QEMU accepts. The SATA driver has only run on QEMU's controller and
  on the T430's.
- **QEMU does not imitate the slowness of screen memory.** What write
  combining gains can only be measured on a real machine.
- **QEMU has no Intel graphics chip.** Everything about it is tested
  with a made-up chip in the unit tests and with `display flip try` in
  QEMU; the real thing was tried on the T430 only.
- **Times depend on the host.** Measured in QEMU on a laptop, times are
  three to four times longer when the guest idles than when a `spin`
  task runs. Compare like with like.
- **Screen sizes.** To see the interface at another size, change
  `grid::DESIGNED` for a build (1360x768 is close to the T430's panel).
  Not 1366x768: QEMU's display rounds it to 1360 while the firmware
  still says 1366, and the picture comes out sheared there and only
  there.
- **The firmware writes too.** QEMU's firmware keeps its settings in a
  file `NvVars` that it writes in the first start-up partition it
  finds: a start-up partition that went through a start of QEMU holds
  that file besides the kernel.

## On the real machine

The T430 has no serial port in use, so what happens there is read on
its screen:

- the boot log (`Alt+1`) says what was found;
- `qr COMMAND` shows any output as a QR code for a phone;
- `net registers` and `display registers` are sized to fit one code;
- once the network works, `remote on` gives a prompt from the
  development machine, and `tools/netboot.py` delivers each new kernel
  without a USB stick.

## Conventions of the code

- Code, comments and everything the system shows are in English.
  Messages are whole sentences in plain words: `1 row added to notes,
  with id 1.`, `The system needs that task.`
- Text is bytes in the font's character set, not UTF-8: `&[u8]`,
  `b"..."`, `FixedStr<N>`.
- `xila-core` allocates nothing. A new buffer there has a fixed size
  and a stated limit.
- A device register is changed bit by bit, never written whole unless
  every one of its bits is known.
- Register addresses are taken from a named source and checked there,
  not written from memory: the AHCI and UEFI specifications, Linux's
  `i915_reg.h` and `e1000e` driver (v4.9), read for what they say of
  the hardware, not copied.
- The file `CLAUDE.md` at the root of the repository is the briefing
  the AI model worked from. It is a dense summary of all of the above
  and is kept as it was used.
