//! A fake results server: `hushrun-results/v0` (spec C-12) for one org, at
//! `http://127.0.0.1:<port>/o/acme`, as `hushrun run NAME -- COMMAND` and `hushrun repository
//! usage` talk to it. It answers lookups, records and \[ADR 0032\] the org's usage, takes one
//! token, and answers what a test injects instead, such as the 402 `quota_exceeded` of a record
//! the org's plan doesn't allow ([`quota_exceeded`]). \[ADR 0046\] Given an agent's key
//! ([`FakeResults::agent_string`]), it takes that agent's token too, and as hushrun cloud does,
//! records its results only signed by that key. It keeps each request whole, for tests that look
//! at everything that left a process ([`FakeResults::raw_requests`]). \[ADR 0052\] Told to, it
//! answers a lookup without a token as a repository's CI, as hushrun cloud does for a public
//! repository of its open-source program ([`FakeResults::answer_without_a_token`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine as _;
use hushrun_attest::SigningKey;
use hushrun_core::agent::AgentString;
use hushrun_core::digest::Digest;
use hushrun_core::signers::PublicKey;
use serde_json::{Value, json};

use crate::http::{Reply, Request, Response, Server};

/// A request the fake received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    /// The path below the org's URL, such as `v0/results/<key>`, or the whole path when it
    /// isn't below it.
    pub path: String,
    pub authorization: Option<String>,
}

struct Injected {
    method: String,
    path: String,
    status: u16,
    body: Value,
    times: usize,
}

#[derive(Default)]
struct State {
    /// By run key, oldest first.
    results: BTreeMap<String, Vec<Value>>,
    /// What `GET {base}/v0/usage` answers; `None`: 404, a server that doesn't report usage.
    usage: Option<Value>,
    /// The `free` of that answer; `None` leaves it as it is.
    free: Option<Value>,
    /// The repositories whose CI reuses only CI's results.
    ci_only: Vec<String>,
    /// Who a lookup says recorded a result, as a server does; `None` leaves it out.
    recorded_by: Option<String>,
    /// It answers a lookup as a server from before `?job=` does: the newest for the key.
    ignores_job: bool,
    injected: Vec<Injected>,
    seen: Vec<Seen>,
    /// Every request, whole.
    raw: Vec<Request>,
    /// The agent's key, whose token [`FakeResults::AGENT_TOKEN`] it takes.
    agent: Option<PublicKey>,
    /// The repository as whose CI a lookup without a token is answered.
    anonymous: Option<String>,
}

/// A running fake results server for the org [`FakeResults::ORG`].
pub struct FakeResults {
    server: Server,
    state: Arc<Mutex<State>>,
}

impl FakeResults {
    /// The org whose URL the fake answers at.
    pub const ORG: &'static str = "acme";
    /// The token it takes, as `HUSHRUN_TOKEN` with `HUSHRUN_BACKEND` sends it (C-13). It is
    /// neither a personal token nor an agent token, so hushrun registers no machine with it.
    pub const TOKEN: &'static str = "fake-results-token";
    /// The agent token it takes once it knows the agent's key.
    pub const AGENT_TOKEN: &'static str = "hra_fakeresults0123456789abcdefghijABCDEFGH";

    pub fn start() -> FakeResults {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let server = Server::start(move |request| handle(&shared, request));
        FakeResults { server, state }
    }

    /// The org's URL: `http://127.0.0.1:<port>/o/acme`.
    pub fn url(&self) -> String {
        format!("{}/o/{}", self.server.url(), Self::ORG)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake results state")
    }

    /// Answers the next `times` requests with `method` and a path below the org's URL that
    /// contains `path` with `status` and `body`, instead of what the protocol says.
    pub fn answer(&self, method: &str, path: &str, status: u16, body: Value, times: usize) {
        self.state().injected.push(Injected {
            method: method.to_string(),
            path: path.to_string(),
            status,
            body,
            times,
        });
    }

    /// What `GET {base}/v0/usage` answers from now on, with 200 (ADR 0032).
    pub fn set_usage(&self, usage: Value) {
        self.state().usage = Some(usage);
    }

    /// What open source repositories recorded for free, as `(repository, checks)`: the `free` of
    /// `GET {base}/v0/usage`'s answer from now on. A test of a server from before the field calls
    /// only [`FakeResults::set_usage`].
    pub fn set_free(&self, free: &[(&str, u64)]) {
        let free = free
            .iter()
            .map(|(repository, checks)| json!({"repository": repository, "checks": checks}))
            .collect();
        self.state().free = Some(Value::Array(free));
    }

    /// The repository's CI reuses only CI's results from now on, as a lookup `as=ci` finds out
    /// (C-12).
    pub fn set_ci_reuses_only_ci(&self, repository: &str) {
        self.state().ci_only.push(repository.to_string());
    }

    /// A lookup that finds a result says `by` recorded it from now on, as a server does.
    pub fn set_recorded_by(&self, by: &str) {
        self.state().recorded_by = Some(by.to_string());
    }

    /// From now on it answers a lookup as a server from before `?job=` does (results-v0 §3):
    /// the newest result for the key, whatever job recorded it, and no `forJob`.
    pub fn ignore_job_queries(&self) {
        self.state().ignores_job = true;
    }

    /// \[ADR 0052\] From now on a lookup without a token, as `repository`'s CI (`?as=ci&repository=`),
    /// finds what CI recorded, with `recordedBy` `ci` and no `invokedBy`, as hushrun cloud answers
    /// one for a public repository of its open-source program. Nothing else takes no token.
    pub fn answer_without_a_token(&self, repository: &str) {
        self.state().anonymous = Some(repository.to_string());
    }

    /// Every result recorded, oldest first for each key.
    pub fn recorded(&self) -> Vec<Value> {
        self.state().results.values().flatten().cloned().collect()
    }

    /// Every request so far, oldest first.
    pub fn requests(&self) -> Vec<Seen> {
        self.state().seen.clone()
    }

    /// Every request so far, whole: its query, headers and body.
    pub fn raw_requests(&self) -> Vec<Request> {
        self.state().raw.clone()
    }

    /// \[ADR 0046\] The agent `fleet-a` this fake now knows, its token and `key`, as the one string
    /// `hushrun token create` prints for this backend.
    pub fn agent_string(&self, key: &SigningKey) -> String {
        self.state().agent = Some(key.public_key());
        AgentString::encode(&self.url(), Self::AGENT_TOKEN, key.to_pem().as_str())
    }
}

/// Why a result an agent's token sends isn't recorded, as hushrun cloud says it (C-18).
fn agent_refusal(agent: &PublicKey, key: &str, request: &Request) -> Option<Reply> {
    let Some(header) = request.header("hushrun-signature") else {
        return Some(error(401, "machine_required", "no machine signed this"));
    };
    let unknown = || Some(error(401, "machine_unknown", "no machine of the agent's"));
    let Some((keyid, signature)) = header.split_once(' ') else {
        return unknown();
    };
    let Ok(signature) = base64::engine::general_purpose::STANDARD.decode(signature) else {
        return unknown();
    };
    let message = format!(
        "hushrun-result/v1\n{key}\n{}\n",
        Digest::of(&request.body).hex()
    );
    let signed = keyid == agent.keyid()
        && hushrun_attest::keys::verify_signature(agent, message.as_bytes(), &signature);
    if signed { None } else { unknown() }
}

/// The body of a server's 402 for a write the org's plan doesn't allow (ADR 0032): `message`,
/// one line a person can act on, and `upgrade_url`, where an admin upgrades, with the plan and
/// the period's numbers of an org on the Free plan that used its 2,000 checks.
pub fn quota_exceeded(message: &str, upgrade_url: &str) -> Value {
    json!({
        "error": {"code": "quota_exceeded", "message": message},
        "plan": "free",
        "used": 2000,
        "limit": 2000,
        "period_end": "2026-11-01T00:00:00Z",
        "upgrade_url": upgrade_url,
    })
}

fn error(status: u16, code: &str, message: &str) -> Reply {
    Reply::Send(Response::json(
        status,
        &json!({"error": {"code": code, "message": message}}),
    ))
}

fn handle(state: &Mutex<State>, request: Request) -> Reply {
    let base = format!("/o/{}/", FakeResults::ORG);
    let below = request.path.strip_prefix(&base).map(str::to_string);
    let bearer = request.header("authorization").map(str::to_string);
    let mut state = state.lock().expect("fake results state");
    state.seen.push(Seen {
        method: request.method.clone(),
        path: below.clone().unwrap_or_else(|| request.path.clone()),
        authorization: bearer.clone(),
    });
    state.raw.push(request.clone());
    let Some(path) = below else {
        return error(404, "not_found", "no such org");
    };
    let query = |name: &str| {
        request
            .query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    let mut agent = None;
    let mut anonymous = false;
    match bearer.as_deref().and_then(|v| v.strip_prefix("Bearer ")) {
        Some(FakeResults::TOKEN) => {}
        Some(FakeResults::AGENT_TOKEN) if state.agent.is_some() => agent = state.agent,
        Some(_) => return error(401, "unauthenticated", "this token is unknown here"),
        None if request.method == "GET"
            && path.starts_with("v0/results/")
            && query("as") == Some("ci")
            && state.anonymous.as_deref().is_some_and(|own| {
                query("repository").is_some_and(|asked| asked.eq_ignore_ascii_case(own))
            }) =>
        {
            anonymous = true;
        }
        None => {
            return error(
                401,
                "unauthenticated",
                "no token: run hushrun login, set HUSHRUN_TOKEN, or in GitHub Actions give the job `permissions: id-token: write`",
            );
        }
    }
    let injected = state
        .injected
        .iter_mut()
        .find(|i| i.times > 0 && i.method == request.method && path.contains(i.path.as_str()));
    if let Some(injected) = injected {
        injected.times -= 1;
        return Reply::Send(Response::json(injected.status, &injected.body));
    }
    let key = path.strip_prefix("v0/results/").map(str::to_string);
    match (request.method.as_str(), path.as_str(), key) {
        ("GET", "v0", _) if agent.is_some() => Reply::Send(Response::json(
            200,
            &json!({
                "protocol": "hushrun-results/v0",
                "org": FakeResults::ORG,
                "as": "agent:fleet-a",
                "token": {
                    "kind": "agent",
                    "name": "fleet-a",
                    "prefix": &FakeResults::AGENT_TOKEN[..10],
                    "repository": null,
                    "expires_at": "2026-11-19T10:00:00.000Z",
                    "results_per_minute": 300,
                },
            }),
        )),
        ("GET", "v0", _) => Reply::Send(Response::json(
            200,
            &json!({
                "protocol": "hushrun-results/v0",
                "org": FakeResults::ORG,
                "as": "user:ana@acme.dev",
            }),
        )),
        ("GET", "v0/usage", _) => match &state.usage {
            Some(usage) => {
                let mut usage = usage.clone();
                if let Some(free) = &state.free {
                    usage["free"] = free.clone();
                }
                Reply::Send(Response::json(200, &usage))
            }
            None => error(404, "not_found", "no such endpoint"),
        },
        ("GET", _, Some(key)) => {
            // What a repository's CI would find, asked from anywhere (C-12).
            let ci_only = !anonymous
                && query("as") == Some("ci")
                && query("repository")
                    .is_some_and(|repository| state.ci_only.iter().any(|r| r == repository));
            // The newest that counts in a job, and a variant of it (results-v0 §3).
            let job = query("job").filter(|_| !state.ignores_job);
            let variant = query("variant");
            let counts = |r: &Value| {
                let Some(job) = job else {
                    return true;
                };
                match (r["provider"].as_str(), r["job"].as_str()) {
                    (Some("local"), _) => variant.is_none(),
                    (_, Some(recorded)) => {
                        recorded == job && r["variant"]["key"].as_str() == variant
                    }
                    (Some("generic"), None) => variant.is_none(),
                    _ => false,
                }
            };
            // Like a server: skips never answer a lookup (C-10), and without a token, only what CI
            // recorded does.
            let ci_results = ci_only || anonymous;
            let mut newest = state
                .results
                .get(&key)
                .and_then(|results| {
                    results.iter().rev().find(|r| {
                        r["result"] != "skip"
                            && (!ci_results || r["provider"] != "local")
                            && counts(r)
                    })
                })
                .cloned();
            if anonymous && let Some(fields) = newest.as_mut().and_then(Value::as_object_mut) {
                fields.remove("invokedBy");
            }
            let found = newest.is_some();
            let mut answer = json!({"result": newest});
            let by = if anonymous {
                Some("ci")
            } else {
                state.recorded_by.as_deref()
            };
            if let (true, Some(by)) = (found, by) {
                answer["recordedBy"] = json!(by);
            }
            if ci_only {
                answer["ciOnly"] = json!(true);
            }
            if job.is_some() {
                answer["forJob"] = json!(true);
            }
            Reply::Send(Response::json(200, &answer))
        }
        ("POST", _, Some(key)) => {
            if let Some(refused) = agent.and_then(|agent| agent_refusal(&agent, &key, &request)) {
                return refused;
            }
            match serde_json::from_slice::<Value>(&request.body) {
                Ok(result) => {
                    state.results.entry(key).or_default().push(result);
                    Reply::Send(Response::json(201, &json!({})))
                }
                Err(_) => error(400, "invalid", "not a run result"),
            }
        }
        _ => error(404, "not_found", "no such endpoint"),
    }
}
