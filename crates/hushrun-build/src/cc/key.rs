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

use hushrun_core::Digest;
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

/// The folders of the include search, absolute, in the order the compiler searches them: the
/// quote folders, `-I` and `-F`, the variables' folders, `-isystem` and `-idirafter`, then the
/// driver's system folders (B-54).
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
    let mut dirs: Vec<Dir> = compile.quote.iter().map(abs).collect();
    let (user, system): (Vec<&Dir>, Vec<&Dir>) = compile.search.iter().partition(|d| !d.system);
    dirs.extend(user.into_iter().map(abs));
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
    dirs.extend(from_var("CPATH", false));
    dirs.extend(system.into_iter().map(abs));
    let lang_var = match compile.lang {
        Lang::C => "C_INCLUDE_PATH",
        Lang::Cxx => "CPLUS_INCLUDE_PATH",
    };
    dirs.extend(from_var(lang_var, true));
    dirs.extend(identity.system.iter().map(|d| Dir {
        path: d.path.clone(),
        framework: d.framework,
        system: true,
    }));
    let mut seen = BTreeSet::new();
    dirs.retain(|d| seen.insert((d.path.clone(), d.framework)));
    dirs
}

/// The name a header found in `dir` was asked by, from its absolute path: `rest` for a plain
/// folder, `N/rest` for `dir/N.framework/Headers/rest`.
fn spelling(path: &str, dir: &Dir) -> Option<String> {
    let rest = path
        .strip_prefix(dir.path.trim_end_matches('/'))?
        .strip_prefix('/')?;
    if !dir.framework {
        return Some(rest.to_string());
    }
    let (framework, inner) = rest.split_once('/')?;
    let name = framework.strip_suffix(".framework")?;
    let inner = inner.strip_prefix("Headers/")?;
    Some(format!("{name}/{inner}"))
}

/// Where `name` would be in `dir`.
fn candidate(dir: &Dir, name: &str) -> Option<PathBuf> {
    if !dir.framework {
        return Some(Path::new(&dir.path).join(name));
    }
    let (framework, inner) = name.split_once('/')?;
    Some(
        Path::new(&dir.path)
            .join(format!("{framework}.framework"))
            .join("Headers")
            .join(inner),
    )
}

fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
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

/// The places and probes of a compile (B-54), with roots and bases as placeholders, each once.
/// `source_dir` stands for an including file's folder where the graph names none.
pub fn places_and_probes(
    reads: &[Read<'_>],
    dirs: &[Dir],
    source_dir: &str,
    places: &Places,
) -> (Vec<String>, Vec<(String, bool)>) {
    let mut out_places = Vec::new();
    let mut out_probes = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for (i, read) in reads.iter().enumerate() {
        // The source itself is named by the call, not found by a search.
        if i > 0 {
            let mut folders: Vec<Dir> = Vec::new();
            let includers: Vec<String> = if read.includers.is_empty() {
                vec![source_dir.to_string()]
            } else {
                read.includers
                    .iter()
                    .filter_map(|f| {
                        Path::new(f)
                            .parent()
                            .map(|p| p.to_string_lossy().into_owned())
                    })
                    .collect()
            };
            for dir in includers {
                folders.push(Dir {
                    path: dir,
                    framework: false,
                    system: false,
                });
            }
            folders.extend(dirs.iter().cloned());
            for (pos, dir) in folders.iter().enumerate() {
                let Some(name) = spelling(read.abs, dir) else {
                    continue;
                };
                for earlier in &folders[..pos] {
                    let Some(at) = candidate(earlier, &name) else {
                        continue;
                    };
                    if at.as_os_str() == read.abs {
                        continue;
                    }
                    if !exists(&at) && seen.insert(at.clone()) {
                        out_places.push(places.hide(&at.to_string_lossy()));
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
        folders.extend(dirs.iter().cloned());
        for probe in &read.seen.probes {
            let (name, _) = text::probe_name(probe);
            for dir in &folders {
                let Some(at) = candidate(dir, name) else {
                    continue;
                };
                if seen.insert(at.clone()) {
                    out_probes.push((places.hide(&at.to_string_lossy()), exists(&at)));
                }
            }
        }
    }
    (out_places, out_probes)
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
        let (found_places, probes) =
            places_and_probes(&reads, &dirs, &format!("{root}/src"), &places);
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

    // covers: B-54
    #[test]
    fn a_framework_header_is_spelled_by_its_framework() {
        let fw = dir("/F", true);
        assert_eq!(
            spelling("/F/Kit.framework/Headers/sub/k.h", &fw).as_deref(),
            Some("Kit/sub/k.h")
        );
        assert_eq!(
            candidate(&fw, "Kit/sub/k.h").unwrap(),
            PathBuf::from("/F/Kit.framework/Headers/sub/k.h")
        );
        assert!(spelling("/F/other/k.h", &fw).is_none());
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
