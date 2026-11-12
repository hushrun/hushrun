//! \[ADR 0068\] What this machine keeps of test files (C-33), apart from the map: unit results and
//! the reuse state.
//!
//! - **Unit results** go with the run that executed them: one object per run, its key, its result
//!   and the list of `{key, result, durationMs}`, in `<cache dir>/results/unit-runs/v0/<run key>/`,
//!   and an entry per unit key in `<cache dir>/results/units/v0/<unit key>/` that names the run.
//!   Both sit beside this machine's run results (ADR 0067) and are swept with them. \[ADR 0069\] With
//!   a server, they go with the run's result instead (`POST {base}/v0/runs/{key}`), and
//!   [`UnitResults`] asks it in one batch ([`Server`]).
//! - \[ADR 0069\] **The reuse state** of a check, `on` or `suspended`, with who and when: in local
//!   mode, `<cache dir>/units/reuse/<id>.json` ([`reuse_file`]), read, changed and written whole
//!   while a lock file beside it is held, as credentials are (ADR 0061); with a server, the
//!   server's, and a suspension that can't reach it waits in `<cache dir>/units/outbox/` until it
//!   does.
//! - \[ADR 0072\] **The map** ([`Map`]) and **the audit** ([`Disagreement`]) are in this machine's
//!   store, [`super::db`]: hints and explanations, which no decision reads that a recorded result
//!   doesn't confirm. The two above are what decides, and stay files.
//! - \[C-38\] **Hints**, what other machines saw a check's test files read: a server keeps them
//!   ([`UnitResults::hints`], [`UnitResults::put_hint`]); this machine's directory keeps none.

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

/// \[C-34, ADR 0069\] Whether a check reuses its test files, for a repository: `on`, or
/// `suspended` by an audit run's disagreement until a person turns it on again. One state, read
/// with the batch lookup, from one home: the server's record with a server, this machine's file
/// in local mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReuseState {
    pub state: Reuse,
    /// How many transitions it went through: a resume names the one it saw.
    #[serde(default)]
    pub generation: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_by: Option<String>,
    /// In local mode, the test file that suspended it: names stay on this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// The two states of a check's reuse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reuse {
    #[default]
    On,
    Suspended,
}

impl ReuseState {
    /// \[C-34\] The one function every reader asks: whether a test file may be reused now.
    pub fn allows_reuse(&self) -> bool {
        self.state == Reuse::On
    }
}

/// What one batch lookup found: each key's newest result that counts here, and the check's reuse
/// state.
#[derive(Debug, Clone, Default)]
pub struct Lookup {
    pub found: BTreeMap<Digest, Found>,
    pub reuse: ReuseState,
}

/// How a person's resume ended.
#[derive(Debug, Clone)]
pub enum Resumed {
    /// It was suspended, and is on now.
    Done,
    /// Nothing changed, and why, with the state it is in: on already, or suspended again since.
    Stale(ReuseState),
}

/// \[C-33, C-34\] Where unit results and a check's reuse state are kept and found: this machine's
/// directory, or a server. A lookup is one batch; a store that can't answer says why, and then
/// nothing is reused.
pub trait UnitResults {
    /// The newest result under each of `keys` whose run's result `counts` takes, the rule a
    /// check's pass is held to (C-10, C-25), so a check and its units can't disagree; and the
    /// check's reuse state. `Err` says why it can't tell.
    fn lookup(
        &self,
        keys: &[Digest],
        counts: &dyn Fn(&RunResult) -> bool,
    ) -> Result<Lookup, String>;
    /// Suspends the check's reuse, for the disagreement of `unit` (its path, for this machine)
    /// under `key` found by the run `run`. It sticks: what can't be written now is written before
    /// the next plan reads the state.
    fn suspend(
        &self,
        unit: &str,
        key: &Digest,
        run: &Digest,
        by: &str,
    ) -> Result<ReuseState, String>;
    /// Turns the check's reuse on again, for a person, when it is still the suspension of
    /// `generation`.
    fn resume(&self, generation: i64, by: &str) -> Result<Resumed, String>;
    /// \[C-38\] The bytes of the newest hints at `place`, newest first: none where results stay
    /// on this machine (ADR 0067). `Err` says why the server didn't answer; a server from before
    /// hints answers none, and the run says so once ([`crate::results::Backend::keeps_no_hints`]).
    fn hints(&self, place: &HintPlace) -> Result<Vec<Vec<u8>>, String> {
        let _ = place;
        Ok(Vec::new())
    }
    /// \[C-38\] Sends a hint's `bytes` to `place`: `Ok` once the server keeps it.
    fn put_hint(&self, place: &HintPlace, bytes: &[u8]) -> Result<(), String> {
        let _ = (place, bytes);
        Err("results stay on this machine, which shares no hint".into())
    }
}

/// \[C-38\] Where a hint is: the check's id (C-33), its address, and the system that saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintPlace {
    pub check_id: Digest,
    pub address: Digest,
    pub os: hushrun_core::hints::Os,
}

/// This machine's unit results, in its results directory, and \[ADR 0069\] the check's reuse
/// state in local mode.
pub struct Local {
    ledger: FileLedger,
    reuse: Option<PathBuf>,
}

impl Local {
    /// The store in `results`, this machine's results directory (C-11).
    #[cfg(test)]
    pub fn new(results: &Path) -> Local {
        Local {
            ledger: FileLedger::new(results.to_path_buf()),
            reuse: None,
        }
    }

    /// The store, with the reuse state of one check at `reuse` (see [`reuse_file`]).
    pub fn with_reuse(results: &Path, reuse: PathBuf) -> Local {
        Local {
            ledger: FileLedger::new(results.to_path_buf()),
            reuse: Some(reuse),
        }
    }

    /// The newest result under `key` whose run's result `counts` takes.
    pub fn newest(&self, key: &Digest, counts: &dyn Fn(&RunResult) -> bool) -> Option<Found> {
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

    /// Records `units` with `run`, the result of the run that executed them, recorded under
    /// `run_key`: in this machine's directory, beside the run's own result.
    pub fn record(
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

impl UnitResults for Local {
    fn lookup(
        &self,
        keys: &[Digest],
        counts: &dyn Fn(&RunResult) -> bool,
    ) -> Result<Lookup, String> {
        let found = keys
            .iter()
            .filter_map(|key| Some((*key, self.newest(key, counts)?)))
            .collect();
        let reuse = self.reuse.as_deref().map(load_reuse).unwrap_or_default();
        Ok(Lookup { found, reuse })
    }

    fn suspend(
        &self,
        unit: &str,
        _key: &Digest,
        _run: &Digest,
        by: &str,
    ) -> Result<ReuseState, String> {
        let Some(path) = &self.reuse else {
            return Err("there is no reuse state to write".into());
        };
        let mut after = ReuseState::default();
        locked::<ReuseFile>(path, |file| {
            // Only a check that is on is suspended: a suspended one keeps its first suspension.
            if file.reuse.allows_reuse() {
                file.reuse = ReuseState {
                    state: Reuse::Suspended,
                    generation: file.reuse.generation + 1,
                    suspended_at: Some(now()),
                    suspended_by: Some(by.to_string()),
                    resumed_at: None,
                    resumed_by: None,
                    unit: Some(unit.to_string()),
                };
            }
            file.schema = REUSE_SCHEMA.into();
            after = file.reuse.clone();
        })
        .map_err(|e| format!("{e:#}"))?;
        Ok(after)
    }

    fn resume(&self, generation: i64, by: &str) -> Result<Resumed, String> {
        let Some(path) = &self.reuse else {
            return Err("there is no reuse state to write".into());
        };
        let mut outcome = Resumed::Stale(ReuseState::default());
        locked::<ReuseFile>(path, |file| {
            if file.reuse.state == Reuse::Suspended && file.reuse.generation == generation {
                file.reuse.state = Reuse::On;
                file.reuse.generation += 1;
                file.reuse.resumed_at = Some(now());
                file.reuse.resumed_by = Some(by.to_string());
                file.schema = REUSE_SCHEMA.into();
                outcome = Resumed::Done;
            } else {
                outcome = Resumed::Stale(file.reuse.clone());
            }
        })
        .map_err(|e| format!("{e:#}"))?;
        Ok(outcome)
    }
}

/// The schema of this machine's reuse state of a check.
pub const REUSE_SCHEMA: &str = "hushrun.unit-reuse/v0";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ReuseFile {
    schema: String,
    #[serde(flatten)]
    reuse: ReuseState,
}

/// \[ADR 0069\] Where this machine keeps a check's reuse state for a repository:
/// `<cache dir>/units/reuse/<id>.json`, the id the SHA-256 of the JCS form of
/// `{"schema": "hushrun.unit-reuse-id/v0", "repository", "check"}`, the repository lowercase, `""`
/// when the run names none.
pub fn reuse_file(cache: &Path, repository: Option<&str>, check: &str) -> PathBuf {
    let value = json!({
        "schema": "hushrun.unit-reuse-id/v0",
        "repository": repository.unwrap_or_default().to_lowercase(),
        "check": check,
    });
    let id = hushrun_core::jcs::digest(&value)
        .unwrap_or_else(|_| Digest::of(value.to_string().as_bytes()));
    cache
        .join("units")
        .join("reuse")
        .join(format!("{}.json", id.hex()))
}

fn load_reuse(path: &Path) -> ReuseState {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ReuseFile>(&bytes).ok())
        .filter(|file| file.schema == REUSE_SCHEMA)
        .map(|file| file.reuse)
        // A file that doesn't read can't say it is on: it is a suspension nobody can name.
        .unwrap_or_else(|| {
            if path.exists() {
                ReuseState {
                    state: Reuse::Suspended,
                    generation: -1,
                    ..ReuseState::default()
                }
            } else {
                ReuseState::default()
            }
        })
}

/// The command's clock, as RFC 3339.
fn now() -> String {
    Time::from_timestamp(jiff::Timestamp::now()).to_string()
}

/// \[ADR 0069\] A server's unit results and reuse state for one check of one repository, asked
/// as the run's job and variant (C-10): `POST {base}/v0/units/lookup` and `…/reuse`. A suspension
/// that can't reach the server waits in this machine's outbox, and until it does, the check's
/// reuse reads as suspended: the server is the state's home, the outbox only carries a write to it.
pub struct Server<'a> {
    pub backend: &'a crate::results::Backend,
    /// The org's URL, which names the outbox's entries.
    pub base: String,
    pub check: String,
    pub repository: Option<String>,
    pub job: Option<String>,
    pub variant: Option<String>,
    pub cache: PathBuf,
    /// \[C-22, ADR 0061\] Only read: a lookup sends nothing that waits in the outbox, and reads
    /// the check as suspended while something does, as `hushrun ready` must.
    pub read_only: bool,
}

/// An answer of the server's batch lookup.
#[derive(Debug, Deserialize)]
struct ServerLookup {
    #[serde(default)]
    units: BTreeMap<String, ServerUnit>,
    #[serde(default)]
    runs: Vec<serde_json::Value>,
    #[serde(default)]
    reuse: Option<ReuseState>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerUnit {
    result: String,
    #[serde(default)]
    duration_ms: u64,
    run: usize,
}

/// One suspension on its way to a server.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    base: String,
    check: String,
    repository: Option<String>,
    unit: String,
    run: String,
}

impl Server<'_> {
    fn outbox(&self) -> PathBuf {
        self.cache.join("units").join("outbox")
    }

    fn reuse_body(&self, more: serde_json::Value) -> serde_json::Value {
        let mut body = json!({"check": self.check, "repository": self.repository});
        if let (Some(fields), Some(more)) = (body.as_object_mut(), more.as_object()) {
            fields.extend(more.clone());
        }
        body
    }

    /// Whether a suspension of this check waits in the outbox.
    fn waiting(&self) -> bool {
        let Ok(entries) = std::fs::read_dir(self.outbox()) else {
            return false;
        };
        entries.flatten().any(|entry| {
            std::fs::read(entry.path())
                .ok()
                .and_then(|b| serde_json::from_slice::<Pending>(&b).ok())
                .is_some_and(|p| {
                    p.base == self.base && p.check == self.check && p.repository == self.repository
                })
        })
    }

    /// Sends what waits in the outbox for this check: `Ok(true)` when nothing waits any more.
    fn flush(&self) -> bool {
        let Ok(entries) = std::fs::read_dir(self.outbox()) else {
            return true;
        };
        let mut clear = true;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(pending) = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Pending>(&b).ok())
            else {
                continue;
            };
            if pending.base != self.base
                || pending.check != self.check
                || pending.repository != self.repository
            {
                continue;
            }
            let body = self.reuse_body(
                json!({"state": "suspended", "unit": pending.unit, "run": pending.run}),
            );
            match self.backend.units_request("reuse", &body) {
                // An answer that isn't a failure of the server or the way to it: done, whatever it
                // said, since sending it again would get the same.
                Ok(
                    crate::results::UnitsCall::Answered(_) | crate::results::UnitsCall::Conflict(_),
                ) => {
                    let _ = std::fs::remove_file(&path);
                }
                Ok(crate::results::UnitsCall::Unsupported) => {
                    let _ = std::fs::remove_file(&path);
                }
                Ok(crate::results::UnitsCall::Refused(why)) => {
                    tracing::debug!("a suspension was refused: {why}");
                    let _ = std::fs::remove_file(&path);
                }
                Err(err) => {
                    tracing::debug!("a suspension still waits: {err:#}");
                    clear = false;
                }
            }
        }
        clear
    }
}

impl UnitResults for Server<'_> {
    fn lookup(
        &self,
        keys: &[Digest],
        counts: &dyn Fn(&RunResult) -> bool,
    ) -> Result<Lookup, String> {
        if self.read_only && self.waiting() {
            return Ok(Lookup {
                found: BTreeMap::new(),
                reuse: ReuseState {
                    state: Reuse::Suspended,
                    generation: -1,
                    ..ReuseState::default()
                },
            });
        }
        if !self.read_only && !self.flush() {
            return Err(format!(
                "a suspension of {}'s reuse hasn't reached {} yet",
                self.check, self.base
            ));
        }
        let mut body = json!({
            "check": self.check,
            "repository": self.repository,
            "keys": keys.iter().map(Digest::hex).collect::<Vec<_>>(),
        });
        if let Some(job) = &self.job {
            body["job"] = json!(job);
            if let Some(variant) = &self.variant {
                body["variant"] = json!(variant);
            }
        }
        let answer = match self.backend.units_request("lookup", &body) {
            Ok(crate::results::UnitsCall::Answered(answer)) => answer,
            Ok(crate::results::UnitsCall::Unsupported) => {
                return Err(UNSUPPORTED.to_string());
            }
            Ok(crate::results::UnitsCall::Conflict(_)) => return Err("409".into()),
            Ok(crate::results::UnitsCall::Refused(why)) => return Err(why),
            Err(err) => return Err(format!("{err:#}")),
        };
        let answer: ServerLookup = serde_json::from_value(answer)
            .map_err(|e| format!("the answer isn't a lookup's: {e}"))?;
        let runs: Vec<Option<RunResult>> = answer
            .runs
            .into_iter()
            .map(|run| {
                serde_json::from_value::<RunResult>(run)
                    .ok()
                    .filter(|r| r.schema == crate::results::RESULT_SCHEMA)
            })
            .collect();
        let mut found = BTreeMap::new();
        for key in keys {
            let Some(unit) = answer.units.get(&key.hex()) else {
                continue;
            };
            // The server stores; the CLI decides again whether the run counts here (C-10).
            let Some(Some(run)) = runs.get(unit.run) else {
                continue;
            };
            if !counts(run) || !matches!(unit.result.as_str(), "pass" | "fail") {
                continue;
            }
            found.insert(
                *key,
                Found {
                    passed: unit.result == "pass",
                    duration_ms: unit.duration_ms,
                },
            );
        }
        let reuse = answer.reuse.unwrap_or_else(|| ReuseState {
            // A server that answers no state can't say it is on.
            state: Reuse::Suspended,
            generation: -1,
            ..ReuseState::default()
        });
        Ok(Lookup { found, reuse })
    }

    fn suspend(
        &self,
        _unit: &str,
        key: &Digest,
        run: &Digest,
        _by: &str,
    ) -> Result<ReuseState, String> {
        let pending = Pending {
            base: self.base.clone(),
            check: self.check.clone(),
            repository: self.repository.clone(),
            unit: key.hex(),
            run: run.hex(),
        };
        // Into the outbox first, written whole: a suspension is never lost between here and the
        // server, and the next plan sends it before it reads anything.
        let name = Digest::of(
            serde_json::to_string(&pending)
                .unwrap_or_default()
                .as_bytes(),
        )
        .hex();
        let path = self.outbox().join(format!("{name}.json"));
        super::facts::write_whole(&path, &serde_json::to_vec(&pending).unwrap_or_default())
            .map_err(|e| format!("the suspension couldn't be kept: {e}"))?;
        let body = self
            .reuse_body(json!({"state": "suspended", "unit": pending.unit, "run": pending.run}));
        match self.backend.units_request("reuse", &body) {
            Ok(crate::results::UnitsCall::Answered(answer)) => {
                let _ = std::fs::remove_file(&path);
                serde_json::from_value(answer["reuse"].clone())
                    .map_err(|e| format!("the answer isn't a reuse state: {e}"))
            }
            Ok(crate::results::UnitsCall::Unsupported) => {
                let _ = std::fs::remove_file(&path);
                Err(UNSUPPORTED.to_string())
            }
            Ok(crate::results::UnitsCall::Refused(why)) => {
                let _ = std::fs::remove_file(&path);
                Err(why)
            }
            Ok(crate::results::UnitsCall::Conflict(_)) => {
                let _ = std::fs::remove_file(&path);
                Err("409".into())
            }
            Err(err) => Err(format!(
                "{err:#}; it is sent before the next run reuses anything"
            )),
        }
    }

    fn resume(&self, generation: i64, _by: &str) -> Result<Resumed, String> {
        let body = self.reuse_body(json!({"state": "on", "generation": generation}));
        let state = |answer: &serde_json::Value| {
            serde_json::from_value::<ReuseState>(answer["reuse"].clone())
                .map_err(|e| format!("the answer isn't a reuse state: {e}"))
        };
        match self.backend.units_request("reuse", &body) {
            Ok(crate::results::UnitsCall::Answered(_)) => Ok(Resumed::Done),
            Ok(crate::results::UnitsCall::Conflict(answer)) => Ok(Resumed::Stale(state(&answer)?)),
            Ok(crate::results::UnitsCall::Unsupported) => Err(UNSUPPORTED.to_string()),
            Ok(crate::results::UnitsCall::Refused(why)) => Err(why),
            Err(err) => Err(format!("{err:#}")),
        }
    }

    fn hints(&self, place: &HintPlace) -> Result<Vec<Vec<u8>>, String> {
        let body = json!({
            "repository": self.repository,
            "checkId": place.check_id.hex(),
            "address": place.address.hex(),
            "os": place.os.word(),
        });
        let answer = match self.backend.hints_lookup(&body) {
            Ok(crate::results::UnitsCall::Answered(answer)) => answer,
            Ok(crate::results::UnitsCall::Unsupported) => return Ok(Vec::new()),
            Ok(crate::results::UnitsCall::Refused(why)) => return Err(why),
            Ok(crate::results::UnitsCall::Conflict(_)) => return Err("409".into()),
            Err(err) => return Err(format!("{err:#}")),
        };
        // Bytes whose SHA-256 isn't the one named came apart on the way: left out.
        let found = answer["hints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|hint| {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(hint["body"].as_str()?)
                    .ok()?;
                (hint["sha256"].as_str() == Some(Digest::of(&bytes).hex().as_str()))
                    .then_some(bytes)
            })
            .collect();
        Ok(found)
    }

    fn put_hint(&self, place: &HintPlace, bytes: &[u8]) -> Result<(), String> {
        let query = [
            (
                "repository",
                self.repository.clone().unwrap_or_default().to_lowercase(),
            ),
            ("checkId", place.check_id.hex()),
            ("address", place.address.hex()),
            ("os", place.os.word().to_string()),
        ];
        match self.backend.hint_put(&Digest::of(bytes), &query, bytes) {
            Ok(crate::results::UnitsCall::Answered(_)) => Ok(()),
            Ok(crate::results::UnitsCall::Unsupported) => {
                Err("its server doesn't keep them".into())
            }
            Ok(crate::results::UnitsCall::Refused(why)) => Err(why),
            Ok(crate::results::UnitsCall::Conflict(_)) => Err("409".into()),
            Err(err) => Err(format!("{err:#}")),
        }
    }
}

/// Why a server from before test files' results reuses none.
pub const UNSUPPORTED: &str = "its server doesn't keep test files' results yet";

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

/// The check id: the run key's fields but `digest`, hashed, so that a check's map and audit
/// follow it across changes to its files, and the repository the run names (C-10, lowercase,
/// `""` for none), so that two repositories with one check and one command on one machine keep
/// their maps apart.
pub fn check_id(base: &hushrun_core::units::KeyBase, repository: Option<&str>) -> Digest {
    let value = json!({
        "schema": "hushrun.unit-map-id/v0",
        "check": base.check,
        "command": base.command,
        "cwd": base.cwd,
        "scope": base.scope,
        "repository": repository.unwrap_or_default().to_lowercase(),
    });
    hushrun_core::jcs::digest(&value).unwrap_or_else(|_| Digest::of(value.to_string().as_bytes()))
}

/// \[ADR 0072\] A check's map, as this machine's store holds it: a hint, never trusted (C-33).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Map {
    /// Some run of the check here had its adapter report test files: the store holds the check.
    pub reported: bool,
    /// Each test file: what it read the last time it ran here.
    pub units: BTreeMap<String, Mapped>,
    /// The rest set then, by path: each file's kind and content hash, as a manifest writes them.
    pub rest: BTreeMap<String, String>,
    /// The sources set's paths then.
    pub sources: BTreeSet<String>,
    /// \[ADR 0071\] The test files the adapter listed then, for an adapter whose unit rule is its
    /// runner's own list: Vitest's.
    pub unit_files: BTreeSet<String>,
}

impl Map {
    /// \[ADR 0074\] Whether some run of the check here had its adapter report test files: the
    /// store holds a check only then.
    pub fn reported(&self) -> bool {
        self.reported
    }
}

/// \[C-38\] Where a test file's row of the map came from: what it read when it ran here, or a list
/// another machine of a system saw it read whose key had a pass that counted here, which reused
/// it. The next run of it here replaces a shared row with what it sees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Origin {
    #[default]
    SeenHere,
    /// The system whose machine saw the list.
    Shared(hushrun_core::hints::Os),
}

impl Origin {
    /// The word the store's `origin` column takes.
    pub fn word(self) -> &'static str {
        match self {
            Origin::SeenHere => "seen here",
            Origin::Shared(_) => "shared",
        }
    }

    /// The system the store's `shared_os` column holds: one for `shared`, none for `seen here`.
    pub fn system(self) -> Option<hushrun_core::hints::Os> {
        match self {
            Origin::SeenHere => None,
            Origin::Shared(os) => Some(os),
        }
    }

    /// The origin a row's two columns say, `None` for a pair the store's CHECK refuses.
    pub fn from_row(word: &str, os: Option<&str>) -> Option<Origin> {
        match (word, os) {
            ("seen here", None) => Some(Origin::SeenHere),
            ("shared", Some(os)) => hushrun_core::hints::Os::from_word(os).map(Origin::Shared),
            _ => None,
        }
    }
}

/// What a test file read the last time it ran here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mapped {
    /// \[ADR 0072\] The files it read that its key holds (units and sources), each with its kind
    /// and content hash then, as a manifest writes them: which files a plan's key hashes, and
    /// what `hushrun why` compares with the files now.
    pub reads: BTreeMap<String, String>,
    /// What the adapter couldn't follow then.
    pub not_followed: BTreeSet<hushrun_core::units::NotFollowed>,
    /// \[ADR 0070\] Directories it listed then: a plan adds every file below each.
    pub dirs: BTreeSet<String>,
    /// \[C-38\] Whether it ran here, or another machine saw it read these files.
    pub origin: Origin,
    /// \[ADR 0077\] Its tests, by id, as each last ran here, for a check with `tests.unit: test`:
    /// the ids a plan keys before COMMAND (C-39).
    pub tests: BTreeMap<String, TestRow>,
}

/// \[ADR 0077\] One test as it last ran here (C-39).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRow {
    /// Its state then: never `reused`, since a test reused keeps the row of the run it stands on.
    pub state: hushrun_core::units::State,
    pub duration_ms: u64,
    /// What wasn't followed inside its own window.
    pub not_followed: BTreeSet<hushrun_core::units::NotFollowed>,
}

impl TestRow {
    /// The state a row's `state` column holds, `None` for one the store's CHECK refuses.
    pub fn state_from_word(word: &str) -> Option<hushrun_core::units::State> {
        use hushrun_core::units::State;
        [
            State::Passed,
            State::Failed,
            State::NotObserved,
            State::NotRun,
        ]
        .into_iter()
        .find(|state| state.word() == word)
    }
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

/// \[ADR 0067\] Forgets this machine's unit state that nothing needs: a run's directory a hushrun
/// that stopped left behind, a day after it began, and a file of `units/` no run touched for 30
/// days, \[ADR 0072\] but this schema's stores, which forget what is 30 days old themselves.
/// Unit results go with the results sweep. It never fails.
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
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_file() && !super::db::is_store_file(&name) && older(&path, 30) {
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

    fn run(provider: hushrun_core::run::Provider, job: Option<&str>) -> RunResult {
        serde_json::from_value(json!({
            "schema": crate::results::RESULT_SCHEMA,
            "check": "unit",
            "result": "pass",
            "exitCode": 0,
            "command": ["pytest"],
            "cwd": "",
            "scope": "default",
            "digest": Digest::of(b"x"),
            "startedAt": "2026-11-10T12:00:00Z",
            "durationMs": 10,
            "provider": provider,
            "job": job,
            "tool": "hushrun test",
            "units": {"total": 2, "ran": 2, "reused": 0},
        }))
        .unwrap()
    }

    // covers: C-33, C-10
    #[test]
    fn a_units_pass_counts_where_its_runs_result_would() {
        use hushrun_core::run::Provider;
        let dir = tempfile::tempdir().unwrap();
        let store = Local::new(dir.path());
        let key = Digest::of(b"unit");
        let other = Digest::of(b"other unit");
        let ci = run(Provider::Github, Some("test@linux/amd64"));
        assert_eq!(ci.units.as_ref().unwrap().total, 2);
        store
            .record(
                &Digest::of(b"run"),
                &ci,
                &[
                    UnitRecord {
                        key,
                        result: "pass".into(),
                        duration_ms: 4,
                    },
                    UnitRecord {
                        key: other,
                        result: "fail".into(),
                        duration_ms: 5,
                    },
                ],
            )
            .unwrap();
        // The one rule a check's pass is held to: in its own CI job, or for a run outside CI.
        let found = |job: Option<&'static str>| {
            store.newest(&key, &move |r: &RunResult| r.counts_in(job, None))
        };
        assert!(found(Some("test@linux/amd64")).is_some_and(|f| f.passed && f.duration_ms == 4));
        assert!(found(None).is_some());
        assert!(found(Some("lint@linux/amd64")).is_none());
        // A fail is the newest result under its key.
        assert!(store.newest(&other, &|_| true).is_some_and(|f| !f.passed));
        assert!(store.newest(&Digest::of(b"never"), &|_| true).is_none());
        // Nothing is written for a run without units.
        store.record(&Digest::of(b"run2"), &ci, &[]).unwrap();
        assert_eq!(std::fs::read_dir(dir.path().join(RUNS)).unwrap().count(), 1);
    }
}
