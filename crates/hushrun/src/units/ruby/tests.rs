use super::*;

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_string).collect()
}

fn tree_of(paths: &[&str]) -> Tree {
    let mut bytes = format!("{}\n", hushrun_core::CANONICALIZATION).into_bytes();
    let mut paths = paths.to_vec();
    paths.sort();
    for path in paths {
        bytes.extend(format!("f {} {path}\0", "0".repeat(64)).as_bytes());
    }
    Tree::from_manifest(&bytes).unwrap()
}

fn facts(value: serde_json::Value) -> FactsFile {
    serde_json::from_value(value).unwrap()
}

// covers: C-82
#[test]
fn rspec_and_minitest_are_recognized_however_they_start() {
    for (command, runner) in [
        ("rspec", Runner::RSpec),
        ("bundle exec rspec spec/models", Runner::RSpec),
        ("bin/rspec --format progress", Runner::RSpec),
        ("bin/bundle exec rspec", Runner::RSpec),
        ("bin/rails test", Runner::Minitest),
        ("rails test:all", Runner::Minitest),
        ("bundle exec rails test test/models", Runner::Minitest),
        ("bundle exec rake test", Runner::Minitest),
        ("bin/rake spec", Runner::RSpec),
        ("rake", Runner::Either),
        ("ruby -Itest test/cart_test.rb", Runner::Either),
        ("ruby -I lib -S rspec", Runner::RSpec),
    ] {
        assert_eq!(
            recognize(&words(command)),
            Some(Ok(Recognized { runner })),
            "{command}"
        );
    }
    for command in [
        "bundle install",
        "bundle exec rubocop",
        "rails db:migrate",
        "rubocop",
        "pytest",
        "jruby -S rspec",
    ] {
        assert_eq!(recognize(&words(command)), None, "{command}");
    }
    for command in [
        "rspec --dry-run",
        "bundle exec rspec --only-failures",
        "rspec --next-failure",
        "rspec --bisect=verbose",
        "bin/spring rspec",
        "bundle exec spring rails test",
        "ruby --disable=rubyopt test/a_test.rb",
    ] {
        assert!(
            matches!(recognize(&words(command)), Some(Err(_))),
            "{command}"
        );
    }
}

// covers: C-82
#[test]
fn the_plugin_loads_beside_what_the_environment_says() {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let vars = variables(
        Path::new("/c/adapters/ruby-1"),
        Some("/repo/lib".into()),
        Some("-W0".into()),
    );
    let get = |name: &str| {
        vars.iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert_eq!(
        get("RUBYLIB"),
        OsString::from(format!("/c/adapters/ruby-1{sep}/repo/lib"))
    );
    assert_eq!(get("RUBYOPT"), OsString::from("-W0 -rhushrun_ruby"));
    assert_eq!(get("DISABLE_BOOTSNAP_COMPILE_CACHE"), OsString::from("1"));
    let bare = variables(Path::new("/c/a b"), None, Some("".into()));
    assert_eq!(bare[0].1, OsString::from("/c/a b"));
    assert_eq!(bare[1].1, OsString::from("-rhushrun_ruby"));
}

// covers: C-82
#[test]
fn the_plugin_is_written_once_by_its_contents() {
    let dir = tempfile::tempdir().unwrap();
    let first = install(dir.path()).unwrap();
    assert!(
        first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ruby-")
    );
    let file = first.join("hushrun_ruby.rb");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), PLUGIN);
    assert_eq!(install(dir.path()).unwrap(), first);
}

// covers: C-83, C-84
#[test]
fn what_each_process_wrote_comes_to_one_unit_each() {
    let tree = tree_of(&[
        "Gemfile",
        "lib/shop.rb",
        "lib/shop/cart.rb",
        "spec/cart_spec.rb",
        "spec/fixtures/a.yml",
        "spec/fixtures/b.yml",
        "spec/greet_spec.rb",
        "spec/spec_helper.rb",
    ]);
    let worker = |pid: u32, finished: u64| {
        facts(serde_json::json!({
            "schema": "hushrun.unit-facts/v1", "adapter": WORD, "failuresByUnit": true,
            "start": "s", "pid": pid,
            "settings": {"framework": "rspec",
                         "testFiles": ["spec/cart_spec.rb", "spec/greet_spec.rb"]},
            "units": [
                {"unit": "spec/cart_spec.rb", "selected": 2, "finished": finished,
                 "reads": ["lib/shop.rb", "lib/shop/cart.rb", "spec/spec_helper.rb", "/elsewhere.rb"],
                 "dirs": ["spec/fixtures"],
                 "tests": [{"id": "[1:1]", "finished": true}]},
                {"unit": "spec/greet_spec.rb", "deselected": true, "reads": []},
                // A unit the runner didn't list is none.
                {"unit": "spec/spec_helper.rb", "selected": 1, "finished": 1, "reads": []},
            ],
        }))
    };
    assert!(observe(&[], &tree).unwrap().is_none());
    // Two processes of one start, each finishing one of the two examples.
    let seen = observe(&[worker(1, 1), worker(2, 1)], &tree)
        .unwrap()
        .unwrap();
    let cart = &seen.units["spec/cart_spec.rb"];
    assert!(cart.facts.complete && !cart.facts.failed);
    assert_eq!(
        cart.reads.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "lib/shop.rb",
            "lib/shop/cart.rb",
            "spec/cart_spec.rb",
            "spec/fixtures/a.yml",
            "spec/fixtures/b.yml",
            "spec/spec_helper.rb"
        ]
    );
    assert!(seen.units["spec/greet_spec.rb"].facts.deselected);
    assert!(!seen.units.contains_key("spec/spec_helper.rb"));
    assert_eq!(seen.unit_files.len(), 2);
    assert!(seen.failures_by_unit);
    // One process alone finished one of two: not whole.
    let one = observe(&[worker(1, 1)], &tree).unwrap().unwrap();
    assert!(!one.units["spec/cart_spec.rb"].facts.complete);
    // The adapter's own error: nothing of the run is told.
    let mut broken = worker(1, 2);
    broken.errors.push("rspec: NoMethodError: x".into());
    assert!(
        observe(&[broken], &tree)
            .unwrap_err()
            .contains("NoMethodError")
    );
}

// covers: C-82, C-84
#[test]
fn a_run_that_failed_outside_every_test_keeps_no_pass() {
    let tree = tree_of(&["test/a_test.rb"]);
    let file = facts(serde_json::json!({
        "schema": "hushrun.unit-facts/v0", "adapter": WORD, "failuresByUnit": false,
        "notKept": "minitest failed outside every test (…), so no pass of the run is kept",
        "settings": {"framework": "minitest", "testFiles": ["test/a_test.rb"]},
        "units": [{"unit": "test/a_test.rb", "selected": 1, "finished": 1, "reads": []}],
    }));
    let seen = observe(&[file], &tree).unwrap().unwrap();
    assert!(!seen.failures_by_unit);
    assert!(seen.not_kept.unwrap().contains("outside every test"));
}

// covers: C-82
#[test]
fn a_runner_named_by_command_that_wrote_nothing_says_why() {
    let named = Recognized {
        runner: Runner::RSpec,
    };
    assert!(named.nothing_written().unwrap().contains("RUBYOPT"));
    let rake = Recognized {
        runner: Runner::Either,
    };
    assert!(rake.nothing_written().is_none());
    assert_eq!(rake.runner(), "RSpec or Minitest");
}
