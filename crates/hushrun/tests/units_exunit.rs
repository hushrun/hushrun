//! \[ADR 0122\] Test files with `mix test` (C-114 to C-117): a unit is a test file, what it read is
//! what its code references to the end, what ran while it ran, and the project's configuration,
//! and reuse never misses a failure.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- mix test` would, through the ExUnit adapter's own type, `exunit::ExUnit`, and the
//! code every adapter shares, and never starts the `hushrun` binary (`common/in_process.rs`).
//! Mix itself runs as COMMAND, for real. What only the CLI shows of it (its lines, the result it
//! records) is in `e2e_exunit.rs`.
//!
//! The projects use Elixir's standard library only, and need Elixir and Erlang/OTP:
//! `HUSHRUN_TEST_MIX` names the `mix` to use (by default the one on `PATH`), with `erl` and `erlc`
//! beside it. The tests say so and pass where it is missing, unless `HUSHRUN_TEST_TOOLS=required`
//! and `HUSHRUN_TEST_MIX` are both set, as CI's Linux job sets them.

#[path = "common/exunit.rs"]
mod ex;
#[path = "common/hex.rs"]
mod hex;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::collections::BTreeSet;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, exunit};

use ex::*;
use in_process::{Ran, Repo};

/// What the ExUnit adapter answers that never changes in a run, for the code every adapter
/// shares.
fn profile(adapter: &exunit::ExUnit) -> Profile {
    Profile {
        word: adapter.word(),
        runner: adapter.runner(),
        test_file: adapter.test_file(),
        tests: adapter.tests(),
        hints: adapter.hints(),
        binds: adapter.binds(),
        keys: adapter.keys(),
        not_by_test: adapter.not_by_test(),
        not_watched: adapter.not_watched(),
    }
}

/// `hushrun run unit -- ARGS`, in-process, through the ExUnit adapter alone.
fn run(r: &Repo, args: &[&str]) -> Ran {
    let env: &[(&str, &str)] = &[];
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to the ExUnit adapter (`exunit::recognized`), with no
    // trait object.
    match exunit::recognize(&command) {
        Some(Ok(adapter)) => observe(r, args, env, &adapter),
        Some(Err(why)) => in_process::Run::new(r, "unit", args, env).unobserved(&why),
        None => panic!("{args:?} doesn't run `mix test`"),
    }
}

/// The plan's line, which counts test files.
fn plan_line(ran: &Ran) -> &str {
    ran.lines
        .iter()
        .find(|l| is_plan_line(l))
        .unwrap_or_else(|| panic!("no plan line in:\n{}", ran.said()))
}

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order. Every call is on `exunit::ExUnit` itself, which the static map resolves to its
/// own `impl Adapter`; through a trait object or a generic it would reach every adapter
/// (CONTRIBUTING.md, "Which tests CI runs again"). Another adapter's test file copies this
/// function and changes the type.
fn observe(r: &Repo, args: &[&str], env: &[(&str, &str)], adapter: &exunit::ExUnit) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // ExUnit's adapter keeps the trait's `plan`, which is its `plan_from_files`.
            Ok(context) => {
                let planning = adapter.plan_from_files(&observer.plan_at(&input, &context));
                observer.planned(&input, context, planning)
            }
            Err(planned) => planned,
        };
        drop(input);
        run.planned(&observer, planned);
    }
    // COMMAND, once, or \[C-32\] once more with nothing left out when the adapter left out what
    // the plan didn't list.
    let mut starts = 0;
    let (code, mut observed) = loop {
        starts += 1;
        let start = adapter.start_variables(observer.run_dir());
        let (code, passed) = run.start_command(&observer, &start);
        let end = run.finish_input(passed);
        let observed = match observer.read_context(&end) {
            Ok(context) => {
                let read = adapter.read(&observer.read_at(&end, &context));
                observer.finished(&end, context, read)
            }
            Err(observed) => observed.map(|observed| *observed),
        };
        drop(end);
        if starts == 1 && observed.as_ref().is_some_and(|o| o.left_out.is_some()) {
            let extras = adapter.again(observer.root(), observer.cwd(), &BTreeSet::new());
            if observer.whole_again(extras).is_ok() {
                continue;
            }
        }
        break (code, observed);
    };
    // \[C-34\] An audited unit that failed runs once more, alone.
    if let Some(o) = observed.as_mut().filter(|o| !o.again.is_empty()) {
        let wanted: BTreeSet<String> = o.again.keys().cloned().collect();
        let extras = adapter.again(observer.root(), o.cwd(), o.unit_files());
        let ended = match observer.again(o, &wanted, extras) {
            Some(again) => {
                let start = adapter.start_variables(again.dir());
                let again = again.with_start(start);
                let first = adapter.start_variables(observer.run_dir());
                let passed = run.start_again(&observer, &first, &again);
                let files = again.files();
                let map = in_process::no_map();
                let read = adapter
                    .read(&observer.again_read_at(&files, o, &run.tree, passed, &map))
                    .ok()
                    .flatten();
                observer.again_ended(&again, o, passed, read)
            }
            None => in_process::Run::not_again(&wanted),
        };
        run.disagreements(o, ended);
    }
    run.record(code, observed)
}

const MIX_TEST: &[&str] = &["mix", "test", "--trace", "--seed", "0"];

// covers: C-114, C-115, C-116
#[test]
fn test_files_read_what_their_code_references_and_what_ran() {
    if !has_mix() {
        return;
    }
    let r = repo(&shop("observe"));
    let first = run(&r, MIX_TEST);
    assert_eq!(first.code, 0, "{}", first.said());
    assert_eq!(
        modules_ran(&first.stdout),
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
    let reads = read_by(&r, "test/cart_test.exs");
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
    let reads = read_by(&r, "test/tax_test.exs");
    assert!(reads.contains(&"lib/shop/tax.ex".to_string()), "{reads:?}");
    // A module called by a name made at run time is read through what ran while it ran.
    let reads = read_by(&r, "test/plugin_test.exs");
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
    let r = repo(&shop("{mode: reuse, always: []}"));
    let first = run(&r, MIX_TEST);
    assert_eq!(first.code, 0, "{}", first.said());
    // Until ExUnit has reported the check's test files here, the plan says nothing: every test
    // file runs.
    assert_eq!(modules_ran(&first.stdout).len(), 5, "{}", first.stdout);
    // The greeting changes: its test file runs, and the others' passes hold (an audit may run
    // one of them as a check).
    write(
        &r,
        "lib/shop/greet.ex",
        "defmodule Shop.Greet do\n  def hello, do: \"h\" <> \"i\"\nend\n",
    );
    let second = run(&r, MIX_TEST);
    assert_eq!(second.code, 0, "{}", second.said());
    assert!(
        plan_line(&second).contains("of 5 test files run"),
        "{}",
        second.said()
    );
    let ran = modules_ran(&second.stdout);
    assert!(ran.contains(&"Shop.GreetTest".to_string()), "{ran:?}");
    assert!(
        ran.len() <= 2,
        "{ran:?}\n{}\n{:#}",
        second.said(),
        r.map()["units"]
    );
    assert!(
        second.stdout.contains("excluded"),
        "ExUnit's own filter left them out:\n{}",
        second.stdout
    );
    // The price changes: the cart's test file, which calls it through the cart, runs too.
    write(
        &r,
        "lib/shop/price.ex",
        "defmodule Shop.Price do\n  def of(x), do: x + x\nend\n",
    );
    let third = run(&r, MIX_TEST);
    assert_eq!(third.code, 0, "{}", third.said());
    let ran = modules_ran(&third.stdout);
    assert!(
        ran.contains(&"Shop.PriceTest".to_string()) && ran.contains(&"Shop.CartTest".to_string()),
        "{ran:?}"
    );
    // The module only called by a name made at run time changes: its test file runs.
    write(
        &r,
        "lib/shop/plugin.ex",
        "defmodule Shop.Plugin do\n  def name, do: :other\nend\n",
    );
    let fourth = run(&r, MIX_TEST);
    assert_ne!(fourth.code, 0, "{}", fourth.said());
    assert!(
        modules_ran(&fourth.stdout).contains(&"Shop.PluginTest".to_string()),
        "{}",
        fourth.stdout
    );
    // A failure is never reused: fixed, it runs again, and passes.
    write(
        &r,
        "test/plugin_test.exs",
        &test_file(
            "Shop.PluginTest",
            "assert Module.concat([\"Shop\", \"Plugin\"]).name() == :other",
        ),
    );
    let fifth = run(&r, MIX_TEST);
    assert_eq!(fifth.code, 0, "{}", fifth.said());
    assert!(modules_ran(&fifth.stdout).contains(&"Shop.PluginTest".to_string()));
    // Nothing changed: nothing runs but an audit's check.
    let sixth = run(&r, MIX_TEST);
    assert_eq!(sixth.code, 0, "{}", sixth.said());
    assert!(
        modules_ran(&sixth.stdout).len() <= 1,
        "{:?}",
        modules_ran(&sixth.stdout)
    );
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
    let r = repo(&files);
    let first = run(&r, MIX_TEST);
    assert_eq!(first.code, 0, "{}", first.said());
    assert_eq!(
        modules_ran(&first.stdout),
        ["CoreTest", "PlainTest", "WebTest"],
        "{}",
        first.stdout
    );
    let reads = read_by(&r, "apps/web/test/web_test.exs");
    assert!(
        reads.contains(&"apps/core/lib/core.ex".to_string()),
        "{reads:?}"
    );
    assert!(
        reads.contains(&"config/config.exs".to_string()),
        "{reads:?}"
    );
    // Core changes: the web app's test file that calls it runs again, with core's own.
    write(
        &r,
        "apps/core/lib/core.ex",
        "defmodule Core do\n  def hello, do: :earth\nend\n",
    );
    let second = run(&r, MIX_TEST);
    assert_ne!(second.code, 0, "{}", second.said());
    let ran = modules_ran(&second.stdout);
    assert!(
        ran.contains(&"CoreTest".to_string()) && ran.contains(&"WebTest".to_string()),
        "{ran:?}\n{}",
        second.stdout
    );
}

// covers: C-114
#[test]
fn mix_test_in_a_form_hushrun_doesnt_read_is_left_alone_and_says_why() {
    if !has_mix() {
        return;
    }
    let r = repo(&shop("reuse"));
    let ran = run(&r, &["mix", "test", "--stale"]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(
        ran.said()
            .contains("`mix test --stale` chooses test files from its own earlier runs"),
        "{}",
        ran.said()
    );
}

/// \[ADR 0157\] The shop with a dependency from Hex, `leaf`, which a module and its test file
/// call, its `mix.lock` written by a network install from `server` and committed; `tests` its
/// mode.
fn shop_with_leaf(h: &hex::Hex, server: &hex::Server, tests: &str) -> Repo {
    let mut files = shop(tests);
    files.retain(|(path, _)| path != "mix.exs");
    files.push(file("mix.exs", &hex::mix_exs("shop", &[("leaf", "1.0.0")])));
    files.push(file(
        "lib/shop/leafy.ex",
        "defmodule Shop.Leafy do\n  def version, do: Leaf.version()\nend\n",
    ));
    files.push(file(
        "test/leafy_test.exs",
        &test_file("Shop.LeafyTest", "assert Shop.Leafy.version() == \"1.0.0\""),
    ));
    let mut r = repo(&files).clear(hex::Hex::CLEARED);
    for (name, value) in h.vars(server) {
        r = r.give(name, value);
    }
    r
}

/// `mix deps.get` in `r` with Hex's cache at `home`, from nothing: no `deps/`, no `_build`.
fn deps_get(r: &Repo, h: &hex::Hex, home: &std::path::Path) {
    let _ = std::fs::remove_dir_all(r.repo.join("deps"));
    let _ = std::fs::remove_dir_all(r.repo.join("_build"));
    let out = r
        .command(&h.mix.to_string_lossy())
        .arg("deps.get")
        .env("HEX_HOME", home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Seeds the tarballs of the Hex cache `from` into the empty Hex cache `into`, as a run seeds a
/// store's `hex` part: each one `r`'s `mix.lock` pins.
fn seed(r: &Repo, from: &std::path::Path, into: &std::path::Path) {
    use hushrun_packages::part::Part as _;
    use hushrun_packages::parts::hex::Hex as HexPart;
    let rows: Vec<hushrun_packages::store::Row> = std::fs::read_dir(from.join("packages/hexpm"))
        .unwrap()
        .flatten()
        .map(|e| {
            let bytes = std::fs::read(e.path()).unwrap();
            hushrun_packages::store::Row {
                path: format!("packages/hexpm/{}", e.file_name().to_string_lossy()),
                kind: hushrun_packages::store::Kind::File,
                bytes: bytes.len() as u64,
                sha256: hushrun_core::blocks::Hex32::sha256(&bytes),
                target: None,
                package: None,
                locked: None,
            }
        })
        .collect();
    use hushrun_core::blocks::Hex32;
    use hushrun_packages::store::{Class, PartEntry, RowsFile, VerifiedBy};
    // As a store's part: its folder holds the cache's `packages/hexpm/`.
    let read = hushrun_packages::state::ReadPart {
        entry: PartEntry {
            part: "hex".into(),
            class: Class::Download,
            tool: "fetched".into(),
            runtime: None,
            inputs: Hex32::sha256(b"inputs"),
            manifests: vec!["mix.lock".into()],
            identity: Hex32::sha256(b"identity"),
            rows: RowsFile {
                sha256: Hex32::sha256(b"rows"),
                bytes: 0,
                count: rows.len() as u64,
            },
            bytes: 0,
            verified_by: VerifiedBy::Lockfile,
        },
        rows,
        dir: from.to_path_buf(),
        behind: false,
    };
    let manifests = ["mix.lock".to_string()];
    let (dest, under) = HexPart
        .seed_into(&r.repo, &|name: &str| {
            (name == "HEX_HOME").then(|| into.as_os_str().to_os_string())
        })
        .unwrap();
    let admit = HexPart.admit(&r.repo, &manifests);
    let seeded =
        hushrun_packages::seed::seed(&read, under, &dest, &*admit, HexPart.links()).unwrap();
    assert_eq!((seeded.copied, seeded.changed.len()), (1, 0), "{seeded:?}");
}

// covers: C-115, K-10
#[test]
fn a_seeded_install_leaves_every_key_as_a_network_install_does() {
    if !has_mix() {
        return;
    }
    let Some(h) = hex::hex() else { return };
    let registry = tempfile::tempdir().unwrap();
    let public = hex::repository(&h, registry.path(), &[hex::Package::plain("leaf", "1.0.0")]);
    let server = hex::Server::start(&public);
    let units = [
        "test/cart_test.exs",
        "test/greet_test.exs",
        "test/leafy_test.exs",
        "test/plugin_test.exs",
        "test/price_test.exs",
        "test/tax_test.exs",
    ];
    // Every test file runs each time and records what it read: a network install's reads, then
    // a seeded install's, from no deps/ and no _build, with every tarball refused.
    let r = shop_with_leaf(&h, &server, "observe");
    let net = r.root.join("net-hex");
    deps_get(&r, &h, &net);
    hushrun_testkit::git::commit_all(&r.repo, "lock");
    let first = run(&r, MIX_TEST);
    assert_eq!(first.code, 0, "{}", first.said());
    let reads_net: Vec<Vec<String>> = units.iter().map(|u| read_by(&r, u)).collect();
    assert!(
        reads_net[2].contains(&"lib/shop/leafy.ex".to_string()),
        "{:?}",
        reads_net[2]
    );
    let seeded = r.root.join("seeded-hex");
    seed(&r, &net, &seeded);
    server.tarballs_asked();
    server.refuse_tarballs(true);
    deps_get(&r, &h, &seeded);
    assert_eq!(server.tarballs_asked(), Vec::<String>::new());
    let second = run(&r, MIX_TEST);
    assert_eq!(second.code, 0, "{}", second.said());
    assert_eq!(
        modules_ran(&second.stdout).len(),
        units.len(),
        "{}",
        second.stdout
    );
    let reads_seeded: Vec<Vec<String>> = units.iter().map(|u| read_by(&r, u)).collect();
    assert_eq!(reads_seeded, reads_net);
    // With reuse, after a network install and then a seeded one, every pass holds: nothing runs
    // but an audit's check.
    server.refuse_tarballs(false);
    let r = shop_with_leaf(&h, &server, "{mode: reuse, always: []}");
    let net = r.root.join("net-hex");
    deps_get(&r, &h, &net);
    hushrun_testkit::git::commit_all(&r.repo, "lock");
    let first = run(&r, MIX_TEST);
    assert_eq!(first.code, 0, "{}", first.said());
    assert_eq!(modules_ran(&first.stdout).len(), units.len());
    let seeded = r.root.join("seeded-hex");
    seed(&r, &net, &seeded);
    server.refuse_tarballs(true);
    deps_get(&r, &h, &seeded);
    let second = run(&r, MIX_TEST);
    assert_eq!(second.code, 0, "{}", second.said());
    assert!(
        modules_ran(&second.stdout).len() <= 1,
        "{:?}\n{}",
        modules_ran(&second.stdout),
        second.said()
    );
}
