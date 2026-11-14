//! `hushrun run [NAME] -- COMMAND`: runs COMMAND unless this exact code
//! already passed it, and remembers the result.
//!
//! It needs no configuration, and works the same on a laptop, in CI and in an agent's sandbox.
//! The code is the repository's files as they are now, ignored ones left out, so a pass in one
//! place stands in for a run in another when the files match. Whenever hushrun can't tell, it
//! runs COMMAND: the most it can cost is a run, never a skip that should have been a run.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};
use std::time::{Instant, SystemTime};

use anyhow::{Context as _, anyhow};

use hushrun_build::session::Session;
use hushrun_ci::{Env, RunContext};
use hushrun_core::counts_from::Rule;
use hushrun_core::digest::Digest;
use hushrun_core::names::{CheckName, ScopeName};
use hushrun_core::policy::{Policy, ReportsCi};
use hushrun_core::run::Provider;
use hushrun_core::scope::Scopes;
use hushrun_core::time::Time;
use hushrun_reports::{Ratio, Reports};
use hushrun_vcs::Snapshot;

use crate::ci::Ci as Runner;
use crate::cli::{GlobalOpts, RunArgs};
use crate::cmd::build_session::{self, system_time};
use crate::cmd::init::origin_repository;
use crate::cmd::setup::{checks, variants};
use crate::context::{self, Content, Source};
use crate::exit::Exit;
use crate::measure::{self, Judged};
use crate::output::Outputs;
use crate::results::{
    self, Backend, Ended, ExternalInput, Off, RanAnyway, Refusal, RunEnvironment, RunResult,
    Selection, Verdict,
};
use crate::units;
use hushrun_core::units::Mode;

pub fn run(global: &GlobalOpts, args: &RunArgs) -> ExitCode {
    let invoked = Instant::now();
    let (check, command) = match parse(global, args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("error: {err:#}");
            return Exit::Usage.into();
        }
    };
    let say = Say {
        quiet: global.quiet > 0,
    };
    let dir = match context::start_dir(global) {
        Ok(dir) => dir,
        Err(failure) => {
            eprintln!("error: {:#}", failure.error);
            return failure.exit.into();
        }
    };

    // Who runs it, a label for the dashboard (ADR 0027 item 4).
    let invoked_by = match invoked_by() {
        Ok(label) => label,
        Err(why) => {
            say.line(format_args!("{why}"));
            None
        }
    };
    // Which code this is, and where results live. Without either, the command just runs.
    let code = match Code::read(global, args, &check, command, &dir, invoked_by) {
        Ok(code) => Some(code),
        Err(err) => {
            say.line(format_args!(
                "{check} running without skipping: can't tell which code this is ({err:#})"
            ));
            None
        }
    };
    // B-25: build blocks belong to the repository, not to a scope, so a run that can't tell
    // which code it is (several scopes and no `--scope`) still shares them when the repository
    // can be read.
    let build_repo = match &code {
        Some(code) => Some(BuildRepo::of(code)),
        None => BuildRepo::read(global, args).ok(),
    };
    let backend = match Backend::select(build_repo.as_ref().map(|repo| repo.root.as_path())) {
        Ok(Selection::On(backend)) => Some(backend),
        // \[ADR 0067\] Said once: a CI job that shares nothing would otherwise look as if it did.
        Ok(Selection::Off(off @ Off::InCi)) => {
            if Env::from_process().get(variants::INSIDE_A_RUN).is_none() {
                say.line(format_args!(
                    "{check} running without skipping: results are off: {}",
                    off.reason()
                ));
            }
            None
        }
        Ok(Selection::Off(_)) => None,
        Err(err) => {
            say.line(format_args!(
                "{check} running without skipping: results are off ({err:#})"
            ));
            None
        }
    };
    if let Some(base) = backend.as_ref().and_then(Backend::withheld_token) {
        say.line(format_args!(
            "HUSHRUN_TOKEN isn't sent to {base}: it goes only to the backend HUSHRUN_BACKEND names, and an agent token (hra_…) to hushrun's host, {}",
            crate::hosted::origin()
        ));
    }

    // \[ADR 0044\] A key holds no environment, so a job that runs its steps several times can't
    // reuse a pass (C-14). \[ADR 0047\] A later step that reads the check's reports finds none after
    // a skip (C-26). The run reads its job once, for both.
    let env = Env::from_process();
    // \[ADR 0048\] What the job's readers read is where its reports may be (C-27).
    let mut code = code;
    let (no_pass_skips, reports_read, reader_paths) = match &mut code {
        Some(code) => {
            let read = |path: &str| std::fs::read(code.root.join(path)).ok();
            let ci = variants::Ci::read(&env, &read);
            let words: Vec<&str> = command.iter().map(String::as_str).collect();
            // \[ADR 0049\] A job of several variants that says which this run is skips it on that
            // variant's passes alone (C-14).
            let no_pass_skips = match ci.variants(&env, check.as_str(), !args.checks.is_empty()) {
                variants::Variants::Alike => None,
                variants::Variants::Variant(variant) => {
                    code.variant = Some(variant);
                    None
                }
                variants::Variants::NoSkip(no) => Some(no),
            };
            (
                no_pass_skips,
                ci.reports(
                    &read,
                    checks::kind_in(&words, Some(code.cwd.as_str()), &read),
                    code.reports_skip,
                    args.reports_read,
                ),
                ci.reader_paths(&read),
            )
        }
        None => (None, None, Vec::new()),
    };

    let exit = skip_or_run(
        global,
        args,
        &say,
        &check,
        command,
        &dir,
        code.as_ref(),
        build_repo.as_ref(),
        backend.as_ref(),
        no_pass_skips.as_ref(),
        reports_read.as_ref(),
        &reader_paths,
        invoked,
    );
    // \[ADR 0067\] This machine's directory forgets what is 30 days old, once the run is over.
    if let Some(backend) = backend.as_ref() {
        backend.sweep(now(global));
        // \[ADR 0068\] What a stopped run left of its test files' state.
        if let Some(cache) = context::cache_dir() {
            units::store::sweep(&cache, SystemTime::now());
        }
    }
    // \[ADR 0067\] The repository's backend, which nobody here signed in to: this machine kept
    // the result, and the person can share it.
    if let Some(org) = backend
        .as_ref()
        .and_then(Backend::local)
        .and_then(results::Local::org)
    {
        if backend.as_ref().is_some_and(Backend::sign_in_helps) && std::io::stderr().is_terminal() {
            say.line(format_args!(
                "sign in to share results with {org}: npx hushrun login"
            ));
        } else {
            say.line(format_args!(
                "results aren't shared with {org}: nobody is signed in to it here, so they stay on this machine"
            ));
        }
    }
    // A request without a token was refused for want of one, or \[ADR 0052\] a run with none only
    // read: once and last, where a person reads it. At a terminal, signing in fixes it; a script
    // or a CI job has no one to sign in, so it gets the server's reason (C-11, C-14).
    if let Some(backend) = backend.as_ref()
        && let Some(why) = match backend.refusal() {
            Some(Refusal::NoToken(why)) => Some(why),
            _ if backend.reads_only() => Some(results::NO_ID_TOKEN.to_string()),
            _ => None,
        }
    {
        let org = backend.org().unwrap_or_default();
        if backend.sign_in_helps() && std::io::stderr().is_terminal() {
            say.line(format_args!(
                "sign in to share results with {org}: npx hushrun login"
            ));
        } else if backend.withheld_token().is_none() {
            // A withheld HUSHRUN_TOKEN was said first: the server's "set HUSHRUN_TOKEN" would
            // only contradict it. A fork's pull request can't sign in or get a token: it's told
            // why instead of how.
            if hushrun_ci::github::from_a_fork(&Env::from_process()) {
                let fork = if backend.answered_without_a_token() {
                    FORK_READ
                } else {
                    FORK
                };
                say.line(format_args!("results aren't shared with {org}: {fork}"));
            } else {
                say.line(format_args!("results aren't shared with {org}: {why}"));
            }
        }
    }
    exit
}

/// Why a run of a pull request from a fork shares no results: GitHub gives its job neither an ID
/// token nor a secret (results-v0 §2), and the server answered no lookup without one.
const FORK: &str =
    "GitHub gives a pull request from a fork no ID token, so its checks run, and record nothing";

/// \[ADR 0052\] The same, when the server answered its lookup without one, as it does for a public
/// repository of its open-source program: it may skip, and records nothing.
const FORK_READ: &str =
    "GitHub gives a pull request from a fork no ID token, so it records nothing";

/// `HUSHRUN_INVOKED_BY`, `agent:<id>` or `human:<id>`, as events take it (E-4); or why it
/// isn't one.
fn invoked_by() -> Result<Option<String>, String> {
    let Some(value) = hushrun_ci::var("HUSHRUN_INVOKED_BY")
        .ok()
        .filter(|v| !v.trim().is_empty())
    else {
        return Ok(None);
    };
    match value.split_once(':') {
        Some((kind @ ("agent" | "human"), id)) if !id.trim().is_empty() => {
            Ok(Some(format!("{kind}:{}", id.trim())))
        }
        _ => Err(format!(
            "HUSHRUN_INVOKED_BY is {value:?}, not agent:<id> or human:<id>, so runs aren't labeled"
        )),
    }
}

/// Whether the backend refused who sent a request, so that recording would be refused too.
fn refused_caller(backend: &Backend) -> bool {
    matches!(
        backend.refusal(),
        Some(Refusal::NoToken(_) | Refusal::Credential(_))
    )
}

/// Skips COMMAND when `backend` has a pass for this exact `code` that counts in this job (C-10);
/// else runs it, and records how it ended, with the summary of the reports it wrote (C-27).
/// `no_pass_skips` says why it runs whatever passed, and `reports_read` what in this job reads
/// reports a skip doesn't write (C-26); `reader_paths` are what the job's readers read. A run
/// that a pass would have skipped but for one rule records the rule (`ranAnyway`), and a verdict
/// the result that decided here didn't have says it may be flaky.
#[allow(clippy::too_many_arguments)]
fn skip_or_run(
    global: &GlobalOpts,
    args: &RunArgs,
    say: &Say,
    check: &CheckName,
    command: &[String],
    dir: &Path,
    code: Option<&Code>,
    build_repo: Option<&BuildRepo>,
    backend: Option<&Backend>,
    no_pass_skips: Option<&variants::NoSkip>,
    reports_read: Option<&variants::ReportsRead>,
    reader_paths: &[String],
    invoked: Instant,
) -> ExitCode {
    let job = hushrun_ci::result_job(&Env::from_process());
    // Why it runs though a pass of its key would have skipped it, and the result that decided
    // here, which a run with the other verdict calls flaky.
    let mut anyway: Option<RanAnyway> = None;
    let mut decided: Option<RunResult> = None;
    if let (Some(code), Some(backend)) = (code, backend) {
        // B-31: a token's lookup from CI names the repository the run is of.
        backend.names_repository(code.repository_here().as_deref());
        // \[ADR 0049\] The newest result that counts in this job, and this variant (C-10).
        let variant = code.variant.as_ref().map(|variant| variant.key.as_str());
        if args.force {
            say.line(format_args!("{check} running (--force)"));
            // One request, to know whether a pass would have skipped it.
            decided = counting(backend, &code.key, job.as_deref(), variant);
            anyway = passed(decided.as_ref()).then_some(RanAnyway::Force);
        } else if let Some(no) = no_pass_skips {
            say.line(format_args!("{check} running: {}", no.why));
            // One request, to know whether a pass of this job would have skipped it, had the job
            // said which variant or call this is. Its verdict decides nothing here.
            let inside = Env::from_process().get(variants::INSIDE_A_RUN).is_some();
            let rule = match no.wall {
                variants::Wall::Variants if inside => RanAnyway::InsideRun,
                variants::Wall::Variants => RanAnyway::Variants,
                variants::Wall::Reusable => RanAnyway::Reusable,
                variants::Wall::Reports => RanAnyway::Reports,
                variants::Wall::CantTell => RanAnyway::CantTell,
            };
            let found = counting(backend, &code.key, job.as_deref(), None);
            anyway = passed(found.as_ref()).then_some(rule);
        } else {
            let latest = backend.latest_for_job(&code.key, job.as_deref(), variant);
            // C-12: the repository lets CI skip only on passes CI recorded.
            let ci_only = match &latest {
                Ok(latest) if latest.ci_only => "; skipped only on CI's passes",
                _ => "",
            };
            // \[ADR 0049\] When nothing counts here, the newest pass anywhere says why, and only
            // that: one that would count here is the server's to answer, so it never skips. A
            // server from before the query answered that already.
            let latest = latest.map(|latest| match latest.result {
                None if latest.for_job => backend
                    .latest(&code.key)
                    .ok()
                    .and_then(|anywhere| anywhere.result)
                    .filter(|anywhere| {
                        anywhere.result == Verdict::Pass
                            && !anywhere.counts_in(job.as_deref(), variant)
                    }),
                result => result,
            });
            // \[ADR 0047\] Why a later step that reads its reports keeps the pass from skipping it.
            let read_anyway = match (&latest, reports_read) {
                (Ok(Some(last)), Some(read)) if last.result == Verdict::Pass => {
                    still_read(read, last, job.as_deref(), code)
                }
                _ => None,
            };
            match latest {
                Ok(Some(last))
                    if last.result == Verdict::Pass && !last.counts_in(job.as_deref(), variant) =>
                {
                    say.line(format_args!(
                        "{check} running: {}{ci_only}",
                        elsewhere(&last, job.as_deref())
                    ));
                    anyway = Some(RanAnyway::Rule);
                }
                // \[ADR 0047\] It would skip, but a later step reads its reports.
                Ok(Some(last)) if last.result == Verdict::Pass && read_anyway.is_some() => {
                    let why = read_anyway.map(|no| no.why).unwrap_or_default();
                    say.line(format_args!("{check} running: {why}"));
                    anyway = Some(RanAnyway::Reports);
                    decided = Some(last);
                }
                Ok(Some(last)) if last.result == Verdict::Pass => {
                    let place = if last.provider == Provider::Local {
                        ""
                    } else {
                        " in CI"
                    };
                    let when = ago(now(global).millis_since(last.finished_at()));
                    // \[ADR 0048\] What the pass's reports said, which the skip stands in for.
                    say.line(format_args!(
                        "{check} skipped: this exact code passed{place} {when} (saves {}){}{ci_only}",
                        took(last.duration_ms),
                        said(last.reports.as_ref()),
                    ));
                    let runner = Runner::from_process(global);
                    write_outputs(&runner, Verdict::Skip, last.reports.as_ref());
                    let summary = skip_summary(check, &last, &when, code.repository_here().as_deref());
                    if let Err(err) = runner.summary(&summary) {
                        tracing::debug!("the step summary wasn't written: {err:#}");
                    }
                    // Counted where results are kept: the time the skip saved.
                    let skip = code.result(
                        check,
                        command,
                        Verdict::Skip,
                        None,
                        now(global),
                        elapsed(invoked),
                        Some(last.duration_ms),
                        // Skipping measures nothing.
                        BTreeMap::new(),
                        last.reports.clone(),
                        None,
                    );
                    if let Err(err) = backend.record(&code.key, &skip) {
                        // Without a machine to sign with, nothing this person or agent runs is
                        // recorded: worth one line (ADR 0028).
                        match backend.refusal() {
                            Some(Refusal::Machine(why)) => {
                                say.line(format_args!("the skip wasn't recorded: {why}"));
                            }
                            _ => tracing::debug!("the skip wasn't recorded: {err:#}"),
                        }
                    }
                    return ExitCode::SUCCESS;
                }
                Ok(Some(last)) => {
                    say.line(format_args!(
                        "{check} running: it failed on this exact code last time"
                    ));
                    decided = Some(last);
                }
                Ok(None) if !ci_only.is_empty() => say.line(format_args!(
                    "{check} running: no CI pass yet for this exact code; skipped only on CI's passes"
                )),
                Ok(None) => say.line(format_args!(
                    "{check} running: no pass yet for this exact code"
                )),
                Err(err) => match backend.refusal() {
                    // Said once, last (see `run`).
                    Some(Refusal::NoToken(_)) => {}
                    Some(Refusal::Credential(why)) => say.line(format_args!(
                        "{check} running: {} refused {}: {why}",
                        backend.org().unwrap_or_default(),
                        backend.credential().map_or("the token", |c| c.named()),
                    )),
                    _ => say.line(format_args!(
                        "{check} running: can't read earlier results ({err})"
                    )),
                },
            }
        }
    }

    // \[ADR 0045\] Said before a long run, and only that: what is measured after the command decides,
    // since a toolchain may arrive during it.
    if let (Some(code), Some(_)) = (code, backend)
        && let Some(rule) = &code.rule
        && let Judged::NotMet(why) = measure::judge(rule, command, dir, &code.root)
    {
        say.line(format_args!("{check} running: this run won't count: {why}"));
    }

    // \[ADR 0068\] With `tests` on, an adapter records what each test file read and how it ended
    // (C-32); with it off, nothing here changes COMMAND's environment or writes a file.
    let mut session = match (code, backend) {
        (Some(code), Some(_)) => observe_tests(say, check, code, command, dir),
        _ => None,
    };
    // B-2: with reuse on, cargo in COMMAND compiles through the wrapper: one session for every
    // start of COMMAND in this run, begun before the first and finished after the last. It
    // begins before the plan, whose build is COMMAND's made first, and compiles through it too.
    let mut build = BuildRun::begin(global, say, check, build_repo, backend);
    if let Some(session) = session.as_deref_mut() {
        session.compile_with(build.env());
    }
    let started_at = now(global);
    // \[ADR 0069\] With `tests: reuse`, the plan: which test files already passed on the files they
    // read, and aren't run again (C-34); or why every test file runs.
    let mut units_anyway: Option<String> = None;
    if let (Some(session), Some(code), Some(backend)) = (session.as_deref_mut(), code, backend)
        && code.tests.as_ref().is_some_and(|t| t.mode == Mode::Reuse)
    {
        let why_not = if args.force {
            Some(("--force runs it whole".to_string(), "force"))
        } else if let Some(no) = no_pass_skips {
            Some((no.why.clone(), ran_anyway_word(no, inside_run())))
        } else if let Some(read) = reports_read {
            Some((
                read.no_skip()
                    .why
                    .replace("a skip writes none", "a test file it reuses writes none"),
                "reports",
            ))
        } else if backend.reads_only() || refused_caller(backend) {
            Some((
                "nothing here can read which passed".to_string(),
                "cant-tell",
            ))
        } else {
            None
        };
        match why_not {
            Some((why, word)) => {
                say.line(format_args!("{check}: every test file runs: {why}"));
                units_anyway = Some(word.to_string());
            }
            None => match plan_units(
                global,
                session,
                code,
                backend,
                command,
                dir,
                job.as_deref(),
                started_at,
            ) {
                Some(units::Planned::Plan(line)) if line.is_empty() => {}
                Some(units::Planned::Plan(line)) => say.line(format_args!("{line}")),
                Some(units::Planned::Everything(why)) => {
                    say.line(format_args!("{check}: every test file runs: {why}"));
                    if why.contains("suspended") {
                        units_anyway = Some("suspended".to_string());
                    }
                }
                None => {}
            },
        }
    }

    let clock = Instant::now();
    let started = SystemTime::now();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut child = Command::new(program(&command[0], dir, &path));
    child
        .args(&command[1..])
        .current_dir(dir)
        .env(variants::INSIDE_A_RUN, "1");
    build.apply(&mut child);
    if let Some(session) = &session {
        session.apply(&mut child);
    }
    let status = child.status();
    let duration_ms = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
    let status = match status {
        Ok(status) => status,
        Err(err) => {
            eprintln!("hushrun: can't start {}: {err}", command[0]);
            // As shells report it: 127, not found; 126, found but not runnable.
            let code = if err.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            };
            return ExitCode::from(code);
        }
    };
    let exit = exit_code(&status);
    // G13: a verdict the result that decided here didn't have.
    let flaky = flaky(decided.as_ref(), status.success());
    if signaled(&status) {
        if code.is_some() && backend.is_some() {
            say.line(format_args!(
                "{check} stopped by a signal; nothing recorded"
            ));
        }
        return ExitCode::from(exit);
    }
    let verdict = if status.success() {
        Verdict::Pass
    } else {
        Verdict::Fail
    };
    let runner = Runner::from_process(global);
    let (Some(code), Some(backend)) = (code, backend) else {
        write_outputs(&runner, verdict, None);
        return ExitCode::from(exit);
    };
    // \[ADR 0060\] A failure here outranks an earlier pass here that nothing recorded (C-22),
    // whatever becomes of its own record.
    if verdict == Verdict::Fail {
        let _ = results::ran_here(&code.key, Ended::Failed);
    }
    // \[ADR 0068\] What the adapter saw, decided, and what `tests: reuse` would have done; none
    // when the files aren't the ones the run's key is for.
    let mut observed = session.as_ref().and_then(|session| {
        observed(
            global,
            session,
            code,
            backend,
            command,
            dir,
            status.success(),
            job.as_deref(),
        )
    });
    let reused = observed
        .as_ref()
        .and_then(|(o, _)| o.counts.as_ref())
        .map_or(0, |c| c.reused);
    // \[ADR 0048\] What the reports this run wrote say, read after it, pass or fail (C-27).
    let mut reports = code.reports_of(dir, started, command, reader_paths);
    // \[ADR 0069\] Coverage of the test files that ran isn't the check's: a partial figure would
    // read as a drop (C-27).
    if reused > 0
        && let Some(reports) = reports.as_mut()
    {
        reports.coverage = None;
    }
    write_outputs(&runner, verdict, reports.as_ref());
    let mut summary = said(reports.as_ref());
    // \[ADR 0069\] What reuse saved, from the reused test files' recorded durations.
    if reused > 0 {
        let saved = observed
            .as_ref()
            .and_then(|(o, _)| o.counts.as_ref()?.saved_ms)
            .unwrap_or(0);
        let files = if reused == 1 {
            "1 test file".to_string()
        } else {
            format!("{reused} test files")
        };
        if saved >= 1000 {
            summary.push_str(&format!("; reusing {files} saved about {}", took(saved)));
        } else {
            summary.push_str(&format!("; {files} reused"));
        }
    }
    let ended = if verdict == Verdict::Pass {
        "passed"
    } else {
        "failed"
    };
    // How it ended, with what its reports say, for the lines that end it.
    let outcome = if verdict == Verdict::Pass {
        format!("{check} passed in {}{summary}", took(duration_ms))
    } else {
        format!(
            "{check} failed in {} (exit {exit}){summary}",
            took(duration_ms)
        )
    };
    // On GitHub, the step summary says it may be flaky too.
    if !flaky.is_empty()
        && let Err(err) = runner.summary(&flaky_summary(check, &format!("{outcome}{flaky}")))
    {
        tracing::debug!("the step summary wasn't written: {err:#}");
    }
    // \[ADR 0052\] With no credential at all, as in a fork's pull request, a run only reads: that
    // is said once, last.
    if backend.reads_only() {
        return ExitCode::from(exit);
    }
    // The backend refused who sent the lookup: it would refuse the record too. A refusal for
    // want of a token is said once, last; any other, here.
    if refused_caller(backend) {
        if !matches!(backend.refusal(), Some(Refusal::NoToken(_))) {
            say.line(format_args!("{outcome}; not recorded{flaky}"));
        }
        return ExitCode::from(exit);
    }
    // \[ADR 0045\] What decides, measured where the command ran: a run that doesn't meet the rule
    // records nothing under its key, so nothing can skip on it, and costs no quota. Its mark, pass
    // or fail, is how `hushrun ready`, which starts no tool, learns why and stops asking.
    let didnt_count = |why: &str| {
        if let Err(not_kept) = results::ran_here(&code.key, Ended::DidntCount(why)) {
            say.line(format_args!(
                "{check}: hushrun ready won't learn of this, as the note for it couldn't be kept: {not_kept}"
            ));
        }
        say.line(format_args!("{outcome}; it won't count: {why}{flaky}"));
        ExitCode::from(exit)
    };
    let judged = (code.rule.as_ref()).map(|rule| measure::judge(rule, command, dir, &code.root));
    let met = match judged {
        None => None,
        Some(Judged::Met { line, tools }) => Some((line, tools)),
        Some(Judged::NotMet(why) | Judged::Unknown(why)) => return didnt_count(&why),
    };
    // \[C-34, ADR 0069\] An audited test file that failed, or one the plan listed that ran all the
    // same and failed, runs once more: COMMAND as written, selecting it alone.
    let mut again_states: BTreeMap<String, hushrun_core::units::State> = BTreeMap::new();
    if let (Some(session), Some((o, tree))) = (session.as_ref(), observed.as_ref())
        && !o.again.is_empty()
    {
        let names: Vec<&str> = o.again.keys().map(String::as_str).collect();
        say.line(format_args!(
            "{check}: {} failed, though {} passed before on the same files {} read: running {} once more",
            names.join(", "),
            if names.len() == 1 { "it" } else { "they" },
            if names.len() == 1 { "it" } else { "they" },
            if names.len() == 1 { "it" } else { "them" },
        ));
        let wanted: BTreeSet<String> = o.again.keys().cloned().collect();
        again_states = match session.again(o, &wanted) {
            Some(again) => {
                let mut second = Command::new(program(&command[0], dir, &path));
                second
                    .args(&command[1..])
                    .current_dir(dir)
                    .env(variants::INSIDE_A_RUN, "1");
                build.apply(&mut second);
                session.apply(&mut second);
                again.apply(&mut second);
                let passed = second.status().is_ok_and(|s| s.success());
                session.again_ended(&again, o, tree, passed)
            }
            // A second run that can't be set up tells nothing: each failed again, for all hushrun
            // can tell.
            None => wanted
                .iter()
                .map(|unit| (unit.clone(), hushrun_core::units::State::NotRun))
                .collect(),
        };
    }
    // B-20: COMMAND starts no more: what its compiles came to, whatever its exit codes. A run
    // that returned before here said it when it returned (`BuildRun`'s drop).
    build.finish();
    match code.unchanged(global, args) {
        Ok(true) => {}
        Ok(false) => {
            say.line(format_args!(
                "{check} {ended} in {}, but files changed while it ran, so it wasn't recorded. If {check} writes files, list them in .gitignore{flaky}",
                took(duration_ms)
            ));
            // Running it again would do the same: `hushrun ready` mustn't ask for it for ever.
            if verdict == Verdict::Pass {
                let _ = results::ran_here(&code.key, Ended::Unrecorded);
            }
            return ExitCode::from(exit);
        }
        Err(err) => {
            say.line(format_args!(
                "{check} {ended} in {}, but it wasn't recorded: can't read the files again ({err:#}){flaky}",
                took(duration_ms)
            ));
            return ExitCode::from(exit);
        }
    }

    if let Some((o, _)) = &observed {
        for line in &o.lines {
            say.line(format_args!("{line}"));
        }
    }
    // \[C-34\] What the second runs say: a pass, flaky; anything else, a disagreement, which
    // suspends the check's reuse for the repository until a person turns it on again.
    if let Some((o, _)) = observed.as_mut() {
        let store = unit_store(backend, code, job.as_deref());
        for (unit, state) in &again_states {
            let Some(key) = o.again.get(unit).copied() else {
                continue;
            };
            if *state == hushrun_core::units::State::Passed {
                say.line(format_args!(
                    "{check}: {unit} failed, then passed when it ran again: it may be flaky"
                ));
                continue;
            }
            o.disagree(unit, key);
            let by = crate::coding_agent::agent(&|name| std::env::var(name).ok())
                .map_or_else(|| "a person".to_string(), |agent| format!("agent:{agent}"));
            let suspended = match store.as_deref() {
                Some(store) => store.suspend(unit, &key, &code.key, &by),
                None => Err("there is nowhere to keep it".into()),
            };
            match suspended {
                Ok(_) => say.line(format_args!(
                    "{check}: {unit} failed again, though it passed before on the same files it read: reuse of {check}'s test files is suspended until a person turns it on again with `hushrun tests resume {check}`"
                )),
                Err(why) => say.line(format_args!(
                    "{check}: {unit} failed again, though it passed before on the same files it read; reuse of {check}'s test files couldn't be suspended: {why}"
                )),
            }
        }
    }
    let mut result = code.result(
        check,
        command,
        verdict,
        status.code(),
        started_at,
        duration_ms,
        None,
        met.as_ref()
            .map(|(_, tools)| tools.clone())
            .unwrap_or_default(),
        reports,
        anyway,
    );
    result.units = observed.as_ref().and_then(|(o, _)| o.counts.clone());
    if let (Some(counts), Some(word)) = (result.units.as_mut(), units_anyway) {
        counts.ran_anyway = Some(word);
    }
    // \[ADR 0069\] Unit results go with the run that executed them: to a server in the same
    // request, else beside the run's result on this machine, once it was recorded (C-33).
    let unit_records: &[units::store::UnitRecord] =
        observed.as_ref().map_or(&[], |(o, _)| o.records());
    let mut units_kept = false;
    let recorded = if backend.is_server() {
        backend
            .record_with_units(&code.key, &result, unit_records)
            .map(|kept| {
                units_kept = kept;
                if !kept {
                    say.line(format_args!(
                        "{check}: test files' results weren't kept: {} {}",
                        backend.org().unwrap_or_default(),
                        "doesn't keep them yet, so no test file is reused"
                    ));
                }
            })
    } else {
        backend.record(&code.key, &result)
    };
    if let Some((o, tree)) = &observed {
        o.keep(&code.key, tree, now(global));
        if recorded.is_ok()
            && !backend.is_server()
            && let Some(store) =
                units::local_store(code.repository_here().as_deref(), check.as_str())
            && let Err(err) = store.record(&code.key, &result, o.records())
        {
            tracing::debug!("the test files' results weren't kept: {err:#}");
        }
        // \[C-38\] What this machine saw its test files read goes to the server that kept their
        // results, for a machine with an empty cache; a server from before hints is said once.
        if recorded.is_ok() && units_kept {
            if backend.keeps_no_hints() {
                say.line(format_args!(
                    "{check}: {} doesn't keep what other machines saw its test files read yet, so this machine goes by what it saw itself",
                    backend.org().unwrap_or_default()
                ));
            } else if let Some(store) = unit_store(backend, code, job.as_deref())
                && let Some(line) = o.share(store.as_ref(), now(global))
            {
                say.line(format_args!("{line}"));
            }
        }
    }
    let mark = match (&recorded, verdict) {
        (Ok(()), _) => Some(Ended::Recorded),
        (Err(_), Verdict::Pass) => Some(Ended::Unrecorded),
        // Its mark went with the failure, above.
        (Err(_), _) => None,
    };
    if let Some(mark) = mark {
        let _ = results::ran_here(&code.key, mark);
    }
    match recorded {
        Ok(()) if verdict == Verdict::Pass && met.is_some() => {
            let on = met.as_ref().map_or("", |(line, _)| line.as_str());
            say.line(format_args!(
                "{check} passed in {} on {on}{summary}; it counts{flaky}",
                took(duration_ms)
            ));
        }
        // This job runs it every time, so it says nothing of the next run.
        Ok(())
            if verdict == Verdict::Pass
                && (no_pass_skips.is_some()
                    || reports_read.is_some_and(|read| {
                        still_read(read, &result, job.as_deref(), code).is_some()
                    })) =>
        {
            say.line(format_args!("{outcome}{flaky}"));
        }
        Ok(()) if verdict == Verdict::Pass => {
            say.line(format_args!(
                "{outcome}; it won't run again on this code{flaky}"
            ));
        }
        Ok(()) => say.line(format_args!("{outcome}{flaky}")),
        Err(err) => match backend.refusal() {
            // ADR 0028: no machine signed it, or the server refused the machine: what to do.
            Some(
                Refusal::Credential(why)
                | Refusal::Limited(why)
                | Refusal::Machine(why)
                | Refusal::Invalid(why),
            ) => {
                say.line(format_args!("{outcome}; not recorded: {why}{flaky}"));
            }
            // ADR 0032: the org's plan records no more checks this period; it says where to
            // upgrade (C-14).
            Some(Refusal::Quota(quota)) => {
                say.line(format_args!(
                    "{outcome}; not recorded: {}{flaky}",
                    quota.line()
                ));
            }
            Some(Refusal::NoToken(_)) => say.line(format_args!("{outcome}; not recorded{flaky}")),
            None => say.line(format_args!(
                "{check} {ended} in {}, but it couldn't be recorded ({err:#}){flaky}",
                took(duration_ms)
            )),
        },
    }
    ExitCode::from(exit)
}

/// B-31: the commit a run's build requests name, decided once per run: the workspace's; in CI
/// none when the files git tracks differ from it here (a version bump, a nightly `cargo
/// update`), since the code built isn't that commit's, and the backend counts a request that
/// names none as one whose commit isn't the token's. None either when git can't tell.
fn build_commit(repo: &BuildRepo, in_ci: bool) -> Option<String> {
    let revision = repo.revision.clone()?;
    if !in_ci || tracked_files_are(&repo.root, &revision) {
        Some(revision)
    } else {
        None
    }
}

/// B-25: what sharing build blocks needs of the repository, which they belong to whatever the
/// scope: read with the run's [`Code`], or, for a run that can't tell which code it is (several
/// scopes and no `--scope`), from the repository alone.
struct BuildRepo {
    root: PathBuf,
    revision: Option<String>,
    /// `host/owner/name`, when hushrun.yaml or the `origin` remote says.
    repository: Option<String>,
    /// The repository's `build: env:` (B-35).
    build_env: Vec<String>,
    /// No results lookup asks the backend whether the org shares (the run can't tell which code
    /// it is): `open` does, and an org that doesn't share is as silent as ever.
    open_decides: bool,
}

impl BuildRepo {
    fn of(code: &Code) -> BuildRepo {
        BuildRepo {
            root: code.root.clone(),
            revision: code.revision.clone(),
            repository: code.repository.clone(),
            build_env: code.build_env.clone(),
            open_decides: false,
        }
    }

    /// The repository at or above the start directory, at `--rev` or as the worktree is, with
    /// no scope chosen.
    fn read(global: &GlobalOpts, args: &RunArgs) -> anyhow::Result<BuildRepo> {
        let snapshot = match &args.content.rev {
            Some(rev) => Snapshot::Rev(rev.clone()),
            None => Snapshot::Worktree,
        };
        let content = Content::open(global, Source::Repo(snapshot)).map_err(|f| f.error)?;
        Ok(BuildRepo {
            root: content.vcs.root().to_path_buf(),
            revision: content.source_info().revision,
            repository: repository_of(&content),
            build_env: content
                .policy
                .as_ref()
                .map(|policy| policy.build_env.clone())
                .unwrap_or_default(),
            open_decides: true,
        })
    }

    /// The repository a run's blocks are shared for, as [`Code::repository_here`] says it.
    fn repository_here(&self) -> Option<String> {
        hushrun_ci::context(&Env::from_process())
            .repository
            .or_else(|| self.repository.clone())
    }
}

/// Whether every file git tracks in the checkout at `root` is as `commit` has it: changed,
/// deleted, staged and added ones all differ; a file git doesn't track, and changes inside a
/// submodule's own checkout, are not compared.
fn tracked_files_are(root: &Path, commit: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "diff",
            "--quiet",
            "--no-ext-diff",
            "--ignore-submodules=dirty",
            commit,
            "--",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// B-2, B-20: a run's build session, begun once before COMMAND first starts. Every start of
/// COMMAND gets its environment, and it is finished once, after the last: by [`BuildRun::finish`]
/// where the run's lines go, else when the run returns before there, whatever COMMAND's exit
/// codes. Nothing in it changes how COMMAND ends or what is recorded.
struct BuildRun<'a> {
    session: Option<Session>,
    global: &'a GlobalOpts,
    say: &'a Say,
}

impl<'a> BuildRun<'a> {
    /// Begins the run's session, when reuse is on here. With none, COMMAND runs as usual, and
    /// only a line worth reading is said (B-19).
    fn begin(
        global: &'a GlobalOpts,
        say: &'a Say,
        check: &CheckName,
        repo: Option<&BuildRepo>,
        backend: Option<&Backend>,
    ) -> BuildRun<'a> {
        // B-26: the run's results lookup told whether the org shares build blocks; a run that
        // made none lets `open` tell.
        let access = backend.and_then(Backend::build_access);
        let unread = repo.is_none() && access.as_ref().is_some_and(|a| a.credential.is_some());
        let share = match (repo, access) {
            (Some(repo), Some(access)) => Some(build_session::ShareStart {
                access,
                commit: build_commit(repo, hushrun_ci::in_ci(&Env::from_process())),
                check: check.as_str().to_string(),
                open_decides: repo.open_decides,
            }),
            _ => None,
        };
        let session = match build_session::begin(
            system_time(now(global)),
            repo.and_then(BuildRepo::repository_here).as_deref(),
            repo.map_or(&[][..], |repo| &repo.build_env),
            share,
            &|| backend.and_then(Backend::build_place),
        ) {
            Ok(mut session) => {
                // The one case it can't share: no repository to share for.
                if unread {
                    session.add_line(
                        "build: sharing is off for this run: it can't read the repository"
                            .to_string(),
                    );
                }
                Some(session)
            }
            Err(why) => {
                if let Some(why) = why {
                    say.line(format_args!("{why}"));
                }
                None
            }
        };
        BuildRun {
            session,
            global,
            say,
        }
    }

    /// What a start of COMMAND gets: the wrapper and the session; nothing without a session.
    fn env(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        self.session.as_ref().map(Session::env).unwrap_or_default()
    }

    /// Sets the wrapper and the session on a start of COMMAND, when there is a session.
    fn apply(&self, command: &mut Command) {
        command.envs(self.env());
    }

    /// Ends the session, once, and says what its compiles came to.
    fn finish(&mut self) {
        if let Some(session) = self.session.take() {
            for line in build_session::finish(session, system_time(now(self.global))).lines() {
                self.say.line(format_args!("{line}"));
            }
        }
    }
}

impl Drop for BuildRun<'_> {
    fn drop(&mut self) {
        self.finish();
    }
}

/// The newest result of `key` that counts in the CI job `job` and its `variant`, for a run that
/// decides without asking: what a run that asked would have found. One request; none when the
/// backend can't say.
fn counting(
    backend: &Backend,
    key: &Digest,
    job: Option<&str>,
    variant: Option<&str>,
) -> Option<RunResult> {
    let latest = backend.latest_for_job(key, job, variant).ok()?;
    latest.result.filter(|last| last.counts_in(job, variant))
}

/// Whether `found` is a pass, which would have skipped the run.
fn passed(found: Option<&RunResult>) -> bool {
    found.is_some_and(|last| last.result == Verdict::Pass)
}

/// G13: what the line that ends a run adds when its verdict isn't the one of `decided`, the result
/// that decided here: it may be flaky. Nothing else is recorded: the server tells flaky checks
/// from the results it has.
fn flaky(decided: Option<&RunResult>, passed: bool) -> &'static str {
    match decided.map(|last| last.result) {
        Some(Verdict::Pass) if !passed => {
            "; it passed on this exact code before, so it may be flaky"
        }
        Some(Verdict::Fail) if passed => {
            "; it failed on this exact code before, so it may be flaky"
        }
        _ => "",
    }
}

/// The step summary of a run that may be flaky: its line.
fn flaky_summary(check: &CheckName, line: &str) -> String {
    format!(
        "### {} may be flaky\n\n{}\n",
        markdown(check.as_str()),
        markdown(line)
    )
}

/// \[ADR 0049\] Why `last`, a pass of this exact code, doesn't count in the CI job `job` (C-10,
/// C-14): it is another variant's of this job, or from outside CI, which no variant ran as, or
/// another job's.
fn elsewhere(last: &RunResult, job: Option<&str>) -> &'static str {
    let outside = last.provider == Provider::Local
        || (last.provider == Provider::Generic && last.job.is_none());
    if job.is_some() && last.job.as_deref() == job {
        "this exact code passed in another variant of this job, and each variant counts only its own passes"
    } else if outside {
        "this exact code passed outside CI, and a job with several variants counts only its own variant's passes"
    } else {
        "this exact code passed in another CI job, and a CI pass counts in its own job"
    }
}

/// \[ADR 0047\] Why `pass`, a pass that counts here, doesn't skip the check when something in this
/// job reads reports, `read` (C-26): \[ADR 0048\] only a reader that reads what the pass wrote,
/// when its record can say, else any. `None` when none does.
fn still_read(
    read: &variants::ReportsRead,
    pass: &RunResult,
    job: Option<&str>,
    code: &Code,
) -> Option<variants::NoSkip> {
    match wrote_here(pass, job, code, read) {
        Some(files) => read.despite(files),
        None => Some(read.no_skip()),
    }
}

/// \[ADR 0048\] Every report file `pass` wrote, when that can narrow the readers rule here (C-26):
/// it is this very job's pass on GitHub, recorded by this very hushrun, whose record names every
/// report file it wrote; this checkout is the job's workspace, where the readers' paths start; and
/// the files the readers are read from are in the check's scope, so that its key changes with
/// them. `None` otherwise.
fn wrote_here<'a>(
    pass: &'a RunResult,
    job: Option<&str>,
    code: &Code,
    read: &variants::ReportsRead,
) -> Option<&'a [hushrun_reports::File]> {
    let reports = pass
        .reports
        .as_ref()
        .filter(|reports| reports.files_complete)?;
    let this_job = pass.provider == Provider::Github && job.is_some() && pass.job.as_deref() == job;
    let real = |path: &Path| std::fs::canonicalize(path).ok();
    let workspace = hushrun_ci::var("GITHUB_WORKSPACE")
        .ok()
        .and_then(|dir| real(Path::new(&dir)));
    let here = workspace.is_some() && workspace == real(&code.root);
    let in_scope = read.sources.iter().all(|path| code.in_scope(path));
    (this_job && pass.tool == tool() && here && in_scope).then_some(reports.files.as_slice())
}

/// What a result says recorded it: hushrun, and this version.
fn tool() -> String {
    format!("{} {}", hushrun_core::PRODUCT, env!("CARGO_PKG_VERSION"))
}

/// \[ADR 0048\] `: <summary>`, what a run's line says of its reports after its time; nothing
/// without a summary (C-27).
fn said(reports: Option<&Reports>) -> String {
    reports
        .and_then(Reports::summary)
        .map_or_else(String::new, |summary| format!(": {summary}"))
}

/// \[ADR 0048\] GitHub's outputs of a run or a skip (C-27): `hushrun-result`, `hushrun-skipped`,
/// and the summary's tests, failed tests and lines covered, empty without one, so that the last
/// run of a step that runs several says it all.
fn write_outputs(runner: &Runner, verdict: Verdict, reports: Option<&Reports>) {
    let tests = reports.and_then(|r| r.tests.as_ref());
    let lines = reports
        .and_then(|r| r.coverage.as_ref())
        .and_then(|c| c.lines.or(c.statements));
    let mut outputs = Outputs::new();
    outputs.set(
        "hushrun-result",
        match verdict {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Skip => "skip",
        },
    );
    outputs.set("hushrun-skipped", (verdict == Verdict::Skip).to_string());
    outputs.set(
        "hushrun-tests",
        tests.map_or_else(String::new, |t| t.total.to_string()),
    );
    outputs.set(
        "hushrun-tests-failed",
        tests.map_or_else(String::new, |t| {
            t.failed.saturating_add(t.errors).to_string()
        }),
    );
    outputs.set(
        "hushrun-coverage-lines",
        lines.map_or_else(String::new, |ratio| {
            hushrun_reports::percent(ratio)
                .trim_end_matches('%')
                .to_string()
        }),
    );
    if let Err(err) = runner.outputs(&outputs) {
        tracing::debug!("the step outputs weren't written: {err:#}");
    }
}

/// \[ADR 0048\] GitHub's step summary of a skip (C-27): when and where this exact code passed,
/// linked to its run only when the pass is this repository's, since a pass can come from another
/// of the org's, what the skip saved, and the pass's summary in the columns that have values.
fn skip_summary(check: &CheckName, last: &RunResult, when: &str, here: Option<&str>) -> String {
    let place = if last.provider == Provider::Local {
        ""
    } else {
        " in CI"
    };
    let link = match (&last.run_url, &last.repository, here) {
        (Some(url), Some(theirs), Some(ours))
            if theirs == ours
                && url.starts_with("https://")
                && !url.contains(|c: char| c.is_whitespace() || "()<>[]".contains(c)) =>
        {
            format!(" ([its run]({url}))")
        }
        _ => String::new(),
    };
    let mut text = format!(
        "### {} skipped\n\nThis exact code passed{place} {when}{link}, so this run saved {}.\n",
        markdown(check.as_str()),
        took(last.duration_ms)
    );
    if let Some(reports) = &last.reports {
        text.push_str(&summary_table(reports));
    }
    text
}

/// The table of a summary, in the columns that have values; nothing for none.
fn summary_table(reports: &Reports) -> String {
    let covered = |ratio: Ratio| {
        format!(
            "{} ({} of {})",
            hushrun_reports::percent(ratio),
            hushrun_reports::count(ratio.covered),
            hushrun_reports::count(ratio.total)
        )
    };
    let mut columns: Vec<(&str, String)> = Vec::new();
    if let Some(tests) = &reports.tests {
        columns.push(("Tests", hushrun_reports::count(tests.total)));
        columns.push((
            "Failed",
            hushrun_reports::count(tests.failed.saturating_add(tests.errors)),
        ));
        columns.push(("Skipped", hushrun_reports::count(tests.skipped)));
    }
    if let Some(coverage) = &reports.coverage {
        if let Some(lines) = coverage.lines {
            columns.push(("Lines covered", covered(lines)));
        } else if let Some(statements) = coverage.statements {
            columns.push(("Statements covered", covered(statements)));
        }
        if let Some(branches) = coverage.branches {
            columns.push(("Branches covered", covered(branches)));
        }
    }
    if columns.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = columns.iter().map(|(name, _)| *name).collect();
    let values: Vec<&str> = columns.iter().map(|(_, value)| value.as_str()).collect();
    format!(
        "\n| {} |\n|{}\n| {} |\n",
        names.join(" | "),
        "---:|".repeat(columns.len()),
        values.join(" | ")
    )
}

/// `text` as Markdown shows it literally.
fn markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\`*_{}[]()#+-.!|<>~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The check's name and its command, or why this isn't a run of one command.
fn parse<'a>(global: &GlobalOpts, args: &'a RunArgs) -> anyhow::Result<(CheckName, &'a [String])> {
    let theirs = [
        (args.all, "--all"),
        (args.isolated, "--isolated"),
        (args.timeout.is_some(), "--timeout"),
        (args.environment_image.is_some(), "--environment-image"),
        (global.plan.is_some(), "--plan"),
    ];
    if let Some((_, flag)) = theirs.iter().find(|(given, _)| *given) {
        return Err(anyhow!(
            "{flag} goes with hushrun.yaml's checks: drop the command after --, as in hushrun run --all"
        ));
    }
    let given = match args.checks.as_slice() {
        [] => None,
        [check] => Some(check),
        _ => {
            return Err(anyhow!(
                "name one check, then its command after --: hushrun run unit -- vitest run"
            ));
        }
    };
    let command = args.command.as_slice();
    let Some(program) = command.first() else {
        return Err(match given {
            Some(check) => {
                anyhow!("give {check}'s command after --: hushrun run {check} -- vitest run")
            }
            None => anyhow!("give the check's command after --: hushrun run -- vitest run"),
        });
    };
    let check = match given {
        Some(check) => check.clone(),
        None => {
            let script = std::env::var("npm_lifecycle_event").ok();
            derived_name(script.as_deref(), program).ok_or_else(|| {
                anyhow!("name the check, as {program} gives no name: hushrun run NAME -- {program}")
            })?
        }
    };
    Ok((check, command))
}

/// The check's name when the command line gives none (C-9): the script that runs it, as npm,
/// pnpm, yarn and bun name it in `npm_lifecycle_event`, made a name; else `program`'s file name
/// without its extension, made one. So `"test": "hushrun run -- vitest run"` in package.json
/// names its check `test`, `"test:unit": …` names it `test-unit`, and `hushrun run -- vitest
/// run` at a prompt names it `vitest`.
pub(crate) fn derived_name(script: Option<&str>, program: &str) -> Option<CheckName> {
    // npx and `npm exec` run what they start as the script `npx`, which names nothing.
    if let Some(name) = script
        .map(str::trim)
        .filter(|script| *script != "npx")
        .and_then(made_name)
    {
        return Some(name);
    }
    let file = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let stem = match file.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => file,
    };
    made_name(stem)
}

/// `text` made a name (L-2: a lowercase letter first, then lowercase letters, digits, `_` and
/// `-`): in ASCII lowercase, the characters before its first letter dropped, each run of other
/// characters one `-`, 63 characters at most and no trailing `-`; `None` when nothing is left.
pub(crate) fn made_name(text: &str) -> Option<CheckName> {
    let mut name = String::new();
    for c in text.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_lowercase() || (!name.is_empty() && (c.is_ascii_digit() || c == '_')) {
            name.push(c);
        } else if !name.is_empty() && !name.ends_with('-') {
            name.push('-');
        }
    }
    name.truncate(hushrun_core::names::MAX_NAME_LEN);
    CheckName::new(name.trim_end_matches('-')).ok()
}

/// What decides whether an earlier run stands in for this one.
struct Code {
    key: Digest,
    /// The check the key is for.
    check: CheckName,
    scope: ScopeName,
    digest: Digest,
    /// \[ADR 0037\] What the scope declares outside the repository.
    external: Vec<ExternalInput>,
    /// \[ADR 0045\] What the check needs of where it runs: part of `key`.
    rule: Option<Rule>,
    /// The repository's top directory, whose package.json may name a backend (C-11).
    root: PathBuf,
    cwd: String,
    revision: Option<String>,
    /// `host/owner/name`, when hushrun.yaml or the `origin` remote says.
    repository: Option<String>,
    /// Whether the files can change under the command: they can in the worktree, not at a
    /// revision or under `--digest`.
    live: bool,
    /// `HUSHRUN_INVOKED_BY`'s label.
    invoked_by: Option<String>,
    /// \[ADR 0047\] `reports.ci: skip`: the repository's readers handle a skip (C-26).
    reports_skip: bool,
    /// \[ADR 0048\] `reports.files`: report files it writes, besides those hushrun finds (C-27).
    reports_files: Vec<String>,
    /// Every scope, to tell which files are in this one's.
    scopes: Scopes,
    /// \[ADR 0049\] Which variant of its CI job this run is, when the job has several and says
    /// which (C-14): its result says it, and only that variant's passes skip it.
    variant: Option<results::Variant>,
    /// \[ADR 0068\] The check's `tests`, when they aren't off (C-5).
    tests: Option<hushrun_core::units::Tests>,
    /// The repository's `build: env:` (B-35): variables every compile is keyed by.
    build_env: Vec<String>,
}

impl Code {
    fn read(
        global: &GlobalOpts,
        args: &RunArgs,
        check: &CheckName,
        command: &[String],
        dir: &Path,
        invoked_by: Option<String>,
    ) -> anyhow::Result<Code> {
        let current = current(global, args)?;
        let cwd = relative(&current.root, dir)?;
        let (key, rule) = results::run_key(
            current.policy.as_ref(),
            check,
            command,
            &cwd,
            &current.scope,
            &current.digest,
            &current.external,
        )?;
        let reports = current
            .policy
            .as_ref()
            .and_then(|policy| policy.reports.get(check));
        let reports_skip = reports.is_some_and(|reports| reports.ci == ReportsCi::Skip);
        let reports_files = reports
            .map(|reports| reports.files.clone())
            .unwrap_or_default();
        let tests = current
            .policy
            .as_ref()
            .and_then(|policy| policy.tests.get(check))
            .cloned();
        let build_env = current
            .policy
            .as_ref()
            .map(|policy| policy.build_env.clone())
            .unwrap_or_default();
        Ok(Code {
            key,
            check: check.clone(),
            scope: current.scope,
            digest: current.digest,
            external: current.external,
            rule,
            root: current.root,
            cwd,
            revision: current.revision,
            repository: current.repository,
            live: args.content.rev.is_none() && args.content.digest.is_none(),
            invoked_by,
            reports_skip,
            reports_files,
            scopes: current.scopes,
            variant: None,
            tests,
            build_env,
        })
    }

    /// What the backend keeps of a run, or of a skip: `saved_ms` is how long the run it stood
    /// in for took, and `tools` what a rule made hushrun measure. The CI provider names the
    /// repository when it can. \[ADR 0050\] Its `command` is `command` with every word that holds
    /// a secret redacted (C-10); the key, which [`Code::read`] made from `command` itself, stays
    /// as it is.
    #[allow(clippy::too_many_arguments)]
    fn result(
        &self,
        check: &CheckName,
        command: &[String],
        verdict: Verdict,
        exit_code: Option<i32>,
        started_at: Time,
        duration_ms: u64,
        saved_ms: Option<u64>,
        tools: BTreeMap<String, String>,
        reports: Option<Reports>,
        ran_anyway: Option<RanAnyway>,
    ) -> RunResult {
        let ci: RunContext = hushrun_ci::context(&Env::from_process());
        let vars = variables();
        RunResult {
            schema: results::RESULT_SCHEMA.into(),
            check: check.clone(),
            result: verdict,
            exit_code,
            command: redacted(command, vars.iter().map(|(n, v)| (n.as_str(), v.as_str()))),
            cwd: self.cwd.clone(),
            scope: self.scope.clone(),
            digest: self.digest,
            external: self.external.clone(),
            revision: self.revision.clone(),
            repository: ci.repository.or_else(|| self.repository.clone()),
            started_at,
            duration_ms,
            saved_ms,
            provider: ci.provider,
            job: hushrun_ci::result_job(&Env::from_process()),
            variant: self.variant.clone(),
            run_url: ci.url,
            tool: tool(),
            invoked_by: self.invoked_by.clone(),
            environment: Some(RunEnvironment {
                platform: hushrun_ci::platform(),
                tools,
            }),
            reports,
            ran_anyway: ran_anyway.map(|why| why.word().to_string()),
            units: None,
        }
    }

    /// Whether the file at `path`, from the root, is in the check's scope (D-7), so that its key
    /// changes when the file does.
    fn in_scope(&self, path: &str) -> bool {
        self.scopes
            .get(self.scope.as_str())
            .is_some_and(|scope| self.scopes.contains(scope, path))
    }

    /// The repository a result recorded here names, as [`Code::result`] says it.
    fn repository_here(&self) -> Option<String> {
        hushrun_ci::context(&Env::from_process())
            .repository
            .or_else(|| self.repository.clone())
    }

    /// \[ADR 0048\] What a result keeps of the reports a run of `command` in `dir`, started at
    /// `started`, wrote (C-27): `None` when the search went past its bounds, or found only what it
    /// couldn't read.
    fn reports_of(
        &self,
        dir: &Path,
        started: SystemTime,
        command: &[String],
        readers: &[String],
    ) -> Option<Reports> {
        let query = hushrun_reports::Query {
            root: &self.root,
            cwd: dir,
            started,
            command,
            declared: &self.reports_files,
            readers,
        };
        let search = hushrun_reports::find(&query)?;
        hushrun_reports::record(&search, &hushrun_reports::Limits::default())
    }

    /// Whether the files are still the ones the command started on.
    fn unchanged(&self, global: &GlobalOpts, args: &RunArgs) -> anyhow::Result<bool> {
        if !self.live {
            return Ok(true);
        }
        let now = current(global, args)?;
        Ok(now.digest == self.digest && now.external == self.external)
    }
}

/// \[ADR 0068\] Starts observing a run's test files, when its check's `tests` isn't off: says
/// once why it can't, and that `reuse` observes in this version (C-5, C-32).
fn observe_tests(
    say: &Say,
    check: &CheckName,
    code: &Code,
    command: &[String],
    dir: &Path,
) -> Option<Box<units::Session>> {
    let tests = code.tests.as_ref()?;
    if !code.live {
        say.line(format_args!(
            "{check}: test files aren't observed: --rev and --digest name other files than these"
        ));
        return None;
    }
    let session = match units::Session::start(Some(tests), command, &code.root, dir) {
        units::Start::Off => return None,
        units::Start::Not(why) => {
            say.line(format_args!("{check}: test files aren't observed: {why}"));
            return None;
        }
        units::Start::On(session) => session,
    };
    Some(session)
}

/// \[ADR 0068\] What a session saw, decided, with the scope's files it ran on; `None` when the
/// adapter wrote nothing, or the files aren't the ones the run's key is for.
#[allow(clippy::too_many_arguments)]
fn observed(
    global: &GlobalOpts,
    session: &units::Session,
    code: &Code,
    backend: &Backend,
    command: &[String],
    dir: &Path,
    passed: bool,
    job: Option<&str>,
) -> Option<(units::Observed, hushrun_core::units::Tree)> {
    let (digest, tree) = scope_tree(global, &code.scope)
        .map_err(|err| tracing::debug!("the scope's files can't be read again: {err:#}"))
        .ok()?;
    if digest != code.digest {
        return None;
    }
    let store = unit_store(backend, code, job)?;
    let variant = code.variant.as_ref().map(|v| v.key.as_str());
    let counts = |run: &RunResult| run.counts_in(job, variant);
    let repository = code.repository_here();
    let end = units::Finish {
        base: key_base(code, command),
        repository: repository.as_deref(),
        tree: &tree,
        command_passed: passed,
        counts: &counts,
        store: store.as_ref(),
        root: &code.root,
        cwd: dir,
        mode: code.tests.as_ref().map_or(Mode::Off, |tests| tests.mode),
    };
    session.finish(&end).map(|observed| (observed, tree))
}

/// What every unit key holds of the run's key (C-31).
fn key_base(code: &Code, command: &[String]) -> hushrun_core::units::KeyBase {
    hushrun_core::units::KeyBase {
        check: code.check.clone(),
        command: command.to_vec(),
        cwd: code.cwd.clone(),
        scope: code.scope.clone(),
        external: (!code.external.is_empty()).then(|| serde_json::json!(code.external)),
        counts_from: code.rule.as_ref().map(Rule::normal),
    }
}

/// \[ADR 0069\] Where this run's test files' results are found, with its check's reuse state:
/// the server's, asked as this job and variant, or this machine's (C-33, C-34).
fn unit_store<'a>(
    backend: &'a Backend,
    code: &Code,
    job: Option<&str>,
) -> Option<Box<dyn units::store::UnitResults + 'a>> {
    let repository = code.repository_here();
    if backend.is_server() {
        let base = match backend.place() {
            results::Place::Server { base, .. } => base,
            results::Place::Directory { .. } => return None,
        };
        return Some(Box::new(units::store::Server {
            backend,
            base,
            check: code.check.to_string(),
            repository,
            job: job.map(str::to_string),
            variant: code.variant.as_ref().map(|v| v.key.clone()),
            cache: context::cache_dir()?,
            read_only: false,
        }));
    }
    units::local_store(repository.as_deref(), code.check.as_str())
        .map(|store| Box::new(store) as Box<dyn units::store::UnitResults>)
}

/// \[ADR 0069\] The plan, for a check in `reuse` (RFC 0003 §3.5 step 1): on the scope's files as
/// they are now, which must be the ones the run's key is for; `None` when they aren't.
#[allow(clippy::too_many_arguments)]
fn plan_units(
    global: &GlobalOpts,
    session: &mut units::Session,
    code: &Code,
    backend: &Backend,
    command: &[String],
    dir: &Path,
    job: Option<&str>,
    started: Time,
) -> Option<units::Planned> {
    let (digest, tree) = scope_tree(global, &code.scope).ok()?;
    if digest != code.digest {
        return None;
    }
    let store = unit_store(backend, code, job)?;
    let variant = code.variant.as_ref().map(|v| v.key.as_str());
    let counts = |run: &RunResult| run.counts_in(job, variant);
    let started = started.to_string();
    let repository = code.repository_here();
    let input = units::PlanInput {
        base: key_base(code, command),
        repository: repository.as_deref(),
        tree: &tree,
        store: store.as_ref(),
        counts: &counts,
        root: &code.root,
        cwd: dir,
        started: &started,
    };
    Some(session.plan(&input))
}

/// Whether this run is inside another `hushrun run`.
fn inside_run() -> bool {
    Env::from_process().get(variants::INSIDE_A_RUN).is_some()
}

/// The word a record says for why a job's variants stop a skip, or test files' reuse.
fn ran_anyway_word(no: &variants::NoSkip, inside: bool) -> &'static str {
    match no.wall {
        variants::Wall::Variants if inside => RanAnyway::InsideRun.word(),
        variants::Wall::Variants => RanAnyway::Variants.word(),
        variants::Wall::Reusable => RanAnyway::Reusable.word(),
        variants::Wall::Reports => RanAnyway::Reports.word(),
        variants::Wall::CantTell => RanAnyway::CantTell.word(),
    }
}

/// The scope's digest and files in the worktree now, as `git add -A && git commit` would record
/// them (D-10).
pub(crate) fn scope_tree(
    global: &GlobalOpts,
    scope: &ScopeName,
) -> anyhow::Result<(Digest, hushrun_core::units::Tree)> {
    let content = Content::open(global, Source::Repo(Snapshot::Worktree)).map_err(|f| f.error)?;
    let selected = content
        .scopes
        .get(scope.as_str())
        .ok_or_else(|| anyhow!("unknown scope `{scope}`"))?;
    let run = hushrun_vcs::digest_scopes(
        content.vcs.as_ref(),
        &content.listing,
        &content.scopes,
        &[selected],
        false,
    )?;
    let digested = &run.scopes[0];
    let tree = hushrun_core::units::Tree::from_manifest(&digested.manifest)
        .ok_or_else(|| anyhow!("the scope's manifest doesn't read"))?;
    Ok((digested.digest, tree))
}

/// \[ADR 0050\] What a record shows in place of a word that holds a secret (C-10).
const REDACTED: &str = "[redacted]";

/// \[ADR 0050\] What a variable's name holds, in any case, when its value is a secret.
const SECRET_NAMES: [&str; 10] = [
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASS",
    "KEY",
    "CREDENTIAL",
    "AUTH",
    "PRIVATE",
    "CERT",
];

/// \[ADR 0050\] The fewest bytes such a variable's value has for a word equal to it to be
/// redacted: a shorter one, such as `1` or `true`, is too common a word to give anything away.
const SECRET_BYTES: usize = 8;

/// \[ADR 0050\] How a token starts that a word is redacted for, whatever the variables say:
/// GitHub's, GitLab's and a PEM block's. Slack's, AWS's and `sk-` keys are in [`token_shaped`].
const TOKEN_STARTS: [&str; 8] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "-----BEGIN",
];

/// \[ADR 0050\] COMMAND as its record shows it (C-10): a word that is a secret is `[redacted]`, and
/// so is what follows an `=` in a word when that is one, as in `NAME=value` or `--token=value`,
/// with what comes before the first such `=` kept. A secret is the value, of 8 bytes or more, of
/// one of `vars` whose name says it is one, or anything shaped like a token. Only the record
/// changes: the key is made from COMMAND itself (C-9), so what skips stays as it was.
fn redacted<'a>(
    command: &[String],
    vars: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<String> {
    let values: BTreeSet<&str> = vars
        .into_iter()
        .filter(|(name, value)| value.len() >= SECRET_BYTES && names_a_secret(name))
        .map(|(_, value)| value)
        .collect();
    let secret = |text: &str| values.contains(text) || token_shaped(text);
    command
        .iter()
        .map(|word| {
            if secret(word) {
                return REDACTED.to_string();
            }
            match word
                .match_indices('=')
                .find(|(at, _)| secret(&word[at + 1..]))
            {
                Some((at, _)) => format!("{}{REDACTED}", &word[..=at]),
                None => word.clone(),
            }
        })
        .collect()
}

/// Whether a variable's name says its value is a secret, in any case.
fn names_a_secret(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    SECRET_NAMES.iter().any(|part| name.contains(part))
}

/// Whether `text` starts like a token: one of [`TOKEN_STARTS`]; Slack's, `xox`, one of `abprs`
/// and `-`; an AWS access key's, `AKIA` and 16 uppercase letters or digits; or an API key's, `sk-`
/// and 20 or more letters, digits, `-` or `_`.
fn token_shaped(text: &str) -> bool {
    // Whether `rest` starts with `n` bytes that are all `of`.
    let starts =
        |rest: &str, n: usize, of: fn(u8) -> bool| rest.len() >= n && rest.bytes().take(n).all(of);
    let aws = |b: u8| b.is_ascii_uppercase() || b.is_ascii_digit();
    let api = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    let slack = text.strip_prefix("xox").is_some_and(|rest| {
        matches!(
            rest.as_bytes(),
            [b'a' | b'b' | b'p' | b'r' | b's', b'-', ..]
        )
    });
    TOKEN_STARTS.iter().any(|start| text.starts_with(start))
        || slack
        || text
            .strip_prefix("AKIA")
            .is_some_and(|rest| starts(rest, 16, aws))
        || text
            .strip_prefix("sk-")
            .is_some_and(|rest| starts(rest, 20, api))
}

/// The process's variables, by name, for [`redacted`]: a value that isn't UTF-8 can't be a word
/// of COMMAND, so it is left out.
fn variables() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(name, value)| {
            Some((
                name.to_string_lossy().into_owned(),
                value.into_string().ok()?,
            ))
        })
        .collect()
}

/// The selected scope's digest right now, with what it was computed from.
struct Current {
    scope: ScopeName,
    /// Every scope of the content.
    scopes: Scopes,
    digest: Digest,
    external: Vec<ExternalInput>,
    root: PathBuf,
    revision: Option<String>,
    repository: Option<String>,
    /// `hushrun.yaml` as this content reads it: the file at `--rev`, `--config`'s or
    /// `--config-ref`'s. It alone says the rule, so a key and what a run must meet can't differ.
    policy: Option<Policy>,
}

/// The worktree as `git add -A && git commit` would record it (D-10), or `--rev`; `--digest`
/// as given.
fn current(global: &GlobalOpts, args: &RunArgs) -> anyhow::Result<Current> {
    let snapshot = match &args.content.rev {
        Some(rev) => Snapshot::Rev(rev.clone()),
        None => Snapshot::Worktree,
    };
    let content = Content::open(global, Source::Repo(snapshot)).map_err(|f| f.error)?;
    let subject = content
        .subject(&args.content.scopes, args.content.digest)
        .map_err(|f| f.error)?;
    let repository = repository_of(&content);
    let external = content.external(&subject.scope)?;
    Ok(Current {
        scope: subject.scope,
        scopes: content.scopes.clone(),
        digest: subject.digest,
        external,
        root: content.vcs.root().to_path_buf(),
        revision: subject.source.revision,
        repository,
        policy: content.policy,
    })
}

/// The repository a result recorded here names (C-10), as [`Code::result`] takes it: the CI
/// provider's, else hushrun.yaml's, else the `origin` remote's, in the worktree; `None` outside
/// a repository. \[ADR 0032\] `hushrun repository usage` finds this repository's share by it
/// (C-19).
pub(crate) fn results_repository(global: &GlobalOpts) -> Option<String> {
    hushrun_ci::context(&Env::from_process())
        .repository
        .or_else(|| {
            let content = Content::open(global, Source::Repo(Snapshot::Worktree)).ok()?;
            repository_of(&content)
        })
}

/// hushrun.yaml's repository, else the `origin` remote's (C-10).
pub(crate) fn repository_of(content: &Content) -> Option<String> {
    match &content.policy {
        Some(policy) => Some(policy.repository.clone()),
        None => origin_repository(content.vcs.root()),
    }
}

/// Milliseconds since `start`.
fn elapsed(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// `dir` relative to the repository's `root`, `/`-separated: `""` at the root.
pub(crate) fn relative(root: &Path, dir: &Path) -> anyhow::Result<String> {
    let real = |path: &Path| {
        std::fs::canonicalize(path).with_context(|| format!("reading {}", path.display()))
    };
    let (root, dir) = (real(root)?, real(dir)?);
    let inside = dir.strip_prefix(&root).map_err(|_| {
        anyhow!(
            "{} is outside the repository at {}",
            dir.display(),
            root.display()
        )
    })?;
    Ok(inside
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

/// The command's time: `--now`, else the clock's.
pub(crate) fn now(global: &GlobalOpts) -> Time {
    global
        .now
        .as_deref()
        .and_then(|text| Time::parse(text).ok())
        .unwrap_or_else(|| Time::from_timestamp(jiff::Timestamp::now()))
}

/// What starts for `name`, the command's first word, with `path` for a PATH. On Windows, `Command`
/// tries a bare name only with `.exe`, so npm's shims, such as `node_modules\.bin\vitest.cmd`, are
/// never found; a name without an extension is looked up here as a shell would, in each PATH
/// directory with each of PATHEXT's extensions in turn. Given the full path, `Command` starts a
/// `.cmd` or `.bat` file through cmd.exe and quotes its arguments safely. A name not found is left
/// to `Command`.
#[cfg(windows)]
pub(crate) fn program(name: &str, dir: &Path, path: &OsStr) -> PathBuf {
    // What `Command` can start: PATHEXT may also list .js or .vbs, which need a file association.
    const STARTABLE: [&str; 4] = [".com", ".exe", ".bat", ".cmd"];
    // cmd.exe's default, when it's unset.
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let exts: Vec<String> = pathext
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let lower = name.to_ascii_lowercase();
    if Path::new(name).file_name() != Some(name.as_ref())
        || exts.iter().any(|ext| lower.ends_with(ext.as_str()))
    {
        return name.into();
    }
    for entry in std::env::split_paths(path).filter(|entry| !entry.as_os_str().is_empty()) {
        // A relative entry as a shell in `dir` reads it.
        let entry = dir.join(entry);
        for ext in exts.iter().filter(|ext| STARTABLE.contains(&ext.as_str())) {
            let file = entry.join(format!("{name}{ext}"));
            // Not `is_file`, which follows links: it misses app execution aliases, such as the
            // Microsoft Store's python.exe.
            if std::fs::symlink_metadata(&file).is_ok_and(|meta| !meta.is_dir()) {
                return file;
            }
        }
    }
    name.into()
}

/// What starts for `name`: `Command` looks a bare name up in PATH as a shell would.
#[cfg(not(windows))]
pub(crate) fn program(name: &str, _: &Path, _: &OsStr) -> PathBuf {
    name.into()
}

/// The command's exit code, as a shell reports it: 128 + N after signal N.
fn exit_code(status: &ExitStatus) -> u8 {
    match status.code() {
        Some(code) => u8::try_from(code).unwrap_or(1),
        None => signal_code(status),
    }
}

#[cfg(unix)]
fn signal_code(status: &ExitStatus) -> u8 {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .signal()
        .and_then(|signal| u8::try_from(128 + signal).ok())
        .unwrap_or(1)
}

#[cfg(not(unix))]
fn signal_code(_: &ExitStatus) -> u8 {
    1
}

#[cfg(unix)]
fn signaled(status: &ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal().is_some()
}

#[cfg(not(unix))]
fn signaled(_: &ExitStatus) -> bool {
    false
}

/// A run's length, rounded for people: `0.4s`, `41s`, `3m 12s`, `1h 4m`.
pub(crate) fn took(ms: u64) -> String {
    let s = ms / 1000;
    match ms {
        0..1_000 => format!("{:.1}s", ms as f64 / 1000.0),
        1_000..60_000 => format!("{s}s"),
        60_000..3_600_000 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3_600, s / 60 % 60),
    }
}

/// How long ago, rounded for people: `just now`, `12s ago`, `4 min ago`, `3 h ago`.
fn ago(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0..10 => "just now".into(),
        10..60 => format!("{s}s ago"),
        60..3_600 => format!("{} min ago", s / 60),
        3_600..86_400 => format!("{} h ago", s / 3_600),
        86_400..172_800 => "a day ago".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// hushrun's own lines, on stderr: stdout belongs to the command.
struct Say {
    quiet: bool,
}

impl Say {
    fn line(&self, text: fmt::Arguments<'_>) {
        if !self.quiet {
            eprintln!("hushrun: {text}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ago, derived_name, redacted, took};

    /// `text`'s words, split at each space.
    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-10
    #[test]
    fn a_word_equal_to_a_secret_variables_value_is_redacted() {
        let secret = "s3cret-value-1";
        // Every part of a name that says its value is a secret, in any case.
        for name in [
            "DEPLOY_TOKEN",
            "deploy_token",
            "Client_Secret",
            "DB_PASSWORD",
            "mysql_passwd",
            "SSHPASS",
            "Api_Key",
            "GCP_CREDENTIALS",
            "auth",
            "PRIVATE_REGISTRY",
            "tls_cert_pem",
        ] {
            let vars = [("HOME", "/home/someone"), (name, secret)];
            assert_eq!(
                redacted(&words(&format!("deploy --token {secret} --to prod")), vars),
                words("deploy --token [redacted] --to prod"),
                "{name}"
            );
            // What follows an `=`: a `NAME=value` word's value, and a flag's.
            assert_eq!(
                redacted(
                    &words(&format!("env DEPLOY={secret} make --token={secret}")),
                    vars
                ),
                words("env DEPLOY=[redacted] make --token=[redacted]"),
                "{name}"
            );
        }
        // After any `=`, the first one that leaves a secret.
        let vars = [("NPM_TOKEN", secret)];
        assert_eq!(
            redacted(
                &words(&format!("docker build --build-arg=NPM_TOKEN={secret} .")),
                vars
            ),
            words("docker build --build-arg=NPM_TOKEN=[redacted] ."),
        );
        // A value that holds an `=` is still one value.
        let padded = "c2VjcmV0LXZhbHVl==";
        assert_eq!(
            redacted(
                &words(&format!("{padded} KEY={padded}")),
                [("API_KEY", padded)]
            ),
            words("[redacted] KEY=[redacted]"),
        );
        // A word that holds more than the value, or a part of it, stays; so does the value of a
        // variable whose name says nothing.
        let vars = [("DEPLOY_TOKEN", secret), ("BRANCH", "release-candidate")];
        let kept = words(&format!(
            "git push origin release-candidate x{secret} {secret}x s3cret A={secret}x"
        ));
        assert_eq!(redacted(&kept, vars), kept);
    }

    // covers: C-10
    #[test]
    fn a_secret_variables_value_needs_8_bytes() {
        let vars = [("SHORT_TOKEN", "1234567"), ("LONG_TOKEN", "12345678")];
        assert_eq!(
            redacted(&words("seed 1234567 12345678 N=1234567 N=12345678"), vars),
            words("seed 1234567 [redacted] N=1234567 N=[redacted]"),
        );
        // Bytes, not characters: four two-byte characters are 8 bytes.
        assert_eq!(
            redacted(
                &words("say éééé ééé"),
                [("A_KEY", "éééé"), ("B_KEY", "ééé")]
            ),
            words("say [redacted] ééé"),
        );
        // An empty value, or a word made of nothing, never matches.
        assert_eq!(
            redacted(&words("run  A="), [("EMPTY_TOKEN", "")]),
            words("run  A=")
        );
    }

    // covers: C-10
    #[test]
    fn a_word_shaped_like_a_token_is_redacted() {
        let body = |n: usize| "a1B2c3D4e5".repeat(n.div_ceil(10))[..n].to_string();
        let tokens = [
            format!("ghp_{}", body(36)),
            format!("gho_{}", body(36)),
            format!("ghu_{}", body(36)),
            format!("ghs_{}", body(36)),
            format!("ghr_{}", body(36)),
            format!("github_pat_{}", body(82)),
            format!("glpat-{}", body(20)),
            format!("xoxa-{}", body(24)),
            format!("xoxb-{}", body(24)),
            format!("xoxp-{}", body(24)),
            format!("xoxr-{}", body(24)),
            format!("xoxs-{}", body(24)),
            format!("AKIA{}", "IOSFODNN7EXAMPL0"),
            format!("AKIA{}:{}", "IOSFODNN7EXAMPL0", body(40)),
            format!("sk-{}", body(20)),
            format!("sk-proj-{}", body(48)),
            "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIA==\n-----END PRIVATE KEY-----\n"
                .to_string(),
        ];
        for token in &tokens {
            // With no variable at all, alone and after an `=`.
            assert_eq!(
                redacted(std::slice::from_ref(token), []),
                ["[redacted]"],
                "{token}"
            );
            assert_eq!(
                redacted(&[format!("GH_TOKEN={token}"), format!("--key={token}")], []),
                ["GH_TOKEN=[redacted]", "--key=[redacted]"],
                "{token}"
            );
        }
        // What only looks close stays.
        let close = [
            "ghp".to_string(),
            "gh_token".to_string(),
            "ghx_123".to_string(),
            "glpat_123".to_string(),
            "github_patch".to_string(),
            "xoxc-123".to_string(),
            "xox-123".to_string(),
            "xoxb".to_string(),
            format!("AKIA{}", "IOSFODNN7EXAMP"),
            format!("AKIA{}", "iosfodnn7exampl0"),
            format!("sk-{}", body(19)),
            format!("sk-{}.{}", body(10), body(10)),
            "----BEGIN PRIVATE KEY----".to_string(),
            "x-----BEGIN".to_string(),
            "AKIA".to_string(),
            "sk-".to_string(),
            "a=b".to_string(),
        ];
        assert_eq!(redacted(&close, []), close);
    }

    // covers: C-9
    #[test]
    fn a_check_without_a_name_takes_its_scripts_or_its_programs() {
        let name = |script: Option<&str>, program: &str| {
            derived_name(script, program).map(|name| name.to_string())
        };
        // The script npm, pnpm, yarn or bun runs, made a name as a program's is.
        for (script, named) in [
            ("test", "test"),
            ("type-check", "type-check"),
            ("test:unit", "test-unit"),
            ("format:check", "format-check"),
            ("Lint", "lint"),
            ("test:e2e:ci", "test-e2e-ci"),
            ("check_types", "check_types"),
            (" lint ", "lint"),
            ("1:test", "test"),
            ("test:", "test"),
            ("build/test.js", "build-test-js"),
        ] {
            assert_eq!(
                name(Some(script), "vitest").as_deref(),
                Some(named),
                "{script}"
            );
        }
        assert_eq!(
            name(Some(&format!("test:{}", "a".repeat(80))), "vitest").as_deref(),
            Some(format!("test-{}", "a".repeat(58)).as_str())
        );
        // npx's, or one that leaves no name: the program's.
        assert_eq!(name(Some("npx"), "eslint").as_deref(), Some("eslint"));
        assert_eq!(name(Some(""), "tsc").as_deref(), Some("tsc"));
        assert_eq!(name(Some("123"), "tsc").as_deref(), Some("tsc"));
        assert_eq!(name(Some("::"), "tsc").as_deref(), Some("tsc"));
        for (program, named) in [
            ("vitest", "vitest"),
            ("./node_modules/.bin/vitest", "vitest"),
            ("C:\\tools\\Vitest.CMD", "vitest"),
            ("vue-tsc", "vue-tsc"),
            ("run_tests.sh", "run_tests"),
            ("python3.12", "python3"),
            ("my.tool.sh", "my-tool"),
            ("2to3", "to3"),
            ("Ünïcode", "n-code"),
            ("tool--", "tool"),
            (".bin", "bin"),
        ] {
            assert_eq!(name(None, program).as_deref(), Some(named), "{program}");
        }
        assert_eq!(
            name(None, &"a".repeat(80)).as_deref(),
            Some("a".repeat(63).as_str())
        );
        for program in ["", "123", "./9.sh", "..", "-"] {
            assert_eq!(name(None, program), None, "{program}");
        }
    }

    #[test]
    fn durations_read_like_people_say_them() {
        assert_eq!(took(400), "0.4s");
        assert_eq!(took(41_000), "41s");
        assert_eq!(took(192_000), "3m 12s");
        assert_eq!(took(3_840_000), "1h 4m");
        assert_eq!(ago(-5), "just now");
        assert_eq!(ago(12_000), "12s ago");
        assert_eq!(ago(240_000), "4 min ago");
        assert_eq!(ago(3 * 3_600_000), "3 h ago");
        assert_eq!(ago(100_000_000), "a day ago");
        assert_eq!(ago(3 * 86_400_000), "3 days ago");
    }
}
