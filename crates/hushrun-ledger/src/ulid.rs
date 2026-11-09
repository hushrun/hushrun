//! ULIDs for event keys: 48 bits of milliseconds and 80 random bits, in Crockford base32.
//! They make keys unique; they carry no authority (E-9).

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A new ULID for `millis` since the Unix epoch.
pub fn new_ulid(millis: i64) -> String {
    let mut random = [0u8; 10];
    getrandom::fill(&mut random).expect("the OS random number generator works");
    encode(millis, &random)
}

/// The milliseconds a ULID encodes, or `None` if `ulid` isn't one.
pub fn ulid_millis(ulid: &str) -> Option<i64> {
    if ulid.len() != 26 {
        return None;
    }
    let mut ms: i64 = 0;
    for c in ulid[..10].bytes() {
        let digit = ALPHABET.iter().position(|a| *a == c.to_ascii_uppercase())?;
        ms = (ms << 5) | digit as i64;
    }
    // 10 characters carry 50 bits; a ULID's timestamp is 48.
    (ms < (1 << 48)).then_some(ms)
}

fn encode(millis: i64, random: &[u8; 10]) -> String {
    let ms = u128::try_from(millis.max(0)).unwrap_or(0) & ((1 << 48) - 1);
    let mut value = ms << 80;
    for (i, byte) in random.iter().enumerate() {
        value |= u128::from(*byte) << (8 * (9 - i));
    }
    (0..26)
        .rev()
        .map(|i| ALPHABET[((value >> (5 * i)) & 31) as usize] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_the_ulid_spec() {
        // The spec's example timestamp 1469918176385 is 01ARYZ6S41.
        assert_eq!(&encode(1_469_918_176_385, &[0; 10])[..10], "01ARYZ6S41");
        assert_eq!(encode(0, &[0; 10]), "00000000000000000000000000");
        assert_eq!(encode(0, &[0xff; 10]), "0000000000ZZZZZZZZZZZZZZZZ");
        let a = new_ulid(1_000);
        let b = new_ulid(1_000);
        assert_eq!(a.len(), 26);
        assert_ne!(a, b);
        assert!(new_ulid(1_000) < new_ulid(2_000));
    }

    #[test]
    fn decodes_the_timestamp() {
        assert_eq!(
            ulid_millis(&new_ulid(1_469_918_176_385)),
            Some(1_469_918_176_385)
        );
        assert_eq!(
            ulid_millis("01ARYZ6S41TSV4RRFFQ69G5FAV"),
            Some(1_469_918_176_385)
        );
        assert_eq!(
            ulid_millis("8ZZZZZZZZZ0000000000000000"),
            None,
            "past 48 bits"
        );
        assert_eq!(ulid_millis("01ARYZ6S41"), None);
        assert_eq!(
            ulid_millis("01ARYZ6S4U0000000000000000"),
            None,
            "U isn't Crockford"
        );
    }
}
