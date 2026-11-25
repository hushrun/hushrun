//! \[ADR 0087\] The Gradle builds and tools the tests of Gradle's adapter run, in-process
//! (`units_gradle.rs`) and end to end (`e2e_gradle.rs`). See `units_gradle.rs` for the tools.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use super::repo::Repo;

/// A repository of `files` whose COMMAND runs the tests' Gradle: none of a person's Gradle or
/// JVM settings reach it, and its Gradle user home is the tests' own.
pub fn repo(files: &[(String, String)]) -> Repo {
    let repo = Repo::new(files).clear(&[
        "HUSHRUN_GRADLE",
        "HUSHRUN_GRADLE_HOOK",
        "GRADLE_OPTS",
        "JAVA_TOOL_OPTIONS",
        "JAVA_OPTS",
    ]);
    match gradle_user_home() {
        Some(home) => repo.give("GRADLE_USER_HOME", home),
        None => repo,
    }
}

/// The test cases of a class Gradle's XML report in `repo` says ran, by name.
pub fn xml_cases(repo: &Path, project: &str, class: &str) -> Vec<String> {
    let path = repo
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

/// The line a build's `settings.gradle` loads hushrun's adapter with ([`hushrun::units::gradle::GROOVY_LINE`]).
pub const LINE: &str =
    r#"if (System.getenv("HUSHRUN_GRADLE")) apply from: System.getenv("HUSHRUN_GRADLE")"#;

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

/// `--offline`, the tests' Gradle option, but with `HUSHRUN_TEST_GRADLE_ONLINE=1`: then Gradle runs
/// online and fills the Gradle user home with what the tests' builds need, which is how CI fills
/// it once before its test run (a system property nothing reads stands in).
pub fn offline() -> &'static str {
    if std::env::var("HUSHRUN_TEST_GRADLE_ONLINE").as_deref() == Ok("1") {
        "-Dhushrun.tests.online=true"
    } else {
        "--offline"
    }
}

/// The Gradle the tests run.
pub fn gradle() -> String {
    std::env::var("HUSHRUN_TEST_GRADLE").unwrap_or_else(|_| "gradle".into())
}

/// The Gradle user home the tests run offline against.
pub fn gradle_user_home() -> Option<String> {
    std::env::var("HUSHRUN_TEST_GRADLE_USER_HOME")
        .ok()
        .filter(|h| !h.is_empty())
}

/// Whether Gradle and its user home are here, asked once.
pub fn has_gradle() -> bool {
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

/// What the tests need, for [`tools`].
pub const NEEDS: &str = "Gradle (HUSHRUN_TEST_GRADLE) and a Gradle user home with JUnit (HUSHRUN_TEST_GRADLE_USER_HOME)";

/// One file of a fixture: its path from the root, and its text.
pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

/// hushrun.yaml, with the check `unit` and its `tests`.
pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// What a fixture's Java projects test with.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Framework {
    Junit5,
    Junit4,
}

/// The build's root files: the settings file with its line, and every project a Java project.
pub fn build(framework: Framework, projects: &[&str], tests: &str) -> Vec<(String, String)> {
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
pub fn shop(tests: &str) -> Vec<(String, String)> {
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
pub fn command<'a>(gradle: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![gradle, offline(), "--console=plain", "test", "--continue"];
    args.extend_from_slice(extra);
    args
}

/// The test classes of [`shop`] the tests name.
pub const CART: &str = "app/src/test/java/shop/app/CartTest.java";
pub const PRICE: &str = "lib/src/test/java/shop/lib/PriceTest.java";

/// Projects `testlib`, `core` and `other` under JUnit 4: `core`'s `MapSuiteTest` has a static
/// `suite()`, run by `AllTests` (Gradle runs no class with a `suite()` alone), that runs `testlib`'s cases, as Guava's testlib does, one of a class no test file is
/// (`testlib.Checks`) and one whose class doesn't load (`testlib.Gone`). The suite's `testTwo` and
/// `check` fail when `SHOP_OTHER` asks; `AskTest` when `FAIL` does.
pub fn suites(tests: &str) -> Vec<(String, String)> {
    let mut files = build(Framework::Junit4, &["testlib", "core", "other"], tests);
    let calc = |project: &str, a: u32| {
        format!(
            "package shop.{project};\nimport org.junit.*;\npublic class CalcTest {{ @Test public void adds() {{ Assert.assertEquals({}, {a} + 1); }} }}\n",
            a + 1
        )
    };
    files.extend([
        file("testlib/build.gradle", "dependencies { implementation 'junit:junit:4.13.2' }\n"),
        file(
            "testlib/src/main/java/testlib/Checks.java",
            "package testlib;\nimport junit.framework.TestCase;\npublic class Checks extends TestCase {\n  private final String subject;\n  public Checks(String method, String subject) { super(method); this.subject = subject; }\n  @Override public String getName() { return super.getName() + \"[\" + subject + \"]\"; }\n  public void testOne() { assertTrue(subject.length() > 0); }\n  public void testTwo() { assertNull(System.getenv(\"SHOP_OTHER\")); }\n}\n",
        ),
        file(
            "testlib/src/main/java/testlib/Described.java",
            "package testlib;\nimport junit.framework.*;\nimport org.junit.runner.Describable;\nimport org.junit.runner.Description;\npublic class Described implements Test, Describable {\n  public int countTestCases() { return 1; }\n  public Description getDescription() { return Description.createTestDescription(\"testlib.Gone\", \"check\"); }\n  public void run(TestResult result) {\n    result.startTest(this);\n    if (System.getenv(\"SHOP_OTHER\") != null) result.addFailure(this, new AssertionFailedError(\"asked\"));\n    result.endTest(this);\n  }\n}\n",
        ),
        file("core/build.gradle", "dependencies { testImplementation project(':testlib') }\n"),
        file(
            "core/src/test/java/shop/core/MapSuiteTest.java",
            "package shop.core;\nimport junit.framework.Test;\nimport junit.framework.TestSuite;\nimport testlib.*;\n@org.junit.runner.RunWith(org.junit.runners.AllTests.class)\npublic class MapSuiteTest {\n  public static Test suite() {\n    TestSuite suite = new TestSuite(\"maps\");\n    suite.addTest(new Checks(\"testOne\", \"maps\"));\n    suite.addTest(new Checks(\"testTwo\", \"maps\"));\n    suite.addTest(new Described());\n    return suite;\n  }\n}\n",
        ),
        file("core/src/test/java/shop/core/CalcTest.java", &calc("core", 1)),
        file(
            "core/src/test/java/shop/core/AskTest.java",
            "package shop.core;\nimport org.junit.*;\npublic class AskTest { @Test public void failsWhenAsked() { Assert.assertNull(System.getenv(\"FAIL\")); } }\n",
        ),
        file("other/src/test/java/shop/other/CalcTest.java", &calc("other", 1)),
    ]);
    files
}

pub const G_SUITE: &str = "core/src/test/java/shop/core/MapSuiteTest.java";
pub const G_CORE_CALC: &str = "core/src/test/java/shop/core/CalcTest.java";

/// A Kotlin Multiplatform build of two projects, `lib` and `other`, each with a JVM and a
/// Kotlin/JS (Node.js) target and tests in `commonTest`, and `targets` added to each (a Kotlin/
/// Native one). `other`'s `flips` fails while `FAIL` is set; an `env` of each platform reads it.
pub fn kmp_build(tests: &str, targets: &str, natives: &[&str]) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(
            ".gitignore",
            "build/\n.gradle/\n.kotlin/\nkotlin-js-store/\n",
        ),
        file(
            "settings.gradle.kts",
            "pluginManagement { repositories { gradlePluginPortal(); mavenCentral() } }\ndependencyResolutionManagement { repositories { mavenCentral() } }\nrootProject.name = \"kmp\"\ninclude(\"lib\", \"other\")\nSystem.getenv(\"HUSHRUN_GRADLE\")?.let { apply(from = it) }\n",
        ),
        file(
            "build.gradle.kts",
            "plugins { kotlin(\"multiplatform\") version \"2.3.21\" apply false }\n",
        ),
    ];
    for project in ["lib", "other"] {
        files.push(file(
            &format!("{project}/build.gradle.kts"),
            &format!(
                "plugins {{ kotlin(\"multiplatform\") }}\nkotlin {{\n  jvm()\n  js {{ nodejs() }}\n{targets}  sourceSets {{ commonTest.dependencies {{ implementation(kotlin(\"test\")) }} }}\n}}\n"
            ),
        ));
        files.push(file(
            &format!("{project}/src/commonTest/kotlin/p/{project}/Env.kt"),
            &format!("package p.{project}\nexpect fun env(name: String): String?\n"),
        ));
        files.push(file(
            &format!("{project}/src/jvmTest/kotlin/p/{project}/Env.jvm.kt"),
            &format!(
                "package p.{project}\nactual fun env(name: String): String? = System.getenv(name)\n"
            ),
        ));
        files.push(file(
            &format!("{project}/src/jsTest/kotlin/p/{project}/Env.js.kt"),
            &format!("package p.{project}\nactual fun env(name: String): String? = js(\"process.env[name]\") as String?\n"),
        ));
        for native in natives {
            files.push(file(
                &format!("{project}/src/{native}Test/kotlin/p/{project}/Env.{native}.kt"),
                &format!("package p.{project}\nimport kotlinx.cinterop.toKString\n@OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)\nactual fun env(name: String): String? = platform.posix.getenv(name)?.toKString()\n"),
            ));
        }
    }
    files.extend([
        file(
            "lib/src/commonMain/kotlin/p/lib/Price.kt",
            "package p.lib\nobject Price { fun of(n: Int) = n * 2 }\n",
        ),
        // Two classes in one file are one unit: the file that declares them.
        file(
            "lib/src/commonTest/kotlin/p/lib/PriceTest.kt",
            "package p.lib\nimport kotlin.test.*\nclass PriceTest { @Test fun doubles() { assertEquals(6, Price.of(3)) } }\nclass ZeroTest { @Test fun zero() { assertEquals(0, Price.of(0)) } }\n",
        ),
        file(
            "other/src/commonTest/kotlin/p/other/OtherTest.kt",
            "package p.other\nimport kotlin.test.*\nclass OtherTest {\n  @Test fun works() { assertTrue(true) }\n  @Test fun flips() { assertNull(env(\"FAIL\")) }\n}\n",
        ),
        file(
            "other/src/commonTest/kotlin/p/other/MoreTest.kt",
            "package p.other\nimport kotlin.test.*\nclass MoreTest { @Test fun more() { assertEquals(2, 1 + 1) } }\n",
        ),
    ]);
    files
}

/// The test classes of [`kmp_build`] the tests name.
pub const OTHER: &str = "other/src/commonTest/kotlin/p/other/OtherTest.kt";
pub const MORE: &str = "other/src/commonTest/kotlin/p/other/MoreTest.kt";
pub const LIB_PRICE: &str = "lib/src/commonTest/kotlin/p/lib/PriceTest.kt";
