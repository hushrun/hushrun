//! A fake GitHub REST API on the loopback, for `doctor --org`: repositories, their files by
//! branch through the contents API, and a token it requires.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::http::{Reply, Request, Response, Server};

#[derive(Default)]
struct State {
    /// `owner/name` as GitHub spells it, by its lowercase form, with its default branch.
    repositories: BTreeMap<String, (String, String)>,
    /// (`owner/name`, branch, path) to bytes.
    files: BTreeMap<(String, String, String), Vec<u8>>,
    requests: Vec<String>,
}

pub struct FakeGitHub {
    server: Server,
    state: Arc<Mutex<State>>,
}

impl FakeGitHub {
    pub const TOKEN: &'static str = "ghs_fake-token";

    pub fn start() -> FakeGitHub {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let server = Server::start(move |request| handle(&shared, &request));
        FakeGitHub { server, state }
    }

    /// The API's base URL, for `GITHUB_API_URL`.
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// A repository, `owner/name` as GitHub spells it.
    pub fn add_repository(&self, full_name: &str, default_branch: &str) {
        self.state.lock().expect("state").repositories.insert(
            full_name.to_ascii_lowercase(),
            (full_name.to_string(), default_branch.to_string()),
        );
    }

    pub fn put_file(&self, repository: &str, branch: &str, path: &str, bytes: &[u8]) {
        self.state.lock().expect("state").files.insert(
            (
                repository.to_ascii_lowercase(),
                branch.to_string(),
                path.to_string(),
            ),
            bytes.to_vec(),
        );
    }

    /// Every request line it answered, such as `GET /repos/a/b/contents/x?ref=main`.
    pub fn requests(&self) -> Vec<String> {
        self.state.lock().expect("state").requests.clone()
    }
}

fn error(status: u16, message: &str) -> Reply {
    Reply::Send(Response::json(status, &json!({"message": message})))
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
    if request.header("authorization") != Some(&format!("Bearer {}", FakeGitHub::TOKEN)) {
        return error(401, "Bad credentials");
    }
    let Some(rest) = request.path.strip_prefix("/repos/") else {
        return error(404, "Not Found");
    };
    let parts: Vec<&str> = rest.splitn(4, '/').collect();
    if parts.len() < 2 {
        return error(404, "Not Found");
    }
    let repository = format!("{}/{}", parts[0], parts[1]).to_ascii_lowercase();
    let Some((full_name, default_branch)) = state.repositories.get(&repository).cloned() else {
        return error(404, "Not Found");
    };
    match parts.get(2) {
        None => Reply::Send(Response::json(
            200,
            &json!({"full_name": full_name, "default_branch": default_branch}),
        )),
        Some(&"contents") => {
            let path = parts.get(3).copied().unwrap_or("").to_string();
            let branch = request.query("ref").unwrap_or(&default_branch).to_string();
            if let Some(bytes) =
                state
                    .files
                    .get(&(repository.clone(), branch.clone(), path.clone()))
            {
                return Reply::Send(Response::bytes(200, bytes.clone()));
            }
            // A directory: the files right under it.
            let prefix = format!("{path}/");
            let entries: Vec<_> = state
                .files
                .keys()
                .filter(|(r, b, p)| *r == repository && *b == branch && p.starts_with(&prefix))
                .filter_map(|(_, _, p)| {
                    let name = &p[prefix.len()..];
                    (!name.contains('/')).then(|| json!({"name": name, "type": "file", "path": p}))
                })
                .collect();
            if entries.is_empty() {
                error(404, "Not Found")
            } else {
                Reply::Send(Response::json(200, &json!(entries)))
            }
        }
        Some(_) => error(404, "Not Found"),
    }
}
