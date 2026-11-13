//! What `hushrun run NAME -- COMMAND` remembers: one result per run of a check's command on
//! some content, in a backend that only appends.
//!
//! A run's key is the SHA-256 of what decides whether it can stand in for another: the check's
//! name, its command, the directory it ran in (relative to the repository root), the scope's
//! source digest, \[ADR 0037\] those of what the scope declares outside the repository and
//! \[ADR 0045\] the rule `hushrun.yaml` gives the check. The machine and the CI provider aren't
//! part of it, so a pass on a laptop and a run in CI on the same files share a key.
//!
//! The backend is a server that speaks the results protocol, `hushrun-results/v0`, at an org's
//! URL such as `https://host/o/acme`, or, when none is configured, a directory in hushrun's
//! cache, which only this machine reads:
//!
//! - `GET {base}/v0/results/{key}`: `{"result": …}`, the newest result for the key's 64 hex
//!   characters, or `{"result": null}`, with `"ciOnly": true` when the server answered from CI's
//!   results only; \[ADR 0049\] with `?job=<job>[&variant=<key>]`, the newest that counts in that
//!   job, which a server from before the query ignores;
//! - `POST {base}/v0/results/{key}`: records one; \[ADR 0032\] a server may refuse it for the
//!   org's plan (402 `quota_exceeded`), and the result then isn't recorded;
//! - `GET {base}/v0`: the protocol, the org, and who the caller is to it;
//! - \[ADR 0032\] `GET {base}/v0/usage`, optional: the org's plan and this period's recorded
//!   checks, for `hushrun repository usage`.
//!
//! Requests carry `HUSHRUN_TOKEN` when `HUSHRUN_BACKEND` names the backend, else the token
//! `hushrun login` saved for the host, else in GitHub Actions an ID token whose audience is the
//! base URL. A backend that the repository's `package.json` or `hushrun.yaml` names never gets
//! `HUSHRUN_TOKEN`, so a change to a file in the repository can't send a CI secret elsewhere; except an agent
//! token (`hra_…`) when that backend is on hushrun's host, which the environment names and the
//! repository can't (ADR 0027). \[ADR 0046\] `HUSHRUN_AGENT` names the backend and its token
//! together, and that token goes to that backend's origin and nowhere else.
//!
//! \[ADR 0067\] [`Backend::select`] alone decides whether results are shared, stay on this machine
//! or are off: outside CI, a backend the repository's files name that nothing here has a
//! credential for at all is this machine's directory, until someone signs in; and in CI, where a
//! runner's directory is shared with no one and a cache of it could be forged, the directory is
//! off. This machine's directory forgets a run 30 days after it ([`Backend::sweep`]).
//!
//! \[ADR 0052\] In GitHub Actions with none of these, as in a pull request from a fork, which
//! GitHub gives no ID token, a lookup asks as the repository's CI, `?as=ci&repository=` of
//! `GITHUB_REPOSITORY`, with no credential: a server answers it for a public repository in its
//! open-source program, from what that repository's CI recorded. Such a run records nothing, and
//! sends no `POST`.
//!
//! \[ADR 0028, amended 2026-10-28\] A result recorded with a person's or an agent's token always
//! carries the signature of a machine (`Hushrun-Signature`): a person's, registered at sign-in,
//! or the agent's, the key `hushrun token create --agent` made with its token, which the
//! environment names. Without a key, the result isn't sent, and the run says what to do; a
//! server's 401 `machine_required` or `machine_unknown` says the same. CI's ID tokens never sign.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;

use hushrun_ci::Env;
use hushrun_core::counts_from::Rule;
use hushrun_core::digest::Digest;
use hushrun_core::ledger_url::LedgerUrl;
use hushrun_core::names::{CheckName, ScopeName};
use hushrun_core::policy::Policy;
use hushrun_core::run::Provider;
use hushrun_core::time::Time;
use hushrun_ledger::{
    FileLedger, HttpLedger, Ledger, LedgerError, OidcEndpoint, Options, Quota, Secret,
};

use crate::credentials::Credentials;
use crate::hosted;

/// Changing it gives every run a new key, so no earlier result counts.
const KEY_SCHEMA: &str = "hushrun.run-key/v0";

/// \[ADR 0052\] Why a GitHub Actions job with no credential records nothing, as a server would say
/// it, for the job's own repository.
pub const NO_ID_TOKEN: &str = "no token: give the job `permissions: id-token: write`";

/// A lookup's answer (C-12).
#[derive(Debug, Default)]
pub struct Latest {
    /// The newest pass or fail recorded for the key.
    pub result: Option<RunResult>,
    /// Whether the server answered from CI's results only: the repository's setting that CI
    /// reuses only CI's results, for a CI caller (`"ciOnly": true`).
    pub ci_only: bool,
    /// \[ADR 0045\] Who recorded it, as the server says: unlike the result's own fields, no machine
    /// wrote this.
    pub recorded_by: Option<String>,
    /// \[ADR 0049\] The answer is for the job asked (`"forJob": true`): a server from before the
    /// query answers the newest for the key instead.
    pub for_job: bool,
}

/// The schema of a stored result.
pub const RESULT_SCHEMA: &str = "hushrun.run-result/v0";
/// Where results live in a directory: `results/v0/<key hex>/<ULID>.json`, and skips apart,
/// under `skips/v0/`, so that they never answer a lookup.
const PREFIX: &str = "results/v0";
const SKIP_PREFIX: &str = "skips/v0";
/// How many of a key's newest results a lookup reads, looking for one it can parse.
const LOOKUP_LIMIT: usize = 10;
/// A day, in milliseconds.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// \[ADR 0067\] How long this machine keeps a result or a skip, by its ULID's time.
const KEPT_FOR_MS: i64 = 30 * DAY_MS;
/// \[ADR 0067\] How often this machine sweeps its directory at most, and the file, in the directory,
/// that says when it last began to: an RFC 3339 time.
const SWEEP_EVERY_MS: i64 = DAY_MS;
const SWEPT: &str = "swept";
/// The most a server's requests may take in one run: past it, the command just runs.
const SERVER_BUDGET: Duration = Duration::from_secs(10);
/// How much of a mark `hushrun ready` reads.
const MARK_LIMIT: u64 = 1000;

/// What this machine keeps of its own last run of a key that nothing records (C-22), for
/// `hushrun ready`, which starts no tool: `<cache dir>/unrecorded/<key hex>`, absent when there
/// is nothing to say. \[ADR 0060\] [`ran_here`] makes every move, and [`mark`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// Its last run here passed, and couldn't be recorded, say for the org's plan: an empty
    /// file. Nothing recorded it, so `hushrun ready` would otherwise ask for the same run for ever.
    Unrecorded,
    /// \[ADR 0045\] Its last run here was under a rule that didn't count, pass or fail: the file
    /// holds why. `hushrun ready` starts no tool, so this is how it learns, and it stops asking.
    DidntCount(String),
}

/// How a run of a key here ended, for its mark (C-22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended<'a> {
    /// Its result, or its check event, was recorded: newer than any run of its own that wasn't.
    Recorded,
    /// It passed, and nothing recorded it.
    Unrecorded,
    /// It failed, recorded or not: \[ADR 0060\] a failure here outranks an earlier pass here
    /// that nothing recorded, so `hushrun ready` asks for the run again.
    Failed,
    /// \[ADR 0045\] It ran under a rule this machine doesn't meet, pass or fail, for this reason.
    DidntCount(&'a str),
}

/// \[ADR 0060\] The one place a mark moves: what a run here that `ended` so leaves of `key`.
/// Why the mark couldn't be kept, when it couldn't. One recorded elsewhere leaves the file:
/// `hushrun ready` reads a mark only when there is no result.
pub fn ran_here(key: &Digest, ended: Ended<'_>) -> Result<(), String> {
    match ended {
        Ended::Recorded | Ended::Failed => {
            if let Some(path) = unrecorded_mark(key) {
                let _ = std::fs::remove_file(path);
            }
            Ok(())
        }
        Ended::Unrecorded => write_mark(key, ""),
        Ended::DidntCount(why) => write_mark(key, why),
    }
}

fn write_mark(key: &Digest, text: &str) -> Result<(), String> {
    let path = unrecorded_mark(key).ok_or_else(no_cache)?;
    path.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|err| {
            tracing::debug!("{}: {err}", path.display());
            format!("{}: {err}", path.display())
        })
}

fn no_cache() -> String {
    "there is no cache directory".to_string()
}

/// \[ADR 0045\] Whether marks can be kept here, or why not: with no mark, `hushrun ready` could never
/// learn that a run didn't count, and would ask for it for ever.
pub fn marks_stay() -> Result<(), String> {
    let dir = crate::context::cache_dir()
        .ok_or_else(no_cache)?
        .join("unrecorded");
    std::fs::create_dir_all(&dir)
        .and_then(|()| tempfile::NamedTempFile::new_in(&dir).map(|_| ()))
        .map_err(|err| format!("{}: {err}", dir.display()))
}

/// The mark of `key`, when a run here left one. \[ADR 0045\] A reason is one line, of a length that
/// fits one, whatever the file holds, since a tool's words get into it.
pub fn mark(key: &Digest) -> Option<Mark> {
    let mut bytes = Vec::new();
    std::fs::File::open(unrecorded_mark(key)?)
        .and_then(|file| file.take(MARK_LIMIT).read_to_end(&mut bytes))
        .ok()?;
    let text: String = String::from_utf8_lossy(&bytes)
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let text = text.trim();
    Some(match text.is_empty() {
        true => Mark::Unrecorded,
        false => Mark::DidntCount(text.to_string()),
    })
}

fn unrecorded_mark(key: &Digest) -> Option<PathBuf> {
    crate::context::cache_dir().map(|dir| dir.join("unrecorded").join(key.hex()))
}

/// What marks hushrun.yaml's `check` (`hushrun run CHECK`) on `scope` at `digest` as a pass that
/// wasn't recorded: a key of no command, which no `hushrun run NAME -- COMMAND` has (C-9). The
/// formula is the one a run of hushrun.yaml's checks used before 0.9, so marks made then still
/// read (ADR 0062).
pub fn ledger_check_key(
    check: &CheckName,
    scope: &ScopeName,
    digest: &Digest,
) -> anyhow::Result<Digest> {
    key(check, &[], "", scope, digest, &[], None)
}

/// \[ADR 0037\] What a scope declares outside the repository, digested: a key's and a result's
/// `external`, by path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalInput {
    pub path: String,
    pub digest: Digest,
}

/// \[ADR 0045\] Where a run happened, in E-7's shape: the platform, which every result has, and the
/// tools a rule made hushrun measure. It informs: only the key decides. A machine's own word.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEnvironment {
    pub platform: String,
    /// The version of each tool measured, by the name `counts_from` gives it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, String>,
}

/// \[ADR 0045\] The key of a run and the rule in it, as `policy` says (C-9, C-25). `hushrun run` and
/// `hushrun ready` both call this, so they can't look for a pass in different places, and a run
/// that holds this key knows the rule it has to meet.
#[allow(clippy::too_many_arguments)]
pub fn run_key(
    policy: Option<&Policy>,
    check: &CheckName,
    command: &[String],
    cwd: &str,
    scope: &ScopeName,
    digest: &Digest,
    external: &[ExternalInput],
) -> anyhow::Result<(Digest, Option<Rule>)> {
    let rule = policy
        .and_then(|policy| policy.counts_from.resolve(check, scope))
        .cloned();
    let key = key(check, command, cwd, scope, digest, external, rule.as_ref())?;
    Ok((key, rule))
}

/// The key of a run of `command` for `check`, started in `cwd`, on `scope` at `digest`, with what
/// the scope declares outside the repository at `external`, by path, under `rule`. A scope that
/// declares nothing and a check with no rule have the key they always had: `external` and the rule
/// are only in the key when there is one. The key holds the rule and never what a machine
/// measured, so every machine that meets the rule computes it, and `ready` needs no server for it.
fn key(
    check: &CheckName,
    command: &[String],
    cwd: &str,
    scope: &ScopeName,
    digest: &Digest,
    external: &[ExternalInput],
    rule: Option<&Rule>,
) -> anyhow::Result<Digest> {
    let mut input = json!({
        "schema": KEY_SCHEMA,
        "check": check,
        "command": command,
        "cwd": cwd,
        "scope": scope,
        "digest": digest,
    });
    if !external.is_empty() {
        input["external"] = json!(external);
    }
    if let Some(rule) = rule {
        input["counts_from"] = rule.normal();
    }
    let canonical = hushrun_core::jcs::canonicalize(&input)?;
    Ok(Digest::of(canonical.as_bytes()))
}

/// How a run ended; or `Skip`, a run that didn't happen because the same code had passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    Skip,
}

/// One run, as a backend keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResult {
    pub schema: String,
    pub check: CheckName,
    pub result: Verdict,
    pub exit_code: Option<i32>,
    pub command: Vec<String>,
    /// The directory the command ran in, relative to the repository root: `""` at the root.
    pub cwd: String,
    pub scope: ScopeName,
    pub digest: Digest,
    /// \[ADR 0037\] What the scope declares outside the repository, part of the key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external: Vec<ExternalInput>,
    /// The commit checked out, when there was one. It informs; the digest decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// `host/owner/name`, for the dashboard to tell repositories apart. It informs too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    pub started_at: Time,
    /// How long the command ran; for a skip, how long hushrun took.
    pub duration_ms: u64,
    /// A skip's: how long the run it stood in for took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_ms: Option<u64>,
    /// Where it ran: `local`, or the CI provider.
    pub provider: Provider,
    /// \[ADR 0044\] The CI job that ran it and the system it ran on, `<job>@<platform>`, when CI
    /// names a job: whose runs its pass stands in for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    /// \[ADR 0049\] Which variant of its job it was, when the job has several and says which
    /// (`HUSHRUN_VARIANT`): its pass stands in for that variant's runs only.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "variant"
    )]
    pub variant: Option<Variant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_url: Option<String>,
    /// The hushrun that recorded it.
    pub tool: String,
    /// Who ran it, from `HUSHRUN_INVOKED_BY` (`agent:<id>` or `human:<id>`): a label that tells
    /// a fleet's runs apart, when they share a token. It informs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoked_by: Option<String>,
    /// \[ADR 0045\] Where it ran. It informs, and a result from before this has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<RunEnvironment>,
    /// \[ADR 0048\] The summary of the test and coverage reports the run wrote (C-27); a skip's is
    /// the one of the pass it stood in for.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "summary"
    )]
    pub reports: Option<hushrun_reports::Reports>,
    /// Why it ran although a pass of its key would have skipped it: the one rule that stopped the
    /// skip, a [`RanAnyway`]'s word, or another client's. Left out when no such pass was found,
    /// and of a skip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_anyway: Option<String>,
    /// \[ADR 0068\] A run's test files, when its check's `tests` isn't off (C-10, C-33): how many
    /// it had, ran and reused, and \[ADR 0069\] why none was reused when something stopped it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "unit_counts"
    )]
    pub units: Option<crate::units::Counts>,
}

/// Why a run ran although a pass of its key would have skipped it (`ranAnyway`): what the
/// dashboard counts as runs that could have skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RanAnyway {
    /// \[ADR 0044\] The job has several variants, and the run doesn't say which it is.
    Variants,
    /// \[ADR 0049\] The job is a called workflow's, and the run can't tell which.
    Reusable,
    /// \[ADR 0047\] A later step of the job reads the check's reports (C-26).
    Reports,
    /// The pass doesn't count in this job: another job's, another variant's, or from outside CI
    /// in a job with variants (C-10).
    Rule,
    /// `--force`.
    Force,
    /// \[ADR 0044\] It runs inside another run, in a job with several variants.
    InsideRun,
    /// hushrun can't read the job.
    CantTell,
}

impl RanAnyway {
    /// The word a record says.
    pub fn word(self) -> &'static str {
        match self {
            RanAnyway::Variants => "variants",
            RanAnyway::Reusable => "reusable",
            RanAnyway::Reports => "reports",
            RanAnyway::Rule => "rule",
            RanAnyway::Force => "force",
            RanAnyway::InsideRun => "inside-run",
            RanAnyway::CantTell => "cant-tell",
        }
    }
}

/// \[ADR 0049\] A run's variant of its job (C-14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variant {
    /// `sha256:` of what tells the variant apart, as JCS writes it: what a lookup asks for.
    pub key: String,
    /// Its values, for people to read: at most 100 characters.
    pub label: String,
}

/// A result's `variant`, or none when it doesn't read as one, as another client's might not.
fn variant<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Variant>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// A result's `reports`, or none when it doesn't read as a summary, as another client's might
/// not: readers take what they know of a result (C-10), and a summary never stops a lookup.
fn summary<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<hushrun_reports::Reports>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// A result's `units`, or none when they don't read, as another client's might not.
fn unit_counts<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<crate::units::Counts>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

impl RunResult {
    /// \[ADR 0044\] Whether this pass stands in for a run in the CI job `job` (C-10), `None` for a
    /// run no CI names a job for. A CI pass belongs to its job, so a job is never skipped on what
    /// another job ran on another system. A pass from before results named their job can't say
    /// whose it is, but on a CI that names none. \[ADR 0049\] In a job of several variants, a
    /// run of one, `variant` its key, counts only its own variant's CI passes: no pass from
    /// outside CI ran as any one of them (D3).
    pub fn counts_in(&self, job: Option<&str>, variant: Option<&str>) -> bool {
        let ours = self.variant.as_ref().map(|variant| variant.key.as_str());
        match (job, self.provider, self.job.as_deref()) {
            (None, ..) => true,
            (_, Provider::Local, _) => variant.is_none(),
            (Some(job), _, Some(recorded)) => recorded == job && ours == variant,
            (Some(_), provider, None) => provider == Provider::Generic && variant.is_none(),
        }
    }

    /// When the run finished.
    pub fn finished_at(&self) -> Time {
        let duration = i64::try_from(self.duration_ms).unwrap_or(i64::MAX);
        Time::from_millis(self.started_at.millis().saturating_add(duration))
            .unwrap_or(self.started_at)
    }
}

/// The prefix of an agent token, which an org admin creates for agents in sandboxes and for CI
/// outside GitHub (ADR 0027).
pub const AGENT_TOKEN_PREFIX: &str = hushrun_core::agent::TOKEN_PREFIX;

/// The prefix of personal tokens that `hushrun login` saves.
pub const USER_TOKEN_PREFIX: &str = "hru_";

/// What a person whose results no registered machine can sign does (C-18).
pub const PERSON_NEEDS_A_MACHINE: &str = "this machine isn't registered: run `hushrun login`";
/// \[ADR 0060\] What a CI job whose personal token no key it names can sign for does (C-18).
pub const CI_NEEDS_A_NAMED_KEY: &str = "in CI, a personal token's results are signed with the key the job names: set HUSHRUN_MACHINE_KEY or HUSHRUN_MACHINE_KEY_FILE";
/// What an agent whose results its token's key doesn't sign does (C-17, C-18).
pub const AGENT_NEEDS_ITS_KEY: &str =
    "set `HUSHRUN_MACHINE_KEY` to the key `hushrun token create --agent` printed";
/// \[ADR 0046\] What an agent whose string holds a key that can't sign does: `HUSHRUN_MACHINE_KEY`
/// wouldn't help, since the string wins over it.
pub const STRING_NEEDS_REPLACING: &str = "HUSHRUN_AGENT's key can't sign for its token: make a new string with `hushrun token create --agent`";

/// Where results are kept.
pub struct Backend {
    store: Store,
}

enum Store {
    /// A directory only this machine reads, where it is, and \[ADR 0067\] why results stay there.
    File(FileLedger, PathBuf, Local),
    Server(Box<Server>),
}

/// A server that speaks `hushrun-results/v0`, at an org's URL.
struct Server {
    ledger: HttpLedger,
    base: String,
    named_by: NamedBy,
    /// What goes with its requests.
    credential: Credential,
    /// `HUSHRUN_TOKEN` is set, and this backend doesn't get it.
    withheld: bool,
    /// Why the server last refused a request, when it was for the caller (401, 403) or for
    /// writing too fast (429).
    refusal: RefCell<Option<Refusal>>,
    /// \[ADR 0028\] The personal or agent token the requests carry, to register this machine
    /// with.
    token: Option<String>,
    /// The key id the login's sign-in registered, when it did.
    signed_in_machine: Option<String>,
    /// The machine that signs its results, once found: `None` when nobody signs (CI, another
    /// server's token); an error, what to do, when a person or an agent has no key to sign with.
    machine: OnceCell<Result<Option<crate::machine::Machine>, String>>,
    /// \[ADR 0060\] Whether this is CI, where no login goes, and a person's token signs with the
    /// key the job names only.
    in_ci: bool,
    /// Whether this is GitHub Actions, where a build credential is the job's ID token or none
    /// (B-31).
    in_github: bool,
    /// \[ADR 0052\] With no credential in GitHub Actions, the repository its lookups ask as, and
    /// then it records nothing.
    reads_as: Option<String>,
    /// Whether such a lookup was answered.
    answered: Cell<bool>,
    /// In GitHub Actions: the ID token endpoint and the request token (B-31).
    oidc: Option<(String, String)>,
    /// \[C-38\] It answered 404 or 405 to a request about hints: a server from before them, which
    /// this run asks nothing more of them, and says so once.
    keeps_no_hints: Cell<bool>,
}

/// Who signs the results a server records (ADR 0028).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signer {
    /// A personal token: this machine, registered at sign-in.
    Person,
    /// An agent token: the key made with it, which the environment names.
    Agent,
    /// CI's ID token, no token, or a token that isn't hushrun's.
    Nobody,
}

/// The credential a server's requests carry (C-13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// `HUSHRUN_TOKEN`; `agent` when it is an agent token.
    Token { agent: bool },
    /// \[ADR 0046\] The agent token in `HUSHRUN_AGENT`.
    Agent,
    /// The personal token `hushrun login` saved for the backend's origin, and its email.
    Login { email: String },
    /// A GitHub Actions ID token whose audience is the backend.
    GithubOidc,
    /// No token at all.
    Nothing,
}

impl Credential {
    /// How a line names it: `acme refused HUSHRUN_TOKEN`.
    pub fn named(&self) -> &'static str {
        match self {
            Credential::Token { .. } => "HUSHRUN_TOKEN",
            Credential::Agent => hushrun_core::agent::VARIABLE,
            Credential::Login { .. } => "your login",
            Credential::GithubOidc => "this job's ID token",
            Credential::Nothing => "a request without a token",
        }
    }
}

/// Why a server refused a request, as `hushrun run` tells it (C-14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// It carried no token, and the server wants one (401).
    NoToken(String),
    /// It refused the credential the request carried (401 or 403).
    Credential(String),
    /// The token wrote more than its limit allows (429).
    Limited(String),
    /// \[ADR 0032\] The org's plan records no more checks this period (402 `quota_exceeded`).
    Quota(Box<Quota>),
    /// \[ADR 0028\] A person's or an agent's result that no machine could sign, so it wasn't
    /// sent, or that the server refused for its machine (401 `machine_required`,
    /// `machine_unknown`): what to do.
    Machine(String),
    /// \[ADR 0048\] A result the server found invalid (400 `invalid`), in its words: the CLI
    /// sends none, but another server may judge a field, such as a summary (C-27), otherwise.
    Invalid(String),
}

/// \[ADR 0032\] What a backend says of the org's usage (C-19).
#[derive(Debug, Clone)]
pub enum Usage {
    /// Results stay in a directory on this machine: nothing is metered.
    Directory,
    /// The server's answer to `GET {base}/v0/usage`.
    Reported(serde_json::Value),
    /// The server doesn't report usage: it answered this status, 404 or 405.
    Unreported(u16),
}

/// Where results go, as `hushrun whoami` tells it.
#[derive(Debug, Clone)]
pub enum Place {
    /// A directory only this machine reads, and \[ADR 0067\] why results stay there.
    Directory { dir: PathBuf, local: Local },
    /// A server.
    Server {
        base: String,
        /// What named it (C-11).
        named_by: NamedBy,
        credential: Credential,
        /// `HUSHRUN_TOKEN` is set, and this backend doesn't get it.
        withheld: bool,
    },
}

/// What named the backend (C-11). It decides which tokens go with requests (C-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedBy {
    /// `HUSHRUN_BACKEND`, which lives outside the code, with the CI's other settings.
    Environment,
    /// \[ADR 0046\] The backend in `HUSHRUN_AGENT`, as much the environment's.
    Agent,
    /// `"hushrun": {"backend": …}` in the `package.json` at the repository's root.
    Repository,
    /// \[ADR 0037\] `backend:` in the `hushrun.yaml` at the repository's root. It is a file in
    /// the repository too, so it gets what package.json's gets, and nothing more (C-13).
    Policy,
    /// `hushrun login`.
    Login,
}

impl NamedBy {
    pub fn source(self) -> &'static str {
        match self {
            NamedBy::Environment => "HUSHRUN_BACKEND",
            NamedBy::Agent => hushrun_core::agent::VARIABLE,
            NamedBy::Repository => "package.json's hushrun.backend",
            NamedBy::Policy => "hushrun.yaml's backend",
            NamedBy::Login => "the backend hushrun login saved",
        }
    }
}

/// \[ADR 0067\] Why results stay on this machine (C-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Local {
    /// Nothing names a server: no variable, no file of the repository, no login.
    Unnamed,
    /// The repository's files name `base`, `named_by` package.json or hushrun.yaml, and nothing
    /// here signs in to it: no login for it, no token that goes there, no ID token, outside CI.
    SignedOut { base: String, named_by: NamedBy },
}

impl Local {
    /// The org the repository names, `acme` in `https://host/o/acme`, else its URL; `None` when it
    /// names none.
    pub fn org(&self) -> Option<String> {
        match self {
            Local::Unnamed => None,
            Local::SignedOut { base, .. } => {
                Some(hosted::org_of(base).unwrap_or_else(|| base.clone()))
            }
        }
    }
}

/// Why results are off (C-11): nothing is looked up, and nothing is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Off {
    /// `HUSHRUN_BACKEND` is set, and empty.
    EmptyBackend,
    /// There is no cache directory for this machine's results.
    NoCache,
    /// \[ADR 0067\] In CI, and nothing names a server: a runner's own directory is shared with no
    /// one, and whoever can write a cache CI restores could forge what it holds.
    InCi,
}

impl Off {
    /// Why, as a line ends with it.
    pub fn reason(self) -> &'static str {
        match self {
            Off::EmptyBackend => "HUSHRUN_BACKEND is empty",
            Off::NoCache => "there is no cache directory (HUSHRUN_CACHE_DIR is empty)",
            Off::InCi => {
                "in CI, nothing names a backend, and a runner's own directory isn't shared: run hushrun init to share results with CI, or set HUSHRUN_BACKEND"
            }
        }
    }
}

/// \[ADR 0067\] Where results go, as [`Backend::select`] decides it, the one place that does
/// (C-11): a server's, shared; this machine's directory, local; or nowhere, off. Every command
/// that says where results go reads it from here.
pub enum Selection {
    /// A server, or this machine's directory: [`Backend::place`] says which.
    On(Backend),
    /// Results are off.
    Off(Off),
}

/// Where this machine keeps its results, `<cache dir>/results` (C-11), whether or not a backend
/// sends them elsewhere; `None` without a cache directory.
pub fn local_directory() -> Option<PathBuf> {
    crate::context::cache_dir().map(|cache| cache.join("results"))
}

impl Backend {
    /// Where results go (C-11): \[ADR 0046\] the backend in `HUSHRUN_AGENT`; `HUSHRUN_BACKEND`;
    /// else the backend the `package.json` or the `hushrun.yaml` at the repository's `root` names;
    /// else the one `hushrun login` signed in to; else `<cache dir>/results`. Off when
    /// `HUSHRUN_BACKEND` is empty, or there is no cache directory. \[ADR 0067\] The repository's
    /// backend with no credential at all for it, outside CI, is this machine's directory too, and
    /// in CI the directory is off.
    pub fn select(root: Option<&Path>) -> anyhow::Result<Selection> {
        let env = Env::from_process();
        let in_ci = hushrun_ci::in_ci(&env);
        match hushrun_ci::var("HUSHRUN_BACKEND") {
            Ok(text) if text.trim().is_empty() => return Ok(Selection::Off(Off::EmptyBackend)),
            Ok(text) => {
                let named_by = match env.agent() {
                    Some(_) => NamedBy::Agent,
                    None => NamedBy::Environment,
                };
                return Backend::server(text.trim(), named_by).map(Selection::On);
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(err) => return Err(anyhow!("HUSHRUN_BACKEND: {err}")),
        }
        if let Some((url, named_by)) = root.and_then(hosted::named_backend) {
            let server = Backend::server(&url, named_by)?;
            // Nothing here could share a result with that server, and asking it would only be
            // refused: this machine keeps them, until someone signs in. Never in CI, never for
            // an agent, and never beside a token, however it fares.
            let signed_out = !in_ci
                && !env.agent_set()
                && matches!(&server.store, Store::Server(s) if s.signed_out());
            if let (true, Some(dir)) = (signed_out, local_directory()) {
                let base = match &server.store {
                    Store::Server(s) => s.base.clone(),
                    Store::File(..) => url,
                };
                return Backend::directory(dir, Local::SignedOut { base, named_by })
                    .map(Selection::On);
            }
            return Ok(Selection::On(server));
        }
        if let Some(url) = Credentials::load()?.backend {
            return Backend::server(&url, NamedBy::Login).map(Selection::On);
        }
        if in_ci {
            return Ok(Selection::Off(Off::InCi));
        }
        match local_directory() {
            Some(dir) => Backend::directory(dir, Local::Unnamed).map(Selection::On),
            None => Ok(Selection::Off(Off::NoCache)),
        }
    }

    /// \[ADR 0041\] Registers this machine in the org at `base`, for the login there, unless it
    /// is registered: `init` does, for an org no sign-in was approved for, since signed events
    /// need it as much as results do. Says what to do when the server refuses. An agent token in
    /// the environment takes the login's place here, and its machine is the one made with it.
    pub fn register_machine(base: &str) -> Result<(), String> {
        let backend = Backend::server(base, NamedBy::Login).map_err(|e| format!("{e:#}"))?;
        match &backend.store {
            Store::Server(server) if matches!(server.signer(), Signer::Person) => {
                server.machine().map(|_| ())
            }
            _ => Ok(()),
        }
    }

    fn directory(dir: PathBuf, local: Local) -> anyhow::Result<Backend> {
        // A missing directory is an empty backend: the most it costs is a run.
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Backend {
            store: Store::File(FileLedger::new(dir.clone()), dir, local),
        })
    }

    /// \[ADR 0067\] Forgets what this machine's directory holds of runs that finished 30 days or
    /// more before `now`, the command's clock, at most once a day. It never fails, and a server
    /// keeps its own: what goes wrong is only logged.
    pub fn sweep(&self, now: Time) {
        if let Store::File(_, dir, _) = &self.store {
            sweep(dir, now);
        }
    }

    /// \[ADR 0067\] Why results stay on this machine, when they do.
    pub fn local(&self) -> Option<&Local> {
        match &self.store {
            Store::File(_, _, local) => Some(local),
            Store::Server(_) => None,
        }
    }

    fn server(text: &str, named_by: NamedBy) -> anyhow::Result<Backend> {
        let url = LedgerUrl::parse(text).map_err(|e| anyhow!("{}: {e}", named_by.source()))?;
        let var = |name: &str| {
            hushrun_ci::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        // A CI secret goes only where the CI's own settings send it: the repository's files
        // could name any server. An agent token also goes to hushrun's host, which the
        // environment names (HUSHRUN_HOST) and the server binds the token's org at (ADR 0027).
        let env = Env::from_process();
        // \[ADR 0046\] Beside a string hushrun can't read, an agent runs here, and a personal token
        // doesn't speak for it, as no login does (`Credentials::login_at`).
        let given = var("HUSHRUN_TOKEN")
            .filter(|token| !(env.agent_unreadable() && token.starts_with(USER_TOKEN_PREFIX)));
        let agent = given.as_deref().is_some_and(is_agent_token);
        let sends = named_by == NamedBy::Environment
            || (agent && hosted::agent_token_goes_to(&env, &crate::credentials::origin(&url)));
        // \[ADR 0060\] CI takes no person's login, as the repository's ledger doesn't (C-13): on a
        // runner where someone signed in, a job's results aren't that person's.
        let in_ci = hushrun_ci::is_ci(hushrun_ci::detect(&env));
        let in_github = hushrun_ci::detect(&env) == Provider::Github;
        // GitHub sets both when the job has `permissions: id-token: write`. The ID token names
        // this URL as its audience, so no other server can use it.
        let oidc = match (
            var("ACTIONS_ID_TOKEN_REQUEST_URL"),
            var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
        ) {
            (Some(url), Some(token)) => Some((url, token)),
            _ => None,
        };
        // C-13, B-31: in GitHub Actions the job's ID token, when it has one, comes before any
        // token the environment holds, as the repository's ledger takes it (L-10): what the
        // backend answers, `Hushrun-Build` included, is an answer to the job itself.
        let id_token_first = in_github && oidc.is_some();
        let withheld = given.is_some() && !sends && !id_token_first;
        let mut signed_in_machine = None;
        let (token, mut credential) = match given.filter(|_| sends && !id_token_first) {
            Some(token) if env.agent().is_some() => (Some(token), Credential::Agent),
            Some(token) => (Some(token), Credential::Token { agent }),
            None if in_ci => (None, Credential::Nothing),
            None => match Credentials::load()?.login_for(&url) {
                Some(login) => {
                    // The key its sign-in registered counts in the org of that sign-in only.
                    if login.machine_org.is_some()
                        && login.machine_org == hosted::org_of(&url.to_string())
                    {
                        signed_in_machine.clone_from(&login.machine);
                    }
                    (
                        Some(login.token.clone()),
                        Credential::Login {
                            email: login.email.clone(),
                        },
                    )
                }
                None => (None, Credential::Nothing),
            },
        };
        let github_oidc = oidc.clone().map(|(url, token)| OidcEndpoint {
            url,
            token: Secret::new(token),
        });
        if credential == Credential::Nothing && github_oidc.is_some() {
            credential = Credential::GithubOidc;
        }
        // \[ADR 0052\] Nothing to send, in GitHub Actions, as in a pull request from a fork.
        let reads_as = (credential == Credential::Nothing)
            .then(|| hushrun_ci::github::asks_as(&env))
            .flatten();
        let options = Options {
            budget: Some(SERVER_BUDGET),
            token: token.clone().map(Secret::new),
            token_command: None,
            github_oidc,
        };
        Ok(Backend {
            store: Store::Server(Box::new(Server {
                ledger: HttpLedger::open(&url, &options).map_err(plain)?,
                base: url.to_string(),
                named_by,
                credential,
                withheld,
                refusal: RefCell::new(None),
                token,
                signed_in_machine,
                machine: OnceCell::new(),
                in_ci,
                in_github,
                reads_as,
                answered: Cell::new(false),
                oidc,
                keeps_no_hints: Cell::new(false),
            })),
        })
    }

    /// The backend's URL, when `HUSHRUN_TOKEN` is set but doesn't go there: only the backend
    /// `HUSHRUN_BACKEND` names gets it, and an agent token also one on hushrun's host (C-13).
    pub fn withheld_token(&self) -> Option<&str> {
        match &self.store {
            Store::Server(server) if server.withheld => Some(&server.base),
            _ => None,
        }
    }

    /// Where results go, and with which credential.
    pub fn place(&self) -> Place {
        match &self.store {
            Store::File(_, dir, local) => Place::Directory {
                dir: dir.clone(),
                local: local.clone(),
            },
            Store::Server(server) => Place::Server {
                base: server.base.clone(),
                named_by: server.named_by,
                credential: server.credential.clone(),
                withheld: server.withheld,
            },
        }
    }

    /// The org a server backend is, `acme` in `https://host/o/acme`, else its URL; `None` for
    /// a directory.
    pub fn org(&self) -> Option<String> {
        match &self.store {
            Store::Server(server) => {
                Some(hosted::org_of(&server.base).unwrap_or_else(|| server.base.clone()))
            }
            Store::File(..) => None,
        }
    }

    /// \[ADR 0052\] Whether this backend only reads: a server, with no credential, in GitHub
    /// Actions, as in a pull request from a fork. Its lookups ask as the repository's CI, and it
    /// records nothing.
    pub fn reads_only(&self) -> bool {
        matches!(&self.store, Store::Server(server) if server.reads_as.is_some())
    }

    /// Whether a lookup of a backend that [only reads](Backend::reads_only) was answered.
    pub fn answered_without_a_token(&self) -> bool {
        matches!(&self.store, Store::Server(server) if server.answered.get())
    }

    /// Why the server last refused a request for who sent it, or for writing too fast.
    pub fn refusal(&self) -> Option<Refusal> {
        match &self.store {
            Store::Server(server) => server.refusal.borrow().clone(),
            Store::File(..) => None,
        }
    }

    /// The credential a server backend's requests carry.
    pub fn credential(&self) -> Option<&Credential> {
        match &self.store {
            Store::Server(server) => Some(&server.credential),
            Store::File(..) => None,
        }
    }

    /// Whether a person at a terminal can fix a refusal for want of a token by signing in: the
    /// backend is the repository's or the login's, not one the environment named (C-11), and
    /// \[ADR 0046\] no `HUSHRUN_AGENT` refuses the sign-in.
    pub fn sign_in_helps(&self) -> bool {
        let named_here = match &self.store {
            Store::Server(server) => server.named_by != NamedBy::Environment,
            // \[ADR 0067\] The repository names a backend that nobody here signed in to.
            Store::File(_, _, local) => matches!(local, Local::SignedOut { .. }),
        };
        named_here && !Env::from_process().agent_set()
    }

    /// `GET {base}/v0`: the org, who the credential is to it, and, for an agent token, the
    /// token (C-12). `None` for a directory.
    pub fn info(&self) -> anyhow::Result<Option<serde_json::Value>> {
        let Store::Server(server) = &self.store else {
            return Ok(None);
        };
        let (status, body) = server
            .ledger
            .json_request("GET", "v0", None)
            .map_err(plain)?;
        if status != 200 {
            return Err(server.refused(status, &body, "checking access"));
        }
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| anyhow!("{}: the answer isn't JSON: {e}", server.base))
    }

    /// \[ADR 0032\] `GET {base}/v0/usage`: the org's plan and what it recorded this period
    /// (C-12). A directory meters nothing; a server without the request answers 404 or 405. A
    /// server that can't be reached, refuses the credential or answers anything else is an
    /// error, in its own words.
    pub fn usage(&self) -> anyhow::Result<Usage> {
        let server = match &self.store {
            Store::File(..) => return Ok(Usage::Directory),
            Store::Server(server) => server,
        };
        let (status, body) = server
            .ledger
            .json_request("GET", "v0/usage", None)
            .map_err(plain)?;
        match status {
            200 => serde_json::from_slice(&body)
                .map(Usage::Reported)
                .map_err(|e| anyhow!("{}: the usage answer isn't JSON: {e}", server.base)),
            404 | 405 => Ok(Usage::Unreported(status)),
            _ => Err(server.refused(status, &body, "reading the org's usage")),
        }
    }

    /// Why a result recorded now would be refused for want of a machine to sign it, when this
    /// machine can tell (C-18). \[ADR 0060\] It sends nothing and registers nothing, so that
    /// `hushrun ready` and the pre-push hook spend no machine of the person's in the org: the
    /// first result registers it.
    pub fn cannot_sign(&self) -> Option<String> {
        match &self.store {
            Store::Server(server) => match server.machine.get() {
                Some(found) => found.as_ref().err().cloned(),
                None => server.key().err(),
            },
            Store::File(..) => None,
        }
    }

    /// \[ADR 0049\] The newest result recorded for `key` that this hushrun can read and that counts
    /// in the CI job `job`, of the variant `variant` (C-10): `None` outside CI. A server from
    /// before the query answers the newest for the key, which [`RunResult::counts_in`] checks
    /// again: a pass of another job then runs the check, as before.
    pub fn latest_for_job(
        &self,
        key: &Digest,
        job: Option<&str>,
        variant: Option<&str>,
    ) -> anyhow::Result<Latest> {
        let Some(job) = job else {
            return self.latest(key);
        };
        let mut query = format!("job={}", query_value(job));
        if let Some(variant) = variant {
            query.push_str(&format!("&variant={}", query_value(variant)));
        }
        self.lookup(key, Some(query), true, &|result| {
            result.counts_in(Some(job), variant)
        })
    }

    /// The newest result recorded for `key` that this hushrun can read, whatever job recorded it.
    pub fn latest(&self, key: &Digest) -> anyhow::Result<Latest> {
        self.lookup(key, None, false, &|_| true)
    }

    /// \[ADR 0037\] What `repository`'s CI would find for `key`, asked from anywhere: a server
    /// answers from CI's results only, with `ci_only`, when the repository's CI reuses nothing
    /// else (C-12). A server from before this asks nothing of the query, and answers as to anyone.
    pub fn latest_for_ci(&self, key: &Digest, repository: Option<&str>) -> anyhow::Result<Latest> {
        let query =
            repository.map(|repository| format!("as=ci&repository={}", query_value(repository)));
        self.lookup(key, query, false, &|_| true)
    }

    /// The newest result for `key`, asked with `query`; in a directory, the newest of those that
    /// `counts` takes, as a server answers the query, `for_job` when it asks for a job's.
    fn lookup(
        &self,
        key: &Digest,
        query: Option<String>,
        for_job: bool,
        counts: &dyn Fn(&RunResult) -> bool,
    ) -> anyhow::Result<Latest> {
        let ledger = match &self.store {
            Store::File(ledger, ..) => ledger,
            Store::Server(server) => {
                let path = format!("v0/results/{}", key.hex());
                // \[ADR 0052\] With no credential, as the repository's CI, unless it asks so already.
                let query = match (&server.reads_as, query) {
                    (Some(repository), query)
                        if !query.as_deref().is_some_and(|q| q.starts_with("as=ci&")) =>
                    {
                        let asked = format!("as=ci&repository={}", query_value(repository));
                        Some(match query {
                            Some(query) => format!("{asked}&{query}"),
                            None => asked,
                        })
                    }
                    (_, query) => query,
                };
                let (status, body) = match &query {
                    Some(query) => server.ledger.json_get_query(&path, query),
                    None => server.ledger.json_request("GET", &path, None),
                }
                .map_err(plain)?;
                if status != 200 {
                    return Err(server.refused(status, &body, "reading the newest result"));
                }
                if server.reads_as.is_some() {
                    server.answered.set(true);
                }
                #[derive(Deserialize)]
                struct Newest {
                    result: Option<serde_json::Value>,
                    #[serde(default, rename = "ciOnly")]
                    ci_only: bool,
                    #[serde(default, rename = "recordedBy")]
                    recorded_by: Option<String>,
                    #[serde(default, rename = "forJob")]
                    for_job: bool,
                }
                let newest: Newest = serde_json::from_slice(&body)
                    .map_err(|e| anyhow!("{}: the answer isn't a result: {e}", server.base))?;
                return Ok(Latest {
                    result: newest
                        .result
                        .and_then(|value| serde_json::from_value::<RunResult>(value).ok())
                        .filter(|result| {
                            result.schema == RESULT_SCHEMA && result.result != Verdict::Skip
                        }),
                    ci_only: newest.ci_only,
                    recorded_by: newest.recorded_by,
                    for_job: newest.for_job,
                });
            }
        };
        let objects = ledger
            .list(&format!("{PREFIX}/{}/", key.hex()))
            .map_err(plain)?;
        for object in objects.iter().rev().take(LOOKUP_LIMIT) {
            let parsed = ledger
                .get(object)
                .map(|bytes| serde_json::from_slice::<RunResult>(&bytes));
            match parsed {
                Ok(Ok(result))
                    if result.schema == RESULT_SCHEMA
                        && result.result != Verdict::Skip
                        && counts(&result) =>
                {
                    return Ok(Latest {
                        result: Some(result),
                        ci_only: false,
                        recorded_by: None,
                        for_job,
                    });
                }
                Ok(Ok(result)) => tracing::debug!(
                    "{object}: no answer here: {} {:?} in {:?}",
                    result.schema,
                    result.result,
                    result.job
                ),
                Ok(Err(err)) => tracing::debug!("{object}: {err}"),
                Err(err) => tracing::debug!("{object}: {err}"),
            }
        }
        Ok(Latest {
            for_job,
            ..Latest::default()
        })
    }

    /// Records `result` under `key`.
    pub fn record(&self, key: &Digest, result: &RunResult) -> anyhow::Result<()> {
        match &self.store {
            Store::File(ledger, ..) => {
                let name = hushrun_ledger::new_ulid(result.finished_at().millis());
                let prefix = if result.result == Verdict::Skip {
                    SKIP_PREFIX
                } else {
                    PREFIX
                };
                let object = format!("{prefix}/{}/{name}.json", key.hex());
                let mut bytes = serde_json::to_vec_pretty(result)?;
                bytes.push(b'\n');
                ledger.append(&object, &bytes).map_err(plain)?;
            }
            Store::Server(server) => {
                // \[ADR 0052\] With no credential, nothing is sent: the server would refuse it.
                if server.reads_as.is_some() {
                    return Err(anyhow!("{}: not recorded: {NO_ID_TOKEN}", server.base));
                }
                let path = format!("v0/results/{}", key.hex());
                // \[ADR 0069\] `units`, the run's counts, goes with it (C-10).
                let body = serde_json::to_vec(result)?;
                // ADR 0028: a person's or an agent's result carries this machine's signature,
                // and without a key to sign with it isn't sent.
                let machine = server.machine().map_err(|why| {
                    *server.refusal.borrow_mut() = Some(Refusal::Machine(why.clone()));
                    anyhow!("{}: not recorded: {why}", server.base)
                })?;
                let signature = machine.map(|machine| {
                    (
                        "Hushrun-Signature",
                        machine.result_signature(&key.hex(), &body),
                    )
                });
                let (status, answer) = server
                    .ledger
                    .json_request_with("POST", &path, Some(&body), signature)
                    .map_err(plain)?;
                if !(200..300).contains(&status) {
                    let err = server.refused(status, &answer, "recording the result");
                    if status == 400 && error_code(&answer).as_deref() == Some("invalid") {
                        *server.refusal.borrow_mut() = Some(Refusal::Invalid(
                            error_message(&answer).unwrap_or_else(|| "400 invalid".to_string()),
                        ));
                    }
                    return Err(err);
                }
            }
        }
        Ok(())
    }
}

/// What a run's relay needs to talk to an org's shared store of build blocks (the build protocol, B-26):
/// where, as whom, and whether the org said it shares.
pub struct BuildAccess {
    /// The org's base URL.
    pub base: String,
    /// The backend's origin.
    pub host: String,
    /// The org, as in the URL.
    pub org: String,
    /// What the requests carry: a person's or an agent's token, or in GitHub Actions the job's
    /// ID token, minted for each request as it nears its expiry. `None`: nothing to send (CI
    /// with no ID token), and the run shares nothing.
    pub credential: Option<BuildCredential>,
    /// Whether the token is an agent token.
    pub agent: bool,
    /// The machine that signs a `begin`, when one is here. It is not registered from here: the
    /// results a run records do that, and a `begin` before it is refused and waits.
    pub machine: Option<crate::machine::Machine>,
    /// Whether an answer of this run carried `Hushrun-Build: on`.
    pub shares: bool,
    /// Whether the server answered any request of this run: without, whether the org shares is
    /// unknown, which a CI job says (B-2).
    pub answered: bool,
}

/// What a relay's requests carry (B-26, B-31).
#[derive(Clone)]
pub enum BuildCredential {
    /// A person's or an agent's token.
    Token(String),
    /// GitHub Actions: where the job mints ID tokens, the request token that asks, and the
    /// job's digest (`cmd::build_session::github_job`), which its fingerprint is made of.
    GithubOidc {
        url: String,
        request_token: String,
        job: String,
    },
}

impl Backend {
    /// What a relay needs (B-26, B-31): for a server, the credential its build requests carry.
    /// In CI that is the job's ID token and nothing else, also beside an agent token (B-31); in
    /// GitHub Actions with none, the credential is `None`, whatever token the job holds; in other
    /// CI with none, an agent token, which reads only. Elsewhere a person's or an agent's token; with
    /// none, no access. `None` for a directory.
    pub fn build_access(&self) -> Option<BuildAccess> {
        let Store::Server(server) = &self.store else {
            return None;
        };
        let url = LedgerUrl::parse(&server.base).ok()?;
        let agent_token = matches!(server.signer(), Signer::Agent);
        let (credential, agent) = if server.in_ci {
            match (server.oidc.clone(), server.token.clone()) {
                (Some((url, request_token)), _) => {
                    let job =
                        crate::cmd::build_session::github_job(&|name| hushrun_ci::var_os(name))
                            .map_or_else(
                                || hushrun_build::origin::fingerprint(&request_token),
                                |job| job.digest,
                            );
                    (
                        Some(BuildCredential::GithubOidc {
                            url,
                            request_token,
                            job,
                        }),
                        false,
                    )
                }
                // In GitHub Actions without an ID token, nothing: whatever else the job holds,
                // sharing is off there (B-31).
                (None, _) if server.in_github => (None, false),
                // Anywhere else in CI an agent token reads the org's blocks, and writes none
                // (`hushrun_build::share::what_a_run_does`).
                (None, Some(token)) if agent_token => (Some(BuildCredential::Token(token)), true),
                (None, _) => (None, false),
            }
        } else {
            let token = server.token.clone()?;
            (Some(BuildCredential::Token(token)), agent_token)
        };
        Some(BuildAccess {
            base: server.base.clone(),
            host: crate::credentials::origin(&url),
            org: hosted::org_of(&server.base).unwrap_or_else(|| server.base.clone()),
            credential,
            agent,
            machine: if server.in_ci {
                None
            } else {
                server.key().ok().flatten()
            },
            shares: server.ledger.build_sharing_seen(),
            answered: server.ledger.answered_any(),
        })
    }
}

/// \[ADR 0069\] What a server answered one of the requests about a check's test files (results-v0
/// §3).
#[derive(Debug)]
pub enum UnitsCall {
    /// 200, and its body.
    Answered(serde_json::Value),
    /// 409, and its body: a stale write, with what the state is now.
    Conflict(serde_json::Value),
    /// 404 or 405: a server from before test files' results.
    Unsupported,
    /// Anything else but a server's failure (5xx, 429), which is an error, in the server's words.
    Refused(String),
}

impl Backend {
    /// \[ADR 0069\] `POST {base}/v0/units/<what>` with `body`: the batch lookup (`lookup`), or a
    /// check's reuse state (`reuse`). A directory, or a server asked with no credential, answers
    /// nothing (an error); a server that can't be reached is an error too.
    pub fn units_request(&self, what: &str, body: &serde_json::Value) -> anyhow::Result<UnitsCall> {
        let Store::Server(server) = &self.store else {
            return Err(anyhow!("this machine's directory has no server to ask"));
        };
        if server.reads_as.is_some() {
            return Err(anyhow!("{}: {NO_ID_TOKEN}", server.base));
        }
        let bytes = serde_json::to_vec(body)?;
        let (status, answer) = server
            .ledger
            .json_request_page("POST", &format!("v0/units/{what}"), Some(&bytes), None)
            .map_err(plain)?;
        // \[ADR 0069\] A server that fails, or asks to be asked later, after L-4's retries: it
        // wasn't reached, so a suspension waits in the outbox rather than being dropped.
        if status == 429 || (500..600).contains(&status) {
            return Err(anyhow!(
                "{}: {}",
                server.base,
                error_message(&answer).unwrap_or_else(|| format!("answered {status}"))
            ));
        }
        Ok(match status {
            200 => UnitsCall::Answered(
                serde_json::from_slice(&answer)
                    .map_err(|e| anyhow!("{}: the answer isn't JSON: {e}", server.base))?,
            ),
            409 => UnitsCall::Conflict(serde_json::from_slice(&answer).unwrap_or_default()),
            404 | 405 => UnitsCall::Unsupported,
            _ => UnitsCall::Refused(
                error_message(&answer)
                    .unwrap_or_else(|| format!("{} answered {status}", server.base)),
            ),
        })
    }

    /// \[ADR 0069\] Whether results go to a server, where a run's test files go with it in one
    /// request ([`Backend::record_with_units`]), rather than this machine's directory.
    pub fn is_server(&self) -> bool {
        matches!(self.store, Store::Server(_))
    }

    /// \[ADR 0069\] Records `result` under `key` with `units`, the results of the test files that
    /// ran, in one request to a server: `POST {base}/v0/runs/{key}` (results-v0 §3), signed as a
    /// result is. A server from before it (404, 405) records the result alone, as
    /// [`Backend::record`] does, and keeps no test file: `Ok(false)`. A directory records the
    /// result; the caller keeps the units beside it.
    pub fn record_with_units(
        &self,
        key: &Digest,
        result: &RunResult,
        units: &[crate::units::store::UnitRecord],
    ) -> anyhow::Result<bool> {
        let server = match &self.store {
            Store::Server(server) if !units.is_empty() => server,
            _ => return self.record(key, result).map(|()| true),
        };
        if server.reads_as.is_some() {
            return Err(anyhow!("{}: not recorded: {NO_ID_TOKEN}", server.base));
        }
        // Keys as the protocol has them: 64 lowercase hex characters.
        let units: Vec<serde_json::Value> = units
            .iter()
            .map(|unit| {
                json!({"key": unit.key.hex(), "result": unit.result, "durationMs": unit.duration_ms})
            })
            .collect();
        let body = serde_json::to_vec(&json!({
            "schema": RUN_UNITS_SCHEMA,
            "result": result,
            "units": units,
        }))?;
        let machine = server.machine().map_err(|why| {
            *server.refusal.borrow_mut() = Some(Refusal::Machine(why.clone()));
            anyhow!("{}: not recorded: {why}", server.base)
        })?;
        let signature = machine.map(|machine| {
            (
                "Hushrun-Signature",
                machine.result_signature(&key.hex(), &body),
            )
        });
        let (status, answer) = server
            .ledger
            .json_request_with(
                "POST",
                &format!("v0/runs/{}", key.hex()),
                Some(&body),
                signature,
            )
            .map_err(plain)?;
        if status == 404 || status == 405 {
            return self.record(key, result).map(|()| false);
        }
        if !(200..300).contains(&status) {
            let err = server.refused(status, &answer, "recording the result");
            if status == 400 && error_code(&answer).as_deref() == Some("invalid") {
                *server.refusal.borrow_mut() = Some(Refusal::Invalid(
                    error_message(&answer).unwrap_or_else(|| "400 invalid".to_string()),
                ));
            }
            return Err(err);
        }
        Ok(true)
    }
}

/// \[ADR 0069\] The schema of a run's result with its test files' results (results-v0 §3).
pub const RUN_UNITS_SCHEMA: &str = "hushrun.run-units/v0";

/// \[C-38\] The most bytes a lookup of hints answers: 3 hints of 2 MiB each, in base64, and room.
const MOST_HINTS_ANSWER: usize = 12 << 20;

impl Backend {
    /// \[C-38\] `POST {base}/v0/units/hints/lookup` with `body` (results-v0, Hints): the newest
    /// hints at a place. A directory, a server asked with no credential, or one that can't be
    /// reached is an error; one that said it keeps no hints this run isn't asked again.
    pub fn hints_lookup(&self, body: &serde_json::Value) -> anyhow::Result<UnitsCall> {
        let server = self.hint_server()?;
        if server.keeps_no_hints.get() {
            return Ok(UnitsCall::Unsupported);
        }
        let bytes = serde_json::to_vec(body)?;
        let (status, answer) = server
            .ledger
            .json_request_sized(
                "POST",
                "v0/units/hints/lookup",
                Some(&bytes),
                MOST_HINTS_ANSWER,
            )
            .map_err(plain)?;
        server.hints_answer(status, &answer)
    }

    /// \[C-38\] `PUT {base}/v0/units/hints/{sha256}?repository=&checkId=&address=&os=` with a
    /// hint's bytes, `query` its four values (results-v0, Hints). Never signed: a hint is never
    /// trusted.
    pub fn hint_put(
        &self,
        sha256: &Digest,
        query: &[(&str, String)],
        bytes: &[u8],
    ) -> anyhow::Result<UnitsCall> {
        let server = self.hint_server()?;
        if server.keeps_no_hints.get() {
            return Ok(UnitsCall::Unsupported);
        }
        let query = query
            .iter()
            .map(|(name, value)| format!("{name}={}", query_value(value)))
            .collect::<Vec<_>>()
            .join("&");
        let (status, answer) = server
            .ledger
            .put_bytes(&format!("v0/units/hints/{}", sha256.hex()), &query, bytes)
            .map_err(plain)?;
        server.hints_answer(status, &answer)
    }

    /// \[C-38\] Whether the server said, this run, that it keeps no hints: what the run says once.
    pub fn keeps_no_hints(&self) -> bool {
        matches!(&self.store, Store::Server(server) if server.keeps_no_hints.get())
    }

    /// The server hints are asked of, which has a credential to ask with.
    fn hint_server(&self) -> anyhow::Result<&Server> {
        let Store::Server(server) = &self.store else {
            return Err(anyhow!("this machine's directory has no server to ask"));
        };
        if server.reads_as.is_some() {
            return Err(anyhow!("{}: {NO_ID_TOKEN}", server.base));
        }
        Ok(server)
    }
}

impl Server {
    /// \[C-38\] What an answer to a request about hints is: as [`Backend::units_request`]'s, and
    /// 404 or 405 marks the server as keeping none, for the rest of the run.
    fn hints_answer(&self, status: u16, answer: &[u8]) -> anyhow::Result<UnitsCall> {
        if status == 429 || (500..600).contains(&status) {
            return Err(anyhow!(
                "{}: {}",
                self.base,
                error_message(answer).unwrap_or_else(|| format!("answered {status}"))
            ));
        }
        Ok(match status {
            200 | 201 => UnitsCall::Answered(
                serde_json::from_slice(answer)
                    .map_err(|e| anyhow!("{}: the answer isn't JSON: {e}", self.base))?,
            ),
            404 | 405 => {
                self.keeps_no_hints.set(true);
                UnitsCall::Unsupported
            }
            _ => UnitsCall::Refused(
                error_message(answer).unwrap_or_else(|| format!("{} answered {status}", self.base)),
            ),
        })
    }

    /// \[ADR 0067\] Whether nothing here could be sent with its requests: no token goes there,
    /// none is withheld from it, no login and no ID token, and it asks as no repository's CI.
    fn signed_out(&self) -> bool {
        self.credential == Credential::Nothing
            && self.token.is_none()
            && !self.withheld
            && self.reads_as.is_none()
    }

    /// Who signs this server's results: a person's token, an agent token, or nobody.
    fn signer(&self) -> Signer {
        let Some(token) = self.token.as_deref() else {
            return Signer::Nobody;
        };
        match self.credential {
            Credential::Login { .. } => Signer::Person,
            Credential::Token { agent: true } | Credential::Agent => Signer::Agent,
            Credential::Token { agent: false } if token.starts_with(USER_TOKEN_PREFIX) => {
                Signer::Person
            }
            _ => Signer::Nobody,
        }
    }

    /// \[ADR 0028\] The machine that signs this server's results, found once: for a person,
    /// this machine's key, registered at sign-in or now with `POST {base}/v0/machines` (C-18);
    /// for an agent, the key the environment names, made with its token. `None` when nobody
    /// signs. An error says what to do when a person or an agent has no key to sign with, or
    /// the server refused to register this machine.
    fn machine(&self) -> Result<Option<&crate::machine::Machine>, String> {
        match self.machine.get_or_init(|| self.find_machine()) {
            Ok(machine) => Ok(machine.as_ref()),
            Err(why) => Err(why.clone()),
        }
    }

    fn find_machine(&self) -> Result<Option<crate::machine::Machine>, String> {
        match (self.signer(), self.key()?) {
            (Signer::Person, Some(machine)) => self.register(machine).map(Some),
            (_, machine) => Ok(machine),
        }
    }

    /// The key that signs this server's results, without registering it: `None` when nobody
    /// signs; an error, what to do, when a person or an agent has no key to sign with.
    fn key(&self) -> Result<Option<crate::machine::Machine>, String> {
        match self.signer() {
            Signer::Nobody => Ok(None),
            // One key per agent, made with its token: never a person's key file left here, and
            // never one registered from a sandbox.
            Signer::Agent => match crate::machine::named() {
                Ok(Some(machine)) => Ok(Some(machine)),
                Ok(None) => Err(AGENT_NEEDS_ITS_KEY.to_string()),
                Err(err) => Err(format!("{err:#}")),
            },
            // \[ADR 0060\] In CI, a personal `HUSHRUN_TOKEN` signs with the key the job names, never
            // the one a person's sign-in left on the runner, as events don't (T-15).
            Signer::Person if self.in_ci => match crate::machine::named() {
                Ok(Some(machine)) => Ok(Some(machine)),
                Ok(None) => Err(CI_NEEDS_A_NAMED_KEY.to_string()),
                Err(err) => Err(format!("{err:#}")),
            },
            Signer::Person => match crate::machine::load() {
                Ok(Some(machine)) => Ok(Some(machine)),
                Ok(None) => Err(PERSON_NEEDS_A_MACHINE.to_string()),
                Err(err) => Err(format!("{err:#}")),
            },
        }
    }

    /// A person's machine, registered for the token in this org: by the sign-in, or now, once,
    /// with `POST {base}/v0/machines`, which a sign-in approved for no org, or for another,
    /// leaves to the first write. A server without machines (404, 405) ignores signatures, so
    /// the key still signs there. A refusal, such as 409 `machine_limit`, is what to do.
    fn register(
        &self,
        machine: crate::machine::Machine,
    ) -> Result<crate::machine::Machine, String> {
        let Some(token) = self.token.as_deref() else {
            return Ok(machine);
        };
        let keyid = machine.keyid();
        if self.signed_in_machine.as_deref() == Some(keyid.as_str()) {
            return Ok(machine);
        }
        // What an earlier run learned, so a machine registers once per token and org.
        let marker = crate::context::cache_dir().map(|dir| {
            let key = format!("{token}\n{}", self.base);
            dir.join("machines")
                .join(&Digest::of(key.as_bytes()).hex()[..32])
        });
        let remembered = marker
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok());
        match remembered.as_deref().map(str::trim) {
            Some(line)
                if line == format!("registered {keyid}")
                    || line == format!("unsupported {keyid}") =>
            {
                return Ok(machine);
            }
            _ => {}
        }
        let body = json!({
            "key": machine.public_key().to_string(),
            "name": crate::machine::name(),
            "proof": machine.token_proof(token),
        });
        let answer = self.ledger.json_request(
            "POST",
            "v0/machines",
            Some(&serde_json::to_vec(&body).expect("JSON")),
        );
        let kept = match answer {
            Ok((200..=299, _)) => "registered",
            Ok((404 | 405, _)) => "unsupported",
            Ok((status, body)) => {
                let why = error_message(&body).unwrap_or_else(|| format!("{status}"));
                return Err(match error_code(&body).as_deref() {
                    // [ADR 0028, amended] Revoked from the dashboard: this machine needs a new key.
                    Some("machine_revoked") => format!(
                        "{why}. To make this machine a new key, remove {}, then run `hushrun login`",
                        machine.source
                    ),
                    _ => format!("{} didn't register this machine: {why}", self.base),
                });
            }
            Err(err) => {
                return Err(format!(
                    "{} didn't register this machine: {}",
                    self.base,
                    plain(err)
                ));
            }
        };
        if let Some(path) = marker {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
            let _ = std::fs::write(&path, format!("{kept} {keyid}\n"));
        }
        Ok(machine)
    }

    /// The error for an answer other than success, keeping why the server refused the caller.
    fn refused(&self, status: u16, body: &[u8], what: &str) -> anyhow::Error {
        let why = || error_message(body).unwrap_or_else(|| format!("{status}"));
        let machine = matches!(
            error_code(body).as_deref(),
            Some("machine_required" | "machine_unknown")
        );
        let kind = match status {
            401 if self.credential == Credential::Nothing => Some(Refusal::NoToken(why())),
            // ADR 0028: the result needs a machine's signature that counts, which the person or
            // the agent fixes as when they have no key.
            401 if machine => Some(Refusal::Machine(
                match (self.signer(), &self.credential) {
                    (_, Credential::Agent) => STRING_NEEDS_REPLACING,
                    (Signer::Agent, _) => AGENT_NEEDS_ITS_KEY,
                    _ if self.in_ci => CI_NEEDS_A_NAMED_KEY,
                    _ => PERSON_NEEDS_A_MACHINE,
                }
                .to_string(),
            )),
            401 | 403 => Some(Refusal::Credential(why())),
            429 => Some(Refusal::Limited(why())),
            // ADR 0032: a record the org's plan doesn't allow; any other 402 says nothing more.
            402 => Quota::from_answer(status, body).map(|quota| Refusal::Quota(Box::new(quota))),
            _ => None,
        };
        *self.refusal.borrow_mut() = kind;
        refusal(&self.base, status, body, what)
    }
}

/// \[ADR 0067\] [`Backend::sweep`] of the directory `dir`. Its stamp, [`SWEPT`], is written whole
/// before anything goes, so that runs at once mostly leave the sweep to one; two that both sweep
/// delete the same files, and a sweep stopped halfway is finished by the next. A file goes only
/// when its name is a ULID older than the cutoff, which no new run writes, so no sweep takes a
/// result newer than its own cutoff; anything else in the directory stays. A key directory left
/// empty goes too, and a run that wanted it makes it again (L-3). A clock behind the stamp's
/// sweeps by its own time.
fn sweep(dir: &Path, now: Time) {
    let stamp = dir.join(SWEPT);
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|text| Time::parse(text.trim()).ok());
    if last.is_some_and(|last| (0..SWEEP_EVERY_MS).contains(&now.millis_since(last))) {
        return;
    }
    // Without a stamp, every run would sweep: none does.
    if let Err(err) = crate::cmd::setup::write(&stamp, &format!("{now}\n")) {
        tracing::debug!("{}: not swept: {err:#}", stamp.display());
        return;
    }
    let cutoff = now.millis().saturating_sub(KEPT_FOR_MS);
    let mut gone = 0usize;
    // \[ADR 0068\] Unit results, and the runs they went with, are this machine's too (C-33).
    for prefix in [
        PREFIX,
        SKIP_PREFIX,
        crate::units::store::ENTRIES,
        crate::units::store::RUNS,
    ] {
        let top = dir.join(prefix);
        let Ok(keys) = std::fs::read_dir(&top) else {
            continue;
        };
        for key in keys.flatten() {
            let key = key.path();
            if !key.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&key) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name();
                let old = name
                    .to_str()
                    .and_then(|name| name.strip_suffix(".json"))
                    .and_then(hushrun_ledger::ulid_millis)
                    .is_some_and(|finished| finished < cutoff);
                if !old {
                    continue;
                }
                match std::fs::remove_file(file.path()) {
                    Ok(()) => gone += 1,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => tracing::debug!("{}: {err}", file.path().display()),
                }
            }
            // Only an empty one goes: one a run has just written to stays.
            let _ = std::fs::remove_dir(&key);
        }
    }
    tracing::debug!(
        "{}: swept {gone} results from before {cutoff}",
        dir.display()
    );
}

/// `text` for a URL's query.
fn query_value(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                char::from(byte).to_string()
            }
            byte => format!("%{byte:02X}"),
        })
        .collect()
}

/// Whether `token` is an agent token (`hra_…`).
pub fn is_agent_token(token: &str) -> bool {
    token.starts_with(AGENT_TOKEN_PREFIX)
}

/// A backend's error without the ledger's wording around it.
pub fn plain(err: LedgerError) -> anyhow::Error {
    match err {
        LedgerError::NotFound(m)
        | LedgerError::AlreadyExists(m)
        | LedgerError::Denied(m)
        | LedgerError::Unavailable(m)
        | LedgerError::Invalid(m) => anyhow!(m),
        // ADR 0032: the server's message, and where to upgrade.
        LedgerError::Quota(quota) => anyhow!(quota.line()),
    }
}

/// What a server's answer other than success says, in its own words when it sent some.
pub fn refusal(base: &str, status: u16, body: &[u8], what: &str) -> anyhow::Error {
    match error_message(body) {
        Some(message) => anyhow!("{base}: {what}: {status}: {message}"),
        None if status == 404 => {
            anyhow!("{base}: {what}: 404: no results backend answers at this URL")
        }
        None => anyhow!("{base}: {what}: {status}"),
    }
}

/// The message of a server's `{"error": {"code", "message"}}`.
pub fn error_message(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value["error"]["message"].as_str().map(str::to_string)
}

/// The code of a server's `{"error": {"code", "message"}}`.
pub fn error_code(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value["error"]["code"].as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(provider: Provider, job: Option<&str>) -> RunResult {
        RunResult {
            schema: RESULT_SCHEMA.into(),
            check: CheckName::new("unit").unwrap(),
            result: Verdict::Pass,
            exit_code: Some(0),
            command: vec!["x".into()],
            cwd: String::new(),
            scope: ScopeName::new("default").unwrap(),
            digest: Digest::of(b"x"),
            external: Vec::new(),
            revision: None,
            repository: None,
            started_at: Time::parse("2026-11-02T00:00:00Z").unwrap(),
            duration_ms: 1,
            saved_ms: None,
            provider,
            job: job.map(str::to_string),
            variant: None,
            run_url: None,
            tool: "hushrun".into(),
            invoked_by: None,
            environment: None,
            reports: None,
            ran_anyway: None,
            units: None,
        }
    }

    // covers: C-27
    #[test]
    fn a_summary_that_doesnt_read_never_stops_a_result_reading() {
        let mut record = serde_json::to_value(pass(Provider::Github, None)).unwrap();
        assert!(record.get("reports").is_none(), "no summary, no field");
        for odd in [
            serde_json::json!("1,204 tests"),
            serde_json::json!({"tests": {"total": -1}}),
        ] {
            record["reports"] = odd;
            let read: RunResult = serde_json::from_value(record.clone()).unwrap();
            assert_eq!(read.reports, None);
        }
        record["reports"] =
            serde_json::json!({"tests": {"total": 3, "failed": 1, "errors": 0, "skipped": 0}});
        let read: RunResult = serde_json::from_value(record).unwrap();
        assert_eq!(read.reports.unwrap().tests.unwrap().failed, 1);
    }

    // covers: C-10
    #[test]
    fn a_ci_pass_counts_in_its_own_job() {
        let here = Some("test@linux/amd64");
        let counts = |result: RunResult, job| result.counts_in(job, None);
        // Its own, whichever workflow's file it was in, and no other.
        assert!(counts(pass(Provider::Github, here), here));
        assert!(!counts(
            pass(Provider::Github, Some("lint@linux/amd64")),
            here
        ));
        // The same job on another system.
        assert!(!counts(
            pass(Provider::Github, Some("test@darwin/arm64")),
            here
        ));
        assert!(!counts(
            pass(Provider::Gitlab, Some("t 1/3@linux/amd64")),
            Some("t 2/3@linux/amd64")
        ));
        // Laptops and agents stand in for CI, as C-9 says.
        assert!(counts(pass(Provider::Local, None), here));
        // From before results named their job, a CI that names jobs can't say whose it is.
        assert!(!counts(pass(Provider::Github, None), here));
        assert!(!counts(pass(Provider::Gitlab, None), here));
        assert!(!counts(pass(Provider::Buildkite, None), here));
        // A CI that names none stays as it was.
        assert!(counts(pass(Provider::Generic, None), here));
        // A run nothing names a job for takes any pass, as it did.
        assert!(counts(pass(Provider::Github, Some("a@b/c")), None));
        assert!(counts(pass(Provider::Github, None), None));
    }

    // covers: C-10, C-14
    #[test]
    fn a_variants_pass_counts_for_that_variant_only() {
        let here = Some("test@linux/amd64");
        let of = |provider, job, key: Option<&str>| RunResult {
            variant: key.map(|key| Variant {
                key: key.to_string(),
                label: "18".to_string(),
            }),
            ..pass(provider, job)
        };
        let (a, b) = (Some("sha256:aa"), Some("sha256:bb"));
        assert!(of(Provider::Github, here, a).counts_in(here, a));
        assert!(!of(Provider::Github, here, b).counts_in(here, a));
        // A pass of the job without variants doesn't stand in for one of its variants, nor the
        // other way round.
        assert!(!of(Provider::Github, here, None).counts_in(here, a));
        assert!(!of(Provider::Github, here, a).counts_in(here, None));
        // No pass from outside CI ran as any one variant (D3).
        assert!(!of(Provider::Local, None, None).counts_in(here, a));
        assert!(!of(Provider::Generic, None, None).counts_in(here, a));
        // Outside CI, any pass stands in, as before.
        assert!(of(Provider::Github, here, a).counts_in(None, None));
    }

    // covers: C-10
    #[test]
    fn a_variant_that_doesnt_read_never_stops_a_result_reading() {
        let mut record = serde_json::to_value(pass(Provider::Github, None)).unwrap();
        assert!(record.get("variant").is_none(), "no variant, no field");
        record["variant"] = serde_json::json!("node 20");
        let read: RunResult = serde_json::from_value(record.clone()).unwrap();
        assert_eq!(read.variant, None);
        record["variant"] = serde_json::json!({"key": "sha256:aa", "label": "20", "more": 1});
        let read: RunResult = serde_json::from_value(record).unwrap();
        assert_eq!(read.variant.unwrap().key, "sha256:aa");
    }

    // covers: C-9, C-25
    #[test]
    fn a_key_holds_external_inputs_and_a_rule_only_when_there_is_one() {
        // C-9's three examples, worked out with Python's json and hashlib.
        let check = CheckName::new("unit").unwrap();
        let command = ["vitest".to_string(), "run".to_string()];
        let scope = ScopeName::default_scope();
        let digest = format!("sha256:{}", "1".repeat(64)).parse().unwrap();
        let without = key(&check, &command, "", &scope, &digest, &[], None).unwrap();
        assert_eq!(
            without.hex(),
            "1abcbeed3a0264f77f5da8fad0aa13880079d906e0708bc931cbca454b345425"
        );
        let cli = ExternalInput {
            path: "../cli".into(),
            digest: format!("sha256:{}", "2".repeat(64)).parse().unwrap(),
        };
        let with = key(&check, &command, "", &scope, &digest, &[cli], None).unwrap();
        assert_eq!(
            with.hex(),
            "853ae97a48a239d2f64a5079d77204544acd3aa5350167385dac1dbd557539ee"
        );
        let rule = Policy::parse(
            "hushrun.yaml",
            b"version: 1\nrepository: github.com/acme/app\ncounts_from: {go: \"1.25\", os: linux}\n",
        )
        .unwrap();
        let (ruled, found) =
            run_key(Some(&rule), &check, &command, "", &scope, &digest, &[]).unwrap();
        assert_eq!(
            found.as_ref().map(Rule::normal),
            Some(json!({"go": ["1.25"], "os": ["linux"]}))
        );
        assert_eq!(
            ruled.hex(),
            "7a6794efe55fb3b588d8677119e52de0a035ed3f6e772da188f7136839d8d40b"
        );
        // `anywhere`, and no hushrun.yaml, add nothing.
        let anywhere = Policy::parse(
            "hushrun.yaml",
            b"version: 1\nrepository: github.com/acme/app\ncounts_from: anywhere\n",
        )
        .unwrap();
        for policy in [Some(&anywhere), None] {
            let (same, rule) = run_key(policy, &check, &command, "", &scope, &digest, &[]).unwrap();
            assert_eq!(same, without);
            assert_eq!(rule, None);
        }
    }
}
