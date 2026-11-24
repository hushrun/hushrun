//! Gradle's tasks as build blocks (B-77 to B-83), end to end: `hushrun run NAME -- gradle …` on
//! two machines (two checkouts, two stores, two Gradle user homes) against the fake build
//! protocol, with the real Gradle. They need Gradle 8 or later and a JDK, and say so and pass
//! where either is missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets. Nothing here
//! downloads: the build applies Gradle's own `java` plugin alone. (`--offline` would turn
//! Gradle's remote build cache off, hushrun's with it.)

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::fake_build::FakeBuild;
use hushrun_testkit::git;

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
    "HUSHRUN_LOG",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_BUILD_SHARE",
    "HUSHRUN_BUILD_AUDIT",
    "HUSHRUN_GRADLE",
    "HUSHRUN_GRADLE_HOOK",
    "HUSHRUN_GRADLE_CACHE",
    "HUSHRUN_GRADLE_CACHE_TOKEN",
    "HUSHRUN_UNITS",
    "RUSTC_WRAPPER",
    "GRADLE_USER_HOME",
    "GRADLE_OPTS",
];

/// Whether Gradle is here; else the test passes with a note, unless `HUSHRUN_TEST_TOOLS=required`.
fn have_gradle() -> bool {
    let found = Command::new("gradle")
        .arg("--version")
        .env(
            "GRADLE_USER_HOME",
            std::env::temp_dir().join("hushrun-gradle-probe"),
        )
        .output()
        .is_ok_and(|o| o.status.success());
    if found {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("Gradle is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: Gradle is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

/// A Java project with two classes, its settings file loading hushrun's script when `line`,
/// caching on, committed in a repository.
fn project(root: &Path, line: bool) -> PathBuf {
    let repo = root.join("repo");
    let write = |path: &str, text: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let mut settings = "rootProject.name = 'shop'\n".to_string();
    if line {
        settings.push_str(
            "if (System.getenv(\"HUSHRUN_GRADLE\")) apply from: System.getenv(\"HUSHRUN_GRADLE\")\n",
        );
    }
    write("settings.gradle", &settings);
    write("build.gradle", "plugins { id 'java' }\n");
    write("gradle.properties", "org.gradle.caching=true\n");
    write(".gitignore", "build/\n.gradle/\n");
    write(
        "src/main/java/shop/Cart.java",
        "package shop;\n\npublic class Cart {\n    public static int total() { return Price.of(2) * 3; }\n}\n",
    );
    write(
        "src/main/java/shop/Price.java",
        "package shop;\n\npublic class Price {\n    public static int of(int n) { return n * 7; }\n}\n",
    );
    git::init(&repo, "sha1");
    git::commit_all(&repo, "init");
    repo
}

/// A machine: its own checkout, hushrun cache (and so store), Gradle user home and machine key.
struct Machine {
    root: PathBuf,
    repo: PathBuf,
}

impl Machine {
    fn new(dir: &Path, name: &str, from: Option<&Path>, line: bool) -> Machine {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let repo = match from {
            None => project(&root, line),
            Some(from) => {
                let repo = root.join("checkout");
                git::run(
                    &root,
                    &[
                        "clone",
                        "-q",
                        "--no-hardlinks",
                        from.to_str().unwrap(),
                        repo.to_str().unwrap(),
                    ],
                    None,
                );
                repo
            }
        };
        std::fs::write(
            root.join("machine.pem"),
            hushrun_testkit::world::test_key(7).to_pem().as_str(),
        )
        .unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        Machine { root, repo }
    }

    fn hushrun(&self, fake: &FakeBuild) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.current_dir(&self.repo)
            .env("GRADLE_USER_HOME", self.root.join("gradle-home"))
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_MACHINE_KEY_FILE", self.root.join("machine.pem"))
            .env("HUSHRUN_HOST", "http://127.0.0.1:9")
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HUSHRUN_BACKEND", fake.url())
            .env("HUSHRUN_TOKEN", FakeBuild::TOKEN)
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_BUILD_AUDIT", "none")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"));
        cmd
    }

    /// `hushrun run build -- gradle ARGS`, with Gradle's plain console and no daemon.
    fn run(&self, fake: &FakeBuild, args: &[&str]) -> (i32, String, String) {
        let mut cmd = self.hushrun(fake);
        cmd.args(["run", "build", "--force", "--", "gradle"])
            .args(["--console=plain", "--no-daemon"])
            .args(args);
        let output = cmd.output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

/// The Gradle line of a run's summary.
fn gradle_line(stderr: &str) -> String {
    stderr
        .lines()
        .find(|l| l.contains("build: gradle: "))
        .unwrap_or_default()
        .to_string()
}

/// The `gradle/1` blocks the fake holds.
fn gradle_blocks(fake: &FakeBuild) -> usize {
    fake.begins()
        .iter()
        .filter(|b| b.kind == "gradle/1")
        .map(|b| b.blocks.len())
        .sum()
}

// covers: B-77, B-80, B-81, B-82, B-83
#[test]
fn a_task_one_machine_ran_comes_from_the_shared_store_on_another() {
    if !have_gradle() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, true);
    let (code, out, err) = a.run(&fake, &["compileJava", "--configuration-cache"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("> Task :compileJava\n"), "{out}");
    assert!(
        gradle_line(&err).contains("1 run and stored"),
        "{out}\n{err}"
    );
    assert!(gradle_blocks(&fake) > 0, "nothing went up: {err}");
    let open = fake.opens().into_iter().find(|o| o.kind == "gradle/1");
    assert!(
        open.is_some_and(|o| o.platform == "jvm"),
        "{:?}",
        fake.opens()
    );
    let close = fake
        .closes()
        .into_iter()
        .find(|c| c.kind == "gradle/1")
        .expect("a Gradle close");
    assert_eq!(close.manifest.len(), 1);

    // B: another checkout at another path, another store, another Gradle user home: the task
    // comes from what A ran.
    let b = Machine::new(dir.path(), "b", Some(&a.repo), true);
    let (code, out, err) = b.run(&fake, &["compileJava", "--configuration-cache"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        out.contains("> Task :compileJava FROM-CACHE"),
        "{out}\n{err}"
    );
    let line = gradle_line(&err);
    assert!(line.contains("from hushrun's cache"), "{err}");
    assert!(line.contains("from the shared store"), "{err}");
    assert!(
        b.repo
            .join("build/classes/java/main/shop/Cart.class")
            .is_file()
    );

    // Run again: Gradle reuses its configuration cache, since the cache's address is the same
    // from run to run, and its own local cache serves the task.
    std::fs::remove_dir_all(b.repo.join("build")).unwrap();
    let (code, out, err) = b.run(&fake, &["compileJava", "--configuration-cache"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("Reusing configuration cache."), "{out}");
    assert!(out.contains("> Task :compileJava FROM-CACHE"), "{out}");
}

// covers: B-77
#[test]
fn a_settings_file_without_the_line_is_said_and_shares_nothing() {
    if !have_gradle() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, false);
    let (code, out, err) = a.run(&fake, &["compileJava"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        err.contains("Gradle's tasks aren't shared in this run: settings.gradle doesn't load hushrun's script"),
        "{err}"
    );
    assert_eq!(gradle_blocks(&fake), 0);
    assert!(!fake.opens().iter().any(|o| o.kind == "gradle/1"));
}
