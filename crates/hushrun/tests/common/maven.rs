//! \[ADR 0088\] The Maven reactors and tools the tests of Maven's adapter run, in-process
//! (`units_maven.rs`) and end to end (`e2e_maven.rs`). See `units_maven.rs` for the tools.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::repo::Repo;

/// What makes a fixture's `failsWhenAsked` tests fail: not a file, so not part of any key.
pub const FAIL: &str = "SHOP_FAIL";
/// What makes `other`'s `failsWhenOtherAsked` fail.
pub const OTHER: &str = "SHOP_OTHER";

/// Whether a tool the tests need is `here`: a test without it says so and passes, but under
/// `HUSHRUN_TEST_TOOLS=required`, which CI sets, where it fails.
pub fn tools(what: &str, here: bool) -> bool {
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
pub fn maven() -> Option<Maven> {
    let mvn = std::env::var("HUSHRUN_TEST_MAVEN")
        .ok()
        .filter(|m| !m.is_empty());
    let repo = std::env::var("HUSHRUN_TEST_MAVEN_REPO")
        .ok()
        .filter(|r| !r.is_empty());
    let found = mvn.zip(repo).filter(|(mvn, _)| {
        hushrun_testkit::tool::command(mvn)
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
    found.map(|(mvn, repo)| Maven {
        mvn,
        m2: PathBuf::from(repo),
    })
}

/// Whether Maven runs online, filling the tests' repository (`HUSHRUN_TEST_MAVEN_ONLINE=1`).
pub fn online() -> bool {
    std::env::var("HUSHRUN_TEST_MAVEN_ONLINE").as_deref() == Ok("1")
}

/// The tests' Maven: the path of its `mvn`, and the local repository it reads offline.
#[derive(Clone)]
pub struct Maven {
    pub mvn: String,
    pub m2: PathBuf,
}

/// A repository of a reactor, committed, whose COMMAND runs the tests' Maven: none of a person's
/// Maven or JVM settings, nor the fixtures' own variables, reach it.
pub struct Reactor {
    repo: Repo,
    maven: Maven,
}

impl Reactor {
    /// A reactor of `files`, `target/` ignored, committed.
    pub fn new(files: &[(String, String)], maven: Maven) -> Reactor {
        let mut all = vec![file(".gitignore", "target/\n")];
        all.extend(files.iter().cloned());
        let repo = Repo::new(&all).clear(&[
            "MAVEN_OPTS",
            "MAVEN_ARGS",
            "MAVEN_CONFIG",
            "JAVA_TOOL_OPTIONS",
            "JDK_JAVA_OPTIONS",
            "_JAVA_OPTIONS",
            FAIL,
            OTHER,
            "SHOP_CRASH",
        ]);
        Reactor { repo, maven }
    }

    /// COMMAND: Maven offline, its own local repository with the shared one below it.
    pub fn mvn(&self, args: &[&str]) -> Vec<String> {
        let mut out = vec![self.maven.mvn.clone(), "-B".to_string()];
        if online() {
            out.push(format!("-Dmaven.repo.local={}", self.maven.m2.display()));
        } else {
            out.push("-o".to_string());
            out.push(format!(
                "-Dmaven.repo.local={}",
                self.root.join("m2").display()
            ));
            out.push(format!(
                "-Dmaven.repo.local.tail={}",
                self.maven.m2.display()
            ));
        }
        out.extend(args.iter().map(|a| a.to_string()));
        out
    }

    /// Maven alone, outside hushrun.
    pub fn plain(&self, args: &[&str]) -> std::process::Output {
        let words = self.mvn(args);
        self.command(&words[0])
            .args(&words[1..])
            .output()
            .expect("mvn")
    }
}

impl std::ops::Deref for Reactor {
    type Target = Repo;

    fn deref(&self) -> &Repo {
        &self.repo
    }
}

/// How many tests Surefire ran of each top-level class, from its own lines.
pub fn classes(stdout: &str) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for line in stdout.lines() {
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

/// One file of a fixture: its path from the root, and its text.
pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

/// hushrun.yaml, with the one check `unit` and its `tests`.
pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// The reactor's parent POM: JUnit `junit` (`5` or `4`) and Surefire `surefire` for every module.
pub fn parent(modules: &[&str], junit: &str, surefire: &str) -> String {
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

/// A module's POM: `name`, its parent the reactor's, depending on the reactor's `depends`.
pub fn module_pom(name: &str, depends: &[&str]) -> String {
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
pub fn shop(tests: &str) -> Vec<(String, String)> {
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

pub const PRICE_TEST: &str = "lib/src/test/java/shop/lib/PriceTest.java";
pub const ROUND_TEST: &str = "lib/src/test/java/shop/lib/RoundTest.java";
pub const CART_TEST: &str = "app/src/test/java/shop/app/CartTest.java";
pub const GREET_TEST: &str = "other/src/test/java/shop/other/GreetTest.java";

/// The classes a run ran that it shouldn't have, but one: the audit runs one class of those a
/// plan could reuse (C-34).
pub fn at_most_the_audit(ran: &BTreeMap<String, u64>, must: &[&str]) {
    for class in must {
        assert!(ran.contains_key(*class), "{class} didn't run: {ran:?}");
    }
    let others = ran.keys().filter(|c| !must.contains(&c.as_str())).count();
    assert!(others <= 1, "more than the audited one ran: {ran:?}");
}

/// One module with JUnit 4 on Surefire 3.5.4, which runs it with its JUnit 4 provider: the
/// test-JVM hook doesn't load there.
pub fn junit4(tests: &str) -> Vec<(String, String)> {
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

/// One module with TestNG 7.10.2 on Surefire 3.5.4, which runs it with its TestNG provider: the
/// hook's interceptor loads there, its listener doesn't. Nine green classes, and `extra`.
pub fn testng(tests: &str, extra: &[(&str, &str)]) -> Vec<(String, String)> {
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

/// For [`testng`]: `SetupTest`, whose `@BeforeSuite` fails without a database (`SHOP_NO_DB`),
/// which skips every test of the suite, and `OrderTest`, whose `total` fails whenever it runs.
pub const SUITE_SETUP: &[(&str, &str)] = &[
    (
        "src/test/java/ng/SetupTest.java",
        "package ng;\nimport org.testng.annotations.*;\npublic class SetupTest {\n  @BeforeSuite public void db() { if (System.getenv(\"SHOP_NO_DB\") != null) throw new IllegalStateException(\"no database\"); }\n  @Test public void setupWorks() {}\n}\n",
    ),
    (
        "src/test/java/ng/OrderTest.java",
        "package ng;\nimport org.testng.annotations.*;\nimport static org.testng.Assert.*;\npublic class OrderTest {\n  @Test public void total() { assertEquals(1, 2); }\n}\n",
    ),
];

/// A module of the reactor: `name`, its parent the reactor's, with `body` inside `<project>`.
pub fn reactor_module(name: &str, body: &str) -> String {
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

/// `n` green JUnit 5 classes, `Green1Test` and on, in package `pkg` below `dir`.
pub fn greens(dir: &str, pkg: &str, n: usize) -> Vec<(String, String)> {
    (1..=n)
        .map(|i| {
            file(
                &format!("{dir}/{pkg}/Green{i}Test.java"),
                &format!("package {pkg};\n\nimport org.junit.jupiter.api.Test;\n\nclass Green{i}Test {{\n  @Test void green() {{}}\n}}\n"),
            )
        })
        .collect()
}

/// A reactor whose `core` has a JUnit 4 class with a static `suite()` that builds a JUnit 3 suite
/// of `testlib`'s checks, as Guava's testlib does: its cases are named by `testlib.Checks`, a class
/// of no test file, and one by `testlib.Gone`, which no class is. Only the suite class says which
/// class they came from. Beside it, two plain classes, and `other`, an unrelated module.
/// `AskTest` fails when asked; the suite's `testTwo` and its `check` fail when `SHOP_OTHER` asks.
pub fn suites(tests: &str) -> Vec<(String, String)> {
    let junit4 = r#"<dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.13.2</version></dependency>
    <dependency><groupId>org.junit.vintage</groupId><artifactId>junit-vintage-engine</artifactId><version>5.14.4</version><scope>test</scope></dependency>"#;
    let pom = |name: &str, deps: &str| {
        format!(
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>shop</groupId><artifactId>parent</artifactId><version>1.0</version></parent>
  <artifactId>{name}</artifactId>
  <dependencies>{deps}</dependencies>
</project>
"#
        )
    };
    let calc_test = |name: &str, a: u32| {
        format!(
            "package shop.{name};\n\nimport org.junit.Test;\nimport static org.junit.Assert.*;\n\npublic class CalcTest {{\n  @Test public void adds() {{ assertEquals({}, {a} + 1); }}\n}}\n",
            a + 1
        )
    };
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file(
            "pom.xml",
            &parent(&["testlib", "core", "other"], "4", "3.5.4").replace(
                "<dependencies><dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.13.2</version><scope>test</scope></dependency></dependencies>",
                "",
            ),
        ),
        file("testlib/pom.xml", &pom("testlib", junit4)),
        file(
            "testlib/src/main/java/testlib/Checks.java",
            &format!(
                r#"package testlib;

import junit.framework.TestCase;

public class Checks extends TestCase {{
  private final String subject;
  public Checks(String method, String subject) {{ super(method); this.subject = subject; }}
  @Override public String getName() {{ return super.getName() + "[" + subject + "]"; }}
  public void testOne() {{ assertTrue(subject.length() > 0); }}
  public void testTwo() {{ assertNull(System.getenv("{OTHER}")); }}
}}
"#
            ),
        ),
        file(
            "testlib/src/main/java/testlib/Described.java",
            &format!(
                r#"package testlib;

import junit.framework.AssertionFailedError;
import junit.framework.Test;
import junit.framework.TestResult;
import org.junit.runner.Describable;
import org.junit.runner.Description;

public class Described implements Test, Describable {{
  public int countTestCases() {{ return 1; }}
  public Description getDescription() {{ return Description.createTestDescription("testlib.Gone", "check"); }}
  public void run(TestResult result) {{
    result.startTest(this);
    if (System.getenv("{OTHER}") != null) result.addFailure(this, new AssertionFailedError("asked"));
    result.endTest(this);
  }}
}}
"#
            ),
        ),
        file(
            "core/pom.xml",
            &pom(
                "core",
                &format!("{junit4}<dependency><groupId>shop</groupId><artifactId>testlib</artifactId><version>1.0</version></dependency>"),
            ),
        ),
        file(
            "core/src/test/java/shop/core/MapSuiteTest.java",
            "package shop.core;\n\nimport junit.framework.Test;\nimport junit.framework.TestSuite;\nimport testlib.Checks;\nimport testlib.Described;\n\npublic class MapSuiteTest {\n  public static Test suite() {\n    TestSuite suite = new TestSuite(\"maps\");\n    suite.addTest(new Checks(\"testOne\", \"maps\"));\n    suite.addTest(new Checks(\"testTwo\", \"maps\"));\n    suite.addTest(new Described());\n    return suite;\n  }\n}\n",
        ),
        file("core/src/test/java/shop/core/CalcTest.java", &calc_test("core", 1)),
        file(
            "core/src/test/java/shop/core/AskTest.java",
            &format!(
                "package shop.core;\n\nimport org.junit.Test;\nimport static org.junit.Assert.*;\n\npublic class AskTest {{\n  @Test public void failsWhenAsked() {{ assertNull(System.getenv(\"{FAIL}\")); }}\n}}\n"
            ),
        ),
        file("other/pom.xml", &pom("other", junit4)),
        file("other/src/test/java/shop/other/CalcTest.java", &calc_test("other", 1)),
    ]
}

pub const SUITE_TEST: &str = "core/src/test/java/shop/core/MapSuiteTest.java";
pub const CORE_CALC_TEST: &str = "core/src/test/java/shop/core/CalcTest.java";

pub const PRICE_IT: &str = "lib/src/test/java/shop/lib/PriceIT.java";

/// `shop`, with lib's integration test, which Failsafe runs after Surefire's tests passed, and a
/// file `tests` may ignore below `notes/`.
pub fn failsafe(tests: &str) -> Vec<(String, String)> {
    let mut files = shop(tests);
    for (path, text) in files.iter_mut() {
        if path == "lib/pom.xml" {
            *text = text.replace(
                "<dependencies></dependencies>",
                "<dependencies></dependencies>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-failsafe-plugin</artifactId><version>3.5.4</version><executions><execution><goals><goal>integration-test</goal><goal>verify</goal></goals></execution></executions></plugin></plugins></build>",
            );
        }
    }
    files.push(file(
        PRICE_IT,
        "package shop.lib;

import org.junit.jupiter.api.Test;
import static org.junit.jupiter.api.Assertions.*;

class PriceIT {
  @Test void doubles() { assertEquals(8, Price.of(4)); }
}
",
    ));
    files.push(file(
        "notes/n.txt",
        "0
",
    ));
    files
}
