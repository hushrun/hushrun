//! \[ADR 0118\] The `dotnet test` adapter, compiler-given (C-30, C-32, C-98): for .NET projects
//! tested through VSTest (xUnit, NUnit, MSTest and any other VSTest adapter, in C#, F# or Visual
//! Basic), a unit is a **test project** built for one target framework and configuration, and a
//! test is one test method, by its fully qualified name. What a test project read is what MSBuild
//! says it and the projects it references compile.
//!
//! - **Loading:** `dotnet test` takes the .NET SDK's VSTest target from the file the `VSTestTargets`
//!   property names, which MSBuild reads from the environment. hushrun names a file of its own
//!   ([`TARGETS`]), which imports the SDK's and replaces its `VSTest` target in each test project
//!   ([`VSTEST_TARGETS`]): COMMAND stays byte for byte as written, and every `dotnet test` it
//!   starts, a script's too, is observed.
//! - **Units:** `tests/Calc.Tests/Calc.Tests.csproj (net8.0, Debug)`: the project file's path from
//!   the root, its target framework and configuration.
//! - **Ids:** a test's `<class>.<method>` as VSTest's TRX file names it, its fully qualified name;
//!   a theory's or a data-driven test's rows are one test.
//! - **The plan** keys each unit by what the map says it read last time. It writes the lists it
//!   keyed each unit by to a file of its own in the run's directory, which hushrun checks again
//!   against what MSBuild says the project compiles now, before it leaves anything out
//!   ([`run`]).

mod run;
mod trx;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use hushrun_core::units::{self, Facts, Set, Sets, Sources};

use super::Seen;
use super::adapter::{Adapter, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt};
use super::command::program_name;
use super::facts;

pub use run::from_args;

/// The adapter's word, in its facts files, this machine's store and a hint.
pub const ADAPTER: &str = "dotnet";
/// The suffixes of the files a .NET project compiles that are sources (C-30): C#, F#, Visual Basic.
pub const SOURCE_KINDS: &[&str] = &[".cs", ".fs", ".fsi", ".vb"];
/// hushrun's stand-in for the SDK's `Microsoft.TestPlatform.targets`.
pub const TARGETS: &str = include_str!("hushrun_dotnet.targets");
/// hushrun's `VSTest` target, which [`TARGETS`] imports in a run of hushrun's.
pub const VSTEST_TARGETS: &str = include_str!("hushrun_dotnet_vstest.targets");
/// The variable MSBuild reads the VSTest targets' file from.
pub const VSTEST_TARGETS_VAR: &str = "VSTestTargets";
/// The variable that names hushrun for the targets' `Exec`.
pub const EXE_VAR: &str = "HUSHRUN_DOTNET";
/// The variable that keeps a `VSTestTargets` COMMAND's environment had, which hushrun's imports.
pub const WAS_VAR: &str = "HUSHRUN_DOTNET_VSTEST_TARGETS";
/// The plan's file in the run's directory: what each unit's key was made from.
const PLAN_FILE: &str = "dotnet-plan.v1";
const PLAN_SCHEMA: &str = "hushrun.dotnet-plan/v1";

/// What the plan keyed each unit by, for hushrun's VSTest target to check before it leaves any
/// out.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PlanFile {
    schema: String,
    /// Each unit's lists, each the files its key holds.
    units: BTreeMap<String, Vec<Vec<String>>>,
    /// The scope's source files of the sources and units sets: what a key can hold.
    scope: Vec<String>,
}

/// Whether `path` is of a source kind.
pub fn is_source(path: &str) -> bool {
    SOURCE_KINDS.iter().any(|k| path.ends_with(k))
}

/// The `dotnet test` adapter for one COMMAND.
#[derive(Debug)]
pub struct Recognized {
    /// The run's directory, from [`Adapter::variables`], where the plan writes its file.
    dir: OnceLock<PathBuf>,
}

/// \[ADR 0118\] The adapter for `dotnet test …` (C-98). `Some(Err)` for one that runs no MSBuild:
/// `dotnet test` of an assembly, and the SDK's own Microsoft.Testing.Platform mode. `None` for any
/// other COMMAND.
pub fn recognized(command: &[String], cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    let found = recognize(command, cwd)?;
    Some(found.map(|()| {
        Box::new(Recognized {
            dir: OnceLock::new(),
        }) as Box<dyn Adapter>
    }))
}

/// What `command` in `cwd` is for the `dotnet test` adapter, without the adapter itself: `None`
/// when it isn't `dotnet test`, `Some(Err(why))` when it runs no MSBuild, which the run says.
pub fn recognize(command: &[String], cwd: &Path) -> Option<Result<(), String>> {
    let first = command.first()?;
    if program_name(first) != "dotnet" || command.get(1).map(String::as_str) != Some("test") {
        return None;
    }
    let args = &command[2..];
    let before_dashes = args.iter().take_while(|a| *a != "--");
    for arg in before_dashes {
        let lower = arg.to_ascii_lowercase();
        if !arg.starts_with('-') && (lower.ends_with(".dll") || lower.ends_with(".exe")) {
            return Some(Err(format!(
                "dotnet test {arg} runs an assembly with VSTest and no MSBuild, which hushrun's VSTest target can't stand in for: test its project or solution"
            )));
        }
    }
    if let Some(global) = testing_platform_global_json(cwd) {
        return Some(Err(format!(
            "{} sets dotnet test to run on Microsoft.Testing.Platform, whose results hushrun doesn't read yet",
            global.display()
        )));
    }
    Some(Ok(()))
}

/// \[ADR 0118\] The `global.json` above `cwd` that sets `dotnet test`'s runner to
/// Microsoft.Testing.Platform (.NET 10 and later), which never starts the VSTest target.
fn testing_platform_global_json(cwd: &Path) -> Option<PathBuf> {
    let found = cwd
        .ancestors()
        .map(|dir| dir.join("global.json"))
        .find(|p| p.is_file())?;
    let text = std::fs::read_to_string(&found).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let runner = value.get("test")?.get("runner")?.as_str()?;
    runner
        .eq_ignore_ascii_case("Microsoft.Testing.Platform")
        .then_some(found)
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
        "dotnet test"
    }

    fn test_file(&self) -> &'static str {
        "test project"
    }

    fn tests(&self) -> super::adapter::KnowsTests {
        super::adapter::KnowsTests {
            unit: true,
            reads: false,
        }
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let exe = at.exe()?;
        if exe.to_string_lossy().contains('"') {
            return Err(format!(
                "hushrun's path holds a quote ({}), which MSBuild's Exec can't start",
                exe.display()
            ));
        }
        let dir = at.dir.join("dotnet-targets");
        let targets = dir.join("hushrun_dotnet.targets");
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&targets, TARGETS))
            .and_then(|()| {
                std::fs::write(dir.join("hushrun_dotnet_vstest.targets"), VSTEST_TARGETS)
            })
            .map_err(|err| format!("can't write hushrun's VSTest targets: {err}"))?;
        let _ = self.dir.set(at.dir.to_path_buf());
        let mut out = vec![
            (VSTEST_TARGETS_VAR.to_string(), targets.into_os_string()),
            (EXE_VAR.to_string(), exe.into_os_string()),
        ];
        // \[ADR 0118\] A file COMMAND's environment named stays the one imported.
        if let Some(was) = (at.env)(VSTEST_TARGETS_VAR).filter(|v| !v.is_empty()) {
            out.push((WAS_VAR.to_string(), was));
        }
        Ok(out)
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        Ok(Some(self.planning(at)))
    }

    fn plan(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let planning = self.planning(at);
        // What each unit's key holds, for hushrun's VSTest target to check before it leaves any
        // out. A file that can't be written leaves nothing out.
        if let Some(dir) = self.dir.get() {
            let scope = at
                .tree
                .paths()
                .filter(|p| {
                    is_source(p) && matches!(planning.sets.of(p), Some(Set::Sources | Set::Units))
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
            return Err(
                "dotnet test wrote nothing for hushrun: no test project reached hushrun's VSTest target (`dotnet test` of an assembly, `dotnet vstest` and Microsoft.Testing.Platform run without it)"
                    .to_string(),
            );
        }
        if mine.iter().all(|f| f.units.is_empty()) {
            let why = mine
                .iter()
                .find_map(|f| f.settings.get("notObserved").and_then(|v| v.as_str()))
                .unwrap_or("dotnet test ran no test project");
            return Err(why.to_string());
        }
        let mut own: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in &mine {
            if let Some(map) = file.settings.get("own").and_then(|v| v.as_object()) {
                for (unit, files) in map {
                    let files = files.as_array().into_iter().flatten();
                    own.entry(unit.clone())
                        .or_default()
                        .extend(files.filter_map(|f| f.as_str().map(str::to_string)));
                }
            }
        }
        let merged = facts::merged(at.files, ADAPTER);
        let mut seen: BTreeMap<String, Seen> = BTreeMap::new();
        let mut named: BTreeSet<String> = BTreeSet::new();
        for (unit, m) in merged {
            // \[C-30\] Sources are what some test project compiles: a file of a source kind none
            // compiles is in the rest set, and a change to it runs everything.
            named.extend(m.reads.iter().filter(|f| is_source(f)).cloned());
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
                    reads: m.reads,
                    duration_ms: m.duration_ms,
                    tests: m.tests,
                },
            );
        }
        let own: BTreeMap<String, Vec<String>> = own
            .into_iter()
            .filter(|(unit, _)| seen.contains_key(unit))
            .map(|(unit, mut files)| {
                files.retain(|f| at.tree.contains(f));
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
                let compiled: BTreeSet<String> =
                    s.reads.iter().filter(|f| is_source(f)).cloned().collect();
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
            "{unit} isn't a test project {check} runs as far as this machine knows: its last dotnet test here didn't run it"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("its test projects and what each compiles are those of its last dotnet test here")
    }

    fn why_asks_hints(&self, map: &super::store::Map) -> bool {
        !map.sources.is_empty()
    }

    fn why_no_path(&self) -> Option<&'static str> {
        Some(
            "its test project compiles it: MSBuild names it among the project's or a referenced project's Compile items",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-98
    #[test]
    fn dotnet_test_is_recognized_and_the_forms_without_msbuild_say_why() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        for taken in [
            "dotnet test",
            "dotnet test Demo.sln -c Release --no-restore",
            "/usr/share/dotnet/dotnet test tests/A/A.csproj --filter Category=Fast",
            "dotnet.exe test -- RunConfiguration.MaxCpuCount=2",
        ] {
            assert!(
                matches!(recognize(&words(taken), cwd), Some(Ok(()))),
                "{taken}"
            );
        }
        for other in ["dotnet build", "dotnet vstest a.dll", "pytest", "dotnet"] {
            assert!(recognize(&words(other), cwd).is_none(), "{other}");
        }
        assert!(matches!(
            recognize(&words("dotnet test bin/Debug/net8.0/A.Tests.dll"), cwd),
            Some(Err(_))
        ));
        std::fs::write(
            cwd.join("global.json"),
            r#"{"sdk": {"version": "10.0.100"}, "test": {"runner": "Microsoft.Testing.Platform"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        let why = recognize(&words("dotnet test"), &cwd.join("src"))
            .unwrap()
            .unwrap_err();
        assert!(why.contains("Microsoft.Testing.Platform"), "{why}");
        std::fs::write(
            cwd.join("global.json"),
            r#"{"sdk": {"version": "8.0.100"}}"#,
        )
        .unwrap();
        assert!(matches!(
            recognize(&words("dotnet test"), cwd),
            Some(Ok(()))
        ));
    }
}
