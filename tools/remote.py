#!/usr/bin/env python3
"""Types at the prompt of a XilaOS machine, from another machine of its network.

On the XilaOS machine, `remote on` opens its prompt to the local network
and shows a code and the address to use. Then, from here:

    python3 tools/remote.py 192.168.1.11 K7QD-2MXP            # a prompt
    python3 tools/remote.py 192.168.1.11 K7QD-2MXP net        # one command

Nothing is encrypted: the code and the commands cross the network as
they are. It is for a network one trusts; `remote off` closes the door.

A command goes as one message, the code first, and its answer comes back
in a few; a message the network loses is asked for again, and the
machine does not run the command twice (see core/src/remote.rs).
"""

import argparse
import socket
import sys
import time

PORT = 4000


class Machine:
    """The prompt of one XilaOS machine."""

    def __init__(self, address, code, port=PORT):
        self.to = (address, port)
        self.code = code
        self.link = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        # the number of a request tells it from the one before, also across runs of this tool
        self.number = int(time.time() * 1000) % 900_000_000

    def ask(self, line, wait=3.0, tries=3):
        """Runs a line at the prompt. Returns its answer as text, or None if none came."""
        self.number += 1
        request = ("%s %d %s" % (self.code, self.number, line)).encode("latin-1", "replace")
        for _ in range(tries):
            self.link.sendto(request, self.to)
            parts, count, until = {}, None, time.time() + wait
            while time.time() < until:
                self.link.settimeout(max(0.01, until - time.time()))
                try:
                    message, sender = self.link.recvfrom(4096)
                except socket.timeout:
                    break
                except ConnectionError:
                    # the machine says nothing listens there: closed, or not started yet
                    time.sleep(0.2)
                    continue
                first, _, rest = message.partition(b"\n")
                words = first.split()
                if sender[0] != self.to[0] or len(words) != 3 or not all(word.isdigit() for word in words):
                    continue
                if int(words[0]) != self.number:
                    # the answer to an earlier request, late
                    continue
                parts[int(words[1])], count = rest, int(words[2])
                if len(parts) == count:
                    return b"".join(parts[index] for index in sorted(parts)).decode("latin-1")
        return None

    def cmd(self, line, **how):
        """Runs a line. Returns (ok, lines) as the tools of the serial port do; ok is None without an answer."""
        answer = self.ask(line, **how)
        if answer is None:
            return None, []
        lines = answer.split("\n")
        while lines and lines[-1] == "":
            lines.pop()
        status = lines.pop() if lines else ""
        return status == "#ok", lines


def show(lines):
    """Prints what a command said; tables with their columns in line. Returns whether more lines are expected."""
    more, table = False, None
    for line in lines:
        if line.startswith("#table"):
            table = [line.split("\t")[1:]]
        elif line == "#end" and table is not None:
            widths = [max(len(row[at]) if at < len(row) else 0 for row in table) for at in range(len(table[0]))]
            for row in table:
                print("  " + "  ".join(cell.ljust(width) for cell, width in zip(row, widths)).rstrip())
            table = None
        elif table is not None:
            table.append(line.split("\t"))
        elif line == "#more":
            more = True
        elif line.startswith("#event "):
            print("[event] " + line[7:].replace("\t", ": "))
        elif line.startswith("#lost "):
            print("[%s events were lost]" % line[6:])
        elif line.startswith("#cut "):
            print("[the answer was too long: %s characters were left out]" % line[5:])
        else:
            print(line)
    return more


def main():
    parser = argparse.ArgumentParser(description="Type at the prompt of a XilaOS machine over the local network.")
    parser.add_argument("address", help="the address of the machine, as its `remote on` shows it")
    parser.add_argument("code", help="the code its `remote on` shows")
    parser.add_argument("command", nargs="*", help="one command to run; without it, a prompt")
    parser.add_argument("--port", type=int, default=PORT)
    arguments = parser.parse_args()
    machine = Machine(arguments.address, arguments.code, arguments.port)
    silent = "No answer from %s. Is its prompt open (remote on), and is the code right?" % arguments.address
    if arguments.command:
        ok, lines = machine.cmd(" ".join(arguments.command))
        if ok is None:
            sys.exit(silent)
        show(lines)
        sys.exit(0 if ok else 1)
    try:
        import readline  # noqa: F401  (gives the prompt its history and its arrows)
    except ImportError:
        pass
    more = False
    while True:
        try:
            line = input("          ..> " if more else "xila> ")
        except (EOFError, KeyboardInterrupt):
            print()
            break
        ok, lines = machine.cmd(line)
        if ok is None:
            print(silent)
            continue
        more = show(lines)


if __name__ == "__main__":
    main()
