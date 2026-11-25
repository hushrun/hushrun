//! \[ADR 0121\] A reader of `project.pbxproj`, the old-style property list an Xcode project is
//! kept in: dictionaries `{ key = value; }`, arrays `( a, b, )`, strings quoted or bare, data
//! `<hex>`, and `/* … */` or `// …` comments anywhere. Nothing is written back; what Xcode would
//! refuse is refused here too, and the project is then `not observed`.

use std::collections::BTreeMap;

/// One value of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A string, quoted or bare, escapes read.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// A dictionary, by key.
    Dict(BTreeMap<String, Value>),
    /// Data, `<…>`, as its bytes.
    Data(Vec<u8>),
}

impl Value {
    /// The string, when this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The dictionary, when this is one.
    pub fn as_dict(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// The array, when this is one.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The value under `key`, when this is a dictionary that has it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_dict()?.get(key)
    }

    /// The string under `key`.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    /// The strings of the array under `key`; none when it isn't one.
    pub fn strs(&self, key: &str) -> Vec<&str> {
        self.get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect()
    }
}

/// Why the text isn't a property list, with the byte it stopped at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub at: usize,
    pub what: &'static str,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.what, self.at)
    }
}

/// The deepest nesting read: a project is a few levels deep; anything past this is no project.
const DEEPEST: usize = 64;

/// Reads the whole text as one value (the `// !$*UTF8*$!` line is a comment).
pub fn parse(text: &str) -> Result<Value, Error> {
    let mut p = Parser {
        bytes: text.as_bytes(),
        at: 0,
    };
    let value = p.value(0)?;
    p.skip();
    if p.at != p.bytes.len() {
        return Err(p.error("text after the value"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn error(&self, what: &'static str) -> Error {
        Error { at: self.at, what }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    /// Passes white space and comments.
    fn skip(&mut self) {
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_whitespace() => self.at += 1,
                Some(b'/') if self.bytes.get(self.at + 1) == Some(&b'*') => {
                    match find(&self.bytes[self.at + 2..], b"*/") {
                        Some(end) => self.at += 2 + end + 2,
                        None => self.at = self.bytes.len(),
                    }
                }
                Some(b'/') if self.bytes.get(self.at + 1) == Some(&b'/') => {
                    while let Some(b) = self.peek() {
                        self.at += 1;
                        if b == b'\n' {
                            break;
                        }
                    }
                }
                _ => return,
            }
        }
    }

    fn expect(&mut self, byte: u8, what: &'static str) -> Result<(), Error> {
        self.skip();
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.error(what))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > DEEPEST {
            return Err(self.error("nesting too deep"));
        }
        self.skip();
        match self.peek() {
            Some(b'{') => {
                self.at += 1;
                let mut dict = BTreeMap::new();
                loop {
                    self.skip();
                    if self.peek() == Some(b'}') {
                        self.at += 1;
                        return Ok(Value::Dict(dict));
                    }
                    let key = self.string()?;
                    self.expect(b'=', "no `=` after a key")?;
                    let value = self.value(depth + 1)?;
                    self.expect(b';', "no `;` after a value")?;
                    dict.insert(key, value);
                }
            }
            Some(b'(') => {
                self.at += 1;
                let mut array = Vec::new();
                loop {
                    self.skip();
                    if self.peek() == Some(b')') {
                        self.at += 1;
                        return Ok(Value::Array(array));
                    }
                    array.push(self.value(depth + 1)?);
                    self.skip();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b')') => {}
                        _ => return Err(self.error("no `,` between values")),
                    }
                }
            }
            Some(b'<') => {
                self.at += 1;
                let mut hex = Vec::new();
                loop {
                    match self.peek() {
                        Some(b'>') => {
                            self.at += 1;
                            break;
                        }
                        Some(b) if b.is_ascii_hexdigit() => {
                            hex.push(b);
                            self.at += 1;
                        }
                        Some(b) if b.is_ascii_whitespace() => self.at += 1,
                        _ => return Err(self.error("bad data")),
                    }
                }
                if hex.len() % 2 != 0 {
                    return Err(self.error("odd data"));
                }
                let bytes = hex
                    .chunks(2)
                    .map(|pair| {
                        let text = std::str::from_utf8(pair).unwrap_or("00");
                        u8::from_str_radix(text, 16).unwrap_or(0)
                    })
                    .collect();
                Ok(Value::Data(bytes))
            }
            Some(_) => Ok(Value::String(self.string()?)),
            None => Err(self.error("the text ended")),
        }
    }

    fn string(&mut self) -> Result<String, Error> {
        self.skip();
        match self.peek() {
            Some(quote @ (b'"' | b'\'')) => {
                self.at += 1;
                let mut out: Vec<u8> = Vec::new();
                loop {
                    let Some(b) = self.peek() else {
                        return Err(self.error("a string never ended"));
                    };
                    self.at += 1;
                    if b == quote {
                        break;
                    }
                    if b != b'\\' {
                        out.push(b);
                        continue;
                    }
                    let Some(e) = self.peek() else {
                        return Err(self.error("a string never ended"));
                    };
                    self.at += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'a' => out.push(7),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'v' => out.push(11),
                        b'U' | b'u' => {
                            let end = (self.at + 4).min(self.bytes.len());
                            let hex = std::str::from_utf8(&self.bytes[self.at..end])
                                .map_err(|_| self.error("bad escape"))?;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| self.error("bad escape"))?;
                            self.at = end;
                            let c = char::from_u32(code).unwrap_or('\u{fffd}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                        }
                        b'0'..=b'7' => {
                            let mut code = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        code = code * 8 + u32::from(d - b'0');
                                        self.at += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(u8::try_from(code & 0xff).unwrap_or(0));
                        }
                        other => out.push(other),
                    }
                }
                String::from_utf8(out).map_err(|_| self.error("a string isn't UTF-8"))
            }
            Some(b) if bare(b) => {
                let start = self.at;
                while self.peek().is_some_and(bare) {
                    self.at += 1;
                }
                Ok(String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned())
            }
            _ => Err(self.error("no string where one should be")),
        }
    }
}

/// What a bare string may hold.
fn bare(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'/' | b':' | b'.' | b'-' | b'+')
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-110
    #[test]
    fn a_project_file_reads_with_its_comments_quotes_and_trailing_commas() {
        let text = r#"// !$*UTF8*$!
{
	archiveVersion = 1;
	objects = {
		AB /* Lib.swift in Sources */ = {isa = PBXBuildFile; fileRef = CD /* Lib.swift */; };
		CD = {isa = PBXFileReference; path = "My \"Lib\".swift"; sourceTree = "<group>"; };
		EF = {isa = PBXGroup; children = (CD /* x */, ); data = <0aff 10>; };
	};
	rootObject = EF;
}
"#;
        let v = parse(text).unwrap();
        assert_eq!(v.str("archiveVersion"), Some("1"));
        let objects = v.get("objects").unwrap();
        assert_eq!(
            objects.get("CD").unwrap().str("path"),
            Some("My \"Lib\".swift")
        );
        assert_eq!(objects.get("EF").unwrap().strs("children"), vec!["CD"]);
        assert_eq!(
            objects.get("EF").unwrap().get("data"),
            Some(&Value::Data(vec![0x0a, 0xff, 0x10]))
        );
        assert_eq!(objects.get("AB").unwrap().str("isa"), Some("PBXBuildFile"));
        assert_eq!(v.str("rootObject"), Some("EF"));
    }

    // covers: C-110
    #[test]
    fn escapes_read_as_xcode_writes_them() {
        let v = parse(r#"{ a = "x\ny\\z\U00e9"; b = "\101"; }"#).unwrap();
        assert_eq!(v.str("a"), Some("x\ny\\zé"));
        assert_eq!(v.str("b"), Some("A"));
    }

    // covers: C-110
    #[test]
    fn what_is_no_property_list_is_refused() {
        for bad in [
            "{ a = b }",
            "{ a b; }",
            "( a b )",
            "{ a = \"b; }",
            "{ a = b; } c",
            "<abc>",
            "",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let deep = format!("{}{}", "(".repeat(100), ")".repeat(100));
        assert!(parse(&deep).is_err());
    }
}
