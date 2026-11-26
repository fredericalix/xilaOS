# XilaOS documentation

The same content as one file: [xilaos.pdf](xilaos.pdf) (rebuild it with
`python3 tools/docs.py`).

## Using it

1. [Overview](01-overview.md): what XilaOS is, what works, what does not.
2. [Getting started](02-getting-started.md): build, run in QEMU, a first
   session, a real machine.
3. [Using XilaOS](03-using-xilaos.md): the screen, the keys, the city,
   every command.
4. [The database and SQL](04-sql.md): tables, types, statements,
   LISTEN / NOTIFY.
5. [XilaScript](05-xilascript.md): the language of the prompt.

## How it works

6. [Architecture](06-architecture.md): the four crates and how the
   pieces fit.
7. [The kernel](07-kernel.md): boot, memory, page tables, interrupts,
   tasks, drivers.
8. [The storage engine](08-storage.md): the database that stands in for
   a file system.
9. [Disk layout and installation](09-disk-layout-and-install.md):
   partitions, the start-up partition, `install`.
10. [Networking](10-networking.md): the stack, TCP, the remote prompt,
    the web pages.
11. [Hardware and graphics](11-hardware.md): drivers, the display path,
    the Intel chip.

14. [The companion](14-the-companion.md): what to ask it, what it
    sees, what is kept, how it is built.

## Working on it

12. [Development and testing](12-development.md): the serial protocol,
    the tools, the checks.
13. [Status and roadmap](13-status-and-roadmap.md): what is done, what
    is not, what is designed, and the work plan.

## Also here

- `screenshots/`: the pictures used in these chapters, all taken from
  the running system in QEMU.
- `mockup/trame-0.3/`: the interactive web mock-up the interface was
  drawn from. See [mockup/README.md](mockup/README.md).
