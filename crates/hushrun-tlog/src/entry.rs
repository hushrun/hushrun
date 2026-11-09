//! What a leaf of the log records (L-11): one stored object, one configuration write, or one
//! deletion; \[M19\] or the root of the prefix map (L-12).
//!
//! An entry is a short text, and the leaf is RFC 6962's hash of it, `SHA-256(0x00 || entry)`:
//!
//! ```text
//! hushrun-tlog-entry/v1
//! object                        object, config or tombstone
//! v1/events/api/…/….started.production.json
//! 5f0c…e1                       SHA-256 of the bytes, lowercase hex
//! gc 9a1b…                      tombstones only: SHA-256 of the GC statement, or `gc none`
//! ```
//!
//! So a leaf is the hash of an object's key and of its bytes, through their SHA-256: a
//! deletion can drop the bytes and keep the leaf, and anyone holding the bytes can check them
//! against it. Names follow L-10's key rules, so they never hold a newline.
//!
//! \[M19\] A map entry has no name: it records the root of the prefix map ([`crate::map`]) of
//! the log's leaves before it, and the ledger ends every batch with one, so that every
//! checkpoint commits to its map.
//!
//! ```text
//! hushrun-tlog-entry/v1
//! map
//! 0e5f…77                       the map's root, lowercase hex
//! ```

use std::fmt;

use crate::hash::Hash;

/// The first line of every entry.
pub const ENTRY_HEADER: &str = "hushrun-tlog-entry/v1";

/// Why an entry doesn't parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryError(pub String);

impl fmt::Display for EntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a log entry: {}", self.0)
    }
}

impl std::error::Error for EntryError {}

/// What an entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryKind {
    /// An object created under `v1/` (L-2).
    Object,
    /// A configuration object written: `trust.yaml`, `trust.dsse.json`, `root/<n>.json`.
    Config,
    /// An object deleted (GC, L-9).
    Tombstone,
    /// \[M19\] The root of the prefix map of every leaf before this one (L-12).
    Map,
}

impl EntryKind {
    pub const ALL: [EntryKind; 4] = [
        EntryKind::Object,
        EntryKind::Config,
        EntryKind::Tombstone,
        EntryKind::Map,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Object => "object",
            EntryKind::Config => "config",
            EntryKind::Tombstone => "tombstone",
            EntryKind::Map => "map",
        }
    }

    pub fn parse(text: &str) -> Option<EntryKind> {
        EntryKind::ALL.into_iter().find(|k| k.as_str() == text)
    }
}

impl fmt::Display for EntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    /// The object's key, or the configuration object's name; empty for a map entry.
    pub name: String,
    /// SHA-256 of the object's bytes: the bytes stored, or for a tombstone, the bytes deleted.
    /// \[M19\] For a map entry, the map's root.
    pub sha256: Hash,
    /// Tombstones only: SHA-256 of the GC statement an administrator signed for this deletion
    /// (L-9), or `None` for a deletion nobody signed. Always `None` for other kinds.
    pub gc: Option<Hash>,
}

impl Entry {
    /// An object created with `bytes`.
    pub fn object(key: &str, bytes: &[u8]) -> Entry {
        Entry::object_with_hash(key, Hash::of(bytes))
    }

    pub fn object_with_hash(key: &str, sha256: Hash) -> Entry {
        Entry {
            kind: EntryKind::Object,
            name: key.to_string(),
            sha256,
            gc: None,
        }
    }

    /// A configuration object written with `bytes`.
    pub fn config(name: &str, bytes: &[u8]) -> Entry {
        Entry {
            kind: EntryKind::Config,
            name: name.to_string(),
            sha256: Hash::of(bytes),
            gc: None,
        }
    }

    /// The deletion of the object `key` whose bytes hashed to `sha256`, signed for by the GC
    /// statement with hash `gc`, if any.
    pub fn tombstone(key: &str, sha256: Hash, gc: Option<Hash>) -> Entry {
        Entry {
            kind: EntryKind::Tombstone,
            name: key.to_string(),
            sha256,
            gc,
        }
    }

    /// \[M19\] The map entry that commits to the prefix map whose root is `root` (L-12).
    pub fn map(root: Hash) -> Entry {
        Entry {
            kind: EntryKind::Map,
            name: String::new(),
            sha256: root,
            gc: None,
        }
    }

    /// Whether the entry can be encoded: a valid name (none for a map entry), and a GC hash
    /// only on a tombstone.
    pub fn check(&self) -> Result<(), EntryError> {
        if self.kind == EntryKind::Map {
            if !self.name.is_empty() {
                return Err(EntryError("a map entry has no name".into()));
            }
        } else if !crate::valid_name(&self.name) {
            return Err(EntryError(format!(
                "{:?} is not a key or configuration name",
                self.name
            )));
        }
        if self.kind != EntryKind::Tombstone && self.gc.is_some() {
            return Err(EntryError(format!(
                "only tombstones carry a GC statement, not {} entries",
                self.kind
            )));
        }
        Ok(())
    }

    /// The entry's bytes, as its leaf hashes them.
    pub fn encode(&self) -> Vec<u8> {
        if self.kind == EntryKind::Map {
            return format!("{ENTRY_HEADER}\nmap\n{}\n", self.sha256.hex()).into_bytes();
        }
        let mut text = format!(
            "{ENTRY_HEADER}\n{}\n{}\n{}\n",
            self.kind,
            self.name,
            self.sha256.hex()
        );
        if self.kind == EntryKind::Tombstone {
            match &self.gc {
                Some(gc) => text.push_str(&format!("gc {}\n", gc.hex())),
                None => text.push_str("gc none\n"),
            }
        }
        text.into_bytes()
    }

    /// The leaf hash: `SHA-256(0x00 || entry)`.
    pub fn leaf_hash(&self) -> Hash {
        Hash::leaf(&self.encode())
    }

    /// Parses an entry's bytes.
    pub fn decode(bytes: &[u8]) -> Result<Entry, EntryError> {
        let err = |why: &str| EntryError(why.to_string());
        let text = std::str::from_utf8(bytes).map_err(|_| err("it isn't UTF-8"))?;
        let body = text
            .strip_suffix('\n')
            .ok_or_else(|| err("it doesn't end in a newline"))?;
        let lines: Vec<&str> = body.split('\n').collect();
        if lines.first() != Some(&ENTRY_HEADER) {
            return Err(err("the first line isn't hushrun-tlog-entry/v1"));
        }
        let kind = lines
            .get(1)
            .and_then(|k| EntryKind::parse(k))
            .ok_or_else(|| err("the kind is not object, config, tombstone or map"))?;
        if kind == EntryKind::Map {
            let [_, _, root] = lines.as_slice() else {
                return Err(err("a map entry is three lines"));
            };
            let root =
                Hash::from_hex(root).ok_or_else(|| err("the map root is not 64 lowercase hex"))?;
            return Ok(Entry::map(root));
        }
        let name = lines.get(2).ok_or_else(|| err("no name"))?;
        let sha256 = lines
            .get(3)
            .and_then(|h| Hash::from_hex(h))
            .ok_or_else(|| err("the hash is not 64 lowercase hex digits"))?;
        let gc = match (kind, lines.get(4)) {
            (EntryKind::Tombstone, Some(&"gc none")) => None,
            (EntryKind::Tombstone, Some(line)) => Some(
                line.strip_prefix("gc ")
                    .and_then(Hash::from_hex)
                    .ok_or_else(|| err("the gc line is neither `gc none` nor `gc <hex>`"))?,
            ),
            (EntryKind::Tombstone, None) => return Err(err("a tombstone without a gc line")),
            (_, None) => None,
            (_, Some(_)) => return Err(err("extra lines")),
        };
        let expected_lines = if kind == EntryKind::Tombstone { 5 } else { 4 };
        if lines.len() != expected_lines {
            return Err(err("extra lines"));
        }
        let entry = Entry {
            kind,
            name: name.to_string(),
            sha256,
            gc,
        };
        entry.check()?;
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: L-11
    #[test]
    fn entries_have_one_encoding() {
        let object = Entry::object("v1/events/api/x/01J.started.production.json", b"bytes");
        let expected = format!(
            "hushrun-tlog-entry/v1\nobject\nv1/events/api/x/01J.started.production.json\n{}\n",
            Hash::of(b"bytes").hex()
        );
        assert_eq!(String::from_utf8(object.encode()).unwrap(), expected);
        assert_eq!(object.leaf_hash(), Hash::leaf(expected.as_bytes()));
        let config = Entry::config("trust.dsse.json", b"{}");
        let unsigned = Entry::tombstone("v1/events/a/b", Hash::of(b"x"), None);
        let signed = Entry::tombstone("v1/events/a/b", Hash::of(b"x"), Some(Hash::of(b"gc")));
        assert!(
            String::from_utf8(unsigned.encode())
                .unwrap()
                .ends_with("\ngc none\n")
        );
        for entry in [&object, &config, &unsigned, &signed] {
            assert_eq!(Entry::decode(&entry.encode()).as_ref(), Ok(entry));
        }
        let map = Entry::map(Hash::of(b"a map"));
        assert_eq!(
            String::from_utf8(map.encode()).unwrap(),
            format!("hushrun-tlog-entry/v1\nmap\n{}\n", Hash::of(b"a map").hex())
        );
        assert_eq!(Entry::decode(&map.encode()).as_ref(), Ok(&map));
        let leaves = [&object, &config, &unsigned, &signed, &map].map(|e| e.leaf_hash());
        for (i, a) in leaves.iter().enumerate() {
            for b in &leaves[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn entries_decode_strictly() {
        let hex = Hash::of(b"x").hex();
        for bad in [
            format!("hushrun-tlog-entry/v2\nobject\nk\n{hex}\n"),
            format!("hushrun-tlog-entry/v1\nthing\nk\n{hex}\n"),
            format!("hushrun-tlog-entry/v1\nobject\nk\n{}\n", hex.to_uppercase()),
            format!("hushrun-tlog-entry/v1\nobject\nk\n{hex}"),
            format!("hushrun-tlog-entry/v1\nobject\nk\n{hex}\ngc none\n"),
            format!("hushrun-tlog-entry/v1\ntombstone\nk\n{hex}\n"),
            format!("hushrun-tlog-entry/v1\ntombstone\nk\n{hex}\ngc maybe\n"),
            format!("hushrun-tlog-entry/v1\ntombstone\nk\n{hex}\ngc none\nmore\n"),
            format!("hushrun-tlog-entry/v1\nobject\na b\n{hex}\n"),
            format!("hushrun-tlog-entry/v1\nobject\n../x\n{hex}\n"),
            format!("hushrun-tlog-entry/v1\nmap\nname\n{hex}\n"),
            format!("hushrun-tlog-entry/v1\nmap\n{}\n", hex.to_uppercase()),
            "hushrun-tlog-entry/v1\nmap\n".to_string(),
        ] {
            assert!(Entry::decode(bad.as_bytes()).is_err(), "{bad:?}");
        }
        let mut odd = Entry::config("trust.yaml", b"");
        odd.gc = Some(Hash::empty());
        assert!(odd.check().is_err());
        let mut named = Entry::map(Hash::empty());
        named.name = "map".into();
        assert!(named.check().is_err());
    }
}
