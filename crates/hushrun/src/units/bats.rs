//! \[ADR 0115\] Bats (bats-core): a `.bats` test file is a unit, and each of its tests a test
//! (C-86 to C-89).
//!
//! An observed adapter. `BASH_ENV` names hushrun's hook ([`HOOK`]), which every bash process
//! COMMAND starts reads before its first command: bats' own processes, wherever bats was started
//! from (`bats`, `./test/bats/bin/bats`, `npx bats`), and every bash script a test runs, and what
//! those run, as long as they keep the environment. The hook notes, through a DEBUG trap (with
//! `set -T`, so in functions, subshells and command substitutions too), each file a command ran
//! from: the scripts bash ran, the files `source`, `.`, `load` and `bats_load_library` read, and
//! the helpers those loaded. bats' own DEBUG trap, and any a user sets, run after it.
//!
//! In bats' own processes the hook also says what each test file and test came to, from bats'
//! exit traps, and leaves out what `hushrun.json` lists by bats' own skip: a test file through the
//! skip bats has for every test of a file (no `setup_file`, no test process), a test by a `setup`
//! that skips and a `teardown` that does nothing. A reused test reads as `ok … # skip reused by
//! hushrun` in bats' output, so the count bats checks stays whole.
//!
//! When bats' main process ends, hushrun again ([`SUBCOMMAND`]) turns the hook's notes into one
//! facts file ([`facts`]); until then a `.started` file says the start didn't say what it ran.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use hushrun_core::units::{self, Facts, Set, Sets, Sources, Tree};

use super::Seen;
use super::adapter::{
    Adapter, Hints, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::command::program_name;
use super::facts::{self, FactsFile, RunFile, TestFacts, Touched, Unfollowed, UnitFacts};

/// The hook `BASH_ENV` names.
pub const HOOK: &str = include_str!("hushrun_bats.bash");
/// The word its facts files, this machine's store and a hint name it by.
pub const WORD: &str = "bats";
/// \[C-87\] What a unit is: a file bats runs by default from a directory it's given.
pub const UNIT_SUFFIX: &str = ".bats";
/// \[C-87\] The kinds of file that may be sources: bash's, when bash is what runs them.
pub const SOURCE_KINDS: &[&str] = &[".bash", ".sh"];
/// The hidden subcommand the hook starts hushrun with: never shown, never typed.
pub const SUBCOMMAND: &str = "__hushrun-bats";
/// The oldest bats-core whose processes the hook knows.
pub const OLDEST: (u64, u64) = (1, 10);

/// Variables the hook reads, which COMMAND's environment gains.
const EXE_VAR: &str = "HUSHRUN_BATS_EXE";
const ENV_WAS_VAR: &str = "HUSHRUN_BATS_ENV_WAS";

/// The Bats adapter.
#[derive(Debug, Clone, Copy)]
pub struct Bats;

/// Programs that start the program after them, with the options of theirs that come first.
const LAUNCHERS: &[(&str, &[&str])] = &[
    (
        "npx",
        &["--no-install", "--no", "-y", "--yes", "--quiet", "-q"],
    ),
    ("bunx", &[]),
    ("pnpx", &[]),
];

/// Whether `word` names bats: `bats`, or a path to it (`test/bats/bin/bats`).
fn is_bats(word: &str) -> bool {
    program_name(word) == "bats"
}

/// \[C-86\] Whether `command` starts bats: by its name or a path, or through `npx`, `bunx`,
/// `pnpx`, `pnpm [exec]`, `yarn [exec]`, `bun x` or `npm exec [--]`.
pub fn starts_bats(command: &[String]) -> bool {
    let Some(first) = command.first() else {
        return false;
    };
    if is_bats(first) {
        return true;
    }
    let name = program_name(first);
    let mut rest = &command[1..];
    if let Some((_, options)) = LAUNCHERS.iter().find(|(n, _)| *n == name) {
        while rest.first().is_some_and(|w| options.contains(&w.as_str())) {
            rest = &rest[1..];
        }
        return rest.first().is_some_and(|w| is_bats(w));
    }
    match &*name {
        "pnpm" | "yarn" => {
            if rest.first().is_some_and(|w| w == "exec") {
                rest = &rest[1..];
            }
        }
        "bun" => {
            if rest.first().is_none_or(|w| w != "x") {
                return false;
            }
            rest = &rest[1..];
        }
        "npm" => {
            if rest.first().is_none_or(|w| w != "exec") {
                return false;
            }
            rest = &rest[1..];
            if rest.first().is_some_and(|w| w == "--") {
                rest = &rest[1..];
            }
        }
        _ => return false,
    }
    rest.first().is_some_and(|w| is_bats(w))
}

/// \[C-86\] The registry's recognizer.
pub fn recognized(command: &[String], _cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    if !starts_bats(command) {
        return None;
    }
    if cfg!(windows) {
        return Some(Err(
            "hushrun observes Bats on Linux and macOS, not on Windows".into(),
        ));
    }
    Some(Ok(Box::new(Bats)))
}

/// Writes the hook to `<cache>/adapters/bats-<hash>/`, whole, when it isn't there.
pub fn install(cache: &Path) -> std::io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(HOOK.as_bytes()).hex();
    let path = cache
        .join("adapters")
        .join(format!("bats-{}", &digest[..16]))
        .join("hushrun_bats.bash");
    if std::fs::read(&path).ok().as_deref() != Some(HOOK.as_bytes()) {
        facts::write_whole(&path, HOOK.as_bytes())?;
    }
    Ok(path)
}

/// \[C-86\] What COMMAND's environment gains: `BASH_ENV`, the hook, which reads the one the
/// environment had (`HUSHRUN_BATS_ENV_WAS`) after itself, and the hushrun it starts again.
pub fn variables(hook: &Path, exe: &Path, was: Option<OsString>) -> Vec<(String, OsString)> {
    vec![
        ("BASH_ENV".to_string(), hook.as_os_str().to_os_string()),
        (ENV_WAS_VAR.to_string(), was.unwrap_or_default()),
        (EXE_VAR.to_string(), exe.as_os_str().to_os_string()),
    ]
}

/// \[C-87\] Whether a file whose name ends in `.sh` or `.bash`, with `head` its first bytes and
/// `executable` its mode, is bash's to run: a `#!` line that names bash (`/bin/bash`,
/// `/usr/bin/env bash`), or none at all in a file nobody can run as a program, which only a
/// shell's `source` reads. A `#!/bin/sh` script, or one another shell runs, isn't: no `BASH_ENV`
/// reaches it, and a change to it runs every test file.
pub fn bash_runs(head: &[u8], executable: bool) -> bool {
    let Some(line) = head.strip_prefix(b"#!") else {
        return !executable;
    };
    let line = line.split(|b| *b == b'\n').next().unwrap_or(line);
    let line = String::from_utf8_lossy(line);
    let mut words = line.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    let name = program.rsplit('/').next().unwrap_or(program);
    if name == "bash" {
        return true;
    }
    if name != "env" {
        return false;
    }
    // `env [-S] [NAME=value…] bash`.
    words
        .find(|w| !w.starts_with('-') && !w.contains('='))
        .is_some_and(|w| w == "bash")
}

/// \[C-87\] The sources of the scope, read from the files on disk: each `.bash` and `.sh` file
/// bash runs, by [`bash_runs`], and each file whose name has no `.` and whose `#!` line names
/// bash (`bin/tool`), a script the kernel starts bash for.
pub fn bash_files<'p>(root: &Path, paths: impl IntoIterator<Item = &'p str>) -> BTreeSet<String> {
    paths
        .into_iter()
        .filter(|p| {
            let kind = SOURCE_KINDS.iter().any(|k| p.ends_with(k));
            let bare = !p.rsplit('/').next().unwrap_or(p).contains('.');
            if !kind && !bare {
                return false;
            }
            let path = root.join(p);
            let head = read_head(&path);
            if kind {
                bash_runs(&head, is_executable(&path))
            } else {
                head.starts_with(b"#!") && bash_runs(&head, true)
            }
        })
        .map(str::to_string)
        .collect()
}

fn read_head(path: &Path) -> Vec<u8> {
    use std::io::Read as _;
    let mut head = vec![0u8; 256];
    let read = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    head.truncate(read);
    head
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_: &Path) -> bool {
    false
}

/// \[C-87\] Whether `path` is a unit: a `.bats` file.
pub fn is_test_file(path: &str) -> bool {
    path.ends_with(UNIT_SUFFIX)
}

/// \[C-87\] The four sets of `tree` for Bats.
pub fn sets(root: &Path, tree: &Tree, ignore: &[hushrun_core::glob::Pattern]) -> Sets {
    let named = bash_files(root, tree.paths());
    units::classify(
        tree.paths(),
        &is_test_file,
        Sources::Named(&named, &[""]),
        ignore,
    )
}

impl Adapter for Bats {
    fn word(&self) -> &'static str {
        WORD
    }

    fn runner(&self) -> &'static str {
        "Bats"
    }

    /// \[C-88\] The hook names each test, leaves out those `reuseTests` lists, says how each
    /// ended, and notes what each test's processes ran apart from its file's.
    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            reads: true,
        }
    }

    fn hints(&self) -> Hints {
        Hints::Sources
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let hook = install(at.cache).map_err(|err| format!("can't write its bash hook: {err}"))?;
        let exe = at.exe()?;
        Ok(variables(&hook, &exe, (at.env)("BASH_ENV")))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let sets = sets(at.root, at.tree, at.ignore);
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
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
        let merged = facts::merged(at.files, WORD);
        if merged.is_empty() {
            return Ok(None);
        }
        let failures_by_unit = merged.values().all(|m| m.failures_by_unit);
        let seen: BTreeMap<String, Seen> = merged
            .into_iter()
            .filter(|(unit, _)| is_test_file(unit))
            .map(|(unit, m)| {
                let facts = Facts {
                    complete: m.complete,
                    failed: m.failed,
                    not_followed: m.not_followed,
                    deselected: m.deselected,
                    skipped: m.skipped > 0,
                    ..Facts::default()
                };
                let seen = Seen {
                    facts,
                    reads: m.reads,
                    duration_ms: m.duration_ms,
                    tests: m.tests,
                    ..Seen::default()
                };
                (unit, seen)
            })
            .collect();
        let sets = sets(at.root, at.tree, at.ignore);
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
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras::default()
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        sets(at.root, at.tree, at.ignore)
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!("{unit} isn't a test file of {check}: Bats' test files end in .bats")
    }
}

// ---------------------------------------------------------------------------------------------
// The hidden subcommand: what the hook asks of hushrun.

/// \[ADR 0115\] hushrun as the hook starts it: `__hushrun-bats plan <units> <run>` writes the
/// lists `hushrun.json` gives in a form bash reads (`<run>/plan.bash`); `__hushrun-bats facts
/// <units> <run>` turns the hook's notes into the start's facts file. `None` for any other
/// command line.
pub fn from_args(args: &[OsString]) -> Option<ExitCode> {
    if args.get(1).map(OsString::as_os_str) != Some(SUBCOMMAND.as_ref()) {
        return None;
    }
    let (Some(what), Some(units), Some(run)) = (args.get(2), args.get(3), args.get(4)) else {
        eprintln!("error: {SUBCOMMAND} is hushrun's own, started by its bash hook");
        return Some(ExitCode::from(2));
    };
    let (units, run) = (Path::new(units), Path::new(run));
    let done = match what.to_str() {
        Some("plan") => write_plan(units, run),
        Some("facts") => write_facts(units, run),
        _ => Err("unknown".into()),
    };
    Some(match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    })
}

fn run_file(units: &Path) -> Result<RunFile, String> {
    let text = std::fs::read(units.join(facts::RUN_FILE)).map_err(|e| e.to_string())?;
    serde_json::from_slice(&text).map_err(|e| e.to_string())
}

/// Text in bash's `$'…'` quoting, every byte but letters, digits and `_./-` as `\xHH`, so that
/// it reads back as exactly these bytes.
pub fn quoted(text: &str) -> String {
    let mut out = String::from("$'");
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-') {
            out.push(b as char);
        } else {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out.push('\'');
    out
}

/// The roots a unit's path is taken from: the root as `hushrun.json` gives it, and with its
/// symlinks resolved.
fn roots(root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![root.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(root)
        && real != root
    {
        roots.push(real);
    }
    roots
}

/// \[C-88\] The bash the hook sources for what it may leave out: the roots, a function that
/// says whether a test file is reused whole, and one whether a test is, by its unit and id. A
/// case of quoted words matches each exactly, in bash 3.2 too.
pub fn plan_bash(run: &RunFile) -> String {
    let mut out = String::from("__hushrun_bats_roots=(");
    for root in roots(&run.root) {
        out.push(' ');
        out.push_str(&quoted(&root.to_string_lossy()));
    }
    out.push_str(" )\n");
    let case = |name: &str, words: Vec<String>| {
        let mut text = format!("{name}() {{\n");
        if !words.is_empty() {
            text.push_str("  case $1 in\n");
            for word in words {
                text.push_str(&format!("  {word}) return 0 ;;\n"));
            }
            text.push_str("  esac\n");
        }
        text.push_str("  return 1\n}\n");
        text
    };
    out.push_str(&case(
        "__hushrun_bats_reused_file",
        run.reuse.iter().map(|u| quoted(u)).collect(),
    ));
    let by_test = run.unit.as_deref() == Some("test");
    let tests = run
        .reuse_tests
        .iter()
        .filter(|_| by_test)
        .flat_map(|(unit, ids)| ids.iter().map(move |id| quoted(&format!("{unit}\x1f{id}"))))
        .collect();
    let mut text = case("__hushrun_bats_reused_one", tests);
    text.push_str(
        "__hushrun_bats_reused_test() { __hushrun_bats_reused_one \"$1\"$'\\x1f'\"$2\"; }\n",
    );
    out.push_str(&text);
    out
}

fn write_plan(units: &Path, run: &Path) -> Result<(), String> {
    let file = run_file(units)?;
    facts::write_whole(&run.join("plan.bash"), plan_bash(&file).as_bytes())
        .map_err(|e| e.to_string())
}

fn write_facts(units: &Path, run: &Path) -> Result<(), String> {
    let name = run
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("no run")?;
    let file = run_file(units)?;
    let facts = facts(&file, run, &name);
    let bytes = serde_json::to_vec(&facts).map_err(|e| e.to_string())?;
    facts::write_whole(&units.join(format!("{name}.json")), &bytes).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(units.join(format!("{name}{}", facts::STARTED_SUFFIX)));
    Ok(())
}

/// One of the hook's `key value` files: each line's first word and the rest.
fn fields(path: &Path) -> Option<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

/// The path `name`, from the directory `dir`, lexically: `.` and `..` taken away.
fn lexical(dir: &Path, name: &str) -> PathBuf {
    let joined = if Path::new(name).is_absolute() {
        PathBuf::from(name)
    } else {
        dir.join(name)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
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

/// What resolves a note to a file of the repository, from its root.
struct Resolver {
    roots: Vec<PathBuf>,
}

impl Resolver {
    /// The file a note names, from the root, when it is a file of the repository: as bash named
    /// it from the directory it was in, with its symlinks resolved.
    fn resolve(&self, dir: &str, name: &str) -> Option<String> {
        let path = lexical(Path::new(dir), name);
        let real = std::fs::canonicalize(&path).ok()?;
        if !real.is_file() {
            return None;
        }
        self.roots.iter().find_map(|root| {
            let rel = real.strip_prefix(root).ok()?;
            let text = rel.to_str()?.replace('\\', "/");
            (!text.is_empty()).then_some(text)
        })
    }

    /// The files of the repository a log of notes names.
    fn log(&self, path: &Path) -> BTreeSet<String> {
        let Ok(bytes) = std::fs::read(path) else {
            return BTreeSet::new();
        };
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .filter_map(|(dir, name)| self.resolve(dir, name))
            .collect()
    }
}

/// One try of one test, as the hook noted it.
#[derive(Debug, Default)]
struct Try {
    try_number: u64,
    id: Option<String>,
    reused: bool,
    ended: Option<(bool, bool)>,
    duration_ms: u64,
    reads: BTreeSet<String>,
}

/// One test file's bats-exec-file, as the hook noted it.
#[derive(Debug, Default)]
struct File {
    unit: String,
    tests: Vec<String>,
    reused: bool,
    failed: Option<String>,
    reads: BTreeSet<String>,
}

/// The parts of a version, `1.10.0`.
fn version(text: &str) -> Option<(u64, u64)> {
    let mut parts = text.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// \[C-88\] The facts file of one start of bats, from the hook's notes in `run`.
pub fn facts(file: &RunFile, run: &Path, start: &str) -> FactsFile {
    let resolver = Resolver {
        roots: roots(&file.root)
            .into_iter()
            .filter_map(|r| std::fs::canonicalize(&r).ok().or(Some(r)))
            .collect(),
    };
    let by_test = file.unit.as_deref() == Some("test");
    let mut out = FactsFile {
        schema: if by_test {
            facts::FACTS_SCHEMA_V1
        } else {
            facts::FACTS_SCHEMA
        }
        .to_string(),
        adapter: WORD.to_string(),
        failures_by_unit: true,
        start: Some(start.to_string()),
        ..FactsFile::default()
    };
    let suite_reads = resolver.log(&run.join("suite.log"));
    if let Ok(reason) = std::fs::read_to_string(run.join("suite.failed")) {
        out.failures_by_unit = false;
        out.not_kept = Some(format!(
            "{reason}, which no test file owns: no pass of the run is kept"
        ));
    }
    if let Some(found) = std::fs::read_to_string(run.join("version"))
        .ok()
        .as_deref()
        .and_then(version)
        && found < OLDEST
    {
        out.errors.push(format!(
            "bats {}.{} is older than the {}.{} hushrun knows",
            found.0, found.1, OLDEST.0, OLDEST.1
        ));
    }
    let mut files: Vec<File> = Vec::new();
    let mut tries: BTreeMap<(String, String), Vec<Try>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(run) else {
        out.errors.push("its notes can't be read".into());
        return out;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    for name in &names {
        let Some(stem) = name.strip_suffix(".meta") else {
            continue;
        };
        let Some(meta) = fields(&run.join(name)) else {
            continue;
        };
        let unit = meta.get("unit").cloned().unwrap_or_default();
        let at = |suffix: &str| run.join(format!("{stem}{suffix}"));
        if stem.starts_with("file-") {
            let tests = std::fs::read_to_string(at(".tests"))
                .unwrap_or_default()
                .lines()
                .map(str::trim_end)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            files.push(File {
                unit,
                tests,
                reused: meta.get("reused").is_some_and(|r| r == "1"),
                failed: std::fs::read_to_string(at(".failed")).ok(),
                reads: resolver.log(&at(".log")),
            });
        } else if stem.starts_with("test-") {
            // bats 1.11 and later end a test's name in its list with a space.
            let name = meta
                .get("name")
                .map(|n| n.trim_end().to_string())
                .unwrap_or_default();
            let number = |key: &str| meta.get(key).and_then(|v| v.parse::<u64>().ok());
            let end = fields(&at(".end"));
            let ended = end.as_ref().map(|end| {
                (
                    end.get("state").is_some_and(|s| s == "failed"),
                    end.get("skipped").is_some_and(|s| s == "1"),
                )
            });
            let duration_ms = match (
                number("start"),
                end.as_ref()
                    .and_then(|e| e.get("end"))
                    .and_then(|v| v.parse::<u64>().ok()),
            ) {
                (Some(start), Some(end)) if start > 0 => end.saturating_sub(start),
                _ => 0,
            };
            tries.entry((unit, name)).or_default().push(Try {
                try_number: number("try").unwrap_or(1),
                id: std::fs::read_to_string(at(".id")).ok(),
                reused: std::fs::read_to_string(at(".reused")).is_ok_and(|r| r == "1"),
                ended,
                duration_ms,
                reads: resolver.log(&at(".log")),
            });
        }
    }
    let mut units: BTreeMap<String, UnitFacts> = BTreeMap::new();
    let mut paths: Vec<String> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut indices = |set: &BTreeSet<String>| -> Vec<usize> {
        set.iter()
            .map(|p| {
                *index.entry(p.clone()).or_insert_with(|| {
                    paths.push(p.clone());
                    paths.len() - 1
                })
            })
            .collect()
    };
    for f in files {
        if f.unit.is_empty() {
            // A test file outside the repository: not one of the check's.
            continue;
        }
        let unit = units.entry(f.unit.clone()).or_insert_with(|| UnitFacts {
            unit: f.unit.clone(),
            deselected: true,
            ..UnitFacts::default()
        });
        let mut loaded: BTreeSet<String> = suite_reads.clone();
        loaded.extend(f.reads.iter().cloned());
        loaded.insert(f.unit.clone());
        unit.reads.extend(loaded.iter().cloned());
        // Reused whole only when each of its tests skipped as reused: one that ran is read as
        // run, and the file with it.
        let every_test_reused = f.tests.iter().all(|name| {
            tries
                .get(&(f.unit.clone(), name.clone()))
                .is_none_or(|all| all.iter().all(|t| t.reused))
        });
        if f.reused && every_test_reused {
            for name in &f.tests {
                tries.remove(&(f.unit.clone(), name.clone()));
            }
            continue;
        }
        unit.deselected = false;
        if let Some(reason) = &f.failed {
            unit.failed = true;
            unit.not_followed.push(Unfollowed {
                why: "file".into(),
                detail: format!("its {reason} failed"),
            });
        }
        let mut tests: Vec<TestFacts> = Vec::new();
        for name in &f.tests {
            let Some(mut all) = tries.remove(&(f.unit.clone(), name.clone())) else {
                // It never started: bats stopped before it.
                unit.selected += 1;
                continue;
            };
            all.sort_by_key(|t| t.try_number);
            let last = all.last().expect("one try at least");
            let reads: BTreeSet<String> =
                all.iter().flat_map(|t| t.reads.iter().cloned()).collect();
            unit.reads.extend(reads.iter().cloned());
            let Some(id) = last.id.clone() else {
                // It ended before bats named it (its file didn't load in its process).
                unit.selected += 1;
                unit.failed |= last.ended.is_some_and(|(failed, _)| failed);
                continue;
            };
            let mut test = TestFacts {
                id,
                deselected: last.reused,
                duration_ms: all.iter().map(|t| t.duration_ms).sum(),
                ..TestFacts::default()
            };
            if !last.reused {
                unit.selected += 1;
                if let Some((failed, skipped)) = last.ended {
                    test.finished = true;
                    test.failed = failed;
                    test.skipped = skipped && !failed;
                    unit.finished += 1;
                    unit.failed |= failed;
                    unit.skipped += u64::from(test.skipped);
                }
                if all.len() > 1 {
                    test.not_followed.push(Unfollowed {
                        why: "retried".into(),
                        detail: format!(
                            "bats ran it {} times (BATS_TEST_RETRIES): a pass after a failure proves nothing",
                            all.len()
                        ),
                    });
                }
                if !test.finished && !test.failed {
                    test.not_kept = Some("it didn't finish: bats stopped before its end".into());
                }
            }
            unit.duration_ms += test.duration_ms;
            if by_test {
                test.ran = Some(indices(&reads));
                test.opened = Some(Vec::new());
                test.dirs = Some(Vec::new());
            }
            tests.push(test);
        }
        // \[C-88\] An id two tests of the file share is never left out: neither keeps a key.
        let mut count: BTreeMap<&str, usize> = BTreeMap::new();
        for t in &tests {
            *count.entry(t.id.as_str()).or_default() += 1;
        }
        let shared: BTreeSet<String> = count
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(id, _)| id.to_string())
            .collect();
        for t in &mut tests {
            t.never_left_out |= shared.contains(&t.id);
        }
        if by_test {
            unit.loaded = Some(Touched {
                ran: indices(&loaded),
                opened: Vec::new(),
            });
            unit.tests.get_or_insert_with(Vec::new).extend(tests);
        }
    }
    for unit in units.values_mut() {
        unit.reads.sort();
        unit.reads.dedup();
    }
    out.paths = paths;
    out.units = units.into_values().collect();
    out
}

#[cfg(test)]
mod tests;
