//! \[C-32\] The `HUSHRUN_UNITS` directory: what hushrun writes there for adapters, and what they
//! write back, each file whole.
//!
//! - `hushrun.json`, hushrun's, written before COMMAND starts: `{"schema": "hushrun.units/v0",
//!   "root": <the repository's root>, "reuse": [<units the plan lets adapters deselect>]}`, and
//!   \[ADR 0069\] what an adapter checks before it deselects any: `pythonFiles`, pytest's unit rule
//!   as hushrun read it, and `binaries`, each reused test binary's path and SHA-256.
//! - `<anything>.json`, an adapter's, one per process: `{"schema": "hushrun.unit-facts/v0", …}`,
//!   [`FactsFile`]. A name that starts with `.` is a file being written, and is never read.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use hushrun_core::units::NotFollowed;

/// The schema of `hushrun.json`.
pub const RUN_SCHEMA: &str = "hushrun.units/v0";
/// The schema of an adapter's file.
pub const FACTS_SCHEMA: &str = "hushrun.unit-facts/v0";
/// hushrun's own file in the directory.
pub const RUN_FILE: &str = "hushrun.json";
/// The most adapter files a run reads: past it, hushrun records no unit (C-32).
pub const MOST_FILES: usize = 100_000;

/// `hushrun.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunFile {
    pub schema: String,
    pub root: PathBuf,
    /// \[ADR 0069\] Units the plan lets an adapter deselect (C-32): those whose newest result that
    /// counts here passed on the files they read, but the audited ones.
    pub reuse: Vec<String>,
    /// \[ADR 0069\] pytest's `python_files` as hushrun read it: a plugin whose pytest uses another
    /// deselects nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_files: Option<Vec<String>>,
    /// \[ADR 0069\] The test binaries of `reuse`, each by the path and the SHA-256 of the build
    /// whose dep-info its key holds: the target runner reuses only a binary that is that one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binaries: Vec<Binary>,
    /// \[ADR 0071\] The test files the plan classified as such, from the root: Vitest's adapter
    /// deselects nothing when its own list isn't this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_files: Option<Vec<String>>,
}

/// \[ADR 0069\] A test binary the plan reuses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Binary {
    pub unit: String,
    /// Its path, as the build named it.
    pub path: PathBuf,
    /// The SHA-256 of its bytes, lowercase hex.
    pub sha256: String,
}

/// Something an adapter couldn't follow, as its file writes it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unfollowed {
    pub why: String,
    #[serde(default)]
    pub detail: String,
}

impl From<&Unfollowed> for NotFollowed {
    fn from(u: &Unfollowed) -> NotFollowed {
        NotFollowed {
            why: u.why.clone(),
            detail: u.detail.clone(),
        }
    }
}

/// One adapter process's file (C-32). An observed adapter writes `units`; hushrun's target
/// runner writes one `invocation` per test binary it started.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactsFile {
    pub schema: String,
    pub adapter: String,
    /// An xdist controller's file, which holds no unit.
    #[serde(default)]
    pub controller: bool,
    /// The adapter reports each failure by unit: a unit's pass holds when COMMAND failed.
    #[serde(default)]
    pub failures_by_unit: bool,
    /// What the runner said of its own configuration, such as pytest's `python_files`.
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
    /// What it couldn't follow outside any unit: it applies to every unit of this file.
    #[serde(default)]
    pub not_followed: Vec<Unfollowed>,
    #[serde(default)]
    pub units: Vec<UnitFacts>,
    #[serde(default)]
    pub invocation: Option<Invocation>,
    /// The adapter's own errors: with any, nothing of this file's units is recorded.
    #[serde(default)]
    pub errors: Vec<String>,
    /// \[ADR 0069\] How many tests this process deselected, as `hushrun.json` listed their units.
    #[serde(default)]
    pub removed_items: u64,
    /// \[ADR 0072\] Direct edges between files of the repository it saw, `[from, to, kind]`,
    /// `kind` `import`, `open` or `read`: what `hushrun why` shows a path with, never a key.
    #[serde(default, deserialize_with = "edges")]
    pub edges: Vec<(String, String, String)>,
}

/// A file's `edges`, or none when they don't read: they explain, and never stop a run.
fn edges<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<(String, String, String)>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default())
}

/// \[ADR 0072\] The edges `adapter`'s files report, those of a kind the store knows.
pub fn edges_of(files: &[FactsFile], adapter: &str) -> BTreeSet<super::db::Edge> {
    files
        .iter()
        .filter(|f| f.adapter == adapter && !f.controller)
        .flat_map(|f| f.edges.iter())
        .filter_map(|(from, to, kind)| {
            Some((
                from.clone(),
                to.clone(),
                super::db::EdgeKind::from_word(kind)?,
            ))
        })
        .collect()
}

/// What an observed adapter saw of one unit in one process.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitFacts {
    /// Its path from the repository root, `/`-separated.
    pub unit: String,
    /// How many of its tests the runner selected to run, after every deselection.
    #[serde(default)]
    pub selected: u64,
    /// How many of those finished, pass or fail.
    #[serde(default)]
    pub finished: u64,
    /// A test of it failed or errored, or collecting it did.
    #[serde(default)]
    pub failed: bool,
    /// The adapter deselected it, as `hushrun.json`'s `reuse` listed it.
    #[serde(default)]
    pub deselected: bool,
    #[serde(default)]
    pub duration_ms: u64,
    /// Files it read, from the repository root.
    #[serde(default)]
    pub reads: Vec<String>,
    #[serde(default)]
    pub not_followed: Vec<Unfollowed>,
    /// \[ADR 0071\] Directories it listed, from the repository root or absolute: it read every
    /// file below each.
    #[serde(default)]
    pub dirs: Vec<String>,
}

/// One start of a test binary by hushrun's target runner.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invocation {
    /// The binary, as Cargo or nextest named it.
    pub binary: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// For a `--list`, the tests it listed (libtest's terse format), else none.
    #[serde(default)]
    pub listed: Option<Vec<String>>,
    /// Its exit code, when it exited.
    pub exit: Option<i32>,
    /// The signal that stopped it, when one did.
    pub signal: Option<i32>,
    pub duration_ms: u64,
    /// \[ADR 0069\] The runner didn't start it: `hushrun.json` lists it, and it is the binary the
    /// plan's key was for. A list answered with no test, a run with exit 0.
    #[serde(default)]
    pub reused: bool,
    /// \[ADR 0070\] The directory it ran in, which for `go test` is its package's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// \[ADR 0070\] Go's test log of the start, its lines as the binary wrote them, when it wrote
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testlog: Option<Vec<String>>,
    /// \[ADR 0070\] The test log had more lines than hushrun reads.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub testlog_truncated: bool,
}

/// Every adapter file of a run's directory, in name order, with those that don't parse or have
/// another schema left out; `None` when there are too many to read.
pub fn read_all(dir: &Path) -> Option<Vec<FactsFile>> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.ends_with(".json") && !name.starts_with('.') && name != RUN_FILE
        })
        .collect();
    if names.len() > MOST_FILES {
        return None;
    }
    names.sort();
    Some(
        names
            .iter()
            .filter_map(|path| {
                let bytes = std::fs::read(path).ok()?;
                let file: FactsFile = serde_json::from_slice(&bytes)
                    .map_err(|err| tracing::debug!("{}: {err}", path.display()))
                    .ok()?;
                (file.schema == FACTS_SCHEMA).then_some(file)
            })
            .collect(),
    )
}

/// What every process of an observed adapter saw of one unit, merged: the most tests any process
/// selected, since each collects them all; the tests every process finished; deselected only when
/// every process that saw it deselected it; and the union of everything else.
#[derive(Debug, Clone, Default)]
pub struct Merged {
    pub selected: u64,
    pub finished: u64,
    pub failed: bool,
    pub deselected: bool,
    pub duration_ms: u64,
    pub reads: BTreeSet<String>,
    pub not_followed: BTreeSet<NotFollowed>,
    pub failures_by_unit: bool,
}

/// The units of `adapter`'s files, merged across its processes (C-32).
pub fn merged(files: &[FactsFile], adapter: &str) -> BTreeMap<String, Merged> {
    let mut out: BTreeMap<String, Merged> = BTreeMap::new();
    for file in files
        .iter()
        .filter(|f| f.adapter == adapter && !f.controller)
    {
        let mut whole: BTreeSet<NotFollowed> = file.not_followed.iter().map(Into::into).collect();
        if !file.errors.is_empty() {
            whole.insert(NotFollowed {
                why: "adapter-error".into(),
                detail: file.errors[0].chars().take(200).collect(),
            });
        }
        for unit in &file.units {
            let merged = out.entry(unit.unit.clone()).or_insert_with(|| Merged {
                failures_by_unit: true,
                deselected: true,
                ..Merged::default()
            });
            merged.selected = merged.selected.max(unit.selected);
            merged.finished = merged.finished.saturating_add(unit.finished);
            merged.failed |= unit.failed;
            // \[ADR 0069\] A process that ran it, as one the plan doesn't speak for may, outweighs one
            // that deselected it.
            merged.deselected &= unit.deselected;
            merged.duration_ms = merged.duration_ms.saturating_add(unit.duration_ms);
            merged.reads.extend(unit.reads.iter().cloned());
            merged
                .not_followed
                .extend(unit.not_followed.iter().map(NotFollowed::from));
            merged.not_followed.extend(whole.iter().cloned());
            merged.failures_by_unit &= file.failures_by_unit;
        }
    }
    out
}

/// What a file says of a runner setting, as a list of strings, when every file that says it
/// agrees; `Err(())` when two disagree.
pub fn setting(files: &[FactsFile], adapter: &str, key: &str) -> Result<Option<Vec<String>>, ()> {
    let mut found: Option<Vec<String>> = None;
    for file in files.iter().filter(|f| f.adapter == adapter) {
        let Some(value) = file.settings.get(key) else {
            continue;
        };
        let list: Vec<String> = match value.as_array() {
            Some(items) => items
                .iter()
                .map(|v| v.as_str().map(str::to_string).ok_or(()))
                .collect::<Result<_, _>>()?,
            None => return Err(()),
        };
        match &found {
            Some(seen) if *seen != list => return Err(()),
            _ => found = Some(list),
        }
    }
    Ok(found)
}

/// Writes `bytes` to `path` whole: a temporary file beside it, then a rename, so that a reader
/// never sees half of it.
pub fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let temp = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    let written = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(adapter: &str, units: Vec<UnitFacts>) -> FactsFile {
        FactsFile {
            schema: FACTS_SCHEMA.into(),
            adapter: adapter.into(),
            failures_by_unit: true,
            units,
            ..FactsFile::default()
        }
    }

    fn unit(name: &str, selected: u64, finished: u64, reads: &[&str]) -> UnitFacts {
        UnitFacts {
            unit: name.into(),
            selected,
            finished,
            reads: reads.iter().map(|r| r.to_string()).collect(),
            ..UnitFacts::default()
        }
    }

    // covers: C-32
    #[test]
    fn processes_merge_into_one_unit() {
        // Two xdist workers each collected all three tests of the file and ran some of them.
        let a = file("pytest", vec![unit("tests/test_a.py", 3, 2, &["app/x.py"])]);
        let mut b = file("pytest", vec![unit("tests/test_a.py", 3, 1, &["app/y.py"])]);
        b.not_followed.push(Unfollowed {
            why: "subprocess".into(),
            detail: "git".into(),
        });
        let controller = FactsFile {
            controller: true,
            ..file(
                "pytest",
                vec![unit("tests/test_a.py", 9, 0, &["secret.py"])],
            )
        };
        let merged = merged(&[a, b, controller], "pytest");
        let m = &merged["tests/test_a.py"];
        assert_eq!((m.selected, m.finished), (3, 3));
        assert_eq!(
            m.reads.iter().map(String::as_str).collect::<Vec<_>>(),
            ["app/x.py", "app/y.py"]
        );
        // What a process couldn't follow outside any unit reaches every unit it reported.
        assert!(m.not_followed.iter().any(|n| n.why == "subprocess"));
        // An adapter's own error makes its units not observed.
        let mut broken = file("pytest", vec![unit("tests/test_b.py", 1, 1, &[])]);
        broken.errors.push("on_open: boom".into());
        let merged = super::merged(&[broken], "pytest");
        assert!(
            merged["tests/test_b.py"]
                .not_followed
                .iter()
                .any(|n| n.why == "adapter-error")
        );
    }

    // covers: C-32
    #[test]
    fn a_unit_is_deselected_only_when_every_process_that_saw_it_deselected_it() {
        let mut gone = unit("tests/test_a.py", 0, 0, &[]);
        gone.deselected = true;
        let mut ran = unit("tests/test_a.py", 2, 2, &["app/x.py"]);
        ran.failed = true;
        let merged = merged(
            &[
                file("pytest", vec![gone.clone()]),
                file("pytest", vec![ran]),
            ],
            "pytest",
        );
        let m = &merged["tests/test_a.py"];
        assert!(!m.deselected && m.failed && m.finished == 2);
        let both = super::merged(
            &[
                file("pytest", vec![gone.clone()]),
                file("pytest", vec![gone]),
            ],
            "pytest",
        );
        assert!(both["tests/test_a.py"].deselected);
    }

    // covers: C-32
    #[test]
    fn a_directory_reads_only_whole_files_of_its_schema() {
        let dir = tempfile::tempdir().unwrap();
        let good = file("pytest", vec![unit("t.py", 1, 1, &[])]);
        write_whole(
            &dir.path().join("pytest-1.json"),
            &serde_json::to_vec(&good).unwrap(),
        )
        .unwrap();
        // Being written, another schema, hushrun's own and not JSON: none is read.
        std::fs::write(dir.path().join(".pytest-2.json.tmp"), b"{").unwrap();
        std::fs::write(dir.path().join(".pytest-3.json"), b"{}").unwrap();
        std::fs::write(
            dir.path().join("other.json"),
            br#"{"schema":"other/v1","adapter":"pytest"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.json"), b"{\"schema\":").unwrap();
        std::fs::write(
            dir.path().join(RUN_FILE),
            br#"{"schema":"hushrun.units/v0","root":"/","reuse":[]}"#,
        )
        .unwrap();
        let files = read_all(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].units[0].unit, "t.py");
        assert!(
            std::fs::read_dir(dir.path()).unwrap().flatten().all(|e| !e
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
                || e.file_name() == ".pytest-2.json.tmp")
        );
    }
}
