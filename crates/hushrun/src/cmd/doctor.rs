//! `hushrun doctor` (§10.4): checks the setup and says what to fix. Exits 2 when any check
//! fails; warnings don't change the exit code.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use serde_json::json;

use hushrun_artifact::Repository;
use hushrun_core::policy::Policy;
use hushrun_core::run::Provider;
use hushrun_core::signers::Signers;
use hushrun_vcs::Snapshot;

use crate::app::App;
use crate::cli::DoctorArgs;
use crate::cmd::common::connect;
use crate::cmd::setup::{self, workflows};
use crate::context::{Content, Source};
use crate::exit::{CmdResult, Exit};
use crate::output::{self, Format};

/// Clock skew beyond this is a warning: P-6 compares `passed` and `failed` within 60 s.
const SKEW_WARNING: Duration = Duration::from_secs(30);
/// Bytes of source scanned for baked configuration (A-7).
const SCAN_BUDGET: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warning,
    Error,
    Skipped,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warning => "warning",
            Level::Error => "error",
            Level::Skipped => "skipped",
        }
    }
}

struct Report {
    checks: Vec<(&'static str, Level, String)>,
}

impl Report {
    fn add(&mut self, name: &'static str, level: Level, detail: impl Into<String>) {
        self.checks.push((name, level, detail.into()));
    }
}

pub fn run(app: &App<'_>, args: &DoctorArgs) -> CmdResult {
    let mut report = Report { checks: Vec::new() };

    // git, and which CI this is.
    match std::process::Command::new("git").arg("--version").output() {
        Ok(out) if out.status.success() => report.add(
            "git",
            Level::Ok,
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ),
        _ => report.add("git", Level::Error, "git is not installed or not on PATH"),
    }
    report.add(
        "ci",
        Level::Ok,
        match app.run.provider {
            Provider::Local if app.env.agent_set() => {
                "local run (not CI), as HUSHRUN_AGENT says".to_string()
            }
            Provider::Local => "local run (not CI)".to_string(),
            provider => format!(
                "{} run {}{}",
                provider.as_str(),
                app.run.run_id.as_deref().unwrap_or("(no run id)"),
                app.run
                    .merge_queue
                    .as_ref()
                    .map_or(String::new(), |queue| format!(
                        ", the merge queue's run of pull request #{} into {}",
                        queue.pull_request, queue.base
                    ))
            ),
        },
    );

    agent_string(&mut report, app);

    // Where `hushrun run` records results, and as whom (C-17): before hushrun.yaml, which a
    // repository that only uses `hushrun run` doesn't have.
    let (good, detail) = crate::cmd::whoami::Identity::read(app.global).line(app.now.millis());
    report.add(
        "results",
        if good { Level::Ok } else { Level::Warning },
        detail,
    );

    // Configuration.
    let content = match Content::open(app.global, Source::Repo(Snapshot::default())) {
        Ok(content) => content,
        Err(failure) => {
            report.add("hushrun.yaml", Level::Error, format!("{:#}", failure.error));
            return finish(app, report);
        }
    };
    variants(&mut report, &content);
    let policy = match content.policy() {
        Ok(policy) => {
            report.add(
                "hushrun.yaml",
                Level::Ok,
                format!(
                    "{} scope(s), {}, repository {}",
                    content.scopes.len(),
                    match policy.deploy.as_ref().map(|d| d.branch.as_deref()) {
                        Some(Some(branch)) => format!("deploys from {branch}"),
                        Some(None) => "deploys".to_string(),
                        None => "no deploy".to_string(),
                    },
                    policy.repository
                ),
            );
            // \[ADR 0062\] What an older file says that this release reads past.
            for retired in &policy.retired {
                report.add("hushrun.yaml", Level::Warning, retired.notice(&policy.file));
            }
            policy
        }
        Err(failure) => {
            report.add("hushrun.yaml", Level::Error, format!("{:#}", failure.error));
            return finish(app, report);
        }
    };
    if let Some(repository) = &app.run.repository
        && !repository.eq_ignore_ascii_case(&policy.repository)
    {
        report.add(
            "repository",
            Level::Warning,
            format!(
                "this run is in {repository}, but hushrun.yaml says {}",
                policy.repository
            ),
        );
    }

    // The repository's server, and who counts (T-15).
    let mut ledger_url = None;
    let ledger = match app.ledger(Some(policy)) {
        Ok((url, ledger)) => match ledger.list_first("v1/", 1) {
            Ok(_) => {
                report.add("server", Level::Ok, format!("{url} is reachable"));
                protocol(&mut report, app, &url, policy);
                clock(&mut report, &url, app);
                ledger_url = Some(url);
                Some(ledger)
            }
            Err(e) => {
                report.add("server", Level::Error, format!("{url}: {e}"));
                None
            }
        },
        Err(failure) => {
            report.add("server", Level::Error, format!("{:#}", failure.error));
            None
        }
    };
    if let (Some(ledger), Some(url)) = (ledger.as_deref(), &ledger_url) {
        transparency_log(&mut report, app, url, ledger, policy);
    }
    let signers = match ledger.as_deref() {
        Some(ledger) => match app.signers(policy, ledger) {
            Ok(signers) => {
                report.add(
                    "signers",
                    if signers.ci.is_empty() {
                        Level::Warning
                    } else {
                        Level::Ok
                    },
                    signers.summary(),
                );
                Some(signers)
            }
            Err(failure) => {
                report.add("signers", Level::Error, format!("{:#}", failure.error));
                None
            }
        },
        None => None,
    };
    if let Some(signers) = &signers {
        signing(&mut report, app, signers);
    }

    // Registries, the deploy workflow, baked configuration, and the CI's own settings: GitLab's
    // API, protected branch and pipeline variables.
    registries(&mut report, policy, args);
    workflows(&mut report, &content);
    baked_configuration(&mut report, &content);
    if app.run.provider == Provider::Gitlab || app.env.get("HUSHRUN_GITLAB_TOKEN").is_some() {
        gitlab(&mut report, app, policy);
    }
    finish(app, report)
}

/// \[ADR 0044\] A wrapped check that a job with several variants runs. It is safe, so this says it:
/// CI runs it every time (C-14), or \[ADR 0049\] each variant until it passes, when the job says
/// which variant each run is. It warns of a job that sets that wrong, since no pass skips it there.
/// This comes before hushrun.yaml, which a repository that only uses `hushrun run` lacks.
fn variants(report: &mut Report, content: &Content) {
    let read = |path: &str| {
        content
            .listing
            .find(path)
            .and_then(|entry| content.vcs.read_entry(entry).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };
    let pipelines = workflows::pipelines_among(
        content.listing.entries.iter().map(|e| e.path.as_str()),
        read,
    );
    let package = read("package.json");
    let bytes = |path: &str| {
        content
            .listing
            .find(path)
            .and_then(|entry| content.vcs.read_entry(entry).ok())
    };
    let wrapped = setup::wrapped_runs(
        content.vcs.root(),
        package.as_deref(),
        &pipelines,
        None,
        "hushrun",
        &bytes,
        &setup::reports_skip(content.policy.as_ref()),
    );
    let warned: BTreeSet<String> = wrapped
        .iter()
        .filter_map(|run| Some(format!("{}: {}", run.check, run.variants_warning.as_ref()?)))
        .collect();
    let said: BTreeSet<String> = wrapped
        .iter()
        .filter(|run| run.variants_warning.is_none())
        .filter_map(|run| Some(format!("{}: {}", run.check, run.variants.as_ref()?)))
        .collect();
    if said.is_empty() && warned.is_empty() {
        report.add(
            "variants",
            Level::Ok,
            "no job with variants in CI's files runs a wrapped check",
        );
    }
    for line in warned {
        report.add("variants", Level::Warning, line);
    }
    for line in said {
        report.add("variants", Level::Ok, line);
    }
    // \[ADR 0047\] A wrapped check whose reports a later step reads runs every time in CI, which
    // is safe: a skip would leave that step nothing (C-26).
    let reads: BTreeSet<String> = wrapped
        .iter()
        .filter_map(|run| {
            Some(format!(
                "{}: CI runs it every time, since {} reads its reports",
                run.check,
                run.reports.as_ref()?
            ))
        })
        .collect();
    if reads.is_empty() {
        report.add(
            "reports",
            Level::Ok,
            "no step in CI's files reads the reports of a wrapped check",
        );
    }
    for line in reads {
        report.add("reports", Level::Ok, line);
    }
}

/// \[M18\] An https ledger's transparency log (L-11): hushrun.yaml names its key (T-9), and its
/// latest checkpoint verifies with it.
fn transparency_log(
    report: &mut Report,
    app: &App<'_>,
    url: &hushrun_core::ledger_url::LedgerUrl,
    ledger: &dyn hushrun_ledger::Ledger,
    policy: &Policy,
) {
    let Some(log) = ledger.log() else {
        return;
    };
    // What the ledger says about its log: the origin its checkpoints name, which is its own
    // URL's whichever URL hushrun reaches it by (T-4's base-derived one, say), and its key.
    let stated = hushrun_ledger::describe(url, &app.ledger_options_for(url, None))
        .ok()
        .and_then(|info| info.log);
    if let Err(failure) = crate::app::configure_log(ledger, policy) {
        report.add(
            "transparency log",
            Level::Error,
            format!("{:#}", failure.error),
        );
        return;
    }
    let Some(declared) = &policy.transparency_log else {
        let hint = stated
            .as_ref()
            .map(|log| format!(": add to hushrun.yaml\n{}", log_lines(log)))
            .unwrap_or_default();
        report.add(
            "transparency log",
            Level::Warning,
            format!(
                "hushrun.yaml names no log key (`transparency_log`), so receipts, listings and checkpoints go unchecked{hint}"
            ),
        );
        return;
    };
    let expected_origin = stated.as_ref().map_or_else(
        || hushrun_tlog::origin_of(&url.to_string()),
        |log| log.origin.clone(),
    );
    if declared.origin != expected_origin {
        report.add(
            "transparency log",
            Level::Warning,
            format!(
                "`transparency_log.origin` is {}, but this server's checkpoints should name {expected_origin}",
                declared.origin
            ),
        );
    }
    match log.latest() {
        Ok(checkpoint) => report.add(
            "transparency log",
            Level::Ok,
            format!(
                "checkpoint of {} leaves, signed by the log key hushrun.yaml names",
                checkpoint.size()
            ),
        ),
        Err(e) => report.add("transparency log", Level::Error, e.to_string()),
    }
}

/// L-10: the ledger speaks this client's protocol, holds this repository, and calls itself
/// by the URL in use, which CI's ID tokens name as their audience.
fn protocol(
    report: &mut Report,
    app: &App<'_>,
    url: &hushrun_core::ledger_url::LedgerUrl,
    policy: &Policy,
) {
    let info = match hushrun_ledger::describe(url, &app.ledger_options_for(url, None)) {
        Ok(info) => info,
        Err(e) => {
            report.add("server protocol", Level::Error, e.to_string());
            return;
        }
    };
    if info.protocol != hushrun_ledger::PROTOCOL {
        report.add(
            "server protocol",
            Level::Error,
            format!(
                "{url} speaks {:?}, and this hushrun speaks {}",
                info.protocol,
                hushrun_ledger::PROTOCOL
            ),
        );
        return;
    }
    match info.repository.as_deref() {
        Some(repository) if repository.eq_ignore_ascii_case(&policy.repository) => report.add(
            "server protocol",
            Level::Ok,
            format!("{} for {repository}", info.protocol),
        ),
        Some(repository) => report.add(
            "server protocol",
            Level::Error,
            format!(
                "{url} holds {repository}, but hushrun.yaml's repository is {}: one URL holds one repository",
                policy.repository
            ),
        ),
        None => report.add(
            "server protocol",
            Level::Warning,
            format!("{url} doesn't say which repository it holds"),
        ),
    }
    if let Some(name) = info.ledger.as_deref()
        && name != url.to_string()
    {
        report.add(
            "repository url",
            Level::Warning,
            format!(
                "the server gives this repository's URL as {name}, but hushrun uses {url}: CI's ID tokens are requested for {url}, so use {name} in HUSHRUN_REPOSITORY_URL and hushrun.yaml"
            ),
        );
    }
}

fn clock(report: &mut Report, url: &hushrun_core::ledger_url::LedgerUrl, app: &App<'_>) {
    match hushrun_ledger::server_time(url) {
        Ok(Some(server)) => {
            let now = SystemTime::from(app.now.timestamp());
            let skew = server.duration_since(now).unwrap_or_else(|e| e.duration());
            // HTTP dates have whole seconds.
            if skew > SKEW_WARNING {
                report.add(
                    "clock",
                    Level::Warning,
                    format!("this machine's clock is {} s off the storage service's; events compare times across machines", skew.as_secs()),
                );
            } else {
                report.add(
                    "clock",
                    Level::Ok,
                    format!("within {} s of the storage service", skew.as_secs().max(1)),
                );
            }
        }
        Ok(None) => report.add("clock", Level::Skipped, "no service to compare with"),
        Err(e) => report.add("clock", Level::Warning, format!("{e}")),
    }
}

/// \[ADR 0046\] `HUSHRUN_AGENT`: whether hushrun can read it, and whether its key can sign. A string
/// that can't costs a run a line, and a sandbox everything it shares, so here it is a failure.
fn agent_string(report: &mut Report, app: &App<'_>) {
    let wrong: Vec<String> = hushrun_ci::ignored(&app.env)
        .iter()
        .filter(|why| {
            matches!(
                why,
                hushrun_ci::CiError::Agent(_) | hushrun_ci::CiError::AgentStringIn(_)
            )
        })
        .map(ToString::to_string)
        .collect();
    if !wrong.is_empty() {
        report.add("agent", Level::Error, wrong.join("; "));
    } else if app.env.agent().is_some() {
        match crate::machine::load() {
            Ok(Some(machine)) => report.add(
                "agent",
                Level::Ok,
                format!(
                    "HUSHRUN_AGENT reads, and its key {} can sign",
                    crate::hosted::short_keyid(&machine.keyid())
                ),
            ),
            Ok(None) | Err(_) => report.add(
                "agent",
                Level::Error,
                crate::results::STRING_NEEDS_REPLACING,
            ),
        }
    }
}

/// T-12: the Sigstore trust root; what signs and whether its events count (T-15); and whether a
/// timestamp authority answers (E-10).
fn signing(report: &mut Report, app: &App<'_>, signers: &Signers) {
    let root = match app.sigstore_root() {
        Ok(root) => {
            report.add(
                "sigstore trust root",
                Level::Ok,
                format!(
                    "{} ({}): {} certificate authorities, {} timestamp authorities",
                    root.source,
                    root.digest,
                    root.certificate_authorities.len(),
                    root.timestamp_authorities.len()
                ),
            );
            root
        }
        Err(failure) => {
            report.add(
                "sigstore trust root",
                Level::Error,
                format!("{:#}", failure.error),
            );
            return;
        }
    };
    // An agent's machine key signs a pipeline outside GitHub Actions by default (T-12).
    let configured = [
        "HUSHRUN_SIGNING",
        "HUSHRUN_SIGNING_KEY",
        "HUSHRUN_SIGNING_KEY_FILE",
        "HUSHRUN_MACHINE_KEY",
        "HUSHRUN_MACHINE_KEY_FILE",
    ]
    .iter()
    .any(|v| app.env.get(v).is_some());
    if !configured {
        report.add(
            "signer",
            Level::Skipped,
            "no HUSHRUN_SIGNING, HUSHRUN_SIGNING_KEY or HUSHRUN_SIGNING_KEY_FILE: fine for jobs that only read",
        );
        return;
    }
    let signing = match app.signing_with(&root) {
        Ok(signing) => signing,
        Err(failure) => {
            report.add("signer", Level::Error, format!("{:#}", failure.error));
            return;
        }
    };
    let signer = signing.signer();
    match signer.public_key() {
        Some(key) => match signers.machine(&key.keyid()) {
            Some(machine) if machine.revoked.is_some() => report.add(
                "signer",
                Level::Error,
                format!(
                    "{key} is the machine {} of {}, revoked: its new events don't count",
                    machine.name, machine.owner
                ),
            ),
            Some(machine) => report.add(
                "signer",
                Level::Ok,
                format!(
                    "{} signs as {} (machine {})",
                    signer.describe(),
                    machine.owner,
                    machine.name
                ),
            ),
            None => report.add(
                "signer",
                Level::Error,
                format!(
                    "{key} isn't a machine the server lists for a member or an agent of the org: its events wouldn't count. Sign in with `hushrun login`, or give an agent the key `hushrun token create --agent` made with its token"
                ),
            ),
        },
        None => match app.keyless_identity() {
            Ok(Some(identity)) => match signers.ci_matching(&identity) {
                Some(_) => report.add(
                    "signer",
                    Level::Ok,
                    format!("{} signs as the repository's CI", signer.describe()),
                ),
                None => report.add(
                    "signer",
                    Level::Error,
                    format!(
                        "this job's identity ({}) isn't the repository's CI as the server names it: its events wouldn't count",
                        hushrun_attest::verify::describe(&identity)
                    ),
                ),
            },
            Ok(None) => report.add(
                "signer",
                Level::Warning,
                format!(
                    "{}: the identity Fulcio would certify is known only in GitHub Actions and GitLab CI jobs",
                    signer.describe()
                ),
            ),
            Err(failure) => report.add("signer", Level::Error, format!("{:#}", failure.error)),
        },
    }
    match signing.probe_timestamp() {
        Ok(Some(authority)) => report.add(
            "timestamps",
            Level::Ok,
            format!("{authority} answers with timestamps that verify"),
        ),
        Ok(None) => report.add(
            "timestamps",
            Level::Warning,
            "HUSHRUN_TSA=none: events count only while their machine counts",
        ),
        Err(why) => report.add(
            "timestamps",
            if signer.public_key().is_none() {
                Level::Error
            } else {
                Level::Warning
            },
            why,
        ),
    }
}

fn registries(report: &mut Report, policy: &Policy, args: &DoctorArgs) {
    let mut locations: Vec<&String> = policy.artifacts.values().map(|a| &a.location).collect();
    locations.sort();
    locations.dedup();
    if locations.is_empty() {
        report.add("registries", Level::Skipped, "no artifacts declared");
        return;
    }
    let artifacts = match connect(&args.registry) {
        Ok(artifacts) => artifacts,
        Err(failure) => {
            report.add("registries", Level::Warning, format!("{:#}", failure.error));
            return;
        }
    };
    for location in locations {
        let Ok(repository) = Repository::parse(location) else {
            continue;
        };
        match artifacts.check_access(&repository) {
            Ok(()) => report.add("registry", Level::Ok, format!("{location}: readable")),
            Err(e) => report.add(
                "registry",
                Level::Warning,
                format!("{location}: {e} (credentials may exist only in CI)"),
            ),
        }
    }
}

/// D-7: a scope must cover its deploy workflow, or editing the workflow doesn't change the
/// digest. GitHub's workflows, and GitLab's `.gitlab-ci.yml` with the files under `.gitlab/` it
/// may include.
fn workflows(report: &mut Report, content: &Content) {
    let yaml = |path: &str| path.ends_with(".yml") || path.ends_with(".yaml");
    let deploy: Vec<&str> = content
        .listing
        .entries
        .iter()
        .filter(|e| {
            e.path == ".gitlab-ci.yml"
                || ((e.path.starts_with(".github/workflows/") || e.path.starts_with(".gitlab/"))
                    && yaml(&e.path))
        })
        .filter(|e| {
            content
                .vcs
                .read_entry(e)
                .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains("hushrun "))
        })
        .map(|e| e.path.as_str())
        .collect();
    if deploy.is_empty() {
        report.add(
            "deploy workflow",
            Level::Skipped,
            "no workflow runs hushrun yet",
        );
        return;
    }
    let mut missing = Vec::new();
    for scope in content.scopes.iter() {
        for path in &deploy {
            if !content.scopes.contains(scope, path) {
                missing.push(format!("{} leaves out {path}", scope.name));
            }
        }
    }
    if missing.is_empty() {
        report.add(
            "deploy workflow",
            Level::Ok,
            format!("every scope covers {}", deploy.join(", ")),
        );
    } else {
        report.add(
            "deploy workflow",
            Level::Warning,
            format!(
                "{}: a workflow change wouldn't change the digest",
                missing.join("; ")
            ),
        );
    }
}

/// A-7: configuration baked into a build artifact differs between environments, so promoting
/// that artifact would ship staging's values to production.
fn baked_configuration(report: &mut Report, content: &Content) {
    const PATTERNS: [(&str, &str); 5] = [
        ("NEXT_PUBLIC_", "Next.js"),
        ("VITE_", "Vite"),
        ("REACT_APP_", "Create React App"),
        ("NUXT_PUBLIC_", "Nuxt"),
        ("$env/static/public", "SvelteKit"),
    ];
    let source = |path: &str| {
        [
            ".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".vue", ".svelte",
        ]
        .iter()
        .any(|ext| path.ends_with(ext))
            || path
                .rsplit('/')
                .next()
                .is_some_and(|name| name.starts_with(".env"))
    };
    let mut scanned = 0u64;
    let mut found: Vec<String> = Vec::new();
    for entry in content.listing.entries.iter().filter(|e| source(&e.path)) {
        if scanned > SCAN_BUDGET || found.len() >= 5 {
            break;
        }
        let Ok(bytes) = content.vcs.read_entry(entry) else {
            continue;
        };
        scanned += bytes.len() as u64;
        let text = String::from_utf8_lossy(&bytes);
        if let Some((pattern, framework)) = PATTERNS.iter().find(|(p, _)| text.contains(p)) {
            found.push(format!("{} ({framework}: {pattern})", entry.path));
        }
    }
    if found.is_empty() {
        report.add(
            "baked configuration",
            Level::Ok,
            "no build-time environment variables found",
        );
    } else {
        report.add(
            "baked configuration",
            Level::Warning,
            format!(
                "{}: build-time values are baked into the artifact, so one reused by digest carries the values it was built with",
                found.join(", ")
            ),
        );
    }
}

/// GitLab (ADR 0019), with `HUSHRUN_GITLAB_TOKEN`: the branch it deploys from being protected, since protected variables reach only protected branches
/// and who may push to it decides who signs its deploys; and who may set pipeline variables, which
/// outrank `HUSHRUN_REPOSITORY_URL` (T-4). A token whose role can't read a setting skips it.
fn gitlab(report: &mut Report, app: &App<'_>, policy: &Policy) {
    let Some(api) = hushrun_ci::gitlab::Api::from_env(&app.env) else {
        report.add(
            "gitlab api",
            if app.run.provider == Provider::Gitlab {
                Level::Warning
            } else {
                Level::Skipped
            },
            "set HUSHRUN_GITLAB_TOKEN (a project access token with read_api) so doctor can check the branch it deploys from and who may set pipeline variables",
        );
        return;
    };
    let project = match api.project() {
        Ok(project) => {
            report.add(
                "gitlab api",
                Level::Ok,
                format!("{} (project {})", project.path_with_namespace, project.id),
            );
            project
        }
        Err(e) => {
            report.add("gitlab api", Level::Error, e.to_string());
            return;
        }
    };
    let branches: Vec<&String> = policy
        .deploy
        .iter()
        .filter_map(|d| d.branch.as_ref())
        .collect();
    for branch in branches {
        match api.branch_protected(branch) {
            Ok(Some(true)) => report.add("protected branch", Level::Ok, format!("{branch} is protected")),
            Ok(Some(false)) => report.add(
                "protected branch",
                Level::Warning,
                format!(
                    "{branch} isn't protected: protected variables don't reach its pipelines, and anyone who can push to it can make a pipeline sign its deploys"
                ),
            ),
            Ok(None) => report.add(
                "protected branch",
                Level::Skipped,
                format!("HUSHRUN_GITLAB_TOKEN may not read whether {branch} is protected"),
            ),
            Err(e) => report.add("protected branch", Level::Warning, e.to_string()),
        }
    }
    let restricted = match (
        project
            .ci_pipeline_variables_minimum_override_role
            .as_deref(),
        project.restrict_user_defined_variables,
    ) {
        (Some(role @ ("no_one_allowed" | "owner" | "maintainer")), _) => {
            Some((true, role.to_string()))
        }
        (Some(role), _) => Some((false, role.to_string())),
        (None, Some(true)) => Some((true, "maintainer".to_string())),
        (None, Some(false)) => Some((false, "developer".to_string())),
        (None, None) => None,
    };
    match restricted {
        Some((true, role)) => report.add(
            "pipeline variables",
            Level::Ok,
            format!("only {} may set them", role.replace('_', " ")),
        ),
        Some((false, role)) => report.add(
            "pipeline variables",
            Level::Warning,
            format!(
                "the {role} role may run pipelines with variables, which override HUSHRUN_REPOSITORY_URL and the signing settings: set Settings → CI/CD → Variables → Minimum role to use pipeline variables to No one allowed"
            ),
        ),
        None => report.add(
            "pipeline variables",
            Level::Skipped,
            "HUSHRUN_GITLAB_TOKEN can't read who may set pipeline variables (that takes the Maintainer role)",
        ),
    }
}

fn finish(app: &App<'_>, report: Report) -> CmdResult {
    let failed = report.checks.iter().any(|(_, l, _)| *l == Level::Error);
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.doctor/v1",
            "ok": !failed,
            "checks": report.checks.iter().map(|(name, level, detail)| json!({
                "check": name, "status": level.as_str(), "detail": detail,
            })).collect::<Vec<_>>(),
        }))?,
        _ => {
            let mut text = String::new();
            for (name, level, detail) in &report.checks {
                let mark = match level {
                    Level::Ok => "ok  ",
                    Level::Warning => "warn",
                    Level::Error => "FAIL",
                    Level::Skipped => "skip",
                };
                let _ = writeln!(text, "{mark}  {name:<20} {detail}");
            }
            output::print(&text)?;
        }
    }
    Ok(if failed { Exit::Usage } else { Exit::Ok })
}

/// The `transparency_log` lines for what the server says about its log (T-9).
fn log_lines(log: &hushrun_ledger::LogInfo) -> String {
    format!(
        "transparency_log:\n  origin: {}\n  key: \"{}\"\n",
        log.origin,
        log.key
            .as_deref()
            .unwrap_or("<the key the repository's page shows>")
    )
}
