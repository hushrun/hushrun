//! \[ADR 0120\] C++20 modules and precompiled headers in a CMake build through the launcher
//! (B-241 to B-243), end to end with clang: CMake 3.28 and later scan every C++20 source and give
//! each compile a module map; one that imports no module gets an empty map, and its object is
//! stored and reused as any other, while a compile that reads a module's interface, or a
//! precompiled header, passes through. And a clang that doesn't take the include graph's
//! variables (clang 15 to 18) compiles as asked.
//!
//! They need CMake 3.28 or later, Ninja, `clang++` and the `clang-scan-deps` CMake finds beside
//! it, and run where `HUSHRUN_TEST_CXX_MODULES` is set, which CI sets on Linux; elsewhere they
//! say so and pass.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

use hushrun_testkit::git;

const NO_HOST: &str = "http://127.0.0.1:9";

const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_CC",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "CMAKE_GENERATOR",
    "CMAKE_BUILD_TYPE",
    "CC_PRINT_HEADERS_FORMAT",
    "CC_PRINT_HEADERS_FILTERING",
    "CC_PRINT_HEADERS_FILE",
];

fn wanted() -> bool {
    if std::env::var_os("HUSHRUN_TEST_CXX_MODULES").is_some() {
        return true;
    }
    eprintln!(
        "note: skipping: HUSHRUN_TEST_CXX_MODULES isn't set (CMake 3.28, clang++, clang-scan-deps)"
    );
    false
}

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git::init(&repo, "sha1");
        let files = [
            (
                "hushrun.yaml",
                "version: 1\nrepository: example.com/acme/mods\nchecks:\n  build:\n    run: cmake --build build\n",
            ),
            (".gitignore", "build/\n"),
            (
                "CMakeLists.txt",
                "cmake_minimum_required(VERSION 3.28)\nproject(mods CXX)\nset(CMAKE_CXX_STANDARD 20)\n\
                 add_library(greet)\ntarget_sources(greet PUBLIC FILE_SET CXX_MODULES FILES greet.cppm)\n\
                 add_executable(app main.cpp plain.cpp)\ntarget_link_libraries(app greet)\n\
                 add_executable(pch_app pch_main.cpp)\ntarget_precompile_headers(pch_app PRIVATE big.h)\n",
            ),
            (
                "greet.cppm",
                "export module greet;\nexport int greet() { return 42; }\n",
            ),
            (
                "main.cpp",
                "import greet;\nint plain();\nint main() { return greet() + plain() - 42; }\n",
            ),
            (
                "plain.cpp",
                "#include <vector>\nint plain() { return std::vector<int>{}.size(); }\n",
            ),
            (
                "big.h",
                "#include <vector>\ninline int big() { return 1; }\n",
            ),
            ("pch_main.cpp", "int main() { return big() - 1; }\n"),
        ];
        for (path, text) in files {
            std::fs::write(repo.join(path), text).unwrap();
        }
        git::commit_all(&repo, "init");
        Repo {
            _dir: dir,
            root,
            repo,
        }
    }

    fn hushrun(&self, args: &[&str]) -> (i32, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
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
                || name.starts_with("RUSTC_WRAPPER")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        // B-16: no audit, which would compile the one reuse again one time in 50 and count it
        // as compared, not reused.
        cmd.env("HUSHRUN_BUILD_AUDIT", "none");
        let out = cmd.args(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// COMMAND `sh -c script`, run whatever passed before.
    fn run(&self, name: &str, script: &str) -> (i32, String, String) {
        self.hushrun(&["run", name, "--force", "--", "sh", "-c", script])
    }
}

fn cc_line(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|l| l.contains("c/c++:"))
        .collect::<Vec<_>>()
        .join("\n")
}

// covers: B-241, B-242, B-243
#[test]
fn a_cxx20_source_that_imports_no_module_is_reused_and_modules_and_pchs_pass_through() {
    if !wanted() {
        return;
    }
    let r = Repo::new();
    assert_eq!(r.hushrun(&["build", "on"]).0, 0);
    let configure = "cmake -G Ninja -S . -B build -DCMAKE_CXX_COMPILER=clang++ >/dev/null";
    let (code, out, err) = r.run(
        "build",
        &format!("{configure} && cmake --build build && ./build/app && ./build/pch_app"),
    );
    // Every compile succeeds through the launcher, whichever clang this is (B-243).
    assert_eq!(code, 0, "{out}{err}");
    let first = cc_line(&err);
    assert!(first.contains("0 reused"), "{first}");
    assert!(first.contains("modules"), "{first}");
    assert!(first.contains("pch"), "{first}");
    // Built again from nothing: plain.cpp, whose module map is empty, is reused; the module, its
    // importer and the precompiled header and its user compile again.
    let (code, out, err) = r.run(
        "build",
        "ninja -C build -t clean >/dev/null && cmake --build build && ./build/app && ./build/pch_app",
    );
    assert_eq!(code, 0, "{out}{err}");
    let second = cc_line(&err);
    assert!(second.contains("1 reused"), "{second}");
    assert!(second.contains("2 modules"), "{second}");
    assert!(second.contains("2 pch"), "{second}");
}
