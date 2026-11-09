//! Signing keys (E-2): Ed25519 and \[M10\] ECDSA P-256, as PKCS#8 PEM private keys,
//! `ed25519:<base64>` and `ecdsa-p256:<base64>` public keys, and `sha256:<hex>` key ids.
//! Private keys never appear in logs or `Debug` output.
//!
//! P-256 signatures are ASN.1 DER `ECDSA-Sig-Value`s over SHA-256, the form X.509 and Sigstore
//! use. They are made and checked with ring, the TLS provider's crypto.

use std::fmt;

use ed25519_dalek::Signer as _;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use ring::rand::SystemRandom;
use ring::signature::{self as ring_sig, EcdsaKeyPair, KeyPair as _};
use zeroize::Zeroizing;

use hushrun_core::signers::PublicKey;

use crate::signer::{SignError, Signer};

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("the private key is not a PKCS#8 PEM Ed25519 or ECDSA P-256 key ({0})")]
    Pem(String),
    #[error("could not read randomness from the OS: {0}")]
    Random(String),
}

/// The PEM label of a PKCS#8 private key.
const PRIVATE_KEY_LABEL: &str = "PRIVATE KEY";

/// A signing key: Ed25519, or ECDSA P-256.
pub enum SigningKey {
    Ed25519(ed25519_dalek::SigningKey),
    P256(P256Key),
}

/// An ECDSA P-256 key pair, and its PKCS#8 document for writing it back.
pub struct P256Key {
    pair: EcdsaKeyPair,
    pkcs8: Zeroizing<Vec<u8>>,
}

impl SigningKey {
    /// A new Ed25519 key from the OS random number generator.
    pub fn generate() -> Result<SigningKey, KeyError> {
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(seed.as_mut()).map_err(|e| KeyError::Random(e.to_string()))?;
        Ok(SigningKey::from_seed(&seed))
    }

    /// A new ECDSA P-256 key from the OS random number generator.
    pub fn generate_p256() -> Result<SigningKey, KeyError> {
        let rng = SystemRandom::new();
        let document =
            EcdsaKeyPair::generate_pkcs8(&ring_sig::ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
                .map_err(|_| KeyError::Random("generating a P-256 key".into()))?;
        SigningKey::from_p256_pkcs8(document.as_ref())
    }

    /// An Ed25519 key from its 32-byte seed (tests use fixed seeds).
    pub fn from_seed(seed: &[u8; 32]) -> SigningKey {
        SigningKey::Ed25519(ed25519_dalek::SigningKey::from_bytes(seed))
    }

    /// Parses a PKCS#8 PEM private key (`-----BEGIN PRIVATE KEY-----`): Ed25519 or P-256.
    pub fn from_pem(pem: &str) -> Result<SigningKey, KeyError> {
        let pem = pem.trim();
        if let Ok(key) = ed25519_dalek::SigningKey::from_pkcs8_pem(pem) {
            return Ok(SigningKey::Ed25519(key));
        }
        let (label, der) = der::pem::decode_vec(pem.as_bytes())
            .map_err(|e| KeyError::Pem(format!("not PEM: {e}")))?;
        let der = Zeroizing::new(der);
        if label != PRIVATE_KEY_LABEL {
            return Err(KeyError::Pem(format!(
                "a {label} block; convert it with `openssl pkcs8 -topk8 -nocrypt`"
            )));
        }
        SigningKey::from_p256_pkcs8(&der)
    }

    fn from_p256_pkcs8(der: &[u8]) -> Result<SigningKey, KeyError> {
        let pair = EcdsaKeyPair::from_pkcs8(
            &ring_sig::ECDSA_P256_SHA256_ASN1_SIGNING,
            der,
            &SystemRandom::new(),
        )
        .map_err(|e| KeyError::Pem(e.to_string()))?;
        Ok(SigningKey::P256(P256Key {
            pair,
            pkcs8: Zeroizing::new(der.to_vec()),
        }))
    }

    /// The PKCS#8 PEM form, wiped from memory when dropped.
    pub fn to_pem(&self) -> Zeroizing<String> {
        match self {
            SigningKey::Ed25519(key) => key
                .to_pkcs8_pem(LineEnding::LF)
                .expect("an Ed25519 key always encodes"),
            SigningKey::P256(key) => Zeroizing::new(
                der::pem::encode_string(PRIVATE_KEY_LABEL, der::pem::LineEnding::LF, &key.pkcs8)
                    .expect("a PKCS#8 document always encodes"),
            ),
        }
    }

    pub fn public_key(&self) -> PublicKey {
        match self {
            SigningKey::Ed25519(key) => PublicKey::Ed25519(key.verifying_key().to_bytes()),
            SigningKey::P256(key) => PublicKey::from_p256_point(key.pair.public_key().as_ref())
                .expect("ring's P-256 public keys are uncompressed points"),
        }
    }

    /// `sha256:<hex>` of the public key (E-2).
    pub fn keyid(&self) -> String {
        self.public_key().keyid()
    }

    /// A signature over `message`: 64 bytes for Ed25519, a DER `ECDSA-Sig-Value` for P-256.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        match self {
            SigningKey::Ed25519(key) => key.sign(message).to_bytes().to_vec(),
            SigningKey::P256(key) => key
                .pair
                .sign(&SystemRandom::new(), message)
                .expect("P-256 signing fails only without OS randomness")
                .as_ref()
                .to_vec(),
        }
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SigningKey({})", self.keyid())
    }
}

impl Signer for SigningKey {
    fn public_key(&self) -> Option<PublicKey> {
        Some(SigningKey::public_key(self))
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SignError> {
        Ok(SigningKey::sign(self, message))
    }

    fn describe(&self) -> String {
        format!("key {}", self.keyid())
    }
}

/// Verifies a signature by `public` over `message`: Ed25519 strictly (no small-order keys, no
/// malleable signatures), P-256 as a DER `ECDSA-Sig-Value` over SHA-256.
pub fn verify_signature(public: &PublicKey, message: &[u8], signature: &[u8]) -> bool {
    match public {
        PublicKey::Ed25519(raw) => {
            let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(raw) else {
                return false;
            };
            let Ok(signature) = ed25519_dalek::Signature::from_slice(signature) else {
                return false;
            };
            key.verify_strict(message, &signature).is_ok()
        }
        PublicKey::P256(point) => {
            ring_sig::UnparsedPublicKey::new(&ring_sig::ECDSA_P256_SHA256_ASN1, point.as_slice())
                .verify(message, signature)
                .is_ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // covers: E-2
    #[test]
    fn rfc8032_vectors() {
        // RFC 8032 §7.1, tests 1-3.
        for (seed, public, message, signature) in [
            (
                "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
                "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
                "",
                "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
            ),
            (
                "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
                "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
                "72",
                "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
            ),
            (
                "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
                "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
                "af82",
                "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
            ),
        ] {
            let key = SigningKey::from_seed(&unhex(seed).try_into().unwrap());
            assert_eq!(
                key.public_key(),
                PublicKey::Ed25519(unhex(public).try_into().unwrap())
            );
            let sig = key.sign(&unhex(message));
            assert_eq!(sig, unhex(signature));
            assert!(verify_signature(&key.public_key(), &unhex(message), &sig));
            assert!(!verify_signature(&key.public_key(), b"other", &sig));
        }
    }

    // covers: E-2
    #[test]
    fn pem_round_trip_and_key_ids() {
        let key = SigningKey::generate().unwrap();
        let pem = key.to_pem();
        assert!(
            pem.starts_with("-----BEGIN PRIVATE KEY-----\n"),
            "{}",
            pem.as_str()
        );
        let back = SigningKey::from_pem(&pem).unwrap();
        assert_eq!(back.public_key(), key.public_key());
        let keyid = key.keyid();
        assert!(keyid.starts_with("sha256:") && keyid.len() == 71);
        assert!(key.public_key().to_string().starts_with("ed25519:"));
        assert!(
            SigningKey::from_pem("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----")
                .is_err()
        );
        // Debug never shows the secret.
        let shown = format!("{key:?}");
        assert_eq!(shown, format!("SigningKey({keyid})"));
    }

    // covers: E-2
    #[test]
    fn p256_keys_sign_and_verify() {
        let key = SigningKey::generate_p256().unwrap();
        let public = key.public_key();
        assert_eq!(public.algorithm(), "ecdsa-p256");
        let pem = key.to_pem();
        assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----\n"));
        let back = SigningKey::from_pem(&pem).unwrap();
        assert_eq!(back.public_key(), public);
        let signature = key.sign(b"message");
        assert_eq!(signature[0], 0x30, "a DER ECDSA-Sig-Value");
        assert!(verify_signature(&public, b"message", &signature));
        assert!(verify_signature(
            &public,
            b"message",
            &back.sign(b"message")
        ));
        assert!(!verify_signature(&public, b"other", &signature));
        // Another key's signature, and an Ed25519 key, don't verify.
        let other = SigningKey::generate_p256().unwrap();
        assert!(!verify_signature(
            &other.public_key(),
            b"message",
            &signature
        ));
        let ed = SigningKey::from_seed(&[1; 32]);
        assert!(!verify_signature(&ed.public_key(), b"message", &signature));
        assert!(!verify_signature(&public, b"message", &ed.sign(b"message")));
        assert_eq!(
            format!("{key:?}"),
            format!("SigningKey({})", public.keyid())
        );
    }

    // covers: E-2
    #[test]
    fn reads_p256_keys_openssl_wrote() {
        let pem = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../testdata/keys/p256-1.pem"
        ))
        .unwrap();
        let key = SigningKey::from_pem(&pem).unwrap();
        assert_eq!(
            key.public_key().to_string(),
            "ecdsa-p256:MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE6hj7Jxp4+YP5tnNglObbgpECMJDO5JNJEtrs+qceALsSV4Y+HJ+wbCn77fmLMsWLA5Db7UUqhbR4CQoOwcYRCg=="
        );
        let sec1 = "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIA==\n-----END EC PRIVATE KEY-----";
        let err = SigningKey::from_pem(sec1).unwrap_err().to_string();
        assert!(err.contains("openssl pkcs8 -topk8"), "{err}");
    }
}
