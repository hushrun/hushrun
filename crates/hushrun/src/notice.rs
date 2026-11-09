//! The newer-release notice (C-21, ADR 0035). At most once a day, at a terminal, hushrun says in
//! one line on stderr that a newer release is out: ``hushrun 0.2.0 is available: run `hushrun
//! update` ``.
//!
//! It learns of releases from GitHub, in the background while a command runs, and waits at most
//! a moment after the command for the answer: whatever happens, the command's output and exit
//! code are what they would have been. It asks once a day; a question that got no answer in
//! time, such as during a quick command, is asked again an hour later at the earliest.
//!
//! It stays quiet in CI, without a terminal on stderr, with `--format` other than text, with
//! `-q`, and with `HUSHRUN_NO_UPDATE_CHECK` set. Without a cache directory to remember its
//! questions in (`HUSHRUN_CACHE_DIR` set to nothing), it doesn't ask at all.
//!
//! \[ADR 0065\] A coding agent's shell has no terminal, and the agent reads stderr: when the
//! environment says an agent runs hushrun, the notice needs none, and says it in words for the
//! agent, who tells the person. `hushrun guide` reads what the notice learned.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::cli::{Cli, Command};
use crate::output::Format;
use crate::releases::{self, Releases};

/// How often hushrun learns of releases, and says what it learned.
const DAY: i64 = 24 * 60 * 60;
/// How soon a question that got no answer is asked again.
const HOUR: i64 = 60 * 60;
/// How long the question may take, in the background.
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a command waits for the answer once the command is done.
const GRACE: Duration = Duration::from_millis(200);
/// What hushrun remembers, in its cache directory.
const FILE: &str = "update-check.json";
const FORMAT: u32 = 1;
/// What turns the check off.
pub const OFF_VARIABLE: &str = "HUSHRUN_NO_UPDATE_CHECK";
/// The notice of one command: nothing to do, or what hushrun remembers and the question under
/// way.
pub struct Notice(Option<Pending>);

struct Pending {
    path: PathBuf,
    current: Version,
    /// \[ADR 0065\] Whether a coding agent runs this command, and reads the notice.
    agent: bool,
    /// The answer, when this command asked: the newest release, or nothing.
    answer: Option<mpsc::Receiver<Option<Version>>>,
}

/// What hushrun remembers between commands, in seconds since 1970.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    format: u32,
    /// When hushrun last asked GitHub for the newest release.
    asked: i64,
    /// When GitHub last answered.
    answered: i64,
    /// The newest release it named then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    /// When hushrun last said that a newer release is out.
    noticed: i64,
}

impl Notice {
    /// Asks GitHub in the background, when this command may show the notice and it is time to
    /// ask.
    pub fn start(cli: &Cli) -> Notice {
        let env = hushrun_ci::Env::from_process();
        if !wanted(cli, &env, std::io::stderr().is_terminal()) {
            return Notice(None);
        }
        let Some(dir) = crate::context::cache_dir() else {
            return Notice(None);
        };
        let agent = agent(&env);
        Notice(
            start_in(dir.join(FILE), releases::current(), now(), check)
                .map(|pending| Pending { agent, ..pending }),
        )
    }

    /// Once the answer came, or a moment passed, prints the notice when a newer release is out
    /// and a day passed since hushrun last said so.
    pub fn finish(self) {
        if let Some(pending) = self.0 {
            let (agent, current) = (pending.agent, pending.current.clone());
            if let Some(latest) = finish_at(pending, now(), GRACE) {
                let npm = crate::cmd::update::came_with_npm();
                match agent {
                    true => eprintln!("{}", agent_line(&latest, &current, npm)),
                    false => eprintln!("{}", line(&latest, npm)),
                }
            }
        }
    }
}

/// \[ADR 0065\] The newest release the notice learned of, when it is newer than this hushrun:
/// what `hushrun guide` reports, from the record alone. It asks nobody, and says nothing with
/// `HUSHRUN_NO_UPDATE_CHECK` set.
pub fn known_newer() -> Option<Version> {
    let env = hushrun_ci::Env::from_process();
    if turned_off(env.get(OFF_VARIABLE)) {
        return None;
    }
    let path = crate::context::cache_dir()?.join(FILE);
    newer_in(&load(&path), &releases::current())
}

/// The release `state` names, when it is newer than `current`.
fn newer_in(state: &State, current: &Version) -> Option<Version> {
    state
        .latest
        .as_deref()
        .and_then(|text| Version::parse(text).ok())
        .filter(|latest| latest.cmp_precedence(current).is_gt())
}

/// How this hushrun is updated: what `hushrun update` does, or the package manager.
pub fn update_command(npm: bool) -> &'static str {
    match npm {
        true => "npm install hushrun@latest",
        false => "hushrun update",
    }
}

/// \[ADR 0065\] The notice for a coding agent: it isn't the one to update hushrun, since the
/// package is the repository's and the binary the person's, and an update in the middle of a
/// task changes the tool under it. It tells the person, and says how.
pub fn agent_line(latest: &Version, current: &Version, npm: bool) -> String {
    let how = match npm {
        true => {
            "updates the hushrun package with the package manager that installed it, such as npm install hushrun@latest"
        }
        false => "updates it with `hushrun update`",
    };
    format!(
        "hushrun: hushrun {latest} is available, and this is {current}: when you report back, tell the person, who {how}"
    )
}

/// Whether the environment says a coding agent runs this (C-23).
fn agent(env: &hushrun_ci::Env) -> bool {
    crate::coding_agent::agent(&|name| env.get(name).map(str::to_string)).is_some()
}

/// The notice, for a hushrun that came with a JavaScript package manager or not. \[ADR 0061\]
/// `hushrun update` leaves npm's alone, so the notice says what update would.
fn line(latest: &Version, npm: bool) -> String {
    match npm {
        true => format!(
            "hushrun {latest} is available: update the hushrun package with the package manager that installed it, such as npm install hushrun@latest"
        ),
        false => format!("hushrun {latest} is available: run `hushrun update`"),
    }
}

/// Whether this command may show the notice: at a terminal or to a coding agent, outside CI, as
/// text (`--format text` without `-q`), unless `HUSHRUN_NO_UPDATE_CHECK` turns it off. `hushrun
/// update` says what it finds itself. \[ADR 0061\] CI is what [`hushrun_ci::in_ci`] says, as
/// everywhere else in hushrun; and a sandbox that says `CI` with no agent in it, such as one with
/// `HUSHRUN_PROVIDER=local`, which detection calls local, has no person to tell either.
/// \[ADR 0065\] An agent, as the hook tells one (C-23), reads the notice without a terminal, and
/// in a sandbox that says `CI`.
fn wanted(cli: &Cli, env: &hushrun_ci::Env, terminal: bool) -> bool {
    let agent = agent(env);
    (terminal || agent)
        && !matches!(cli.command, Command::Update)
        && cli.global.quiet == 0
        && cli.global.format == Format::Text
        && !turned_off(env.get(OFF_VARIABLE))
        && !hushrun_ci::in_ci(env)
        && (agent || !env.says_yes("CI"))
}

/// `HUSHRUN_NO_UPDATE_CHECK`'s value turns the check off unless it is unset, empty, `false`,
/// `0`, `no` or `off`: a value hushrun can't read can only have been meant to turn it off.
fn turned_off(value: Option<&str>) -> bool {
    let value = value.map(|v| v.trim().to_ascii_lowercase());
    !matches!(
        value.as_deref(),
        None | Some("" | "false" | "0" | "no" | "off")
    )
}

/// Seconds since 1970.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Whether `span` passed between `then` and `now`, or `then` is after `now`, as a clock set
/// back makes it.
fn passed(span: i64, then: i64, now: i64) -> bool {
    now < then || now.saturating_sub(then) >= span
}

/// Reads the state at `path`; an unreadable file, or one of another format, is a fresh state.
fn load(path: &Path) -> State {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<State>(&bytes).ok())
        .filter(|state| state.format == FORMAT)
        .unwrap_or_default()
}

/// Writes `state` to `path` whole, through a temporary file, so that another command never
/// reads half of it.
fn save(path: &Path, state: &State) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{FILE}.{}.tmp", std::process::id()));
    let state = State {
        format: FORMAT,
        ..state.clone()
    };
    let written = std::fs::write(&temp, serde_json::to_vec(&state)?)
        .and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Asks, with `ask`, when a day passed since GitHub's last answer and an hour since the last
/// question. The question is recorded before it is asked, so that commands running at the same
/// time don't ask too; and when hushrun can't record it, it doesn't ask.
fn start_in(
    path: PathBuf,
    current: Version,
    now: i64,
    ask: impl FnOnce() -> mpsc::Receiver<Option<Version>>,
) -> Option<Pending> {
    let mut state = load(&path);
    let answer = if passed(DAY, state.answered, now) && passed(HOUR, state.asked, now) {
        state.asked = now;
        save(&path, &state).ok()?;
        Some(ask())
    } else {
        None
    };
    Some(Pending {
        path,
        current,
        agent: false,
        answer,
    })
}

/// Asks GitHub for the newest release on a thread of its own, which answers on the channel,
/// with nothing when GitHub doesn't name one.
fn check() -> mpsc::Receiver<Option<Version>> {
    let (send, answer) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("hushrun-update-check".into())
        .spawn(move || {
            let latest = releases::base()
                .and_then(|base| Releases::new(&base, CHECK_TIMEOUT))
                .and_then(|releases| releases.latest(CHECK_TIMEOUT))
                .ok();
            let _ = send.send(latest);
        });
    answer
}

/// The release to tell of, if any, once the answer came or `grace` passed: when the newest
/// release hushrun knows of is newer than this one, and a day passed since it last said so.
fn finish_at(pending: Pending, now: i64, grace: Duration) -> Option<Version> {
    let answer = pending
        .answer
        .and_then(|answer| answer.recv_timeout(grace).ok().flatten());
    // Read again: another command may have learned or said something since this one started.
    let mut state = load(&pending.path);
    let mut changed = false;
    if let Some(latest) = answer {
        state.latest = Some(latest.to_string());
        state.answered = now;
        changed = true;
    }
    let newer = newer_in(&state, &pending.current);
    let said = match newer {
        Some(latest) if passed(DAY, state.noticed, now) => {
            state.noticed = now;
            changed = true;
            Some(latest)
        }
        _ => None,
    };
    if changed {
        let _ = save(&pending.path, &state);
    }
    said
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use clap::Parser as _;

    use super::*;

    fn cli(args: &[&str]) -> Cli {
        let mut cli = Cli::try_parse_from(std::iter::once("hushrun").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{args:?}: {e}"));
        crate::environment::read(&mut cli, |_| None);
        cli
    }

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// A question that GitHub answers with `latest` at once (nothing: no answer), and that
    /// counts it was asked.
    fn answering(
        latest: Option<&str>,
        asked: &Cell<u32>,
    ) -> impl FnOnce() -> mpsc::Receiver<Option<Version>> {
        let latest = latest.map(version);
        move || {
            asked.set(asked.get() + 1);
            let (send, answer) = mpsc::channel();
            send.send(latest).unwrap();
            answer
        }
    }

    // covers: C-21
    #[test]
    fn only_people_at_a_terminal_outside_ci_see_it() {
        let none = hushrun_ci::Env::default();
        let run = cli(&["digest"]);
        assert!(wanted(&run, &none, true));
        assert!(!wanted(&run, &none, false), "no terminal on stderr");
        assert!(!wanted(&cli(&["update"]), &none, true));
        assert!(!wanted(&cli(&["-q", "digest"]), &none, true));
        for format in ["json", "env", "dotenv"] {
            assert!(!wanted(&cli(&["--format", format, "digest"]), &none, true));
        }
        let with = |vars: &[(&str, &str)]| {
            wanted(
                &run,
                &hushrun_ci::Env::from_pairs(vars.iter().copied()),
                true,
            )
        };
        for vars in [
            &[("CI", "true")][..],
            &[("GITHUB_ACTIONS", "true")],
            &[("GITLAB_CI", "true")],
            &[("BUILDKITE", "true")],
            &[("CIRCLECI", "true")],
            &[
                ("JENKINS_URL", "https://jenkins.example"),
                ("BUILD_ID", "7"),
            ],
            &[("TF_BUILD", "True")],
            &[("TEAMCITY_VERSION", "2025.07")],
            &[("CODEBUILD_BUILD_ID", "shop:1")],
            // A sandbox with no agent in it: detection calls it local, and no one reads it.
            &[("CI", "true"), ("HUSHRUN_PROVIDER", "local")],
        ] {
            assert!(!with(vars), "{vars:?}");
        }
        // ADR 0061: what CI detection says, so a shell set up for Jenkins' CLI is a person's.
        assert!(with(&[("JENKINS_URL", "https://jenkins.example")]));
        assert!(with(&[("CI", "false")]) && with(&[("CI", "0")]) && with(&[("CI", "")]));
        for off in ["1", "true", "YES", "on", "please"] {
            assert!(!with(&[(OFF_VARIABLE, off)]), "{off}");
        }
        for on in ["0", "false", "No", "off", "", " "] {
            assert!(with(&[(OFF_VARIABLE, on)]), "{on:?}");
        }
    }

    // covers: C-21
    #[test]
    fn agents_see_it_without_a_terminal() {
        let with = |args: &[&str], vars: &[(&str, &str)]| {
            wanted(
                &cli(args),
                &hushrun_ci::Env::from_pairs(vars.iter().copied()),
                false,
            )
        };
        // [ADR 0065] An agent's shell has no terminal, and it reads stderr.
        for vars in [
            &[("CLAUDECODE", "1")][..],
            &[("CODEX_SANDBOX", "seatbelt")],
            &[("AI_AGENT", "amp")],
            &[("HUSHRUN_INVOKED_BY", "agent:worker-1")],
            // A sandbox that says CI, with an agent in it, isn't CI (C-17).
            &[("CI", "1"), ("HUSHRUN_AGENT", "x")],
            &[
                ("CI", "true"),
                ("HUSHRUN_PROVIDER", "local"),
                ("CLAUDECODE", "1"),
            ],
        ] {
            assert!(with(&["digest"], vars), "{vars:?}");
        }
        // Everything else keeps it quiet, as it does for people.
        let agent = [("CLAUDECODE", "1")];
        assert!(!with(&["update"], &agent));
        assert!(!with(&["-q", "digest"], &agent));
        assert!(!with(&["--format", "json", "digest"], &agent));
        assert!(!with(
            &["digest"],
            &[("CLAUDECODE", "1"), ("GITHUB_ACTIONS", "true")]
        ));
        assert!(!with(
            &["digest"],
            &[("CLAUDECODE", "1"), (OFF_VARIABLE, "1")]
        ));
        // A person says who they are, whatever the shell has.
        assert!(!with(
            &["digest"],
            &[("CLAUDECODE", "1"), ("HUSHRUN_INVOKED_BY", "human:ana")]
        ));
        // Agents get their own words: what to do, and who updates it.
        let latest = version("9.9.9");
        assert_eq!(
            agent_line(&latest, &version("0.11.0"), false),
            "hushrun: hushrun 9.9.9 is available, and this is 0.11.0: when you report back, tell the person, who updates it with `hushrun update`"
        );
        assert_eq!(
            agent_line(&latest, &version("0.11.0"), true),
            "hushrun: hushrun 9.9.9 is available, and this is 0.11.0: when you report back, tell the person, who updates the hushrun package with the package manager that installed it, such as npm install hushrun@latest"
        );
    }

    // covers: C-21
    #[test]
    fn it_asks_and_says_at_most_once_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache").join(FILE);
        let asked = Cell::new(0);
        let at = 1_800_000_000;
        let run = |now: i64, latest: Option<&str>| {
            let pending = start_in(
                path.clone(),
                version("0.1.0"),
                now,
                answering(latest, &asked),
            )
            .expect("a pending notice");
            finish_at(pending, now, Duration::from_secs(1)).map(|latest| line(&latest, false))
        };
        let said = |latest: &str| {
            Some(format!(
                "hushrun {latest} is available: run `hushrun update`"
            ))
        };
        // ADR 0061: what update says to an install of npm's, which it leaves alone.
        assert_eq!(
            line(&version("0.2.0"), true),
            "hushrun 0.2.0 is available: update the hushrun package with the package manager that installed it, such as npm install hushrun@latest"
        );

        // The first command asks, and says.
        assert_eq!(run(at, Some("0.2.0")), said("0.2.0"));
        assert_eq!(asked.get(), 1);
        let state = load(&path);
        assert_eq!(
            (
                state.asked,
                state.answered,
                state.latest.as_deref(),
                state.noticed
            ),
            (at, at, Some("0.2.0"), at)
        );
        // The rest of the day: no question, no notice.
        assert_eq!(run(at + 3600, Some("0.3.0")), None);
        assert_eq!(run(at + DAY - 1, Some("0.3.0")), None);
        assert_eq!(asked.get(), 1);
        // A day later: asked again, and said again.
        assert_eq!(run(at + DAY, Some("0.3.0")), said("0.3.0"));
        assert_eq!(asked.get(), 2);
        // A question without an answer is asked again an hour later, not before; meanwhile,
        // what hushrun knows is still said once a day.
        let later = at + 3 * DAY;
        assert_eq!(run(later, None), said("0.3.0"));
        assert_eq!(run(later + HOUR - 1, None), None);
        assert_eq!(asked.get(), 3);
        assert_eq!(run(later + HOUR, Some("0.4.0")), None, "said today");
        assert_eq!(asked.get(), 4);
        assert_eq!(load(&path).latest.as_deref(), Some("0.4.0"));
        assert_eq!(run(later + HOUR + 1, Some("0.5.0")), None);
        assert_eq!(asked.get(), 4, "answered today");
        // The newest release, or an older one, says nothing.
        let quiet = |latest: &str| {
            std::fs::remove_file(&path).unwrap();
            run(at, Some(latest))
        };
        assert_eq!(quiet("0.1.0"), None);
        assert_eq!(quiet("0.1.0-rc.1"), None);
        assert_eq!(quiet("0.0.9"), None);
        assert_eq!(load(&path).noticed, 0);
        assert_eq!(asked.get(), 7);
        // A clock set back asks again.
        assert_eq!(run(at - 10, Some("0.2.0")), said("0.2.0"));
        assert_eq!(asked.get(), 8);
        // A damaged time asks again, and never overflows.
        std::fs::write(
            &path,
            format!(
                r#"{{"format":1,"asked":{},"answered":{},"noticed":0}}"#,
                i64::MIN,
                i64::MIN
            ),
        )
        .unwrap();
        assert_eq!(run(at, Some("0.2.0")), said("0.2.0"));
        assert_eq!(asked.get(), 9);
        // Another format of the file, or a damaged one, starts over.
        std::fs::write(&path, r#"{"format":2,"asked":1,"answered":1,"noticed":1}"#).unwrap();
        assert_eq!(load(&path), State::default());
        std::fs::write(&path, "{").unwrap();
        assert_eq!(load(&path), State::default());
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "no temporary file"
        );
    }

    // covers: C-21
    #[test]
    fn it_never_asks_when_it_cant_remember_asking() {
        let dir = tempfile::tempdir().unwrap();
        // The cache "directory" is a file: nothing can be written there.
        let blocked = dir.path().join("cache");
        std::fs::write(&blocked, "").unwrap();
        let asked = Cell::new(0);
        let pending = start_in(
            blocked.join(FILE),
            version("0.1.0"),
            1_800_000_000,
            answering(Some("0.2.0"), &asked),
        );
        assert!(pending.is_none());
        assert_eq!(asked.get(), 0);
    }

    // covers: C-21
    #[test]
    fn a_slow_answer_doesnt_hold_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let at = 1_800_000_000;
        let (send, answer) = mpsc::channel::<Option<Version>>();
        let pending =
            start_in(path.clone(), version("0.1.0"), at, || answer).expect("a pending notice");
        let started = std::time::Instant::now();
        assert_eq!(finish_at(pending, at, Duration::from_millis(50)), None);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(send);
        // The question was asked: commands in the next hour don't ask again.
        let state = load(&path);
        assert_eq!((state.asked, state.answered), (at, 0));
    }
}
