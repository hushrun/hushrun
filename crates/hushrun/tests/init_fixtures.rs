//! Golden init fixtures (C-15): each case under `testdata/init/` is a minimal repository, written by
//! hand, of a pattern the survey of 100 repositories' CI found (its `case.md` says which), and
//! `expected.json` pins the plan `hushrun init --dry-run --format json` makes of it.
//! `HUSHRUN_UPDATE_GOLDEN=1` rewrites them instead, for a review of the diff in git.
//!
//! A9: init's prediction is the run's decision. For each line init wraps and each wrapped script a
//! job runs, the line runs twice in that job, as GitHub Actions would start it, against the fake
//! results server, with the tools of `testdata/init/bin` on `PATH`: the second run skips when the
//! plan says `skips`, and otherwise runs saying the plan's `ciText`, or in words of its own when
//! the plan has none. Which workflow GitHub names in a job, its own or a caller's, is read here
//! from the fixture's files, apart from init. \[ADR 0048\] The tools write the reports their
//! flags name, and each run records their summary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hushrun_testkit::{git, schema};
use serde_json::{Value, json};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the hosted backend would be: nothing listens on the discard port.
const NO_HOST: &str = "http://127.0.0.1:9";

/// The org's URL in a plan that signed in to nothing.
const PLACEHOLDER: &str = "<your org's URL, once you sign in>";

/// The cases every phase starts from (Appendix E of the plan).
const SEEDS: [&str; 15] = [
    "F01", "F02", "F03", "F04", "F05", "F06", "F30", "F31", "F32", "F33", "F34", "F35", "F36",
    "F37", "F38",
];

/// Variables a test must never inherit: CI's, and hushrun's own.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITHUB_JOB",
    "GITHUB_REPOSITORY",
    "GITHUB_WORKFLOW_REF",
    "GITHUB_WORKSPACE",
    "HUSHRUN_VARIANT",
    "HUSHRUN_WORKFLOW",
    "HUSHRUN_INPUTS",
    "GITHUB_OUTPUT",
    "GITHUB_STEP_SUMMARY",
    "GITLAB_CI",
    "CI_JOB_NAME",
    "CI_NODE_TOTAL",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_LOG",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_NPM_PACKAGE",
    "npm_lifecycle_event",
];

/// The cases, by name, in order.
fn cases() -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(hushrun_testkit::testdata("init"))
        .expect("testdata/init")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.join("repo").is_dir())
        .collect();
    cases.sort();
    cases
}

fn name(case: &Path) -> String {
    case.file_name()
        .expect("a name")
        .to_string_lossy()
        .into_owned()
}

/// A case's repository in a directory of its own, committed, with an origin on github.com, and a
/// home, cache and credentials of its own.
struct Case {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Case {
    fn new(case: &Path) -> Case {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("real path");
        let repo = root.join("repo");
        copy(&case.join("repo"), &repo);
        git::init(&repo, "sha1");
        git::run(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/shop.git",
            ],
            None,
        );
        git::commit_all(&repo, "the fixture");
        Case {
            _dir: dir,
            root,
            repo,
        }
    }

    /// `program` here, with nothing of the machine's hushrun, CI or git configuration.
    fn command(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        for var in CLEARED {
            cmd.env_remove(var);
        }
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        cmd
    }

    /// The plan, which its schema takes.
    fn plan(&self) -> Value {
        let out = self
            .command(&assert_cmd::cargo::cargo_bin("hushrun"))
            .args(["init", "--dry-run", "--format", "json"])
            .output()
            .expect("hushrun");
        assert!(out.status.success(), "{}", text(&out.stderr));
        let plan: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", text(&out.stdout)));
        schema::assert_valid(&schema::load("init-plan.v1.schema.json"), &plan);
        plan
    }
}

/// `from` under `to`, file by file, keeping the executable bit.
fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("entry");
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copy");
        }
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The plan as `expected.json` keeps it: this release's version made a placeholder, so that a
/// release changes no fixture. It is in an action's ref, a pin after `@`, and the range
/// package.json's devDependencies get (`"hushrun": "^<version>"`, quoted inside a diff).
fn pinned(plan: &Value) -> String {
    let mut plan = plan.clone();
    plan["tool"] = json!("hushrun <version>");
    let text = serde_json::to_string_pretty(&plan).expect("JSON");
    text.replace(&format!("@v{VERSION}"), "@v<version>")
        .replace(&format!("@^{VERSION}"), "@^<version>")
        .replace(&format!(r#"\"^{VERSION}\""#), r#"\"^<version>\""#)
        + "\n"
}

/// What changed from `expected` to `actual`, line by line, with a line of context.
fn line_diff(expected: &str, actual: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    // The longest common subsequence, from the end.
    let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut lines: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            lines.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || table[i][j + 1] >= table[i + 1][j]) {
            lines.push(('+', b[j]));
            j += 1;
        } else {
            lines.push(('-', a[i]));
            i += 1;
        }
    }
    let near = |n: usize| {
        (n.saturating_sub(1)..=(n + 1).min(lines.len().saturating_sub(1)))
            .any(|m| lines[m].0 != ' ')
    };
    lines
        .iter()
        .enumerate()
        .filter(|(n, _)| near(*n))
        .map(|(_, (mark, line))| format!("{mark} {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// covers: C-15
#[test]
fn every_fixture_gets_the_plan_it_expects() {
    let update = std::env::var("HUSHRUN_UPDATE_GOLDEN").is_ok_and(|v| v == "1");
    let cases = cases();
    let names: Vec<String> = cases.iter().map(|case| name(case)).collect();
    for seed in SEEDS {
        assert!(
            names.iter().any(|name| name.starts_with(seed)),
            "no case {seed}: {names:?}"
        );
    }
    let mut failures = Vec::new();
    for case in &cases {
        let actual = pinned(&Case::new(case).plan());
        let path = case.join("expected.json");
        if update {
            std::fs::write(&path, &actual).expect("write expected.json");
            continue;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_default();
        if expected != actual {
            failures.push(format!(
                "{}:\n{}",
                name(case),
                line_diff(&expected, &actual)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}\n\nRun with HUSHRUN_UPDATE_GOLDEN=1 to take these plans, and review the diff in git.",
        failures.join("\n\n")
    );
}

/// What starts a workflow: the keys of its `on:`.
fn triggers(workflow: &Value) -> Vec<String> {
    match workflow.get("on").or_else(|| workflow.get("true")) {
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// The workflow GitHub names in a job of `file`, `GITHUB_WORKFLOW_REF`: its own; for a reusable
/// workflow, the top-level workflow here that calls it, since GitHub names the outermost one; or
/// another repository's when none here does.
fn named_in(repo: &Path, file: &str) -> String {
    let dir = repo.join(".github/workflows");
    let mut workflows: Vec<(String, Value)> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("workflows") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(&path).expect("read");
        let value: Value = serde_saphyr::from_str(&text).expect("YAML");
        workflows.push((format!(".github/workflows/{name}"), value));
    }
    let called = |path: &str| {
        workflows
            .iter()
            .any(|(p, w)| p == path && triggers(w).iter().any(|t| t == "workflow_call"))
    };
    let otherwise = |path: &str| {
        workflows
            .iter()
            .any(|(p, w)| p == path && triggers(w).iter().any(|t| t != "workflow_call"))
    };
    let callers = |path: &str| -> Vec<String> {
        let mut found: Vec<String> = workflows
            .iter()
            .filter(|(_, w)| {
                w["jobs"].as_object().into_iter().flatten().any(|(_, job)| {
                    job["uses"]
                        .as_str()
                        .and_then(|uses| uses.strip_prefix("./"))
                        .map(|uses| uses.split('@').next().unwrap_or(uses))
                        == Some(path)
                })
            })
            .map(|(p, _)| p.clone())
            .collect();
        found.sort();
        found
    };
    let elsewhere = "other/repository/.github/workflows/ci.yml".to_string();
    if !called(file) {
        return format!("acme/shop/{file}");
    }
    let mut seen = vec![file.to_string()];
    let mut current = file.to_string();
    loop {
        let unseen = |of: &str, seen: &[String]| {
            callers(of)
                .into_iter()
                .find(|caller| !seen.contains(caller))
        };
        let Some(caller) = unseen(&current, &seen) else {
            return elsewhere;
        };
        seen.push(caller.clone());
        if called(&caller) && unseen(&caller, &seen).is_some() {
            current = caller;
            continue;
        }
        return if otherwise(&caller) || !called(&caller) {
            format!("acme/shop/{caller}")
        } else {
            elsewhere
        };
    }
}

/// The variables GitLab gives `job` of `file`: the file's, then the job's own, as text.
fn gitlab_variables(repo: &Path, file: &str, job: &str) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(repo.join(file)).expect("read");
    let doc: Value = serde_saphyr::from_str(&text).expect("YAML");
    let mut variables: Vec<(String, String)> = Vec::new();
    for scope in [&doc["variables"], &doc[job]["variables"]] {
        for (name, value) in scope.as_object().into_iter().flatten() {
            let value = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            variables.retain(|(had, _)| had != name);
            variables.push((name.clone(), value));
        }
    }
    variables
}

/// \[ADR 0049\] The env GitHub gives `job` of `file`: the file's, then the job's own, with the
/// expressions that tell its runs apart filled in, the same for both runs of a case. Any other
/// expression is CI's to fill in, which this can't, so it leaves it out.
fn github_env(repo: &Path, file: &str, job: &str) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(repo.join(file)).expect("read");
    let doc: Value = serde_saphyr::from_str(&text).expect("YAML");
    let mut env: Vec<(String, String)> = Vec::new();
    for scope in [&doc["env"], &doc["jobs"][job]["env"]] {
        for (name, value) in scope.as_object().into_iter().flatten() {
            let value = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            let words: String = value
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase();
            let value = match words.as_str() {
                "${{tojson(matrix)}}" => r#"{"variant":"first"}"#.to_string(),
                "${{tojson(inputs)}}" => r#"{"inputs":"first"}"#.to_string(),
                _ if value.contains("${{") => continue,
                _ => value,
            };
            env.retain(|(had, _)| had != name);
            env.push((name.clone(), value));
        }
    }
    env
}

/// The matrix of `job` of `file`, as its file lists it; `null` for a job with none.
fn github_matrix(repo: &Path, file: &str, job: &str) -> Value {
    let text = std::fs::read_to_string(repo.join(file)).expect("read");
    let doc: Value = serde_saphyr::from_str(&text).expect("YAML");
    doc["jobs"][job]["strategy"]["matrix"].clone()
}

/// \[ADR 0050\] `line` as the shell gets it, since GitHub fills in each `${{ … }}` first: a matrix
/// value is the first its job lists, and anything else a word that stands for what CI fills in,
/// the same for both runs of a case.
fn filled(line: &str, matrix: &Value) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("${{") {
        let Some(end) = rest[start..].find("}}").map(|end| start + end + 2) else {
            break;
        };
        out.push_str(&rest[..start]);
        let inner = rest[start + 3..end - 2].trim();
        let value = inner
            .strip_prefix("matrix.")
            .and_then(|key| matrix[key].as_array()?.first().cloned())
            .map(|value| match value {
                Value::String(text) => text,
                other => other.to_string(),
            });
        out.push_str(value.as_deref().unwrap_or("first"));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// A9 for one case: what each wrapped line and script does on its job's second run, beside what
/// the plan said. Each attempt starts from the checkout as CI's would, so a report the first run
/// wrote isn't code the second one sees.
#[cfg(unix)]
struct Job<'c> {
    case: &'c Case,
    bin: PathBuf,
    backend: String,
    /// The workflow GitHub names in the job: `<owner>/<name>/<file>`.
    workflow: String,
    file: String,
    job: String,
    /// On GitLab, the variables the job gets from its file, as GitLab passes them.
    gitlab: Option<Vec<(String, String)>>,
    /// \[ADR 0049\] On GitHub, the env the job gets from its file, as GitHub fills it in.
    github: Vec<(String, String)>,
    /// \[ADR 0050\] On GitHub, the job's matrix, whose first values fill in its lines.
    matrix: Value,
}

#[cfg(unix)]
impl Job<'_> {
    /// `line`, in `dir`, as a step of this job runs it: what the run said on stderr.
    fn step(&self, line: &str, dir: &str, script: Option<&str>) -> (bool, String) {
        git::run(&self.case.repo, &["checkout", "-q", "--", "."], None);
        git::run(&self.case.repo, &["clean", "-fdxq"], None);
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            [hushrun_testkit::testdata("init/bin"), self.bin.clone()]
                .into_iter()
                .chain(std::env::split_paths(&path)),
        )
        .expect("PATH");
        let mut cmd = self.case.command(Path::new("/bin/sh"));
        cmd.current_dir(self.case.repo.join(dir))
            .env("PATH", path)
            .env("CI", "true");
        match &self.gitlab {
            Some(variables) => {
                cmd.env("GITLAB_CI", "true")
                    .env("CI_JOB_NAME", &self.job)
                    .env("CI_NODE_TOTAL", "1")
                    .envs(variables.iter().map(|(name, value)| (name, value)));
            }
            None => {
                cmd.env("GITHUB_ACTIONS", "true")
                    .env("GITHUB_REPOSITORY", "acme/shop")
                    .env(
                        "GITHUB_WORKFLOW_REF",
                        format!("{}@refs/heads/main", self.workflow),
                    )
                    .env("GITHUB_JOB", &self.job)
                    .env("GITHUB_WORKSPACE", &self.case.repo)
                    .envs(self.github.iter().map(|(name, value)| (name, value)));
            }
        }
        cmd.env("HUSHRUN_BACKEND", &self.backend)
            .env(
                "HUSHRUN_TOKEN",
                hushrun_testkit::fake_results::FakeResults::TOKEN,
            )
            .args(["-c", &filled(line, &self.matrix)]);
        if let Some(script) = script {
            cmd.env("npm_lifecycle_event", script);
        }
        let out: Output = cmd.output().expect("sh");
        (out.status.success(), text(&out.stderr))
    }

    /// Runs `line` twice: the second run's decision is the plan's.
    fn agrees(&self, line: &str, dir: &str, script: Option<&str>, check: &str, plan: &Value) {
        let (passed, first) = self.step(line, dir, script);
        assert!(passed, "{} {}: {line}\n{first}", self.file, self.job);
        let (passed, second) = self.step(line, dir, script);
        assert!(passed, "{} {}: {line}\n{second}", self.file, self.job);
        let expected = match (plan["ci"].as_str(), plan["ciText"].as_str()) {
            (Some("skips"), _) => format!("hushrun: {check} skipped: "),
            (Some("runs"), Some(text)) => format!("hushrun: {check} running: {text}\n"),
            // The run says why in words the plan can't know: it runs.
            (Some("runs"), None) => format!("hushrun: {check} running"),
            other => panic!("no prediction for {line}: {other:?}"),
        };
        assert!(
            second.contains(&expected) && (plan["ci"] == "skips" || !second.contains("skipped")),
            "{} job {}: the plan said {expected:?}, and the run said:\n{second}",
            self.file,
            self.job
        );
    }
}

// covers: C-15, C-14
#[cfg(unix)]
#[test]
fn what_init_predicts_is_what_the_run_decides() {
    let fake = hushrun_testkit::fake_results::FakeResults::start();
    let mut agreed = Vec::new();
    for case_dir in cases() {
        let case = Case::new(&case_dir);
        let plan = case.plan();
        // The edits, as a person applies them once the org is known.
        for edit in plan["edits"].as_array().expect("edits") {
            let diff = edit["diff"]
                .as_str()
                .expect("a diff")
                .replace(PLACEHOLDER, &fake.url());
            git::run(&case.repo, &["apply", "-"], Some(diff.as_bytes()));
        }
        git::commit_all(&case.repo, "init's edits");
        let bin = case.root.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        std::os::unix::fs::symlink(assert_cmd::cargo::cargo_bin("hushrun"), bin.join("hushrun"))
            .expect("link");
        let package: Option<Value> = std::fs::read(case.repo.join("package.json"))
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).expect("package.json"));
        for job in plan["jobs"].as_array().expect("jobs") {
            let file = job["file"].as_str().expect("file").to_string();
            let id = job["job"].as_str().expect("job").to_string();
            let gitlab =
                (job["provider"] == "gitlab").then(|| gitlab_variables(&case.repo, &file, &id));
            let github = match gitlab {
                Some(_) => Vec::new(),
                None => github_env(&case.repo, &file, &id),
            };
            let matrix = match gitlab {
                Some(_) => Value::Null,
                None => github_matrix(&case.repo, &file, &id),
            };
            let runner = Job {
                case: &case,
                bin: bin.clone(),
                backend: fake.url(),
                workflow: match gitlab {
                    Some(_) => String::new(),
                    None => named_in(&case.repo, &file),
                },
                file,
                job: id,
                gitlab,
                github,
                matrix,
            };
            for command in job["commands"].as_array().expect("commands") {
                if command["status"] != "wrapped" {
                    continue;
                }
                let line = command["after"].as_str().expect("after");
                let dir = command["dir"].as_str().expect("dir");
                let check = command["check"].as_str().expect("check");
                runner.agrees(line, dir, None, check, command);
                agreed.push(format!("{}: {check}", name(&case_dir)));
            }
            for script in job["scripts"].as_array().expect("scripts") {
                let name_of = script["script"].as_str().expect("script");
                // \[ADR 0051\] A workspace package's script runs in its directory, from its own
                // package.json, as its package manager runs it.
                let (dir, manifest) = match script["package"].as_str() {
                    Some(path) => {
                        let dir = path.strip_suffix("/package.json").expect("a package.json");
                        let bytes = std::fs::read(case.repo.join(path)).expect("package.json");
                        let manifest: Value = serde_json::from_slice(&bytes).expect("package.json");
                        (dir.to_string(), manifest)
                    }
                    None => (String::new(), package.clone().expect("package.json")),
                };
                let line = manifest["scripts"][name_of].as_str().expect("the script");
                let check = script["check"].as_str().expect("check");
                runner.agrees(line, &dir, Some(name_of), check, script);
                let of = match dir.as_str() {
                    "" => String::new(),
                    dir => format!(" of {dir}"),
                };
                agreed.push(format!("{}: {check} (script{of})", name(&case_dir)));
            }
        }
    }
    // \[ADR 0048\] What the tools wrote is what the runs recorded (C-27).
    let summary = |check: &str, command: &str| {
        fake.recorded()
            .into_iter()
            .find(|result| {
                result["check"] == check
                    && result["command"]
                        .as_array()
                        .is_some_and(|words| words.iter().any(|word| word == command))
            })
            .map(|result| result["reports"].clone())
            .unwrap_or_else(|| panic!("no result of {check}"))
    };
    let tests = |reports: Value| reports["tests"]["total"].clone();
    assert_eq!(
        tests(summary("test", "--junitxml=test-results/junit.xml")),
        1
    );
    assert_eq!(tests(summary("test", "RspecJunitFormatter")), 1);
    let cobertura = summary("test", "--collect:XPlat");
    assert_eq!(
        cobertura["coverage"],
        json!({"format": "cobertura", "lines": {"covered": 1, "total": 2}})
    );
    assert_eq!(
        cobertura["files"],
        json!([{"path": "TestResults/run/coverage.cobertura.xml", "kind": "coverage", "format": "cobertura"}])
    );
    // The seeds hold a wrapped line that skips, a script that skips, scripts of reusable
    // workflows, called from here and from elsewhere, \[ADR 0047\] lines that run for their
    // reports, on GitHub and on GitLab, and a linter that skips beside them; \[ADR 0049\] matrix
    // jobs that skip per variant, one that runs for its variants since init can't write its env,
    // and called files that skip per call; \[ADR 0050\] lines whose words CI fills in, that set a
    // variable first, and that sit in a composite action, runners the table learned, and make
    // targets by their recipe, a linter's beside Codecov among them: each kind of decision is
    // checked.
    for expected in [
        "F01-coverage-flag-beside-codecov: test",
        "F11-no-coverage-is-no-report: test",
        "F14-lint-and-test-beside-codecov: ruff",
        "F15-gitlab-junit-report: test",
        "F02-matrix-runs-a-wrapped-script: test (script)",
        "F03-a-called-workflow-runs-pytest: test",
        "F03b-a-called-workflow-runs-a-wrapped-script: test (script)",
        "F06-pnpm-workspace-root-test-fans-out: lint (script)",
        // \[ADR 0051\] A workspace package's script, in its directory, under its scope.
        "F06-pnpm-workspace-root-test-fans-out: test (script of packages/a)",
        "F30-pnpm-workspace-a-depends-on-b: test (script of packages/a)",
        "F31-npm-workspaces-as-an-object: test (script of packages/web)",
        "F32-yarn-workspaces: test (script of packages/app)",
        "F33-bun-workspaces: test (script of apps/api)",
        "F34-a-script-using-a-shared-vitest-config: test (script of packages/a)",
        "F35-tsconfig-references-a-sibling: test (script of packages/app)",
        "F36-an-unreadable-package-graph: test (script of packages/web)",
        "F37-turborepo-passes-hushruns-variables: test (script of packages/b)",
        "F38-turbo-json-with-comments-and-a-list: lint (script of packages/ui)",
        "F09-matrix-with-a-test-reporter: test (script)",
        "F17-matrix-from-an-expression: test",
        "F19-flow-style-env: test",
        "F20-a-workflow-of-another-repository: test",
        "F21-one-called-file-two-calls: test",
        "F22-a-caller-matrix-feeds-inputs: test",
        "F05-matrix-values-in-a-command: test",
        "F25-a-variable-set-in-front-of-go-test: test-test",
        "F04-a-composite-action-runs-pytest: test",
        "F23-a-composite-action-two-jobs-use: test",
        "F26-make-targets-by-their-recipe: test-ci",
        "F26-make-targets-by-their-recipe: lint-ci",
        "F26-make-targets-by-their-recipe: ui-test",
        "F27-more-runners: test",
        "F27-more-runners: build",
        "F27-more-runners: rails-test",
    ] {
        assert!(
            agreed.iter().any(|a| a == expected),
            "{expected}: {agreed:?}"
        );
    }
}
