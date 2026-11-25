//! A check run in-process through pytest's adapter alone, `pytest::Pytest`: what `hushrun run
//! CHECK -- COMMAND` does with a local backend, without the binary (`common/in_process.rs`,
//! CONTRIBUTING.md, "Which tests CI runs again"). A test file of pytest's includes this with
//! `common/in_process.rs` as `in_process`, `common/repo.rs` as `repo`, `common/store.rs` as
//! `store` and `common/python.rs` as `python`, and never `common/cli.rs`.
//!
//! Every adapter call is on `pytest::Pytest` itself, which the static map resolves to its own
//! `impl Adapter`. It is pytest's copy of `units_gradle.rs`'s `observe`, kept here once for the
//! test files that drive pytest (`units.rs`, `test_as_unit.rs`, `reads_test.rs`, `reuse.rs`).

#![allow(dead_code)]

use std::collections::BTreeSet;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, pytest};

use super::in_process::{self, Ran, Repo};

/// What pytest's adapter answers that never changes in a run, for the code every adapter shares.
pub fn profile(adapter: &pytest::Pytest) -> Profile {
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

/// A repository of `files` for pytest: its COMMAND never inherits pytest's options, a
/// `PYTHONPATH` or Cargo's target directory.
pub fn repo(files: &[(String, String)]) -> Repo {
    Repo::new(files).clear(super::python::PYTEST_CLEARED)
}

/// [`repo`], of borrowed files.
pub fn repo_of(files: &[(&str, &str)]) -> Repo {
    let owned: Vec<(String, String)> = files
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
    repo(&owned)
}

/// `hushrun run unit -- ARGS`, with `env` besides, in-process, through pytest's adapter alone.
pub fn run(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Ran {
    run_check(r, "unit", args, env)
}

/// `hushrun run CHECK -- ARGS`, with `env` besides, in-process, through pytest's adapter alone.
pub fn run_check(r: &Repo, check: &str, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to pytest's adapter (`pytest::any_other`), with no
    // trait object: whether it can load safely for COMMAND.
    if let Some(why) = pytest::unsafe_for(hushrun::units::command::without_env(&command)) {
        return in_process::Run::new(r, check, args, env).unobserved(why);
    }
    observe(r, check, args, env, &pytest::Pytest)
}

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order, on `pytest::Pytest` itself (`units_gradle.rs`'s `observe`, with the type changed).
pub fn observe(
    r: &Repo,
    check: &str,
    args: &[&str],
    env: &[(&str, &str)],
    adapter: &pytest::Pytest,
) -> Ran {
    let mut run = in_process::Run::new(r, check, args, env);
    let Some(mut observer) = run.open(profile(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    // \[C-32\] An adapter that can't load observes nothing: COMMAND runs as it is.
    if let Err(why) = &variables {
        return run.unobserved(why);
    }
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // pytest's adapter keeps the trait's `plan`, which is its `plan_from_files`.
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

/// `python -m pytest -q -p no:cacheprovider EXTRA`: COMMAND as the pytest tests run it.
pub fn pytest_command<'a>(python: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![python, "-m", "pytest", "-q", "-p", "no:cacheprovider"];
    args.extend(extra);
    args
}
