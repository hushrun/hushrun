//! A fake results server: `hushrun-results/v0` (spec C-12) for one org, at
//! `http://127.0.0.1:<port>/o/acme`, as `hushrun run NAME -- COMMAND` and `hushrun repository
//! usage` talk to it. It answers lookups, records and \[ADR 0032\] the org's usage, takes one
//! token, and answers what a test injects instead, such as the 402 `quota_exceeded` of a record
//! the org's plan doesn't allow ([`quota_exceeded`]). \[ADR 0046\] Given an agent's key
//! ([`FakeResults::agent_string`]), it takes that agent's token too, and as hushrun cloud does,
//! records its results only signed by that key. It keeps each request whole, for tests that look
//! at everything that left a process ([`FakeResults::raw_requests`]). \[ADR 0052\] Told to, it
//! answers a lookup without a token as a repository's CI, as hushrun cloud does for a public
//! repository of its open-source program ([`FakeResults::answer_without_a_token`]). \[ADR 0069\]
//! It keeps a run's test files' results with it (`POST {base}/v0/runs/{key}`), answers the batch
//! lookup and a check's reuse state as hushrun cloud does (results-v0 §3), or, told to, answers
//! them 404 as a server from before them does ([`FakeResults::forget_units`]). What it recorded
//! can be put back as it stood ([`FakeResults::recorded_so_far`], [`FakeResults::go_back_to`]),
//! for tests that run two machines against the same server. \[C-38\] It keeps hints as results-v0
//! has them (`PUT {base}/v0/units/hints/{sha256}`, `POST {base}/v0/units/hints/lookup`): opaque
//! bytes at a place, the same bytes twice kept once, the newest 3 for an address and system; or,
//! told to, answers both 404 or 405 as a server from before them does
//! ([`FakeResults::forget_hints`]).

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
    /// \[ADR 0069\] Every run that came with test files' results, oldest first.
    runs: Vec<Value>,
    /// Each test file's result: its key, `pass` or `fail`, its duration and its run's index.
    units: Vec<(String, String, u64, usize)>,
    /// Each check's reuse state, by lowercase repository and check.
    reuse: BTreeMap<(String, String), Value>,
    /// It answers what a server from before test files' results does: 404.
    no_units: bool,
    /// \[C-38\] Every hint it keeps.
    hints: Vec<Hint>,
    /// What moves each time a hint is sent again: the order in which hints were last seen.
    hint_clock: u64,
    /// It answers the hint requests with this status (404 or 405), as a server from before them.
    no_hints: Option<u16>,
    /// \[RFC 0004 §3.8\] It takes what a server from before tests took: 5,000 results a run in
    /// 1 MiB, and no `savedMs` above 0 with `units.reused` 0.
    older_limits: bool,
}

/// \[C-38\] One hint the fake keeps, where it is: what `PUT {base}/v0/units/hints/{sha256}` sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    /// Lowercase, `""` for none.
    pub repository: String,
    pub check_id: String,
    pub address: String,
    pub os: String,
    pub sha256: String,
    pub body: Vec<u8>,
    /// When it was last seen, on the fake's own clock.
    seen: u64,
}

/// What a fake results server recorded at one moment: results, the runs that came with test
/// files' results and theirs, each check's reuse state, and the hints.
#[derive(Debug, Clone)]
pub struct Recorded {
    results: BTreeMap<String, Vec<Value>>,
    runs: Vec<Value>,
    units: Vec<(String, String, u64, usize)>,
    reuse: BTreeMap<(String, String), Value>,
    hints: Vec<Hint>,
    hint_clock: u64,
}

/// \[C-38\] The most hints a server keeps for one address and system.
pub const HINTS_PER_ADDRESS: usize = 3;
/// \[C-38\] The most addresses a server keeps hints for, for one repository and check.
pub const HINT_ADDRESSES: usize = 50;
/// \[C-38\] The most bytes a hint has.
pub const HINT_BYTES: usize = 2 * 1024 * 1024;

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

    /// \[RFC 0004 §3.8\] From now on it takes what a server from before tests as the unit took:
    /// a run's body of 1 MiB at most (413 and a plain-text body past it) and 5,000 results (400
    /// `invalid` past them); and a result whose `units` has `savedMs` above 0 with `reused` 0
    /// gets 400 `invalid`, on `POST {base}/v0/runs/{key}` and `POST {base}/v0/results/{key}`.
    pub fn take_older_limits(&self) {
        self.state().older_limits = true;
    }

    /// \[ADR 0069\] From now on it answers the requests about test files 404, as a server from
    /// before them does.
    pub fn forget_units(&self) {
        self.state().no_units = true;
    }

    /// \[ADR 0069\] How many test files' results it keeps.
    pub fn unit_results(&self) -> usize {
        self.state().units.len()
    }

    /// \[ADR 0069\] Every run that came with test files' results, oldest first.
    /// How many of the test files' and tests' results it keeps are `result` (`pass` or `fail`).
    pub fn unit_results_that(&self, result: &str) -> usize {
        self.state()
            .units
            .iter()
            .filter(|(_, r, _, _)| r == result)
            .count()
    }

    pub fn runs(&self) -> Vec<Value> {
        self.state().runs.clone()
    }

    /// \[C-38\] From now on it answers both hint requests with `status`, 404 or 405, as a server
    /// from before hints does. Test files' results stay as they are.
    pub fn forget_hints(&self, status: u16) {
        self.state().no_hints = Some(status);
    }

    /// \[C-38\] Every hint it keeps, the least recently seen first.
    pub fn hints(&self) -> Vec<Hint> {
        let mut hints = self.state().hints.clone();
        hints.sort_by_key(|h| h.seen);
        hints
    }

    /// \[C-38\] Forgets every hint it keeps, as if none had been sent.
    pub fn drop_hints(&self) {
        self.state().hints.clear();
    }

    /// \[C-38\] Keeps `body` as a hint at a place, as `PUT {base}/v0/units/hints/{sha256}` would:
    /// what a test sends that no machine saw, such as a hint written to mislead.
    pub fn add_hint(&self, repository: &str, check_id: &str, address: &str, os: &str, body: &[u8]) {
        let mut state = self.state();
        put_hint(
            &mut state,
            repository.trim().to_lowercase(),
            check_id,
            address,
            os,
            &Digest::of(body).hex(),
            body,
        );
    }

    /// What it has recorded so far, to put back with [`FakeResults::go_back_to`].
    pub fn recorded_so_far(&self) -> Recorded {
        let state = self.state();
        Recorded {
            results: state.results.clone(),
            runs: state.runs.clone(),
            units: state.units.clone(),
            reuse: state.reuse.clone(),
            hints: state.hints.clone(),
            hint_clock: state.hint_clock,
        }
    }

    /// Forgets everything recorded since `recorded`: a second machine then finds the server as
    /// the first left it, whatever another machine recorded meanwhile, hints included. The
    /// requests it saw stay.
    pub fn go_back_to(&self, recorded: &Recorded) {
        let mut state = self.state();
        state.results = recorded.results.clone();
        state.runs = recorded.runs.clone();
        state.units = recorded.units.clone();
        state.reuse = recorded.reuse.clone();
        state.hints = recorded.hints.clone();
        state.hint_clock = recorded.hint_clock;
    }

    /// \[ADR 0069\] Records a test file's result under `key` (64 hex) with a run of `run`, as a
    /// run would have: what a test makes wrong for the files to see an audit catch it.
    pub fn add_unit_result(&self, key: &str, result: &str, duration_ms: u64, run: Value) {
        let mut state = self.state();
        state.runs.push(run);
        let index = state.runs.len() - 1;
        state
            .units
            .push((key.to_string(), result.to_string(), duration_ms, index));
    }

    /// \[ADR 0069\] A check's reuse state for a repository, as a lookup answers it.
    pub fn reuse_state(&self, repository: &str, check: &str) -> Value {
        self.state()
            .reuse
            .get(&(repository.to_lowercase(), check.to_string()))
            .cloned()
            .unwrap_or_else(|| json!({"state": "on", "generation": 0}))
    }

    /// Every result recorded, oldest first for each key.
    pub fn recorded(&self) -> Vec<Value> {
        self.state().results.values().flatten().cloned().collect()
    }

    /// Every result recorded, with its run key's hex, oldest first for each key.
    pub fn recorded_by_key(&self) -> Vec<(String, Value)> {
        self.state()
            .results
            .iter()
            .flat_map(|(key, results)| results.iter().map(move |r| (key.clone(), r.clone())))
            .collect()
    }

    /// Changes every result recorded with `edit`, as a test makes one look as an older client, or
    /// another system, would have recorded it.
    pub fn rewrite(&self, mut edit: impl FnMut(&mut Value)) {
        for results in self.state().results.values_mut() {
            results.iter_mut().for_each(&mut edit);
        }
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
    // \[C-38\] Hints, which a server from before test files' results doesn't know either.
    if path.starts_with("v0/units/hints/") {
        if state.no_units {
            return error(404, "not_found", "no such endpoint");
        }
        if let Some(status) = state.no_hints {
            return error(status, "not_found", "no such endpoint");
        }
        return hints(&mut state, &request, &path);
    }
    // \[ADR 0069\] A check's test files (results-v0 §3).
    if path.starts_with("v0/runs/") || path.starts_with("v0/units/") {
        if state.no_units {
            return error(404, "not_found", "no such endpoint");
        }
        return units(&mut state, &request, &path, agent.as_ref());
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
                Ok(result) if state.older_limits && saved_without_reuse(&result) => {
                    error(400, "invalid", SAVED_WITHOUT_REUSE)
                }
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

/// What a server from before tests as the unit says of `savedMs` above 0 with `units.reused` 0.
const SAVED_WITHOUT_REUSE: &str = "a result's `units` is {\"total\", \"ran\", \"reused\"}, whole numbers of its test files where ran and reused add up to at most total, with `savedMs`, the milliseconds what was reused took when it ran, 0 when none was reused";

/// Whether a result's `units` has `savedMs` above 0 with `reused` 0, which a server from before
/// tests as the unit refuses.
fn saved_without_reuse(result: &Value) -> bool {
    let units = &result["units"];
    units["savedMs"].as_u64().is_some_and(|ms| ms > 0) && units["reused"].as_u64() == Some(0)
}

/// Whether a run's result counts in the job `job`, of the variant `variant` (results-v0 §3).
fn counts_in(r: &Value, job: Option<&str>, variant: Option<&str>) -> bool {
    let Some(job) = job else {
        return true;
    };
    match (r["provider"].as_str(), r["job"].as_str()) {
        (Some("local"), _) => variant.is_none(),
        (_, Some(recorded)) => recorded == job && r["variant"]["key"].as_str() == variant,
        (Some("generic"), None) => variant.is_none(),
        _ => false,
    }
}

/// \[ADR 0069\] `POST {base}/v0/runs/{key}`, `…/units/lookup` and `…/units/reuse`, as hushrun
/// cloud answers them.
fn units(state: &mut State, request: &Request, path: &str, agent: Option<&PublicKey>) -> Reply {
    if request.method != "POST" {
        return error(405, "invalid", "another method");
    }
    let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
        return error(400, "invalid", "not JSON");
    };
    if let Some(key) = path.strip_prefix("v0/runs/") {
        if let Some(refused) = agent.and_then(|agent| agent_refusal(agent, key, request)) {
            return refused;
        }
        if body["schema"] != "hushrun.run-units/v0" {
            return error(400, "invalid", "not a run with its test files' results");
        }
        // \[RFC 0004 §3.8\] The limits: a server from before tests' results, or this one.
        let count = body["units"].as_array().map_or(0, Vec::len);
        let (most, most_bytes) = if state.older_limits {
            (5_000, 1 << 20)
        } else {
            (50_000, 8 << 20)
        };
        if request.body.len() > most_bytes {
            return Reply::Send(Response::bytes(
                413,
                "Failed to buffer the request body: length limit exceeded",
            ));
        }
        if count > most {
            let message = if state.older_limits {
                format!("a run records at most 5000 test files' results, and this one has {count}")
            } else {
                format!(
                    "a run records at most 50000 results of its test files and tests, and this one has {count}"
                )
            };
            return error(400, "invalid", &message);
        }
        if state.older_limits && saved_without_reuse(&body["result"]) {
            return error(400, "invalid", SAVED_WITHOUT_REUSE);
        }
        let result = body["result"].clone();
        state
            .results
            .entry(key.to_string())
            .or_default()
            .push(result.clone());
        state.runs.push(result);
        let index = state.runs.len() - 1;
        for unit in body["units"].as_array().into_iter().flatten() {
            state.units.push((
                unit["key"].as_str().unwrap_or_default().to_string(),
                unit["result"].as_str().unwrap_or_default().to_string(),
                unit["durationMs"].as_u64().unwrap_or_default(),
                index,
            ));
        }
        return Reply::Send(Response::json(201, &json!({})));
    }
    let check = body["check"].as_str().unwrap_or_default().to_string();
    let repository = body["repository"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    let reuse_key = (repository, check);
    let reuse = state
        .reuse
        .get(&reuse_key)
        .cloned()
        .unwrap_or_else(|| json!({"state": "on", "generation": 0}));
    match path {
        "v0/units/lookup" => {
            let job = body["job"].as_str();
            let variant = body["variant"].as_str();
            let mut units = serde_json::Map::new();
            let mut runs: Vec<Value> = Vec::new();
            let mut index: BTreeMap<usize, usize> = BTreeMap::new();
            for key in body["keys"].as_array().into_iter().flatten() {
                let key = key.as_str().unwrap_or_default();
                let newest =
                    state.units.iter().rev().find(|(k, _, _, run)| {
                        k == key && counts_in(&state.runs[*run], job, variant)
                    });
                if let Some((_, result, ms, run)) = newest {
                    let at = *index.entry(*run).or_insert_with(|| {
                        runs.push(state.runs[*run].clone());
                        runs.len() - 1
                    });
                    units.insert(
                        key.to_string(),
                        json!({"result": result, "durationMs": ms, "run": at}),
                    );
                }
            }
            let mut answer = json!({"units": units, "runs": runs, "reuse": reuse});
            if job.is_some() {
                answer["forJob"] = json!(true);
            }
            Reply::Send(Response::json(200, &answer))
        }
        "v0/units/reuse" => match body["state"].as_str() {
            Some("suspended") => {
                if reuse["state"] != "suspended" {
                    let generation = reuse["generation"].as_i64().unwrap_or(0) + 1;
                    state.reuse.insert(
                        reuse_key.clone(),
                        json!({"state": "suspended", "generation": generation,
                               "suspendedAt": "2026-11-10T12:00:00.000Z",
                               "suspendedBy": "user:ana@acme.dev"}),
                    );
                }
                let now = state.reuse[&reuse_key].clone();
                Reply::Send(Response::json(200, &json!({"reuse": now})))
            }
            Some("on") => {
                if agent.is_some() {
                    return error(
                        403,
                        "forbidden",
                        "an agent token can't turn reuse on again: a person does",
                    );
                }
                let generation = reuse["generation"].as_i64().unwrap_or(0);
                if reuse["state"] != "suspended" || body["generation"].as_i64() != Some(generation)
                {
                    return Reply::Send(Response::json(
                        409,
                        &json!({"error": {"code": "conflict", "message": "stale"}, "reuse": reuse}),
                    ));
                }
                let now = json!({"state": "on", "generation": generation + 1,
                                 "suspendedAt": reuse["suspendedAt"], "suspendedBy": reuse["suspendedBy"],
                                 "resumedAt": "2026-11-10T13:00:00.000Z", "resumedBy": "user:ana@acme.dev"});
                state.reuse.insert(reuse_key, now.clone());
                Reply::Send(Response::json(200, &json!({"reuse": now})))
            }
            _ => error(400, "invalid", "`state` is suspended or on"),
        },
        _ => error(404, "not_found", "no such endpoint"),
    }
}

/// Whether `text` is 64 lowercase hex characters.
fn hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// \[C-38\] Where a hint is, from four values as a request carries them, or the 400 that names
/// what doesn't fit (results-v0, Hints).
fn hint_place(
    repository: Option<&str>,
    check_id: Option<&str>,
    address: Option<&str>,
    os: Option<&str>,
) -> Result<(String, String, String, String), Reply> {
    let repository = repository.unwrap_or_default().trim().to_lowercase();
    if repository.len() > 200 {
        return Err(error(
            400,
            "invalid",
            "`repository` is longer than 200 bytes",
        ));
    }
    let check_id = check_id
        .filter(|c| hex64(c))
        .ok_or_else(|| error(400, "invalid", "`checkId` is 64 lowercase hex characters"))?;
    let address = address
        .filter(|a| hex64(a))
        .ok_or_else(|| error(400, "invalid", "`address` is 64 lowercase hex characters"))?;
    let os = os
        .filter(|o| ["linux", "macos", "windows"].contains(o))
        .ok_or_else(|| error(400, "invalid", "`os` is linux, macos or windows"))?;
    Ok((
        repository,
        check_id.to_string(),
        address.to_string(),
        os.to_string(),
    ))
}

/// \[C-38\] Keeps one hint, or sees it again: the same bytes at the same place are one hint,
/// whose time moves. Past the limits, what was seen least recently goes, never this one. `true`
/// when it is new.
fn put_hint(
    state: &mut State,
    repository: String,
    check_id: &str,
    address: &str,
    os: &str,
    sha256: &str,
    body: &[u8],
) -> bool {
    state.hint_clock += 1;
    let now = state.hint_clock;
    let at = |h: &Hint| {
        h.repository == repository && h.check_id == check_id && h.address == address && h.os == os
    };
    if let Some(hint) = state.hints.iter_mut().find(|h| at(h) && h.sha256 == sha256) {
        hint.seen = now;
        return false;
    }
    state.hints.push(Hint {
        repository: repository.clone(),
        check_id: check_id.to_string(),
        address: address.to_string(),
        os: os.to_string(),
        sha256: sha256.to_string(),
        body: body.to_vec(),
        seen: now,
    });
    // At most 3 for the address and system.
    let mut here: Vec<u64> = state
        .hints
        .iter()
        .filter(|h| at(h))
        .map(|h| h.seen)
        .collect();
    here.sort_unstable_by(|a, b| b.cmp(a));
    if let Some(oldest_kept) = here.get(HINTS_PER_ADDRESS - 1).copied() {
        state.hints.retain(|h| !at(h) || h.seen >= oldest_kept);
    }
    // At most 50 addresses for the repository and check, every system together.
    let mut addresses: BTreeMap<String, u64> = BTreeMap::new();
    for h in state
        .hints
        .iter()
        .filter(|h| h.repository == repository && h.check_id == check_id)
    {
        let seen = addresses.entry(h.address.clone()).or_default();
        *seen = (*seen).max(h.seen);
    }
    if addresses.len() > HINT_ADDRESSES {
        let drop = addresses.len() - HINT_ADDRESSES;
        let mut by_seen: Vec<(u64, String)> = addresses
            .into_iter()
            .filter(|(a, _)| a != address)
            .map(|(a, s)| (s, a))
            .collect();
        by_seen.sort();
        let gone: Vec<String> = by_seen.into_iter().take(drop).map(|(_, a)| a).collect();
        state.hints.retain(|h| {
            !(h.repository == repository && h.check_id == check_id && gone.contains(&h.address))
        });
    }
    true
}

/// \[C-38\] `PUT {base}/v0/units/hints/{sha256}` and `POST {base}/v0/units/hints/lookup`, as
/// hushrun cloud answers them (results-v0, Hints).
fn hints(state: &mut State, request: &Request, path: &str) -> Reply {
    let query = |name: &str| {
        request
            .query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    if path == "v0/units/hints/lookup" {
        if request.method != "POST" {
            return error(405, "invalid", "another method");
        }
        let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
            return error(400, "invalid", "not JSON");
        };
        let (repository, check_id, address, os) = match hint_place(
            body["repository"].as_str(),
            body["checkId"].as_str(),
            body["address"].as_str(),
            body["os"].as_str(),
        ) {
            Ok(place) => place,
            Err(reply) => return reply,
        };
        let mut found: Vec<&Hint> = state
            .hints
            .iter()
            .filter(|h| {
                h.repository == repository
                    && h.check_id == check_id
                    && h.address == address
                    && h.os == os
            })
            .collect();
        found.sort_by(|a, b| b.seen.cmp(&a.seen));
        let answer: Vec<Value> = found
            .into_iter()
            .take(HINTS_PER_ADDRESS)
            .map(|h| {
                json!({
                    "sha256": h.sha256,
                    "body": base64::engine::general_purpose::STANDARD.encode(&h.body),
                })
            })
            .collect();
        return Reply::Send(Response::json(200, &json!({"hints": answer})));
    }
    let Some(sha256) = path.strip_prefix("v0/units/hints/") else {
        return error(404, "not_found", "no such endpoint");
    };
    if request.method != "PUT" {
        return error(405, "invalid", "another method");
    }
    if request.body.len() > HINT_BYTES {
        return error(413, "too_large", "a hint is at most 2 MiB");
    }
    if !hex64(sha256) {
        return error(
            400,
            "invalid",
            "the path's SHA-256 is 64 lowercase hex characters",
        );
    }
    let (repository, check_id, address, os) = match hint_place(
        query("repository"),
        query("checkId"),
        query("address"),
        query("os"),
    ) {
        Ok(place) => place,
        Err(reply) => return reply,
    };
    if request.body.is_empty() {
        return error(400, "invalid", "an empty hint");
    }
    if Digest::of(&request.body).hex() != sha256 {
        return error(400, "invalid", "the body's SHA-256 isn't the path's");
    }
    let new = put_hint(
        state,
        repository,
        &check_id,
        &address,
        &os,
        sha256,
        &request.body,
    );
    Reply::Send(Response::json(if new { 201 } else { 200 }, &json!({})))
}
