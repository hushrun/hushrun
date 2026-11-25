//! The package store's format (K-1 to K-5): `packages.json`, its rows, its three digests, and the
//! one function that says whether a folder is a package store.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use hushrun_core::blocks::Hex32;
use serde::{Deserialize, Serialize};
use sha2_hw::{Digest as _, Sha256};

/// `packages.json`'s name.
pub const PACKAGES_JSON: &str = "packages.json";
/// K-6: the store's signature, a Sigstore bundle beside `packages.json`.
pub const SIGNATURE: &str = "packages.sigstore.json";
/// The rows' folder: `rows/<part>.jsonl`.
pub const ROWS: &str = "rows";
/// The parts' folder: `parts/<part>/…`.
pub const PARTS: &str = "parts";
/// `packages.json`'s schema. Any other is refused.
pub const SCHEMA: &str = "hushrun.baked-packages/v1";
/// What a part's identity is the digest of, before its fields.
const PART_SCHEMA: &str = "hushrun.packages-part/v1";
/// What a store's identity is the digest of, before its parts.
const STORE_SCHEMA: &str = "hushrun.baked-packages-id/v1";
/// What a part's inputs are the digest of, before its fields.
const INPUTS_SCHEMA: &str = "hushrun.packages-inputs/v1";
/// The most bytes `packages.json` may have.
const MOST_PACKAGES_JSON: u64 = 16 << 20;
/// The most bytes one rows file may have.
const MOST_ROWS: u64 = 512 << 20;

/// `packages.json` (K-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Packages {
    /// [`SCHEMA`].
    pub schema: String,
    /// The repository, as a run names it, when known.
    pub repository: Option<String>,
    /// The platform of the machine that baked it, in a tag's words: `linux-x64`, `macos-arm64`.
    pub platform: String,
    /// [`store_identity`] of its parts.
    pub identity: Hex32,
    /// When it was baked, in milliseconds since the epoch (`SOURCE_DATE_EPOCH` when set).
    pub baked_ms: u64,
    /// One entry per part, each part once, sorted by name.
    pub parts: Vec<PartEntry>,
}

/// One part of `packages.json`: one package manager's files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartEntry {
    /// The package manager's name (`gradle`, `maven`, …); a built part's ends in `-built`.
    pub part: String,
    /// What its files are (K-5).
    pub class: Class,
    /// The tool's version line, as the bake read it, or `harvested`.
    pub tool: String,
    /// `None` for a download part; what a built part's entries are bound to (K-9).
    pub runtime: Option<String>,
    /// [`inputs_digest`]: shown, and what lets a bake skip. Never evidence.
    pub inputs: Hex32,
    /// The manifest set it was baked from, as paths from the checkout's root (K-4).
    pub manifests: Vec<String>,
    /// [`part_identity`] of its rows.
    pub identity: Hex32,
    /// Its rows file, `rows/<part>.jsonl`.
    pub rows: RowsFile,
    /// The bytes of its files.
    pub bytes: u64,
    /// What checks its bytes when a tool reads them.
    pub verified_by: VerifiedBy,
}

/// A rows file, by its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowsFile {
    /// The SHA-256 of the file's bytes.
    pub sha256: Hex32,
    /// Its size.
    pub bytes: u64,
    /// How many rows it holds.
    pub count: u64,
}

/// K-5: what a part's files are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// Files the package manager fetched, unchanged.
    Download,
    /// Files that exist because code ran during an install (K-9).
    Built,
}

impl Class {
    /// The word `packages.json` writes.
    pub fn word(self) -> &'static str {
        match self {
            Class::Download => "download",
            Class::Built => "built",
        }
    }
}

/// K-2: what checks a part's bytes when a tool reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedBy {
    /// Every row carries the lockfile's own hash of its package.
    Lockfile,
    /// A signature by a signer the repository names.
    Signature,
}

/// K-3: one row, one file of a part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    /// Relative to the part's folder: printable, `/`-separated, no `.` or `..`.
    pub path: String,
    /// What it is.
    pub kind: Kind,
    /// Its size; a link's, the length of its target.
    pub bytes: u64,
    /// The SHA-256 of its bytes; a link's, of its target's text.
    pub sha256: Hex32,
    /// A link's target, relative, inside the part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// A download's package, as the tool names it (`group:name:version`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// The lockfile's own hash of that package, as the lockfile writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<String>,
}

/// K-3: a row's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A regular file no one may run.
    File,
    /// A regular file its owner may run.
    Exe,
    /// A symbolic link inside the part.
    Link,
}

impl Kind {
    /// The word a row writes.
    pub fn word(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Exe => "exe",
            Kind::Link => "link",
        }
    }
}

/// Why a folder or a file isn't a package store a run reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub String);

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Packages {
    /// K-2: reads `packages.json`'s bytes. The schema is read first, so that a newer version is
    /// named as such; then every field, an unknown one refused.
    pub fn parse(bytes: &[u8]) -> Result<Packages, Invalid> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|err| Invalid(format!("{PACKAGES_JSON} can't be read: {err}")))?;
        match value.get("schema").and_then(|s| s.as_str()) {
            Some(SCHEMA) => {}
            Some(other) => {
                return Err(Invalid(format!(
                    "{PACKAGES_JSON} is {other}, a version this hushrun doesn't read"
                )));
            }
            None => return Err(Invalid(format!("{PACKAGES_JSON} names no schema"))),
        }
        let packages: Packages = serde_json::from_value(value)
            .map_err(|err| Invalid(format!("{PACKAGES_JSON} can't be read: {err}")))?;
        let mut seen = BTreeSet::new();
        for part in &packages.parts {
            if !seen.insert(part.part.as_str()) {
                return Err(Invalid(format!(
                    "{PACKAGES_JSON} names the part {} twice",
                    part.part
                )));
            }
            if !is_part_name(&part.part) {
                return Err(Invalid(format!(
                    "{PACKAGES_JSON} names a part {:?}, which is no part's name",
                    part.part
                )));
            }
            let built = part.part.ends_with("-built");
            match (part.class, built, &part.runtime, part.verified_by) {
                (Class::Download, false, None, _) => {}
                (Class::Built, true, Some(_), VerifiedBy::Signature) => {}
                _ => {
                    return Err(Invalid(format!(
                        "{PACKAGES_JSON}'s part {} claims what no bake writes (class {}, runtime {:?}, verified by {:?})",
                        part.part,
                        part.class.word(),
                        part.runtime,
                        part.verified_by
                    )));
                }
            }
            if let Some(bad) = part.manifests.iter().find(|m| !is_row_path(m)) {
                return Err(Invalid(format!(
                    "{PACKAGES_JSON}'s part {} names a manifest {bad:?}, which is no path in a checkout",
                    part.part
                )));
            }
        }
        let parts: Vec<(&str, Hex32)> = packages
            .parts
            .iter()
            .map(|p| (p.part.as_str(), p.identity))
            .collect();
        if store_identity(&parts) != packages.identity {
            return Err(Invalid(format!(
                "{PACKAGES_JSON}'s identity isn't its parts'"
            )));
        }
        Ok(packages)
    }

    /// Reads `packages.json` in `folder`, with the SHA-256 of its bytes, what a signature names.
    pub fn read_bytes(folder: &Path) -> Result<(Packages, Hex32), Invalid> {
        let path = folder.join(PACKAGES_JSON);
        let bytes = read_most(&path, MOST_PACKAGES_JSON)
            .map_err(|err| Invalid(format!("{}: {err}", path.display())))?;
        let packages = Packages::parse(&bytes)?;
        Ok((packages, Hex32::sha256(&bytes)))
    }

    /// The part named `name`.
    pub fn part(&self, name: &str) -> Option<&PartEntry> {
        self.parts.iter().find(|p| p.part == name)
    }

    /// The bytes `packages.json` is written as: two-space JSON and a newline, the same on any
    /// machine for the same store.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = serde_json::to_vec_pretty(self).expect("packages.json serializes");
        out.push(b'\n');
        out
    }
}

/// Whether `name` is a part's name: lowercase ASCII letters and digits, in words joined by `-`.
pub fn is_part_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.split('-').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// K-3: whether `path` is a row's path: relative, `/`-separated, printable ASCII or any other
/// character than a control one, no empty, `.` or `..` component, no backslash.
pub fn is_row_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.chars().any(|c| c.is_control() || c == '\\')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// Whether a link at `path` (a row's path) whose target is `target` stays inside its part:
/// relative, and never above the part's folder at any step.
pub fn link_stays_inside(path: &str, target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') || target.chars().any(|c| c.is_control()) {
        return false;
    }
    let mut depth: usize = path.split('/').count() - 1;
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            _ => depth += 1,
        }
    }
    true
}

/// Writes `fields` into `hasher`, each followed by one NUL byte: no field holds one.
fn fields(hasher: &mut Sha256, fields: &[&str]) {
    for field in fields {
        hasher.update(field.as_bytes());
        hasher.update([0u8]);
    }
}

/// K-2: a part's identity: the SHA-256 of `hushrun.packages-part/v1`, its name, class and runtime
/// (empty when none), then each row's path, kind and SHA-256, sorted by path as bytes, every
/// field followed by a NUL byte.
pub fn part_identity(name: &str, class: Class, runtime: Option<&str>, rows: &[Row]) -> Hex32 {
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    let mut hasher = Sha256::new();
    fields(
        &mut hasher,
        &[PART_SCHEMA, name, class.word(), runtime.unwrap_or("")],
    );
    for row in sorted {
        fields(
            &mut hasher,
            &[&row.path, row.kind.word(), &row.sha256.to_string()],
        );
    }
    Hex32(hasher.finalize().into())
}

/// K-2: a store's identity: the SHA-256 of `hushrun.baked-packages-id/v1`, then each part's name
/// and identity, sorted by name, every field followed by a NUL byte.
pub fn store_identity(parts: &[(&str, Hex32)]) -> Hex32 {
    let mut sorted: Vec<&(&str, Hex32)> = parts.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut hasher = Sha256::new();
    fields(&mut hasher, &[STORE_SCHEMA]);
    for (name, identity) in sorted {
        fields(&mut hasher, &[name, &identity.to_string()]);
    }
    Hex32(hasher.finalize().into())
}

/// K-2: a part's inputs: the SHA-256 of `hushrun.packages-inputs/v1`, its name, the tool's version
/// line and the platform, then each file of its manifest set as its path from the checkout's root
/// and the SHA-256 of its bytes, sorted by path as bytes, every field followed by a NUL byte.
pub fn inputs_digest(
    name: &str,
    tool: &str,
    platform: &str,
    manifests: &[(String, Hex32)],
) -> Hex32 {
    let mut sorted: Vec<&(String, Hex32)> = manifests.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut hasher = Sha256::new();
    fields(&mut hasher, &[INPUTS_SCHEMA, name, tool, platform]);
    for (path, sha256) in sorted {
        fields(&mut hasher, &[path, &sha256.to_string()]);
    }
    Hex32(hasher.finalize().into())
}

/// The bytes a rows file is written as: one row a line, sorted by path as bytes.
pub fn rows_bytes(rows: &[Row]) -> Vec<u8> {
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    let mut out = Vec::new();
    for row in sorted {
        out.extend(serde_json::to_vec(row).expect("a row serializes"));
        out.push(b'\n');
    }
    out
}

/// K-3: reads one part's rows file against what `packages.json` says of it: the bytes its
/// SHA-256 names, as many rows as it counts, each a row, sorted by path as bytes, each path once,
/// and the part's identity and size theirs.
pub fn read_rows(folder: &Path, entry: &PartEntry) -> Result<Vec<Row>, Invalid> {
    let path = folder.join(ROWS).join(format!("{}.jsonl", entry.part));
    let bytes = read_most(&path, MOST_ROWS)
        .map_err(|err| Invalid(format!("{ROWS}/{}.jsonl: {err}", entry.part)))?;
    if bytes.len() as u64 != entry.rows.bytes || Hex32::sha256(&bytes) != entry.rows.sha256 {
        return Err(Invalid(format!(
            "{ROWS}/{}.jsonl isn't the file {PACKAGES_JSON} names",
            entry.part
        )));
    }
    let mut rows: Vec<Row> = Vec::new();
    let body = match bytes.strip_suffix(b"\n") {
        Some(body) => body,
        None if bytes.is_empty() => &bytes[..],
        None => {
            return Err(Invalid(format!(
                "{ROWS}/{}.jsonl doesn't end its last row with a newline",
                entry.part
            )));
        }
    };
    let lines: Vec<&[u8]> = if body.is_empty() {
        Vec::new()
    } else {
        body.split(|b| *b == b'\n').collect()
    };
    for (n, line) in lines.into_iter().enumerate() {
        let row: Row = serde_json::from_slice(line)
            .map_err(|err| Invalid(format!("{ROWS}/{}.jsonl line {}: {err}", entry.part, n + 1)))?;
        check_row(&row)
            .map_err(|why| Invalid(format!("{ROWS}/{}.jsonl line {}: {why}", entry.part, n + 1)))?;
        if let Some(last) = rows.last()
            && last.path.as_bytes() >= row.path.as_bytes()
        {
            return Err(Invalid(format!(
                "{ROWS}/{}.jsonl isn't sorted by path, each path once (line {})",
                entry.part,
                n + 1
            )));
        }
        rows.push(row);
    }
    if rows.len() as u64 != entry.rows.count {
        return Err(Invalid(format!(
            "{ROWS}/{}.jsonl holds {} rows, and {PACKAGES_JSON} counts {}",
            entry.part,
            rows.len(),
            entry.rows.count
        )));
    }
    if part_identity(&entry.part, entry.class, entry.runtime.as_deref(), &rows) != entry.identity {
        return Err(Invalid(format!(
            "the part {}'s identity isn't its rows'",
            entry.part
        )));
    }
    let bytes: u64 = rows.iter().map(|r| r.bytes).sum();
    if bytes != entry.bytes {
        return Err(Invalid(format!(
            "the part {}'s rows hold {bytes} bytes, and {PACKAGES_JSON} says {}",
            entry.part, entry.bytes
        )));
    }
    Ok(rows)
}

/// What a row may not be, whatever its file.
fn check_row(row: &Row) -> Result<(), String> {
    if !is_row_path(&row.path) {
        return Err(format!("{:?} is no path a part holds", row.path));
    }
    match (row.kind, &row.target) {
        (Kind::Link, Some(target)) => {
            if !link_stays_inside(&row.path, target) {
                return Err(format!("{} is a link out of its part ({target})", row.path));
            }
            if Hex32::sha256(target.as_bytes()) != row.sha256 || target.len() as u64 != row.bytes {
                return Err(format!("{} isn't the link its row hashes", row.path));
            }
        }
        (Kind::Link, None) => return Err(format!("{} is a link with no target", row.path)),
        (_, Some(_)) => return Err(format!("{} has a target and isn't a link", row.path)),
        (_, None) => {}
    }
    Ok(())
}

/// How far [`check_contents`] looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Names, kinds and sizes: what a run checks when it begins (K-8).
    Names,
    /// And every file's SHA-256: what a bake checks before a folder is put in place, what a run
    /// that may write what others trust checks, and `hushrun packages verify`.
    Bytes,
}

/// K-1: a package store's folder holds `packages.json`, optionally `packages.sigstore.json`,
/// `rows/<part>.jsonl` for each part and `parts/<part>/…` for each part, and nothing else. With
/// `packages`, every part it names has its rows file and its folder and no other is there, and
/// each part's folder holds exactly its rows: no file a row doesn't name, no row whose file is
/// missing, a file's size (and at [`Depth::Bytes`], its SHA-256) the row's, a link only where a
/// row of kind `link` names it with that target, no special file. `Err` names the first thing out
/// of place. Returns each part's rows, in `packages`' order.
pub fn check_contents(
    folder: &Path,
    packages: Option<&Packages>,
    depth: Depth,
) -> Result<Vec<Vec<Row>>, Invalid> {
    let mut rows_files = BTreeSet::new();
    let mut part_dirs = BTreeSet::new();
    for (name, kind) in entries(folder)? {
        match (name.as_str(), kind) {
            (PACKAGES_JSON, Entry::File) | (SIGNATURE, Entry::File) => {}
            (ROWS, Entry::Dir) => {
                for (name, kind) in entries(&folder.join(ROWS))? {
                    let part = name
                        .strip_suffix(".jsonl")
                        .filter(|p| kind == Entry::File && is_part_name(p))
                        .ok_or_else(|| Invalid(format!("{ROWS}/{name} is not a part's rows")))?;
                    rows_files.insert(part.to_string());
                }
            }
            (PARTS, Entry::Dir) => {
                for (name, kind) in entries(&folder.join(PARTS))? {
                    if kind != Entry::Dir || !is_part_name(&name) {
                        return Err(Invalid(format!("{PARTS}/{name} is not a part's folder")));
                    }
                    part_dirs.insert(name);
                }
            }
            (other, Entry::Dir) => {
                return Err(Invalid(format!("{other}/ is not part of a package store")));
            }
            (other, Entry::File) => {
                return Err(Invalid(format!("{other} is not part of a package store")));
            }
            (other, _) => {
                return Err(Invalid(format!(
                    "{other} is a link or a special file, which a package store never holds there"
                )));
            }
        }
    }
    let Some(packages) = packages else {
        return Ok(Vec::new());
    };
    let named: BTreeSet<String> = packages.parts.iter().map(|p| p.part.clone()).collect();
    for (what, there) in [(ROWS, &rows_files), (PARTS, &part_dirs)] {
        if let Some(extra) = there.difference(&named).next() {
            return Err(Invalid(format!(
                "{what}/{extra} is of a part {PACKAGES_JSON} doesn't name"
            )));
        }
        if let Some(missing) = named.difference(there).next() {
            return Err(Invalid(format!(
                "{PACKAGES_JSON} names the part {missing}, and {what}/ has nothing of it"
            )));
        }
    }
    let mut all = Vec::new();
    for entry in &packages.parts {
        let rows = read_rows(folder, entry)?;
        check_part(
            &folder.join(PARTS).join(&entry.part),
            &entry.part,
            &rows,
            depth,
        )?;
        all.push(rows);
    }
    Ok(all)
}

/// K-8: the part `name`'s folder under the store `folder` holds exactly `rows`, every file by its
/// SHA-256. `Err` names the first file that differs.
pub fn verify_part(folder: &Path, name: &str, rows: &[Row]) -> Result<(), Invalid> {
    check_part(&part_dir(folder, name), name, rows, Depth::Bytes)
}

/// K-1: the folder of the part `name` holds exactly `rows`.
fn check_part(dir: &Path, name: &str, rows: &[Row], depth: Depth) -> Result<(), Invalid> {
    let mut found = BTreeMap::new();
    walk(dir, "", &mut found)?;
    let by_path: BTreeMap<&str, &Row> = rows.iter().map(|r| (r.path.as_str(), r)).collect();
    // Every folder above a row is there because of it; a folder no row is under is extra.
    let mut folders = BTreeSet::new();
    for row in rows {
        let mut at = row.path.as_str();
        while let Some((parent, _)) = at.rsplit_once('/') {
            folders.insert(parent);
            at = parent;
        }
    }
    for (path, kind) in &found {
        let place = format!("{PARTS}/{name}/{path}");
        match (kind, by_path.get(path.as_str())) {
            (Found::Dir, None) if folders.contains(path.as_str()) => {}
            (Found::Dir, _) => {
                return Err(Invalid(format!("{place}/ is a folder no row is under")));
            }
            (Found::Special, _) => {
                return Err(Invalid(format!(
                    "{place} is a special file, which a package store never holds"
                )));
            }
            (_, None) => return Err(Invalid(format!("{place} is a file no row names"))),
            (Found::Link(target), Some(row)) => {
                if row.kind != Kind::Link || row.target.as_deref() != Some(target.as_str()) {
                    return Err(Invalid(format!("{place} is a link that isn't its row's")));
                }
            }
            (Found::File { bytes, exe }, Some(row)) => {
                let want_exe = match row.kind {
                    Kind::File => false,
                    Kind::Exe => true,
                    Kind::Link => {
                        return Err(Invalid(format!("{place} is a file, and its row a link")));
                    }
                };
                if cfg!(unix) && *exe != want_exe {
                    return Err(Invalid(format!(
                        "{place} {} run, and its row says it {}",
                        if *exe { "may" } else { "may not" },
                        if want_exe { "may" } else { "may not" }
                    )));
                }
                if *bytes != row.bytes {
                    return Err(Invalid(format!(
                        "{place} has {bytes} bytes, and its row says {}",
                        row.bytes
                    )));
                }
                if depth == Depth::Bytes {
                    let sha256 = sha256_file(&dir.join(path))
                        .map_err(|err| Invalid(format!("{place}: {err}")))?;
                    if sha256 != row.sha256 {
                        return Err(Invalid(format!("{place} isn't the file its row hashes")));
                    }
                }
            }
        }
    }
    if let Some(missing) = rows.iter().find(|r| !found.contains_key(&r.path)) {
        return Err(Invalid(format!(
            "{ROWS}/{name}.jsonl names {}, which isn't there",
            missing.path
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Found {
    Dir,
    File { bytes: u64, exe: bool },
    Link(String),
    Special,
}

/// Every entry under `dir`, by its path from the part's folder; a link is never followed.
fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Found>) -> Result<(), Invalid> {
    let read = fs::read_dir(dir).map_err(|err| Invalid(format!("{}: {err}", dir.display())))?;
    for entry in read {
        let entry = entry.map_err(|err| Invalid(format!("{}: {err}", dir.display())))?;
        let name = entry.file_name().into_string().map_err(|name| {
            Invalid(format!(
                "{} is not a name a package store holds",
                name.to_string_lossy()
            ))
        })?;
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(entry.path())
            .map_err(|err| Invalid(format!("{}: {err}", entry.path().display())))?;
        let kind = meta.file_type();
        if kind.is_dir() {
            out.insert(path.clone(), Found::Dir);
            walk(&entry.path(), &path, out)?;
        } else if kind.is_file() {
            out.insert(
                path,
                Found::File {
                    bytes: meta.len(),
                    exe: is_exe(&meta),
                },
            );
        } else if kind.is_symlink() {
            let target = fs::read_link(entry.path())
                .map_err(|err| Invalid(format!("{}: {err}", entry.path().display())))?;
            out.insert(path, Found::Link(target.to_string_lossy().into_owned()));
        } else {
            out.insert(path, Found::Special);
        }
    }
    Ok(())
}

/// Whether the file's owner may run it.
pub fn is_exe(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o100 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Entry {
    File,
    Dir,
    Other,
}

/// A folder's entries, by name and kind (a link is never followed), sorted.
fn entries(dir: &Path) -> Result<Vec<(String, Entry)>, Invalid> {
    let read = fs::read_dir(dir).map_err(|err| Invalid(format!("{}: {err}", dir.display())))?;
    let mut out = Vec::new();
    for entry in read {
        let entry = entry.map_err(|err| Invalid(format!("{}: {err}", dir.display())))?;
        let name = entry.file_name().into_string().map_err(|name| {
            Invalid(format!(
                "{} is not a name a package store holds",
                name.to_string_lossy()
            ))
        })?;
        let kind = match entry.file_type() {
            Ok(t) if t.is_file() => Entry::File,
            Ok(t) if t.is_dir() => Entry::Dir,
            _ => Entry::Other,
        };
        out.push((name, kind));
    }
    out.sort();
    Ok(out)
}

/// The SHA-256 of the file at `path`, read as a stream.
pub fn sha256_file(path: &Path) -> io::Result<Hex32> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Hex32(hasher.finalize().into()))
}

/// The bytes of the file at `path`, refused past `most`.
fn read_most(path: &Path, most: u64) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(most + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > most {
        return Err(io::Error::other(format!("larger than {most} bytes")));
    }
    Ok(bytes)
}

/// The row of the file, folder entry or link at `path` under a part's folder, named `name` there.
/// `None` for a folder or a special file.
pub fn row_of(path: &Path, name: &str) -> io::Result<Option<Row>> {
    let meta = fs::symlink_metadata(path)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        let target = fs::read_link(path)?.to_string_lossy().into_owned();
        return Ok(Some(Row {
            path: name.to_string(),
            kind: Kind::Link,
            bytes: target.len() as u64,
            sha256: Hex32::sha256(target.as_bytes()),
            target: Some(target),
            package: None,
            locked: None,
        }));
    }
    if !kind.is_file() {
        return Ok(None);
    }
    Ok(Some(Row {
        path: name.to_string(),
        kind: if is_exe(&meta) { Kind::Exe } else { Kind::File },
        bytes: meta.len(),
        sha256: sha256_file(path)?,
        target: None,
        package: None,
        locked: None,
    }))
}

/// A part's folder under the store `folder`.
pub fn part_dir(folder: &Path, part: &str) -> PathBuf {
    folder.join(PARTS).join(part)
}

/// Whether `path` is only normal components (a relative path with no `.` or `..`).
pub fn is_plain_relative(path: &Path) -> bool {
    path.components().all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod tests;
