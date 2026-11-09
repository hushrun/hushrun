//! What CI repeats (C-16): how often, in the last 30 days, a repository's workflows ran again on
//! code that had already passed them, and how long those runs took. Init shows it while the
//! sign-in waits. It is a best effort: it never fails init, and says nothing when it can't
//! tell.
//!
//! It asks GitHub through `gh`, the GitHub CLI, signed in as the person running init: no token
//! goes through hushrun.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use hushrun_core::time::Time;

use super::manager::program;

/// How long the estimate may take, from its start.
pub const BUDGET: Duration = Duration::from_secs(10);
/// The most runs it reads, the newest first.
pub const MAX_RUNS: usize = 1000;
/// How many days back it looks.
const DAYS: i64 = 30;
const DAY_MS: i64 = 86_400_000;

/// One line per run, its fields separated by tabs.
const JQ: &str = r#".workflow_runs[] | [.workflow_id, (.head_commit.tree_id // ""), .status, (.conclusion // ""), (.run_started_at // ""), (.updated_at // "")] | @tsv"#;

/// A workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub workflow: String,
    /// The tree of the commit it ran on: the same files, whatever the commit.
    pub tree: String,
    pub completed: bool,
    pub success: bool,
    pub started: Time,
    /// When it last changed: when it finished, once completed.
    pub updated: Time,
}

impl Run {
    /// A line of what [`JQ`] prints.
    fn parse(line: &str) -> Option<Run> {
        let fields: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
        let [workflow, tree, status, conclusion, started, updated] = fields.as_slice() else {
            return None;
        };
        if workflow.is_empty() || tree.is_empty() {
            return None;
        }
        Some(Run {
            workflow: workflow.to_string(),
            tree: tree.to_string(),
            completed: *status == "completed",
            success: *status == "completed" && *conclusion == "success",
            started: Time::parse(started).ok()?,
            updated: Time::parse(updated).ok()?,
        })
    }
}

/// The runs on code that had already passed, and how long they took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repeats {
    pub runs: usize,
    pub ms: u64,
    /// The days the runs read cover: 30, or fewer when there were more than [`MAX_RUNS`].
    pub days: i64,
}

/// Of `runs`, grouped by workflow and tree, every completed run that started after a run of the
/// same workflow on the same tree had passed. `capped` says `runs` are the newest
/// [`MAX_RUNS`] of more, so that the days they cover count from the oldest of them to `now`.
pub fn estimate(runs: &[Run], capped: bool, now: Time) -> Option<Repeats> {
    let mut groups: HashMap<(&str, &str), Vec<&Run>> = HashMap::new();
    for run in runs {
        groups
            .entry((&run.workflow, &run.tree))
            .or_default()
            .push(run);
    }
    let (mut count, mut ms) = (0, 0u64);
    for group in groups.values() {
        // When a run of this workflow first passed on this tree.
        let Some(passed) = group.iter().filter(|r| r.success).map(|r| r.updated).min() else {
            continue;
        };
        for run in group.iter().filter(|r| r.completed && r.started > passed) {
            count += 1;
            ms += u64::try_from(run.updated.millis_since(run.started)).unwrap_or(0);
        }
    }
    if count == 0 {
        return None;
    }
    let days = match runs.iter().map(|r| r.started).min() {
        Some(oldest) if capped => {
            let ms = now.millis_since(oldest).max(1);
            ((ms + DAY_MS - 1) / DAY_MS).clamp(1, DAYS)
        }
        _ => DAYS,
    };
    Some(Repeats {
        runs: count,
        ms,
        days,
    })
}

/// `Last 30 days, CI ran 96 times on code it had already passed: about 11 h.`
pub fn line(repeats: &Repeats) -> String {
    let period = match repeats.days {
        1 => "Last day".to_string(),
        days => format!("Last {days} days"),
    };
    let times = match repeats.runs {
        1 => "once".to_string(),
        runs => format!("{runs} times"),
    };
    format!(
        "{period}, CI ran {times} on code it had already passed: {}.",
        about(repeats.ms)
    )
}

/// A total, rounded: `about 11 h`, `about 25 min`, `under a minute`.
fn about(ms: u64) -> String {
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    match (ms + MINUTE / 2) / MINUTE {
        0 => "under a minute".into(),
        minutes @ 1..60 => format!("about {minutes} min"),
        _ => format!("about {} h", (ms + HOUR / 2) / HOUR),
    }
}

/// An estimate running in the background.
pub struct Pending {
    receiver: mpsc::Receiver<Option<String>>,
    deadline: Instant,
}

/// Starts the estimate for `repository` when it is on github.com.
pub fn start(repository: Option<&str>) -> Option<Pending> {
    let path = repository?.strip_prefix("github.com/")?.to_string();
    if path.split('/').count() != 2 {
        return None;
    }
    let deadline = Instant::now() + BUDGET;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let now = Time::from_timestamp(jiff::Timestamp::now());
        let since = Time::from_millis(now.millis() - DAYS * DAY_MS).unwrap_or(now);
        let found = fetch(&program("gh"), &path, &since.to_string()[..10], deadline)
            .and_then(|(runs, capped)| estimate(&runs, capped, now))
            .map(|repeats| line(&repeats));
        let _ = sender.send(found);
    });
    Some(Pending { receiver, deadline })
}

impl Pending {
    /// The estimate's line, once it is done or its time is up.
    pub fn finish(self) -> Option<String> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        self.receiver
            .recv_timeout(left + Duration::from_millis(200))
            .ok()
            .flatten()
    }
}

/// The runs of `owner/name` created on or after `since` (`YYYY-MM-DD`), from `gh`, the newest
/// [`MAX_RUNS`] at most, and whether there were more. Nothing when `gh` isn't installed or
/// signed in, fails, or takes past `deadline`.
fn fetch(gh: &Path, path: &str, since: &str, deadline: Instant) -> Option<(Vec<Run>, bool)> {
    let mut child = Command::new(gh)
        .args(["api", "--hostname", "github.com", "--paginate", "-X", "GET"])
        .arg(format!("repos/{path}/actions/runs"))
        .args(["-f", "per_page=100", "-f"])
        .arg(format!("created=>={since}"))
        .args(["--jq", JQ])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut runs = Vec::new();
        let mut capped = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            runs.extend(Run::parse(&line));
            if runs.len() >= MAX_RUNS {
                capped = true;
                break;
            }
        }
        let _ = sender.send((runs, capped));
    });
    let stop = |mut child: std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let Ok((runs, capped)) =
        receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    else {
        stop(child);
        return None;
    };
    if capped {
        stop(child);
        return Some((runs, true));
    }
    // A listing is whole only when gh says it succeeded.
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Some((runs, false)),
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                stop(child);
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes: i64) -> Time {
        Time::from_millis(1_790_000_000_000 + minutes * 60_000).unwrap()
    }

    fn run(workflow: &str, tree: &str, conclusion: &str, from: i64, to: i64) -> Run {
        Run {
            workflow: workflow.into(),
            tree: tree.into(),
            completed: conclusion != "running",
            success: conclusion == "success",
            started: at(from),
            updated: at(to),
        }
    }

    // covers: C-16
    #[test]
    fn runs_after_a_pass_of_the_same_workflow_on_the_same_tree_count() {
        let runs = [
            // Workflow 1 on tree a: passes, then runs twice more, once failing.
            run("1", "a", "failure", 0, 8),
            run("1", "a", "success", 10, 20),
            run("1", "a", "success", 30, 42),
            run("1", "a", "failure", 50, 55),
            // Started before the pass ended: it couldn't have skipped.
            run("1", "a", "success", 15, 25),
            // Still going: no duration yet.
            run("1", "a", "running", 60, 61),
            // Another tree, another workflow: their first passes.
            run("1", "b", "success", 30, 40),
            run("2", "a", "success", 30, 40),
            run("2", "b", "cancelled", 30, 31),
        ];
        let repeats = estimate(&runs, false, at(100)).unwrap();
        assert_eq!(
            repeats,
            Repeats {
                runs: 2,
                ms: 17 * 60_000,
                days: 30
            }
        );
        assert_eq!(
            line(&repeats),
            "Last 30 days, CI ran 2 times on code it had already passed: about 17 min."
        );
        // Nothing repeated, nothing to say.
        assert_eq!(
            estimate(&runs[..3], false, at(100)).map(|r| r.runs),
            Some(1)
        );
        assert_eq!(estimate(&runs[6..], false, at(100)), None);
        assert_eq!(estimate(&[], false, at(100)), None);
    }

    // covers: C-16
    #[test]
    fn the_newest_thousand_runs_cover_fewer_days() {
        let runs = [
            run("1", "a", "success", 0, 10),
            run("1", "a", "success", 20, 30),
        ];
        // Read whole, they cover 30 days; capped, from the oldest to now: here 2 days.
        assert_eq!(estimate(&runs, false, at(3 * 24 * 60)).unwrap().days, 30);
        assert_eq!(estimate(&runs, true, at(2 * 24 * 60 - 1)).unwrap().days, 2);
        assert_eq!(estimate(&runs, true, at(60)).unwrap().days, 1);
        assert_eq!(estimate(&runs, true, at(90 * 24 * 60)).unwrap().days, 30);
        let one = estimate(&runs, true, at(60)).unwrap();
        assert_eq!(
            line(&one),
            "Last day, CI ran once on code it had already passed: about 10 min."
        );
    }

    #[test]
    fn totals_read_like_people_say_them() {
        assert_eq!(about(20_000), "under a minute");
        assert_eq!(about(40_000), "about 1 min");
        assert_eq!(about(25 * 60_000), "about 25 min");
        assert_eq!(about(59 * 60_000 + 40_000), "about 1 h");
        assert_eq!(about(11 * 3_600_000 + 20 * 60_000), "about 11 h");
    }

    #[test]
    fn lines_of_runs_are_read() {
        let line = "1234\tabc\tcompleted\tsuccess\t2026-10-01T10:00:00Z\t2026-10-01T10:10:00Z";
        let parsed = Run::parse(line).unwrap();
        assert!(parsed.completed && parsed.success);
        assert_eq!(parsed.updated.millis_since(parsed.started), 600_000);
        let running =
            Run::parse("1\tabc\tin_progress\t\t2026-10-01T10:00:00Z\t2026-10-01T10:01:00Z")
                .unwrap();
        assert!(!running.completed && !running.success);
        for bad in [
            "",
            "1\t\tcompleted\tsuccess\t2026-10-01T10:00:00Z\t2026-10-01T10:10:00Z",
            "1\tabc\tcompleted\tsuccess\tyesterday\t2026-10-01T10:10:00Z",
            "1\tabc\tcompleted",
        ] {
            assert_eq!(Run::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_github_repositories_are_estimated() {
        assert!(start(None).is_none());
        assert!(start(Some("gitlab.com/acme/shop")).is_none());
        assert!(start(Some("github.com/acme")).is_none());
        assert!(start(Some("github.com/acme/shop/extra")).is_none());
    }

    /// A fake gh in its own directory that runs `body`.
    #[cfg(unix)]
    fn fake_gh(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    // covers: C-16
    #[cfg(unix)]
    #[test]
    fn gh_is_asked_within_the_budget() {
        let soon = || Instant::now() + Duration::from_secs(5);
        let (dir, gh) = fake_gh(
            "printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\nprintf '1\\ta\\tcompleted\\tsuccess\\t2026-10-01T10:00:00Z\\t2026-10-01T10:10:00Z\\n'",
        );
        let (runs, capped) = fetch(&gh, "acme/shop", "2026-09-19", soon()).unwrap();
        assert_eq!((runs.len(), capped), (1, false));
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(
            args,
            [
                "api",
                "--hostname",
                "github.com",
                "--paginate",
                "-X",
                "GET",
                "repos/acme/shop/actions/runs",
                "-f",
                "per_page=100",
                "-f",
                "created=>=2026-09-19",
                "--jq",
                JQ,
            ]
        );
        // Failing, it says nothing; past the budget, it stops waiting.
        let (_dir, failing) = fake_gh("echo 'gh: Not Found (HTTP 404)' >&2; exit 1");
        assert_eq!(fetch(&failing, "acme/shop", "2026-09-19", soon()), None);
        let (_dir, slow) = fake_gh("exec sleep 5");
        let started = Instant::now();
        let budget = Instant::now() + Duration::from_millis(300);
        assert_eq!(fetch(&slow, "acme/shop", "2026-09-19", budget), None);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        // More than the cap: the newest are read, and gh is stopped.
        let (_dir, many) = fake_gh(
            "i=0; while [ $i -lt 1200 ]; do printf '1\\ta\\tcompleted\\tsuccess\\t2026-10-01T10:00:00Z\\t2026-10-01T10:10:00Z\\n'; i=$((i+1)); done",
        );
        let (runs, capped) = fetch(&many, "acme/shop", "2026-09-19", soon()).unwrap();
        assert_eq!((runs.len(), capped), (MAX_RUNS, true));
        assert_eq!(
            fetch(
                Path::new("/nonexistent/gh"),
                "acme/shop",
                "2026-09-19",
                soon()
            ),
            None
        );
    }
}
