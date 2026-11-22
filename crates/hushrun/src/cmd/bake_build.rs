//! B-72, B-74: the builds `hushrun build bake` runs itself, each in a store and a target folder of
//! its own beside the bake: the dependencies-only build (`--deps-only`), whose store the bake then
//! bakes, and the two builds `--report` times, cold and from the baked store.
//!
//! None of them reads or writes the person's store, a relay, or a baked store the bake didn't hand
//! it, so what they compile, reuse and measure is the bake's alone.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use hushrun_build::session::{Counts, Session, Stub};

use super::build_session;

/// A build a bake ran: how long it took on the wall clock, and what its session counted.
#[derive(Debug, Clone)]
pub(crate) struct Ran {
    pub(crate) wall: Duration,
    pub(crate) counts: Counts,
}

/// The folders of one of a bake's own builds, under the bake's aside folder: its store, and the
/// target folder its commands build into.
pub(crate) struct Own {
    pub(crate) store: PathBuf,
    pub(crate) target: PathBuf,
}

impl Own {
    pub(crate) fn under(aside: &Path, name: &str) -> Own {
        Own {
            store: aside.join(format!("{name}-store")),
            target: aside.join(format!("{name}-target")),
        }
    }
}

/// What the build needs to know of the repository.
pub(crate) struct Repo<'a> {
    /// The folder bake runs in, where COMMAND runs.
    pub(crate) folder: &'a Path,
    /// The checkout it is in.
    pub(crate) checkout: &'a Path,
    pub(crate) repository: Option<&'a str>,
    /// hushrun.yaml's `build: env:` (B-35).
    pub(crate) repository_env: &'a [String],
    /// A Go module's folder: its third-party packages are built too (B-72), and its builds get
    /// Go's cache program.
    pub(crate) go: bool,
}

/// Runs `command` in `folder` with `session`'s variables, building into `target` (cargo's target
/// folder, and a Go build cache of its own there), through [`in_bake`] and [`run_in_bake`] as
/// every build of a bake runs. `Err` when it can't start or doesn't succeed.
fn run(session: &Session, command: &[String], folder: &Path, target: &Path) -> Result<(), String> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| "no command to run".to_string())?;
    let mut built = in_bake(
        Command::new(program),
        folder,
        &[],
        Some(session),
        &[
            ("CARGO_TARGET_DIR", target.as_os_str().to_owned()),
            ("GOCACHE", target.join("go-build").into_os_string()),
        ],
    );
    built.args(args);
    run_in_bake(built, None).map_err(|ended| match ended {
        Ended::Unstarted(err) => format!("`{program}` couldn't start: {err}"),
        other => format!("`{}` {}", shell_words(command), other.words()),
    })
}

/// ADR 0102: the one way a bake runs anything of the repository's (its build of what's missing,
/// the dependencies-only build, `--report`'s builds, and what they ask of the toolchain first):
/// `command` in `folder` with the bake's environment less what it withholds ([`WITHHELD`]), CI's
/// variables for it (`vars`), the session's, `extra` (the target folders the bake gives it), no
/// results of its own (a `hushrun run` inside it looks up and records nothing: `HUSHRUN_BACKEND`
/// empty, and it knows it runs inside a run), no baked store of the person's, and its output to
/// stderr, so bake's own stdout stays its report. [`run_in_bake`] runs it.
pub(crate) fn in_bake(
    mut command: Command,
    folder: &Path,
    vars: &[(String, String)],
    session: Option<&Session>,
    extra: &[(&str, std::ffi::OsString)],
) -> Command {
    command
        .current_dir(folder)
        .envs(vars.iter().map(|(name, value)| (name, value)));
    if let Some(session) = session {
        command.envs(session.env());
    }
    command
        .env(hushrun_build::baked::LOWER, "")
        .env("HUSHRUN_BACKEND", "")
        .env(super::setup::variants::INSIDE_A_RUN, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()));
    for (name, value) in extra {
        command.env(name, value);
    }
    for name in WITHHELD {
        command.env_remove(name);
    }
    command
}

/// How a command [`run_in_bake`] ran didn't succeed.
#[derive(Debug)]
pub(crate) enum Ended {
    Unstarted(std::io::Error),
    Failed(std::process::ExitStatus),
    /// Stopped, with every process it started, past its time.
    Stopped(Duration),
    Lost(std::io::Error),
}

impl Ended {
    fn words(&self) -> String {
        match self {
            Ended::Unstarted(err) => format!("couldn't start: {err}"),
            Ended::Failed(status) => format!("failed ({status})"),
            Ended::Stopped(within) => {
                format!("stopped after {} minutes", within.as_secs().div_ceil(60))
            }
            Ended::Lost(err) => err.to_string(),
        }
    }
}

/// ADR 0102: runs a command [`in_bake`] made to its end, or, with `within`, stops it past that
/// with every process it started (it runs in a process group of its own then). On Linux the bake
/// is its subreaper while it runs: whatever it left running is killed when it ends, so no
/// process of a bake's build outlives it (the bake signs only after its builds return).
pub(crate) fn run_in_bake(mut command: Command, within: Option<Duration>) -> Result<(), Ended> {
    let _reaper = Reaper::start();
    #[cfg(unix)]
    if within.is_some() {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(Ended::Unstarted)?;
    let deadline = within.map(|within| (Instant::now() + within, within));
    let ended = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(status)) => break Err(Ended::Failed(status)),
            Ok(None) if deadline.is_some_and(|(at, _)| Instant::now() >= at) => {
                // The command's group, then every process under it, a group of its own too (a
                // test runner's children).
                #[cfg(unix)]
                if let Some(pid) = i32::try_from(child.id())
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
                {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                }
                super::run::kill_trees(&[child.id()]);
                let _ = child.kill();
                let _ = child.wait();
                break Err(Ended::Stopped(deadline.map_or(Duration::ZERO, |(_, w)| w)));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(err) => break Err(Ended::Lost(err)),
        }
    };
    Reaper::sweep();
    ended
}

/// [`run_in_bake`] for a command whose output the bake reads (what the toolchain says before a
/// build): its stdout, whatever its exit, with the subreaper held the same way.
fn output_in_bake(mut command: Command) -> std::io::Result<std::process::Output> {
    let _reaper = Reaper::start();
    command.stdout(Stdio::piped());
    let out = command.output();
    Reaper::sweep();
    out
}

/// `command` as a person would type it.
pub(crate) fn shell_words(command: &[String]) -> String {
    command
        .iter()
        .map(|w| {
            if !w.is_empty()
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@+,".contains(c))
            {
                w.clone()
            } else {
                format!("'{}'", w.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// B-72: the flags of a check's Go command (`go test -race -tags x ./...`) that change what its
/// third-party packages compile to, and so their keys: `-race`, `-msan`, `-asan`, `-cover` and
/// its mode and packages, `-tags`, `-trimpath`, `-gcflags`, `-asmflags`, `-ldflags`, `-mod`,
/// `-buildmode`, `-pgo`. `GOFLAGS` reaches `go` from the environment as it is. None for a command
/// that isn't `go`'s.
pub(crate) fn go_flags(command: &[String]) -> Vec<String> {
    if command.first().map(String::as_str) != Some("go") {
        return Vec::new();
    }
    const ALONE: &[&str] = &["-race", "-msan", "-asan", "-cover", "-trimpath"];
    const VALUED: &[&str] = &[
        "-tags",
        "-covermode",
        "-coverpkg",
        "-gcflags",
        "-asmflags",
        "-ldflags",
        "-mod",
        "-buildmode",
        "-pgo",
    ];
    let mut out = Vec::new();
    let mut words = command.iter().skip(2);
    while let Some(word) = words.next() {
        let flag = word.strip_prefix('-').map_or(word.as_str(), |_| {
            // `--race` reads as `-race` to go.
            word.strip_prefix("--")
                .map_or(word.as_str(), |rest| &word[word.len() - rest.len() - 1..])
        });
        let name = flag.split('=').next().unwrap_or(flag);
        if ALONE.contains(&name) {
            out.push(flag.to_string());
        } else if VALUED.contains(&name) {
            if flag.contains('=') {
                out.push(flag.to_string());
            } else if let Some(value) = words.next() {
                out.push(format!("{name}={value}"));
            }
        }
    }
    out
}

/// B-72: a Go module's third-party packages, every one its packages and their tests import that
/// isn't the standard library's or the main module's, as `go list -deps -test` names them with
/// the check's flags (`flags`: build tags change what is imported).
fn go_third_party(
    folder: &Path,
    session: &Session,
    flags: &[String],
) -> Result<Vec<String>, String> {
    let mut list = in_bake(Command::new("go"), folder, &[], Some(session), &[]);
    list.arg("list").args(flags).args([
        "-deps",
        "-test",
        "-f",
        "{{if and (not .Standard) .Module}}{{if not .Module.Main}}{{.ImportPath}}{{end}}{{end}}",
        "./...",
    ]);
    let out = output_in_bake(list).map_err(|err| format!("`go list` couldn't start: {err}"))?;
    if !out.status.success() {
        return Err(format!(
            "`go list -deps -test ./...` failed ({})",
            out.status
        ));
    }
    let mut packages: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(' '))
        .map(str::to_string)
        .collect();
    packages.sort();
    packages.dedup();
    Ok(packages)
}

/// B-72: COMMAND as a dependencies-only build runs it: `cargo test` compiles its tests and runs
/// none (`--no-run`), since rustdoc compiles doctests without hushrun's wrapper, against the
/// repository's own crates as the stubs left them, and running the stubs' tests proves nothing.
/// The doc tests are left out: `cargo test --doc` compiles the library's tests instead
/// (`--lib`), whose third-party crates are the doc tests' (cargo refuses `--doc` with
/// `--no-run`). Any other command runs as given.
pub(crate) fn deps_command(command: &[String]) -> Vec<String> {
    let mut out = command.to_vec();
    if command.first().map(String::as_str) != Some("cargo") {
        return out;
    }
    // cargo's own options before the subcommand: `+toolchain`, `-v`, `--locked`, `-Z x`, …
    let mut at = 1;
    while let Some(word) = command.get(at) {
        if word.starts_with('+') || (word.starts_with('-') && word != "--") {
            at += if matches!(word.as_str(), "-Z" | "-C" | "--config" | "--color") {
                2
            } else {
                1
            };
            continue;
        }
        break;
    }
    if !matches!(command.get(at).map(String::as_str), Some("test" | "t")) {
        return out;
    }
    let own = |word: &str| {
        command[at + 1..]
            .iter()
            .take_while(|w| *w != "--")
            .any(|w| w == word)
    };
    let ours = own("--no-run");
    if own("--doc") {
        let lib = own("--lib");
        let ends = out[at + 1..]
            .iter()
            .position(|w| w == "--")
            .map_or(out.len(), |i| at + 1 + i);
        out = out
            .iter()
            .enumerate()
            .filter_map(|(i, w)| {
                if i > at && i < ends && w == "--doc" {
                    (!lib).then(|| "--lib".to_string())
                } else {
                    Some(w.clone())
                }
            })
            .collect();
    }
    if !ours {
        out.insert(at + 1, "--no-run".to_string());
    }
    out
}

/// What a dependencies-only build did.
pub(crate) struct DepsBuilt {
    pub(crate) ran: Ran,
    /// The Go packages it built, when it built any.
    pub(crate) go_packages: usize,
}

/// B-72: the dependencies-only build, into `own`: COMMAND (the check's cargo command) with the
/// repository's own crates (`own_packages`, the lockfile's path packages, inside the checkout)
/// compiled as empty stubs, so cargo compiles every third-party crate as the real build does and
/// with the same keys; then, in a Go module, `go build` of its third-party packages. Its session
/// leaves the manifest of `check` (an id, or none) in `own.store`, which the bake reads.
pub(crate) fn deps_only(
    repo: &Repo<'_>,
    own: &Own,
    command: &[String],
    own_packages: Vec<String>,
    check: Option<hushrun_core::blocks::Hex32>,
    now: SystemTime,
) -> Result<DepsBuilt, String> {
    let mut session = build_session::begin_own(
        now,
        repo.repository,
        repo.repository_env,
        &own.store,
        repo.folder,
        repo.go,
    )?;
    let started = Instant::now();
    let result = (|| {
        if !command.is_empty() {
            session
                .set_stub(Stub {
                    packages: own_packages,
                    under: repo.checkout.to_path_buf(),
                })
                .map_err(|err| format!("the session couldn't be written: {err}"))?;
        }
        session.set_manifest_slot(hushrun_build::manifests::Slot {
            check,
            target: build_session::platform(),
            checkout: repo.checkout.to_str().map(str::to_string),
            go: session.go().map(|go| go.platform.clone()),
        });
        // A Go command isn't run (it would build and test the module's own packages): its flags
        // go to the third-party build instead.
        let go_command = command.first().map(String::as_str) == Some("go");
        if !command.is_empty() && !go_command {
            run(&session, &deps_command(command), repo.folder, &own.target)?;
        }
        let mut go_packages = 0;
        if repo.go && session.go().is_some() {
            let flags = go_flags(command);
            let packages = go_third_party(repo.folder, &session, &flags)?;
            go_packages = packages.len();
            if !packages.is_empty() {
                let mut build = vec!["go".to_string(), "build".to_string()];
                build.extend(flags);
                build.extend(packages);
                run(&session, &build, repo.folder, &own.target)?;
            }
        }
        Ok::<usize, String>(go_packages)
    })();
    let wall = started.elapsed();
    let summary = build_session::finish(session, SystemTime::now());
    let go_packages = result?;
    Ok(DepsBuilt {
        ran: Ran {
            wall,
            counts: summary.counts().clone(),
        },
        go_packages,
    })
}

/// B-74: COMMAND timed with nothing to reuse: a fresh store and a fresh target folder, as a
/// sandbox with no image builds it. With `stub`, the repository's own crates are stubs, as the
/// dependencies-only build compiled them.
pub(crate) fn cold(
    repo: &Repo<'_>,
    own: &Own,
    command: &[String],
    stub: Option<Vec<String>>,
) -> Result<Ran, String> {
    timed(repo, own, command, stub, None)
}

/// B-74: COMMAND timed from the baked store at `lower`, with a fresh store above it and a fresh
/// target folder, as a sandbox started from the image runs it. The state's words come back too.
pub(crate) fn warm(
    repo: &Repo<'_>,
    own: &Own,
    command: &[String],
    stub: Option<Vec<String>>,
    lower: (&Path, Option<&str>, &hushrun_core::policy::BakedTrust),
) -> Result<(Ran, String), String> {
    let mut words = String::new();
    let ran = timed(repo, own, command, stub, Some((lower, &mut words)))?;
    Ok((ran, words))
}

#[allow(clippy::type_complexity)]
fn timed(
    repo: &Repo<'_>,
    own: &Own,
    command: &[String],
    stub: Option<Vec<String>>,
    lower: Option<(
        (&Path, Option<&str>, &hushrun_core::policy::BakedTrust),
        &mut String,
    )>,
) -> Result<Ran, String> {
    let mut session = build_session::begin_own(
        SystemTime::now(),
        repo.repository,
        repo.repository_env,
        &own.store,
        repo.folder,
        repo.go,
    )?;
    let stubbed = stub.is_some();
    let prepared = (|| {
        if let Some(packages) = stub {
            session
                .set_stub(Stub {
                    packages,
                    under: repo.checkout.to_path_buf(),
                })
                .map_err(|err| format!("the session couldn't be written: {err}"))?;
        }
        if let Some(((folder, go, signers), words)) = lower {
            *words = build_session::give_own_lower(
                &mut session,
                folder,
                repo.repository,
                repo.folder,
                go,
                signers,
            );
        }
        Ok::<(), String>(())
    })();
    // B-72: a build with the repository's own crates as stubs runs no test of theirs.
    let command = if stubbed {
        deps_command(command)
    } else {
        command.to_vec()
    };
    let started = Instant::now();
    let result = prepared.and_then(|()| run(&session, &command, repo.folder, &own.target));
    let wall = started.elapsed();
    let summary = build_session::finish(session, SystemTime::now());
    result?;
    Ok(Ran {
        wall,
        counts: summary.counts().clone(),
    })
}

/// B-75: the most a bake's build of what's missing may take before it is stopped, and the bake
/// goes on with what the backend holds then.
pub(crate) const BUILD_WITHIN: Duration = Duration::from_secs(30 * 60);

/// B-75: whether a bake in CI on the default branch builds before it bakes: the one decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Behind {
    /// Nothing to build, and why: every check's newest build on the backend is of the commit
    /// checked out, or of one that commit doesn't come after.
    Current(String),
    /// These checks' aren't, each with why: the backend has no build of it, its newest names no
    /// commit, or names one the commit checked out descends from. Only they are built.
    Build(Vec<(Option<hushrun_core::blocks::Hex32>, String)>),
}

/// B-75: from what `open` answered for each check (its newest manifest and the commit its run
/// named) and the commit checked out, `head`: a check is behind when the backend holds no build
/// of it, a build of no named commit, or one of a commit `head` descends from (`descends`:
/// `Some(true)` when the commit is an ancestor of `head`, `Some(false)` when it isn't, `None`
/// when the checkout can't tell). A check that compiles nothing (a linter, a JS test) closes an
/// empty manifest: one of a named commit counts as its build of that commit like any other. A
/// newest build of a commit `head` doesn't descend from is a later one's (a re-run of an older
/// bake, a retried old pipeline): building `head` would set the backend's newest build, and the
/// image, back, so nothing is built. With no `head`, or a commit the checkout's history doesn't
/// hold, nothing is built and the backend's newest is baked as it is.
pub(crate) fn behind(
    newest: &[super::build_relay::saved::Newest],
    head: Option<&str>,
    descends: &dyn Fn(&str) -> Option<bool>,
) -> Behind {
    let Some(head) = head else {
        return Behind::Current(
            "no commit is checked out to compare the backend's newest build with: nothing to build"
                .into(),
        );
    };
    let short = |c: &str| c.chars().take(12).collect::<String>();
    let mut build = Vec::new();
    for (check, pairs, commit) in newest {
        match commit.as_deref() {
            None if pairs.is_empty() => build.push((
                *check,
                "the backend holds no build of it on the default branch".to_string(),
            )),
            None => build.push((
                *check,
                "the backend doesn't say which commit its newest build is of".to_string(),
            )),
            Some(commit) if commit == head => {}
            Some(commit) => match descends(commit) {
                Some(true) => build.push((
                    *check,
                    format!(
                        "its newest build is of {}, and the default branch is at {} (CI skipped the checks this code passed elsewhere)",
                        short(commit),
                        short(head)
                    ),
                )),
                Some(false) => {
                    return Behind::Current(format!(
                        "the backend's newest build is of {}, which {} doesn't come after (a later commit's, or another history's): nothing to build",
                        short(commit),
                        short(head)
                    ));
                }
                None => {
                    return Behind::Current(format!(
                        "the backend's newest build is of {}, which this checkout's history doesn't hold, so whether {} comes after it can't be told: nothing to build (check out the whole history, `fetch-depth: 0`, to build)",
                        short(commit),
                        short(head)
                    ));
                }
            },
        }
    }
    if build.is_empty() {
        Behind::Current(
            "the backend's newest build is of the commit checked out: nothing to build".into(),
        )
    } else {
        Behind::Build(build)
    }
}

/// Whether the commit `older` is an ancestor of the checkout's `HEAD` at `checkout`, as git
/// says: `None` when it can't (a commit the history doesn't hold, a shallow checkout).
pub(crate) fn descends(checkout: &Path, older: &str) -> Option<bool> {
    if !hushrun_core::blocks::is_commit(older) {
        return None;
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["merge-base", "--is-ancestor", older, "HEAD"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// B-75: how long a bake's build waits, after each check, for what it built to go up: the bake
/// closes a check's build only once all of it is on the backend (its store goes with its job).
pub(crate) const UPLOADS_WITHIN: Duration = Duration::from_secs(10 * 60);

/// B-75 (ADR 0102): the variables a bake's build never hands its commands: the credentials the
/// bake itself signs, reads the backend and pushes with. The bake holds what it needs of them;
/// what the build runs (build scripts, proc macros, tests, a copied step's tools) gets none.
pub(crate) const WITHHELD: &[&str] = &[
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_RUNTIME_TOKEN",
    "SIGSTORE_ID_TOKEN",
    "HUSHRUN_ID_TOKEN",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "CI_JOB_TOKEN",
    "CI_JOB_JWT",
    "CI_JOB_JWT_V2",
    "CI_REGISTRY_PASSWORD",
    "CI_DEPLOY_PASSWORD",
    "HUSHRUN_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_GITLAB_TOKEN",
    "HUSHRUN_REPOSITORY_TOKEN",
    "HUSHRUN_REPOSITORY_TOKEN_COMMAND",
    "HUSHRUN_ORG_TOKEN_COMMAND",
    "HUSHRUN_SIGNING_KEY",
    "HUSHRUN_SIGNING_KEY_FILE",
    "HUSHRUN_MACHINE_KEY",
    "HUSHRUN_MACHINE_KEY_FILE",
];

/// B-75: runs each check as CI's build job runs it (`checks`: its command, shell, folder and the
/// variables CI's file gives it), through a session that shares through the process's relay as
/// CI on the default branch, in the bake's store, compiling what no trusted store holds. Each
/// builds into the target folder cargo would give it in CI ([`Target`]). All of them within
/// `within`, after which the one running is stopped, with every process it started, and the rest
/// are left. A check's build closes a manifest only when it succeeded and all it built went up
/// ([`super::build_relay::Ending::Bake`]). Each line for the log comes back; `Err` only when a
/// session can't begin. Each command runs through [`in_bake`] and [`run_in_bake`]: no credential
/// of the job's, and nothing it left running outlives it (the bake signs only after this returns).
pub(crate) fn build_missing(
    repo: &Repo<'_>,
    checks: &[super::setup::replay::Replay],
    store: &Path,
    cc_maps: bool,
    start: &dyn Fn(&str) -> Option<super::build_session::ShareStart>,
    within: Duration,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let deadline = Instant::now() + within;
    let target = Target::of(repo.checkout, checks, store);
    if let Some(line) = &target.said {
        lines.push(line.clone());
    }
    for (index, replay) in checks.iter().enumerate() {
        let name = &replay.check;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            lines.push(format!(
                "check `{name}` wasn't built: the build had {} minutes",
                within.as_secs() / 60
            ));
            continue;
        }
        let share = start(name).ok_or_else(|| "no credential to share with".to_string())?;
        let session = build_session::begin_for_bake(
            SystemTime::now(),
            repo.repository,
            repo.repository_env,
            cc_maps,
            name,
            store,
            share,
        )?;
        let started = Instant::now();
        let folder = repo.checkout.join(&replay.dir);
        let ran = run_check(
            &session,
            replay,
            &folder,
            target.dirs.get(index).cloned().flatten().as_deref(),
            store,
            left,
        );
        let summary = build_session::finish_as(
            session,
            SystemTime::now(),
            super::build_relay::Ending::Bake {
                succeeded: ran.is_ok(),
                within: UPLOADS_WITHIN,
            },
        );
        let counts = summary.counts();
        lines.push(format!(
            "check `{name}`: {} in {:.0} s ({} of {} compiles reused, {} built)",
            match &ran {
                Ok(()) => "built".to_string(),
                Err(why) => why.clone(),
            },
            started.elapsed().as_secs_f64(),
            counts.reused,
            counts.compiles,
            counts.built_and_stored + counts.built_not_stored
        ));
        lines.extend(summary.lines().iter().cloned());
    }
    target.remove();
    Ok(lines)
}

/// B-75: the target folder each of a bake's checks builds into, fresh, as a CI job's: the one
/// cargo gives it in CI where that is free (`CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` as
/// CI's variables or the job set them, `.cargo/config.toml`'s `build.target-dir`, else the
/// workspace's `target`, as `cargo metadata` says in the check's folder with its variables; the
/// bake's checkout is fresh), so that what is keyed by the target folder's path is keyed as in
/// CI; else, where one is there already, a fresh folder in the bake's store for it, and a line
/// says so. Checks CI builds into one folder build into one here too. Each folder is removed
/// once the checks are built.
struct Target {
    /// For each check, the folder `CARGO_TARGET_DIR` names: `None` for the one cargo gives it.
    dirs: Vec<Option<PathBuf>>,
    /// The folders the build may make, to remove after it: those that weren't there before.
    made: Vec<PathBuf>,
    said: Option<String>,
}

impl Target {
    fn of(checkout: &Path, checks: &[super::setup::replay::Replay], store: &Path) -> Target {
        let env = |name: &str| {
            hushrun_ci::var_os(name)
                .filter(|v| !v.is_empty())
                .and_then(|v| v.into_string().ok())
        };
        Target::decide(
            checkout,
            checks,
            store,
            &|replay, folder| cargo_target(replay, folder, &output_in_bake),
            &env,
        )
    }

    /// [`Target::of`] with `cargo`'s answer for a check, and `env` the bake's own variables.
    fn decide(
        checkout: &Path,
        checks: &[super::setup::replay::Replay],
        store: &Path,
        cargo: &dyn Fn(&super::setup::replay::Replay, &Path) -> Option<PathBuf>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Target {
        let mut dirs = Vec::new();
        let mut made = Vec::new();
        let mut fresh: Vec<(Vec<PathBuf>, PathBuf)> = Vec::new();
        let mut there = Vec::new();
        for replay in checks {
            let folder = checkout.join(&replay.dir);
            let candidates: Vec<PathBuf> = match cargo(replay, &folder) {
                Some(dir) => vec![dir],
                None => {
                    let named = ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"]
                        .iter()
                        .find_map(|name| {
                            replay
                                .env
                                .iter()
                                .rev()
                                .find(|(n, _)| n == name)
                                .map(|(_, v)| v.clone())
                                .or_else(|| env(name))
                        });
                    match named {
                        Some(dir) => vec![folder.join(dir)],
                        None => vec![checkout.join("target"), folder.join("target")],
                    }
                }
            };
            if let Some((_, dir)) = fresh.iter().find(|(c, _)| *c == candidates) {
                dirs.push(Some(dir.clone()));
                continue;
            }
            let existing: Vec<&PathBuf> = candidates
                .iter()
                .filter(|dir| dir.exists() && !made.contains(*dir))
                .collect();
            if existing.is_empty() {
                for dir in &candidates {
                    if !made.contains(dir) {
                        made.push(dir.clone());
                    }
                }
                dirs.push(None);
            } else {
                there.extend(existing.into_iter().cloned());
                let dir = store
                    .join("tmp")
                    .join(hushrun_build::baked::unique("bake-build-target"));
                made.push(dir.clone());
                fresh.push((candidates, dir.clone()));
                dirs.push(Some(dir));
            }
        }
        let said = (!there.is_empty()).then(|| {
            format!(
                "the checkout has a target folder already ({}), so the build used a fresh one elsewhere: what is keyed by the target folder's path gets other keys than CI's",
                there
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        Target { dirs, made, said }
    }

    fn remove(&self) {
        for dir in &self.made {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// The target folder cargo gives a check in `folder` with its variables, as `cargo metadata`
/// says: `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `.cargo/config.toml`'s `build.target-dir`
/// and the workspace's root all count, as for its build. `None` where there is no Cargo project.
/// `output` runs it ([`output_in_bake`] in a bake).
fn cargo_target(
    replay: &super::setup::replay::Replay,
    folder: &Path,
    output: &dyn Fn(Command) -> std::io::Result<std::process::Output>,
) -> Option<PathBuf> {
    if !folder
        .ancestors()
        .any(|dir| dir.join("Cargo.toml").is_file())
    {
        return None;
    }
    let mut metadata = in_bake(Command::new("cargo"), folder, &replay.env, None, &[]);
    metadata
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .stderr(Stdio::null());
    let out = output(metadata).ok()?;
    if !out.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    value["target_directory"].as_str().map(PathBuf::from)
}

/// A check of a bake's build (B-75) run as CI's job runs it: COMMAND through the shell CI's job
/// runs it with ([`super::setup::replay::Shell`]), in `folder`, through [`in_bake`] with CI's
/// variables for it, and stopped with every process it started past `within`. `Err` says how it
/// ended otherwise.
fn run_check(
    session: &Session,
    replay: &super::setup::replay::Replay,
    folder: &Path,
    target: Option<&Path>,
    store: &Path,
    within: Duration,
) -> Result<(), String> {
    use super::setup::replay::Shell;
    let extra: Vec<(&str, std::ffi::OsString)> = target
        .map(|t| ("CARGO_TARGET_DIR", t.as_os_str().to_owned()))
        .into_iter()
        .collect();
    let mut script = None;
    let (program, args): (String, Vec<std::ffi::OsString>) = match &replay.shell {
        Shell::File(template) => {
            let file = store
                .join("tmp")
                .join(hushrun_build::baked::unique("bake-step"));
            let written = std::fs::create_dir_all(store.join("tmp"))
                .and_then(|()| std::fs::write(&file, format!("{}\n", replay.line)));
            if let Err(err) = written {
                return Err(format!("its script couldn't be written: {err}"));
            }
            let (program, rest) = template
                .split_first()
                .ok_or_else(|| "its shell names no program".to_string())?;
            let args = rest
                .iter()
                .map(|word| {
                    if word == "{0}" {
                        file.clone().into_os_string()
                    } else {
                        word.into()
                    }
                })
                .collect();
            script = Some(file);
            (program.clone(), args)
        }
        Shell::Inline(program) => (
            program.clone(),
            vec!["-c".into(), replay.line.clone().into()],
        ),
        Shell::Image => {
            // GitLab's runner: bash where the image has it, else sh.
            let bash = ["/usr/local/bin/bash", "/usr/bin/bash", "/bin/bash"]
                .iter()
                .any(|p| Path::new(p).is_file());
            if bash {
                (
                    "bash".into(),
                    vec![
                        "-e".into(),
                        "-o".into(),
                        "pipefail".into(),
                        "-c".into(),
                        replay.line.clone().into(),
                    ],
                )
            } else {
                (
                    "sh".into(),
                    vec!["-e".into(), "-c".into(), replay.line.clone().into()],
                )
            }
        }
    };
    let mut command = in_bake(
        Command::new(&program),
        folder,
        &replay.env,
        Some(session),
        &extra,
    );
    command.args(&args);
    let ran = run_in_bake(command, Some(within)).map_err(|ended| match ended {
        Ended::Unstarted(err) => format!("`{program}` couldn't start: {err}"),
        other => other.words(),
    });
    if let Some(file) = script {
        let _ = std::fs::remove_file(file);
    }
    ran
}

/// B-75 (ADR 0102): while it lives, on Linux, the bake is its build's subreaper, so a process the
/// build left running (a daemon, a test runner's child in a group of its own) becomes the bake's
/// child when its parent ends, and [`Reaper::sweep`] kills and reaps it. Elsewhere it does
/// nothing: what a build leaves behind there is the system's.
struct Reaper {
    #[cfg(target_os = "linux")]
    set: bool,
}

impl Reaper {
    fn start() -> Reaper {
        #[cfg(target_os = "linux")]
        {
            let set = rustix::process::set_child_subreaper(Some(rustix::process::getpid())).is_ok();
            Reaper { set }
        }
        #[cfg(not(target_os = "linux"))]
        Reaper {}
    }

    /// Kills and reaps every child of this process, tree and all, until none is left: the bake
    /// starts no process of its own while its build runs, so each is the build's.
    fn sweep() {
        #[cfg(target_os = "linux")]
        for _ in 0..100 {
            let me = std::process::id();
            let children: Vec<u32> = super::run::processes()
                .into_iter()
                .filter(|(_, parent)| *parent == me)
                .map(|(pid, _)| pid)
                .collect();
            if children.is_empty() {
                return;
            }
            super::run::kill_trees(&children);
            for pid in children {
                if let Some(pid) = i32::try_from(pid)
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
                {
                    let _ =
                        rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::empty());
                }
            }
        }
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        Reaper::sweep();
        #[cfg(target_os = "linux")]
        if self.set {
            let _ = rustix::process::set_child_subreaper(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hushrun_core::blocks::{Hex32, Pair};

    // covers: B-75
    // The one decision, every arm: nothing to compare, nothing held, no commit named, the same
    // commit, an older one HEAD comes after, a later or unrelated one, one the checkout can't
    // place.
    #[test]
    fn a_bake_builds_only_what_comes_after_the_backend_s_newest_build() {
        let head = "a".repeat(40);
        let parent = "b".repeat(40);
        let later = "c".repeat(40);
        let gone = "d".repeat(40);
        let pair = vec![Pair(Hex32::sha256(b"1"), Hex32::sha256(b"2"))];
        let newest = |commit: Option<&str>| vec![(None, pair.clone(), commit.map(str::to_string))];
        let descends = |older: &str| match older {
            c if c == parent => Some(true),
            c if c == later => Some(false),
            _ => None,
        };
        let build = |b: &Behind| matches!(b, Behind::Build(_));
        assert!(!build(&behind(&newest(Some(&parent)), None, &descends)));
        assert!(build(&behind(
            &[(None, Vec::new(), None)],
            Some(&head),
            &descends
        )));
        assert!(build(&behind(&newest(None), Some(&head), &descends)));
        assert!(!build(&behind(
            &newest(Some(&head)),
            Some(&head),
            &descends
        )));
        assert!(build(&behind(
            &newest(Some(&parent)),
            Some(&head),
            &descends
        )));
        // A re-run of an older bake: the backend's newest is a later commit's.
        let older = behind(&newest(Some(&later)), Some(&head), &descends);
        assert!(
            matches!(&older, Behind::Current(w) if w.contains("doesn't come after")),
            "{older:?}"
        );
        // A shallow checkout that doesn't hold the newest build's commit can't tell.
        let shallow = behind(&newest(Some(&gone)), Some(&head), &descends);
        assert!(
            matches!(&shallow, Behind::Current(w) if w.contains("fetch-depth")),
            "{shallow:?}"
        );
        // Several checks: one behind is enough; one later wins over all.
        let two = vec![
            (None, pair.clone(), Some(head.clone())),
            (None, pair.clone(), Some(parent.clone())),
        ];
        assert!(build(&behind(&two, Some(&head), &descends)));
    }

    // covers: B-75
    // A check that compiles nothing closes an empty manifest, of the commit its run named: built
    // at that commit, never "no build". Only the checks that are behind are built.
    #[test]
    fn a_check_that_compiles_nothing_is_built_at_the_commit_its_empty_manifest_names() {
        let head = "a".repeat(40);
        let parent = "b".repeat(40);
        let pair = vec![Pair(Hex32::sha256(b"1"), Hex32::sha256(b"2"))];
        let (build, lint) = (Hex32::sha256(b"build"), Hex32::sha256(b"lint"));
        let descends = |older: &str| (older == parent).then_some(true);
        let current = behind(
            &[
                (Some(build), pair.clone(), Some(head.clone())),
                (Some(lint), Vec::new(), Some(head.clone())),
            ],
            Some(&head),
            &descends,
        );
        assert!(matches!(current, Behind::Current(_)), "{current:?}");
        let one = behind(
            &[
                (Some(build), pair.clone(), Some(parent.clone())),
                (Some(lint), Vec::new(), Some(head.clone())),
            ],
            Some(&head),
            &descends,
        );
        let Behind::Build(checks) = one else {
            panic!("{one:?}");
        };
        assert_eq!(
            checks.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
            [Some(build)]
        );
    }

    // covers: B-72
    #[test]
    fn a_dependencies_only_cargo_test_compiles_its_tests_and_runs_none() {
        let words = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        let of = |s: &str| deps_command(&words(s)).join(" ");
        assert_eq!(of("cargo test --locked"), "cargo test --no-run --locked");
        assert_eq!(
            of("cargo +1.94 -Z unstable-options t -p a -- --nocapture"),
            "cargo +1.94 -Z unstable-options t --no-run -p a -- --nocapture"
        );
        assert_eq!(
            of("cargo test --no-run --locked"),
            "cargo test --no-run --locked"
        );
        // `--no-run` after `--` is the harness's, not cargo's.
        assert_eq!(
            of("cargo test -- --no-run"),
            "cargo test --no-run -- --no-run"
        );
        // Doc tests are left out: cargo takes no `--no-run` with `--doc`.
        assert_eq!(of("cargo test --doc"), "cargo test --no-run --lib");
        assert_eq!(
            of("cargo test --doc --lib -p a -- --doc"),
            "cargo test --no-run --lib -p a -- --doc"
        );
        assert_eq!(of("cargo build --locked"), "cargo build --locked");
        assert_eq!(of("make test"), "make test");
    }

    fn replay(check: &str, dir: &str, env: &[(&str, &str)]) -> super::super::setup::replay::Replay {
        super::super::setup::replay::Replay {
            check: check.into(),
            line: "cargo build".into(),
            dir: dir.into(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            left_out: Vec::new(),
            from: "ci.yml".into(),
            shell: super::super::setup::replay::Shell::Inline("sh".into()),
        }
    }

    // covers: B-75
    // Each check builds into the target folder cargo gives it in CI: `.cargo/config.toml`'s
    // `build.target-dir` and `CARGO_BUILD_TARGET_DIR` count as `CARGO_TARGET_DIR` does, and each
    // check gets its own, never the first check's. A folder there already gives that check (and
    // every check CI builds into it) a fresh one in the store.
    #[test]
    fn each_check_builds_into_the_target_folder_cargo_gives_it() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("repo");
        let store = dir.path().join("store");
        std::fs::create_dir_all(checkout.join("app/src")).unwrap();
        std::fs::create_dir_all(checkout.join("app/.cargo")).unwrap();
        std::fs::write(
            checkout.join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(checkout.join("app/src/lib.rs"), "").unwrap();
        std::fs::write(
            checkout.join("app/.cargo/config.toml"),
            "[build]\ntarget-dir = \"out\"\n",
        )
        .unwrap();
        // cargo's own answer: the config's folder. (Run as a bake runs it but for the subreaper,
        // whose sweep would take other tests' processes, and with none of this process's target
        // folder variables.)
        let cargo = |replay: &super::super::setup::replay::Replay, folder: &Path| {
            cargo_target(replay, folder, &|mut command: Command| {
                command
                    .env_remove("CARGO_TARGET_DIR")
                    .env_remove("CARGO_BUILD_TARGET_DIR")
                    .stdout(Stdio::piped())
                    .output()
            })
        };
        let target = Target::decide(
            &checkout,
            &[replay("a", "app", &[])],
            &store,
            &cargo,
            &|_| None,
        );
        assert_eq!(target.dirs, [None]);
        assert!(
            target
                .made
                .iter()
                .any(|d| d.ends_with("app/out") && !d.ends_with("target")),
            "{:?}",
            target.made
        );
        // There already: a fresh one in the store.
        std::fs::create_dir_all(checkout.join("app/out")).unwrap();
        let target = Target::decide(
            &checkout,
            &[replay("a", "app", &[])],
            &store,
            &cargo,
            &|_| None,
        );
        assert!(target.dirs[0].as_ref().unwrap().starts_with(&store));
        assert!(target.said.as_deref().unwrap().contains("app/out"));
        // Without cargo's answer: CI's variables, each check its own.
        let none = |_: &super::super::setup::replay::Replay, _: &Path| None;
        let checks = [
            replay("a", "", &[("CARGO_TARGET_DIR", "/t/a")]),
            replay("b", "", &[("CARGO_BUILD_TARGET_DIR", "/t/b")]),
            replay("c", "", &[("CARGO_TARGET_DIR", "/t/a")]),
        ];
        let target = Target::decide(&checkout, &checks, &store, &none, &|_| None);
        assert_eq!(target.dirs, [None, None, None]);
        assert!(target.made.contains(&PathBuf::from("/t/a")));
        assert!(target.made.contains(&PathBuf::from("/t/b")));
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let (a, b) = (a.to_str().unwrap(), b.to_str().unwrap());
        let checks = [
            replay("a", "", &[("CARGO_TARGET_DIR", a)]),
            replay("b", "", &[("CARGO_BUILD_TARGET_DIR", b)]),
            replay("c", "", &[("CARGO_TARGET_DIR", a)]),
        ];
        let target = Target::decide(&checkout, &checks, &store, &none, &|_| None);
        let fresh: Vec<&PathBuf> = target.dirs.iter().map(|d| d.as_ref().unwrap()).collect();
        assert_ne!(
            fresh[0], fresh[1],
            "the second check got the first's folder"
        );
        assert_eq!(
            fresh[0], fresh[2],
            "checks CI builds into one folder share one"
        );
    }

    // covers: B-72
    #[test]
    fn a_go_check_s_flags_go_to_its_dependencies_build() {
        let words = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            go_flags(&words(
                "go test -race -tags integration -count=1 -coverpkg=./... -v ./..."
            )),
            ["-race", "-tags=integration", "-coverpkg=./..."]
        );
        assert_eq!(
            go_flags(&words("go build --trimpath -ldflags=-s ./cmd/x")),
            ["-trimpath", "-ldflags=-s"]
        );
        assert!(go_flags(&words("cargo test -race")).is_empty());
    }

    // covers: B-75
    #[test]
    fn git_says_which_commit_comes_after_which() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);
        let first = git(&["rev-parse", "HEAD"]);
        git(&["commit", "-q", "--allow-empty", "-m", "two"]);
        let second = git(&["rev-parse", "HEAD"]);
        assert_eq!(descends(dir.path(), &first), Some(true));
        git(&["checkout", "-q", &first]);
        assert_eq!(descends(dir.path(), &second), Some(false));
        assert_eq!(descends(dir.path(), &"e".repeat(40)), None);
        assert_eq!(descends(dir.path(), "not a commit"), None);
    }
}
