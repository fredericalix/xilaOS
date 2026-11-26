//! The keys of a conversation, and the sealing of what is said.
//!
//! Both sides start from one secret that only they can work out (the
//! X25519 exchange) and from what they said to each other so far, and
//! draw from them, step by step, the keys of each direction: first those
//! that hide the rest of the handshake, then those of the conversation
//! itself. This is the "key schedule" of RFC 8446, section 7.1, for the
//! one hash used here, SHA-256.
//!
//! Everything said under a key goes in records. A record is sealed with
//! ChaCha20-Poly1305: nobody without the key can read it, and one that
//! was changed on the way does not open. Each record is sealed under its
//! own number, counted from 0 on each side, so that records cannot be
//! replayed, dropped or put in another order.

use chacha20poly1305::aead::AeadInOut;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub(crate) type Secret = [u8; 32];

/// What a seal adds to a record: the byte that says its real kind, and
/// the sixteen bytes that prove it was not changed.
pub(crate) const SEAL: usize = 17;
/// A record starts with five bytes: its kind, a version nobody reads any
/// more, and its length.
pub(crate) const HEAD: usize = 5;

pub(crate) fn hmac(key: &[u8], parts: &[&[u8]]) -> Secret {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("a key of any length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// "HKDF-Expand-Label": `len` bytes drawn from a secret for a named use.
/// At most 32, which is all that is ever asked for here; the rest of the
/// array is zeros.
pub(crate) fn label(secret: &Secret, name: &[u8], context: &[u8], len: usize) -> Secret {
    let head = [(len >> 8) as u8, len as u8, (6 + name.len()) as u8];
    let mut out = hmac(secret, &[&head, b"tls13 ", name, &[context.len() as u8], context, &[1]]);
    out[len..].fill(0);
    out
}

/// The secrets of the two directions at one moment of the conversation.
#[derive(Clone, Copy)]
pub(crate) struct Secrets {
    pub client: Secret,
    pub server: Secret,
}

/// The secrets that hide the handshake, from the shared secret and the
/// hash of the two hellos; and the master secret, kept for `application`.
pub(crate) fn handshake(shared: &[u8; 32], hellos: &Secret) -> (Secrets, Secret) {
    let zero = [0u8; 32];
    let empty: Secret = Sha256::digest(b"").into();
    let early = hmac(&zero, &[&zero]);
    let secret = hmac(&label(&early, b"derived", &empty, 32), &[shared]);
    let secrets = Secrets { client: label(&secret, b"c hs traffic", hellos, 32), server: label(&secret, b"s hs traffic", hellos, 32) };
    (secrets, hmac(&label(&secret, b"derived", &empty, 32), &[&zero]))
}

/// The secrets of the conversation itself, from the master secret and
/// the hash of everything up to the server's Finished.
pub(crate) fn application(master: &Secret, finished: &Secret) -> Secrets {
    Secrets { client: label(master, b"c ap traffic", finished, 32), server: label(master, b"s ap traffic", finished, 32) }
}

/// What a side says in its Finished: the proof that it saw the same
/// handshake, `script` being the hash of it, and holds the secret.
pub(crate) fn finished(secret: &Secret, script: &Secret) -> Secret {
    hmac(&label(secret, b"finished", b"", 32), &[script])
}

/// The secret that follows one, when a side asks for new keys.
pub(crate) fn next(secret: &Secret) -> Secret {
    label(secret, b"traffic upd", b"", 32)
}

/// Whether two proofs are the same, in a time that does not tell where
/// they differ.
pub(crate) fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |differ, (a, b)| differ | (a ^ b)) == 0
}

/// One direction of the conversation under one secret.
pub(crate) struct Seal {
    key: ChaCha20Poly1305,
    iv: [u8; 12],
    count: u64,
}

impl Seal {
    pub fn new(secret: &Secret) -> Seal {
        let mut iv = [0; 12];
        iv.copy_from_slice(&label(secret, b"iv", b"", 12)[..12]);
        Seal { key: ChaCha20Poly1305::new(&label(secret, b"key", b"", 32).into()), iv, count: 0 }
    }

    /// The number each record is sealed under: never twice the same.
    fn nonce(&mut self) -> [u8; 12] {
        let mut nonce = self.iv;
        for (at, byte) in self.count.to_be_bytes().iter().enumerate() {
            nonce[4 + at] ^= byte;
        }
        self.count += 1;
        nonce
    }

    /// Seals `plain` as a record of a kind into `into`, which must have
    /// room for `HEAD + plain.len() + SEAL` bytes. Returns its length.
    pub fn seal(&mut self, kind: u8, plain: &[u8], into: &mut [u8]) -> usize {
        let length = plain.len() + SEAL;
        // what anyone sees: "application data", whatever is inside
        into[..HEAD].copy_from_slice(&[23, 3, 3, (length >> 8) as u8, length as u8]);
        into[HEAD..HEAD + plain.len()].copy_from_slice(plain);
        into[HEAD + plain.len()] = kind;
        let nonce = self.nonce();
        let (head, body) = into.split_at_mut(HEAD);
        let (body, tag) = body[..length].split_at_mut(plain.len() + 1);
        let proof = self.key.encrypt_inout_detached(&nonce.into(), head, body.into()).expect("a record is never too long to seal");
        tag.copy_from_slice(&proof);
        HEAD + length
    }

    /// Opens a record in place: `head` is its five first bytes, `body`
    /// what follows. Returns its real kind and the length of what it
    /// holds, at the start of `body`; None if it was not sealed with
    /// this key under the number expected, or holds nothing.
    pub fn open(&mut self, head: &[u8], body: &mut [u8]) -> Option<(u8, usize)> {
        let length = body.len().checked_sub(16)?;
        let nonce = self.nonce();
        let (body, tag) = body.split_at_mut(length);
        let tag: [u8; 16] = (&*tag).try_into().ok()?;
        self.key.decrypt_inout_detached(&nonce.into(), head, (&mut *body).into(), &tag.into()).ok()?;
        // after what was said come its kind, then zeros that hide its length
        let end = body.iter().rposition(|byte| *byte != 0)?;
        Some((body[end], end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> std::vec::Vec<u8> {
        (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
    }

    /// The values were worked out by other code: Python's `hmac` and
    /// `hashlib`, with the formulas of RFC 8446 written again there.
    #[test]
    fn the_secrets_are_those_another_program_works_out() {
        // the one value every TLS 1.3 conversation shares, printed in RFC 8448
        assert_eq!(hmac(&[0; 32], &[&[0; 32]]).to_vec(), hex("33ad0a1c607ec03b09e6cd9893680ce210adf300aa1f2660e1b22e10f170f92a"));
        let shared: [u8; 32] = core::array::from_fn(|at| at as u8);
        let hellos: Secret = Sha256::digest(b"both hellos").into();
        let done: Secret = Sha256::digest(b"up to the server's Finished").into();
        let (shake, master) = handshake(&shared, &hellos);
        assert_eq!(shake.client.to_vec(), hex("b0e19b64f44c27a8364ee5229df7d57a00790daebb65c059c2dba25cbdd52894"));
        assert_eq!(shake.server.to_vec(), hex("2bfe89afdb7b768e2e36cd8778e665cbf154819e1b313743727ce14d6315f3a0"));
        assert_eq!(master.to_vec(), hex("055796c9a2f048dd920b01351ba00d131ba528efa37749efb03e31b643c615b2"));
        let traffic = application(&master, &done);
        assert_eq!(traffic.client.to_vec(), hex("64334715a3f7ba99bcad08d50fc500d4bb88b2dcad733a9f492d1ac26644efef"));
        assert_eq!(traffic.server.to_vec(), hex("a440384d7d6b6cb10512c50806a25d9a50f920365560be9dde85765541fa2e54"));
        assert_eq!(label(&shake.client, b"key", b"", 32).to_vec(), hex("58420b45da937da349e6c0ce6dc5114a87ddc66de3a61a24fcaf5570b2635bd5"));
        assert_eq!(label(&shake.client, b"iv", b"", 12)[..12].to_vec(), hex("cbf049096dfb031b5aead4e0"));
        assert_eq!(label(&shake.client, b"iv", b"", 12)[12..], [0; 20]);
        assert_eq!(finished(&shake.server, &hellos).to_vec(), hex("f5734610330be420694695fcfeb8da83f1635bfdca3e103c0ab8dacd36dfd31f"));
        assert_eq!(next(&traffic.client).to_vec(), hex("5deae78ceb5c9e1b841a346c79aec0c07126c975ff581c1ae50a7d5ee0a358ef"));
    }

    #[test]
    fn a_record_opens_only_whole_in_its_turn_and_under_its_key() {
        let (mut ours, mut theirs) = (Seal::new(&[7; 32]), Seal::new(&[7; 32]));
        let mut first = [0u8; 64];
        let length = ours.seal(22, b"hello", &mut first);
        assert_eq!((length, &first[..5]), (5 + 5 + 17, &[23, 3, 3, 0, 22][..]));
        assert!(!first[5..length].windows(5).any(|five| five == b"hello"), "nothing readable");
        let mut second = [0u8; 64];
        let second_length = ours.seal(23, b"", &mut second);
        // a copy changed on the way, or cut, does not open, and uses up no number of theirs
        for at in 0..length {
            let mut changed = first;
            changed[at] ^= 1;
            let (head, body) = changed.split_at_mut(5);
            assert_eq!(Seal { key: theirs.key.clone(), iv: theirs.iv, count: theirs.count }.open(head, &mut body[..length - 5]), None, "byte {at}");
        }
        let (head, body) = first.split_at_mut(5);
        assert_eq!(Seal { key: theirs.key.clone(), iv: theirs.iv, count: 0 }.open(head, &mut body[..length - 6]), None);
        assert_eq!(Seal::new(&[8; 32]).open(head, &mut body[..length - 5]), None, "another key");
        // out of turn: the second before the first
        let (second_head, second_body) = second.split_at_mut(5);
        let mut early = [0u8; 64];
        early[..second_length - 5].copy_from_slice(&second_body[..second_length - 5]);
        assert_eq!(Seal { key: theirs.key.clone(), iv: theirs.iv, count: 0 }.open(second_head, &mut early[..second_length - 5]), None);
        // in turn, each opens: what was said and its kind; an empty one too
        assert_eq!(theirs.open(head, &mut body[..length - 5]), Some((22, 5)));
        assert_eq!(&body[..5], b"hello");
        assert_eq!(theirs.open(second_head, &mut second_body[..second_length - 5]), Some((23, 0)));
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"abc", b"ab"));
    }
}
