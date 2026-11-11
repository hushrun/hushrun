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
use crate::invocation::{Env, Invocation};
use crate::key::{
    Declared, EnvRead, ExternBy, ExternInput, Generated, Link, LinkLibrary, Observed, Place, Read,
    ReadList, ReadState, Shape,
};
use crate::known::{self, Where};
use crate::model::{Located, NotStored, RootName};
use crate::roots::Roots;
use crate::script::{self, Script};
use crate::{shape, sidecar};

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
}

/// One file hashed before the compile, with how it was then. After the compile it must be the
/// same file with no date at or after the compile's start (B-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    /// The file.
    pub path: PathBuf,
    /// How it was when hashed; `None` when it was absent.
    pub stat: Option<Stat>,
}

/// The parts of the second key that don't come from the dep-info: externs, the build script's
/// output, what was declared and, for a linked output, the link. The same before and after the
/// compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Common {
    /// Every `--extern`.
    pub externs: Vec<ExternInput>,
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

/// How far back of the compile's start a file date still counts as "during" it. Linux stamps
/// files from a clock that may lag the real one by a tick; macOS stamps them exactly.
#[cfg(target_os = "linux")]
const CLOCK_SLACK: Duration = Duration::from_millis(20);
#[cfg(not(target_os = "linux"))]
const CLOCK_SLACK: Duration = Duration::ZERO;

fn ns(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_nanos()).unwrap_or(i64::MAX),
    }
}

/// Whether a stat carries a date at or after `start` (B-11).
pub fn dated_after(stat: &Stat, start: SystemTime) -> bool {
    let floor = ns(start.checked_sub(CLOCK_SLACK).unwrap_or(start));
    stat.mtime_ns >= floor || stat.ctime_ns >= floor
}

/// Whether two stats are the same file, unchanged: size, both dates, inode and device (B-11).
pub fn same(a: &Stat, b: &Stat) -> bool {
    a.size == b.size
        && a.mtime_ns == b.mtime_ns
        && a.ctime_ns == b.ctime_ns
        && a.inode == b.inode
        && a.device == b.device
}

/// The SHA-256 of a file, stat'ed before and after: an error when it changed while it was read,
/// so the hash describes one version of it.
pub fn hash_steady(path: &Path) -> io::Result<(Digest, Stat)> {
    let before = Stat::of(&fs::metadata(path)?);
    let (sha, _) = hushrun_digest::hash::hash_file(path)?;
    let after = Stat::of(&fs::metadata(path)?);
    if !same(&before, &after) {
        return Err(io::Error::other(format!(
            "{} changed while it was hashed",
            path.display()
        )));
    }
    Ok((Digest::from_bytes(sha), after))
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

    /// Where a path lies (B-8). A path with a `..` in it is first resolved on disk, so
    /// `a/src/../data/x.txt` (as rustc writes an `include_str!("../data/x.txt")`) is the file
    /// under the package it names, and a `..` that leaves through a symlink lands where the link
    /// points.
    pub fn located_path(&self, path: &Path) -> Result<Located, NotStored> {
        self.located(path)
    }

    fn located(&self, path: &Path) -> Result<Located, NotStored> {
        let resolved;
        let path = if path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            resolved = resolve_dots(path);
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

    /// Every `--extern`, by content, or by the block that produced it for a proc macro whose
    /// record names one (B-6).
    pub fn externs(&self) -> Result<Vec<ExternInput>, NotStored> {
        let mut out = Vec::with_capacity(self.invocation.externs().len());
        for ext in self.invocation.externs() {
            let by = match &ext.path {
                None => ExternBy::Builtin,
                Some(path) => {
                    let path = self.absolute(path);
                    let known = sidecar::lookup(&path).map_err(|_| NotStored::Hash)?;
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
        Ok(out)
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
        Ok(Common {
            externs: self.externs()?,
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
        let files = script::tree(out_dir).map_err(|_| NotStored::Hash)?;
        let mut out = Vec::with_capacity(files.len());
        for file in files {
            let path = out_dir.join(&file.rest);
            watched.push(Watched {
                stat: fs::symlink_metadata(&path).ok().map(|m| Stat::of(&m)),
                path,
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
        for place in &known.files {
            files.extend(places(place, manifest.as_deref(), &cwd));
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
                    let (sha, stat) = hash_steady(&path).map_err(|_| NotStored::Hash)?;
                    (ReadState::Present(sha), Some(stat))
                }
            };
            watched.push(Watched { path, stat });
            reads.push(Read { at, state });
        }
        env.sort();
        env.dedup();
        let env = env
            .iter()
            .map(|name| self.env_now(name))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Declared { files: reads, env }))
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

    /// The second key's parts from a hint, with every file as it is now (B-7). `None` when the
    /// hint can't be followed here: a root this call doesn't have, a folder gone, a value that
    /// can't be keyed. That hint is skipped; it is never an error.
    pub fn from_hint(&self, hint: &ReadList, common: &Common) -> Option<Observed> {
        let mut reads = Vec::with_capacity(hint.reads.len());
        for at in &hint.reads {
            let path = self.open_at(at)?;
            let state = match fs::metadata(&path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => ReadState::Absent,
                Err(_) => return None,
                Ok(_) => ReadState::Present(hash_steady(&path).ok()?.0),
            };
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
            Some(list) => Some(self.link_from_hint(list)?),
        };
        Some(Observed {
            step: self.step,
            reads,
            env,
            externs: common.externs.clone(),
            shape: shapes,
            generated: common.generated.clone(),
            declared: common.declared.clone(),
            link,
            place: hint.place.place(self.roots.tied()),
        })
    }

    fn link_from_hint(&self, list: &crate::key::LinkList) -> Option<Link> {
        let mut libraries = Vec::with_capacity(list.libraries.len());
        for at in &list.libraries {
            let path = self.open_at(at)?;
            libraries.push(LinkLibrary {
                at: at.clone(),
                sha256: library_digest(&path).ok()?,
            });
        }
        Some(Link {
            libraries,
            linker: linker_text(Path::new(&list.linker))?,
            sdk: sdk_text(list.sdk.as_deref()),
            linker_path: list.linker.clone(),
            sdk_path: list.sdk.clone(),
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
                    changed |= dated_after(&stat, start);
                    reads.push(Read {
                        at,
                        state: ReadState::Present(sha),
                    });
                }
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
                    let (sha, stat) = hash_steady(&path).map_err(|_| NotStored::Hash)?;
                    changed |= dated_after(&stat, start);
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
        for watched in &common.watched {
            let now = fs::symlink_metadata(&watched.path)
                .ok()
                .map(|m| Stat::of(&m));
            changed |= match (&watched.stat, &now) {
                (None, None) => false,
                (Some(then), Some(now)) => !same(then, now) || dated_after(now, start),
                _ => true,
            };
        }
        Ok(Gathered {
            observed: Observed {
                step: self.step,
                reads,
                env,
                externs: common.externs.clone(),
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

/// `path` with its `..` parts resolved on disk: the longest part of it that exists is resolved
/// (symlinks followed), and the rest joined to it. A path where nothing can be resolved is kept,
/// and its `..` makes it lie outside every root.
fn resolve_dots(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut head = path;
    loop {
        if let Ok(real) = fs::canonicalize(head) {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            if out
                .components()
                .any(|c| c == std::path::Component::ParentDir)
            {
                return path.to_path_buf();
            }
            return out;
        }
        match (head.parent(), head.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                head = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Whether a file is a linked library a proc macro can be: never an `.rlib` or `.rmeta`.
fn is_linked_library(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("dylib" | "so" | "dll")
    )
}

/// The paths a known macro's place names, in order. A search upward names the file in every
/// folder from the start to the file system's root.
fn places(place: &Where, package: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
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
fn walk(dir: &Path, skip: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
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

/// A library handed to the linker, by content: through its record when it lies in a folder
/// the wrapper writes records in, else read.
pub fn library_digest(path: &Path) -> io::Result<Digest> {
    let in_target = path
        .parent()
        .is_some_and(|dir| dir.join(sidecar::DIR).is_dir());
    if in_target {
        return sidecar::lookup(path).map(|known| known.sha256);
    }
    hash_steady(path).map(|(sha, _)| sha)
}

/// The linker's identity as one text: its path, size and modification time (B-18). `None`
/// when it can't be read.
pub fn linker_text(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let stat = Stat::of(&meta);
    Some(format!(
        "{} {} {}",
        path.to_str()?,
        stat.size,
        stat.mtime_ns
    ))
}

/// The SDK as one text: its path and the modification time of its settings file, which an
/// update of the SDK in place changes (B-18).
pub fn sdk_text(sdk: Option<&str>) -> String {
    match sdk {
        None => String::new(),
        Some(path) => {
            let settings = Path::new(path).join("SDKSettings.json");
            let stamp = fs::metadata(settings)
                .map(|m| Stat::of(&m).mtime_ns.to_string())
                .unwrap_or_default();
            format!("{path} {stamp}")
        }
    }
}
