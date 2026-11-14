//! \[ADR 0089\] Test targets and tests with `swift test` (C-48): a unit is a test target, named by
//! its directory, what it read is what `swift package describe` says it is built from, a test is
//! its XCTest `Class/method` or its Swift Testing function, and reuse never misses a failure.
//!
//! The packages are tiny, and need Swift 6: `HUSHRUN_TEST_SWIFT` names the `swift` to use (by
//! default the one on `PATH`). The tests say so and pass where it is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;
use serde_json::{Value, json};

/// Environment variables a test must never inherit.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_UNITS",
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "npm_lifecycle_event",
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CODEX_SANDBOX",
    "CODEX_CI",
    "CODEX_THREAD_ID",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "CURSOR_AGENT",
    "CURSOR_EXTENSION_HOST_ROLE",
    "GEMINI_CLI",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
    "COPILOT_MODEL",
    "COPILOT_ALLOW_ALL",
    "COPILOT_GITHUB_TOKEN",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "RUSTC_WRAPPER",
    "FAIL",
    "HIDDEN",
    "CRASH",
    "NESTED_FAIL",
];

const NO_HOST: &str = "http://127.0.0.1:9";

/// The `swift` these tests run, and whether it is Swift 6 or later.
fn swift() -> Option<PathBuf> {
    let named = std::env::var_os("HUSHRUN_TEST_SWIFT").map(PathBuf::from);
    let swift = named.unwrap_or_else(|| PathBuf::from("swift"));
    let output = Command::new(&swift).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let major: u32 = text
        .split("Swift version ")
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    (output.status.success() && major >= 6).then_some(swift)
}

fn tools(what: &str, here: bool) -> bool {
    if here {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    /// `PATH` with the test's `swift` first.
    path: std::ffi::OsString,
}

impl Repo {
    fn new(swift: &Path, files: &[(String, String)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::fs::create_dir_all(root.join("home")).expect("home");
        git::init(&repo, "sha1");
        let mut dirs: Vec<PathBuf> = Vec::new();
        if swift.is_absolute()
            && let Some(parent) = swift.parent()
        {
            dirs.push(parent.to_path_buf());
        }
        dirs.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let r = Repo {
            _dir: dir,
            cache: root.join("cache"),
            path: std::env::join_paths(dirs).expect("PATH"),
            root,
            repo,
        };
        for (path, text) in files {
            r.write(path, text);
        }
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn hushrun(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", self.root.join("home"))
            .env("PATH", &self.path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        for var in CLEARED {
            cmd.env_remove(var);
        }
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        cmd
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = self.hushrun();
        for (name, value) in env {
            cmd.env(name, value);
        }
        let output = cmd.args(args).output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn map(&self) -> Value {
        store::map(&self.cache)
    }

    fn newest_result(&self) -> Value {
        let mut files = Vec::new();
        collect(&self.cache.join("results/results/v0"), &mut files);
        files.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
        let newest = files.last().expect("a result");
        serde_json::from_slice(&std::fs::read(newest).unwrap()).unwrap()
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// The line of what hushrun said that starts so.
    fn line(&self, start: &str) -> &str {
        self.stderr
            .lines()
            .find(|l| l.starts_with(start))
            .unwrap_or_else(|| panic!("no line starting {start:?} in:\n{}", self.stderr))
    }

    /// Whether XCTest ran `Class/method` of `target`, as its console says on macOS or Linux.
    fn xctest_ran(&self, target: &str, class: &str, method: &str) -> bool {
        self.stdout
            .contains(&format!("Test Case '-[{target}.{class} {method}]' started"))
            || self
                .stdout
                .contains(&format!("Test Case '{class}.{method}' started"))
    }

    /// Whether Swift Testing ran the function `name`.
    fn swift_testing_ran(&self, name: &str) -> bool {
        self.stdout
            .lines()
            .chain(self.stderr.lines())
            .any(|l| l.contains(&format!("Test {name} started")))
    }
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

const MANIFEST: &str = r#"// swift-tools-version: 6.0
import PackageDescription
let package = Package(
  name: "Shop",
  targets: [
    .target(name: "Core"),
    .target(name: "Cart", dependencies: ["Core"]),
    .target(name: "Greet"),
    .testTarget(name: "CartTests", dependencies: ["Cart"]),
    .testTarget(name: "CoreTests", dependencies: ["Core"]),
    .testTarget(name: "GreetTests", dependencies: ["Greet"]),
  ]
)
"#;

/// A shop in Swift: Core, Cart on Core, Greet; XCTest for Cart and Greet, Swift Testing for Core.
/// `FAIL` fails one test of each of Cart's and Core's; `CRASH` crashes them.
fn shop(tests: &str) -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", ".build/\n"),
        file("Package.swift", MANIFEST),
        file(
            "Sources/Core/Core.swift",
            "public func price(_ x: Int) -> Int { x * 2 }\n",
        ),
        file(
            "Sources/Cart/Cart.swift",
            "import Core\npublic func total(_ xs: [Int]) -> Int { xs.map(price).reduce(0, +) }\n",
        ),
        file(
            "Sources/Greet/Greet.swift",
            "public func hello() -> String { \"hi\" }\n",
        ),
        file(
            "Tests/CartTests/CartTests.swift",
            r#"import XCTest
import Foundation
@testable import Cart

final class CartTests: XCTestCase {
    func testTotal() { XCTAssertEqual(total([1, 2]), 6) }
    func testTax() { XCTAssertEqual(total([5]), 10) }
    func testFlips() {
        if ProcessInfo.processInfo.environment["CRASH"] != nil { fatalError("crash") }
        XCTAssertNil(ProcessInfo.processInfo.environment["FAIL"])
    }
}
"#,
        ),
        file(
            "Tests/CoreTests/CoreTests.swift",
            r#"import Testing
import Foundation
@testable import Core

@Suite struct PriceTests {
    @Test func price() { #expect(Core.price(3) == 6) }
    @Test func tax() { #expect(Core.price(5) == 10) }
    @Test func flips() {
        if ProcessInfo.processInfo.environment["CRASH"] != nil { fatalError("crash") }
        #expect(ProcessInfo.processInfo.environment["FAIL"] == nil)
    }
}
"#,
        ),
        file(
            "Tests/GreetTests/GreetTests.swift",
            "import XCTest\n@testable import Greet\n\nfinal class GreetTests: XCTestCase {\n    func testHello() { XCTAssertEqual(hello(), \"hi\") }\n}\n",
        ),
        file("notes/a.md", "a\n"),
    ]
}

const SWIFT_TEST: &[&str] = &["run", "unit", "--", "swift", "test"];

// covers: C-48
#[test]
fn test_targets_are_observed_then_reused_and_a_library_change_runs_those_that_read_it() {
    let Some(swift) = swift() else {
        tools("Swift 6", false);
        return;
    };
    let r = Repo::new(&swift, &shop("{mode: reuse, ignore: ['notes/**']}"));
    let first = r.run(SWIFT_TEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // Nothing had passed yet: every test target runs.
    assert_eq!(
        first.line("hushrun: unit: all "),
        "hushrun: unit: all 3 test targets run: none had passed here yet on the files it read"
    );
    let map = r.map();
    let reads = |unit: &str| -> Vec<String> {
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit}: {map:#}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    // A test target read its sources and those of every target it depends on.
    assert_eq!(
        reads("Tests/CartTests"),
        [
            "Sources/Cart/Cart.swift",
            "Sources/Core/Core.swift",
            "Tests/CartTests/CartTests.swift"
        ]
    );
    assert_eq!(
        reads("Tests/CoreTests"),
        ["Sources/Core/Core.swift", "Tests/CoreTests/CoreTests.swift"]
    );
    assert!(map["rest"]["Package.swift"].is_string(), "{map:#}");
    // The list after the run is kept, by unit and id.
    let listed: Vec<&str> = map["unitFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        listed.contains(&"Tests/CartTests::CartTests/testTotal"),
        "{listed:?}"
    );
    assert!(
        listed.contains(&"Tests/CoreTests::PriceTests/price()"),
        "{listed:?}"
    );
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 3, "ran": 3, "reused": 0})
    );

    // Greet changes: GreetTests runs; of Cart's and Core's, one runs as a check, one is reused.
    r.write(
        "Sources/Greet/Greet.swift",
        "public func hello() -> String { \"h\" + \"i\" }\n",
    );
    let second = r.run(SWIFT_TEST, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        second.line("hushrun: unit: "),
        "hushrun: unit: 2 of 3 test targets run; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(second.xctest_ran("GreetTests", "GreetTests", "testHello"));
    let cart = second.xctest_ran("CartTests", "CartTests", "testTotal");
    let core = second.swift_testing_ran("price()");
    assert!(cart != core, "one of the two runs:\n{}", second.stdout);
    assert_eq!(
        r.newest_result()["units"]["reused"],
        json!(1),
        "{}",
        second.stderr
    );
    // \[C-37\] `hushrun why` knows a test target by its directory, the reused one too.
    for unit in ["Tests/CartTests", "Tests/CoreTests"] {
        let why = r.run(&["why", "unit", unit], &[]);
        assert!(
            why.stdout.contains(&format!("{unit} would be reused")),
            "{}\n{}",
            why.stdout,
            why.stderr
        );
    }

    // Core changes: Cart's and Core's tests read it, and run; Greet's runs as a check.
    r.write(
        "Sources/Core/Core.swift",
        "public func price(_ x: Int) -> Int { x + x }\n",
    );
    let third = r.run(SWIFT_TEST, &[]);
    assert_eq!(third.code, 0, "{}\n{}", third.stdout, third.stderr);
    assert_eq!(
        third.line("hushrun: unit: "),
        "hushrun: unit: all 3 test targets run; 1 that already passed on the files it read runs anyway, as a check"
    );
    assert!(third.xctest_ran("CartTests", "CartTests", "testTotal"));
    assert!(third.swift_testing_ran("price()"));

    // A file no target reads, ignored: every target may be reused but the one audited.
    r.write("notes/a.md", "b\n");
    let fourth = r.run(SWIFT_TEST, &[]);
    assert_eq!(fourth.code, 0, "{}\n{}", fourth.stdout, fourth.stderr);
    assert_eq!(
        fourth.line("hushrun: unit: "),
        "hushrun: unit: 1 of 3 test targets run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );

    // A wrapper that says swift test but starts swift by its path goes round hushrun's swift:
    // nothing was observed, and the run says so.
    let real = which(&swift, &r.path);
    let bypassed = r.run(
        &[
            "run",
            "--force",
            "unit",
            "--",
            "sh",
            "-c",
            &format!("echo swift test; '{}' build", real.display()),
        ],
        &[],
    );
    assert_eq!(bypassed.code, 0, "{}", bypassed.stderr);
    assert!(
        bypassed.stderr.contains(
            "hushrun: unit: test files weren't observed: swift test wrote nothing for hushrun: no `swift test` reached hushrun's `swift` on PATH"
        ),
        "{}",
        bypassed.stderr
    );
    let direct = r.run(
        &[
            "run",
            "--force",
            "unit",
            "--",
            &real.display().to_string(),
            "build",
        ],
        &[],
    );
    // \[C-32\] Not `swift test`, and no wrapper whose text says it: not Swift's adapter's, and
    // nothing is said of Swift.
    assert!(
        !direct.stderr.contains("test files weren't observed") && !direct.stderr.contains("swift"),
        "{}",
        direct.stderr
    );
    if cfg!(target_os = "macos") {
        let xcrun = r.run(
            &[
                "run",
                "--force",
                "unit",
                "--",
                "xcrun",
                "swift",
                "test",
                "--skip-build",
            ],
            &[],
        );
        assert!(
            xcrun.stderr.contains(
                "hushrun: unit: test files aren't observed: xcrun starts swift from the selected toolchain"
            ),
            "{}",
            xcrun.stderr
        );
    }

    // Everything left out: hushrun's swift on its own, with every target listed. SwiftPM warns,
    // and a pass stays a pass.
    let units = r.root.join("units-all");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(
        units.join("hushrun.json"),
        serde_json::to_vec(&json!({
            "schema": "hushrun.units/v0",
            "root": r.repo,
            "reuse": ["Tests/CartTests", "Tests/CoreTests", "Tests/GreetTests"],
            "unit": "file",
        }))
        .unwrap(),
    )
    .unwrap();
    let all = r
        .hushrun()
        .args(["__hushrun-program", "swift", "test"])
        .env("HUSHRUN_UNITS", &units)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&all.stderr);
    assert_eq!(all.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("No matching test cases were run"),
        "{stderr}"
    );
    let facts: Vec<Value> = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("swift-"))
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .collect();
    assert_eq!(facts.len(), 1);
    let deselected: Vec<bool> = facts[0]["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["deselected"].as_bool().unwrap_or(false))
        .collect();
    assert_eq!(deselected, [true, true, true], "{:#}", facts[0]);
}

/// The real `swift` a shell finds on `path`, by its whole path.
fn which(swift: &Path, path: &std::ffi::OsStr) -> PathBuf {
    if swift.is_absolute() {
        return swift.to_path_buf();
    }
    std::env::split_paths(path)
        .map(|d| d.join(swift))
        .find(|p| p.is_file())
        .expect("swift on PATH")
}

// covers: C-48, C-39
#[test]
fn with_tests_as_the_unit_only_the_tests_that_failed_run_again_and_a_crash_records_no_pass() {
    let Some(swift) = swift() else {
        tools("Swift 6", false);
        return;
    };
    let r = Repo::new(&swift, &shop("{mode: reuse, unit: test}"));
    let first = r.run(SWIFT_TEST, &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let map = r.map();
    assert_eq!(
        map["units"]["Tests/CartTests"]["tests"],
        json!({"CartTests/testFlips": "failed", "CartTests/testTax": "passed", "CartTests/testTotal": "passed"}),
        "{map:#}"
    );
    assert_eq!(
        map["units"]["Tests/CoreTests"]["tests"],
        json!({"PriceTests/flips()": "failed", "PriceTests/price()": "passed", "PriceTests/tax()": "passed"}),
        "{map:#}"
    );

    // The same files, nothing failing: the two that failed run, with the one test the audit
    // draws of the five the plan could leave out; the others are left out by --skip.
    let second = r.run(SWIFT_TEST, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        hushrun_testkit::lines::drawn(second.line("hushrun: unit: ")),
        hushrun_testkit::lines::drawn(
            "hushrun: unit: 3 of 7 tests run, in 2 of 3 test targets; 4 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
    assert!(second.xctest_ran("CartTests", "CartTests", "testFlips"));
    assert!(second.swift_testing_ran("flips()"));
    let ran_again = [
        second.xctest_ran("CartTests", "CartTests", "testTotal"),
        second.xctest_ran("CartTests", "CartTests", "testTax"),
        second.swift_testing_ran("price()"),
        second.swift_testing_ran("tax()"),
    ];
    // \[C-34\] One audited, of the four left out one by one and Greet's, reused whole: wherever
    // the draw found it.
    let greet = second.xctest_ran("GreetTests", "GreetTests", "testHello");
    assert_eq!(
        ran_again.iter().filter(|r| **r).count() + usize::from(greet),
        1,
        "one audited:\n{}",
        second.stdout
    );
    let units = r.newest_result()["units"].clone();
    assert_eq!(
        units["tests"],
        json!({"total": 7, "ran": 3, "reused": 4}),
        "{units}"
    );
    assert_eq!(
        r.map()["units"]["Tests/CartTests"]["tests"]["CartTests/testFlips"],
        "passed"
    );

    // A crash in each library: the crashed tests fail, and nothing that didn't finish passes.
    r.write(
        "Sources/Cart/Cart.swift",
        "import Core\npublic func total(_ xs: [Int]) -> Int { xs.map(price).reduce(0) { $0 + $1 } }\n",
    );
    r.write(
        "Sources/Core/Core.swift",
        "public func price(_ x: Int) -> Int { 2 * x }\n",
    );
    let crashed = r.run(SWIFT_TEST, &[("CRASH", "1")]);
    assert_ne!(crashed.code, 0, "{}", crashed.stderr);
    let map = r.map();
    let cart = &map["units"]["Tests/CartTests"]["tests"];
    assert_eq!(cart["CartTests/testFlips"], "failed", "{map:#}");
    for (id, state) in cart.as_object().unwrap() {
        assert!(
            state == "failed" || state == "passed" || state == "not-run",
            "{id}: {state}"
        );
    }
    let core = &map["units"]["Tests/CoreTests"]["tests"];
    assert_eq!(core["PriceTests/flips()"], "failed", "{map:#}");
    // Neither target passed whole.
    assert_eq!(r.newest_result()["result"], "fail", "{}", crashed.stderr);
}

// covers: C-48, C-34
#[test]
fn an_audited_test_that_fails_again_alone_through_hushruns_swift_suspends_reuse() {
    let Some(swift) = swift() else {
        tools("Swift 6", false);
        return;
    };
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", ".build/\n"),
        file(
            "Package.swift",
            "// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: \"Two\", targets: [.target(name: \"Lib\"), .testTarget(name: \"TwoTests\", dependencies: [\"Lib\"])])\n",
        ),
        file("Sources/Lib/Lib.swift", "public let one = 1\n"),
        file(
            "Tests/TwoTests/TwoTests.swift",
            r#"import XCTest
import Foundation
@testable import Lib

final class TwoTests: XCTestCase {
    func testOk() { XCTAssertNil(ProcessInfo.processInfo.environment["HIDDEN"]) }
    func testFlips() { XCTAssertNil(ProcessInfo.processInfo.environment["FAIL"]) }
}
"#,
        ),
    ];
    let r = Repo::new(&swift, &files);
    assert_eq!(r.run(SWIFT_TEST, &[("FAIL", "1")]).code, 1);
    // testOk passed on these files and is the one test that could be reused: it runs as a check,
    // and fails. It runs once more, alone, through the first run's swift with the second run's
    // directory, and fails again: a disagreement.
    let caught = r.run(SWIFT_TEST, &[("HIDDEN", "1")]);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        caught.line("hushrun: unit: all"),
        "hushrun: unit: all 2 tests run; 1 that already passed on the files it read runs anyway, as a check"
    );
    assert!(
        caught.stderr.contains(
            "hushrun: unit: Tests/TwoTests::TwoTests/testOk failed, though it passed before on the same files it read: running it once more"
        ),
        "{}",
        caught.stderr
    );
    // The second run ran testOk alone: testFlips was left out by --skip.
    // With a selection, XCTest's outermost suite is 'Selected tests'.
    assert!(
        caught
            .stdout
            .contains("Test Suite 'Selected tests' started"),
        "{}",
        caught.stdout
    );
    let last = caught
        .stdout
        .rsplit("Test Suite 'Selected tests' started")
        .next()
        .unwrap();
    assert!(
        last.contains("testOk") && !last.contains("testFlips"),
        "{}",
        caught.stdout
    );
    assert!(
        caught.stderr.contains(
            "hushrun: unit: Tests/TwoTests::TwoTests/testOk failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
    assert_eq!(
        store::disagreements(&r.cache),
        ["Tests/TwoTests::TwoTests/testOk"]
    );
}

// covers: C-48
#[cfg(target_os = "macos")]
#[test]
fn a_nested_xctest_class_makes_nothing_be_left_out_on_macos() {
    let Some(swift) = swift() else {
        tools("Swift 6", false);
        return;
    };
    let r = Repo::new(&swift, &shop("{mode: reuse}"));
    assert_eq!(r.run(SWIFT_TEST, &[]).code, 0);
    // A nested class appears in Cart's tests, failing, as Greet's sources change: the plan, from
    // the last list, leaves Core's and Greet's targets out; SwiftPM then wouldn't run the nested
    // class, so hushrun's swift runs swift test again with nothing left out, and the failure shows.
    r.write(
        "Tests/CartTests/Nested.swift",
        "import XCTest\n\nenum Outer {\n    final class InnerTests: XCTestCase {\n        func testInner() { XCTAssertTrue(ProcessInfo.processInfo.environment[\"NESTED_FAIL\"] == nil) }\n    }\n}\n",
    );
    let again = r.run(SWIFT_TEST, &[("NESTED_FAIL", "1")]);
    assert_eq!(again.code, 1, "{}\n{}", again.stdout, again.stderr);
    assert!(
        again.stderr.contains(
            "hushrun: swift test runs again with nothing left out: on macOS SwiftPM doesn't run a nested XCTest class (CartTests.Outer.InnerTests) once a test is left out"
        ),
        "{}",
        again.stderr
    );
    assert!(
        again.stdout.contains("InnerTests testInner]' failed"),
        "{}",
        again.stdout
    );
    // The list now holds the nested class: the next plan leaves nothing out.
    r.write("Tests/CartTests/Nested.swift", "import XCTest\n\nenum Outer {\n    final class InnerTests: XCTestCase {\n        func testInner() {}\n    }\n}\n");
    let after = r.run(SWIFT_TEST, &[]);
    assert_eq!(after.code, 0, "{}", after.stderr);
    assert_eq!(
        after.line("hushrun: unit: every test target runs"),
        "hushrun: unit: every test target runs: on macOS SwiftPM doesn't run a nested XCTest class (Outer.InnerTests, in Tests/CartTests) once any test is left out, so none is"
    );
    assert!(
        after.stdout.contains("InnerTests testInner]' passed"),
        "{}",
        after.stdout
    );
}

// covers: C-48
#[cfg(target_os = "macos")]
#[test]
fn with_no_list_kept_a_plan_leaves_out_and_the_list_after_the_run_catches_a_nested_class() {
    let Some(swift) = swift() else {
        tools("Swift 6", false);
        return;
    };
    let r = Repo::new(&swift, &shop("{mode: reuse}"));
    assert_eq!(r.run(SWIFT_TEST, &[]).code, 0);
    // This machine keeps no run's list now, as a fresh one with hints wouldn't: the plan leaves
    // Core's and Greet's targets out all the same.
    store::forget_listed(&r.cache);
    r.write(
        "Tests/CartTests/Nested.swift",
        "import XCTest\n\nenum Outer {\n    final class InnerTests: XCTestCase {\n        func testInner() { XCTAssertTrue(ProcessInfo.processInfo.environment[\"NESTED_FAIL\"] == nil) }\n    }\n}\n",
    );
    let again = r.run(SWIFT_TEST, &[("NESTED_FAIL", "1")]);
    assert_eq!(again.code, 1, "{}\n{}", again.stdout, again.stderr);
    // The nested class SwiftPM skipped once a test was left out is in the list after the run:
    // swift test runs again whole, and the failure shows.
    assert!(
        again.stderr.contains(
            "hushrun: swift test runs again with nothing left out: on macOS SwiftPM doesn't run a nested XCTest class (CartTests.Outer.InnerTests) once a test is left out"
        ),
        "{}",
        again.stderr
    );
    assert!(
        again.stdout.contains("InnerTests testInner]' failed"),
        "{}",
        again.stdout
    );
}

/// A `swift` that says, for `test`, whether its standard output is a terminal, in color, and the
/// `PATH` it got; lists one test; and exits 3.
#[cfg(unix)]
fn fake_swift(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).unwrap();
    let script = "#!/bin/sh\ncase \"$1 $2\" in\n\"--version \"*) echo 'Swift version 6.4.2 (fake)' ;;\n\"test list\") echo 'T.C/testA' ;;\n\"test \"*) if [ -t 1 ]; then printf '\\033[1mterminal\\033[0m\\nnext\\n'; else echo pipe; fi; echo \"path:$PATH\"; exit 3 ;;\nesac\n";
    let path = dir.join("swift");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// covers: C-48
#[cfg(unix)]
#[test]
fn hushruns_swift_passes_output_through_a_terminal_byte_for_byte() {
    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use std::io::Read as _;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let real = root.join("real");
    fake_swift(&real);
    // A directory of programs, as a run makes it, first on PATH; PATH as COMMAND had it after.
    let programs = root.join("programs");
    std::fs::create_dir_all(&programs).unwrap();
    std::fs::write(programs.join(".hushrun-programs"), "").unwrap();
    let was = format!("{}:/usr/bin:/bin", real.display());
    let units = root.join("units");
    std::fs::create_dir_all(&units).unwrap();
    let command = |stdout: std::process::Stdio| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.args(["__hushrun-program", "swift", "test", "--parallel"])
            .current_dir(&root)
            .env("PATH", format!("{}:{was}", programs.display()))
            .env("HUSHRUN_PROGRAMS_PATH", format!("={was}"))
            .env("HUSHRUN_UNITS", &units)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HUSHRUN_HOST", NO_HOST)
            .stdin(std::process::Stdio::null())
            .stdout(stdout);
        cmd
    };
    // Through a pipe: what swift wrote, and its exit code.
    let piped = command(std::process::Stdio::piped()).output().unwrap();
    assert_eq!(piped.status.code(), Some(3));
    let text = String::from_utf8_lossy(&piped.stdout).into_owned();
    assert!(text.starts_with("pipe\npath:"), "{text}");
    // What the real swift got: PATH as COMMAND had it, so a swift a test starts isn't hushrun's.
    assert_eq!(text.trim_end(), format!("pipe\npath:{was}"));
    // On a terminal in raw mode: swift sees a terminal too, and its bytes arrive as written.
    let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&controller).unwrap();
    unlockpt(&controller).unwrap();
    let name = ptsname(&controller, Vec::new()).unwrap();
    let terminal = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    let mut modes = rustix::termios::tcgetattr(&terminal).unwrap();
    modes.make_raw();
    rustix::termios::tcsetattr(&terminal, rustix::termios::OptionalActions::Now, &modes).unwrap();
    let mut cmd = command(std::fs::File::from(terminal).into());
    let mut child = cmd.spawn().unwrap();
    drop(cmd);
    let mut controller = std::fs::File::from(controller);
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0; 4096];
        while let Ok(n @ 1..) = controller.read(&mut buf) {
            out.extend_from_slice(&buf[..n]);
        }
        out
    });
    let status = child.wait().unwrap();
    let out = reader.join().unwrap();
    assert_eq!(status.code(), Some(3));
    assert!(
        out.starts_with(b"\x1b[1mterminal\x1b[0m\nnext\npath:"),
        "{:?}",
        String::from_utf8_lossy(&out)
    );
    // Its facts: the one test listed, which nothing says ran.
    let facts: Vec<Value> = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .collect();
    assert_eq!(facts.len(), 2, "{facts:?}");
    assert!(facts.iter().all(|f| f["units"][0]["tests"][0]["finished"] == false && f["failuresByUnit"] == false), "{facts:?}");
}

/// A `swift` for hushrun's own `swift` alone: a package with one test target, `ATests`, whose
/// `test list` exits 1 when `LIST_FAILS` is set; `test` prints its arguments and exits 0.
#[cfg(unix)]
fn fake_package_swift(dir: &Path, root: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).unwrap();
    let describe = json!({"name": "Fake", "path": root, "dependencies": [], "products": [],
        "targets": [{"name": "ATests", "c99name": "ATests", "type": "test", "path": "Tests/ATests"}]});
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in\n*--version*) echo 'Swift version 6.4.2 (fake)' ;;\n\
         *describe*) echo '{describe}' ;;\n*dump-package*) echo '{{\"targets\": []}}' ;;\n\
         'test list'*) [ -n \"$LIST_FAILS\" ] && exit 1; echo 'ATests.ATests/testA' ;;\n\
         test*) echo \"args:$*\" ;;\nesac\n"
    );
    let path = dir.join("swift");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// covers: C-48, C-32
#[cfg(unix)]
#[test]
fn a_list_after_the_run_that_fails_where_tests_were_left_out_runs_again_whole() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(root.join("Tests/ATests")).unwrap();
    let real = root.join("real");
    fake_package_swift(&real, &root);
    let was = format!("{}:/usr/bin:/bin", real.display());
    let units = root.join("units");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(
        units.join("hushrun.json"),
        json!({"schema": "hushrun.units/v0", "root": root, "reuse": ["Tests/ATests"], "unit": "file"})
            .to_string(),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_hushrun"))
        .args(["__hushrun-program", "swift", "test"])
        .current_dir(&root)
        .env("PATH", &was)
        .env("HUSHRUN_PROGRAMS_PATH", format!("={was}"))
        .env("HUSHRUN_UNITS", &units)
        .env("HUSHRUN_NO_UPDATE_CHECK", "1")
        .env("HUSHRUN_HOST", NO_HOST)
        .env("LIST_FAILS", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("--skip ^ATests\\."), "{stdout}");
    let facts: Vec<Value> = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("swift-"))
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .collect();
    assert_eq!(facts.len(), 1, "{facts:?}");
    // Each hushrun's swift is a start of its own (C-32).
    assert!(facts[0]["start"].is_string(), "{:#}", facts[0]);
    // What SwiftPM ran of what hushrun left out can't be checked: the run starts COMMAND once
    // more with nothing left out.
    assert!(
        facts[0]["leftOutUnlisted"]
            .as_str()
            .is_some_and(|w| w.contains("swift test list")),
        "{:#}",
        facts[0]
    );
}

// covers: C-48, C-32
#[test]
fn a_skip_in_a_run_that_failed_records_nothing() {
    let Some(swift) = swift() else {
        tools("Swift 6", false);
        return;
    };
    let files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(".gitignore", ".build/\n"),
        file(
            "Package.swift",
            "// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: \"Skips\", targets: [\n  .testTarget(name: \"XSkipTests\"),\n  .testTarget(name: \"TSkipTests\"),\n  .testTarget(name: \"FailTests\"),\n  .testTarget(name: \"OkTests\"),\n])\n",
        ),
        file(
            "Tests/XSkipTests/XSkipTests.swift",
            "import XCTest\n\nfinal class XSkipTests: XCTestCase {\n    func testOk() {}\n    func testSkip() throws { throw XCTSkip(\"not here\") }\n}\n",
        ),
        file(
            "Tests/TSkipTests/TSkipTests.swift",
            "import Testing\n\n@Test func ok() {}\n@Test(.disabled(\"off\")) func off() {}\n",
        ),
        file(
            "Tests/FailTests/FailTests.swift",
            "import XCTest\n\nfinal class FailTests: XCTestCase {\n    func testFails() { XCTFail(\"no\") }\n}\n",
        ),
        file(
            "Tests/OkTests/OkTests.swift",
            "import XCTest\n\nfinal class OkTests: XCTestCase {\n    func testOk() {}\n}\n",
        ),
    ];
    let r = Repo::new(&swift, &files);
    let ran = r.run(SWIFT_TEST, &[]);
    assert_ne!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    // FailTests failed and OkTests passed; the two targets with a skipped test record no pass,
    // since a skip in a run that failed proves nothing.
    let mut results = Vec::new();
    collect(&r.cache.join("results/units/v0"), &mut results);
    let mut results: Vec<String> = results
        .iter()
        .map(|p| {
            let v: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            v["result"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    results.sort();
    assert_eq!(results, ["fail", "pass"], "{}", ran.stderr);
}

/// A `swift` for a whole run: a package whose `CoreTests` depends on `Extra` only when the
/// environment has `WITH_EXTRA`, as a manifest that reads `Context.environment` may say, and
/// whose XCTest tests print as macOS prints them, but those a `--skip` leaves out.
/// With `LOUD`, it prints 17 MiB before its tests: of other lines (`junk`), or of `Test Case`
/// lines of no test (`cases`).
#[cfg(unix)]
fn fake_env_swift(dir: &Path, root: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).unwrap();
    let target = |name: &str, kind: &str, path: &str, deps: &str| {
        format!(
            r#"{{"name": "{name}", "c99name": "{name}", "type": "{kind}", "path": "{path}", "target_dependencies": [{deps}]}}"#
        )
    };
    let describe = |core_deps: &str| {
        format!(
            r#"{{"name": "Fake", "path": "{}", "dependencies": [], "products": [], "targets": [{}, {}, {}, {}, {}]}}"#,
            root.display(),
            target("CoreTests", "test", "Tests/CoreTests", core_deps),
            target("GreetTests", "test", "Tests/GreetTests", r#""Greet""#),
            target("Core", "library", "Sources/Core", ""),
            target("Extra", "library", "Sources/Extra", ""),
            target("Greet", "library", "Sources/Greet", ""),
        )
    };
    let dump = r#"{"targets": [{"name": "CoreTests", "settings": []}, {"name": "GreetTests", "settings": []}, {"name": "Core", "settings": []}, {"name": "Extra", "settings": []}, {"name": "Greet", "settings": []}]}"#;
    let test = |target: &str, method: &str| {
        format!(
            "case \"$*\" in *'^{target}'*) ;; *) echo \"Test Case '-[{target}.{target} {method}]' started.\"; echo \"Test Case '-[{target}.{target} {method}]' passed (0.001 seconds).\" ;; esac\n"
        )
    };
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in\n*--version*) echo 'Swift version 6.4.2 (fake)'; exit 0 ;;\n\
         *describe*) if [ -n \"$WITH_EXTRA\" ]; then echo '{with}'; else echo '{without}'; fi; exit 0 ;;\n\
         *dump-package*) echo '{dump}'; exit 0 ;;\n\
         'test list'*) printf 'CoreTests.CoreTests/testA\\nGreetTests.GreetTests/testB\\n'; exit 0 ;;\n\
         esac\n\
         case \"$LOUD\" in\n\
         junk) yes 'a loud test prints this' | head -c 17825792; echo ;;\n\
         cases) yes \"Test Case '-[Noise.Noise testN]' started.\" | head -c 17825792; echo ;;\n\
         esac\n{a}{b}",
        with = describe(r#""Core", "Extra""#),
        without = describe(r#""Core""#),
        a = test("CoreTests", "testA"),
        b = test("GreetTests", "testB"),
    );
    let path = dir.join("swift");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// covers: C-48, C-32
#[cfg(unix)]
#[test]
fn the_tools_hushrun_starts_for_swift_get_commands_own_env_assignments() {
    let files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file("Package.swift", "// swift-tools-version: 6.0\n"),
        file("Sources/Core/Core.swift", "public let core = 1\n"),
        file("Sources/Extra/Extra.swift", "public let extra = 1\n"),
        file("Sources/Greet/Greet.swift", "public let greet = 1\n"),
        file("Tests/CoreTests/CoreTests.swift", "// a\n"),
        file("Tests/GreetTests/GreetTests.swift", "// b\n"),
    ];
    let dir = tempfile::tempdir().unwrap();
    let fake = std::fs::canonicalize(dir.path()).unwrap().join("swift");
    let r = Repo::new(&fake, &files);
    fake_env_swift(&fake.parent().unwrap().join("bin"), &r.repo);
    let fake = fake.parent().unwrap().join("bin/swift");
    let r = Repo {
        path: std::env::join_paths([
            fake.parent().unwrap(),
            Path::new("/usr/bin"),
            Path::new("/bin"),
        ])
        .unwrap(),
        ..r
    };
    let command = ["run", "unit", "--", "env", "WITH_EXTRA=1", "swift", "test"];
    let first = r.run(&command, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // `swift package describe`, which the plan starts before COMMAND, sees the WITH_EXTRA COMMAND
    // sets: CoreTests reads Extra, and runs when Extra changes.
    r.write("Sources/Extra/Extra.swift", "public let extra = 2\n");
    let second = r.run(&command, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        second.line("hushrun: unit: "),
        "hushrun: unit: all 2 test targets run; 1 that already passed on the files it read runs anyway, as a check"
    );
}

// covers: C-48, C-37
#[cfg(unix)]
#[test]
fn a_loud_swift_test_is_recorded_and_past_what_hushrun_reads_says_why() {
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file("Package.swift", "// swift-tools-version: 6.0\n"),
        file("Sources/Core/Core.swift", "public let core = 1\n"),
        file("Sources/Extra/Extra.swift", "public let extra = 1\n"),
        file("Sources/Greet/Greet.swift", "public let greet = 1\n"),
        file("Tests/CoreTests/CoreTests.swift", "// a\n"),
        file("Tests/GreetTests/GreetTests.swift", "// b\n"),
    ];
    let dir = tempfile::tempdir().unwrap();
    let fake = std::fs::canonicalize(dir.path()).unwrap().join("swift");
    let r = Repo::new(&fake, &files);
    fake_env_swift(&fake.parent().unwrap().join("bin"), &r.repo);
    let fake = fake.parent().unwrap().join("bin/swift");
    let r = Repo {
        path: std::env::join_paths([
            fake.parent().unwrap(),
            Path::new("/usr/bin"),
            Path::new("/bin"),
        ])
        .unwrap(),
        ..r
    };
    // 17 MiB of `Test Case` lines: past what hushrun reads, nothing of the tests is recorded,
    // and the run says why once, as `hushrun why` does of each test.
    let cut = r.run(
        &["run", "unit", "--", "swift", "test"],
        &[("LOUD", "cases")],
    );
    assert_eq!(cut.code, 0, "{}", cut.stderr);
    let why = "swift test printed 17.0 MiB of `Test Case` lines, more than the 16 MiB hushrun reads: none of its tests is recorded";
    let said: Vec<&str> = cut.stderr.lines().filter(|l| l.contains(why)).collect();
    assert_eq!(said, [format!("hushrun: unit: {why}")], "{}", cut.stderr);
    let asked = r.run(
        &[
            "why",
            "unit",
            "Tests/CoreTests::CoreTests/testA",
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(asked.code, 0, "{}\n{}", asked.stdout, asked.stderr);
    let asked: Value = serde_json::from_str(&asked.stdout).unwrap();
    assert_eq!(
        asked["says"],
        format!(
            "Tests/CoreTests::CoreTests/testA runs: hushrun couldn't follow all it read the last time it ran here: output ({why})"
        ),
        "{asked:#}"
    );
    // 17 MiB of what a test printed, then its `Test Case` lines: hushrun keeps those as they go
    // by, and the tests are recorded.
    let loud = r.run(
        &["run", "--force", "unit", "--", "swift", "test"],
        &[("LOUD", "junk")],
    );
    assert_eq!(loud.code, 0, "{}", loud.stderr);
    assert_eq!(
        r.map()["units"]["Tests/CoreTests"]["tests"],
        json!({"CoreTests/testA": "passed"}),
        "{}",
        loud.stderr
    );
}

// covers: C-48, C-30
#[cfg(unix)]
#[test]
fn a_swift_file_no_test_target_reads_is_in_the_rest_set() {
    let files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file("Package.swift", "// swift-tools-version: 6.0\n"),
        file("Sources/Core/Core.swift", "public let core = 1\n"),
        file("Sources/Extra/Extra.swift", "public let extra = 1\n"),
        file("Sources/Greet/Greet.swift", "public let greet = 1\n"),
        file("Tests/CoreTests/CoreTests.swift", "// a\n"),
        file("Tests/GreetTests/GreetTests.swift", "// b\n"),
        file("Fixtures/case.swift", "let fixture = 1\n"),
        file("Examples/Demo/main.swift", "print(1)\n"),
    ];
    let dir = tempfile::tempdir().unwrap();
    let fake = std::fs::canonicalize(dir.path()).unwrap().join("swift");
    let r = Repo::new(&fake, &files);
    fake_env_swift(&fake.parent().unwrap().join("bin"), &r.repo);
    let fake = fake.parent().unwrap().join("bin/swift");
    let r = Repo {
        path: std::env::join_paths([
            fake.parent().unwrap(),
            Path::new("/usr/bin"),
            Path::new("/bin"),
        ])
        .unwrap(),
        ..r
    };
    let first = r.run(SWIFT_TEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // Extra is a target no test target depends on here, and Fixtures and Examples are below no
    // target: a test may read or build them at run time, so they are in the rest set and a change
    // to one runs every test target. What a test target reads stays a source.
    let map = r.map();
    for path in [
        "Sources/Extra/Extra.swift",
        "Fixtures/case.swift",
        "Examples/Demo/main.swift",
    ] {
        assert!(map["rest"][path].is_string(), "{path}: {map:#}");
    }
    assert!(map["rest"]["Sources/Core/Core.swift"].is_null(), "{map:#}");
    r.write("Sources/Extra/Extra.swift", "public let extra = 2\n");
    let second = r.run(SWIFT_TEST, &[]);
    assert!(
        second
            .line("hushrun: unit: ")
            .starts_with("hushrun: unit: all 2 test targets run"),
        "{}",
        second.stderr
    );
}

// covers: C-48
#[cfg(unix)]
#[test]
fn a_program_a_test_left_running_keeps_printing_after_hushruns_swift_returns() {
    use std::io::Read as _;
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let real = root.join("real");
    std::fs::create_dir_all(&real).unwrap();
    // `swift test` passes its one test, and leaves a program running that prints a line 2 s
    // after it exited.
    let script = "#!/bin/sh\ncase \"$1 $2\" in\n\"--version \"*) echo 'Swift version 6.4.2 (fake)' ;;\n\"test list\") echo 'T.C/testA' ;;\n\"test \"*) echo \"Test Case '-[T.C testA]' started.\"; echo \"Test Case '-[T.C testA]' passed (0.001 seconds).\"; (sleep 2; echo late line) & exit 3 ;;\nesac\n";
    std::fs::write(real.join("swift"), script).unwrap();
    std::fs::set_permissions(real.join("swift"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let was = format!("{}:/usr/bin:/bin", real.display());
    let units = root.join("units");
    std::fs::create_dir_all(&units).unwrap();
    let started = std::time::Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hushrun"))
        .args(["__hushrun-program", "swift", "test"])
        .current_dir(&root)
        .env("PATH", &was)
        .env("HUSHRUN_PROGRAMS_PATH", format!("={was}"))
        .env("HUSHRUN_UNITS", &units)
        .env("HUSHRUN_NO_UPDATE_CHECK", "1")
        .env("HUSHRUN_HOST", NO_HOST)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        out
    });
    let status = child.wait().unwrap();
    let returned = started.elapsed();
    // hushrun's swift returns with swift's code as swift does, without waiting for that program.
    assert_eq!(status.code(), Some(3));
    assert!(returned < std::time::Duration::from_secs(2), "{returned:?}");
    // What it prints later still comes, once that program ends.
    let out = reader.join().unwrap();
    assert!(out.contains("passed") && out.contains("late line"), "{out}");
    // No pass is recorded of a run whose output was still held, and the facts say why.
    let facts: Vec<Value> = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .collect();
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(
        facts[0]["units"][0]["tests"][0]["notFollowed"][0]["why"], "output",
        "{:#}",
        facts[0]
    );
    assert!(facts[0]["settings"]["held"].is_string(), "{:#}", facts[0]);
}

// covers: C-48
#[cfg(unix)]
#[test]
fn a_signal_after_swift_ended_ends_hushruns_swift() {
    use hushrun_testkit::env::Hermetic as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::ExitStatusExt as _;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let real = root.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let marker = root.join("listing.pid");
    // `swift test` ends at once; the list after it takes a while, and says when it started.
    let script = format!(
        "#!/bin/sh\ncase \"$1 $2\" in\n\"--version \"*) echo 'Swift version 6.4.2 (fake)' ;;\n\"test list\") echo $$ > '{}'; exec sleep 20 ;;\n\"test \"*) exit 0 ;;\nesac\n",
        marker.display()
    );
    std::fs::write(real.join("swift"), script).unwrap();
    std::fs::set_permissions(real.join("swift"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let was = format!("{}:/usr/bin:/bin", real.display());
    let units = root.join("units");
    std::fs::create_dir_all(&units).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hushrun"))
        // What the developer's shell exports never reaches a runner a test starts.
        .hermetic()
        .args(["__hushrun-program", "swift", "test"])
        .current_dir(&root)
        .env("PATH", &was)
        .env("HUSHRUN_PROGRAMS_PATH", format!("={was}"))
        .env("HUSHRUN_UNITS", &units)
        .env("HUSHRUN_NO_UPDATE_CHECK", "1")
        .env("HUSHRUN_HOST", NO_HOST)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let waited = std::time::Instant::now();
    while !marker.exists() && waited.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let listing = std::fs::read_to_string(&marker).unwrap_or_default();
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    let sent = std::time::Instant::now();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    let status = child.wait().unwrap();
    let took = sent.elapsed();
    if let Some(pid) = listing
        .trim()
        .parse()
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
    // swift had ended: the signal's own action ends hushrun's swift, as it would any program.
    assert_eq!(status.signal(), Some(15), "{status:?}");
    assert!(took < std::time::Duration::from_secs(5), "{took:?}");
}

// covers: C-48
#[test]
fn a_target_built_with_a_local_packages_plugin_reads_its_package_whole_end_to_end() {
    let Some(swift) = swift() else {
        tools("Swift 6", false);
        return;
    };
    let files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(".gitignore", ".build/\n"),
        file(
            "Package.swift",
            "// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: \"Root\", dependencies: [.package(path: \"LocalPlug\")], targets: [\n  .target(name: \"Lib\", plugins: [.plugin(name: \"P\", package: \"LocalPlug\")]),\n  .target(name: \"Other\"),\n  .testTarget(name: \"LibTests\", dependencies: [\"Lib\"]),\n])\n",
        ),
        file("Sources/Lib/Lib.swift", "public let lib = 1\n"),
        file("Sources/Other/Other.swift", "public let other = 1\n"),
        file(
            "Tests/LibTests/LibTests.swift",
            "import XCTest\n@testable import Lib\n\nfinal class LibTests: XCTestCase {\n    func testLib() { XCTAssertEqual(lib, 1) }\n}\n",
        ),
        file(
            "LocalPlug/Package.swift",
            "// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: \"LocalPlug\", products: [.plugin(name: \"P\", targets: [\"P\"])], targets: [.plugin(name: \"P\", capability: .buildTool())])\n",
        ),
        file(
            "LocalPlug/Plugins/P/plugin.swift",
            "import PackagePlugin\n\n@main struct P: BuildToolPlugin {\n    func createBuildCommands(context: PluginContext, target: Target) throws -> [Command] { [] }\n}\n",
        ),
    ];
    let r = Repo::new(&swift, &files);
    let first = r.run(SWIFT_TEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    let map = r.map();
    let reads: Vec<String> = map["units"]["Tests/LibTests"]["reads"]
        .as_array()
        .unwrap_or_else(|| panic!("{map:#}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    // Lib is built with LocalPlug's plugin: its package is read whole, a target no test depends
    // on included, and so is the plugin's own code.
    for path in [
        "Sources/Other/Other.swift",
        "LocalPlug/Plugins/P/plugin.swift",
        "Sources/Lib/Lib.swift",
    ] {
        assert!(reads.contains(&path.to_string()), "{path}: {reads:?}");
    }
}
