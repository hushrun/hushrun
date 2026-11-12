//! One `hushrun run`: what the parent hands the wrapper, the log of outcomes, and the summary
//! line (B-20).
//!
//! `hushrun run` calls [`begin`] before it starts COMMAND, sets [`Session::env`] on the child,
//! and calls [`Session::finish`] after it exits. The wrapper finds the session through
//! [`SESSION`] and reads no setting of its own, so every compile of one run sees one mode.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::audit::Audit;
use crate::model::{Class, Mode, NotStored, Outcome, PassedThrough, Programs};
use crate::settings::{self, Overrides, Settings};
use crate::store::{DAY, Store};
use crate::unit::Checked;

/// The variable that names the session's folder for the wrapper.
pub const SESSION: &str = "HUSHRUN_BUILD_SESSION";

/// The variable that names a file each compile of the run appends its keys' parts to, one JSON
/// line per compile ([`Current::key_log`]). Read once, as `hushrun run` begins; a path that is
/// not absolute is ignored, since compiles run in other folders.
pub const KEY_LOG: &str = "HUSHRUN_BUILD_KEY_LOG";

/// The variable cargo reads the wrapper from.
pub const RUSTC_WRAPPER: &str = "RUSTC_WRAPPER";

/// What `hushrun run` knows when it starts COMMAND.
pub struct Start<'a> {
    /// The per-user settings file (B-21).
    pub settings: &'a Path,
    /// hushrun's cache directory. `None` when the person turned caches off.
    pub cache_dir: Option<&'a Path>,
    /// The hushrun binary that is running.
    pub exe: &'a Path,
    /// Whether this is CI (C-14's detection).
    pub in_ci: bool,
    /// The repository's name, when hushrun knows it. Blocks record it (B-13).
    pub repository: Option<&'a str>,
    /// The environment of `hushrun run`.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    /// The command's time.
    pub now: SystemTime,
}

/// Why a run has no session: COMMAND runs with nothing set (B-2, B-19).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotBegun {
    /// The mode is `off`.
    Off,
    /// CI: nothing is reused there yet.
    Ci,
    /// Windows: not yet.
    Windows,
    /// `RUSTC_WRAPPER` is set already, and stays as it is.
    WrapperSet,
    /// hushrun has no cache directory.
    NoCache,
    /// The settings, a variable or the store's folder can't be used. The text says which and
    /// why, for one line on stderr.
    Unusable(String),
}

/// One run's session.
#[derive(Debug)]
pub struct Session {
    current: Current,
    link: PathBuf,
    /// What the check before COMMAND removed, as the summary says it (B-23).
    rebuilt: Vec<String>,
}

/// What the wrapper reads at each compiler call: written once by [`begin`], so every compile of
/// one run sees the same mode and options, and none reads the settings itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Current {
    /// The mode of this run: `observe` or `on`.
    pub mode: Mode,
    /// The store's folder (B-13).
    pub store: PathBuf,
    /// How long blocks are kept (B-14).
    pub keep_days: u32,
    /// Packages never stored (B-4).
    pub never_store: Vec<String>,
    /// The person's extra variables for the first key (B-5).
    pub env: Vec<String>,
    /// Nothing is stored with less free space than this (B-22). 0 turns the rule off.
    pub min_free_gb: u32,
    /// What the wrapper does with a program on an Apple target (B-9, B-21).
    #[serde(default)]
    pub programs: Programs,
    /// The repository's name, when hushrun knows it (B-13).
    pub repository: Option<String>,
    /// When the run started, in milliseconds since the epoch. The audit's pick uses it (B-16).
    pub started_ms: u64,
    /// Folders that are the same for every checkout on this machine (B-8): the cargo home, the
    /// rustup home and the system's own.
    pub shared: Vec<String>,
    /// The cargo home, as `hushrun run` found it: a base (B-8), and searched for in outputs.
    #[serde(default)]
    pub cargo_home: Option<String>,
    /// The rustup home, as `hushrun run` found it: searched for in outputs.
    #[serde(default)]
    pub rustup_home: Option<String>,
    /// The home folder, `HOME`: searched for in outputs.
    #[serde(default)]
    pub home: Option<String>,
    /// Where each compile appends one line of its keys' parts, when `HUSHRUN_BUILD_KEY_LOG`
    /// named a file as `hushrun run` began: what tells which part of a key differs between two
    /// machines. Nothing is written otherwise.
    #[serde(default)]
    pub key_log: Option<PathBuf>,
    /// How many of the compiles that could be reused this run audits (B-16), from
    /// `HUSHRUN_BUILD_AUDIT` as `hushrun run` began.
    #[serde(default)]
    pub audit: Audit,
    /// The session's folder.
    pub folder: PathBuf,
}

/// One compile's line in the run's log (B-20).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// How the call ended (B-3).
    pub outcome: Outcome,
    /// What it produces, when the call was read that far.
    pub class: Option<Class>,
    /// The crate's name, or empty.
    pub crate_name: String,
    /// The package's `name-version`, or empty when unknown: every build script's crate is
    /// `build_script_build`, and this tells them apart.
    #[serde(default)]
    pub package: String,
    /// How long rustc ran, in milliseconds. 0 when it didn't.
    pub rustc_ms: u64,
    /// For a reuse: how long rustc took when the block was built, less what the reuse took.
    pub saved_ms: u64,
    /// Bytes this call added to the store.
    pub stored_bytes: u64,
    /// Whether a block existed for this compile before it ran.
    pub found: bool,
    /// Whether the fresh outputs differed from that block's (B-1 `observe`, B-16).
    pub differed: bool,
    /// One line for the person, when something needs saying (a suspension, low space).
    pub note: Option<String>,
    /// Whether the block may leave the machine, and why not: for a compile that got as far as
    /// searching its outputs, and for a reuse (from the block's record). `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable: Option<crate::portable::Portability>,
}

const SESSION_FILE: &str = "session.json";
const LOG_FILE: &str = "log.jsonl";
/// The store's folder of sessions: one per run, removed when the run ends.
const SESSIONS: &str = "sessions";
/// The store's folder of wrapper links: one per hushrun binary.
const BIN: &str = "bin";
/// A session untouched for longer belongs to a run that was killed.
const STALE_SESSION: Duration = Duration::from_secs(2 * DAY.as_secs());
/// The most note lines a summary writes.
const MAX_NOTES: usize = 5;

/// System folders that every checkout on a machine shares (B-8).
const SYSTEM: &[&str] = &[
    "/usr",
    "/opt",
    "/Library",
    "/System",
    "/Applications",
    "/nix",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
];

/// The session of the run this process belongs to, if any. `None` is a reason to pass through:
/// no session, or one that can't be read.
pub fn current(env: &dyn Fn(&str) -> Option<OsString>) -> Option<Current> {
    let folder = PathBuf::from(env(SESSION).filter(|value| !value.is_empty())?);
    let text = std::fs::read(folder.join(SESSION_FILE)).ok()?;
    serde_json::from_slice(&text).ok()
}

impl Current {
    /// Appends one line to the run's log. A log that can't be written never fails a compile.
    pub fn log(&self, entry: &Entry) {
        use std::io::Write;
        let Ok(mut line) = serde_json::to_vec(entry) else {
            return;
        };
        line.push(b'\n');
        // One write of one whole line, to a file opened for appending: lines from compiles
        // running at once don't mix.
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.folder.join(LOG_FILE))
        {
            let _ = file.write_all(&line);
        }
    }

    /// The lines logged so far, in the order they were written. A line that can't be read is
    /// left out.
    pub fn entries(&self) -> Vec<Entry> {
        let Ok(text) = std::fs::read_to_string(self.folder.join(LOG_FILE)) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

/// A folder a variable names, else `default` under the home folder, as cargo and rustup find
/// theirs.
fn under_home(
    env: &dyn Fn(&str) -> Option<OsString>,
    variable: &str,
    default: &str,
) -> Option<String> {
    let home = env("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    env(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(default)))
        .and_then(|path| path.to_str().map(str::to_string))
}

fn shared_folders(env: &dyn Fn(&str) -> Option<OsString>) -> Vec<String> {
    let mut shared: Vec<String> = SYSTEM.iter().map(|s| s.to_string()).collect();
    shared.extend(under_home(env, "CARGO_HOME", ".cargo"));
    shared.extend(under_home(env, "RUSTUP_HOME", ".rustup"));
    shared
}

/// What a run's compiles came to, counted from its log (B-20). Calls that compile none of the
/// build's own crates count nowhere: cargo's probes (`--print`, `-vV`, no `--crate-name`), and
/// the probes a build script runs through the wrapper (autocfg's, with `-o`, or without
/// `-C extra-filename`, `--out-dir` or `--json=artifacts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// The run's mode.
    pub mode: Mode,
    /// Compiles: every call but the probes.
    pub compiles: u64,
    /// Compiles whose block was put in place, with rustc not started.
    pub reused: u64,
    /// Compiles that ran and were stored.
    pub built_and_stored: u64,
    /// Compiles that ran and were not stored, for any reason.
    pub built_not_stored: u64,
    /// Compiles that ran exactly as asked: an incremental rebuild, a `cdylib`, clippy's driver.
    pub passed_through: u64,
    /// Compiles that found a block. In `observe`, the ones that would have been reused.
    pub found: u64,
    /// Compiles whose fresh outputs differed from the block found (B-1 `observe`, B-16).
    pub differed: u64,
    /// Milliseconds reuse saved.
    pub saved_ms: u64,
    /// Bytes the run added to the store.
    pub stored_bytes: u64,
    /// Compiles not stored because the store's volume had too little free space (B-22).
    pub low_space: u64,
    /// The free space, in GB, below which nothing is stored (B-22).
    pub min_free_gb: u32,
    /// The distinct notes the compiles left, in the order first seen.
    pub notes: Vec<String>,
}

impl Counts {
    /// Counts a run's log.
    pub fn of(mode: Mode, min_free_gb: u32, entries: &[Entry]) -> Counts {
        let mut counts = Counts {
            mode,
            min_free_gb,
            ..Counts::default()
        };
        for entry in entries.iter().filter(|entry| !is_probe(entry.outcome)) {
            counts.compiles += 1;
            match entry.outcome {
                Outcome::Reused => counts.reused += 1,
                Outcome::BuiltAndStored => counts.built_and_stored += 1,
                Outcome::BuiltNotStored(why) => {
                    counts.built_not_stored += 1;
                    if why == NotStored::LowSpace {
                        counts.low_space += 1;
                    }
                }
                Outcome::PassedThrough(_) => counts.passed_through += 1,
            }
            counts.found += u64::from(entry.found);
            counts.differed += u64::from(entry.differed);
            counts.saved_ms = counts.saved_ms.saturating_add(entry.saved_ms);
            counts.stored_bytes = counts.stored_bytes.saturating_add(entry.stored_bytes);
            if let Some(note) = entry.note.as_deref().map(str::trim)
                && !note.is_empty()
                && !counts.notes.iter().any(|seen| seen == note)
            {
                counts.notes.push(note.to_string());
            }
        }
        counts
    }

    /// Compiles where rustc ran: stored, not stored and passed through.
    pub fn built(&self) -> u64 {
        self.compiles - self.reused
    }
}

/// Probes: calls that compile none of the build's own crates. Cargo always passes a crate of
/// the build `--crate-name`, `-C extra-filename`, `--out-dir` and `--json=artifacts`, and never
/// `-o`; a call without them is cargo's probe or a build script's own.
fn is_probe(outcome: Outcome) -> bool {
    matches!(
        outcome,
        Outcome::PassedThrough(
            PassedThrough::Probe
                | PassedThrough::NoCrateName
                | PassedThrough::NoExtraFilename
                | PassedThrough::NoOutDir
                | PassedThrough::OutputFile
                | PassedThrough::NoArtifacts
        )
    )
}

/// What a run's compiles came to (B-20).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    counts: Counts,
    lines: Vec<String>,
}

impl Summary {
    /// The summary of a run's log.
    pub fn of(mode: Mode, min_free_gb: u32, entries: &[Entry]) -> Summary {
        Summary::of_run(mode, min_free_gb, entries, &[])
    }

    /// The summary of a run's log, with what the check before COMMAND removed (B-23).
    pub fn of_run(mode: Mode, min_free_gb: u32, entries: &[Entry], rebuilt: &[String]) -> Summary {
        let counts = Counts::of(mode, min_free_gb, entries);
        let lines = lines(&counts, rebuilt);
        Summary { counts, lines }
    }

    /// The lines `hushrun run` writes after COMMAND, without a prefix. Empty when nothing was
    /// compiled.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// The numbers the lines come from.
    pub fn counts(&self) -> &Counts {
        &self.counts
    }
}

/// B-20's lines: the totals, then what the check before COMMAND removed (B-23), then at most
/// [`MAX_NOTES`] notes, then low space once (B-22).
fn lines(counts: &Counts, rebuilt: &[String]) -> Vec<String> {
    let rebuilt = rebuilt.iter().map(|line| format!("build: {line}"));
    if counts.compiles == 0 {
        return rebuilt.collect();
    }
    let all = crates(counts.compiles);
    let mut first = match counts.mode {
        Mode::Observe => {
            let mut line = format!(
                "build: observing: {} of {all} would have been reused",
                counts.found
            );
            if counts.differed > 0 {
                line.push_str(&format!(
                    ", {} differed from what a reuse would have given",
                    counts.differed
                ));
            }
            line
        }
        Mode::On | Mode::Off if counts.reused == 0 => {
            format!("build: {} built, none reused", crates(counts.built()))
        }
        Mode::On | Mode::Off => {
            let mut line = format!("build: {} of {all} reused", counts.reused);
            if counts.built() > 0 {
                line.push_str(&format!(", {} built", counts.built()));
            }
            line
        }
    };
    if counts.mode != Mode::Observe && counts.saved_ms >= 1_000 {
        first.push_str(&format!(", saving about {}", took(counts.saved_ms)));
    }
    let mut lines = vec![first];
    lines.extend(rebuilt);
    lines.extend(
        counts
            .notes
            .iter()
            .take(MAX_NOTES)
            .map(|note| format!("build: {note}")),
    );
    if counts.low_space > 0 {
        let what = if counts.built_and_stored == 0 {
            "nothing was stored".to_string()
        } else if counts.low_space == 1 {
            "1 crate was not stored".to_string()
        } else {
            format!("{} crates were not stored", counts.low_space)
        };
        lines.push(format!(
            "build: {what} because the disk has less than {} GB free",
            counts.min_free_gb
        ));
    }
    lines
}

/// `1 crate`, `41 crates`.
fn crates(n: u64) -> String {
    if n == 1 {
        "1 crate".to_string()
    } else {
        format!("{n} crates")
    }
}

/// A length of time, rounded for people: `0.4s`, `41s`, `3m 12s`, `1h 4m`. The same form as the
/// time a skipped check saved (`took` in the hushrun crate's `cmd/wrap.rs`).
fn took(ms: u64) -> String {
    let s = ms / 1000;
    match ms {
        0..1_000 => format!("{:.1}s", ms as f64 / 1000.0),
        1_000..60_000 => format!("{s}s"),
        60_000..3_600_000 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3_600, s / 60 % 60),
    }
}

/// The store's folder (B-13): `HUSHRUN_BUILD_DIR`, or `build` under hushrun's cache directory.
pub fn store_dir(cache_dir: Option<&Path>, overrides: &Overrides) -> Option<PathBuf> {
    match &overrides.dir {
        Some(dir) => Some(dir.clone()),
        None => cache_dir.map(|dir| dir.join("build")),
    }
}

/// Starts a session, or says why there is none.
pub fn begin(start: &Start<'_>) -> Result<Session, NotBegun> {
    if cfg!(windows) {
        return Err(NotBegun::Windows);
    }
    let overrides =
        Overrides::from(start.env).map_err(|err| NotBegun::Unusable(err.to_string()))?;
    let settings: Settings = settings::load(start.settings)
        .map_err(|err| NotBegun::Unusable(err.to_string()))?
        .with(&overrides);
    if settings.mode == Mode::Off {
        return Err(NotBegun::Off);
    }
    if start.in_ci {
        return Err(NotBegun::Ci);
    }
    if (start.env)(RUSTC_WRAPPER).is_some_and(|value| !value.is_empty()) {
        return Err(NotBegun::WrapperSet);
    }
    let Some(store) = store_dir(start.cache_dir, &overrides) else {
        return Err(NotBegun::NoCache);
    };
    let millis = start
        .now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    let folder = store
        .join(SESSIONS)
        .join(format!("{millis}-{}", std::process::id()));
    std::fs::create_dir_all(&folder)
        .map_err(|err| NotBegun::Unusable(format!("{}: {err}", folder.display())))?;
    let link = link_to(&store, start.exe)
        .map_err(|err| NotBegun::Unusable(format!("{}: {err}", store.display())))?;
    let current = Current {
        mode: settings.mode,
        store,
        keep_days: settings.keep_days,
        never_store: settings.never_store,
        env: settings.env,
        min_free_gb: settings.min_free_gb,
        programs: settings.programs,
        repository: start.repository.map(str::to_string),
        started_ms: millis,
        shared: shared_folders(start.env),
        cargo_home: under_home(start.env, "CARGO_HOME", ".cargo"),
        rustup_home: under_home(start.env, "RUSTUP_HOME", ".rustup"),
        home: (start.env)("HOME")
            .filter(|value| !value.is_empty())
            .and_then(|value| value.into_string().ok()),
        key_log: (start.env)(KEY_LOG)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute()),
        audit: Audit::from_var(
            (start.env)(crate::audit::VAR)
                .as_deref()
                .and_then(OsStr::to_str),
        ),
        folder,
    };
    let file = current.folder.join(SESSION_FILE);
    serde_json::to_vec(&current)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&file, bytes))
        .map_err(|err| NotBegun::Unusable(format!("{}: {err}", file.display())))?;
    Ok(Session {
        current,
        link,
        rebuilt: Vec::new(),
    })
}

impl Session {
    /// What to set on COMMAND: the wrapper for cargo, and the session for the wrapper.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        vec![
            (RUSTC_WRAPPER.into(), self.link.clone().into_os_string()),
            (SESSION.into(), self.current.folder.clone().into_os_string()),
        ]
    }

    /// The mode every compile of this run sees.
    pub fn mode(&self) -> Mode {
        self.current.mode
    }

    /// What the wrapper reads for this run.
    pub fn current(&self) -> &Current {
        &self.current
    }

    /// Before COMMAND starts (B-23): removes the outputs of every crate the wrapper saw compiled
    /// in the target folders of `folder` (the folder `hushrun run` runs in) whose inputs changed
    /// in a way cargo can't see, so that cargo builds them. What it removed goes into the run's
    /// summary. It stops at `deadline`, leaving the rest unchecked.
    pub fn check(&mut self, folder: &Path, deadline: Option<std::time::Instant>) -> Checked {
        let checked = crate::unit::check(&Store::new(&self.current.store), folder, deadline);
        self.rebuilt = checked.lines();
        checked
    }

    /// After COMMAND: reads the log, removes the session's folder and, at most once a day,
    /// sweeps the store (B-14). Then it removes what other runs left: sessions untouched for
    /// more than two days (a run that was killed) and wrapper links whose binary is gone. Nothing
    /// here fails the run: every step tolerates a missing file and another process doing the
    /// same at once.
    pub fn finish(self, now: SystemTime) -> Summary {
        let current = &self.current;
        let summary = Summary::of_run(
            current.mode,
            current.min_free_gb,
            &current.entries(),
            &self.rebuilt,
        );
        let _ = remove_dir(&current.folder);
        let _ = crate::sweep::sweep(&Store::new(&current.store), now, current.keep_days);
        remove_stale_sessions(&current.store, now);
        remove_dangling_links(&current.store);
        summary
    }
}

/// Removes a folder and what it holds. Already gone is fine.
fn remove_dir(path: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Removes the sessions of runs that were killed: folders under `sessions/` where nothing changed
/// for more than two days. A session's log is written to as long as its run compiles, so a long
/// run is not taken for a killed one.
fn remove_stale_sessions(store: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(store.join(SESSIONS)) else {
        return;
    };
    for entry in entries.flatten() {
        let folder = entry.path();
        let Ok(meta) = fs::symlink_metadata(&folder) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let changed = [SESSION_FILE, LOG_FILE]
            .iter()
            .filter_map(|name| {
                fs::metadata(folder.join(name))
                    .and_then(|m| m.modified())
                    .ok()
            })
            .chain(meta.modified().ok())
            .max();
        // A date after `now` is a clock set back: not stale.
        if changed.is_some_and(|changed| {
            now.duration_since(changed)
                .is_ok_and(|age| age > STALE_SESSION)
        }) {
            let _ = remove_dir(&folder);
        }
    }
}

/// Removes the wrapper links whose binary no longer exists (an uninstalled or rebuilt hushrun),
/// each with its folder under `bin/`. A folder with anything else in it stays.
fn remove_dangling_links(store: &Path) {
    let Ok(entries) = fs::read_dir(store.join(BIN)) else {
        return;
    };
    for entry in entries.flatten() {
        let folder = entry.path();
        let link = folder.join(crate::WRAPPER_NAME);
        let is_link = fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink());
        let dangling = matches!(
            fs::metadata(&link),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound
        );
        if is_link && dangling {
            let _ = fs::remove_file(&link);
            let _ = fs::remove_dir(&folder);
        }
    }
}

/// The link cargo runs (B-2): `bin/<a name for this binary>/hushrun-rustc`, pointing at the
/// binary. Each binary has its own, so two versions of hushrun running at once don't take each
/// other's.
#[cfg(unix)]
fn link_to(store: &Path, exe: &Path) -> std::io::Result<PathBuf> {
    let meta = std::fs::metadata(exe)?;
    let stat = hushrun_digest::cache::Stat::of(&meta);
    let name = hushrun_core::Digest::of(
        format!("{}\n{}\n{}", exe.display(), stat.size, stat.mtime_ns).as_bytes(),
    )
    .hex();
    let dir = store.join(BIN).join(&name[..16]);
    let link = dir.join(crate::WRAPPER_NAME);
    if std::fs::read_link(&link).is_ok_and(|target| target == exe) {
        return Ok(link);
    }
    std::fs::create_dir_all(&dir)?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        crate::WRAPPER_NAME,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&temp);
    std::os::unix::fs::symlink(exe, &temp)?;
    std::fs::rename(&temp, &link)?;
    Ok(link)
}

#[cfg(not(unix))]
fn link_to(_store: &Path, _exe: &Path) -> std::io::Result<PathBuf> {
    Err(std::io::Error::other("no wrapper link on this platform"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn start<'a>(
        dir: &'a Path,
        settings: &'a Path,
        exe: &'a Path,
        env: &'a dyn Fn(&str) -> Option<OsString>,
    ) -> Start<'a> {
        Start {
            settings,
            cache_dir: Some(dir),
            exe,
            in_ci: false,
            repository: None,
            env,
            now: UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000),
        }
    }

    // covers: B-1, B-2, B-21
    #[test]
    fn a_run_has_a_session_only_when_reuse_is_on_here() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("build.json");
        let exe = dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        let none = |_: &str| None;

        // Off until the person turns it on.
        assert_eq!(
            begin(&start(dir.path(), &settings, &exe, &none)).unwrap_err(),
            NotBegun::Off
        );

        settings::update(&settings, |s| s.mode = Mode::Observe).unwrap();
        let session = begin(&start(dir.path(), &settings, &exe, &none)).unwrap();
        assert_eq!(session.mode(), Mode::Observe);
        let env = session.env();
        let wrapper = PathBuf::from(&env[0].1);
        assert_eq!(env[0].0, RUSTC_WRAPPER);
        assert!(crate::wrapper::called_as_wrapper(wrapper.as_os_str()));
        assert_eq!(std::fs::read_link(&wrapper).unwrap(), exe);
        assert_eq!(env[1].0, SESSION);
        assert!(Path::new(&env[1].1).is_dir());
        // The wrapper finds the same session through the variable, and logs to it.
        let folder = env[1].1.clone();
        let found = current(&|name| (name == SESSION).then(|| folder.clone())).unwrap();
        assert_eq!(&found, session.current());
        assert_eq!(found.mode, Mode::Observe);
        assert_eq!(found.programs, Programs::Movable);
        assert!(found.shared.iter().any(|s| s == "/usr"));
        let entry = Entry {
            outcome: crate::decide::passed_through(crate::model::PassedThrough::Probe),
            class: None,
            crate_name: String::new(),
            package: String::new(),
            rustc_ms: 0,
            saved_ms: 0,
            stored_bytes: 0,
            found: false,
            differed: false,
            note: None,
            portable: None,
        };
        found.log(&entry);
        found.log(&entry);
        assert_eq!(found.entries(), vec![entry.clone(), entry]);
        session.finish(UNIX_EPOCH);

        // What happens to programs is the person's, and every compile of the run sees it.
        settings::update(&settings, |s| s.programs = Programs::Tied).unwrap();
        let session = begin(&start(dir.path(), &settings, &exe, &none)).unwrap();
        let folder = session.env()[1].1.clone();
        let found = current(&|name| (name == SESSION).then(|| folder.clone())).unwrap();
        assert_eq!(found.programs, Programs::Tied);
        assert_eq!(found.audit, Audit::OneIn);
        session.finish(UNIX_EPOCH);

        // How many compiles the run audits is read once, as it begins (B-16).
        let every = |name: &str| (name == crate::audit::VAR).then(|| OsString::from("every"));
        let session = begin(&start(dir.path(), &settings, &exe, &every)).unwrap();
        let folder = session.env()[1].1.clone();
        let found = current(&|name| (name == SESSION).then(|| folder.clone())).unwrap();
        assert_eq!(found.audit, Audit::Every);
        session.finish(UNIX_EPOCH);

        // Never in CI, and never over a wrapper the person set.
        let mut ci = start(dir.path(), &settings, &exe, &none);
        ci.in_ci = true;
        assert_eq!(begin(&ci).unwrap_err(), NotBegun::Ci);
        let set = |name: &str| (name == RUSTC_WRAPPER).then(|| OsString::from("sccache"));
        assert_eq!(
            begin(&start(dir.path(), &settings, &exe, &set)).unwrap_err(),
            NotBegun::WrapperSet
        );
        // One command can turn it off.
        let off = |name: &str| (name == "HUSHRUN_BUILD").then(|| OsString::from("off"));
        assert_eq!(
            begin(&start(dir.path(), &settings, &exe, &off)).unwrap_err(),
            NotBegun::Off
        );
    }

    // covers: B-2
    #[test]
    fn each_binary_has_its_own_link() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one");
        let two = dir.path().join("two");
        std::fs::write(&one, b"1").unwrap();
        std::fs::write(&two, b"22").unwrap();
        let a = link_to(dir.path(), &one).unwrap();
        let b = link_to(dir.path(), &two).unwrap();
        assert_ne!(a, b);
        assert_eq!(link_to(dir.path(), &one).unwrap(), a);
        assert_eq!(std::fs::read_link(&a).unwrap(), one);
        assert_eq!(std::fs::read_link(&b).unwrap(), two);
    }

    // The summary's lines.

    fn entry(outcome: Outcome) -> Entry {
        Entry {
            outcome,
            class: Some(Class::Library),
            crate_name: "c".into(),
            package: String::new(),
            rustc_ms: 0,
            saved_ms: 0,
            stored_bytes: 0,
            found: false,
            differed: false,
            note: None,
            portable: None,
        }
    }

    fn reused(saved_ms: u64) -> Entry {
        Entry {
            found: true,
            saved_ms,
            ..entry(Outcome::Reused)
        }
    }

    fn built() -> Entry {
        Entry {
            stored_bytes: 100,
            ..entry(Outcome::BuiltAndStored)
        }
    }

    fn probe(why: PassedThrough) -> Entry {
        Entry {
            class: None,
            crate_name: String::new(),
            ..entry(Outcome::PassedThrough(why))
        }
    }

    fn noted(note: &str) -> Entry {
        Entry {
            note: Some(note.into()),
            ..entry(Outcome::BuiltNotStored(NotStored::Suspended))
        }
    }

    fn lines_of(mode: Mode, entries: &[Entry]) -> Vec<String> {
        Summary::of(mode, 10, entries).lines().to_vec()
    }

    fn many(n: usize, make: impl Fn() -> Entry) -> Vec<Entry> {
        (0..n).map(|_| make()).collect()
    }

    // covers: B-20
    #[test]
    fn nothing_compiled_says_nothing() {
        assert!(lines_of(Mode::On, &[]).is_empty());
        let probes = [
            probe(PassedThrough::Probe),
            probe(PassedThrough::NoCrateName),
        ];
        let summary = Summary::of(Mode::On, 10, &probes);
        assert!(summary.lines().is_empty());
        assert_eq!(summary.counts().compiles, 0);
        assert!(lines_of(Mode::Observe, &probes).is_empty());
    }

    // covers: B-20
    #[test]
    fn all_built_says_none_reused() {
        assert_eq!(
            lines_of(Mode::On, &many(41, built)),
            ["build: 41 crates built, none reused"]
        );
    }

    // covers: B-20
    #[test]
    fn some_reused_says_how_many_and_the_time_saved() {
        let mut entries = many(331, || reused(0));
        entries[0].saved_ms = 100_000;
        entries[1].saved_ms = 60_000;
        entries.extend(many(41, built));
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(
            summary.lines(),
            ["build: 331 of 372 crates reused, 41 built, saving about 2m 40s"]
        );
        let counts = summary.counts();
        assert_eq!(
            (counts.compiles, counts.reused, counts.built_and_stored),
            (372, 331, 41)
        );
        assert_eq!(counts.found, 331);
        assert_eq!(counts.saved_ms, 160_000);
        assert_eq!(counts.stored_bytes, 4_100);
        assert_eq!(counts.built(), 41);

        // Under a second saved: no time.
        let small = [reused(300), reused(100), built()];
        assert_eq!(
            lines_of(Mode::On, &small),
            ["build: 2 of 3 crates reused, 1 built"]
        );
        // Exactly a second is said.
        let second = [reused(1_000), built()];
        assert_eq!(
            lines_of(Mode::On, &second),
            ["build: 1 of 2 crates reused, 1 built, saving about 1s"]
        );
        // All reused: nothing built to count.
        assert_eq!(
            lines_of(Mode::On, &many(3, || reused(41_000))),
            ["build: 3 of 3 crates reused, saving about 2m 3s"]
        );
    }

    // covers: B-1, B-20
    #[test]
    fn observe_says_what_would_have_been_reused_and_what_differed() {
        let mut entries = many(2, || Entry {
            found: true,
            ..built()
        });
        entries.push(built());
        assert_eq!(
            lines_of(Mode::Observe, &entries),
            ["build: observing: 2 of 3 crates would have been reused"]
        );
        entries[1].differed = true;
        entries[1].outcome = Outcome::BuiltNotStored(NotStored::Differed);
        let summary = Summary::of(Mode::Observe, 10, &entries);
        assert_eq!(
            summary.lines(),
            [
                "build: observing: 2 of 3 crates would have been reused, 1 differed from what a \
                 reuse would have given"
            ]
        );
        assert_eq!(summary.counts().differed, 1);
        assert_eq!(summary.counts().built_not_stored, 1);
        // Nothing would have been reused.
        assert_eq!(
            lines_of(Mode::Observe, &many(4, built)),
            ["build: observing: 0 of 4 crates would have been reused"]
        );
    }

    // covers: B-20
    #[test]
    fn one_crate_is_singular() {
        assert_eq!(
            lines_of(Mode::On, &[built()]),
            ["build: 1 crate built, none reused"]
        );
        assert_eq!(
            lines_of(Mode::On, &[reused(5_000)]),
            ["build: 1 of 1 crate reused, saving about 5s"]
        );
        assert_eq!(
            lines_of(Mode::Observe, &[built()]),
            ["build: observing: 0 of 1 crate would have been reused"]
        );
    }

    // covers: B-20
    #[test]
    fn cargo_probes_are_not_compiles() {
        let entries = [
            probe(PassedThrough::Probe),
            reused(0),
            probe(PassedThrough::NoCrateName),
            built(),
            probe(PassedThrough::Probe),
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(summary.lines(), ["build: 1 of 2 crates reused, 1 built"]);
        assert_eq!(summary.counts().compiles, 2);
        assert_eq!(summary.counts().passed_through, 0);
    }

    // covers: B-20
    #[test]
    fn a_build_scripts_own_rustc_probes_are_not_crates() {
        // autocfg and friends call rustc through the wrapper with `-o`, or without
        // `-C extra-filename`, `--out-dir` or `--json=artifacts`: not the build's own crates.
        let entries = [
            reused(0),
            built(),
            entry(Outcome::PassedThrough(PassedThrough::NoExtraFilename)),
            entry(Outcome::PassedThrough(PassedThrough::NoOutDir)),
            entry(Outcome::PassedThrough(PassedThrough::OutputFile)),
            entry(Outcome::PassedThrough(PassedThrough::NoArtifacts)),
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(summary.lines(), ["build: 1 of 2 crates reused, 1 built"]);
        assert_eq!(summary.counts().compiles, 2);
        assert_eq!(summary.counts().passed_through, 0);
    }

    // covers: B-9, B-20
    #[test]
    fn a_compile_passed_through_counts_as_built() {
        // A crate type that is never stored, and clippy's driver: rustc ran, so the crate was
        // built. An incremental rebuild is passed through the same way.
        let entries = [
            reused(0),
            entry(Outcome::PassedThrough(PassedThrough::CrateType)),
            entry(Outcome::PassedThrough(PassedThrough::NotRustc)),
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(summary.lines(), ["build: 1 of 3 crates reused, 2 built"]);
        assert_eq!(summary.counts().passed_through, 2);
    }

    // covers: B-15, B-20
    #[test]
    fn notes_are_said_once_each_and_at_most_five() {
        let entries = [
            noted("a"),
            noted("b"),
            noted("a"),
            built(),
            noted("c"),
            noted("d"),
            noted("e"),
            noted("f"),
            noted("b"),
            noted("  "),
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(
            summary.lines(),
            [
                "build: 10 crates built, none reused",
                "build: a",
                "build: b",
                "build: c",
                "build: d",
                "build: e",
            ]
        );
        assert_eq!(summary.counts().notes, ["a", "b", "c", "d", "e", "f"]);
    }

    // covers: B-20, B-23
    #[test]
    fn what_the_check_removed_comes_after_the_totals() {
        let rebuilt = [
            "app is rebuilt: a file was added to migrations, which cargo doesn't watch".to_string(),
        ];
        assert_eq!(
            Summary::of_run(Mode::On, 10, &[built(), noted("a")], &rebuilt).lines(),
            [
                "build: 2 crates built, none reused",
                "build: app is rebuilt: a file was added to migrations, which cargo doesn't watch",
                "build: a",
            ]
        );
        // Said even when COMMAND compiled nothing.
        assert_eq!(
            Summary::of_run(Mode::On, 10, &[], &rebuilt).lines().len(),
            1
        );
        assert!(Summary::of_run(Mode::On, 10, &[], &[]).lines().is_empty());
    }

    // covers: B-22
    #[test]
    fn low_space_is_said_once() {
        let low = || entry(Outcome::BuiltNotStored(NotStored::LowSpace));
        let entries = [reused(0), low(), low()];
        assert_eq!(
            lines_of(Mode::On, &entries),
            [
                "build: 1 of 3 crates reused, 2 built",
                "build: nothing was stored because the disk has less than 10 GB free",
            ]
        );
        // Some were stored before the disk filled.
        let entries = [built(), low(), low()];
        assert_eq!(
            Summary::of(Mode::On, 20, &entries).lines(),
            [
                "build: 3 crates built, none reused",
                "build: 2 crates were not stored because the disk has less than 20 GB free",
            ]
        );
        let entries = [built(), low()];
        assert_eq!(
            lines_of(Mode::On, &entries)[1],
            "build: 1 crate was not stored because the disk has less than 10 GB free"
        );
    }

    #[test]
    fn durations_read_like_the_time_a_skip_saved() {
        assert_eq!(took(400), "0.4s");
        assert_eq!(took(41_000), "41s");
        assert_eq!(took(192_000), "3m 12s");
        assert_eq!(took(3_840_000), "1h 4m");
    }

    // finish.

    /// 2026-11-11T00:00:00Z.
    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_794_355_200)
    }

    fn days(n: u64) -> Duration {
        Duration::from_secs(n * 86_400)
    }

    struct Run {
        dir: tempfile::TempDir,
        settings: PathBuf,
        exe: PathBuf,
    }

    impl Run {
        fn new(mode: Mode) -> Run {
            let dir = tempfile::tempdir().unwrap();
            let settings = dir.path().join("build.json");
            let exe = dir.path().join("hushrun");
            std::fs::write(&exe, b"binary").unwrap();
            settings::update(&settings, |s| s.mode = mode).unwrap();
            Run { dir, settings, exe }
        }

        fn begin(&self) -> Session {
            let none = |_: &str| None;
            let mut start = start(self.dir.path(), &self.settings, &self.exe, &none);
            start.now = now();
            begin(&start).unwrap()
        }

        fn store(&self) -> Store {
            Store::new(&self.dir.path().join("build"))
        }
    }

    fn set_modified(path: &Path, when: SystemTime) {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    // covers: B-20
    #[test]
    fn finish_reads_the_log_and_removes_the_session() {
        let run = Run::new(Mode::On);
        let session = run.begin();
        let folder = session.current().folder.clone();
        session.current().log(&reused(2_000));
        session.current().log(&built());
        // A line cut short, and one that is not JSON: left out, the rest still counts.
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(folder.join(LOG_FILE))
            .unwrap();
        std::io::Write::write_all(&mut log, b"{\"outcome\":\nnot json\n").unwrap();
        drop(log);
        session.current().log(&built());
        let summary = session.finish(now());
        assert_eq!(
            summary.lines(),
            ["build: 1 of 3 crates reused, 2 built, saving about 2s"]
        );
        assert_eq!(summary.counts().mode, Mode::On);
        assert!(!folder.exists());
        // The other runs' folder stays.
        assert!(folder.parent().unwrap().is_dir());
    }

    // covers: B-20
    #[test]
    fn a_run_with_no_log_says_nothing() {
        let run = Run::new(Mode::Observe);
        let session = run.begin();
        let folder = session.current().folder.clone();
        let summary = session.finish(now());
        assert!(summary.lines().is_empty());
        assert_eq!(summary.counts().mode, Mode::Observe);
        assert!(!folder.exists());
    }

    // covers: B-14
    #[test]
    fn finish_sweeps_the_store_once_a_day() {
        let run = Run::new(Mode::On);
        let store = run.store();
        let key = |n: u32| hushrun_core::Digest::of(&n.to_le_bytes());
        let publish = |n: u32, used: SystemTime| {
            let source = run.dir.path().join(format!("source-{n}"));
            std::fs::write(&source, n.to_le_bytes()).unwrap();
            let file = crate::store::NewFile {
                name: "lib.rlib".into(),
                source,
            };
            let description = serde_json::json!({ "repository": "r" });
            store.publish(&key(n), &description, &[file], used).unwrap();
        };
        let has = |n: u32| store.block(&key(n)).unwrap().is_some();
        publish(1, now() - days(20));
        store.used(&key(1), now() - days(16)).unwrap();
        publish(2, now() - days(20));
        store.used(&key(2), now() - days(14)).unwrap();
        run.begin().finish(now());
        assert!(!has(1), "last used 16 days before");
        assert!(has(2), "last used 14 days before");

        // Within the day, a second run sweeps nothing.
        publish(3, now() - days(16));
        run.begin().finish(now() + Duration::from_secs(3_600));
        assert!(has(3));
        // The next day's run does.
        run.begin().finish(now() + days(1));
        assert!(!has(3));
    }

    // covers: B-19
    #[test]
    fn finish_removes_the_sessions_of_killed_runs() {
        let run = Run::new(Mode::On);
        let sessions = run.dir.path().join("build").join(SESSIONS);
        let killed = |name: &str, age: Duration| {
            let folder = sessions.join(name);
            std::fs::create_dir_all(&folder).unwrap();
            let file = folder.join(SESSION_FILE);
            std::fs::write(&file, b"{}").unwrap();
            set_modified(&file, now() - age);
            set_modified(&folder, now() - age);
            folder
        };
        let old = killed("1-1", days(3));
        let recent = killed("2-2", days(1));
        // A log written to a day ago: a run still compiling, though it began long before.
        let long = killed("3-3", days(5));
        let log = long.join(LOG_FILE);
        std::fs::write(&log, b"").unwrap();
        set_modified(&log, now() - days(1));
        set_modified(&long, now() - days(5));
        run.begin().finish(now());
        assert!(!old.exists());
        assert!(recent.is_dir());
        assert!(long.is_dir());
    }

    // covers: B-2
    #[test]
    fn finish_removes_links_to_a_binary_that_is_gone() {
        let run = Run::new(Mode::On);
        let store = run.dir.path().join("build");
        let gone = run.dir.path().join("uninstalled");
        std::fs::write(&gone, b"old").unwrap();
        let dangling = link_to(&store, &gone).unwrap();
        std::fs::remove_file(&gone).unwrap();
        let session = run.begin();
        let live = PathBuf::from(&session.env()[0].1);
        session.finish(now());
        assert!(std::fs::symlink_metadata(&dangling).is_err());
        assert!(!dangling.parent().unwrap().exists());
        assert_eq!(std::fs::read_link(&live).unwrap(), run.exe);
        // A folder with no link in it is not this rule's.
        let other = store.join(BIN).join("other");
        std::fs::create_dir_all(&other).unwrap();
        run.begin().finish(now());
        assert!(other.is_dir());
        assert!(live.exists());
    }
}
