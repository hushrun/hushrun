//! \[ADR 0122\] The ExUnit adapter, observed: for `mix test`, an Erlang module embedded in the
//! binary, compiled once with the repository's `erlc` into hushrun's cache directory, and loaded
//! into every Erlang VM of COMMAND through `ERL_LIBS` and `ELIXIR_ERL_OPTIONS` (C-114). Its unit
//! is a test file, by Mix's default rule (C-114); what each read comes from Elixir's lexical
//! tracker, Mix's compile manifests and call counters (C-115); it leaves out what the plan lists
//! through ExUnit's own `exclude` filter (C-116).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_core::units::{self, Facts, Set, Sets, Sources, Tree};

use super::Seen;
use super::adapter::{
    Adapter, Hints, KeysOutside, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt,
    WhyAt,
};
use super::shared::Shared;
use super::store::Map;

/// The module's source, compiled into the cache directory with the repository's `erlc`.
const SOURCE: &str = include_str!("hushrun_exunit.erl");
/// The module's name: its folder in `ERL_LIBS` and what `ELIXIR_ERL_OPTIONS` starts.
pub const MODULE: &str = "hushrun_exunit";
/// The word its facts files, this machine's store and a hint name it by (C-36, C-38).
pub const WORD: &str = "exunit";
/// \[C-114\] The kinds of file whose reads it observes: what Mix compiles and evaluates.
pub const SOURCE_KINDS: &[&str] = &[".ex", ".exs", ".erl"];
/// \[C-114\] What `ELIXIR_ERL_OPTIONS` gains: a start that can't stop the VM, even where the
/// module can't load (an `erl` older than the `erlc` that compiled it). No space, no glob: the
/// `elixir` script splits the variable on blanks unquoted.
pub const START: &str = "-eval catch(hushrun_exunit:start()).";
/// \[C-114\] The options of `mix test` that choose test files from an earlier run, watch, or
/// replace ExUnit's formatters, hushrun's among them.
const NOT_OFFERED: &[(&str, &str)] = &[
    (
        "--stale",
        "`mix test --stale` chooses test files from its own earlier runs",
    ),
    (
        "--failed",
        "`mix test --failed` chooses tests from its own earlier runs",
    ),
    (
        "--listen-on-stdin",
        "`mix test --listen-on-stdin` runs again and again",
    ),
    (
        "--formatter",
        "`mix test --formatter` replaces ExUnit's formatters, hushrun's among them",
    ),
];

/// \[C-114\] The adapter for a COMMAND that runs `mix test`.
#[derive(Debug, Clone, Copy)]
pub struct ExUnit;

/// \[C-114\] `mix test`, or `mix test` after `env NAME=value` and `MIX_ENV=test`, from any
/// folder; `mix do …`, an alias of another name and `iex -S mix test` aren't.
pub fn recognized(command: &[String], _cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    Some(recognize(command)?.map(|found| Box::new(found) as Box<dyn Adapter>))
}

/// What `command` is for the adapter: `None` when it doesn't run `mix test`; `Some(Err(why))`
/// when it does, in a way hushrun doesn't observe, which the run says.
pub fn recognize(command: &[String]) -> Option<Result<ExUnit, String>> {
    let command = super::command::without_env(command);
    let (program, rest) = command.split_first()?;
    let name = super::command::program_name(program);
    if name != "mix" {
        return None;
    }
    let task = rest.iter().find(|word| !word.starts_with('-'))?;
    if task != "test" {
        return None;
    }
    for word in rest {
        let option = word.split('=').next().unwrap_or(word);
        if let Some((_, why)) = NOT_OFFERED.iter().find(|(name, _)| *name == option) {
            return Some(Err((*why).to_string()));
        }
    }
    Some(Ok(ExUnit))
}

/// Compiles the module into `<cache dir>/adapters/exunit-<hash>/hushrun_exunit/ebin`, unless it
/// is there, and returns the folder `ERL_LIBS` names. The hash holds the source and the `erlc`
/// that compiles it, so that another Erlang/OTP gets a module of its own.
pub fn install(cache: &Path, erlc: &Path) -> Result<PathBuf, String> {
    let compiler = std::fs::canonicalize(erlc).unwrap_or_else(|_| erlc.to_path_buf());
    let stamp = std::fs::metadata(&compiler)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let digest =
        hushrun_core::Digest::of(format!("{SOURCE}\0{}\0{stamp}", compiler.display()).as_bytes())
            .hex();
    let lib = cache
        .join("adapters")
        .join(format!("exunit-{}", &digest[..16]));
    let ebin = lib.join(MODULE).join("ebin");
    let beam = ebin.join(format!("{MODULE}.beam"));
    if beam.is_file() {
        return Ok(lib);
    }
    let work =
        cache
            .join("adapters")
            .join(format!(".exunit-{}-{}", &digest[..16], std::process::id()));
    std::fs::create_dir_all(&work).map_err(|err| err.to_string())?;
    let source = work.join(format!("{MODULE}.erl"));
    std::fs::write(&source, SOURCE).map_err(|err| err.to_string())?;
    let output = std::process::Command::new(erlc)
        .arg("+deterministic")
        .arg("-o")
        .arg(&work)
        .arg(&source)
        .env_remove(super::runner::UNITS_VAR)
        .output()
        .map_err(|err| format!("{}: {err}", erlc.display()))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!(
            "erlc failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    std::fs::create_dir_all(&ebin).map_err(|err| err.to_string())?;
    let built = work.join(format!("{MODULE}.beam"));
    // Another hushrun may have put it there meanwhile: its bytes are these.
    std::fs::rename(&built, &beam).map_err(|err| err.to_string())?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(lib)
}

/// `erlc` on `path`, the `PATH` COMMAND gets.
pub fn erlc(path: Option<OsString>) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["erlc.exe", "erlc.cmd", "erlc.bat"]
    } else {
        &["erlc"]
    };
    std::env::split_paths(&path?)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// \[C-114\] What COMMAND's environment gains: the module's folder first in `ERL_LIBS`, and its
/// start last in `ELIXIR_ERL_OPTIONS`, each keeping what it had.
pub fn variables(
    lib: &Path,
    erl_libs: Option<OsString>,
    options: Option<OsString>,
) -> Vec<(String, OsString)> {
    let mut libs = lib.as_os_str().to_os_string();
    if let Some(existing) = erl_libs.filter(|value| !value.is_empty()) {
        libs.push(if cfg!(windows) { ";" } else { ":" });
        libs.push(existing);
    }
    let mut opts = options
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    if !opts.is_empty() {
        opts.push(" ");
    }
    opts.push(START);
    vec![
        ("ERL_LIBS".to_string(), libs),
        ("ELIXIR_ERL_OPTIONS".to_string(), opts),
    ]
}

/// \[C-114\] Mix's unit rule, from the files alone: a file whose name ends `_test.exs`, below the
/// `test` folder of a Mix project, a folder with a `mix.exs` (the root, an umbrella's app, a path
/// dependency). `None` when that project's `mix.exs` sets `test_paths` or `test_pattern`, which
/// this version doesn't read.
pub fn is_test_file(root: &Path, tree: &Tree, path: &str) -> Option<bool> {
    if !path.ends_with("_test.exs") {
        return Some(false);
    }
    let parts: Vec<&str> = path.split('/').collect();
    for (i, part) in parts.iter().enumerate().take(parts.len().saturating_sub(1)) {
        if *part != "test" {
            continue;
        }
        let project = parts[..i].join("/");
        let mix = if project.is_empty() {
            "mix.exs".to_string()
        } else {
            format!("{project}/mix.exs")
        };
        if tree.contains(&mix) {
            let text = std::fs::read_to_string(root.join(&mix)).unwrap_or_default();
            let custom = text.contains("test_paths") || text.contains("test_pattern");
            return (!custom).then_some(true);
        }
    }
    Some(false)
}

fn sets_of(tree: &Tree, root: &Path, ignore: &[hushrun_core::glob::Pattern]) -> Sets {
    units::classify(
        tree.paths(),
        &|path| is_test_file(root, tree, path) == Some(true),
        Sources::Kinds(SOURCE_KINDS),
        ignore,
    )
}

fn speaks_of_test_files(map: &Map, shared: &Shared) -> bool {
    map.reported() || shared.names_any()
}

/// What the run says when no VM wrote anything for hushrun.
const NOTHING: &str = "ExUnit wrote nothing for hushrun: no Erlang VM of COMMAND loaded its \
     adapter (a script that sets ERL_LIBS or ELIXIR_ERL_OPTIONS instead of adding to them, an \
     `erl` older than the `erlc` on PATH), its test helper's `ExUnit.start(formatters: …)` left \
     out hushrun's formatter, or no suite ran (a compile error)";

impl Adapter for ExUnit {
    fn word(&self) -> &'static str {
        WORD
    }

    fn runner(&self) -> &'static str {
        "ExUnit"
    }

    // `test_file`, `tests`, `hints`, `binds`, `keys`, `start_variables`, `not_by_test` and
    // `not_watched` are the trait's defaults, written out: a call on this type then names this
    // impl's own method, which the static test map follows to it alone, where the trait's would
    // count every adapter that overrides it (CONTRIBUTING.md, "Which tests CI runs again").
    fn test_file(&self) -> &'static str {
        "test file"
    }

    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: false,
            reads: false,
        }
    }

    fn hints(&self) -> Hints {
        Hints::Sources
    }

    fn binds(&self) -> bool {
        false
    }

    fn keys(&self) -> KeysOutside {
        KeysOutside::None
    }

    fn start_variables(&self, run_dir: &Path) -> Vec<(String, Option<OsString>)> {
        let _ = run_dir;
        Vec::new()
    }

    fn not_by_test(&self) -> Option<&'static str> {
        None
    }

    fn not_watched(&self) -> Option<&'static str> {
        None
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let erlc = erlc((at.env)("PATH"))
            .ok_or("can't compile its ExUnit adapter: there's no erlc on PATH")?;
        let lib = install(at.cache, &erlc)
            .map_err(|err| format!("can't compile its ExUnit adapter: {err}"))?;
        Ok(variables(
            &lib,
            (at.env)("ERL_LIBS"),
            (at.env)("ELIXIR_ERL_OPTIONS"),
        ))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let sets = sets_of(at.tree, at.root, at.ignore);
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        if !speaks_of_test_files(at.map, &shared) {
            return Ok(None);
        }
        let known = sets
            .files(Set::Units)
            .map(|unit| {
                let lists = super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    None,
                    None,
                );
                (unit.to_string(), lists)
            })
            .collect();
        Ok(Some(Planning {
            sets,
            known,
            received: shared.received,
            ..Planning::default()
        }))
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        let merged = super::facts::merged(at.files, WORD);
        if merged.is_empty() {
            return if at.files.iter().any(|f| f.adapter == WORD) {
                Ok(None)
            } else {
                Err(NOTHING.into())
            };
        }
        let failures_by_unit = merged.values().all(|m| m.failures_by_unit);
        // \[C-114\] A file ExUnit ran that Mix's default rule doesn't name (a project's own
        // `test_paths`) stays, not followed: it always runs.
        let seen: BTreeMap<String, Seen> = merged
            .into_iter()
            .map(|(unit, m)| {
                let mut not_followed = m.not_followed;
                if is_test_file(at.root, at.tree, &unit) != Some(true) {
                    not_followed.insert(units::NotFollowed {
                        why: "test-paths".into(),
                        detail: "Mix's default rule (`test/**/*_test.exs` beside a mix.exs) doesn't name it".into(),
                    });
                }
                let facts = Facts {
                    complete: m.complete,
                    failed: m.failed,
                    not_followed,
                    deselected: m.deselected,
                    skipped: m.skipped > 0,
                    ..Facts::default()
                };
                let seen = Seen {
                    outside: Default::default(),
                    declared: None,
                    getenv: Default::default(),
                    facts,
                    reads: m.reads,
                    duration_ms: m.duration_ms,
                    tests: m.tests,
                };
                (unit, seen)
            })
            .collect();
        let sets = sets_of(at.tree, at.root, at.ignore);
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = seen
            .keys()
            .filter_map(|unit| {
                let lists = super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    None,
                    None,
                )?;
                Some((unit.clone(), lists))
            })
            .collect();
        Ok(Some(Read {
            seen,
            failures_by_unit,
            sets,
            planned,
            edges: super::facts::edges_of(at.files, WORD),
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras::default()
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        sets_of(at.tree, at.root, at.ignore)
    }

    fn why_not_a_unit(&self, at: &WhyAt<'_>, check: &str, unit: &str) -> String {
        if !at.map.reported() && at.shared.hints.is_empty() {
            return format!(
                "{unit} isn't a test file of {check} as far as this machine knows: no run of {check} here has had ExUnit report its test files"
            );
        }
        format!(
            "{unit} isn't a test file of {check}: Mix's default rule names a file ending _test.exs below the test folder of a project with a mix.exs, whose mix.exs sets neither test_paths nor test_pattern"
        )
    }
}

#[cfg(test)]
mod tests;
