//! \[C-147\] The part of a SCIP index (<https://github.com/sourcegraph/scip>, `scip.proto`) the
//! reach walk reads: each document's path and its occurrences, each a range, a symbol and its
//! roles. A small protobuf reader of its own: the fields it doesn't know are skipped, and bytes it
//! can't read are an error, never a shorter index.

use std::collections::BTreeMap;

/// SCIP's `SymbolRole.Definition`.
pub const DEFINITION: i32 = 1;

/// One occurrence: where, what, and whether it defines its symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// Zero-based, as SCIP has them: start line and character, end line and character.
    pub range: [u32; 4],
    pub symbol: String,
    pub roles: i32,
}

impl Occurrence {
    pub fn is_definition(&self) -> bool {
        self.roles & DEFINITION != 0
    }

    /// Whether the symbol is one of its document's locals (`local 12`): never another file's.
    pub fn is_local(&self) -> bool {
        self.symbol.starts_with("local ")
    }
}

/// One document: its path, relative to the index's project root, and its occurrences.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub path: String,
    pub occurrences: Vec<Occurrence>,
    /// SCIP's `PositionEncoding`: 1 UTF-8 bytes, 2 UTF-16 code units, 3 UTF-32 (characters); 0
    /// unspecified, which this reader takes as UTF-8, as rust-analyzer writes.
    pub encoding: i32,
}

/// An index: its project root, as the tool wrote it (a `file://` URI), and its documents by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub project_root: String,
    pub documents: BTreeMap<String, Document>,
}

/// Why bytes aren't a SCIP index this reader can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the SCIP index can't be read: {}", self.0)
    }
}

impl std::error::Error for Malformed {}

/// Reads an index. A document that appears twice is merged.
pub fn parse(bytes: &[u8]) -> Result<Index, Malformed> {
    let mut index = Index::default();
    let mut r = Reader::new(bytes);
    while let Some((field, wire)) = r.key()? {
        match (field, wire) {
            (1, LEN) => {
                let mut m = Reader::new(r.bytes()?);
                while let Some((field, wire)) = m.key()? {
                    match (field, wire) {
                        (3, LEN) => index.project_root = m.string()?,
                        _ => m.skip(wire)?,
                    }
                }
            }
            (2, LEN) => {
                let doc = document(r.bytes()?)?;
                let entry = index
                    .documents
                    .entry(doc.path.clone())
                    .or_insert_with(|| Document {
                        path: doc.path.clone(),
                        occurrences: Vec::new(),
                        encoding: doc.encoding,
                    });
                entry.occurrences.extend(doc.occurrences);
            }
            _ => r.skip(wire)?,
        }
    }
    Ok(index)
}

fn document(bytes: &[u8]) -> Result<Document, Malformed> {
    let mut doc = Document::default();
    let mut r = Reader::new(bytes);
    while let Some((field, wire)) = r.key()? {
        match (field, wire) {
            (1, LEN) => doc.path = r.string()?,
            (2, LEN) => doc.occurrences.push(occurrence(r.bytes()?)?),
            (6, VARINT) => doc.encoding = r.varint()? as i32,
            _ => r.skip(wire)?,
        }
    }
    Ok(doc)
}

fn occurrence(bytes: &[u8]) -> Result<Occurrence, Malformed> {
    let mut range: Vec<u32> = Vec::new();
    let mut symbol = String::new();
    let mut roles = 0i32;
    let mut r = Reader::new(bytes);
    while let Some((field, wire)) = r.key()? {
        match (field, wire) {
            (1, LEN) => {
                let mut p = Reader::new(r.bytes()?);
                while !p.done() {
                    range.push(as_u32(p.varint()?)?);
                }
            }
            (1, VARINT) => range.push(as_u32(r.varint()?)?),
            (2, LEN) => symbol = r.string()?,
            // An int32 is sent as a 64-bit varint, a negative one sign-extended.
            (3, VARINT) => roles = r.varint()? as i32,
            _ => r.skip(wire)?,
        }
    }
    let range = match range.as_slice() {
        [line, start, end] => [*line, *start, *line, *end],
        [l0, c0, l1, c1] => [*l0, *c0, *l1, *c1],
        _ => {
            return Err(Malformed(
                "an occurrence's range has neither 3 nor 4 numbers",
            ));
        }
    };
    Ok(Occurrence {
        range,
        symbol,
        roles,
    })
}

fn as_u32(v: u64) -> Result<u32, Malformed> {
    u32::try_from(v).map_err(|_| Malformed("a position past 32 bits"))
}

const VARINT: u8 = 0;
const I64: u8 = 1;
const LEN: u8 = 2;
const I32: u8 = 5;

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, at: 0 }
    }

    fn done(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn varint(&mut self) -> Result<u64, Malformed> {
        let mut out = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self
                .bytes
                .get(self.at)
                .ok_or(Malformed("a number ends early"))?;
            self.at += 1;
            out |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(out);
            }
        }
        Err(Malformed("a number longer than 10 bytes"))
    }

    fn key(&mut self) -> Result<Option<(u64, u8)>, Malformed> {
        if self.done() {
            return Ok(None);
        }
        let key = self.varint()?;
        Ok(Some((key >> 3, (key & 7) as u8)))
    }

    fn bytes(&mut self) -> Result<&'a [u8], Malformed> {
        let len = usize::try_from(self.varint()?).map_err(|_| Malformed("a length past usize"))?;
        let end = self
            .at
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(Malformed("a field ends past its message"))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn string(&mut self) -> Result<String, Malformed> {
        String::from_utf8(self.bytes()?.to_vec())
            .map_err(|_| Malformed("a string that isn't UTF-8"))
    }

    fn skip(&mut self, wire: u8) -> Result<(), Malformed> {
        let len = match wire {
            VARINT => return self.varint().map(drop),
            I64 => 8,
            LEN => return self.bytes().map(drop),
            I32 => 4,
            _ => return Err(Malformed("a field of a wire type protobuf has no more")),
        };
        if self.at + len > self.bytes.len() {
            return Err(Malformed("a field ends past its message"));
        }
        self.at += len;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Protobuf bytes, written as the reader reads them, for tests.
    #[derive(Default)]
    pub struct Writer(pub Vec<u8>);

    impl Writer {
        pub fn varint(&mut self, mut v: u64) -> &mut Self {
            loop {
                let byte = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    self.0.push(byte);
                    return self;
                }
                self.0.push(byte | 0x80);
            }
        }
        pub fn key(&mut self, field: u64, wire: u8) -> &mut Self {
            self.varint((field << 3) | u64::from(wire))
        }
        pub fn len(&mut self, field: u64, bytes: &[u8]) -> &mut Self {
            self.key(field, LEN).varint(bytes.len() as u64);
            self.0.extend_from_slice(bytes);
            self
        }
    }

    /// One occurrence of a test's index: its range, symbol and roles.
    pub type Occ<'a> = ([u32; 4], &'a str, i32);

    /// An index of `docs`: each a path and its occurrences.
    pub fn index_bytes(root: &str, docs: &[(&str, Vec<Occ<'_>>)]) -> Vec<u8> {
        let mut index = Writer::default();
        let mut meta = Writer::default();
        meta.len(3, root.as_bytes());
        index.len(1, &meta.0);
        for (path, occurrences) in docs {
            let mut doc = Writer::default();
            doc.len(1, path.as_bytes());
            for (range, symbol, roles) in occurrences {
                let mut packed = Writer::default();
                for n in range {
                    packed.varint(u64::from(*n));
                }
                let mut occ = Writer::default();
                occ.len(1, &packed.0).len(2, symbol.as_bytes());
                occ.key(3, VARINT).varint(*roles as u64);
                doc.len(2, &occ.0);
            }
            index.len(2, &doc.0);
        }
        index.0
    }

    // covers: C-147
    #[test]
    fn an_index_is_read_and_damage_is_an_error() {
        let bytes = index_bytes(
            "file:///w",
            &[(
                "src/lib.rs",
                vec![([0, 3, 0, 6], "rust-analyzer cargo a 0.1.0 f().", 1)],
            )],
        );
        let index = parse(&bytes).unwrap();
        assert_eq!(index.project_root, "file:///w");
        let doc = &index.documents["src/lib.rs"];
        assert_eq!(doc.occurrences.len(), 1);
        assert!(doc.occurrences[0].is_definition());
        assert_eq!(doc.occurrences[0].range, [0, 3, 0, 6]);
        // Every cut of it but at a field's end is an error, never a shorter index.
        let whole = parse(&bytes).unwrap();
        for cut in 1..bytes.len() {
            if let Ok(short) = parse(&bytes[..cut]) {
                assert_ne!(short, whole, "a cut at {cut} read as the whole index");
            }
        }
        assert!(parse(&[0x12, 0x05, 0x0a]).is_err());
    }
}
