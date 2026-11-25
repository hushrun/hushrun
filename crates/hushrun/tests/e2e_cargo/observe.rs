//! \[ADR 0068\] `tests: observe` of Cargo's test binaries, end to end (C-30 to C-33, C-39): each
//! start of a test binary goes through the target runner, the hushrun binary itself, and the
//! programs `units::program` puts on `PATH` are the binary too. These were `units.rs`'s; its
//! pytest tests are in-process now.
//!
//! The binary is started with the environment these tests always gave it, which keeps the
//! person's `HOME`, where rustup and Cargo find their toolchains.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

use hushrun_testkit::git;
use serde_json::Value;

use super::python::tools;
use super::store;

/// Environment variables a test must never inherit.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "CLAUDE_CODE_REMOTE",
    "CURSOR_AGENT_SOCKET",
    "COPILOT_AGENT_SESSION_ID",
    "CODEX_PROXY_CERT",
    "HUSHRUN_SANDBOX",
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
    "PYTEST_ADDOPTS",
    "PYTHONPATH",
    "CARGO_TARGET_DIR",
    "npm_lifecycle_event",
    // B-1, B-2: the developer's build mode, store and wrapper stay out: a test run that cargo
    // itself compiles through the wrapper doesn't pass its session on to a fixture's cargo.
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_CC",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "RUSTC_WRAPPER",
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

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let r = Repo {
            _dir: dir,
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

    fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// `hushrun` in the repository, as a local run with no inherited configuration, and no
    /// variable an outer Cargo or nextest set, which a Cargo it starts would read.
    fn hushrun(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").expect("binary");
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", self.cache())
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
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
                || name.starts_with("RUSTC_WORKSPACE_WRAPPER")
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

    /// The check's map, as this cache's store holds it (ADR 0072).
    fn map(&self) -> Value {
        store::map(&self.cache())
    }

    fn reads(&self, unit: &str) -> Vec<String> {
        let map = self.map();
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    /// How many unit results this machine holds.
    fn unit_results(&self) -> usize {
        let dir = self.cache().join("results/units/v0");
        let Ok(keys) = std::fs::read_dir(dir) else {
            return 0;
        };
        keys.flatten()
            .map(|k| std::fs::read_dir(k.path()).map_or(0, |files| files.count()))
            .sum()
    }

    /// The run directories a stopped or finished run left behind.
    fn run_dirs(&self) -> usize {
        std::fs::read_dir(self.cache().join("units/runs")).map_or(0, |d| d.count())
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// The `observe` line: what `tests: reuse` would have done.
    fn observe_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| l.contains("with `tests: reuse`"))
            .unwrap_or_else(|| panic!("no observe line in:\n{}", self.stderr))
    }
}

fn has_nextest() -> bool {
    Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A Cargo workspace: a library crate, a binary crate whose integration test runs the binary, a
/// crate whose test opens a `.rs` file under `testdata/` as data, and an example.
fn rust_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/ws\nchecks:\n  test:\n    tests: observe\n",
        ),
        (".gitignore", "target/\n"),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/core\", \"crates/app\", \"crates/other\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/core/Cargo.toml",
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"core_lib\"\n",
        ),
        (
            "crates/core/src/lib.rs",
            "mod util;\npub fn answer() -> u32 { util::base() + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
        ),
        ("crates/core/src/util.rs", "pub fn base() -> u32 { 41 }\n"),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ncore-lib = { path = \"../core\" }\n",
        ),
        (
            "crates/app/src/main.rs",
            "fn main() { println!(\"{}\", core_lib::answer()); }\n",
        ),
        (
            "crates/app/examples/demo.rs",
            "fn main() { println!(\"demo\"); }\n",
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
            "crates/other/tests/reads_data.rs",
            "#[test]\nfn reads_testdata() {\n    let path = concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/testdata/fixture.rs\");\n    assert!(std::fs::read_to_string(path).unwrap().contains(\"fn fixture\"));\n}\n",
        ),
        ("crates/other/testdata/fixture.rs", "fn fixture() {}\n"),
    ]
}

// covers: C-30, C-31, C-32, C-33
#[test]
fn rust_test_binaries_are_observed_from_their_dep_info() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let r = Repo::new(&rust_files());
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");
    // Four test threads: the four tests start at once, so the slow failure below stops nothing.
    let nextest = [
        "run",
        "test",
        "--",
        "cargo",
        "nextest",
        "run",
        "--workspace",
        "--locked",
        "--test-threads",
        "4",
    ];

    let first = r.run(&nextest, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(
        first
            .observe_line()
            .contains("all 5 test binaries would have run: none had passed here yet"),
        "{}",
        first.stderr
    );
    // A binary crate's integration test reads the binary's sources and its library's, and the
    // package's example: it may run any of them.
    let runs_binary = r.reads("crates/app/tests/runs_binary.rs");
    for file in [
        "crates/app/src/main.rs",
        "crates/core/src/lib.rs",
        "crates/core/src/util.rs",
        "crates/app/examples/demo.rs",
    ] {
        assert!(runs_binary.contains(&file.to_string()), "{runs_binary:?}");
    }
    // A library's unit tests read the library, and nothing of the other crates.
    assert_eq!(
        r.reads("crates/core/src/lib.rs"),
        ["crates/core/src/lib.rs", "crates/core/src/util.rs"]
    );
    // A `.rs` file no build names is data: the rest set, never a source.
    let map = r.map();
    assert!(
        map["rest"]["crates/other/testdata/fixture.rs"].is_string(),
        "{map:#}"
    );
    assert!(
        !map["sources"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("crates/other/testdata/fixture.rs"))
    );
    assert_eq!(r.unit_results(), 5);

    // A change to one crate runs its two test binaries.
    r.write("crates/other/src/lib.rs", "pub fn two() -> u32 { 1 + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n");
    let other = r.run(&nextest, &[]);
    assert!(
        other
            .observe_line()
            .contains("2 of 5 test binaries would have run; 3 already passed"),
        "{}",
        other.stderr
    );

    // An example is read by its package's integration tests only.
    r.write(
        "crates/app/examples/demo.rs",
        "fn main() { println!(\"demo 2\"); }\n",
    );
    let example = r.run(&nextest, &[]);
    assert!(
        example
            .observe_line()
            .contains("1 of 5 test binaries would have run; 4 already passed"),
        "{}",
        example.stderr
    );

    // A failing test, with nextest's fail-fast: the exit code is nextest's, and the binaries that
    // ran whole and passed are recorded all the same. Here `other` changes too, so that only this
    // failing run can have recorded its passes; the failure comes late, once they finished.
    let core = "mod util;\npub fn answer() -> u32 { util::base() + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n";
    r.write(
        "crates/core/src/lib.rs",
        &core.replace(
            "{ assert_eq!(super::answer(), 42); }",
            "{ std::thread::sleep(std::time::Duration::from_secs(3)); assert_eq!(super::answer(), 43); }",
        ),
    );
    r.write("crates/other/src/lib.rs", "pub fn two() -> u32 { 2 * 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n");
    let failing = r.run(&nextest, &[]);
    assert_eq!(failing.code, 100, "{}", failing.stderr);
    assert!(
        failing
            .observe_line()
            .contains("5 test binaries would have run"),
        "{}",
        failing.stderr
    );
    // Fixed: every test file already passed on the files it read, `other`'s in the failing run.
    r.write("crates/core/src/lib.rs", core);
    let fixed = r.run(&nextest, &[]);
    assert_eq!(fixed.code, 0, "{}", fixed.stderr);
    assert!(
        fixed
            .observe_line()
            .contains("0 of 5 test binaries would have run; 5 already passed"),
        "{}",
        fixed.stderr
    );

    // The data file a test opens changes: the rest set, so everything.
    r.write("crates/other/testdata/fixture.rs", "fn fixture() { }\n");
    let data = r.run(&nextest, &[]);
    assert!(
        data.observe_line().contains(
            "all 5 test binaries would have run: crates/other/testdata/fixture.rs changed, and any test may read it"
        ),
        "{}",
        data.stderr
    );

    // Plain `cargo test` goes through the same runner, one start per binary.
    let plain = [
        "run",
        "test",
        "--",
        "cargo",
        "test",
        "--workspace",
        "--locked",
        "-q",
    ];
    let first_plain = r.run(&plain, &[]);
    assert_eq!(first_plain.code, 0, "{}", first_plain.stderr);
    assert!(
        first_plain
            .observe_line()
            .contains("all 5 test binaries would have run")
    );
    r.write("crates/other/src/lib.rs", "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n");
    let second_plain = r.run(&plain, &[]);
    assert!(
        second_plain
            .observe_line()
            .contains("2 of 5 test binaries would have run; 3 already passed"),
        "{}",
        second_plain.stderr
    );
    assert_eq!(r.run_dirs(), 0);
}

// covers: C-32
#[cfg(unix)]
#[test]
fn the_target_runner_passes_everything_through() {
    let dir = tempfile::tempdir().unwrap();
    let units = dir.path().join("units");
    std::fs::create_dir_all(&units).unwrap();
    let runner = |args: &[&str]| {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").unwrap();
        cmd.arg("__hushrun-test-runner")
            .args(args)
            .env("HUSHRUN_UNITS", &units)
            .env("CARGO_TARGET_X_RUNNER", "set-by-hushrun");
        cmd.output().unwrap()
    };
    // The exit code, and what the test sees of the environment: no directory of hushrun's, and no
    // runner variable when the environment had none.
    let out = runner(&[
        "X",
        "0",
        "sh",
        "-c",
        "echo \"[$HUSHRUN_UNITS][${CARGO_TARGET_X_RUNNER-unset}]\"; exit 3",
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[][unset]\n");
    // A chained runner starts the binary, and the test sees it as its runner.
    let out = runner(&["X", "1", "env", "sh", "-c", "echo $CARGO_TARGET_X_RUNNER"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "env\n");
    // A list goes on as it came, and is kept.
    let out = runner(&[
        "X",
        "0",
        "sh",
        "-c",
        "printf 'a: test\\nb: benchmark\\n'",
        "--list",
    ]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a: test\nb: benchmark\n"
    );
    // A signal ends the runner as it ended the test.
    let out = runner(&["X", "0", "sh", "-c", "kill -TERM $$"]);
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(out.status.signal(), Some(15));
    let files: Vec<Value> = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .collect();
    assert_eq!(files.len(), 4);
    assert!(files.iter().any(|f| f["invocation"]["exit"] == 3));
    assert!(files.iter().any(|f| f["invocation"]["signal"] == 15));
    assert!(
        files
            .iter()
            .any(|f| f["invocation"]["listed"] == serde_json::json!(["a"]))
    );
    // Without a directory it still runs the test, and writes nothing.
    let mut cmd = assert_cmd::Command::cargo_bin("hushrun").unwrap();
    let out = cmd
        .args(["__hushrun-test-runner", "X", "0", "sh", "-c", "exit 0"])
        .env_remove("HUSHRUN_UNITS")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
}

// covers: C-39
#[cfg(unix)]
#[test]
fn a_program_on_path_starts_the_real_one_with_path_as_it_was() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // A directory of hushrun's programs, which a real program is never taken from, first on PATH;
    // then the real one, a fake tool that says its arguments and environment.
    let programs = root.join("programs");
    std::fs::create_dir_all(&programs).unwrap();
    std::fs::write(programs.join(".hushrun-programs"), b"").unwrap();
    let real = root.join("real");
    std::fs::create_dir_all(&real).unwrap();
    for at in [&programs, &real] {
        let tool = at.join("faketool");
        std::fs::write(
            &tool,
            "#!/bin/sh\necho \"[$*][$PATH][${HUSHRUN_PROGRAMS-unset}][${HUSHRUN_PROGRAMS_PATH-unset}]\"\nexit 4\n",
        )
        .unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let was = format!("{}:/usr/bin:/bin", real.display());
    let program = |name: &str, args: &[&str]| {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").unwrap();
        cmd.arg("__hushrun-program")
            .arg(name)
            .args(args)
            .env("PATH", format!("{}:{was}", programs.display()))
            .env("HUSHRUN_PROGRAMS", &programs)
            .env("HUSHRUN_PROGRAMS_PATH", format!("={was}"))
            .env("HUSHRUN_UNITS", root.join("units"));
        cmd.output().unwrap()
    };
    // The real program, with no program of hushrun's named for it: its arguments as they came,
    // its exit code, PATH as COMMAND had it, and neither of hushrun's variables.
    let out = program("faketool", &["test", "--parallel"]);
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("[test --parallel][{was}][unset][unset]\n")
    );
    // None on PATH: 127, as a shell says.
    let out = program("nothere", &[]);
    assert_eq!(out.status.code(), Some(127));
}
