//! \[ADR 0112\] Doctests with `cargo test --doc` (C-74 to C-77): a unit is a library whose doctests
//! Cargo runs, named by its root file; what it read is what its unit test binary would have read;
//! a library whose doctests already passed on those files isn't handed to rustdoc. And the ways of
//! running Cargo's tests that hushrun can't read say why.
//!
//! The workspaces are tiny and need only Cargo, offline. hushrun's rustdoc builds with a rustdoc
//! that runs nothing, `true`: Unix only.
#![cfg(unix)]

#[path = "common/store.rs"]
mod store;

use std::path::PathBuf;
use std::process::Command;

use hushrun_testkit::git;

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
    "HUSHRUN_RUSTDOC",
    "RUSTDOC",
    "RUSTDOCFLAGS",
    "CARGO_TARGET_DIR",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";

const DOCTEST: &[&str] = &[
    "run",
    "doctest",
    "--",
    "cargo",
    "test",
    "--workspace",
    "--doc",
    "--locked",
    "--offline",
];

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// The plan's line for the check.
    fn plan_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| l.starts_with("hushrun: doctest: ") && l.contains(" run"))
            .unwrap_or_else(|| panic!("no plan line in:\n{}", self.stderr))
    }

    /// Whether rustdoc ran the doctests of the library in the folder `package`: it names each
    /// doctest by its file.
    fn ran(&self, package: &str) -> bool {
        let prefix = format!("test {package}/src/lib.rs - ");
        self.stdout.lines().any(|l| l.starts_with(&prefix))
    }
}

impl Repo {
    fn new(files: &[(String, String)]) -> Repo {
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
        let lock = Command::new("cargo")
            .args(["generate-lockfile", "--offline", "-q"])
            .current_dir(&r.repo)
            .env_remove("CARGO_TARGET_DIR")
            .status()
            .expect("cargo");
        assert!(lock.success());
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

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").expect("binary");
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", self.cache())
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
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
                || name.starts_with("RUSTC_WORKSPACE_WRAPPER")
            {
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

    fn reads(&self, unit: &str) -> Vec<String> {
        let map = store::map(&self.cache());
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }
}

fn package(name: &str, deps: &str, dev: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}\n[dev-dependencies]\n{dev}"
    )
}

/// A library whose one doctest says it returns `n`.
fn library(name: &str, n: u32) -> String {
    format!(
        "/// ```\n/// assert_eq!({name}::value(), {n});\n/// ```\npub fn value() -> u32 {{\n    {n}\n}}\n"
    )
}

/// A workspace of five libraries with doctests: `core`, whose doctests use its dev-dependency
/// `helper`, `app`, which depends on `core`, and `one`, `two` and `three` on their own.
fn workspace() -> Vec<(String, String)> {
    let files = vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/docs\nchecks:\n  doctest:\n    tests:\n      mode: reuse\n".to_string(),
        ),
        (".gitignore", "target/\n".to_string()),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"core\", \"helper\", \"app\", \"one\", \"two\", \"three\"]\nresolver = \"2\"\n".to_string(),
        ),
        // `helper`'s own doctests are off: it is only a dev-dependency here.
        (
            "helper/Cargo.toml",
            format!("{}\n[lib]\ndoctest = false\n", package("helper", "", "")),
        ),
        ("helper/src/lib.rs", "pub fn half(x: u32) -> u32 {\n    x / 2\n}\n".to_string()),
        (
            "core/Cargo.toml",
            package("core", "", "helper = { path = \"../helper\" }\n"),
        ),
        (
            "core/src/lib.rs",
            "/// ```\n/// assert_eq!(helper::half(core::value()), 21);\n/// ```\npub fn value() -> u32 {\n    42\n}\n".to_string(),
        ),
        (
            "app/Cargo.toml",
            package("app", "core = { path = \"../core\" }\n", ""),
        ),
        ("app/src/lib.rs", "/// ```\n/// assert_eq!(app::value(), 43);\n/// ```\npub fn value() -> u32 {\n    core::value() + 1\n}\n".to_string()),
    ];
    let mut files: Vec<(String, String)> = files
        .into_iter()
        .map(|(path, text)| (path.to_string(), text))
        .collect();
    for (n, name) in [(1, "one"), (2, "two"), (3, "three")] {
        files.push((format!("{name}/Cargo.toml"), package(name, "", "")));
        files.push((format!("{name}/src/lib.rs"), library(name, n)));
    }
    files
}

// covers: C-74, C-75, C-76
#[test]
fn a_librarys_doctests_that_passed_on_what_they_read_are_not_built_again() {
    let r = Repo::new(&workspace());
    let first = r.run(DOCTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.plan_line(),
        "hushrun: doctest: all 5 doctest crates run: none had passed here yet on the files it read"
    );
    for package in ["core", "app", "one", "two", "three"] {
        assert!(first.ran(package), "{package}: {}", first.stdout);
    }
    // A library's doctests read its sources, and those of the libraries they may link: its
    // dependencies' and its dev-dependencies'.
    let core = r.reads("core/src/lib.rs");
    assert!(core.contains(&"helper/src/lib.rs".to_string()), "{core:?}");
    let app = r.reads("app/src/lib.rs");
    assert!(app.contains(&"core/src/lib.rs".to_string()), "{app:?}");
    assert!(!app.contains(&"helper/src/lib.rs".to_string()), "{app:?}");

    // `app` changes: its doctests run; of the four others, one runs as a check and three aren't
    // handed to rustdoc at all.
    r.write(
        "app/src/lib.rs",
        "/// ```\n/// assert_eq!(app::value(), 43);\n/// ```\npub fn value() -> u32 {\n    1 + core::value()\n}\n",
    );
    git::commit_all(&r.repo, "app");
    let second = r.run(DOCTEST, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        second.plan_line(),
        "hushrun: doctest: 2 of 5 doctest crates run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(second.ran("app"), "{}", second.stdout);
    let doctests_run = second
        .stdout
        .lines()
        .filter(|l| l.starts_with("test ") && l.contains("/src/lib.rs - "))
        .count();
    assert_eq!(doctests_run, 2, "{}\n{}", second.stdout, second.stderr);

    // A dev-dependency changes: the doctests that use it run, and those of `app`, which links
    // `core` but not its dev-dependencies, don't for that.
    r.write(
        "helper/src/lib.rs",
        "pub fn half(x: u32) -> u32 {\n    x >> 1\n}\n",
    );
    git::commit_all(&r.repo, "helper");
    let third = r.run(DOCTEST, &[]);
    assert_eq!(third.code, 0, "{}\n{}", third.stdout, third.stderr);
    assert!(third.ran("core"), "{}", third.stdout);
    assert!(
        third
            .plan_line()
            .starts_with("hushrun: doctest: 2 of 5 doctest crates run; 3 already passed"),
        "{}",
        third.plan_line()
    );
}

// covers: C-76
#[test]
fn a_doctest_that_fails_runs_again_until_it_passes() {
    let r = Repo::new(&workspace());
    let first = r.run(DOCTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    r.write(
        "two/src/lib.rs",
        &library("two", 2).replace("{\n    2\n}", "{\n    5\n}"),
    );
    git::commit_all(&r.repo, "break two");
    let failed = r.run(DOCTEST, &[]);
    assert_ne!(failed.code, 0, "{}\n{}", failed.stdout, failed.stderr);
    assert!(failed.ran("two"), "{}", failed.stdout);
    // The same code again: `two`'s doctests didn't pass, so they run again.
    let again = r.run(DOCTEST, &[]);
    assert_ne!(again.code, 0, "{}\n{}", again.stdout, again.stderr);
    assert!(again.ran("two"), "{}", again.stdout);
}

// covers: C-74, C-77
#[test]
fn what_hushrun_cant_read_of_cargos_tests_says_why_and_runs_as_it_was() {
    let r = Repo::new(&workspace());
    // A rustdoc of the person's own: hushrun doesn't stand in front of it.
    let rustdoc = String::from_utf8(
        Command::new("rustc")
            .args(["--print", "sysroot"])
            .current_dir(&r.repo)
            .output()
            .expect("rustc")
            .stdout,
    )
    .expect("utf-8");
    let rustdoc = format!("{}/bin/rustdoc", rustdoc.trim());
    let theirs = r.run(DOCTEST, &[("RUSTDOC", &rustdoc)]);
    assert_eq!(theirs.code, 0, "{}\n{}", theirs.stdout, theirs.stderr);
    assert!(
        theirs.stderr.contains(
            "hushrun: doctest: test files aren't observed: COMMAND's environment sets RUSTDOC"
        ),
        "{}",
        theirs.stderr
    );
    assert!(theirs.ran("one"), "{}", theirs.stdout);
    // An archive's binaries, and a partition by count: COMMAND runs as it was, whatever it does.
    for (args, why) in [
        (
            &[
                "run",
                "unit",
                "--",
                "cargo",
                "nextest",
                "run",
                "--archive-file",
                "none.tar.zst",
            ][..],
            "cargo nextest runs binaries built elsewhere (--archive-file)",
        ),
        (
            &[
                "run",
                "unit",
                "--",
                "cargo",
                "nextest",
                "run",
                "--partition",
                "count:1/2",
            ][..],
            "cargo nextest splits the tests by their place in the list (--partition count:1/2)",
        ),
    ] {
        r.write(
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/docs\nchecks:\n  unit:\n    tests:\n      mode: reuse\n",
        );
        let ran = r.run(args, &[]);
        assert!(
            ran.stderr
                .contains(&format!("hushrun: unit: test files aren't observed: {why}")),
            "{}",
            ran.stderr
        );
    }
}
