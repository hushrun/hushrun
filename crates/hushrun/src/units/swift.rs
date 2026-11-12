//! \[ADR 0086\] The `swift test` adapter (C-48), for SwiftPM's tests, XCTest and Swift Testing: a
//! unit is a test target, named by its directory from the repository root, and a test is its
//! XCTest `Class/method` or its Swift Testing `Suite/func(labels:)`, as `swift test list` prints
//! them after the target.
//!
//! - **Loading.** `swift` and `swift-test` on COMMAND's `PATH` are hushrun ([`super::program`]).
//!   Every start that isn't a `swift test` that runs tests passes through untouched; so does
//!   anything the real `swift` starts, which gets `PATH` as COMMAND had it.
//! - **Before COMMAND,** the plan reads what each test target read from `swift package describe`,
//!   followed into the packages it depends on by path, and `dump-package` for the settings
//!   `describe` leaves out ([`Graph`]). Nothing is listed before the run.
//! - **In COMMAND,** hushrun's `swift` adds a `--skip` for each class, suite or target the plan
//!   left out ([`skip_patterns`]), a Swift Testing event stream of its own, and with `--parallel`
//!   an xunit file of its own; copies standard output as it goes by ([`program::run_copying_stdout`]);
//!   lists the tests after the run with `--skip-build`; and writes what each test did, read from
//!   the event stream, the console or the xunit file ([`read_events`], [`read_console`],
//!   [`read_xunit`]).
//! - **Nothing is left out** where SwiftPM would run less than it was told: on macOS when the
//!   package has a nested XCTest class, and where the XCTest tests that remain may not fit in one
//!   argument. After the run, this run's list says whether either hit, and then `swift test` runs
//!   again with nothing left out ([`run_again_whole`]): that is the guard. The previous run's
//!   list, which the map keeps as its list of what the runner listed, only saves that second run
//!   ([`leaves_nothing_out`]); with none, the plan leaves out as it plans.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use hushrun_core::Digest;
use hushrun_core::units::{self, Facts, NotFollowed, Sets, Sources, Tree};

use super::Seen;
use super::adapter::{
    Adapter, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::command::{self, program_name};
use super::facts::{self, FactsFile, RunFile, TestFacts, UnitFacts};
use super::program::{self, Call, Copied, Program};
use super::store::Map;

/// The adapter's word, in a facts file and the store.
pub const ADAPTER: &str = "swift";
/// What SwiftPM compiles, by suffix: the sources, of the files below a target's directory (C-30).
pub const KINDS: &[&str] = &[
    ".swift", ".c", ".cc", ".cpp", ".cxx", ".c++", ".m", ".mm", ".h", ".hh", ".hpp", ".hxx",
    ".h++", ".inc", ".ipp", ".s", ".S",
];
/// The longest hushrun waits for `swift package` or `swift test list`.
const BUDGET: Duration = Duration::from_secs(120);
/// The longest one argument Linux takes (`MAX_ARG_STRLEN`), its NUL aside.
const ONE_ARGUMENT: usize = 128 * 1024 - 1;
/// On Linux, what the XCTest ids of the last list may come to before a plan leaves nothing out:
/// SwiftPM passes those that remain as one argument, and tests added since need room.
const LINUX_XCTEST_BYTES: usize = 96 * 1024;
/// Elsewhere, the same within the whole command line.
const OTHER_XCTEST_BYTES: usize = 512 * 1024;
/// The longest `--skip` value hushrun writes: a class with more tests gets more than one.
const MOST_PATTERN: usize = 64 * 1024;
/// The most bytes of command line and environment a `swift test` with hushrun's `--skip`s may
/// take: SwiftPM passes them again to each Swift Testing process.
const MOST_COMMAND_LINE: usize = 512 * 1024;
/// Why nothing was observed when hushrun's `swift` wrote nothing.
pub const NOT_CAUGHT: &str = "swift test wrote nothing for hushrun: no `swift test` reached hushrun's `swift` on PATH, which stands in front of the `swift` and `swift-test` a command finds there; start it as `swift test`, not with `xcrun swift test` or a path to swift";

/// \[C-48\] The `swift test` adapter for a run whose plan describes the package at `package`.
#[derive(Debug)]
pub struct Swift {
    package: PathBuf,
    /// The newest target map, by the package and the scope's files it was made for: the plan's
    /// serves the read after COMMAND, whose files are the same.
    graph: Mutex<Option<(Digest, Arc<Graph>)>>,
}

impl Swift {
    fn new(package: PathBuf) -> Swift {
        Swift {
            package,
            graph: Mutex::new(None),
        }
    }

    /// The target map of the package at `package` for the files `tree`, described once, by a
    /// `swift` started in `cwd` with \[C-32\] COMMAND's own `env NAME=value` assignments
    /// (`assigned`): a manifest may read the environment (`Context.environment`).
    fn graph(
        &self,
        package: &Path,
        root: &Path,
        tree: &Tree,
        cwd: &Path,
        assigned: &[(String, String)],
    ) -> Result<Arc<Graph>, String> {
        let key = Digest::of(
            format!("{}\0{}", package.display(), tree.digest(tree.paths()).hex()).as_bytes(),
        );
        if let Ok(cached) = self.graph.lock()
            && let Some((k, graph)) = cached.as_ref()
            && *k == key
        {
            return Ok(Arc::clone(graph));
        }
        let scratch =
            tempfile::tempdir().map_err(|err| format!("no temporary directory: {err}"))?;
        let swift = || command::tool("swift", cwd, assigned);
        let described = describe_all(&swift, package, scratch.path())?;
        let graph = Arc::new(graph_of(&described, root, tree));
        if let Ok(mut cached) = self.graph.lock() {
            *cached = Some((key, Arc::clone(&graph)));
        }
        Ok(graph)
    }
}

/// \[ADR 0086\] The adapter for `swift test …`, `swift-test …`, or, in a directory that holds a
/// `Package.swift`, a wrapper whose text starts `swift test` or `swift-test` and no other runner
/// ([`command::wrapper_starts`]); `None` for any other COMMAND.
pub fn recognized(command: &[String], cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    Some(recognize(command, cwd)?.map(|s| Box::new(s) as Box<dyn Adapter>))
}

/// What `command` in `cwd` is for the adapter: `None` when it isn't its; `Some(Err(why))` when it
/// is, in a form hushrun can't observe.
pub fn recognize(command: &[String], cwd: &Path) -> Option<Result<Swift, String>> {
    let words: Vec<&str> = command.iter().map(String::as_str).collect();
    let first = *words.first()?;
    let name = program_name(first);
    let name = &*name;
    let args: Option<Vec<&str>> = match name {
        "swift" if words.get(1) == Some(&"test") => Some(words[2..].to_vec()),
        "swift-test" => Some(words[1..].to_vec()),
        "xcrun"
            if words.windows(2).any(|w| w == ["swift", "test"])
                || words.contains(&"swift-test") =>
        {
            return Some(Err(
                "xcrun starts swift from the selected toolchain, never from PATH, where hushrun's swift is".into(),
            ));
        }
        _ => None,
    };
    if let Some(args) = args {
        if cfg!(windows) {
            return Some(Err(WINDOWS.into()));
        }
        if first.contains(['/', '\\']) {
            return Some(Err(
                "COMMAND starts swift by its path, and hushrun's swift is on PATH".into(),
            ));
        }
        let parsed = parse(&args);
        if let Some(why) = parsed.passthrough.or(parsed.refused) {
            return Some(Err(why));
        }
        let package = parsed
            .package_path
            .map_or_else(|| cwd.to_path_buf(), |p| cwd.join(p));
        return Some(Ok(Swift::new(real(&package))));
    }
    if cwd.join("Package.swift").is_file()
        && command::wrapper_starts(command, cwd, command::Runner::SwiftTest)
    {
        if cfg!(windows) {
            return Some(Err(WINDOWS.into()));
        }
        return Some(Ok(Swift::new(real(cwd))));
    }
    None
}

const WINDOWS: &str = "hushrun doesn't observe swift test on Windows";

/// `swift test`'s options that take a value (`swift test --help-hidden`, 6.4.2).
const VALUE_OPTS: &[&str] = &[
    "--package-path",
    "--cache-path",
    "--config-path",
    "--security-path",
    "--scratch-path",
    "--build-path",
    "--swift-sdks-path",
    "--experimental-swift-sdks-path",
    "--toolset",
    "--pkg-config-path",
    "--manifest-cache",
    "--netrc-file",
    "--resolver-fingerprint-checking",
    "--resolver-signing-entity-checking",
    "--default-registry-url",
    "-c",
    "--configuration",
    "-Xcc",
    "-Xswiftc",
    "-Xlinker",
    "-Xcxx",
    "-Xxcbuild",
    "-Xmanifest",
    "--triple",
    "--sdk",
    "--toolchain",
    "--swift-sdk",
    "--experimental-swift-sdk",
    "--sanitize",
    "-j",
    "--jobs",
    "--build-system",
    "-debug-info-format",
    "--explicit-target-dependency-import-check",
    "--experimental-codesize-profile-output-dir",
    "--experimental-test-entry-point-path",
    "--experimental-lto-mode",
    "--experimental-trace-events-file",
    "--traits",
    "--experimental-prebuilts-download-url",
    "--experimental-prebuilts-root-cert",
    "--test-product",
    "--configuration-path",
    "--event-stream-output-path",
    "--event-stream-version",
    "--attachments-path",
    "--num-workers",
    "--experimental-maximum-parallelization-width",
    "--maximum-repetitions",
    "--repeat-until",
    "-s",
    "--specifier",
    "--filter",
    "--skip",
    "--xunit-output",
    "--test-output",
];
/// Options after which `swift test` runs no test.
const PASSTHROUGH_FLAGS: &[&str] = &[
    "-h",
    "-help",
    "--help",
    "--help-hidden",
    "--version",
    "-l",
    "--list-tests",
    "--show-codecov-path",
    "--show-code-coverage-path",
    "--show-coverage-path",
    "--debugger",
];
/// Options that only choose or report tests: a listing after the run leaves them out.
const RUN_ONLY: &[&str] = &[
    "--filter",
    "--skip",
    "-s",
    "--specifier",
    "--xunit-output",
    "--num-workers",
    "--event-stream-output-path",
    "--event-stream-version",
    "--attachments-path",
    "--maximum-repetitions",
    "--repeat-until",
    "--test-output",
    "--experimental-maximum-parallelization-width",
    "--parallel",
    "--no-parallel",
    "--experimental-xunit-message-failure",
    "--skip-build",
];

/// \[C-48\] What `swift test`'s arguments (after `test`) say to hushrun.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    /// Why this start runs no test, or isn't one hushrun reads: it passes through untouched.
    pub passthrough: Option<String>,
    /// Why it runs tests in a way hushrun doesn't observe: it runs untouched, and the run says so.
    pub refused: Option<String>,
    pub parallel: bool,
    /// COMMAND chooses tests itself, with `--filter` or `--skip`.
    pub chooses: bool,
    pub xunit: Option<String>,
    pub event_stream: Option<String>,
    pub package_path: Option<String>,
    pub xctest: bool,
    pub swift_testing: bool,
    /// The arguments a listing after the run takes again.
    pub list_args: Vec<String>,
}

/// \[C-48\] Reads `swift test`'s arguments, those after `test`.
pub fn parse<S: AsRef<str>>(args: &[S]) -> Parsed {
    let mut out = Parsed {
        xctest: true,
        swift_testing: true,
        ..Parsed::default()
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_ref();
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if arg.starts_with("--") => (name, Some(value.to_string())),
            _ => (arg, None),
        };
        if PASSTHROUGH_FLAGS.contains(&name) {
            out.passthrough = Some(format!("swift test {name} runs no test"));
        }
        if !arg.starts_with('-') {
            out.passthrough = Some(if ["list", "last", "help"].contains(&arg) {
                format!("swift test {arg} runs no test")
            } else {
                format!("swift test's argument {arg} isn't one hushrun reads")
            });
            i += 1;
            continue;
        }
        let mut consumed = vec![arg.to_string()];
        let value = if VALUE_OPTS.contains(&name) && inline.is_none() {
            let Some(value) = args.get(i + 1) else {
                out.passthrough = Some(format!("swift test's {name} has no value"));
                break;
            };
            consumed.push(value.as_ref().to_string());
            i += 1;
            Some(value.as_ref().to_string())
        } else {
            inline
        };
        i += 1;
        match name {
            "--parallel" => out.parallel = true,
            "--no-parallel" => out.parallel = false,
            "--filter" | "-s" | "--specifier" | "--skip" => out.chooses = true,
            "--xunit-output" => out.xunit = value,
            "--event-stream-output-path" => out.event_stream = value,
            "--package-path" => out.package_path = value,
            "--disable-xctest" => out.xctest = false,
            "--enable-xctest" => out.xctest = true,
            "--disable-swift-testing" => out.swift_testing = false,
            "--enable-swift-testing" => out.swift_testing = true,
            "--enable-code-coverage" => {
                out.refused = Some(
                    "with --enable-code-coverage, hushrun's listing after the run could add to the coverage data".into(),
                );
            }
            // \[C-48\] It runs the test products as an earlier build left them, which may be
            // older than the files hushrun keys them by (a `swift build --build-tests` before,
            // and a change since): no pass of it is known to be of these files.
            "--skip-build" => {
                out.refused = Some(
                    "with --skip-build, swift test runs what an earlier build made, which may be older than the files hushrun would record it for".into(),
                );
            }
            "--maximum-repetitions" | "--repeat-until" => {
                out.refused = Some(format!(
                    "with {name}, swift test runs a test more than once"
                ));
            }
            _ => {}
        }
        if !RUN_ONLY.contains(&name) {
            out.list_args.extend(consumed);
        }
    }
    out
}

/// Escapes every regular expression metacharacter of `text`, as both of SwiftPM's matchers read
/// them: `/`, backticks, spaces and `:` need none.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Whether a listed Swift Testing id ends in its file, line and column (`/A.swift:2:2`), as
/// `swift test list` writes two tests that would share an id: such an id moves when a line does,
/// and its test is never left out.
pub fn has_location(id: &str) -> bool {
    let Some((_, last)) = id.rsplit_once('/') else {
        return false;
    };
    let mut parts = last.rsplitn(3, ':');
    let (Some(column), Some(line), Some(file)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(column) && digits(line) && file.ends_with(".swift")
}

/// The target of a listed id: what comes before its first `.`.
fn target_of(id: &str) -> &str {
    id.split_once('.').map_or(id, |(target, _)| target)
}

/// \[C-48\] The `--skip` values that leave out `targets` whole, by module name (`^Target\.`), and
/// `tests`, listed ids, one value per class or suite: `^Target\.Class/(?:m1|m2)(?:/|$)`, every
/// metacharacter escaped. XCTest is matched against `Target.Class/method`, so `$` ends it; Swift
/// Testing against the id and `/File.swift:L:C`, so `/` does. A test whose id has a location is
/// never left out.
pub fn skip_patterns(targets: &BTreeSet<String>, tests: &BTreeSet<String>) -> Vec<String> {
    let mut out: Vec<String> = targets
        .iter()
        .map(|t| format!("^{}\\.", escape(t)))
        .collect();
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in tests {
        if targets.contains(target_of(id)) || has_location(id) {
            continue;
        }
        let Some((target, rest)) = id.split_once('.') else {
            continue;
        };
        let (head, last) = match rest.rsplit_once('/') {
            Some((suite, last)) => (format!("{target}.{suite}/"), last),
            None => (format!("{target}."), rest),
        };
        groups.entry(escape(&head)).or_default().push(escape(last));
    }
    for (head, alternatives) in groups {
        let mut current: Vec<String> = Vec::new();
        let mut len = head.len() + 16;
        for alternative in alternatives {
            if !current.is_empty() && len + alternative.len() + 1 > MOST_PATTERN {
                out.push(format!("^{head}(?:{})(?:/|$)", current.join("|")));
                current.clear();
                len = head.len() + 16;
            }
            len += alternative.len() + 1;
            current.push(alternative);
        }
        if !current.is_empty() {
            out.push(format!("^{head}(?:{})(?:/|$)", current.join("|")));
        }
    }
    out
}

/// \[C-48\] What `swift test list` printed: each test's id with its target, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub tests: Vec<String>,
    set: BTreeSet<String>,
    /// Each XCTest `Class/method` without its target, to the listed ids that end so: what the
    /// console says on Linux.
    tails: BTreeMap<String, Vec<String>>,
}

impl Listing {
    /// Reads `swift test list`'s output: one id a line.
    pub fn parse(text: &str) -> Listing {
        let mut out = Listing::default();
        for line in text.lines() {
            let id = line.trim_end_matches('\r');
            if id.trim().is_empty() || !id.contains('.') || id.contains(' ') && !id.contains('`') {
                continue;
            }
            if !out.set.insert(id.to_string()) {
                continue;
            }
            out.tests.push(id.to_string());
            if xctest(id)
                && let Some((_, tail)) = id.split_once('.')
            {
                out.tails
                    .entry(tail.to_string())
                    .or_default()
                    .push(id.to_string());
            }
        }
        out
    }

    pub fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }
}

/// Whether a listed id is an XCTest one: Swift Testing's always hold `(`, XCTest's never do.
pub fn xctest(id: &str) -> bool {
    !id.contains('(')
}

/// Whether an XCTest id names a class nested in a type: `Target.Outer.InnerTests/testInner`.
pub fn nested(id: &str) -> bool {
    xctest(id)
        && id
            .split_once('/')
            .is_some_and(|(class, _)| class.matches('.').count() > 1)
}

/// How one test ended, as a source says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed(u64),
    Failed(u64),
    Skipped(u64),
    /// It started and nothing says it ended.
    Started,
}

/// The text of a terminal's output without its escape sequences.
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// \[C-48\] Demangles the Objective-C name of a nested Swift class as XCTest prints it on macOS:
/// `_TtCO9CartTests5Outer10InnerTests` is `CartTests.Outer.InnerTests`. `None` for any other form.
pub fn demangle(name: &str) -> Option<String> {
    let rest = name.strip_prefix("_Tt")?;
    let kinds = rest.bytes().take_while(|b| b"COV".contains(b)).count();
    if kinds == 0 {
        return None;
    }
    let mut rest = &rest[kinds..];
    let mut parts = Vec::new();
    for _ in 0..=kinds {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let n: usize = rest.get(..digits)?.parse().ok()?;
        let part = rest.get(digits..digits + n)?;
        parts.push(part);
        rest = &rest[digits + n..];
    }
    rest.is_empty().then(|| parts.join("."))
}

fn millis(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0).round() as u64
    } else {
        0
    }
}

/// \[C-48\] What a serial XCTest run printed on standard output, its only record: each listed
/// test that started, and how it ended. macOS prints `Test Case '-[Target.Class method]' passed
/// (0.001 seconds).`, a nested class by its mangled name; Linux `Test Case 'Class.method' passed
/// (0.0 seconds)`, with no target, which names a listed test only when one target has it. A pass
/// or a skip counts only after the test's `started` line.
pub fn read_console(bytes: &[u8], listing: &Listing) -> BTreeMap<String, Outcome> {
    let mut out = BTreeMap::new();
    let mut open: BTreeSet<String> = BTreeSet::new();
    let text = plain(bytes);
    for line in text.split(['\n', '\r']) {
        let mut from = 0;
        while let Some(at) = line[from..].find("Test Case '") {
            let start = from + at + "Test Case '".len();
            from = start;
            let after = &line[start..];
            let Some(end) = after.find("' ") else {
                continue;
            };
            let name = &after[..end];
            let rest = &after[end + 2..];
            let verb = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('.');
            let Some(id) = console_id(name, listing) else {
                continue;
            };
            let seconds = rest
                .split_once('(')
                .and_then(|(_, r)| r.split_once(" seconds"))
                .and_then(|(n, _)| n.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            // A pass or a skip counts only as the end of a start this console said: a line
            // printed for a test that never started (by a test that prints XCTest's words, or a
            // sibling after a crash) is no pass. A failure counts whatever comes before it.
            let outcome = match verb {
                "started" => {
                    open.insert(id.clone());
                    Outcome::Started
                }
                "passed" if open.remove(&id) => Outcome::Passed(millis(seconds)),
                "skipped" if open.remove(&id) => Outcome::Skipped(millis(seconds)),
                "failed" => {
                    open.remove(&id);
                    Outcome::Failed(millis(seconds))
                }
                _ => continue,
            };
            out.insert(id, outcome);
        }
    }
    out
}

/// The listed id the console's name for a test is.
fn console_id(name: &str, listing: &Listing) -> Option<String> {
    let (class, method) = match name.strip_prefix("-[").and_then(|n| n.strip_suffix(']')) {
        Some(inner) => inner.split_once(' ')?,
        None => name.rsplit_once('.')?,
    };
    let class = match class.starts_with("_Tt") {
        true => demangle(class)?,
        false => class.to_string(),
    };
    let id = format!("{class}/{method}");
    if listing.contains(&id) {
        return Some(id);
    }
    match listing.tails.get(&id).map(Vec::as_slice) {
        Some([only]) => Some(only.clone()),
        _ => None,
    }
}

/// \[C-48\] What a Swift Testing event stream (`--event-stream-version 0`, JSON lines) says.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Events {
    /// Each listed test it says something of.
    pub outcomes: BTreeMap<String, Outcome>,
    pub runs_started: usize,
    pub runs_ended: usize,
}

/// \[C-48\] Reads an event stream: a test passed only with a start, an end and no issue that is an
/// error and not known; a skip is a skipped test; a start without an end, a test that didn't end.
/// An event's id is the listed id and `/File.swift:line:column`; the listed id is taken when the
/// list holds it, else the event's own when the list holds that; any other is left out. A line
/// that doesn't parse, a torn last one, is passed over.
pub fn read_events(bytes: &[u8], listing: &Listing) -> Events {
    let mut functions: BTreeMap<String, String> = BTreeMap::new();
    let mut started: BTreeMap<String, f64> = BTreeMap::new();
    let mut ended: BTreeMap<String, f64> = BTreeMap::new();
    let mut skipped: BTreeSet<String> = BTreeSet::new();
    let mut issues: BTreeSet<String> = BTreeSet::new();
    let mut out = Events::default();
    for line in bytes.split(|b| *b == b'\n') {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if record
            .get("version")
            .and_then(Value::as_u64)
            .is_some_and(|v| v != 0)
        {
            continue;
        }
        let payload = &record["payload"];
        match record["kind"].as_str() {
            Some("test") if payload["kind"] == "function" => {
                let Some(id) = payload["id"].as_str() else {
                    continue;
                };
                let location = &payload["sourceLocation"];
                let file = location["fileID"].as_str().unwrap_or("");
                let file = file.rsplit('/').next().unwrap_or(file);
                let suffix = format!("/{file}:{}:{}", location["line"], location["column"]);
                let short = id.strip_suffix(&suffix).unwrap_or(id);
                let listed = if listing.contains(short) {
                    short
                } else if listing.contains(id) {
                    id
                } else {
                    continue;
                };
                functions.insert(id.to_string(), listed.to_string());
            }
            Some("event") => {
                let id = payload["testID"].as_str().unwrap_or("").to_string();
                let at = payload["instant"]["absolute"].as_f64().unwrap_or(0.0);
                match payload["kind"].as_str() {
                    Some("runStarted") => out.runs_started += 1,
                    Some("runEnded") => out.runs_ended += 1,
                    Some("testStarted") => {
                        started.insert(id, at);
                    }
                    Some("testEnded") => {
                        ended.insert(id, at);
                    }
                    Some("testSkipped") => {
                        skipped.insert(id);
                    }
                    Some("issueRecorded") => {
                        let issue = &payload["issue"];
                        let known = issue["isKnown"].as_bool().unwrap_or(false);
                        let error = issue["severity"].as_str().is_none_or(|s| s == "error");
                        if !known && error {
                            issues.insert(id);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    for (event_id, listed) in functions {
        let outcome = if skipped.contains(&event_id) {
            Outcome::Skipped(0)
        } else if let (Some(start), Some(end)) = (started.get(&event_id), ended.get(&event_id)) {
            let ms = millis(end - start);
            if issues.contains(&event_id) {
                Outcome::Failed(ms)
            } else {
                Outcome::Passed(ms)
            }
        } else if started.contains_key(&event_id) {
            Outcome::Started
        } else {
            continue;
        };
        out.outcomes.insert(listed, outcome);
    }
    out
}

/// \[C-48\] Reads the xunit file SwiftPM writes for XCTest under `--parallel`: each `testcase`'s
/// `classname/name`, failed with a `failure` or an `error`, skipped with a `skipped`, else passed
/// (an XCTSkip reads as passed there). Only listed XCTest ids.
pub fn read_xunit(bytes: &[u8], listing: &Listing) -> BTreeMap<String, Outcome> {
    use quick_xml::Reader;
    use quick_xml::events::{BytesStart, Event};
    fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
        e.attributes()
            .with_checks(false)
            .flatten()
            .find(|a| a.key.local_name().as_ref() == name)
            .map(|a| a.value.into_owned())
    }
    let mut out = BTreeMap::new();
    let mut reader = Reader::from_reader(bytes);
    let mut buf = Vec::new();
    let mut current: Option<(String, u64, bool, bool)> = None;
    let mut finish = |case: Option<(String, u64, bool, bool)>| {
        if let Some((id, ms, failed, skipped)) = case
            && xctest(&id)
            && listing.contains(&id)
        {
            let outcome = if failed {
                Outcome::Failed(ms)
            } else if skipped {
                Outcome::Skipped(ms)
            } else {
                Outcome::Passed(ms)
            };
            out.insert(id, outcome);
        }
    };
    loop {
        let event = match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(event) => event,
        };
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "testcase" => {
                let case = attr(&e, "classname").zip(attr(&e, "name")).map(|(c, n)| {
                    let seconds = attr(&e, "time").and_then(|t| t.parse().ok()).unwrap_or(0.0);
                    (format!("{c}/{n}"), millis(seconds), false, false)
                });
                if empty {
                    finish(case);
                } else {
                    current = case;
                }
            }
            Event::Start(e) | Event::Empty(e) => {
                if let Some(case) = current.as_mut() {
                    match e.local_name().as_ref() {
                        "failure" | "error" => case.2 = true,
                        "skipped" => case.3 = true,
                        _ => {}
                    }
                }
            }
            Event::End(e) if e.local_name().as_ref() == "testcase" => finish(current.take()),
            _ => {}
        }
        buf.clear();
    }
    out
}

/// One package as `swift package describe` and `dump-package` say it.
#[derive(Debug, Clone)]
pub struct Described {
    pub dir: PathBuf,
    pub describe: Value,
    /// `None` when `dump-package` failed.
    pub dump: Option<Value>,
}

/// `swift package --package-path <dir> --scratch-path <scratch> <args>`: its JSON. The scratch
/// path keeps it from writing `.build` into the package; neither resolves nor builds.
fn swift_package(
    swift: &dyn Fn() -> Command,
    dir: &Path,
    scratch: &Path,
    args: &[&str],
) -> Result<Value, String> {
    let mut command = swift();
    command
        .arg("package")
        .arg("--package-path")
        .arg(dir)
        .arg("--scratch-path")
        .arg(scratch)
        .args(args);
    let stdout = output_within(command).map_err(|why| {
        format!(
            "swift package {} failed for {}: {why}",
            args[0],
            dir.display()
        )
    })?;
    let text = String::from_utf8_lossy(&stdout);
    let json = text.find('{').map_or("", |at| &text[at..]);
    serde_json::from_str(json)
        .map_err(|err| format!("swift package {} for {}: {err}", args[0], dir.display()))
}

/// A command's standard output, within [`BUDGET`], with no input and its errors dropped; `Err` why
/// it gave none.
fn output_within(mut command: Command) -> Result<Vec<u8>, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|err| err.to_string())?;
    let mut stdout = child.stdout.take().ok_or("no output")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > BUDGET => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("it took too long".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(err) => return Err(err.to_string()),
        }
    };
    let bytes = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("it exited {}", status.code().unwrap_or(-1)));
    }
    Ok(bytes)
}

/// Describes the package at `package` and every package it depends on by path, each with its
/// `describe` and `dump-package`, both at once.
pub fn describe_all(
    swift: &(dyn Fn() -> Command + Sync),
    package: &Path,
    scratch: &Path,
) -> Result<Vec<Described>, String> {
    let mut out: Vec<Described> = Vec::new();
    let mut queue = vec![real(package)];
    let mut seen = BTreeSet::new();
    while let Some(dir) = queue.pop() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        let n = seen.len();
        let (describe, dump) = std::thread::scope(|s| {
            let dump = s.spawn(|| {
                swift_package(
                    swift,
                    &dir,
                    &scratch.join(format!("d{n}")),
                    &["dump-package"],
                )
            });
            let describe = swift_package(
                swift,
                &dir,
                &scratch.join(format!("s{n}")),
                &["describe", "--type", "json"],
            );
            (describe, dump.join().ok().and_then(Result::ok))
        });
        let describe = describe?;
        for dep in describe["dependencies"].as_array().into_iter().flatten() {
            if dep["type"] == "fileSystem"
                && let Some(path) = dep["path"].as_str()
            {
                queue.push(real(Path::new(path)));
            }
        }
        out.push(Described {
            dir: describe["path"]
                .as_str()
                .map_or(dir.clone(), |p| real(Path::new(p))),
            describe,
            dump,
        });
    }
    // The root package first.
    if let Some(at) = out.iter().position(|d| d.dir == real(package)) {
        out.swap(0, at);
    }
    Ok(out)
}

/// `path` as the file system has it, or written out without `.` and `..` when it isn't there.
fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path))
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path` from `root`, `/`-separated, when it is inside it.
fn relative(path: &Path, root: &Path) -> Option<String> {
    let inside = real(path).strip_prefix(root).ok()?.to_path_buf();
    let parts: Vec<String> = inside
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// Every file of `tree` below `dir` (`""` for all).
fn below<'t>(tree: &'t Tree, dir: &str) -> impl Iterator<Item = &'t str> {
    let prefix = format!("{dir}/");
    let all = dir.is_empty();
    tree.paths().filter(move |p| all || p.starts_with(&prefix))
}

fn of_kind(path: &str) -> bool {
    KINDS.iter().any(|k| path.ends_with(k))
}

/// One test target of the root package: what it read and what can't be followed.
#[derive(Debug, Clone, Default)]
pub struct TestTarget {
    /// Its module name, which `swift test list` writes before its tests.
    pub c99: String,
    /// Every file of the scope below its directory and those of the targets it depends on.
    pub reads: BTreeSet<String>,
    /// Those directories, from the root.
    pub dirs: BTreeSet<String>,
    /// Its own sources.
    pub own: Vec<String>,
    pub not_followed: BTreeSet<NotFollowed>,
}

/// \[C-48\] What `describe` and `dump-package` say of a package and those it depends on by path,
/// for the scope's files.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    /// The root package's test targets, by their directories from the repository root.
    pub tests: BTreeMap<String, TestTarget>,
    /// Their directories, by module name.
    pub by_c99: BTreeMap<String, String>,
    /// The sources (C-30): every file of a kind SwiftPM compiles below a target's directory (a
    /// binary or system library target aside) that a test target reads.
    pub named: BTreeSet<String>,
    /// Which target's directory depends on which, for `hushrun why`.
    pub edges: BTreeSet<super::db::Edge>,
}

impl Graph {
    /// Whether `path` is a unit file: of a kind SwiftPM compiles, below a test target's directory.
    fn is_unit(&self, path: &str) -> bool {
        of_kind(path)
            && self
                .tests
                .keys()
                .any(|dir| dir.is_empty() || path.starts_with(&format!("{dir}/")))
    }

    fn merge(&mut self, other: &Graph) {
        self.tests
            .extend(other.tests.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.by_c99
            .extend(other.by_c99.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.named.extend(other.named.iter().cloned());
        self.edges.extend(other.edges.iter().cloned());
    }
}

/// Whether a flag of `unsafeFlags` may name a file or a directory a compile reads.
fn names_a_path(flag: &str) -> bool {
    const PATH_FLAGS: &[&str] = &[
        "-I",
        "-F",
        "-L",
        "-isystem",
        "-iquote",
        "-idirafter",
        "-include",
        "-imacros",
        "-fmodule-map-file",
        "-vfsoverlay",
        "-ivfsoverlay",
        "-import-objc-header",
        "-load-plugin",
        "-plugin-path",
        "-external-plugin-path",
        "-module-map",
        "-fplugin",
        "-sdk",
        "-resource-dir",
        "--sysroot",
        "-isysroot",
    ];
    flag.contains(['/', '\\'])
        || flag.starts_with('@')
        || PATH_FLAGS.iter().any(|p| flag.starts_with(p))
}

/// \[C-48\] The target map: for each test target of the root package (`described[0]`), every
/// directory it reads, followed through the targets it depends on in its package, the products of
/// packages it depends on by path, the plugins it is built with (whose package it reads whole),
/// and the header search paths of each; with what can't be followed, which makes it `not
/// observed`.
pub fn graph_of(described: &[Described], root: &Path, tree: &Tree) -> Graph {
    let root = real(root);
    let mut out = Graph::default();
    let Some(first) = described.first() else {
        return out;
    };
    let targets = |i: usize| -> &[Value] {
        described[i].describe["targets"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
    };
    let find = |i: usize, name: &str| targets(i).iter().find(|t| t["name"] == name);
    let settings = |i: usize, name: &str| -> Option<Vec<Value>> {
        let dump = described[i].dump.as_ref()?;
        let target = dump["targets"]
            .as_array()?
            .iter()
            .find(|t| t["name"] == name)?;
        Some(target["settings"].as_array().cloned().unwrap_or_default())
    };
    // \[C-48\] Whether `dump-package` says a target is built with a plugin, of its package or
    // another's: `pluginUsages`, which `describe` leaves out for another package's.
    let uses_plugin = |i: usize, name: &str| -> bool {
        described[i].dump.as_ref().is_some_and(|dump| {
            dump["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| t["name"] == name)
                .any(|t| t["pluginUsages"].as_array().is_some_and(|u| !u.is_empty()))
        })
    };
    // Every file of a kind SwiftPM compiles below a target's directory: what the graph can give a
    // test target.
    let mut compiled: BTreeSet<String> = BTreeSet::new();
    let mut products: BTreeMap<String, Vec<(usize, Vec<String>)>> = BTreeMap::new();
    let mut plugin_products: BTreeSet<String> = BTreeSet::new();
    for (i, d) in described.iter().enumerate() {
        for p in d.describe["products"].as_array().into_iter().flatten() {
            if let Some(name) = p["name"].as_str()
                && p["type"].get("plugin").is_some()
            {
                plugin_products.insert(name.to_string());
            }
            let names = strings(&p["targets"]);
            if let Some(name) = p["name"].as_str() {
                products
                    .entry(name.to_string())
                    .or_default()
                    .push((i, names));
            }
        }
        for t in targets(i) {
            let kind = t["type"].as_str().unwrap_or("");
            let Some(path) = t["path"].as_str() else {
                continue;
            };
            let dir = d.dir.join(path);
            let Some(from) = relative(&dir, &root) else {
                continue;
            };
            if kind != "binary" && kind != "system-target" {
                compiled.extend(
                    below(tree, &from)
                        .filter(|p| of_kind(p))
                        .map(str::to_string),
                );
            }
            for dep in strings(&t["target_dependencies"]) {
                if let Some(to) = find(i, &dep)
                    .and_then(|t| t["path"].as_str())
                    .and_then(|p| relative(&d.dir.join(p), &root))
                    && to != from
                {
                    out.edges
                        .insert((from.clone(), to, super::db::EdgeKind::Import));
                }
            }
        }
    }
    let resolved =
        relative(&first.dir.join("Package.resolved"), &root).is_some_and(|p| tree.contains(&p));
    for t in targets(0).iter().filter(|t| t["type"] == "test") {
        let (Some(name), Some(path)) = (t["name"].as_str(), t["path"].as_str()) else {
            continue;
        };
        let Some(unit) = relative(&first.dir.join(path), &root) else {
            continue;
        };
        let c99 = t["c99name"].as_str().unwrap_or(name).to_string();
        let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut not_followed: BTreeSet<NotFollowed> = BTreeSet::new();
        let mut seen: BTreeSet<(usize, String)> = BTreeSet::new();
        let mut stack = vec![(0usize, name.to_string())];
        while let Some((i, name)) = stack.pop() {
            if !seen.insert((i, name.clone())) {
                continue;
            }
            let pkg = &described[i];
            let Some(t) = find(i, &name) else {
                not_followed.insert(NotFollowed {
                    why: "describe".into(),
                    detail: format!("{name} isn't described"),
                });
                continue;
            };
            let kind = t["type"].as_str().unwrap_or("");
            let path = t["path"].as_str().unwrap_or("");
            // A remote binary target is pinned by its URL and checksum in the manifest, which
            // is in the rest set.
            if kind == "binary" && path.starts_with("remote/") {
                continue;
            }
            let dir = pkg.dir.join(path);
            dirs.insert(normalize(&dir));
            match settings(i, &name) {
                Some(settings) => {
                    for s in settings {
                        if let Some(p) = s["kind"]["headerSearchPath"]["_0"].as_str() {
                            dirs.insert(normalize(&dir.join(p)));
                        }
                        if strings(&s["kind"]["unsafeFlags"]["_0"])
                            .iter()
                            .any(|f| names_a_path(f))
                        {
                            not_followed.insert(NotFollowed {
                                why: "unsafe-flags".into(),
                                detail: name.clone(),
                            });
                        }
                    }
                }
                None => {
                    not_followed.insert(NotFollowed {
                        why: "describe".into(),
                        detail: format!(
                            "swift package dump-package failed for {}",
                            pkg.dir.display()
                        ),
                    });
                }
            }
            // A build-tool plugin may read any file of the package of the target it builds,
            // whichever package the plugin is of.
            if uses_plugin(i, &name)
                || strings(&t["product_dependencies"])
                    .iter()
                    .any(|p| plugin_products.contains(p))
            {
                dirs.insert(pkg.dir.clone());
            }
            for dep in strings(&t["target_dependencies"]) {
                if find(i, &dep).is_some_and(|d| d["type"] == "plugin") {
                    dirs.insert(pkg.dir.clone());
                }
                stack.push((i, dep));
            }
            for product in strings(&t["product_dependencies"]) {
                let local: Vec<&(usize, Vec<String>)> = products
                    .get(&product)
                    .into_iter()
                    .flatten()
                    .filter(|(j, _)| *j != i)
                    .collect();
                if local.is_empty() && !resolved {
                    not_followed.insert(NotFollowed {
                        why: "unpinned".into(),
                        detail: product.clone(),
                    });
                }
                for (j, names) in local {
                    stack.extend(names.iter().map(|n| (*j, n.clone())));
                }
            }
        }
        let mut reads = BTreeSet::new();
        let mut rel_dirs = BTreeSet::new();
        for dir in &dirs {
            let Some(rel) = relative(dir, &root) else {
                not_followed.insert(NotFollowed {
                    why: "outside-scope".into(),
                    detail: dir.display().to_string(),
                });
                continue;
            };
            let before = reads.len();
            reads.extend(below(tree, &rel).map(str::to_string));
            if reads.len() == before && !below(tree, &rel).any(|_| true) {
                not_followed.insert(NotFollowed {
                    why: "outside-scope".into(),
                    detail: rel.clone(),
                });
            }
            rel_dirs.insert(rel);
        }
        let own: Vec<String> = below(tree, &unit)
            .filter(|p| of_kind(p))
            .map(str::to_string)
            .collect();
        // \[C-30\] Sources are the files the graph gives a test target: of a compiled kind, below a
        // target's directory, and read by one. Any other file of a compiled kind (a target no test
        // target depends on, an `Examples/` or `Fixtures/` tree, a package this one doesn't
        // depend on) is in the rest set, so a change to it runs every test target.
        out.named
            .extend(reads.iter().filter(|p| compiled.contains(*p)).cloned());
        out.by_c99.insert(c99.clone(), unit.clone());
        out.tests.insert(
            unit,
            TestTarget {
                c99,
                reads,
                dirs: rel_dirs,
                own,
                not_followed,
            },
        );
    }
    out
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// \[C-48\] Why a plan leaves nothing out, from the last list this machine kept (`map`'s list of
/// what the runner listed, `<unit>::<id>`): on macOS, a nested XCTest class, which SwiftPM
/// silently doesn't run once any `--skip` is given; or XCTest ids that may not fit the one argument
/// SwiftPM passes those that remain in. With no list kept the plan leaves out as it plans:
/// hushrun's `swift` checks this run's own list after it and runs `swift test` again with nothing
/// left out when either fault hit ([`run_again_whole`]), as it does for a class added since the
/// list; a kept list that says one would hit only saves that second run.
pub fn leaves_nothing_out(map: &Map, graph: &Graph) -> Result<(), String> {
    let ids: Vec<(&str, &str)> = map
        .unit_files
        .iter()
        .filter_map(|name| name.split_once(units::TEST_SEPARATOR))
        .filter(|(_, id)| xctest(id))
        .collect();
    if cfg!(target_os = "macos")
        && let Some((unit, id)) = ids.iter().find(|(_, id)| nested(&format!("T.{id}")))
    {
        let class = id.split_once('/').map_or(*id, |(c, _)| c);
        return Err(format!(
            "on macOS SwiftPM doesn't run a nested XCTest class ({class}, in {unit}) once any test is left out, so none is"
        ));
    }
    let bytes: usize = ids
        .iter()
        .map(|(unit, id)| {
            let target = graph.tests.get(*unit).map_or(unit.len(), |t| t.c99.len());
            target + 1 + id.len() + 1
        })
        .sum();
    let most = if cfg!(target_os = "linux") {
        LINUX_XCTEST_BYTES
    } else {
        OTHER_XCTEST_BYTES
    };
    if bytes > most {
        return Err(format!(
            "SwiftPM passes the XCTest tests that remain as one argument, and its {} XCTest tests might not fit ({bytes} bytes, more than {most}), so none is left out",
            ids.len()
        ));
    }
    Ok(())
}

impl Adapter for Swift {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "swift test"
    }

    fn test_file(&self) -> &'static str {
        "test target"
    }

    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            reads: false,
        }
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let path = (at.env)("PATH");
        let search = path.clone().unwrap_or_default();
        // Only names COMMAND finds: a script that asks for `swift-test` gets the answer it got.
        let names: Vec<&str> = ["swift", "swift-test"]
            .into_iter()
            .filter(|name| program::real(name, &search).is_some())
            .collect();
        if names.is_empty() {
            return Err("there is no swift on COMMAND's PATH".into());
        }
        let dir = program::make(at.dir, &at.exe()?, &names)
            .map_err(|err| format!("can't make hushrun's swift: {err}"))?;
        program::variables(&dir, path)
    }

    fn plan(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let graph = self.graph(&self.package, at.root, at.tree, at.cwd, at.assigned)?;
        leaves_nothing_out(at.map, &graph)?;
        let sets = units::classify(
            at.tree.paths(),
            &|path| graph.is_unit(path),
            Sources::Named(&graph.named, KINDS),
            at.ignore,
        );
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let known = graph
            .tests
            .iter()
            .map(|(unit, t)| {
                let whole = t.not_followed.is_empty()
                    && !t.own.is_empty()
                    && t.own.iter().all(|f| at.tree.contains(f));
                let lists = whole
                    .then(|| {
                        super::shared::lists(
                            unit,
                            at.map.units.get(unit),
                            &shared,
                            at.tree,
                            Some(&t.reads),
                        )
                    })
                    .flatten();
                (unit.clone(), lists)
            })
            .collect();
        Ok(Some(Planning {
            sets,
            known,
            received: shared.received,
            ..Planning::default()
        }))
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        let files: Vec<&FactsFile> = at.files.iter().filter(|f| f.adapter == ADAPTER).collect();
        if files.is_empty() {
            return Err(NOT_CAUGHT.into());
        }
        if let Some(why) = files.iter().find_map(|f| f.errors.first()) {
            return Err(why.clone());
        }
        let mut graph = Graph::default();
        let mut graphs: BTreeMap<PathBuf, Arc<Graph>> = BTreeMap::new();
        let mut translated: Vec<FactsFile> = Vec::new();
        let mut unit_files = BTreeSet::new();
        let mut refused = None;
        for file in &files {
            let package = file
                .settings
                .get("package")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .ok_or("hushrun's swift didn't say which package it tested")?;
            let this = match graphs.get(&package) {
                Some(g) => Arc::clone(g),
                None => {
                    let g = self.graph(&package, at.root, at.tree, at.cwd, at.assigned)?;
                    graphs.insert(package.clone(), Arc::clone(&g));
                    graph.merge(&g);
                    g
                }
            };
            let mut file = (*file).clone();
            for unit in &mut file.units {
                let Some(dir) = this.by_c99.get(&unit.unit) else {
                    return Err(format!(
                        "swift test ran the tests of {}, which swift package describe doesn't name as a test target of the repository",
                        unit.unit
                    ));
                };
                unit.unit = dir.clone();
            }
            for id in file.settings.get("tests").map(strings).unwrap_or_default() {
                if let Some((c99, rest)) = id.split_once('.')
                    && let Some(dir) = this.by_c99.get(c99)
                {
                    unit_files.insert(units::test_name(dir, rest));
                }
            }
            if refused.is_none() {
                refused = file
                    .settings
                    .get("refused")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            translated.push(file);
        }
        let merged = facts::merged(&translated, ADAPTER);
        let failures_by_unit = files.iter().all(|f| f.failures_by_unit);
        let mut seen = BTreeMap::new();
        let mut own = BTreeMap::new();
        let mut dirs = BTreeMap::new();
        for (unit, m) in merged {
            let Some(target) = graph.tests.get(&unit) else {
                continue;
            };
            let complete = match &m.tests {
                Some(tests) => tests.values().filter(|t| !t.deselected).all(|t| t.finished),
                None => m.complete,
            };
            let tests = m.tests.map(|tests| {
                tests
                    .into_iter()
                    .filter(|(id, _)| !has_location(id))
                    .collect()
            });
            let mut not_followed = m.not_followed;
            not_followed.extend(target.not_followed.iter().cloned());
            own.insert(unit.clone(), target.own.clone());
            dirs.insert(unit.clone(), target.dirs.clone());
            seen.insert(
                unit,
                Seen {
                    facts: Facts {
                        complete,
                        failed: m.failed,
                        not_followed,
                        deselected: m.deselected,
                        ..Facts::default()
                    },
                    reads: target.reads.clone(),
                    duration_ms: m.duration_ms,
                    tests,
                },
            );
        }
        let sets = units::classify(
            at.tree.paths(),
            &|path| graph.is_unit(path),
            Sources::Named(&graph.named, KINDS),
            at.ignore,
        );
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = seen
            .keys()
            .filter_map(|unit| {
                let reads = &graph.tests.get(unit)?.reads;
                let lists = super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    Some(reads),
                )?;
                Some((unit.clone(), lists))
            })
            .collect();
        Ok(Some(Read {
            seen,
            failures_by_unit,
            sets,
            planned,
            own,
            dirs,
            edges: graph.edges.clone(),
            unit_files: (!unit_files.is_empty()).then_some(unit_files),
            refused,
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras::default()
    }

    fn why_unit(&self, asked: &str) -> String {
        asked.trim_end_matches('/').to_string()
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        units::classify(
            at.tree.paths(),
            &|p| {
                of_kind(p)
                    && at
                        .map
                        .units
                        .keys()
                        .any(|dir| p.starts_with(&format!("{dir}/")))
            },
            Sources::Named(&at.map.sources, KINDS),
            at.ignore,
        )
    }

    fn why_is_unit(&self, at: &WhyAt<'_>, _: &Sets, unit: &str) -> bool {
        at.map.units.contains_key(unit) || at.shared.knows(unit)
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!(
            "{unit} isn't a test target {check} ran as far as this machine knows: swift package describe didn't name it in its last run here"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some(
            "its test targets and what each reads are those swift package describe named in its last run here",
        )
    }

    fn why_asks_hints(&self, map: &Map) -> bool {
        !map.sources.is_empty()
    }

    fn why_units_are_directories(&self) -> bool {
        true
    }
}

/// \[ADR 0086\] hushrun's `swift` (and `swift-test`) on COMMAND's `PATH`.
#[derive(Debug)]
pub struct SwiftProgram(&'static str);

/// `swift`.
pub const SWIFT: SwiftProgram = SwiftProgram("swift");
/// `swift-test`, which Linux has on `PATH`.
pub const SWIFT_TEST: SwiftProgram = SwiftProgram("swift-test");

impl Program for SwiftProgram {
    fn name(&self) -> &'static str {
        self.0
    }

    fn args(&self, call: &Call<'_>) -> Vec<OsString> {
        call.args.to_vec()
    }

    fn run(&self, call: &Call<'_>) -> Option<ExitCode> {
        let units = call.units?;
        if cfg!(windows) {
            return None;
        }
        let args: Vec<String> = call
            .args
            .iter()
            .map(|a| a.to_str().map(str::to_string))
            .collect::<Option<_>>()?;
        let (prefix, test): (&[&str], &[String]) = if call.name == "swift" {
            if args.first().map(String::as_str) != Some("test") {
                return None;
            }
            (&["test"], &args[1..])
        } else {
            (&[], &args[..])
        };
        let parsed = parse(test);
        if parsed.passthrough.is_some() {
            return None;
        }
        if let Some(why) = parsed
            .refused
            .clone()
            .or_else(|| too_old(call.real, prefix))
        {
            write_facts(units, &refusal(why));
            return None;
        }
        Some(observe(call.real, prefix, test, &parsed, units))
    }
}

/// Why the real `swift` is one hushrun doesn't read: older than 6.0, or of no version it can tell.
fn too_old(real: &Path, prefix: &[&str]) -> Option<String> {
    let _ = prefix;
    let mut command = program::real_command(real);
    command.arg("--version");
    let text = String::from_utf8_lossy(&output_within(command).ok()?).into_owned();
    let major = swift_major(&text)?;
    (major < 6)
        .then(|| format!("swift test of Swift {major} is older than 6.0, which hushrun reads"))
}

/// The major version in `swift --version`'s output: `Apple Swift version 6.4.2`, `Swift version
/// 6.4.2 (swift-6.4.2-RELEASE)`, or `Swift Package Manager - Swift 6.4.2`.
pub fn swift_major(text: &str) -> Option<u32> {
    let at = text
        .find("Swift version ")
        .map(|i| i + "Swift version ".len())
        .or_else(|| text.find("- Swift ").map(|i| i + "- Swift ".len()))?;
    let digits: String = text[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn refusal(why: String) -> FactsFile {
    FactsFile {
        schema: facts::FACTS_SCHEMA_V1.into(),
        adapter: ADAPTER.into(),
        errors: vec![why],
        ..FactsFile::default()
    }
}

fn write_facts(units: &Path, file: &FactsFile) {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = units.join(format!("swift-{}-{nanos}.json", std::process::id()));
    // \[C-32\] Each hushrun's swift is a start of its own, and writes one file.
    let file = FactsFile {
        start: Some(format!("swift-{}-{nanos}", std::process::id())),
        ..file.clone()
    };
    if let Ok(bytes) = serde_json::to_vec(&file) {
        let _ = facts::write_whole(&path, &bytes);
    }
}

/// What hushrun's `swift` leaves out of one run.
#[derive(Debug, Default, Clone)]
struct LeftOut {
    patterns: Vec<String>,
    /// The targets left out whole, by module name.
    targets: BTreeSet<String>,
    /// The tests left out one by one, by listed id.
    tests: BTreeSet<String>,
    /// Why it leaves out nothing `hushrun.json` lists.
    refused: Option<String>,
}

impl LeftOut {
    fn refused(why: String) -> LeftOut {
        LeftOut {
            refused: Some(why),
            ..LeftOut::default()
        }
    }

    fn holds(&self, id: &str) -> bool {
        !self.patterns.is_empty()
            && (self.targets.contains(target_of(id)) || self.tests.contains(id))
    }
}

/// The test targets of the package `describe` says, by their directories from `root`, to their
/// module names.
fn test_targets(describe: &Value, package: &Path, root: &Path) -> BTreeMap<String, String> {
    let root = real(root);
    describe["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == "test")
        .filter_map(|t| {
            let dir = relative(&package.join(t["path"].as_str()?), &root)?;
            let c99 = t["c99name"].as_str().or(t["name"].as_str())?;
            Some((dir, c99.to_string()))
        })
        .collect()
}

/// What `run` lists that `swift test` may leave out, as `--skip` values; with how many bytes of
/// command line and environment the start already takes.
fn leave_out(
    run: &RunFile,
    real_swift: &Path,
    package: &Path,
    work: &Path,
    used: usize,
) -> LeftOut {
    if run.reuse.is_empty() && run.reuse_tests.is_empty() {
        return LeftOut::default();
    }
    let swift = || {
        let mut command = program::real_command(real_swift);
        command.env_remove(super::runner::UNITS_VAR);
        command
    };
    let describe = match swift_package(
        &swift,
        package,
        &work.join("describe"),
        &["describe", "--type", "json"],
    ) {
        Ok(describe) => describe,
        Err(why) => return LeftOut::refused(why),
    };
    let units = test_targets(&describe, package, &run.root);
    let targets: BTreeSet<String> = run
        .reuse
        .iter()
        .filter_map(|u| units.get(u))
        .cloned()
        .collect();
    let mut tests = BTreeSet::new();
    for (unit, ids) in &run.reuse_tests {
        if let Some(c99) = units.get(unit) {
            tests.extend(
                ids.iter()
                    .filter(|id| !has_location(id))
                    .map(|id| format!("{c99}.{id}")),
            );
        }
    }
    let patterns = skip_patterns(&targets, &tests);
    let added: usize = patterns.iter().map(|p| p.len() + "--skip".len() + 18).sum();
    if patterns.iter().any(|p| p.len() >= ONE_ARGUMENT) || used + added > MOST_COMMAND_LINE {
        return LeftOut::refused(format!(
            "leaving them out would make swift test's command line longer than the {MOST_COMMAND_LINE} bytes hushrun gives it"
        ));
    }
    LeftOut {
        patterns,
        targets,
        tests,
        refused: None,
    }
}

/// A file a run may write, and whether it was there before.
struct Fresh {
    path: PathBuf,
    before: Option<SystemTime>,
}

impl Fresh {
    fn new(path: PathBuf) -> Fresh {
        let before = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        Fresh { path, before }
    }

    /// Its bytes, when this run wrote it: it is there, changed, and not older than the run's
    /// start, by a clock's second.
    fn read(&self, started: SystemTime) -> Option<Vec<u8>> {
        let modified = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()?;
        let recent = match modified.duration_since(started) {
            Ok(_) => true,
            Err(err) => err.duration() < Duration::from_secs(1),
        };
        (recent && Some(modified) != self.before)
            .then(|| std::fs::read(&self.path).ok())
            .flatten()
    }
}

/// One start of the real `swift test`, with what hushrun adds, and where its results go.
struct Started {
    copied: Copied,
    events: Option<Fresh>,
    xunit: Option<Fresh>,
    at: SystemTime,
}

fn start(
    real_swift: &Path,
    prefix: &[&str],
    test: &[String],
    parsed: &Parsed,
    skips: &[String],
    work: &Path,
    n: u32,
) -> std::io::Result<Started> {
    let mut added: Vec<String> = Vec::new();
    for pattern in skips {
        added.push("--skip".into());
        added.push(pattern.clone());
    }
    let events = if !parsed.swift_testing {
        None
    } else if let Some(theirs) = &parsed.event_stream {
        Some(Fresh::new(PathBuf::from(theirs)))
    } else {
        let path = work.join(format!("events-{n}.jsonl"));
        added.extend([
            "--event-stream-output-path".to_string(),
            path.display().to_string(),
            "--event-stream-version".to_string(),
            "0".to_string(),
        ]);
        Some(Fresh::new(path))
    };
    let xunit = if !(parsed.parallel && parsed.xctest) {
        None
    } else if let Some(theirs) = &parsed.xunit {
        Some(Fresh::new(PathBuf::from(theirs)))
    } else {
        let path = work.join(format!("xunit-{n}.xml"));
        added.extend(["--xunit-output".to_string(), path.display().to_string()]);
        Some(Fresh::new(path))
    };
    let mut command = program::real_command(real_swift);
    command
        .args(prefix)
        .args(test)
        .args(&added)
        .env_remove(super::runner::UNITS_VAR);
    let at = SystemTime::now();
    let copied = program::run_copying_stdout(command)?;
    Ok(Started {
        copied,
        events,
        xunit,
        at,
    })
}

/// `swift test list --skip-build` with COMMAND's options, after the run.
fn list(real_swift: &Path, prefix: &[&str], parsed: &Parsed) -> Option<Listing> {
    let mut command = program::real_command(real_swift);
    command
        .args(prefix)
        .arg("list")
        .arg("--skip-build")
        .args(&parsed.list_args)
        .env_remove(super::runner::UNITS_VAR);
    let stdout = output_within(command).ok()?;
    Some(Listing::parse(&String::from_utf8_lossy(&stdout)))
}

/// \[C-48\] Why a run in which hushrun left tests out must run again whole, from the list after
/// it: on macOS, a nested XCTest class in a target it ran, which SwiftPM silently didn't run; on
/// Linux, a run that failed with no XCTest test started while the XCTest ids left came to more
/// than one argument takes.
fn run_again_whole(
    listing: &Listing,
    left: &LeftOut,
    run: &Copied,
    parsed: &Parsed,
) -> Option<String> {
    if left.patterns.is_empty() || !parsed.xctest {
        return None;
    }
    if cfg!(target_os = "macos")
        && let Some(id) = listing
            .tests
            .iter()
            .find(|id| nested(id) && !left.targets.contains(target_of(id)))
    {
        let class = id.split_once('/').map_or(id.as_str(), |(c, _)| c);
        return Some(format!(
            "on macOS SwiftPM doesn't run a nested XCTest class ({class}) once a test is left out"
        ));
    }
    if cfg!(target_os = "linux") && run.exit != Some(0) {
        let started = plain(&run.stdout).contains("Test Case '");
        let bytes: usize = listing
            .tests
            .iter()
            .filter(|id| xctest(id) && !left.holds(id))
            .map(|id| id.len() + 1)
            .sum();
        if !started && bytes >= ONE_ARGUMENT {
            return Some(format!(
                "SwiftPM passed the XCTest tests that remain as one argument of {bytes} bytes, more than Linux takes"
            ));
        }
    }
    None
}

/// The run of `swift test` hushrun's `swift` observes: what it leaves out, the run, the list
/// after it, and its facts file. Its exit is the real one's, whatever hushrun's part comes to.
fn observe(
    real_swift: &Path,
    prefix: &[&str],
    test: &[String],
    parsed: &Parsed,
    units: &Path,
) -> ExitCode {
    let work = units.join(format!("swift-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&work);
    let cwd = std::env::current_dir().unwrap_or_default();
    let package = real(
        &parsed
            .package_path
            .as_ref()
            .map_or(cwd.clone(), |p| cwd.join(p)),
    );
    let run_file: Option<RunFile> = std::fs::read(units.join(facts::RUN_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let used: usize = std::env::vars_os()
        .map(|(k, v)| k.len() + v.len() + 10)
        .chain(test.iter().map(|a| a.len() + 9))
        .sum();
    let mut left = run_file
        .as_ref()
        .map(|run| leave_out(run, real_swift, &package, &work, used))
        .unwrap_or_default();
    let mut started = match start(real_swift, prefix, test, parsed, &left.patterns, &work, 1) {
        Ok(started) => started,
        Err(err) => {
            eprintln!("error: can't start {}: {err}", real_swift.display());
            let _ = std::fs::remove_dir_all(&work);
            return ExitCode::from(if err.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            });
        }
    };
    let mut listing = list(real_swift, prefix, parsed);
    if let Some(found) = &listing
        && let Some(why) = run_again_whole(found, &left, &started.copied, parsed)
    {
        eprintln!("hushrun: swift test runs again with nothing left out: {why}");
        if let Ok(again) = start(real_swift, prefix, test, parsed, &[], &work, 2) {
            started = again;
            listing = list(real_swift, prefix, parsed);
            left = LeftOut::refused(why);
        }
    }
    let file = match &listing {
        Some(listing) => facts_of(listing, &started, &left, parsed, &package),
        None => {
            let mut file = refusal("swift test list didn't list the tests after the run".into());
            // \[C-32\] What SwiftPM ran of what hushrun left out can't be checked against the
            // list (a nested class, a test added since): the run starts COMMAND once more with
            // nothing left out.
            if !left.patterns.is_empty() {
                file.left_out_unlisted = Some(
                    "swift test list didn't list the tests after the run, so what swift test ran of those left out can't be checked".into(),
                );
            }
            file
        }
    };
    write_facts(units, &file);
    let _ = std::fs::remove_dir_all(&work);
    started.copied.exit_code()
}

/// \[C-48\] What a run did, as a facts file: each listed test of each target, from the event stream
/// for Swift Testing, the console for serial XCTest and the xunit file under `--parallel`.
fn facts_of(
    listing: &Listing,
    run: &Started,
    left: &LeftOut,
    parsed: &Parsed,
    package: &Path,
) -> FactsFile {
    let console = if parsed.xctest && !parsed.parallel {
        read_console(&run.copied.stdout, listing)
    } else {
        BTreeMap::new()
    };
    let events = run
        .events
        .as_ref()
        .and_then(|f| f.read(run.at))
        .map(|bytes| read_events(&bytes, listing));
    let xunit = run
        .xunit
        .as_ref()
        .and_then(|f| f.read(run.at))
        .map(|bytes| read_xunit(&bytes, listing));
    tested(
        listing,
        &console,
        events.as_ref(),
        xunit.as_ref(),
        left,
        parsed,
        run.copied.exit,
        package,
        run.copied.held,
    )
}

/// \[C-48\] The facts file of a run from what each source said (pure, for tests).
#[allow(clippy::too_many_arguments)]
fn tested(
    listing: &Listing,
    console: &BTreeMap<String, Outcome>,
    events: Option<&Events>,
    xunit: Option<&BTreeMap<String, Outcome>>,
    left: &LeftOut,
    parsed: &Parsed,
    exit: Option<i32>,
    package: &Path,
    held: bool,
) -> FactsFile {
    // On macOS, with any `--skip` or `--filter`, a nested XCTest class doesn't run, and the
    // parallel xunit says it passed: nothing of it counts.
    let trap = cfg!(target_os = "macos") && (parsed.chooses || !left.patterns.is_empty());
    let mut by_target: BTreeMap<String, Vec<TestFacts>> = BTreeMap::new();
    let mut any_failed = false;
    let mut in_flight = false;
    for id in &listing.tests {
        let Some((target, rest)) = id.split_once('.') else {
            continue;
        };
        let outcome = if xctest(id) {
            if !parsed.xctest || (trap && nested(id)) {
                None
            } else if parsed.parallel {
                xunit.and_then(|x| x.get(id))
            } else {
                console.get(id)
            }
        } else if parsed.swift_testing {
            events.and_then(|e| e.outcomes.get(id))
        } else {
            None
        };
        // \[C-32\] Under --parallel the xunit file reads an XCTSkip as a pass: in a run that
        // failed, where a skip records nothing, an XCTest pass there may be a skip.
        let may_skip = xctest(id) && parsed.parallel && exit != Some(0);
        let (finished, failed, duration_ms, skipped) = match outcome {
            Some(Outcome::Passed(ms)) => (true, false, *ms, may_skip),
            Some(Outcome::Skipped(ms)) => (true, false, *ms, true),
            Some(Outcome::Failed(ms)) => (true, true, *ms, false),
            // A start without an end, in a run that failed, is a crash.
            Some(Outcome::Started) => {
                in_flight |= !xctest(id);
                (false, exit != Some(0), 0, false)
            }
            None => (false, false, 0, false),
        };
        // \[C-48\] Output a program swift test started held past its end: what it printed can't
        // be read whole, and no pass of it is recorded. A failure seen is a failure.
        let finished = finished && (failed || !held);
        any_failed |= failed;
        by_target
            .entry(target.to_string())
            .or_default()
            .push(TestFacts {
                id: rest.to_string(),
                finished,
                failed,
                deselected: left.holds(id) && outcome.is_none(),
                duration_ms,
                skipped,
                ..TestFacts::default()
            });
    }
    let units = by_target
        .into_iter()
        .map(|(target, tests)| UnitFacts {
            unit: target,
            selected: tests.iter().filter(|t| !t.deselected).count() as u64,
            finished: tests.iter().filter(|t| t.finished).count() as u64,
            failed: tests.iter().any(|t| t.failed),
            deselected: !tests.is_empty() && tests.iter().all(|t| t.deselected),
            duration_ms: tests.iter().map(|t| t.duration_ms).sum(),
            tests: Some(tests),
            ..UnitFacts::default()
        })
        .collect();
    // A failure of the run that no test explains (a build that failed, a Swift Testing process
    // that ended without its end) leaves no pass standing.
    let unexplained = events.is_some_and(|e| e.runs_started > e.runs_ended) && !in_flight;
    let failures_by_unit = exit == Some(0) || (any_failed && !unexplained);
    let mut settings = BTreeMap::new();
    settings.insert(
        "package".to_string(),
        Value::String(package.display().to_string()),
    );
    settings.insert(
        "tests".to_string(),
        Value::Array(listing.tests.iter().cloned().map(Value::String).collect()),
    );
    if let Some(why) = &left.refused {
        settings.insert("refused".to_string(), Value::String(why.clone()));
    }
    if held {
        settings.insert(
            "held".to_string(),
            Value::String(format!(
                "a program swift test started held its output {} seconds after it ended: no pass of it is recorded",
                program::OUTPUT_GRACE.as_secs()
            )),
        );
    }
    FactsFile {
        schema: facts::FACTS_SCHEMA_V1.into(),
        adapter: ADAPTER.into(),
        failures_by_unit,
        settings,
        units,
        ..FactsFile::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_MACOS: &str = include_str!("../../../../testdata/swift/list-macos.txt");
    const LIST_LINUX: &str = include_str!("../../../../testdata/swift/list-linux.txt");
    const CONSOLE_MACOS: &str = include_str!("../../../../testdata/swift/console-macos-crash.txt");
    const CONSOLE_LINUX: &str = include_str!("../../../../testdata/swift/console-linux-crash.txt");
    const EVENTS_MACOS: &str = include_str!("../../../../testdata/swift/events-macos-crash.jsonl");
    const EVENTS_LINUX: &str = include_str!("../../../../testdata/swift/events-linux.jsonl");
    const XUNIT: &str = include_str!("../../../../testdata/swift/xunit-macos-parallel.xml");
    const XUNIT_NESTED: &str = include_str!("../../../../testdata/swift/xunit-macos-nested.xml");

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-48
    #[test]
    fn swift_test_is_recognized_and_what_it_runs_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let r = recognize(
            &words("swift test --parallel --filter Cart -c release"),
            cwd,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.package, real(cwd));
        let p = parse(&[
            "--parallel",
            "--filter",
            "Cart",
            "-c",
            "release",
            "--skip-build",
        ]);
        assert!(p.parallel && p.chooses);
        assert_eq!(p.list_args, ["-c", "release"]);
        let p = parse(&[
            "--package-path=sub",
            "--disable-xctest",
            "--xunit-output",
            "x.xml",
        ]);
        assert_eq!(p.package_path.as_deref(), Some("sub"));
        assert!(!p.xctest && p.swift_testing);
        assert_eq!(p.xunit.as_deref(), Some("x.xml"));
        assert_eq!(p.list_args, ["--package-path=sub", "--disable-xctest"]);
        // Starts that run no test, or that hushrun doesn't observe, say why.
        for (command, why) in [
            ("swift test list", "runs no test"),
            ("swift test --list-tests", "runs no test"),
            ("swift test --help", "runs no test"),
            ("swift test --enable-code-coverage", "coverage"),
            ("swift test --repeat-until pass", "more than once"),
            ("swift test --filter", "has no value"),
            ("xcrun swift test", "xcrun"),
            ("/usr/bin/swift test", "by its path"),
        ] {
            let got = recognize(&words(command), cwd).unwrap();
            assert!(
                matches!(&got, Err(e) if e.contains(why)),
                "{command}: {got:?}"
            );
        }
        assert!(
            recognize(
                crate::units::command::without_env(&words("env FOO=1 swift-test --parallel")),
                cwd
            )
            .unwrap()
            .is_ok()
        );
        // A wrapper: only where a package is, and only when its text starts swift test.
        std::fs::write(cwd.join("Makefile"), "test:\n\tswift test\n").unwrap();
        assert!(recognize(&words("make test"), cwd).is_none());
        std::fs::write(cwd.join("Package.swift"), "// swift-tools-version: 6.0\n").unwrap();
        assert!(recognize(&words("make test"), cwd).unwrap().is_ok());
        std::fs::write(cwd.join("Makefile"), "test:\n\t./run-tests\n").unwrap();
        assert!(recognize(&words("make test"), cwd).is_none());
        assert!(recognize(&words("python -m pytest"), cwd).is_none());
        assert!(recognize(&words("uv run pytest"), cwd).is_none());
        assert_eq!(
            swift_major("swift-driver version: 1.168.6 Apple Swift version 6.4.2 (x)"),
            Some(6)
        );
        assert_eq!(
            swift_major("Swift version 5.10.1 (swift-5.10.1-RELEASE)"),
            Some(5)
        );
        assert_eq!(swift_major("Swift Package Manager - Swift 6.4.2"), Some(6));
    }

    // covers: C-48
    #[test]
    fn a_run_of_what_was_built_before_is_refused() {
        // --skip-build runs the test products as they were built, which may be older than the
        // files hushrun keys them by: nothing is left out or recorded.
        let p = parse(&["--skip-build", "--parallel"]);
        assert!(
            p.refused
                .as_deref()
                .is_some_and(|w| w.contains("--skip-build")),
            "{p:?}"
        );
        let dir = tempfile::tempdir().unwrap();
        let got = recognize(&words("swift test --skip-build"), dir.path()).unwrap();
        assert!(
            matches!(&got, Err(e) if e.contains("--skip-build")),
            "{got:?}"
        );
        // --skip-update builds as usual.
        assert!(parse(&["--skip-update"]).refused.is_none());
    }

    // covers: C-48
    #[test]
    fn a_skip_pattern_escapes_every_metacharacter_and_groups_by_class() {
        assert_eq!(
            escape(r"a.b|c(d)[e]{f}^$?*+\g"),
            r"a\.b\|c\(d\)\[e\]\{f\}\^\$\?\*\+\\g"
        );
        assert_eq!(escape("`raw id`/x:y"), "`raw id`/x:y");
        let targets: BTreeSet<String> = ["MixedTests".to_string()].into();
        let tests: BTreeSet<String> = [
            "CoreTests.PriceTests/testTotal()",
            "CoreTests.PriceTests/fails()",
            "CoreTests.PriceTests/Nested/Deeper/deepest()",
            "CoreTests.freeParam(n:)",
            "CartTests.CartTests/testTotal",
            "MixedTests.MixedXCTests/testTotal",
            "DupTests.same()/A.swift:2:2",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(
            skip_patterns(&targets, &tests),
            [
                r"^MixedTests\.",
                r"^CartTests\.CartTests/(?:testTotal)(?:/|$)",
                r"^CoreTests\.(?:freeParam\(n:\))(?:/|$)",
                r"^CoreTests\.PriceTests/(?:fails\(\)|testTotal\(\))(?:/|$)",
                r"^CoreTests\.PriceTests/Nested/Deeper/(?:deepest\(\))(?:/|$)",
            ]
        );
        assert!(has_location("DupTests.same()/A.swift:2:2"));
        assert!(!has_location("CoreTests.PriceTests/testTotal()"));
        assert!(!has_location("CoreTests.a()/b:1:2"));
        // A class with many tests gets more than one value, each within its bound.
        let many: BTreeSet<String> = (0..5000)
            .map(|n| format!("Big.BigTests/testNumber{n:05}"))
            .collect();
        let patterns = skip_patterns(&BTreeSet::new(), &many);
        assert!(patterns.len() > 1);
        assert!(patterns.iter().all(|p| p.len() <= MOST_PATTERN));
        assert_eq!(
            patterns
                .iter()
                .map(|p| p.matches('|').count())
                .sum::<usize>(),
            5000
        );
    }

    // covers: C-48
    #[test]
    fn lists_from_macos_and_linux_say_the_same_ids() {
        let mac = Listing::parse(LIST_MACOS);
        let linux = Listing::parse(LIST_LINUX);
        assert_eq!(mac.tests.len(), 41);
        assert!(mac.contains("CoreTests.`raw identifier test`()"));
        assert!(mac.contains("CoreTests.freeParam(n:)"));
        // The nested XCTest class is macOS's alone: Linux can't compile it.
        let mac_only: Vec<&String> = mac.tests.iter().filter(|t| !linux.contains(t)).collect();
        assert_eq!(mac_only, ["CartTests.Outer.InnerTests/testInner"]);
        assert!(nested("CartTests.Outer.InnerTests/testInner"));
        assert!(!nested("CartTests.CartTests/testTotal"));
        assert!(!nested("CoreTests.PriceTests/Nested/inner()"));
    }

    // covers: C-48
    #[test]
    fn the_console_says_what_serial_xctest_did_on_macos_and_linux() {
        assert_eq!(
            demangle("_TtCO9CartTests5Outer10InnerTests").as_deref(),
            Some("CartTests.Outer.InnerTests")
        );
        assert_eq!(demangle("_TtC9CartTestsP33_AB12Priv").as_deref(), None);
        let listing = Listing::parse(LIST_MACOS);
        let mac = read_console(CONSOLE_MACOS.as_bytes(), &listing);
        assert_eq!(mac["CartTests.CartTests/testTotal"], Outcome::Passed(0));
        assert!(matches!(
            mac["CartTests.CartTests/testFails"],
            Outcome::Failed(59)
        ));
        assert!(matches!(
            mac["CartTests.CartTests/testSkipped"],
            Outcome::Skipped(_)
        ));
        // A crash: started, and no end; the rest of its bundle never ran.
        assert_eq!(
            mac["MixedTests.MixedXCTests/testCrashWhenAsked"],
            Outcome::Started
        );
        assert!(!mac.contains_key("MixedTests.MixedXCTests/testFails"));
        // A nested class prints its mangled name.
        assert!(mac.contains_key("CartTests.Outer.InnerTests/testInner"));
        // Linux names no target: a name one target has is its, one two share is no one's.
        let listing = Listing::parse(LIST_LINUX);
        let linux = read_console(CONSOLE_LINUX.as_bytes(), &listing);
        assert_eq!(
            linux["MixedTests.MixedXCTests/testCrashWhenAsked"],
            Outcome::Started
        );
        assert!(matches!(
            linux["MixedTests.MixedXCTests/testAsync"],
            Outcome::Passed(_)
        ));
        let shared = Listing::parse("A.Same/testX\nB.Same/testX\nA.Only/testY\n");
        let both = read_console(
            b"Test Case 'Same.testX' started at x\nTest Case 'Same.testX' passed (0.0 seconds)\nTest Case 'Only.testY' started at x\nTest Case 'Only.testY' passed (0.1 seconds)\n",
            &shared,
        );
        assert_eq!(both.len(), 1);
        assert_eq!(both["A.Only/testY"], Outcome::Passed(100));
        // Colors, a line a test began without its newline, and a carriage return change nothing.
        let colored = read_console(
            b"\x1b[1mno newline\x1b[0mTest Case 'Only.testY' started at x\rTest Case 'Only.testY' failed (0.002 seconds)\n",
            &shared,
        );
        assert_eq!(colored["A.Only/testY"], Outcome::Failed(2));
    }

    // covers: C-48
    #[test]
    fn a_console_pass_counts_only_after_its_test_started() {
        let listing = Listing::parse("A.Only/testY\nA.Only/testZ\n");
        // A line that says a test passed, printed by a sibling that never started it (a test
        // that prints XCTest's words, a crash before its start): no pass.
        let told = read_console(
            b"Test Case 'Only.testZ' started at x\nTest Case 'Only.testY' passed (0.1 seconds)\nTest Case 'Only.testZ' passed (0.2 seconds)\nTest Case 'Only.testZ' passed (0.3 seconds)\n",
            &listing,
        );
        assert!(!told.contains_key("A.Only/testY"), "{told:?}");
        assert_eq!(told["A.Only/testZ"], Outcome::Passed(200));
        // A failure counts with no start: it never makes a pass.
        let failed = read_console(b"Test Case 'Only.testY' failed (0.1 seconds)\n", &listing);
        assert_eq!(failed["A.Only/testY"], Outcome::Failed(100));
    }

    // covers: C-48
    #[test]
    fn the_event_stream_says_what_swift_testing_did() {
        let listing = Listing::parse(LIST_LINUX);
        let events = read_events(EVENTS_LINUX.as_bytes(), &listing);
        assert_eq!(events.runs_started, events.runs_ended);
        assert!(matches!(
            events.outcomes["CoreTests.PriceTests/testTotal()"],
            Outcome::Passed(_)
        ));
        assert!(matches!(
            events.outcomes["CoreTests.PriceTests/fails()"],
            Outcome::Failed(_)
        ));
        assert!(matches!(
            events.outcomes["CoreTests.PriceTests/skipped()"],
            Outcome::Skipped(_)
        ));
        assert!(matches!(
            events.outcomes["CoreTests.DisabledSuite/neverRuns()"],
            Outcome::Skipped(_)
        ));
        assert!(matches!(
            events.outcomes["CoreTests.PriceTests/twoArgs(s:n:)"],
            Outcome::Passed(_)
        ));
        // No XCTest id, and only listed ids.
        assert!(
            events
                .outcomes
                .keys()
                .all(|id| listing.contains(id) && !xctest(id))
        );
        // A crash on macOS: the process's run never ended, and the tests in flight have no end.
        let listing = Listing::parse(LIST_MACOS);
        let crashed = read_events(EVENTS_MACOS.as_bytes(), &listing);
        assert!(crashed.runs_started > crashed.runs_ended, "{crashed:?}");
        assert_eq!(
            crashed.outcomes["MixedTests.MixedSuite/crashWhenAsked()"],
            Outcome::Started
        );
        // A torn last line is passed over.
        let torn = format!("{}{{\"kind\":\"ev", EVENTS_LINUX);
        assert_eq!(
            read_events(torn.as_bytes(), &Listing::parse(LIST_LINUX)),
            read_events(EVENTS_LINUX.as_bytes(), &Listing::parse(LIST_LINUX))
        );
        // Two private tests of one name in two files: the list keeps their locations, and so
        // does what the stream says of them.
        let dup = Listing::parse("DupTests.same()/A.swift:2:2\nDupTests.same()/B.swift:2:2\n");
        let stream = br#"{"kind":"test","payload":{"id":"DupTests.same()/A.swift:2:2","kind":"function","sourceLocation":{"fileID":"DupTests/A.swift","line":2,"column":2}},"version":0}
{"kind":"event","payload":{"kind":"testStarted","testID":"DupTests.same()/A.swift:2:2","instant":{"absolute":1.0}},"version":0}
{"kind":"event","payload":{"kind":"testEnded","testID":"DupTests.same()/A.swift:2:2","instant":{"absolute":1.5}},"version":0}
"#;
        let read = read_events(stream, &dup);
        assert_eq!(
            read.outcomes["DupTests.same()/A.swift:2:2"],
            Outcome::Passed(500)
        );
        // A known issue, or a warning, fails nothing.
        let listing = Listing::parse("T.ok()\n");
        let warned = br#"{"kind":"test","payload":{"id":"T.ok()/T.swift:1:1","kind":"function","sourceLocation":{"fileID":"T/T.swift","line":1,"column":1}},"version":0}
{"kind":"event","payload":{"kind":"testStarted","testID":"T.ok()/T.swift:1:1","instant":{"absolute":1.0}},"version":0}
{"kind":"event","payload":{"kind":"issueRecorded","testID":"T.ok()/T.swift:1:1","issue":{"isKnown":true}},"version":0}
{"kind":"event","payload":{"kind":"issueRecorded","testID":"T.ok()/T.swift:1:1","issue":{"isKnown":false,"severity":"warning"}},"version":0}
{"kind":"event","payload":{"kind":"testEnded","testID":"T.ok()/T.swift:1:1","instant":{"absolute":1.0}},"version":0}
"#;
        assert_eq!(
            read_events(warned, &listing).outcomes["T.ok()"],
            Outcome::Passed(0)
        );
    }

    // covers: C-48
    #[test]
    fn the_parallel_xunit_says_what_xctest_did() {
        let listing = Listing::parse(LIST_MACOS);
        let read = read_xunit(XUNIT.as_bytes(), &listing);
        assert!(matches!(
            read["CartTests.CartTests/testFails"],
            Outcome::Failed(_)
        ));
        // An XCTSkip reads as passed there.
        assert!(matches!(
            read["CartTests.CartTests/testSkipped"],
            Outcome::Passed(_)
        ));
        assert!(read.keys().all(|id| xctest(id)));
        // A nested class that never ran reads as passed too: under any --skip, it doesn't count.
        let nested_read = read_xunit(XUNIT_NESTED.as_bytes(), &listing);
        assert!(matches!(
            nested_read["CartTests.Outer.InnerTests/testInner"],
            Outcome::Passed(_)
        ));
        let parsed = Parsed {
            parallel: true,
            chooses: true,
            ..parse::<&str>(&[])
        };
        let file = tested(
            &listing,
            &BTreeMap::new(),
            None,
            Some(&nested_read),
            &LeftOut::default(),
            &parsed,
            Some(0),
            Path::new("/work/Shop"),
            false,
        );
        let cart = file.units.iter().find(|u| u.unit == "CartTests").unwrap();
        let inner = cart
            .tests
            .as_ref()
            .unwrap()
            .iter()
            .find(|t| t.id == "Outer.InnerTests/testInner")
            .unwrap();
        assert_eq!(inner.finished, !cfg!(target_os = "macos"));
    }

    // covers: C-48
    #[test]
    fn output_held_past_the_end_records_no_pass() {
        let listing = Listing::parse("A.C/testOk\nA.C/testBad\n");
        let console: BTreeMap<String, Outcome> = [
            ("A.C/testOk".to_string(), Outcome::Passed(1)),
            ("A.C/testBad".to_string(), Outcome::Failed(1)),
        ]
        .into();
        let parsed = parse::<&str>(&[]);
        let file = tested(
            &listing,
            &console,
            None,
            None,
            &LeftOut::default(),
            &parsed,
            Some(1),
            Path::new("/p"),
            true,
        );
        let tests = file.units[0].tests.as_ref().unwrap();
        let of = |id: &str| tests.iter().find(|t| t.id == id).unwrap().clone();
        assert!(!of("C/testOk").finished);
        assert!(of("C/testBad").finished && of("C/testBad").failed);
        assert!(file.settings.contains_key("held"));
    }

    // covers: C-48, C-32
    #[test]
    fn a_skipped_test_is_told_as_skipped() {
        let listing = Listing::parse("A.C/testSkip\nA.C/testOk\nB.s()\n");
        let console: BTreeMap<String, Outcome> = [
            ("A.C/testSkip".to_string(), Outcome::Skipped(1)),
            ("A.C/testOk".to_string(), Outcome::Passed(1)),
        ]
        .into();
        let events = Events {
            outcomes: [("B.s()".to_string(), Outcome::Skipped(0))].into(),
            runs_started: 1,
            runs_ended: 1,
        };
        let parsed = parse::<&str>(&[]);
        let file = tested(
            &listing,
            &console,
            Some(&events),
            None,
            &LeftOut::default(),
            &parsed,
            Some(1),
            Path::new("/p"),
            false,
        );
        let skipped: BTreeMap<String, bool> = file
            .units
            .iter()
            .flat_map(|u| u.tests.iter().flatten().map(|t| (t.id.clone(), t.skipped)))
            .collect();
        assert_eq!(
            skipped,
            [
                ("C/testOk".to_string(), false),
                ("C/testSkip".to_string(), true),
                ("s()".to_string(), true)
            ]
            .into()
        );
        // Under --parallel the xunit file reads an XCTSkip as a pass: in a run that failed, any
        // XCTest pass there may be a skip.
        let parallel = parse(&["--parallel"]);
        let xunit: BTreeMap<String, Outcome> =
            [("A.C/testOk".to_string(), Outcome::Passed(1))].into();
        for (exit, may_skip) in [(Some(1), true), (Some(0), false)] {
            let file = tested(
                &listing,
                &BTreeMap::new(),
                None,
                Some(&xunit),
                &LeftOut::default(),
                &parallel,
                exit,
                Path::new("/p"),
                false,
            );
            let ok = file.units[0].tests.as_ref().unwrap();
            let ok = ok.iter().find(|t| t.id == "C/testOk").unwrap();
            assert!(ok.finished);
            assert_eq!(ok.skipped, may_skip, "{exit:?}");
        }
    }

    // covers: C-48
    #[test]
    fn a_runs_facts_never_take_a_pass_from_absence() {
        let listing = Listing::parse(LIST_MACOS);
        let console = read_console(CONSOLE_MACOS.as_bytes(), &listing);
        let mut events = read_events(EVENTS_MACOS.as_bytes(), &listing);
        // As if the plan's `--skip` had left it out.
        events.outcomes.remove("CoreTests.PriceTests/testTotal()");
        let mut left = LeftOut {
            patterns: vec!["x".into()],
            ..LeftOut::default()
        };
        left.tests.insert("CoreTests.PriceTests/testTotal()".into());
        let parsed = parse::<&str>(&[]);
        let file = tested(
            &listing,
            &console,
            Some(&events),
            None,
            &left,
            &parsed,
            Some(1),
            Path::new("/p"),
            false,
        );
        let unit = |name: &str| file.units.iter().find(|u| u.unit == name).unwrap();
        let test = |u: &str, id: &str| {
            unit(u)
                .tests
                .as_ref()
                .unwrap()
                .iter()
                .find(|t| t.id == id)
                .unwrap()
                .clone()
        };
        // The crashed XCTest test failed, and those after it in its bundle didn't run.
        let crashed = test("MixedTests", "MixedXCTests/testCrashWhenAsked");
        assert!(crashed.failed && !crashed.finished);
        let after = test("MixedTests", "MixedXCTests/testFails");
        assert!(!after.finished && !after.failed);
        assert!(unit("MixedTests").failed);
        // A test the plan left out, and that didn't run, is deselected.
        assert!(test("CoreTests", "PriceTests/testTotal()").deselected);
        assert!(!unit("CoreTests").deselected);
        // A start without an end, in a run that passed, is no failure: it didn't finish.
        let quiet = tested(
            &listing,
            &console,
            Some(&events),
            None,
            &LeftOut::default(),
            &parsed,
            Some(0),
            Path::new("/p"),
            false,
        );
        let crashed = quiet
            .units
            .iter()
            .find(|u| u.unit == "MixedTests")
            .unwrap()
            .tests
            .as_ref()
            .unwrap()
            .iter()
            .find(|t| t.id == "MixedXCTests/testCrashWhenAsked")
            .unwrap()
            .clone();
        assert!(!crashed.failed && !crashed.finished);
        // A failed run no test explains keeps no pass.
        let none = tested(
            &listing,
            &BTreeMap::new(),
            None,
            None,
            &LeftOut::default(),
            &parsed,
            Some(1),
            Path::new("/p"),
            false,
        );
        assert!(!none.failures_by_unit);
        assert!(none.units.iter().all(|u| u.finished == 0));
        assert_eq!(none.settings["tests"].as_array().unwrap().len(), 41);
    }

    fn fixture(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/swift")
            .join(name);
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn tree_of(paths: &[&str]) -> Tree {
        let manifest: String = paths
            .iter()
            .map(|p| format!("f {} {p}\0", "0".repeat(64)))
            .collect();
        Tree::from_manifest(format!("{}\n{manifest}", hushrun_core::CANONICALIZATION).as_bytes())
            .expect("a tree")
    }

    // covers: C-48
    #[test]
    fn a_test_target_reads_its_directory_and_those_it_depends_on() {
        let root = Path::new("/work");
        let described = vec![
            Described {
                dir: PathBuf::from("/work/Shop"),
                describe: fixture("describe-shop.json"),
                dump: Some(fixture("dump-shop.json")),
            },
            Described {
                dir: PathBuf::from("/work/LocalMoney"),
                describe: fixture("describe-localmoney.json"),
                dump: Some(fixture("dump-localmoney.json")),
            },
        ];
        let files = [
            "Shop/Package.swift",
            "Shop/Package.resolved",
            "Shop/Sources/Core/Core.swift",
            "Shop/Sources/CHelpers/helpers.c",
            "Shop/Sources/CHelpers/include/helpers.h",
            "Shop/Sources/CZ/module.modulemap",
            "Shop/Sources/Cart/Cart.swift",
            "Shop/Sources/gen-tool/main.swift",
            "Shop/Sources/shop-cli/main.swift",
            "Shop/Plugins/GenPlugin/plugin.swift",
            "Shop/Tests/CoreTests/CoreTests.swift",
            "Shop/Tests/CartTests/CartTests.swift",
            "Shop/Tests/MixedTests/MixedXCTests.swift",
            "Shop/Tests/MixedTests/Fixtures/a.json",
            "Shop/README.md",
            "LocalMoney/Package.swift",
            "LocalMoney/Sources/Money/Money.swift",
            "LocalMoney/Sources/MoneyFormat/Format.swift",
        ];
        let tree = tree_of(&files);
        let graph = graph_of(&described, root, &tree);
        assert_eq!(
            graph.tests.keys().collect::<Vec<_>>(),
            [
                "Shop/Tests/CartTests",
                "Shop/Tests/CoreTests",
                "Shop/Tests/MixedTests"
            ]
        );
        let core = &graph.tests["Shop/Tests/CoreTests"];
        assert!(core.not_followed.is_empty(), "{:?}", core.not_followed);
        let keyed: Vec<&String> = core.reads.iter().filter(|p| of_kind(p)).collect();
        assert_eq!(
            keyed,
            [
                "LocalMoney/Sources/Money/Money.swift",
                "LocalMoney/Sources/MoneyFormat/Format.swift",
                "Shop/Sources/CHelpers/helpers.c",
                "Shop/Sources/CHelpers/include/helpers.h",
                "Shop/Sources/Core/Core.swift",
                "Shop/Tests/CoreTests/CoreTests.swift",
            ]
        );
        assert_eq!(core.own, ["Shop/Tests/CoreTests/CoreTests.swift"]);
        // Cart is built with a plugin: what depends on it reads its package whole.
        let cart = &graph.tests["Shop/Tests/CartTests"];
        assert!(cart.reads.contains("Shop/Sources/shop-cli/main.swift"));
        assert_eq!(graph.by_c99["CartTests"], "Shop/Tests/CartTests");
        // Sources: what SwiftPM compiles below a target's directory; never a resource, a
        // manifest or a module map.
        assert!(graph.named.contains("Shop/Sources/Core/Core.swift"));
        assert!(
            !graph
                .named
                .contains("Shop/Tests/MixedTests/Fixtures/a.json")
        );
        assert!(!graph.named.contains("Shop/Sources/CZ/module.modulemap"));
        assert!(graph.is_unit("Shop/Tests/CartTests/CartTests.swift"));
        assert!(!graph.is_unit("Shop/Tests/MixedTests/Fixtures/a.json"));
        // Without Package.resolved, a remote product isn't pinned.
        let unpinned = graph_of(
            &described,
            root,
            &tree_of(
                &files
                    .iter()
                    .copied()
                    .filter(|f| *f != "Shop/Package.resolved")
                    .collect::<Vec<_>>(),
            ),
        );
        assert!(
            unpinned.tests["Shop/Tests/CoreTests"]
                .not_followed
                .iter()
                .any(|n| n.why == "unpinned")
        );
        // A local package outside the scope can't be keyed.
        let outside = graph_of(
            &described,
            root,
            &tree_of(
                &files
                    .iter()
                    .copied()
                    .filter(|f| !f.starts_with("LocalMoney"))
                    .collect::<Vec<_>>(),
            ),
        );
        assert!(
            outside.tests["Shop/Tests/CoreTests"]
                .not_followed
                .iter()
                .any(|n| n.why == "outside-scope")
        );
        // A header search path is read; flags that name a path aren't followed.
        let mut edge = described.clone();
        let dump = edge[0].dump.as_mut().unwrap();
        for t in dump["targets"].as_array_mut().unwrap() {
            if t["name"] == "Core" {
                t["settings"] = serde_json::json!([
                    {"tool": "c", "kind": {"headerSearchPath": {"_0": "../CHelpers/include"}}},
                    {"tool": "swift", "kind": {"unsafeFlags": {"_0": ["-warnings-as-errors"]}}}
                ]);
            }
        }
        let g = graph_of(&edge, root, &tree);
        assert!(
            g.tests["Shop/Tests/CoreTests"]
                .dirs
                .contains("Shop/Sources/CHelpers/include")
        );
        assert!(g.tests["Shop/Tests/CoreTests"].not_followed.is_empty());
        for t in edge[0].dump.as_mut().unwrap()["targets"]
            .as_array_mut()
            .unwrap()
        {
            if t["name"] == "Core" {
                t["settings"] = serde_json::json!([{"tool": "swift", "kind": {"unsafeFlags": {"_0": ["-I", "../outside"]}}}]);
            }
        }
        let g = graph_of(&edge, root, &tree);
        assert!(
            g.tests["Shop/Tests/CoreTests"]
                .not_followed
                .iter()
                .any(|n| n.why == "unsafe-flags")
        );
        assert!(names_a_path("-Iinclude") && !names_a_path("-enable-upcoming-feature"));
    }

    // covers: C-48
    #[test]
    fn a_build_tool_plugin_of_another_package_makes_its_target_read_its_package_whole() {
        let root = Path::new("/work");
        let mut described = vec![
            Described {
                dir: PathBuf::from("/work/Shop"),
                describe: fixture("describe-shop.json"),
                dump: Some(fixture("dump-shop.json")),
            },
            Described {
                dir: PathBuf::from("/work/LocalMoney"),
                describe: fixture("describe-localmoney.json"),
                dump: Some(fixture("dump-localmoney.json")),
            },
        ];
        // Core is built with a plugin of another package: `describe` names it only among its
        // product dependencies, and `dump-package` among its plugin usages.
        for t in described[0].dump.as_mut().unwrap()["targets"]
            .as_array_mut()
            .unwrap()
        {
            if t["name"] == "Core" {
                t["pluginUsages"] = serde_json::json!([{"plugin": {"_0": "P", "_1": "LocalPlug"}}]);
            }
        }
        let files = [
            "Shop/Package.swift",
            "Shop/Package.resolved",
            "Shop/Sources/Core/Core.swift",
            "Shop/Sources/shop-cli/main.swift",
            "Shop/Tests/CoreTests/CoreTests.swift",
            "LocalMoney/Package.swift",
            "LocalMoney/Sources/Money/Money.swift",
            "LocalMoney/Sources/MoneyFormat/Format.swift",
        ];
        let graph = graph_of(&described, root, &tree_of(&files));
        let core = &graph.tests["Shop/Tests/CoreTests"];
        assert!(
            core.reads.contains("Shop/Sources/shop-cli/main.swift"),
            "{:?}",
            core.reads
        );
        // The same from `describe` alone, when `dump-package` names no plugin usage: a product
        // dependency that is a plugin product.
        for t in described[0].dump.as_mut().unwrap()["targets"]
            .as_array_mut()
            .unwrap()
        {
            t.as_object_mut().unwrap().remove("pluginUsages");
        }
        for t in described[1].describe["products"].as_array_mut().unwrap() {
            if t["name"] == "Money" {
                t["type"] = serde_json::json!({"plugin": null});
            }
        }
        let graph = graph_of(&described, root, &tree_of(&files));
        assert!(
            graph.tests["Shop/Tests/CoreTests"]
                .reads
                .contains("Shop/Sources/shop-cli/main.swift")
        );
    }

    // covers: C-48
    #[test]
    fn a_plan_leaves_nothing_out_where_its_list_says_swiftpm_would_run_less() {
        let graph = Graph::default();
        let mut map = Map::default();
        // No list kept: the list after the run is the check (run_again_whole).
        assert!(leaves_nothing_out(&map, &graph).is_ok());
        map.unit_files = [
            "Tests/CartTests::CartTests/testTotal",
            "Tests/CoreTests::PriceTests/a()",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert!(leaves_nothing_out(&map, &graph).is_ok());
        map.unit_files
            .insert("Tests/CartTests::Outer.InnerTests/testInner".into());
        assert_eq!(
            leaves_nothing_out(&map, &graph).is_err(),
            cfg!(target_os = "macos")
        );
        map.unit_files
            .remove("Tests/CartTests::Outer.InnerTests/testInner");
        for n in 0..20000 {
            map.unit_files
                .insert(format!("Tests/BigTests::BigTests/testNumber{n:05}"));
        }
        assert!(
            leaves_nothing_out(&map, &graph)
                .unwrap_err()
                .contains("one argument")
        );
    }
}
