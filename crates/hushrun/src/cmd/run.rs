//! `hushrun run`: runs checks and records the results (§10.4).
//!
//! hushrun.yaml's checks, named, with no command after `--` (that one is `cmd::wrap`):
//! - with a plan and no names: exactly the plan's `checks_to_run`, whatever evidence exists;
//! - with a plan and names: those, each in the plan's `checks_to_run`;
//! - without a plan, or with `--all`: the named checks, or every check hushrun.yaml requires
//!   ([`hushrun_core::policy::Policy::required`]), skipping the satisfied ones unless `--force`
//!   is given.
//!
//! \[M8\] With `--isolated`, each check runs in its pinned image on an export of exactly the
//! digested entries (E-12), and its evidence claims custody `isolated`.
//!
//! \[ADR 0032\] A check event that hushrun's server refuses for the org's plan isn't recorded:
//! the run says so once, marks the check `not recorded`, and goes on, exiting as it would have.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use serde_json::json;

use hushrun_build::session::Session;
use hushrun_core::event::{
    CheckRecord, Environment, EventKind, EvidenceKind, Outcome, VerifiedEvent, pinned_digest,
};
use hushrun_core::names::CheckName;
use hushrun_core::plan::{Status, check_status};
use hushrun_core::policy::{CheckDef, Shell};
use hushrun_core::run::Custody;
use hushrun_core::scope::Scope;
use hushrun_core::time::{Dur, Time};
use hushrun_digest::Entry;
use hushrun_vcs::{Snapshot, digest_scopes};

use crate::app::{App, ledger_failure};
use crate::cli::RunArgs;
use crate::cmd::build_session::{self, system_time};
use crate::cmd::common::base_event;
use crate::cmd::wrap;
use crate::context::{Content, Source, Subject};
use crate::events::{self, Recorded, Unrecorded};
use crate::exit::{CmdResult, Exit, Failure};
use crate::isolated::{self, Export, Isolation, Runtime};
use crate::lifecycle::Recorder;
use crate::output::{self, Format};
use crate::planfile::PlanFile;
use crate::results::Ended;

/// What happened to one check.
struct Ran {
    check: CheckName,
    result: &'static str,
    detail: String,
    exit_code: Option<i32>,
    attempts: u32,
    duration_ms: u64,
    event: Option<String>,
    custody: Option<Custody>,
    environment: Option<Environment>,
}

impl Ran {
    fn skipped(check: CheckName, detail: String) -> Ran {
        Ran {
            check,
            result: "skipped",
            detail,
            exit_code: None,
            attempts: 0,
            duration_ms: 0,
            event: None,
            custody: None,
            environment: None,
        }
    }
}

/// How one check will run: checked, all of them, before any runs.
struct Prepared<'p> {
    name: CheckName,
    why: String,
    def: &'p CheckDef,
    check_kind: EvidenceKind,
    program: Program,
    environment: Option<Environment>,
    isolation: Option<Isolation>,
}

pub fn run(app: &App<'_>, args: &RunArgs) -> CmdResult {
    if args.isolated && args.environment_image.is_some() {
        return Err(Failure::usage(anyhow!(
            "--isolated runs each check in its own pinned image; --environment-image is for a job that runs in that image itself"
        )));
    }
    let plan = PlanFile::load(app)?;
    let snapshot = args.content.snapshot();
    let content = Content::open(app.global, Source::Repo(snapshot))?;
    // Without hushrun.yaml, a name with no command after -- is most likely a forgotten --.
    if content.policy.is_none()
        && let Some(name) = args.checks.first()
    {
        return Err(Failure::usage(anyhow!(
            "there is no hushrun.yaml here, so `{name}` names none of its checks: give {name}'s command after --, as in hushrun run {name} -- COMMAND"
        )));
    }
    let policy = content.policy()?;
    let subject = match &plan {
        Some(plan) => {
            plan.check(&content)?;
            plan.subject(args.content.scopes.scopes.first(), &content)?
        }
        None => content.subject(&args.content.scopes, args.content.digest)?,
    };
    let (_, ledger) = app.ledger(Some(policy))?;
    let signers = app.signers(policy, ledger.as_ref())?;
    let sigstore = app.sigstore_root()?;
    let decision = match &plan {
        Some(plan) => Some(plan.decision(Some(&subject.scope))?),
        None => None,
    };

    // Which checks, and why each one runs.
    let mut selected: Vec<(CheckName, String)> = Vec::new();
    let mut outcomes: Vec<Ran> = Vec::new();
    let evaluate = decision.is_none() || args.all;
    let store = crate::events::EventStore {
        ledger: ledger.as_ref(),
        signers: &signers,
        sigstore: &sigstore,
        repository: &policy.repository,
        now: app.now,
    };
    // Evidence is evaluated as the planner evaluates it for each check.
    let events: Vec<VerifiedEvent> = if evaluate && !args.force {
        store
            .read(&subject.scope, &subject.digest)
            .map_err(ledger_failure)?
            .accepted
    } else {
        Vec::new()
    };
    let candidates: Vec<CheckName> = match (&decision, args.checks.is_empty(), args.all) {
        (Some(decision), true, false) => {
            selected = decision
                .checks_to_run
                .iter()
                .map(|c| {
                    (
                        c.check.clone(),
                        format!("the plan runs it ({})", reason_text(c.reason)),
                    )
                })
                .collect();
            Vec::new()
        }
        (Some(decision), false, false) => {
            for name in &args.checks {
                let Some(c) = decision.checks_to_run.iter().find(|c| &c.check == name) else {
                    return Err(Failure::usage(anyhow!(
                        "`{name}` isn't in the plan's checks_to_run for `{}`",
                        subject.scope
                    )));
                };
                selected.push((
                    name.clone(),
                    format!("the plan runs it ({})", reason_text(c.reason)),
                ));
            }
            Vec::new()
        }
        (_, false, _) => args.checks.clone(),
        (_, true, _) => policy.required(),
    };
    for name in candidates {
        if !args.force {
            let (status, event) = check_status(&name, &events, policy, &signers.ci_only, app.now);
            if status == Status::Satisfied {
                outcomes.push(Ran::skipped(
                    name.clone(),
                    format!("satisfied by {}", event.unwrap_or_default()),
                ));
                continue;
            }
        }
        selected.push((
            name,
            if args.force {
                "--force".into()
            } else {
                "no valid evidence".into()
            },
        ));
    }

    // \[ADR 0060\] A check that runs where it is runs on the files here: they must be the
    // subject's, or its evidence would be about content that never ran.
    if !args.isolated && !selected.is_empty() {
        let given = plan.is_none() && args.content.digest.is_some();
        runs_here(&content, &subject, &args.content.snapshot(), given)?;
    }

    // An isolated run exports the entries of the subject's scope from this checkout: they must be
    // the content the evidence is about.
    let runtime = if args.isolated && !selected.is_empty() {
        Some(Runtime::detect(app)?)
    } else {
        None
    };
    let entries: Vec<&Entry> = if args.isolated && !selected.is_empty() {
        let entries = exported_entries(&content, &subject)?;
        isolated::refuse_exported_key(app, content.vcs.root(), &entries)?;
        entries
    } else {
        Vec::new()
    };
    if let Some(runtime) = &runtime {
        runtime.sweep();
    }
    let restricted_network = app.env.get(isolated::RESTRICTED_NETWORK_VAR);

    // Everything is checked before anything runs.
    let mut prepared = Vec::new();
    for (name, why) in &selected {
        let def = policy.checks.get(name).ok_or_else(|| {
            Failure::usage(anyhow!(
                "`{name}` isn't a check in hushrun.yaml; to run a command as a check: hushrun run {name} -- COMMAND"
            ))
        })?;
        let check_kind = EvidenceKind::Source;
        let program = program(name, def, args.isolated)?;
        let (environment, isolation) = match &runtime {
            Some(runtime) => {
                let isolation = Isolation::of(app, name, def)?;
                let platform = runtime.prepare(&isolation.image, isolation.platform.as_deref())?;
                (
                    Some(isolation.environment(&platform, runtime)),
                    Some(isolation),
                )
            }
            None => (
                environment(name, def, args.environment_image.as_deref())?,
                None,
            ),
        };
        prepared.push(Prepared {
            name: name.clone(),
            why: why.clone(),
            def,
            check_kind,
            program,
            environment,
            isolation,
        });
    }
    if prepared.is_empty() && outcomes.is_empty() {
        eprintln!("nothing to run");
    }

    let signing = if prepared.is_empty() {
        None
    } else {
        Some(app.signing_with(&sigstore)?)
    };
    let producer = app.producer_for(args.isolated)?;
    let run = app.run_info_for(plan.as_ref())?;
    let forced = plan.as_ref().is_some_and(|p| p.forced);
    // Carrying out a plan in a newer attempt of its run starts that attempt (§10.4).
    if let (Some(plan), Some(signing)) = (&plan, &signing)
        && run.attempt > plan.run.attempt
    {
        let recorder = Recorder {
            ledger: ledger.as_ref(),
            signers: &signers,
            policy,
            signing,
            sigstore: &sigstore,
            producer: &producer,
            run: &run,
            now: app.now,
            forced,
        };
        let read = recorder.read(&subject)?;
        recorder.ensure_started(&subject, &read, None, false, None)?;
    }

    let ci = crate::ci::Ci::of(app);
    let mut unrecorded = Unrecorded::default();
    // B-2: what build blocks record a check run here under, as `hushrun run NAME -- COMMAND`
    // says it. A line saying why reuse is off is said once, not for every check.
    let repository = app
        .run
        .repository
        .clone()
        .or_else(|| wrap::repository_of(&content));
    let mut said_off = false;
    // B-36: the backend a saved fetch must be from, found only when the store holds one; a
    // check asks it nothing.
    let place = || match crate::results::Backend::select(Some(content.vcs.root())) {
        Ok(crate::results::Selection::On(backend)) => backend.build_place(),
        _ => None,
    };
    for Prepared {
        name,
        why,
        def,
        check_kind,
        program,
        environment,
        isolation,
    } in prepared
    {
        match (&isolation, &runtime) {
            (Some(isolation), Some(runtime)) => eprintln!(
                "hushrun run: `{name}` ({why}), isolated in {} by {}, network {}: {}",
                isolation.image,
                runtime.description,
                isolation.network.as_str(),
                program.describe()
            ),
            _ => eprintln!("hushrun run: `{name}` ({why}): {}", program.describe()),
        }
        let timeout = args.timeout.or(def.timeout);
        // B-2: a check run here compiles through the wrapper, in one session for all its
        // attempts. An isolated one never begins a session: its container has neither the link
        // nor the store. B-25: no backend is handed to share through, so a check shares nothing,
        // and in CI gets no session.
        let build = match isolation {
            Some(_) => None,
            None => match build_session::begin(
                system_time(wrap::now(app.global)),
                repository.as_deref(),
                &policy.build_env,
                None,
                &place,
            ) {
                Ok(session) => Some(session),
                Err(why) => {
                    if let Some(why) = why.filter(|_| !said_off) {
                        say(app, &why);
                        said_off = true;
                    }
                    None
                }
            },
        };
        let here_env = build.as_ref().map(Session::env).unwrap_or_default();
        let started_at = Time::from_timestamp(jiff::Timestamp::now());
        let started = Instant::now();
        let mut attempts = 0;
        let mut exit_code;
        loop {
            attempts += 1;
            exit_code = match (&isolation, &runtime) {
                // Every attempt starts from a fresh export: the last one may have changed it.
                (Some(isolation), Some(runtime)) => {
                    // A stop signal now stops the check: its container and export are removed
                    // before hushrun ends, and nothing is recorded.
                    let held = isolated::Held::hold();
                    let export = Export::materialize(
                        content.vcs.as_ref(),
                        &entries,
                        &isolated::export_base(app),
                    )?;
                    let code = isolated::execute(
                        runtime,
                        isolation,
                        restricted_network,
                        &program,
                        &export,
                        &name,
                        timeout,
                    );
                    export.remove(runtime, &isolation.image);
                    if let Some(signal) = held.signal() {
                        eprintln!(
                            "hushrun run: interrupted: `{name}`'s container and export are removed, and nothing was recorded for it"
                        );
                        isolated::end_as(signal);
                    }
                    code?
                }
                _ => match program.execute(timeout, &here_env) {
                    Ok(code) => code,
                    Err(err) => {
                        finish(app, build);
                        return Err(err);
                    }
                },
            };
            if exit_code == Some(0) || attempts > def.retries {
                break;
            }
            eprintln!(
                "hushrun run: `{name}` failed; retrying ({attempts}/{})",
                def.retries
            );
        }
        let duration = started.elapsed();
        // B-20: what its compiles came to, whatever its exit code.
        finish(app, build);
        let finished_at = Time::from_timestamp(jiff::Timestamp::now());
        let result = if exit_code == Some(0) {
            Outcome::Pass
        } else {
            Outcome::Fail
        };
        let environment_digest = environment.as_ref().map(|e| {
            hushrun_core::jcs::digest(&serde_json::to_value(e).expect("serializes"))
                .expect("strings")
        });
        let mut event = base_event(
            EventKind::Check,
            policy,
            &subject,
            &run,
            &producer,
            finished_at,
        );
        if forced {
            event.forced = Some(true);
        }
        event.check = Some(CheckRecord {
            name: name.clone(),
            check_kind,
            definition_digest: def.definition_digest,
            environment: environment.clone(),
            environment_digest,
            result,
            attempts,
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            started_at,
            finished_at,
            exit_code,
            // E-6: informational, for a command given after `--`, which `hushrun run CHECK`
            // never has (ADR 0062).
            command: None,
            reports: None,
        });
        let recorded = events::record(
            ledger.as_ref(),
            signing.as_ref().expect("a signer for checks to run"),
            &event,
            &subject.digest,
        )?;
        let mut detail = match exit_code {
            Some(code) => format!("exit {code}"),
            None => "timed out or killed".into(),
        };
        // \[ADR 0060\] What `hushrun ready` reads of this check here: a pass nothing recorded,
        // until a check event of it is recorded here or it fails (C-22).
        let ended = match (&recorded, result) {
            (Recorded::Keys(_), _) => Ended::Recorded,
            (Recorded::OverQuota(_), Outcome::Pass) => Ended::Unrecorded,
            (Recorded::OverQuota(_), _) => Ended::Failed,
        };
        if let Ok(key) = crate::results::ledger_check_key(&name, &subject.scope, &subject.digest) {
            let _ = crate::results::ran_here(&key, ended);
        }
        // ADR 0032: refused for the org's plan, the check passed or failed all the same, and
        // nothing was recorded for it, so it claims no custody or environment.
        let (key, custody, environment) = match recorded {
            Recorded::Keys(keys) => (keys.into_iter().next(), Some(producer.custody), environment),
            Recorded::OverQuota(quota) => {
                unrecorded.say(&ci, &quota);
                detail.push_str("; not recorded");
                (None, None, None)
            }
        };
        outcomes.push(Ran {
            check: name,
            result: if result == Outcome::Pass {
                "pass"
            } else {
                "fail"
            },
            detail,
            exit_code,
            attempts,
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            event: key,
            custody,
            environment,
        });
    }

    let failed = outcomes.iter().any(|o| o.result == "fail");
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.run/v2",
            "scope": subject.scope,
            "digest": subject.digest,
            "run": run,
            "isolated": args.isolated,
            "checks": outcomes.iter().map(|o| json!({
                "check": o.check,
                "result": o.result,
                "detail": o.detail,
                "exitCode": o.exit_code,
                "attempts": o.attempts,
                "durationMs": o.duration_ms,
                "event": o.event,
                "custody": o.custody,
                "environment": o.environment,
            })).collect::<Vec<_>>(),
        }))?,
        _ => {
            let mut text = String::new();
            for o in &outcomes {
                let _ = writeln!(text, "{:<8} {} ({})", o.result, o.check, o.detail);
            }
            output::print(&text)?;
        }
    }
    Ok(if failed { Exit::CheckFailed } else { Exit::Ok })
}

/// One of hushrun's lines about a check, on stderr: stdout is the command's output. `-q`
/// silences it.
fn say(app: &App<'_>, line: &str) {
    if app.global.quiet == 0 {
        eprintln!("hushrun run: {line}");
    }
}

/// Ends a check's build session, if it had one, and says what its compiles came to (B-20).
fn finish(app: &App<'_>, build: Option<Session>) {
    if let Some(session) = build {
        for line in build_session::finish(session, system_time(wrap::now(app.global))).lines() {
            say(app, line);
        }
    }
}

/// The subject's scope in this checkout.
fn scope_of<'c>(content: &'c Content, subject: &Subject) -> Result<&'c Scope, Failure> {
    content
        .scopes
        .iter()
        .find(|s| s.name == subject.scope)
        .ok_or_else(|| Failure::usage(anyhow!("unknown scope `{}`", subject.scope)))
}

/// \[ADR 0060\] Fails unless a check that runs where it is runs on the subject's content (§10.4):
/// with a digest `given`, it is the scope's here; and at a revision, each of the scope's files
/// there is in the worktree as it is at the revision. A file the revision doesn't have is left
/// out: a job's own steps write some, such as a credentials file, which its checks don't read.
fn runs_here(
    content: &Content,
    subject: &Subject,
    snapshot: &Snapshot,
    given: bool,
) -> Result<(), Failure> {
    let scope = scope_of(content, subject)?;
    if given {
        let digested = digest_scopes(
            content.vcs.as_ref(),
            &content.listing,
            &content.scopes,
            &[scope],
            false,
        )?;
        if digested.scopes[0].digest != subject.digest {
            return Err(Failure::usage(anyhow!(
                "`{}` here digests to {}, not {}: a check runs on the files here, so run it where the content is",
                subject.scope,
                digested.scopes[0].digest,
                subject.digest
            )));
        }
    }
    let revision = &content.listing.revision;
    let (Snapshot::Rev(_), Some(commit), "git") = (
        snapshot,
        revision.revision.as_deref(),
        revision.vcs.as_str(),
    ) else {
        return Ok(());
    };
    let changed = changed_files(content.vcs.root(), commit)?;
    let differ: Vec<&String> = changed
        .iter()
        .filter(|path| content.scopes.contains(scope, path))
        .collect();
    let Some(first) = differ.first() else {
        return Ok(());
    };
    let short: String = commit.chars().take(7).collect();
    let which = match differ.len() {
        1 => format!("{first} differs"),
        n => format!("{first} and {} more differ", n - 1),
    };
    Err(Failure::usage(anyhow!(
        "`{}` here isn't as at {short}: {which}, and a check runs on the files as they are. Check out {short}, or commit or stash what's different, or pass --worktree to record it for the files here",
        subject.scope
    )))
}

/// The files git tracks that the worktree holds otherwise than `commit` does: changed, deleted,
/// or of another kind, by path from the root, as `git add` would record them. A file `commit`
/// doesn't have, staged or not, isn't one.
fn changed_files(root: &std::path::Path, commit: &str) -> Result<Vec<String>, Failure> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-renames",
            "--ignore-submodules=dirty",
            "--diff-filter=MDTU",
            "--name-only",
            "-z",
            commit,
            "--",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Failure::from(anyhow!("can't compare the files here with {commit}: {e}")))?;
    if !output.status.success() {
        return Err(Failure::from(anyhow!(
            "can't compare the files here with {commit}: git diff: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

/// The entries of the subject's scope in this checkout, which an isolated run exports. Their
/// digest must be the subject's: evidence is about the content that ran.
fn exported_entries<'c>(
    content: &'c Content,
    subject: &Subject,
) -> Result<Vec<&'c Entry>, Failure> {
    let scope = scope_of(content, subject)?;
    let digested = digest_scopes(
        content.vcs.as_ref(),
        &content.listing,
        &content.scopes,
        &[scope],
        false,
    )?;
    if digested.scopes[0].digest != subject.digest {
        return Err(Failure::usage(anyhow!(
            "`{}` here digests to {}, not {}: an isolated run exports this checkout, so run it where the content is (pass --worktree for uncommitted changes)",
            subject.scope,
            digested.scopes[0].digest,
            subject.digest
        )));
    }
    Ok(content
        .listing
        .entries
        .iter()
        .filter(|e| content.scopes.contains(scope, &e.path))
        .collect())
}

fn reason_text(reason: hushrun_core::plan::RunReason) -> &'static str {
    use hushrun_core::plan::RunReason as R;
    match reason {
        R::Missing => "missing",
        R::Expired => "expired",
        R::Failed => "failed",
        R::Forced => "forced",
    }
}

/// How a check runs: its `run` through its shell.
pub(crate) enum Program {
    Shell { shell: Shell, script: String },
}

fn program(name: &CheckName, def: &CheckDef, isolated: bool) -> Result<Program, Failure> {
    match &def.run {
        Some(script) => Ok(Program::Shell {
            // An isolated run is in a Linux container, whatever the host.
            shell: def.shell.unwrap_or(if cfg!(windows) && !isolated {
                Shell::Pwsh
            } else {
                Shell::Sh
            }),
            script: script.clone(),
        }),
        None => Err(Failure::usage(anyhow!(
            "`{name}` has no `run` in hushrun.yaml: record its result with `hushrun attest --check {name}=pass`, or run a command as a check with `hushrun run {name} -- COMMAND`"
        ))),
    }
}

impl Program {
    fn describe(&self) -> String {
        match self {
            Program::Shell { shell, script } => format!("{} -c {script:?}", shell.as_str()),
        }
    }

    /// Runs once, here, with `env` added to hushrun's own; the exit code, or `None` when killed
    /// at the timeout or by a signal. The check's stdout goes to hushrun's stderr, so hushrun's
    /// stdout stays its own output. An isolated run never comes here (`isolated::execute`).
    fn execute(
        &self,
        timeout: Option<Dur>,
        env: &[(OsString, OsString)],
    ) -> Result<Option<i32>, Failure> {
        let mut command = match self {
            Program::Shell { shell, script } => {
                let mut c = Command::new(match shell {
                    Shell::Sh => "sh",
                    Shell::Bash => "bash",
                    Shell::Pwsh => "pwsh",
                    Shell::Cmd => "cmd",
                });
                match shell {
                    Shell::Sh | Shell::Bash => c.args(["-c", script]),
                    Shell::Pwsh => c.args(["-NoProfile", "-NonInteractive", "-Command", script]),
                    Shell::Cmd => c.args(["/C", script]),
                };
                c
            }
        };
        command
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::io::stderr()))
            .stderr(Stdio::inherit());
        let mut child = command
            .spawn()
            .map_err(|e| Failure::usage(anyhow!("starting {}: {e}", self.describe())))?;
        let deadline = timeout.map(|t| Instant::now() + t.as_std());
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| Failure::from(anyhow!("waiting for the check: {e}")))?
            {
                return Ok(status.code());
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                kill(&mut child);
                eprintln!(
                    "hushrun run: killed after {} s (timeout)",
                    timeout.map_or(0, |t| t.as_std().as_secs())
                );
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Kills a check and, on Unix, every process under it, then reaps it. Killing its shell alone
/// would leave its commands running past the timeout, and holding hushrun's output open: `sh -c`
/// runs them as its children (dash even the last one), and they start their own. Each process
/// found is stopped first, so that none starts another meanwhile, and all are killed once the
/// tree stops growing. The check stays in hushrun's process group: a terminal's signals and
/// prompts reach it as before.
fn kill(child: &mut Child) {
    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal, kill_process};
        let send = |pid: u32, signal: Signal| {
            if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
                let _ = kill_process(pid, signal);
            }
        };
        let mut tree = vec![child.id()];
        let mut stopped = 0;
        while stopped < tree.len() {
            for &pid in &tree[stopped..] {
                send(pid, Signal::STOP);
            }
            stopped = tree.len();
            let processes = processes();
            let mut grew = true;
            while grew {
                grew = false;
                for &(pid, parent) in &processes {
                    if tree.contains(&parent) && !tree.contains(&pid) {
                        tree.push(pid);
                        grew = true;
                    }
                }
            }
        }
        for &pid in &tree {
            send(pid, Signal::KILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Every process's ID and its parent's, from `/proc`.
#[cfg(target_os = "linux")]
fn processes() -> Vec<(u32, u32)> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|entry| {
        let entry = entry.ok()?;
        let pid = entry.file_name().to_str()?.parse().ok()?;
        let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
        // `pid (name) state ppid …`, where the name can hold spaces and parentheses.
        let parent = stat.rsplit_once(')')?.1.split_whitespace().nth(1)?;
        Some((pid, parent.parse().ok()?))
    })
    .collect()
}

/// Every process's ID and its parent's, from `ps`.
#[cfg(all(unix, not(target_os = "linux")))]
fn processes() -> Vec<(u32, u32)> {
    let Ok(output) = Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "ppid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect()
}

/// The environment of a check that pins an image: the job declares the image it runs in,
/// and it must be the pinned one, or the evidence would never count (E-7, P-1).
fn environment(
    name: &CheckName,
    def: &CheckDef,
    declared: Option<&str>,
) -> Result<Option<Environment>, Failure> {
    let Some(pinned) = &def.image else {
        return Ok(declared.map(|image| Environment {
            image: Some(image.to_string()),
            platform: def.platform.clone(),
            network: None,
            runner: None,
        }));
    };
    let Some(image) = declared else {
        return Err(Failure::usage(anyhow!(
            "check `{name}` pins image {pinned}: pass --environment-image with the image this job runs in, or run it in that image with --isolated"
        )));
    };
    if pinned_digest(image).is_none() || pinned_digest(image) != pinned_digest(pinned) {
        return Err(Failure::usage(anyhow!(
            "check `{name}` pins {pinned}, but this job runs in {image}: that evidence would never count"
        )));
    }
    Ok(Some(Environment {
        image: Some(image.to_string()),
        platform: def.platform.clone(),
        network: None,
        runner: None,
    }))
}
