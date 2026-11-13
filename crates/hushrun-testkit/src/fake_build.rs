//! A fake of the build protocol the CLI speaks to share build blocks (`{base}/v0/build/…`), for
//! one org at `http://127.0.0.1:<port>/o/<org>`, with an object store of its own beside it
//! (`http://127.0.0.1:<port>/store/<id>`). Every body is read and written with the wire types
//! of `hushrun_core::blocks`, so it can't drift from them.
//!
//! It keeps what a server keeps: the policy `open` answers, blocks by address and scope
//! (`uploading` until a `commit` finds their pack, then `stored`), hints, closes and reports.
//! `begin` answers `put`, `have`, or a refusal as the protocol says; `commit` asks the object
//! store; a lookup serves stored blocks in the scopes the caller reads. A results lookup carries
//! `Hushrun-Build: on` while the org shares, and when it doesn't, every build request answers the
//! 404 a server without them answers.
//!
//! Tests steer it: an answer forced for one address ([`FakeBuild::force_begin`],
//! [`FakeBuild::force_commit`]), a pause or a failure for a route ([`FakeBuild::delay`],
//! [`FakeBuild::fail`]), an object store held to a rate ([`FakeBuild::throttle_store`]), blocks
//! seeded or withdrawn ([`FakeBuild::put_block`], [`FakeBuild::withdraw`]). It counts every
//! request by route and keeps what each carried ([`FakeBuild::seen`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine as _;
use hushrun_core::blocks::{
    self, Base64, BeginRequest, Class, CloseRequest, CommitRequest, Found, FoundHints, Hex32,
    HintsLookupRequest, HintsLookupResponse, HintsRequest, HintsResponse, Limits, Link,
    LookupRequest, LookupResponse, Mode, OpenRequest, Pair, Policy, ReportRequest,
};
use serde_json::{Value, json};

use crate::http::{self, Reply, Request, Response, Server};

/// The routes the fake counts, pauses and fails by.
pub mod route {
    pub const RESULTS: &str = "results";
    pub const MACHINES: &str = "machines";
    pub const OPEN: &str = "open";
    pub const HINTS_LOOKUP: &str = "hints/lookup";
    pub const HINTS: &str = "hints";
    pub const LOOKUP: &str = "blocks/lookup";
    pub const BEGIN: &str = "blocks/begin";
    pub const COMMIT: &str = "blocks/commit";
    pub const CLOSE: &str = "close";
    pub const REPORT: &str = "blocks/report";
    /// `GET /store/<id>`: a download.
    pub const STORE_GET: &str = "store-get";
    /// `PUT /store/<id>`: an upload.
    pub const STORE_PUT: &str = "store-put";
}

/// How long an upload link lives, as the protocol says.
pub const PUT_SECONDS: u32 = 900;
/// How long a download link lives.
pub const GET_SECONDS: u32 = 600;

/// A request the fake received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    /// One of [`route`]'s names, or the path for anything else.
    pub route: String,
    /// The whole path.
    pub path: String,
    /// The `Authorization` header, when there was one.
    pub authorization: Option<String>,
    /// The `Hushrun-Signature` header, when there was one.
    pub signature: Option<String>,
    /// The body, but for the object store's, which is only counted.
    pub body: Vec<u8>,
}

/// Whether a block can be looked up yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockState {
    Uploading,
    Stored,
}

/// One block the fake keeps, in one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub address: Hex32,
    pub scope: String,
    pub hint_address: Option<Hex32>,
    pub bytes: u64,
    pub pack_sha256: Hex32,
    pub outputs: Hex32,
    pub cost_ms: u32,
    pub class: Class,
    pub state: BlockState,
    /// The key id of the machine whose signature its `begin` carried.
    pub machine: Option<String>,
    /// How many upload links it was given.
    pub links: u32,
    /// Whether lookups serve it: [`FakeBuild::withdraw`] stops that.
    pub served: bool,
    /// How many lookups found it.
    pub uses: u32,
    object: String,
    link_expires: Option<Instant>,
}

struct Failure {
    route: String,
    status: u16,
    times: usize,
}

struct Forced {
    address: Hex32,
    answer: Value,
    times: usize,
}

struct State {
    org: String,
    origin: String,
    sharing: bool,
    policy: Policy,
    blocks: Vec<Block>,
    objects: BTreeMap<String, Vec<u8>>,
    hints: BTreeMap<Hex32, Vec<Vec<u8>>>,
    suspended: BTreeSet<Hex32>,
    opens: Vec<OpenRequest>,
    /// The manifests kept, newest last: each close's, and those a test seeded.
    manifests: Vec<Kept>,
    closes: Vec<CloseRequest>,
    reports: Vec<ReportRequest>,
    begins: Vec<BeginRequest>,
    forced_begin: Vec<Forced>,
    forced_commit: Vec<Forced>,
    delays: BTreeMap<String, Duration>,
    failures: Vec<Failure>,
    counts: BTreeMap<String, usize>,
    seen: Vec<Seen>,
    /// The link between the machines and the object store: one rate for all of its traffic.
    store_link: Arc<http::Link>,
    /// Bytes the object store was asked to send, whole or cut off.
    served_bytes: u64,
    /// ID tokens the fake's token endpoint issued, each with its expiry in seconds since the
    /// epoch, and how long a new one lives.
    id_tokens: Vec<(String, u64)>,
    id_token_seconds: u64,
    /// The audiences the token endpoint was asked for.
    audiences: Vec<String>,
    /// Every connection is closed unanswered, as a network that drops it does.
    unreachable: bool,
    /// The paths of the requests dropped so.
    dropped: Vec<String>,
}

/// A manifest the fake keeps, as the backend keeps one per repository, check and platform: from
/// a close, or seeded by a test (with no repository and no platform: any).
#[derive(Debug, Clone)]
struct Kept {
    repository: Option<String>,
    check: Option<Hex32>,
    platform: Option<String>,
    pairs: Vec<Pair>,
}

/// The manifest `open` answers, as the contract says ([`OpenRequest::check`]): for an `open`
/// naming a check, the newest kept for that check of the repository and platform, else none; for
/// one naming none, the newest kept of the repository and platform, whatever its check.
fn manifest_for(kept: &[Kept], open: &OpenRequest) -> Vec<Pair> {
    kept.iter()
        .rev()
        .find(|k| {
            k.repository
                .as_ref()
                .is_none_or(|r| Some(r) == open.repository.as_ref())
                && k.platform.as_ref().is_none_or(|p| *p == open.platform)
                && (open.check.is_none() || k.check == open.check)
        })
        .map(|k| k.pairs.clone())
        .unwrap_or_default()
}

/// A running fake of the build protocol for one org.
pub struct FakeBuild {
    server: Server,
    state: Arc<Mutex<State>>,
}

impl FakeBuild {
    /// A personal token the fake takes. Its `begin`s must be signed.
    pub const TOKEN: &'static str = "hru_fakebuild0123456789abcdefghijABCDEFGH";
    /// An agent token the fake takes. Its `begin`s must be signed too.
    pub const AGENT_TOKEN: &'static str = "hra_fakebuild0123456789abcdefghijABCDEFGH";

    /// A fake for the org `acme`.
    pub fn start() -> FakeBuild {
        FakeBuild::start_for("acme")
    }

    /// A fake for the org `org`, which answers at `/o/<org>` only.
    pub fn start_for(org: &str) -> FakeBuild {
        let store_link = http::Link::new();
        let state = Arc::new(Mutex::new(State {
            org: org.to_string(),
            origin: String::new(),
            sharing: true,
            policy: default_policy(),
            blocks: Vec::new(),
            objects: BTreeMap::new(),
            hints: BTreeMap::new(),
            suspended: BTreeSet::new(),
            opens: Vec::new(),
            manifests: Vec::new(),
            closes: Vec::new(),
            reports: Vec::new(),
            begins: Vec::new(),
            forced_begin: Vec::new(),
            forced_commit: Vec::new(),
            delays: BTreeMap::new(),
            failures: Vec::new(),
            counts: BTreeMap::new(),
            seen: Vec::new(),
            store_link: store_link.clone(),
            served_bytes: 0,
            id_tokens: Vec::new(),
            id_token_seconds: 300,
            audiences: Vec::new(),
            unreachable: false,
            dropped: Vec::new(),
        }));
        let shared = state.clone();
        let link = store_link;
        let server = Server::start_with(
            move |method, path| {
                (method == "PUT" && path.starts_with("/store/") && link.rate() > 0)
                    .then(|| link.clone())
            },
            move |request| handle(&shared, request),
        );
        state.lock().expect("fake build state").origin = server.url();
        FakeBuild { server, state }
    }

    /// What `ACTIONS_ID_TOKEN_REQUEST_TOKEN` must be for the fake's ID token endpoint.
    pub const ID_TOKEN_REQUEST: &'static str = "fake-id-token-request";

    /// The fake's ID token endpoint, as `ACTIONS_ID_TOKEN_REQUEST_URL` names one:
    /// `GET {origin}/id-token?audience=…`, with `ID_TOKEN_REQUEST` as its bearer. It issues a
    /// JWT-shaped token with an `exp`, which the org's routes take until it expires.
    pub fn id_token_url(&self) -> String {
        format!("{}/id-token", self.origin())
    }

    /// How long the ID tokens the endpoint issues from now on live, in seconds.
    pub fn set_id_token_seconds(&self, seconds: u64) {
        self.state().id_token_seconds = seconds;
    }

    /// The ID tokens issued so far, oldest first.
    pub fn id_tokens(&self) -> Vec<String> {
        self.state()
            .id_tokens
            .iter()
            .map(|(t, _)| t.clone())
            .collect()
    }

    /// The audiences ID tokens were asked for.
    pub fn audiences(&self) -> Vec<String> {
        self.state().audiences.clone()
    }

    /// The org's URL: `http://127.0.0.1:<port>/o/<org>`.
    pub fn url(&self) -> String {
        format!("{}/o/{}", self.server.url(), self.state().org)
    }

    /// `http://127.0.0.1:<port>`, where the object store answers too.
    pub fn origin(&self) -> String {
        self.server.url()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake build state")
    }

    // --- the switch and the policy ---------------------------------------------------------

    /// Turns the org's sharing on or off. Off, a results lookup carries no header and every
    /// build request answers 404, as a server without them does.
    pub fn set_sharing(&self, on: bool) {
        self.state().sharing = on;
    }

    /// Changes what `open` answers.
    pub fn set_policy(&self, change: impl FnOnce(&mut Policy)) {
        change(&mut self.state().policy);
    }

    /// What `open` answers now.
    pub fn policy(&self) -> Policy {
        self.state().policy.clone()
    }

    /// The org's shared mode.
    pub fn set_mode(&self, mode: Mode) {
        self.state().policy.mode = Some(mode);
    }

    /// Where the caller writes (`None`: nowhere) and the scopes it reads, in order.
    pub fn set_scopes(&self, write: Option<&str>, read: &[&str]) {
        let mut state = self.state();
        state.policy.write = write.map(str::to_string);
        state.policy.writes = write.is_some();
        state.policy.read = read.iter().map(|s| s.to_string()).collect();
    }

    /// Seeds a manifest kept under no check, for any repository and platform: what an `open`
    /// naming no check answers until a close keeps a newer one. An `open` naming a check gets
    /// only that check's (kept from a close, or [`FakeBuild::set_check_manifest`]).
    pub fn set_manifest(&self, manifest: Vec<Pair>) {
        self.state().manifests.push(Kept {
            repository: None,
            check: None,
            platform: None,
            pairs: manifest,
        });
    }

    /// Seeds the manifest of the check `check`, for any repository and platform.
    pub fn set_check_manifest(&self, check: Hex32, manifest: Vec<Pair>) {
        self.state().manifests.push(Kept {
            repository: None,
            check: Some(check),
            platform: None,
            pairs: manifest,
        });
    }

    // --- what tests steer --------------------------------------------------------------------

    /// Answers `begin` for `address` with `answer` (the members of one answer but the address,
    /// such as `{"answer": "refused", "reason": "busy"}`, or a word no version knows) the next
    /// `times` times it is named, instead of what the protocol says.
    pub fn force_begin(&self, address: Hex32, answer: Value, times: usize) {
        self.state().forced_begin.push(Forced {
            address,
            answer,
            times,
        });
    }

    /// Answers `commit` for `address` with the state `word` (`missing`, `gone`, `differs`, or a
    /// word no version knows) the next `times` times.
    pub fn force_commit(&self, address: Hex32, word: &str, times: usize) {
        self.state().forced_commit.push(Forced {
            address,
            answer: Value::String(word.to_string()),
            times,
        });
    }

    /// Pauses every request of `route` by `pause` before it is answered.
    pub fn delay(&self, route: &str, pause: Duration) {
        self.state().delays.insert(route.to_string(), pause);
    }

    /// Answers the next `times` requests of `route` with `status` (503 with `Retry-After: 1`).
    pub fn fail(&self, route: &str, status: u16, times: usize) {
        self.state().failures.push(Failure {
            route: route.to_string(),
            status,
            times,
        });
    }

    /// Makes the whole server, the object store included, unreachable or reachable again: a
    /// request's connection is closed with no answer, and the request is not recorded among
    /// those [`FakeBuild::seen`] lists, only among [`FakeBuild::dropped`].
    pub fn set_unreachable(&self, unreachable: bool) {
        self.state().unreachable = unreachable;
    }

    /// The paths of the requests an unreachable server dropped, oldest first.
    pub fn dropped(&self) -> Vec<String> {
        self.state().dropped.clone()
    }

    /// Holds the object store's downloads and uploads, all together, to `bytes_per_second` (0:
    /// no limit): one link for every connection, so eight downloads at once share the rate.
    pub fn throttle_store(&self, bytes_per_second: u64) {
        self.state().store_link.set_rate(bytes_per_second);
    }

    /// Makes every upload link given so far expire now.
    pub fn expire_links(&self) {
        for block in &mut self.state().blocks {
            block.link_expires = Some(Instant::now());
        }
    }

    /// Lists `address` as suspended in the repository: lookups name it and serve it not, and
    /// `begin` refuses it.
    pub fn suspend(&self, address: Hex32) {
        self.state().suspended.insert(address);
    }

    /// A stored block in `scope`, as if a machine had uploaded `pack`: its size and SHA-256
    /// computed, served from now on.
    pub fn put_block(
        &self,
        address: Hex32,
        scope: &str,
        pack: &[u8],
        outputs: Hex32,
        cost_ms: u32,
        class: Class,
    ) {
        let mut state = self.state();
        let object = object_id(scope, &address);
        state.objects.insert(object.clone(), pack.to_vec());
        state
            .blocks
            .retain(|b| !(b.address == address && b.scope == scope));
        state.blocks.push(Block {
            address,
            scope: scope.to_string(),
            hint_address: None,
            bytes: pack.len() as u64,
            pack_sha256: Hex32::sha256(pack),
            outputs,
            cost_ms,
            class,
            state: BlockState::Stored,
            machine: None,
            links: 0,
            served: true,
            uses: 0,
            object,
            link_expires: None,
        });
    }

    /// Stops serving `address` in every scope: its writer's authority lapsed, or it was
    /// deleted after a disagreement.
    pub fn withdraw(&self, address: Hex32) {
        for block in &mut self.state().blocks {
            if block.address == address {
                block.served = false;
            }
        }
    }

    /// Says of every block it holds that its compile took `cost_ms` where it was built.
    pub fn set_costs(&self, cost_ms: u32) {
        for block in &mut self.state().blocks {
            block.cost_ms = cost_ms;
        }
    }

    /// Forgets `address` in every scope, its objects too: evicted.
    pub fn evict(&self, address: Hex32) {
        let mut state = self.state();
        let gone: Vec<String> = state
            .blocks
            .iter()
            .filter(|b| b.address == address)
            .map(|b| b.object.clone())
            .collect();
        state.blocks.retain(|b| b.address != address);
        for object in gone {
            state.objects.remove(&object);
        }
    }

    /// A hint for the first key's address `address`, as if a machine had sent it.
    pub fn seed_hint(&self, address: Hex32, list: &[u8]) {
        keep_hint(&mut self.state(), address, list.to_vec());
    }

    // --- what tests read ---------------------------------------------------------------------

    /// Every block, in every scope and state.
    pub fn blocks(&self) -> Vec<Block> {
        self.state().blocks.clone()
    }

    /// The stored blocks.
    pub fn stored(&self) -> Vec<Block> {
        self.blocks()
            .into_iter()
            .filter(|b| b.state == BlockState::Stored)
            .collect()
    }

    /// The block at `address`, the first scope that has it.
    pub fn block(&self, address: Hex32) -> Option<Block> {
        self.state()
            .blocks
            .iter()
            .find(|b| b.address == address)
            .cloned()
    }

    /// The bytes the object store holds for `address`, the first scope that has them.
    pub fn object(&self, address: Hex32) -> Option<Vec<u8>> {
        let state = self.state();
        let block = state.blocks.iter().find(|b| b.address == address)?;
        state.objects.get(&block.object).cloned()
    }

    /// The hints kept for `address`, newest first.
    pub fn hints(&self, address: Hex32) -> Vec<Vec<u8>> {
        self.state()
            .hints
            .get(&address)
            .cloned()
            .unwrap_or_default()
    }

    pub fn opens(&self) -> Vec<OpenRequest> {
        self.state().opens.clone()
    }

    pub fn closes(&self) -> Vec<CloseRequest> {
        self.state().closes.clone()
    }

    pub fn reports(&self) -> Vec<ReportRequest> {
        self.state().reports.clone()
    }

    pub fn begins(&self) -> Vec<BeginRequest> {
        self.state().begins.clone()
    }

    /// How many requests of `route` came.
    pub fn count(&self, route: &str) -> usize {
        self.state().counts.get(route).copied().unwrap_or(0)
    }

    /// Every request, in order.
    pub fn seen(&self) -> Vec<Seen> {
        self.state().seen.clone()
    }

    /// Bytes the object store was asked to send, whole or cut off.
    pub fn served_bytes(&self) -> u64 {
        self.state().served_bytes
    }
}

/// What `open` answers until a test changes it: sharing, mode `on`, a person's own scope
/// written and read.
fn default_policy() -> Policy {
    Policy {
        sharing: true,
        reason: None,
        mode: Some(Mode::On),
        writes: true,
        untrusted: None,
        write: Some("person/1".to_string()),
        read: vec!["person/1".to_string()],
        kept_days: 30,
        limits: Limits::default(),
        allowance_bytes: 10_000_000_000,
        held_bytes: 0,
        manifest: Vec::new(),
    }
}

/// An object's name in the store.
fn object_id(scope: &str, address: &Hex32) -> String {
    let scope = Hex32::sha256(scope.as_bytes()).to_string();
    format!("{}-{address}", &scope[..16])
}

fn keep_hint(state: &mut State, address: Hex32, list: Vec<u8>) {
    let lists = state.hints.entry(address).or_default();
    lists.retain(|l| *l != list);
    lists.insert(0, list);
    lists.truncate(blocks::MOST_HINTS_PER_ADDRESS);
}

fn error(status: u16, code: &str, message: &str) -> Response {
    Response::json(
        status,
        &json!({"error": {"code": code, "message": message}}),
    )
}

fn answer(response: Response, pause: Option<Duration>) -> Reply {
    match pause {
        Some(pause) => Reply::Later(pause, response),
        None => Reply::Send(response),
    }
}

/// The route of a path below the org's URL, `v0/build/open` or `v0/results/<key>`.
fn route_of(below: &str) -> Option<&'static str> {
    Some(match below {
        "v0/build/open" => route::OPEN,
        "v0/build/hints/lookup" => route::HINTS_LOOKUP,
        "v0/build/hints" => route::HINTS,
        "v0/build/blocks/lookup" => route::LOOKUP,
        "v0/build/blocks/begin" => route::BEGIN,
        "v0/build/blocks/commit" => route::COMMIT,
        "v0/build/close" => route::CLOSE,
        "v0/build/blocks/report" => route::REPORT,
        "v0/machines" => route::MACHINES,
        _ if below.starts_with("v0/results/") => route::RESULTS,
        _ => return None,
    })
}

fn handle(shared: &Mutex<State>, request: Request) -> Reply {
    let mut state = shared.lock().expect("fake build state");
    if state.unreachable {
        state.dropped.push(request.path.clone());
        return Reply::Close;
    }
    let store_object = request.path.strip_prefix("/store/").map(str::to_string);
    let prefix = format!("/o/{}/", state.org);
    let route: String = match (&store_object, request.path.strip_prefix(&prefix)) {
        (Some(_), _) if request.method == "PUT" => route::STORE_PUT.to_string(),
        (Some(_), _) => route::STORE_GET.to_string(),
        (None, Some(below)) => route_of(below)
            .map(str::to_string)
            .unwrap_or_else(|| request.path.clone()),
        (None, None) => request.path.clone(),
    };
    *state.counts.entry(route.clone()).or_default() += 1;
    state.seen.push(Seen {
        method: request.method.clone(),
        route: route.clone(),
        path: request.path.clone(),
        authorization: request.header("authorization").map(str::to_string),
        signature: request.header("hushrun-signature").map(str::to_string),
        body: if store_object.is_some() {
            Vec::new()
        } else {
            request.body.clone()
        },
    });
    let pause = state.delays.get(&route).copied();
    if let Some(failure) = state
        .failures
        .iter_mut()
        .find(|f| f.route == route && f.times > 0)
    {
        failure.times -= 1;
        let mut response = error(failure.status, "injected", "a test asked for this");
        if failure.status == 503 {
            response = error(503, "unavailable", "try again").header("retry-after", "1");
        }
        return answer(response, pause);
    }
    if let Some(object) = store_object {
        return store(&mut state, &request, &object, pause);
    }
    if request.path == "/id-token" {
        return answer(id_token(&mut state, &request), pause);
    }
    if request.path.strip_prefix(&prefix).is_none() {
        return answer(error(404, "not_found", "no such endpoint"), pause);
    }
    // Who calls: the token, else 401, as every org request.
    let token = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let id_token = token.is_some_and(|token| {
        state
            .id_tokens
            .iter()
            .any(|(issued, expires)| issued == token && *expires > now)
    });
    if !id_token && !matches!(token, Some(FakeBuild::TOKEN | FakeBuild::AGENT_TOKEN)) {
        return answer(
            error(401, "unauthorized", "a token this server takes is needed"),
            pause,
        );
    }
    let response = match route.as_str() {
        route::RESULTS => {
            let response = Response::json(200, &json!({"result": null}));
            if state.sharing {
                response.header("Hushrun-Build", "on")
            } else {
                response
            }
        }
        route::MACHINES => Response::json(201, &json!({})),
        _ if !state.sharing => error(404, "not_found", "no such endpoint"),
        _ if request.method != "POST" => error(405, "method_not_allowed", "POST only"),
        route::OPEN => match serde_json::from_slice::<OpenRequest>(&request.body) {
            Ok(open) => {
                let mut policy = state.policy.clone();
                policy.manifest = manifest_for(&state.manifests, &open);
                state.opens.push(open);
                Response::json(200, &serde_json::to_value(&policy).expect("json"))
            }
            Err(err) => invalid(err),
        },
        route::HINTS_LOOKUP => match serde_json::from_slice::<HintsLookupRequest>(&request.body) {
            Ok(ask) if ask.addresses.len() > blocks::MOST_LOOKUP => too_many(),
            Ok(ask) => {
                let hints = ask
                    .addresses
                    .iter()
                    .filter_map(|address| {
                        let lists = state.hints.get(address)?;
                        Some(FoundHints {
                            address: *address,
                            lists: lists.iter().cloned().map(Base64).collect(),
                        })
                    })
                    .collect();
                to_json(&HintsLookupResponse { hints })
            }
            Err(err) => invalid(err),
        },
        route::HINTS => match serde_json::from_slice::<HintsRequest>(&request.body) {
            Ok(sent) if sent.hints.len() > blocks::MOST_HINTS_SENT => too_many(),
            // Hints go into the caller's own scope: a caller that writes nowhere keeps none.
            Ok(_) if !state.policy.writes => to_json(&HintsResponse { stored: 0 }),
            Ok(sent) => {
                let stored = sent.hints.len() as u32;
                for hint in sent.hints {
                    keep_hint(&mut state, hint.address, hint.list.0);
                }
                to_json(&HintsResponse { stored })
            }
            Err(err) => invalid(err),
        },
        route::LOOKUP => match serde_json::from_slice::<LookupRequest>(&request.body) {
            Ok(ask) if ask.addresses.len() > blocks::MOST_LOOKUP => too_many(),
            Ok(ask) => lookup(&mut state, &ask),
            Err(err) => invalid(err),
        },
        route::BEGIN => match serde_json::from_slice::<BeginRequest>(&request.body) {
            Ok(ask) if ask.blocks.len() > blocks::MOST_BEGIN => too_many(),
            Ok(ask) => match request.header("hushrun-signature") {
                // CI's ID tokens never sign.
                None if id_token => begin(&mut state, ask, None),
                None => error(401, "machine_required", "a machine's signature is needed"),
                Some(signature) => {
                    let machine = signature.split_whitespace().next().map(str::to_string);
                    begin(&mut state, ask, machine)
                }
            },
            Err(err) => invalid(err),
        },
        route::COMMIT => match serde_json::from_slice::<CommitRequest>(&request.body) {
            Ok(ask) if ask.addresses.len() > blocks::MOST_BEGIN => too_many(),
            Ok(ask) => commit(&mut state, &ask),
            Err(err) => invalid(err),
        },
        route::CLOSE => match serde_json::from_slice::<CloseRequest>(&request.body) {
            Ok(close) if close.manifest.len() > blocks::MOST_MANIFEST => too_many(),
            Ok(close) => {
                // Sent again with the same run, it counts once.
                if !state.closes.iter().any(|c| c.run == close.run) {
                    // The manifest is kept for the repository, check and platform, unless the
                    // one kept has more than twice its entries (the contract's 7 days aside).
                    let kept = state.manifests.iter().rposition(|k| {
                        k.repository == close.repository
                            && k.check == close.check
                            && k.platform.as_deref() == Some(close.platform.as_str())
                    });
                    let smaller = kept
                        .is_some_and(|i| close.manifest.len() * 2 < state.manifests[i].pairs.len());
                    if !smaller && !close.manifest.is_empty() {
                        if let Some(i) = kept {
                            state.manifests.remove(i);
                        }
                        state.manifests.push(Kept {
                            repository: close.repository.clone(),
                            check: close.check,
                            platform: Some(close.platform.clone()),
                            pairs: close.manifest.clone(),
                        });
                    }
                    state.closes.push(close);
                }
                Response::json(200, &json!({}))
            }
            Err(err) => invalid(err),
        },
        route::REPORT => match serde_json::from_slice::<ReportRequest>(&request.body) {
            Ok(report) if !state.policy.read.contains(&report.scope) => {
                error(403, "forbidden", "a scope the caller doesn't read")
            }
            Ok(report) => {
                state.reports.push(report);
                Response::json(200, &json!({}))
            }
            Err(err) => invalid(err),
        },
        _ => error(404, "not_found", "no such endpoint"),
    };
    answer(response, pause)
}

fn to_json<T: serde::Serialize>(value: &T) -> Response {
    Response::json(200, &serde_json::to_value(value).expect("json"))
}

fn invalid(err: serde_json::Error) -> Response {
    error(400, "invalid", &err.to_string())
}

fn too_many() -> Response {
    error(400, "invalid", "too many in one request")
}

/// The object store: `PUT` keeps the bytes unless some are there (412), `GET` serves them.
fn store(state: &mut State, request: &Request, object: &str, pause: Option<Duration>) -> Reply {
    match request.method.as_str() {
        "PUT" => {
            // A link works for as long as it said: a PUT on one that expired is refused.
            let expired = state.blocks.iter().any(|b| {
                b.object == object && b.link_expires.is_some_and(|until| until <= Instant::now())
            });
            if expired {
                return answer(Response::new(403), pause);
            }
            if state.objects.contains_key(object) {
                return answer(Response::new(412), pause);
            }
            state
                .objects
                .insert(object.to_string(), request.body.clone());
            answer(Response::new(200), pause)
        }
        "GET" => match state.objects.get(object) {
            Some(bytes) => {
                state.served_bytes += bytes.len() as u64;
                let response = Response::bytes(200, bytes.clone());
                match state.store_link.rate() {
                    0 => answer(response, pause),
                    // Held to a rate, it answers at once: the link is the delay.
                    _ => Reply::Slow(state.store_link.clone(), response),
                }
            }
            None => answer(Response::new(404), pause),
        },
        _ => answer(Response::new(405), pause),
    }
}

fn link(origin: &str, method: &str, object: &str, seconds: u32) -> Link {
    Link {
        method: method.to_string(),
        url: format!("{origin}/store/{object}"),
        headers: BTreeMap::new(),
        expires_in_seconds: seconds,
    }
}

fn lookup(state: &mut State, ask: &LookupRequest) -> Response {
    let mut found = Vec::new();
    let mut suspended = Vec::new();
    let read = state.policy.read.clone();
    let origin = state.origin.clone();
    for address in &ask.addresses {
        let best = read.iter().find_map(|scope| {
            state.blocks.iter().position(|b| {
                b.address == *address
                    && b.scope == *scope
                    && b.served
                    && b.state == BlockState::Stored
            })
        });
        let Some(index) = best else { continue };
        if state.suspended.contains(address) {
            suspended.push(*address);
            continue;
        }
        let block = &mut state.blocks[index];
        block.uses += 1;
        found.push(Found {
            address: *address,
            scope: block.scope.clone(),
            bytes: block.bytes,
            pack_sha256: block.pack_sha256,
            outputs: block.outputs,
            cost_ms: block.cost_ms,
            class: block.class,
            mine: ask.machine.is_some() && ask.machine == block.machine,
            get: ask
                .fetch
                .then(|| link(&origin, "GET", &block.object, GET_SECONDS)),
        });
    }
    to_json(&LookupResponse { found, suspended })
}

fn take_forced(forced: &mut [Forced], address: &Hex32) -> Option<Value> {
    let entry = forced
        .iter_mut()
        .find(|f| f.address == *address && f.times > 0)?;
    entry.times -= 1;
    Some(entry.answer.clone())
}

fn begin(state: &mut State, ask: BeginRequest, machine: Option<String>) -> Response {
    state.begins.push(ask.clone());
    let origin = state.origin.clone();
    let write = state.policy.write.clone();
    let read = state.policy.read.clone();
    let mut answers = Vec::new();
    for sent in &ask.blocks {
        let refused = |reason: &str| json!({"answer": "refused", "reason": reason});
        let answer = if let Some(forced) = take_forced(&mut state.forced_begin, &sent.address) {
            forced
        } else if let Some(write) = write.as_deref().filter(|_| state.policy.writes) {
            let held = read.iter().find_map(|scope| {
                state.blocks.iter().find(|b| {
                    b.address == sent.address
                        && b.scope == *scope
                        && b.served
                        && b.state == BlockState::Stored
                })
            });
            if sent.bytes > blocks::MOST_PACK_BYTES {
                refused("too_large")
            } else if sent.class == Class::Unknown {
                refused("unsupported")
            } else if state.suspended.contains(&sent.address)
                || sent
                    .hint_address
                    .is_some_and(|hint| state.suspended.contains(&hint))
            {
                refused("suspended")
            } else if let Some(held) = held {
                if held.outputs == sent.outputs {
                    json!({"answer": "have", "scope": held.scope})
                } else {
                    refused("differs")
                }
            } else {
                let now = Instant::now();
                let row = state
                    .blocks
                    .iter_mut()
                    .find(|b| b.address == sent.address && b.scope == write);
                match row {
                    Some(row) if row.state == BlockState::Stored => {
                        json!({"answer": "have", "scope": write})
                    }
                    Some(row) => {
                        let same = row.bytes == sent.bytes && row.pack_sha256 == sent.pack_sha256;
                        let live = row.link_expires.is_some_and(|until| until > now);
                        if same || !live {
                            // The same upload retried gets a new link; another pack takes the
                            // row over once the last link has expired.
                            row.bytes = sent.bytes;
                            row.pack_sha256 = sent.pack_sha256;
                            row.outputs = sent.outputs;
                            row.cost_ms = sent.cost_ms;
                            row.class = sent.class;
                            row.machine = machine.clone();
                            row.links += 1;
                            row.link_expires =
                                Some(now + Duration::from_secs(u64::from(PUT_SECONDS)));
                            put_answer(&origin, row)
                        } else {
                            refused("busy")
                        }
                    }
                    None => {
                        let mut row = Block {
                            address: sent.address,
                            scope: write.to_string(),
                            hint_address: sent.hint_address,
                            bytes: sent.bytes,
                            pack_sha256: sent.pack_sha256,
                            outputs: sent.outputs,
                            cost_ms: sent.cost_ms,
                            class: sent.class,
                            state: BlockState::Uploading,
                            machine: machine.clone(),
                            links: 1,
                            served: true,
                            uses: 0,
                            object: object_id(write, &sent.address),
                            link_expires: Some(now + Duration::from_secs(u64::from(PUT_SECONDS))),
                        };
                        let answer = put_answer(&origin, &mut row);
                        state.blocks.push(row);
                        answer
                    }
                }
            }
        } else {
            refused("no_scope")
        };
        let mut entry = json!({"address": sent.address});
        if let (Some(entry), Some(members)) = (entry.as_object_mut(), answer.as_object()) {
            entry.extend(members.clone());
        }
        answers.push(entry);
    }
    Response::json(200, &json!({"blocks": answers}))
}

fn put_answer(origin: &str, row: &mut Block) -> Value {
    let mut put = link(origin, "PUT", &row.object, PUT_SECONDS);
    put.headers
        .insert("content-length".to_string(), row.bytes.to_string());
    put.headers
        .insert("if-none-match".to_string(), "*".to_string());
    put.headers.insert(
        "x-amz-checksum-sha256".to_string(),
        base64::engine::general_purpose::STANDARD.encode(row.pack_sha256.0),
    );
    json!({"answer": "put", "link": put})
}

fn commit(state: &mut State, ask: &CommitRequest) -> Response {
    let write = state.policy.write.clone().unwrap_or_default();
    let mut answers = Vec::new();
    for address in &ask.addresses {
        let word = if let Some(forced) = take_forced(&mut state.forced_commit, address) {
            forced
        } else {
            let objects = &state.objects;
            match state
                .blocks
                .iter_mut()
                .find(|b| b.address == *address && b.scope == write)
            {
                None => json!("gone"),
                Some(row) if row.state == BlockState::Stored => json!("stored"),
                Some(row) => match objects.get(&row.object) {
                    None => json!("missing"),
                    Some(bytes)
                        if bytes.len() as u64 == row.bytes
                            && Hex32::sha256(bytes) == row.pack_sha256 =>
                    {
                        row.state = BlockState::Stored;
                        json!("stored")
                    }
                    Some(_) => json!("differs"),
                },
            }
        };
        answers.push(json!({"address": address, "state": word}));
    }
    Response::json(200, &json!({"blocks": answers}))
}

/// `GET /id-token?audience=…`: a JWT-shaped token whose payload holds its `exp`.
fn id_token(state: &mut State, request: &Request) -> Response {
    let bearer = format!("Bearer {}", FakeBuild::ID_TOKEN_REQUEST);
    if request.header("authorization") != Some(bearer.as_str()) {
        return error(401, "unauthorized", "the request token is needed");
    }
    let audience = request.query("audience").unwrap_or_default().to_string();
    state.audiences.push(audience);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let exp = now + state.id_token_seconds;
    let n = state.id_tokens.len();
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(json!({"exp": exp, "n": n}).to_string());
    let token = format!("eyJhbGciOiJub25lIn0.{payload}.c2ln");
    state.id_tokens.push((token.clone(), exp));
    Response::json(200, &json!({"count": 1, "value": token}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hushrun_core::blocks::{Answer, BeginBlock, BeginResponse, CommitResponse, CommitState};
    use std::io::{Read, Write};
    use std::net::TcpStream;

    /// One request to the fake: the status, the headers' text and the body.
    fn call(
        fake: &FakeBuild,
        method: &str,
        url: &str,
        body: &[u8],
        extra: &[(&str, &str)],
    ) -> (u16, String, Vec<u8>) {
        let path = url.strip_prefix(&fake.origin()).expect("the fake's URL");
        let mut stream = TcpStream::connect(fake.server.addr()).unwrap();
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nhost: h\r\ncontent-length: {}\r\n",
            body.len()
        );
        for (name, value) in extra {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).unwrap();
        let split = answer
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a head");
        let head = String::from_utf8_lossy(&answer[..split]).into_owned();
        let status = head[9..12].parse().unwrap();
        (status, head, answer[split + 4..].to_vec())
    }

    fn post(fake: &FakeBuild, route: &str, body: &Value) -> (u16, Value) {
        let bearer = format!("Bearer {}", FakeBuild::TOKEN);
        let (status, _, body) = call(
            fake,
            "POST",
            &format!("{}/v0/build/{route}", fake.url()),
            &serde_json::to_vec(body).unwrap(),
            &[
                ("authorization", &bearer),
                ("hushrun-signature", "sha256:abc c2ln"),
            ],
        );
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    fn address(n: u8) -> Hex32 {
        Hex32([n; 32])
    }

    // covers: B-30, B-36
    #[test]
    fn open_answers_the_named_checks_manifest_or_the_repositorys_newest() {
        let fake = FakeBuild::start();
        let close = |check: Option<Hex32>, repository: &str, pairs: Vec<Pair>| {
            let mut run = [0u8; 32];
            run[0] = fake.closes().len() as u8;
            let body = json!({
                "kind": "rustc/1",
                "run": Hex32(run),
                "repository": repository,
                "check": check,
                "platform": "aarch64-apple-darwin",
                "manifest": pairs,
            });
            assert_eq!(post(&fake, "close", &body).0, 200);
        };
        let open = |check: Option<Hex32>, repository: &str| {
            let body = json!({
                "kind": "rustc/1",
                "repository": repository,
                "check": check,
                "platform": "aarch64-apple-darwin",
            });
            let (status, policy) = post(&fake, "open", &body);
            assert_eq!(status, 200);
            serde_json::from_value::<Policy>(policy).unwrap().manifest
        };
        let shop = "github.com/acme/shop";
        let (build, test) = (address(1), address(2));
        let one = vec![Pair(address(10), address(11))];
        let two = vec![Pair(address(20), address(21))];
        close(Some(build), shop, one.clone());
        close(Some(test), shop, two.clone());
        // A check gets its own manifest, never another check's.
        assert_eq!(open(Some(build), shop), one);
        assert_eq!(open(Some(test), shop), two);
        assert_eq!(open(Some(address(3)), shop), Vec::new());
        // No check: the repository's newest.
        assert_eq!(open(None, shop), two);
        // Another repository's: none.
        assert_eq!(open(None, "github.com/acme/other"), Vec::new());
        assert_eq!(open(Some(build), "github.com/acme/other"), Vec::new());
        // A seeded manifest under no check answers only an `open` naming none.
        fake.set_manifest(one.clone());
        assert_eq!(open(None, "github.com/acme/other"), one);
        assert_eq!(open(Some(address(3)), shop), Vec::new());
    }

    #[test]
    fn open_and_the_header_follow_the_switch() {
        let fake = FakeBuild::start();
        let bearer = format!("Bearer {}", FakeBuild::TOKEN);
        let results = format!("{}/v0/results/{}", fake.url(), address(1));
        let (status, head, _) = call(&fake, "GET", &results, b"", &[("authorization", &bearer)]);
        assert_eq!(status, 200);
        assert!(
            head.to_ascii_lowercase().contains("hushrun-build: on"),
            "{head}"
        );
        let open = json!({"kind": "rustc/1", "platform": "aarch64-apple-darwin"});
        let (status, policy) = post(&fake, "open", &open);
        assert_eq!(status, 200);
        let policy: Policy = serde_json::from_value(policy).unwrap();
        assert!(policy.sharing && policy.mode == Some(Mode::On));
        assert_eq!(fake.opens().len(), 1);

        fake.set_sharing(false);
        let (_, head, _) = call(&fake, "GET", &results, b"", &[("authorization", &bearer)]);
        assert!(
            !head.to_ascii_lowercase().contains("hushrun-build"),
            "{head}"
        );
        let (status, body) = post(&fake, "open", &open);
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "not_found");
        // Without the token: 401.
        let (status, _, _) = call(
            &fake,
            "POST",
            &format!("{}/v0/build/open", fake.url()),
            b"{}",
            &[],
        );
        assert_eq!(status, 401);
    }

    #[test]
    fn a_block_goes_up_and_is_served() {
        let fake = FakeBuild::start();
        let pack = b"a sealed pack".to_vec();
        let block = BeginBlock {
            address: address(2),
            hint_address: Some(address(3)),
            bytes: pack.len() as u64,
            pack_sha256: Hex32::sha256(&pack),
            outputs: address(4),
            class: Class::Library,
            cost_ms: 1200,
        };
        let ask = json!({"kind": "rustc/1", "blocks": [block]});
        let (status, answer) = post(&fake, "blocks/begin", &ask);
        assert_eq!(status, 200, "{answer}");
        let answer: BeginResponse = serde_json::from_value(answer).unwrap();
        let Answer::Put { link } = &answer.blocks[0].answer else {
            panic!("{answer:?}");
        };
        assert_eq!(link.headers["if-none-match"], "*");
        let (status, _, _) = call(&fake, "PUT", &link.url, &pack, &[]);
        assert_eq!(status, 200);
        let (status, _, _) = call(&fake, "PUT", &link.url, &pack, &[]);
        assert_eq!(status, 412, "already there");
        let commit = json!({"kind": "rustc/1", "addresses": [address(2)]});
        let (_, answer) = post(&fake, "blocks/commit", &commit);
        let answer: CommitResponse = serde_json::from_value(answer).unwrap();
        assert_eq!(answer.blocks[0].state, CommitState::Stored);
        assert_eq!(
            fake.block(address(2)).unwrap().machine.as_deref(),
            Some("sha256:abc")
        );

        let lookup = json!({"kind": "rustc/1", "addresses": [address(2), address(9)], "fetch": true, "machine": "sha256:abc"});
        let (_, answer) = post(&fake, "blocks/lookup", &lookup);
        let answer: LookupResponse = serde_json::from_value(answer).unwrap();
        assert_eq!(answer.found.len(), 1);
        let found = &answer.found[0];
        assert!(found.mine && found.outputs == address(4));
        let (status, _, bytes) = call(&fake, "GET", &found.get.as_ref().unwrap().url, b"", &[]);
        assert_eq!((status, bytes), (200, pack));

        // Again: `have`. Withdrawn: not served, and `begin` makes a new upload of it.
        let (_, answer) = post(&fake, "blocks/begin", &ask);
        assert_eq!(answer["blocks"][0]["answer"], "have");
        fake.withdraw(address(2));
        let (_, answer) = post(&fake, "blocks/lookup", &lookup);
        assert_eq!(answer["found"], json!([]));
        // Unsigned: refused.
        let bearer = format!("Bearer {}", FakeBuild::TOKEN);
        let (status, _, body) = call(
            &fake,
            "POST",
            &format!("{}/v0/build/blocks/begin", fake.url()),
            &serde_json::to_vec(&ask).unwrap(),
            &[("authorization", &bearer)],
        );
        assert_eq!(status, 401, "{}", String::from_utf8_lossy(&body));
    }

    #[test]
    fn forced_answers_come_back_as_given() {
        let fake = FakeBuild::start();
        fake.force_begin(address(5), json!({"answer": "later"}), 1);
        fake.force_begin(
            address(6),
            json!({"answer": "refused", "reason": "new_word"}),
            1,
        );
        fake.force_commit(address(5), "pending", 1);
        let block = |n| BeginBlock {
            address: address(n),
            hint_address: None,
            bytes: 10,
            pack_sha256: address(7),
            outputs: address(8),
            class: Class::Library,
            cost_ms: 1,
        };
        let ask = json!({"kind": "rustc/1", "blocks": [block(5), block(6)]});
        let (_, answer) = post(&fake, "blocks/begin", &ask);
        let answer: BeginResponse = serde_json::from_value(answer).unwrap();
        assert_eq!(answer.blocks[0].answer, Answer::Unknown);
        assert_eq!(
            answer.blocks[1].answer,
            Answer::Refused {
                reason: blocks::Refusal::Unknown
            }
        );
        let (_, answer) = post(
            &fake,
            "blocks/commit",
            &json!({"kind": "rustc/1", "addresses": [address(5)]}),
        );
        let answer: CommitResponse = serde_json::from_value(answer).unwrap();
        assert_eq!(answer.blocks[0].state, CommitState::Unknown);
        // Used up: the protocol's answer again.
        let (_, answer) = post(&fake, "blocks/begin", &ask);
        assert_eq!(answer["blocks"][0]["answer"], "put");

        fake.fail(route::OPEN, 503, 1);
        let open = json!({"kind": "rustc/1", "platform": "x"});
        assert_eq!(post(&fake, "open", &open).0, 503);
        assert_eq!(post(&fake, "open", &open).0, 200);
        assert_eq!(fake.count(route::OPEN), 2);
    }

    #[test]
    fn a_throttled_store_sends_no_faster_than_its_rate() {
        let fake = FakeBuild::start();
        let pack = vec![7u8; 64 * 1024];
        fake.put_block(
            address(1),
            "person/1",
            &pack,
            address(2),
            10,
            Class::Library,
        );
        let lookup = json!({"kind": "rustc/1", "addresses": [address(1)], "fetch": true});
        let (_, answer) = post(&fake, "blocks/lookup", &lookup);
        let url = answer["found"][0]["get"]["url"]
            .as_str()
            .unwrap()
            .to_string();

        let started = Instant::now();
        let (_, _, bytes) = call(&fake, "GET", &url, b"", &[]);
        let fast = started.elapsed();
        assert_eq!(bytes.len(), pack.len());

        // 256 KiB a second: 64 KiB take at least a quarter of a second.
        fake.throttle_store(256 * 1024);
        let started = Instant::now();
        let (_, _, bytes) = call(&fake, "GET", &url, b"", &[]);
        let slow = started.elapsed();
        assert_eq!(bytes.len(), pack.len());
        assert!(slow >= Duration::from_millis(200), "{slow:?}");
        assert!(slow > fast, "{slow:?} {fast:?}");
        assert_eq!(fake.served_bytes(), 2 * pack.len() as u64);

        // Uploads too.
        let block = BeginBlock {
            address: address(3),
            hint_address: None,
            bytes: pack.len() as u64,
            pack_sha256: Hex32::sha256(&pack),
            outputs: address(4),
            class: Class::Library,
            cost_ms: 1,
        };
        let (_, answer) = post(
            &fake,
            "blocks/begin",
            &json!({"kind": "rustc/1", "blocks": [block]}),
        );
        let put = answer["blocks"][0]["link"]["url"]
            .as_str()
            .unwrap()
            .to_string();
        let started = Instant::now();
        let (status, _, _) = call(&fake, "PUT", &put, &pack, &[]);
        assert_eq!(status, 200);
        assert!(started.elapsed() >= Duration::from_millis(200));
        // The hushrun token never goes to the object store.
        assert!(
            fake.seen()
                .iter()
                .filter(|s| s.route.starts_with("store"))
                .all(|s| s.authorization.is_none())
        );
    }

    #[test]
    fn a_throttled_store_is_one_link_that_downloads_at_once_share() {
        // Per connection, four downloads at once would each get the whole rate and end in the
        // time one takes. On one link they share it: together they take four times as long.
        let fake = FakeBuild::start();
        let pack = vec![7u8; 64 * 1024];
        let urls: Vec<String> = (1..=4)
            .map(|n| {
                fake.put_block(
                    address(n),
                    "person/1",
                    &pack,
                    address(n + 10),
                    10,
                    Class::Library,
                );
                let lookup = json!({"kind": "rustc/1", "addresses": [address(n)], "fetch": true});
                let (_, answer) = post(&fake, "blocks/lookup", &lookup);
                answer["found"][0]["get"]["url"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        // 512 KiB a second: one pack alone takes an eighth of a second, four take half of one.
        fake.throttle_store(512 * 1024);
        let started = Instant::now();
        let pack = &pack;
        std::thread::scope(|scope| {
            for url in &urls {
                let fake = &fake;
                scope.spawn(move || {
                    let (_, _, bytes) = call(fake, "GET", url, b"", &[]);
                    assert_eq!(bytes.len(), pack.len());
                });
            }
        });
        let together = started.elapsed();
        assert!(together >= Duration::from_millis(450), "{together:?}");
    }
}
