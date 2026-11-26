//! The client: the handshake, then the conversation.
//!
//! The handshake, as RFC 8446 has it and as it goes here:
//!
//! ```text
//!   we say                           the server says
//!   ClientHello  ----------------->
//!     our half of the key, the       ServerHello
//!     name we call, what we speak      its half of the key
//!                                    (from here on, sealed)
//!                                    EncryptedExtensions
//!                                    Certificate        who it says it is
//!                                    CertificateVerify  proof it holds that key
//!                <-----------------  Finished           proof it saw what we saw
//!   Finished     ----------------->
//!   (the conversation, sealed under new keys)
//! ```
//!
//! Each message is hashed as it goes by, and what each side proves is
//! always about that hash: nothing can be taken out, added or changed
//! in the handshake without the two Finished disagreeing.
//!
//! Messages travel in records, and need not fit them: one record may
//! hold several messages and one message may spread over several
//! records. The records of a server that fails before it has keys are
//! plain; all the others are sealed (`keys::Seal`).

use rustls_pki_types::TrustAnchor;
use sha2::{Digest, Sha256};

use crate::check::{self, Refused, CHAIN, SCHEMES};
use crate::keys::{self, Seal, Secret, Secrets, HEAD, SEAL};

/// The longest name of a server.
pub const NAME: usize = 64;
/// The most a record may hold, and the most that may come with it when
/// it is sealed.
const MOST: usize = 16384;
const INBOX: usize = HEAD + MOST + 256;
/// Room for the messages of the handshake that are not whole yet: the
/// certificates are the long ones, a few kilobytes together.
const HAND: usize = 16384;
/// Room for what waits to be sent.
const OUTBOX: usize = 4096;
/// The most of what is written that goes in one record.
const PIECE: usize = 2048;

/// What a ServerHello holds in place of chance when it is not one: the
/// server asks for another hello, with another kind of key.
pub(crate) const RETRY: [u8; 32] = [
    0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8, 0x91, 0xc2, 0xa2, 0x11, 0x16, 0x7a, 0xbb, 0x8c, 0x5e, 0x07,
    0x9e, 0x09, 0xe2, 0xc8, 0xa8, 0x33, 0x9c,
];

/// Why a conversation could not be had, or was cut.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// What came is not TLS, or not as its rules say.
    NotTls,
    /// The server speaks only a TLS older than 1.3.
    Old,
    /// The server and XilaOS have no way in common of agreeing on a key
    /// or of sealing what they say.
    NoCommon,
    /// The server gave up, and said why with the number TLS has for it.
    Alert(u8),
    /// The certificate the server showed is refused.
    Certificate(Refused),
    /// The server does not hold the key of the certificate it showed.
    NotTheirs,
    /// Something was changed on the way, or the two sides do not have
    /// the same keys.
    Forged,
    /// The server said more than there is room for.
    TooLong,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// The handshake is going on.
    Shaking,
    /// The conversation is open: `write` and `read`.
    Open,
    /// The server said it has no more to say. Nothing was cut off.
    Ended,
    Failed(Error),
}

/// Which message of the handshake is expected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Hello,
    Extensions,
    Certificate,
    Verify,
    Finished,
    Open,
}

pub struct Client {
    name: [u8; NAME],
    name_len: usize,
    /// Seconds since 1970, to check the certificates against.
    now: u64,
    roots: &'static [TrustAnchor<'static>],
    /// Our half of the key exchange: the number nobody else knows.
    secret: [u8; 32],
    stage: Stage,
    state: State,
    /// The hash of the handshake so far.
    script: Sha256,
    shake: Option<Secrets>,
    master: Secret,
    traffic: Option<Secrets>,
    reading: Option<Seal>,
    writing: Option<Seal>,
    /// The record that is arriving, then what it held when that is for
    /// whoever reads: `plain` is where, until it has been read.
    inbox: [u8; INBOX],
    held: usize,
    plain: (usize, usize),
    /// Messages of the handshake, from `kept` on those not looked at
    /// yet. The certificates stay, before `kept`, until the signature
    /// that goes with them has come: `leaf` is the server's own.
    hand: [u8; HAND],
    hand_len: usize,
    kept: usize,
    leaf: (usize, usize),
    outbox: [u8; OUTBOX],
    out: (usize, usize),
    /// The server asked for new keys on our side too, and that is not
    /// said yet.
    owed: bool,
    closing: bool,
}

fn be(bytes: &[u8]) -> usize {
    bytes.iter().fold(0, |sum, byte| sum << 8 | *byte as usize)
}

/// Writes a message piece by piece, and the lengths that come before
/// what they measure.
struct Put<'a> {
    bytes: &'a mut [u8],
    at: usize,
}

impl Put<'_> {
    fn u8(&mut self, value: u8) {
        self.bytes[self.at] = value;
        self.at += 1;
    }

    fn u16(&mut self, value: usize) {
        self.u8((value >> 8) as u8);
        self.u8(value as u8);
    }

    fn all(&mut self, bytes: &[u8]) {
        self.bytes[self.at..self.at + bytes.len()].copy_from_slice(bytes);
        self.at += bytes.len();
    }

    /// Leaves room for a length of `width` bytes, to be written by `close`.
    fn open(&mut self, width: usize) -> (usize, usize) {
        self.at += width;
        (self.at, width)
    }

    fn close(&mut self, (from, width): (usize, usize)) {
        let length = self.at - from;
        for at in 0..width {
            self.bytes[from - 1 - at] = (length >> (8 * at)) as u8;
        }
    }
}

/// Reads a ServerHello: the server's half of the key.
fn server_hello(body: &[u8]) -> Result<[u8; 32], Error> {
    // a version nobody reads, 32 bytes of chance, the session we named
    // (none), the cipher, a zero, then what the server adds
    let chance = body.get(2..34).ok_or(Error::NotTls)?;
    let rest = match body.get(34) {
        Some(0) => &body[35..],
        _ => return Err(Error::NotTls),
    };
    if rest.len() < 5 || 5 + be(&rest[3..5]) != rest.len() {
        return Err(Error::NotTls);
    }
    let (mut version, mut theirs) = (false, None);
    let mut added = &rest[5..];
    while !added.is_empty() {
        if added.len() < 4 || added.len() < 4 + be(&added[2..4]) {
            return Err(Error::NotTls);
        }
        let (what, value) = (be(&added[..2]), &added[4..4 + be(&added[2..4])]);
        match what {
            // the version that is really spoken: 1.3
            43 => version = value == [3, 4],
            // its half of the key: X25519, 32 bytes
            51 if value.len() == 36 && value[..4] == [0, 0x1d, 0, 32] => theirs = <[u8; 32]>::try_from(&value[4..]).ok(),
            _ => {}
        }
        added = &added[4 + value.len()..];
    }
    if !version {
        return Err(Error::Old);
    }
    if chance == RETRY || rest[..3] != [0x13, 0x03, 0] {
        return Err(Error::NoCommon);
    }
    theirs.ok_or(Error::NotTls)
}

impl Client {
    /// Starts calling the server of that name. `chance` must be 64 bytes
    /// that nobody else can guess: the secrecy of everything rests on
    /// them. `now` is the time, in seconds since 1970, and `roots` the
    /// authorities that are trusted (`public_roots()`).
    pub fn new(name: &[u8], chance: &[u8; 64], now: u64, roots: &'static [TrustAnchor<'static>]) -> Client {
        let mut client = Client {
            name: [0; NAME],
            name_len: name.len().min(NAME),
            now,
            roots,
            secret: [0; 32],
            stage: Stage::Hello,
            state: State::Shaking,
            script: Sha256::new(),
            shake: None,
            master: [0; 32],
            traffic: None,
            reading: None,
            writing: None,
            inbox: [0; INBOX],
            held: 0,
            plain: (0, 0),
            hand: [0; HAND],
            hand_len: 0,
            kept: 0,
            leaf: (0, 0),
            outbox: [0; OUTBOX],
            out: (0, 0),
            owed: false,
            closing: false,
        };
        // a name too long is cut, and then is not the name of the certificate
        client.name[..client.name_len].copy_from_slice(&name[..client.name_len]);
        client.name.make_ascii_lowercase();
        client.secret.copy_from_slice(&chance[..32]);
        let public = x25519_dalek::x25519(client.secret, x25519_dalek::X25519_BASEPOINT_BYTES);

        let mut put = Put { bytes: &mut client.outbox[HEAD..], at: 0 };
        // a ClientHello
        put.u8(1);
        let whole = put.open(3);
        // the version, as TLS 1.2 wrote it: the real one is further down
        put.u16(0x0303);
        put.all(&chance[32..]);
        // no session of older days
        put.u8(0);
        // one cipher: ChaCha20-Poly1305 with SHA-256
        put.u16(2);
        put.u16(0x1303);
        // "no compression", which is all there is
        put.u8(1);
        put.u8(0);
        let added = put.open(2);
        // the name of the server we call: one address may serve several
        put.u16(0);
        let (list, entry) = (put.open(2), put.open(2));
        put.u8(0);
        put.u16(client.name_len);
        put.all(&client.name[..client.name_len]);
        put.close(entry);
        put.close(list);
        // the version we speak: 1.3 and no other
        put.u16(43);
        put.u16(3);
        put.u8(2);
        put.u16(0x0304);
        // the one way we agree on a key: X25519
        put.u16(10);
        put.u16(4);
        put.u16(2);
        put.u16(0x001d);
        // the signatures we can check
        put.u16(13);
        put.u16(2 + 2 * SCHEMES.len());
        put.u16(2 * SCHEMES.len());
        for scheme in SCHEMES {
            put.u16(scheme as usize);
        }
        // our half of the key
        put.u16(51);
        put.u16(38);
        put.u16(36);
        put.u16(0x001d);
        put.u16(32);
        put.all(&public);
        // what will be spoken inside: HTTP 1.1
        put.u16(16);
        put.u16(11);
        put.u16(9);
        put.u8(8);
        put.all(b"http/1.1");
        put.close(added);
        put.close(whole);
        let length = put.at;
        client.script.update(&client.outbox[HEAD..HEAD + length]);
        // a plain record of the handshake (22), with the version of the oldest TLS, as is done
        client.outbox[..HEAD].copy_from_slice(&[22, 3, 1, (length >> 8) as u8, length as u8]);
        client.out = (0, HEAD + length);
        client
    }

    pub fn state(&self) -> State {
        self.state
    }

    fn hash(&self) -> Secret {
        self.script.clone().finalize().into()
    }

    /// Makes room at the end of the outbox. False if there is not that much.
    fn room(&mut self, need: usize) -> bool {
        if OUTBOX - self.out.1 < need {
            self.outbox.copy_within(self.out.0..self.out.1, 0);
            self.out = (0, self.out.1 - self.out.0);
        }
        OUTBOX - self.out.1 >= need
    }

    /// Puts a record in the outbox: sealed once there are keys.
    fn emit(&mut self, kind: u8, plain: &[u8]) -> bool {
        if !self.room(HEAD + plain.len() + SEAL) {
            return false;
        }
        let at = self.out.1;
        self.out.1 += match &mut self.writing {
            Some(seal) => seal.seal(kind, plain, &mut self.outbox[at..]),
            None => {
                self.outbox[at..at + HEAD].copy_from_slice(&[kind, 3, 3, (plain.len() >> 8) as u8, plain.len() as u8]);
                self.outbox[at + HEAD..at + HEAD + plain.len()].copy_from_slice(plain);
                HEAD + plain.len()
            }
        };
        true
    }

    /// Gives up, and tells the server why with the number TLS has for it.
    fn fail(&mut self, why: Error) {
        if matches!(self.state, State::Failed(_)) {
            return;
        }
        let number = match why {
            Error::NotTls => 10,
            Error::Old => 70,
            Error::NoCommon => 40,
            Error::Certificate(Refused::Expired) => 45,
            Error::Certificate(Refused::Unknown) => 48,
            Error::Certificate(Refused::Unsupported) => 43,
            Error::Certificate(_) => 42,
            Error::NotTheirs => 51,
            Error::Forged => 20,
            Error::TooLong => 22,
            // it gave up first: nothing to tell it
            Error::Alert(_) => 0,
        };
        if number != 0 {
            self.emit(21, &[2, number]);
        }
        self.state = State::Failed(why);
    }

    /// How many bytes from the server can be taken now. None while what
    /// came before waits to be read, or once all is over.
    pub fn wants(&self) -> usize {
        if !matches!(self.state, State::Shaking | State::Open) || self.plain.0 < self.plain.1 {
            0
        } else if self.held < HEAD {
            HEAD - self.held
        } else {
            (HEAD + be(&self.inbox[3..5])).saturating_sub(self.held)
        }
    }

    /// Bytes came from the server. Returns how many were taken: all of
    /// them, unless something waits to be read first (`wants`).
    pub fn input(&mut self, mut bytes: &[u8]) -> usize {
        let mut taken = 0;
        loop {
            let count = self.wants().min(bytes.len());
            if count == 0 {
                return taken;
            }
            self.inbox[self.held..self.held + count].copy_from_slice(&bytes[..count]);
            self.held += count;
            taken += count;
            bytes = &bytes[count..];
            if self.held < HEAD {
                continue;
            }
            let length = be(&self.inbox[3..5]);
            if length > INBOX - HEAD {
                self.fail(Error::NotTls);
            } else if self.held == HEAD + length {
                if let Err(why) = self.record(length) {
                    self.fail(why);
                }
            }
        }
    }

    /// A whole record is in the inbox.
    fn record(&mut self, length: usize) -> Result<(), Error> {
        let (kind, length) = match (self.inbox[0], &mut self.reading) {
            // "change cipher spec", a leftover of older TLS that servers
            // send during the handshake so that old equipment on the way
            // lets it through. It says nothing, and always the same.
            (20, _) if self.stage != Stage::Open && length == 1 && self.inbox[HEAD] == 1 => {
                self.held = 0;
                return Ok(());
            }
            // sealed: its real kind is inside
            (23, Some(seal)) => {
                let (head, body) = self.inbox.split_at_mut(HEAD);
                seal.open(head, &mut body[..length]).filter(|(_, length)| *length <= MOST).ok_or(Error::Forged)?
            }
            // plain, before there are keys: the handshake, or giving up
            (21 | 22, None) => (self.inbox[0], length),
            _ => return Err(Error::NotTls),
        };
        match kind {
            21 if length == 2 => match self.inbox[HEAD + 1] {
                // "I have no more to say", after everything arrived
                0 if self.stage == Stage::Open => self.state = State::Ended,
                // "the user gave up": the end follows
                90 => {}
                number => self.state = State::Failed(Error::Alert(number)),
            },
            22 if length > 0 => {
                if self.hand_len + length > HAND {
                    return Err(Error::TooLong);
                }
                self.hand[self.hand_len..self.hand_len + length].copy_from_slice(&self.inbox[HEAD..HEAD + length]);
                self.hand_len += length;
                self.messages()?;
            }
            23 if self.stage == Stage::Open => {
                if length > 0 {
                    // stays in the inbox until it has been read
                    self.plain = (HEAD, HEAD + length);
                    return Ok(());
                }
            }
            _ => return Err(Error::NotTls),
        }
        self.held = 0;
        Ok(())
    }

    /// Looks at the messages of the handshake that are whole.
    fn messages(&mut self) -> Result<(), Error> {
        loop {
            let at = self.kept;
            if self.hand_len - at < 4 {
                return Ok(());
            }
            let length = 4 + be(&self.hand[at + 1..at + 4]);
            if at + length > HAND {
                return Err(Error::TooLong);
            }
            if self.hand_len - at < length {
                return Ok(());
            }
            let before = self.stage;
            self.message(at, length)?;
            let end = at + length;
            if before == Stage::Certificate {
                // the certificates stay until the signature has come
                self.kept = end;
            } else {
                self.hand.copy_within(end..self.hand_len, 0);
                self.hand_len -= end;
                self.kept = 0;
            }
        }
    }

    /// One whole message of the handshake, at `at` in `hand`.
    fn message(&mut self, at: usize, length: usize) -> Result<(), Error> {
        let end = at + length;
        match (self.stage, self.hand[at]) {
            (Stage::Hello, 2) => {
                let theirs = server_hello(&self.hand[at + 4..end])?;
                self.script.update(&self.hand[at..end]);
                // nothing may follow in the same record: what follows is under the new keys
                let shared = x25519_dalek::x25519(self.secret, theirs);
                if self.hand_len != end || shared == [0; 32] {
                    return Err(Error::NotTls);
                }
                let (shake, master) = keys::handshake(&shared, &self.hash());
                self.reading = Some(Seal::new(&shake.server));
                self.writing = Some(Seal::new(&shake.client));
                (self.shake, self.master) = (Some(shake), master);
                self.stage = Stage::Extensions;
            }
            (Stage::Extensions, 8) => {
                // what the server adds to its hello: nothing we asked for needs reading
                self.script.update(&self.hand[at..end]);
                self.stage = Stage::Certificate;
            }
            (Stage::Certificate, 11) => {
                // a zero, the length of the list, then each certificate
                // after its length and before what is added to it
                let body = &self.hand[at + 4..end];
                if body.len() < 4 || body[0] != 0 || 4 + be(&body[1..4]) != body.len() {
                    return Err(Error::NotTls);
                }
                let mut certificates: [&[u8]; CHAIN] = [&[]; CHAIN];
                let (mut count, mut from) = (0, 4);
                while from < body.len() {
                    let size = be(body.get(from..from + 3).ok_or(Error::NotTls)?);
                    let certificate = body.get(from + 3..from + 3 + size).ok_or(Error::NotTls)?;
                    if count == 0 {
                        self.leaf = (at + 4 + from + 3, at + 4 + from + 3 + size);
                    }
                    // more than a chain ever needs are not looked at
                    if count < CHAIN {
                        certificates[count] = certificate;
                        count += 1;
                    }
                    from += 3 + size;
                    from += 2 + be(body.get(from..from + 2).ok_or(Error::NotTls)?);
                }
                if from != body.len() {
                    return Err(Error::NotTls);
                }
                check::chain(&certificates[..count], &self.name[..self.name_len], self.now, self.roots).map_err(Error::Certificate)?;
                self.script.update(&self.hand[at..end]);
                self.stage = Stage::Verify;
            }
            // "show me a certificate of yours": XilaOS has none
            (Stage::Certificate, 13) => return Err(Error::NoCommon),
            (Stage::Verify, 15) => {
                // the way of signing, then the signature after its length:
                // over 64 spaces, a title, a zero and the hash of the
                // handshake up to the certificates
                let body = &self.hand[at + 4..end];
                if body.len() < 4 || 4 + be(&body[2..4]) != body.len() {
                    return Err(Error::NotTls);
                }
                let mut signed = [0x20u8; 64 + 33 + 1 + 32];
                signed[64..97].copy_from_slice(b"TLS 1.3, server CertificateVerify");
                signed[97] = 0;
                signed[98..].copy_from_slice(&self.hash());
                if !check::signed(&self.hand[self.leaf.0..self.leaf.1], be(&body[..2]) as u16, &signed, &body[4..]) {
                    return Err(Error::NotTheirs);
                }
                self.script.update(&self.hand[at..end]);
                self.stage = Stage::Finished;
            }
            (Stage::Finished, 20) => {
                let shake = self.shake.ok_or(Error::NotTls)?;
                if !keys::same(&self.hand[at + 4..end], &keys::finished(&shake.server, &self.hash())) {
                    return Err(Error::Forged);
                }
                self.script.update(&self.hand[at..end]);
                if self.hand_len != end {
                    return Err(Error::NotTls);
                }
                // our own Finished, the last thing under the keys of the handshake
                let script = self.hash();
                let mut ours = [0u8; 36];
                ours[..4].copy_from_slice(&[20, 0, 0, 32]);
                ours[4..].copy_from_slice(&keys::finished(&shake.client, &script));
                if !self.emit(22, &ours) {
                    return Err(Error::TooLong);
                }
                let traffic = keys::application(&self.master, &script);
                self.reading = Some(Seal::new(&traffic.server));
                self.writing = Some(Seal::new(&traffic.client));
                self.traffic = Some(traffic);
                (self.stage, self.state) = (Stage::Open, State::Open);
            }
            // a ticket to take this session up again later: not used
            (Stage::Open, 4) => {}
            // new keys: the server's from now on, and ours if it asks
            (Stage::Open, 24) => {
                let traffic = self.traffic.as_mut().ok_or(Error::NotTls)?;
                if length != 5 || self.hand[at + 4] > 1 || self.hand_len != end {
                    return Err(Error::NotTls);
                }
                traffic.server = keys::next(&traffic.server);
                self.reading = Some(Seal::new(&traffic.server));
                self.owed |= self.hand[at + 4] == 1;
            }
            _ => return Err(Error::NotTls),
        }
        Ok(())
    }

    /// Says that our keys change, if the server asked, and changes them.
    fn owe(&mut self) {
        if self.owed && self.state == State::Open && self.emit(22, &[24, 0, 0, 1, 0]) {
            if let Some(traffic) = self.traffic.as_mut() {
                traffic.client = keys::next(&traffic.client);
                self.writing = Some(Seal::new(&traffic.client));
            }
            self.owed = false;
        }
    }

    /// Bytes to send to the server: fills `into` with as many as there
    /// are, and returns how many.
    pub fn output(&mut self, into: &mut [u8]) -> usize {
        self.owe();
        let count = into.len().min(self.out.1 - self.out.0);
        into[..count].copy_from_slice(&self.outbox[self.out.0..self.out.0 + count]);
        self.out.0 += count;
        if self.out.0 == self.out.1 {
            self.out = (0, 0);
        }
        count
    }

    /// Says something to the server, once the conversation is open.
    /// Returns how many bytes were taken: what there is room for.
    pub fn write(&mut self, bytes: &[u8]) -> usize {
        self.owe();
        let count = bytes.len().min(PIECE);
        if self.state != State::Open || self.owed || self.closing || count == 0 || !self.emit(23, &bytes[..count]) {
            return 0;
        }
        count
    }

    /// What the server said: fills `into` and returns how many bytes.
    pub fn read(&mut self, into: &mut [u8]) -> usize {
        let count = into.len().min(self.plain.1 - self.plain.0);
        into[..count].copy_from_slice(&self.inbox[self.plain.0..self.plain.0 + count]);
        self.plain.0 += count;
        if count > 0 && self.plain.0 == self.plain.1 {
            // the record is read: the next one may come
            (self.plain, self.held) = ((0, 0), 0);
        }
        count
    }

    /// Tells the server we have no more to say. What it still says can
    /// be read.
    pub fn close(&mut self) {
        if matches!(self.state, State::Open | State::Ended) && !self.closing && self.emit(21, &[1, 0]) {
            self.closing = true;
        }
    }
}
