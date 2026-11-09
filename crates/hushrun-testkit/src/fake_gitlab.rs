//! A fake GitLab REST API (v4) on the loopback, for GitLab CI's provider (M9): projects, their
//! pipelines by id and by ref, protected branches, and the token it requires. It records every
//! request and can answer every one with a fault.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::http::{Reply, Request, Response, Server};

/// A pipeline as the fake lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub id: u64,
    pub reference: String,
    pub sha: String,
    pub status: String,
    pub source: String,
}

#[derive(Debug, Clone)]
struct Project {
    id: u64,
    path: String,
    default_branch: String,
    protected: BTreeSet<String>,
    /// What `GET /projects/:id` adds, such as `ci_pipeline_variables_minimum_override_role`.
    settings: BTreeMap<String, Value>,
}

#[derive(Default)]
struct State {
    projects: Vec<Project>,
    pipelines: BTreeMap<u64, Vec<Pipeline>>,
    /// Paths the token may not read (403), such as protected branches for a Reporter.
    forbidden: Vec<String>,
    /// Every answer, when set.
    fault: Option<u16>,
    requests: Vec<String>,
}

pub struct FakeGitLab {
    server: Server,
    state: Arc<Mutex<State>>,
}

impl FakeGitLab {
    /// The project access token it accepts, as `Authorization: Bearer` or `PRIVATE-TOKEN`.
    pub const TOKEN: &'static str = "glpat-fake-token";

    pub fn start() -> FakeGitLab {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let server = Server::start(move |request| handle(&shared, &request));
        FakeGitLab { server, state }
    }

    /// The API's base URL, for `CI_API_V4_URL`.
    pub fn url(&self) -> String {
        format!("{}/api/v4", self.server.url())
    }

    /// A project, `group/name` as GitLab spells it, with its numeric id.
    pub fn add_project(&self, id: u64, path: &str, default_branch: &str) {
        self.state.lock().expect("state").projects.push(Project {
            id,
            path: path.to_string(),
            default_branch: default_branch.to_string(),
            protected: BTreeSet::new(),
            settings: BTreeMap::new(),
        });
    }

    pub fn protect(&self, project: u64, branch: &str) {
        let mut state = self.state.lock().expect("state");
        if let Some(p) = state.projects.iter_mut().find(|p| p.id == project) {
            p.protected.insert(branch.to_string());
        }
    }

    /// A project setting `GET /projects/:id` shows.
    pub fn set(&self, project: u64, name: &str, value: Value) {
        let mut state = self.state.lock().expect("state");
        if let Some(p) = state.projects.iter_mut().find(|p| p.id == project) {
            p.settings.insert(name.to_string(), value);
        }
    }

    /// A pipeline of `project`; the newest has the highest id.
    pub fn add_pipeline(&self, project: u64, pipeline: Pipeline) {
        self.state
            .lock()
            .expect("state")
            .pipelines
            .entry(project)
            .or_default()
            .push(pipeline);
    }

    /// Sets the status of a pipeline.
    pub fn set_status(&self, project: u64, id: u64, status: &str) {
        let mut state = self.state.lock().expect("state");
        for pipeline in state.pipelines.entry(project).or_default() {
            if pipeline.id == id {
                pipeline.status = status.to_string();
            }
        }
    }

    /// Answers requests whose path contains `fragment` with 403.
    pub fn forbid(&self, fragment: &str) {
        self.state
            .lock()
            .expect("state")
            .forbidden
            .push(fragment.to_string());
    }

    /// Answers every request with `status`, or normally again with `None`.
    pub fn fail_with(&self, status: Option<u16>) {
        self.state.lock().expect("state").fault = status;
    }

    /// Every request line it answered, such as `GET /api/v4/projects/7/pipelines?ref=develop`.
    pub fn requests(&self) -> Vec<String> {
        self.state.lock().expect("state").requests.clone()
    }
}

fn error(status: u16, message: &str) -> Reply {
    Reply::Send(Response::json(status, &json!({"message": message})))
}

/// Decodes the `%XX` escapes of a path segment.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn pipeline_json(project: &Project, p: &Pipeline) -> Value {
    json!({
        "id": p.id,
        "iid": p.id,
        "project_id": project.id,
        "sha": p.sha,
        "ref": p.reference,
        "status": p.status,
        "source": p.source,
        "web_url": format!("https://gitlab.com/{}/-/pipelines/{}", project.path, p.id),
        "created_at": "2026-10-16T12:00:00.000Z",
        "updated_at": "2026-10-16T12:00:00.000Z",
    })
}

fn handle(state: &Mutex<State>, request: &Request) -> Reply {
    let mut state = state.lock().expect("state");
    let query: Vec<String> = request
        .query
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    state.requests.push(format!(
        "{} {}{}",
        request.method,
        request.path,
        if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        }
    ));
    if let Some(status) = state.fault {
        return error(status, "injected fault");
    }
    let bearer = format!("Bearer {}", FakeGitLab::TOKEN);
    if request.header("authorization") != Some(bearer.as_str())
        && request.header("private-token") != Some(FakeGitLab::TOKEN)
    {
        return error(401, "401 Unauthorized");
    }
    if state
        .forbidden
        .iter()
        .any(|f| request.path.contains(f.as_str()))
    {
        return error(403, "403 Forbidden");
    }
    let Some(rest) = request.path.strip_prefix("/api/v4/projects/") else {
        return error(404, "404 Not Found");
    };
    let parts: Vec<&str> = rest.split('/').collect();
    let wanted = decode(parts[0]);
    let Some(project) = state
        .projects
        .iter()
        .find(|p| p.id.to_string() == wanted || p.path.eq_ignore_ascii_case(&wanted))
        .cloned()
    else {
        return error(404, "404 Project Not Found");
    };
    let pipelines = state
        .pipelines
        .get(&project.id)
        .cloned()
        .unwrap_or_default();
    match parts.as_slice() {
        [_] => {
            let mut body = json!({
                "id": project.id,
                "path_with_namespace": project.path,
                "default_branch": project.default_branch,
                "web_url": format!("https://gitlab.com/{}", project.path),
            });
            for (name, value) in &project.settings {
                body[name] = value.clone();
            }
            Reply::Send(Response::json(200, &body))
        }
        [_, "pipelines"] => {
            let mut listed: Vec<&Pipeline> = pipelines
                .iter()
                .filter(|p| request.query("ref").is_none_or(|r| p.reference == r))
                .filter(|p| request.query("status").is_none_or(|s| p.status == s))
                .filter(|p| request.query("source").is_none_or(|s| p.source == s))
                .collect();
            listed.sort_by_key(|p| p.id);
            if request.query("sort") != Some("asc") {
                listed.reverse();
            }
            let per_page = request
                .query("per_page")
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(20)
                .clamp(1, 100);
            let page = request
                .query("page")
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(1)
                .max(1);
            let body: Vec<Value> = listed
                .into_iter()
                .skip((page - 1) * per_page)
                .take(per_page)
                .map(|p| pipeline_json(&project, p))
                .collect();
            Reply::Send(Response::json(200, &json!(body)))
        }
        [_, "pipelines", id] => match pipelines.iter().find(|p| p.id.to_string() == *id) {
            Some(p) => Reply::Send(Response::json(200, &pipeline_json(&project, p))),
            None => error(404, "404 Not found"),
        },
        [_, "protected_branches", name] => {
            let name = decode(name);
            if project.protected.contains(&name) {
                Reply::Send(Response::json(
                    200,
                    &json!({"id": 1, "name": name, "allow_force_push": false}),
                ))
            } else {
                error(404, "404 Not found")
            }
        }
        _ => error(404, "404 Not Found"),
    }
}
