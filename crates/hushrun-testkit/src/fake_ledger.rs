//! A fake HTTP ledger: the protocol `hushrun-ledger-http/1` (spec L-10) over loopback http,
//! with roles, the key shapes of L-2, and faults to inject. The HTTP backend's tests and the
//! CLI's tests run against it. It implements the contract, not the hosted ledger's storage.
//!
//! \[M18\] It keeps a transparency log (L-11) as the hosted ledger does: a leaf for every write,
//! a checkpoint after each, receipts, signed listings and the log's endpoints. And it can lie as
//! a dishonest ledger would: leave keys out of listings while signing for them, show some tokens
//! another history, or send receipts that prove nothing.
//!
//! \[M19\] With [`FakeLedger::set_maps`], each write is a batch that ends in a map leaf, and
//! listings of map prefixes carry their proofs (L-12). Its lies then include a map that leaves
//! keys out.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};

use hushrun_core::digest::Digest;
use hushrun_ledger::layout::{parse_deployed_key, parse_event_key, parse_policy_key};
use hushrun_ledger::log::wire::ListingProofWire;
use hushrun_ledger::ulid_millis;
use hushrun_tlog::map::{ListingProof, MapBuilder, PageProof, is_map_prefix};
use hushrun_tlog::{
    Checkpoint, Entry, Hash, Listing, MemTree, SignedCheckpoint, SignedListing, Signer,
};

use crate::http::{Reply, Request, Response, Server};

/// The largest object body (L-10).
const MAX_BODY: usize = 1 << 20;
/// The largest listing page and batch-get.
const MAX_PAGE: usize = 1000;
const MAX_BATCH: usize = 100;

/// What a token may do (L-10 roles).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Reader,
    Writer,
    Admin,
}

/// Something to do instead of, or besides, answering a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Answer this status with an error body, and `Retry-After` when given.
    Status(u16, Option<String>),
    /// Close the connection before doing anything.
    Drop,
    /// Do the request, then close the connection without answering: a lost answer.
    DropAfter,
    /// Do the request, then answer after a pause.
    Delay(Duration),
    /// Answer `307` to this location.
    Redirect(String),
    /// Answer this status with this JSON body, such as a server's 402 `quota_exceeded` for a
    /// write the org's plan doesn't allow ([`crate::fake_results::quota_exceeded`], ADR 0032).
    Answer(u16, Value),
}

/// \[M18\] How the fake's create receipts misbehave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReceiptFault {
    #[default]
    None,
    /// Receipts whose inclusion proof leads elsewhere.
    Corrupt,
    /// Creates answer without a receipt.
    Missing,
}

/// A request the fake received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    /// The path below the ledger's base, such as `objects/v1/…` or `` for `GET {base}`.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub authorization: Option<String>,
    pub if_none_match: Option<String>,
}

struct Injected {
    method: Option<String>,
    path: Option<String>,
    fault: Fault,
    times: usize,
}

/// \[M18\] The fake's transparency log: every write's entry, and the GC statement of signed
/// tombstones.
struct FakeLog {
    enabled: bool,
    signer: Option<Signer>,
    entries: Vec<(Entry, Option<Vec<u8>>)>,
    /// Keys every listing leaves out, while the statement signs for what it shows.
    hidden: BTreeSet<String>,
    /// Tokens shown another history: one without these keys' leaves.
    forks: BTreeMap<String, BTreeSet<String>>,
    receipts: ReceiptFault,
    /// Listings come without a statement.
    no_statements: bool,
    /// \[M19\] Each write ends in a map leaf, and listings carry proofs (L-12).
    maps: bool,
    /// \[M19\] Keys the map leaves leave out from the given write on: a lie the listing proofs
    /// agree with, which only a monitor's replay finds.
    unmapped: Vec<(usize, String)>,
    /// Listings read at a tree of this many leaves of the true history, whatever is newer.
    listings_at: Option<usize>,
}

impl View {
    /// The keys live at the end of the view.
    fn live(&self) -> BTreeSet<String> {
        let mut live = BTreeSet::new();
        for (entry, _) in &self.entries {
            match entry.kind {
                hushrun_tlog::EntryKind::Object => {
                    live.insert(entry.name.clone());
                }
                hushrun_tlog::EntryKind::Tombstone => {
                    live.remove(&entry.name);
                }
                _ => {}
            }
        }
        live
    }
}

/// One token's view of the log.
struct View {
    entries: Vec<(Entry, Option<Vec<u8>>)>,
    tree: MemTree,
    hidden: BTreeSet<String>,
    /// \[M19\] The map as of the view's last map leaf.
    map: MapBuilder,
}

struct State {
    objects: BTreeMap<String, Vec<u8>>,
    log: FakeLog,
    tokens: BTreeMap<String, Role>,
    faults: Vec<Injected>,
    seen: Vec<Seen>,
    /// Every request, whole.
    raw: Vec<Request>,
    repository: String,
    page_limit: usize,
    batch_delay: Duration,
    batches_in_flight: usize,
    max_batches_in_flight: usize,
    /// \[ADR 0031\] The machines `GET {base}/signers` names (L-13), by key id.
    machines: BTreeMap<String, Machine>,
    /// \[ADR 0031\] The repository's CI: issuer and repository id.
    ci: Vec<(String, String)>,
    /// \[ADR 0031 amendment 1\] The stages that take evidence from CI only (T-16).
    ci_only: hushrun_core::signers::CiOnly,
}

/// \[ADR 0031\] A machine the fake's signers name.
#[derive(Clone)]
pub struct Machine {
    pub key: hushrun_core::signers::PublicKey,
    pub owner: String,
    pub revoked: Option<hushrun_core::time::Time>,
    pub gc: bool,
}

/// A running fake ledger at `http://127.0.0.1:<port>/l/acme/back`.
pub struct FakeLedger {
    server: Server,
    state: Arc<Mutex<State>>,
    /// The ledger's path on the server.
    base: String,
}

impl FakeLedger {
    pub const READER: &'static str = "hrl_fake-reader-token";
    pub const WRITER: &'static str = "hrl_fake-writer-token";
    pub const ADMIN: &'static str = "hrl_fake-admin-token";
    /// The repository `GET {base}` names.
    pub const REPOSITORY: &'static str = "github.com/acme/back";
    const BASE: &'static str = "/l/acme/back";
    /// The seed of the log key, which signs checkpoints and listings.
    const LOG_SEED: [u8; 32] = [0x5a; 32];

    /// A fake ledger with the three tokens above.
    pub fn start() -> FakeLedger {
        FakeLedger::start_at(Self::BASE)
    }

    /// A fake ledger at `http://127.0.0.1:<port><base>`, such as one under an organization's
    /// ledger base (T-4).
    pub fn start_at(base: &str) -> FakeLedger {
        let state = Arc::new(Mutex::new(State {
            objects: BTreeMap::new(),
            log: FakeLog {
                enabled: true,
                signer: None,
                entries: Vec::new(),
                hidden: BTreeSet::new(),
                forks: BTreeMap::new(),
                receipts: ReceiptFault::None,
                no_statements: false,
                maps: false,
                unmapped: Vec::new(),
                listings_at: None,
            },
            tokens: [
                (Self::READER.to_string(), Role::Reader),
                (Self::WRITER.to_string(), Role::Writer),
                (Self::ADMIN.to_string(), Role::Admin),
            ]
            .into(),
            faults: Vec::new(),
            seen: Vec::new(),
            raw: Vec::new(),
            repository: Self::REPOSITORY.to_string(),
            page_limit: MAX_PAGE,
            batch_delay: Duration::ZERO,
            batches_in_flight: 0,
            max_batches_in_flight: 0,
            machines: BTreeMap::new(),
            ci: Vec::new(),
            ci_only: hushrun_core::signers::CiOnly::none(),
        }));
        let shared = state.clone();
        let base_url = Arc::new(Mutex::new(String::new()));
        let url_for_handler = base_url.clone();
        let server = Server::start(move |request| {
            let url = url_for_handler.lock().expect("url").clone();
            handle(&shared, &url, request)
        });
        let url = format!("{}{base}", server.url());
        *base_url.lock().expect("url") = url.clone();
        let origin = hushrun_tlog::origin_of(&url);
        state.lock().expect("state").log.signer =
            Some(Signer::new(&origin, Self::LOG_SEED).expect("a valid log key"));
        FakeLedger {
            server,
            state,
            base: base.to_string(),
        }
    }

    /// \[M18\] The log's origin and the public half of its key, `ed25519:<base64>`, as
    /// `transparency_log` in trust.yaml names them (T-9).
    pub fn log_key(&self) -> (String, String) {
        let state = self.state();
        let signer = state.log.signer.as_ref().expect("the log key");
        (
            signer.name().to_string(),
            format!(
                "ed25519:{}",
                base64::engine::general_purpose::STANDARD.encode(signer.public_key())
            ),
        )
    }

    /// \[M18\] Whether the fake keeps a log: without one, it answers as a Phase 1 ledger did.
    pub fn set_log(&self, enabled: bool) {
        self.state().log.enabled = enabled;
    }

    /// \[M18\] Leaves `key` out of every listing, and signs the statement for what is shown:
    /// the lie a monitor must catch.
    pub fn hide_from_listings(&self, key: &str) {
        self.state().log.hidden.insert(key.to_string());
    }

    /// \[M18\] Shows `token` another history: one in which `keys` were never written.
    pub fn fork_for(&self, token: &str, keys: &[String]) {
        self.state()
            .log
            .forks
            .insert(token.to_string(), keys.iter().cloned().collect());
    }

    pub fn set_receipt_fault(&self, fault: ReceiptFault) {
        self.state().log.receipts = fault;
    }

    /// Listings come without a signed statement.
    pub fn set_no_statements(&self, on: bool) {
        self.state().log.no_statements = on;
    }

    /// \[M19\] Ends each write's batch with a map leaf, and proves listings of map prefixes
    /// (L-12), as the hosted ledger does.
    pub fn set_maps(&self, on: bool) {
        self.state().log.maps = on;
    }

    /// \[M19\] From now on, leaves `key` out of the map its map leaves commit to, starting with a
    /// map leaf of its own, so that listings that leave it out are proven: the lie only a
    /// monitor's replay of the log finds. The history before stays as it was.
    pub fn leave_out_of_map(&self, key: &str) {
        let mut state = self.state();
        let at = state.log.entries.len();
        state.log.unmapped.push((at, key.to_string()));
    }

    /// Reads listings at the tree of the first `entries` writes, as an older checkpoint of the
    /// same history, whatever came since; `None` reads them at the latest.
    pub fn serve_listings_at(&self, entries: Option<usize>) {
        self.state().log.listings_at = entries;
    }

    /// The log's entries, oldest first.
    pub fn log_entries(&self) -> Vec<Entry> {
        self.state()
            .log
            .entries
            .iter()
            .map(|(e, _)| e.clone())
            .collect()
    }

    /// The latest signed checkpoint of the true history.
    pub fn checkpoint(&self) -> String {
        let state = self.state();
        state.log.checkpoint(&state.log.view(None)).note
    }

    /// The ledger's URL, `http://127.0.0.1:<port>/l/acme/back` unless it was started elsewhere.
    pub fn url(&self) -> String {
        format!("{}{}", self.server.url(), self.base)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake ledger state")
    }

    pub fn add_token(&self, token: &str, role: Role) {
        self.state().tokens.insert(token.to_string(), role);
    }

    /// Applies `fault` to the next `times` requests.
    pub fn inject(&self, fault: Fault, times: usize) {
        self.inject_on(None, None, fault, times);
    }

    /// Applies `fault` to the next `times` requests with this method and a path below the base
    /// that contains `path`.
    pub fn inject_on(&self, method: Option<&str>, path: Option<&str>, fault: Fault, times: usize) {
        self.state().faults.push(Injected {
            method: method.map(str::to_string),
            path: path.map(str::to_string),
            fault,
            times,
        });
    }

    /// Every request so far, oldest first.
    /// Every request so far, whole: its query, headers and body.
    pub fn raw_requests(&self) -> Vec<Request> {
        self.state().raw.clone()
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.state().seen.clone()
    }

    pub fn keys(&self) -> Vec<String> {
        self.state().objects.keys().cloned().collect()
    }

    pub fn object(&self, key: &str) -> Option<Vec<u8>> {
        self.state().objects.get(key).cloned()
    }

    /// Stores an object directly, as if a writer had.
    pub fn put_object(&self, key: &str, bytes: &[u8]) {
        let mut state = self.state();
        state.objects.insert(key.to_string(), bytes.to_vec());
        state.log.append(Entry::object(key, bytes), None);
    }

    /// Deletes an object directly, as if an admin had, without a GC statement: an unsigned
    /// tombstone (L-9).
    pub fn delete_object(&self, key: &str) {
        let mut state = self.state();
        if let Some(bytes) = state.objects.remove(key) {
            state
                .log
                .append(Entry::tombstone(key, Hash::of(&bytes), None), None);
        }
    }

    pub fn set_repository(&self, repository: &str) {
        self.state().repository = repository.to_string();
    }

    /// \[ADR 0031\] Names `key` as a machine of `owner` (`user:<email>` or `agent:<name>`) in
    /// `GET {base}/signers` (L-13); `gc` when the owner may sign GC statements.
    pub fn register_machine(&self, key: &hushrun_core::signers::PublicKey, owner: &str, gc: bool) {
        self.state().machines.insert(
            key.keyid(),
            Machine {
                key: *key,
                owner: owner.to_string(),
                revoked: None,
                gc,
            },
        );
    }

    /// \[ADR 0031\] Marks the machine with `keyid` revoked at `at`.
    pub fn revoke_machine(&self, keyid: &str, at: hushrun_core::time::Time) {
        if let Some(machine) = self.state().machines.get_mut(keyid) {
            machine.revoked = Some(at);
        }
    }

    /// \[ADR 0031\] Forgets the machine with `keyid`, as for a key nobody registered.
    pub fn forget_machine(&self, keyid: &str) {
        self.state().machines.remove(keyid);
    }

    /// \[ADR 0031\] Names the repository's CI: certificates of `issuer` whose Source Repository
    /// Identifier is `repository_id` (T-15).
    pub fn set_ci(&self, issuer: &str, repository_id: &str) {
        self.state().ci = vec![(issuer.to_string(), repository_id.to_string())];
    }

    /// \[ADR 0062\] Makes the repository CI-only from `since` on in `GET {base}/signers` (T-16,
    /// L-13), as its one switch does on a server.
    pub fn set_repository_ci_only(&self, since: hushrun_core::time::Time) {
        let mut state = self.state();
        state.ci_only = std::mem::take(&mut state.ci_only).with_stageless_period(since, None);
    }

    /// \[ADR 0062\] Adds a period when the repository was CI-only, from `from` until `until`
    /// (still, without one), to `GET {base}/signers`'s history (T-16, L-13).
    pub fn add_repository_ci_only_period(
        &self,
        from: hushrun_core::time::Time,
        until: Option<hushrun_core::time::Time>,
    ) {
        let mut state = self.state();
        state.ci_only = std::mem::take(&mut state.ci_only).with_stageless_period(from, until);
    }

    /// \[ADR 0031 amendment 1\] Names `stage` CI-only from `since` on in `GET {base}/signers`
    /// (T-16, L-13), as a 0.8 server answered: 0.9 reads a stage's periods as the repository's.
    pub fn set_ci_only(&self, stage: &str, since: hushrun_core::time::Time) {
        let mut state = self.state();
        let stage = hushrun_core::names::StageName::new(stage).expect("a stage name");
        state.ci_only = std::mem::take(&mut state.ci_only).with(stage, since);
    }

    /// \[ADR 0031 amendment 2\] Adds a period when `stage` was CI-only, from `from` until
    /// `until` (still, without one), to `GET {base}/signers`'s history (T-16, L-13).
    pub fn add_ci_only_period(
        &self,
        stage: &str,
        from: hushrun_core::time::Time,
        until: Option<hushrun_core::time::Time>,
    ) {
        let mut state = self.state();
        let stage = hushrun_core::names::StageName::new(stage).expect("a stage name");
        state.ci_only = std::mem::take(&mut state.ci_only).with_period(stage, from, until);
    }

    /// Answers listings with pages of at most `n` keys, whatever `limit` asks for.
    pub fn set_page_limit(&self, n: usize) {
        self.state().page_limit = n.clamp(1, MAX_PAGE);
    }

    /// Holds every batch-get this long, so that concurrent ones overlap.
    pub fn set_batch_delay(&self, delay: Duration) {
        self.state().batch_delay = delay;
    }

    /// The most batch-gets that were in progress at once.
    pub fn max_batches_in_flight(&self) -> usize {
        self.state().max_batches_in_flight
    }
}

fn error(status: u16, code: &str, message: &str) -> Response {
    Response::json(
        status,
        &json!({"error": {"code": code, "message": message}}),
    )
}

/// Every key and name L-10 accepts: 1 to 1024 bytes of `[A-Za-z0-9._-]` segments.
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 1024
        && key.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

fn handle(state: &Mutex<State>, url: &str, request: Request) -> Reply {
    // The base's path: what follows the authority of `http://127.0.0.1:<port><base>`.
    let base = url
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| &rest[i..]))
        .unwrap_or_default();
    let below = request
        .path
        .strip_prefix(base)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .map(|rest| rest.trim_start_matches('/').to_string());
    let fault = {
        let mut state = state.lock().expect("state");
        state.seen.push(Seen {
            method: request.method.clone(),
            path: below.clone().unwrap_or_else(|| request.path.clone()),
            query: request.query.clone(),
            authorization: request.header("authorization").map(str::to_string),
            if_none_match: request.header("if-none-match").map(str::to_string),
        });
        state.raw.push(request.clone());
        let path = below.as_deref().unwrap_or_default();
        let found = state.faults.iter_mut().find(|f| {
            f.times > 0
                && f.method.as_deref().is_none_or(|m| m == request.method)
                && f.path.as_deref().is_none_or(|p| path.contains(p))
        });
        found.map(|f| {
            f.times -= 1;
            f.fault.clone()
        })
    };
    match fault {
        Some(Fault::Status(status, retry_after)) => {
            let mut response = error(status, "unavailable", "injected by the test");
            if let Some(after) = retry_after {
                response = response.header("retry-after", after);
            }
            return Reply::Send(response);
        }
        Some(Fault::Drop) => return Reply::Close,
        Some(Fault::Redirect(location)) => {
            return Reply::Send(Response::new(307).header("location", location));
        }
        Some(Fault::Answer(status, body)) => {
            return Reply::Send(Response::json(status, &body).header("date", http_date()));
        }
        _ => {}
    }
    let response = match below {
        Some(path) => route(state, url, &request, &path),
        None => error(404, "not_found", "no such ledger"),
    };
    let response = response.header("date", http_date());
    match fault {
        Some(Fault::DropAfter) => Reply::Close,
        Some(Fault::Delay(pause)) => Reply::Later(pause, response),
        _ => Reply::Send(response),
    }
}

fn http_date() -> String {
    jiff::fmt::rfc2822::DateTimePrinter::new()
        .timestamp_to_rfc9110_string(&jiff::Timestamp::now())
        .expect("a current date formats")
}

/// \[ADR 0031\] `GET {base}/signers` (L-13): the machines and CI the test named, with the
/// statement signed by the log key when the fake keeps a log.
fn signers(state: &Mutex<State>) -> Response {
    let state = state.lock().expect("state");
    let signers = hushrun_core::signers::Signers {
        repository: state.repository.clone(),
        time: None,
        ci: state
            .ci
            .iter()
            .map(|(issuer, id)| hushrun_core::signers::CiIdentity {
                issuer: issuer.clone(),
                repository_id: id.clone(),
            })
            .collect(),
        machines: state
            .machines
            .iter()
            .map(|(keyid, m)| {
                (
                    keyid.clone(),
                    hushrun_core::signers::Machine {
                        key: m.key,
                        name: m.owner.split(':').nth(1).unwrap_or(&m.owner).to_string(),
                        owner: m.owner.clone(),
                        registered: None,
                        revoked: m.revoked,
                        gc: m.gc,
                    },
                )
            })
            .collect(),
        ci_only: state.ci_only.clone(),
    };
    let document = signers.to_json();
    let statement = match (&state.log.signer, state.log.enabled) {
        (Some(signer), true) => {
            let digest = hushrun_core::signers::Signers::digest_of(&document).expect("JCS");
            let statement = hushrun_tlog::signers::SignersStatement {
                origin: signer.name().to_string(),
                document: Hash::from_hex(&digest.hex()).expect("a hex digest"),
            };
            Value::String(statement.sign(signer).expect("signs"))
        }
        _ => Value::Null,
    };
    Response::json(200, &json!({"signers": document, "statement": statement}))
}

fn route(state: &Mutex<State>, url: &str, request: &Request, path: &str) -> Response {
    let token = request
        .header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let role = {
        let state = state.lock().expect("state");
        token
            .as_deref()
            .and_then(|token| state.tokens.get(token).copied())
    };
    let Some(role) = role else {
        return error(401, "unauthenticated", "a ledger token is required");
    };
    let token = token.as_deref();
    let need = |wanted: Role| -> Option<Response> {
        (role < wanted).then(|| error(403, "denied", "this token's role can't do that"))
    };
    let method = request.method.as_str();
    if path.is_empty() && method == "GET" {
        let state = state.lock().expect("state");
        let mut info = json!({
            "protocol": "hushrun-ledger-http/1",
            "ledger": url,
            "repository": state.repository,
            "org": "acme",
            "name": "back",
        });
        if state.log.enabled {
            let signer = state.log.signer.as_ref().expect("the log key");
            info["log"] = json!({
                "origin": signer.name(),
                "key": format!("ed25519:{}", base64::engine::general_purpose::STANDARD.encode(signer.public_key())),
                "size": state.log.view(token).tree.size(),
            });
        }
        return Response::json(200, &info);
    }
    if path == "objects" && method == "GET" {
        return list(state, request, token);
    }
    if path == "signers" && method == "GET" {
        return signers(state);
    }
    if let Some(endpoint) = path.strip_prefix("log/")
        && method == "GET"
    {
        return log_endpoint(state, request, token, endpoint);
    }
    if path == "batch-get" && method == "POST" {
        return batch_get(state, request, token);
    }
    if let Some(key) = path.strip_prefix("objects/") {
        if !valid_key(key) {
            return error(400, "invalid", "not a ledger key");
        }
        return match method {
            "GET" => {
                let state = state.lock().expect("state");
                match state.objects.get(key) {
                    Some(bytes) if !state.log.forked_away(token, key) => {
                        Response::bytes(200, bytes.clone())
                            .header("x-hushrun-sha256", Digest::of(bytes).hex())
                    }
                    _ => error(404, "not_found", "no such object"),
                }
            }
            "PUT" => need(Role::Writer).unwrap_or_else(|| create(state, request, key, token)),
            "DELETE" => need(Role::Admin).unwrap_or_else(|| delete(state, request, key)),
            _ => error(400, "invalid", "unsupported method"),
        };
    }
    error(404, "not_found", "no such endpoint")
}

/// `PUT {base}/objects/{key}`: create-only, under `v1/`, in the shapes of L-2.
fn create(state: &Mutex<State>, request: &Request, key: &str, token: Option<&str>) -> Response {
    if !key.starts_with("v1/") {
        return error(403, "denied", "writers create keys under v1/ only");
    }
    let shaped = parse_event_key(key).is_some_and(|k| ulid_millis(&k.ulid).is_some())
        || deployed_shape(key)
        || parse_policy_key(key).is_some_and(|digest| digest == Digest::of(&request.body));
    if !shaped {
        return error(400, "invalid", "the key has none of the v1/ shapes");
    }
    if request.header("if-none-match") != Some("*") {
        return error(428, "invalid", "send If-None-Match: *");
    }
    if request.body.len() > MAX_BODY {
        return error(413, "too_large", "objects are at most 1 MiB");
    }
    let mut state = state.lock().expect("state");
    if let Some(existing) = state.objects.get(key) {
        let mut answer = json!({
            "error": {"code": "already_exists", "message": "the key exists"},
            "sha256": Digest::of(existing).hex(),
        });
        let existing = existing.clone();
        if let Some(receipt) = state.log.receipt(token, &Entry::object(key, &existing)) {
            answer["receipt"] = receipt;
        }
        return Response::json(412, &answer);
    }
    state.objects.insert(key.to_string(), request.body.clone());
    let entry = Entry::object(key, &request.body);
    state.log.append(entry.clone(), None);
    let mut answer =
        json!({"key": key, "sha256": Digest::of(&request.body).hex(), "size": request.body.len()});
    if let Some(receipt) = state.log.receipt(token, &entry) {
        answer["receipt"] = receipt;
    }
    Response::json(201, &answer)
}

/// `DELETE {base}/objects/{key}`, with the tombstone's GC statement from `X-Hushrun-Gc`.
fn delete(state: &Mutex<State>, request: &Request, key: &str) -> Response {
    let statement = match request.header("x-hushrun-gc") {
        None => None,
        Some(encoded) => match base64::engine::general_purpose::STANDARD.decode(encoded) {
            Ok(bytes) => Some(bytes),
            Err(_) => return error(400, "invalid", "X-Hushrun-Gc is not base64"),
        },
    };
    let mut state = state.lock().expect("state");
    match state.objects.remove(key) {
        Some(bytes) => {
            let gc = statement.as_deref().map(Hash::of);
            state
                .log
                .append(Entry::tombstone(key, Hash::of(&bytes), gc), statement);
            Response::new(204)
        }
        None => error(404, "not_found", "no such object"),
    }
}

/// `v1/deployed/<scope>/<stage or _>/<13 digits>.<ULID>.json`.
fn deployed_shape(key: &str) -> bool {
    parse_deployed_key(key).is_some_and(|parsed| ulid_millis(&parsed.ulid).is_some())
}

/// `GET {base}/objects?prefix&after&limit`, at the latest checkpoint, with its statement.
fn list(state: &Mutex<State>, request: &Request, token: Option<&str>) -> Response {
    let prefix = request.query("prefix").unwrap_or_default().to_string();
    let after = request.query("after").map(str::to_string);
    let limit = match request.query("limit").map(str::parse::<usize>) {
        None => MAX_PAGE,
        Some(Ok(n)) if (1..=MAX_PAGE).contains(&n) => n,
        Some(_) => return error(400, "invalid", "limit is 1 to 1000"),
    };
    let state = state.lock().expect("state");
    let limit = limit.min(state.page_limit);
    let lower = match &after {
        Some(after) if after.as_str() >= prefix.as_str() => Bound::Excluded(after.clone()),
        _ => Bound::Included(prefix.clone()),
    };
    let view = match state.log.listings_at {
        Some(writes) => state.log.view_of(token, writes),
        None => state.log.view(token),
    };
    let older: Option<BTreeSet<String>> = state.log.listings_at.map(|_| view.live());
    let mut keys: Vec<String> = state
        .objects
        .range::<String, _>((lower, Bound::Unbounded))
        .map(|(k, _)| k)
        .take_while(|k| k.starts_with(prefix.as_str()))
        .filter(|k| !view.hidden.contains(*k) && !state.log.hidden.contains(*k))
        .filter(|k| older.as_ref().is_none_or(|live| live.contains(*k)))
        .take(limit + 1)
        .cloned()
        .collect();
    let more = keys.len() > limit;
    keys.truncate(limit);
    let next = if more { keys.last().cloned() } else { None };
    let mut answer = json!({"keys": keys, "next": next});
    if state.log.enabled {
        let checkpoint = state.log.checkpoint(&view);
        if state.log.maps && is_map_prefix(&prefix) {
            let proof = state
                .log
                .proof(&view, &checkpoint, &prefix, after.as_deref(), &keys);
            answer["proof"] = json!(ListingProofWire::from(&proof));
        }
        answer["checkpoint"] = json!(checkpoint.note);
        if !state.log.no_statements {
            let listing = Listing::new(
                &checkpoint.checkpoint,
                &prefix,
                after.as_deref(),
                next.as_deref(),
                &keys,
            );
            match listing {
                Ok(listing) => {
                    let signer = state.log.signer.as_ref().expect("the log key");
                    let signed = SignedListing::sign(listing, signer).expect("signs");
                    answer["statement"] = json!(signed.note);
                }
                Err(e) => return error(400, "invalid", &e.to_string()),
            }
        }
    }
    Response::json(200, &answer)
}

impl FakeLog {
    fn append(&mut self, entry: Entry, statement: Option<Vec<u8>>) {
        self.entries.push((entry, statement));
    }

    /// Whether `token` is shown a history without `key`.
    fn forked_away(&self, token: Option<&str>, key: &str) -> bool {
        token
            .and_then(|t| self.forks.get(t))
            .is_some_and(|hidden| hidden.contains(key))
    }

    /// The history `token` is shown: \[M19\] with maps, every write followed by its map leaf.
    fn view(&self, token: Option<&str>) -> View {
        self.view_of(token, self.entries.len())
    }

    /// The history `token` is shown, as of its first `writes` writes.
    fn view_of(&self, token: Option<&str>, writes: usize) -> View {
        let hidden = token
            .and_then(|t| self.forks.get(t))
            .cloned()
            .unwrap_or_default();
        let mut map = MapBuilder::new();
        let mut entries: Vec<(Entry, Option<Vec<u8>>)> = Vec::new();
        let mut unmapped = BTreeSet::new();
        // The lies told from write `at` on: each takes its key out of the map, in a batch of
        // its own.
        let lie = |at: usize,
                   map: &mut MapBuilder,
                   entries: &mut Vec<(Entry, Option<Vec<u8>>)>,
                   unmapped: &mut BTreeSet<String>| {
            for (_, key) in self.unmapped.iter().filter(|(from, _)| *from == at) {
                unmapped.insert(key.clone());
                map.remove(key);
                if self.maps {
                    entries.push((Entry::map(map.commit()), None));
                }
            }
        };
        for (at, (entry, statement)) in self.entries.iter().enumerate().take(writes) {
            lie(at, &mut map, &mut entries, &mut unmapped);
            if hidden.contains(&entry.name) {
                continue;
            }
            entries.push((entry.clone(), statement.clone()));
            if self.maps {
                if !unmapped.contains(&entry.name) {
                    map.apply(entry);
                }
                entries.push((Entry::map(map.commit()), None));
            }
        }
        lie(writes, &mut map, &mut entries, &mut unmapped);
        let tree = MemTree::from_leaves(entries.iter().map(|(e, _)| e.leaf_hash()));
        View {
            entries,
            tree,
            hidden,
            map,
        }
    }

    /// \[M19\] The proof of a listing page at the view's checkpoint (L-12), from the map its
    /// last leaf commits to: an honest one, whatever the page leaves out.
    fn proof(
        &self,
        view: &View,
        checkpoint: &SignedCheckpoint,
        prefix: &str,
        after: Option<&str>,
        keys: &[String],
    ) -> ListingProof {
        let size = checkpoint.size();
        let all = view.map.keys(prefix);
        let index = all
            .iter()
            .take_while(|k| after.is_some_and(|after| k.as_str() <= after))
            .count();
        let page = (!all.is_empty())
            .then(|| PageProof::new(&all, index, keys.len().min(all.len() - index)))
            .flatten();
        ListingProof {
            map_root: view.map.map().root(),
            map_leaf: if size == 0 {
                Vec::new()
            } else {
                view.tree
                    .inclusion_proof(size - 1, size)
                    .expect("the view's last leaf")
            },
            map: view.map.map().prove(prefix),
            page,
        }
    }

    fn checkpoint(&self, view: &View) -> SignedCheckpoint {
        let signer = self.signer.as_ref().expect("the log key");
        let checkpoint = Checkpoint::new(signer.name(), view.tree.size(), view.tree.root());
        SignedCheckpoint::sign(checkpoint, signer).expect("signs")
    }

    /// The receipt of `entry`'s latest leaf in `token`'s view, as the create's answer carries it.
    fn receipt(&self, token: Option<&str>, entry: &Entry) -> Option<Value> {
        if !self.enabled || self.receipts == ReceiptFault::Missing {
            return None;
        }
        let view = self.view(token);
        let index = view.entries.iter().rposition(|(e, _)| e == entry)? as u64;
        let mut proof = view.tree.inclusion_proof(index, view.tree.size())?;
        if self.receipts == ReceiptFault::Corrupt {
            match proof.first_mut() {
                Some(first) => *first = Hash::leaf(b"corrupt"),
                None => proof.push(Hash::leaf(b"corrupt")),
            }
        }
        Some(json!({
            "index": index,
            "checkpoint": self.checkpoint(&view).note,
            "proof": proof.iter().map(Hash::base64).collect::<Vec<_>>(),
        }))
    }
}

/// `GET {base}/log/…` (L-11): the latest checkpoint, proofs and leaves of `token`'s view.
fn log_endpoint(
    state: &Mutex<State>,
    request: &Request,
    token: Option<&str>,
    endpoint: &str,
) -> Response {
    let state = state.lock().expect("state");
    if !state.log.enabled {
        return error(404, "not_found", "this ledger keeps no log");
    }
    let view = state.log.view(token);
    let size = view.tree.size();
    let number = |name: &str| request.query(name).and_then(|v| v.parse::<u64>().ok());
    let proof = |hashes: Vec<Hash>| hashes.iter().map(Hash::base64).collect::<Vec<_>>();
    match endpoint {
        "checkpoint" => Response::bytes(200, state.log.checkpoint(&view).note.into_bytes())
            .header("content-type", "text/plain; charset=utf-8"),
        "consistency" => match (number("from"), number("to")) {
            (Some(from), Some(to)) if from <= to && to <= size => Response::json(
                200,
                &json!({"from": from, "to": to, "proof": proof(view.tree.consistency_proof(from, to).expect("in range"))}),
            ),
            _ => error(
                400,
                "invalid",
                "from and to must satisfy from <= to <= the tree's size",
            ),
        },
        "inclusion" if request.query("key").is_some() => {
            let key = request.query("key").unwrap_or_default();
            match state
                .objects
                .get(key)
                .filter(|_| !view.hidden.contains(key))
            {
                Some(bytes) => {
                    let entry = Entry::object(key, bytes);
                    match state.log.receipt(token, &entry) {
                        Some(receipt) => Response::json(
                            200,
                            &json!({"key": key, "sha256": entry.sha256.hex(), "receipt": receipt}),
                        ),
                        None => error(404, "not_found", "no receipt"),
                    }
                }
                None => error(404, "not_found", "no such object"),
            }
        }
        "inclusion" => match (number("index"), number("size")) {
            (Some(index), Some(at)) if index < at && at <= size => Response::json(
                200,
                &json!({"index": index, "size": at, "proof": proof(view.tree.inclusion_proof(index, at).expect("in range"))}),
            ),
            _ => error(
                400,
                "invalid",
                "index and size must satisfy index < size <= the tree's size",
            ),
        },
        "leaves" => {
            let start = number("start").unwrap_or(0);
            let limit = number("limit").unwrap_or(1000).clamp(1, 1000);
            let leaves: Vec<Value> = view
                .entries
                .iter()
                .enumerate()
                .skip(usize::try_from(start).unwrap_or(usize::MAX))
                .take(limit as usize)
                .map(|(index, (entry, statement))| {
                    json!({
                        "index": index,
                        "kind": entry.kind.as_str(),
                        "name": entry.name,
                        "sha256": entry.sha256.hex(),
                        "gc": entry.gc.map(|h| h.hex()),
                        "statement": statement.as_ref().map(|s| base64::engine::general_purpose::STANDARD.encode(s)),
                    })
                })
                .collect();
            Response::json(200, &json!({"leaves": leaves}))
        }
        _ => error(404, "not_found", "no such endpoint"),
    }
}

/// `POST {base}/batch-get`.
fn batch_get(state: &Mutex<State>, request: &Request, token: Option<&str>) -> Response {
    let keys: Option<Vec<String>> = serde_json::from_slice::<Value>(&request.body)
        .ok()
        .and_then(|v| serde_json::from_value(v.get("keys")?.clone()).ok());
    let Some(keys) = keys.filter(|k| (1..=MAX_BATCH).contains(&k.len())) else {
        return error(400, "invalid", "send {\"keys\": [...]} with 1 to 100 keys");
    };
    if !keys.iter().all(|k| valid_key(k)) {
        return error(400, "invalid", "not a ledger key");
    }
    let delay = {
        let mut state = state.lock().expect("state");
        state.batches_in_flight += 1;
        state.max_batches_in_flight = state.max_batches_in_flight.max(state.batches_in_flight);
        state.batch_delay
    };
    std::thread::sleep(delay);
    let mut state = state.lock().expect("state");
    state.batches_in_flight -= 1;
    let objects: Vec<Value> = keys
        .iter()
        .map(|key| {
            match state
                .objects
                .get(key)
                .filter(|_| !state.log.forked_away(token, key))
            {
                Some(bytes) => json!({
                    "key": key,
                    "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                }),
                None => json!({"key": key, "error": "not_found"}),
            }
        })
        .collect();
    Response::json(200, &json!({"objects": objects}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_names_like_the_contract() {
        for good in [
            "trust.yaml",
            "root/1.json",
            "v1/events/a/b/01JA.check._.json",
        ] {
            assert!(valid_key(good), "{good}");
        }
        for bad in [
            "",
            "/a",
            "a/",
            "a//b",
            "a/../b",
            "a b",
            "é",
            &"a".repeat(1025),
        ] {
            assert!(!valid_key(bad), "{bad}");
        }
        let ulid = "01JA0000000000000000000000";
        assert!(deployed_shape(&format!(
            "v1/deployed/a/b/9999999999999.{ulid}.json"
        )));
        for bad in [
            format!("v1/deployed/a/b/-000000000001.{ulid}.json"),
            "v1/deployed/a/b/9999999999999.NOTAULIDNOTAULIDNOTAULIDNO.json".to_string(),
        ] {
            assert!(!deployed_shape(&bad), "{bad}");
        }
    }
}
