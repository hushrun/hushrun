//! \[ADR 0114\] The Ruby adapter end to end: the `hushrun` binary, as a person or CI runs it, for
//! what only the CLI does with RSpec's and Minitest's test files. Every other test of the adapter
//! is in-process, in `units_ruby.rs`.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: that the CLI gives
//! COMMAND the Ruby adapter, the run's lines as the CLI says them, and the check's own result.
//! The tools are those of `units_ruby.rs`.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/ruby.rs"]
mod ruby_fixtures;
#[path = "common/store.rs"]
mod store;

use serde_json::json;

use cli::Cli as _;
use ruby_fixtures::*;

// covers: C-30, C-32, C-34, C-82
#[test]
fn rspec_through_the_cli_says_what_it_reuses_and_records_it() {
    for ruby in rubies(false) {
        let r = repo(&ruby, &rspec_shop("reuse"));
        let rspec = ["run", "unit", "--", "rspec"];
        let first = r.run(&rspec, &[]);
        assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
        assert_eq!(
            first.line("hushrun: unit: every"),
            "hushrun: unit: every test file runs: no run here has listed RSpec's test files yet"
        );
        assert_eq!(
            r.newest_result()["units"],
            json!({"total": 7, "ran": 7, "reused": 0})
        );
        r.write(
            "spec/greet_spec.rb",
            "RSpec.describe \"greet\" do\n  it(\"says hi\") { expect(Shop::Greet.hello).to eq(\"hi\") }\nend\n",
        );
        let second = r.run(&rspec, &[]);
        assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
        assert!(
            second.line("hushrun: unit: ").starts_with(
                "hushrun: unit: 2 of 7 test files run; 5 already passed on the files they read"
            ),
            "{}",
            second.stderr
        );
        let result = r.newest_result();
        let saved = result["units"]["savedMs"].clone();
        assert_eq!(
            result["units"],
            json!({"total": 7, "ran": 2, "reused": 5, "savedMs": saved})
        );
    }
}

// covers: C-30, C-32, C-82
#[test]
fn rake_test_through_the_cli_gets_the_ruby_adapter() {
    for ruby in rubies(false) {
        let r = repo(&ruby, &minitest_shop("reuse"));
        let rake = ["run", "unit", "--", "rake", "test"];
        let first = r.run(&rake, &[]);
        assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
        assert_eq!(
            first.line("hushrun: unit: every"),
            "hushrun: unit: every test file runs: no run here has listed Minitest's test files yet"
        );
        assert_eq!(
            r.newest_result()["units"],
            json!({"total": 7, "ran": 7, "reused": 0})
        );
    }
}
