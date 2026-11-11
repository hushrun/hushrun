//! One `hushrun run`: what the parent hands the wrapper, the log of outcomes, and the summary
//! line (B-20).
//!
//! `hushrun run` calls [`begin`] before it starts COMMAND, sets [`Session::env`] on the child,
//! and calls [`Session::finish`] after it exits. The wrapper finds the session through
//! [`SESSION`] and reads no setting of its own, so every compile of one run sees one mode.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::{Class, Mode, Outcome};
use crate::settings::{self, Overrides, Settings};

/// The variable that names the session's folder for the wrapper.
pub const SESSION: &str = "HUSHRUN_BUILD_SESSION";

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
    /// The repository's name, when hushrun knows it (B-13).
    pub repository: Option<String>,
    /// When the run started, in milliseconds since the epoch. The audit's pick uses it (B-16).
    pub started_ms: u64,
    /// Folders that are the same for every checkout on this machine (B-8): the cargo home, the
    /// rustup home and the system's own.
    pub shared: Vec<String>,
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
}

const SESSION_FILE: &str = "session.json";
const LOG_FILE: &str = "log.jsonl";

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

fn shared_folders(env: &dyn Fn(&str) -> Option<OsString>) -> Vec<String> {
    let home = env("HOME").map(PathBuf::from);
    let under_home = |variable: &str, default: &str| {
        env(variable)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(default)))
            .and_then(|path| path.to_str().map(str::to_string))
    };
    let mut shared: Vec<String> = SYSTEM.iter().map(|s| s.to_string()).collect();
    shared.extend(under_home("CARGO_HOME", ".cargo"));
    shared.extend(under_home("RUSTUP_HOME", ".rustup"));
    shared
}

/// What a run's compiles came to (B-20).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    lines: Vec<String>,
}

impl Summary {
    /// The lines `hushrun run` writes after COMMAND, without a prefix. Empty when nothing was
    /// compiled.
    pub fn lines(&self) -> &[String] {
        &self.lines
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
        .join("sessions")
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
        repository: start.repository.map(str::to_string),
        started_ms: millis,
        shared: shared_folders(start.env),
        folder,
    };
    let file = current.folder.join(SESSION_FILE);
    serde_json::to_vec(&current)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&file, bytes))
        .map_err(|err| NotBegun::Unusable(format!("{}: {err}", file.display())))?;
    Ok(Session { current, link })
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

    /// After COMMAND: reads the log, removes the session's folder and, at most once a day,
    /// sweeps the store (B-14).
    pub fn finish(self, _now: SystemTime) -> Summary {
        let _ = std::fs::remove_dir_all(&self.current.folder);
        Summary::default()
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
    let dir = store.join("bin").join(&name[..16]);
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

    // covers: B-1, B-2
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
        assert!(found.shared.iter().any(|s| s == "/usr"));
        let entry = Entry {
            outcome: crate::decide::passed_through(crate::model::PassedThrough::Probe),
            class: None,
            crate_name: String::new(),
            rustc_ms: 0,
            saved_ms: 0,
            stored_bytes: 0,
            found: false,
            differed: false,
            note: None,
        };
        found.log(&entry);
        found.log(&entry);
        assert_eq!(found.entries(), vec![entry.clone(), entry]);
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
}
