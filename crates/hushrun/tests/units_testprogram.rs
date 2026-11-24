//! \[ADR 0120\] C++ test programs COMMAND starts itself, by their path, not through CTest
//! (C-106 to C-108): a GoogleTest or Catch2 3 program of a CMake build directory is a unit, each of
//! its tests a test, what it read what its build directory says it is built from, and hushrun
//! leaves tests out through the framework's own variables, after listing again with them.
//!
//! The fixtures are CMake projects that find GoogleTest and Catch2 3 installed (Debian's and
//! Ubuntu's `libgtest-dev` and `catch2`), and need CMake and Ninja on `PATH`. They run where
//! `HUSHRUN_TEST_CXX_TESTS` is set, which CI sets on Linux after installing them, and must pass
//! there; elsewhere they say so and pass. No fixture needs the network.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;

const NO_HOST: &str = "http://127.0.0.1:9";

/// Variables a run must never inherit.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_UNITS",
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "HUSHRUN_BUILD",
    "HUSHRUN_CC",
    "GTEST_FILTER",
    "GTEST_OUTPUT",
    "GTEST_TOTAL_SHARDS",
    "GTEST_SHARD_INDEX",
    "BAZEL_TEST",
    "XML_OUTPUT_FILE",
    "TESTBRIDGE_TEST_ONLY",
    "TEST_SHARD_INDEX",
    "TEST_TOTAL_SHARDS",
    "TEST_SHARD_STATUS_FILE",
    "CMAKE_GENERATOR",
    "CMAKE_BUILD_TYPE",
    "CMAKE_CXX_COMPILER_LAUNCHER",
];

fn wanted() -> bool {
    if std::env::var_os("HUSHRUN_TEST_CXX_TESTS").is_some() {
        return true;
    }
    eprintln!("note: skipping: HUSHRUN_TEST_CXX_TESTS isn't set (GoogleTest, Catch2 3, CMake)");
    false
}

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    built: std::cell::Cell<Option<std::time::SystemTime>>,
}

impl Project {
    fn new(files: &[(&str, &str)]) -> Project {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let p = Project {
            _dir: dir,
            cache: root.join("cache"),
            root,
            repo,
            built: std::cell::Cell::new(None),
        };
        for (path, text) in files {
            p.write(path, text);
        }
        git::commit_all(&p.repo, "init");
        let out = p
            .tool("cmake")
            .args(["-S", ".", "-B", "build", "-G", "Ninja"])
            .output()
            .expect("cmake");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        p.build();
        p
    }

    fn tool(&self, name: &str) -> Command {
        let mut cmd = Command::new(name);
        cmd.current_dir(&self.repo);
        for var in CLEARED {
            cmd.env_remove(var);
        }
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        cmd
    }

    fn build(&self) {
        let out = self
            .tool("cmake")
            .args(["--build", "build"])
            .output()
            .expect("cmake --build");
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        self.built.set(Some(std::time::SystemTime::now()));
    }

    /// Writes a file, in a later second than the last build, as the CTest adapter's tests do.
    fn write(&self, path: &str, text: &str) {
        if let Some(built) = self.built.get() {
            let second = |t: std::time::SystemTime| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs())
            };
            while second(std::time::SystemTime::now()) <= second(built) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        for var in CLEARED {
            cmd.env_remove(var);
        }
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
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
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// The tests GoogleTest says it ran.
    fn gtest_ran(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .stdout
            .lines()
            .filter_map(|l| l.strip_prefix("[ RUN      ] "))
            .map(str::to_string)
            .collect();
        out.sort();
        out
    }

    /// The fixture's tests Catch2 says it ran, from `--durations yes`, which names each
    /// section too.
    fn catch2_ran(&self) -> Vec<String> {
        let tests = ["adds, small", "multiplies [x]", "adds zero"];
        let mut out: Vec<String> = self
            .stdout
            .lines()
            .filter_map(|l| l.split_once(" s: ").map(|(_, name)| name.to_string()))
            .filter(|name| tests.contains(&name.as_str()))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn says(&self, text: &str) -> bool {
        self.stderr.contains(text)
    }
}

fn yaml() -> String {
    "version: 1\nrepository: example.com/acme/calc\nchecks:\n  unit:\n    tests: {mode: reuse, unit: test}\n"
        .to_string()
}

const LISTS: &str = "cmake_minimum_required(VERSION 3.20)\nproject(calc CXX)\n\
    find_package(GTest REQUIRED)\nfind_package(Catch2 3 REQUIRED)\n\
    add_library(calc STATIC src/calc.cpp)\ntarget_include_directories(calc PUBLIC src)\n\
    add_executable(t_calc tests/t_calc.cpp)\ntarget_link_libraries(t_calc PRIVATE calc GTest::gtest_main)\n\
    add_executable(t_other tests/t_other.cpp)\ntarget_link_libraries(t_other PRIVATE GTest::gtest_main)\n\
    add_executable(c_calc tests/c_calc.cpp)\ntarget_link_libraries(c_calc PRIVATE calc Catch2::Catch2WithMain)\n";

const CALC_H: &str = "#pragma once\nint add(int, int);\nint mul(int, int);\n";
const CALC: &str = "#include \"calc.h\"\nint add(int a, int b) { return a + b; }\nint mul(int a, int b) { return a * b; }\n";

fn files() -> Vec<(&'static str, String)> {
    vec![
        ("hushrun.yaml", yaml()),
        (".gitignore", "build/\n".into()),
        ("CMakeLists.txt", LISTS.into()),
        ("src/calc.h", CALC_H.into()),
        ("src/calc.cpp", CALC.into()),
        (
            "tests/t_calc.cpp",
            "#include <gtest/gtest.h>\n#include \"calc.h\"\n\
             TEST(Add, Small) { EXPECT_EQ(add(1, 2), 3); }\nTEST(Add, Zero) { EXPECT_EQ(add(0, 0), 0); }\n\
             TEST(Mul, Small) { EXPECT_EQ(mul(2, 3), 6); }\nTEST(Mul, One) { EXPECT_EQ(mul(1, 7), 7); }\n\
             TEST(Skip, Me) { GTEST_SKIP(); }\n"
                .into(),
        ),
        (
            "tests/t_other.cpp",
            "#include <gtest/gtest.h>\nTEST(Other, Works) { SUCCEED(); }\n".into(),
        ),
        (
            "tests/c_calc.cpp",
            "#include <catch2/catch_test_macros.hpp>\n#include \"calc.h\"\n\
             TEST_CASE(\"adds, small\") { REQUIRE(add(1, 2) == 3); }\n\
             TEST_CASE(\"multiplies [x]\") { SECTION(\"small\") { REQUIRE(mul(2, 3) == 6); } SECTION(\"one\") { REQUIRE(mul(1, 7) == 7); } }\n\
             TEST_CASE(\"adds zero\") { REQUIRE(add(0, 0) == 0); }\n"
                .into(),
        ),
    ]
}

fn project() -> Project {
    let files = files();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    Project::new(&refs)
}

/// What a change to `calc.cpp` that breaks `mul` does: every test that calls it fails.
const CALC_BROKEN: &str = "#include \"calc.h\"\nint add(int a, int b) { return a + b; }\nint mul(int a, int b) { return a * b + 1; }\n";

// covers: C-106, C-108
#[test]
fn googletests_tests_that_passed_on_their_programs_files_are_left_out_and_a_change_runs_them() {
    if !wanted() {
        return;
    }
    let p = project();
    let first = p.run(&["run", "unit", "--", "build/t_calc"], &[]);
    assert_eq!(first.code, 0, "{}{}", first.stdout, first.stderr);
    assert_eq!(first.gtest_ran().len(), 5, "{}", first.stdout);
    // A file of another program only: t_calc's tests passed on what it is built from.
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n",
    );
    p.build();
    // Its first naming of t_other's sources runs everything once; after that, they're reused.
    let _ = p.run(&["run", "unit", "--", "build/t_calc"], &[]);
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n// two\n",
    );
    p.build();
    let second = p.run(&["run", "unit", "--", "build/t_calc"], &[]);
    assert_eq!(second.code, 0, "{}{}", second.stdout, second.stderr);
    assert!(
        second.says("already passed on the files they read"),
        "{}",
        second.stderr
    );
    assert!(second.says("tests reused"), "{}", second.stderr);
    assert!(
        second.gtest_ran().len() <= 1,
        "the audit runs one: {}",
        second.stdout
    );
    assert!(
        second.stdout.contains("Google Test filter = -"),
        "{}",
        second.stdout
    );
    // A change to what t_calc is built from runs every test of it, and the broken ones fail.
    p.write("src/calc.cpp", CALC_BROKEN);
    p.build();
    let broken = p.run(&["run", "unit", "--", "build/t_calc"], &[]);
    assert_ne!(broken.code, 0, "{}{}", broken.stdout, broken.stderr);
    assert_eq!(broken.gtest_ran().len(), 5, "{}", broken.stdout);
    assert!(
        broken.stdout.contains("[  FAILED  ] Mul.Small"),
        "{}",
        broken.stdout
    );
}

// covers: C-106, C-108
#[test]
fn a_program_older_than_what_it_is_built_from_records_no_pass() {
    if !wanted() {
        return;
    }
    let p = project();
    p.write("src/calc.cpp", CALC_BROKEN);
    let ran = p.run(&["run", "unit", "--", "build/t_calc"], &[]);
    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert!(ran.says("ran old code"), "{}", ran.stderr);
    assert!(ran.says("isn't recorded"), "{}", ran.stderr);
}

// covers: C-106
#[test]
fn a_filter_of_commands_own_is_observed_and_leaves_nothing_out() {
    if !wanted() {
        return;
    }
    let p = project();
    let first = p.run(
        &["run", "unit", "--", "build/t_calc", "--gtest_filter=Add.*"],
        &[],
    );
    assert_eq!(first.code, 0, "{}", first.stderr);
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n",
    );
    p.build();
    let again = p.run(
        &["run", "unit", "--", "build/t_calc", "--gtest_filter=Add.*"],
        &[],
    );
    assert!(again.says("wins over the GTEST_FILTER"), "{}", again.stderr);
    assert_eq!(
        again.gtest_ran(),
        ["Add.Small", "Add.Zero"],
        "{}",
        again.stdout
    );
}

// covers: C-107, C-108
#[test]
fn catch2s_tests_are_left_out_by_their_names_through_bazels_variables() {
    if !wanted() {
        return;
    }
    let p = project();
    let first = p.run(
        &["run", "unit", "--", "build/c_calc", "--durations", "yes"],
        &[],
    );
    assert_eq!(first.code, 0, "{}{}", first.stdout, first.stderr);
    assert_eq!(first.catch2_ran().len(), 3, "{}", first.stdout);
    for text in ["// one\n", "// two\n"] {
        p.write(
            "tests/t_other.cpp",
            &format!("#include <gtest/gtest.h>\nTEST(Other, Works) {{ }}\n{text}"),
        );
        p.build();
        let _ = p.run(
            &["run", "unit", "--", "build/c_calc", "--durations", "yes"],
            &[],
        );
    }
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n// three\n",
    );
    p.build();
    let reused = p.run(
        &["run", "unit", "--", "build/c_calc", "--durations", "yes"],
        &[],
    );
    assert_eq!(reused.code, 0, "{}{}", reused.stdout, reused.stderr);
    assert!(reused.says("tests reused"), "{}", reused.stderr);
    // Never every test: Catch2 fails when it runs none, so the audit's, or one, runs.
    assert!(!reused.catch2_ran().is_empty(), "{}", reused.stdout);
    assert!(reused.catch2_ran().len() < 3, "{}", reused.stdout);
    assert!(reused.stdout.contains("Filters:"), "{}", reused.stdout);
    p.write("src/calc.cpp", CALC_BROKEN);
    p.build();
    let broken = p.run(
        &["run", "unit", "--", "build/c_calc", "--durations", "yes"],
        &[],
    );
    assert_ne!(broken.code, 0, "{}", broken.stdout);
    assert_eq!(broken.catch2_ran().len(), 3, "{}", broken.stdout);
}

// covers: C-106
#[test]
fn a_doctest_program_is_said_not_to_be_observed() {
    if !wanted() {
        return;
    }
    let Some(doctest) = [
        "/usr/include/doctest/doctest.h",
        "/usr/local/include/doctest/doctest.h",
    ]
    .iter()
    .find(|p| Path::new(p).is_file()) else {
        eprintln!("note: skipping: doctest isn't installed");
        return;
    };
    let _ = doctest;
    let p = Project::new(&[
        ("hushrun.yaml", &yaml()),
        (".gitignore", "build/\n"),
        (
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.20)\nproject(d CXX)\nadd_executable(t_d t_d.cpp)\n",
        ),
        (
            "t_d.cpp",
            "#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN\n#include <doctest/doctest.h>\nTEST_CASE(\"one\") { CHECK(1 == 1); }\n",
        ),
    ]);
    let ran = p.run(&["run", "unit", "--", "build/t_d"], &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.says("doctest takes what to run"), "{}", ran.stderr);
}
