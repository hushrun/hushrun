//! The two keys of a compiler call (B-102, B-103) and the hint that links them.
//!
//! The first key is what was asked: the compiler's identity by content, the arguments, the
//! folder from the checkout's root, the platform, whether names keep case, whether stdout is a
//! terminal, and the variables the compiler reads. The second is what was read: each file by its
//! path from the root and the SHA-256 of the bytes the compiler got; each path looked at, by its
//! kind or absence; each folder listed, by its entries; each path resolved, by where it led; and
//! each name looked for above the checkout and not found. A lookup checks the hint against the
//! files as they are now and never trusts it (B-7's rule, read for TypeScript).

use std::path::{Path, PathBuf};

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

use super::call::{Call, Record};

/// The schema of the first key.
const STEP_SCHEMA: &str = "hushrun.tsc-key/v1";
/// The schema of the second key.
const BLOCK_SCHEMA: &str = "hushrun.tsc-reads/v1";
/// The schema of a tied block's key.
const TIED_SCHEMA: &str = "hushrun.tsc-tied/v1";
/// The schema of a hint.
pub const HINT_SCHEMA: &str = "hushrun.tsc-hint/v1";
/// The placeholder for the checkout's root.
pub const ROOT: &str = "{root}";

/// The variables the compiler reads (B-102): its colours and its assertion level.
pub const KEYED_ENV: &[&str] = &["NO_COLOR", "FORCE_COLOR", "NODE_ENV"];

/// The checkout's root in its forms: a path under one is recorded from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Places {
    /// Longest first.
    roots: Vec<String>,
    /// The form paths are shown in: the resolved one, which Node's `process.cwd()` gives.
    shown: String,
}

impl Places {
    /// `roots`: the given form first, then the resolved one.
    pub fn new(roots: &[String]) -> Places {
        let shown = roots.last().cloned().unwrap_or_default();
        let mut sorted: Vec<String> = roots
            .iter()
            .map(|r| r.trim_end_matches('/').to_string())
            .filter(|r| !r.is_empty())
            .collect();
        sorted.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        sorted.dedup();
        Places {
            roots: sorted,
            shown: shown.trim_end_matches('/').to_string(),
        }
    }

    /// The roots, longest first.
    pub fn roots(&self) -> &[String] {
        &self.roots
    }

    /// `path` from the root (`{root}/src/a.ts`), or `None` outside it.
    pub fn hide(&self, path: &str) -> Option<String> {
        for root in &self.roots {
            if path == root {
                return Some(ROOT.to_string());
            }
            if let Some(rest) = path.strip_prefix(root.as_str())
                && rest.starts_with('/')
            {
                return Some(format!("{ROOT}{rest}"));
            }
        }
        None
    }

    /// A recorded path as a path here: `{root}/…` under this checkout's root, an absolute path
    /// as it is. `None` for a path with a `..` part, which a hint never holds.
    pub fn show(&self, text: &str) -> Option<PathBuf> {
        if text.split('/').any(|part| part == "..") {
            return None;
        }
        if text == ROOT {
            return Some(PathBuf::from(&self.shown));
        }
        if let Some(rest) = text.strip_prefix(ROOT) {
            return rest
                .starts_with('/')
                .then(|| PathBuf::from(format!("{}{rest}", self.shown)));
        }
        text.starts_with('/').then(|| PathBuf::from(text))
    }

    /// Every root's text in `text` replaced by the placeholder, the longest first.
    pub fn hide_text(&self, text: &str) -> String {
        let mut out = text.to_string();
        for root in &self.roots {
            out = out.replace(root.as_str(), ROOT);
        }
        out
    }

    /// The placeholder in `text` replaced by the root as shown here.
    pub fn show_text(&self, text: &str) -> String {
        text.replace(ROOT, &self.shown)
    }

    /// The folders above the root, nearest first, to `/`: where the compiler walks up looking for
    /// `package.json` and `node_modules`.
    pub fn above(&self) -> Vec<PathBuf> {
        Path::new(&self.shown)
            .ancestors()
            .skip(1)
            .map(Path::to_path_buf)
            .collect()
    }

    /// For a path outside the root, the name below the nearest folder above the root that holds
    /// it (`/home/me/package.json` for a root `/home/me/repo` is `package.json`): `None` when no
    /// folder above the root holds it (another root's form) or it is the root's own name.
    pub fn above_name(&self, path: &str) -> Option<String> {
        let path = Path::new(path);
        let root = Path::new(&self.shown);
        if path.starts_with(root) {
            return None;
        }
        self.above()
            .iter()
            .find_map(|dir| path.strip_prefix(dir).ok())
            .and_then(|rest| rest.to_str())
            .filter(|rest| !rest.is_empty())
            .map(str::to_string)
    }
}

/// TypeScript's `swapCase`: the compiler tells whether file names keep case by looking for its
/// own file under the name with every letter's case swapped (B-102).
pub fn swap_case(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect()
}

/// What the first key holds (B-102).
#[derive(Debug, Clone, Serialize)]
struct StepBody<'a> {
    schema: &'static str,
    kind: &'static str,
    program: &'a str,
    version: &'a str,
    identity: String,
    args: Vec<String>,
    cwd: String,
    platform: &'a str,
    case_sensitive: bool,
    tty: bool,
    env: Vec<(String, String)>,
    person: Vec<(String, Option<String>)>,
}

/// The first key of `call` and its body as text for the key log. `cwd` is the call's folder from
/// the root; `person` the person's and the repository's `build: env:` variables with their values.
pub fn step(
    call: &Call,
    identity: &Digest,
    places: &Places,
    cwd: &str,
    case_sensitive: bool,
    person: Vec<(String, Option<String>)>,
) -> (Digest, String) {
    let body = StepBody {
        schema: STEP_SCHEMA,
        kind: super::KIND,
        program: &call.program,
        version: &call.version,
        identity: identity.hex(),
        args: call.args.iter().map(|a| places.hide_text(a)).collect(),
        cwd: cwd.to_string(),
        platform: &call.platform,
        case_sensitive,
        tty: call.tty,
        env: KEYED_ENV
            .iter()
            .filter_map(|name| call.env.get(*name).map(|v| ((*name).to_string(), v.clone())))
            .collect(),
        person,
    };
    canonical(&serde_json::to_value(&body).expect("a key's body is plain data"))
}

fn canonical(value: &serde_json::Value) -> (Digest, String) {
    let text = hushrun_core::jcs::canonicalize(value)
        .expect("strings, numbers and lists always have a canonical form");
    (Digest::of(text.as_bytes()), text)
}

/// What a compile read, by path, as the store keeps it under its first key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hint {
    pub schema: String,
    /// Files read, in the order their digests go into the second key.
    pub reads: Vec<String>,
    /// Paths looked at, with `f`, `d`, `o` or `-`.
    pub looks: Vec<(String, String)>,
    /// Folders listed, with the digest of their entries, or none when they couldn't be.
    pub lists: Vec<(String, Option<Digest>)>,
    /// Paths resolved, with where they led, or none when they couldn't be.
    pub links: Vec<(String, Option<String>)>,
    /// Names looked for above the checkout and not found: absent below every folder above it.
    pub above: Vec<String>,
}

impl Hint {
    /// A hint from its bytes, when it is one.
    pub fn read(bytes: &[u8]) -> Option<Hint> {
        let hint: Hint = serde_json::from_slice(bytes).ok()?;
        (hint.schema == HINT_SCHEMA).then_some(hint)
    }

    /// Its bytes.
    pub fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a hint is plain data")
    }

    /// Whether it names a path outside the checkout: a block made from it is tied (B-104).
    pub fn outside(&self) -> bool {
        let outside = |p: &String| !p.starts_with(ROOT);
        self.reads.iter().any(outside)
            || self.looks.iter().map(|(p, _)| p).any(outside)
            || self.lists.iter().map(|(p, _)| p).any(outside)
            || self
                .links
                .iter()
                .any(|(p, to)| outside(p) || to.as_ref().is_some_and(outside))
    }
}

/// Why a record makes no hint, in the run's words (B-103).
pub type Unkeyable = &'static str;

/// The hint of a record and the digests of its reads, in order: every path from the root, or
/// absolute where it lies outside it (the block is then tied), but names looked for above the
/// checkout and not found, and the compiler's look at its own file under swapped case, which the
/// first key holds.
pub fn hint_of(
    record: &Record,
    places: &Places,
    compiler: &str,
) -> Result<(Hint, Vec<Digest>), Unkeyable> {
    let case_probe = swap_case(compiler);
    let named = |p: &str| -> Result<String, Unkeyable> {
        if !p.starts_with('/') || p.split('/').any(|part| part == "..") {
            return Err("path");
        }
        if p.contains(ROOT) {
            return Err("placeholder");
        }
        Ok(places.hide(p).unwrap_or_else(|| p.to_string()))
    };
    let mut reads: Vec<(String, Digest)> = Vec::with_capacity(record.reads.len());
    for (path, sha256, _) in &record.reads {
        let digest = Digest::from_hex(sha256).map_err(|_| "digest")?;
        reads.push((named(path)?, digest));
    }
    reads.sort_by(|a, b| a.0.cmp(&b.0));
    reads.dedup_by(|a, b| a.0 == b.0);
    let mut looks = Vec::new();
    let mut above = Vec::new();
    for (path, kind) in &record.looks {
        if *path == case_probe {
            continue;
        }
        if !matches!(kind.as_str(), "f" | "d" | "o" | "-") {
            return Err("kind");
        }
        if kind == "-"
            && places.hide(path).is_none()
            && let Some(name) = places.above_name(path)
        {
            if name.split('/').any(|part| part == "..") {
                return Err("path");
            }
            above.push(name);
            continue;
        }
        looks.push((named(path)?, kind.clone()));
    }
    let mut lists = Vec::new();
    for (path, entries) in &record.lists {
        lists.push((named(path)?, entries.as_ref().map(|e| listing(e.clone()))));
    }
    let mut links = Vec::new();
    for (path, to) in &record.links {
        let to = match to {
            Some(to) => Some(named(to)?),
            None => None,
        };
        links.push((named(path)?, to));
    }
    looks.sort();
    looks.dedup_by(|a, b| a.0 == b.0);
    lists.sort();
    lists.dedup_by(|a, b| a.0 == b.0);
    links.sort();
    links.dedup_by(|a, b| a.0 == b.0);
    above.sort();
    above.dedup();
    let (paths, digests): (Vec<String>, Vec<Digest>) = reads.into_iter().unzip();
    Ok((
        Hint {
            schema: HINT_SCHEMA.to_string(),
            reads: paths,
            looks,
            lists,
            links,
            above,
        },
        digests,
    ))
}

/// The digest of a folder's entries: names and kinds, sorted by name's bytes.
pub fn listing(mut entries: Vec<(String, String)>) -> Digest {
    entries.sort();
    entries.dedup();
    Digest::of(&serde_json::to_vec(&entries).expect("plain data"))
}

/// A folder's entries as Node's `readdirSync` with file types names them: `f`, `d`, `l`, `o`.
pub fn list_here(dir: &Path) -> Option<Digest> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.ok()?;
        let kind = entry.file_type().ok()?;
        let word = if kind.is_symlink() {
            "l"
        } else if kind.is_file() {
            "f"
        } else if kind.is_dir() {
            "d"
        } else {
            "o"
        };
        out.push((entry.file_name().to_str()?.to_string(), word.to_string()));
    }
    Some(listing(out))
}

/// A path's kind as Node's `statSync` gives it, following links: `-` when absent.
pub fn kind_here(path: &Path) -> Option<&'static str> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Some("f"),
        Ok(meta) if meta.is_dir() => Some("d"),
        Ok(_) => Some("o"),
        Err(err)
            if err.kind() == std::io::ErrorKind::NotFound
                || err.kind() == std::io::ErrorKind::NotADirectory =>
        {
            Some("-")
        }
        Err(_) => None,
    }
}

/// Whether everything but the reads of `hint` holds here: each kind, listing, link and absence.
/// The reads are hashed by the caller.
pub fn holds(hint: &Hint, places: &Places) -> bool {
    for (path, kind) in &hint.looks {
        let Some(here) = places.show(path) else {
            return false;
        };
        if kind_here(&here) != Some(kind.as_str()) {
            return false;
        }
    }
    for (path, digest) in &hint.lists {
        let Some(here) = places.show(path) else {
            return false;
        };
        if list_here(&here) != *digest {
            return false;
        }
    }
    for (path, to) in &hint.links {
        let Some(here) = places.show(path) else {
            return false;
        };
        let led = std::fs::canonicalize(&here).ok().map(|p| {
            let text = p.to_string_lossy().into_owned();
            places.hide(&text).unwrap_or(text)
        });
        if led != *to {
            return false;
        }
    }
    let above = places.above();
    for name in &hint.above {
        if name.split('/').any(|part| part == ".." || part.is_empty()) {
            return false;
        }
        for dir in &above {
            if kind_here(&dir.join(name)) != Some("-") {
                return false;
            }
        }
    }
    true
}

/// The second key: the first, the hint and the digests of its reads, in order.
pub fn block(step: &Digest, hint: &Hint, reads: &[Digest]) -> Digest {
    let body = serde_json::json!({
        "schema": BLOCK_SCHEMA,
        "step": step.hex(),
        "hint": Digest::of(&hint.bytes()).hex(),
        "reads": reads.iter().map(Digest::hex).collect::<Vec<_>>(),
    });
    canonical(&body).0
}

/// The key of a block tied to this checkout's folder (B-104): its outputs hold the root's text,
/// or its hint a path outside the checkout.
pub fn tied(block: &Digest, places: &Places) -> Digest {
    let body = serde_json::json!({
        "schema": TIED_SCHEMA,
        "block": block.hex(),
        "roots": places.roots(),
    });
    canonical(&body).0
}

/// The compiler's identity (B-102): the SHA-256 of its file, memoised in `memo` by the file's
/// path, size, dates, inode and device.
pub fn identity(compiler: &Path, memo: &Path) -> std::io::Result<Digest> {
    let meta = std::fs::metadata(compiler)?;
    let stat = stat_words(&meta);
    let name = Digest::of(format!("{}\n{stat}", compiler.display()).as_bytes()).hex();
    let file = memo.join(&name[..32]);
    if let Ok(text) = std::fs::read_to_string(&file)
        && let Ok(digest) = Digest::from_hex(text.trim())
    {
        return Ok(digest);
    }
    let digest = Digest::of(&std::fs::read(compiler)?);
    if stat_words(&std::fs::metadata(compiler)?) == stat {
        let _ = std::fs::create_dir_all(memo);
        let temp = memo.join(crate::baked::unique(".id.tmp"));
        if std::fs::write(&temp, digest.hex()).is_ok() {
            let _ = std::fs::rename(&temp, &file);
        }
    }
    Ok(digest)
}

#[cfg(unix)]
fn stat_words(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt as _;
    format!(
        "{} {} {} {} {} {}",
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime_nsec(),
        meta.ino(),
        meta.dev()
    )
}

#[cfg(not(unix))]
fn stat_words(meta: &std::fs::Metadata) -> String {
    format!("{} {:?}", meta.len(), meta.modified().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places() -> Places {
        Places::new(&["/w/repo".to_string(), "/real/w/repo".to_string()])
    }

    // covers: B-103
    #[test]
    fn paths_are_kept_from_the_root_in_either_form() {
        let p = places();
        assert_eq!(p.hide("/w/repo/src/a.ts").as_deref(), Some("{root}/src/a.ts"));
        assert_eq!(p.hide("/real/w/repo/src/a.ts").as_deref(), Some("{root}/src/a.ts"));
        assert_eq!(p.hide("/w/repo").as_deref(), Some("{root}"));
        assert_eq!(p.hide("/w/repo2/a.ts"), None);
        assert_eq!(p.show("{root}/src/a.ts").unwrap(), PathBuf::from("/real/w/repo/src/a.ts"));
        assert_eq!(p.show("{root}/../x"), None);
        assert_eq!(p.show("relative"), None);
        assert_eq!(p.hide_text("error /w/repo/a.ts and /real/w/repo/b.ts"), "error {root}/a.ts and {root}/b.ts");
        assert_eq!(p.show_text("{root}/a.ts"), "/real/w/repo/a.ts");
    }

    // covers: B-103
    #[test]
    fn names_looked_for_above_the_checkout_are_kept_by_name() {
        let p = places();
        assert_eq!(p.above_name("/real/w/package.json").as_deref(), Some("package.json"));
        assert_eq!(p.above_name("/real/node_modules/@types").as_deref(), Some("node_modules/@types"));
        assert_eq!(p.above_name("/real/w/repo/x"), None);
        assert_eq!(p.above(), vec![PathBuf::from("/real/w"), PathBuf::from("/real"), PathBuf::from("/")]);
    }

    fn record() -> Record {
        let sha = Digest::of(b"x").hex();
        Record {
            schema: super::super::call::RECORD_SCHEMA.into(),
            reads: vec![
                ("/real/w/repo/src/b.ts".into(), sha.clone(), 1),
                ("/real/w/repo/src/a.ts".into(), sha, 1),
            ],
            looks: vec![
                ("/real/w/repo/src/package.json".into(), "-".into()),
                ("/real/w/package.json".into(), "-".into()),
                ("/REAL/W/REPO/NODE_MODULES/TYPESCRIPT/LIB/_TSC.JS".into(), "-".into()),
            ],
            lists: vec![("/real/w/repo/src".into(), Some(vec![("a.ts".into(), "f".into())]))],
            links: vec![("/w/repo".into(), Some("/real/w/repo".into()))],
            ..Record::default()
        }
    }

    // covers: B-102, B-103
    #[test]
    fn a_hint_holds_paths_from_the_root_sorted_and_leaves_out_the_case_probe() {
        let (hint, digests) = hint_of(
            &record(),
            &places(),
            "/real/w/repo/node_modules/typescript/lib/_tsc.js",
        )
        .unwrap();
        assert_eq!(hint.reads, vec!["{root}/src/a.ts", "{root}/src/b.ts"]);
        assert_eq!(digests.len(), 2);
        assert_eq!(hint.looks, vec![("{root}/src/package.json".to_string(), "-".to_string())]);
        assert_eq!(hint.above, vec!["package.json"]);
        assert_eq!(hint.links, vec![("{root}".to_string(), Some("{root}".to_string()))]);
        assert!(!hint.outside());
        assert_eq!(Hint::read(&hint.bytes()), Some(hint));
    }

    // covers: B-104
    #[test]
    fn a_read_outside_the_checkout_ties_the_block() {
        let mut r = record();
        r.reads.push(("/usr/lib/node_modules/typescript/lib/lib.d.ts".into(), Digest::of(b"l").hex(), 1));
        let (hint, _) = hint_of(&r, &places(), "/c").unwrap();
        assert!(hint.outside());
    }

    // covers: B-103
    #[test]
    fn a_hint_is_checked_against_the_files_as_they_are_now() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "a").unwrap();
        let text = root.to_string_lossy().into_owned();
        let p = Places::new(std::slice::from_ref(&text));
        let r = Record {
            looks: vec![
                (format!("{text}/src/package.json"), "-".into()),
                (format!("{text}/src/a.ts"), "f".into()),
            ],
            lists: vec![(format!("{text}/src"), Some(vec![("a.ts".into(), "f".into())]))],
            links: vec![(text.clone(), Some(text.clone()))],
            ..Record::default()
        };
        let (hint, _) = hint_of(&r, &p, "/c").unwrap();
        assert!(holds(&hint, &p));
        // A file that appears where one was looked for and not found.
        std::fs::write(root.join("src/package.json"), "{}").unwrap();
        assert!(!holds(&hint, &p));
        std::fs::remove_file(root.join("src/package.json")).unwrap();
        assert!(holds(&hint, &p));
        // A new file in a folder the compiler listed.
        std::fs::write(root.join("src/c.ts"), "c").unwrap();
        assert!(!holds(&hint, &p));
        std::fs::remove_file(root.join("src/c.ts")).unwrap();
        // A name looked for above the checkout that appears there.
        let mut above = hint.clone();
        above.above.push("hushrun-test-above.json".into());
        assert!(holds(&above, &p));
        std::fs::write(dir.path().join("hushrun-test-above.json"), "{}").unwrap();
        assert!(!holds(&above, &p));
    }

    // covers: B-102
    #[test]
    fn swapping_case_is_typescript_s() {
        assert_eq!(swap_case("/a/B_c.js"), "/A/b_C.JS");
    }

    // covers: B-102
    #[test]
    fn the_identity_is_the_compiler_s_bytes_and_is_memoised_by_stat() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("_tsc.js");
        std::fs::write(&file, "one").unwrap();
        let memo = dir.path().join("ids");
        assert_eq!(identity(&file, &memo).unwrap(), Digest::of(b"one"));
        assert_eq!(identity(&file, &memo).unwrap(), Digest::of(b"one"));
        std::fs::write(&file, "two!").unwrap();
        assert_eq!(identity(&file, &memo).unwrap(), Digest::of(b"two!"));
    }
}
