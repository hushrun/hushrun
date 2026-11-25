//! \[ADR 0115\] Test files and tests with Bats (C-30 to C-34, C-39, C-86 to C-89): a unit is a
//! `.bats` file, a test one `@test`, and what each read is what its bash processes ran from,
//! as hushrun's hook on `BASH_ENV` noted it: the files it loaded and sourced, and the scripts it
//! ran, and what they sourced.
//!
//! End to end, every one (CONTRIBUTING.md, "Which tests CI runs again"): the hook starts the
//! `hushrun` binary itself (`HUSHRUN_BATS_EXE`) when bats starts and ends, so every run Bats'
//! adapter observes reads all of hushrun, and none of these is in-process. Each test runs the
//! binary, or the hook with the binary, as a person's run would.
//!
//! The fixtures are small shell projects, and need bats-core 1.10 or later: `HUSHRUN_TEST_BATS`
//! names the directories holding a `bats`, separated as `PATH` is, and each runs every test here;
//! without it, the `bats` on `PATH`. They say so and pass where bats is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets. No fixture needs the network.

#![cfg(unix)]

#[path = "common/store.rs"]
mod store;

use std::os::unix::fs::PermissionsExt as _;
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
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "HUSHRUN_BATS_EXE",
    "HUSHRUN_BATS_ENV_WAS",
    "HUSHRUN_BATS_RUN",
    "HUSHRUN_BATS_LOG",
    "HUSHRUN_BATS_INSIDE",
    "HUSHRUN_BATS_LIBEXEC",
    "BASH_ENV",
    "BATS_TEST_RETRIES",
    "BATS_NUMBER_OF_PARALLEL_JOBS",
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
];

const NO_HOST: &str = "http://127.0.0.1:9";

/// The directories holding the bats to run the fixtures with.
fn bats_dirs() -> Vec<PathBuf> {
    let dirs: Vec<PathBuf> = match std::env::var_os("HUSHRUN_TEST_BATS") {
        Some(list) => std::env::split_paths(&list).collect(),
        None => which("bats")
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .into_iter()
            .collect(),
    };
    let found: Vec<PathBuf> = dirs
        .into_iter()
        .filter(|d| d.join("bats").is_file())
        .collect();
    if found.is_empty() {
        if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
            panic!("bats is missing (HUSHRUN_TEST_BATS), and HUSHRUN_TEST_TOOLS=required");
        }
        eprintln!("note: skipping: bats is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    }
    found
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    path: std::ffi::OsString,
}

impl Project {
    fn new(bats: &Path, files: &[(&str, &str)]) -> Project {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let mut dirs = vec![bats.to_path_buf()];
        dirs.extend(which("bash").and_then(|b| b.parent().map(Path::to_path_buf)));
        dirs.push("/usr/bin".into());
        dirs.push("/bin".into());
        let path = std::env::join_paths(dirs).unwrap();
        let p = Project {
            _dir: dir,
            cache: root.join("cache"),
            root,
            repo,
            path,
        };
        for (path, text) in files {
            p.write(path, text);
        }
        git::commit_all(&p.repo, "init");
        p
    }

    fn write(&self, path: &str, text: &str) {
        let full = self.repo.join(path);
        std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        std::fs::write(&full, text).expect("write");
        if text.starts_with("#!") {
            std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Changes `path` and commits it.
    fn change(&self, path: &str, text: &str) {
        self.write(path, text);
        git::commit_all(&self.repo, "change");
    }

    fn hushrun(&self) -> Command {
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
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("CLAUDE")
                || name.starts_with("BATS_")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        cmd
    }

    fn run(&self, command: &[&str]) -> Ran {
        let output = self
            .hushrun()
            .args(["run", "unit", "--"])
            .args(command)
            .output()
            .expect("hushrun");
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
    /// The tests bats ran, by their names: those it didn't skip as reused.
    fn ran(&self) -> Vec<String> {
        self.tests(false)
    }

    /// The tests hushrun left out.
    fn reused(&self) -> Vec<String> {
        self.tests(true)
    }

    fn tests(&self, reused: bool) -> Vec<String> {
        let mut out: Vec<String> = self
            .stdout
            .lines()
            .filter_map(|l| {
                let rest = l
                    .strip_prefix("ok ")
                    .or_else(|| l.strip_prefix("not ok "))?;
                let name = rest.split_once(' ')?.1;
                match name.strip_suffix(" # skip reused by hushrun") {
                    Some(name) if reused => Some(name.to_string()),
                    None if !reused => Some(name.to_string()),
                    _ => None,
                }
            })
            .collect();
        out.sort();
        out
    }

    fn says(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

fn yaml(tests: &str) -> String {
    format!(
        "version: 1\nrepository: example.com/acme/shell\nchecks:\n  unit:\n    tests: {tests}\n"
    )
}

/// A shell project: a library two tests source, a script one test runs, which sources a
/// library of its own, a POSIX `sh` script one test runs, and a helper every test file loads.
fn shop(tests: &str) -> Vec<(&'static str, String)> {
    vec![
        ("hushrun.yaml", yaml(tests)),
        ("lib/price.sh", "price() { echo $(( $1 * 2 )); }\n".into()),
        ("lib/greet.bash", "greet() { echo \"hello $1\"; }\n".into()),
        (
            "bin/hello",
            "#!/usr/bin/env bash\nsource \"${BASH_SOURCE%/*}/../lib/greet.bash\"\ngreet \"$1\"\n"
                .into(),
        ),
        ("bin/posix.sh", "#!/bin/sh\necho posix\n".into()),
        (
            "test/helpers/common.bash",
            "ROOT=\"$BATS_TEST_DIRNAME/..\"\n".into(),
        ),
        (
            "test/price.bats",
            "load helpers/common\nsetup() { source \"$ROOT/lib/price.sh\"; }\n\
             @test \"doubles\" { [ \"$(price 2)\" = 4 ]; }\n\
             @test \"doubles zero\" { [ \"$(price 0)\" = 0 ]; }\n"
                .into(),
        ),
        (
            "test/hello.bats",
            "load helpers/common\n\
             @test \"says hello\" { run \"$ROOT/bin/hello\" you; [ \"$output\" = \"hello you\" ]; }\n\
             @test \"posix\" { run \"$ROOT/bin/posix.sh\"; [ \"$status\" -eq 0 ]; }\n\
             @test \"plain\" { true; }\n"
                .into(),
        ),
    ]
}

fn files<'a>(list: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    list.iter().map(|(p, t)| (*p, t.as_str())).collect()
}

// covers: C-86, C-87, C-88, C-39
#[test]
fn a_test_runs_again_only_when_a_file_its_bash_ran_from_changed() {
    for bats in bats_dirs() {
        hook_writes_facts(&bats);
        let shop = shop("{mode: reuse, unit: test, reads: test}");
        let p = Project::new(&bats, &files(&shop));
        let first = p.run(&["bats", "test"]);
        assert_eq!(first.code, 0, "{}", first.says());
        assert_eq!(first.ran().len(), 5, "{}", first.says());
        let map = store::map(&p.cache);
        let reads = &map["units"]["test/hello.bats"]["reads"];
        for file in ["bin/hello", "lib/greet.bash", "test/helpers/common.bash"] {
            assert!(
                reads.as_array().unwrap().contains(&json!(file)),
                "{file} in {map:#}"
            );
        }

        // The library a script `says hello` ran sourced: that test runs, and the audit may run
        // one more.
        p.change(
            "lib/greet.bash",
            "greet() { echo \"hello $1\"; }\n# changed\n",
        );
        let second = p.run(&["bats", "test"]);
        assert_eq!(second.code, 0, "{}", second.says());
        assert!(
            second.ran().contains(&"says hello".to_string()),
            "{}",
            second.says()
        );
        assert!(second.ran().len() <= 2, "{}", second.says());
        assert!(second.reused().len() >= 3, "{}", second.says());
        let units = &p.newest_result()["units"];
        assert_eq!(units["tests"]["total"], json!(5), "{units:#}");

        // The library a test file's setup sources: both its tests, and nothing of the other file
        // but what the audit draws.
        p.change("lib/price.sh", "price() { echo $(( $1 + $1 )); }\n");
        let third = p.run(&["bats", "test"]);
        assert_eq!(third.code, 0, "{}", third.says());
        for name in ["doubles", "doubles zero"] {
            assert!(third.ran().contains(&name.to_string()), "{}", third.says());
        }
        assert!(third.reused().len() >= 2, "{}", third.says());
    }
}

// covers: C-87
#[test]
fn a_script_bash_doesnt_run_is_in_the_rest_and_runs_everything() {
    for bats in bats_dirs() {
        hook_writes_facts(&bats);
        let shop = shop("{mode: reuse, unit: test, reads: test}");
        let p = Project::new(&bats, &files(&shop));
        assert_eq!(p.run(&["bats", "test"]).code, 0);
        p.change("bin/posix.sh", "#!/bin/sh\necho posix again\n");
        let second = p.run(&["bats", "test"]);
        assert_eq!(second.code, 0, "{}", second.says());
        assert_eq!(second.ran().len(), 5, "{}", second.says());
        assert!(second.reused().is_empty(), "{}", second.says());
    }
}

// covers: C-86, C-88
#[test]
fn a_test_file_reused_whole_runs_neither_its_tests_nor_its_setup_file() {
    for bats in bats_dirs() {
        hook_writes_facts(&bats);
        let mut shop = shop("{mode: reuse}");
        // setup_file leaves a mark outside the repository, which teardown_file needs.
        shop.push((
            "test/slow.bats",
            "setup_file() { touch \"$MARK\"; }\n\
             teardown_file() { [ -f \"$MARK\" ]; }\n\
             @test \"slow\" { true; }\n"
                .into(),
        ));
        let p = Project::new(&bats, &files(&shop));
        let mark = p.root.join("setup_file_ran");
        let run = |args: &[&str]| -> Ran {
            let output = p
                .hushrun()
                .env("MARK", &mark)
                .args(["run", "unit", "--"])
                .args(args)
                .output()
                .expect("hushrun");
            Ran {
                code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }
        };
        let first = run(&["bats", "test"]);
        assert_eq!(first.code, 0, "{}", first.says());
        assert!(mark.exists());
        std::fs::remove_file(&mark).unwrap();
        p.change(
            "lib/price.sh",
            "price() { echo $(( $1 * 2 )); }\n# changed\n",
        );
        let second = run(&["bats", "test"]);
        assert_eq!(second.code, 0, "{}", second.says());
        assert!(
            second.ran().contains(&"doubles".to_string()),
            "{}",
            second.says()
        );
        // Reused whole (but for what the audit draws): its tests skip, and setup_file and
        // teardown_file do nothing, whose own check would fail without the other.
        if second.reused().contains(&"slow".to_string()) {
            assert!(!mark.exists());
        }
        // Of the two test files that could be reused, the audit runs one.
        let reused = second.reused();
        assert!(
            reused == ["slow"] || reused == ["plain", "posix", "says hello"],
            "{}",
            second.says()
        );
    }
}

// covers: C-88
#[test]
fn a_failing_test_is_never_reused_and_bats_says_what_it_says_without_hushrun() {
    for bats in bats_dirs() {
        hook_writes_facts(&bats);
        let mut shop = shop("{mode: reuse, unit: test, reads: test}");
        shop.push((
            "test/broken.bats",
            "@test \"fails\" { [ -z \"${MUST_NOT_BE_SET-}\" ]; false; }\n@test \"ok\" { true; }\n"
                .into(),
        ));
        let p = Project::new(&bats, &files(&shop));
        // bats alone, in an environment of nothing but PATH.
        let plain = Command::new(bats.join("bats"))
            .arg("test")
            .current_dir(&p.repo)
            .env_clear()
            .env("PATH", &p.path)
            .output()
            .unwrap();
        let plain = String::from_utf8_lossy(&plain.stdout).into_owned();
        let first = p.run(&["bats", "test"]);
        assert_ne!(first.code, 0);
        let strip = |text: &str| -> String {
            text.lines()
                .filter(|l| l.starts_with("ok") || l.starts_with("not ok") || l.starts_with("1.."))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip(&first.stdout), strip(&plain));
        let second = p.run(&["bats", "test"]);
        assert_ne!(second.code, 0);
        assert!(
            second.ran().contains(&"fails".to_string()),
            "{}",
            second.says()
        );
        assert!(!second.reused().contains(&"fails".to_string()));
    }
}

/// Every file below `dir`, each with its first lines: what a failure shows of the hook's notes.
fn listing(dir: &Path) -> String {
    let mut out = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(here) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&here) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let head: Vec<&str> = text.lines().take(12).collect();
            out.push_str(&format!("== {}\n{}\n", path.display(), head.join("\n")));
        }
    }
    out
}

// covers: C-86, C-88
#[test]
fn the_hook_writes_one_facts_file_for_each_start_of_bats() {
    for bats in bats_dirs() {
        hook_writes_facts(&bats);
    }
}

/// Runs bats with the hook alone and checks that it wrote one facts file in which every test
/// finished; otherwise it fails with bash's version and the hook's notes. The other tests call it
/// first, so a machine where the hook can't work says why in whichever test fails first.
fn hook_writes_facts(bats: &Path) {
    {
        let shop = shop("{mode: reuse, unit: test, reads: test}");
        let p = Project::new(bats, &files(&shop));
        let units = p.root.join("units");
        std::fs::create_dir_all(&units).unwrap();
        std::fs::write(
            units.join("hushrun.json"),
            json!({"schema": "hushrun.units/v0", "root": p.repo, "reuse": [], "unit": "test", "reads": "test"})
                .to_string(),
        )
        .unwrap();
        let hook = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/units/hushrun_bats.bash");
        let mut cmd = Command::new(bats.join("bats"));
        cmd.arg("test")
            .current_dir(&p.repo)
            .env_clear()
            .env("PATH", &p.path)
            .env("HOME", p.root.join("home"))
            .env("BASH_ENV", &hook)
            .env("HUSHRUN_UNITS", &units)
            .env("HUSHRUN_BATS_EXE", env!("CARGO_BIN_EXE_hushrun"));
        let out = cmd.output().expect("bats");
        let bash = Command::new("bash")
            .arg("--version")
            .env("PATH", &p.path)
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
            .unwrap_or_default();
        // What this bash has for `$0` and the arguments while it reads `BASH_ENV`, and after.
        let probe = p.root.join("probe.bash");
        let script = p.root.join("probe-script");
        std::fs::write(&probe, "echo \"while BASH_ENV: \\$0=$0 \\$#=$#\"\n").unwrap();
        std::fs::write(&script, "echo \"after: \\$0=$0 \\$#=$#\"\n").unwrap();
        let zero = Command::new("bash")
            .arg(&script)
            .arg("one")
            .env("PATH", &p.path)
            .env("BASH_ENV", &probe)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let says = format!(
            "bats in {}, {bash}\n{zero}exit {:?}\n{}{}\n{}",
            bats.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            listing(&units)
        );
        assert!(out.status.success(), "{says}");
        let facts: Vec<Value> = std::fs::read_dir(&units)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json") && !p.ends_with("hushrun.json"))
            .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
            .collect();
        assert_eq!(facts.len(), 1, "{says}");
        let left: Vec<_> = std::fs::read_dir(&units)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".started"))
            .collect();
        assert!(left.is_empty(), "{says}");
        let units_seen = facts[0]["units"].as_array().cloned().unwrap_or_default();
        assert_eq!(units_seen.len(), 2, "{says}");
        for unit in &units_seen {
            assert_eq!(unit["selected"], unit["finished"], "{says}");
            for test in unit["tests"].as_array().unwrap() {
                assert_eq!(test["finished"], json!(true), "{says}");
            }
        }
    }
}
