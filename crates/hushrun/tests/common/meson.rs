//! \[ADR 0119\] The fixtures of the `meson test` adapter's tests, which `units_meson.rs`
//! (in-process) and `e2e_meson.rs` (the CLI) share: generated Meson projects of plain C, which need
//! Meson and Ninja. `HUSHRUN_TEST_MESON` names the `meson` to use (and `HUSHRUN_TEST_CMAKE` the
//! folder of `ninja`, as for the CTest tests). Without them, the ones on `PATH`. The tests say so
//! and pass where they are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets. No
//! fixture needs the network.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// The folders holding `meson` and `ninja`, or `None`, said, where either is missing.
pub fn tools() -> Option<Vec<PathBuf>> {
    let meson = std::env::var_os("HUSHRUN_TEST_MESON")
        .map(PathBuf::from)
        .or_else(|| which("meson"));
    let ninja = std::env::var_os("HUSHRUN_TEST_CMAKE")
        .and_then(|dirs| {
            std::env::split_paths(&dirs)
                .map(|d| d.join("ninja"))
                .find(|p| p.is_file())
        })
        .or_else(|| which("ninja"));
    match (meson, ninja) {
        (Some(meson), Some(ninja)) => Some(vec![
            meson.parent().unwrap().to_path_buf(),
            ninja.parent().unwrap().to_path_buf(),
        ]),
        _ => {
            if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
                panic!(
                    "Meson or Ninja is missing (HUSHRUN_TEST_MESON), and HUSHRUN_TEST_TOOLS=required"
                );
            }
            eprintln!(
                "note: skipping: Meson or Ninja is missing (HUSHRUN_TEST_TOOLS=required makes this fail)"
            );
            None
        }
    }
}

/// `PATH` for the tools of a project: `bin`'s folders, `cc`'s, and the system's.
pub fn path(bin: &[PathBuf]) -> std::ffi::OsString {
    let mut dirs = bin.to_vec();
    dirs.extend(which("cc").and_then(|m| m.parent().map(Path::to_path_buf)));
    dirs.push("/usr/bin".into());
    dirs.push("/bin".into());
    std::env::join_paths(dirs).unwrap()
}

/// A shop in C: a price library two test programs link, a cart header one includes, and a
/// greeting; `extra` ends `meson.build`.
pub fn shop(extra: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: reuse\n"
                .into(),
        ),
        (".gitignore", "build/\n".into()),
        (
            "meson.build",
            format!(
                "project('shop', 'c')\ninc = include_directories('src')\n\
                 price = static_library('price', 'src/price.c', include_directories: inc)\n\
                 t_price = executable('t_price', 'tests/t_price.c', link_with: price, include_directories: inc)\n\
                 t_cart = executable('t_cart', 'tests/t_cart.c', link_with: price, include_directories: inc)\n\
                 t_greet = executable('t_greet', 'tests/t_greet.c', include_directories: inc)\n\
                 test('price', t_price)\ntest('cart_total', t_cart, args: ['total'])\n\
                 test('cart_empty', t_cart, args: ['empty'])\ntest('greet', t_greet)\n{extra}"
            ),
        ),
        ("src/price.h", "int price(int x);\n".into()),
        (
            "src/price.c",
            "#include \"price.h\"\nint price(int x) { return x * 2; }\n".into(),
        ),
        (
            "src/cart.h",
            "static int cart_total(int a, int b) { return a + b; }\n".into(),
        ),
        (
            "src/greet.h",
            "static const char *greet(void) { return \"hi\"; }\n".into(),
        ),
        (
            "tests/t_price.c",
            "#include <stdlib.h>\n#include \"price.h\"\nint main(void) { if (getenv(\"FAIL_PRICE\")) return 1; return price(3) == 6 ? 0 : 1; }\n"
                .into(),
        ),
        (
            "tests/t_cart.c",
            "#include <string.h>\n#include \"price.h\"\n#include \"cart.h\"\n\
             int main(int argc, char **argv) { if (argc > 1 && strcmp(argv[1], \"total\") == 0) return cart_total(price(1), price(2)) == 6 ? 0 : 1; return 0; }\n"
                .into(),
        ),
        (
            "tests/t_greet.c",
            "#include \"greet.h\"\nint main(void) { return greet()[0] == 'h' ? 0 : 1; }\n".into(),
        ),
    ]
}
