//! Which commands in a CI file are checks: one table, whatever the language (C-15). CI's own files
//! are the list of a repository's checks, so there is no second list to keep in step. Adding a
//! language is adding a row, with an example the tests run through `hushrun init`.
//!
//! A row matches a command once [`LAUNCHERS`] are off it: its program, then its words in order,
//! skipping options, `has` anywhere and none of `without`. \[ADR 0050\] A build tool's task can
//! follow a `clean`, and one that must stand alone has only options and `NAME=value`s after it.
//! Nothing here decides what a pass means: only that this command, on these files, gives the same
//! answer next time. That is why a check of the world outside the code, such as an advisory
//! database, is a row that says so and stays as it is.

use super::manager;
use super::recipes;
use super::scripts;

/// One kind of command.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Row {
    /// What the plan and the tests call it.
    pub language: &'static str,
    /// The program, `|` between the ways to write it.
    pub program: &'static str,
    /// What follows it in order, options aside, `|` between alternatives.
    pub words: &'static [&'static str],
    /// A word it must also have, `|` between alternatives; empty for none.
    pub has: &'static str,
    /// The check's name; empty for the first of `words` as the command writes it.
    pub name: &'static str,
    /// A command it matches, which the tests wrap.
    pub example: &'static str,
    /// Why init leaves this command as it is; empty for a check.
    pub leave: &'static str,
    /// \[C-26\] What kind of check it is: a linter's skip leaves coverage and test results as
    /// they were.
    pub kind: Kind,
    /// \[ADR 0050\] Words it mustn't have, `|` between them: each counts alone, or with `=` and
    /// any value but `false`, as `-DskipTests=true` does.
    pub without: &'static str,
    /// \[ADR 0050\] A build tool's task, which a `clean` before it leaves as it is.
    pub after_clean: bool,
    /// \[ADR 0050\] Nothing but options and `NAME=value` follows its words: another task, such
    /// as `publish` after `build`, could do anything.
    pub alone: bool,
}

/// \[C-26\] What a check is, as the run tells a linter from the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Linters, formatters, type checkers and vet.
    Lint,
    /// Everything else: tests, builds, and what may write anything.
    Test,
}

/// What a command's answer depends on when it isn't only the code.
const ADVISORIES: &str = "its answer changes with an advisory database, not only with the code";
/// What `fastlane scan` leaves behind by default, whatever its options.
const SCAN_REPORTS: &str = "it writes reports to fastlane/test_output, which a skip wouldn't";
/// What a package manager runs: one of `package.json`'s scripts.
const SCRIPT: &str = "package-script";
/// \[ADR 0055\] What `cargo xtask` runs: one of the repository's own tasks.
const XTASK: &str = "xtask-task";

const fn check(
    language: &'static str,
    program: &'static str,
    words: &'static [&'static str],
    name: &'static str,
    example: &'static str,
) -> Row {
    Row {
        language,
        program,
        words,
        has: "",
        name,
        example,
        leave: "",
        kind: Kind::Test,
        without: "",
        after_clean: false,
        alone: false,
    }
}

/// A row for a linter, a formatter, a type checker or vet.
const fn lint(row: Row) -> Row {
    Row {
        kind: Kind::Lint,
        ..row
    }
}

const fn needs(row: Row, has: &'static str) -> Row {
    Row { has, ..row }
}

/// \[ADR 0050\] A row a command isn't with any of `words`.
const fn without(row: Row, words: &'static str) -> Row {
    Row {
        without: words,
        ..row
    }
}

/// \[ADR 0050\] A build tool's task: a `clean` before it is looked past.
const fn task(row: Row) -> Row {
    Row {
        after_clean: true,
        ..row
    }
}

/// \[ADR 0050\] A row of a command with no other task after its words.
const fn alone(row: Row) -> Row {
    Row { alone: true, ..row }
}

/// \[ADR 0050\] What turns Maven's tests off, so that its phase is a build.
const SKIPS_TESTS: &str = "-DskipTests|-Dmaven.test.skip|-Dmaven.test.skip.exec";

/// \[ADR 0050\] What makes `ctest` list its tests, or run a dashboard that can build and submit
/// its results, rather than run them here.
const CTEST_ELSE: &str = "-N|--show-only|-D|--dashboard|-M|--test-model|-T|--test-action|-S|-SP|-SR|--script|--script-new-process|--version|--help";

const fn leave(
    language: &'static str,
    program: &'static str,
    words: &'static [&'static str],
    example: &'static str,
) -> Row {
    Row {
        language,
        program,
        words,
        has: "",
        name: "",
        example,
        leave: ADVISORIES,
        kind: Kind::Test,
        without: "",
        after_clean: false,
        alone: false,
    }
}

/// Targets a task runner's check goes by. A target isn't a tool hushrun knows, so only these wrap.
const TARGETS: &str = "test|tests|check|checks|lint|vet|typecheck|fmt-check|format-check|unit";

/// What starts another program in a project's environment: it isn't the check.
pub const LAUNCHERS: &[&[&str]] = &[
    &["uv", "run"],
    &["poetry", "run"],
    &["pipenv", "run"],
    &["pdm", "run"],
    &["rye", "run"],
    &["hatch", "run"],
    &["python", "-m"],
    &["python3", "-m"],
    &["bundle", "exec"],
    &["npx"],
    &["bunx"],
    &["pnpm", "exec"],
    &["yarn", "exec"],
];

/// The table.
pub const ROWS: &[Row] = &[
    // Rust
    check("Rust", "cargo", &["test"], "test", "cargo test --locked"),
    check(
        "Rust",
        "cargo",
        &["nextest", "run"],
        "test",
        "cargo nextest run --workspace",
    ),
    lint(check(
        "Rust",
        "cargo",
        &["clippy"],
        "clippy",
        "cargo clippy --all-targets -- -D warnings",
    )),
    lint(check(
        "Rust",
        "cargo",
        &["check"],
        "check",
        "cargo check --workspace",
    )),
    lint(needs(
        check("Rust", "cargo", &["fmt"], "fmt", "cargo fmt --all --check"),
        "--check",
    )),
    // \[ADR 0055\] rustdoc, which fails on what its flags make an error, such as a broken link
    // under `-D warnings`; `--open` starts a browser.
    lint(without(
        check(
            "Rust",
            "cargo",
            &["doc|rustdoc"],
            "rustdoc",
            "cargo doc --workspace --no-deps",
        ),
        "--open",
    )),
    // \[ADR 0055\] The repository's own tasks, which can do anything: a task runner's names, or
    // one asked only to `--check`.
    Row {
        language: "Rust",
        program: "cargo",
        words: &["xtask"],
        has: "",
        name: XTASK,
        example: "cargo xtask notices --check",
        leave: "",
        kind: Kind::Test,
        without: "",
        after_clean: false,
        alone: false,
    },
    leave("Rust", "cargo", &["deny"], "cargo deny check"),
    leave("Rust", "cargo", &["audit"], "cargo audit"),
    // Go
    check("Go", "go", &["test"], "test", "go test ./..."),
    lint(check("Go", "go", &["vet"], "vet", "go vet ./...")),
    lint(check(
        "Go",
        "golangci-lint",
        &["run"],
        "lint",
        "golangci-lint run",
    )),
    lint(check(
        "Go",
        "staticcheck",
        &[],
        "staticcheck",
        "staticcheck ./...",
    )),
    leave("Go", "govulncheck", &[], "govulncheck ./..."),
    // Python
    check("Python", "pytest", &[], "test", "pytest -q"),
    lint(check("Python", "ruff", &["check"], "ruff", "ruff check .")),
    lint(needs(
        check(
            "Python",
            "ruff",
            &["format"],
            "ruff-format",
            "ruff format --check .",
        ),
        "--check",
    )),
    lint(check("Python", "mypy", &[], "mypy", "mypy src")),
    lint(check("Python", "pyright", &[], "pyright", "pyright")),
    lint(check("Python", "flake8", &[], "flake8", "flake8 src")),
    lint(needs(
        check("Python", "black", &[], "black", "black --check ."),
        "--check",
    )),
    check("Python", "tox", &[], "tox", "tox -e py312"),
    check("Python", "nox", &[], "nox", "nox -s tests"),
    // \[ADR 0050\] The standard library's runner, past its launcher: `python -m unittest`. Its
    // program is the module, as `python -m pytest`'s is, never `test`, which is the shell's own.
    check(
        "Python",
        "unittest",
        &[],
        "test",
        "python -m unittest discover -s tests",
    ),
    leave("Python", "pip-audit", &[], "pip-audit"),
    // Java and Kotlin. \[ADR 0050\] A build runs the tests: Maven's phases unless they're
    // turned off, and Gradle's `build`; and each after a `clean`.
    without(
        task(check(
            "Java",
            "mvn|./mvnw|mvnw",
            &["test"],
            "test",
            "mvn -B test",
        )),
        SKIPS_TESTS,
    ),
    without(
        task(check(
            "Java",
            "mvn|./mvnw|mvnw",
            &["verify"],
            "verify",
            "./mvnw verify",
        )),
        SKIPS_TESTS,
    ),
    // `install` puts what it built where later steps may look for it: with no other phase or
    // goal after it, such as `deploy`.
    alone(without(
        task(check(
            "Java",
            "mvn|./mvnw|mvnw",
            &["install"],
            "install",
            "mvn -B clean install",
        )),
        SKIPS_TESTS,
    )),
    task(check(
        "Java",
        "gradle|./gradlew|gradlew",
        &["test"],
        "test",
        "./gradlew test",
    )),
    task(check(
        "Java",
        "gradle|./gradlew|gradlew",
        &["check"],
        "check",
        "gradle check",
    )),
    // With no other task after it, such as `publish`, or `-x test`.
    alone(task(check(
        "Java",
        "gradle|./gradlew|gradlew",
        &["build"],
        "build",
        "./gradlew build",
    ))),
    // Scala
    alone(task(check("Scala", "sbt", &["test"], "test", "sbt test"))),
    lint(check(
        "Kotlin",
        "gradle|./gradlew|gradlew",
        &["detekt"],
        "detekt",
        "./gradlew detekt",
    )),
    lint(check(
        "Kotlin",
        "gradle|./gradlew|gradlew",
        &["ktlintCheck"],
        "ktlint",
        "./gradlew ktlintCheck",
    )),
    // Ruby
    check("Ruby", "rspec|bin/rspec", &[], "test", "bundle exec rspec"),
    lint(check(
        "Ruby",
        "rubocop|bin/rubocop",
        &[],
        "rubocop",
        "bundle exec rubocop",
    )),
    check(
        "Ruby",
        "rake|bin/rake",
        &["test|spec"],
        "test",
        "bundle exec rake test",
    ),
    // \[ADR 0050\] Rake with no task runs its default one, which a Ruby project makes its tests.
    alone(check(
        "Ruby",
        "rake|bin/rake",
        &[],
        "test",
        "bundle exec rake",
    )),
    check(
        "Ruby",
        "rails|bin/rails",
        &["test"],
        "test",
        "bin/rails test",
    ),
    leave(
        "Ruby",
        "bundle-audit|bundler-audit",
        &[],
        "bundle-audit check",
    ),
    // PHP
    check(
        "PHP",
        "phpunit|vendor/bin/phpunit|./vendor/bin/phpunit",
        &[],
        "test",
        "vendor/bin/phpunit",
    ),
    check(
        "PHP",
        "pest|vendor/bin/pest|./vendor/bin/pest",
        &[],
        "test",
        "vendor/bin/pest",
    ),
    lint(check(
        "PHP",
        "phpstan|vendor/bin/phpstan|./vendor/bin/phpstan",
        &["analyse|analyze"],
        "phpstan",
        "vendor/bin/phpstan analyse",
    )),
    lint(check(
        "PHP",
        "psalm|vendor/bin/psalm|./vendor/bin/psalm",
        &[],
        "psalm",
        "vendor/bin/psalm",
    )),
    check("PHP", "composer", &["test"], "test", "composer test"),
    leave("PHP", "composer", &["audit"], "composer audit"),
    // .NET
    check(
        ".NET",
        "dotnet",
        &["test"],
        "test",
        "dotnet test --no-restore",
    ),
    lint(needs(
        check(
            ".NET",
            "dotnet",
            &["format"],
            "format",
            "dotnet format --verify-no-changes",
        ),
        "--verify-no-changes",
    )),
    // Elixir
    check("Elixir", "mix", &["test"], "test", "mix test"),
    lint(check(
        "Elixir",
        "mix",
        &["credo"],
        "credo",
        "mix credo --strict",
    )),
    lint(check(
        "Elixir",
        "mix",
        &["dialyzer"],
        "dialyzer",
        "mix dialyzer",
    )),
    lint(needs(
        check(
            "Elixir",
            "mix",
            &["format"],
            "format",
            "mix format --check-formatted",
        ),
        "--check-formatted",
    )),
    leave("Elixir", "mix", &["deps.audit"], "mix deps.audit"),
    // Swift
    check("Swift", "swift", &["test"], "test", "swift test"),
    lint(check(
        "Swift",
        "swiftlint",
        &[],
        "swiftlint",
        "swiftlint --strict",
    )),
    // Its action can stand anywhere among its options: `verdict` looks for it.
    needs(
        check(
            "Swift",
            "xcodebuild",
            &[],
            "test",
            "xcodebuild -scheme App test",
        ),
        "test|test-without-building",
    ),
    // Fastlane's own action for the tests. A lane, such as `fastlane test`, can do anything, and
    // the table doesn't know it.
    Row {
        leave: SCAN_REPORTS,
        ..leave("Swift", "fastlane", &["scan"], "bundle exec fastlane scan")
    },
    // \[ADR 0050\] C and C++: CMake's runner and Meson's, of a build an earlier step made.
    without(
        check(
            "C and C++",
            "ctest",
            &[],
            "test",
            "ctest --output-on-failure",
        ),
        CTEST_ELSE,
    ),
    without(
        check(
            "C and C++",
            "meson",
            &["test"],
            "test",
            "meson test -C build",
        ),
        "--list",
    ),
    // \[ADR 0050\] Dart, and Flutter's
    check("Dart", "dart", &["test"], "test", "dart test"),
    check("Dart", "flutter", &["test"], "test", "flutter test"),
    // Terraform
    lint(needs(
        check(
            "Terraform",
            "terraform",
            &["fmt"],
            "terraform-fmt",
            "terraform fmt -check -recursive",
        ),
        "-check",
    )),
    lint(check(
        "Terraform",
        "terraform",
        &["validate"],
        "terraform-validate",
        "terraform validate",
    )),
    // JavaScript: tools a project's CI runs by name, and its package manager's scripts, for a
    // package.json init doesn't edit: one in a subdirectory of the repository.
    check("JavaScript", "vitest", &["run"], "test", "vitest run"),
    check("JavaScript", "jest", &[], "test", "jest --ci"),
    lint(check("JavaScript", "eslint", &[], "lint", "eslint .")),
    lint(check("JavaScript", "tsc", &[], "typecheck", "tsc --noEmit")),
    check(
        "JavaScript",
        "playwright",
        &["test"],
        "test-e2e",
        "playwright test",
    ),
    check("JavaScript", "node", &[], "test", "node --test"),
    leave("JavaScript", "npm|pnpm|yarn", &["audit"], "npm audit"),
    // Bun's own test runner, which never reads package.json: no script stands in for it, so it
    // counts whatever the repository's root holds.
    check("Bun", "bun", &["test"], "test", "bun test"),
    // \[ADR 0050\] Deno's, which no script stands in for either.
    check("Deno", "deno", &["test"], "test", "deno test --allow-read"),
    Row {
        language: "JavaScript",
        program: "npm|pnpm|yarn|bun",
        words: &[],
        has: "",
        name: SCRIPT,
        example: "pnpm test",
        leave: "",
        // Its script's name says: see `kind_of`.
        kind: Kind::Test,
        without: "",
        after_clean: false,
        alone: false,
    },
    // Task runners
    check("make", "make", &[TARGETS], "", "make test"),
    check("just", "just", &[TARGETS], "", "just lint"),
    check("task", "task", &[TARGETS], "", "task check"),
];

/// \[ADR 0045\] Whether `words`, launchers aside, are a check that means little away from a Mac:
/// the only ones that earn a rule. A narrow list on purpose: a rule init guesses wrongly costs skips
/// for ever. `swift` is here though it runs on Linux too: init only asks of a job on a Mac, and
/// another of the same check on Linux says no.
pub fn earns_a_rule(words: &[&str]) -> bool {
    matches!(without_launchers(words), ["xcodebuild", ..] | ["swift", ..])
}

/// \[ADR 0045\] Whether `words` can stand under a rule another check earned. `swiftlint` reads the
/// same on any machine that has it, so it earns none, and it doesn't stop one.
pub fn fits_a_rule(words: &[&str]) -> bool {
    earns_a_rule(words) || without_launchers(words).first() == Some(&"swiftlint")
}

/// Whether `words`, what a command's output is piped into, only show it: a skip leaves them nothing
/// to read, and they write no report a later step could want.
pub fn only_shows_output(words: &[&str]) -> bool {
    matches!(
        without_launchers(words),
        ["xcpretty", ..] | ["xcbeautify", ..]
    ) && !words.contains(&"-r")
        && writes_a_report(words).is_none()
}

/// What the table says of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A check, and the name it goes by.
    Check(String),
    /// A command the table knows and leaves as it is, for this reason.
    Leave(&'static str),
}

/// What the table says of `words`, or `None` when it doesn't know it: a command nothing here
/// recognizes stays as it is, and says nothing. `javascript` says whether JavaScript's rows count:
/// a repository with a package.json at its root wraps its checks there, as it always did.
pub fn recognize(words: &[&str], javascript: bool) -> Option<Verdict> {
    let command = without_launchers(words);
    ROWS.iter()
        .filter(|row| javascript || row.language != "JavaScript")
        .find_map(|row| verdict(row, command))
}

/// \[C-26\] What kind of check `words` are, as the table says. A command it doesn't know may write
/// anything, so it counts as a test; a package manager's script goes by its name, and a task
/// runner's target by its own.
pub fn kind_of(words: &[&str]) -> Kind {
    let command = without_launchers(words);
    for row in ROWS {
        let Some(Verdict::Check(name)) = verdict(row, command) else {
            continue;
        };
        if row.name == SCRIPT {
            let script = manager::script_of(command).unwrap_or_default();
            return match scripts::kind(&script) {
                Some(scripts::Kind::Lint | scripts::Kind::Typecheck) => Kind::Lint,
                _ => Kind::Test,
            };
        }
        if row.words == [TARGETS] || row.name == XTASK {
            return target_kind(&name).unwrap_or(Kind::Test);
        }
        return row.kind;
    }
    Kind::Test
}

/// \[ADR 0050\] The kind of the task runner's target `name`, when it is one of [`TARGETS`]: a
/// check without its recipe read.
pub(super) fn target_kind(name: &str) -> Option<Kind> {
    if !TARGETS.split('|').any(|target| target == name) {
        return None;
    }
    Some(match name {
        "lint" | "vet" | "typecheck" | "fmt-check" | "format-check" => Kind::Lint,
        _ => Kind::Test,
    })
}

/// \[ADR 0050\] What the table says of `words`, run in `dir` of the repository that `read` reads
/// by paths from its root: as [`recognize`] says, or for a `make`, `just` or `task` target it
/// doesn't name, what the target's recipe runs (C-15). Without `dir`, no recipe is read.
pub fn recognize_in(
    words: &[&str],
    javascript: bool,
    dir: Option<&str>,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Option<Verdict> {
    recognize(words, javascript).or_else(|| match recipes::target(words, dir?, read)? {
        recipes::Recipe::Check(name, _) => Some(Verdict::Check(name)),
        recipes::Recipe::Leave(why) => Some(Verdict::Leave(why)),
    })
}

/// \[ADR 0050\] \[C-26\] What kind of check `words`, run in `dir`, are: as [`kind_of`] says, but
/// a task runner's target the table doesn't name is a linter when every check its recipe runs is
/// one. init's plan and the run read the same recipe, so they agree.
pub fn kind_in(words: &[&str], dir: Option<&str>, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Kind {
    if recognize(words, true).is_some() {
        return kind_of(words);
    }
    match dir.and_then(|dir| recipes::target(words, dir, read)) {
        Some(recipes::Recipe::Check(_, kind)) => kind,
        _ => Kind::Test,
    }
}

/// \[ADR 0050\] Why a target stays whose recipe runs a command the table leaves for `why`.
pub(super) fn in_a_recipe(why: &'static str) -> &'static str {
    match why {
        ADVISORIES => {
            "its recipe's answer changes with an advisory database, not only with the code"
        }
        SCAN_REPORTS => "its recipe writes reports to fastlane/test_output, which a skip wouldn't",
        _ => "its recipe runs a command init leaves as it is",
    }
}

/// \[ADR 0050\] The options a launcher takes before the program it starts, which
/// [`without_launchers`] reads past: those that take no value, then those that take one, as the
/// next word or after `=`. An option it doesn't know stops it, so that a command it can't read for
/// sure is no check.
const LAUNCHER_OPTIONS: &[(&[&str], &[&str], &[&str])] = &[
    (
        &["uv", "run"],
        &[
            "--no-sync",
            "--frozen",
            "--locked",
            "--no-project",
            "--all-extras",
            "--no-dev",
            "--isolated",
            "--active",
            "--exact",
            "--offline",
            "--no-editable",
            "--all-packages",
            "--no-cache",
            "-q",
            "--quiet",
            "-v",
            "--verbose",
        ],
        &[
            "--with",
            "--with-editable",
            "--with-requirements",
            "--extra",
            "--group",
            "--only-group",
            "--no-group",
            "--package",
            "--project",
            "--directory",
            "--python",
            "-p",
            "--env-file",
            "--index",
            "--default-index",
            "--index-url",
            "--extra-index-url",
            "--find-links",
            "--prerelease",
            "--resolution",
        ],
    ),
    (
        &["npx"],
        &["--yes", "-y", "--no", "--no-install", "--quiet", "-q"],
        &["--package", "-p"],
    ),
];

/// `words` without what starts a program in a project's environment, such as `uv run`, \[ADR
/// 0050\] and the launcher's own options, as `uv run --no-sync pytest` has. With an option the
/// launcher table doesn't know, they are left as they are, and name no program the table knows.
pub(crate) fn without_launchers<'a>(words: &'a [&'a str]) -> &'a [&'a str] {
    let Some(launcher) = LAUNCHERS
        .iter()
        .find(|launcher| words.len() > launcher.len() && words.starts_with(launcher))
    else {
        return words;
    };
    let mut rest = &words[launcher.len()..];
    if let Some((_, flags, valued)) = LAUNCHER_OPTIONS.iter().find(|(l, _, _)| l == launcher) {
        while let Some((first, after)) = rest.split_first() {
            if *first == "--" {
                rest = after;
                break;
            }
            if !first.starts_with('-') {
                break;
            }
            rest = match after.split_first() {
                _ if flags.contains(first) => after,
                Some((_, value_after)) if valued.contains(first) => value_after,
                _ if first
                    .split_once('=')
                    .is_some_and(|(name, _)| valued.contains(&name)) =>
                {
                    after
                }
                _ => return words,
            };
        }
    }
    if rest.is_empty() { words } else { rest }
}

fn verdict(row: &Row, words: &[&str]) -> Option<Verdict> {
    let (program, rest) = words.split_first()?;
    if !row.program.split('|').any(|name| name == *program) {
        return None;
    }
    if row.name == SCRIPT {
        return script(words);
    }
    if row.name == XTASK {
        return xtask(rest);
    }
    if !row.has.is_empty()
        && !rest
            .iter()
            .any(|word| row.has.split('|').any(|has| has == *word))
    {
        return None;
    }
    // `node` runs anything: only its test runner is a check.
    if row.program == "node" && !rest.contains(&"--test") {
        return None;
    }
    if row.program == "xcodebuild" && !xcodebuild_tests(rest, row.has) {
        return None;
    }
    if rest.iter().any(|word| one_of(row.without, word)) {
        return None;
    }
    let mut named = None;
    let mut rest = rest.iter().copied().filter(|word| !option(word)).peekable();
    if row.after_clean {
        while rest.next_if_eq(&"clean").is_some() {}
    }
    for wanted in row.words {
        let word = rest.next()?;
        if !wanted.split('|').any(|alternative| alternative == word) {
            return None;
        }
        named.get_or_insert(word);
    }
    if row.alone && rest.any(|word| !scripts::assignment(word)) {
        return None;
    }
    Some(match (row.leave, row.name) {
        ("", "") => Verdict::Check(named?.to_string()),
        ("", name) => Verdict::Check(name.to_string()),
        (why, _) => Verdict::Leave(why),
    })
}

/// Whether `words`, what follows `xcodebuild`, ask it for one of `actions`. An action can stand
/// anywhere among its options, most of which take a value, and a scheme can be called `test`. So a
/// word counts first, or after a word that isn't an option, or after an option known to take no
/// value. After any other option it is that option's value: not knowing one costs a skip, and never
/// wraps a build.
fn xcodebuild_tests(words: &[&str], actions: &str) -> bool {
    const NO_VALUE: &[&str] = &[
        "-allowProvisioningDeviceRegistration",
        "-allowProvisioningUpdates",
        "-disableAutomaticPackageResolution",
        "-hideShellScriptEnvironment",
        "-onlyUsePackageVersionsFromResolvedFile",
        "-parallelizeTargets",
        "-quiet",
        "-showBuildTimingSummary",
        "-skipMacroValidation",
        "-skipPackagePluginValidation",
        "-verbose",
    ];
    // These ask something else of it, and it runs nothing.
    const ASKS: &[&str] = &[
        "-dry-run",
        "-help",
        "-list",
        "-n",
        "-showBuildSettings",
        "-showTestPlans",
        "-showdestinations",
        "-showsdks",
        "-usage",
        "-version",
    ];
    let takes_a_value = |word: &str| {
        word.starts_with('-') && !word.contains([':', '=']) && !NO_VALUE.contains(&word)
    };
    !words.iter().any(|word| ASKS.contains(word))
        && words.iter().enumerate().any(|(at, word)| {
            actions.split('|').any(|action| action == *word)
                && (at == 0 || !takes_a_value(words[at - 1]))
        })
}

/// \[ADR 0055\] A task of the repository's own, `cargo xtask <task>`, as `rest` follows `cargo`:
/// a check named after itself when its name is one of [`TARGETS`], as a task runner's target is,
/// or when it is asked to `--check`, which looks and changes nothing. Any other task can do
/// anything, such as release, so the table doesn't know it, and init only suggests it.
fn xtask(rest: &[&str]) -> Option<Verdict> {
    let mut words = rest.iter().copied().filter(|word| !option(word));
    if words.next()? != "xtask" {
        return None;
    }
    let task = words.next()?;
    let checks = TARGETS.split('|').any(|target| target == task) || rest.contains(&"--check");
    let name = crate::cmd::wrap::made_name(task)?;
    checks.then(|| Verdict::Check(name.as_str().to_string()))
}

/// \[ADR 0055\] The task of `cargo xtask <task>` that `words` run, any task, when they do: what
/// init suggests where the table doesn't know it.
pub fn xtask_task<'a>(words: &[&'a str]) -> Option<&'a str> {
    let (&"cargo", rest) = words.split_first()? else {
        return None;
    };
    let mut words = rest.iter().copied().filter(|word| !option(word));
    (words.next()? == "xtask").then_some(())?;
    words.next()
}

/// A package manager's script that is a check, named as C-9 names it.
fn script(words: &[&str]) -> Option<Verdict> {
    let script = manager::script_of(words)?;
    let check = scripts::kind(&script).is_some() || script.starts_with("test");
    check.then(|| Verdict::Check(script.replace(':', "-")))
}

fn option(word: &str) -> bool {
    word.starts_with('-') || word.starts_with('+')
}

/// \[ADR 0050\] Whether `word` is one of `words`, `|` between them, written alone or with `=` and
/// a value: `-DskipTests` and `-DskipTests=true` are, and `-DskipTests=false` isn't.
fn one_of(words: &str, word: &str) -> bool {
    !words.is_empty()
        && words.split('|').any(|one| match word.strip_prefix(one) {
            Some("") => true,
            Some(value) => value
                .strip_prefix('=')
                .is_some_and(|value| !value.eq_ignore_ascii_case("false")),
            None => false,
        })
}

/// Why a command that writes a report can't skip: a later step may read the file, and a skip
/// wouldn't write it. A flag that turns one off, such as `--no-coverage` or `--coverage=false`,
/// and a reporter that only prints, such as `--reporter=dot`, write nothing.
pub fn writes_a_report(words: &[&str]) -> Option<String> {
    report_flag(words).map(|flag| format!("it writes a report ({flag}), which a skip wouldn't"))
}

/// What a reporter shows on the terminal only: its value says so.
const PRINTS_ONLY: &[&str] = &[
    "dot", "spec", "list", "verbose", "default", "progress", "min", "tap", "summary", "basic",
];

/// What a flag's name holds when it writes a report: a coverage tool's, a test results format's,
/// or a file it writes to. `-o` counts only whole.
const REPORT_FLAGS: &[&str] = &[
    "cover",
    "junit",
    "--logger",
    "--output",
    "--results",
    "--html",
    "CodeCoverage",
    "resultBundle",
    "xunit",
];

/// The flag of `words` that writes a report, with its value when that is what says so.
fn report_flag(words: &[&str]) -> Option<String> {
    for (at, &word) in words.iter().enumerate() {
        // A value of its own, as `--reporter junit` writes it.
        let next = words
            .get(at + 1)
            .copied()
            .filter(|next| !next.starts_with('-'));
        let with = |value: Option<&str>| match value {
            Some(value) if !word.contains(value) => format!("{word} {value}"),
            _ => word.to_string(),
        };
        // MSBuild's properties: `/p:CollectCoverage=true`, `-p:CollectCoverage=true`.
        if let Some(property) = word
            .strip_prefix("/p:")
            .or_else(|| word.strip_prefix("-p:"))
        {
            if property.eq_ignore_ascii_case("CollectCoverage=true") {
                return Some(word.to_string());
            }
            continue;
        }
        if !word.starts_with('-') {
            continue;
        }
        // Turned off: `--no-cov`, `--no-coverage`, `--coverage=false`, `--coverage false`.
        if word.starts_with("--no-") || word.ends_with("=false") || next == Some("false") {
            continue;
        }
        let (name, value) = match word.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (word, None),
        };
        // pytest-cov: `--cov`, `--cov=src`, `--cov-report=xml`.
        if name == "--cov" || name.starts_with("--cov-") {
            return Some(word.to_string());
        }
        // RSpec's formatters: `--format RspecJunitFormatter`, `--require rspec_junit_formatter`.
        if matches!(name, "--format" | "-f") {
            let value = value.or(next);
            if value.is_some_and(|value| value.to_ascii_lowercase().contains("junit")) {
                return Some(with(value));
            }
            continue;
        }
        if matches!(name, "--require" | "-r") {
            let value = value.or(next);
            if value == Some("rspec_junit_formatter") {
                return Some(with(value));
            }
            continue;
        }
        // .NET's collectors: `--collect:"XPlat Code Coverage"`, `--collect "Code Coverage"`.
        if let Some(rest) = word.strip_prefix("--collect") {
            let value = match rest.strip_prefix([':', '=']) {
                Some(value) => Some(value),
                None if rest.is_empty() => next,
                None => None,
            };
            if value.is_some_and(|value| value.to_ascii_lowercase().contains("coverage")) {
                return Some(with(value));
            }
            continue;
        }
        // A flag about reports, such as `--make-reports` or `--reporter=junit`, unless its value
        // only prints, as `--reporter=dot` does.
        if name.to_ascii_lowercase().contains("report") {
            let value = value.or(next);
            let prints = value.is_some_and(|value| {
                value
                    .split(',')
                    .all(|part| PRINTS_ONLY.contains(&part.trim().to_ascii_lowercase().as_str()))
            });
            if prints {
                continue;
            }
            return Some(with(value.filter(|_| name.starts_with("--reporter"))));
        }
        if word == "-o" || REPORT_FLAGS.iter().any(|flag| word.contains(flag)) {
            return Some(word.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// \[ADR 0050\] A launcher's own options are read past, as the corpus's `uv run --no-sync
    /// pytest` needs; one the table doesn't know leaves the command no check.
    // covers: C-15
    #[test]
    fn a_launchers_own_options_are_read_past() {
        let is_check = |line: &str| {
            let words: Vec<&str> = line.split_whitespace().collect();
            matches!(recognize(&words, true), Some(Verdict::Check(_)))
        };
        for line in [
            "uv run --no-sync pytest tests",
            "uv run --frozen --with pytest-xdist pytest -n auto",
            "uv run --python 3.12 --group=test -- pytest -q",
            "uv run -p 3.13 --no-project --no-sync pytest",
            "npx --yes vitest run",
            "npx -p vitest vitest run",
        ] {
            assert!(is_check(line), "{line}");
        }
        for line in [
            "uv run --some-new-flag pytest",
            "uv run --with",
            "uv run --no-sync",
        ] {
            assert!(!is_check(line), "{line}");
        }
        assert_eq!(
            without_launchers(&["uv", "run", "--no-sync", "pytest", "-x"]),
            ["pytest", "-x"]
        );
        assert_eq!(
            without_launchers(&["uv", "run", "--unknown", "pytest"]),
            ["uv", "run", "--unknown", "pytest"]
        );
    }

    // covers: C-15
    #[test]
    fn every_row_matches_its_own_example() {
        for row in ROWS {
            let words: Vec<&str> = row.example.split_whitespace().collect();
            let found = recognize(&words, true)
                .unwrap_or_else(|| panic!("{}: {} isn't recognized", row.language, row.example));
            let wanted = match (row.leave, row.name) {
                ("", SCRIPT) => Verdict::Check("test".into()),
                // \[ADR 0055\] A task of the repository's own takes its task's name.
                ("", XTASK) => Verdict::Check(words[2].to_string()),
                ("", "") => Verdict::Check(words[1].to_string()),
                ("", name) => Verdict::Check(name.to_string()),
                (why, _) => Verdict::Leave(why),
            };
            assert_eq!(found, wanted, "{}: {}", row.language, row.example);
            // Every name is one `hushrun run` takes.
            if let Verdict::Check(name) = found {
                assert!(
                    hushrun_core::names::CheckName::new(&name).is_ok(),
                    "{name}: {}",
                    row.example
                );
            }
        }
    }

    // covers: C-15
    #[test]
    fn options_launchers_and_near_misses() {
        let check = |line: &str| -> Option<String> {
            let words: Vec<&str> = line.split_whitespace().collect();
            match recognize(&words, true) {
                Some(Verdict::Check(name)) => Some(name),
                _ => None,
            }
        };
        assert_eq!(check("cargo +nightly test").as_deref(), Some("test"));
        assert_eq!(check("mvn -B -ntp verify").as_deref(), Some("verify"));
        assert_eq!(check("./gradlew --no-daemon test").as_deref(), Some("test"));
        assert_eq!(check("uv run pytest -q").as_deref(), Some("test"));
        assert_eq!(check("poetry run mypy src").as_deref(), Some("mypy"));
        assert_eq!(check("python -m pytest").as_deref(), Some("test"));
        assert_eq!(check("python3 -m mypy .").as_deref(), Some("mypy"));
        assert_eq!(check("bundle exec rake spec").as_deref(), Some("test"));
        assert_eq!(check("make -j4 lint").as_deref(), Some("lint"));
        assert_eq!(check("pnpm test:e2e").as_deref(), Some("test-e2e"));
        assert_eq!(check("npm run lint").as_deref(), Some("lint"));
        assert_eq!(check("node --test npm/test/a.mjs").as_deref(), Some("test"));
        // What only looks like a check.
        for not in [
            "cargo build --locked",
            "cargo fmt --all",
            "go build ./...",
            "ruff format .",
            "black .",
            "terraform fmt",
            "terraform plan",
            "make deploy",
            "just release",
            "pnpm build",
            "pnpm install --frozen-lockfile",
            "node scripts/migrate.js",
            "dotnet format",
            "mix format",
            "gradle assemble",
            "python manage.py test",
            "echo cargo test",
        ] {
            assert_eq!(check(not), None, "{not}");
        }
        // A repository's own package.json wraps its checks where they are.
        assert_eq!(recognize(&["npm", "test"], false), None);
        assert_eq!(recognize(&["npx", "eslint", "."], false), None);
        assert_eq!(recognize(&["vitest", "run"], false), None);
        // Bun's own runner isn't a script, so package.json can't stand in for it.
        assert_eq!(
            recognize(&["bun", "test"], false),
            Some(Verdict::Check("test".into()))
        );
        assert_eq!(recognize(&["bun", "run", "test"], false), None);
    }

    // covers: C-15
    /// \[ADR 0055\] rustdoc is a linter's check, and the repository's own tasks count as a task
    /// runner's targets do: by a name that says check, or when asked to `--check`.
    #[test]
    fn rustdoc_and_the_repositorys_own_tasks() {
        let check = |line: &str| -> Option<(String, Kind)> {
            let words: Vec<&str> = line.split_whitespace().collect();
            match recognize(&words, true) {
                Some(Verdict::Check(name)) => Some((name, kind_of(&words))),
                _ => None,
            }
        };
        let rustdoc = Some(("rustdoc".to_string(), Kind::Lint));
        assert_eq!(check("cargo doc --workspace --no-deps --locked"), rustdoc);
        assert_eq!(check("cargo +nightly doc --no-deps"), rustdoc);
        assert_eq!(check("cargo rustdoc -p a -- -D warnings"), rustdoc);
        assert_eq!(check("cargo doc --open"), None);
        assert_eq!(
            check("cargo xtask notices --check"),
            Some(("notices".to_string(), Kind::Test))
        );
        assert_eq!(
            check("cargo xtask lint"),
            Some(("lint".to_string(), Kind::Lint))
        );
        assert_eq!(
            check("cargo xtask --verbose test --all"),
            Some(("test".to_string(), Kind::Test))
        );
        for not in [
            "cargo xtask trace",
            "cargo xtask dist",
            "cargo xtask release --dry-run",
            "cargo xtask",
            "cargo xtask --check",
        ] {
            assert_eq!(check(not), None, "{not}");
        }
        assert_eq!(xtask_task(&["cargo", "xtask", "trace"]), Some("trace"));
        assert_eq!(
            xtask_task(&["cargo", "+stable", "xtask", "dist"]),
            Some("dist")
        );
        assert_eq!(xtask_task(&["cargo", "test"]), None);
        assert_eq!(xtask_task(&["make", "xtask"]), None);
    }

    fn words(line: &str) -> Vec<&str> {
        line.split_whitespace().collect()
    }

    // covers: C-15
    #[test]
    fn more_runners_are_checks_and_what_only_looks_like_one_isnt() {
        let check = |line: &str| match recognize(&words(line), true) {
            Some(Verdict::Check(name)) => Some(name),
            _ => None,
        };
        for (line, name) in [
            ("python -m unittest", "test"),
            ("python3 -m unittest discover -s tests -p test_*.py", "test"),
            ("./gradlew build", "build"),
            ("./gradlew build --no-daemon", "build"),
            ("gradle --scan clean build", "build"),
            ("./gradlew clean test", "test"),
            ("mvn -B clean install", "install"),
            ("./mvnw install -DskipTests=false", "install"),
            ("mvn clean verify", "verify"),
            ("mvn -B -ntp verify -Dstyle.color=always", "verify"),
            ("sbt test", "test"),
            ("sbt clean test", "test"),
            ("sbt ++2.13.12 test", "test"),
            ("bundle exec rake", "test"),
            ("rake", "test"),
            ("bin/rake TESTOPTS=-v", "test"),
            ("ctest --output-on-failure", "test"),
            ("ctest -L main -C Release --verbose --timeout 900", "test"),
            ("ctest --test-dir build -j4", "test"),
            ("meson test -C build", "test"),
            ("meson test --suite unit", "test"),
            ("flutter test", "test"),
            ("flutter test test/widget_test.dart", "test"),
            ("dart test", "test"),
            ("dart test -p chrome", "test"),
            ("deno test -A", "test"),
        ] {
            assert_eq!(check(line).as_deref(), Some(name), "{line}");
            assert_eq!(kind_of(&words(line)), Kind::Test, "{line}");
        }
        for not in [
            // Another task after one that must stand alone could publish.
            "./gradlew build publish",
            "./gradlew build -x test",
            "./gradlew assemble",
            "mvn install deploy",
            "mvn -B clean install sonar:sonar",
            "mvn clean",
            "mvn deploy",
            "sbt test publishSigned",
            "sbt publish",
            // The tests turned off.
            "mvn install -DskipTests",
            "mvn -B clean install -Dmaven.test.skip=true",
            "./mvnw verify -DskipTests",
            "mvn test -Dmaven.test.skip.exec=true",
            // Rake's other tasks.
            "bundle exec rake push",
            "rake release",
            "rake db:migrate",
            // A list, or a dashboard, in place of a run here.
            "ctest -N",
            "ctest --show-only=json-v1",
            "ctest -D Experimental",
            "ctest -S ci.cmake",
            "meson test --list",
            "meson compile -C build",
            "flutter build apk",
            "dart run test",
            "dart --enable-asserts dev/bots/test.dart",
            "deno run main.ts",
        ] {
            assert_eq!(check(not), None, "{not}");
        }
    }

    /// `python -m` is a launcher, so the row a command matches is its module's: a row for
    /// `python -m test` would be a row for `test`, which is the shell's own.
    // covers: C-15
    #[test]
    fn the_shells_test_and_what_one_repository_runs_are_no_checks() {
        for not in [
            "test -f x",
            "test -d build",
            "test -n $CI",
            "[ -f x ]",
            // CPython's own suite, Django's own script, and runners no other repository showed.
            "python -m test",
            "python3 -m test --fast-ci",
            "./python -m test --multiprocess 0",
            "python -Wall tests/runtests.py -v2",
            "python runtests.py --settings=test_sqlite",
            "hatch test",
            "pixi run test",
            "bin/flatware rspec",
        ] {
            assert_eq!(recognize(&words(not), true), None, "{not}");
        }
    }

    // covers: C-15, C-25
    #[test]
    fn what_earns_a_rule_is_the_swift_rows_init_wraps_but_swiftlint() {
        for row in ROWS {
            let swift = row.language == "Swift" && row.leave.is_empty();
            let lint = row.program == "swiftlint";
            assert_eq!(
                earns_a_rule(&words(row.example)),
                swift && !lint,
                "{}",
                row.example
            );
            assert_eq!(fits_a_rule(&words(row.example)), swift, "{}", row.example);
        }
        assert!(fits_a_rule(&words("bundle exec swiftlint --strict")));
        assert!(fits_a_rule(&words("npx swiftlint")));
        // swiftlint reads the same anywhere, so it can only stand beside what earned the rule.
        assert!(!earns_a_rule(&words("swiftlint --strict")));
        for not in [
            "",
            "swiftformat .",
            "npm test",
            "make test",
            "xcrun xcodebuild test",
            // It writes reports, so it stays, and a lane can do anything.
            "fastlane scan",
            "bundle exec fastlane scan",
            "fastlane test",
            "bundle exec fastlane ios test",
        ] {
            assert!(!fits_a_rule(&words(not)), "{not}");
        }
    }

    // covers: C-15, C-25
    #[test]
    fn xcodebuild_is_a_check_when_its_action_is_the_tests() {
        let check = |line: &str| recognize(&words(line), true);
        let test = Some(Verdict::Check("test".into()));
        for line in [
            "xcodebuild test",
            "xcodebuild test -scheme App",
            "xcodebuild -scheme App test",
            "xcodebuild -workspace App.xcworkspace -scheme App test",
            "xcodebuild -scheme App -destination platform=macOS test",
            "xcodebuild clean test -scheme App",
            "xcodebuild -scheme App clean test",
            "xcodebuild build test -scheme App",
            "xcodebuild test-without-building -scheme App",
            "xcodebuild -scheme App test-without-building",
            "xcodebuild -scheme test test",
            "xcodebuild -quiet test",
            "xcodebuild -quiet -scheme App test",
            "xcodebuild -scheme App -only-testing:AppTests test",
            "xcodebuild -scheme App CODE_SIGNING_ALLOWED=NO test",
            "bundle exec xcodebuild -scheme App test",
        ] {
            assert_eq!(check(line), test, "{line}");
        }
        for line in [
            "xcodebuild",
            "xcodebuild build",
            "xcodebuild build -scheme App",
            "xcodebuild -scheme App build",
            "xcodebuild archive -scheme App",
            "xcodebuild -scheme App archive",
            "xcodebuild build-for-testing -scheme App",
            "xcodebuild docbuild -scheme App",
            "xcodebuild analyze -scheme App",
            // `test` is what an option holds.
            "xcodebuild -scheme test",
            "xcodebuild -scheme test build",
            "xcodebuild -target test build",
            "xcodebuild -configuration test build",
            "xcodebuild -testPlan test build",
            "xcodebuild -derivedDataPath test build",
            "xcodebuild -sdk test build",
            // It asks, and runs nothing.
            "xcodebuild -showBuildSettings",
            "xcodebuild -showBuildSettings -scheme App",
            "xcodebuild -showBuildSettings test",
            "xcodebuild test -showBuildSettings",
            "xcodebuild test -scheme App -showBuildSettings",
            "xcodebuild -list -json",
            "xcodebuild -showdestinations -scheme App",
            "xcodebuild test -dry-run",
        ] {
            assert_eq!(check(line), None, "{line}");
        }
    }

    // covers: C-15
    #[test]
    fn fastlanes_scan_stays_with_its_reason_and_a_lane_isnt_a_check() {
        let check = |line: &str| recognize(&words(line), true);
        let stays = Some(Verdict::Leave(
            "it writes reports to fastlane/test_output, which a skip wouldn't",
        ));
        assert_eq!(check("fastlane scan"), stays);
        assert_eq!(check("bundle exec fastlane scan --scheme App"), stays);
        for line in [
            "fastlane test",
            "bundle exec fastlane test",
            "bundle exec fastlane ios test",
            "fastlane gym",
        ] {
            assert_eq!(check(line), None, "{line}");
        }
    }

    // covers: C-15
    #[test]
    fn what_only_shows_output_writes_nothing() {
        for line in [
            "xcpretty",
            "xcbeautify",
            "xcpretty --color --simple",
            "xcbeautify --quiet --is-ci",
            "bundle exec xcpretty",
        ] {
            assert!(only_shows_output(&words(line)), "{line}");
        }
        for line in [
            "",
            "tee log",
            "grep -v warning",
            "xcpretty -r junit",
            "xcpretty --report junit",
            "xcpretty --report html --output out.html",
            "xcpretty -o out.xml",
            "xcbeautify --report junit --report-path out",
            "xcbeautify --junit-report-filename out.xml",
        ] {
            assert!(!only_shows_output(&words(line)), "{line}");
        }
    }

    // covers: C-15
    #[test]
    fn a_report_a_later_step_reads_stays() {
        for words in [
            ["go", "test", "-coverprofile=c.out"],
            ["pytest", "--cov", "--junitxml=r.xml"],
            ["dotnet", "test", "--logger"],
            ["vitest", "run", "--coverage"],
            ["cargo", "test", "-o"],
            ["xcodebuild", "test", "-resultBundlePath"],
            ["xcodebuild", "test", "-enableCodeCoverage"],
            ["swift", "test", "--enable-code-coverage"],
            ["swift", "test", "--xunit-output"],
        ] {
            assert!(writes_a_report(&words).is_some(), "{words:?}");
        }
        assert_eq!(writes_a_report(&["go", "test", "-race"]), None);
        assert_eq!(writes_a_report(&["cargo", "test", "--locked"]), None);
    }

    // covers: C-26
    #[test]
    fn a_linter_is_told_from_a_test() {
        for line in [
            "cargo clippy --all-targets -- -D warnings",
            "cargo check --workspace",
            "cargo fmt --all --check",
            "go vet ./...",
            "uv run ruff check .",
            "mypy src",
            "npx eslint .",
            "tsc --noEmit",
            "bundle exec rubocop",
            "mix credo --strict",
            "terraform validate",
            "pnpm lint",
            "npm run typecheck",
            "yarn format:check",
            "make lint",
            "just typecheck",
        ] {
            assert_eq!(kind_of(&words(line)), Kind::Lint, "{line}");
        }
        for line in [
            "cargo test --locked",
            "uv run pytest --cov",
            "gradle check",
            "npm test",
            "pnpm run test:unit",
            "make test",
            "just check",
            "./scripts/ci.sh",
            "npm audit",
        ] {
            assert_eq!(kind_of(&words(line)), Kind::Test, "{line}");
        }
        // Every row says what it is, and a linter's is a check, not a command it leaves.
        for row in ROWS.iter().filter(|row| row.kind == Kind::Lint) {
            assert!(row.leave.is_empty(), "{}", row.example);
            assert_eq!(kind_of(&words(row.example)), Kind::Lint, "{}", row.example);
        }
    }

    // covers: C-15
    #[test]
    fn each_way_to_write_a_report_is_named() {
        let named = |line: &str| writes_a_report(&words(line));
        for (line, flag) in [
            // pytest-cov, alone and as n8n writes it.
            ("pytest --cov", "--cov"),
            (
                "uv run pytest --cov=src --cov-report=xml --cov-report=term-missing",
                "--cov=src",
            ),
            ("pytest --cov-report=xml", "--cov-report=xml"),
            // RSpec's JUnit formatter, its name a value of its own.
            (
                "bundle exec rspec --format RspecJunitFormatter --out rspec.xml",
                "--format RspecJunitFormatter",
            ),
            ("rspec -f RspecJunitFormatter", "-f RspecJunitFormatter"),
            (
                "rspec --require rspec_junit_formatter",
                "--require rspec_junit_formatter",
            ),
            // .NET: a coverage collector, MSBuild's coverlet property, a results directory.
            (
                "dotnet test --collect:XPlat_Code_Coverage",
                "--collect:XPlat_Code_Coverage",
            ),
            (
                "dotnet test /p:CollectCoverage=true",
                "/p:CollectCoverage=true",
            ),
            (
                "dotnet test -p:CollectCoverage=true",
                "-p:CollectCoverage=true",
            ),
            ("dotnet test --results-directory out", "--results-directory"),
            // Flags about reports, unless they only print.
            ("python manage.py test --make-reports", "--make-reports"),
            ("vitest run --reporter=junit", "--reporter=junit"),
            ("vitest run --reporter junit", "--reporter junit"),
            (
                "jest --reporters=default --reporters=jest-junit",
                "--reporters=jest-junit",
            ),
            ("mocha --reporter dot,junit", "--reporter dot,junit"),
        ] {
            assert_eq!(
                named(line),
                Some(format!(
                    "it writes a report ({flag}), which a skip wouldn't"
                )),
                "{line}"
            );
        }
        // The collector's name, as a shell hands it over: one word, spaces and all.
        for words in [
            ["dotnet", "test", "--collect:XPlat Code Coverage"],
            ["dotnet", "test", "--collect=XPlat Code Coverage"],
        ] {
            assert!(writes_a_report(&words).is_some(), "{words:?}");
        }
        assert_eq!(
            writes_a_report(&["dotnet", "test", "--collect", "Code Coverage"]),
            Some("it writes a report (--collect Code Coverage), which a skip wouldn't".into())
        );
        // What writes nothing: coverage turned off, reporters that print, other collectors.
        for line in [
            "pytest --no-cov",
            "pytest --no-coverage",
            "jest --coverage=false",
            "jest --coverage false",
            "vitest run --coverage.enabled=false",
            "vitest run --reporter=dot",
            "mocha --reporter spec",
            "jest --reporters=default",
            "vitest run --reporter=verbose,dot",
            "rspec --format documentation",
            "rspec -f progress",
            "rspec --require spec_helper",
            "dotnet test /p:CollectCoverage=false",
            "go test -race ./...",
            "cargo test --locked",
        ] {
            assert_eq!(named(line), None, "{line}");
        }
        assert_eq!(
            writes_a_report(&["dotnet", "test", "--collect", "Code Analysis"]),
            None
        );
    }
}
