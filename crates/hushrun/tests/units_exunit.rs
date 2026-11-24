//! \[ADR 0122\] Test files with `mix test` (C-114 to C-117): a unit is a test file, what it read is
//! what its code references to the end, what ran while it ran, and the project's configuration,
//! and reuse never misses a failure.
//!
//! The projects use Elixir's standard library only, and need Elixir and Erlang/OTP:
//! `HUSHRUN_TEST_MIX` names the `mix` to use (by default the one on `PATH`), with `erl` and `erlc`
//! beside it. The tests say so and pass where it is missing, unless `HUSHRUN_TEST_TOOLS=required`
//! and `HUSHRUN_TEST_MIX` are both set, as CI's Linux job sets them.

#[path = "common/store.rs"]
mod store;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use hushrun_testkit::git;
use serde_json::Value;

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
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "ERL_LIBS",
    "ELIXIR_ERL_OPTIONS",
    "MIX_ENV",
    "MIX_HOME",
    "MIX_BUILD_PATH",
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

/// The `mix` these tests run, when there is one.
fn mix() -> Option<PathBuf> {
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
fn has_mix() -> bool {
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
fn path() -> OsString {
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

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
}

impl Repo {
    fn new(files: &[(String, String)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let r = Repo {
            _dir: dir,
            cache: root.join("cache"),
            root,
            repo,
        };
        for (path, text) in files {
            r.write(path, text);
        }
        r.write(".gitignore", "/_build/\n/deps/\n");
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        // Mix compiles a file again when it is newer than its manifest, by the second.
        if path.exists() {
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        std::fs::write(&path, text).expect("write");
    }

    fn hushrun(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", self.root.join("home"))
            .env("PATH", path())
            .env("LANG", "C.UTF-8")
            .env("ELIXIR_ERL_OPTIONS", "+fnu")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        for var in CLEARED {
            if *var != "ELIXIR_ERL_OPTIONS" {
                cmd.env_remove(var);
            }
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
        cmd
    }

    fn run(&self, args: &[&str]) -> Ran {
        let output = self.hushrun().args(args).output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn map(&self) -> Value {
        store::map(&self.cache)
    }

    fn reads(&self, unit: &str) -> Vec<String> {
        let map = self.map();
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit}: {map:#}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn plan_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| l.contains("test file") && (l.contains(" run") || l.contains("reused")))
            .unwrap_or_else(|| panic!("no plan line in:\n{}\n{}", self.stderr, self.stdout))
    }

    /// The test modules a test of which ExUnit ran, as `mix test --trace` names them: a module
    /// whose every test it excluded isn't one.
    fn ran(&self) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        let mut current: Option<String> = None;
        for line in self.stdout.lines() {
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
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

const MIX_EXS: &str = "defmodule Shop.MixProject do\n  use Mix.Project\n\n  def project do\n    [app: :shop, version: \"0.1.0\", elixir: \"~> 1.14\", deps: []]\n  end\nend\n";

fn test_file(module: &str, body: &str) -> String {
    format!(
        "defmodule {module} do\n  use ExUnit.Case, async: false\n\n  test \"it\" do\n    {body}\n  end\nend\n"
    )
}

/// A shop in Elixir: a price, a cart that calls it, a greeting, a module only ever called by a
/// name made at run time, and a macro; a test file for each.
fn shop(tests: &str) -> Vec<(String, String)> {
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

const MIX_TEST: &[&str] = &["run", "unit", "--", "mix", "test", "--trace", "--seed", "0"];

// covers: C-114, C-115, C-116
#[test]
fn test_files_read_what_their_code_references_and_what_ran() {
    if !has_mix() {
        return;
    }
    let r = Repo::new(&shop("observe"));
    let first = r.run(MIX_TEST);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.ran(),
        [
            "Shop.CartTest",
            "Shop.GreetTest",
            "Shop.PluginTest",
            "Shop.PriceTest",
            "Shop.TaxTest"
        ],
        "{}",
        first.stdout
    );
    let reads = r.reads("test/cart_test.exs");
    // Its own file, what its code references to the end, and the project's configuration.
    for path in [
        "test/cart_test.exs",
        "lib/shop/cart.ex",
        "lib/shop/price.ex",
        "mix.exs",
        "test/test_helper.exs",
    ] {
        assert!(reads.contains(&path.to_string()), "{path}: {reads:?}");
    }
    assert!(
        !reads.contains(&"lib/shop/greet.ex".to_string()),
        "{reads:?}"
    );
    // A macro is a compile-time reference.
    let reads = r.reads("test/tax_test.exs");
    assert!(reads.contains(&"lib/shop/tax.ex".to_string()), "{reads:?}");
    // A module called by a name made at run time is read through what ran while it ran.
    let reads = r.reads("test/plugin_test.exs");
    assert!(
        reads.contains(&"lib/shop/plugin.ex".to_string()),
        "{reads:?}"
    );
    let map = r.map();
    assert!(map["rest"][".gitignore"].is_string(), "{map:#}");
}

// covers: C-114, C-116, C-117
#[test]
fn mix_test_reuses_what_already_passed_and_runs_what_changed() {
    if !has_mix() {
        return;
    }
    let r = Repo::new(&shop("{mode: reuse, always: []}"));
    let first = r.run(MIX_TEST);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // Until ExUnit has reported the check's test files here, the plan says nothing: every test
    // file runs.
    assert_eq!(first.ran().len(), 5, "{}", first.stdout);
    // The greeting changes: its test file runs, and the others' passes hold (an audit may run
    // one of them as a check).
    r.write(
        "lib/shop/greet.ex",
        "defmodule Shop.Greet do\n  def hello, do: \"h\" <> \"i\"\nend\n",
    );
    let whys: Vec<String> = ["price", "cart", "plugin", "tax"]
        .iter()
        .map(|n| {
            r.run(&["why", "unit", &format!("test/{n}_test.exs")])
                .stdout
        })
        .collect();
    let second = r.run(MIX_TEST);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.plan_line().contains("of 5 test files run"),
        "{}",
        second.stderr
    );
    let ran = second.ran();
    assert!(ran.contains(&"Shop.GreetTest".to_string()), "{ran:?}");
    assert!(
        ran.len() <= 2,
        "{ran:?}\n{}\n{whys:#?}\n{:#}",
        second.stderr,
        r.map()["units"]
    );
    assert!(
        second.stdout.contains("excluded"),
        "ExUnit's own filter left them out:\n{}",
        second.stdout
    );
    // The price changes: the cart's test file, which calls it through the cart, runs too.
    r.write(
        "lib/shop/price.ex",
        "defmodule Shop.Price do\n  def of(x), do: x + x\nend\n",
    );
    let third = r.run(MIX_TEST);
    assert_eq!(third.code, 0, "{}\n{}", third.stdout, third.stderr);
    let ran = third.ran();
    assert!(
        ran.contains(&"Shop.PriceTest".to_string()) && ran.contains(&"Shop.CartTest".to_string()),
        "{ran:?}"
    );
    // The module only called by a name made at run time changes: its test file runs.
    r.write(
        "lib/shop/plugin.ex",
        "defmodule Shop.Plugin do\n  def name, do: :other\nend\n",
    );
    let fourth = r.run(MIX_TEST);
    assert_ne!(fourth.code, 0, "{}\n{}", fourth.stdout, fourth.stderr);
    assert!(
        fourth.ran().contains(&"Shop.PluginTest".to_string()),
        "{}",
        fourth.stdout
    );
    // A failure is never reused: fixed, it runs again, and passes.
    r.write(
        "test/plugin_test.exs",
        &test_file(
            "Shop.PluginTest",
            "assert Module.concat([\"Shop\", \"Plugin\"]).name() == :other",
        ),
    );
    let fifth = r.run(MIX_TEST);
    assert_eq!(fifth.code, 0, "{}\n{}", fifth.stdout, fifth.stderr);
    assert!(fifth.ran().contains(&"Shop.PluginTest".to_string()));
    // Nothing changed: nothing runs but an audit's check.
    let sixth = r.run(MIX_TEST);
    assert_eq!(sixth.code, 0, "{}\n{}", sixth.stdout, sixth.stderr);
    assert!(sixth.ran().len() <= 1, "{:?}", sixth.ran());
}

// covers: C-115, C-117
#[test]
fn an_umbrella_app_reads_the_apps_it_depends_on() {
    if !has_mix() {
        return;
    }
    let app = |name: &str, deps: &str| {
        format!(
            "defmodule {}.MixProject do\n  use Mix.Project\n\n  def project do\n    [app: :{name}, version: \"0.1.0\", build_path: \"../../_build\", config_path: \"../../config/config.exs\", deps_path: \"../../deps\", lockfile: \"../../mix.lock\", elixir: \"~> 1.14\", deps: [{deps}]]\n  end\nend\n",
            capitalize(name)
        )
    };
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, always: []}")),
        file(
            "mix.exs",
            "defmodule Umb.MixProject do\n  use Mix.Project\n\n  def project do\n    [apps_path: \"apps\", version: \"0.1.0\", deps: []]\n  end\nend\n",
        ),
        file("config/config.exs", "import Config\n"),
        file("apps/core/mix.exs", &app("core", "")),
        file(
            "apps/core/lib/core.ex",
            "defmodule Core do\n  def hello, do: :world\nend\n",
        ),
        file("apps/core/test/test_helper.exs", "ExUnit.start()\n"),
        file(
            "apps/core/test/core_test.exs",
            &test_file("CoreTest", "assert Core.hello() == :world"),
        ),
        file(
            "apps/web/mix.exs",
            &app("web", "{:core, in_umbrella: true}"),
        ),
        file(
            "apps/web/lib/web.ex",
            "defmodule Web do\n  def page, do: {:ok, Core.hello()}\nend\n",
        ),
        file("apps/web/test/test_helper.exs", "ExUnit.start()\n"),
        file(
            "apps/web/test/web_test.exs",
            &test_file("WebTest", "assert Web.page() == {:ok, :world}"),
        ),
        file(
            "apps/web/test/plain_test.exs",
            &test_file("PlainTest", "assert 1 + 1 == 2"),
        ),
    ];
    let r = Repo::new(&files);
    let first = r.run(MIX_TEST);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.ran(),
        ["CoreTest", "PlainTest", "WebTest"],
        "{}",
        first.stdout
    );
    let reads = r.reads("apps/web/test/web_test.exs");
    assert!(
        reads.contains(&"apps/core/lib/core.ex".to_string()),
        "{reads:?}"
    );
    assert!(
        reads.contains(&"config/config.exs".to_string()),
        "{reads:?}"
    );
    // Core changes: the web app's test file that calls it runs again, with core's own.
    r.write(
        "apps/core/lib/core.ex",
        "defmodule Core do\n  def hello, do: :earth\nend\n",
    );
    let second = r.run(MIX_TEST);
    assert_ne!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    let ran = second.ran();
    assert!(
        ran.contains(&"CoreTest".to_string()) && ran.contains(&"WebTest".to_string()),
        "{ran:?}\n{}",
        second.stdout
    );
}

fn capitalize(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

// covers: C-114
#[test]
fn mix_test_in_a_form_hushrun_doesnt_read_is_left_alone_and_says_why() {
    if !has_mix() {
        return;
    }
    let r = Repo::new(&shop("reuse"));
    let ran = r.run(&["run", "unit", "--", "mix", "test", "--stale"]);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr
            .contains("`mix test --stale` chooses test files from its own earlier runs"),
        "{}",
        ran.stderr
    );
}
