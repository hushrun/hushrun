//! \[ADR 0087\] Gradle's adapter end to end: the `hushrun` binary, as a person or CI runs it,
//! for what only the CLI does with Gradle's test classes. Every other test of the adapter is
//! in-process, in `units_gradle.rs`.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: the run's lines as
//! the CLI says them and their order, its last line and exit code, the check's own result,
//! the audit's second start of COMMAND and its suspension, and `hushrun why`. The tools are
//! those of `units_gradle.rs`, and CI's fill of the Gradle user home covers these builds too.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/gradle.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use serde_json::json;

use cli::Cli as _;
use fixtures::*;

/// `hushrun run unit -- COMMAND`.
fn cli<'a>(command: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["run", "unit", "--"];
    args.extend_from_slice(command);
    args
}

// covers: C-30, C-32, C-34, C-37, C-46
#[test]
fn a_run_says_what_it_reuses_records_it_and_hushrun_why_tells_a_class_by_its_file() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&shop("reuse"));
    let first = r.run(&cli(&command(&gradle, &[])), &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.line("hushrun: unit: every"),
        "hushrun: unit: every test class runs: no run here has listed Gradle's test classes yet"
    );
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 5, "ran": 5, "reused": 0})
    );

    // lib changes: lib's and app's classes run, other's were reused but the one audited.
    r.write(
        "lib/src/main/java/shop/lib/Price.java",
        "package shop.lib;\npublic class Price { public static int of(int x) { return x + x; } }\n",
    );
    let changed = r.run(&cli(&command(&gradle, &[])), &[]);
    assert_eq!(changed.code, 0, "{}\n{}", changed.stdout, changed.stderr);
    assert_eq!(
        changed.line("hushrun: unit: "),
        "hushrun: unit: 3 of 5 test classes run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    let result = r.newest_result();
    let saved = result["units"]["savedMs"].clone();
    assert_eq!(
        result["units"],
        json!({"total": 5, "ran": 3, "reused": 2, "savedMs": saved})
    );
    // \[C-37\] `hushrun why` knows a class by the name the store keeps, the reused ones too.
    let why = r.run(
        &["why", "unit", "other/src/test/java/shop/other/BTest.java"],
        &[],
    );
    assert!(
        why.stdout.contains("BTest.java would be reused"),
        "{}",
        why.stdout
    );
}

// covers: C-32, C-46
#[test]
fn the_last_line_of_a_run_in_which_gradle_ran_no_test_counts_no_test() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&shop("reuse"));
    let first = r.run(&cli(&command(&gradle, &[])), &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // A file no test reads changes, and Gradle finds every test task up to date.
    r.write("README.md", "A shop, again.\n");
    let idle = r.run(&cli(&command(&gradle, &[])), &[]);
    assert_eq!(idle.code, 0, "{}\n{}", idle.stdout, idle.stderr);
    // The adapter's note, once; the last line says no count of tests, since none ran, and the
    // reports the tasks left aren't this run's.
    assert_eq!(
        idle.stderr
            .lines()
            .filter(|l| l.starts_with("hushrun: unit: Gradle ran no test: "))
            .count(),
        1,
        "{}",
        idle.stderr
    );
    let last = idle.line("hushrun: unit passed in ");
    assert!(!last.contains("test"), "{last}");
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 5, "ran": 0, "reused": 0})
    );
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
    let r = repo(&files);
    let cc = ["--configuration-cache"];
    let first = r.run(
        &cli(&command(&gradle, &cc)),
        &[("FAIL", "1"), ("COUNTS", &count_dir)],
    );
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    // ok passed on these files, and is the one test that could be reused: it runs as a check, and
    // fails. It runs once more, alone, from the second run's own list, and fails again.
    let caught = r.run(
        &cli(&command(&gradle, &cc)),
        &[("HIDDEN", "1"), ("COUNTS", &count_dir)],
    );
    assert_eq!(caught.code, 1, "{}\n{}", caught.stdout, caught.stderr);
    assert_eq!(
        caught.line("hushrun: unit: all"),
        "hushrun: unit: all 2 tests run; 1 that already passed on the files it read runs anyway, as a check"
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
    assert_eq!(r.disagreements(), [name]);
}

// covers: C-37, C-39, C-46
#[test]
fn hushrun_why_never_says_a_junit4_suite_class_would_be_reused() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&suites("{mode: reuse, unit: test}"));
    let first = r.run(&cli(&command(&gradle, &[])), &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stderr, first.stdout);
    // A change in `other`: core's plain classes are reused, the suite class runs.
    r.write(
        "other/src/test/java/shop/other/CalcTest.java",
        "package shop.other;\nimport org.junit.*;\npublic class CalcTest { @Test public void adds() { Assert.assertEquals(3, 2 + 1); } }\n",
    );
    let second = r.run(&cli(&command(&gradle, &[])), &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stderr, second.stdout);
    let why = r.run(&["why", "unit", G_SUITE], &[]);
    assert!(!why.stdout.contains("would be reused"), "{}", why.stdout);
    assert!(why.stdout.contains("can't leave it out"), "{}", why.stdout);
}

// covers: C-37, C-94
#[test]
fn hushrun_why_knows_a_kotlin_js_class_by_its_file() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let gradle = gradle();
    let r = repo(&kmp_build("{mode: reuse, unit: test}", "", &[]));
    // Kotlin/JS installs its npm packages with yarn, `--offline` under Gradle's, from the cache
    // beside the Gradle user home (`units_gradle.rs`).
    let yarn = gradle_user_home()
        .map(|home| format!("{home}/hushrun-yarn-cache"))
        .unwrap_or_default();
    let first = r.run(
        &cli(&[
            gradle.as_str(),
            offline(),
            "--console=plain",
            "jvmTest",
            "jsNodeTest",
            "--continue",
            "--configuration-cache",
        ]),
        &[("FAIL", "1"), ("YARN_CACHE_FOLDER", yarn.as_str())],
    );
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    // MoreTest passed on both platforms, and nothing it read changed.
    let why = r.run(&["why", "unit", MORE], &[]);
    assert!(
        why.stdout.contains("MoreTest.kt would be reused"),
        "{}",
        why.stdout
    );
}

// covers: C-46
#[cfg(unix)]
#[test]
fn a_settings_file_without_the_line_is_said_before_command_runs_as_it_is() {
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
    let ran = r.run(&cli(&["./gradlew", "test"]), &[]);
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
