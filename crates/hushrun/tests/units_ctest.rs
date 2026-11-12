//! \[ADR 0087\] Test programs with CTest (C-30 to C-34, C-39, C-49): a unit is a test program, a
//! test one CTest test, what a program read is what its build directory says it is built from,
//! and hushrun's `ctest` on `PATH` leaves out only what it was told, after checking it again.
//!
//! The fixtures are generated CMake projects of plain C, and need CMake: `HUSHRUN_TEST_CMAKE` names
//! the directories holding `cmake` and `ctest` (and `ninja`, where there), separated as `PATH` is;
//! each CMake there runs the tests, and a directory holding only `ninja` gives the others one.
//! Without it, the `cmake` on `PATH`. They say so and pass where CMake is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets. No fixture needs the network.

#![cfg(unix)]

#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;
use serde_json::{Value, json};

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
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "CTEST_OUTPUT_ON_FAILURE",
    "CTEST_PARALLEL_LEVEL",
    "CTEST_NO_TESTS_ACTION",
    "CMAKE_GENERATOR",
    "CMAKE_BUILD_TYPE",
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

/// One CMake to run the fixtures with.
#[derive(Debug, Clone)]
struct Cmake {
    bin: PathBuf,
    ninja: Option<PathBuf>,
    version: (u32, u32),
}

/// The CMakes `HUSHRUN_TEST_CMAKE` names, or the one on `PATH`.
fn cmakes() -> Vec<Cmake> {
    let dirs: Vec<PathBuf> = match std::env::var_os("HUSHRUN_TEST_CMAKE") {
        Some(list) => std::env::split_paths(&list).collect(),
        None => which("cmake")
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .into_iter()
            .collect(),
    };
    let any_ninja = dirs
        .iter()
        .map(|d| d.join("ninja"))
        .find(|n| n.is_file())
        .or_else(|| which("ninja"));
    dirs.iter()
        .filter(|d| d.join("cmake").is_file() && d.join("ctest").is_file())
        .filter_map(|d| {
            let out = Command::new(d.join("cmake"))
                .arg("--version")
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let v = text.split("version ").nth(1)?;
            let mut parts = v.split('.').map(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u32>()
                    .unwrap_or(0)
            });
            let version = (parts.next()?, parts.next()?);
            let own = d.join("ninja");
            Some(Cmake {
                bin: d.clone(),
                ninja: Some(own)
                    .filter(|n| n.is_file())
                    .or_else(|| any_ninja.clone()),
                version,
            })
        })
        .collect()
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

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

/// The CMakes to run with, or none, said.
fn with_cmake() -> Vec<Cmake> {
    let found = cmakes();
    if tools(
        "CMake 3.21 or later (HUSHRUN_TEST_CMAKE)",
        !found.is_empty(),
    ) {
        found
    } else {
        Vec::new()
    }
}

/// The generators a CMake can use here: Ninja where there is one, and Makefiles where `make` is.
fn generators(cmake: &Cmake) -> Vec<&'static str> {
    let mut out = Vec::new();
    if cmake.ninja.is_some() {
        out.push("Ninja");
    }
    if which("make").is_some() {
        out.push("Unix Makefiles");
    }
    out
}

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    cmake: Cmake,
    path: std::ffi::OsString,
    /// When the last build ended.
    built: std::cell::Cell<Option<std::time::SystemTime>>,
}

impl Project {
    /// A repository of `files`, configured in `build` with `generator` and built.
    fn new(cmake: &Cmake, generator: &str, files: &[(String, String)]) -> Project {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let mut dirs = vec![cmake.bin.clone()];
        dirs.extend(
            cmake
                .ninja
                .as_ref()
                .and_then(|n| n.parent().map(Path::to_path_buf)),
        );
        dirs.extend(which("make").and_then(|m| m.parent().map(Path::to_path_buf)));
        dirs.extend(which("cc").and_then(|m| m.parent().map(Path::to_path_buf)));
        dirs.push("/usr/bin".into());
        dirs.push("/bin".into());
        let path = std::env::join_paths(dirs).unwrap();
        let p = Project {
            _dir: dir,
            cache: root.join("cache"),
            root,
            repo,
            cmake: cmake.clone(),
            path,
            built: std::cell::Cell::new(None),
        };
        for (path, text) in files {
            p.write(path, text);
        }
        git::commit_all(&p.repo, "init");
        let mut configure = p.tool("cmake");
        configure.args(["-S", ".", "-B", "build", "-G", generator]);
        if generator == "Ninja" {
            configure.arg(format!(
                "-DCMAKE_MAKE_PROGRAM={}",
                cmake.ninja.as_ref().unwrap().display()
            ));
        }
        let out = configure.output().expect("cmake");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        p.build();
        p
    }

    fn tool(&self, name: &str) -> Command {
        let mut cmd = Command::new(self.cmake.bin.join(name));
        cmd.current_dir(&self.repo).env("PATH", &self.path);
        for var in CLEARED {
            cmd.env_remove(var);
        }
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

    /// Writes a file of the repository, in a later second than the last build: Make 3.81, macOS's,
    /// compares times by the second, and builds nothing again for a header written in the second
    /// its object was built.
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

    fn hushrun(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("PATH", &self.path)
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
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("RUSTC_WORKSPACE_WRAPPER")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        cmd
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = self.hushrun();
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

    /// hushrun's `ctest` started as COMMAND's tools start it, with a run's directory holding
    /// `run` as `hushrun.json` and `plan` as the plan's file: its exit code, what it printed, and
    /// the facts it wrote.
    fn shim(&self, args: &[&str], run: Value, plan: Option<Value>, env: &[(&str, &str)]) -> Shim {
        let units = tempfile::tempdir().expect("tempdir");
        let mut run = run;
        run["schema"] = json!("hushrun.units/v0");
        run["root"] = json!(self.repo);
        if run.get("reuse").is_none() {
            run["reuse"] = json!([]);
        }
        std::fs::write(units.path().join("hushrun.json"), run.to_string()).unwrap();
        if let Some(mut plan) = plan {
            plan["schema"] = json!("hushrun.ctest-plan/v1");
            let file = units.path().join("ctest-plan.v1");
            std::fs::write(&file, plan.to_string()).unwrap();
            // A plan made before every program was last built.
            if plan.get("old").is_some() {
                let made = Command::new("touch")
                    .args(["-t", "200001010000"])
                    .arg(&file)
                    .status()
                    .unwrap();
                assert!(made.success());
            }
        }
        let mut cmd = self.hushrun();
        let mut was = std::ffi::OsString::from("=");
        was.push(&self.path);
        cmd.env("HUSHRUN_UNITS", units.path())
            .env("HUSHRUN_PROGRAMS_PATH", was)
            .arg("__hushrun-program")
            .arg("ctest")
            .args(args);
        for (name, value) in env {
            cmd.env(name, value);
        }
        let output = cmd.output().expect("hushrun's ctest");
        let mut facts: Vec<Value> = std::fs::read_dir(units.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                name.starts_with("ctest-") && name.ends_with(".json")
            })
            .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
            .collect();
        assert!(facts.len() <= 1, "{facts:#?}");
        Shim {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            facts: facts.pop().unwrap_or(Value::Null),
        }
    }

    /// The newest result this machine recorded for the check.
    fn newest_result(&self) -> Value {
        let mut files = Vec::new();
        collect(&self.cache.join("results/results/v0"), &mut files);
        files.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
        let newest = files.last().expect("a result");
        serde_json::from_slice(&std::fs::read(newest).unwrap()).unwrap()
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
            .unwrap_or_else(|| panic!("no line starting {start:?} in:\n{}", self.stderr))
    }

    /// The tests ctest says it ran.
    fn ran(&self) -> Vec<String> {
        self.stdout
            .lines()
            .filter_map(|l| {
                let rest = l.split_once(" Test #")?.1;
                let name = rest.split_once(": ")?.1;
                Some(name.split(" .").next()?.trim().to_string())
            })
            .collect()
    }
}

struct Shim {
    code: i32,
    stdout: String,
    facts: Value,
}

impl Shim {
    /// Each unit's tests the facts say were left out, `unit::id`.
    fn left_out(&self) -> Vec<String> {
        let mut out = Vec::new();
        for unit in self.facts["units"].as_array().into_iter().flatten() {
            for test in unit["tests"].as_array().into_iter().flatten() {
                if test["deselected"] == json!(true) {
                    out.push(format!(
                        "{}::{}",
                        unit["unit"].as_str().unwrap(),
                        test["id"].as_str().unwrap()
                    ));
                }
            }
        }
        out.sort();
        out
    }

    fn unit(&self, name: &str) -> &Value {
        self.facts["units"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|u| u["unit"] == json!(name))
            .unwrap_or_else(|| panic!("no unit {name} in {:#}", self.facts))
    }

    fn selection(&self) -> &str {
        self.facts["settings"]["selection"].as_str().unwrap_or("")
    }
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A shop in C: a price library two test programs link, a cart header one of them includes, and
/// three programs that read headers of their own, or none. Every test but `greet` fails when
/// `FAIL_HIDDEN` is set, which no file says, as a missed dependency would.
fn shop(tests: &str) -> Vec<(String, String)> {
    let hidden = "  if (getenv(\"FAIL_HIDDEN\")) return 1;\n";
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "build/\n"),
        file(
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.19)\nproject(shop C)\nenable_testing()\n\
             add_library(price STATIC src/price.c)\ntarget_include_directories(price PUBLIC src)\n\
             foreach(t t_price t_cart t_greet t_util t_misc)\n  add_executable(${t} tests/${t}.c)\n  target_include_directories(${t} PRIVATE src)\nendforeach()\n\
             target_link_libraries(t_price PRIVATE price)\ntarget_link_libraries(t_cart PRIVATE price)\n\
             add_test(NAME price COMMAND t_price)\n\
             add_test(NAME cart_total COMMAND t_cart total)\nadd_test(NAME cart_empty COMMAND t_cart empty)\nadd_test(NAME cart_tax COMMAND t_cart tax)\n\
             add_test(NAME greet COMMAND t_greet)\nadd_test(NAME util COMMAND t_util)\nadd_test(NAME misc COMMAND t_misc)\n",
        ),
        file("src/price.h", "int price(int x);\n"),
        file(
            "src/price.c",
            "#include \"price.h\"\nint price(int x) { return x * 2; }\n",
        ),
        file(
            "src/cart.h",
            "static int cart_total(int a, int b) { return a + b; }\n",
        ),
        file(
            "src/greet.h",
            "static const char *greet(void) { return \"hi\"; }\n",
        ),
        file("src/util.h", "static int twice(int x) { return 2 * x; }\n"),
        file(
            "tests/t_price.c",
            &format!(
                "#include <stdlib.h>\n#include \"price.h\"\nint main(void) {{\n{hidden}  return price(3) == 6 ? 0 : 1;\n}}\n"
            ),
        ),
        file(
            "tests/t_cart.c",
            &format!(
                "#include <stdlib.h>\n#include <string.h>\n#include \"price.h\"\n#include \"cart.h\"\n\
                 int main(int argc, char **argv) {{\n  const char *c = argc > 1 ? argv[1] : \"\";\n{hidden}\
                 \x20 if (getenv(\"FAIL_CART\") && strcmp(getenv(\"FAIL_CART\"), c) == 0) return 1;\n\
                 \x20 if (strcmp(c, \"total\") == 0) return cart_total(price(1), price(2)) == 6 ? 0 : 1;\n  return 0;\n}}\n"
            ),
        ),
        file(
            "tests/t_greet.c",
            "#include \"greet.h\"\nint main(void) { return greet()[0] == 'h' ? 0 : 1; }\n",
        ),
        file(
            "tests/t_util.c",
            &format!(
                "#include <stdlib.h>\n#include \"util.h\"\nint main(void) {{\n{hidden}  return twice(2) == 4 ? 0 : 1;\n}}\n"
            ),
        ),
        file(
            "tests/t_misc.c",
            &format!("#include <stdlib.h>\nint main(void) {{\n{hidden}  return 0;\n}}\n"),
        ),
    ]
}

const CTEST: &[&str] = &["run", "unit", "--", "ctest", "--test-dir", "build"];

/// The files each of the shop's programs is built from.
fn reads(unit: &str) -> Vec<&'static str> {
    match unit {
        "t_price" => vec!["src/price.c", "src/price.h", "tests/t_price.c"],
        "t_cart" => vec!["src/cart.h", "src/price.c", "src/price.h", "tests/t_cart.c"],
        "t_greet" => vec!["src/greet.h", "tests/t_greet.c"],
        "t_util" => vec!["src/util.h", "tests/t_util.c"],
        "t_misc" => vec!["tests/t_misc.c"],
        _ => unreachable!(),
    }
}

/// The plan's file for the shop, each unit listed by what it is built from.
fn shop_plan(units: &[&str]) -> Value {
    let scope = [
        "src/cart.h",
        "src/greet.h",
        "src/price.c",
        "src/price.h",
        "src/util.h",
        "tests/t_cart.c",
        "tests/t_greet.c",
        "tests/t_misc.c",
        "tests/t_price.c",
        "tests/t_util.c",
    ];
    let units: serde_json::Map<String, Value> = units
        .iter()
        .map(|u| (u.to_string(), json!([reads(u)])))
        .collect();
    json!({"units": units, "scope": scope})
}

// covers: C-49, C-30, C-31, C-33, C-34
#[test]
fn programs_are_observed_then_reused_and_a_header_runs_only_the_programs_that_include_it() {
    for cmake in with_cmake() {
        for generator in generators(&cmake) {
            let at = format!("CMake {:?}, {generator}", cmake.version);
            let p = Project::new(&cmake, generator, &shop("reuse"));
            let first = p.run(CTEST, &[]);
            assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
            assert_eq!(first.ran().len(), 7, "{at}\n{}", first.stdout);
            // What each program read is what its build says it is built from: its own source,
            // the headers it includes, and a library's source and headers.
            let map = store::map(&p.cache);
            for unit in ["t_price", "t_cart", "t_greet", "t_util", "t_misc"] {
                assert_eq!(
                    map["units"][unit]["reads"],
                    json!(reads(unit)),
                    "{at}: {map:#}"
                );
            }
            assert!(map["rest"]["CMakeLists.txt"].is_string(), "{at}: {map:#}");
            assert_eq!(
                p.newest_result()["units"],
                json!({"total": 5, "ran": 5, "reused": 0}),
                "{at}"
            );
            // price.h changes: the two programs that include it run; of the three others, which
            // already passed on the files they read, one runs as a check and two are reused.
            p.write("src/price.h", "int price(int x); /* changed */\n");
            p.build();
            let second = p.run(CTEST, &[]);
            assert_eq!(second.code, 0, "{at}\n{}\n{}", second.stdout, second.stderr);
            assert_eq!(
                second.line("hushrun: unit: "),
                "hushrun: unit: 3 of 5 test files run; 2 already passed on the files they read, and 1 more that did runs as a check",
                "{at}"
            );
            let ran = second.ran();
            for name in ["price", "cart_total", "cart_empty", "cart_tax"] {
                assert!(ran.contains(&name.to_string()), "{at}: {ran:?}");
            }
            assert_eq!(ran.len(), 5, "{at}: {ran:?}");
            let result = p.newest_result();
            assert_eq!(result["units"]["reused"], json!(2), "{at}: {result:#}");
            assert_eq!(result["units"]["ran"], json!(3), "{at}: {result:#}");
        }
    }
}

/// A program of the project that writes a header (`gen.h`) a test program includes: the
/// generator's source and the header it includes are what that test program reads.
fn generated() -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(".gitignore", "build/\n"),
        file(
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.19)\nproject(gen C)\nenable_testing()\n\
             add_executable(gen tools/gen.c)\ntarget_include_directories(gen PRIVATE common)\n\
             add_custom_command(OUTPUT ${CMAKE_BINARY_DIR}/gen.h COMMAND gen ${CMAKE_BINARY_DIR}/gen.h DEPENDS gen)\n\
             add_executable(t_u tests/t_u.c ${CMAKE_BINARY_DIR}/gen.h)\ntarget_include_directories(t_u PRIVATE ${CMAKE_BINARY_DIR})\n\
             add_executable(t_s tests/t_s.c)\ntarget_include_directories(t_s PRIVATE common)\n\
             add_executable(t_p tests/t_p.c)\n\
             add_test(NAME u COMMAND t_u)\nadd_test(NAME s COMMAND t_s)\nadd_test(NAME p COMMAND t_p)\n",
        ),
        file("common/val.h", "#define VAL 1\n"),
        file(
            "tools/gen.c",
            "#include <stdio.h>\n#include \"val.h\"\nint main(int argc, char **argv) {\n\
             \x20 FILE *f = fopen(argv[1], \"w\"); if (!f) return 1;\n\
             \x20 fprintf(f, \"#define GEN_VAL %d\\n\", VAL); fclose(f); return 0;\n}\n",
        ),
        file(
            "tests/t_u.c",
            "#include \"gen.h\"\nint main(void) { return GEN_VAL == 1 ? 0 : 1; }\n",
        ),
        file(
            "tests/t_s.c",
            "#include \"val.h\"\nint main(void) { return VAL > 0 ? 0 : 1; }\n",
        ),
        file("tests/t_p.c", "int main(void) { return 0; }\n"),
    ]
}

// covers: C-49, C-30
#[test]
fn a_header_a_program_of_the_project_writes_reads_what_that_program_is_built_from() {
    for cmake in with_cmake() {
        for generator in generators(&cmake) {
            let at = format!("CMake {:?}, {generator}", cmake.version);
            let p = Project::new(&cmake, generator, &generated());
            let first = p.run(CTEST, &[]);
            assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
            // t_u includes gen.h, which gen writes: it reads gen's source and the header gen.c
            // includes, as much as its own source.
            let map = store::map(&p.cache);
            let reads = map["units"]["t_u"]["reads"].clone();
            for file in ["common/val.h", "tests/t_u.c", "tools/gen.c"] {
                assert!(
                    reads.as_array().unwrap().contains(&json!(file)),
                    "{at}: t_u read {reads}"
                );
            }
            // val.h changes, and the build writes another gen.h: t_u runs, and fails.
            p.write("common/val.h", "#define VAL 2\n");
            git::commit_all(&p.repo, "val");
            p.build();
            let second = p.run(CTEST, &[]);
            assert!(
                second.ran().contains(&"u".to_string()),
                "{at}\n{}\n{}",
                second.stdout,
                second.stderr
            );
            assert_ne!(second.code, 0, "{at}\n{}", second.stdout);
        }
    }
}

// covers: C-49, C-39
#[test]
fn a_test_that_failed_runs_alone_and_its_programs_other_tests_are_reused() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    let generator = generators(&cmake)[0];
    let p = Project::new(&cmake, generator, &shop("{mode: reuse, unit: test}"));
    let first = p.run(CTEST, &[("FAIL_CART", "total")]);
    assert_ne!(first.code, 0, "{}", first.stdout);
    let map = store::map(&p.cache);
    assert_eq!(
        map["units"]["t_cart"]["tests"],
        json!({"cart_empty": "passed", "cart_tax": "passed", "cart_total": "failed"}),
        "{map:#}"
    );
    // The failure fixed: t_cart's own key failed, so its tests are asked one by one. Of the two
    // that passed, one runs as a check; of the four programs that passed whole, one too.
    let second = p.run(CTEST, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        second.line("hushrun: unit: "),
        "hushrun: unit: 3 of 7 tests run, in 2 of 5 test files; 4 already passed on the files they read, and 2 more that did run as a check"
    );
    assert!(second.ran().contains(&"cart_total".to_string()));
    let units = &p.newest_result()["units"];
    assert_eq!(units["tests"], json!({"total": 7, "ran": 3, "reused": 4}));
}

// covers: C-49, C-34
#[test]
fn an_audited_program_that_fails_again_alone_suspends_the_checks_reuse() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    let generator = generators(&cmake)[0];
    let p = Project::new(&cmake, generator, &shop("reuse"));
    assert_eq!(p.run(CTEST, &[]).code, 0);
    // greet.h changes; the four other programs could be reused, and one runs as a check. Every
    // one of them fails while FAIL_HIDDEN is set, which no file says: the audited one fails, runs
    // again alone in a second run of its own (whose directory is another, and whose hushrun's
    // ctest finds the plan the first run's wrote), fails again, and the check's reuse stops.
    p.write(
        "src/greet.h",
        "static const char *greet(void) { return \"hey\"; }\n",
    );
    p.build();
    let caught = p.run(CTEST, &[("FAIL_HIDDEN", "1")]);
    assert_ne!(caught.code, 0, "{}", caught.stdout);
    assert!(
        caught
            .stderr
            .contains("though it passed before on the same files it read: running it once more"),
        "{}",
        caught.stderr
    );
    assert!(
        caught.stderr.contains(
            "failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
    // The second run ran the audited program's tests alone: two `Test project` lines, and the
    // second ran one program's tests.
    let projects: Vec<&str> = caught.stdout.split("Test project").collect();
    assert_eq!(projects.len(), 3, "{}", caught.stdout);
    let again_ran = projects[2].matches(" Test #").count();
    assert!((1..=3).contains(&again_ran), "{}", caught.stdout);
    // And nothing is reused until a person turns it on again.
    p.write("src/util.h", "static int twice(int x) { return x + x; }\n");
    p.build();
    let after = p.run(CTEST, &[]);
    assert!(
        after
            .stderr
            .contains("reuse of its test files is suspended"),
        "{}",
        after.stderr
    );
}

// covers: C-49
#[test]
fn hushruns_ctest_leaves_out_what_it_is_told_once_it_checked_each_program() {
    for (cmake, generator) in with_cmake()
        .into_iter()
        .flat_map(|c| generators(&c).into_iter().map(move |g| (c.clone(), g)))
    {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        let p = Project::new(&cmake, generator, &shop("reuse"));
        let run = json!({"unit": "test", "reuse": ["t_greet", "t_util"],
                         "reuseTests": {"t_cart": ["cart_empty"]}});
        // t_util's list in the plan isn't what it is built from now: it runs.
        let mut plan = shop_plan(&["t_greet", "t_cart"]);
        plan["units"]["t_util"] = json!([["src/util.h", "tests/t_util.c", "src/greet.h"]]);
        let shim = p.shim(
            &["--test-dir", "build"],
            run.clone(),
            Some(plan.clone()),
            &[],
        );
        assert_eq!(shim.code, 0, "{at}\n{}", shim.stdout);
        assert_eq!(
            shim.left_out(),
            ["t_cart::cart_empty", "t_greet::greet"],
            "{at}"
        );
        let expected = if cmake.version >= (3, 29) {
            "--exclude-from-file"
        } else {
            "-E"
        };
        assert_eq!(shim.selection(), expected, "{at}");
        assert_eq!(shim.unit("t_greet")["deselected"], json!(true), "{at}");
        assert_eq!(shim.unit("t_cart")["deselected"], json!(false), "{at}");
        assert_eq!(shim.unit("t_cart")["selected"], json!(2), "{at}");
        assert_eq!(shim.unit("t_cart")["finished"], json!(2), "{at}");
        assert_eq!(shim.unit("t_cart")["reads"], json!(reads("t_cart")), "{at}");
        // With no plan's file, or a program built again after it, nothing is left out.
        let none = p.shim(&["--test-dir", "build"], run.clone(), None, &[]);
        assert!(none.left_out().is_empty(), "{at}");
        // A program whose source changed and wasn't built again ran old code: it isn't left out,
        // and none of its tests is observed.
        p.write(
            "src/greet.h",
            "static const char *greet(void) { return \"ho\"; }\n",
        );
        let stale = p.shim(
            &["--test-dir", "build"],
            run.clone(),
            Some(plan.clone()),
            &[],
        );
        assert_eq!(stale.left_out(), ["t_cart::cart_empty"], "{at}");
        assert_eq!(
            stale.unit("t_greet")["notFollowed"][0]["why"],
            json!("stale program"),
            "{at}"
        );
        // Built again before the plan: it is left out; built again after it, it runs.
        p.build();
        let rebuilt = p.shim(
            &["--test-dir", "build"],
            run.clone(),
            Some(plan.clone()),
            &[],
        );
        assert_eq!(
            rebuilt.left_out(),
            ["t_cart::cart_empty", "t_greet::greet"],
            "{at}"
        );
        plan["old"] = json!(true);
        let after = p.shim(&["--test-dir", "build"], run, Some(plan), &[]);
        assert!(after.left_out().is_empty(), "{at}");
    }
}

// covers: C-49
#[test]
fn a_filter_ctest_drops_is_caught_by_listing_again() {
    for cmake in with_cmake().into_iter().filter(|c| c.version < (4, 0)) {
        let generator = generators(&cmake)[0];
        let p = Project::new(&cmake, generator, &shop("reuse"));
        // Ten groups: CMake 3.22 takes nine, and drops the expression it can't compile. The
        // user's -E joined to hushrun's names is dropped too, so its listing gives every test:
        // -R with the names kept is used instead.
        let groups = "(n0)|(n1)|(n2)|(n3)|(n4)|(n5)|(n6)|(n7)|(n8)|(n9)";
        let shim = p.shim(
            &["--test-dir", "build", "-E", groups],
            json!({"unit": "test", "reuse": ["t_greet"]}),
            Some(shop_plan(&["t_greet"])),
            &[],
        );
        assert_eq!(shim.code, 0, "{}", shim.stdout);
        assert_eq!(shim.selection(), "-R");
        assert_eq!(shim.left_out(), ["t_greet::greet"]);
    }
}

// covers: C-49
#[test]
fn a_ctest_that_ignores_every_filter_runs_the_command_unchanged() {
    // A ctest of the test's that lists the same two tests whatever it is told, and writes the
    // arguments it ran with: every way of leaving out fails the check, and nothing is changed.
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let bin = root.join("bin");
    let build = root.join("repo/build");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&build).unwrap();
    std::fs::write(
        build.join("CMakeCache.txt"),
        "CMAKE_GENERATOR:INTERNAL=Ninja\n",
    )
    .unwrap();
    for program in ["t_a", "t_b"] {
        std::fs::write(build.join(program), "").unwrap();
    }
    let listing = json!({"kind": "ctestInfo", "tests": [
        {"name": "a", "command": [build.join("t_a")]},
        {"name": "b", "command": [build.join("t_b")]}]});
    let ran = root.join("ran.txt");
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'ctest version 4.4.5' ;;\n  *--show-only=json-v1*) cat <<'EOF'\n{listing}\nEOF\n ;;\n  *) printf '%s\\n' \"$@\" > '{}' ;;\nesac\n",
        ran.display()
    );
    std::fs::write(bin.join("ctest"), script).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(bin.join("ctest"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let units = root.join("units");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(
        units.join("hushrun.json"),
        json!({"schema": "hushrun.units/v0", "root": root.join("repo"), "reuse": ["t_a"], "unit": "test"})
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        units.join("ctest-plan.v1"),
        json!({"schema": "hushrun.ctest-plan/v1", "units": {"t_a": [[]]}, "scope": []}).to_string(),
    )
    .unwrap();
    let path = std::env::join_paths([bin.clone(), "/usr/bin".into(), "/bin".into()]).unwrap();
    let mut was = std::ffi::OsString::from("=");
    was.push(&path);
    let out = Command::new(env!("CARGO_BIN_EXE_hushrun"))
        .current_dir(root.join("repo"))
        .env("PATH", &path)
        .env("HUSHRUN_UNITS", &units)
        .env("HUSHRUN_PROGRAMS_PATH", was)
        .env("HUSHRUN_NO_UPDATE_CHECK", "1")
        .env("HUSHRUN_HOST", NO_HOST)
        .args(["__hushrun-program", "ctest", "--test-dir", "build", "-j2"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let args = std::fs::read_to_string(&ran).unwrap();
    let args: Vec<&str> = args.lines().collect();
    // Its own arguments, as given, and hushrun's JUnit file: nothing left out.
    assert_eq!(&args[..3], ["--test-dir", "build", "-j2"]);
    assert_eq!(args[3], "--output-junit");
    assert_eq!(args.len(), 5, "{args:?}");
    let facts: Value = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.extension().is_some_and(|e| e == "json") && p.file_name().unwrap() != "hushrun.json"
        })
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .unwrap();
    assert_eq!(
        facts["settings"]["selection"],
        json!("no way of leaving tests out listed exactly the tests expected")
    );
    // No JUnit file came: no test passed.
    for unit in facts["units"].as_array().unwrap() {
        assert_eq!(unit["finished"], json!(0), "{facts:#}");
    }
}

/// A database fixture: a setup, a cleanup and two tests that need them, and one that doesn't.
fn fixtures() -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(".gitignore", "build/\n"),
        file(
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.19)\nproject(db C)\nenable_testing()\n\
             add_executable(t_db t_db.c)\n\
             add_test(NAME db_setup COMMAND t_db setup)\nadd_test(NAME db_query COMMAND t_db query)\n\
             add_test(NAME db_write COMMAND t_db write)\nadd_test(NAME db_cleanup COMMAND t_db cleanup)\n\
             add_test(NAME alone COMMAND t_db alone)\n\
             set_tests_properties(db_setup PROPERTIES FIXTURES_SETUP DB)\n\
             set_tests_properties(db_cleanup PROPERTIES FIXTURES_CLEANUP DB)\n\
             set_tests_properties(db_query db_write PROPERTIES FIXTURES_REQUIRED DB)\n",
        ),
        file(
            "t_db.c",
            "#include <stdio.h>\n#include <string.h>\nint main(int argc, char **argv) {\n\
             \x20 const char *c = argc > 1 ? argv[1] : \"\";\n\
             \x20 if (strcmp(c, \"setup\") == 0) { FILE *f = fopen(\"db.state\", \"w\"); if (!f) return 1; fclose(f); return 0; }\n\
             \x20 if (strcmp(c, \"cleanup\") == 0) { remove(\"db.state\"); return 0; }\n\
             \x20 if (strcmp(c, \"alone\") == 0) return 0;\n\
             \x20 FILE *f = fopen(\"db.state\", \"r\"); if (!f) return 1; fclose(f); return 0;\n}\n",
        ),
    ]
}

// covers: C-49
#[test]
fn a_fixture_a_kept_test_needs_is_never_left_out_and_everything_left_out_passes() {
    for cmake in with_cmake() {
        let generator = generators(&cmake)[0];
        let at = format!("CMake {:?}", cmake.version);
        let p = Project::new(&cmake, generator, &fixtures());
        let plan = json!({"units": {"t_db": [["t_db.c"]]}, "scope": ["t_db.c"]});
        // db_write runs, so its setup and cleanup do; db_query and alone are left out.
        let shim = p.shim(
            &["--test-dir", "build"],
            json!({"unit": "test",
                   "reuseTests": {"t_db": ["db_setup", "db_query", "db_cleanup", "alone"]}}),
            Some(plan.clone()),
            &[],
        );
        assert_eq!(shim.code, 0, "{at}\n{}", shim.stdout);
        assert_eq!(shim.left_out(), ["t_db::alone", "t_db::db_query"], "{at}");
        assert_eq!(shim.unit("t_db")["finished"], json!(3), "{at}");
        assert_eq!(shim.unit("t_db")["failed"], json!(false), "{at}");
        // Everything left out: ctest runs nothing, and a pass stays a pass, even where the user
        // asked an empty run to fail.
        let all = p.shim(
            &["--test-dir", "build", "--no-tests=error"],
            json!({"unit": "test", "reuse": ["t_db"]}),
            Some(plan.clone()),
            &[],
        );
        assert_eq!(all.code, 0, "{at}\n{}", all.stdout);
        assert_eq!(all.left_out().len(), 5, "{at}");
        assert_eq!(all.unit("t_db")["deselected"], json!(true), "{at}");
        assert!(!all.stdout.contains("Test #"), "{at}\n{}", all.stdout);
        // --rerun-failed picks tests by position: hushrun only observes.
        let rerun = p.shim(
            &["--test-dir", "build", "--rerun-failed"],
            json!({"unit": "test", "reuse": ["t_db"]}),
            Some(plan),
            &[],
        );
        assert!(rerun.left_out().is_empty(), "{at}");
        assert_eq!(rerun.selection(), "observed only", "{at}");
    }
}

// covers: C-49
#[test]
fn discovered_googletest_tests_are_named_by_their_filter_on_every_cmake() {
    let program = "#include <stdio.h>\n#include <string.h>\n\
        int main(int argc, char **argv) {\n\
        \x20 for (int i = 1; i < argc; i++) if (strcmp(argv[i], \"--gtest_list_tests\") == 0) {\n\
        \x20   printf(\"Calc.\\n  Adds\\n  Subs\\nTyped/0.  # TypeParam = int\\n  Works\\nTyped/1.  # TypeParam = double\\n  Works\\n\");\n\
        \x20   return 0;\n  }\n\
        \x20 for (int i = 1; i < argc; i++) if (strncmp(argv[i], \"--gtest_filter=\", 15) == 0) {\n\
        \x20   printf(\"[ RUN      ] %s\\n[       OK ] %s (0 ms)\\n\", argv[i] + 15, argv[i] + 15);\n\
        \x20   return 0;\n  }\n  return 0;\n}\n";
    let mut ids = Vec::new();
    for cmake in with_cmake() {
        let generator = generators(&cmake)[0];
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("{mode: observe, unit: test}")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(gt C)\nenable_testing()\ninclude(GoogleTest)\n\
                     add_executable(fake_gtest fake_gtest.c)\ngtest_discover_tests(fake_gtest)\n",
                ),
                file("fake_gtest.c", program),
            ],
        );
        let ran = p.run(CTEST, &[]);
        assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
        let map = store::map(&p.cache);
        ids.push((cmake.version, map["units"]["fake_gtest"]["tests"].clone()));
    }
    for (version, tests) in &ids {
        assert_eq!(
            *tests,
            json!({"Calc.Adds": "passed", "Calc.Subs": "passed",
                   "Typed/0.Works": "passed", "Typed/1.Works": "passed"}),
            "CMake {version:?}"
        );
    }
}

// covers: C-49, C-32
#[test]
fn the_forms_that_bypass_path_say_so_and_a_shell_line_is_observed() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    let generator = generators(&cmake)[0];
    let p = Project::new(&cmake, generator, &shop("observe"));
    let bypassed = p.run(
        &[
            "run", "unit", "--", "cmake", "--build", "build", "--target", "test",
        ],
        &[],
    );
    assert_eq!(bypassed.code, 0, "{}", bypassed.stderr);
    assert!(
        bypassed.stderr.contains(
            "hushrun: unit: test files aren't observed: make test, ninja test and cmake --build … --target test start ctest by its full path"
        ),
        "{}",
        bypassed.stderr
    );
    let line = p.run(
        &["run", "unit", "--", "sh", "-c", "cd build && ctest -j2"],
        &[],
    );
    assert_eq!(line.code, 0, "{}", line.stderr);
    assert_eq!(
        line.line("hushrun: unit: with `tests: reuse`"),
        "hushrun: unit: with `tests: reuse`, all 5 test files would have run: none had passed here yet on the files it read"
    );
    // A stale program is observed as one: it ran old code.
    let p = Project::new(&cmake, generator, &shop("observe"));
    p.write("src/util.h", "static int twice(int x) { return x << 1; }\n");
    let stale = p.run(
        &[
            "run",
            "--force",
            "unit",
            "--",
            "ctest",
            "--test-dir",
            "build",
        ],
        &[],
    );
    assert_eq!(stale.code, 0, "{}", stale.stderr);
    let map = store::map(&p.cache);
    assert_eq!(
        map["units"]["t_util"]["notFollowed"],
        json!(["stale program"]),
        "{map:#}"
    );
    assert!(map["units"]["t_greet"]["notFollowed"].is_null(), "{map:#}");
}
