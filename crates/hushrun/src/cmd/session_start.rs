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

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use hushrun_ci::platforms::{Kind, Platform};
use hushrun_vcs::Snapshot;

use crate::cli::{GlobalOpts, SessionStartArgs};
use crate::cmd::ready::{self, State, Target};
use crate::exit::Exit;
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

/// One step, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// What became of a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Done,
    /// It ended with this exit code (or none, for a signal).
    Failed(Option<i32>),
    /// It couldn't start.
    NotStarted(String),
    /// Not run, for this reason.
    Skipped(&'static str),
    /// The budget ran out while it ran: it goes on in the background.
    StillRunning,
}

impl Outcome {
    fn said(&self) -> String {
        match self {
            Outcome::Done => "done".into(),
            Outcome::Failed(Some(code)) => format!("failed (exit {code})"),
            Outcome::Failed(None) => "stopped by a signal".into(),
            Outcome::NotStarted(_) => "couldn't start".into(),
            Outcome::Skipped(why) => format!("skipped ({why})"),
            Outcome::StillRunning => "still running in the background".into(),
        }
    }
}

/// Why a step is skipped without a credential.
const NO_CREDENTIAL: &str = "no credential for the backend";
/// Why a step is skipped when the budget is spent.
const NO_TIME: &str = "out of time";

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

/// Where the steps' output goes: `<cache dir>/session-start.log`, emptied each time.
fn log_path() -> Option<PathBuf> {
    let dir = crate::context::cache_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("session-start.log"))
}

/// Runs `step` as a child of this hushrun until `until`; past it, the child goes on.
fn spawn_step(global: &GlobalOpts, log: Option<&Path>, step: Step, until: Instant) -> Outcome {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return Outcome::NotStarted(err.to_string()),
    };
    let mut cmd = Command::new(exe);
    if let Some(repo) = &global.repo {
        cmd.arg("-C").arg(repo);
    }
    cmd.args(step.args()).stdin(Stdio::null());
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
            Ok(Some(status)) if status.success() => return Outcome::Done,
            Ok(Some(status)) => return Outcome::Failed(status.code()),
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

/// `hushrun hook session-start`: always [`Exit::Ok`].
pub fn run(global: &GlobalOpts, args: &SessionStartArgs) -> Exit {
    let start = Instant::now();
    let Some(platform) = hosted() else {
        tracing::debug!("session-start: not a hosted agent's sandbox, so nothing to do");
        return Exit::Ok;
    };
    let budget = Budget::new(start, Duration::from_secs(args.budget));
    let quiet = global.quiet > 0;
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        session_start(global, platform, budget, quiet);
    }));
    if caught.is_err() {
        tracing::debug!("session-start: stopped short");
    }
    Exit::Ok
}

fn session_start(global: &GlobalOpts, platform: &'static Platform, budget: Budget, quiet: bool) {
    let root = crate::cmd::login::repository_root(global);
    let Some(root) = root else {
        tracing::debug!("session-start: not in a repository, so nothing to do");
        return;
    };
    let sharing = sharing(Some(&root));
    let log = log_path();
    if let Some(log) = &log {
        let _ = std::fs::write(log, "");
    }
    let steps = run_steps(
        STEPS,
        budget,
        sharing == Sharing::Shared,
        &Instant::now,
        &mut |step, until| {
            let began = Instant::now();
            let outcome = spawn_step(global, log.as_deref(), step, until);
            if !quiet {
                let shown = log
                    .as_ref()
                    .filter(|_| !matches!(outcome, Outcome::Done))
                    .map(|log| format!(" ({})", log.display()))
                    .unwrap_or_default();
                eprintln!(
                    "hushrun: session-start: {} {} in {:.1}s{shown}",
                    step.name(),
                    outcome.said(),
                    began.elapsed().as_secs_f64()
                );
            }
            outcome
        },
    );
    // What it says when `ready` doesn't answer in time: printed by the watchdog, which ends
    // the command.
    let reduced = block(platform.name, &steps, &sharing, None);
    let end = budget.end;
    std::thread::spawn(move || {
        std::thread::sleep(end.saturating_duration_since(Instant::now()));
        if say(&reduced) {
            std::process::exit(Exit::Ok as i32);
        }
    });
    let target = Target {
        snapshot: Snapshot::Rev("HEAD".into()),
        flag: None,
        branch: None,
        scopes: None,
    };
    let summary = match ready::evaluate(global, &target) {
        Ok(report) => Some(Summary::of(&report, crate::results::lookups_failed())),
        Err(failure) => {
            tracing::debug!("session-start: can't tell what passed: {:#}", failure.error);
            None
        }
    };
    say(&block(platform.name, &steps, &sharing, summary.as_ref()));
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
