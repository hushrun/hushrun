//! \[ADR 0085\] Test classes with Maven (C-47): a unit is a test class of a reactor's module, what
//! it read is every source below the roots of its module and of the modules it depends on, as
//! Maven's own model says, and reuse never misses a failure.
//!
//! The fixtures are generated reactors that need Maven 3.9 or later (`HUSHRUN_TEST_MAVEN`, the
//! path of its `mvn`), a JDK, and a local repository that already holds every plugin and library
//! they use (`HUSHRUN_TEST_MAVEN_REPO`): Maven runs offline, reading that repository as a tail
//! below one of its own for each test, which it never writes to. With
//! `HUSHRUN_TEST_MAVEN_ONLINE=1` it runs online and fills that repository instead, which is how
//! CI fills it once before its test run. The tests say so and pass where Maven is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/store.rs"]
mod store;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;
use serde_json::Value;

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
    "HUSHRUN_TESTS_LEAVE_OUT",
    "HUSHRUN_TESTS_EVENTS",
    "MAVEN_OPTS",
    "MAVEN_ARGS",
    "MAVEN_CONFIG",
    "JAVA_TOOL_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "_JAVA_OPTIONS",
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
/// What makes a fixture's `failsWhenAsked` tests fail: not a file, so not part of any key.
const FAIL: &str = "SHOP_FAIL";
/// What makes `other`'s `failsWhenOtherAsked` fail.
const OTHER: &str = "SHOP_OTHER";

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

/// Maven and its repository, when the tests have them.
fn maven() -> Option<(String, PathBuf)> {
    let mvn = std::env::var("HUSHRUN_TEST_MAVEN")
        .ok()
        .filter(|m| !m.is_empty());
    let repo = std::env::var("HUSHRUN_TEST_MAVEN_REPO")
        .ok()
        .filter(|r| !r.is_empty());
    let found = mvn.zip(repo).filter(|(mvn, _)| {
        Command::new(mvn)
            .arg("--version")
            .env_remove("MAVEN_OPTS")
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if !tools(
        "Maven (HUSHRUN_TEST_MAVEN and HUSHRUN_TEST_MAVEN_REPO)",
        found.is_some(),
    ) {
        return None;
    }
    found.map(|(mvn, repo)| (mvn, PathBuf::from(repo)))
}

fn online() -> bool {
    std::env::var("HUSHRUN_TEST_MAVEN_ONLINE").as_deref() == Ok("1")
}

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    mvn: String,
    m2: PathBuf,
}

impl Repo {
    fn new(files: &[(String, String)], (mvn, m2): (String, PathBuf)) -> Repo {
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
            mvn,
            m2,
        };
        r.write(".gitignore", "target/\n");
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

    /// COMMAND: Maven offline, its own local repository with the shared one below it.
    fn mvn(&self, args: &[&str]) -> Vec<String> {
        let mut out = vec![self.mvn.clone(), "-B".to_string()];
        if online() {
            out.push(format!("-Dmaven.repo.local={}", self.m2.display()));
        } else {
            out.push("-o".to_string());
            out.push(format!(
                "-Dmaven.repo.local={}",
                self.root.join("m2").display()
            ));
            out.push(format!("-Dmaven.repo.local.tail={}", self.m2.display()));
        }
        out.extend(args.iter().map(|a| a.to_string()));
        out
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
            )
            .env_remove(FAIL)
            .env_remove(OTHER)
            .env_remove("SHOP_CRASH");
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

    /// `hushrun run [flags] unit -- mvn … <args>`.
    fn run(&self, flags: &[&str], args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = self.hushrun();
        for (name, value) in env {
            cmd.env(name, value);
        }
        cmd.arg("run").args(flags).args(["unit", "--"]);
        cmd.args(self.mvn(args));
        let output = cmd.output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Maven alone, outside hushrun.
    fn plain(&self, args: &[&str]) -> std::process::Output {
        let words = self.mvn(args);
        let mut cmd = Command::new(&words[0]);
        cmd.args(&words[1..])
            .current_dir(&self.repo)
            .env("HOME", self.root.join("home"));
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.output().expect("mvn")
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
    fn unit_results(&self) -> BTreeMap<String, usize> {
        let mut files = Vec::new();
        collect(&self.cache.join("results/units/v0"), &mut files);
        let mut out = BTreeMap::new();
        for f in files {
            let v: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
            let result = v["result"].as_str().unwrap_or("?").to_string();
            *out.entry(result).or_default() += 1;
        }
        out
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
    fn line(&self, start: &str) -> &str {
        self.stderr
            .lines()
            .find(|l| l.starts_with(start))
            .unwrap_or_else(|| panic!("no line {start:?} in:\n{}\n{}", self.stderr, self.stdout))
    }

    /// How many tests Surefire ran of each top-level class, from its own lines.
    fn ran(&self) -> BTreeMap<String, u64> {
        let mut out = BTreeMap::new();
        for line in self.stdout.lines() {
            let Some((counts, class)) = line.split_once(" -- in ") else {
                continue;
            };
            let Some(n) = counts
                .split("Tests run: ")
                .nth(1)
                .and_then(|r| r.split(',').next())
                .and_then(|n| n.trim().parse::<u64>().ok())
            else {
                continue;
            };
            let top = class.trim().split('$').next().unwrap().to_string();
            *out.entry(top).or_default() += n;
        }
        out
    }
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// The reactor's parent POM: JUnit `junit` (`5` or `4`) and Surefire `surefire` for every module.
fn parent(modules: &[&str], junit: &str, surefire: &str) -> String {
    let dependency = if junit == "4" {
        "<dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.13.2</version><scope>test</scope></dependency>"
    } else {
        "<dependency><groupId>org.junit.jupiter</groupId><artifactId>junit-jupiter</artifactId><version>5.11.4</version><scope>test</scope></dependency>"
    };
    let modules: String = modules
        .iter()
        .map(|m| format!("<module>{m}</module>"))
        .collect();
    format!(
        r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>shop</groupId><artifactId>parent</artifactId><version>1.0</version><packaging>pom</packaging>
  <modules>{modules}</modules>
  <properties>
    <maven.compiler.release>11</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  <dependencies>{dependency}</dependencies>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-surefire-plugin</artifactId><version>{surefire}</version></plugin></plugins></build>
</project>
"#
    )
}

fn module_pom(name: &str, depends: &[&str]) -> String {
    let deps: String = depends
        .iter()
        .map(|d| format!("<dependency><groupId>shop</groupId><artifactId>{d}</artifactId><version>1.0</version></dependency>"))
        .collect();
    format!(
        r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>shop</groupId><artifactId>parent</artifactId><version>1.0</version></parent>
  <artifactId>{name}</artifactId>
  <dependencies>{deps}</dependencies>
</project>
"#
    )
}

/// A shop with JUnit 5 on Surefire 3.5.4: `lib` (prices, with a parametrized test, a nested class
/// and a test that fails when asked), `app` (a cart, which depends on lib), `other` (unrelated).
fn shop(tests: &str) -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file("pom.xml", &parent(&["lib", "app", "other"], "5", "3.5.4")),
        file("lib/pom.xml", &module_pom("lib", &[])),
        file(
            "lib/src/main/java/shop/lib/Price.java",
            "package shop.lib;\n\npublic class Price {\n  public static int of(int x) { return x * 2; }\n}\n",
        ),
        file(
            "lib/src/test/java/shop/lib/PriceTest.java",
            &format!(
                r#"package shop.lib;

import org.junit.jupiter.api.*;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;
import static org.junit.jupiter.api.Assertions.*;

class PriceTest {{
  @Test void passes() {{ assertEquals(4, Price.of(2)); }}
  @Test void failsWhenAsked() {{ assertNull(System.getenv("{FAIL}"), "asked to fail"); }}
  @ParameterizedTest @ValueSource(ints = {{1, 2, 3}}) void param(int x) {{ assertTrue(Price.of(x) > x); }}
  @Nested class Inner {{
    @Test void innerPasses() {{ assertEquals(0, Price.of(0)); }}
  }}
}}
"#
            ),
        ),
        file(
            "lib/src/test/java/shop/lib/RoundTest.java",
            "package shop.lib;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass RoundTest {\n  @Test void rounds() { assertEquals(2, Price.of(1)); }\n}\n",
        ),
        file("app/pom.xml", &module_pom("app", &["lib"])),
        file(
            "app/src/main/java/shop/app/Cart.java",
            "package shop.app;\n\nimport shop.lib.Price;\n\npublic class Cart {\n  public static int total(int... xs) { int n = 0; for (int x : xs) n += Price.of(x); return n; }\n}\n",
        ),
        file(
            "app/src/test/java/shop/app/CartTest.java",
            "package shop.app;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass CartTest {\n  @Test void totals() { assertEquals(6, Cart.total(1, 2)); }\n}\n",
        ),
        file("other/pom.xml", &module_pom("other", &[])),
        file(
            "other/src/main/java/shop/other/Greet.java",
            "package shop.other;\n\npublic class Greet {\n  public static String hello() { return \"hi\"; }\n}\n",
        ),
        file(
            "other/src/test/java/shop/other/GreetTest.java",
            &format!(
                "package shop.other;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass GreetTest {{\n  @Test void greets() {{ assertEquals(\"hi\", Greet.hello()); }}\n  @Test void failsWhenOtherAsked() {{ assertNull(System.getenv(\"{OTHER}\")); }}\n}}\n"
            ),
        ),
        file(
            "other/src/test/java/shop/other/WaveTest.java",
            "package shop.other;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass WaveTest {\n  @Test void waves() { assertEquals(2, Greet.hello().length()); }\n}\n",
        ),
        file(
            "other/src/test/java/shop/other/ByeTest.java",
            "package shop.other;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass ByeTest {\n  @Test void byes() { assertNotNull(Greet.hello()); }\n}\n",
        ),
    ]
}

const PRICE_TEST: &str = "lib/src/test/java/shop/lib/PriceTest.java";
const ROUND_TEST: &str = "lib/src/test/java/shop/lib/RoundTest.java";
const CART_TEST: &str = "app/src/test/java/shop/app/CartTest.java";
const GREET_TEST: &str = "other/src/test/java/shop/other/GreetTest.java";

/// The classes a run ran that it shouldn't have, but one: the audit runs one class of those a
/// plan could reuse (C-34).
fn at_most_the_audit(ran: &BTreeMap<String, u64>, must: &[&str]) {
    for class in must {
        assert!(ran.contains_key(*class), "{class} didn't run: {ran:?}");
    }
    let others = ran.keys().filter(|c| !must.contains(&c.as_str())).count();
    assert!(others <= 1, "more than the audited one ran: {ran:?}");
}

// covers: C-30, C-31, C-47
#[test]
fn maven_test_classes_read_their_modules_and_the_modules_they_depend_on() {
    let Some(maven) = maven() else { return };
    let r = Repo::new(&shop("observe"), maven);
    let first = r.run(&[], &["test"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stderr, first.stdout);
    assert_eq!(
        first.line("hushrun: unit: with `tests: reuse`"),
        "hushrun: unit: with `tests: reuse`, all 6 test files would have run: none had passed here yet on the files it read"
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
    // A class reads every source below its module's roots and the main roots of the modules of
    // the reactor its module depends on; never an unrelated module's.
    assert_eq!(
        reads(CART_TEST),
        [
            CART_TEST,
            "app/src/main/java/shop/app/Cart.java",
            "lib/src/main/java/shop/lib/Price.java"
        ]
        .map(String::from)
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
    assert_eq!(
        reads(GREET_TEST),
        [
            "other/src/main/java/shop/other/Greet.java",
            "other/src/test/java/shop/other/ByeTest.java",
            GREET_TEST,
            "other/src/test/java/shop/other/WaveTest.java"
        ]
    );
    assert!(reads(PRICE_TEST).contains(&ROUND_TEST.to_string()));
    let dirs = &map["units"][CART_TEST]["dirs"];
    assert!(
        dirs.as_array()
            .unwrap()
            .iter()
            .any(|d| d == "lib/src/main/java"),
        "{dirs}"
    );
    // POMs are the rest: a change to one runs everything.
    assert!(map["rest"]["pom.xml"].is_string(), "{map:#}");
    assert!(map["rest"]["app/pom.xml"].is_string(), "{map:#}");
    let sources: Vec<&str> = map["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        sources.contains(&"lib/src/main/java/shop/lib/Price.java"),
        "{sources:?}"
    );
    assert_eq!(
        r.newest_result()["units"],
        serde_json::json!({"total": 6, "ran": 6, "reused": 0})
    );
}

// covers: C-34, C-47
#[test]
fn a_change_runs_the_classes_of_its_module_and_of_the_modules_that_depend_on_it_only() {
    let Some(maven) = maven() else { return };
    let r = Repo::new(&shop("reuse"), maven);
    let first = r.run(&[], &["test"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stderr, first.stdout);
    assert_eq!(first.ran().len(), 6, "{:?}", first.ran());

    // A change in `other` runs its classes, never lib's or app's, but the one of those three
    // the audit runs.
    r.write(
        "other/src/main/java/shop/other/Greet.java",
        "package shop.other;\n\npublic class Greet {\n  public static String hello() { return \"h\" + \"i\"; }\n}\n",
    );
    let other = r.run(&[], &["test"], &[]);
    assert_eq!(other.code, 0, "{}\n{}", other.stderr, other.stdout);
    assert_eq!(
        other.line("hushrun: unit: "),
        "hushrun: unit: 4 of 6 test files run; 2 already passed on the files they read, and 1 more that did runs as a check"
    );
    at_most_the_audit(
        &other.ran(),
        &[
            "shop.other.ByeTest",
            "shop.other.GreetTest",
            "shop.other.WaveTest",
        ],
    );
    let units = r.newest_result()["units"].clone();
    assert_eq!(
        units,
        serde_json::json!({"total": 6, "ran": 4, "reused": 2, "savedMs": units["savedMs"]})
    );

    // A change in `lib` runs lib's classes and app's, which depends on it, never other's.
    r.write(
        "lib/src/main/java/shop/lib/Price.java",
        "package shop.lib;\n\npublic class Price {\n  public static int of(int x) { return x + x; }\n}\n",
    );
    let lib = r.run(&[], &["test"], &[]);
    assert_eq!(lib.code, 0, "{}\n{}", lib.stderr, lib.stdout);
    at_most_the_audit(
        &lib.ran(),
        &[
            "shop.lib.PriceTest",
            "shop.lib.RoundTest",
            "shop.app.CartTest",
        ],
    );
    // A POM is the rest: everything runs.
    r.write(
        "app/pom.xml",
        &module_pom("app", &["lib"]).replace("</project>", "<!-- x -->\n</project>"),
    );
    let pom = r.run(&[], &["test"], &[]);
    assert_eq!(pom.code, 0, "{}\n{}", pom.stderr, pom.stdout);
    assert_eq!(pom.ran().len(), 6, "{:?}", pom.ran());
}

// covers: C-39, C-47
#[test]
fn a_failed_test_runs_alone_and_the_other_tests_of_its_class_are_reused_one_by_one() {
    let Some(maven) = maven() else { return };
    let r = Repo::new(&shop("{mode: reuse, unit: test}"), maven);
    // At the end: lib's failure doesn't stop other's classes, which don't depend on it; app's,
    // which do, don't run.
    let first = r.run(&[], &["--fail-at-end", "test"], &[(FAIL, "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stderr, first.stdout);
    let map = r.map();
    // A test is its method, its nested class's path before it; a parametrized method is one
    // test, whatever its invocations.
    assert_eq!(
        map["units"][PRICE_TEST]["tests"],
        serde_json::json!({
            "Inner#innerPasses": "passed",
            "failsWhenAsked": "failed",
            "param": "passed",
            "passes": "passed"
        }),
        "{map:#}"
    );

    let second = r.run(&[], &["--fail-at-end", "test"], &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stderr, second.stdout);
    let line = second.line("hushrun: unit: ");
    assert!(line.contains(" tests run, in "), "{line}");
    // The failed test ran, and the class's others were reused one by one, but one the audit may
    // have run: PriceTest ran 1 or 2 of its 6 invocations, never all.
    let ran = second.ran();
    let price = ran.get("shop.lib.PriceTest").copied().unwrap_or(0);
    assert!((1..=4).contains(&price), "{ran:?}\n{}", second.stdout);
    let map = r.map();
    assert_eq!(
        map["units"][PRICE_TEST]["tests"]["failsWhenAsked"], "passed",
        "{map:#}"
    );
    let tests = &r.newest_result()["units"]["tests"];
    assert!(
        tests["reused"].as_u64().unwrap() >= 4,
        "{tests}\n{}\n{}",
        second.stderr,
        second.stdout
    );
}

// covers: C-34, C-47
#[test]
fn an_audited_class_that_fails_again_alone_suspends_the_checks_reuse() {
    let Some(maven) = maven() else { return };
    let mut files = shop("reuse");
    // Every class fails when asked, as a missed dependency would: whichever the audit runs fails.
    for (path, text) in files.iter_mut() {
        if path.ends_with("Test.java") && !path.ends_with("PriceTest.java") {
            *text = text.replacen(
                "  @Test void ",
                &format!("  @Test void failsWhenAsked() {{ assertNull(System.getenv(\"{FAIL}\")); }}\n  @Test void "),
                1,
            );
        }
    }
    let r = Repo::new(&files, maven);
    // other's GreetTest fails, so the check didn't pass; the five others did.
    let first = r.run(&[], &["test"], &[(OTHER, "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stderr, first.stdout);
    let caught = r.run(&[], &["test"], &[(FAIL, "1")]);
    assert_ne!(caught.code, 0, "{}\n{}", caught.stderr, caught.stdout);
    assert!(
        caught.stderr.contains(
            "failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
    assert_eq!(store::disagreements(&r.cache).len(), 1);
    let after = r.run(&[], &["test"], &[]);
    assert!(
        after
            .stderr
            .contains("hushrun: unit: every test file runs: reuse of its test files is suspended"),
        "{}",
        after.stderr
    );
}

/// One module with JUnit 4 on Surefire 3.5.4, which runs it with its JUnit 4 provider: the
/// test-JVM hook doesn't load there.
fn junit4(tests: &str) -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file("pom.xml", &parent(&["core"], "4", "3.5.4")),
        file("core/pom.xml", &module_pom("core", &[])),
        file(
            "core/src/main/java/shop/core/Calc.java",
            "package shop.core;\n\npublic class Calc {\n  public static int add(int a, int b) { return a + b; }\n}\n",
        ),
        file(
            "core/src/test/java/shop/core/CalcTest.java",
            "package shop.core;\n\nimport org.junit.Test;\nimport static org.junit.Assert.*;\n\npublic class CalcTest {\n  @Test public void adds() { assertEquals(3, Calc.add(1, 2)); }\n}\n",
        ),
        file(
            "core/src/test/java/shop/core/AskTest.java",
            &format!(
                "package shop.core;\n\nimport org.junit.Test;\nimport static org.junit.Assert.*;\n\npublic class AskTest {{\n  @Test public void failsWhenAsked() {{ assertNull(System.getenv(\"{FAIL}\")); }}\n}}\n"
            ),
        ),
        file(
            "core/src/test/java/shop/core/CrashTest.java",
            "package shop.core;\n\nimport org.junit.Test;\n\npublic class CrashTest {\n  @Test public void crashesWhenAsked() { if (System.getenv(\"SHOP_CRASH\") != null) Runtime.getRuntime().halt(3); }\n}\n",
        ),
    ]
}

// covers: C-47
#[test]
fn junit4_where_the_hook_cant_load_runs_everything_and_records_what_ran() {
    let Some(maven) = maven() else { return };
    let r = Repo::new(&junit4("reuse"), maven);
    let first = r.run(&[], &["test"], &[(FAIL, "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stderr, first.stdout);
    assert_eq!(
        r.unit_results(),
        BTreeMap::from([("fail".into(), 1), ("pass".into(), 2)])
    );
    // The plan lists the classes that passed, and the hook, which can't load in JUnit 4's
    // provider, leaves nothing out: every class runs, and says so.
    let second = r.run(&[], &["test"], &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stderr, second.stdout);
    assert_eq!(second.ran().len(), 3, "{:?}", second.ran());
    assert!(
        second.stderr.contains(
            "hushrun: unit: no test file was reused: the test-JVM hook loaded in no test JVM"
        ),
        "{}",
        second.stderr
    );
    assert_eq!(
        r.newest_result()["units"],
        serde_json::json!({"total": 3, "ran": 3, "reused": 0})
    );
}

// covers: C-47
#[test]
fn a_fork_that_died_and_its_stale_report_never_read_as_a_pass() {
    let Some(maven) = maven() else { return };
    let r = Repo::new(&junit4("observe"), maven);
    // AskTest fails, so the check didn't pass; CrashTest passed, and wrote its report.
    let first = r.run(&[], &["test"], &[(FAIL, "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stderr, first.stdout);
    assert_eq!(r.newest_result()["units"]["total"], 3);
    let report = r
        .repo
        .join("core/target/surefire-reports/TEST-shop.core.CrashTest.xml");
    assert!(report.is_file());
    // CrashTest's fork dies: its report from the first run is still there, and is never this
    // run's, so it is never a pass.
    let crashed = r.run(&[], &["test"], &[("SHOP_CRASH", "1")]);
    assert_ne!(crashed.code, 0, "{}\n{}", crashed.stderr, crashed.stdout);
    assert!(
        crashed.stdout.contains("The forked VM terminated"),
        "{}",
        crashed.stdout
    );
    assert!(report.is_file(), "the stale report is still there");
    let units = &r.newest_result()["units"];
    assert!(units["total"].as_u64().unwrap() <= 2, "{units}");
}

// covers: C-47
#[test]
fn a_module_built_without_the_modules_it_depends_on_isnt_followed() {
    let Some(maven) = maven() else { return };
    let r = Repo::new(&shop("observe"), maven);
    if online() {
        // Filling the repository: the plugins that install lib.
        let _ = r.plain(&["-pl", "lib", "-am", "install", "-DskipTests"]);
    }
    let installed = r.plain(&["-pl", "lib", "-am", "install", "-DskipTests"]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stdout)
    );
    let app = r.run(&[], &["-pl", "app", "test"], &[]);
    assert_eq!(app.code, 0, "{}\n{}", app.stderr, app.stdout);
    assert!(
        app.line("hushrun: unit: with `tests: reuse`")
            .contains("hushrun couldn't follow all that 1 of them read"),
        "{}",
        app.stderr
    );
    let map = r.map();
    assert_eq!(
        map["units"][CART_TEST]["notFollowed"],
        serde_json::json!(["upstream-not-built"]),
        "{map:#}"
    );
}

/// One module with TestNG 7.10.2 on Surefire 3.5.4, which runs it with its TestNG provider: the
/// hook's interceptor loads there, its listener doesn't. Nine green classes, and `extra`.
fn testng(tests: &str, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(
            "pom.xml",
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>shop</groupId><artifactId>ng</artifactId><version>1.0</version>
  <properties>
    <maven.compiler.release>11</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  <dependencies><dependency><groupId>org.testng</groupId><artifactId>testng</artifactId><version>7.10.2</version><scope>test</scope></dependency></dependencies>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-surefire-plugin</artifactId><version>3.5.4</version></plugin></plugins></build>
</project>
"#,
        ),
    ];
    for n in 1..=9 {
        files.push(file(
            &format!("src/test/java/ng/Green{n}Test.java"),
            &format!("package ng;\nimport org.testng.annotations.*;\npublic class Green{n}Test {{\n  @Test public void green() {{}}\n}}\n"),
        ));
    }
    files.extend(extra.iter().map(|(path, text)| file(path, text)));
    files
}

// covers: C-32, C-47
#[test]
fn tests_testng_skipped_after_a_failed_suite_setup_never_pass() {
    let Some(maven) = maven() else { return };
    // `SetupTest`'s `@BeforeSuite` fails without a database (`SHOP_NO_DB`), which skips every
    // test of the suite; `OrderTest.total` fails whenever it runs.
    let files = testng(
        "reuse",
        &[
            (
                "src/test/java/ng/SetupTest.java",
                "package ng;\nimport org.testng.annotations.*;\npublic class SetupTest {\n  @BeforeSuite public void db() { if (System.getenv(\"SHOP_NO_DB\") != null) throw new IllegalStateException(\"no database\"); }\n  @Test public void setupWorks() {}\n}\n",
            ),
            (
                "src/test/java/ng/OrderTest.java",
                "package ng;\nimport org.testng.annotations.*;\nimport static org.testng.Assert.*;\npublic class OrderTest {\n  @Test public void total() { assertEquals(1, 2); }\n}\n",
            ),
        ],
    );
    let r = Repo::new(&files, maven);
    // The suite's setup fails: TestNG skips every test, OrderTest's failing one among them.
    let first = r.run(&[], &["test"], &[("SHOP_NO_DB", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stderr, first.stdout);
    // Only the setup's failure is recorded: no skipped class passed.
    assert_eq!(
        r.unit_results(),
        BTreeMap::from([("fail".into(), 1)]),
        "{}\n{}",
        first.stderr,
        first.stdout
    );
    // The database is back: OrderTest runs, and fails, as plain Maven says.
    let second = r.run(&[], &["test"], &[]);
    assert_eq!(second.code, 1, "{}\n{}", second.stderr, second.stdout);
    assert!(
        second.stdout.contains("ng.OrderTest.total"),
        "{}",
        second.stdout
    );
}

// covers: C-39, C-47
#[test]
fn a_reused_testng_base_class_leaves_out_none_of_the_tests_its_subclasses_inherit() {
    let Some(maven) = maven() else { return };
    // ATest inherits Base's test, which fails in ATest: a test of each class, as TestNG runs it.
    let files = testng(
        "reuse",
        &[
            (
                "src/test/java/ng/Base.java",
                "package ng;\nimport org.testng.annotations.*;\nimport static org.testng.Assert.*;\npublic class Base {\n  protected int value() { return 1; }\n  @Test public void inherited() { assertEquals(value(), 1); }\n}\n",
            ),
            (
                "src/test/java/ng/ATest.java",
                "package ng;\nimport org.testng.annotations.*;\npublic class ATest extends Base {\n  @Override protected int value() { return 2; }\n  @Test public void own() {}\n}\n",
            ),
        ],
    );
    let r = Repo::new(&files, maven);
    let first = r.run(&[], &["test", "-Dtest=Base,ATest,Green*Test"], &[]);
    assert_eq!(first.code, 1, "{}\n{}", first.stderr, first.stdout);
    assert!(
        first.stdout.contains("ng.ATest.inherited"),
        "{}",
        first.stdout
    );
    // Nothing changed: Base is reused, and ATest runs whole, its inherited test failing again.
    let second = r.run(&[], &["test", "-Dtest=Base,ATest,Green*Test"], &[]);
    assert_eq!(second.code, 1, "{}\n{}", second.stderr, second.stdout);
    assert!(
        second.stdout.contains("ng.ATest.inherited"),
        "{}",
        second.stdout
    );
}

/// A processor's module, `proc`, and `app`, whose tests read only what `proc`'s processor
/// generates: `app` names `proc` in its compiler's `annotationProcessorPaths`, never as a
/// dependency. `VALUE` is what the generated `GenValue.value()` returns.
fn processor(value: u32) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(
            "pom.xml",
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>shop</groupId><artifactId>parent</artifactId><version>1.0</version><packaging>pom</packaging>
  <modules><module>proc</module><module>app</module></modules>
  <properties><maven.compiler.release>17</maven.compiler.release><project.build.sourceEncoding>UTF-8</project.build.sourceEncoding></properties>
  <dependencies><dependency><groupId>org.junit.jupiter</groupId><artifactId>junit-jupiter</artifactId><version>5.11.4</version><scope>test</scope></dependency></dependencies>
  <build><plugins>
    <plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-surefire-plugin</artifactId><version>3.5.4</version></plugin>
    <plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-compiler-plugin</artifactId><version>3.15.0</version></plugin>
  </plugins></build>
</project>
"#,
        ),
        file(
            "proc/pom.xml",
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>shop</groupId><artifactId>parent</artifactId><version>1.0</version></parent>
  <artifactId>proc</artifactId>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-compiler-plugin</artifactId><configuration><proc>none</proc></configuration></plugin></plugins></build>
</project>
"#,
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
            "package proc;\nimport org.junit.jupiter.api.Test;\nclass ProcTest { @Test void loads() { new GenProcessor(); } }\n",
        ),
        file(
            "app/pom.xml",
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>shop</groupId><artifactId>parent</artifactId><version>1.0</version></parent>
  <artifactId>app</artifactId>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-compiler-plugin</artifactId>
    <configuration><annotationProcessorPaths><path><groupId>shop</groupId><artifactId>proc</artifactId><version>1.0</version></path></annotationProcessorPaths></configuration>
  </plugin></plugins></build>
</project>
"#,
        ),
        file(
            "app/src/main/java/app/App.java",
            "package app;\npublic class App {}\n",
        ),
        file(
            "app/src/test/java/app/AppTest.java",
            "package app;\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\nclass AppTest { @Test void generated() { assertEquals(1, GenValue.value()); } }\n",
        ),
    ];
    files.push(values(value));
    for n in 1..=9 {
        files.push(file(
            &format!("app/src/test/java/app/Green{n}Test.java"),
            &format!("package app;\nimport org.junit.jupiter.api.Test;\nclass Green{n}Test {{ @Test void green() {{}} }}\n"),
        ));
    }
    files
}

fn values(value: u32) -> (String, String) {
    file(
        "proc/src/main/java/proc/Values.java",
        &format!(
            "package proc;\npublic class Values {{ public static final int VALUE = {value}; }}\n"
        ),
    )
}

// covers: C-47
#[test]
fn a_change_to_an_annotation_processor_of_the_build_runs_the_tests_of_the_code_it_generates() {
    let Some(maven) = maven() else { return };
    let r = Repo::new(&processor(1), maven);
    let first = r.run(&[], &["clean", "test"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stderr, first.stdout);
    // app's classes read proc's sources, which its compiler runs.
    let map = r.map();
    let reads = map["units"]["app/src/test/java/app/AppTest.java"]["reads"].clone();
    assert!(
        reads
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("proc/src/main/java/proc/Values.java")),
        "{map:#}"
    );
    let (path, text) = values(2);
    r.write(&path, &text);
    let second = r.run(&[], &["clean", "test"], &[]);
    assert_eq!(second.code, 1, "{}\n{}", second.stderr, second.stdout);
    assert!(
        second.stdout.contains("app.AppTest.generated"),
        "{}",
        second.stdout
    );
}

// covers: C-32, C-47
#[test]
fn a_fork_that_died_after_its_class_tests_passed_records_no_pass_for_it() {
    let Some(maven) = maven() else { return };
    // JUnit 4, which Surefire runs without the hook, and JUnit 5, which it runs with it: a class
    // whose tests passed and whose JVM dies in its class teardown, run last of its module.
    let crash4 = "package shop.core;\n\nimport org.junit.*;\n\npublic class CrashTest {\n  @Test public void a() {}\n  @Test public void b() {}\n  @AfterClass public static void after() { if (System.getenv(\"SHOP_CRASH\") != null) Runtime.getRuntime().halt(3); }\n}\n";
    let crash5 = "package shop.lib;\n\nimport org.junit.jupiter.api.*;\n\nclass CrashTest {\n  @Test void a() {}\n  @Test void b() {}\n  @AfterAll static void after() { if (System.getenv(\"SHOP_CRASH\") != null) Runtime.getRuntime().halt(3); }\n}\n";
    let mut four = junit4("observe");
    four.retain(|(path, _)| !path.ends_with("CrashTest.java"));
    four.push(file("core/src/test/java/shop/core/CrashTest.java", crash4));
    let mut five = shop("observe");
    five.push(file("lib/src/test/java/shop/lib/CrashTest.java", crash5));
    // JUnit 4: AskTest and CalcTest pass, then CrashTest. JUnit 5: lib's RoundTest and PriceTest
    // pass, then CrashTest; app, which depends on lib, doesn't run; other's three pass.
    for (files, order, passed) in [(four, "alphabetical", 2), (five, "reversealphabetical", 5)] {
        let r = Repo::new(&files, maven.clone());
        let ran = r.run(
            &[],
            &[
                "--fail-at-end",
                "test",
                &format!("-Dsurefire.runOrder={order}"),
            ],
            &[("SHOP_CRASH", "1")],
        );
        assert_ne!(ran.code, 0, "{}\n{}", ran.stderr, ran.stdout);
        assert!(
            ran.stdout.contains("The forked VM terminated"),
            "{}",
            ran.stdout
        );
        assert_eq!(
            r.unit_results(),
            BTreeMap::from([("pass".into(), passed)]),
            "{order}: {}\n{}",
            ran.stderr,
            ran.stdout
        );
    }
}

// covers: C-32, C-47
#[cfg(unix)]
#[test]
fn a_maven_wrapper_that_starts_maven_again_records_neither_start() {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(maven) = maven() else { return };
    let counts = tempfile::tempdir().unwrap();
    // A wrapper that starts Maven again when it fails, and a test that fails the first time.
    let mut files = junit4("{mode: reuse, unit: test}");
    files.push(file(
        "mvnw",
        "#!/bin/sh\n\"$REAL_MVN\" \"$@\" || \"$REAL_MVN\" \"$@\"\n",
    ));
    files.push(file(
        "core/src/test/java/shop/core/FlakyTest.java",
        "package shop.core;\n\nimport org.junit.Test;\nimport java.nio.file.*;\nimport static org.junit.Assert.*;\n\npublic class FlakyTest {\n  @Test public void flaky() throws Exception { Path p = Paths.get(System.getenv(\"COUNTS\"), \"ran\"); boolean again = Files.exists(p); Files.write(p, new byte[0]); assertTrue(again); }\n}\n",
    ));
    let r = Repo::new(&files, maven);
    std::fs::set_permissions(r.repo.join("mvnw"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut words = r.mvn(&["test"]);
    let real = std::mem::replace(&mut words[0], "./mvnw".into());
    let output = r
        .hushrun()
        .env("REAL_MVN", &real)
        .env("COUNTS", counts.path())
        .args(["run", "unit", "--"])
        .args(&words)
        .output()
        .expect("hushrun");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stderr}\n{stdout}");
    assert!(
        stdout.contains("<<< FAILURE!") && stdout.contains("BUILD SUCCESS"),
        "{stdout}"
    );
    // Two starts of Maven under one COMMAND: which ran in which can't be told, so neither the
    // first start's failure nor the second's pass is recorded.
    assert!(
        stderr.contains("Maven started 2 times under COMMAND"),
        "{stderr}"
    );
    assert!(r.unit_results().is_empty(), "{stderr}");
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

// covers: C-39, C-47
#[test]
fn junit5_tests_that_look_alike_in_subclasses_and_nested_classes_are_left_out_one_by_one() {
    let Some(maven) = maven() else { return };
    let mut files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file("pom.xml", &parent(&["core"], "5", "3.5.4")),
        file("core/pom.xml", &module_pom("core", &[])),
    ];
    for (path, text) in LOOK_ALIKE_J5 {
        files.push(file(&format!("core/src/test/java/{path}"), text));
    }
    let r = Repo::new(&files, maven);
    let first = r.run(&[], &["test"], LOOK_ALIKE_FIRST);
    assert_eq!(first.code, 1, "{}\n{}", first.stderr, first.stdout);
    // ATest's, OuterTest's and their inherited tests passed and are listed; BTest's and Inner's
    // look-alikes failed, aren't, and run again, and fail again.
    let second = r.run(&[], &["test"], LOOK_ALIKE_SECOND);
    assert_eq!(second.code, 1, "{}\n{}", second.stderr, second.stdout);
    let failed: Vec<&str> = second
        .stdout
        .lines()
        .filter(|l| l.contains("<<< FAILURE!") && l.starts_with("[ERROR] shop."))
        .map(|l| l["[ERROR] ".len()..].split(' ').next().unwrap())
        .collect();
    // The look-alikes that passed were listed, and left out: all 13 that passed but the audit's.
    let reused = r.newest_result()["units"]["tests"]["reused"].clone();
    assert_eq!(reused, 12, "{}", second.stderr);
    // Surefire names a nested class's test by its outer class: OuterTest's own `inherited` and
    // `same` never fail, so these are Inner's.
    for name in [
        "shop.BTest.inherited",
        "shop.OuterTest.inherited",
        "shop.OuterTest.same",
    ] {
        assert!(
            failed.contains(&name),
            "{name}: {failed:?}\n{}",
            second.stdout
        );
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

// covers: C-39, C-47
#[test]
fn junit4_tests_that_look_alike_in_subclasses_are_left_out_one_by_one_through_vintage() {
    let Some(maven) = maven() else { return };
    let mut files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(
            "pom.xml",
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>shop</groupId><artifactId>vintage</artifactId><version>1.0</version>
  <properties><maven.compiler.release>11</maven.compiler.release><project.build.sourceEncoding>UTF-8</project.build.sourceEncoding></properties>
  <dependencies>
    <dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.13.2</version><scope>test</scope></dependency>
    <dependency><groupId>org.junit.vintage</groupId><artifactId>junit-vintage-engine</artifactId><version>5.14.4</version><scope>test</scope></dependency>
  </dependencies>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-surefire-plugin</artifactId><version>3.5.4</version></plugin></plugins></build>
</project>
"#,
        ),
    ];
    for (path, text) in LOOK_ALIKE_J4 {
        files.push(file(&format!("src/test/java/{path}"), text));
    }
    let r = Repo::new(&files, maven);
    let first = r.run(&[], &["test"], LOOK_ALIKE_J4_FIRST);
    assert_eq!(first.code, 1, "{}\n{}", first.stderr, first.stdout);
    let second = r.run(&[], &["test"], LOOK_ALIKE_J4_SECOND);
    assert_eq!(second.code, 1, "{}\n{}", second.stderr, second.stdout);
    // The look-alikes that passed were listed, and left out: all 13 that passed but the audit's.
    let reused = r.newest_result()["units"]["tests"]["reused"].clone();
    assert_eq!(reused, 12, "{}", second.stderr);
    let failed: Vec<&str> = second
        .stdout
        .lines()
        .filter(|l| l.contains("<<< FAILURE!") && l.starts_with("[ERROR] shop."))
        .map(|l| l["[ERROR] ".len()..].split(' ').next().unwrap())
        .collect();
    for name in [
        "shop.BTest.inherited",
        "shop.SubTest.inherited",
        "shop.Base2Test.inherited",
    ] {
        assert!(
            failed.contains(&name),
            "{name}: {failed:?}\n{}",
            second.stdout
        );
    }
}

// covers: C-30, C-47
#[test]
fn a_jvm_file_no_module_of_the_build_owns_is_the_rest() {
    let Some(maven) = maven() else { return };
    // A module the reactor leaves out, one in a profile that isn't active, and a tool beside
    // app's sources that a script builds.
    let mut files = shop("reuse");
    for (path, text) in files.iter_mut() {
        if path == "pom.xml" {
            *text = text.replace(
                "</modules>",
                "</modules>\n  <profiles><profile><id>extra</id><modules><module>prof</module></modules></profile></profiles>",
            );
        }
    }
    files.extend([
        file("extra/pom.xml", &module_pom("extra", &[])),
        file(
            "extra/src/main/java/shop/extra/E.java",
            "package shop.extra;\npublic class E {}\n",
        ),
        file("prof/pom.xml", &module_pom("prof", &[])),
        file(
            "prof/src/main/java/shop/prof/P.java",
            "package shop.prof;\npublic class P {}\n",
        ),
        file(
            "app/tools/Gen.java",
            "package tools;\npublic class Gen {}\n",
        ),
    ]);
    let r = Repo::new(&files, maven);
    let first = r.run(&[], &["test"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stderr, first.stdout);
    let map = r.map();
    for path in [
        "extra/src/main/java/shop/extra/E.java",
        "prof/src/main/java/shop/prof/P.java",
        "app/tools/Gen.java",
    ] {
        assert!(map["rest"][path].is_string(), "{path}: {map:#}");
    }
    r.write(
        "extra/src/main/java/shop/extra/E.java",
        "package shop.extra;\npublic class E { int x; }\n",
    );
    let changed = r.run(&[], &["test"], &[]);
    assert_eq!(changed.code, 0, "{}\n{}", changed.stderr, changed.stdout);
    assert_eq!(changed.ran().len(), 6, "{:?}", changed.ran());
}

// covers: C-30, C-47
#[test]
fn a_module_this_command_didnt_build_keeps_its_sources_on_the_lists_of_the_tests_that_read_them() {
    let Some(maven) = maven() else { return };
    let r = Repo::new(&shop("reuse"), maven);
    // app and what it depends on: lib's sources are on app's list, other's are the rest.
    let first = r.run(&[], &["-pl", "app", "-am", "test"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stderr, first.stdout);
    let map = r.map();
    let reads = map["units"][CART_TEST]["reads"].as_array().unwrap().clone();
    assert!(
        reads.contains(&serde_json::json!("lib/src/main/java/shop/lib/Price.java")),
        "{map:#}"
    );
    assert!(
        map["rest"]["other/src/main/java/shop/other/Greet.java"].is_string(),
        "{map:#}"
    );
    r.write(
        "lib/src/main/java/shop/lib/Price.java",
        "package shop.lib;\n\npublic class Price {\n  public static int of(int x) { return x + x; }\n}\n",
    );
    let second = r.run(&[], &["-pl", "app", "-am", "test"], &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stderr, second.stdout);
    assert!(
        second.ran().contains_key("shop.app.CartTest"),
        "{:?}",
        second.ran()
    );
}

// covers: C-32, C-47
#[test]
fn a_test_that_ends_its_fork_with_system_exit_records_no_pass() {
    let Some(maven) = maven() else { return };
    // JUnit 4 without the hook and JUnit 5 with it: `b` ends the fork with `System.exit(0)`.
    let exit4 = "package shop.core;\n\nimport org.junit.*;\nimport org.junit.runners.MethodSorters;\n\n@FixMethodOrder(MethodSorters.NAME_ASCENDING)\npublic class ExitTest {\n  @Test public void a() {}\n  @Test public void b() { if (System.getenv(\"SHOP_EXIT\") != null) System.exit(0); }\n  @Test public void c() { Assert.fail(\"c\"); }\n}\n";
    let exit5 = "package shop.lib;\n\nimport org.junit.jupiter.api.*;\n\n@TestMethodOrder(MethodOrderer.MethodName.class)\nclass ExitTest {\n  @Test void a() {}\n  @Test void b() { if (System.getenv(\"SHOP_EXIT\") != null) System.exit(0); }\n  @Test void c() { Assertions.fail(\"c\"); }\n}\n";
    let mut four = junit4("observe");
    four.push(file("core/src/test/java/shop/core/ExitTest.java", exit4));
    let mut five = shop("observe");
    five.push(file("lib/src/test/java/shop/lib/ExitTest.java", exit5));
    // Run last of its module: JUnit 4's AskTest, CalcTest and CrashTest pass first; lib's
    // RoundTest and PriceTest, and other's three, in JUnit 5.
    for (files, order, passed) in [(four, "alphabetical", 3), (five, "reversealphabetical", 5)] {
        let r = Repo::new(&files, maven.clone());
        let ran = r.run(
            &[],
            &[
                "--fail-at-end",
                "test",
                &format!("-Dsurefire.runOrder={order}"),
            ],
            &[("SHOP_EXIT", "1")],
        );
        assert_ne!(ran.code, 0, "{}\n{}", ran.stderr, ran.stdout);
        assert!(
            ran.stdout.contains("The forked VM terminated"),
            "{}",
            ran.stdout
        );
        assert_eq!(
            r.unit_results(),
            BTreeMap::from([("pass".into(), passed)]),
            "{order}: {}\n{}",
            ran.stderr,
            ran.stdout
        );
    }
}

/// A module of the reactor: `name`, its parent the reactor's, with `body` inside `<project>`.
fn reactor_module(name: &str, body: &str) -> String {
    format!(
        r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>shop</groupId><artifactId>parent</artifactId><version>1.0</version></parent>
  <artifactId>{name}</artifactId>
  {body}
</project>
"#
    )
}

fn greens(dir: &str, pkg: &str, n: usize) -> Vec<(String, String)> {
    (1..=n)
        .map(|i| {
            file(
                &format!("{dir}/{pkg}/Green{i}Test.java"),
                &format!("package {pkg};\n\nimport org.junit.jupiter.api.Test;\n\nclass Green{i}Test {{\n  @Test void green() {{}}\n}}\n"),
            )
        })
        .collect()
}

// covers: C-47
#[test]
fn test_classes_a_module_reaches_through_another_modules_test_jar_are_read() {
    let Some(maven) = maven() else { return };
    // core makes a test-jar of its test classes, Fixtures among them; testkit depends on it at
    // compile scope; x depends on testkit, and XTest reads Fixtures through Kit.
    let mut files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file("pom.xml", &parent(&["core", "testkit", "x"], "5", "3.5.4")),
        file(
            "core/pom.xml",
            &reactor_module(
                "core",
                "<build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-jar-plugin</artifactId><version>3.5.0</version><executions><execution><goals><goal>test-jar</goal></goals></execution></executions></plugin></plugins></build>",
            ),
        ),
        file(
            "core/src/main/java/core/Core.java",
            "package core;\n\npublic class Core {}\n",
        ),
        file(
            "core/src/test/java/core/Fixtures.java",
            "package core;\n\npublic class Fixtures {\n  public static int v() { return 1; }\n}\n",
        ),
        file(
            "core/src/test/java/core/CoreTest.java",
            "package core;\n\nimport org.junit.jupiter.api.Test;\n\nclass CoreTest {\n  @Test void core() {}\n}\n",
        ),
        file(
            "testkit/pom.xml",
            &reactor_module(
                "testkit",
                "<dependencies><dependency><groupId>shop</groupId><artifactId>core</artifactId><version>1.0</version><type>test-jar</type></dependency></dependencies>",
            ),
        ),
        file(
            "testkit/src/main/java/kit/Kit.java",
            "package kit;\n\npublic class Kit {\n  public static int v() { return core.Fixtures.v(); }\n}\n",
        ),
        file(
            "x/pom.xml",
            &reactor_module(
                "x",
                "<dependencies><dependency><groupId>shop</groupId><artifactId>testkit</artifactId><version>1.0</version><scope>test</scope></dependency></dependencies>",
            ),
        ),
        file(
            "x/src/test/java/x/XTest.java",
            "package x;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.*;\n\nclass XTest {\n  @Test void v() { assertEquals(1, kit.Kit.v()); }\n}\n",
        ),
    ];
    files.extend(greens("x/src/test/java", "x", 9));
    let r = Repo::new(&files, maven);
    let first = r.run(&[], &["test"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stderr, first.stdout);
    let map = r.map();
    let reads = map["units"]["x/src/test/java/x/XTest.java"]["reads"].clone();
    assert!(
        reads
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("core/src/test/java/core/Fixtures.java")),
        "{map:#}"
    );
    r.write(
        "core/src/test/java/core/Fixtures.java",
        "package core;\n\npublic class Fixtures {\n  public static int v() { return 2; }\n}\n",
    );
    let second = r.run(&[], &["test"], &[]);
    assert_eq!(second.code, 1, "{}\n{}", second.stderr, second.stdout);
    assert!(second.stdout.contains("x.XTest.v"), "{}", second.stdout);
}
