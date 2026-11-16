//! The cache program (B-40 to B-47): what the `go` command talks to over stdin and stdout when
//! `GOCACHEPROG` names `hushrun-gocache`.
//!
//! **The protocol** (`cmd/go/internal/cacheprog`, Go 1.24 to 1.27). The program first writes
//! `{"ID":0,"KnownCommands":["get","put","close"]}`. Then Go sends one JSON line per request,
//! many at once, and a `put` with a body is followed by a line holding the body as a base64 JSON
//! string. Answers go in any order, one line each, matched by `ID`. Go waits for each answer
//! without a limit, takes an answer to an id it doesn't know (a second answer included) or a line
//! that isn't JSON as fatal, and fails the whole command when the program exits before `close`.
//!
//! **Each request is a state machine** of two states, `open` and `answered`, moved by one
//! compare-and-set (`Program::answer`): the handler's answer, the answer a panic leaves
//! (`catch_unwind`), and the watchdog's for a request open past its budget all race for it, and
//! only the first is written. A `get` that fails in any way is a miss; a `put` always answers a
//! file holding the body, in Go's cache, else in a folder of the program's own.
//!
//! **What a `get` serves**, in this order: Go's own cache (`GOCACHE`), as plain `go` would; else
//! a block in hushrun's store, checked byte for byte ([`super::archive::portable`]) and only as
//! [`crate::session::Shared::may_reuse`] lets a downloaded one be reused; else, when the run's
//! relay holds the run's Go manifest naming it, a download. One time in fifty (one in ten on a
//! downloaded archive's first reuse here) a store hit is answered as a miss on purpose, never
//! twice for one key in a run, and what Go then compiles is compared with it (B-44).
//!
//! **What a `put` keeps**: every body, in `GOCACHE` in Go's format, which is what Go is handed;
//! and, when the run shares, a portable package archive also as a block in hushrun's store, for
//! the relay to upload.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use hushrun_core::blocks::Base64;
use serde::Deserialize;
use serde_json::{Value, json};

use super::archive::{self, Local};
use super::command::{self, Altered};
use super::gocache::{self, GoCache};
use super::toolchain::Toolchain;
use crate::model::{Class, Kind, Mode, NotStored, Outcome};
use crate::origin::Origin;
use crate::relay::{self, Answer, Problem, Request};
use crate::session::{Current, Entry, GoEntry};
use crate::store::{NewFile, Store};

/// The longest a `get` waits for the relay (B-40's ceiling: half of a fetch's most cost).
const RELAY_WAIT: Duration = Duration::from_secs(30);
/// How long a question the relay answers from what it holds may take: whether the run still
/// shares, a notice.
const QUICK_WAIT: Duration = Duration::from_secs(2);
/// A request still open this long is answered by the watchdog: a miss for a `get`, the body in a
/// file of the program's own for a `put`.
const BUDGET: Duration = Duration::from_secs(40);
/// How often the watchdog looks.
const TICK: Duration = Duration::from_millis(50);
/// The session's folder of bodies handed to Go from hushrun's store, each named by its SHA-256
/// (F8): never rewritten or removed while the run lives.
const BODIES: &str = "go-bodies";
/// The session's folder of keys audited in the run: one empty file each, made with `create_new`
/// so that two `go` commands of one run never audit one key twice (B-44).
const AUDITED: &str = "go-audited";
/// The only file of a Go block in the store.
pub const ARCHIVE: &str = "archive";
/// The schema of a Go block's description.
pub const BLOCK_SCHEMA: &str = "hushrun.go-block/v1";

/// Test only, never set in a release build: the watchdog's budget in milliseconds, and the
/// relay's wait at half of it.
#[cfg(debug_assertions)]
const BUDGET_VAR: &str = "HUSHRUN_GOCACHE_BUDGET_MS";
/// Test only, never read in a release build: `hang-get=N`, `slow-get=N`, `panic-get=N`,
/// `panic-put=N`, comma separated: the N-th request of that command hangs, answers after twice
/// the budget, or panics in its handler.
#[cfg(debug_assertions)]
const FAULT_VAR: &str = "HUSHRUN_GOCACHE_FAULT";

/// One request of the `go` command, as Go writes it.
#[derive(Deserialize)]
struct Wire {
    #[serde(rename = "ID")]
    id: i64,
    #[serde(rename = "Command")]
    command: String,
    #[serde(rename = "ActionID", default)]
    action: Option<Base64>,
    #[serde(rename = "OutputID", default)]
    output: Option<Base64>,
    #[serde(rename = "BodySize", default)]
    body_size: i64,
}

/// What a request asks, read.
enum Asked {
    Get {
        action: [u8; 32],
    },
    Put {
        action: [u8; 32],
        output: [u8; 32],
        body: Arc<Vec<u8>>,
    },
    Close,
    /// A command this program never declared, or ids it can't read: answered with an error.
    Bad(String),
}

/// What answers a request when its handler can't: a miss, or the body in a file.
#[derive(Clone)]
enum Fallback {
    Miss,
    Put {
        output: [u8; 32],
        body: Arc<Vec<u8>>,
    },
    Close,
}

/// One request: open, then answered once (rule 1).
struct Slot {
    id: i64,
    answered: AtomicBool,
    since: Instant,
    fallback: Fallback,
}

/// An audit the program started by answering a miss (B-44): what Go's compile is compared with.
#[derive(Clone)]
struct Audit {
    key: Digest,
    sha256: Digest,
    downloaded: bool,
}

/// How long the program waits, by default and in a test.
#[derive(Clone, Copy)]
struct Waits {
    budget: Duration,
    relay: Duration,
}

/// Faults a test injects into handlers (never in a release build).
#[derive(Default)]
#[cfg_attr(not(debug_assertions), allow(dead_code))]
struct Faults {
    hang_get: Option<u64>,
    slow_get: Option<u64>,
    panic_get: Option<u64>,
    panic_put: Option<u64>,
    gets: AtomicU64,
    puts: AtomicU64,
}

/// The program, for one `go` command.
struct Program {
    /// Go's cache, when its format is one this program knows; else bodies go to `temp`.
    gocache: Option<GoCache>,
    /// A folder of the program's own, removed at `close`: puts with no `GOCACHE` (B-47), and a
    /// put whose `GOCACHE` write failed.
    temp: PathBuf,
    /// The run's session, when there is a usable one.
    current: Option<Current>,
    toolchain: Option<Toolchain>,
    /// The parent's arguments and `GOFLAGS`, read once (B-42).
    altered: OnceLock<Option<Altered>>,
    /// hushrun's identity of the toolchain (B-43), computed once.
    identity: OnceLock<Result<Digest, String>>,
    out: Mutex<Box<dyn Write + Send>>,
    open: Mutex<HashMap<i64, Arc<Slot>>>,
    /// Handlers still running, answered or not: `close` waits for them, within the budget.
    busy: AtomicUsize,
    /// When each action missed, for its compile's cost (the time between Go's miss and its put).
    misses: Mutex<HashMap<[u8; 32], Instant>>,
    /// The size of each body put by action: Go puts a compile's output (`Subkey(id, "stdout")`)
    /// just before its archive, and waits for the answer.
    put_sizes: Mutex<HashMap<[u8; 32], u64>>,
    audits: Mutex<HashMap<[u8; 32], Audit>>,
    /// A call to the relay failed or timed out in this command: none is made after it (F10).
    relay_down: AtomicBool,
    waits: Waits,
    faults: Faults,
    env: HashMap<String, OsString>,
    /// `close` was answered, or the input ended: the watchdog stops.
    done: AtomicBool,
    parent: Parent,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now() -> SystemTime {
    SystemTime::now()
}

fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn epoch_ns(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// A time as Go's `time.Time` writes it in JSON (RFC 3339 with nanoseconds, in UTC, trailing
/// zeros of the fraction left out).
pub(crate) fn rfc3339(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let nanos = ns.rem_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let mut text = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    );
    if nanos != 0 {
        let fraction = format!("{nanos:09}");
        text.push('.');
        text.push_str(fraction.trim_end_matches('0'));
    }
    text.push('Z');
    text
}

fn b64(bytes: &[u8]) -> Value {
    serde_json::to_value(Base64(bytes.to_vec())).unwrap_or(Value::Null)
}

fn id32(field: Option<Base64>) -> Option<[u8; 32]> {
    let bytes = field?.0;
    bytes.try_into().ok()
}

/// The program's entry point, on the process's own stdin and stdout.
pub fn main() -> std::process::ExitCode {
    // The program's stderr is the `go` command's, which is the person's: a panic, which only
    // ever ends one handler in an answer of a miss, says nothing there.
    std::panic::set_hook(Box::new(|_| {}));
    let env: HashMap<String, OsString> = std::env::vars_os()
        .filter_map(|(k, v)| k.into_string().ok().map(|k| (k, v)))
        .collect();
    let stdout: Box<dyn Write + Send> = Box::new(io::stdout());
    serve(BufReader::new(io::stdin()), stdout, env);
    std::process::ExitCode::SUCCESS
}

/// Serves one `go` command: declares the commands, answers every request, returns at `close`
/// or at the end of the input. Never panics out, never returns early.
pub fn serve(input: impl BufRead, output: Box<dyn Write + Send>, env: HashMap<String, OsString>) {
    serve_with(input, output, env, Box::new(command::parent_command));
}

/// What reads the `go` command's arguments, given its binary: the parent process, or a test's.
pub(crate) type Parent = Box<dyn Fn(&Path) -> Result<String, Altered> + Send + Sync>;

/// [`serve`], with the `go` command's arguments read by `parent`.
pub(crate) fn serve_with(
    input: impl BufRead,
    output: Box<dyn Write + Send>,
    env: HashMap<String, OsString>,
    parent: Parent,
) {
    let program = Arc::new(Program::new(output, env, parent));
    program.write(&json!({"ID": 0, "KnownCommands": ["get", "put", "close"]}));
    program.start_background();
    let watchdog = {
        let program = program.clone();
        std::thread::Builder::new()
            .name("hushrun-gocache-watchdog".into())
            .spawn(move || program.watch())
    };
    program.read(input);
    drop(watchdog);
}

impl Program {
    fn new(
        output: Box<dyn Write + Send>,
        env: HashMap<String, OsString>,
        parent: Parent,
    ) -> Program {
        let get = |name: &str| env.get(name).cloned().filter(|v| !v.is_empty());
        let known = get("GOVERSION")
            .and_then(|v| v.into_string().ok())
            .is_some_and(|v| command::version_known(&v));
        let gocache = get("GOCACHE")
            .map(PathBuf::from)
            .filter(|dir| known && dir.is_absolute() && dir.is_dir())
            .map(|dir| GoCache::new(&dir));
        // B-47: a session that can't be read, or none, is no session: Go's own cache only.
        let lookup = |name: &str| get(name);
        let current = crate::session::current(&lookup).filter(|c| c.mode != Mode::Off);
        let current = current.filter(|_| gocache.is_some());
        let toolchain = Toolchain::from_env(&lookup);
        static PROGRAMS: AtomicU64 = AtomicU64::new(0);
        let temp = std::env::temp_dir().join(format!(
            "hushrun-gocache-{}-{}-{}",
            std::process::id(),
            epoch_ns(now()),
            PROGRAMS.fetch_add(1, Ordering::Relaxed)
        ));
        #[allow(unused_mut)]
        let mut waits = Waits {
            budget: BUDGET,
            relay: RELAY_WAIT,
        };
        #[allow(unused_mut)]
        let mut faults = Faults::default();
        #[cfg(debug_assertions)]
        {
            if let Some(ms) = get(BUDGET_VAR)
                .and_then(|v| v.into_string().ok())
                .and_then(|v| v.parse::<u64>().ok())
            {
                waits.budget = Duration::from_millis(ms);
                waits.relay = Duration::from_millis(ms / 2);
            }
            if let Some(text) = get(FAULT_VAR).and_then(|v| v.into_string().ok()) {
                for part in text.split(',') {
                    let Some((what, n)) = part.split_once('=') else {
                        continue;
                    };
                    let n = n.parse().ok();
                    match what {
                        "hang-get" => faults.hang_get = n,
                        "slow-get" => faults.slow_get = n,
                        "panic-get" => faults.panic_get = n,
                        "panic-put" => faults.panic_put = n,
                        _ => {}
                    }
                }
            }
        }
        Program {
            gocache,
            temp,
            current,
            toolchain,
            altered: OnceLock::new(),
            identity: OnceLock::new(),
            out: Mutex::new(output),
            open: Mutex::new(HashMap::new()),
            busy: AtomicUsize::new(0),
            misses: Mutex::new(HashMap::new()),
            put_sizes: Mutex::new(HashMap::new()),
            audits: Mutex::new(HashMap::new()),
            relay_down: AtomicBool::new(false),
            waits,
            faults,
            env,
            done: AtomicBool::new(false),
            parent,
        }
    }

    fn var(&self, name: &str) -> Option<OsString> {
        self.env.get(name).cloned().filter(|v| !v.is_empty())
    }

    /// One line to Go. Only [`Program::answer`] and the first line call it.
    fn write(&self, value: &Value) {
        let Ok(mut line) = serde_json::to_vec(value) else {
            return;
        };
        line.push(b'\n');
        let mut out = lock(&self.out);
        let _ = out.write_all(&line);
        let _ = out.flush();
    }

    /// Answers `slot` with `value`, when nobody did: the one compare-and-set every answer goes
    /// through (rule 1).
    fn answer(&self, slot: &Slot, mut value: Value) {
        if slot
            .answered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        value["ID"] = json!(slot.id);
        self.write(&value);
        lock(&self.open).remove(&slot.id);
    }

    /// The answer a handler that can't give its own leaves: a miss, or for a put the body in a
    /// file of the program's own (an error, which Go ignores but where it needs the file at once,
    /// if even that file can't be written).
    fn fall_back(&self, slot: &Slot) {
        match &slot.fallback {
            Fallback::Miss => self.answer(slot, json!({"Miss": true})),
            Fallback::Close => self.answer(slot, json!({})),
            Fallback::Put { output, body } => match self.write_temp(output, body) {
                Ok(path) => self.answer(slot, json!({"DiskPath": path})),
                Err(err) => self.answer(slot, json!({"Err": err.to_string()})),
            },
        }
    }

    /// The body in the program's own folder, named by its SHA-256.
    fn write_temp(&self, output: &[u8; 32], body: &[u8]) -> io::Result<PathBuf> {
        let path = self.temp.join(Digest::from_bytes(*output).hex());
        if std::fs::metadata(&path).is_ok_and(|m| m.len() == body.len() as u64) {
            return Ok(path);
        }
        gocache::write_whole(&path, body, now())?;
        Ok(path)
    }

    /// Starts reading the parent's arguments and hashing the toolchain off Go's path, for a
    /// run whose session shares: the first request that needs them waits for what is left.
    fn start_background(self: &Arc<Program>) {
        if self
            .current
            .as_ref()
            .is_none_or(|c| c.share == crate::session::Shared::Off)
        {
            return;
        }
        let program = self.clone();
        let _ = std::thread::Builder::new()
            .name("hushrun-gocache-start".into())
            .spawn(move || {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    program.altered();
                    let _ = program.identity();
                }));
            });
    }

    /// How the `go` command was altered (B-42), read once.
    fn altered(&self) -> Option<Altered> {
        *self.altered.get_or_init(|| {
            let goflags = command::goflags(&|name| self.var(name));
            let Some(toolchain) = &self.toolchain else {
                return Some(Altered::Unreadable);
            };
            match (self.parent)(&command::go_binary(&toolchain.goroot)) {
                Ok(args) => command::altered(&args, &goflags),
                Err(why) => Some(why),
            }
        })
    }

    /// The toolchain's identity (B-43), computed once.
    fn identity(&self) -> Result<Digest, Local> {
        self.identity
            .get_or_init(|| match &self.toolchain {
                Some(toolchain) => {
                    toolchain.identity(self.current.as_ref().map(|c| c.folder.as_path()))
                }
                None => Err("no GOROOT or GOVERSION".into()),
            })
            .clone()
            .map_err(|_| Local::Toolchain)
    }

    /// Reads requests until `close` or the end of the input.
    fn read(self: &Arc<Program>, mut input: impl BufRead) {
        let mut line = Vec::new();
        loop {
            line.clear();
            match input.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let Ok(wire) = serde_json::from_slice::<Wire>(trim(&line)) else {
                continue;
            };
            let mut body = Vec::new();
            if wire.command == "put" && wire.body_size > 0 {
                // Go writes each request, then a blank line, then the body's line.
                let mut text = Vec::new();
                loop {
                    text.clear();
                    if input.read_until(b'\n', &mut text).unwrap_or(0) == 0 {
                        break;
                    }
                    if !trim(&text).is_empty() {
                        break;
                    }
                }
                if text.is_empty() {
                    break;
                }
                match serde_json::from_slice::<Base64>(trim(&text)) {
                    Ok(decoded) => body = decoded.0,
                    Err(_) => body.clear(),
                }
            }
            let asked = match wire.command.as_str() {
                "get" => match id32(wire.action) {
                    Some(action) => Asked::Get { action },
                    None => Asked::Bad("a get without a 32-byte ActionID".into()),
                },
                "put" => match (id32(wire.action), id32(wire.output)) {
                    (Some(action), Some(output)) if body.len() as i64 == wire.body_size.max(0) => {
                        Asked::Put {
                            action,
                            output,
                            body: Arc::new(body),
                        }
                    }
                    _ => Asked::Bad("a put whose ids or body can't be read".into()),
                },
                "close" => Asked::Close,
                other => Asked::Bad(format!("unknown command {other:?}")),
            };
            let closing = matches!(asked, Asked::Close);
            let fallback = match &asked {
                Asked::Put { output, body, .. } => Fallback::Put {
                    output: *output,
                    body: body.clone(),
                },
                Asked::Close => Fallback::Close,
                Asked::Get { .. } | Asked::Bad(_) => Fallback::Miss,
            };
            let slot = Arc::new(Slot {
                id: wire.id,
                answered: AtomicBool::new(false),
                since: Instant::now(),
                fallback,
            });
            if closing {
                self.close(&slot);
                return;
            }
            lock(&self.open).insert(slot.id, slot.clone());
            self.busy.fetch_add(1, Ordering::AcqRel);
            let program = self.clone();
            let handler_slot = slot.clone();
            let spawned = std::thread::Builder::new()
                .name("hushrun-gocache".into())
                .spawn(move || {
                    let handled =
                        catch_unwind(AssertUnwindSafe(|| program.handle(&handler_slot, asked)));
                    if handled.is_err() {
                        program.fall_back(&handler_slot);
                    }
                    program.busy.fetch_sub(1, Ordering::AcqRel);
                });
            if spawned.is_err() {
                // No thread: answered here, with what a failed handler leaves.
                self.fall_back(&slot);
                self.busy.fetch_sub(1, Ordering::AcqRel);
            }
        }
        // The end of the input with no `close`: Go is gone, or closed stdin.
        self.drain();
        self.done.store(true, Ordering::Release);
        let _ = std::fs::remove_dir_all(&self.temp);
    }

    /// Waits for the handlers still running, at most the budget: the watchdog answers what they
    /// don't.
    fn drain(&self) {
        let deadline = Instant::now() + self.waits.budget + TICK * 4;
        while self.busy.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// `close`: every handler done or given up, then the answer, after which the process exits.
    /// The program's own folder goes: Go reads nothing after `close`.
    fn close(&self, slot: &Slot) {
        self.drain();
        for open in lock(&self.open).values().cloned().collect::<Vec<_>>() {
            self.fall_back(&open);
        }
        self.answer(slot, json!({}));
        self.done.store(true, Ordering::Release);
        let _ = std::fs::remove_dir_all(&self.temp);
    }

    /// The watchdog: a request open longer than the budget gets its fallback (rule 1).
    fn watch(&self) {
        while !self.done.load(Ordering::Acquire) {
            std::thread::sleep(TICK);
            let late: Vec<Arc<Slot>> = lock(&self.open)
                .values()
                .filter(|slot| slot.since.elapsed() >= self.waits.budget)
                .cloned()
                .collect();
            for slot in late {
                self.fall_back(&slot);
            }
        }
    }

    fn handle(&self, slot: &Slot, asked: Asked) {
        match asked {
            Asked::Get { action } => {
                self.fault_get();
                let value = self.get(&action);
                self.answer(slot, value);
            }
            Asked::Put {
                action,
                output,
                body,
            } => {
                self.fault_put();
                self.put(slot, &action, &output, &body);
            }
            Asked::Bad(why) => self.answer(slot, json!({"Err": why})),
            Asked::Close => self.answer(slot, json!({})),
        }
    }

    #[cfg(debug_assertions)]
    fn fault_get(&self) {
        let n = self.faults.gets.fetch_add(1, Ordering::AcqRel) + 1;
        if self.faults.panic_get == Some(n) {
            panic!("the test's panic: a get");
        }
        if self.faults.hang_get == Some(n) {
            std::thread::sleep(Duration::from_secs(3600));
        }
        // Slower than the budget, then answering: the watchdog has answered it already.
        if self.faults.slow_get == Some(n) {
            std::thread::sleep(self.waits.budget * 2);
        }
    }

    #[cfg(not(debug_assertions))]
    fn fault_get(&self) {
        let _ = &self.faults;
    }

    #[cfg(debug_assertions)]
    fn fault_put(&self) {
        let n = self.faults.puts.fetch_add(1, Ordering::AcqRel) + 1;
        if self.faults.panic_put == Some(n) {
            panic!("the test's panic: a put");
        }
    }

    #[cfg(not(debug_assertions))]
    fn fault_put(&self) {}

    // --- get ---------------------------------------------------------------------------

    fn get(&self, action: &[u8; 32]) -> Value {
        let miss = json!({"Miss": true});
        let Some(gocache) = &self.gocache else {
            return miss;
        };
        let at = now();
        if let Some(hit) = gocache.get(action, at) {
            self.seen_in_go_cache(action, &hit);
            return json!({
                "OutputID": b64(&hit.output),
                "Size": hit.size,
                "Time": rfc3339(hit.time_ns),
                "DiskPath": hit.path,
            });
        }
        lock(&self.misses).insert(*action, Instant::now());
        match self.served_from_store(action, at) {
            Some(value) => value,
            None => miss,
        }
    }

    /// What may be served from hushrun's store for this command: a session in which this
    /// machine reuses, a command that wasn't altered, a toolchain that is known.
    fn reuse_side(&self) -> Option<(&Current, Store, Digest)> {
        let current = self.current.as_ref()?;
        if self.altered().is_some() {
            return None;
        }
        let identity = self.identity().ok()?;
        Some((current, Store::new(&current.store), identity))
    }

    /// A store hit, checked, or a download the relay brings: `None` is a miss.
    fn served_from_store(&self, action: &[u8; 32], at: SystemTime) -> Option<Value> {
        let (current, store, identity) = self.reuse_side()?;
        let key = super::key(action, &identity);
        if store.suspended_at(&key, at).is_some() {
            return None;
        }
        if let Ok(Some(_)) = store.block(&key) {
            return self.serve(current, &store, &key, action, false, at);
        }
        // B-45: a download, only where sharing is on (the relay says) and the run's Go manifest
        // names it.
        if current.mode != Mode::On {
            return None;
        }
        let asked = Request::GoFetch {
            key,
            local_ms: store.step_cost(&key),
        };
        let wait = store.step_cost(&key).map_or(self.waits.relay, |ms| {
            (Duration::from_millis(ms) / 2 + QUICK_WAIT).min(self.waits.relay)
        });
        match self.ask(&asked, wait) {
            Some(Answer::Ready { key: got }) if got == key => {
                self.serve(current, &store, &key, action, true, at)
            }
            _ => None,
        }
    }

    /// Serves the block under `key` for `action`, or `None` (a miss) when it can't be: not a Go
    /// block for this action, not vouched for (B-34), bytes that fail their checks (F2), or
    /// picked by the audit (B-44).
    fn serve(
        &self,
        current: &Current,
        store: &Store,
        key: &Digest,
        action: &[u8; 32],
        downloaded_now: bool,
        at: SystemTime,
    ) -> Option<Value> {
        let block = store.block(key).ok().flatten()?;
        let action_hex = Digest::from_bytes(*action).hex();
        if Kind::of_description(&block.description) != Kind::Go
            || block.description["action"].as_str() != Some(action_hex.as_str())
            || block.files.len() != 1
            || block.files[0].name != ARCHIVE
        {
            return None;
        }
        let file = &block.files[0];
        let origin = store.origin(key);
        let outputs = outputs_of(file.size, &file.sha256);
        let vouch = Request::GoVouch { key: *key, outputs };
        let cost = block.description["rustc_ms"].as_u64().unwrap_or(0);
        let vouch_wait = (Duration::from_millis(cost) / 2 + QUICK_WAIT).min(self.waits.relay);
        let reusable =
            current
                .share
                .may_reuse(origin.as_ref(), epoch_ms(at), vouch, &mut |_, request| {
                    let wait = if matches!(request, Request::Sharing) {
                        QUICK_WAIT
                    } else {
                        vouch_wait
                    };
                    self.ask(request, wait)
                });
        if !reusable {
            return None;
        }
        let read = Checked::of(&file.path)?;
        let bytes = &read.bytes;
        let downloaded = origin.as_ref().is_some_and(Origin::downloaded_only);
        if Digest::of(bytes) != file.sha256 || bytes.len() as u64 != file.size {
            // Changed on disk since it was stored: gone, and compiled.
            let _ = store.discard(key);
            return None;
        }
        if let Err(why) = archive::portable(bytes, action) {
            // F2: a block whose bytes aren't what its build ID says is never served, and one
            // that was downloaded is reported to the scope it came from.
            let _ = store.discard(key);
            if downloaded {
                self.tell(&Request::GoReport {
                    key: *key,
                    problem: Problem::Rejected,
                });
            }
            self.log(Entry {
                note: Some(format!(
                    "a Go package archive in the store was refused: {}",
                    why.words()
                )),
                ..go_entry(Outcome::BuiltNotStored(NotStored::Compare), key)
            });
            return None;
        }
        // B-44: one in fifty, one in ten on a downloaded archive's first reuse here, answered as
        // a miss and compared with what Go then compiles; every one in local `observe`.
        let first_download = origin
            .as_ref()
            .is_some_and(|o| o.downloaded_only() && !o.reused);
        let picked = current.mode == Mode::Observe
            || current
                .audit
                .picks(|| crate::audit::sample(key, current.started_ms, first_download));
        if picked && self.claim_audit(current, key) {
            lock(&self.audits).insert(
                *action,
                Audit {
                    key: *key,
                    sha256: file.sha256,
                    downloaded,
                },
            );
            return None;
        }
        let path = self.hand_out(current, &read, &file.sha256)?;
        let _ = store.used(key, at);
        if first_download {
            let _ = store.update_origin(key, |origin| {
                let mut origin = origin?;
                (!origin.reused).then(|| {
                    origin.reused = true;
                    origin
                })
            });
        }
        lock(&self.misses).remove(action);
        self.log(Entry {
            saved_ms: cost,
            found: true,
            downloaded: downloaded_now,
            ..go_entry(Outcome::Reused, key)
        });
        Some(json!({
            "OutputID": b64(file.sha256.as_bytes()),
            "Size": file.size,
            "Time": rfc3339(epoch_ns(at)),
            "DiskPath": path,
        }))
    }

    /// Claims the audit of `key` for the run: whether no `go` command of the run audited it yet.
    fn claim_audit(&self, current: &Current, key: &Digest) -> bool {
        let dir = current.folder.join(AUDITED);
        let _ = std::fs::create_dir_all(&dir);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(key.hex()))
            .is_ok()
    }

    /// The file Go is handed for a block's archive (F8): in the session's folder, named by its
    /// SHA-256, a link to the store's file that was read and checked (whose bytes no one
    /// rewrites: a store's file is replaced only by a rename), else a copy of the bytes checked;
    /// never rewritten or removed while the run lives, whatever happens to the block.
    fn hand_out(&self, current: &Current, read: &Checked, sha256: &Digest) -> Option<PathBuf> {
        let dir = current.folder.join(BODIES);
        let path = dir.join(sha256.hex());
        if std::fs::metadata(&path).is_ok_and(|m| m.len() == read.bytes.len() as u64) {
            return Some(path);
        }
        std::fs::create_dir_all(&dir).ok()?;
        let temp = dir.join(format!(".{}.{}.tmp", sha256.hex(), std::process::id()));
        let _ = std::fs::remove_file(&temp);
        let linked = std::fs::hard_link(&read.path, &temp).is_ok()
            && std::fs::metadata(&temp).is_ok_and(|m| same_inode(&m, &read.meta));
        if !linked {
            let _ = std::fs::remove_file(&temp);
            std::fs::write(&temp, &read.bytes).ok()?;
        }
        match std::fs::rename(&temp, &path) {
            Ok(()) => Some(path),
            Err(_) => {
                let _ = std::fs::remove_file(&temp);
                std::fs::metadata(&path)
                    .is_ok_and(|m| m.len() == read.bytes.len() as u64)
                    .then_some(path)
            }
        }
    }

    /// A hit in Go's own cache, which is served as plain `go` would serve it. When the run
    /// shares and the toolchain is known already, a package archive is logged for the
    /// manifest, and compared for free with a block of the same key in hushrun's store
    /// (B-44): a difference suspends the key here, and is reported when the block was only
    /// downloaded.
    fn seen_in_go_cache(&self, action: &[u8; 32], hit: &gocache::Hit) {
        let Some(current) = &self.current else {
            return;
        };
        if current.share.relay().is_none() {
            return;
        }
        // Never waited for: a warm build pays nothing for them.
        let (Some(Ok(identity)), Some(None)) = (self.identity.get(), self.altered.get()) else {
            return;
        };
        let mut head = [0u8; 8];
        let is_archive = std::fs::File::open(&hit.path)
            .and_then(|mut f| f.read_exact(&mut head))
            .is_ok()
            && head == *archive::AR_MAGIC;
        if !is_archive {
            return;
        }
        let key = super::key(action, identity);
        let store = Store::new(&current.store);
        let mut entry = go_entry(Outcome::Reused, &key);
        entry.found = true;
        entry.go = Some(GoEntry {
            go_cache: true,
            local: None,
        });
        if let Ok(Some(block)) = store.block(&key)
            && Kind::of_description(&block.description) == Kind::Go
            && let [file] = block.files.as_slice()
            && *file.sha256.as_bytes() != hit.output
        {
            entry.differed = true;
            self.differs(current, &store, &key);
        }
        self.log(entry);
    }

    /// A block of `key` disagrees with a compile here: the key is suspended for the store's
    /// kept days, and a block only downloaded is reported to the scope it came from.
    fn differs(&self, current: &Current, store: &Store, key: &Digest) {
        let at = now();
        let until = at + Duration::from_secs(u64::from(current.keep_days) * 86_400);
        let _ = store.suspend_until(key, "a Go compile here made other bytes", until, at);
        if store.origin(key).is_some_and(|o| o.downloaded_only()) {
            self.tell(&Request::GoReport {
                key: *key,
                problem: Problem::Differs,
            });
        }
    }

    // --- put ---------------------------------------------------------------------------

    fn put(&self, slot: &Slot, action: &[u8; 32], output: &[u8; 32], body: &Arc<Vec<u8>>) {
        let at = now();
        let sha = Digest::of(body);
        if sha.as_bytes() != output {
            self.answer(
                slot,
                json!({"Err": "the body's SHA-256 is not its OutputID"}),
            );
            return;
        }
        lock(&self.put_sizes).insert(*action, body.len() as u64);
        let path = match &self.gocache {
            Some(gocache) => gocache
                .put(action, output, body, at)
                .or_else(|_| self.write_temp(output, body)),
            None => self.write_temp(output, body),
        };
        let path = match path {
            Ok(path) => path,
            Err(err) => {
                self.answer(slot, json!({"Err": err.to_string()}));
                return;
            }
        };
        // Go goes on at once; what is kept beside its cache is decided after.
        self.answer(slot, json!({"DiskPath": path}));
        if archive::is_archive(body) {
            self.keep(action, body, &sha, &path, at);
        }
    }

    /// A package archive Go compiled: the audit's comparison, and the block, when it may leave.
    fn keep(&self, action: &[u8; 32], body: &[u8], sha: &Digest, path: &Path, at: SystemTime) {
        let Some(current) = &self.current else {
            return;
        };
        let cost_ms = lock(&self.misses)
            .remove(action)
            .map_or(0, |since| since.elapsed().as_millis() as u64);
        let store = Store::new(&current.store);
        let audit = lock(&self.audits).remove(action);
        let Ok(identity) = self.identity() else {
            return self.log_local(Local::Toolchain, None, cost_ms);
        };
        let key = super::key(action, &identity);
        if let Some(audit) = audit {
            let differed = audit.sha256 != *sha;
            if differed {
                self.differs(current, &store, &audit.key);
                if audit.downloaded {
                    // The compile here takes the downloaded block's place (B-32).
                    let _ =
                        self.publish(current, &store, &key, action, &identity, path, cost_ms, at);
                }
            }
            self.log(Entry {
                found: true,
                differed,
                rustc_ms: cost_ms,
                note: differed.then(|| {
                    format!(
                        "a Go package built differently from its block in the store; its key is not reused for {} days",
                        current.keep_days
                    )
                }),
                ..go_entry(Outcome::BuiltAndStored, &key)
            });
            return;
        }
        let local = if current.share.relay().is_none() {
            Some(Local::NotSharing)
        } else if self.altered().is_some() {
            Some(Local::Altered)
        } else if lock(&self.put_sizes).get(&super::subkey(action, "stdout")) != Some(&0) {
            // F4: what the compile printed, Go prints only from this machine's own cache.
            Some(Local::Printed)
        } else {
            archive::portable(body, action).err()
        };
        if let Some(local) = local {
            return self.log_local(local, Some(key), cost_ms);
        }
        if store.suspended_at(&key, at).is_some() {
            return self.log(Entry {
                rustc_ms: cost_ms,
                ..go_entry(Outcome::BuiltNotStored(NotStored::Suspended), &key)
            });
        }
        let floor = u64::from(current.min_free_gb).saturating_mul(1 << 30);
        if floor > 0 && store.free_bytes().is_ok_and(|free| free < floor) {
            return self.log(Entry {
                rustc_ms: cost_ms,
                ..go_entry(Outcome::BuiltNotStored(NotStored::LowSpace), &key)
            });
        }
        // A block there already: compared, as a second opinion (B-32). One only downloaded gives
        // way to this compile, which is from then on built here.
        if let Ok(Some(block)) = store.block(&key)
            && let [file] = block.files.as_slice()
        {
            let differed = file.sha256 != *sha;
            if differed {
                self.differs(current, &store, &key);
            }
            let downloaded = store.origin(&key).is_some_and(|o| o.downloaded_only());
            let took = downloaded
                && self.publish(current, &store, &key, action, &identity, path, cost_ms, at);
            if took && !differed {
                self.tell(&Request::GoBuilt { key, cost_ms });
            }
            return self.log(Entry {
                found: true,
                differed,
                rustc_ms: cost_ms,
                ..go_entry(Outcome::BuiltAndStored, &key)
            });
        }
        if self.publish(current, &store, &key, action, &identity, path, cost_ms, at) {
            let _ = store.set_step_cost(&key, cost_ms, at);
            self.tell(&Request::GoBuilt { key, cost_ms });
            self.log(Entry {
                rustc_ms: cost_ms,
                stored_bytes: body.len() as u64,
                ..go_entry(Outcome::BuiltAndStored, &key)
            });
        } else {
            self.log(Entry {
                rustc_ms: cost_ms,
                ..go_entry(Outcome::BuiltNotStored(NotStored::StoreRefused), &key)
            });
        }
    }

    /// Publishes the archive at `path` as the block of `key`, built here by this run (B-32),
    /// in place of one only downloaded. Whether it is stored.
    #[allow(clippy::too_many_arguments)]
    fn publish(
        &self,
        current: &Current,
        store: &Store,
        key: &Digest,
        action: &[u8; 32],
        identity: &Digest,
        path: &Path,
        cost_ms: u64,
        at: SystemTime,
    ) -> bool {
        let Some(share) = current.share.relay() else {
            return false;
        };
        let description = json!({
            "schema": BLOCK_SCHEMA,
            "kind": Kind::Go.word(),
            "step": key,
            "class": Class::Library.as_str(),
            "action": Digest::from_bytes(*action).hex(),
            "toolchain": identity.hex(),
            "repository": current.repository,
            "rustc_ms": cost_ms,
            "built_ms": epoch_ms(at),
        });
        let origin = Origin::built_here(&share.run, Some(share.by.clone()));
        let files = [NewFile {
            name: ARCHIVE.into(),
            source: path.to_path_buf(),
        }];
        store
            .replace(key, &description, &files, &origin, at, |there, _| {
                there.is_some_and(Origin::downloaded_only)
            })
            .is_ok_and(|replaced| replaced == crate::store::Replaced::Stored)
    }

    fn log_local(&self, local: Local, key: Option<Digest>, cost_ms: u64) {
        let mut entry = Entry {
            rustc_ms: cost_ms,
            ..go_entry(
                Outcome::BuiltNotStored(NotStored::StoreSkipped),
                &key.unwrap_or(Digest::from_bytes([0; 32])),
            )
        };
        if key.is_none() {
            entry.step = None;
            entry.key = None;
        }
        entry.go = Some(GoEntry {
            go_cache: false,
            local: Some(local),
        });
        self.log(entry);
    }

    fn log(&self, entry: Entry) {
        if let Some(current) = &self.current {
            current.log(&entry);
        }
    }

    // --- the relay -----------------------------------------------------------------------

    /// Asks the run's relay within `wait` (F10): no relay, or one call that failed or timed
    /// out in this command, and nothing more is asked.
    fn ask(&self, request: &Request, wait: Duration) -> Option<Answer> {
        if self.relay_down.load(Ordering::Acquire) {
            return None;
        }
        let share = self.current.as_ref()?.share.relay()?;
        let answer = relay::ask(&share.socket, request, wait);
        if answer.is_none() {
            self.relay_down.store(true, Ordering::Release);
        }
        answer
    }

    /// Hands the relay a notice, reading no answer.
    fn tell(&self, request: &Request) {
        if self.relay_down.load(Ordering::Acquire) {
            return;
        }
        let Some(share) = self.current.as_ref().and_then(|c| c.share.relay()) else {
            return;
        };
        if !relay::tell(&share.socket, request, QUICK_WAIT) {
            self.relay_down.store(true, Ordering::Release);
        }
    }
}

/// A file's bytes and the stat of the very file they were read from.
struct Checked {
    path: PathBuf,
    bytes: Vec<u8>,
    meta: std::fs::Metadata,
}

impl Checked {
    fn of(path: &Path) -> Option<Checked> {
        let mut file = std::fs::File::open(path).ok()?;
        let meta = file.metadata().ok()?;
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        file.read_to_end(&mut bytes).ok()?;
        Some(Checked {
            path: path.to_path_buf(),
            bytes,
            meta,
        })
    }
}

/// Whether two stats are of one file.
#[cfg(unix)]
fn same_inode(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_inode(_a: &std::fs::Metadata, _b: &std::fs::Metadata) -> bool {
    false
}

/// A line without its newline.
fn trim(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &line[..end]
}

/// A Go block's digest of outputs: its one file, `archive`.
pub fn outputs_of(size: u64, sha256: &Digest) -> hushrun_core::blocks::Hex32 {
    hushrun_core::blocks::outputs_digest(&[hushrun_core::blocks::OutputFile {
        name: ARCHIVE,
        size,
        sha256: hushrun_core::blocks::Hex32(*sha256.as_bytes()),
    }])
}

/// A Go archive's line in the run's log.
fn go_entry(outcome: Outcome, key: &Digest) -> Entry {
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
        step: Some(*key),
        key: Some(*key),
        downloaded: false,
        go: Some(GoEntry::default()),
    }
}

#[cfg(test)]
mod tests;
