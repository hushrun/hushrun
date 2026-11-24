//! \[ADR 0114\] Test files and tests with RSpec and Minitest (C-30 to C-34, C-39, C-82 to C-85): a
//! unit is a test file, what it read is the code that ran for it, what that code required and
//! opened, and what code outside the repository loaded before it; each test is told on its own;
//! and reuse never misses a failure.
//!
//! The fixtures need CRuby 3.1 or later and gems installed outside the repository:
//! `HUSHRUN_TEST_RUBY` names the `ruby` to run, `HUSHRUN_TEST_RUBY_GEMS` a gem directory with
//! RSpec 3.13, Minitest 6 and Rake (CI: `gem install --install-dir <dir> rspec:3.13.2
//! minitest:6.1.0 rake:13.4.2`), and `HUSHRUN_TEST_RUBY_GEMS5` one with Minitest 5 and Rake.
//! Each test runs on what it finds, says so and passes where Ruby is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which needs all three.

#[path = "common/store.rs"]
mod store;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;
use serde_json::Value;

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
    "HUSHRUN_RUBY_START",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "RUBYOPT",
    "RUBYLIB",
    "SPEC_OPTS",
    "TESTOPTS",
    "BUNDLE_GEMFILE",
    "GEM_HOME",
    "GEM_PATH",
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
    "HUSHRUN_CC",
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";

/// A Ruby these tests run on, with one gem directory.
#[derive(Clone)]
struct Ruby {
    ruby: PathBuf,
    gems: PathBuf,
    /// What the gem directory holds, for messages.
    name: &'static str,
}

impl Ruby {
    /// `PATH` with the gems' programs and Ruby's own first.
    fn path(&self) -> OsString {
        let mut dirs = vec![self.gems.join("bin")];
        if let Some(dir) = self.ruby.parent() {
            dirs.push(dir.to_path_buf());
        }
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        std::env::join_paths(dirs).expect("PATH")
    }
}

fn ruby_in(gems: &str, name: &'static str) -> Option<Ruby> {
    let ruby = PathBuf::from(std::env::var_os("HUSHRUN_TEST_RUBY")?);
    let gems = PathBuf::from(std::env::var_os(gems)?);
    let ok = Command::new(&ruby)
        .args([
            "-e",
            "exit(RUBY_ENGINE == 'ruby' && RUBY_VERSION >= '3.1' ? 0 : 1)",
        ])
        .output()
        .is_ok_and(|o| o.status.success());
    (ok && gems.is_dir()).then_some(Ruby { ruby, gems, name })
}

/// The Ruby with RSpec and Minitest 6; none, said, where it is missing, and a failure with
/// `HUSHRUN_TEST_TOOLS=required`.
fn rubies(with_minitest5: bool) -> Vec<Ruby> {
    let mut found: Vec<Ruby> = ruby_in("HUSHRUN_TEST_RUBY_GEMS", "RSpec and Minitest 6")
        .into_iter()
        .collect();
    let five = ruby_in("HUSHRUN_TEST_RUBY_GEMS5", "Minitest 5");
    let required = std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required");
    if required && (found.is_empty() || five.is_none()) {
        panic!(
            "Ruby (HUSHRUN_TEST_RUBY) with RSpec and Minitest 6 (HUSHRUN_TEST_RUBY_GEMS) and Minitest 5 (HUSHRUN_TEST_RUBY_GEMS5) is missing, and HUSHRUN_TEST_TOOLS=required"
        );
    }
    if with_minitest5 {
        found.extend(five);
    }
    if found.is_empty() {
        eprintln!(
            "note: skipping: Ruby 3.1 or later with RSpec and Minitest is missing (HUSHRUN_TEST_TOOLS=required makes this fail)"
        );
    }
    found
}

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    ruby: Ruby,
}

impl Repo {
    fn new(ruby: &Ruby, files: &[(String, String)]) -> Repo {
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
            ruby: ruby.clone(),
        };
        for (path, text) in files {
            r.write(path, text);
        }
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn clean(&self, cmd: &mut Command) {
        for var in CLEARED {
            cmd.env_remove(var);
        }
        hushrun_testkit::env::Hermetic::hermetic(cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        cmd.env("GEM_HOME", &self.ruby.gems)
            .env("GEM_PATH", &self.ruby.gems)
            .env("PATH", self.ruby.path());
    }

    fn run(&self, args: &[&str]) -> Ran {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        self.clean(&mut cmd);
        let output = cmd.args(args).output().expect("hushrun");
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
            .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
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
    fn line(&self, start: &str) -> &str {
        self.stderr
            .lines()
            .find(|l| l.starts_with(start))
            .unwrap_or_else(|| panic!("no line starting {start:?} in:\n{}", self.out()))
    }

    fn out(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A shop: a library its spec_helper requires whole, pricing a cart uses, a greeting that reads a
/// data file through an autoload, and five spec files that read only what every one does, so
/// that a plan has something to reuse.
fn rspec_shop(tests: &str) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".rspec", "--require spec_helper\n"),
        file(
            "lib/shop.rb",
            "require \"shop/pricing\"\nrequire_relative \"shop/cart\"\nmodule Shop\n  autoload :Greet, \"shop/greet\"\nend\n",
        ),
        file(
            "lib/shop/pricing.rb",
            "module Shop\n  module Pricing\n    def self.price(x)\n      x * 2\n    end\n  end\nend\n",
        ),
        file(
            "lib/shop/cart.rb",
            "module Shop\n  class Cart\n    def total(xs)\n      xs.sum { |x| Pricing.price(x) }\n    end\n  end\nend\n",
        ),
        file(
            "lib/shop/greet.rb",
            "module Shop\n  module Greet\n    def self.hello\n      File.read(File.expand_path(\"../../data/hello.yml\", __dir__)).strip\n    end\n  end\nend\n",
        ),
        file("data/hello.yml", "hi\n"),
        file(
            "spec/spec_helper.rb",
            "$LOAD_PATH.unshift File.expand_path(\"../lib\", __dir__)\nrequire \"shop\"\n",
        ),
        file(
            "spec/cart_spec.rb",
            "RSpec.describe Shop::Cart do\n  it(\"totals\") { expect(Shop::Cart.new.total([1, 2])).to eq 6 }\n  context \"empty\" do\n    it(\"is zero\") { expect(Shop::Cart.new.total([])).to eq 0 }\n  end\nend\n",
        ),
        file(
            "spec/greet_spec.rb",
            "RSpec.describe \"greet\" do\n  it(\"says hi\") { expect(Shop::Greet.hello).to eq \"hi\" }\nend\n",
        ),
    ];
    for n in 1..=5 {
        files.push(file(
            &format!("spec/same{n}_spec.rb"),
            "RSpec.describe \"same\" do\n  it(\"adds\") { expect(1 + 1).to eq 2 }\nend\n",
        ));
    }
    files
}

const RSPEC_RUN: &[&str] = &["run", "unit", "--", "rspec"];

// covers: C-30, C-31, C-32, C-82, C-83
#[test]
fn rspec_test_files_are_observed_from_what_ran_and_what_it_required() {
    for ruby in rubies(false) {
        let r = Repo::new(&ruby, &rspec_shop("observe"));
        let first = r.run(RSPEC_RUN);
        assert_eq!(first.code, 0, "{}: {}", ruby.name, first.out());
        assert_eq!(
            first.line("hushrun: unit: with"),
            "hushrun: unit: with `tests: reuse`, all 7 test files would have run: none had passed here yet on the files it read"
        );
        // What its spec_helper required, to the end, and the file a greeting it didn't run read.
        assert_eq!(
            r.reads("spec/cart_spec.rb"),
            [
                "lib/shop.rb",
                "lib/shop/cart.rb",
                "lib/shop/pricing.rb",
                "spec/cart_spec.rb",
                "spec/spec_helper.rb"
            ]
        );
        // The autoload it ran, and the file that code opened.
        let greet = r.reads("spec/greet_spec.rb");
        assert!(
            greet.contains(&"lib/shop/greet.rb".to_string())
                && greet.contains(&"data/hello.yml".to_string()),
            "{greet:?}"
        );
        let map = r.map();
        assert_eq!(map["unitFiles"].as_array().unwrap().len(), 7, "{map:#}");
        r.write("spec/greet_spec.rb", "RSpec.describe \"greet\" do\n  it(\"says hi\") { expect(Shop::Greet.hello).to eq(\"hi\") }\nend\n");
        let changed = r.run(RSPEC_RUN);
        assert_eq!(changed.code, 0, "{}", changed.out());
        assert!(
            hushrun_testkit::lines::without_saving(changed.line("hushrun: unit: with")).starts_with(
                "hushrun: unit: with `tests: reuse`, 1 of 7 test files would have run; 6 already passed on the files they read"
            ),
            "{}",
            changed.stderr
        );
    }
}

// covers: C-34, C-82, C-84
#[test]
fn rspec_leaves_out_test_files_that_already_passed_and_never_a_failure() {
    for ruby in rubies(false) {
        let r = Repo::new(&ruby, &rspec_shop("reuse"));
        let first = r.run(RSPEC_RUN);
        assert_eq!(first.code, 0, "{}", first.out());
        assert_eq!(
            first.line("hushrun: unit: every"),
            "hushrun: unit: every test file runs: no run here has listed RSpec's test files yet"
        );
        r.write(
            "spec/greet_spec.rb",
            "RSpec.describe \"greet\" do\n  it(\"says hi\") { expect(Shop::Greet.hello).to eq(\"hi\") }\nend\n",
        );
        let second = r.run(RSPEC_RUN);
        assert_eq!(second.code, 0, "{}", second.out());
        let line = second.line("hushrun: unit: ");
        assert!(
            line.starts_with(
                "hushrun: unit: 2 of 7 test files run; 5 already passed on the files they read"
            ),
            "{}",
            second.out()
        );
        // RSpec left them out before their hooks: 2 examples of the 8 ran.
        assert!(
            second.stdout.contains("2 examples, 0 failures"),
            "{}",
            second.out()
        );
        // A change every spec file reads, which breaks one: it runs, fails, and the run fails.
        r.write(
            "lib/shop/pricing.rb",
            "module Shop\n  module Pricing\n    def self.price(x)\n      x * 3\n    end\n  end\nend\n",
        );
        let broken = r.run(RSPEC_RUN);
        assert_ne!(broken.code, 0, "{}", broken.out());
        assert!(
            broken.stdout.contains("8 examples, 1 failure"),
            "{}",
            broken.out()
        );
        // Nothing else changed: the failure runs again, and fails again.
        let again = r.run(RSPEC_RUN);
        assert_ne!(again.code, 0, "{}", again.out());
        assert!(again.stdout.contains("1 failure"), "{}", again.out());
    }
}

// covers: C-39, C-84, C-85
#[test]
fn rspec_examples_are_reused_one_by_one() {
    for ruby in rubies(false) {
        let r = Repo::new(&ruby, &rspec_shop("{mode: reuse, unit: test, reads: test}"));
        let first = r.run(RSPEC_RUN);
        assert_eq!(first.code, 0, "{}", first.out());
        // One example more in a file: its file runs, and only the new example in it.
        r.write(
            "spec/cart_spec.rb",
            "RSpec.describe Shop::Cart do\n  it(\"totals\") { expect(Shop::Cart.new.total([1, 2])).to eq 6 }\n  context \"empty\" do\n    it(\"is zero\") { expect(Shop::Cart.new.total([])).to eq 0 }\n  end\n  it(\"totals one\") { expect(Shop::Cart.new.total([1])).to eq 2 }\nend\n",
        );
        let second = r.run(RSPEC_RUN);
        assert_eq!(second.code, 0, "{}", second.out());
        // The plan counts the tests it knows: the new one runs besides.
        let line = second.line("hushrun: unit: ");
        assert!(
            line.starts_with("hushrun: unit: 3 of 8 tests run, in 2 of 7 test files"),
            "{}",
            second.out()
        );
        assert!(
            second.stdout.contains("4 examples, 0 failures"),
            "{}",
            second.out()
        );
        let map = r.map();
        let tests = map["units"]["spec/cart_spec.rb"]["tests"].clone();
        assert!(
            tests.get("[1:1]").is_some() && tests.get("[1:2:1]").is_some(),
            "{map:#}"
        );
    }
}

/// The shop as Minitest's, run by Rake's test task.
fn minitest_shop(tests: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = rspec_shop(tests)
        .into_iter()
        .filter(|(p, _)| !p.starts_with("spec/") && p != ".rspec")
        .collect();
    files.push(file(
        "Rakefile",
        "require \"rake/testtask\"\nRake::TestTask.new(:test) do |t|\n  t.libs << \"test\" << \"lib\"\n  t.pattern = \"test/**/*_test.rb\"\n  t.warning = false\nend\n",
    ));
    files.push(file(
        "test/test_helper.rb",
        "require \"minitest/autorun\"\nrequire \"shop\"\n",
    ));
    files.push(file(
        "test/cart_test.rb",
        "require \"test_helper\"\nclass CartTest < Minitest::Test\n  def test_total\n    assert_equal 6, Shop::Cart.new.total([1, 2])\n  end\n\n  def test_empty\n    assert_equal 0, Shop::Cart.new.total([])\n  end\nend\n",
    ));
    files.push(file(
        "test/greet_test.rb",
        "require_relative \"test_helper\"\nclass GreetTest < Minitest::Test\n  def test_hello\n    assert_equal \"hi\", Shop::Greet.hello\n  end\nend\n",
    ));
    for n in 1..=5 {
        files.push(file(
            &format!("test/same{n}_test.rb"),
            &format!(
                "require \"test_helper\"\nclass Same{n}Test < Minitest::Test\n  def test_adds\n    assert_equal 2, 1 + 1\n  end\nend\n"
            ),
        ));
    }
    files
}

const RAKE_RUN: &[&str] = &["run", "unit", "--", "rake", "test"];

// covers: C-30, C-34, C-82, C-83, C-84
#[test]
fn minitest_test_files_are_observed_and_reused_under_rake() {
    for ruby in rubies(true) {
        let r = Repo::new(&ruby, &minitest_shop("reuse"));
        let first = r.run(RAKE_RUN);
        assert_eq!(first.code, 0, "{}: {}", ruby.name, first.out());
        // What it and its test_helper required, to the end; what another test file loaded
        // through an autoload only when that one ran first (Minitest's order is random); and no
        // other test file.
        let cart = r.reads("test/cart_test.rb");
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
        let second = r.run(RAKE_RUN);
        assert_eq!(second.code, 0, "{}: {}", ruby.name, second.out());
        assert!(
            second.line("hushrun: unit: ").starts_with(
                "hushrun: unit: 2 of 7 test files run; 5 already passed on the files they read"
            ),
            "{}: {}",
            ruby.name,
            second.out()
        );
        assert!(
            second.stdout.contains("2 runs,"),
            "{}: {}",
            ruby.name,
            second.out()
        );
        // A failure in a file every test file reads: all run, and the run fails.
        r.write(
            "lib/shop/pricing.rb",
            "module Shop\n  module Pricing\n    def self.price(x)\n      x * 3\n    end\n  end\nend\n",
        );
        let broken = r.run(RAKE_RUN);
        assert_ne!(broken.code, 0, "{}: {}", ruby.name, broken.out());
        let again = r.run(RAKE_RUN);
        assert_ne!(again.code, 0, "{}: {}", ruby.name, again.out());
    }
}

// covers: C-39, C-84
#[test]
fn minitest_tests_are_reused_one_by_one() {
    for ruby in rubies(true) {
        let r = Repo::new(&ruby, &minitest_shop("{mode: reuse, unit: test}"));
        let first = r.run(RAKE_RUN);
        assert_eq!(first.code, 0, "{}: {}", ruby.name, first.out());
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
        let second = r.run(RAKE_RUN);
        assert_eq!(second.code, 0, "{}: {}", ruby.name, second.out());
        assert!(
            second
                .line("hushrun: unit: ")
                .starts_with("hushrun: unit: 3 of 8 tests run, in 2 of 7 test files"),
            "{}: {}",
            ruby.name,
            second.out()
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
