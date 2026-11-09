//! SHA-256 digests, written `sha256:<64 lowercase hex>`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The prefix of a written digest.
pub const SHA256_PREFIX: &str = "sha256:";

/// A SHA-256 digest: of a source manifest (D-5), a policy file, a check definition, an
/// artifact, and so on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest([u8; 32]);

/// Why a string is not a digest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DigestError {
    #[error("{0:?} is not a digest: expected sha256:<64 lowercase hex digits>")]
    Format(String),
}

impl Digest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The SHA-256 of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        use sha2::Digest as _;
        Self(sha2::Sha256::digest(bytes).into())
    }

    /// The 64 lowercase hex digits, without the `sha256:` prefix.
    pub fn hex(&self) -> String {
        to_hex(&self.0)
    }

    /// Parses `sha256:<64 lowercase hex>`.
    pub fn parse(text: &str) -> Result<Self, DigestError> {
        text.strip_prefix(SHA256_PREFIX)
            .and_then(parse_hex32)
            .map(Self)
            .ok_or_else(|| DigestError::Format(text.to_string()))
    }

    /// Parses 64 lowercase hex digits, without a prefix.
    pub fn from_hex(hex: &str) -> Result<Self, DigestError> {
        parse_hex32(hex)
            .map(Self)
            .ok_or_else(|| DigestError::Format(hex.to_string()))
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(SHA256_PREFIX)?;
        f.write_str(&self.hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Digest {
    type Err = DigestError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Lowercase hex of `bytes`.
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// 64 lowercase hex digits to 32 bytes. Uppercase digits are rejected: written digests are
/// always lowercase, and accepting two spellings would make string comparisons unsafe.
fn parse_hex32(hex: &str) -> Option<[u8; 32]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        out[i] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Some(out)
}

fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "5f0c0000000000000000000000000000000000000000000000000000000000e1";

    // covers: D-5
    #[test]
    fn round_trips_written_form() {
        let digest = Digest::parse(&format!("sha256:{HEX}")).unwrap();
        assert_eq!(digest.to_string(), format!("sha256:{HEX}"));
        assert_eq!(digest.hex(), HEX);
        assert_eq!(Digest::from_hex(HEX).unwrap(), digest);
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(json, format!("\"sha256:{HEX}\""));
        assert_eq!(serde_json::from_str::<Digest>(&json).unwrap(), digest);
    }

    #[test]
    fn rejects_other_spellings() {
        for bad in [
            HEX.to_string(),
            format!("sha256:{}", HEX.to_uppercase()),
            format!("sha512:{HEX}"),
            format!("sha256:{}", &HEX[1..]),
            format!("sha256:{HEX}0"),
            format!("sha256:{}g", &HEX[1..]),
        ] {
            assert!(Digest::parse(&bad).is_err(), "{bad}");
        }
    }
}
