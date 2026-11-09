//! Parsing `hushrun.yaml` and `trust.yaml`: size and structure budgets, duplicate keys as
//! errors, and a `file:line:column` position on every error (C-8).

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{DeserializeOwned, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// Configuration files larger than this are rejected before parsing (C-8).
pub const MAX_CONFIG_BYTES: usize = 1 << 20;

/// A position in a configuration file. Lines and columns count from 1; 0 means unknown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub line: u64,
    pub column: u64,
}

impl Pos {
    pub const UNKNOWN: Pos = Pos { line: 0, column: 0 };

    fn from_location(location: &serde_saphyr::Location) -> Self {
        Pos {
            line: location.line(),
            column: location.column(),
        }
    }
}

/// An error in a configuration file, at a position.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{file}:{}:{}: {message}", pos.line.max(1), pos.column.max(1))]
pub struct ConfigError {
    pub file: String,
    pub pos: Pos,
    pub message: String,
}

impl ConfigError {
    pub fn new(file: impl Into<String>, pos: Pos, message: impl Into<String>) -> Self {
        ConfigError {
            file: file.into(),
            pos,
            message: message.into(),
        }
    }
}

/// Every error found in a configuration file, in file order.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ConfigErrors(pub Vec<ConfigError>);

impl fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, error) in self.0.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{error}")?;
        }
        Ok(())
    }
}

impl From<ConfigError> for ConfigErrors {
    fn from(error: ConfigError) -> Self {
        ConfigErrors(vec![error])
    }
}

/// Collects validation errors for one file.
#[derive(Debug)]
pub struct Errors {
    file: String,
    errors: Vec<ConfigError>,
}

impl Errors {
    pub fn new(file: impl Into<String>) -> Self {
        Errors {
            file: file.into(),
            errors: Vec::new(),
        }
    }

    pub fn push(&mut self, pos: Pos, message: impl Into<String>) {
        self.errors
            .push(ConfigError::new(self.file.clone(), pos, message));
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// How many errors were pushed so far.
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// `Ok(value)` if no error was pushed, else every error sorted by position.
    pub fn finish<T>(mut self, value: T) -> Result<T, ConfigErrors> {
        if self.errors.is_empty() {
            Ok(value)
        } else {
            self.errors.sort_by_key(|e| e.pos);
            self.errors.dedup();
            Err(ConfigErrors(self.errors))
        }
    }
}

/// Parses one YAML document into `T`, with hushrun's limits.
pub fn parse<T: DeserializeOwned>(file: &str, bytes: &[u8]) -> Result<T, ConfigError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::new(
            file,
            Pos::UNKNOWN,
            format!(
                "the file is {} bytes; the limit is {MAX_CONFIG_BYTES} (1 MiB)",
                bytes.len()
            ),
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|err| {
        ConfigError::new(file, Pos::UNKNOWN, format!("the file is not UTF-8: {err}"))
    })?;
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_documents: 1,
            max_depth: 32,
            max_nodes: 100_000,
            max_events: 400_000,
            max_aliases: 1_000,
            max_anchors: 1_000,
            max_total_scalar_bytes: MAX_CONFIG_BYTES,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        strict_booleans: true,
    };
    serde_saphyr::from_str_with_options(text, options).map_err(|err| {
        let pos = err
            .location()
            .map(|l| Pos::from_location(&l))
            .unwrap_or_default();
        ConfigError::new(file, pos, message_of(&err))
    })
}

/// The error message without serde-saphyr's own location prefix or source snippet: hushrun
/// prints the position itself, as `file:line:column`.
fn message_of(err: &serde_saphyr::Error) -> String {
    let text = err.without_snippet().to_string();
    let text = text.lines().next().unwrap_or_default().trim().to_string();
    strip_location_suffix(&text)
}

fn strip_location_suffix(text: &str) -> String {
    // serde-saphyr appends " at line L, column C" to some messages.
    match text.rfind(" at line ") {
        Some(at) if text[at..].contains("column") => text[..at].to_string(),
        _ => text.to_string(),
    }
}

/// A value together with the position of its YAML node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spanned<T> {
    pub value: T,
    pub pos: Pos,
}

impl<T> Spanned<T> {
    pub fn new(value: T, pos: Pos) -> Self {
        Spanned { value, pos }
    }
}

impl<T> std::ops::Deref for Spanned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Spanned<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spanned = serde_saphyr::Spanned::<T>::deserialize(deserializer)?;
        Ok(Spanned {
            pos: Pos::from_location(&spanned.referenced),
            value: spanned.value,
        })
    }
}

/// A YAML mapping that remembers the position of each key. Keys are unique: the parser
/// rejects duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map<K: Ord, V> {
    entries: BTreeMap<K, (Pos, V)>,
}

impl<K: Ord, V> Default for Map<K, V> {
    fn default() -> Self {
        Map {
            entries: BTreeMap::new(),
        }
    }
}

impl<K: Ord, V> Map<K, V> {
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.entries.get(key).map(|(_, v)| v)
    }

    /// Where `key` was written.
    pub fn pos_of<Q>(&self, key: &Q) -> Pos
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.entries.get(key).map(|(p, _)| *p).unwrap_or_default()
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.entries.contains_key(key)
    }

    /// Entries in key order, with the position of each key.
    pub fn iter(&self) -> impl Iterator<Item = (&K, Pos, &V)> {
        self.entries.iter().map(|(k, (p, v))| (k, *p, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.values().map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert(&mut self, key: K, pos: Pos, value: V) {
        self.entries.insert(key, (pos, value));
    }

    /// The plain map, without positions.
    pub fn into_map(self) -> BTreeMap<K, V> {
        self.entries.into_iter().map(|(k, (_, v))| (k, v)).collect()
    }
}

impl<'de, K, V> Deserialize<'de> for Map<K, V>
where
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor<K, V>(PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for MapVisitor<K, V>
        where
            K: Deserialize<'de> + Ord,
            V: Deserialize<'de>,
        {
            type Value = Map<K, V>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a mapping")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut map = Map::default();
                while let Some(key) = access.next_key::<Spanned<K>>()? {
                    let value = access.next_value::<V>()?;
                    map.insert(key.value, key.pos, value);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(MapVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Doc {
        items: Map<String, Spanned<u32>>,
        #[serde(default)]
        flag: bool,
    }

    // covers: C-8
    #[test]
    fn reports_positions_of_keys_and_values() {
        let doc: Doc = parse("t.yaml", b"items:\n  a: 1\n  bb: 22\n").unwrap();
        assert_eq!(doc.items.pos_of("bb"), Pos { line: 3, column: 3 });
        let value = doc.items.get("bb").unwrap();
        assert_eq!(value.value, 22);
        assert_eq!(value.pos, Pos { line: 3, column: 7 });
    }

    // covers: C-1, C-8
    #[test]
    fn unknown_keys_are_errors_with_positions() {
        let err = parse::<Doc>("hushrun.yaml", b"items: {}\nflga: true\n").unwrap_err();
        assert_eq!(err.pos.line, 2, "{err}");
        assert!(err.to_string().starts_with("hushrun.yaml:2:"), "{err}");
        assert!(err.message.contains("flga"), "{err}");
    }

    // covers: C-8
    #[test]
    fn duplicate_keys_are_errors() {
        let err = parse::<Doc>("t.yaml", b"items:\n  a: 1\n  a: 2\n").unwrap_err();
        assert_eq!(err.pos.line, 3, "{err}");
    }

    // covers: C-8
    #[test]
    fn rejects_oversized_files() {
        let big = vec![b' '; MAX_CONFIG_BYTES + 1];
        let err = parse::<Doc>("t.yaml", &big).unwrap_err();
        assert!(err.message.contains("1 MiB"), "{err}");
    }

    #[test]
    fn strict_booleans() {
        assert!(parse::<Doc>("t.yaml", b"items: {}\nflag: yes\n").is_err());
        assert!(
            parse::<Doc>("t.yaml", b"items: {}\nflag: true\n")
                .unwrap()
                .flag
        );
    }
}
