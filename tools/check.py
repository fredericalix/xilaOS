#!/usr/bin/env python3
"""Checks XilaOS in QEMU: starts it and goes through what it must do.

Run after tools/build.sh. Prints one line per check and exits with status 1
if any failed. Screenshots go to build/shots/.
"""
import hashlib
import os
import re
import struct
import subprocess
import sys
import time
import uuid
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from xq import LINUX_PARTITION, ROOT, XILA_PARTITION, Xila, blank_disk, partitioned_disk

failed = []
# A statement is on the disk some 30 to 50 ms after it is sent: the power
# cuts are spread over that time, so that some fall before, some during,
# some after.
PAUSES = (0, 0.026, 0.03, 0.034, 0.038, 0.042, 0.046, 0.06)


def check(name, condition, detail=""):
    print(("ok    " if condition else "FAIL  ") + name + (("  -- " + str(detail)) if detail and not condition else ""))
    if not condition:
        failed.append(name)


def many_is_whole(x, rows):
    """Whether the table `many` holds the numbers from 0 on, `rows` of them
    or three more. Returns that and how many it holds."""
    found = x.table("select count(*), sum(n) from many")
    count = int(found[0]["count"]) if found else -1
    return count in (rows, rows + 3) and found[0]["sum"] == str(sum(range(count))), count


def power_cuts(start, rows, pauses, also=lambda x: True):
    """Starts the machine, sends a statement that adds three rows to `many`
    and cuts the power, once per pause. Returns whether every start found
    the table whole, how many rows each cut left, and the rows at the last
    start: the caller looks at what the last cut left."""
    whole, kept = True, []
    for pause in pauses:
        with start() as x:
            ok, count = many_is_whole(x, rows)
            whole &= ok and also(x)
            kept.append(count - rows)
            rows = count
            x.serial.sendall(("insert into many values (%d, 'a'), (%d, 'b'), (%d, 'c')\r" % (rows, rows + 1, rows + 2)).encode())
            time.sleep(pause)
            x.cut()
    return whole, kept[1:], rows


def sha(data):
    return hashlib.sha256(data).hexdigest()


def body(x):
    """The rows of the screen below the title bar that are not empty."""
    return [row for row in x.peek()[5:] if row.strip()]


def title(x):
    return x.peek()[4].split("  ")[1].strip()


shots = os.path.join(ROOT, "build", "shots")
os.makedirs(shots, exist_ok=True)

# the disk of the database: empty at the start of every run
disk = blank_disk(os.path.join(ROOT, "build", "check.img"))

with Xila(disk=disk) as x:
    log = "\n".join(x.boot_log)
    check("boot log reaches 'System ready.'", "System ready." in log, log)
    check("boot log: the disk is found", "Disk ................ virtio, 64 MB" in log, log)
    check("boot log: an empty disk gets a new database", "Database ............ new, on the disk" in log, log)
    check("boot log names the screen mode", "1280x800, 50 rows of 80 characters" in log, log)
    # 1280 x 800 pixels of 4 bytes are 1000 pages: the screen and not a page more
    check("boot log: the screen memory is write combining, and only it", "Screen memory ....... write combining, 1000 pages of 4 KB" in log, log)
    check("boot log: a whole screen is timed", "Whole screen ........ shown in " in log and " ms as the firmware left the memory" in log, log)

    # the serial shell
    about = {row["item"]: row["value"] for row in x.table("about")}
    check("about: system", about.get("system") == "XilaOS 0.1", about)
    check("about: a processor name", bool(about.get("processor")), about)
    check("about: the disk and where the tables are", about.get("disk") == "virtio, 64 MB" and about.get("stored tables", "").startswith("on disk, transaction "), about)
    mem = {row["kind"]: row["size"] for row in x.table("mem")}
    check("mem: usable memory is near what QEMU was given", 200 <= int(mem.get("usable", "0 MB").split()[0]) <= 256, mem)
    ok, lines = x.cmd("frobnicate")
    check("unknown command fails with a message", not ok and lines == ["Unknown command: frobnicate"], lines)
    ok, lines = x.cmd("mood intense")
    check("mood is set", ok and lines == ["Mood is intense."], lines)
    time.sleep(1.2)
    check("the gauge shows the mood", "mood [intense] [#########.]" in x.peek()[2], x.peek()[2])
    x.cmd("mood auto")

    # the timer
    first = float(x.cmd("uptime")[1][0].split()[2])
    time.sleep(2)
    second = float(x.cmd("uptime")[1][0].split()[2])
    check("the timer runs at about 100 ticks per second", 1.5 <= second - first <= 3.0, second - first)

    # the screen
    rows = x.peek()
    check("the screen has 50 rows", len(rows) == 50, len(rows))
    check("header: version", "Version 0.1" in rows[0], rows[0])
    check("title bar", title(x) == "OVERVIEW", rows[4])
    check("welcome text", any("Type help to begin" in row for row in rows), rows[5:9])

    # the keyboard starts with the US layout, which matches the key names QEMU sends
    layout = x.table("select value from settings where name = 'keymap'")
    check("the keyboard layout is us at start", layout == [{"value": "us"}], layout)
    x.type("uptime")
    check("typed text appears at the prompt", body(x)[-1] == "XilaOS|all:/>uptime", body(x)[-1])
    x.key("ret")
    check("Enter runs the command", body(x)[-2].startswith("Up for "), body(x)[-3:])
    x.type("Sh1ft! (ok)")
    check("shifted characters", body(x)[-1] == "XilaOS|all:/>Sh1ft! (ok)", body(x)[-1])
    for _ in range(11):
        x.key("backspace", 0.05)
    check("Backspace erases", body(x)[-1] == "XilaOS|all:/>", body(x)[-1])
    x.key("up")
    check("Up recalls the last command", body(x)[-1] == "XilaOS|all:/>uptime", body(x)[-1])
    x.key("down")
    x.type("the")
    x.key("tab")
    check("Tab completes a command", body(x)[-1] == "XilaOS|all:/>theme", body(x)[-1])
    x.type("night")
    x.key("ret")
    check("theme is changed from the keyboard", x.cmd("theme")[1] == ["Theme is night."])
    x.shot(os.path.join(shots, "check-night"))
    x.cmd("theme day")

    # numbered results
    x.type("screen")
    x.key("ret")
    check("screen lists the screens with numbers", " [2] [Alt]+[1]  boot log" in body(x)[-4] and " [3] [Alt]+[2]  companion" in body(x)[-3], body(x)[-5:])
    # a list belongs to who was shown it: the serial port has its own
    ok, lines = x.cmd("1")
    check("a list shown on the screen is not the serial port's", not ok and lines == ["There is no list to follow yet."], lines)
    x.cmd("help")
    ok, lines = x.cmd("3")
    check("the serial port follows its own list", ok and lines[0] == "XilaOS keys:", lines[:1])
    x.type("2")
    x.key("ret")
    check("typing the number follows the line", title(x) == "BOOT LOG", title(x))
    check("the boot log is on its screen", any("System ready." in row for row in x.peek()), body(x)[-3:])
    x.key("esc")
    check("Esc goes back to the overview", title(x) == "OVERVIEW", title(x))

    # menu and shortcuts
    x.key("alt-e")
    check("Alt+E opens the menu", any("[Alt]+[1]  boot log" in row for row in x.peek()), body(x)[:4])
    x.shot(os.path.join(shots, "check-menu"))
    x.key("down")
    x.key("ret")
    check("the menu switches screens", title(x) == "BOOT LOG", title(x))
    x.key("alt-0")
    check("Alt+0 shows the overview", title(x) == "OVERVIEW", title(x))
    x.key("alt-x")
    check("Alt+X changes the mood", x.cmd("mood")[1] == ["Mood is intense."], x.cmd("mood")[1])
    x.cmd("mood auto")

    # scrolling
    x.type("help c")
    x.key("ret")
    for _ in range(4):
        x.key("up", 0.05)
        x.key("ret", 0.1)
    time.sleep(0.3)
    last = x.peek()[49]
    check("long output scrolls: the prompt stays on the last row", last.strip() == "XilaOS|all:/>", last)
    x.key("pgup")
    check("PgUp shows older rows", x.peek()[49].strip() != "XilaOS|all:/>", x.peek()[49])
    x.key("pgdn")
    check("PgDn comes back", x.peek()[49].strip() == "XilaOS|all:/>", x.peek()[49])

    # the French layout: the key marked Q on a US keyboard is A
    x.cmd("keymap fr")
    x.key("q")
    x.key("w")
    check("French layout", body(x)[-1] == "XilaOS|all:/>az", body(x)[-1])
    # the key right of P is a dead key: its accent goes on the next letter.
    # A letter with an accent leaves the serial port as a question mark.
    x.key("bracket_left")
    check("French layout: a dead key shows nothing by itself", body(x)[-1] == "XilaOS|all:/>az", body(x)[-1])
    x.key("e")
    x.key("bracket_left")
    x.key("t")
    x.key("bracket_left")
    x.key("spc")
    check("French layout: the accent goes on a letter that takes it, before another, or alone", body(x)[-1] == "XilaOS|all:/>az?^t^", body(x)[-1])
    for _ in range(4):
        x.key("backspace", 0.05)
    x.key("alt-1")
    check("Alt+1 works with the French layout", title(x) == "BOOT LOG", title(x))
    x.key("alt-0")
    x.cmd("keymap us")
    x.key("backspace")
    x.key("backspace")
    x.cmd("clear")
    x.shot(os.path.join(shots, "check-day"))

    # tasks
    names = [row["name"] for row in x.table("tasks")]
    check("tasks: interface, idle and serial shell", names == ["interface", "idle", "serial shell"], names)
    x.cmd("spin")
    time.sleep(2)
    tasks = {row["name"]: row for row in x.table("tasks")}
    ran = float(tasks.get("spin", {}).get("ran", "0 s").split()[0])
    check("a task that never rests gets the processor", ran >= 1.0, tasks)
    x.type("mood")
    x.key("ret")
    check("the interface still answers while it runs", body(x)[-2] == "Mood is auto.", body(x)[-3:])
    ok, lines = x.cmd("stop " + tasks.get("spin", {}).get("id", "9"))
    names = [row["name"] for row in x.table("tasks")]
    check("stop ends the task", ok and "spin" not in names, names)
    ok, lines = x.cmd("stop 0")
    check("a task the system needs cannot be stopped", not ok and lines == ["The system needs that task."], lines)
    idle = lambda: float({row["name"]: row for row in x.table("tasks")}["idle"]["ran"].split()[0])
    start = idle()
    time.sleep(2)
    check("with nothing to do the processor idles", idle() - start >= 1.5, idle() - start)

    # the city shows the real state of the system
    city = lambda: {(row["plot"], row["block"]): row for row in x.table("city")}
    blocks = city()
    check("city: one block per task", {b for p, b in blocks if p == "running"} == {"interface", "idle", "serial shell"}, blocks.keys())
    check("city: settings, logs and the serial port", all(k in blocks for k in [("settings", "mood"), ("logs", "boot log"), ("elsewhere", "serial port")]), blocks.keys())
    x.cmd("spin")
    time.sleep(1.5)
    spin = city().get(("running", "spin"), {})
    check("city: a busy task stands tall and is lit", int(spin.get("height", 0)) >= 15 and spin.get("lit") == "yes", spin)
    x.shot(os.path.join(shots, "check-city"))
    x.cmd("stop " + [row["id"] for row in x.table("tasks") if row["name"] == "spin"][0])
    try:
        from PIL import Image
        picture = Image.open(os.path.join(shots, "check-city.png")).convert("RGB").crop((0, 80, 1280, 432))
        colors = len(picture.getcolors(100000))
        check("city: the picture is on the screen", colors >= 25, colors)
    except ImportError:
        print("skip  city: the picture is on the screen (needs the Pillow module)")

    # how the screen is drawn
    time.sleep(2.2)
    display = {row["item"]: row["value"] for row in x.table("display")}
    check("display: the screen and its memory", display.get("screen") == "1280x800, cells of 16 pixels" and display.get("screen memory") == "write combining", display)
    pictures = int(display.get("pictures shown", "0").split()[0])
    check("display: the city moves at 25 pictures per second", 22 <= pictures <= 30, display)
    times = [display.get(item, "") for item in ("whole screen", "the firmware's way", "longest picture", "city")]
    check("display: times are measured", all(re.match(r"\d+\.\d ms", value) and float(value.split()[0]) > 0 for value in times), display)
    check("display: pictures are not skipped when nothing else runs", int(display.get("pictures skipped", "99").split()[0]) <= 5, display)
    ok, lines = x.cmd("display test")
    measured = re.match(r"The whole screen: drawn in (\d+\.\d) ms, shown in (\d+\.\d) ms \(best of 5\)\.$", lines[0] if lines else "")
    check("display test: times a whole screen", ok and measured and 0 < float(measured[1]) < 100 and 0 < float(measured[2]) < 100, lines)
    ok, lines = x.cmd("display fast")
    check("display: an unknown word is an error", not ok and lines == ["Type display, or display with test, chip, registers, flip on, flip off, brightness."], lines)
    found = {row["kind"]: row for row in x.table("devices")}
    drives = found.get("SATA disks", {}).get("driver") == "XilaOS" and found.get("graphics", {}).get("maker_model") == "1234:1111" and found.get("graphics", {}).get("driver") == "none"
    check("devices: lists the parts of the machine and which are driven", drives and "bridge to the processor" in found, found)
    # QEMU imitates no chip XilaOS can read: it must say so and touch nothing
    check("the boot log names the graphics chip", "Graphics chip ....... 1234:1111, not known to XilaOS" in "\n".join(x.boot_log), x.boot_log)
    ok, lines = x.cmd("display chip")
    check("display chip: a chip XilaOS does not know is not read", not ok and lines == ["The graphics chip, 1234:1111, is not one XilaOS knows how to read."], lines)
    ok, lines = x.cmd("display registers")
    check("display registers: nor are its registers", not ok and len(lines) == 1, lines)
    ok, lines = x.cmd("display brightness 50")
    check("display brightness: nor is the light of its screen set", not ok and lines == ["The graphics chip, 1234:1111, is not one XilaOS knows how to read."], lines)
    ok, lines = x.cmd("display brightness high")
    check("display brightness: wants a number", not ok and lines == ["Type display brightness, or with a number from 5 to 100: display brightness 60"], lines)
    rows = {row["item"]: row["value"] for row in x.table("display")}
    check("display: the timer gives the rhythm where no chip does", rows.get("rhythm") == "a picture every 4 ticks, 100 ticks a second" and rows.get("pictures change") == "while the screen is swept", rows)
    check("the boot log says nothing of two pictures without a chip", "Pictures ...." not in "\n".join(x.boot_log), x.boot_log)
    rows = x.table("select value from machine where item = 'graphics chip'")
    check("select: the machine view names the graphics chip", rows == [{"value": "1234:1111, not known to XilaOS"}], rows)
    rows = x.table("select value from machine where item = 'screen memory'")
    check("select: the machine view tells how the screen memory is written", rows == [{"value": "write combining"}], rows)
    x.type("display test")
    x.key("ret")
    time.sleep(0.3)
    check("display test typed at the keyboard answers on the screen", body(x)[-2].startswith("The whole screen: drawn in "), body(x)[-3:])
    try:
        from PIL import Image, ImageChops
        # below the city, where nothing moves but the cursor
        below = lambda name: Image.open(x.shot(os.path.join(shots, name))).convert("RGB").crop((0, 432, 1280, 800))
        before = below("check-before")
        check("display test leaves the city on the screen", len(Image.open(os.path.join(shots, "check-before.png")).convert("RGB").crop((0, 80, 1280, 432)).getcolors(100000)) >= 25)
        x.type("W")
        time.sleep(0.2)
        box = ImageChops.difference(before, below("check-after")).getbbox()
        # the prompt is 13 characters wide: the W takes the place of the cursor, which moves one cell
        row = max(number for number, text in enumerate(x.peek()) if text.strip())
        expected = (13 * 16, row * 16 - 432, 15 * 16, row * 16 - 432 + 16)
        check("a typed character redraws its cell and the cursor's, nothing else", box is not None and box[0] >= expected[0] and box[1] >= expected[1] and box[2] <= expected[2] and box[3] <= expected[3] and box[2] - box[0] > 16, (box, expected))
        x.key("backspace")
    except ImportError:
        print("skip  the pixels of a typed character (needs the Pillow module)")

    # SQL on the tables of the system
    names = [row["name"] for row in x.table("select name from tasks order by name")]
    check("select: the tasks, sorted", names == ["idle", "interface", "serial shell"], names)
    # nine views of the system, and the table the settings are kept in since the theme was changed
    count = x.table("select count(*) from tables")
    check("select: count", count == [{"count": "10"}], count)
    ok, lines = x.cmd("select nope from tasks")
    check("select: an unknown column is an error", not ok and lines == ["Unknown column: nope"], lines)
    listed = [row["name"] for row in x.table("tables")]
    check("tables lists the catalog", listed[:3] == ["tables", "columns", "tasks"] and listed[9:] == ["kept_settings"], listed)
    x.type("select value from settings where name = 'keymap'")
    x.key("ret")
    check("select typed at the keyboard", body(x)[-2].strip() == "us", body(x)[-3:])
    said = x.table("select 'it''s' as said, length('l''eau') as letters")
    check("select: a quote inside a text is written twice", said == [{"said": "it's", "letters": "5"}], said)
    about = x.table("select about from tables where name = 'memory'")
    check("tables: what a view is about is not cut", about == [{"about": "memory: the machine, the kernel, the database"}], about)
    x.cmd("select * from memory")
    lit = {row["block"]: row["lit"] for row in x.table("select block, lit from city where plot = 'data'")}
    check("city: a table that was just read is lit", lit.get("memory") == "yes" and lit.get("screens") == "", lit)

    # stored tables
    ok, lines = x.cmd("create table expenses (day text, label text, amount decimal)")
    check("create table", ok and lines == ["Table expenses created."], lines)
    ok, lines = x.cmd("insert into expenses values ('2026-11-18', 'coffee', 3.5), ('2026-11-18', 'train', 42)")
    check("insert", ok and lines == ["2 rows added to expenses, with ids 1 to 2."], lines)
    rows = x.table("select id, label, amount from expenses order by amount desc")
    wanted = [{"id": "2", "label": "train", "amount": "42.00"}, {"id": "1", "label": "coffee", "amount": "3.50"}]
    check("select on a stored table", rows == wanted, rows)
    ok, lines = x.cmd("insert into expenses values ('2026-11-18', 'tea', 'two')")
    check("a value of the wrong type is refused", not ok and lines == ["Column amount takes a number."], lines)
    ok, lines = x.cmd("update expenses set amount = 0.1 where label = 'coffee'")
    check("update", ok and lines == ["1 row changed in expenses."], lines)
    ok, lines = x.cmd("insert into expenses (label, amount) values ('tea', 0.2)")
    total = x.table("select sum(amount), count(day) from expenses where amount < 1")
    check("decimals add up exactly, and empty values are skipped", total == [{"sum": "0.30", "count": "1"}], total)
    stored = x.table("select name, columns, rows from tables where kind = 'stored' and name <> 'kept_settings'")
    check("the catalog lists the stored table", stored == [{"name": "expenses", "columns": "4", "rows": "3"}], stored)
    types = [row["type"] for row in x.table("select type from columns where table_name = 'expenses'")]
    check("the catalog lists its columns", types == ["int", "text", "text", "decimal(18,2)"], types)
    home = x.table("select block, height, lit from city where plot = 'home' and block <> 'kept_settings'")
    check("city: the stored table lives at home", home == [{"block": "expenses", "height": "11", "lit": "yes"}], home)
    # calculations
    check("select without a table is a calculation", x.table("select 7 / 2 as half, 0.1 + 0.2 as sum") == [{"half": "3.5", "sum": "0.3"}], x.table("select 7 / 2 as half, 0.1 + 0.2 as sum"))
    ok, lines = x.cmd("update expenses set amount = amount * 2 + 1, label = upper(label) where label = 'tea' or amount > 40")
    check("update works values out from the row", ok and lines == ["2 rows changed in expenses."], lines)
    rows = x.table("select label, amount, amount > 10 as large from expenses order by id")
    wanted = [{"label": "coffee", "amount": "0.10", "large": "0"}, {"label": "TRAIN", "amount": "85.00", "large": "1"}, {"label": "TEA", "amount": "1.40", "large": "0"}]
    check("the calculations are exact", rows == wanted, rows)
    ok, lines = x.cmd("update expenses set amount = amount / 0")
    check("a calculation that fails changes nothing", not ok and lines == ["Division by zero."] and x.table("select sum(amount) from expenses") == [{"sum": "86.50"}], lines)
    x.cmd("update expenses set amount = 42, label = 'train' where id = 2")
    x.cmd("update expenses set amount = 0.2, label = 'tea' where id = 3")
    x.type("select label, avg(amount) from expenses group by label order by avg desc limit 1")
    x.key("ret")
    check("a calculation over rows typed at the keyboard", body(x)[-2].split() == ["train", "42.00"], body(x)[-3:])
    ok, lines = x.cmd("delete from tasks")
    check("the views of the system cannot be changed", not ok and "view of the system" in lines[0], lines)
    x.type("delete from expenses where id = 1")
    x.key("ret")
    check("delete typed at the keyboard", body(x)[-2].strip() == "1 row removed from expenses.", body(x)[-3:])

    # listen / notify
    x.type("listen expenses")
    x.key("ret")
    check("listen typed at the keyboard", body(x)[-2].strip() == "Listening to expenses.", body(x)[-3:])
    x.cmd("insert into expenses (label) values ('heard')")
    time.sleep(0.3)
    check("the screen hears of a change made on the serial port", body(x)[-2].strip() == "event expenses: insert 4", body(x)[-3:])
    lit = {row["block"]: row["lit"] for row in x.table("select block, lit from city where plot = 'logs'")}
    check("city: the events block is lit", lit.get("events") == "yes", lit)
    x.type("unlisten")
    x.key("ret")
    x.cmd("listen expenses")
    x.cmd("listen alerts")
    x.events.clear()
    x.cmd("update expenses set label = 'x' where id > 2")
    check("the serial port hears of every changed row", x.events == [("expenses", "update 3"), ("expenses", "update 4")], x.events)
    x.events.clear()
    ok, lines = x.cmd("notify alerts 'the disk is full'")
    check("notify reaches who listens", ok and lines == ["Sent to 1 listener."] and x.events == [("alerts", "the disk is full")], (lines, x.events))
    x.events.clear()
    x.cmd("insert into expenses values ('a', 'b', 'c')")
    x.cmd("")
    check("a refused change sends nothing", x.events == [], x.events)
    x.cmd("unlisten")
    for batch in range(5):
        x.cmd("insert into expenses (label) values " + ", ".join("('n%d')" % (batch * 8 + i) for i in range(8)))
    ok, lines = x.cmd("select id from expenses")
    check("a long table says what is not shown", ok and lines[-1] == "11 more rows are not shown. Narrow the query with WHERE.", lines[-2:])
    ok, lines = x.cmd("drop table expenses")
    check("drop table", ok and lines == ["Table expenses removed, with its 43 rows."], lines)
    check("the dropped table is gone", x.table("select name from tables where kind = 'stored'") == [{"name": "kept_settings"}], None)
    x.type("create table longer (first text, second text, third decimal, fourth int)")
    check("a line longer than the row slides", body(x)[-1] == "XilaOS|all:/> table longer (first text, second text, third decimal, fourth int)", body(x)[-1])
    x.key("ret")
    check("a long line typed at the keyboard runs", body(x)[-2].strip() == "Table longer created.", body(x)[-3:])
    x.cmd("drop table longer")

    # memory
    check("boot log: the kernel heap passed its test", "heap tested" in log, log)
    mem = lambda: {row["kind"]: row["size"] for row in x.table("mem")}
    managed = int(mem().get("managed by the kernel", "0 MB").split()[0])
    check("the kernel manages most of the memory", 150 <= managed <= 256, mem())
    free = lambda: int(mem()["free"].split()[0])
    before = free()
    x.cmd("spin")
    during = free()
    check("a task takes memory for its stack", before - during == 256, (before, during))
    for _ in range(10):
        spinning = [row["id"] for row in x.table("tasks") if row["name"] == "spin"]
        x.cmd("stop " + spinning[0])
        x.cmd("spin")
    x.cmd("stop " + [row["id"] for row in x.table("tasks") if row["name"] == "spin"][0])
    check("stopped tasks give their memory back", free() == before, (before, free()))

    # the language
    x.cmd("create table spent (label text, amount decimal)")
    x.cmd("insert into spent values ('coffee', 3.5), ('train', 42), ('bread', 1.2)")
    ok, lines = x.cmd("let total = select sum(amount) from spent")
    check("let keeps a value", ok and lines == [], lines)
    ok, lines = x.cmd("say 'Total: ' + total")
    check("say shows a calculation", ok and lines == ["Total: 46.70"], lines)
    x.cmd("let floor = 3")
    rows = x.table("select label from spent where amount > floor order by label")
    check("a name is a value in SQL", rows == [{"label": "coffee"}, {"label": "train"}], rows)
    ok, lines = x.cmd("if total > 40")
    check("a block waits for its end", ok and lines == [] and x.more, (lines, x.more))
    x.cmd("say 'over'")
    x.cmd("else")
    x.cmd("say 'under'")
    ok, lines = x.cmd("end")
    check("if runs the lines that hold", ok and lines == ["over"] and not x.more, (lines, x.more))
    for line in ("program report", "# what was spent, largest first", "for row in select label, amount from spent order by amount desc",
                 "if row.amount > 10", "say upper(row.label) + ' ' + row.amount", "else", "say row.label + ' ' + row.amount", "end", "end"):
        x.cmd(line)
    ok, lines = x.cmd("end")
    check("program keeps lines under a name", ok and lines == ["Program report kept: 8 lines."], lines)
    report = ["TRAIN 42.00", "coffee 3.50", "bread 1.20"]
    ok, lines = x.cmd("report")
    check("the name of a program runs it", ok and lines == report, lines)
    listed = x.table("programs")
    check("programs lists it", listed == [{"name": "report", "lines": "8", "runs": "when asked"}], listed)
    ok, lines = x.cmd("show report")
    check("show gives its lines back", ok and lines[0] == "program report" and lines[3] == "        if row.amount > 10" and lines[-1] == "end", lines)
    x.type("report")
    x.key("ret")
    check("a program typed at the keyboard", [row.strip() for row in body(x)[-4:-1]] == report, body(x)[-5:])
    x.type("let floor = 3")
    x.key("ret")
    x.type("while floor > 0")
    x.key("ret")
    check("the screen shows that a block is open", body(x)[-1].strip() == "..>", body(x)[-2:])
    x.type("let floor = floor - 1")
    x.key("ret")
    x.type("end")
    x.key("ret")
    x.type("say 'floor is ' + floor")
    x.key("ret")
    check("a loop typed at the keyboard", body(x)[-2].strip() == "floor is 0", body(x)[-3:])
    for line in ("program broken", "say 'before'", "let x = 1 / 0"):
        x.cmd(line)
    x.cmd("end")
    ok, lines = x.cmd("broken")
    check("a program that fails says where", not ok and lines == ["before", "Division by zero.", "Stopped at line 2 of broken."], lines)
    ok, lines = x.cmd("while 1 = 1")
    x.cmd("let floor = floor + 1")
    ok, lines = x.cmd("end")
    check("a loop that never ends is stopped", not ok and lines[0] == "This loop went round 10000 times and was stopped.", lines)
    ok, lines = x.cmd("help l")
    check("help l describes the language", ok and lines[0].startswith("XilaScript") and len(lines) == 23, lines[:2])
    x.cmd("drop program broken")

    # programs that run by themselves
    x.cmd("create table heard (what text)")
    for line in ("on insert into spent", "say 'saw ' + new.label", "insert into heard values ('added ' || new.label || ', row ' || id)"):
        x.cmd(line)
    ok, lines = x.cmd("end")
    wanted = ["Program on_insert_spent kept: 2 lines.", "It runs on insert into spent. What it says goes to the log."]
    check("on: a program is kept to run when rows are added", ok and lines == wanted, lines)
    x.cmd("insert into spent values ('tea', 2), ('milk', 1)")
    time.sleep(0.3)
    rows = [row["what"] for row in x.table("select what from heard")]
    check("on: it ran for each row, with the row", rows == ["added tea, row 4", "added milk, row 5"], rows)
    # the time is known there too, beside the names of what happened
    x.cmd("create table dated (label text, day text)")
    for line in ("on insert into heard", "insert into dated values (new.what, today)", "end"):
        x.cmd(line)
    x.cmd("insert into heard values ('a day')")
    time.sleep(0.3)
    rows = x.table("select label, day from dated")
    check("on: a program that hears a row knows the day", len(rows) == 1 and rows[0]["label"] == "a day" and re.fullmatch(r"20\d\d-\d\d-\d\d", rows[0]["day"]), rows)
    x.cmd("drop program on_insert_heard")
    x.cmd("drop table dated")
    x.cmd("screen 1")
    log_rows = [row.strip() for row in x.peek()]
    x.cmd("screen 0")
    check("on: what it says is in the log", "on_insert_spent: saw tea" in log_rows and "on_insert_spent: saw milk" in log_rows, log_rows[-6:])
    for line in ("on notify ping", "mood intense"):
        x.cmd(line)
    x.cmd("end")
    x.cmd("delete from heard")
    for line in ("every 1 second", "insert into heard values ('tick')"):
        x.cmd(line)
    x.cmd("end")
    time.sleep(3.6)
    ticks = int(x.table("select count(*) from heard")[0]["count"])
    check("every: a program runs again and again", 2 <= ticks <= 4, ticks)
    listed = {row["name"]: row["runs"] for row in x.table("programs")}
    wanted = {"report": "when asked", "on_insert_spent": "on insert into spent", "on_notify_ping": "on notify ping", "every_1_second": "every 1 second"}
    check("programs says when each one runs", listed == wanted, listed)
    x.cmd("drop program every_1_second")
    after = int(x.table("select count(*) from heard")[0]["count"])
    time.sleep(1.5)
    check("a program that is removed runs no more", int(x.table("select count(*) from heard")[0]["count"]) == after, after)
    x.cmd("drop program on_insert_spent")
    x.cmd("drop table heard")
    x.cmd("drop table spent")

    # the disk
    x.cmd("create table kept (label text, amount decimal(10,3))")
    x.cmd("insert into kept values ('first', 1.5), ('second', -0.125), ('gone', 9)")
    x.cmd("delete from kept where label = 'gone'")
    kept = [{"id": "1", "label": "first", "amount": "1.500"}, {"id": "2", "label": "second", "amount": "-0.125"}]
    check("a table on the disk", x.table("select * from kept") == kept, x.table("select * from kept"))
    elsewhere = {row["block"]: row["lit"] for row in x.table("select block, lit from city where plot = 'elsewhere'")}
    check("city: the disk is there, lit by the change", elsewhere.get("disk") == "yes", elsewhere)
    started = time.time()
    for n in range(20):
        x.cmd("update kept set amount = %d where id = 2" % n)
    x.cmd("update kept set amount = -0.125 where id = 2")
    check("a change is safe on the disk in well under a second", time.time() - started < 8, time.time() - started)

    # restart
    x.serial.sendall(b"reboot\r")
    try:
        x.read_until_prompt(10)  # the prompt that follows the command
        again = "\n".join(x.read_until_prompt(60))
    except Exception as error:  # noqa: BLE001
        again = str(error)
    check("reboot starts the system again", "XilaOS 0.1 starting" in again and "System ready." in again, again[-300:])
    check("after a reboot the database is found again", "Database ............ on the disk, 3 tables" in again, again[-400:])
    check("and the table is as it was", x.table("select * from kept") == kept, x.table("select * from kept"))

# The machine is stopped and started again: what was written is still there.
with Xila(disk=disk) as x:
    log = "\n".join(x.boot_log)
    check("after the machine stops the database is found again", "Database ............ on the disk, 3 tables" in log, log)
    check("and the table is as it was", x.table("select * from kept") == kept, x.table("select * from kept"))
    ok, lines = x.cmd("show report")
    check("and the program is still there", ok and len(lines) == 10 and lines[0] == "program report", lines)
    x.cmd("notify ping")
    time.sleep(0.3)
    ok, lines = x.cmd("mood")
    check("and the program that waits for an event waits again", ok and lines == ["Mood is intense."], lines)
    x.cmd("mood auto")
    ok, lines = x.cmd("report")
    check("its table is gone, and it says so", not ok and lines == ["Unknown table: spent. Type tables to list them.", "Stopped at line 2 of report."], lines)
    ok, lines = x.cmd("insert into kept values ('third', 3)")
    check("ids go on from where they were: 3 was given before", ok and lines == ["1 row added to kept, with id 4."], lines)
    # enough rows for several pages
    x.cmd("create table many (n int, label text)")
    for batch in range(40):
        x.cmd("insert into many values " + ", ".join("(%d, 'row %d')" % (n, n) for n in range(batch * 8, batch * 8 + 8)))
    total = x.table("select count(*), sum(n) from many")
    check("a table over several pages", total == [{"count": "320", "sum": str(sum(range(320)))}], total)
    # conditions: among, between, not like
    found = x.table("select count(*) from many where n in (1, 5, 9) or n between 100 and 102")
    check("select: in and between", found == [{"count": "6"}], found)
    found = x.table("select label from many where label not like 'row 1%' and id between 1 and 3")
    check("select: not like", found == [{"label": "row 0"}, {"label": "row 2"}], found)
    ok, lines = x.cmd("select n from many where n in (1,2,3,4,5,6,7,8,9,10,11)")
    check("select: a list after in has its limit, and says so", not ok and lines == ["A list after IN takes ten values at most."], lines)
    # a calculation made of functions over the rows
    found = x.table("select sum(n) / count(*) as mean, max(n) - min(n) as spread, round(avg(n), 0) from many")
    check("select: a calculation on functions over the rows", found == [{"mean": "159.5", "spread": "319", "round(avg(n),0)": "160"}], found)
    ok, lines = x.cmd("select sum(n) + n from many")
    check("select: a column next to such a function is refused", not ok and lines == ["Next to a function over the rows, a column must be inside one."], lines)

    # a row is reached by its id: the pages on the way down to it, not those of the table
    def pages(line):
        read = lambda: int({row["item"]: row["value"] for row in x.table("disk")}["pages read"])
        before = read()
        answer = x.table(line)
        return read() - before, answer
    whole, answer = pages("select count(*) from many where label like 'row%'")
    one, row = pages("select n, label from many where id = 300")
    check("select: a row is reached by its id without reading the table", row == [{"n": "299", "label": "row 299"}] and one == 2 and whole > one, (one, whole, row))
    few, rows = pages("select sum(n) from many where id between 10 and 12 and n != 10")
    check("select: and so are the rows between two ids", rows == [{"sum": "20"}] and few <= 3, (few, rows))
    ok, lines = x.cmd("update many set label = 'changed' where id = 300")
    check("update: a row is changed by its id", ok and lines == ["1 row changed in many."] and x.table("select label from many where id in (299, 300, 301)") == [{"label": "row 298"}, {"label": "changed"}, {"label": "row 300"}], lines)
    x.cmd("update many set label = 'row 299' where id = 300")

    # two tables joined: beside each row of the first, the rows of the second the condition finds
    found = x.table("select k.label as kept, m.label from kept k join many m on m.n = k.id order by k.id")
    wanted = [{"kept": "first", "label": "row 1"}, {"kept": "second", "label": "row 2"}, {"kept": "third", "label": "row 4"}]
    check("select: two tables are joined", found == wanted, found)
    cost, found = pages("select m.n, k.label from many m left join kept k on k.id = m.n where m.id between 2 and 5")
    wanted = [{"n": "1", "label": "first"}, {"n": "2", "label": "second"}, {"n": "3", "label": ""}, {"n": "4", "label": "third"}]
    check("select: a left join keeps the rows that find nothing, and finds the others by their id", found == wanted and cost <= 8, (cost, found))
    found = x.table("select count(*), sum(k.amount) from many m join kept k on k.id = m.n")
    check("select: functions over the rows of a join", found == [{"count": "3", "sum": "4.375"}], found)
    found = x.table("select t.name, count(*) from tables t join columns c on c.table_name = t.name where t.name in ('kept', 'many') group by t.name")
    check("select: the views of the system are joined too", found == [{"name": "kept", "count": "3"}, {"name": "many", "count": "3"}], found)
    ok, lines = x.cmd("select id from many m join kept k on k.id = m.n")
    check("select: a column both tables have must be named with its table", not ok and lines == ["Both tables have a column id: write its table before it, as in t.id"], lines)

    # every page of every table read back, with its checksum
    ok, lines = x.cmd("disk check")
    used = int({row["item"]: row["value"] for row in x.table("disk")}["used"].split()[0])
    check("disk check: the tables are read whole", ok and lines == ["The tables were read whole: %d pages (%d KB), every one is right." % (used // 4, used)] and used >= 16, lines)
    ok, lines = x.cmd("disk sideways")
    check("disk: an unknown word is an error", not ok and lines == ["Usage: disk, disk check, or disk erase"], lines)
    x.cmd("delete from many where n >= 300")
labels = [{"id": "1", "label": "first"}, {"id": "2", "label": "second"}, {"id": "4", "label": "third"}]

# The power is cut while a statement is written, several times. Each time
# the statement must be there whole or not at all, and the rest untouched.
whole, cuts, rows = power_cuts(lambda: Xila(disk=disk), 300, PAUSES, lambda x: x.table("select id, label from kept") == labels)
with Xila(disk=disk) as x:
    ok, count = many_is_whole(x, rows)
    cuts.append(count - rows)
    check("after power cuts a statement is there whole or not at all", whole and ok, cuts)
    print("      rows kept at each cut: %s" % cuts)
    ok, lines = x.cmd("insert into many values (%d, 'after')" % count)
    check("and the database takes changes again", ok and x.table("select count(*) from many") == [{"count": str(count + 1)}], lines)

# A machine without a disk keeps its tables in memory, and says so.
with Xila() as x:
    log = "\n".join(x.boot_log)
    # the disk the machine started from is a SATA disk, and not one of XilaOS
    check("without a disk for it the boot log says so", "no XilaOS partition: not used" in log and "Database ............ in memory" in log, log)
    sizes = re.search(r"Screen sizes \.+ ((?:\d+x\d+ ?)+)", log)
    check("the boot log lists the sizes of screen the firmware offers", sizes and "1280x800" in sizes.group(1).split() and "Panel ............... " in log, log)
    check("the screen is the size the interface was designed on", "Screen .............. 1280x800, 50 rows of 80 characters" in log, log)
    check("the kernel maps the memory itself", "Page tables ......... 64 GB mapped, pages of 2 MB" in log, log)
    machine = {row["item"]: row["value"] for row in x.table("select * from machine")}
    check("without a disk the tables are in memory", machine.get("stored tables") == "in memory, lost when the machine stops", machine)
    x.cmd("create table notes (title text)")
    ok, lines = x.cmd("insert into notes values ('here')")
    check("and they work", ok and x.table("select title from notes") == [{"title": "here"}], lines)

# A disk that holds something else is not touched.
other = blank_disk(os.path.join(ROOT, "build", "other.img"), 16)
with open(other, "r+b") as image:
    image.write(b"somebody else's data, which must stay as it is")
before = sha(open(other, "rb").read())
with Xila(disk=other) as x:
    log = "\n".join(x.boot_log)
    check("a disk that holds something else is not used", "Disk ................ virtio, 16 MB, holds something else: not used" in log, log)
    x.cmd("create table notes (title text)")
    ok, lines = x.cmd("insert into notes values ('in memory')")
    check("and the tables are in memory", ok and x.table("select value from machine where item = 'stored tables'") == [{"value": "in memory, lost when the machine stops"}], lines)
check("and the disk is exactly as it was", sha(open(other, "rb").read()) == before)

# A SATA disk, as real machines have. It holds another system; XilaOS has
# one partition of it and must write nowhere else.
sata = os.path.join(ROOT, "build", "sata.img")
theirs, ours = partitioned_disk(sata, 64, [(LINUX_PARTITION, 1, 8), (XILA_PARTITION, 9, 40)])
with open(sata, "r+b") as image:
    image.seek(theirs[0])
    image.write(b"\xab" * (theirs[1] - theirs[0]))
    image.seek(ours[1])
    image.write(b"\xcd" * 65536)


def outside(path, inside):
    data = open(path, "rb").read()
    return sha(data[:inside[0]] + data[inside[1]:])


before = outside(sata, ours)
with Xila(sata=sata) as x:
    log = "\n".join(x.boot_log)
    check("sata: the partition of XilaOS is found", "Disk ................ sata, QEMU HARDDISK, partition of 40 MB" in log, log)
    check("sata: an empty partition gets a new database", "Database ............ new, on the disk" in log, log)
    about = {row["item"]: row["value"] for row in x.table("about")}
    check("sata: about names the partition", about.get("disk") == "sata, partition of 40 MB", about)
    x.cmd("create table many (n int, label text)")
    for batch in range(25):
        x.cmd("insert into many values " + ", ".join("(%d, 'row %d')" % (n, n) for n in range(batch * 8, batch * 8 + 8)))
    check("sata: a table over several pages", many_is_whole(x, 200) == (True, 200), x.table("select count(*), sum(n) from many"))
# the same disk on a processor with pages of 1 GB, as recent ones have
with Xila(sata=sata, cpu="max") as x:
    log = "\n".join(x.boot_log)
    check("sata: after the machine stops the database is found again", "Database ............ on the disk, 1 table" in log, log)
    check("the kernel maps the memory itself, with large pages", "Page tables ......... 512 GB mapped, pages of 1 GB" in log, log)
    check("with pages of 1 GB too, only the screen is write combining", "Screen memory ....... write combining, 1000 pages of 4 KB" in log, log)
    check("sata: and the table is as it was", many_is_whole(x, 200) == (True, 200), x.table("select count(*), sum(n) from many"))
whole, cuts, rows = power_cuts(lambda: Xila(sata=sata), 200, PAUSES[::2])
with Xila(sata=sata) as x:
    ok, count = many_is_whole(x, rows)
    cuts.append(count - rows)
    check("sata: after power cuts a statement is there whole or not at all", whole and ok, cuts)
    print("      rows kept at each cut: %s" % cuts)
check("sata: nothing was written outside the partition", outside(sata, ours) == before)

# A SATA disk without a partition for XilaOS is not touched, even empty.
plain = os.path.join(ROOT, "build", "plain.img")
partitioned_disk(plain, 16, [(LINUX_PARTITION, 1, 14)])
before = sha(open(plain, "rb").read())
with Xila(sata=plain) as x:
    log = "\n".join(x.boot_log)
    check("sata: a disk without a partition for XilaOS is not used", "Disk ................ 2 sata disks, no XilaOS partition: not used" in log, log)
    ok, lines = x.cmd("create table notes (title text)")
    check("sata: and the tables are in memory", ok and "Database ............ in memory" in log, lines)
check("sata: and that disk is exactly as it was", sha(open(plain, "rb").read()) == before)

# A partition of XilaOS that holds something else is not touched either.
(taken,) = partitioned_disk(plain, 16, [(XILA_PARTITION, 1, 14)])
with open(plain, "r+b") as image:
    image.seek(taken[0])
    image.write(b"somebody else's data, which must stay as it is")
before = sha(open(plain, "rb").read())
with Xila(sata=plain) as x:
    log = "\n".join(x.boot_log)
    check("sata: a partition that holds something else is not used", "Disk ................ sata partition, holds something else: not used" in log, log)
    x.cmd("create table notes (title text)")
    ok, lines = x.cmd("disk")
    check("disk: says that the disk can be given to XilaOS", ok and lines[-1] == "The disk was not used. To give it to XilaOS, type: disk erase", lines)
    ok, lines = x.cmd("disk erase")
    check("disk erase: says what will be lost and asks again", ok and lines[0].startswith("This erases the 14 MB") and lines[-1] == "To do it, type: disk erase yes", lines)
check("sata: and it is exactly as it was", sha(open(plain, "rb").read()) == before)

# The same partition, given to XilaOS on purpose.
before = outside(plain, taken)
with Xila(sata=plain) as x:
    ok, lines = x.cmd("disk erase yes")
    check("disk erase yes: the partition is taken", ok and lines == ["The disk is erased. The tables are now kept on it."], lines)
    about = {row["item"]: row["value"] for row in x.table("disk")}
    check("disk: the tables are on it", about.get("disk") == "sata, 14 MB" and about.get("stored tables") == "on disk, transaction 1", about)
    ok, lines = x.cmd("disk erase yes")
    check("disk erase: there is nothing more to erase", not ok and lines == ["There is no disk to erase."], lines)
    x.cmd("create table notes (title text)")
    x.cmd("insert into notes values ('on the erased disk')")
with Xila(sata=plain) as x:
    log = "\n".join(x.boot_log)
    check("disk erase: the database is found at the next start", "Database ............ on the disk, 1 table" in log, log)
    check("disk erase: with what was put in it", x.table("select title from notes") == [{"title": "on the erased disk"}], None)
check("disk erase: nothing was written outside the partition", outside(plain, taken) == before)

# The network. QEMU plugs the card into a small network of its own making,
# which gives addresses and whose gateway answers a ping; and it keeps
# every frame that goes by, for the checks at the end.
def internet_sum(data):
    """The checksum of the internet protocols, worked out here."""
    if len(data) % 2:
        data += b"\0"
    total = sum(struct.unpack(">%dH" % (len(data) // 2), data))
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return total


def frames_of(path):
    """The frames of a capture file, as bytes."""
    data = open(path, "rb").read()
    at, found = 24, []
    while at + 16 <= len(data):
        length = struct.unpack_from("<I", data, at + 8)[0]
        found.append(data[at + 16:at + 16 + length])
        at += 16 + length
    return found


with Xila() as x:
    ok, lines = x.cmd("net")
    check("net: a machine without a card XilaOS drives says so", not ok and lines == ["XilaOS drives no network card of this machine. Type devices to see what it has."], lines)
    check("the boot log says so too", "Network ............. no card XilaOS drives" in "\n".join(x.boot_log), x.boot_log)

for card, name in (("e1000e", "Intel 82574L"), ("e1000", "Intel 82540EM")):
    with Xila(net=card) as x:
        log = "\n".join(x.boot_log)
        check("net (%s): the boot log names the card and its address" % card, "Network ............. %s, 52:54:00:12:34:56" % name in log, log)
        told = {}
        for _ in range(40):
            told = {row["item"]: row["value"] for row in x.table("net")}
            if told.get("address") == "10.0.2.15":
                break
            time.sleep(0.25)
        wanted = {"card": name, "its address": "52:54:00:12:34:56", "cable": "1000 megabits per second", "address": "10.0.2.15", "mask": "255.255.255.0", "gateway": "10.0.2.2"}
        check("net (%s): the network gives an address, a mask and a gateway" % card, all(told.get(item) == value for item, value in wanted.items()), told)
        check("net (%s): for a day" % card, told.get("given by the network") in ("for 23 hours more", "for 24 hours more"), told)
        ok, lines = x.cmd("net ping 10.0.2.2")
        check("net ping (%s): the gateway answers" % card, ok and re.fullmatch(r"10\.0\.2\.2 answered in [\d.]+ ms\.", lines[0]), lines)
        ok, lines = x.cmd("net ping 10.0.2.99")
        check("net ping (%s): an address nobody has does not, and that is a failure" % card, not ok and lines == ["10.0.2.99 did not answer within a second."], lines)
        # what the card counted is what it counted until now, not until the last second
        sent = int({row["item"]: row["value"] for row in x.table("net")}["frames sent"])
        x.cmd("net ping 10.0.2.2")
        now = int({row["item"]: row["value"] for row in x.table("net")}["frames sent"])
        check("net (%s): a frame that was just sent is counted" % card, now == sent + 1, (sent, now))
        raw = {row["register"]: int(row["value"], 16) for row in x.table("net registers")}
        # as the driver set them: receiving and sending are on, the link is up, the address is the card's
        on = raw.get("RCTL", 0) & 2 and raw.get("TCTL", 0) & 2 and raw.get("STATUS", 0) & 2 and raw.get("TDBAL", 0) != 0
        moving = raw.get("TDH") == raw.get("TDT") == raw.get("given to the card") and raw.get("TDLEN") == 256
        check("net registers (%s): the card is as the driver set it" % card, len(raw) == 32 and on and moving, raw)
        ok, lines = x.cmd("net ping there")
        check("net ping (%s): wants an address" % card, not ok and lines == ["Type net ping and an address: net ping 192.168.1.1"], lines)
        driven = [row for row in x.table("devices") if row["kind"] == "Ethernet"]
        check("devices (%s): the card is driven" % card, len(driven) == 1 and driven[0]["driver"] == "XilaOS", driven)
        told = {row["item"]: row["value"] for row in x.table("net")}
        clean = told["damaged on the way"] == "0" and told["missed, no room"] == "0" and told["not sent, card busy"] == "0"
        check("net (%s): frames are counted, none damaged, missed or refused" % card, int(told["frames received"]) >= 3 and int(told["frames sent"]) >= 4 and clean, told)
        # the card reset and set up again: it asks for its address anew, and the network is there again
        ok, lines = x.cmd("net restart")
        check("net restart (%s): the card comes back" % card, ok and lines == ["The card was reset and set up again. It asks the network for an address."], lines)
        for _ in range(40):
            told = {row["item"]: row["value"] for row in x.table("net")}
            if told.get("address") == "10.0.2.15":
                break
            time.sleep(0.25)
        ok, lines = x.cmd("net ping 10.0.2.2")
        check("net restart (%s): with its address, and the gateway answers again" % card, told.get("address") == "10.0.2.15" and lines[0].startswith("10.0.2.2 answered in "), (told, lines))
    # What XilaOS put on the cable, read by other means: every frame is
    # whole, and its checksums are right.
    ours = [frame for frame in frames_of(os.path.join(ROOT, "build", "net.pcap")) if frame[6:12] == bytes.fromhex("525400123456")]
    kinds, wrong = set(), []
    for frame in ours:
        if len(frame) < 60:
            wrong.append("a frame of %d bytes" % len(frame))
        elif frame[12:14] == b"\x08\x06":
            kinds.add("arp")
        elif frame[12:14] == b"\x08\x00":
            total = struct.unpack_from(">H", frame, 16)[0]
            packet = frame[14:14 + total]
            if internet_sum(packet[:20]) != 0xFFFF:
                wrong.append("an IP header")
            if packet[9] == 1:
                kinds.add("ping")
                if internet_sum(packet[20:]) != 0xFFFF:
                    wrong.append("a ping")
            elif packet[9] == 17:
                kinds.add("udp")
                if internet_sum(packet[12:20] + bytes([0, 17]) + packet[24:26] + packet[20:]) != 0xFFFF:
                    wrong.append("a UDP message")
        else:
            wrong.append("a frame of kind " + frame[12:14].hex())
    check("net (%s): what was sent is whole, with right checksums" % card, kinds == {"arp", "ping", "udp"} and not wrong, (kinds, wrong))

# The prompt, from another machine of the network: closed until `remote
# on` is typed at the machine, then open to who has the code.
import socket
from remote import Machine
probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
probe.bind(("127.0.0.1", 0))
forwarded = probe.getsockname()[1]
probe.close()
with Xila(net="e1000e", forward=forwarded) as x:
    for _ in range(40):
        if {row["item"]: row["value"] for row in x.table("net")}.get("address") == "10.0.2.15":
            break
        time.sleep(0.25)
    told = {row["item"]: row["value"] for row in x.table("remote")}
    check("remote: closed when the machine starts", told == {"this prompt": "closed to the network", "address": "10.0.2.15"}, told)
    stranger = Machine("127.0.0.1", "AAAA-AAAA", forwarded)
    check("remote: closed, it answers nobody", stranger.cmd("uptime", wait=0.7, tries=1) == (None, []))
    ok, lines = x.cmd("remote on")
    opened = re.fullmatch(r"This prompt is open to the local network\. The code is ([A-Z2-9]{4}-[A-Z2-9]{4})\.", lines[0]) if lines else None
    check("remote on: opens it and shows a code", ok and opened and len(lines) == 3, lines)
    code = opened.group(1) if opened else "?"
    check("remote on: and the line to type on the other machine", lines[1:2] == ["From another machine: python3 tools/remote.py 10.0.2.15 " + code], lines)
    far = Machine("127.0.0.1", code, forwarded)
    ok, lines = far.cmd("uptime")
    check("remote: a command with the code is run and answered", ok and len(lines) == 1 and re.fullmatch(r"Up for [\d.]+ s\.", lines[0]), lines)
    # an answer of several messages, the same as the serial port gives
    check("remote: a long answer comes whole, as on the serial port", far.cmd("help c") == x.cmd("help c") and len(x.cmd("help c")[1]) > 30)
    check("remote: a table too", far.cmd("select name from commands") == x.cmd("select name from commands"))
    check("remote: an error is an error", far.cmd("frobnicate") == (False, ["Unknown command: frobnicate"]))
    check("remote: the code without its dash, in small letters", Machine("127.0.0.1", code.replace("-", "").lower(), forwarded).cmd("uptime")[0] is True)
    check("remote: install is refused there", far.cmd("install 1 yes") == (False, ["install takes a disk: it is typed at the machine itself."]))
    check("remote: and so is opening the prompt", far.cmd("remote on") == (False, ["remote on is typed at the machine itself."]))
    check("remote: and erasing the disk of the tables", far.cmd("disk erase") == (False, ["disk erase takes a disk: it is typed at the machine itself."]))
    check("remote: in the form that does it too", far.cmd("disk erase yes") == (False, ["disk erase takes a disk: it is typed at the machine itself."]))
    # a block over several messages, and names that are this prompt's own
    block = [far.cmd("let far_away = 6"), far.cmd("if far_away > 5"), far.cmd("say 'more than five'"), far.cmd("end")]
    check("remote: a block is typed over several commands", block == [(True, []), (True, ["#more"]), (True, ["#more"]), (True, ["more than five"])], block)
    check("remote: its names are its own", x.cmd("say far_away")[0] is False)
    # a program written there and left to run by itself is no way around
    planted = [far.cmd("on notify way_around"), far.cmd("web on"), far.cmd("end"), far.cmd("notify way_around 'now'")]
    time.sleep(0.5)
    x.key("alt-1")
    log = [row.strip() for row in x.peek() if "way_around" in row]
    x.key("alt-0")
    web_is = {row["item"]: row["value"] for row in x.table("web")}.get("the tables")
    check("remote: a program written there, running by itself, opens nothing either", all(ok for ok, _ in planted) and web_is == "closed to the network" and log[:1] == ["on_notify_way_around: web on is typed at the machine itself."], (planted, web_is, log))
    far.cmd("drop program on_notify_way_around")
    # who the companion is says where its key is sent: that table is not made, named or changed from here
    wanted = (False, ["The table companions is changed at the machine: it says where the key goes."])
    held = [far.cmd("create table companions (name text, place text, model text, key text, character text)"), far.cmd("create table elsewhere (name text)"), far.cmd("alter table elsewhere rename to companions"),
            x.cmd("create table companions (name text, place text, model text, key text, character text)"), far.cmd("update companions set place = 'https://elsewhere.example/v1'"), far.cmd("select count(*) from companions")[0]]
    check("remote: the table that says where the key goes is not made, named or changed from there", held == [wanted, (True, ["Table elsewhere created."]), wanted, (True, ["Table companions created."]), wanted, True], held)
    far.cmd("drop table elsewhere")
    x.cmd("drop table companions")
    # the same message twice, as a network may deliver it: the command runs once
    x.cmd("create table doors (n int)")
    raw = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    raw.settimeout(2)
    answers = []
    for _ in range(2):
        raw.sendto(("%s 5 insert into doors values (1)" % code).encode(), ("127.0.0.1", forwarded))
        answers.append(raw.recvfrom(4096)[0])
    rows = x.table("select count(*) from doors")
    check("remote: a message that comes twice is run once and answered twice", answers[0] == answers[1] and answers[0].startswith(b"5 1 1\n") and rows == [{"count": "1"}], (answers, rows))
    # what this prompt listens to comes with its next command
    far.cmd("listen doors")
    x.cmd("insert into doors values (2)")
    ok, lines = far.cmd("")
    check("remote: it hears what it listens to", ok and lines == ["#event doors\tinsert 2"], lines)
    # a wrong code gets no answer, and is counted
    check("remote: a wrong code gets no answer", stranger.cmd("uptime", wait=0.7, tries=1) == (None, []))
    told = {row["item"]: row["value"] for row in x.table("remote")}
    check("remote: the machine shows who asked and how often", told.get("this prompt") == "open, on port 4000" and told.get("code") == code and told.get("wrong codes") == "1" and told.get("last asked from") == "10.0.2.2" and int(told.get("commands run", 0)) >= 12, told)
    # after twenty wrong codes it closes itself: the right one comes too late
    for _ in range(19):
        raw.sendto(b"GUESSING 1 uptime", ("127.0.0.1", forwarded))
    time.sleep(0.5)
    told = {row["item"]: row["value"] for row in x.table("remote")}
    check("remote: it closes itself after twenty wrong codes", told.get("this prompt") == "closed itself after 20 wrong codes" and far.cmd("uptime", wait=0.7, tries=1) == (None, []), told)
    ok, lines = x.cmd("remote on")
    again = lines[0].split()[-1].rstrip(".")
    check("remote on: a new code each time", ok and again != code and far.cmd("uptime", wait=0.7, tries=1) == (None, []), lines)
    far = Machine("127.0.0.1", again, forwarded)
    check("remote off: closes it, also from afar", far.cmd("remote off") == (True, ["This prompt is closed to the network."]) and far.cmd("uptime", wait=0.7, tries=1) == (None, []))
    raw.close()

# The tables as pages, for a browser on another machine of the network:
# closed until `web on` is typed at the machine, then given at an address
# that holds a code. They only read.
import urllib.error
import urllib.request
probe = socket.socket()
probe.bind(("127.0.0.1", 0))
browsed = probe.getsockname()[1]
probe.close()


def page(path, method="GET"):
    """What a browser gets at a path of the guest: (status, text), or (None, why) without an answer."""
    try:
        asked = urllib.request.Request("http://127.0.0.1:%d%s" % (browsed, path), method=method)
        with urllib.request.urlopen(asked, timeout=10) as answer:
            return answer.status, answer.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()
    except OSError as error:
        return None, repr(error)


with Xila(net="e1000e", web=browsed) as x:
    for _ in range(40):
        if {row["item"]: row["value"] for row in x.table("net")}.get("address") == "10.0.2.15":
            break
        time.sleep(0.25)
    told = {row["item"]: row["value"] for row in x.table("web")}
    check("web: closed when the machine starts", told == {"the tables": "closed to the network"}, told)
    check("web: closed, a browser is refused", page("/")[0] is None, page("/"))
    ok, lines = x.cmd("web on")
    opened = re.fullmatch(r"http://10\.0\.2\.15/([A-Z2-9]{4}-[A-Z2-9]{4})/", lines[1]) if len(lines) == 3 else None
    check("web on: opens the pages and shows their address", ok and opened and lines[0] == "The tables can be read from a browser on the local network, at:", lines)
    code = opened.group(1) if opened else "?"
    x.cmd("create table notes (title text, stars int)")
    x.cmd("insert into notes values ('<i>tags</i> & such', 4)")
    x.cmd("key set openrouter sk-test-on-the-web")
    status, text = page("/%s/" % code)
    listed = set(re.findall(r'<td><a href="/%s/t/(\w+)">\1</a></td>' % code, text))
    names = {row["name"] for row in x.table("select name from tables")}
    check("web: the first page lists every table, with a link to each", status == 200 and listed == names and "notes" in names and text.startswith("<!doctype html>"), (status, listed, names))
    hidden = [page("/%s/t/%s" % (code, table)) for table in ("kept_keys", "KEPT_KEYS", "tables", "columns")]
    check("web: no page shows a key, nor where keys are kept", "kept_keys" not in text and [status for status, _ in hidden] == [404, 404, 200, 200] and not any("sk-test" in said or "kept_keys" in said for _, said in hidden[2:]), [status for status, _ in hidden])
    status, text = page("/%s/t/notes" % code)
    check("web: the page of a table shows its rows, as text and never as a page of their own", status == 200 and "<td>&lt;i&gt;tags&lt;/i&gt; &amp; such</td><td>4</td>" in text and "<i>" not in text and "<p>1 row.</p>" in text, text[-300:])
    status, text = page("/%s/t/commands" % code)
    cells = re.findall(r"<tr><td>(\w+)</td>", text)
    check("web: a long table comes whole in several segments, cut where the prompt cuts it", status == 200 and len(text) > 1300 and cells == [row["name"] for row in x.table("select name from commands")][:32] and "The first 32 rows of" in text, (len(text), cells[:3]))
    check("web: reading changes nothing", x.table("select title, stars from notes") == [{"title": "<i>tags</i> & such", "stars": "4"}])
    plain = page("/")
    check("web: without the code nothing is found, whatever is asked", plain[0] == 404 and "XilaOS" not in plain[1] and page("/favicon.ico") == plain and page("/t/notes") == plain, plain)
    check("web: a table that is not there is not found", page("/%s/t/nothing" % code)[0] == 404 and page("/%s/elsewhere" % code)[0] == 404)
    check("web: it only gives pages", page("/%s/" % code, "POST")[0] == 405 and page("/%s/t/notes" % code, "DELETE")[0] == 405)
    start = time.time()
    many = [page("/%s/t/tasks" % code)[0] for _ in range(40)]
    check("web: forty pages one after the other, each at once", many == [200] * 40 and time.time() - start < 10, (many.count(200), time.time() - start))
    told = {row["item"]: row["value"] for row in x.table("web")}
    check("web: the machine shows the address, who asked and how often", told.get("at") == "http://10.0.2.15/%s/" % code and told.get("last asked from") == "10.0.2.2" and int(told.get("pages given", 0)) >= 43 and told.get("wrong codes") == "0", told)
    bar = [row for row in x.peek() if "OVERVIEW" in row]
    check("web: the title bar says the pages are open, and to whom", bar and bar[0].rstrip().endswith("web: 10.0.2.2  ="), bar)
    # a wrong code that has the shape of one is counted; twenty close the pages
    for _ in range(20):
        page("/AAAA-AAAA/")
    time.sleep(0.3)
    told = {row["item"]: row["value"] for row in x.table("web")}
    check("web: it closes itself after twenty wrong codes", told.get("the tables") == "closed itself after 20 wrong codes" and page("/%s/" % code)[0] is None, told)
    ok, lines = x.cmd("web on")
    again = lines[1].rstrip("/").split("/")[-1]
    check("web on: a new code each time", ok and again != code and page("/%s/" % code)[0] == 404 and page("/%s/" % again)[0] == 200, lines)
    ok, lines = x.cmd("web off")
    check("web off: closes them", ok and lines == ["The tables are closed to the network."] and page("/%s/" % again)[0] is None, lines)
    check("web: the rest of the network is as it was", x.cmd("net ping 10.0.2.2")[1][0].startswith("10.0.2.2 answered in "))

# The calling side of the network: XilaOS asks a machine that serves pages,
# here a small server on this one, which the guest sees at 10.0.2.2.
import http.server
import threading
asked_of_us = []


import json
LONG = b"".join(b"line %d of a long page\n" % n for n in range(400))
asked_by, sent_to_us = [], []
model_asked, model_says = [], []


class Served(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        asked_of_us.append((self.path, self.headers.get("Host")))
        asked_by.append((self.request_version, self.headers.get("User-Agent")))
        if self.path == "/missing":
            self.send_response(404)
            self.end_headers()
            return
        if self.path in ("/chunks", "/cut-chunks"):
            # In chunks of every size, each after its length in hexadecimal,
            # some with a note after the length; an empty one ends it, and
            # lines may follow. Or no end at all: cut.
            self.protocol_version = "HTTP/1.1"
            self.close_connection = True
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            at, size = 0, 1
            while at < len(LONG):
                piece = LONG[at:at + size]
                self.wfile.write(b"%X%s\r\n" % (len(piece), b";note=%d" % size if size % 2 else b"") + piece + b"\r\n")
                at, size = at + size, size * 3 % 997 + 1
            if self.path == "/chunks":
                self.wfile.write(b"0\r\nX-After: the last chunk\r\n\r\n")
            return
        if self.path == "/long":
            body = LONG
        else:
            body = b"hello from the host\nyou asked for " + self.path.encode() + b"\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        if self.path == "/cut":
            # says more than it gives
            self.send_header("Content-Length", str(len(LONG) + 100))
            body = LONG
        elif self.path != "/to-the-end":
            # (that one says no length: it ends when we hang up)
            self.send_header("Content-Length", str(len(body)))
        else:
            body = LONG
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        text = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if self.path == "/v1/chat/completions":
            return self.model(text)
        sent_to_us.append((self.path, self.headers.get("Content-Type"), text))
        body = b"you sent " + text + b"\nto " + self.path.encode() + b"\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def model(self, text):
        """A made-up model: says what `model_says` holds next, as a model's server would."""
        try:
            asked = json.loads(text)
        except ValueError:
            asked = None
        model_asked.append((dict(self.headers), asked))
        answer = model_says.pop(0) if model_says else {"said": "I have nothing to say."}
        time.sleep(answer.get("wait", 0))
        body = answer.get("raw")
        if body is None:
            message = {"role": "assistant", "content": answer.get("said"), "refusal": None}
            # what it wants done: queries, lines to run, notes to keep
            wanted = [("select", "query", query) for query in answer.get("queries", [])] + [("run", "command", line) for line in answer.get("runs", [])] + [("remember", "fact", fact) for fact in answer.get("facts", [])]
            if wanted:
                # each as JSON written as a text, as models do
                message["tool_calls"] = [{"type": "function", "index": n, "id": "call_%d" % n, "function": {"name": name, "arguments": json.dumps({given: value})}} for n, (name, given, value) in enumerate(wanted)]
            body = b"\n   \n" + json.dumps({"id": "gen-test", "choices": [{"index": 0, "finish_reason": "stop", "message": message}], "usage": {"cost": 0.00002}}).encode()
        self.protocol_version = "HTTP/1.1"
        self.close_connection = True
        self.send_response(answer.get("status", 200))
        self.send_header("Content-Type", "application/json")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        for at in range(0, len(body), 100):
            self.wfile.write(b"%x\r\n" % len(body[at:at + 100]) + body[at:at + 100] + b"\r\n")
        self.wfile.write(b"0\r\n\r\n")

    def log_message(self, *words):
        pass


serving = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Served)
there = "10.0.2.2:%d" % serving.server_address[1]
threading.Thread(target=serving.serve_forever, daemon=True).start()
# a machine that takes the call and says nothing
mute = socket.socket()
mute.bind(("127.0.0.1", 0))
mute.listen(1)
with Xila(net="e1000e") as x:
    for _ in range(40):
        if {row["item"]: row["value"] for row in x.table("net")}.get("address") == "10.0.2.15":
            break
        time.sleep(0.25)
    ok, lines = x.cmd("net get %s/some/page" % there)
    check("net get: a page of another machine is asked for and shown", ok and lines == [there + " answered 200.", "hello from the host", "you asked for /some/page"], lines)
    check("net get: the machine was asked by its name, for that page", asked_of_us == [("/some/page", there)], asked_of_us)
    ok, lines = x.cmd("net get http://%s/missing" % there)
    check("net get: what is not there is said so, and is a failure", not ok and lines == [there + " answered 404."], lines)
    ok, lines = x.cmd("net get %s/long" % there)
    whole = "(and more: %d bytes in all)" % len(LONG)
    check("net get: of a long page the start is shown, and how long it is in all", lines[:2] == [there + " answered 200.", "line 0 of a long page"] and lines[-1] == whole and len(lines) == 10, lines)
    check("net get: the question says what asks, in the HTTP of today", asked_by[0] == ("HTTP/1.1", "XilaOS/0.1"), asked_by[:1])
    ok, lines = x.cmd("net get %s/chunks" % there)
    check("net get: a page that comes in chunks is put together", ok and lines[:2] == [there + " answered 200.", "line 0 of a long page"] and lines[8:] == ["line 7 of a long page", whole], lines)
    ok, lines = x.cmd("net get %s/to-the-end" % there)
    check("net get: a page with no length said goes on until they hang up", ok and lines[:2] == [there + " answered 200.", "line 0 of a long page"] and lines[-1] == whole, lines)
    ok, lines = x.cmd("net get %s/cut" % there)
    check("net get: a page shorter than the length it said was cut", not ok and lines == [there + " hung up before it had said everything."], lines)
    ok, lines = x.cmd("net get %s/cut-chunks" % there)
    check("net get: and so was one whose chunks have no end", not ok and lines == [there + " hung up before it had said everything."], lines)
    ok, lines = x.cmd("net post %s/notes 'big expense: ' || 2 * 21" % there)
    check("net post: a text is sent to a page, and what the page says is shown", ok and lines == [there + " answered 200.", "you sent big expense: 42", "to /notes"], lines)
    check("net post: the machine got the text, said to be plain text", sent_to_us == [("/notes", "text/plain", b"big expense: 42")], sent_to_us)
    ok, lines = x.cmd("net post %s/notes" % there)
    check("net post: wants a text", not ok and lines == ["Type net post, where, and a text: net post 192.168.1.20:8000/notes 'hello'"] and len(sent_to_us) == 1, lines)
    start = time.time()
    ok, lines = x.cmd("net get 10.0.2.2:1/")
    check("net get: a port nobody listens on does not answer, at once", lines == ["10.0.2.2:1 does not answer."] and time.time() - start < 2, (lines, time.time() - start))
    ok, lines = x.cmd("net get 10.0.2.2:%d/" % mute.getsockname()[1], timeout=30)
    check("net get: a machine that says nothing is given up after a while", lines == ["10.0.2.2:%d took the call and then said nothing." % mute.getsockname()[1]], lines)
    many = [x.cmd("net get %s/n%d" % (there, n))[1][:1] for n in range(20)]
    check("net get: twenty calls one after the other, each from a place that is freed", many == [[there + " answered 200."]] * 20, many)
    ok, lines = x.cmd("net get no where")
    check("net get: wants a place to ask", not ok and lines == ["Type net get and where to ask: net get 192.168.1.20:8000/page"], lines)
    check("net get: the rest of the network is as it was", x.cmd("net ping 10.0.2.2")[1][0].startswith("10.0.2.2 answered in "))

    # Names. The network QEMU makes up names a name server (10.0.2.3, which
    # passes the questions on to this machine's own). To check without the
    # Internet, the guest is told to ask a small one here instead.
    names_asked = []

    def name_server(listening):
        while True:
            try:
                question, who = listening.recvfrom(512)
            except OSError:
                return
            # the name of the question, word by word, each after its length
            at, words = 12, []
            while question[at]:
                words.append(question[at + 1:at + 1 + question[at]].decode())
                at += 1 + question[at]
            name = ".".join(words)
            names_asked.append(name)

            def answer(code, lines):
                # the question back, as it was asked, then the lines
                return question[:2] + bytes([0x81, 0x80 | code]) + struct.pack(">HHHH", 1, len(lines), 0, 0) + question[12:at + 5] + b"".join(lines)

            def line(kind, value):
                # about the name of the question (a pointer back to it), to be kept a minute
                return b"\xc0\x0c" + struct.pack(">HHIH", kind, 1, 60, len(value)) + value

            here = bytes([10, 0, 2, 2])
            if name.lower() == "xila.test":
                listening.sendto(answer(0, [line(1, here)]), who)
            elif name.lower() == "alias.test":
                # another name for a machine (5), then the address of that machine
                listening.sendto(answer(0, [line(5, b"\x04xila\x04test\x00"), line(1, here)]), who)
            elif name.lower() == "six.test":
                # only an address of sixteen bytes (28)
                listening.sendto(answer(0, [line(28, bytes(16))]), who)
            elif name.lower() != "mute.test":
                listening.sendto(answer(3, []), who)

    def net_says():
        return {row["item"]: row["value"] for row in x.table("net")}

    listening = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    listening.bind(("127.0.0.1", 0))
    threading.Thread(target=name_server, args=(listening,), daemon=True).start()
    ours = "10.0.2.2:%d" % listening.getsockname()[1]
    check("names: the network named a name server along with the address", net_says().get("name server") == "10.0.2.3", net_says())
    ok, lines = x.cmd("net names " + ours)
    check("net names: who is asked for names is set by hand", ok and lines == ["Names are now asked of %s." % ours], lines)
    check("net names: and net says so", net_says().get("name server") == ours + ", set by hand", net_says())
    ok, lines = x.cmd("net name xila.test")
    check("net name: the address of a machine known by its name", ok and lines == ["xila.test is at 10.0.2.2."], lines)
    ok, lines = x.cmd("net name XILA.Test")
    check("net name: known the second time, whatever the capitals, without asking again", ok and lines == ["XILA.Test is at 10.0.2.2."] and names_asked == ["xila.test"], (lines, names_asked))
    ok, lines = x.cmd("net name alias.test")
    check("net name: a name that is another name for a machine", ok and lines == ["alias.test is at 10.0.2.2."], lines)
    ok, lines = x.cmd("net name none.test")
    check("net name: a name nobody has is a failure", not ok and lines == ["The name server knows no machine of that name."], lines)
    ok, lines = x.cmd("net name six.test")
    check("net name: a name with no address of four bytes is a failure", not ok and lines == ["The name server gave no address for that name."], lines)
    del names_asked[:]
    start = time.time()
    ok, lines = x.cmd("net name mute.test", timeout=30)
    took = time.time() - start
    check("net name: a server that says nothing is asked three times, then given up", not ok and lines == ["The name server did not answer."] and names_asked == ["mute.test"] * 3 and 2 < took < 10, (lines, names_asked, took))
    ok, lines = x.cmd("net name 10.0.2.2")
    check("net name: wants a name", not ok and lines == ["Type net name and the name of a machine: net name example.org"], lines)
    ok, lines = x.cmd("net names there")
    check("net names: wants an address", not ok and lines == ["Type net names and the address of a name server: net names 192.168.1.1"], lines)
    del asked_of_us[:]
    named = "xila.test:%d" % serving.server_address[1]
    ok, lines = x.cmd("net get %s/some/page" % named)
    check("net get: a machine asked by its name", ok and lines == [named + " answered 200.", "hello from the host", "you asked for /some/page"], lines)
    check("net get: the machine is told the name it was asked by", asked_of_us == [("/some/page", named)], asked_of_us)
    ok, lines = x.cmd("net get none.test/page")
    check("net get: a name nobody has: nothing is asked, and it is a failure", not ok and lines == ["The name server knows no machine of that name."] and len(asked_of_us) == 1, (lines, asked_of_us))
    # the card set up again: the names are those of the network again
    x.cmd("net restart")
    for _ in range(80):
        if net_says().get("address") == "10.0.2.15":
            break
        time.sleep(0.25)
    check("net restart: the name server is again the one the network named", net_says().get("name server") == "10.0.2.3", net_says())
    ok, lines = x.cmd("net name xila.test", timeout=30)
    check("net restart: and what the other one said is forgotten", not ok, lines)
    # A name of the Internet, asked of a server of the Internet, if this
    # machine can reach one. Not of the server the network named: QEMU
    # passes those questions on to this machine's own, and with some (a
    # VPN's) no answer comes back.
    public = "1.1.1.1"
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    probe.settimeout(2)
    try:
        probe.sendto(b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x03com\x00\x00\x01\x00\x01", (public, 53))
        online = probe.recvfrom(512)[0][:2] == b"\x12\x34"
    except OSError:
        online = False
    probe.close()
    if online:
        x.cmd("net names " + public)
        ok, lines = x.cmd("net name example.com", timeout=30)
        check("net name: a name of the Internet, asked of a server of the Internet", ok and re.fullmatch(r"example\.com is at \d+\.\d+\.\d+\.\d+\.", lines[0]), lines)
        ok, lines = x.cmd("net name www.github.com", timeout=30)
        check("net name: one that is another name for a machine, as real servers say it", ok and re.fullmatch(r"www\.github\.com is at \d+\.\d+\.\d+\.\d+\.", lines[0]), lines)
    else:
        print("skip  net name: a name of the Internet (no name server of the Internet answers this machine)")
    check("names: the rest of the network is as it was", x.cmd("net ping 10.0.2.2")[1][0].startswith("10.0.2.2 answered in "))
    # The processor QEMU imitates unless told otherwise has no source of
    # chance of its own: nothing is asked through TLS, and it is said why.
    check("the boot log says there is no chance good enough for keys", "Chance .............. none good enough for keys: no https" in x.boot_log, x.boot_log)
    ok, lines = x.cmd("net get https://%s/some/page" % there)
    check("net get https: without a source of chance nothing is asked", not ok and lines == ["This processor has no source of chance: nothing here can be kept secret."], lines)
    listening.close()

# TLS. First the crate by itself, on this machine, against OpenSSL as the
# server: other code than ours on the other side, with certificates of
# made-up authorities (tls/fixtures) that the example is told to trust.
import shutil
FIXTURES = os.path.join(ROOT, "tls", "fixtures")
MEET = os.path.join(ROOT, "target", "release", "examples", "meet")
openssl = shutil.which("openssl")
if openssl:
    built = subprocess.run(["cargo", "build", "--release", "-p", "xila-tls", "--example", "meet"], cwd=ROOT, capture_output=True, text=True)
    if built.returncode != 0:
        check("tls: the example that tries it on this machine is built", False, built.stderr[-400:])
        openssl = None
else:
    print("skip  tls against OpenSSL (needs the openssl command)")


def free_port():
    probe = socket.socket()
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    return port


def openssl_server(certificate, *more):
    """Starts OpenSSL as a TLS server that answers any page with one of its
    own, showing that certificate of tls/fixtures. Returns it and its port."""
    port = free_port()
    chain = ["-cert_chain", "between.pem"] if certificate == "leaf" else []
    server = subprocess.Popen([openssl, "s_server", "-accept", str(port), "-cert", certificate + ".pem", "-key", certificate + "-key.pem"] + chain + ["-www", "-quiet"] + list(more),
                              cwd=FIXTURES, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(100):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=1).close()
            break
        except OSError:
            time.sleep(0.05)
    return server, port


def meet(port, *words):
    return subprocess.run([MEET, "--at", "127.0.0.1:%d" % port] + list(words), capture_output=True, text=True, timeout=60).stdout.split("\n")


if openssl:
    root, rsa_root = os.path.join(FIXTURES, "root.der"), os.path.join(FIXTURES, "rsa-root.der")
    curve, curve_port = openssl_server("leaf", "-tls1_3")
    rsa, rsa_port = openssl_server("rsa-leaf", "-tls1_3")
    said = meet(curve_port, "xila.test", "--root", root)
    check("tls: a conversation with OpenSSL, certificates on curves", said[0] == "met: xila.test, and it said when it had finished" and "  HTTP/1.0 200 ok" in said, said[:4])
    said = meet(rsa_port, "xila.test", "--root", rsa_root)
    check("tls: a conversation with OpenSSL, RSA certificate and signature", said[0] == "met: xila.test, and it said when it had finished" and "  HTTP/1.0 200 ok" in said, said[:4])
    check("tls: an authority that is not trusted is refused", meet(curve_port, "xila.test")[0] == "refused: Certificate(Unknown)" and meet(curve_port, "xila.test", "--root", rsa_root)[0] == "refused: Certificate(Unknown)")
    check("tls: the certificate of another name is refused", meet(curve_port, "other.test", "--root", root)[0] == "refused: Certificate(WrongName)")
    check("tls: a certificate out of its time is refused, after and before", meet(curve_port, "xila.test", "--root", root, "--days", "40000")[0] == "refused: Certificate(Expired)" and meet(curve_port, "xila.test", "--root", root, "--days", "-400")[0] == "refused: Certificate(NotYet)")
    old, old_port = openssl_server("leaf", "-tls1_2")
    said = meet(old_port, "xila.test", "--root", root)
    check("tls: a server that only speaks TLS 1.2 is not spoken with", said[0] in ("refused: Alert(70)", "refused: Old"), said[:2])
    other, other_port = openssl_server("leaf", "-tls1_3", "-groups", "P-256")
    said = meet(other_port, "xila.test", "--root", root)
    check("tls: nor one that wants another kind of key", said[0] in ("refused: NoCommon", "refused: Alert(40)"), said[:2])
    aes, aes_port = openssl_server("leaf", "-tls1_3", "-ciphersuites", "TLS_AES_256_GCM_SHA384")
    said = meet(aes_port, "xila.test", "--root", root)
    check("tls: nor one that wants another cipher", said[0] in ("refused: NoCommon", "refused: Alert(40)"), said[:2])

    # Then XilaOS itself, on a processor with a source of chance. It only
    # trusts the authorities of the Internet, so what can be checked
    # without the Internet is that the handshake with OpenSSL goes as far
    # as the certificate, and that the certificate is then refused.
    listening = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    listening.bind(("127.0.0.1", 0))
    threading.Thread(target=name_server, args=(listening,), daemon=True).start()
    with Xila(net="e1000e", cpu="max") as x:
        for _ in range(40):
            if net_says().get("address") == "10.0.2.15":
                break
            time.sleep(0.25)
        check("the boot log says the processor has a source of chance", "Chance .............. the processor's own source" in x.boot_log, x.boot_log)
        heap = [row for row in x.table("mem") if row["kind"] == "kernel heap in use"]
        x.cmd("net names 10.0.2.2:%d" % listening.getsockname()[1])
        ok, lines = x.cmd("net get https://xila.test:%d/page" % curve_port, timeout=30)
        check("net get https: a server nobody trusted vouches for is not asked", not ok and lines == ["No authority that XilaOS trusts vouches for that machine's certificate."], lines)
        ok, lines = x.cmd("net get https://xila.test:%d/page" % rsa_port, timeout=30)
        check("net get https: the same with an RSA certificate", not ok and lines == ["No authority that XilaOS trusts vouches for that machine's certificate."], lines)
        ok, lines = x.cmd("net get https://xila.test:%d/page" % old_port, timeout=30)
        check("net get https: a server that only speaks TLS 1.2", not ok and lines == ["That machine only speaks a TLS older than 1.3, which XilaOS does not."], lines)
        ok, lines = x.cmd("net get https://xila.test:%d/page" % other_port, timeout=30)
        check("net get https: a server that wants another kind of key", not ok and lines == ["That machine and XilaOS have no way in common of keeping a secret."], lines)
        ok, lines = x.cmd("net get https://xila.test:%d/page" % aes_port, timeout=30)
        check("net get https: a server that wants another cipher", not ok and lines == ["That machine and XilaOS have no way in common of keeping a secret."], lines)
        del asked_of_us[:]
        ok, lines = x.cmd("net get https://%s/some/page" % there, timeout=30)
        check("net get https: a server that does not speak TLS is told nothing of the page", not ok and lines == ["That machine does not speak TLS there, or not as TLS is spoken."] and asked_of_us == [], (lines, asked_of_us))
        ok, lines = x.cmd("net get http://xila.test:%d/some/page" % serving.server_address[1])
        check("net get: plainly, on the same machine, as before", ok and lines[0].endswith(" answered 200."), lines)
        if online:
            x.cmd("net names " + public)
            ok, lines = x.cmd("net get https://example.com/", timeout=40)
            check("net get https: a page of the Internet, asked through TLS", ok and lines[0] == "example.com:443 answered 200." and len(lines) > 1, lines)
            ok, lines = x.cmd("net get https://www.debian.org/", timeout=40)
            check("net get https: one whose certificates are signed with RSA", re.fullmatch(r"www\.debian\.org:443 answered \d+\.", lines[0]), lines)
            ok, lines = x.cmd("net get https://openrouter.ai/api/v1/models", timeout=60)
            long = re.fullmatch(r"\(and more: (\d+) bytes in all\)", lines[-1])
            check("net get https: a long answer in chunks from the server the companion will speak through", lines[0] == "openrouter.ai:443 answered 200." and lines[1].startswith('{"data":[') and long and int(long.group(1)) > 100_000, (lines[:2], lines[-1]))
            ok, lines = x.cmd("net post https://openrouter.ai/api/v1/chat/completions '{}'", timeout=40)
            check("net post https: a question to the model without a key is refused by the model's server, in its own words", not ok and lines[0] == "openrouter.ai:443 answered 401." and len(lines) > 1 and "error" in lines[1], lines)
        else:
            print("skip  net get https: a page of the Internet (no name server of the Internet answers this machine)")
        check("net get https: the memory each conversation took is given back", [row for row in x.table("mem") if row["kind"] == "kernel heap in use"] == heap, heap)
        check("net get https: the rest of the network is as it was", x.cmd("net ping 10.0.2.2")[1][0].startswith("10.0.2.2 answered in "))
    if online:
        # what went over the cable to and from port 443: nothing of the question or of the answer can be read
        sealed = b"".join(frame[54:] for frame in frames_of(os.path.join(ROOT, "build", "net.pcap")) if frame[12:14] == b"\x08\x00" and frame[23] == 6 and 443 in struct.unpack(">HH", frame[34:38]))
        check("net get https: nothing of what was asked or answered can be read on the cable", len(sealed) > 2000 and b"HTTP/1." not in sealed and b"GET /" not in sealed and b"/api/v1/models" not in sealed and b"<html" not in sealed, len(sealed))
    listening.close()
    for server in (curve, rsa, old, other, aes):
        server.kill()
# The companion. Its model is a made-up one on this machine, which says
# what the checks tell it to: the way to it is plain HTTP, so no key goes.
with Xila(net="e1000e") as x:
    for _ in range(40):
        if net_says().get("address") == "10.0.2.15":
            break
        time.sleep(0.25)
    x.cmd("create table notes (label text, stars int)")
    x.cmd("insert into notes values ('rent', 3), ('holidays', 5)")
    told = {row["item"]: row["value"] for row in x.table("companion")}
    check("companion: who it is before anything is asked", (told.get("name"), told.get("at"), told.get("key"), told.get("now")) == ("Xila", "https://openrouter.ai/api/v1/chat/completions", "openrouter: not set, see key", "nothing asked"), told)
    check("ask: without a key the model of the Internet is not asked", x.cmd("ask hello?") == (False, ["No key opens the model. Type key set openrouter and the key."]), x.cmd("ask hello?"))
    # who the companion is is a row of a table: here, one whose model is ours
    x.cmd("create table companions (name text, place text, model text, key text, character text)")
    x.cmd("insert into companions values ('Xila', 'http://%s/v1/chat/completions', 'made-up/model', 'openrouter', 'You are brief.')" % there)
    x.cmd("key set openrouter sk-test-never-sent-in-clear")
    model_says[:] = [{"said": "Hello. I am a made-up model \u2014 \u00e0 votre service, d\u00e9j\u00e0. \u00c8ve, c\u0153ur."}]
    ok, lines = x.cmd("ask are you there?", timeout=30)
    check("ask: the companion says what the model says, in the characters XilaOS has", ok and lines == ["Hello. I am a made-up model - ? votre service, d?j?. Eve, coeur."], lines)
    headers, asked = model_asked[0] if model_asked else ({}, None)
    told = asked["messages"] if asked else []
    check("ask: the model was sent who it is, the question and its tool", asked and asked["model"] == "made-up/model" and [m["role"] for m in told] == ["system", "user"] and told[0]["content"].startswith("Your name is Xila. You live inside XilaOS") and told[0]["content"].endswith(" Your character: You are brief.") and told[1]["content"] == "are you there?" and asked["tools"][0]["function"]["name"] == "select", asked)
    check("ask: the key does not go where the way is not sealed", "Authorization" not in headers and headers.get("Content-Type") == "application/json" and "sk-test" not in json.dumps(asked), headers)
    model_says[:] = [{"said": "Let me look.", "queries": ["select count(*) from notes", "select label from notes where stars > 4"]}, {"said": "You have 2 notes.\nThe best one is holidays."}]
    ok, lines = x.cmd("ask how many notes do I have, and which is best?", timeout=30)
    check("ask: the model looks at the tables, then answers", ok and lines == ["Let me look.", "You have 2 notes.", "The best one is holidays."], lines)
    again = model_asked[-1][1]["messages"] if model_asked[-1][1] else []
    check("ask: it was sent back what its queries gave, each under the number of its query",
          [m["role"] for m in again[-4:]] == ["user", "assistant", "tool", "tool"] and [m["content"] for m in again[-2:]] == ["count\n2", "label\nholidays"]
          and [c["id"] for c in again[-3]["tool_calls"]] == [m["tool_call_id"] for m in again[-2:]] and json.loads(again[-3]["tool_calls"][0]["function"]["arguments"]) == {"query": "select count(*) from notes"}, again[-4:])
    check("ask: and what was said before", [m["content"] for m in again[1:3]] == ["are you there?", "Hello. I am a made-up model - \u00e0 votre service, d\u00e9j\u00e0. Eve, coeur."], again[1:3])
    talk = [(row["turn"], row["who"]) for row in x.table("select turn, who, text from talk order by id")]
    check("talk: everything that is said is kept, turn by turn", talk == [("1", "you"), ("1", "xila"), ("2", "you"), ("2", "xila"), ("2", "select"), ("2", "select"), ("2", "result"), ("2", "result"), ("2", "xila")], talk)
    model_says[:] = [{"queries": ["delete from notes", "select * from kept_keys", "drop table notes"]}, {"said": "I cannot."}]
    x.cmd("ask empty my notes", timeout=30)
    sent = model_asked[-1][1]["messages"]
    check("ask: the model can only read, and not where keys are kept", [m["content"][:6] for m in sent[-3:]] == ["error:"] * 3 and x.table("select count(*) from notes")[0]["count"] == "2" and "sk-test" not in json.dumps([asked for _, asked in model_asked]), sent[-3:])
    model_says[:] = [{"status": 401, "raw": b'{"error":{"message":"No auth credentials found","code":401}}'}]
    check("ask: a server that refuses is quoted, and the question fails", x.cmd("ask hello?", timeout=30) == (False, ["The model's server refused (401): No auth credentials found."]))
    model_says[:] = [{"raw": b"<html>not a model</html>"}]
    check("ask: what is not a model's answer is said not to be", x.cmd("ask hello?", timeout=30) == (False, ["What the model's server said is not an answer."]))
    x.cmd("update companions set place = 'http://10.0.2.2:1/v1'")
    check("ask: a model that cannot be reached", x.cmd("ask hello?", timeout=30) == (False, ["The model's server does not answer."]))
    x.cmd("update companions set place = 'http://%s/v1/chat/completions'" % there)
    # The machine lives while the model thinks: asked at the keyboard, the
    # answer comes to the screen, and the serial prompt answers meanwhile.
    model_says[:] = [{"said": "Slow but sure.", "wait": 3}]
    x.type("ask take your time")
    x.key("ret", 0.3)
    start = time.time()
    told = {row["item"]: row["value"] for row in x.table("companion")}
    alive = x.cmd("tasks")[0] and x.table("select count(*) from notes")[0]["count"] == "2" and time.time() - start < 2
    check("ask: while the model thinks the machine goes on, and says the companion is answering", alive and told.get("now", "").startswith("answering, for "), (told, time.time() - start))
    check("ask: a second question waits for the first", x.cmd("ask and me?") == (False, ["The companion is still answering the last question."]))
    for _ in range(60):
        if any("Slow but sure." in row for row in x.peek()):
            break
        time.sleep(0.2)
    took = time.time() - start
    screen = [row for row in x.peek() if row.strip()]
    check("ask: the answer comes to the screen that asked, when the model gives it", any("Slow but sure." in row for row in screen) and 2.5 < took < 12, (screen[-4:], took))
    # Its own screen: what is typed there is for it, and what is said is
    # written as a conversation is.
    model_says[:] = [{"said": "Here I am.\nOn my own screen.", "wait": 1}]
    x.key("alt-2", 0.4)
    empty = [row.rstrip() for row in x.peek() if row.strip()]
    check("companion screen: [Alt]+[2] shows it, with a line to type on", "COMPANION" in empty[4] and empty[-1] == "<you>" and empty[-2].startswith("The companion is here."), empty[4:])
    x.type("are you here?")
    x.key("ret", 0.3)
    thinking = [row.rstrip() for row in x.peek() if row.strip()]
    check("companion screen: what is typed is asked, and it says the companion thinks", re.fullmatch(r"\d\d:\d\d <you> are you here\?", thinking[-2]) and thinking[-1] == "(thinking)", thinking[-2:])
    for _ in range(60):
        if any("On my own screen." in row for row in x.peek()):
            break
        time.sleep(0.2)
    said = [row.rstrip() for row in x.peek() if row.strip()]
    check("companion screen: the answer is written under who says it", re.fullmatch(r"\d\d:\d\d <xila> Here I am\.", said[-3]) and said[-2] == " " * 13 + "On my own screen." and said[-1] == "<you>", said[-4:])
    check("companion screen: the model was asked what was typed there", model_asked[-1][1]["messages"][-1] == {"role": "user", "content": "are you here?"}, model_asked[-1][1]["messages"][-1])
    # letters beyond ASCII: typed on the French keyboard they reach the model as the rest of the world writes them
    model_says[:] = [{"said": "D\u00e9j\u00e0 l\u00e0, gar\u00e7on."}]
    asked_before = len(model_asked)
    x.cmd("keymap fr")
    for name in ("2", "t", "2", "spc", "0", "spc", "9", "q"):
        x.key(name)
    x.key("ret", 0.3)
    x.cmd("keymap us")
    for _ in range(60):
        if len(model_asked) > asked_before and any("gar?on." in row for row in x.peek()):
            break
        time.sleep(0.2)
    typed = model_asked[-1][1]["messages"][-1] if len(model_asked) > asked_before else None
    check("companion screen: accented letters typed there reach the model as it reads them", typed == {"role": "user", "content": "\u00e9t\u00e9 \u00e0 \u00e7a"}, typed)
    said = [row.rstrip() for row in x.peek() if row.strip()]
    check("companion screen: and its own come back as letters, one cell each", re.fullmatch(r"\d\d:\d\d <xila> D\?j\? l\?, gar\?on\.", said[-2]) is not None, said[-3:])
    x.key("esc", 0.4)
    back = x.peek()
    check("companion screen: [Esc] goes back, and the prompt's screen did not get the answer", "OVERVIEW" in back[4] and not any("On my own screen." in row for row in back), [row for row in back if row.strip()][-3:])
    check("companion screen: it is one of the screens", [row["name"] for row in x.table("select name from screens")] == ["overview", "boot log", "companion"])
    # A program that runs by itself asks the companion: nobody waits, and
    # what is said is on the companion's screen, under the program's name.
    for line in ("on notify alerts", "ask the alert says: {text}. Anything to do?", "end"):
        x.cmd(line)
    model_says[:] = [{"said": "Nothing to do."}]
    asked_before = len(model_asked)
    x.cmd("notify alerts 'the disk is full'")
    for _ in range(60):
        if len(model_asked) > asked_before and any("Nothing to do." in row for row in x.peek()):
            break
        time.sleep(0.2)
    check("ask in a program: what happened is asked about, with its values in the question", len(model_asked) > asked_before and model_asked[-1][1]["messages"][-1]["content"] == "the alert says: the disk is full. Anything to do?", model_asked[-1][1]["messages"][-1])
    x.key("alt-2", 0.4)
    said = [row.rstrip() for row in x.peek() if row.strip()]
    check("ask in a program: question and answer are on the companion's screen, under the program's name", re.fullmatch(r"\d\d:\d\d <on_notify_alerts> the alert says: the disk is full\. Anything to do\?", said[-3]) and re.fullmatch(r"\d\d:\d\d <xila> Nothing to do\.", said[-2]), said[-3:])
    x.key("esc", 0.4)
    # What happens to the machine is said on channels: the network here.
    x.cmd("listen network")
    x.cmd("listen tasks")
    x.events.clear()
    x.monitor("set_link net0 off")
    time.sleep(2.5)
    x.cmd("")
    check("events: a cable that is pulled out is said", ("network", "cable out") in x.events, x.events)
    x.monitor("set_link net0 on")
    time.sleep(2.5)
    x.cmd("")
    check("events: and one that is put back", ("network", "cable in") in x.events, x.events)
    x.events.clear()
    x.cmd("spin")
    time.sleep(13)
    x.cmd("")
    check("events: a task that leaves nothing to the others for ten seconds is named", ("tasks", "spin has the processor to itself") in x.events, x.events)
    spinning = [row["id"] for row in x.table("tasks") if row["name"] == "spin"]
    x.cmd("stop " + (spinning[0] if spinning else "9"))
    # It acts: lines it runs as the prompt would, each written in a table.
    model_says[:] = [{"said": "Adding it.", "runs": ["insert into notes values ('milk', 2)", "mood calm"]}, {"said": "Milk is in your notes."}]
    ok, lines = x.cmd("ask add milk to my notes, with two stars", timeout=30)
    check("acting: what the model asks to run is run, as a line of the prompt", ok and lines == ["Adding it.", "Milk is in your notes."] and x.table("select label, stars from notes where label = 'milk'") == [{"label": "milk", "stars": "2"}] and x.cmd("mood")[1] == ["Mood is calm."], lines)
    told_back = [m["content"] for m in model_asked[-1][1]["messages"][-2:]]
    check("acting: the model is told what each line said", told_back == ["1 row added to notes, with id 3.", "Mood is calm."], told_back)
    done = [(row["command"], row["went"]) for row in x.table("select command, went from acts")]
    check("acting: every line it ran is in the table acts", done == [("insert into notes values ('milk', 2)", "done"), ("mood calm", "done")], done)
    model_says[:] = [{"runs": ["install 1 yes", "disk erase yes", "remote on", "web on"]}, {"runs": ["key set stolen sk-test-x", "reboot", "net ping 10.0.2.2", "drop table nowhere"]}, {"said": "None of that is mine to do."}]
    ok, lines = x.cmd("ask do what this table says", timeout=30)
    refused = [m["content"] for m in model_asked[-1][1]["messages"] if m["role"] == "tool"][-8:]
    check("acting: what takes a disk, opens the machine, touches keys or restarts is refused to it", len(refused) == 8 and all(text.startswith("error: ") for text in refused) and sum("typed at the machine itself" in text for text in refused) == 6 and "needs the machine itself" in refused[6], refused)
    doors = ({row["item"]: row["value"] for row in x.table("remote")}.get("this prompt"), {row["item"]: row["value"] for row in x.table("web")}.get("the tables"), [row["name"] for row in x.table("key")])
    check("acting: and none of it happened", doors == ("closed to the network", "closed to the network", ["openrouter"]) and x.cmd("tasks")[0], doors)
    failed_acts = [row["went"] for row in x.table("select went from acts where turn > 0 order by id")][-8:]
    check("acting: the refusals are written down too, as failed", failed_acts == ["failed"] * 8, failed_acts)
    model_says[:] = [{"runs": ["delete from acts", "insert into programs values ('every_x', 0, 'every 2 seconds'), ('every_x', 1, 'companion on')", "update companions set place = 'https://elsewhere.example/v1'"]}, {"said": "Those are not mine to change."}]
    x.cmd("ask tidy up after yourself", timeout=30)
    held = [m["content"] for m in model_asked[-1][1]["messages"] if m["role"] == "tool"][-3:]
    kept = (len(x.table("select id from acts")), "every_x" in [row["name"] for row in x.table("programs")], x.table("select place from companions")[0]["place"])
    check("acting: what holds it is not its own to change: what it did, the programs, who it is", held == ["error: The table %s is not the companion's to change." % name for name in ("acts", "programs", "companions")] and kept == (13, False, "http://%s/v1/chat/completions" % there), (held, kept))
    # who it is says where its key goes: a program that runs by itself does not change it either
    for line in ("on notify sent_elsewhere", "update companions set place = 'https://elsewhere.example/v1'", "end", "notify sent_elsewhere 'now'"):
        x.cmd(line)
    time.sleep(0.5)
    x.cmd("screen 1")
    log_rows = [row.strip() for row in x.peek() if "sent_elsewhere" in row]
    x.cmd("screen 0")
    place = x.table("select place from companions")[0]["place"]
    check("who the companion is: a program that runs by itself does not change it", place == "http://%s/v1/chat/completions" % there and log_rows[:1] and log_rows[0].startswith("on_notify_sent_elsewhere: The table companions is changed at the machine: it"), (place, log_rows))
    x.cmd("drop program on_notify_sent_elsewhere")
    # Notes: kept in a table, and told to it again in the next conversations.
    model_says[:] = [{"facts": ["The user counts stars from 1 to 5."]}, {"said": "Noted."}]
    x.cmd("ask remember that I count stars from one to five", timeout=30)
    check("remembering: what it keeps is a row of the table remembered", [row["text"] for row in x.table("select text from remembered")] == ["The user counts stars from 1 to 5."], x.cmd("select * from remembered"))
    x.cmd("delete from talk")
    model_says[:] = [{"said": "No: five at most."}]
    x.cmd("ask can a note have seven stars?", timeout=30)
    told = model_asked[-1][1]["messages"]
    check("remembering: it is told again, even when what was said is gone", len(told) == 2 and told[0]["content"].endswith("\nWhat you remembered:\n- The user counts stars from 1 to 5."), told[0]["content"][-80:])
    # Off: it is asked nothing and does nothing, until it is turned on.
    ok, lines = x.cmd("companion off")
    check("companion off: it is turned off", ok and lines == ["The companion is off: it is asked nothing and does nothing until companion on."] and {row["item"]: row["value"] for row in x.table("companion")}.get("now") == "off", lines)
    asked_before = len(model_asked)
    check("companion off: nothing is asked of it", x.cmd("ask are you there?") == (False, ["The companion is off. Type companion on to have it back."]))
    x.cmd("notify alerts 'again'")
    time.sleep(1.5)
    check("companion off: and a program that asks is told so, and the model hears nothing", len(model_asked) == asked_before, len(model_asked) - asked_before)
    x.cmd("companion on")
    model_says[:] = [{"said": "Back."}]
    check("companion on: it answers again", x.cmd("ask are you there?", timeout=30) == (True, ["Back."]))
    told = {row["item"]: row["value"] for row in x.table("companion")}
    check("companion: says who it is and what it has done", (told.get("model"), told.get("key"), told.get("character"), told.get("now"), told.get("queries run for it"), told.get("lines it ran")) == ("made-up/model", "openrouter: kept", "You are brief.", "nothing asked", "5", "13"), told)

# And the real one, when there is a key for it and a way to the Internet.
model_key = os.environ.get("OPENROUTER_API_KEY", "")
if not model_key and os.path.exists(os.path.join(ROOT, ".env.local")):
    for line in open(os.path.join(ROOT, ".env.local")):
        if line.startswith("OPENROUTER_API_KEY="):
            model_key = line.split("=", 1)[1].strip().strip('"')
if online and model_key:
    with Xila(net="e1000e", cpu="max") as x:
        for _ in range(40):
            if net_says().get("address") == "10.0.2.15":
                break
            time.sleep(0.25)
        x.cmd("net names " + public)
        x.cmd("key set openrouter " + model_key)
        x.cmd("create table notes (label text, stars int)")
        x.cmd("insert into notes values ('rent', 3), ('holidays', 5), ('taxes', 1)")
        ok, lines = x.cmd("ask how many rows has the table notes? Answer with the number only.", timeout=180)
        check("ask: a real model, through TLS, reads the tables and answers", ok and "3" in " ".join(lines), lines)
        ran = [row["text"] for row in x.table("select text from talk where who = 'select'")]
        check("ask: and it looked before answering", any("notes" in query for query in ran), ran)
else:
    print("skip  ask: a real model (needs the Internet and a key, in OPENROUTER_API_KEY or .env.local)")
serving.shutdown()
mute.close()

# The time and the date, by the clock of the machine: here, one that says
# it is a quarter of a minute before midnight.
with Xila(more=["-rtc", "base=2026-03-04T23:59:45"]) as x:
    log = "\n".join(x.boot_log)
    check("the boot log tells the time by the clock of the machine", re.search(r"Clock \.+ 2026-03-04 23:59:[45]\d", log), log)
    ok, lines = x.cmd("time")
    check("time: the day and the time", ok and re.fullmatch(r"Wednesday 4 March 2026, 23:59:[45]\d\.", lines[0]), lines)
    check("today is a name in any calculation", x.cmd("say today") == (True, ["2026-03-04"]), x.cmd("say today"))
    rows = x.table("select today as day, length(now) as size")
    check("select: now and today", rows == [{"day": "2026-03-04", "size": "19"}], rows)
    clock = x.table("select value from machine where item = 'clock'")
    check("select: the machine view has the clock", clock and clock[0]["value"].startswith("2026-03-04 23:59:"), clock)
    x.cmd("create table mornings (day text)")
    for line in ("every day at 0:00", "insert into mornings values (today)"):
        x.cmd(line)
    ok, lines = x.cmd("end")
    check("every day at: the program is kept", ok and lines == ["Program every_day_at_0h00 kept: 1 line.", "It runs every day at 0:00. What it says goes to the log."], lines)
    check("every day at: it waits for its time", x.table("select day from mornings") == [], None)
    # midnight comes
    for _ in range(40):
        if x.cmd("say today") == (True, ["2026-03-05"]):
            break
        time.sleep(0.5)
    time.sleep(0.5)
    check("every day at: it runs when the clock shows that time", x.table("select day from mornings") == [{"day": "2026-03-05"}], x.cmd("time"))
    ok, lines = x.cmd("time")
    check("time: the next day has come", ok and lines[0].startswith("Thursday 5 March 2026, 00:00:"), lines)
    # like any block, it is read when its end comes
    x.cmd("every day at noon")
    ok, lines = x.cmd("end")
    check("every day at: wants a time", not ok and lines == ["Write: every 5 minutes (or seconds, hours, days), or every day at 8:00"], lines)

# The start of the system is an event, and so is the address the network
# gives: programs kept on the disk hear both at the next start.
start_disk = blank_disk(os.path.join(ROOT, "build", "start.img"))
with Xila(disk=start_disk, net="e1000e") as x:
    for line in ("on notify start", "say 'started: ' + text", "end", "on notify network", "say 'the network says: ' + text", "end"):
        x.cmd(line)
    time.sleep(0.3)
with Xila(disk=start_disk, net="e1000e") as x:
    for _ in range(40):
        if {row["item"]: row["value"] for row in x.table("net")}.get("address") == "10.0.2.15":
            break
        time.sleep(0.25)
    time.sleep(1.5)
    x.key("alt-1", 0.5)
    log = [row.rstrip() for row in x.peek() if row.strip()]
    check("events: the start of the system is said, and a program kept on the disk hears it", "on_notify_start: started: XilaOS 0.1" in log, log[-4:])
    check("events: so is the address the network gave", "on_notify_network: the network says: address 10.0.2.15" in log, log[-4:])

# Keys to services: kept on the disk, and shown by nothing.
keys_disk = blank_disk(os.path.join(ROOT, "build", "keys.img"))
with Xila(disk=keys_disk) as x:
    check("key: none is kept at first", x.cmd("key") == (True, ["No key is kept. To keep one: key set openrouter and the key."]), x.cmd("key"))
    ok, lines = x.cmd("key set openrouter sk-test-0123456789")
    check("key set: a key is kept", ok and lines == ["The key openrouter is kept. It is never shown."], lines)
    x.cmd("create table mine (n int)")
    check("key: lists the names, never the keys", x.table("key") == [{"name": "openrouter", "key": "kept, never shown"}], x.cmd("key"))
    shown = []
    for command in ("tables", "select * from tables", "select * from columns", "city", "disk", "disk check", "mem", "select * from kept_keys", "select * from mine join kept_keys on 1 = 1"):
        shown += x.cmd(command, timeout=60)[1]
    check("key: nothing lists where keys are kept, and nothing shows one", not any("sk-test" in line or "kept_keys" in line.replace("Unknown table: kept_keys", "") for line in shown), [line for line in shown if "kept" in line or "sk-" in line])
    refused = [x.cmd(statement)[0] for statement in ("select * from kept_keys", "drop table kept_keys", "delete from kept_keys", "update kept_keys set value = 'x'", "insert into kept_keys values ('a', 'b')", "create table kept_keys (n int)")]
    check("key: SQL can neither read, change, drop nor make that table", refused == [False] * 6, refused)
    # typed at the keyboard: the screen keeps stars, and the arrow does not bring the line back
    x.type("key set typed abc123")
    x.key("ret", 0.4)
    screen = x.peek()
    check("key set: typed at the keyboard, the screen shows stars in its place", any("key set typed ********" in row for row in screen) and not any("abc123" in row for row in screen), [row for row in screen if "key" in row])
    x.key("up", 0.3)
    check("key set: and the arrow that brings lines back does not bring that one", not any("abc123" in row or "key set" in row.split(">")[-1] for row in x.peek()[-3:]), x.peek()[-3:])
    time.sleep(0.3)
with Xila(disk=keys_disk) as x:
    check("key: kept from one start to the next", sorted(row["name"] for row in x.table("key")) == ["openrouter", "typed"], x.cmd("key"))
    ok, lines = x.cmd("key forget typed")
    check("key forget: a key is forgotten", ok and lines == ["The key typed is forgotten."] and x.table("key") == [{"name": "openrouter", "key": "kept, never shown"}], lines)
    check("key forget: wants a key that is kept", x.cmd("key forget typed") == (False, ["No key is kept under that name. Type key to list them."]))

# What a table asks of its columns is kept with it, and asked of every row.
rules_disk = blank_disk(os.path.join(ROOT, "build", "rules.img"))
with Xila(disk=rules_disk) as x:
    made = x.cmd("create table parts (label text not null unique, code int unique, count int default 1 check (count >= 0), check (code is null or code > 99))")
    check("constraints: a table is made with what its columns ask", made == (True, ["Table parts created."]), made)
    x.cmd("insert into parts (label, code) values ('bolt', 100), ('nut', null)")
    rows = x.table("select * from parts")
    check("constraints: a row takes the value by default where it is given none", rows == [{"id": "1", "label": "bolt", "code": "100", "count": "1"}, {"id": "2", "label": "nut", "code": "", "count": "1"}], rows)
    refused = [x.cmd(statement) for statement in (
        "insert into parts (code) values (300)",
        "insert into parts (label) values ('nut')",
        "insert into parts (label, code) values ('pin', 100)",
        "insert into parts (label, count) values ('pin', -1)",
        "update parts set code = 5 where id = 1",
        "update parts set label = 'bolt' where id = 2",
        "insert into parts (label) values ('a'), ('b'), ('a')")]
    wanted = ["Column label must have a value.", "Column label takes no value twice: row 2 has that one.", "Column code takes no value twice: row 1 has that one.",
              "Refused by a check: count >= 0", "Refused by a check: code is null or code > 99", "Column label takes no value twice: row 1 has that one.",
              "Column label takes no value twice: row 3 has that one."]
    check("constraints: a row that breaks one is refused, and the answer says which", refused == [(False, [line]) for line in wanted], refused)
    check("constraints: and nothing of those statements was written", x.table("select * from parts") == rows, x.table("select * from parts"))
    check("constraints: values may pass through one another, as long as none is twice in the end", x.cmd("update parts set code = code + 100") == (True, ["2 rows changed in parts."]) and x.cmd("update parts set code = code - 100")[0])
    asked = [(row["name"], row["rules"]) for row in x.table("select name, rules from columns where table_name = 'parts'")]
    check("constraints: the view of the columns says what each asks", asked == [("id", ""), ("label", "not null unique"), ("code", "unique"), ("count", "default 1")], asked)
    shown = x.cmd("show table parts")
    check("show table: writes the statement that would make the table", shown == (True, ["create table parts (", "    label text not null unique,", "    code int unique,", "    count int default 1,", "    check (count >= 0),", "    check (code is null or code > 99)", ")"]), shown)
    check("constraints: what is wrong in them is said when the table is made", x.cmd("create table t (a int default 'one')") == (False, ["Column a takes a whole number."]) and x.cmd("create table t (a int check (b > 0))") == (False, ["Unknown column: b"]) and x.cmd("create table t (a int primary key)")[0] is False)
    time.sleep(0.2)
    x.cut()
with Xila(disk=rules_disk) as x:
    # ids 3 and 4 went to the rows of the statement that was refused: an id is never given twice
    again = (x.cmd("show table parts"), x.cmd("insert into parts (label) values ('nut')"), x.cmd("insert into parts (label, count) values ('pin', -1)"), x.cmd("insert into parts (label) values ('pin')"))
    check("constraints: after a power cut the table still asks the same", again == (shown, (False, ["Column label takes no value twice: row 2 has that one."]), (False, ["Refused by a check: count >= 0"]), (True, ["1 row added to parts, with id 5."])) and x.cmd("disk check")[0], again)


# A disk written before columns could ask anything says version 1 in its
# headers; the tables on it are, byte for byte, what version 2 writes for
# a table that asks nothing. So one is made here by changing the headers.
def crc32c(data):
    crc = 0xffffffff
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ 0x82f63b78 if crc & 1 else crc >> 1
    return crc ^ 0xffffffff


def header_versions(path, becomes=None):
    """The versions the two headers of a database say, the last commit first. With `becomes`, makes them say that one."""
    found = []
    with open(path, "r+b") as image:
        for slot in range(2):
            image.seek(slot * 4096)
            page = bytearray(image.read(4096))
            if page[4:12] != b"XilaOSdb" or struct.unpack("<I", page[:4])[0] != crc32c(page[4:]):
                continue
            if becomes is not None:
                page[12:16] = struct.pack("<I", becomes)
                page[:4] = struct.pack("<I", crc32c(page[4:]))
                image.seek(slot * 4096)
                image.write(page)
            found.append((struct.unpack("<Q", page[20:28])[0], struct.unpack("<I", page[12:16])[0]))
    return [version for _, version in sorted(found, reverse=True)]


old_disk = blank_disk(os.path.join(ROOT, "build", "version1.img"))
with Xila(disk=old_disk) as x:
    x.cmd("create table plain (n int)")
    x.cmd("insert into plain values (7), (8)")
    time.sleep(0.2)
check("version 1: this XilaOS writes version 3", header_versions(old_disk) == [3, 3], header_versions(old_disk))
header_versions(old_disk, becomes=1)
with Xila(disk=old_disk) as x:
    log = "\n".join(x.boot_log)
    found = (x.table("select n from plain"), x.cmd("disk check")[0])
    check("version 1: a disk of the format before is found and read as it is", "Database ............ on the disk, 1 table" in log and found == ([{"n": "7"}, {"n": "8"}], True), (found, log[-300:]))
    time.sleep(0.2)
check("version 1: reading it writes nothing", header_versions(old_disk) == [1, 1], header_versions(old_disk))
with Xila(disk=old_disk) as x:
    x.cmd("insert into plain values (9)")
    time.sleep(0.2)
    first = header_versions(old_disk)
    made = x.cmd("create table ruled (n int not null)")
    time.sleep(0.2)
check("version 1: the first change makes it version 3, and columns may ask there", first == [3, 1] and made[0] and header_versions(old_disk) == [3, 3], (first, made, header_versions(old_disk)))
with Xila(disk=old_disk) as x:
    found = (x.table("select n from plain"), x.cmd("insert into ruled values (null)"), x.cmd("disk check")[0])
    check("version 1: and everything is there at the next start", found == ([{"n": "7"}, {"n": "8"}, {"n": "9"}], (False, ["Column n must have a value."]), True), found)
header_versions(old_disk, becomes=4)
with Xila(disk=old_disk) as x:
    log = "\n".join(x.boot_log)
    check("version 4: a disk of a format to come is left alone", "Database ............ in memory" in log and x.table("select name from tables where kind = 'stored'") == [], log[-400:])
check("version 4: and nothing is written on it", header_versions(old_disk) == [4, 4], header_versions(old_disk))

# Indexes: from a value of a column to its rows, without reading the table.
index_disk = blank_disk(os.path.join(ROOT, "build", "index.img"))


def pages_for(x, line):
    """What a query gives, and the pages of the disk it read to give it."""
    read = lambda: int({row["item"]: row["value"] for row in x.table("disk")}["pages read"])
    before = read()
    answer = x.table(line)
    return answer, read() - before


with Xila(disk=index_disk) as x:
    # rows made long by what their columns take by default: a serial line holds 240 characters
    long = "words text default '%s', more text default '%s', again text default '%s'" % ("a few words, the same for each visit", "and a few more, to make the row long", "and as many again, for many pages...")
    x.cmd("create table visits (town text, year int, %s)" % long)
    towns = ["paris", "parma", "oslo", "lima"]
    filled = []
    for batch in range(100):
        rows = ", ".join("('%s', %d)" % (towns[n % 4], 1990 + n % 200) for n in range(batch * 8, batch * 8 + 8))
        filled.append(x.cmd("insert into visits (town, year) values " + rows)[0])
    queries = ["select id from visits where year = 2000", "select count(*) from visits where year between 2100 and 2101", "select count(*) from visits where town = 'oslo'", "select count(*) from visits where year = 2000 or town = 'lima'"]
    whole = [pages_for(x, query) for query in queries]
    check("index: (the table is many pages, and a query reads them all)", all(filled) and whole[0][0] == [{"id": "11"}, {"id": "211"}, {"id": "411"}, {"id": "611"}] and whole[0][1] > 20, whole[0])
    made = (x.cmd("create index on visits (year)"), x.cmd("create index on visits (town)"))
    check("create index: makes it from the rows that are there", made == ((True, ["Index made on visits (year), from 800 rows."]), (True, ["Index made on visits (town), from 800 rows."])), made)
    by = [pages_for(x, query) for query in queries]
    check("index: a query gives the same rows as before", [found for found, _ in by] == [found for found, _ in whole], by)
    check("index: and reads a few pages for a value few rows have, or a span of values", by[0][1] * 2 < whole[0][1] and by[1][1] * 2 < whole[1][1], (by, whole))
    check("index: a value a quarter of the rows have reads the table as it lies, and so does or", by[2][1] <= whole[2][1] + 4 and by[3][1] == whole[3][1], (by, whole))
    x.cmd("update visits set year = 2000, town = 'rome' where id = 5")
    x.cmd("delete from visits where id = 211")
    x.cmd("insert into visits (town, year) values ('rome', 2000)")
    found = (x.table("select id from visits where year = 2000"), x.table("select id from visits where town = 'rome'"))
    check("index: it follows the rows that change, go and come", found == ([{"id": "5"}, {"id": "11"}, {"id": "411"}, {"id": "611"}, {"id": "801"}], [{"id": "5"}, {"id": "801"}]), found)
    x.cmd("create table bought (visit int, thing text default 'a thing, with a few words after its name')")
    for batch in range(100):
        x.cmd("insert into bought (visit) values " + ", ".join("(%d)" % (1 + n * 2) for n in range(batch * 8, batch * 8 + 8)))
    join = "select visits.id, bought.id as thing from visits join bought on bought.visit = visits.id where visits.id between 100 and 110"
    without = pages_for(x, join)
    x.cmd("create index on bought (visit)")
    beside = pages_for(x, join)
    check("index: a join finds the rows beside by it", beside[0] == without[0] and len(beside[0]) == 5 and beside[1] * 2 < without[1], (beside, without))
    shown = x.cmd("show table visits")
    check("show table: says the indexes of a table", shown[1][-3:] == [")", "create index on visits (town)", "create index on visits (year)"], shown)
    listed = [(row["name"], row["index"]) for row in x.table("select name, index from columns where table_name = 'visits'")]
    check("index: the view of the columns says which have one", listed == [("id", ""), ("town", "yes"), ("year", "yes"), ("words", ""), ("more", ""), ("again", "")], listed)
    said = [x.cmd(line) for line in ("create index on visits (year)", "create index by_year on visits (words)", "create index on visits (id)", "create index on tasks (name)", "drop index on visits (words)")]
    wanted = ["visits (year) has an index already.", "An index has no name: create index on t (c), drop index on t (c).", "id needs no index: the rows are kept in its order.", "tasks is a view of the system and cannot be changed.", "visits (words) has no index."]
    check("index: what is wrong is said", said == [(False, [line]) for line in wanted], said)
    # a column that takes no value twice has its index with it
    x.cmd("create table towns (name text unique, people int)")
    x.cmd("insert into towns values ('paris', 2100), ('oslo', 700)")
    unique = (x.cmd("insert into towns values ('oslo', 1)"), x.cmd("drop index on towns (name)"), x.table("select index from columns where table_name = 'towns' and name = 'name'"))
    check("index: a unique column has one from the start, and keeps it", unique == ((False, ["Column name takes no value twice: row 2 has that one."]), (False, ["Column name takes no value twice: its index stays with it."]), [{"index": "yes"}]), unique)
    check("disk check: reads every index against its table", x.cmd("disk check")[0], x.cmd("disk check"))
    time.sleep(0.2)
    x.cut()
with Xila(disk=index_disk) as x:
    again = pages_for(x, queries[0].replace("2000", "2001"))
    check("index: after a power cut it is there and leads to the rows", again[0] == [{"id": "12"}, {"id": "212"}, {"id": "412"}, {"id": "612"}] and again[1] * 2 < whole[0][1] and x.cmd("disk check")[0], again)
    used = lambda: int({row["item"]: row["value"] for row in x.table("disk")}["used"].split()[0])
    before = used()
    dropped = x.cmd("drop index on visits (year)")
    time.sleep(0.2)
    back = pages_for(x, queries[0])
    check("drop index: the pages are given back, and the query reads the table again", dropped == (True, ["Index removed from visits (year)."]) and used() < before and back[1] >= whole[0][1] and back[0][0] == {"id": "5"} and x.cmd("disk check")[0], (dropped, before, used(), back))

# A table changed after it was made: columns come and go, take other
# names, ask more or less; all of it on the disk.
alter_disk = blank_disk(os.path.join(ROOT, "build", "alter.img"))
with Xila(disk=alter_disk) as x:
    x.cmd("create table spent (day text, label text, amount decimal, tag text)")
    x.cmd("insert into spent values ('2026-11-18', 'coffee', 3.5, 'food'), ('2026-11-18', 'train', 42, 'travel'), (null, 'refund', -0.05, null)")
    x.cmd("listen spent")
    x.events.clear()
    said = [x.cmd(line) for line in (
        "alter table spent add column paid text default 'no'",
        "alter table spent add rate decimal(6,3) not null default 1.5",
        "alter table spent drop column day",
        "alter table spent rename column tag to kind",
        "alter table spent alter column kind set default 'misc'",
        "alter table spent add check (amount > -1)",
        "alter table spent add unique (label)")]
    wanted = ["Column paid added to spent.", "Column rate added to spent.", "Column day removed from spent.", "Column tag of spent is now kind.", "Column kind of spent takes 'misc' by default.",
              "Check added to spent: every row holds it.", "Column label of spent takes no value twice from now on."]
    check("alter table: columns come and go, take other names, ask more", said == [(True, [line]) for line in wanted], said)
    x.cmd("")
    check("alter table: who listens hears that the table changed, not each of its rows", x.events == [("spent", "alter")] * 7, x.events)
    rows = x.table("select * from spent")
    check("alter table: the rows that were there have the value by default, and lose what goes",
          rows == [{"id": "1", "label": "coffee", "amount": "3.50", "kind": "food", "paid": "no", "rate": "1.500"}, {"id": "2", "label": "train", "amount": "42.00", "kind": "travel", "paid": "no", "rate": "1.500"},
                   {"id": "3", "label": "refund", "amount": "-0.05", "kind": "", "paid": "no", "rate": "1.500"}], rows)
    refused = [x.cmd(line) for line in (
        "alter table spent add must int not null",
        "alter table spent alter kind set not null",
        "alter table spent add check (amount > 0)",
        "alter table spent drop amount",
        "insert into spent (label) values ('train')",
        "alter table spent add ninth int unique default 1",
        "alter table tasks add n int")]
    wanted = ["The table has rows: a column that must have a value needs a default.", "A row has no value in kind: nothing was changed.", "Row 3 breaks that check: nothing was changed.",
              "A check names amount: drop the check first.", "Column label takes no value twice: row 2 has that one.", "The rows would all have the same value: a unique column takes none twice.",
              "tasks is a view of the system and cannot be changed."]
    check("alter table: what the rows do not allow is refused, and says why", refused == [(False, [line]) for line in wanted], refused)
    check("alter table: and nothing of it was done", x.table("select * from spent") == rows and x.cmd("disk check")[0])
    renamed = (x.cmd("alter table spent rename to costs"), x.cmd("select count(*) from spent")[0], x.table("select count(*) from costs"))
    check("alter table: a table takes another name", renamed == ((True, ["Table spent is now costs."]), False, [{"count": "3"}]), renamed)
    shown = x.cmd("show table costs")
    check("show table: says the table as it is now", shown == (True, ["create table costs (", "    label text unique,", "    amount decimal(18,2),", "    kind text default 'misc',", "    paid text default 'no',", "    rate decimal(18,3) not null default 1.500,", "    check (amount > -1)", ")"]), shown)
    time.sleep(0.2)
    x.cut()
with Xila(disk=alter_disk) as x:
    added = x.cmd("insert into costs (label, amount) values ('tea', 2.25)")
    again = (x.cmd("show table costs"), x.table("select * from costs where id < 4"), x.table("select kind, paid, rate from costs where id = 4"))
    check("alter table: after a power cut the table is as it was changed", added[0] and again == (shown, rows, [{"kind": "misc", "paid": "no", "rate": "1.500"}]) and x.cmd("disk check")[0], (added, again))
    undone = [x.cmd(line)[0] for line in ("alter table costs drop check (amount > -1)", "alter table costs drop unique (label)", "drop index on costs (label)", "alter table costs alter rate drop not null", "alter table costs alter kind drop default", "alter table costs drop rate", "alter table costs drop paid")]
    check("alter table: and what was asked can be taken back", all(undone) and x.cmd("show table costs") == (True, ["create table costs (", "    label text,", "    amount decimal(18,2),", "    kind text", ")"]) and x.cmd("disk check")[0], (undone, x.cmd("show table costs")))

# What the user chose is kept from one start to the next, in a table.
kept_disk = blank_disk(os.path.join(ROOT, "build", "settings.img"))
with Xila(disk=kept_disk) as x:
    log = "\n".join(x.boot_log)
    check("settings: a new disk has none kept", "Settings ...." not in log and "Keyboard ............ layout us" in log, log)
    x.cmd("create table mine (n int)")
    time.sleep(0.2)
    stored = "select name from tables where kind = 'stored'"
    check("settings: nothing is kept while nothing was chosen", x.table(stored) == [{"name": "mine"}], x.cmd("tables"))
    for command in ("keymap fr", "theme night", "mood calm", "display flip off"):
        x.cmd(command)
    time.sleep(0.3)
    rows = x.table("select name, value from kept_settings order by name")
    wanted = [{"name": "keymap", "value": "fr"}, {"name": "mood", "value": "calm"}, {"name": "pictures", "value": "direct"}, {"name": "theme", "value": "night"}]
    check("settings: what was chosen is in the table kept_settings", rows == wanted, rows)
    x.cmd("theme day")
    time.sleep(0.3)
with Xila(disk=kept_disk) as x:
    log = "\n".join(x.boot_log)
    check("settings: the next start finds them", "Settings ............ 4 kept from before, in the table kept_settings" in log and "Keyboard ............ layout fr" in log, log)
    now = {row["name"]: row["value"] for row in x.table("select * from settings")}
    check("settings: and puts them in use", (now.get("keymap"), now.get("theme"), now.get("mood")) == ("fr", "day", "calm"), now)
    x.type("q")
    time.sleep(0.2)
    check("settings: the keyboard types as it was left", body(x)[-1].endswith(":/>a"), body(x)[-1])
    x.key("backspace")
with Xila() as x:
    x.cmd("keymap fr")
    time.sleep(0.3)
    check("settings: tables in memory keep none", x.table("select name from tables where kind = 'stored'") == [], x.cmd("tables"))

# Two pictures shown in turn. QEMU has no chip that can, so this is the
# trial kind, with both pictures in memory: what it proves is that each of
# them gets every change, whatever the order of things. A change that one
# picture missed would show as old pixels every other time.
with Xila() as x:
    ok, lines = x.cmd("display flip on")
    check("display flip on: not without a chip XilaOS knows", not ok and lines == ["The graphics chip, 1234:1111, is not one XilaOS knows how to read."], lines)
    try:
        from PIL import Image, ImageChops

        def terminal(name):
            """The screen below the city, without the row of the prompt, where the cursor beats."""
            picture = Image.open(x.shot(os.path.join(shots, name))).convert("RGB")
            row = max(number for number, text in enumerate(x.peek()) if text.strip())
            picture.paste((0, 0, 0), (0, row * 16, 1280, row * 16 + 16))
            return picture.crop((0, 432, 1280, 800))

        def busy():
            """Changes of every size: a line, a screenful, all the colors, another screen, a code."""
            for typed in ("uptime", "help c", "theme night", "tables", "theme day"):
                x.type(typed)
                x.key("ret")
                time.sleep(0.15)
            x.key("alt-1")
            time.sleep(0.15)
            x.key("alt-0")
            x.type("qr about")
            x.key("ret")
            time.sleep(0.3)
            x.key("esc")
            x.type("select name from tasks")
            x.key("ret")
            time.sleep(0.4)

        ok, lines = x.cmd("display flip try")
        check("display flip try: pictures go through two in memory", ok and lines == ["Pictures now go through two pictures in memory: a test, no better to look at."], lines)
        rows = {row["item"]: row["value"] for row in x.table("display")}
        check("display: says how pictures change", rows.get("pictures change") == "through two pictures in memory (a test)", rows)
        busy()
        first = terminal("flip-first")
        # one more change, so that the other picture is the one on the screen
        x.type("x")
        time.sleep(0.2)
        x.key("backspace")
        time.sleep(0.1)
        x.type("y")
        time.sleep(0.2)
        second = terminal("flip-second")
        x.key("backspace")
        time.sleep(0.2)
        ok, lines = x.cmd("display test")
        check("display test: works with two pictures", ok and re.fullmatch(r"The whole screen: drawn in [\d.]+ ms, shown in [\d.]+ ms \(best of 5\)\.", lines[0]), lines)
        time.sleep(0.2)
        third = terminal("flip-third")
        pictures = int(x.table("select value from machine where item = 'pictures per second'")[0]["value"])
        rows = {row["item"]: row["value"] for row in x.table("display")}
        # the trial counts sixty sweeps per second: a picture every two, thirty per second
        check("with two pictures the screen gives the rhythm", rows.get("rhythm") == "a picture every 2 sweeps, 60 sweeps a second", rows)
        check("and the city moves at that rhythm", 26 <= pictures <= 36, pictures)
        ok, lines = x.cmd("display flip off")
        check("display flip off: the screen is written directly again", ok and lines == ["Pictures are written to the screen directly."], lines)
        time.sleep(0.2)
        direct = terminal("flip-direct")
        same = [ImageChops.difference(direct, picture).getbbox() for picture in (first, second, third)]
        check("each of the two pictures got every change", same == [None, None, None], same)
        check("and they hold a screenful of text", len(direct.getcolors(100000)) >= 3 and len([row for row in body(x) if row.strip()]) > 15, body(x)[-3:])
        # the same things done to a screen written directly end in the same picture
        rows = {row["item"]: row["value"] for row in x.table("display")}
        back = rows.get("pictures change") == "while the screen is swept" and rows.get("rhythm") == "a picture every 4 ticks, 100 ticks a second"
        check("display: pictures change while the screen is swept, as at the start", back, rows)
        # the count is that of the last whole second, which may have begun
        # up to two seconds ago: wait until none of it was at the other rhythm
        time.sleep(2.2)
        pictures = int(x.table("select value from machine where item = 'pictures per second'")[0]["value"])
        check("and the timer gives the rhythm again", 22 <= pictures <= 30, pictures)
    except ImportError:
        print("skip  two pictures shown in turn (needs the Pillow module)")

# A code on the screen, for a phone: what a command says leaves a machine
# that has no other way out. It is read back here by the reader of macOS.
def read_code(x, name):
    """The text of the code on the screen, or None without a reader."""
    if subprocess.run(["which", "swift"], capture_output=True).returncode != 0:
        return None
    x.shot(os.path.join(shots, name))
    found = subprocess.run(["swift", os.path.join(ROOT, "tools", "qr.swift"), os.path.join(shots, name + ".png")], capture_output=True, text=True)
    return found.stdout if found.returncode == 0 else "no code was read: " + found.stderr


def said(lines):
    """What a command said on the serial port, as `qr` keeps it."""
    return "".join(line.replace("#table ", "", 1).split("\t", 1)[1] + "\n" if line.startswith("#table ") else line + "\n" for line in lines if line != "#end")


with Xila() as x:
    ok, about = x.cmd("about")
    ok, lines = x.cmd("qr about")
    check("qr: a command's words become a code", ok and lines == ["A code of %d characters is on the screen." % len(said(about))], lines)
    time.sleep(0.4)
    check("qr: the code takes the screen", title(x) == "CODE" and x.peek()[-1].startswith("Read this code with the camera of a phone."), x.peek()[4:])
    text = read_code(x, "code-small")
    if text is not None:
        check("qr: another reader finds the same words in it", text == said(about), text)
    # a long text makes a code with small dots
    ok, helped = x.cmd("help c")
    ok, lines = x.cmd("qr help c")
    time.sleep(0.4)
    text = read_code(x, "code-large")
    if text is not None:
        check("qr: a text of two thousand characters is read back whole", len(text) > 1900 and text == said(helped), text)
    # what the kernel answers after the command is part of the text
    x.cmd("qr display test")
    time.sleep(0.4)
    text = read_code(x, "code-answer")
    if text is not None:
        check("qr: an answer of the kernel is in the code", re.fullmatch(r"The whole screen: drawn in [\d.]+ ms, shown in [\d.]+ ms \(best of 5\)\.\n", text), text)
    x.key("esc")
    time.sleep(0.4)
    check("qr: a key puts the code away", title(x) == "OVERVIEW" and body(x)[-1].startswith("XilaOS|all:/>"), body(x)[-3:])
    x.type("qr uptime")
    x.key("ret")
    time.sleep(0.4)
    check("qr typed at the keyboard shows a code", title(x) == "CODE", x.peek()[4:6])
    x.key("spc")
    time.sleep(0.4)
    check("qr: and the terminal is back, with what was said", title(x) == "OVERVIEW" and " characters is on the screen." in body(x)[-2], body(x)[-3:])
    ok, lines = x.cmd("qr")
    check("qr: without a command it says how", not ok and lines == ["Type qr and a command, to read what it says with a phone: qr about"], lines)
    ok, lines = x.cmd("qr clear")
    check("qr: a command that says nothing makes no code, and that is a failure", not ok and lines == ["That command said nothing: there is no code to show."] and title(x) == "OVERVIEW", lines)

# Installing. The machine starts from a USB stick, which XilaOS has no
# driver for: only the firmware reads it. Its disk holds another system,
# and what a system leaves behind where the new partitions will be.
subprocess.run([os.path.join(ROOT, "tools", "usb.sh")], check=True, stdout=subprocess.DEVNULL)
stick = os.path.join(ROOT, "build", "xilaos-usb.img")
system = open(os.path.join(ROOT, "build", "esp", "EFI", "BOOT", "BOOTX64.EFI"), "rb").read()
target = os.path.join(ROOT, "build", "install.img")
theirs, taken = partitioned_disk(target, 192, [(LINUX_PARTITION, 1, 120), (XILA_PARTITION, 121, 60)])
with open(target, "r+b") as image:
    image.seek(theirs[0])
    image.write(os.urandom(110 << 20))
    image.seek(taken[0])
    image.write(b"somebody else's data")


def struct_at(data, at, form):
    return struct.unpack_from("<" + form, data, at)


def partitions_of(disk):
    """Reads the partition table of the image of a disk, both copies, as the
    specification describes it. Returns (type, name, first byte, end byte)
    for each partition, or a text that says what is wrong."""
    sectors = len(disk) // 512
    if disk[510:512] != b"\x55\xaa" or disk[450] != 0xEE:
        return "the first sector does not say that the disk is taken"
    tables = []
    for here, other in ((1, sectors - 1), (sectors - 1, 1)):
        header = disk[here * 512:here * 512 + 92]
        said = struct_at(header, 16, "I")[0]
        if header[:8] != b"EFI PART" or zlib.crc32(header[:16] + bytes(4) + header[20:]) != said:
            return "the header at sector %d is wrong" % here
        if struct_at(header, 24, "QQ") != (here, other) or struct_at(header, 40, "QQ") != (34, sectors - 34):
            return "the header at sector %d does not say where things are" % here
        entries_at, count, size, entries_crc = struct_at(header, 72, "QIII")
        entries = disk[entries_at * 512:entries_at * 512 + count * size]
        if (count, size) != (128, 128) or entries_at != (2 if here == 1 else sectors - 33) or zlib.crc32(entries) != entries_crc:
            return "the entries of the header at sector %d are wrong" % here
        tables.append((header[56:72], entries))
    if tables[0] != tables[1]:
        return "the two copies of the table differ"
    found = []
    for at in range(0, 128 * 128, 128):
        entry = tables[0][1][at:at + 128]
        if entry[:16] != bytes(16):
            first, last = struct_at(entry, 32, "QQ")
            name = entry[56:].decode("utf-16-le").rstrip("\0")
            found.append((str(uuid.UUID(bytes_le=entry[:16])).upper(), name, first * 512, (last + 1) * 512))
    return found


def started_file(partition):
    """Reads EFI\\BOOT\\BOOTX64.EFI in a FAT32 partition the way a firmware
    does, from its first sector on."""
    if partition[510:512] != b"\x55\xaa" or partition[82:90] != b"FAT32   " or partition[:512] != partition[6 * 512:7 * 512]:
        return None
    sector, per_cluster, reserved, tables = struct_at(partition, 11, "HBHB")
    total, table, root = struct_at(partition, 32, "I")[0], struct_at(partition, 36, "I")[0], struct_at(partition, 44, "I")[0]
    data = reserved + tables * table
    if sector != 512 or total * 512 != len(partition) or (total - data) // per_cluster < 65525:
        return None
    if partition[reserved * 512:(reserved + table) * 512] != partition[(reserved + table) * 512:data * 512]:
        return None

    def chain(cluster):
        content = b""
        while 2 <= cluster < 0x0FFFFFF8:
            at = (data + (cluster - 2) * per_cluster) * 512
            content += partition[at:at + per_cluster * 512]
            cluster = struct_at(partition, reserved * 512 + cluster * 4, "I")[0] & 0x0FFFFFFF
        return content

    here, size = chain(root), None
    for name in (b"EFI        ", b"BOOT       ", b"BOOTX64 EFI"):
        entries = [here[at:at + 32] for at in range(0, len(here), 32)]
        entry = next((entry for entry in entries if entry[:11] == name), None)
        if entry is None:
            return None
        high, low, size = struct_at(entry, 20, "H")[0], struct_at(entry, 26, "H")[0], struct_at(entry, 28, "I")[0]
        here = chain(high << 16 | low)
    return here[:size]


def start_up_is_sound(partition, name):
    """Asks the tool of this machine that checks such partitions, if it has one."""
    if subprocess.run(["which", "fsck_msdos"], capture_output=True).returncode != 0:
        return
    path = os.path.join(ROOT, "build", "start-up.img")
    with open(path, "wb") as image:
        image.write(partition)
    result = subprocess.run(["fsck_msdos", "-n", path], capture_output=True, text=True)
    os.remove(path)
    check(name, result.returncode == 0, result.stdout + result.stderr)


def start_up_of(path):
    disk = open(path, "rb").read()
    found = partitions_of(disk)
    return disk[found[0][2]:found[0][3]] if isinstance(found, list) and found else b""


before = sha(open(target, "rb").read())
with Xila(stick=stick, sata=target) as x:
    log = "\n".join(x.boot_log)
    check("install: the machine starts from the image of the USB stick", "USB HARDDRIVE" in log and "System ready." in log, log)
    check("install: the kernel kept the file it was started from", "System file ......... %d KB, kept in memory for install" % -(-len(system) // 1024) in log, log)
    check("install: lists the disks and what they hold", x.table("install") == [{"disk": "1", "model": "QEMU HARDDISK", "size": "192 MB", "holds": "2 partitions"}], x.cmd("install"))
    ok, lines = x.cmd("install 1")
    said = lines[:2] == ["Disk 1, QEMU HARDDISK, 192 MB, holds: 2 partitions.", "Installing takes the whole disk: all it holds is lost."]
    check("install 1: says what will be lost and asks again", ok and said and lines[-1] == "To do it, type: install 1 yes", lines)
    ok, lines = x.cmd("install 2 yes")
    check("install: a disk that is not there is an error", not ok and lines == ["There is no disk 2. Type install to list the disks."], lines)
    ok, lines = x.cmd("install system")
    check("install system: only where XilaOS is installed", not ok and lines == ["XilaOS is not installed on any disk. Type install to list the disks."], lines)
check("install: until yes is typed the disk is exactly as it was", sha(open(target, "rb").read()) == before)

with Xila(stick=stick, sata=target) as x:
    x.cmd("create table lost (n int)")
    stored = "select name from tables where kind = 'stored'"
    check("install: a table made before is in memory", x.table(stored) == [{"name": "lost"}], x.cmd("tables"))
    ok, lines = x.cmd("install 1 yes")
    done = ["XilaOS is installed on disk 1. What was written was read back and is right.", "The tables are now kept on that disk.",
            "To start from it: take the USB stick out, then type reboot."]
    check("install 1 yes: the disk is taken", ok and lines == done, lines)
    # while it wrote, a bar on the screen said how far it was: the log keeps the count
    x.key("alt-1")
    shown = [re.search(r"Install: disk 1, (\d+) sectors written and read back, shown in (\d+) steps\.", row) for row in x.peek()]
    shown = [(int(found.group(1)), int(found.group(2))) for found in shown if found]
    check("install: the screen showed how far the writing was", len(shown) == 1 and shown[0][0] * 512 > len(system) and shown[0][1] == 41, shown)
    x.key("alt-0")
    check("install: and the bar is gone when it is done", not any("Reading it back" in row or "Writing the disk" in row for row in x.peek()), x.peek()[-2:])
    ok, lines = x.cmd("disk check")
    check("disk check: a new place for the tables has nothing to read yet", ok and lines == ["There are no stored tables yet: nothing to read."], lines)
    about = {row["item"]: row["value"] for row in x.table("disk")}
    check("install: the tables are on the new partition", about.get("disk") == "sata, partition of 90 MB" and about.get("stored tables") == "on disk, transaction 1", about)
    check("install: which starts empty", x.table(stored) == [], x.cmd("tables"))
    check("install: the disk is listed as holding XilaOS", x.table("install")[0]["holds"] == "XilaOS, in use", x.cmd("install"))
    x.cmd("create table notes (title text)")
    x.cmd("insert into notes values ('installed')")

# What is on the disk now, read by other means than XilaOS.
disk = open(target, "rb").read()
found = partitions_of(disk)
expected = [("C12A7328-F81F-11D2-BA4B-00A0C93EC93B", "XilaOS start", 1 << 20, 101 << 20), (XILA_PARTITION, "XilaOS", 101 << 20, 191 << 20)]
check("install: the partition table is whole, with its copy, and has the two partitions", found == expected, found)
check("install: a firmware finds the system in the start-up partition", started_file(start_up_of(target)) == system)
start_up_is_sound(start_up_of(target), "install: the start-up partition passes the check of this machine")

with Xila(bare=True, sata=target) as x:
    log = "\n".join(x.boot_log)
    check("install: the machine starts from its disk, without the stick", "HARDDISK" in log and "USB" not in log and "System ready." in log, log)
    check("install: and finds its tables", "Database ............ on the disk, 1 table" in log and x.table("select title from notes") == [{"title": "installed"}], log)
    # the disk it runs from, taken again: nothing of the old tables may stay
    ok, lines = x.cmd("install 1 yes")
    check("install: the disk the system runs from can be taken again", ok and lines == done, lines)
    ok, lines = x.cmd("select title from notes")
    check("install: the tables it held are gone", not ok, lines)
    x.cmd("create table second (n int)")
    ok, lines = x.cmd("insert into second values (2)")
    check("install: and new ones are kept", ok, lines)
    # typed at the keyboard, the answer comes on the screen
    x.type("install")
    x.key("ret")
    time.sleep(0.5)
    listed = [line for line in body(x) if "QEMU HARDDISK" in line]
    check("install typed at the keyboard lists the disks on the screen", len(listed) == 1 and "XilaOS, in use" in listed[0], body(x)[-6:])
with Xila(bare=True, sata=target) as x:
    log = "\n".join(x.boot_log)
    check("install: after that the machine starts from its disk again", "Database ............ on the disk, 1 table" in log and x.table("select n from second") == [{"n": "2"}], log)

# The system alone. What starts the machine is destroyed first: the stick
# must bring it back and leave the tables alone.
found = partitions_of(open(target, "rb").read())
with open(target, "r+b") as image:
    image.seek(found[0][2])
    image.write(bytes(8 << 20))
tables_before = sha(open(target, "rb").read()[found[1][2]:found[1][3]])
with Xila(stick=stick, sata=target) as x:
    log = "\n".join(x.boot_log)
    check("install system: from the stick, the tables on the disk are found", "Database ............ on the disk, 1 table" in log, log)
    ok, lines = x.cmd("install system")
    replaced = ["The system on disk 1 is replaced. What was written was read back and is right.",
                "The tables were not touched. It runs at the next start: type reboot."]
    check("install system: the system is replaced", ok and lines == replaced, lines)
    ok, lines = x.cmd("install 1 now")
    check("install: an unknown word is an error", not ok and lines == ["Type install to list the disks, install 1 to take disk 1, or install system."], lines)
disk = open(target, "rb").read()
check("install system: the partition of the tables is exactly as it was", sha(disk[found[1][2]:found[1][3]]) == tables_before)
check("install system: the partition table too", partitions_of(disk) == found, partitions_of(disk))
check("install system: a firmware finds the system again", started_file(start_up_of(target)) == system)
start_up_is_sound(start_up_of(target), "install system: the start-up partition passes the check of this machine")
with Xila(bare=True, sata=target) as x:
    log = "\n".join(x.boot_log)
    check("install system: the machine starts from its disk", "System ready." in log and x.table("select n from second") == [{"n": "2"}], log)

# The system from the network, without a stick: the machine, started from
# its disk, fetches a newer system from a machine that serves it and puts
# it in place of its own. The newer one is the same with a tail, to tell
# them apart.
import tempfile
served = tempfile.mkdtemp(prefix="xilaserved")
newer = system + bytes(4096)
with open(os.path.join(served, "BOOTX64.EFI"), "wb") as file:
    file.write(newer)
no_system = ["This XilaOS cannot install: the firmware did not give it its own file.",
             "A machine that serves the system can give it: install from and its address."]
with Xila(bare=True, sata=target, net="e1000e", tftp=served) as x:
    for _ in range(40):
        if {row["item"]: row["value"] for row in x.table("net")}.get("address") == "10.0.2.15":
            break
        time.sleep(0.25)
    ok, lines = x.cmd("install from 10.0.2.2", timeout=90)
    gave = ["10.0.2.2 gave a system of %d bytes, check %08x." % (len(newer), zlib.crc32(newer)),
            "Nothing is written yet. To put it on a disk: install, or install system."]
    check("install from: fetches the system and says its size and check number", ok and lines == gave, lines)
    ok, lines = x.cmd("install system")
    check("install from: install system puts that one on the disk", ok and lines == replaced, lines)
    # what is not a system is not kept, and nothing is left to install
    with open(os.path.join(served, "BOOTX64.EFI"), "wb") as file:
        file.write(b"not a program at all. " * 400)
    ok, lines = x.cmd("install from 10.0.2.2", timeout=90)
    check("install from: what is not a system is refused", not ok and lines == ["10.0.2.2 gave something that is not a system for this machine."], lines)
    ok, lines = x.cmd("install system")
    check("install from: and install has nothing to put on a disk then", not ok and lines == no_system, lines)
    os.remove(os.path.join(served, "BOOTX64.EFI"))
    ok, lines = x.cmd("install from 10.0.2.2", timeout=90)
    check("install from: a machine that has no system says so", not ok and lines == ["10.0.2.2 has no system to give."], lines)
    ok, lines = x.cmd("install from 10.0.2.99", timeout=90)
    check("install from: a machine that is not there does not answer", not ok and lines == ["10.0.2.99 does not answer. Is tools/netboot.py running there?"], lines)
    check("install from: the network still works after all that", x.cmd("net ping 10.0.2.2")[1][0].startswith("10.0.2.2 answered in "))
disk = open(target, "rb").read()
check("install from: a firmware finds the fetched system on the disk", started_file(start_up_of(target)) == newer)
check("install from: the partition of the tables is exactly as it was", sha(disk[found[1][2]:found[1][3]]) == tables_before)
with Xila(bare=True, sata=target) as x:
    log = "\n".join(x.boot_log)
    check("install from: the machine starts from its disk with it", "System ready." in log and x.table("select n from second") == [{"n": "2"}], log)

# A disk too small for the two partitions is refused before anything is written.
small = blank_disk(os.path.join(ROOT, "build", "small.img"), 100)
with Xila(stick=stick, sata=small) as x:
    ok, lines = x.cmd("install 1 yes")
    check("install: a disk that is too small is refused", not ok and lines == ["Disk 1 is too small: XilaOS needs 118 MB at least."], lines)
check("install: and nothing is written on it", open(small, "rb").read() == bytes(100 << 20))

print()
print("%d failed" % len(failed) if failed else "all checks passed")
sys.exit(1 if failed else 0)
