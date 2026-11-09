//! RFC 8785 JSON Canonicalization Scheme (JCS), used for check definition and environment
//! digests (E-6, E-7).
//!
//! - Object members are sorted by the UTF-16 code units of their names.
//! - Strings use the minimal JSON escapes; everything else is written as UTF-8.
//! - Numbers are IEEE-754 doubles written as ECMAScript's `Number.prototype.toString` does.
//! - No whitespace.

use std::fmt::Write as _;

use serde_json::Value;

/// Why a value has no canonical form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JcsError {
    #[error("JSON numbers must be finite")]
    NonFinite,
}

/// The canonical form of `value`.
pub fn canonicalize(value: &Value) -> Result<String, JcsError> {
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out)
}

fn write_value(value: &Value, out: &mut String) -> Result<(), JcsError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            let f = n.as_f64().ok_or(JcsError::NonFinite)?;
            write_number(f, out)?;
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(members) => {
            let mut names: Vec<&String> = members.keys().collect();
            names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, name) in names.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(name, out);
                out.push(':');
                write_value(&members[name], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// ECMAScript `Number.prototype.toString` for a finite double (ECMA-262 §6.1.6.1.20).
fn write_number(f: f64, out: &mut String) -> Result<(), JcsError> {
    if !f.is_finite() {
        return Err(JcsError::NonFinite);
    }
    if f == 0.0 {
        out.push('0');
        return Ok(());
    }
    if f < 0.0 {
        out.push('-');
    }
    let (digits, exponent) = shortest_digits(f.abs());
    let k = digits.len() as i32;
    let n = exponent + 1;
    if k <= n && n <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-n) as usize));
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = n - 1;
        let _ = write!(out, "e{}{}", if e >= 0 { '+' } else { '-' }, e.abs());
    }
    Ok(())
}

/// The shortest digits that round-trip to `f` (positive, finite), and the decimal exponent of
/// the first digit, as ECMAScript chooses them.
fn shortest_digits(f: f64) -> (String, i32) {
    let sci = format!("{f:e}");
    let (mantissa, exponent) = sci.split_once('e').expect("`{:e}` has an exponent");
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exponent: i32 = exponent.parse().expect("integer exponent");
    // When the exact value lies exactly halfway between two shortest candidates, ECMAScript
    // takes the one with the even last digit, while Rust's formatting rounds up. A tie means
    // the exact value is the midpoint between this candidate and the one below it.
    let last = digits.as_bytes()[digits.len() - 1] - b'0';
    if last % 2 == 1 && !(digits.len() == 1 && last == 1) {
        let mut below = digits[..digits.len() - 1].to_string();
        below.push(char::from(b'0' + last - 1));
        let midpoint = format!("{below}5");
        // Formatting with a precision is exact; a double has at most 767 significant digits.
        let exact = format!("{f:.1100e}");
        let (exact_mantissa, exact_exponent) = exact.split_once('e').expect("exponent");
        let exact_digits: String = exact_mantissa.chars().filter(|c| *c != '.').collect();
        if exact_exponent.parse::<i32>() == Ok(exponent)
            && exact_digits.trim_end_matches('0') == midpoint
        {
            digits = below;
        }
    }
    (digits, exponent)
}

/// SHA-256 of the canonical form of `value`.
pub fn digest(value: &Value) -> Result<crate::Digest, JcsError> {
    Ok(crate::Digest::of(canonicalize(value)?.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(bits: u64) -> String {
        let mut out = String::new();
        write_number(f64::from_bits(bits), &mut out).unwrap();
        out
    }

    // covers: E-6
    #[test]
    fn rfc8785_example() {
        // RFC 8785 §3.2.2.
        let input = r#"{
          "numbers": [333333333.33333329, 1E30, 4.50, 2e-3, 0.000000000000000000000000001],
          "string": "\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/",
          "literals": [null, true, false]
        }"#;
        let value: Value = serde_json::from_str(input).unwrap();
        assert_eq!(
            canonicalize(&value).unwrap(),
            r#"{"literals":[null,true,false],"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"string":"€$\u000f\nA'B\"\\\\\"/"}"#
        );
    }

    // covers: E-6
    #[test]
    fn rfc8785_member_order_is_utf16() {
        // RFC 8785 §3.2.3: sorting by UTF-16 code units, not code points.
        let input = r#"{
          "€": "Euro Sign",
          "\r": "Carriage Return",
          "דּ": "Hebrew Letter Dalet With Dagesh",
          "1": "One",
          "😀": "Emoji: Grinning Face",
          "\u0080": "Control",
          "ö": "Latin Small Letter O With Diaeresis"
        }"#;
        let value: Value = serde_json::from_str(input).unwrap();
        let canonical = canonicalize(&value).unwrap();
        let order: Vec<&str> = [
            "\\r",
            "1",
            "\u{80}",
            "\u{f6}",
            "\u{20ac}",
            "\u{1f600}",
            "\u{fb33}",
        ]
        .into_iter()
        .collect();
        let mut last = 0;
        for key in order {
            let at = canonical.find(&format!("\"{key}\"")).unwrap();
            assert!(at >= last, "{key} out of order in {canonical}");
            last = at;
        }
    }

    // covers: E-6
    #[test]
    fn rfc8785_numbers() {
        // RFC 8785 Appendix B (IEEE-754 bits → ECMAScript form).
        for (bits, expected) in [
            (0x0000000000000000u64, "0"),
            (0x8000000000000000, "0"),
            (0x0000000000000001, "5e-324"),
            (0x8000000000000001, "-5e-324"),
            (0x7fefffffffffffff, "1.7976931348623157e+308"),
            (0xffefffffffffffff, "-1.7976931348623157e+308"),
            (0x4340000000000000, "9007199254740992"),
            (0xc340000000000000, "-9007199254740992"),
            (0x4430000000000000, "295147905179352830000"),
            (0x44b52d02c7e14af5, "9.999999999999997e+22"),
            (0x44b52d02c7e14af6, "1e+23"),
            (0x44b52d02c7e14af7, "1.0000000000000001e+23"),
            (0x444b1ae4d6e2ef4e, "999999999999999700000"),
            (0x444b1ae4d6e2ef4f, "999999999999999900000"),
            (0x444b1ae4d6e2ef50, "1e+21"),
            (0x3eb0c6f7a0b5ed8c, "9.999999999999997e-7"),
            (0x3eb0c6f7a0b5ed8d, "0.000001"),
            (0x41b3de4355555553, "333333333.3333332"),
            (0x41b3de4355555554, "333333333.33333325"),
            (0x41b3de4355555555, "333333333.3333333"),
            (0x41b3de4355555556, "333333333.3333334"),
            (0x41b3de4355555557, "333333333.33333343"),
            (0xbecbf647612f3696, "-0.0000033333333333333333"),
            (0x43143ff3c1cb0959, "1424953923781206.2"),
        ] {
            assert_eq!(number(bits), expected, "{bits:#018x}");
        }
        let mut out = String::new();
        assert_eq!(write_number(f64::NAN, &mut out), Err(JcsError::NonFinite));
        assert_eq!(
            write_number(f64::INFINITY, &mut out),
            Err(JcsError::NonFinite)
        );
    }

    #[test]
    fn control_characters_and_unicode() {
        let value = serde_json::json!({"a": "\u{1}\u{1f}\u{7f}é/", "b": [1, 2.5, -0.0]});
        assert_eq!(
            canonicalize(&value).unwrap(),
            "{\"a\":\"\\u0001\\u001f\u{7f}é/\",\"b\":[1,2.5,0]}"
        );
    }
}
