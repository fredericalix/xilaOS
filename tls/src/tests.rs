//! The client against a server played by the tests.
//!
//! The server here is written for the tests only, and shares the drawing
//! of keys and the sealing with the client it tests (`keys.rs`, whose
//! values are checked against another program's). So these tests are
//! about the client's own work: the order of the messages, what it
//! accepts, what it refuses and how. That the whole speaks real TLS is
//! checked elsewhere, against OpenSSL and against servers of the
//! Internet (`examples/meet.rs`, `tools/check.py`).

use std::vec::Vec;

use sha2::{Digest, Sha256};

use crate::check::tests::{roots, BETWEEN, LEAF, NOW, OTHER, STRANGER};
use crate::check::Refused;
use crate::keys::{self, Seal, Secret, Secrets};
use crate::{Client, Error, State};

/// What the server does wrong, when it does.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Wrong {
    Nothing,
    /// Signs something else than the handshake.
    Signature,
    /// Says a Finished that is not the proof.
    Finished,
    /// Says it speaks TLS 1.2.
    Version,
    /// Asks for another hello.
    Retry,
    /// Picks a cipher that was not offered.
    Cipher,
    /// Asks the client for a certificate.
    AsksWho,
    /// Skips its certificates.
    NoCertificate,
    /// Speaks before its Finished.
    Early,
}

struct Server {
    certificates: Vec<&'static [u8]>,
    wrong: Wrong,
    /// The most that goes in one record of the handshake; 0 puts the
    /// whole of what follows the hello in one.
    cut: usize,
    script: Sha256,
    shake: Option<Secrets>,
    master: Secret,
    traffic: Option<Secrets>,
    reading: Option<Seal>,
    writing: Option<Seal>,
    inbox: Vec<u8>,
    out: Vec<u8>,
    /// What the client proved, said and gave up with.
    shaken: bool,
    heard: Vec<u8>,
    alert: Option<(u8, u8)>,
    name: Vec<u8>,
}

fn be(bytes: &[u8]) -> usize {
    bytes.iter().fold(0, |sum, byte| sum << 8 | *byte as usize)
}

fn message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut whole = std::vec![kind, (body.len() >> 16) as u8, (body.len() >> 8) as u8, body.len() as u8];
    whole.extend_from_slice(body);
    whole
}

impl Server {
    fn new(certificates: &[&'static [u8]]) -> Server {
        Server {
            certificates: certificates.to_vec(),
            wrong: Wrong::Nothing,
            cut: 0,
            script: Sha256::new(),
            shake: None,
            master: [0; 32],
            traffic: None,
            reading: None,
            writing: None,
            inbox: Vec::new(),
            out: Vec::new(),
            shaken: false,
            heard: Vec::new(),
            alert: None,
            name: Vec::new(),
        }
    }

    fn hash(&self) -> Secret {
        self.script.clone().finalize().into()
    }

    fn plain(&mut self, kind: u8, body: &[u8]) {
        self.out.extend_from_slice(&[kind, 3, 3, (body.len() >> 8) as u8, body.len() as u8]);
        self.out.extend_from_slice(body);
    }

    fn sealed(&mut self, kind: u8, body: &[u8]) {
        let mut record = std::vec![0u8; body.len() + 22];
        let length = self.writing.as_mut().unwrap().seal(kind, body, &mut record);
        self.out.extend_from_slice(&record[..length]);
    }

    /// Says something in the conversation.
    fn say(&mut self, text: &[u8]) {
        self.sealed(23, text);
    }

    fn close(&mut self) {
        self.sealed(21, &[1, 0]);
    }

    /// New keys for what the server says, and for the client too if asked.
    fn new_keys(&mut self, both: bool) {
        self.sealed(22, &[24, 0, 0, 1, both as u8]);
        let traffic = self.traffic.as_mut().unwrap();
        traffic.server = keys::next(&traffic.server);
        self.writing = Some(Seal::new(&traffic.server));
    }

    /// Bytes came from the client.
    fn take(&mut self, bytes: &[u8]) {
        self.inbox.extend_from_slice(bytes);
        while self.inbox.len() >= 5 && self.inbox.len() >= 5 + be(&self.inbox[3..5]) {
            let record: Vec<u8> = self.inbox.drain(..5 + be(&self.inbox[3..5])).collect();
            let (head, body) = record.split_at(5);
            let mut body = body.to_vec();
            let (kind, length) = match &mut self.reading {
                Some(seal) if head[0] == 23 => seal.open(head, &mut body).expect("the client seals with the keys of the moment"),
                _ => (head[0], body.len()),
            };
            let body = &body[..length];
            match (kind, body.first()) {
                (22, Some(1)) => self.hello(body),
                (22, Some(20)) => {
                    let shake = self.shake.unwrap();
                    assert_eq!(&body[4..], &keys::finished(&shake.client, &self.hash()), "the client's Finished");
                    self.shaken = true;
                    self.reading = Some(Seal::new(&self.traffic.unwrap().client));
                }
                (22, Some(24)) => {
                    assert_eq!(body, [24, 0, 0, 1, 0]);
                    let traffic = self.traffic.as_mut().unwrap();
                    traffic.client = keys::next(&traffic.client);
                    self.reading = Some(Seal::new(&traffic.client));
                }
                (23, _) => self.heard.extend_from_slice(body),
                (21, _) => self.alert = Some((body[0], body[1])),
                other => panic!("the client sent {other:?}"),
            }
        }
    }

    /// The client's hello came: the server's whole side of the handshake.
    fn hello(&mut self, hello: &[u8]) {
        // skip to what the client adds: after the chance, the session, the ciphers and the zero
        let mut at = 4 + 2 + 32;
        at += 1 + hello[at] as usize;
        assert_eq!(&hello[at..at + 4], [0, 2, 0x13, 0x03], "one cipher");
        at += 4 + 2;
        let mut added = &hello[at + 2..];
        assert_eq!(be(&hello[at..at + 2]), added.len());
        let mut theirs = [0u8; 32];
        let mut seen = Vec::new();
        while !added.is_empty() {
            let (what, value) = (be(&added[..2]), &added[4..4 + be(&added[2..4])]);
            seen.push(what);
            match what {
                0 => self.name = value[5..].to_vec(),
                43 => assert_eq!(value, [2, 3, 4]),
                10 => assert_eq!(value, [0, 2, 0, 0x1d]),
                51 => {
                    assert_eq!(value[..6], [0, 36, 0, 0x1d, 0, 32]);
                    theirs.copy_from_slice(&value[6..]);
                }
                _ => {}
            }
            added = &added[4 + value.len()..];
        }
        assert_eq!(seen, [0, 43, 10, 13, 51, 16]);
        self.script.update(hello);

        let secret = [0x5a; 32];
        let public = x25519_dalek::x25519(secret, x25519_dalek::X25519_BASEPOINT_BYTES);
        let mut body = std::vec![3, 3];
        body.extend_from_slice(&if self.wrong == Wrong::Retry { crate::client::RETRY } else { [0xab; 32] });
        body.push(0);
        body.extend_from_slice(if self.wrong == Wrong::Cipher { &[0x13, 0x01] } else { &[0x13, 0x03] });
        body.push(0);
        let mut added = Vec::new();
        if self.wrong != Wrong::Version {
            added.extend_from_slice(&[0, 43, 0, 2, 3, 4]);
        }
        added.extend_from_slice(&[0, 51, 0, 36, 0, 0x1d, 0, 32]);
        added.extend_from_slice(&public);
        body.extend_from_slice(&[(added.len() >> 8) as u8, added.len() as u8]);
        body.extend_from_slice(&added);
        let hello = message(2, &body);
        self.script.update(&hello);
        self.plain(22, &hello);
        // what old equipment on the way wants to see: it says nothing
        self.plain(20, &[1]);

        let (shake, master) = keys::handshake(&x25519_dalek::x25519(secret, theirs), &self.hash());
        (self.shake, self.master) = (Some(shake), master);
        self.writing = Some(Seal::new(&shake.server));
        self.reading = Some(Seal::new(&shake.client));

        let mut flight = Vec::new();
        let say = |server: &mut Server, flight: &mut Vec<u8>, whole: Vec<u8>| {
            server.script.update(&whole);
            flight.extend_from_slice(&whole);
        };
        say(self, &mut flight, message(8, &[0, 0]));
        if self.wrong == Wrong::AsksWho {
            say(self, &mut flight, message(13, &[0, 0, 0]));
        }
        if self.wrong != Wrong::NoCertificate {
            let mut list = Vec::new();
            for certificate in &self.certificates {
                list.extend_from_slice(&[(certificate.len() >> 16) as u8, (certificate.len() >> 8) as u8, certificate.len() as u8]);
                list.extend_from_slice(certificate);
                list.extend_from_slice(&[0, 0]);
            }
            let mut body = std::vec![0, (list.len() >> 16) as u8, (list.len() >> 8) as u8, list.len() as u8];
            body.extend_from_slice(&list);
            say(self, &mut flight, message(11, &body));
        }
        let mut signed = std::vec![0x20u8; 64];
        signed.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
        signed.extend_from_slice(&if self.wrong == Wrong::Signature { [0; 32] } else { self.hash() });
        use p256::ecdsa::signature::Signer;
        let key = p256::ecdsa::SigningKey::from_slice(include_bytes!("../fixtures/leaf-key.raw")).unwrap();
        let signature: p256::ecdsa::Signature = key.sign(&signed);
        let signature = signature.to_der();
        let signature = signature.as_bytes();
        let mut body = std::vec![4, 3, (signature.len() >> 8) as u8, signature.len() as u8];
        body.extend_from_slice(signature);
        say(self, &mut flight, message(15, &body));
        let mut proof = keys::finished(&shake.server, &self.hash());
        if self.wrong == Wrong::Finished {
            proof[31] ^= 1;
        }
        if self.wrong == Wrong::Early {
            self.sealed(22, &flight);
            flight.clear();
            self.sealed(23, b"too early");
        }
        say(self, &mut flight, message(20, &proof));
        let pieces: Vec<Vec<u8>> = flight.chunks(if self.cut == 0 { flight.len() } else { self.cut }).map(|piece| piece.to_vec()).collect();
        for piece in pieces {
            self.sealed(22, &piece);
        }
        let traffic = keys::application(&self.master, &self.hash());
        self.traffic = Some(traffic);
        self.writing = Some(Seal::new(&traffic.server));
    }
}

fn client(name: &[u8], now: u64) -> std::boxed::Box<Client> {
    let chance: [u8; 64] = core::array::from_fn(|at| (at * 7 + 1) as u8);
    std::boxed::Box::new(Client::new(name, &chance, now, roots()))
}

/// Carries bytes both ways until nobody has anything to say: to the
/// client `piece` bytes at a time.
fn carry(client: &mut Client, server: &mut Server, piece: usize) {
    loop {
        let mut bytes = [0u8; 700];
        let count = client.output(&mut bytes);
        server.take(&bytes[..count]);
        let mut given = 0;
        while given < server.out.len() {
            let end = server.out.len().min(given + piece);
            let taken = client.input(&server.out[given..end]);
            given += taken;
            if taken == 0 {
                break;
            }
        }
        server.out.drain(..given);
        if count == 0 && given == 0 {
            return;
        }
    }
}

fn read(client: &mut Client) -> Vec<u8> {
    let mut all = Vec::new();
    let mut bytes = [0u8; 300];
    loop {
        let count = client.read(&mut bytes);
        if count == 0 {
            return all;
        }
        all.extend_from_slice(&bytes[..count]);
    }
}

/// A whole handshake with a server that shows these certificates and
/// does this wrong: how the client ends, and what the server was told.
fn meet(certificates: &[&'static [u8]], wrong: Wrong, name: &[u8], now: u64) -> (State, Option<(u8, u8)>) {
    let mut client = client(name, now);
    let mut server = Server::new(certificates);
    server.wrong = wrong;
    carry(&mut client, &mut server, 1000);
    assert_eq!(server.shaken, client.state() == State::Open);
    (client.state(), server.alert)
}

#[test]
fn a_conversation_opens_and_goes_both_ways() {
    let mut client = client(b"Xila.Test", NOW);
    let mut server = Server::new(&[LEAF, BETWEEN]);
    assert_eq!((client.state(), client.write(b"too early"), client.read(&mut [0; 8])), (State::Shaking, 0, 0));
    carry(&mut client, &mut server, 1000);
    assert_eq!((client.state(), server.shaken, server.alert), (State::Open, true, None));
    assert_eq!(server.name, b"xila.test", "the server is told the name it is called by, in small letters");
    // what the client says arrives, and what the server says
    assert_eq!(client.write(b"GET / HTTP/1.0\r\n\r\n"), 18);
    carry(&mut client, &mut server, 1000);
    assert_eq!(server.heard, b"GET / HTTP/1.0\r\n\r\n");
    server.say(b"HTTP/1.0 200 OK\r\n\r\n");
    server.say(b"hello");
    carry(&mut client, &mut server, 1000);
    // one record at a time: the second waits until the first has been read
    assert_eq!((client.wants(), server.out.is_empty()), (0, false));
    assert_eq!(read(&mut client), b"HTTP/1.0 200 OK\r\n\r\n");
    carry(&mut client, &mut server, 1000);
    assert_eq!(read(&mut client), b"hello");
    // the server has no more to say; the client says so too
    server.close();
    carry(&mut client, &mut server, 1000);
    assert_eq!(client.state(), State::Ended);
    client.close();
    client.close();
    carry(&mut client, &mut server, 1000);
    assert_eq!((server.alert, client.write(b"more")), (Some((1, 0)), 0));
    assert_eq!(client.wants(), 0);
}

#[test]
fn however_the_bytes_are_cut_the_handshake_is_the_same() {
    // records that hold several messages, one each, or pieces of one; bytes one at a time or all at once
    for cut in [0, 1, 37, 500, 1000] {
        for piece in [1, 3, 64, 5000] {
            let mut client = client(b"xila.test", NOW);
            let mut server = Server::new(&[LEAF, BETWEEN]);
            server.cut = cut;
            carry(&mut client, &mut server, piece);
            assert_eq!((client.state(), server.shaken), (State::Open, true), "records of {cut}, {piece} bytes at a time");
            server.say(b"hello");
            carry(&mut client, &mut server, piece);
            assert_eq!(read(&mut client), b"hello");
        }
    }
}

#[test]
fn a_server_that_is_not_who_was_called_is_refused() {
    // the certificate of another machine: 42 is "bad certificate", said to the server before leaving
    assert_eq!(meet(&[OTHER, BETWEEN], Wrong::Nothing, b"xila.test", NOW), (State::Failed(Error::Certificate(Refused::WrongName)), Some((2, 42))));
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::Nothing, b"other.test", NOW).0, State::Failed(Error::Certificate(Refused::WrongName)));
    // nobody trusted vouches for it: 48 is "unknown authority"
    assert_eq!(meet(&[STRANGER], Wrong::Nothing, b"xila.test", NOW), (State::Failed(Error::Certificate(Refused::Unknown)), Some((2, 48))));
    assert_eq!(meet(&[LEAF], Wrong::Nothing, b"xila.test", NOW).0, State::Failed(Error::Certificate(Refused::Unknown)));
    // out of its time by this machine's clock: 45 is "certificate expired"
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::Nothing, b"xila.test", NOW + 150 * 365 * 86_400), (State::Failed(Error::Certificate(Refused::Expired)), Some((2, 45))));
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::Nothing, b"xila.test", 1_000_000_000).0, State::Failed(Error::Certificate(Refused::NotYet)));
    // no certificate at all
    assert_eq!(meet(&[], Wrong::Nothing, b"xila.test", NOW).0, State::Failed(Error::Certificate(Refused::Bad)));
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::NoCertificate, b"xila.test", NOW), (State::Failed(Error::NotTls), Some((2, 10))));
    // a good certificate shown by someone who does not hold its key: 51 is "the signature does not hold"
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::Signature, b"xila.test", NOW), (State::Failed(Error::NotTheirs), Some((2, 51))));
    // a handshake that was not the same seen from both sides: 20 is "does not open"
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::Finished, b"xila.test", NOW), (State::Failed(Error::Forged), Some((2, 20))));
}

#[test]
fn a_server_that_speaks_otherwise_is_told_so() {
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::Version, b"xila.test", NOW), (State::Failed(Error::Old), Some((2, 70))));
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::Retry, b"xila.test", NOW), (State::Failed(Error::NoCommon), Some((2, 40))));
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::Cipher, b"xila.test", NOW).0, State::Failed(Error::NoCommon));
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::AsksWho, b"xila.test", NOW).0, State::Failed(Error::NoCommon));
    assert_eq!(meet(&[LEAF, BETWEEN], Wrong::Early, b"xila.test", NOW).0, State::Failed(Error::NotTls));
    // what is not TLS at all, and a record longer than any may be
    for said in [&b"HTTP/1.1 400 Bad Request\r\n\r\n"[..], b"\x16\x03\x03\xff\xff", b"\x17\x03\x03\x00\x05hello", b"\x16\x03\x03\x00\x00", b"\x16\x03\x03\x00\x04\x02\xff\xff\xff"] {
        let mut client = client(b"xila.test", NOW);
        client.input(said);
        assert!(matches!(client.state(), State::Failed(Error::NotTls | Error::TooLong)), "{said:?}");
        assert_eq!(client.wants(), 0);
    }
    // a server that gives up says why with a number: 40 is "nothing in common", 80 "something broke"
    for (number, level) in [(40, 2), (80, 2), (0, 1)] {
        let mut client = client(b"xila.test", NOW);
        client.input(&[21, 3, 3, 0, 2, level, number]);
        assert_eq!(client.state(), State::Failed(Error::Alert(number)));
    }
}

#[test]
fn what_was_changed_on_the_way_does_not_open() {
    // one bit anywhere in what the server sends: never a conversation
    let mut whole = Server::new(&[LEAF, BETWEEN]);
    let mut first = client(b"xila.test", NOW);
    let mut bytes = [0u8; 700];
    let count = first.output(&mut bytes);
    whole.take(&bytes[..count]);
    let sent = whole.out.clone();
    for at in (0..sent.len()).step_by(11) {
        let mut client = client(b"xila.test", NOW);
        let mut changed = sent.clone();
        changed[at] ^= 0x04;
        client.input(&changed);
        assert!(matches!(client.state(), State::Failed(_) | State::Shaking), "byte {at}: {:?}", client.state());
    }
    // and in the conversation: the record does not open, and nothing of it is given to read
    let mut client = client(b"xila.test", NOW);
    let mut server = Server::new(&[LEAF, BETWEEN]);
    carry(&mut client, &mut server, 1000);
    server.say(b"pay 10 to alice");
    let last = server.out.len() - 1;
    server.out[last] ^= 1;
    carry(&mut client, &mut server, 1000);
    assert_eq!((client.state(), read(&mut client), server.alert), (State::Failed(Error::Forged), Vec::new(), Some((2, 20))));
    // a record said twice is as bad as one changed
    let mut client = self::client(b"xila.test", NOW);
    let mut server = Server::new(&[LEAF, BETWEEN]);
    carry(&mut client, &mut server, 1000);
    server.say(b"pay 10 to alice");
    let twice = server.out.clone();
    server.out.extend_from_slice(&twice);
    carry(&mut client, &mut server, 1000);
    assert_eq!(read(&mut client), b"pay 10 to alice");
    carry(&mut client, &mut server, 1000);
    assert_eq!((client.state(), read(&mut client)), (State::Failed(Error::Forged), Vec::new()));
}

#[test]
fn keys_change_when_the_server_says_so() {
    let mut client = client(b"xila.test", NOW);
    let mut server = Server::new(&[LEAF, BETWEEN]);
    carry(&mut client, &mut server, 1000);
    // a ticket for a later session is let go by
    server.sealed(22, &message(4, &[0; 40]));
    // its own keys only
    server.new_keys(false);
    server.say(b"under the second keys");
    carry(&mut client, &mut server, 1000);
    assert_eq!(read(&mut client), b"under the second keys");
    assert_eq!(client.write(b"still the first of ours"), 23);
    carry(&mut client, &mut server, 1000);
    assert_eq!(server.heard, b"still the first of ours");
    // and ours too: the client says so, then seals under the new ones
    server.new_keys(true);
    server.say(b"under the third");
    carry(&mut client, &mut server, 1000);
    assert_eq!(read(&mut client), b"under the third");
    assert_eq!(client.write(b", now the second"), 16);
    carry(&mut client, &mut server, 1000);
    assert_eq!((client.state(), &server.heard[23..]), (State::Open, &b", now the second"[..]));
}

#[test]
fn much_is_said_piece_by_piece() {
    let mut client = client(b"xila.test", NOW);
    let mut server = Server::new(&[LEAF, BETWEEN]);
    carry(&mut client, &mut server, 1000);
    // more than a record holds, and more than waits to be sent at once
    let said: Vec<u8> = (0..20_000u32).map(|at| (at % 251) as u8).collect();
    let mut sent = 0;
    assert_eq!(client.state(), State::Open);
    while sent < said.len() {
        let taken = client.write(&said[sent..]);
        sent += taken;
        if taken == 0 {
            carry(&mut client, &mut server, 1000);
        }
    }
    carry(&mut client, &mut server, 1000);
    assert_eq!(server.heard, said);
    // the longest record there is, read in small pieces while more waits
    let long: Vec<u8> = (0..16_384u32).map(|at| (at % 241) as u8).collect();
    server.say(&long);
    server.say(b"and then");
    let mut all = Vec::new();
    for _ in 0..200 {
        carry(&mut client, &mut server, 999);
        all.extend_from_slice(&read(&mut client));
    }
    assert_eq!(all.len(), long.len() + 8);
    assert!(all[..long.len()] == long[..] && &all[long.len()..] == b"and then");
    assert_eq!(client.state(), State::Open);
}

#[test]
fn two_calls_never_use_the_same_numbers() {
    // the first half of the chance is the secret, the second is sent as it is
    let hello = |secret: u8, shown: u8| {
        let chance: [u8; 64] = core::array::from_fn(|at| if at < 32 { secret } else { shown });
        let mut client = std::boxed::Box::new(Client::new(b"xila.test", &chance, NOW, roots()));
        let mut bytes = [0u8; 700];
        let count = client.output(&mut bytes);
        bytes[..count].to_vec()
    };
    let (one, other) = (hello(1, 2), hello(3, 4));
    assert_eq!(one.len(), other.len());
    // what differs: the chance that is shown, and the half of the key drawn from the secret
    let differ = one.iter().zip(&other).filter(|(a, b)| a != b).count();
    assert!((48..=64).contains(&differ), "{differ}");
    assert!(one.windows(32).any(|part| part == [2; 32]) && !one.windows(32).any(|part| part == [1; 32]), "the secret itself is never sent");
    // the same chance would give the same call: it must never be the same
    assert_eq!(hello(1, 2), one);
}

#[test]
fn a_chain_too_long_for_the_room_there_is_is_refused() {
    let mut client = client(b"xila.test", NOW);
    let mut server = Server::new(&[LEAF; 60]);
    server.cut = 1000;
    carry(&mut client, &mut server, 1000);
    assert_eq!(client.state(), State::Failed(Error::TooLong));
}
