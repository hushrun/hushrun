//! What init writes for promotion: `hushrun.yaml`, once the repository has a URL on hushrun,
//! naming the repository, that URL and the server's log key (C-3, T-9). \[ADR 0031\] It writes
//! no trust: the events that count are the ones members' machines and the repository's CI sign,
//! as the server names them (T-15), so nothing needs merging before they do.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::anyhow;

use hushrun_core::counts_from::{Level, Rule};
use hushrun_core::ledger_url::LedgerUrl;
use hushrun_core::names::ScopeName;
use hushrun_core::policy::{POLICY_FILE, Policy};

use crate::credentials::{self, Credentials};
use crate::exit::{Exit, Failure};

use super::projects::Project;
use super::scripts;

pub use super::workflows::GITLAB_CI;

/// `.gitlab-ci.yml`, when a line of it runs one of `wrapped` through a package manager at the
/// repository root: GitLab's one pipeline.
pub fn gitlab_pipeline(root: &Path, wrapped: &[String]) -> Option<String> {
    let text = std::fs::read_to_string(root.join(GITLAB_CI)).ok()?;
    text.lines()
        .map(|line| {
            let line = line.trim_start();
            line.strip_prefix("- ").unwrap_or(line)
        })
        .filter(|line| !line.starts_with('#'))
        .flat_map(scripts::scripts_run)
        .any(|script| wrapped.contains(&script))
        .then(|| GITLAB_CI.to_string())
}

/// What the plan says of a `hushrun.yaml` that has all [`write_policy`] writes.
pub const KEPT: &str = "keeps it as it is";

/// The plan's lines for `hushrun.yaml`, given the file as it is.
pub fn plan(repository: &str, existing: Option<&Policy>) -> Vec<String> {
    match existing {
        None => vec![format!(
            "a new file: repository {repository}, its URL on hushrun, and the key that verifies hushrun's log"
        )],
        Some(policy) => {
            let mut adds = Vec::new();
            if policy.ledger.is_none() {
                adds.push("its URL on hushrun");
            }
            if policy.transparency_log.is_none() {
                adds.push("the key that verifies hushrun's log");
            }
            let mut lines = Vec::new();
            if !adds.is_empty() {
                lines.push(format!("adds {}", adds.join(" and ")));
            }
            let (kinds, left) = retired(policy);
            if !kinds.is_empty() {
                lines.push(format!(
                    "removes {}, which hushrun 0.9 reads past: every check is a source check",
                    kinds.join(", ")
                ));
            }
            if !left.is_empty() {
                lines.push(format!(
                    "leaves {} for you to move: hushrun 0.9 doesn't read them, and `hushrun plan` refuses the file until `deploy` and one `location` per artifact say what they said",
                    left.join(" and ")
                ));
            }
            if lines.is_empty() {
                lines.push(KEPT.into());
            }
            lines
        }
    }
}

/// Whether init would change `existing`.
pub fn changes(existing: Option<&Policy>) -> bool {
    existing.is_none_or(|p| {
        p.ledger.is_none() || p.transparency_log.is_none() || !retired(p).0.is_empty()
    })
}

/// \[ADR 0062\] What `policy` says that hushrun 0.9 reads past: each check's `kind`, which init
/// removes, and the keys it leaves to a person, `stages` and `locations`, whose meaning moves to
/// `deploy` and `location`: removing them would drop the gates they set, which is why `plan`
/// refuses them.
fn retired(policy: &Policy) -> (Vec<String>, Vec<String>) {
    let mut kinds = Vec::new();
    let mut left = Vec::new();
    for key in &policy.retired {
        if key.key.ends_with(".kind") {
            kinds.push(format!("`{}`", key.key));
            continue;
        }
        let name = if key.key == "stages" {
            "`stages`"
        } else {
            "`locations`"
        };
        if !left.iter().any(|l| l == name) {
            left.push(name.to_string());
        }
    }
    (kinds, left)
}

/// `text` without each check's `kind` [`retired`] names: its line, written on one of its own,
/// or its entry, in a flow map such as `unit: {kind: source, run: x}`.
fn without_kinds(text: &str, policy: &Policy) -> String {
    let at: Vec<usize> = policy
        .retired
        .iter()
        .filter(|r| r.key.ends_with(".kind"))
        .map(|r| (r.pos.line as usize).saturating_sub(1))
        .collect();
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if !at.contains(&i) {
            out.push_str(line);
        } else if line.trim_start().starts_with("kind:") {
            // A line of its own: gone.
        } else {
            out.push_str(&without_flow_kind(line));
        }
    }
    out
}

/// `line` without the `kind: <word>` entry of its flow map.
fn without_flow_kind(line: &str) -> String {
    let Some(start) = line.find("kind:") else {
        return line.to_string();
    };
    let rest = &line[start + "kind:".len()..];
    let value = rest.trim_start();
    let word = value
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(value.len());
    let mut end = start + "kind:".len() + (rest.len() - value.len()) + word;
    let after = &line[end..];
    if let Some(comma) = after.strip_prefix(',') {
        // `kind: x, run: y`: the entry and its comma.
        end += 1 + (comma.len() - comma.trim_start().len());
        let gap = if line[..start].ends_with(',') {
            " "
        } else {
            ""
        };
        return format!("{}{gap}{}", &line[..start], &line[end..]);
    }
    // `run: y, kind: x` or `{kind: x}`: the entry and the comma before it.
    let before = line[..start].trim_end();
    let cut = before.strip_suffix(',').map_or(start, str::len);
    format!("{}{}", &line[..cut], &line[end..])
}

/// The server's log origin and key, from `GET {repository_url}`, with the login saved for its
/// origin: trust on first use, pinned by the commit people review (T-9). `None` when the
/// server doesn't say, or won't answer this login.
pub fn log_key(ledger: &str) -> Option<(String, String)> {
    let url = LedgerUrl::parse(ledger).ok()?;
    let token = Credentials::load()
        .ok()?
        .login_for(&url)
        .map(|login| login.token.clone());
    let options = hushrun_ledger::Options {
        token: token.map(hushrun_ledger::Secret::new),
        budget: Some(std::time::Duration::from_secs(10)),
        ..hushrun_ledger::Options::default()
    };
    match hushrun_ledger::describe(&url, &options) {
        Ok(info) => info.log.and_then(|log| Some((log.origin, log.key?))),
        Err(e) => {
            tracing::debug!(
                "{url} didn't say its log key to {}: {e}",
                credentials::origin(&url)
            );
            None
        }
    }
}

/// What writing `hushrun.yaml` did.
pub enum Written {
    New,
    Updated,
    Kept,
}

/// The `transparency_log` lines for `log`.
fn log_lines(log: &(String, String)) -> String {
    format!(
        "transparency_log:   # the server's log key\n  origin: {}\n  key: \"{}\"\n",
        log.0, log.1
    )
}

/// Writes `hushrun.yaml` at `root`: a new file naming the repository, its `repository_url` and
/// the log key, or what of those an existing one lacks, added as lines, keeping every other byte.
pub fn write_policy(
    root: &Path,
    repository: &str,
    ledger: &str,
    log: Option<&(String, String)>,
) -> Result<Written, Failure> {
    let path = root.join(POLICY_FILE);
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(Failure::new(
                Exit::Unexpected,
                anyhow!("reading {}: {e}", path.display()),
            ));
        }
    };
    let (text, written) = match existing {
        Some(text) => {
            let policy = Policy::parse(POLICY_FILE, text.as_bytes())
                .map_err(|e| Failure::usage(anyhow!("{e}")))?;
            let mut text = text;
            let mut changed = false;
            if !retired(&policy).0.is_empty() {
                let without = without_kinds(&text, &policy);
                // A file that wouldn't read without them keeps them.
                if Policy::parse(POLICY_FILE, without.as_bytes()).is_ok() {
                    text = without;
                    changed = true;
                }
            }
            if policy.ledger.is_none() {
                text = with_ledger(&text, ledger);
                changed = true;
            }
            if let (None, Some(log)) = (&policy.transparency_log, log) {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push('\n');
                text.push_str(&log_lines(log));
                changed = true;
            }
            if !changed {
                return Ok(Written::Kept);
            }
            (text, Written::Updated)
        }
        None => {
            let mut text = format!(
                "# hushrun.yaml: where this repository's events go.\nversion: 1\nrepository: {repository}\nrepository_url: {ledger}\n"
            );
            if let Some(log) = log {
                text.push('\n');
                text.push_str(&log_lines(log));
            }
            (text, Written::New)
        }
    };
    // What init writes must read back as a valid policy, or nothing is written.
    Policy::parse(POLICY_FILE, text.as_bytes()).map_err(|e| {
        Failure::new(
            Exit::Unexpected,
            anyhow!("the hushrun.yaml init would write doesn't parse ({e}); please report this"),
        )
    })?;
    super::write(&path, &text).map_err(|e| Failure::new(Exit::Unexpected, e))?;
    Ok(written)
}

/// `text` with `repository_url: <url>` after its `repository:` line.
fn with_ledger(text: &str, ledger: &str) -> String {
    after_repository(text, &format!("repository_url: {ledger}"))
}

/// `text` with `added`, a line, after its `repository:` line.
fn after_repository(text: &str, added: &str) -> String {
    let mut out = String::new();
    let mut done = false;
    for line in text.split_inclusive('\n') {
        out.push_str(line);
        if !done && line.starts_with("repository:") {
            if !line.ends_with('\n') {
                out.push('\n');
            }
            let _ = writeln!(out, "{added}");
            done = true;
        }
    }
    out
}

/// \[ADR 0041\] `text`, a hushrun.yaml, once its repository goes from the org `from` to the org
/// at `backend`: its `backend:` line, any comment on it included, names that org, and the URL and
/// log key `from` gave it go, for [`write_policy`] to add the new org's. Every other line stays. A
/// URL that isn't `from`'s, which someone set themselves, stays with its log key. The result must
/// parse. `None` when nothing changes.
pub fn moved(text: &str, backend: &str, from: &str) -> Result<Option<String>, Failure> {
    let policy =
        Policy::parse(POLICY_FILE, text.as_bytes()).map_err(|e| Failure::usage(anyhow!("{e}")))?;
    let stale = policy
        .ledger
        .as_ref()
        .is_some_and(|url| url.to_string().contains(&format!("/l/{from}/")));
    let mut out = String::new();
    let mut in_log = false;
    // Blank lines and comments inside the log's block go with it, and ones after it stay.
    let mut held = String::new();
    for line in text.split_inclusive('\n') {
        if in_log && (line.trim().is_empty() || line.starts_with('#')) {
            held.push_str(line);
            continue;
        }
        if in_log && line.starts_with([' ', '\t']) {
            held.clear();
            continue;
        }
        in_log = false;
        out.push_str(&held);
        held.clear();
        if line.starts_with("backend:") {
            let end = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
            let _ = write!(out, "backend: {backend}{end}");
        } else if stale && line.starts_with("transparency_log:") {
            in_log = true;
            // The blank line init set it off with.
            if out.ends_with("\r\n\r\n") {
                out.truncate(out.len() - 2);
            } else if out.ends_with("\n\n") {
                out.pop();
            }
        } else if !(stale && line.starts_with("repository_url:")) {
            out.push_str(line);
        }
    }
    out.push_str(&held);
    if out == text {
        return Ok(None);
    }
    Policy::parse(POLICY_FILE, out.as_bytes()).map_err(|e| {
        Failure::new(
            Exit::Unexpected,
            anyhow!("the hushrun.yaml init would write doesn't parse ({e}); please report this"),
        )
    })?;
    Ok(Some(out))
}

/// What a split can't see, which the plan says whenever it splits.
const CANT_SEE: &str = "hushrun can't see a path built at run time, such as path.join(__dirname, \"..\", \"landing\"): add what such a check reads to its scope's include";

/// \[ADR 0045\] Why init wrote a rule, in the file and in the plan.
const MACOS_ONLY: &str =
    "on macOS in CI, with Xcode or Swift, so a pass on another system won't count";

/// \[ADR 0045\] Where a rule goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleLevel {
    /// The top of the file: the repository is one scope.
    Top,
    Scope(String),
}

/// \[ADR 0037\] What init adds to `hushrun.yaml` to run a repository's checks: where they share
/// results, for a repository with no package.json to say so, and its scopes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Setup {
    pub backend: Option<String>,
    pub projects: Vec<Project>,
    /// \[ADR 0045\] Where `counts_from: {os: macos}` goes.
    pub macos: Vec<RuleLevel>,
}

impl Setup {
    pub fn is_empty(&self) -> bool {
        self.backend.is_none() && self.projects.is_empty() && self.macos.is_empty()
    }

    /// \[ADR 0045\] This with `counts_from: {os: macos}` at `level`, or `None` when `existing`, or a
    /// new file, wouldn't read back with that rule there: a scope in flow style has no line to add
    /// it on. A rule is never worth failing init for, so the plan promises only what fits.
    pub fn with_macos(
        &self,
        level: RuleLevel,
        existing: Option<&str>,
        repository: &str,
    ) -> Option<Setup> {
        let mut setup = self.clone();
        setup.macos.push(level.clone());
        // Before a sign-in the backend is a placeholder, which this isn't about.
        let unnamed = Setup {
            backend: None,
            ..setup.clone()
        };
        let text = unnamed.apply(existing, repository);
        let policy = Policy::parse(POLICY_FILE, text.as_bytes()).ok()?;
        let macos = Level::Rule(Rule {
            os: vec!["macos".to_string()],
            ..Rule::default()
        });
        let there = match &level {
            RuleLevel::Top => policy.counts_from.top.as_ref(),
            RuleLevel::Scope(scope) => policy.counts_from.scopes.get(&ScopeName::new(scope).ok()?),
        };
        (there == Some(&macos)).then_some(setup)
    }

    /// What the plan says of it.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(backend) = &self.backend {
            lines.push(format!(
                "backend: {backend}, where CI, teammates and agents share results"
            ));
        }
        for project in &self.projects {
            let mut line = format!("scope {}: {}", project.name, project.include.join(", "));
            if !project.exclude.is_empty() {
                line.push_str(&format!(", leaving out {}", project.exclude.join(", ")));
            }
            if !project.external.is_empty() {
                line.push_str(&format!(
                    ", and outside the repository {}",
                    project.external.join(", ")
                ));
            }
            lines.push(line);
        }
        if !self.projects.is_empty() {
            lines.push(CANT_SEE.to_string());
        }
        for level in &self.macos {
            lines.push(match level {
                RuleLevel::Top => {
                    format!("counts_from: {{os: macos}}: every check hushrun runs here is {MACOS_ONLY}")
                }
                RuleLevel::Scope(scope) => {
                    format!(
                        "counts_from: {{os: macos}} in scope {scope}: every check hushrun runs there is {MACOS_ONLY}"
                    )
                }
            });
        }
        lines
    }

    /// The `scopes:` block.
    fn scopes(&self) -> String {
        let mut text = String::from(
            "# A check skips until its scope's files change, so each project's checks run for its\n# own changes, and what CI and its tools read.\nscopes:\n",
        );
        for project in &self.projects {
            let _ = writeln!(text, "  {}:\n    include:", project.name);
            for pattern in &project.include {
                let _ = writeln!(text, "      - \"{pattern}\"");
            }
            if !project.exclude.is_empty() {
                let _ = writeln!(text, "    exclude:");
                for pattern in &project.exclude {
                    let _ = writeln!(text, "      - \"{pattern}\"");
                }
            }
            if !project.external.is_empty() {
                let _ = writeln!(text, "    external:");
                for path in &project.external {
                    let _ = writeln!(text, "      - path: {path}");
                }
            }
            if self.macos.contains(&RuleLevel::Scope(project.name.clone())) {
                let _ = writeln!(text, "    counts_from: {{os: macos}}");
            }
        }
        text
    }

    /// `existing`, or a new file, with this added, every other byte kept.
    fn apply(&self, existing: Option<&str>, repository: &str) -> String {
        let mut text = match existing {
            Some(text) => text.to_string(),
            None => format!(
                "# hushrun.yaml: where this repository's events go.\nversion: 1\nrepository: {repository}\n"
            ),
        };
        if let Some(backend) = &self.backend {
            text = after_repository(&text, &format!("backend: {backend}"));
        }
        if !self.projects.is_empty() {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push('\n');
            text.push_str(&self.scopes());
        }
        for level in &self.macos {
            match level {
                RuleLevel::Top => {
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                    let _ = write!(
                        text,
                        "\n# Every check hushrun runs here is {MACOS_ONLY}.\ncounts_from:\n  os: macos\n"
                    );
                }
                RuleLevel::Scope(scope) if !self.projects.iter().any(|p| p.name == *scope) => {
                    text = with_scope_rule(&text, scope);
                }
                RuleLevel::Scope(_) => {}
            }
        }
        text
    }

    /// `hushrun.yaml` at `root` with this added, as [`Setup::write`] would write it.
    pub fn new_text(&self, root: &Path, repository: &str) -> String {
        let existing = std::fs::read_to_string(root.join(POLICY_FILE)).ok();
        self.apply(existing.as_deref(), repository)
    }

    /// Adds this to `hushrun.yaml` at `root`. What it writes must read back as a valid policy, or
    /// nothing is written.
    pub fn write(&self, root: &Path, repository: &str) -> Result<(), Failure> {
        let path = root.join(POLICY_FILE);
        let text = self.new_text(root, repository);
        Policy::parse(POLICY_FILE, text.as_bytes()).map_err(|e| {
            Failure::new(
                Exit::Unexpected,
                anyhow!(
                    "the hushrun.yaml init would write doesn't parse ({e}); please report this"
                ),
            )
        })?;
        super::write(&path, &text).map_err(|e| Failure::new(Exit::Unexpected, e))
    }
}

/// `text`, a hushrun.yaml, with `counts_from: {os: macos}` as the last key of its scope `scope`,
/// every other byte kept; or as it is when the scope isn't a block of lines to add it to.
fn with_scope_rule(text: &str, scope: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let content = |line: &str| !line.trim().is_empty() && !line.trim_start().starts_with('#');
    let indent = |line: &str| line.len() - line.trim_start().len();
    let bare = |line: &str| line.split(" #").next().unwrap_or(line).trim().to_string();
    let Some(scopes) = lines.iter().position(|line| line.starts_with("scopes:")) else {
        return text.to_string();
    };
    let block = |from: usize, above: usize| {
        (from..lines.len())
            .find(|at| content(lines[*at]) && indent(lines[*at]) <= above)
            .unwrap_or(lines.len())
    };
    let names_end = block(scopes + 1, 0);
    let Some(key) = (scopes + 1..names_end)
        .find(|at| content(lines[*at]) && bare(lines[*at]) == format!("{scope}:"))
    else {
        return text.to_string();
    };
    let end = block(key + 1, indent(lines[key]));
    let mut body = (key + 1..end).filter(|at| content(lines[*at]));
    let (Some(first), Some(last)) = (body.next(), (key + 1..end).rfind(|at| content(lines[*at])))
    else {
        return text.to_string();
    };
    let newline = if lines[key].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut out = lines[..=last].concat();
    if !out.ends_with('\n') {
        out.push_str(newline);
    }
    let pad = " ".repeat(indent(lines[first]));
    let _ = write!(out, "{pad}counts_from: {{os: macos}}{newline}");
    out.push_str(&lines[last + 1..].concat());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    // covers: C-15, T-9
    #[test]
    fn a_new_file_names_the_url_and_the_log_key() {
        let dir = tempfile::tempdir().unwrap();
        let log = ("ledger.example/l/acme/shop".to_string(), KEY.to_string());
        assert!(matches!(
            write_policy(
                dir.path(),
                "github.com/acme/shop",
                "https://ledger.example/l/acme/shop",
                Some(&log)
            )
            .unwrap(),
            Written::New
        ));
        let text = std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap();
        assert!(!text.contains("trust"), "{text}");
        let policy = Policy::parse(POLICY_FILE, text.as_bytes()).unwrap();
        assert_eq!(
            policy.transparency_log.as_ref().unwrap().origin,
            "ledger.example/l/acme/shop"
        );
        // Again: nothing to add.
        assert!(matches!(
            write_policy(
                dir.path(),
                "github.com/acme/shop",
                "https://ledger.example/l/acme/shop",
                Some(&log)
            )
            .unwrap(),
            Written::Kept
        ));
        assert_eq!(
            plan("github.com/acme/shop", Some(&policy)),
            ["keeps it as it is"]
        );
        assert!(!changes(Some(&policy)));
    }

    // covers: C-15
    #[test]
    fn an_existing_file_gets_what_it_lacks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(POLICY_FILE),
            "version: 1\nrepository: github.com/acme/shop\nchecks:\n  unit: {kind: source}\n",
        )
        .unwrap();
        let existing = Policy::parse(
            POLICY_FILE,
            &std::fs::read(dir.path().join(POLICY_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            plan("github.com/acme/shop", Some(&existing)),
            [
                "adds its URL on hushrun and the key that verifies hushrun's log",
                "removes `checks.unit.kind`, which hushrun 0.9 reads past: every check is a source check"
            ]
        );
        let log = ("ledger.example/l/acme/shop".to_string(), KEY.to_string());
        write_policy(
            dir.path(),
            "github.com/acme/shop",
            "https://ledger.example/l/acme/shop",
            Some(&log),
        )
        .unwrap();
        let text = std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap();
        assert!(text.contains("checks:\n  unit: {}\n"), "{text}");
        assert!(text.contains("repository_url: https://ledger.example/l/acme/shop\nchecks:"));
        assert!(text.contains("transparency_log:"));
    }

    // covers: C-15
    #[test]
    fn a_moved_repository_gets_the_new_orgs_url_and_key() {
        let text = format!(
            "# Ours.\nversion: 1\nrepository: github.com/acme/shop\nrepository_url: https://api.example/l/acme/shop\nbackend: https://api.example/o/acme\nscopes:\n  all:\n    include:\n      - \"**\"\n\ntransparency_log:   # the server's log key\n  origin: api.example/l/acme/shop\n  key: \"{KEY}\"\n"
        );
        let after = moved(&text, "https://api.example/o/initech", "acme")
            .unwrap()
            .unwrap();
        assert_eq!(
            after,
            "# Ours.\nversion: 1\nrepository: github.com/acme/shop\nbackend: https://api.example/o/initech\nscopes:\n  all:\n    include:\n      - \"**\"\n"
        );
        // What init then adds, it adds where a first init would.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(POLICY_FILE), &after).unwrap();
        let log = ("api.example/l/initech/shop".to_string(), KEY.to_string());
        write_policy(
            dir.path(),
            "github.com/acme/shop",
            "https://api.example/l/initech/shop",
            Some(&log),
        )
        .unwrap();
        let policy = Policy::parse(
            POLICY_FILE,
            &std::fs::read(dir.path().join(POLICY_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            policy.ledger.unwrap().to_string(),
            "https://api.example/l/initech/shop"
        );
        assert_eq!(policy.transparency_log.unwrap().origin, log.0);
        assert_eq!(
            policy.backend.unwrap().to_string(),
            "https://api.example/o/initech"
        );
    }

    // covers: C-15
    #[test]
    fn a_move_reads_the_file_as_yaml_would() {
        // A blank line inside the log's block is still the block, and line endings stay.
        let text = format!(
            "version: 1\r\nrepository: github.com/acme/shop\r\nrepository_url: https://api.example/l/acme/shop\r\nbackend: https://api.example/o/acme # ours\r\ntransparency_log:\n  origin: api.example/l/acme/shop\n\n# rotated in 2026\n  key: \"{KEY}\"\n\nchecks:\n  unit: {{kind: source}}\n"
        );
        assert_eq!(
            moved(&text, "https://api.example/o/initech", "acme")
                .unwrap()
                .as_deref(),
            Some(
                "version: 1\r\nrepository: github.com/acme/shop\r\nbackend: https://api.example/o/initech\r\n\nchecks:\n  unit: {kind: source}\n"
            )
        );
    }

    // covers: C-15
    #[test]
    fn a_move_keeps_what_isnt_the_old_orgs() {
        // A URL someone set themselves, and its log key.
        let text = format!(
            "version: 1\nrepository: github.com/acme/shop\nrepository_url: https://mine.example/l/other/shop\n\ntransparency_log:\n  origin: mine.example/l/other/shop\n  key: \"{KEY}\"\n"
        );
        assert_eq!(
            moved(&text, "https://api.example/o/initech", "acme").unwrap(),
            None
        );
        // The file names no backend: package.json does.
        let text = "version: 1\nrepository: github.com/acme/shop\nrepository_url: https://api.example/l/acme/shop\n";
        assert_eq!(
            moved(text, "https://api.example/o/initech", "acme")
                .unwrap()
                .as_deref(),
            Some("version: 1\nrepository: github.com/acme/shop\n")
        );
    }

    #[test]
    fn gitlab_pipelines_that_run_a_wrapped_check() {
        let dir = tempfile::tempdir().unwrap();
        let wrapped = vec!["test".to_string()];
        assert_eq!(gitlab_pipeline(dir.path(), &wrapped), None);
        std::fs::write(
            dir.path().join(GITLAB_CI),
            "test:\n  script:\n    - npm ci\n    # - npm run lint\n    - npm test\n",
        )
        .unwrap();
        assert_eq!(
            gitlab_pipeline(dir.path(), &wrapped).as_deref(),
            Some(".gitlab-ci.yml")
        );
        assert_eq!(gitlab_pipeline(dir.path(), &["lint".to_string()]), None);
    }

    // covers: C-15
    #[test]
    fn a_repository_of_projects_gets_its_backend_and_scopes() {
        let setup = Setup {
            backend: Some("https://hushrun.example/o/acme".into()),
            projects: vec![
                Project {
                    name: "back".into(),
                    dir: "back".into(),
                    include: vec!["back/**".into(), "*".into(), ".github/workflows/**".into()],
                    exclude: vec!["*.md".into(), "LICENSE*".into(), "NOTICE*".into()],
                    external: vec!["../cli".into()],
                },
                Project {
                    name: "ui".into(),
                    dir: "ui".into(),
                    include: vec!["ui/**".into()],
                    exclude: Vec::new(),
                    external: Vec::new(),
                },
            ],
            ..Setup::default()
        };
        let dir = tempfile::tempdir().unwrap();
        setup.write(dir.path(), "github.com/acme/shop").unwrap();
        let text = std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap();
        let policy = Policy::parse(POLICY_FILE, text.as_bytes()).unwrap();
        assert_eq!(policy.scopes.len(), 2);
        let back = policy.scopes.get("back").unwrap();
        assert_eq!(back.external[0].path, "../cli");
        // What init writes means what it says: the root's files, but not its prose, and a
        // project's own docs count.
        for (path, member) in [
            ("tsconfig.base.json", true),
            (".eslintrc.json", true),
            ("rust-toolchain.toml", true),
            ("README.md", false),
            ("CHANGELOG.md", false),
            ("LICENSE.md", false),
            ("NOTICE", false),
            ("back/README.md", true),
            ("back/LICENSE", true),
            ("docs/guide.txt", false),
            ("ui/app.tsx", false),
        ] {
            assert_eq!(policy.scopes.contains(back, path), member, "{path}");
        }
        assert_eq!(
            policy.backend.unwrap().to_string(),
            "https://hushrun.example/o/acme"
        );
        // Then the URL and the log key go in with everything kept.
        let log = ("ledger.example/l/acme/shop".to_string(), KEY.to_string());
        write_policy(
            dir.path(),
            "github.com/acme/shop",
            "https://ledger.example/l/acme/shop",
            Some(&log),
        )
        .unwrap();
        let after = std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap();
        assert!(
            after.contains("backend: https://hushrun.example/o/acme\n"),
            "{after}"
        );
        assert!(
            after.contains("repository_url: https://ledger.example/l/acme/shop\nbackend:"),
            "{after}"
        );
        assert_eq!(
            Policy::parse(POLICY_FILE, after.as_bytes())
                .unwrap()
                .scopes
                .len(),
            2
        );
    }

    // covers: C-15
    #[test]
    fn an_existing_file_keeps_what_it_has_with_a_backend_and_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POLICY_FILE);
        std::fs::write(
            &path,
            "# Ours.\nversion: 1\nrepository: github.com/acme/shop",
        )
        .unwrap();
        let setup = Setup {
            backend: Some("https://hushrun.example/o/acme".into()),
            ..Setup::default()
        };
        setup.write(dir.path(), "github.com/acme/shop").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Ours.\nversion: 1\nrepository: github.com/acme/shop\nbackend: https://hushrun.example/o/acme\n"
        );
    }

    fn rule_of(text: &str, scope: Option<&str>) -> Option<Level> {
        let rules = Policy::parse(POLICY_FILE, text.as_bytes())
            .unwrap()
            .counts_from;
        match scope {
            None => rules.top,
            Some(scope) => rules.scopes.get(&ScopeName::new(scope).unwrap()).cloned(),
        }
    }

    fn macos() -> Option<Level> {
        Some(Level::Rule(Rule {
            os: vec!["macos".into()],
            ..Rule::default()
        }))
    }

    const FILE: &str = "# Ours.\nversion: 1\nrepository: github.com/acme/shop\n";

    // covers: C-15, C-25
    #[test]
    fn a_rule_at_the_top_keeps_every_other_byte() {
        let setup = Setup::default()
            .with_macos(RuleLevel::Top, Some(FILE), "github.com/acme/shop")
            .unwrap();
        let text = setup.apply(Some(FILE), "github.com/acme/shop");
        assert_eq!(
            text,
            format!(
                "{FILE}\n# Every check hushrun runs here is on macOS in CI, with Xcode or Swift, so a pass on another system won't count.\ncounts_from:\n  os: macos\n"
            )
        );
        assert_eq!(rule_of(&text, None), macos());
        // Lacking a final newline changes nothing of that.
        let open = FILE.trim_end();
        let setup = Setup::default()
            .with_macos(RuleLevel::Top, Some(open), "github.com/acme/shop")
            .unwrap();
        assert!(setup.apply(Some(open), "x").starts_with(FILE));
    }

    // covers: C-15, C-25
    #[test]
    fn a_new_file_holds_a_rule_with_only_what_it_needs() {
        let setup = Setup::default()
            .with_macos(RuleLevel::Top, None, "github.com/acme/shop")
            .unwrap();
        let text = setup.apply(None, "github.com/acme/shop");
        assert!(
            text.starts_with(
                "# hushrun.yaml: where this repository's events go.\nversion: 1\nrepository: github.com/acme/shop\n\n# Every"
            ),
            "{text}"
        );
        assert_eq!(rule_of(&text, None), macos());
        // Then the URL and the log key go in with everything kept.
        let dir = tempfile::tempdir().unwrap();
        setup.write(dir.path(), "github.com/acme/shop").unwrap();
        let log = ("ledger.example/l/acme/shop".to_string(), KEY.to_string());
        write_policy(
            dir.path(),
            "github.com/acme/shop",
            "https://ledger.example/l/acme/shop",
            Some(&log),
        )
        .unwrap();
        let after = std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap();
        assert_eq!(rule_of(&after, None), macos());
        assert!(
            after.contains("counts_from:\n  os: macos\n\ntransparency_log:"),
            "{after}"
        );
    }

    // covers: C-15, C-25
    #[test]
    fn a_scope_init_adds_holds_its_rule() {
        let project = |name: &str| Project {
            name: name.into(),
            dir: name.into(),
            include: vec![format!("{name}/**")],
            exclude: vec!["*.md".into()],
            external: Vec::new(),
        };
        let setup = Setup {
            projects: vec![project("ios"), project("api")],
            ..Setup::default()
        }
        .with_macos(RuleLevel::Scope("ios".into()), None, "github.com/acme/shop")
        .unwrap();
        let text = setup.apply(None, "github.com/acme/shop");
        assert_eq!(rule_of(&text, Some("ios")), macos());
        assert_eq!(rule_of(&text, Some("api")), None);
        assert_eq!(rule_of(&text, None), None);
        assert_eq!(
            setup.lines().last().unwrap(),
            "counts_from: {os: macos} in scope ios: every check hushrun runs there is on macOS in CI, with Xcode or Swift, so a pass on another system won't count"
        );
    }

    // covers: C-15, C-25
    #[test]
    fn a_scope_the_file_declares_gets_its_rule_as_a_line_of_its_own() {
        let add = |text: &str, scope: &str| {
            let level = RuleLevel::Scope(scope.into());
            Setup::default()
                .with_macos(level, Some(text), "github.com/acme/shop")
                .map(|setup| setup.apply(Some(text), "github.com/acme/shop"))
        };
        let text = format!(
            "{FILE}scopes:   # ours\n  ios: # the app\n    # all of it\n    include:\n    - \"ios/**\"\n\n    # not the docs\n    exclude: [\"ios/docs/**\"]\n\n  api:\n    include: [\"api/**\"]\ntransparency_log:\n  origin: a.example/l/acme/shop\n  key: \"{KEY}\"\n"
        );
        assert_eq!(
            add(&text, "ios").unwrap(),
            text.replace(
                "    exclude: [\"ios/docs/**\"]\n",
                "    exclude: [\"ios/docs/**\"]\n    counts_from: {os: macos}\n"
            )
        );
        assert_eq!(
            add(&text, "api").unwrap(),
            text.replace(
                "    include: [\"api/**\"]\n",
                "    include: [\"api/**\"]\n    counts_from: {os: macos}\n"
            )
        );
        // The last line of a file with no final newline, and Windows' line endings.
        let open = format!("{FILE}scopes:\n  ios:\n    include: [\"**\"]");
        assert_eq!(rule_of(&add(&open, "ios").unwrap(), Some("ios")), macos());
        let crlf = "version: 1\r\nrepository: github.com/acme/shop\r\nscopes:\r\n  ios:\r\n    include: [\"**\"]\r\n";
        assert_eq!(
            add(crlf, "ios").unwrap(),
            format!("{crlf}    counts_from: {{os: macos}}\r\n")
        );
        // What isn't lines of keys, or isn't there, can't take one: nothing is promised.
        for text in [
            format!(
                "{FILE}scopes:\n  ios: {{include: [\"**\"]}}\n  api: {{include: [\"api/**\"]}}\n"
            ),
            format!(
                "{FILE}scopes: {{ios: {{include: [\"**\"]}}, api: {{include: [\"api/**\"]}}}}\n"
            ),
            format!(
                "{FILE}scopes:\n  api:\n    include: [\"**\"]\n  web:\n    include: [\"web/**\"]\n"
            ),
        ] {
            assert_eq!(add(&text, "ios"), None, "{text}");
        }
        // A scope has no rule of it with no block to add to: `include` has a default.
        let bare = format!("{FILE}scopes:\n  ios:\n  api:\n    include: [\"api/**\"]\n");
        assert_eq!(add(&bare, "ios"), None);
    }

    #[test]
    fn an_existing_file_gets_the_repository_url_after_its_repository() {
        assert_eq!(
            with_ledger(
                "version: 1\nrepository: github.com/a/b\nchecks: {}\n",
                "https://l/x"
            ),
            "version: 1\nrepository: github.com/a/b\nrepository_url: https://l/x\nchecks: {}\n"
        );
    }
    #[test]
    fn a_flow_map_loses_only_its_kind() {
        for (line, after) in [
            ("  unit: {kind: source}\n", "  unit: {}\n"),
            ("  unit: {kind: source, run: x}\n", "  unit: {run: x}\n"),
            ("  unit: {run: x, kind: source}\n", "  unit: {run: x}\n"),
            (
                "  unit: {run: x,kind: source, timeout: 5m}\n",
                "  unit: {run: x, timeout: 5m}\n",
            ),
        ] {
            assert_eq!(without_flow_kind(line), after, "{line}");
        }
    }

    // covers: C-15 (ADR 0062: init removes each check's `kind`, and leaves `stages` and
    // `locations`, whose gates only a person can move to `deploy`)
    #[test]
    fn an_older_file_loses_its_kinds_and_keeps_its_stages() {
        let dir = tempfile::tempdir().unwrap();
        let older = "version: 1\nrepository: github.com/acme/shop\nrepository_url: https://ledger.example/l/acme/shop\nchecks:\n  unit:\n    kind: source\n    run: go test ./...\n  lint: {kind: source, run: golangci-lint run}\nstages:\n  production:\n    branch: main\n    require: [unit]\n";
        std::fs::write(dir.path().join(POLICY_FILE), older).unwrap();
        let policy = Policy::parse(POLICY_FILE, older.as_bytes()).unwrap();
        let lines = plan("github.com/acme/shop", Some(&policy));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("removes `checks.unit.kind`,")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("leaves `stages` for you to move")),
            "{lines:?}"
        );
        assert!(changes(Some(&policy)));
        write_policy(
            dir.path(),
            "github.com/acme/shop",
            "https://ledger.example/l/acme/shop",
            None,
        )
        .unwrap();
        let after = std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap();
        assert_eq!(
            after,
            older
                .replace("    kind: source\n", "")
                .replace("{kind: source, run:", "{run:")
        );
        // What's left reads, with only what a person must move still named.
        let keys: Vec<String> = Policy::parse(POLICY_FILE, after.as_bytes())
            .unwrap()
            .retired
            .into_iter()
            .map(|r| r.key)
            .collect();
        assert_eq!(keys, ["stages"]);
    }
}
