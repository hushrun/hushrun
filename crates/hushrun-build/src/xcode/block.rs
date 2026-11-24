//! An Xcode block (B-87): one value Xcode put, and every object it reaches, as one block of kind
//! `xcode/1` in hushrun's store. Objects are content-addressed ([`super::cas::id`]), so a block
//! is the unit that travels: a download brings a compile's value and its outputs together, and
//! the lookups Xcode then makes for those objects are answered here, with no further request.
//!
//! **The roots of a value** are the objects it names. Xcode's plugin writes a compile's result
//! under the entry `value`, as a `CASObject` whose references are the compile's outputs (read
//! from a real cache by x8, an open-source service of the same protocol); [`roots`] takes those,
//! and any entry of exactly an id's length that names an object the cache holds. A value whose
//! roots can't be found that way stays on this machine (`unknown-value`): sharing it would hand
//! another machine a value naming objects it can't get.
//!
//! **A block's files**: `value` (the value's one encoding) and `objects/<id hex>`, one per
//! object reached, each in [`super::cas::Object::file_bytes`]'s form. Opening a block checks
//! every object against the id its name gives, and that every id the value reaches is there,
//! before the value is kept: a block with a missing or altered object is refused whole.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::SystemTime;

use hushrun_core::Digest;

use super::cas::{self, Cache, Id, Object};
use super::proto::{self, Value};

/// The file of a block that holds its value.
pub const VALUE_FILE: &str = "value";
/// The folder of a block that holds its objects.
pub const OBJECTS_DIR: &str = "objects";
/// The schema of an Xcode block's description.
pub const BLOCK_SCHEMA: &str = "hushrun.xcode-block/v1";
/// The entry of a value Xcode's plugin writes a compile's result under.
pub const RESULT_ENTRY: &str = "value";
/// The most bytes one block may hold. Packs over 64 MiB are read whole for their upload (B-29).
pub const MOST_BYTES: u64 = 256 << 20;
/// The most objects one block may hold.
pub const MOST_OBJECTS: usize = 20_000;

/// Why a value stays on this machine. Each is a line's last words and a log field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Local {
    /// No entry names objects this code can find.
    UnknownValue,
    /// An object the value reaches isn't in the cache.
    Incomplete,
    /// An object holds a folder of this machine: the home folder, the checkout, DerivedData.
    HoldsPath,
    /// Over [`MOST_BYTES`] or [`MOST_OBJECTS`].
    Large,
    /// The run doesn't share, and reads no baked store: nothing is kept beside the cache.
    NotSharing,
}

impl Local {
    pub fn words(self) -> &'static str {
        match self {
            Local::UnknownValue => "a value whose outputs can't be found",
            Local::Incomplete => "an output that isn't in the cache",
            Local::HoldsPath => "an output that holds a folder of this machine",
            Local::Large => "more than 256 MiB or 20,000 objects",
            Local::NotSharing => "the run doesn't share",
        }
    }

    /// The reason as the log writes it. Stable.
    pub fn as_str(self) -> &'static str {
        match self {
            Local::UnknownValue => "unknown-value",
            Local::Incomplete => "incomplete",
            Local::HoldsPath => "holds-path",
            Local::Large => "large",
            Local::NotSharing => "not-sharing",
        }
    }
}

/// The objects a value names directly.
pub fn roots(value: &Value, cache: &Cache) -> Vec<Id> {
    let mut out = Vec::new();
    for (name, bytes) in value {
        if name == RESULT_ENTRY
            && let Ok(object) = proto::read_object(bytes)
        {
            out.extend(object.references.iter().filter_map(|id| cas::id_of(id)));
            continue;
        }
        if let Some(id) = cas::id_of(bytes)
            && cache.has(&id)
        {
            out.push(id);
        }
    }
    out
}

/// Everything a value reaches, read from the cache, or why it can't travel.
pub fn closure(
    value: &Value,
    cache: &Cache,
    now: SystemTime,
) -> Result<BTreeMap<Id, Object>, Local> {
    let roots = roots(value, cache);
    if roots.is_empty() {
        return Err(Local::UnknownValue);
    }
    let mut found = BTreeMap::new();
    let mut bytes = 0u64;
    let mut next: Vec<Id> = roots;
    while let Some(id) = next.pop() {
        if found.contains_key(&id) {
            continue;
        }
        let Some(object) = cache.object(&id, now) else {
            return Err(Local::Incomplete);
        };
        bytes += object.data.len() as u64 + (object.references.len() * cas::ID_LEN) as u64;
        if bytes > MOST_BYTES || found.len() >= MOST_OBJECTS {
            return Err(Local::Large);
        }
        next.extend(object.references.iter().copied());
        found.insert(id, object);
    }
    Ok(found)
}

/// Whether every object `value` reaches is in the cache, by the files' heads alone: what a hit
/// from the cache checks before it is served (each object is checked against its id when Xcode
/// loads it). A value whose roots can't be found is taken as whole: it is Xcode's own business,
/// served as it was put.
pub fn present(value: &Value, cache: &Cache) -> bool {
    let mut next = roots(value, cache);
    let mut seen = BTreeSet::new();
    while let Some(id) = next.pop() {
        if !seen.insert(id) {
            continue;
        }
        if seen.len() > MOST_OBJECTS {
            return true;
        }
        match cache.references(&id) {
            Some(references) => next.extend(references),
            None => return false,
        }
    }
    true
}

/// Whether an object holds one of `folders` (the home folder, the checkout's in its given and
/// resolved forms, DerivedData's), as raw bytes wherever they lie. Folders shorter than two
/// bytes are never looked for.
pub fn holds_a_folder<'a>(
    objects: impl IntoIterator<Item = &'a Object>,
    folders: &[String],
) -> bool {
    let finders: Vec<memchr::memmem::Finder<'_>> = folders
        .iter()
        .filter(|folder| folder.len() >= 2)
        .map(|folder| memchr::memmem::Finder::new(folder.as_bytes()))
        .collect();
    if finders.is_empty() {
        return false;
    }
    objects.into_iter().any(|object| {
        finders
            .iter()
            .any(|finder| finder.find(&object.data).is_some())
    })
}

/// The files of the block of `value`, written into `dir` for the store to copy: `value` and one
/// file per object. Their names, for [`crate::store::NewFile`].
pub fn write_files(
    dir: &Path,
    value: &Value,
    objects: &BTreeMap<Id, Object>,
) -> std::io::Result<Vec<(String, std::path::PathBuf)>> {
    let mut out = Vec::with_capacity(objects.len() + 1);
    std::fs::create_dir_all(dir.join(OBJECTS_DIR))?;
    let value_path = dir.join(VALUE_FILE);
    std::fs::write(&value_path, proto::encode_value(value))?;
    out.push((VALUE_FILE.to_string(), value_path));
    for (id, object) in objects {
        let name = format!("{OBJECTS_DIR}/{}", hushrun_core::digest::to_hex(id));
        let path = dir.join(&name);
        std::fs::write(&path, object.file_bytes())?;
        out.push((name, path));
    }
    Ok(out)
}

/// Why a block can't be opened. A downloaded one is then reported `rejected` (B-34).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// No `value` file, or one that doesn't parse.
    Value,
    /// A file that isn't `value` or an object named by its id.
    Stray(String),
    /// An object whose bytes aren't the id its name gives.
    Object(String),
    /// An object the value reaches is missing.
    Missing,
    /// The value isn't the one the block's record names.
    Digest,
}

impl Refused {
    pub fn words(&self) -> String {
        match self {
            Refused::Value => "its value can't be read".into(),
            Refused::Stray(name) => format!("a file that is no object ({name})"),
            Refused::Object(name) => format!("an object that isn't its id ({name})"),
            Refused::Missing => "an output it names is missing".into(),
            Refused::Digest => "its value isn't the one its record names".into(),
        }
    }
}

/// Reads a block's files (`(name, bytes)`), checks every one, and keeps its objects in the cache.
/// The value, for the caller to keep under its key once it decides to serve it.
pub fn open(
    files: impl IntoIterator<Item = (String, Vec<u8>)>,
    expected: Option<&Digest>,
    cache: &Cache,
    now: SystemTime,
) -> Result<Value, Refused> {
    let mut value = None;
    let mut objects = BTreeMap::new();
    for (name, bytes) in files {
        if name == VALUE_FILE {
            value = Some(proto::read_value(&bytes).map_err(|_| Refused::Value)?);
            continue;
        }
        let Some(hex) = name.strip_prefix(&format!("{OBJECTS_DIR}/")) else {
            return Err(Refused::Stray(name));
        };
        let id = hex_id(hex).ok_or_else(|| Refused::Stray(name.clone()))?;
        let object = Object::from_file_bytes(&bytes, &id).ok_or(Refused::Object(name))?;
        objects.insert(id, object);
    }
    let value = value.ok_or(Refused::Value)?;
    if let Some(expected) = expected
        && cas::value_digest(&value) != *expected
    {
        return Err(Refused::Digest);
    }
    // Every object the value reaches, from the block or the cache already.
    let mut seen = BTreeSet::new();
    let mut next = Vec::new();
    for (name, bytes) in &value {
        if name == RESULT_ENTRY
            && let Ok(object) = proto::read_object(bytes)
        {
            next.extend(object.references.iter().filter_map(|id| cas::id_of(id)));
        } else if let Some(id) = cas::id_of(bytes)
            && (objects.contains_key(&id) || cache.has(&id))
        {
            next.push(id);
        }
    }
    while let Some(id) = next.pop() {
        if !seen.insert(id) {
            continue;
        }
        match objects.get(&id) {
            Some(object) => next.extend(object.references.iter().copied()),
            None if cache.has(&id) => {}
            None => return Err(Refused::Missing),
        }
    }
    if seen.is_empty() {
        return Err(Refused::Missing);
    }
    for object in objects.values() {
        cache
            .put_object(object, now)
            .map_err(|_| Refused::Missing)?;
    }
    Ok(value)
}

fn hex_id(hex: &str) -> Option<Id> {
    if hex.len() != cas::ID_LEN * 2 {
        return None;
    }
    let mut id = [0u8; cas::ID_LEN];
    for (i, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(chunk).ok()?;
        if text.bytes().any(|b| b.is_ascii_uppercase()) {
            return None;
        }
        id[i] = u8::from_str_radix(text, 16).ok()?;
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xcode::proto::{Bytes, Object as Wire};

    fn result_value(roots: &[Id]) -> Value {
        let result = Wire {
            blob: Bytes::Data(b"result".to_vec()),
            references: roots.iter().map(|id| id.to_vec()).collect(),
        };
        [(RESULT_ENTRY.to_string(), proto::encode_object(&result))].into()
    }

    fn put(cache: &Cache, data: &[u8], references: &[Id]) -> Id {
        cache
            .put_object(
                &Object {
                    data: data.to_vec(),
                    references: references.to_vec(),
                },
                SystemTime::now(),
            )
            .unwrap()
    }

    // covers: B-87
    #[test]
    fn a_block_holds_a_value_and_everything_it_reaches() {
        let here = tempfile::tempdir().unwrap();
        let cache = Cache::in_store(here.path());
        let now = SystemTime::now();
        let object_file = put(&cache, b"\xcf\xfa\xed\xfe object", &[]);
        let diagnostics = put(&cache, b"no warnings", &[]);
        let outputs = put(&cache, b"outputs", &[object_file, diagnostics]);
        let schema = put(&cache, b"llvm.cas.schema", &[]);
        let value = result_value(&[outputs, schema]);
        let objects = closure(&value, &cache, now).unwrap();
        assert_eq!(objects.len(), 4);

        let staged = here.path().join("staged");
        let files = write_files(&staged, &value, &objects).unwrap();
        assert_eq!(files.len(), 5);

        // Another machine, an empty cache: the block opens, and every object is there after.
        let there = tempfile::tempdir().unwrap();
        let other = Cache::in_store(there.path());
        let read: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(name, path)| (name.clone(), std::fs::read(path).unwrap()))
            .collect();
        let digest = cas::value_digest(&value);
        let opened = open(read.clone(), Some(&digest), &other, now).unwrap();
        assert_eq!(opened, value);
        for id in [object_file, diagnostics, outputs, schema] {
            assert!(other.has(&id));
        }

        // An object left out: refused whole.
        let missing: Vec<_> = read
            .iter()
            .filter(|(name, _)| !name.ends_with(&hushrun_core::digest::to_hex(&diagnostics)))
            .cloned()
            .collect();
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            open(missing, None, &Cache::in_store(empty.path()), now),
            Err(Refused::Missing)
        );
        // An object altered: refused.
        let mut altered = read.clone();
        let at = altered
            .iter()
            .position(|(name, _)| name.starts_with(OBJECTS_DIR))
            .unwrap();
        altered[at].1.push(b'!');
        assert!(matches!(
            open(altered, None, &Cache::in_store(empty.path()), now),
            Err(Refused::Object(_))
        ));
        // Another value than the record names.
        assert_eq!(
            open(read.clone(), Some(&Digest::of(b"other")), &other, now),
            Err(Refused::Digest)
        );
        // A stray file.
        let mut stray = read;
        stray.push(("notes.txt".into(), b"hi".to_vec()));
        assert!(matches!(
            open(stray, None, &other, now),
            Err(Refused::Stray(_))
        ));
    }

    // covers: B-87
    #[test]
    fn a_value_that_names_nothing_found_or_reaches_a_missing_object_stays_here() {
        let here = tempfile::tempdir().unwrap();
        let cache = Cache::in_store(here.path());
        let now = SystemTime::now();
        let unknown: Value = [("other".to_string(), b"not an object".to_vec())].into();
        assert_eq!(closure(&unknown, &cache, now), Err(Local::UnknownValue));
        let missing = result_value(&[[9u8; 32]]);
        assert_eq!(closure(&missing, &cache, now), Err(Local::Incomplete));
        // An entry of an id's length naming an object here is a root too.
        let id = put(&cache, b"x", &[]);
        let bare: Value = [("output".to_string(), id.to_vec())].into();
        assert_eq!(closure(&bare, &cache, now).unwrap().len(), 1);
    }

    // covers: B-87
    #[test]
    fn a_folder_of_this_machine_in_an_output_is_found() {
        let objects = [Object {
            data: b"debug info: /Users/ana/src/app/main.swift".to_vec(),
            references: vec![],
        }];
        assert!(holds_a_folder(&objects, &["/Users/ana".into()]));
        assert!(!holds_a_folder(&objects, &["/Users/bo".into()]));
        assert!(!holds_a_folder(&objects, &["/".into(), String::new()]));
    }
}
