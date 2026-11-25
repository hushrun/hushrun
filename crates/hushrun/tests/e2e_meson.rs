//! \[ADR 0119\] Test programs with `meson test` (C-102 to C-105), end to end: a unit is a test
//! program, a test one Meson test, what a program read is what its build folder says it is built
//! from, and hushrun's `meson` on `PATH` leaves out only what it was told, after checking it again.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): hushrun's `meson` on COMMAND's `PATH` is the `hushrun` binary itself
//! (`units::program`), so every run the adapter observes starts it, and reaches the whole binary.
//! What needs no helper is in-process, in `units_meson.rs`. The fixtures are `common/meson.rs`'s:
//! generated Meson projects of plain C, which need Meson and Ninja.

#![cfg(unix)]

#[path = "common/meson.rs"]
mod fixtures;
#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;

use fixtures::{shop, tools};
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
    "MESON_TESTTHREADS",
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
    "MAKEFILES",
    "RUSTC_WRAPPER",
    "CC",
    "CXX",
];

const NO_HOST: &str = "http://127.0.0.1:9";

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    path: std::ffi::OsString,
}

impl Project {
    /// A repository of `files`, set up by Meson in `build`.
    fn new(bin: &[PathBuf], files: &[(&str, String)]) -> Project {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let p = Project {
            _dir: dir,
            cache: root.join("cache"),
            root,
            repo,
            path: fixtures::path(bin),
        };
        for (path, text) in files {
            p.write(path, text);
        }
        git::commit_all(&p.repo, "init");
        let out = p.tool("meson").args(["setup", "build"]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        p
    }

    fn tool(&self, name: &str) -> Command {
        let mut cmd = Command::new(name);
        cmd.current_dir(&self.repo).env("PATH", &self.path);
        for var in CLEARED {
            cmd.env_remove(var);
        }
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        cmd
    }

    /// Writes a file of the repository in a later second than any before: Ninja compares times.
    fn write(&self, path: &str, text: &str) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn run(&self, args: &[&str]) -> Ran {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("PATH", &self.path)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        for var in CLEARED {
            cmd.env_remove(var);
        }
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
        let output = cmd.args(args).output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// The newest result this machine recorded for the check.
    fn newest_result(&self) -> Value {
        let mut files = Vec::new();
        collect(&self.cache.join("results/results/v0"), &mut files);
        files.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
        serde_json::from_slice(&std::fs::read(files.last().expect("a result")).unwrap()).unwrap()
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
    fn text(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }

    /// The tests `meson test` says it ran: its `1/4 name OK` lines.
    fn ran(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .stdout
            .lines()
            .filter_map(|l| {
                let (count, rest) = l.trim_start().split_once(' ')?;
                let (a, b) = count.split_once('/')?;
                (!a.is_empty() && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit()))
                    .then_some(())?;
                Some(rest.split_whitespace().next()?.to_string())
            })
            .collect();
        out.sort();
        out
    }
}

const MESON_TEST: &[&str] = &["run", "unit", "--", "meson", "test", "-C", "build"];

// covers: C-102, C-103, C-104, C-105
#[test]
fn programs_are_observed_then_reused_and_a_header_runs_only_the_programs_built_from_it() {
    let Some(bin) = tools() else { return };
    let p = Project::new(&bin, &shop(""));
    let first = p.run(MESON_TEST);
    assert_eq!(first.code, 0, "{}", first.text());
    assert_eq!(first.ran(), ["cart_empty", "cart_total", "greet", "price"]);
    // What each program read is what its build folder says it is built from.
    let map = store::map(&p.cache);
    assert_eq!(
        map["units"]["t_price (debug)"]["reads"],
        json!(["src/price.c", "src/price.h", "tests/t_price.c"]),
        "{map:#}"
    );
    assert_eq!(
        map["units"]["t_cart (debug)"]["reads"],
        json!(["src/cart.h", "src/price.c", "src/price.h", "tests/t_cart.c"]),
        "{map:#}"
    );
    assert_eq!(
        map["units"]["t_greet (debug)"]["reads"],
        json!(["src/greet.h", "tests/t_greet.c"]),
        "{map:#}"
    );
    assert!(map["rest"]["meson.build"].is_string(), "{map:#}");

    // greet.h changes: t_greet runs (meson test builds it again first); of the two others, which
    // already passed on the files they read, at most one runs as a check.
    p.write(
        "src/greet.h",
        "static const char *greet(void) { return \"ho\"; }\n",
    );
    let second = p.run(MESON_TEST);
    assert_eq!(second.code, 0, "{}", second.text());
    let ran = second.ran();
    assert!(ran.contains(&"greet".to_string()), "{ran:?}");
    let result = p.newest_result();
    assert_eq!(result["units"]["total"], json!(3), "{result:#}");
    let reused = result["units"]["reused"].as_u64().unwrap();
    assert!((1..=2).contains(&reused), "{result:#}");
    // A program reused leaves out every one of its tests, by name.
    if reused == 2 {
        assert_eq!(ran, ["greet"]);
    }
    // And the same code again is the check's own pass.
    let third = p.run(MESON_TEST);
    assert!(
        third.stderr.contains("this exact code passed"),
        "{}",
        third.text()
    );
}

// covers: C-105
#[test]
fn a_failing_test_fails_its_program_and_runs_again_next_time() {
    let Some(bin) = tools() else { return };
    let p = Project::new(&bin, &shop(""));
    let first = p.run(MESON_TEST);
    assert_eq!(first.code, 0, "{}", first.text());
    // price.c changes and its test fails: its program failed; the next run runs it again.
    p.write(
        "src/price.c",
        "#include \"price.h\"\nint price(int x) { return x * 3; }\n",
    );
    let second = p.run(MESON_TEST);
    assert_ne!(second.code, 0, "{}", second.text());
    let third = p.run(MESON_TEST);
    assert_ne!(third.code, 0, "{}", third.text());
    assert!(
        third.ran().contains(&"price".to_string()),
        "{}",
        third.text()
    );
}

// covers: C-102, C-103
#[test]
fn a_test_that_starts_a_program_not_built_here_is_not_observed_and_always_runs() {
    let Some(bin) = tools() else { return };
    let p = Project::new(
        &bin,
        &shop("sh = find_program('sh')\ntest('shell', sh, args: ['-c', 'exit 0'])\n"),
    );
    let first = p.run(MESON_TEST);
    assert_eq!(first.code, 0, "{}", first.text());
    p.write(
        "src/greet.h",
        "static const char *greet(void) { return \"ho\"; }\n",
    );
    let second = p.run(MESON_TEST);
    assert_eq!(second.code, 0, "{}", second.text());
    assert!(
        second.ran().contains(&"shell".to_string()),
        "{}",
        second.text()
    );
}

// covers: C-104
#[test]
fn a_program_built_again_after_the_plan_is_not_left_out() {
    let Some(bin) = tools() else { return };
    let p = Project::new(&bin, &shop(""));
    let first = p.run(MESON_TEST);
    assert_eq!(first.code, 0, "{}", first.text());
    // A shell line that builds between the plan and meson test: every program built again
    // after the plan runs its tests, whatever its key.
    p.write(
        "src/greet.h",
        "static const char *greet(void) { return \"ho\"; }\n",
    );
    let line =
        "touch src/price.c tests/t_cart.c && ninja -C build >/dev/null && meson test -C build";
    let second = p.run(&["run", "unit", "--", "sh", "-c", line]);
    assert_eq!(second.code, 0, "{}", second.text());
    assert_eq!(
        second.ran(),
        ["cart_empty", "cart_total", "greet", "price"],
        "{}",
        second.text()
    );
}
