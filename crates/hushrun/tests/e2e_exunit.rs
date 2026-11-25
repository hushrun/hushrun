//! \[ADR 0122\] The ExUnit adapter end to end: the `hushrun` binary, as a person or CI runs it, for
//! what only the CLI does with `mix test`'s test files. Every other test of the adapter is
//! in-process, in `units_exunit.rs`.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: that the CLI gives
//! COMMAND the ExUnit adapter, the run's lines as the CLI says them, and the check's own result.
//! The tools are those of `units_exunit.rs`.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/exunit.rs"]
mod ex;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use cli::Cli as _;
use ex::*;

// covers: C-114, C-116, C-117
#[test]
fn mix_test_through_the_cli_says_what_it_reuses_and_records_it() {
    if !has_mix() {
        return;
    }
    let r = repo(&shop("{mode: reuse, always: []}"));
    let mix_test = ["run", "unit", "--", "mix", "test", "--trace", "--seed", "0"];
    let first = r.run(&mix_test, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(modules_ran(&first.stdout).len(), 5, "{}", first.stdout);
    assert_eq!(r.newest_result()["units"]["total"], 5, "{}", first.stderr);
    write(
        &r,
        "lib/shop/greet.ex",
        "defmodule Shop.Greet do\n  def hello, do: \"h\" <> \"i\"\nend\n",
    );
    let second = r.run(&mix_test, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    let plan = second
        .stderr
        .lines()
        .find(|l| is_plan_line(l))
        .unwrap_or_else(|| panic!("no plan line in:\n{}", second.stderr));
    assert!(
        plan.starts_with("hushrun: unit: ") && plan.contains("of 5 test files run"),
        "{}",
        second.stderr
    );
    let ran = modules_ran(&second.stdout);
    assert!(ran.contains(&"Shop.GreetTest".to_string()), "{ran:?}");
    assert_eq!(r.newest_result()["units"]["total"], 5, "{}", second.stderr);
}
