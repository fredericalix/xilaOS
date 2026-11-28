//! The commands of the prompt.
//!
//! A command writes to an `Out`, which is the terminal on the screen or the
//! serial port. Results that are lists go through a `Table`, so that the
//! screen can align and number them and the serial port can send them in a
//! form a program reads easily.

use core::fmt::Write;

use crate::grid::{COLS, MAX_ROWS};
use crate::events::{Events, Listen};
use crate::keys::Layout;
use crate::sql::Name;
use crate::text::{parse_number, split_word, trim, FixedStr};
use crate::theme::THEMES;

pub const PROMPT: &[u8] = b"XilaOS|all:/>";

/// The virtual screens, in the order of their Alt+digit shortcut.
pub const SCREENS: [&[u8]; 3] = [b"overview", b"boot log", b"companion"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    Out,
    Error,
    /// Says that what was asked is done: `1 row added...`. A program does
    /// not show these, a prompt does.
    Done,
}

pub const TABLE_COLS: usize = xila_db::MAX_COLUMNS;
pub const TABLE_ROWS: usize = 32;
pub type Text = FixedStr<48>;

/// A small result table. Rows may carry a command that is run when the user
/// types the number of the row.
pub struct Table {
    pub name: Name,
    pub header: [Name; TABLE_COLS],
    pub cols: usize,
    pub rows: usize,
    pub cells: [[Text; TABLE_COLS]; TABLE_ROWS],
    pub follow: [Text; TABLE_ROWS],
    pub numbered: bool,
    /// Rows that exist but did not fit in the table.
    pub more: usize,
}

impl Table {
    pub fn new(name: &[u8], header: &[&[u8]]) -> Table {
        let mut h = [Name::new(); TABLE_COLS];
        let cols = header.len().min(TABLE_COLS);
        for (to, from) in h.iter_mut().zip(header) {
            *to = Name::from_bytes(from);
        }
        Table { name: Name::from_bytes(name), header: h, cols, rows: 0, cells: [[Text::new(); TABLE_COLS]; TABLE_ROWS], follow: [Text::new(); TABLE_ROWS], numbered: false, more: 0 }
    }

    /// Adds a row. Rows beyond the capacity are dropped.
    pub fn row(&mut self, cells: &[&[u8]]) {
        if self.rows == TABLE_ROWS {
            return;
        }
        for (i, cell) in cells.iter().take(self.cols).enumerate() {
            self.cells[self.rows][i] = Text::from_bytes(cell);
        }
        self.rows += 1;
    }

    /// Adds a row that runs `command` when its number is typed.
    pub fn row_to(&mut self, cells: &[&[u8]], command: &[u8]) {
        if self.rows < TABLE_ROWS {
            self.follow[self.rows] = Text::from_bytes(command);
            self.numbered = true;
            self.row(cells);
        }
    }

    fn width(&self, col: usize) -> usize {
        let mut w = self.header[col].len();
        for r in 0..self.rows {
            w = w.max(self.cells[r][col].len());
        }
        w
    }

    /// One row as aligned text; `None` gives the header.
    pub fn aligned(&self, row: Option<usize>) -> FixedStr<COLS> {
        let mut s = FixedStr::<COLS>::new();
        if self.numbered {
            match row {
                Some(r) => {
                    let _ = write!(s, "{:>4} ", FixedNum(r + 1));
                }
                None => s.push_bytes(b"     "),
            }
        } else {
            s.push_bytes(b"   ");
        }
        for col in 0..self.cols {
            let cell: &[u8] = match row {
                Some(r) => self.cells[r][col].bytes(),
                None => self.header[col].bytes(),
            };
            s.push_bytes(cell);
            if col + 1 < self.cols {
                for _ in cell.len()..self.width(col) + 2 {
                    s.push(b' ');
                }
            }
        }
        s
    }
}

/// Formats a row number as `[n]`.
struct FixedNum(usize);

impl core::fmt::Display for FixedNum {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let mut s = FixedStr::<8>::new();
        let _ = write!(s, "[{}]", self.0);
        // only ASCII was written
        f.pad(core::str::from_utf8(s.bytes()).unwrap_or(""))
    }
}

pub trait Out {
    fn line(&mut self, style: Style, text: &[u8]);
    fn table(&mut self, table: &Table);
    /// Empties what the user sees, where that makes sense.
    fn clear(&mut self) {}
}

/// What a command says, kept as text to make a code of it (`qr`): one
/// line per line, and the cells of a table apart by tabs.
#[derive(Clone, Copy)]
pub struct Note {
    bytes: [u8; crate::qr::MOST_TEXT],
    len: usize,
    /// Characters that did not fit.
    pub lost: usize,
}

impl Note {
    pub const fn new() -> Note {
        Note { bytes: [0; crate::qr::MOST_TEXT], len: 0, lost: 0 }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn push(&mut self, text: &[u8]) {
        for &byte in text {
            if self.len < self.bytes.len() {
                // a phone reads the text as its own characters: only those both have
                self.bytes[self.len] = if byte.is_ascii() { byte } else { b'?' };
                self.len += 1;
            } else {
                self.lost += 1;
            }
        }
    }
}

impl Out for Note {
    fn line(&mut self, _style: Style, text: &[u8]) {
        self.push(text);
        self.push(b"\n");
    }

    fn table(&mut self, t: &Table) {
        for col in 0..t.cols {
            self.push(if col > 0 { b"\t" } else { b"" });
            self.push(t.header[col].bytes());
        }
        self.push(b"\n");
        for row in 0..t.rows {
            for col in 0..t.cols {
                self.push(if col > 0 { b"\t" } else { b"" });
                self.push(t.cells[row][col].bytes());
            }
            self.push(b"\n");
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoodMode {
    Calm,
    Auto,
    Intense,
}

impl MoodMode {
    pub fn name(self) -> &'static [u8] {
        match self {
            MoodMode::Calm => b"calm",
            MoodMode::Auto => b"auto",
            MoodMode::Intense => b"intense",
        }
    }

    pub fn next(self) -> MoodMode {
        match self {
            MoodMode::Calm => MoodMode::Auto,
            MoodMode::Auto => MoodMode::Intense,
            MoodMode::Intense => MoodMode::Calm,
        }
    }
}

/// The mood is one number, from 0 (calm) to 1000 (intense).
pub const ENERGY_MAX: u16 = 1000;

/// What the kernel found out about the machine at start.
pub struct Machine {
    pub cpu: FixedStr<48>,
    pub firmware: FixedStr<32>,
    pub screen_width: u32,
    pub screen_height: u32,
    /// Pixels per font pixel.
    pub scale: u32,
    pub serial: bool,
    /// Memory in KB: usable, and kept by the firmware for its own tables.
    pub mem_usable_kb: u64,
    pub mem_firmware_kb: u64,
    /// Memory the kernel manages itself, and how much of it is free.
    pub mem_managed_kb: u64,
    pub mem_free_kb: u64,
    /// Bytes of the kernel heap in use.
    pub heap_used: u64,
    pub mem_regions: u32,
    /// The disk the kernel found for the database, and what it did with it.
    pub disk: Text,
    /// The graphics chip, by its name if XilaOS knows it.
    pub graphics: Text,
}

/// How a picture gets to the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pictures {
    /// What changed is written to the screen, while it is being swept.
    Direct,
    /// The graphics chip shows one of two pictures while the other is
    /// prepared, and changes between two sweeps of the screen.
    Two,
    /// The same with two pictures in ordinary memory, copied whole to the
    /// screen: slower and no better to look at, but it works on any
    /// machine, QEMU for one, and so the rest of the system can be tried.
    Trial,
}

/// How the screen is drawn, as the kernel measures it.
pub struct Display {
    pub shown: Pictures,
    /// When the screen itself gives the rhythm: how many of its sweeps a
    /// picture of what moves by itself stays, and sweeps per second. Both 0
    /// while the timer gives the rhythm (`FRAME_TICKS`).
    pub every: u32,
    pub sweeps: u32,
    /// The brightness of the screen in hundredths, once the user has set
    /// it or a start has found it kept; and whether the user asked for the
    /// screen to be written directly. Both are kept (`settings`).
    pub light: Option<u32>,
    pub direct: bool,
    /// The processor gathers what is written to the memory of the screen
    /// (write combining) instead of sending it piece by piece.
    pub combining: bool,
    /// Microseconds to copy a whole picture to the screen, measured when the
    /// system started: as the screen memory is now, and as the firmware had
    /// left it. On a real machine the difference is what write combining gains.
    pub whole_us: u32,
    pub firmware_us: u32,
    /// Pictures shown during the last whole second, and how long the longest
    /// of them took, in microseconds.
    pub pictures: u32,
    pub longest_us: u32,
    /// Microseconds to draw the last picture of the city and enlarge it.
    pub city_us: u32,
    /// Pictures of what moves by itself that were skipped since the start,
    /// because the system was late.
    pub skipped: u64,
}

/// What the kernel measured for `display test`: microseconds to draw every
/// character of the screen, and to copy the whole picture to the screen.
/// The best of several rounds, because another task can take the processor
/// in the middle of one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Measure {
    pub rounds: u32,
    pub drawn_us: u32,
    pub shown_us: u32,
}

/// Timer ticks between two pictures of what moves by itself, the cursor and
/// the city: 25 pictures per second. What the user changes is shown at the
/// next tick, without waiting for a picture.
pub const FRAME_TICKS: u64 = 4;

/// Most tasks the kernel runs at once.
pub const MAX_TASKS: usize = 8;

/// A task of the kernel, as commands see it.
#[derive(Clone, Copy)]
pub struct TaskInfo {
    pub name: FixedStr<16>,
    pub state: &'static [u8],
    /// Timer ticks during which the task had the processor.
    pub ticks: u64,
    /// Tasks the system needs cannot be stopped.
    pub system: bool,
    pub used: bool,
}

impl TaskInfo {
    pub const NONE: TaskInfo = TaskInfo { name: FixedStr::new(), state: b"", ticks: 0, system: false, used: false };
}

/// How many registers of the network card `net registers` lists.
pub const NET_REGISTERS: usize = 32;

/// The network card the kernel drives, as `net` shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NetCard {
    pub name: &'static str,
    /// The speed of the link in megabits per second, and whether both
    /// ends may talk at once; None if the cable is out or the other end silent.
    pub link: Option<(u32, bool)>,
    /// Frames the card saw damaged, and frames it had no room for.
    pub damaged: u64,
    pub missed: u64,
    /// Frames given to the card to send, and frames it was too busy to take.
    pub sent: u64,
    pub refused: u64,
}

/// Most devices `devices` lists.
pub const MAX_DEVICES: usize = TABLE_ROWS;

/// Something on the PCI bus of the machine, as `devices` shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Device {
    /// Where it is: bus, device, function.
    pub place: (u8, u8, u8),
    /// `model << 16 | maker`.
    pub id: u32,
    /// `class << 16 | subclass << 8 | interface`.
    pub class: u32,
    /// XilaOS has a driver for it and uses it.
    pub driven: bool,
}

/// What a device is, from its class: the words of the PCI standard, made
/// plain.
fn device_kind(class: u32) -> &'static str {
    match (class >> 16, class >> 8 & 0xff, class & 0xff) {
        (0x01, 0x06, _) => "SATA disks",
        (0x01, 0x08, _) => "NVMe disk",
        (0x01, 0x01, _) => "IDE disks",
        (0x01, _, _) => "disks",
        (0x02, 0x00, _) => "Ethernet",
        (0x02, 0x80, _) | (0x0d, _, _) => "wireless network",
        (0x02, _, _) => "network",
        (0x03, _, _) => "graphics",
        (0x04, 0x03, _) | (0x04, 0x01, _) => "sound",
        (0x04, _, _) => "sound or video",
        (0x05, _, _) => "memory",
        (0x06, 0x00, _) => "bridge to the processor",
        (0x06, 0x01, _) => "bridge to old devices",
        (0x06, 0x04, _) => "bridge to another bus",
        (0x06, _, _) => "bridge",
        (0x07, 0x00, _) => "serial port",
        (0x07, _, _) => "communication",
        (0x08, _, _) => "part of the system",
        (0x09, _, _) => "input",
        (0x0c, 0x03, 0x30) => "USB 3",
        (0x0c, 0x03, 0x20) => "USB 2",
        (0x0c, 0x03, _) => "USB 1",
        (0x0c, 0x05, _) => "bus of small sensors",
        (0x0c, _, _) => "serial bus",
        (0x10, _, _) => "encryption",
        _ => "something else",
    }
}

/// The tables as pages for the browsers of the local network: what state
/// that is in, `web on` to open it with a new code, `web off` to close
/// it. See `web.rs`.
fn web(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    use crate::remote::written;
    use crate::web::WRONG;
    if arg.eq_ignore_ascii_case(b"on") {
        if !at_the_machine(sys) {
            out.line(Style::Error, b"web on is typed at the machine itself.");
            return false;
        }
        let Some(code) = sys.web.open(sys.chance ^ sys.ticks.rotate_left(24), &mut sys.net.tcp) else {
            out.line(Style::Error, b"The network listens on as many ports as it can already.");
            return false;
        };
        match sys.net.ip {
            Some(ip) => {
                out.line(Style::Done, b"The tables can be read from a browser on the local network, at:");
                let mut line = FixedStr::<COLS>::from_bytes(b"http://");
                line.push_bytes(crate::net::ip_text(ip).bytes());
                line.push(b'/');
                line.push_bytes(written(code).bytes());
                line.push(b'/');
                out.line(Style::Out, line.bytes());
            }
            None => {
                let mut line = FixedStr::<COLS>::from_bytes(b"The tables can be read from a browser, with the code ");
                line.push_bytes(written(code).bytes());
                line.push(b'.');
                out.line(Style::Done, line.bytes());
                out.line(Style::Out, b"This machine has no address on the network yet. Type net to see why.");
            }
        }
        out.line(Style::Out, b"Nothing is encrypted: who sees this address can read every table. web off closes it.");
        return true;
    }
    if arg.eq_ignore_ascii_case(b"off") {
        sys.web.close(&mut sys.net.tcp);
        sys.web.gave_up = false;
        out.line(Style::Done, b"The tables are closed to the network.");
        return true;
    }
    if !arg.is_empty() {
        out.line(Style::Error, b"Type web, web on or web off.");
        return false;
    }
    let mut t = Table::new(b"web", &[b"item", b"value"]);
    let mut text = Text::new();
    match (sys.web.code(), sys.net.ip) {
        (Some(code), Some(ip)) => {
            t.row(&[b"the tables", b"open to browsers of the local network"]);
            let _ = write!(text, "http://");
            text.push_bytes(crate::net::ip_text(ip).bytes());
            text.push(b'/');
            text.push_bytes(written(code).bytes());
            text.push(b'/');
            t.row(&[b"at", text.bytes()]);
        }
        (Some(code), None) => {
            t.row(&[b"the tables", b"open, but this machine has no address"]);
            t.row(&[b"code", written(code).bytes()]);
        }
        (None, _) if sys.web.gave_up => {
            let _ = write!(text, "closed itself after {WRONG} wrong codes");
            t.row(&[b"the tables", text.bytes()]);
        }
        (None, _) => t.row(&[b"the tables", b"closed to the network"]),
    }
    if sys.web.code().is_some() || sys.web.gave_up {
        for (item, count) in [(&b"pages given"[..], sys.web.served), (b"wrong codes", sys.web.wrong)] {
            text.clear();
            let _ = write!(text, "{count}");
            t.row(&[item, text.bytes()]);
        }
        if let Some(from) = sys.web.from {
            t.row(&[b"last asked from", crate::net::ip_text(from).bytes()]);
        }
    }
    out.table(&t);
    true
}

/// The prompt for the network: what state it is in, `remote on` to open
/// it with a new code, `remote off` to close it. See `remote.rs`.
fn remote(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    use crate::remote::{written, PORT, WRONG};
    if arg.eq_ignore_ascii_case(b"on") {
        if !at_the_machine(sys) {
            out.line(Style::Error, b"remote on is typed at the machine itself.");
            return false;
        }
        // the moment it is typed is part of what nobody else can guess
        let code = written(sys.remote.open(sys.chance ^ sys.ticks.rotate_left(32)));
        let mut line = FixedStr::<COLS>::new();
        line.push_bytes(b"This prompt is open to the local network. The code is ");
        line.push_bytes(code.bytes());
        line.push(b'.');
        out.line(Style::Done, line.bytes());
        line.clear();
        match sys.net.ip {
            Some(ip) => {
                line.push_bytes(b"From another machine: python3 tools/remote.py ");
                line.push_bytes(crate::net::ip_text(ip).bytes());
                line.push(b' ');
                line.push_bytes(code.bytes());
            }
            None => line.push_bytes(b"This machine has no address on the network yet. Type net to see why."),
        }
        out.line(Style::Out, line.bytes());
        out.line(Style::Out, b"Nothing is encrypted: who sees the code can type here. remote off closes it.");
        return true;
    }
    if arg.eq_ignore_ascii_case(b"off") {
        sys.remote.close();
        out.line(Style::Done, b"This prompt is closed to the network.");
        return true;
    }
    if !arg.is_empty() {
        out.line(Style::Error, b"Type remote, remote on or remote off.");
        return false;
    }
    let mut t = Table::new(b"remote", &[b"item", b"value"]);
    let mut text = Text::new();
    match sys.remote.code() {
        Some(code) => {
            let _ = write!(text, "open, on port {PORT}");
            t.row(&[b"this prompt", text.bytes()]);
            t.row(&[b"code", written(code).bytes()]);
        }
        None if sys.remote.gave_up => {
            let _ = write!(text, "closed itself after {WRONG} wrong codes");
            t.row(&[b"this prompt", text.bytes()]);
        }
        None => t.row(&[b"this prompt", b"closed to the network"]),
    }
    match sys.net.ip {
        Some(ip) => t.row(&[b"address", crate::net::ip_text(ip).bytes()]),
        None => t.row(&[b"address", b"none yet"]),
    }
    if sys.remote.code().is_some() || sys.remote.gave_up {
        for (item, count) in [(&b"commands run"[..], sys.remote.served), (b"wrong codes", sys.remote.wrong)] {
            text.clear();
            let _ = write!(text, "{count}");
            t.row(&[item, text.bytes()]);
        }
        if let Some(from) = sys.remote.from {
            t.row(&[b"last asked from", crate::net::ip_text(from).bytes()]);
        }
    }
    out.table(&t);
    true
}

/// The network: the card, the cable, the address the network gave. `net
/// ping 192.168.1.1` asks another machine to answer.
fn net(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    let Some(card) = sys.net_card else {
        out.line(Style::Error, b"XilaOS drives no network card of this machine. Type devices to see what it has.");
        return false;
    };
    let (word, rest) = split_word(arg);
    if word.eq_ignore_ascii_case(b"ping") {
        let Some(to) = crate::net::ip_of(trim(rest)) else {
            out.line(Style::Error, b"Type net ping and an address: net ping 192.168.1.1");
            return false;
        };
        if sys.net.ip.is_none() {
            out.line(Style::Error, b"This machine has no address on the network yet. Type net to see why.");
            return false;
        }
        sys.request = Some(Request::Ping(to));
        return true;
    }
    if word.eq_ignore_ascii_case(b"name") {
        // the address of a machine known by its name
        let name = trim(rest);
        if !crate::dns::valid(name) {
            out.line(Style::Error, b"Type net name and the name of a machine: net name example.org");
            return false;
        }
        if sys.net.ip.is_none() {
            out.line(Style::Error, b"This machine has no address on the network yet. Type net to see why.");
            return false;
        }
        sys.asked_machine = FixedStr::from_bytes(name);
        sys.request = Some(Request::Name);
        return true;
    }
    if word.eq_ignore_ascii_case(b"names") {
        // who is asked for names, in place of whom the network named
        let rest = trim(rest);
        let (address, port) = match rest.iter().position(|byte| *byte == b':') {
            Some(colon) => (&rest[..colon], parse_number(&rest[colon + 1..]).filter(|port| (1..65536).contains(port))),
            None => (rest, Some(crate::dns::PORT as usize)),
        };
        let (Some(server), Some(port)) = (crate::net::ip_of(address), port) else {
            out.line(Style::Error, b"Type net names and the address of a name server: net names 192.168.1.1");
            return false;
        };
        sys.net.names.set(server, port as u16);
        let mut line = FixedStr::<COLS>::from_bytes(b"Names are now asked of ");
        line.push_bytes(name_server(server, port as u16).bytes());
        line.push(b'.');
        out.line(Style::Done, line.bytes());
        return true;
    }
    let post = word.eq_ignore_ascii_case(b"post");
    if post || word.eq_ignore_ascii_case(b"get") {
        // a page of another machine, or a text sent to one: the calling
        // side of the network, tried by hand
        let (place, text) = if post { split_word(trim(rest)) } else { (trim(rest), &b""[..]) };
        let Some(place) = crate::ask::place(place).filter(|place| place.path.len() <= 200) else {
            out.line(Style::Error, if post { b"Type net post, where, and a text: net post 192.168.1.20:8000/notes 'hello'" } else { b"Type net get and where to ask: net get 192.168.1.20:8000/page" });
            return false;
        };
        // the text is a value: a 'text' between quotes, or a calculation
        let mut said = Text::new();
        if post {
            let mut lexer = crate::sql::Lexer::new(text);
            let names = crate::expr::Names::without(sys.names(), b"Put the text between quotes, or give a name that has a value, not: ");
            let value = crate::expr::parse(&mut lexer, names).and_then(|text| lexer.end().map(|_| text)).and_then(|text| text.eval(&[crate::sql::Value::Null; TABLE_COLS]));
            match value {
                Ok(value) => said = value.shown(),
                Err(_) if trim(text).is_empty() => {
                    out.line(Style::Error, b"Type net post, where, and a text: net post 192.168.1.20:8000/notes 'hello'");
                    return false;
                }
                Err(message) => {
                    out.line(Style::Error, message.bytes());
                    return false;
                }
            }
        }
        if sys.net.ip.is_none() {
            out.line(Style::Error, b"This machine has no address on the network yet. Type net to see why.");
            return false;
        }
        sys.asked_machine = FixedStr::from_bytes(place.machine);
        sys.asked_path = FixedStr::from_bytes(place.path);
        sys.asked_text = said;
        sys.request = Some(Request::Ask { to: place.ip, port: place.port, secret: place.secret, post });
        return true;
    }
    if arg.eq_ignore_ascii_case(b"registers") {
        // for whoever wants to know why a card does not do what it should
        sys.request = Some(Request::NetRegisters);
        return true;
    }
    if arg.eq_ignore_ascii_case(b"restart") {
        sys.request = Some(Request::NetRestart);
        return true;
    }
    if !arg.is_empty() {
        out.line(Style::Error, b"After net: restart, registers, ping, get, post, name or names, and what they need.");
        return false;
    }
    let mut t = Table::new(b"net", &[b"item", b"value"]);
    t.row(&[b"card", card.name.as_bytes()]);
    if let Some(mac) = sys.net.mac {
        t.row(&[b"its address", crate::net::mac_text(mac).bytes()]);
    }
    let mut cable = Text::new();
    let _ = match card.link {
        None => write!(cable, "not plugged in, or the other end is silent"),
        Some((speed, true)) => write!(cable, "{speed} megabits per second"),
        Some((speed, false)) => write!(cable, "{speed} megabits per second, one way at a time"),
    };
    t.row(&[b"cable", cable.bytes()]);
    match sys.net.ip {
        Some(ip) => {
            t.row(&[b"address", crate::net::ip_text(ip).bytes()]);
            t.row(&[b"mask", crate::net::ip_text(sys.net.mask).bytes()]);
            t.row(&[b"gateway", crate::net::ip_text(sys.net.gateway).bytes()]);
            match sys.net.names.server {
                Some((server, port)) => {
                    let mut text = name_server(server, port);
                    if sys.net.names.by_hand {
                        text.push_bytes(b", set by hand");
                    }
                    t.row(&[b"name server", text.bytes()]);
                }
                None => t.row(&[b"name server", b"none named by the network"]),
            }
            let mut kept = Text::new();
            let minutes = sys.net.held_until().map_or(0, |until| until.saturating_sub(sys.ticks) / 6000);
            let _ = if minutes >= 120 { write!(kept, "for {} hours more", minutes / 60) } else { write!(kept, "for {minutes} minutes more") };
            t.row(&[b"given by the network", kept.bytes()]);
        }
        None => t.row(&[b"address", if card.link.is_some() { b"none yet: asking the network for one" } else { b"none: no cable" }]),
    }
    let mut count = |item: &[u8], value: u64| {
        let mut text = Text::new();
        let _ = write!(text, "{value}");
        t.row(&[item, text.bytes()]);
    };
    count(b"frames received", sys.net.received);
    count(b"frames sent", card.sent);
    count(b"not sent, card busy", card.refused);
    count(b"damaged on the way", card.damaged);
    count(b"missed, no room", card.missed);
    show(sys, out, &t);
    true
}

/// A name server as it is written: its address, and its port when it is
/// not the usual one.
fn name_server(server: crate::net::Ip, port: u16) -> Text {
    let mut text = crate::net::ip_text(server);
    if port != crate::dns::PORT {
        let _ = write!(text, ":{port}");
    }
    text
}

/// Why the address of a name was not found, for the user.
pub(crate) fn unnamed(why: crate::dns::Failed) -> &'static [u8] {
    use crate::dns::Failed;
    match why {
        Failed::NoServer => b"The network named no server to ask for names. Type net names and an address.",
        Failed::Silent => b"The name server did not answer.",
        Failed::NoSuchName => b"The name server knows no machine of that name.",
        Failed::NoAddress => b"The name server gave no address for that name.",
    }
}

/// Why a machine could not be asked through TLS, for the user. Nothing
/// was asked of it, or nothing of what it said is shown.
pub(crate) fn unsure(why: crate::ask::Failed) -> FixedStr<COLS> {
    use crate::ask::Failed;
    use xila_tls::{Error, Refused};
    let text: &[u8] = match why {
        Failed::NoChance => b"This processor has no source of chance: nothing here can be kept secret.",
        Failed::NoClock => b"This machine does not know the date: no certificate can be checked.",
        Failed::Secret(Error::Certificate(why)) => match why {
            Refused::Expired => b"That machine's certificate is out of date, by the clock of this one.",
            Refused::NotYet => b"That machine's certificate is not good yet, by the clock of this one.",
            Refused::WrongName => b"That machine showed the certificate of another: it is not who was asked.",
            Refused::Unknown => b"No authority that XilaOS trusts vouches for that machine's certificate.",
            Refused::Unsupported => b"That machine's certificate is signed in a way XilaOS cannot check.",
            Refused::Forged => b"A signature in that machine's certificates is not its signer's.",
            Refused::Bad => b"That machine's certificate cannot be read, or is not a server's.",
        },
        Failed::Secret(Error::NotTheirs) => b"That machine does not hold the key of the certificate it showed.",
        Failed::Secret(Error::Forged) => b"What that machine said was changed on the way: nothing of it is shown.",
        Failed::Secret(Error::NotTls) => b"That machine does not speak TLS there, or not as TLS is spoken.",
        // said by us, or by the machine with the numbers TLS has for it
        Failed::Secret(Error::Old | Error::Alert(70)) => b"That machine only speaks a TLS older than 1.3, which XilaOS does not.",
        Failed::Secret(Error::NoCommon | Error::Alert(40)) => b"That machine and XilaOS have no way in common of keeping a secret.",
        Failed::Secret(Error::TooLong) => b"That machine showed more certificates than XilaOS has room for.",
        Failed::Secret(Error::Alert(number)) => {
            let mut line = FixedStr::new();
            let _ = write!(line, "That machine gave up before the conversation was secret (TLS alert {number}).");
            return line;
        }
        _ => b"That machine could not be asked.",
    };
    FixedStr::from_bytes(text)
}

/// Lists what the PCI bus of the machine holds, and what XilaOS drives.
fn devices(sys: &mut System, out: &mut dyn Out) -> bool {
    let mut t = Table::new(b"devices", &[b"place", b"kind", b"maker_model", b"class", b"driver"]);
    for device in sys.devices.iter().flatten() {
        let (mut place, mut id, mut class) = (Text::new(), Text::new(), Text::new());
        let _ = write!(place, "{:02x}:{:02x}.{}", device.place.0, device.place.1, device.place.2);
        let _ = write!(id, "{:04x}:{:04x}", device.id & 0xffff, device.id >> 16);
        let _ = write!(class, "{:06x}", device.class);
        t.row(&[place.bytes(), device_kind(device.class).as_bytes(), id.bytes(), class.bytes(), if device.driven { b"XilaOS" } else { b"none" }]);
    }
    if t.rows == 0 {
        out.line(Style::Out, b"No device was found on the PCI bus.");
        return true;
    }
    show(sys, out, &t);
    true
}

/// Most disks `install` lists.
pub const MAX_DISKS: usize = 8;

/// A disk the kernel found, as `install` shows it.
#[derive(Clone, Copy)]
pub struct DiskInfo {
    /// What the disk says it is.
    pub model: FixedStr<40>,
    pub sectors: u64,
    pub holds: crate::gpt::Holds,
    /// The partition the kernel found for the tables at start, or was
    /// given since, is on this disk: the tables are there, unless that
    /// partition was refused (`System::spare`).
    pub taken: bool,
}

/// What `install` did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Installed {
    /// The number of the disk, from 1.
    pub disk: usize,
    /// The whole disk was taken; or only the system was replaced.
    pub whole: bool,
}

/// Why `install` did not do it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstallError {
    Disk(crate::install::Failed),
    /// XilaOS is on the disk, but the place of the tables could not be
    /// prepared.
    Database(xila_db::Error),
}

/// What a machine answered to `net get` or `net post`: the number that
/// says how it went, the start of what it said, and how many bytes it
/// said in all.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Got {
    pub status: u16,
    pub start: FixedStr<640>,
    pub total: usize,
}

/// What a command asks the kernel to do once it has returned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
    /// Start a task that uses the processor without ever resting.
    Spin,
    /// Stop the task with this number.
    Stop(usize),
    /// Time the drawing of a whole screen. The answer comes in
    /// `System::measured` and `served` shows it.
    Measure,
    /// Change how pictures get to the screen. The answer comes in
    /// `System::flipped`.
    Flip(Pictures),
    /// Send a ping to this address and wait a second for its answer,
    /// which comes in `System::pinged`.
    Ping(crate::net::Ip),
    /// Read the registers of the network card, into
    /// `System::net_registers`.
    NetRegisters,
    /// Reset the network card and set it up again. The answer comes in
    /// `System::net_restarted`.
    NetRestart,
    /// Tell the brightness of the screen, after setting it if one is
    /// given, in hundredths. The answer comes in `System::lit`.
    Light(Option<u32>),
    /// Read what the graphics chip is doing, to say it or to list its
    /// registers. The answer comes in `System::chip`.
    Chip { registers: bool },
    /// Put XilaOS on this disk, numbered from 1: on the whole disk, or
    /// only the system in place of the one it holds. The answer comes in
    /// `System::installed`.
    Install { disk: usize, whole: bool },
    /// Fetch the system from another machine of the network, to install
    /// it: answered in `System::fetched`.
    Fetch(crate::net::Ip),
    /// Ask the machine `System::asked_machine` for the page at
    /// `System::asked_path`, on a port: answered in `System::got`. `to`
    /// is the address of the machine, or none when it was written as a
    /// name whose address is to be found first; `secret` says whether to
    /// ask through TLS (`https://`); with `post`, `System::asked_text`
    /// is sent to the page instead of only asking for it.
    Ask { to: Option<crate::net::Ip>, port: u16, secret: bool, post: bool },
    /// Find the address of the machine named `System::asked_machine`:
    /// answered in `System::named`.
    Name,
}

/// The state of the system that commands can read and change.
pub struct System {
    pub theme: usize,
    pub layout: Layout,
    pub mood: MoodMode,
    pub energy: u16,
    /// The screen shown: an index into `SCREENS`.
    pub screen: usize,
    /// Timer ticks since start, 100 per second.
    pub ticks: u64,
    pub machine: Machine,
    /// Set by `reboot`; the kernel restarts the machine when it sees it.
    pub reboot: bool,
    /// The tasks, copied from the kernel before each command.
    pub tasks: [TaskInfo; MAX_TASKS],
    /// Share of the processor each task had lately, in thousandths.
    pub cpu_share: [u16; MAX_TASKS],
    cpu_seen: [u64; MAX_TASKS],
    sampled_at: u64,
    /// Bytes that went through the serial port since start, and lately.
    pub serial_bytes: u64,
    serial_seen: u64,
    pub serial_rate: u16,
    /// Rows in the boot log and in the terminal.
    pub log_rows: usize,
    pub term_rows: usize,
    /// The stored tables.
    pub db: xila_db::Database,
    /// Tick of the last change that was written to them, 0 for never.
    pub stored_at: u64,
    /// A disk that was found for the database and refused, because it holds
    /// something else or a damaged database. Nothing was written on it.
    /// `disk erase yes` starts an empty database there.
    pub spare: Option<xila_db::Volume>,
    /// Who listens to what, and the events that wait for them.
    pub events: Events,
    /// What each session keeps under a name (`let`), or what the program
    /// that runs for it does while one runs: read through `names`.
    /// The last ones are those of the programs that run by themselves.
    pub vars: [crate::expr::Vars; crate::events::SESSIONS + 1],
    /// The programs that run by themselves, and when.
    pub triggers: crate::lang::Triggers,
    /// The lines each session typed since it opened a block.
    pub blocks: [crate::lang::Block; crate::events::SESSIONS],
    /// Who runs the command: `events::SCREEN`, `events::SERIAL` or
    /// `events::NETWORK`.
    pub session: usize,
    /// Tick of the last query on each view of the catalog, 0 for never.
    pub queried: [u64; crate::catalog::TABLES.len()],
    /// Left by a command for the kernel.
    pub request: Option<Request>,
    /// The virtual machine, which runs the language: its units of code,
    /// where each is, its cursors. See `machine.rs`. (`machine` is the
    /// computer.)
    pub vm: crate::machine::Machine,
    /// The rooms the kernel gave for jobs: a machine each. None on a system
    /// that was given none, where everything runs at once in `vm`.
    pub rooms: Option<&'static mut [crate::machine::Machine]>,
    /// The jobs: what runs in those rooms. See `jobs.rs`.
    pub jobs: crate::jobs::Jobs,
    /// What jobs asked of the network and wait for (`waits.rs`).
    pub waits: crate::waits::Waits,
    pub display: Display,
    /// The kernel's answer to `Request::Measure`.
    pub measured: Option<Measure>,
    /// What the command given to `qr` said so far, until `served` makes
    /// the code of it.
    pub note: Note,
    pub noting: bool,
    /// The code on the screen, in place of everything below the title
    /// bar, until a key is pressed.
    pub code: Option<crate::qr::Code>,
    /// How many codes were made since the start: a code may take the
    /// place of another one that is still on the screen.
    pub codes: u64,
    /// The graphics chip the kernel found on the PCI bus, if any.
    pub card: Option<crate::intel::Card>,
    /// The kernel's answer to `Request::Chip`.
    pub chip: Option<(crate::intel::State, bool)>,
    /// What the clock of the machine said, in seconds since the year 2000,
    /// and the tick at which it was read. None if it tells no time.
    pub clock: Option<(u64, u64)>,
    /// The settings as they were last written to the database
    /// (`settings::keep`); None when its tables were just given a disk.
    pub kept: Option<crate::settings::Kept>,
    /// The kernel's answer to `Request::Light`: the brightness in
    /// hundredths, or nothing if no chip XilaOS knows sets it.
    pub lit: Option<Option<u32>>,
    /// Pictures of what moves by itself since the start, when the screen
    /// gives the rhythm and not the timer: the kernel counts them.
    pub picture: Option<u64>,
    /// The kernel's answer to `Request::Flip`: how pictures get to the
    /// screen now, or why nothing was changed.
    pub flipped: Option<Result<Pictures, crate::intel::NoFlip>>,
    /// The prompt for the network, closed until `remote on`; and a number
    /// nobody else can guess, which the kernel keeps stirring, from which
    /// its code is drawn.
    pub remote: crate::remote::Remote,
    pub chance: u64,
    /// The tables as pages for browsers of the network, closed until `web on`.
    pub web: crate::web::Web,
    /// The network, and the card it goes through, if the kernel drives one.
    pub net: crate::net::Net,
    pub net_card: Option<NetCard>,
    /// The kernel's answer to `Request::NetRestart`: whether the card came back.
    pub net_restarted: Option<bool>,
    /// The kernel's answer to `Request::NetRegisters`: name, address, value.
    pub net_registers: Option<[(&'static str, u32, u32); NET_REGISTERS]>,
    /// The kernel's answer to `Request::Ping`: the address, and how many
    /// microseconds its answer took, if it came.
    pub pinged: Option<(crate::net::Ip, Option<u32>)>,
    /// What the kernel found on the PCI bus, for `devices`.
    pub devices: [Option<Device>; MAX_DEVICES],
    /// The disks the kernel found, in the order `install` numbers them.
    pub disks: [Option<DiskInfo>; MAX_DISKS],
    /// Bytes of the copy of itself the kernel keeps to install it; 0 if
    /// the firmware did not give it one.
    pub system_bytes: usize,
    /// The kernel's answer to `Request::Install`.
    pub installed: Option<Result<Installed, InstallError>>,
    /// The kernel's answer to `Request::Fetch`: who was asked, and the
    /// size of what came with its check number.
    pub fetched: Option<(crate::net::Ip, Result<(usize, u32), crate::tftp::Failed>)>,
    /// What `net get` asks for, of which machine (as it was written, an
    /// address or a name), and the kernel's answer: the port, then how
    /// it went and the start of what was said.
    /// The companion: what it is asked, and how far its answer is.
    pub companion: crate::companion::Companion,
    /// The program that runs by itself now, if one does: who asks, when a
    /// program asks the companion.
    pub firing: Name,
    /// The companion is running a line by itself: some commands refuse.
    pub acting: bool,
    /// What was last seen of the state of the system, to say what changes.
    pub watch: crate::watch::Watch,
    /// Pages of the tables found damaged since the start. Counted where
    /// tables are read, which only looks at the system: hence the cell.
    pub damaged: core::cell::Cell<u64>,
    pub asked_machine: crate::dns::Name,
    pub asked_path: FixedStr<200>,
    pub asked_text: Text,
    pub got: Option<(u16, Result<Got, crate::ask::Failed>)>,
    /// The kernel's answer to `Request::Name`: the address of
    /// `asked_machine`.
    pub named: Option<Result<crate::net::Ip, crate::dns::Failed>>,
    /// Commands of the last numbered table each session was shown.
    pub last: [[Text; TABLE_ROWS]; crate::events::SESSIONS],
    pub last_len: [usize; crate::events::SESSIONS],
    /// The characters on the screen at the last redraw, for `peek`.
    pub peek: [[u8; COLS]; MAX_ROWS],
    pub peek_rows: usize,
}

impl System {
    /// The names in use: those of the session, which are those of the
    /// program that runs for it while one does, or those of the job in
    /// its turn when it has names of its own. Whatever reads or gives a
    /// name goes through here.
    pub fn names(&self) -> &crate::expr::Vars {
        crate::jobs::names(&self.vars, &self.jobs, self.session)
    }

    pub fn names_mut(&mut self) -> &mut crate::expr::Vars {
        crate::jobs::names_mut(&mut self.vars, &mut self.jobs, self.session)
    }

    /// The time by the clock of the machine, in seconds since the year
    /// 2000: what it said when it was last read, and what the timer has
    /// counted since. None if that clock tells no time.
    pub fn now(&self) -> Option<u64> {
        self.clock.map(|(seconds, at)| seconds + self.ticks.saturating_sub(at) / 100)
    }

    pub const fn new() -> System {
        System {
            theme: 0,
            layout: Layout::Us,
            mood: MoodMode::Auto,
            energy: 0,
            screen: 0,
            ticks: 0,
            machine: Machine {
                cpu: FixedStr::new(),
                firmware: FixedStr::new(),
                screen_width: 0,
                screen_height: 0,
                scale: 1,
                serial: false,
                mem_usable_kb: 0,
                mem_firmware_kb: 0,
                mem_managed_kb: 0,
                mem_free_kb: 0,
                heap_used: 0,
                mem_regions: 0,
                disk: FixedStr::new(),
                graphics: FixedStr::new(),
            },
            reboot: false,
            tasks: [TaskInfo::NONE; MAX_TASKS],
            cpu_share: [0; MAX_TASKS],
            cpu_seen: [0; MAX_TASKS],
            sampled_at: 0,
            serial_bytes: 0,
            serial_seen: 0,
            serial_rate: 0,
            log_rows: 0,
            term_rows: 0,
            db: xila_db::Database::new(),
            stored_at: 0,
            spare: None,
            events: Events::new(),
            vars: [crate::expr::Vars::new(), crate::expr::Vars::new(), crate::expr::Vars::new(), crate::expr::Vars::new()],
            triggers: crate::lang::Triggers::new(),
            blocks: [crate::lang::Block::new(), crate::lang::Block::new(), crate::lang::Block::new()],
            session: crate::events::SCREEN,
            queried: [0; crate::catalog::TABLES.len()],
            request: None,
            vm: crate::machine::Machine::new(),
            rooms: None,
            jobs: crate::jobs::Jobs::new(),
            waits: crate::waits::Waits::new(),
            display: Display { shown: Pictures::Direct, every: 0, sweeps: 0, light: None, direct: false, combining: false, whole_us: 0, firmware_us: 0, pictures: 0, longest_us: 0, city_us: 0, skipped: 0 },
            measured: None,
            note: Note::new(),
            noting: false,
            code: None,
            codes: 0,
            card: None,
            chip: None,
            clock: None,
            kept: None,
            lit: None,
            picture: None,
            flipped: None,
            remote: crate::remote::Remote::new(),
            chance: 0,
            web: crate::web::Web::new(),
            net: crate::net::Net::new(),
            net_card: None,
            net_restarted: None,
            net_registers: None,
            pinged: None,
            devices: [None; MAX_DEVICES],
            disks: [None; MAX_DISKS],
            system_bytes: 0,
            installed: None,
            fetched: None,
            companion: crate::companion::Companion::new(),
            firing: Name::new(),
            acting: false,
            watch: crate::watch::Watch::new(),
            damaged: core::cell::Cell::new(0),
            asked_machine: FixedStr::new(),
            asked_path: FixedStr::new(),
            asked_text: Text::new(),
            got: None,
            named: None,
            last: [[Text::new(); TABLE_ROWS]; crate::events::SESSIONS],
            last_len: [0; crate::events::SESSIONS],
            peek: [[b' '; COLS]; MAX_ROWS],
            peek_rows: 0,
        }
    }

    /// Raises the energy of the mood: typing and commands do this.
    pub fn nudge(&mut self, amount: u16) {
        if self.mood == MoodMode::Auto {
            self.energy = (self.energy + amount).min(ENERGY_MAX);
        }
    }

    /// Lets time pass: the energy drifts to where the mood wants it. Left to
    /// itself it falls back to zero in five seconds; a mood that was chosen is
    /// reached in one.
    pub fn settle(&mut self, ticks: u64) {
        let (target, speed) = match self.mood {
            MoodMode::Calm => (100, 10),
            MoodMode::Auto => (0, 2),
            MoodMode::Intense => (900, 10),
        };
        let step = (ticks * speed).min(ENERGY_MAX as u64) as u16;
        if self.energy > target {
            self.energy = self.energy.saturating_sub(step).max(target);
        } else {
            self.energy = (self.energy + step).min(target);
        }
    }

    /// Works out, twice per second, what each task and the serial port did
    /// since the last time. `tasks` must be fresh.
    pub fn sample(&mut self) {
        let window = self.ticks - self.sampled_at;
        if window < 50 {
            return;
        }
        for id in 0..MAX_TASKS {
            let ran = self.tasks[id].ticks;
            // a smaller count means the slot went to a new task
            let delta = ran.saturating_sub(self.cpu_seen[id]);
            self.cpu_share[id] = if self.tasks[id].used { (delta * 1000 / window).min(1000) as u16 } else { 0 };
            self.cpu_seen[id] = ran;
        }
        self.serial_rate = (self.serial_bytes - self.serial_seen).min(u16::MAX as u64) as u16;
        self.serial_seen = self.serial_bytes;
        self.sampled_at = self.ticks;
    }

    /// The mood as a gauge of ten characters.
    pub fn meter(&self) -> [u8; 12] {
        let mut m = *b"[..........]";
        let filled = ((self.energy as usize + 50) / 100).min(10);
        for c in m.iter_mut().skip(1).take(filled) {
            *c = b'#';
        }
        m
    }
}

/// Names of the commands, for completion.
pub const NAMES: [&[u8]; 59] = [
    b"about", b"alter", b"ask", b"cancel", b"city", b"clear", b"companion", b"create", b"delete", b"devices", b"disk", b"display", b"drop", b"else", b"end", b"every", b"for", b"help",
    b"if", b"insert", b"install", b"job", b"jobs", b"key", b"keymap", b"let", b"listen", b"mem", b"mood", b"net", b"notify", b"on", b"panel", b"panels", b"peek", b"program", b"programs", b"qr", b"reboot",
    b"remote", b"return",
    b"run", b"say", b"screen", b"select", b"show", b"spin", b"stop", b"submit", b"tables", b"tasks", b"theme", b"time", b"unlisten", b"update",
    b"uptime", b"vars", b"web", b"while",
];

/// The commands that are not statements of the language: what `command` runs.
const LANGUAGE: [&[u8]; 14] =
    [b"cancel", b"else", b"end", b"every", b"for", b"if", b"let", b"on", b"panel", b"program", b"return", b"run", b"say", b"while"];

/// Whether a word, in small letters, is a command of the prompt. The
/// statements of the language are not: `lang` takes them first.
pub fn knows(word: &[u8]) -> bool {
    (NAMES.contains(&word) && !LANGUAGE.contains(&word)) || matches!(word, b"?" | b"cls")
}

/// What a session is asked to type: the prompt, or its short form while a
/// block is open.
pub fn prompt(sys: &System, session: usize) -> &'static [u8] {
    if sys.blocks[session].open() {
        CONTINUE
    } else {
        PROMPT
    }
}

/// The prompt while a block is open: as wide as the other.
pub const CONTINUE: &[u8] = b"          ..>";

const HELP_COMMANDS: [&[u8]; 38] = [
    b"XilaOS commands:",
    b"   tables                     - Lists the tables: the system's and yours.",
    b"   select ...                 - Reads a table: select name from tasks",
    b"   create table ...           - Makes a table: create table t (a int, b text)",
    b"   alter table ...            - Changes a table: alter table t add c int",
    b"   insert into ...            - Adds rows: insert into t values (1, 'x')",
    b"   update ... set ...         - Changes rows: update t set b = 'y'",
    b"   delete from ...            - Removes rows: delete from t where a = 1",
    b"   drop table ...             - Removes a table and its rows.",
    b"   listen [channel]           - Tells you when a table changes, or on notify.",
    b"   unlisten [channel]         - Stops that; every channel if none is named.",
    b"   notify channel ['text']    - Sends an event to those who listen.",
    b"   help [c|k|l]               - This help: commands, keys, or the language.",
    b"   about                      - What this system is and what it runs on.",
    b"   mem                        - Memory, as reported by the firmware.",
    b"   devices                    - The parts of the machine.",
    b"   disk [check|erase]         - Where the tables are; reads all; takes a disk.",
    b"   install [n|system|from a]  - Puts XilaOS on a disk; from: fetches it first.",
    b"   net [ping|get|post|name]   - The network: card, cable, address. Pings; asks.",
    b"   remote [on|off]            - Opens this prompt to the local network.",
    b"   web [on|off]               - Shows the tables to browsers of that network.",
    b"   screen [n]                 - Lists the screens, or shows screen n.",
    b"   display [test|chip]        - How fast the screen is drawn; chip: by what.",
    b"   qr command                 - What a command says, as a code a phone reads.",
    b"   mood [calm|auto|intense]   - Sets the mood. Auto follows your typing.",
    b"   theme [day|night]          - Changes the colors.",
    b"   keymap [us|fr]             - Changes the keyboard layout.",
    b"   key [set n k|forget n]     - Keys to services, never shown.",
    b"   ask question               - Asks the companion.",
    b"   companion [off|on]         - Who the companion is. Stops it.",
    b"   time                       - The date and the time.",
    b"   uptime                     - Time since the system started.",
    b"   city                       - The blocks of the city, as a table.",
    b"   tasks                      - Lists the tasks and the time each one ran.",
    b"   spin                       - Starts a task that never rests (a test).",
    b"   stop [n]                   - Stops task n.",
    b"   clear                      - Empties the terminal.",
    b"   reboot                     - Restarts the machine.",
];

const HELP_LANGUAGE: [&[u8]; 31] = [
    b"XilaScript: one statement per line, at the prompt or in a program.",
    b"   let name = value           - Keeps a value: let total = select sum(n) from t",
    b"   say value                  - Shows a value: say 'Total: ' + total",
    b"   if condition ... end       - Runs lines when it holds. Also: else, else if.",
    b"   while condition ... end    - Runs lines again and again while it holds.",
    b"   for row in select ... end  - Runs lines for each row; row.name is a column.",
    b"   else                       - In an if: the lines for when it does not hold.",
    b"   end                        - Closes if, while, for or program.",
    b"   program name ... end       - Keeps lines under a name, in the database.",
    b"   program name(a, b) ... end - It takes two values, under the names a and b.",
    b"   run name(1, 'x')           - Runs a program. name(1, 'x') alone does too.",
    b"   submit name(1, 'x')        - Runs it as a job: the prompt does not wait.",
    b"   jobs                       - Lists the jobs. job 3 shows one, what it said.",
    b"   job 3 end                  - Stops a job. Also: job 3 hold, job 3 release.",
    b"   on insert into t ... end   - Runs when a row is added. Also: on update t,",
    b"                                on delete from t, on notify channel.",
    b"   every 5 minutes ... end    - Runs again and again: seconds, hours, days.",
    b"   every day at 8:00 ... end  - Runs once a day, by the clock of the machine.",
    b"   panel name on table ... end - Keeps a screen of an application, on a table:",
    b"                                title 'Stock', list a, b, field a 'Label'.",
    b"   panels                     - Lists the panels. show panel x, drop panel x.",
    b"   programs                   - Lists the programs.",
    b"   show name                  - Shows a program; show table t, a table.",
    b"   drop program name          - Removes a program.",
    b"   return                     - Leaves the program.",
    b"   vars                       - Lists the names that have a value.",
    b"   cancel                     - Drops the lines typed since if, for, program...",
    b"   today, now                 - The date, and the date with the time, as texts.",
    b"   A value is a number, a 'text', a name, a calculation or a select. Every",
    b"   command is a statement, SQL too. A line that starts with # is a note.",
    b"   A program sees its own names only: its values, and what let and for make.",
];

const HELP_KEYS: [&[u8]; 10] = [
    b"XilaOS keys:",
    b"   [Alt]+[E]             - Opens the menu of screens.",
    b"   [Alt]+[0] [Alt]+[1]   - Shows a screen.",
    b"   [Alt]+[X]             - Changes the mood.",
    b"   [Tab]                 - Completes the command.",
    b"   [Up] [Down]           - Recalls the commands already typed.",
    b"   [PgUp] [PgDn]         - Scrolls the terminal.",
    b"   [Esc]                 - Closes the menu, or goes back to the overview.",
    b"                           At the prompt, stops the program it waits for.",
    b"   [Alt]+[B]             - Sends that program to the background, as a job.",
];

/// Runs a line typed at a prompt. Returns false if it failed.
pub fn exec(sys: &mut System, line: &[u8], out: &mut dyn Out) -> bool {
    let line = trim(line);
    // `now` and `today` are the moment the line was given
    let now = sys.now();
    sys.names_mut().clock = now;
    // A program that runs by itself and the companion have no prompt, so no
    // block to collect lines in: a command of theirs that types a line (`qr`)
    // types one that is whole.
    let Some(block) = sys.blocks.get(sys.session) else {
        if crate::lang::opens(line) {
            out.line(Style::Error, b"A block is typed at a prompt, not by a program that runs by itself.");
            return false;
        }
        return line.is_empty() || crate::lang::line(sys, line, out);
    };
    // the lines of a block wait for its end
    if block.open() || crate::lang::opens(line) {
        return crate::lang::typed(sys, line, out);
    }
    if line.is_empty() {
        return true;
    }
    if let Some(n) = parse_number(line) {
        // a list belongs to the session it was shown to; a program that
        // runs by itself has none
        let listed = sys.last_len.get(sys.session).copied().unwrap_or(0);
        if n >= 1 && n <= listed {
            let command = sys.last[sys.session][n - 1];
            return command_line(sys, command.bytes(), out);
        }
        let why: &[u8] = if listed == 0 { b"There is no list to follow yet." } else { b"That line leads nowhere." };
        out.line(Style::Error, why);
        return false;
    }
    crate::lang::line(sys, line, out)
}

/// Runs a line that a list gave: a command, as if it had been typed.
fn command_line(sys: &mut System, line: &[u8], out: &mut dyn Out) -> bool {
    crate::lang::line(sys, line, out)
}

/// Runs one command of the prompt. The statements of the language (`let`,
/// `if`...) are not commands: `lang` takes them before this.
pub(crate) fn command(sys: &mut System, line: &[u8], out: &mut dyn Out) -> bool {
    let (word, arg) = split_word(line);
    let mut name = FixedStr::<16>::new();
    for &b in word {
        name.push(b.to_ascii_lowercase());
    }
    match name.bytes() {
        b"help" | b"?" => help(sys, arg, out),
        b"about" => about(sys, out),
        b"mem" => mem(sys, out),
        b"disk" => disk(sys, arg, out),
        b"install" => install(sys, arg, out),
        b"devices" => devices(sys, out),
        b"net" => net(sys, arg, out),
        b"remote" => remote(sys, arg, out),
        b"web" => web(sys, arg, out),
        b"qr" => qr(sys, arg, out),
        b"screen" => screen(sys, arg, out),
        b"display" => display(sys, arg, out),
        b"mood" => mood(sys, arg, out),
        b"theme" => theme(sys, arg, out),
        b"keymap" => keymap(sys, arg, out),
        b"key" => key(sys, arg, out),
        b"ask" => ask(sys, arg, out),
        b"companion" => companion(sys, arg, out),
        b"uptime" => uptime(sys, out),
        b"time" => time(sys, out),
        b"tasks" => tasks(sys, out),
        b"city" => city(sys, out),
        b"select" => query(sys, line, out),
        b"drop" if split_word(arg).0.eq_ignore_ascii_case(b"program") && sys.acting => {
            out.line(Style::Error, b"Programs are not the companion's to change.");
            false
        }
        b"drop" if split_word(arg).0.eq_ignore_ascii_case(b"program") => crate::lang::remove(sys, trim(split_word(arg).1), out),
        b"drop" if split_word(arg).0.eq_ignore_ascii_case(b"panel") && sys.acting => {
            out.line(Style::Error, b"Panels are not the companion's to change.");
            false
        }
        b"drop" if split_word(arg).0.eq_ignore_ascii_case(b"panel") => crate::panel::remove(sys, trim(split_word(arg).1), out),
        b"panels" => crate::panel::list(sys, out),
        b"show" if split_word(arg).0.eq_ignore_ascii_case(b"panel") && !trim(split_word(arg).1).is_empty() => crate::panel::show(sys, trim(split_word(arg).1), out),
        b"alter" | b"create" | b"drop" | b"insert" | b"update" | b"delete" => change(sys, line, out),
        b"programs" => crate::lang::list(sys, out),
        b"submit" => submit(sys, arg, out),
        b"jobs" => jobs(sys, out),
        b"job" => job(sys, arg, out),
        // a table as the statement that would make it; `show table` alone is a program of that name
        b"show" if split_word(arg).0.eq_ignore_ascii_case(b"table") && !trim(split_word(arg).1).is_empty() => crate::change::written(&sys.db, trim(split_word(arg).1), out),
        b"show" => crate::lang::show(sys, arg, out),
        b"vars" => crate::lang::vars(sys, out),
        b"listen" => listen(sys, arg, out),
        b"unlisten" => unlisten(sys, arg, out),
        b"notify" => notify(sys, arg, out),
        b"tables" => tables(sys, out),
        b"spin" => spin(sys, out),
        b"stop" => stop(sys, arg, out),
        b"peek" => peek(sys, out),
        b"clear" | b"cls" => {
            out.clear();
            true
        }
        b"reboot" if sys.acting => {
            out.line(Style::Error, b"reboot is typed at the machine itself.");
            false
        }
        b"reboot" => {
            out.line(Style::Out, b"Restarting.");
            sys.reboot = true;
            true
        }
        _ => {
            let mut msg = FixedStr::<COLS>::from_bytes(b"Unknown command: ");
            msg.push_bytes(word);
            out.line(Style::Error, msg.bytes());
            false
        }
    }
}

/// Shows a table and remembers where its numbered rows lead.
pub(crate) fn show(sys: &mut System, out: &mut dyn Out, table: &Table) {
    // (a job nobody waits for shows its lists to nobody: the numbers typed
    // at its session's prompt still follow the list that prompt was shown)
    let aside = crate::jobs::in_turn(sys).is_some_and(|job| job.watch == crate::jobs::Watch::Nobody);
    if table.numbered && sys.session < crate::events::SESSIONS && !aside {
        sys.last[sys.session] = table.follow;
        sys.last_len[sys.session] = table.rows;
    }
    out.table(table);
    if table.more > 0 {
        let mut note = FixedStr::<COLS>::new();
        let _ = write!(note, "{} more rows are not shown. Narrow the query with WHERE.", table.more);
        out.line(Style::Out, note.bytes());
    }
}

/// Runs a SELECT typed at the prompt.
fn query(sys: &mut System, line: &[u8], out: &mut dyn Out) -> bool {
    match crate::sql::query(&*sys, sys.names(), line).map(|rows| rows.table()) {
        Ok(table) => {
            // the city lights the table that was read
            let now = sys.ticks.max(1);
            if let Some(index) = crate::catalog::find(table.name.bytes()) {
                sys.queried[index] = now;
            } else if let Some(index) = sys.db.find(table.name.bytes()) {
                sys.db.table_mut(index).read_at = now;
            }
            show(sys, out, &table);
            true
        }
        Err(message) => {
            out.line(Style::Error, message.bytes());
            false
        }
    }
}

/// Runs a statement that changes stored tables.
fn change(sys: &mut System, line: &[u8], out: &mut dyn Out) -> bool {
    // the table the statement is about, and the name it would give one
    for table in [crate::change::target(line), crate::change::becomes(line)].into_iter().flatten() {
        // the companion acting by itself leaves alone what holds it
        if sys.acting && crate::companion::held_by(table) {
            let mut message = crate::sql::Message::from_bytes(b"The table ");
            message.push_bytes(table);
            message.push_bytes(b" is not the companion's to change.");
            out.line(Style::Error, message.bytes());
            return false;
        }
        // who the companion is says where its key is sent: from the network,
        // or from a program either could have written, that would be a way
        // to have the key sent elsewhere
        if !at_the_machine(sys) && table.eq_ignore_ascii_case(crate::companion::COMPANIONS) {
            out.line(Style::Error, b"The table companions is changed at the machine: it says where the key goes.");
            return false;
        }
    }
    let now = sys.ticks.max(1);
    let (events, vars) = (&mut sys.events, crate::jobs::names(&sys.vars, &sys.jobs, sys.session));
    // those who listen to the table hear of every row, once the change is certain
    let mut tell = |table: &[u8], change: xila_db::Change, id: u64| {
        let what = match change {
            xila_db::Change::Insert => crate::events::What::Insert(id),
            xila_db::Change::Update => crate::events::What::Update(id),
            xila_db::Change::Delete => crate::events::What::Delete(id),
        };
        events.changed(table, what, b"", now);
    };
    match crate::change::change_with(&mut sys.db, vars, line, &mut tell) {
        Ok(done) => {
            sys.stored_at = now;
            if let Some(event) = done.event {
                sys.events.changed(done.table.bytes(), crate::events::What::Table, event, now);
            }
            // the table of programs may have been changed by hand, and a
            // table a program waits on may be gone
            if done.event.is_some() || done.table.bytes() == b"programs" {
                sys.triggers.changed();
            }
            // the city lights the table that was written
            if let Some(index) = sys.db.find(done.table.bytes()) {
                sys.db.table_mut(index).written_at = now;
            }
            out.line(Style::Done, done.message.bytes());
            true
        }
        Err(message) => {
            out.line(Style::Error, message.bytes());
            false
        }
    }
}

/// Starts listening to a channel, or lists the channels listened to.
/// A program that runs by itself has no prompt to hear at.
fn at_a_prompt(sys: &System, out: &mut dyn Out) -> bool {
    if sys.session >= crate::events::SESSIONS {
        out.line(Style::Error, b"Only a prompt listens. A program waits for events with: on notify ...");
        return false;
    }
    true
}

fn listen(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    if !at_a_prompt(sys, out) {
        return false;
    }
    if arg.is_empty() {
        let mut t = Table::new(b"listen", &[b"channel"]);
        for channel in sys.events.channels(sys.session) {
            t.row(&[channel.bytes()]);
        }
        if t.rows == 0 {
            out.line(Style::Out, b"You listen to nothing. Try: listen name_of_a_table");
        } else {
            show(sys, out, &t);
        }
        return true;
    }
    if !xila_db::good_name(arg) {
        out.line(Style::Error, b"A channel is a letter, then letters, digits or _; 24 at most.");
        return false;
    }
    let (text, ok): (&[u8], bool) = match sys.events.listen(sys.session, arg) {
        Listen::Started => (b"Listening to ", true),
        Listen::Already => (b"Already listening to ", true),
        Listen::Full => (b"Too many channels are listened to. Cannot add ", false),
    };
    let message = FixedStr::<COLS>::from_bytes(text).also(arg).also(b".");
    out.line(if ok { Style::Done } else { Style::Error }, message.bytes());
    ok
}

fn unlisten(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    if !at_a_prompt(sys, out) {
        return false;
    }
    let all = arg.is_empty() || arg == b"*";
    let stopped = sys.events.unlisten(sys.session, if all { None } else { Some(arg) });
    if all {
        out.line(Style::Done, b"Listening to nothing now.");
    } else if stopped == 0 {
        out.line(Style::Error, FixedStr::<COLS>::from_bytes(b"You were not listening to ").also(arg).also(b".").bytes());
        return false;
    } else {
        out.line(Style::Done, FixedStr::<COLS>::from_bytes(b"No longer listening to ").also(arg).also(b".").bytes());
    }
    true
}

/// Sends an event: `notify channel`, `notify channel 'text'` or, as in
/// PostgreSQL, `notify channel, 'text'`.
fn notify(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    let (channel, rest) = split_word(arg);
    let (channel, rest) = match channel.iter().position(|&c| c == b',') {
        Some(comma) => (&channel[..comma], if comma + 1 < channel.len() { &arg[comma + 1..] } else { rest }),
        None => (channel, rest),
    };
    let rest = trim(rest.strip_prefix(b",").unwrap_or(rest));
    // the text is a value: a 'text' between quotes, or a calculation
    let text = if rest.is_empty() {
        Ok(Text::new())
    } else {
        let mut lexer = crate::sql::Lexer::new(rest);
        let names = crate::expr::Names::without(sys.names(), b"Put the text between quotes, or give a name that has a value, not: ");
        crate::expr::parse(&mut lexer, names).and_then(|text| lexer.end().map(|_| text)).and_then(|text| text.eval(&[crate::sql::Value::Null; TABLE_COLS]))
            .map(|value| value.shown())
    };
    let text = match text {
        Ok(text) => text,
        Err(message) => {
            out.line(Style::Error, message.bytes());
            return false;
        }
    };
    let text = text.bytes();
    if !xila_db::good_name(channel) {
        out.line(Style::Error, b"Give a channel (letters, digits, _), then a text: notify alerts 'the disk is full'");
        return false;
    }
    let reached = sys.events.notify(channel, text, sys.ticks);
    let mut message = FixedStr::<COLS>::new();
    match reached {
        0 => {
            let _ = write!(message, "Nobody listens to {}.", FixedStr::<24>::from_bytes(channel));
        }
        _ => {
            let _ = write!(message, "Sent to {reached} listener{}.", if reached == 1 { "" } else { "s" });
        }
    }
    out.line(Style::Done, message.bytes());
    true
}

/// Lists the tables; typing the number of a row shows the table.
fn tables(sys: &mut System, out: &mut dyn Out) -> bool {
    let Ok(listed) = crate::sql::select(&*sys, b"select name, kind, rows, about from tables") else {
        return false;
    };
    let mut t = Table::new(b"tables", &[b"name", b"kind", b"rows", b"about"]);
    for r in 0..listed.rows {
        let cells = &listed.cells[r];
        let mut command = Text::from_bytes(b"select * from ");
        command.push_bytes(cells[0].bytes());
        t.row_to(&[cells[0].bytes(), cells[1].bytes(), cells[2].bytes(), cells[3].bytes()], command.bytes());
    }
    show(sys, out, &t);
    true
}

fn lines(out: &mut dyn Out, text: &[&[u8]]) {
    for l in text {
        out.line(Style::Out, l);
    }
}

fn help(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    match arg {
        b"c" | b"C" => lines(out, &HELP_COMMANDS),
        b"k" | b"K" => lines(out, &HELP_KEYS),
        b"l" | b"L" => lines(out, &HELP_LANGUAGE),
        _ => {
            let mut t = Table::new(b"help", &[b"topic", b"shows"]);
            t.row_to(&[b"help c", b"the commands"], b"help c");
            t.row_to(&[b"help l", b"XilaScript, the language"], b"help l");
            t.row_to(&[b"help k", b"the keys"], b"help k");
            show(sys, out, &t);
        }
    }
    true
}

fn about(sys: &mut System, out: &mut dyn Out) -> bool {
    let m = &sys.machine;
    let mut t = Table::new(b"about", &[b"item", b"value"]);
    let mut v = Text::from_bytes(b"XilaOS ");
    v.push_bytes(crate::VERSION);
    t.row(&[b"system", v.bytes()]);
    t.row(&[b"processor", m.cpu.bytes()]);
    t.row(&[b"firmware", m.firmware.bytes()]);
    let mut v = Text::new();
    let _ = write!(v, "{}x{}, cells of {} pixels", m.screen_width, m.screen_height, 8 * m.scale);
    t.row(&[b"screen", v.bytes()]);
    t.row(&[b"keyboard", sys.layout.name()]);
    t.row(&[b"serial port", if m.serial { b"yes" } else { b"no" }]);
    t.row(&[b"disk", m.disk.bytes()]);
    t.row(&[b"stored tables", crate::catalog::kept(sys).bytes()]);
    show(sys, out, &t);
    true
}

fn megabytes(kb: u64) -> Text {
    let mut v = Text::new();
    let _ = write!(v, "{} MB", (kb + 512) / 1024);
    v
}

fn mem(sys: &mut System, out: &mut dyn Out) -> bool {
    let m = &sys.machine;
    let mut t = Table::new(b"mem", &[b"kind", b"size"]);
    t.row(&[b"usable", megabytes(m.mem_usable_kb).bytes()]);
    t.row(&[b"kept by the firmware", megabytes(m.mem_firmware_kb).bytes()]);
    t.row(&[b"managed by the kernel", megabytes(m.mem_managed_kb).bytes()]);
    let mut v = Text::new();
    let _ = write!(v, "{} KB", m.mem_free_kb);
    t.row(&[b"free", v.bytes()]);
    let mut v = Text::new();
    let _ = write!(v, "{} bytes", m.heap_used);
    t.row(&[b"kernel heap in use", v.bytes()]);
    let mut v = Text::new();
    let _ = write!(v, "{}", m.mem_regions);
    t.row(&[b"regions in the map", v.bytes()]);
    show(sys, out, &t);
    true
}

/// Shows where the stored tables are kept. `disk erase yes` gives XilaOS a
/// disk it found and refused.
fn disk(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    let mut words = FixedStr::<16>::new();
    for word in arg.split(|b| *b == b' ').filter(|word| !word.is_empty()) {
        if !words.is_empty() {
            words.push(b' ');
        }
        for &b in word {
            words.push(b.to_ascii_lowercase());
        }
    }
    match words.bytes() {
        b"" => {
            let storage = sys.db.storage();
            let mut t = Table::new(b"disk", &[b"item", b"value"]);
            t.row(&[b"disk", sys.machine.disk.bytes()]);
            t.row(&[b"stored tables", crate::catalog::kept(sys).bytes()]);
            t.row(&[b"room", megabytes(storage.pages * 4).bytes()]);
            let mut used = Text::new();
            let _ = write!(used, "{} KB", storage.used * 4);
            t.row(&[b"used", used.bytes()]);
            // what reading and changing the tables cost since the start
            let mut looked = Text::new();
            let _ = write!(looked, "{}", storage.looked);
            t.row(&[b"pages read", looked.bytes()]);
            show(sys, out, &t);
            if sys.spare.is_some() {
                out.line(Style::Out, b"The disk was not used. To give it to XilaOS, type: disk erase");
            }
            true
        }
        b"check" => {
            // every page of every tree is read, and its checksum with it
            let storage = sys.db.storage();
            if storage.used == 0 {
                out.line(Style::Out, b"There are no stored tables yet: nothing to read.");
                return true;
            }
            match sys.db.verify() {
                Ok(()) => {
                    let mut line = FixedStr::<COLS>::new();
                    let _ = write!(line, "The tables were read whole: {} pages ({} KB), every one is right.", storage.used, storage.used * 4);
                    out.line(Style::Out, line.bytes());
                    true
                }
                Err(error) => {
                    if matches!(error, xila_db::Error::Damaged(_)) {
                        sys.damaged.set(sys.damaged.get() + 1);
                    }
                    out.line(Style::Error, crate::change::engine_error(error, None).bytes());
                    out.line(Style::Error, b"The check stopped there. Nothing was changed.");
                    false
                }
            }
        }
        // both forms: the one that tells what would be lost, and the one that does it
        b"erase" | b"erase yes" if !at_the_machine(sys) => {
            out.line(Style::Error, b"disk erase takes a disk: it is typed at the machine itself.");
            false
        }
        b"erase" => {
            let Some(spare) = &sys.spare else {
                out.line(Style::Error, b"There is no disk to erase.");
                return false;
            };
            let mut what = FixedStr::<COLS>::new();
            let _ = write!(what, "This erases the {} that were found on the disk and not used,", megabytes(spare.pages() * 4));
            out.line(Style::Out, what.bytes());
            out.line(Style::Out, b"and starts an empty database there. What the disk holds is lost.");
            if !sys.db.tables().is_empty() {
                out.line(Style::Out, b"The tables made since the start are in memory: they are lost too.");
            }
            out.line(Style::Out, b"To do it, type: disk erase yes");
            true
        }
        b"erase yes" => {
            let Some(spare) = sys.spare.take() else {
                out.line(Style::Error, b"There is no disk to erase.");
                return false;
            };
            let name = spare.name();
            match xila_db::Database::format(spare) {
                Ok(db) => {
                    sys.machine.disk = Text::new();
                    let _ = write!(sys.machine.disk, "{name}, {}", megabytes(db.storage().pages * 4));
                    sys.db = db;
                    sys.triggers.changed();
                    sys.stored_at = sys.ticks.max(1);
                    // the settings in use go with the tables to their new place
                    sys.kept = None;
                    out.line(Style::Out, b"The disk is erased. The tables are now kept on it.");
                    true
                }
                Err((error, spare)) => {
                    sys.spare = Some(spare);
                    out.line(Style::Error, crate::change::engine_error(error, None).bytes());
                    false
                }
            }
        }
        _ => {
            out.line(Style::Error, b"Usage: disk, disk check, or disk erase");
            false
        }
    }
}

/// The size of a disk or of a part of one, as people say it.
fn size(sectors: u64) -> Text {
    let mut v = Text::new();
    let _ = if sectors < 10 << 21 { write!(v, "{} MB", sectors >> 11) } else { write!(v, "{} GB", sectors >> 21) };
    v
}

/// What a disk holds, in a few words.
fn holds(sys: &System, disk: &DiskInfo) -> Text {
    use crate::gpt::Holds;
    let mut v = Text::new();
    let _ = match disk.holds {
        Holds::Unreadable => write!(v, "does not answer"),
        Holds::Nothing => write!(v, "nothing"),
        Holds::OldTable => write!(v, "an old kind of partition table"),
        Holds::Unknown => write!(v, "something, without partitions"),
        Holds::Damaged => write!(v, "a damaged partition table"),
        Holds::Partitions { count: 0, .. } => write!(v, "no partition"),
        Holds::Partitions { count: 1, system: None } => write!(v, "1 partition"),
        Holds::Partitions { count, system: None } => write!(v, "{count} partitions"),
        Holds::Partitions { count, system: Some(_) } if count <= 2 => write!(v, "XilaOS"),
        Holds::Partitions { count: 3, system: Some(_) } => write!(v, "XilaOS and 1 other partition"),
        Holds::Partitions { count, system: Some(_) } => write!(v, "XilaOS and {} other partitions", count - 2),
    };
    if disk.taken && sys.spare.is_none() && sys.db.storage().keeps {
        v.push_bytes(b", in use");
    }
    v
}

/// The first words of what is said of a disk: "Disk 1, QEMU HARDDISK, 192 MB".
fn disk_name(number: usize, disk: &DiskInfo) -> FixedStr<COLS> {
    let mut v = FixedStr::<COLS>::new();
    let _ = write!(v, "Disk {number}, ");
    v.push_bytes(disk.model.bytes());
    v.also(b", ").also(size(disk.sectors).bytes())
}

/// What to do when the firmware gave the system no file of its own, as
/// after a start from the network.
const NO_SYSTEM: &[u8] = b"A machine that serves the system can give it: install from and its address.";

/// Puts XilaOS on a disk, so that the machine starts from it. Nothing is
/// written before `install n yes`, or `install system` which loses nothing.
/// `install from 192.168.1.20` fetches the system from a machine that
/// serves it (`tools/netboot.py`), in place of the one the firmware gave.
fn install(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    use crate::gpt::Holds;
    if !at_the_machine(sys) {
        out.line(Style::Error, b"install takes a disk: it is typed at the machine itself.");
        return false;
    }
    let mut words = [&b""[..]; 3];
    let mut count = 0;
    for word in arg.split(|b| *b == b' ').filter(|word| !word.is_empty()) {
        if count < words.len() {
            words[count] = word;
        }
        count += 1;
    }
    if words[0].eq_ignore_ascii_case(b"from") {
        // the system itself, from a machine that serves it: nothing is written yet
        let Some(server) = (count == 2).then(|| crate::net::ip_of(words[1])).flatten() else {
            out.line(Style::Error, b"Type install from and the address of the machine that serves the system.");
            return false;
        };
        if sys.net_card.is_none() {
            out.line(Style::Error, b"XilaOS drives no network card of this machine. Type devices to see what it has.");
            return false;
        }
        if sys.net.ip.is_none() {
            out.line(Style::Error, b"This machine has no address on the network yet. Type net to see why.");
            return false;
        }
        sys.request = Some(Request::Fetch(server));
        return true;
    }
    let system = words[0].eq_ignore_ascii_case(b"system");
    let sure = count == 2 && words[1].eq_ignore_ascii_case(b"yes");
    if count > 2 || (count == 2 && !system && !sure) {
        out.line(Style::Error, b"Type install to list the disks, install 1 to take disk 1, or install system.");
        return false;
    }
    if count == 0 {
        let mut t = Table::new(b"install", &[b"disk", b"model", b"size", b"holds"]);
        for (i, disk) in sys.disks.iter().flatten().enumerate() {
            let mut number = Text::new();
            let _ = write!(number, "{}", i + 1);
            let model = FixedStr::<24>::from_bytes(disk.model.bytes());
            t.row(&[number.bytes(), model.bytes(), size(disk.sectors).bytes(), holds(sys, disk).bytes()]);
        }
        if t.rows == 0 {
            out.line(Style::Out, b"No disk was found. XilaOS installs on SATA disks only, for now.");
            return true;
        }
        show(sys, out, &t);
        if sys.system_bytes == 0 {
            out.line(Style::Out, b"This XilaOS cannot install: the firmware did not give it its own file.");
            out.line(Style::Out, NO_SYSTEM);
            return true;
        }
        out.line(Style::Out, b"To put XilaOS on a disk, type install and its number. Nothing is written yet.");
        if sys.disks.iter().flatten().any(|disk| matches!(disk.holds, Holds::Partitions { system: Some(_), .. })) {
            out.line(Style::Out, b"To replace the system only, and keep the tables: install system");
        }
        return true;
    }

    // which disk: its number, or for the system alone the one that has XilaOS
    let installed = |disk: &DiskInfo| matches!(disk.holds, Holds::Partitions { system: Some(_), .. });
    let number = if !system || count == 2 {
        let Some(number) = parse_number(words[if system { 1 } else { 0 }]) else {
            out.line(Style::Error, b"Type install to list the disks, install 1 to take disk 1, or install system.");
            return false;
        };
        number
    } else {
        let mut with = sys.disks.iter().enumerate().filter(|(_, disk)| disk.as_ref().is_some_and(installed));
        match (with.next(), with.next()) {
            (Some((i, _)), None) => i + 1,
            (None, _) => {
                out.line(Style::Error, b"XilaOS is not installed on any disk. Type install to list the disks.");
                return false;
            }
            _ => {
                out.line(Style::Error, b"XilaOS is on several disks. Type install system and the number of one.");
                return false;
            }
        }
    };
    let Some(disk) = number.checked_sub(1).and_then(|i| sys.disks.get(i).copied().flatten()) else {
        let mut line = FixedStr::<COLS>::new();
        let _ = write!(line, "There is no disk {number}. Type install to list the disks.");
        out.line(Style::Error, line.bytes());
        return false;
    };
    if sys.system_bytes == 0 {
        out.line(Style::Error, b"This XilaOS cannot install: the firmware did not give it its own file.");
        out.line(Style::Out, NO_SYSTEM);
        return false;
    }

    if system {
        if !installed(&disk) {
            let mut line = FixedStr::<COLS>::new();
            let _ = write!(line, "XilaOS is not on disk {number}: there is no system to replace. See: install {number}");
            out.line(Style::Error, line.bytes());
            return false;
        }
        sys.request = Some(Request::Install { disk: number, whole: false });
        return true;
    }
    let Some(layout) = crate::gpt::Layout::new(disk.sectors, 0) else {
        let mut line = FixedStr::<COLS>::new();
        let _ = write!(line, "Disk {number} is too small: XilaOS needs 118 MB at least.");
        out.line(Style::Error, line.bytes());
        return false;
    };
    if disk.holds == Holds::Unreadable {
        let mut line = FixedStr::<COLS>::new();
        let _ = write!(line, "Disk {number} does not answer.");
        out.line(Style::Error, line.bytes());
        return false;
    }
    if sure {
        sys.request = Some(Request::Install { disk: number, whole: true });
        return true;
    }
    out.line(Style::Out, disk_name(number, &disk).also(b", holds: ").also(holds(sys, &disk).bytes()).also(b".").bytes());
    out.line(Style::Out, b"Installing takes the whole disk: all it holds is lost.");
    let mut line = FixedStr::<COLS>::new();
    let _ = write!(line, "XilaOS gets {} to start from, and ", size(layout.start.sectors));
    out.line(Style::Out, line.also(size(layout.data.sectors).bytes()).also(b" for the tables.").bytes());
    if !sys.db.storage().keeps && !sys.db.tables().is_empty() {
        out.line(Style::Out, b"The tables made since the start are in memory: they are lost too.");
    }
    let mut line = FixedStr::<COLS>::new();
    let _ = write!(line, "To do it, type: install {number} yes");
    out.line(Style::Out, line.bytes());
    true
}

/// A time as the user reads it: "3.2 ms".
fn milliseconds(us: u32) -> Text {
    let mut v = Text::new();
    let tenths = (us + 50) / 100;
    let _ = write!(v, "{}.{} ms", tenths / 10, tenths % 10);
    v
}

/// Whether the kernel can read the graphics chip. Says why not.
fn chip_reachable(sys: &System, out: &mut dyn Out) -> bool {
    let mut line = FixedStr::<COLS>::new();
    let _ = match sys.card {
        Some(card) if crate::intel::known(card.id).is_some() && card.registers != 0 => return true,
        None => write!(line, "No graphics chip was found: the screen is the one the firmware set up."),
        Some(card) if crate::intel::known(card.id).is_some() => write!(line, "The registers of the graphics chip cannot be reached."),
        Some(card) => write!(line, "The graphics chip, {:04x}:{:04x}, is not one XilaOS knows how to read.", card.id & 0xffff, card.id >> 16),
    };
    out.line(Style::Error, line.bytes());
    false
}

fn display(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    if let Some(wish) = arg.strip_prefix(b"brightness") {
        // from 5 to 100: the kernel sets it and tells, or only tells
        let wish = trim(wish);
        let hundredths = parse_number(wish).filter(|n| (1..=100).contains(n));
        if (!wish.is_empty() && hundredths.is_none()) || (arg.len() > 10 && arg[10] != b' ') {
            out.line(Style::Error, b"Type display brightness, or with a number from 5 to 100: display brightness 60");
            return false;
        }
        if !chip_reachable(sys, out) {
            return false;
        }
        sys.request = Some(Request::Light(hundredths.map(|n| n as u32)));
        return true;
    }
    match arg {
        b"" => {}
        b"test" => {
            // the kernel does it once this command has returned; `served` tells
            sys.request = Some(Request::Measure);
            return true;
        }
        b"flip off" => {
            sys.request = Some(Request::Flip(Pictures::Direct));
            return true;
        }
        b"flip try" => {
            sys.request = Some(Request::Flip(Pictures::Trial));
            return true;
        }
        b"chip" | b"registers" | b"flip on" => {
            // what the chip is doing now is read by the kernel, as above
            if !chip_reachable(sys, out) {
                return false;
            }
            sys.request = Some(if arg == b"flip on" { Request::Flip(Pictures::Two) } else { Request::Chip { registers: arg == b"registers" } });
            return true;
        }
        _ => {
            out.line(Style::Error, b"Type display, or display with test, chip, registers, flip on, flip off, brightness.");
            return false;
        }
    }
    let (m, d) = (&sys.machine, &sys.display);
    let mut t = Table::new(b"display", &[b"item", b"value"]);
    let mut v = Text::new();
    let _ = write!(v, "{}x{}, cells of {} pixels", m.screen_width, m.screen_height, 8 * m.scale);
    t.row(&[b"screen", v.bytes()]);
    t.row(&[b"screen memory", if d.combining { b"write combining" } else { b"written piece by piece" }]);
    t.row(&[
        b"pictures change",
        match d.shown {
            Pictures::Direct => b"while the screen is swept",
            Pictures::Two => b"between two sweeps of the screen",
            Pictures::Trial => b"through two pictures in memory (a test)",
        },
    ]);
    let mut v = Text::new();
    let _ = match d.every {
        0 => write!(v, "a picture every {FRAME_TICKS} ticks, 100 ticks a second"),
        1 => write!(v, "a picture every sweep, {} sweeps a second", d.sweeps),
        every => write!(v, "a picture every {every} sweeps, {} sweeps a second", d.sweeps),
    };
    t.row(&[b"rhythm", v.bytes()]);
    t.row(&[b"whole screen", Text::from_bytes(milliseconds(d.whole_us).bytes()).also(b" to show, at the start").bytes()]);
    t.row(&[b"the firmware's way", Text::from_bytes(milliseconds(d.firmware_us).bytes()).also(b" to show, at the start").bytes()]);
    let mut v = Text::new();
    let _ = write!(v, "{} in the last second", d.pictures);
    t.row(&[b"pictures shown", v.bytes()]);
    t.row(&[b"longest picture", milliseconds(d.longest_us).bytes()]);
    t.row(&[b"city", Text::from_bytes(milliseconds(d.city_us).bytes()).also(b" to draw one picture").bytes()]);
    let mut v = Text::new();
    let _ = write!(v, "{} since the start", d.skipped);
    t.row(&[b"pictures skipped", v.bytes()]);
    show(sys, out, &t);
    true
}

/// Runs a command and keeps what it says, to show it as a code a phone
/// reads off the screen: the way out for text on a machine that has no
/// other. The code is made by `served`, once the command has said all.
fn qr(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    if arg.is_empty() {
        out.line(Style::Error, b"Type qr and a command, to read what it says with a phone: qr about");
        return false;
    }
    // A code takes the screen until a key is pressed: that is for who typed
    // the command. A program that runs by itself, the companion and a job
    // nobody waits for are nobody at a prompt.
    let aside = crate::jobs::in_turn(sys).is_some_and(|job| job.watch == crate::jobs::Watch::Nobody);
    if sys.session >= crate::events::SESSIONS || aside {
        out.line(Style::Error, b"qr is typed at a prompt: its code takes the screen of who asked for it.");
        return false;
    }
    let mut note = Note::new();
    let ok = exec(sys, arg, &mut note);
    sys.note = note;
    sys.noting = true;
    ok
}

/// What is refused to the companion about jobs: nothing it starts may
/// outlive its answer, and those of the user are not its own to stop.
fn not_its_jobs(sys: &System, out: &mut dyn Out) -> bool {
    if sys.acting {
        out.line(Style::Error, b"Jobs are not the companion's to start or stop.");
    }
    sys.acting
}

/// `submit name`, `submit name(values)`: runs a program as a job nobody
/// waits for. Its values are worked out now, with the names of who asks.
fn submit(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    if not_its_jobs(sys, out) {
        return false;
    }
    if arg.is_empty() || crate::lang::call_of(arg).is_none() {
        out.line(Style::Error, b"Type submit and a program: submit report, submit restock('cable', 5)");
        return false;
    }
    match crate::jobs::begin(sys, crate::jobs::What::Program(arg), crate::jobs::Watch::Nobody, out) {
        Ok(number) => {
            let mut line = FixedStr::<COLS>::new();
            let program = crate::jobs::find(sys, number).map_or(Text::new(), |at| sys.jobs.list()[at].program);
            let _ = write!(line, "Job {number} submitted: {program}.");
            out.line(Style::Done, line.bytes());
            true
        }
        Err(message) => {
            // nothing more to say when why was said already
            if !message.is_empty() {
                out.line(Style::Error, message.bytes());
            }
            false
        }
    }
}

/// When a job began, as a time of the day, if the machine tells the time.
fn began(job: &crate::jobs::Job) -> Text {
    job.began.map_or(Text::new(), |seconds| Text::from_bytes(&crate::clock::Moment::at(seconds).text().bytes()[11..]))
}

/// `jobs`: the jobs that run, and those that ended and are still listed.
fn jobs(sys: &mut System, out: &mut dyn Out) -> bool {
    let mut listed = Table::new(b"jobs", &[b"job", b"program", b"state", b"by", b"since", b"statements"]);
    // in the order they began
    let mut numbers = [0u32; crate::jobs::PLACES + 1];
    for (number, job) in numbers.iter_mut().zip(sys.jobs.list()) {
        *number = job.number;
    }
    numbers.sort_unstable();
    for number in numbers.into_iter().filter(|number| *number != 0) {
        let Some(job) = sys.jobs.list().iter().find(|job| job.number == number) else {
            continue;
        };
        let (mut shown, mut statements, mut command) = (Text::new(), Text::new(), Text::new());
        let _ = write!(shown, "{number}");
        let _ = write!(statements, "{}", job.statements);
        let _ = write!(command, "job {number}");
        listed.row_to(&[shown.bytes(), job.program.bytes(), crate::jobs::word(job.state), crate::jobs::by(job.by), began(job).bytes(), statements.bytes()], command.bytes());
    }
    if listed.rows == 0 {
        out.line(Style::Out, b"No job yet. submit name runs a program as one.");
        return true;
    }
    show(sys, out, &listed);
    true
}

/// `job 3`: one job, and what it said. `job 3 end`, `hold`, `release`.
fn job(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    let (number, verb) = split_word(arg);
    let (Some(number), true) = (parse_number(number), matches!(trim(verb), b"" | b"end" | b"hold" | b"release")) else {
        out.line(Style::Error, b"Type job and its number: job 3, job 3 end, job 3 hold, job 3 release");
        return false;
    };
    let number = number as u32;
    let done: Result<&str, crate::sql::Message> = match trim(verb) {
        b"" => {
            return match crate::jobs::find(sys, number) {
                Ok(at) => {
                    let job = &sys.jobs.list()[at];
                    let mut line = FixedStr::<COLS>::new();
                    let _ = write!(line, "Job {number} {}: {}", crate::jobs::told(job.state), job.program);
                    out.line(Style::Out, line.bytes());
                    line.clear();
                    let from: &str = match job.by {
                        crate::events::SCREEN => "at the screen",
                        crate::events::SERIAL => "at the serial port",
                        crate::events::NETWORK => "at the network prompt",
                        _ => "by a program",
                    };
                    let _ = write!(line, "Started {from}");
                    if job.began.is_some() {
                        let _ = write!(line, " at {}", began(job));
                    }
                    let _ = write!(line, ". {} statement{} run.", job.statements, if job.statements == 1 { "" } else { "s" });
                    out.line(Style::Out, line.bytes());
                    if job.said.dropped() > 0 {
                        line.clear();
                        let _ = write!(line, "({} lines it said before these are no longer kept.)", job.said.dropped());
                        out.line(Style::Out, line.bytes());
                    }
                    for said in job.said.lines() {
                        // the cells of a table it showed were kept apart by tabs
                        line.clear();
                        for (at, cell) in said.split(|byte| *byte == b'\t').enumerate() {
                            line.push_bytes(if at > 0 { b"  " } else { b"" });
                            line.push_bytes(cell);
                        }
                        out.line(Style::Out, line.bytes());
                    }
                    true
                }
                Err(message) => {
                    out.line(Style::Error, message.bytes());
                    false
                }
            };
        }
        _ if not_its_jobs(sys, out) => return false,
        b"end" => crate::jobs::end(sys, number).map(|_| "is stopped."),
        b"hold" => crate::jobs::hold(sys, number).map(|_| "is held."),
        _ => crate::jobs::release(sys, number).map(|_| "goes on."),
    };
    match done {
        Ok(what) => {
            let mut line = FixedStr::<COLS>::new();
            let _ = write!(line, "Job {number} {what}");
            out.line(Style::Done, line.bytes());
            true
        }
        Err(message) => {
            out.line(Style::Error, message.bytes());
            false
        }
    }
}

/// Tells whether something that was said is an error, on its way to
/// where it is shown.
struct Watched<'a> {
    out: &'a mut dyn Out,
    failed: bool,
}

impl Out for Watched<'_> {
    fn line(&mut self, style: Style, text: &[u8]) {
        self.failed |= style == Style::Error;
        self.out.line(style, text);
    }

    fn table(&mut self, table: &Table) {
        self.out.table(table);
    }

    fn clear(&mut self) {
        self.out.clear();
    }
}

/// Shows what the kernel answered to the request of the last command, if
/// it asked for something that has an answer. Whoever calls `exec` calls
/// this once the kernel has served the request. Returns false if the
/// answer is a failure: the command then failed, whatever `exec` said
/// when it left its request.
pub fn served(sys: &mut System, out: &mut dyn Out) -> bool {
    let mut watched = Watched { out, failed: false };
    answer(sys, &mut watched);
    !watched.failed
}

fn answer(sys: &mut System, out: &mut dyn Out) {
    if !sys.noting {
        return answers(sys, out);
    }
    // the command was for `qr`: its answer joins what it said, and the
    // whole becomes the code
    sys.noting = false;
    let mut note = sys.note;
    answers(sys, &mut note);
    if note.bytes().is_empty() {
        out.line(Style::Error, b"That command said nothing: there is no code to show.");
        return;
    }
    // as much text as a code holds that a camera still reads on this screen
    let rows = if sys.peek_rows == 0 { MAX_ROWS } else { sys.peek_rows };
    let most = crate::qr::most_version(crate::ui::code_points(rows));
    let text = &note.bytes()[..note.bytes().len().min(crate::qr::room(most))];
    let lost = note.lost + note.bytes().len() - text.len();
    let Some(code) = crate::qr::encode(text, most) else {
        return;
    };
    sys.code = Some(code);
    sys.codes += 1;
    let mut line = FixedStr::<COLS>::new();
    let _ = write!(line, "A code of {} characters is on the screen.", text.len());
    if lost > 0 {
        let _ = write!(line, " The last {lost} did not fit.");
    }
    out.line(Style::Out, line.bytes());
}

/// What the kernel answered, as text.
fn answers(sys: &mut System, out: &mut dyn Out) {
    if let Some(m) = sys.measured.take() {
        let mut line = FixedStr::<COLS>::from_bytes(b"The whole screen: drawn in ")
            .also(milliseconds(m.drawn_us).bytes())
            .also(b", shown in ")
            .also(milliseconds(m.shown_us).bytes());
        let _ = write!(line, " (best of {}).", m.rounds);
        out.line(Style::Out, line.bytes());
    }
    if let Some((state, registers)) = sys.chip.take() {
        if registers {
            let mut t = Table::new(b"registers", &[b"register", b"address", b"value"]);
            crate::intel::registers(&state, &mut t);
            show(sys, out, &t);
        } else if let Some(card) = sys.card {
            let mut t = Table::new(b"chip", &[b"item", b"value"]);
            crate::intel::describe(&card, &state, &mut t);
            show(sys, out, &t);
        }
    }
    match sys.net_restarted.take() {
        None => {}
        Some(true) => out.line(Style::Out, b"The card was reset and set up again. It asks the network for an address."),
        Some(false) => out.line(Style::Error, b"The card did not come back from its reset."),
    }
    if let Some(registers) = sys.net_registers.take() {
        let mut t = Table::new(b"registers", &[b"register", b"address", b"value"]);
        for (name, address, value) in registers {
            let (mut at, mut holds) = (Text::new(), Text::new());
            let _ = write!(at, "{address:05x}");
            let _ = write!(holds, "{value:08x}");
            t.row(&[name.as_bytes(), at.bytes(), holds.bytes()]);
        }
        show(sys, out, &t);
    }
    if let Some((to, took)) = sys.pinged.take() {
        let mut line = FixedStr::<COLS>::from_bytes(crate::net::ip_text(to).bytes());
        let _ = match took {
            Some(micros) => write!(line, " answered in {}.", core::str::from_utf8(milliseconds(micros).bytes()).unwrap_or("?")),
            None => write!(line, " did not answer within a second."),
        };
        out.line(if took.is_some() { Style::Out } else { Style::Error }, line.bytes());
    }
    match sys.lit.take() {
        None => {}
        Some(None) => out.line(Style::Error, b"No chip XilaOS knows sets the brightness of this screen."),
        Some(Some(hundredths)) => {
            let mut line = FixedStr::<COLS>::new();
            let _ = write!(line, "Brightness: {hundredths} of 100.");
            out.line(Style::Out, line.bytes());
        }
    }
    if let Some(flipped) = sys.flipped.take() {
        use crate::intel::NoFlip;
        let (style, text): (Style, &[u8]) = match flipped {
            Ok(Pictures::Two) => (Style::Out, b"Pictures now change between two sweeps of the screen."),
            Ok(Pictures::Direct) => (Style::Out, b"Pictures are written to the screen directly."),
            Ok(Pictures::Trial) => (Style::Out, b"Pictures now go through two pictures in memory: a test, no better to look at."),
            Err(NoFlip::NoPicture) => (Style::Error, b"The chip shows no picture of its own: nothing was changed."),
            Err(NoFlip::Pixels) => (Style::Error, b"The chip shows pixels of a kind XilaOS does not write: nothing was changed."),
            Err(NoFlip::NotTheScreen) => (Style::Error, b"What the chip shows is not the screen the firmware gave: nothing was changed."),
            Err(NoFlip::NoRoom) => (Style::Error, b"There is no graphics memory behind the screen for a second picture."),
            Err(NoFlip::Slow) => (Style::Error, b"The memory of a second picture could not be made quick to write."),
            Err(NoFlip::Deaf) => (Style::Error, b"The chip did not show the second picture: the screen is written directly."),
        };
        out.line(style, text);
    }
    match sys.named.take() {
        None => {}
        Some(Ok(ip)) => {
            let mut line = FixedStr::<COLS>::from_bytes(sys.asked_machine.bytes());
            let _ = write!(line, " is at {}.", core::str::from_utf8(crate::net::ip_text(ip).bytes()).unwrap_or("?"));
            out.line(Style::Out, line.bytes());
        }
        Some(Err(why)) => out.line(Style::Error, unnamed(why)),
    }
    if let Some((port, got)) = sys.got.take() {
        use crate::ask::Failed;
        let mut line = FixedStr::<COLS>::new();
        line.push_bytes(sys.asked_machine.bytes());
        let _ = write!(line, ":{port}");
        match got {
            Ok(Got { status, start: said, total }) => {
                let _ = write!(line, " answered {status}.");
                out.line(if status < 400 { Style::Done } else { Style::Error }, line.bytes());
                // the start of what it said, as text: shown, never run
                // its first eight lines that hold something, without what ends a line
                let mut rows = said.bytes().split(|byte| *byte == b'\n').map(|row| row.trim_ascii()).filter(|row| !row.is_empty());
                for row in rows.by_ref().take(8) {
                    let mut shown = FixedStr::<COLS>::new();
                    for &byte in row.iter().take(COLS - 1) {
                        shown.push(if (0x20..0x7f).contains(&byte) { byte } else { b'?' });
                    }
                    out.line(Style::Out, shown.bytes());
                }
                if rows.next().is_some() || total > said.len() {
                    let mut more = FixedStr::<COLS>::new();
                    let _ = write!(more, "(and more: {total} bytes in all)");
                    out.line(Style::Out, more.bytes());
                }
            }
            // its address was not found: nothing was asked of it
            Err(Failed::Name(why)) => out.line(Style::Error, unnamed(why)),
            // it could not be asked in secret: nothing was asked of it
            Err(why @ (Failed::Secret(_) | Failed::NoChance | Failed::NoClock)) => out.line(Style::Error, unsure(why).bytes()),
            Err(why) => {
                line.push_bytes(match why {
                    Failed::NoRoom => b" cannot be asked now: too many conversations at once.",
                    Failed::NoAnswer | Failed::Name(_) | Failed::Secret(_) | Failed::NoChance | Failed::NoClock => b" does not answer.",
                    Failed::Silent => b" took the call and then said nothing.",
                    Failed::NotAnAnswer => b" answered something that is not a page.",
                    Failed::Cut => b" hung up before it had said everything.",
                    Failed::TooLong => b" says more than XilaOS listens to: given up.",
                });
                out.line(Style::Error, line.bytes());
            }
        }
    }
    if let Some((server, fetched)) = sys.fetched.take() {
        use crate::tftp::Failed;
        let mut line = FixedStr::<COLS>::new();
        line.push_bytes(crate::net::ip_text(server).bytes());
        match fetched {
            Ok((bytes, check)) => {
                let _ = write!(line, " gave a system of {bytes} bytes, check {check:08x}.");
                out.line(Style::Done, line.bytes());
                out.line(Style::Out, b"Nothing is written yet. To put it on a disk: install, or install system.");
            }
            Err(why) => {
                line.push_bytes(match why {
                    Failed::NoWay => b" cannot be reached from here. Type net to see why.",
                    Failed::Silent => b" does not answer. Is tools/netboot.py running there?",
                    Failed::Refused => b" has no system to give.",
                    Failed::TooLarge => b" gives something too large to be a system.",
                    Failed::NotASystem => b" gave something that is not a system for this machine.",
                    Failed::NoMemory => b" was not asked: there is no memory to keep a system in.",
                });
                out.line(Style::Error, line.bytes());
            }
        }
    }
    match sys.installed.take() {
        None => {}
        Some(Ok(Installed { disk, whole })) => {
            let mut line = FixedStr::<COLS>::new();
            let _ = match whole {
                true => write!(line, "XilaOS is installed on disk {disk}. What was written was read back and is right."),
                false => write!(line, "The system on disk {disk} is replaced. What was written was read back and is right."),
            };
            out.line(Style::Out, line.bytes());
            if whole {
                out.line(Style::Out, b"The tables are now kept on that disk.");
                out.line(Style::Out, b"To start from it: take the USB stick out, then type reboot.");
            } else {
                out.line(Style::Out, b"The tables were not touched. It runs at the next start: type reboot.");
            }
        }
        Some(Err(error)) => {
            use crate::install::Failed;
            let mut line = FixedStr::<COLS>::new();
            let _ = match error {
                InstallError::Disk(Failed::TooSmall) => write!(line, "The disk is too small. Nothing was written."),
                InstallError::Disk(Failed::NoRoom) => write!(line, "The start-up partition has no room for the system. Nothing was written."),
                InstallError::Disk(Failed::Write(sector)) => write!(line, "The disk refused to write sector {sector}."),
                InstallError::Disk(Failed::Read(sector)) => write!(line, "The disk could not read back sector {sector}."),
                InstallError::Disk(Failed::Differs(sector)) => write!(line, "Sector {sector} of the disk does not hold what was written in it."),
                InstallError::Database(_) => write!(line, "XilaOS is on the disk, but the place of its tables could not be prepared:"),
            };
            out.line(Style::Error, line.bytes());
            match error {
                InstallError::Disk(Failed::TooSmall | Failed::NoRoom) => {}
                InstallError::Disk(_) => out.line(Style::Error, b"The installation is not complete: do it again, or take another disk."),
                InstallError::Database(error) => out.line(Style::Error, crate::change::engine_error(error, None).bytes()),
            }
        }
    }
}

fn screen(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    if arg.is_empty() {
        let mut t = Table::new(b"screen", &[b"key", b"screen", b"state"]);
        for (i, name) in SCREENS.iter().enumerate() {
            let mut key = Text::new();
            let _ = write!(key, "[Alt]+[{}]", i);
            let mut command = Text::new();
            let _ = write!(command, "screen {}", i);
            t.row_to(&[key.bytes(), name, if i == sys.screen { b"shown" } else { b"" }], command.bytes());
        }
        show(sys, out, &t);
        return true;
    }
    let wanted = parse_number(arg).or_else(|| SCREENS.iter().position(|name| name.eq_ignore_ascii_case(arg)));
    match wanted {
        Some(n) if n < SCREENS.len() => {
            sys.screen = n;
            true
        }
        _ => {
            out.line(Style::Error, b"There is no such screen. Type screen to list them.");
            false
        }
    }
}

/// Reports a setting, or sets it from `arg` with `set`.
fn setting(
    sys: &mut System,
    out: &mut dyn Out,
    what: &[u8],
    arg: &[u8],
    choices: &[u8],
    current: fn(&System) -> &'static [u8],
    set: fn(&mut System, &[u8]) -> bool,
) -> bool {
    if !arg.is_empty() && !set(sys, arg) {
        let mut msg = FixedStr::<COLS>::from_bytes(b"Choose one of: ");
        msg.push_bytes(choices);
        out.line(Style::Error, msg.bytes());
        return false;
    }
    let mut msg = FixedStr::<COLS>::from_bytes(what);
    msg.push_bytes(b" is ");
    msg.push_bytes(current(sys));
    msg.push(b'.');
    // an answer when the setting was asked for, a confirmation when it was set
    out.line(if arg.is_empty() { Style::Out } else { Style::Done }, msg.bytes());
    true
}

fn mood(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    setting(sys, out, b"Mood", arg, b"calm, auto, intense", |s| s.mood.name(), |s, a| {
        s.mood = match a {
            b"calm" | b"c" => MoodMode::Calm,
            b"auto" | b"a" => MoodMode::Auto,
            b"intense" | b"i" => MoodMode::Intense,
            _ => return false,
        };
        true
    })
}

fn theme(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    setting(sys, out, b"Theme", arg, b"day, night", |s| THEMES[s.theme].name, |s, a| {
        match THEMES.iter().position(|t| t.name == a) {
            Some(i) => {
                s.theme = i;
                true
            }
            None => false,
        }
    })
}

fn keymap(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    setting(sys, out, b"Keyboard layout", arg, b"us, fr", |s| s.layout.name(), |s, a| {
        s.layout = match a {
            b"us" => Layout::Us,
            b"fr" => Layout::Fr,
            _ => return false,
        };
        true
    })
}

/// A line as it may be shown on a screen and kept in the history of the
/// prompt: the same, unless it sets a key, whose value becomes stars.
pub fn typed(line: &[u8]) -> crate::line::Line {
    let mut shown = crate::line::Line::from_bytes(line);
    let (first, rest) = split_word(trim(line));
    let (second, rest) = split_word(trim(rest));
    let (_, value) = split_word(trim(rest));
    let value = trim(value);
    if first.eq_ignore_ascii_case(b"key") && second.eq_ignore_ascii_case(b"set") && !value.is_empty() {
        // everything after the name, whatever it is
        shown = crate::line::Line::from_bytes(&line[..line.len() - value.len() - (line.len() - trim_end(line).len())]);
        shown.push_bytes(b"********");
    }
    shown
}

fn trim_end(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().rposition(|byte| *byte != b' ').map_or(0, |at| at + 1);
    &bytes[..end]
}

/// `ask how many tasks are running?`: a question to the companion. Its
/// answer comes when the model has given it: on the screen it is shown
/// then, and the prompt is free meanwhile; the serial port waits for it.
///
/// In a program that runs by itself (`on insert into expenses`, `every
/// day at 8:00`) nobody waits: the question takes its turn and what is
/// said is written on the companion's screen. Between `{` and `}`, a
/// name or a calculation is replaced by its value: `ask is {new.label}
/// at {new.amount} unusual?`.
fn ask(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    if sys.acting {
        out.line(Style::Error, b"The companion does not ask itself.");
        return false;
    }
    // what is asked, with the values of what stands between braces
    let mut text = FixedStr::<{ crate::companion::ASKED }>::new();
    let mut rest = arg;
    let mut whole = true;
    while let Some(open) = rest.iter().position(|byte| *byte == b'{') {
        let Some(length) = rest[open..].iter().position(|byte| *byte == b'}') else {
            break;
        };
        let mut lexer = crate::sql::Lexer::new(&rest[open + 1..open + length]);
        let names = crate::expr::Names::without(sys.names(), b"Between { and } goes a name that has a value, or a calculation, not: ");
        let value = crate::expr::parse(&mut lexer, names).and_then(|value| lexer.end().map(|_| value)).and_then(|value| value.eval(&[crate::sql::Value::Null; TABLE_COLS]));
        match value {
            Ok(value) => {
                text.push_bytes(&rest[..open]);
                for &byte in value.shown().bytes() {
                    whole &= text.push(byte);
                }
            }
            Err(message) => {
                out.line(Style::Error, message.bytes());
                return false;
            }
        }
        rest = &rest[open + length + 1..];
    }
    for &byte in rest {
        whole &= text.push(byte);
    }
    if !whole {
        out.line(Style::Error, b"What is asked is 150 characters at most, values included.");
        return false;
    }
    let asked = match sys.session {
        // a program that runs by itself: nobody waits, the question takes its turn
        crate::events::BACKGROUND => {
            let who = sys.firing;
            crate::companion::ask_later(sys, text.bytes(), who.bytes())
        }
        crate::events::SCREEN | crate::events::SERIAL => crate::companion::ask(sys, text.bytes(), sys.session),
        // the answer has to find its way back: from the network it would not
        _ => Err(crate::sql::Message::from_bytes(b"ask is typed at the machine itself, or on its serial port.")),
    };
    match asked {
        Ok(()) => true,
        Err(message) => {
            out.line(Style::Error, message.bytes());
            false
        }
    }
}

/// Whether who types is at the machine: at its keyboard or on its serial
/// port. Not at the prompt over the network, not the companion acting by
/// itself, and not a program that runs by itself, which either of those
/// could have written. What takes a disk or opens the machine to others
/// is for nobody else.
fn at_the_machine(sys: &System) -> bool {
    matches!(sys.session, crate::events::SCREEN | crate::events::SERIAL) && !sys.acting
}

/// Who the companion is, and what it is doing. `companion off` stops it:
/// it is asked nothing and does nothing until `companion on`.
fn companion(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    if arg.eq_ignore_ascii_case(b"off") || arg.eq_ignore_ascii_case(b"on") {
        let on = arg.eq_ignore_ascii_case(b"on");
        // it may turn itself off, never on
        if on && sys.acting {
            out.line(Style::Error, b"companion on is typed at the machine itself.");
            return false;
        }
        crate::companion::switch(sys, on);
        out.line(Style::Done, if on { b"The companion is on." } else { b"The companion is off: it is asked nothing and does nothing until companion on." });
        return true;
    }
    if !arg.is_empty() {
        out.line(Style::Error, b"After companion: nothing, off or on. To change who it is: update companions set ...");
        return false;
    }
    let who = crate::companion::who(&sys.db);
    let mut t = Table::new(b"companion", &[b"item", b"value"]);
    t.row(&[b"name", who.name.bytes()]);
    t.row(&[b"model", who.model.bytes()]);
    t.row(&[b"at", who.place.bytes()]);
    let mut key = Text::from_bytes(who.key.bytes());
    key.push_bytes(if crate::secrets::get(&sys.db, who.key.bytes()).is_some() { b": kept" } else { b": not set, see key" });
    t.row(&[b"key", key.bytes()]);
    if !who.character.is_empty() {
        t.row(&[b"character", who.character.bytes()]);
    }
    let mut text = Text::new();
    let _ = match sys.companion.stage {
        _ if sys.companion.off => write!(text, "off"),
        crate::companion::Stage::Idle => write!(text, "nothing asked"),
        crate::companion::Stage::Calling => write!(text, "answering, for {} s", sys.ticks.saturating_sub(sys.companion.since) / 100),
    };
    t.row(&[b"now", text.bytes()]);
    for (item, count) in [(&b"calls to the model"[..], sys.companion.calls), (b"queries run for it", sys.companion.queries), (b"lines it ran", sys.companion.acts)] {
        text.clear();
        let _ = write!(text, "{count}");
        t.row(&[item, text.bytes()]);
    }
    out.table(&t);
    true
}

/// The keys that open services to this machine: `key` lists their names,
/// `key set openrouter sk-...` keeps one, `key forget openrouter` forgets
/// it. A key is never shown: not here, not by SQL, not to a browser.
fn key(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    // keys are not the companion's: not even their names
    if sys.acting {
        out.line(Style::Error, b"key is typed at the machine itself.");
        return false;
    }
    let (word, rest) = split_word(arg);
    let (name, value) = split_word(trim(rest));
    let value = trim(value);
    if arg.is_empty() {
        let mut t = Table::new(b"keys", &[b"name", b"key"]);
        crate::secrets::names(&sys.db, &mut |name| t.row(&[name, b"kept, never shown"]));
        if t.rows == 0 {
            out.line(Style::Out, b"No key is kept. To keep one: key set openrouter and the key.");
            return true;
        }
        out.table(&t);
        return true;
    }
    if word.eq_ignore_ascii_case(b"set") {
        // from the network it would have crossed it in clear already
        if sys.session == crate::events::NETWORK {
            out.line(Style::Error, b"key set is typed at the machine itself: the network would see the key.");
            return false;
        }
        if !xila_db::good_name(name) || !crate::secrets::good_value(value) {
            out.line(Style::Error, b"Type key set, a name, and the key, which holds no space: key set openrouter sk-...");
            return false;
        }
        return match crate::secrets::set(&mut sys.db, name, value) {
            Ok(replaced) => {
                let mut line = FixedStr::<COLS>::from_bytes(b"The key ");
                line.push_bytes(name);
                line.push_bytes(if replaced { b" is replaced. It is never shown." } else { b" is kept. It is never shown." });
                out.line(Style::Done, line.bytes());
                true
            }
            Err(xila_db::Error::BadName) => {
                out.line(Style::Error, b"A table named kept_keys is in the way: it is not the one keys are kept in.");
                false
            }
            Err(why) => {
                out.line(Style::Error, crate::change::engine_error(why, None).bytes());
                false
            }
        };
    }
    if word.eq_ignore_ascii_case(b"forget") && xila_db::good_name(name) && value.is_empty() {
        return match crate::secrets::forget(&mut sys.db, name) {
            Ok(true) => {
                let mut line = FixedStr::<COLS>::from_bytes(b"The key ");
                line.push_bytes(name);
                line.push_bytes(b" is forgotten.");
                out.line(Style::Done, line.bytes());
                true
            }
            Ok(false) => {
                out.line(Style::Error, b"No key is kept under that name. Type key to list them.");
                false
            }
            Err(why) => {
                out.line(Style::Error, crate::change::engine_error(why, None).bytes());
                false
            }
        };
    }
    out.line(Style::Error, b"After key: nothing, set with a name and the key, or forget with a name.");
    false
}

/// Says the date and the time.
fn time(sys: &mut System, out: &mut dyn Out) -> bool {
    let Some(now) = sys.now() else {
        out.line(Style::Error, b"The clock of this machine does not tell the time.");
        return false;
    };
    let moment = crate::clock::Moment::at(now);
    let mut line = FixedStr::<COLS>::new();
    let (weekday, month) = (crate::clock::WEEKDAYS[moment.weekday()], crate::clock::MONTHS[moment.month as usize - 1]);
    let _ = write!(line, "{weekday} {} {month} {}, {:02}:{:02}:{:02}.", moment.day, moment.year, moment.hour, moment.minute, moment.second);
    out.line(Style::Out, line.bytes());
    true
}

fn uptime(sys: &mut System, out: &mut dyn Out) -> bool {
    let mut msg = FixedStr::<COLS>::new();
    let _ = write!(msg, "Up for {}.{:02} s.", sys.ticks / 100, sys.ticks % 100);
    out.line(Style::Out, msg.bytes());
    true
}

fn tasks(sys: &mut System, out: &mut dyn Out) -> bool {
    let mut t = Table::new(b"tasks", &[b"id", b"name", b"state", b"ran"]);
    let list = sys.tasks;
    for (id, task) in list.iter().enumerate().filter(|(_, task)| task.used) {
        let (mut number, mut ran) = (Text::new(), Text::new());
        let _ = write!(number, "{}", id);
        let _ = write!(ran, "{}.{:02} s", task.ticks / 100, task.ticks % 100);
        t.row(&[number.bytes(), task.name.bytes(), task.state, ran.bytes()]);
    }
    show(sys, out, &t);
    true
}

/// The city as a list: what a program sees of what the user sees.
fn city(sys: &mut System, out: &mut dyn Out) -> bool {
    let plots = crate::city::scene(sys);
    let mut t = Table::new(b"city", &[b"plot", b"block", b"height", b"lit"]);
    for plot in plots.iter() {
        for block in plot.blocks.iter().filter(|block| block.used) {
            let mut height = Text::new();
            let _ = write!(height, "{}", block.height);
            t.row(&[plot.name, block.name.bytes(), height.bytes(), if block.lit { b"yes" } else { b"" }]);
        }
    }
    show(sys, out, &t);
    true
}

fn spin(sys: &mut System, out: &mut dyn Out) -> bool {
    if sys.tasks.iter().all(|task| task.used) {
        out.line(Style::Error, b"There is no room for another task.");
        return false;
    }
    sys.request = Some(Request::Spin);
    out.line(Style::Done, b"Started a task that never rests. See tasks; stop it with stop and its id.");
    true
}

fn stop(sys: &mut System, arg: &[u8], out: &mut dyn Out) -> bool {
    let task = parse_number(arg).filter(|&id| id < MAX_TASKS && sys.tasks[id].used);
    match task {
        None => {
            out.line(Style::Error, b"Give the id of a task. Type tasks to list them.");
            false
        }
        Some(id) if sys.tasks[id].system => {
            out.line(Style::Error, b"The system needs that task.");
            false
        }
        Some(id) => {
            sys.request = Some(Request::Stop(id));
            out.line(Style::Done, b"Stopped.");
            true
        }
    }
}

/// Writes the characters on the screen, one line per row. This is how a
/// program on the serial port sees what the user sees.
fn peek(sys: &mut System, out: &mut dyn Out) -> bool {
    let rows = sys.peek;
    for row in rows.iter().take(sys.peek_rows) {
        let end = row.iter().rposition(|&c| c != b' ').map_or(0, |i| i + 1);
        out.line(Style::Out, &row[..end]);
    }
    true
}

/// Completes the start of a command name. Returns how many names match and
/// what the line becomes: the name and a space if there is one match, the
/// part the matches have in common otherwise.
pub fn complete(prefix: &[u8]) -> (usize, FixedStr<16>) {
    let mut matches = 0;
    let mut common = FixedStr::<16>::new();
    for name in NAMES {
        if name.starts_with(prefix) {
            if matches == 0 {
                common = FixedStr::from_bytes(name);
            } else {
                let same = common.bytes().iter().zip(name.iter()).take_while(|(a, b)| a == b).count();
                common = FixedStr::from_bytes(&name[..same]);
            }
            matches += 1;
        }
    }
    if matches == 1 {
        common.push(b' ');
    }
    (matches, common)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Collects output as text, tables in their aligned form.
    #[derive(Default)]
    pub struct Lines {
        pub lines: Vec<String>,
        pub errors: usize,
        pub cleared: bool,
    }

    impl Out for Lines {
        fn line(&mut self, style: Style, text: &[u8]) {
            if style == Style::Error {
                self.errors += 1;
            }
            self.lines.push(String::from_utf8_lossy(text).into_owned());
        }
        fn table(&mut self, t: &Table) {
            self.line(Style::Out, t.aligned(None).bytes());
            for r in 0..t.rows {
                self.line(Style::Out, t.aligned(Some(r)).bytes());
            }
        }
        fn clear(&mut self) {
            self.cleared = true;
        }
    }

    pub(crate) fn run_line(sys: &mut System, line: &str) -> (bool, Lines) {
        let mut out = Lines::default();
        let ok = exec(sys, line.as_bytes(), &mut out);
        (ok, out)
    }

    #[test]
    fn help_fits_the_screen_and_covers_every_command() {
        for line in HELP_COMMANDS.iter().chain(HELP_KEYS.iter()) {
            assert!(line.len() < COLS, "too long: {}", String::from_utf8_lossy(line));
        }
        for line in HELP_LANGUAGE.iter() {
            assert!(line.len() < COLS, "too long: {}", String::from_utf8_lossy(line));
        }
        for name in NAMES.iter().filter(|name| **name != b"peek") {
            let listed = HELP_COMMANDS.iter().chain(HELP_LANGUAGE.iter()).any(|line| line.len() > 3 && line[3..].starts_with(name));
            assert!(listed, "not in the help: {}", String::from_utf8_lossy(name));
        }
    }

    /// A disk in memory of which one page can be made to come back changed.
    struct Rots {
        disk: xila_db::Memory,
        rotten: std::rc::Rc<std::cell::Cell<Option<u64>>>,
    }

    impl xila_db::Disk for Rots {
        fn name(&self) -> &'static str {
            "test"
        }
        fn pages(&self) -> u64 {
            self.disk.pages()
        }
        fn read(&mut self, page: u64, into: &mut xila_db::Page) -> Result<(), xila_db::DiskError> {
            self.disk.read(page, into)?;
            if self.rotten.get() == Some(page) {
                into[100] ^= 1;
            }
            Ok(())
        }
        fn write(&mut self, page: u64, from: &xila_db::Page) -> Result<(), xila_db::DiskError> {
            self.disk.write(page, from)
        }
        fn flush(&mut self) -> Result<(), xila_db::DiskError> {
            self.disk.flush()
        }
    }

    #[test]
    fn a_row_is_reached_by_its_id_without_reading_its_table() {
        let mut sys = System::new();
        sys.db.set_limit(16 << 20);
        assert!(run_line(&mut sys, "create table many (n int, label text)").0);
        for batch in 0..200 {
            let rows: Vec<String> = (batch * 4..batch * 4 + 4).map(|n| format!("({n}, 'row {n} {}')", "x".repeat(30))).collect();
            let (ok, out) = run_line(&mut sys, &format!("insert into many values {}", rows.join(", ")));
            assert!(ok, "{:?}", out.lines);
        }
        // what a statement costs, in pages, as `disk` counts them
        let cost = |sys: &mut System, line: &str| {
            let before = sys.db.storage().looked;
            let (ok, out) = run_line(sys, line);
            assert!(ok, "{line}: {:?}", out.lines);
            (sys.db.storage().looked - before, out.lines)
        };
        let (whole, lines) = cost(&mut sys, "select count(*) from many");
        assert_eq!(lines[1].trim(), "800");
        assert!(whole > 10, "{whole}");
        let (one, lines) = cost(&mut sys, "select n from many where id = 640");
        assert_eq!((one, lines[1].trim()), (2, "639"));
        let (few, lines) = cost(&mut sys, "select sum(n) from many where id between 100 and 103 and n != 100");
        assert_eq!(lines[1].trim(), (99 + 101 + 102).to_string());
        assert!(few <= 3, "{few}");
        let (among, lines) = cost(&mut sys, "select count(*) from many where id in (5, 6, 700) and label like 'row%'");
        assert_eq!(lines[1].trim(), "3");
        assert!(among < whole, "{among}");
        assert_eq!(cost(&mut sys, "select n from many where id = 1 and id = 2"), (0, vec!["   n".to_string()]));
        assert_eq!(cost(&mut sys, "select n from many where id = -4").0, 0);
        // a condition that says nothing of the id reads everything, as before
        assert_eq!(cost(&mut sys, "select count(*) from many where n = 5 or id = 3").0, whole);
        // changes go the same way
        let (changed, lines) = cost(&mut sys, "update many set label = 'changed' where id = 400");
        assert_eq!(lines, ["1 row changed in many."]);
        let (removed, lines) = cost(&mut sys, "delete from many where id >= 799");
        assert_eq!(lines, ["2 rows removed from many."]);
        // finding the rows, then going down to each one to change it
        assert!(changed < whole && removed < whole, "{changed} {removed} {whole}");
        assert_eq!(run_line(&mut sys, "update many set n = 0 where id = 3 and id = 4").1.lines, ["0 rows changed in many."]);
        assert_eq!(run_line(&mut sys, "delete from many where id < 0").1.lines, ["0 rows removed from many."]);
        assert_eq!(run_line(&mut sys, "select id, label from many where id > 397 and id <= 400 order by id desc limit 1").1.lines[1].trim(), "400  changed");
        assert_eq!(run_line(&mut sys, "select count(*) from many").1.lines[1].trim(), "798");
        // the views of the system are not found by an id: they are read as before
        assert_eq!(run_line(&mut sys, "select name from tasks where id = 0 and id = 1").1.lines.len(), 1);
        let (ok, out) = run_line(&mut sys, "disk");
        assert!(ok && out.lines[5].trim().starts_with("pages read") && out.lines[5].trim().split_whitespace().last().unwrap().parse::<u64>().unwrap() == sys.db.storage().looked, "{:?}", out.lines);
    }

    #[test]
    fn stored_tables_are_joined_and_the_second_is_found_by_its_id() {
        let mut sys = System::new();
        sys.db.set_limit(16 << 20);
        assert!(run_line(&mut sys, "create table tags (name text)").0);
        assert!(run_line(&mut sys, "create table spent (label text, tag int, amount decimal)").0);
        for batch in 0..300 {
            let rows: Vec<String> = (batch * 4..batch * 4 + 4).map(|n| format!("('tag {n} {}')", "x".repeat(30))).collect();
            assert!(run_line(&mut sys, &format!("insert into tags values {}", rows.join(", "))).0);
        }
        assert!(run_line(&mut sys, "update tags set name = 'food' where id = 7").0);
        assert!(run_line(&mut sys, "update tags set name = 'travel' where id = 390").0);
        assert!(run_line(&mut sys, "insert into spent values ('coffee', 7, 3.5), ('train', 390, 42), ('bread', 7, 2.1), ('gift', null, 20), ('lost', 9999, 1)").0);
        let said = |sys: &mut System, line: &str| {
            let before = sys.db.storage().looked;
            let (ok, out) = run_line(sys, line);
            assert!(ok, "{line}: {:?}", out.lines);
            (out.lines.iter().map(|line| line.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>(), sys.db.storage().looked - before)
        };
        let (lines, cost) = said(&mut sys, "select s.label, t.name from spent s join tags t on t.id = s.tag order by s.id");
        assert_eq!(lines, ["label name", "coffee food", "train travel", "bread food"]);
        // each tag is found by its id: a few pages for each row of spent, not the pages of tags
        let (_, whole) = said(&mut sys, "select count(*) from tags");
        assert!(whole > 15 && cost <= whole, "{cost} {whole}");
        let (lines, _) = said(&mut sys, "select s.label, t.name from spent s left join tags t on s.tag = t.id where s.amount > 10");
        assert_eq!(lines, ["label name", "train travel", "gift"]);
        let (lines, _) = said(&mut sys, "select t.name, sum(s.amount) as total, count(*) from spent s join tags t on t.id = s.tag group by t.name order by total desc");
        assert_eq!(lines, ["name total count", "travel 42.00 1", "food 5.60 2"]);
        // both ids are named after their table; a table is joined to itself, read while it is read
        assert_eq!(run_line(&mut sys, "select id from spent s join tags t on t.id = s.tag").1.lines, ["Both tables have a column id: write its table before it, as in t.id"]);
        let (lines, _) = said(&mut sys, "select a.label, b.label from spent a join spent b on a.tag = b.tag and a.id < b.id");
        assert_eq!(lines, ["label label", "coffee bread"]);
        let (lines, _) = said(&mut sys, "select count(*) from tags a join tags b on b.id = a.id + 1 where a.id > 1195");
        assert_eq!(lines, ["count", "4"]);
        // a view of the system beside a stored table, and the other way round
        let (lines, _) = said(&mut sys, "select t.name, c.name from tables t join columns c on c.table_name = t.name where t.kind = 'stored' and c.position = 2");
        assert_eq!(lines, ["name name", "tags name", "spent label"]);
        let (lines, _) = said(&mut sys, "select s.label, k.name from spent s join tasks k on k.id = s.tag - 7 where s.id = 1");
        assert_eq!(lines, ["label name"]);
        // in a loop of the language, and as a value
        let (lines, _) = said(&mut sys, "let most = select t.name from spent s join tags t on t.id = s.tag order by s.amount desc limit 1");
        assert_eq!(lines, Vec::<String>::new());
        assert_eq!(said(&mut sys, "say most").0, ["travel"]);
    }

    #[test]
    fn disk_check_reads_every_page() {
        let mut sys = System::new();
        let (ok, out) = run_line(&mut sys, "disk check");
        assert_eq!((ok, out.lines), (true, vec!["There are no stored tables yet: nothing to read.".to_string()]));
        let rotten = std::rc::Rc::new(std::cell::Cell::new(None));
        let disk = Rots { disk: xila_db::Memory::new(256), rotten: rotten.clone() };
        let Ok(db) = xila_db::Database::open(xila_db::Volume::single(Box::new(disk))) else {
            panic!("an empty disk was refused");
        };
        sys.db = db;
        assert!(run_line(&mut sys, "create table notes (title text)").0);
        assert!(run_line(&mut sys, "insert into notes values ('one'), ('two')").0);
        let used = sys.db.storage().used;
        let (ok, out) = run_line(&mut sys, "disk check");
        assert_eq!((ok, out.lines), (true, vec![format!("The tables were read whole: {used} pages ({} KB), every one is right.", used * 4)]));
        // one page after the other comes back changed: each time the check says which,
        // though the page was read before and is kept in memory
        let (mut found, mut met) = (0, 0);
        for page in 0..256 {
            rotten.set(Some(page));
            let (ok, out) = run_line(&mut sys, "disk check");
            if !ok {
                assert_eq!(out.lines, [format!("The database is damaged: page {page} cannot be read."), "The check stopped there. Nothing was changed.".to_string()]);
                found += 1;
                // a query that has to read that page from the disk meets it too
                met += !run_line(&mut sys, "select count(*) from notes").0 as u64;
            }
        }
        // every page of the trees (the two headers are not counted among the used)
        assert_eq!(found, used);
        assert!(met >= 1);
        // each was counted, and is said as an event of the disk at the next look, once
        assert_eq!(sys.damaged.get(), found + met);
        run_line(&mut sys, "listen disk");
        crate::watch::look(&mut sys);
        let mut events = Vec::new();
        while let Some(event) = sys.events.take(crate::events::SCREEN) {
            events.push((String::from_utf8_lossy(event.channel.bytes()).to_string(), String::from_utf8_lossy(event.text.bytes()).to_string()));
        }
        assert_eq!(events, [("disk".to_string(), "damaged page found".to_string())]);
        rotten.set(None);
        assert!(run_line(&mut sys, "disk check").0);
        assert_eq!(run_line(&mut sys, "disk nonsense").1.lines, ["Usage: disk, disk check, or disk erase"]);
    }

    #[test]
    fn unknown_command_fails() {
        let mut sys = System::new();
        let (ok, out) = run_line(&mut sys, "frobnicate now");
        assert!(!ok);
        assert_eq!(out.lines, ["Unknown command: frobnicate"]);
        assert_eq!(out.errors, 1);
    }

    #[test]
    fn empty_line_does_nothing() {
        let mut sys = System::new();
        let (ok, out) = run_line(&mut sys, "   ");
        assert!(ok);
        assert!(out.lines.is_empty());
    }

    #[test]
    fn help_is_a_numbered_list_that_can_be_followed() {
        let mut sys = System::new();
        let (ok, out) = run_line(&mut sys, "HELP");
        assert!(ok);
        assert_eq!(out.lines, ["     topic   shows", " [1] help c  the commands", " [2] help l  XilaScript, the language", " [3] help k  the keys"]);
        let (ok, out) = run_line(&mut sys, "3");
        assert!(ok);
        assert_eq!(out.lines[0], "XilaOS keys:");
        assert_eq!(run_line(&mut sys, "2").1.lines[1], "   let name = value           - Keeps a value: let total = select sum(n) from t");
        let (ok, out) = run_line(&mut sys, "7");
        assert!(!ok);
        assert_eq!(out.lines, ["That line leads nowhere."]);
    }

    #[test]
    fn a_numbered_list_belongs_to_who_was_shown_it() {
        let mut sys = System::new();
        // the screen is shown a list, then the serial port another one
        assert!(run_line(&mut sys, "help").0);
        sys.session = crate::events::SERIAL;
        assert_eq!(run_line(&mut sys, "1").1.lines, ["There is no list to follow yet."]);
        assert!(run_line(&mut sys, "screen").0);
        assert!(run_line(&mut sys, "2").0);
        assert_eq!(sys.screen, 1);
        // the screen still follows its own
        sys.session = crate::events::SCREEN;
        assert_eq!(run_line(&mut sys, "3").1.lines[0], "XilaOS keys:");
        // a program that runs by itself has no list, and leaves the others alone
        sys.session = crate::events::BACKGROUND;
        assert!(crate::lang::line(&mut sys, b"screen", &mut Lines::default()));
        sys.session = crate::events::SCREEN;
        assert_eq!(run_line(&mut sys, "3").1.lines[0], "XilaOS keys:");
    }

    #[test]
    fn a_number_without_a_list_is_an_error() {
        let mut sys = System::new();
        let (ok, out) = run_line(&mut sys, "1");
        assert!(!ok);
        assert_eq!(out.lines, ["There is no list to follow yet."]);
    }

    #[test]
    fn settings_report_and_change() {
        let mut sys = System::new();
        assert_eq!(run_line(&mut sys, "mood").1.lines, ["Mood is auto."]);
        assert_eq!(run_line(&mut sys, "mood intense").1.lines, ["Mood is intense."]);
        assert_eq!(sys.mood, MoodMode::Intense);
        let (ok, out) = run_line(&mut sys, "mood sleepy");
        assert!(!ok);
        assert_eq!(out.lines, ["Choose one of: calm, auto, intense"]);
        assert_eq!(run_line(&mut sys, "theme night").1.lines, ["Theme is night."]);
        assert_eq!(sys.theme, 1);
        assert_eq!(run_line(&mut sys, "keymap us").1.lines, ["Keyboard layout is us."]);
        assert_eq!(sys.layout, Layout::Us);
    }

    #[test]
    fn qr_makes_a_code_of_what_a_command_says() {
        let mut sys = System::new();
        let (ok, out) = run_line(&mut sys, "qr");
        assert!(!ok && !sys.noting);
        assert_eq!(out.lines, ["Type qr and a command, to read what it says with a phone: qr about"]);

        // the command says nothing where it was typed: it is kept for the code
        sys.machine.cpu = FixedStr::from_bytes(b"A processor");
        let (ok, out) = run_line(&mut sys, "qr about");
        assert!(ok && out.lines.is_empty() && sys.noting && sys.code.is_none());
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        let text = String::from_utf8_lossy(sys.note.bytes()).into_owned();
        assert!(text.starts_with("item\tvalue\nsystem\tXilaOS 0.1\nprocessor\tA processor\n"), "{text}");
        assert_eq!(out.lines, [format!("A code of {} characters is on the screen.", text.len())]);
        assert!(!sys.noting);
        let code = sys.code.take().expect("a code to show");
        assert_eq!(code.size(), crate::qr::encode(text.as_bytes(), 40).unwrap().size());

        // what the kernel answers afterwards is part of it
        assert!(run_line(&mut sys, "qr display test").0);
        assert_eq!(sys.request.take(), Some(Request::Measure));
        sys.measured = Some(Measure { rounds: 5, drawn_us: 2449, shown_us: 12_000 });
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert_eq!(out.lines, ["A code of 65 characters is on the screen."]);
        assert!(sys.code.take().is_some() && sys.measured.is_none());
        assert_eq!(sys.codes, 2);

        // an error is text too; nothing at all is not
        let (ok, _) = run_line(&mut sys, "qr nonsense");
        assert!(!ok);
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert!(sys.code.take().is_some() && out.errors == 0);
        run_line(&mut sys, "qr clear");
        let mut out = Lines::default();
        // no code is a failure, though the command itself had nothing to say against it
        assert!(!served(&mut sys, &mut out));
        assert_eq!(out.lines, ["That command said nothing: there is no code to show."]);
        assert!(sys.code.is_none() && !sys.noting);

        // more text than a code holds on this screen: the end is left out, and said
        sys.peek_rows = 40;
        run_line(&mut sys, "create table long (a text, b text, c text)");
        for _ in 0..30 {
            assert!(run_line(&mut sys, "insert into long values ('0123456789012345678901234567890123456789', '0123456789012345678901234567890123456789', 'x')").0);
        }
        run_line(&mut sys, "qr select * from long");
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        let room = crate::qr::room(crate::qr::most_version(crate::ui::code_points(40)));
        assert_eq!(out.lines, [format!("A code of {room} characters is on the screen. The last {} did not fit.", sys.note.bytes().len() - room)]);
        assert_eq!(sys.code.unwrap().size(), 17 + 4 * 27);
        // without `qr`, `served` is as before
        sys.code = None;
        run_line(&mut sys, "about");
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert!(out.lines.is_empty() && sys.code.is_none());
    }

    #[test]
    fn install_asks_twice_before_it_takes_a_disk() {
        use crate::gpt::{Holds, Partition};
        use crate::install::Failed;
        let mut sys = System::new();
        assert_eq!(run_line(&mut sys, "install").1.lines, ["No disk was found. XilaOS installs on SATA disks only, for now."]);
        assert_eq!(run_line(&mut sys, "install 1").1.lines, ["There is no disk 1. Type install to list the disks."]);
        assert_eq!(run_line(&mut sys, "install system").1.lines, ["XilaOS is not installed on any disk. Type install to list the disks."]);

        let disk = |model: &[u8], megabytes: u64, holds| Some(DiskInfo { model: FixedStr::from_bytes(model), sectors: megabytes << 11, holds, taken: false });
        let system = Some((Partition { first: 2048, sectors: 100 << 11 }, Partition { first: 101 << 11, sectors: 300 << 11 }));
        sys.disks[0] = disk(b"HITACHI HTS725050A7E630", 476_940, Holds::Partitions { count: 3, system: None });
        sys.disks[1] = disk(b"QEMU HARDDISK", 64, Holds::Nothing);
        sys.disks[2] = disk(b"A DISK WITH A NAME OF FORTY CHARACTERS !", 500, Holds::OldTable);
        let listed = [
            "   disk  model                     size    holds",
            "   1     HITACHI HTS725050A7E630   465 GB  3 partitions",
            "   2     QEMU HARDDISK             64 MB   nothing",
            "   3     A DISK WITH A NAME OF FO  500 MB  an old kind of partition table",
        ];
        // without its own file XilaOS has nothing to put on a disk
        let (ok, out) = run_line(&mut sys, "install");
        assert!(ok);
        assert_eq!(out.lines[..4], listed);
        let no_system = ["This XilaOS cannot install: the firmware did not give it its own file.", "A machine that serves the system can give it: install from and its address."];
        assert_eq!(out.lines[4..], no_system);
        let (ok, out) = run_line(&mut sys, "install 1 yes");
        assert!(!ok && sys.request.is_none() && out.lines == no_system);
        // it can be fetched from a machine of the network, once there is a network
        for wrong in ["install from", "install from the-mac", "install from 192.168.1", "install from 192.168.1.168 now"] {
            let (ok, out) = run_line(&mut sys, wrong);
            assert!(!ok && sys.request.is_none(), "{wrong}");
            assert_eq!(out.lines, ["Type install from and the address of the machine that serves the system."], "{wrong}");
        }
        assert_eq!(run_line(&mut sys, "install from 192.168.1.168").1.lines, ["XilaOS drives no network card of this machine. Type devices to see what it has."]);
        sys.net_card = Some(NetCard { name: "Intel 82579LM", link: Some((100, true)), damaged: 0, missed: 0, sent: 0, refused: 0 });
        assert_eq!(run_line(&mut sys, "install from 192.168.1.168").1.lines, ["This machine has no address on the network yet. Type net to see why."]);
        (sys.net.ip, sys.net.mask) = (Some([192, 168, 1, 11]), [255, 255, 255, 0]);
        let (ok, out) = run_line(&mut sys, "install from 192.168.1.168");
        assert!(ok && out.lines.is_empty());
        assert_eq!(sys.request.take(), Some(Request::Fetch([192, 168, 1, 168])));
        // what the kernel answers
        use crate::tftp::Failed as NotFetched;
        let said = |sys: &mut System, fetched| {
            sys.fetched = Some(([192, 168, 1, 168], fetched));
            let mut out = Lines::default();
            served(sys, &mut out);
            assert!(out.lines.iter().all(|line| line.len() < COLS));
            (out.errors, out.lines)
        };
        assert_eq!(said(&mut sys, Ok((760_832, 0x1a2b_3c4d))), (0, vec![
            "192.168.1.168 gave a system of 760832 bytes, check 1a2b3c4d.".to_string(),
            "Nothing is written yet. To put it on a disk: install, or install system.".to_string(),
        ]));
        assert_eq!(said(&mut sys, Err(NotFetched::Silent)), (1, vec!["192.168.1.168 does not answer. Is tools/netboot.py running there?".to_string()]));
        assert_eq!(said(&mut sys, Err(NotFetched::Refused)), (1, vec!["192.168.1.168 has no system to give.".to_string()]));
        assert_eq!(said(&mut sys, Err(NotFetched::NotASystem)), (1, vec!["192.168.1.168 gave something that is not a system for this machine.".to_string()]));
        for why in [NotFetched::NoWay, NotFetched::TooLarge, NotFetched::NoMemory] {
            assert_eq!(said(&mut sys, Err(why)).0, 1);
        }
        assert!(sys.fetched.is_none());
        sys.system_bytes = 600_000;
        let (_, out) = run_line(&mut sys, "install");
        assert_eq!(out.lines[4..], ["To put XilaOS on a disk, type install and its number. Nothing is written yet."]);

        // what will be lost is said first, and nothing is asked of the kernel
        run_line(&mut sys, "create table notes (title text)");
        let (ok, out) = run_line(&mut sys, "install 1");
        assert!(ok);
        assert_eq!(
            out.lines,
            [
                "Disk 1, HITACHI HTS725050A7E630, 465 GB, holds: 3 partitions.",
                "Installing takes the whole disk: all it holds is lost.",
                "XilaOS gets 100 MB to start from, and 465 GB for the tables.",
                "The tables made since the start are in memory: they are lost too.",
                "To do it, type: install 1 yes",
            ]
        );
        assert!(sys.request.is_none());
        for wrong in ["install 1 now", "install yes", "install 1 yes please", "install one", "install system 1 yes"] {
            let (ok, out) = run_line(&mut sys, wrong);
            assert!(!ok && sys.request.is_none(), "{wrong}");
            assert_eq!(out.lines, ["Type install to list the disks, install 1 to take disk 1, or install system."], "{wrong}");
        }
        assert_eq!(run_line(&mut sys, "install 2").1.lines, ["Disk 2 is too small: XilaOS needs 118 MB at least."]);
        assert_eq!(run_line(&mut sys, "install 4 yes").1.lines, ["There is no disk 4. Type install to list the disks."]);
        assert_eq!(run_line(&mut sys, "install 0").1.lines, ["There is no disk 0. Type install to list the disks."]);
        assert!(sys.request.is_none());
        let (ok, out) = run_line(&mut sys, "Install 1 YES");
        assert!(ok && out.lines.is_empty());
        assert_eq!(sys.request.take(), Some(Request::Install { disk: 1, whole: true }));

        // what the kernel answers
        let told = |sys: &mut System, answer| {
            sys.installed = Some(answer);
            let mut out = Lines::default();
            served(sys, &mut out);
            assert!(sys.installed.is_none());
            (out.errors, out.lines)
        };
        assert_eq!(
            told(&mut sys, Ok(Installed { disk: 1, whole: true })),
            (
                0,
                vec![
                    "XilaOS is installed on disk 1. What was written was read back and is right.".to_string(),
                    "The tables are now kept on that disk.".to_string(),
                    "To start from it: take the USB stick out, then type reboot.".to_string()
                ]
            )
        );
        assert_eq!(
            told(&mut sys, Err(InstallError::Disk(Failed::Differs(2051)))),
            (
                2,
                vec![
                    "Sector 2051 of the disk does not hold what was written in it.".to_string(),
                    "The installation is not complete: do it again, or take another disk.".to_string()
                ]
            )
        );
        assert_eq!(told(&mut sys, Err(InstallError::Disk(Failed::Write(7)))).1[0], "The disk refused to write sector 7.");
        assert_eq!(
            told(&mut sys, Err(InstallError::Database(xila_db::Error::Disk))).1,
            ["XilaOS is on the disk, but the place of its tables could not be prepared:", "The disk does not answer. Nothing was changed."]
        );

        // the system alone: only where XilaOS is, and without asking twice
        assert_eq!(run_line(&mut sys, "install system 1").1.lines, ["XilaOS is not on disk 1: there is no system to replace. See: install 1"]);
        sys.disks[0] = disk(b"HITACHI HTS725050A7E630", 476_940, Holds::Partitions { count: 2, system });
        sys.disks[0].as_mut().unwrap().taken = true;
        // the tables are in memory: the partition found on that disk was not used
        assert_eq!(run_line(&mut sys, "install").1.lines[1], "   1     HITACHI HTS725050A7E630   465 GB  XilaOS");
        let Ok(stored) = xila_db::Database::format(xila_db::Volume::single(Box::new(Used(xila_db::Memory::new(2560))))) else {
            panic!("an empty disk was refused");
        };
        sys.db = stored;
        let (_, out) = run_line(&mut sys, "install");
        assert_eq!(out.lines[1], "   1     HITACHI HTS725050A7E630   465 GB  XilaOS, in use");
        assert_eq!(out.lines[5], "To replace the system only, and keep the tables: install system");
        let (ok, out) = run_line(&mut sys, "install system");
        assert!(ok && out.lines.is_empty());
        assert_eq!(sys.request.take(), Some(Request::Install { disk: 1, whole: false }));
        assert_eq!(
            told(&mut sys, Ok(Installed { disk: 1, whole: false })).1,
            [
                "The system on disk 1 is replaced. What was written was read back and is right.",
                "The tables were not touched. It runs at the next start: type reboot."
            ]
        );
        sys.disks[2] = disk(b"OTHER", 500, Holds::Partitions { count: 4, system });
        assert_eq!(run_line(&mut sys, "install").1.lines[3], "   3     OTHER                    500 MB  XilaOS and 2 other partitions");
        assert_eq!(run_line(&mut sys, "install system").1.lines, ["XilaOS is on several disks. Type install system and the number of one."]);
        assert!(run_line(&mut sys, "install system 3").0);
        assert_eq!(sys.request.take(), Some(Request::Install { disk: 3, whole: false }));
    }

    #[test]
    fn display_tells_how_the_screen_is_drawn() {
        let mut sys = System::new();
        sys.machine.screen_width = 1280;
        sys.machine.screen_height = 800;
        sys.machine.scale = 2;
        sys.display = Display { shown: Pictures::Direct, every: 0, sweeps: 0, light: None, direct: false, combining: true, whole_us: 1040, firmware_us: 31_960, pictures: 25, longest_us: 3249, city_us: 950, skipped: 2 };
        let (ok, out) = run_line(&mut sys, "display");
        assert!(ok);
        assert_eq!(
            out.lines,
            [
                "   item                value",
                "   screen              1280x800, cells of 16 pixels",
                "   screen memory       write combining",
                "   pictures change     while the screen is swept",
                "   rhythm              a picture every 4 ticks, 100 ticks a second",
                "   whole screen        1.0 ms to show, at the start",
                "   the firmware's way  32.0 ms to show, at the start",
                "   pictures shown      25 in the last second",
                "   longest picture     3.2 ms",
                "   city                1.0 ms to draw one picture",
                "   pictures skipped    2 since the start",
            ]
        );
        sys.display.combining = false;
        assert_eq!(run_line(&mut sys, "display").1.lines[2], "   screen memory       written piece by piece");
        assert_eq!(run_line(&mut sys, "display now").1.lines, ["Type display, or display with test, chip, registers, flip on, flip off, brightness."]);

        // the chip: only one XilaOS knows, and the kernel reads it
        assert_eq!(run_line(&mut sys, "display chip").1.lines, ["No graphics chip was found: the screen is the one the firmware set up."]);
        let (card, mut chip) = crate::intel::tests::t430();
        sys.card = Some(crate::intel::Card { id: 0x1111_1234, ..card });
        let (ok, out) = run_line(&mut sys, "display chip");
        assert!(!ok && sys.request.is_none());
        assert_eq!(out.lines, ["The graphics chip, 1234:1111, is not one XilaOS knows how to read."]);
        sys.card = Some(crate::intel::Card { registers: 0, ..card });
        assert_eq!(run_line(&mut sys, "display registers").1.lines, ["The registers of the graphics chip cannot be reached."]);
        sys.card = Some(card);
        let (ok, out) = run_line(&mut sys, "display chip");
        assert!(ok && out.lines.is_empty());
        assert_eq!(sys.request.take(), Some(Request::Chip { registers: false }));
        let state = crate::intel::look(crate::intel::Generation::Seven, &mut chip);
        sys.chip = Some((state, false));
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert_eq!(out.lines.len(), 18);
        assert_eq!(out.lines[3], "   picture                 1366x768, from pipe A");
        assert!(run_line(&mut sys, "display registers").0);
        assert_eq!(sys.request.take(), Some(Request::Chip { registers: true }));
        sys.chip = Some((state, true));
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert_eq!(out.lines.len(), 33);
        assert_eq!(out.lines[1], "   PIPECONF A    70008    c0000000");
        assert!(sys.chip.is_none());

        // two pictures shown in turn: asked of the kernel, which says how it went
        for (typed, wish) in [("display flip on", Pictures::Two), ("display flip off", Pictures::Direct), ("display flip try", Pictures::Trial)] {
            let (ok, out) = run_line(&mut sys, typed);
            assert!(ok && out.lines.is_empty(), "{typed}");
            assert_eq!(sys.request.take(), Some(Request::Flip(wish)));
        }
        assert!(!run_line(&mut sys, "display flip").0 && sys.request.is_none());
        let told = |sys: &mut System, answer| {
            sys.flipped = Some(answer);
            let mut out = Lines::default();
            served(sys, &mut out);
            (out.errors, out.lines.concat())
        };
        assert_eq!(told(&mut sys, Ok(Pictures::Two)), (0, "Pictures now change between two sweeps of the screen.".to_string()));
        assert_eq!(told(&mut sys, Err(crate::intel::NoFlip::Deaf)), (1, "The chip did not show the second picture: the screen is written directly.".to_string()));
        sys.display.shown = Pictures::Two;
        (sys.display.every, sys.display.sweeps) = (2, 60);
        let shown = run_line(&mut sys, "display").1.lines;
        assert_eq!(shown[3..5], ["   pictures change     between two sweeps of the screen", "   rhythm              a picture every 2 sweeps, 60 sweeps a second"]);

        // the brightness: told, or set first
        for (typed, wish) in [("display brightness", None), ("display brightness 60", Some(60)), ("display brightness  100", Some(100))] {
            assert!(run_line(&mut sys, typed).0, "{typed}");
            assert_eq!(sys.request.take(), Some(Request::Light(wish)));
        }
        for wrong in ["display brightness 0", "display brightness 101", "display brightness high", "display brightness60"] {
            let (ok, out) = run_line(&mut sys, wrong);
            assert!(!ok && sys.request.is_none() && out.lines.len() == 1, "{wrong}");
        }
        sys.lit = Some(Some(60));
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert_eq!(out.lines, ["Brightness: 60 of 100."]);
        sys.lit = Some(None);
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert_eq!((out.errors, out.lines.concat()), (1, "No chip XilaOS knows sets the brightness of this screen.".to_string()));
        // with a chip XilaOS cannot read, only the trial and going back are possible
        sys.card = None;
        assert!(!run_line(&mut sys, "display flip on").0 && sys.request.is_none());
        assert!(!run_line(&mut sys, "display brightness 50").0 && sys.request.is_none());
        assert!(run_line(&mut sys, "display flip try").0 && sys.request.take().is_some());

        // the test is the kernel's work: the command asks, `served` tells
        let (ok, out) = run_line(&mut sys, "display test");
        assert!(ok && out.lines.is_empty());
        assert_eq!(sys.request.take(), Some(Request::Measure));
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert!(out.lines.is_empty(), "nothing to tell before the kernel answers");
        sys.measured = Some(Measure { rounds: 5, drawn_us: 2449, shown_us: 12_000 });
        served(&mut sys, &mut out);
        assert_eq!(out.lines, ["The whole screen: drawn in 2.4 ms, shown in 12.0 ms (best of 5)."]);
        assert_eq!(sys.measured, None);
    }

    #[test]
    fn screens_are_listed_and_switched() {
        let mut sys = System::new();
        let (_, out) = run_line(&mut sys, "screen");
        assert_eq!(out.lines[1], " [1] [Alt]+[0]  overview   shown");
        assert_eq!(out.lines[2], " [2] [Alt]+[1]  boot log   ");
        assert_eq!(out.lines[3], " [3] [Alt]+[2]  companion  ");
        assert!(run_line(&mut sys, "2").0);
        assert_eq!(sys.screen, 1);
        assert!(run_line(&mut sys, "screen overview").0);
        assert_eq!(sys.screen, 0);
        assert!(run_line(&mut sys, "screen companion").0);
        assert_eq!(sys.screen, 2);
        sys.screen = 0;
        assert!(!run_line(&mut sys, "screen 9").0);
    }

    #[test]
    fn clear_and_reboot_are_requests() {
        let mut sys = System::new();
        assert!(run_line(&mut sys, "clear").1.cleared);
        run_line(&mut sys, "reboot");
        assert!(sys.reboot);
    }

    #[test]
    fn remote_opens_this_prompt_to_the_network_and_closes_it() {
        use crate::remote::Asked;
        let mut sys = System::new();
        assert_eq!(run_line(&mut sys, "remote").1.lines, ["   item         value", "   this prompt  closed to the network", "   address      none yet"]);
        sys.chance = 42;
        let (ok, out) = run_line(&mut sys, "remote on");
        let code = String::from_utf8(crate::remote::written(sys.remote.code().unwrap()).bytes().to_vec()).unwrap();
        assert!(ok);
        assert_eq!(out.lines, [
            format!("This prompt is open to the local network. The code is {code}."),
            "This machine has no address on the network yet. Type net to see why.".to_string(),
            "Nothing is encrypted: who sees the code can type here. remote off closes it.".to_string(),
        ]);
        // with an address, the line to type on the other machine is given whole
        (sys.net.ip, sys.net.mask) = (Some([192, 168, 1, 11]), [255, 255, 255, 0]);
        sys.ticks = 77;
        let (_, out) = run_line(&mut sys, "remote on");
        let again = String::from_utf8(crate::remote::written(sys.remote.code().unwrap()).bytes().to_vec()).unwrap();
        // a new code each time it is opened
        assert!(again != code);
        assert_eq!(out.lines[1], format!("From another machine: python3 tools/remote.py 192.168.1.11 {again}"));
        assert!(out.lines.iter().all(|line| line.len() < COLS));
        let request = format!("{again} 1 uptime");
        assert_eq!(sys.remote.asked([192, 168, 1, 168], sys.net.local([192, 168, 1, 168]), request.as_bytes()), Asked::Run(1, b"uptime"));
        assert_eq!(sys.remote.asked([10, 0, 0, 1], sys.net.local([10, 0, 0, 1]), request.as_bytes()), Asked::Nothing);
        let shown = run_line(&mut sys, "remote").1.lines;
        assert_eq!(shown[1..], [
            "   this prompt      open, on port 4000".to_string(),
            format!("   code             {again}"),
            "   address          192.168.1.11".to_string(),
            "   commands run     1".to_string(),
            "   wrong codes      0".to_string(),
            "   last asked from  192.168.1.168".to_string(),
        ]);
        // what the prompt for the network may not do: take a disk, or open itself
        sys.session = crate::events::NETWORK;
        assert_eq!(run_line(&mut sys, "install 1 yes").1.lines, ["install takes a disk: it is typed at the machine itself."]);
        assert_eq!(run_line(&mut sys, "install").1.lines, ["install takes a disk: it is typed at the machine itself."]);
        assert_eq!(run_line(&mut sys, "remote on").1.lines, ["remote on is typed at the machine itself."]);
        assert_eq!(run_line(&mut sys, "disk erase").1.lines, ["disk erase takes a disk: it is typed at the machine itself."]);
        // the form that erases, not only the one that tells what would be lost
        assert_eq!(run_line(&mut sys, "disk erase yes").1.lines, ["disk erase takes a disk: it is typed at the machine itself."]);
        assert!(run_line(&mut sys, "disk").0);
        assert_eq!(sys.remote.code().map(|code| crate::remote::written(code).bytes().to_vec()), Some(again.clone().into_bytes()));
        // it has names and blocks of its own
        assert!(run_line(&mut sys, "let seen = 3").0);
        sys.session = crate::events::SCREEN;
        assert!(!run_line(&mut sys, "say seen").0);
        // it may close the door behind itself, and anyone may
        assert_eq!(run_line(&mut sys, "remote off").1.lines, ["This prompt is closed to the network."]);
        assert_eq!(sys.remote.asked([192, 168, 1, 168], true, request.as_bytes()), Asked::Nothing);
        let (ok, out) = run_line(&mut sys, "remote maybe");
        assert!(!ok && out.lines == ["Type remote, remote on or remote off."]);
        // after too many wrong codes it says why it is closed
        run_line(&mut sys, "remote on");
        for _ in 0..crate::remote::WRONG {
            sys.remote.asked([192, 168, 1, 30], true, b"guess 1 help");
        }
        let shown = run_line(&mut sys, "remote").1.lines;
        assert_eq!(shown[1], "   this prompt   closed itself after 20 wrong codes");
        assert_eq!(shown[4], "   wrong codes   20");
    }

    #[test]
    fn web_opens_the_tables_to_browsers_and_closes_them() {
        let mut sys = System::new();
        assert_eq!(run_line(&mut sys, "web").1.lines, ["   item        value", "   the tables  closed to the network"]);
        let (ok, out) = run_line(&mut sys, "web on");
        let code = String::from_utf8(crate::remote::written(sys.web.code().unwrap()).bytes().to_vec()).unwrap();
        assert!(ok && sys.net.tcp.listens(crate::web::PORT));
        assert_eq!(out.lines[..2], [format!("The tables can be read from a browser, with the code {code}."), "This machine has no address on the network yet. Type net to see why.".to_string()]);
        (sys.net.ip, sys.net.mask) = (Some([192, 168, 1, 11]), [255, 255, 255, 0]);
        sys.ticks = 5;
        let (_, out) = run_line(&mut sys, "web on");
        let again = String::from_utf8(crate::remote::written(sys.web.code().unwrap()).bytes().to_vec()).unwrap();
        assert!(again != code);
        assert_eq!(out.lines, [
            "The tables can be read from a browser on the local network, at:".to_string(),
            format!("http://192.168.1.11/{again}/"),
            "Nothing is encrypted: who sees this address can read every table. web off closes it.".to_string(),
        ]);
        let shown = run_line(&mut sys, "web").1.lines;
        assert_eq!(shown[1..], [
            "   the tables   open to browsers of the local network".to_string(),
            format!("   at           http://192.168.1.11/{again}/"),
            "   pages given  0".to_string(),
            "   wrong codes  0".to_string(),
        ]);
        // opening it is done at the machine
        sys.session = crate::events::NETWORK;
        assert_eq!(run_line(&mut sys, "web on").1.lines, ["web on is typed at the machine itself."]);
        sys.session = crate::events::SCREEN;
        assert_eq!(run_line(&mut sys, "web off").1.lines, ["The tables are closed to the network."]);
        assert!(sys.web.code().is_none() && !sys.net.tcp.listens(crate::web::PORT));
        let (ok, out) = run_line(&mut sys, "web maybe");
        assert!(!ok && out.lines == ["Type web, web on or web off."]);
    }

    #[test]
    fn keys_are_kept_and_never_shown() {
        let mut sys = System::new();
        assert_eq!(run_line(&mut sys, "key").1.lines, ["No key is kept. To keep one: key set openrouter and the key."]);
        for wrong in ["key set", "key set openrouter", "key set open router sk", "key set 1x sk-or", "key set openrouter caf\u{e9}"] {
            let (ok, out) = run_line(&mut sys, wrong);
            assert!(!ok, "{wrong}");
            assert_eq!(out.lines, ["Type key set, a name, and the key, which holds no space: key set openrouter sk-..."], "{wrong}");
        }
        for wrong in ["key show openrouter", "key forget", "key forget open router", "key openrouter"] {
            assert_eq!(run_line(&mut sys, wrong).1.lines, ["After key: nothing, set with a name and the key, or forget with a name."], "{wrong}");
        }
        assert_eq!(run_line(&mut sys, "key forget openrouter").1.lines, ["No key is kept under that name. Type key to list them."]);
        let (ok, out) = run_line(&mut sys, "key set openrouter sk-or-v1-secret");
        assert!(ok);
        assert_eq!(out.lines, ["The key openrouter is kept. It is never shown."]);
        assert_eq!(run_line(&mut sys, "key set OpenRouter sk-or-v1-secret-two").1.lines, ["The key OpenRouter is replaced. It is never shown."]);
        run_line(&mut sys, "create table notes (label text)");
        assert_eq!(run_line(&mut sys, "key").1.lines, ["   name        key", "   OpenRouter  kept, never shown"]);
        // SQL does not know where keys are kept: nothing lists the table, reads it or changes it
        let statements = [
            "key", "tables", "select * from tables", "select * from columns", "select * from kept_keys", "select value from kept_keys", "select * from notes join kept_keys on 1 = 1",
            "drop table kept_keys", "delete from kept_keys", "insert into kept_keys values ('a', 'b')", "update kept_keys set value = 'x'", "create table kept_keys (n int)", "create table KEPT_KEYS (n int)",
            "disk", "mem", "city", "show table kept_keys", "show table KEPT_KEYS",
        ];
        for statement in statements {
            let (ok, out) = run_line(&mut sys, statement);
            assert!(!out.lines.iter().any(|line| line.contains("sk-or")), "{statement}: {:?}", out.lines);
            if statement != "key" {
                assert!(!out.lines.iter().any(|line| line.contains("kept_keys")) || !ok, "{statement} lists it: {:?}", out.lines);
            }
            if statement.to_ascii_lowercase().contains("kept_keys") {
                assert!(!ok, "{statement}");
            }
        }
        // nor does the language, which reads tables the same way
        run_line(&mut sys, "for k in select * from kept_keys");
        run_line(&mut sys, "say k");
        let (ok, out) = run_line(&mut sys, "end");
        assert!(!ok && !out.lines.iter().any(|line| line.contains("sk-or")), "{:?}", out.lines);
        assert_eq!(run_line(&mut sys, "delete from kept_keys").1.lines, ["kept_keys is kept apart by the system: SQL neither reads nor changes it."]);
        assert_eq!(run_line(&mut sys, "create table kept_keys (n int)").1.lines, ["kept_keys is a table of the system. Pick another name."]);
        assert_eq!(run_line(&mut sys, "select count(*) from tables where kind = 'stored'").1.lines[1], "   1");
        // after all that the key is still there, whole
        assert_eq!(crate::secrets::get(&sys.db, b"openrouter").unwrap().bytes(), b"sk-or-v1-secret-two");
        // from the network a key would cross it in clear: it is set at the machine
        sys.session = crate::events::NETWORK;
        assert_eq!(run_line(&mut sys, "key set other secret").1.lines, ["key set is typed at the machine itself: the network would see the key."]);
        assert_eq!(run_line(&mut sys, "key").1.lines.len(), 2);
        sys.session = crate::events::SCREEN;
        assert_eq!(run_line(&mut sys, "key forget openrouter").1.lines, ["The key openrouter is forgotten."]);
        assert_eq!(run_line(&mut sys, "key").1.lines, ["No key is kept. To keep one: key set openrouter and the key."]);
        // what a screen shows of a line that sets a key, and keeps in its history
        assert_eq!(typed(b"key set openrouter sk-or-v1-secret").bytes(), b"key set openrouter ********");
        assert_eq!(typed(b"  KEY  SET  openrouter   sk-or two words  ").bytes(), b"  KEY  SET  openrouter   ********");
        for plain in ["key set openrouter", "key forget openrouter", "key", "select 'key set a b'", "keymap fr", ""] {
            assert_eq!(typed(plain.as_bytes()).bytes(), plain.as_bytes(), "{plain}");
        }
    }

    #[test]
    fn the_companion_is_asked_and_tells_who_asked() {
        use crate::companion::{heard, tell, waits, Outcome, Stage};
        let mut sys = System::new();
        // who it is before anything was asked: what it will be
        assert_eq!(
            run_line(&mut sys, "companion").1.lines,
            [
                "   item                value",
                "   name                Xila",
                "   model               deepseek/deepseek-v4.1-flash-1017",
                "   at                  https://openrouter.ai/api/v1/chat/completions",
                "   key                 openrouter: not set, see key",
                "   now                 nothing asked",
                "   calls to the model  0",
                "   queries run for it  0",
                "   lines it ran        0",
            ]
        );
        assert_eq!(run_line(&mut sys, "companion now").1.lines, ["After companion: nothing, off or on. To change who it is: update companions set ..."]);
        // off: it is asked nothing; on again
        assert_eq!(run_line(&mut sys, "companion off").1.lines, ["The companion is off: it is asked nothing and does nothing until companion on."]);
        assert_eq!(run_line(&mut sys, "companion").1.lines[5], "   now                 off");
        assert_eq!(run_line(&mut sys, "ask hello?").1.lines, ["The companion is off. Type companion on to have it back."]);
        assert_eq!(run_line(&mut sys, "companion ON").1.lines, ["The companion is on."]);
        assert_eq!(run_line(&mut sys, "ask").1.lines, ["Type ask and what you want to know: ask how many tasks are running?"]);
        assert_eq!(run_line(&mut sys, "ask hello?").1.lines, ["No key opens the model. Type key set openrouter and the key."]);
        run_line(&mut sys, "key set openrouter sk-or-v1-secret");
        assert_eq!(run_line(&mut sys, "ask hello?").1.lines, ["This machine has no address on the network yet. Type net to see why."]);
        sys.net.ip = Some([10, 0, 2, 15]);
        // from the network the answer would not find its way back
        sys.session = crate::events::NETWORK;
        assert_eq!(run_line(&mut sys, "ask hello?").1.lines, ["ask is typed at the machine itself, or on its serial port."]);
        sys.session = crate::events::SERIAL;
        // asked: nothing is said yet, and the one who asked waits
        let (ok, out) = run_line(&mut sys, "ask how are you, in a few words?");
        assert!(ok && out.lines.is_empty());
        assert!(waits(&sys, crate::events::SERIAL) && !waits(&sys, crate::events::SCREEN));
        let mut told = Lines::default();
        assert_eq!((tell(&mut sys, crate::events::SERIAL, COLS - 1, &mut told), tell(&mut sys, crate::events::SCREEN, COLS - 1, &mut told)), (None, None));
        sys.ticks += 250;
        assert_eq!(run_line(&mut sys, "companion").1.lines[4..6], ["   key                 openrouter: kept", "   now                 answering, for 2 s"]);
        assert_eq!(run_line(&mut sys, "ask and now?").1.lines, ["The companion is still answering the last question."]);
        // the answer comes: long lines are cut at spaces, and the lines of the model are kept
        let long = "word ".repeat(30);
        let answer = format!(r#"{{"choices":[{{"message":{{"content":"Fine.\n{}end\n\nA last line with a tab\there."}}}}]}}"#, long);
        heard(&mut sys, Ok((200, answer.as_bytes())));
        assert_eq!((sys.companion.stage, sys.companion.outcome), (Stage::Idle, Some(Outcome::Said(1))));
        assert!(waits(&sys, crate::events::SERIAL));
        assert_eq!(tell(&mut sys, crate::events::SCREEN, COLS - 1, &mut told), None, "not the one who asked");
        assert_eq!(tell(&mut sys, crate::events::SERIAL, COLS - 1, &mut told), Some(true));
        assert_eq!(told.lines, ["Fine.".to_string(), "word ".repeat(15).trim_end().to_string(), "word ".repeat(15) + "end", String::new(), "A last line with a tab here.".to_string()]);
        assert!(told.lines.iter().all(|line| line.len() < COLS) && told.errors == 0);
        // told once
        assert!(!waits(&sys, crate::events::SERIAL));
        // what it says before it looks, and what it answers after, are apart; a long answer is one
        run_line(&mut sys, "ask and the notes?");
        heard(&mut sys, Ok((200, br#"{"choices":[{"message":{"content":"Let me look.","tool_calls":[{"id":"a","function":{"name":"select","arguments":"{\"query\":\"select 1\"}"}}]}}]}"#)));
        let long = "x".repeat(950);
        heard(&mut sys, Ok((200, format!(r#"{{"choices":[{{"message":{{"content":"Found: {long}"}}}}]}}"#).as_bytes())));
        let mut told = Lines::default();
        assert_eq!(tell(&mut sys, crate::events::SERIAL, COLS - 1, &mut told), Some(true));
        // (a line is cut at its last space, or where the screen ends when it has none)
        assert_eq!(told.lines[..3], ["Let me look.".to_string(), "Found:".to_string(), "x".repeat(79)]);
        assert_eq!(told.lines[2..].concat(), long);
        assert_eq!(tell(&mut sys, crate::events::SERIAL, COLS - 1, &mut told), None);
        // a question that fails is a command that fails, in the server's own words when it has some
        run_line(&mut sys, "ask hello?");
        heard(&mut sys, Ok((402, br#"{"error":{"message":"This request requires more credits than the key has left, or the account is not allowed to use this model at all today","code":402}}"#)));
        let mut told = Lines::default();
        assert_eq!(tell(&mut sys, crate::events::SERIAL, COLS - 1, &mut told), Some(false));
        assert_eq!(told.lines.len(), 2);
        assert!(told.lines[0].starts_with("The model's server refused (402): This request requires more credits") && told.errors == 2, "{:?}", told.lines);
        for why in [crate::ask::Failed::Silent, crate::ask::Failed::Name(crate::dns::Failed::NoSuchName), crate::ask::Failed::NoChance, crate::ask::Failed::Cut] {
            run_line(&mut sys, "ask hello?");
            heard(&mut sys, Err(why));
            let mut told = Lines::default();
            assert_eq!(tell(&mut sys, crate::events::SERIAL, COLS - 1, &mut told), Some(false));
            assert!(told.lines.len() == 1 && told.lines[0].len() > 20 && told.lines[0].len() < COLS, "{:?}", told.lines);
        }
        assert_eq!(run_line(&mut sys, "companion").1.lines[6], "   calls to the model  8");
        // between braces, a name or a calculation gives its value to what is asked
        run_line(&mut sys, "let limit = 50");
        assert!(run_line(&mut sys, "ask is {limit * 2} more than {limit}, {'yes' || ' or no'}? {not closed").0);
        let mut asked = Vec::new();
        let table = sys.db.find(b"talk").unwrap();
        sys.db.scan(table, &mut |fields| {
            if let xila_db::Field::Text(text) = fields[3] {
                asked.push(String::from_utf8_lossy(text).to_string());
            }
        })
        .unwrap();
        assert_eq!(asked.last().unwrap(), "is 100 more than 50, yes or no? {not closed");
        heard(&mut sys, Err(crate::ask::Failed::Silent));
        tell(&mut sys, crate::events::SERIAL, COLS - 1, &mut Lines::default());
        assert_eq!(run_line(&mut sys, "ask is {nothing} there?").1.lines, ["Between { and } goes a name that has a value, or a calculation, not: nothing"]);
        assert_eq!(run_line(&mut sys, &format!("ask {} {{limit}}", "x".repeat(148))).1.lines, ["What is asked is 150 characters at most, values included."]);
        assert_eq!(sys.companion.stage, Stage::Idle);
    }

    #[test]
    fn net_tells_of_the_card_the_cable_and_the_address() {
        let mut sys = System::new();
        assert_eq!(run_line(&mut sys, "net").1.lines, ["XilaOS drives no network card of this machine. Type devices to see what it has."]);
        assert!(!run_line(&mut sys, "net ping 10.0.2.2").0);
        // a card, and no cable
        sys.net_card = Some(NetCard { name: "Intel 82579LM", link: None, damaged: 0, missed: 0, sent: 0, refused: 0 });
        sys.net.start([0x3c, 0x97, 0x0e, 1, 2, 3], 0);
        let (ok, out) = run_line(&mut sys, "net");
        assert!(ok);
        assert_eq!(
            out.lines,
            [
                "   item                 value",
                "   card                 Intel 82579LM",
                "   its address          3c:97:0e:01:02:03",
                "   cable                not plugged in, or the other end is silent",
                "   address              none: no cable",
                "   frames received      0",
                "   frames sent          0",
                "   not sent, card busy  0",
                "   damaged on the way   0",
                "   missed, no room      0",
            ]
        );
        assert_eq!(run_line(&mut sys, "net ping 10.0.2.2").1.lines, ["This machine has no address on the network yet. Type net to see why."]);
        // the cable is in, and the network gave an address for a day
        sys.net_card = Some(NetCard { name: "Intel 82579LM", link: Some((1000, true)), damaged: 2, missed: 1, sent: 3, refused: 4 });
        assert_eq!(run_line(&mut sys, "net").1.lines[3..5], ["   cable                1000 megabits per second", "   address              none yet: asking the network for one"]);
        let mut wire = crate::net::tests::Kept::default();
        sys.net.tick(0, &mut wire);
        let exchange = u32::from_be_bytes([wire.0[0][46], wire.0[0][47], wire.0[0][48], wire.0[0][49]]);
        for kind in [2, 5] {
            let mut body = vec![0u8; 240];
            body[..4].copy_from_slice(&[2, 1, 6, 0]);
            body[4..8].copy_from_slice(&exchange.to_be_bytes());
            body[16..20].copy_from_slice(&[192, 168, 1, 20]);
            body[28..34].copy_from_slice(&[0x3c, 0x97, 0x0e, 1, 2, 3]);
            body[236..240].copy_from_slice(&[99, 130, 83, 99]);
            body.extend_from_slice(&[53, 1, kind, 54, 4, 192, 168, 1, 1, 51, 4, 0, 1, 0x51, 0x80, 1, 4, 255, 255, 255, 0, 3, 4, 192, 168, 1, 1, 255]);
            let data = crate::net::tests::udp([192, 168, 1, 1], [255; 4], 67, 68, &body);
            let frame = crate::net::tests::packet([0xff; 6], [2; 6], [192, 168, 1, 1], [255; 4], 17, &data);
            sys.net.receive(&frame, 5, &mut wire);
        }
        let (_, out) = run_line(&mut sys, "net");
        assert_eq!(
            out.lines[4..],
            [
                "   address               192.168.1.20",
                "   mask                  255.255.255.0",
                "   gateway               192.168.1.1",
                "   name server           none named by the network",
                "   given by the network  for 24 hours more",
                "   frames received       2",
                "   frames sent           3",
                "   not sent, card busy   4",
                "   damaged on the way    2",
                "   missed, no room       1",
            ]
        );
        // a ping is the kernel's work
        assert!(run_line(&mut sys, "net restart").0);
        assert_eq!(sys.request.take(), Some(Request::NetRestart));
        sys.net_restarted = Some(true);
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert_eq!(out.lines, ["The card was reset and set up again. It asks the network for an address."]);
        for wrong in ["net ping", "net ping box", "net ping 1.2.3", "net status"] {
            let (ok, out) = run_line(&mut sys, wrong);
            assert!(!ok && sys.request.is_none() && out.lines.len() == 1, "{wrong}");
        }
        assert!(run_line(&mut sys, "net ping 192.168.1.1").0);
        assert_eq!(sys.request.take(), Some(Request::Ping([192, 168, 1, 1])));
        assert!(run_line(&mut sys, "net registers").0);
        assert_eq!(sys.request.take(), Some(Request::NetRegisters));
        // a page of another machine
        for wrong in ["net get", "net get 192.168.1", "net get the mac", "net get 192.168.1.20:x"] {
            assert_eq!(run_line(&mut sys, wrong).1.lines, ["Type net get and where to ask: net get 192.168.1.20:8000/page"], "{wrong}");
        }
        assert!(run_line(&mut sys, "net get http://192.168.1.20:8000/notes/1").0);
        assert_eq!((sys.request.take(), sys.asked_machine.bytes(), sys.asked_path.bytes()), (Some(Request::Ask { to: Some([192, 168, 1, 20]), port: 8000, secret: false, post: false }), &b"192.168.1.20"[..], &b"/notes/1"[..]));
        let whole = |status, text: &[u8]| Ok(Got { status, start: FixedStr::from_bytes(text), total: text.len() });
        let said = |sys: &mut System, got| {
            sys.got = Some((8000, got));
            let mut out = Lines::default();
            // an answer that is a failure makes the command one
            assert_eq!(served(sys, &mut out), out.errors == 0);
            (out.errors, out.lines)
        };
        let text = b"first line\r\n\r\n<b>second</b> \x07\xe9\n3\n4\n5\n6\n7\n8\n9\n";
        assert_eq!(said(&mut sys, whole(200, text)), (0, vec!["192.168.1.20:8000 answered 200.".to_string(), "first line".to_string(), "<b>second</b> ??".to_string(), "3".to_string(), "4".to_string(), "5".to_string(), "6".to_string(), "7".to_string(), "8".to_string(), format!("(and more: {} bytes in all)", text.len())]));
        // more was said than the start that was kept: how much in all
        assert_eq!(said(&mut sys, Ok(Got { status: 200, start: FixedStr::from_bytes(b"one\ntwo\n"), total: 425_702 })), (0, vec!["192.168.1.20:8000 answered 200.".to_string(), "one".to_string(), "two".to_string(), "(and more: 425702 bytes in all)".to_string()]));
        assert_eq!(said(&mut sys, whole(404, b"")), (1, vec!["192.168.1.20:8000 answered 404.".to_string()]));
        assert_eq!(said(&mut sys, Err(crate::ask::Failed::Cut)), (1, vec!["192.168.1.20:8000 hung up before it had said everything.".to_string()]));
        assert_eq!(said(&mut sys, Err(crate::ask::Failed::TooLong)), (1, vec!["192.168.1.20:8000 says more than XilaOS listens to: given up.".to_string()]));
        assert_eq!(said(&mut sys, Err(crate::ask::Failed::NoAnswer)), (1, vec!["192.168.1.20:8000 does not answer.".to_string()]));
        // a machine asked by its name: the kernel finds its address first
        assert!(run_line(&mut sys, "net get Example.org:8000/notes/1").0);
        assert_eq!((sys.request.take(), sys.asked_machine.bytes()), (Some(Request::Ask { to: None, port: 8000, secret: false, post: false }), &b"Example.org"[..]));
        assert_eq!(said(&mut sys, whole(200, b"hello")), (0, vec!["Example.org:8000 answered 200.".to_string(), "hello".to_string()]));
        // through TLS: said by https:// before the place
        assert!(run_line(&mut sys, "net get https://openrouter.ai/api/v1/models").0);
        assert_eq!((sys.request.take(), sys.asked_machine.bytes(), sys.asked_path.bytes()), (Some(Request::Ask { to: None, port: 443, secret: true, post: false }), &b"openrouter.ai"[..], &b"/api/v1/models"[..]));
        // a text sent to a page: a value, as for notify
        for wrong in ["net post", "net post 192.168.1.20:8000/notes", "net post 192.168.1 'hello'", "net post 'hello'"] {
            let (ok, out) = run_line(&mut sys, wrong);
            assert!(!ok && sys.request.is_none(), "{wrong}");
            assert_eq!(out.lines, ["Type net post, where, and a text: net post 192.168.1.20:8000/notes 'hello'"], "{wrong}");
        }
        assert_eq!(run_line(&mut sys, "net post 192.168.1.20/notes hello").1.lines, ["Put the text between quotes, or give a name that has a value, not: hello"]);
        assert!(run_line(&mut sys, "net post https://Example.org/notes 'big expense: ' || 2 * 21").0);
        assert_eq!(
            (sys.request.take(), sys.asked_machine.bytes(), sys.asked_path.bytes(), sys.asked_text.bytes()),
            (Some(Request::Ask { to: None, port: 443, secret: true, post: true }), &b"Example.org"[..], &b"/notes"[..], &b"big expense: 42"[..])
        );
        assert!(run_line(&mut sys, "net post 192.168.1.20/notes ''").0);
        assert_eq!((sys.request.take(), sys.asked_text.bytes()), (Some(Request::Ask { to: Some([192, 168, 1, 20]), port: 80, secret: false, post: true }), &b""[..]));
        sys.asked_machine = FixedStr::from_bytes(b"openrouter.ai");
        use crate::ask::Failed as NoPage;
        use xila_tls::{Error as NoSecret, Refused};
        let mut unsure = vec![NoPage::NoChance, NoPage::NoClock];
        unsure.extend([Refused::Expired, Refused::NotYet, Refused::WrongName, Refused::Unknown, Refused::Unsupported, Refused::Forged, Refused::Bad].map(|why| NoPage::Secret(NoSecret::Certificate(why))));
        unsure.extend([NoSecret::NotTheirs, NoSecret::Forged, NoSecret::NotTls, NoSecret::Old, NoSecret::NoCommon, NoSecret::TooLong, NoSecret::Alert(255)].map(NoPage::Secret));
        let mut told = Vec::new();
        for why in unsure {
            let (errors, lines) = said(&mut sys, Err(why));
            // one line each, that fits the screen, says "that machine" or "this" one, and is no other's
            assert!(errors == 1 && lines.len() == 1 && lines[0].len() < COLS && lines[0].ends_with(['.', ')']), "{why:?}: {lines:?}");
            assert!(lines[0].starts_with("That machine") || lines[0].starts_with("This ") || lines[0].starts_with("No authority") || lines[0].starts_with("A signature") || lines[0].starts_with("What that machine"), "{lines:?}");
            assert!(!told.contains(&lines[0]) && lines[0] != "That machine could not be asked.", "{lines:?}");
            told.push(lines[0].clone());
        }
        assert_eq!(said(&mut sys, Err(NoPage::Secret(NoSecret::Certificate(Refused::Unknown)))).1, ["No authority that XilaOS trusts vouches for that machine's certificate."]);
        assert_eq!(said(&mut sys, Err(NoPage::Secret(NoSecret::Alert(80)))).1, ["That machine gave up before the conversation was secret (TLS alert 80)."]);
        // two numbers say what XilaOS says itself in other cases
        assert_eq!(said(&mut sys, Err(NoPage::Secret(NoSecret::Alert(40)))), said(&mut sys, Err(NoPage::Secret(NoSecret::NoCommon))));
        assert_eq!(said(&mut sys, Err(NoPage::Secret(NoSecret::Alert(70)))), said(&mut sys, Err(NoPage::Secret(NoSecret::Old))));
        use crate::dns::Failed as NoName;
        let whys = [
            (NoName::NoServer, "The network named no server to ask for names. Type net names and an address."),
            (NoName::Silent, "The name server did not answer."),
            (NoName::NoSuchName, "The name server knows no machine of that name."),
            (NoName::NoAddress, "The name server gave no address for that name."),
        ];
        for (why, text) in whys {
            assert!(text.len() <= COLS);
            assert_eq!(said(&mut sys, Err(crate::ask::Failed::Name(why))), (1, vec![text.to_string()]));
        }
        // the address of a name, by itself
        for wrong in ["net name", "net name 192.168.1.20", "net name two words", "net name a..b"] {
            let (ok, out) = run_line(&mut sys, wrong);
            assert!(!ok && sys.request.is_none(), "{wrong}");
            assert_eq!(out.lines, ["Type net name and the name of a machine: net name example.org"], "{wrong}");
        }
        assert!(run_line(&mut sys, "net name openrouter.ai").0);
        assert_eq!((sys.request.take(), sys.asked_machine.bytes()), (Some(Request::Name), &b"openrouter.ai"[..]));
        sys.named = Some(Ok([104, 18, 3, 115]));
        let mut out = Lines::default();
        assert!(served(&mut sys, &mut out));
        assert_eq!(out.lines, ["openrouter.ai is at 104.18.3.115."]);
        for (why, text) in whys {
            sys.named = Some(Err(why));
            let mut out = Lines::default();
            assert!(!served(&mut sys, &mut out));
            assert_eq!(out.lines, [text]);
        }
        // the longest name still fits in a line with what is said of it
        let long = "a.".repeat(crate::dns::NAME / 2 - 1) + "fr";
        assert!(run_line(&mut sys, &format!("net get {long}:65000/")).0);
        sys.request = None;
        sys.got = Some((65000, Ok(Got { status: 200, start: FixedStr::new(), total: 0 })));
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert_eq!(out.lines, [format!("{long}:65000 answered 200.")]);
        assert!(out.lines[0].len() <= COLS);
        assert!(run_line(&mut sys, &format!("net name {long}")).0);
        sys.request = None;
        sys.named = Some(Ok([255, 255, 255, 255]));
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert_eq!(out.lines, [format!("{long} is at 255.255.255.255.")]);
        assert!(out.lines[0].len() <= COLS);
        // who is asked for names: whom the network named, or one set by hand
        for wrong in ["net names", "net names example.org", "net names 10.0.2.2:", "net names 10.0.2.2:0", "net names 10.0.2.2:99999"] {
            assert_eq!(run_line(&mut sys, wrong).1.lines, ["Type net names and the address of a name server: net names 192.168.1.1"], "{wrong}");
        }
        sys.net.names.told([192, 168, 1, 1]);
        assert_eq!(run_line(&mut sys, "net").1.lines[7], "   name server           192.168.1.1");
        let (ok, out) = run_line(&mut sys, "net names 9.9.9.9");
        assert!(ok);
        assert_eq!(out.lines, ["Names are now asked of 9.9.9.9."]);
        assert_eq!(run_line(&mut sys, "net").1.lines[7], "   name server           9.9.9.9, set by hand");
        assert_eq!(run_line(&mut sys, "net names 10.0.2.2:5300").1.lines, ["Names are now asked of 10.0.2.2:5300."]);
        assert_eq!(run_line(&mut sys, "net").1.lines[7], "   name server           10.0.2.2:5300, set by hand");
        assert_eq!(run_line(&mut sys, "net status").1.lines, ["After net: restart, registers, ping, get, post, name or names, and what they need."]);
        let mut read = [("CTRL", 0, 0x0018_0240); NET_REGISTERS];
        read[1] = ("STATUS", 8, 0x0008_0083);
        sys.net_registers = Some(read);
        let mut out = Lines::default();
        served(&mut sys, &mut out);
        assert_eq!(out.lines[..3], ["   register  address  value", "   CTRL      00000    00180240", "   STATUS    00008    00080083"]);
        assert_eq!(out.lines.len(), NET_REGISTERS + 1);
        sys.pinged = Some(([192, 168, 1, 1], Some(420)));
        let mut out = Lines::default();
        assert!(served(&mut sys, &mut out));
        assert_eq!(out.lines, ["192.168.1.1 answered in 0.4 ms."]);
        sys.pinged = Some(([192, 168, 1, 9], None));
        let mut out = Lines::default();
        assert!(!served(&mut sys, &mut out));
        // nothing to answer: nothing failed
        assert!(served(&mut sys, &mut Lines::default()));
        assert_eq!((out.errors, out.lines.concat()), (1, "192.168.1.9 did not answer within a second.".to_string()));
    }

    #[test]
    fn show_table_writes_the_statement_that_would_make_it() {
        let mut sys = System::new();
        assert!(run_line(&mut sys, "create table notes (label text not null, stars int default 3 check (stars between 1 and 5))").0);
        let (ok, out) = run_line(&mut sys, "show table Notes");
        assert!(ok);
        assert_eq!(out.lines, ["create table notes (", "    label text not null,", "    stars int default 3,", "    check (stars between 1 and 5)", ")"]);
        // typed again, it makes the same table
        assert!(run_line(&mut sys, "drop table notes").0);
        assert!(run_line(&mut sys, &out.lines.join(" ")).0);
        assert_eq!(run_line(&mut sys, "show table notes").1.lines, out.lines);
        assert_eq!(run_line(&mut sys, "insert into notes (label) values ('rent')").1.lines, ["1 row added to notes, with id 1."]);
        assert_eq!(run_line(&mut sys, "update notes set stars = 6").1.lines, ["Refused by a check: stars between 1 and 5"]);
        // a view of the system is not made by a statement; a program may be called table
        let (ok, out) = run_line(&mut sys, "show table tasks");
        assert!(!ok && out.lines == ["tasks is a view of the system and cannot be changed."]);
        assert_eq!(run_line(&mut sys, "show table").1.lines, ["Unknown program: table. Type programs to list them."]);
    }

    #[test]
    fn devices_lists_what_the_machine_is_made_of() {
        let mut sys = System::new();
        assert_eq!(run_line(&mut sys, "devices").1.lines, ["No device was found on the PCI bus."]);
        // some of a ThinkPad T430
        let found = [((0, 0, 0), 0x0154_8086, 0x06_00_00, false), ((0, 2, 0), 0x0166_8086, 0x03_00_00, true), ((0, 0x19, 0), 0x1502_8086, 0x02_00_00, false),
            ((0, 0x1a, 0), 0x1e2d_8086, 0x0c_03_20, false), ((0, 0x1f, 2), 0x1e03_8086, 0x01_06_01, true), ((3, 0, 0), 0x0085_8086, 0x02_80_00, false)];
        for (slot, (place, id, class, driven)) in sys.devices.iter_mut().zip(found) {
            *slot = Some(Device { place, id, class, driven });
        }
        let (ok, out) = run_line(&mut sys, "devices");
        assert!(ok);
        assert_eq!(
            out.lines,
            [
                "   place    kind                     maker_model  class   driver",
                "   00:00.0  bridge to the processor  8086:0154    060000  none",
                "   00:02.0  graphics                 8086:0166    030000  XilaOS",
                "   00:19.0  Ethernet                 8086:1502    020000  none",
                "   00:1a.0  USB 2                    8086:1e2d    0c0320  none",
                "   00:1f.2  SATA disks               8086:1e03    010601  XilaOS",
                "   03:00.0  wireless network         8086:0085    028000  none",
            ]
        );
    }

    #[test]
    fn the_time_is_told_and_has_two_names() {
        let mut sys = System::new();
        assert_eq!(run_line(&mut sys, "time").1.lines, ["The clock of this machine does not tell the time."]);
        assert_eq!(run_line(&mut sys, "say today").1.lines, ["Unknown name: today"]);
        // the kernel read the clock at tick 500: 2026-11-18 14:32:07
        let then = crate::clock::Moment { year: 2026, month: 11, day: 18, hour: 14, minute: 32, second: 7 };
        sys.ticks = 500;
        sys.clock = Some((then.seconds(), 500));
        assert_eq!(run_line(&mut sys, "time").1.lines, ["Wednesday 18 November 2026, 14:32:07."]);
        // the timer counts on from there
        sys.ticks += 5399;
        assert_eq!(run_line(&mut sys, "time").1.lines, ["Wednesday 18 November 2026, 14:33:00."]);
        // in a calculation: the date, and the date with the time, as texts that sort
        assert_eq!(run_line(&mut sys, "say today").1.lines, ["2026-11-18"]);
        assert_eq!(run_line(&mut sys, "select now, today as day").1.lines[1].trim(), "2026-11-18 14:33:00  2026-11-18");
        run_line(&mut sys, "create table visits (day text, who text)");
        assert!(run_line(&mut sys, "insert into visits values (today, 'ada'), ('2026-11-17', 'bob')").0);
        assert_eq!(run_line(&mut sys, "select who from visits where day = today").1.lines[1..], ["   ada"]);
        assert_eq!(run_line(&mut sys, "select who from visits where day < today").1.lines[1..], ["   bob"]);
        // a column or a variable of that name comes first
        run_line(&mut sys, "let today = 'my own'");
        assert_eq!(run_line(&mut sys, "say today").1.lines, ["my own"]);
        assert_eq!(run_line(&mut sys, "say now").1.lines, ["2026-11-18 14:33:00"]);
    }

    #[test]
    fn uptime_counts_ticks() {
        let mut sys = System::new();
        sys.ticks = 1234;
        assert_eq!(run_line(&mut sys, "uptime").1.lines, ["Up for 12.34 s."]);
    }

    #[test]
    fn mood_energy_rises_and_settles() {
        let mut sys = System::new();
        sys.nudge(430);
        assert_eq!(&sys.meter(), b"[####......]");
        sys.settle(100);
        assert_eq!(sys.energy, 230);
        sys.mood = MoodMode::Intense;
        sys.settle(5000);
        assert_eq!(sys.energy, 900);
        sys.nudge(500); // only auto follows the typing
        assert_eq!(sys.energy, 900);
        sys.mood = MoodMode::Calm;
        sys.settle(5000);
        assert_eq!(&sys.meter(), b"[#.........]");
    }

    #[test]
    fn tasks_are_listed_started_and_stopped() {
        let mut sys = System::new();
        let task = |name: &[u8], ticks, system| TaskInfo { name: FixedStr::from_bytes(name), state: b"ready", ticks, system, used: true };
        sys.tasks[0] = task(b"interface", 1234, true);
        sys.tasks[2] = task(b"spin", 5, false);
        let (_, out) = run_line(&mut sys, "tasks");
        assert_eq!(out.lines, ["   id  name       state  ran", "   0   interface  ready  12.34 s", "   2   spin       ready  0.05 s"]);
        assert!(run_line(&mut sys, "spin").0);
        assert_eq!(sys.request.take(), Some(Request::Spin));
        assert!(run_line(&mut sys, "stop 2").0);
        assert_eq!(sys.request.take(), Some(Request::Stop(2)));
        assert_eq!(run_line(&mut sys, "stop 0").1.lines, ["The system needs that task."]);
        assert_eq!(run_line(&mut sys, "stop 5").1.lines, ["Give the id of a task. Type tasks to list them."]);
        assert_eq!(sys.request, None);
    }

    #[test]
    fn select_reads_the_live_state() {
        let mut sys = System::new();
        let task = |name: &[u8], ticks| TaskInfo { name: FixedStr::from_bytes(name), state: b"ready", ticks, system: true, used: true };
        sys.tasks[0] = task(b"interface", 12);
        sys.tasks[1] = task(b"idle", 340);
        sys.cpu_share = [50, 950, 0, 0, 0, 0, 0, 0];
        let (ok, out) = run_line(&mut sys, "select name, ran_ms, share from tasks order by share desc");
        assert!(ok);
        assert_eq!(out.lines, ["   name       ran_ms  share", "   idle       3400    95", "   interface  120     5"]);
        assert_eq!(run_line(&mut sys, "select value from settings where name = 'mood'").1.lines[1], "   auto");
        run_line(&mut sys, "mood calm");
        assert_eq!(run_line(&mut sys, "SELECT value FROM settings WHERE name = 'mood'").1.lines[1], "   calm");
        let (ok, out) = run_line(&mut sys, "select nothing from tasks");
        assert!(!ok);
        assert_eq!(out.lines, ["Unknown column: nothing"]);
    }

    #[test]
    fn tables_lists_the_catalog_and_leads_to_each_table() {
        let mut sys = System::new();
        let (ok, out) = run_line(&mut sys, "tables");
        assert!(ok);
        assert_eq!(out.lines[0], "     name      kind    rows  about");
        assert_eq!(out.lines[1], " [1] tables    system  10    all tables: views of the system, stored tables");
        assert_eq!(out.lines.len(), 11);
        let (_, out) = run_line(&mut sys, "8"); // the table of the commands
        assert_eq!(out.lines[..2], ["   name", "   about"]);
        // more commands than a result shows
        assert_eq!(out.lines.len(), 1 + TABLE_ROWS + 1);
        assert_eq!(out.lines[TABLE_ROWS + 1], format!("{} more rows are not shown. Narrow the query with WHERE.", NAMES.len() - TABLE_ROWS));
        assert_eq!(run_line(&mut sys, "select count(*) from tables").1.lines[1], "   10");
    }

    #[test]
    fn listeners_hear_of_changes_and_of_notify() {
        use crate::events::{SCREEN, SERIAL};
        let mut sys = System::new();
        let heard = |sys: &mut System, session: usize| {
            let mut all = Vec::new();
            while let Some(event) = sys.events.take(session) {
                all.push(format!("{}: {}", event.channel, event.text));
            }
            all
        };
        assert_eq!(run_line(&mut sys, "listen").1.lines, ["You listen to nothing. Try: listen name_of_a_table"]);
        assert_eq!(run_line(&mut sys, "listen notes").1.lines, ["Listening to notes."]);
        assert_eq!(run_line(&mut sys, "listen Notes").1.lines, ["Already listening to Notes."]);
        assert_eq!(run_line(&mut sys, "listen alerts").1.lines, ["Listening to alerts."]);
        assert_eq!(run_line(&mut sys, "listen").1.lines, ["   channel", "   notes", "   alerts"]);
        assert!(!run_line(&mut sys, "listen two words").0);

        // the other prompt changes the table
        sys.session = SERIAL;
        run_line(&mut sys, "create table notes (title text)");
        run_line(&mut sys, "insert into notes values ('a'), ('b')");
        run_line(&mut sys, "update notes set title = 'c' where id = 2");
        assert!(!run_line(&mut sys, "insert into notes values ('a'), (2)").0);
        run_line(&mut sys, "delete from notes where id = 1");
        run_line(&mut sys, "drop table notes");
        assert_eq!(run_line(&mut sys, "notify alerts 'the disk is full'").1.lines, ["Sent to 1 listener."]);
        assert_eq!(run_line(&mut sys, "notify alerts, 'again'").1.lines, ["Sent to 1 listener."]);
        assert_eq!(run_line(&mut sys, "notify alerts").1.lines, ["Sent to 1 listener."]);
        assert_eq!(run_line(&mut sys, "notify nobody 'hello'").1.lines, ["Nobody listens to nobody."]);
        assert!(!run_line(&mut sys, "notify alerts the disk is full").0);
        assert_eq!(heard(&mut sys, SERIAL), [] as [&str; 0]);
        assert_eq!(
            heard(&mut sys, SCREEN),
            [
                "notes: create",
                "notes: insert 1",
                "notes: insert 2",
                "notes: update 2",
                "notes: delete 1",
                "notes: drop",
                "alerts: the disk is full",
                "alerts: again",
                "alerts: ",
            ]
        );

        sys.session = SCREEN;
        assert_eq!(run_line(&mut sys, "unlisten notes").1.lines, ["No longer listening to notes."]);
        assert!(!run_line(&mut sys, "unlisten notes").0);
        assert_eq!(run_line(&mut sys, "unlisten").1.lines, ["Listening to nothing now."]);
        assert_eq!(sys.events.listeners(), 0);
    }

    #[test]
    fn stored_tables_are_made_changed_and_listed() {
        let mut sys = System::new();
        sys.ticks = 700;
        assert_eq!(run_line(&mut sys, "create table notes (title text, stars int)").1.lines, ["Table notes created."]);
        assert_eq!(run_line(&mut sys, "INSERT INTO notes VALUES ('first', 3)").1.lines, ["1 row added to notes, with id 1."]);
        assert_eq!(sys.db.tables()[0].written_at, 700);
        let (ok, out) = run_line(&mut sys, "insert into notes values (3, 'first')");
        assert!(!ok);
        assert_eq!((out.lines, out.errors), (vec!["Column title takes a 'text' between quotes.".to_string()], 1));
        let (_, out) = run_line(&mut sys, "tables");
        assert_eq!(out.lines[11].trim_end(), "[11] notes     stored  1");
        sys.ticks = 800;
        assert_eq!(run_line(&mut sys, "11").1.lines, ["   id  title  stars", "   1   first  3"]);
        assert_eq!(sys.db.tables()[0].read_at, 800);
        assert_eq!(run_line(&mut sys, "update notes set stars = 5").1.lines, ["1 row changed in notes."]);
        assert_eq!(run_line(&mut sys, "delete from notes").1.lines, ["1 row removed from notes."]);
        assert_eq!(run_line(&mut sys, "drop table notes").1.lines, ["Table notes removed, with its 0 rows."]);
    }

    #[test]
    fn a_query_marks_its_table_for_the_city() {
        let mut sys = System::new();
        sys.ticks = 500;
        run_line(&mut sys, "select * from memory");
        let memory = crate::catalog::find(b"memory").unwrap();
        assert_eq!(sys.queried[memory], 500);
        assert_eq!(sys.queried.iter().filter(|&&t| t != 0).count(), 1);
    }

    /// Memory that says it is a disk, with something written on it.
    pub(crate) struct Used(pub(crate) xila_db::Memory);

    impl xila_db::Disk for Used {
        fn name(&self) -> &'static str {
            "sata"
        }
        fn pages(&self) -> u64 {
            self.0.pages()
        }
        fn read(&mut self, page: u64, into: &mut xila_db::Page) -> Result<(), xila_db::DiskError> {
            self.0.read(page, into)
        }
        fn write(&mut self, page: u64, from: &xila_db::Page) -> Result<(), xila_db::DiskError> {
            self.0.write(page, from)
        }
        fn flush(&mut self) -> Result<(), xila_db::DiskError> {
            Ok(())
        }
    }

    #[test]
    fn a_disk_that_was_refused_is_erased_only_when_asked_twice() {
        use xila_db::Disk;
        let mut sys = System::new();
        assert_eq!(run_line(&mut sys, "disk erase").1.lines, ["There is no disk to erase."]);
        assert!(!run_line(&mut sys, "disk erase yes").0);

        // the kernel found a disk with something else on it, and kept it aside
        let mut used = Used(xila_db::Memory::new(2560));
        used.write(0, &[7; xila_db::PAGE]).unwrap();
        let Err((error, spare)) = xila_db::Database::open(xila_db::Volume::single(Box::new(used))) else {
            panic!("a disk that holds something else was opened");
        };
        assert_eq!(error, xila_db::Error::Foreign);
        sys.spare = Some(spare);
        sys.machine.disk = Text::from_bytes(b"sata partition, holds something else: not used");
        run_line(&mut sys, "create table notes (title text)");

        let (ok, out) = run_line(&mut sys, "disk");
        assert!(ok);
        assert_eq!(out.lines[1].trim(), "disk           sata partition, holds something else: not used");
        assert_eq!(out.lines[2].trim(), "stored tables  in memory, lost when the machine stops");
        assert_eq!(out.lines[6], "The disk was not used. To give it to XilaOS, type: disk erase");
        let (ok, out) = run_line(&mut sys, "disk erase");
        assert!(ok);
        assert_eq!(
            out.lines,
            [
                "This erases the 10 MB that were found on the disk and not used,",
                "and starts an empty database there. What the disk holds is lost.",
                "The tables made since the start are in memory: they are lost too.",
                "To do it, type: disk erase yes"
            ]
        );
        // nothing happened yet
        assert!(sys.spare.is_some() && sys.db.find(b"notes").is_some());
        assert!(!run_line(&mut sys, "disk erase now").0);
        assert!(sys.spare.is_some());

        let (ok, out) = run_line(&mut sys, "Disk  Erase YES");
        assert!(ok);
        assert_eq!(out.lines, ["The disk is erased. The tables are now kept on it."]);
        assert!(sys.spare.is_none() && sys.db.tables().is_empty());
        assert_eq!(sys.db.storage().disk, "sata");
        let (_, out) = run_line(&mut sys, "disk");
        assert_eq!(out.lines[1].trim(), "disk           sata, 10 MB");
        assert_eq!(out.lines[2].trim(), "stored tables  on disk, transaction 1");
        assert_eq!(out.lines[3].trim(), "room           10 MB");
        assert_eq!(out.lines.len(), 6);
        assert!(run_line(&mut sys, "create table notes (title text)").0);
        assert_eq!(run_line(&mut sys, "disk erase yes").1.lines, ["There is no disk to erase."]);
    }

    #[test]
    fn completion() {
        assert_eq!(complete(b"d").0, 5);
        assert_eq!(complete(b"he"), (1, FixedStr::from_bytes(b"help ")));
        let (n, common) = complete(b"m");
        assert_eq!((n, common.bytes()), (2, &b"m"[..]));
        assert_eq!(complete(b"c").0, 5);
        assert_eq!(complete(b"u").0, 3);
        assert_eq!(complete(b"wh"), (1, FixedStr::from_bytes(b"while ")));
        let mut sorted = NAMES;
        sorted.sort();
        assert_eq!(sorted, NAMES, "the names are kept in order");
        assert_eq!(complete(b"se"), (1, FixedStr::from_bytes(b"select ")));
        let (n, common) = complete(b"st");
        assert_eq!((n, common.bytes()), (1, &b"stop "[..]));
        assert_eq!(complete(b"zz").0, 0);
    }

    #[test]
    fn submit_and_the_prompt_give_a_program_its_values() {
        let mut sys = Box::new(System::new());
        crate::machine::tests::rooms(&mut sys, crate::jobs::PLACES + 1);
        let said = |sys: &mut System, line: &str| run_line(sys, line).1.lines;
        for line in ["program tally(to, label)", "let n = 0", "while n < to", "let n = n + 1", "end", "say label + n", "end", "let n = 3"] {
            run_line(&mut sys, line);
        }
        // worked out when it is submitted, with the names of the prompt
        assert_eq!(said(&mut sys, "submit tally(n * 2, 'did ')"), ["Job 1 submitted: tally(6, 'did ')."]);
        run_line(&mut sys, "let n = 100");
        for _ in 0..100 {
            crate::jobs::work(&mut sys, &mut Lines::default(), &mut || true);
        }
        let shown = said(&mut sys, "job 1");
        assert_eq!((shown[0].as_str(), shown.last().unwrap().as_str()), ("Job 1 is done: tally(6, 'did ')", "did 6"));
        assert_eq!(said(&mut sys, "select program from jobs"), ["   program", "   tally(6, 'did ')"]);
        assert_eq!(said(&mut sys, "select program, went from job_log"), ["   program           went", "   tally(6, 'did ')  done"]);
        assert_eq!(said(&mut sys, "say n"), ["100"]);
        // what is wrong with the call is said at once, and no job is made
        assert_eq!(said(&mut sys, "submit tally(1)"), ["tally takes 2 values: to, label."]);
        assert_eq!(said(&mut sys, "submit tally"), ["tally takes 2 values: to, label."]);
        assert_eq!(said(&mut sys, "submit tally(1, )"), ["A value is missing between the parentheses."]);
        assert_eq!(said(&mut sys, "submit tally(missing, 'x')"), ["Unknown name: missing"]);
        assert_eq!(said(&mut sys, "select count(*) from jobs"), ["   count", "   1"]);
        // called at the prompt, with or without `run`, it is the job the prompt waits for
        for (line, listed, says) in [("tally(2, 'a ')", "tally(2, 'a ')", "a 2"), ("run tally(n - 97, 'b ')", "tally(3, 'b ')", "b 3")] {
            let (ok, out) = run_line(&mut sys, line);
            assert!(ok && out.lines.is_empty(), "{line}: {:?}", out.lines);
            let at = crate::jobs::watched(&sys, crate::jobs::Watch::Screen).unwrap();
            assert_eq!(sys.jobs.list()[at].program.bytes(), listed.as_bytes());
            let mut out = Lines::default();
            while crate::jobs::outcome(&mut sys, crate::jobs::Watch::Screen).is_none() {
                crate::jobs::turn(&mut sys, at, &mut out, &mut || true);
            }
            assert_eq!(out.lines, [says]);
        }
        assert_eq!(said(&mut sys, "say n"), ["100"]);
        // one that takes none is called as before, or with nothing between parentheses
        for line in ["program plain", "say 'plain'", "end"] {
            run_line(&mut sys, line);
        }
        assert_eq!(said(&mut sys, "submit plain()"), ["Job 4 submitted: plain."]);
        assert_eq!(said(&mut sys, "submit plain"), ["Job 5 submitted: plain."]);
    }

    #[test]
    fn jobs_are_started_listed_shown_and_ended() {
        let mut sys = Box::new(System::new());
        let said = |sys: &mut System, line: &str| run_line(sys, line).1.lines;
        // a system that was given no rooms has no jobs
        assert_eq!(said(&mut sys, "jobs"), ["No job yet. submit name runs a program as one."]);
        run_line(&mut sys, "program count");
        for line in ["let n = 0", "while n < 40", "let n = n + 1", "say n", "end", "end"] {
            run_line(&mut sys, line);
        }
        let (ok, out) = run_line(&mut sys, "submit count");
        assert_eq!((ok, out.lines), (false, vec!["This machine has no room for jobs.".to_string()]));
        crate::machine::tests::rooms(&mut sys, crate::jobs::PLACES + 1);
        // what is typed wrong
        assert_eq!(said(&mut sys, "submit"), ["Type submit and a program: submit report, submit restock('cable', 5)"]);
        assert_eq!(said(&mut sys, "submit count now"), ["Type submit and a program: submit report, submit restock('cable', 5)"]);
        assert_eq!(said(&mut sys, "submit count(1)"), ["count takes no value."]);
        assert_eq!(said(&mut sys, "submit nowhere"), ["Unknown program: nowhere. Type programs to list them."]);
        assert_eq!(said(&mut sys, "job"), ["Type job and its number: job 3, job 3 end, job 3 hold, job 3 release"]);
        assert_eq!(said(&mut sys, "job 9"), ["There is no job 9. Type jobs to list them."]);
        // submitted: the prompt has its answer at once, and the job runs when it is given turns
        assert_eq!(run_line(&mut sys, "submit count").1.lines, ["Job 1 submitted: count."]);
        assert_eq!(said(&mut sys, "submit count"), ["Job 2 submitted: count."]);
        for _ in 0..30 {
            crate::jobs::work(&mut sys, &mut Lines::default(), &mut || true);
        }
        let listed = said(&mut sys, "jobs");
        assert_eq!(listed[0].split_whitespace().collect::<Vec<_>>(), ["job", "program", "state", "by", "since", "statements"]);
        assert_eq!(listed[1].split_whitespace().collect::<Vec<_>>()[..5], ["[1]", "1", "count", "running", "screen"]);
        assert_eq!(listed.len(), 3);
        // the number of a row shows that job
        let shown = said(&mut sys, "2");
        assert_eq!(shown[0], "Job 2 is running: count");
        assert!(shown[1].starts_with("Started at the screen. ") && shown[1].ends_with(" statements run."), "{shown:?}");
        assert_eq!(shown[2], "1");
        // held, released, ended
        assert_eq!(said(&mut sys, "job 2 hold"), ["Job 2 is held."]);
        assert_eq!(said(&mut sys, "job 2")[0], "Job 2 is held: count");
        assert_eq!(said(&mut sys, "job 2 hold"), ["Job 2 is held already."]);
        assert_eq!(said(&mut sys, "job 2 release"), ["Job 2 goes on."]);
        assert_eq!(said(&mut sys, "job 2 end"), ["Job 2 is stopped."]);
        let (ok, out) = run_line(&mut sys, "job 2 end");
        assert_eq!((ok, out.lines), (false, vec!["Job 2 has ended.".to_string()]));
        assert_eq!(said(&mut sys, "job 2 dance"), ["Type job and its number: job 3, job 3 end, job 3 hold, job 3 release"]);
        // the other runs to its end: its last lines are kept, and how it went
        for _ in 0..1000 {
            crate::jobs::work(&mut sys, &mut Lines::default(), &mut || true);
        }
        let shown = said(&mut sys, "job 1");
        assert_eq!(shown[0], "Job 1 is done: count");
        assert_eq!((shown.len(), shown.last().unwrap().as_str()), (42, "40"));
        assert_eq!(said(&mut sys, "select job, went from job_log order by job"), ["   job  went", "   1    done", "   2    stopped"]);
        // SQL sees them as a view
        assert_eq!(said(&mut sys, "select job, program, state, by from jobs where state = 'stopped'"), ["   job  program  state    by", "   2    count    stopped  screen"]);
        assert_eq!(said(&mut sys, "select count(*) from jobs"), ["   count", "   2"]);
        // a job is who started it: from the network, what is refused there is refused to it
        run_line(&mut sys, "program opens");
        run_line(&mut sys, "web on");
        run_line(&mut sys, "end");
        sys.session = crate::events::NETWORK;
        assert_eq!(said(&mut sys, "submit opens"), ["Job 3 submitted: opens."]);
        sys.session = crate::events::SCREEN;
        for _ in 0..10 {
            crate::jobs::work(&mut sys, &mut Lines::default(), &mut || true);
        }
        let shown = said(&mut sys, "job 3");
        assert_eq!(shown[0], "Job 3 has failed: opens");
        assert_eq!(shown[1..], ["Started at the network prompt. 1 statement run.", "web on is typed at the machine itself.", "Stopped at line 1 of opens."]);
        assert!(sys.web.code().is_none());
    }

    #[test]
    fn a_job_is_told_of_in_plain_words_and_leaves_the_prompt_its_list() {
        let mut sys = Box::new(System::new());
        crate::machine::tests::rooms(&mut sys, 3);
        let said = |sys: &mut System, line: &str| run_line(sys, line).1.lines;
        for line in ["program lists", "programs", "select 1 as a, 'two' as b", "say 1 / 0", "end", "tables", "submit lists"] {
            run_line(&mut sys, line);
        }
        for _ in 0..20 {
            crate::jobs::work(&mut sys, &mut Lines::default(), &mut || true);
        }
        // the list the prompt was shown is still the one its numbers follow
        assert_eq!(said(&mut sys, "3")[0].trim(), "id  name  state  ran_ms  share");
        let shown = said(&mut sys, "job 1");
        assert_eq!(shown[0], "Job 1 has failed: lists");
        assert_eq!(shown[1], "Started at the screen. 3 statements run.");
        // a table it showed is in columns, not in tabs
        assert!(shown.iter().any(|line| line == "a  b") && shown.iter().any(|line| line == "1  two"), "{shown:?}");
        assert!(!shown.iter().any(|line| line.contains('\t')));
        for (line, wanted) in [("say 1", "Job 2 is done: say 1"), ("while 1 = 1", "Job 3 is running: while 1 = 1")] {
            use crate::lang::Line;
            let lines: Vec<Line> = if line == "say 1" { vec![Line::from_bytes(line.as_bytes())] } else { vec![Line::from_bytes(line.as_bytes()), Line::from_bytes(b"end")] };
            crate::jobs::begin(&mut sys, crate::jobs::What::Lines(&lines), crate::jobs::Watch::Nobody, &mut Lines::default()).unwrap();
            for _ in 0..2 {
                crate::jobs::work(&mut sys, &mut Lines::default(), &mut || true);
            }
            let number = wanted.split_whitespace().nth(1).unwrap().to_string();
            assert_eq!(said(&mut sys, &format!("job {number}"))[0], wanted);
        }
        assert_eq!(said(&mut sys, "job 2")[1], "Started at the screen. 1 statement run.");
        assert_eq!(said(&mut sys, "job 3 hold"), ["Job 3 is held."]);
        assert_eq!(said(&mut sys, "job 3")[0], "Job 3 is held: while 1 = 1");
        assert_eq!(said(&mut sys, "job 3 end"), ["Job 3 is stopped."]);
        assert_eq!(said(&mut sys, "job 3")[0], "Job 3 was stopped: while 1 = 1");
        // a program that runs by itself is ended, not held: held, it would keep all the others from running
        for line in ["on notify alerts", "while 1 = 1", "end", "end", "notify alerts"] {
            run_line(&mut sys, line);
        }
        crate::lang::background(&mut sys, &mut Lines::default());
        assert_eq!(said(&mut sys, "job 4 hold"), ["Job 4 is a program that runs by itself: it is ended, not held."]);
        assert_eq!(said(&mut sys, "job 4 end"), ["Job 4 is stopped."]);
        // the keys that stop a program and send it aside are in the help of the keys
        let keys = said(&mut sys, "help k").join("\n");
        assert!(keys.contains("[Esc]") && keys.contains("[Alt]+[B]"), "{keys}");
    }
}
