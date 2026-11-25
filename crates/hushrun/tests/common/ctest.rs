//! \[ADR 0090\] The fixtures of the CTest adapter's tests, which `units_ctest.rs` (in-process)
//! and `e2e_ctest.rs` (the CLI) share: generated CMake projects of plain C, which need CMake.
//! `HUSHRUN_TEST_CMAKE` names the directories holding `cmake` and `ctest` (and `ninja`, where
//! there), separated as `PATH` is; each CMake there runs the tests, and a directory holding only
//! `ninja` gives the others one. Without it, the `cmake` on `PATH`. The tests say so and pass where
//! CMake is missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets. No fixture needs the
//! network.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// One CMake to run the fixtures with.
#[derive(Debug, Clone)]
pub struct Cmake {
    pub bin: PathBuf,
    pub ninja: Option<PathBuf>,
    pub version: (u32, u32),
}

/// The CMakes `HUSHRUN_TEST_CMAKE` names, or the one on `PATH`.
pub fn cmakes() -> Vec<Cmake> {
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
            let out = hushrun_testkit::tool::command(d.join("cmake"))
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

pub fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

pub fn tools(what: &str, here: bool) -> bool {
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
pub fn with_cmake() -> Vec<Cmake> {
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
pub fn generators(cmake: &Cmake) -> Vec<&'static str> {
    let mut out = Vec::new();
    if cmake.ninja.is_some() {
        out.push("Ninja");
    }
    if which("make").is_some() {
        out.push("Unix Makefiles");
    }
    out
}

pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A shop in C: a price library two test programs link, a cart header one of them includes, and
/// three programs that read headers of their own, or none. Every test but `greet` fails when
/// `FAIL_HIDDEN` is set, which no file says, as a missed dependency would.
pub fn shop(tests: &str) -> Vec<(String, String)> {
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

/// `PATH` for a project's tools and COMMAND: `cmake`'s directory, Ninja's, Make's and the C
/// compiler's, and the system's.
pub fn path(cmake: &Cmake) -> std::ffi::OsString {
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
    std::env::join_paths(dirs).unwrap()
}

/// `cmake`'s arguments that configure the repository in `build_dir` with `generator`.
pub fn configure(cmake: &Cmake, generator: &str, build_dir: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-S", ".", "-B", build_dir, "-G", generator]
        .map(String::from)
        .to_vec();
    if generator == "Ninja" {
        args.push(format!(
            "-DCMAKE_MAKE_PROGRAM={}",
            cmake.ninja.as_ref().unwrap().display()
        ));
    }
    args
}
