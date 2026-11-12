//! \[ADR 0087\] The CTest adapter, compiler-given (C-30, C-32, C-49): for C and C++ projects built
//! with CMake, a unit is a test program, the program a CTest test starts, and a test is one CTest
//! test. What a test program read is what its build directory says it is built from.
//!
//! - **Loading:** `ctest` takes nothing from the environment, so hushrun puts a `ctest` of its own
//!   first on COMMAND's `PATH` ([`super::program`], [`run::Ctest`]). Every `ctest` started by name
//!   reaches it: typed as COMMAND, or started by a shell line, a script or a Makefile. `make test`,
//!   `ninja test` and `cmake --build … --target test` start CTest by its full path, and aren't
//!   seen.
//! - **Units:** a test program's name is its path from its build directory, without a
//!   configuration's folder or `.exe`, and with the configuration it was built in: `tests/t_add
//!   (Release)`. A test whose command isn't a program built here (`sh`, `python3`) is in a unit
//!   that is `not observed`.
//! - **Ids:** a test's CTest name; for a test `gtest_discover_tests` made, its `--gtest_filter`.
//! - **The plan** keys each unit by what the map says it read last time and, when COMMAND is
//!   `ctest` itself, by what its build says it is built from now (listing and walking before
//!   COMMAND). It writes the lists it keyed each unit by to a file of its own in the run's
//!   directory, which hushrun's `ctest` checks again before it leaves anything out.

mod args;
mod build;
mod junit;
mod run;
mod select;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use hushrun_core::units::{self, Facts, Set, Sets, Sources};

use super::Seen;
use super::adapter::{Adapter, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt};
use super::command::{self, program_name};
use super::facts;
use super::program;

pub use build::SOURCE_KINDS;
pub use run::Ctest;

/// The adapter's word, in its facts files, this machine's store and a hint.
pub const ADAPTER: &str = "ctest";
/// The plan's file in the run's directory: what each unit's key was made from.
const PLAN_FILE: &str = "ctest-plan.v1";
const PLAN_SCHEMA: &str = "hushrun.ctest-plan/v1";

/// What the plan keyed each unit by, for hushrun's `ctest` to check before it leaves any out.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PlanFile {
    schema: String,
    /// Each unit's lists, each the files its key holds.
    units: BTreeMap<String, Vec<Vec<String>>>,
    /// The scope's C and C++ files that aren't ignored: what a key can hold.
    scope: Vec<String>,
}

/// Why nothing is seen of a CTest that a build tool starts by its full path.
const BYPASSED: &str = "make test, ninja test and cmake --build … --target test start ctest by its full path, which hushrun's ctest on PATH can't stand in for; `ctest --test-dir <build directory>` can be observed";

/// The CTest adapter for one COMMAND.
#[derive(Debug)]
pub struct Recognized {
    /// COMMAND's arguments when COMMAND is `ctest` itself: the plan can list and walk before it.
    direct: Option<Vec<String>>,
    /// The run's directory, from [`Adapter::variables`], where the plan writes its file.
    dir: OnceLock<PathBuf>,
}

/// \[ADR 0087\] The CTest adapter for a COMMAND that starts `ctest` by name (C-49): `ctest …`
/// itself, or a wrapper whose text starts `ctest` and no other runner
/// ([`command::wrapper_starts`]); never a Makefile CMake wrote. `Some(Err)` for `ctest` started by its path and for CMake's own `test` targets,
/// which bypass `PATH`. `None` for any other COMMAND.
pub fn recognized(command: &[String], cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    let found = recognize(command, cwd)?;
    Some(found.map(|direct| {
        Box::new(Recognized {
            direct,
            dir: OnceLock::new(),
        }) as Box<dyn Adapter>
    }))
}

/// What COMMAND is to the CTest adapter: `Some(Ok(Some(args)))` for `ctest …` itself,
/// `Some(Ok(None))` for a COMMAND that starts it by name through something else.
fn recognize(command: &[String], cwd: &Path) -> Option<Result<Option<Vec<String>>, String>> {
    let first = command.first()?;
    let rest = &command[1..];
    match program_name(first) {
        "ctest" if first.contains(['/', '\\']) => Some(Err(format!(
            "COMMAND starts ctest by its path ({first}), which hushrun's ctest on PATH can't stand in for: start it as `ctest`"
        ))),
        "ctest" => Some(Ok(Some(rest.to_vec()))),
        "cmake" => {
            let build = rest.iter().any(|w| w == "--build");
            let test = rest
                .windows(2)
                .any(|w| matches!(w[0].as_str(), "--target" | "-t") && w[1] == "test")
                || rest.iter().any(|w| w == "--target=test");
            (build && test).then(|| Err(BYPASSED.to_string()))
        }
        "make" | "gmake" | "ninja" => {
            let dir = rest
                .windows(2)
                .find(|w| w[0] == "-C")
                .map_or_else(|| cwd.to_path_buf(), |w| cwd.join(&w[1]));
            let targets_test = rest.iter().any(|w| w == "test");
            if targets_test && dir.join("CMakeCache.txt").is_file() {
                return Some(Err(BYPASSED.to_string()));
            }
            if program_name(first) == "ninja" || dir.join("CMakeCache.txt").is_file() {
                return None;
            }
            wrapped(command, cwd)
        }
        _ => wrapped(command, cwd),
    }
}

/// A wrapper COMMAND that starts `ctest` by name, by the one rule.
fn wrapped(command: &[String], cwd: &Path) -> Option<Result<Option<Vec<String>>, String>> {
    command::wrapper_starts(command, cwd, command::Runner::Ctest).then_some(Ok(None))
}

impl Recognized {
    /// What the build says each listed unit is built from now, for COMMAND `ctest …`, before it:
    /// `None` when it can't be told, and then the map alone keys the plan.
    fn compiled(&self, at: &PlanAt<'_>) -> Option<Compiled> {
        let words = self.direct.as_ref()?;
        let items = args::parse(words);
        if !matches!(args::mode(&items), args::Mode::Select) {
            return None;
        }
        let path = std::env::var_os("PATH");
        let real = program::real("ctest", path.as_deref().unwrap_or_default())?;
        let root = std::fs::canonicalize(at.root).ok()?;
        let (tests, _, _) = run::listed(&real, &items, at.cwd, &root).ok()?;
        let built = run::walk(&tests, &root, path);
        let mut out = Compiled::default();
        let mut programs: BTreeMap<&str, bool> = BTreeMap::new();
        for test in &tests {
            let built_here = matches!(test.program, select::Program::Built { .. });
            *programs.entry(&test.unit).or_insert(true) &= built_here;
        }
        let every: BTreeSet<String> = every_source(at.tree);
        for (unit, built_here) in programs {
            let Some(b) = built.get(unit).filter(|b| built_here && b.stale.is_none()) else {
                out.units.insert(unit.to_string(), None);
                continue;
            };
            let reads: BTreeSet<String> = if b.every_source {
                every.clone()
            } else {
                b.reads.clone()
            };
            out.named
                .extend(reads.iter().filter(|f| build::is_source(f)).cloned());
            out.unit_files.extend(b.own.iter().cloned());
            out.units
                .insert(unit.to_string(), Some((reads, b.own.clone())));
        }
        Some(out)
    }

    /// The plan from the map (and hints), with what the build says now when it is known.
    fn planning(&self, at: &PlanAt<'_>, compiled: Option<&Compiled>) -> Planning {
        let sets = match compiled {
            Some(c) => units::classify(
                at.tree.paths(),
                &|p| c.unit_files.contains(p),
                Sources::Named(&c.named, SOURCE_KINDS),
                at.ignore,
            ),
            None => units::classify(
                at.tree.paths(),
                &|p| at.map.unit_files.contains(p),
                Sources::Named(&at.map.sources, SOURCE_KINDS),
                at.ignore,
            ),
        };
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let known = match compiled {
            Some(c) => c
                .units
                .iter()
                .map(|(unit, seen)| {
                    let lists = seen
                        .as_ref()
                        .filter(|(_, own)| own.iter().all(|f| at.tree.contains(f)))
                        .and_then(|(reads, _)| {
                            let compiled: BTreeSet<String> = reads
                                .iter()
                                .filter(|f| build::is_source(f))
                                .cloned()
                                .collect();
                            super::shared::lists(
                                unit,
                                at.map.units.get(unit),
                                &shared,
                                at.tree,
                                Some(&compiled),
                            )
                        });
                    (unit.clone(), lists)
                })
                .collect(),
            None => at
                .map
                .units
                .keys()
                .map(|unit| {
                    let lists =
                        super::shared::lists(unit, at.map.units.get(unit), &shared, at.tree, None);
                    (unit.clone(), lists)
                })
                .collect(),
        };
        Planning {
            sets,
            known,
            received: shared.received,
            ..Planning::default()
        }
    }
}

/// What a unit's programs are built from, and of those its own files.
type Made = (BTreeSet<String>, BTreeSet<String>);

/// What a plan's walk says each unit is built from: `None` for one that can't be keyed.
#[derive(Debug, Default)]
struct Compiled {
    units: BTreeMap<String, Option<Made>>,
    named: BTreeSet<String>,
    unit_files: BTreeSet<String>,
}

/// Every C and C++ file of the scope: what a program whose build can't be read reads.
fn every_source(tree: &units::Tree) -> BTreeSet<String> {
    tree.paths()
        .filter(|p| build::is_source(p))
        .map(str::to_string)
        .collect()
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "CTest"
    }

    fn test_file(&self) -> &'static str {
        "test program"
    }

    fn tests(&self) -> super::adapter::KnowsTests {
        super::adapter::KnowsTests {
            unit: true,
            reads: false,
        }
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let exe = at.exe()?;
        let dir = program::make(at.dir, &exe, &["ctest"])
            .map_err(|err| format!("can't put hushrun's ctest on PATH: {err}"))?;
        let _ = self.dir.set(at.dir.to_path_buf());
        program::variables(&dir, (at.env)("PATH"))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        Ok(Some(self.planning(at, None)))
    }

    fn plan(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let compiled = self.compiled(at);
        let planning = self.planning(at, compiled.as_ref());
        // What each unit's key holds, for hushrun's ctest to check before it leaves any out. A
        // file that can't be written leaves nothing out.
        if let Some(dir) = self.dir.get() {
            let scope = at
                .tree
                .paths()
                .filter(|p| build::is_source(p) && planning.sets.of(p) != Some(Set::Ignored))
                .map(str::to_string)
                .collect();
            let units = planning
                .known
                .iter()
                .filter_map(|(unit, lists)| {
                    let lists = lists.as_ref()?;
                    let files = lists
                        .iter()
                        .map(|l| {
                            planning
                                .sets
                                .keyed(l.files.iter().map(String::as_str))
                                .into_iter()
                                .map(str::to_string)
                                .collect()
                        })
                        .collect();
                    Some((unit.clone(), files))
                })
                .collect();
            let file = PlanFile {
                schema: PLAN_SCHEMA.into(),
                units,
                scope,
            };
            if let Ok(bytes) = serde_json::to_vec(&file) {
                let _ = facts::write_whole(&dir.join(PLAN_FILE), &bytes);
            }
        }
        Ok(Some(planning))
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        let mine: Vec<&facts::FactsFile> =
            at.files.iter().filter(|f| f.adapter == ADAPTER).collect();
        if mine.is_empty() {
            return Err(format!(
                "CTest wrote nothing for hushrun: no `ctest` reached hushrun's `ctest` on PATH; {BYPASSED}"
            ));
        }
        if mine.iter().all(|f| f.units.is_empty()) {
            let why = mine
                .iter()
                .find_map(|f| f.settings.get("notObserved").and_then(|v| v.as_str()))
                .unwrap_or("ctest ran no test");
            return Err(why.to_string());
        }
        let mut own: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut every: BTreeSet<String> = BTreeSet::new();
        for file in &mine {
            if let Some(map) = file.settings.get("own").and_then(|v| v.as_object()) {
                for (unit, files) in map {
                    let files = files.as_array().into_iter().flatten();
                    own.entry(unit.clone())
                        .or_default()
                        .extend(files.filter_map(|f| f.as_str().map(str::to_string)));
                }
            }
            if let Some(list) = file.settings.get("everySource").and_then(|v| v.as_array()) {
                every.extend(list.iter().filter_map(|u| u.as_str().map(str::to_string)));
            }
        }
        let all_sources = every_source(at.tree);
        let merged = facts::merged(at.files, ADAPTER);
        let mut seen: BTreeMap<String, Seen> = BTreeMap::new();
        let mut named: BTreeSet<String> = BTreeSet::new();
        for (unit, m) in merged {
            let mut reads = m.reads;
            if every.contains(&unit) {
                reads.extend(all_sources.iter().cloned());
            }
            named.extend(reads.iter().filter(|f| build::is_source(f)).cloned());
            let facts = Facts {
                complete: m.finished >= m.selected,
                failed: m.failed,
                not_followed: m.not_followed,
                deselected: m.deselected,
                ..Facts::default()
            };
            seen.insert(
                unit,
                Seen {
                    facts,
                    reads,
                    duration_ms: m.duration_ms,
                    tests: m.tests,
                },
            );
        }
        let own: BTreeMap<String, Vec<String>> = own
            .into_iter()
            .filter(|(unit, _)| seen.contains_key(unit))
            .map(|(unit, mut files)| {
                files.sort();
                files.dedup();
                (unit, files)
            })
            .collect();
        let unit_files: BTreeSet<String> = own.values().flatten().cloned().collect();
        let sets = units::classify(
            at.tree.paths(),
            &|p| unit_files.contains(p),
            Sources::Named(&named, SOURCE_KINDS),
            at.ignore,
        );
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = seen
            .iter()
            .filter(|(_, s)| s.facts.not_followed.is_empty())
            .filter_map(|(unit, s)| {
                let compiled: BTreeSet<String> = s
                    .reads
                    .iter()
                    .filter(|f| build::is_source(f))
                    .cloned()
                    .collect();
                let lists = super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    Some(&compiled),
                )?;
                Some((unit.clone(), lists))
            })
            .collect();
        Ok(Some(Read {
            seen,
            failures_by_unit: true,
            sets,
            planned,
            own,
            unit_files: Some(unit_files),
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras::default()
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        units::classify(
            at.tree.paths(),
            &|p| at.map.unit_files.contains(p),
            Sources::Named(&at.map.sources, SOURCE_KINDS),
            at.ignore,
        )
    }

    fn why_is_unit(&self, at: &WhyAt<'_>, _: &Sets, unit: &str) -> bool {
        at.map.units.contains_key(unit) || at.shared.knows(unit)
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!(
            "{unit} isn't a test program {check} runs as far as this machine knows: its last ctest here didn't list it"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("its test programs and what each is built from are those of its last ctest here")
    }

    fn why_asks_hints(&self, map: &super::store::Map) -> bool {
        !map.sources.is_empty()
    }

    fn why_no_path(&self) -> Option<&'static str> {
        Some("its test program is built from it: its build directory's records name it")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-49
    #[test]
    fn ctest_by_name_is_recognized_and_the_forms_that_bypass_path_say_why() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        assert!(matches!(
            recognize(&words("ctest --test-dir build -j8"), cwd),
            Some(Ok(Some(args))) if args == words("--test-dir build -j8")
        ));
        for refused in [
            "/opt/cmake/bin/ctest --test-dir build",
            "cmake --build build --target test",
            "cmake --build build -t test",
            "cmake --build build --target=test",
        ] {
            assert!(
                matches!(recognize(&words(refused), cwd), Some(Err(_))),
                "{refused}"
            );
        }
        std::fs::create_dir_all(cwd.join("build")).unwrap();
        std::fs::write(cwd.join("build/CMakeCache.txt"), "").unwrap();
        assert!(matches!(
            recognize(&words("make -C build test"), cwd),
            Some(Err(_))
        ));
        assert!(matches!(
            recognize(&words("ninja -C build test"), cwd),
            Some(Err(_))
        ));
        assert!(recognize(&words("ninja -C build"), cwd).is_none());
        // A shell line, a script and a Makefile that start ctest by name; not one that runs pytest.
        let sh = |line: &str| vec!["sh".to_string(), "-c".to_string(), line.to_string()];
        assert!(matches!(
            recognize(&sh("cmake --build build && cd build && ctest -j8"), cwd),
            Some(Ok(None))
        ));
        assert!(recognize(&sh("ctest && pytest"), cwd).is_none());
        assert!(recognize(&sh("cmake --build build"), cwd).is_none());
        std::fs::write(
            cwd.join("check.sh"),
            "#!/bin/sh\nset -e\nctest --test-dir build\n",
        )
        .unwrap();
        assert!(matches!(
            recognize(&words("./check.sh"), cwd),
            Some(Ok(None))
        ));
        std::fs::write(cwd.join("Makefile"), "check:\n\tctest --test-dir build\n").unwrap();
        assert!(matches!(
            recognize(&words("make check"), cwd),
            Some(Ok(None))
        ));
        assert!(recognize(&words("go test ./..."), cwd).is_none());
        assert!(recognize(&words("pytest"), cwd).is_none());
    }
}
