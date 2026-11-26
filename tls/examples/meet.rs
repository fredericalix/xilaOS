//! Calls a server from this computer through xila-tls, asks it for a
//! page and prints the start of what it says. It is how the crate is
//! tried against real servers, and against OpenSSL in `tools/check.py`,
//! without starting XilaOS:
//!
//! ```sh
//! cargo run -p xila-tls --example meet -- openrouter.ai
//! cargo run -p xila-tls --example meet -- xila.test --at 127.0.0.1:4433 --root tls/fixtures/root.der
//! ```
//!
//! `--at` says where to call when the name is not to be looked up,
//! `--root` trusts the authority of that file instead of the public ones
//! (it may be given several times), `--days N` moves the clock, and a
//! last word that starts with `/` is the page.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use xila_tls::{anchor_from_trusted_cert, public_roots, CertificateDer, Client, State, TrustAnchor};

fn main() {
    let mut words = std::env::args().skip(1);
    let (mut name, mut at, mut path, mut days) = (None, None, "/".to_string(), 0i64);
    let mut roots: Vec<TrustAnchor<'static>> = Vec::new();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--at" => at = words.next(),
            "--days" => days = words.next().and_then(|days| days.parse().ok()).expect("--days and a number"),
            "--root" => {
                let file: &'static [u8] = Box::leak(std::fs::read(words.next().expect("--root and a file")).expect("a file that can be read").into_boxed_slice());
                let kept: &'static CertificateDer<'static> = Box::leak(Box::new(CertificateDer::from(file)));
                roots.push(anchor_from_trusted_cert(kept).expect("a certificate of an authority"));
            }
            page if page.starts_with('/') => path = page.to_string(),
            _ => name = Some(word),
        }
    }
    let name = name.expect("the name of a server");
    let roots: &'static [TrustAnchor<'static>] = if roots.is_empty() { public_roots() } else { Box::leak(roots.into_boxed_slice()) };
    let at = at.unwrap_or(format!("{name}:443"));
    let mut chance = [0u8; 64];
    std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut chance)).expect("chance");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 + days * 86_400;

    let mut cable = TcpStream::connect(at.as_str()).expect("no answer at that address");
    cable.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut client = Box::new(Client::new(name.as_bytes(), &chance, now as u64, roots));
    let question = format!("GET {path} HTTP/1.0\r\nHost: {name}\r\nConnection: close\r\n\r\n");
    let (mut asked, mut said, mut over) = (0, Vec::new(), false);
    let mut bytes = [0u8; 4096];
    loop {
        if client.state() == State::Open && asked < question.len() {
            asked += client.write(&question.as_bytes()[asked..]);
        }
        loop {
            let count = client.output(&mut bytes);
            if count == 0 {
                break;
            }
            cable.write_all(&bytes[..count]).expect("the server hung up");
        }
        loop {
            let count = client.read(&mut bytes);
            if count == 0 {
                break;
            }
            said.extend_from_slice(&bytes[..count]);
        }
        if over || !matches!(client.state(), State::Shaking | State::Open) {
            break;
        }
        let wanted = client.wants().min(bytes.len());
        match cable.read(&mut bytes[..wanted]) {
            Ok(0) | Err(_) => over = true,
            Ok(count) => assert_eq!(client.input(&bytes[..count]), count),
        }
    }
    match client.state() {
        State::Failed(why) => {
            println!("refused: {why:?}");
            std::process::exit(1);
        }
        State::Shaking => {
            println!("refused: the server hung up during the handshake");
            std::process::exit(1);
        }
        state => println!("met: {name}, {}", if state == State::Ended { "and it said when it had finished" } else { "and it hung up without saying it had finished" }),
    }
    let text = String::from_utf8_lossy(&said);
    println!("{} bytes", said.len());
    for line in text.lines().take(12) {
        println!("  {}", line.chars().take(100).collect::<String>());
    }
}
