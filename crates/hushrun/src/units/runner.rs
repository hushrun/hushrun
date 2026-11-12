//! \[ADR 0068\] hushrun's target runner: what Cargo and nextest start every test binary through
//! when a check's `tests` isn't off (C-32). It starts the binary as the runner it stands in for
//! would have, with the environment that runner would have given it, passes stdio, signals and
//! the exit status through untouched, and writes one file of facts per start: the binary, its
//! arguments, the tests a `--list` listed, and how it ended.
//!
//! \[ADR 0069\] It selects too: a binary `hushrun.json`'s `binaries` lists, by its path and the
//! SHA-256 of its bytes, isn't started. nextest's `--list` of it lists no test, so nextest runs
//! none of its tests; and plain `cargo test`'s one start of it exits 0. Any doubt (a file that
//! can't be read, another path, other bytes) starts it.
//!
//! \[ADR 0070\] `go test` starts its test binaries through it too, from `GOFLAGS`'s `-exec`, with
//! `go` in place of the variable: it starts each in its package's directory with Go's test log
//! (`-test.testlogfile`), gives the test `GOFLAGS` as COMMAND's environment had it, and doesn't
//! start one whose package `hushrun.json` lists.
//!
//! Its command line is `hushrun __hushrun-test-runner <VARIABLE> <N> <runner word>… <binary>
//! <argument>…`: `CARGO_TARGET_<VARIABLE>_RUNNER` is the variable hushrun set, and the `N` words
//! after it are the runner the environment had set there, which it chains to and gives back to
//! the test, or none.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use super::facts::{self, FactsFile, Invocation};

/// The hidden subcommand: never shown, never typed.
pub const SUBCOMMAND: &str = "__hushrun-test-runner";
/// The variable that names a run's directory (C-32).
pub const UNITS_VAR: &str = "HUSHRUN_UNITS";

/// Runs as the target runner when `args` (the process's, program first) say so; `None` for any
/// other command line, which hushrun parses as usual.
pub fn from_args(args: &[OsString]) -> Option<ExitCode> {
    if args.get(1).map(|a| a.as_os_str()) != Some(SUBCOMMAND.as_ref()) {
        return None;
    }
    Some(run(&args[2..]))
}

/// What the runner was told, past the subcommand.
struct Told {
    variable: String,
    chain: Vec<OsString>,
    binary: OsString,
    args: Vec<OsString>,
}

fn parse(args: &[OsString]) -> Option<Told> {
    let variable = args.first()?.to_str()?.to_string();
    let n: usize = args.get(1)?.to_str()?.parse().ok()?;
    let chain = args.get(2..2 + n)?.to_vec();
    let binary = args.get(2 + n)?.clone();
    let rest = args.get(3 + n..)?.to_vec();
    Some(Told {
        variable,
        chain,
        binary,
        args: rest,
    })
}

fn run(args: &[OsString]) -> ExitCode {
    let Some(told) = parse(args) else {
        eprintln!("error: {SUBCOMMAND} is hushrun's own target runner, started by Cargo");
        return ExitCode::from(2);
    };
    let dir = std::env::var_os(UNITS_VAR).map(PathBuf::from);
    // \[ADR 0070\] `go test` starts each test binary in its package's directory.
    let go = told.variable == super::gotest::RUNNER_WORD;
    let here = std::env::current_dir().ok();
    let reuse = dir.as_deref().is_some_and(|dir| {
        if go {
            here.as_deref()
                .is_some_and(|here| reused_package(dir, here))
        } else {
            reused(dir, Path::new(&told.binary))
        }
    });
    // \[ADR 0070\] Go's test log, which `go test` asks for itself when it caches a result: what
    // the tests opened, listed and started.
    let testlog = (go && !reuse).then_some(()).and(dir.as_ref()).map(|dir| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        dir.join(format!("go-{}-{nanos}.testlog", std::process::id()))
    });
    // \[ADR 0083\] Tests one by one: the binary's tests listed, those `reuseTests` names left out,
    // and its output read as it passes through. Its output can only be read through a pipe of
    // hushrun's, made before anything is left out: without one, nothing is.
    let watch = if go && !reuse {
        dir.as_deref()
            .zip(here.as_deref())
            .and_then(|(dir, here)| tests_here(dir, here, &told))
            .and_then(|at| Some((at, Watch::new()?)))
    } else {
        None
    };
    let mut args: Vec<OsString> = Vec::new();
    if let Some(log) = &testlog {
        let mut arg = OsString::from("-test.testlogfile=");
        arg.push(log);
        args.push(arg);
    }
    // First, before any argument of COMMAND's that isn't a flag, where `flag` stops: the tests
    // left out, and the profile Go's test main writes only once it returned from running them.
    let returned = watch.as_ref().zip(dir.as_ref()).map(|(_, dir)| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        dir.join(format!(".go-{}-{nanos}.memprofile", std::process::id()))
    });
    args.extend(watch.as_ref().and_then(|(at, _)| at.skip_arg()));
    if let Some(path) = &returned {
        let mut arg = OsString::from("-test.memprofile=");
        arg.push(path);
        args.push(arg);
    }
    args.extend(told.args.iter().cloned());
    let (program, mut command) = command_for(&told, go, &args);
    let listing = !go && told.args.iter().any(|a| a == "--list");
    // \[ADR 0069\] A binary the plan reuses: what the run would have said of its passing tests.
    if let Some(dir) = &dir
        && reuse
    {
        record(
            dir,
            Invocation {
                binary: PathBuf::from(&told.binary),
                args: told
                    .args
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
                listed: listing.then(Vec::new),
                exit: Some(0),
                signal: None,
                duration_ms: 0,
                reused: true,
                dir: go.then(|| here.clone()).flatten(),
                testlog: None,
                testlog_truncated: false,
            },
            None,
        );
        return ExitCode::SUCCESS;
    }
    if listing {
        command.stdout(Stdio::piped());
    }
    let watch = watch.map(|(at, watch)| (at, watch.attach(&mut command)));
    let started = Instant::now();
    let spawned = command.spawn();
    // The pipe's ends the command held close here, so that the output ends with the test's.
    drop(command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            eprintln!("error: can't start {}: {err}", program.to_string_lossy());
            return ExitCode::from(if err.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            });
        }
    };
    let forwarding = forward_signals(child.id());
    let reading = watch.map(|(at, reader)| {
        let json = at.flags.json;
        (at, read_through(reader, json))
    });
    // A list goes on to Cargo or nextest byte for byte, as it comes; hushrun keeps a copy.
    let mut listed_bytes = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let mut stdout = std::io::stdout();
        let mut buffer = [0u8; 16 * 1024];
        loop {
            match out.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = stdout.write_all(&buffer[..n]);
                    if listed_bytes.len() < 64 << 20 {
                        listed_bytes.extend_from_slice(&buffer[..n]);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = stdout.flush();
    }
    let status = loop {
        match child.wait() {
            Ok(status) => break Some(status),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break None,
        }
    };
    drop(forwarding);
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (exit, signal) = match &status {
        Some(status) => (status.code(), signal_of(status)),
        None => (None, None),
    };
    // \[ADR 0083\] What each test came to, from what the binary printed and how it ended. The
    // output goes on being passed through after that, to its end (`draining`).
    let mut draining = None;
    let tests = match reading {
        Some((at, mut reading)) => {
            let (out, read_whole) = reading.finish();
            draining = Some(reading);
            let returned = returned.as_ref().is_some_and(|path| {
                let written = std::fs::metadata(path).is_ok_and(|m| m.len() > 0);
                let _ = std::fs::remove_file(path);
                written
            });
            let ended = super::gotest::Ended {
                exit,
                signal,
                read_whole,
                returned,
            };
            let (tests, note) = super::gotest::outcome(&at, &out, ended);
            Some((at.unit.clone(), tests, note))
        }
        None => None,
    };
    if let Some(dir) = dir {
        let listed = listing.then(|| terse_list(&listed_bytes));
        let (lines, truncated) = match &testlog {
            Some(log) => {
                let read = read_testlog(log);
                let _ = std::fs::remove_file(log);
                match read {
                    Some((lines, truncated)) => (Some(lines), truncated),
                    None => (None, false),
                }
            }
            None => (None, false),
        };
        record(
            &dir,
            Invocation {
                binary: PathBuf::from(&told.binary),
                args: told
                    .args
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
                listed,
                exit,
                signal,
                duration_ms,
                reused: false,
                dir: go.then(|| here.clone()).flatten(),
                testlog: lines,
                testlog_truncated: truncated,
            },
            tests,
        );
    }
    // \[ADR 0083, amended\] What a program the test left running still prints goes on to `go
    // test` until it lets the output go, as it would without hushrun between them.
    if let Some(reading) = draining {
        reading.drain();
    }
    match (exit, signal) {
        (Some(code), _) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        (None, Some(signal)) => end_as(signal),
        (None, None) => ExitCode::from(1),
    }
}

/// The command that starts `told`'s binary with `args`, through the runner it chains to, with the
/// environment the runner it stands in for would have given it: its runner variable, or
/// `GOFLAGS`, as the environment had it, and no directory of hushrun's.
fn command_for(told: &Told, go: bool, args: &[OsString]) -> (OsString, Command) {
    let (program, rest) = match told.chain.split_first() {
        Some((first, more)) => {
            let mut rest: Vec<OsString> = more.to_vec();
            rest.push(told.binary.clone());
            rest.extend(args.iter().cloned());
            (first.clone(), rest)
        }
        None => (told.binary.clone(), args.to_vec()),
    };
    let mut command = Command::new(&program);
    command.args(&rest);
    if go {
        match std::env::var_os(super::gotest::GOFLAGS_WAS) {
            Some(was) => {
                let was = was.to_string_lossy().into_owned();
                match was.strip_prefix('=') {
                    Some(value) => command.env("GOFLAGS", value),
                    None => command.env_remove("GOFLAGS"),
                };
            }
            None => {
                command.env_remove("GOFLAGS");
            }
        }
        command.env_remove(super::gotest::GOFLAGS_WAS);
    } else {
        let runner_var = format!("CARGO_TARGET_{}_RUNNER", told.variable);
        if told.chain.is_empty() {
            command.env_remove(&runner_var);
        } else {
            let words: Vec<String> = told
                .chain
                .iter()
                .map(|w| w.to_string_lossy().into_owned())
                .collect();
            command.env(&runner_var, words.join(" "));
        }
    }
    command.env_remove(UNITS_VAR);
    (program, command)
}

/// The run's `hushrun.json`, when it reads.
fn run_file(dir: &Path) -> Option<facts::RunFile> {
    std::fs::read(dir.join(facts::RUN_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<facts::RunFile>(&bytes).ok())
        .filter(|run| run.schema == facts::RUN_SCHEMA)
}

/// \[ADR 0070\] The Go package whose directory is `here`, by its directory from the run's root
/// (`.` for the root); `None` outside it, or when either can't be read.
fn package_at(run: &facts::RunFile, here: &Path) -> Option<String> {
    let root = std::fs::canonicalize(&run.root).ok()?;
    let here = std::fs::canonicalize(here).ok()?;
    let inside = here.strip_prefix(&root).ok()?;
    let parts: Vec<String> = inside
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// \[ADR 0083\] The longest hushrun waits for a test binary to list its tests.
const LIST_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);
/// \[ADR 0083, amended\] How long after a test binary exited its output may still be open for its
/// tests to pass: a program it started that holds the output longer may print anything into it, so
/// none of the binary's tests passes. The output is passed on to its end either way, as `go test`
/// reads it without hushrun (Go 1.27 waited the 12 s such a program held it).
const OUTPUT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// \[ADR 0083\] With `unit: test` (C-39), what the Go package in `here` runs test by test: its
/// tests listed, and those `hushrun.json`'s `reuseTests` names to be left out. `None` with
/// anything that can't be told, and the package runs as a package.
fn tests_here(dir: &Path, here: &Path, told: &Told) -> Option<super::gotest::TestsAt> {
    let run = run_file(dir)?;
    if run.unit.as_deref() != Some("test") {
        return None;
    }
    let unit = package_at(&run, here)?;
    let reuse = run.reuse_tests.get(&unit).cloned().unwrap_or_default();
    let mut list = |extra: &[&str]| list_binary(told, extra, dir);
    // Said once a run, by the first start that finds it.
    let mut old = || {
        let first = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(".go-old"));
        if first.is_ok() {
            eprintln!(
                "hushrun: go test's tests are reused by package: this Go can't leave out one test (Go 1.20 and later can)"
            );
        }
    };
    super::gotest::prepare(
        &unit,
        Path::new(&told.binary),
        &told.args,
        &reuse,
        &mut list,
        &mut old,
    )
}

/// \[ADR 0083\] What `told`'s binary prints with `extra` for its arguments, as the run would start
/// it, when it exits 0 within [`LIST_BUDGET`]; `None` otherwise. A binary built with `-cover`
/// writes what listing covered into the run's directory `dir`, not COMMAND's.
fn list_binary(told: &Told, extra: &[&str], dir: &Path) -> Option<Vec<u8>> {
    let args: Vec<OsString> = extra.iter().map(OsString::from).collect();
    let (_, mut command) = command_for(told, true, &args);
    command.env("GOCOVERDIR", dir);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut out).take(16 << 20 | 1).read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > LIST_BUDGET => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(_) => return None,
        }
    };
    let bytes = reader.join().ok()?;
    status.success().then_some(bytes)
}

/// \[ADR 0083\] A pipe the test binary's output goes through: its standard output, and its
/// standard error too when the runner's are one file, as `go test` makes them, so that what it
/// prints keeps its order.
struct Watch {
    reader: std::io::PipeReader,
    out: std::io::PipeWriter,
    err: Option<std::io::PipeWriter>,
}

impl Watch {
    fn new() -> Option<Watch> {
        let (reader, out) = std::io::pipe().ok()?;
        let err = if stdout_is_stderr() {
            Some(out.try_clone().ok()?)
        } else {
            None
        };
        Some(Watch { reader, out, err })
    }

    /// Gives `command` the pipe, and the end to read it by.
    fn attach(self, command: &mut Command) -> std::io::PipeReader {
        command.stdout(self.out);
        if let Some(err) = self.err {
            command.stderr(err);
        }
        self.reader
    }
}

/// Whether this process's standard output and error are one file.
#[cfg(unix)]
fn stdout_is_stderr() -> bool {
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::MetadataExt as _;
    let meta = |fd: std::os::fd::BorrowedFd<'_>| {
        fd.try_clone_to_owned()
            .ok()
            .and_then(|fd| std::fs::File::from(fd).metadata().ok())
    };
    match (
        meta(std::io::stdout().as_fd()),
        meta(std::io::stderr().as_fd()),
    ) {
        (Some(a), Some(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn stdout_is_stderr() -> bool {
    false
}

/// \[ADR 0083\] A test binary's output being passed on byte for byte, and read.
struct Reading {
    output: std::sync::Arc<std::sync::Mutex<super::gotest::Output>>,
    done: std::sync::mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Passes what comes from `reader` on to this process's standard output as it comes, and reads it.
fn read_through(reader: std::io::PipeReader, json: bool) -> Reading {
    let output = std::sync::Arc::new(std::sync::Mutex::new(super::gotest::Output::new(json)));
    let (tell, done) = std::sync::mpsc::channel();
    let shared = std::sync::Arc::clone(&output);
    let thread = std::thread::spawn(move || {
        let mut reader = reader;
        let mut buffer = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let mut stdout = std::io::stdout().lock();
                    let _ = stdout.write_all(&buffer[..n]);
                    let _ = stdout.flush();
                    if let Ok(mut out) = shared.lock() {
                        out.feed(&buffer[..n]);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
        if let Ok(mut out) = shared.lock() {
            out.end();
        }
        let _ = tell.send(());
    });
    Reading {
        output,
        done,
        thread: Some(thread),
    }
}

impl Reading {
    /// What was read, and whether to the end: once the binary exited, its output ends when every
    /// program holding the pipe let it go. Read to the end within [`OUTPUT_GRACE`], it is whole.
    fn finish(&mut self) -> (super::gotest::Output, bool) {
        let whole = self.done.recv_timeout(OUTPUT_GRACE).is_ok();
        let out = match self.output.lock() {
            Ok(out) => out.clone(),
            Err(_) => return (super::gotest::Output::default(), false),
        };
        (out, whole)
    }

    /// Waits until the output ended, everything in it passed on.
    fn drain(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// \[ADR 0069\] Whether the run's `hushrun.json` reuses `binary`: it lists a binary at the same
/// real path whose SHA-256 is that of `binary`'s bytes now. Anything that can't be read is no.
fn reused(dir: &Path, binary: &Path) -> bool {
    let Some(run) = std::fs::read(dir.join(facts::RUN_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<facts::RunFile>(&bytes).ok())
        .filter(|run| run.schema == facts::RUN_SCHEMA)
    else {
        return false;
    };
    let Ok(real) = std::fs::canonicalize(binary) else {
        return false;
    };
    let Some(listed) = run
        .binaries
        .iter()
        .find(|b| std::fs::canonicalize(&b.path).is_ok_and(|p| p == real))
    else {
        return false;
    };
    std::fs::read(&real).is_ok_and(|bytes| hushrun_core::Digest::of(&bytes).hex() == listed.sha256)
}

/// \[ADR 0070\] Whether the run's `hushrun.json` reuses the Go package whose directory is `here`:
/// it lists that directory, from the repository's root (`.` for the root). Anything that can't be
/// read is no.
fn reused_package(dir: &Path, here: &Path) -> bool {
    run_file(dir)
        .is_some_and(|run| package_at(&run, here).is_some_and(|unit| run.reuse.contains(&unit)))
}

/// \[ADR 0070\] A Go test log's lines, and whether there were more than hushrun reads; `None` when
/// the binary wrote none, or not one that starts as Go's do.
fn read_testlog(path: &Path) -> Option<(Vec<String>, bool)> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if lines.next()? != "# test log" {
        return None;
    }
    let mut out = Vec::new();
    for line in lines {
        if out.len() >= super::gotest::MOST_LOG_LINES {
            return Some((out, true));
        }
        out.push(line.to_string());
    }
    Some((out, false))
}

/// The tests a libtest `--list --format terse` printed: `name: test` lines.
fn terse_list(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(str::to_string)
        .collect()
}

/// Writes one start's facts, whole, under a name no other start of this run has. A file that
/// can't be written changes nothing for the test: hushrun then knows less, and records less.
/// \[ADR 0083\] With `tests`, a Go package's tests one by one, the file is `v1`.
fn record(
    dir: &std::path::Path,
    invocation: Invocation,
    tests: Option<(String, Vec<facts::TestFacts>, Option<&'static str>)>,
) {
    // \[ADR 0070\] A start `go test` asked for says where it ran.
    let adapter = if invocation.dir.is_some() {
        super::gotest::ADAPTER
    } else {
        super::cargo::ADAPTER
    };
    let schema = if tests.is_some() {
        facts::FACTS_SCHEMA_V1
    } else {
        facts::FACTS_SCHEMA
    };
    // \[ADR 0083, amended\] Why no test of it but those that failed finished, which the run says.
    let note = tests.as_ref().and_then(|(_, _, note)| *note);
    let units = tests
        .map(|(unit, tests, _)| facts::UnitFacts {
            unit,
            tests: Some(tests),
            ..facts::UnitFacts::default()
        })
        .into_iter()
        .collect();
    let file = FactsFile {
        schema: schema.into(),
        adapter: adapter.into(),
        failures_by_unit: true,
        // \[C-32\] `go test` starts every test binary of one start of it: one start per `go test`.
        start: (adapter == super::gotest::ADAPTER).then(go_start).flatten(),
        invocation: Some(invocation),
        units,
        settings: note
            .map(|note| BTreeMap::from([("note".to_string(), serde_json::json!(note))]))
            .unwrap_or_default(),
        ..FactsFile::default()
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = dir.join(format!("{adapter}-{}-{nanos}.json", std::process::id()));
    if let Ok(bytes) = serde_json::to_vec(&file) {
        let _ = facts::write_whole(&path, &bytes);
    }
}

/// \[C-32\] The start of `go test` this runner belongs to: the `go` process that started it.
#[cfg(unix)]
fn go_start() -> Option<String> {
    Some(format!("go-test-{}", std::os::unix::process::parent_id()))
}

#[cfg(not(unix))]
fn go_start() -> Option<String> {
    None
}

#[cfg(unix)]
pub(super) fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
pub(super) fn signal_of(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Ends this process as the test's did, by the same signal, so that nextest and Cargo see what
/// they would have seen; 128 + the signal when it can't.
#[cfg(unix)]
pub(super) fn end_as(signal: i32) -> ExitCode {
    let _ = signal_hook::low_level::emulate_default_handler(signal);
    ExitCode::from(u8::try_from(128 + signal).unwrap_or(1))
}

#[cfg(not(unix))]
pub(super) fn end_as(signal: i32) -> ExitCode {
    ExitCode::from(u8::try_from(128 + signal).unwrap_or(1))
}

/// Passes the signals that stop a process on to the test while it runs: a terminal or nextest
/// may signal this process alone. Dropping what it returns stops passing them.
#[cfg(unix)]
pub(super) fn forward_signals(child: u32) -> Option<Forwarding> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let arrived = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    for signal in [SIGINT, SIGTERM, SIGHUP, SIGQUIT] {
        let value = usize::try_from(signal).ok()?;
        signal_hook::flag::register_usize(signal, Arc::clone(&arrived), value).ok()?;
    }
    let pid = i32::try_from(child)
        .ok()
        .and_then(rustix::process::Pid::from_raw)?;
    let (seen, stop) = (Arc::clone(&arrived), Arc::clone(&done));
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let signal = seen.swap(0, Ordering::Relaxed);
            if let Some(sig) = i32::try_from(signal)
                .ok()
                .filter(|s| *s != 0)
                .and_then(rustix::process::Signal::from_named_raw)
            {
                let _ = rustix::process::kill_process(pid, sig);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });
    Some(Forwarding(done))
}

/// While it lives, signals go on to the test.
#[cfg(unix)]
pub(super) struct Forwarding(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(unix)]
impl Drop for Forwarding {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Where there are no signals, nothing to pass on. Not `()`: the caller drops what it gets, and
/// rustc and clippy warn of dropping a value that has nothing to drop in the Windows build.
#[cfg(not(unix))]
pub(super) struct Forwarding;

#[cfg(not(unix))]
impl Drop for Forwarding {
    fn drop(&mut self) {}
}

#[cfg(not(unix))]
pub(super) fn forward_signals(_: u32) -> Option<Forwarding> {
    Some(Forwarding)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-32
    #[test]
    fn the_runner_reads_its_chain_and_the_binary() {
        let words = |text: &str| -> Vec<OsString> { text.split(' ').map(OsString::from).collect() };
        let told = parse(&words(
            "X86_64_UNKNOWN_LINUX_GNU 0 /t/deps/a-1 --exact x --nocapture",
        ))
        .unwrap();
        assert!(told.chain.is_empty());
        assert_eq!(told.binary, "/t/deps/a-1");
        assert_eq!(told.args, words("--exact x --nocapture"));
        let told = parse(&words(
            "AARCH64_UNKNOWN_LINUX_GNU 2 qemu-aarch64 -L /t/deps/a-1 --list",
        ))
        .unwrap();
        assert_eq!(told.chain, words("qemu-aarch64 -L"));
        assert_eq!(told.binary, "/t/deps/a-1");
        assert!(parse(&words("X 3 a b")).is_none());
        assert_eq!(
            terse_list(b"tests::a: test\ntests::b: test\nbench_x: benchmark\n"),
            ["tests::a", "tests::b"]
        );
    }
}
