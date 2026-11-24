//! \[ADR 0118\] hushrun inside `dotnet test`, started by the VSTest target of hushrun's for each test
//! project (`hushrun_dotnet_vstest.targets`), as `hushrun __hushrun-dotnet before|after <file>`.
//!
//! - **before**, ahead of the project's build: from `<file>.project`, what MSBuild says the project
//!   is and compiles, it names the unit, decides what to leave out, and writes `<file>.action`
//!   (`skip` or `run`), `<file>.loggers` (COMMAND's loggers and a TRX logger of hushrun's, one a
//!   line), `<file>.filter` (COMMAND's filter and hushrun's, as one VSTest test case filter) and
//!   `<file>.decision`, what `after` reads; and `<units>/dotnet-<id>.started`, so that a project
//!   whose `after` never ran records no pass (C-32).
//! - **after**, once the project's tests ran (or were skipped), whether MSBuild's call of them
//!   passed: it reads the TRX file and writes the project's facts, `<units>/dotnet-<id>.json`.
//!
//! Anything that can't be read leaves nothing out: `before` then writes no action, and the
//! project runs as COMMAND says, unobserved.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};

use super::super::facts::{self, FactsFile, RunFile, TestFacts, Unfollowed, UnitFacts};
use super::super::runner::UNITS_VAR;
use super::trx::{self, Outcome};
use super::{ADAPTER, PLAN_FILE, PLAN_SCHEMA, PlanFile, is_source};

/// The hidden subcommand: never shown, never typed.
pub const SUBCOMMAND: &str = "__hushrun-dotnet";

/// The longest test case filter hushrun adds: past it, nothing is left out one by one. VSTest
/// passes the filter to its console on one command line.
const FILTER_MOST: usize = 60_000;

/// Runs as one step of hushrun's VSTest target when `args` (the process's, program first) say so;
/// `None` for any other command line.
pub fn from_args(args: &[OsString]) -> Option<ExitCode> {
    if args.get(1).map(|a| a.as_os_str()) != Some(SUBCOMMAND.as_ref()) {
        return None;
    }
    let step = args.get(2).and_then(|a| a.to_str());
    let file = args.get(3).map(PathBuf::from);
    match (step, file) {
        (Some("before"), Some(file)) => before(&file),
        (Some("after"), Some(file)) => {
            let passed = args.get(4).and_then(|a| a.to_str()) == Some("true");
            after(&file, passed);
        }
        _ => {
            eprintln!(
                "error: {SUBCOMMAND} is a step of hushrun's VSTest target, started by MSBuild"
            );
            return Some(ExitCode::from(2));
        }
    }
    // A step never fails the build: what it couldn't do leaves nothing out.
    Some(ExitCode::SUCCESS)
}

/// What MSBuild wrote of a test project, `<file>.project`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Project {
    pub project: PathBuf,
    pub framework: String,
    pub configuration: String,
    pub assembly: PathBuf,
    /// `VSTestSessionCorrelationId`: one for each `dotnet test`.
    pub start: String,
    pub no_build: bool,
    /// `VSTestResultsDirectory`, where COMMAND's own TRX logger writes.
    pub results: Option<PathBuf>,
    /// The project runs on Microsoft.Testing.Platform (`TestingPlatformDotnetTestSupport`).
    pub testing_platform: bool,
    /// Its own Compile items.
    pub own: Vec<PathBuf>,
    /// What it and the projects it references compile, and their project files.
    pub sources: Vec<PathBuf>,
    /// MSBuild told the sources: its call of `HushrunSources` passed.
    pub sources_told: bool,
}

/// `<file>.project`'s lines, `name=value`.
pub fn parse_project(text: &str) -> Project {
    let mut p = Project::default();
    for line in text.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim_end_matches('\r');
        match name {
            "project" => p.project = PathBuf::from(value),
            "framework" => p.framework = value.to_string(),
            "configuration" => p.configuration = value.to_string(),
            "assembly" => p.assembly = PathBuf::from(value),
            "start" => p.start = value.to_string(),
            "nobuild" => p.no_build = value.eq_ignore_ascii_case("true"),
            "results" if !value.is_empty() => p.results = Some(PathBuf::from(value)),
            "testingplatform" => p.testing_platform = value.eq_ignore_ascii_case("true"),
            "own" if !value.is_empty() => p.own.push(PathBuf::from(value)),
            "source" if !value.is_empty() => p.sources.push(PathBuf::from(value)),
            "sources" => p.sources_told = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    p
}

/// \[ADR 0118\] A test project's unit: its project file's path from the root, `/`-separated, with
/// its target framework and configuration, `tests/Calc.Tests/Calc.Tests.csproj (net8.0, Debug)`,
/// so that two builds of one project never share a result. `None` outside the root.
pub fn unit_name(root: &Path, p: &Project) -> Option<String> {
    let rel = relative(root, &p.project)?;
    let mut detail: Vec<&str> = Vec::new();
    if !p.framework.is_empty() {
        detail.push(&p.framework);
    }
    if !p.configuration.is_empty() {
        detail.push(&p.configuration);
    }
    Some(if detail.is_empty() {
        rel
    } else {
        format!("{rel} ({})", detail.join(", "))
    })
}

/// `path` from `root`, `/`-separated, when it is under it: both as the file system has them.
pub fn relative(root: &Path, path: &Path) -> Option<String> {
    let real = std::fs::canonicalize(path).ok()?;
    let rel = real.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    let rel = parts?.join("/");
    (!rel.is_empty()).then_some(rel)
}

/// What `before` decided, for `after`: `<file>.decision`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Decision {
    unit: String,
    skip: bool,
    /// The tests hushrun's filter leaves out, by id.
    left_out: Vec<String>,
    reads: Vec<String>,
    own: Vec<String>,
    /// Report each test: the run's `unit` is `test`.
    tests: bool,
    start: String,
    assembly: PathBuf,
    root: PathBuf,
    /// \[ADR 0118\] COMMAND has a TRX logger of its own, which VSTest keeps alone: its file is read,
    /// the one written in this directory since `since` whose tests are in `assembly`.
    #[serde(default)]
    user_trx: Option<PathBuf>,
    #[serde(default)]
    since_ms: u64,
    /// What couldn't be followed of the project, for every test of it.
    #[serde(default)]
    not_followed: Vec<Unfollowed>,
}

/// \[ADR 0118\] Whether a test's id can be left out by an exact `FullyQualifiedName!=<id>`: VSTest
/// escapes its filter's operators with `\`, which MSBuild turns into `/` on Linux and macOS, so an
/// id that would need one (NUnit's `Zero(1)`, a test with `=` or `|` in its name) is never left
/// out one by one, and runs whenever its project does.
pub fn leaves_out_exactly(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '`' | '-'))
}

/// \[ADR 0118\] The filter that runs what COMMAND's filter runs, but the tests `left_out`.
pub fn filter(user: &str, left_out: &[String]) -> String {
    let ours: Vec<String> = left_out
        .iter()
        .map(|id| format!("FullyQualifiedName!={id}"))
        .collect();
    let ours = ours.join("&");
    match (user.trim().is_empty(), ours.is_empty()) {
        (true, _) => ours,
        (false, true) => user.trim().to_string(),
        (false, false) => format!("({})&({ours})", user.trim()),
    }
}

fn units_dir(file: &Path) -> Option<PathBuf> {
    std::env::var_os(UNITS_VAR)
        .map(PathBuf::from)
        .or_else(|| file.parent()?.parent().map(Path::to_path_buf))
}

fn id_of(file: &Path) -> String {
    file.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string()
}

fn with(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// The lines of a file MSBuild wrote one item a line, as one value: its items joined by `;`.
fn joined(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(";")
}

fn before(file: &Path) {
    let Some(units) = units_dir(file) else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(with(file, ".project")) else {
        return;
    };
    let project = parse_project(&text);
    let Some(run) = std::fs::read(units.join(facts::RUN_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<RunFile>(&b).ok())
    else {
        return;
    };
    let id = id_of(file);
    let Ok(root) = std::fs::canonicalize(&run.root) else {
        return;
    };
    let Some(unit) = unit_name(&root, &project) else {
        return;
    };
    let rel = |paths: &[PathBuf]| -> Vec<String> {
        let mut out: Vec<String> = paths.iter().filter_map(|p| relative(&root, p)).collect();
        out.sort();
        out.dedup();
        out
    };
    let reads = rel(&project.sources);
    let own = rel(&project.own);
    let mut not_followed = Vec::new();
    if !project.sources_told {
        not_followed.push(Unfollowed {
            why: "sources".into(),
            detail: "MSBuild didn't say what the project and the projects it references compile"
                .into(),
        });
    }
    // \[ADR 0118\] A project on Microsoft.Testing.Platform runs its own way: it is a unit not
    // observed, which reads what it compiles, and the run says once why.
    if project.testing_platform {
        let why = format!(
            "{} runs on Microsoft.Testing.Platform, whose results hushrun doesn't read yet: its tests aren't observed",
            relative(&root, &project.project)
                .unwrap_or_else(|| project.project.display().to_string())
        );
        let mut settings = BTreeMap::new();
        settings.insert("note".to_string(), serde_json::json!(why));
        settings.insert("own".to_string(), serde_json::json!({ unit.clone(): own }));
        let unit = UnitFacts {
            unit,
            selected: 1,
            reads,
            not_followed: vec![Unfollowed {
                why: "Microsoft.Testing.Platform".into(),
                detail: why,
            }],
            ..UnitFacts::default()
        };
        write_facts(
            &units,
            &id,
            &project.start,
            vec![unit],
            settings,
            false,
            None,
        );
        return;
    }
    let tests = run.unit.as_deref() == Some("test");
    let verified = not_followed.is_empty() && verify(&unit, &reads, &units);
    let skip = verified && run.reuse.contains(&unit);
    let mut left_out: Vec<String> = if verified && tests && !skip {
        run.reuse_tests
            .get(&unit)
            .into_iter()
            .flatten()
            .filter(|id| leaves_out_exactly(id))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let user_filter = joined(&with(file, ".userfilter"));
    let mut combined = filter(&user_filter, &left_out);
    if combined.len() > FILTER_MOST {
        left_out.clear();
        combined = filter(&user_filter, &left_out);
    }
    let mut loggers: Vec<String> = std::fs::read_to_string(with(file, ".userloggers"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .filter(|l| !l.is_empty())
        .collect();
    // \[ADR 0118\] VSTest keeps one TRX logger: COMMAND's own, when it has one, else hushrun's.
    let user_trx = loggers.iter().any(|l| is_trx(l)).then(|| {
        project.results.clone().unwrap_or_else(|| {
            project
                .project
                .parent()
                .unwrap_or(Path::new("."))
                .join("TestResults")
        })
    });
    if user_trx.is_none() {
        loggers.push(format!(
            "trx;LogFileName={}",
            with(file, ".trx").to_string_lossy()
        ));
    }
    let since_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let decision = Decision {
        unit,
        skip,
        left_out,
        reads,
        own,
        tests,
        start: project.start.clone(),
        assembly: project.assembly.clone(),
        root,
        user_trx,
        since_ms,
        not_followed,
    };
    let Ok(bytes) = serde_json::to_vec(&decision) else {
        return;
    };
    let written = std::fs::write(
        units.join(format!("{ADAPTER}-{id}{}", facts::STARTED_SUFFIX)),
        serde_json::to_vec(&serde_json::json!({
            "why": format!("dotnet test ended before {}'s tests did", decision.unit)
        }))
        .unwrap_or_default(),
    )
    .and_then(|()| std::fs::write(with(file, ".decision"), bytes))
    .and_then(|()| std::fs::write(with(file, ".filter"), format!("{combined}\n")))
    .and_then(|()| std::fs::write(with(file, ".loggers"), loggers.join("\n") + "\n"));
    if written.is_ok() {
        // Last: with no action, the project runs as COMMAND says.
        let action = if skip { "skip" } else { "run" };
        let _ = std::fs::write(with(file, ".action"), action);
    }
}

/// The plan's file, in the run's directory, or for an audit's second run in the first run's.
fn plan_file(units: &Path) -> Option<PlanFile> {
    let mut places = vec![units.join(PLAN_FILE)];
    if units.file_name().is_some_and(|n| n == "again")
        && let Some(parent) = units.parent()
    {
        places.push(parent.join(PLAN_FILE));
    }
    places.into_iter().find_map(|path| {
        let plan: PlanFile = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
        (plan.schema == PLAN_SCHEMA).then_some(plan)
    })
}

/// \[ADR 0118\] Whether a unit the plan lists may be left out, checked when its tests are about
/// to run: what stands in for binding a reused assembly to its bytes, which the plan can't know
/// before COMMAND builds it. The source files of the scope the project and the projects it
/// references compile now are exactly those of every list the plan keyed its unit by: so the key
/// held every file the assembly is compiled from, and the build that passed compiled the same.
fn verify(unit: &str, reads: &[String], units: &Path) -> bool {
    let Some(plan) = plan_file(units) else {
        return false;
    };
    verify_with(unit, reads, &plan)
}

fn verify_with(unit: &str, reads: &[String], plan: &PlanFile) -> bool {
    let Some(lists) = plan.units.get(unit).filter(|l| !l.is_empty()) else {
        return false;
    };
    let scope: BTreeSet<&str> = plan.scope.iter().map(String::as_str).collect();
    let now: BTreeSet<&str> = reads
        .iter()
        .map(String::as_str)
        .filter(|f| is_source(f) && scope.contains(f))
        .collect();
    lists.iter().all(|list| {
        let list: BTreeSet<&str> = list
            .iter()
            .map(String::as_str)
            .filter(|f| is_source(f))
            .collect();
        list == now
    })
}

fn after(file: &Path, passed: bool) {
    let Some(units) = units_dir(file) else {
        return;
    };
    let Some(decision) = std::fs::read(with(file, ".decision"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Decision>(&b).ok())
    else {
        return;
    };
    let id = id_of(file);
    let trx = (!decision.skip).then(|| match &decision.user_trx {
        Some(dir) => users_trx(dir, decision.since_ms, &decision.assembly),
        None => std::fs::read(with(file, ".trx"))
            .map_err(|err| format!("hushrun's TRX file wasn't written: {err}"))
            .and_then(|bytes| trx::read(&bytes)),
    });
    let (unit, stale) = unit_facts(&decision, trx.as_ref(), passed);
    let mut settings = BTreeMap::new();
    settings.insert(
        "own".to_string(),
        serde_json::json!({ decision.unit.clone(): decision.own }),
    );
    if let Some(Err(why)) = &trx {
        settings.insert("trx".to_string(), serde_json::json!(why));
    }
    write_facts(
        &units,
        &id,
        &decision.start,
        vec![unit],
        settings,
        decision.tests,
        stale,
    );
}

/// Whether a logger COMMAND gave is VSTest's TRX logger, by its name or its URI.
fn is_trx(logger: &str) -> bool {
    let name = logger.split(';').next().unwrap_or("").trim();
    name.eq_ignore_ascii_case("trx")
        || name.eq_ignore_ascii_case("logger://Microsoft/TestPlatform/TrxLogger/v1")
}

/// \[ADR 0118\] COMMAND's own TRX file for `assembly`: of the `.trx` files in `dir` written since
/// `since_ms` (a second's slack for coarse clocks), the one whose tests are all in `assembly`.
fn users_trx(dir: &Path, since_ms: u64, assembly: &Path) -> Result<trx::Trx, String> {
    let since =
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(since_ms.saturating_sub(1000));
    let assembly = std::fs::canonicalize(assembly).unwrap_or_else(|_| assembly.to_path_buf());
    let entries = std::fs::read_dir(dir).map_err(|err| {
        format!(
            "COMMAND's TRX file can't be found in {}: {err}",
            dir.display()
        )
    })?;
    let mut found: Vec<trx::Trx> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let fresh = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|m| m >= since);
        if !fresh
            || path
                .extension()
                .is_none_or(|e| !e.eq_ignore_ascii_case("trx"))
        {
            continue;
        }
        let Ok(read) = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| trx::read(&b))
        else {
            continue;
        };
        let ours = !read.code_bases.is_empty()
            && read
                .code_bases
                .iter()
                .all(|c| std::fs::canonicalize(c).is_ok_and(|c| c == assembly));
        if ours {
            found.push(read);
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(format!(
            "no TRX file written in {} since the project's tests started holds its tests",
            dir.display()
        )),
        n => Err(format!(
            "{n} TRX files written in {} since the project's tests started hold its tests",
            dir.display()
        )),
    }
}

/// \[ADR 0118\] What a project came to: its facts, and why its assembly ran old code, when it did.
fn unit_facts(
    d: &Decision,
    trx: Option<&Result<trx::Trx, String>>,
    passed: bool,
) -> (UnitFacts, Option<String>) {
    let mut unit = UnitFacts {
        unit: d.unit.clone(),
        reads: d.reads.clone(),
        not_followed: d.not_followed.clone(),
        ..UnitFacts::default()
    };
    let Some(trx) = trx else {
        // Skipped whole: the plan reused it.
        unit.deselected = true;
        return (unit, None);
    };
    let results = match trx {
        Ok(trx) => trx,
        Err(why) => {
            // Nothing of it can be told: it ran, and nothing of it finished.
            unit.selected = 1;
            unit.failed = !passed;
            unit.not_followed.push(Unfollowed {
                why: "trx".into(),
                detail: why.clone(),
            });
            return (unit, None);
        }
    };
    // Each test by its id, over its data rows: it finished when every row did, failed when one
    // did, was skipped when one was.
    let mut by_id: BTreeMap<&str, (bool, bool, bool, u64)> = BTreeMap::new();
    for r in &results.results {
        let e = by_id.entry(&r.id).or_insert((true, false, false, 0));
        e.0 &= matches!(
            r.outcome,
            Outcome::Passed | Outcome::Failed | Outcome::Skipped
        );
        e.1 |= r.outcome == Outcome::Failed;
        e.2 |= r.outcome == Outcome::Skipped;
        e.3 = e.3.saturating_add(r.duration_ms);
    }
    let mut tests: Vec<TestFacts> = Vec::new();
    for (id, (finished, failed, skipped, duration_ms)) in &by_id {
        unit.selected += 1;
        unit.finished += u64::from(*finished);
        unit.failed |= *failed;
        unit.skipped += u64::from(*skipped && *finished);
        unit.duration_ms = unit.duration_ms.saturating_add(*duration_ms);
        tests.push(TestFacts {
            id: (*id).to_string(),
            finished: *finished,
            failed: *failed,
            duration_ms: *duration_ms,
            skipped: *skipped,
            never_left_out: !leaves_out_exactly(id),
            not_kept: (!finished && !failed)
                .then(|| "VSTest gave it an outcome that is neither a pass nor a fail".into()),
            ..TestFacts::default()
        });
    }
    for left in &d.left_out {
        if !by_id.contains_key(left.as_str()) {
            tests.push(TestFacts {
                id: left.clone(),
                deselected: true,
                ..TestFacts::default()
            });
        }
    }
    // \[C-32\] MSBuild's call of the tests failed and no test did (a test host that crashed, a
    // data collector's error): the project failed, and nothing of it passes.
    if !passed && !unit.failed {
        unit.failed = true;
        unit.not_followed.push(Unfollowed {
            why: "dotnet test".into(),
            detail: "the project's test run failed, and no test's result says why".into(),
        });
    }
    if d.tests {
        unit.tests = Some(tests);
        // Told by test: skips are each test's.
        unit.skipped = 0;
    }
    (unit, stale(d))
}

/// \[C-32\] Why the assembly that ran is older than a file it is compiled from (`--no-build`
/// after an edit): its pass proves nothing of the files now.
fn stale(d: &Decision) -> Option<String> {
    let built = std::fs::metadata(&d.assembly)
        .and_then(|m| m.modified())
        .ok()?;
    let newer = d.reads.iter().find(|read| {
        std::fs::metadata(d.root.join(read))
            .and_then(|m| m.modified())
            .is_ok_and(|m| m > built)
    })?;
    Some(format!(
        "{} ran an assembly older than {newer}: build it first, or leave out --no-build",
        d.unit
    ))
}

fn write_facts(
    units: &Path,
    id: &str,
    start: &str,
    found: Vec<UnitFacts>,
    settings: BTreeMap<String, serde_json::Value>,
    tests: bool,
    stale_build: Option<String>,
) {
    let file = FactsFile {
        schema: if tests {
            facts::FACTS_SCHEMA_V1
        } else {
            facts::FACTS_SCHEMA
        }
        .into(),
        adapter: ADAPTER.into(),
        failures_by_unit: true,
        settings,
        units: found,
        // \[C-32\] Each `dotnet test` is one start: a project counts as run whole only when each
        // start that ran any of it ran it whole.
        start: Some(if start.is_empty() {
            format!("{ADAPTER}-{id}")
        } else {
            format!("{ADAPTER}-{start}")
        }),
        stale_build,
        ..FactsFile::default()
    };
    if let Ok(bytes) = serde_json::to_vec(&file) {
        let path = units.join(format!("{ADAPTER}-{id}.json"));
        if facts::write_whole(&path, &bytes).is_ok() {
            let _ = std::fs::remove_file(
                units.join(format!("{ADAPTER}-{id}{}", facts::STARTED_SUFFIX)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-98
    #[test]
    fn a_project_is_named_by_its_file_its_framework_and_its_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("tests/A")).unwrap();
        std::fs::write(root.join("tests/A/A.csproj"), "").unwrap();
        let text = format!(
            "project={}\nframework=net8.0\nconfiguration=Release\nassembly=/x/A.dll\nstart=1_x\nnobuild=\ntestingplatform=\nown={}\nsource={}\nsource=/elsewhere/B.cs\nsources=true\n",
            root.join("tests/A/A.csproj").display(),
            root.join("tests/A/T.cs").display(),
            root.join("tests/A/T.cs").display(),
        );
        let p = parse_project(&text);
        assert!(p.sources_told && !p.no_build && !p.testing_platform);
        assert_eq!(p.sources.len(), 2);
        assert_eq!(
            unit_name(&root, &p).as_deref(),
            Some("tests/A/A.csproj (net8.0, Release)")
        );
        let outside = Project {
            project: "/".into(),
            ..p
        };
        assert_eq!(unit_name(&root, &outside), None);
    }

    // covers: C-100
    #[test]
    fn the_filter_keeps_commands_own_and_leaves_out_only_exact_names() {
        assert_eq!(filter("", &[]), "");
        assert_eq!(filter("Category=Fast", &[]), "Category=Fast");
        let ids = vec!["A.B.C".to_string(), "A.B.D".to_string()];
        assert_eq!(
            filter("", &ids),
            "FullyQualifiedName!=A.B.C&FullyQualifiedName!=A.B.D"
        );
        assert_eq!(
            filter("Category=Fast|Priority=1", &ids),
            "(Category=Fast|Priority=1)&(FullyQualifiedName!=A.B.C&FullyQualifiedName!=A.B.D)"
        );
        assert!(leaves_out_exactly("Calc.Tests.Outer+Inner.Generic`1.Adds"));
        for refused in [
            "Calc.T.Zero(1)",
            "A.B=C",
            "A|B",
            "A&B",
            "A!B",
            "A~B",
            "A\\B",
            "A B",
            "",
        ] {
            assert!(!leaves_out_exactly(refused), "{refused}");
        }
    }

    // covers: C-100
    #[test]
    fn a_unit_is_left_out_only_when_it_compiles_what_its_key_was_made_from() {
        let plan = PlanFile {
            schema: PLAN_SCHEMA.into(),
            units: BTreeMap::from([(
                "t/T.csproj (net8.0, Debug)".to_string(),
                vec![vec![
                    "src/A.cs".to_string(),
                    "t/T.cs".to_string(),
                    "t/T.csproj".to_string(),
                ]],
            )]),
            scope: vec!["src/A.cs".into(), "src/B.cs".into(), "t/T.cs".into()],
        };
        let unit = "t/T.csproj (net8.0, Debug)";
        let reads = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(verify_with(
            unit,
            &reads(&["src/A.cs", "t/T.cs", "t/T.csproj"]),
            &plan
        ));
        // A file outside the scope's sources (a package's) changes nothing.
        assert!(verify_with(
            unit,
            &reads(&["src/A.cs", "t/T.cs", "/n/P.cs"]),
            &plan
        ));
        // It compiles one more, or one fewer: it runs.
        assert!(!verify_with(
            unit,
            &reads(&["src/A.cs", "src/B.cs", "t/T.cs"]),
            &plan
        ));
        assert!(!verify_with(unit, &reads(&["t/T.cs"]), &plan));
        assert!(!verify_with("other", &reads(&["t/T.cs"]), &plan));
    }

    fn decision(tests: bool) -> Decision {
        Decision {
            unit: "t/T.csproj (net8.0, Debug)".into(),
            left_out: vec!["N.C.Gone".into()],
            tests,
            assembly: "/nowhere/T.dll".into(),
            root: "/nowhere".into(),
            ..Decision::default()
        }
    }

    fn result(id: &str, outcome: Outcome) -> trx::Result {
        trx::Result {
            id: id.into(),
            outcome,
            duration_ms: 2,
        }
    }

    // covers: C-99
    #[test]
    fn rows_of_one_test_are_one_test_and_a_failed_run_with_no_failed_test_fails_its_project() {
        let trx: Result<trx::Trx, String> = Ok(trx::Trx {
            results: vec![
                result("N.C.Theory", Outcome::Passed),
                result("N.C.Theory", Outcome::Failed),
                result("N.C.Fine", Outcome::Passed),
                result("N.C.Later", Outcome::Skipped),
                result("N.C.Zero(1)", Outcome::Passed),
            ],
            summary: "Failed".into(),
            code_bases: BTreeSet::new(),
        });
        let (unit, stale) = unit_facts(&decision(true), Some(&trx), false);
        assert!(stale.is_none());
        assert!(unit.failed);
        assert_eq!((unit.selected, unit.finished), (4, 4));
        let tests = unit.tests.unwrap();
        let of = |id: &str| tests.iter().find(|t| t.id == id).unwrap().clone();
        assert!(of("N.C.Theory").failed);
        assert!(!of("N.C.Fine").failed && of("N.C.Fine").finished);
        assert!(of("N.C.Later").skipped);
        assert!(of("N.C.Zero(1)").never_left_out);
        assert!(of("N.C.Gone").deselected);

        // Every test passed, and MSBuild's call failed all the same.
        let ok: Result<trx::Trx, String> = Ok(trx::Trx {
            results: vec![result("N.C.Fine", Outcome::Passed)],
            summary: "Completed".into(),
            code_bases: BTreeSet::new(),
        });
        let (unit, _) = unit_facts(&decision(false), Some(&ok), false);
        assert!(unit.failed && unit.tests.is_none());
        let (unit, _) = unit_facts(&decision(false), Some(&ok), true);
        assert!(!unit.failed && unit.finished == 1);

        // No TRX file: nothing of it finished.
        let none: Result<trx::Trx, String> = Err("missing".into());
        let (unit, _) = unit_facts(&decision(true), Some(&none), true);
        assert_eq!((unit.selected, unit.finished), (1, 0));
        // Skipped whole.
        let (unit, _) = unit_facts(&decision(true), None, true);
        assert!(unit.deselected && unit.tests.is_none());
    }
}
