//! The tables, read from a browser on another machine of the local network.
//!
//! Everything in XilaOS is a table, so a page that lists the tables and
//! shows the rows of each is a view of the whole system. It only reads:
//! all it ever runs is `select`, on the system as it is, and it changes
//! nothing, not even what the prompts remember.
//!
//! Like the prompt for the network (`remote.rs`), it is closed when the
//! machine starts. `web on`, typed at the machine, opens it and shows an
//! address that holds a code; `web off` closes it. What protects it, and
//! what does not:
//! - only machines of the same local network are answered;
//! - a page is given only at an address that starts with the code; any
//!   other gets "not found", which says nothing of what is there;
//! - after twenty addresses with a wrong code it closes itself;
//! - but nothing is encrypted. The address and the pages cross the
//!   network as they are, and a machine that can listen to that network
//!   can read both. It is for a network one trusts.
//!
//! The pages are HTTP as it was first made: a browser connects, asks for
//! one page in a few lines of text, gets it, and the conversation closes.
//! One browser is served at a time; the others wait their turn, which is
//! short. A page is built whole in memory before any of it is sent.

use core::fmt::Write;

use crate::net::Ip;
use crate::remote::{draw, same, Code};
use crate::shell::{System, Table};
use crate::tcp::{Id, Tcp};
use crate::text::FixedStr;

/// The port browsers ask on.
pub const PORT: u16 = 80;
/// The longest page, with what is said before it.
const MOST: usize = 48 * 1024;
/// Room kept before a page for what is said before it: that it was
/// found, what it is, how long it is.
const BEFORE: usize = 256;
/// What a browser asks, as far as it is read: its first line is all that counts.
const ASKED: usize = 512;
/// Ticks a browser has to say what it wants.
const PATIENCE: u64 = 500;
/// Wrong codes after which it closes itself.
pub const WRONG: u32 = 20;

/// A page, as it goes to the browser.
pub struct Page {
    bytes: [u8; MOST],
    /// Where what is to be sent starts and ends, and how far it is sent.
    from: usize,
    len: usize,
    sent: usize,
}

impl Page {
    pub const fn new() -> Page {
        Page { bytes: [0; MOST], from: BEFORE, len: BEFORE, sent: BEFORE }
    }

    /// The page as it is sent: what is said before it, then the page.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[self.from..self.len]
    }

    fn start(&mut self) {
        (self.from, self.len, self.sent) = (BEFORE, BEFORE, BEFORE);
    }

    /// Adds text that is already HTML.
    fn html(&mut self, text: &[u8]) {
        let room = (MOST - self.len).min(text.len());
        self.bytes[self.len..self.len + room].copy_from_slice(&text[..room]);
        self.len += room;
    }

    /// Adds text as it is to be read: the characters HTML gives a meaning
    /// to are written so that they have none. Characters beyond the plain
    /// ones become `?`: the system's own set is not the browser's.
    fn text(&mut self, text: &[u8]) {
        for &byte in text {
            match byte {
                b'&' => self.html(b"&amp;"),
                b'<' => self.html(b"&lt;"),
                b'>' => self.html(b"&gt;"),
                b'"' => self.html(b"&quot;"),
                0x20..=0x7e => self.html(&[byte]),
                _ => self.html(b"?"),
            }
        }
    }

    /// What every page starts with: its title and its look, and the way
    /// back to the list of tables.
    fn head(&mut self, code: &[u8], title: &[u8]) {
        self.html(b"<!doctype html><html><head><meta charset=\"utf-8\"><title>XilaOS");
        if !title.is_empty() {
            self.html(b" - ");
            self.text(title);
        }
        // no little picture for the tab: a browser would ask for one at each page
        self.html(b"</title><link rel=\"icon\" href=\"data:,\"><style>");
        self.html(b"body{background:#0b1020;color:#cfd8e6;font:15px/1.4 ui-monospace,Menlo,Consolas,monospace;margin:2em}");
        self.html(b"h1{font-size:15px;color:#8fb7ff;margin:0 0 1.2em}h1 a{color:inherit;text-decoration:none}");
        self.html(b"a{color:#7fe0c0}table{border-collapse:collapse}");
        self.html(b"th,td{text-align:left;padding:.1em 1.6em .1em 0;white-space:pre}");
        self.html(b"th{color:#7d8aa3;font-weight:normal;border-bottom:1px solid #2a3350}");
        self.html(b"p{color:#7d8aa3}.no{color:#ff9d7a}");
        self.html(b"</style></head><body><h1><a href=\"/");
        self.html(code);
        self.html(b"/\">XilaOS</a>");
        if !title.is_empty() {
            self.html(b" / ");
            self.text(title);
        }
        self.html(b"</h1>");
    }

    /// A table. With `link`, each cell of the first column leads to the
    /// page of the table it names.
    fn table(&mut self, table: &Table, link: Option<&[u8]>) {
        self.html(b"<table><tr>");
        for col in 0..table.cols {
            self.html(b"<th>");
            self.text(table.header[col].bytes());
            self.html(b"</th>");
        }
        self.html(b"</tr>");
        for row in 0..table.rows {
            self.html(b"<tr>");
            for col in 0..table.cols {
                let cell = table.cells[row][col].bytes();
                self.html(b"<td>");
                match link {
                    Some(code) if col == 0 => {
                        self.html(b"<a href=\"/");
                        self.html(code);
                        self.html(b"/t/");
                        self.text(cell);
                        self.html(b"\">");
                        self.text(cell);
                        self.html(b"</a>");
                    }
                    _ => self.text(cell),
                }
                self.html(b"</td>");
            }
            self.html(b"</tr>");
        }
        self.html(b"</table>");
        let mut count = FixedStr::<80>::new();
        let _ = match (table.rows, table.more) {
            (0, _) => write!(count, "<p>No row.</p>"),
            (1, 0) => write!(count, "<p>1 row.</p>"),
            (rows, 0) => write!(count, "<p>{rows} rows.</p>"),
            (rows, more) => write!(count, "<p>The first {rows} rows of {}.</p>", rows + more),
        };
        self.html(count.bytes());
    }

    /// Ends the page and writes before it what a browser must be told
    /// first: whether it was found, what it is and how long.
    fn end(&mut self, status: &str) {
        self.html(b"</body></html>\n");
        let mut before = FixedStr::<BEFORE>::new();
        let _ = write!(before, "HTTP/1.0 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n", self.len - BEFORE);
        // not kept by the browser, and the address is not told to other sites
        before.push_bytes(b"Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n");
        self.from = BEFORE - before.len();
        self.bytes[self.from..BEFORE].copy_from_slice(before.bytes());
        self.sent = self.from;
    }

    /// A page that only says something went wrong, without saying more
    /// than the browser needs.
    fn refusal(&mut self, status: &str, words: &[u8]) {
        self.start();
        self.html(b"<!doctype html><html><head><meta charset=\"utf-8\"><title>");
        self.text(status.as_bytes());
        self.html(b"</title><link rel=\"icon\" href=\"data:,\"></head><body><p>");
        self.text(words);
        self.html(b"</p>");
        self.end(status);
    }
}

/// The browser being served.
struct Serving {
    id: Id,
    since: u64,
    asked: [u8; ASKED],
    asked_len: usize,
    /// Its page is built and on its way.
    answered: bool,
}

/// The state of the pages for the network.
pub struct Web {
    code: Option<Code>,
    serving: Option<Serving>,
    /// Pages given since it was opened, wrong codes since then, and who
    /// asked last with the right one.
    pub served: u32,
    pub wrong: u32,
    pub from: Option<Ip>,
    /// It closed itself after too many wrong codes.
    pub gave_up: bool,
}

/// What a browser asked for, once its words are read.
#[derive(PartialEq, Eq, Debug)]
enum Wanted<'a> {
    /// The list of the tables.
    Tables,
    /// The rows of one.
    Rows(&'a [u8]),
    /// Something that is not there, with or without the right code.
    Nothing,
    /// An address that starts with a code which is not ours.
    WrongCode,
    /// Not a question this answers: it only gives pages.
    NotAsked,
}

/// Reads the first line of what a browser sent.
fn wanted<'a>(asked: &'a [u8], code: Code) -> Wanted<'a> {
    let line = asked.split(|byte| *byte == b'\r' || *byte == b'\n').next().unwrap_or(b"");
    let mut words = line.split(|byte| *byte == b' ').filter(|word| !word.is_empty());
    let (Some(how), Some(path)) = (words.next(), words.next()) else {
        return Wanted::NotAsked;
    };
    if how != b"GET" {
        return Wanted::NotAsked;
    }
    // what follows a question mark is not part of where
    let path = path.split(|byte| *byte == b'?').next().unwrap_or(b"");
    let mut parts = path.strip_prefix(b"/").unwrap_or(path).splitn(2, |byte| *byte == b'/');
    let (first, rest) = (parts.next().unwrap_or(b""), parts.next());
    if !same(code, first) {
        // Only what has the shape of a code counts as a wrong one: a
        // browser asks for other things by itself, and those are simply
        // not there.
        let shaped = first.iter().filter(|byte| **byte != b'-').count() == code.len() && first.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-');
        return if shaped { Wanted::WrongCode } else { Wanted::Nothing };
    }
    match rest {
        None | Some(b"") => Wanted::Tables,
        Some(rest) => match rest.strip_prefix(b"t/") {
            // a name of a table is letters, digits and underscores: nothing else is passed on
            Some(name) if !name.is_empty() && name.len() <= xila_db::MAX_NAME && name.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') => Wanted::Rows(name),
            _ => Wanted::Nothing,
        },
    }
}

impl Web {
    pub const fn new() -> Web {
        Web { code: None, serving: None, served: 0, wrong: 0, from: None, gave_up: false }
    }

    /// Opens the pages with a new code drawn from `chance`. None if the
    /// network cannot listen for browsers.
    pub fn open(&mut self, chance: u64, tcp: &mut Tcp) -> Option<Code> {
        self.close(tcp);
        if !tcp.listen(PORT) {
            return None;
        }
        let code = draw(chance);
        *self = Web { code: Some(code), ..Web::new() };
        Some(code)
    }

    /// Closes the pages: the browser being served is cut short, and no
    /// other is answered.
    pub fn close(&mut self, tcp: &mut Tcp) {
        if let Some(serving) = self.serving.take() {
            tcp.abort(serving.id);
        }
        if self.code.take().is_some() {
            tcp.unlisten(PORT);
        }
    }

    /// The code, while the pages are open.
    pub fn code(&self) -> Option<Code> {
        self.code
    }
}

/// Builds the page a browser asked for.
fn build(sys: &mut System, asked: &[u8], from: Ip, page: &mut Page) {
    let Some(code) = sys.web.code else {
        return page.refusal("404 Not Found", b"Not found.");
    };
    let shown = crate::remote::written(code);
    let wanted = wanted(asked, code);
    match wanted {
        Wanted::NotAsked => page.refusal("405 Method Not Allowed", b"This only gives pages."),
        Wanted::Nothing => page.refusal("404 Not Found", b"Not found."),
        Wanted::WrongCode => {
            sys.web.wrong += 1;
            if sys.web.wrong >= WRONG {
                sys.web.gave_up = true;
            }
            page.refusal("404 Not Found", b"Not found.");
        }
        Wanted::Tables => {
            (sys.web.served, sys.web.from) = (sys.web.served + 1, Some(from));
            page.start();
            page.head(shown.bytes(), b"");
            match crate::sql::select(&*sys, b"select name, kind, rows, about from tables") {
                Ok(tables) => page.table(&tables, Some(shown.bytes())),
                Err(why) => {
                    page.html(b"<p class=\"no\">");
                    page.text(why.bytes());
                    page.html(b"</p>");
                }
            }
            page.end("200 OK");
        }
        Wanted::Rows(name) => {
            (sys.web.served, sys.web.from) = (sys.web.served + 1, Some(from));
            let mut query = FixedStr::<80>::from_bytes(b"select * from ");
            query.push_bytes(name);
            page.start();
            page.head(shown.bytes(), name);
            match crate::sql::select(&*sys, query.bytes()) {
                Ok(rows) => {
                    page.table(&rows, None);
                    page.end("200 OK");
                }
                Err(why) => {
                    page.html(b"<p class=\"no\">");
                    page.text(why.bytes());
                    page.html(b"</p>");
                    page.end("404 Not Found");
                }
            }
        }
    }
}

/// Does a little of what the pages have to do: takes the next browser,
/// reads what it asks, builds its page, sends it as there is room, and
/// closes. To be called often; `Net::speak` then sends what was written.
pub fn step(sys: &mut System, page: &mut Page) {
    if sys.web.code.is_none() {
        return;
    }
    if sys.web.gave_up {
        // twenty wrong codes: the door shuts, and says why to who asks at the machine
        sys.web.close(&mut sys.net.tcp);
        sys.web.gave_up = true;
        return;
    }
    let now = sys.ticks;
    if sys.web.serving.is_none() {
        let Some(id) = sys.net.tcp.accept(PORT) else {
            return;
        };
        // from beyond this network: not answered, in any way
        if !sys.net.local(sys.net.tcp.peer(id).0) {
            sys.net.tcp.abort(id);
            return;
        }
        sys.web.serving = Some(Serving { id, since: now, asked: [0; ASKED], asked_len: 0, answered: false });
    }
    let Some(serving) = &mut sys.web.serving else {
        return;
    };
    let id = serving.id;
    let tcp = &mut sys.net.tcp;
    if !serving.answered {
        let count = tcp.read(id, &mut serving.asked[serving.asked_len..]);
        serving.asked_len += count;
        let asked = &serving.asked[..serving.asked_len];
        // a browser ends what it asks with an empty line; its first line is enough
        let whole = asked.windows(4).any(|four| four == b"\r\n\r\n") || asked.windows(2).any(|two| two == b"\n\n") || serving.asked_len == ASKED;
        if !whole {
            // gone, or too slow to say what it wants
            if tcp.ended(id) || now >= serving.since + PATIENCE {
                tcp.abort(id);
                sys.web.serving = None;
            }
            return;
        }
        serving.answered = true;
        let (asked, asked_len, from) = (serving.asked, serving.asked_len, tcp.peer(id).0);
        build(sys, &asked[..asked_len], from, page);
    }
    // the rest of what it says is of no use, and must not fill the memory kept for it
    let mut rest = [0u8; 256];
    while sys.net.tcp.read(id, &mut rest) > 0 {}
    if !sys.net.tcp.alive(id) {
        sys.net.tcp.close(id);
        sys.web.serving = None;
        return;
    }
    page.sent += sys.net.tcp.write(id, &page.bytes[page.sent..page.len]);
    if page.sent == page.len {
        sys.net.tcp.close(id);
        sys.web.serving = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: Code = *b"K7QD2MXP";

    #[test]
    fn what_a_browser_asks_is_read_from_its_first_line() {
        assert_eq!(wanted(b"GET /K7QD-2MXP/ HTTP/1.1\r\nHost: 192.168.1.11\r\n\r\n", CODE), Wanted::Tables);
        assert_eq!(wanted(b"GET /k7qd2mxp HTTP/1.0\r\n\r\n", CODE), Wanted::Tables);
        assert_eq!(wanted(b"GET /K7QD-2MXP/t/tasks HTTP/1.1\r\n\r\n", CODE), Wanted::Rows(b"tasks"));
        assert_eq!(wanted(b"GET /K7QD-2MXP/t/kept_settings?x=1 HTTP/1.1\r\n\r\n", CODE), Wanted::Rows(b"kept_settings"));
        // what is not a name of a table is not passed on to anything
        // (a space ends an address: a browser writes one as %20)
        for path in ["t/", "t/tasks;drop", "t/tasks/x", "t/a%20b", "t/'", "x", "t", "tasks", "t/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"] {
            let asked = std::format!("GET /K7QD-2MXP/{path} HTTP/1.1\r\n\r\n");
            assert_eq!(wanted(asked.as_bytes(), CODE), Wanted::Nothing, "{path}");
        }
        // a wrong code is one that has the shape of a code
        assert_eq!(wanted(b"GET /AAAA-AAAA/ HTTP/1.1\r\n\r\n", CODE), Wanted::WrongCode);
        assert_eq!(wanted(b"GET /K7QD2MXQ/t/tasks HTTP/1.1\r\n\r\n", CODE), Wanted::WrongCode);
        // what a browser asks by itself is not
        for path in ["/", "/favicon.ico", "/robots.txt", "", "/K7QD-2MX/", "/K7QD-2MXPP/", "/apple-touch-icon.png"] {
            let asked = std::format!("GET {path} HTTP/1.1\r\n\r\n");
            assert_eq!(wanted(asked.as_bytes(), CODE), Wanted::Nothing, "{path}");
        }
        // it only gives pages
        for asked in ["POST /K7QD-2MXP/ HTTP/1.1\r\n\r\n", "DELETE /K7QD-2MXP/t/tasks HTTP/1.1\r\n\r\n", "get /K7QD-2MXP/ HTTP/1.1", "", "GET", "\r\n\r\n"] {
            assert_eq!(wanted(asked.as_bytes(), CODE), Wanted::NotAsked, "{asked}");
        }
    }

    fn page_of(sys: &mut System, asked: &str) -> std::string::String {
        let mut page = std::boxed::Box::new(Page::new());
        build(sys, asked.as_bytes(), [192, 168, 1, 168], &mut page);
        std::string::String::from_utf8(page.bytes().to_vec()).unwrap()
    }

    fn opened() -> std::boxed::Box<System> {
        let mut sys = std::boxed::Box::new(System::new());
        let sys_ref = &mut *sys;
        let code = sys_ref.web.open(1, &mut sys_ref.net.tcp);
        assert!(code.is_some() && sys_ref.net.tcp.listens(PORT));
        sys_ref.web.code = Some(CODE);
        sys
    }

    #[test]
    fn the_first_page_lists_the_tables_with_a_link_to_each() {
        let mut sys = opened();
        crate::shell::tests::run_line(&mut sys, "create table notes (title text, stars int)");
        let page = page_of(&mut sys, "GET /K7QD-2MXP/ HTTP/1.1\r\nHost: x\r\n\r\n");
        let (before, body) = page.split_once("\r\n\r\n").unwrap();
        assert!(before.starts_with("HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n"), "{before}");
        assert!(before.contains(&std::format!("Content-Length: {}\r\n", body.len())), "{before} / {}", body.len());
        assert!(before.contains("Cache-Control: no-store") && before.contains("Connection: close"));
        assert!(body.starts_with("<!doctype html>") && body.ends_with("</body></html>\n"));
        assert!(body.contains("<th>name</th><th>kind</th><th>rows</th><th>about</th>"));
        assert!(body.contains("<td><a href=\"/K7QD-2MXP/t/tasks\">tasks</a></td><td>system</td>"), "{body}");
        assert!(body.contains("<td><a href=\"/K7QD-2MXP/t/notes\">notes</a></td>"));
        assert_eq!((sys.web.served, sys.web.wrong, sys.web.from), (1, 0, Some([192, 168, 1, 168])));
    }

    #[test]
    fn the_page_of_a_table_shows_its_rows_as_they_are() {
        let mut sys = opened();
        crate::shell::tests::run_line(&mut sys, "create table notes (title text, stars int)");
        crate::shell::tests::run_line(&mut sys, "insert into notes values ('<b>bold & \"quoted\"</b>', 3)");
        crate::shell::tests::run_line(&mut sys, "insert into notes values ('plain', 5)");
        let page = page_of(&mut sys, "GET /K7QD-2MXP/t/notes HTTP/1.1\r\n\r\n");
        assert!(page.starts_with("HTTP/1.0 200 OK\r\n"));
        assert!(page.contains("<title>XilaOS - notes</title>"));
        assert!(page.contains("<th>title</th><th>stars</th>"));
        // what a row holds is shown, never run by the browser
        assert!(page.contains("<td>&lt;b&gt;bold &amp; &quot;quoted&quot;&lt;/b&gt;</td><td>3</td>"), "{page}");
        assert!(page.contains("<td>plain</td><td>5</td>") && page.contains("<p>2 rows.</p>") && !page.contains("<b>"));
        // reading changes nothing: the rows are as they were
        assert_eq!(crate::shell::tests::run_line(&mut sys, "select count(*) from notes").1.lines[1].trim(), "2");
        // more rows than a page shows
        crate::shell::tests::run_line(&mut sys, "create table many (n int)");
        for n in 0..40 {
            crate::shell::tests::run_line(&mut sys, &std::format!("insert into many values ({n})"));
        }
        let page = page_of(&mut sys, "GET /K7QD-2MXP/t/many HTTP/1.1\r\n\r\n");
        assert!(page.contains("<p>The first 32 rows of 40.</p>"), "{}", &page[page.len() - 200..]);
        let page = page_of(&mut sys, "GET /K7QD-2MXP/t/notes HTTP/1.1\r\n\r\n");
        assert!(page.contains("<p>2 rows.</p>") && !page.contains("of 40"));
    }

    #[test]
    fn what_is_not_there_is_not_found_and_says_no_more() {
        let mut sys = opened();
        let missing = page_of(&mut sys, "GET /K7QD-2MXP/t/nothing HTTP/1.1\r\n\r\n");
        assert!(missing.starts_with("HTTP/1.0 404 Not Found\r\n") && missing.contains("class=\"no\""), "{missing}");
        // without the code, the answer is the same whatever was asked: it tells nothing of what is here
        let plain = page_of(&mut sys, "GET / HTTP/1.1\r\n\r\n");
        assert!(plain.starts_with("HTTP/1.0 404 Not Found\r\n") && plain.contains("<p>Not found.</p>"));
        assert_eq!(page_of(&mut sys, "GET /favicon.ico HTTP/1.1\r\n\r\n"), plain);
        assert_eq!(page_of(&mut sys, "GET /AAAA-AAAA/t/tasks HTTP/1.1\r\n\r\n"), plain);
        assert!(!plain.contains("XilaOS") && !plain.contains("tasks"));
        assert_eq!((sys.web.wrong, sys.web.gave_up), (1, false));
        assert!(page_of(&mut sys, "POST /K7QD-2MXP/ HTTP/1.1\r\n\r\n").starts_with("HTTP/1.0 405 Method Not Allowed\r\n"));
        // twenty wrong codes, and it gives up
        for _ in 1..WRONG {
            page_of(&mut sys, "GET /AAAA-AAAA/ HTTP/1.1\r\n\r\n");
        }
        assert!(sys.web.gave_up);
        let mut page = std::boxed::Box::new(Page::new());
        step(&mut sys, &mut page);
        assert!(sys.web.code().is_none() && sys.web.gave_up && !sys.net.tcp.listens(PORT));
    }
}
