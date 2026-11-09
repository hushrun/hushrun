//! GitHub Actions (§11.2): run identity and trigger, the runner's outputs, step summaries and
//! annotations (§10.2), and the REST calls for liveness (P-6) and pending runs (P-7).

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use hushrun_core::run::{Provider, Trigger};
use serde::Deserialize;

pub use crate::ApiError;
use crate::{Env, MergeQueue, PullRequest, RunContext, TriggeringRun};

/// The run context on GitHub Actions.
pub(crate) fn context(env: &Env) -> RunContext {
    let attempt = crate::attempt(env, "GITHUB_RUN_ATTEMPT");
    let server = env.get("GITHUB_SERVER_URL").unwrap_or("https://github.com");
    let repository = env.get("GITHUB_REPOSITORY");
    let run_id = env.get("GITHUB_RUN_ID").map(str::to_string);
    let url = match (repository, &run_id) {
        (Some(repo), Some(id)) => Some(format!(
            "{server}/{repo}/actions/runs/{id}/attempts/{}",
            attempt.unwrap_or(1)
        )),
        _ => None,
    };
    let event = env.get("GITHUB_EVENT_NAME");
    let payload = env
        .get("GITHUB_EVENT_PATH")
        .and_then(|path| std::fs::read(path).ok());
    let triggered_by = match event {
        Some("workflow_run") => payload.as_deref().and_then(triggering_run),
        _ => None,
    };
    let source = repository.map(|repo| crate::SourceRepository {
        url: format!("{}/{repo}", server.trim_end_matches('/')),
        default_branch: payload.as_deref().and_then(default_branch_of),
        id: env
            .get("GITHUB_REPOSITORY_ID")
            .filter(|id| !id.is_empty())
            .map(str::to_string),
    });
    RunContext {
        provider: Provider::Github,
        in_ci: true,
        run_id,
        attempt,
        url,
        trigger: event.and_then(trigger),
        branch: env
            .get("GITHUB_REF")
            .and_then(|r| r.strip_prefix("refs/heads/"))
            .map(str::to_string),
        job: env.get("GITHUB_JOB").map(str::to_string),
        job_id: None,
        repository: repository.map(|repo| format!("{}/{repo}", host_of(server))),
        revision: env.get("GITHUB_SHA").map(str::to_string),
        triggered_by,
        // T-5: these events run the pull request's own workflow on its merge commit.
        pull_request: matches!(event, Some("pull_request" | "pull_request_target")).then(|| {
            PullRequest {
                base: env.get("GITHUB_BASE_REF").map(str::to_string),
            }
        }),
        merge_queue: match event {
            Some("merge_group") => env.get("GITHUB_REF").and_then(merge_queue),
            _ => None,
        },
        source,
    }
}

/// \[ADR 0061\] Whether this runs in GitHub Actions, as [`crate::detect`] says, which
/// `HUSHRUN_PROVIDER` overrides: a sandbox inside a GitHub Actions job that says `local` is no
/// job of GitHub's.
fn on_github(env: &Env) -> bool {
    crate::detect(env) == Provider::Github
}

/// Whether this run is a GitHub Actions run of a pull request from another repository, a fork,
/// which GitHub gives no ID token and no secret (results-v0 §2): the pull request's head
/// repository, as the event's payload names it, isn't the one the run is in, or is gone.
pub fn from_a_fork(env: &Env) -> bool {
    if !on_github(env) || env.get("GITHUB_EVENT_NAME") != Some("pull_request") {
        return false;
    }
    let payload = env
        .get("GITHUB_EVENT_PATH")
        .and_then(|path| std::fs::read(path).ok());
    match (payload, env.get("GITHUB_REPOSITORY")) {
        (Some(payload), Some(here)) => head_elsewhere(&payload, here),
        _ => false,
    }
}

/// \[ADR 0052\] The repository a GitHub Actions job with no credential asks as, as a pull request
/// from a fork has none: `github.com/<owner>/<name>` of `GITHUB_REPOSITORY`, which in a fork's
/// pull request is the base repository. `None` outside GitHub Actions, and on a GitHub other than
/// github.com, whose repositories the hosted backend's open-source program doesn't take.
pub fn asks_as(env: &Env) -> Option<String> {
    if !on_github(env) {
        return None;
    }
    let server = env.get("GITHUB_SERVER_URL").unwrap_or("https://github.com");
    if !server
        .trim_end_matches('/')
        .eq_ignore_ascii_case("https://github.com")
    {
        return None;
    }
    let repository = env.get("GITHUB_REPOSITORY")?;
    let (owner, name) = repository.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| format!("github.com/{repository}"))
}

/// Whether a `pull_request` event's head repository is another than `here`, `<owner>/<name>`, or
/// gone, as a deleted fork's is. A payload that doesn't read says no.
fn head_elsewhere(payload: &[u8], here: &str) -> bool {
    #[derive(Deserialize)]
    struct Payload {
        pull_request: Pull,
    }
    #[derive(Deserialize)]
    struct Pull {
        head: Head,
    }
    #[derive(Deserialize)]
    struct Head {
        repo: Option<Repo>,
    }
    #[derive(Deserialize)]
    struct Repo {
        full_name: String,
    }
    match serde_json::from_slice::<Payload>(payload) {
        Ok(payload) => payload
            .pull_request
            .head
            .repo
            .is_none_or(|repo| !repo.full_name.eq_ignore_ascii_case(here)),
        Err(_) => false,
    }
}

/// The merge queue run a ref names, `refs/heads/gh-readonly-queue/<base>/pr-<n>-<sha>`: the base
/// may hold slashes, so the pull request is read from the end.
fn merge_queue(reference: &str) -> Option<MergeQueue> {
    let queued = reference
        .strip_prefix("refs/heads/")
        .unwrap_or(reference)
        .strip_prefix("gh-readonly-queue/")?;
    let (base, entry) = queued.rsplit_once('/')?;
    let (number, head) = entry.strip_prefix("pr-")?.split_once('-')?;
    let commit = matches!(head.len(), 40 | 64) && head.bytes().all(|b| b.is_ascii_hexdigit());
    let pull_request: u64 = number.parse().ok().filter(|n| *n > 0)?;
    (!base.is_empty() && commit).then(|| MergeQueue {
        base: base.to_string(),
        pull_request,
        head: head.to_string(),
    })
}

/// The workflow this job runs in, `<owner>/<name>/<file>` from `GITHUB_WORKFLOW_REF` without its
/// `@<ref>`, and the job's id. In a reusable workflow, the workflow is the caller's and the job the
/// called file's.
pub fn workflow_job(env: &Env) -> Option<(&str, &str)> {
    if !on_github(env) {
        return None;
    }
    let (workflow, _) = env.get("GITHUB_WORKFLOW_REF")?.split_once('@')?;
    Some((workflow, env.get("GITHUB_JOB")?))
}

/// The repository's default branch, as every event's payload names it
/// (`repository.default_branch`).
fn default_branch_of(payload: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Payload {
        repository: Repository,
    }
    #[derive(Deserialize)]
    struct Repository {
        default_branch: String,
    }
    serde_json::from_slice::<Payload>(payload)
        .ok()
        .map(|p| p.repository.default_branch)
        .filter(|b| !b.is_empty())
}

/// `GITHUB_EVENT_NAME` → trigger (§11.2). Other events, such as pull requests, say nothing.
pub fn trigger(event: &str) -> Option<Trigger> {
    match event {
        "push" => Some(Trigger::Push),
        "workflow_run" => Some(Trigger::Upstream),
        "workflow_dispatch" | "repository_dispatch" => Some(Trigger::Manual),
        "schedule" => Some(Trigger::Schedule),
        _ => None,
    }
}

fn host_of(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

/// The run a `workflow_run` event names: the upstream run that just completed.
fn triggering_run(payload: &[u8]) -> Option<TriggeringRun> {
    #[derive(Deserialize)]
    struct Payload {
        workflow_run: Run,
    }
    #[derive(Deserialize)]
    struct Run {
        id: u64,
        #[serde(default)]
        run_attempt: Option<u32>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        conclusion: Option<String>,
        #[serde(default)]
        html_url: Option<String>,
        #[serde(default)]
        head_branch: Option<String>,
        #[serde(default)]
        head_sha: Option<String>,
    }
    let payload: Payload = serde_json::from_slice(payload).ok()?;
    let run = payload.workflow_run;
    Some(TriggeringRun {
        id: run.id.to_string(),
        attempt: run.run_attempt.unwrap_or(1),
        workflow: run.name,
        conclusion: run.conclusion,
        url: run.html_url,
        head_branch: run.head_branch,
        head_sha: run.head_sha,
    })
}

/// Annotation levels (`::notice`, `::warning`, `::error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Notice,
    Warning,
    Error,
}

/// A workflow command that annotates the run, escaped so a message can't inject another
/// command.
pub fn annotation(level: Level, title: &str, message: &str) -> String {
    let name = match level {
        Level::Notice => "notice",
        Level::Warning => "warning",
        Level::Error => "error",
    };
    format!(
        "::{name} title={}::{}",
        escape_property(title),
        escape_data(message)
    )
}

/// A workflow command that annotates a line of a file, such as a hushrun.yaml line that breaks a
/// constraint: GitHub shows it on that line of the commit and the pull request.
pub fn annotation_at(
    level: Level,
    title: &str,
    message: &str,
    file: &str,
    line: u64,
    column: u64,
) -> String {
    let name = match level {
        Level::Notice => "notice",
        Level::Warning => "warning",
        Level::Error => "error",
    };
    format!(
        "::{name} file={},line={},col={},title={}::{}",
        escape_property(file),
        line.max(1),
        column.max(1),
        escape_property(title),
        escape_data(message)
    )
}

fn escape_data(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn escape_property(text: &str) -> String {
    escape_data(text).replace(':', "%3A").replace(',', "%2C")
}

/// Where the runner reads outputs and the step summary.
#[derive(Debug, Clone)]
pub struct Runner {
    output: Option<PathBuf>,
    summary: Option<PathBuf>,
}

impl Runner {
    /// The runner's files, on GitHub Actions.
    pub fn from_env(env: &Env) -> Runner {
        Runner {
            output: env.get("GITHUB_OUTPUT").map(PathBuf::from),
            summary: env.get("GITHUB_STEP_SUMMARY").map(PathBuf::from),
        }
    }

    /// Appends `key=value` outputs to `$GITHUB_OUTPUT`; multi-line values use a heredoc with
    /// a random delimiter that can't occur in the value.
    pub fn write_outputs<'a>(
        &self,
        values: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> std::io::Result<()> {
        let Some(path) = &self.output else {
            return Ok(());
        };
        let mut text = String::new();
        for (key, value) in values {
            if value.contains(['\n', '\r']) {
                let delimiter = delimiter(value);
                let _ = writeln!(text, "{key}<<{delimiter}\n{value}\n{delimiter}");
            } else {
                let _ = writeln!(text, "{key}={value}");
            }
        }
        append(path, &text)
    }

    /// Appends Markdown to `$GITHUB_STEP_SUMMARY`.
    pub fn write_summary(&self, markdown: &str) -> std::io::Result<()> {
        match &self.summary {
            Some(path) => append(path, &format!("{}\n", markdown.trim_end())),
            None => Ok(()),
        }
    }
}

fn append(path: &PathBuf, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(text.as_bytes())
}

fn delimiter(value: &str) -> String {
    let mut n = 0u64;
    loop {
        let candidate = format!("hushrun_EOF_{n:x}");
        if !value.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn error(message: impl std::fmt::Display) -> ApiError {
    ApiError(format!("GitHub API: {message}"))
}

/// The GitHub REST API, with `GITHUB_TOKEN` or `GH_TOKEN` (`actions: read`).
pub struct Api {
    runtime: tokio::runtime::Runtime,
    http: reqwest::Client,
    base: String,
    repository: String,
    token: String,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api")
            .field("base", &self.base)
            .field("repository", &self.repository)
            .finish_non_exhaustive()
    }
}

impl Api {
    /// The API for `GITHUB_REPOSITORY`, when a token is set.
    pub fn from_env(env: &Env) -> Option<Api> {
        let token = env.get("GITHUB_TOKEN").or_else(|| env.get("GH_TOKEN"))?;
        let repository = env.get("GITHUB_REPOSITORY")?;
        let base = env
            .get("GITHUB_API_URL")
            .unwrap_or("https://api.github.com");
        Api::new(base, repository, token).ok()
    }

    pub fn new(base: &str, repository: &str, token: &str) -> Result<Api, ApiError> {
        crate::install_crypto_provider();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| error(format!("starting the I/O runtime: {e}")))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("hushrun/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| error(format!("building the HTTP client: {e}")))?;
        Ok(Api {
            runtime,
            http,
            base: base.trim_end_matches('/').to_string(),
            repository: repository.to_string(),
            token: token.to_string(),
        })
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let url = format!("{}{path}", self.base);
        self.runtime.block_on(async {
            let response = self
                .http
                .get(&url)
                .bearer_auth(&self.token)
                .header("accept", "application/vnd.github+json")
                .header("x-github-api-version", "2022-11-28")
                .send()
                .await
                .map_err(|e| error(format!("{path}: {}", e.without_url())))?;
            let status = response.status();
            if !status.is_success() {
                return Err(error(format!("{path}: HTTP {status}")));
            }
            response
                .json::<T>()
                .await
                .map_err(|e| error(format!("{path}: unexpected response: {}", e.without_url())))
        })
    }

    /// The repository's default branch (`doctor`).
    pub fn default_branch(&self) -> Result<String, ApiError> {
        #[derive(Deserialize)]
        struct Repository {
            default_branch: String,
        }
        let repo: Repository = self.get(&format!("/repos/{}", self.repository))?;
        Ok(repo.default_branch)
    }

    /// The repository's name as GitHub spells it, and its default branch (`doctor --org`).
    pub fn repository_info(&self) -> Result<RepositoryInfo, ApiError> {
        self.get(&format!("/repos/{}", self.repository))
    }

    /// The bytes of a file at `reference`, or `None` when it doesn't exist there (`doctor
    /// --org`, with `contents: read`).
    pub fn file(&self, path: &str, reference: &str) -> Result<Option<Vec<u8>>, ApiError> {
        self.contents(path, reference, "application/vnd.github.raw+json")
    }

    /// The names of the files in a directory at `reference`, or `None` when it doesn't exist.
    pub fn directory(&self, path: &str, reference: &str) -> Result<Option<Vec<String>>, ApiError> {
        #[derive(Deserialize)]
        struct Entry {
            name: String,
            #[serde(rename = "type")]
            kind: String,
        }
        let Some(bytes) = self.contents(path, reference, "application/vnd.github+json")? else {
            return Ok(None);
        };
        let entries: Vec<Entry> = serde_json::from_slice(&bytes)
            .map_err(|_| error(format!("{path} is not a directory")))?;
        Ok(Some(
            entries
                .into_iter()
                .filter(|e| e.kind == "file")
                .map(|e| e.name)
                .collect(),
        ))
    }

    /// `GET /repos/{repository}/contents/{path}?ref=` as `accept`; `None` on 404.
    fn contents(
        &self,
        path: &str,
        reference: &str,
        accept: &str,
    ) -> Result<Option<Vec<u8>>, ApiError> {
        let path = format!(
            "/repos/{}/contents/{}?ref={}",
            self.repository,
            encode_path(path),
            encode(reference)
        );
        let url = format!("{}{path}", self.base);
        self.runtime.block_on(async {
            let response = self
                .http
                .get(&url)
                .bearer_auth(&self.token)
                .header("accept", accept)
                .header("x-github-api-version", "2022-11-28")
                .send()
                .await
                .map_err(|e| error(format!("{path}: {}", e.without_url())))?;
            let status = response.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !status.is_success() {
                return Err(error(format!("{path}: HTTP {status}")));
            }
            response
                .bytes()
                .await
                .map(|b| Some(b.to_vec()))
                .map_err(|e| error(format!("{path}: {}", e.without_url())))
        })
    }
}

/// A repository as the REST API describes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RepositoryInfo {
    /// `owner/name`, as GitHub spells it.
    pub full_name: String,
    pub default_branch: String,
}

/// Percent-encodes each segment of a path, keeping its `/`.
fn encode_path(path: &str) -> String {
    path.split('/').map(encode).collect::<Vec<_>>().join("/")
}

/// Percent-encodes a path or query component.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(extra: &[(&str, &str)]) -> Env {
        let mut pairs = vec![
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_RUN_ID", "1234"),
            ("GITHUB_RUN_ATTEMPT", "2"),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "Acme/api"),
            ("GITHUB_REF", "refs/heads/main"),
            ("GITHUB_SHA", "abc123"),
            ("GITHUB_JOB", "plan"),
        ];
        pairs.extend_from_slice(extra);
        Env::from_pairs(pairs)
    }

    #[test]
    fn a_pull_request_from_another_repository_is_a_forks() {
        let of = |head: &str| format!(r#"{{"pull_request": {{"head": {{"repo": {head}}}}}}}"#);
        assert!(head_elsewhere(
            of(r#"{"full_name": "someone/back"}"#).as_bytes(),
            "Acme/api"
        ));
        assert!(!head_elsewhere(
            of(r#"{"full_name": "acme/api"}"#).as_bytes(),
            "Acme/api"
        ));
        // A deleted fork's head has no repository.
        assert!(head_elsewhere(of("null").as_bytes(), "Acme/api"));
        assert!(!head_elsewhere(b"{}", "Acme/api"));
        // Only a GitHub Actions run of a pull request's event reads it.
        let dir = std::env::temp_dir().join(format!("hushrun-ci-fork-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("event.json");
        std::fs::write(&path, of(r#"{"full_name": "someone/back"}"#)).unwrap();
        let path = path.to_str().unwrap();
        let fork = github(&[
            ("GITHUB_EVENT_NAME", "pull_request"),
            ("GITHUB_EVENT_PATH", path),
        ]);
        assert!(from_a_fork(&fork));
        let pushed = github(&[("GITHUB_EVENT_NAME", "push"), ("GITHUB_EVENT_PATH", path)]);
        assert!(!from_a_fork(&pushed));
        let elsewhere = Env::from_pairs([
            ("GITHUB_EVENT_NAME", "pull_request"),
            ("GITHUB_EVENT_PATH", path),
            ("GITHUB_REPOSITORY", "Acme/api"),
        ]);
        assert!(!from_a_fork(&elsewhere));
        // ADR 0061: a sandbox inside the job that says local isn't GitHub's.
        let local = github(&[
            ("GITHUB_EVENT_NAME", "pull_request"),
            ("GITHUB_EVENT_PATH", path),
            ("HUSHRUN_PROVIDER", "local"),
        ]);
        assert!(!from_a_fork(&local));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// ADR 0052: a job without a credential asks as its repository, the base one in a fork's pull
    /// request, on github.com only.
    #[test]
    fn a_job_without_a_credential_asks_as_its_repository() {
        assert_eq!(
            asks_as(&github(&[])).as_deref(),
            Some("github.com/Acme/api")
        );
        let unset = Env::from_pairs([
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_REPOSITORY", "Acme/api"),
        ]);
        assert_eq!(asks_as(&unset).as_deref(), Some("github.com/Acme/api"));
        let enterprise = github(&[("GITHUB_SERVER_URL", "https://github.acme.dev")]);
        assert_eq!(asks_as(&enterprise), None);
        assert_eq!(
            asks_as(&Env::from_pairs([("GITHUB_REPOSITORY", "a/b")])),
            None
        );
        // ADR 0061: what detection says, which HUSHRUN_PROVIDER overrides.
        assert_eq!(asks_as(&github(&[("HUSHRUN_PROVIDER", "local")])), None);
        assert_eq!(
            asks_as(&Env::from_pairs([
                ("GITHUB_ACTIONS", "True"),
                ("GITHUB_REPOSITORY", "a/b")
            ]))
            .as_deref(),
            Some("github.com/a/b")
        );
        assert_eq!(
            workflow_job(&github(&[
                (
                    "GITHUB_WORKFLOW_REF",
                    "a/b/.github/workflows/ci.yml@refs/heads/main"
                ),
                ("HUSHRUN_PROVIDER", "local"),
            ])),
            None
        );
        for odd in ["", "back", "/back", "acme/", "acme/back/more"] {
            assert_eq!(
                asks_as(&github(&[("GITHUB_REPOSITORY", odd)])),
                None,
                "{odd}"
            );
        }
    }

    /// M4: a merge queue's run, by its event and its ref.
    #[test]
    fn a_merge_queue_run_names_its_base_and_its_pull_request() {
        let head = "0123456789abcdef0123456789abcdef01234567";
        let queued = |reference: &str| {
            crate::context(&github(&[
                ("GITHUB_EVENT_NAME", "merge_group"),
                ("GITHUB_REF", reference),
            ]))
        };
        let run = queued(&format!("refs/heads/gh-readonly-queue/main/pr-12-{head}"));
        assert_eq!(
            run.merge_queue,
            Some(MergeQueue {
                base: "main".to_string(),
                pull_request: 12,
                head: head.to_string(),
            })
        );
        // It isn't a pull request's pipeline, nor a push, and its branch is the queue's.
        assert_eq!(run.pull_request, None);
        assert_eq!(run.trigger, None);
        assert_eq!(
            run.branch,
            Some(format!("gh-readonly-queue/main/pr-12-{head}"))
        );
        let release = queued(&format!(
            "refs/heads/gh-readonly-queue/release/1.x/pr-7-{head}"
        ));
        assert_eq!(
            release.merge_queue.map(|q| (q.base, q.pull_request)),
            Some(("release/1.x".to_string(), 7))
        );
        // A SHA-256 repository's commit has 64 hex digits.
        let long = "ab".repeat(32);
        assert!(
            queued(&format!("refs/heads/gh-readonly-queue/main/pr-1-{long}"))
                .merge_queue
                .is_some()
        );
        for odd in [
            "refs/heads/main".to_string(),
            format!("refs/heads/gh-readonly-queue/pr-12-{head}"),
            format!("refs/heads/gh-readonly-queue/main/pr-x-{head}"),
            format!("refs/heads/gh-readonly-queue/main/pr-0-{head}"),
            "refs/heads/gh-readonly-queue/main/pr-12-abc".to_string(),
            format!("refs/heads/gh-readonly-queue/main/12-{head}"),
        ] {
            assert_eq!(queued(&odd).merge_queue, None, "{odd}");
        }
        // The ref alone doesn't make one: a push to such a branch is a push.
        let pushed = crate::context(&github(&[
            ("GITHUB_EVENT_NAME", "push"),
            (
                "GITHUB_REF",
                &format!("refs/heads/gh-readonly-queue/main/pr-12-{head}"),
            ),
        ]));
        assert_eq!(pushed.merge_queue, None);
    }

    #[test]
    fn run_identity_and_trigger() {
        let run = crate::context(&github(&[("GITHUB_EVENT_NAME", "push")]));
        assert_eq!(run.provider, Provider::Github);
        assert_eq!(run.run_id.as_deref(), Some("1234"));
        assert_eq!(run.attempt, Some(2));
        assert_eq!(
            run.url.as_deref(),
            Some("https://github.com/Acme/api/actions/runs/1234/attempts/2")
        );
        assert_eq!(run.trigger, Some(Trigger::Push));
        assert_eq!(run.branch.as_deref(), Some("main"));
        assert_eq!(run.repository.as_deref(), Some("github.com/Acme/api"));
        assert_eq!(run.revision.as_deref(), Some("abc123"));
        for (event, trigger) in [
            ("workflow_run", Some(Trigger::Upstream)),
            ("workflow_dispatch", Some(Trigger::Manual)),
            ("repository_dispatch", Some(Trigger::Manual)),
            ("schedule", Some(Trigger::Schedule)),
            ("pull_request", None),
        ] {
            assert_eq!(super::trigger(event), trigger, "{event}");
        }
    }

    // covers: T-4
    #[test]
    fn the_source_repository_comes_from_the_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("event.json");
        std::fs::write(
            &path,
            r#"{"ref":"refs/heads/develop","repository":{"id":42,"default_branch":"trunk"}}"#,
        )
        .unwrap();
        let env = github(&[
            ("GITHUB_EVENT_NAME", "push"),
            ("GITHUB_EVENT_PATH", path.to_str().unwrap()),
            ("GITHUB_REPOSITORY_ID", "42"),
        ]);
        let source = crate::context(&env).source.unwrap();
        assert_eq!(source.url, "https://github.com/Acme/api");
        assert_eq!(source.default_branch.as_deref(), Some("trunk"));
        assert_eq!(source.id.as_deref(), Some("42"));
        // Without a payload the branch is unknown, and found later from the remote.
        let source = crate::context(&github(&[("GITHUB_EVENT_NAME", "push")]))
            .source
            .unwrap();
        assert_eq!(source.default_branch, None);
        assert_eq!(source.id, None);
    }

    #[test]
    fn workflow_run_payloads_name_the_upstream_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("event.json");
        std::fs::write(
            &path,
            r#"{"action":"completed","workflow_run":{"id":99,"run_attempt":3,"name":"Deploy to Staging","conclusion":"success","html_url":"https://github.com/a/b/actions/runs/99","head_branch":"develop","head_sha":"def456"}}"#,
        )
        .unwrap();
        let env = github(&[
            ("GITHUB_EVENT_NAME", "workflow_run"),
            ("GITHUB_EVENT_PATH", path.to_str().unwrap()),
        ]);
        let run = crate::context(&env);
        let upstream = run.triggered_by.unwrap();
        assert_eq!(upstream.id, "99");
        assert_eq!(upstream.attempt, 3);
        assert_eq!(upstream.workflow.as_deref(), Some("Deploy to Staging"));
        assert_eq!(upstream.head_branch.as_deref(), Some("develop"));
    }

    #[test]
    fn annotations_can_name_a_line() {
        assert_eq!(
            annotation_at(
                Level::Error,
                "hushrun plan: production",
                "must require `sast`\n(C-8)",
                "hushrun.yaml",
                20,
                5
            ),
            "::error file=hushrun.yaml,line=20,col=5,title=hushrun plan%3A production::must require `sast`%0A(C-8)"
        );
        assert!(
            annotation_at(Level::Error, "t", "m", "a,b:c", 0, 0)
                .starts_with("::error file=a%2Cb%3Ac,line=1,col=1,")
        );
    }

    #[test]
    fn annotations_are_escaped() {
        assert_eq!(
            annotation(
                Level::Warning,
                "hushrun: plan",
                "50% done\n::error::injected"
            ),
            "::warning title=hushrun%3A plan::50%25 done%0A::error::injected"
        );
    }

    #[test]
    fn outputs_use_heredocs_for_several_lines() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");
        let summary = dir.path().join("summary");
        let runner = Runner::from_env(&Env::from_pairs([
            ("GITHUB_OUTPUT", output.to_str().unwrap()),
            ("GITHUB_STEP_SUMMARY", summary.to_str().unwrap()),
        ]));
        runner
            .write_outputs([
                ("mode", "promote"),
                ("reason", "line one\nhushrun_EOF_0\nline three"),
            ])
            .unwrap();
        runner.write_summary("## Plan").unwrap();
        let text = std::fs::read_to_string(&output).unwrap();
        assert_eq!(
            text,
            "mode=promote\nreason<<hushrun_EOF_1\nline one\nhushrun_EOF_0\nline three\nhushrun_EOF_1\n"
        );
        assert_eq!(std::fs::read_to_string(&summary).unwrap(), "## Plan\n");
    }

    fn serve(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        crate::testing::serve(200, body)
    }

    #[test]
    fn files_by_reference() {
        let (base, handle) = serve("version: 1\n");
        let api = Api::new(&base, "acme/shop", "t").unwrap();
        assert_eq!(
            api.file("hushrun.yaml", "main").unwrap().as_deref(),
            Some(&b"version: 1\n"[..])
        );
        let request = handle.join().unwrap();
        assert!(
            request.starts_with("GET /repos/acme/shop/contents/hushrun.yaml?ref=main "),
            "{request}"
        );
        assert!(
            request.contains("application/vnd.github.raw+json"),
            "{request}"
        );
        let (base, handle) =
            serve(r#"[{"name":"deploy-staging.yml","type":"file"},{"name":"sub","type":"dir"}]"#);
        let api = Api::new(&base, "acme/shop", "t").unwrap();
        assert_eq!(
            api.directory(".github/workflows", "release/1.0").unwrap(),
            Some(vec!["deploy-staging.yml".to_string()])
        );
        assert!(
            handle
                .join()
                .unwrap()
                .starts_with("GET /repos/acme/shop/contents/.github/workflows?ref=release%2F1.0 ")
        );
    }
}
