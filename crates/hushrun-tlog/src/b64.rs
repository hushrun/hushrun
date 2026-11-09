//! Standard base64 with padding (RFC 4648 §4), as signed notes and checkpoints write it.
//! Decoding is strict: one encoding per byte string, so a hash or a signature has exactly one
//! text form.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `data` as standard, padded base64.
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        let sextet = |shift: u32| char::from(ALPHABET[((n >> shift) & 63) as usize]);
        out.push(sextet(18));
        out.push(sextet(12));
        out.push(if chunk.len() > 1 { sextet(6) } else { '=' });
        out.push(if chunk.len() > 2 { sextet(0) } else { '=' });
    }
    out
}

/// Decodes standard, padded base64. `None` for anything else: other characters, missing or
/// misplaced padding, or unused bits that aren't zero.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let groups = bytes.len() / 4;
    let mut out = Vec::with_capacity(groups * 3);
    for (i, group) in bytes.chunks(4).enumerate() {
        let pad = group.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && i + 1 != groups) {
            return None;
        }
        let mut n = 0u32;
        for (j, &c) in group.iter().enumerate() {
            let value = if j >= 4 - pad { 0 } else { value(c)? };
            n = (n << 6) | value;
        }
        let unused = match pad {
            1 => n & 0xff,
            2 => n & 0xffff,
            _ => 0,
        };
        if unused != 0 {
            return None;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

fn value(c: u8) -> Option<u32> {
    Some(u32::from(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 4648 §10.
    #[test]
    fn rfc_4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in cases {
            assert_eq!(encode(plain.as_bytes()), encoded);
            assert_eq!(decode(encoded).as_deref(), Some(plain.as_bytes()));
        }
        assert_eq!(encode(&[0xfb, 0xff]), "+/8=");
        assert_eq!(decode("+/8=").unwrap(), [0xfb, 0xff]);
    }

    #[test]
    fn decoding_is_strict() {
        for bad in [
            "Zg",
            "Zg=",
            "Zg===",
            "Z===",
            "Zm9v=Zm9",
            "Zh==",
            "Zm9=",
            "Zm9v Zg==",
            "Zm-v",
            "Zm_v",
            "====",
        ] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn round_trips() {
        let data: Vec<u8> = (0..=255u8).collect();
        for n in 0..data.len() {
            assert_eq!(decode(&encode(&data[..n])).unwrap(), &data[..n]);
        }
    }
}
