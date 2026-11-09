//! \[ADR 0050\] Lines of CI's files init could also wrap, for a person to decide (C-15): a job
//! named for its tests that runs a test script of the repository, such as `./scripts/test.sh`,
//! `bash scripts/ci/test.sh` or `./runtest`. init doesn't wrap them itself: a script can deploy or
//! write reports, which a skip wouldn't do, and only its people know what it does. So the plan
//! lists each with the line to use, and changes nothing for it.

use std::collections::{BTreeMap, BTreeSet};

use hushrun_core::policy::Policy;

use crate::cmd::wrap::{derived_name, made_name};

use super::checks;
use super::projects::{self, Layout};
use super::scripts;
use super::variants;
use super::workflows::{Host, Job, Runs, Workflow};
use super::wrapping::{Proposal, ScriptChecks};

/// Why init leaves a suggestion to a person, as the plan says it.
pub const WHY: &str =
    "it runs a script, which can deploy or write reports: wrap it if it only runs checks";

/// \[ADR 0055\] Why init leaves a task of the repository's own to a person.
pub const XTASK_WHY: &str = "it runs a task of the repository's own (cargo xtask), which can deploy or write reports: wrap it if it only runs checks";

/// A line init could also wrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Its file, an index into the files [`Proposal::read_with`] read.
    pub file: usize,
    pub job: String,
    /// The line it starts on, 0-based.
    pub line: usize,
    /// The directory it runs in, relative to the root, `/`-separated: `""` at the root.
    pub dir: String,
    /// The command, as init read it.
    pub before: String,
    /// The check it would be.
    pub check: String,
    /// The line to use in its place.
    pub after: String,
    /// Its job installs hushrun itself.
    pub installed: bool,
    /// Why init leaves it to a person: [`WHY`], or \[ADR 0055\] [`XTASK_WHY`].
    pub why: &'static str,
}

/// What a job's id or name holds when it runs tests or checks.
const JOB_WORDS: &[&str] = &["test", "spec", "check", "unit", "integration"];

/// What a test script's file name says, as one of its words.
const SCRIPT_WORDS: &[&str] = &[
    "test",
    "tests",
    "spec",
    "specs",
    "check",
    "checks",
    "unit",
    "unittest",
    "unittests",
    "integration",
    "runtest",
    "runtests",
    "testall",
];

/// Shells, which run the script that follows them, their options aside.
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh"];

/// Other programs that run the script that follows them, their options aside, and their options
/// that run code of their own or a module instead of a file.
const INTERPRETERS: &[(&str, &[&str])] = &[
    ("python", &["-c", "-m"]),
    ("python3", &["-c", "-m"]),
    ("node", &["-e", "--eval", "-p", "--print"]),
    ("ruby", &["-e"]),
    ("perl", &["-e", "-E"]),
    ("php", &["-r"]),
    (
        "pwsh",
        &["-c", "-Command", "-command", "-e", "-ec", "-EncodedCommand"],
    ),
    (
        "powershell",
        &["-c", "-Command", "-command", "-e", "-ec", "-EncodedCommand"],
    ),
];

/// Whether `option` makes `program`, a shell or an interpreter, run code of its own or a module
/// rather than a file: a shell's `-c` or `-s`, alone or among other letters, as in `-ec`.
fn runs_code(program: &str, option: &str) -> bool {
    if SHELLS.contains(&program) {
        return option
            .strip_prefix('-')
            .is_some_and(|letters| !letters.starts_with('-') && letters.contains(['c', 's']));
    }
    INTERPRETERS
        .iter()
        .any(|(name, code)| *name == program && code.contains(&option))
}

/// The test scripts CI's jobs run that a person could wrap, in the order the files have them,
/// each named beside what `proposal` wraps. `called` are the files a running workflow calls,
/// which run though no push starts them; `existing`, `javascript` and `scripts` are what
/// [`Proposal::read_with`] took, so that what it wraps or leaves is no suggestion.
pub fn read(
    files: &[Workflow],
    called: &BTreeSet<String>,
    proposal: &Proposal,
    existing: Option<&Policy>,
    javascript: bool,
    scripts: &ScriptChecks,
) -> Vec<Suggestion> {
    let mut found = Vec::new();
    for (index, file) in files.iter().enumerate() {
        if !file.reads_as_yaml || (file.runs_for(None) == Runs::No && !called.contains(&file.path))
        {
            continue;
        }
        let yaml = (file.host == Host::GitHub)
            .then(|| variants::parse(&file.path, file.source().as_bytes()))
            .flatten();
        for job in &file.jobs {
            let name = yaml
                .as_ref()
                .and_then(|yaml| yaml["jobs"][job.id.as_str()]["name"].as_str());
            let named = named_for_checks(&job.id) || name.is_some_and(named_for_checks);
            found.extend(in_job(index, job, javascript, scripts, named));
        }
    }
    settle(found, proposal, existing)
}

/// The test scripts `job` of file `index` runs, when it is `named` for its checks, each named
/// after its script for now; \[ADR 0055\] and, in any job, the tasks of the repository's own it
/// runs, `cargo xtask <task>`, each named after its task.
fn in_job(
    index: usize,
    job: &Job,
    javascript: bool,
    scripts: &ScriptChecks,
    named: bool,
) -> Vec<Suggestion> {
    let mut found = Vec::new();
    for step in &job.steps {
        let Ok(dir) = &step.dir else {
            continue;
        };
        let mut here = Some(String::new());
        for source in &step.sources {
            // \[ADR 0055\] A line a `# hushrun: leave` comment marks is a person's decision made.
            if source.leave.is_some() {
                // A `cd` on it still moves what follows.
                let _ = scripts::commands(&source.shell(), &mut here);
                continue;
            }
            for command in scripts::commands(&source.shell(), &mut here) {
                // What the shell or CI changes, or a `cd` moves where init can't follow, can't be
                // wrapped as it is. \[ADR 0050\] Nor is a line whose words CI or the shell fills in,
                // or that sets variables first, suggested: whether it may hold a secret is init's
                // to tell, as it does for what it wraps.
                let (None, Some(moved)) = (&command.leave, &command.moved) else {
                    continue;
                };
                if command.fills_in || command.prefixed {
                    continue;
                }
                let words: Vec<&str> = command.words.iter().map(String::as_str).collect();
                let before = words.join(" ");
                let dir = projects::joined(dir, moved);
                // What init wraps or leaves is its own: a wrapped script's command at the root,
                // and what the table knows.
                if (dir.is_empty() && scripts.contains_key(&before))
                    || checks::recognize(&words, javascript).is_some()
                {
                    continue;
                }
                let script = test_script(&words)
                    .filter(|_| named)
                    .and_then(|script| derived_name(None, script))
                    .map(|name| (name, WHY));
                let task = || {
                    checks::xtask_task(&words)
                        .and_then(made_name)
                        .map(|name| (name, XTASK_WHY))
                };
                let Some((check, why)) = script.or_else(task) else {
                    continue;
                };
                let check = check.as_str().to_string();
                found.push(Suggestion {
                    file: index,
                    job: job.id.clone(),
                    line: source.place(command.at).0,
                    dir,
                    before,
                    check,
                    after: String::new(),
                    installed: job.installs_hushrun,
                    why,
                });
            }
        }
    }
    found
}

/// Whether a job's id or name says it runs tests or checks, in any case.
fn named_for_checks(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    JOB_WORDS.iter().any(|word| text.contains(word))
}

/// The script of the repository that `words` run, when its file name says it tests or checks:
/// `./scripts/test.sh` and `script/test`, a path with a directory, or what an interpreter runs,
/// such as `bash scripts/ci/test.sh` or `python runtests.py`.
fn test_script<'a>(words: &[&'a str]) -> Option<&'a str> {
    let (&program, rest) = words.split_first()?;
    let interprets =
        SHELLS.contains(&program) || INTERPRETERS.iter().any(|(name, _)| *name == program);
    let script = if interprets {
        let mut rest = rest.iter();
        loop {
            let word = *rest.next()?;
            if runs_code(program, word) {
                return None;
            }
            if !word.starts_with('-') {
                break word;
            }
        }
    } else if program.contains('/') {
        program
    } else {
        // A program on the PATH is a tool, not a script of the repository.
        return None;
    };
    if script.starts_with(['/', '~']) || script.contains(['$', '`', '\\']) {
        return None;
    }
    let file = script.rsplit('/').next().unwrap_or(script);
    let stem = match file.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => file,
    };
    words_of(stem)
        .iter()
        .any(|word| SCRIPT_WORDS.contains(&word.as_str()))
        .then_some(script)
}

/// A file name's words, in lowercase: split at `-`, `_`, `.` and spaces, and where a capital
/// follows a small letter, so that `run_tests`, `run-tests` and `runTests` hold `tests`.
fn words_of(name: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.' | ' ') {
            words.push(std::mem::take(&mut word));
            previous_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && previous_lower {
            words.push(std::mem::take(&mut word));
        }
        previous_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        word.push(c.to_ascii_lowercase());
    }
    words.push(word);
    words.retain(|word| !word.is_empty());
    words
}

/// Names each of `found` as init names what it wraps (C-15): one name, one command, so that a
/// line of `hushrun ready` or the dashboard is one check. A script's name stays its own, unless
/// another command in its directory has that name, or another job runs it too, since two jobs may
/// run on two systems, which a key doesn't tell apart: then its job's name goes in front, then a
/// number. Its scope is the one a check init wraps there gets, or, where hushrun.yaml declares
/// scopes, the one of them that holds its directory; one no scope fits is left out, since init
/// couldn't place it either.
fn settle(
    found: Vec<Suggestion>,
    proposal: &Proposal,
    existing: Option<&Policy>,
) -> Vec<Suggestion> {
    let mut jobs: BTreeMap<(String, String), BTreeSet<(usize, String)>> = BTreeMap::new();
    for suggestion in &found {
        jobs.entry((suggestion.dir.clone(), suggestion.before.clone()))
            .or_default()
            .insert((suggestion.file, suggestion.job.clone()));
    }
    let mut taken: BTreeMap<(String, String), String> = proposal
        .wrapped()
        .into_iter()
        .map(|wrap| ((wrap.dir, wrap.check), wrap.before))
        .collect();
    let mut settled = Vec::new();
    for mut suggestion in found {
        // Where hushrun.yaml declares them, init's layout holds only what it wraps.
        let scope = match (&proposal.layout, existing) {
            (Layout::Declared(_), Some(policy)) => projects::the_scope_of(policy, &suggestion.dir)
                .map(Some)
                .ok_or(()),
            (layout, _) => layout.scope_of(&suggestion.dir).map_err(|_| ()),
        };
        let Ok(scope) = scope else {
            continue;
        };
        let with_job = |name: &str| {
            made_name(&format!("{}-{name}", suggestion.job))
                .map_or_else(|| name.to_string(), |made| made.to_string())
        };
        let several = jobs[&(suggestion.dir.clone(), suggestion.before.clone())].len() > 1;
        let mut name = if several {
            with_job(&suggestion.check)
        } else {
            suggestion.check.clone()
        };
        let free = |name: &str, taken: &BTreeMap<(String, String), String>| {
            taken
                .get(&(suggestion.dir.clone(), name.to_string()))
                .is_none_or(|command| *command == suggestion.before)
        };
        if !free(&name, &taken) {
            let base = with_job(&name);
            let mut n = 1;
            name.clone_from(&base);
            while !free(&name, &taken) {
                n += 1;
                name = format!("{base}-{n}");
            }
        }
        taken.insert(
            (suggestion.dir.clone(), name.clone()),
            suggestion.before.clone(),
        );
        let scope = scope.map_or_else(String::new, |scope| format!("--scope {scope} "));
        suggestion.after = format!("hushrun run {scope}{name} -- {}", suggestion.before);
        suggestion.check = name;
        settled.push(suggestion);
    }
    settled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::setup::policy::RuleLevel;

    fn words(line: &str) -> Vec<&str> {
        line.split_whitespace().collect()
    }

    /// What init makes of `files`, in a repository with no package.json.
    fn read_all(files: &[Workflow]) -> Proposal {
        let dir = tempfile::tempdir().unwrap();
        Proposal::read(dir.path(), files, None, true, &ScriptChecks::new())
    }

    /// The suggestions of one workflow, as `(job, check, after)`.
    fn suggested(text: &str) -> Vec<(String, String, String)> {
        let files = [Workflow::parse(
            ".github/workflows/ci.yml".into(),
            text.to_string(),
        )];
        read_all(&files)
            .suggestions
            .into_iter()
            .map(|s| (s.job, s.check, s.after))
            .collect()
    }

    /// A workflow of these jobs, each `(id, steps)`.
    fn workflow(jobs: &[(&str, &str)]) -> String {
        let mut text = String::from("on: [push, pull_request]\njobs:\n");
        for (id, steps) in jobs {
            text.push_str(&format!(
                "  {id}:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n{steps}"
            ));
        }
        text
    }

    fn one(job: &str, check: &str, after: &str) -> (String, String, String) {
        (job.to_string(), check.to_string(), after.to_string())
    }

    // covers: C-15
    #[test]
    fn a_test_jobs_script_is_suggested_with_the_line_to_use() {
        let text = workflow(&[("test", "      - run: ./scripts/test.sh\n")]);
        assert_eq!(
            suggested(&text),
            [one("test", "test", "hushrun run test -- ./scripts/test.sh")]
        );
        let files = [Workflow::parse(".github/workflows/ci.yml".into(), text)];
        let proposal = read_all(&files);
        let found = &proposal.suggestions[0];
        assert_eq!(
            (found.file, found.line, found.dir.as_str(), found.installed),
            (0, 6, "", false)
        );
        // init applies none of it.
        assert!(proposal.is_empty() && proposal.left.is_empty());
        // A job's name says it as well as its id, and a directory is where the line runs.
        let named = "on: push\njobs:\n  server:\n    name: Server tests\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: ./runtest --verbose\n        working-directory: server\n";
        assert_eq!(
            suggested(named),
            [one(
                "server",
                "runtest",
                "hushrun run runtest -- ./runtest --verbose"
            )]
        );
        let files = [Workflow::parse("ci.yml".into(), named.to_string())];
        assert_eq!(read_all(&files).suggestions[0].dir, "server");
        // A job that checks nothing out runs no script of this repository.
        let elsewhere = named.replace("      - uses: actions/checkout@v5\n", "");
        assert!(suggested(&elsewhere).is_empty());
        // GitLab's jobs too.
        let gitlab = Workflow::parse_gitlab("unit:\n  script:\n    - bash ci/unit.sh\n".into());
        let found = read_all(&[gitlab]).suggestions;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].after, "hushrun run unit -- bash ci/unit.sh");
    }

    // covers: C-15
    #[test]
    fn what_cant_be_wrapped_as_it_is_or_isnt_a_test_job_is_no_suggestion() {
        for steps in [
            // The shell or CI changes it, or a `cd` moves it where init can't follow.
            "      - run: ./scripts/test.sh | tee test.log\n",
            "      - run: SUITE=all ./scripts/test.sh\n",
            "      - run: ./scripts/test.sh ${{ matrix.suite }}\n",
            "      - run: |\n          cd \"$DIR\"\n          ./scripts/test.sh\n",
            // Not a test script, and what the table knows, which init wraps or leaves itself.
            "      - run: ./scripts/setup-db.sh\n",
            "      - run: cargo test\n",
            "      - run: cargo deny check\n",
            "      - run: hushrun run test -- ./scripts/test.sh\n",
        ] {
            assert!(
                suggested(&workflow(&[("test", steps)])).is_empty(),
                "{steps}"
            );
        }
        // A job whose id and name don't say it tests.
        let build = workflow(&[("build", "      - run: ./scripts/test.sh\n")]);
        assert!(suggested(&build).is_empty());
        // A workflow no push or pull request starts.
        let nightly = build
            .replace(
                "on: [push, pull_request]",
                "on:\n  schedule:\n    - cron: '0 0 * * *'",
            )
            .replace("build:", "test:");
        assert!(suggested(&nightly).is_empty());
        // A command a wrapped script of package.json runs is that script's check.
        let files = [Workflow::parse(
            "ci.yml".into(),
            workflow(&[("test", "      - run: ./scripts/test.sh\n")]),
        )];
        let dir = tempfile::tempdir().unwrap();
        let scripts = ScriptChecks::from([("./scripts/test.sh".to_string(), "test".to_string())]);
        let proposal = Proposal::read(dir.path(), &files, None, false, &scripts);
        assert!(proposal.suggestions.is_empty());
        assert_eq!(proposal.names(), ["test"]);
    }

    // covers: C-15
    #[test]
    fn a_suggestion_is_named_as_init_names_what_it_wraps() {
        // A name a wrapped command has takes its job's in front.
        let text = workflow(&[
            (
                "test",
                "      - run: cargo test --locked\n      - run: ./scripts/test.sh\n",
            ),
            ("unit", "      - run: bash scripts/ci/test.sh\n"),
        ]);
        assert_eq!(
            suggested(&text),
            [
                one(
                    "test",
                    "test-test",
                    "hushrun run test-test -- ./scripts/test.sh"
                ),
                one(
                    "unit",
                    "unit-test",
                    "hushrun run unit-test -- bash scripts/ci/test.sh"
                ),
            ]
        );
        // Two jobs of one script: each takes its job's name, since they may run on two systems.
        let twice = workflow(&[
            ("test-linux", "      - run: ./scripts/test.sh\n"),
            ("test-mac", "      - run: ./scripts/test.sh\n"),
        ]);
        assert_eq!(
            suggested(&twice),
            [
                one(
                    "test-linux",
                    "test-linux-test",
                    "hushrun run test-linux-test -- ./scripts/test.sh"
                ),
                one(
                    "test-mac",
                    "test-mac-test",
                    "hushrun run test-mac-test -- ./scripts/test.sh"
                ),
            ]
        );
        // Twice in one job is one check.
        let again = workflow(&[(
            "test",
            "      - run: ./scripts/test.sh\n      - run: ./scripts/test.sh\n",
        )]);
        let names: Vec<String> = suggested(&again).into_iter().map(|(_, c, _)| c).collect();
        assert_eq!(names, ["test", "test"]);
    }

    // covers: C-15
    #[test]
    fn a_suggestion_takes_its_scope_and_none_goes_where_no_scope_fits() {
        let text = workflow(&[
            (
                "test",
                "      - run: cargo test\n        working-directory: back\n      - run: ./scripts/test.sh\n        working-directory: back\n",
            ),
            (
                "unit",
                "      - run: ./scripts/test.sh\n        working-directory: front\n",
            ),
            ("integration", "      - run: ./scripts/test.sh\n"),
        ]);
        let files = [Workflow::parse(".github/workflows/ci.yml".into(), text)];
        let dir = tempfile::tempdir().unwrap();
        // Scopes hushrun.yaml declares: one holds each project, and none the root.
        let policy = Policy::parse(
            "hushrun.yaml",
            b"version: 1\nrepository: github.com/a/b\nscopes:\n  back:\n    include: [\"back/**\"]\n  front:\n    include: [\"front/**\"]\n",
        )
        .unwrap();
        let proposal = Proposal::read(
            dir.path(),
            &files,
            Some(&policy),
            true,
            &ScriptChecks::new(),
        );
        let found: Vec<&str> = proposal
            .suggestions
            .iter()
            .map(|s| s.after.as_str())
            .collect();
        assert_eq!(
            found,
            [
                "hushrun run --scope back test-test -- ./scripts/test.sh",
                "hushrun run --scope front test -- ./scripts/test.sh",
            ]
        );
        // Projects init finds give theirs, as what it wraps there gets.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("back")).unwrap();
        std::fs::write(dir.path().join("back/Cargo.toml"), "[package]\n").unwrap();
        hushrun_testkit::git::init(dir.path(), "sha1");
        hushrun_testkit::git::commit_all(dir.path(), "init");
        let proposal = Proposal::read(dir.path(), &files, None, true, &ScriptChecks::new());
        assert_eq!(
            proposal.suggestions[0].after,
            "hushrun run --scope back test-test -- ./scripts/test.sh"
        );
    }

    // covers: C-15
    #[test]
    fn a_repository_set_up_gets_no_suggestions() {
        let files = [Workflow::parse(
            "ci.yml".into(),
            workflow(&[("test", "      - run: ./scripts/test.sh\n")]),
        )];
        let mut proposal = read_all(&files);
        assert_eq!(proposal.suggestions.len(), 1);
        proposal.keep_script_runs(&files);
        assert!(proposal.suggestions.is_empty());
    }

    // covers: C-15, C-25
    #[test]
    fn a_script_a_person_may_wrap_says_no_to_a_rule() {
        let ios = "  ios:\n    runs-on: macos-15\n    steps:\n      - uses: actions/checkout@v5\n      - run: xcodebuild test -scheme App\n";
        let text = |run: &str| {
            format!(
                "on: push\njobs:\n{ios}  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: {run}\n"
            )
        };
        let rule = |text: String| read_all(&[Workflow::parse("ci.yml".into(), text)]).macos;
        assert_eq!(rule(text("echo done")), [RuleLevel::Top]);
        // A rule would hold the script to a Mac once a person wraps it.
        assert_eq!(rule(text("./scripts/test.sh")), []);
    }

    // covers: C-15
    #[test]
    fn a_test_script_is_one_of_the_repository_whose_name_says_it_tests() {
        for (line, script) in [
            ("./scripts/test.sh", "./scripts/test.sh"),
            ("scripts/test.sh --verbose", "scripts/test.sh"),
            ("./runtest", "./runtest"),
            ("./runtest --clients 2", "./runtest"),
            ("script/test", "script/test"),
            ("bin/test", "bin/test"),
            ("./test", "./test"),
            ("bash scripts/ci/test.sh", "scripts/ci/test.sh"),
            ("bash -e ./scripts/run-tests.sh", "./scripts/run-tests.sh"),
            ("bash -eux ./scripts/test.sh", "./scripts/test.sh"),
            ("sh ci/unit_tests.sh", "ci/unit_tests.sh"),
            ("python runtests.py", "runtests.py"),
            (
                "python3 -u tests/runtests.py --parallel",
                "tests/runtests.py",
            ),
            ("node scripts/test.mjs", "scripts/test.mjs"),
            ("pwsh -File ./build/RunTests.ps1", "./build/RunTests.ps1"),
            ("./scripts/integration.sh", "./scripts/integration.sh"),
            ("./ci/check.sh", "./ci/check.sh"),
            ("./scripts/test-all.sh", "./scripts/test-all.sh"),
            ("./tools/integrationTest.sh", "./tools/integrationTest.sh"),
            ("./spec.sh", "./spec.sh"),
        ] {
            assert_eq!(test_script(&words(line)), Some(script), "{line}");
        }
        for line in [
            // A program on the PATH, and a builtin.
            "runtest",
            "test -f out.txt",
            "pytest",
            // Not a test by its name: setting up, building, and words that only hold one.
            "./scripts/setup-db.sh",
            "./scripts/build.sh test",
            "./gradlew test",
            "./x.py test",
            "./scripts/latest.sh",
            "./scripts/checkout.sh",
            "./scripts/community.sh",
            // Code of the interpreter's own, or a module.
            "bash -c ./scripts/test.sh",
            "bash -ec ./scripts/test.sh",
            "sh -s ./scripts/test.sh",
            "python -m test",
            "node -e test",
            "perl -e test",
            // Not the repository's, or not what it says.
            "/usr/local/bin/test.sh",
            "~/test.sh",
            "./scripts/$SUITE/test.sh",
            "",
        ] {
            assert_eq!(test_script(&words(line)), None, "{line}");
        }
    }

    // covers: C-15
    #[test]
    fn a_names_words_split_at_separators_and_capitals() {
        assert_eq!(words_of("run_tests"), ["run", "tests"]);
        assert_eq!(words_of("run-unit-tests"), ["run", "unit", "tests"]);
        assert_eq!(words_of("runTests"), ["run", "tests"]);
        assert_eq!(words_of("RunTests"), ["run", "tests"]);
        assert_eq!(words_of("CI"), ["ci"]);
        assert_eq!(words_of("test2Suite"), ["test2", "suite"]);
        assert!(words_of("").is_empty());
    }

    // covers: C-15
    #[test]
    fn a_job_is_named_for_checks_by_its_id_or_name() {
        for text in [
            "test",
            "Tests",
            "unit-tests",
            "integration",
            "rspec",
            "typecheck",
            "Run the tests on ${{ matrix.os }}",
        ] {
            assert!(named_for_checks(text), "{text}");
        }
        for text in ["build", "deploy", "lint", "release"] {
            assert!(!named_for_checks(text), "{text}");
        }
    }
}
