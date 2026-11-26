//! \[ADR 0087\] Test classes with Gradle (C-30 to C-34, C-39, C-46): a unit is a test class, named
//! by its source file, a test is one of its methods, and what a class read is every source file
//! below its project and each project its classpath is built from.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- gradle …` would, through Gradle's adapter's own type, `gradle::Recognized`, and the
//! code every adapter shares, and never starts the `hushrun` binary (`common/in_process.rs`).
//! Gradle itself runs as COMMAND, for real. What only the CLI shows of Gradle (its lines, the
//! audit's second run, `hushrun why`) is in `e2e_gradle.rs`.
//!
//! The fixtures are generated builds (JUnit 5, JUnit 4, TestNG, Spock, Kotest), run by the Gradle
//! that `HUSHRUN_TEST_GRADLE` names (a `gradle` program) with `--offline`, against the Gradle user
//! home `HUSHRUN_TEST_GRADLE_USER_HOME` names, which must hold every library and plugin they use.
//! With `HUSHRUN_TEST_GRADLE_ONLINE=1` they run online and fill that home instead, which is how CI
//! fills it once before its test run: what the tests need is the tests' own builds, never a list
//! beside them. They say so and pass where Gradle is missing, unless `HUSHRUN_TEST_TOOLS=required`,
//! which CI sets. Daemons stay warm between tests, as they do on a person's machine.
//!
//! The Android test needs Android's SDK as well, named by `HUSHRUN_TEST_ANDROID_SDK`, and Android's
//! Gradle plugin, 9.1.1 or the version `HUSHRUN_TEST_ANDROID_PLUGIN` names (8.13.2 for Gradle 8).
//! Without the SDK it fails under `HUSHRUN_TEST_TOOLS=required`, as every tool does, unless the run
//! opts out of it by name with `HUSHRUN_TEST_ANDROID=skip`.

#[path = "common/gradle.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::collections::BTreeSet;
use std::path::Path;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, gradle};
use serde_json::{Value, json};

use fixtures::*;
use in_process::{Ran, Repo};

/// What Gradle's adapter answers that never changes in a run, for the code every adapter shares.
fn profile(adapter: &gradle::Recognized) -> Profile {
    Profile {
        word: adapter.word(),
        runner: adapter.runner(),
        test_file: adapter.test_file(),
        tests: adapter.tests(),
        hints: adapter.hints(),
        binds: adapter.binds(),
        keys: adapter.keys(),
        not_by_test: adapter.not_by_test(),
        not_watched: adapter.not_watched(),
    }
}

/// `hushrun run unit -- ARGS`, with `env` besides, in-process, through Gradle's adapter alone.
fn run(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to Gradle's adapter (`gradle::recognized`), with no
    // trait object: what COMMAND is, and whether its settings file loads the adapter.
    let adapter = match gradle::recognize(hushrun::units::command::without_env(&command)) {
        Some(Ok(adapter)) => adapter,
        Some(Err(why)) => return in_process::Run::new(r, "unit", args, env).unobserved(&why),
        None => panic!("{args:?} doesn't start Gradle"),
    };
    if let Some(why) = gradle::unloadable(&adapter, &r.repo) {
        return in_process::Run::new(r, "unit", args, env).unobserved(&why);
    }
    observe(r, args, env, &adapter)
}

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order. Every call is on `gradle::Recognized` itself, which the static map resolves to its
/// own `impl Adapter`; through a trait object or a generic it would reach every adapter
/// (CONTRIBUTING.md, "Which tests CI runs again"). Another adapter's test file copies this
/// function and changes the type.
fn observe(r: &Repo, args: &[&str], env: &[(&str, &str)], adapter: &gradle::Recognized) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // Gradle's adapter keeps the trait's `plan`, which is its `plan_from_files`.
            Ok(context) => {
                let planning = adapter.plan_from_files(&observer.plan_at(&input, &context));
                observer.planned(&input, context, planning)
            }
            Err(planned) => planned,
        };
        drop(input);
        run.planned(&observer, planned);
    }
    // COMMAND, once, or \[C-32\] once more with nothing left out when the adapter left out what
    // the plan didn't list.
    let mut starts = 0;
    let (code, mut observed) = loop {
        starts += 1;
        let start = adapter.start_variables(observer.run_dir());
        let (code, passed) = run.start_command(&observer, &start);
        let end = run.finish_input(passed);
        let observed = match observer.read_context(&end) {
            Ok(context) => {
                let read = adapter.read(&observer.read_at(&end, &context));
                observer.finished(&end, context, read)
            }
            Err(observed) => observed.map(|observed| *observed),
        };
        drop(end);
        if starts == 1 && observed.as_ref().is_some_and(|o| o.left_out.is_some()) {
            let extras = adapter.again(observer.root(), observer.cwd(), &BTreeSet::new());
            if observer.whole_again(extras).is_ok() {
                continue;
            }
        }
        break (code, observed);
    };
    // \[C-34\] An audited unit that failed runs once more, alone.
    if let Some(o) = observed.as_mut().filter(|o| !o.again.is_empty()) {
        let wanted: BTreeSet<String> = o.again.keys().cloned().collect();
        let extras = adapter.again(observer.root(), o.cwd(), o.unit_files());
        let ended = match observer.again(o, &wanted, extras) {
            Some(again) => {
                let start = adapter.start_variables(again.dir());
                let again = again.with_start(start);
                let first = adapter.start_variables(observer.run_dir());
                let passed = run.start_again(&observer, &first, &again);
                let files = again.files();
                let map = in_process::no_map();
                let read = adapter
                    .read(&observer.again_read_at(&files, o, &run.tree, passed, &map))
                    .ok()
                    .flatten();
                observer.again_ended(&again, o, passed, read)
            }
            None => in_process::Run::not_again(&wanted),
        };
        run.disagreements(o, ended);
    }
    run.record(code, observed)
}

// covers: C-30, C-31, C-32, C-34, C-46
#[test]
fn test_classes_are_observed_then_reused_and_a_change_runs_the_classes_that_read_it() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&shop("reuse"));
    let first = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    assert_eq!(
        first.line("unit: every"),
        "unit: every test class runs: no run here has listed Gradle's test classes yet",
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
    // A project is read as its source directories (ADR 0087).
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
    assert_eq!(first.units, json!({"total": 5, "ran": 5, "reused": 0}));

    // lib changes: lib's and app's classes run, other's were reused but the one audited.
    r.write(
        "lib/src/main/java/shop/lib/Price.java",
        "package shop.lib;\npublic class Price { public static int of(int x) { return x + x; } }\n",
    );
    let changed = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(changed.code, 0, "{}", changed.said());
    assert_eq!(
        changed.line("unit: "),
        "unit: 3 of 5 test classes run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    let saved = changed.units["savedMs"].clone();
    assert_eq!(
        changed.units,
        json!({"total": 5, "ran": 3, "reused": 2, "savedMs": saved, "audited": 1})
    );
    // Gradle didn't run the reused classes: two of other's three wrote no report.
    let ran_in_other = ["A", "B", "C"]
        .iter()
        .filter(|n| !xml_cases(&r.repo, "other", &format!("shop.other.{n}Test")).is_empty())
        .count();
    assert_eq!(ran_in_other, 1, "{}", changed.stdout);
    // \[C-37\] What `hushrun why` says of a class is end to end: e2e_gradle.rs.
}

// covers: C-39, C-46, C-32
#[test]
fn junit5_tests_are_reused_one_by_one_and_a_class_whose_setup_failed_reruns() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    // lib keeps no test here: a task whose list didn't change is up to date, and Gradle wouldn't
    // run an audited class of it (ADR 0087).
    let mut files = shop("{mode: reuse, unit: test}");
    files.retain(|(path, _)| path != PRICE);
    files.push(file(
        "app/src/test/java/shop/app/SetupTest.java",
        "package shop.app;\nimport org.junit.jupiter.api.*;\nclass SetupTest {\n  @BeforeAll static void up() { Assertions.assertNull(System.getenv(\"FAIL\")); }\n  @Test void works() {}\n}\n",
    ));
    let r = repo(&files);
    let cc = ["--configuration-cache"];
    let first = run(&r, &command(&gradle, &cc), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
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
    let second = run(&r, &command(&gradle, &cc), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    assert!(
        second.stdout.contains("Reusing configuration cache"),
        "{}",
        second.stdout
    );
    assert_eq!(
        hushrun_testkit::lines::drawn(second.line("unit: ")),
        hushrun_testkit::lines::drawn(
            "unit: 2 of 5 test classes run; 3 already passed on the files they read, and so did 2 tests of the others, and 1 more test that already passed runs anyway, as a check"
        )
    );
    // \[C-39\] Tests are counted where Gradle named them, and a class reused whole, which Gradle
    // named none of, by the tests this machine knows of it: every one reused.
    assert_eq!(
        second.units["tests"],
        json!({"total": 8, "ran": 3, "reused": 5})
    );
    // A nested class's method was left out too (Gradle 9's filter can't do it; the hook can):
    // of Inner's two, at most the audited one ran.
    let inner = xml_cases(&r.repo, "app", "shop.app.CartTest$Inner");
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
    let r = repo(&files);
    let first = run(&r, &command(&gradle, &[]), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    assert_eq!(
        r.map()["units"]["app/src/test/java/shop/app/ParamTest.java"]["tests"],
        json!({"shop.app.ParamTest.flips": "failed", "shop.app.ParamTest.small": "passed"})
    );
    let second = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    // total and small passed, and PlainTest passed whole: of their three tests, the audit runs one,
    // wherever the draw finds it.
    assert_eq!(
        hushrun_testkit::lines::drawn(second.line("unit: ")),
        hushrun_testkit::lines::drawn(
            "unit: 3 of 5 tests run, in 2 of 3 test classes; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
    // Gradle's patterns left out either `total`, or every invocation of `small`.
    let total = xml_cases(&r.repo, "app", "shop.app.CartTest");
    let small: Vec<String> = xml_cases(&r.repo, "app", "shop.app.ParamTest")
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
    let r = repo(&shop("observe"));
    let first = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    let before = r.map();
    // A file no test reads changes: the check misses, and Gradle finds every test task up to date.
    r.write("README.md", "A shop, again.\n");
    let again = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(again.code, 0, "{}", again.said());
    assert!(
        again.stdout.contains("> Task :app:test UP-TO-DATE"),
        "{}",
        again.stdout
    );
    // \[C-32\] One true line: no test ran, why, and what makes Gradle run them.
    assert_eq!(
        again.line("unit: Gradle ran no test"),
        "unit: Gradle ran no test: :app:test, :lib:test and :other:test were up to date, which it does when they passed before on the same inputs, so this run learned nothing new of their test classes; a change to what they read makes Gradle run them"
    );
    assert_eq!(again.units, json!({"total": 5, "ran": 0, "reused": 0}));
    assert_eq!(r.map()["units"], before["units"]);
}

// covers: C-32, C-34, C-46
#[test]
fn a_run_in_which_gradle_ran_no_test_never_says_tests_ran() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&shop("reuse"));
    let first = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    // A file no test reads changes: every test class's key with it, and Gradle finds every test
    // task up to date, so nothing runs.
    r.write("README.md", "A shop, again.\n");
    let idle = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(idle.code, 0, "{}", idle.said());
    assert!(
        idle.stdout.contains("> Task :app:test UP-TO-DATE"),
        "{}",
        idle.stdout
    );
    // The adapter's note, once. (That the run's last line then says no count of tests, since
    // none ran, is the CLI's: e2e_gradle.rs.)
    assert_eq!(
        idle.count("unit: Gradle ran no test: "),
        1,
        "{}",
        idle.said()
    );
    assert_eq!(idle.units, json!({"total": 5, "ran": 0, "reused": 0}));
    // The next plan knows that run executed nothing: it claims no class runs, only that none is
    // left out, and says why none kept a pass.
    r.write(
        "lib/src/main/java/shop/lib/Price.java",
        "package shop.lib;\npublic class Price { public static int of(int x) { return x + x; } }\n",
    );
    let next = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(next.code, 0, "{}", next.said());
    assert_eq!(
        next.line("unit: "),
        "unit: none of its 5 test classes is left out: its last run here executed none of them, so none kept a pass on the files it read"
    );
    // Gradle ran lib's and app's tasks: the store forgets that a run executed nothing.
    r.write("README.md", "A shop, once more.\n");
    let after = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(after.code, 0, "{}", after.said());
    assert!(
        after
            .line("unit: ")
            .starts_with("unit: all 5 test classes run: "),
        "{}",
        after.said()
    );
}

// covers: C-46
#[cfg(unix)]
#[test]
fn a_settings_file_without_the_line_observes_nothing_and_says_so() {
    use std::os::unix::fs::PermissionsExt as _;
    // No Gradle is needed: the run stops at the settings file.
    let r = repo(&[
        file("hushrun.yaml", &yaml("observe")),
        file("settings.gradle", "rootProject.name = 'shop'\n"),
        file("gradlew", "#!/bin/sh\necho \"gradlew $*\"\n"),
    ]);
    std::fs::set_permissions(
        r.repo.join("gradlew"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let ran = run(&r, &["./gradlew", "test"], &[]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert_eq!(
        ran.line("unit: test files"),
        format!(
            "unit: test files aren't observed: Gradle loads hushrun's adapter from one line in settings.gradle, which `hushrun init` proposes: `{LINE}`"
        ),
    );
    assert!(ran.stdout.contains("gradlew test"), "{}", ran.stdout);
}

// covers: C-46
#[test]
fn with_isolated_projects_on_nothing_is_observed_and_the_build_is_as_it_was() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    // A build Isolated Projects takes: each project configures itself.
    let r = repo(&[
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
    let ran = run(
        &r,
        &command(&gradle, &["-Dorg.gradle.unsafe.isolated-projects=true"]),
        &[],
    );
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(
        ran.stdout.contains(
            "hushrun: Isolated Projects is on, so hushrun looks at no test class of this build"
        ),
        "{}",
        ran.stdout
    );
    assert!(ran.units.is_null(), "{}", ran.said());
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
        // Missing, it fails under HUSHRUN_TEST_TOOLS=required, as every tool does, unless the
        // run opts out of it by name: HUSHRUN_TEST_ANDROID=skip.
        if std::env::var("HUSHRUN_TEST_ANDROID").as_deref() == Ok("skip") {
            eprintln!("note: skipping: HUSHRUN_TEST_ANDROID=skip (ADR 0087)");
            return;
        }
        tools(
            "the Android SDK (HUSHRUN_TEST_ANDROID_SDK), or HUSHRUN_TEST_ANDROID=skip",
            false,
        );
        return;
    };
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let plugin = std::env::var("HUSHRUN_TEST_ANDROID_PLUGIN").unwrap_or_else(|_| "9.1.1".into());
    let r = repo(&[
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
        gradle.as_str(),
        offline(),
        "--console=plain",
        ":core:testDebugUnitTest",
    ];
    let env = [("ANDROID_HOME", sdk.as_str())];
    let first = run(&r, &args, &[env[0], ("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    assert_eq!(
        r.map()["units"]["core/src/test/java/shop/core/CoreTest.java"]["tests"],
        json!({"shop.core.CoreTest.again": "passed", "shop.core.CoreTest.flips": "failed",
               "shop.core.CoreTest.twice": "passed"})
    );
    let second = run(&r, &args, &env);
    assert_eq!(second.code, 0, "{}", second.said());
    assert_eq!(
        second.line("unit: "),
        "unit: 2 of 3 tests run, in 1 of 1 test class; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check"
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
    let r = repo(&testng_build("reuse"));
    // The suite's setup fails: TestNG skips every test, OrderTest's failing one among them.
    let first = run(&r, &command(&gradle, &[]), &[("NO_DB", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    // Only the setup's failure is recorded: no skipped class passed.
    assert_eq!(
        r.unit_results(),
        std::collections::BTreeMap::from([("fail".into(), 1)]),
        "{}",
        first.said()
    );
    // The database is back: OrderTest runs, and fails, as plain Gradle says.
    let second = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(second.code, 1, "{}", second.said());
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
    let r = repo(&processor_build(1));
    let first = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(first.code, 0, "{}", first.said());
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
    let second = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(second.code, 1, "{}", second.said());
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
        let r = repo(&crash_build());
        // The test JVM dies: Gradle fails the task, and no test of it failed.
        let ran = run(&r, &command(&gradle, &[]), &[("CRASH", place)]);
        assert_eq!(ran.code, 1, "{place}: {}", ran.said());
        assert!(
            ran.stderr.contains("finished with non-zero exit value 3"),
            "{place}: {}",
            ran.said()
        );
        // other's classes passed; CrashTest recorded nothing.
        assert_eq!(
            r.unit_results(),
            std::collections::BTreeMap::from([("pass".into(), 9)]),
            "{place} ({round}): {}",
            ran.said()
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
    let r = repo(&[
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
    let first = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(first.code, 1, "{}", first.said());
    // Nothing changed: one of the two that passed is left out, the other runs as a check, and
    // the one that failed runs, and fails again.
    let second = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(second.code, 1, "{}", second.said());
    let cases = xml_cases(&r.repo, ".", "ex.ParseSpec");
    assert!(
        cases.contains(&"parses [b]".to_string()),
        "{cases:?}\n{}",
        second.said()
    );
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
    let r = repo(&files);
    let first = run(
        &r,
        &command(&gradle, &[]),
        &[("FAIL", "1"), ("P_FAIL", "1")],
    );
    assert_eq!(first.code, 1, "{}", first.said());
    // The default package's add tests passed and may be left out; p.CalcTest's failed, and run.
    let second = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    let cases = xml_cases(&r.repo, "app", "p.CalcTest");
    assert_eq!(cases.len(), 9, "{cases:?}\n{}", second.said());
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
    let r = repo(&files);
    std::fs::set_permissions(
        r.repo.join("gradlew"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let ran = run(
        &r,
        &[
            "./gradlew",
            offline(),
            "--console=plain",
            "test",
            "--continue",
        ],
        &[("REAL_GRADLE", &gradle), ("COUNTS", &count_dir)],
    );
    assert_eq!(ran.code, 0, "{}", ran.said());
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
        ran.said()
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
    let r = repo(&files);
    let first = run(&r, &command(&gradle, &[]), LOOK_ALIKE_FIRST);
    assert_eq!(first.code, 1, "{}", first.said());
    let second = run(&r, &command(&gradle, &[]), LOOK_ALIKE_SECOND);
    assert_eq!(second.code, 1, "{}", second.said());
    // The look-alikes that passed were listed, and left out: all 13 that passed but the audit's.
    let reused = second.units["tests"]["reused"].clone();
    assert_eq!(reused, 12, "{}", second.said());
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
    let r = repo(&files);
    let first = run(&r, &command(&gradle, &[]), LOOK_ALIKE_J4_FIRST);
    assert_eq!(first.code, 1, "{}", first.said());
    let second = run(&r, &command(&gradle, &[]), LOOK_ALIKE_J4_SECOND);
    assert_eq!(second.code, 1, "{}", second.said());
    let reused = second.units["tests"]["reused"].clone();
    assert_eq!(reused, 12, "{}", second.said());
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
    let r = repo(&[
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
    let first = run(
        &r,
        &command(&gradle, &[]),
        &[("FLIP", "1"), ("BSpec_FAIL", "1"), ("Sub2Spec_FAIL", "1")],
    );
    assert_eq!(first.code, 1, "{}", first.said());
    let second = run(
        &r,
        &command(&gradle, &[]),
        &[("BSpec_FAIL", "1"), ("Sub2Spec_FAIL", "1")],
    );
    assert_eq!(second.code, 1, "{}", second.said());
    // ASpec's ten and BSpec's own, and Base2Spec's inherited: all 12 but the audit's.
    let reused = second.units["tests"]["reused"].clone();
    assert_eq!(reused, 11, "{}", second.said());
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
    let r = repo(&files);
    let first = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(first.code, 0, "{}", first.said());
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
    let changed = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(changed.code, 0, "{}", changed.said());
    assert_eq!(
        changed.line("unit: "),
        "unit: all 5 test classes run: app/tools/Gen.java changed, and any test may read it"
    );
}

// covers: C-30, C-46
#[test]
fn a_task_of_one_project_keeps_the_sources_it_read_through_another_on_its_list() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&shop("reuse"));
    let only = |args: &[&'static str]| -> Vec<&'static str> { args.to_vec() };
    let g: &'static str = Box::leak(gradle.into_boxed_str());
    // Only app's tests: lib's sources, which app's classes read, are on their list.
    let first = run(
        &r,
        &only(&[g, offline(), "--console=plain", ":app:test"]),
        &[],
    );
    assert_eq!(first.code, 0, "{}", first.said());
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
    let second = run(
        &r,
        &only(&[g, offline(), "--console=plain", ":app:test"]),
        &[],
    );
    assert_eq!(second.code, 0, "{}", second.said());
    assert!(
        second.stdout.contains("CartTest")
            || xml_cases(&r.repo, "app", "shop.app.CartTest").len() >= 2,
        "{}",
        second.stdout
    );
    assert_eq!(second.units["reused"], json!(0), "{}", second.said());
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
    let r = repo(&files);
    let ran = run(&r, &command(&gradle, &[]), &[("CRASH", "1")]);
    assert_eq!(ran.code, 1, "{}", ran.said());
    // AFailTest failed; other's passed; CrashTest recorded nothing.
    assert_eq!(
        r.unit_results(),
        std::collections::BTreeMap::from([("fail".into(), 1), ("pass".into(), 9)]),
        "{}",
        ran.said()
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
    let r = repo(&[
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
    let ran = run(&r, &command(&gradle, &[]), &[("EXIT", "1")]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    // The green classes passed; neither ExitTest did.
    assert_eq!(
        r.unit_results(),
        std::collections::BTreeMap::from([("pass".into(), 2)]),
        "{}",
        ran.said()
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
    // And one Maven downloaded from a remote repository, as Maven leaves it: `_remote.repositories`
    // names the remote for the jar, and nothing installed it here. It is as good as Gradle's cache.
    let dl = m2.join("com/other/dl/1.0");
    std::fs::create_dir_all(&dl).unwrap();
    std::fs::write(dl.join("dl-1.0.jar"), &empty).unwrap();
    std::fs::write(
        dl.join("dl-1.0.pom"),
        "<project><modelVersion>4.0.0</modelVersion><groupId>com.other</groupId><artifactId>dl</artifactId><version>1.0</version></project>\n",
    )
    .unwrap();
    std::fs::write(
        dl.join("_remote.repositories"),
        "#NOTE: This is a Maven Resolver internal implementation file.\ndl-1.0.jar>central=\ndl-1.0.pom>central=\n",
    )
    .unwrap();
    // lib publishes to the local repository, and app takes it from there, not as a project.
    let mut files = build(Framework::Junit5, &["lib", "app"], "reuse");
    files[3] = file(
        "build.gradle",
        "subprojects {\n  apply plugin: 'java'\n  repositories { mavenLocal(); mavenCentral() }\n  dependencies {\n    testImplementation platform('org.junit:junit-bom:5.11.4')\n    testImplementation 'org.junit.jupiter:junit-jupiter'\n    testRuntimeOnly 'org.junit.platform:junit-platform-launcher'\n  }\n  tasks.withType(Test).configureEach { useJUnitPlatform() }\n}\nproject(':lib') { apply plugin: 'maven-publish'; group = 'com.example'; version = '1.0'\n  publishing { publications { maven(MavenPublication) { from components.java } } } }\nproject(':app') {\n  dependencies { implementation 'com.example:lib:1.0'; testImplementation 'com.other:dl:1.0'; testImplementation(System.getenv('THING') ? 'com.other:thing:1.0' : 'com.example:lib:1.0') }\n  tasks.named('compileJava') { mustRunAfter(':lib:publishToMavenLocal') }\n  tasks.named('test') { mustRunAfter(':lib:publishToMavenLocal') }\n}\n",
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
    let r = repo(&files);
    let repo_local = format!("-Dmaven.repo.local={}", m2.display());
    let args = |extra: &[&'static str]| -> Vec<String> {
        let mut out: Vec<String> = [gradle.as_str(), offline(), "--console=plain"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        out.push(repo_local.clone());
        out.extend(extra.iter().map(|s| s.to_string()));
        out
    };
    let run = |extra: &[&'static str], env: &[(&str, &str)]| {
        let a = args(extra);
        run(&r, &a.iter().map(String::as_str).collect::<Vec<_>>(), env)
    };
    let tasks = [":lib:publishToMavenLocal", "test", "--continue"];
    let first = run(&tasks, &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    // app's classes read lib's sources: lib built the jar they took from the local repository.
    // The jar Maven downloaded is followed as Gradle's own cache is.
    let map = r.map();
    assert!(
        map["units"]["app/src/test/java/app/AppTest.java"]
            .get("notFollowed")
            .is_none(),
        "{map:#}"
    );
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
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.stdout.contains("AppTest > v() FAILED"),
        "{}",
        second.stdout
    );
    // A jar of the local repository no project of the build makes: the class that ran (AppTest,
    // which failed and runs again) isn't followed, and says why.
    let third = run(&tasks, &[("THING", "1")]);
    assert_eq!(third.code, 1, "{}", third.said());
    let map = r.map();
    assert_eq!(
        map["units"]["app/src/test/java/app/AppTest.java"]["notFollowed"],
        json!(["local-maven-repository"]),
        "{map:#}"
    );
}

// covers: C-39, C-46
#[test]
fn kotest_tests_which_cant_be_left_out_are_never_counted_as_reused() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&[
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "build/\n.gradle/\n.kotlin/\n"),
        file(
            "settings.gradle",
            &format!(
                "pluginManagement {{ repositories {{ gradlePluginPortal(); mavenCentral() }} }}\n{LINE}\nrootProject.name = 'kt'\n"
            ),
        ),
        file(
            "build.gradle",
            "plugins { id 'org.jetbrains.kotlin.jvm' version '2.3.21' }\nrepositories { mavenCentral() }\ndependencies { testImplementation 'io.kotest:kotest-runner-junit5:5.9.1'; testRuntimeOnly 'org.junit.platform:junit-platform-launcher' }\ntest { useJUnitPlatform() }\n",
        ),
        file(
            "src/test/kotlin/p/NamesSpec.kt",
            "package p\nimport io.kotest.core.spec.style.FunSpec\nclass NamesSpec : FunSpec({\n  test(\"one\") { }\n  test(\"two\") { check(System.getenv(\"FAIL\") == null) }\n  test(\"three\") { }\n})\n",
        ),
    ]);
    let first = run(&r, &command(&gradle, &[]), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    // Kotest's tests can't be left out one by one: none is listed, and the line says they run.
    let second = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    let line = second.line("unit: ");
    assert!(!line.contains("already passed"), "{line}");
    assert_eq!(xml_cases(&r.repo, ".", "p.NamesSpec").len(), 3);
}

// covers: C-32, C-46
#[test]
fn a_test_jvm_killed_timed_out_or_out_of_memory_records_no_pass_for_its_class() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    // app's one class does what `WHAT` says; other's nine green classes are another task's.
    let what = "package shop.app;\nimport org.junit.jupiter.api.*;\nclass WhatTest {\n  @Test void a() {}\n  @Test void b() throws Exception {\n    String what = System.getenv(\"WHAT\");\n    if (\"kill\".equals(what)) new ProcessBuilder(\"kill\", \"-9\", String.valueOf(ProcessHandle.current().pid())).start().waitFor();\n    if (\"slow\".equals(what)) Thread.sleep(120_000);\n    if (\"oom\".equals(what)) { java.util.List<long[]> all = new java.util.ArrayList<>(); while (true) all.add(new long[1 << 20]); }\n  }\n  @Test void c() {}\n}\n";
    for (case, expected) in [
        ("kill", vec![("pass", 9)]),
        ("slow", vec![("pass", 9)]),
        // A 16 MB test JVM out of memory takes Gradle's worker down with it: nothing of the task
        // is recorded, its failure neither.
        ("oom", vec![("pass", 9)]),
    ] {
        let mut files = build(Framework::Junit5, &["app", "other"], "reuse");
        files.push(file(
            "app/build.gradle",
            "tasks.named('test') { maxHeapSize = '16m'; timeout = java.time.Duration.ofSeconds(15) }\n",
        ));
        files.push(file("app/src/test/java/shop/app/WhatTest.java", what));
        for n in 1..=9 {
            files.push(file(
                &format!("other/src/test/java/shop/other/Green{n}Test.java"),
                &format!("package shop.other;\nimport org.junit.jupiter.api.*;\nclass Green{n}Test {{ @Test void green() {{}} }}\n"),
            ));
        }
        let r = repo(&files);
        let ran = run(&r, &command(&gradle, &[]), &[("WHAT", case)]);
        assert_eq!(ran.code, 1, "{case}: {}", ran.said());
        let expected: std::collections::BTreeMap<String, usize> = expected
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        assert_eq!(r.unit_results(), expected, "{case}: {}", ran.said());
    }
}

// covers: C-32, C-46
#[test]
fn a_test_task_gradle_restores_from_its_build_cache_records_and_leaves_out_nothing() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&shop("reuse"));
    let cache = ["clean", "--build-cache"];
    let first = run(&r, &command(&gradle, &cache), &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    // \[C-46\] The first run's tasks, of which the check knew nothing, executed and weren't cached;
    // the second's, which it knows, are.
    r.write("README.md", "A shop, too.\n");
    let second = run(&r, &command(&gradle, &cache), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    let before = r.map();
    // A file of the rest changes: every class runs as far as hushrun goes, the tasks' inputs are
    // those of the second run, and Gradle takes their outputs from its build cache.
    r.write("README.md", "A shop, again.\n");
    let again = run(&r, &command(&gradle, &cache), &[]);
    assert_eq!(again.code, 0, "{}", again.said());
    assert!(again.stdout.contains("FROM-CACHE"), "{}", again.stdout);
    assert_eq!(
        again.units,
        json!({"total": 5, "ran": 0, "reused": 0}),
        "{}",
        again.said()
    );
    assert_eq!(r.map()["units"], before["units"]);
}

// covers: C-39, C-46
#[test]
fn a_test_the_build_excludes_by_tag_is_neither_run_nor_listed() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let mut files = build(Framework::Junit5, &["app"], "{mode: reuse, unit: test}");
    files.push(file(
        "app/build.gradle",
        "tasks.named('test') { useJUnitPlatform { excludeTags 'slow' } }\n",
    ));
    files.push(file(
        "app/src/test/java/shop/app/TagTest.java",
        "package shop.app;\nimport org.junit.jupiter.api.*;\nclass TagTest {\n  @Test void fast() {}\n  @Test void flip() { Assertions.assertNull(System.getenv(\"FAIL\")); }\n  @Test @Tag(\"slow\") void slow() { Assertions.fail(\"never runs\"); }\n  @Test void more() {}\n}\n",
    ));
    let r = repo(&files);
    let first = run(&r, &command(&gradle, &[]), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    let second = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    // `fast` or `more` was left out, the other audited, `flip` ran; `slow` never.
    let cases = xml_cases(&r.repo, "app", "shop.app.TagTest");
    assert!(cases.contains(&"flip()".to_string()), "{cases:?}");
    assert!(!cases.contains(&"slow()".to_string()), "{cases:?}");
    assert_eq!(cases.len(), 2, "{cases:?}\n{}", second.said());
}

// covers: C-30, C-46
#[test]
fn a_jar_from_a_file_repository_in_the_checkout_is_read_by_the_project_that_published_it() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    // lib publishes to localrepo/, which git ignores, and app takes it from there.
    let mut files = build(Framework::Junit5, &["lib", "app"], "reuse");
    files[1] = file(".gitignore", "build/\n.gradle/\nlocalrepo/\n");
    files[3] = file(
        "build.gradle",
        "subprojects {\n  apply plugin: 'java'\n  repositories { maven { url = rootProject.file('localrepo') }; mavenCentral() }\n  dependencies {\n    testImplementation platform('org.junit:junit-bom:5.11.4')\n    testImplementation 'org.junit.jupiter:junit-jupiter'\n    testRuntimeOnly 'org.junit.platform:junit-platform-launcher'\n  }\n  tasks.withType(Test).configureEach { useJUnitPlatform() }\n}\nproject(':lib') { apply plugin: 'maven-publish'; group = 'com.example'; version = '1.0'\n  publishing { publications { maven(MavenPublication) { from components.java } }; repositories { maven { url = rootProject.file('localrepo') } } } }\nproject(':app') {\n  dependencies { implementation 'com.example:lib:1.0' }\n  tasks.named('compileJava') { mustRunAfter(':lib:publish') }\n  tasks.named('test') { mustRunAfter(':lib:publish') }\n}\n",
    );
    files.extend([
        file(
            "lib/src/main/java/lib/Lib.java",
            "package lib;\npublic class Lib { public static int v() { return 1; } }\n",
        ),
        file(
            "app/src/test/java/app/AppTest.java",
            "package app;\nimport org.junit.jupiter.api.*;\nclass AppTest { @Test void v() { Assertions.assertEquals(1, lib.Lib.v()); } }\n",
        ),
    ]);
    let r = repo(&files);
    let first = run(&r, &command(&gradle, &[":lib:publish"]), &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    let map = r.map();
    let app = &map["units"]["app/src/test/java/app/AppTest.java"];
    assert!(app.get("notFollowed").is_none(), "{map:#}");
    assert!(
        app["reads"]
            .as_array()
            .unwrap()
            .contains(&json!("lib/src/main/java/lib/Lib.java")),
        "{map:#}"
    );
}

// covers: C-39, C-46
#[test]
fn a_junit4_suite_classs_cases_no_test_file_owns_are_its_own_and_it_is_never_counted_as_reused() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&suites("{mode: reuse, unit: test}"));
    let first = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    // The suite's cases are the suite class's tests, by the class each names.
    let map = r.map();
    let suite_tests: Vec<&String> = map["units"][G_SUITE]["tests"]
        .as_object()
        .unwrap_or_else(|| panic!("{map:#}"))
        .keys()
        .collect();
    assert_eq!(
        suite_tests,
        [
            "testlib.Checks.testOne",
            "testlib.Checks.testTwo",
            "testlib.Gone.check"
        ],
        "{map:#}"
    );

    // A change in `other`: core's plain classes are reused, the suite class runs, as nothing of
    // it can be left out, and the plan says so.
    r.write(
        "other/src/test/java/shop/other/CalcTest.java",
        "package shop.other;\nimport org.junit.*;\npublic class CalcTest { @Test public void adds() { Assert.assertEquals(3, 2 + 1); } }\n",
    );
    let second = run(&r, &command(&gradle, &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    // The suite ran: Gradle 9 files its cases under the suite class, Gradle 8 under theirs.
    let reports: String = std::fs::read_dir(r.repo.join("core/build/test-results/test"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect();
    assert!(
        reports.contains("<testcase name=\"testOne"),
        "{}",
        second.stdout
    );
    assert_eq!(
        second.line("unit: "),
        "unit: 5 of 6 tests run, in 3 of 4 test classes; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check"
    );
    // (What `hushrun why` says of the suite class is end to end: e2e_gradle.rs.)

    // The suite's cases fail: the suite class failed, and core's plain classes, which ran, keep
    // their passes.
    r.write(
        G_CORE_CALC,
        "package shop.core;\nimport org.junit.*;\npublic class CalcTest { @Test public void adds() { Assert.assertEquals(2, 0 + 2); } }\n",
    );
    let failed = run(&r, &command(&gradle, &[]), &[("SHOP_OTHER", "1")]);
    assert_eq!(failed.code, 1, "{}", failed.said());
    let map = r.map();
    assert_eq!(
        map["units"][G_CORE_CALC]["tests"]["shop.core.CalcTest.adds"], "passed",
        "{map:#}"
    );
    assert_eq!(
        map["units"][G_SUITE]["tests"]["testlib.Gone.check"], "failed",
        "{map:#}"
    );
    r.clean();
}

/// A JUnit 4 task of one class with 300 tests that pass and one, `flips`, that fails when asked: a
/// run that fails, so that the next lists the 300.
fn many_junit4(gradle: &str) -> Repo {
    let mut body = String::from(
        "package shop.app;\nimport org.junit.*;\npublic class ManyTest {\n  @Test public void flips() { Assert.assertNull(System.getenv(\"FAIL\")); }\n",
    );
    for n in 0..300 {
        body.push_str(&format!("  @Test public void t{n:03}() {{}}\n"));
    }
    body.push_str("}\n");
    let mut files = build(Framework::Junit4, &["app"], "{mode: reuse, unit: test}");
    files.push(file("app/src/test/java/shop/app/ManyTest.java", &body));
    let r = repo(&files);
    let first = run(&r, &command(gradle, &[]), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    r
}

// covers: C-39, C-46
#[test]
fn a_junit4_task_tells_how_many_of_its_tests_it_leaves_out_at_most() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let r = many_junit4(&gradle());
    // The task's report, as the script keeps it in the task's outputs, as in its facts.
    let kept: Value = serde_json::from_slice(
        &std::fs::read(r.repo.join("app/build/hushrun/test.json")).expect("the kept report"),
    )
    .unwrap();
    assert_eq!(
        kept["report"]["leaveOutAtMost"],
        json!({"most": 250, "units": ["app/src/test/java/shop/app/ManyTest.java"]})
    );
    r.clean();
}

// covers: C-39, C-46
#[test]
fn a_junit4_task_leaves_out_every_test_the_plan_counts_as_reused() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let r = many_junit4(&gradle());
    let second = run(&r, &command(&gradle(), &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    // What the plan said would run is what ran.
    let said = second.line("unit: ");
    let ran = xml_cases(&r.repo, "app", "shop.app.ManyTest");
    let counted: usize = said["unit: ".len()..]
        .split(' ')
        .next()
        .and_then(|n| n.replace(',', "").parse().ok())
        .unwrap_or_else(|| panic!("{said}"));
    assert_eq!(counted, ran.len(), "{said}\n{ran:?}");
    r.clean();
}

// covers: C-46, C-32
#[test]
fn a_test_task_nothing_was_learned_of_under_the_check_executes_in_its_first_run() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    for (mode, extra) in [
        ("observe", &[][..]),
        ("reuse", &["--configuration-cache"][..]),
    ] {
        let r = repo(&shop(mode));
        let first = run(&r, &command(&gradle, extra), &[]);
        assert_eq!(first.code, 0, "{mode}: {}", first.said());
        // The check's store is lost (another machine's build directory, a cache restored without
        // it): Gradle would find every test task up to date, and the check would learn nothing.
        for entry in std::fs::read_dir(r.cache.join("units")).unwrap().flatten() {
            if entry.file_name().to_string_lossy().contains(".sqlite") {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
        r.write("README.md", "A shop, again.\n");
        let again = run(&r, &command(&gradle, extra), &[]);
        assert_eq!(again.code, 0, "{mode}: {}", again.said());
        for task in [":app:test", ":lib:test", ":other:test"] {
            assert!(
                again.stdout.lines().any(|l| l == format!("> Task {task}")),
                "{mode}: {task} didn't execute:\n{}",
                again.stdout
            );
        }
        assert!(
            !again.said().contains("Gradle ran no test"),
            "{mode}: {}",
            again.said()
        );
        assert_eq!(
            r.map()["units"].as_object().map_or(0, |u| u.len()),
            5,
            "{mode}: {:#}",
            r.map()
        );
        // Now that the check knows them, Gradle's own reuse stands: a run on the same inputs finds
        // them up to date again (in `reuse`, the plan's list is an input, and changed).
        if mode == "reuse" {
            r.clean();
            continue;
        }
        r.write("README.md", "A shop, once more.\n");
        let third = run(&r, &command(&gradle, extra), &[]);
        assert_eq!(third.code, 0, "{mode}: {}", third.said());
        assert!(
            third.stdout.contains("> Task :other:test UP-TO-DATE"),
            "{mode}: {}",
            third.stdout
        );
        r.clean();
    }
}

// --- Kotlin Multiplatform (ADR 0117) ------------------------------------------------------------

/// The test cases a task's XML report holds for one report file, `TEST-<file>.xml`.
fn task_cases(r: &Repo, project: &str, task: &str, report: &str) -> Vec<String> {
    let path = r
        .repo
        .join(project)
        .join(format!("build/test-results/{task}/TEST-{report}.xml"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.split("<testcase name=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next().map(str::to_string))
        .collect()
}

/// Every test a task's XML reports say ran, in `lib` and `other`, as `<class>.<method>`: a
/// Kotlin/JS class is named after its task too (`jsNodeTest.p.lib.PriceTest`), and a name holds
/// its platform (`doubles[js, node]`).
fn task_ran(r: &Repo, task: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for project in ["lib", "other"] {
        let dir = r.repo.join(project).join("build/test-results").join(task);
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for case in text.split("<testcase ").skip(1) {
                let attr = |name: &str| {
                    case.split(&format!("{name}=\""))
                        .nth(1)
                        .and_then(|v| v.split('"').next())
                        .map(str::to_string)
                };
                let (Some(name), Some(class)) = (attr("name"), attr("classname")) else {
                    continue;
                };
                let class = class
                    .strip_prefix(&format!("{task}."))
                    .unwrap_or(&class)
                    .to_string();
                out.insert(format!(
                    "{class}.{}",
                    name.split('[').next().unwrap_or(&name)
                ));
            }
        }
    }
    out
}

// covers: C-94, C-95, C-96
#[test]
fn kotlin_js_test_classes_are_units_and_their_tests_are_reused_one_by_one_beside_the_jvms() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&kmp_build("{mode: reuse, unit: test}", "", &[]));
    let args = |extra: &[&'static str]| -> Vec<String> {
        let mut a: Vec<String> = [gradle.as_str(), offline(), "--console=plain"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        a.extend(
            [
                "jvmTest",
                "jsNodeTest",
                "--continue",
                "--configuration-cache",
            ]
            .map(String::from),
        );
        a.extend(extra.iter().map(|s| s.to_string()));
        a
    };
    // Kotlin/JS installs its npm packages with yarn, `--offline` under Gradle's: yarn's cache sits
    // beside the Gradle user home, where the online run that fills that home fills it too.
    let yarn = gradle_user_home()
        .map(|home| format!("{home}/hushrun-yarn-cache"))
        .unwrap_or_default();
    let run = |env: &[(&str, &str)]| {
        let a = args(&[]);
        let mut env = env.to_vec();
        env.push(("YARN_CACHE_FOLDER", yarn.as_str()));
        run(&r, &a.iter().map(String::as_str).collect::<Vec<_>>(), &env)
    };
    let first = run(&[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    let map = r.map();
    // A Kotlin/JS class is named by the file that declares it, as its JVM twin is by its class
    // file: one unit for both tasks, whose tests are one test each.
    assert_eq!(
        map["units"][OTHER]["tests"],
        json!({"p.other.OtherTest.flips": "failed", "p.other.OtherTest.works": "passed"}),
        "{map:#}"
    );
    assert_eq!(
        map["units"][LIB_PRICE]["tests"],
        json!({"p.lib.PriceTest.doubles": "passed", "p.lib.ZeroTest.zero": "passed"}),
        "{map:#}"
    );
    assert!(
        !first.said().contains("isn't a JVM Test task"),
        "{}",
        first.said()
    );

    // The failed test runs again on both platforms, the one that passed doesn't.
    let second = run(&[]);
    assert_eq!(second.code, 0, "{}", second.said());
    // What each platform ran, by test: the same on both, the failed test and at most one more
    // that already passed, which the audit runs as a check.
    let js = task_ran(&r, "jsNodeTest");
    assert_eq!(js, task_ran(&r, "jvmTest"), "{}", second.stdout);
    assert!(js.contains("p.other.OtherTest.flips"), "{js:?}");
    assert!(js.len() <= 2, "{js:?}\n{}", second.stdout);
    // lib changes: lib's classes run on both platforms; other's, which don't read lib, are left
    // out whole on both but one that an audit runs.
    r.write(
        "lib/src/commonMain/kotlin/p/lib/Price.kt",
        "package p.lib\nobject Price { fun of(n: Int) = n + n }\n",
    );
    let third = run(&[]);
    assert_eq!(third.code, 0, "{}", third.said());
    let said = third.line("unit: ");
    assert!(
        said.contains("of 5 tests run") && said.contains("already passed on the files they read"),
        "{said}"
    );
    assert_eq!(
        task_cases(&r, "lib", "jsNodeTest", "jsNodeTest.p.lib.PriceTest"),
        ["doubles[js, node]"]
    );
    let other_ran = |task: &str, report: &str| !task_cases(&r, "other", task, report).is_empty();
    let js = [
        other_ran("jsNodeTest", "jsNodeTest.p.other.OtherTest"),
        other_ran("jsNodeTest", "jsNodeTest.p.other.MoreTest"),
    ];
    let jvm = [
        other_ran("jvmTest", "p.other.OtherTest"),
        other_ran("jvmTest", "p.other.MoreTest"),
    ];
    // A class left out is left out of both tasks, as one unit.
    assert_eq!(js, jvm, "{}", third.stdout);
    assert!(
        js.iter().filter(|ran| **ran).count() <= 1,
        "{}",
        third.stdout
    );
    // \[C-37\] That `hushrun why` knows a class by its file is end to end: e2e_gradle.rs.
}

// covers: C-94, C-95, C-96
#[test]
fn kotlin_native_test_classes_are_units_and_their_tests_are_reused_one_by_one() {
    // Kotlin/Native's toolchain comes from download.jetbrains.com on first use, hundreds of
    // megabytes: CI's Linux job opts in with HUSHRUN_TEST_KOTLIN_NATIVE=1, and elsewhere it is
    // skipped by name, as Android's is.
    if std::env::var("HUSHRUN_TEST_KOTLIN_NATIVE").as_deref() != Ok("1")
        || !cfg!(all(target_os = "linux", target_arch = "x86_64"))
    {
        eprintln!(
            "note: skipping: Kotlin/Native runs with HUSHRUN_TEST_KOTLIN_NATIVE=1 on Linux x86-64 (ADR 0117)"
        );
        return;
    }
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&kmp_build(
        "{mode: reuse, unit: test}",
        "  linuxX64()\n",
        &["linuxX64"],
    ));
    let args = [
        gradle.as_str(),
        offline(),
        "--console=plain",
        "linuxX64Test",
        "--continue",
        "--configuration-cache",
    ];
    // Kotlin/Native keeps its toolchain outside the Gradle user home, where the online run that
    // fills that home leaves it too.
    let konan = std::env::var("HUSHRUN_TEST_KONAN_DATA_DIR")
        .ok()
        .or_else(|| gradle_user_home().map(|home| format!("{home}/hushrun-konan")))
        .unwrap_or_default();
    let konan = ("KONAN_DATA_DIR", konan.as_str());
    let first = run(&r, &args, &[("FAIL", "1"), konan]);
    assert_eq!(first.code, 1, "{}", first.said());
    let map = r.map();
    assert_eq!(
        map["units"][OTHER]["tests"],
        json!({"p.other.OtherTest.flips": "failed", "p.other.OtherTest.works": "passed"}),
        "{map:#}"
    );
    assert!(
        !first.said().contains("isn't a JVM Test task"),
        "{}",
        first.said()
    );
    let second = run(&r, &args, &[konan]);
    assert_eq!(second.code, 0, "{}", second.said());
    let ran = task_ran(&r, "linuxX64Test");
    assert!(ran.contains("p.other.OtherTest.flips"), "{ran:?}");
    assert!(ran.len() <= 2, "{ran:?}\n{}", second.stdout);
}

/// Kotest specs in two Kotlin projects: `a`'s reads `a`'s code, `b`'s two read only their own.
fn kotest_build(tests: &str) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "build/\n.gradle/\n.kotlin/\n"),
        file(
            "settings.gradle",
            &format!(
                "pluginManagement {{ repositories {{ gradlePluginPortal(); mavenCentral() }} }}\n{LINE}\nrootProject.name = 'kt'\ninclude 'a', 'b'\n"
            ),
        ),
        file(
            "build.gradle",
            "plugins { id 'org.jetbrains.kotlin.jvm' version '2.3.21' apply false }\nsubprojects {\n  apply plugin: 'org.jetbrains.kotlin.jvm'\n  repositories { mavenCentral() }\n  dependencies { testImplementation 'io.kotest:kotest-runner-junit5:5.9.1'; testRuntimeOnly 'org.junit.platform:junit-platform-launcher' }\n  test { useJUnitPlatform() }\n}\n",
        ),
        file(
            "a/src/main/kotlin/p/a/Price.kt",
            "package p.a\nobject Price { fun of(n: Int) = n * 2 }\n",
        ),
        file(
            "a/src/test/kotlin/p/a/PriceSpec.kt",
            "package p.a\nimport io.kotest.core.spec.style.FunSpec\nclass PriceSpec : FunSpec({ test(\"doubles\") { check(Price.of(3) == 6) } })\n",
        ),
    ];
    for name in ["One", "Two", "Three"] {
        files.push(file(
            &format!("b/src/test/kotlin/p/b/{name}Spec.kt"),
            &format!("package p.b\nimport io.kotest.core.spec.style.StringSpec\nclass {name}Spec : StringSpec({{\n  \"works\" {{ }}\n  \"also\" {{ }}\n}})\n"),
        ));
    }
    files
}

// covers: C-97
#[test]
fn kotest_specs_are_left_out_whole_when_what_they_read_is_unchanged() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    for tests in ["reuse", "{mode: reuse, unit: test}"] {
        let r = repo(&kotest_build(tests));
        let first = run(&r, &command(&gradle, &[]), &[]);
        assert_eq!(first.code, 0, "{tests}: {}", first.said());
        r.write(
            "a/src/main/kotlin/p/a/Price.kt",
            "package p.a\nobject Price { fun of(n: Int) = n + n }\n",
        );
        let second = run(&r, &command(&gradle, &[]), &[]);
        assert_eq!(second.code, 0, "{tests}: {}", second.said());
        // a's spec runs; b's are left out whole by Gradle's class exclusion, which Kotest's
        // engine honours, but one an audit may run.
        assert_eq!(
            xml_cases(&r.repo, "a", "p.a.PriceSpec"),
            ["doubles"],
            "{tests}"
        );
        let ran = ["One", "Two", "Three"]
            .iter()
            .filter(|n| !xml_cases(&r.repo, "b", &format!("p.b.{n}Spec")).is_empty())
            .count();
        assert!(
            ran <= 1,
            "{tests}: {ran} of b's specs ran\n{}",
            second.stdout
        );
        let said = second.line("unit: ");
        assert!(
            said.contains("already passed on the files"),
            "{tests}: {said}"
        );
    }
}
