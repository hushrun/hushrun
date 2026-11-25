//! \[ADR 0114\] Test files and tests with RSpec and Minitest (C-30 to C-34, C-39, C-82 to C-85): a
//! unit is a test file, what it read is the code that ran for it, what that code required and
//! opened, and what code outside the repository loaded before it; each test is told on its own;
//! and reuse never misses a failure.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- rspec` would, through the Ruby adapter's own type, `ruby::Recognized`, and the
//! code every adapter shares, and never starts the `hushrun` binary (`common/in_process.rs`).
//! Ruby itself runs as COMMAND, for real. What only the CLI shows of it (its lines, the result it
//! records) is in `e2e_ruby.rs`.
//!
//! The fixtures need CRuby 3.1 or later and gems installed outside the repository:
//! `HUSHRUN_TEST_RUBY` names the `ruby` to run, `HUSHRUN_TEST_RUBY_GEMS` a gem directory with
//! RSpec 3.13, Minitest 6 and Rake (CI: `gem install --install-dir <dir> rspec:3.13.2
//! minitest:6.1.0 rake:13.4.2`), and `HUSHRUN_TEST_RUBY_GEMS5` one with Minitest 5 and Rake.
//! Each test runs on what it finds, says so and passes where Ruby is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which needs all three.

#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/ruby.rs"]
mod ruby_fixtures;
#[path = "common/store.rs"]
mod store;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, ruby};

use in_process::{Ran, Repo};
use ruby_fixtures::*;

/// What the Ruby adapter answers that never changes in a run, for the code every adapter shares.
fn profile(adapter: &ruby::Recognized) -> Profile {
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

/// `hushrun run unit -- ARGS`, in-process, through the Ruby adapter alone.
fn run(r: &Repo, args: &[&str]) -> Ran {
    let env: &[(&str, &str)] = &[];
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to the Ruby adapter (`ruby::recognized`), with no
    // trait object.
    match ruby::recognize(hushrun::units::command::without_env(&command)) {
        Some(Ok(adapter)) => observe(r, args, env, &adapter),
        Some(Err(why)) => in_process::Run::new(r, "unit", args, env).unobserved(&why),
        None => panic!("{args:?} starts neither RSpec nor Minitest"),
    }
}

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order. Every call is on `ruby::Recognized` itself, which the static map resolves to its
/// own `impl Adapter`; through a trait object or a generic it would reach every adapter
/// (CONTRIBUTING.md, "Which tests CI runs again"). Another adapter's test file copies this
/// function and changes the type.
fn observe(r: &Repo, args: &[&str], env: &[(&str, &str)], adapter: &ruby::Recognized) -> Ran {
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
            // Ruby's adapter keeps the trait's `plan`, which is its `plan_from_files`.
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

const RSPEC: &[&str] = &["rspec"];

// covers: C-30, C-31, C-32, C-82, C-83
#[test]
fn rspec_test_files_are_observed_from_what_ran_and_what_it_required() {
    for ruby in rubies(false) {
        let r = repo(&ruby, &rspec_shop("observe"));
        let first = run(&r, RSPEC);
        assert_eq!(first.code, 0, "{}: {}", ruby.name, first.said());
        assert_eq!(
            first.line("unit: with"),
            "unit: with `tests: reuse`, all 7 test files would have run: none had passed here yet on the files it read"
        );
        // What its spec_helper required, to the end, and the file a greeting it didn't run read.
        assert_eq!(
            reads(&r, "spec/cart_spec.rb"),
            [
                "lib/shop.rb",
                "lib/shop/cart.rb",
                "lib/shop/pricing.rb",
                "spec/cart_spec.rb",
                "spec/spec_helper.rb",
                "spec/support/money.rb"
            ]
        );
        // The autoload it ran, and the file that code opened.
        let greet = reads(&r, "spec/greet_spec.rb");
        assert!(
            greet.contains(&"lib/shop/greet.rb".to_string())
                && greet.contains(&"data/hello.yml".to_string()),
            "{greet:?}"
        );
        let map = r.map();
        assert_eq!(map["unitFiles"].as_array().unwrap().len(), 7, "{map:#}");
        r.write("spec/greet_spec.rb", "RSpec.describe \"greet\" do\n  it(\"says hi\") { expect(Shop::Greet.hello).to eq(\"hi\") }\nend\n");
        let changed = run(&r, RSPEC);
        assert_eq!(changed.code, 0, "{}", changed.said());
        assert!(
            hushrun_testkit::lines::without_saving(changed.line("unit: with")).starts_with(
                "unit: with `tests: reuse`, 1 of 7 test files would have run; 6 already passed on the files they read"
            ),
            "{}",
            changed.said()
        );
        // A file added where spec_helper's code lists: every spec file read that directory.
        r.write("spec/support/tax.rb", "TAX = 1\n");
        let added = run(&r, RSPEC);
        assert_eq!(added.code, 0, "{}", added.said());
        assert!(
            added
                .line("unit: with")
                .starts_with("unit: with `tests: reuse`, all 7 test files would have run"),
            "{}",
            added.said()
        );
    }
}

// covers: C-34, C-82, C-84
#[test]
fn rspec_leaves_out_test_files_that_already_passed_and_never_a_failure() {
    for ruby in rubies(false) {
        let r = repo(&ruby, &rspec_shop("reuse"));
        let first = run(&r, RSPEC);
        assert_eq!(first.code, 0, "{}", first.said());
        assert_eq!(
            first.line("unit: every"),
            "unit: every test file runs: no run here has listed RSpec's test files yet"
        );
        r.write(
            "spec/greet_spec.rb",
            "RSpec.describe \"greet\" do\n  it(\"says hi\") { expect(Shop::Greet.hello).to eq(\"hi\") }\nend\n",
        );
        let second = run(&r, RSPEC);
        assert_eq!(second.code, 0, "{}", second.said());
        let line = second.line("unit: ");
        assert!(
            line.starts_with(
                "unit: 2 of 7 test files run; 5 already passed on the files they read"
            ),
            "{}",
            second.said()
        );
        // RSpec left them out before their hooks: the changed file's example and the audited
        // file's (one or two, as the audit chooses by key and start) of the 8 ran.
        assert!(
            ["2 examples, 0 failures", "3 examples, 0 failures"]
                .iter()
                .any(|line| second.stdout.contains(line)),
            "{}",
            second.said()
        );
        // A change every spec file reads, which breaks one: it runs, fails, and the run fails.
        r.write(
            "lib/shop/pricing.rb",
            "module Shop\n  module Pricing\n    def self.price(x)\n      x * 3\n    end\n  end\nend\n",
        );
        let broken = run(&r, RSPEC);
        assert_ne!(broken.code, 0, "{}", broken.said());
        assert!(
            broken.stdout.contains("8 examples, 1 failure"),
            "{}",
            broken.said()
        );
        // Nothing else changed: the failure runs again, and fails again.
        let again = run(&r, RSPEC);
        assert_ne!(again.code, 0, "{}", again.said());
        assert!(again.stdout.contains("1 failure"), "{}", again.said());
    }
}

// covers: C-39, C-84, C-85
#[test]
fn rspec_examples_are_reused_one_by_one() {
    for ruby in rubies(false) {
        let r = repo(&ruby, &rspec_shop("{mode: reuse, unit: test, reads: test}"));
        let first = run(&r, RSPEC);
        assert_eq!(first.code, 0, "{}", first.said());
        // One example more in a file: its file runs, and only the new example in it.
        r.write(
            "spec/cart_spec.rb",
            "RSpec.describe Shop::Cart do\n  it(\"totals\") { expect(Shop::Cart.new.total([1, 2])).to eq 6 }\n  context \"empty\" do\n    it(\"is zero\") { expect(Shop::Cart.new.total([])).to eq 0 }\n  end\n  it(\"totals one\") { expect(Shop::Cart.new.total([1])).to eq 2 }\nend\n",
        );
        let second = run(&r, RSPEC);
        assert_eq!(second.code, 0, "{}", second.said());
        // The plan counts the tests it knows: the new one runs besides.
        let line = second.line("unit: ");
        assert!(
            line.starts_with("unit: 3 of 8 tests run, in 2 of 7 test files"),
            "{}",
            second.said()
        );
        assert!(
            second.stdout.contains("4 examples, 0 failures"),
            "{}",
            second.said()
        );
        let map = r.map();
        let tests = map["units"]["spec/cart_spec.rb"]["tests"].clone();
        assert!(
            tests.get("[1:1]").is_some() && tests.get("[1:2:1]").is_some(),
            "{map:#}"
        );
    }
}

const RAKE: &[&str] = &["rake", "test"];

// covers: C-30, C-34, C-82, C-83, C-84
#[test]
fn minitest_test_files_are_observed_and_reused_under_rake() {
    for ruby in rubies(true) {
        let r = repo(&ruby, &minitest_shop("reuse"));
        let first = run(&r, RAKE);
        assert_eq!(first.code, 0, "{}: {}", ruby.name, first.said());
        // What it and its test_helper required, to the end; what another test file loaded
        // through an autoload only when that one ran first (Minitest's order is random); and no
        // other test file.
        let cart = reads(&r, "test/cart_test.rb");
        for read in [
            "lib/shop.rb",
            "lib/shop/cart.rb",
            "lib/shop/pricing.rb",
            "test/cart_test.rb",
            "test/test_helper.rb",
        ] {
            assert!(cart.contains(&read.to_string()), "{}: {cart:?}", ruby.name);
        }
        assert!(
            cart.iter().all(|p| !p.starts_with("test/")
                || p == "test/cart_test.rb"
                || p == "test/test_helper.rb"),
            "{}: {cart:?}",
            ruby.name
        );
        r.write(
            "test/greet_test.rb",
            "require_relative \"test_helper\"\nclass GreetTest < Minitest::Test\n  def test_hello\n    assert_equal(\"hi\", Shop::Greet.hello)\n  end\nend\n",
        );
        let second = run(&r, RAKE);
        assert_eq!(second.code, 0, "{}: {}", ruby.name, second.said());
        assert!(
            second.line("unit: ").starts_with(
                "unit: 2 of 7 test files run; 5 already passed on the files they read"
            ),
            "{}: {}",
            ruby.name,
            second.said()
        );
        // The changed file's test and the audited file's (one or two) of the 8 ran.
        assert!(
            ["2 runs,", "3 runs,"]
                .iter()
                .any(|line| second.stdout.contains(line)),
            "{}: {}",
            ruby.name,
            second.said()
        );
        // A failure in a file every test file reads: all run, and the run fails.
        r.write(
            "lib/shop/pricing.rb",
            "module Shop\n  module Pricing\n    def self.price(x)\n      x * 3\n    end\n  end\nend\n",
        );
        let broken = run(&r, RAKE);
        assert_ne!(broken.code, 0, "{}: {}", ruby.name, broken.said());
        let again = run(&r, RAKE);
        assert_ne!(again.code, 0, "{}: {}", ruby.name, again.said());
    }
}

// covers: C-39, C-84
#[test]
fn minitest_tests_are_reused_one_by_one() {
    for ruby in rubies(true) {
        let r = repo(&ruby, &minitest_shop("{mode: reuse, unit: test}"));
        let first = run(&r, RAKE);
        assert_eq!(first.code, 0, "{}: {}", ruby.name, first.said());
        let map = r.map();
        let tests = &map["units"]["test/cart_test.rb"]["tests"];
        assert!(
            tests.get("CartTest#test_total").is_some()
                && tests.get("CartTest#test_empty").is_some(),
            "{map:#}"
        );
        r.write(
            "test/cart_test.rb",
            "require \"test_helper\"\nclass CartTest < Minitest::Test\n  def test_total\n    assert_equal 6, Shop::Cart.new.total([1, 2])\n  end\n\n  def test_empty\n    assert_equal 0, Shop::Cart.new.total([])\n  end\n\n  def test_one\n    assert_equal 2, Shop::Cart.new.total([1])\n  end\nend\n",
        );
        let second = run(&r, RAKE);
        assert_eq!(second.code, 0, "{}: {}", ruby.name, second.said());
        assert!(
            second
                .line("unit: ")
                .starts_with("unit: 3 of 8 tests run, in 2 of 7 test files"),
            "{}: {}",
            ruby.name,
            second.said()
        );
    }
}

/// The adapter's directory in a cache, once a run wrote it.
#[allow(dead_code, reason = "kept for a failing test to look into")]
fn adapter(cache: &Path) -> Option<PathBuf> {
    std::fs::read_dir(cache.join("adapters"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ruby-")
        })
}
