//! What every command shares: the environment, the run context, the clock, and where
//! configuration, the repository's server, its signers and keys come from (T-4, §10.1).

use anyhow::{Context as _, anyhow};

use hushrun_attest::SigningKey;
use hushrun_ci::{Env, RunContext};
use hushrun_core::event::{InvokedBy, Invoker, Producer, RunInfo};
use hushrun_core::ledger_url::LedgerUrl;
use hushrun_core::policy::Policy;
use hushrun_core::run::{Custody, Provider, Trigger};
use hushrun_core::signers::Signers;
use hushrun_core::time::Time;
use hushrun_core::transparency::LedgerLog;
use hushrun_ledger::{Ledger, LedgerError, LogKey};

use crate::cli::GlobalOpts;
use crate::exit::{Exit, Failure};

/// The total time `plan`'s ledger requests may take (L-4).
const PLAN_LEDGER_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);
/// `policy explain`'s budget for the signers answer.
const EXPLAIN_LEDGER_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

/// The shared state of one command.
pub struct App<'a> {
    pub global: &'a GlobalOpts,
    pub env: Env,
    pub run: RunContext,
    pub now: Time,
}

impl<'a> App<'a> {
    pub fn new(global: &'a GlobalOpts) -> Result<App<'a>, Failure> {
        let env = Env::from_process();
        let run = hushrun_ci::context(&env);
        let now = match &global.now {
            Some(text) => Time::parse(text).map_err(Failure::usage)?,
            None => Time::from_timestamp(jiff::Timestamp::now()),
        };
        Ok(App {
            global,
            env,
            run,
            now,
        })
    }

    /// The ledger (T-4). In CI it comes from `HUSHRUN_REPOSITORY_URL`, set outside the repository, or
    /// as `<HUSHRUN_REPOSITORY_BASE>/<repository id>` with the id from the CI provider, and must
    /// match `hushrun.yaml`'s, else from `hushrun.yaml` as evaluated (T-5); locally, `--repository-url`, `HUSHRUN_REPOSITORY_URL`, the base with
    /// `hushrun.yaml`'s repository, or `hushrun.yaml`'s `ledger`.
    pub fn ledger_url(&self, policy: Option<&Policy>) -> Result<LedgerUrl, Failure> {
        self.check_policy_source()?;
        let parse = |text: &str, what: &str| {
            LedgerUrl::parse(text).map_err(|e| Failure::usage(anyhow!("{what}: {e}")))
        };
        let base = self
            .env
            .get("HUSHRUN_REPOSITORY_BASE")
            .map(|v| parse(v, "HUSHRUN_REPOSITORY_BASE"))
            .transpose()?;
        let from_env = self
            .env
            .get("HUSHRUN_REPOSITORY_URL")
            .map(|v| parse(v, "HUSHRUN_REPOSITORY_URL"))
            .transpose()?;
        let from_flag = self
            .global
            .repository_url
            .as_deref()
            .map(|v| parse(v, "--repository-url"))
            .transpose()?;
        let from_policy = policy.and_then(|p| p.ledger.clone());
        if self.run.in_ci {
            let url = match (from_env, &base) {
                // A repository's own ledger wins over the organization's base.
                (Some(url), Some(base)) => {
                    if let Some(derived) = self
                        .run
                        .repository
                        .as_deref()
                        .and_then(|r| base.under(r).ok())
                        .filter(|derived| *derived != url)
                    {
                        tracing::warn!(
                            "HUSHRUN_REPOSITORY_URL {url} overrides the URL HUSHRUN_REPOSITORY_BASE gives this repository, {derived}"
                        );
                    }
                    url
                }
                (Some(url), None) => url,
                (None, Some(base)) => self.ledger_under(base, policy)?,
                // \[ADR 0031\] hushrun.yaml as evaluated: a pull request's pipeline reads it from
                // the branch it merges into (T-5).
                (None, None) => match &from_policy {
                    Some(url) => url.clone(),
                    None => {
                        return Err(Failure::usage(anyhow!(
                            "in CI, hushrun reads the repository's URL from HUSHRUN_REPOSITORY_URL or HUSHRUN_REPOSITORY_BASE, else from hushrun.yaml's `repository_url`; none is set"
                        )));
                    }
                },
            };
            if let Some(flag) = from_flag.filter(|f| *f != url) {
                return Err(Failure::usage(anyhow!(
                    "--repository-url {flag} differs from HUSHRUN_REPOSITORY_URL {url}; in CI only HUSHRUN_REPOSITORY_URL counts"
                )));
            }
            if let Some(declared) = from_policy.filter(|p| *p != url) {
                return Err(Failure::usage(anyhow!(
                    "HUSHRUN_REPOSITORY_URL is {url}, but hushrun.yaml's `repository_url` is {declared}; a redirected URL could hide failures"
                )));
            }
            return Ok(url);
        }
        let from_base = match (&base, policy) {
            (Some(base), Some(policy)) => Some(base.under(&policy.repository).map_err(|e| {
                Failure::usage(anyhow!(
                    "HUSHRUN_REPOSITORY_BASE and hushrun.yaml's repository: {e}"
                ))
            })?),
            _ => None,
        };
        from_flag
            .or(from_env)
            .or(from_base)
            .or(from_policy)
            .ok_or_else(|| {
                Failure::usage(match self.uncommitted_repository_url(policy) {
                    Some(why) => anyhow!("no repository URL: {why}"),
                    None => anyhow!(
                        "no repository URL: set `repository_url:` in hushrun.yaml, HUSHRUN_REPOSITORY_URL, or pass --repository-url"
                    ),
                })
            })
    }

    /// Why the URL is missing when the working tree's `hushrun.yaml` names one, but the committed
    /// one, which hushrun reads (T-5), doesn't: such as right after `hushrun init`, before its
    /// commit. `None` otherwise, and with `--config` or `--config-ref`.
    fn uncommitted_repository_url(&self, policy: Option<&Policy>) -> Option<String> {
        use hushrun_core::policy::POLICY_FILE;
        if self.global.config.is_some() || self.global.config_ref.is_some() {
            return None;
        }
        let start = crate::context::start_dir(self.global).ok()?;
        let hushrun_vcs::Detected::Git { root } = hushrun_vcs::detect(&start).ok()? else {
            return None;
        };
        let path = root.join(POLICY_FILE);
        let working = Policy::parse(POLICY_FILE, &std::fs::read(&path).ok()?).ok()?;
        working.ledger.as_ref()?;
        // `--config` takes a path from the directory hushrun starts in.
        let flag = match path.strip_prefix(&start) {
            Ok(relative) => relative.display().to_string(),
            Err(_) => path.display().to_string(),
        };
        Some(match policy {
            None => format!(
                "hushrun.yaml isn't committed yet, and hushrun reads it as committed: commit it, or pass --config {flag}"
            ),
            Some(_) => format!(
                "hushrun.yaml's `repository_url:` isn't committed yet, and hushrun reads hushrun.yaml as committed: commit it, or pass --config {flag}"
            ),
        })
    }

    /// \[M8\] T-5: a pull request's pipeline reads `hushrun.yaml` from the branch it merges into,
    /// which was reviewed, never from the change it evaluates: `--config-ref` must name that
    /// branch. Commands that read or write the ledger check this, as they check T-4.
    fn check_policy_source(&self) -> Result<(), Failure> {
        let Some(pull) = &self.run.pull_request else {
            return Ok(());
        };
        let base = pull.base.as_deref();
        match (&self.global.config_ref, base) {
            (None, _) => Err(Failure::usage(anyhow!(
                "this is a pull request's pipeline, so hushrun.yaml comes from the branch it merges into, which was reviewed, not from the change: pass --config-ref origin/{}",
                base.unwrap_or("<base branch>")
            ))),
            (Some(reference), Some(base))
                if reference != base && !reference.ends_with(&format!("/{base}")) =>
            {
                Err(Failure::usage(anyhow!(
                    "--config-ref {reference} isn't the branch this pull request merges into, {base}: pass --config-ref origin/{base}"
                )))
            }
            _ => Ok(()),
        }
    }

    /// T-4 with `HUSHRUN_REPOSITORY_BASE`, in CI: the ledger is `<base>/<repository id>`, with the id
    /// from the CI provider, not from the tree. `hushrun.yaml`'s `repository` must be that id, and
    /// its `repository_url` that URL, so the two halves T-4 cross-checks stay apart: a changed base can't
    /// redirect a repository silently.
    fn ledger_under(
        &self,
        base: &LedgerUrl,
        policy: Option<&Policy>,
    ) -> Result<LedgerUrl, Failure> {
        let Some(repository) = self.run.repository.as_deref() else {
            return Err(Failure::usage(anyhow!(
                "HUSHRUN_REPOSITORY_BASE gives each repository the URL <base>/<repository id>, with the id from the CI provider, and this CI doesn't say which repository runs: set HUSHRUN_REPOSITORY_URL instead"
            )));
        };
        let url = base
            .under(repository)
            .map_err(|e| Failure::usage(anyhow!("HUSHRUN_REPOSITORY_BASE: {e}")))?;
        let Some(policy) = policy else {
            return Ok(url);
        };
        if policy.repository != repository {
            let case = if policy.repository.eq_ignore_ascii_case(repository) {
                "; they differ only in case, and URL paths don't: write it as the CI does"
            } else {
                ""
            };
            return Err(Failure::usage(anyhow!(
                "this run is in {repository}, but hushrun.yaml's repository is {}{case}. With HUSHRUN_REPOSITORY_BASE the CI provider names the repository, and hushrun.yaml must agree",
                policy.repository
            )));
        }
        match &policy.ledger {
            None => Err(Failure::usage(anyhow!(
                "with HUSHRUN_REPOSITORY_BASE, hushrun.yaml must name the URL it gives {repository}: add `repository_url: {url}`"
            ))),
            Some(declared) if *declared != url => Err(Failure::usage(anyhow!(
                "HUSHRUN_REPOSITORY_BASE gives {repository} the URL {url}, but hushrun.yaml's `repository_url` is {declared}: a changed base can't redirect a repository silently"
            ))),
            Some(_) => Ok(url),
        }
    }

    /// Opens the ledger.
    pub fn ledger(&self, policy: Option<&Policy>) -> Result<(LedgerUrl, Box<dyn Ledger>), Failure> {
        self.open_ledger(policy, None)
    }

    /// The ledger for `plan` and `status`, whose requests share a 60 s budget (L-4).
    pub fn ledger_for_plan(
        &self,
        policy: Option<&Policy>,
    ) -> Result<(LedgerUrl, Box<dyn Ledger>), Failure> {
        self.open_ledger(policy, Some(PLAN_LEDGER_BUDGET))
    }

    /// The ledger for `policy explain`, which only asks for the signers answer and shows the
    /// file without it when the server doesn't answer in 15 s (T-16).
    pub fn ledger_for_explain(
        &self,
        policy: Option<&Policy>,
    ) -> Result<(LedgerUrl, Box<dyn Ledger>), Failure> {
        self.open_ledger(policy, Some(EXPLAIN_LEDGER_BUDGET))
    }

    fn open_ledger(
        &self,
        policy: Option<&Policy>,
        budget: Option<std::time::Duration>,
    ) -> Result<(LedgerUrl, Box<dyn Ledger>), Failure> {
        let url = self.ledger_url(policy)?;
        let ledger = hushrun_ledger::open_with(&url, &self.ledger_options_for(&url, budget))
            .map_err(ledger_failure)?;
        Ok((url, ledger))
    }

    /// Whether `url` is a ledger someone named outside the repository, in `HUSHRUN_REPOSITORY_URL` or
    /// `--repository-url`, or under `HUSHRUN_REPOSITORY_BASE` for the repository the CI provider reports,
    /// rather than one only `hushrun.yaml` names.
    fn named_outside_repository(&self, url: &LedgerUrl) -> bool {
        let names = |text: Option<&str>| {
            text.and_then(|t| LedgerUrl::parse(t).ok())
                .is_some_and(|named| named == *url)
        };
        let under_base = || {
            let base = self
                .env
                .get("HUSHRUN_REPOSITORY_BASE")
                .and_then(|b| LedgerUrl::parse(b).ok());
            match (base, self.run.repository.as_deref()) {
                (Some(base), Some(repository)) => {
                    base.under(repository).is_ok_and(|derived| derived == *url)
                }
                _ => false,
            }
        };
        names(self.env.get("HUSHRUN_REPOSITORY_URL"))
            || names(self.global.repository_url.as_deref())
            || under_base()
    }

    /// The ledger credentials for `url` (L-10, T-4). The environment's ledger tokens go only to
    /// a ledger someone named outside the repository: a URL in `hushrun.yaml` alone must not
    /// decide which host receives one. An agent token on hushrun's host, and outside CI a login,
    /// go to their own origin whoever named the URL, since no other host could use them.
    pub fn ledger_options_for(
        &self,
        url: &LedgerUrl,
        budget: Option<std::time::Duration>,
    ) -> hushrun_ledger::Options {
        let mut options = self.ledger_options(budget);
        if !self.named_outside_repository(url) && self.run.in_ci {
            // \[ADR 0031\] hushrun.yaml names it. The job's ID token names this ledger as its
            // audience, so no other server accepts it (L-10); bearer secrets still go only where
            // HUSHRUN_REPOSITORY_URL or --repository-url says.
            let held = options.token.take().is_some() | options.token_command.take().is_some();
            if held {
                tracing::warn!(
                    "{url} comes from hushrun.yaml, so hushrun sends it the job's ID token but not HUSHRUN_REPOSITORY_TOKEN: set HUSHRUN_REPOSITORY_URL to it to send that too"
                );
            }
        } else if !self.named_outside_repository(url) {
            let held = options.token.take().is_some()
                | options.token_command.take().is_some()
                | options.github_oidc.take().is_some();
            if held {
                tracing::warn!(
                    "{url} comes from hushrun.yaml only, so hushrun sends it no token: the repository must not choose where tokens go. Set HUSHRUN_REPOSITORY_URL to it, or pass --repository-url"
                );
            }
        }
        // ADR 0028 item 5: an agent token on hushrun's host, in CI too, and outside CI a person's
        // login, read the org's ledgers and write what their machine signed. Neither is any use
        // elsewhere: the server there binds an agent token to its org, and a login goes only to
        // the origin that issued it (C-13). The job's ID token, in GitHub Actions, comes first.
        if options.token.is_none()
            && options.token_command.is_none()
            && options.github_oidc.is_none()
        {
            options.token = self
                .personal_token_for(url)
                .map(hushrun_ledger::Secret::new);
        }
        options
    }

    /// The agent or personal token that goes to `url`'s origin: `HUSHRUN_TOKEN` when it is an
    /// agent token and `url` is on hushrun's host (ADR 0027 item 6), in CI as outside it, or
    /// \[ADR 0046\] `HUSHRUN_AGENT`'s when `url` is on its backend's origin; else, outside CI, the
    /// login saved for the origin. CI takes no person's login.
    fn personal_token_for(&self, url: &LedgerUrl) -> Option<String> {
        let origin = crate::credentials::origin(url);
        if let Some(token) = self.env.get("HUSHRUN_TOKEN").map(str::trim)
            && crate::results::is_agent_token(token)
            && crate::hosted::agent_token_goes_to(&self.env, &origin)
        {
            return Some(token.to_string());
        }
        if self.run.in_ci {
            return None;
        }
        crate::credentials::Credentials::load()
            .ok()?
            .login_for(url)
            .map(|login| login.token.clone())
    }

    /// Ledger credentials, read from the environment only, like HUSHRUN_REPOSITORY_URL (T-4): a
    /// repository must not choose a token, or a command that runs with ledger credentials.
    fn ledger_options(&self, budget: Option<std::time::Duration>) -> hushrun_ledger::Options {
        let var = |name: &str| {
            self.env
                .get(name)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        // GitHub sets both when the job has `permissions: id-token: write` (L-10).
        let github_oidc = match (
            var("ACTIONS_ID_TOKEN_REQUEST_URL"),
            var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
        ) {
            (Some(url), Some(token)) => Some(hushrun_ledger::OidcEndpoint {
                url,
                token: hushrun_ledger::Secret::new(token),
            }),
            _ => None,
        };
        hushrun_ledger::Options {
            budget,
            token_command: var("HUSHRUN_REPOSITORY_TOKEN_COMMAND"),
            token: var("HUSHRUN_REPOSITORY_TOKEN").map(hushrun_ledger::Secret::new),
            github_oidc,
        }
    }

    /// \[ADR 0031\] The signers whose events count for the repository (T-15), as its server names
    /// them (L-13), once the server's log is set to the key hushrun.yaml pins (T-9), which checks
    /// the server's statement over them.
    pub fn signers(&self, policy: &Policy, ledger: &dyn Ledger) -> Result<Signers, Failure> {
        Ok(self.signers_answer(policy, ledger)?.signers)
    }

    /// [`App::signers`], with the answer as it came and whether its statement was checked
    /// against the pinned log key.
    pub fn signers_answer(
        &self,
        policy: &Policy,
        ledger: &dyn Ledger,
    ) -> Result<hushrun_ledger::SignersAnswer, Failure> {
        configure_log(ledger, policy)?;
        let answer = ledger.signers(&policy.repository).map_err(ledger_failure)?;
        self.check_ci(&answer.signers)?;
        tracing::info!("signers: {}", answer.signers.summary());
        Ok(answer)
    }

    /// [`App::signers`], saying on stderr who counts, for commands that show what verifies:
    /// `verify`, `repository show`. `-q` silences it.
    pub fn signers_said(&self, policy: &Policy, ledger: &dyn Ledger) -> Result<Signers, Failure> {
        let signers = self.signers(policy, ledger)?;
        if self.global.quiet == 0 {
            eprintln!("signers: {}", signers.summary());
        }
        Ok(signers)
    }

    /// In CI, the server must name this run's repository, by the id the provider reports, as the
    /// repository's CI: a URL that points at another repository's server fails loudly instead of
    /// counting another repository's signers (T-15).
    fn check_ci(&self, signers: &Signers) -> Result<(), Failure> {
        let Some(id) = self
            .run
            .source
            .as_ref()
            .filter(|_| self.run.in_ci)
            .and_then(|s| s.id.as_deref())
        else {
            return Ok(());
        };
        if signers.ci.is_empty() {
            // Only news to a job that can sign keylessly: one that signs as an agent, such as a
            // GitLab pipeline, never counts as the CI.
            if self.has_id_token() {
                tracing::warn!(
                    "the server doesn't know this repository's id yet ({id}), so this job's keyless signatures don't count as its CI until it does"
                );
            }
            return Ok(());
        }
        if !signers.ci.iter().any(|ci| ci.repository_id == id) {
            let named: Vec<&str> = signers
                .ci
                .iter()
                .map(|ci| ci.repository_id.as_str())
                .collect();
            return Err(Failure::usage(anyhow!(
                "the server names repository id {} as {}'s CI, and this job runs in repository id {id}: is repository_url another repository's?",
                named.join(", "),
                signers.repository
            )));
        }
        Ok(())
    }

    pub fn signing_key(&self) -> Result<SigningKey, Failure> {
        let pem = match (
            self.env.get("HUSHRUN_SIGNING_KEY"),
            self.env.get("HUSHRUN_SIGNING_KEY_FILE"),
        ) {
            (Some(pem), _) => pem.to_string(),
            (None, Some(file)) => std::fs::read_to_string(file)
                .with_context(|| format!("reading HUSHRUN_SIGNING_KEY_FILE {file}"))
                .map_err(Failure::usage)?,
            (None, None) => {
                return Err(Failure::usage(anyhow!(
                    "this command signs events: set HUSHRUN_SIGNING_KEY (PEM) or HUSHRUN_SIGNING_KEY_FILE"
                )));
            }
        };
        SigningKey::from_pem(&pem).map_err(Failure::usage)
    }

    /// The custody a command claims (E-4, E-12): `HUSHRUN_CUSTODY`, else `ci` in CI, \[M8\] else
    /// `isolated` for a check `run --isolated` ran, and `local` otherwise. Only an isolated run
    /// claims `isolated`: a `HUSHRUN_CUSTODY=isolated` that names anything else records `local`,
    /// with a warning, since hushrun didn't isolate what it signs.
    pub fn custody_for(&self, isolated: bool) -> Result<Custody, Failure> {
        match self.env.get("HUSHRUN_CUSTODY") {
            Some(value) => {
                let custody: Custody = value
                    .parse()
                    .map_err(|e: String| Failure::usage(anyhow!("HUSHRUN_CUSTODY: {e}")))?;
                if custody == Custody::Isolated && !isolated {
                    tracing::warn!(
                        "HUSHRUN_CUSTODY=isolated, but hushrun isolates only what `run --isolated` runs: recording custody local"
                    );
                    return Ok(Custody::Local);
                }
                Ok(custody)
            }
            None if self.run.in_ci => Ok(Custody::Ci),
            None if isolated => Ok(Custody::Isolated),
            None => Ok(Custody::Local),
        }
    }

    pub fn producer(&self) -> Result<Producer, Failure> {
        self.producer_for(false)
    }

    /// The producer of events a command signs; `isolated` for checks `run --isolated` ran.
    pub fn producer_for(&self, isolated: bool) -> Result<Producer, Failure> {
        Ok(Producer {
            tool: hushrun_core::PRODUCT.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            custody: self.custody_for(isolated)?,
            invoked_by: self.invoked_by()?,
        })
    }

    /// \[M8\] `HUSHRUN_INVOKED_BY`, `agent:<id>` or `human:<id>`: who ran the command, as
    /// events record it (E-4). Informational: nothing decides on it.
    fn invoked_by(&self) -> Result<Option<InvokedBy>, Failure> {
        let Some(value) = self.env.get("HUSHRUN_INVOKED_BY") else {
            return Ok(None);
        };
        let (invoker, id) = match value.split_once(':') {
            Some(("agent", id)) if !id.trim().is_empty() => (Invoker::Agent, id),
            Some(("human", id)) if !id.trim().is_empty() => (Invoker::Human, id),
            _ => {
                return Err(Failure::usage(anyhow!(
                    "HUSHRUN_INVOKED_BY is {value:?}: write agent:<id> or human:<id>, such as agent:claude-code"
                )));
            }
        };
        Ok(Some(InvokedBy {
            invoker,
            id: id.trim().to_string(),
        }))
    }

    /// The run identity for events (§10.4): the provider's id, else a new id per local
    /// command; the attempt from the provider or `HUSHRUN_RUN_ATTEMPT`, else `fallback`.
    pub fn run_info(
        &self,
        trigger: Option<Trigger>,
        fallback_attempt: Option<u32>,
    ) -> Result<RunInfo, Failure> {
        self.run_info_with(trigger, fallback_attempt, None)
    }

    /// The run identity of a command that carries out `plan`: the provider's id and attempt
    /// when it has them, else the plan's (§10.4).
    pub fn run_info_for(
        &self,
        plan: Option<&crate::planfile::PlanFile>,
    ) -> Result<RunInfo, Failure> {
        match plan {
            None => self.run_info(None, None),
            Some(plan) => {
                let mut run = self.run_info_with(
                    Some(plan.run.trigger),
                    Some(plan.run.attempt),
                    Some(&plan.run.id),
                )?;
                if run.url.is_none() {
                    run.url = plan.run.url.clone();
                }
                Ok(run)
            }
        }
    }

    fn run_info_with(
        &self,
        trigger: Option<Trigger>,
        fallback_attempt: Option<u32>,
        fallback_id: Option<&str>,
    ) -> Result<RunInfo, Failure> {
        let id = match (&self.run.run_id, fallback_id, self.run.provider) {
            (Some(id), _, _) => id.clone(),
            (None, Some(id), _) => id.to_string(),
            (None, None, Provider::Local) => hushrun_ledger::new_ulid(self.now.millis()),
            (None, None, _) => {
                return Err(Failure::usage(anyhow!(
                    "on generic CI, set HUSHRUN_RUN_ID to identify the run"
                )));
            }
        };
        Ok(RunInfo {
            provider: self.run.provider,
            id,
            attempt: self.run.attempt.or(fallback_attempt).unwrap_or(1),
            url: self.run.url.clone(),
            trigger: trigger.or(self.run.trigger).unwrap_or(Trigger::Manual),
            job: self.run.job.clone(),
            job_id: self.run.job_id.clone(),
        })
    }
}

/// \[M18\] Points the ledger's transparency log, when it keeps one, at the log key hushrun.yaml
/// pins (T-9): from then on its receipts, listings and checkpoints are checked (L-11). Without
/// the key nothing is, and the ledger warns that it keeps a log hushrun.yaml doesn't name.
pub fn configure_log(ledger: &dyn Ledger, policy: &Policy) -> Result<(), Failure> {
    let Some(log) = ledger.log() else {
        return Ok(());
    };
    if log.configured() {
        return Ok(());
    }
    let key = match &policy.transparency_log {
        Some(declared) => Some(LogKey {
            origin: declared.origin.clone(),
            key: log_key(declared)?,
        }),
        None => None,
    };
    log.configure(key).map_err(ledger_failure)
}

/// The log key's raw bytes. hushrun.yaml refuses anything but Ed25519 there (T-9).
pub fn log_key(declared: &LedgerLog) -> Result<[u8; 32], Failure> {
    declared.key.ed25519().ok_or_else(|| {
        Failure::usage(anyhow!(
            "`transparency_log.key` is {}, and a log key is Ed25519",
            declared.key.algorithm()
        ))
    })
}

/// Ledger errors: unavailable or denied exit 6 (L-5); invalid requests are usage errors.
///
/// \[ADR 0032\] A write refused for the org's plan exits 6 too, as a failed write does (L-5).
/// Only a `check` event goes on without it (`events::record`, in `run CHECK` and `attest`);
/// hushrun's server refuses no other write for the plan, so any other one that is refused fails
/// the command.
pub fn ledger_failure(err: LedgerError) -> Failure {
    match err {
        LedgerError::Invalid(_) => Failure::usage(err),
        LedgerError::Unavailable(_) | LedgerError::Denied(_) | LedgerError::Quota(_) => {
            Failure::new(Exit::Ledger, err)
        }
        _ => Failure::new(Exit::Unexpected, err),
    }
}
