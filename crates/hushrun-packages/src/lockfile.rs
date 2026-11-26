//! What the JavaScript parts read of a lockfile: a tarball's `integrity` (`sha512-<base64>`), a
//! SHA-512 in hex, and JSON that allows a comma before a closing bracket, as `bun.lock` writes it.

use base64::Engine as _;
use sha2_hw::Digest as _;

/// The lowercase hex SHA-512 of `bytes`.
pub fn sha512_hex(bytes: &[u8]) -> String {
    sha2_hw::Sha512::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The SHA-512 an `integrity` names (`sha512-<base64>`, among others separated by spaces), in
/// lowercase hex.
pub fn integrity_hex(integrity: &str) -> Option<String> {
    integrity.split_whitespace().find_map(|one| {
        let b64 = one.strip_prefix("sha512-")?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        (bytes.len() == 64).then(|| bytes.iter().map(|b| format!("{b:02x}")).collect())
    })
}

/// Whether `hex` is 128 lowercase hex characters: a SHA-512.
pub fn is_sha512_hex(hex: &str) -> bool {
    hex.len() == 128
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `text` as JSON, a comma before `}` or `]` allowed (outside strings).
pub fn json_with_trailing_commas(text: &str) -> Result<serde_json::Value, serde_json::Error> {
    let mut out = String::with_capacity(text.len());
    let (mut in_string, mut escaped) = (false, false);
    for (i, c) in text.char_indices() {
        if in_string {
            out.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ',' => {
                let next = text[i + 1..].trim_start().chars().next();
                if !matches!(next, Some('}' | ']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    serde_json::from_str(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: K-3
    #[test]
    fn an_integrity_is_a_sha512_and_trailing_commas_are_read() {
        let hex = sha512_hex(b"chalk");
        let b64 = base64::engine::general_purpose::STANDARD.encode(
            (0..64)
                .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
                .collect::<Vec<u8>>(),
        );
        assert_eq!(
            integrity_hex(&format!("sha1-abc sha512-{b64}")).as_deref(),
            Some(hex.as_str())
        );
        assert!(is_sha512_hex(&hex));
        assert_eq!(integrity_hex("sha1-abc"), None);
        let v = json_with_trailing_commas("{\"a\": [1, 2,], \"b\": \"x,]\",\n}").unwrap();
        assert_eq!(v["a"][1], 2);
        assert_eq!(v["b"], "x,]");
    }
}
