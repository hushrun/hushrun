//! The helper (B-100 to B-109): `hushrun-tsc lookup`, which the hook asks before the compiler
//! loads, and `hushrun-tsc store <plan>`, which it calls as the process ends.
//!
//! Neither ever changes what the compiler does unless a block is put in place whole: any failure
//! of a lookup answers [`Answer::Pass`] or [`Answer::Record`], and a store's failure only means
//! nothing was stored (B-106).

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

use super::call::{Answer, CALL_SCHEMA, Call, Plan, RECORD_SCHEMA, Record};
use super::key::{self, Hint, Places, ROOT};
use super::{KIND, args};
use crate::model::{Class, Mode, NotStored, Outcome, PassedThrough};
use crate::session::{self, Current, Entry};
use crate::store::{Block, NewFile, Published, Store};

/// The schema of a TypeScript block's description.
const DESCRIPTION_SCHEMA: &str = "hushrun.tsc-block/v1";
/// The session's folder for the helper's plans, records and messages.
const WORK: &str = "tsc";

/// What a TypeScript line of the run's log says beside the rest (B-109).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TscEntry {
    /// The compile's folder from the root, and its arguments: what the person would call it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call: String,
    /// Why it passed through or wasn't stored, in the helper's words (B-101, B-103, B-104).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Why the block stays on this machine, when it was stored (B-104): `tied`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local: Vec<String>,
    /// How long the helper took from the lookup to its line, in milliseconds: for a reuse, what
    /// a hit costs.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub took_ms: u64,
    /// How many files the block holds.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub files: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// What a block's description says of each file it puts in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Output {
    /// Its path, from the root.
    path: String,
    /// Its name in the block.
    file: String,
}

/// The block's description, as the store keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Description {
    schema: String,
    kind: String,
    /// The first key.
    step: Digest,
    /// The call, from the root.
    call: String,
    /// What the compile took, in milliseconds: what a reuse saves.
    compile_ms: u64,
    /// `anywhere` or `tied` (B-104).
    place: String,
    /// Why it may not leave the machine; empty when it may.
    local: Vec<String>,
    /// Node's platform (B-108).
    platform: String,
    /// The compiler's exit status.
    code: i32,
    /// The files it wrote, in the order it wrote them.
    outputs: Vec<Output>,
    /// The repository that built it.
    repository: Option<String>,
}

/// `hushrun-tsc lookup` or `hushrun-tsc store <plan>`. Always succeeds: what it says on stdout
/// is the hook's only input.
pub fn main() -> ExitCode {
    std::panic::set_hook(Box::new(|_| {}));
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    let var = |name: &str| std::env::var_os(name);
    match argv.first().and_then(|a| a.to_str()) {
        Some("lookup") => {
            let mut input = Vec::new();
            let answer = if std::io::stdin().read_to_end(&mut input).is_ok() {
                catch_unwind(AssertUnwindSafe(|| lookup(&var, SystemTime::now(), &input)))
                    .unwrap_or(Answer::Pass)
            } else {
                Answer::Pass
            };
            let text = serde_json::to_string(&answer).unwrap_or_else(|_| "{\"do\":\"pass\"}".into());
            let _ = std::io::stdout().write_all(text.as_bytes());
        }
        Some("store") => {
            if let Some(plan) = argv.get(1) {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    store(&var, SystemTime::now(), Path::new(plan))
                }));
            }
        }
        _ => {
            let _ = std::io::stderr().write_all(
                format!(
                    "error: {} is asked by hushrun's TypeScript hook (`lookup`, `store <plan>`), not by people\n",
                    super::PROGRAM_NAME
                )
                .as_bytes(),
            );
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

fn ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A log line with nothing measured yet.
fn entry(outcome: Outcome, tsc: TscEntry) -> Entry {
    Entry {
        outcome,
        class: Some(Class::Library),
        crate_name: String::new(),
        package: String::new(),
        rustc_ms: 0,
        saved_ms: 0,
        stored_bytes: 0,
        found: false,
        differed: false,
        note: None,
        portable: None,
        step: None,
        key: None,
        downloaded: false,
        baked: false,
        go: None,
        xcode: None,
        cc: None,
        gradle: None,
        tsc: Some(tsc),
    }
}

/// How the run's log names a call: its folder from the root, then its arguments.
fn call_words(call: &Call, places: &Places) -> String {
    let cwd = places.hide(&call.cwd).unwrap_or_else(|| call.cwd.clone());
    let cwd = cwd.strip_prefix(ROOT).unwrap_or(&cwd).trim_start_matches('/');
    let cwd = if cwd.is_empty() { "." } else { cwd };
    let mut words = vec![format!("{cwd}: tsc")];
    words.extend(call.args.iter().map(|a| places.hide_text(a)));
    words.join(" ")
}

fn log_pass(current: &Current, call: &Call, places: &Places, why: &str) {
    let reason = match why {
        "compiler" => PassedThrough::Compiler,
        "store-read" | "restore" => PassedThrough::Store,
        _ => PassedThrough::UnknownArgument,
    };
    current.log(&entry(
        Outcome::PassedThrough(reason),
        TscEntry {
            call: call_words(call, places),
            why: Some(why.to_string()),
            ..TscEntry::default()
        },
    ));
}

/// B-101 to B-105: what the hook does with this call.
pub fn lookup(var: &dyn Fn(&str) -> Option<OsString>, now: SystemTime, input: &[u8]) -> Answer {
    if cfg!(windows) {
        return Answer::Pass;
    }
    let Some(current) = session::current(var) else {
        return Answer::Pass;
    };
    let Some(tsc) = current.tsc.clone() else {
        return Answer::Pass;
    };
    let Ok(call) = serde_json::from_slice::<Call>(input) else {
        return Answer::Pass;
    };
    if call.schema != CALL_SCHEMA {
        return Answer::Pass;
    }
    let places = Places::new(&tsc.roots);
    let pass = |why: &str| {
        log_pass(&current, &call, &places, why);
        Answer::Pass
    };
    let asked = match args::classify(&call.args) {
        Ok(asked) => asked,
        Err(why) => return pass(why),
    };
    let Some(cwd) = places.hide(&call.cwd) else {
        return pass("cwd-outside-roots");
    };
    let store = Store::new(&current.store);
    let compiler = Path::new(&call.compiler);
    let Ok(identity) = key::identity(compiler, &store.root().join(super::FOLDER).join("ids"))
    else {
        return pass("compiler");
    };
    let case_sensitive = call.platform != "win32"
        && key::kind_here(Path::new(&key::swap_case(&call.compiler))) != Some("f");
    let person: Vec<(String, Option<String>)> = current
        .env
        .iter()
        .chain(current.repository_env.iter())
        .map(|name| (name.clone(), var(name).map(|v| v.to_string_lossy().into_owned())))
        .collect();
    let (step, body) = key::step(&call, &identity, &places, &cwd, case_sensitive, person);
    key_log(&current, &step, &body, None);
    let suspended = store.suspended_at(&step, now).is_some();
    let mut found = None;
    if !suspended {
        match find(&store, &step, &places) {
            Ok(Some(hit)) => {
                let audited = current.audits_reuse(store.origin(&hit.block.key).as_ref(), |reuse| {
                    crate::audit::sample(&hit.block.key, current.started_ms, reuse)
                });
                if current.mode == Mode::On && !audited {
                    return match restore(&places, &hit) {
                        Ok(reused) => {
                            let _ = store.used(&hit.block.key, now);
                            let took = ms(SystemTime::now()).saturating_sub(ms(now));
                            current.log(&Entry {
                                saved_ms: hit.description.compile_ms.saturating_sub(took),
                                found: true,
                                step: Some(step),
                                key: Some(hit.block.key),
                                ..entry(
                                    Outcome::Reused,
                                    TscEntry {
                                        call: call_words(&call, &places),
                                        local: hit.description.local.clone(),
                                        took_ms: took,
                                        files: hit.description.outputs.len() as u64,
                                        ..TscEntry::default()
                                    },
                                )
                            });
                            reused
                        }
                        // B-105: a restore that fails part way runs the compiler, which writes
                        // over what was put in place.
                        Err(_) => pass("restore"),
                    };
                }
                found = Some(hit.block.key);
            }
            Ok(None) => {}
            Err(_) => return pass("store-read"),
        }
    }
    let dir = current.folder.join(WORK);
    if std::fs::create_dir_all(&dir).is_err() {
        return pass("session");
    }
    let nanos = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let name = format!("{}-{nanos}", call.pid);
    let record = dir.join(format!("{name}.record.json"));
    let plan_file = dir.join(format!("{name}.plan.json"));
    let plan = Plan {
        call: call.clone(),
        step,
        identity,
        start_ms: ms(now),
        found,
        suspended,
        record: record.to_string_lossy().into_owned(),
        build: asked.build,
    };
    let written = serde_json::to_vec(&plan)
        .map_err(std::io::Error::other)
        .and_then(|bytes| write_whole(&plan_file, &bytes));
    if written.is_err() {
        return pass("session");
    }
    Answer::Record {
        record: plan.record,
        plan: plan_file.to_string_lossy().into_owned(),
    }
}

/// A block a hint led to.
struct Hit {
    block: Block,
    description: Description,
}

/// B-103: the block for this call by its first key's hints, the newest first: each read hashed
/// as it is now, each kind, listing, link and absence checked.
fn find(
    store: &Store,
    step: &Digest,
    places: &Places,
) -> Result<Option<Hit>, crate::store::StoreError> {
    'hints: for bytes in store.hints(step) {
        let Some(hint) = Hint::read(&bytes) else {
            continue;
        };
        if !key::holds(&hint, places) {
            continue;
        }
        let mut digests = Vec::with_capacity(hint.reads.len());
        for read in &hint.reads {
            let Some(path) = places.show(read) else {
                continue 'hints;
            };
            match crate::regular::read(&path) {
                Ok(bytes) => digests.push(Digest::of(&bytes)),
                Err(_) => continue 'hints,
            }
        }
        let block_key = key::block(step, &hint, &digests);
        for key in [block_key, key::tied(&block_key, places)] {
            let Some(block) = store.block(&key)? else {
                continue;
            };
            if block.description.get("kind").and_then(|k| k.as_str()) != Some(KIND) {
                continue;
            }
            let Ok(description) = serde_json::from_value::<Description>(block.description.clone())
            else {
                continue;
            };
            if description.schema != DESCRIPTION_SCHEMA {
                continue;
            }
            return Ok(Some(Hit { block, description }));
        }
    }
    Ok(None)
}

/// B-105: puts a block in place: each file under a temporary name in its folder, checked
/// against the digest its record holds, dated now and renamed into place; the messages with
/// this checkout's root, for the hook to print.
fn restore(places: &Places, hit: &Hit) -> std::io::Result<Answer> {
    let file = |name: &str| hit.block.files.iter().find(|f| f.name == name);
    let text = |name: &str| -> std::io::Result<String> {
        match file(name) {
            Some(f) => {
                let bytes = std::fs::read(&f.path)?;
                if Digest::of(&bytes) != f.sha256 {
                    return Err(std::io::Error::other("a message changed in the store"));
                }
                String::from_utf8(bytes).map_err(std::io::Error::other)
            }
            None => Ok(String::new()),
        }
    };
    let stdout = places.show_text(&text("stdout")?);
    let stderr = places.show_text(&text("stderr")?);
    // Every path first: nothing is written for a block that names one outside the checkout.
    let mut planned = Vec::with_capacity(hit.description.outputs.len());
    for output in &hit.description.outputs {
        let dest = places
            .show(&output.path)
            .filter(|_| output.path.starts_with(&format!("{ROOT}/")))
            .ok_or_else(|| std::io::Error::other("an output outside the checkout"))?;
        let stored = file(&output.file).ok_or_else(|| std::io::Error::other("a file is missing"))?;
        planned.push((dest, stored));
    }
    for (dest, stored) in planned {
        let bytes = std::fs::read(&stored.path)?;
        if bytes.len() as u64 != stored.size || Digest::of(&bytes) != stored.sha256 {
            return Err(std::io::Error::other("a file changed in the store"));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_whole(&dest, &bytes)?;
    }
    Ok(Answer::Reused {
        stdout,
        stderr,
        code: hit.description.code,
    })
}

/// Writes `bytes` to `dest` whole: under a temporary name in its folder, then renamed.
fn write_whole(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dest.with_file_name(format!(".{name}.{}.hushrun.tmp", std::process::id()));
    let done = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, dest));
    if done.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    done
}

fn key_log(current: &Current, step: &Digest, first: &str, block: Option<&Digest>) {
    let Some(path) = &current.key_log else {
        return;
    };
    let line = serde_json::json!({
        "kind": KIND,
        "step": step,
        "first": serde_json::from_str::<serde_json::Value>(first).ok(),
        "block": block,
    });
    let Ok(mut bytes) = serde_json::to_vec(&line) else {
        return;
    };
    bytes.push(b'\n');
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(&bytes);
    }
}

/// B-103, B-104: keys and stores what the hook recorded of the compile `plan` names, and logs
/// its line. The plan and the record are removed whatever happens.
pub fn store(var: &dyn Fn(&str) -> Option<OsString>, now: SystemTime, plan_file: &Path) {
    let Some(current) = session::current(var) else {
        return;
    };
    let Some(tsc) = current.tsc.clone() else {
        return;
    };
    let Some(plan) = std::fs::read(plan_file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Plan>(&bytes).ok())
    else {
        let _ = std::fs::remove_file(plan_file);
        return;
    };
    let _ = std::fs::remove_file(plan_file);
    let record = std::fs::read(&plan.record)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok());
    let _ = std::fs::remove_file(&plan.record);
    let places = Places::new(&tsc.roots);
    let compile_ms = ms(now).saturating_sub(plan.start_ms);
    let base = TscEntry {
        call: call_words(&plan.call, &places),
        took_ms: compile_ms,
        ..TscEntry::default()
    };
    let not_stored = |why: NotStored, words: &str| {
        current.log(&Entry {
            rustc_ms: compile_ms,
            step: Some(plan.step),
            found: plan.found.is_some(),
            ..entry(
                Outcome::BuiltNotStored(why),
                TscEntry {
                    why: Some(words.to_string()),
                    ..base.clone()
                },
            )
        });
    };
    let Some(record) = record.filter(|r| r.schema == RECORD_SCHEMA) else {
        return not_stored(NotStored::Messages, "no-record");
    };
    if plan.suspended {
        return not_stored(NotStored::Suspended, "suspended");
    }
    if !record.errors.is_empty() {
        return not_stored(NotStored::Messages, "hook-error");
    }
    if let Some(other) = record.other.first() {
        return not_stored(NotStored::Unkeyable, other);
    }
    // B-104: the compiler's own statuses: 0, and 1 and 2 for errors reported, with outputs
    // skipped or written. Any other (a crash, a cycle of references) stores nothing.
    if !matches!(record.code, 0..=2) {
        return not_stored(NotStored::RustcFailed, "status");
    }
    // B-104: `--build` decides by dates where it finds a build's state: only a build that found
    // none is stored.
    if plan.build
        && record
            .reads
            .iter()
            .any(|(path, _, _)| path.ends_with(".tsbuildinfo"))
    {
        return not_stored(NotStored::Unkeyable, "build-over-outputs");
    }
    let (hint, digests) = match key::hint_of(&record, &places, &plan.call.compiler) {
        Ok(made) => made,
        Err(why) => return not_stored(NotStored::Unkeyable, why),
    };
    if record.stdout.contains(ROOT) || record.stderr.contains(ROOT) {
        return not_stored(NotStored::Messages, "placeholder");
    }
    let mut files = Vec::new();
    let mut outputs = Vec::new();
    let mut tied = hint.outside();
    let mut stored_bytes = 0u64;
    let mut ours: Vec<(String, Digest)> = Vec::new();
    for (i, written) in record.writes.iter().enumerate() {
        let Some(path) = places.hide(written).filter(|p| p.starts_with(&format!("{ROOT}/"))) else {
            return not_stored(NotStored::Outputs, "write-outside-roots");
        };
        let bytes = match crate::regular::read(Path::new(written)) {
            Ok(bytes) => bytes,
            Err(_) => return not_stored(NotStored::Outputs, "output-gone"),
        };
        if places.roots().iter().any(|root| holds_text(&bytes, root)) {
            tied = true;
        }
        stored_bytes += bytes.len() as u64;
        let name = format!("o/{i}");
        ours.push((path.clone(), Digest::of(&bytes)));
        outputs.push(Output {
            path,
            file: name.clone(),
        });
        files.push(NewFile {
            name,
            source: PathBuf::from(written),
        });
    }
    let dir = current.folder.join(WORK);
    let stem = plan_file
        .file_name()
        .map(|n| n.to_string_lossy().trim_end_matches(".plan.json").to_string())
        .unwrap_or_default();
    let mut temps = Vec::new();
    for (name, text) in [("stdout", &record.stdout), ("stderr", &record.stderr)] {
        if text.is_empty() {
            continue;
        }
        let hidden = places.hide_text(text);
        ours.push((name.to_string(), Digest::of(hidden.as_bytes())));
        let temp = dir.join(format!("{stem}.{name}"));
        if write_whole(&temp, hidden.as_bytes()).is_err() {
            return not_stored(NotStored::Messages, "messages");
        }
        files.push(NewFile {
            name: name.to_string(),
            source: temp.clone(),
        });
        temps.push(temp);
    }
    let cleanup = || {
        for temp in &temps {
            let _ = std::fs::remove_file(temp);
        }
    };
    let store = Store::new(&current.store);
    let block_key = key::block(&plan.step, &hint, &digests);
    let key = if tied {
        key::tied(&block_key, &places)
    } else {
        block_key
    };
    key_log(&current, &plan.step, "null", Some(&key));
    // B-16 read for TypeScript: a compile with a block found for it (observe, an audit) is
    // compared with it; a difference suspends the first key.
    if let Some(found) = plan.found {
        let theirs = store.block(&found).ok().flatten().map(|block| {
            let description: Option<Description> =
                serde_json::from_value(block.description.clone()).ok();
            let mut list: Vec<(String, Digest)> = description
                .map(|d| {
                    d.outputs
                        .iter()
                        .filter_map(|o| {
                            let f = block.files.iter().find(|f| f.name == o.file)?;
                            Some((o.path.clone(), f.sha256))
                        })
                        .collect()
                })
                .unwrap_or_default();
            for name in ["stdout", "stderr"] {
                if let Some(f) = block.files.iter().find(|f| f.name == name) {
                    list.push((name.to_string(), f.sha256));
                }
            }
            list
        });
        let mut mine = ours.clone();
        mine.sort();
        let theirs = theirs.map(|mut t| {
            t.sort();
            t
        });
        if theirs.as_ref() != Some(&mine) {
            let _ = store.suspend(
                &plan.step,
                "a TypeScript compile wrote other files than its block",
                now,
            );
            cleanup();
            return current.log(&Entry {
                rustc_ms: compile_ms,
                step: Some(plan.step),
                key: Some(found),
                found: true,
                differed: true,
                note: Some(format!(
                    "build: a TypeScript compile ({}) wrote other files than the block found for it; its key is suspended",
                    base.call
                )),
                ..entry(
                    Outcome::BuiltNotStored(NotStored::Differed),
                    TscEntry {
                        why: Some("differed".into()),
                        ..base.clone()
                    },
                )
            });
        }
    }
    if let Some(min) = u64::from(current.min_free_gb).checked_mul(1 << 30)
        && min > 0
        && store.free_bytes().is_ok_and(|free| free < min)
    {
        cleanup();
        return not_stored(NotStored::LowSpace, "low-space");
    }
    let local: Vec<String> = if tied { vec!["tied".to_string()] } else { Vec::new() };
    let description = Description {
        schema: DESCRIPTION_SCHEMA.to_string(),
        kind: KIND.to_string(),
        step: plan.step,
        call: base.call.clone(),
        compile_ms,
        place: if tied { "tied" } else { "anywhere" }.to_string(),
        local: local.clone(),
        platform: plan.call.platform.clone(),
        code: record.code,
        outputs,
        repository: current.repository.clone(),
    };
    let Ok(description) = serde_json::to_value(&description) else {
        cleanup();
        return not_stored(NotStored::StoreRefused, "store");
    };
    let published = store
        .add_hint(&plan.step, &hint.bytes(), now)
        .and_then(|()| store.publish(&key, &description, &files, now));
    cleanup();
    match published {
        Ok(published) => {
            let _ = store.set_step_cost(&plan.step, compile_ms, now);
            current.log(&Entry {
                rustc_ms: compile_ms,
                stored_bytes: if published == Published::Stored {
                    stored_bytes
                } else {
                    0
                },
                step: Some(plan.step),
                key: Some(key),
                found: plan.found.is_some(),
                ..entry(
                    Outcome::BuiltAndStored,
                    TscEntry {
                        local,
                        files: record.writes.len() as u64,
                        ..base
                    },
                )
            });
        }
        Err(_) => not_stored(NotStored::StoreRefused, "store"),
    }
}

/// Whether `bytes` hold `text`.
fn holds_text(bytes: &[u8], text: &str) -> bool {
    let needle = text.as_bytes();
    !needle.is_empty() && bytes.windows(needle.len()).any(|w| w == needle)
}

/// How long a lookup may take before the hook stops waiting is the hook's own: none. A lookup
/// reads only the store and the files a hint names.
#[allow(dead_code)]
const _NO_WAIT: Duration = Duration::ZERO;

#[cfg(test)]
mod tests;
