use super::*;

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_string).collect()
}

/// A CMake build directory in `dir` holding a "program" whose bytes are `bytes`.
fn program(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let build = dir.join("build");
    std::fs::create_dir_all(build.join("tests")).unwrap();
    std::fs::write(
        build.join("CMakeCache.txt"),
        "CMAKE_GENERATOR:INTERNAL=Ninja\n",
    )
    .unwrap();
    let path = build.join("tests").join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

// covers: C-106
#[test]
fn a_program_of_a_build_directory_started_by_its_path_is_recognized_by_what_it_holds() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    program(&cwd, "t_g", b"\x7fELF ... --gtest_list_tests ...");
    program(
        &cwd,
        "t_c",
        b"--allow-running-no-tests BAZEL_TEST TESTBRIDGE_TEST_ONLY XML_OUTPUT_FILE",
    );
    program(&cwd, "t_c3", b"--allow-running-no-tests XML_OUTPUT_FILE");
    program(&cwd, "t_d", b"doctest dt-no-run");
    program(&cwd, "t_x", b"plain");
    let word = |command: &str| match recognized(&words(command), &cwd) {
        Some(Ok(adapter)) => Ok(adapter.runner().to_string()),
        Some(Err(why)) => Err(why),
        None => Err("none".into()),
    };
    assert_eq!(word("build/tests/t_g").as_deref(), Ok("GoogleTest"));
    assert_eq!(word("./build/tests/t_c -s").as_deref(), Ok("Catch2"));
    assert!(word("build/tests/t_c3").unwrap_err().contains("BAZEL_TEST"));
    assert!(word("build/tests/t_d").unwrap_err().contains("doctest"));
    assert!(
        word("build/tests/t_g --gtest_output=xml:o.xml")
            .unwrap_err()
            .contains("--gtest_output")
    );
    // Not a test program hushrun knows, not started by its path, not in a build directory.
    assert_eq!(word("build/tests/t_x").unwrap_err(), "none");
    assert_eq!(word("t_g").unwrap_err(), "none");
    std::fs::write(cwd.join("t_g"), b"--gtest_list_tests").unwrap();
    assert_eq!(word("./t_g").unwrap_err(), "none");
}

// covers: C-106
#[test]
fn it_comes_before_ctest_and_takes_no_ctest_command() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    program(&cwd, "t_g", b"--gtest_list_tests");
    let chosen = super::super::adapter::choose(&words("build/tests/t_g"), &cwd).unwrap();
    assert_eq!(chosen.word(), ADAPTER);
    let ctest = super::super::adapter::choose(&words("ctest --test-dir build"), &cwd).unwrap();
    assert_eq!(ctest.word(), "ctest");
}

fn pending(dir: &Path, listed: &[&str], left: &[&str]) -> Pending {
    Pending {
        unit: "tests/t_g".into(),
        xml: dir.join("out.xml"),
        started_ms: now_ms(SystemTime::now()) - 1000,
        listed: Some(listed.iter().map(|s| s.to_string()).collect()),
        left: left.iter().map(|s| s.to_string()).collect(),
        whole: false,
        report_tests: true,
        selection: "GTEST_FILTER".into(),
        reads: vec!["tests/g.cpp".into()],
        own: vec!["tests/g.cpp".into()],
        ..Pending::default()
    }
}

const GTEST_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites tests="3" name="AllTests">
  <testsuite name="Add" tests="3">
    <testcase name="One" status="run" result="completed" time="0.001" classname="Add" />
    <testcase name="Skip" status="run" result="skipped" time="0." classname="Add" />
    <testcase name="Off" status="notrun" result="suppressed" time="0." classname="Add" />
  </testsuite>
</testsuites>"#;

fn test<'a>(file: &'a FactsFile, id: &str) -> &'a TestFacts {
    file.units[0]
        .tests
        .as_ref()
        .unwrap()
        .iter()
        .find(|t| t.id == id)
        .unwrap()
}

// covers: C-106, C-108
#[test]
fn each_test_comes_to_what_the_xml_says_and_nothing_absent_passes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("out.xml"), GTEST_XML).unwrap();
    let p = pending(
        dir.path(),
        &["Add.One", "Add.Two", "Add.Skip", "Add.Off", "Add.Left"],
        &["Add.Left"],
    );
    let file = complete(&p, Framework::Gtest, true);
    assert!(test(&file, "Add.One").finished && !test(&file, "Add.One").failed);
    assert!(test(&file, "Add.Skip").finished && test(&file, "Add.Skip").skipped);
    assert!(!test(&file, "Add.Off").finished && test(&file, "Add.Off").skipped);
    assert!(test(&file, "Add.Left").deselected);
    // A kept test GoogleTest didn't run: the run starts COMMAND again with nothing left out.
    assert!(!test(&file, "Add.Two").finished);
    assert!(
        file.settings["leftOut"]
            .as_str()
            .unwrap()
            .contains("Add.Two")
    );
    let unit = &file.units[0];
    assert_eq!((unit.selected, unit.finished), (3, 2));

    // Nothing left out: an absent test didn't run, and that's all.
    let p = pending(dir.path(), &["Add.One", "Add.Two"], &[]);
    let file = complete(&p, Framework::Gtest, true);
    assert!(!file.settings.contains_key("leftOut"));
    assert!(!test(&file, "Add.Two").finished);

    // No XML: no test finished.
    let p = pending(&dir.path().join("none"), &["Add.One"], &[]);
    let file = complete(&p, Framework::Gtest, true);
    assert!(!test(&file, "Add.One").finished);
    assert!(
        file.settings["xml"]
            .as_str()
            .unwrap()
            .contains("wasn't read")
    );
}

// covers: C-108
#[test]
fn a_failure_outside_every_test_and_an_old_program_record_no_pass() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("out.xml"),
        r#"<testsuites><testsuite name="">
<testcase name="" status="run" result="completed" classname=""><failure message="SetUp"/></testcase>
<testcase name="One" status="run" result="completed" classname="Add"/>
</testsuite></testsuites>"#,
    )
    .unwrap();
    let file = complete(
        &pending(dir.path(), &["Add.One"], &[]),
        Framework::Gtest,
        false,
    );
    assert!(
        file.errors[0].contains("outside every test"),
        "{:?}",
        file.errors
    );
    let mut p = pending(dir.path(), &["Add.One"], &[]);
    p.stale = Some((true, "g.cpp changed after g.cpp.o was made".into()));
    let file = complete(&p, Framework::Gtest, true);
    assert!(
        file.stale_build
            .as_deref()
            .unwrap()
            .contains("ran old code")
    );
    assert_eq!(file.units[0].not_followed[0].why, "stale program");
}

// covers: C-107, C-108
#[test]
fn catch2s_sections_make_one_test_and_a_failure_with_none_failing_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("out.xml"),
        r#"<testsuites><testsuite name="t_c" tests="3">
<testcase classname="t_c.global" name="gamma/s1" time="0.001" status="run"/>
<testcase classname="t_c.global" name="gamma/s2" time="0.002" status="run"><failure message="x"/></testcase>
<testcase classname="t_c.global" name="skips" time="0" status="run"><skipped/></testcase>
</testsuite></testsuites>"#,
    )
    .unwrap();
    let p = pending(dir.path(), &["alpha", "gamma", "skips"], &["alpha"]);
    let file = complete(&p, Framework::Catch2, false);
    assert!(test(&file, "gamma").failed);
    assert_eq!(test(&file, "gamma").duration_ms, 3);
    assert!(test(&file, "skips").skipped);
    // A test failed: COMMAND's failure is told.
    assert!(!file.settings.contains_key("leftOut"));
    std::fs::write(
        dir.path().join("out.xml"),
        r#"<testsuites><testsuite name="t_c" tests="1">
<testcase classname="t_c.global" name="skips" time="0" status="run"><skipped/></testcase>
</testsuite></testsuites>"#,
    )
    .unwrap();
    let p = pending(dir.path(), &["alpha", "skips"], &["alpha"]);
    let file = complete(&p, Framework::Catch2, false);
    assert!(
        file.settings["leftOut"]
            .as_str()
            .unwrap()
            .contains("no test failing")
    );
}

// covers: C-106
#[test]
fn what_a_program_and_its_environment_let_be_left_out() {
    assert_eq!(Framework::Gtest.args(&words("--gtest_shuffle")), Ok(None));
    let shard = |n: &str| (n == "GTEST_SHARD_INDEX").then(|| OsString::from("1"));
    assert!(Framework::Gtest.env(&shard).unwrap().is_some());
    let bazel = |n: &str| (n == "XML_OUTPUT_FILE").then(|| OsString::from("x"));
    assert!(Framework::Catch2.env(&bazel).is_err());
    assert_eq!(millis(Some("0.0015")), 2);
    assert_eq!(millis(Some("x")), 0);
}
