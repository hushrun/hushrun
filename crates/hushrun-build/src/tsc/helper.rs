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
use crate::relay::{Answer as RelayAnswer, Problem, Request};
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
    /// Node's platform: the manifest a block goes in (B-108).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub platform: String,
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
    /// What the compile took, in milliseconds: what a reuse saves. A download's pack carries
    /// none: the relay's word for it is `rustc_ms` (B-28).
    #[serde(default)]
    compile_ms: u64,
    #[serde(default, skip_serializing)]
    rustc_ms: u64,
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
    /// The repository that built it; a download's pack carries none.
    #[serde(default)]
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
            let text =
                serde_json::to_string(&answer).unwrap_or_else(|_| "{\"do\":\"pass\"}".into());
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
    let cwd = cwd
        .strip_prefix(ROOT)
        .unwrap_or(&cwd)
        .trim_start_matches('/');
    let cwd = if cwd.is_empty() { "." } else { cwd };
    let mut words = vec![format!("{cwd}: tsc")];
    words.extend(call.args.iter().map(|a| places.hide_text(a)));
    words.join(" ")
}

fn log_pass(current: &Current, call: &Call, places: &Places, why: &str) {
    let platform = call.platform.clone();
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
            platform,
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
        .map(|name| {
            (
                name.clone(),
                var(name).map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    let (step, body) = key::step(&call, &identity, &places, &cwd, case_sensitive, person);
    key_log(&current, &step, &body, None);
    let suspended = store.suspended_at(&step, now).is_some();
    let mut found = None;
    if !suspended {
        match find(var, &current, &store, &step, &places, &call.platform, now) {
            // B-108: who vouches for it, before anything else.
            Ok(Some(hit)) if !vet(var, &current, &hit, now) => {}
            Ok(Some(hit)) => {
                let origin = store.origin(&hit.block.key);
                let downloaded = origin
                    .as_ref()
                    .is_some_and(crate::origin::Origin::downloaded_only);
                let first_download = downloaded && origin.as_ref().is_some_and(|o| !o.reused);
                let audited = current
                    .audits_reuse(origin.as_ref().filter(|_| first_download), |reuse| {
                        crate::audit::sample(&hit.block.key, current.started_ms, reuse)
                    });
                if current.mode == Mode::On && !audited {
                    return match restore(&places, &hit, downloaded) {
                        Ok(reused) => {
                            let _ = store.used(&hit.block.key, now);
                            if first_download {
                                let _ = store.update_origin(&hit.block.key, |origin| {
                                    let mut origin = origin?;
                                    (!origin.reused).then(|| {
                                        origin.reused = true;
                                        origin
                                    })
                                });
                            }
                            let took = ms(SystemTime::now()).saturating_sub(ms(now));
                            current.log(&Entry {
                                saved_ms: hit.description.compile_ms.saturating_sub(took),
                                found: true,
                                downloaded,
                                step: Some(step),
                                key: Some(hit.block.key),
                                ..entry(
                                    Outcome::Reused,
                                    TscEntry {
                                        call: call_words(&call, &places),
                                        local: hit.description.local.clone(),
                                        took_ms: took,
                                        files: hit.description.outputs.len() as u64,
                                        platform: call.platform.clone(),
                                        ..TscEntry::default()
                                    },
                                )
                            });
                            reused
                        }
                        // B-105: a restore that fails part way runs the compiler, which writes
                        // over what was put in place. B-108: a download that fails its checks
                        // is reported, its first key suspended and the block removed.
                        Err(_) => {
                            if downloaded {
                                reject(var, &current, &store, &step, &hit.block.key, now);
                            }
                            pass("restore")
                        }
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

/// The most second keys one fetch names (the `rustc` wrapper's limit).
const MOST_FETCH_KEYS: usize = 8;
/// The most paths a downloaded hint may make a lookup hash or look at: a hint is untrusted
/// bytes, and what it names sets a lookup's cost.
const MOST_DOWNLOADED_PATHS: usize = 200_000;

/// Whether a downloaded hint names only paths under the checkout and names above it, within
/// what a lookup follows: one naming `/etc/…` would make this machine hash a file it never
/// reads, or tell by a lookup's outcome whether a file exists there.
fn may_follow(hint: &Hint) -> bool {
    let under = |p: &String| p == ROOT || p.starts_with(&format!("{ROOT}/"));
    let count = hint.reads.len() + hint.looks.len() + hint.lists.len() + hint.links.len();
    count + hint.above.len() <= MOST_DOWNLOADED_PATHS
        && !hint.outside()
        && hint.reads.iter().all(under)
        && hint.looks.iter().all(|(p, _)| under(p))
        && hint.lists.iter().all(|(p, _)| under(p))
        && hint
            .links
            .iter()
            .all(|(p, to)| under(p) && to.as_ref().is_none_or(under))
        && hint
            .above
            .iter()
            .all(|n| !n.starts_with('/') && !n.split('/').any(|p| p == ".." || p.is_empty()))
}

/// B-103, B-108: the block for this call by its first key's hints, the newest first: this
/// machine's own, then those downloaded from the shared store, asked of the run's relay when the
/// store has none; and when one leads to a second key the store lacks, a fetch of it. The relay
/// looks a first key up only when the check's `tsc/1` manifest names it.
#[allow(clippy::too_many_arguments)]
fn find(
    var: &dyn Fn(&str) -> Option<OsString>,
    current: &Current,
    store: &Store,
    step: &Digest,
    places: &Places,
    platform: &str,
    now: SystemTime,
) -> Result<Option<Hit>, crate::store::StoreError> {
    let mut candidates = Vec::new();
    if let Some(hit) = follow(
        store,
        step,
        places,
        &store.hints(step),
        false,
        &mut candidates,
    )? {
        return Ok(Some(hit));
    }
    // A first key kept on this machine asks the shared store nothing (B-34).
    if store.kept_local_at(step, now).is_some() {
        return Ok(None);
    }
    let asks = current.mode == Mode::On
        && current.share.relay().is_some()
        && hushrun_core::blocks::is_platform(platform);
    if !asks {
        return Ok(None);
    }
    let waits = crate::cc::share::Waits::of(var);
    let local_ms = store.step_cost(step);
    let mut downloaded = store.shared_hints(step);
    if downloaded.is_empty() {
        let asked = Request::TscHints {
            step: *step,
            platform: platform.to_string(),
            local_ms,
        };
        let answer = crate::cc::share::ask(
            current,
            &asked,
            waits.within(local_ms),
            &waits,
            &mut crate::relay::ask,
        );
        if matches!(answer, Some(RelayAnswer::Hints { count }) if count > 0) {
            downloaded = store.shared_hints(step);
        }
    }
    let mut fetchable = Vec::new();
    if let Some(hit) = follow(store, step, places, &downloaded, true, &mut fetchable)? {
        return Ok(Some(hit));
    }
    fetchable.truncate(MOST_FETCH_KEYS);
    if fetchable.is_empty() {
        return Ok(None);
    }
    let asked = Request::TscFetch {
        step: *step,
        keys: fetchable.clone(),
        platform: platform.to_string(),
        local_ms,
    };
    let answer = crate::cc::share::ask(
        current,
        &asked,
        waits.within(local_ms),
        &waits,
        &mut crate::relay::ask,
    );
    match answer {
        Some(RelayAnswer::Ready { key }) if fetchable.contains(&key) => block_of(store, &key),
        _ => Ok(None),
    }
}

/// Follows `hints`: the first whose every read, kind, listing, link and absence holds here and
/// that names a stored block of this kind wins; the keys it names that the store lacks are kept
/// in `candidates`, for the relay to fetch. `downloaded`: the hints came from the shared store,
/// and one [`may_follow`] refuses is passed over.
fn follow(
    store: &Store,
    step: &Digest,
    places: &Places,
    hints: &[Vec<u8>],
    downloaded: bool,
    candidates: &mut Vec<Digest>,
) -> Result<Option<Hit>, crate::store::StoreError> {
    'hints: for bytes in hints {
        let Some(hint) = Hint::read(bytes) else {
            continue;
        };
        if downloaded && !may_follow(&hint) {
            continue;
        }
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
        let keys = if downloaded {
            vec![block_key]
        } else {
            vec![block_key, key::tied(&block_key, places)]
        };
        for key in keys {
            if let Some(hit) = block_of(store, &key)? {
                return Ok(Some(hit));
            }
            if !candidates.contains(&key) {
                candidates.push(key);
            }
        }
    }
    Ok(None)
}

/// The stored block under `key`, when it is a TypeScript block this hushrun reads.
fn block_of(store: &Store, key: &Digest) -> Result<Option<Hit>, crate::store::StoreError> {
    let Some(block) = store.block(key)? else {
        return Ok(None);
    };
    if block.description.get("kind").and_then(|k| k.as_str()) != Some(KIND) {
        return Ok(None);
    }
    let Ok(description) = serde_json::from_value::<Description>(block.description.clone()) else {
        return Ok(None);
    };
    if description.schema != DESCRIPTION_SCHEMA {
        return Ok(None);
    }
    let mut description = description;
    description.compile_ms = description.compile_ms.max(description.rustc_ms);
    Ok(Some(Hit { block, description }))
}

/// B-108: whether a block found may be served as far as who vouches for it goes
/// ([`Shared::may_reuse`](crate::session::Shared::may_reuse)): one built here may; one only
/// downloaded is asked about.
fn vet(
    var: &dyn Fn(&str) -> Option<OsString>,
    current: &Current,
    hit: &Hit,
    now: SystemTime,
) -> bool {
    let store = Store::new(&current.store);
    let origin = store.origin(&hit.block.key);
    let outputs = hushrun_core::blocks::outputs_digest(
        &hit.block
            .files
            .iter()
            .map(|f| hushrun_core::blocks::OutputFile {
                name: &f.name,
                size: f.size,
                sha256: hushrun_core::blocks::Hex32(*f.sha256.as_bytes()),
            })
            .collect::<Vec<_>>(),
    );
    let waits = crate::cc::share::Waits::of(var);
    let vouch = Request::TscVouch {
        key: hit.block.key,
        outputs,
    };
    current.may_reuse(origin.as_ref(), ms(now), vouch, &mut |_, request| {
        let wait = if matches!(request, Request::Sharing) {
            waits.quick
        } else {
            waits.within(Some(hit.description.compile_ms))
        };
        crate::cc::share::ask(current, request, wait, &waits, &mut crate::relay::ask)
    })
}

/// B-108: a downloaded block that failed its checks: reported `rejected` to the scope its record
/// names, its first key suspended for the store's kept days, and the block removed.
fn reject(
    var: &dyn Fn(&str) -> Option<OsString>,
    current: &Current,
    store: &Store,
    step: &Digest,
    key: &Digest,
    now: SystemTime,
) {
    let waits = crate::cc::share::Waits::of(var);
    let _ = crate::cc::share::ask(
        current,
        &Request::TscReport {
            key: *key,
            problem: Problem::Rejected,
        },
        waits.quick,
        &waits,
        &mut crate::relay::ask,
    );
    let until = now + Duration::from_secs(u64::from(current.keep_days) * 86_400);
    let _ = store.suspend_until(
        step,
        "a downloaded TypeScript block failed its checks",
        until,
        now,
    );
    let _ = store.discard(key);
}

/// A downloaded block's messages lose every control character but a newline, a tab and SGR
/// colour (`ESC [ … m`), as a C or C++ object's do (B-59).
fn only_colour(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            let mut seq = String::from("\u{1b}[");
            chars.next();
            let mut ok = false;
            while let Some(&n) = chars.peek() {
                chars.next();
                if n == 'm' {
                    ok = true;
                    break;
                }
                if !(n.is_ascii_digit() || n == ';') {
                    break;
                }
                seq.push(n);
            }
            if ok {
                seq.push('m');
                out.push_str(&seq);
            }
            continue;
        }
        if c.is_control() && c != '\n' && c != '\t' && c != '\r' {
            continue;
        }
        out.push(c);
    }
    out
}

/// B-105: puts a block in place: each file under a temporary name in its folder, checked
/// against the digest its record holds, dated now and renamed into place; the messages with
/// this checkout's root, for the hook to print.
fn restore(places: &Places, hit: &Hit, downloaded: bool) -> std::io::Result<Answer> {
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
    let shown = |t: String| {
        let t = places.show_text(&t);
        if downloaded { only_colour(&t) } else { t }
    };
    let stdout = shown(text("stdout")?);
    let stderr = shown(text("stderr")?);
    // Every path first: nothing is written for a block that names one outside the checkout.
    let mut planned = Vec::with_capacity(hit.description.outputs.len());
    for output in &hit.description.outputs {
        let dest = places
            .show(&output.path)
            .filter(|_| output.path.starts_with(&format!("{ROOT}/")))
            .ok_or_else(|| std::io::Error::other("an output outside the checkout"))?;
        let stored =
            file(&output.file).ok_or_else(|| std::io::Error::other("a file is missing"))?;
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
        platform: plan.call.platform.clone(),
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
        let Some(path) = places
            .hide(written)
            .filter(|p| p.starts_with(&format!("{ROOT}/")))
        else {
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
        .map(|n| {
            n.to_string_lossy()
                .trim_end_matches(".plan.json")
                .to_string()
        })
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
            // A block only downloaded is reported to the scope it came from, before it goes.
            if store
                .origin(&found)
                .as_ref()
                .is_some_and(crate::origin::Origin::downloaded_only)
            {
                let waits = crate::cc::share::Waits::of(var);
                let _ = crate::cc::share::ask(
                    &current,
                    &Request::TscReport {
                        key: found,
                        problem: Problem::Differs,
                    },
                    waits.quick,
                    &waits,
                    &mut crate::relay::ask,
                );
                let _ = store.discard(&found);
            }
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
    let local: Vec<String> = if tied {
        vec!["tied".to_string()]
    } else {
        Vec::new()
    };
    let description = Description {
        schema: DESCRIPTION_SCHEMA.to_string(),
        kind: KIND.to_string(),
        step: plan.step,
        call: base.call.clone(),
        compile_ms,
        rustc_ms: 0,
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
    // B-32: built here, by this run, and by whom when the run shares.
    let relay = current.share.relay();
    let run = relay
        .map(|share| share.run.clone())
        .or_else(|| {
            current
                .folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let origin = crate::origin::Origin::built_here(&run, relay.map(|share| share.by.clone()));
    let published = store
        .add_hint(&plan.step, &hint.bytes(), now)
        .and_then(|()| store.publish_with_origin(&key, &description, &files, &origin, now));
    cleanup();
    match published {
        Ok(published) => {
            let _ = store.set_step_cost(&plan.step, compile_ms, now);
            // B-108: a block that may leave the machine goes up through the run's relay.
            if !tied && hushrun_core::blocks::is_platform(&plan.call.platform) {
                let waits = crate::cc::share::Waits::of(var);
                crate::cc::share::tell(
                    &current,
                    &Request::TscBuilt {
                        key,
                        step: plan.step,
                        cost_ms: compile_ms,
                    },
                    &waits,
                    &mut crate::relay::tell,
                );
            }
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
#[test]
fn a_download_s_messages_keep_only_their_colour() {
    assert_eq!(
        only_colour("\u{1b}[91merror\u{1b}[0m TS1\u{7}\u{1b}]0;title\n"),
        "\u{1b}[91merror\u{1b}[0m TS1]0;title\n"
    );
}

#[cfg(test)]
mod tests;
