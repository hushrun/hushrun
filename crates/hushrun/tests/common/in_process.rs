//! In-process adapter tests: a test calls one adapter's own code, on its own type, and the code
//! every adapter shares (the [`Observer`]'s steps and the unit store), and never starts the
//! `hushrun` binary. The static map of what each test reaches then reaches that adapter alone
//! (CONTRIBUTING.md, "Which tests CI runs again").
//!
//! A test file includes this module (`#[path = "common/in_process.rs"] mod in_process;`), with
//! `common/repo.rs` as `repo` and `common/store.rs` as `store`, and never `common/cli.rs`. It
//! names its adapter by its own type (`gradle::Recognized`), and runs a check with a function of
//! its own (`observe` in `units_gradle.rs`, which it copies): the steps of [`Run`] and
//! [`Observer`], and between them each call `hushrun run` makes of the adapter, in order, on that
//! value. It is a function, not a macro: the static map resolves each call in a function to its
//! own item, and only by its name in a macro's body, where every adapter's `read` is a `read`.
//! What a test must never call: `units::adapter::choose`, `choose_kept`, `RECOGNIZED`, anything
//! that takes a `Box<dyn Adapter>` or `&dyn Adapter` (`units::Session`, `units::why`), a function
//! generic over `Adapter`, the CLI (`cmd::…`, `hushrun::cli`), or the binary: each reaches every
//! adapter, or the whole crate.
//!
//! What a run here does, as `hushrun run CHECK -- COMMAND` does with a local backend (C-30 to
//! C-34): the plan, in `reuse`; COMMAND, started once, or twice when the adapter left out what the
//! plan didn't list; the facts, decided; an audited unit that failed, run once more and, failing
//! again, suspending the check's reuse; the units' results recorded in this machine's results
//! directory; and the map kept. What only the CLI does stays with the end-to-end tests
//! (`tests/e2e_*.rs`): its lines' `hushrun: ` and their order with COMMAND's output, the run's
//! last line and exit code, the check's own result and skip, the target runner, hooks, CI
//! detection and `hushrun why`.
//!
//! # Process-wide state
//!
//! Nothing here changes what the test process shares: no environment variable is set, the
//! current directory stays, and no signal handler is installed. COMMAND gets its environment on
//! its own [`Command`]; the observer gets hushrun's cache directory as an argument; the adapter
//! reads COMMAND's environment through [`Run::env`], not this process's; the scope's files are
//! read with the cache directory given. So these tests are as safe as threads under `cargo test`
//! as they are one per process under `cargo nextest`, which CI runs. An adapter's own code may
//! still read this process's environment where it reads nothing else (`HOME`, `PATH`): a test
//! that needs one of those different sets it on COMMAND, which is where the adapter's tools look.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use hushrun::results::{RESULT_SCHEMA, RunResult, Verdict};
use hushrun::units::store::{Local, Map, UnitResults};
use hushrun::units::{Finish, Observed, Observer, PlanInput, Planned, Profile};
use hushrun_core::digest::Digest;
use hushrun_core::time::Time;
use hushrun_core::units::{KeyBase, Mode, State, Tests, Tree};
use serde_json::Value;

pub use super::repo::Repo;

/// What `hushrun run` sets on every start of COMMAND (`cmd::wrap`): that it runs inside a run.
const INSIDE_A_RUN: &str = "HUSHRUN_INSIDE_RUN";

/// What a run came to: COMMAND's exit code and output, and what hushrun's steps said and
/// counted.
#[derive(Debug)]
pub struct Ran {
    /// COMMAND's exit code, of its first start (or of its start with nothing left out).
    pub code: i32,
    /// COMMAND's output, every start's.
    pub stdout: String,
    pub stderr: String,
    /// What the run's steps said, each a line `hushrun run` writes after `hushrun: `, in order:
    /// the observer's notes, the plan's line, and after COMMAND what its facts came to.
    pub lines: Vec<String>,
    /// The units' counts the check's result keeps (`units` in it): `null` when the units
    /// weren't read.
    pub units: Value,
}

impl Ran {
    /// The line that starts `start`; it panics, showing every line and COMMAND's output, when
    /// there is none.
    pub fn line(&self, start: &str) -> &str {
        self.lines
            .iter()
            .find(|l| l.starts_with(start))
            .unwrap_or_else(|| {
                panic!(
                    "no line starting {start:?} in:\n{}\n{}\n{}",
                    self.lines.join("\n"),
                    self.stdout,
                    self.stderr
                )
            })
    }

    /// How many lines start `start`.
    pub fn count(&self, start: &str) -> usize {
        self.lines.iter().filter(|l| l.starts_with(start)).count()
    }

    /// Every line, COMMAND's output after them, for an assertion's message.
    pub fn said(&self) -> String {
        format!(
            "{}\n--- stdout\n{}\n--- stderr\n{}",
            self.lines.join("\n"),
            self.stdout,
            self.stderr
        )
    }
}

/// One run of a check, as a test's own function makes it: what it goes by besides its observer,
/// and what it said.
pub struct Run<'r> {
    pub repo: &'r Repo,
    pub check: hushrun_core::CheckName,
    pub tests: Option<Tests>,
    pub repository: Option<String>,
    pub command: Vec<String>,
    /// The variables the test gives this run's COMMAND.
    env: Vec<(String, String)>,
    pub base: KeyBase,
    scope: hushrun_core::names::ScopeName,
    digest: Digest,
    pub tree: Tree,
    pub store: Local,
    started: Time,
    started_text: String,
    run_key: Digest,
    /// How long COMMAND's last start took.
    duration_ms: u64,
    pub lines: Vec<String>,
    stdout: String,
    stderr: String,
}

impl<'r> Run<'r> {
    /// A run of `check` in `repo`'s root: `args` is COMMAND, whose environment gains `env`.
    pub fn new(repo: &'r Repo, check: &str, args: &[&str], env: &[(&str, &str)]) -> Run<'r> {
        let policy = repo.policy();
        let check: hushrun_core::CheckName = check.parse().expect("a check's name");
        let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let (scope, digest, tree) = repo.scope(&policy);
        let cwd = String::new();
        let (run_key, rule) =
            hushrun::results::run_key(Some(&policy), &check, &command, &cwd, &scope, &digest, &[])
                .expect("the run's key");
        let base = KeyBase {
            check: check.clone(),
            command: command.clone(),
            cwd,
            scope: scope.clone(),
            external: None,
            counts_from: rule.as_ref().map(|rule| rule.normal()),
        };
        let repository = Some(policy.repository.clone()).filter(|r| !r.is_empty());
        let store = Local::with_reuse(
            &repo.cache.join("results"),
            hushrun::units::store::reuse_file(&repo.cache, repository.as_deref(), check.as_str()),
        );
        let started = Time::from_timestamp(jiff::Timestamp::now());
        Run {
            repo,
            tests: policy.tests.get(&check).cloned(),
            check,
            repository,
            command,
            env: env
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            base,
            scope,
            digest,
            tree,
            store,
            started_text: started.to_string(),
            started,
            run_key,
            duration_ms: 0,
            lines: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// The repository's root, where COMMAND runs.
    pub fn cwd(&self) -> &Path {
        &self.repo.repo
    }

    /// COMMAND, without a start: the process [`Repo::command`] makes, with its arguments and the
    /// test's variables.
    fn process(&self) -> Command {
        let mut cmd = self.repo.command(&self.command[0]);
        cmd.args(&self.command[1..]);
        for (name, value) in &self.env {
            cmd.env(name, value);
        }
        cmd
    }

    /// COMMAND's environment as an adapter reads it ([`Observer::start_at`]): what COMMAND is
    /// given, else this process's; never what COMMAND has removed.
    pub fn env(&self) -> impl Fn(&str) -> Option<OsString> + use<'_, 'r> {
        let process = self.process();
        let given: Vec<(OsString, Option<OsString>)> = process
            .get_envs()
            .map(|(n, v)| (n.to_os_string(), v.map(|v| v.to_os_string())))
            .collect();
        let assigned = hushrun::units::command::assignments(&self.command);
        move |name: &str| {
            if let Some((_, value)) = assigned.iter().rev().find(|(n, _)| n == name) {
                return Some(OsString::from(value));
            }
            match given.iter().rev().find(|(n, _)| n == name) {
                Some((_, value)) => value.clone(),
                None => std::env::var_os(name),
            }
        }
    }

    /// The observer, as [`units::Session::start`](hushrun::units::Session::start) opens it, for
    /// the adapter `profile` describes: `None` when the check's `tests` are off.
    pub fn open(&mut self, profile: Profile) -> Option<Observer> {
        let tests = self.tests.clone().filter(Tests::is_on)?;
        let observer = Observer::open(
            &tests,
            profile,
            &self.repo.cache,
            &self.command,
            &self.repo.repo,
            &self.repo.repo,
            false,
        )
        .expect("the run's directory");
        for note in observer.notes() {
            self.lines.push(format!("{}: {note}", self.check));
        }
        Some(observer)
    }

    /// The adapter's variables, kept: it panics when the adapter can't load, which the test
    /// asserts with [`Run::unobserved`] instead.
    pub fn started(
        &mut self,
        observer: &mut Observer,
        variables: Result<Vec<(String, OsString)>, String>,
    ) {
        observer.started(variables).expect("the adapter loads");
        // \[C-46\] Before COMMAND, in every mode, what the store knows.
        observer.learned(&self.base, self.repository.as_deref());
    }

    /// \[C-32\] The run of a COMMAND no adapter observes, and why: COMMAND as it is.
    pub fn unobserved(mut self, why: &str) -> Ran {
        self.lines
            .push(format!("{}: test files aren't observed: {why}", self.check));
        self.plain()
    }

    /// The run of a check whose `tests` are off: COMMAND as it is, and nothing said.
    pub fn plain(mut self) -> Ran {
        let mut cmd = self.process();
        cmd.env(INSIDE_A_RUN, "1");
        let code = self.start(&mut cmd);
        self.ran(code, None)
    }

    /// What [`Observer::plan_context`] takes: `None` unless the check is in `reuse`.
    pub fn plan_input(&self) -> Option<PlanInput<'_>> {
        let tests = self.tests.as_ref()?;
        (tests.mode == Mode::Reuse).then(|| PlanInput {
            base: self.base.clone(),
            repository: self.repository.as_deref(),
            tree: &self.tree,
            store: &self.store,
            counts: &counts,
            root: &self.repo.repo,
            cwd: &self.repo.repo,
            started: &self.started_text,
        })
    }

    /// What the plan came to, said as `hushrun run` says it.
    pub fn planned(&mut self, observer: &Observer, planned: Planned) {
        match planned {
            Planned::Plan(line) => {
                for note in observer.plan_notes() {
                    self.lines.push(note.clone());
                }
                if !line.is_empty() {
                    self.lines.push(line);
                }
            }
            Planned::Everything(why) => {
                let noun = observer.test_file();
                self.lines
                    .push(format!("{}: every {noun} runs: {why}", self.check));
            }
        }
    }

    /// Starts COMMAND with the observer's variables and `start`, what
    /// [`Adapter::start_variables`](hushrun::units::adapter::Adapter::start_variables) made:
    /// whether it passed.
    pub fn start_command(
        &mut self,
        observer: &Observer,
        start: &[(String, Option<OsString>)],
    ) -> (i32, bool) {
        let mut cmd = self.process();
        cmd.env(INSIDE_A_RUN, "1");
        observer.apply(&mut cmd, start);
        let code = self.start(&mut cmd);
        (code, code == 0)
    }

    /// The audit's second start of COMMAND, `again` ([`Observer::again`]), over the first's
    /// variables and `start`: whether it passed.
    pub fn start_again(
        &mut self,
        observer: &Observer,
        start: &[(String, Option<OsString>)],
        again: &hushrun::units::Again,
    ) -> bool {
        let mut cmd = self.process();
        cmd.env(INSIDE_A_RUN, "1");
        observer.apply(&mut cmd, start);
        again.apply(&mut cmd);
        self.start(&mut cmd) == 0
    }

    fn start(&mut self, cmd: &mut Command) -> i32 {
        let clock = std::time::Instant::now();
        let output = cmd.output().expect("COMMAND starts");
        self.duration_ms = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.stdout
            .push_str(&String::from_utf8_lossy(&output.stdout));
        self.stderr
            .push_str(&String::from_utf8_lossy(&output.stderr));
        output.status.code().unwrap_or(-1)
    }

    /// What [`Observer::read_context`] takes, once COMMAND ended (`passed`): the scope's files
    /// now must be those it started on, as `hushrun run` requires to observe it.
    pub fn finish_input(&self, passed: bool) -> Finish<'_> {
        let (_, digest, _) = self.repo.scope(&self.repo.policy());
        assert_eq!(
            digest, self.digest,
            "COMMAND changed the scope's files, so hushrun run would observe nothing"
        );
        Finish {
            base: self.base.clone(),
            repository: self.repository.as_deref(),
            tree: &self.tree,
            command_passed: passed,
            counts: &counts,
            store: &self.store,
            root: &self.repo.repo,
            cwd: &self.repo.repo,
            mode: self.tests.as_ref().map_or(Mode::Off, |t| t.mode),
        }
    }

    /// \[C-34\] What the audit's second run said of each unit it ran again (`ended`,
    /// [`Observer::again_ended`]): a pass is a flaky unit; anything else suspends the check's
    /// reuse, as `hushrun run` does.
    pub fn disagreements(&mut self, observed: &mut Observed, ended: BTreeMap<String, State>) {
        for (unit, state) in &ended {
            let Some(key) = observed.again.get(unit).copied() else {
                continue;
            };
            if *state == State::Passed {
                continue;
            }
            observed.disagree(unit, key);
            self.store
                .suspend(unit, &key, &self.run_key, "a person")
                .expect("reuse suspends");
        }
    }

    /// \[C-34\] The audit's second run could not be set up: each unit counts as failed again.
    pub fn not_again(wanted: &BTreeSet<String>) -> BTreeMap<String, State> {
        wanted
            .iter()
            .map(|unit| (unit.clone(), State::NotRun))
            .collect()
    }

    /// The run's end, as `hushrun run` records it with a local backend: what the facts said, the
    /// units' results beside the run's, and the map; or, for a pass on a stale build, the map
    /// alone. `code` is COMMAND's exit code.
    pub fn record(mut self, code: i32, observed: Option<Observed>) -> Ran {
        let Some(observed) = observed else {
            return self.ran(code, None);
        };
        let passed = code == 0;
        let duration_ms = self.duration_ms;
        let now = Time::from_timestamp(jiff::Timestamp::now());
        if observed.left_out.is_some() {
            // Nothing of a run that left out what no plan listed is recorded (C-32).
            observed.keep(&self.run_key, &self.tree, now, duration_ms);
            self.lines.push(format!(
                "{} wasn't recorded: its adapter still left out what the plan didn't list",
                self.check
            ));
            return self.ran(code, None);
        }
        if passed && observed.stale_build.is_some() {
            observed.keep(&self.run_key, &self.tree, now, duration_ms);
            return self.ran(code, None);
        }
        self.lines.extend(observed.lines.iter().cloned());
        let result = self.result(code, duration_ms, observed.counts.clone());
        observed.keep(&self.run_key, &self.tree, now, duration_ms);
        self.store
            .record(&self.run_key, &result, observed.records())
            .expect("the units' results are kept");
        let counts = observed.counts.clone();
        self.ran(code, counts)
    }

    fn result(
        &self,
        code: i32,
        duration_ms: u64,
        units: Option<hushrun::units::Counts>,
    ) -> RunResult {
        RunResult {
            schema: RESULT_SCHEMA.into(),
            check: self.check.clone(),
            result: if code == 0 {
                Verdict::Pass
            } else {
                Verdict::Fail
            },
            exit_code: Some(code),
            command: self.command.clone(),
            cwd: String::new(),
            scope: self.scope.clone(),
            digest: self.digest,
            external: Vec::new(),
            revision: None,
            repository: self.repository.clone(),
            started_at: self.started,
            duration_ms,
            saved_ms: None,
            provider: hushrun_core::run::Provider::Local,
            job: None,
            variant: None,
            run_url: None,
            tool: format!("hushrun {}", env!("CARGO_PKG_VERSION")),
            invoked_by: None,
            environment: None,
            reports: None,
            ran_anyway: None,
            units,
        }
    }

    fn ran(self, code: i32, counts: Option<hushrun::units::Counts>) -> Ran {
        Ran {
            code,
            stdout: self.stdout,
            stderr: self.stderr,
            lines: self.lines,
            units: counts.map_or(Value::Null, |c| serde_json::to_value(c).unwrap()),
        }
    }
}

/// Whether a run's result counts here: every local one does, as `hushrun run` asks it outside
/// CI's jobs and variants.
fn counts(run: &RunResult) -> bool {
    run.counts_in(None, None)
}

/// An empty map, for [`Observer::again_read_at`].
pub fn no_map() -> Map {
    Map::default()
}
