//! The repository's ledger (C-15): init asks the org's backend for it, so that promoting the
//! repository's builds later starts from one. An org admin's init makes it, and anyone's finds
//! it. It never fails init.
//!
//! A ledger takes CI's ID tokens by GitHub's id for its repository, which a backend can't read
//! for a private repository by itself. So, for a repository on github.com, init asks `gh`, the
//! GitHub CLI, for the id while the sign-in waits, through `gh`'s own sign-in: no token goes
//! through hushrun. Without `gh`, the backend finds the id later (C-12).

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::hosted::{self, LedgerAnswer};

use super::manager::program;

/// How long `gh` may take to name the id, from its start.
const BUDGET: Duration = Duration::from_secs(10);

/// What `gh` says about the repository: its id, which pins its CI at the server (results-v0 §2),
/// and its default branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRepo {
    pub id: i64,
    pub default_branch: Option<String>,
}

/// GitHub's id for the repository, being read in the background.
pub struct GithubId {
    receiver: mpsc::Receiver<Option<GithubRepo>>,
    deadline: Instant,
}

impl GithubId {
    /// Starts reading GitHub's id for `repository` when it is on github.com.
    pub fn start(repository: Option<&str>) -> Option<GithubId> {
        let path = repository?.strip_prefix("github.com/")?.to_string();
        if path.split('/').count() != 2 {
            return None;
        }
        let deadline = Instant::now() + BUDGET;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(read(&program("gh"), &path, deadline));
        });
        Some(GithubId { receiver, deadline })
    }

    /// The repository, once `gh` named its id; nothing when it couldn't, or its time is up.
    pub fn finish(self) -> Option<GithubRepo> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        self.receiver
            .recv_timeout(left + Duration::from_millis(200))
            .ok()
            .flatten()
    }
}

/// The fields init reads from `gh api repos/<owner>/<name>`, one line, separated by tabs.
pub const FIELDS: &str = "[.id, .private, .default_branch, .permissions.admin] | @tsv";

/// `gh api repos/<owner>/<name> --jq <FIELDS>`: nothing when `gh` isn't installed or signed in,
/// can't see the repository, prints anything but an id first, or runs past `deadline`.
fn read(gh: &Path, path: &str, deadline: Instant) -> Option<GithubRepo> {
    let mut command = Command::new(gh);
    command
        .args(["api", "--hostname", "github.com"])
        .arg(format!("repos/{path}"))
        .args(["--jq", FIELDS])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    parse_id(&output(command, deadline)?)
}

/// What `command` prints on stdout, when it succeeds before `deadline`.
fn output(mut command: Command, deadline: Instant) -> Option<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).ok().map(|_| text)
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let text = reader.join().ok().flatten()?;
    status.success().then_some(text)
}

/// What `--jq <FIELDS>` prints: the id, then whether it's private (unread), its default branch
/// and whether the viewer administers it (unread), each optional.
fn parse_id(text: &str) -> Option<GithubRepo> {
    let line = text.trim_end_matches(['\n', '\r']);
    let mut fields = line.split('\t');
    let id = fields
        .next()?
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)?;
    let _private = fields.next();
    let default_branch = fields
        .next()
        .map(str::trim)
        .filter(|b| !b.is_empty() && *b != "null")
        .map(str::to_string);
    Some(GithubRepo { id, default_branch })
}

/// What init's plan says of the repository's ledger, from a dry run (ADR 0026 item 19).
#[derive(Debug, Clone)]
pub enum Preview {
    /// The org has it; or a backend from before `dry_run` just found or made it.
    Found(hosted::Ledger),
    /// Applying the plan makes it, under this name as things stand.
    WouldMake(hosted::Ledger),
    /// Applying would be refused, in the backend's words.
    Refused(String),
    /// Nothing known: the backend makes no ledgers, or didn't answer.
    Unknown,
}

impl Preview {
    /// The plan's line for it, under the org.
    pub fn line(&self, repository: &str, org: &str) -> String {
        match self {
            Preview::Found(ledger) => format!(
                "finds {repository} in {org}, as {}: {}",
                ledger.name,
                ledger.dashboard.as_deref().unwrap_or(&ledger.url)
            ),
            Preview::WouldMake(ledger) => format!(
                "adds {repository} to {org}'s dashboard as {}: {}",
                ledger.name, ledger.url
            ),
            Preview::Refused(why) => format!("can't add {repository} to {org}: {why}"),
            Preview::Unknown => {
                format!("adds {repository} to {org}'s dashboard, unless it is there already")
            }
        }
    }
}

/// Asks `backend` what `repository`'s ledger would be, without making it: `dry_run`.
pub fn preview(backend: &str, ask: hosted::LedgerAsk<'_>) -> Preview {
    let ask = hosted::LedgerAsk {
        dry_run: true,
        ..ask
    };
    match hosted::repository_ledger(backend, ask) {
        Ok(LedgerAnswer::Ledger(ledger)) => match ledger.exists {
            Some(false) => Preview::WouldMake(ledger),
            _ => Preview::Found(ledger),
        },
        Ok(LedgerAnswer::Refused {
            status: 403 | 409,
            why,
        }) => Preview::Refused(why),
        Ok(LedgerAnswer::Refused { .. } | LedgerAnswer::Unsupported) => Preview::Unknown,
        Err(err) => {
            tracing::debug!("asking {backend} for the ledger: {err:#}");
            Preview::Unknown
        }
    }
}

/// Asks `backend` for `repository`'s ledger, with GitHub's id for the repository when `gh` named
/// it and its default branch, and says what came of it: the ledger and its page, or a warning.
/// Returns the ledger.
pub fn ensure(backend: &str, ask: hosted::LedgerAsk<'_>) -> Option<hosted::Ledger> {
    let repository = ask.repository;
    match hosted::repository_ledger(
        backend,
        hosted::LedgerAsk {
            dry_run: false,
            ..ask
        },
    ) {
        Ok(LedgerAnswer::Ledger(ledger)) => {
            let page = ledger.dashboard.as_deref().unwrap_or(&ledger.url);
            if ledger.created {
                println!("Added {repository} to hushrun as {}: {page}", ledger.name);
            } else {
                println!("{repository} is on hushrun as {}: {page}", ledger.name);
            }
            return Some(ledger);
        }
        Ok(LedgerAnswer::Unsupported) => eprintln!(
            "warning: {backend} doesn't add repositories from here: promoting {repository}'s builds needs it added in {backend}'s own settings"
        ),
        // A refusal for the request's sake names the repository itself: an org's member who isn't
        // an admin, or an id that isn't the one the org knows.
        Ok(LedgerAnswer::Refused {
            status: 403 | 409,
            why,
        }) => eprintln!("warning: {why}"),
        Ok(LedgerAnswer::Refused { why, .. }) => {
            eprintln!("warning: {repository} wasn't added: {why}");
        }
        Err(err) => eprintln!("warning: {repository} wasn't added: {err:#}"),
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_positive_numbers() {
        assert_eq!(parse_id("4242\n").map(|r| r.id), Some(4242));
        assert_eq!(parse_id(" 1 ").map(|r| r.id), Some(1));
        for bad in ["", "0", "-5", "null", "4242 4243", "{\"id\": 4242}"] {
            assert_eq!(parse_id(bad), None, "{bad:?}");
        }
        assert_eq!(
            parse_id("4242\ttrue\tmain\tfalse\n"),
            Some(GithubRepo {
                id: 4242,
                default_branch: Some("main".into()),
            })
        );
        let partial = parse_id("4242\t\t\t\n").unwrap();
        assert_eq!(partial.default_branch, None);
    }

    #[test]
    fn only_github_repositories_ask_gh() {
        assert!(GithubId::start(None).is_none());
        assert!(GithubId::start(Some("gitlab.com/acme/shop")).is_none());
        assert!(GithubId::start(Some("github.com/acme")).is_none());
        assert!(GithubId::start(Some("github.com/acme/shop/extra")).is_none());
    }
}
