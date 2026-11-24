//! \[ADR 0119\] The `meson test` adapter (C-102 to C-105): for C and C++ projects built with
//! Meson, a unit is a test program, the program a Meson test starts, and a test is one Meson
//! test. What a test program read is what its build folder says it is built from: Meson's builds
//! are Ninja's, read as CTest's are ([`super::ctest::build`]).
//!
//! - **Loading:** `meson test` takes nothing from the environment that lets hushrun in, so hushrun
//!   puts a `meson` of its own first on COMMAND's `PATH` ([`super::program`], [`run::Meson`]).
//!   Every `meson test` started by name reaches it.
//! - **Units:** a test program's name is its path from its build folder, with the folder's build
//!   type: `tests/t_add (debug)`. A test whose command isn't a program built here (`python3`, a
//!   script of the repository) is in a unit that is `not observed`.
//! - **Ids:** a test's name as `meson test --list` prints it.
//! - **The plan** keys each unit by what the map says it read last time, and writes the lists it
//!   keyed each unit by to a file in the run's directory, which hushrun's `meson` checks again
//!   before it leaves anything out.

mod run;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use hushrun_core::units::{self, Facts, Set, Sets, Sources};

use super::Seen;
use super::adapter::{Adapter, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt};
use super::command::{self, program_name};
use super::ctest::SOURCE_KINDS;
use super::ctest::build;
use super::facts;
use super::program;

pub use run::Meson;

/// The adapter's word, in its facts files, this machine's store and a hint.
pub const ADAPTER: &str = "meson";
/// The plan's file in the run's directory: what each unit's key was made from.
const PLAN_FILE: &str = "meson-plan.v1";
const PLAN_SCHEMA: &str = "hushrun.meson-plan/v1";

/// What the plan keyed each unit by, for hushrun's `meson` to check before it leaves any out.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PlanFile {
    schema: String,
    /// Each unit's lists, each the files its key holds.
    units: BTreeMap<String, Vec<Vec<String>>>,
    /// The scope's C and C++ files of the sources and units sets: what a key can hold.
    scope: Vec<String>,
}

/// Why nothing is seen of a `meson test` that hushrun's `meson` didn't stand in for.
const BYPASSED: &str = "no `meson test` reached hushrun's meson on PATH: one started by its path (or `ninja test`, which starts Meson by its path) goes round it; `meson test -C <build folder>` can be observed";

/// The `meson test` adapter for one COMMAND.
#[derive(Debug)]
pub struct Recognized {
    /// The run's directory, from [`Adapter::variables`], where the plan writes its file.
    dir: OnceLock<PathBuf>,
}

/// \[C-102\] The `meson test` adapter for a COMMAND that starts `meson test` by name: `meson test
/// …` itself, or a wrapper whose text starts `meson test` and no other runner
/// ([`command::wrapper_starts`]). `Some(Err)` for `meson test` started by its path, which bypasses
/// `PATH`. `None` for any other COMMAND.
pub fn recognized(command: &[String], cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    let found = recognize(command, cwd)?;
    Some(found.map(|()| {
        Box::new(Recognized {
            dir: OnceLock::new(),
        }) as Box<dyn Adapter>
    }))
}

fn recognize(command: &[String], cwd: &Path) -> Option<Result<(), String>> {
    let first = command.first()?;
    let test = command.get(1).is_some_and(|w| w == "test");
    match &*program_name(first) {
        "meson" if test && first.contains(['/', '\\']) => Some(Err(format!(
            "COMMAND starts meson by its path ({first}), which hushrun's meson on PATH can't stand in for: start it as `meson test`"
        ))),
        "meson" if test => Some(Ok(())),
        "meson" => None,
        _ => {
            if command::wrapper_starts(command, cwd, command::Runner::MesonTest) {
                return Some(Ok(()));
            }
            command::wrapper_starts_by_path(command, cwd, command::Runner::MesonTest).map(|path| {
                Err(format!(
                    "COMMAND starts meson by its path ({path}), which hushrun's meson on PATH can't stand in for: start it as `meson test`"
                ))
            })
        }
    }
}

/// Every C and C++ file of the scope: what a program whose build can't be read reads.
fn every_source(tree: &units::Tree) -> BTreeSet<String> {
    tree.paths()
        .filter(|p| build::is_source(p))
        .map(str::to_string)
        .collect()
}

impl Recognized {
    /// The plan from the map (and hints).
    fn planning(&self, at: &PlanAt<'_>) -> Planning {
        let sets = units::classify(
            at.tree.paths(),
            &|p| at.map.unit_files.contains(p),
            Sources::Named(&at.map.sources, SOURCE_KINDS),
            at.ignore,
        );
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let known = at
            .map
            .units
            .keys()
            .map(|unit| {
                let lists = super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    None,
                    None,
                );
                (unit.clone(), lists)
            })
            .collect();
        Planning {
            sets,
            known,
            received: shared.received,
            ..Planning::default()
        }
    }
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "Meson"
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
        let dir = program::make(at.dir, &exe, &["meson"])
            .map_err(|err| format!("can't put hushrun's meson on PATH: {err}"))?;
        let _ = self.dir.set(at.dir.to_path_buf());
        program::variables(&dir, (at.env)("PATH"))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        Ok(Some(self.planning(at)))
    }

    fn plan(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let planning = self.planning(at);
        // \[C-104\] What each unit's key holds, for hushrun's meson to check before it leaves any
        // out. A file that can't be written leaves nothing out.
        if let Some(dir) = self.dir.get() {
            let scope = at
                .tree
                .paths()
                .filter(|p| {
                    build::is_source(p)
                        && matches!(planning.sets.of(p), Some(Set::Sources | Set::Units))
                })
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
            return Err(format!("Meson wrote nothing for hushrun: {BYPASSED}"));
        }
        if mine.iter().all(|f| f.units.is_empty()) {
            let why = mine
                .iter()
                .find_map(|f| f.settings.get("notObserved").and_then(|v| v.as_str()))
                .unwrap_or("meson test ran no test");
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
        let in_source = |m: &facts::Merged| {
            m.not_followed
                .iter()
                .find(|n| n.why == "in-source build")
                .map(|n| n.detail.clone())
        };
        let in_source_why = merged.values().find_map(in_source);
        if let Some(why) = &in_source_why
            && merged.values().all(|m| in_source(m).is_some())
        {
            return Err(why.clone());
        }
        let mut seen: BTreeMap<String, Seen> = BTreeMap::new();
        let mut named: BTreeSet<String> = BTreeSet::new();
        for (unit, m) in merged {
            let mut reads = m.reads;
            // \[C-103\] Sources are what a build's graph gives some program, as for CTest.
            named.extend(reads.iter().filter(|f| build::is_source(f)).cloned());
            if every.contains(&unit) {
                reads.extend(all_sources.iter().cloned());
            }
            let facts = Facts {
                complete: m.complete,
                failed: m.failed,
                not_followed: m.not_followed,
                deselected: m.deselected,
                skipped: m.skipped > 0,
                ..Facts::default()
            };
            seen.insert(
                unit,
                Seen {
                    outside: Default::default(),
                    declared: None,
                    getenv: Default::default(),
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
                    None,
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
            refused: in_source_why,
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
            "{unit} isn't a test program {check} runs as far as this machine knows: its last meson test here didn't list it"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("its test programs and what each is built from are those of its last meson test here")
    }

    fn why_asks_hints(&self, map: &super::store::Map) -> bool {
        !map.sources.is_empty()
    }

    fn why_no_path(&self) -> Option<&'static str> {
        Some("its test program is built from it: its build folder's records name it")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-102
    #[test]
    fn meson_test_by_name_is_recognized_and_by_its_path_says_why() {
        let cwd = Path::new("/nonexistent");
        assert!(matches!(
            recognize(&words("meson test -C build"), cwd),
            Some(Ok(()))
        ));
        assert!(matches!(recognize(&words("meson test"), cwd), Some(Ok(()))));
        assert!(recognize(&words("meson setup build"), cwd).is_none());
        assert!(recognize(&words("meson compile -C build"), cwd).is_none());
        assert!(matches!(
            recognize(&words("/usr/bin/meson test -C build"), cwd),
            Some(Err(why)) if why.contains("by its path")
        ));
        // A shell line that starts meson test by name, and one by its path.
        let line = |text: &str| vec!["sh".to_string(), "-c".to_string(), text.to_string()];
        assert!(matches!(
            recognize(&line("meson setup b && meson test -C b"), cwd),
            Some(Ok(()))
        ));
        assert!(matches!(
            recognize(&line("/opt/meson/meson test -C b"), cwd),
            Some(Err(_))
        ));
        // One that names another runner too is pytest's, as C-32's rule says.
        assert!(recognize(&line("meson test -C b && pytest"), cwd).is_none());
        assert!(recognize(&line("ninja -C b"), cwd).is_none());
    }
}
