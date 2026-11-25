//! \[ADR 0070\] The Go modules and the Go the tests of Go's adapter run, in-process (`units_go.rs`)
//! and end to end (`e2e_go.rs`). The fixtures use the standard library only, and need Go: they say
//! so and pass where it is missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#![allow(dead_code)]

use std::process::Command;

use super::repo::{CLEARED, Repo};

/// The module the fixtures are.
pub const MODULE: &str = "example.com/shop";

/// Go's variables COMMAND must never inherit from the test's, besides [`CLEARED`].
pub const GO_CLEARED: &[&str] = &["HUSHRUN_GOFLAGS", "GOFLAGS", "GOOS", "GOARCH", "GOWORK"];

/// A repository of `files` whose COMMAND runs Go as these tests run it: this machine's build
/// cache, so that the standard library builds once, no toolchain download, and no configuration
/// file of the person's.
pub fn repo(files: &[(String, String)]) -> Repo {
    Repo::new(files)
        .clear(GO_CLEARED)
        .give("GOCACHE", go_cache())
        .give("GOTOOLCHAIN", "local")
        .give("GOENV", "off")
}

/// Go, outside hushrun, as these tests run it ([`repo`]): what the developer's shell exports
/// never reaches it.
pub fn go_env(cmd: &mut Command) {
    cmd.env("GOCACHE", go_cache())
        .env("GOTOOLCHAIN", "local")
        .env("GOENV", "off")
        .env("GOFLAGS", "")
        .env_remove("GOFLAGS");
    for var in CLEARED.iter().chain(GO_CLEARED) {
        cmd.env_remove(var);
    }
    hushrun_testkit::env::Hermetic::hermetic(cmd);
}

/// Go's own build cache, which the tests share.
pub fn go_cache() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            Command::new("go")
                .args(["env", "GOCACHE"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| {
                    std::env::temp_dir()
                        .join("hushrun-test-gocache")
                        .display()
                        .to_string()
                })
        })
        .clone()
}

/// Whether a tool the tests need is `here`: a test without it says so and passes, but under
/// `HUSHRUN_TEST_TOOLS=required`, which CI sets, where it fails.
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

pub fn has_go() -> bool {
    Command::new("go")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

/// A shop in Go: a pricing package and a cart that imports it, each with tests; a greeting with
/// tests of its own; and a command with none.
pub fn shop(tests: &str) -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file(
            "pricing/pricing.go",
            "package pricing\n\nfunc Price(x int) int { return x * 2 }\n",
        ),
        file(
            "pricing/pricing_test.go",
            "package pricing\n\nimport \"testing\"\n\nfunc TestPrice(t *testing.T) {\n\tif Price(3) != 6 {\n\t\tt.Fatal(\"price\")\n\t}\n}\n",
        ),
        file(
            "cart/cart.go",
            &format!(
                "package cart\n\nimport \"{MODULE}/pricing\"\n\nfunc Total(xs []int) int {{\n\tn := 0\n\tfor _, x := range xs {{\n\t\tn += pricing.Price(x)\n\t}}\n\treturn n\n}}\n"
            ),
        ),
        file(
            "cart/cart_test.go",
            &format!(
                "package cart_test\n\nimport (\n\t\"testing\"\n\n\t\"{MODULE}/cart\"\n)\n\nfunc TestTotal(t *testing.T) {{\n\tif cart.Total([]int{{1, 2}}) != 6 {{\n\t\tt.Fatal(\"total\")\n\t}}\n}}\n"
            ),
        ),
        file(
            "greet/greet.go",
            "package greet\n\nfunc Hello() string { return \"hi\" }\n",
        ),
        file(
            "greet/greet_test.go",
            "package greet\n\nimport \"testing\"\n\nfunc TestHello(t *testing.T) {\n\tif Hello() != \"hi\" {\n\t\tt.Fatal(\"hello\")\n\t}\n}\n",
        ),
        file("cmd/tool/main.go", "package main\n\nfunc main() {}\n"),
    ]
}
