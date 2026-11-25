//! \[ADR 0114\] The Rubies, gems and shops the tests of the Ruby adapter run, in-process
//! (`units_ruby.rs`) and end to end (`e2e_ruby.rs`). See `units_ruby.rs` for the tools.

#![allow(dead_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use super::repo::Repo;

/// A Ruby these tests run on, with one gem directory.
#[derive(Clone)]
pub struct Ruby {
    pub ruby: PathBuf,
    pub gems: PathBuf,
    /// What the gem directory holds, for messages.
    pub name: &'static str,
}

impl Ruby {
    /// `PATH` with the gems' programs and Ruby's own first.
    pub fn path(&self) -> OsString {
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
pub fn rubies(with_minitest5: bool) -> Vec<Ruby> {
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

/// A repository of `files` whose COMMAND runs on `ruby` and its gems: none of a person's Ruby
/// settings reach it.
pub fn repo(ruby: &Ruby, files: &[(String, String)]) -> Repo {
    Repo::new(files)
        .clear(&[
            "HUSHRUN_RUBY_START",
            "RUBYOPT",
            "RUBYLIB",
            "SPEC_OPTS",
            "TESTOPTS",
            "BUNDLE_GEMFILE",
            "GEM_HOME",
            "GEM_PATH",
        ])
        .give("GEM_HOME", &ruby.gems)
        .give("GEM_PATH", &ruby.gems)
        .give("PATH", ruby.path())
}

/// What `unit` read, as this machine's store keeps it.
pub fn reads(r: &Repo, unit: &str) -> Vec<String> {
    let map = r.map();
    map["units"][unit]["reads"]
        .as_array()
        .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A shop: a library its spec_helper requires whole, pricing a cart uses, a greeting that reads a
/// data file through an autoload, and five spec files that read only what every one does, so
/// that a plan has something to reuse.
pub fn rspec_shop(tests: &str) -> Vec<(String, String)> {
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
            "$LOAD_PATH.unshift File.expand_path(\"../lib\", __dir__)\nrequire \"shop\"\nDir[File.join(__dir__, \"support/*.rb\")].sort.each { |f| require f }\n",
        ),
        file("spec/support/money.rb", "MONEY = 1\n"),
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

/// The shop as Minitest's, run by Rake's test task.
pub fn minitest_shop(tests: &str) -> Vec<(String, String)> {
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
