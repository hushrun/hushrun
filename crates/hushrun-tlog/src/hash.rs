//! SHA-256 hashes, and RFC 6962's domain-separated leaf and node hashes (RFC 9162 §2.1.1).

use std::fmt;

use ring::digest::{Context, SHA256, digest};

use crate::b64;

/// Bytes in a hash.
pub const HASH_LEN: usize = 32;

/// A SHA-256 hash: of bytes, of a leaf, or of a node of the tree.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash(pub [u8; HASH_LEN]);

impl Hash {
    /// SHA-256 of `data`.
    pub fn of(data: &[u8]) -> Hash {
        Hash::from_slice(digest(&SHA256, data).as_ref()).expect("SHA-256 is 32 bytes")
    }

    /// The root of the empty tree: SHA-256 of nothing (RFC 6962 §2.1).
    pub fn empty() -> Hash {
        Hash::of(&[])
    }

    /// A leaf's hash: SHA-256 of `0x00` and the entry.
    pub fn leaf(entry: &[u8]) -> Hash {
        let mut context = Context::new(&SHA256);
        context.update(&[0x00]);
        context.update(entry);
        Hash::from_slice(context.finish().as_ref()).expect("SHA-256 is 32 bytes")
    }

    /// An interior node's hash: SHA-256 of `0x01`, the left child and the right child.
    pub fn node(left: &Hash, right: &Hash) -> Hash {
        let mut context = Context::new(&SHA256);
        context.update(&[0x01]);
        context.update(&left.0);
        context.update(&right.0);
        Hash::from_slice(context.finish().as_ref()).expect("SHA-256 is 32 bytes")
    }

    /// A hash from exactly 32 bytes.
    pub fn from_slice(bytes: &[u8]) -> Option<Hash> {
        bytes.try_into().ok().map(Hash)
    }

    /// Lowercase hex.
    pub fn hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(HASH_LEN * 2);
        for byte in self.0 {
            out.push(char::from(DIGITS[usize::from(byte >> 4)]));
            out.push(char::from(DIGITS[usize::from(byte & 0xf)]));
        }
        out
    }

    /// Parses 64 lowercase hex digits; anything else (uppercase included) is `None`, so a hash
    /// has one text form.
    pub fn from_hex(text: &str) -> Option<Hash> {
        let bytes = text.as_bytes();
        if bytes.len() != HASH_LEN * 2 {
            return None;
        }
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut out = [0u8; HASH_LEN];
        for (i, pair) in bytes.chunks(2).enumerate() {
            out[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
        }
        Some(Hash(out))
    }

    /// Standard, padded base64, as checkpoints write roots.
    pub fn base64(&self) -> String {
        b64::encode(&self.0)
    }

    /// Parses standard, padded base64 of exactly 32 bytes.
    pub fn from_base64(text: &str) -> Option<Hash> {
        Hash::from_slice(&b64::decode(text)?)
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", self.hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hashes() {
        assert_eq!(
            Hash::empty().hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // RFC 6962's first test leaf, the empty entry.
        assert_eq!(
            Hash::leaf(b"").hex(),
            "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d"
        );
    }

    #[test]
    fn text_forms() {
        let hash = Hash::of(b"hushrun");
        assert_eq!(Hash::from_hex(&hash.hex()), Some(hash));
        assert_eq!(Hash::from_base64(&hash.base64()), Some(hash));
        assert_eq!(Hash::from_hex(&hash.hex().to_uppercase()), None);
        assert_eq!(Hash::from_hex(&hash.hex()[1..]), None);
        assert_eq!(Hash::from_base64("AAAA"), None);
        assert_eq!(format!("{hash}"), hash.hex());
    }

    #[test]
    fn domains_are_separated() {
        let a = Hash::of(b"a");
        let b = Hash::of(b"b");
        let mut joined = a.0.to_vec();
        joined.extend_from_slice(&b.0);
        assert_ne!(Hash::node(&a, &b), Hash::leaf(&joined));
        assert_ne!(Hash::node(&a, &b), Hash::node(&b, &a));
    }
}
