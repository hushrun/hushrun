//! \[ADR 0090, 0120\] The plan and the read of test programs built in a CMake build directory,
//! which CTest's adapter and the test-program adapter share: what each program is built from, the
//! plan's file hushrun's `ctest` checks, and what the facts of a run say each program read. Here,
//! beside the build directory's walk (`build.rs`, `select.rs`), not in `ctest.rs`, so that the
//! static test map follows a test-program test to CTest's build-directory code alone, never to
//! CTest's own adapter (ADR 0145).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use hushrun_core::units::{self, Facts, Set, Sources};

use super::super::Seen;
use super::super::adapter::{Hints, PlanAt, Planning, Read, ReadAt};
use super::super::facts;
use super::{Built, SOURCE_KINDS, build};

/// The plan's file in the run's directory: what each unit's key was made from.
pub(super) const PLAN_FILE: &str = "ctest-plan.v1";
pub(super) const PLAN_SCHEMA: &str = "hushrun.ctest-plan/v1";

/// What the plan keyed each unit by, for hushrun's `ctest` to check before it leaves any out.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct PlanFile {
    pub(crate) schema: String,
    /// Each unit's lists, each the files its key holds.
    pub(crate) units: BTreeMap<String, Vec<Vec<String>>>,
    /// The scope's C and C++ files of the sources and units sets: what a key can hold.
    pub(crate) scope: Vec<String>,
}

/// \[ADR 0090, 0120\] What a plan's walk says each unit is built from, for the units `programs`
/// names, each with whether every program of it is built here.
pub(crate) fn compiled_from(
    programs: &BTreeMap<&str, bool>,
    built: &BTreeMap<String, Built>,
    tree: &units::Tree,
) -> Compiled {
    let mut out = Compiled::default();
    let every: BTreeSet<String> = every_source(tree);
    for (&unit, &built_here) in programs {
        let Some(b) = built
            .get(unit)
            .filter(|b| built_here && b.refused.is_none())
        else {
            out.units.insert(unit.to_string(), None);
            continue;
        };
        // \[C-30\] Only what the graph gives a program is a source (see `read`); a stale
        // program's graph is the build's still, so its files keep their sets, and the run's
        // line says what `hushrun why` says of them, from the map's.
        out.named
            .extend(b.reads.iter().filter(|f| build::is_source(f)).cloned());
        out.unit_files.extend(b.own.iter().cloned());
        // A stale program ran old code: it isn't keyed, so it runs.
        if b.stale.is_some() {
            out.units.insert(unit.to_string(), None);
            continue;
        }
        let reads: BTreeSet<String> = if b.every_source {
            every.clone()
        } else {
            b.reads.clone()
        };
        out.units
            .insert(unit.to_string(), Some((reads, b.own.clone())));
    }
    out
}

/// The plan from the map (and hints), with what the build says now when it is known.
pub(crate) fn planning(at: &PlanAt<'_>, compiled: Option<&Compiled>) -> Planning {
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
    let shared = super::super::fetch(at.asker, Hints::Sources, &sets, at.tree, at.ignore);
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
                        super::super::shared::lists(
                            unit,
                            at.map.units.get(unit),
                            &shared,
                            at.tree,
                            Some(&compiled),
                            None,
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
                let lists = super::super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    None,
                    None,
                );
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

/// \[ADR 0090, 0120\] Writes, in the run's directory `dir`, the lists each unit's key was made
/// from and the scope's C and C++ files, for the check before anything is left out.
pub(crate) fn write_plan(dir: &Path, planning: &Planning, tree: &units::Tree) {
    let scope = tree
        .paths()
        .filter(|p| {
            build::is_source(p) && matches!(planning.sets.of(p), Some(Set::Sources | Set::Units))
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

/// What a unit's programs are built from, and of those its own files.
type Made = (BTreeSet<String>, BTreeSet<String>);

/// What a plan's walk says each unit is built from: `None` for one that can't be keyed.
#[derive(Debug, Default)]
pub(crate) struct Compiled {
    pub(super) units: BTreeMap<String, Option<Made>>,
    pub(super) named: BTreeSet<String>,
    pub(super) unit_files: BTreeSet<String>,
}

impl Compiled {
    /// \[ADR 0120\] Names `sources` as sources besides those of the units' programs.
    pub(crate) fn name(&mut self, sources: impl IntoIterator<Item = String>) {
        self.named.extend(sources);
    }
}

/// Every C and C++ file of the scope: what a program whose build can't be read reads.
pub(crate) fn every_source(tree: &units::Tree) -> BTreeSet<String> {
    tree.paths()
        .filter(|p| build::is_source(p))
        .map(str::to_string)
        .collect()
}

/// \[ADR 0090, 0120\] What the facts files of `word` in `files` say of test programs built here:
/// each program's tests, what its build says it read, and the sets and lists a plan of this run
/// would have keyed it by.
pub(crate) fn read_built(
    at: &ReadAt<'_>,
    files: &[facts::FactsFile],
    word: &str,
) -> Result<Option<Read>, String> {
    let mut own: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut every: BTreeSet<String> = BTreeSet::new();
    let mut built_sources: BTreeSet<String> = BTreeSet::new();
    for file in files.iter().filter(|f| f.adapter == word) {
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
        // \[ADR 0120\] The sources of the build's other programs, which a program run on its
        // own names.
        if let Some(list) = file.settings.get("sources").and_then(|v| v.as_array()) {
            built_sources.extend(list.iter().filter_map(|f| f.as_str().map(str::to_string)));
        }
    }
    let all_sources = every_source(at.tree);
    let merged = facts::merged(files, word);
    // \[ADR 0090\] An in-source build: said once in the run. When every program is of one,
    // nothing of it can be observed, and the run says so; else each such program isn't.
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
        // \[C-30\] Sources are what a build's graph gives some program: a file of a source
        // kind no program is built from is in the rest set, and a change to it runs
        // everything. A program whose build couldn't be read reads every source, which
        // doesn't make every file of a source kind a source.
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
    named.extend(built_sources.into_iter().filter(|f| at.tree.contains(f)));
    let unit_files: BTreeSet<String> = own.values().flatten().cloned().collect();
    let sets = units::classify(
        at.tree.paths(),
        &|p| unit_files.contains(p),
        Sources::Named(&named, SOURCE_KINDS),
        at.ignore,
    );
    let shared = super::super::fetch(at.asker, Hints::Sources, &sets, at.tree, at.ignore);
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
            let lists = super::super::shared::lists(
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
