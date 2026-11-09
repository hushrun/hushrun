//! \[M8\] Isolated runs (E-12, plan §11.6): a check runs in its pinned image and platform, with
//! Docker or Podman, on an export of exactly the entries its scope's digest covers. The network
//! is off unless the check declares one, only the variables its `env` names go in, and secrets
//! never do. hushrun keeps the signing key on the host: the container only runs the check.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::anyhow;

use hushrun_core::event::{Environment, Network};
use hushrun_core::names::CheckName;
use hushrun_core::policy::{CheckDef, Shell};
use hushrun_core::time::Dur;
use hushrun_digest::{Entry, Kind};
use hushrun_vcs::Vcs;

use crate::app::App;
use crate::cmd::run::Program;
use crate::exit::{Exit, Failure};

/// The container runtime: `docker`, `podman`, or the path of a Docker-compatible CLI.
pub const RUNTIME_VAR: &str = "HUSHRUN_CONTAINER_RUNTIME";
/// The network a check that declares `network: restricted` runs on: one whose egress the
/// operator restricts. hushrun can't restrict a network by itself.
pub const RESTRICTED_NETWORK_VAR: &str = "HUSHRUN_RESTRICTED_NETWORK";
/// Where exports go. By default `<cache dir>/exports`, which Docker Desktop, Colima and Podman
/// machines share with their VMs, else the system's temporary directory.
pub const EXPORT_DIR_VAR: &str = "HUSHRUN_EXPORT_DIR";

/// Where the export is mounted, and the check's working directory.
pub const MOUNT: &str = "/src";

/// Docker and Podman answer 125 when they fail themselves, and when the check exits 125.
const RUNTIME_FAILED: i32 = 125;

/// The label on every container an isolated run starts, with the check's name: what is left of
/// runs that didn't end is found by it.
const LABEL: &str = "com.hushrun.check";

/// Exports and stopped containers this old are left from runs that didn't end, such as a killed
/// hushrun's: the next isolated run removes them.
const STALE: Duration = Duration::from_secs(24 * 3600);

/// Variables an isolated run never passes in, whatever a check's `env` names (E-12): hushrun's
/// and CI's credentials by name, and any name that reads like a secret.
const SECRET_NAMES: &[&str] = &[
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_RUNTIME_URL",
    "AMQP_URL",
    "CI_JOB_JWT",
    "CI_JOB_JWT_V2",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "HUSHRUN_AGENT",
    "MONGODB_URI",
    "MONGO_URL",
    "REDIS_URL",
    "SSH_AUTH_SOCK",
];
const SECRET_PARTS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "CREDENTIAL",
    "PRIVATE",
    "AUTH",
    "ACCESS_KEY",
    "APIKEY",
    "DATABASE_URL",
    "_KEY_",
];
const SECRET_SUFFIXES: &[&str] = &["_KEY", "_PAT", "_DSN", "_PWD"];

/// Whether an isolated run refuses to pass `name` in (E-12).
pub fn is_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_NAMES.contains(&upper.as_str())
        || SECRET_PARTS.iter().any(|part| upper.contains(part))
        || SECRET_SUFFIXES.iter().any(|suffix| upper.ends_with(suffix))
}

/// A Docker-compatible container runtime.
pub struct Runtime {
    program: PathBuf,
    /// `docker`, `podman` or the program's file name, and its version: the evidence's `runner`.
    pub description: String,
    podman: bool,
}

impl Runtime {
    /// What to look for: `HUSHRUN_CONTAINER_RUNTIME`, else `docker`, else `podman`.
    fn candidates(app: &App<'_>) -> Vec<String> {
        match app.env.get(RUNTIME_VAR).map(str::trim) {
            Some(value) => vec![value.to_string()],
            None => vec!["docker".into(), "podman".into()],
        }
    }

    /// Whether a runtime is here, found on `PATH` and never started: \[ADR 0045\] all `hushrun ready`
    /// and the hook need to know.
    pub fn is_here(app: &App<'_>) -> bool {
        Runtime::candidates(app)
            .iter()
            .any(|candidate| find_program(candidate).is_some())
    }

    /// `HUSHRUN_CONTAINER_RUNTIME`, else `docker`, else `podman`, found on `PATH`.
    pub fn detect(app: &App<'_>) -> Result<Runtime, Failure> {
        let candidates = Runtime::candidates(app);
        for candidate in &candidates {
            let Some(program) = find_program(candidate) else {
                continue;
            };
            let version = Command::new(&program)
                .arg("--version")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            let name = program
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("container runtime")
                .to_string();
            let podman = version.to_ascii_lowercase().starts_with("podman") || name == "podman";
            // `Docker version 27.3.1, build ce12230`, `podman version 5.2.3`: the number.
            let number = version
                .split([' ', ','])
                .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
                .unwrap_or("(unknown version)");
            return Ok(Runtime {
                description: format!("{name} {number}"),
                program,
                podman,
            });
        }
        Err(Failure::usage(anyhow!(
            "--isolated runs checks in containers, and {} isn't on PATH: install Docker or Podman, or set {RUNTIME_VAR} to a Docker-compatible CLI",
            match app.env.get(RUNTIME_VAR).map(str::trim) {
                Some(value) => format!("{value} ({RUNTIME_VAR})"),
                None => "neither docker nor podman".to_string(),
            }
        )))
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.stdin(Stdio::null());
        command
    }

    /// The platform of local image `image`, such as `linux/amd64`, or `None` when it isn't here.
    fn local_platform(&self, image: &str) -> Option<String> {
        let output = self
            .command()
            .args([
                "image",
                "inspect",
                "--format",
                "{{.Os}}/{{.Architecture}}{{if .Variant}}/{{.Variant}}{{end}}",
                image,
            ])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (output.status.success() && !text.is_empty()).then_some(text)
    }

    /// Makes `image` (pinned by digest) present for `platform`, pulling it when it isn't, and
    /// returns the platform it is for. A pull that fails is the runtime's failure, not the
    /// check's: nothing is recorded.
    pub fn prepare(&self, image: &str, platform: Option<&str>) -> Result<String, Failure> {
        if let Some(local) = self.local_platform(image)
            && platform.is_none_or(|p| p == local)
        {
            return Ok(local);
        }
        eprintln!("hushrun run: pulling {image}");
        let mut pull = self.command();
        pull.arg("pull");
        if let Some(platform) = platform {
            pull.args(["--platform", platform]);
        }
        let status = pull
            .arg(image)
            .stdout(Stdio::from(std::io::stderr()))
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| Failure::from(anyhow!("starting {}: {e}", self.description)))?;
        if !status.success() {
            return Err(Failure::new(
                Exit::Unexpected,
                anyhow!("{} couldn't pull {image} ({status})", self.description),
            ));
        }
        self.local_platform(image).ok_or_else(|| {
            Failure::new(
                Exit::Unexpected,
                anyhow!("{} pulled {image}, but can't inspect it", self.description),
            )
        })
    }

    /// Removes container `name`, running or not.
    fn remove(&self, name: &str) {
        let _ = self
            .command()
            .args(["rm", "--force", name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// What container `name` says of its check once the CLI exited, or `None` when the runtime
    /// has no such container, as when it failed before creating one.
    fn state(&self, name: &str) -> Option<ContainerState> {
        let output = self
            .command()
            .args([
                "container",
                "inspect",
                "--format",
                "{{.State.StartedAt}}|{{.State.ExitCode}}|{{.State.Error}}",
                name,
            ])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        ContainerState::parse(&String::from_utf8_lossy(&output.stdout))
    }

    /// Removes this runtime's stopped containers that isolated runs left more than a day ago.
    pub fn sweep(&self) {
        let _ = self
            .command()
            .args(["container", "prune", "--force", "--filter"])
            .arg(format!("label={LABEL}"))
            .arg("--filter")
            .arg(format!("until={}h", STALE.as_secs() / 3600))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// A container's state, from `container inspect`.
#[derive(Debug, PartialEq, Eq)]
struct ContainerState {
    /// Whether the check's process started: Docker and Podman show a zero time otherwise.
    started: bool,
    exit_code: i32,
    /// The runtime's own error, such as an entrypoint it couldn't start.
    error: String,
}

impl ContainerState {
    /// `<started at>|<exit code>|<error>`.
    fn parse(text: &str) -> Option<ContainerState> {
        let mut parts = text.trim_end().splitn(3, '|');
        let started_at = parts.next()?.trim();
        let exit_code = parts.next()?.trim().parse().ok()?;
        Some(ContainerState {
            started: !started_at.is_empty() && !started_at.starts_with("0001-01-01"),
            exit_code,
            error: parts.next().unwrap_or("").trim().to_string(),
        })
    }

    /// The check's exit code, when the check itself ran and ended: the runtime didn't fail.
    fn check_exit(&self) -> Option<i32> {
        (self.started && self.error.is_empty()).then_some(self.exit_code)
    }
}

/// The signals that stop an isolated run: what a terminal, a CI runner or an agent's harness
/// sends to stop hushrun.
#[cfg(unix)]
const STOP_SIGNALS: &[i32] = &[
    signal_hook::consts::SIGINT,
    signal_hook::consts::SIGTERM,
    signal_hook::consts::SIGHUP,
];
#[cfg(not(unix))]
const STOP_SIGNALS: &[i32] = &[signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM];

/// How hushrun handles [`STOP_SIGNALS`], once an isolated run installs it: as by default while
/// `idle`, else by recording the signal in `arrived`, for the run to remove its container and
/// export before it ends hushrun as the signal asked.
struct Signals {
    idle: Arc<AtomicBool>,
    arrived: Arc<AtomicUsize>,
}

/// The handlers, installed on first use; `None` when the system refused them, and then the
/// signals act as by default.
fn signals() -> Option<&'static Signals> {
    static SIGNALS: OnceLock<Option<Signals>> = OnceLock::new();
    SIGNALS
        .get_or_init(|| {
            let signals = Signals {
                idle: Arc::new(AtomicBool::new(true)),
                arrived: Arc::new(AtomicUsize::new(0)),
            };
            for &signal in STOP_SIGNALS {
                // Handlers run in the order they are registered: the default action first, so an
                // idle hushrun ends at once.
                let installed = signal_hook::flag::register_conditional_default(
                    signal,
                    Arc::clone(&signals.idle),
                )
                .and_then(|_| {
                    signal_hook::flag::register_usize(
                        signal,
                        Arc::clone(&signals.arrived),
                        usize::try_from(signal).unwrap_or(0),
                    )
                });
                if let Err(e) = installed {
                    tracing::warn!(
                        "can't handle signal {signal} ({e}): stopping hushrun while a check runs isolated may leave its container and export behind"
                    );
                    return None;
                }
            }
            Some(signals)
        })
        .as_ref()
}

/// While held, a stop signal no longer ends hushrun at once: the check's container stops, and
/// the run removes it and the export, then calls [`end_as`]. Dropping it restores the default.
pub struct Held(());

impl Held {
    pub fn hold() -> Held {
        if let Some(signals) = signals() {
            signals.arrived.store(0, Ordering::SeqCst);
            signals.idle.store(false, Ordering::SeqCst);
        }
        Held(())
    }

    /// The stop signal that arrived while held.
    pub fn signal(&self) -> Option<i32> {
        signals()
            .map(|s| s.arrived.load(Ordering::SeqCst))
            .filter(|&signal| signal != 0)
            .and_then(|signal| i32::try_from(signal).ok())
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(signals) = signals() {
            signals.idle.store(true, Ordering::SeqCst);
        }
    }
}

/// Whether a stop signal arrived while a [`Held`] is held.
fn interrupted() -> bool {
    signals().is_some_and(|s| s.arrived.load(Ordering::SeqCst) != 0)
}

/// Ends hushrun as `signal` would have without the handlers.
pub fn end_as(signal: i32) -> ! {
    let _ = signal_hook::low_level::emulate_default_handler(signal);
    std::process::exit(128 + signal)
}

/// `program` as given when it is a path, else found on `PATH`.
fn find_program(program: &str) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    let given = Path::new(program);
    if given.components().count() > 1 {
        return given.is_file().then(|| given.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        cfg!(windows)
            .then(|| dir.join(format!("{program}.exe")))
            .filter(|c| c.is_file())
    })
}

/// How a check is isolated, from its definition (C-5): its pinned image and platform, its
/// network, and the variables it may see.
pub struct Isolation {
    pub image: String,
    pub platform: Option<String>,
    pub network: Network,
    /// The `env` names set here; the rest the check doesn't see.
    pub env: Vec<String>,
}

impl Isolation {
    /// Everything is checked before anything runs: an image pinned by digest, no secret in
    /// `env`, and a network hushrun can provide.
    pub fn of(app: &App<'_>, name: &CheckName, def: &CheckDef) -> Result<Isolation, Failure> {
        let Some(image) = &def.image else {
            return Err(Failure::usage(anyhow!(
                "check `{name}` pins no `image`, so it can't run isolated: pin the image CI runs it in, by digest, in hushrun.yaml"
            )));
        };
        let secrets: Vec<&String> = def.env.iter().filter(|v| is_secret(v)).collect();
        if !secrets.is_empty() {
            return Err(Failure::usage(anyhow!(
                "check `{name}` names {} in `env`, and an isolated run never passes secrets in: run it without --isolated, or drop {}",
                list(&secrets),
                if secrets.len() == 1 { "it" } else { "them" }
            )));
        }
        let network = def.network.unwrap_or(Network::None);
        if network == Network::Restricted && app.env.get(RESTRICTED_NETWORK_VAR).is_none() {
            return Err(Failure::usage(anyhow!(
                "check `{name}` declares `network: restricted`, which hushrun can't provide by itself: set {RESTRICTED_NETWORK_VAR} to a Docker or Podman network whose egress you restrict"
            )));
        }
        let mut env = Vec::new();
        for var in &def.env {
            if app.env.get(var).is_some() {
                env.push(var.clone());
            } else {
                tracing::warn!("check `{name}` names {var} in `env`, and it isn't set here");
            }
        }
        Ok(Isolation {
            image: image.clone(),
            platform: def.platform.clone(),
            network,
            env,
        })
    }

    /// The evidence's environment (E-7): the pinned image, the platform it ran on, the network,
    /// and the runtime.
    pub fn environment(&self, platform: &str, runtime: &Runtime) -> Environment {
        Environment {
            image: Some(self.image.clone()),
            platform: Some(
                self.platform
                    .clone()
                    .unwrap_or_else(|| platform.to_string()),
            ),
            network: Some(self.network),
            runner: Some(format!("hushrun run --isolated ({})", runtime.description)),
        }
    }
}

fn list(names: &[&String]) -> String {
    names
        .iter()
        .map(|n| n.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// An export of a scope's entries: a new directory holding exactly them, with their bytes and
/// modes, removed when dropped.
pub struct Export {
    pub dir: PathBuf,
}

impl Export {
    /// Writes `entries` under a new directory in `base`: regular files with mode 0644 or 0755,
    /// symbolic links with their targets as stored, and a gitlink as an empty directory, read
    /// through the VCS's own readers (§11.1). Paths are normalized (D-2) and nothing on the way
    /// is followed, so no entry lands outside the export. The export is then listed again and
    /// must hold exactly those paths: a case-insensitive filesystem that folds two of them into
    /// one fails here, not in the check.
    pub fn materialize(vcs: &dyn Vcs, entries: &[&Entry], base: &Path) -> Result<Export, Failure> {
        std::fs::create_dir_all(base)
            .map_err(|e| Failure::from(anyhow!("creating {}: {e}", base.display())))?;
        sweep_exports(base, jiff::Timestamp::now().as_millisecond());
        let dir = base.join(format!(
            "hushrun-{}",
            hushrun_ledger::new_ulid(jiff::Timestamp::now().as_millisecond()).to_lowercase()
        ));
        std::fs::create_dir(&dir)
            .map_err(|e| Failure::from(anyhow!("creating {}: {e}", dir.display())))?;
        let export = Export { dir };
        let mut failure: Option<Failure> = None;
        let result = vcs.read_entries(entries, &mut |entry, bytes| {
            if let Err(err) = export.write(entry, &bytes) {
                failure = Some(err);
                return Err(hushrun_vcs::Error::Io {
                    path: export.dir.join(&entry.path),
                    source: std::io::Error::other("export failed"),
                });
            }
            Ok(())
        });
        if let Some(failure) = failure {
            return Err(failure);
        }
        result?;
        export.check(entries)?;
        Ok(export)
    }

    fn write(&self, entry: &Entry, bytes: &[u8]) -> Result<(), Failure> {
        let path = self.place(&entry.path)?;
        let fail = |what: &str, e: std::io::Error| {
            Failure::from(anyhow!("{what} {} in the export: {e}", entry.path))
        };
        match entry.kind {
            Kind::File | Kind::Exec => {
                use std::io::Write as _;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|e| collision(&entry.path, e))?;
                file.write_all(bytes).map_err(|e| fail("writing", e))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    let mode = if entry.kind == Kind::Exec {
                        0o755
                    } else {
                        0o644
                    };
                    file.set_permissions(std::fs::Permissions::from_mode(mode))
                        .map_err(|e| fail("setting the mode of", e))?;
                }
            }
            Kind::Link => {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt as _;
                    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(bytes), &path)
                        .map_err(|e| collision(&entry.path, e))?;
                }
                #[cfg(not(unix))]
                {
                    let _ = (bytes, path);
                    return Err(Failure::usage(anyhow!(
                        "{} is a symbolic link, and isolated runs export links on POSIX hosts only",
                        entry.path
                    )));
                }
            }
            // A submodule's content isn't digested, only its commit (D-3): an empty directory,
            // as an uninitialized submodule checks out.
            Kind::Gitlink => {
                std::fs::create_dir(&path).map_err(|e| collision(&entry.path, e))?;
            }
        }
        Ok(())
    }

    /// The host path of `relative`, with its parent directories created. Every component is
    /// checked again (D-2) as one name on this host, and an existing one must be a directory,
    /// never a link.
    fn place(&self, relative: &str) -> Result<PathBuf, Failure> {
        let segments: Vec<&str> = relative.split('/').collect();
        if let Some(segment) = segments.iter().find(|s| !one_name(s, cfg!(windows))) {
            return Err(Failure::usage(anyhow!(
                "{relative:?} has {segment:?}, which isn't one file name on this host, so it can't be exported"
            )));
        }
        let mut path = self.dir.clone();
        for segment in &segments[..segments.len() - 1] {
            path.push(segment);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(Failure::usage(anyhow!(
                        "{relative}: {} is not a directory in the export",
                        path.display()
                    )));
                }
                Err(_) => std::fs::create_dir(&path).map_err(|e| collision(relative, e))?,
            }
        }
        path.push(segments[segments.len() - 1]);
        // What one_name lets through stays in the export, segment by segment.
        if !path.starts_with(&self.dir)
            || path.components().count() != self.dir.components().count() + segments.len()
        {
            return Err(Failure::usage(anyhow!(
                "{relative:?} would land outside the export, so it can't be exported"
            )));
        }
        Ok(path)
    }

    /// The export holds exactly `entries`' paths (D-11 lists it as a check would see it).
    fn check(&self, entries: &[&Entry]) -> Result<(), Failure> {
        let listed = hushrun_vcs::dir::Dir::open(&self.dir, hushrun_vcs::Options::default())?
            .list(&hushrun_vcs::Snapshot::default())?;
        let found: BTreeSet<&str> = listed.entries.iter().map(|e| e.path.as_str()).collect();
        let wanted: BTreeSet<&str> = entries
            .iter()
            .filter(|e| e.kind != Kind::Gitlink)
            .map(|e| e.path.as_str())
            .collect();
        if found != wanted {
            let missing: Vec<&str> = wanted.difference(&found).copied().take(5).collect();
            let extra: Vec<&str> = found.difference(&wanted).copied().take(5).collect();
            return Err(Failure::usage(anyhow!(
                "the export in {} doesn't hold exactly the digested files (missing: {}; unexpected: {}): on a case-insensitive filesystem, paths that differ only in case collide; set {EXPORT_DIR_VAR} to a case-sensitive one",
                self.dir.display(),
                if missing.is_empty() {
                    "none".into()
                } else {
                    missing.join(", ")
                },
                if extra.is_empty() {
                    "none".into()
                } else {
                    extra.join(", ")
                },
            )));
        }
        Ok(())
    }
}

/// Whether `segment` names one entry of a directory, on Windows when `windows`: never empty, `.`
/// or `..`, one normal path component, and on Windows none of what it reads as something else: a
/// separator, a drive or stream (`:`), a device name such as `NUL` or `COM1`, a name it strips
/// of a final dot or space, or a character its files can't have.
fn one_name(segment: &str, windows: bool) -> bool {
    let mut components = Path::new(segment).components();
    let normal = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    );
    if segment.is_empty() || segment == "." || segment == ".." || !normal {
        return false;
    }
    if !windows {
        return true;
    }
    if segment.ends_with(['.', ' '])
        || segment
            .chars()
            .any(|c| matches!(c, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*') || c < ' ')
    {
        return false;
    }
    // A device name stays a device with any extension: `nul.txt` is NUL.
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .trim_end()
        .to_ascii_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem[3..].chars().count() == 1
        && stem[3..]
            .chars()
            .all(|c| matches!(c, '1'..='9' | '¹' | '²' | '³')));
    !device
}

/// Removes the exports in `base` that runs left more than [`STALE`] before `now_ms`: a run that
/// ends removes its own, and one that was killed can't.
fn sweep_exports(base: &Path, now_ms: i64) {
    let Ok(dir) = std::fs::read_dir(base) else {
        return;
    };
    let stale = i64::try_from(STALE.as_millis()).unwrap_or(i64::MAX);
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(millis) = name
            .to_str()
            .and_then(|n| n.strip_prefix("hushrun-"))
            .and_then(hushrun_ledger::ulid_millis)
        else {
            continue;
        };
        if now_ms.saturating_sub(millis) > stale
            && entry.file_type().is_ok_and(|t| t.is_dir())
            && let Err(e) = std::fs::remove_dir_all(entry.path())
        {
            tracing::debug!(
                "couldn't remove the stale export {}: {e}",
                entry.path().display()
            );
        }
    }
}

/// E-12: the signing key stays with hushrun. A key file among the entries an isolated run
/// exports would reach the check, and with `--worktree` the commit, so the run is refused.
pub fn refuse_exported_key(app: &App<'_>, root: &Path, entries: &[&Entry]) -> Result<(), Failure> {
    let Some(file) = app.env.get("HUSHRUN_SIGNING_KEY_FILE") else {
        return Ok(());
    };
    let (Ok(key), Ok(root)) = (std::fs::canonicalize(file), std::fs::canonicalize(root)) else {
        return Ok(());
    };
    let Ok(relative) = key.strip_prefix(&root) else {
        return Ok(());
    };
    let relative = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if entries.iter().any(|e| e.path == relative) {
        return Err(Failure::usage(anyhow!(
            "the signing key HUSHRUN_SIGNING_KEY_FILE is {relative}, in the content this run exports: the check would see it, and committing this content would publish it. Keep the key outside the repository"
        )));
    }
    Ok(())
}

/// A path that already exists in the export: two entries that the filesystem folds into one.
fn collision(path: &str, e: std::io::Error) -> Failure {
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        Failure::usage(anyhow!(
            "{path} collides with another digested path in the export: on a case-insensitive filesystem, paths that differ only in case are one; set {EXPORT_DIR_VAR} to a case-sensitive directory"
        ))
    } else {
        Failure::from(anyhow!("exporting {path}: {e}"))
    }
}

impl Export {
    /// Removes the export after a run in `image`. What the container wrote as its root user, a
    /// rootful runtime leaves owned by root on the host, where hushrun can't delete it: the
    /// same image empties the export first.
    pub fn remove(self, runtime: &Runtime, image: &str) {
        if std::fs::remove_dir_all(&self.dir).is_ok() {
            return;
        }
        let mut volume = self.dir.as_os_str().to_owned();
        volume.push(format!(":{MOUNT}"));
        if runtime.podman {
            volume.push(":z");
        }
        let _ = runtime
            .command()
            .args(["run", "--rm", "--pull", "never", "--network", "none"])
            .arg("--volume")
            .arg(volume)
            .args(["--entrypoint", "sh", image, "-c"])
            .arg(format!("rm -rf {MOUNT}/..?* {MOUNT}/.[!.]* {MOUNT}/*"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // Drop removes what is left, or warns.
    }
}

impl Drop for Export {
    fn drop(&mut self) {
        if self.dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&self.dir)
        {
            tracing::warn!("couldn't remove the export {}: {e}", self.dir.display());
        }
    }
}

/// The directory exports go in: `HUSHRUN_EXPORT_DIR`, else `<cache dir>/exports`, else the
/// system's temporary directory.
pub fn export_base(app: &App<'_>) -> PathBuf {
    if let Some(dir) = app.env.get(EXPORT_DIR_VAR) {
        return PathBuf::from(dir);
    }
    crate::context::cache_dir()
        .map(|d| d.join("exports"))
        .unwrap_or_else(std::env::temp_dir)
}

/// Runs `command` once in a new container on `export`: the exit code, or `None` when it was
/// stopped at `timeout` or by a stop signal while [`Held`]. The container's output goes to
/// hushrun's stderr. The runtime's own failure is an error: nothing about the content was
/// learned. The container is removed in every case.
pub fn execute(
    runtime: &Runtime,
    isolation: &Isolation,
    restricted_network: Option<&str>,
    command: &Program,
    export: &Export,
    check: &CheckName,
    timeout: Option<Dur>,
) -> Result<Option<i32>, Failure> {
    let name = format!(
        "hushrun-{check}-{}",
        hushrun_ledger::new_ulid(jiff::Timestamp::now().as_millisecond()).to_lowercase()
    );
    let mut docker = runtime.command();
    // Not `--rm`: the container's state tells the runtime's failures from the check's exit 125.
    // `--init` forwards signals to the check and reaps what it leaves, as CI's runners do.
    docker
        .args([
            "run", "--name", &name, "--pull", "never", "--init", "--label",
        ])
        .arg(format!("{LABEL}={check}"));
    match isolation.network {
        Network::None => {
            docker.args(["--network", "none"]);
        }
        Network::Restricted => {
            docker.args([
                "--network",
                restricted_network.expect("checked before running"),
            ]);
        }
        Network::Open => {}
    }
    if let Some(platform) = &isolation.platform {
        docker.args(["--platform", platform]);
    }
    let mut volume = export.dir.as_os_str().to_owned();
    volume.push(format!(":{MOUNT}"));
    // Podman on SELinux hosts needs the mount relabeled to read it.
    if runtime.podman {
        volume.push(":z");
    }
    docker
        .args(["--security-opt", "no-new-privileges"])
        .arg("--volume")
        .arg(volume)
        .args(["--workdir", MOUNT]);
    // Values never reach the command line: the runtime reads each from its own environment.
    for var in &isolation.env {
        docker.args(["--env", var]);
    }
    match command {
        Program::Shell { shell, script } => {
            let (program, flags): (&str, &[&str]) = match shell {
                Shell::Sh => ("sh", &["-c"]),
                Shell::Bash => ("bash", &["-c"]),
                Shell::Pwsh => ("pwsh", &["-NoProfile", "-NonInteractive", "-Command"]),
                Shell::Cmd => {
                    return Err(Failure::usage(anyhow!(
                        "check `{check}` runs in cmd, and isolated runs use Linux containers"
                    )));
                }
            };
            docker
                .args(["--entrypoint", program, &isolation.image])
                .args(flags)
                .arg(script);
        }
    }
    docker
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::inherit());
    let child = docker
        .spawn()
        .map_err(|e| Failure::from(anyhow!("starting {}: {e}", runtime.description)))?;
    let waited = wait(child, runtime, &name, timeout);
    // The CLI answers 125 for the runtime's failures and for a check that exits 125: only the
    // container knows which.
    let result = match waited {
        Ok(Some(RUNTIME_FAILED)) => match runtime.state(&name).and_then(|s| s.check_exit()) {
            Some(code) => Ok(Some(code)),
            None => Err(Failure::new(
                Exit::Unexpected,
                anyhow!(
                    "{} failed to run check `{check}` (exit {RUNTIME_FAILED}): nothing was recorded",
                    runtime.description
                ),
            )),
        },
        other => other,
    };
    runtime.remove(&name);
    result
}

/// Waits for the CLI running container `name`, polling for `timeout` and stop signals.
fn wait(
    mut child: Child,
    runtime: &Runtime,
    name: &str,
    timeout: Option<Dur>,
) -> Result<Option<i32>, Failure> {
    let deadline = timeout.map(|t| Instant::now() + t.as_std());
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code()),
            Ok(None) => {}
            Err(e) => {
                discard(&mut child, runtime, name);
                return Err(Failure::from(anyhow!("waiting for the container: {e}")));
            }
        }
        if interrupted() {
            discard(&mut child, runtime, name);
            return Ok(None);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            discard(&mut child, runtime, name);
            eprintln!(
                "hushrun run: stopped after {} s (timeout)",
                timeout.map_or(0, |t| t.as_std().as_secs())
            );
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Removes container `name`, which stopping the CLI wouldn't stop, then stops and reaps the CLI.
fn discard(child: &mut Child, runtime: &Runtime, name: &str) {
    runtime.remove(name);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: E-12
    #[test]
    fn secrets_never_go_in() {
        for name in [
            "HUSHRUN_AGENT",
            "HUSHRUN_MACHINE_KEY",
            "HUSHRUN_SIGNING_KEY",
            "HUSHRUN_SIGNING_KEY_FILE",
            "HUSHRUN_REPOSITORY_TOKEN",
            "HUSHRUN_ORG_TOKEN_COMMAND",
            "SIGSTORE_ID_TOKEN",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "NPM_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_ACCESS_KEY_ID",
            "AWS_SESSION_TOKEN",
            "AZURE_CLIENT_SECRET",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "DB_PASSWORD",
            "STRIPE_API_KEY",
            "SSH_AUTH_SOCK",
            "docker_auth_config",
            "DATABASE_URL",
            "TEST_DATABASE_URL",
            "REDIS_URL",
            "MONGODB_URI",
            "SENTRY_DSN",
            "GITLAB_PAT",
            "MYSQL_PWD",
            "OPENAI_APIKEY",
        ] {
            assert!(is_secret(name), "{name}");
        }
        for name in [
            "GO_VERSION",
            "NODE_ENV",
            "RUST_LOG",
            "TZ",
            "LANG",
            "KEYBOARD",
            "PWD",
            "PATH",
            "API_URL",
        ] {
            assert!(!is_secret(name), "{name}");
        }
    }

    // covers: E-12, D-2
    #[test]
    fn exported_paths_are_one_name_per_segment() {
        for (segment, posix, windows) in [
            ("main.go", true, true),
            (".github", true, true),
            ("a b", true, true),
            ("", false, false),
            (".", false, false),
            ("..", false, false),
            // A backslash is a name on POSIX, a separator on Windows.
            ("a\\b", true, false),
            ("..\\..\\x", true, false),
            // Drives and alternate data streams.
            ("C:", true, false),
            ("C:x", true, false),
            ("file:stream", true, false),
            // Devices, with any extension and in any case.
            ("NUL", true, false),
            ("nul.txt", true, false),
            ("Com1.log", true, false),
            ("lpt9", true, false),
            ("COM\u{b9}", true, false),
            ("CONIN$", true, false),
            ("COM10", true, true),
            ("console", true, true),
            ("nullable.go", true, true),
            // Names Windows strips, and characters its files can't have.
            ("trailing.", true, false),
            ("trailing ", true, false),
            ("what?", true, false),
            ("a*b", true, false),
            ("tab\there", true, false),
        ] {
            assert_eq!(one_name(segment, false), posix, "{segment:?} on POSIX");
            assert_eq!(one_name(segment, true), windows, "{segment:?} on Windows");
        }
    }

    // covers: E-12
    #[test]
    fn container_states_tell_the_checks_exit_from_the_runtimes_failure() {
        // Docker's and Podman's renderings of a container that ran its check.
        for text in [
            "2026-10-16T12:00:00.123456789Z|125|\n",
            "2026-10-16 12:00:00.123456789 +0000 UTC|125|\n",
        ] {
            assert_eq!(ContainerState::parse(text).unwrap().check_exit(), Some(125));
        }
        // Never started: the entrypoint couldn't start, or the runtime failed first.
        for text in [
            "0001-01-01T00:00:00Z|127|exec: \"bash\": executable file not found in $PATH\n",
            "0001-01-01 00:00:00 +0000 UTC|0|\n",
            "2026-10-16T12:00:00Z|125|OCI runtime error: a|b\n",
        ] {
            assert_eq!(
                ContainerState::parse(text).unwrap().check_exit(),
                None,
                "{text}"
            );
        }
        assert_eq!(ContainerState::parse("garbage"), None);
        assert_eq!(
            ContainerState::parse("2026-10-16T12:00:00Z|1|a|b")
                .unwrap()
                .error,
            "a|b"
        );
    }
}
