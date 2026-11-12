use super::*;

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_string).collect()
}

fn tree(paths: &[&str]) -> Tree {
    let mut bytes = format!("{}\n", hushrun_core::CANONICALIZATION).into_bytes();
    let mut sorted: Vec<&str> = paths.to_vec();
    sorted.sort();
    for (n, path) in sorted.iter().enumerate() {
        bytes.extend_from_slice(format!("f {:064x} {path}", n + 1).as_bytes());
        bytes.push(0);
    }
    Tree::from_manifest(&bytes).expect("a manifest")
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// An invocation as the script writes it.
fn inv(class: &str, name: &str, parents: &[(&str, &str)], result: &str) -> Invocation {
    Invocation {
        c: Some(class.to_string()),
        n: name.to_string(),
        p: parents
            .iter()
            .map(|(n, c)| (n.to_string(), c.to_string()))
            .collect(),
        r: result.to_string(),
        ms: 1,
    }
}

// covers: C-46
#[test]
fn gradle_is_recognized_by_its_program_and_continuous_builds_are_refused() {
    for command in [
        "gradle test",
        "./gradlew test --continue",
        "gradlew.bat test",
        "/opt/gradle/bin/gradle :app:test --tests ex.CalcTest",
        "./gradlew -p sub build",
        "gradle.bat check --configuration-cache",
    ] {
        assert!(
            matches!(recognize(&words(command)), Some(Ok(_))),
            "{command}"
        );
    }
    let r = recognize(&words("./gradlew -p sub --settings-file=s.gradle test"))
        .unwrap()
        .unwrap();
    assert_eq!(r.project_dir.as_deref(), Some("sub"));
    assert_eq!(r.settings.as_deref(), Some("s.gradle"));
    for refused in ["gradle -t test", "./gradlew test --continuous"] {
        assert!(
            matches!(recognize(&words(refused)), Some(Err(_))),
            "{refused}"
        );
    }
    // A script that hides Gradle isn't Gradle: the default adapter, which does nothing for it.
    for other in [
        "make test",
        "./ci.sh",
        "npm test",
        "mvn test",
        "sh gradlew test",
    ] {
        assert!(recognize(&words(other)).is_none(), "{other}");
    }
}

// covers: C-46
#[test]
fn a_settings_file_without_the_line_loads_nothing_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let r = recognize(&words("./gradlew test")).unwrap().unwrap();
    assert!(
        unloadable(&r, root)
            .unwrap()
            .contains("and there is none here")
    );
    std::fs::write(root.join("settings.gradle.kts"), "include(\"app\")\n").unwrap();
    let why = unloadable(&r, root).unwrap();
    assert!(
        why.contains("one line in settings.gradle.kts") && why.contains(KOTLIN_LINE),
        "{why}"
    );
    // From a project's directory, Gradle reads the build's settings file above it.
    std::fs::create_dir_all(root.join("app")).unwrap();
    assert!(unloadable(&r, &root.join("app")).is_some());
    std::fs::write(
        root.join("settings.gradle.kts"),
        format!("include(\"app\")\n{KOTLIN_LINE}\n"),
    )
    .unwrap();
    assert!(unloadable(&r, root).is_none());
    assert!(unloadable(&r, &root.join("app")).is_none());
    // Groovy's file gets Groovy's line.
    let groovy = tempfile::tempdir().unwrap();
    std::fs::write(groovy.path().join("settings.gradle"), "include 'app'\n").unwrap();
    assert!(unloadable(&r, groovy.path()).unwrap().contains(GROOVY_LINE));
}

// covers: C-15, C-46
#[test]
fn init_proposes_the_settings_line_for_a_gradle_build() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let checks = vec!["unit".to_string()];
    assert_eq!(proposal(root, &checks), None);
    std::fs::write(root.join("build.gradle"), "plugins { id 'java' }\n").unwrap();
    assert!(
        proposal(root, &checks)
            .unwrap()
            .starts_with("settings.gradle: write `if (System.getenv")
    );
    std::fs::write(root.join("settings.gradle"), "rootProject.name = 'x'\n").unwrap();
    let line = proposal(root, &checks).unwrap();
    assert!(
        line.starts_with(&format!("settings.gradle: add `{GROOVY_LINE}` after any")),
        "{line}"
    );
    assert_eq!(proposal(root, &[]), None);
    std::fs::write(
        root.join("settings.gradle"),
        format!("rootProject.name = 'x'\n{GROOVY_LINE}\n"),
    )
    .unwrap();
    assert_eq!(proposal(root, &checks), None);
}

// covers: C-46
#[test]
fn a_tests_id_is_its_class_and_method_for_every_invocation() {
    let c = "ex.app.CalcTest";
    let class = [(c, c)];
    let cases: Vec<(Invocation, &str)> = vec![
        // JUnit 5: plain, overloads, parametrized, repeated, factory, nested, Kotlin backticks.
        (
            inv(c, "passes()", &class, "SUCCESS"),
            "ex.app.CalcTest.passes",
        ),
        (
            inv(c, "overloaded(TestInfo)", &class, "SUCCESS"),
            "ex.app.CalcTest.overloaded",
        ),
        (
            inv(c, "param(int)[3]", &[("param(int)", c), (c, c)], "FAILURE"),
            "ex.app.CalcTest.param",
        ),
        (
            inv(c, "repeated()[1]", &[("repeated()", c), (c, c)], "SUCCESS"),
            "ex.app.CalcTest.repeated",
        ),
        (
            inv(
                "ex.app.CalcTest$Inner",
                "innerFails()",
                &[("ex.app.CalcTest$Inner", "ex.app.CalcTest$Inner"), (c, c)],
                "FAILURE",
            ),
            "ex.app.CalcTest$Inner.innerFails",
        ),
        (
            inv(c, "name with spaces()", &class, "SUCCESS"),
            "ex.app.CalcTest.name with spaces",
        ),
        (
            inv(c, "takes args(String, int[])", &class, "SUCCESS"),
            "ex.app.CalcTest.takes args",
        ),
        // JUnit 4: Parameterized under its `[0: x=1]` suite, on Gradle 9 and on AGP 8.13 alike.
        (
            inv(
                "ex.j4.ParamTest",
                "small[0: x=1]",
                &[
                    ("[0: x=1]", "ex.j4.ParamTest"),
                    ("ex.j4.ParamTest", "ex.j4.ParamTest"),
                ],
                "SUCCESS",
            ),
            "ex.j4.ParamTest.small",
        ),
        (
            inv("ex.j4.ParamTest", "small[1: x=2]", &[], "SUCCESS"),
            "ex.j4.ParamTest.small",
        ),
        // TestNG's data provider.
        (
            inv("ex.tng.CalcTest", "param[0](1)", &[], "SUCCESS"),
            "ex.tng.CalcTest.param",
        ),
        // Spock: an unrolled feature is its feature.
        (
            inv(
                "ex.spock.CalcSpec",
                "param 1",
                &[
                    ("param #x", "ex.spock.CalcSpec"),
                    ("ex.spock.CalcSpec", "ex.spock.CalcSpec"),
                ],
                "SUCCESS",
            ),
            "ex.spock.CalcSpec.param #x",
        ),
    ];
    for (invocation, id) in cases {
        assert_eq!(test_id(&invocation).as_deref(), Some(id), "{invocation:?}");
    }
    assert_eq!(
        test_id(&Invocation {
            c: None,
            n: "x".into(),
            ..Invocation::default()
        }),
        None
    );
}

// covers: C-30, C-46
#[test]
fn sources_are_jvm_files_below_the_projects_test_classes_read_and_not_build_logic() {
    let t = tree(&[
        "settings.gradle",
        "build.gradle",
        "app/build.gradle",
        "app/src/main/java/ex/App.java",
        "app/src/test/java/ex/AppTest.java",
        "app/src/test/resources/data.json",
        "lib/src/main/kotlin/ex/Lib.kt",
        "other/src/main/java/ex/Other.java",
        "buildSrc/src/main/groovy/Conv.groovy",
        "build-logic/settings.gradle",
        "build-logic/src/main/kotlin/Conv.kt",
        "shared-lib/settings.gradle",
        "shared-lib/greeter/src/main/java/Greeter.java",
        "tools/Script.java",
    ]);
    let got = sources_below(&t, &set(&["app", "lib", "shared-lib/greeter"]));
    assert_eq!(
        got.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "app/src/main/java/ex/App.java",
            "app/src/test/java/ex/AppTest.java",
            "lib/src/main/kotlin/ex/Lib.kt",
            "shared-lib/greeter/src/main/java/Greeter.java",
        ]
    );
    // The root project's directory holds every project, but not build logic.
    let got = sources_below(&t, &set(&["."]));
    assert!(got.contains("tools/Script.java") && got.contains("other/src/main/java/ex/Other.java"));
    assert!(!got.contains("buildSrc/src/main/groovy/Conv.groovy"));
    assert!(!got.contains("build-logic/src/main/kotlin/Conv.kt"));
    assert!(!got.contains("shared-lib/greeter/src/main/java/Greeter.java"));
    let sets = classify(
        &t,
        &set(&["app/src/test/java/ex/AppTest.java"]),
        &set(&["app", "lib"]),
        &[],
    );
    assert_eq!(
        sets.of("app/src/test/java/ex/AppTest.java"),
        Some(units::Set::Units)
    );
    assert_eq!(
        sets.of("lib/src/main/kotlin/ex/Lib.kt"),
        Some(units::Set::Sources)
    );
    // A file of a project no test class read, resources and build scripts run everything.
    for rest in [
        "other/src/main/java/ex/Other.java",
        "app/src/test/resources/data.json",
        "app/build.gradle",
        "settings.gradle",
        "buildSrc/src/main/groovy/Conv.groovy",
    ] {
        assert_eq!(sets.of(rest), Some(units::Set::Rest), "{rest}");
    }
}

/// A task's facts file, as the script wrote it in a run of the spike's sample builds.
fn captured(text: &str) -> FactsFile {
    serde_json::from_str(text).expect("a facts file")
}

/// `file` with its report changed by `change`.
fn changed(file: &FactsFile, change: impl Fn(&mut serde_json::Value)) -> FactsFile {
    let mut file = file.clone();
    change(file.settings.get_mut(ADAPTER).expect("a report"));
    file
}

fn state(seen: &crate::units::Seen, planned: bool) -> units::State {
    units::state(
        &seen.facts,
        &units::RunFacts {
            command_passed: false,
            failures_by_unit: true,
            planned,
        },
    )
}

fn test_states(seen: &crate::units::Seen, planned: &[&str]) -> BTreeMap<String, units::State> {
    seen.tests
        .as_ref()
        .expect("tests")
        .iter()
        .map(|(id, t)| {
            let facts = Facts {
                complete: t.finished,
                failed: t.failed,
                deselected: t.deselected,
                ..Facts::default()
            };
            let run = units::RunFacts {
                command_passed: false,
                failures_by_unit: true,
                planned: planned.contains(&id.as_str()),
            };
            (id.clone(), units::state(&facts, &run))
        })
        .collect()
}

const JUNIT5_APP: &str = include_str!("junit5-left-out-app.json");
const JUNIT5_LIB: &str = include_str!("junit5-left-out-lib.json");

fn junit5_tree() -> Tree {
    tree(&[
        "settings.gradle",
        "buildSrc/src/main/groovy/ex.java-conventions.gradle",
        "app/build.gradle",
        "app/src/main/java/ex/app/App.java",
        "app/src/test/java/ex/app/BeforeAllFailTest.java",
        "app/src/test/java/ex/app/CalcTest.java",
        "app/src/test/java/ex/app/Extras.java",
        "app/src/test/java/ex/app/StaticInitFailTest.java",
        "lib/build.gradle",
        "lib/src/main/java/ex/lib/Lib.java",
        "lib/src/test/java/ex/lib/LibTest.java",
        "other/src/test/java/ex/other/OtherTest.java",
    ])
}

// covers: C-46, C-39, C-32
#[test]
fn a_junit5_task_reports_classes_and_tests_left_out_and_class_failures() {
    let t = junit5_tree();
    let observed = observe(&[captured(JUNIT5_APP), captured(JUNIT5_LIB)], &t)
        .unwrap()
        .unwrap();
    let units = &observed.units;
    assert_eq!(
        units.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "app/src/test/java/ex/app/BeforeAllFailTest.java",
            "app/src/test/java/ex/app/CalcTest.java",
            "app/src/test/java/ex/app/Extras.java",
            "app/src/test/java/ex/app/StaticInitFailTest.java",
            "lib/src/test/java/ex/lib/LibTest.java",
        ]
    );
    // Two classes of one file are one unit, left out whole; a task with every class left out
    // had no source, and its class was left out all the same.
    let extras = &units["app/src/test/java/ex/app/Extras.java"];
    assert_eq!(state(extras, true), units::State::Reused);
    assert_eq!(state(extras, false), units::State::NotRun);
    let lib = &units["lib/src/test/java/ex/lib/LibTest.java"];
    assert_eq!(state(lib, true), units::State::Reused);
    // A failing @BeforeAll fails its class, and no test of it is told.
    let before = &units["app/src/test/java/ex/app/BeforeAllFailTest.java"];
    assert_eq!(state(before, false), units::State::Failed);
    assert!(before.tests.as_ref().unwrap().is_empty());
    // A static initializer that failed fails each test.
    let init = &units["app/src/test/java/ex/app/StaticInitFailTest.java"];
    assert_eq!(state(init, false), units::State::Failed);
    // The tests left out one by one through the hook, a nested class's included, are reused; the
    // others ran, every invocation one test, and the class passed what ran of it.
    let calc = &units["app/src/test/java/ex/app/CalcTest.java"];
    let left = [
        "ex.app.CalcTest$Inner.innerFails",
        "ex.app.CalcTest.dynamic",
        "ex.app.CalcTest.fails",
        "ex.app.CalcTest.param",
    ];
    let states = test_states(calc, &left);
    for id in left {
        assert_eq!(states[id], units::State::Reused, "{id}");
    }
    for id in [
        "ex.app.CalcTest$Inner$Deeper.deep",
        "ex.app.CalcTest$Inner.innerPasses",
        "ex.app.CalcTest.overloaded",
        "ex.app.CalcTest.paramNamed",
        "ex.app.CalcTest.repeated",
    ] {
        assert_eq!(states[id], units::State::Passed, "{id}");
    }
    // \[C-32\] Skipped in a run where other classes failed: told skipped, and not finished, so
    // the class didn't run whole.
    let tests = calc.tests.as_ref().unwrap();
    for id in [
        "ex.app.CalcTest.abortedByAssumption",
        "ex.app.CalcTest.disabled",
    ] {
        assert_eq!(states[id], units::State::NotRun, "{id}");
        assert!(tests[id].skipped, "{id}");
    }
    assert!(!tests["ex.app.CalcTest.repeated"].skipped);
    assert!(!calc.facts.failed && !calc.facts.complete && calc.facts.skipped);
    // What a class read: every file below its project's directory and below each project its
    // classpath is built from, build logic and other projects left out.
    assert!(calc.reads.contains("lib/src/main/java/ex/lib/Lib.java"));
    assert!(
        calc.reads
            .contains("app/src/test/java/ex/app/CalcTest.java")
    );
    assert!(!calc.reads.iter().any(|r| r.starts_with("other/")));
    assert_eq!(
        observed.dirs["app/src/test/java/ex/app/CalcTest.java"],
        set(&["app", "lib"])
    );
    assert_eq!(
        observed.dirs["lib/src/test/java/ex/lib/LibTest.java"],
        set(&["lib"])
    );
    assert_eq!(observed.known_dirs, set(&["app", "lib"]));
    assert!(observed.own.is_empty());
    // A project no test class read is in the rest set, and so is build logic.
    let sets = classify(&t, &observed.unit_files, &observed.known_dirs, &[]);
    assert_eq!(
        sets.of("other/src/test/java/ex/other/OtherTest.java"),
        Some(units::Set::Rest)
    );
    assert_eq!(
        sets.of("lib/src/main/java/ex/lib/Lib.java"),
        Some(units::Set::Sources)
    );
}

// covers: C-46, C-39
#[test]
fn testng_kotest_spock_and_junit4_tests_record_only_what_they_earned() {
    // TestNG reports a failing @BeforeClass as a failed test named after the method, and skips
    // the class's tests: those didn't finish. A test skipped by an exception in a class with a
    // failure records nothing either.
    let t = tree(&[
        "tng/src/test/java/ex/tng/BeforeClassFailTest.java",
        "tng/src/test/java/ex/tng/CalcTest.java",
    ]);
    let observed = observe(&[captured(include_str!("testng.json"))], &t)
        .unwrap()
        .unwrap();
    let before = &observed.units["tng/src/test/java/ex/tng/BeforeClassFailTest.java"];
    assert_eq!(state(before, false), units::State::Failed);
    let states = test_states(before, &[]);
    assert_eq!(
        states["ex.tng.BeforeClassFailTest.neverRuns"],
        units::State::NotRun
    );
    assert_eq!(
        states["ex.tng.BeforeClassFailTest.boom"],
        units::State::Failed
    );
    let calc = test_states(
        &observed.units["tng/src/test/java/ex/tng/CalcTest.java"],
        &[],
    );
    assert_eq!(
        calc["ex.tng.CalcTest.skippedByException"],
        units::State::NotRun
    );
    assert_eq!(calc["ex.tng.CalcTest.param"], units::State::Failed);
    assert_eq!(calc["ex.tng.CalcTest.passes"], units::State::Passed);
    assert_eq!(
        calc["ex.tng.CalcTest$Nested.nestedPasses"],
        units::State::Passed
    );

    // Kotest: a failed beforeSpec fails its spec; a spec's tests are what Gradle names them.
    let t = tree(&["kotest/src/test/kotlin/ex/kotest/KoSpec.kt"]);
    let observed = observe(&[captured(include_str!("kotest.json"))], &t)
        .unwrap()
        .unwrap();
    let spec = &observed.units["kotest/src/test/kotlin/ex/kotest/KoSpec.kt"];
    assert_eq!(state(spec, false), units::State::Failed);
    let states = test_states(spec, &[]);
    assert_eq!(states["ex.kotest.KoFunSpec.inside"], units::State::Passed);
    assert_eq!(states["ex.kotest.KoFunSpec.3"], units::State::Failed);

    // Spock: a failing setupSpec fails its spec; an unrolled feature is one test.
    let t = tree(&[
        "spock/src/test/groovy/ex/spock/CalcSpec.groovy",
        "spock/src/test/groovy/ex/spock/SetupSpecFailSpec.groovy",
    ]);
    let observed = observe(&[captured(include_str!("spock.json"))], &t)
        .unwrap()
        .unwrap();
    assert_eq!(
        state(
            &observed.units["spock/src/test/groovy/ex/spock/SetupSpecFailSpec.groovy"],
            false
        ),
        units::State::Failed
    );
    let states = test_states(
        &observed.units["spock/src/test/groovy/ex/spock/CalcSpec.groovy"],
        &[],
    );
    assert_eq!(states["ex.spock.CalcSpec.param #x"], units::State::Failed);
    assert_eq!(
        states["ex.spock.CalcSpec$Inner.inner works"],
        units::State::Passed
    );

    // JUnit 4 through Gradle's filter patterns: a nested class's test and a Parameterized test
    // left out; @BeforeClass and a class JUnit 4 refuses fail their class. The same on Gradle 8
    // and 9, whose Parameterized suites differ.
    let t = tree(&[
        "app/src/test/java/ex/app/BeforeClassFailTest.java",
        "app/src/test/java/ex/app/CalcTest.java",
        "app/src/test/java/ex/app/EnclosedTest.java",
        "app/src/test/java/ex/app/Extras.java",
        "app/src/test/java/ex/app/ParamTest.java",
        "libs/local.jar",
    ]);
    let observed = observe(&[captured(include_str!("junit4-patterns.json"))], &t)
        .unwrap()
        .unwrap();
    let left = [
        "ex.app.CalcTest$Nested.nestedFails",
        "ex.app.CalcTest.fails",
        "ex.app.ParamTest.small",
    ];
    let calc = test_states(
        &observed.units["app/src/test/java/ex/app/CalcTest.java"],
        &left,
    );
    assert_eq!(calc["ex.app.CalcTest.fails"], units::State::Reused);
    assert_eq!(
        calc["ex.app.CalcTest$Nested.nestedFails"],
        units::State::Reused
    );
    assert_eq!(calc["ex.app.CalcTest.passes"], units::State::Passed);
    let param = test_states(
        &observed.units["app/src/test/java/ex/app/ParamTest.java"],
        &left,
    );
    assert_eq!(param["ex.app.ParamTest.small"], units::State::Reused);
    assert_eq!(param["ex.app.ParamTest.positive"], units::State::Passed);
    for failed in [
        "app/src/test/java/ex/app/BeforeClassFailTest.java",
        "app/src/test/java/ex/app/Extras.java",
    ] {
        assert_eq!(
            state(&observed.units[failed], false),
            units::State::Failed,
            "{failed}"
        );
    }
    // A jar of the repository on the classpath is read; it is in the rest set.
    assert!(
        observed.units["app/src/test/java/ex/app/CalcTest.java"]
            .reads
            .contains("libs/local.jar")
    );
    for (text, version) in [
        (include_str!("junit4-gradle8.json"), 8),
        (include_str!("junit4-gradle9.json"), 9),
    ] {
        let observed = observe(&[captured(text)], &t).unwrap().unwrap();
        let param = test_states(
            &observed.units["app/src/test/java/ex/app/ParamTest.java"],
            &[],
        );
        assert_eq!(
            param.keys().map(String::as_str).collect::<Vec<_>>(),
            ["ex.app.ParamTest.positive", "ex.app.ParamTest.small"],
            "Gradle {version}"
        );
        assert_eq!(param["ex.app.ParamTest.small"], units::State::Failed);
    }
}

// covers: C-46, C-39
#[test]
fn a_flaky_test_is_a_failed_one_and_a_kotlin_class_is_named_by_its_file() {
    // The test-retry plugin's retry that passed: the first run's fail stands (ADR 0069).
    let t = tree(&["src/test/java/ex/retry/FlakyTest.java"]);
    let observed = observe(&[captured(include_str!("retry.json"))], &t)
        .unwrap()
        .unwrap();
    let flaky = &observed.units["src/test/java/ex/retry/FlakyTest.java"];
    assert_eq!(
        test_states(flaky, &[])["ex.retry.FlakyTest.flaky"],
        units::State::Failed
    );
    assert_eq!(
        observed.dirs["src/test/java/ex/retry/FlakyTest.java"],
        set(&["."])
    );
    // Kotlin: two classes in Things.kt, a class whose package isn't its directory, backticks.
    let t = tree(&[
        "app/src/test/kotlin/ex/app/BeforeAllFail.kt",
        "app/src/test/kotlin/ex/app/CalcTest.kt",
        "app/src/test/kotlin/ex/app/Things.kt",
        "app/src/test/kotlin/flat/Misplaced.kt",
    ]);
    let observed = observe(&[captured(include_str!("kotlin-junit5.json"))], &t)
        .unwrap()
        .unwrap();
    assert!(
        observed
            .units
            .contains_key("app/src/test/kotlin/ex/app/Things.kt")
    );
    assert!(
        observed
            .units
            .contains_key("app/src/test/kotlin/flat/Misplaced.kt")
    );
    let calc = test_states(
        &observed.units["app/src/test/kotlin/ex/app/CalcTest.kt"],
        &[],
    );
    assert_eq!(
        calc["ex.app.CalcTest.name with spaces"],
        units::State::Passed
    );
}

// covers: C-46, C-32
#[test]
fn a_task_gradle_didnt_run_whole_records_nothing() {
    let t = junit5_tree();
    // Up to date: Gradle's kept results, not a run.
    let up = observe(&[captured(include_str!("up-to-date.json"))], &t)
        .unwrap()
        .unwrap();
    let lib = &up.units["lib/src/test/java/ex/lib/LibTest.java"];
    assert_eq!(state(lib, true), units::State::NotRun);
    assert_eq!(state(lib, false), units::State::NotRun);
    assert!(lib.tests.is_none());
    // Its directories still count for where the sources are.
    assert_eq!(up.known_dirs, set(&["lib"]));
    // A task that ended before its root suite did (a test JVM that died): a test that failed
    // failed, and nothing passed.
    let app = captured(JUNIT5_APP);
    let crashed = changed(&app, |r| {
        r["rootDone"] = false.into();
        r["taskFailed"] = true.into();
    });
    let observed = observe(&[crashed], &t).unwrap().unwrap();
    let calc = &observed.units["app/src/test/java/ex/app/CalcTest.java"];
    assert_eq!(state(calc, false), units::State::NotRun);
    assert!(
        test_states(calc, &[])
            .values()
            .all(|s| *s == units::State::NotRun)
    );
    assert_eq!(
        state(
            &observed.units["app/src/test/java/ex/app/StaticInitFailTest.java"],
            false
        ),
        units::State::Failed
    );
    // A failure outside every class: nothing of the task ran whole.
    let outside = changed(&app, |r| {
        r["suiteFailures"] = serde_json::json!([{"n": "Gradle Test Executor 1", "c": null}]);
    });
    let observed = observe(&[outside], &t).unwrap().unwrap();
    assert_eq!(
        state(
            &observed.units["app/src/test/java/ex/app/CalcTest.java"],
            false
        ),
        units::State::NotRun
    );
    // A test the plan listed that ran all the same ran: never reused.
    let ran_anyway = changed(&app, |r| {
        r["invocations"].as_array_mut().unwrap().push(
            serde_json::json!({"c": "ex.app.CalcTest", "n": "fails()", "r": "SUCCESS", "ms": 3,
                                     "p": [["ex.app.CalcTest", "ex.app.CalcTest"]]}),
        );
    });
    let observed = observe(&[ran_anyway], &t).unwrap().unwrap();
    let states = test_states(
        &observed.units["app/src/test/java/ex/app/CalcTest.java"],
        &["ex.app.CalcTest.fails"],
    );
    assert_eq!(states["ex.app.CalcTest.fails"], units::State::Passed);
}

// covers: C-46, C-32
#[test]
fn what_the_script_couldnt_follow_makes_its_task_not_observed() {
    let t = junit5_tree();
    // Without the skipped tests, which other classes' failures keep from finishing (C-32).
    let app = changed(&captured(JUNIT5_APP), |r| {
        r["invocations"]
            .as_array_mut()
            .unwrap()
            .retain(|inv| inv["r"] != "SKIPPED");
    });
    // A test framework the script doesn't know; the script's own error.
    let unknown = changed(&app, |r| {
        r["notFollowed"] = serde_json::json!([{"why": "test-framework", "detail": "x"}]);
    });
    let mut broken = app.clone();
    broken.errors.push("selection: boom".into());
    for file in [unknown, broken] {
        let observed = observe(&[file], &t).unwrap().unwrap();
        assert_eq!(
            state(
                &observed.units["app/src/test/java/ex/app/CalcTest.java"],
                false
            ),
            units::State::NotObserved
        );
        assert!(
            !observed.units["app/src/test/java/ex/app/CalcTest.java"]
                .facts
                .not_followed
                .is_empty()
        );
    }
    // A classpath entry of the repository that is no file of it (built output outside every
    // project's build directory), a directory of it, and a name no source file gives.
    let odd = changed(&app, |r| {
        r["reads"]["paths"] = serde_json::json!(["out/classes", "other"]);
        r["names"]["ex.app.CalcTest"] = "app::ex.app.CalcTest".into();
    });
    let observed = observe(&[odd], &t).unwrap().unwrap();
    let calc = &observed.units["app::ex.app.CalcTest"];
    assert!(
        calc.facts
            .not_followed
            .iter()
            .any(|n| n.why == "classpath" && n.detail == "out/classes")
    );
    assert!(observed.dirs["app::ex.app.CalcTest"].contains("other"));
    assert_eq!(observed.own["app::ex.app.CalcTest"], Vec::<String>::new());
    assert!(!observed.unit_files.contains("app::ex.app.CalcTest"));
    // A test task the script doesn't follow reports no unit; no file of the adapter's, nothing.
    let not_followed: FactsFile = serde_json::from_value(serde_json::json!({
        "schema": "hushrun.unit-facts/v1", "adapter": "gradle", "units": [],
        "settings": {"gradle": {"schema": REPORT_SCHEMA, "task": ":inc:test", "outcome": "not-followed",
                                "notFollowed": [{"why": "task", "detail": "included build"}]}}
    }))
    .unwrap();
    assert!(
        observe(&[not_followed], &t)
            .unwrap()
            .unwrap()
            .units
            .is_empty()
    );
    assert!(observe(&[], &t).unwrap().is_none());
    // A report that can't be read: nothing can be told.
    let mut unread = app.clone();
    unread
        .settings
        .insert(ADAPTER.into(), serde_json::json!({"schema": "other/v9"}));
    assert!(observe(&[unread], &t).is_err());
}

// covers: C-46, C-39
#[test]
fn a_class_two_tasks_ran_passed_only_where_each_ran_it_whole() {
    // Android's testDebugUnitTest and testReleaseUnitTest run the same class: a test finished
    // only when both finished it, and left out only when both left it out.
    let t = junit5_tree();
    let app = captured(JUNIT5_APP);
    let release = changed(&app, |r| {
        r["task"] = ":app:testRelease".into();
        r["rootDone"] = false.into();
        r["taskFailed"] = true.into();
    });
    let observed = observe(&[app.clone(), release], &t).unwrap().unwrap();
    let calc = &observed.units["app/src/test/java/ex/app/CalcTest.java"];
    assert_eq!(state(calc, false), units::State::NotRun);
    let states = test_states(calc, &["ex.app.CalcTest.fails"]);
    assert_eq!(states["ex.app.CalcTest.passes"], units::State::NotRun);
    assert_eq!(states["ex.app.CalcTest.fails"], units::State::NotRun);
    let both = observe(
        &[app.clone(), changed(&app, |r| r["task"] = ":b".into())],
        &t,
    )
    .unwrap()
    .unwrap();
    let states = test_states(
        &both.units["app/src/test/java/ex/app/CalcTest.java"],
        &["ex.app.CalcTest.fails"],
    );
    assert_eq!(states["ex.app.CalcTest.fails"], units::State::Reused);
    assert_eq!(states["ex.app.CalcTest.passes"], units::State::Passed);
}

// covers: C-46, C-38
#[test]
fn a_plan_knows_the_last_runs_test_classes_and_the_directories_they_read() {
    use crate::units::store::{Map, Mapped};
    let t = junit5_tree();
    let mut map = Map::default();
    let empty = Shared::default();
    let (files, unnamed, dirs) = known(&map, &empty, &t);
    assert!(files.is_empty() && unnamed.is_empty() && dirs.is_empty());
    map.unit_files = set(&[
        "app/src/test/java/ex/app/CalcTest.java",
        "app/src/test/java/ex/app/Gone.java",
    ]);
    map.units.insert(
        "app/src/test/java/ex/app/CalcTest.java".into(),
        Mapped {
            dirs: set(&["app", "lib"]),
            ..Mapped::default()
        },
    );
    map.units.insert(
        "app::ex.app.Gen".into(),
        Mapped {
            dirs: set(&["app"]),
            ..Mapped::default()
        },
    );
    // A row no longer in the list (a class whose tests are all gone) says nothing.
    map.units.insert(
        "other/src/test/java/ex/other/OtherTest.java".into(),
        Mapped {
            dirs: set(&["other"]),
            ..Mapped::default()
        },
    );
    let (files, unnamed, dirs) = known(&map, &empty, &t);
    assert_eq!(files, set(&["app/src/test/java/ex/app/CalcTest.java"]));
    assert_eq!(unnamed, set(&["app::ex.app.Gen"]));
    assert_eq!(dirs, set(&["app", "lib"]));
}
