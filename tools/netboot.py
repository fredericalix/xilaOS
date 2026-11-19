#!/usr/bin/env python3
"""Serves the kernel to a machine that starts from its network cable.

A PC can ask the network for a program to start (PXE). This answers: it
tells the machine where the kernel is, and hands it over. With it, a new
kernel reaches the T430 by restarting the T430, with no USB stick.

    python3 tools/netboot.py            # serves build/esp/EFI/BOOT/BOOTX64.EFI
    python3 tools/netboot.py --address 192.168.1.20

The network must already give addresses (a home router does): this only
adds, to machines that ask to start from the network, the name of the
file and where to fetch it. It gives no address and answers nothing
else, so it disturbs no other machine. It serves that one file, to
whoever asks on the local network, as long as it runs: stop it with
Ctrl+C when the trial is over.

It also gives the kernel to a running XilaOS that asks for it, to put it
on its disk: `install from` and this machine's address, typed there.

Two small servers in one:
- on port 67 (and 4011), the answer to "what do I start?" (proxy DHCP);
- on port 69, the file itself (TFTP).

On the machine: in the firmware's setup, turn on starting from the
network in UEFI mode over IPv4, then choose the network card in the
start-up menu (F12 on a ThinkPad).

Tried against a TFTP client and against requests made up for the test
(`--check`), and on one real machine: a Lenovo T430 behind a home router,
this server on a Mac on Wi-Fi. Its firmware took the plain answer (see
`answer`) and came to ask on port 4011.
"""

import argparse
import os
import socket
import struct
import sys
import threading
import time
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
KERNEL = os.path.join(ROOT, "build", "esp", "EFI", "BOOT", "BOOTX64.EFI")
NAME = b"BOOTX64.EFI"
COOKIE = b"\x63\x82\x53\x63"
# what a machine says it is, in option 93: these are PCs with UEFI on 64 bits
UEFI_64 = (7, 9)


def say(*words):
    print(time.strftime("%H:%M:%S"), *words, flush=True)


def options(packet):
    """The options of a DHCP packet, as {number: bytes}; None if it is not one."""
    if len(packet) < 240 or packet[236:240] != COOKIE:
        return None
    found, at = {}, 240
    while at < len(packet):
        number = packet[at]
        if number == 255:
            break
        if number == 0:
            at += 1
            continue
        if at + 2 > len(packet):
            return None
        size = packet[at + 1]
        found[number] = packet[at + 2:at + 2 + size]
        at += 2 + size
    return found


def answer(packet, ours, full=False, asked_here=False):
    """The answer to a machine that asks what to start, or None.

    `ours` is this machine's address, as 4 bytes. Returns (kind, bytes):
    kind 2 is an offer, sent to everyone since the asker has no address
    yet; kind 5 is an acknowledgement, sent to the asker.

    There are two ways of saying it, and firmwares differ in which they
    take. Plain: "I know what you start", and the firmware comes back to
    ask on port 4011. Full: the name of the file is in the offer itself,
    with the word that says to ask no more.

    A request (kind 3) is answered only if it was sent to us: on port
    4011 (`asked_here`), or naming us as the server. The other requests
    are for the server that gives addresses.
    """
    asked = options(packet)
    if asked is None or packet[0] != 1:
        return None
    if not asked.get(60, b"").startswith(b"PXEClient"):
        return None
    kind = asked.get(53, b"\0")[0]
    if kind not in (1, 3):
        return None
    if kind == 3 and not asked_here and asked.get(54) != ours:
        return None
    machine = struct.unpack(">H", asked[93])[0] if len(asked.get(93, b"")) == 2 else None
    if machine not in UEFI_64:
        return None
    reply = 2 if kind == 1 else 5
    head = bytearray(236)
    head[0] = 2
    head[1:4] = packet[1:4]
    head[4:8] = packet[4:8]            # the number of the exchange
    head[10:12] = packet[10:12]        # the flags: answer to everyone, or not
    head[12:16] = packet[12:16]        # the asker's address, if it has one
    head[20:24] = ours                 # where the file is
    head[28:44] = packet[28:44]        # the asker's card
    head[108:108 + len(NAME)] = NAME   # the name of the file
    said = [(53, bytes([reply])), (54, ours), (60, b"PXEClient")]
    if full:
        said += [(66, socket.inet_ntoa(ours).encode()), (67, NAME + b"\0")]
    body = bytearray(COOKIE)
    for number, value in said:
        body += bytes([number, len(value)]) + value
    if 97 in asked:
        body += bytes([97, len(asked[97])]) + asked[97]
    if full:
        # option 43, item 6, value 8: the name of the file is in this answer, ask no more
        body += bytes([43, 4, 6, 1, 8, 255])
    body += b"\xff"
    # never shorter than the oldest machines expect
    body += bytes(max(0, 300 - len(head) - len(body)))
    return reply, bytes(head + body)


def card(packet):
    return ":".join("%02x" % byte for byte in packet[28:34])


def on_interface(link, interface):
    """Keeps a socket on one network interface: what it sends to everyone goes there."""
    if interface and sys.platform == "darwin":
        try:
            link.setsockopt(socket.IPPROTO_IP, 25, socket.if_nametoindex(interface))
        except OSError as error:
            say("cannot keep to %s: %s" % (interface, error))


def dhcp(port, ours, to_port, interface=None, everyone=("255.255.255.255",)):
    """Answers, on one port, the machines that ask what to start."""
    listener = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    on_interface(listener, interface)
    listener.bind(("", port))
    asked_before = {}
    while True:
        packet, (address, from_port) = listener.recvfrom(2048)
        said = options(packet)
        if said is None or packet[0] != 1:
            continue
        who = card(packet)
        kind = said.get(53, b"\0")[0]
        if kind == 3 and port == 67 and who in asked_before and said.get(54) != ours:
            # the machine took an address: it had the offers it wanted
            wanted = socket.inet_ntoa(said[50]) if len(said.get(50, b"")) == 4 else "?"
            server = socket.inet_ntoa(said[54]) if len(said.get(54, b"")) == 4 else "?"
            say("%s takes the address %s from %s: it accepted our answer" % (who, wanted, server))
            continue
        count = asked_before.get(who, 0)
        # one way, then the other, at each new question: firmwares differ
        full = count % 2 == 1
        found = answer(packet, ours, full=full, asked_here=port != 67)
        if found is None:
            continue
        kind, reply = found
        try:
            if kind == 2:
                asked_before[who] = count + 1
                flags = "to everyone" if packet[10] & 0x80 else "to it alone"
                say("%s asks what to start (wants the answer %s): told it %s" % (who, flags, "with the name of the file" if full else "to come and ask here"))
                for to in everyone:
                    listener.sendto(reply, (to, to_port))
            else:
                say("%s (%s) asks for the file on port %d: told it %s" % (who, address, port, NAME.decode()))
                if address == "0.0.0.0":
                    for to in everyone:
                        listener.sendto(reply, (to, to_port))
                else:
                    listener.sendto(reply, (address, from_port))
        except OSError as error:
            say("could not send the answer:", error)


def others(ours, interface=None):
    """Says what the other servers of the network answer to machines that start: who gives them an address."""
    listener = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    if hasattr(socket, "SO_REUSEPORT"):
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    on_interface(listener, interface)
    try:
        listener.bind(("", 68))
    except OSError as error:
        say("cannot listen to the other servers:", error)
        return
    seen = set()
    while True:
        packet, (address, _) = listener.recvfrom(2048)
        said = options(packet)
        if said is None or packet[0] != 2:
            continue
        kind = said.get(53, b"\0")[0]
        mine = said.get(54) == ours
        key = (packet[4:8], address, kind, len(packet))
        if key in seen:
            continue
        seen.add(key)
        if mine:
            say("    (our answer to %s went out on the network)" % card(packet))
        else:
            what = {2: "offers", 5: "gives", 6: "refuses"}.get(kind, "says %d about" % kind)
            say("    %s %s the address %s to %s" % (address, what, socket.inet_ntoa(packet[16:20]), card(packet)))


def send_file(asker, path, wanted):
    """Hands one file to one machine, block by block (TFTP)."""
    try:
        with open(path, "rb") as file:
            data = file.read()
    except OSError as error:
        say("cannot read the kernel:", error)
        return
    link = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    link.settimeout(1)

    def exchange(packet, block):
        """Sends a packet until the block is acknowledged. False if the asker went away."""
        for _ in range(6):
            link.sendto(packet, asker)
            try:
                while True:
                    reply, sender = link.recvfrom(1500)
                    if sender != asker or len(reply) < 4:
                        continue
                    code, number = struct.unpack(">HH", reply[:4])
                    if code == 5:
                        return False
                    if code == 4 and number == block & 0xffff:
                        return True
            except socket.timeout:
                continue
        return False

    size = 512
    agreed = b""
    if "blksize" in wanted:
        try:
            size = max(8, min(int(wanted["blksize"]), 1428))
            agreed += b"blksize\0%d\0" % size
        except ValueError:
            pass
    if "tsize" in wanted:
        agreed += b"tsize\0%d\0" % len(data)
    if agreed and not exchange(struct.pack(">H", 6) + agreed, 0):
        # a firmware first asks only for the size, then says stop: not a failure
        link.close()
        return
    block, at = 1, 0
    while True:
        piece = data[at:at + size]
        if not exchange(struct.pack(">HH", 3, block & 0xffff) + piece, block):
            say("%s stopped at %d of %d bytes" % (asker[0], at, len(data)))
            break
        at += len(piece)
        block += 1
        if len(piece) < size:
            # the check number is the one `install from` shows on the machine that fetched it
            say("%s has the kernel: %d bytes, check %08x, built %s" % (asker[0], len(data), zlib.crc32(data), time.strftime("%H:%M:%S", time.localtime(os.path.getmtime(path)))))
            break
    link.close()


def tftp(port, path):
    """Waits for machines that ask for the file."""
    listener = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("", port))
    while True:
        packet, asker = listener.recvfrom(1500)
        if len(packet) < 4 or packet[:2] != b"\0\1":
            continue
        words = packet[2:].split(b"\0")
        name = words[0].decode("latin-1")
        wanted = {words[at].decode("latin-1").lower(): words[at + 1].decode("latin-1") for at in range(2, len(words) - 1, 2)}
        if os.path.basename(name.replace("\\", "/")).upper() != NAME.decode():
            # only the kernel is served, whatever is asked
            reply = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            reply.sendto(struct.pack(">HH", 5, 1) + b"No such file\0", asker)
            reply.close()
            say("%s asked for %r: refused" % (asker[0], name))
            continue
        threading.Thread(target=send_file, args=(asker, path, wanted), daemon=True).start()


def own_address():
    """This machine's address on the local network."""
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        # nothing is sent: this only asks the system which address it would use
        probe.connect(("192.0.2.1", 9))
        return probe.getsockname()[0]
    finally:
        probe.close()


def own_interface(address):
    """The interface that has this address, and its address for everyone: (name, a.b.c.d), or (None, None)."""
    import subprocess
    try:
        text = subprocess.run(["ifconfig"], capture_output=True, text=True).stdout
    except OSError:
        return None, None
    name = None
    for line in text.splitlines():
        if line and not line[0].isspace():
            name = line.split(":")[0]
        words = line.split()
        if len(words) >= 2 and words[0] == "inet" and words[1] == address:
            return name, words[words.index("broadcast") + 1] if "broadcast" in words else None
    return None, None


def check():
    """Tries both servers on this machine alone. Returns the number of failures."""
    failures = 0

    def expect(what, good, shown=None):
        nonlocal failures
        print("  ok  " if good else "  FAIL", what, "" if good else repr(shown))
        failures += not good

    ours = socket.inet_aton("192.168.1.20")

    def ask(kind, vendor=b"PXEClient:Arch:00007:UNDI:003016", machine=7):
        head = bytearray(236)
        head[0], head[1], head[2] = 1, 1, 6
        head[4:8] = b"\x12\x34\x56\x78"
        head[10:12] = b"\x80\x00"
        head[28:34] = bytes.fromhex("28d244081dc4")
        body = COOKIE + bytes([53, 1, kind]) + bytes([60, len(vendor)]) + vendor
        if machine is not None:
            body += bytes([93, 2]) + struct.pack(">H", machine)
        body += bytes([97, 17]) + bytes(17) + b"\xff"
        return bytes(head) + body

    found = answer(ask(1), ours)
    told = options(found[1]) if found else None
    plain = found is not None and found[0] == 2 and len(found[1]) >= 300 and found[1][20:24] == ours and found[1][108:119] == NAME and sorted(told) == [53, 54, 60, 97]
    expect("a PC that asks what to start is told to come and ask here", plain, found)
    found = answer(ask(1), ours, full=True)
    expect("or gets an offer with everything in it", found is not None and found[0] == 2, found)
    if found:
        reply = found[1]
        told = options(reply)
        expect("with the exchange's number, the card, where and what", reply[0] == 2 and reply[4:8] == b"\x12\x34\x56\x78" and reply[28:34] == bytes.fromhex("28d244081dc4") and reply[20:24] == ours and reply[108:119] == NAME, reply[:120])
        expect("and the options a firmware looks for", told is not None and told.get(53) == b"\x02" and told.get(54) == ours and told.get(60) == b"PXEClient" and told.get(67) == NAME + b"\0" and told.get(97) == bytes(17) and told.get(43) == b"\x06\x01\x08\xff", told)
    found = answer(ask(3), ours, asked_here=True)
    expect("the second question gets an acknowledgement", found is not None and found[0] == 5 and options(found[1]).get(53) == b"\x05", found)
    expect("but not a request meant for the server that gives addresses", answer(ask(3), ours) is None)
    expect("a machine that does not start from the network gets nothing", answer(ask(1, vendor=b"MSFT 5.0"), ours) is None)
    expect("nor an old PC without UEFI", answer(ask(1, machine=0), ours) is None)
    expect("nor what is not a question", answer(b"\x02" + ask(1)[1:], ours) is None and answer(b"junk", ours) is None)

    # the file, fetched back by a client written here
    import tempfile
    folder = tempfile.mkdtemp(prefix="netboot")
    path = os.path.join(folder, "kernel")
    data = bytes(range(256)) * 700 + b"end"
    with open(path, "wb") as file:
        file.write(data)
    port = 6969
    threading.Thread(target=tftp, args=(port, path), daemon=True).start()
    time.sleep(0.2)

    def fetch(name, wanted=()):
        client = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client.settimeout(2)
        request = b"\0\1" + name + b"\0octet\0" + b"".join(key + b"\0" + value + b"\0" for key, value in wanted)
        client.sendto(request, ("127.0.0.1", port))
        got, agreed = b"", None
        try:
            while True:
                packet, server = client.recvfrom(2048)
                code, number = struct.unpack(">HH", packet[:4])
                if code == 5:
                    return None, packet[4:]
                if code == 6:
                    words = packet[2:].split(b"\0")
                    agreed = dict(zip(words[0::2], words[1::2]))
                    client.sendto(struct.pack(">HH", 4, 0), server)
                    continue
                got += packet[4:]
                client.sendto(struct.pack(">HH", 4, number), server)
                size = int(agreed[b"blksize"]) if agreed and b"blksize" in agreed else 512
                if len(packet) - 4 < size:
                    return got, agreed
        except socket.timeout:
            return got, b"timeout"
        finally:
            client.close()

    got, agreed = fetch(b"BOOTX64.EFI")
    expect("the file comes whole, in blocks of 512 bytes", got == data, (len(got or b""), agreed))
    got, agreed = fetch(b"\\EFI\\BOOT\\bootx64.efi", ((b"tsize", b"0"), (b"blksize", b"1468")))
    expect("and in larger blocks, with its size said first", got == data and agreed == {b"blksize": b"1428", b"tsize": b"%d" % len(data)}, (len(got or b""), agreed))
    got, why = fetch(b"other.txt")
    expect("another file is refused", got is None and why.startswith(b"No such file"), (got, why))
    print("all checks passed" if failures == 0 else "%d checks failed" % failures)
    return failures


def main():
    parser = argparse.ArgumentParser(description="Serve the XilaOS kernel to a machine that starts from the network.")
    parser.add_argument("--address", help="this machine's address on the local network (found by itself otherwise)")
    parser.add_argument("--kernel", default=KERNEL, help="the file to serve")
    parser.add_argument("--check", action="store_true", help="try the servers on this machine alone, and stop")
    arguments = parser.parse_args()
    if arguments.check:
        sys.exit(1 if check() else 0)
    if not os.path.exists(arguments.kernel):
        sys.exit("No kernel at %s: build it first with tools/build.sh." % arguments.kernel)
    address = arguments.address or own_address()
    ours = socket.inet_aton(address)
    interface, broadcast = own_interface(address)
    everyone = ("255.255.255.255",) + ((broadcast,) if broadcast else ())
    servers = [(dhcp, (67, ours, 68, interface, everyone)), (dhcp, (4011, ours, 68, interface, everyone)), (others, (ours, interface)), (tftp, (69, arguments.kernel))]
    for server, given in servers:
        threading.Thread(target=server, args=given, daemon=True).start()
    time.sleep(0.3)
    say("Serving %s from %s%s. Start the machine from its network card. Ctrl+C stops." % (arguments.kernel, address, " (%s)" % interface if interface else ""))
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        say("Stopped.")


if __name__ == "__main__":
    main()
