//! `hushrun ready` (C-22): what CI would still run on this exact code, and the commands that run
//! it here first. The pre-push hook (C-23) tells the same thing.
//!
//! A check is `missing` only when it hasn't passed on this code and a command can make it pass
//! here: running that command is what makes it `passed`, so an agent that does what this says
//! ends. Everything else is `ci`: nothing here changes it, and CI or another machine has to run it.
//!
//! \[ADR 0037\] Checks come from hushrun.yaml's `deploy`, package.json's scripts and CI's own files,
//! so a repository in any language has them, and each is looked up by the key `hushrun run` would
//! use: its scope, its directory, what the scope declares outside the repository and \[ADR 0045\]
//! its rule. Of programs, this starts git and no tool, for any repository: of a rule it reads only
//! what needs nothing started, and a run that didn't count leaves why, in a mark this reads.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Context as _;
use serde::Serialize;

use hushrun_attest::TrustedRoot;
use hushrun_core::digest::Digest;
use hushrun_core::names::{CheckName, ScopeName};
use hushrun_core::plan::{Status, check_status};
use hushrun_core::policy::{CheckDef, Policy};
use hushrun_core::signers::Signers;
use hushrun_vcs::{Snapshot, digest_scopes};

use crate::app::App;
use crate::cli::{GlobalOpts, ReadyArgs, ScopeOpts};
use crate::cmd::setup::{self, WrappedRun, workflows};
use crate::cmd::wrap::repository_of;
use crate::context::{Content, Source};
use crate::events::EventStore;
use crate::exit::{CmdResult, Exit, Failure};
use crate::isolated::{Isolation, Runtime};
use crate::measure;
use crate::output::{self, Format};
use crate::results::{self, Backend, ExternalInput, Latest, Mark, Place, RunResult, Verdict};

/// What a check needs before a push.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// It passed on this code: CI skips it.
    Passed,
    /// It hasn't, and `run` makes it pass here.
    Missing,
    /// Nothing here changes it: CI or another machine has to run it.
    Ci,
}

/// One check, as `hushrun.ready/v1` shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub check: String,
    pub state: State,
    /// Why it isn't `passed`.
    pub why: Option<String>,
    /// The command that runs it here, for a `missing` check a run here can clear.
    pub run: Option<String>,
    /// \[ADR 0045\] Who recorded a `passed` check's result, as the server says.
    #[serde(rename = "recordedBy")]
    pub recorded_by: Option<String>,
    /// \[ADR 0048\] What a `passed` check's pass said of its test and coverage reports (C-27).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reports: Option<hushrun_reports::Reports>,
}

/// `hushrun.ready/v2`. \[ADR 0062\] v1 named the stage the code went to.
#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: &'static str,
    /// The commit looked at; none for the worktree of a repository with no commit yet.
    pub revision: Option<String>,
    pub branch: Option<String>,
    pub ready: bool,
    pub checks: Vec<Item>,
    /// The commands to run, in order.
    pub next: Vec<String>,
    /// What else to know before running them.
    pub notes: Vec<String>,
    /// \[ADR 0040\] Whether a note says CI runs a check outside hushrun, so that it wouldn't skip
    /// what passed here.
    #[serde(skip)]
    pub runs_outside: bool,
    /// `--strict`: every check has to have passed.
    #[serde(skip)]
    strict: bool,
}

impl Report {
    /// The checks left to run here.
    pub fn missing(&self) -> impl Iterator<Item = &Item> {
        self.checks.iter().filter(|i| i.state == State::Missing)
    }

    /// The checks that passed on this code.
    pub fn passed(&self) -> impl Iterator<Item = &Item> {
        self.checks.iter().filter(|i| i.state == State::Passed)
    }

    /// Leaves every check of `names` with CI, for one reason.
    fn leave(&mut self, names: &[CheckName], why: &str) {
        for name in names {
            self.checks.push(ci(name.as_str(), why));
        }
    }

    /// \[ADR 0045\] `--strict`: ready only when a check is known and every check passed. Plain
    /// `ready` is true when nothing is `missing`, so what nobody ran reads as ready.
    fn insist(&mut self) {
        self.strict = true;
        self.ready =
            !self.checks.is_empty() && self.checks.iter().all(|i| i.state == State::Passed);
    }

    /// The revision, short, for a person: `the worktree` when there is none.
    pub fn short(&self) -> String {
        self.revision
            .as_deref()
            .map_or_else(|| "the worktree".into(), |r| r.chars().take(7).collect())
    }

    fn text(&self, worktree: bool) -> String {
        let code = if worktree {
            "the worktree".to_string()
        } else {
            self.short()
        };
        let mut text = String::new();
        if self.checks.is_empty() {
            let _ = writeln!(
                text,
                "hushrun knows no checks for {code}: hushrun.yaml's `deploy`, package.json's `hushrun run` scripts and CI's files name them."
            );
            for note in &self.notes {
                let _ = writeln!(text, "{note}");
            }
            return text;
        }
        if self.ready {
            let _ = writeln!(text, "{code} is ready to push.");
        } else if self.strict && self.missing().next().is_none() {
            let _ = writeln!(
                text,
                "{code} isn't ready: {} left, and nothing here changes that.",
                count(self.checks.len() - self.passed().count())
            );
        } else {
            let _ = writeln!(
                text,
                "{code} isn't ready: CI would run {} again.",
                count(self.missing().count())
            );
        }
        let width = self.checks.iter().map(|i| i.check.len()).max().unwrap_or(0);
        for item in &self.checks {
            let state = match item.state {
                State::Passed => "passed",
                State::Missing => "missing",
                State::Ci => "ci",
            };
            let summary = item
                .reports
                .as_ref()
                .and_then(hushrun_reports::Reports::summary);
            let said = match (&item.why, &item.recorded_by, summary) {
                (Some(why), ..) => Some(why.clone()),
                (None, Some(by), Some(summary)) => Some(format!("recorded by {by}: {summary}")),
                (None, Some(by), None) => Some(format!("recorded by {by}")),
                (None, None, summary) => summary,
            };
            let line = match said {
                Some(said) => format!("  {state:<8}{:<width$}  {said}", item.check),
                None => format!("  {state:<8}{}", item.check),
            };
            let _ = writeln!(text, "{}", line.trim_end());
        }
        if !self.next.is_empty() {
            let _ = writeln!(text, "Run these here, then push:");
            for command in &self.next {
                let _ = writeln!(text, "  {command}");
            }
        }
        for note in &self.notes {
            let _ = writeln!(text, "{note}");
        }
        text
    }
}

/// `1 check`, `2 checks`.
pub fn count(n: usize) -> String {
    match n {
        1 => "1 check".into(),
        _ => format!("{n} checks"),
    }
}

/// What to look at.
pub struct Target<'a> {
    pub snapshot: Snapshot,
    /// What the commands printed name it by: none for HEAD.
    pub flag: Option<String>,
    pub branch: Option<String>,
    pub scopes: Option<&'a ScopeOpts>,
}

pub fn run(global: &GlobalOpts, args: &ReadyArgs) -> CmdResult {
    let flag = if args.worktree {
        Some("--worktree".to_string())
    } else {
        args.rev.as_ref().map(|rev| format!("--rev {rev}"))
    };
    let target = Target {
        snapshot: crate::cli::snapshot(args.rev.as_deref(), args.worktree),
        flag,
        branch: args.branch.clone(),
        scopes: Some(&args.scopes),
    };
    let mut report = evaluate(global, &target)?;
    if args.strict {
        report.insist();
    }
    match global.format {
        Format::Json => {
            let value = serde_json::to_value(&report).context("writing the report")?;
            output::print_json(&value)?;
        }
        _ => output::print(&report.text(args.worktree))?,
    }
    Ok(if report.ready {
        Exit::Ok
    } else {
        Exit::NotReady
    })
}

/// Looks at `target`. Only a usage or configuration error is an error: what can't be read about a
/// check leaves it with CI.
pub fn evaluate(global: &GlobalOpts, target: &Target<'_>) -> Result<Report, Failure> {
    let content = Content::open(global, Source::Repo(target.snapshot.clone()))?;
    let root = content.vcs.root().to_path_buf();
    let branch = target.branch.clone().or_else(|| current_branch(&root));
    let mut report = Report {
        schema: "hushrun.ready/v2",
        revision: content.listing.revision.revision.clone(),
        branch: branch.clone(),
        ready: true,
        checks: Vec::new(),
        next: Vec::new(),
        notes: Vec::new(),
        runs_outside: false,
        strict: false,
    };
    let mut in_place = false;
    // \[ADR 0062\] A repository that deploys with hushrun requires its checks' evidence: `deploy`
    // is what says so, as it is for C-8.
    if let Some(policy) = content.policy.as_ref().filter(|p| p.deploy.is_some()) {
        in_place |= required(global, &content, policy, target, &mut report)?;
    }
    in_place |= runs(&content, branch.as_deref(), &mut report);
    let ready = report.missing().next().is_none();
    report.ready = ready;
    if in_place && target.snapshot != Snapshot::Worktree {
        let elsewhere = target.flag.is_some();
        if elsewhere || uncommitted(&root) {
            report.notes.push(format!(
                "These run on the files as they are now, and those aren't {}: commit or stash what's different first.",
                report.short()
            ));
        }
    }
    Ok(report)
}

/// How the checks hushrun.yaml requires stand, per scope. Whether a command to run works on the
/// files as they are, unlike an isolated run, which exports the revision.
fn required(
    global: &GlobalOpts,
    content: &Content,
    policy: &Policy,
    target: &Target<'_>,
    report: &mut Report,
) -> Result<bool, Failure> {
    let app = App::new(global)?;
    let names = policy.required();
    let scopes = match target.scopes {
        Some(opts) => content.select(opts, true)?,
        None => content.scopes.iter().collect(),
    };
    let digests = digest_scopes(
        content.vcs.as_ref(),
        &content.listing,
        &content.scopes,
        &scopes,
        false,
    )?;
    let several = digests.scopes.len() > 1;
    // Whatever keeps every required check from counting: they all stay with CI.
    let cant = |failure: Failure| format!("can't read its evidence: {:#}", failure.error);
    let opened = (|| {
        let (_, ledger) = app.ledger_for_plan(Some(policy)).map_err(cant)?;
        let signers = app.signers(policy, ledger.as_ref()).map_err(cant)?;
        let sigstore = app.sigstore_root().map_err(cant)?;
        Ok::<_, String>((ledger, signers, sigstore))
    })();
    let (ledger, signers, sigstore) = match opened {
        Ok(opened) => opened,
        Err(why) => {
            report.leave(&names, &why);
            return Ok(false);
        }
    };
    let ci_only = &signers.ci_only;
    let ci_only_now = ci_only.since_anywhere().is_some();
    // A repository that takes CI's evidence only needs no signer here: `ci_only` says why first.
    if !ci_only_now && let Some(why) = cannot_count(&app, &sigstore, &signers) {
        report.leave(&names, &why);
        return Ok(false);
    }
    let store = EventStore {
        ledger: ledger.as_ref(),
        signers: &signers,
        sigstore: &sigstore,
        repository: &policy.repository,
        now: app.now,
    };
    let mut in_place = false;
    let mut runtime = None;
    for scope in &digests.scopes {
        let events = match store.read(&scope.scope, &scope.digest) {
            Ok(events) => events.accepted,
            Err(err) => {
                report.leave(&names, &format!("can't read its evidence: {err}"));
                continue;
            }
        };
        let mut isolated = Vec::new();
        let mut local = Vec::new();
        for name in &names {
            let label = match several {
                true => format!("{}/{name}", scope.scope),
                false => name.to_string(),
            };
            let (status, _) = check_status(name, &events, policy, ci_only, app.now);
            if status == Status::Satisfied {
                report.checks.push(passed(label));
                continue;
            }
            // \[ADR 0060\] A failure outranks a pass here that nothing recorded: the push waits
            // for it.
            if status != Status::Failed
                && results::ledger_check_key(name, &scope.scope, &scope.digest)
                    .is_ok_and(|key| results::mark(&key) == Some(Mark::Unrecorded))
            {
                report.checks.push(ci(&label, NOT_RECORDED));
                continue;
            }
            if ci_only_now {
                report
                    .checks
                    .push(ci(&label, "this repository takes CI's evidence only"));
                continue;
            }
            let def = &policy.checks[name];
            let how = match runnable(&app, name, def, &mut runtime) {
                Ok(how) => how,
                Err(why) => {
                    report.checks.push(ci(&label, &why));
                    continue;
                }
            };
            let group = match how {
                How::Isolated => &mut isolated,
                How::InPlace => &mut local,
            };
            group.push((report.checks.len(), name.clone()));
            report.checks.push(Item {
                check: label,
                state: State::Missing,
                why: Some(status_text(status).into()),
                run: None,
                recorded_by: None,
                reports: None,
            });
        }
        in_place |= !local.is_empty();
        for (how, group) in [("--isolated ", isolated), ("", local)] {
            if group.is_empty() {
                continue;
            }
            let checks: Vec<&str> = group.iter().map(|(_, name)| name.as_str()).collect();
            let scoped = match several {
                true => format!("--scope {} ", scope.scope),
                false => String::new(),
            };
            let flag = target
                .flag
                .as_ref()
                .map(|flag| format!("{flag} "))
                .unwrap_or_default();
            let command = format!("{} run {scoped}{flag}{how}{}", hushrun(), checks.join(" "));
            for (at, _) in &group {
                report.checks[*at].run = Some(command.clone());
            }
            report.next.push(command);
        }
    }
    Ok(in_place)
}

/// Why what this machine signs wouldn't count, when it wouldn't: a run here would change nothing,
/// and saying to run it again would never end.
fn cannot_count(app: &App<'_>, sigstore: &TrustedRoot, signers: &Signers) -> Option<String> {
    if let Some(failure) = app.cannot_sign() {
        return Some(format!("{:#}", failure.error));
    }
    let signing = match app.signing_with(sigstore) {
        Ok(signing) => signing,
        Err(failure) => return Some(format!("{:#}", failure.error)),
    };
    let key = signing.signer().public_key()?;
    match signers.machine(&key.keyid()) {
        Some(machine) if machine.revoked.is_none() => None,
        Some(machine) => Some(format!(
            "this machine, {}, is revoked, so what it signs doesn't count",
            machine.name
        )),
        None => Some(
            "the server lists no such machine, so what it signs doesn't count: `hushrun doctor` says what to do"
                .into(),
        ),
    }
}

/// How a check runs here.
enum How {
    /// In its pinned image, on an export of the revision.
    Isolated,
    /// On the files as they are.
    InPlace,
}

/// How `def` can run here, or why it can't. `runtime` remembers what asking Docker said.
fn runnable(
    app: &App<'_>,
    name: &CheckName,
    def: &CheckDef,
    runtime: &mut Option<Result<(), String>>,
) -> Result<How, String> {
    if def.run.is_none() {
        return Err("hushrun.yaml gives it no `run`, so its CI job runs it".into());
    }
    // A check whose variables hold secrets stays in CI: E-12 keeps them out of containers, and
    // this machine doesn't have them.
    if def.image.is_none() {
        return match def.env.iter().find(|var| crate::isolated::is_secret(var)) {
            Some(var) => Err(format!("it needs {var}, which stays in CI")),
            None => Ok(How::InPlace),
        };
    }
    Isolation::of(app, name, def).map_err(|failure| format!("{:#}", failure.error))?;
    let found = runtime.get_or_insert_with(|| match Runtime::is_here(app) {
        true => Ok(()),
        false => Err("it runs in its pinned image, and there is no Docker or Podman here".into()),
    });
    found.clone().map(|()| How::Isolated)
}

/// What a run's key takes from its scope: the scope, and its code as it is at the revision looked
/// at.
struct Scoped {
    scope: ScopeName,
    digest: Digest,
    external: Vec<ExternalInput>,
}

/// The code of the scope `name`, `None` for the only one: what `hushrun run` would key this run
/// on, or why it would run without skipping, so that a run here wouldn't change anything (C-14).
fn scoped(content: &Content, name: Option<&str>) -> Result<Scoped, String> {
    let scope = match name {
        Some(name) => content
            .scopes
            .get(name)
            .ok_or_else(|| format!("hushrun.yaml has no scope {name}"))?,
        None => match content.scopes.iter().collect::<Vec<_>>().as_slice() {
            [scope] => *scope,
            _ => return Err("it names no --scope, and hushrun.yaml has several".to_string()),
        },
    };
    let digests = digest_scopes(
        content.vcs.as_ref(),
        &content.listing,
        &content.scopes,
        &[scope],
        false,
    )
    .map_err(|err| format!("can't read its code: {err}"))?;
    let digest = digests
        .scopes
        .first()
        .map(|found| found.digest)
        .ok_or_else(|| "can't read its code".to_string())?;
    let external = content
        .external(&scope.name)
        .map_err(|err| format!("{err:#}"))?;
    Ok(Scoped {
        scope: scope.name.clone(),
        digest,
        external,
    })
}

/// How the `hushrun run`s of package.json's scripts and of CI's files stand. Whether there is a
/// command to run.
fn runs(content: &Content, branch: Option<&str>, report: &mut Report) -> bool {
    let root = content.vcs.root();
    let read = |path: &str| {
        content
            .listing
            .find(path)
            .and_then(|entry| content.vcs.read_entry(entry).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };
    let package = read("package.json");
    let pipelines = workflows::pipelines_among(
        content.listing.entries.iter().map(|e| e.path.as_str()),
        read,
    );
    // \[ADR 0047\] The files a job's actions are, and the checks hushrun.yaml lets CI skip
    // whatever reads their reports.
    let bytes = |path: &str| {
        content
            .listing
            .find(path)
            .and_then(|entry| content.vcs.read_entry(entry).ok())
    };
    let reports_skip = setup::reports_skip(content.policy.as_ref());
    // One check, however many jobs run it: all of those say what CI does.
    let mut wrapped: Vec<WrappedRun> = Vec::new();
    let found = setup::wrapped_runs(
        root,
        package.as_deref(),
        &pipelines,
        branch,
        hushrun(),
        &bytes,
        &reports_skip,
    );
    for run in found {
        match wrapped
            .iter_mut()
            .find(|seen| seen.key.is_ok() && seen.key == run.key)
        {
            Some(seen) => {
                seen.jobs.extend(run.jobs);
                seen.variants = seen.variants.take().or(run.variants);
                seen.variants_warning = seen.variants_warning.take().or(run.variants_warning);
                seen.reports = seen.reports.take().or(run.reports);
            }
            None => wrapped.push(run),
        }
    }
    // \[ADR 0045\] A rule for a name nothing runs is a typo: the check it meant has no rule.
    if let Some(policy) = &content.policy {
        report.notes.extend(
            policy
                .counts_from
                .checks
                .keys()
                .filter(|name| !wrapped.iter().any(|run| starts(run, name)))
                .map(|name| format!("hushrun.yaml has a rule for {name}, and nothing runs it")),
        );
    }
    if wrapped.is_empty() {
        return false;
    }
    // \[ADR 0040\] A pass here does nothing for a check CI runs outside hushrun.
    let outside = setup::outside_hushrun(package.as_deref(), &pipelines, branch, hushrun());
    report.runs_outside = !outside.is_empty();
    report.notes.extend(outside);
    let backend = match Backend::select(Some(root)) {
        Ok(Some(backend)) => backend,
        Ok(None) => return leave_all(report, &wrapped, results::off_reason()),
        Err(err) => return leave_all(report, &wrapped, &format!("{err:#}")),
    };
    if matches!(backend.place(), Place::Directory(_)) {
        return leave_all(
            report,
            &wrapped,
            "results stay on this machine, so CI can't see a pass here",
        );
    }
    let repository = repository_of(content);
    let several = content.scopes.len() > 1;
    let mut codes: BTreeMap<Option<String>, Result<Scoped, String>> = BTreeMap::new();
    let mut marks_stay = None;
    let mut any = false;
    for run in &wrapped {
        let parts = match &run.key {
            Ok(parts) => parts,
            Err(why) => {
                report.checks.push(ci(&run.check, why));
                continue;
            }
        };
        let code = codes
            .entry(parts.scope.clone())
            .or_insert_with(|| scoped(content, parts.scope.as_deref()));
        let code = match code {
            Ok(code) => &*code,
            Err(why) => {
                report.checks.push(ci(&run.check, why));
                continue;
            }
        };
        let label = match several {
            true => format!("{}/{}", code.scope, run.check),
            false => run.check.clone(),
        };
        let Ok((key, rule)) = results::run_key(
            content.policy.as_ref(),
            &parts.check,
            &parts.command,
            &parts.cwd,
            &code.scope,
            &code.digest,
            &code.external,
        ) else {
            continue;
        };
        let latest = backend.latest_for_ci(&key, repository.as_deref());
        let recorded_by = latest.as_ref().ok().and_then(|l| l.recorded_by.clone());
        let ci_only = matches!(&latest, Ok(latest) if latest.ci_only);
        let failed = matches!(
            &latest,
            Ok(Latest { result: Some(last), .. }) if last.result == Verdict::Fail
        );
        // \[ADR 0044\] No pass skips it in CI, unless it failed here: that is still to fix first.
        // \[ADR 0047\] Nor when a later step of its job reads its reports (C-26).
        let every_time = run.variants.clone().or_else(|| {
            run.reports.as_ref().map(|reader| {
                format!("CI runs it every time: a later step reads its reports ({reader})")
            })
        });
        if let (Some(why), false) = (&every_time, failed) {
            report.checks.push(ci(&label, why));
            continue;
        }
        // \[ADR 0045\] A run here that didn't count says something only of a check with no result:
        // a failure recorded anywhere stands.
        let mark = results::mark(&key);
        let marked = match (&rule, &mark) {
            (Some(_), Some(Mark::DidntCount(why))) => Some(why.clone()),
            _ => None,
        };
        let (why, state): (String, State) = match (latest.map(|latest| latest.result), &marked) {
            (Ok(Some(last)), _) if last.result == Verdict::Pass && ci_takes(&last, &run.jobs) => {
                report.checks.push(Item {
                    recorded_by,
                    reports: last.reports,
                    ..passed(label)
                });
                continue;
            }
            // CI's jobs won't take it, and a run here would skip on it, so nothing here helps.
            (Ok(Some(last)), _) if last.result == Verdict::Pass => (OTHER_JOB.into(), State::Ci),
            // A pass here wouldn't change what CI runs, so this machine has nothing to do.
            _ if ci_only => (CI_REUSES_ONLY_CI.into(), State::Ci),
            (Ok(Some(_)), _) => (FAILED.into(), State::Missing),
            // A run here didn't count, and doing it again wouldn't either.
            (Ok(None), Some(why)) => (format!("{DIDNT_COUNT}: {why}"), State::Ci),
            (Ok(None), None) if mark.is_some() => (NOT_RECORDED.into(), State::Ci),
            (Ok(None), None) => ("no pass yet for this exact code".into(), State::Missing),
            (Err(err), _) => {
                let why = match backend.refusal() {
                    Some(_) => {
                        let org = backend.org().unwrap_or_default();
                        format!("{org} didn't answer for this sign-in; `hushrun whoami` says why")
                    }
                    None => format!("can't read earlier results ({err})"),
                };
                report.checks.push(ci(&label, &why));
                continue;
            }
        };
        if state == State::Ci {
            report.checks.push(ci(&label, &why));
            continue;
        }
        if let Some(needs) = &run.only_ci {
            report.checks.push(ci(&label, needs));
            continue;
        }
        // A run that can't start records nothing: asking for it again would never end.
        if let Some(tool) = &run.needs
            && !tool.found(&root.join(&parts.cwd))
        {
            let why = format!("{} isn't installed here", tool.program);
            report.checks.push(ci(&label, &why));
            continue;
        }
        if let Some(why) = backend.cannot_sign() {
            report
                .checks
                .push(ci(&label, &format!("not recorded: {why}")));
            continue;
        }
        if let Some(rule) = &rule {
            match (failed, marked.or_else(|| measure::unmet_here(rule))) {
                // A failure under this key came from a machine that met the rule, so the push
                // waits for it, and a run here couldn't clear it: no command would help.
                (true, Some(why)) => {
                    report.checks.push(Item {
                        check: label,
                        state: State::Missing,
                        why: Some(format!(
                            "{FAILED}, and a run here wouldn't count ({why}): change the code, or pass it on a machine that meets the rule"
                        )),
                        run: None,
                        recorded_by: None,
                        reports: None,
                    });
                    continue;
                }
                (false, Some(why)) => {
                    report.checks.push(ci(&label, &why));
                    continue;
                }
                // The rest is the run's to judge, as nothing here starts a tool. It says so in a
                // mark, and without one this would ask for ever.
                (false, None) if !rule.tools.is_empty() => {
                    let kept = marks_stay.get_or_insert_with(results::marks_stay);
                    if let Err(why) = kept {
                        let why = format!("a run here couldn't say if it counted: {why}");
                        report.checks.push(ci(&label, &why));
                        continue;
                    }
                }
                _ => {}
            }
        }
        any = true;
        report.checks.push(Item {
            check: label,
            state,
            why: Some(why),
            run: Some(run.with.clone()),
            recorded_by: None,
            reports: None,
        });
        if !report.next.contains(&run.with) {
            report.next.push(run.with.clone());
        }
    }
    any
}

/// \[ADR 0044\] Whether each of `jobs`, CI's jobs that run a check, would take `last` (C-10), by
/// their names: a job's platform is measured where it runs, so that isn't known here. `run`
/// decides; this only keeps `ready` from saying CI skips what it won't.
fn ci_takes(last: &RunResult, jobs: &[String]) -> bool {
    let recorded = last.job.as_deref();
    jobs.iter().all(|job| {
        let here = match recorded.and_then(|named| named.rsplit_once('@')) {
            Some((name, _)) if name == job => recorded,
            _ => Some(job.as_str()),
        };
        // A laptop can't know a variant's values: a job with several keeps `ci` (C-22).
        last.counts_in(here, None)
    })
}

/// What a pass from another CI job is (C-10).
const OTHER_JOB: &str = "it passed in another CI job, and a CI pass counts in its own job";

/// Why a repository's checks stay with CI when it reuses only what CI recorded (C-12).
const CI_REUSES_ONLY_CI: &str =
    "this repository's CI reuses only CI's results, so a pass here wouldn't change what it runs";

/// What a pass here that nothing recorded is: CI can't see it, and running it again won't help.
const NOT_RECORDED: &str = "it passed here, but the result couldn't be recorded";

/// \[ADR 0045\] What a mark's reason follows: a run here under a rule didn't count, and it stands for
/// this code on this machine until there is a result for it.
const DIDNT_COUNT: &str = "a run here didn't count";

/// What the newest result is when it isn't a pass.
const FAILED: &str = "it failed on this exact code last time";

/// Whether `run` starts the check `name`.
fn starts(run: &WrappedRun, name: &CheckName) -> bool {
    run.check == name.as_str() || matches!(&run.key, Ok(key) if key.check == *name)
}

fn leave_all(report: &mut Report, wrapped: &[WrappedRun], why: &str) -> bool {
    for run in wrapped {
        report.checks.push(ci(&run.check, why));
    }
    false
}

fn passed(check: String) -> Item {
    Item {
        check,
        state: State::Passed,
        why: None,
        run: None,
        recorded_by: None,
        reports: None,
    }
}

fn ci(check: &str, why: &str) -> Item {
    Item {
        check: check.to_string(),
        state: State::Ci,
        why: Some(why.to_string()),
        run: None,
        recorded_by: None,
        reports: None,
    }
}

fn status_text(status: Status) -> &'static str {
    match status {
        Status::Expired => "its pass expired",
        Status::Failed => "it failed on this exact code last time",
        _ => "no pass yet for this exact code",
    }
}

/// How a person or an agent starts this hushrun: npm's isn't on a shell's PATH.
pub(crate) fn hushrun() -> &'static str {
    let npm = std::env::current_exe()
        .is_ok_and(|path| path.components().any(|c| c.as_os_str() == "node_modules"));
    if npm { "npx hushrun" } else { "hushrun" }
}

fn current_branch(root: &Path) -> Option<String> {
    git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
}

/// Whether the worktree differs from HEAD, ignored files left out.
fn uncommitted(root: &Path) -> bool {
    git(root, &["status", "--porcelain"]).is_some()
}

/// What `git` prints, trimmed, when it succeeds and prints something.
pub(crate) fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_counts_as_people_say_it() {
        assert_eq!(count(1), "1 check");
        assert_eq!(count(2), "2 checks");
    }
}
