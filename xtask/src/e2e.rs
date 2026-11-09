//! `cargo xtask e2e`: drives a sandbox repository through hushrun's lifecycle and checks what
//! each pipeline decided (M6, M9, VALIDATION.md §9).
//!
//! The sandbox is set up as the provider's guide describes, with the recipe, the repository's one
//! pipeline on `main` (ADR 0062), and a `test.sh` that its `unit` check runs:
//!
//! - a GitHub repository (`--repo OWNER/NAME`, `docs/guides/github.md`), driven with an
//!   authenticated `gh`;
//! - a GitLab project (`--gitlab GROUP/PROJECT`, `docs/guides/gitlab.md`, "A sandbox project"),
//!   driven with an authenticated `glab`.
//!
//! The driver needs `git` too. It pushes commits, waits for the runs they start, and reads each
//! plan's decision from the log of the run's plan job (`<scope> <mode>: <reason>`).
//!
//! `--dry-run` prints the steps without touching the sandbox.
//!
//! `--scenario merge-queue` (plan G12) runs GitHub's merge queue end to end, and only when named:
//! the sandbox must be an organization's repository whose `main` requires a merge queue (a
//! ruleset), with a `ci.yml` that runs `hushrun run unit -- ./test.sh` on `pull_request`,
//! `merge_group` and pushes to `main`. A pull request passes; the queue's run of the same tree
//! skips; the push to `main` that merging makes skips too.
//!
//! \[M8\] With `--agent-key FILE`, an agent's scenario (E10) runs too: before pushing, the agent
//! validates with `hushrun run --all --isolated --worktree`, signed with that key, and the
//! pipeline's plan must then run none of its checks. The key is an agent's registered machine
//! key (the one `hushrun token create --agent` makes with its token, T-15), `unit` pins its
//! image, and the driver's machine has Docker or Podman and ledger credentials that can append
//! (the agent guide).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

/// The branch the pipeline deploys from.
const BRANCH: &str = "main";
/// The pipeline's workflow on GitHub, as `hushrun adopt` names it.
const DEPLOY_WORKFLOW: &str = "hushrun.yml";
/// \[G12\] The workflow the merge queue scenario reads.
const CI_WORKFLOW: &str = "ci.yml";
const POLL: Duration = Duration::from_secs(15);
const RUN_TIMEOUT: Duration = Duration::from_secs(25 * 60);
const MODES: [&str; 5] = ["promote", "refresh", "full", "current", "blocked"];
/// Who the driver's commits are by.
const IDENTITY: [&str; 4] = [
    "-c",
    "user.name=hushrun e2e",
    "-c",
    "user.email=e2e@example.invalid",
];

/// Where the sandbox lives.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Host {
    /// `OWNER/NAME` on GitHub.
    Github(String),
    /// `GROUP/PROJECT` on GitLab (`GITLAB_HOST` for a self-managed instance, as for `glab`).
    Gitlab(String),
}

struct Options {
    host: Option<Host>,
    dry_run: bool,
    only: Option<String>,
    /// \[M8\] The agent's signing key, for the agent's scenario.
    agent_key: Option<PathBuf>,
    /// The hushrun the agent runs.
    hushrun: String,
}

pub fn run(_root: &Path, args: &[String]) -> Result<bool> {
    let mut options = Options {
        host: None,
        dry_run: false,
        only: None,
        agent_key: None,
        hushrun: "hushrun".into(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--repo" => {
                options.host = Some(Host::Github(
                    args.next().context("--repo OWNER/NAME")?.clone(),
                ))
            }
            "--gitlab" => {
                options.host = Some(Host::Gitlab(
                    args.next().context("--gitlab GROUP/PROJECT")?.clone(),
                ))
            }
            "--dry-run" => options.dry_run = true,
            "--scenario" => options.only = Some(args.next().context("--scenario NAME")?.clone()),
            "--agent-key" => {
                options.agent_key = Some(PathBuf::from(args.next().context("--agent-key FILE")?));
            }
            "--hushrun" => options.hushrun = args.next().context("--hushrun PATH")?.clone(),
            other => bail!("unknown argument {other}"),
        }
    }
    match &options.host {
        Some(Host::Github(repo)) if repo.split('/').count() == 2 => {}
        Some(Host::Gitlab(project))
            if project.split('/').count() >= 2 && !project.split('/').any(str::is_empty) => {}
        _ => bail!(
            "--repo OWNER/NAME names a GitHub sandbox repository, or --gitlab GROUP/PROJECT a GitLab sandbox project"
        ),
    }
    let named: Vec<&(&str, Scenario)> = BY_NAME
        .iter()
        .filter(|(name, _)| options.only.as_deref() == Some(*name))
        .collect();
    if let Some((name, _)) = named.first()
        && !matches!(options.host, Some(Host::Github(_)))
    {
        bail!("--scenario {name} runs on GitHub alone: pass --repo OWNER/NAME");
    }
    let sandbox = Sandbox::open(&options)?;
    let mut passed = true;
    for (name, scenario) in SCENARIOS.iter().chain(named) {
        if options.only.as_deref().is_some_and(|only| only != *name) {
            continue;
        }
        eprintln!("== {name}");
        match scenario(&sandbox) {
            Ok(()) => eprintln!("   ok"),
            Err(err) => {
                eprintln!("   FAILED: {err:#}");
                passed = false;
            }
        }
    }
    Ok(passed)
}

type Scenario = fn(&Sandbox) -> Result<()>;

/// The lifecycle of plan §8.7, on a real repository.
const SCENARIOS: &[(&str, Scenario)] = &[
    (
        "new content: the pipeline builds and deploys it",
        new_content,
    ),
    ("the same content again: the pipeline skips", same_content),
    ("the tests fail: the pipeline deploys nothing", tests_fail),
    (
        "an agent's isolated evidence: the pipeline runs none of its checks",
        agent_evidence,
    ),
];

/// Scenarios that run only when `--scenario` names them, since the sandbox needs more for them.
const BY_NAME: &[(&str, Scenario)] = &[("merge-queue", merge_queue)];

/// \[G12\] A pull request passes, the merge queue's run of the same tree skips, and so does the
/// push to `main` that merging it makes: keys come from content, whatever the ref.
fn merge_queue(s: &Sandbox) -> Result<()> {
    let branch = format!("e2e-queue-{}", std::process::id());
    let head = s.commit_on_new_branch(
        &branch,
        "app.txt",
        &format!("queued {}\n", s.nonce()),
        "e2e: through the merge queue",
    )?;
    let number = s.open_pull_request(&branch)?;
    let pull = s.wait_for(CI_WORKFLOW, "pull_request", &Wanted::Head(&head))?;
    s.expect_line(&pull, "success", "hushrun: unit passed")?;
    // With a queue required, this adds the pull request to it.
    s.gh(&["pr", "merge", &number.to_string()])?;
    let queued = s.wait_for(CI_WORKFLOW, "merge_group", &Wanted::Queued(number))?;
    s.expect_line(&queued, "success", "hushrun: unit skipped")?;
    // Merging moves `main` to the queue's commit.
    let merged = s.wait_for(CI_WORKFLOW, "push", &Wanted::Head(&queued.head))?;
    s.expect_line(&merged, "success", "hushrun: unit skipped")
}

fn new_content(s: &Sandbox) -> Result<()> {
    let sha = s.commit(
        "app.txt",
        &format!("change {}\n", s.nonce()),
        "e2e: new content",
    )?;
    let run = s.wait(&sha)?;
    s.expect(&run, "success", "full")
}

fn same_content(s: &Sandbox) -> Result<()> {
    // Markdown is outside the digest: main moves on, the digest doesn't, and main runs it.
    let sha = s.commit(
        "NOTES.md",
        &format!("note {}\n", s.nonce()),
        "e2e: docs only",
    )?;
    let run = s.wait(&sha)?;
    s.expect(&run, "success", "current")
}

fn tests_fail(s: &Sandbox) -> Result<()> {
    let sha = s.commit("test.sh", "#!/bin/sh\nexit 1\n", "e2e: failing tests")?;
    let run = s.wait(&sha)?;
    s.expect(&run, "failure", "full")?;
    // Fix forward, so later runs start from passing tests.
    let sha = s.commit("test.sh", "#!/bin/sh\nexit 0\n", "e2e: fix tests")?;
    s.wait(&sha).map(|_| ())
}

/// E10 (M8): the agent validates the change in the pinned image before pushing it, and the
/// pipeline's plan counts that evidence.
fn agent_evidence(s: &Sandbox) -> Result<()> {
    let Some(key) = &s.agent_key else {
        eprintln!("   skipped: pass --agent-key FILE");
        return Ok(());
    };
    s.git(&["fetch", "--quiet", "origin"])?;
    s.git(&[
        "checkout",
        "--quiet",
        "-B",
        BRANCH,
        &format!("origin/{BRANCH}"),
    ])?;
    if !s.dry_run {
        std::fs::write(s.dir.join("app.txt"), format!("agent {}\n", s.nonce()))?;
    }
    s.agent(&["run", "--all", "--isolated", "--worktree"], key)?;
    s.git(&["add", "app.txt"])?;
    s.git(&[
        "-c",
        "user.name=hushrun e2e agent",
        "-c",
        "user.email=e2e@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "e2e: an agent's change, validated before the push",
    ])?;
    s.git(&["push", "--quiet", "origin", BRANCH])?;
    let sha = s.git(&["rev-parse", "HEAD"])?;
    let run = s.wait(&sha)?;
    s.expect(&run, "success", "full")?;
    s.expect_no_checks(&run)
}

/// A completed run: a GitHub workflow run, or a GitLab pipeline. `conclusion` is GitHub's
/// word: `success`, `failure`, `cancelled`, …
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    id: u64,
    conclusion: String,
    /// The commit it ran: on GitHub, `headSha`.
    head: String,
}

/// \[G12\] Which run of a workflow to wait for.
enum Wanted<'a> {
    /// The run of this commit.
    Head(&'a str),
    /// The merge queue's run of this pull request: its branch is
    /// `gh-readonly-queue/<base>/pr-<n>-<sha>`.
    Queued(u64),
}

struct Sandbox {
    host: Host,
    dir: PathBuf,
    dry_run: bool,
    agent_key: Option<PathBuf>,
    hushrun: String,
    _temp: Option<TempDir>,
}

/// A temporary directory removed on drop.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Sandbox {
    fn open(options: &Options) -> Result<Sandbox> {
        let host = options.host.clone().context("no sandbox")?;
        let dir = std::env::temp_dir().join(format!("hushrun-e2e-{}", std::process::id()));
        let name = match &host {
            Host::Github(repo) | Host::Gitlab(repo) => repo.clone(),
        };
        if options.dry_run {
            eprintln!("would clone {name} into {}", dir.display());
            return Ok(Sandbox {
                host,
                dir,
                dry_run: true,
                agent_key: options.agent_key.clone(),
                hushrun: options.hushrun.clone(),
                _temp: None,
            });
        }
        let path = dir.to_str().context("temp path")?;
        match &host {
            Host::Github(repo) => {
                exec("gh", &["auth", "status"], None)
                    .context("gh must be installed and logged in")?;
                exec("gh", &["repo", "clone", repo, path, "--", "--quiet"], None)?;
            }
            Host::Gitlab(project) => {
                exec("glab", &["auth", "status"], None)
                    .context("glab must be installed and logged in")?;
                exec(
                    "glab",
                    &["repo", "clone", project, path, "--", "--quiet"],
                    None,
                )?;
            }
        }
        Ok(Sandbox {
            host,
            dir: dir.clone(),
            dry_run: false,
            agent_key: options.agent_key.clone(),
            hushrun: options.hushrun.clone(),
            _temp: Some(TempDir(dir)),
        })
    }

    fn nonce(&self) -> String {
        format!("{:?}", std::time::SystemTime::now())
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        if self.dry_run {
            eprintln!("   would run: git {}", args.join(" "));
            return Ok("0".repeat(40));
        }
        exec("git", args, Some(&self.dir))
    }

    /// Runs hushrun in the clone as the agent: its key, on a person's machine, not in CI.
    fn agent(&self, args: &[&str], key: &Path) -> Result<()> {
        if self.dry_run {
            eprintln!("   would run as the agent: hushrun {}", args.join(" "));
            return Ok(());
        }
        let status = Command::new(&self.hushrun)
            .args(args)
            .current_dir(&self.dir)
            .env("HUSHRUN_SIGNING_KEY_FILE", key)
            .env("HUSHRUN_PROVIDER", "local")
            .env("HUSHRUN_INVOKED_BY", "agent:e2e")
            // It shares this terminal, where hushrun would say when a newer release is out.
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .status()
            .with_context(|| format!("running {}", self.hushrun))?;
        if !status.success() {
            bail!("hushrun {} ended {status}", args.join(" "));
        }
        Ok(())
    }

    /// Commits `content` to `file` on `main` and pushes it; returns the commit.
    fn commit(&self, file: &str, content: &str, message: &str) -> Result<String> {
        self.git(&["fetch", "--quiet", "origin"])?;
        self.git(&[
            "checkout",
            "--quiet",
            "-B",
            BRANCH,
            &format!("origin/{BRANCH}"),
        ])?;
        if !self.dry_run {
            std::fs::write(self.dir.join(file), content)?;
        }
        self.git(&["add", file])?;
        self.git(&[&IDENTITY[..], &["commit", "--quiet", "-m", message]].concat())?;
        self.git(&["push", "--quiet", "origin", BRANCH])?;
        self.git(&["rev-parse", "HEAD"])
    }

    /// Waits for the pipeline's run for `sha` to complete: on GitHub, its workflow's run on
    /// `main`; on GitLab, the push pipeline on `main`.
    fn wait(&self, sha: &str) -> Result<Run> {
        let branch = BRANCH;
        if self.dry_run {
            eprintln!("   would wait for the pipeline on {branch} at {sha}");
            return Ok(Run {
                id: 0,
                conclusion: "success".into(),
                head: sha.to_string(),
            });
        }
        let started = Instant::now();
        loop {
            let found = match &self.host {
                Host::Github(repo) => {
                    let listed = exec(
                        "gh",
                        &[
                            "run",
                            "list",
                            "--repo",
                            repo,
                            "--workflow",
                            DEPLOY_WORKFLOW,
                            "--branch",
                            branch,
                            "--json",
                            "databaseId,headSha,status,conclusion",
                            "--limit",
                            "20",
                        ],
                        None,
                    )?;
                    completed_run(&listed, sha)?
                }
                Host::Gitlab(project) => {
                    let listed = exec(
                        "glab",
                        &[
                            "api",
                            &format!(
                                "projects/{}/pipelines?ref={branch}&sha={sha}&source=push&per_page=20",
                                encode(project)
                            ),
                        ],
                        None,
                    )?;
                    completed_pipeline(&listed, sha)?
                }
            };
            if let Some(run) = found {
                return Ok(run);
            }
            if started.elapsed() > RUN_TIMEOUT {
                bail!(
                    "the pipeline on {branch} at {sha} didn't complete in {} min",
                    RUN_TIMEOUT.as_secs() / 60
                );
            }
            std::thread::sleep(POLL);
        }
    }

    /// The log a run's plan step or job wrote.
    fn plan_log(&self, found: &Run) -> Result<String> {
        match &self.host {
            Host::Github(repo) => exec(
                "gh",
                &[
                    "run",
                    "view",
                    &found.id.to_string(),
                    "--repo",
                    repo,
                    "--log",
                ],
                None,
            ),
            Host::Gitlab(project) => {
                let project = encode(project);
                let jobs = exec(
                    "glab",
                    &[
                        "api",
                        &format!(
                            "projects/{project}/pipelines/{}/jobs?per_page=100",
                            found.id
                        ),
                    ],
                    None,
                )?;
                let job = plan_job(&jobs)?
                    .ok_or_else(|| anyhow!("pipeline {} has no plan job", found.id))?;
                exec(
                    "glab",
                    &["api", &format!("projects/{project}/jobs/{job}/trace")],
                    None,
                )
            }
        }
    }

    /// Checks the run's conclusion and the mode its plan decided.
    fn expect(&self, found: &Run, conclusion: &str, mode: &str) -> Result<()> {
        if self.dry_run {
            eprintln!("   would expect {conclusion} with mode {mode}");
            return Ok(());
        }
        let log = self.plan_log(found)?;
        let decided = decided_mode(&log)
            .ok_or_else(|| anyhow!("run {} logged no plan decision", found.id))?;
        if found.conclusion != conclusion || decided != mode {
            bail!(
                "run {} ended {} with mode {decided}; expected {conclusion} with {mode}",
                found.id,
                found.conclusion
            );
        }
        Ok(())
    }
}

impl Sandbox {
    /// `gh` against the sandbox repository.
    fn gh(&self, args: &[&str]) -> Result<String> {
        let Host::Github(repo) = &self.host else {
            bail!("the sandbox isn't on GitHub");
        };
        if self.dry_run {
            eprintln!("   would run: gh {} --repo {repo}", args.join(" "));
            return Ok(String::new());
        }
        exec("gh", &[args, &["--repo", repo]].concat(), None)
    }

    /// Commits `content` to `file` on a new branch from `main` and pushes it; returns the commit.
    fn commit_on_new_branch(
        &self,
        branch: &str,
        file: &str,
        content: &str,
        message: &str,
    ) -> Result<String> {
        self.git(&["fetch", "--quiet", "origin"])?;
        self.git(&["checkout", "--quiet", "-B", branch, "origin/main"])?;
        if !self.dry_run {
            std::fs::write(self.dir.join(file), content)?;
        }
        self.git(&["add", file])?;
        self.git(&[&IDENTITY[..], &["commit", "--quiet", "-m", message]].concat())?;
        self.git(&["push", "--quiet", "origin", branch])?;
        self.git(&["rev-parse", "HEAD"])
    }

    /// Opens a pull request of `branch` into `main`; returns its number.
    fn open_pull_request(&self, branch: &str) -> Result<u64> {
        let url = self.gh(&[
            "pr",
            "create",
            "--base",
            "main",
            "--head",
            branch,
            "--title",
            "e2e: through the merge queue",
            "--body",
            "hushrun's merge queue scenario (cargo xtask e2e --scenario merge-queue).",
        ])?;
        if self.dry_run {
            return Ok(1);
        }
        pull_request_number(&url).ok_or_else(|| anyhow!("gh pr create printed {url:?}"))
    }

    /// Waits for the completed run of `workflow` on `event` that `wanted` names.
    fn wait_for(&self, workflow: &str, event: &str, wanted: &Wanted<'_>) -> Result<Run> {
        let what = match wanted {
            Wanted::Head(sha) => format!("{workflow} on {event} at {sha}"),
            Wanted::Queued(number) => format!("{workflow} on {event} for #{number}"),
        };
        if self.dry_run {
            eprintln!("   would wait for {what}");
            return Ok(Run {
                id: 0,
                conclusion: "success".into(),
                head: "0".repeat(40),
            });
        }
        let started = Instant::now();
        loop {
            let listed = self.gh(&[
                "run",
                "list",
                "--workflow",
                workflow,
                "--event",
                event,
                "--json",
                "databaseId,headSha,headBranch,status,conclusion",
                "--limit",
                "20",
            ])?;
            if let Some(run) = completed_wanted(&listed, wanted)? {
                return Ok(run);
            }
            if started.elapsed() > RUN_TIMEOUT {
                bail!(
                    "{what} didn't complete in {} min",
                    RUN_TIMEOUT.as_secs() / 60
                );
            }
            std::thread::sleep(POLL);
        }
    }

    /// Checks the run's conclusion, and that its log holds `line`.
    fn expect_line(&self, found: &Run, conclusion: &str, line: &str) -> Result<()> {
        if self.dry_run {
            eprintln!("   would expect {conclusion}, and the log to say {line:?}");
            return Ok(());
        }
        let log = self.plan_log(found)?;
        if found.conclusion != conclusion || !log.contains(line) {
            bail!(
                "run {} ended {}, and its log says {line:?}: {}; expected {conclusion}",
                found.id,
                found.conclusion,
                if log.contains(line) { "yes" } else { "no" }
            );
        }
        Ok(())
    }

    /// Checks that the run's plan had no checks to run.
    fn expect_no_checks(&self, found: &Run) -> Result<()> {
        if self.dry_run {
            eprintln!("   would expect no checks to run");
            return Ok(());
        }
        let log = self.plan_log(found)?;
        match planned_checks(&log) {
            Some(checks) => bail!("run {} still ran {checks}", found.id),
            None => Ok(()),
        }
    }
}

/// The checks a plan logged it would run (`  checks to run: unit lint`), if any.
fn planned_checks(log: &str) -> Option<&str> {
    log.lines().find_map(|line| {
        line.split_once("checks to run: ")
            .map(|(_, checks)| checks.trim())
    })
}

/// \[G12\] The number of the pull request whose URL `gh pr create` printed last.
fn pull_request_number(printed: &str) -> Option<u64> {
    let url = printed.lines().rev().find(|line| line.contains("/pull/"))?;
    url.trim().rsplit_once("/pull/")?.1.parse().ok()
}

/// \[G12\] The completed run `wanted` names in `gh run list --json` output, with
/// `headBranch`.
fn completed_wanted(listed: &str, wanted: &Wanted<'_>) -> Result<Option<Run>> {
    let runs: Value = serde_json::from_str(listed).context("gh run list output")?;
    Ok(runs.as_array().into_iter().flatten().find_map(|r| {
        let head = r["headSha"].as_str()?;
        let matches = match wanted {
            Wanted::Head(sha) => head == *sha,
            Wanted::Queued(number) => r["headBranch"]
                .as_str()
                .and_then(|branch| branch.strip_prefix("gh-readonly-queue/"))
                .and_then(|rest| rest.rsplit_once('/'))
                .is_some_and(|(_, entry)| entry.starts_with(&format!("pr-{number}-"))),
        };
        (matches && r["status"] == "completed").then(|| Run {
            id: r["databaseId"].as_u64().unwrap_or(0),
            conclusion: r["conclusion"].as_str().unwrap_or("").to_string(),
            head: head.to_string(),
        })
    }))
}

/// The completed run for `sha` in `gh run list --json` output.
fn completed_run(listed: &str, sha: &str) -> Result<Option<Run>> {
    let runs: Value = serde_json::from_str(listed).context("gh run list output")?;
    Ok(runs.as_array().into_iter().flatten().find_map(|r| {
        (r["headSha"] == sha && r["status"] == "completed").then(|| Run {
            id: r["databaseId"].as_u64().unwrap_or(0),
            conclusion: r["conclusion"].as_str().unwrap_or("").to_string(),
            head: sha.to_string(),
        })
    }))
}

/// The newest completed pipeline for `sha` in the pipelines API's output, its status in
/// GitHub's words.
fn completed_pipeline(listed: &str, sha: &str) -> Result<Option<Run>> {
    let pipelines: Value = serde_json::from_str(listed).context("the pipelines API's output")?;
    Ok(pipelines.as_array().into_iter().flatten().find_map(|p| {
        let conclusion = match p["status"].as_str()? {
            "success" => "success",
            "failed" => "failure",
            "canceled" => "cancelled",
            "skipped" => "skipped",
            _ => return None,
        };
        (p["sha"] == sha).then(|| Run {
            id: p["id"].as_u64().unwrap_or(0),
            conclusion: conclusion.to_string(),
            head: sha.to_string(),
        })
    }))
}

/// The id of the newest job named `plan` in the pipeline jobs API's output.
fn plan_job(jobs: &str) -> Result<Option<u64>> {
    let jobs: Value = serde_json::from_str(jobs).context("the jobs API's output")?;
    Ok(jobs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|j| j["name"] == "plan")
        .filter_map(|j| j["id"].as_u64())
        .max())
}

/// The first `<scope> <mode>: `, or `<mode>: ` for the scope `default`, a plan logged.
fn decided_mode(log: &str) -> Option<&'static str> {
    log.lines().find_map(|line| {
        MODES
            .iter()
            .find(|mode| {
                let said = format!("{mode}: ");
                line.starts_with(&said)
                    || line.contains(&format!(" {said}"))
                    || line.contains(&format!("\t{said}"))
            })
            .copied()
    })
}

/// A project path for the API: `group/sub/name` → `group%2Fsub%2Fname`.
fn encode(project: &str) -> String {
    project.replace('/', "%2F")
}

fn exec(program: &str, args: &[&str], dir: Option<&Path>) -> Result<String> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_merge_queues_run_of_a_pull_request() {
        let head = "0123456789abcdef0123456789abcdef01234567";
        let listed = format!(
            r#"[
            {{"databaseId": 9, "headSha": "{head}", "headBranch": "gh-readonly-queue/main/pr-12-{head}", "status": "in_progress", "conclusion": ""}},
            {{"databaseId": 8, "headSha": "ccc", "headBranch": "gh-readonly-queue/main/pr-121-{head}", "status": "completed", "conclusion": "success"}},
            {{"databaseId": 7, "headSha": "ddd", "headBranch": "gh-readonly-queue/release/1.x/pr-12-{head}", "status": "completed", "conclusion": "success"}}
        ]"#
        );
        // The first one that matches and completed: pr-121 isn't pr-12.
        assert_eq!(
            completed_wanted(&listed, &Wanted::Queued(12)).unwrap(),
            Some(Run {
                id: 7,
                conclusion: "success".into(),
                head: "ddd".into()
            })
        );
        assert_eq!(
            completed_wanted(&listed, &Wanted::Head("ccc"))
                .unwrap()
                .map(|run| run.id),
            Some(8)
        );
        assert_eq!(completed_wanted(&listed, &Wanted::Queued(5)).unwrap(), None);
        assert_eq!(
            pull_request_number("Creating pull request\nhttps://github.com/acme/sandbox/pull/12\n"),
            Some(12)
        );
        assert_eq!(pull_request_number("no URL here"), None);
    }

    #[test]
    fn finds_the_completed_run_of_a_commit() {
        let listed = r#"[
            {"databaseId": 3, "headSha": "bbb", "status": "in_progress", "conclusion": ""},
            {"databaseId": 2, "headSha": "aaa", "status": "completed", "conclusion": "success"}
        ]"#;
        assert_eq!(
            completed_run(listed, "aaa").unwrap(),
            Some(Run {
                id: 2,
                conclusion: "success".into(),
                head: "aaa".into()
            })
        );
        assert_eq!(completed_run(listed, "bbb").unwrap(), None);
    }

    #[test]
    fn finds_the_completed_pipeline_of_a_commit() {
        let listed = r#"[
            {"id": 12, "sha": "bbb", "status": "running", "ref": "main"},
            {"id": 11, "sha": "aaa", "status": "failed", "ref": "main"},
            {"id": 10, "sha": "aaa", "status": "success", "ref": "main"}
        ]"#;
        assert_eq!(
            completed_pipeline(listed, "aaa").unwrap(),
            Some(Run {
                id: 11,
                conclusion: "failure".into(),
                head: "aaa".into()
            })
        );
        assert_eq!(completed_pipeline(listed, "bbb").unwrap(), None);
        let jobs =
            r#"[{"id": 5, "name": "test"}, {"id": 4, "name": "plan"}, {"id": 7, "name": "plan"}]"#;
        assert_eq!(plan_job(jobs).unwrap(), Some(7));
        assert_eq!(plan_job("[]").unwrap(), None);
        assert_eq!(encode("acme/sub/back"), "acme%2Fsub%2Fback");
    }

    #[test]
    fn reads_the_checks_a_plan_runs_from_a_log() {
        let log = "plan\thushrun plan\t2026-10-16T00:00:00Z full: no reusable `image`\nplan\thushrun plan\t2026-10-16T00:00:01Z   checks to run: unit lint\n";
        assert_eq!(planned_checks(log), Some("unit lint"));
        assert_eq!(planned_checks("full: building"), None);
    }

    #[test]
    fn reads_the_decision_from_a_log() {
        let log = "plan\tRun hushrun plan --start\t2026-10-14T12:00:00Z api promote: every required check is met, reusing image\n";
        assert_eq!(decided_mode(log), Some("promote"));
        // The scope `default` goes unnamed.
        let log = "plan\tRun hushrun plan --start\t2026-10-14T12:00:00Z current: this repository already runs sha256:329354c1d5b2\n";
        assert_eq!(decided_mode(log), Some("current"));
        assert_eq!(
            decided_mode("blocked: this repository deploys only from main\n"),
            Some("blocked")
        );
        // `wait` went with upstreams (ADR 0062).
        assert_eq!(decided_mode("wait: an upstream run is still going\n"), None);
        assert_eq!(decided_mode("nothing here"), None);
    }

    #[test]
    fn the_scenarios_commit_to_main_one_after_another() {
        // The scenarios' pushes in order, against a local remote: each lands on `main`, whose
        // head is then the commit the driver waits for.
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote.git");
        let dir = temp.path().join("clone");
        let remote = remote.to_str().unwrap();
        exec("git", &["init", "--quiet", "--bare", remote], None).unwrap();
        exec(
            "git",
            &["clone", "--quiet", remote, dir.to_str().unwrap()],
            None,
        )
        .unwrap();
        let s = Sandbox {
            host: Host::Gitlab("acme/back".into()),
            dir,
            dry_run: false,
            agent_key: None,
            hushrun: "hushrun".into(),
            _temp: None,
        };
        std::fs::write(s.dir.join("app.txt"), "initial\n").unwrap();
        s.git(&["add", "app.txt"]).unwrap();
        s.git(&[&IDENTITY[..], &["commit", "--quiet", "-m", "initial"]].concat())
            .unwrap();
        s.git(&["push", "--quiet", "origin", "HEAD:refs/heads/main"])
            .unwrap();

        let mut pushed = Vec::new();
        for (file, content, message) in [
            ("app.txt", "change\n", "new content"),
            ("NOTES.md", "note\n", "docs only"),
            ("test.sh", "#!/bin/sh\nexit 1\n", "failing tests"),
            ("test.sh", "#!/bin/sh\nexit 0\n", "fix tests"),
            ("app.txt", "again\n", "new content"),
        ] {
            let sha = s.commit(file, content, message).unwrap();
            s.git(&["fetch", "--quiet", "origin"]).unwrap();
            assert_eq!(
                s.git(&["rev-parse", "origin/main"]).unwrap(),
                sha,
                "{message}"
            );
            assert!(!pushed.contains(&sha), "{message}");
            pushed.push(sha);
        }
        // One after another: each commit's parent is the one before it.
        let parent = s.git(&["rev-parse", "HEAD~1"]).unwrap();
        assert_eq!(parent, pushed[pushed.len() - 2]);
    }

    #[test]
    fn a_dry_run_on_gitlab_touches_nothing() {
        let args: Vec<String> = ["--gitlab", "acme/back", "--dry-run"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(run(Path::new("."), &args).unwrap());
        let bad: Vec<String> = ["--gitlab", "back"].iter().map(|s| s.to_string()).collect();
        assert!(run(Path::new("."), &bad).is_err());
    }
}
