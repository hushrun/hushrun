//! Changes cargo can't see (B-23): a record of what each compile read beside its outputs, and
//! the check `hushrun run` makes before COMMAND starts.
//!
//! Cargo decides whether to call rustc by dates and by the files rustc listed last time. A file
//! added to a folder a macro lists, a file a known macro reads unseen, a module that appears
//! where one could have been, or a file whose bytes changed under an older date never reach the
//! wrapper. Its keys cover all four (B-6); they are only never consulted. So every compile the
//! wrapper sees through to its end leaves a **unit record** in the output folder's `.hushrun/`,
//! and before a run the check compares each record with the disk and removes the outputs of a
//! unit whose inputs changed that way: cargo finds them missing and calls rustc, and the wrapper
//! answers by content as usual.
//!
//! # The record
//!
//! `<out dir>/.hushrun/units/<crate><extra-filename>.json`, written whole (a temporary name
//! that begins with a dot, then a rename) after the outputs are in place. It names the unit's
//! outputs by inode, device and size (rustc writes each as a new file; cargo moves their dates),
//! and what to watch, each with what was seen: a file's size, both dates, inode and device, and
//! its SHA-256 when the compile hashed it; "absent"; a folder's entry names. Only what lies
//! outside the shared folders (registry and toolchain files never change) is watched, so a
//! crate with nothing local to watch gets no record. [`settle`] is the only code that writes or
//! removes one.
//!
//! # The check
//!
//! [`check`] reads the registry of output folders ([`Store::out_dirs`]) and takes those whose
//! compiles ran in the folder `hushrun run` runs in, inside it or above it. It takes cargo's own
//! lock on each target folder first and skips one a build holds, so no cargo uses an output it
//! removes. It reads no file unless a stat differs and a digest is on record to settle it, never
//! touches a source file, and removes only the files a record lists, the dep-info aside, so an
//! edit keeps incremental compilation (B-9). A record that doesn't describe the outputs there
//! any more (plain cargo built them since) is removed and its outputs are left.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use hushrun_digest::cache::Stat;
use serde::{Deserialize, Serialize};

use crate::depinfo::DepInfo;
use crate::inputs::{self, Context};
use crate::invocation::{Invocation, NativeKind, SearchKind};
use crate::key::{Observed, ReadState};
use crate::known;
use crate::model::{Class, Located};
use crate::store::{OutDir, Store};
use crate::{link, regular, shape, sidecar};

/// The folder under an output folder's `.hushrun/` that holds the unit records.
pub const UNITS: &str = "units";

/// The schema name every record carries. A record with another one is ignored.
const SCHEMA: &str = "hushrun.build.unit/1";

/// The lock cargo holds on a target folder's profile folder for as long as a build runs.
const CARGO_LOCK: &str = ".cargo-lock";

/// The most lines the check adds to a run's summary.
const MAX_LINES: usize = 3;

/// A file as the record holds it: what a later stat is compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    /// Size in bytes.
    pub size: u64,
    /// Modification time, nanoseconds since the epoch.
    pub mtime_ns: i64,
    /// Change time, nanoseconds since the epoch: nobody can set it back.
    pub ctime_ns: i64,
    /// Inode.
    pub inode: u64,
    /// Device.
    pub device: u64,
}

impl FileStat {
    fn of(meta: &fs::Metadata) -> FileStat {
        let stat = Stat::of(meta);
        FileStat {
            size: stat.size,
            mtime_ns: stat.mtime_ns,
            ctime_ns: stat.ctime_ns,
            inode: stat.inode,
            device: stat.device,
        }
    }

    fn as_stat(&self) -> Stat {
        Stat {
            size: self.size,
            mtime_ns: self.mtime_ns,
            ctime_ns: self.ctime_ns,
            inode: self.inode,
            device: self.device,
            mode: 0,
        }
    }

    /// The same folder, unchanged: no entry was added, removed or renamed in it.
    fn same_folder(&self, other: &FileStat) -> bool {
        self.mtime_ns == other.mtime_ns
            && self.ctime_ns == other.ctime_ns
            && self.inode == other.inode
            && self.device == other.device
    }
}

/// One output of the unit, by what rustc's writing a new file changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OutputAt {
    path: PathBuf,
    inode: u64,
    device: u64,
    size: u64,
}

impl OutputAt {
    fn describes(&self, meta: &fs::Metadata) -> bool {
        let stat = Stat::of(meta);
        stat.inode == self.inode && stat.device == self.device && stat.size == self.size
    }
}

/// One file watched, or one place that was absent (`stat` is `None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileAt {
    path: PathBuf,
    #[serde(default)]
    stat: Option<FileStat>,
    #[serde(default)]
    sha256: Option<Digest>,
    /// Listed in the dep-info: cargo itself sees a newer date or the file gone.
    read: bool,
}

/// One folder watched, by its entry names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DirAt {
    path: PathBuf,
    stat: FileStat,
    names: Vec<String>,
}

/// A unit record as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    schema: String,
    crate_name: String,
    #[serde(default)]
    package: Option<String>,
    cwd: PathBuf,
    /// When the compile or the reuse began: a date at or after it is one cargo sees.
    since_ns: i64,
    outputs: Vec<OutputAt>,
    files: Vec<FileAt>,
    dirs: Vec<DirAt>,
}

/// Something a compile leaves to watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A file, or a place where none was: present or absent as it is now. `sha256` is what the
    /// compile hashed, when it did; `read` says the dep-info lists it.
    File {
        /// The path.
        path: PathBuf,
        /// What the compile's key holds for it.
        sha256: Option<Digest>,
        /// Whether rustc's dep-info lists it.
        read: bool,
    },
    /// A folder, by its entry names.
    Dir(PathBuf),
}

impl Item {
    fn path(&self) -> &Path {
        match self {
            Item::File { path, .. } | Item::Dir(path) => path,
        }
    }
}

/// A compile the wrapper saw through to a successful end, for its record.
#[derive(Debug, Clone)]
pub struct Seen {
    /// The crate's name.
    pub crate_name: String,
    /// The package's name, when cargo said it.
    pub package: Option<String>,
    /// rustc's working directory.
    pub cwd: PathBuf,
    /// The outputs: what `link` and `metadata` produced. The dep-info is not one.
    pub outputs: Vec<PathBuf>,
    /// What to watch.
    pub items: Vec<Item>,
    /// When the compile or the reuse began, as file dates count it: an [`inputs::floor`].
    pub since: SystemTime,
}

/// Where the record of the unit `stem` (`<crate><extra-filename>`) in `out_dir` lives.
pub fn record_path(out_dir: &Path, stem: &str) -> PathBuf {
    out_dir
        .join(sidecar::DIR)
        .join(UNITS)
        .join(format!("{stem}.json"))
}

/// Record written, or record removed: the one place that decides whether a unit has a record.
/// With `seen`, the record is written when it can describe the unit as it is now: every output
/// there, every watched file and folder unchanged since the compile began (a date at or after
/// `since` means it changed while compiling, and the record would pair new bytes with an old
/// digest). Otherwise, and with `None` (before rustc runs, or for a compile the wrapper can't
/// see through), any record there is removed. Returns whether one was written.
///
/// Cost: one `stat` per output and per file watched, one `readdir` per folder watched, and one
/// small write and rename; with `None`, one `unlink`.
pub fn settle(out_dir: &Path, stem: &str, seen: Option<&Seen>) -> bool {
    let path = record_path(out_dir, stem);
    let written = seen
        .and_then(describe)
        .is_some_and(|record| write(&path, &record).is_ok());
    if !written {
        remove(&path);
    }
    written
}

/// Removes the record of the unit the arguments of a call name (`--crate-name`,
/// `-C extra-filename`, `--out-dir`), for a call the wrapper runs as asked without reading them:
/// no session, or arguments the parser refused (B-23). Cost: a scan of the arguments and one
/// `unlink`.
pub fn forget_for_args(args: &[std::ffi::OsString], cwd: Option<&Path>) {
    if let Some((dir, stem)) = unit_of_args(args, cwd) {
        remove(&record_path(&dir, &stem));
    }
}

/// Marks the unit the arguments name as compiled as asked ([`crate::sidecar::mark_as_asked`]),
/// for a call the parser refused inside a run: rustc runs without the wrapper's remaps.
pub fn mark_as_asked_for_args(args: &[std::ffi::OsString], cwd: &Path) {
    if let Some((dir, stem)) = unit_of_args(args, Some(cwd)) {
        crate::sidecar::mark_as_asked(&dir, &stem);
    }
}

/// The output folder and the unit's stem (`<crate name><extra filename>`) the arguments name.
fn unit_of_args(args: &[std::ffi::OsString], cwd: Option<&Path>) -> Option<(PathBuf, String)> {
    let mut crate_name = None;
    let mut extra = None;
    let mut out_dir = None;
    let mut iter = args.iter().filter_map(|arg| arg.to_str());
    while let Some(arg) = iter.next() {
        let (flag, value) = match arg.split_once('=') {
            Some((flag, value)) if flag == "--crate-name" || flag == "--out-dir" => {
                (flag, Some(value))
            }
            _ => (arg, None),
        };
        match flag {
            "--crate-name" => crate_name = value.or_else(|| iter.next()),
            "--out-dir" => out_dir = value.or_else(|| iter.next()),
            "-C" => {
                if let Some(rest) = iter.next().and_then(|v| v.strip_prefix("extra-filename=")) {
                    extra = Some(rest);
                }
            }
            _ => {
                if let Some(rest) = arg.strip_prefix("-Cextra-filename=") {
                    extra = Some(rest);
                }
            }
        }
    }
    let (Some(name), Some(dir)) = (crate_name, out_dir) else {
        return None;
    };
    let dir = Path::new(dir);
    let dir = match (dir.is_absolute(), cwd) {
        (true, _) => dir.to_path_buf(),
        (false, Some(cwd)) => cwd.join(dir),
        (false, None) => return None,
    };
    Some((dir, format!("{name}{}", extra.unwrap_or(""))))
}

fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

/// The record `seen` gives, or `None` when it can't describe the unit as it is now.
fn describe(seen: &Seen) -> Option<Record> {
    if seen.items.is_empty() || seen.outputs.is_empty() {
        return None;
    }
    let mut outputs = Vec::with_capacity(seen.outputs.len());
    for path in &seen.outputs {
        let stat = Stat::of(&fs::metadata(path).ok()?);
        outputs.push(OutputAt {
            path: path.clone(),
            inode: stat.inode,
            device: stat.device,
            size: stat.size,
        });
    }
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut seen_paths = BTreeSet::new();
    for item in &seen.items {
        if !seen_paths.insert(item.path().to_path_buf()) {
            continue;
        }
        let meta = match fs::metadata(item.path()) {
            Ok(meta) => Some(meta),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };
        let stat = meta.as_ref().map(FileStat::of);
        if stat.is_some_and(|stat| inputs::dated_after(&stat.as_stat(), seen.since)) {
            return None;
        }
        match (item, meta) {
            (Item::Dir(path), Some(meta)) if meta.is_dir() => dirs.push(DirAt {
                path: path.clone(),
                stat: FileStat::of(&meta),
                names: shape::names(path).ok()?,
            }),
            (Item::Dir(path), _) => files.push(FileAt {
                path: path.clone(),
                stat,
                sha256: None,
                read: false,
            }),
            (Item::File { path, sha256, read }, _) => files.push(FileAt {
                path: path.clone(),
                stat,
                // A digest only for what is there: an absent place has none.
                sha256: stat.and(*sha256),
                read: *read,
            }),
        }
    }
    Some(Record {
        schema: SCHEMA.to_string(),
        crate_name: seen.crate_name.clone(),
        package: seen.package.clone(),
        cwd: seen.cwd.clone(),
        since_ns: ns(seen.since),
        outputs,
        files,
        dirs,
    })
}

fn ns(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_nanos()).unwrap_or(i64::MAX),
    }
}

/// Writes a record whole: a temporary name in its folder that begins with a dot, then a rename.
fn write(path: &Path, record: &Record) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new(""));
    fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let pid = std::process::id();
    for attempt in 0..1000u32 {
        let temp = dir.join(format!(".{name}.{pid}.{attempt}.tmp"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(mut file) => {
                let written = io::Write::write_all(&mut file, &bytes).and_then(|()| {
                    drop(file);
                    fs::rename(&temp, path)
                });
                if written.is_err() {
                    let _ = fs::remove_file(&temp);
                }
                return written;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free temporary name",
    ))
}

fn read(path: &Path) -> Option<Record> {
    let bytes = fs::read(path).ok()?;
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    (record.schema == SCHEMA).then_some(record)
}

/// The shared folders (B-8) in their given and resolved forms: what lies under one is the same
/// for every checkout and never watched.
#[derive(Debug, Clone, Default)]
pub struct Shared {
    folders: Vec<PathBuf>,
}

impl Shared {
    /// The folders, each with its resolved form. Cost: one `realpath` each.
    pub fn new(folders: &[String]) -> Shared {
        let mut out = Vec::with_capacity(folders.len() * 2);
        for folder in folders {
            let path = PathBuf::from(folder);
            if let Ok(resolved) = fs::canonicalize(&path)
                && resolved != path
            {
                out.push(resolved);
            }
            out.push(path);
        }
        Shared { folders: out }
    }

    /// Whether `path` lies under a shared folder.
    pub fn holds(&self, path: &Path) -> bool {
        self.folders.iter().any(|folder| path.starts_with(folder))
    }
}

/// `path` with `.` and `..` taken out by their names, for looking it up among the paths a key
/// names. Never opened as it is.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The digests a compile's key holds for the places it read (B-6): the files the dep-info
/// lists, what was declared and the known macros read, the native libraries and the static
/// libraries in native folders, by where they are in this call.
pub fn digests(cx: &Context<'_>, observed: &Observed) -> BTreeMap<PathBuf, Digest> {
    let mut out = BTreeMap::new();
    let reads = observed
        .reads
        .iter()
        .chain(observed.native.iter())
        .chain(observed.declared.iter().flat_map(|d| d.files.iter()));
    for read in reads {
        let ReadState::Present(sha) = read.state else {
            continue;
        };
        // What lies in a shared folder or a base is the same for every checkout: never watched.
        if matches!(&read.at, Located::Shared(_))
            || matches!(&read.at, Located::Rooted(rooted) if rooted.root.is_base())
        {
            continue;
        }
        if let Some(path) = cx.open_at(&read.at) {
            out.insert(lexical(&path), sha);
        }
    }
    for dir in &observed.native_dirs {
        let Some(path) = cx.open_at(&dir.dir) else {
            continue;
        };
        for (name, sha) in &dir.archives {
            out.insert(lexical(&path.join(name)), *sha);
        }
    }
    out
}

/// What a unit leaves to watch (B-23): each file the dep-info lists and each place a module of
/// a listed `.rs` file could also have been; the folder that holds each listed file; the inputs
/// of the known macros among its externs, folders walked in full; for a library, each native
/// folder by its names and the static libraries in it; and the places each `-l` may be found.
/// Nothing under a shared folder or in the unit's own output folder. `digests` gives what the
/// compile hashed, where it did.
///
/// Cost: no file is opened; one `stat` per known folder entry walked.
pub fn items(
    inv: &Invocation,
    info: &DepInfo,
    shared: &Shared,
    digests: &BTreeMap<PathBuf, Digest>,
) -> Vec<Item> {
    let cwd = PathBuf::from(inv.cwd());
    let absolute = |path: &str| {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        }
    };
    let out_dir = absolute(inv.out_dir());
    let keep = |path: &Path| !shared.holds(path) && !path.starts_with(&out_dir);
    let sha = |path: &Path| digests.get(&lexical(path)).copied();
    let mut items: Vec<Item> = Vec::new();
    let file = |items: &mut Vec<Item>, path: PathBuf, read: bool| {
        if keep(&path) {
            items.push(Item::File {
                sha256: sha(&path),
                path,
                read,
            });
        }
    };
    let mut folders = BTreeSet::new();
    for listed in &info.files {
        let path = absolute(listed);
        if let Some(parent) = path.parent() {
            folders.insert(parent.to_path_buf());
        }
        file(&mut items, path, true);
    }
    for listed in &info.files {
        if let Some(candidate) = crate::depinfo::module_candidate(listed) {
            file(&mut items, absolute(&candidate), false);
        }
    }
    for folder in folders {
        if keep(&folder) {
            items.push(Item::Dir(folder));
        }
    }

    // The known macros' inputs (B-6 `declared`): files, and folders walked in full so that a
    // file added anywhere in them is seen.
    let names: Vec<&str> = inv.externs().iter().map(|e| e.name.as_str()).collect();
    let known = known::inputs(&names);
    let manifest = inv.package().manifest_dir.as_deref().map(absolute);
    for place in &known.files {
        for path in inputs::places(place, manifest.as_deref(), &cwd) {
            file(&mut items, path, false);
        }
    }
    let mut skip = vec![out_dir.clone()];
    if let Some(script_out) = &inv.package().out_dir {
        skip.push(absolute(script_out));
    }
    for place in &known.folders {
        for path in inputs::places(place, manifest.as_deref(), &cwd) {
            if !keep(&path) {
                continue;
            }
            if path.is_dir() {
                walk(&path, &skip, &mut |entry, is_dir| {
                    if is_dir {
                        items.push(Item::Dir(entry.to_path_buf()));
                    } else {
                        items.push(Item::File {
                            sha256: sha(entry),
                            path: entry.to_path_buf(),
                            read: false,
                        });
                    }
                });
            } else {
                file(&mut items, path, false);
            }
        }
    }

    // Native folders, for a library: a static library found there is bundled unseen (B-6).
    if inv.class() == Class::Library {
        for search in inv.search_paths() {
            if !matches!(search.kind, SearchKind::Native | SearchKind::All) {
                continue;
            }
            let dir = absolute(&search.path);
            if !keep(&dir) {
                continue;
            }
            if let Ok(names) = shape::names(&dir) {
                for name in names {
                    if name.ends_with(".a") || name.ends_with(".lib") {
                        file(&mut items, dir.join(&name), false);
                    }
                }
                items.push(Item::Dir(dir));
            } else {
                file(&mut items, dir, false);
            }
        }
    }
    // Every place each `-l` may be found (B-6 `native`).
    for lib in inv.native_libs() {
        let names = match lib.kind {
            NativeKind::Framework => link::framework_files(&lib.name),
            NativeKind::Static | NativeKind::Dylib => link::library_files(&lib.name),
        };
        for dir in inv.native_dirs(lib.kind) {
            let dir = absolute(dir);
            for name in &names {
                file(&mut items, dir.join(name), false);
            }
        }
    }
    items
}

/// Calls `found` for every folder and file under `dir` (not `dir` itself first: it is passed as
/// a folder too), skipping `.git`, folders named `target` and the folders in `skip`, as the key's
/// walk does (B-6). A symlink is a file here.
fn walk(dir: &Path, skip: &[PathBuf], found: &mut dyn FnMut(&Path, bool)) {
    found(dir, true);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            let name = entry.file_name();
            if name == ".git" || name == "target" || skip.iter().any(|s| s.starts_with(&path)) {
                continue;
            }
            walk(&path, skip, found);
        } else {
            found(&path, false);
        }
    }
}

/// One unit the check removed, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebuilt {
    /// The crate, or `the build script of <package>`.
    pub who: String,
    /// What changed, in the person's words.
    pub cause: String,
}

/// What the check did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checked {
    /// Output folders looked at.
    pub out_dirs: u64,
    /// Output folders skipped because a build holds cargo's lock there.
    pub busy: u64,
    /// Unit records read.
    pub units: u64,
    /// Records that no longer described the outputs there, removed with the outputs left.
    pub stale: u64,
    /// The units removed so that cargo builds them, with why.
    pub rebuilt: Vec<Rebuilt>,
    /// The check stopped at its deadline, with units left unchecked.
    pub stopped: bool,
}

impl Checked {
    /// The lines for the run's summary, without hushrun's prefix: one per cause, at most three.
    /// None when nothing was removed.
    pub fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for rebuilt in &self.rebuilt {
            let line = format!("{} is rebuilt: {}", rebuilt.who, rebuilt.cause);
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
        if lines.len() > MAX_LINES {
            let more = lines.len() - (MAX_LINES - 1);
            lines.truncate(MAX_LINES - 1);
            lines.push(format!(
                "{more} more are rebuilt for changes cargo doesn't watch"
            ));
        }
        lines
    }
}

/// What the check found for one record.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// Nothing changed, or nothing cargo can't see.
    Fine,
    /// Cargo will build the unit anyway: a file the dep-info lists is gone or newer.
    CargoSees,
    /// The record doesn't describe the outputs there: removed, outputs left.
    Stale,
    /// Something changed that cargo can't see.
    Changed(String),
}

/// Before a run (B-23): for each output folder the registry holds whose compiles ran in
/// `folder`, inside it or above it, compares every unit record there with the disk, and removes
/// the outputs of a unit whose inputs changed in a way cargo can't see. A target folder a build
/// holds cargo's lock on is skipped. After `deadline` it stops, leaving the rest unchecked.
/// Never an error: what can't be read is left as it is.
pub fn check(store: &Store, folder: &Path, deadline: Option<Instant>) -> Checked {
    let mut checked = Checked::default();
    let folder_forms = forms(folder);
    for entry in store.out_dirs() {
        if !related(&entry, &folder_forms) || !entry.out_dir.is_dir() {
            continue;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            checked.stopped = true;
            break;
        }
        checked.out_dirs += 1;
        check_out_dir(&entry.out_dir, &folder_forms, deadline, &mut checked);
    }
    checked
}

/// A folder in its given and resolved forms.
fn forms(folder: &Path) -> Vec<PathBuf> {
    let mut out = vec![folder.to_path_buf()];
    if let Ok(resolved) = fs::canonicalize(folder)
        && resolved != folder
    {
        out.push(resolved);
    }
    out
}

/// Whether an output folder's compiles ran in the folder, inside it or above it.
fn related(entry: &OutDir, folder: &[PathBuf]) -> bool {
    entry.cwds.iter().any(|cwd| {
        folder
            .iter()
            .any(|form| cwd.starts_with(form) || form.starts_with(cwd))
    })
}

/// Cargo's lock for the target folder `out_dir` is in: `.cargo-lock` in the profile folder,
/// at most three folders up (`deps/`, `build/<unit>/`).
fn cargo_lock(out_dir: &Path) -> Option<fs::File> {
    out_dir
        .ancestors()
        .take(4)
        .map(|dir| dir.join(CARGO_LOCK))
        .find(|lock| lock.is_file())
        .and_then(|lock| fs::OpenOptions::new().read(true).open(lock).ok())
}

fn check_out_dir(
    out_dir: &Path,
    folder: &[PathBuf],
    deadline: Option<Instant>,
    checked: &mut Checked,
) {
    let units = out_dir.join(sidecar::DIR).join(UNITS);
    let Ok(entries) = fs::read_dir(&units) else {
        return;
    };
    // Cargo's own lock: while a build holds it, no output there is touched. Without one this
    // is no cargo target folder, and nothing is touched either.
    let Some(lock) = cargo_lock(out_dir) else {
        checked.busy += 1;
        return;
    };
    if lock.try_lock().is_err() {
        checked.busy += 1;
        return;
    }
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    paths.sort();
    for path in paths {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            checked.stopped = true;
            break;
        }
        // A record that can't be read is ignored: the next compile of the unit replaces it.
        let Some(record) = read(&path) else {
            continue;
        };
        checked.units += 1;
        match judge(&record, folder) {
            Verdict::Fine | Verdict::CargoSees => {}
            Verdict::Stale => {
                checked.stale += 1;
                remove(&path);
            }
            Verdict::Changed(cause) => {
                if remove_outputs(&record) {
                    remove(&path);
                    checked.rebuilt.push(Rebuilt {
                        who: who(&record),
                        cause,
                    });
                }
            }
        }
    }
    drop(lock);
}

/// The unit as the person reads it.
fn who(record: &Record) -> String {
    match (&record.package, record.crate_name.as_str()) {
        (Some(package), "build_script_build") => format!("the build script of {package}"),
        _ => record.crate_name.clone(),
    }
}

/// Removes a unit's outputs that are still the ones its record names, and their records.
/// False when one couldn't be removed: the unit record stays, so the next check tries again.
fn remove_outputs(record: &Record) -> bool {
    let mut all = true;
    for output in &record.outputs {
        match fs::symlink_metadata(&output.path) {
            Ok(meta) if output.describes(&meta) => match fs::remove_file(&output.path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) => all = false,
            },
            _ => {}
        }
        let _ = sidecar::forget(&output.path);
    }
    all
}

/// A path as the person reads it: from the folder `hushrun run` runs in when it lies there.
fn shown(path: &Path, folder: &[PathBuf]) -> String {
    folder
        .iter()
        .find_map(|form| path.strip_prefix(form).ok())
        .filter(|rest| !rest.as_os_str().is_empty())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Compares one record with the disk. Stats and listings only; a file is read only when its
/// stat differs and a digest is on record to settle it.
fn judge(record: &Record, folder: &[PathBuf]) -> Verdict {
    for output in &record.outputs {
        match fs::symlink_metadata(&output.path) {
            Ok(meta) if output.describes(&meta) => {}
            _ => return Verdict::Stale,
        }
    }
    let now: Vec<Option<FileStat>> = record
        .files
        .iter()
        .map(|file| fs::metadata(&file.path).ok().map(|m| FileStat::of(&m)))
        .collect();
    // Cargo builds the unit anyway when a file the dep-info lists is gone or newer than the
    // compile: nothing to say, nothing to remove.
    for (file, now) in record.files.iter().zip(&now) {
        if file.read && file.stat.is_some() && now.is_none_or(|now| now.mtime_ns >= record.since_ns)
        {
            return Verdict::CargoSees;
        }
    }
    let unwatched = |what: String| Verdict::Changed(format!("{what}, which cargo doesn't watch"));
    for (file, now) in record.files.iter().zip(&now) {
        let at = shown(&file.path, folder);
        match (&file.stat, now) {
            (None, None) => {}
            (None, Some(_)) => return unwatched(format!("{at} appeared")),
            (Some(_), None) => return unwatched(format!("{at} was removed")),
            (Some(then), Some(now)) => {
                if regular::same(&then.as_stat(), &now.as_stat()) {
                    continue;
                }
                let same_bytes = file.sha256.is_some_and(|sha| {
                    regular::hash_steady(&file.path).is_ok_and(|(now, _)| now == sha)
                });
                if same_bytes {
                    continue;
                }
                if file.read {
                    return Verdict::Changed(format!(
                        "{at} changed but kept a date older than the last build, which cargo \
                         doesn't see"
                    ));
                }
                return unwatched(format!("{at} changed"));
            }
        }
    }
    for dir in &record.dirs {
        let at = shown(&dir.path, folder);
        let Ok(meta) = fs::metadata(&dir.path) else {
            return unwatched(format!("{at} was removed"));
        };
        if dir.stat.same_folder(&FileStat::of(&meta)) {
            continue;
        }
        let Ok(names) = shape::names(&dir.path) else {
            return unwatched(format!("{at} can't be read"));
        };
        if names == dir.names {
            continue;
        }
        let added = names.iter().any(|name| !dir.names.contains(name));
        let gone = dir.names.iter().any(|name| !names.contains(name));
        return unwatched(match (added, gone) {
            (true, false) => format!("a file was added to {at}"),
            (false, true) => format!("a file was removed from {at}"),
            _ => format!("the files in {at} changed"),
        });
    }
    Verdict::Fine
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn seen(dir: &Path, items: Vec<Item>) -> Seen {
        Seen {
            crate_name: "app".into(),
            package: Some("app".into()),
            cwd: dir.to_path_buf(),
            outputs: vec![dir.join("out/libapp-1.rlib")],
            items,
            // As the wrapper hands it: a floor, here without the file system's clock.
            since: inputs::floor(None, SystemTime::now()),
        }
    }

    struct Unit {
        dir: tempfile::TempDir,
        out: PathBuf,
        src: PathBuf,
    }

    /// A target folder with cargo's lock and one output, and a source folder with one file,
    /// all dated a minute back.
    fn unit() -> Unit {
        let dir = tempfile::tempdir().unwrap();
        let root =
            PathBuf::from(crate::roots::resolve_on_disk(dir.path().to_str().unwrap()).unwrap());
        let out = root.join("target/debug/deps");
        let src = root.join("src");
        fs::create_dir_all(&out).unwrap();
        fs::create_dir_all(&src).unwrap();
        fs::write(root.join("target/debug/.cargo-lock"), b"").unwrap();
        fs::write(out.join("libapp-1.rlib"), b"rlib").unwrap();
        fs::write(src.join("lib.rs"), b"pub fn f() {}\n").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        Unit { dir, out, src }
    }

    impl Unit {
        fn root(&self) -> PathBuf {
            self.out
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf()
        }

        fn seen(&self) -> Seen {
            let lib = self.src.join("lib.rs");
            let mut seen = seen(&self.root(), Vec::new());
            seen.outputs = vec![self.out.join("libapp-1.rlib")];
            seen.items = vec![
                Item::File {
                    sha256: Some(Digest::of(b"pub fn f() {}\n")),
                    path: lib,
                    read: true,
                },
                Item::File {
                    path: self.src.join("lib/mod.rs"),
                    sha256: None,
                    read: false,
                },
                Item::Dir(self.src.clone()),
            ];
            seen
        }

        fn store(&self) -> Store {
            let store = Store::new(&self._store_dir());
            store
                .register_out_dir(&self.out, &self.root(), SystemTime::now())
                .unwrap();
            store
        }

        fn _store_dir(&self) -> PathBuf {
            self.dir.path().join("store")
        }

        fn record(&self) -> PathBuf {
            record_path(&self.out, "app-1")
        }
    }

    // covers: B-23
    #[test]
    fn one_function_writes_a_record_or_removes_it() {
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        let text = fs::read_to_string(u.record()).unwrap();
        assert!(text.contains("lib.rs"), "{text}");
        // No temporary file left, and none beside the outputs.
        let names: Vec<_> = fs::read_dir(u.record().parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["app-1.json"]);
        // Before rustc runs, or for a call that can't be seen through: removed.
        assert!(!settle(&u.out, "app-1", None));
        assert!(!u.record().exists());
        // Nothing local to watch: none.
        let mut nothing = u.seen();
        nothing.items.clear();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        assert!(!settle(&u.out, "app-1", Some(&nothing)));
        assert!(!u.record().exists());
        // A watched file dated during the compile: the record would pair new bytes with an old
        // digest, so there is none, and the old one is gone too.
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        let mut late = u.seen();
        late.since = inputs::floor(None, SystemTime::now());
        std::thread::sleep(Duration::from_millis(20));
        fs::write(u.src.join("lib.rs"), b"pub fn g() {}\n").unwrap();
        assert!(!settle(&u.out, "app-1", Some(&late)));
        assert!(!u.record().exists());
        // An output gone: none.
        let mut gone = u.seen();
        gone.outputs.push(u.out.join("libapp-1.rmeta"));
        assert!(!settle(&u.out, "app-1", Some(&gone)));
    }

    // covers: B-23
    #[test]
    fn a_call_seen_without_its_arguments_read_loses_its_record() {
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        let args: Vec<std::ffi::OsString> = [
            "--crate-name",
            "app",
            "--edition=2021",
            "-C",
            "extra-filename=-1",
            "--out-dir",
            u.out.to_str().unwrap(),
            "@args",
        ]
        .iter()
        .map(Into::into)
        .collect();
        forget_for_args(&args, None);
        assert!(!u.record().exists());
        // The other spellings.
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        let joined = format!("--out-dir={}", u.out.display());
        let args: Vec<std::ffi::OsString> = ["--crate-name=app", "-Cextra-filename=-1", &joined]
            .iter()
            .map(Into::into)
            .collect();
        forget_for_args(&args, None);
        assert!(!u.record().exists());
    }

    // covers: B-23
    #[test]
    fn the_registry_remembers_each_output_folder_once_and_forgets_the_gone() {
        let u = unit();
        let store = u.store();
        store
            .register_out_dir(&u.out, &u.root(), SystemTime::now())
            .unwrap();
        let all = store.out_dirs();
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].out_dir, u.out);
        assert_eq!(all[0].cwds, [u.root()]);
        // A second workspace sharing the target folder is kept beside the first.
        let shared = u.root().join("second");
        fs::create_dir_all(&shared).unwrap();
        store
            .register_out_dir(&u.out, &shared, SystemTime::now())
            .unwrap();
        assert_eq!(store.out_dirs()[0].cwds, [u.root(), shared]);
        // Another folder, then gone, and one not seen for long: both forgotten by the sweep.
        let other = u.root().join("other/target/debug/deps");
        fs::create_dir_all(&other).unwrap();
        store
            .register_out_dir(&other, &u.root().join("other"), SystemTime::now())
            .unwrap();
        let old = u.root().join("old/deps");
        fs::create_dir_all(&old).unwrap();
        store
            .register_out_dir(&old, &u.root(), UNIX_EPOCH + Duration::from_secs(1_000))
            .unwrap();
        assert_eq!(store.out_dirs().len(), 3);
        fs::remove_dir_all(u.root().join("other")).unwrap();
        // A damaged entry is dropped too.
        let entries = u._store_dir().join("out-dirs");
        fs::write(entries.join("damaged.json"), b"{").unwrap();
        let tally = store.remove_stale_out_dirs(UNIX_EPOCH + Duration::from_secs(10_000));
        assert_eq!(tally.removed, 3);
        assert_eq!(store.out_dirs().len(), 1);
    }

    fn checked(u: &Unit) -> Checked {
        check(&u.store(), &u.root(), None)
    }

    // covers: B-23
    #[test]
    fn nothing_changed_removes_nothing_and_says_nothing() {
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        let c = checked(&u);
        assert_eq!((c.units, c.rebuilt.len(), c.stale), (1, 0, 0));
        assert!(c.lines().is_empty());
        assert!(u.out.join("libapp-1.rlib").exists());
        assert!(u.record().exists());
    }

    // covers: B-23
    #[test]
    fn a_file_added_beside_a_read_one_removes_the_outputs_and_says_why() {
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        crate::sidecar::record(
            &u.out.join("libapp-1.rlib"),
            Digest::of(b"rlib"),
            None,
            false,
        )
        .unwrap();
        fs::write(u.src.join("notes.txt"), b"").unwrap();
        let c = checked(&u);
        assert_eq!(
            c.lines(),
            ["app is rebuilt: a file was added to src, which cargo doesn't watch"]
        );
        assert!(!u.out.join("libapp-1.rlib").exists());
        assert!(
            !crate::sidecar::record_path(&u.out.join("libapp-1.rlib"))
                .unwrap()
                .exists()
        );
        assert!(!u.record().exists());
        // The source is untouched.
        assert!(u.src.join("lib.rs").exists() && u.src.join("notes.txt").exists());
    }

    // covers: B-23
    #[test]
    fn a_module_that_appears_and_a_file_changed_under_an_old_date_are_seen() {
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        fs::create_dir_all(u.src.join("lib")).unwrap();
        fs::write(u.src.join("lib/mod.rs"), b"").unwrap();
        let c = checked(&u);
        assert_eq!(c.rebuilt.len(), 1, "{c:?}");
        assert!(
            c.rebuilt[0].cause.starts_with("src/lib/mod.rs appeared"),
            "{c:?}"
        );

        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        let lib = u.src.join("lib.rs");
        let old = fs::metadata(&lib).unwrap().modified().unwrap();
        fs::write(&lib, b"pub fn other() {}\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&lib)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let c = checked(&u);
        assert_eq!(
            c.lines(),
            [
                "app is rebuilt: src/lib.rs changed but kept a date older than the last build, \
              which cargo doesn't see"
            ]
        );
    }

    // covers: B-23
    #[test]
    fn what_cargo_sees_itself_is_left_to_cargo() {
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        // Linux dates files from a clock that lags the real one by up to a tick: an edit made
        // right after the record's start can carry an older date, which the check takes, on the
        // safe side, for a change cargo doesn't see. Wait for a date that is surely newer.
        std::thread::sleep(Duration::from_millis(20));
        // An edit with a new date, and a file added: cargo builds the unit anyway.
        fs::write(u.src.join("lib.rs"), b"pub fn edited() {}\n").unwrap();
        fs::write(u.src.join("new.rs"), b"").unwrap();
        let c = checked(&u);
        assert!(c.rebuilt.is_empty(), "{c:?}");
        assert!(u.out.join("libapp-1.rlib").exists());
        // The same bytes under a new inode and an old date: settled by the digest.
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        let lib = u.src.join("lib.rs");
        let old = fs::metadata(&lib).unwrap().modified().unwrap();
        let copy = u.src.join("copy");
        fs::write(&copy, b"pub fn f() {}\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&copy)
            .unwrap()
            .set_modified(old)
            .unwrap();
        fs::rename(&copy, &lib).unwrap();
        let c = checked(&u);
        assert!(c.rebuilt.is_empty(), "{c:?}");
    }

    // covers: B-23
    #[test]
    fn a_record_that_no_longer_describes_the_outputs_is_removed_and_they_stay() {
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        // Plain cargo built the unit again since: a new file.
        let rlib = u.out.join("libapp-1.rlib");
        fs::remove_file(&rlib).unwrap();
        fs::write(&rlib, b"rlib built by plain cargo").unwrap();
        fs::write(u.src.join("notes.txt"), b"").unwrap();
        let c = checked(&u);
        assert_eq!((c.stale, c.rebuilt.len()), (1, 0), "{c:?}");
        assert!(rlib.exists());
        assert!(!u.record().exists());
        // And the next check finds nothing at all.
        assert_eq!(
            checked(&u),
            Checked {
                out_dirs: 1,
                ..Checked::default()
            }
        );
    }

    // covers: B-23
    #[test]
    fn a_truncated_or_unreadable_record_is_ignored() {
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        let full = fs::read(u.record()).unwrap();
        for bad in [&full[..full.len() / 2], b"", b"[]"] {
            fs::write(u.record(), bad).unwrap();
            fs::write(u.src.join("notes.txt"), b"").unwrap();
            let c = checked(&u);
            assert_eq!((c.units, c.rebuilt.len()), (0, 0), "{c:?}");
            assert!(u.out.join("libapp-1.rlib").exists());
        }
        let other = String::from_utf8(full)
            .unwrap()
            .replace(SCHEMA, "hushrun.build.unit/0");
        fs::write(u.record(), other).unwrap();
        assert!(checked(&u).rebuilt.is_empty());
    }

    // covers: B-23
    #[test]
    fn a_target_folder_a_build_holds_is_left_alone() {
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        fs::write(u.src.join("notes.txt"), b"").unwrap();
        let held = fs::File::open(u.out.parent().unwrap().join(CARGO_LOCK)).unwrap();
        held.lock().unwrap();
        // cargo's lock is per open file: another one, as another process would take it.
        let c = checked(&u);
        assert_eq!((c.busy, c.rebuilt.len()), (1, 0), "{c:?}");
        assert!(u.out.join("libapp-1.rlib").exists());
        held.unlock().unwrap();
        assert_eq!(checked(&u).rebuilt.len(), 1);
    }

    // covers: B-23
    #[test]
    fn a_check_and_a_compile_at_once_leave_a_whole_record_or_none() {
        let u = unit();
        fs::write(u.src.join("notes.txt"), b"").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let seen = u.seen();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..200 {
                    settle(&u.out, "app-1", Some(&seen));
                }
            });
            scope.spawn(|| {
                for _ in 0..50 {
                    let _ = checked(&u);
                }
            });
        });
        // Whatever the order, a record left is a whole one, and no temporary file is left.
        if u.record().exists() {
            assert!(read(&u.record()).is_some());
        }
        let left: Vec<_> = fs::read_dir(u.record().parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    // covers: B-23
    #[test]
    fn only_output_folders_of_this_folder_are_checked() {
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        fs::write(u.src.join("notes.txt"), b"").unwrap();
        let store = u.store();
        let elsewhere = tempfile::tempdir().unwrap();
        assert_eq!(check(&store, elsewhere.path(), None).out_dirs, 0);
        // Inside it (a package folder) and above it both count.
        assert_eq!(check(&store, &u.src, None).rebuilt.len(), 1);
        let u = unit();
        assert!(settle(&u.out, "app-1", Some(&u.seen())));
        fs::write(u.src.join("notes.txt"), b"").unwrap();
        let c = check(&u.store(), u.root().parent().unwrap(), None);
        assert_eq!(c.rebuilt.len(), 1, "{c:?}");
    }

    #[test]
    fn at_most_three_lines() {
        let rebuilt = |n: usize| Rebuilt {
            who: format!("c{n}"),
            cause: "x".into(),
        };
        let mut c = Checked {
            rebuilt: (0..3).map(rebuilt).collect(),
            ..Checked::default()
        };
        c.rebuilt.push(rebuilt(0));
        assert_eq!(c.lines().len(), 3);
        c.rebuilt = (0..5).map(rebuilt).collect();
        assert_eq!(
            c.lines(),
            [
                "c0 is rebuilt: x",
                "c1 is rebuilt: x",
                "3 more are rebuilt for changes cargo doesn't watch"
            ]
        );
    }
}
