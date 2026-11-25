//! \[ADR 0122\] The Elixir projects and tools the tests of the ExUnit adapter run, in-process
//! (`units_exunit.rs`) and end to end (`e2e_exunit.rs`). See `units_exunit.rs` for the tools.

#![allow(dead_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use super::repo::Repo;

/// The `mix` these tests run, when there is one.
pub fn mix() -> Option<PathBuf> {
    let named = std::env::var_os("HUSHRUN_TEST_MIX").map(PathBuf::from);
    let mix = named.unwrap_or_else(|| PathBuf::from("mix"));
    let ok = Command::new(&mix)
        .arg("--version")
        .env("HOME", std::env::temp_dir())
        .output()
        .is_ok_and(|o| o.status.success());
    ok.then_some(mix)
}

/// Whether the tests can run here: Elixir is missing, they say so, and pass, unless CI asked for
/// it by name.
pub fn has_mix() -> bool {
    if mix().is_some() {
        return true;
    }
    let required = std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required")
        && std::env::var_os("HUSHRUN_TEST_MIX").is_some();
    if required {
        panic!("Elixir is missing, and HUSHRUN_TEST_TOOLS=required names HUSHRUN_TEST_MIX");
    }
    eprintln!("note: skipping: Elixir is missing (set HUSHRUN_TEST_MIX)");
    false
}

/// `PATH` with the folder of `HUSHRUN_TEST_MIX` first.
pub fn path() -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(mix) = std::env::var_os("HUSHRUN_TEST_MIX").map(PathBuf::from)
        && let Some(dir) = mix.parent()
    {
        dirs.push(dir.to_path_buf());
    }
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(dirs).expect("PATH")
}

/// A repository of `files`, with Mix's outputs ignored, whose COMMAND runs the tests' Elixir:
/// none of a person's Erlang or Mix settings reach it.
pub fn repo(files: &[(String, String)]) -> Repo {
    let mut files = files.to_vec();
    files.push(file(".gitignore", "/_build/\n/deps/\n"));
    Repo::new(&files)
        .clear(&[
            "ERL_LIBS",
            "ELIXIR_ERL_OPTIONS",
            "MIX_ENV",
            "MIX_HOME",
            "MIX_BUILD_PATH",
        ])
        .give("PATH", path())
        .give("LANG", "C.UTF-8")
        .give("ELIXIR_ERL_OPTIONS", "+fnu")
}

/// Writes `text` at `path` in `r`, a second after what it replaces: Mix compiles a file again
/// when it is newer than its manifest, by the second.
pub fn write(r: &Repo, path: &str, text: &str) {
    if r.repo.join(path).exists() {
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    r.write(path, text);
}

/// What `unit` read, as this machine's store keeps it.
pub fn read_by(r: &Repo, unit: &str) -> Vec<String> {
    let map = r.map();
    map["units"][unit]["reads"]
        .as_array()
        .unwrap_or_else(|| panic!("{unit}: {map:#}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// The test modules a test of which ExUnit ran, as `mix test --trace` names them in `stdout`: a
/// module whose every test it excluded isn't one.
pub fn modules_ran(stdout: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in stdout.lines() {
        if !line.starts_with(' ') && line.contains(" [") && line.contains("_test.exs]") {
            current = line.split(" [").next().map(|n| n.trim().to_string());
        } else if line.trim_start().starts_with("* test ") && !line.contains("(excluded)") {
            found.extend(current.clone());
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Whether `line` is the plan's, which counts test files.
pub fn is_plan_line(line: &str) -> bool {
    line.contains("test file") && (line.contains(" run") || line.contains("reused"))
}

pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

pub const MIX_EXS: &str = "defmodule Shop.MixProject do\n  use Mix.Project\n\n  def project do\n    [app: :shop, version: \"0.1.0\", elixir: \"~> 1.14\", deps: []]\n  end\nend\n";

pub fn test_file(module: &str, body: &str) -> String {
    format!(
        "defmodule {module} do\n  use ExUnit.Case, async: false\n\n  test \"it\" do\n    {body}\n  end\nend\n"
    )
}

/// A shop in Elixir: a price, a cart that calls it, a greeting, a module only ever called by a
/// name made at run time, and a macro; a test file for each.
pub fn shop(tests: &str) -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file("mix.exs", MIX_EXS),
        file(
            "lib/shop/price.ex",
            "defmodule Shop.Price do\n  def of(x), do: x * 2\nend\n",
        ),
        file(
            "lib/shop/cart.ex",
            "defmodule Shop.Cart do\n  def total(xs), do: xs |> Enum.map(&Shop.Price.of/1) |> Enum.sum()\nend\n",
        ),
        file(
            "lib/shop/greet.ex",
            "defmodule Shop.Greet do\n  def hello, do: \"hi\"\nend\n",
        ),
        file(
            "lib/shop/plugin.ex",
            "defmodule Shop.Plugin do\n  def name, do: :plugin\nend\n",
        ),
        file(
            "lib/shop/tax.ex",
            "defmodule Shop.Tax do\n  defmacro rate, do: 10\nend\n",
        ),
        file("test/test_helper.exs", "ExUnit.start()\n"),
        file(
            "test/price_test.exs",
            &test_file("Shop.PriceTest", "assert Shop.Price.of(3) == 6"),
        ),
        file(
            "test/cart_test.exs",
            &test_file("Shop.CartTest", "assert Shop.Cart.total([1, 2]) == 6"),
        ),
        file(
            "test/greet_test.exs",
            &test_file("Shop.GreetTest", "assert Shop.Greet.hello() == \"hi\""),
        ),
        file(
            "test/plugin_test.exs",
            &test_file(
                "Shop.PluginTest",
                "assert Module.concat([\"Shop\", \"Plugin\"]).name() == :plugin",
            ),
        ),
        file(
            "test/tax_test.exs",
            "defmodule Shop.TaxTest do\n  use ExUnit.Case\n  require Shop.Tax\n\n  test \"it\" do\n    assert Shop.Tax.rate() == 10\n  end\nend\n",
        ),
    ]
}

pub fn capitalize(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
