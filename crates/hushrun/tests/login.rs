//! `hushrun login`, `hushrun logout`, and `hushrun run` with a server backend: the results
//! protocol (`hushrun-results/v0`) against a fake server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hushrun_testkit::http::{Reply, Request, Response, Server};
use hushrun_testkit::{git, schema};
use serde_json::{Value, json};

const TOKEN: &str = "hru_0123456789abcdefghijABCDEFGHIJ0123456789";
/// An agent token (ADR 0027), which the fake knows once a test adds it.
const AGENT: &str = "hra_0123456789abcdefghijABCDEFGHIJ0123456789";

/// Environment variables a test must never inherit.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_LOG",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_RUN_ID",
    "HUSHRUN_RUN_URL",
    "HUSHRUN_PLAN_JSON",
    "HUSHRUN_MACHINE_KEY",
    "HUSHRUN_MACHINE_KEY_FILE",
    "npm_lifecycle_event",
];

/// Where the hosted backend is unless a test names its fake: nothing listens on the discard
/// port, so no test reaches the real one.
const NO_HOST: &str = "http://127.0.0.1:9";

/// A results backend, at `/o/<org>` for any org, which signs in `ana@acme.dev`.
#[derive(Default)]
struct Fake {
    deny: bool,
    /// What the token answer adds to the token and the email: the org the sign-in was
    /// approved for, and its URL.
    approved: Mutex<Value>,
    polls: AtomicUsize,
    revoked: AtomicBool,
    /// By org and key.
    results: Mutex<HashMap<(String, String), Vec<Value>>>,
    /// Every request: method, path, bearer token.
    seen: Mutex<Vec<(String, String, Option<String>)>>,
    /// The body of each sign-in started.
    started: Mutex<Vec<Value>>,
    /// Agent tokens, by token.
    agents: Mutex<HashMap<String, Agent>>,
    /// The body of each agent token created.
    created: Mutex<Vec<Value>>,
    /// \[ADR 0028\] Whether it registers machines and requires a person's and an agent's
    /// results to carry one's signature, as hushrun cloud does. Without, it ignores signatures,
    /// as a server without machines does.
    machines_on: AtomicBool,
    /// The machines registered: token, key text and key id.
    machines: Mutex<Vec<(String, String, String)>>,
    /// Each result's `Hushrun-Signature`, in order.
    signatures: Mutex<Vec<Option<String>>>,
    /// Whether lookups answer from CI's results only (`"ciOnly": true`), as a repository with
    /// "CI reuses only CI's results" answers its CI.
    ci_only: AtomicBool,
    /// What the next record is answered, a status and an error code, instead of being kept.
    next_record: Mutex<Option<(u16, String)>>,
    /// How many polls for the sign-in's token get a 503 first, as a host that is starting does.
    busy: AtomicUsize,
    /// Whether every poll is told `slow_down`.
    slow: AtomicBool,
    /// How long a sign-in's code lasts, in seconds, when not 60.
    expires_in: AtomicUsize,
    /// Orgs that answer `GET …/v0` 404, as to someone who isn't a member.
    outsider_of: Mutex<Vec<String>>,
}

/// Whether `proof` (standard base64) is `key`'s signature over `message`.
fn signed(key: &str, message: &str, proof: &str) -> bool {
    use base64::Engine as _;
    let (Ok(key), Ok(proof)) = (
        hushrun_core::signers::PublicKey::parse(key),
        base64::engine::general_purpose::STANDARD.decode(proof),
    ) else {
        return false;
    };
    hushrun_attest::keys::verify_signature(&key, message.as_bytes(), &proof)
}

/// An agent token the fake knows, as hushrun cloud keeps one (ADR 0027).
#[derive(Debug, Clone)]
struct Agent {
    id: String,
    name: String,
    org: String,
    /// `None`: the whole org.
    repository: Option<String>,
    /// `active`, `expired` or `revoked`.
    state: &'static str,
    /// How many results it may record: in a test, the minute never ends.
    limit: usize,
    recorded: usize,
    expires_at: String,
}

impl Agent {
    fn new(name: &str, org: &str, repository: Option<&str>) -> Agent {
        Agent {
            id: format!("00000000-0000-4000-8000-{:012}", name.len()),
            name: name.to_string(),
            org: org.to_string(),
            repository: repository.map(str::to_string),
            state: "active",
            limit: 300,
            recorded: 0,
            expires_at: "2026-11-19T10:00:00.000Z".to_string(),
        }
    }

    fn prefix(token: &str) -> String {
        token.chars().take(10).collect()
    }

    /// As `GET {base}/v0/agent-tokens` lists it.
    fn listed(&self, token: &str) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "repository": self.repository,
            "prefix": Agent::prefix(token),
            "results_per_minute": self.limit,
            "created_at": "2026-10-20T10:00:00.000Z",
            "created_by": "ana@acme.dev",
            "expires_at": self.expires_at,
            "last_used_at": null,
        })
    }

    /// Whether `repository` is the token's: by its path's last segments, ignoring case.
    fn covers(&self, repository: &str) -> bool {
        let Some(own) = &self.repository else {
            return true;
        };
        let tail = |r: &str| {
            let parts: Vec<String> = r.to_lowercase().split('/').map(str::to_string).collect();
            parts[parts.len().saturating_sub(2)..].join("/")
        };
        tail(own) == tail(repository)
    }
}

impl Fake {
    fn start(deny: bool) -> (Server, Arc<Fake>) {
        let fake = Arc::new(Fake {
            deny,
            ..Fake::default()
        });
        let answering = fake.clone();
        (Server::start(move |request| answering.reply(request)), fake)
    }

    /// One whose sign-ins are approved for `org`, at its URL on `server`.
    fn approving(org: &str) -> (Server, Arc<Fake>) {
        let (server, fake) = Fake::start(false);
        fake.approve(json!({"org": org, "backend": format!("{}/o/{org}", server.url())}));
        (server, fake)
    }

    fn approve(&self, answer: Value) {
        *self.approved.lock().unwrap() = answer;
    }

    /// Registers machines from now on (ADR 0028).
    fn register_machines(&self) {
        self.machines_on.store(true, Ordering::SeqCst);
    }

    /// The requests to `path`, by method.
    fn count(&self, method: &str, path: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| m == method && p == path)
            .count()
    }

    /// Knows `token` as `agent`.
    fn add_agent(&self, token: &str, agent: Agent) {
        self.agents.lock().unwrap().insert(token.to_string(), agent);
    }

    /// Answers the next record `status` with the error `code`, and keeps nothing.
    fn answer_next_record(&self, status: u16, code: &str) {
        *self.next_record.lock().unwrap() = Some((status, code.to_string()));
    }

    /// Knows `key` as the machine `token` signs with: an agent token's key, made with it.
    fn give_key(&self, token: &str, key: &hushrun_attest::SigningKey) {
        self.machines.lock().unwrap().push((
            token.to_string(),
            key.public_key().to_string(),
            key.public_key().keyid(),
        ));
    }

    /// The requests to the results protocol with `method`, from the `from`th request on.
    fn requests_since(&self, from: usize, method: &str) -> usize {
        self.seen.lock().unwrap()[from..]
            .iter()
            .filter(|(m, path, _)| m == method && path.starts_with("/o/"))
            .count()
    }

    /// What an agent token may do at `org`, as hushrun cloud answers (results-v0 §2).
    fn agent(&self, token: &str, org: &str) -> Result<Agent, Reply> {
        let send = |status: u16, code: &str, message: String| {
            Reply::Send(Response::json(
                status,
                &json!({"error": {"code": code, "message": message}}),
            ))
        };
        let prefix = Agent::prefix(token);
        let Some(agent) = self.agents.lock().unwrap().get(token).cloned() else {
            return Err(send(
                401,
                "unauthenticated",
                format!(
                    "this agent token ({prefix}) is unknown here: check HUSHRUN_TOKEN, or have an org admin create one with hushrun token create --agent <name>"
                ),
            ));
        };
        let who = format!("agent token {prefix} (agent:{})", agent.name);
        match agent.state {
            "expired" => Err(send(
                401,
                "unauthenticated",
                format!(
                    "{who} expired on 2026-10-19 10:00 UTC: an admin of {} creates another with hushrun token create --agent {}",
                    agent.org, agent.name
                ),
            )),
            "revoked" => Err(send(
                401,
                "unauthenticated",
                format!(
                    "{who} was revoked on 2026-10-19 10:00 UTC by ana@acme.dev: an admin of {} creates another with hushrun token create --agent {}",
                    agent.org, agent.name
                ),
            )),
            _ if agent.org != org => Err(send(
                403,
                "forbidden",
                format!("{who} records at {} only, not at {org}", agent.org),
            )),
            _ => Ok(agent),
        }
    }

    /// The agent-token requests an admin's personal token makes (results-v0 §3).
    fn agent_tokens(&self, method: &str, org: &str, id: Option<&str>, body: &[u8]) -> Reply {
        let send = |status: u16, value: Value| Reply::Send(Response::json(status, &value));
        let mut agents = self.agents.lock().unwrap();
        match (method, id) {
            ("GET", None) => {
                let mut items: Vec<Value> = agents
                    .iter()
                    .filter(|(_, a)| a.org == org && a.state != "revoked")
                    .map(|(token, a)| a.listed(token))
                    .collect();
                items.sort_by(|a, b| a["prefix"].as_str().cmp(&b["prefix"].as_str()));
                send(200, json!({"items": items}))
            }
            ("POST", None) => {
                let body: Value = serde_json::from_slice(body).unwrap();
                self.created.lock().unwrap().push(body.clone());
                let name = body["name"].as_str().unwrap_or_default();
                // ADR 0028, amended: the token comes with its one machine, the key its creator
                // made, with proof.
                let refused = |code: &str, message: &str| {
                    send(400, json!({"error": {"code": code, "message": message}}))
                };
                let Some(key) = body["machine"]["key"].as_str() else {
                    return refused("invalid", "an agent token comes with its machine key");
                };
                let Ok(parsed) = hushrun_core::signers::PublicKey::parse(key) else {
                    return refused("invalid", "`machine.key` isn't a key");
                };
                let keyid = parsed.keyid();
                let message = format!("hushrun-agent-machine/v1\n{org}\n{name}\n{keyid}\n");
                if !signed(
                    key,
                    &message,
                    body["machine"]["proof"].as_str().unwrap_or_default(),
                ) {
                    return refused("invalid_proof", "`machine.proof` doesn't verify");
                }
                let n = agents.len();
                let token = format!("hra_{n:0>4}{}", "x".repeat(36));
                let mut agent = Agent::new(name, org, body["repository"].as_str());
                agent.id = format!("00000000-0000-4000-8000-{n:012}");
                agent.limit = body["results_per_minute"].as_u64().unwrap_or(300) as usize;
                let mut answer = agent.listed(&token);
                answer["token"] = json!(token);
                answer["backend"] = json!(format!("/o/{org}"));
                answer["machine"] = json!({"id": format!("m-{n}"), "keyid": keyid, "name": format!("agent:{name}")});
                self.machines
                    .lock()
                    .unwrap()
                    .push((token.clone(), key.to_string(), keyid));
                agents.insert(token, agent);
                send(201, answer)
            }
            ("DELETE", Some(id)) => {
                match agents
                    .values_mut()
                    .find(|a| a.id == id && a.org == org && a.state != "revoked")
                {
                    Some(agent) => {
                        agent.state = "revoked";
                        Reply::Send(Response::new(204))
                    }
                    None => send(
                        404,
                        json!({"error": {"code": "not_found", "message": "there is no such agent token"}}),
                    ),
                }
            }
            _ => send(
                405,
                json!({"error": {"code": "invalid", "message": "no such method"}}),
            ),
        }
    }

    fn reply(&self, request: Request) -> Reply {
        let bearer = request
            .header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_string);
        self.seen.lock().unwrap().push((
            request.method.clone(),
            request.path.clone(),
            bearer.clone(),
        ));
        let send = |status: u16, value: Value| Reply::Send(Response::json(status, &value));
        let error =
            |code: &str, message: &str| json!({"error": {"code": code, "message": message}});
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/cli/login") => {
                self.started
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&request.body).unwrap());
                send(
                    200,
                    json!({
                        "device_code": "device-1",
                        "user_code": "BCDF-GHJK",
                        "verification_url": "http://dashboard.invalid/device?code=BCDF-GHJK",
                        "expires_in": match self.expires_in.load(Ordering::SeqCst) {
                            0 => 60,
                            n => n,
                        },
                        "interval": 1,
                    }),
                )
            }
            ("POST", "/cli/login/token") => {
                let busy = self
                    .busy
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok();
                if busy {
                    return Reply::Send(Response::new(503));
                }
                if self.slow.load(Ordering::SeqCst) {
                    return send(400, error("slow_down", "polling too often"));
                }
                if self.deny {
                    send(
                        400,
                        error("access_denied", "the sign-in was denied in the browser"),
                    )
                } else if self.polls.fetch_add(1, Ordering::SeqCst) == 0 {
                    send(400, error("authorization_pending", "waiting for approval"))
                } else {
                    let mut issued = json!({"token": TOKEN, "email": "ana@acme.dev"});
                    if let Some(approved) = self.approved.lock().unwrap().as_object() {
                        for (field, value) in approved {
                            issued[field] = value.clone();
                        }
                    }
                    // ADR 0028: the sign-in registers the machine its start named, with proof.
                    let started = self.started.lock().unwrap().last().cloned();
                    if let Some(key) = started
                        .as_ref()
                        .and_then(|s| s["machine"]["key"].as_str())
                        .filter(|_| self.machines_on.load(Ordering::SeqCst))
                    {
                        let poll: Value = serde_json::from_slice(&request.body).unwrap();
                        let keyid = hushrun_core::signers::PublicKey::parse(key)
                            .unwrap()
                            .keyid();
                        let message = format!("hushrun-machine/v1\ndevice-1\n{keyid}\n");
                        if !signed(key, &message, poll["proof"].as_str().unwrap_or_default()) {
                            return send(400, error("invalid_proof", "the proof doesn't verify"));
                        }
                        self.machines.lock().unwrap().push((
                            TOKEN.to_string(),
                            key.to_string(),
                            keyid.clone(),
                        ));
                        issued["machine"] = json!({"id": "m-1", "keyid": keyid});
                        // A sign-in registers a machine in the org it names, and only then.
                        if issued.get("org").is_none() {
                            issued["org"] = json!("acme");
                        }
                    }
                    send(200, issued)
                }
            }
            ("POST", "/cli/logout") => {
                if bearer.as_deref() == Some(TOKEN) {
                    self.revoked.store(true, Ordering::SeqCst);
                }
                Reply::Send(Response::new(204))
            }
            (method, path) if path.starts_with("/o/") => {
                let (org, rest) = path["/o/".len()..].split_once('/').unwrap_or(("", ""));
                if !rest.starts_with("v0") {
                    return send(404, error("not_found", "no such endpoint"));
                }
                if rest == "v0" && self.outsider_of.lock().unwrap().iter().any(|o| o == org) {
                    return send(
                        404,
                        error(
                            "not_found",
                            "there is no org here that ana@acme.dev belongs to",
                        ),
                    );
                }
                let agent = match bearer.as_deref() {
                    Some(token) if token.starts_with("hra_") => match self.agent(token, org) {
                        Ok(agent) => Some((token.to_string(), agent)),
                        Err(reply) => return reply,
                    },
                    Some(TOKEN) if !self.revoked.load(Ordering::SeqCst) => None,
                    _ => {
                        return send(
                            401,
                            error(
                                "unauthenticated",
                                "no token: run hushrun login, set HUSHRUN_TOKEN, or in GitHub Actions give the job `permissions: id-token: write`",
                            ),
                        );
                    }
                };
                if let Some(id) = rest.strip_prefix("v0/agent-tokens") {
                    if agent.is_some() {
                        return send(
                            403,
                            error(
                                "forbidden",
                                "agent tokens record and read results only: managing agent tokens takes an org admin's personal token from hushrun login",
                            ),
                        );
                    }
                    let id = id.strip_prefix('/').filter(|id| !id.is_empty());
                    return self.agent_tokens(method, org, id, &request.body);
                }
                if rest == "v0/machines" && self.machines_on.load(Ordering::SeqCst) {
                    // An agent token's machine is the key made with it: no sandbox registers.
                    if agent.is_some() {
                        return send(
                            403,
                            error(
                                "forbidden",
                                "an agent token's machine is the key registered when the token was made",
                            ),
                        );
                    }
                    let token = bearer.clone().unwrap_or_default();
                    let body: Value = serde_json::from_slice(&request.body).unwrap();
                    let key = body["key"].as_str().unwrap_or_default();
                    let Ok(parsed) = hushrun_core::signers::PublicKey::parse(key) else {
                        return send(400, error("invalid", "not a key"));
                    };
                    let keyid = parsed.keyid();
                    let hash = hushrun_core::digest::Digest::of(token.as_bytes()).hex();
                    let message = format!("hushrun-machine/v1\n{hash}\n{keyid}\n");
                    if !signed(key, &message, body["proof"].as_str().unwrap_or_default()) {
                        return send(400, error("invalid_proof", "the proof doesn't verify"));
                    }
                    let mut machines = self.machines.lock().unwrap();
                    let known = machines.iter().any(|(t, _, k)| *t == token && *k == keyid);
                    if !known {
                        machines.push((token, key.to_string(), keyid.clone()));
                    }
                    return send(
                        if known { 200 } else { 201 },
                        json!({"machine": {"id": "m-2", "keyid": keyid}, "registered": true}),
                    );
                }
                let key = rest.strip_prefix("v0/results/").map(str::to_string);
                if method == "POST"
                    && key.is_some()
                    && let Some((status, code)) = self.next_record.lock().unwrap().take()
                {
                    return send(status, error(&code, "refused as the test asked"));
                }
                if method == "POST" && key.is_some() {
                    let signature = request.header("hushrun-signature").map(str::to_string);
                    // As hushrun cloud: a person's or an agent's result must be signed.
                    let machines = self.machines_on.load(Ordering::SeqCst);
                    if machines && signature.is_none() {
                        return send(
                            401,
                            error(
                                "machine_required",
                                "results carry a registered machine's signature",
                            ),
                        );
                    }
                    if let Some(header) = signature.as_ref().filter(|_| machines) {
                        let token = bearer.clone().unwrap_or_default();
                        let (keyid, sig) = header.split_once(' ').unwrap_or_default();
                        let registered = self
                            .machines
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|(t, _, k)| *t == token && k == keyid)
                            .map(|(_, key, _)| key.clone());
                        let message = format!(
                            "hushrun-result/v1\n{}\n{}\n",
                            key.as_deref().unwrap_or_default(),
                            hushrun_core::digest::Digest::of(&request.body).hex()
                        );
                        match registered {
                            Some(public) if signed(&public, &message, sig) => {}
                            Some(_) => {
                                return send(
                                    401,
                                    error("invalid_signature", "the signature doesn't verify"),
                                );
                            }
                            None => {
                                return send(
                                    401,
                                    error("machine_unknown", "this machine isn't registered"),
                                );
                            }
                        }
                    }
                    self.signatures.lock().unwrap().push(signature);
                }
                match (method, key) {
                    ("GET", None) => match &agent {
                        Some((token, a)) => send(
                            200,
                            json!({
                                "protocol": "hushrun-results/v0", "org": org,
                                "as": format!("agent:{}", a.name),
                                "token": {
                                    "kind": "agent", "name": a.name, "prefix": Agent::prefix(token),
                                    "repository": a.repository, "expires_at": a.expires_at,
                                    "results_per_minute": a.limit,
                                },
                            }),
                        ),
                        None => send(
                            200,
                            json!({"protocol": "hushrun-results/v0", "org": org, "as": "user:ana@acme.dev"}),
                        ),
                    },
                    ("GET", Some(key)) => {
                        let results = self.results.lock().unwrap();
                        // Like the server: skips never answer a lookup, and a token scoped to a
                        // repository reads that repository's only.
                        let newest = results
                            .get(&(org.to_string(), key))
                            .and_then(|r| {
                                r.iter().rev().find(|r| {
                                    r["result"] != "skip"
                                        && agent.as_ref().is_none_or(|(_, a)| {
                                            a.repository.is_none()
                                                || r["repository"].as_str()
                                                    == a.repository.as_deref()
                                        })
                                })
                            })
                            .cloned();
                        if self.ci_only.load(Ordering::SeqCst) {
                            // Like the server: only what CI recorded answers.
                            let newest = newest.filter(|r| r["provider"] != "local");
                            return send(200, json!({"result": newest, "ciOnly": true}));
                        }
                        send(200, json!({"result": newest}))
                    }
                    ("POST", Some(key)) => {
                        let mut record: Value = serde_json::from_slice(&request.body).unwrap();
                        if let Some((token, a)) = &agent {
                            let prefix = Agent::prefix(token);
                            if let Some(named) = record["repository"].as_str()
                                && !a.covers(named)
                            {
                                return send(
                                    403,
                                    error(
                                        "forbidden",
                                        &format!(
                                            "agent token {prefix} (agent:{}) records results for {} only, and this result is for {named}: use a token for that repository, or one for the whole org",
                                            a.name,
                                            a.repository.as_deref().unwrap_or_default()
                                        ),
                                    ),
                                );
                            }
                            if a.recorded >= a.limit {
                                return Reply::Send(
                                    Response::json(
                                        429,
                                        &error(
                                            "rate_limited",
                                            &format!(
                                                "agent:{} (token {prefix}) recorded {} results in the last minute, its limit: this one isn't recorded",
                                                a.name, a.limit
                                            ),
                                        ),
                                    )
                                    .header("retry-after", "60"),
                                );
                            }
                            if let Some(own) = &a.repository {
                                record["repository"] = json!(own);
                            }
                            if let Some(known) = self.agents.lock().unwrap().get_mut(token) {
                                known.recorded += 1;
                            }
                        }
                        self.results
                            .lock()
                            .unwrap()
                            .entry((org.to_string(), key))
                            .or_default()
                            .push(record);
                        send(201, json!({}))
                    }
                    _ => send(405, error("invalid", "no such method")),
                }
            }
            _ => send(404, error("not_found", "no such endpoint")),
        }
    }

    fn recorded(&self) -> Vec<Value> {
        self.results
            .lock()
            .unwrap()
            .values()
            .flatten()
            .cloned()
            .collect()
    }

    /// What `org` holds.
    fn recorded_at(&self, org: &str) -> Vec<Value> {
        self.results
            .lock()
            .unwrap()
            .iter()
            .filter(|((at, _), _)| at == org)
            .flat_map(|(_, results)| results.clone())
            .collect()
    }

    fn requests(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// The bearer tokens of the requests to the results protocol, from the `from`th request on.
    fn tokens_since(&self, from: usize) -> Vec<Option<String>> {
        self.seen.lock().unwrap()[from..]
            .iter()
            .filter(|(_, path, _)| path.starts_with("/o/"))
            .map(|(_, _, token)| token.clone())
            .collect()
    }
}

/// A repository without hushrun.yaml, a cache and a credentials file of its own.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        std::fs::write(repo.join("lib.txt"), "one\n").expect("write");
        git::commit_all(&repo, "init");
        Repo {
            _dir: dir,
            root,
            repo,
        }
    }

    /// The `origin` remote, which names the repository results are for (C-10).
    fn remote(&self, url: &str) {
        git::run(&self.repo, &["remote", "add", "origin", url], None);
    }

    /// The repository's backend, committed with its code (C-11).
    fn name_backend(&self, url: &str) {
        let manifest = json!({"name": "shop", "hushrun": {"backend": url}});
        std::fs::write(self.repo.join("package.json"), manifest.to_string()).expect("write");
        git::commit_all(&self.repo, "hushrun");
    }

    /// \[ADR 0037\] The same, for a repository with no package.json.
    fn name_backend_in_policy(&self, url: &str) {
        let policy = format!("version: 1\nrepository: github.com/acme/shop\nbackend: {url}\n");
        std::fs::write(self.repo.join("hushrun.yaml"), policy).expect("write");
        git::commit_all(&self.repo, "hushrun");
    }

    fn credentials(&self) -> PathBuf {
        self.root.join("config").join("credentials.json")
    }

    /// `hushrun` in the repository, with its own cache and credentials, and nothing inherited.
    fn hushrun(&self) -> assert_cmd::Command {
        self.hushrun_with_cache(&self.root.join("cache"))
    }

    fn hushrun_with_cache(&self, cache: &Path) -> assert_cmd::Command {
        assert_cmd::Command::from_std(self.command(cache))
    }

    /// The same, as a `std` command, to give it a terminal.
    fn command(&self, cache: &Path) -> std::process::Command {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", cache)
            .env("HUSHRUN_CREDENTIALS", self.credentials())
            .env("HUSHRUN_HOST", NO_HOST)
            // At a terminal, hushrun would ask GitHub for its newest release (C-21).
            .env("HUSHRUN_NO_UPDATE_CHECK", "1");
        for var in CLEARED {
            cmd.env_remove(var);
        }
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        cmd
    }

    fn saved(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.credentials()).expect("credentials"))
            .expect("JSON")
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

fn ran(mut cmd: assert_cmd::Command, args: &[&str]) -> Ran {
    let output = cmd.args(args).output().expect("hushrun");
    Ran {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

const UNIT: &[&str] = &["run", "unit", "--", "git", "--version"];

fn login(repo: &Repo, server: &Server) -> Ran {
    let backend = format!("{}/o/acme", server.url());
    ran(repo.hushrun(), &["login", &backend, "--no-browser"])
}

// covers: C-11, C-12, C-13
#[test]
fn login_keeps_a_token_that_run_then_uses() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    let signed_in = login(&r, &server);
    assert_eq!(signed_in.code, 0, "{}", signed_in.stderr);
    assert!(
        signed_in.stdout.contains(
            "Open http://dashboard.invalid/device?code=BCDF-GHJK and approve the code BCDF-GHJK there."
        ),
        "{}",
        signed_in.stdout
    );
    assert!(
        signed_in
            .stdout
            .contains("Signed in to acme as ana@acme.dev: hushrun run now shares results there."),
        "{}",
        signed_in.stdout
    );
    let saved = r.saved();
    assert_eq!(saved["backend"], format!("{}/o/acme", server.url()));
    assert_eq!(saved["hosts"][server.url()]["token"], TOKEN);
    assert_eq!(saved["hosts"][server.url()]["email"], "ana@acme.dev");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(r.credentials())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }

    // hushrun run records to the server, with the token.
    let first = ran(r.hushrun(), UNIT);
    assert!(first.stdout.contains("git version"), "{}", first.stderr);
    let recorded = fake.recorded();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["result"], "pass");
    assert_eq!(recorded[0]["check"], "unit");
    assert!(
        fake.seen
            .lock()
            .unwrap()
            .iter()
            .any(|(method, path, token)| method == "POST"
                && path.starts_with("/o/acme/v0/results/")
                && token.as_deref() == Some(TOKEN))
    );

    // Another machine, here another cache, skips: the pass is on the server.
    let elsewhere = ran(r.hushrun_with_cache(&r.root.join("other-cache")), UNIT);
    assert_eq!(elsewhere.code, 0, "{}", elsewhere.stderr);
    assert!(elsewhere.stdout.is_empty(), "{}", elsewhere.stdout);
    assert!(
        elsewhere
            .stderr
            .contains("unit skipped: this exact code passed"),
        "{}",
        elsewhere.stderr
    );
    // The skip is counted on the server, with the time it saved; the next lookup still finds
    // the pass.
    let recorded = fake.recorded();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    for record in &recorded {
        schema::assert_valid(&schema::load("run-result.v0.schema.json"), record);
    }
    let skip = recorded
        .iter()
        .find(|r| r["result"] == "skip")
        .expect("a skip");
    assert_eq!(skip["savedMs"], recorded[0]["durationMs"]);
    assert_eq!(skip["check"], "unit");
    let again = ran(r.hushrun_with_cache(&r.root.join("third-cache")), UNIT);
    assert!(again.stdout.is_empty(), "{}", again.stderr);
}

// covers: C-13, C-18
#[test]
fn ci_never_sends_a_persons_saved_login() {
    // A self-hosted runner where someone once signed in: its CI jobs aren't that person.
    let (server, fake) = Fake::start(false);
    fake.register_machines();
    let r = Repo::new();
    assert_eq!(login(&r, &server).code, 0);
    let before = fake.requests();
    for ci in [
        &[("CI", "true")][..],
        &[("CI", "true"), ("GITLAB_CI", "true")],
        &[("CI", "true"), ("BUILDKITE", "true")],
    ] {
        let mut cmd = r.hushrun();
        cmd.envs(ci.iter().copied());
        let run = ran(cmd, &["run", "unit", "--force", "--", "git", "--version"]);
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(run.stdout.contains("git version"), "{}", run.stderr);
    }
    let tokens = fake.tokens_since(before);
    assert!(!tokens.is_empty(), "the backend was asked");
    assert!(
        tokens.iter().all(Option::is_none),
        "a login went with a CI job's requests: {tokens:?}"
    );
    assert!(fake.recorded().is_empty());
    assert!(fake.signatures.lock().unwrap().is_empty());
    assert_eq!(fake.count("POST", "/o/acme/v0/machines"), 0);

    // A job that names a person's token signs with the key it names too, never with the one the
    // sign-in left on the runner.
    let as_ana = |key: Option<&str>| {
        let mut cmd = r.hushrun();
        cmd.env("CI", "true")
            .env("HUSHRUN_BACKEND", format!("{}/o/acme", server.url()))
            .env("HUSHRUN_TOKEN", TOKEN);
        if let Some(pem) = key {
            cmd.env("HUSHRUN_MACHINE_KEY", pem);
        }
        ran(cmd, &["run", "unit", "--force", "--", "git", "--version"])
    };
    let unnamed = as_ana(None);
    assert_eq!(unnamed.code, 0, "{}", unnamed.stderr);
    assert!(
        unnamed
            .stderr
            .contains("not recorded: in CI, a personal token's results are signed with the key the job names: set HUSHRUN_MACHINE_KEY or HUSHRUN_MACHINE_KEY_FILE"),
        "{}",
        unnamed.stderr
    );
    assert!(fake.recorded().is_empty());
    let key = machine_key();
    let named = as_ana(Some(key.to_pem().as_str()));
    assert_eq!(named.code, 0, "{}", named.stderr);
    assert_eq!(fake.recorded().len(), 1, "{}", named.stderr);
    let signatures = fake.signatures.lock().unwrap().clone();
    let keyid = key.public_key().keyid();
    assert!(
        signatures[0]
            .as_deref()
            .is_some_and(|s| s.starts_with(&format!("{keyid} "))),
        "{signatures:?}"
    );

    // Outside CI, the same machine's runs are that person's, as before.
    let local = ran(r.hushrun(), UNIT);
    assert_eq!(local.code, 0, "{}", local.stderr);
    assert_eq!(fake.recorded().len(), 2);
}

// covers: C-12
#[test]
fn a_repository_whose_ci_reuses_only_cis_results_says_so() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    assert_eq!(login(&r, &server).code, 0);
    // A laptop's pass.
    assert_eq!(ran(r.hushrun(), UNIT).code, 0);
    fake.ci_only.store(true, Ordering::SeqCst);
    // The server answers from CI's results only: the laptop's pass doesn't skip, and hushrun
    // says why.
    let ran_again = ran(r.hushrun_with_cache(&r.root.join("other-cache")), UNIT);
    assert_eq!(ran_again.code, 0, "{}", ran_again.stderr);
    assert!(
        ran_again.stderr.contains(
            "unit running: no CI pass yet for this exact code; skipped only on CI's passes"
        ),
        "{}",
        ran_again.stderr
    );
    // A pass CI recorded skips, and says it was CI's.
    {
        let mut results = fake.results.lock().unwrap();
        for list in results.values_mut() {
            for result in list.iter_mut() {
                result["provider"] = json!("github");
            }
        }
    }
    let skipped = ran(r.hushrun_with_cache(&r.root.join("third-cache")), UNIT);
    assert!(
        skipped
            .stderr
            .contains("unit skipped: this exact code passed in CI"),
        "{}",
        skipped.stderr
    );
    assert!(
        skipped.stderr.contains("; skipped only on CI's passes"),
        "{}",
        skipped.stderr
    );
}

// covers: C-11, C-13, C-18
#[test]
fn the_environment_names_the_backend_and_the_token() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    let key = machine_key();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_BACKEND", format!("{}/o/acme", server.url()))
        .env("HUSHRUN_TOKEN", TOKEN)
        .env("HUSHRUN_MACHINE_KEY", key.to_pem().as_str());
    let first = ran(cmd, UNIT);
    assert!(first.stdout.contains("git version"), "{}", first.stderr);
    assert_eq!(fake.recorded().len(), 1);
    assert!(!r.credentials().exists());
    // A person's result is signed, by the key the environment names.
    let signature = fake.signatures.lock().unwrap()[0].clone().unwrap();
    assert!(
        signature.starts_with(&format!("{} ", key.public_key().keyid())),
        "{signature}"
    );

    // Without a machine key, the result isn't sent: the run says what to do, and exits as the
    // command did.
    let before = fake.requests();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_BACKEND", format!("{}/o/acme", server.url()))
        .env("HUSHRUN_TOKEN", TOKEN);
    let run = ran(cmd, &["run", "unit", "--force", "--", "git", "--version"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("git version"));
    let last = *hushrun_lines(&run.stderr).last().unwrap();
    assert!(
        last.starts_with("hushrun: unit passed in")
            && last.ends_with("; not recorded: this machine isn't registered: run `hushrun login`"),
        "{}",
        run.stderr
    );
    assert_eq!(fake.requests_since(before, "POST"), 0);
    assert_eq!(fake.recorded().len(), 1);
}

/// hushrun's own lines in `stderr`, but for the hook it installs in a new clone (ADR 0043).
fn hushrun_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.starts_with("hushrun: ") && !line.contains("the pre-push hook"))
        .collect()
}

/// A fork's pull request in GitHub Actions gets no ID token and can't sign in: its run says why,
/// where another would say how to get a token (results-v0 §2).
// covers: C-13, C-14
#[test]
fn a_forks_pull_request_is_told_why_it_shares_nothing() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    let event = r.root.join("event.json");
    let said = |head: &str| {
        std::fs::write(
            &event,
            format!(r#"{{"pull_request": {{"head": {{"repo": {{"full_name": "{head}"}}}}}}}}"#),
        )
        .expect("write");
        let mut cmd = r.hushrun();
        cmd.env("HUSHRUN_BACKEND", format!("{}/o/acme", server.url()))
            .env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_EVENT_NAME", "pull_request")
            .env("GITHUB_EVENT_PATH", &event)
            .env("GITHUB_REPOSITORY", "acme/back");
        let run = ran(cmd, UNIT);
        assert_eq!(run.code, 0, "{}", run.stderr);
        hushrun_lines(&run.stderr)
            .last()
            .map(|line| line.to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        said("someone/back"),
        "hushrun: results aren't shared with acme: GitHub gives a pull request from a fork no ID token, so its checks run, and record nothing"
    );
    // A pull request from the repository itself can get a token: the server's reason says how.
    assert!(
        said("acme/back")
            .ends_with("in GitHub Actions give the job `permissions: id-token: write`"),
        "{}",
        said("acme/back")
    );
    assert!(fake.recorded().is_empty());
}

// covers: C-13, C-14
#[test]
fn without_a_token_it_runs_and_says_why_once() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_BACKEND", format!("{}/o/acme", server.url()));
    let before = fake.requests();
    let run = ran(cmd, UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("git version"));
    // One line, last, with the server's reason: no 401 for the lookup and another for the
    // record.
    assert_eq!(
        hushrun_lines(&run.stderr),
        [
            "hushrun: results aren't shared with acme: no token: run hushrun login, set HUSHRUN_TOKEN, or in GitHub Actions give the job `permissions: id-token: write`"
        ],
        "{}",
        run.stderr
    );
    // The lookup was refused, so hushrun didn't try to record.
    assert_eq!(fake.requests_since(before, "POST"), 0);
    assert!(fake.recorded().is_empty());
}

// covers: C-11
#[test]
fn package_json_and_hushrun_yaml_naming_two_backends_is_said_once() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    let acme = format!("{}/o/acme", server.url());
    let initech = format!("{}/o/initech", server.url());
    r.name_backend_in_policy(&initech);
    r.name_backend(&acme);
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_TOKEN", TOKEN);
    let run = ran(cmd, UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let said = format!(
        "package.json's hushrun.backend is {acme} and hushrun.yaml's backend is {initech}, so results go to package.json's: hushrun init --org initech finishes a move there, or make the two the same"
    );
    assert_eq!(run.stderr.matches(&said).count(), 1, "{}", run.stderr);
    // package.json's wins, as C-11 says.
    let paths: Vec<String> = fake
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, path, _)| path.clone())
        .collect();
    assert!(paths.iter().any(|p| p.starts_with("/o/acme/")), "{paths:?}");
    assert!(
        !paths.iter().any(|p| p.starts_with("/o/initech/")),
        "{paths:?}"
    );
    let mut quiet = r.hushrun();
    quiet.arg("-q");
    assert!(!ran(quiet, UNIT).stderr.contains(&said));
}

// covers: C-11, C-13 (ADR 0037: a backend hushrun.yaml names is a file in the repository, like
// package.json's)
#[test]
fn hushrun_yaml_names_a_backend_with_the_same_rules_as_package_json() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    let backend = format!("{}/o/acme", server.url());
    r.name_backend_in_policy(&backend);

    // A CI secret doesn't follow a file in the repository.
    let before = fake.requests();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_TOKEN", TOKEN);
    let run = ran(cmd, UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let tokens = fake.tokens_since(before);
    assert!(!tokens.is_empty());
    assert!(tokens.iter().all(Option::is_none), "{tokens:?}");
    assert!(
        run.stderr.contains(&format!(
            "hushrun: HUSHRUN_TOKEN isn't sent to {backend}: it goes only to the backend HUSHRUN_BACKEND names"
        )),
        "{}",
        run.stderr
    );
    assert!(fake.recorded().is_empty());

    // Nor does an agent token, off hushrun's host.
    let before = fake.requests();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_TOKEN", AGENT);
    ran(cmd, UNIT);
    assert!(fake.tokens_since(before).iter().all(Option::is_none));

    // HUSHRUN_BACKEND comes first, with an empty one turning results off.
    let before = fake.requests();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_BACKEND", "");
    ran(cmd, UNIT);
    assert!(fake.tokens_since(before).is_empty());

    // whoami says which file named it.
    let mut cmd = r.hushrun();
    cmd.args(["whoami", "--format", "json"]);
    let output = cmd.output().expect("whoami");
    let who: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(who["named_by"], "hushrun.yaml", "{who}");
    schema::assert_valid(&schema::load("whoami.v1.schema.json"), &who);
}

// covers: C-12, C-14
#[test]
fn an_unreachable_backend_just_runs() {
    let r = Repo::new();
    let mut cmd = r.hushrun();
    // Nothing listens on the discard port.
    cmd.env("HUSHRUN_BACKEND", "http://127.0.0.1:9/o/acme");
    let started = Instant::now();
    let run = ran(cmd, UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("git version"));
    assert!(
        run.stderr
            .contains("unit running: can't read earlier results"),
        "{}",
        run.stderr
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
}

// covers: C-13
#[test]
fn a_denied_sign_in_keeps_nothing() {
    let (server, _fake) = Fake::start(true);
    let r = Repo::new();
    let denied = login(&r, &server);
    assert_eq!(denied.code, 1, "{}", denied.stderr);
    assert!(
        denied
            .stderr
            .contains("not signed in: the sign-in was denied in the browser"),
        "{}",
        denied.stderr
    );
    assert!(!r.credentials().exists());
}

// covers: C-11, C-13
#[test]
fn logout_revokes_the_token_and_forgets_it() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    assert_eq!(login(&r, &server).code, 0);

    let out = ran(r.hushrun(), &["logout"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        format!("Signed out of {} (ana@acme.dev).", server.url())
    );
    assert!(fake.revoked.load(Ordering::SeqCst));
    let saved = r.saved();
    assert!(saved.get("backend").is_none(), "{saved}");
    assert_eq!(saved["hosts"], json!({}));

    // hushrun run is back on the local cache: the server hears nothing.
    let before = fake.requests();
    let run = ran(r.hushrun(), UNIT);
    assert!(run.stdout.contains("git version"), "{}", run.stderr);
    assert_eq!(fake.requests(), before);

    let again = ran(r.hushrun(), &["logout"]);
    assert_eq!(again.stdout.trim(), "You aren't signed in anywhere.");
}

// covers: C-13
#[test]
fn login_needs_an_https_backend() {
    let r = Repo::new();
    let file = ran(
        r.hushrun(),
        &["login", "file:///tmp/results", "--no-browser"],
    );
    assert_eq!(file.code, 2, "{}", file.stderr);
    assert!(
        file.stderr
            .contains("hushrun login signs in to an https:// backend"),
        "{}",
        file.stderr
    );
    let mut host = r.hushrun();
    host.env("HUSHRUN_HOST", "ftp://hushrun.invalid");
    let host = ran(host, &["login", "--no-browser"]);
    assert_eq!(host.code, 2, "{}", host.stderr);
    assert!(
        host.stderr
            .contains("hushrun login signs in to an https:// backend"),
        "{}",
        host.stderr
    );
    assert!(!r.credentials().exists());
}

// covers: C-13
#[test]
fn login_without_a_url_signs_in_to_the_hosted_backend() {
    let (server, fake) = Fake::approving("acme");
    let r = Repo::new();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_HOST", server.url());
    let signed_in = ran(cmd, &["login", "--no-browser"]);
    assert_eq!(signed_in.code, 0, "{}", signed_in.stderr);
    assert!(
        signed_in.stderr.contains(&format!(
            "Signing in to {}, hushrun's hosted backend.",
            server.url()
        )),
        "{}",
        signed_in.stderr
    );
    // No org to hint at: the person picks one when they approve.
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1, "{started:?}");
    assert!(started[0].get("org").is_none(), "{started:?}");
    assert!(
        started[0]["client"]
            .as_str()
            .is_some_and(|c| c.starts_with("hushrun ")),
        "{started:?}"
    );
    // The org they approved it for is the backend.
    assert!(
        signed_in
            .stdout
            .contains("Signed in to acme as ana@acme.dev: hushrun run now shares results there."),
        "{}",
        signed_in.stdout
    );
    let saved = r.saved();
    assert_eq!(saved["backend"], format!("{}/o/acme", server.url()));
    assert_eq!(saved["hosts"][server.url()]["token"], TOKEN);

    // hushrun run shares results there.
    assert!(ran(r.hushrun(), UNIT).stdout.contains("git version"));
    assert_eq!(fake.recorded_at("acme").len(), 1);
}

// covers: C-13
#[test]
fn a_host_that_is_starting_while_the_sign_in_waits_is_asked_again() {
    let (server, fake) = Fake::approving("acme");
    // More than the retries of one request: the poll itself gets a 503.
    fake.busy.store(6, Ordering::SeqCst);
    let r = Repo::new();
    let signed_in = login(&r, &server);
    assert_eq!(signed_in.code, 0, "{}", signed_in.stderr);
    assert!(
        signed_in.stderr.contains(&format!(
            "hushrun: {} answered 503: asking again until the code expires\n",
            server.url()
        )),
        "{}",
        signed_in.stderr
    );
    assert_eq!(
        signed_in.stderr.matches("asking again").count(),
        1,
        "{}",
        signed_in.stderr
    );
    assert_eq!(r.saved()["hosts"][server.url()]["token"], TOKEN);
}

// covers: C-13
#[test]
fn a_code_ends_when_it_expires_whatever_the_server_asks() {
    let (server, fake) = Fake::approving("acme");
    fake.slow.store(true, Ordering::SeqCst);
    fake.expires_in.store(1, Ordering::SeqCst);
    let r = Repo::new();
    let started = Instant::now();
    let out = login(&r, &server);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains(
            "not signed in: the code BCDF-GHJK expired before it was approved: run the command again for a new one"
        ),
        "{}",
        out.stderr
    );
    assert!(!r.credentials().exists());
}

// covers: C-13
#[test]
fn an_org_that_refuses_the_person_doesnt_become_the_backend() {
    let (server, fake) = Fake::approving("acme");
    let r = Repo::new();
    let acme = format!("{}/o/acme", server.url());
    assert_eq!(login(&r, &server).code, 0);
    assert_eq!(r.saved()["backend"], acme);

    fake.outsider_of.lock().unwrap().push("initech".into());
    fake.polls.store(0, Ordering::SeqCst);
    let initech = format!("{}/o/initech", server.url());
    let out = ran(r.hushrun(), &["login", &initech, "--no-browser"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains(&format!(
            "warning: signed in as ana@acme.dev, but {initech} refused it, so results still go to {acme}: "
        )),
        "{}",
        out.stderr
    );
    assert_eq!(r.saved()["backend"], acme);
    assert_eq!(r.saved()["hosts"][server.url()]["token"], TOKEN);
}

// covers: C-13
#[test]
fn an_org_url_hints_at_its_org_and_stays_the_backend() {
    // The person approves the sign-in for another org: the URL given still wins.
    let (server, fake) = Fake::approving("globex");
    let r = Repo::new();
    let signed_in = login(&r, &server);
    assert_eq!(signed_in.code, 0, "{}", signed_in.stderr);
    // A URL on the command line needs no word on where it came from.
    assert!(
        !signed_in.stderr.contains("Signing in to"),
        "{}",
        signed_in.stderr
    );
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started[0]["org"], "acme", "{started:?}");
    assert_eq!(r.saved()["backend"], format!("{}/o/acme", server.url()));
    assert!(
        signed_in
            .stdout
            .contains("Signed in to acme as ana@acme.dev"),
        "{}",
        signed_in.stdout
    );
}

// covers: C-11, C-13
#[test]
fn login_signs_in_to_the_repositorys_backend() {
    let (server, fake) = Fake::approving("acme");
    let r = Repo::new();
    let backend = format!("{}/o/acme", server.url());
    r.name_backend(&backend);
    // HUSHRUN_HOST is nowhere: the repository's backend comes first.
    let signed_in = ran(r.hushrun(), &["login", "--no-browser"]);
    assert_eq!(signed_in.code, 0, "{}", signed_in.stderr);
    assert!(
        signed_in.stderr.contains(&format!(
            "Signing in to {backend}, which this repository's package.json names."
        )),
        "{}",
        signed_in.stderr
    );
    assert_eq!(fake.started.lock().unwrap()[0]["org"], "acme");
    assert_eq!(r.saved()["backend"], backend);

    // HUSHRUN_BACKEND comes before it.
    let (other, other_fake) = Fake::approving("globex");
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_BACKEND", format!("{}/o/globex", other.url()));
    let env = ran(cmd, &["login", "--no-browser"]);
    assert_eq!(env.code, 0, "{}", env.stderr);
    assert!(
        env.stderr.contains("which HUSHRUN_BACKEND names"),
        "{}",
        env.stderr
    );
    assert_eq!(other_fake.started.lock().unwrap()[0]["org"], "globex");
}

// covers: C-13
#[test]
fn a_server_that_names_no_org_leaves_results_on_this_machine() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    let signed_in = ran(r.hushrun(), &["login", &server.url(), "--no-browser"]);
    assert_eq!(signed_in.code, 0, "{}", signed_in.stderr);
    assert!(
        signed_in
            .stdout
            .contains(&format!("Signed in to {} as ana@acme.dev.", server.url())),
        "{}",
        signed_in.stdout
    );
    assert!(
        signed_in.stderr.contains(&format!(
            "warning: the sign-in wasn't approved for an organization, so results stay on this machine: create or join one at http://dashboard.invalid/new, then run hushrun login {0}/o/<org> to share them",
            server.url()
        )),
        "{}",
        signed_in.stderr
    );
    let saved = r.saved();
    assert!(saved.get("backend").is_none(), "{saved}");
    assert_eq!(saved["hosts"][server.url()]["token"], TOKEN);

    // An org's URL on another origin is no use to a token for this one.
    fake.approve(json!({"org": "acme", "backend": "https://elsewhere.invalid/o/acme"}));
    fake.polls.store(0, Ordering::SeqCst);
    let elsewhere = ran(r.hushrun(), &["login", &server.url(), "--no-browser"]);
    assert_eq!(elsewhere.code, 0, "{}", elsewhere.stderr);
    assert!(
        elsewhere
            .stderr
            .contains("named no URL here for acme, so results stay on this machine"),
        "{}",
        elsewhere.stderr
    );
    assert!(r.saved().get("backend").is_none());

    // hushrun run keeps results here: the server hears nothing.
    let before = fake.requests();
    assert!(ran(r.hushrun(), UNIT).stdout.contains("git version"));
    assert_eq!(fake.requests(), before);
}

// covers: C-11
#[test]
fn the_repositorys_backend_comes_after_hushrun_backend_and_before_the_login() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    assert_eq!(login(&r, &server).code, 0);
    r.name_backend(&format!("{}/o/globex", server.url()));

    // Before the login's backend, with the login's token: same origin.
    let run = ran(r.hushrun(), UNIT);
    assert!(run.stdout.contains("git version"), "{}", run.stderr);
    assert_eq!(fake.recorded_at("globex").len(), 1);
    assert!(fake.recorded_at("acme").is_empty());

    // After HUSHRUN_BACKEND.
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_BACKEND", format!("{}/o/acme", server.url()));
    let run = ran(cmd, UNIT);
    assert!(run.stdout.contains("git version"), "{}", run.stderr);
    assert_eq!(fake.recorded_at("acme").len(), 1);
    assert_eq!(fake.recorded_at("globex").len(), 1);
}

// covers: C-11, C-13
#[test]
fn hushrun_token_goes_only_to_the_backend_hushrun_backend_names() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    let backend = format!("{}/o/acme", server.url());
    r.name_backend(&backend);

    // The repository names the backend: a CI secret doesn't follow a file in it.
    let before = fake.requests();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_TOKEN", TOKEN);
    let run = ran(cmd, UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("git version"));
    let tokens = fake.tokens_since(before);
    assert!(!tokens.is_empty());
    assert!(tokens.iter().all(Option::is_none), "{tokens:?}");
    assert!(
        run.stderr.contains(&format!(
            "hushrun: HUSHRUN_TOKEN isn't sent to {backend}: it goes only to the backend HUSHRUN_BACKEND names"
        )),
        "{}",
        run.stderr
    );
    assert!(fake.recorded().is_empty());

    // HUSHRUN_BACKEND names it: the token goes there, with the machine key that signs for it.
    let before = fake.requests();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_TOKEN", TOKEN)
        .env("HUSHRUN_BACKEND", &backend)
        .env("HUSHRUN_MACHINE_KEY", machine_key().to_pem().as_str());
    let run = ran(cmd, UNIT);
    assert!(run.stdout.contains("git version"), "{}", run.stderr);
    assert!(!run.stderr.contains("isn't sent"), "{}", run.stderr);
    let tokens = fake.tokens_since(before);
    assert!(!tokens.is_empty());
    assert!(
        tokens.iter().all(|t| t.as_deref() == Some(TOKEN)),
        "{tokens:?}"
    );
    assert_eq!(fake.recorded_at("acme").len(), 1);
}

// covers: C-19 (ADR 0067: signed out of the backend the repository names, usage says whose plan
// counts results once someone signs in)
#[test]
fn signed_out_usage_says_whose_plan_counts_results_once_signed_in() {
    let (server, fake) = Fake::start(false);
    let backend = format!("{}/o/acme", server.url());
    let r = Repo::new();
    r.name_backend(&backend);
    let mut cmd = r.hushrun();
    cmd.args(["repository", "usage"]);
    let out = cmd.output().expect("usage");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains(
            "usage      results stay on this machine, so nothing is metered: hushrun login shares them with acme, whose plan then counts them\n"
        ),
        "{text}"
    );
    assert_eq!(
        fake.requests(),
        0,
        "a signed-out usage asks the server nothing"
    );
}

// covers: C-11, C-14 (ADR 0067: a repository's backend that nothing here signs in to leaves
// results on this machine, and says how to share them)
#[test]
fn a_repositorys_backend_without_any_credential_keeps_results_here() {
    let (server, fake) = Fake::start(false);
    let backend = format!("{}/o/acme", server.url());
    let r = Repo::new();
    r.name_backend(&backend);

    // Without a terminal, such as a script or an agent's sandbox: no one to sign in, so why,
    // once, and nothing asked of the server.
    let run = ran(r.hushrun(), UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("git version"));
    let lines = hushrun_lines(&run.stderr);
    assert_eq!(lines.len(), 3, "{}", run.stderr);
    assert!(
        lines[1].ends_with("; it won't run again on this code"),
        "{}",
        run.stderr
    );
    assert_eq!(
        lines[2],
        "hushrun: results aren't shared with acme: nobody is signed in to it here, so they stay on this machine",
        "{}",
        run.stderr
    );
    assert_eq!(
        fake.requests(),
        0,
        "a signed-out run asks the server nothing"
    );
    // This machine's directory keeps it, and the next run here skips.
    let again = ran(r.hushrun(), UNIT);
    assert!(!again.stdout.contains("git version"), "{}", again.stderr);
    assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
    assert_eq!(fake.requests(), 0);
    // whoami reads it from the same place: here, and why.
    let mut cmd = r.hushrun();
    cmd.args(["whoami", "--format", "json"]);
    let who: Value = serde_json::from_slice(&cmd.output().expect("whoami").stdout).expect("JSON");
    schema::assert_valid(&schema::load("whoami.v1.schema.json"), &who);
    assert_eq!(who["backend"], Value::Null, "{who}");
    assert_eq!(who["signed_out"], json!(backend), "{who}");
    assert_eq!(who["named_by"], "package.json", "{who}");
    assert!(who["directory"].is_string(), "{who}");
    let mut cmd = r.hushrun();
    cmd.arg("whoami");
    let text = String::from_utf8_lossy(&cmd.output().expect("whoami").stdout).into_owned();
    assert!(
        text.contains(&format!(
            "share     this repository shares results at {backend}: sign in with hushrun login"
        )),
        "{text}"
    );

    // At a terminal: the hint is hushrun's last line (C-11, C-14).
    #[cfg(unix)]
    {
        let (code, terminal) = terminal::stderr_on_a_terminal(r.command(&r.root.join("cache")));
        assert_eq!(code, 0, "{terminal}");
        assert_eq!(
            hushrun_lines(&terminal).last().copied(),
            Some("hushrun: sign in to share results with acme: npx hushrun login"),
            "{terminal}"
        );
        assert_eq!(fake.requests(), 0);
    }

    // hushrun.yaml's backend is the repository's too.
    let yaml = Repo::new();
    yaml.name_backend_in_policy(&backend);
    assert!(ran(yaml.hushrun(), UNIT).stdout.contains("git version"));
    assert!(!ran(yaml.hushrun(), UNIT).stdout.contains("git version"));
    assert_eq!(fake.requests(), 0);
}

// covers: C-11, C-14 (ADR 0067: only no credential at all, outside CI, keeps results here)
#[test]
fn a_backend_the_environment_names_or_a_refused_login_never_keeps_results_here() {
    let (server, fake) = Fake::start(false);
    let backend = format!("{}/o/acme", server.url());
    let r = Repo::new();
    r.name_backend(&backend);
    let twice = |vars: &[(&str, &str)]| {
        let before = fake.requests();
        for _ in 0..2 {
            let mut cmd = r.hushrun();
            for (name, value) in vars {
                cmd.env(name, value);
            }
            let run = ran(cmd, UNIT);
            assert!(
                run.stdout.contains("git version"),
                "{vars:?}: {}",
                run.stderr
            );
            assert!(
                !run.stderr.contains("they stay on this machine"),
                "{vars:?}: {}",
                run.stderr
            );
        }
        assert!(fake.requests() > before, "{vars:?} asks the server");
    };
    // HUSHRUN_BACKEND names it, with no token.
    twice(&[("HUSHRUN_BACKEND", &backend)]);
    // In CI, as a job with no ID token.
    twice(&[("CI", "true")]);
    // A login the server refuses: it is a credential, refused.
    std::fs::create_dir_all(r.credentials().parent().unwrap()).unwrap();
    std::fs::write(
        r.credentials(),
        json!({"hosts": {server.url(): {"token": "hru_refused", "email": "ana@acme.dev"}}})
            .to_string(),
    )
    .unwrap();
    twice(&[]);
    assert!(!r.root.join("cache/results/results").exists());
}

/// A repository that names `server`'s org acme as its backend, on hushrun's host, and the
/// command that runs hushrun there with `token` as HUSHRUN_TOKEN and an agent's key as
/// HUSHRUN_MACHINE_KEY, which a fake that registers machines must be given ([`Fake::give_key`]).
fn on_hushruns_host(server: &Server, token: &str) -> (Repo, impl Fn(&Repo) -> assert_cmd::Command) {
    let r = Repo::new();
    r.name_backend(&format!("{}/o/acme", server.url()));
    let host = server.url();
    let token = token.to_string();
    let pem = machine_key().to_pem().to_string();
    let hushrun = move |r: &Repo| {
        let mut cmd = r.hushrun();
        cmd.env("HUSHRUN_HOST", &host)
            .env("HUSHRUN_TOKEN", &token)
            .env("HUSHRUN_MACHINE_KEY", &pem);
        cmd
    };
    (r, hushrun)
}

/// A machine key, as hushrun makes one: P-256.
fn machine_key() -> hushrun_attest::SigningKey {
    hushrun_attest::SigningKey::generate_p256().expect("a P-256 key")
}

// covers: C-10, C-13
#[test]
fn an_agent_token_follows_the_repositorys_backend_on_hushruns_host() {
    let (server, fake) = Fake::start(false);
    fake.add_agent(
        AGENT,
        Agent::new("fleet-a", "acme", Some("github.com/acme/shop")),
    );
    let (r, hushrun) = on_hushruns_host(&server, AGENT);

    let before = fake.requests();
    let mut cmd = hushrun(&r);
    cmd.env("HUSHRUN_INVOKED_BY", "agent:worker-7");
    let run = ran(cmd, UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("git version"));
    assert!(!run.stderr.contains("isn't sent"), "{}", run.stderr);
    assert!(
        run.stderr.contains("unit passed in") && run.stderr.contains("it won't run again"),
        "{}",
        run.stderr
    );
    let tokens = fake.tokens_since(before);
    assert!(!tokens.is_empty());
    assert!(
        tokens.iter().all(|t| t.as_deref() == Some(AGENT)),
        "{tokens:?}"
    );
    let recorded = fake.recorded_at("acme");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    // The label goes with the record; the server files it under the token's repository.
    assert_eq!(recorded[0]["invokedBy"], "agent:worker-7");
    assert_eq!(recorded[0]["repository"], "github.com/acme/shop");
    schema::assert_valid(&schema::load("run-result.v0.schema.json"), &recorded[0]);

    // Another sandbox of the fleet skips.
    let mut cmd = hushrun(&r);
    cmd.env("HUSHRUN_CACHE_DIR", r.root.join("other-cache"));
    let skipped = ran(cmd, UNIT);
    assert!(skipped.stdout.is_empty(), "{}", skipped.stderr);
    assert!(
        skipped
            .stderr
            .contains("unit skipped: this exact code passed"),
        "{}",
        skipped.stderr
    );

    // A label that isn't one is left out, in a line.
    let mut cmd = hushrun(&r);
    cmd.env("HUSHRUN_INVOKED_BY", "worker-7");
    let run = ran(cmd, &["run", "unit", "--force", "--", "git", "--version"]);
    assert!(
        run.stderr.contains(
            "hushrun: HUSHRUN_INVOKED_BY is \"worker-7\", not agent:<id> or human:<id>, so runs aren't labeled"
        ),
        "{}",
        run.stderr
    );
    let last = fake.recorded_at("acme");
    assert!(
        last.iter().all(|r| r["invokedBy"] != "worker-7"),
        "{last:?}"
    );
}

// covers: C-11, C-13
#[test]
fn an_agent_token_stays_off_other_hosts_and_other_tokens_stay_put() {
    let (server, fake) = Fake::start(false);
    fake.add_agent(AGENT, Agent::new("fleet-a", "acme", None));
    let r = Repo::new();
    let backend = format!("{}/o/acme", server.url());
    r.name_backend(&backend);

    // hushrun's host is elsewhere: the repository's backend doesn't get the agent token.
    let before = fake.requests();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_TOKEN", AGENT);
    let run = ran(cmd, UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(fake.tokens_since(before).iter().all(Option::is_none));
    assert!(
        run.stderr.contains(&format!(
            "hushrun: HUSHRUN_TOKEN isn't sent to {backend}: it goes only to the backend HUSHRUN_BACKEND names, and an agent token (hra_…) to hushrun's host, {NO_HOST}"
        )),
        "{}",
        run.stderr
    );
    // That line says why nothing is shared: not the server's "set HUSHRUN_TOKEN" after it.
    assert!(
        !run.stderr.contains("results aren't shared"),
        "{}",
        run.stderr
    );

    // A personal token in HUSHRUN_TOKEN keeps ADR 0023's rule, on hushrun's host too.
    let before = fake.requests();
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_TOKEN", TOKEN)
        .env("HUSHRUN_HOST", server.url());
    let run = ran(cmd, UNIT);
    assert!(fake.tokens_since(before).iter().all(Option::is_none));
    assert!(run.stderr.contains("isn't sent"), "{}", run.stderr);
    assert!(fake.recorded().is_empty());
}

// covers: C-14
#[test]
fn a_refused_agent_token_says_why_once_and_records_nothing() {
    let (server, fake) = Fake::start(false);
    for (token, state) in [
        (AGENT, "revoked"),
        ("hra_expiredexpiredexpiredexpiredexpired00", "expired"),
    ] {
        let mut agent = Agent::new("fleet-a", "acme", None);
        agent.state = state;
        fake.add_agent(token, agent);
        let (r, hushrun) = on_hushruns_host(&server, token);
        let before = fake.requests();
        let run = ran(hushrun(&r), UNIT);
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(run.stdout.contains("git version"));
        let lines = hushrun_lines(&run.stderr);
        assert_eq!(lines.len(), 2, "{}", run.stderr);
        let prefix = &token[..10];
        assert!(
            lines[0].starts_with(&format!(
                "hushrun: unit running: acme refused HUSHRUN_TOKEN: agent token {prefix} (agent:fleet-a) {}",
                if state == "revoked" { "was revoked" } else { "expired" }
            )) && lines[0].ends_with("hushrun token create --agent fleet-a"),
            "{}",
            run.stderr
        );
        assert!(
            lines[1].starts_with("hushrun: unit passed in") && lines[1].ends_with("; not recorded"),
            "{}",
            run.stderr
        );
        assert_eq!(fake.requests_since(before, "POST"), 0);
    }
    assert!(fake.recorded().is_empty());
}

// covers: C-14
#[test]
fn a_result_for_another_repository_isnt_recorded() {
    let (server, fake) = Fake::start(false);
    fake.add_agent(
        AGENT,
        Agent::new("fleet-a", "acme", Some("github.com/acme/other")),
    );
    let (r, hushrun) = on_hushruns_host(&server, AGENT);
    r.remote("https://github.com/acme/shop.git");
    let run = ran(hushrun(&r), UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let last = *hushrun_lines(&run.stderr).last().unwrap();
    assert!(
        last.starts_with("hushrun: unit passed in")
            && last.contains("; not recorded: agent token hra_012345 (agent:fleet-a) records results for github.com/acme/other only, and this result is for github.com/acme/shop"),
        "{}",
        run.stderr
    );
    assert!(fake.recorded().is_empty());
}

// covers: C-12, C-14
#[test]
fn a_token_over_its_limit_runs_and_says_so_in_one_line() {
    let (server, fake) = Fake::start(false);
    let mut agent = Agent::new("fleet-a", "acme", None);
    agent.limit = 1;
    fake.add_agent(AGENT, agent);
    let (r, hushrun) = on_hushruns_host(&server, AGENT);
    assert_eq!(ran(hushrun(&r), UNIT).code, 0);
    assert_eq!(fake.recorded().len(), 1);

    let started = Instant::now();
    let run = ran(
        hushrun(&r),
        &["run", "unit", "--force", "--", "git", "--version"],
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("git version"));
    assert_eq!(
        hushrun_lines(&run.stderr).last().copied(),
        Some(
            format!(
                "hushrun: unit passed in {}; not recorded: agent:fleet-a (token hra_012345) recorded 1 results in the last minute, its limit: this one isn't recorded",
                run.stderr.split("unit passed in ").nth(1).and_then(|t| t.split(';').next()).unwrap_or("?")
            )
            .as_str()
        ),
        "{}",
        run.stderr
    );
    // Retry-After: 60 is past the run's budget: hushrun didn't wait for it.
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(fake.recorded().len(), 1);
}

// covers: C-17
#[test]
fn an_admin_creates_lists_and_revokes_agent_tokens() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    let backend = format!("{}/o/acme", server.url());
    let token = |args: &[&str]| {
        let mut all = vec!["token"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["--backend", &backend]);
        let mut cmd = r.hushrun();
        cmd.args(&all).arg("--now").arg("2026-10-20T10:00:00Z");
        let output = cmd.output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    };

    // Without a login: nothing to act as.
    let refused = token(&["list"]);
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(
        refused.stderr.contains(&format!(
            "agent tokens take an org admin's login at {}: run hushrun login {backend}",
            server.url()
        )),
        "{}",
        refused.stderr
    );

    assert_eq!(login(&r, &server).code, 0);
    let empty = token(&["list"]);
    assert_eq!(empty.code, 0, "{}", empty.stderr);
    assert_eq!(
        empty.stdout.trim(),
        "acme has no agent tokens. Create one with hushrun token create --agent <name>."
    );

    // The token alone on stdout; what it is, and the environment, on stderr.
    let created = token(&[
        "create",
        "--agent",
        "fleet-a",
        "--repository",
        "github.com/acme/shop",
        "--expires",
        "12h",
        "--rate",
        "50",
    ]);
    assert_eq!(created.code, 0, "{}", created.stderr);
    let secret = created.stdout.trim().to_string();
    assert!(
        secret.starts_with("hra_") && !secret.contains('\n'),
        "{}",
        created.stdout
    );
    // ADR 0046: piped, the string stays out, and stderr says how a script gets it.
    assert!(
        !created.stderr.contains("hushrun_agent_v1_")
            && created
                .stderr
                .contains("--format json gives it as hushrun_agent"),
        "{}",
        created.stderr
    );
    assert!(
        created.stderr.contains(&format!(
            "Created {}… for agent:fleet-a in acme: github.com/acme/shop, expires 2026-11-19 10:00 UTC (in 30 days), up to 50 results a minute.",
            &secret[..10]
        )),
        "{}",
        created.stderr
    );
    assert!(
        created
            .stderr
            .contains("HUSHRUN_TOKEN=<the token on stdout>")
            && !created.stderr.contains(&secret),
        "{}",
        created.stderr
    );
    // hushrun's host is elsewhere, so the sandbox needs the backend too.
    assert!(
        created
            .stderr
            .contains(&format!("HUSHRUN_BACKEND={backend}")),
        "{}",
        created.stderr
    );
    let mut body = fake.created.lock().unwrap()[0].clone();
    // ADR 0028, amended: the agent's key, made here, goes with the request; the fake checked
    // its proof.
    let machine = body.as_object_mut().unwrap().remove("machine").unwrap();
    assert!(
        machine["key"].as_str().unwrap().starts_with("ecdsa-p256:"),
        "{machine}"
    );
    assert_eq!(
        body,
        json!({"name": "fleet-a", "repository": "github.com/acme/shop", "expires_in_hours": 12, "results_per_minute": 50})
    );
    // ADR 0046: stderr isn't a terminal here, so the key stays out, and it says what to do.
    assert!(
        !created.stderr.contains("PRIVATE KEY")
            && created
                .stderr
                .contains("Without its key this token records nothing")
            && created
                .stderr
                .contains("--format json, which gives the token, the key and the string"),
        "{}",
        created.stderr
    );

    // By default, for the repository hushrun runs in; --org-wide for every one.
    r.remote("git@github.com:acme/shop.git");
    assert_eq!(token(&["create", "--agent", "fleet-a"]).code, 0);
    let json_out = {
        let mut cmd = r.hushrun();
        cmd.args([
            "token",
            "create",
            "--agent",
            "gitlab-ci",
            "--org-wide",
            "--format",
            "json",
            "--backend",
            &backend,
        ]);
        cmd.output().expect("hushrun")
    };
    assert!(json_out.status.success());
    let answer: Value = serde_json::from_slice(&json_out.stdout).unwrap();
    schema::assert_valid(&schema::load("token.v1.schema.json"), &answer);
    assert_eq!(answer["org"], "acme");
    assert!(
        answer["token"]
            .as_str()
            .is_some_and(|t| t.starts_with("hra_"))
    );
    let bodies = fake.created.lock().unwrap().clone();
    assert_eq!(bodies[1]["repository"], "github.com/acme/shop");
    assert_eq!(bodies[1]["expires_in_hours"], 720);
    assert_eq!(bodies[2]["repository"], Value::Null);

    // The list, then a revoke by name that two tokens share, and by prefix.
    let listed = token(&["list"]);
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    let lines: Vec<&str> = listed.stdout.lines().collect();
    assert_eq!(lines.len(), 4, "{}", listed.stdout);
    assert!(
        lines[0].starts_with("TOKEN") && lines[0].contains("CREATED BY"),
        "{}",
        listed.stdout
    );
    assert!(
        listed.stdout.contains("agent:gitlab-ci")
            && listed.stdout.contains("whole org")
            && listed.stdout.contains("in 30 days")
            && listed.stdout.contains("never")
            && listed.stdout.contains("ana@acme.dev"),
        "{}",
        listed.stdout
    );
    let ambiguous = token(&["revoke", "fleet-a"]);
    assert_eq!(ambiguous.code, 2, "{}", ambiguous.stderr);
    assert!(
        ambiguous.stderr.contains("agent:fleet-a has 2 tokens"),
        "{}",
        ambiguous.stderr
    );
    let revoked = token(&["revoke", &secret[..10]]);
    assert_eq!(revoked.code, 0, "{}", revoked.stderr);
    assert_eq!(
        revoked.stdout.trim(),
        format!(
            "Revoked {}… (agent:fleet-a, github.com/acme/shop): anything that uses it is refused from now on.",
            &secret[..10]
        )
    );
    assert_eq!(token(&["list"]).stdout.lines().count(), 3);
    let gone = token(&["revoke", &secret[..10]]);
    assert_eq!(gone.code, 1, "{}", gone.stderr);

    // The token it created records, as its agent, signed with the key made with it.
    let (_, hushrun) = on_hushruns_host(&server, answer["token"].as_str().unwrap());
    let fresh = Repo::new();
    fresh.name_backend(&backend);
    let mut cmd = hushrun(&fresh);
    cmd.env(
        "HUSHRUN_MACHINE_KEY",
        answer["machine_key"]["pem"].as_str().unwrap(),
    );
    let run = ran(cmd, UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(fake.recorded_at("acme").len(), 1);

    // Outside a repository, the scope must be named.
    let outside = tempfile::tempdir().unwrap();
    let mut cmd = r.hushrun();
    cmd.current_dir(outside.path()).args([
        "token",
        "create",
        "--agent",
        "x1",
        "--backend",
        &backend,
    ]);
    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--org-wide"));
}

// covers: C-17
#[test]
fn whoami_and_doctor_show_the_agent() {
    let (server, fake) = Fake::start(false);
    fake.add_agent(
        AGENT,
        Agent::new("fleet-a", "acme", Some("github.com/acme/shop")),
    );
    let (r, hushrun) = on_hushruns_host(&server, AGENT);
    let mut cmd = hushrun(&r);
    cmd.args(["whoami", "--now", "2026-10-20T10:00:00Z"]);
    let output = cmd.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "{text}");
    let backend = format!("{}/o/acme", server.url());
    // The key the agent signs with, from the environment, last.
    let (text, machine) = text.trim_end().rsplit_once('\n').unwrap();
    assert!(
        machine.starts_with("machine   sha256:") && machine.ends_with("… (HUSHRUN_MACHINE_KEY)"),
        "{machine}"
    );
    assert_eq!(
        format!("{text}\n"),
        format!(
            "backend   {backend} (this repository's package.json names it)\n\
             token     HUSHRUN_TOKEN, an agent token\n\
             as        agent:fleet-a in acme\n\
             agent     fleet-a (hra_012345…)\n\
             for       github.com/acme/shop\n\
             expires   2026-11-19 10:00 UTC (in 30 days)\n\
             limit     300 results a minute\n"
        )
    );
    // Without its key, whoami says which to set.
    let mut cmd = hushrun(&r);
    cmd.env_remove("HUSHRUN_MACHINE_KEY").arg("whoami");
    let keyless = String::from_utf8_lossy(&cmd.output().unwrap().stdout).into_owned();
    assert!(
        keyless.ends_with(
            "machine   none: set `HUSHRUN_MACHINE_KEY` to the key `hushrun token create --agent` printed\n"
        ),
        "{keyless}"
    );
    let mut cmd = hushrun(&r);
    cmd.args(["whoami", "--format", "json"]);
    let answer: Value = serde_json::from_slice(&cmd.output().unwrap().stdout).unwrap();
    schema::assert_valid(&schema::load("whoami.v1.schema.json"), &answer);
    assert_eq!(answer["credential"], "agent_token");
    assert_eq!(answer["as"], "agent:fleet-a");
    assert_eq!(answer["token"]["repository"], "github.com/acme/shop");

    // doctor says the same in a line, before hushrun.yaml, which this repository lacks.
    let mut cmd = hushrun(&r);
    cmd.args(["doctor", "--now", "2026-10-20T10:00:00Z"]);
    let output = cmd.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        text.contains(&format!(
            "ok    results              {backend} as agent:fleet-a with HUSHRUN_TOKEN, an agent token, for github.com/acme/shop, expires 2026-11-19 10:00 UTC (in 30 days)"
        )),
        "{text}"
    );

    // Revoked: whoami says the server's words, and exits 1.
    fake.agents.lock().unwrap().get_mut(AGENT).unwrap().state = "revoked";
    let mut cmd = hushrun(&r);
    cmd.arg("whoami");
    let output = cmd.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(
        text.contains("refused   ") && text.contains("was revoked"),
        "{text}"
    );

    // A login: whoami names it; the token withheld, why.
    assert_eq!(login(&r, &server).code, 0);
    let mut cmd = r.hushrun();
    cmd.arg("whoami");
    let text = String::from_utf8_lossy(&cmd.output().unwrap().stdout).into_owned();
    assert!(
        text.contains("token     the token hushrun login saved for ana@acme.dev")
            && text.contains("as        user:ana@acme.dev in acme"),
        "{text}"
    );
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_TOKEN", AGENT)
        .env("HUSHRUN_HOST", NO_HOST)
        .env_remove("HUSHRUN_CREDENTIALS");
    cmd.env("HUSHRUN_CREDENTIALS", r.root.join("none.json"))
        .arg("whoami");
    let text = String::from_utf8_lossy(&cmd.output().unwrap().stdout).into_owned();
    assert!(
        text.contains("token     none: HUSHRUN_TOKEN isn't sent here"),
        "{text}"
    );
}

#[cfg(unix)]
mod terminal {
    use std::io::Read as _;
    use std::process::{Command, Stdio};

    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    /// Runs `hushrun run unit -- git --version` as `cmd` with its stderr on a terminal of its
    /// own, and returns the exit code and what it wrote there.
    pub fn stderr_on_a_terminal(mut cmd: Command) -> (i32, String) {
        let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("openpt");
        grantpt(&controller).expect("grantpt");
        unlockpt(&controller).expect("unlockpt");
        let name = ptsname(&controller, Vec::new()).expect("ptsname");
        // CLOEXEC: a test beside this one may start a command while this end is open here, and
        // that command would go on holding the terminal once hushrun exits.
        let terminal = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("the terminal");
        cmd.args(super::UNIT)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::from(terminal));
        let mut child = cmd.spawn().expect("hushrun");
        // Only hushrun holds the terminal now, so reading it ends when hushrun exits.
        drop(cmd);
        let mut controller = std::fs::File::from(controller);
        let reader = std::thread::spawn(move || {
            let mut text = Vec::new();
            let mut buf = [0; 4096];
            // Once nothing holds the terminal, Linux answers EIO, and macOS the end.
            while let Ok(n @ 1..) = controller.read(&mut buf) {
                text.extend_from_slice(&buf[..n]);
            }
            String::from_utf8_lossy(&text).replace("\r\n", "\n")
        });
        let status = child.wait().expect("hushrun");
        (status.code().unwrap_or(-1), reader.join().expect("reader"))
    }

    /// A terminal, and what its reader will give.
    fn terminal() -> (std::fs::File, std::thread::JoinHandle<String>) {
        let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("openpt");
        grantpt(&controller).expect("grantpt");
        unlockpt(&controller).expect("unlockpt");
        let name = ptsname(&controller, Vec::new()).expect("ptsname");
        // CLOEXEC, as above.
        let terminal = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("the terminal");
        let mut controller = std::fs::File::from(controller);
        let reader = std::thread::spawn(move || {
            let mut text = Vec::new();
            let mut buf = [0; 4096];
            while let Ok(n @ 1..) = controller.read(&mut buf) {
                text.extend_from_slice(&buf[..n]);
            }
            String::from_utf8_lossy(&text).replace("\r\n", "\n")
        });
        (std::fs::File::from(terminal), reader)
    }

    /// Runs `cmd` with its stdout and its stderr each on a terminal of its own or on a pipe, and
    /// returns the exit code and what it wrote to each.
    pub fn run(mut cmd: Command, stdout: bool, stderr: bool) -> (i32, String, String) {
        let reader_of = |tty: bool, stream: &mut dyn FnMut(Stdio)| {
            if tty {
                let (end, reader) = terminal();
                stream(end.into());
                Some(reader)
            } else {
                stream(Stdio::piped());
                None
            }
        };
        let out = reader_of(stdout, &mut |io| {
            cmd.stdout(io);
        });
        let err = reader_of(stderr, &mut |io| {
            cmd.stderr(io);
        });
        cmd.stdin(Stdio::null());
        let child = cmd.spawn().expect("hushrun");
        drop(cmd);
        let output = child.wait_with_output().expect("hushrun");
        let text = |reader: Option<std::thread::JoinHandle<String>>, piped: &[u8]| match reader {
            Some(reader) => reader.join().expect("reader"),
            None => String::from_utf8_lossy(piped).into_owned(),
        };
        (
            output.status.code().unwrap_or(-1),
            text(out, &output.stdout),
            text(err, &output.stderr),
        )
    }
}

// covers: C-13, C-18
#[test]
fn login_registers_this_machine_and_run_signs_its_results() {
    let (server, fake) = Fake::start(false);
    fake.register_machines();
    let r = Repo::new();
    let signed_in = login(&r, &server);
    assert_eq!(signed_in.code, 0, "{}", signed_in.stderr);
    // A P-256 key beside the credentials, only its owner may read, registered with proof.
    let key_file = r.root.join("config").join("machine-key.pem");
    let pem = std::fs::read_to_string(&key_file).expect("the machine key");
    assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----"), "{pem}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&key_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }
    let started = fake.started.lock().unwrap()[0].clone();
    let key = started["machine"]["key"].as_str().unwrap().to_string();
    assert!(key.starts_with("ecdsa-p256:"), "{started}");
    assert!(!started["machine"]["name"].as_str().unwrap().is_empty());
    let keyid = hushrun_core::signers::PublicKey::parse(&key)
        .unwrap()
        .keyid();
    assert_eq!(r.saved()["hosts"][server.url()]["machine"], keyid);
    assert!(
        signed_in.stdout.contains(&format!(
            "Registered this machine, {} (sha256:{}…), for its writes to {}.",
            started["machine"]["name"].as_str().unwrap(),
            &keyid["sha256:".len().."sha256:".len() + 12],
            server.url()
        )),
        "{}",
        signed_in.stdout
    );

    // Its results carry the machine's signature, which the server checks.
    let first = ran(r.hushrun(), UNIT);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let signatures = fake.signatures.lock().unwrap().clone();
    assert_eq!(signatures.len(), 1);
    assert!(
        signatures[0]
            .as_deref()
            .unwrap()
            .starts_with(&format!("{keyid} "))
    );
    // Registered at sign-in: no second registration.
    assert_eq!(fake.count("POST", "/o/acme/v0/machines"), 0);

    // Signing in again keeps the machine's key.
    fake.polls.store(0, Ordering::SeqCst);
    let again = login(&r, &server);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert_eq!(std::fs::read_to_string(&key_file).unwrap(), pem);
    assert_eq!(fake.started.lock().unwrap()[1]["machine"]["key"], key);

    // whoami shows it.
    let who = ran(r.hushrun(), &["whoami", "--format", "json"]);
    let who: Value = serde_json::from_str(&who.stdout).unwrap();
    schema::assert_valid(&schema::load("whoami.v1.schema.json"), &who);
    assert_eq!(who["machine"]["keyid"], keyid);
}

// covers: C-13, C-18 (ADR 0041: a machine is registered per org, so each org gets it once)
#[test]
fn a_machine_registers_in_each_org_it_writes_to() {
    let (server, fake) = Fake::start(false);
    fake.register_machines();
    let r = Repo::new();
    assert_eq!(login(&r, &server).code, 0, "signs in to acme");
    let acme = ran(r.hushrun(), UNIT);
    assert_eq!(acme.code, 0, "{}", acme.stderr);
    assert_eq!(fake.count("POST", "/o/acme/v0/machines"), 0);

    // The sign-in registered it in acme: another org of the same token still needs it.
    let run_in = |org: &str| {
        let mut cmd = r.hushrun();
        r.name_backend(&format!("{}/o/{org}", server.url()));
        cmd.env_remove("HUSHRUN_BACKEND");
        ran(cmd, &["run", "unit", "--force", "--", "git", "--version"])
    };
    for org in ["initech", "umbrella"] {
        for _ in 0..2 {
            let run = run_in(org);
            assert_eq!(run.code, 0, "{}", run.stderr);
            assert!(!run.stderr.contains("not recorded"), "{}", run.stderr);
        }
        // Once per org, for each of them.
        assert_eq!(fake.count("POST", &format!("/o/{org}/v0/machines")), 1);
    }
    assert_eq!(fake.count("POST", "/o/acme/v0/machines"), 0);
}

// covers: C-13, C-18
#[test]
fn a_login_from_before_machines_registers_once() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    // A login made before the server registered machines.
    assert_eq!(login(&r, &server).code, 0);
    fake.register_machines();
    for _ in 0..2 {
        assert_eq!(ran(r.hushrun(), UNIT).code, 0);
    }
    // The first run registered the machine for the login's token; the next knew it had.
    assert_eq!(fake.count("POST", "/o/acme/v0/machines"), 1);
    let signatures = fake.signatures.lock().unwrap().clone();
    assert!(signatures.iter().all(Option::is_some), "{signatures:?}");
}

// covers: C-18, C-22
#[test]
fn ready_registers_no_machine_and_the_first_result_does() {
    let (server, fake) = Fake::start(false);
    let r = Repo::new();
    // A login from before the server registered machines: none is registered for it yet.
    assert_eq!(login(&r, &server).code, 0);
    fake.register_machines();
    let manifest = json!({
        "name": "shop",
        "hushrun": {"backend": format!("{}/o/acme", server.url())},
        "scripts": {"test": "hushrun run unit -- git --version"},
    });
    std::fs::write(r.repo.join("package.json"), manifest.to_string()).expect("write");
    git::commit_all(&r.repo, "scripts");

    // `ready` only reads, and the pre-push hook runs it on every push: it spends no machine of
    // the person's in the org.
    let ready = ran(r.hushrun(), &["ready", "--format", "json"]);
    let report: Value = serde_json::from_str(&ready.stdout).expect(&ready.stderr);
    assert_eq!(report["checks"][0]["state"], "missing", "{report:#}");
    assert_eq!(fake.count("POST", "/o/acme/v0/machines"), 0);

    // The run it asks for registers the machine, with its result.
    let run = ran(r.hushrun(), &["run", "unit", "--", "git", "--version"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(fake.count("POST", "/o/acme/v0/machines"), 1);
    assert_eq!(fake.recorded().len(), 1);
}

// covers: C-17, C-18 (ADR 0028, amended 2026-10-28: one key per agent, made with its token)
#[test]
fn token_create_makes_the_agents_key_and_every_sandbox_signs_with_it() {
    let (server, fake) = Fake::start(false);
    fake.register_machines();
    let r = Repo::new();
    r.remote("git@github.com:acme/shop.git");
    assert_eq!(login(&r, &server).code, 0);

    // The token, and its key with it, once.
    let made = ran(
        r.hushrun(),
        &["token", "create", "--agent", "fleet-a", "--format", "json"],
    );
    assert_eq!(made.code, 0, "{}", made.stderr);
    let first: Value = serde_json::from_str(&made.stdout).unwrap();
    let token = first["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("hra_"), "{}", made.stdout);
    let pem = first["machine_key"]["pem"].as_str().unwrap().to_string();
    let key = hushrun_attest::SigningKey::from_pem(&pem).unwrap();
    // Registered with the token by the request that made it: no other registration.
    let machines = fake.machines.lock().unwrap().clone();
    assert!(
        machines
            .iter()
            .any(|(t, _, k)| *t == token && *k == key.public_key().keyid()),
        "{machines:?}"
    );

    // As JSON: the string, the token, the key and the machine the server registered.
    let made = ran(
        r.hushrun(),
        &["token", "create", "--agent", "fleet-b", "--format", "json"],
    );
    assert_eq!(made.code, 0, "{}", made.stderr);
    let out: Value = serde_json::from_str(&made.stdout).unwrap();
    schema::assert_valid(&schema::load("token.v1.schema.json"), &out);
    let string = out["hushrun_agent"].as_str().unwrap();
    let from_json = hushrun_core::agent::AgentString::parse(string).unwrap();
    assert_eq!(from_json.token(), out["token"].as_str().unwrap());
    assert_eq!(from_json.key(), out["machine_key"]["pem"].as_str().unwrap());
    assert_eq!(
        from_json.backend().to_string(),
        format!("{}/o/acme", server.url())
    );
    let json_key =
        hushrun_attest::SigningKey::from_pem(out["machine_key"]["pem"].as_str().unwrap()).unwrap();
    assert_eq!(out["machine_key"]["keyid"], json_key.public_key().keyid());
    assert_eq!(out["machine"]["keyid"], json_key.public_key().keyid());
    assert_eq!(out["machine"]["name"], "agent:fleet-b");

    // Each sandbox signs with that key, and none registers its own.
    let sandbox = Repo::new();
    let agent = |cache: &str| {
        let mut cmd = sandbox.hushrun_with_cache(&sandbox.root.join(cache));
        cmd.env("HUSHRUN_BACKEND", format!("{}/o/acme", server.url()))
            .env("HUSHRUN_TOKEN", &token);
        cmd
    };
    let registrations = fake.count("POST", "/o/acme/v0/machines");
    for cache in ["sandbox-1", "sandbox-2"] {
        let mut cmd = agent(cache);
        cmd.env("HUSHRUN_MACHINE_KEY", &pem);
        let run = ran(cmd, &["run", "unit", "--force", "--", "git", "--version"]);
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(
            run.stderr.contains("it won't run again on this code"),
            "{}",
            run.stderr
        );
    }
    assert_eq!(fake.count("POST", "/o/acme/v0/machines"), registrations);
    let signatures = fake.signatures.lock().unwrap().clone();
    let keyid = key.public_key().keyid();
    let ours: Vec<_> = signatures
        .iter()
        .flatten()
        .filter(|s| s.starts_with(&format!("{keyid} ")))
        .collect();
    assert_eq!(ours.len(), 2, "{signatures:?}");
    // ADR 0046: the string is all a sandbox needs, and signs the same.
    let mut cmd = sandbox.hushrun_with_cache(&sandbox.root.join("sandbox-string"));
    cmd.env("HUSHRUN_AGENT", string);
    let run = ran(cmd, &["run", "unit", "--force", "--", "git", "--version"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stderr.contains("it won't run again on this code"),
        "{}",
        run.stderr
    );
    assert_eq!(fake.count("POST", "/o/acme/v0/machines"), registrations);
    let signatures = fake.signatures.lock().unwrap().clone();
    let json_keyid = json_key.public_key().keyid();
    assert_eq!(
        signatures
            .iter()
            .flatten()
            .filter(|s| s.starts_with(&format!("{json_keyid} ")))
            .count(),
        1,
        "{signatures:?}"
    );
    // The key in a file HUSHRUN_MACHINE_KEY_FILE names signs the same.
    let file = sandbox.root.join("agent-key.pem");
    std::fs::write(&file, &pem).unwrap();
    let mut cmd = agent("sandbox-3");
    cmd.env("HUSHRUN_MACHINE_KEY_FILE", &file);
    assert_eq!(
        ran(cmd, &["run", "unit", "--force", "--", "git", "--version"]).code,
        0
    );
    assert_eq!(fake.recorded_at("acme").len(), 4);
}

// covers: C-11 (ADR 0046: with HUSHRUN_AGENT set signing in is refused, so no one is told to)
#[cfg(unix)]
#[test]
fn at_a_terminal_an_agent_isnt_told_to_sign_in() {
    let (server, _fake) = Fake::start(false);
    let r = Repo::new();
    r.name_backend(&format!("{}/o/acme", server.url()));
    let mut cmd = r.command(&r.root.join("cache"));
    cmd.env("HUSHRUN_AGENT", "not a string");
    let (code, terminal) = terminal::stderr_on_a_terminal(cmd);
    assert_eq!(code, 0, "{terminal}");
    assert!(!terminal.contains("sign in to share results"), "{terminal}");
    assert!(
        terminal.contains("results aren't shared with acme"),
        "{terminal}"
    );
}

// covers: C-17 (ADR 0046: at a terminal, the string comes first on stderr, and stdout stays the
// token alone)
#[cfg(unix)]
#[test]
fn at_a_terminal_token_create_shows_the_string_first() {
    let (server, fake) = Fake::start(false);
    fake.register_machines();
    let r = Repo::new();
    r.remote("git@github.com:acme/shop.git");
    assert_eq!(login(&r, &server).code, 0);
    let mut cmd = r.command(&r.root.join("cache"));
    cmd.args(["token", "create", "--agent", "fleet-a"]);
    let (code, stdout, stderr) = terminal::run(cmd, true, true);
    assert_eq!(code, 0, "{stderr}");
    let token = stdout.trim();
    assert!(
        token.starts_with("hra_") && !token.contains('\n'),
        "{stdout}"
    );
    let (first, rest) = stderr.split_once('\n').unwrap();
    let string = first
        .strip_prefix("HUSHRUN_AGENT=")
        .unwrap_or_else(|| panic!("{stderr}"));
    let agent = hushrun_core::agent::AgentString::parse(string).unwrap();
    assert_eq!(agent.token(), token);
    assert_eq!(
        agent.backend().to_string(),
        format!("{}/o/acme", server.url())
    );
    assert!(
        rest.contains(&format!("HUSHRUN_TOKEN={token}\n")),
        "{stderr}"
    );
    assert!(!rest.contains("hushrun_agent_v1_"), "once: {stderr}");
    assert!(
        rest.contains("HUSHRUN_MACHINE_KEY=<the private key below, as one value>")
            && rest.contains("-----BEGIN PRIVATE KEY-----"),
        "{stderr}"
    );
}

// covers: C-17 (ADR 0046: the string, the token and the key reach stderr only where a person reads
// them, since a pipe is often a log and a job's always is; stdout stays the token whatever else)
#[cfg(unix)]
#[test]
fn token_create_shows_its_secrets_only_at_a_terminal_outside_ci() {
    let (server, fake) = Fake::start(false);
    fake.register_machines();
    let r = Repo::new();
    r.remote("git@github.com:acme/shop.git");
    assert_eq!(login(&r, &server).code, 0);
    const PIPED: bool = false;
    const TERMINAL: bool = true;
    let ci = [("CI", "true"), ("HUSHRUN_RUN_ID", "1")];
    let github = [("CI", "true"), ("GITHUB_ACTIONS", "true")];
    let gitlab = [("CI", "true"), ("GITLAB_CI", "true")];
    let local: [(&str, &str); 0] = [];
    // stdout, stderr, the environment, then whether stderr holds the string, the token, the key.
    type Case<'a> = (
        &'a str,
        bool,
        bool,
        &'a [(&'a str, &'a str)],
        bool,
        bool,
        bool,
    );
    let cases: [Case; 8] = [
        ("a person", TERMINAL, TERMINAL, &local, true, true, true),
        (
            "$(…) at a terminal",
            PIPED,
            TERMINAL,
            &local,
            false,
            false,
            true,
        ),
        (
            "stderr to a log",
            TERMINAL,
            PIPED,
            &local,
            false,
            false,
            false,
        ),
        ("a script", PIPED, PIPED, &local, false, false, false),
        ("a pty in CI", TERMINAL, TERMINAL, &ci, false, false, false),
        (
            "a pty on GitHub",
            TERMINAL,
            TERMINAL,
            &github,
            false,
            false,
            false,
        ),
        (
            "a pty on GitLab",
            TERMINAL,
            TERMINAL,
            &gitlab,
            false,
            false,
            false,
        ),
        ("a GitHub step", PIPED, PIPED, &github, false, false, false),
    ];
    for (n, (case, out, err, vars, string, token, key)) in cases.into_iter().enumerate() {
        let mut cmd = r.command(&r.root.join("cache"));
        cmd.args(["token", "create", "--agent", &format!("fleet-{n}")]);
        cmd.envs(vars.iter().copied());
        let (code, stdout, stderr) = terminal::run(cmd, out, err);
        assert_eq!(code, 0, "{case}: {stderr}");
        let secret = stdout.trim();
        assert!(secret.starts_with("hra_"), "{case}: {stdout}");
        assert_eq!(
            stderr.contains("hushrun_agent_v1_"),
            string,
            "{case}: {stderr}"
        );
        assert_eq!(stderr.contains(secret), token, "{case}: {stderr}");
        assert_eq!(stderr.contains("PRIVATE KEY"), key, "{case}: {stderr}");
        if !key {
            assert!(stderr.contains("--format json"), "{case}: {stderr}");
        }
    }
}

// covers: C-14, C-17, C-18 (an agent without its key: the result isn't sent, and the run says
// which key to set; with another key, or without a signature, the server's 401 says the same)
#[test]
fn an_agent_without_its_key_says_which_key_to_set() {
    let (server, fake) = Fake::start(false);
    fake.register_machines();
    fake.add_agent(AGENT, Agent::new("fleet-a", "acme", None));
    let key = machine_key();
    fake.give_key(AGENT, &key);
    let sandbox = Repo::new();
    let agent = || {
        let mut cmd = sandbox.hushrun();
        cmd.env("HUSHRUN_BACKEND", format!("{}/o/acme", server.url()))
            .env("HUSHRUN_TOKEN", AGENT);
        cmd
    };
    let says = "set `HUSHRUN_MACHINE_KEY` to the key `hushrun token create --agent` printed";

    // No key: nothing is sent to record, not even a registration; the command runs.
    let before = fake.requests();
    let run = ran(agent(), UNIT);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("git version"));
    let last = *hushrun_lines(&run.stderr).last().unwrap();
    assert!(
        last.starts_with("hushrun: unit passed in")
            && last.ends_with(&format!("; not recorded: {says}")),
        "{}",
        run.stderr
    );
    assert_eq!(fake.requests_since(before, "POST"), 0);
    assert!(fake.recorded().is_empty());

    // A person's key file left on the machine doesn't sign for the agent.
    let left = sandbox.root.join("config").join("machine-key.pem");
    std::fs::create_dir_all(left.parent().unwrap()).unwrap();
    std::fs::write(&left, machine_key().to_pem().as_str()).unwrap();
    let before = fake.requests();
    let run = ran(
        agent(),
        &["run", "unit", "--force", "--", "git", "--version"],
    );
    assert!(run.stderr.contains(says), "{}", run.stderr);
    assert_eq!(fake.requests_since(before, "POST"), 0);

    // Another key: the server's 401 machine_unknown gives the same guidance.
    let mut cmd = agent();
    cmd.env("HUSHRUN_MACHINE_KEY", machine_key().to_pem().as_str());
    let run = ran(cmd, &["run", "unit", "--force", "--", "git", "--version"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let last = *hushrun_lines(&run.stderr).last().unwrap();
    assert!(
        last.ends_with(&format!("; not recorded: {says}")),
        "{}",
        run.stderr
    );
    assert!(fake.recorded().is_empty());

    // Its own key records.
    let mut cmd = agent();
    cmd.env("HUSHRUN_MACHINE_KEY", key.to_pem().as_str());
    let run = ran(cmd, &["run", "unit", "--force", "--", "git", "--version"]);
    assert!(
        run.stderr.contains("it won't run again on this code"),
        "{}",
        run.stderr
    );
    assert_eq!(fake.recorded().len(), 1);
}

// covers: C-14, C-18 (a person's results the server refuses for their machine, 401
// machine_required or machine_unknown, say to sign in)
#[test]
fn a_refused_machine_says_to_sign_in() {
    let (server, fake) = Fake::start(false);
    fake.register_machines();
    let r = Repo::new();
    assert_eq!(login(&r, &server).code, 0);
    let says = "this machine isn't registered: run `hushrun login`";
    for code in ["machine_unknown", "machine_required"] {
        fake.answer_next_record(401, code);
        let run = ran(
            r.hushrun(),
            &["run", "unit", "--force", "--", "git", "--version"],
        );
        assert_eq!(run.code, 0, "{}", run.stderr);
        let last = *hushrun_lines(&run.stderr).last().unwrap();
        assert!(
            last.starts_with("hushrun: unit passed in")
                && last.ends_with(&format!("; not recorded: {says}")),
            "{code}: {}",
            run.stderr
        );
    }
    assert!(fake.recorded().is_empty());
}
