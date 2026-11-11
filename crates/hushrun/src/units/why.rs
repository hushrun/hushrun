//! \[ADR 0073\] `hushrun why NAME UNIT` (C-37): what a run of the check would do with one test
//! file, for the files on disk now, and why, from what this machine knows.
//!
//! It explains what the plan decides (C-34) and decides nothing: the reasons that don't depend on
//! any recorded result (not a test file, `tests` off, an adapter not offered, `tests.always`, never
//! seen run here, not observed last time) come from `hushrun.yaml`, COMMAND and this machine's store
//! (ADR 0072) alone; whether it would be reused, whether its newest result failed, and a
//! suspension come from one read-only batch lookup, as `hushrun ready`'s (C-22), which sends
//! nothing waiting in the outbox. When the lookup can't answer, or the test files are known only
//! from the last build or run here (Cargo, Go, Vitest), it says the answer is as far as this
//! machine knows. It writes nothing (ADR 0061).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use hushrun_core::units::{self, KeyBase, Mode, Set, Sources, Tests, Tree};

use super::db::{self, EdgeKind};
use super::store::{self, Map, UnitResults};
use super::{Adapter, Keyer, cargo, choose, gotest, mapped_reads, pytest, rest_of, vitest};
use crate::results::RunResult;

/// What `hushrun why` needs.
pub struct WhyInput<'a> {
    /// The check's key fields as a run of it here has them.
    pub base: KeyBase,
    /// The repository the run names (C-10).
    pub repository: Option<&'a str>,
    /// The check's `tests`, `None` when they are off.
    pub tests: Option<&'a Tests>,
    /// The test file asked about, from the repository root; for Go, a package's directory or one
    /// of its test files.
    pub unit: &'a str,
    /// The scope's files on disk now.
    pub tree: &'a Tree,
    pub root: &'a Path,
    pub cwd: &'a Path,
    /// This machine's store.
    pub store: &'a Path,
    /// Where results are found, read-only; `Err` why there is nowhere to look.
    pub results: Result<&'a dyn UnitResults, String>,
    /// Whether a run's result counts here (C-10).
    pub counts: &'a dyn Fn(&RunResult) -> bool,
}

/// What a run would do with the test file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Runs,
    Reused,
    /// It isn't a test file of the check.
    NotATestFile,
    /// The check's test files aren't looked at: they run with it.
    Off,
}

impl Decision {
    pub fn word(self) -> &'static str {
        match self {
            Decision::Runs => "runs",
            Decision::Reused => "reused",
            Decision::NotATestFile => "not-a-test-file",
            Decision::Off => "off",
        }
    }
}

/// One hop of a path between files: `file`, and how it reaches the next, if there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    pub file: String,
    pub then: Option<EdgeKind>,
}

/// The explanation.
#[derive(Debug, Clone)]
pub struct Explanation {
    /// The test file, as the check names it (for Go, the package's directory).
    pub unit: String,
    pub decision: Decision,
    /// A word for the reason, the JSON's.
    pub reason: &'static str,
    /// The first line.
    pub says: String,
    /// Lines after it, indented in text.
    pub detail: Vec<String>,
    /// The answer comes from what this machine knows, which may not be all.
    pub here_only: bool,
    /// The file that changed, or is new or gone, when that is the reason.
    pub file: Option<String>,
    /// A path of direct edges from the test file to that file, when the adapter saw one.
    pub path: Vec<Hop>,
}

/// Explains what a run of the check would do with `input.unit`.
pub fn explain(input: &WhyInput<'_>) -> Explanation {
    let check = &input.base.check;
    let asked = input.unit.trim_start_matches("./").trim_end_matches('/');
    let mut out = Explanation {
        unit: asked.to_string(),
        decision: Decision::Runs,
        reason: "",
        says: String::new(),
        detail: Vec::new(),
        here_only: false,
        file: None,
        path: Vec::new(),
    };
    let Some(tests) = input.tests.filter(|t| t.is_on()) else {
        out.decision = Decision::Off;
        out.reason = "tests-off";
        out.says = format!(
            "{check}'s `tests` is off in hushrun.yaml: its test files run with it, whole, whenever it runs"
        );
        return out;
    };
    let adapter = match choose(&input.base.command, input.cwd) {
        Ok(adapter) => adapter,
        Err(why) => {
            out.decision = Decision::Off;
            out.reason = "not-offered";
            out.says = format!("{check}'s test files aren't observed: {why}");
            return out;
        }
    };
    let loaded = db::load(input.store, &store::check_id(&input.base, input.repository));
    if let Some(unread) = &loaded.unread {
        out.detail
            .push(format!("every test file runs: {}", unread.why()));
    }
    let map = loaded.map;
    // For Go a unit is a package's directory, which one of its test files names too.
    let unit = match adapter {
        Adapter::Go(_) if asked.ends_with("_test.go") => match asked.rsplit_once('/') {
            Some((dir, _)) => dir.to_string(),
            None => ".".to_string(),
        },
        Adapter::Go(_) if asked.is_empty() => ".".to_string(),
        _ => asked.to_string(),
    };
    out.unit = unit.clone();
    let would = tests.mode != Mode::Reuse;
    let verb = |decision: Decision| match (decision, would) {
        (Decision::Reused, false) => "would be reused",
        (Decision::Reused, true) => "would be reused with `tests: reuse`",
        (_, false) => "runs",
        (_, true) => "would run with `tests: reuse`",
    };
    let says = |out: &mut Explanation, decision: Decision, reason: &'static str, why: String| {
        out.decision = decision;
        out.reason = reason;
        out.says = match decision {
            Decision::Runs | Decision::Reused => format!("{} {}: {why}", out.unit, verb(decision)),
            Decision::NotATestFile | Decision::Off => why,
        };
    };

    // The four sets, by the adapter's rule (C-30): pytest's from files now; the others' from the
    // last run here, as far as this machine knows.
    let python_files = pytest::python_files(input.root, input.cwd);
    let sets = match &adapter {
        Adapter::Pytest => units::classify(
            input.tree.paths(),
            &|p| pytest::is_test_file(&python_files, p) == Some(true),
            Sources::Kinds(pytest::SOURCE_KINDS),
            &tests.ignore,
        ),
        Adapter::Vitest(_) => units::classify(
            input.tree.paths(),
            &|p| map.unit_files.contains(p),
            Sources::Kinds(vitest::SOURCE_KINDS),
            &tests.ignore,
        ),
        Adapter::Cargo(_) => units::classify(
            input.tree.paths(),
            &|p| map.units.contains_key(p),
            Sources::Named(&map.sources, cargo::SOURCE_KINDS),
            &tests.ignore,
        ),
        Adapter::Go(_) => units::classify(
            input.tree.paths(),
            &|p| p.ends_with("_test.go") && map.units.contains_key(&package_of(p)),
            Sources::Named(&map.sources, gotest::SOURCE_KINDS),
            &tests.ignore,
        ),
    };
    let known_from_last_run = match &adapter {
        Adapter::Pytest => None,
        Adapter::Vitest(_) => Some("Vitest's list of test files is the one of its last run here"),
        Adapter::Cargo(_) => {
            Some("its test binaries and what each is built from are those of its last build here")
        }
        Adapter::Go(_) => Some(
            "its packages and what each is built from are those go list named in its last run here",
        ),
    };
    if let Some(why) = known_from_last_run {
        out.here_only = true;
        out.detail
            .push(format!("as far as this machine knows: {why}"));
    }

    // Is it a test file of the check at all?
    let is_unit = match &adapter {
        Adapter::Go(_) => map.units.contains_key(&unit),
        _ => sets.of(&unit) == Some(Set::Units),
    };
    if !is_unit {
        let why = match &adapter {
            _ if sets.of(&unit).is_none() && !matches!(adapter, Adapter::Go(_)) => {
                format!("{unit} isn't a file of {check}'s scope as it is now")
            }
            _ if sets.of(&unit) == Some(Set::Ignored) => {
                format!("{unit} is in {check}'s `tests.ignore`: no test reads it")
            }
            Adapter::Pytest if !map.reported() => format!(
                "{unit} isn't a test file of {check} as far as this machine knows: no run of {check} here has had pytest report its test files"
            ),
            Adapter::Pytest => format!(
                "{unit} isn't a test file of {check}: pytest's python_files ({}) doesn't name it",
                python_files.join(", ")
            ),
            Adapter::Vitest(_) => format!(
                "{unit} isn't a test file of {check} as far as this machine knows: Vitest didn't list it in its last run here"
            ),
            Adapter::Cargo(_) => format!(
                "{unit} isn't a test file of {check} as far as this machine knows: no test binary of its last build here has it as its root file"
            ),
            Adapter::Go(_) => format!(
                "{unit} isn't a package {check} tests as far as this machine knows: go list didn't name it in its last run here"
            ),
        };
        says(&mut out, Decision::NotATestFile, "not-a-test-file", why);
        return out;
    }
    if tests.always_runs(&unit) {
        says(
            &mut out,
            Decision::Runs,
            "always",
            "`tests.always` in hushrun.yaml names it".into(),
        );
        return out;
    }
    let Some(mapped) = map.units.get(&unit) else {
        says(
            &mut out,
            Decision::Runs,
            "never-ran-here",
            "this machine has never seen it run, so it doesn't know what it reads".into(),
        );
        return out;
    };
    if !mapped.not_followed.is_empty() {
        let what: Vec<String> = mapped
            .not_followed
            .iter()
            .map(|n| match n.detail.as_str() {
                "" => super::describe(&n.why),
                detail => format!("{} ({detail})", super::describe(&n.why)),
            })
            .collect();
        says(
            &mut out,
            Decision::Runs,
            "not-observed",
            format!(
                "hushrun couldn't follow all it read the last time it ran here: {}",
                what.join(", ")
            ),
        );
        out.detail
            .insert(0, "so it runs every time, and records nothing".into());
        return out;
    }
    let reads = mapped_reads(&map, &unit, input.tree).unwrap_or_default();
    let keyer = Keyer::new(input.tree, &sets, &input.base);
    let key = keyer.key(&unit, &reads);
    let changes = changes(&map, &sets, input.tree, &unit, &reads);

    let looked = match (&input.results, key) {
        (Ok(results), Some(key)) => results
            .lookup(&[key], input.counts)
            .map(|lookup| (key, lookup)),
        (Err(why), _) => Err(why.clone()),
        (_, None) => Err("its key can't be computed".into()),
    };
    match looked {
        Ok((_, lookup)) if !lookup.reuse.allows_reuse() => {
            says(
                &mut out,
                Decision::Runs,
                "suspended",
                super::suspended(check, &lookup.reuse),
            );
        }
        Ok((key, lookup)) => match lookup.found.get(&key) {
            Some(found) if found.passed => {
                says(
                    &mut out,
                    Decision::Reused,
                    "reused",
                    "it passed on the files it read, and none of them has changed since".into(),
                );
                out.detail.insert(
                    0,
                    "an audit run may still run it: each run runs 2% of the test files that could be reused, at least one, chosen by their keys and the run's start".into(),
                );
            }
            Some(_) => says(
                &mut out,
                Decision::Runs,
                "failed",
                "its newest result on the files it reads now failed".into(),
            ),
            None => explain_change(&mut out, &changes, &says, &map, input, &unit, &adapter),
        },
        Err(why) => {
            out.here_only = true;
            out.detail.push(format!(
                "as far as this machine knows: can't read which passed ({why})"
            ));
            explain_change(&mut out, &changes, &says, &map, input, &unit, &adapter);
        }
    }
    out
}

/// A Go test file's package: its directory, `.` for the root.
fn package_of(file: &str) -> String {
    match file.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => ".".to_string(),
    }
}

/// What changed since the test file last ran here, among what its key holds.
#[derive(Debug, Default)]
struct Changes {
    /// A file it read whose bytes changed, is gone, or is new below a directory it listed.
    read: Option<(String, &'static str)>,
    /// A file of the rest set that changed, is new or gone, or a source new or gone: what runs
    /// every test file.
    every: Option<(String, String, &'static str)>,
}

fn changes(
    map: &Map,
    sets: &units::Sets,
    tree: &Tree,
    unit: &str,
    reads: &BTreeSet<String>,
) -> Changes {
    let mut out = Changes::default();
    if let Some(mapped) = map.units.get(unit) {
        for (path, entry) in &mapped.reads {
            match tree.entry(path) {
                None => {
                    out.read = Some((path.clone(), "gone"));
                    break;
                }
                Some((kind, hash)) if format!("{kind} {hash}") != *entry => {
                    out.read = Some((path.clone(), "changed"));
                    break;
                }
                Some(_) => {}
            }
        }
        if out.read.is_none()
            && let Some(new) = reads
                .iter()
                .find(|p| sets.keyed([p.as_str()]).len() == 1 && !mapped.reads.contains_key(*p))
        {
            out.read = Some((new.clone(), "new"));
        }
    }
    let rest = rest_of(sets, tree);
    let sources: BTreeSet<String> = sets.files(Set::Sources).map(str::to_string).collect();
    out.every = rest
        .iter()
        .find(|(path, entry)| map.rest.get(*path).is_some_and(|before| before != *entry))
        .map(|(p, _)| {
            (
                p.clone(),
                format!("{p} changed, and any test may read it"),
                "rest-changed",
            )
        })
        .or_else(|| {
            rest.keys().find(|p| !map.rest.contains_key(*p)).map(|p| {
                (
                    p.clone(),
                    format!("{p} is new, and any test may read it"),
                    "rest-changed",
                )
            })
        })
        .or_else(|| {
            map.rest.keys().find(|p| !rest.contains_key(*p)).map(|p| {
                (
                    p.clone(),
                    format!("{p} is gone, and any test may have read it"),
                    "rest-changed",
                )
            })
        })
        .or_else(|| {
            sources.iter().find(|p| !map.sources.contains(*p)).map(|p| {
                (
                    p.clone(),
                    format!("{p} is new, which can change what tests load"),
                    "shape",
                )
            })
        })
        .or_else(|| {
            map.sources.iter().find(|p| !sources.contains(*p)).map(|p| {
                (
                    p.clone(),
                    format!("{p} is gone, which can change what tests load"),
                    "shape",
                )
            })
        });
    out
}

type Says<'a> = dyn Fn(&mut Explanation, Decision, &'static str, String) + 'a;

/// Why a test file with no result under its key runs: what changed, most particular first.
fn explain_change(
    out: &mut Explanation,
    changes: &Changes,
    says: &Says<'_>,
    map: &Map,
    input: &WhyInput<'_>,
    unit: &str,
    adapter: &Adapter,
) {
    if let Some((file, how)) = &changes.read {
        let (reason, why) = match *how {
            "gone" => ("read-gone", format!("it read {file}, which is gone")),
            "new" => (
                "read-changed",
                format!("{file} is new in a directory it listed"),
            ),
            _ => ("read-changed", format!("it read {file}, which changed")),
        };
        says(out, Decision::Runs, reason, why);
        out.file = Some(file.clone());
        out.path = path_to(input, map, unit, file, adapter);
        if out.path.len() > 1 {
            let mut line = String::new();
            for hop in &out.path {
                line.push_str(&hop.file);
                if let Some(kind) = hop.then {
                    line.push(' ');
                    line.push_str(match kind {
                        EdgeKind::Import => "imports",
                        EdgeKind::Open => "opens",
                        EdgeKind::Read => "reads",
                    });
                    line.push(' ');
                }
            }
            out.detail.insert(0, line);
        } else if matches!(adapter, Adapter::Cargo(_)) {
            out.detail.insert(
                0,
                "its test binary is built from it: rustc's dep-info for the build names it".into(),
            );
        }
        if let Some((_, every, _)) = &changes.every {
            out.detail
                .push(format!("every test file runs in any case: {every}"));
        }
        return;
    }
    if let Some((file, every, reason)) = &changes.every {
        says(out, Decision::Runs, reason, every.clone());
        out.file = Some(file.clone());
        out.detail
            .insert(0, "so every test file of the check runs".into());
        return;
    }
    says(
        out,
        Decision::Runs,
        "no-pass",
        "no pass that counts here is recorded for the files it read, though none of them has changed since it last ran here".into(),
    );
    out.detail.insert(
        0,
        "its last result there didn't count here, or is gone: this machine forgets a result 30 days after its run".into(),
    );
}

/// The shortest path of direct edges the store holds from `unit` to `file` (for Go, to its
/// package's directory), through files `unit` read; empty when there is none.
fn path_to(input: &WhyInput<'_>, map: &Map, unit: &str, file: &str, adapter: &Adapter) -> Vec<Hop> {
    let check_id = store::check_id(&input.base, input.repository);
    let Ok(edges) = db::edges(input.store, &check_id) else {
        return Vec::new();
    };
    let go = matches!(adapter, Adapter::Go(_));
    let target = if go {
        package_of(file)
    } else {
        file.to_string()
    };
    let allowed: BTreeSet<&str> = map
        .units
        .get(unit)
        .map(|m| m.reads.keys().map(String::as_str).collect())
        .unwrap_or_default();
    // Imports and opens first, which say how: a test file reads every module its collection
    // loaded, which is true and says less.
    for kinds in [
        &[EdgeKind::Import, EdgeKind::Open][..],
        &[EdgeKind::Import, EdgeKind::Open, EdgeKind::Read],
    ] {
        let hops = shortest(&edges, kinds, &allowed, go, unit, &target);
        if !hops.is_empty() {
            return hops;
        }
    }
    Vec::new()
}

/// The shortest path from `unit` to `target` over `edges` of `kinds`, through `allowed` files
/// unless `any`.
fn shortest(
    edges: &BTreeSet<db::Edge>,
    kinds: &[EdgeKind],
    allowed: &BTreeSet<&str>,
    any: bool,
    unit: &str,
    target: &str,
) -> Vec<Hop> {
    let mut next: BTreeMap<&str, Vec<(&str, EdgeKind)>> = BTreeMap::new();
    for (from, to, kind) in edges {
        if kinds.contains(kind) && (any || allowed.contains(to.as_str())) {
            next.entry(from.as_str())
                .or_default()
                .push((to.as_str(), *kind));
        }
    }
    let mut came: BTreeMap<&str, (&str, EdgeKind)> = BTreeMap::new();
    let mut queue = VecDeque::from([unit]);
    let mut seen = BTreeSet::from([unit]);
    while let Some(at) = queue.pop_front() {
        if at == target {
            let mut hops = vec![Hop {
                file: at.to_string(),
                then: None,
            }];
            let mut here = at;
            while let Some((from, kind)) = came.get(here) {
                hops.push(Hop {
                    file: from.to_string(),
                    then: Some(*kind),
                });
                here = from;
            }
            hops.reverse();
            return hops;
        }
        for (to, kind) in next.get(at).into_iter().flatten() {
            if seen.insert(to) {
                came.insert(to, (at, *kind));
                queue.push_back(to);
            }
        }
    }
    Vec::new()
}
