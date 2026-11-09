//! A minimal reader of raw DER TLVs, for the byte ranges that signatures cover: a certificate's
//! `TBSCertificate`, a CMS signer's signed attributes. Typed decoding would re-encode them; a
//! signature covers the bytes as they were written.

/// One DER TLV: its tag byte, all of its bytes, and its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tlv<'a> {
    pub tag: u8,
    pub whole: &'a [u8],
    pub value: &'a [u8],
}

/// The TLV at the start of `bytes`, and what follows it. Only single-byte tags and definite
/// lengths: DER needs nothing else for the structures read here.
pub(crate) fn next(bytes: &[u8]) -> Result<(Tlv<'_>, &[u8]), String> {
    let short = || "truncated DER".to_string();
    let tag = *bytes.first().ok_or_else(short)?;
    if tag & 0x1f == 0x1f {
        return Err("a multi-byte DER tag".into());
    }
    let first = *bytes.get(1).ok_or_else(short)?;
    let (length, header) = if first & 0x80 == 0 {
        (usize::from(first), 2)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return Err("an indefinite or oversized DER length".into());
        }
        let digits = bytes.get(2..2 + count).ok_or_else(short)?;
        if digits[0] == 0 {
            return Err("a DER length with a leading zero".into());
        }
        let length = digits
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        if length < 0x80 {
            return Err("a long-form DER length for a short value".into());
        }
        (length, 2 + count)
    };
    let end = header.checked_add(length).ok_or_else(short)?;
    let whole = bytes.get(..end).ok_or_else(short)?;
    Ok((
        Tlv {
            tag,
            whole,
            value: &whole[header..],
        },
        &bytes[end..],
    ))
}

/// The TLVs inside a constructed value, in order.
pub(crate) fn children(mut value: &[u8]) -> Result<Vec<Tlv<'_>>, String> {
    let mut out = Vec::new();
    while !value.is_empty() {
        let (tlv, rest) = next(value)?;
        out.push(tlv);
        value = rest;
    }
    Ok(out)
}

/// The single TLV that is all of `bytes`.
pub(crate) fn only(bytes: &[u8]) -> Result<Tlv<'_>, String> {
    let (tlv, rest) = next(bytes)?;
    if !rest.is_empty() {
        return Err("trailing bytes after DER".into());
    }
    Ok(tlv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_short_and_long_lengths() {
        let (tlv, rest) = next(&[0x30, 0x03, 0x02, 0x01, 0x05, 0xff]).unwrap();
        assert_eq!(
            (tlv.tag, tlv.value, rest),
            (0x30, &[0x02, 0x01, 0x05][..], &[0xff][..])
        );
        assert_eq!(children(tlv.value).unwrap()[0].value, &[0x05]);
        let mut long = vec![0x04, 0x81, 0x80];
        long.extend([7u8; 0x80]);
        assert_eq!(only(&long).unwrap().value.len(), 0x80);
        for bad in [
            &[0x30][..],
            &[0x30, 0x05, 0x00],
            &[0x30, 0x80],
            &[0x30, 0x81, 0x05, 0, 0, 0, 0, 0],
            &[0x1f, 0x01, 0x00],
        ] {
            assert!(next(bad).is_err(), "{bad:?}");
        }
        assert!(only(&[0x05, 0x00, 0x00]).is_err());
    }
}
