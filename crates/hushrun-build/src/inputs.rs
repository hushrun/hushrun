//! What a compile read (B-6), gathered two ways: from a hint before the compile (B-7), and from
//! the compile's own dep-info after it (B-11). Both give the same [`Observed`] for the same files,
//! so a lookup finds the block an earlier compile of the same inputs stored.
//!
//! A path recorded under a root is looked up under this call's root, never under the one it was
//! recorded from (B-8). Every error here is a value: the wrapper turns it into a compile.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use hushrun_digest::cache::Stat;

use crate::depinfo::{self, DepInfo};
use crate::invocation::{Env, Invocation, NativeKind, SearchKind};
use crate::key::{
    Declared, EnvRead, ExternBy, ExternInput, FoundAbove, Generated, Link, LinkLibrary, NativeDir,
    Observed, Place, PlaceKind, Read, ReadList, ReadState, Shape, roots_digest,
};
use crate::known::{self, Where};
use crate::model::{Class, Located, NotStored, RootName};
use crate::roots::Roots;
use crate::script::{self, Script};
use crate::{link, memo, regular, shape, sidecar};

/// Every `--extern` as the key holds it, and each proc macro's crate name with whether another
/// machine names it the same way.
pub type Externs = (Vec<ExternInput>, Vec<(String, bool)>);

/// What every gathering needs: the call, its roots and variables, and the build script's
/// saved output when it has one.
pub struct Context<'a> {
    /// The call.
    pub invocation: &'a Invocation,
    /// Its roots.
    pub roots: &'a Roots,
    /// Its variables.
    pub env: &'a Env,
    /// The first key.
    pub step: Digest,
    /// What the build script printed, when the package has one (`OUT_DIR` is set).
    pub script: Option<&'a Script>,
    /// The run's session folder, where digests of files in folders the wrapper doesn't own are
    /// remembered for the run ([`crate::memo`]); `None` hashes them each time.
    pub run_memo: Option<&'a Path>,
}

/// One file hashed before the compile, with how it was then. After the compile it must be the
/// same file with no date at or after the compile's start, and no link on the way to it may be
/// newer than that either (B-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    /// The file.
    pub path: PathBuf,
    /// How it was when hashed; `None` when it was absent.
    pub stat: Option<Stat>,
    /// Whether `stat` is of what the path points at (the file was hashed through its links),
    /// or of the path itself (a symlink keyed by its text).
    pub follow: bool,
}

/// Whether any watched file moved (B-11): another file, another stat, a date at or after
/// `start`, or, for one hashed through its links, a link on the way to it that is. `links` is
/// the set of paths already looked at in this compile.
pub fn moved(watched: &[Watched], start: SystemTime, links: &mut BTreeSet<PathBuf>) -> bool {
    watched.iter().any(|watched| {
        let now = if watched.follow {
            fs::metadata(&watched.path)
        } else {
            fs::symlink_metadata(&watched.path)
        }
        .ok()
        .map(|m| Stat::of(&m));
        let file_moved = match (&watched.stat, &now) {
            (None, None) => false,
            (Some(then), Some(now)) => !same(then, now) || dated_after(now, start),
            _ => true,
        };
        file_moved || (watched.follow && links_changed(&watched.path, start, links))
    })
}

/// The most links followed on the way to one file before giving up, as the kernel's own limit.
const MAX_LINKS: u32 = 40;

/// Whether a symlink on the way to `path` carries a date at or after `start` (B-11): each
/// folder of the path and the path itself, and what each link points at, in turn, up to
/// 40 links (more counts as changed). A link made or retargeted during the compile
/// is new even when what it points at is old. A path that can't be stat'ed is left to the
/// hashing, which fails for it.
///
/// Cost: one `lstat` per part of the path not looked at before in this compile (`seen`), and a
/// `readlink` per link.
pub fn links_changed(path: &Path, start: SystemTime, seen: &mut BTreeSet<PathBuf>) -> bool {
    links_changed_from(path, start, seen, 0)
}

fn links_changed_from(
    path: &Path,
    start: SystemTime,
    seen: &mut BTreeSet<PathBuf>,
    depth: u32,
) -> bool {
    if depth > MAX_LINKS {
        return true;
    }
    let mut prefix = PathBuf::new();
    for part in path.components() {
        prefix.push(part);
        if !seen.insert(prefix.clone()) {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&prefix) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        if dated_after(&Stat::of(&meta), start) {
            return true;
        }
        let Ok(target) = fs::read_link(&prefix) else {
            return true;
        };
        let next = match prefix.parent() {
            Some(parent) => parent.join(&target),
            None => target,
        };
        if links_changed_from(&next, start, seen, depth + 1) {
            return true;
        }
    }
    false
}

/// The parts of the second key that don't come from the dep-info: externs, the build script's
/// output, what was declared and, for a linked output, the link. The same before and after the
/// compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Common {
    /// Every `--extern`.
    pub externs: Vec<ExternInput>,
    /// Each proc macro among the externs by crate name, and whether another machine names it the
    /// same way.
    pub externs_portable: Vec<(String, bool)>,
    /// Each place a library named with `-l` may be in the `-L` folders (B-6).
    pub native: Vec<Read>,
    /// For a library compile, every `-L native=`, `all=` and bare folder: its names and static
    /// libraries (B-6). Empty for a linked output, whose link line says what it takes.
    pub native_dirs: Vec<NativeDir>,
    /// Every file under `OUT_DIR`, when it is set.
    pub generated: Option<Vec<Generated>>,
    /// What the build script declared and the known macros read.
    pub declared: Option<Declared>,
    /// A declared read lies outside every root: the block is tied (B-8).
    pub outside: bool,
    /// Files hashed for these parts, checked again after the compile (B-11).
    pub watched: Vec<Watched>,
}

/// What a compile read, from its dep-info, with the facts B-10 and B-11 need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gathered {
    /// The second key's parts, with the place still `anywhere`: the caller decides it.
    pub observed: Observed,
    /// A read changed while compiling, or carries a date at or after its start (B-11).
    pub changed: bool,
    /// A read lies outside every root, the cargo home, the toolchain and the system folders.
    pub outside: bool,
}

/// How far back of the compile's start a file date still counts as "during" it when the file
/// system's own clock couldn't be read ([`floor`]). Linux stamps files from a clock that moves
/// once a tick (1 to 10 ms) and lags the real one; macOS stamps them exactly.
#[cfg(target_os = "linux")]
pub(crate) const CLOCK_SLACK: Duration = Duration::from_millis(20);
#[cfg(not(target_os = "linux"))]
pub(crate) const CLOCK_SLACK: Duration = Duration::ZERO;

/// The compile's start as file dates count it (B-11): the date the file system gave a file
/// created just before (`probe`), never after `now`; without one, `now` less the slack. A file
/// written after the probe is dated at or after it, since both dates come from one clock; one
/// written in an earlier tick is dated before it, even a moment before the compile.
pub fn floor(probe: Option<SystemTime>, now: SystemTime) -> SystemTime {
    match probe {
        Some(probe) => probe.min(now),
        None => now.checked_sub(CLOCK_SLACK).unwrap_or(now),
    }
}

fn ns(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_nanos()).unwrap_or(i64::MAX),
    }
}

/// Whether a stat carries a date at or after `start` (B-11), which is a [`floor`]: the
/// compile's start as file dates count it, not the real clock's.
pub fn dated_after(stat: &Stat, start: SystemTime) -> bool {
    let floor = ns(start);
    stat.mtime_ns >= floor || stat.ctime_ns >= floor
}

pub use crate::regular::{hash_steady, same};

/// Why hashing an input failed, as the outcome says it: a path that is not a regular file is
/// never opened (B-19), anything else is a failed hash.
fn hash_error(err: &io::Error) -> NotStored {
    if regular::is_not_regular(err) {
        NotStored::NotAFile
    } else {
        NotStored::Hash
    }
}

impl Context<'_> {
    /// A path of the call as an absolute path: a relative one starts from the working directory.
    pub fn absolute(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(self.invocation.cwd()).join(path)
        }
    }

    /// Where to open a recorded path in this call: under this call's root for a rooted one.
    /// `None` when this call has no such root.
    pub fn open_at(&self, at: &Located) -> Option<PathBuf> {
        match at {
            Located::Rooted(rooted) => self.roots.resolve(rooted),
            Located::Shared(path) | Located::Outside(path) => Some(PathBuf::from(path)),
        }
    }

    /// Where a path lies (B-8). A path with a `..` in it is resolved up to its last `..` and
    /// the rest kept as it is, so `a/src/../data/x.txt` (as rustc writes an
    /// `include_str!("../data/x.txt")`) is the file under the package it names, read through
    /// whatever is at that path now. A `..` after a linked folder can't be placed with
    /// certainty: [`NotStored::Unkeyable`].
    pub fn located_path(&self, path: &Path) -> Result<Located, NotStored> {
        self.located(path)
    }

    fn located(&self, path: &Path) -> Result<Located, NotStored> {
        let resolved;
        let path = if path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            resolved = resolve_dots(&Path::new(self.invocation.cwd()).join(path))?;
            resolved.as_path()
        } else {
            path
        };
        let text = path.to_str().ok_or(NotStored::Unkeyable)?;
        Ok(self.roots.locate(text))
    }

    /// A variable as the key holds it: its value now, with roots as placeholders, or unset.
    fn env_now(&self, name: &str) -> Result<EnvRead, NotStored> {
        let value = match self.env.get(name) {
            Some(value) => Some(
                value
                    .to_str()
                    .ok_or(NotStored::Unkeyable)
                    .map(|v| self.roots.to_placeholders(v))?,
            ),
            None => None,
        };
        Ok(EnvRead {
            name: name.to_string(),
            value,
        })
    }

    /// Every `--extern`, by content, or by what it was built from for a proc macro whose
    /// record names it (B-6); with, for each proc macro, whether another machine names it the
    /// same way (its record says so). A proc macro's record is written before cargo starts any
    /// dependent; a library's may not be yet (cargo starts a dependent once the `.rmeta` is
    /// written), and it is keyed by its bytes anyway.
    pub fn externs(&self) -> Result<Externs, NotStored> {
        let mut out = Vec::with_capacity(self.invocation.externs().len());
        let mut portable = Vec::with_capacity(self.invocation.externs().len());
        for ext in self.invocation.externs() {
            let by = match &ext.path {
                None => ExternBy::Builtin,
                Some(path) => {
                    let path = self.absolute(path);
                    let known = sidecar::lookup(&path).map_err(|_| NotStored::Hash)?;
                    if is_linked_library(&path) {
                        portable.push((ext.name.clone(), known.portable));
                    }
                    match known.produced_by {
                        Some(origin) if is_linked_library(&path) => ExternBy::ProducedBy(origin),
                        _ => ExternBy::Content(known.sha256),
                    }
                }
            };
            out.push(ExternInput {
                name: ext.name.clone(),
                options: ext.options.clone(),
                by,
            });
        }
        Ok((out, portable))
    }

    /// The parts that don't depend on the dep-info. Hashed now, checked again after the compile.
    pub fn common(&self) -> Result<Common, NotStored> {
        let mut watched = Vec::new();
        let generated = match &self.invocation.package().out_dir {
            Some(out_dir) => Some(self.generated(&self.absolute(out_dir), &mut watched)?),
            None => None,
        };
        let mut outside = false;
        let declared = self.declared(&mut watched, &mut outside)?;
        let native = self.native(generated.is_some(), &mut watched, &mut outside)?;
        // Only a library bundles what it finds there unseen. A linked output's link line names
        // every library it takes, and `link` keys those (B-6, B-18).
        let native_dirs = if self.invocation.class() == Class::Library {
            self.native_dirs(generated.is_some(), &mut watched)?
        } else {
            Vec::new()
        };
        let (externs, externs_portable) = self.externs()?;
        Ok(Common {
            externs,
            externs_portable,
            native,
            native_dirs,
            generated,
            declared,
            outside,
            watched,
        })
    }

    fn generated(
        &self,
        out_dir: &Path,
        watched: &mut Vec<Watched>,
    ) -> Result<Vec<Generated>, NotStored> {
        let files = script::tree(out_dir).map_err(|err| match err {
            script::Error::Special { .. } => NotStored::NotAFile,
            _ => NotStored::Hash,
        })?;
        let mut out = Vec::with_capacity(files.len());
        for file in files {
            let path = out_dir.join(&file.rest);
            watched.push(Watched {
                stat: fs::symlink_metadata(&path).ok().map(|m| Stat::of(&m)),
                path,
                follow: false,
            });
            out.push(Generated {
                // A symlink is its target's text; this keeps it apart from a file of those bytes.
                sha256: if file.symlink {
                    Digest::of(format!("symlink\0{}", file.sha256.hex()).as_bytes())
                } else {
                    file.sha256
                },
                rest: file.rest,
            });
        }
        Ok(out)
    }

    /// B-6's `declared`: for a package that is not under a shared folder, what its build script
    /// declared (or, when it declared nothing, the whole package folder: cargo's rule); and for
    /// any package, the inputs of the known macros among its externs.
    fn declared(
        &self,
        watched: &mut Vec<Watched>,
        outside: &mut bool,
    ) -> Result<Option<Declared>, NotStored> {
        let names: Vec<&str> = self
            .invocation
            .externs()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        let known = known::inputs(&names);
        let package = self.roots.given(RootName::Package).map(PathBuf::from);
        let local_script = self.script.filter(|_| package.is_some());
        if local_script.is_none() && known == known::Known::default() {
            return Ok(None);
        }
        let mut files: Vec<PathBuf> = Vec::new();
        let mut folders: Vec<PathBuf> = Vec::new();
        let mut env: Vec<String> = Vec::new();
        if let (Some(script), Some(package)) = (local_script, &package) {
            if script.declared_anything() {
                for declared in &script.rerun_if_changed {
                    let path = package.join(declared);
                    if path.is_dir() {
                        folders.push(path);
                    } else {
                        files.push(path);
                    }
                }
                env.extend(script.rerun_if_env_changed.iter().cloned());
            } else {
                folders.push(package.clone());
            }
        }
        let manifest = self
            .invocation
            .package()
            .manifest_dir
            .as_deref()
            .map(|dir| self.absolute(dir));
        let cwd = PathBuf::from(self.invocation.cwd());
        let mut above = Vec::new();
        for place in &known.files {
            match place {
                Where::Upward(name) => {
                    self.upward(&cwd, name, &mut files, &mut above, watched)?;
                }
                Where::UpwardFromPackage(name) => {
                    if let Some(package) = &manifest {
                        self.upward(package, name, &mut files, &mut above, watched)?;
                    }
                }
                Where::Package(_) | Where::Workspace(_) => {
                    files.extend(places(place, manifest.as_deref(), &cwd));
                }
            }
        }
        for place in &known.folders {
            for path in places(place, manifest.as_deref(), &cwd) {
                if path.is_dir() {
                    folders.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        env.extend(known.env.iter().map(|name| name.to_string()));

        let skip = self.never_inputs();
        let mut seen = BTreeSet::new();
        let mut reads = Vec::new();
        for folder in folders {
            for file in walk(&folder, &skip).map_err(|_| NotStored::Hash)? {
                files.push(file);
            }
        }
        for path in files {
            let at = self.located(&path)?;
            if !seen.insert(at.clone()) {
                continue;
            }
            *outside |= matches!(at, Located::Outside(_));
            let (state, stat) = match fs::symlink_metadata(&path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => (ReadState::Absent, None),
                Err(_) => return Err(NotStored::Hash),
                Ok(_) => {
                    let (sha, stat) = hash_steady(&path).map_err(|err| hash_error(&err))?;
                    (ReadState::Present(sha), Some(stat))
                }
            };
            watched.push(Watched {
                path,
                stat,
                follow: true,
            });
            reads.push(Read { at, state });
        }
        env.sort();
        env.dedup();
        let env = env
            .iter()
            .map(|name| self.env_now(name))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Declared {
            files: reads,
            env,
            above,
        }))
    }

    /// An upward search for the file `name` from the folder `from` (B-6 `declared`), as a macro
    /// that reads the first one it finds walking up does it. Each place under a root or a base
    /// goes into `files`, read where it lies as any other; so does each place under a shared
    /// folder when the search starts there. From the first folder above them all, one entry goes
    /// into `above`: the first file of that name found walking on up from there, by its bytes,
    /// or none. A shared folder above a root or a base (`/opt` above a cargo home in
    /// `/opt/tools/cargo`) is the machine's own layout, like the home folder above `~/.cargo`,
    /// and is in that entry too, never read where it lies. The entry holds no folder outside the
    /// roots, so it doesn't tie the block (B-8), and it is computed from this call's own folders,
    /// so it never opens another checkout's path. Every place it looked at is watched, so a file
    /// that appears, goes or changes there while compiling is caught (B-11).
    ///
    /// Cost: one `stat` per folder above the roots, up to the first file found.
    fn upward(
        &self,
        from: &Path,
        name: &str,
        files: &mut Vec<PathBuf>,
        above: &mut Vec<FoundAbove>,
        watched: &mut Vec<Watched>,
    ) -> Result<(), NotStored> {
        let mut below = None;
        let mut rooted = false;
        for dir in from.ancestors() {
            let candidate = dir.join(name);
            let at = self.located(&candidate)?;
            let read_here = match at {
                Located::Rooted(_) => true,
                Located::Shared(_) => !rooted,
                Located::Outside(_) => false,
            };
            if read_here {
                rooted |= matches!(at, Located::Rooted(_));
                files.push(candidate);
                below = Some(self.located(dir)?);
                continue;
            }
            let Some(below) = below else {
                // The search starts outside every root: each place is a read where it lies,
                // which ties the block.
                files.extend(from.ancestors().map(|dir| dir.join(name)));
                return Ok(());
            };
            above.push(FoundAbove {
                name: name.to_string(),
                below,
                state: first_found(dir, name, watched)?,
            });
            return Ok(());
        }
        Ok(())
    }

    /// B-6's `native`: for each `-l`, every name its file may have in every folder rustc and
    /// the linker search for it (`-L native=`, `all=` and bare; `framework=` for a framework),
    /// in order, present (by content, through the record where one exists) or absent. A file
    /// under the build script's output is in `generated` already. A name found in none of them
    /// is a system library: the SDK and the linker cover it.
    ///
    /// Cost: one stat per candidate, six names per folder, only for calls that have `-l`.
    fn native(
        &self,
        generated: bool,
        watched: &mut Vec<Watched>,
        outside: &mut bool,
    ) -> Result<Vec<Read>, NotStored> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for lib in self.invocation.native_libs() {
            let files = match lib.kind {
                NativeKind::Framework => link::framework_files(&lib.name),
                NativeKind::Static | NativeKind::Dylib => link::library_files(&lib.name),
            };
            for dir in self.invocation.native_dirs(lib.kind) {
                let dir = self.absolute(dir);
                let mut found = false;
                for file in &files {
                    let path = dir.join(file);
                    let (read, stat, is_outside) = self.place(&path)?;
                    found |= read.state != ReadState::Absent;
                    let covered = generated
                        && matches!(&read.at, Located::Rooted(r) if r.root == RootName::ScriptOut);
                    if covered || !seen.insert(read.at.clone()) {
                        continue;
                    }
                    *outside |= is_outside;
                    watched.push(Watched {
                        path,
                        stat,
                        follow: true,
                    });
                    out.push(read);
                }
                // A framework folder with neither its binary nor its stub: its file can't be
                // named with certainty.
                if lib.kind == NativeKind::Framework
                    && !found
                    && dir.join(format!("{}.framework", lib.name)).exists()
                {
                    return Err(NotStored::NativeOutside);
                }
            }
        }
        Ok(out)
    }

    /// B-6's `native_dirs`: each `-L native=`, `all=` and bare folder, by where it lies, the
    /// names of its entries, and the bytes of each static library directly in it (`*.a`,
    /// `*.lib`), which a `#[link(kind = "static")]` in the source would bundle. These are
    /// usually other packages' build-script outputs: each is a root of its own (B-8) and no
    /// record is written there; digests are remembered for the run instead ([`crate::memo`]).
    ///
    /// Cost: one `readdir` per folder, and per static library one `stat` and a small read once
    /// it was hashed in this run.
    fn native_dirs(
        &self,
        generated: bool,
        watched: &mut Vec<Watched>,
    ) -> Result<Vec<NativeDir>, NotStored> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for search in self.invocation.search_paths() {
            if !matches!(search.kind, SearchKind::Native | SearchKind::All) {
                continue;
            }
            let dir = self.absolute(&search.path);
            let at = self.located(&dir)?;
            // The package's own output is in `generated` already, names and bytes.
            let covered =
                generated && matches!(&at, Located::Rooted(r) if r.root == RootName::ScriptOut);
            if covered || !seen.insert(at.clone()) {
                continue;
            }
            let names = match shape::names(&dir) {
                Ok(names) => Some(names),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(_) => return Err(NotStored::Hash),
            };
            watched.push(Watched {
                stat: fs::metadata(&dir).ok().map(|m| Stat::of(&m)),
                path: dir.clone(),
                follow: true,
            });
            let mut archives = Vec::new();
            for name in names.iter().flatten() {
                if !(name.ends_with(".a") || name.ends_with(".lib")) {
                    continue;
                }
                let path = dir.join(name);
                if fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
                    continue;
                }
                let (sha, stat) =
                    memo::digest(self.run_memo, &path).map_err(|err| hash_error(&err))?;
                watched.push(Watched {
                    path,
                    stat: Some(stat),
                    follow: true,
                });
                archives.push((name.clone(), sha));
            }
            out.push(NativeDir {
                dir: at,
                names,
                archives,
            });
        }
        Ok(out)
    }

    /// A file's SHA-256: through its record where it lies in a folder the wrapper writes
    /// records in (an output folder), else remembered for the run ([`crate::memo`]).
    pub fn digest(&self, path: &Path) -> io::Result<Digest> {
        if in_record_folder(path) {
            return sidecar::lookup(path).map(|known| known.sha256);
        }
        memo::digest(self.run_memo, path).map(|(sha, _)| sha)
    }

    /// One place a file may be, as the key holds it: where it lies, and its content (through
    /// the record where one exists) or absent; with the stat of what it points at for B-11 and
    /// whether it lies outside every root (B-8).
    pub fn place(&self, path: &Path) -> Result<(Read, Option<Stat>, bool), NotStored> {
        let at = self.located(path)?;
        let outside = matches!(at, Located::Outside(_));
        let (state, stat) = match fs::symlink_metadata(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => (ReadState::Absent, None),
            Err(_) => return Err(NotStored::Hash),
            Ok(_) => {
                let sha = self.digest(path).map_err(|err| hash_error(&err))?;
                let meta = fs::metadata(path).map_err(|err| hash_error(&err))?;
                (ReadState::Present(sha), Some(Stat::of(&meta)))
            }
        };
        Ok((Read { at, state }, stat, outside))
    }

    /// The state of a recorded place as it is now, for a hint, with the stat of what it points
    /// at: `None` when it can't be read.
    fn state_now(&self, at: &Located, watched: &mut Vec<Watched>) -> Option<ReadState> {
        let path = self.open_at(at)?;
        let state = match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => ReadState::Absent,
            Err(_) => return None,
            Ok(_) => ReadState::Present(self.digest(&path).ok()?),
        };
        let stat = match state {
            ReadState::Absent => None,
            ReadState::Present(_) => Some(Stat::of(&fs::metadata(&path).ok()?)),
        };
        watched.push(Watched {
            path,
            stat,
            follow: true,
        });
        Some(state)
    }

    /// Folders never walked as inputs: the target folders this call writes to (they hold the
    /// outputs), and `.git`.
    fn never_inputs(&self) -> Vec<PathBuf> {
        let mut skip = vec![self.absolute(self.invocation.out_dir())];
        if let Some(out) = &self.invocation.package().out_dir {
            skip.push(self.absolute(out));
        }
        skip
    }

    /// The second key's parts from a hint, with every file as it is now (B-7), and the stats
    /// they were hashed at, which the compile that may follow checks again (B-11). `None` when
    /// the hint can't be followed here: a tied block's hint recorded with other roots (checked
    /// before anything is opened, B-8), a root this call doesn't have, a folder gone, a value
    /// that can't be keyed, a file that is not a regular file. That hint is skipped; it is
    /// never an error.
    pub fn from_hint(&self, hint: &ReadList, common: &Common) -> Option<(Observed, Vec<Watched>)> {
        if hint.place == PlaceKind::Tied && hint.roots != Some(roots_digest(&self.roots.tied())) {
            return None;
        }
        let mut watched = Vec::with_capacity(hint.reads.len());
        let mut reads = Vec::with_capacity(hint.reads.len());
        for at in &hint.reads {
            let path = self.open_at(at)?;
            let (state, stat) = match fs::metadata(&path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => (ReadState::Absent, None),
                Err(_) => return None,
                Ok(_) => {
                    let (sha, stat) = hash_steady(&path).ok()?;
                    (ReadState::Present(sha), Some(stat))
                }
            };
            watched.push(Watched {
                path,
                stat,
                follow: true,
            });
            reads.push(Read {
                at: at.clone(),
                state,
            });
        }
        let env = hint
            .env
            .iter()
            .map(|name| self.env_now(name).ok())
            .collect::<Option<Vec<_>>>()?;
        let mut shapes = Vec::with_capacity(hint.shape.len());
        for dir in &hint.shape {
            let names = shape::names(&self.open_at(dir)?).ok()?;
            shapes.push(Shape {
                dir: dir.clone(),
                names,
            });
        }
        if hint.generated != common.generated.is_some() {
            return None;
        }
        let link = match &hint.link {
            None => None,
            Some(list) => Some(self.link_from_hint(list, &mut watched)?),
        };
        let observed = Observed {
            step: self.step,
            reads,
            env,
            externs: common.externs.clone(),
            native: common.native.clone(),
            native_dirs: common.native_dirs.clone(),
            shape: shapes,
            generated: common.generated.clone(),
            declared: common.declared.clone(),
            link,
            place: hint.place.place(self.roots.tied()),
        };
        Some((observed, watched))
    }

    fn link_from_hint(
        &self,
        list: &crate::key::LinkList,
        watched: &mut Vec<Watched>,
    ) -> Option<Link> {
        let mut libraries = Vec::with_capacity(list.libraries.len());
        for at in &list.libraries {
            let path = self.open_at(at)?;
            let sha256 = self.digest(&path).ok()?;
            watched.push(Watched {
                stat: Some(Stat::of(&fs::metadata(&path).ok()?)),
                path,
                follow: true,
            });
            libraries.push(LinkLibrary {
                at: at.clone(),
                sha256,
            });
        }
        let mut files = Vec::with_capacity(list.files.len());
        for at in &list.files {
            files.push(Read {
                at: at.clone(),
                state: self.state_now(at, watched)?,
            });
        }
        let linker = self.open_at(&list.linker)?;
        let ld = match &list.ld {
            Some(at) => Some((at, self.open_at(at)?)),
            None => None,
        };
        let sdk = match &list.sdk {
            Some(at) => Some((at, self.open_at(at)?)),
            None => None,
        };
        let dsymutil = match &list.dsymutil {
            Some(at) => Some((at, self.open_at(at)?)),
            None => None,
        };
        Some(Link {
            libraries,
            files,
            linker: linkers_text(
                (&list.linker, &linker),
                ld.as_ref().map(|(at, p)| (*at, p.as_path())),
                dsymutil.as_ref().map(|(at, p)| (*at, p.as_path())),
            )?,
            sdk: sdk_text(sdk.as_ref().map(|(at, p)| (*at, p.as_path()))),
            linker_path: list.linker.clone(),
            ld_path: list.ld.clone(),
            sdk_path: list.sdk.clone(),
            dsymutil_path: list.dsymutil.clone(),
        })
    }

    /// The second key's parts from the compile's dep-info, each read hashed now and checked
    /// against the compile's start (B-6, B-11).
    pub fn after(
        &self,
        info: &DepInfo,
        common: &Common,
        start: SystemTime,
    ) -> Result<Gathered, NotStored> {
        let mut changed = false;
        let mut links = BTreeSet::new();
        let mut outside = common.outside;
        let mut reads = Vec::with_capacity(info.files.len() * 2);
        let mut seen = BTreeSet::new();
        let mut folders = BTreeSet::new();
        for file in &info.files {
            let path = self.absolute(file);
            let at = self.located(&path)?;
            outside |= matches!(at, Located::Outside(_));
            if !seen.insert(at.clone()) {
                continue;
            }
            match hash_steady(&path) {
                Ok((sha, stat)) => {
                    changed |= dated_after(&stat, start) || links_changed(&path, start, &mut links);
                    reads.push(Read {
                        at,
                        state: ReadState::Present(sha),
                    });
                }
                Err(err) if regular::is_not_regular(&err) => return Err(NotStored::NotAFile),
                Err(_) => {
                    // Gone or changing since rustc read it: what it read can't be recorded.
                    changed = true;
                }
            }
            if let Some(parent) = path.parent() {
                folders.insert(parent.to_path_buf());
            }
        }
        // The other place each module could have been (B-6): absent, or hashed if a file is
        // there that rustc didn't read.
        for file in &info.files {
            let Some(candidate) = depinfo::module_candidate(file) else {
                continue;
            };
            let path = self.absolute(&candidate);
            let at = self.located(&path)?;
            if !seen.insert(at.clone()) {
                continue;
            }
            outside |= matches!(at, Located::Outside(_));
            let state = match fs::symlink_metadata(&path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => ReadState::Absent,
                Err(_) => return Err(NotStored::Hash),
                Ok(_) => {
                    let (sha, stat) = hash_steady(&path).map_err(|err| hash_error(&err))?;
                    changed |= dated_after(&stat, start) || links_changed(&path, start, &mut links);
                    ReadState::Present(sha)
                }
            };
            reads.push(Read { at, state });
        }
        let env = info
            .env
            .iter()
            .map(|dep| EnvRead {
                name: dep.name.clone(),
                value: dep.value.as_deref().map(|v| self.roots.to_placeholders(v)),
            })
            .collect();
        let mut shapes = Vec::with_capacity(folders.len());
        for folder in folders {
            let dir = self.located(&folder)?;
            let names = shape::names(&folder).map_err(|_| NotStored::Hash)?;
            shapes.push(Shape { dir, names });
        }
        changed |= moved(&common.watched, start, &mut links);
        Ok(Gathered {
            observed: Observed {
                step: self.step,
                reads,
                env,
                externs: common.externs.clone(),
                native: common.native.clone(),
                native_dirs: common.native_dirs.clone(),
                shape: shapes,
                generated: common.generated.clone(),
                declared: common.declared.clone(),
                link: None,
                place: Place::Anywhere,
            },
            changed,
            outside,
        })
    }
}

/// An absolute `path` with its `..` parts gone (B-8): the part up to and including its last
/// `..` is resolved on disk, and the parts after it are appended as they are, so a symlink at
/// the end stays the path the source named and is hashed through whatever it points at now.
///
/// That resolved part must be what removing the `..` without the file system gives, once
/// resolved too: a linked folder before a `..` makes it something else (`a/src/..` is not `a`
/// when `a/src` is a link), and such a read can't be placed under a root with certainty. That,
/// and a part that can't be resolved, is [`NotStored::Unkeyable`].
///
/// Cost: two `realpath` calls, only for a path that has a `..`.
fn resolve_dots(path: &Path) -> Result<PathBuf, NotStored> {
    use std::path::Component;
    let parts: Vec<Component<'_>> = path.components().collect();
    let Some(last) = parts.iter().rposition(|c| *c == Component::ParentDir) else {
        return Ok(path.to_path_buf());
    };
    let head: PathBuf = parts[..=last].iter().collect();
    let mut lexical = PathBuf::new();
    for part in &parts[..=last] {
        match part {
            Component::ParentDir => {
                if !lexical.pop() || lexical.as_os_str().is_empty() {
                    return Err(NotStored::Unkeyable);
                }
            }
            Component::CurDir => {}
            other => lexical.push(other),
        }
    }
    let resolved = fs::canonicalize(&head).map_err(|_| NotStored::Unkeyable)?;
    let expected = fs::canonicalize(&lexical).map_err(|_| NotStored::Unkeyable)?;
    if resolved != expected {
        return Err(NotStored::Unkeyable);
    }
    let mut out = lexical;
    for part in &parts[last + 1..] {
        if *part != Component::CurDir {
            out.push(part);
        }
    }
    Ok(out)
}

/// Whether a file is a linked library a proc macro can be: never an `.rlib` or `.rmeta`.
pub(crate) fn is_linked_library(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("dylib" | "so" | "dll")
    )
}

/// The first regular file named `name` in `dir` or a folder above it, by SHA-256, or absent:
/// what `dotenvy`'s search finds. A folder or other entry of that name is passed over, as that
/// search passes over it. Each place looked at is added to `watched` as it was (B-11).
fn first_found(dir: &Path, name: &str, watched: &mut Vec<Watched>) -> Result<ReadState, NotStored> {
    for folder in dir.ancestors() {
        let path = folder.join(name);
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => {
                let (sha, stat) = hash_steady(&path).map_err(|err| hash_error(&err))?;
                watched.push(Watched {
                    path,
                    stat: Some(stat),
                    follow: true,
                });
                return Ok(ReadState::Present(sha));
            }
            Ok(meta) => watched.push(Watched {
                path,
                stat: Some(Stat::of(&meta)),
                follow: true,
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => watched.push(Watched {
                path,
                stat: None,
                follow: true,
            }),
            Err(_) => return Err(NotStored::Hash),
        }
    }
    Ok(ReadState::Absent)
}

/// The paths a known macro's place names, in order. A search upward names the file in every
/// folder from the start to the file system's root.
pub(crate) fn places(place: &Where, package: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
    let upward = |from: &Path, name: &str| -> Vec<PathBuf> {
        from.ancestors().map(|dir| dir.join(name)).collect()
    };
    match place {
        Where::Package(rest) => package.map(|p| vec![p.join(rest)]).unwrap_or_default(),
        Where::Workspace(rest) => vec![cwd.join(rest)],
        Where::Upward(name) => upward(cwd, name),
        Where::UpwardFromPackage(name) => package.map(|p| upward(p, name)).unwrap_or_default(),
    }
}

/// Every file under `dir`, sorted, skipping `.git`, folders named `target` and any folder in
/// `skip` or holding one of them: cargo leaves those out of a package's files too, and they
/// hold this build's own outputs. A symlink is listed, not followed. A missing `dir` gives
/// nothing.
pub(crate) fn walk(dir: &Path, skip: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(folder) = stack.pop() {
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound && folder == dir => {
                return Ok(out);
            }
            Err(err) => return Err(err),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                let name = entry.file_name();
                if name == ".git" || name == "target" || skip.iter().any(|s| s.starts_with(&path)) {
                    continue;
                }
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Whether `path` lies in a folder the wrapper writes records in: an output folder.
fn in_record_folder(path: &Path) -> bool {
    path.parent()
        .is_some_and(|dir| dir.join(sidecar::DIR).is_dir())
}

/// Where a place lies, as one text for a key.
fn located_text(at: &Located) -> Option<String> {
    serde_json::to_string(at).ok()
}

/// The linker's identity as one text (B-18): where it lies (B-8) and, for one under a root of
/// the checkout (a script in the repository, another file in each checkout), its SHA-256; for
/// any other, the toolchain's `rust-lld` among them, its size and modification time. `None`
/// when it can't be read.
pub fn linker_text(at: &Located, path: &Path) -> Option<String> {
    let place = located_text(at)?;
    if matches!(at, Located::Rooted(rooted) if !rooted.root.is_base()) {
        let (sha, _) = regular::hash_steady(path).ok()?;
        return Some(format!("{place} {}", sha.hex()));
    }
    let meta = fs::metadata(path).ok()?;
    let stat = Stat::of(&meta);
    Some(format!("{place} {} {}", stat.size, stat.mtime_ns))
}

/// The identity of the linkers a link ran: the driver's ([`linker_text`]) and, on a line of
/// its own, the one `-fuse-ld` named; then, on a line that says what it is, the `dsymutil` that
/// wrote a program's debug bundle (B-9). `None` when one can't be read.
pub fn linkers_text(
    driver: (&Located, &Path),
    ld: Option<(&Located, &Path)>,
    dsymutil: Option<(&Located, &Path)>,
) -> Option<String> {
    let mut text = linker_text(driver.0, driver.1)?;
    if let Some((at, path)) = ld {
        text.push('\n');
        text.push_str(&linker_text(at, path)?);
    }
    if let Some((at, path)) = dsymutil {
        text.push_str("\ndsymutil ");
        text.push_str(&linker_text(at, path)?);
    }
    Some(text)
}

/// The SDK as one text: where it lies and the modification time of its settings file, which an
/// update of the SDK in place changes (B-18).
pub fn sdk_text(sdk: Option<(&Located, &Path)>) -> String {
    match sdk {
        None => String::new(),
        Some((at, path)) => {
            let settings = path.join("SDKSettings.json");
            let stamp = fs::metadata(settings)
                .map(|m| Stat::of(&m).mtime_ns.to_string())
                .unwrap_or_default();
            format!("{} {stamp}", located_text(at).unwrap_or_default())
        }
    }
}
