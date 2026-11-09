//! GitLab CI (§11.2, ADR 0019): run identity and trigger, and the REST calls for liveness (P-6)
//! and pending pipelines (P-7), which need `HUSHRUN_GITLAB_TOKEN`.
//!
//! GitLab has no runner files: jobs pass outputs as dotenv reports (§10.2), which the binary
//! writes. It counts no attempts either: a retried job keeps its pipeline id, so the attempt
//! comes from the plan (§10.4).

use std::fmt::Write as _;
use std::time::Duration;

use hushrun_core::run::{Provider, Trigger};
use serde::Deserialize;

use crate::{ApiError, Env, RunContext};

/// The run context on GitLab CI.
pub(crate) fn context(env: &Env) -> RunContext {
    RunContext {
        provider: Provider::Gitlab,
        in_ci: true,
        run_id: env.get("CI_PIPELINE_ID").map(str::to_string),
        attempt: crate::attempt(env, "HUSHRUN_RUN_ATTEMPT"),
        url: env.get("CI_PIPELINE_URL").map(str::to_string),
        trigger: env.get("CI_PIPELINE_SOURCE").and_then(trigger),
        // Unset in tag and merge request pipelines: their branch is unknown (P-8).
        branch: env.get("CI_COMMIT_BRANCH").map(str::to_string),
        job: env.get("CI_JOB_NAME").map(str::to_string),
        job_id: env.get("CI_JOB_ID").map(str::to_string),
        repository: env.get("CI_PROJECT_URL").and_then(repository_of),
        revision: env.get("CI_COMMIT_SHA").map(str::to_string),
        triggered_by: None,
        // T-5: a merge request pipeline runs the merge request's own pipeline definition.
        pull_request: crate::gitlab_merge_request(env),
        // GitLab's merge trains run as merge request pipelines.
        merge_queue: None,
        source: env
            .get("CI_PROJECT_URL")
            .filter(|url| !url.is_empty())
            .map(|url| crate::SourceRepository {
                url: url.trim_end_matches('/').to_string(),
                default_branch: env
                    .get("CI_DEFAULT_BRANCH")
                    .filter(|b| !b.is_empty())
                    .map(str::to_string),
                id: env
                    .get("CI_PROJECT_ID")
                    .filter(|id| !id.is_empty())
                    .map(str::to_string),
            }),
    }
}

/// `CI_PIPELINE_SOURCE` → trigger (§11.2). Other sources, such as merge request pipelines, say
/// nothing.
pub fn trigger(source: &str) -> Option<Trigger> {
    match source {
        "push" => Some(Trigger::Push),
        "pipeline" | "parent_pipeline" | "trigger" => Some(Trigger::Upstream),
        "web" | "api" => Some(Trigger::Manual),
        "schedule" => Some(Trigger::Schedule),
        _ => None,
    }
}

/// The repository id of a project URL (C-2): no scheme, no port, a lowercase host, and the path
/// as GitLab spells it, which is what `init` derives from the `origin` remote.
fn repository_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let (host, path) = rest.split_once('/')?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    if host.is_empty() || path.is_empty() || path.split('/').any(str::is_empty) {
        return None;
    }
    Some(format!("{host}/{path}"))
}

/// A project as the REST API describes it, for `doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Project {
    pub id: u64,
    /// `group/sub/name`, as GitLab spells it.
    pub path_with_namespace: String,
    #[serde(default)]
    pub default_branch: Option<String>,
    /// Who may set pipeline variables (`no_one_allowed`, `developer`, `maintainer` or `owner`);
    /// GitLab shows it only to tokens with the Maintainer role or more.
    #[serde(default)]
    pub ci_pipeline_variables_minimum_override_role: Option<String>,
    /// The older switch: `true` means only Maintainers may set pipeline variables.
    #[serde(default)]
    pub restrict_user_defined_variables: Option<bool>,
}

fn error(message: impl std::fmt::Display) -> ApiError {
    ApiError(format!("GitLab API: {message}"))
}

/// The GitLab REST API, with `HUSHRUN_GITLAB_TOKEN`: a project access token with `read_api`.
pub struct Api {
    runtime: tokio::runtime::Runtime,
    http: reqwest::Client,
    base: String,
    project: String,
    token: String,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api")
            .field("base", &self.base)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl Api {
    /// The API for this job's project, when `HUSHRUN_GITLAB_TOKEN` is set: `CI_API_V4_URL`
    /// (by default gitlab.com's) and `CI_PROJECT_ID`, else `CI_PROJECT_PATH`.
    pub fn from_env(env: &Env) -> Option<Api> {
        let token = env.get("HUSHRUN_GITLAB_TOKEN")?;
        let project = env
            .get("CI_PROJECT_ID")
            .or_else(|| env.get("CI_PROJECT_PATH"))?;
        let base = env
            .get("CI_API_V4_URL")
            .unwrap_or("https://gitlab.com/api/v4");
        Api::new(base, project, token).ok()
    }

    /// `project` is the numeric id or the `group/name` path.
    pub fn new(base: &str, project: &str, token: &str) -> Result<Api, ApiError> {
        crate::install_crypto_provider();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| error(format!("starting the I/O runtime: {e}")))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("hushrun/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| error(format!("building the HTTP client: {e}")))?;
        Ok(Api {
            runtime,
            http,
            base: base.trim_end_matches('/').to_string(),
            project: encode(project),
            token: token.to_string(),
        })
    }

    /// `GET <base><path>`: `Ok(None)` on 404, so callers can tell a missing thing from an
    /// error.
    fn fetch<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<Option<T>, ApiError> {
        let url = format!("{}{path}", self.base);
        self.runtime.block_on(async {
            let response = self
                .http
                .get(&url)
                .bearer_auth(&self.token)
                .header("accept", "application/json")
                .send()
                .await
                .map_err(|e| error(format!("{path}: {}", e.without_url())))?;
            let status = response.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !status.is_success() {
                let hint = match status.as_u16() {
                    401 => "; is HUSHRUN_GITLAB_TOKEN a valid token?",
                    403 => "; HUSHRUN_GITLAB_TOKEN needs read_api on this project",
                    _ => "",
                };
                return Err(error(format!("{path}: HTTP {status}{hint}")));
            }
            response
                .json::<T>()
                .await
                .map(Some)
                .map_err(|e| error(format!("{path}: unexpected response: {}", e.without_url())))
        })
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        self.fetch(path)?
            .ok_or_else(|| error(format!("{path}: HTTP 404 Not Found")))
    }

    /// The project (`doctor`).
    pub fn project(&self) -> Result<Project, ApiError> {
        self.get(&format!("/projects/{}", self.project))
    }

    /// Whether `branch` is protected: `None` when the token may not read protected branches
    /// (`doctor`).
    pub fn branch_protected(&self, branch: &str) -> Result<Option<bool>, ApiError> {
        #[derive(Deserialize)]
        struct Protected {}
        match self.fetch::<Protected>(&format!(
            "/projects/{}/protected_branches/{}",
            self.project,
            encode(branch)
        )) {
            Ok(Some(_)) => Ok(Some(true)),
            Ok(None) => Ok(Some(false)),
            Err(e) if e.0.contains("HTTP 403") => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Percent-encodes a path segment or query value: a project path's `/` becomes `%2F`, as the
/// API wants.
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
    use crate::testing::serve;

    fn gitlab(extra: &[(&str, &str)]) -> Env {
        let mut pairs = vec![
            ("GITLAB_CI", "true"),
            ("CI", "true"),
            ("CI_PIPELINE_ID", "4242"),
            (
                "CI_PIPELINE_URL",
                "https://gitlab.com/acme/back/-/pipelines/4242",
            ),
            ("CI_PROJECT_URL", "https://GitLab.com/acme/back"),
            ("CI_COMMIT_BRANCH", "main"),
            ("CI_COMMIT_SHA", "abc123"),
            ("CI_JOB_NAME", "plan"),
            ("CI_JOB_ID", "777"),
        ];
        pairs.extend_from_slice(extra);
        Env::from_pairs(pairs)
    }

    #[test]
    fn run_identity_and_trigger() {
        let run = crate::context(&gitlab(&[("CI_PIPELINE_SOURCE", "push")]));
        assert_eq!(run.provider, Provider::Gitlab);
        assert!(run.in_ci);
        assert_eq!(run.run_id.as_deref(), Some("4242"));
        assert_eq!(run.attempt, None);
        assert_eq!(
            run.url.as_deref(),
            Some("https://gitlab.com/acme/back/-/pipelines/4242")
        );
        assert_eq!(run.trigger, Some(Trigger::Push));
        assert_eq!(run.branch.as_deref(), Some("main"));
        assert_eq!(run.job.as_deref(), Some("plan"));
        assert_eq!(run.job_id.as_deref(), Some("777"));
        assert_eq!(run.repository.as_deref(), Some("gitlab.com/acme/back"));
        assert_eq!(run.revision.as_deref(), Some("abc123"));
        // The repository as GitLab names it (T-15).
        let source = crate::context(&gitlab(&[
            ("CI_DEFAULT_BRANCH", "trunk"),
            ("CI_PROJECT_ID", "12"),
        ]))
        .source
        .unwrap();
        assert_eq!(source.url, "https://GitLab.com/acme/back");
        assert_eq!(source.default_branch.as_deref(), Some("trunk"));
        assert_eq!(source.id.as_deref(), Some("12"));
        // GitLab counts no attempts; HUSHRUN_RUN_ATTEMPT may give one, HUSHRUN_RUN_ID nothing.
        let run = crate::context(&gitlab(&[
            ("HUSHRUN_RUN_ATTEMPT", "3"),
            ("HUSHRUN_RUN_ID", "other"),
        ]));
        assert_eq!(run.attempt, Some(3));
        assert_eq!(run.run_id.as_deref(), Some("4242"));
        // One that isn't a positive whole number is ignored (`crate::ignored` says so).
        assert_eq!(
            crate::context(&gitlab(&[("HUSHRUN_RUN_ATTEMPT", "zero")])).attempt,
            None
        );
        for (source, trigger) in [
            ("push", Some(Trigger::Push)),
            ("pipeline", Some(Trigger::Upstream)),
            ("parent_pipeline", Some(Trigger::Upstream)),
            ("trigger", Some(Trigger::Upstream)),
            ("web", Some(Trigger::Manual)),
            ("api", Some(Trigger::Manual)),
            ("schedule", Some(Trigger::Schedule)),
            ("merge_request_event", None),
            ("chat", None),
        ] {
            assert_eq!(super::trigger(source), trigger, "{source}");
        }
    }

    #[test]
    fn repositories_from_project_urls() {
        for (url, expected) in [
            ("https://gitlab.com/acme/back", Some("gitlab.com/acme/back")),
            (
                "https://GitLab.Example.com:8443/group/sub/app/",
                Some("gitlab.example.com/group/sub/app"),
            ),
            (
                "https://example.com/gitlab/group/app",
                Some("example.com/gitlab/group/app"),
            ),
            ("https://gitlab.com/", None),
            ("https://gitlab.com", None),
        ] {
            assert_eq!(repository_of(url).as_deref(), expected, "{url}");
        }
    }

    #[test]
    fn errors_name_the_problem_and_never_the_token() {
        let (base, _handle) = serve(401, r#"{"message":"401 Unauthorized"}"#);
        let api = Api::new(&base, "12", "glpat-secret").unwrap();
        let err = api.project().unwrap_err().to_string();
        assert!(
            err.contains("401") && err.contains("HUSHRUN_GITLAB_TOKEN"),
            "{err}"
        );
        assert!(!err.contains("glpat-secret"), "{err}");
        // A redirect is an error, not a request to another host with the token.
        let (base, _handle) = serve(302, "");
        let api = Api::new(&base, "12", "glpat-secret").unwrap();
        assert!(api.project().is_err());
    }

    #[test]
    fn the_api_needs_a_token_and_a_project() {
        assert!(Api::from_env(&gitlab(&[])).is_none());
        assert!(
            Api::from_env(&gitlab(&[
                ("HUSHRUN_GITLAB_TOKEN", "t"),
                ("CI_PROJECT_ID", "12"),
                ("CI_API_V4_URL", "https://gitlab.example.com/api/v4"),
            ]))
            .is_some()
        );
        assert!(Api::from_env(&Env::from_pairs([("HUSHRUN_GITLAB_TOKEN", "t")])).is_none());
    }
}
