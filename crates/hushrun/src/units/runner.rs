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
    let mut args: Vec<OsString> = Vec::new();
    if let Some(log) = &testlog {
        let mut arg = OsString::from("-test.testlogfile=");
        arg.push(log);
        args.push(arg);
    }
    args.extend(told.args.iter().cloned());
    let (program, rest) = match told.chain.split_first() {
        Some((first, more)) => {
            let mut rest: Vec<OsString> = more.to_vec();
            rest.push(told.binary.clone());
            rest.extend(args.iter().cloned());
            (first.clone(), rest)
        }
        None => (told.binary.clone(), args.clone()),
    };
    let mut command = Command::new(&program);
    command.args(&rest);
    // What the test would have seen without hushrun: its runner variable, or `GOFLAGS`, as the
    // environment had it, and no directory of hushrun's.
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
        );
        return ExitCode::SUCCESS;
    }
    if listing {
        command.stdout(Stdio::piped());
    }
    let started = Instant::now();
    let mut child = match command.spawn() {
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
        );
    }
    match (exit, signal) {
        (Some(code), _) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        (None, Some(signal)) => end_as(signal),
        (None, None) => ExitCode::from(1),
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
    let Some(run) = std::fs::read(dir.join(facts::RUN_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<facts::RunFile>(&bytes).ok())
        .filter(|run| run.schema == facts::RUN_SCHEMA)
    else {
        return false;
    };
    let (Ok(root), Ok(here)) = (
        std::fs::canonicalize(&run.root),
        std::fs::canonicalize(here),
    ) else {
        return false;
    };
    let Ok(inside) = here.strip_prefix(&root) else {
        return false;
    };
    let parts: Vec<String> = inside
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let unit = if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    };
    run.reuse.contains(&unit)
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
fn record(dir: &std::path::Path, invocation: Invocation) {
    // \[ADR 0070\] A start `go test` asked for says where it ran.
    let adapter = if invocation.dir.is_some() {
        super::gotest::ADAPTER
    } else {
        super::cargo::ADAPTER
    };
    let file = FactsFile {
        schema: facts::FACTS_SCHEMA.into(),
        adapter: adapter.into(),
        failures_by_unit: true,
        invocation: Some(invocation),
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

#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Ends this process as the test's did, by the same signal, so that nextest and Cargo see what
/// they would have seen; 128 + the signal when it can't.
#[cfg(unix)]
fn end_as(signal: i32) -> ExitCode {
    let _ = signal_hook::low_level::emulate_default_handler(signal);
    ExitCode::from(u8::try_from(128 + signal).unwrap_or(1))
}

#[cfg(not(unix))]
fn end_as(signal: i32) -> ExitCode {
    ExitCode::from(u8::try_from(128 + signal).unwrap_or(1))
}

/// Passes the signals that stop a process on to the test while it runs: a terminal or nextest
/// may signal this process alone. Dropping what it returns stops passing them.
#[cfg(unix)]
fn forward_signals(child: u32) -> Option<Forwarding> {
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
struct Forwarding(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(unix)]
impl Drop for Forwarding {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Where there are no signals, nothing to pass on. Not `()`: the caller drops what it gets, and
/// rustc and clippy warn of dropping a value that has nothing to drop in the Windows build.
#[cfg(not(unix))]
struct Forwarding;

#[cfg(not(unix))]
impl Drop for Forwarding {
    fn drop(&mut self) {}
}

#[cfg(not(unix))]
fn forward_signals(_: u32) -> Option<Forwarding> {
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
