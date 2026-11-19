#!/usr/bin/env python3
"""Drive XilaOS in QEMU without a window: serial shell, keyboard, screenshots.

usage: xq.py [action...]

actions, run in order after the system has started:
  cmd:LINE      run a command on the serial shell and print its output
  peek          print the characters on the screen (the `peek` command)
  type:TEXT     type text on the keyboard (US key positions)
  key:NAME      press a key by its QEMU name: ret, tab, esc, up, pgup, alt-e, ...
  wait:SECONDS  wait
  shot:FILE     save a screenshot as FILE.png

Without actions it starts the system, prints the boot log and stops.
The exit status is 1 if a command failed or the system did not start.

As a module: `with Xila() as x: ok, lines = x.cmd("about")`.
"""
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROMPT = b"XilaOS|all:/>"

KEYS = {" ": "spc", "-": "minus", "=": "equal", ".": "dot", ",": "comma", "/": "slash", ";": "semicolon",
        "'": "apostrophe", "[": "bracket_left", "]": "bracket_right", "\\": "backslash", "`": "grave_accent",
        "!": "shift-1", "@": "shift-2", "#": "shift-3", "$": "shift-4", "%": "shift-5", "^": "shift-6",
        "&": "shift-7", "*": "shift-8", "(": "shift-9", ")": "shift-0", "_": "shift-minus", "+": "shift-equal",
        ":": "shift-semicolon", '"': "shift-apostrophe", "<": "shift-comma", ">": "shift-dot", "?": "shift-slash",
        "{": "shift-bracket_left", "}": "shift-bracket_right", "|": "shift-backslash", "~": "shift-grave_accent"}


def firmware():
    if os.environ.get("XILA_FIRMWARE"):
        return os.environ["XILA_FIRMWARE"]
    qemu = subprocess.run(["which", "qemu-system-x86_64"], capture_output=True, text=True).stdout.strip()
    return os.path.join(os.path.dirname(os.path.dirname(os.path.realpath(qemu))), "share", "qemu", "edk2-x86_64-code.fd")


def disk_arguments(path):
    """QEMU arguments for the disk of the database: a virtio disk on a file."""
    return ["-drive", "if=none,id=data,format=raw,file=" + path,
            "-device", "virtio-blk-pci,drive=data,disable-legacy=off"]


def blank_disk(path, megabytes=64):
    """Makes an empty file for a disk, in place of whatever was there."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as image:
        image.truncate(megabytes << 20)
    return path


# Types of partitions: the one XilaOS takes, and one of another system.
XILA_PARTITION = "04DA9601-6187-4182-88FE-27F226C8DEE9"
LINUX_PARTITION = "0FC63DAF-8483-4772-8E79-3D69D8477DE4"


def partitioned_disk(path, megabytes, partitions):
    """Makes the file of a disk with a partition table (GPT).

    `partitions` lists (type, first megabyte, megabytes). Returns the byte
    ranges of the partitions, in the same order.
    """
    sectors = megabytes * 2048
    entries = b""
    ranges = []
    for kind, first, size in partitions:
        first_sector, last_sector = first * 2048, (first + size) * 2048 - 1
        entries += uuid.UUID(kind).bytes_le + uuid.uuid4().bytes_le + struct.pack("<QQQ", first_sector, last_sector, 0)
        entries += "part".encode("utf-16-le").ljust(72, b"\0")
        ranges.append((first_sector * 512, (last_sector + 1) * 512))
    entries = entries.ljust(128 * 128, b"\0")
    identity = uuid.uuid4().bytes_le

    def header(here, other, entries_at):
        fields = [b"EFI PART", 0x00010000, 92, 0, 0, here, other, 34, sectors - 34, identity, entries_at, 128, 128, zlib.crc32(entries)]
        fields[3] = zlib.crc32(struct.pack("<8sIIIIQQQQ16sQIII", *fields))
        return struct.pack("<8sIIIIQQQQ16sQIII", *fields).ljust(512, b"\0")

    # the first sector tells old tools that the whole disk is taken
    guard = struct.pack("<BBBBBBBBII", 0, 0, 2, 0, 0xEE, 0xFF, 0xFF, 0xFF, 1, min(sectors - 1, 0xFFFFFFFF))
    blank_disk(path, megabytes)
    with open(path, "r+b") as image:
        image.write(b"\0" * 446 + guard + b"\0" * 48 + b"\x55\xaa")
        image.write(header(1, sectors - 1, 2) + entries)
        image.seek((sectors - 33) * 512)
        image.write(entries + header(sectors - 1, 1, sectors - 33))
    return ranges


class Xila:
    def __init__(self, esp=None, memory=256, timeout=60, disk=None, sata=None, cpu=None, stick=None, bare=False, more=(), net=None, forward=None, tftp=None, web=None):
        """Starts the system.

        `disk` is the file of a virtio disk, which the database takes whole.
        `sata` is the file of a SATA disk, of which it takes the partition
        marked for XilaOS. Without either the tables are kept in memory.
        `cpu` picks the processor QEMU imitates.

        The machine starts from the folder `esp`, which it sees as a SATA
        disk. With `stick`, the image of a USB stick, it starts from that
        instead, and with `bare` from whatever its other disks hold: as a
        real machine does, which XilaOS was installed on. `more` is more
        arguments for QEMU.

        `net` plugs a network card, of the model it names ("e1000e" is the
        family of the T430's), into a small network QEMU makes up: it gives
        the machine the address 10.0.2.15, and 10.0.2.2 answers a ping.
        Every frame that goes by is kept in build/net.pcap. `forward` is a
        UDP port of this machine that leads to the guest's prompt for the
        network (`remote on`), which nothing else can reach. `tftp` is a
        folder whose files the small network gives to who asks at 10.0.2.2
        (`install from 10.0.2.2`). `web` is a TCP port of this machine that
        leads to the guest's pages for browsers (`web on`).
        """
        self.dir = tempfile.mkdtemp(prefix="xq")
        qmp, ser = os.path.join(self.dir, "qmp"), os.path.join(self.dir, "serial")
        esp = esp or os.path.join(ROOT, "build", "esp")
        self.qemu = subprocess.Popen(
            ["qemu-system-x86_64", "-machine", "q35", "-m", str(memory), "-net", "none", "-display", "none",
             "-drive", "if=pflash,format=raw,readonly=on,file=" + firmware(),
             "-qmp", f"unix:{qmp},server=on,wait=off",
             "-chardev", f"socket,id=ser,path={ser},server=on,wait=off", "-serial", "chardev:ser"]
            + (["-device", "qemu-xhci", "-drive", "if=none,id=stick,format=raw,file=" + stick,
                "-device", "usb-storage,drive=stick,bootindex=0"] if stick else
               [] if bare else ["-drive", "format=raw,file=fat:rw:" + esp])
            + (disk_arguments(disk) if disk else [])
            + (["-drive", "if=none,id=sata,format=raw,file=" + sata, "-device", "ide-hd,drive=sata,bus=ide.1"] if sata else [])
            + (["-cpu", cpu] if cpu else []) + list(more)
            + (["-netdev", "user,id=net0" + (",hostfwd=udp:127.0.0.1:%d-:4000" % forward if forward else "") + (",tftp=" + tftp if tftp else "") + (",hostfwd=tcp:127.0.0.1:%d-:80" % web if web else ""), "-device", net + ",netdev=net0",
                "-object", "filter-dump,id=dump0,netdev=net0,file=" + os.path.join(ROOT, "build", "net.pcap")] if net else []),
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        for _ in range(200):
            if os.path.exists(qmp) and os.path.exists(ser):
                break
            time.sleep(0.02)
        # events and lost counts received on the serial port, see cmd()
        self.events, self.lost = [], 0
        # whether the last command left a block open (if, for, program... without its end)
        self.more = False
        self.serial = socket.socket(socket.AF_UNIX)
        self.serial.connect(ser)
        self.q = socket.socket(socket.AF_UNIX)
        self.q.connect(qmp)
        self.qf = self.q.makefile("rw")
        self.qf.readline()
        self.qmp("qmp_capabilities")
        self.boot_log = self.read_until_prompt(timeout)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def cut(self):
        """Stops the machine at once, as a power cut would."""
        self.qemu.kill()
        self.qemu.wait()

    def close(self):
        try:
            self.qmp("quit")
        except Exception:
            pass
        try:
            self.qemu.wait(timeout=10)
        except Exception:
            self.qemu.kill()

    def qmp(self, command, **arguments):
        self.qf.write(json.dumps({"execute": command, "arguments": arguments}) + "\n")
        self.qf.flush()
        while True:
            reply = json.loads(self.qf.readline())
            if "return" in reply or "error" in reply:
                return reply

    def monitor(self, line):
        return self.qmp("human-monitor-command", **{"command-line": line}).get("return", "")

    def read_until_prompt(self, timeout=20, after=(b"",)):
        """Reads the serial port until the prompt. Returns the lines before it.

        `after` lists what may come right before the prompt. Output can contain
        the text of the prompt (`peek` shows it), so a command is only over
        when its status line is followed by the prompt.
        """
        ends = tuple(mark + PROMPT for mark in after)
        data, end = b"", time.time() + timeout
        while not data.endswith(ends):
            left = end - time.time()
            if left <= 0:
                raise TimeoutError("no prompt on the serial port; received: %r" % data[-400:])
            self.serial.settimeout(left)
            try:
                chunk = self.serial.recv(4096)
            except socket.timeout:
                continue
            if not chunk:
                raise ConnectionError("QEMU closed the serial port: " + self.qemu.stderr.read().decode()[-400:])
            data += chunk
        text = data[:-len(PROMPT)].decode("ascii", "replace").replace("\r", "")
        return [line for line in text.split("\n")]

    def cmd(self, line, timeout=20):
        """Runs a command on the serial shell. Returns (ok, output lines)."""
        self.serial.sendall(line.encode() + b"\r")
        lines = self.read_until_prompt(timeout, (b"#ok\r\n", b"#error\r\n"))
        lines = lines[1:]  # the echo of the command
        while lines and lines[-1] == "":
            lines.pop()
        status = lines.pop() if lines else ""
        # events this prompt listens to come before the status; set them apart
        for text in lines:
            if text.startswith("#event "):
                self.events.append(tuple(text[7:].split("\t", 1)))
            elif text.startswith("#lost "):
                self.lost += int(text[6:])
        self.more = "#more" in lines
        lines = [text for text in lines if not text.startswith(("#event ", "#lost ")) and text != "#more"]
        return status == "#ok", lines

    def table(self, line):
        """Runs a command and returns the rows of its first table as dicts."""
        ok, lines = self.cmd(line)
        rows, header = [], None
        for text in lines:
            if text.startswith("#table"):
                header = text.split("\t")[1:]
            elif text == "#end":
                break
            elif header is not None:
                rows.append(dict(zip(header, text.split("\t"))))
        return rows

    def peek(self):
        """The characters on the screen, one string per row."""
        return self.cmd("peek")[1]

    def key(self, name, pause=0.15):
        self.monitor("sendkey " + name)
        time.sleep(pause)

    def type(self, text):
        for ch in text:
            self.key(KEYS.get(ch) or ("shift-" + ch.lower() if ch.isupper() else ch), 0.08)

    def shot(self, path):
        """Saves a screenshot. Returns the path of the PNG (or PPM without sips)."""
        base = os.path.abspath(path)
        self.monitor(f'screendump "{base}.ppm"')
        time.sleep(0.4)
        done = subprocess.run(["sips", "-s", "format", "png", base + ".ppm", "--out", base + ".png"], capture_output=True)
        if done.returncode == 0 and os.path.exists(base + ".png"):
            os.unlink(base + ".ppm")
            return base + ".png"
        return base + ".ppm"


def main(actions):
    failed = False
    with Xila() as x:
        if not actions:
            print("\n".join(x.boot_log))
        for action in actions:
            kind, _, value = action.partition(":")
            if kind == "cmd":
                ok, lines = x.cmd(value)
                print("\n".join(lines))
                failed |= not ok
            elif kind == "peek":
                print("\n".join(x.peek()))
            elif kind == "type":
                x.type(value)
            elif kind == "key":
                x.key(value)
            elif kind == "wait":
                time.sleep(float(value))
            elif kind == "shot":
                print(x.shot(value))
            else:
                sys.exit("unknown action: " + action)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
