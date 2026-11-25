//! \[ADR 0114\] The Ruby adapter, observed (C-30, C-32, C-39, C-82 to C-85): RSpec's and
//! Minitest's test files, Rails' `bin/rails test` among them. A unit is a test file, as the runner
//! itself names them in the run (RSpec's `files_to_run`, the files Minitest's test methods are
//! defined in); what it read is the code that ran for it, what that code required and opened, and
//! what code outside the repository loaded or opened while it or a test file before it ran; each
//! test is told on its own, with what it ran and opened when the check asks for `reads: test`.
//!
//! - Loading: a file embedded in this binary, written to `<cache dir>/adapters/ruby-<hash>/`,
//!   which every Ruby process of COMMAND loads through `RUBYOPT=-rhushrun_ruby` and `RUBYLIB`,
//!   added to what the environment had: no file of the repository changes.
//! - The unit rule: the test files the runner listed in the run that wrote the check's map, as
//!   Vitest's and Jest's (ADR 0071, 0085).
//! - Selection: RSpec through an exclusion filter added before it announces its filters, so a
//!   left-out example runs none of its hooks; Minitest by leaving the test out of
//!   `runnable_methods`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_core::units::{self, Facts, NotFollowed, Set, Sets, Sources, Tree};

use super::adapter::{
    Adapter, Hints, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::command::program_name;
use super::facts::FactsFile;

/// The adapter's word: its facts files, this machine's store and a hint name it so.
pub const WORD: &str = "ruby";
/// What the adapter observes being read: its sources (C-30). Code, and what Ruby code opens
/// through `File` and `IO` (templates, fixtures, configuration). `Gemfile`, `Gemfile.lock`, a
/// gemspec and a `.rake` file are read before the adapter loads, or by a process it doesn't
/// watch (Bundler, Rake's own), so they stay in the rest set, whose change runs every test file.
pub const SOURCE_KINDS: &[&str] = &[
    ".rb",
    ".erb",
    ".haml",
    ".slim",
    ".jbuilder",
    ".builder",
    ".yml",
    ".yaml",
    ".json",
];
/// The file every Ruby process of COMMAND loads, by its feature name.
pub const FEATURE: &str = "hushrun_ruby";
/// The plugin.
const PLUGIN: &str = include_str!("hushrun_ruby.rb");

/// Which runner COMMAND starts, as far as its words say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    RSpec,
    Minitest,
    /// Rake or a Ruby script: either.
    Either,
}

/// COMMAND, when it starts RSpec or Minitest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    pub runner: Runner,
}

/// RSpec's options that make a run that isn't one run of its examples, or that choose them from
/// an earlier run.
const RSPEC_REFUSED: &[(&str, &str)] = &[
    (
        "--dry-run",
        "rspec --dry-run reports every example passed without running it",
    ),
    ("--bisect", "rspec --bisect runs the suite many times over"),
    (
        "--only-failures",
        "rspec --only-failures chooses examples from an earlier run",
    ),
    (
        "--next-failure",
        "rspec --next-failure chooses examples from an earlier run",
    ),
    (
        "-n",
        "rspec --next-failure chooses examples from an earlier run",
    ),
    ("--init", "rspec --init isn't a run of the examples"),
    ("--version", "rspec --version isn't a run of the examples"),
    ("-v", "rspec --version isn't a run of the examples"),
    ("--help", "rspec --help isn't a run of the examples"),
    ("-h", "rspec --help isn't a run of the examples"),
];

/// Ruby's options that take the next word as their value.
const RUBY_VALUES: &[&str] = &["-I", "-r", "-C", "-E", "-x", "-e", "--encoding", "-F", "-K"];

/// What `command` is for the Ruby adapter: `None` when it starts neither RSpec nor Minitest by
/// name; `Some(Err(why))` when it does, in a way hushrun doesn't observe, which the run says.
pub fn recognize(command: &[String]) -> Option<Result<Recognized, String>> {
    let mut rest = command;
    // `bundle exec …`, `bin/bundle exec …`.
    if program_name(rest.first()?) == "bundle" {
        if rest.get(1).map(String::as_str) != Some("exec") {
            return None;
        }
        rest = &rest[2..];
        while rest.first().is_some_and(|w| w.starts_with("--")) {
            rest = &rest[1..];
        }
    }
    let first = rest.first()?;
    let name = program_name(first);
    match &*name {
        "spring" => Some(Err(
            "Spring runs tests in a process it started earlier, which hushrun's adapter didn't load into".into(),
        )),
        "rspec" => rspec(&rest[1..]),
        "rails" => {
            let sub = rest.get(1).map(String::as_str)?;
            if sub == "test" || sub.starts_with("test:") {
                Some(Ok(Recognized {
                    runner: Runner::Minitest,
                }))
            } else if sub == "spec" || sub.starts_with("spec:") {
                Some(Ok(Recognized {
                    runner: Runner::RSpec,
                }))
            } else {
                None
            }
        }
        "rake" => {
            let tasks: Vec<&String> = rest[1..].iter().filter(|w| !w.starts_with('-')).collect();
            let runner = if tasks.iter().any(|t| t.starts_with("spec")) {
                Runner::RSpec
            } else if tasks.iter().any(|t| t.starts_with("test")) {
                Runner::Minitest
            } else {
                Runner::Either
            };
            Some(Ok(Recognized { runner }))
        }
        "ruby" => {
            let mut next = &rest[1..];
            while let Some(word) = next.first() {
                if word == "--disable=rubyopt" || word == "--disable-rubyopt" {
                    return Some(Err("a Ruby started with --disable=rubyopt ignores RUBYOPT".into()));
                }
                if !word.starts_with('-') || word == "-S" {
                    break;
                }
                let takes = RUBY_VALUES.contains(&word.as_str());
                next = &next[if takes { 2.min(next.len()) } else { 1 }..];
            }
            match next.first().map(String::as_str) {
                Some("-S") => recognize(&next[1..]),
                Some(_) | None => Some(Ok(Recognized {
                    runner: Runner::Either,
                })),
            }
        }
        _ => None,
    }
}

fn rspec(args: &[String]) -> Option<Result<Recognized, String>> {
    for word in args {
        let flag = word.split_once('=').map_or(word.as_str(), |(f, _)| f);
        if let Some((_, why)) = RSPEC_REFUSED.iter().find(|(f, _)| *f == flag) {
            return Some(Err((*why).to_string()));
        }
    }
    Some(Ok(Recognized {
        runner: Runner::RSpec,
    }))
}

/// \[ADR 0114\] The Ruby adapter for a COMMAND that starts RSpec or Minitest; `None` for any
/// other COMMAND.
pub fn recognized(command: &[String], _cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    Some(recognize(command)?.map(|r| Box::new(r) as Box<dyn Adapter>))
}

/// The directory that holds the plugin for this binary, written whole when it isn't there yet:
/// `<cache dir>/adapters/ruby-<first 16 hex of its SHA-256>`.
pub fn install(cache: &Path) -> std::io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(PLUGIN.as_bytes()).hex();
    let dir = cache
        .join("adapters")
        .join(format!("ruby-{}", &digest[..16]));
    let path = dir.join(format!("{FEATURE}.rb"));
    if std::fs::read(&path).ok().as_deref() != Some(PLUGIN.as_bytes()) {
        super::facts::write_whole(&path, PLUGIN.as_bytes())?;
    }
    Ok(dir)
}

/// The variables that load the plugin from `dir`, each keeping what COMMAND's environment had:
/// `RUBYLIB` with `dir` first (a list, so any path fits, which `RUBYOPT`'s words can't hold),
/// `RUBYOPT` with `-rhushrun_ruby` last; and Bootsnap's compile cache off, which would load the
/// repository's files without Ruby compiling them, so that what each test ran can be watched.
pub fn variables(
    dir: &Path,
    rubylib: Option<OsString>,
    rubyopt: Option<OsString>,
) -> Vec<(String, OsString)> {
    let mut lib = dir.as_os_str().to_os_string();
    if let Some(existing) = rubylib.filter(|p| !p.is_empty()) {
        lib.push(if cfg!(windows) { ";" } else { ":" });
        lib.push(existing);
    }
    let mut opt = rubyopt.filter(|o| !o.is_empty()).unwrap_or_default();
    if !opt.is_empty() {
        opt.push(" ");
    }
    opt.push(format!("-r{FEATURE}"));
    vec![
        ("RUBYLIB".to_string(), lib),
        ("RUBYOPT".to_string(), opt),
        ("DISABLE_BOOTSNAP_COMPILE_CACHE".to_string(), "1".into()),
        ("DISABLE_SPRING".to_string(), "1".into()),
    ]
}

/// What the Ruby adapter observed of a run, before hushrun decides any state.
#[derive(Debug, Default)]
pub struct Observed {
    pub units: BTreeMap<String, super::Seen>,
    pub failures_by_unit: bool,
    /// The unit rule: the test files the runner listed, from the root.
    pub unit_files: BTreeSet<String>,
    /// The directories each unit listed, from the root.
    pub dirs: BTreeMap<String, BTreeSet<String>>,
    pub edges: BTreeSet<super::db::Edge>,
    pub not_kept: Option<String>,
}

/// A list of strings a file's settings hold under `key`.
fn strings(file: &FactsFile, key: &str) -> Vec<String> {
    file.settings
        .get(key)
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Reads what the Ruby adapter wrote in a run's directory, for the repository whose files are
/// `tree`. `Ok(None)` when no process of COMMAND ran RSpec or Minitest.
pub fn observe(files: &[FactsFile], tree: &Tree) -> Result<Option<Observed>, String> {
    let ours: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == WORD).collect();
    if ours.is_empty() {
        return Ok(None);
    }
    if let Some(error) = ours.iter().find_map(|f| f.errors.first()) {
        return Err(format!(
            "hushrun's Ruby adapter failed: {}",
            error.chars().take(300).collect::<String>()
        ));
    }
    let mut unit_files: BTreeSet<String> = BTreeSet::new();
    let mut listed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in &ours {
        unit_files.extend(strings(file, "testFiles"));
        for unit in &file.units {
            listed
                .entry(unit.unit.clone())
                .or_default()
                .extend(unit.dirs.iter().cloned());
        }
    }
    let merged = super::facts::merged(files, WORD);
    let mut out = Observed {
        failures_by_unit: merged.values().all(|m| m.failures_by_unit),
        edges: super::facts::edges_of(files, WORD),
        not_kept: super::facts::run_not_kept(files),
        ..Observed::default()
    };
    for (unit, m) in merged {
        // \[C-30\] A unit is a test file of the runner's own list, as the plan counts them.
        if !unit_files.contains(&unit) {
            continue;
        }
        let dirs = listed.remove(&unit).unwrap_or_default();
        let mut reads = m.reads;
        reads.insert(unit.clone());
        reads.extend(super::gotest::under(tree, &dirs));
        reads.retain(|p| tree.contains(p));
        let not_followed: BTreeSet<NotFollowed> = m.not_followed;
        out.dirs.insert(unit.clone(), dirs);
        out.units.insert(
            unit,
            super::Seen {
                outside: Default::default(),
                declared: None,
                getenv: Default::default(),
                facts: Facts {
                    complete: m.complete,
                    failed: m.failed,
                    not_followed,
                    deselected: m.deselected,
                    skipped: m.skipped > 0,
                    ..Facts::default()
                },
                reads,
                duration_ms: m.duration_ms,
                tests: m.tests,
            },
        );
    }
    out.unit_files = unit_files;
    Ok(Some(out))
}

impl Recognized {
    /// What the run says when the runner it names wrote nothing for hushrun.
    fn nothing_written(&self) -> Option<String> {
        let runner = match self.runner {
            Runner::RSpec => "RSpec",
            Runner::Minitest => "Minitest",
            // Rake or a script may run something else: nothing to say.
            Runner::Either => return None,
        };
        Some(format!(
            "{runner} wrote nothing for hushrun: its Ruby didn't load hushrun's adapter, as when a script sets RUBYOPT instead of adding to it (`RUBYOPT=-W0 rspec`); keep what it had (`RUBYOPT=\"$RUBYOPT -W0\"`), or it is a Ruby older than 3.1 or not CRuby, which the adapter doesn't observe"
        ))
    }
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        WORD
    }

    fn runner(&self) -> &'static str {
        match self.runner {
            Runner::RSpec => "RSpec",
            Runner::Minitest => "Minitest",
            Runner::Either => "RSpec or Minitest",
        }
    }

    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            // \[C-85\] Where a test can't be watched, the adapter's facts say why.
            reads: true,
        }
    }

    fn hints(&self) -> Hints {
        Hints::Kinds(SOURCE_KINDS)
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let dir =
            install(at.cache).map_err(|err| format!("can't write its Ruby adapter: {err}"))?;
        Ok(variables(&dir, (at.env)("RUBYLIB"), (at.env)("RUBYOPT")))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let shared = super::fetch(at.asker, self.hints(), &Sets::default(), at.tree, at.ignore);
        let mut listed: BTreeSet<String> = at
            .map
            .unit_files
            .iter()
            .filter(|f| at.tree.contains(f))
            .cloned()
            .collect();
        if listed.is_empty() {
            listed = shared.unit_files();
        }
        if listed.is_empty() {
            return Err(format!(
                "no run here has listed {}'s test files yet",
                self.runner()
            ));
        }
        let sets = units::classify(
            at.tree.paths(),
            &|path| listed.contains(path),
            Sources::Kinds(SOURCE_KINDS),
            at.ignore,
        );
        let known = sets
            .files(Set::Units)
            .map(|unit| {
                let lists = super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    None,
                    None,
                );
                (unit.to_string(), lists)
            })
            .collect();
        Ok(Some(Planning {
            sets,
            known,
            received: shared.received,
            extras: RunExtras {
                python_files: None,
                test_files: Some(listed.into_iter().collect()),
            },
            ..Planning::default()
        }))
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        let Some(observed) = observe(at.files, at.tree)? else {
            return match self.nothing_written() {
                Some(why) => Err(why),
                None => Ok(None),
            };
        };
        let sets = units::classify(
            at.tree.paths(),
            &|path| observed.unit_files.contains(path),
            Sources::Kinds(SOURCE_KINDS),
            at.ignore,
        );
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = observed
            .units
            .keys()
            .filter_map(|unit| {
                let lists = super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    None,
                    None,
                )?;
                Some((unit.clone(), lists))
            })
            .collect();
        Ok(Some(Read {
            seen: observed.units,
            failures_by_unit: observed.failures_by_unit,
            sets,
            planned,
            dirs: observed.dirs,
            unit_files: Some(observed.unit_files),
            edges: observed.edges,
            received: shared.received,
            not_kept: observed.not_kept,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, unit_files: &BTreeSet<String>) -> RunExtras {
        RunExtras {
            python_files: None,
            test_files: Some(unit_files.iter().cloned().collect()),
        }
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        units::classify(
            at.tree.paths(),
            &|p| at.listed.contains(p),
            Sources::Kinds(SOURCE_KINDS),
            at.ignore,
        )
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!(
            "{unit} isn't a test file of {check} as far as this machine knows: {} didn't list it in its last run here",
            self.runner()
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("the runner's list of test files is the one of its last run here")
    }
}

#[cfg(test)]
mod tests;
