//! \[ADR 0084\] Test classes with Gradle (C-30 to C-34, C-39, C-46): a unit is a test class, named
//! by its source file, a test is one of its methods, and what a class read is every source file
//! below its project and each project its classpath is built from.
//!
//! The fixtures are generated multi-project builds with JUnit 5 or JUnit 4, run by the Gradle that
//! `HUSHRUN_TEST_GRADLE` names (a `gradle` program) with `--offline`, against the Gradle user home
//! `HUSHRUN_TEST_GRADLE_USER_HOME` names, which holds JUnit 5.11.4 and JUnit 4.13.2: CI fills it
//! once before the tests. They say so and pass where either is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets. Daemons stay warm between tests, as they do on a
//! person's machine.
//!
//! The Android test needs Android's SDK as well, named by `HUSHRUN_TEST_ANDROID_SDK`, and a Gradle
//! user home that holds Android's Gradle plugin, 9.1.1 or the version `HUSHRUN_TEST_ANDROID_PLUGIN`
//! names (8.13.2 for Gradle 8); without the SDK it says so and passes, even when tools are
//! required.

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
    "HUSHRUN_GRADLE",
    "HUSHRUN_GRADLE_HOOK",
    "HUSHRUN_TESTS_LEAVE_OUT",
    "HUSHRUN_TESTS_EVENTS",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "GRADLE_OPTS",
    "JAVA_TOOL_OPTIONS",
    "JAVA_OPTS",
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
];

const NO_HOST: &str = "http://127.0.0.1:9";
const LINE: &str =
    r#"if (System.getenv("HUSHRUN_GRADLE")) apply from: System.getenv("HUSHRUN_GRADLE")"#;

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
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
            cache: root.join("cache"),
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
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        if let Some(home) = gradle_user_home() {
            cmd.env("GRADLE_USER_HOME", home);
        }
        for var in CLEARED {
            cmd.env_remove(var);
        }
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

    /// Every unit result this machine recorded, `pass` or `fail`, by count.
    fn unit_results(&self) -> std::collections::BTreeMap<String, usize> {
        let mut files = Vec::new();
        collect(&self.cache.join("results/units/v0"), &mut files);
        let mut out = std::collections::BTreeMap::new();
        for f in files {
            let v: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
            let result = v["result"].as_str().unwrap_or("?").to_string();
            *out.entry(result).or_default() += 1;
        }
        out
    }

    /// The test cases of a class Gradle's XML report says ran, by name.
    fn xml_cases(&self, project: &str, class: &str) -> Vec<String> {
        let path = self
            .repo
            .join(project)
            .join(format!("build/test-results/test/TEST-{class}.xml"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        text.split("<testcase name=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next().map(str::to_string))
            .collect()
    }

    /// Removes the builds' outputs, which the tests are done with.
    fn clean(&self) {
        for entry in std::fs::read_dir(&self.repo)
            .into_iter()
            .flatten()
            .flatten()
        {
            let _ = std::fs::remove_dir_all(entry.path().join("build"));
        }
        let _ = std::fs::remove_dir_all(self.repo.join("build"));
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        self.clean();
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

fn line<'a>(ran: &'a Ran, start: &str) -> &'a str {
    ran.stderr
        .lines()
        .find(|l| l.starts_with(start))
        .unwrap_or_else(|| {
            panic!(
                "no line starting {start:?} in:\n{}\n{}",
                ran.stderr, ran.stdout
            )
        })
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

/// The Gradle the tests run.
fn gradle() -> String {
    std::env::var("HUSHRUN_TEST_GRADLE").unwrap_or_else(|_| "gradle".into())
}

/// The Gradle user home the tests run offline against.
fn gradle_user_home() -> Option<String> {
    std::env::var("HUSHRUN_TEST_GRADLE_USER_HOME")
        .ok()
        .filter(|h| !h.is_empty())
}

fn has_gradle() -> bool {
    static HERE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HERE.get_or_init(|| {
        gradle_user_home().is_some()
            && Command::new(gradle())
                .arg("--version")
                .env_remove("GRADLE_OPTS")
                .output()
                .is_ok_and(|o| o.status.success())
    })
}

const NEEDS: &str = "Gradle (HUSHRUN_TEST_GRADLE) and a Gradle user home with JUnit (HUSHRUN_TEST_GRADLE_USER_HOME)";

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Framework {
    Junit5,
    Junit4,
}

/// The build's root files: the settings file with its line, and every project a Java project.
fn build(framework: Framework, projects: &[&str], tests: &str) -> Vec<(String, String)> {
    let includes = projects
        .iter()
        .map(|p| format!("'{p}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let deps = match framework {
        Framework::Junit5 => {
            "    testImplementation platform('org.junit:junit-bom:5.11.4')\n    testImplementation 'org.junit.jupiter:junit-jupiter'\n    testRuntimeOnly 'org.junit.platform:junit-platform-launcher'\n"
        }
        Framework::Junit4 => "    testImplementation 'junit:junit:4.13.2'\n",
    };
    let runner = match framework {
        Framework::Junit5 => "useJUnitPlatform()",
        Framework::Junit4 => "useJUnit()",
    };
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "build/\n.gradle/\n.kotlin/\n"),
        file(
            "settings.gradle",
            &format!("rootProject.name = 'shop'\ninclude {includes}\n{LINE}\n"),
        ),
        file(
            "build.gradle",
            &format!(
                "subprojects {{\n  apply plugin: 'java'\n  repositories {{ mavenCentral() }}\n  dependencies {{\n{deps}  }}\n  tasks.withType(Test).configureEach {{ {runner} }}\n}}\n"
            ),
        ),
    ]
}

/// A shop: `lib` prices, `app` has a cart that uses them, `other` stands alone, each with tests.
fn shop(tests: &str) -> Vec<(String, String)> {
    let mut files = build(Framework::Junit5, &["app", "lib", "other"], tests);
    files.extend([
        file("app/build.gradle", "dependencies { implementation project(':lib') }\n"),
        file(
            "lib/src/main/java/shop/lib/Price.java",
            "package shop.lib;\npublic class Price { public static int of(int x) { return x * 2; } }\n",
        ),
        file(
            "lib/src/test/java/shop/lib/PriceTest.java",
            "package shop.lib;\nimport org.junit.jupiter.api.*;\nclass PriceTest { @Test void doubles() { Assertions.assertEquals(6, Price.of(3)); } }\n",
        ),
        file(
            "app/src/main/java/shop/app/Cart.java",
            "package shop.app;\npublic class Cart { public static int total(int... xs) { int n = 0; for (int x : xs) n += shop.lib.Price.of(x); return n; } }\n",
        ),
        file(
            "app/src/test/java/shop/app/CartTest.java",
            "package shop.app;\nimport org.junit.jupiter.api.*;\nclass CartTest {\n  @Test void total() { Assertions.assertEquals(6, Cart.total(1, 2)); }\n  @Test void flips() { Assertions.assertNull(System.getenv(\"FAIL\")); }\n  @Nested class Inner {\n    @Test void a() {}\n    @Test void b() {}\n  }\n}\n",
        ),
        file("README.md", "A shop.\n"),
    ]);
    for name in ["A", "B", "C"] {
        files.push(file(
            &format!("other/src/test/java/shop/other/{name}Test.java"),
            &format!("package shop.other;\nimport org.junit.jupiter.api.*;\nclass {name}Test {{ @Test void works() {{}} }}\n"),
        ));
    }
    files
}

/// COMMAND: the tests, offline, each failing task reported, with `extra`.
fn command<'a>(gradle: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "run",
        "unit",
        "--",
        gradle,
        "--offline",
        "--console=plain",
        "test",
        "--continue",
    ];
    args.extend_from_slice(extra);
    args
}

const CART: &str = "app/src/test/java/shop/app/CartTest.java";
const PRICE: &str = "lib/src/test/java/shop/lib/PriceTest.java";

// covers: C-30, C-31, C-32, C-34, C-46
#[test]
fn test_classes_are_observed_then_reused_and_a_change_runs_the_classes_that_read_it() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = Repo::new(&shop("reuse"));
    let first = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(
        first
            .stderr
            .contains("every test file runs: no run here has listed Gradle's test classes yet"),
        "{}",
        first.stderr
    );
    let map = r.map();
    // A class read every file below its project's directory and below each project its
    // classpath is built from: its own, its project's sources and lib's.
    let reads = |unit: &str| -> Vec<String> {
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit}: {map:#}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        reads(CART),
        [
            "app/src/main/java/shop/app/Cart.java",
            CART,
            "lib/src/main/java/shop/lib/Price.java",
            PRICE,
        ]
    );
    // A project is read as its source directories (ADR 0084).
    assert_eq!(
        map["units"][CART]["dirs"],
        json!([
            "app/src/main/java",
            "app/src/main/resources",
            "app/src/test/java",
            "app/src/test/resources",
            "lib/src/main/java",
            "lib/src/main/resources",
            "lib/src/test/java",
            "lib/src/test/resources"
        ])
    );
    assert_eq!(
        reads("other/src/test/java/shop/other/ATest.java"),
        [
            "other/src/test/java/shop/other/ATest.java",
            "other/src/test/java/shop/other/BTest.java",
            "other/src/test/java/shop/other/CTest.java",
        ]
    );
    // Build scripts and the rest are in the rest set; the test classes' files are the list.
    assert!(map["rest"]["build.gradle"].is_string(), "{map:#}");
    assert!(map["rest"]["settings.gradle"].is_string(), "{map:#}");
    assert!(
        map["sources"]
            .as_array()
            .unwrap()
            .contains(&json!("lib/src/main/java/shop/lib/Price.java"))
    );
    assert_eq!(map["unitFiles"].as_array().unwrap().len(), 5, "{map:#}");
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 5, "ran": 5, "reused": 0})
    );

    // lib changes: lib's and app's classes run, other's were reused but the one audited.
    r.write(
        "lib/src/main/java/shop/lib/Price.java",
        "package shop.lib;\npublic class Price { public static int of(int x) { return x + x; } }\n",
    );
    let changed = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(changed.code, 0, "{}\n{}", changed.stdout, changed.stderr);
    assert_eq!(
        line(&changed, "hushrun: unit: "),
        "hushrun: unit: 3 of 5 test files run; 2 already passed on the files they read, and 1 more that did runs as a check"
    );
    let result = r.newest_result();
    let saved = result["units"]["savedMs"].clone();
    assert_eq!(
        result["units"],
        json!({"total": 5, "ran": 3, "reused": 2, "savedMs": saved})
    );
    // Gradle didn't run the reused classes: two of other's three wrote no report.
    let ran_in_other = ["A", "B", "C"]
        .iter()
        .filter(|n| {
            !r.xml_cases("other", &format!("shop.other.{n}Test"))
                .is_empty()
        })
        .count();
    assert_eq!(ran_in_other, 1, "{}", changed.stdout);
}

// covers: C-39, C-46, C-32
#[test]
fn junit5_tests_are_reused_one_by_one_and_a_class_whose_setup_failed_reruns() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    // lib keeps no test here: a task whose list didn't change is up to date, and Gradle wouldn't
    // run an audited class of it (ADR 0084).
    let mut files = shop("{mode: reuse, unit: test}");
    files.retain(|(path, _)| path != PRICE);
    files.push(file(
        "app/src/test/java/shop/app/SetupTest.java",
        "package shop.app;\nimport org.junit.jupiter.api.*;\nclass SetupTest {\n  @BeforeAll static void up() { Assertions.assertNull(System.getenv(\"FAIL\")); }\n  @Test void works() {}\n}\n",
    ));
    let r = Repo::new(&files);
    let cc = ["--configuration-cache"];
    let first = r.run(&command(&gradle, &cc), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let map = r.map();
    assert_eq!(
        map["units"][CART]["tests"],
        json!({"shop.app.CartTest$Inner.a": "passed", "shop.app.CartTest$Inner.b": "passed",
               "shop.app.CartTest.flips": "failed", "shop.app.CartTest.total": "passed"}),
        "{map:#}"
    );
    // A failing @BeforeAll fails its class, and no test of it recorded a pass.
    assert!(
        map["units"]["app/src/test/java/shop/app/SetupTest.java"]
            .get("tests")
            .is_none(),
        "{map:#}"
    );

    // The same files, the flip fixed: CartTest's own key failed, so its tests are asked one by
    // one; of the three that passed, one runs as a check. SetupTest's tests aren't known: it runs.
    let second = r.run(&command(&gradle, &cc), &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.stdout.contains("Reusing configuration cache"),
        "{}",
        second.stdout
    );
    assert_eq!(
        line(&second, "hushrun: unit: "),
        "hushrun: unit: 3 of 5 test files run; 2 already passed on the files they read, and so did 2 tests of the others, and 2 more that did run as a check"
    );
    // \[C-39\] Tests are counted where Gradle named them, and a class reused whole, which Gradle
    // named none of, by the tests this machine knows of it: every one reused.
    let result = r.newest_result();
    assert_eq!(
        result["units"]["tests"],
        json!({"total": 8, "ran": 4, "reused": 4})
    );
    // A nested class's method was left out too (Gradle 9's filter can't do it; the hook can):
    // of Inner's two, at most the audited one ran.
    let inner = r.xml_cases("app", "shop.app.CartTest$Inner");
    assert!(inner.len() <= 1, "{inner:?}");
    assert_eq!(
        r.map()["units"]["app/src/test/java/shop/app/SetupTest.java"]["tests"],
        json!({"shop.app.SetupTest.works": "passed"})
    );
}

// covers: C-39, C-46
#[test]
fn junit4_tests_are_reused_one_by_one_through_gradles_filter() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let mut files = build(Framework::Junit4, &["app"], "{mode: reuse, unit: test}");
    files.extend([
        file(
            "app/src/test/java/shop/app/CartTest.java",
            "package shop.app;\nimport org.junit.*;\npublic class CartTest {\n  @Test public void total() {}\n  @Test public void flips() { Assert.assertNull(System.getenv(\"FAIL\")); }\n}\n",
        ),
        file(
            "app/src/test/java/shop/app/ParamTest.java",
            "package shop.app;\nimport org.junit.*;\nimport org.junit.runner.RunWith;\nimport org.junit.runners.Parameterized;\nimport java.util.*;\n@RunWith(Parameterized.class)\npublic class ParamTest {\n  @Parameterized.Parameters(name = \"{index}: x={0}\") public static Collection<Object[]> data() { return Arrays.asList(new Object[][]{{1},{2},{3}}); }\n  private final int x; public ParamTest(int x) { this.x = x; }\n  @Test public void small() { Assert.assertTrue(x < 4); }\n  @Test public void flips() { Assert.assertNull(System.getenv(\"FAIL\")); }\n}\n",
        ),
        file(
            "app/src/test/java/shop/app/PlainTest.java",
            "package shop.app;\nimport org.junit.*;\npublic class PlainTest { @Test public void works() {} }\n",
        ),
    ]);
    let r = Repo::new(&files);
    let first = r.run(&command(&gradle, &[]), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        r.map()["units"]["app/src/test/java/shop/app/ParamTest.java"]["tests"],
        json!({"shop.app.ParamTest.flips": "failed", "shop.app.ParamTest.small": "passed"})
    );
    let second = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    // total and small passed: one runs as a check, one is reused; PlainTest passed whole, and is
    // the one test file audited.
    assert_eq!(
        line(&second, "hushrun: unit: "),
        "hushrun: unit: 4 of 5 tests run, in 3 of 3 test files; 1 already passed on the files it read, and 2 more that did run as a check"
    );
    // Gradle's patterns left out either `total`, or every invocation of `small`.
    let total = r.xml_cases("app", "shop.app.CartTest");
    let small: Vec<String> = r
        .xml_cases("app", "shop.app.ParamTest")
        .into_iter()
        .filter(|c| c.starts_with("small"))
        .collect();
    assert!(
        !total.contains(&"total".to_string()) || small.is_empty(),
        "{total:?} {small:?}"
    );
    assert!(small.is_empty() || small.len() == 3, "{small:?}");
}

// covers: C-46, C-32
#[test]
fn a_task_gradle_didnt_execute_records_nothing() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = Repo::new(&shop("observe"));
    let first = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    let before = r.map();
    // A file no test reads changes: the check misses, and Gradle finds every test task up to date.
    r.write("README.md", "A shop, again.\n");
    let again = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(again.code, 0, "{}\n{}", again.stdout, again.stderr);
    assert!(
        again.stdout.contains("> Task :app:test UP-TO-DATE"),
        "{}",
        again.stdout
    );
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 5, "ran": 0, "reused": 0})
    );
    assert_eq!(r.map()["units"], before["units"]);
}

// covers: C-46
#[cfg(unix)]
#[test]
fn a_settings_file_without_the_line_observes_nothing_and_says_so() {
    use std::os::unix::fs::PermissionsExt as _;
    // No Gradle is needed: the run stops at the settings file.
    let r = Repo::new(&[
        file("hushrun.yaml", &yaml("observe")),
        file("settings.gradle", "rootProject.name = 'shop'\n"),
        file("gradlew", "#!/bin/sh\necho \"gradlew $*\"\n"),
    ]);
    std::fs::set_permissions(
        r.repo.join("gradlew"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let ran = r.run(&["run", "unit", "--", "./gradlew", "test"], &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr.contains(&format!(
            "hushrun: unit: test files aren't observed: Gradle loads hushrun's adapter from one line in settings.gradle, which `hushrun init` proposes: `{LINE}`"
        )),
        "{}",
        ran.stderr
    );
    assert!(ran.stdout.contains("gradlew test"), "{}", ran.stdout);
}

// covers: C-34, C-39, C-46
#[test]
fn an_audited_test_runs_again_alone_under_a_warm_daemon_and_the_configuration_cache() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let counts = tempfile::tempdir().unwrap();
    let count_dir = counts.path().to_string_lossy().into_owned();
    // One class, two tests: one fails while FAIL is set, the other while HIDDEN is, which no file
    // says, as a missed dependency would. Each counts its runs in a file outside the repository.
    let mut files = build(Framework::Junit5, &["app"], "{mode: reuse, unit: test}");
    files.push(file(
        "app/src/test/java/shop/app/TwoTest.java",
        "package shop.app;\nimport org.junit.jupiter.api.*;\nimport java.nio.file.*;\nclass TwoTest {\n  static void count(String name) throws Exception { Files.write(Paths.get(System.getenv(\"COUNTS\"), name), \"x\".getBytes(), StandardOpenOption.CREATE, StandardOpenOption.APPEND); }\n  @Test void ok() throws Exception { count(\"ok\"); Assertions.assertNull(System.getenv(\"HIDDEN\")); }\n  @Test void flips() throws Exception { count(\"flips\"); Assertions.assertNull(System.getenv(\"FAIL\")); }\n}\n",
    ));
    let r = Repo::new(&files);
    let cc = ["--configuration-cache"];
    let first = r.run(
        &command(&gradle, &cc),
        &[("FAIL", "1"), ("COUNTS", &count_dir)],
    );
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    // ok passed on these files, and is the one test that could be reused: it runs as a check, and
    // fails. It runs once more, alone, from the second run's own list, and fails again.
    let caught = r.run(
        &command(&gradle, &cc),
        &[("HIDDEN", "1"), ("COUNTS", &count_dir)],
    );
    assert_eq!(caught.code, 1, "{}\n{}", caught.stdout, caught.stderr);
    assert_eq!(
        line(&caught, "hushrun: unit: all"),
        "hushrun: unit: all 2 tests run; 1 that already passed on the files it read runs as a check"
    );
    let name = "app/src/test/java/shop/app/TwoTest.java::shop.app.TwoTest.ok";
    assert!(
        caught.stderr.contains(&format!(
            "hushrun: unit: {name} failed, though it passed before on the same files it read: running it once more"
        )),
        "{}",
        caught.stderr
    );
    assert!(
        caught.stderr.contains(&format!(
            "hushrun: unit: {name} failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        )),
        "{}",
        caught.stderr
    );
    // Both starts of COMMAND reused the configuration cache, and the second ran ok alone.
    assert_eq!(
        caught.stdout.matches("Reusing configuration cache").count(),
        2,
        "{}",
        caught.stdout
    );
    let count = |name: &str| {
        std::fs::read_to_string(counts.path().join(name))
            .unwrap_or_default()
            .len()
    };
    assert_eq!((count("ok"), count("flips")), (3, 2));
    assert_eq!(store::disagreements(&r.cache), [name]);
}

// covers: C-46
#[test]
fn with_isolated_projects_on_nothing_is_observed_and_the_build_is_as_it_was() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    // A build Isolated Projects takes: each project configures itself.
    let r = Repo::new(&[
        file("hushrun.yaml", &yaml("observe")),
        file(".gitignore", "build/\n.gradle/\n"),
        file(
            "settings.gradle",
            &format!("rootProject.name = 'shop'\ninclude 'app'\n{LINE}\n"),
        ),
        file(
            "app/build.gradle",
            "plugins { id 'java' }\nrepositories { mavenCentral() }\ndependencies {\n  testImplementation platform('org.junit:junit-bom:5.11.4')\n  testImplementation 'org.junit.jupiter:junit-jupiter'\n  testRuntimeOnly 'org.junit.platform:junit-platform-launcher'\n}\ntest { useJUnitPlatform() }\n",
        ),
        file(
            "app/src/test/java/shop/app/OneTest.java",
            "package shop.app;\nimport org.junit.jupiter.api.*;\nclass OneTest { @Test void works() {} }\n",
        ),
    ]);
    let ran = r.run(
        &command(&gradle, &["-Dorg.gradle.unsafe.isolated-projects=true"]),
        &[],
    );
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout.contains(
            "hushrun: Isolated Projects is on, so hushrun looks at no test class of this build"
        ),
        "{}",
        ran.stdout
    );
    assert!(r.newest_result().get("units").is_none(), "{}", ran.stderr);
}

/// The Android SDK and a Gradle user home that holds Android's Gradle plugin.
fn android_sdk() -> Option<String> {
    std::env::var("HUSHRUN_TEST_ANDROID_SDK")
        .ok()
        .filter(|s| Path::new(s).is_dir())
}

// covers: C-46, C-39
#[test]
fn an_android_librarys_local_unit_tests_are_reused_one_by_one() {
    let Some(sdk) = android_sdk() else {
        eprintln!(
            "note: skipping: HUSHRUN_TEST_ANDROID_SDK names no Android SDK (a manual check, ADR 0084)"
        );
        return;
    };
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let plugin = std::env::var("HUSHRUN_TEST_ANDROID_PLUGIN").unwrap_or_else(|_| "9.1.1".into());
    let r = Repo::new(&[
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(
            ".gitignore",
            "build/\n.gradle/\n.kotlin/\nlocal.properties\n",
        ),
        // As the spike's Android sample is, whose Gradle user home the tests run offline against.
        file(
            "settings.gradle.kts",
            "pluginManagement { repositories { google(); mavenCentral(); gradlePluginPortal() } }\ndependencyResolutionManagement { repositories { google(); mavenCentral() } }\nSystem.getenv(\"HUSHRUN_GRADLE\")?.let { apply(from = it) }\nrootProject.name = \"droid\"\ninclude(\":core\")\n",
        ),
        file("gradle.properties", "android.useAndroidX=true\n"),
        file(
            "build.gradle.kts",
            &format!(
                "plugins {{\n  id(\"com.android.application\") version \"{plugin}\" apply false\n  id(\"com.android.library\") version \"{plugin}\" apply false\n}}\n"
            ),
        ),
        file(
            "core/build.gradle.kts",
            "plugins { id(\"com.android.library\") }\nandroid {\n  namespace = \"shop.core\"\n  compileSdk = 36\n  defaultConfig { minSdk = 24 }\n}\ndependencies { testImplementation(\"junit:junit:4.13.2\") }\n",
        ),
        file(
            "core/src/main/java/shop/core/Core.java",
            "package shop.core;\npublic class Core { public static int twice(int x) { return x * 2; } }\n",
        ),
        file(
            "core/src/test/java/shop/core/CoreTest.java",
            "package shop.core;\nimport org.junit.*;\npublic class CoreTest {\n  @Test public void twice() { Assert.assertEquals(4, Core.twice(2)); }\n  @Test public void again() { Assert.assertEquals(6, Core.twice(3)); }\n  @Test public void flips() { Assert.assertNull(System.getenv(\"FAIL\")); }\n}\n",
        ),
    ]);
    let args = [
        "run",
        "unit",
        "--",
        gradle.as_str(),
        "--offline",
        "--console=plain",
        ":core:testDebugUnitTest",
    ];
    let env = [("ANDROID_HOME", sdk.as_str())];
    let first = r.run(&args, &[env[0], ("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        r.map()["units"]["core/src/test/java/shop/core/CoreTest.java"]["tests"],
        json!({"shop.core.CoreTest.again": "passed", "shop.core.CoreTest.flips": "failed",
               "shop.core.CoreTest.twice": "passed"})
    );
    let second = r.run(&args, &env);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        line(&second, "hushrun: unit: "),
        "hushrun: unit: 2 of 3 tests run, in 1 of 1 test file; 1 already passed on the files it read, and 1 more that did runs as a check"
    );
}

/// A TestNG build: `SetupTest`'s `@BeforeSuite` fails without a database (`NO_DB`), which skips
/// every test of the suite; `OrderTest.total` fails whenever it runs; nine green classes.
fn testng_build(tests: &str) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "build/\n.gradle/\n"),
        file(
            "settings.gradle",
            &format!("rootProject.name = 'ng'\n{LINE}\n"),
        ),
        file(
            "build.gradle",
            "plugins { id 'java' }\nrepositories { mavenCentral() }\ndependencies { testImplementation 'org.testng:testng:7.10.2' }\ntest { useTestNG() }\n",
        ),
        file(
            "src/test/java/ng/SetupTest.java",
            "package ng;\nimport org.testng.annotations.*;\npublic class SetupTest {\n  @BeforeSuite public void db() { if (System.getenv(\"NO_DB\") != null) throw new IllegalStateException(\"no database\"); }\n  @Test public void setupWorks() {}\n}\n",
        ),
        file(
            "src/test/java/ng/OrderTest.java",
            "package ng;\nimport org.testng.annotations.*;\nimport static org.testng.Assert.*;\npublic class OrderTest {\n  @Test public void total() { assertEquals(1, 2); }\n}\n",
        ),
    ];
    for n in 1..=9 {
        files.push(file(
            &format!("src/test/java/ng/Green{n}Test.java"),
            &format!("package ng;\nimport org.testng.annotations.*;\npublic class Green{n}Test {{\n  @Test public void green() {{}}\n}}\n"),
        ));
    }
    files
}

// covers: C-32, C-46
#[test]
fn tests_testng_skipped_after_a_failed_suite_setup_never_pass() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = Repo::new(&testng_build("reuse"));
    // The suite's setup fails: TestNG skips every test, OrderTest's failing one among them.
    let first = r.run(&command(&gradle, &[]), &[("NO_DB", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    // Only the setup's failure is recorded: no skipped class passed.
    assert_eq!(
        r.unit_results(),
        std::collections::BTreeMap::from([("fail".into(), 1)]),
        "{}\n{}",
        first.stdout,
        first.stderr
    );
    // The database is back: OrderTest runs, and fails, as plain Gradle says.
    let second = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.stdout.contains("OrderTest > total FAILED"),
        "{}",
        second.stdout
    );
}

/// `proc`'s annotation processor generates `app.GenValue`, whose `value()` is `proc.Values.VALUE`;
/// `app` names `proc` only as its `annotationProcessor`. Nine green classes in app.
fn processor_build(value: u32) -> Vec<(String, String)> {
    let mut files = build(Framework::Junit5, &["proc", "app"], "reuse");
    files.extend([
        file(
            "app/build.gradle",
            "dependencies { annotationProcessor project(':proc') }\n",
        ),
        file(
            "proc/src/main/java/proc/GenProcessor.java",
            r#"package proc;
import java.io.Writer;
import java.util.Set;
import javax.annotation.processing.*;
import javax.lang.model.SourceVersion;
import javax.lang.model.element.TypeElement;
@SupportedAnnotationTypes("*")
public class GenProcessor extends AbstractProcessor {
  private boolean done;
  @Override public SourceVersion getSupportedSourceVersion() { return SourceVersion.latestSupported(); }
  @Override public boolean process(Set<? extends TypeElement> a, RoundEnvironment env) {
    if (done) return false; done = true;
    try (Writer w = processingEnv.getFiler().createSourceFile("app.GenValue").openWriter()) {
      w.write("package app; public class GenValue { public static int value() { return " + Values.VALUE + "; } }");
    } catch (Exception e) { throw new RuntimeException(e); }
    return false;
  }
}
"#,
        ),
        file(
            "proc/src/main/resources/META-INF/services/javax.annotation.processing.Processor",
            "proc.GenProcessor\n",
        ),
        file(
            "proc/src/test/java/proc/ProcTest.java",
            "package proc;\nimport org.junit.jupiter.api.*;\nclass ProcTest { @Test void loads() { new GenProcessor(); } }\n",
        ),
        file("app/src/main/java/app/App.java", "package app;\npublic class App {}\n"),
        file(
            "app/src/test/java/app/AppTest.java",
            "package app;\nimport org.junit.jupiter.api.*;\nclass AppTest { @Test void generated() { Assertions.assertEquals(1, GenValue.value()); } }\n",
        ),
        processor_values(value),
    ]);
    for n in 1..=9 {
        files.push(file(
            &format!("app/src/test/java/app/Green{n}Test.java"),
            &format!("package app;\nimport org.junit.jupiter.api.*;\nclass Green{n}Test {{ @Test void green() {{}} }}\n"),
        ));
    }
    files
}

fn processor_values(value: u32) -> (String, String) {
    file(
        "proc/src/main/java/proc/Values.java",
        &format!(
            "package proc;\npublic class Values {{ public static final int VALUE = {value}; }}\n"
        ),
    )
}

// covers: C-30, C-46
#[test]
fn a_change_to_an_annotation_processor_of_the_build_runs_the_tests_of_the_code_it_generates() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = Repo::new(&processor_build(1));
    let first = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // app's classes read proc, whose processor app's compiler runs.
    let map = r.map();
    assert_eq!(
        map["units"]["app/src/test/java/app/AppTest.java"]["dirs"],
        json!([
            "app/src/main/java",
            "app/src/main/resources",
            "app/src/test/java",
            "app/src/test/resources",
            "proc/src/main/java",
            "proc/src/main/resources",
            "proc/src/test/java",
            "proc/src/test/resources"
        ]),
        "{map:#}"
    );
    let (path, text) = processor_values(2);
    r.write(&path, &text);
    let second = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.stdout.contains("AppTest > generated() FAILED"),
        "{}",
        second.stdout
    );
}

/// `app`'s `CrashTest`, whose test JVM dies when `CRASH` names where: `after` in its `@AfterAll`,
/// once both its tests passed, `test` inside its second test. Nine green classes in `other`.
fn crash_build() -> Vec<(String, String)> {
    let mut files = build(Framework::Junit5, &["app", "other"], "reuse");
    files.push(file(
        "app/src/test/java/shop/app/CrashTest.java",
        "package shop.app;\nimport org.junit.jupiter.api.*;\n@TestMethodOrder(MethodOrderer.MethodName.class)\nclass CrashTest {\n  static void crashIf(String where) { if (where.equals(System.getenv(\"CRASH\"))) Runtime.getRuntime().halt(3); }\n  @Test void a() {}\n  @Test void b() { crashIf(\"test\"); }\n  @AfterAll static void after() { crashIf(\"after\"); }\n}\n",
    ));
    for n in 1..=9 {
        files.push(file(
            &format!("other/src/test/java/shop/other/Green{n}Test.java"),
            &format!("package shop.other;\nimport org.junit.jupiter.api.*;\nclass Green{n}Test {{ @Test void green() {{}} }}\n"),
        ));
    }
    files
}

// covers: C-32, C-46
#[test]
fn a_test_jvm_that_died_records_no_pass_for_the_class_it_was_in() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    for (round, place) in ["after", "test"].into_iter().enumerate() {
        let r = Repo::new(&crash_build());
        // The test JVM dies: Gradle fails the task, and no test of it failed.
        let ran = r.run(&command(&gradle, &[]), &[("CRASH", place)]);
        assert_eq!(ran.code, 1, "{place}: {}\n{}", ran.stdout, ran.stderr);
        assert!(
            ran.stderr.contains("finished with non-zero exit value 3"),
            "{place}: {}",
            ran.stderr
        );
        // other's classes passed; CrashTest recorded nothing.
        assert_eq!(
            r.unit_results(),
            std::collections::BTreeMap::from([("pass".into(), 9)]),
            "{place} ({round}): {}\n{}",
            ran.stdout,
            ran.stderr
        );
    }
}

// covers: C-39, C-46
#[test]
fn spock_features_whose_names_differ_after_a_bracket_are_left_out_one_by_one() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = Repo::new(&[
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "build/\n.gradle/\n"),
        file(
            "settings.gradle",
            &format!("rootProject.name = 'spock'\n{LINE}\n"),
        ),
        file(
            "build.gradle",
            "plugins { id 'groovy' }\nrepositories { mavenCentral() }\ndependencies {\n  testImplementation platform('org.spockframework:spock-bom:2.3-groovy-4.0')\n  testImplementation 'org.spockframework:spock-core'\n  testImplementation 'org.apache.groovy:groovy:4.0.24'\n  testRuntimeOnly 'org.junit.platform:junit-platform-launcher'\n}\ntest { useJUnitPlatform() }\n",
        ),
        file(
            "src/test/groovy/ex/ParseSpec.groovy",
            "package ex\nimport spock.lang.*\nclass ParseSpec extends Specification {\n  def \"parses [a]\"() { expect: true }\n  def \"parses [c]\"() { expect: true }\n  def \"parses [b]\"() { expect: 1 == 2 }\n}\n",
        ),
    ]);
    let first = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    // Nothing changed: one of the two that passed is left out, the other runs as a check, and
    // the one that failed runs, and fails again.
    let second = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    let cases = r.xml_cases(".", "ex.ParseSpec");
    assert!(cases.contains(&"parses [b]".to_string()), "{cases:?}");
    assert_eq!(cases.len(), 2, "{cases:?}");
}

// covers: C-39, C-46
#[test]
fn junit4_tests_of_a_class_in_the_default_package_leave_out_nothing_of_a_class_named_alike() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    // Two classes named CalcTest, one in the default package, with tests of the same names.
    let adds = |when: &str| -> String {
        (1..=9)
            .map(|n| format!("  @Test public void add{n}() {{ Assert.assertNull(System.getenv(\"{when}\")); }}\n"))
            .collect()
    };
    let mut files = build(Framework::Junit4, &["app"], "{mode: reuse, unit: test}");
    files.extend([
        file(
            "app/src/test/java/CalcTest.java",
            &format!(
                "import org.junit.*;\npublic class CalcTest {{\n{}  @Test public void other() {{ Assert.assertNull(System.getenv(\"FAIL\")); }}\n}}\n",
                adds("NEVER")
            ),
        ),
        file(
            "app/src/test/java/p/CalcTest.java",
            &format!("package p;\nimport org.junit.*;\npublic class CalcTest {{\n{}}}\n", adds("P_FAIL")),
        ),
    ]);
    let r = Repo::new(&files);
    let first = r.run(&command(&gradle, &[]), &[("FAIL", "1"), ("P_FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    // The default package's add tests passed and may be left out; p.CalcTest's failed, and run.
    let second = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    let cases = r.xml_cases("app", "p.CalcTest");
    assert_eq!(cases.len(), 9, "{cases:?}\n{}", second.stderr);
}

// covers: C-32, C-46
#[cfg(unix)]
#[test]
fn a_gradle_wrapper_that_starts_gradle_again_keeps_what_the_first_start_failed() {
    use std::os::unix::fs::PermissionsExt as _;
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let counts = tempfile::tempdir().unwrap();
    let count_dir = counts.path().to_string_lossy().into_owned();
    // A wrapper that starts Gradle again when it fails, and a test that fails the first time.
    let mut files = build(Framework::Junit5, &["app"], "{mode: reuse, unit: test}");
    files.extend([
        file(
            "gradlew",
            "#!/bin/sh\n\"$REAL_GRADLE\" \"$@\" || \"$REAL_GRADLE\" \"$@\"\n",
        ),
        file(
            "app/src/test/java/shop/app/FlakyTest.java",
            "package shop.app;\nimport org.junit.jupiter.api.*;\nimport java.nio.file.*;\nclass FlakyTest {\n  @Test void flaky() throws Exception { Path p = Paths.get(System.getenv(\"COUNTS\"), \"ran\"); boolean again = Files.exists(p); Files.write(p, new byte[0]); Assertions.assertTrue(again); }\n  @Test void steady() {}\n}\n",
        ),
    ]);
    let r = Repo::new(&files);
    std::fs::set_permissions(
        r.repo.join("gradlew"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let ran = r.run(
        &[
            "run",
            "unit",
            "--",
            "./gradlew",
            "--offline",
            "--console=plain",
            "test",
            "--continue",
        ],
        &[("REAL_GRADLE", &gradle), ("COUNTS", &count_dir)],
    );
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(
        ran.stdout.matches("FlakyTest > flaky() FAILED").count(),
        1,
        "{}",
        ran.stdout
    );
    // The second start passed it; the first failed it: a flaky test is a failed one.
    assert_eq!(
        r.map()["units"]["app/src/test/java/shop/app/FlakyTest.java"]["tests"],
        json!({"shop.app.FlakyTest.flaky": "failed", "shop.app.FlakyTest.steady": "passed"}),
        "{}",
        ran.stderr
    );
}

/// \[C-39\] Test classes whose tests look alike: `Base`'s `inherited`, which `ATest`, `BTest`,
/// `OuterTest` and `OuterTest.Inner` each inherit, failing in a class whose simple name with
/// `_FAIL` is set; and `same`, a method of `OuterTest` and of its nested `Inner`. `FLIP` fails
/// `ATest` and `OuterTest`, so their other tests are listed one by one; `o1` to `o9` keep the audit
/// off the look-alikes, mostly.
const LOOK_ALIKE_J5: &[(&str, &str)] = &[
    (
        "shop/Base.java",
        "package shop;\nimport org.junit.jupiter.api.*;\npublic abstract class Base {\n  @Test void inherited() { Assertions.assertNull(System.getenv(getClass().getSimpleName() + \"_FAIL\")); }\n}\n",
    ),
    (
        "shop/ATest.java",
        "package shop;\nimport org.junit.jupiter.api.*;\nclass ATest extends Base {\n  @Test void flip() { Assertions.assertNull(System.getenv(\"FLIP\")); }\n  @Test void o1() {}\n  @Test void o2() {}\n  @Test void o3() {}\n  @Test void o4() {}\n  @Test void o5() {}\n  @Test void o6() {}\n  @Test void o7() {}\n  @Test void o8() {}\n  @Test void o9() {}\n}\n",
    ),
    (
        "shop/BTest.java",
        "package shop;\nimport org.junit.jupiter.api.*;\nclass BTest extends Base {\n  @Test void own() {}\n}\n",
    ),
    (
        "shop/OuterTest.java",
        "package shop;\nimport org.junit.jupiter.api.*;\nclass OuterTest extends Base {\n  @Test void flip() { Assertions.assertNull(System.getenv(\"FLIP\")); }\n  @Test void same() {}\n  @Nested class Inner extends Base {\n    @Test void same() { Assertions.assertNull(System.getenv(\"INNER_FAIL\")); }\n  }\n}\n",
    ),
];
/// Run 1's variables: the flips, and the look-alikes that fail and so aren't listed.
const LOOK_ALIKE_FIRST: &[(&str, &str)] = &[
    ("FLIP", "1"),
    ("BTest_FAIL", "1"),
    ("Inner_FAIL", "1"),
    ("INNER_FAIL", "1"),
];
/// Run 2's: the flips fixed, the look-alikes not listed still failing.
const LOOK_ALIKE_SECOND: &[(&str, &str)] = &[
    ("BTest_FAIL", "1"),
    ("Inner_FAIL", "1"),
    ("INNER_FAIL", "1"),
];

// covers: C-39, C-46
#[test]
fn junit5_tests_that_look_alike_in_subclasses_and_nested_classes_are_left_out_one_by_one() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let mut files = build(Framework::Junit5, &["app"], "{mode: reuse, unit: test}");
    for (path, text) in LOOK_ALIKE_J5 {
        files.push(file(&format!("app/src/test/java/{path}"), text));
    }
    let r = Repo::new(&files);
    let first = r.run(&command(&gradle, &[]), LOOK_ALIKE_FIRST);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let second = r.run(&command(&gradle, &[]), LOOK_ALIKE_SECOND);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    // The look-alikes that passed were listed, and left out: all 13 that passed but the audit's.
    let reused = r.newest_result()["units"]["tests"]["reused"].clone();
    assert_eq!(reused, 12, "{}", second.stderr);
    let failed = |class: &str, test: &str| {
        let path = r
            .repo
            .join(format!("app/build/test-results/test/TEST-{class}.xml"));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        text.split("<testcase ")
            .any(|case| case.starts_with(&format!("name=\"{test}\"")) && case.contains("<failure"))
    };
    for (class, test) in [
        ("shop.BTest", "inherited()"),
        ("shop.OuterTest$Inner", "inherited()"),
        ("shop.OuterTest$Inner", "same()"),
    ] {
        assert!(failed(class, test), "{class} {test}\n{}", second.stdout);
    }
}

/// \[C-39\] JUnit 4 test classes whose tests look alike: abstract `Base`'s `inherited`, which
/// `ATest` and `BTest` inherit; concrete `BaseTest`, whose `inherited` and `flip` `SubTest`
/// inherits; and concrete `Base2Test` and `Sub2Test`, the same. `inherited` fails in a class whose
/// simple name with `_FAIL` is set; `FLIP` fails `flip`.
const LOOK_ALIKE_J4: &[(&str, &str)] = &[
    (
        "shop/Base.java",
        "package shop;\nimport org.junit.*;\npublic abstract class Base {\n  @Test public void inherited() { Assert.assertNull(System.getenv(getClass().getSimpleName() + \"_FAIL\")); }\n}\n",
    ),
    (
        "shop/ATest.java",
        "package shop;\nimport org.junit.*;\npublic class ATest extends Base {\n  @Test public void flip() { Assert.assertNull(System.getenv(\"FLIP\")); }\n  @Test public void o1() {}\n  @Test public void o2() {}\n  @Test public void o3() {}\n  @Test public void o4() {}\n  @Test public void o5() {}\n  @Test public void o6() {}\n  @Test public void o7() {}\n  @Test public void o8() {}\n  @Test public void o9() {}\n}\n",
    ),
    (
        "shop/BTest.java",
        "package shop;\nimport org.junit.*;\npublic class BTest extends Base {\n  @Test public void own() {}\n}\n",
    ),
    (
        "shop/BaseTest.java",
        "package shop;\nimport org.junit.*;\npublic class BaseTest {\n  @Test public void inherited() { Assert.assertNull(System.getenv(getClass().getSimpleName() + \"_FAIL\")); }\n  @Test public void flip() { Assert.assertNull(System.getenv(\"FLIP\")); }\n}\n",
    ),
    (
        "shop/SubTest.java",
        "package shop;\npublic class SubTest extends BaseTest {}\n",
    ),
    (
        "shop/Base2Test.java",
        "package shop;\nimport org.junit.*;\npublic class Base2Test {\n  @Test public void inherited() { Assert.assertNull(System.getenv(getClass().getSimpleName() + \"_FAIL\")); }\n  @Test public void flip() { Assert.assertNull(System.getenv(\"FLIP\")); }\n}\n",
    ),
    (
        "shop/Sub2Test.java",
        "package shop;\npublic class Sub2Test extends Base2Test {}\n",
    ),
];
/// Run 1's variables: the flips, and the look-alikes that fail and so aren't listed.
const LOOK_ALIKE_J4_FIRST: &[(&str, &str)] = &[
    ("FLIP", "1"),
    ("BTest_FAIL", "1"),
    ("SubTest_FAIL", "1"),
    ("Base2Test_FAIL", "1"),
];
/// Run 2's: the flips fixed, the look-alikes not listed still failing.
const LOOK_ALIKE_J4_SECOND: &[(&str, &str)] = &[
    ("BTest_FAIL", "1"),
    ("SubTest_FAIL", "1"),
    ("Base2Test_FAIL", "1"),
];

// covers: C-39, C-46
#[test]
fn junit4_tests_that_look_alike_in_subclasses_are_left_out_one_by_one_through_patterns() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let mut files = build(Framework::Junit4, &["app"], "{mode: reuse, unit: test}");
    for (path, text) in LOOK_ALIKE_J4 {
        files.push(file(&format!("app/src/test/java/{path}"), text));
    }
    let r = Repo::new(&files);
    let first = r.run(&command(&gradle, &[]), LOOK_ALIKE_J4_FIRST);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let second = r.run(&command(&gradle, &[]), LOOK_ALIKE_J4_SECOND);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    let reused = r.newest_result()["units"]["tests"]["reused"].clone();
    assert_eq!(reused, 12, "{}", second.stderr);
    for class in ["shop.BTest", "shop.SubTest", "shop.Base2Test"] {
        let path = r
            .repo
            .join(format!("app/build/test-results/test/TEST-{class}.xml"));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            text.split("<testcase ")
                .any(|case| case.starts_with("name=\"inherited\"") && case.contains("<failure")),
            "{class}: {text}\n{}",
            second.stdout
        );
    }
}

// covers: C-39, C-46
#[test]
fn spock_features_specs_inherit_are_left_out_one_by_one() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let feature = "  def \"inherited\"() { expect: System.getenv(getClass().simpleName + \"_FAIL\") == null }\n";
    let flip = "  def \"flip\"() { expect: System.getenv(\"FLIP\") == null }\n";
    let others: String = (1..=9)
        .map(|n| format!("  def \"o{n}\"() {{ expect: true }}\n"))
        .collect();
    let r = Repo::new(&[
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "build/\n.gradle/\n"),
        file(
            "settings.gradle",
            &format!("rootProject.name = 'spock'\n{LINE}\n"),
        ),
        file(
            "build.gradle",
            "plugins { id 'groovy' }\nrepositories { mavenCentral() }\ndependencies {\n  testImplementation platform('org.spockframework:spock-bom:2.3-groovy-4.0')\n  testImplementation 'org.spockframework:spock-core'\n  testImplementation 'org.apache.groovy:groovy:4.0.24'\n  testRuntimeOnly 'org.junit.platform:junit-platform-launcher'\n}\ntest { useJUnitPlatform() }\n",
        ),
        // An abstract base spec two specs extend, and a concrete one a spec extends.
        file(
            "src/test/groovy/ex/BaseSpec.groovy",
            &format!(
                "package ex\nimport spock.lang.*\nabstract class BaseSpec extends Specification {{\n{feature}}}\n"
            ),
        ),
        file(
            "src/test/groovy/ex/ASpec.groovy",
            &format!("package ex\nclass ASpec extends BaseSpec {{\n{flip}{others}}}\n"),
        ),
        file(
            "src/test/groovy/ex/BSpec.groovy",
            "package ex\nclass BSpec extends BaseSpec {\n  def \"own\"() { expect: true }\n}\n",
        ),
        file(
            "src/test/groovy/ex/Base2Spec.groovy",
            &format!(
                "package ex\nimport spock.lang.*\nclass Base2Spec extends Specification {{\n{feature}{flip}}}\n"
            ),
        ),
        file(
            "src/test/groovy/ex/Sub2Spec.groovy",
            "package ex\nclass Sub2Spec extends Base2Spec {}\n",
        ),
    ]);
    let first = r.run(
        &command(&gradle, &[]),
        &[("FLIP", "1"), ("BSpec_FAIL", "1"), ("Sub2Spec_FAIL", "1")],
    );
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let second = r.run(
        &command(&gradle, &[]),
        &[("BSpec_FAIL", "1"), ("Sub2Spec_FAIL", "1")],
    );
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    // ASpec's ten and BSpec's own, and Base2Spec's inherited: all 12 but the audit's.
    let reused = r.newest_result()["units"]["tests"]["reused"].clone();
    assert_eq!(reused, 11, "{}\n{}", second.stderr, second.stdout);
    for class in ["ex.BSpec", "ex.Sub2Spec"] {
        let path = r
            .repo
            .join(format!("build/test-results/test/TEST-{class}.xml"));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            text.split("<testcase ")
                .any(|case| case.starts_with("name=\"inherited\"") && case.contains("<failure")),
            "{class}: {text}\n{}",
            second.stdout
        );
    }
}

// covers: C-30, C-46
#[test]
fn a_jvm_file_no_project_of_the_build_owns_is_the_rest() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    // A tool beside app's sources, which a script builds and publishes, not app's build; one at
    // the root; and a second build of its own, below app, that nothing here includes.
    let mut files = shop("reuse");
    files.extend([
        file(
            "app/tools/Gen.java",
            "package tools;\npublic class Gen { public static int v() { return 1; } }\n",
        ),
        file(
            "tools/Root.java",
            "package tools;\npublic class Root { public static int v() { return 1; } }\n",
        ),
        file(
            "app/plugin/settings.gradle",
            "rootProject.name = 'plugin'\n",
        ),
        file(
            "app/plugin/src/main/java/plugin/P.java",
            "package plugin;\npublic class P {}\n",
        ),
    ]);
    let r = Repo::new(&files);
    let first = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    let map = r.map();
    for path in [
        "app/tools/Gen.java",
        "tools/Root.java",
        "app/plugin/src/main/java/plugin/P.java",
    ] {
        assert!(map["rest"][path].is_string(), "{path}: {map:#}");
    }
    // A change to one runs every test class, as far as hushrun goes: Gradle finds the tasks up
    // to date, since the file is none of their inputs.
    r.write(
        "app/tools/Gen.java",
        "package tools;\npublic class Gen { public static int v() { return 2; } }\n",
    );
    let changed = r.run(&command(&gradle, &[]), &[]);
    assert_eq!(changed.code, 0, "{}\n{}", changed.stdout, changed.stderr);
    assert_eq!(
        line(&changed, "hushrun: unit: "),
        "hushrun: unit: all 5 test files run: app/tools/Gen.java changed, and any test may read it"
    );
}

// covers: C-30, C-46
#[test]
fn a_task_of_one_project_keeps_the_sources_it_read_through_another_on_its_list() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = Repo::new(&shop("reuse"));
    let only = |args: &[&'static str]| -> Vec<&'static str> {
        let mut out = vec!["run", "unit", "--"];
        out.extend_from_slice(args);
        out
    };
    let g: &'static str = Box::leak(gradle.into_boxed_str());
    // Only app's tests: lib's sources, which app's classes read, are on their list.
    let first = r.run(
        &only(&[g, "--offline", "--console=plain", ":app:test"]),
        &[],
    );
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    let map = r.map();
    let reads = map["units"][CART]["reads"].as_array().unwrap().clone();
    assert!(
        reads.contains(&json!("lib/src/main/java/shop/lib/Price.java")),
        "{map:#}"
    );
    // lib changes: app's tests run.
    r.write(
        "lib/src/main/java/shop/lib/Price.java",
        "package shop.lib;\npublic class Price { public static int of(int x) { return x + x; } }\n",
    );
    let second = r.run(
        &only(&[g, "--offline", "--console=plain", ":app:test"]),
        &[],
    );
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.stdout.contains("CartTest") || r.xml_cases("app", "shop.app.CartTest").len() >= 2,
        "{}",
        second.stdout
    );
    assert_eq!(
        r.newest_result()["units"]["reused"],
        json!(0),
        "{}",
        second.stderr
    );
}

// covers: C-32, C-46
#[test]
fn a_junit4_test_jvm_that_died_beside_a_genuine_failure_records_no_pass_for_its_class() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    // JUnit 4 on Gradle's own runner, which loads no hook: one class fails for real, and the JVM
    // dies in another's class teardown once its tests passed. Nine green classes in `other`.
    let mut files = build(Framework::Junit4, &["app", "other"], "reuse");
    files.extend([
        // A JVM a class, so that the failure is reported before the other JVM dies.
        file("app/build.gradle", "tasks.named('test') { forkEvery = 1 }\n"),
        file(
            "app/src/test/java/shop/app/AFailTest.java",
            "package shop.app;\nimport org.junit.*;\npublic class AFailTest { @Test public void fails() { Assert.fail(\"for real\"); } }\n",
        ),
        file(
            "app/src/test/java/shop/app/CrashTest.java",
            "package shop.app;\nimport org.junit.*;\npublic class CrashTest {\n  @Test public void a() {}\n  @Test public void b() {}\n  @AfterClass public static void after() { if (System.getenv(\"CRASH\") != null) Runtime.getRuntime().halt(3); }\n}\n",
        ),
    ]);
    for n in 1..=9 {
        files.push(file(
            &format!("other/src/test/java/shop/other/Green{n}Test.java"),
            &format!("package shop.other;\nimport org.junit.*;\npublic class Green{n}Test {{ @Test public void green() {{}} }}\n"),
        ));
    }
    let r = Repo::new(&files);
    let ran = r.run(&command(&gradle, &[]), &[("CRASH", "1")]);
    assert_eq!(ran.code, 1, "{}\n{}", ran.stdout, ran.stderr);
    // AFailTest failed; other's passed; CrashTest recorded nothing.
    assert_eq!(
        r.unit_results(),
        std::collections::BTreeMap::from([("fail".into(), 1), ("pass".into(), 9)]),
        "{}\n{}",
        ran.stdout,
        ran.stderr
    );
}

// covers: C-32, C-46
#[test]
fn a_test_that_ends_its_jvm_records_no_pass_under_gradles_own_runners() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    // JUnit 4 and TestNG on Gradle's own runners, which load no listener of the hook: `b` ends
    // the test JVM with `System.exit(0)` when `EXIT` is set, and `c`, which always fails, never
    // runs. Gradle calls the task successful. A JVM a class, so that the green ones run.
    let r = Repo::new(&[
        file("hushrun.yaml", &yaml("reuse")),
        file(".gitignore", "build/\n.gradle/\n"),
        file(
            "settings.gradle",
            &format!("rootProject.name = 'exit'\ninclude 'j4', 'ng'\n{LINE}\n"),
        ),
        file(
            "build.gradle",
            "subprojects { apply plugin: 'java'; repositories { mavenCentral() }; tasks.withType(Test).configureEach { forkEvery = 1 } }\nproject(':j4') { dependencies { testImplementation 'junit:junit:4.13.2' }; test { useJUnit() } }\nproject(':ng') { dependencies { testImplementation 'org.testng:testng:7.10.2' }; test { useTestNG() } }\n",
        ),
        file(
            "j4/src/test/java/p/ExitTest.java",
            "package p;\nimport org.junit.*;\nimport org.junit.runners.MethodSorters;\n@FixMethodOrder(MethodSorters.NAME_ASCENDING)\npublic class ExitTest {\n  @Test public void a() {}\n  @Test public void b() { if (System.getenv(\"EXIT\") != null) System.exit(0); }\n  @Test public void c() { Assert.fail(\"c\"); }\n}\n",
        ),
        file(
            "j4/src/test/java/p/GreenTest.java",
            "package p;\nimport org.junit.*;\npublic class GreenTest { @Test public void g() {} }\n",
        ),
        file(
            "ng/src/test/java/q/ExitTest.java",
            "package q;\nimport org.testng.annotations.*;\nimport static org.testng.Assert.*;\npublic class ExitTest {\n  @Test public void a() {}\n  @Test(dependsOnMethods = \"a\") public void b() { if (System.getenv(\"EXIT\") != null) System.exit(0); }\n  @Test(dependsOnMethods = \"b\") public void c() { fail(\"c\"); }\n}\n",
        ),
        file(
            "ng/src/test/java/q/GreenTest.java",
            "package q;\nimport org.testng.annotations.*;\npublic class GreenTest { @Test public void g() {} }\n",
        ),
    ]);
    let ran = r.run(&command(&gradle, &[]), &[("EXIT", "1")]);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    // The green classes passed; neither ExitTest did.
    assert_eq!(
        r.unit_results(),
        std::collections::BTreeMap::from([("pass".into(), 2)]),
        "{}\n{}",
        ran.stdout,
        ran.stderr
    );
}

// covers: C-30, C-46
#[test]
fn a_jar_from_the_local_maven_repository_is_read_by_what_built_it_or_not_followed() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let local = tempfile::tempdir().unwrap();
    let m2 = std::fs::canonicalize(local.path()).unwrap();
    // A jar of the local repository nothing here builds: an empty zip and its POM.
    let thing = m2.join("com/other/thing/1.0");
    std::fs::create_dir_all(&thing).unwrap();
    let mut empty = b"PK\x05\x06".to_vec();
    empty.extend([0u8; 18]);
    std::fs::write(thing.join("thing-1.0.jar"), &empty).unwrap();
    std::fs::write(
        thing.join("thing-1.0.pom"),
        "<project><modelVersion>4.0.0</modelVersion><groupId>com.other</groupId><artifactId>thing</artifactId><version>1.0</version></project>\n",
    )
    .unwrap();
    // lib publishes to the local repository, and app takes it from there, not as a project.
    let mut files = build(Framework::Junit5, &["lib", "app"], "reuse");
    files[3] = file(
        "build.gradle",
        "subprojects {\n  apply plugin: 'java'\n  repositories { mavenLocal(); mavenCentral() }\n  dependencies {\n    testImplementation platform('org.junit:junit-bom:5.11.4')\n    testImplementation 'org.junit.jupiter:junit-jupiter'\n    testRuntimeOnly 'org.junit.platform:junit-platform-launcher'\n  }\n  tasks.withType(Test).configureEach { useJUnitPlatform() }\n}\nproject(':lib') { apply plugin: 'maven-publish'; group = 'com.example'; version = '1.0'\n  publishing { publications { maven(MavenPublication) { from components.java } } } }\nproject(':app') {\n  dependencies { implementation 'com.example:lib:1.0'; testImplementation(System.getenv('THING') ? 'com.other:thing:1.0' : 'com.example:lib:1.0') }\n  tasks.named('compileJava') { mustRunAfter(':lib:publishToMavenLocal') }\n  tasks.named('test') { mustRunAfter(':lib:publishToMavenLocal') }\n}\n",
    );
    files.extend([
        file(
            "lib/src/main/java/lib/Lib.java",
            "package lib;\npublic class Lib { public static int v() { return 1; } }\n",
        ),
        file(
            "lib/src/test/java/lib/LibTest.java",
            "package lib;\nimport org.junit.jupiter.api.*;\nclass LibTest { @Test void v() { Assertions.assertTrue(Lib.v() > 0); } }\n",
        ),
        file(
            "app/src/test/java/app/AppTest.java",
            "package app;\nimport org.junit.jupiter.api.*;\nclass AppTest { @Test void v() { Assertions.assertEquals(1, lib.Lib.v()); } }\n",
        ),
    ]);
    for n in 1..=9 {
        files.push(file(
            &format!("app/src/test/java/app/Green{n}Test.java"),
            &format!("package app;\nimport org.junit.jupiter.api.*;\nclass Green{n}Test {{ @Test void green() {{}} }}\n"),
        ));
    }
    let r = Repo::new(&files);
    let repo_local = format!("-Dmaven.repo.local={}", m2.display());
    let args = |extra: &[&'static str]| -> Vec<String> {
        let mut out: Vec<String> = [
            "run",
            "unit",
            "--",
            gradle.as_str(),
            "--offline",
            "--console=plain",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        out.push(repo_local.clone());
        out.extend(extra.iter().map(|s| s.to_string()));
        out
    };
    let run = |extra: &[&'static str], env: &[(&str, &str)]| {
        let a = args(extra);
        r.run(&a.iter().map(String::as_str).collect::<Vec<_>>(), env)
    };
    let tasks = [":lib:publishToMavenLocal", "test", "--continue"];
    let first = run(&tasks, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // app's classes read lib's sources: lib built the jar they took from the local repository.
    let map = r.map();
    assert!(
        map["units"]["app/src/test/java/app/AppTest.java"]["reads"]
            .as_array()
            .unwrap()
            .contains(&json!("lib/src/main/java/lib/Lib.java")),
        "{map:#}"
    );
    r.write(
        "lib/src/main/java/lib/Lib.java",
        "package lib;\npublic class Lib { public static int v() { return 2; } }\n",
    );
    let second = run(&tasks, &[]);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.stdout.contains("AppTest > v() FAILED"),
        "{}",
        second.stdout
    );
    // A jar of the local repository no project of the build makes: the class that ran (AppTest,
    // which failed and runs again) isn't followed, and says why.
    let third = run(&tasks, &[("THING", "1")]);
    assert_eq!(third.code, 1, "{}\n{}", third.stdout, third.stderr);
    let map = r.map();
    assert_eq!(
        map["units"]["app/src/test/java/app/AppTest.java"]["notFollowed"],
        json!(["local-maven-repository"]),
        "{map:#}"
    );
}
