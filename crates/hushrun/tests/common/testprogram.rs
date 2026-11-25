//! \[ADR 0120\] The fixtures of the tests of C++ test programs COMMAND starts by their path, which
//! `units_testprogram.rs` (in-process) and `e2e_testprogram.rs` (the CLI) share: CMake projects
//! that find GoogleTest and Catch2 3 installed (Debian's and Ubuntu's `libgtest-dev` and
//! `catch2`), built with Ninja. They run where `HUSHRUN_TEST_CXX_TESTS` is set, which CI sets on
//! Linux after installing them, and must pass there; elsewhere they say so and pass. No fixture
//! needs the network.

#![allow(dead_code)]

use std::time::SystemTime;

use super::repo::Repo;

/// Variables of the frameworks', of CMake's and of hushrun's helpers' that COMMAND must never
/// inherit, besides [`super::repo::CLEARED`].
pub const RUNNER: &[&str] = &[
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
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
];

/// Whether the tests run here: `HUSHRUN_TEST_CXX_TESTS` says the frameworks are installed.
pub fn wanted() -> bool {
    if std::env::var_os("HUSHRUN_TEST_CXX_TESTS").is_some() {
        return true;
    }
    eprintln!("note: skipping: HUSHRUN_TEST_CXX_TESTS isn't set (GoogleTest, Catch2 3, CMake)");
    false
}

/// A repository configured in `build` with Ninja and built, which knows when it was last built.
pub struct Project {
    pub r: Repo,
    built: std::cell::Cell<Option<SystemTime>>,
}

impl Project {
    pub fn new(files: &[(&str, &str)]) -> Project {
        let files: Vec<(String, String)> = files
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect();
        let p = Project {
            r: Repo::new(&files).clear(RUNNER),
            built: std::cell::Cell::new(None),
        };
        let out =
            p.r.command("cmake")
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

    pub fn build(&self) {
        let out = self
            .r
            .command("cmake")
            .args(["--build", "build"])
            .output()
            .expect("cmake --build");
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        self.built.set(Some(SystemTime::now()));
    }

    /// Writes a file, in a later second than the last build, as the CTest adapter's tests do.
    pub fn write(&self, path: &str, text: &str) {
        if let Some(built) = self.built.get() {
            let second = |t: SystemTime| {
                t.duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs())
            };
            while second(SystemTime::now()) <= second(built) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        self.r.write(path, text);
    }
}

/// The tests GoogleTest says it ran, in COMMAND's output `stdout`.
pub fn gtest_ran(stdout: &str) -> Vec<String> {
    let mut out: Vec<String> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("[ RUN      ] "))
        .map(str::to_string)
        .collect();
    out.sort();
    out
}

/// The fixture's tests Catch2 says it ran, from `--durations yes`, which names each section too.
pub fn catch2_ran(stdout: &str) -> Vec<String> {
    let tests = ["adds, small", "multiplies [x]", "adds zero"];
    let mut out: Vec<String> = stdout
        .lines()
        .filter_map(|l| l.split_once(" s: ").map(|(_, name)| name.to_string()))
        .filter(|name| tests.contains(&name.as_str()))
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn yaml() -> String {
    "version: 1\nrepository: example.com/acme/calc\nchecks:\n  unit:\n    tests: {mode: reuse, unit: test}\n"
        .to_string()
}

pub const LISTS: &str = "cmake_minimum_required(VERSION 3.20)\nproject(calc CXX)\n\
    find_package(GTest REQUIRED)\nfind_package(Catch2 3 REQUIRED)\n\
    add_library(calc STATIC src/calc.cpp)\ntarget_include_directories(calc PUBLIC src)\n\
    add_executable(t_calc tests/t_calc.cpp)\ntarget_link_libraries(t_calc PRIVATE calc GTest::gtest_main)\n\
    add_executable(t_other tests/t_other.cpp)\ntarget_link_libraries(t_other PRIVATE GTest::gtest_main)\n\
    add_executable(c_calc tests/c_calc.cpp)\ntarget_link_libraries(c_calc PRIVATE calc Catch2::Catch2WithMain)\n";

pub const CALC_H: &str = "#pragma once\nint add(int, int);\nint mul(int, int);\n";
pub const CALC: &str = "#include \"calc.h\"\nint add(int a, int b) { return a + b; }\nint mul(int a, int b) { return a * b; }\n";

pub fn files() -> Vec<(&'static str, String)> {
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

pub fn project() -> Project {
    let files = files();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    Project::new(&refs)
}

/// What a change to `calc.cpp` that breaks `mul` does: every test that calls it fails.
pub const CALC_BROKEN: &str = "#include \"calc.h\"\nint add(int a, int b) { return a + b; }\nint mul(int a, int b) { return a * b + 1; }\n";

/// A CMake project of one doctest program, `build/t_d`, when doctest is installed.
pub fn doctest_project() -> Option<Project> {
    if ![
        "/usr/include/doctest/doctest.h",
        "/usr/local/include/doctest/doctest.h",
    ]
    .iter()
    .any(|p| std::path::Path::new(p).is_file())
    {
        eprintln!("note: skipping: doctest isn't installed");
        return None;
    }
    Some(Project::new(&[
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
    ]))
}
