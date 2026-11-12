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
    /// The build directory, from the repository.
    build_dir: String,
}

impl Project {
    /// A repository of `files`, configured in `build` with `generator` and built.
    fn new(cmake: &Cmake, generator: &str, files: &[(String, String)]) -> Project {
        Project::new_in(cmake, generator, files, "build")
    }

    /// [`Project::new`], built in `build_dir`.
    fn new_in(
        cmake: &Cmake,
        generator: &str,
        files: &[(String, String)],
        build_dir: &str,
    ) -> Project {
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
            build_dir: build_dir.to_string(),
        };
        for (path, text) in files {
            p.write(path, text);
        }
        git::commit_all(&p.repo, "init");
        let mut configure = p.tool("cmake");
        configure.args(["-S", ".", "-B", build_dir, "-G", generator]);
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
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        cmd
    }

    fn build(&self) {
        let out = self
            .tool("cmake")
            .args(["--build", &self.build_dir])
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
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
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

// covers: C-49, C-30
#[test]
fn a_program_or_a_file_a_test_names_is_read_by_its_test_program() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        let driver = "#include <unistd.h>\n#include <stdlib.h>\nint main(int argc, char **argv) {\n\
            \x20 const char *p = argc > 1 ? argv[1] : getenv(\"SERVER\");\n\
            \x20 if (!p) return 0;\n  execv(p, (char *const[]){(char *)p, 0}); return 2;\n}\n";
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("reuse")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(drv C)\nenable_testing()\n\
                     file(WRITE ${CMAKE_BINARY_DIR}/data.txt \"x\")\n\
                     add_executable(server tools/server.c)\ntarget_include_directories(server PRIVATE common)\n\
                     add_executable(other tools/other.c)\ntarget_include_directories(other PRIVATE common)\n\
                     foreach(t t_driver t_envdriver)\n  add_executable(${t} tests/driver.c)\nendforeach()\n\
                     add_executable(t_dirdriver tests/t_data.c)\n\
                     add_executable(t_data tests/t_data.c)\nadd_executable(t_input tests/t_input.c)\n\
                     add_test(NAME integ COMMAND t_driver $<TARGET_FILE:server>)\n\
                     add_test(NAME env COMMAND t_envdriver)\n\
                     set_tests_properties(env PROPERTIES ENVIRONMENT \"SERVER=$<TARGET_FILE:other>\")\n\
                     add_test(NAME dir COMMAND t_dirdriver --dir=${CMAKE_BINARY_DIR})\n\
                     add_test(NAME data COMMAND t_data ${CMAKE_BINARY_DIR}/data.txt)\n\
                     add_test(NAME input COMMAND t_input ${CMAKE_SOURCE_DIR}/tests/input.c)\n",
                ),
                file("common/val.h", "#define VAL 1\n"),
                file(
                    "tools/server.c",
                    "#include \"val.h\"\nint main(void) { return VAL == 1 ? 0 : 1; }\n",
                ),
                file(
                    "tools/other.c",
                    "#include \"val.h\"\nint main(void) { return VAL == 1 ? 0 : 1; }\n",
                ),
                file("tests/driver.c", driver),
                file("tests/t_data.c", "int main(void) { return 0; }\n"),
                file("tests/t_input.c", "int main(void) { return 0; }\n"),
                file("tests/input.c", "int unused;\n"),
            ],
        );
        let first = p.run(CTEST, &[]);
        assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
        let map = store::map(&p.cache);
        let reads = |unit: &str| -> Vec<String> {
            map["units"][unit]["reads"]
                .as_array()
                .unwrap_or_else(|| panic!("{at}: {unit}: {map:#}"))
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        // A program a test starts by its path on the command line, or from a variable its test
        // sets, is read by the test's program.
        assert_eq!(
            reads("t_driver"),
            ["common/val.h", "tests/driver.c", "tools/server.c"],
            "{at}"
        );
        assert_eq!(
            reads("t_envdriver"),
            ["common/val.h", "tests/driver.c", "tools/other.c"],
            "{at}"
        );
        // The build directory itself: any program in it may start, and every source is read;
        // each program the build makes there is up to date, so the test's program is followed.
        assert!(
            reads("t_dirdriver").contains(&"tools/server.c".to_string()),
            "{at}: {map:#}"
        );
        assert!(
            map["units"]["t_dirdriver"]["notFollowed"]
                .as_array()
                .is_none_or(|n| n.is_empty()),
            "{at}: {map:#}"
        );
        // A file no rule builds is no program; a file of the repository is read.
        assert_eq!(reads("t_data"), ["tests/t_data.c"], "{at}");
        assert_eq!(
            reads("t_input"),
            ["tests/input.c", "tests/t_input.c"],
            "{at}"
        );
    }
}

// covers: C-49
#[test]
fn a_test_presets_repeat_index_and_show_only_are_read() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    let generator = generators(&cmake)[0];
    let mut files = shop("reuse");
    files.push(file(
        "CMakePresets.json",
        r#"{"version": 3,
            "configurePresets": [{"name": "dev", "binaryDir": "${sourceDir}/build"}],
            "testPresets": [
              {"name": "repeat", "configurePreset": "dev",
               "execution": {"repeat": {"mode": "until-pass", "count": 3}}},
              {"name": "index", "configurePreset": "dev",
               "filter": {"include": {"index": {"start": 1, "end": 7}}}},
              {"name": "show", "configurePreset": "dev", "execution": {"showOnly": "human"}}
            ]}"#,
    ));
    let p = Project::new(&cmake, generator, &files);
    // until-pass in the preset: a pass may have come after a failure, as with --repeat.
    let repeat = p.shim(&["--preset", "repeat"], json!({}), None, &[]);
    assert_eq!(repeat.code, 0, "{}", repeat.stdout);
    assert_eq!(
        repeat.unit("t_greet")["notFollowed"][0]["why"],
        json!("repeat until-pass"),
        "{:#}",
        repeat.facts
    );
    // A command line's --repeat replaces the preset's.
    let once = p.shim(
        &["--preset", "repeat", "--repeat", "until-fail:1"],
        json!({}),
        None,
        &[],
    );
    assert!(
        once.unit("t_greet")["notFollowed"]
            .as_array()
            .is_none_or(|n| n.is_empty()),
        "{:#}",
        once.facts
    );
    // The preset picks tests by their position: nothing is left out.
    let index = p.shim(
        &["--preset", "index"],
        json!({"unit": "test", "reuse": ["t_greet"]}),
        Some(shop_plan(&["t_greet"])),
        &[],
    );
    assert_eq!(index.code, 0, "{}", index.stdout);
    assert!(index.left_out().is_empty(), "{:#}", index.facts);
    assert_eq!(index.selection(), "observed only", "{:#}", index.facts);
    // The preset only shows the tests: no test runs, and nothing is observed.
    let show = p.shim(&["--preset", "show"], json!({}), None, &[]);
    assert!(
        show.facts["settings"]["notObserved"]
            .as_str()
            .is_some_and(|w| w.contains("showOnly")),
        "{:#}",
        show.facts
    );
}

// covers: C-49
#[test]
fn hushruns_listings_write_no_file_a_preset_or_the_command_line_names() {
    use std::os::unix::fs::PermissionsExt as _;
    for cmake in with_cmake() {
        let generator = generators(&cmake)[0];
        let at = format!("CMake {:?}", cmake.version);
        let mut files = shop("reuse");
        // A preset's JUnit file came in version 6 (CMake 3.25).
        let (version, junit) = if cmake.version >= (3, 25) {
            (6, r#", "outputJUnitFile": "pj.xml""#)
        } else {
            (3, "")
        };
        files.push(file(
            "CMakePresets.json",
            &format!(
                r#"{{"version": {version},
                "configurePresets": [{{"name": "dev", "binaryDir": "${{sourceDir}}/build"}}],
                "testPresets": [{{"name": "log", "configurePreset": "dev",
                  "output": {{"outputLogFile": "mylog.txt"{junit}}}}}]}}"#
            ),
        ));
        let p = Project::new(&cmake, generator, &files);
        // The user's own log and JUnit file from before.
        p.write("mylog.txt", "mine\n");
        p.write("build/pj.xml", "<mine/>\n");
        // A ctest in front of the real one that says when, as the run starts, a file the user
        // reads isn't as it was before hushrun's first listing.
        let wrap = p.root.join("wrap");
        std::fs::create_dir_all(&wrap).unwrap();
        let marker = p.root.join("changed.txt");
        let first = p.root.join("first.txt");
        let watched = "mylog.txt build/mylog.txt pj.xml build/pj.xml";
        let script = format!(
            "#!/bin/sh\nsum() {{ for f in {watched}; do cksum \"$f\" 2>/dev/null; done; }}\n\
             [ -f '{first}' ] || sum > '{first}'\n\
             case \"$*\" in *--show-only*|*--version*) ;; *) sum | cmp -s - '{first}' || echo \"$*\" >> '{marker}' ;; esac\n\
             exec '{real}' \"$@\"\n",
            real = cmake.bin.join("ctest").display(),
            marker = marker.display(),
            first = first.display()
        );
        std::fs::write(wrap.join("ctest"), script).unwrap();
        std::fs::set_permissions(wrap.join("ctest"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let mut was = std::ffi::OsString::from("=");
        was.push(wrap.as_os_str());
        was.push(":");
        was.push(&p.path);
        let was = was.into_string().unwrap();
        let shim = p.shim(
            &["--preset", "log", "-O", "cli.log"],
            json!({"unit": "test", "reuse": ["t_greet"]}),
            Some(shop_plan(&["t_greet"])),
            &[("HUSHRUN_PROGRAMS_PATH", was.as_str())],
        );
        assert_eq!(shim.code, 0, "{at}\n{}", shim.stdout);
        assert_eq!(shim.left_out(), ["t_greet::greet"], "{at}");
        assert!(
            !marker.exists(),
            "{at}: a listing changed a file before: {}",
            std::fs::read_to_string(&marker).unwrap_or_default()
        );
    }
}

// covers: C-49
#[test]
fn an_in_source_build_says_why_nothing_is_reused() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        let mut files = shop("reuse");
        files.retain(|(path, _)| path != ".gitignore");
        files.push(file(
            ".gitignore",
            "CMakeFiles/\nCMakeCache.txt\n*.cmake\nMakefile\n*.ninja\n.ninja_*\nTesting/\nlib*.a\nt_*\n!tests/\n",
        ));
        let p = Project::new_in(&cmake, generator, &files, ".");
        let first = p.run(&["run", "unit", "--", "ctest"], &[]);
        assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
        // Its build directory is the repository: what each program is built from can't be told
        // apart from the repository's files, and the run says so, once.
        let said = first
            .stderr
            .lines()
            .filter(|l| l.contains("is the repository's: what it is built from can't be told"))
            .count();
        assert_eq!(said, 1, "{at}\n{}", first.stderr);
        assert!(
            first
                .stderr
                .contains("hushrun: unit: test files weren't observed: its build directory"),
            "{at}\n{}",
            first.stderr
        );
    }
}

// covers: C-49, C-32
#[test]
fn a_skipped_test_in_a_run_that_failed_records_no_pass() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    let generator = generators(&cmake)[0];
    let p = Project::new(
        &cmake,
        generator,
        &[
            file("hushrun.yaml", &yaml("reuse")),
            file(".gitignore", "build/\n"),
            file(
                "CMakeLists.txt",
                "cmake_minimum_required(VERSION 3.19)\nproject(skip C)\nenable_testing()\n\
                 add_executable(t_skip tests/t_skip.c)\nadd_executable(t_fail tests/t_fail.c)\n\
                 add_executable(t_ok tests/t_ok.c)\n\
                 add_test(NAME skip COMMAND t_skip)\nadd_test(NAME fail COMMAND t_fail)\nadd_test(NAME ok COMMAND t_ok)\n\
                 set_tests_properties(skip PROPERTIES SKIP_RETURN_CODE 77)\n",
            ),
            file("tests/t_skip.c", "int main(void) { return 77; }\n"),
            file("tests/t_fail.c", "int main(void) { return 1; }\n"),
            file("tests/t_ok.c", "int main(void) { return 0; }\n"),
        ],
    );
    let ran = p.run(CTEST, &[]);
    assert_ne!(ran.code, 0, "{}", ran.stdout);
    // t_fail failed and t_ok passed; t_skip's test was skipped in a run that failed, which
    // proves nothing: it records no pass.
    assert_eq!(
        unit_results(&p.cache),
        ["fail", "pass"],
        "{}\n{}",
        ran.stdout,
        ran.stderr
    );
}

// covers: C-49, C-30
#[test]
fn a_source_no_program_is_built_from_is_in_the_rest_set() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        // t_case reads cases/one.c at run time, by a path it was built with, which no build
        // edge names; t_all names the repository's cases directory, and reads every source; a
        // custom command no test program depends on compiles tools/embed.c; include_copy/x.h is
        // a header nothing includes.
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("reuse")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(own C)\nenable_testing()\n\
                     add_executable(t_case tests/t_case.c)\n\
                     target_compile_definitions(t_case PRIVATE CASE=\"${CMAKE_SOURCE_DIR}/cases/one.c\")\n\
                     add_executable(t_all tests/t_all.c)\nadd_executable(t_p tests/t_p.c)\n\
                     add_custom_command(OUTPUT ${CMAKE_BINARY_DIR}/embed.txt COMMAND ${CMAKE_COMMAND} -E copy ${CMAKE_SOURCE_DIR}/tools/embed.c ${CMAKE_BINARY_DIR}/embed.txt DEPENDS tools/embed.c)\n\
                     add_custom_target(embed ALL DEPENDS ${CMAKE_BINARY_DIR}/embed.txt)\n\
                     add_test(NAME case COMMAND t_case)\nadd_test(NAME all COMMAND t_all ${CMAKE_SOURCE_DIR}/cases)\n\
                     add_test(NAME p COMMAND t_p)\n",
                ),
                file("cases/one.c", "int one = 1;\n"),
                file("tools/embed.c", "int embed;\n"),
                file("include_copy/x.h", "int x;\n"),
                file(
                    "tests/t_case.c",
                    "#include <stdio.h>\n#include <string.h>\nint main(void) {\n\
                     \x20 char line[64] = {0}; FILE *f = fopen(CASE, \"r\"); if (!f) return 1;\n\
                     \x20 fgets(line, sizeof line, f); fclose(f); return strcmp(line, \"int one = 1;\\n\") != 0;\n}\n",
                ),
                file("tests/t_all.c", "int main(void) { return 0; }\n"),
                file("tests/t_p.c", "int main(void) { return 0; }\n"),
            ],
        );
        let first = p.run(CTEST, &[]);
        assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
        // No program's build gives these files to it: they are in the rest set, so a change to
        // one runs every program; t_all reading every source doesn't make them sources.
        let map = store::map(&p.cache);
        for path in ["cases/one.c", "tools/embed.c", "include_copy/x.h"] {
            assert!(map["rest"][path].is_string(), "{at}: {path}: {map:#}");
        }
        assert!(map["rest"]["tests/t_case.c"].is_null(), "{at}: {map:#}");
        // cases/one.c changes: t_case runs, and fails.
        p.write("cases/one.c", "int one = 2;\n");
        let second = p.run(CTEST, &[]);
        assert!(
            second.ran().contains(&"case".to_string()),
            "{at}\n{}\n{}",
            second.stdout,
            second.stderr
        );
        assert_ne!(second.code, 0, "{at}");
    }
}

// covers: C-49, C-30
#[test]
fn a_custom_commands_depfile_is_read() {
    for (cmake, generator) in with_cmake()
        .into_iter()
        .flat_map(|c| generators(&c).into_iter().map(move |g| (c.clone(), g)))
    {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        // tools/gen.sh reads inc/dep.h, which only its depfile names, to write gen_dep.h, which
        // t_dep includes; t_s includes dep.h, so dep.h is a source.
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("reuse")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.20)\nproject(dep C)\nenable_testing()\n\
                     include_directories(${CMAKE_SOURCE_DIR}/inc)\n\
                     add_custom_command(OUTPUT ${CMAKE_BINARY_DIR}/gen_dep.h\n\
                     \x20 COMMAND sh ${CMAKE_SOURCE_DIR}/tools/gen.sh ${CMAKE_SOURCE_DIR}/inc/dep.h ${CMAKE_BINARY_DIR}/gen_dep.h ${CMAKE_BINARY_DIR}/gen_dep.d\n\
                     \x20 DEPENDS ${CMAKE_SOURCE_DIR}/tools/gen.sh DEPFILE ${CMAKE_BINARY_DIR}/gen_dep.d)\n\
                     add_executable(t_dep tests/t_dep.c ${CMAKE_BINARY_DIR}/gen_dep.h)\n\
                     target_include_directories(t_dep PRIVATE ${CMAKE_BINARY_DIR})\n\
                     add_executable(t_s tests/t_s.c)\nadd_executable(t_p tests/t_p.c)\n\
                     add_test(NAME dep COMMAND t_dep)\nadd_test(NAME s COMMAND t_s)\nadd_test(NAME p COMMAND t_p)\n",
                ),
                file("inc/dep.h", "#define DEP 1\n"),
                file(
                    "tools/gen.sh",
                    "v=$(sed -n 's/#define DEP \\([0-9]*\\)/\\1/p' \"$1\")\necho \"#define GEN_DEP $v\" > \"$2\"\necho \"$2: $1\" > \"$3\"\n",
                ),
                file(
                    "tests/t_dep.c",
                    "#include \"gen_dep.h\"\nint main(void) { return GEN_DEP == 1 ? 0 : 1; }\n",
                ),
                file(
                    "tests/t_s.c",
                    "#include \"dep.h\"\nint main(void) { return DEP > 0 ? 0 : 1; }\n",
                ),
                file("tests/t_p.c", "int main(void) { return 0; }\n"),
            ],
        );
        let first = p.run(CTEST, &[]);
        assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
        let map = store::map(&p.cache);
        let reads = map["units"]["t_dep"]["reads"].clone();
        assert!(
            reads.as_array().unwrap().contains(&json!("inc/dep.h")),
            "{at}: t_dep read {reads}"
        );
        p.write("inc/dep.h", "#define DEP 2\n");
        p.build();
        let second = p.run(CTEST, &[]);
        assert!(
            second.ran().contains(&"dep".to_string()),
            "{at}\n{}",
            second.stdout
        );
        assert_ne!(second.code, 0, "{at}");
    }
}

// covers: C-49
#[test]
fn on_makefiles_a_source_newer_than_its_object_is_a_stale_program() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    if which("make").is_none() {
        return;
    }
    let p = Project::new(&cmake, "Unix Makefiles", &shop("reuse"));
    // tests/t_misc.c was saved while the build ran: newer than its object, older than the
    // program. make builds the object again; the program ran old code.
    let touch = |stamp: &str, path: &str| {
        let made = Command::new("touch")
            .args(["-t", stamp])
            .arg(p.repo.join(path))
            .status()
            .unwrap();
        assert!(made.success(), "{path}");
    };
    touch(
        "202001010000",
        "build/CMakeFiles/t_misc.dir/tests/t_misc.c.o",
    );
    touch("202101010000", "tests/t_misc.c");
    touch("202201010000", "build/t_misc");
    let shim = p.shim(&["--test-dir", "build"], json!({}), None, &[]);
    assert_eq!(
        shim.unit("t_misc")["notFollowed"][0]["why"],
        json!("stale program"),
        "{:#}",
        shim.facts
    );
    assert!(
        shim.unit("t_greet")["notFollowed"]
            .as_array()
            .is_none_or(|n| n.is_empty()),
        "{:#}",
        shim.facts
    );
}

// covers: C-49, C-32
#[test]
fn a_pass_on_a_program_older_than_its_sources_is_not_recorded_until_it_is_built() {
    for cmake in with_cmake() {
        for generator in generators(&cmake) {
            let at = format!("CMake {:?} {generator}", cmake.version);
            let p = Project::new(&cmake, generator, &shop("reuse"));
            let first = p.run(CTEST, &[]);
            assert_eq!(first.code, 0, "{at}: {}", first.stderr);
            // util.h changes, and nothing builds: t_util's test runs on the program as it was.
            p.write("src/util.h", "static int twice(int x) { return x + x; }\n");
            let stale = p.run(CTEST, &[]);
            assert_eq!(stale.code, 0, "{at}: {}", stale.stderr);
            let said: Vec<&str> = stale
                .stderr
                .lines()
                .filter(|l| l.starts_with("hushrun: unit passed, but it isn't recorded: "))
                .collect();
            assert_eq!(said.len(), 1, "{at}: {}", stale.stderr);
            assert!(
                said[0].contains("t_util") && said[0].contains("build before ctest"),
                "{at}: {}",
                said[0]
            );
            assert!(
                !stale.stderr.contains("won't run again"),
                "{at}: {}",
                stale.stderr
            );
            // The same code again: nothing was recorded, so it runs again, and says so again.
            let again = p.run(CTEST, &[]);
            assert!(
                !again.stderr.contains("unit skipped"),
                "{at}: {}",
                again.stderr
            );
            assert!(
                again
                    .stderr
                    .contains("hushrun: unit passed, but it isn't recorded: "),
                "{at}: {}",
                again.stderr
            );
            // Built: the next run records as usual, and the one after skips.
            p.build();
            let built = p.run(CTEST, &[]);
            assert!(
                built.stderr.contains("it won't run again on this code")
                    && !built.stderr.contains("isn't recorded"),
                "{at}: {}",
                built.stderr
            );
            assert!(p.run(CTEST, &[]).stderr.contains("unit skipped"), "{at}");
            // A stale program none of whose tests ran in this start says nothing of it: greet.h
            // changes, and only util's test runs.
            p.write(
                "src/greet.h",
                "static const char *greet(void) { return \"hey\"; }\n",
            );
            let other = p.run(
                &[
                    "run",
                    "unit",
                    "--",
                    "ctest",
                    "--test-dir",
                    "build",
                    "-R",
                    "util",
                ],
                &[],
            );
            assert_eq!(other.code, 0, "{at}: {}", other.stderr);
            assert!(
                other.stderr.contains("it won't run again on this code")
                    && !other.stderr.contains("isn't recorded"),
                "{at}: {}",
                other.stderr
            );
        }
    }
}

/// The lines of a run that say its pass isn't recorded.
fn not_recorded(ran: &Ran) -> Vec<&str> {
    ran.stderr
        .lines()
        .filter(|l| l.starts_with("hushrun: unit passed, but it isn't recorded: "))
        .collect()
}

/// Sets a file's modification time.
fn set_time(path: &Path, time: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(time))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

// covers: C-49, C-32
#[test]
fn a_header_a_target_writes_on_every_build_leaves_the_program_that_includes_it_up_to_date() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("reuse")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(stamp C)\nenable_testing()\n\
                     add_custom_target(stamp ALL COMMAND ${CMAKE_COMMAND} -DOUT=${CMAKE_BINARY_DIR}/stamp.h -P ${CMAKE_SOURCE_DIR}/stamp.cmake BYPRODUCTS ${CMAKE_BINARY_DIR}/stamp.h)\n\
                     add_executable(t_stamp tests/t_stamp.c)\ntarget_include_directories(t_stamp PRIVATE ${CMAKE_BINARY_DIR})\nadd_dependencies(t_stamp stamp)\n\
                     add_executable(t_p tests/t_p.c)\n\
                     add_test(NAME stamp COMMAND t_stamp)\nadd_test(NAME p COMMAND t_p)\n",
                ),
                file(
                    "stamp.cmake",
                    "string(TIMESTAMP T \"%s\")\nfile(WRITE ${OUT} \"#define STAMP ${T}\\n\")\n",
                ),
                file(
                    "tests/t_stamp.c",
                    "#include \"stamp.h\"\nint main(void) { return STAMP > 0 ? 0 : 1; }\n",
                ),
                file("tests/t_p.c", "int main(void) { return 0; }\n"),
            ],
        );
        // As people run it: build, then test. The target runs on every build, and writes the
        // header again; the program is built again after it, and is up to date.
        p.build();
        let ran = p.run(CTEST, &[]);
        assert_eq!(ran.code, 0, "{at}: {}", ran.stderr);
        assert!(not_recorded(&ran).is_empty(), "{at}: {}", ran.stderr);
        assert!(
            ran.stderr.contains("it won't run again on this code"),
            "{at}: {}",
            ran.stderr
        );
    }
}

// covers: C-49, C-32
#[test]
fn a_program_in_a_build_directory_a_test_names_is_judged_up_to_date_by_its_own_files() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("reuse")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(viadir C)\nenable_testing()\n\
                     add_executable(helper tools/helper.c)\n\
                     set_target_properties(helper PROPERTIES RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)\n\
                     add_executable(t_run tests/t_run.c)\nadd_test(NAME viadir COMMAND t_run ${CMAKE_BINARY_DIR}/bin)\n\
                     add_executable(t_p tests/t_p.c)\nadd_test(NAME p COMMAND t_p)\n",
                ),
                file("tools/helper.c", "int main(void) { return 0; }\n"),
                file(
                    "tests/t_run.c",
                    "#include <stdio.h>\n#include <stdlib.h>\n\
                     int main(int argc, char **argv) {\n  char line[4096];\n\
                     \x20 snprintf(line, sizeof line, \"%s/helper\", argc > 1 ? argv[1] : \".\");\n  return system(line);\n}\n",
                ),
                file("tests/t_p.c", "int main(void) { return 0; }\n"),
            ],
        );
        let first = p.run(CTEST, &[]);
        assert_eq!(first.code, 0, "{at}: {}", first.stderr);
        assert!(not_recorded(&first).is_empty(), "{at}: {}", first.stderr);
        // The helper the test starts from the directory it names changes, and nothing builds:
        // the test ran the helper as it was.
        p.write("tools/helper.c", "int main(void) { return 0; } /* 2 */\n");
        let stale = p.run(CTEST, &[]);
        assert_eq!(stale.code, 0, "{at}: {}", stale.stderr);
        let said = not_recorded(&stale);
        assert_eq!(said.len(), 1, "{at}: {}", stale.stderr);
        assert!(
            said[0].contains("tools/helper.c")
                && said[0].contains("helper.c.o")
                && said[0].ends_with("build before ctest"),
            "{at}: {}",
            said[0]
        );
        // Built, it records.
        p.build();
        let built = p.run(CTEST, &[]);
        assert!(not_recorded(&built).is_empty(), "{at}: {}", built.stderr);
        assert!(
            built.stderr.contains("it won't run again on this code"),
            "{at}: {}",
            built.stderr
        );
    }
}

// covers: C-49, C-32
#[test]
fn a_source_saved_in_the_second_its_object_was_made_says_what_cures_it() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        let p = Project::new(&cmake, generator, &shop("reuse"));
        // tests/t_misc.c was saved after its object was made, in the same second; the program
        // was linked later. The program ran old code. Ninja compares the times whole, and
        // builds it again; make 3.81 compares whole seconds, and never does.
        let second = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 100;
        let at_ms =
            |ms: u64| std::time::UNIX_EPOCH + std::time::Duration::from_millis(second * 1000 + ms);
        set_time(
            &p.repo.join("build/CMakeFiles/t_misc.dir/tests/t_misc.c.o"),
            at_ms(200),
        );
        set_time(&p.repo.join("tests/t_misc.c"), at_ms(700));
        set_time(&p.repo.join("build/t_misc"), at_ms(5000));
        let ran = p.run(CTEST, &[]);
        assert_eq!(ran.code, 0, "{at}: {}", ran.stderr);
        let said = not_recorded(&ran);
        assert_eq!(said.len(), 1, "{at}: {}", ran.stderr);
        assert!(
            said[0].contains("t_misc ran old code: tests/t_misc.c changed after CMakeFiles/t_misc.dir/tests/t_misc.c.o was made"),
            "{at}: {}",
            said[0]
        );
        if generator == "Ninja" {
            assert!(
                said[0].ends_with("; build before ctest") && !said[0].contains("touch"),
                "{at}: {}",
                said[0]
            );
            p.build();
            let built = p.run(CTEST, &[]);
            assert!(not_recorded(&built).is_empty(), "{at}: {}", built.stderr);
        } else {
            assert!(
                said[0].ends_with(
                    "in the same second: make compares whole seconds, and won't make it again; touch tests/t_misc.c, then build before ctest"
                ),
                "{at}: {}",
                said[0]
            );
        }
    }
}

// covers: C-49, C-30
#[test]
fn a_fixture_whose_program_isnt_built_here_adds_only_what_its_command_names() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        // As the reviewer's project: libraries, programs that link them, a fixture made and
        // removed by `cmake -E`, and one set up by a script of the repository.
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("reuse")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(fxe C)\nenable_testing()\n\
                     foreach(l core fmt util)\n  add_library(${l} STATIC lib/${l}.c)\nendforeach()\n\
                     add_test(NAME tmp_setup COMMAND ${CMAKE_COMMAND} -E make_directory ${CMAKE_BINARY_DIR}/tmp)\n\
                     add_test(NAME tmp_cleanup COMMAND ${CMAKE_COMMAND} -E rm -rf ${CMAKE_BINARY_DIR}/tmp)\n\
                     set_tests_properties(tmp_setup PROPERTIES FIXTURES_SETUP tmp)\n\
                     set_tests_properties(tmp_cleanup PROPERTIES FIXTURES_CLEANUP tmp)\n\
                     add_test(NAME seed COMMAND ${CMAKE_SOURCE_DIR}/tests/seed.sh)\n\
                     set_tests_properties(seed PROPERTIES FIXTURES_SETUP seed)\n\
                     foreach(t core fmt fmt2 util plain seeded)\n  add_executable(t_${t} tests/t_${t}.c)\n  add_test(NAME ${t} COMMAND t_${t})\nendforeach()\n\
                     target_link_libraries(t_core PRIVATE core)\ntarget_link_libraries(t_fmt PRIVATE fmt)\n\
                     target_link_libraries(t_fmt2 PRIVATE fmt)\ntarget_link_libraries(t_util PRIVATE util)\n\
                     set_tests_properties(fmt2 plain PROPERTIES FIXTURES_REQUIRED tmp)\n\
                     set_tests_properties(seeded PROPERTIES FIXTURES_REQUIRED seed)\n",
                ),
                file("lib/core.c", "int core(void) { return 0; }\n"),
                file("lib/fmt.c", "int fmt(void) { return 0; }\n"),
                file("lib/util.c", "int util(void) { return 0; }\n"),
                file("tests/seed.sh", "#!/bin/sh\nexit 0\n"),
                file(
                    "tests/t_core.c",
                    "int core(void);\nint main(void) { return core(); }\n",
                ),
                file(
                    "tests/t_fmt.c",
                    "int fmt(void);\nint main(void) { return fmt(); }\n",
                ),
                file(
                    "tests/t_fmt2.c",
                    "int fmt(void);\nint main(void) { return fmt(); }\n",
                ),
                file(
                    "tests/t_util.c",
                    "int util(void);\nint main(void) { return util(); }\n",
                ),
                file("tests/t_plain.c", "int main(void) { return 0; }\n"),
                file("tests/t_seeded.c", "int main(void) { return 0; }\n"),
            ],
        );
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                p.repo.join("tests/seed.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let first = p.run(CTEST, &[]);
        assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
        let map = store::map(&p.cache);
        let reads = |unit: &str| map["units"][unit]["reads"].clone();
        // `cmake -E` names nothing of the repository: its tests read only their own programs.
        assert_eq!(reads("t_plain"), json!(["tests/t_plain.c"]), "{at}");
        assert_eq!(
            reads("t_fmt2"),
            json!(["lib/fmt.c", "tests/t_fmt2.c"]),
            "{at}"
        );
        // A script of the repository the setup runs is read too; of no source kind, it is in
        // the rest set, so a change to it runs everything.
        assert_eq!(reads("t_seeded"), json!(["tests/t_seeded.c"]), "{at}");
        // A change to one library runs only the programs that link it, and the fixture they
        // need.
        p.write("lib/fmt.c", "int fmt(void) { return 0 * 2; }\n");
        git::commit_all(&p.repo, "fmt");
        p.build();
        // Of the eight test files (six programs, `cmake` and the script, which always run), the
        // two that link it run, and one of the four reused runs as a check.
        let changed = p.run(CTEST, &[]);
        assert_eq!(
            changed.line("hushrun: unit: "),
            "hushrun: unit: 5 of 8 test files run; 3 already passed on the files they read, and 1 more that did runs as a check",
            "{at}"
        );
        let ran = changed.ran();
        assert!(
            ran.contains(&"fmt".to_string()) && ran.contains(&"fmt2".to_string()),
            "{at}: {ran:?}"
        );
        p.write("tests/seed.sh", "#!/bin/sh\n# 2\nexit 0\n");
        git::commit_all(&p.repo, "seed");
        assert_eq!(p.run(CTEST, &[]).ran().len(), 9, "{at}");
    }
}

// covers: C-49, C-30
#[test]
fn a_test_reads_the_programs_of_the_fixtures_and_tests_it_depends_on() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("reuse")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(fx C)\nenable_testing()\n\
                     include_directories(${CMAKE_SOURCE_DIR}/inc)\n\
                     foreach(t t_setup t_inner t_use t_first t_after t_p)\n  add_executable(${t} tests/${t}.c)\nendforeach()\n\
                     add_test(NAME setup COMMAND t_setup)\nadd_test(NAME inner COMMAND t_inner)\n\
                     add_test(NAME use COMMAND t_use)\nadd_test(NAME first COMMAND t_first)\n\
                     add_test(NAME after COMMAND t_after)\nadd_test(NAME p COMMAND t_p)\n\
                     set_tests_properties(setup PROPERTIES FIXTURES_SETUP outer FIXTURES_REQUIRED in)\n\
                     set_tests_properties(inner PROPERTIES FIXTURES_CLEANUP in)\n\
                     set_tests_properties(use PROPERTIES FIXTURES_REQUIRED outer)\n\
                     set_tests_properties(after PROPERTIES DEPENDS first)\n",
                ),
                file("inc/fix.h", "#define FIX 1\n"),
                file("inc/inner.h", "#define INNER 1\n"),
                file("inc/first.h", "#define FIRST 1\n"),
                file(
                    "tests/t_setup.c",
                    "#include \"fix.h\"\nint main(void) { return FIX - 1; }\n",
                ),
                file(
                    "tests/t_inner.c",
                    "#include \"inner.h\"\nint main(void) { return INNER - 1; }\n",
                ),
                file("tests/t_use.c", "int main(void) { return 0; }\n"),
                file(
                    "tests/t_first.c",
                    "#include \"first.h\"\nint main(void) { return FIRST - 1; }\n",
                ),
                file("tests/t_after.c", "int main(void) { return 0; }\n"),
                file("tests/t_p.c", "int main(void) { return 0; }\n"),
            ],
        );
        let first = p.run(CTEST, &[]);
        assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
        let map = store::map(&p.cache);
        let reads = |unit: &str| -> Vec<String> {
            map["units"][unit]["reads"]
                .as_array()
                .unwrap_or_else(|| panic!("{at}: {unit}: {map:#}"))
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        // A fixture's setup, and through it the cleanup of a fixture the setup requires: what
        // they are built from reaches the test that needs them; and what a test it DEPENDS on is
        // built from.
        assert_eq!(
            reads("t_use"),
            [
                "inc/fix.h",
                "inc/inner.h",
                "tests/t_inner.c",
                "tests/t_setup.c",
                "tests/t_use.c"
            ],
            "{at}"
        );
        assert_eq!(
            reads("t_after"),
            ["inc/first.h", "tests/t_after.c", "tests/t_first.c"],
            "{at}"
        );
        assert_eq!(reads("t_p"), ["tests/t_p.c"], "{at}");
    }
}

// covers: C-49, C-30
#[test]
fn a_program_a_test_names_by_a_relative_path_is_read() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        let driver = "#include <unistd.h>\nint main(int argc, char **argv) {\n\
            \x20 if (argc < 2) return 0;\n  execv(argv[1], argv + 1); return 2;\n}\n";
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("reuse")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(rel C)\nenable_testing()\n\
                     add_executable(cli2 tools/cli2.c)\nadd_executable(cli3 tools/cli3.c)\n\
                     add_executable(t_rel tests/driver.c)\nadd_executable(t_rel3 tests/driver.c)\n\
                     add_test(NAME rel COMMAND t_rel ./cli2 WORKING_DIRECTORY ${CMAKE_BINARY_DIR})\n\
                     add_test(NAME rel3 COMMAND t_rel3 ./cli3)\n",
                ),
                file("tools/cli2.c", "int main(void) { return 0; }\n"),
                file("tools/cli3.c", "int main(void) { return 0; }\n"),
                file("tests/driver.c", driver),
            ],
        );
        let first = p.run(CTEST, &[]);
        assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
        let map = store::map(&p.cache);
        // A relative path is taken from the test's WORKING_DIRECTORY, else its directory.
        assert_eq!(
            map["units"]["t_rel"]["reads"],
            json!(["tests/driver.c", "tools/cli2.c"]),
            "{at}"
        );
        assert_eq!(
            map["units"]["t_rel3"]["reads"],
            json!(["tests/driver.c", "tools/cli3.c"]),
            "{at}"
        );
    }
}

// covers: C-49, C-30
#[test]
fn a_program_a_test_program_starts_by_a_compiled_in_path_or_waits_for_is_read() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        let uses = |name: &str| {
            format!("int main(void) {{ const char *p = {name}; return p[0] == '/' ? 0 : 1; }}\n")
        };
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("reuse")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(cdef C)\nenable_testing()\n\
                     add_executable(cli tools/cli.c)\nadd_executable(cli4 tools/cli4.c)\n\
                     add_executable(waited tools/waited.c)\nadd_library(plg MODULE lib/plg.c)\n\
                     add_executable(t_cdef tests/t_cdef.c)\n\
                     target_compile_definitions(t_cdef PRIVATE CLI_PATH=\"$<TARGET_FILE:cli>\")\nadd_dependencies(t_cdef cli)\n\
                     add_executable(t_nodep tests/t_nodep.c)\n\
                     target_compile_definitions(t_nodep PRIVATE CLI_PATH=\"$<TARGET_FILE:cli4>\")\n\
                     add_executable(t_plg tests/t_plg.c)\n\
                     target_compile_definitions(t_plg PRIVATE PLG_PATH=\"$<TARGET_FILE:plg>\")\nadd_dependencies(t_plg plg)\n\
                     add_executable(t_wait tests/t_wait.c)\nadd_dependencies(t_wait waited)\n\
                     add_executable(t_p tests/t_p.c)\n\
                     foreach(t cdef nodep plg wait p)\n  add_test(NAME ${t} COMMAND t_${t})\nendforeach()\n",
                ),
                file("tools/cli.c", "int main(void) { return 0; }\n"),
                file("tools/cli4.c", "int main(void) { return 0; }\n"),
                file("tools/waited.c", "int main(void) { return 0; }\n"),
                file("lib/plg.c", "int plg(void) { return 1; }\n"),
                file("tests/t_cdef.c", &uses("CLI_PATH")),
                file("tests/t_nodep.c", &uses("CLI_PATH")),
                file("tests/t_plg.c", &uses("PLG_PATH")),
                file("tests/t_wait.c", "int main(void) { return 0; }\n"),
                file("tests/t_p.c", "int main(void) { return 0; }\n"),
            ],
        );
        let first = p.run(CTEST, &[]);
        assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
        let map = store::map(&p.cache);
        let reads = |unit: &str| map["units"][unit]["reads"].clone();
        // A path of the build's compiled in, with the program it names waited for or not; a
        // program or module a test program waits for (add_dependencies): each is read.
        assert_eq!(
            reads("t_cdef"),
            json!(["tests/t_cdef.c", "tools/cli.c"]),
            "{at}"
        );
        assert_eq!(
            reads("t_nodep"),
            json!(["tests/t_nodep.c", "tools/cli4.c"]),
            "{at}"
        );
        assert_eq!(
            reads("t_plg"),
            json!(["lib/plg.c", "tests/t_plg.c"]),
            "{at}"
        );
        assert_eq!(
            reads("t_wait"),
            json!(["tests/t_wait.c", "tools/waited.c"]),
            "{at}"
        );
        assert_eq!(reads("t_p"), json!(["tests/t_p.c"]), "{at}");
    }
}

// covers: C-49
#[test]
fn the_projects_discovery_runs_once_more_and_once_more_again_when_tests_are_left_out() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    let generator = generators(&cmake)[0];
    // A script CTest runs each time it reads the tests (how DISCOVERY_MODE PRE_TEST works):
    // it counts.
    let mut files = shop("reuse");
    for (path, text) in &mut files {
        if path == "CMakeLists.txt" {
            text.push_str(
                "file(WRITE ${CMAKE_BINARY_DIR}/count.cmake \"file(APPEND \\\"${CMAKE_BINARY_DIR}/count.txt\\\" \\\"x\\\\n\\\")\")\n\
                 set_property(DIRECTORY APPEND PROPERTY TEST_INCLUDE_FILES ${CMAKE_BINARY_DIR}/count.cmake)\n",
            );
        }
    }
    let p = Project::new(&cmake, generator, &files);
    let count = p.repo.join("build/count.txt");
    let counted = || {
        let n = std::fs::read_to_string(&count).map_or(0, |t| t.lines().count());
        let _ = std::fs::remove_file(&count);
        n
    };
    let plain = p
        .tool("ctest")
        .args(["--test-dir", "build"])
        .output()
        .unwrap();
    assert!(plain.status.success());
    assert_eq!(counted(), 1, "plain ctest");
    // Nothing to leave out: one listing beside the run, to know which program each test runs.
    let first = p.run(CTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(counted(), 2, "{}", first.stderr);
    // Tests left out: one listing more, to check what is left out.
    p.write(
        "src/greet.h",
        "static const char *greet(void) { return \"hey\"; }\n",
    );
    p.build();
    let second = p.run(CTEST, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.stderr.contains("already passed"),
        "{}",
        second.stderr
    );
    assert_eq!(counted(), 3, "{}", second.stderr);
}

// covers: C-49, C-30
#[test]
fn sources_fetched_from_the_repository_built_apart_or_globbed_are_read_or_in_the_rest_set() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    for generator in generators(&cmake) {
        let at = format!("CMake {:?}, {generator}", cmake.version);
        let p = Project::new(
            &cmake,
            generator,
            &[
                file("hushrun.yaml", &yaml("reuse")),
                file(".gitignore", "build/\n"),
                file(
                    "CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(far C)\nenable_testing()\n\
                     include(FetchContent)\n\
                     FetchContent_Declare(dep SOURCE_DIR ${CMAKE_SOURCE_DIR}/third/dep)\nFetchContent_MakeAvailable(dep)\n\
                     include(ExternalProject)\n\
                     ExternalProject_Add(ext SOURCE_DIR ${CMAKE_SOURCE_DIR}/third/ext BINARY_DIR ${CMAKE_BINARY_DIR}/ext-build\n\
                     \x20 INSTALL_COMMAND \"\" BUILD_BYPRODUCTS ${CMAKE_BINARY_DIR}/ext-build/${CMAKE_STATIC_LIBRARY_PREFIX}ext${CMAKE_STATIC_LIBRARY_SUFFIX})\n\
                     add_library(extlib STATIC IMPORTED)\n\
                     set_target_properties(extlib PROPERTIES IMPORTED_LOCATION ${CMAKE_BINARY_DIR}/ext-build/${CMAKE_STATIC_LIBRARY_PREFIX}ext${CMAKE_STATIC_LIBRARY_SUFFIX})\n\
                     add_dependencies(extlib ext)\n\
                     file(GLOB cases ${CMAKE_SOURCE_DIR}/cases/*.c)\n\
                     add_executable(t_dep tests/t_dep.c)\ntarget_link_libraries(t_dep dep)\n\
                     add_executable(t_ext tests/t_ext.c)\ntarget_link_libraries(t_ext extlib)\nadd_dependencies(t_ext ext)\n\
                     add_executable(t_case tests/t_case.c ${cases})\n\
                     foreach(t dep ext case)\n  add_test(NAME ${t} COMMAND t_${t})\nendforeach()\n",
                ),
                file(
                    "third/dep/CMakeLists.txt",
                    "add_library(dep STATIC dep.c)\n",
                ),
                file("third/dep/dep.c", "int dep(void) { return 0; }\n"),
                file(
                    "third/ext/CMakeLists.txt",
                    "cmake_minimum_required(VERSION 3.19)\nproject(ext C)\nadd_library(ext STATIC ext.c)\n",
                ),
                file("third/ext/ext.c", "int ext(void) { return 0; }\n"),
                file("cases/one.c", "int one(void) { return 0; }\n"),
                file(
                    "tests/t_dep.c",
                    "int dep(void);\nint main(void) { return dep(); }\n",
                ),
                file(
                    "tests/t_ext.c",
                    "int ext(void);\nint main(void) { return ext(); }\n",
                ),
                file("tests/t_case.c", "int main(void) { return 0; }\n"),
            ],
        );
        // A file the glob would take, added since the build was configured: no build compiles it.
        p.write("cases/two.c", "int two(void) { return 0; }\n");
        git::commit_all(&p.repo, "two");
        let first = p.run(CTEST, &[]);
        assert_eq!(first.code, 0, "{at}\n{}\n{}", first.stdout, first.stderr);
        let map = store::map(&p.cache);
        // Fetched from the repository and built in this build: read by its user.
        assert!(
            map["units"]["t_dep"]["reads"]
                .as_array()
                .unwrap()
                .contains(&json!("third/dep/dep.c")),
            "{at}: {map:#}"
        );
        assert!(
            map["units"]["t_case"]["reads"]
                .as_array()
                .unwrap()
                .contains(&json!("cases/one.c")),
            "{at}: {map:#}"
        );
        // Built by a build of its own, which this one's records don't name, and a globbed file the
        // build doesn't compile yet: in the rest set, so a change to either runs everything.
        assert!(map["rest"]["third/ext/ext.c"].is_string(), "{at}: {map:#}");
        assert!(map["rest"]["cases/two.c"].is_string(), "{at}: {map:#}");
    }
}

// covers: C-49, C-32
#[test]
fn two_ctest_starts_in_one_command_never_add_up_to_a_program_that_ran_whole() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    let generator = generators(&cmake)[0];
    // t_u's `a` runs in the first start; its `d` and `c` in the second, where t_v fails first
    // and --stop-on-failure stops it before `c`, which always fails.
    let p = Project::new(
        &cmake,
        generator,
        &[
            file("hushrun.yaml", &yaml("reuse")),
            file(".gitignore", "build/\n"),
            file(
                "CMakeLists.txt",
                "cmake_minimum_required(VERSION 3.19)\nproject(two C)\nenable_testing()\n\
                 add_executable(t_u tests/t_u.c)\nadd_executable(t_v tests/t_v.c)\n\
                 add_test(NAME a COMMAND t_u a)\nadd_test(NAME d COMMAND t_u d)\n\
                 add_test(NAME vfail COMMAND t_v)\nadd_test(NAME c COMMAND t_u c)\n\
                 set_tests_properties(a PROPERTIES LABELS L1)\n\
                 set_tests_properties(d vfail c PROPERTIES LABELS L2)\n",
            ),
            file(
                "tests/t_u.c",
                "#include <string.h>\nint main(int argc, char **argv) { return argc > 1 && strcmp(argv[1], \"c\") == 0; }\n",
            ),
            file(
                "tests/t_v.c",
                "#include <stdlib.h>\nint main(void) { return getenv(\"FAIL_V\") != 0; }\n",
            ),
        ],
    );
    let line = "ctest --test-dir build -L L1; ctest --test-dir build -L L2 --stop-on-failure -j1";
    let first = p.run(&["run", "unit", "--", "sh", "-c", line], &[("FAIL_V", "1")]);
    assert_ne!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(!first.ran().contains(&"c".to_string()), "{}", first.stdout);
    // Each start ran a share of t_u, and neither ran it whole: it records no pass. t_v failed.
    assert_eq!(
        unit_results(&p.cache),
        ["fail"],
        "{}\n{}",
        first.stdout,
        first.stderr
    );
}

/// The result of each unit result this machine recorded, `pass` or `fail`, sorted.
fn unit_results(cache: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect(&cache.join("results/units/v0"), &mut files);
    let mut out: Vec<String> = files
        .iter()
        .map(|path| {
            let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            value["result"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    out.sort();
    out
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
    // \[ADR 0087, amended\] Its pass proves nothing of the files now: nothing is recorded, the
    // check's pass included, and the run says why.
    assert!(
        stale.stderr.contains(
            "hushrun: unit passed, but it isn't recorded: t_util ran old code: src/util.h changed after CMakeFiles/t_util.dir/tests/t_util.c.o was made; build before ctest"
        ),
        "{}",
        stale.stderr
    );
    assert!(store::stores(&p.cache).is_empty(), "{}", stale.stderr);
}

// covers: C-49
#[test]
fn a_program_a_test_left_running_keeps_printing_after_hushruns_ctest_returns() {
    use std::io::Read as _;
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        bin.join("ctest"),
        "#!/bin/sh\necho ran; (sleep 2; echo late line) & exit 8\n",
    )
    .unwrap();
    std::fs::set_permissions(bin.join("ctest"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let was = format!("{}:/usr/bin:/bin", bin.display());
    let started = std::time::Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hushrun"))
        .args(["__hushrun-program", "ctest", "--version"])
        .current_dir(&root)
        .env("PATH", &was)
        .env("HUSHRUN_PROGRAMS_PATH", format!("={was}"))
        .env("HUSHRUN_NO_UPDATE_CHECK", "1")
        .env("HUSHRUN_HOST", NO_HOST)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        out
    });
    let status = child.wait().unwrap();
    let returned = started.elapsed();
    // hushrun's ctest hands ctest its own standard output: it returns as ctest does, and what a
    // program left running prints later reaches the same output.
    assert_eq!(status.code(), Some(8));
    assert!(returned < std::time::Duration::from_secs(2), "{returned:?}");
    let out = reader.join().unwrap();
    assert!(out.contains("ran") && out.contains("late line"), "{out}");
}

// covers: C-49
#[test]
fn a_signal_during_a_listing_leaves_the_presets_log_as_it_was() {
    use hushrun_testkit::env::Hermetic as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::ExitStatusExt as _;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("build")).unwrap();
    std::fs::write(
        repo.join("build/CMakeCache.txt"),
        "CMAKE_GENERATOR:INTERNAL=Other\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("CMakePresets.json"),
        r#"{"version": 3, "configurePresets": [{"name": "dev", "binaryDir": "${sourceDir}/build"}],
            "testPresets": [{"name": "log", "configurePreset": "dev", "output": {"outputLogFile": "mylog.txt"}}]}"#,
    )
    .unwrap();
    std::fs::write(repo.join("mylog.txt"), "mine\n").unwrap();
    // A ctest whose listing writes the preset's log, as CTest does, then takes a while.
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let marker = root.join("listing");
    std::fs::write(
        bin.join("ctest"),
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'ctest version 4.4.5' ;;\n  *--show-only=json-v1*) echo listing > mylog.txt; touch '{}'; sleep 3; echo '{{\"tests\": []}}' ;;\nesac\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("ctest"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let units = root.join("units");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(
        units.join("hushrun.json"),
        json!({"schema": "hushrun.units/v0", "root": repo, "reuse": []}).to_string(),
    )
    .unwrap();
    let path = std::env::join_paths([bin.clone(), "/usr/bin".into(), "/bin".into()]).unwrap();
    let mut was = std::ffi::OsString::from("=");
    was.push(&path);
    let mut child = Command::new(env!("CARGO_BIN_EXE_hushrun"))
        // What the developer's shell exports never reaches a runner a test starts.
        .hermetic()
        .current_dir(&repo)
        .env("PATH", &path)
        .env("HUSHRUN_UNITS", &units)
        .env("HUSHRUN_PROGRAMS_PATH", was)
        .env("HUSHRUN_NO_UPDATE_CHECK", "1")
        .env("HUSHRUN_HOST", NO_HOST)
        .args(["__hushrun-program", "ctest", "--preset", "log"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let waited = std::time::Instant::now();
    while !marker.exists() && waited.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
    let status = child.wait().unwrap();
    // Ended by the signal, after the log was put back.
    assert_eq!(status.signal(), Some(2), "{status:?}");
    assert_eq!(
        std::fs::read_to_string(repo.join("mylog.txt")).unwrap(),
        "mine\n"
    );
}

// covers: C-49
#[test]
fn each_preset_field_that_chooses_tests_is_kept_when_tests_are_left_out() {
    for cmake in with_cmake() {
        let generator = generators(&cmake)[0];
        let at = format!("CMake {:?}", cmake.version);
        let mut files = shop("reuse");
        for (path, text) in &mut files {
            if path == "CMakeLists.txt" {
                text.push_str(
                    "set_tests_properties(price cart_total PROPERTIES LABELS core)\nset_tests_properties(greet PROPERTIES LABELS ui)\n",
                );
            }
        }
        files.push(file(
            "CMakePresets.json",
            r#"{"version": 3,
                "configurePresets": [{"name": "dev", "binaryDir": "${sourceDir}/build"}],
                "testPresets": [
                  {"name": "name", "configurePreset": "dev", "filter": {"include": {"name": "^(cart_total|util)$"}}},
                  {"name": "label", "configurePreset": "dev", "filter": {"include": {"label": "core"}}},
                  {"name": "nolabel", "configurePreset": "dev", "filter": {"exclude": {"label": "ui"}}},
                  {"name": "union", "configurePreset": "dev",
                   "filter": {"include": {"name": "^greet$", "label": "core", "useUnion": true}}},
                  {"name": "noname", "configurePreset": "dev", "filter": {"exclude": {"name": "^misc$"}}}
                ]}"#,
        ));
        let p = Project::new(&cmake, generator, &files);
        let ran = |shim: &Shim| -> Vec<String> {
            let mut out: Vec<String> = shim
                .stdout
                .lines()
                .filter_map(|l| {
                    let rest = l.split_once(" Test #")?.1;
                    let name = rest.split_once(": ")?.1;
                    Some(name.split(" .").next()?.trim().to_string())
                })
                .collect();
            out.sort();
            out
        };
        let unit_tests = |unit: &str| -> Vec<&'static str> {
            match unit {
                "t_util" => vec!["util"],
                "t_price" => vec!["price"],
                "t_greet" => vec!["greet"],
                "t_cart" => vec!["cart_empty", "cart_tax", "cart_total"],
                _ => unreachable!(),
            }
        };
        for (preset, reused) in [
            ("name", "t_util"),
            ("label", "t_price"),
            ("nolabel", "t_util"),
            ("union", "t_greet"),
            ("noname", "t_cart"),
        ] {
            // What the preset chooses, as plain ctest lists it.
            let listed = p
                .tool("ctest")
                .args(["--preset", preset, "-N"])
                .output()
                .unwrap();
            let chosen: Vec<String> = String::from_utf8_lossy(&listed.stdout)
                .lines()
                .filter_map(|l| {
                    Some(
                        l.split_once("Test #")?
                            .1
                            .split_once(": ")?
                            .1
                            .trim()
                            .to_string(),
                    )
                })
                .collect();
            let shim = p.shim(
                &["--preset", preset],
                json!({"unit": "test", "reuse": [reused]}),
                Some(shop_plan(&[reused])),
                &[],
            );
            assert_eq!(shim.code, 0, "{at} {preset}\n{}", shim.stdout);
            // What runs is what the preset chooses, less at most what was left out: never a test
            // it doesn't choose, never one fewer than it chooses but those of the unit reused.
            let ran = ran(&shim);
            let left = unit_tests(reused);
            for name in &ran {
                assert!(
                    chosen.contains(name),
                    "{at} {preset}: {name} ran\n{}",
                    shim.stdout
                );
            }
            for name in &chosen {
                assert!(
                    ran.contains(name) || left.contains(&name.as_str()),
                    "{at} {preset}: {name} didn't run\n{}",
                    shim.stdout
                );
            }
            // Where CTest honours the preset, the unit reused was left out.
            if !(preset == "union" && cmake.version < (3, 25)) {
                assert!(
                    left.iter().all(|t| !ran.contains(&t.to_string())),
                    "{at} {preset}\n{}",
                    shim.stdout
                );
            }
        }
    }
}
