//! `hushrun.yaml` (§7.1, C-1 … C-8).
//!
//! Parsing rejects unknown keys at every level, duplicate keys and oversized files; validation
//! then checks references, cycles and the rules of C-8. Every error carries
//! `file:line:column`.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::json;

use crate::agent;
use crate::counts_from::{self, Raw, Rules};
use crate::digest::Digest;
use crate::event::{ArtifactType, Network};
use crate::glob::Pattern;
use crate::ledger_url::LedgerUrl;
use crate::names::{ArtifactName, CheckName, ScopeName, env_key};
use crate::run::Trigger;
use crate::scope::{External, Scope, Scopes};
use crate::time::Dur;
use crate::transparency::{LedgerLog, RawLedgerLog, parse_ledger_log};
use crate::yaml::{self, ConfigError, ConfigErrors, Errors, Map, Pos, Spanned};

/// The default name of the policy file, at the repository root.
pub const POLICY_FILE: &str = "hushrun.yaml";

/// A validated `hushrun.yaml`.
#[derive(Debug, Clone)]
pub struct Policy {
    pub repository: String,
    /// `repository_url`: where the repository's events go (C-3).
    pub ledger: Option<LedgerUrl>,
    /// \[ADR 0037\] `backend`: where its `hushrun run`s share results (C-3, C-11).
    pub backend: Option<LedgerUrl>,
    pub scopes: Scopes,
    pub checks: BTreeMap<CheckName, CheckDef>,
    pub artifacts: BTreeMap<ArtifactName, ArtifactDef>,
    /// \[ADR 0062\] `deploy`: the repository's one pipeline, when it deploys with hushrun (C-7).
    pub deploy: Option<DeployDef>,
    /// \[ADR 0045\] Every `counts_from` (C-25).
    pub counts_from: Rules,
    /// \[ADR 0047\] Each check's `reports` (C-5, C-26). Not part of its definition (E-6).
    pub reports: BTreeMap<CheckName, Reports>,
    /// \[ADR 0068\] Each check's `tests` that isn't `off` (C-5, C-30). Not part of its definition
    /// (E-6).
    pub tests: BTreeMap<CheckName, crate::units::Tests>,
    /// `sha256:` of the file's bytes (`policyDigest`, E-4).
    pub digest: Digest,
    /// \[M18\] `transparency_log`: the server's log key, as the repository pins it (T-9).
    pub transparency_log: Option<LedgerLog>,
    /// \[ADR 0062\] Keys this release reads past without using them, in the order written.
    pub retired: Vec<Retired>,
    /// `build: env:`: variables every compile of the repository's build blocks is keyed by, in
    /// both forms of `hushrun run`, sorted and once each (B-35). Not part of any check's
    /// definition (E-6).
    pub build_env: Vec<String>,
    /// The file name used in error messages.
    pub file: String,
    /// The file's text, for `plan --explain-json`.
    pub text: String,
}

/// \[ADR 0062\] A key of an older hushrun.yaml that this release parses and ignores, such as
/// `stages`: named once per command, so the file can be fixed, and never a reason to stop skipping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retired {
    /// The key's path, such as `stages`, `checks.unit.kind` or `artifacts.image.locations`.
    pub key: String,
    /// Where its value was written.
    pub pos: Pos,
    /// What to write instead, a sentence without a final period.
    pub instead: &'static str,
}

impl Retired {
    /// `file:line:column: `key` is ignored since hushrun 0.9.0: instead`.
    pub fn notice(&self, file: &str) -> String {
        ConfigError::new(
            file,
            self.pos,
            format!(
                "`{}` is ignored since hushrun 0.9.0, which has no stages: {}",
                self.key, self.instead
            ),
        )
        .to_string()
    }
}

/// \[ADR 0047\] `checks.<name>.reports`: the report files a check writes, and whether CI may skip
/// it when a later step of its job reads them (C-26).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reports {
    /// Report files it writes, besides those hushrun finds: globs from the directory it runs in.
    pub files: Vec<String>,
    pub ci: ReportsCi,
}

/// What CI does with a check whose reports a later step reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportsCi {
    /// It runs every time, so that the step finds what it reads.
    #[default]
    Run,
    /// It skips on a pass: the repository's readers handle a skip.
    Skip,
}

/// The most globs `reports.files` takes.
pub const MOST_REPORT_FILES: usize = 32;

/// \[ADR 0062\] What every check's definition object (E-6) says it validates. Checks had kinds
/// while there were stages; every recorded definition digest holds this one.
pub const DEFINITION_KIND: &str = "source";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shell {
    Sh,
    Bash,
    Pwsh,
    Cmd,
}

impl Shell {
    pub fn as_str(self) -> &'static str {
        match self {
            Shell::Sh => "sh",
            Shell::Bash => "bash",
            Shell::Pwsh => "pwsh",
            Shell::Cmd => "cmd",
        }
    }
}

/// `checks.<name>` (C-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckDef {
    pub run: Option<String>,
    pub shell: Option<Shell>,
    pub image: Option<String>,
    pub platform: Option<String>,
    /// Sorted variable names; values are never recorded.
    pub env: Vec<String>,
    pub network: Option<Network>,
    pub timeout: Option<Dur>,
    pub retries: u32,
    pub max_age: Option<Dur>,
    pub version: Option<String>,
    /// E-6: `sha256:` of the JCS form of the definition object.
    pub definition_digest: Digest,
}

/// `artifacts.<name>` (C-6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDef {
    pub artifact_type: ArtifactType,
    pub scope: ScopeName,
    /// \[ADR 0062\] Where it is deployed from: a repository without tag or digest for OCI.
    pub location: String,
}

/// `deploy.on_repository_error` (C-7, L-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnLedgerError {
    #[default]
    Fail,
    Full,
}

/// \[ADR 0062\] `deploy` (C-7): how the repository's one pipeline deploys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployDef {
    /// P-8: the only branch it deploys from, when it names one.
    pub branch: Option<String>,
    /// The checks a deploy needs, when the file lists them; see [`Policy::required`].
    pub require: Option<Vec<CheckName>>,
    pub lease: Dur,
    pub dedupe: Vec<Trigger>,
    pub allow_force: bool,
    pub on_ledger_error: OnLedgerError,
}

impl Default for DeployDef {
    fn default() -> Self {
        DeployDef {
            branch: None,
            require: None,
            lease: DEFAULT_LEASE,
            dedupe: vec![Trigger::Push, Trigger::Upstream],
            allow_force: true,
            on_ledger_error: OnLedgerError::Fail,
        }
    }
}

pub const DEFAULT_LEASE: Dur = Dur::from_secs(3 * 3600);

// --- Raw (as written) ------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    version: Spanned<u64>,
    repository: Spanned<String>,
    #[serde(default)]
    repository_url: Option<Spanned<String>>,
    #[serde(default)]
    backend: Option<Spanned<String>>,
    #[serde(default)]
    scopes: Option<Spanned<Map<ScopeName, RawScope>>>,
    #[serde(default)]
    digest: Option<RawDigest>,
    #[serde(default)]
    checks: Map<CheckName, RawCheck>,
    #[serde(default)]
    artifacts: Map<ArtifactName, RawArtifact>,
    #[serde(default)]
    deploy: Option<Spanned<RawDeploy>>,
    /// \[ADR 0062\] Retired: read past, named, never used.
    #[serde(default)]
    stages: Option<Spanned<IgnoredAny>>,
    #[serde(default)]
    transparency_log: Option<Spanned<RawLedgerLog>>,
    #[serde(default = "counts_from::absent")]
    counts_from: Spanned<Raw>,
    #[serde(default)]
    build: Option<RawBuild>,
}

/// `build`: what the build blocks of every machine's runs take from the repository (B-35).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
    #[serde(default)]
    env: Vec<Spanned<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScope {
    #[serde(default = "default_include")]
    include: Spanned<Vec<Spanned<String>>>,
    #[serde(default)]
    exclude: Vec<Spanned<String>>,
    #[serde(default)]
    external: Vec<RawExternal>,
    #[serde(default = "counts_from::absent")]
    counts_from: Spanned<Raw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExternal {
    path: Spanned<String>,
    #[serde(default = "default_include")]
    include: Spanned<Vec<Spanned<String>>>,
    #[serde(default)]
    exclude: Vec<Spanned<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDigest {
    #[serde(default)]
    exclude: Vec<Spanned<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCheck {
    /// \[ADR 0062\] Retired: every check is a source check.
    #[serde(default)]
    kind: Option<Spanned<IgnoredAny>>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    shell: Option<Shell>,
    #[serde(default)]
    image: Option<Spanned<String>>,
    #[serde(default)]
    platform: Option<Spanned<String>>,
    #[serde(default)]
    env: Vec<Spanned<String>>,
    #[serde(default)]
    network: Option<Network>,
    #[serde(default)]
    timeout: Option<Dur>,
    #[serde(default)]
    retries: Option<u32>,
    #[serde(default)]
    max_age: Option<Dur>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default = "counts_from::absent")]
    counts_from: Spanned<Raw>,
    /// \[ADR 0047\]
    #[serde(default)]
    reports: Option<RawReports>,
    /// \[ADR 0068\] `off`, `observe` or `reuse`, or `{mode, ignore, always}`.
    #[serde(default = "counts_from::absent")]
    tests: Spanned<Raw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReports {
    #[serde(default)]
    files: Option<Spanned<Vec<Spanned<String>>>>,
    #[serde(default)]
    ci: ReportsCi,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArtifact {
    #[serde(rename = "type")]
    artifact_type: ArtifactType,
    #[serde(default)]
    scope: Option<Spanned<ScopeName>>,
    #[serde(default)]
    location: Option<Spanned<String>>,
    /// \[ADR 0062\] Retired: one location per stage.
    #[serde(default)]
    locations: Option<Spanned<IgnoredAny>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeploy {
    #[serde(default)]
    branch: Option<Spanned<String>>,
    #[serde(default)]
    require: Option<Spanned<Vec<Spanned<String>>>>,
    #[serde(default)]
    lease: Option<Dur>,
    #[serde(default)]
    dedupe: Option<Vec<Trigger>>,
    #[serde(default)]
    allow_force: Option<Spanned<bool>>,
    #[serde(default, rename = "on_repository_error")]
    on_ledger_error: Option<Spanned<OnLedgerError>>,
}

fn default_include() -> Spanned<Vec<Spanned<String>>> {
    Spanned::new(
        vec![Spanned::new("**".to_string(), Pos::UNKNOWN)],
        Pos::UNKNOWN,
    )
}

// --- Parsing and validation ------------------------------------------------------------------

impl Policy {
    /// Parses and validates `hushrun.yaml`. `file` names it in error messages.
    pub fn parse(file: &str, bytes: &[u8]) -> Result<Policy, ConfigErrors> {
        // \[ADR 0046\] Before anything can quote it: what this file says gets printed, such as a
        // rule's values, and the file is in git.
        let text = String::from_utf8_lossy(bytes);
        if let Some(line) = text.lines().position(agent::is_agent_string) {
            let pos = Pos {
                line: line as u64 + 1,
                column: 1,
            };
            let why = "this line holds a HUSHRUN_AGENT string, which holds a private key: take it out, and revoke its token";
            return Err(ConfigErrors(vec![ConfigError::new(file, pos, why)]));
        }
        let raw: RawPolicy = yaml::parse(file, bytes)?;
        let mut errors = Errors::new(file);

        // C-1
        if raw.version.value != 1 {
            errors.push(raw.version.pos, "`version` must be 1");
        }
        // C-2
        if let Err(reason) = check_repository(&raw.repository.value) {
            errors.push(raw.repository.pos, reason);
        }
        // C-3
        let mut parse_url = |l: &Spanned<String>| match LedgerUrl::parse(&l.value) {
            Ok(url) => Some(url),
            Err(err) => {
                errors.push(l.pos, err.to_string());
                None
            }
        };
        let ledger = raw.repository_url.as_ref().and_then(&mut parse_url);
        let backend = raw.backend.as_ref().and_then(&mut parse_url);

        // C-25
        let mut rules = Rules {
            top: counts_from::level(&raw.counts_from, &mut errors),
            ..Rules::default()
        };

        // C-4, D-7
        let global_exclude = raw
            .digest
            .as_ref()
            .map(|d| compile_all(&d.exclude, &mut errors))
            .unwrap_or_default();
        let scopes = match &raw.scopes {
            None => Scopes::default_scope(global_exclude),
            Some(declared) => {
                if declared.value.is_empty() {
                    errors.push(
                        declared.pos,
                        "`scopes` is empty; remove it to use the single scope `default`",
                    );
                }
                let mut scopes = Vec::new();
                for (name, _, scope) in declared.value.iter() {
                    if scope.include.value.is_empty() {
                        errors.push(
                            scope.include.pos,
                            format!("scope `{name}` has an empty `include` list"),
                        );
                    }
                    let mut external: Vec<External> = Vec::new();
                    for input in &scope.external {
                        // Ledger evidence is about the repository's own files, and a plan
                        // can't say what a sibling held (ADR 0037, ADR 0062).
                        if raw.deploy.is_some() {
                            errors.push(
                                input.path.pos,
                                format!(
                                    "scope `{name}` declares `external`, and this file has `deploy`: a deploy's evidence covers the repository's own files, so `external` works with `hushrun run NAME -- COMMAND`"
                                ),
                            );
                        }
                        if let Err(reason) = check_external_path(&input.path.value) {
                            errors.push(input.path.pos, reason);
                        }
                        if external.iter().any(|e| e.path == input.path.value) {
                            errors.push(
                                input.path.pos,
                                format!("`{}` is listed twice", input.path.value),
                            );
                        }
                        if input.include.value.is_empty() {
                            errors.push(
                                input.include.pos,
                                format!("`{}` has an empty `include` list", input.path.value),
                            );
                        }
                        external.push(External {
                            path: input.path.value.clone(),
                            include: compile_all(&input.include.value, &mut errors),
                            exclude: compile_all(&input.exclude, &mut errors),
                        });
                    }
                    external.sort_by(|a, b| a.path.cmp(&b.path));
                    if let Some(level) = counts_from::level(&scope.counts_from, &mut errors) {
                        rules.scopes.insert(name.clone(), level);
                    }
                    scopes.push(Scope {
                        name: name.clone(),
                        include: compile_all(&scope.include.value, &mut errors),
                        exclude: compile_all(&scope.exclude, &mut errors),
                        external,
                    });
                }
                check_env_collisions(
                    "scope",
                    declared.value.iter().map(|(n, p, _)| (n.as_str(), p)),
                    &mut errors,
                );
                Scopes::new(scopes, global_exclude)
            }
        };

        // \[ADR 0062\] What older files said that this release reads past.
        let mut retired = Vec::new();
        if let Some(stages) = &raw.stages {
            retired.push(Retired {
                key: "stages".into(),
                pos: stages.pos,
                instead: "delete it; a stage's `branch`, `require` and `lease` go under `deploy`",
            });
        }

        // C-5
        let mut checks = BTreeMap::new();
        let mut reports = BTreeMap::new();
        let mut tests = BTreeMap::new();
        for (name, pos, check) in raw.checks.iter() {
            if let Some(kind) = &check.kind {
                retired.push(Retired {
                    key: format!("checks.{name}.kind"),
                    pos: kind.pos,
                    instead: "delete it; every check is a source check",
                });
            }
            checks.insert(name.clone(), validate_check(name, pos, check, &mut errors));
            if let Some(level) = counts_from::level(&check.counts_from, &mut errors) {
                rules.checks.insert(name.clone(), level);
            }
            if let Some(raw) = &check.reports {
                reports.insert(name.clone(), validate_reports(name, raw, &mut errors));
            }
            if let Some(on) = validate_tests(name, &check.tests, &mut errors) {
                tests.insert(name.clone(), on);
            }
        }
        check_env_collisions(
            "check",
            raw.checks.iter().map(|(n, p, _)| (n.as_str(), p)),
            &mut errors,
        );

        // C-7
        let deploy = raw
            .deploy
            .as_ref()
            .map(|deploy| validate_deploy(&deploy.value, &checks, &mut errors));

        // C-6
        let mut artifacts = BTreeMap::new();
        check_env_collisions(
            "artifact",
            raw.artifacts.iter().map(|(n, p, _)| (n.as_str(), p)),
            &mut errors,
        );
        for (name, pos, artifact) in raw.artifacts.iter() {
            if let Some(locations) = &artifact.locations {
                retired.push(Retired {
                    key: format!("artifacts.{name}.locations"),
                    pos: locations.pos,
                    instead: "give the artifact one `location`",
                });
            }
            let Some(location) = &artifact.location else {
                // An artifact of an older file, with only `locations`: left out, and named above.
                if artifact.locations.is_none() {
                    errors.push(
                        pos,
                        format!("artifact `{name}` needs a `location`, such as ghcr.io/acme/app"),
                    );
                }
                continue;
            };
            let scope = match &artifact.scope {
                Some(scope) => {
                    if scopes.get(scope.value.as_str()).is_none() {
                        errors.push(scope.pos, format!("unknown scope `{}`", scope.value));
                    }
                    scope.value.clone()
                }
                None => {
                    let default = ScopeName::default_scope();
                    if scopes.get(default.as_str()).is_none() {
                        errors.push(
                            location.pos,
                            format!(
                                "artifact `{name}` needs a `scope`: there is no scope `default`"
                            ),
                        );
                    }
                    default
                }
            };
            if let Err(reason) = check_location(artifact.artifact_type, &location.value) {
                errors.push(location.pos, reason);
            }
            artifacts.insert(
                name.clone(),
                ArtifactDef {
                    artifact_type: artifact.artifact_type,
                    scope,
                    location: location.value.clone(),
                },
            );
        }

        // C-8: a deploy's evidence is about the files; only a run has a machine a rule reads.
        if raw.deploy.is_some() {
            let top = std::iter::once(&raw.counts_from);
            let scoped = raw.scopes.iter().flat_map(|s| s.value.values());
            let at = top
                .chain(scoped.map(|scope| &scope.counts_from))
                .chain(raw.checks.values().map(|check| &check.counts_from));
            for level in at.filter(|level| !matches!(level.value, Raw::Absent)) {
                errors.push(
                    level.pos,
                    "`counts_from` is here, and this file has `deploy`: a deploy's evidence has its own rules, so `counts_from` works with `hushrun run NAME -- COMMAND`",
                );
            }
        }

        // T-9, T-13
        let transparency_log = raw
            .transparency_log
            .as_ref()
            .and_then(|log| parse_ledger_log(&log.value, &mut errors));

        // B-35
        let mut build_env: Vec<String> = Vec::new();
        for var in raw.build.iter().flat_map(|build| &build.env) {
            if !is_env_name(&var.value) {
                errors.push(
                    var.pos,
                    format!("{:?} is not an environment variable name", var.value),
                );
            } else if build_env.contains(&var.value) {
                errors.push(var.pos, format!("`build.env` names {} twice", var.value));
            } else {
                build_env.push(var.value.clone());
            }
        }
        build_env.sort();

        retired.sort_by_key(|r| (r.pos.line, r.pos.column));
        errors.finish(Policy {
            repository: raw.repository.value.clone(),
            ledger,
            backend,
            scopes,
            checks,
            artifacts,
            deploy,
            counts_from: rules,
            reports,
            tests,
            digest: Digest::of(bytes),
            transparency_log,
            retired,
            build_env,
            file: file.to_string(),
            text: String::from_utf8_lossy(bytes).into_owned(),
        })
    }

    /// \[ADR 0062\] The checks a commit needs: `deploy.require` when the file lists it, else every
    /// check with `run`, in name order. The one definition `run --all`, `ready`, `plan`, `status`
    /// and the server's index use.
    pub fn required(&self) -> Vec<CheckName> {
        match self.deploy.as_ref().and_then(|d| d.require.as_ref()) {
            Some(require) => require.clone(),
            None => self
                .checks
                .iter()
                .filter(|(_, check)| check.run.is_some())
                .map(|(name, _)| name.clone())
                .collect(),
        }
    }

    /// \[ADR 0062\] Whether the repository deploys with hushrun: the file has `deploy`, or still
    /// has `stages`, which its author is to move there. Either way it holds what a deploy needs, and
    /// nothing `deploy` refuses (C-8).
    pub fn deploys(&self) -> bool {
        self.deploy.is_some() || self.retired.iter().any(|r| r.key == "stages")
    }

    /// The `deploy` settings, or their defaults when the file has none.
    pub fn deploy_or_default(&self) -> DeployDef {
        self.deploy.clone().unwrap_or_default()
    }

    /// The artifacts of `scope`.
    pub fn artifacts_of(
        &self,
        scope: &ScopeName,
    ) -> impl Iterator<Item = (&ArtifactName, &ArtifactDef)> {
        self.artifacts
            .iter()
            .filter(move |(_, a)| &a.scope == scope)
    }
}

/// C-7: `deploy`.
fn validate_deploy(
    raw: &RawDeploy,
    checks: &BTreeMap<CheckName, CheckDef>,
    errors: &mut Errors,
) -> DeployDef {
    let mut require: Option<Vec<CheckName>> = None;
    if let Some(list) = &raw.require {
        let mut names = Vec::new();
        for entry in &list.value {
            if let Some((check, stage)) = entry.value.split_once('@') {
                errors.push(
                    entry.pos,
                    format!(
                        "`{}` names the stage `{stage}`, and hushrun 0.9 has no stages: require `{check}`, and run what checked `{stage}`'s deployment in the deploy step",
                        entry.value
                    ),
                );
                continue;
            }
            let Ok(check) = CheckName::new(&entry.value) else {
                errors.push(entry.pos, format!("{:?} is not a check name", entry.value));
                continue;
            };
            if !checks.contains_key(&check) {
                errors.push(entry.pos, format!("unknown check `{check}`"));
            } else if names.contains(&check) {
                errors.push(entry.pos, format!("`{check}` is required twice"));
            } else {
                names.push(check);
            }
        }
        require = Some(names);
    }
    if let Some(branch) = &raw.branch
        && (branch.value.is_empty() || branch.value.starts_with("refs/"))
    {
        errors.push(branch.pos, "`branch` is a branch name, such as main");
    }
    let defaults = DeployDef::default();
    DeployDef {
        branch: raw.branch.as_ref().map(|b| b.value.clone()),
        require,
        lease: raw.lease.unwrap_or(defaults.lease),
        dedupe: raw.dedupe.clone().unwrap_or(defaults.dedupe),
        allow_force: raw.allow_force.as_ref().is_none_or(|a| a.value),
        on_ledger_error: raw
            .on_ledger_error
            .as_ref()
            .map_or(OnLedgerError::Fail, |o| o.value),
    }
}

/// C-2: `host/path`, lowercase host, no scheme, no `.git`.
pub(crate) fn check_repository(repo: &str) -> Result<(), String> {
    let fail = |why: &str| {
        Err(format!(
            "`repository` {repo:?} {why}; write the canonical id, such as github.com/acme/app"
        ))
    };
    if repo.contains("://") || repo.starts_with("git@") {
        return fail("has a scheme");
    }
    if repo.ends_with(".git") {
        return fail("ends with .git");
    }
    let Some((host, path)) = repo.split_once('/') else {
        return fail("has no path");
    };
    if host.is_empty() || host != host.to_ascii_lowercase() {
        return fail("needs a lowercase host");
    }
    if path.is_empty() || path.ends_with('/') || path.split('/').any(str::is_empty) {
        return fail("has an empty path segment");
    }
    if repo
        .chars()
        .any(|c| c.is_whitespace() || c == '#' || c == '@' || c == '?')
    {
        return fail("has whitespace, `#`, `@` or `?`");
    }
    Ok(())
}

/// C-4: an external input is a directory outside the repository, relative to its root and
/// `/`-separated, so that it reads the same on every machine: `../cli`.
fn check_external_path(path: &str) -> Result<(), String> {
    let mut parts = path.split('/');
    let leaves = parts.next() == Some("..");
    if !leaves
        || path.contains('\\')
        || parts.any(|part| part.is_empty() || part == ".")
        || path.split('/').all(|part| part == "..")
    {
        return Err(format!(
            "`external` path {path:?} must name a directory outside the repository, relative to its root and /-separated, such as ../cli"
        ));
    }
    Ok(())
}

/// C-6: an OCI location is a repository with no tag or digest.
fn check_location(artifact_type: ArtifactType, location: &str) -> Result<(), String> {
    if location.is_empty() || location.chars().any(char::is_whitespace) {
        return Err("a location can't be empty or contain whitespace".into());
    }
    if artifact_type == ArtifactType::Oci {
        let last_segment = location.rsplit('/').next().unwrap_or(location);
        if location.contains('@') || (location.contains('/') && last_segment.contains(':')) {
            return Err(format!(
                "OCI location {location:?} must be a repository with no tag or digest"
            ));
        }
        if !location.contains('/') {
            return Err(format!(
                "OCI location {location:?} needs a registry host, such as us-central1-docker.pkg.dev/project/repo/image"
            ));
        }
    }
    Ok(())
}

/// Whether `name` is an environment variable's name.
pub fn is_env_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// \[ADR 0047\] A check's `reports` (C-5): at most 32 globs, each relative.
fn validate_reports(name: &CheckName, raw: &RawReports, errors: &mut Errors) -> Reports {
    let mut files = Vec::new();
    if let Some(globs) = &raw.files {
        if globs.value.len() > MOST_REPORT_FILES {
            errors.push(
                globs.pos,
                format!(
                    "check `{name}` lists {} report files, and `reports.files` takes at most {MOST_REPORT_FILES}",
                    globs.value.len()
                ),
            );
        }
        for glob in &globs.value {
            let text = glob.value.trim();
            let absolute = text.starts_with('/')
                || text.starts_with('\\')
                || text.as_bytes().get(1) == Some(&b':');
            if text.is_empty() {
                errors.push(
                    glob.pos,
                    format!("check `{name}` lists an empty report file"),
                );
            } else if absolute {
                errors.push(
                    glob.pos,
                    format!(
                        "check `{name}` lists the report file {text:?}, which isn't relative: report files are named from the directory the check runs in"
                    ),
                );
            }
            files.push(text.to_string());
        }
    }
    Reports { files, ci: raw.ci }
}

/// \[ADR 0068\] A check's `tests` (C-5): a mode, short, or `{mode, unit, reads, ignore, always}`,
/// with globs from the repository root. `None` when it is absent or `off`, or written wrongly,
/// which `errors` says. \[ADR 0080\] `reads: test` needs `unit: test`.
fn validate_tests(
    name: &CheckName,
    node: &Spanned<Raw>,
    errors: &mut Errors,
) -> Option<crate::units::Tests> {
    use crate::units::{Mode, Reads, Tests, Unit};
    let mode_of = |raw: &Raw, pos: Pos, errors: &mut Errors| match raw {
        Raw::Text(word) => Mode::from_word(word).or_else(|| {
            errors.push(
                pos,
                format!("check `{name}`'s `tests` is off, observe or reuse, not {word:?}"),
            );
            None
        }),
        _ => {
            errors.push(
                pos,
                format!("check `{name}`'s `tests` is off, observe or reuse"),
            );
            None
        }
    };
    let globs = |raw: &Spanned<Raw>, key: &str, errors: &mut Errors| -> Vec<Pattern> {
        let Raw::List(items) = &raw.value else {
            errors.push(
                raw.pos,
                format!(
                    "check `{name}`'s `tests.{key}` is a list of globs from the repository root"
                ),
            );
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| match &item.value {
                Raw::Text(text) => match Pattern::new(text) {
                    Ok(pattern) => Some(pattern),
                    Err(err) => {
                        errors.push(item.pos, err.to_string());
                        None
                    }
                },
                _ => {
                    errors.push(
                        item.pos,
                        format!("check `{name}`'s `tests.{key}` holds a glob, as a string"),
                    );
                    None
                }
            })
            .collect()
    };
    let tests = match &node.value {
        Raw::Absent => return None,
        Raw::Map(keys) => {
            let mut tests = Tests::default();
            let mut mode = None;
            for (key, pos, value) in keys.iter() {
                match key.as_str() {
                    "mode" => mode = mode_of(&value.value, value.pos, errors),
                    "ignore" => tests.ignore = globs(value, "ignore", errors),
                    "always" => tests.always = globs(value, "always", errors),
                    "unit" => match &value.value {
                        Raw::Text(word) if Unit::from_word(word).is_some() => {
                            tests.unit = Unit::from_word(word).unwrap_or_default();
                        }
                        _ => errors.push(
                            value.pos,
                            format!("check `{name}`'s `tests.unit` is file or test"),
                        ),
                    },
                    "reads" => match &value.value {
                        Raw::Text(word) if Reads::from_word(word).is_some() => {
                            tests.reads = Reads::from_word(word).unwrap_or_default();
                        }
                        _ => errors.push(
                            value.pos,
                            format!("check `{name}`'s `tests.reads` is file or test"),
                        ),
                    },
                    other => errors.push(
                        pos,
                        format!("check `{name}`'s `tests` has no key `{other}`: it takes mode, unit, reads, ignore and always"),
                    ),
                }
            }
            // \[ADR 0080\] What a test read is a question only for a check whose unit is a test.
            if tests.reads == Reads::Test && tests.unit == Unit::File {
                let pos = keys.get("reads").map_or(node.pos, |value| value.pos);
                errors.push(
                    pos,
                    format!(
                        "check `{name}`'s `tests.reads: test` needs `tests.unit: test`: write `unit: test` beside it, or `reads: file`"
                    ),
                );
                return None;
            }
            match mode {
                Some(mode) => tests.mode = mode,
                None if keys.get("mode").is_none() => {
                    errors.push(
                        node.pos,
                        format!("check `{name}`'s `tests` needs a `mode`: off, observe or reuse"),
                    );
                    return None;
                }
                None => return None,
            }
            tests
        }
        raw => Tests {
            mode: mode_of(raw, node.pos, errors)?,
            ..Tests::default()
        },
    };
    tests.is_on().then_some(tests)
}

fn validate_check(name: &CheckName, _pos: Pos, check: &RawCheck, errors: &mut Errors) -> CheckDef {
    if let Some(image) = &check.image
        && crate::event::pinned_digest(&image.value).is_none()
    {
        errors.push(
            image.pos,
            format!(
                "check `{name}` pins image {:?}, which isn't pinned by digest (…@sha256:…)",
                image.value
            ),
        );
    }
    let mut env: Vec<String> = Vec::new();
    for var in &check.env {
        if !is_env_name(&var.value) {
            errors.push(
                var.pos,
                format!("{:?} is not an environment variable name", var.value),
            );
        }
        if env.contains(&var.value) {
            errors.push(var.pos, format!("`{}` is listed twice", var.value));
        }
        env.push(var.value.clone());
    }
    env.sort();
    let mut definition = serde_json::Map::new();
    definition.insert("name".into(), json!(name));
    definition.insert("kind".into(), json!(DEFINITION_KIND));
    if let Some(run) = &check.run {
        definition.insert("run".into(), json!(run));
    }
    if let Some(shell) = check.shell {
        definition.insert("shell".into(), json!(shell.as_str()));
    }
    if let Some(image) = &check.image {
        definition.insert("image".into(), json!(image.value));
    }
    if let Some(platform) = &check.platform {
        definition.insert("platform".into(), json!(platform.value));
    }
    if !check.env.is_empty() {
        definition.insert("env".into(), json!(env));
    }
    if let Some(network) = check.network {
        definition.insert("network".into(), json!(network.as_str()));
    }
    if let Some(version) = &check.version {
        definition.insert("version".into(), json!(version));
    }
    let definition_digest = crate::jcs::digest(&serde_json::Value::Object(definition))
        .expect("definitions hold only strings");
    CheckDef {
        run: check.run.clone(),
        shell: check.shell,
        image: check.image.as_ref().map(|i| i.value.clone()),
        platform: check.platform.as_ref().map(|p| p.value.clone()),
        env,
        network: check.network,
        timeout: check.timeout,
        retries: check.retries.unwrap_or(0),
        max_age: check.max_age,
        version: check.version.clone(),
        definition_digest,
    }
}

/// L-2: two names of one kind that map to the same environment key are an error.
fn check_env_collisions<'a>(
    kind: &str,
    names: impl Iterator<Item = (&'a str, Pos)>,
    errors: &mut Errors,
) {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for (name, pos) in names {
        if let Some(other) = seen.insert(env_key(name), name) {
            errors.push(
                pos,
                format!(
                    "{kind} names `{other}` and `{name}` collide as the environment key {}",
                    env_key(name)
                ),
            );
        }
    }
}

fn compile_all(patterns: &[Spanned<String>], errors: &mut Errors) -> Vec<Pattern> {
    patterns
        .iter()
        .filter_map(|p| match Pattern::new(&p.value) {
            Ok(pattern) => Some(pattern),
            Err(err) => {
                errors.push(p.pos, err.to_string());
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
