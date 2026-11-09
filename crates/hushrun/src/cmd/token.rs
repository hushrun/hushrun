//! `hushrun token` (C-17): an org admin's agent tokens, which let agents in sandboxes, and CI
//! outside GitHub, record results without a browser (ADR 0027). They go through the org's
//! backend, `{base}/v0/agent-tokens`, with the personal token `hushrun login` saved for its
//! origin; `HUSHRUN_TOKEN` never does this, since an agent token can't make another.
//!
//! \[ADR 0028, amended 2026-10-28\] `create` makes the agent's machine key with the token: a
//! P-256 key made here, whose public half and proof go with the request, so the server registers
//! it as the token's one machine. Every sandbox of the agent signs its results with that key, and
//! none registers its own.
//!
//! \[ADR 0046\] It also shows the one string a sandbox needs, `HUSHRUN_AGENT`, where a person reads
//! it: on stderr at a terminal, outside CI, as the key is. Anywhere else stderr may be a log, so
//! neither is printed there, and `--format json` is how scripts get them. Stdout stays the token
//! alone, which scripts hold, and with `HUSHRUN_TOKEN` and `HUSHRUN_MACHINE_KEY` CI outside GitHub
//! keeps working: `HUSHRUN_AGENT` is for sandboxes.
//! An agent has no business here: every `token` command refuses beside `HUSHRUN_AGENT`.

use std::io::IsTerminal as _;

use anyhow::anyhow;
use serde::Deserialize;
use serde_json::{Value, json};

use hushrun_core::agent::AgentString;
use hushrun_core::ledger_url::LedgerUrl;
use hushrun_ledger::HttpLedger;

use crate::cli::{GlobalOpts, TokenBackendArgs, TokenCommand, TokenCreateArgs, TokenRevokeArgs};
use crate::cmd::login::repository_root;
use crate::credentials::{self, Credentials};
use crate::exit::{CmdResult, Exit, Failure};
use crate::hosted;
use crate::output::{self, Format};
use crate::results::{plain, refusal};

const SCHEMA: &str = "hushrun.token/v1";
/// The longest a token lasts: 365 days.
const MAX_HOURS: u64 = 365 * 24;
const HOUR_MS: u64 = 3_600_000;

pub fn run(global: &GlobalOpts, command: &TokenCommand) -> CmdResult {
    // \[ADR 0046\] Beside a person's login, an agent could otherwise mint agents.
    if hushrun_ci::Env::from_process().agent_set() {
        return Err(Failure::usage(anyhow!(
            "agent tokens are an org admin's, and HUSHRUN_AGENT is set here: run hushrun token on your own machine, or unset HUSHRUN_AGENT"
        )));
    }
    match command {
        TokenCommand::Create(args) => create(global, args),
        TokenCommand::List(args) => list(global, args),
        TokenCommand::Revoke(args) => revoke(global, args),
    }
}

/// An agent token as the backend shows it: never the token itself, only its prefix.
#[derive(Debug, Clone, Deserialize)]
struct AgentToken {
    id: String,
    name: String,
    #[serde(default)]
    repository: Option<String>,
    prefix: String,
    #[serde(default)]
    results_per_minute: Option<u64>,
    #[serde(default)]
    created_by: Option<String>,
    expires_at: String,
    #[serde(default)]
    last_used_at: Option<String>,
}

impl AgentToken {
    fn scope(&self) -> &str {
        self.repository.as_deref().unwrap_or("whole org")
    }
}

fn create(global: &GlobalOpts, args: &TokenCreateArgs) -> CmdResult {
    if !hushrun_core::names::is_valid_name(&args.agent) {
        return Err(Failure::usage(anyhow!(
            "--agent {:?} isn't a name: a lowercase letter, then lowercase letters, digits, _ and -, 63 at most, such as fleet-a",
            args.agent
        )));
    }
    let ms = args.expires.millis();
    let hours = ms / HOUR_MS;
    if !ms.is_multiple_of(HOUR_MS) || !(1..=MAX_HOURS).contains(&hours) {
        return Err(Failure::usage(anyhow!(
            "--expires is whole hours from 1h to 365d, such as 12h, 30d or 8w"
        )));
    }
    let repository = if args.org_wide {
        None
    } else {
        match &args.repository {
            Some(repository) => Some(repository.trim().to_string()),
            None => Some(
                repository_root(global)
                    .and_then(|root| crate::cmd::setup::repository_id(&root))
                    .ok_or_else(|| {
                        Failure::usage(anyhow!(
                            "name the repository the token is for with --repository host/owner/name, or --org-wide for every repository of the org: hushrun can't tell which repository this is"
                        ))
                    })?,
            ),
        }
    };
    let org = OrgBackend::find(global, &args.target)?;
    let client = org.client()?;
    let machine = agent_machine(&org.org, &args.agent)?;
    let mut body = json!({
        "name": args.agent,
        "repository": repository,
        "expires_in_hours": hours,
        "machine": {
            "key": machine.public_key().to_string(),
            "proof": machine.agent_proof(&org.org, &args.agent),
        },
    });
    if let Some(rate) = args.rate {
        body["results_per_minute"] = json!(rate);
    }
    let created = org.request(
        &client,
        "POST",
        "v0/agent-tokens",
        Some(&body),
        "creating an agent token",
    )?;
    let token = created["token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("{}: the answer holds no token", org.base))?
        .to_string();
    let row: AgentToken = serde_json::from_value(created.clone())
        .map_err(|e| anyhow!("{}: the answer isn't an agent token: {e}", org.base))?;
    // The server answers the machine it registered with the token: this key.
    let registered = created["machine"]["keyid"].as_str() == Some(machine.keyid().as_str());
    if !registered {
        eprintln!(
            "warning: {} didn't say it registered the agent's key with the token, so it may refuse the results the key signs",
            org.base
        );
    }

    let pem = machine.key.to_pem();
    let string = AgentString::encode(&org.base, &token, pem.as_str());

    if global.format == Format::Json {
        let mut out = created;
        out["schema"] = json!(SCHEMA);
        out["org"] = json!(org.org);
        out["hushrun_agent"] = json!(string);
        out["machine_key"] = json!({
            "pem": pem.as_str(),
            "public_key": machine.public_key().to_string(),
            "keyid": machine.keyid(),
        });
        output::print_json(&out)?;
        return Ok(Exit::Ok);
    }
    // The token alone on stdout, so that `$(hushrun token create …)` holds it.
    output::print(&token)?;
    // The string, the token and the key only where a person reads them: a piped stderr is often a
    // log, and in CI always is. `--format json` is how scripts get them.
    let for_a_person = std::io::stderr().is_terminal()
        && !hushrun_ci::context(&hushrun_ci::Env::from_process()).in_ci;
    let shows_string = for_a_person && std::io::stdout().is_terminal();
    if shows_string {
        eprintln!("HUSHRUN_AGENT={string}");
    }
    let now = now(global);
    let rate = row
        .results_per_minute
        .map(|n| format!(", up to {} a minute", crate::cmd::whoami::results(n)))
        .unwrap_or_default();
    eprintln!(
        "Created {}… for agent:{} in {}: {}, expires {}{rate}.",
        row.prefix,
        row.name,
        org.org,
        row.scope(),
        when(&row.expires_at, now),
    );
    if shows_string {
        eprintln!(
            "That first line is all a sandbox needs: it holds the backend, the token and the agent's key, and isn't shown again. It means a sandbox, so keep it out of CI."
        );
    } else {
        eprintln!(
            "A sandbox needs one variable, HUSHRUN_AGENT, with the backend, the token and the agent's key in one string. It shows at a terminal, and --format json gives it as hushrun_agent."
        );
    }
    let shown = if shows_string {
        token.as_str()
    } else {
        "<the token on stdout>"
    };
    eprintln!(
        "\nFor CI outside GitHub, and sandboxes set up by hand, the same as variables of their own, with its machine key, {}. Neither is shown again:\n",
        crate::hosted::short_keyid(&machine.keyid())
    );
    eprintln!("  HUSHRUN_TOKEN={shown}");
    eprintln!("  HUSHRUN_PROVIDER=local   # where a sandbox sets CI=true; never in CI");
    // An agent token follows a repository's backend on hushrun's host only (C-13).
    if org.origin != hosted::origin() {
        eprintln!("  HUSHRUN_BACKEND={}", org.base);
    }
    if for_a_person {
        eprintln!(
            "  HUSHRUN_MACHINE_KEY=<the private key below, as one value>\n\n{}",
            pem.as_str().trim_end()
        );
    } else {
        eprintln!(
            "  HUSHRUN_MACHINE_KEY=<not printed: stderr isn't a terminal, and a log could keep it>\n\nWithout its key this token records nothing, and the key isn't shown again. Revoke it, and make the next at a terminal, or with --format json, which gives the token, the key and the string."
        );
    }
    eprintln!(
        "\nEvery sandbox of agent:{} signs its results and events with this key, which count while the token does. Its public key:\n  {}",
        row.name,
        machine.public_key()
    );
    Ok(Exit::Ok)
}

/// \[ADR 0028\] The agent's machine key, made here for its new token: one machine for every
/// sandbox the token runs in, registered with the token by the request that makes it.
fn agent_machine(org: &str, agent: &str) -> Result<crate::machine::Machine, Failure> {
    let key = hushrun_attest::SigningKey::generate_p256()
        .map_err(|e| anyhow!("making the agent's machine key for {org}'s agent:{agent}: {e}"))?;
    Ok(crate::machine::Machine {
        key,
        source: "hushrun token create".into(),
    })
}

fn list(global: &GlobalOpts, target: &TokenBackendArgs) -> CmdResult {
    let org = OrgBackend::find(global, target)?;
    let client = org.client()?;
    let answer = org.request(
        &client,
        "GET",
        "v0/agent-tokens",
        None,
        "listing agent tokens",
    )?;
    if global.format == Format::Json {
        output::print_json(&json!({
            "schema": SCHEMA,
            "org": org.org,
            "items": answer["items"].as_array().cloned().unwrap_or_default(),
        }))?;
        return Ok(Exit::Ok);
    }
    let items = tokens(&answer, &org.base)?;
    if items.is_empty() {
        output::print(&format!(
            "{} has no agent tokens. Create one with hushrun token create --agent <name>.",
            org.org
        ))?;
        return Ok(Exit::Ok);
    }
    let now = now(global);
    let mut rows = vec![[
        "TOKEN".to_string(),
        "AGENT".to_string(),
        "FOR".to_string(),
        "EXPIRES".to_string(),
        "LAST USED".to_string(),
        "CREATED BY".to_string(),
    ]];
    for t in &items {
        rows.push([
            format!("{}…", t.prefix),
            format!("agent:{}", t.name),
            t.scope().to_string(),
            relative_to(&t.expires_at, now, true),
            t.last_used_at
                .as_deref()
                .map_or_else(|| "never".to_string(), |at| relative_to(at, now, false)),
            t.created_by.clone().unwrap_or_default(),
        ]);
    }
    output::print(&table(&rows))?;
    Ok(Exit::Ok)
}

fn revoke(global: &GlobalOpts, args: &TokenRevokeArgs) -> CmdResult {
    let org = OrgBackend::find(global, &args.target)?;
    let client = org.client()?;
    let answer = org.request(
        &client,
        "GET",
        "v0/agent-tokens",
        None,
        "listing agent tokens",
    )?;
    let items = tokens(&answer, &org.base)?;
    let token = pick(&items, args.token.trim(), &org.org)?;
    org.request(
        &client,
        "DELETE",
        &format!("v0/agent-tokens/{}", token.id),
        None,
        "revoking the agent token",
    )?;
    if global.format == Format::Json {
        let revoked = answer["items"]
            .as_array()
            .and_then(|items| items.iter().find(|t| t["id"] == token.id.as_str()))
            .cloned()
            .unwrap_or(Value::Null);
        output::print_json(&json!({"schema": SCHEMA, "org": org.org, "revoked": revoked}))?;
        return Ok(Exit::Ok);
    }
    output::print(&format!(
        "Revoked {}… (agent:{}, {}): anything that uses it is refused from now on.",
        token.prefix,
        token.name,
        token.scope()
    ))?;
    Ok(Exit::Ok)
}

/// The token `given` names: its id, its prefix (or the whole token, which starts with it), or
/// its agent's name when only one token has it.
fn pick<'a>(items: &'a [AgentToken], given: &str, org: &str) -> Result<&'a AgentToken, Failure> {
    if let Some(token) = items.iter().find(|t| {
        t.id == given || t.prefix == given || (given.len() > 10 && given.starts_with(&t.prefix))
    }) {
        return Ok(token);
    }
    let named: Vec<&AgentToken> = items.iter().filter(|t| t.name == given).collect();
    match named.as_slice() {
        [only] => Ok(only),
        [] => Err(Failure::new(
            Exit::Unexpected,
            anyhow!(
                "{org} has no agent token {given:?} that isn't revoked: hushrun token list shows them"
            ),
        )),
        several => Err(Failure::usage(anyhow!(
            "agent:{given} has {} tokens: {}; revoke one by its prefix",
            several.len(),
            several
                .iter()
                .map(|t| format!("{}… ({})", t.prefix, t.scope()))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn tokens(answer: &Value, base: &str) -> Result<Vec<AgentToken>, Failure> {
    serde_json::from_value(answer["items"].clone()).map_err(|e| {
        Failure::new(
            Exit::Ledger,
            anyhow!("{base}: the answer isn't a list of agent tokens: {e}"),
        )
    })
}

/// The org's backend, where its tokens are managed.
struct OrgBackend {
    /// `https://host/o/acme`, normalized.
    base: String,
    org: String,
    origin: String,
}

impl OrgBackend {
    /// `--backend`; else `HUSHRUN_BACKEND`; else the repository's backend; else the one
    /// `hushrun login` saved: `hushrun login`'s order, without the hosted default, which names
    /// no org (C-17).
    fn find(global: &GlobalOpts, target: &TokenBackendArgs) -> Result<OrgBackend, Failure> {
        // As every reader of hushrun's variables: \[ADR 0046\] `HUSHRUN_AGENT`'s backend in its
        // place, though `run` refuses before this when one is set.
        let env = hushrun_ci::var("HUSHRUN_BACKEND")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let (text, source) = if let Some(text) = &target.backend {
            (text.clone(), "--backend")
        } else if let Some(text) = env {
            (text, "HUSHRUN_BACKEND")
        } else if let Some((text, named_by)) =
            repository_root(global).and_then(|root| hosted::named_backend(&root))
        {
            (text, named_by.source())
        } else if let Some(text) = Credentials::load()?.backend {
            (text, "the backend hushrun login saved")
        } else {
            return Err(Failure::usage(anyhow!(
                "name your org's backend with --backend, such as --backend {}/o/acme, or sign in with hushrun login",
                hosted::origin()
            )));
        };
        let url = LedgerUrl::parse(text.trim())
            .map_err(|e| Failure::usage(anyhow!("{source}, {text}, isn't a backend's URL: {e}")))?;
        let base = url.to_string();
        let org = hosted::org_of(&base).ok_or_else(|| {
            Failure::usage(anyhow!(
                "{source}, {base}, isn't an org's URL, such as {}/o/acme",
                credentials::origin(&url)
            ))
        })?;
        Ok(OrgBackend {
            origin: credentials::origin(&url),
            base,
            org,
        })
    }

    /// A client with the personal token `hushrun login` saved for the backend's origin.
    fn client(&self) -> Result<HttpLedger, Failure> {
        let url = LedgerUrl::parse(&self.base).map_err(|e| anyhow!("{}: {e}", self.base))?;
        let saved = Credentials::load()?;
        let Some(login) = saved.login_for(&url) else {
            return Err(Failure::usage(anyhow!(
                "agent tokens take an org admin's login at {}: run hushrun login {}",
                self.origin,
                self.base
            )));
        };
        hosted::open(&self.base, Some(&login.token))
    }

    /// A request under the base, and its JSON answer; a refusal in the server's words.
    fn request(
        &self,
        client: &HttpLedger,
        method: &str,
        path: &str,
        body: Option<&Value>,
        what: &str,
    ) -> Result<Value, Failure> {
        let bytes = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(anyhow::Error::from)?;
        let (status, answer) = client
            .json_request(method, path, bytes.as_deref())
            .map_err(|e| Failure::new(Exit::Ledger, plain(e)))?;
        match status {
            204 => Ok(Value::Null),
            200..=299 => serde_json::from_slice(&answer).map_err(|e| {
                Failure::new(
                    Exit::Ledger,
                    anyhow!("{}: {what}: the answer isn't JSON: {e}", self.base),
                )
            }),
            404 | 405 if !path.contains("agent-tokens/") => Err(Failure::new(
                Exit::Unexpected,
                anyhow!(
                    "{}: {what}: {status}: this backend doesn't issue agent tokens; see its own settings",
                    self.base
                ),
            )),
            400 => Err(Failure::usage(refusal(&self.base, status, &answer, what))),
            500..=599 => Err(Failure::new(
                Exit::Ledger,
                refusal(&self.base, status, &answer, what),
            )),
            _ => Err(Failure::new(
                Exit::Unexpected,
                refusal(&self.base, status, &answer, what),
            )),
        }
    }
}

/// Now, in milliseconds: `--now` in tests.
pub(crate) fn now(global: &GlobalOpts) -> i64 {
    global
        .now
        .as_deref()
        .and_then(|text| text.parse::<jiff::Timestamp>().ok())
        .unwrap_or_else(jiff::Timestamp::now)
        .as_millisecond()
}

/// An RFC 3339 time as `2026-11-19 10:00 UTC (in 29 days)`; as given when it doesn't parse.
pub(crate) fn when(time: &str, now: i64) -> String {
    match time.parse::<jiff::Timestamp>() {
        Ok(at) => format!(
            "{} ({})",
            at.strftime("%Y-%m-%d %H:%M UTC"),
            relative(at.as_millisecond() - now)
        ),
        Err(_) => time.to_string(),
    }
}

/// How far an RFC 3339 time is from `now`: `in 29 days`, `2 min ago`; `expired` for an expiry
/// past when `expiry`.
fn relative_to(time: &str, now: i64, expiry: bool) -> String {
    match time.parse::<jiff::Timestamp>() {
        Ok(at) if expiry && at.as_millisecond() <= now => "expired".to_string(),
        Ok(at) => relative(at.as_millisecond() - now),
        Err(_) => time.to_string(),
    }
}

/// `in 30 days`, `in 5 h`, `just now`, `12 min ago`, `3 days ago`, to the nearest unit: a
/// token made a moment ago for 30 days expires `in 30 days`, not 29.
pub(crate) fn relative(ms: i64) -> String {
    let s = ms.unsigned_abs() / 1000;
    let nearest = |unit: u64| (s + unit / 2) / unit;
    let span = match s {
        0..60 => return "just now".to_string(),
        60..3_600 => format!("{} min", nearest(60)),
        3_600..172_800 => format!("{} h", nearest(3_600)),
        _ => format!("{} days", nearest(86_400)),
    };
    if ms > 0 {
        format!("in {span}")
    } else {
        format!("{span} ago")
    }
}

/// Columns padded to their widest cell, two spaces apart.
fn table<const N: usize>(rows: &[[String; N]]) -> String {
    let widths: Vec<usize> = (0..N)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in rows {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_like_people_say_them() {
        assert_eq!(relative(29 * 86_400_000 + 5_000), "in 29 days");
        // A 30-day token a few seconds old.
        assert_eq!(relative(30 * 86_400_000 - 5_000), "in 30 days");
        assert_eq!(relative(24 * 3_600_000 - 5_000), "in 24 h");
        assert_eq!(relative(5 * 3_600_000), "in 5 h");
        assert_eq!(relative(-12 * 60_000), "12 min ago");
        assert_eq!(relative(-3 * 86_400_000), "3 days ago");
        assert_eq!(relative(30_000), "just now");
        let now = "2026-10-20T10:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond();
        assert_eq!(
            when("2026-11-19T10:00:00.000Z", now),
            "2026-11-19 10:00 UTC (in 30 days)"
        );
        assert_eq!(
            relative_to("2026-10-19T10:00:00.000Z", now, true),
            "expired"
        );
        assert_eq!(
            relative_to("2026-10-19T10:00:00.000Z", now, false),
            "24 h ago"
        );
    }

    #[test]
    fn tables_line_up() {
        let rows = [
            ["A".to_string(), "BB".to_string()],
            ["ccc".to_string(), "d".to_string()],
        ];
        assert_eq!(table(&rows), "A    BB\nccc  d\n");
    }
}
