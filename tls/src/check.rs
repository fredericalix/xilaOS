//! Who a server is: its certificates, and the signature that proves it
//! holds the key of the first one.
//!
//! A certificate says "this key belongs to the machine of this name",
//! signed by an authority, whose own certificate may be signed by
//! another, up to one that is trusted beforehand: a root. Going up that
//! chain, with the dates, the names and what each certificate may be
//! used for, is the work of `rustls-webpki`, the library the `rustls`
//! project checks certificates with. It is the part of TLS where a
//! small mistake lets anyone pass for anyone, so it is not written
//! again here. What this file gives it is the arithmetic of the
//! signatures, from the RustCrypto libraries.
//!
//! Nothing asks whether a certificate was taken back by its authority
//! since it was made.

use core::time::Duration;

use rustls_pki_types::{alg_id, AlgorithmIdentifier, CertificateDer, InvalidSignature, ServerName, SignatureVerificationAlgorithm, TrustAnchor, UnixTime};
use sha2::{Digest, Sha256, Sha384, Sha512};
use webpki::{EndEntityCert, KeyUsage};

/// Why a certificate is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Its time is over, by the clock of this machine.
    Expired,
    /// Its time has not come yet, by the clock of this machine.
    NotYet,
    /// It is for a machine of another name.
    WrongName,
    /// No authority that is trusted vouches for it.
    Unknown,
    /// It is signed in a way XilaOS cannot check.
    Unsupported,
    /// A signature in the chain is not that of who is said to have signed.
    Forged,
    /// It cannot be read, or may not be used by a server.
    Bad,
}

fn refused(why: webpki::Error) -> Refused {
    use webpki::Error;
    match why {
        Error::CertExpired { .. } => Refused::Expired,
        Error::CertNotValidYet { .. } => Refused::NotYet,
        Error::CertNotValidForName(_) => Refused::WrongName,
        Error::UnknownIssuer => Refused::Unknown,
        Error::InvalidSignatureForPublicKey => Refused::Forged,
        #[allow(deprecated)]
        Error::UnsupportedSignatureAlgorithm
        | Error::UnsupportedSignatureAlgorithmContext(_)
        | Error::UnsupportedSignatureAlgorithmForPublicKey
        | Error::UnsupportedSignatureAlgorithmForPublicKeyContext(_) => Refused::Unsupported,
        _ => Refused::Bad,
    }
}

/// A signature on one of the two curves, over a message hashed one of
/// two ways: authorities use all four.
#[derive(Debug)]
struct Curve {
    bits: u16,
    hash: u16,
}

impl SignatureVerificationAlgorithm for Curve {
    fn verify_signature(&self, key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), InvalidSignature> {
        let (short, long);
        let hash: &[u8] = if self.hash == 256 {
            short = Sha256::digest(message);
            &short
        } else {
            long = Sha384::digest(message);
            &long
        };
        if self.bits == 256 {
            use p256::ecdsa::signature::hazmat::PrehashVerifier;
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(key).map_err(|_| InvalidSignature)?;
            let signature = p256::ecdsa::Signature::from_der(signature).map_err(|_| InvalidSignature)?;
            key.verify_prehash(hash, &signature).map_err(|_| InvalidSignature)
        } else {
            use p384::ecdsa::signature::hazmat::PrehashVerifier;
            let key = p384::ecdsa::VerifyingKey::from_sec1_bytes(key).map_err(|_| InvalidSignature)?;
            let signature = p384::ecdsa::Signature::from_der(signature).map_err(|_| InvalidSignature)?;
            key.verify_prehash(hash, &signature).map_err(|_| InvalidSignature)
        }
    }

    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        if self.bits == 256 {
            alg_id::ECDSA_P256
        } else {
            alg_id::ECDSA_P384
        }
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        if self.hash == 256 {
            alg_id::ECDSA_SHA256
        } else {
            alg_id::ECDSA_SHA384
        }
    }
}

/// An RSA signature, padded in the old way (PKCS#1 v1.5, which
/// certificates still use) or the new (PSS, which TLS 1.3 asks of
/// servers), over a message hashed one of three ways. The `rsa` library
/// is the one thing here that needs a heap.
#[derive(Debug)]
struct Rsa {
    pss: bool,
    hash: u16,
}

impl SignatureVerificationAlgorithm for Rsa {
    fn verify_signature(&self, key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), InvalidSignature> {
        use rsa::pkcs1::DecodeRsaPublicKey;
        use rsa::signature::Verifier;
        let key = rsa::PublicKey::from_pkcs1_der(key).map_err(|_| InvalidSignature)?;
        macro_rules! check {
            ($hash:ty) => {
                if self.pss {
                    let signature = rsa::pss::Signature::try_from(signature).map_err(|_| InvalidSignature)?;
                    rsa::pss::VerifyingKey::<$hash>::new(key).verify(message, &signature).map_err(|_| InvalidSignature)
                } else {
                    let signature = rsa::pkcs1v15::Signature::try_from(signature).map_err(|_| InvalidSignature)?;
                    rsa::pkcs1v15::VerifyingKey::<$hash>::new(key).verify(message, &signature).map_err(|_| InvalidSignature)
                }
            };
        }
        match self.hash {
            256 => check!(Sha256),
            384 => check!(Sha384),
            _ => check!(Sha512),
        }
    }

    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_ENCRYPTION
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        match (self.pss, self.hash) {
            (false, 256) => alg_id::RSA_PKCS1_SHA256,
            (false, 384) => alg_id::RSA_PKCS1_SHA384,
            (false, _) => alg_id::RSA_PKCS1_SHA512,
            (true, 256) => alg_id::RSA_PSS_SHA256,
            (true, 384) => alg_id::RSA_PSS_SHA384,
            (true, _) => alg_id::RSA_PSS_SHA512,
        }
    }
}

static P256_SHA256: Curve = Curve { bits: 256, hash: 256 };
static P384_SHA384: Curve = Curve { bits: 384, hash: 384 };
static PSS_SHA256: Rsa = Rsa { pss: true, hash: 256 };
static PSS_SHA384: Rsa = Rsa { pss: true, hash: 384 };
static PSS_SHA512: Rsa = Rsa { pss: true, hash: 512 };

/// Every way a certificate may be signed that is checked here.
static IN_CERTIFICATES: &[&dyn SignatureVerificationAlgorithm] = &[
    &P256_SHA256,
    &P384_SHA384,
    &Curve { bits: 256, hash: 384 },
    &Curve { bits: 384, hash: 256 },
    &Rsa { pss: false, hash: 256 },
    &Rsa { pss: false, hash: 384 },
    &Rsa { pss: false, hash: 512 },
    &PSS_SHA256,
    &PSS_SHA384,
    &PSS_SHA512,
];

/// The numbers TLS gives the ways of signing, in the order they are
/// offered to a server: for its own signature the first five, and the
/// last three for certificates only.
pub(crate) const SCHEMES: [u16; 8] = [0x0403, 0x0503, 0x0804, 0x0805, 0x0806, 0x0401, 0x0501, 0x0601];

/// The most certificates a server may show: its own and those between
/// it and a root.
pub(crate) const CHAIN: usize = 6;

/// The authorities that browsers trust, as Mozilla lists them.
pub fn public_roots() -> &'static [TrustAnchor<'static>] {
    webpki_roots::TLS_SERVER_ROOTS
}

/// Checks the certificates a server showed, its own first: that they
/// lead to one of `roots`, that each is in its time at `now` (seconds
/// since 1970), and that the first is for a server of that `name`.
pub fn chain(certificates: &[&[u8]], name: &[u8], now: u64, roots: &[TrustAnchor<'_>]) -> Result<(), Refused> {
    let (first, others) = certificates.split_first().ok_or(Refused::Bad)?;
    let first = CertificateDer::from(*first);
    let leaf = EndEntityCert::try_from(&first).map_err(refused)?;
    let mut between: [CertificateDer; CHAIN] = core::array::from_fn(|_| CertificateDer::from(&[][..]));
    let count = others.len().min(CHAIN);
    for (place, certificate) in between.iter_mut().zip(others) {
        *place = CertificateDer::from(*certificate);
    }
    let time = UnixTime::since_unix_epoch(Duration::from_secs(now));
    leaf.verify_for_usage(IN_CERTIFICATES, roots, &between[..count], time, KeyUsage::server_auth(), None, None).map_err(refused)?;
    let name = core::str::from_utf8(name).ok().and_then(|name| ServerName::try_from(name).ok()).ok_or(Refused::WrongName)?;
    leaf.verify_is_valid_for_subject_name(&name).map_err(refused)
}

/// Whether `signature`, made in the way TLS numbers `scheme`, is that of
/// the key of a certificate over a message: the proof a server gives
/// that the certificate it showed is its own.
pub(crate) fn signed(certificate: &[u8], scheme: u16, message: &[u8], signature: &[u8]) -> bool {
    let way: &dyn SignatureVerificationAlgorithm = match scheme {
        0x0403 => &P256_SHA256,
        0x0503 => &P384_SHA384,
        0x0804 => &PSS_SHA256,
        0x0805 => &PSS_SHA384,
        0x0806 => &PSS_SHA512,
        _ => return false,
    };
    let certificate = CertificateDer::from(certificate);
    EndEntityCert::try_from(&certificate).is_ok_and(|leaf| leaf.verify_signature(way, message, signature).is_ok())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::boxed::Box;
    use std::vec::Vec;

    pub const ROOT: &[u8] = include_bytes!("../fixtures/root.der");
    pub const BETWEEN: &[u8] = include_bytes!("../fixtures/between.der");
    pub const LEAF: &[u8] = include_bytes!("../fixtures/leaf.der");
    pub const OTHER: &[u8] = include_bytes!("../fixtures/other.der");
    pub const STRANGER: &[u8] = include_bytes!("../fixtures/stranger.der");
    const MIXED: &[u8] = include_bytes!("../fixtures/mixed.der");
    const DIRECT: &[u8] = include_bytes!("../fixtures/direct.der");
    const RSA_ROOT: &[u8] = include_bytes!("../fixtures/rsa-root.der");
    const RSA_LEAF: &[u8] = include_bytes!("../fixtures/rsa-leaf.der");

    /// A day of January 2027, within the hundred years of the made-up
    /// certificates.
    pub const NOW: u64 = 1_800_000_000;

    /// The made-up authorities as the only ones trusted.
    pub fn roots() -> &'static [TrustAnchor<'static>] {
        let kept: Vec<TrustAnchor<'static>> = [ROOT, RSA_ROOT]
            .iter()
            .map(|root| {
                let root: &'static CertificateDer<'static> = Box::leak(Box::new(CertificateDer::from(*root)));
                webpki::anchor_from_trusted_cert(root).unwrap()
            })
            .collect();
        Box::leak(kept.into_boxed_slice())
    }

    #[test]
    fn a_chain_that_leads_to_a_trusted_root_is_taken() {
        let roots = roots();
        assert_eq!(chain(&[LEAF, BETWEEN], b"xila.test", NOW, roots), Ok(()));
        // capitals in a name do not matter, and what else the server shows does not hurt
        assert_eq!(chain(&[LEAF, BETWEEN, ROOT, OTHER], b"XILA.Test", NOW, roots), Ok(()));
        // each curve with the other's hash, and RSA
        assert_eq!(chain(&[MIXED, BETWEEN], b"xila.test", NOW, roots), Ok(()));
        assert_eq!(chain(&[DIRECT], b"xila.test", NOW, roots), Ok(()));
        assert_eq!(chain(&[RSA_LEAF], b"xila.test", NOW, roots), Ok(()));
    }

    #[test]
    fn what_does_not_prove_who_the_server_is_is_refused() {
        let roots = roots();
        // the certificate of another machine, however good
        assert_eq!(chain(&[LEAF, BETWEEN], b"other.test", NOW, roots), Err(Refused::WrongName));
        assert_eq!(chain(&[OTHER, BETWEEN], b"xila.test", NOW, roots), Err(Refused::WrongName));
        assert_eq!(chain(&[LEAF, BETWEEN], b"test", NOW, roots), Err(Refused::WrongName));
        assert_eq!(chain(&[LEAF, BETWEEN], b"www.xila.test", NOW, roots), Err(Refused::WrongName));
        assert_eq!(chain(&[LEAF, BETWEEN], b"not a name", NOW, roots), Err(Refused::WrongName));
        // an authority nobody trusts, a missing link, no certificate at all
        assert_eq!(chain(&[STRANGER], b"xila.test", NOW, roots), Err(Refused::Unknown));
        assert_eq!(chain(&[LEAF], b"xila.test", NOW, roots), Err(Refused::Unknown));
        assert_eq!(chain(&[LEAF, BETWEEN], b"xila.test", NOW, public_roots()), Err(Refused::Unknown));
        assert_eq!(chain(&[], b"xila.test", NOW, roots), Err(Refused::Bad));
        // an authority's certificate is not a server's
        assert_eq!(chain(&[BETWEEN], b"xila.test", NOW, roots), Err(Refused::Bad));
        // out of its time: before it was made, and a hundred and fifty years on
        assert_eq!(chain(&[LEAF, BETWEEN], b"xila.test", 1_700_000_000, roots), Err(Refused::NotYet));
        assert_eq!(chain(&[LEAF, BETWEEN], b"xila.test", NOW + 150 * 365 * 86_400, roots), Err(Refused::Expired));
        // one bit changed anywhere: unreadable, about someone else, or a signature that no longer holds
        for at in (0..LEAF.len()).step_by(7) {
            let mut changed = LEAF.to_vec();
            changed[at] ^= 0x10;
            assert!(chain(&[&changed, BETWEEN], b"xila.test", NOW, roots).is_err(), "byte {at} of the server's");
        }
        for at in (0..BETWEEN.len()).step_by(7) {
            let mut changed = BETWEEN.to_vec();
            changed[at] ^= 0x10;
            assert!(chain(&[LEAF, &changed], b"xila.test", NOW, roots).is_err(), "byte {at} of the authority's");
        }
        let mut changed = RSA_LEAF.to_vec();
        *changed.last_mut().unwrap() ^= 1;
        assert_eq!(chain(&[&changed], b"xila.test", NOW, roots), Err(Refused::Forged));
    }

    #[test]
    fn the_public_roots_are_there() {
        assert!(public_roots().len() > 100);
    }
}
