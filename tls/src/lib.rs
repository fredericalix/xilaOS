//! TLS 1.3 for XilaOS: the side that calls a server.
//!
//! TLS is what makes a conversation over the Internet private and sure
//! of whom it is with: `https://` is HTTP spoken through it. This crate
//! speaks version 1.3 of it (RFC 8446), and only that one, as a client.
//!
//! Like `tcp.rs` in `xila-core`, it is a machine with no hardware and no
//! network under it: `Client::input` is given the bytes that came from
//! the server, `Client::output` hands out the bytes to send to it, and
//! between the two `write` and `read` carry what the conversation is
//! about. Whoever uses it moves the bytes over TCP.
//!
//! What is written here is the protocol: the messages of the handshake,
//! their order, the keys drawn at each step, the records. What is not
//! written here, and must not be, is what the protocol rests on. This is
//! the only outside code of XilaOS:
//!
//! - the key both sides agree on: X25519 (`x25519-dalek`);
//! - what seals the records: ChaCha20-Poly1305, and SHA-256 with HMAC to
//!   draw the keys (RustCrypto);
//! - the signatures of certificates and servers: ECDSA on the curves
//!   P-256 and P-384, and RSA (RustCrypto);
//! - whether a chain of certificates leads to a trusted authority
//!   (`rustls-webpki`), and the authorities that browsers trust
//!   (`webpki-roots`).
//!
//! `no_std`. The client itself allocates nothing: its buffers are in
//! `Client`, about 37 KB. The `rsa` library does allocate, while it
//! checks a signature, so whoever links this needs a heap.
//!
//! One way of each thing, on purpose: one key exchange, one cipher, and
//! a server that wants others is told there is nothing in common. No
//! TLS 1.2, no session taken up again later, no certificate of our own
//! to show, and nothing asks whether a certificate was revoked.
//!
//! The kernel is compiled without vector instructions, and four of the
//! libraries would choose them while running: `.cargo/config.toml` tells
//! each to use its plain code for that target.
#![no_std]

#[cfg(test)]
extern crate std;

mod check;
mod client;
mod keys;
#[cfg(test)]
mod tests;

pub use check::{chain, public_roots, Refused};
pub use client::{Client, Error, State, NAME};
pub use rustls_pki_types::{CertificateDer, TrustAnchor};
pub use webpki::anchor_from_trusted_cert;
