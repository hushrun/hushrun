//! \[ADR 0069, 0096\] `tests: reuse` with Cargo and Go (C-31, C-32, C-34, C-35): the tests of
//! `reuse.rs` whose COMMAND starts the hushrun binary itself, so they stay end to end. Every start
//! of a Rust test binary goes through Cargo's target runner, and every Go test binary through the
//! `-exec` program Go's adapter puts in `GOFLAGS`, which are both the binary: such a flow reaches
//! all of it, as the static map says (CONTRIBUTING.md, "Which tests CI runs again").
//!
//! The fixtures need cargo-nextest, a C compiler for the native one, and Go. They say so and pass
//! where those are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

use std::process::Command;

use hushrun_testkit::git;
use serde_json::{Value, json};

use super::python::{PYTEST_CLEARED, tools};
use super::repo::Repo;
use super::store;

/// What a start of the binary came to.
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// The line that says what the plan reuses, or why every test file runs.
    fn plan_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| {
                // \[C-39\] In any adapter's word: `test files`, `test binaries`, `packages`.
                [
                    "test file",
                    "test files",
                    "test binary",
                    "test binaries",
                    "package",
                    "packages",
                    "test program",
                    "test programs",
                    "test class",
                    "test classes",
                    "test target",
                    "test targets",
                ]
                .iter()
                .any(|w| l.contains(&format!("{w} run")))
                    || l.contains("every test file runs")
            })
            .unwrap_or_else(|| panic!("no plan line in:\n{}", self.stderr))
    }
}

/// The binary in a [`Repo`], as these tests start it.
trait Binary {
    /// `hushrun ARGS`, with `env` besides, to its end.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Ran;
    /// The newest result this machine recorded for the check.
    fn newest_result(&self) -> Value;
}

impl Binary for Repo {
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = super::cli::Cli::hushrun(self);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("RUSTC_WORKSPACE_WRAPPER") {
                cmd.env_remove(name.as_ref());
            }
        }
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

    fn newest_result(&self) -> Value {
        super::cli::Cli::newest_result(self)
    }
}

/// A repository of `files`, committed, whose COMMAND never inherits pytest's options, a
/// `PYTHONPATH` or Cargo's target directory.
fn new_repo(files: &[(&str, &str)]) -> Repo {
    let owned: Vec<(String, String)> = files
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
    Repo::new(&owned).clear(PYTEST_CLEARED)
}

fn has_nextest() -> bool {
    Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A Cargo workspace: a library and its tests, a binary crate whose integration test runs it, a
/// second library, and a crate whose one integration test fails when a file outside the
/// workspace's files exists.
fn rust_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/ws\nchecks:\n  test:\n    tests: reuse\n",
        ),
        (".gitignore", "target/\nflag\n"),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/core\", \"crates/app\", \"crates/other\", \"crates/flagged\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/core/Cargo.toml",
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"core_lib\"\n",
        ),
        (
            "crates/core/src/lib.rs",
            "pub fn answer() -> u32 { 42 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
        ),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ncore-lib = { path = \"../core\" }\n",
        ),
        (
            "crates/app/src/main.rs",
            "fn main() { println!(\"{}\", core_lib::answer()); }\n",
        ),
        (
            "crates/app/tests/runs_binary.rs",
            "#[test]\nfn prints_the_answer() {\n    let out = std::process::Command::new(env!(\"CARGO_BIN_EXE_app\")).output().unwrap();\n    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), \"42\");\n}\n",
        ),
        (
            "crates/other/Cargo.toml",
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/other/src/lib.rs",
            "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
        ),
        (
            "crates/flagged/Cargo.toml",
            "[package]\nname = \"flagged\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ntest = false\n",
        ),
        ("crates/flagged/src/lib.rs", "pub fn one() -> u32 { 1 }\n"),
        (
            "crates/flagged/tests/reads_flag.rs",
            "#[test]\nfn no_flag() {\n    let flag = concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../flag\");\n    assert!(!std::path::Path::new(flag).exists());\n}\n",
        ),
    ]
}

// covers: C-34, C-32
#[test]
fn rust_test_binaries_are_reused_and_an_audit_catches_a_missed_dependency() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let r = new_repo(&rust_files());
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");
    let nextest = [
        "run",
        "test",
        "--",
        "cargo",
        "nextest",
        "run",
        "--workspace",
        "--locked",
    ];
    let first = r.run(&nextest, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.plan_line(),
        "hushrun: test: all 5 test binaries run: none had passed here yet on the files it read"
    );
    // One crate changes: its test binary runs, with one audited of the four others.
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 1 + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    let changed = r.run(&nextest, &[]);
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(
        changed.plan_line(),
        "hushrun: test: 2 of 5 test binaries run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    let units = r.newest_result()["units"].clone();
    assert!(units["savedMs"].is_u64(), "{units}");
    assert_eq!(
        units,
        json!({"total": 5, "ran": 2, "reused": 3, "savedMs": units["savedMs"], "audited": 1})
    );
    // \[ADR 0073\] Why, for a test binary: what it is built from, without a path of edges.
    r.write(
        "crates/core/src/lib.rs",
        "pub fn answer() -> u32 { 40 + 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
    );
    let why = r.run(&["why", "test", "crates/app/tests/runs_binary.rs"], &[]);
    assert_eq!(why.code, 0, "{}", why.stderr);
    assert!(
        why.stdout.starts_with(
            "crates/app/tests/runs_binary.rs runs: it read crates/core/src/lib.rs, which changed\n  its test binary is built from it: rustc's dep-info for the build names it\n  as far as this machine knows: its test binaries"
        ),
        "{}",
        why.stdout
    );
    r.write(
        "crates/core/src/lib.rs",
        "pub fn answer() -> u32 { 42 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
    );
    // Plain `cargo test` goes through the same runner.
    let plain = [
        "run",
        "test",
        "--",
        "cargo",
        "test",
        "--workspace",
        "--locked",
    ];
    assert_eq!(r.run(&plain, &[]).code, 0);
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    let second = r.run(&plain, &[]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        second.plan_line(),
        "hushrun: test: 2 of 5 test binaries run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );

    // The flag: what the flagged test reads and no dep-info names. Every other binary changes, so
    // the flagged one is the only one that could be reused, and the one audited.
    r.write("flag", "");
    r.write(
        "crates/core/src/lib.rs",
        "pub fn answer() -> u32 { 40 + 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
    );
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 2 * 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    let caught = r.run(&nextest, &[]);
    assert_ne!(caught.code, 0, "{}", caught.stderr);
    assert!(
        caught.stderr.contains("test: crates/flagged/tests/reads_flag.rs failed again, though it passed before on the same files it read: reuse of test's test files is suspended"),
        "{}",
        caught.stderr
    );
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 1 * 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    let suspended = r.run(&nextest, &[]);
    assert!(
        suspended
            .plan_line()
            .contains("every test binary runs: reuse of its test files is suspended"),
        "{}",
        suspended.stderr
    );
}

fn has_rust_analyzer() -> bool {
    Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// \[ADR 0162\] A workspace whose tests are reused one by one: `core-lib`'s unit tests of `a` and
/// of `b` (and an ignored one, which nextest doesn't run), an integration test of `b`, an
/// integration test with no tests, and a second crate, `other`, with a unit test and an
/// integration test with none.
fn one_by_one_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/ws\nchecks:\n  test:\n    tests:\n      mode: reuse\n      unit: test\n",
        ),
        (".gitignore", "target/\n"),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/core\", \"crates/other\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/core/Cargo.toml",
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"core_lib\"\n",
        ),
        ("crates/core/src/lib.rs", "pub mod a;\npub mod b;\n"),
        ("crates/core/src/a.rs", ONE_A),
        (
            "crates/core/src/b.rs",
            "pub fn b() -> u32 {\n    2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn b_is_two() {\n        assert_eq!(super::b(), 2);\n    }\n\n    #[test]\n    fn b_plus_one() {\n        assert_eq!(super::b() + 1, 3);\n    }\n\n    #[test]\n    #[ignore]\n    fn slow() {}\n}\n",
        ),
        (
            "crates/core/tests/uses_b.rs",
            "#[test]\nfn b_from_outside() {\n    assert_eq!(core_lib::b::b(), 2);\n}\n",
        ),
        ("crates/core/tests/empty.rs", "// No tests here.\n"),
        (
            "crates/other/Cargo.toml",
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/other/src/lib.rs",
            "pub fn two() -> u32 {\n    2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() {\n        assert_eq!(super::two(), 2);\n    }\n}\n",
        ),
        ("crates/other/tests/none.rs", "// No tests here.\n"),
    ]
}

/// `core-lib`'s `a`, with its two tests.
const ONE_A: &str = "pub fn a() -> u32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn a_is_one() {\n        assert_eq!(super::a(), 1);\n    }\n\n    #[test]\n    fn a_twice() {\n        assert_eq!(super::a() * 2, 2);\n    }\n}\n";

/// The tests nextest ran in a run, as `<binary id> <test name>`, from its `PASS` and `FAIL` lines.
fn ran_tests(ran: &Ran) -> std::collections::BTreeSet<String> {
    format!("{}\n{}", ran.stdout, ran.stderr)
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let l = l
                .strip_prefix("PASS [")
                .or_else(|| l.strip_prefix("FAIL ["))?;
            let after = l.split_once(']')?.1.trim_start();
            let after = match after.strip_prefix('(') {
                Some(counted) => counted.split_once(')')?.1.trim_start(),
                None => after,
            };
            let mut words = after.split_whitespace();
            Some(format!("{} {}", words.next()?, words.next()?))
        })
        .collect()
}

// covers: C-146, C-39
// \[ADR 0162\] A fresh machine, a CI runner, with the check's results on the results store and
// no store of its own: after a change to one function, the plan lists each test binary's tests,
// keys each by the static map, and finds the passes another machine's run recorded. Only the
// tests that reach the function run, with one audited; not every test of each binary that links
// it. The test binaries with no tests are known to have none, so the run counts tests.
#[test]
fn a_fresh_machine_reuses_rust_tests_one_by_one_from_the_results_alone() {
    if !tools("cargo-nextest", has_nextest()) || !tools("rust-analyzer", has_rust_analyzer()) {
        return;
    }
    let r = new_repo(&one_by_one_files());
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");
    let first = r.run(&NEXTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(ran_tests(&first).len(), 6, "{}", first.stderr);
    // A fresh machine: the results stay, this machine's store of the check and the index go.
    let units = r.cache.join("units");
    assert!(units.is_dir(), "{}", units.display());
    std::fs::remove_dir_all(&units).unwrap();
    assert!(r.cache.join("results").is_dir());
    r.write(
        "crates/core/src/a.rs",
        &ONE_A.replace("    1\n", "    2 - 1\n"),
    );
    let second = r.run(&NEXTEST, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    let plan = second
        .stderr
        .lines()
        .find(|l| l.starts_with("hushrun: test: ") && l.contains(" run"))
        .unwrap_or_else(|| panic!("no plan line in:\n{}", second.stderr));
    // Six tests, the ignored one not among them; three reused, one audited.
    assert!(
        plan.starts_with("hushrun: test: 3 of 6 tests run, in ")
            && plan.ends_with(" of 5 test binaries; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"),
        "{plan}\n{}",
        second.stderr
    );
    let ran = ran_tests(&second);
    for must in ["core-lib a::tests::a_is_one", "core-lib a::tests::a_twice"] {
        assert!(ran.contains(must), "{must} didn't run: {ran:?}");
    }
    let audited: Vec<&String> = ran
        .iter()
        .filter(|t| !t.starts_with("core-lib a::"))
        .collect();
    assert_eq!(
        audited.len(),
        1,
        "one test audited: {ran:?}\n{}",
        second.stderr
    );
    assert!(
        [
            "core-lib b::tests::b_is_two",
            "core-lib b::tests::b_plus_one",
            "core-lib::uses_b b_from_outside",
            "other tests::two",
        ]
        .contains(&audited[0].as_str()),
        "{ran:?}"
    );
    // The last line counts the tests reused, those of the test binaries reused whole too.
    let last = second
        .stderr
        .lines()
        .rev()
        .find(|l| l.starts_with("hushrun: test passed in "))
        .unwrap_or_else(|| panic!("no last line in:\n{}", second.stderr));
    assert!(
        last.contains("; 3 tests reused;") || last.contains("; reusing 3 tests saved about "),
        "{last}"
    );
}

// covers: C-34, C-39, C-146
// \[ADR 0162\] An audit of a test of a binary the first tier would reuse whole runs it: the binary
// isn't deselected whole. Here the only test that could be left out is `other`'s, of a binary
// whose key passed, so it is the one audited, whatever the run's start.
#[test]
fn an_audited_test_of_a_binary_that_passed_whole_runs() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let files: Vec<(&str, &str)> = one_by_one_files()
        .into_iter()
        .filter(|(path, _)| !path.starts_with("crates/core/") || *path == "crates/core/Cargo.toml")
        .chain([
            ("crates/core/src/lib.rs", "pub mod a;\n"),
            ("crates/core/src/a.rs", ONE_A),
        ])
        .collect();
    let r = new_repo(&files);
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");
    let first = r.run(&NEXTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    r.write(
        "crates/core/src/a.rs",
        &ONE_A.replace("    1\n", "    2 - 1\n"),
    );
    let second = r.run(&NEXTEST, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.stderr.contains(
            "hushrun: test: all 3 tests run; 1 that already passed on the files it read runs anyway, as a check"
        ),
        "{}",
        second.stderr
    );
    assert_eq!(
        ran_tests(&second),
        [
            "core-lib a::tests::a_is_one",
            "core-lib a::tests::a_twice",
            "other tests::two",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        "{}",
        second.stderr
    );
}

/// \[ADR 0095\] A workspace whose test binaries read variables at compile time: `flavored` reads
/// `HUSHRUN_FIXTURE_FLAVOR` with `option_env!`, `scripted` reads `HUSHRUN_FIXTURE_TONE`, which its
/// build script sets with `cargo:rustc-env` from `HUSHRUN_FIXTURE_MOOD`; `steady` and `other` read
/// none.
fn env_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/ws\nchecks:\n  test:\n    tests: reuse\n",
        ),
        (".gitignore", "target/\n"),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/flavored\", \"crates/scripted\", \"crates/steady\", \"crates/other\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/flavored/Cargo.toml",
            "[package]\nname = \"flavored\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/flavored/src/lib.rs",
            "pub const FLAVOR: Option<&str> = option_env!(\"HUSHRUN_FIXTURE_FLAVOR\");\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn flavor() { assert_ne!(super::FLAVOR, Some(\"\")); }\n}\n",
        ),
        (
            "crates/scripted/Cargo.toml",
            "[package]\nname = \"scripted\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/scripted/build.rs",
            "fn main() {\n    println!(\"cargo:rerun-if-env-changed=HUSHRUN_FIXTURE_MOOD\");\n    let mood = std::env::var(\"HUSHRUN_FIXTURE_MOOD\").unwrap_or_else(|_| \"calm\".into());\n    println!(\"cargo:rustc-env=HUSHRUN_FIXTURE_TONE={mood}\");\n}\n",
        ),
        (
            "crates/scripted/src/lib.rs",
            "pub const TONE: &str = env!(\"HUSHRUN_FIXTURE_TONE\");\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn tone() { assert!(!super::TONE.is_empty()); }\n}\n",
        ),
        (
            "crates/steady/Cargo.toml",
            "[package]\nname = \"steady\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/steady/src/lib.rs",
            "pub fn one() -> u32 { 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() { assert_eq!(super::one(), 1); }\n}\n",
        ),
        (
            "crates/other/Cargo.toml",
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/other/src/lib.rs",
            "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
        ),
    ]
}

/// [`env_files`], with its lockfile committed.
fn env_repo() -> Repo {
    let r = new_repo(&env_files());
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");
    r
}

const NEXTEST: [&str; 8] = [
    "run",
    "test",
    "--",
    "cargo",
    "nextest",
    "run",
    "--workspace",
    "--locked",
];

/// The test binaries nextest ran in a run, by their ids (a library's is its package's name), from
/// its `PASS` and `FAIL` lines.
fn ran_binaries(ran: &Ran) -> std::collections::BTreeSet<String> {
    format!("{}\n{}", ran.stdout, ran.stderr)
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let l = l
                .strip_prefix("PASS [")
                .or_else(|| l.strip_prefix("FAIL ["))?;
            let after = l.split_once(']')?.1.trim_start();
            // nextest 0.9.100 and later count the tests: `(1/4) steady tests::one`.
            let after = match after.strip_prefix('(') {
                Some(counted) => counted.split_once(')')?.1.trim_start(),
                None => after,
            };
            Some(after.split_whitespace().next()?.to_string())
        })
        .collect()
}

/// Which binaries a run ran, by name: every one of `must`, and `audited` more of `reusable`, which
/// an audit chose; every other one of `reusable` was reused.
fn assert_ran(ran: &Ran, must: &[&str], reusable: &[&str], audited: usize) {
    let names = ran_binaries(ran);
    for name in must {
        assert!(
            names.contains(*name),
            "{name} didn't run: {names:?}\n{}",
            ran.stderr
        );
    }
    let checked: Vec<&String> = names
        .iter()
        .filter(|n| !must.contains(&n.as_str()))
        .collect();
    assert!(
        checked.iter().all(|n| reusable.contains(&n.as_str())),
        "ran {names:?}, more than {must:?} and audits of {reusable:?}"
    );
    assert_eq!(checked.len(), audited, "ran {names:?}");
}

/// \[ADR 0095\] Which binaries' keys held another `env` after a run than before, by the store,
/// whatever an audit ran: exactly `changed`, each `crates/<name>/src/lib.rs`.
fn assert_env_changed(
    before: &std::collections::BTreeMap<String, String>,
    after: &std::collections::BTreeMap<String, String>,
    changed: &[&str],
) {
    let moved: Vec<String> = after
        .iter()
        .filter(|(unit, env)| before.get(*unit) != Some(*env))
        .map(|(unit, _)| unit.clone())
        .collect();
    let expected: Vec<String> = changed
        .iter()
        .map(|name| format!("crates/{name}/src/lib.rs"))
        .collect();
    assert_eq!(moved, expected, "before {before:?}\nafter {after:?}");
}

/// `crates/other/src/lib.rs` edited, its `n`-th version: a change no other binary reads, so the
/// check's own key misses and the others' keys are what decide.
fn edit_other(r: &Repo, n: u32) {
    r.write(
        "crates/other/src/lib.rs",
        &format!(
            "pub fn two() -> u32 {{ 2 + {n} - {n} }}\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn two() {{ assert_eq!(super::two(), 2); }}\n}}\n"
        ),
    );
}

// covers: C-31, C-34
// A test binary compiled with another value of a variable it read with `option_env!` isn't
// reused: Cargo rebuilt it, and its key holds the variables its build read. The same value again
// reuses it.
#[test]
fn a_test_binary_built_with_another_value_of_a_variable_it_read_runs_again() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let r = env_repo();
    let first = r.run(&NEXTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 4, "ran": 4, "reused": 0})
    );

    // Unset, then set: `flavored` runs with `other`; `scripted` and `steady` could be reused,
    // and one of them is audited. Only `flavored`'s build read another value, whichever an audit
    // ran.
    let before = store::env_of(&r.cache);
    edit_other(&r, 1);
    let set = r.run(&NEXTEST, &[("HUSHRUN_FIXTURE_FLAVOR", "sour")]);
    assert_eq!(set.code, 0, "{}\n{}", set.stdout, set.stderr);
    assert_eq!(
        set.plan_line(),
        "hushrun: test: 3 of 4 test binaries run; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check"
    );
    let units = r.newest_result()["units"].clone();
    assert_eq!(
        units,
        json!({"total": 4, "ran": 3, "reused": 1, "savedMs": units["savedMs"], "audited": 1})
    );
    assert_ran(&set, &["flavored", "other"], &["scripted", "steady"], 1);
    let after_set = store::env_of(&r.cache);
    assert_env_changed(&before, &after_set, &["flavored"]);

    // Another value runs it again.
    edit_other(&r, 2);
    let other = r.run(&NEXTEST, &[("HUSHRUN_FIXTURE_FLAVOR", "sweet")]);
    assert_eq!(other.code, 0, "{}", other.stderr);
    assert_eq!(
        other.plan_line(),
        "hushrun: test: 3 of 4 test binaries run; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check"
    );
    assert_ran(&other, &["flavored", "other"], &["scripted", "steady"], 1);
    let after_other = store::env_of(&r.cache);
    assert_env_changed(&after_set, &after_other, &["flavored"]);

    // The same value as the run before: its pass on that value counts.
    edit_other(&r, 3);
    let same = r.run(&NEXTEST, &[("HUSHRUN_FIXTURE_FLAVOR", "sweet")]);
    assert_eq!(same.code, 0, "{}", same.stderr);
    assert_eq!(
        same.plan_line(),
        "hushrun: test: 2 of 4 test binaries run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert_ran(&same, &["other"], &["flavored", "scripted", "steady"], 1);
    assert_env_changed(&after_other, &store::env_of(&r.cache), &[]);
    // \[C-37\] `hushrun why` keys it as the run does: with what its last build here read.
    let why = r.run(&["why", "test", "crates/flavored/src/lib.rs"], &[]);
    assert_eq!(why.code, 0, "{}", why.stderr);
    assert!(
        why.stdout.starts_with(
            "crates/flavored/src/lib.rs would be reused: it passed on the files it read"
        ),
        "{}",
        why.stdout
    );

    // \[ADR 0095\] A row without what its build read of the environment, as an older hushrun
    // wrote it, keys nothing: `why` says it runs, and so does the plan's line, until a run here
    // records it again from its build.
    store::forget_env(&r.cache);
    let unknown = r.run(&["why", "test", "crates/flavored/src/lib.rs"], &[]);
    assert_eq!(unknown.code, 0, "{}", unknown.stderr);
    assert!(
        unknown.stdout.starts_with(
            "crates/flavored/src/lib.rs runs: this machine doesn't know what its last build here read of the environment"
        ),
        "{}",
        unknown.stdout
    );
    edit_other(&r, 4);
    let rebuilt = r.run(&NEXTEST, &[("HUSHRUN_FIXTURE_FLAVOR", "sweet")]);
    assert_eq!(rebuilt.code, 0, "{}", rebuilt.stderr);
    // The run keys from its build, which its pass on that value stands on.
    assert_eq!(
        rebuilt.plan_line(),
        "hushrun: test: 2 of 4 test binaries run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    let known = r.run(&["why", "test", "crates/flavored/src/lib.rs"], &[]);
    assert!(
        known
            .stdout
            .starts_with("crates/flavored/src/lib.rs would be reused"),
        "{}",
        known.stdout
    );
}

// covers: C-31, C-34
// A build script that sets another value with `cargo:rustc-env` reruns the test binary that reads
// it, though no file it was built from changed: here the script sets what it read from the
// environment, a variable the binary itself never read.
#[test]
fn a_variable_a_build_script_sets_changing_runs_the_test_binary_that_read_it_again() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let r = env_repo();
    let first = r.run(&NEXTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);

    let before = store::env_of(&r.cache);
    edit_other(&r, 1);
    let changed = r.run(&NEXTEST, &[("HUSHRUN_FIXTURE_MOOD", "loud")]);
    assert_eq!(changed.code, 0, "{}\n{}", changed.stdout, changed.stderr);
    assert_eq!(
        changed.plan_line(),
        "hushrun: test: 3 of 4 test binaries run; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check"
    );
    let units = r.newest_result()["units"].clone();
    assert_eq!(
        units,
        json!({"total": 4, "ran": 3, "reused": 1, "savedMs": units["savedMs"], "audited": 1})
    );
    assert_ran(&changed, &["scripted", "other"], &["flavored", "steady"], 1);
    assert_env_changed(&before, &store::env_of(&r.cache), &["scripted"]);
}

// covers: C-31, C-34
// A test binary that links a crate outside the workspace (here a path crate outside the
// repository, standing for a registry or git crate) runs again when that crate was built with
// another value of a variable it read: Cargo rebuilt both.
#[test]
fn a_dependency_outside_the_workspace_built_with_another_value_runs_its_binary_again() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let mut files = env_files();
    files.retain(|(path, _)| *path != "Cargo.toml");
    files.extend([
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/linked\", \"crates/flavored\", \"crates/scripted\", \"crates/steady\", \"crates/other\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/linked/Cargo.toml",
            "[package]\nname = \"linked\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nouter = { path = \"../../../outer\" }\n",
        ),
        (
            "crates/linked/src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn flavor() { assert_ne!(outer::FLAVOR, Some(\"\")); }\n}\n",
        ),
    ]);
    let r = new_repo(&files);
    // Outside the repository: no file of the scope names it.
    std::fs::create_dir_all(r.root.join("outer/src")).unwrap();
    std::fs::write(
        r.root.join("outer/Cargo.toml"),
        "[package]\nname = \"outer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        r.root.join("outer/src/lib.rs"),
        "pub const FLAVOR: Option<&str> = option_env!(\"HUSHRUN_FIXTURE_OUTER\");\n",
    )
    .unwrap();
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");

    let first = r.run(&NEXTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    let before = store::env_of(&r.cache);
    edit_other(&r, 1);
    let set = r.run(&NEXTEST, &[("HUSHRUN_FIXTURE_OUTER", "sour")]);
    assert_eq!(set.code, 0, "{}\n{}", set.stdout, set.stderr);
    assert_eq!(
        set.plan_line(),
        "hushrun: test: 3 of 5 test binaries run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert_ran(
        &set,
        &["linked", "other"],
        &["flavored", "scripted", "steady"],
        1,
    );
    assert_env_changed(&before, &store::env_of(&r.cache), &["linked"]);
}

// covers: C-31, C-32
// A dependency only another platform needs, whose files this machine never fetched (here a
// registry crate the fixture's local registry lists and doesn't hold), keeps the graph of what
// each binary is built against: the crate outside the workspace one binary reads reruns that
// binary alone, offline.
#[test]
fn a_dependency_only_another_platform_needs_leaves_each_binary_its_own_crates() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let mut files = env_files();
    files.retain(|(path, _)| *path != "Cargo.toml");
    files.extend([
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/linked\", \"crates/flavored\", \"crates/scripted\", \"crates/steady\", \"crates/other\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/linked/Cargo.toml",
            "[package]\nname = \"linked\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nouter = { path = \"../../../outer\" }\n\n[target.'cfg(target_os = \"hushrun-none\")'.dependencies]\nelsewhere = \"=1.0.1\"\n",
        ),
        (
            "crates/linked/src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn flavor() { assert_ne!(outer::FLAVOR, Some(\"\")); }\n}\n",
        ),
    ]);
    let r = new_repo(&files);
    std::fs::create_dir_all(r.root.join("outer/src")).unwrap();
    std::fs::write(
        r.root.join("outer/Cargo.toml"),
        "[package]\nname = \"outer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        r.root.join("outer/src/lib.rs"),
        "pub const FLAVOR: Option<&str> = option_env!(\"HUSHRUN_FIXTURE_OUTER\");\n",
    )
    .unwrap();
    // A local registry that lists `elsewhere` and holds none of its files.
    let index = r.root.join("registry/index/el/se");
    std::fs::create_dir_all(&index).unwrap();
    std::fs::write(
        index.join("elsewhere"),
        format!(
            "{{\"name\":\"elsewhere\",\"vers\":\"1.0.1\",\"deps\":[],\"cksum\":\"{}\",\"features\":{{}},\"yanked\":false}}\n",
            "0".repeat(64)
        ),
    )
    .unwrap();
    r.write(
        ".cargo/config.toml",
        &format!(
            "[source.crates-io]\nreplace-with = \"fixture\"\n\n[source.fixture]\nlocal-registry = {:?}\n",
            r.root.join("registry").to_str().unwrap()
        ),
    );
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");
    let offline = [
        "run",
        "test",
        "--",
        "cargo",
        "nextest",
        "run",
        "--workspace",
        "--locked",
        "--offline",
    ];

    let first = r.run(&offline, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    let before = store::env_of(&r.cache);
    edit_other(&r, 1);
    let set = r.run(&offline, &[("HUSHRUN_FIXTURE_OUTER", "sour")]);
    assert_eq!(set.code, 0, "{}\n{}", set.stdout, set.stderr);
    // Only `linked` reads `outer`: the others' keys stay as they were.
    assert_eq!(
        set.plan_line(),
        "hushrun: test: 3 of 5 test binaries run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert_ran(
        &set,
        &["linked", "other"],
        &["flavored", "scripted", "steady"],
        1,
    );
    assert_env_changed(&before, &store::env_of(&r.cache), &["linked"]);
}

/// \[ADR 0096\] A workspace whose build scripts derive what its test binaries are built with from
/// variables, each a way rustc's dep-info doesn't track: `cfgd`'s turns `HUSHRUN_FIXTURE_CFG`
/// into `cargo::rustc-cfg`, `gen`'s writes `HUSHRUN_FIXTURE_GEN` into a file under `OUT_DIR` its
/// library includes, and `linked`'s turns `HUSHRUN_FIXTURE_LINK` into a link argument. `other`
/// has none. Each test passes whatever the value.
fn script_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/ws\nchecks:\n  test:\n    tests: reuse\n",
        ),
        (".gitignore", "target/\n"),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/cfgd\", \"crates/gen\", \"crates/linked\", \"crates/other\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/cfgd/Cargo.toml",
            "[package]\nname = \"cfgd\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/cfgd/build.rs",
            "fn main() {\n    println!(\"cargo::rerun-if-env-changed=HUSHRUN_FIXTURE_CFG\");\n    println!(\"cargo::rustc-check-cfg=cfg(loud)\");\n    if std::env::var_os(\"HUSHRUN_FIXTURE_CFG\").is_some() {\n        println!(\"cargo::rustc-cfg=loud\");\n    }\n}\n",
        ),
        (
            "crates/cfgd/src/lib.rs",
            "#[cfg(loud)]\npub const MOOD: &str = \"loud\";\n#[cfg(not(loud))]\npub const MOOD: &str = \"quiet\";\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn mood() { assert!(!super::MOOD.is_empty()); }\n}\n",
        ),
        (
            "crates/gen/Cargo.toml",
            "[package]\nname = \"gen\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/gen/build.rs",
            "fn main() {\n    println!(\"cargo:rerun-if-env-changed=HUSHRUN_FIXTURE_GEN\");\n    println!(\"cargo:rerun-if-changed=build.rs\");\n    let value = std::env::var(\"HUSHRUN_FIXTURE_GEN\").unwrap_or_else(|_| \"plain\".into());\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::write(format!(\"{out}/gen.rs\"), format!(\"pub const GEN: &str = {value:?};\\n\")).unwrap();\n}\n",
        ),
        (
            "crates/gen/src/lib.rs",
            "include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn generated() { assert!(!super::GEN.is_empty()); }\n}\n",
        ),
        (
            "crates/linked/Cargo.toml",
            "[package]\nname = \"linked\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/linked/build.rs",
            "fn main() {\n    println!(\"cargo:rerun-if-env-changed=HUSHRUN_FIXTURE_LINK\");\n    let name = std::env::var(\"HUSHRUN_FIXTURE_LINK\").unwrap_or_else(|_| \"first\".into());\n    let dir = format!(\"{}/{name}\", std::env::var(\"OUT_DIR\").unwrap());\n    std::fs::create_dir_all(&dir).unwrap();\n    println!(\"cargo:rustc-link-search=native={dir}\");\n}\n",
        ),
        (
            "crates/linked/src/lib.rs",
            "pub fn three() -> u32 { 3 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn three() { assert_eq!(super::three(), 3); }\n}\n",
        ),
        (
            "crates/other/Cargo.toml",
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/other/src/lib.rs",
            "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
        ),
    ]
}

/// `files`, with their lockfile committed.
fn locked_repo(files: &[(&str, &str)]) -> Repo {
    let r = new_repo(files);
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");
    r
}

/// \[ADR 0096\] The run after a first one of [`script_files`], with `other` edited and `variable`
/// set to `value`: `binary`, whose build script derived another build from it, runs with `other`,
/// and so does one of the other two, audited. The same value again reuses it.
fn a_build_script_derivation_reruns(variable: &str, value: &str, binary: &str) {
    let r = locked_repo(&script_files());
    let first = r.run(&NEXTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 4, "ran": 4, "reused": 0})
    );
    let others: Vec<&str> = ["cfgd", "gen", "linked"]
        .into_iter()
        .filter(|name| *name != binary)
        .collect();

    edit_other(&r, 1);
    let set = r.run(&NEXTEST, &[(variable, value)]);
    assert_eq!(set.code, 0, "{}\n{}", set.stdout, set.stderr);
    assert_eq!(
        set.plan_line(),
        "hushrun: test: 3 of 4 test binaries run; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check",
        "{}",
        set.stderr
    );
    assert_ran(&set, &[binary, "other"], &others, 1);

    // The same value: its pass on what its build script gave it counts.
    edit_other(&r, 2);
    let same = r.run(&NEXTEST, &[(variable, value)]);
    assert_eq!(same.code, 0, "{}", same.stderr);
    assert_eq!(
        same.plan_line(),
        "hushrun: test: 2 of 4 test binaries run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
        "{}",
        same.stderr
    );
    let mut reusable = others.clone();
    reusable.push(binary);
    assert_ran(&same, &["other"], &reusable, 1);
    // \[C-37\] `hushrun why` keys it as the run did, by what its last build here gave it, and the
    // variable as the next build would take it.
    let unit = format!("crates/{binary}/src/lib.rs");
    let why = r.run(&["why", "test", &unit], &[(variable, value)]);
    assert_eq!(why.code, 0, "{}", why.stderr);
    assert!(
        why.stdout.starts_with(&format!(
            "{unit} would be reused: it passed on the files it read"
        )),
        "{}",
        why.stdout
    );
}

// covers: C-31, C-34
// A build script that turns a variable into `cargo::rustc-cfg` builds another test binary from the
// same files: it runs again.
#[test]
fn a_cfg_a_build_script_derives_from_a_variable_runs_its_test_binary_again() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    a_build_script_derivation_reruns("HUSHRUN_FIXTURE_CFG", "1", "cfgd");
}

// covers: C-31, C-34
// A build script that writes a variable into a file under `OUT_DIR`, which the library includes,
// builds another test binary from the same files of the repository: it runs again.
#[test]
fn a_file_a_build_script_generates_from_a_variable_runs_its_test_binary_again() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    a_build_script_derivation_reruns("HUSHRUN_FIXTURE_GEN", "spicy", "gen");
}

// covers: C-31, C-34
// A build script that turns a variable into a link argument links the test binary another way:
// it runs again.
#[test]
fn a_link_argument_a_build_script_derives_from_a_variable_runs_its_test_binary_again() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    a_build_script_derivation_reruns("HUSHRUN_FIXTURE_LINK", "second", "linked");
}

fn has_cc() -> bool {
    ["cc", "ar"].iter().all(|tool| {
        Command::new("sh")
            .args(["-c", &format!("command -v {tool}")])
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// \[ADR 0096\] A workspace whose build scripts make what the binary is built from out of what
/// no line of their output says: `native`'s compiles C into a static library in `OUT_DIR` with
/// `-DVAL=$HUSHRUN_FIXTURE_NATIVE`, which it declares with `rerun-if-env-changed`, and prints the
/// same link lines whatever the value; `srcgen`'s writes a git-ignored `src/generated.rs`, which
/// its library includes, from a git-ignored `input.txt` it watches. `steady` and `other` have
/// none. Each test passes whatever the value.
fn native_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/ws\nchecks:\n  test:\n    tests: reuse\n",
        ),
        (
            ".gitignore",
            "target/\ncrates/srcgen/input.txt\ncrates/srcgen/src/generated.rs\n",
        ),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/native\", \"crates/srcgen\", \"crates/steady\", \"crates/other\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/native/Cargo.toml",
            "[package]\nname = \"native\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("crates/native/nat.c", "int nat(void) { return VAL; }\n"),
        (
            "crates/native/build.rs",
            "use std::process::Command;\nfn main() {\n    println!(\"cargo::rerun-if-env-changed=HUSHRUN_FIXTURE_NATIVE\");\n    println!(\"cargo::rerun-if-changed=nat.c\");\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    let val = std::env::var(\"HUSHRUN_FIXTURE_NATIVE\").unwrap_or_else(|_| \"1\".into());\n    assert!(Command::new(\"cc\").args([\"-c\", \"nat.c\", &format!(\"-DVAL={val}\"), \"-o\", &format!(\"{out}/nat.o\")]).status().unwrap().success());\n    assert!(Command::new(\"ar\").args([\"rcs\", &format!(\"{out}/libnat.a\"), &format!(\"{out}/nat.o\")]).status().unwrap().success());\n    println!(\"cargo::rustc-link-search=native={out}\");\n    println!(\"cargo::rustc-link-lib=static=nat\");\n}\n",
        ),
        (
            "crates/native/src/lib.rs",
            "extern \"C\" {\n    fn nat() -> i32;\n}\npub fn value() -> i32 {\n    unsafe { nat() }\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn value() { assert!(super::value() > 0); }\n}\n",
        ),
        (
            "crates/srcgen/Cargo.toml",
            "[package]\nname = \"srcgen\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("crates/srcgen/input.txt", "plain\n"),
        (
            "crates/srcgen/build.rs",
            "fn main() {\n    println!(\"cargo:rerun-if-changed=input.txt\");\n    let dir = std::env::var(\"CARGO_MANIFEST_DIR\").unwrap();\n    let value = std::fs::read_to_string(format!(\"{dir}/input.txt\")).unwrap();\n    let text = format!(\"pub const GEN: &str = {:?};\\n\", value.trim());\n    let path = format!(\"{dir}/src/generated.rs\");\n    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {\n        std::fs::write(path, text).unwrap();\n    }\n}\n",
        ),
        (
            "crates/srcgen/src/lib.rs",
            "include!(\"generated.rs\");\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn generated() { assert!(!super::GEN.is_empty()); }\n}\n",
        ),
        (
            "crates/steady/Cargo.toml",
            "[package]\nname = \"steady\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/steady/src/lib.rs",
            "pub fn one() -> u32 { 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() { assert_eq!(super::one(), 1); }\n}\n",
        ),
        (
            "crates/other/Cargo.toml",
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/other/src/lib.rs",
            "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
        ),
    ]
}

/// \[ADR 0096\] Two runs after a first one of [`native_files`], each with `other` edited and
/// `change` made (it returns the run's variables): the first runs `binary` with `other`, and one
/// of the two others, audited; the second, with the same change again, reuses it.
fn a_native_files_change_reruns(
    binary: &str,
    change: &dyn Fn(&Repo) -> Vec<(&'static str, &'static str)>,
) -> Repo {
    let r = locked_repo(&native_files());
    let first = r.run(&NEXTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 4, "ran": 4, "reused": 0})
    );
    let others: Vec<&str> = ["native", "srcgen", "steady"]
        .into_iter()
        .filter(|name| *name != binary)
        .collect();
    edit_other(&r, 1);
    let vars = change(&r);
    let changed = r.run(&NEXTEST, &vars);
    assert_eq!(changed.code, 0, "{}\n{}", changed.stdout, changed.stderr);
    assert_eq!(
        changed.plan_line(),
        "hushrun: test: 3 of 4 test binaries run; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check",
        "{}",
        changed.stderr
    );
    assert_ran(&changed, &[binary, "other"], &others, 1);
    edit_other(&r, 2);
    let vars = change(&r);
    let same = r.run(&NEXTEST, &vars);
    assert_eq!(same.code, 0, "{}", same.stderr);
    assert_eq!(
        same.plan_line(),
        "hushrun: test: 2 of 4 test binaries run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
        "{}",
        same.stderr
    );
    r
}

// covers: C-31, C-34
// A build script that compiles C into a library in `OUT_DIR` with a variable it declares links
// the test binary against another library with the same link lines: it runs again.
#[test]
fn a_native_library_a_build_script_builds_from_a_variable_runs_its_test_binary_again() {
    if !tools("cargo-nextest", has_nextest()) || !tools("cc and ar", has_cc()) {
        return;
    }
    let r = a_native_files_change_reruns("native", &|_| vec![("HUSHRUN_FIXTURE_NATIVE", "2")]);
    // \[C-37\] `hushrun why` takes the variable as the next build would: its value now.
    let unit = "crates/native/src/lib.rs";
    let why = r.run(&["why", "test", unit], &[("HUSHRUN_FIXTURE_NATIVE", "2")]);
    assert!(
        why.stdout.starts_with(&format!("{unit} would be reused")),
        "{}",
        why.stdout
    );
    let why = r.run(&["why", "test", unit], &[("HUSHRUN_FIXTURE_NATIVE", "3")]);
    assert!(
        why.stdout.starts_with(&format!(
            "{unit} runs: a variable a build script of its build declares has another value now than in its last build here (HUSHRUN_FIXTURE_NATIVE)"
        )),
        "{}",
        why.stdout
    );
}

// covers: C-31, C-34
// A source file of the checkout that no file of the scope is (git-ignored), which a build script
// generates and the library includes, builds another test binary when it changes: it runs again.
#[test]
fn a_git_ignored_source_a_build_script_generates_runs_its_test_binary_again() {
    if !tools("cargo-nextest", has_nextest()) || !tools("cc and ar", has_cc()) {
        return;
    }
    let _ = a_native_files_change_reruns("srcgen", &|r| {
        r.write("crates/srcgen/input.txt", "spicy\n");
        vec![]
    });
}

// covers: C-30, C-31, C-34
// An integration test of a package with a build script is observed, recorded and reused: the
// script's own dep-info is found, so its `build.rs` is a file the binaries of its package read,
// and editing it reruns those alone.
#[test]
fn an_integration_test_of_a_package_with_a_build_script_is_observed_and_reused() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let mut files = script_files();
    files.push((
        "crates/gen/tests/it.rs",
        "#[test]\nfn generated() { assert!(!gen::GEN.is_empty()); }\n",
    ));
    let r = locked_repo(&files);
    let first = r.run(&NEXTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 5, "ran": 5, "reused": 0})
    );
    assert!(
        store::map(&r.cache)["units"]["crates/gen/tests/it.rs"]
            .get("notFollowed")
            .is_none(),
        "{:#}",
        store::map(&r.cache)
    );

    edit_other(&r, 1);
    let second = r.run(&NEXTEST, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        second.plan_line(),
        "hushrun: test: 2 of 5 test binaries run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
        "{}",
        second.stderr
    );
    assert_ran(
        &second,
        &["other"],
        &["cfgd", "gen", "gen::it", "linked"],
        1,
    );

    // Its build script is a file the package's binaries read, and no other's.
    r.write(
        "crates/gen/build.rs",
        &files
            .iter()
            .find(|(path, _)| *path == "crates/gen/build.rs")
            .map(|(_, text)| format!("// edited\n{text}"))
            .unwrap(),
    );
    let edited = r.run(&NEXTEST, &[]);
    assert_eq!(edited.code, 0, "{}\n{}", edited.stdout, edited.stderr);
    assert_eq!(
        edited.plan_line(),
        "hushrun: test: 3 of 5 test binaries run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
        "{}",
        edited.stderr
    );
    assert_ran(
        &edited,
        &["gen", "gen::it"],
        &["cfgd", "linked", "other"],
        1,
    );
}

// Go's: its adapter puts the hushrun binary in `GOFLAGS` as `-exec`, which starts every test
// binary, as Cargo's target runner does.

fn has_go() -> bool {
    Command::new("go")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Go as these tests run it: this machine's build cache, so that the standard library builds
/// once, no toolchain download and no configuration file of the person's.
fn go_env() -> Vec<(&'static str, String)> {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        Command::new("go")
            .args(["env", "GOCACHE"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| {
                std::env::temp_dir()
                    .join("hushrun-test-gocache")
                    .display()
                    .to_string()
            })
    });
    vec![
        ("GOCACHE", cache.clone()),
        ("GOTOOLCHAIN", "local".into()),
        ("GOENV", "off".into()),
    ]
}

/// \[ADR 0096\] A Go module whose packages' tests read variables while they run: `flavor` with
/// `os.Getenv`, `lookup` with `os.LookupEnv`, and `plain` and `other` none. Each test passes
/// whatever the value.
fn getenv_files(tests: &str) -> Vec<(String, String)> {
    let test = |package: &str, body: &str| {
        (
            format!("{package}/{package}_test.go"),
            format!(
                "package {package}\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nvar _ = os.Getenv\n\nfunc TestOne(t *testing.T) {{\n{body}}}\n\nfunc TestTwo(t *testing.T) {{\n{body}}}\n"
            ),
        )
    };
    let mut files = vec![
        (
            "hushrun.yaml".to_string(),
            format!(
                "version: 1\nrepository: example.com/acme/mod\nchecks:\n  unit:\n    tests: {tests}\n"
            ),
        ),
        (
            "go.mod".to_string(),
            "module example.com/mod\n\ngo 1.21\n".to_string(),
        ),
        test(
            "flavor",
            "\tif os.Getenv(\"HUSHRUN_FIXTURE_GO\") == \"never\" {\n\t\tt.Fatal(\"never\")\n\t}\n",
        ),
        test(
            "lookup",
            "\tif v, ok := os.LookupEnv(\"HUSHRUN_FIXTURE_LOOKUP\"); ok && v == \"never\" {\n\t\tt.Fatal(\"never\")\n\t}\n",
        ),
        test("plain", "\tif 1+1 != 2 {\n\t\tt.Fatal(\"two\")\n\t}\n"),
    ];
    files.push(go_other(0));
    files
}

/// `other`'s test, its `n`-th version: a change no other package reads, so the check's own key
/// misses and the others' keys are what decide.
fn go_other(n: u32) -> (String, String) {
    (
        "other/other_test.go".to_string(),
        format!(
            "package other\n\nimport \"testing\"\n\nfunc TestOne(t *testing.T) {{\n\tif {n}-{n} != 0 {{\n\t\tt.Fatal(\"zero\")\n\t}}\n}}\n"
        ),
    )
}

/// The packages `go test -json` ran a test of, by their directory.
fn go_ran(ran: &Ran) -> std::collections::BTreeSet<String> {
    ran.stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["Action"] == "run" && event["Test"].is_string())
        .filter_map(|event| {
            let package = event["Package"].as_str()?;
            Some(package.strip_prefix("example.com/mod/")?.to_string())
        })
        .collect()
}

const GO_TEST: [&str; 7] = ["run", "unit", "--", "go", "test", "-json", "./..."];

/// A run of `go test` in `r` with `vars` besides Go's own.
fn go_run(r: &Repo, args: &[&str], vars: &[(&str, &str)]) -> Ran {
    let go = go_env();
    let mut env: Vec<(&str, &str)> = go.iter().map(|(n, v)| (*n, v.as_str())).collect();
    env.extend_from_slice(vars);
    r.run(args, &env)
}

// covers: C-31, C-34, C-35
// A Go package whose tests read a variable with `os.Getenv` or `os.LookupEnv` runs again when its
// value changes, or it goes from unset to set to nothing, as Go's own test cache would rerun it;
// the same value again reuses it.
#[test]
fn a_go_package_whose_tests_read_another_value_of_a_variable_runs_again() {
    if !tools("Go", has_go()) {
        return;
    }
    let files = getenv_files("reuse");
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let r = new_repo(&files);
    let first = go_run(&r, &GO_TEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.plan_line(),
        "hushrun: unit: all 4 packages run: none had passed here yet on the files it read"
    );

    // Another value: `flavor` runs with `other`, and one of the other two, audited.
    let (path, text) = go_other(1);
    r.write(&path, &text);
    let set = go_run(&r, &GO_TEST, &[("HUSHRUN_FIXTURE_GO", "sour")]);
    assert_eq!(set.code, 0, "{}\n{}", set.stdout, set.stderr);
    assert_eq!(
        set.plan_line(),
        "hushrun: unit: 3 of 4 packages run; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check",
        "{}",
        set.stderr
    );
    let ran = go_ran(&set);
    assert!(ran.contains("flavor") && ran.contains("other"), "{ran:?}");

    // The same value: its pass on it counts. `hushrun why` keys it by the value it is given.
    let (path, text) = go_other(2);
    r.write(&path, &text);
    let same = go_run(&r, &GO_TEST, &[("HUSHRUN_FIXTURE_GO", "sour")]);
    assert_eq!(same.code, 0, "{}", same.stderr);
    assert_eq!(
        same.plan_line(),
        "hushrun: unit: 2 of 4 packages run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
        "{}",
        same.stderr
    );
    assert!(go_ran(&same).contains("other"));
    let why = go_run(
        &r,
        &["why", "unit", "flavor"],
        &[("HUSHRUN_FIXTURE_GO", "sour")],
    );
    assert!(
        why.stdout.starts_with("flavor would be reused"),
        "{}\n{}",
        why.stdout,
        why.stderr
    );
    let why = go_run(
        &r,
        &["why", "unit", "flavor"],
        &[("HUSHRUN_FIXTURE_GO", "sweet")],
    );
    assert!(why.stdout.starts_with("flavor runs"), "{}", why.stdout);
    assert!(
        why.stdout.contains(
            "or a variable its tests read has another value now than when it passed: its key holds the value of each (HUSHRUN_FIXTURE_GO)"
        ),
        "{}",
        why.stdout
    );

    // Set to nothing is another value than unset: `lookup` runs.
    let (path, text) = go_other(3);
    r.write(&path, &text);
    let empty = go_run(
        &r,
        &GO_TEST,
        &[
            ("HUSHRUN_FIXTURE_GO", "sour"),
            ("HUSHRUN_FIXTURE_LOOKUP", ""),
        ],
    );
    assert_eq!(empty.code, 0, "{}", empty.stderr);
    assert_eq!(
        empty.plan_line(),
        "hushrun: unit: 3 of 4 packages run; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check",
        "{}",
        empty.stderr
    );
    let ran = go_ran(&empty);
    assert!(ran.contains("lookup") && ran.contains("other"), "{ran:?}");

    // \[C-37\] A row an older hushrun wrote names no variable, and every Go key holds them now.
    store::forget_getenv(&r.cache);
    let unknown = go_run(
        &r,
        &["why", "unit", "flavor"],
        &[("HUSHRUN_FIXTURE_GO", "sour")],
    );
    assert!(
        unknown.stdout.starts_with(
            "flavor runs: this machine doesn't know which variables its tests read when it last ran here, which every key of a Go package holds now"
        ),
        "{}",
        unknown.stdout
    );
}

// covers: C-31, C-35, C-39
// With tests reused one by one, a test's key holds what its package's tests read of the
// environment too: a package that ran because a variable changed doesn't reuse its tests one by
// one on their earlier passes.
#[test]
fn go_tests_reused_one_by_one_run_again_when_a_variable_their_package_read_changes() {
    if !tools("Go", has_go()) {
        return;
    }
    let files = getenv_files("{mode: reuse, unit: test}");
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let r = new_repo(&files);
    let first = go_run(&r, &GO_TEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);

    let (path, text) = go_other(1);
    r.write(&path, &text);
    let set = go_run(&r, &GO_TEST, &[("HUSHRUN_FIXTURE_GO", "sour")]);
    assert_eq!(set.code, 0, "{}\n{}", set.stdout, set.stderr);
    // Both of `flavor`'s tests ran: neither was reused on its pass with the variable unset.
    let flavor: Vec<String> = set
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|e| {
            e["Action"] == "pass"
                && e["Package"] == "example.com/mod/flavor"
                && e["Test"].is_string()
        })
        .filter_map(|e| e["Test"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        flavor,
        ["TestOne", "TestTwo"],
        "{}\n{}",
        set.stdout,
        set.stderr
    );
}

/// What a step of [`rust_steps`] came to: test reuse's outcome, which build reuse must leave as
/// it is, and what build reuse did.
#[derive(Debug)]
#[cfg(unix)]
struct Step {
    /// Test reuse's own outcome: the exit code, the plan's line, the recorded units without the
    /// time they saved (a time, never the same twice), and whether an audit suspended reuse.
    tests: (i32, String, Value, bool),
    /// The lines that total what the compiles came to (B-20).
    build_totals: Vec<String>,
    /// Each start of `cargo` hushrun made, in order: whether it was COMMAND (inside the run), its
    /// RUSTC_WRAPPER, its HUSHRUN_BUILD_SESSION, whether that session's folder was there (it goes
    /// when the session finishes), and its arguments.
    starts: Vec<(bool, String, String, bool, String)>,
}

/// The steps of the Rust fixture that make test reuse plan, reuse, audit, and start COMMAND a
/// second time for a test file that failed though it had passed on the same files it read
/// (C-34): each step's outcome. `env` goes to every run; `cargo` on PATH is a shim that writes
/// down each start before it hands over to the real one.
#[cfg(unix)]
fn rust_steps(r: &Repo, env: &[(&str, &str)]) -> Vec<Step> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let real = std::env::split_paths(&path)
        .map(|dir| dir.join("cargo"))
        .find(|cargo| cargo.is_file())
        .expect("cargo on PATH");
    let shims = r.root.join("shims");
    std::fs::create_dir_all(&shims).unwrap();
    let log = r.root.join("starts.log");
    let shim = shims.join("cargo");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nthere=no\n[ -n \"$HUSHRUN_BUILD_SESSION\" ] && [ -d \"$HUSHRUN_BUILD_SESSION\" ] && there=yes\nprintf '%s|%s|%s|%s|%s\\n' \"$HUSHRUN_INSIDE_RUN\" \"$RUSTC_WRAPPER\" \"$HUSHRUN_BUILD_SESSION\" \"$there\" \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut dirs = vec![shims];
    dirs.extend(std::env::split_paths(&path));
    let shimmed = std::env::join_paths(dirs).unwrap();
    let shimmed = shimmed.to_str().expect("a PATH in UTF-8");
    let mut env: Vec<(&str, &str)> = env.to_vec();
    env.push(("PATH", shimmed));

    // `--no-fail-fast`: when the audited test file fails, nextest would otherwise cancel the tests
    // it hasn't started, and how many those are depends on how many threads it has and how fast
    // the others finish. A test binary cancelled before it started didn't run whole, so the run's
    // `ran` came to 4 or 5 of 5 by timing alone (on a 3-core macOS runner, not on 4-core Linux).
    let nextest = [
        "run",
        "test",
        "--",
        "cargo",
        "nextest",
        "run",
        "--workspace",
        "--locked",
        "--no-fail-fast",
    ];
    let mut steps = Vec::new();
    let mut step = |r: &Repo| {
        let _ = std::fs::remove_file(&log);
        let ran = r.run(&nextest, &env);
        let starts = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let mut parts = line.splitn(5, '|');
                let mut next = || parts.next().unwrap_or_default().to_string();
                (next() == "1", next(), next(), next() == "yes", next())
            })
            .collect();
        let mut units = r.newest_result()["units"].clone();
        if let Some(units) = units.as_object_mut() {
            units.remove("savedMs");
        }
        steps.push(Step {
            tests: (
                ran.code,
                ran.plan_line().to_string(),
                units,
                ran.stderr
                    .contains("reuse of test's test files is suspended"),
            ),
            build_totals: ran
                .stderr
                .lines()
                .filter(|line| {
                    line.strip_prefix("hushrun: build: ").is_some_and(|rest| {
                        rest.starts_with(|c: char| c.is_ascii_digit())
                            && (rest.contains(" built") || rest.contains(" reused"))
                    })
                })
                .map(str::to_string)
                .collect(),
            starts,
        });
    };
    // Everything runs, and passes.
    step(r);
    // One crate changes: its test binary runs, three are reused and one runs as a check.
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 1 + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    step(r);
    // The flag, which no dep-info names: the only test binary that could be reused is the one
    // audited, it fails, and COMMAND starts again for it alone; it fails again, so reuse is
    // suspended.
    r.write("flag", "");
    r.write(
        "crates/core/src/lib.rs",
        "pub fn answer() -> u32 { 40 + 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
    );
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 2 * 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    step(r);
    steps
}

/// The Rust fixture, with its lockfile committed.
#[cfg(unix)]
fn rust_repo() -> Repo {
    let r = new_repo(&rust_files());
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");
    r
}

// covers: B-2, B-20, C-34
// Build blocks and test reuse in the same run: with build reuse on, every start of COMMAND
// compiles through the wrapper in the run's one session, the second start for an audited test
// file too, and so does the plan's build, which is COMMAND's made first; the compiles are totalled
// once a run; and test reuse decides, records and suspends exactly as it does with build reuse
// off. A second checkout that shares the store reuses crates the first one compiled.
#[cfg(unix)]
#[test]
fn build_reuse_and_test_reuse_work_in_the_same_run() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let off = rust_repo();
    let without = rust_steps(&off, &[("HUSHRUN_BUILD", "off")]);
    let on = rust_repo();
    let store = on.root.join("store");
    let store = store.to_str().unwrap();
    let with = rust_steps(
        &on,
        &[("HUSHRUN_BUILD", "on"), ("HUSHRUN_BUILD_DIR", store)],
    );

    // Test reuse's own outcome, step by step, is the same with build reuse on or off.
    let outcomes = |steps: &[Step]| steps.iter().map(|s| s.tests.clone()).collect::<Vec<_>>();
    assert_eq!(outcomes(&with), outcomes(&without));
    // And it is what test reuse does on this fixture: plan, reuse, audit, suspend.
    let tests = outcomes(&without);
    assert_eq!(tests[0].0, 0, "{tests:?}");
    assert_eq!(
        tests[1].1,
        "hushrun: test: 2 of 5 test binaries run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert_eq!(
        tests[1].2,
        json!({"total": 5, "ran": 2, "reused": 3, "audited": 1})
    );
    assert_ne!(tests[2].0, 0, "{tests:?}");
    // Every test binary ran whole, the audited one too, which failed.
    assert_eq!(
        tests[2].2,
        json!({"total": 5, "ran": 5, "reused": 0, "audited": 1})
    );
    assert!(tests[2].3, "{tests:?}");

    for (n, (step, plain)) in with.iter().zip(&without).enumerate() {
        let commands: Vec<_> = step.starts.iter().filter(|s| s.0).collect();
        // Without build reuse, nothing is set and nothing is totalled.
        assert!(plain.build_totals.is_empty(), "{n}: {plain:?}");
        assert!(
            plain
                .starts
                .iter()
                .all(|s| s.1.is_empty() && s.2.is_empty()),
            "{n}: {plain:?}"
        );
        assert_eq!(
            plain.starts.iter().filter(|s| s.0).count(),
            commands.len(),
            "{n}: COMMAND starts as often either way: {plain:?}\n{step:?}"
        );
        // With it: the wrapper and one session on every start of COMMAND.
        assert!(!commands.is_empty(), "{n}: {step:?}");
        let session = &commands[0].2;
        assert!(!session.is_empty(), "{n}: {step:?}");
        for start in &commands {
            assert!(start.1.ends_with("/hushrun-rustc"), "{n}: {step:?}");
            assert_eq!(&start.2, session, "{n}: one session a run: {step:?}");
            assert!(start.3, "{n}: the session is still going: {step:?}");
        }
        // The plan's build, COMMAND's made first, compiles through the wrapper in that session.
        let plan_builds: Vec<_> = step
            .starts
            .iter()
            .take_while(|s| !s.0)
            .filter(|s| s.4.starts_with("test --no-run"))
            .collect();
        assert!(!plan_builds.is_empty(), "{n}: {step:?}");
        assert!(
            plan_builds
                .iter()
                .all(|s| s.1.ends_with("/hushrun-rustc") && &s.2 == session && s.3),
            "{n}: {step:?}"
        );
        // What the compiles came to, once.
        assert_eq!(step.build_totals.len(), 1, "{n}: {step:?}");
    }
    // The first run compiled every crate through the wrapper; the audited test file's second
    // start of COMMAND had it too.
    assert!(
        with[0].build_totals[0].contains(" built, none reused"),
        "{:?}",
        with[0]
    );
    assert_eq!(
        with[2].starts.iter().filter(|s| s.0).count(),
        2,
        "{:?}",
        with[2]
    );

    // Another checkout of the same files, sharing the store: crates are reused, and test reuse
    // ends as it did in the first checkout.
    let other = rust_repo();
    let again = rust_steps(
        &other,
        &[("HUSHRUN_BUILD", "on"), ("HUSHRUN_BUILD_DIR", store)],
    );
    assert_eq!(outcomes(&again), tests);
    assert!(
        again[0].build_totals[0].contains(" reused")
            && !again[0].build_totals[0].contains("none reused"),
        "{:?}",
        again[0]
    );
}
