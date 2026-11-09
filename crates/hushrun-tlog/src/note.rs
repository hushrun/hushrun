//! C2SP `signed-note` (<https://c2sp.org/signed-note>) with Ed25519 keys.
//!
//! A note is UTF-8 text that ends in a newline, then a blank line, then one signature line per
//! signer: an em dash, a space, the key name, a space, and the standard base64 of the 4-byte
//! key hash and the signature. The key hash is the first four bytes of
//! SHA-256(name, `\n`, the algorithm byte `0x01`, the 32-byte public key). Verifier keys are
//! written `<name>+<hash as 8 hex digits>+<base64 of 0x01 and the public key>`, as Go's
//! `golang.org/x/mod/sumdb/note` writes them.

use std::collections::BTreeSet;
use std::fmt;

use ring::digest::{Context, SHA256};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

use crate::b64;

/// The largest note this crate opens.
pub const MAX_NOTE_BYTES: usize = 1 << 20;
/// The most signature lines a note may have.
pub const MAX_SIGNATURES: usize = 100;
/// The algorithm byte of Ed25519 keys.
const ED25519_ALGORITHM: u8 = 0x01;
/// What every signature line starts with: an em dash and a space.
pub const SIGNATURE_PREFIX: &str = "\u{2014} ";

/// Why a note or a key doesn't parse or verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteError {
    /// The text isn't note text: empty, no final newline, a control character, or a blank
    /// line.
    InvalidText,
    /// A key name with a space or a `+`, or an empty one.
    InvalidName(String),
    /// A verifier or signer key that doesn't parse.
    InvalidKey(String),
    /// The note doesn't have the shape of a signed note.
    Malformed(String),
    /// More than [`MAX_NOTE_BYTES`].
    TooLarge,
    /// A signature by a known key doesn't verify.
    InvalidSignature { name: String },
    /// No signature by a known key.
    Unverified { names: Vec<String> },
}

impl fmt::Display for NoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoteError::InvalidText => f.write_str(
                "note text must end in a newline and have no blank lines or control characters",
            ),
            NoteError::InvalidName(name) => {
                write!(
                    f,
                    "{name:?} can't name a key: it is empty or has a space or a `+`"
                )
            }
            NoteError::InvalidKey(why) => write!(f, "invalid note key: {why}"),
            NoteError::Malformed(why) => write!(f, "not a signed note: {why}"),
            NoteError::TooLarge => write!(f, "the note is larger than {MAX_NOTE_BYTES} bytes"),
            NoteError::InvalidSignature { name } => {
                write!(f, "the signature by {name} doesn't verify")
            }
            NoteError::Unverified { names } if names.is_empty() => {
                f.write_str("the note has no signature")
            }
            NoteError::Unverified { names } => write!(
                f,
                "the note has no signature by a known key; it is signed by {}",
                names.join(", ")
            ),
        }
    }
}

impl std::error::Error for NoteError {}

/// The key hash of an Ed25519 key named `name`.
pub fn key_hash(name: &str, public: &[u8; 32]) -> u32 {
    let mut context = Context::new(&SHA256);
    context.update(name.as_bytes());
    context.update(b"\n");
    context.update(&[ED25519_ALGORITHM]);
    context.update(public);
    let digest = context.finish();
    u32::from_be_bytes(digest.as_ref()[..4].try_into().expect("4 bytes"))
}

/// Whether `name` may name a key: printable ASCII without spaces or `+`.
pub fn valid_key_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic() && b != b'+')
}

/// Whether `text` is note text this crate signs: non-empty, ending in a newline, without
/// control characters other than newlines, and without blank lines.
pub fn valid_text(text: &str) -> bool {
    text.ends_with('\n')
        && !text.starts_with('\n')
        && !text.contains("\n\n")
        && !text
            .chars()
            .any(|c| c != '\n' && (c.is_ascii_control() || c == '\u{7f}'))
}

/// An Ed25519 key that verifies notes.
#[derive(Clone, PartialEq, Eq)]
pub struct Verifier {
    name: String,
    hash: u32,
    public: [u8; 32],
}

impl Verifier {
    /// The verifier of the key `public`, named `name`.
    pub fn new(name: &str, public: [u8; 32]) -> Result<Verifier, NoteError> {
        if !valid_key_name(name) {
            return Err(NoteError::InvalidName(name.to_string()));
        }
        Ok(Verifier {
            name: name.to_string(),
            hash: key_hash(name, &public),
            public,
        })
    }

    /// Parses `<name>+<hash>+<base64 of 0x01 and the key>`.
    pub fn parse(vkey: &str) -> Result<Verifier, NoteError> {
        let invalid = |why: &str| NoteError::InvalidKey(format!("{vkey:?}: {why}"));
        // The key's base64 may hold `+` too: only the first two separate fields.
        let mut parts = vkey.splitn(3, '+');
        let (Some(name), Some(hash), Some(key)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(invalid("not <name>+<hash>+<key>"));
        };
        let hash = parse_hash(hash).ok_or_else(|| invalid("the hash is not 8 hex digits"))?;
        let public = decode_key(key).ok_or_else(|| invalid("not an Ed25519 key"))?;
        let verifier = Verifier::new(name, public)?;
        if verifier.hash != hash {
            return Err(invalid("the hash doesn't match the name and key"));
        }
        Ok(verifier)
    }

    /// `<name>+<hash>+<base64 of 0x01 and the key>`.
    pub fn vkey(&self) -> String {
        format!(
            "{}+{:08x}+{}",
            self.name,
            self.hash,
            b64::encode(&key_bytes(&self.public))
        )
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn hash(&self) -> u32 {
        self.hash
    }

    pub fn public_key(&self) -> &[u8; 32] {
        &self.public
    }

    /// Whether `signature` is this key's Ed25519 signature of `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        signature.len() == 64
            && UnparsedPublicKey::new(&ED25519, &self.public)
                .verify(message, signature)
                .is_ok()
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Verifier({})", self.vkey())
    }
}

/// An Ed25519 key that signs notes. `Debug` never shows the private key.
pub struct Signer {
    name: String,
    hash: u32,
    key: Ed25519KeyPair,
    seed: [u8; 32],
}

impl Signer {
    /// The signer of the key with this 32-byte seed, named `name`.
    pub fn new(name: &str, seed: [u8; 32]) -> Result<Signer, NoteError> {
        if !valid_key_name(name) {
            return Err(NoteError::InvalidName(name.to_string()));
        }
        let key = Ed25519KeyPair::from_seed_unchecked(&seed)
            .map_err(|e| NoteError::InvalidKey(e.to_string()))?;
        let public: [u8; 32] = key
            .public_key()
            .as_ref()
            .try_into()
            .expect("Ed25519 public keys are 32 bytes");
        Ok(Signer {
            name: name.to_string(),
            hash: key_hash(name, &public),
            key,
            seed,
        })
    }

    /// A new key from the system's random generator, and its seed, to keep.
    pub fn generate(name: &str) -> Result<(Signer, [u8; 32]), NoteError> {
        use ring::rand::{SecureRandom, SystemRandom};
        let mut seed = [0u8; 32];
        SystemRandom::new()
            .fill(&mut seed)
            .map_err(|_| NoteError::InvalidKey("the system has no random generator".into()))?;
        Ok((Signer::new(name, seed)?, seed))
    }

    /// Parses `PRIVATE+KEY+<name>+<hash>+<base64 of 0x01 and the seed>`.
    pub fn parse(skey: &str) -> Result<Signer, NoteError> {
        let invalid = |why: &str| NoteError::InvalidKey(format!("a private note key: {why}"));
        let rest = skey
            .strip_prefix("PRIVATE+KEY+")
            .ok_or_else(|| invalid("it doesn't start with PRIVATE+KEY+"))?;
        let mut parts = rest.splitn(3, '+');
        let (Some(name), Some(hash), Some(key)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(invalid("not PRIVATE+KEY+<name>+<hash>+<key>"));
        };
        let hash = parse_hash(hash).ok_or_else(|| invalid("the hash is not 8 hex digits"))?;
        let seed = decode_key(key).ok_or_else(|| invalid("not an Ed25519 seed"))?;
        let signer = Signer::new(name, seed)?;
        if signer.hash != hash {
            return Err(invalid("the hash doesn't match the name and key"));
        }
        Ok(signer)
    }

    /// `PRIVATE+KEY+<name>+<hash>+<base64 of 0x01 and the seed>`: a secret.
    pub fn skey(&self) -> String {
        format!(
            "PRIVATE+KEY+{}+{:08x}+{}",
            self.name,
            self.hash,
            b64::encode(&key_bytes(&self.seed))
        )
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.key
            .public_key()
            .as_ref()
            .try_into()
            .expect("Ed25519 public keys are 32 bytes")
    }

    /// The matching verifier.
    pub fn verifier(&self) -> Verifier {
        Verifier {
            name: self.name.clone(),
            hash: self.hash,
            public: self.public_key(),
        }
    }

    /// Signs `text` into a note with this key's signature.
    pub fn sign(&self, text: &str) -> Result<String, NoteError> {
        sign(text, &[self])
    }
}

impl fmt::Debug for Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signer({})", self.verifier().vkey())
    }
}

fn key_bytes(key: &[u8; 32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(33);
    bytes.push(ED25519_ALGORITHM);
    bytes.extend_from_slice(key);
    bytes
}

fn decode_key(text: &str) -> Option<[u8; 32]> {
    let bytes = b64::decode(text)?;
    let (&algorithm, key) = bytes.split_first()?;
    if algorithm != ED25519_ALGORITHM {
        return None;
    }
    key.try_into().ok()
}

fn parse_hash(text: &str) -> Option<u32> {
    (text.len() == 8 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(text, 16).ok())
        .flatten()
}

/// Signs `text` with each signer, in order.
pub fn sign(text: &str, signers: &[&Signer]) -> Result<String, NoteError> {
    if !valid_text(text) {
        return Err(NoteError::InvalidText);
    }
    let mut note = String::with_capacity(text.len() + 1 + signers.len() * 128);
    note.push_str(text);
    note.push('\n');
    for signer in signers {
        let signature = signer.key.sign(text.as_bytes());
        let mut blob = Vec::with_capacity(4 + 64);
        blob.extend_from_slice(&signer.hash.to_be_bytes());
        blob.extend_from_slice(signature.as_ref());
        note.push_str(SIGNATURE_PREFIX);
        note.push_str(&signer.name);
        note.push(' ');
        note.push_str(&b64::encode(&blob));
        note.push('\n');
    }
    Ok(note)
}

/// A signature line of an opened note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteSignature {
    pub name: String,
    pub hash: u32,
    /// Whether a known key made it; signatures by unknown keys are kept, unverified.
    pub verified: bool,
}

/// An opened note: its text, and its signatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub text: String,
    pub signatures: Vec<NoteSignature>,
}

/// A signature line of a note, parsed but not verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSignature {
    pub name: String,
    /// The key hash: the first four bytes of the signature.
    pub hash: u32,
    /// The signature after the key hash: an Ed25519 signature for `signed-note` keys, or what
    /// another signature type puts there.
    pub signature: Vec<u8>,
}

/// Splits a signed note into its text and its signature lines, without verifying any.
pub fn split(note: &[u8]) -> Result<(String, Vec<RawSignature>), NoteError> {
    if note.len() > MAX_NOTE_BYTES {
        return Err(NoteError::TooLarge);
    }
    let malformed = |why: &str| NoteError::Malformed(why.to_string());
    let note = std::str::from_utf8(note).map_err(|_| malformed("it isn't UTF-8"))?;
    let split = note
        .rfind("\n\n")
        .ok_or_else(|| malformed("no blank line before the signatures"))?;
    let (text, lines) = (&note[..split + 1], &note[split + 2..]);
    if !text.ends_with('\n')
        || text
            .chars()
            .any(|c| c != '\n' && (c.is_ascii_control() || c == '\u{7f}'))
    {
        return Err(NoteError::InvalidText);
    }
    let Some(lines) = lines.strip_suffix('\n') else {
        return Err(malformed("the signatures don't end in a newline"));
    };
    if lines.is_empty() {
        return Err(NoteError::Unverified { names: Vec::new() });
    }
    let mut signatures = Vec::new();
    for line in lines.split('\n') {
        if signatures.len() == MAX_SIGNATURES {
            return Err(malformed("too many signatures"));
        }
        let rest = line
            .strip_prefix(SIGNATURE_PREFIX)
            .ok_or_else(|| malformed("a signature line doesn't start with an em dash"))?;
        let (name, encoded) = rest
            .split_once(' ')
            .ok_or_else(|| malformed("a signature line has no signature"))?;
        if !valid_key_name(name) {
            return Err(NoteError::InvalidName(name.to_string()));
        }
        let blob = b64::decode(encoded).ok_or_else(|| malformed("a signature isn't base64"))?;
        if blob.len() < 5 {
            return Err(malformed("a signature is too short"));
        }
        signatures.push(RawSignature {
            name: name.to_string(),
            hash: u32::from_be_bytes(blob[..4].try_into().expect("4 bytes")),
            signature: blob[4..].to_vec(),
        });
    }
    Ok((text.to_string(), signatures))
}

/// Opens a signed note: the text and its signatures, at least one of them by a key in
/// `verifiers`. A signature by a known key that doesn't verify is an error; signatures by
/// other keys are kept unverified, as C2SP says.
pub fn open(note: &[u8], verifiers: &[&Verifier]) -> Result<Note, NoteError> {
    let (text, raw) = split(note)?;
    let mut signatures = Vec::with_capacity(raw.len());
    let mut seen = BTreeSet::new();
    for line in raw {
        let known = verifiers
            .iter()
            .find(|v| v.name == line.name && v.hash == line.hash);
        let verified = match known {
            Some(verifier) => {
                if !seen.insert((line.name.clone(), line.hash)) {
                    return Err(NoteError::Malformed("a key signed twice".into()));
                }
                if !verifier.verify(text.as_bytes(), &line.signature) {
                    return Err(NoteError::InvalidSignature { name: line.name });
                }
                true
            }
            None => false,
        };
        signatures.push(NoteSignature {
            name: line.name,
            hash: line.hash,
            verified,
        });
    }
    if !signatures.iter().any(|s| s.verified) {
        return Err(NoteError::Unverified {
            names: signatures.iter().map(|s| s.name.clone()).collect(),
        });
    }
    Ok(Note { text, signatures })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The example of Go's golang.org/x/mod/sumdb/note.
    const PETER_SKEY: &str =
        "PRIVATE+KEY+PeterNeumann+c74f20a3+AYEKFALVFGyNhPJEMzD1QIDr+Y7hfZx09iUvxdXHKDFz";
    const PETER_VKEY: &str = "PeterNeumann+c74f20a3+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW";
    const PETER_TEXT: &str = "If you think cryptography is the answer to your problem,\n\
                              then you don't know what your problem is.\n";
    const PETER_NOTE: &str = "If you think cryptography is the answer to your problem,\n\
        then you don't know what your problem is.\n\
        \n\
        \u{2014} PeterNeumann x08go/ZJkuBS9UG/SffcvIAQxVBtiFupLLr8pAcElZInNIuGUgYN1FFYC2pZSNXgKvqfqdngotpRZb6KE6RyyBwJnAM=\n";

    // covers: L-11
    #[test]
    fn signs_like_go() {
        let signer = Signer::parse(PETER_SKEY).unwrap();
        assert_eq!(signer.skey(), PETER_SKEY);
        assert_eq!(signer.verifier().vkey(), PETER_VKEY);
        assert_eq!(signer.sign(PETER_TEXT).unwrap(), PETER_NOTE);
        let verifier = Verifier::parse(PETER_VKEY).unwrap();
        let note = open(PETER_NOTE.as_bytes(), &[&verifier]).unwrap();
        assert_eq!(note.text, PETER_TEXT);
        assert_eq!(
            note.signatures,
            [NoteSignature {
                name: "PeterNeumann".into(),
                hash: 0xc74f20a3,
                verified: true
            }]
        );
        assert!(!format!("{signer:?}").contains("AYEKFALV"));
    }

    #[test]
    fn keys_parse_strictly() {
        let good = Verifier::parse(PETER_VKEY).unwrap();
        assert_eq!(good.name(), "PeterNeumann");
        assert_eq!(good.hash(), 0xc74f20a3);
        for bad in [
            "PeterNeumann+c74f20a4+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW",
            "PeterNeumann+c74f20a3+ApjcmQcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW",
            "PeterNeumann+c74f20a3",
            "Peter+Neumann+c74f20a3+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW",
            "+c74f20a3+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW",
            "PeterNeumann+c74f20a+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW",
        ] {
            assert!(Verifier::parse(bad).is_err(), "{bad}");
        }
        assert!(Signer::parse(&PETER_SKEY.replace("c74f20a3", "c74f20a4")).is_err());
        assert!(Signer::parse(PETER_VKEY).is_err());
        assert!(Signer::new("a name", [7; 32]).is_err());
        assert!(Signer::new("a+b", [7; 32]).is_err());
    }

    #[test]
    fn opening_checks_every_known_signature() {
        let verifier = Verifier::parse(PETER_VKEY).unwrap();
        // A changed text.
        let changed = PETER_NOTE.replace("cryptography", "cryptographs");
        assert_eq!(
            open(changed.as_bytes(), &[&verifier]),
            Err(NoteError::InvalidSignature {
                name: "PeterNeumann".into()
            })
        );
        // Another key's signature is kept, unverified, and doesn't count alone.
        let other = Signer::new("other.example/log", [9; 32]).unwrap();
        let note = sign(PETER_TEXT, &[&other, &Signer::parse(PETER_SKEY).unwrap()]).unwrap();
        let opened = open(note.as_bytes(), &[&verifier]).unwrap();
        assert_eq!(
            opened
                .signatures
                .iter()
                .map(|s| s.verified)
                .collect::<Vec<_>>(),
            [false, true]
        );
        let alone = other.sign(PETER_TEXT).unwrap();
        assert_eq!(
            open(alone.as_bytes(), &[&verifier]),
            Err(NoteError::Unverified {
                names: vec!["other.example/log".into()]
            })
        );
        // The same known key twice.
        let peter = Signer::parse(PETER_SKEY).unwrap();
        let twice = sign(PETER_TEXT, &[&peter, &peter]).unwrap();
        assert!(matches!(
            open(twice.as_bytes(), &[&verifier]),
            Err(NoteError::Malformed(_))
        ));
    }

    #[test]
    fn malformed_notes() {
        let verifier = Verifier::parse(PETER_VKEY).unwrap();
        let sig_line = PETER_NOTE.rsplit_once("\n\n").unwrap().1;
        for bad in [
            String::new(),
            PETER_TEXT.to_string(),
            format!("{PETER_TEXT}\n"),
            format!("{PETER_TEXT}\n{}", sig_line.trim_end()),
            format!("{PETER_TEXT}\n{}", sig_line.replace('\u{2014}', "-")),
            format!("{PETER_TEXT}\n\u{2014} PeterNeumann\n"),
            format!("{PETER_TEXT}\n\u{2014} PeterNeumann AAAA\n"),
            PETER_NOTE.replace("problem,", "problem,\u{7}"),
        ] {
            assert!(open(bad.as_bytes(), &[&verifier]).is_err(), "{bad:?}");
        }
        assert_eq!(
            open(&vec![b'a'; MAX_NOTE_BYTES + 1], &[&verifier]),
            Err(NoteError::TooLarge)
        );
    }

    #[test]
    fn texts_this_crate_signs() {
        assert!(valid_text("a\nb\n"));
        for bad in [
            "",
            "a",
            "\na\n",
            "a\n\nb\n",
            "a\tb\n",
            "a\rb\n",
            "a\u{7f}\n",
        ] {
            assert!(!valid_text(bad), "{bad:?}");
            let signer = Signer::new("x", [1; 32]).unwrap();
            assert_eq!(signer.sign(bad), Err(NoteError::InvalidText));
        }
    }

    #[test]
    fn generated_keys_sign() {
        let (signer, seed) = Signer::generate("hushrun.example/l/acme/back").unwrap();
        let again = Signer::new("hushrun.example/l/acme/back", seed).unwrap();
        assert_eq!(signer.public_key(), again.public_key());
        let note = signer.sign("hello\n").unwrap();
        assert!(open(note.as_bytes(), &[&again.verifier()]).is_ok());
    }
}
