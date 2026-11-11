//! \[ADR 0068\] What this machine keeps of test files (C-33): unit results, the map and the audit's
//! disagreements.
//!
//! - **Unit results** go with the run that executed them: one object per run, its key, its result
//!   and the list of `{key, result, durationMs}`, in `<cache dir>/results/unit-runs/v0/<run key>/`,
//!   and an entry per unit key in `<cache dir>/results/units/v0/<unit key>/` that names the run.
//!   Both sit beside this machine's run results (ADR 0067) and are swept with them. In this version
//!   nothing about units goes to a server; [`UnitResults`] is the seam a server takes later.
//! - **The map**, one file per check, `<cache dir>/units/<check id>.map.json`: for each test file,
//!   the files it read the last time it ran here, and what the rest and sources sets were then. A
//!   hint: it chooses which files a key hashes, and a wrong one costs a run, never a reuse.
//! - **The audit**, `<cache dir>/units/<check id>.audit.json`: each test file that would have been
//!   reused and failed, for suspending reuse later.
//!
//! The map and the audit are each read, changed and written whole while a lock file beside them is
//! held, as credentials are (ADR 0061).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::json;

use hushrun_core::digest::Digest;
use hushrun_core::time::Time;
use hushrun_ledger::{FileLedger, Ledger};

use crate::results::RunResult;

/// The schema of a run's unit results.
pub const RUN_SCHEMA: &str = "hushrun.unit-run/v0";
/// The schema of one unit result's entry.
pub const ENTRY_SCHEMA: &str = "hushrun.unit-result/v0";
/// Where each unit key's entries live, under the results directory.
pub const ENTRIES: &str = "units/v0";
/// Where each run's unit results live, under the results directory.
pub const RUNS: &str = "unit-runs/v0";
/// How many of a key's newest entries a lookup reads.
const LOOKUP_LIMIT: usize = 10;
/// How many disagreements an audit file keeps.
const MOST_DISAGREEMENTS: usize = 200;

/// One unit's result, as it goes with its run (C-33).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitRecord {
    pub key: Digest,
    /// `pass` or `fail`.
    pub result: String,
    pub duration_ms: u64,
}

/// The newest unit result for a key whose run counts here.
#[derive(Debug, Clone)]
pub struct Found {
    pub passed: bool,
    pub duration_ms: u64,
}

/// \[C-33\] Where unit results are kept and found: this machine's directory now, a server later.
pub trait UnitResults {
    /// The newest result under `key` whose run's result `counts` takes: the same rule that says
    /// whether a check's pass counts here (C-10, C-25), so a check and its units can't disagree.
    fn newest(&self, key: &Digest, counts: &dyn Fn(&RunResult) -> bool) -> Option<Found>;
    /// Records `units` with `run`, the result of the run that executed them, recorded under
    /// `run_key`.
    fn record(&self, run_key: &Digest, run: &RunResult, units: &[UnitRecord])
    -> anyhow::Result<()>;
}

/// This machine's unit results, in its results directory.
pub struct Local {
    ledger: FileLedger,
}

impl Local {
    /// The store in `results`, this machine's results directory (C-11).
    pub fn new(results: &Path) -> Local {
        Local {
            ledger: FileLedger::new(results.to_path_buf()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    schema: String,
    result: String,
    duration_ms: u64,
    /// The run's object, under the results directory.
    run: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunObject {
    schema: String,
    run_key: Digest,
    run: RunResult,
    units: Vec<UnitRecord>,
}

impl UnitResults for Local {
    fn newest(&self, key: &Digest, counts: &dyn Fn(&RunResult) -> bool) -> Option<Found> {
        let entries = self
            .ledger
            .list(&format!("{ENTRIES}/{}/", key.hex()))
            .ok()?;
        for object in entries.iter().rev().take(LOOKUP_LIMIT) {
            let Some(entry) = self
                .ledger
                .get(object)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Entry>(&bytes).ok())
                .filter(|e| e.schema == ENTRY_SCHEMA)
            else {
                continue;
            };
            let Some(run) = self
                .ledger
                .get(&entry.run)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<RunObject>(&bytes).ok())
                .filter(|r| r.schema == RUN_SCHEMA)
            else {
                continue;
            };
            if !counts(&run.run) {
                continue;
            }
            return Some(Found {
                passed: entry.result == "pass",
                duration_ms: entry.duration_ms,
            });
        }
        None
    }

    fn record(
        &self,
        run_key: &Digest,
        run: &RunResult,
        units: &[UnitRecord],
    ) -> anyhow::Result<()> {
        if units.is_empty() {
            return Ok(());
        }
        let name = hushrun_ledger::new_ulid(run.finished_at().millis());
        let object = format!("{RUNS}/{}/{name}.json", run_key.hex());
        let body = RunObject {
            schema: RUN_SCHEMA.into(),
            run_key: *run_key,
            run: run.clone(),
            units: units.to_vec(),
        };
        let mut bytes = serde_json::to_vec(&body)?;
        bytes.push(b'\n');
        // The run first: an entry that names no run is never read, and an entry is what a lookup
        // finds.
        self.ledger
            .append(&object, &bytes)
            .map_err(crate::results::plain)?;
        for unit in units {
            let entry = Entry {
                schema: ENTRY_SCHEMA.into(),
                result: unit.result.clone(),
                duration_ms: unit.duration_ms,
                run: object.clone(),
            };
            let mut bytes = serde_json::to_vec(&entry)?;
            bytes.push(b'\n');
            let at = format!("{ENTRIES}/{}/{name}.json", unit.key.hex());
            self.ledger
                .append(&at, &bytes)
                .map_err(crate::results::plain)?;
        }
        Ok(())
    }
}

/// The check id: the run key's fields but `digest`, hashed, so that a check's map and audit
/// follow it across changes to its files.
pub fn check_id(base: &hushrun_core::units::KeyBase) -> Digest {
    let value = json!({
        "schema": "hushrun.unit-map-id/v0",
        "check": base.check,
        "command": base.command,
        "cwd": base.cwd,
        "scope": base.scope,
    });
    hushrun_core::jcs::digest(&value).unwrap_or_else(|_| Digest::of(value.to_string().as_bytes()))
}

/// The schema of a map file.
pub const MAP_SCHEMA: &str = "hushrun.unit-map/v0";
/// The schema of an audit file.
pub const AUDIT_SCHEMA: &str = "hushrun.unit-audit/v0";

/// A check's map: a hint, never trusted (C-33).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Map {
    pub schema: String,
    /// Each test file: what it read the last time it ran here.
    pub units: BTreeMap<String, Mapped>,
    /// The rest set then, by path: each file's kind and content hash, as a manifest writes them.
    #[serde(default)]
    pub rest: BTreeMap<String, String>,
    /// The sources set's paths then.
    #[serde(default)]
    pub sources: BTreeSet<String>,
}

/// What a test file read the last time it ran here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mapped {
    pub reads: BTreeSet<String>,
    /// What the adapter couldn't follow then, by its word.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub not_followed: BTreeSet<String>,
}

/// One test file that would have been reused and failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disagreement {
    pub unit: String,
    /// The key its pass was under, which the plan would have reused.
    pub key: Digest,
    /// The run that found it.
    pub run_key: Digest,
    pub at: Time,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Audit {
    schema: String,
    disagreements: Vec<Disagreement>,
}

/// Where a check's files are: `<cache dir>/units/<check id>.<what>.json`.
pub fn file(cache: &Path, id: &Digest, what: &str) -> PathBuf {
    cache
        .join("units")
        .join(format!("{}.{what}.json", id.hex()))
}

/// A check's map, or an empty one when there is none or it doesn't read.
pub fn load_map(path: &Path) -> Map {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Map>(&bytes).ok())
        .filter(|map| map.schema == MAP_SCHEMA)
        .unwrap_or_default()
}

/// Changes the file at `path` with `change` and writes it whole, while no other hushrun does.
fn locked<T: Serialize + for<'de> Deserialize<'de> + Default>(
    path: &Path,
    change: impl FnOnce(&mut T),
) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(name))
        .with_context(|| format!("locking {}", path.display()))?;
    if let Err(err) = lock.lock() {
        tracing::debug!("can't lock {}: {err}", path.display());
    }
    let mut value: T = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    change(&mut value);
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    super::facts::write_whole(path, &bytes).with_context(|| format!("writing {}", path.display()))
}

/// Writes what this run learned into the check's map: each unit it saw replaces its entry, the
/// others stay as long as their file is still in `tree`; and the rest and sources sets are this
/// run's.
pub fn update_map(
    path: &Path,
    seen: &BTreeMap<String, Mapped>,
    tree: &hushrun_core::units::Tree,
    rest: BTreeMap<String, String>,
    sources: BTreeSet<String>,
) -> anyhow::Result<()> {
    locked::<Map>(path, |map| {
        if map.schema != MAP_SCHEMA {
            *map = Map::default();
        }
        map.schema = MAP_SCHEMA.into();
        map.units.retain(|unit, _| tree.contains(unit));
        for (unit, mapped) in seen {
            map.units.insert(unit.clone(), mapped.clone());
        }
        map.rest = rest;
        map.sources = sources;
    })
}

/// Adds `found` to the check's audit, keeping the newest.
pub fn add_disagreements(path: &Path, found: &[Disagreement]) -> anyhow::Result<()> {
    if found.is_empty() {
        return Ok(());
    }
    locked::<Audit>(path, |audit| {
        audit.schema = AUDIT_SCHEMA.into();
        audit.disagreements.extend(found.iter().cloned());
        let extra = audit.disagreements.len().saturating_sub(MOST_DISAGREEMENTS);
        audit.disagreements.drain(..extra);
    })
}

/// The disagreements a check's audit holds: what suspending reuse reads.
#[cfg(test)]
pub fn disagreements(path: &Path) -> Vec<Disagreement> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Audit>(&bytes).ok())
        .map(|audit| audit.disagreements)
        .unwrap_or_default()
}

/// \[ADR 0067\] Forgets this machine's unit state that nothing needs: a run's directory a hushrun
/// that stopped left behind, a day after it began, and a map or an audit no run touched for 30
/// days. Unit results go with the results sweep. It never fails.
pub fn sweep(cache: &Path, now: std::time::SystemTime) {
    let dir = cache.join("units");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let older = |path: &Path, days: u64| {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age.as_secs() > days * 86_400)
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && older(&path, 30) {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(runs) = std::fs::read_dir(dir.join("runs")) {
        for run in runs.flatten() {
            if older(&run.path(), 1) {
                let _ = std::fs::remove_dir_all(run.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-33
    #[test]
    fn two_runs_writing_the_map_at_once_keep_each_others_units() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("units/x.map.json");
        let tree = hushrun_core::units::Tree::default();
        let _ = tree;
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    locked::<Map>(&path, |map| {
                        std::thread::sleep(std::time::Duration::from_millis(3));
                        map.schema = MAP_SCHEMA.into();
                        map.units.insert(format!("t{n}.py"), Mapped::default());
                    })
                    .unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(load_map(&path).units.len(), 8);
    }

    // covers: C-33
    #[test]
    fn an_audit_keeps_the_newest_disagreements() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("units/x.audit.json");
        let one = Disagreement {
            unit: "tests/test_a.py".into(),
            key: Digest::of(b"k"),
            run_key: Digest::of(b"r"),
            at: Time::parse("2026-11-10T12:00:00Z").unwrap(),
        };
        let many: Vec<Disagreement> = (0..MOST_DISAGREEMENTS + 5).map(|_| one.clone()).collect();
        add_disagreements(&path, &many).unwrap();
        add_disagreements(&path, &[]).unwrap();
        assert_eq!(disagreements(&path).len(), MOST_DISAGREEMENTS);
    }
}
