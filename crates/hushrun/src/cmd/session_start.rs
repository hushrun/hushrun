//! \[ADR 0161\] `hushrun hook session-start` (C-168): what a hosted agent's session-start hook runs.
//! It warms the sandbox, then prints a few lines on stdout that the agent reads from its first
//! turn: what this exact code already passed, what is left to run here and what is left to CI.
//!
//! The hook `hushrun init --agent-clouds claude-code` writes calls this command and nothing else,
//! so what a session gets at its start improves with hushrun's releases, and a repository never
//! runs init again for it.
//!
//! It never fails a session: it exits 0 whatever happens, and within its budget. The steps run as
//! hushrun's own commands, as child processes whose output goes to a log in hushrun's cache
//! directory, never to stdout, which the agent reads. A step the budget cuts off goes on in the
//! background, and the steps after it are skipped. What `hushrun ready` knows comes last, with a
//! part of the budget kept for it; when the code takes longer to hash than that, a reduced block
//! says so.
//!
//! \[ADR 0165\] It reads the hook's payload on stdin for the workspace's folder. With `--format
//! antigravity` the block goes out as Google Antigravity's `injectSteps` JSON, the reduced one
//! too. A start within [`STAMP_MINUTES`] of another on the same commit skips the steps and says
//! what that one said, and a lock keeps two starts, or a start and a step an earlier one left
//! running, from pulling at once.

use std::fmt::Write as _;
use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use hushrun_ci::platforms::{Kind, Platform};
use hushrun_vcs::Snapshot;

use crate::cli::{GlobalOpts, SessionStartArgs};
use crate::cmd::hook_input;
use crate::cmd::ready::{self, State, Target};
use crate::exit::Exit;
use crate::output::Format;
use crate::results::{Backend, Credential, Selection};

/// The default budget, in seconds: well under the 60 seconds Claude Code gives a command hook,
/// with room for `npx` to start hushrun.
pub const BUDGET_SECONDS: u64 = 45;

/// \[C-145\] The `timeout` init gives the hook, in seconds: the budget, and `npx`'s start.
pub const HOOK_TIMEOUT_SECONDS: u64 = 60;

/// The most of the budget kept for what `hushrun ready` knows.
const MOST_KEPT_FOR_CONTEXT: Duration = Duration::from_secs(15);

/// A step starts only with at least this much of the budget left.
const LEAST_FOR_A_STEP: Duration = Duration::from_secs(1);

/// How often a running step is looked at.
const POLL: Duration = Duration::from_millis(50);

/// The most checks a line names; the rest are a count.
const MOST_NAMED: usize = 6;

/// \[ADR 0165\] How long a start's steps stand for another start on the same commit, in
/// minutes: a subagent's conversation, or one resumed, starts its own hook soon after.
pub const STAMP_MINUTES: u64 = 10;

/// One step, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Step {
    /// `hushrun build pull` (B-240): the baked compiled code, into `/hushrun`. A public
    /// repository's needs no credential.
    BuildPull,
    /// \[ADR 0163\] `hushrun packages pull` (K-15): the baked packages, into
    /// `/hushrun-packages`, before anything installs. A public repository's, or one
    /// `HUSHRUN_PACKAGES_IMAGE` names, needs no credential for the backend.
    PackagesPull,
    /// `hushrun fetch` (C-144): the newest results of this tree, and compiled code.
    Fetch,
}

/// The steps, in order. \[ADR 0163\] `hushrun packages pull` goes between the other two: the baked
/// packages, before anything installs.
pub(crate) const STEPS: &[Step] = &[Step::BuildPull, Step::PackagesPull, Step::Fetch];

impl Step {
    /// How lines name it: the command.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Step::BuildPull => "build pull",
            Step::PackagesPull => "packages pull",
            Step::Fetch => "fetch",
        }
    }

    /// hushrun's arguments that run it.
    fn args(self) -> &'static [&'static str] {
        match self {
            Step::BuildPull => &["build", "pull", "-q"],
            Step::PackagesPull => &["packages", "pull", "-q"],
            Step::Fetch => &["fetch", "-q"],
        }
    }

    /// Whether it does nothing without a credential for the backend.
    fn needs_credential(self) -> bool {
        match self {
            Step::BuildPull | Step::PackagesPull => false,
            Step::Fetch => true,
        }
    }
}

/// Why a step wasn't run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Skip {
    NoCredential,
    NoTime,
}

impl Skip {
    fn said(self) -> &'static str {
        match self {
            Skip::NoCredential => "no credential for the backend",
            Skip::NoTime => "out of time",
        }
    }
}

/// What became of a step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Outcome {
    Done,
    /// It ended with this exit code (or none, for a signal).
    Failed(Option<i32>),
    /// It couldn't start.
    NotStarted(String),
    /// Not run, for this reason.
    Skipped(Skip),
    /// The budget ran out while it ran: it goes on in the background.
    StillRunning,
    /// A pull found no store published under its tag ([`Exit::NotPublished`]): nothing to pull
    /// yet, which isn't a failure.
    NotPublished,
}

impl Outcome {
    fn said(&self) -> String {
        match self {
            Outcome::Done => "done".into(),
            Outcome::Failed(Some(code)) => format!("failed (exit {code})"),
            Outcome::Failed(None) => "stopped by a signal".into(),
            Outcome::NotStarted(_) => "couldn't start".into(),
            Outcome::Skipped(why) => format!("skipped ({})", why.said()),
            Outcome::StillRunning => "still running in the background".into(),
            Outcome::NotPublished => "none published yet".into(),
        }
    }

    /// What a step's exit code says.
    fn of_exit(code: Option<i32>) -> Outcome {
        match code {
            Some(0) => Outcome::Done,
            Some(code) if code == Exit::NotPublished as i32 => Outcome::NotPublished,
            code => Outcome::Failed(code),
        }
    }
}

/// Why a step is skipped without a credential.
const NO_CREDENTIAL: Skip = Skip::NoCredential;
/// Why a step is skipped when the budget is spent.
const NO_TIME: Skip = Skip::NoTime;

/// The budget: when it began, and when the steps and the whole command end.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Budget {
    pub steps_end: Instant,
    pub end: Instant,
}

impl Budget {
    /// `total` from `start`, with a third of it, at most [`MOST_KEPT_FOR_CONTEXT`], kept for the
    /// context after the steps.
    pub(crate) fn new(start: Instant, total: Duration) -> Budget {
        let kept = (total / 3).min(MOST_KEPT_FOR_CONTEXT);
        Budget {
            steps_end: start + total.saturating_sub(kept),
            end: start + total,
        }
    }
}

/// Runs `steps` in order within `budget`: one that needs a credential is skipped without one,
/// and once one is still running when the steps' time ends, the rest are skipped. `run` runs a
/// step with the time it has; `now` is the clock.
pub(crate) fn run_steps(
    steps: &[Step],
    budget: Budget,
    credential: bool,
    now: &dyn Fn() -> Instant,
    run: &mut dyn FnMut(Step, Instant) -> Outcome,
) -> Vec<(Step, Outcome)> {
    let mut done = Vec::new();
    let mut out_of_time = false;
    for &step in steps {
        let outcome = if out_of_time {
            Outcome::Skipped(NO_TIME)
        } else if step.needs_credential() && !credential {
            Outcome::Skipped(NO_CREDENTIAL)
        } else if budget.steps_end.saturating_duration_since(now()) < LEAST_FOR_A_STEP {
            out_of_time = true;
            Outcome::Skipped(NO_TIME)
        } else {
            let outcome = run(step, budget.steps_end);
            out_of_time = outcome == Outcome::StillRunning;
            outcome
        };
        done.push((step, outcome));
    }
    done
}

/// Where results go, as the context says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sharing {
    /// A server, with a credential: results are shared.
    Shared,
    /// This machine only: nothing here signs in to the backend.
    NoCredential,
    /// Results are off.
    Off,
}

/// \[C-11, C-13\] Where results go from here: what `hushrun run` would pick.
fn sharing(root: Option<&Path>) -> Sharing {
    match Backend::select(root) {
        Ok(Selection::On(backend)) if backend.local().is_some() => Sharing::NoCredential,
        Ok(Selection::On(backend)) => match backend.credential() {
            None | Some(Credential::Nothing) => Sharing::NoCredential,
            Some(_) => Sharing::Shared,
        },
        Ok(Selection::Off(_)) => Sharing::Off,
        Err(_) => Sharing::NoCredential,
    }
}

/// What `hushrun ready` said, as the context tells it: names of checks only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Summary {
    /// The commit, short.
    pub code: String,
    pub passed: Vec<String>,
    pub missing: Vec<String>,
    pub ci: Vec<String>,
    /// \[ADR 0067\] Its results stay on this machine.
    pub here: bool,
    /// A lookup of the backend was refused or got no answer.
    pub lookups_failed: bool,
}

impl Summary {
    fn of(report: &ready::Report, lookups_failed: bool) -> Summary {
        let named = |state: State| {
            report
                .checks
                .iter()
                .filter(|item| item.state == state)
                .map(|item| item.check.clone())
                .collect()
        };
        Summary {
            code: report.short(),
            passed: named(State::Passed),
            missing: named(State::Missing),
            ci: named(State::Ci),
            here: report.here(),
            lookups_failed,
        }
    }

    fn total(&self) -> usize {
        self.passed.len() + self.missing.len() + self.ci.len()
    }
}

/// `a, b and c`, at most [`MOST_NAMED`] of them, then a count of the rest.
fn names(list: &[String]) -> String {
    let shown: Vec<&str> = list.iter().take(MOST_NAMED).map(String::as_str).collect();
    let rest = list.len().saturating_sub(MOST_NAMED);
    match (shown.as_slice(), rest) {
        ([], _) => String::new(),
        ([one], 0) => (*one).to_string(),
        ([first @ .., last], 0) => format!("{} and {last}", first.join(", ")),
        (all, rest) => format!("{} and {rest} more", all.join(", ")),
    }
}

/// What the context says when `ready` didn't answer within the budget.
pub(crate) const NO_ANSWER: &str = "- hushrun couldn't tell within this hook's time what this code already passed (hashing it takes longer): `hushrun ready` says.";

/// The last line: what to do before a push.
pub(crate) const BEFORE_PUSH: &str = "- Before you push: commit, run the commands `hushrun ready` lists, and push once it says the code is ready.";

/// The block on stdout: at most 10 lines, names of steps and checks only, never a value of a
/// variable. `summary` is none when `ready` didn't answer in time.
pub(crate) fn block(
    platform: &str,
    steps: &[(Step, Outcome)],
    sharing: &Sharing,
    summary: Option<&Summary>,
) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "hushrun, at the start of this {platform} session:");
    if !steps.is_empty() {
        let said: Vec<String> = steps
            .iter()
            .map(|(step, outcome)| format!("{} {}", step.name(), outcome.said()))
            .collect();
        let _ = writeln!(text, "- Setup: {}.", said.join("; "));
    }
    match summary {
        None => {
            let _ = writeln!(text, "{NO_ANSWER}");
        }
        Some(summary) if summary.total() == 0 => {
            let _ = writeln!(
                text,
                "- hushrun knows no checks for {}: hushrun.yaml's `deploy`, package.json's `hushrun run` scripts and CI's files name them.",
                summary.code
            );
        }
        Some(summary) => {
            let passed = summary.passed.len();
            let skips = if summary.here {
                "hushrun run skips them here, and CI still runs them"
            } else {
                "hushrun run skips them here, and CI skips them"
            };
            if passed > 0 {
                let _ = writeln!(
                    text,
                    "- {passed} of {} already passed on this exact code ({}): {skips}.",
                    ready::count(summary.total()),
                    summary.code
                );
            } else {
                let _ = writeln!(
                    text,
                    "- None of {} has passed on this exact code ({}) yet.",
                    ready::count(summary.total()),
                    summary.code
                );
            }
            if !summary.missing.is_empty() {
                let _ = writeln!(text, "- Still to run here: {}.", names(&summary.missing));
            }
            if !summary.ci.is_empty() {
                let _ = writeln!(
                    text,
                    "- Left to CI or another machine: {}.",
                    names(&summary.ci)
                );
            }
        }
    }
    let lookups_failed = summary.is_some_and(|s| s.lookups_failed);
    match sharing {
        Sharing::NoCredential => {
            let _ = writeln!(
                text,
                "- Results stay on this machine, so CI runs every check again: this session has no credential for the backend (HUSHRUN_AGENT, HUSHRUN_AUTH=proxy or a session). `hushrun doctor` says what it needs."
            );
        }
        Sharing::Off => {
            let _ = writeln!(text, "- Results are off here: `hushrun doctor` says why.");
        }
        Sharing::Shared if lookups_failed => {
            let _ = writeln!(
                text,
                "- The backend refused or didn't answer this session's requests, so results may stay on this machine: `hushrun doctor` says which hosts to allow and what the credential needs."
            );
        }
        Sharing::Shared => {}
    }
    let _ = writeln!(text, "{BEFORE_PUSH}");
    text
}

/// \[ADR 0165\] The block as the hook's format has it: plain text, or Antigravity's
/// `injectSteps` with the text as one system message. None is what a hook with nothing to say
/// prints: nothing, or `{}`.
pub(crate) fn render(format: Format, text: Option<&str>) -> String {
    match (format, text) {
        (Format::Antigravity, Some(text)) => {
            let json = serde_json::json!({
                "injectSteps": [{
                    "systemMessage": {
                        "systemMessage": text.trim_end(),
                        "metadata": {"source": "hushrun"},
                    }
                }]
            });
            format!("{json}\n")
        }
        (Format::Antigravity, None) => "{}\n".to_string(),
        (_, Some(text)) => text.to_string(),
        (_, None) => String::new(),
    }
}

/// Whether the block is out: the watchdog and the command print it once between them.
static SAID: AtomicBool = AtomicBool::new(false);

/// Prints `text` on stdout unless it is out already; whether this call printed it.
fn say(text: &str) -> bool {
    if SAID.swap(true, Ordering::SeqCst) {
        return false;
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
    true
}

/// \[ADR 0140\] The hosted agent's sandbox this runs in, by C-140's table: a platform row only.
fn hosted() -> Option<&'static Platform> {
    hushrun_ci::sandbox(&hushrun_ci::Env::from_process())
        .filter(|platform| platform.kind == Kind::Platform)
}

/// Runs `step` as a child of this hushrun until `until`; past it, the child goes on. The child
/// holds `lock` until it ends, wherever this command is by then.
fn spawn_step(
    global: &GlobalOpts,
    log: Option<&Path>,
    lock: Option<&File>,
    step: Step,
    until: Instant,
) -> Outcome {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return Outcome::NotStarted(err.to_string()),
    };
    let mut cmd = Command::new(exe);
    if let Some(repo) = &global.repo {
        cmd.arg("-C").arg(repo);
    }
    cmd.args(step.args());
    hold(&mut cmd, lock);
    let output = |cmd: &mut Command| -> std::io::Result<()> {
        let Some(log) = log else {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
            return Ok(());
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)?;
        let _ = writeln!(file, "== hushrun {}", step.args().join(" "));
        cmd.stdout(file.try_clone()?).stderr(file);
        Ok(())
    };
    if output(&mut cmd).is_err() {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    // A step left running outlives this command and the hook's own process group.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => return Outcome::NotStarted(err.to_string()),
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Outcome::of_exit(status.code()),
            Ok(None) => {}
            Err(err) => return Outcome::NotStarted(err.to_string()),
        }
        let now = Instant::now();
        if now >= until {
            return Outcome::StillRunning;
        }
        std::thread::sleep(POLL.min(until - now));
    }
}

/// \[ADR 0165\] Gives the child `lock` as its stdin, so it holds the lock as long as it runs: a
/// lock taken with `flock` belongs to the open file, which the child's copy keeps open after
/// this command ends. Without a lock, stdin is null. The steps never read stdin.
fn hold(cmd: &mut Command, lock: Option<&File>) {
    match lock.map(File::try_clone) {
        Some(Ok(file)) => cmd.stdin(file),
        _ => cmd.stdin(Stdio::null()),
    };
}

/// \[ADR 0165\] What a start's steps did, for a start soon after: `<cache dir>/session-start.stamp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Stamp {
    /// The repository's root.
    pub root: PathBuf,
    /// HEAD and its tree.
    pub head: String,
    pub tree: String,
    pub steps: Vec<(Step, Outcome)>,
    /// What `ready` said, when it answered in time.
    pub summary: Option<Summary>,
    /// When the steps ended, in seconds since the epoch.
    pub time: u64,
}

impl Stamp {
    /// Whether it stands for a start in `root` at `head` (with `tree`) at `now`: the same commit,
    /// no step left running, and at most [`STAMP_MINUTES`] old.
    pub(crate) fn stands_for(&self, root: &Path, head: &str, tree: &str, now: u64) -> bool {
        self.root == root
            && self.head == head
            && self.tree == tree
            && !self
                .steps
                .iter()
                .any(|(_, outcome)| *outcome == Outcome::StillRunning)
            && now >= self.time
            && now - self.time <= STAMP_MINUTES * 60
    }

    fn read(path: &Path) -> Option<Stamp> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    fn write(&self, path: &Path) {
        let Ok(json) = serde_json::to_vec(self) else {
            return;
        };
        let partial = path.with_extension("stamp.partial");
        if std::fs::write(&partial, json).is_ok() {
            let _ = std::fs::rename(&partial, path);
        }
    }
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// HEAD and its tree, in `root`.
fn head_and_tree(root: &Path) -> Option<(String, String)> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD", "HEAD^{tree}"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines = text.lines();
    Some((lines.next()?.to_string(), lines.next()?.to_string()))
}

/// What a start says when the lock is held: the steps of the start that holds it, as its stamp
/// has them when it left one running, else every step as still running. None starts.
fn held(
    stamp: Option<&Stamp>,
    root: &Path,
    head: Option<&str>,
    credential: bool,
) -> Vec<(Step, Outcome)> {
    if let Some(stamp) = stamp.filter(|stamp| {
        stamp.root == root
            && Some(stamp.head.as_str()) == head
            && stamp
                .steps
                .iter()
                .any(|(_, outcome)| *outcome == Outcome::StillRunning)
    }) {
        return stamp.steps.clone();
    }
    STEPS
        .iter()
        .map(|&step| {
            let outcome = if step.needs_credential() && !credential {
                Outcome::Skipped(NO_CREDENTIAL)
            } else {
                Outcome::StillRunning
            };
            (step, outcome)
        })
        .collect()
}

/// `hushrun hook session-start`: always [`Exit::Ok`].
pub fn run(global: &GlobalOpts, args: &SessionStartArgs) -> Exit {
    let start = Instant::now();
    // Read even where it does nothing, so the hook's writer never waits on a full pipe.
    let input = hook_input::read();
    let format = global.format;
    let Some(platform) = hosted() else {
        tracing::debug!("session-start: not a hosted agent's sandbox, so nothing to do");
        say(&render(format, None));
        return Exit::Ok;
    };
    let budget = Budget::new(start, Duration::from_secs(args.budget));
    let quiet = global.quiet > 0;
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hook_input::enter_workspace(global, &input);
        session_start(global, platform, budget, quiet, format);
    }));
    if caught.is_err() {
        tracing::debug!("session-start: stopped short");
    }
    // Nothing said: a hook that parses stdout still gets what it parses.
    say(&render(format, None));
    Exit::Ok
}

fn session_start(
    global: &GlobalOpts,
    platform: &'static Platform,
    budget: Budget,
    quiet: bool,
    format: Format,
) {
    let root = crate::cmd::login::repository_root(global);
    let Some(root) = root else {
        tracing::debug!("session-start: not in a repository, so nothing to do");
        return;
    };
    let sharing = sharing(Some(&root));
    let credential = sharing == Sharing::Shared;
    let cache = crate::context::cache_dir().filter(|dir| std::fs::create_dir_all(dir).is_ok());
    let stamp_path = cache.as_ref().map(|dir| dir.join("session-start.stamp"));
    let stamp = stamp_path.as_deref().and_then(Stamp::read);
    let commit = head_and_tree(&root);
    let lock = cache.as_ref().and_then(|dir| {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("session-start.lock"))
            .ok()
    });
    // Taken, or there is no lock to take: this start runs the steps. Never unlocked by hand: an
    // unlock would free it for a step left running too. It goes when the last copy closes.
    let locked = lock.as_ref().is_none_or(|file| file.try_lock().is_ok());
    let note = |line: &str| {
        if !quiet {
            eprintln!("hushrun: session-start: {line}");
        }
    };
    let now = epoch_seconds();
    let standing = locked
        .then_some(())
        .and(stamp.as_ref())
        .zip(commit.as_ref())
        .filter(|(stamp, (head, tree))| stamp.stands_for(&root, head, tree, now));
    let (steps, kept) = if let Some((stamp, _)) = standing {
        note(&format!(
            "the steps ran {}s ago on this commit, so they are skipped",
            now - stamp.time
        ));
        (stamp.steps.clone(), Some(stamp.summary.clone()))
    } else if !locked {
        note("another start's steps, or one it left running, hold the lock, so none starts");
        (
            held(
                stamp.as_ref(),
                &root,
                commit.as_ref().map(|(head, _)| head.as_str()),
                credential,
            ),
            None,
        )
    } else {
        let log = cache.as_ref().map(|dir| dir.join("session-start.log"));
        if let Some(log) = &log {
            let _ = std::fs::write(log, "");
        }
        let steps = run_steps(
            STEPS,
            budget,
            credential,
            &Instant::now,
            &mut |step, until| {
                let began = Instant::now();
                let outcome = spawn_step(global, log.as_deref(), lock.as_ref(), step, until);
                let shown = log
                    .as_ref()
                    .filter(|_| !matches!(outcome, Outcome::Done | Outcome::NotPublished))
                    .map(|log| format!(" ({})", log.display()))
                    .unwrap_or_default();
                note(&format!(
                    "{} {} in {:.1}s{shown}",
                    step.name(),
                    outcome.said(),
                    began.elapsed().as_secs_f64()
                ));
                outcome
            },
        );
        (steps, None)
    };
    let skipped = kept.is_some();
    // What the steps did, for a start soon after; what `ready` says joins it below. Then the lock
    // goes, but for a step left running, which holds its own copy.
    let mut stamp = (locked && !skipped)
        .then_some(())
        .zip(commit)
        .map(|((), (head, tree))| Stamp {
            root: root.clone(),
            head,
            tree,
            steps: steps.clone(),
            summary: None,
            time: epoch_seconds(),
        });
    if let (Some(stamp), Some(path)) = (&stamp, &stamp_path) {
        stamp.write(path);
    }
    drop(lock);
    // What it says when `ready` doesn't answer in time: printed by the watchdog, which ends
    // the command.
    let reduced = render(format, Some(&block(platform.name, &steps, &sharing, None)));
    let end = budget.end;
    std::thread::spawn(move || {
        std::thread::sleep(end.saturating_duration_since(Instant::now()));
        if say(&reduced) {
            std::process::exit(Exit::Ok as i32);
        }
    });
    let summary = match kept {
        Some(Some(summary)) => Some(summary),
        _ => {
            let target = Target {
                snapshot: Snapshot::Rev("HEAD".into()),
                flag: None,
                branch: None,
                scopes: None,
            };
            match ready::evaluate(global, &target) {
                Ok(report) => Some(Summary::of(&report, crate::results::lookups_failed())),
                Err(failure) => {
                    tracing::debug!("session-start: can't tell what passed: {:#}", failure.error);
                    None
                }
            }
        }
    };
    if let (Some(stamp), Some(path)) = (&mut stamp, &stamp_path)
        && summary.is_some()
    {
        stamp.summary = summary.clone();
        stamp.write(path);
    }
    say(&render(
        format,
        Some(&block(platform.name, &steps, &sharing, summary.as_ref())),
    ));
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    // covers: C-168
    #[test]
    fn the_budget_keeps_a_third_for_the_context_and_at_most_15_seconds() {
        let start = Instant::now();
        let b = Budget::new(start, Duration::from_secs(45));
        assert_eq!(b.end - start, Duration::from_secs(45));
        assert_eq!(b.steps_end - start, Duration::from_secs(30));
        let b = Budget::new(start, Duration::from_secs(120));
        assert_eq!(b.steps_end - start, Duration::from_secs(105));
        let b = Budget::new(start, Duration::from_secs(3));
        assert_eq!(b.steps_end - start, Duration::from_secs(2));
    }

    // covers: C-168
    #[test]
    fn steps_run_in_order_and_fetch_needs_a_credential() {
        let start = Instant::now();
        let budget = Budget::new(start, Duration::from_secs(45));
        let mut ran = Vec::new();
        let done = run_steps(STEPS, budget, false, &|| start, &mut |step, until| {
            assert_eq!(until, budget.steps_end);
            ran.push(step);
            Outcome::Done
        });
        assert_eq!(ran, [Step::BuildPull, Step::PackagesPull]);
        assert_eq!(
            done,
            [
                (Step::BuildPull, Outcome::Done),
                (Step::PackagesPull, Outcome::Done),
                (Step::Fetch, Outcome::Skipped(NO_CREDENTIAL)),
            ]
        );
        let mut ran = Vec::new();
        let done = run_steps(STEPS, budget, true, &|| start, &mut |step, _| {
            ran.push(step);
            Outcome::Failed(Some(2))
        });
        // A step that fails doesn't stop the next.
        assert_eq!(ran, [Step::BuildPull, Step::PackagesPull, Step::Fetch]);
        assert_eq!(Step::PackagesPull.args(), ["packages", "pull", "-q"]);
        assert!(done.iter().all(|(_, o)| *o == Outcome::Failed(Some(2))));
    }

    // covers: C-168
    #[test]
    fn a_step_the_budget_cuts_off_goes_on_and_the_rest_are_skipped() {
        let start = Instant::now();
        let budget = Budget::new(start, Duration::from_secs(45));
        let mut ran = Vec::new();
        let done = run_steps(STEPS, budget, true, &|| start, &mut |step, _| {
            ran.push(step);
            Outcome::StillRunning
        });
        assert_eq!(ran, [Step::BuildPull]);
        assert_eq!(done[1], (Step::PackagesPull, Outcome::Skipped(NO_TIME)));
        assert_eq!(done[2], (Step::Fetch, Outcome::Skipped(NO_TIME)));
        // With less than a second of the steps' time left, none starts.
        let clock = Cell::new(budget.steps_end - Duration::from_millis(500));
        let done = run_steps(STEPS, budget, true, &|| clock.get(), &mut |_, _| {
            panic!("no step starts")
        });
        assert!(done.iter().all(|(_, o)| *o == Outcome::Skipped(NO_TIME)));
        // Time spent by the first leaves the second none.
        let clock = Cell::new(start);
        let done = run_steps(STEPS, budget, true, &|| clock.get(), &mut |_, until| {
            clock.set(until);
            Outcome::Done
        });
        assert_eq!(
            done,
            [
                (Step::BuildPull, Outcome::Done),
                (Step::PackagesPull, Outcome::Skipped(NO_TIME)),
                (Step::Fetch, Outcome::Skipped(NO_TIME)),
            ]
        );
    }

    fn summary() -> Summary {
        Summary {
            code: "abc1234".into(),
            passed: strings(&["lint", "build", "types"]),
            missing: strings(&["unit"]),
            ci: strings(&["e2e"]),
            here: false,
            lookups_failed: false,
        }
    }

    // covers: C-168
    #[test]
    fn the_block_says_what_passed_what_is_left_and_what_to_do_before_a_push() {
        let steps = [
            (Step::BuildPull, Outcome::Done),
            (Step::PackagesPull, Outcome::Done),
            (Step::Fetch, Outcome::Done),
        ];
        let text = block(
            "Claude Code on the web",
            &steps,
            &Sharing::Shared,
            Some(&summary()),
        );
        assert_eq!(
            text,
            "hushrun, at the start of this Claude Code on the web session:
- Setup: build pull done; packages pull done; fetch done.
- 3 of 5 checks already passed on this exact code (abc1234): hushrun run skips them here, and CI skips them.
- Still to run here: unit.
- Left to CI or another machine: e2e.
- Before you push: commit, run the commands `hushrun ready` lists, and push once it says the code is ready.
"
        );
    }

    // covers: C-168
    #[test]
    fn a_pull_with_nothing_published_is_said_as_such_and_not_as_a_failure() {
        assert_eq!(Outcome::of_exit(Some(0)), Outcome::Done);
        assert_eq!(Outcome::of_exit(Some(12)), Outcome::NotPublished);
        assert_eq!(Outcome::of_exit(Some(1)), Outcome::Failed(Some(1)));
        assert_eq!(Outcome::of_exit(None), Outcome::Failed(None));
        let steps = [
            (Step::BuildPull, Outcome::Done),
            (Step::PackagesPull, Outcome::NotPublished),
            (Step::Fetch, Outcome::Done),
        ];
        let text = block(
            "Claude Code on the web",
            &steps,
            &Sharing::Shared,
            Some(&summary()),
        );
        assert!(
            text.contains(
                "- Setup: build pull done; packages pull none published yet; fetch done."
            ),
            "{text}"
        );
        assert!(!text.contains("failed"), "{text}");
    }

    // covers: C-168
    #[test]
    fn the_block_says_when_results_stay_here_and_points_at_doctor() {
        let steps = [
            (Step::BuildPull, Outcome::Failed(Some(1))),
            (Step::Fetch, Outcome::Skipped(NO_CREDENTIAL)),
        ];
        let mut here = summary();
        here.here = true;
        let text = block("Codex cloud", &steps, &Sharing::NoCredential, Some(&here));
        assert!(
            text.contains("- Setup: build pull failed (exit 1); fetch skipped (no credential for the backend)."),
            "{text}"
        );
        assert!(text.contains("and CI still runs them"), "{text}");
        assert!(text.contains("Results stay on this machine"), "{text}");
        assert!(text.contains("`hushrun doctor`"), "{text}");
        let mut refused = summary();
        refused.lookups_failed = true;
        let text = block("Codex cloud", &steps, &Sharing::Shared, Some(&refused));
        assert!(text.contains("refused or didn't answer"), "{text}");
        assert!(text.contains("`hushrun doctor`"), "{text}");
    }

    // covers: C-168
    #[test]
    fn the_block_is_short_names_no_secret_and_has_a_reduced_form() {
        let many: Vec<String> = (0..40).map(|n| format!("check-{n}")).collect();
        let full = Summary {
            code: "abc1234".into(),
            passed: many.clone(),
            missing: many.clone(),
            ci: many,
            here: true,
            lookups_failed: true,
        };
        let steps = [
            (Step::BuildPull, Outcome::StillRunning),
            (Step::PackagesPull, Outcome::Skipped(NO_TIME)),
            (Step::Fetch, Outcome::Skipped(NO_TIME)),
        ];
        for sharing in [Sharing::Shared, Sharing::NoCredential, Sharing::Off] {
            for summary in [Some(&full), None] {
                let text = block("Claude Code on the web", &steps, &sharing, summary);
                assert!(text.lines().count() <= 10, "{text}");
                assert!(text.ends_with(&format!("{BEFORE_PUSH}\n")), "{text}");
                for secret in ["hra_", "hrs_", "hru_", "HUSHRUN_AGENT=", "Bearer"] {
                    assert!(!text.contains(secret), "{secret} in {text}");
                }
            }
        }
        let text = block(
            "Claude Code on the web",
            &steps,
            &Sharing::Shared,
            Some(&full),
        );
        assert!(text.contains("check-5 and 34 more"), "{text}");
        let reduced = block("Claude Code on the web", &steps, &Sharing::Shared, None);
        assert!(reduced.contains(NO_ANSWER), "{reduced}");
        assert!(
            reduced.contains(
                "build pull still running in the background; packages pull skipped (out of time); fetch skipped (out of time)"
            ),
            "{reduced}"
        );
        let none = Summary {
            code: "abc1234".into(),
            ..Summary::default()
        };
        let text = block("Claude Code on the web", &[], &Sharing::Shared, Some(&none));
        assert!(text.contains("knows no checks for abc1234"), "{text}");
        assert!(!text.contains("Setup:"), "{text}");
    }

    /// The one system message of Antigravity's JSON, checked for shape.
    fn system_message(json: &str) -> String {
        assert!(
            json.ends_with('\n') && json.matches('\n').count() == 1,
            "{json}"
        );
        let value: serde_json::Value = serde_json::from_str(json).expect("JSON");
        let steps = value["injectSteps"].as_array().expect("injectSteps");
        assert_eq!(steps.len(), 1, "{json}");
        assert_eq!(steps[0]["systemMessage"]["metadata"]["source"], "hushrun");
        assert!(value.get("decision").is_none(), "{json}");
        let text = steps[0]["systemMessage"]["systemMessage"]
            .as_str()
            .expect("text")
            .to_string();
        assert!(text.lines().count() <= 10, "{text}");
        text
    }

    // covers: C-168
    #[test]
    fn antigravitys_json_holds_the_block_as_one_system_message() {
        let steps = [
            (Step::BuildPull, Outcome::Done),
            (Step::PackagesPull, Outcome::NotPublished),
            (Step::Fetch, Outcome::Done),
        ];
        let text = block("Antigravity", &steps, &Sharing::Shared, Some(&summary()));
        let json = render(Format::Antigravity, Some(&text));
        assert_eq!(system_message(&json), text.trim_end());
        assert!(
            system_message(&json).contains("packages pull none published yet"),
            "{json}"
        );
        // A check's name can't break it.
        let mut odd = summary();
        odd.missing = strings(&["say \"hi\"\n\\ there"]);
        let text = block("Antigravity", &steps, &Sharing::Shared, Some(&odd));
        assert_eq!(
            system_message(&render(Format::Antigravity, Some(&text))),
            text.trim_end()
        );
        // Nothing to say is `{}`, and plain text stays as it was.
        assert_eq!(render(Format::Antigravity, None), "{}\n");
        assert_eq!(render(Format::Text, None), "");
        assert_eq!(render(Format::Text, Some(&text)), text);
    }

    // covers: C-168
    #[test]
    fn the_watchdogs_reduced_block_is_json_too() {
        let steps = [
            (Step::BuildPull, Outcome::Done),
            (Step::PackagesPull, Outcome::StillRunning),
            (Step::Fetch, Outcome::Skipped(NO_TIME)),
        ];
        let reduced = block("Antigravity", &steps, &Sharing::Shared, None);
        let text = system_message(&render(Format::Antigravity, Some(&reduced)));
        assert!(text.contains(NO_ANSWER), "{text}");
        assert!(
            text.contains(
                "packages pull still running in the background; fetch skipped (out of time)"
            ),
            "{text}"
        );
    }

    fn stamp() -> Stamp {
        Stamp {
            root: PathBuf::from("/repo"),
            head: "h".into(),
            tree: "t".into(),
            steps: vec![
                (Step::BuildPull, Outcome::Done),
                (Step::PackagesPull, Outcome::NotPublished),
                (Step::Fetch, Outcome::Failed(Some(3))),
            ],
            summary: Some(summary()),
            time: 1_000,
        }
    }

    // covers: C-168
    #[test]
    fn a_stamp_stands_for_the_same_commit_for_ten_minutes() {
        let root = Path::new("/repo");
        let s = stamp();
        assert!(s.stands_for(root, "h", "t", 1_000));
        assert!(s.stands_for(root, "h", "t", 1_000 + 600));
        assert!(!s.stands_for(root, "h", "t", 1_000 + 601));
        assert!(!s.stands_for(root, "h", "t", 999), "a clock that went back");
        assert!(!s.stands_for(root, "other", "t", 1_000));
        assert!(!s.stands_for(root, "h", "other", 1_000));
        assert!(!s.stands_for(Path::new("/elsewhere"), "h", "t", 1_000));
        // A step left running may have ended since: the steps run again.
        let mut running = stamp();
        running.steps[1].1 = Outcome::StillRunning;
        assert!(!running.stands_for(root, "h", "t", 1_000));
        // It reads back as it was written, outcomes and all.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session-start.stamp");
        s.write(&path);
        assert_eq!(Stamp::read(&path), Some(s));
        std::fs::write(&path, "{").expect("write");
        assert_eq!(Stamp::read(&path), None);
    }

    // covers: C-168
    #[test]
    fn a_held_lock_starts_no_step_and_says_what_holds_it() {
        let root = Path::new("/repo");
        let all_running = held(None, root, Some("h"), true);
        assert!(
            all_running
                .iter()
                .all(|(_, outcome)| *outcome == Outcome::StillRunning),
            "{all_running:?}"
        );
        let no_credential = held(None, root, Some("h"), false);
        assert_eq!(
            no_credential[2],
            (Step::Fetch, Outcome::Skipped(NO_CREDENTIAL))
        );
        let mut left = stamp();
        left.steps[1].1 = Outcome::StillRunning;
        assert_eq!(held(Some(&left), root, Some("h"), true), left.steps);
        // Another commit's stamp says nothing of this one.
        assert_eq!(held(Some(&left), root, Some("x"), true), all_running);
    }

    // covers: C-168
    #[cfg(unix)]
    #[test]
    fn a_step_holds_the_lock_until_it_ends() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session-start.lock");
        let open = || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .expect("lock")
        };
        let lock = open();
        lock.try_lock().expect("free");
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 1"]);
        hold(&mut cmd, Some(&lock));
        let mut child = cmd.spawn().expect("sh");
        // This command's copies go, as when the hook ends; the step's stays.
        drop(cmd);
        drop(lock);
        assert!(open().try_lock().is_err(), "the step holds it");
        child.wait().expect("wait");
        assert!(open().try_lock().is_ok(), "free once the step ends");
    }

    // covers: C-168
    #[test]
    fn names_are_listed_then_counted() {
        assert_eq!(names(&strings(&["a"])), "a");
        assert_eq!(names(&strings(&["a", "b"])), "a and b");
        assert_eq!(names(&strings(&["a", "b", "c"])), "a, b and c");
        let seven = strings(&["a", "b", "c", "d", "e", "f", "g"]);
        assert_eq!(names(&seven), "a, b, c, d, e, f and 1 more");
    }
}
