//! \[C-47\] The Maven adapter's own rules, on files Surefire, Failsafe and the test-JVM hook wrote
//! (`testdata/maven`, captured from real runs with machine paths and properties taken out).

use super::*;

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/maven");

fn data(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(DATA).join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_string).collect()
}

/// A class's facts from the cases of `files`' reports, as a run that wrote them reads them.
fn from_reports(files: &[&str], class: &str) -> Option<super::super::Seen> {
    let mut seen = ClassSeen::default();
    for name in files {
        for case in reports::cases(&data(name)).unwrap() {
            if case.classname.split('$').next().unwrap() == class {
                seen.add_case(&case);
            }
        }
    }
    decide_class(&seen, false)
}

fn from_events(name: &str, class: &str) -> Option<super::super::Seen> {
    let events = jvm::read_events(&data(name));
    decide_class(
        &ClassSeen {
            events: events.classes.get(class).cloned(),
            ..ClassSeen::default()
        },
        false,
    )
}

fn test<'a>(seen: &'a super::super::Seen, id: &str) -> &'a MergedTest {
    seen.tests
        .as_ref()
        .and_then(|t| t.get(id))
        .unwrap_or_else(|| panic!("no test {id} in {:?}", seen.tests))
}

// covers: C-47
#[test]
fn maven_and_its_wrappers_are_recognized_and_what_skips_tests_or_hides_the_extension_is_refused() {
    for command in [
        "mvn test",
        "./mvnw -B verify",
        "mvnw.cmd test",
        "C:\\tools\\mvn.cmd test",
        "/opt/maven/bin/mvn -pl app -am test",
        "mvn -DskipTests=false test",
        "mvn -Dtest=PriceTest test",
    ] {
        assert!(
            matches!(recognize(&words(command)), Some(Ok(_))),
            "{command}"
        );
    }
    for command in [
        "mvn -DskipTests install",
        "mvn -D skipTests install",
        "./mvnw -Dmaven.test.skip=true package",
        "mvn -Dmaven.ext.class.path=/x.jar test",
        "mvnd test",
    ] {
        assert!(
            matches!(recognize(&words(command)), Some(Err(_))),
            "{command}"
        );
    }
    for command in ["gradle test", "make test", "mvn-helper test"] {
        assert!(recognize(&words(command)).is_none(), "{command}");
    }
}

// covers: C-47
#[test]
fn maven_opts_keep_what_they_said_and_join_hushruns_jars_to_the_same_properties() {
    let ext = Path::new("/c/adapters/jvm-1/hushrun-maven-extension.jar");
    let hook = Path::new("/c/adapters/jvm-2/hushrun-test-jvm-hook.jar");
    let sep = if cfg!(windows) { ";" } else { ":" };
    assert_eq!(
        maven_opts(None, None, ext, hook).unwrap(),
        format!(
            "-Dmaven.ext.class.path={} -Dmaven.test.additionalClasspath={}",
            ext.display(),
            hook.display()
        )
    );
    assert_eq!(
        maven_opts(
            Some("-Xmx1g -Dmaven.ext.class.path=/theirs.jar"),
            Some("-Dmaven.test.additionalClasspath=/a.jar\n-Xss4m"),
            ext,
            hook
        )
        .unwrap(),
        format!(
            "-Xmx1g -Dmaven.ext.class.path=/theirs.jar -Dmaven.ext.class.path=/theirs.jar{sep}{} -Dmaven.test.additionalClasspath=/a.jar,{}",
            ext.display(),
            hook.display()
        )
    );
    assert!(
        maven_opts(None, None, Path::new("/My Cache/x.jar"), hook)
            .unwrap_err()
            .contains("has a space")
    );
}

// covers: C-47
#[test]
fn a_report_names_tests_by_method_and_nested_class_and_a_skipped_test_passes_only_in_a_class_without_failure()
 {
    let seen = from_reports(
        &["surefire-3.6.0-junit5-CalcTest.xml"],
        "com.example.lib.CalcTest",
    )
    .unwrap();
    let ids: Vec<&String> = seen.tests.as_ref().unwrap().keys().collect();
    assert_eq!(
        ids,
        [
            "Inner#innerPasses",
            "Inner$Deeper#deepPasses",
            "InnerTest#nestedNamedLikeATest",
            "assumed",
            "disabled",
            "displayNamed",
            "errors",
            "flaky",
            "over",
            "paramCustom",
            "passes"
        ]
    );
    // Its skipped tests didn't finish, so it didn't run whole; it failed all the same.
    assert!(seen.facts.failed && !seen.facts.complete);
    assert!(test(&seen, "errors").failed);
    assert!(test(&seen, "paramCustom").failed, "one invocation failed");
    assert!(test(&seen, "passes").finished && !test(&seen, "passes").failed);
    assert!(test(&seen, "Inner$Deeper#deepPasses").finished);
    // Disabled and assumed tests are skipped: a pass only in a class where nothing failed.
    assert!(!test(&seen, "disabled").finished);
    assert!(!test(&seen, "assumed").finished);
    assert!(test(&seen, "disabled").skipped && test(&seen, "assumed").skipped);
    assert!(!test(&seen, "passes").skipped && seen.facts.skipped);
}

// covers: C-32, C-47
#[test]
fn a_skipped_test_is_told_and_finishes_only_in_a_run_where_nothing_failed() {
    // TestNG skipped OrderTest's test because another class's @BeforeSuite failed: its own class
    // holds no failure.
    let report = br#"<testsuite name="TestSuite">
<testcase name="db" classname="ng.SetupTest"><failure message="no database"/></testcase>
<testcase name="total" classname="ng.OrderTest"><skipped message="no database"/></testcase>
<testcase name="green" classname="ng.GreenTest"/>
</testsuite>"#;
    let class = |name: &str| {
        let mut seen = ClassSeen::default();
        for case in reports::cases(report).unwrap() {
            if case.classname == name {
                seen.add_case(&case);
            }
        }
        seen
    };
    let order = class("ng.OrderTest");
    let failed_run = decide_class(&order, true).unwrap();
    assert!(!test(&failed_run, "total").finished && test(&failed_run, "total").skipped);
    assert!(!failed_run.facts.complete && failed_run.facts.skipped);
    assert!(class("ng.SetupTest").any_failed() && !order.any_failed());
    // Where nothing failed, a skip is a pass, and the class ran whole; it is still told skipped.
    let passed_run = decide_class(&order, false).unwrap();
    assert!(test(&passed_run, "total").finished && test(&passed_run, "total").skipped);
    assert!(passed_run.facts.complete && passed_run.facts.skipped);
    let green = decide_class(&class("ng.GreenTest"), true).unwrap();
    assert!(green.facts.complete && !green.facts.skipped);
    // The hook's events say the same of a skipped test and of a skipped class.
    let events = jvm::read_events(
        br#"{"e":"jvm","pid":"1","dir":"/w","basedir":"/w"}
{"e":"started","test":false,"uid":"[engine:junit-jupiter]/[class:p.A]","class":"p.A","method":null,"params":null}
{"e":"skipped","test":true,"uid":"[engine:junit-jupiter]/[class:p.A]/[method:off()]","class":"p.A","method":"off","params":"","reason":"disabled"}
{"e":"started","test":true,"uid":"[engine:junit-jupiter]/[class:p.A]/[method:on()]","class":"p.A","method":"on","params":""}
{"e":"finished","test":true,"uid":"[engine:junit-jupiter]/[class:p.A]/[method:on()]","class":"p.A","method":"on","params":"","status":"SUCCESSFUL","ms":1,"message":null}
{"e":"finished","test":false,"uid":"[engine:junit-jupiter]/[class:p.A]","class":"p.A","method":null,"params":null,"status":"SUCCESSFUL","ms":1,"message":null}
{"e":"skipped","test":false,"uid":"[engine:junit-jupiter]/[class:p.B]","class":"p.B","method":null,"params":null,"reason":"disabled"}
"#,
    );
    let of = |name: &str, run_failed: bool| {
        decide_class(
            &ClassSeen {
                events: events.classes.get(name).cloned(),
                ..ClassSeen::default()
            },
            run_failed,
        )
        .unwrap()
    };
    let a = of("p.A", true);
    assert!(test(&a, "off").skipped && !test(&a, "off").finished);
    assert!(test(&a, "on").finished && !a.facts.complete && a.facts.skipped);
    assert!(of("p.A", false).facts.complete);
    let b = of("p.B", false);
    assert!(b.facts.complete && b.facts.skipped);
    // A class JUnit 4 skipped whole, as Surefire's report says it: told skipped.
    let ignored = br#"<testsuite name="p.Ignored"><testcase name="p.Ignored" classname="p.Ignored"><skipped/></testcase></testsuite>"#;
    let mut seen = ClassSeen::default();
    for case in reports::cases(ignored).unwrap() {
        seen.add_case(&case);
    }
    let ignored = decide_class(&seen, true).unwrap();
    assert!(ignored.facts.skipped && !ignored.facts.failed);
}

// covers: C-47
#[test]
fn a_class_failure_in_a_report_fails_the_class_and_finishes_none_of_its_tests() {
    for (file, class) in [
        (
            "surefire-3.6.0-junit5-BeforeAllFailTest.xml",
            "com.example.lib.BeforeAllFailTest",
        ),
        ("junit4-BeforeClassFailTest.xml", "j4.BeforeClassFailTest"),
    ] {
        let seen = from_reports(&[file], class).unwrap();
        assert!(seen.facts.failed, "{file}");
        assert!(
            seen.tests
                .as_ref()
                .unwrap()
                .values()
                .all(|t| !t.finished || t.failed)
        );
    }
    // TestNG reports a failing @BeforeClass as a failed test named after it, and the class's
    // tests as skipped: those never ran, and never pass.
    let seen = from_reports(&["testng-TestSuite.xml"], "ng.BeforeClassFailTest").unwrap();
    assert!(seen.facts.failed);
    assert!(!test(&seen, "one").finished && !test(&seen, "two").finished);
    let green = from_reports(&["testng-TestSuite.xml"], "ng.GreenTest").unwrap();
    assert!(!green.facts.failed && test(&green, "green1").finished);
}

// covers: C-47
#[test]
fn junit4s_parameterized_invocations_are_one_test_and_enclosed_classes_belong_to_their_outer_class()
{
    let seen = from_reports(&["junit4-ParamTest.xml"], "j4.ParamTest").unwrap();
    let ids: Vec<&String> = seen.tests.as_ref().unwrap().keys().collect();
    assert_eq!(ids, ["positive", "small", "smaller"]);
    assert!(test(&seen, "small").failed && !test(&seen, "smaller").failed);
    let enclosed = from_reports(&["junit4-EnclosedTest.xml"], "j4.EnclosedTest").unwrap();
    assert!(test(&enclosed, "Inner#innerFails").failed);
    assert!(test(&enclosed, "InnerTest#nestedNamedLikeATest").finished);
}

// covers: C-47
#[test]
fn a_report_cut_short_holds_nothing() {
    assert!(reports::cases(&data("truncated-CalcTest.xml")).is_err());
}

// covers: C-47
#[test]
fn reports_from_before_the_build_never_count() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, text: &[u8]| std::fs::write(dir.path().join(name), text).unwrap();
    let pass = br#"<testsuite name="p.A"><testcase name="a" classname="p.A"/></testsuite>"#;
    write("TEST-p.A.xml", pass);
    write("TEST-p.B.xml", pass);
    write("TEST-p.C.xml", pass);
    std::fs::create_dir(dir.path().join("junitreports")).unwrap();
    std::fs::write(dir.path().join("junitreports/TEST-p.D.xml"), pass).unwrap();
    let meta = |name: &str| {
        let m = std::fs::metadata(dir.path().join(name)).unwrap();
        let ms = m
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        (ms, m.len())
    };
    let (a_ms, a_len) = meta("TEST-p.A.xml");
    let (b_ms, b_len) = meta("TEST-p.B.xml");
    // A was there before, unchanged: stale. B was there, and is another size now: this run's.
    let before = [
        Snapshot {
            name: "TEST-p.A.xml".into(),
            mtime_ms: a_ms,
            size: a_len,
        },
        Snapshot {
            name: "TEST-p.B.xml".into(),
            mtime_ms: b_ms,
            size: b_len + 1,
        },
    ];
    let cases = fresh_reports(dir.path(), &before, a_ms - 1000).unwrap();
    assert_eq!(cases.len(), 2, "B and C, never A, nor a file below");
    // A report older than the build's start is never this run's, whatever the snapshot says.
    assert!(
        fresh_reports(dir.path(), &[], a_ms + 60_000)
            .unwrap()
            .is_empty()
    );
    // One fresh report that doesn't read: nothing of the directory counts.
    write(
        "TEST-p.E.xml",
        b"<testsuite name=\"p.E\"><testcase name=\"a\"",
    );
    assert!(fresh_reports(dir.path(), &before, a_ms - 1000).is_err());
}

// covers: C-47
#[test]
fn the_hooks_events_say_what_it_left_out_and_what_ran() {
    let events = jvm::read_events(&data("events-junit5-leave-out.jsonl"));
    assert!(events.listened && !events.torn);
    let calc = from_events("events-junit5-leave-out.jsonl", "com.example.lib.CalcTest").unwrap();
    for id in ["fails", "param", "Inner#innerFails"] {
        let t = test(&calc, id);
        assert!(t.deselected && !t.finished, "{id}");
    }
    assert!(test(&calc, "passes").finished && !test(&calc, "passes").deselected);
    assert!(test(&calc, "errors").failed);
    assert!(test(&calc, "over").finished, "both overloads are one test");
    // It failed; its skipped tests didn't finish in a class with a failure, so it didn't run whole.
    assert!(calc.facts.failed && !calc.facts.complete && !calc.facts.deselected);
    // A class left out whole: nothing of it started.
    let green = from_events("events-junit5-leave-out.jsonl", "com.example.lib.GreenTest").unwrap();
    assert!(green.facts.deselected && !green.facts.complete);
    assert!(green.tests.as_ref().unwrap().values().all(|t| t.deselected));
}

// covers: C-47
#[test]
fn a_test_jvm_that_died_finishes_none_of_the_tests_of_the_class_it_was_in() {
    let crash = from_events("events-junit5-crash.jsonl", "com.example.lib.CrashTest").unwrap();
    assert!(!crash.facts.complete && !crash.facts.failed);
    assert!(
        crash
            .tests
            .as_ref()
            .unwrap()
            .values()
            .all(|t| !t.finished && !t.failed),
        "{:?}",
        crash.tests
    );
    let green = from_events("events-junit5-crash.jsonl", "com.example.lib.GreenTest").unwrap();
    assert!(green.facts.complete && !green.facts.failed);
    assert!(test(&green, "green1").finished);
    // A torn last line: what came before stands.
    let mut bytes = data("events-junit5-crash.jsonl");
    bytes.extend_from_slice(b"{\"e\":\"finished\",\"test\":tr");
    let torn = jvm::read_events(&bytes);
    assert!(torn.torn);
    assert!(torn.classes["com.example.lib.CrashTest"].open);
}

// covers: C-47
#[test]
fn an_event_no_class_owns_is_counted_apart() {
    let events = jvm::read_events(
        br#"{"e":"jvm","pid":"1","dir":"/w","basedir":"/w"}
{"e":"finished","test":false,"uid":"[engine:testng]","class":null,"method":null,"params":null,"status":"FAILED","ms":0,"message":"boom"}
{"e":"finished","test":true,"uid":"[engine:junit-jupiter]/[class:p.A]/[nested-class:B]/[test-factory:dyn()]/[dynamic-test:#1]","class":null,"method":null,"params":null,"status":"SUCCESSFUL","ms":1,"message":null}
"#,
    );
    assert_eq!(events.unattributed, 1);
    assert!(events.classes["p.A"].tests.contains_key("B#dyn"));
}

fn module(dir: &str, test_roots: &[&str]) -> Module {
    Module {
        dir: dir.into(),
        test_roots: test_roots.iter().map(|r| r.to_string()).collect(),
        ..Module::default()
    }
}

fn tree(paths: &[&str]) -> Tree {
    let mut bytes = format!("{}\n", hushrun_core::CANONICALIZATION).into_bytes();
    let mut sorted: Vec<&&str> = paths.iter().collect();
    sorted.sort();
    for (n, path) in sorted.iter().enumerate() {
        bytes.extend_from_slice(format!("f {:064x} {path}", n + 1).as_bytes());
        bytes.push(0);
    }
    Tree::from_manifest(&bytes).expect("a manifest")
}

// covers: C-47
#[test]
fn a_test_class_is_named_by_its_one_source_file_else_by_its_module_and_name() {
    let tree = tree(&[
        "lib/src/test/java/shop/PriceTest.java",
        "lib/src/test/java/shop/Two.java",
        "lib/src/test/kotlin/shop/Two.kt",
        "lib/src/test/kotlin/shop/Specs.kt",
        "lib/src/main/java/shop/Price.java",
    ]);
    let m = module("lib", &["lib/src/test/java", "lib/src/test/kotlin"]);
    assert_eq!(
        unit_of(&m, "shop.PriceTest", &tree),
        ("lib/src/test/java/shop/PriceTest.java".into(), None)
    );
    // Two files are the class's: its module and its name, made of the module's test sources.
    let (unit, own) = unit_of(&m, "shop.Two", &tree);
    assert_eq!(unit, "lib::shop.Two");
    assert_eq!(own.unwrap().len(), 4);
    // No file is (Kotlin names files freely).
    assert_eq!(
        unit_of(&m, "shop.KSpecTest", &tree).0,
        "lib::shop.KSpecTest"
    );
    // A root inside another: the file is the class's only below the longest root holding it.
    let nested = module("lib", &["lib/src/test", "lib/src/test/java"]);
    assert_eq!(
        unit_of(&nested, "shop.PriceTest", &tree).0,
        "lib/src/test/java/shop/PriceTest.java"
    );
    assert_eq!(
        unit_of(&nested, "java.shop.PriceTest", &tree).0,
        "lib::java.shop.PriceTest"
    );
}

// covers: C-47
#[test]
fn sources_are_the_jvm_files_below_the_roots_some_unit_read_and_the_rest_is_the_rest() {
    let tree = tree(&[
        "pom.xml",
        "lib/pom.xml",
        "lib/src/main/java/shop/Price.java",
        "lib/src/main/resources/rates.properties",
        "lib/src/test/java/shop/PriceTest.java",
        "tools/Gen.java",
    ]);
    let units: BTreeSet<String> = ["lib/src/test/java/shop/PriceTest.java".to_string()].into();
    let dirs: BTreeSet<String> = [
        "lib/src/main/java".to_string(),
        "lib/src/test/java".to_string(),
    ]
    .into();
    let sets = classify(&tree, &units, &dirs, &[]);
    assert_eq!(
        sets.of("lib/src/test/java/shop/PriceTest.java"),
        Some(units::Set::Units)
    );
    assert_eq!(
        sets.of("lib/src/main/java/shop/Price.java"),
        Some(units::Set::Sources)
    );
    for rest in [
        "pom.xml",
        "lib/pom.xml",
        "lib/src/main/resources/rates.properties",
        "tools/Gen.java",
    ] {
        assert_eq!(sets.of(rest), Some(units::Set::Rest), "{rest}");
    }
}

// covers: C-47
#[test]
fn what_testngs_interceptor_left_out_is_told_beside_what_its_reports_say_ran() {
    // TestNG's own provider: the interceptor wrote what it left out, no listener wrote events, and
    // the report holds what ran. The class didn't run whole.
    let events = jvm::read_events(
        br#"{"e":"jvm","pid":"1","dir":"/w","basedir":"/w"}
{"e":"left-out","by":"testng","class":"ng.GreenTest","method":"green1"}
"#,
    );
    let mut seen = ClassSeen {
        events: events.classes.get("ng.GreenTest").cloned(),
        ..ClassSeen::default()
    };
    for case in reports::cases(&data("testng-TestSuite.xml")).unwrap() {
        if case.classname == "ng.GreenTest" && case.name != "green1" {
            seen.from_reports = true;
            let slot = seen.cases.entry(case.name.clone()).or_default();
            slot.0.push(case.outcome);
        }
    }
    let green = decide_class(&seen, false).unwrap();
    assert!(test(&green, "green1").deselected);
    assert!(test(&green, "green2").finished && !test(&green, "green2").deselected);
}

// covers: C-32, C-47
#[test]
fn a_module_whose_hook_left_out_what_its_list_didnt_name_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let abs = real(dir.path());
    let at = abs.to_string_lossy().into_owned();
    let graph = |lines: &[&str]| -> Graph {
        serde_json::from_value(serde_json::json!({
            "version": 1, "startedMs": 0, "unitsDir": "/u", "projects": [],
            "leaveOut": {"written": true, "listed": {at.clone(): lines}}
        }))
        .unwrap()
    };
    let modules = BTreeMap::from([(
        "lib".to_string(),
        Module {
            dir: "lib".into(),
            abs: abs.clone(),
            ..Module::default()
        },
    )]);
    let events = |lines: &str| {
        BTreeMap::from([(
            "1.jsonl".to_string(),
            jvm::read_events(
                format!(
                    "{{\"e\":\"jvm\",\"pid\":\"1\",\"dir\":{at:?},\"basedir\":{at:?}}}\n{lines}"
                )
                .as_bytes(),
            ),
        )])
    };
    let ran = BTreeMap::from([(("lib".to_string(), "p.A".to_string()), BTreeSet::new())]);
    let left_b = r#"{"e":"left-out","by":"junit-platform","uid":"u","class":"p.A","method":"b","entry":"p.A#b"}"#;
    let started_c = r#"{"e":"started","test":true,"uid":"[engine:junit-jupiter]/[class:p.A]/[method:c()]","class":"p.A","method":"c","params":""}"#;
    // b left out, c ran: as listed.
    let both = events(&format!("{left_b}\n{started_c}\n"));
    assert_eq!(
        left_out_unlisted(&graph(&["p.A#b"]), &modules, &both, &ran),
        None
    );
    // b left out though only c was listed.
    let why = left_out_unlisted(&graph(&["p.A#c"]), &modules, &both, &ran).unwrap();
    assert!(why.contains("left out p.A#b"), "{why}");
    // d listed, in a class that ran, and neither left out nor started.
    let why = left_out_unlisted(&graph(&["p.A#b", "p.A#d"]), &modules, &both, &ran).unwrap();
    assert!(why.contains("found no test p.A#d"), "{why}");
}
