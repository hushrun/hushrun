//! The two keys of a C or C++ compile (B-53, B-54) and the hint that links them.
//!
//! The first key is what was asked. The second is what was read: each file the compiler's full
//! dependency list names, by its path from a root or base and its SHA-256; each **place** where a
//! header would have been found first had it existed, as absent; and each **probe**, a literal
//! name a read file asks `__has_include` about, in every folder searched, as present or absent.
//! The hint records the paths under the first key; a lookup hashes the files as they are now and
//! checks each place and probe, and never trusts the hint (B-7).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use hushrun_core::Digest;
use hushrun_digest::cache::Stat;
use serde::{Deserialize, Serialize};

use super::args::{Compile, DepMode, Dir, Lang};
use super::compiler::Identity;
use super::places::{self, Places};
use super::text;

/// The schema of the first key.
const STEP_SCHEMA: &str = "hushrun.cc-key/v1";
/// The schema of the second key.
const BLOCK_SCHEMA: &str = "hushrun.cc-reads/v1";
/// The schema of a tied block's key.
const TIED_SCHEMA: &str = "hushrun.cc-tied/v1";
/// The schema of a hint.
const HINT_SCHEMA: &str = "hushrun.cc-hint/v1";

/// The variables a compile reads without an argument saying so (B-53).
pub const KEYED_ENV: &[&str] = &[
    "CPATH",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "OBJC_INCLUDE_PATH",
    "SDKROOT",
    "DEVELOPER_DIR",
    "MACOSX_DEPLOYMENT_TARGET",
    "IPHONEOS_DEPLOYMENT_TARGET",
    "TVOS_DEPLOYMENT_TARGET",
    "WATCHOS_DEPLOYMENT_TARGET",
    "XROS_DEPLOYMENT_TARGET",
    "SOURCE_DATE_EPOCH",
    "CCC_OVERRIDE_OPTIONS",
    "GCC_EXEC_PREFIX",
    "COMPILER_PATH",
    "GCC_COMPARE_DEBUG",
    "LANG",
    "LC_ALL",
    "LC_MESSAGES",
    "LC_CTYPE",
    "GCC_COLORS",
    "GCC_URLS",
    "TERM_URLS",
    "NO_COLOR",
];

/// What the first key is taken from.
pub struct Asked<'a> {
    /// The compile.
    pub compile: &'a Compile,
    /// The compiler's identity.
    pub identity: &'a Identity,
    /// The call's places.
    pub places: &'a Places,
    /// The compile's folder.
    pub cwd: &'a str,
    /// What `-march=native` and its kin resolve to, when given.
    pub native: Option<&'a str>,
    /// The flags the launcher adds (B-55).
    pub added: &'a [String],
    /// The variables of the call, by name.
    pub var: &'a dyn Fn(&str) -> Option<OsString>,
    /// The person's and the repository's extra variables (B-21, B-35).
    pub extra: &'a [String],
}

/// The first key (B-53) and its body, for the key log.
pub fn step(asked: &Asked<'_>) -> (Digest, serde_json::Value) {
    let places = asked.places;
    let compile = asked.compile;
    let hide_var = |name: &str| -> serde_json::Value {
        (asked.var)(name)
            .map(|v| places.hide(&v.to_string_lossy()))
            .into()
    };
    let env: BTreeMap<&str, serde_json::Value> = KEYED_ENV
        .iter()
        .map(|name| (*name, hide_var(name)))
        .collect();
    let extra: BTreeMap<&str, serde_json::Value> = asked
        .extra
        .iter()
        .map(|name| (name.as_str(), hide_var(name)))
        .collect();
    let body = serde_json::json!({
        "schema": STEP_SCHEMA,
        "compiler": asked.identity.digest.hex(),
        "lang": compile.lang.word(),
        "cwd": places.hide(asked.cwd),
        "args": compile.keyed.iter().map(|a| places.hide(a)).collect::<Vec<_>>(),
        "depfile": match compile.dep.mode {
            DepMode::None => "none",
            DepMode::All => "MD",
            DepMode::User => "MMD",
        },
        "phony": compile.dep.phony,
        "native": asked.native,
        "env": env,
        "extra": extra,
        "added": asked.added.iter().map(|a| places.hide(a)).collect::<Vec<_>>(),
    });
    let canonical = hushrun_core::jcs::canonicalize(&body)
        .expect("strings and lists always have a canonical form");
    (Digest::of(canonical.as_bytes()), body)
}

/// One read of a hint: its path as the compiler wrote it, with roots and bases as placeholders,
/// and whether it lies in a system folder (`-MMD` leaves those out).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HintRead {
    /// The path.
    pub p: String,
    /// Found in a system folder.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub s: bool,
}

/// A hint (B-54): what a compile under a first key read, where it looked, what it asked about.
/// Every path is a folder of [`Hint::dirs`] and a name in it, so that the thousand headers of a
/// unit and the places beside them are written with each folder once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hint {
    /// Its schema.
    pub schema: String,
    /// The folders, with roots and bases as placeholders; a read's may be relative, as the
    /// compiler wrote it.
    pub dirs: Vec<String>,
    /// Each read, in the dependency list's order, the source first: its folder, its name, and
    /// whether it lies in a system folder.
    pub reads: Vec<(u32, String, bool)>,
    /// Each place, absent when it was compiled: folders and the names absent in each. A
    /// header's places are often the same names in several folders, written once.
    pub places: Vec<(Vec<u32>, Vec<String>)>,
    /// Each probe: a folder, a name, and whether it was present.
    pub probes: Vec<(u32, String, bool)>,
}

/// Splits a path into its folder and its name: `/a/b.h` is `/a` and `b.h`, `/b.h` is `/` and
/// `b.h`, `b.h` is `` and `b.h`.
fn split(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((dir, name)) => (dir, name),
        None => ("", path),
    }
}

/// [`split`]'s inverse.
fn join(dir: &str, name: &str) -> String {
    match dir {
        "" => name.to_string(),
        "/" => format!("/{name}"),
        _ => format!("{dir}/{name}"),
    }
}

impl Hint {
    /// A hint from what a compile read, the places and the probes, each a path with
    /// placeholders.
    pub fn new(reads: Vec<HintRead>, places: Vec<String>, probes: Vec<(String, bool)>) -> Hint {
        let mut dirs: Vec<String> = Vec::new();
        let mut index: BTreeMap<String, u32> = BTreeMap::new();
        let mut at = |dir: &str| -> u32 {
            if let Some(i) = index.get(dir) {
                return *i;
            }
            let i = u32::try_from(dirs.len()).unwrap_or(u32::MAX);
            dirs.push(dir.to_string());
            index.insert(dir.to_string(), i);
            i
        };
        let reads = reads
            .iter()
            .map(|r| {
                let (dir, name) = split(&r.p);
                (at(dir), name.to_string(), r.s)
            })
            .collect();
        let mut grouped: Vec<(u32, Vec<String>)> = Vec::new();
        for place in &places {
            let (dir, name) = split(place);
            let i = at(dir);
            match grouped.iter_mut().find(|(d, _)| *d == i) {
                Some((_, names)) => names.push(name.to_string()),
                None => grouped.push((i, vec![name.to_string()])),
            }
        }
        // Folders absent the same names share one entry.
        let mut merged: Vec<(Vec<u32>, Vec<String>)> = Vec::new();
        for (dir, names) in grouped {
            match merged.iter_mut().find(|(_, n)| *n == names) {
                Some((dirs, _)) => dirs.push(dir),
                None => merged.push((vec![dir], names)),
            }
        }
        let probes = probes
            .iter()
            .map(|(p, present)| {
                let (dir, name) = split(p);
                (at(dir), name.to_string(), *present)
            })
            .collect();
        Hint {
            schema: HINT_SCHEMA.to_string(),
            dirs,
            reads,
            places: merged,
            probes,
        }
    }

    /// The reads, each as a path with placeholders.
    pub fn reads(&self) -> Vec<HintRead> {
        self.reads
            .iter()
            .map(|(d, name, s)| HintRead {
                p: join(self.dir(*d), name),
                s: *s,
            })
            .collect()
    }

    fn dir(&self, i: u32) -> &str {
        self.dirs.get(i as usize).map_or("", String::as_str)
    }

    /// The hint's bytes as the store keeps them.
    pub fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// A hint from the store's bytes; `None` when they aren't one.
    pub fn read(bytes: &[u8]) -> Option<Hint> {
        serde_json::from_slice::<Hint>(bytes)
            .ok()
            .filter(|h| h.schema == HINT_SCHEMA && h.well_formed())
    }

    /// Whether every index names a folder.
    fn well_formed(&self) -> bool {
        let n = self.dirs.len();
        self.reads.iter().all(|(d, _, _)| (*d as usize) < n)
            && self
                .places
                .iter()
                .all(|(dirs, _)| dirs.iter().all(|d| (*d as usize) < n))
            && self.probes.iter().all(|(d, _, _)| (*d as usize) < n)
    }
}

/// The second key (B-54) from a hint and the digests found for its reads, in order.
pub fn block(step: &Digest, hint: &Hint, digests: &[Digest]) -> Digest {
    let digests: Vec<String> = digests.iter().map(Digest::hex).collect();
    let body = serde_json::json!({
        "schema": BLOCK_SCHEMA,
        "step": step.hex(),
        "dirs": hint.dirs,
        "reads": hint.reads,
        "digests": digests,
        "places": hint.places,
        "probes": hint.probes,
    });
    let canonical = hushrun_core::jcs::canonicalize(&body)
        .expect("strings and lists always have a canonical form");
    Digest::of(canonical.as_bytes())
}

/// A tied block's key (B-56): the second key and the root's forms, so that it is reused only
/// where the root is the same.
pub fn tied(block: &Digest, places: &Places) -> Digest {
    let body = serde_json::json!({
        "schema": TIED_SCHEMA,
        "block": block.hex(),
        "roots": places.roots(),
    });
    let canonical = hushrun_core::jcs::canonicalize(&body)
        .expect("strings and lists always have a canonical form");
    Digest::of(canonical.as_bytes())
}

/// The folders of the include search, absolute, in the order the compiler searches them (B-54):
/// the quote folders, `-I` and `-F`, `CPATH`'s folders, `-isystem` and `-iframework`,
/// `C_INCLUDE_PATH`'s (or `CPLUS_INCLUDE_PATH`'s), the driver's system folders, then
/// `-idirafter`. As GCC and clang do, a folder that is also a system one is searched only at its
/// system place (its `-I` is dropped), and a folder named twice only at its first; folders are
/// the same when they resolve to one. The quote folders are kept apart: an angled include never
/// searches them, so they never hide a later place of theirs.
pub fn search(
    compile: &Compile,
    identity: &Identity,
    cwd: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<Dir> {
    let abs = |d: &Dir| Dir {
        path: places::absolute(cwd, &d.path)
            .to_string_lossy()
            .into_owned(),
        framework: d.framework,
        system: d.system,
    };
    let from_var = |name: &str, system: bool| -> Vec<Dir> {
        var(name)
            .map(|value| {
                std::env::split_paths(&value)
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|p| Dir {
                        path: places::absolute(cwd, &p.to_string_lossy())
                            .to_string_lossy()
                            .into_owned(),
                        framework: false,
                        system,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let lang_var = match compile.lang {
        Lang::C => "C_INCLUDE_PATH",
        Lang::Cxx => "CPLUS_INCLUDE_PATH",
    };
    let (user, system): (Vec<&Dir>, Vec<&Dir>) = compile.search.iter().partition(|d| !d.system);
    let mut rest: Vec<Dir> = user.into_iter().map(abs).collect();
    rest.extend(from_var("CPATH", false));
    rest.extend(system.into_iter().map(abs));
    rest.extend(from_var(lang_var, true));
    rest.extend(identity.system.iter().map(|d| Dir {
        path: d.path.clone(),
        framework: d.framework,
        system: true,
    }));
    rest.extend(compile.after.iter().map(abs));
    let same = |d: &Dir| {
        let resolved = std::fs::canonicalize(&d.path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| d.path.clone());
        (resolved, d.framework)
    };
    let systems: BTreeSet<(String, bool)> = rest.iter().filter(|d| d.system).map(same).collect();
    let mut seen = BTreeSet::new();
    rest.retain(|d| {
        let id = same(d);
        (d.system || !systems.contains(&id)) && seen.insert(id)
    });
    let mut seen = BTreeSet::new();
    let mut dirs: Vec<Dir> = compile
        .quote
        .iter()
        .map(abs)
        .filter(|d| seen.insert(same(d)))
        .collect();
    dirs.extend(rest);
    dirs
}

/// Whether a read lies in a system folder for `-MMD` (B-55, B-57): under one of the driver's
/// system folders, and under no folder a user include could have found it in (the source's, a
/// quote folder, `-I`, `-F`, `CPATH`'s). Headers of `-isystem`, `-idirafter` and
/// `C_INCLUDE_PATH` folders are kept: which folder a header was found in only the compiler
/// knows, and a depfile naming one file more costs a stat where one naming one fewer misses a
/// rebuild. One definition for the depfile of a compile and the hint's reads.
pub fn system_read(abs: &str, identity: &Identity, dirs: &[Dir], source_dir: &str) -> bool {
    identity.in_driver_system(abs)
        && !places::under(abs, source_dir)
        && !dirs
            .iter()
            .any(|d| !d.system && places::under(abs, &d.path))
}

/// The name a header found in `dir` was asked by, from its absolute path: `rest` for a plain
/// folder, `N/rest` for `dir/N.framework/Headers/rest` and `dir/N.framework/PrivateHeaders/rest`;
/// and whether it was a framework's private header.
fn spelling(path: &str, dir: &Dir) -> Option<(String, bool)> {
    let rest = path
        .strip_prefix(dir.path.trim_end_matches('/'))?
        .strip_prefix('/')?;
    if !dir.framework {
        return Some((rest.to_string(), false));
    }
    let (framework, inner) = rest.split_once('/')?;
    let name = framework.strip_suffix(".framework")?;
    if let Some(inner) = inner.strip_prefix("Headers/") {
        return Some((format!("{name}/{inner}"), false));
    }
    let inner = inner.strip_prefix("PrivateHeaders/")?;
    Some((format!("{name}/{inner}"), true))
}

/// Where `name` would be in `dir`, in the order the compiler looks: a framework's `Headers`, then
/// its `PrivateHeaders`.
fn candidates(dir: &Dir, name: &str) -> Vec<PathBuf> {
    if !dir.framework {
        return vec![Path::new(&dir.path).join(name)];
    }
    let Some((framework, inner)) = name.split_once('/') else {
        return Vec::new();
    };
    let at = Path::new(&dir.path).join(format!("{framework}.framework"));
    vec![
        at.join("Headers").join(inner),
        at.join("PrivateHeaders").join(inner),
    ]
}

fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Whether `path` is there and dated at or after `floor`: it appeared or changed while the
/// compile ran (B-11).
fn dated_since(path: &Path, floor: SystemTime) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|meta| crate::inputs::dated_after(&Stat::of(&meta), floor))
}

/// One read file after a compile, for the places and probes.
pub struct Read<'a> {
    /// Its absolute path, as the compiler named it.
    pub abs: &'a str,
    /// What it holds that matters (its probes).
    pub seen: &'a text::Seen,
    /// The files that included it, absolute, from clang's graph; empty when unknown.
    pub includers: Vec<String>,
}

/// Where a compile searched, for its places and probes (B-54).
pub struct Searched<'a> {
    /// The folders of the search, in order ([`search`]).
    pub dirs: &'a [Dir],
    /// The source's folder, which stands for an including file's where the graph names none.
    pub source_dir: &'a str,
    /// The compile's folder, searched first for a forced include given relative.
    pub cwd: &'a str,
    /// The forced includes (`-include`, `-imacros`) as given.
    pub forced: &'a [String],
    /// The folders the compile and the launcher wrote into (the object's, the depfile's): their
    /// dates say nothing about a probe.
    pub written: &'a [String],
    /// When the compile started, by the file system's clock: a place or probe dated at or after
    /// it changed while the compiler looked.
    pub floor: SystemTime,
}

/// The places of a compile and its probes, each with whether it was present.
pub type PlacesAndProbes = (Vec<String>, Vec<(String, bool)>);

/// A place or probe changed while the compile ran: the object can't be keyed (B-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Changed;

/// The places and probes of a compile (B-54), with roots and bases as placeholders, each once.
///
/// A place that exists now and isn't a read is left out when it is older than the compile (the
/// folder wasn't searched for that header: an angled include doesn't search the including
/// file's folder, which places over-count), and is [`Changed`] when it isn't: a header that
/// appeared where the compiler had already looked. A probe there now and dated since the floor,
/// or one absent in a folder changed since, is [`Changed`] too.
pub fn places_and_probes(
    reads: &[Read<'_>],
    searched: &Searched<'_>,
    places: &Places,
) -> Result<PlacesAndProbes, Changed> {
    let mut out_places = Vec::new();
    let mut out_probes = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let read_paths: BTreeSet<&str> = reads.iter().map(|r| r.abs).collect();
    let floor = searched.floor;
    for (i, read) in reads.iter().enumerate() {
        // The source itself is named by the call, not found by a search.
        if i > 0 {
            let mut folders: Vec<Dir> = Vec::new();
            let user = |path: String| Dir {
                path,
                framework: false,
                system: false,
            };
            let forced = searched.forced.iter().any(|f| {
                !f.starts_with('/')
                    && read
                        .abs
                        .ends_with(&format!("/{}", f.trim_start_matches("./")))
            });
            if forced {
                folders.push(user(searched.cwd.to_string()));
            }
            if read.includers.is_empty() {
                folders.push(user(searched.source_dir.to_string()));
            } else {
                folders.extend(read.includers.iter().filter_map(|f| {
                    Path::new(f)
                        .parent()
                        .map(|p| user(p.to_string_lossy().into_owned()))
                }));
            }
            folders.extend(searched.dirs.iter().cloned());
            for (pos, dir) in folders.iter().enumerate() {
                let Some((name, private)) = spelling(read.abs, dir) else {
                    continue;
                };
                let mut earlier: Vec<PathBuf> = folders[..pos]
                    .iter()
                    .flat_map(|d| candidates(d, &name))
                    .collect();
                // A private header: the framework's public one is looked for first.
                if private {
                    earlier.extend(candidates(dir, &name).into_iter().take(1));
                }
                for at in earlier {
                    let text = at.to_string_lossy();
                    if text == read.abs || read_paths.contains(text.as_ref()) {
                        continue;
                    }
                    if exists(&at) {
                        if dated_since(&at, floor) {
                            return Err(Changed);
                        }
                        continue;
                    }
                    if seen.insert(at.clone()) {
                        out_places.push(places.hide(&text));
                    }
                }
            }
        }
        if read.seen.probes.is_empty() {
            continue;
        }
        let own = Path::new(read.abs)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut folders = vec![Dir {
            path: own,
            framework: false,
            system: false,
        }];
        folders.extend(searched.dirs.iter().cloned());
        for probe in &read.seen.probes {
            let (name, _) = text::probe_name(probe);
            for dir in &folders {
                for at in candidates(dir, name) {
                    if !seen.insert(at.clone()) {
                        continue;
                    }
                    let present = exists(&at);
                    let changed = if present {
                        dated_since(&at, floor)
                    } else {
                        at.parent().is_some_and(|folder| {
                            !searched.written.iter().any(|w| Path::new(w) == folder)
                                && dated_since(folder, floor)
                        })
                    };
                    if changed {
                        return Err(Changed);
                    }
                    out_probes.push((places.hide(&at.to_string_lossy()), present));
                }
            }
        }
    }
    Ok((out_places, out_probes))
}

/// Whether every place of a hint is still absent and every probe as it was, here (B-54). The
/// places are checked a folder at a time: a folder that doesn't exist holds none of them, and
/// a folder asked about many names is listed once. A name is taken as present when one in the
/// folder matches it ignoring case, as a file system that ignores case would find it: at worst
/// a compile, never a wrong reuse.
pub fn places_hold(hint: &Hint, places: &Places) -> bool {
    let folders = hint
        .places
        .iter()
        .flat_map(|(dirs, names)| dirs.iter().map(move |d| (*d, names)));
    for (dir, names) in folders {
        let folder = PathBuf::from(places.show(hint.dir(dir)));
        match std::fs::symlink_metadata(&folder) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return false,
            Ok(meta) if !meta.is_dir() && !meta.file_type().is_symlink() => continue,
            Ok(_) => {}
        }
        if names.len() <= 3 {
            if names.iter().any(|name| exists(&folder.join(name))) {
                return false;
            }
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            return false;
        };
        let here: BTreeSet<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_lowercase())
            .collect();
        if names.iter().any(|name| here.contains(&name.to_lowercase())) {
            return false;
        }
    }
    hint.probes.iter().all(|(dir, name, present)| {
        exists(&Path::new(&places.show(hint.dir(*dir))).join(name)) == *present
    })
}

/// A hint's read as a path here: shown with this call's places, made absolute from `cwd`.
pub fn read_path(read: &HintRead, places: &Places, cwd: &Path) -> PathBuf {
    let shown = places.show(&read.p);
    places::absolute(cwd, &shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(path: &str, framework: bool) -> Dir {
        Dir {
            path: path.into(),
            framework,
            system: false,
        }
    }

    // covers: B-54
    #[test]
    fn a_header_s_places_are_the_earlier_folders_and_its_probes_every_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        for d in ["src", "first", "second", "fw/Kit.framework/Headers"] {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        std::fs::write(tmp.path().join("second/h.h"), "").unwrap();
        std::fs::write(tmp.path().join("first/opt-present.h"), "").unwrap();
        let places = Places::new(std::slice::from_ref(&root), &[], &[]);
        let dirs = vec![
            dir(&format!("{root}/first"), false),
            dir(&format!("{root}/second"), false),
            dir(&format!("{root}/fw"), true),
        ];
        let none = text::Seen::default();
        let asks = text::Seen {
            probes: vec!["\"opt-present.h\"".into(), "<Kit/k.h>".into()],
            ..text::Seen::default()
        };
        let source = format!("{root}/src/a.c");
        let header = format!("{root}/second/h.h");
        let reads = [
            Read {
                abs: &source,
                seen: &asks,
                includers: vec![],
            },
            Read {
                abs: &header,
                seen: &none,
                includers: vec![],
            },
        ];
        let source_dir = format!("{root}/src");
        let (found_places, probes) =
            places_and_probes(&reads, &searched(&dirs, &source_dir), &places).unwrap();
        assert_eq!(found_places, ["{root}/src/h.h", "{root}/first/h.h"]);
        assert!(probes.contains(&("{root}/first/opt-present.h".into(), true)));
        assert!(probes.contains(&("{root}/src/opt-present.h".into(), false)));
        assert!(probes.contains(&("{root}/fw/Kit.framework/Headers/k.h".into(), false)));
        let h = Hint::new(vec![], found_places, probes);
        assert!(places_hold(&h, &places));
        // A header created where the search looks first breaks the hint.
        std::fs::write(tmp.path().join("first/h.h"), "").unwrap();
        assert!(!places_hold(&h, &places));
        std::fs::remove_file(tmp.path().join("first/h.h")).unwrap();
        // So does a file a probe asked about appearing.
        std::fs::write(tmp.path().join("src/opt-present.h"), "").unwrap();
        assert!(!places_hold(&h, &places));
    }

    // covers: B-54
    #[test]
    fn places_in_one_folder_are_checked_by_listing_it_ignoring_case() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        std::fs::create_dir_all(tmp.path().join("inc")).unwrap();
        let places = Places::new(std::slice::from_ref(&root), &[], &[]);
        let names = ["a.h", "b.h", "c.h", "d.h", "e.h"];
        let mut list: Vec<String> = names.iter().map(|n| format!("{{root}}/inc/{n}")).collect();
        list.push("{root}/missing/x.h".into());
        let h = Hint::new(vec![], list, vec![]);
        assert_eq!(h.dirs, ["{root}/inc", "{root}/missing"]);
        assert!(places_hold(&h, &places));
        // A file whose name differs only in case is a header a case-blind file system finds.
        std::fs::write(tmp.path().join("inc/D.H"), "").unwrap();
        assert!(!places_hold(&h, &places));
        std::fs::remove_file(tmp.path().join("inc/D.H")).unwrap();
        // A folder that appears where none was holds what it holds.
        std::fs::create_dir_all(tmp.path().join("missing")).unwrap();
        assert!(places_hold(&h, &places));
        std::fs::write(tmp.path().join("missing/x.h"), "").unwrap();
        assert!(!places_hold(&h, &places));
        // Reads keep their spelling, relative or absolute.
        let reads = vec![
            HintRead {
                p: "{root}/src/a.c".into(),
                s: false,
            },
            HintRead {
                p: "inc/x.h".into(),
                s: false,
            },
            HintRead {
                p: "x.h".into(),
                s: false,
            },
            HintRead {
                p: "/x.h".into(),
                s: true,
            },
        ];
        assert_eq!(Hint::new(reads.clone(), vec![], vec![]).reads(), reads);
    }

    fn searched<'a>(dirs: &'a [Dir], source_dir: &'a str) -> Searched<'a> {
        Searched {
            dirs,
            source_dir,
            cwd: "/nonexistent",
            forced: &[],
            written: &[],
            floor: SystemTime::now() + std::time::Duration::from_secs(3600),
        }
    }

    // covers: B-54
    #[test]
    fn a_framework_header_is_spelled_by_its_framework_public_or_private() {
        let fw = dir("/F", true);
        assert_eq!(
            spelling("/F/Kit.framework/Headers/sub/k.h", &fw),
            Some(("Kit/sub/k.h".to_string(), false))
        );
        assert_eq!(
            spelling("/F/Kit.framework/PrivateHeaders/k.h", &fw),
            Some(("Kit/k.h".to_string(), true))
        );
        assert_eq!(
            candidates(&fw, "Kit/sub/k.h"),
            [
                PathBuf::from("/F/Kit.framework/Headers/sub/k.h"),
                PathBuf::from("/F/Kit.framework/PrivateHeaders/sub/k.h")
            ]
        );
        assert!(spelling("/F/other/k.h", &fw).is_none());
        // A private header's places: the public header of its own framework, and both forms in
        // every earlier framework folder.
        let places = Places::new(&["/F".into()], &[], &[]);
        let none = text::Seen::default();
        let reads = [
            Read {
                abs: "/F/a.c",
                seen: &none,
                includers: vec![],
            },
            Read {
                abs: "/F/second/Kit.framework/PrivateHeaders/k.h",
                seen: &none,
                includers: vec!["/F/a.c".into()],
            },
        ];
        let dirs = [dir("/F/first", true), dir("/F/second", true)];
        let (found, _) = places_and_probes(&reads, &searched(&dirs, "/F"), &places).unwrap();
        for place in [
            "{root}/first/Kit.framework/Headers/k.h",
            "{root}/first/Kit.framework/PrivateHeaders/k.h",
            "{root}/second/Kit.framework/Headers/k.h",
        ] {
            assert!(found.contains(&place.to_string()), "{place}: {found:?}");
        }
    }

    // covers: B-54
    #[test]
    fn without_a_graph_the_source_s_folder_stands_for_every_including_file_s() {
        // Today's behaviour, pinned (owner decision 5): GCC, clang before 15 and what a system
        // header includes have no graph, and a quoted include from a header in another folder
        // gets no place in that folder. Only the audit finds a header added there.
        let places = Places::new(&["/r".into()], &[], &[]);
        let none = text::Seen::default();
        let read = |abs: &'static str, includers: Vec<String>| Read {
            abs,
            seen: &none,
            includers,
        };
        let dirs = [dir("/r/include", false), dir("/r/gen", false)];
        let without = [
            read("/r/src/a.c", vec![]),
            read("/r/include/foo/a.h", vec![]),
            read("/r/gen/config.h", vec![]),
        ];
        let (found, _) = places_and_probes(&without, &searched(&dirs, "/r/src"), &places).unwrap();
        assert!(
            found.contains(&"{root}/src/config.h".to_string()),
            "{found:?}"
        );
        assert!(
            found.contains(&"{root}/include/config.h".to_string()),
            "{found:?}"
        );
        assert!(
            !found.contains(&"{root}/include/foo/config.h".to_string()),
            "{found:?}"
        );
        // With clang's graph, the including header's folder is a place.
        let with = [
            read("/r/src/a.c", vec![]),
            read("/r/include/foo/a.h", vec!["/r/src/a.c".into()]),
            read("/r/gen/config.h", vec!["/r/include/foo/a.h".into()]),
        ];
        let (found, _) = places_and_probes(&with, &searched(&dirs, "/r/src"), &places).unwrap();
        assert!(
            found.contains(&"{root}/include/foo/config.h".to_string()),
            "{found:?}"
        );
    }

    // covers: B-54
    #[test]
    fn a_folder_also_given_as_a_system_one_is_searched_there_and_after_comes_last() {
        let compile = match super::super::args::classify(
            &OsString::from("cc"),
            &[
                "-iquote",
                "q",
                "-Ia",
                "-Ib",
                "-isystem",
                "a",
                "-idirafter",
                "late",
                "-Iq",
                "-c",
                "a.c",
                "-o",
                "a.o",
            ]
            .map(OsString::from),
            &[],
        ) {
            super::super::args::Classified::Compile(c) => *c,
            other => panic!("{other:?}"),
        };
        let identity = Identity {
            digest: Digest::of(b"x"),
            clang: true,
            graph: true,
            target: String::new(),
            sdk: None,
            sdk_resolved: None,
            toolchain: None,
            system: vec![super::super::compiler::SystemDir {
                path: "/nonexistent/sys".into(),
                framework: false,
                resolved: None,
            }],
        };
        let var = |name: &str| (name == "C_INCLUDE_PATH").then(|| OsString::from("/nonexistent/c"));
        let got: Vec<String> = search(&compile, &identity, Path::new("/w"), &var)
            .into_iter()
            .map(|d| d.path)
            .collect();
        assert_eq!(
            got,
            [
                "/w/q",
                "/w/b",
                "/w/q",
                "/w/a",
                "/nonexistent/c",
                "/nonexistent/sys",
                "/w/late"
            ]
        );
    }

    // covers: B-54
    #[test]
    fn the_second_key_follows_every_read_place_and_probe() {
        let step = Digest::of(b"step");
        let h = Hint::new(
            vec![HintRead {
                p: "{root}/a.c".into(),
                s: false,
            }],
            vec!["{root}/x.h".into()],
            vec![("{root}/o.h".into(), false)],
        );
        let a = block(&step, &h, &[Digest::of(b"one")]);
        assert_eq!(a, block(&step, &h, &[Digest::of(b"one")]));
        assert_ne!(a, block(&step, &h, &[Digest::of(b"two")]));
        let mut other = h.clone();
        other.probes[0].2 = true;
        assert_ne!(a, block(&step, &other, &[Digest::of(b"one")]));
        let mut other = h.clone();
        other.places.clear();
        assert_ne!(a, block(&step, &other, &[Digest::of(b"one")]));
        assert_eq!(Hint::read(&h.bytes()).unwrap(), h);
    }
}
