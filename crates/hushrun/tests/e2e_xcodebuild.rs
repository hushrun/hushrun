//! \[ADR 0121\] Test targets and tests with `xcodebuild test` (C-110 to C-113), end to end: a
//! unit is a test target of an Xcode project, what it read is what the project says it and the
//! targets it depends on are built from, a test is its id in the result bundle, and reuse never
//! misses a failure.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): hushrun's `xcodebuild` on COMMAND's `PATH` is the `hushrun` binary itself
//! (`units::program`), so every run the adapter observes starts it, and reaches the whole binary.
//! What needs no helper is in-process, in `units_xcodebuild.rs`. The fixtures are
//! `common/xcodebuild.rs`'s, and need macOS with Xcode 16 or later.

#[path = "common/xcodebuild.rs"]
mod fixtures;
#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;

use fixtures::*;
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
    "HUSHRUN_CC",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "RUSTC_WRAPPER",
    "FAIL",
    "HIDDEN",
    "CRASH",
    "NESTED_FAIL",
];

const NO_HOST: &str = "http://127.0.0.1:9";

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    /// `PATH` with the test's `xcodebuild` first.
    path: std::ffi::OsString,
}

impl Repo {
    fn new(xcodebuild: &Path, files: &[(String, String)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::fs::create_dir_all(root.join("home")).expect("home");
        git::init(&repo, "sha1");
        let r = Repo {
            _dir: dir,
            cache: root.join("cache"),
            path: fixtures::path(xcodebuild),
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

    /// Whether XCTest ran `method` of the class `class`, which is also its target's name, as
    /// xcodebuild's console says: `Test Case '-[LibTests.LibTests testLib]' started.`
    fn ran(&self, class: &str, method: &str) -> bool {
        self.stdout
            .contains(&format!("Test Case '-[{class}.{class} {method}]' started"))
    }
}

fn xcodebuild_test(r: &Repo) -> Vec<String> {
    let mut args: Vec<String> = ["run", "unit", "--"].map(String::from).to_vec();
    args.extend(test_command(&r.root.join("dd")));
    args
}

fn run_test(r: &Repo, env: &[(&str, &str)]) -> Ran {
    let args = xcodebuild_test(r);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    r.run(&args, env)
}

// covers: C-110, C-111, C-112
#[test]
fn test_targets_are_observed_then_reused_and_a_library_change_runs_those_that_read_it() {
    let Some(xcodebuild) = xcodebuild() else {
        // xcodebuild is macOS's alone: elsewhere there is nothing to miss.
        tools("Xcode 16", !cfg!(target_os = "macos"));
        return;
    };
    let r = Repo::new(&xcodebuild, &project("{mode: reuse, ignore: ['notes/**']}"));
    let first = run_test(&r, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.line("hushrun: unit: all "),
        "hushrun: unit: all 2 test targets run: none had passed here yet on the files it read"
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
    assert_eq!(
        reads("ios/Lib.xcodeproj/LibTests"),
        [
            "ios/Lib/Lib.swift",
            "ios/Lib/One.swift",
            "ios/LibTests/LibTests.swift"
        ]
    );
    assert_eq!(
        reads("ios/Lib.xcodeproj/OtherTests"),
        ["ios/OtherTests/OtherTests.swift"]
    );
    assert!(
        map["rest"]["ios/Lib.xcodeproj/project.pbxproj"].is_string(),
        "{map:#}"
    );
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 2, "ran": 2, "reused": 0})
    );

    // Lib changes: LibTests runs; OtherTests is reused, unless the audit draws it.
    r.write("ios/Lib/One.swift", "func one() -> Int { 2 - 1 }\n");
    let second = run_test(&r, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert!(second.ran("LibTests", "testLib"), "{}", second.stdout);
    let units = r.newest_result()["units"].clone();
    let other = second.ran("OtherTests", "testAlone");
    assert_eq!(
        units,
        json!({"total": 2, "ran": if other { 2 } else { 1 }, "reused": if other { 0 } else { 1 }}),
        "{}",
        second.stderr
    );
}

// covers: C-110, C-112, C-113
#[test]
fn with_tests_as_the_unit_only_the_test_that_failed_runs_again() {
    let Some(xcodebuild) = xcodebuild() else {
        // xcodebuild is macOS's alone: elsewhere there is nothing to miss.
        tools("Xcode 16", !cfg!(target_os = "macos"));
        return;
    };
    let r = Repo::new(&xcodebuild, &project("{mode: reuse, unit: test}"));
    let first = run_test(&r, &[("TEST_RUNNER_FAIL", "1")]);
    assert_ne!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    let map = r.map();
    let tests = &map["units"]["ios/Lib.xcodeproj/LibTests"]["tests"];
    assert_eq!(tests["LibTests/testFlips()"], "failed", "{map:#}");
    assert_eq!(tests["LibTests/testLib()"], "passed", "{map:#}");
    assert_eq!(tests["LibSuite/swiftTesting()"], "passed", "{map:#}");
    assert_eq!(
        map["units"]["ios/Lib.xcodeproj/OtherTests"]["tests"]["OtherTests/testAlone()"], "passed",
        "{map:#}"
    );

    // Nothing fails now: the one that failed runs, the others are left out (or one is audited).
    let second = run_test(&r, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert!(second.ran("LibTests", "testFlips"), "{}", second.stdout);
    let units = r.newest_result()["units"].clone();
    assert_eq!(units["tests"]["total"], json!(4), "{units}");
    assert!(
        units["tests"]["reused"].as_u64().unwrap() >= 2,
        "{units}\n{}",
        second.stderr
    );
    assert_eq!(
        r.map()["units"]["ios/Lib.xcodeproj/LibTests"]["tests"]["LibTests/testFlips()"],
        "passed"
    );
}
