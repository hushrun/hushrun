//! CI providers (§11.2): detection, run identity, outputs, summaries, annotations,
//! liveness and pending runs.
//!
//! GitHub Actions (M6) and GitLab CI (M9) are native. Until their milestones, Buildkite and
//! CircleCI are detected (so T-4 applies) but identify runs like the generic provider, through
//! `HUSHRUN_RUN_ID`.
#![forbid(unsafe_code)]

pub mod github;
pub mod gitlab;

use std::collections::BTreeMap;
use std::env::VarError;
use std::ffi::OsString;
use std::fmt;
use std::sync::Once;

use hushrun_core::agent::{self, AgentString, AgentStringError};
use hushrun_core::ledger_url::LedgerUrl;
use hushrun_core::run::{Provider, Trigger};

/// What `HUSHRUN_AGENT` stands in for: what a sandbox set by hand before it. See [`Env::of`].
const TOKEN: &str = "HUSHRUN_TOKEN";
const MACHINE_KEY: &str = "HUSHRUN_MACHINE_KEY";
const MACHINE_KEY_FILE: &str = "HUSHRUN_MACHINE_KEY_FILE";
const BACKEND: &str = "HUSHRUN_BACKEND";
/// The variables a readable `HUSHRUN_AGENT` answers.
const STOOD_IN_FOR: [&str; 4] = [TOKEN, MACHINE_KEY, MACHINE_KEY_FILE, BACKEND];
/// hushrun's own variables, of which any but `HUSHRUN_AGENT` that holds a string is dropped.
const OWN: &str = "HUSHRUN_";

/// A snapshot of the environment variables hushrun reads.
///
/// \[ADR 0046\] It is also where `HUSHRUN_AGENT` is read. hushrun can't set variables (unsafe in
/// edition 2024, and it would hand the decoded key to the command `hushrun run` starts), so the
/// snapshot does it instead: every reader of hushrun's variables goes through here, or through
/// [`var`] and [`var_os`], and sees what the string holds under the old names, and no string in
/// any other variable, where a message would print it or a request carry it.
#[derive(Clone, Default)]
pub struct Env {
    vars: BTreeMap<String, String>,
    /// `HUSHRUN_AGENT`, read, when it holds anything.
    agent: Option<Result<AgentString, AgentStringError>>,
    /// The variables a readable `HUSHRUN_AGENT` overrode, with other values than its own.
    overridden: Vec<&'static str>,
    /// hushrun's variables that held a `HUSHRUN_AGENT` string, and were dropped.
    misplaced: Vec<String>,
}

/// Names only: the values hold tokens and keys.
impl fmt::Debug for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.vars.keys()).finish()
    }
}

impl Env {
    /// The process environment (UTF-8 variables only: another one, whatever its name, is left
    /// out rather than stopping the command). Only what says an agent runs here stays, whatever
    /// else its value holds: `HUSHRUN_AGENT`, and a string in another of hushrun's variables.
    pub fn from_process() -> Env {
        Env::of(
            std::env::vars_os()
                .filter_map(|(name, value)| {
                    let name = name.into_string().ok()?;
                    let value = value.into_string().or_else(|value| {
                        let lossy = value.to_string_lossy().into_owned();
                        let says_so = name == agent::VARIABLE
                            || (name.starts_with(OWN) && agent::is_agent_string(&lossy));
                        says_so.then_some(lossy).ok_or(())
                    });
                    Some((name, value.ok()?))
                })
                .collect(),
        )
    }

    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Env {
        Env::of(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    /// `vars`, with `HUSHRUN_AGENT` read: a string that can be read wins, whole, over the token,
    /// the key and the backend beside it, since a token and a key from two places don't go
    /// together. Only an empty `HUSHRUN_BACKEND` stays, which turns results off (C-11). A string
    /// in any other of hushrun's variables is dropped, whatever is around it: a token there would
    /// be sent, and most of the rest printed when they can't be read.
    fn of(mut vars: BTreeMap<String, String>) -> Env {
        let misplaced: Vec<String> = vars
            .iter()
            .filter(|(name, value)| {
                name.starts_with(OWN) && *name != agent::VARIABLE && agent::is_agent_string(value)
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in &misplaced {
            vars.remove(name);
        }
        let agent = vars
            .get(agent::VARIABLE)
            .filter(|text| !text.trim().is_empty())
            .map(|text| AgentString::parse(text));
        let mut overridden = Vec::new();
        if let Some(Ok(agent)) = &agent {
            let backend = agent.backend().to_string();
            let held = |name: &str| vars.get(name).map(|v| v.trim()).filter(|v| !v.is_empty());
            if held(TOKEN).is_some_and(|v| v != agent.token()) {
                overridden.push(TOKEN);
            }
            if held(MACHINE_KEY).is_some_and(|v| v != agent.key().trim()) {
                overridden.push(MACHINE_KEY);
            }
            if held(MACHINE_KEY_FILE).is_some() {
                overridden.push(MACHINE_KEY_FILE);
            }
            if held(BACKEND)
                .is_some_and(|v| LedgerUrl::parse(v).map_or(true, |url| url != *agent.backend()))
            {
                overridden.push(BACKEND);
            }
            let results_off = vars.get(BACKEND).is_some_and(|v| v.trim().is_empty());
            if !results_off {
                vars.insert(BACKEND.to_string(), backend);
            }
            vars.insert(TOKEN.to_string(), agent.token().to_string());
            vars.insert(MACHINE_KEY.to_string(), agent.key().to_string());
            vars.remove(MACHINE_KEY_FILE);
        }
        Env {
            vars,
            agent,
            overridden,
            misplaced,
        }
    }

    /// \[ADR 0046\] The `HUSHRUN_AGENT` in effect, when one is set and hushrun can read it.
    pub fn agent(&self) -> Option<&AgentString> {
        self.agent.as_ref().and_then(|agent| agent.as_ref().ok())
    }

    /// \[ADR 0046\] Whether `HUSHRUN_AGENT` holds anything, readable or not, or a string is in
    /// another variable. Whoever set it means an agent, so this, and not [`Env::agent`], decides
    /// what an agent may not do.
    pub fn agent_set(&self) -> bool {
        self.agent.is_some() || !self.misplaced.is_empty()
    }

    /// \[ADR 0046\] Whether an agent runs here and no string can be read: the case where a person's
    /// login or key, if one is around, must not speak for it.
    pub fn agent_unreadable(&self) -> bool {
        self.agent_set() && self.agent().is_none()
    }

    /// \[ADR 0046\] What's wrong with `HUSHRUN_AGENT`, when hushrun can't read it.
    pub fn agent_error(&self) -> Option<AgentStringError> {
        self.agent
            .as_ref()
            .and_then(|agent| agent.as_ref().err())
            .copied()
    }

    /// \[ADR 0046\] The variables a `HUSHRUN_AGENT` overrode.
    pub fn overridden(&self) -> &[&'static str] {
        &self.overridden
    }

    /// Sets a variable, as if the process had it.
    pub fn set(&mut self, name: &str, value: &str) {
        self.vars.insert(name.to_string(), value.to_string());
    }

    /// A variable's value, if set and not empty.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    fn is_true(&self, name: &str) -> bool {
        matches!(self.get(name), Some("true" | "1" | "TRUE" | "True"))
    }

    /// Whether a variable is set to anything but nothing, `false` or `0`, in any case: what a CI
    /// system that sets a URL, a version or an id in it means.
    pub fn says_yes(&self, name: &str) -> bool {
        self.get(name)
            .map(|v| v.trim().to_ascii_lowercase())
            .is_some_and(|v| !matches!(v.as_str(), "" | "false" | "0"))
    }
}

/// \[ADR 0061\] CI systems hushrun has no provider of its own for, each by what it sets in every
/// job: Jenkins (`JENKINS_URL` and `BUILD_ID` together, since people export `JENKINS_URL` for
/// Jenkins' own CLI), Azure Pipelines, TeamCity and AWS CodeBuild, which set no `CI`, and Travis
/// CI, Bitbucket Pipelines, Drone, AppVeyor and Semaphore, which do. Each runs as the generic
/// provider, like a named CI: an agent set there doesn't make it a sandbox.
const OTHER_CI: &[&[&str]] = &[
    &["JENKINS_URL", "BUILD_ID"],
    &["TF_BUILD"],
    &["TEAMCITY_VERSION"],
    &["CODEBUILD_BUILD_ID"],
    &["TRAVIS"],
    &["BITBUCKET_BUILD_NUMBER"],
    &["DRONE"],
    &["APPVEYOR"],
    &["SEMAPHORE"],
];

/// What `name` reads as where [`Env`] differs from the process: nothing for a variable the process
/// answers, and for one a string stands in for or was dropped from, its value or that it's unset.
fn stood_in(name: &str) -> Option<Option<String>> {
    if !name.starts_with(OWN) {
        return None;
    }
    let env = Env::from_process();
    let differs = env.misplaced.iter().any(|dropped| dropped == name)
        || (env.agent().is_some() && STOOD_IN_FOR.contains(&name));
    differs.then(|| env.vars.get(name).cloned())
}

/// \[ADR 0046\] One variable as [`Env`] reads it, with [`std::env::var`]'s answers: for readers
/// that tell an empty variable, or one that isn't UTF-8, from an unset one, which [`Env::get`]
/// doesn't. What no string touches is the process's, to the letter.
pub fn var(name: &str) -> Result<String, VarError> {
    match stood_in(name) {
        Some(value) => value.ok_or(VarError::NotPresent),
        None => std::env::var(name),
    }
}

/// \[ADR 0046\] [`var`], as [`std::env::var_os`] answers: a key file's path needn't be UTF-8.
pub fn var_os(name: &str) -> Option<OsString> {
    match stood_in(name) {
        Some(value) => value.map(OsString::from),
        None => std::env::var_os(name),
    }
}

/// A variable [`context`] ignores, because it holds what hushrun can't read (see [`ignored`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CiError {
    #[error("HUSHRUN_PROVIDER is {0:?}, not github, gitlab, buildkite, circleci, generic or local")]
    Provider(String),
    #[error("{name} is {value:?}, not a positive whole number")]
    Attempt { name: &'static str, value: String },
    /// \[ADR 0046\] Fixed words: the value holds a private key.
    #[error("{0}")]
    Agent(AgentStringError),
    /// \[ADR 0046\] Only the name: the value holds a private key.
    #[error("{0} holds a HUSHRUN_AGENT string, which is never sent: set it as HUSHRUN_AGENT")]
    AgentStringIn(String),
}

/// A failed call to a provider's API. The message names the provider and the path, never a
/// token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ApiError(pub String);

/// Which provider runs this command (§11.2, detection column). `HUSHRUN_PROVIDER` overrides
/// detection; agent sandboxes that export `CI=true` set it to `local`, or \[ADR 0046\] say so with
/// `HUSHRUN_AGENT`. One hushrun doesn't know leaves the provider to detection ([`ignored`] says
/// so). \[ADR 0046\] `HUSHRUN_AGENT` overrides bare `CI=true` and nothing else: it can reach a named
/// CI's job, in a job-level secret or a project variable, and a job that took itself for a sandbox
/// would lose ADR 0044's guards and skip wrongly. Nothing else tells a sandbox from CI: real CI
/// outside GitHub sets `CI=true` with `HUSHRUN_TOKEN` and `HUSHRUN_MACHINE_KEY`, as a sandbox can.
pub fn detect(env: &Env) -> Provider {
    if let Some(provider) = env
        .get("HUSHRUN_PROVIDER")
        .and_then(|name| name.trim().parse().ok())
    {
        return provider;
    }
    if env.is_true("GITHUB_ACTIONS") {
        Provider::Github
    } else if env.is_true("GITLAB_CI") {
        Provider::Gitlab
    } else if env.is_true("BUILDKITE") {
        Provider::Buildkite
    } else if env.is_true("CIRCLECI") {
        Provider::Circleci
    } else if OTHER_CI
        .iter()
        .any(|names| names.iter().all(|name| env.says_yes(name)))
        || (env.is_true("CI") && !env.agent_set())
    {
        Provider::Generic
    } else {
        Provider::Local
    }
}

/// The variables [`detect`] and [`context`] ignore, each with what it holds: a `HUSHRUN_PROVIDER`
/// hushrun doesn't know, which leaves the provider to detection, and an attempt that isn't a
/// positive whole number, which leaves it unknown. \[ADR 0046\] And a `HUSHRUN_AGENT` hushrun can't
/// read, whose token and key it goes on without, and a string in a variable where a token goes. A
/// variable never stops a command (§10.1): hushrun says so once, and goes on without it.
pub fn ignored(env: &Env) -> Vec<CiError> {
    let mut ignored: Vec<CiError> = env.agent_error().map(CiError::Agent).into_iter().collect();
    ignored.extend(env.misplaced.iter().cloned().map(CiError::AgentStringIn));
    if let Some(name) = env
        .get("HUSHRUN_PROVIDER")
        .filter(|name| name.trim().parse::<Provider>().is_err())
    {
        ignored.push(CiError::Provider(name.to_string()));
    }
    // GitHub counts its own attempts; every other provider takes HUSHRUN_RUN_ATTEMPT.
    let name = match detect(env) {
        Provider::Github => "GITHUB_RUN_ATTEMPT",
        _ => "HUSHRUN_RUN_ATTEMPT",
    };
    if let Some(value) = env.get(name).filter(|value| attempt_of(value).is_none()) {
        ignored.push(CiError::Attempt {
            name,
            value: value.to_string(),
        });
    }
    ignored
}

/// Whether a provider is a CI system (T-4 applies) rather than a person's machine.
pub fn is_ci(provider: Provider) -> bool {
    provider != Provider::Local
}

/// \[ADR 0061\] Whether this runs in CI: [`detect`]'s answer, which every part of hushrun that
/// asks goes by, and [`context`]'s `in_ci`.
pub fn in_ci(env: &Env) -> bool {
    is_ci(detect(env))
}

/// What the provider says about this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunContext {
    /// The provider recorded in events.
    pub provider: Provider,
    /// Whether T-4's CI rules apply.
    pub in_ci: bool,
    /// The run id; `None` on generic CI without `HUSHRUN_RUN_ID` and locally (where each
    /// command gets a new one).
    pub run_id: Option<String>,
    /// The provider's own attempt number, or `HUSHRUN_RUN_ATTEMPT`.
    pub attempt: Option<u32>,
    pub url: Option<String>,
    /// What started the run, when the provider says.
    pub trigger: Option<Trigger>,
    pub branch: Option<String>,
    pub job: Option<String>,
    /// The job's own id, when the provider has one (GitLab's `CI_JOB_ID`).
    pub job_id: Option<String>,
    /// The repository as `<host>/<owner>/<name>`, when the provider says (C-2).
    pub repository: Option<String>,
    /// The revision the provider says this run is for.
    pub revision: Option<String>,
    /// The run that started this one (GitHub's `workflow_run`).
    pub triggered_by: Option<TriggeringRun>,
    /// \[M8\] A pull request's pipeline, which reads `hushrun.yaml` from its base branch (T-5),
    /// with that branch when the provider names it.
    pub pull_request: Option<PullRequest>,
    /// A run of GitHub's merge queue (`merge_group`): what it tests before it merges. Its `branch`
    /// is the queue's own, `gh-readonly-queue/<base>/pr-<n>-<sha>`, which no stage's branch rule
    /// takes for its base.
    pub merge_queue: Option<MergeQueue>,
    /// The repository as the provider names it, never the tree: its id must be the one the
    /// server names as the repository's CI (T-15).
    pub source: Option<SourceRepository>,
}

/// The repository a CI run belongs to, as its provider says (T-4, T-15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRepository {
    /// Its URL, such as
    /// `https://github.com/acme/back` (`GITHUB_SERVER_URL`/`GITHUB_REPOSITORY`, GitLab's
    /// `CI_PROJECT_URL`).
    pub url: String,
    /// Its default branch, when the provider says: the event's `repository.default_branch` on
    /// GitHub, `CI_DEFAULT_BRANCH` on GitLab.
    pub default_branch: Option<String>,
    /// Its numeric id: `GITHUB_REPOSITORY_ID`, `CI_PROJECT_ID`.
    pub id: Option<String>,
}

/// \[M8\] A pull request (GitHub) or merge request (GitLab) pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// The branch it merges into, such as `main`.
    pub base: Option<String>,
}

/// A run of GitHub's merge queue, as its ref names it: `gh-readonly-queue/<base>/pr-<n>-<sha>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeQueue {
    /// The branch it merges into, such as `main`.
    pub base: String,
    /// The pull request it tests, ahead of which others in the queue may be merged.
    pub pull_request: u64,
    /// The head commit of that pull request.
    pub head: String,
}

/// \[M8\] GitLab's merge request pipelines (`CI_PIPELINE_SOURCE=merge_request_event`), with
/// their target branch, for GitLab's provider (M9): T-5 holds for them.
fn gitlab_merge_request(env: &Env) -> Option<PullRequest> {
    (env.get("CI_PIPELINE_SOURCE") == Some("merge_request_event")).then(|| PullRequest {
        base: env
            .get("CI_MERGE_REQUEST_TARGET_BRANCH_NAME")
            .map(str::to_string),
    })
}

/// The upstream run named by a `workflow_run` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggeringRun {
    pub id: String,
    pub attempt: u32,
    pub workflow: Option<String>,
    pub conclusion: Option<String>,
    pub url: Option<String>,
    pub head_branch: Option<String>,
    pub head_sha: Option<String>,
}

/// The run context from the environment (§10.4, run identity), without the variables it
/// [`ignored`].
pub fn context(env: &Env) -> RunContext {
    let detected = detect(env);
    match detected {
        Provider::Github => return github::context(env),
        Provider::Gitlab => return gitlab::context(env),
        _ => {}
    }
    let in_ci = is_ci(detected);
    // Other native providers arrive in later milestones; until then they identify runs like
    // the generic provider.
    let provider = if in_ci {
        Provider::Generic
    } else {
        Provider::Local
    };
    RunContext {
        provider,
        in_ci,
        run_id: env.get("HUSHRUN_RUN_ID").map(str::to_string),
        attempt: attempt(env, "HUSHRUN_RUN_ATTEMPT"),
        url: env.get("HUSHRUN_RUN_URL").map(str::to_string),
        trigger: None,
        branch: None,
        job: None,
        job_id: None,
        repository: None,
        revision: None,
        triggered_by: None,
        pull_request: None,
        merge_queue: None,
        source: None,
    }
}

/// \[ADR 0044\] What a run result names as its job, which decides whose runs its pass stands in for
/// (C-10): the job and the system it ran on, `<job>@<platform>`, such as `test@linux/amd64`, or
/// `None` where no job is named. The job is `GITHUB_JOB` or `CI_JOB_NAME`, without the workflow's
/// file, so that a staging and a production workflow share a pass.
pub fn result_job(env: &Env) -> Option<String> {
    context(env).job.map(|job| format!("{job}@{}", platform()))
}

/// The system this hushrun runs on, as E-7 writes a platform: measured from the binary, so it needs
/// nothing from any CI provider.
pub fn platform() -> String {
    platform_of(std::env::consts::OS, std::env::consts::ARCH)
}

fn platform_of(os: &str, arch: &str) -> String {
    let os = match os {
        "macos" => "darwin",
        os => os,
    };
    let arch = match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        arch => arch,
    };
    format!("{os}/{arch}")
}

/// The attempt `name` holds, when it is a positive whole number ([`ignored`] says when it isn't).
pub(crate) fn attempt(env: &Env, name: &str) -> Option<u32> {
    env.get(name).and_then(attempt_of)
}

fn attempt_of(value: &str) -> Option<u32> {
    value.trim().parse::<u32>().ok().filter(|n| *n > 0)
}

/// rustls uses ring (plan §12.3); reqwest is built without a provider.
pub(crate) fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A loopback server for the API tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// Answers one request with `status` and `body`, and returns the request's head.
    pub fn serve(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                request.push_str(&line);
            }
            let mut rest = Vec::new();
            let _ = reader.get_mut().set_nonblocking(true);
            let _ = reader.read_to_end(&mut rest);
            let location = if (300..400).contains(&status) {
                "location: http://127.0.0.1:9/elsewhere\r\n"
            } else {
                ""
            };
            write!(
                stream,
                "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\n{location}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            request
        });
        (base, handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A string, and what it holds.
    const AGENT_TOKEN: &str = "hra_0123456789abcdefghijABCDEFGHIJ0123456789";
    const AGENT_KEY: &str = "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n";
    const AGENT_BACKEND: &str = "https://hushrun.example/o/acme";

    fn agent_string() -> String {
        AgentString::encode(AGENT_BACKEND, AGENT_TOKEN, AGENT_KEY)
    }

    // covers: C-17
    #[test]
    fn a_string_means_local_and_nothing_else_changes_detection() {
        let string = agent_string();
        let detect_with = |pairs: &[(&str, &str)]| detect(&Env::from_pairs(pairs.iter().copied()));
        // Real CI outside GitHub, with what a sandbox could set too: still CI.
        assert_eq!(
            detect_with(&[
                ("CI", "true"),
                ("HUSHRUN_TOKEN", AGENT_TOKEN),
                ("HUSHRUN_MACHINE_KEY", AGENT_KEY),
            ]),
            Provider::Generic
        );
        assert_eq!(detect_with(&[("CI", "true")]), Provider::Generic);
        // The string overrides bare CI=true, and that's all.
        assert_eq!(
            detect_with(&[("CI", "true"), ("HUSHRUN_AGENT", &string)]),
            Provider::Local
        );
        // A named CI beats it, readable or not: a job-level secret reaches every step, and a job
        // that took itself for a sandbox would skip what ADR 0044 keeps it from skipping.
        for value in [string.as_str(), "x", "${{ secrets.X }}"] {
            for (ci, provider) in [
                ("GITHUB_ACTIONS", Provider::Github),
                ("GITLAB_CI", Provider::Gitlab),
                ("BUILDKITE", Provider::Buildkite),
                ("CIRCLECI", Provider::Circleci),
            ] {
                assert_eq!(
                    detect_with(&[("CI", "true"), (ci, "true"), ("HUSHRUN_AGENT", value)]),
                    provider,
                    "{ci}, {value}"
                );
            }
        }
        // One hushrun can't read means the same as a good one: whoever set it meant a sandbox.
        assert_eq!(
            detect_with(&[("CI", "true"), ("HUSHRUN_AGENT", "garbage")]),
            Provider::Local
        );
        // Blank is unset.
        assert_eq!(
            detect_with(&[("CI", "true"), ("HUSHRUN_AGENT", "  ")]),
            Provider::Generic
        );
        assert_eq!(
            detect_with(&[("CI", "true"), ("HUSHRUN_AGENT", "")]),
            Provider::Generic
        );
        // An explicit provider wins, either way.
        assert_eq!(
            detect_with(&[("HUSHRUN_PROVIDER", "gitlab"), ("HUSHRUN_AGENT", &string)]),
            Provider::Gitlab
        );
        assert_eq!(
            detect_with(&[
                ("HUSHRUN_PROVIDER", "local"),
                ("CI", "true"),
                ("HUSHRUN_AGENT", &string)
            ]),
            Provider::Local
        );
    }

    // covers: C-11, C-13, C-18
    #[test]
    fn the_snapshot_reads_what_the_string_holds_under_the_old_names() {
        let string = agent_string();
        let env = Env::from_pairs([("HUSHRUN_AGENT", string.as_str())]);
        assert!(env.agent_set());
        assert_eq!(env.agent().map(AgentString::token), Some(AGENT_TOKEN));
        assert_eq!(env.get("HUSHRUN_TOKEN"), Some(AGENT_TOKEN));
        assert_eq!(env.get("HUSHRUN_MACHINE_KEY"), Some(AGENT_KEY));
        assert_eq!(env.get("HUSHRUN_BACKEND"), Some(AGENT_BACKEND));
        assert!(env.overridden().is_empty());
        assert!(ignored(&env).is_empty());
    }

    // covers: C-13, C-17
    #[test]
    fn a_string_wins_over_what_is_beside_it_whole() {
        let string = agent_string();
        let env = Env::from_pairs([
            ("HUSHRUN_AGENT", string.as_str()),
            ("HUSHRUN_TOKEN", "hra_another"),
            ("HUSHRUN_MACHINE_KEY", "another key"),
            ("HUSHRUN_MACHINE_KEY_FILE", "/run/key.pem"),
            ("HUSHRUN_BACKEND", "https://elsewhere.example/o/acme"),
        ]);
        assert_eq!(env.get("HUSHRUN_TOKEN"), Some(AGENT_TOKEN));
        assert_eq!(env.get("HUSHRUN_MACHINE_KEY"), Some(AGENT_KEY));
        assert_eq!(env.get("HUSHRUN_BACKEND"), Some(AGENT_BACKEND));
        assert_eq!(env.get("HUSHRUN_MACHINE_KEY_FILE"), None);
        assert_eq!(
            env.overridden(),
            [
                "HUSHRUN_TOKEN",
                "HUSHRUN_MACHINE_KEY",
                "HUSHRUN_MACHINE_KEY_FILE",
                "HUSHRUN_BACKEND"
            ]
        );
        // The same values override nothing, however a URL or a key is written.
        let env = Env::from_pairs([
            ("HUSHRUN_AGENT", string.as_str()),
            ("HUSHRUN_TOKEN", AGENT_TOKEN),
            ("HUSHRUN_MACHINE_KEY", AGENT_KEY.trim()),
            ("HUSHRUN_BACKEND", "https://HUSHRUN.example/o/acme/"),
        ]);
        assert!(env.overridden().is_empty(), "{:?}", env.overridden());
        // An empty HUSHRUN_BACKEND still turns results off (C-11), and overrides nothing.
        let env = Env::from_pairs([("HUSHRUN_AGENT", string.as_str()), ("HUSHRUN_BACKEND", "")]);
        assert_eq!(env.get("HUSHRUN_BACKEND"), None);
        assert!(env.overridden().is_empty());
    }

    // covers: C-14, C-17
    #[test]
    fn a_string_hushrun_cant_read_is_ignored_and_says_so_without_its_value() {
        let string = agent_string();
        let cut = &string[..string.len() - 9];
        let env = Env::from_pairs([
            ("HUSHRUN_AGENT", cut),
            ("HUSHRUN_TOKEN", "hra_two_variables"),
            ("HUSHRUN_MACHINE_KEY", "the key"),
        ]);
        assert!(env.agent().is_none());
        assert!(env.agent_set(), "it still says an agent runs here");
        assert_eq!(env.agent_error(), Some(AgentStringError::Damaged));
        // The old form beside it keeps working.
        assert_eq!(env.get("HUSHRUN_TOKEN"), Some("hra_two_variables"));
        assert_eq!(env.get("HUSHRUN_MACHINE_KEY"), Some("the key"));
        assert!(env.overridden().is_empty());
        let said: Vec<String> = ignored(&env).iter().map(ToString::to_string).collect();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].starts_with("HUSHRUN_AGENT is damaged"), "{said:?}");
        assert!(!said[0].contains(cut), "{said:?}");
    }

    // covers: C-13, C-17
    #[test]
    fn a_string_in_another_variable_is_dropped_and_still_says_an_agent_runs_here() {
        let string = agent_string();
        for name in [
            "HUSHRUN_TOKEN",
            "HUSHRUN_MACHINE_KEY",
            "HUSHRUN_REPOSITORY_TOKEN",
            "HUSHRUN_BACKEND",
            "HUSHRUN_INVOKED_BY",
            "HUSHRUN_ANYTHING_ELSE",
        ] {
            // Alone, or as a value goes astray: docker's --env-file keeps quotes.
            for value in [
                string.clone(),
                format!("\"{string}\""),
                format!("Bearer {string}"),
                format!("HUSHRUN_AGENT={string}"),
            ] {
                let env = Env::from_pairs([(name, value.as_str())]);
                assert_eq!(env.get(name), None, "{name}");
                assert!(env.agent().is_none(), "it is no HUSHRUN_AGENT");
                assert!(env.agent_set(), "{name}: it isn't a person's");
                assert!(env.agent_unreadable());
                let said: Vec<String> = ignored(&env).iter().map(ToString::to_string).collect();
                assert_eq!(
                    said,
                    [format!(
                        "{name} holds a HUSHRUN_AGENT string, which is never sent: set it as HUSHRUN_AGENT"
                    )]
                );
            }
        }
        // Others' variables aren't hushrun's to drop, and hold no value it prints.
        assert!(!Env::from_pairs([("SOME_TOOL", string.as_str())]).agent_set());
        // Beside a good one, the good one's value stands.
        let env = Env::from_pairs([
            ("HUSHRUN_AGENT", string.as_str()),
            ("HUSHRUN_TOKEN", string.as_str()),
        ]);
        assert_eq!(env.get("HUSHRUN_TOKEN"), Some(AGENT_TOKEN));
    }

    // covers: C-17
    #[test]
    fn debug_shows_names_only() {
        let string = agent_string();
        let env = Env::from_pairs([("HUSHRUN_AGENT", string.as_str())]);
        let shown = format!("{env:?}");
        assert!(shown.contains("HUSHRUN_TOKEN"), "{shown}");
        for secret in [AGENT_TOKEN, "abc", string.as_str()] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }

    // covers: C-10
    #[test]
    fn a_platform_is_written_as_e7_writes_it() {
        assert_eq!(platform_of("macos", "aarch64"), "darwin/arm64");
        assert_eq!(platform_of("linux", "x86_64"), "linux/amd64");
        assert_eq!(platform_of("linux", "aarch64"), "linux/arm64");
        assert_eq!(platform_of("windows", "x86_64"), "windows/amd64");
        assert_eq!(platform_of("freebsd", "riscv64"), "freebsd/riscv64");
    }

    // covers: C-10
    #[test]
    fn a_result_names_its_job_and_its_system() {
        let job = |pairs: &[(&str, &str)]| result_job(&Env::from_pairs(pairs.iter().copied()));
        let here = platform();
        // Whichever workflow's file holds it.
        assert_eq!(
            job(&[
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_JOB", "test"),
                (
                    "GITHUB_WORKFLOW_REF",
                    "acme/shop/.github/workflows/ci.yml@refs/heads/main"
                ),
            ]),
            Some(format!("test@{here}"))
        );
        assert_eq!(
            job(&[("GITLAB_CI", "true"), ("CI_JOB_NAME", "test 2/3")]),
            Some(format!("test 2/3@{here}"))
        );
        // No job, none to name.
        assert_eq!(job(&[("GITHUB_ACTIONS", "true")]), None);
        assert_eq!(job(&[("CI", "true")]), None);
        assert_eq!(job(&[("BUILDKITE", "true")]), None);
        assert_eq!(job(&[]), None);
        // An agent's sandbox that sets CI's variables says it is none.
        assert_eq!(
            job(&[
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_JOB", "test"),
                ("HUSHRUN_PROVIDER", "local"),
            ]),
            None
        );
    }

    // covers: T-4
    #[test]
    fn detects_providers() {
        let detect_with = |pairs: &[(&str, &str)]| detect(&Env::from_pairs(pairs.iter().copied()));
        assert_eq!(detect_with(&[]), Provider::Local);
        assert_eq!(detect_with(&[("CI", "true")]), Provider::Generic);
        assert_eq!(
            detect_with(&[("CI", "true"), ("GITHUB_ACTIONS", "true")]),
            Provider::Github
        );
        assert_eq!(detect_with(&[("GITLAB_CI", "true")]), Provider::Gitlab);
        assert_eq!(detect_with(&[("BUILDKITE", "true")]), Provider::Buildkite);
        assert_eq!(detect_with(&[("CIRCLECI", "true")]), Provider::Circleci);
        // ADR 0061: CI systems without a provider of their own, CI or not.
        for vars in [
            &[("JENKINS_URL", "https://ci.acme.dev/"), ("BUILD_ID", "41")][..],
            &[("TF_BUILD", "True")],
            &[("TEAMCITY_VERSION", "2025.07")],
            &[("CODEBUILD_BUILD_ID", "shop:8a2f")],
            &[("TRAVIS", "true"), ("CI", "true")],
            &[("BITBUCKET_BUILD_NUMBER", "12")],
            &[("APPVEYOR", "True")],
        ] {
            assert_eq!(detect_with(vars), Provider::Generic, "{vars:?}");
            assert!(in_ci(&Env::from_pairs(vars.iter().copied())), "{vars:?}");
            // An agent set there doesn't make the job a sandbox, as for a named CI.
            let with_agent: Vec<_> = vars
                .iter()
                .copied()
                .chain([("HUSHRUN_AGENT", "x")])
                .collect();
            assert_eq!(detect_with(&with_agent), Provider::Generic, "{vars:?}");
        }
        // A person's shell, with Jenkins' CLI set up, isn't a job; nor is a variable turned off.
        assert_eq!(
            detect_with(&[("JENKINS_URL", "https://ci.acme.dev/")]),
            Provider::Local
        );
        assert_eq!(detect_with(&[("TF_BUILD", "False")]), Provider::Local);
        assert_eq!(detect_with(&[("DRONE", "0")]), Provider::Local);
        assert_eq!(
            detect_with(&[("TF_BUILD", "True"), ("HUSHRUN_PROVIDER", "local")]),
            Provider::Local
        );
        // Agent sandboxes that export CI=true opt out.
        assert_eq!(
            detect_with(&[("CI", "true"), ("HUSHRUN_PROVIDER", "local")]),
            Provider::Local
        );
        // A provider hushrun doesn't know leaves it to detection.
        assert_eq!(
            detect_with(&[("GITLAB_CI", "true"), ("HUSHRUN_PROVIDER", "jenkins")]),
            Provider::Gitlab
        );
        assert_eq!(
            detect_with(&[("HUSHRUN_PROVIDER", "jenkins")]),
            Provider::Local
        );
    }

    #[test]
    fn generic_run_identity() {
        let env = Env::from_pairs([
            ("CI", "true"),
            ("HUSHRUN_RUN_ID", "build-7"),
            ("HUSHRUN_RUN_ATTEMPT", "2"),
            ("HUSHRUN_RUN_URL", "https://ci.example.com/7"),
        ]);
        let run = context(&env);
        assert!(run.in_ci);
        assert_eq!(run.provider, Provider::Generic);
        assert_eq!(run.run_id.as_deref(), Some("build-7"));
        assert_eq!(run.attempt, Some(2));
        let local = context(&Env::default());
        assert!(!local.in_ci);
        assert_eq!(local.provider, Provider::Local);
        assert_eq!(
            context(&Env::from_pairs([("HUSHRUN_RUN_ATTEMPT", "0")])).attempt,
            None
        );
    }

    // covers: T-4
    #[test]
    fn variables_hushrun_cant_read_are_ignored_and_said() {
        let ignored = |pairs: &[(&str, &str)]| ignored(&Env::from_pairs(pairs.iter().copied()));
        assert_eq!(ignored(&[]), []);
        assert_eq!(
            ignored(&[
                ("HUSHRUN_PROVIDER", "local"),
                ("HUSHRUN_RUN_ATTEMPT", " 2 ")
            ]),
            []
        );
        // Empty is unset.
        assert_eq!(
            ignored(&[("HUSHRUN_PROVIDER", ""), ("HUSHRUN_RUN_ATTEMPT", "")]),
            []
        );
        let said: Vec<String> = ignored(&[
            ("CI", "true"),
            ("HUSHRUN_PROVIDER", "jenkins"),
            ("HUSHRUN_RUN_ATTEMPT", "zero"),
        ])
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            said,
            [
                "HUSHRUN_PROVIDER is \"jenkins\", not github, gitlab, buildkite, circleci, generic or local",
                "HUSHRUN_RUN_ATTEMPT is \"zero\", not a positive whole number",
            ]
        );
        // On GitHub Actions the attempt is GitHub's own.
        assert_eq!(
            ignored(&[
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_RUN_ATTEMPT", "-1"),
                ("HUSHRUN_RUN_ATTEMPT", "x"),
            ]),
            [CiError::Attempt {
                name: "GITHUB_RUN_ATTEMPT",
                value: "-1".into()
            }]
        );
    }
}
