//! B-75: what CI's build job runs for a check, read from CI's own files at the commit a bake
//! builds. For a repository whose CI runs `hushrun run NAME -- COMMAND` (what init writes, with
//! no `run` in hushrun.yaml), CI's file is the one definition of a check's command, its folder
//! and the variables its job gives it, as it is for `hushrun ready` ([`super::wrapped_runs`]): a
//! bake reads them there each time, so an edit to CI's file is what the next bake builds, and
//! nothing init copied can disagree with it.
//!
//! The values CI fills in are filled in the same way everywhere: a job's matrix stands for one
//! entry, its primary one ([`primary_entry`]), and `${{ matrix.KEY }}` and `${{ env.NAME }}` take
//! that entry's and the variables' values ([`resolve`]). `init --bake` fills the build job's
//! setup steps it copies with the same function, so the bake job's toolchain is the build's.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::scripts;
use super::workflows::{Host, Job, Step, Workflow};

/// A check as CI's build job runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Replay {
    pub(crate) check: String,
    /// COMMAND as a shell runs it, what CI fills in filled in.
    pub(crate) line: String,
    /// The folder it starts in, relative to the repository root, `""` at the root.
    pub(crate) dir: String,
    /// The variables CI's file gives it, in the order set: on GitHub the workflow's, the job's
    /// and the step's `env`; on GitLab the file's and the job's `variables`, and the job's
    /// primary `parallel: matrix:` entry.
    pub(crate) env: Vec<(String, String)>,
    /// Variables of CI's file left out, each with why: a value only CI knows, such as a secret.
    pub(crate) left_out: Vec<(String, String)>,
    /// Where it was read: the file and the job.
    pub(crate) from: String,
    /// The shell CI's job runs it with.
    pub(crate) shell: Shell,
}

/// The shell a CI job runs a step's commands with, which a bake's build runs COMMAND with (its
/// expansions, `$(…)`, `{a,b}`, `[[`, are the shell's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shell {
    /// GitHub's: a program and its arguments, `{0}` the file the commands are written to. A
    /// step's `shell:`, else its job's `defaults.run.shell`, else the workflow's, else GitHub's
    /// own `bash -e {0}`.
    File(Vec<String>),
    /// `sh -c` or `bash -c` with the commands: hushrun.yaml's `run`, as `hushrun run` runs it.
    Inline(String),
    /// GitLab's: bash where the job's image has it, else sh, as its runner picks.
    Image,
}

/// B-75: the shell a GitHub job runs its step `step` with, as GitHub picks it: the step's
/// `shell:`, else the job's `defaults.run.shell`, else the workflow's, else `bash -e {0}`. `bash`
/// and `sh` are GitHub's own templates for them; a template of its own runs when it is bash's or
/// sh's. `Err` for any other (pwsh, python, cmd): a bake runs COMMAND with bash or sh alone.
fn github_shell(file: &Value, job: &str, step: usize) -> Result<Shell, String> {
    let job_value = &file["jobs"][job];
    let named = job_value["steps"]
        .get(step)
        .and_then(|s| s.get("shell"))
        .or_else(|| {
            job_value
                .get("defaults")
                .and_then(|d| d["run"].get("shell"))
        })
        .or_else(|| file.get("defaults").and_then(|d| d["run"].get("shell")));
    let Some(named) = named else {
        return Ok(Shell::File(vec!["bash".into(), "-e".into(), "{0}".into()]));
    };
    let text = text_of(named).unwrap_or_default();
    let words: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    match words.as_slice() {
        [one] if one == "bash" => Ok(Shell::File(
            ["bash", "--noprofile", "--norc", "-eo", "pipefail", "{0}"]
                .map(str::to_string)
                .to_vec(),
        )),
        [one] if one == "sh" => Ok(Shell::File(
            ["sh", "-e", "{0}"].map(str::to_string).to_vec(),
        )),
        [program, ..]
            if words.iter().any(|w| w == "{0}")
                && matches!(program.rsplit('/').next(), Some("bash" | "sh")) =>
        {
            Ok(Shell::File(words))
        }
        _ => Err(format!(
            "its step runs under `shell: {text}`, and a bake runs a check's command with bash or sh"
        )),
    }
}

/// Which `${{ … }}`s a text may keep as written, when [`resolve`] fills in the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    /// None: the text runs outside CI (a bake's build), where no other expression has a value.
    Nothing,
    /// Those that mean the same in another job of the same repository and commit: `secrets.*`,
    /// `vars.*`, `runner.*` and `github.*` but its event (a copied step of the bake job).
    SameInAnotherJob,
}

/// The values [`resolve`] fills in: a matrix entry's, and variables'.
#[derive(Debug, Clone, Default)]
pub(crate) struct Values {
    pub(crate) matrix: Option<Map<String, Value>>,
    pub(crate) env: BTreeMap<String, String>,
    /// B-75: the environment of the job a bake runs in, a job of the same repository on the same
    /// kind of runner, less the credentials its build never gets: what CI's own variables
    /// (`${{ github.workspace }}`, GitLab's `$CI_PROJECT_DIR`) are filled in from. `None` where
    /// nothing runs (init).
    pub(crate) runner: Option<BTreeMap<String, String>>,
}

/// B-75: the runner's variable GitHub's expression `inner` reads the same value as, in another
/// job of the same repository and commit on the same kind of runner: the workspace, the
/// repository and its ref, the runner's temporary folder, tool cache, system and architecture.
fn runner_variable(inner: &str) -> Option<&'static str> {
    Some(match inner {
        "github.workspace" => "GITHUB_WORKSPACE",
        "github.repository" => "GITHUB_REPOSITORY",
        "github.repository_owner" => "GITHUB_REPOSITORY_OWNER",
        "github.server_url" => "GITHUB_SERVER_URL",
        "github.sha" => "GITHUB_SHA",
        "github.ref" => "GITHUB_REF",
        "github.ref_name" => "GITHUB_REF_NAME",
        "runner.temp" => "RUNNER_TEMP",
        "runner.tool_cache" => "RUNNER_TOOL_CACHE",
        "runner.os" => "RUNNER_OS",
        "runner.arch" => "RUNNER_ARCH",
        _ => return None,
    })
}

/// A value of YAML as CI writes it into text: a string as it is, a number or a boolean in
/// words, nothing for null; a list or a mapping isn't text (`None`).
pub(crate) fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some(String::new()),
        _ => None,
    }
}

/// `text` with each `${{ matrix.KEY }}` and `${{ env.NAME }}` filled in from `values`, and every
/// other `${{ … }}` kept as written where `keep` allows. `Err` names the first expression that
/// can be neither: one of a key or a variable that has no value, or of anything else.
pub(crate) fn resolve(text: &str, values: &Values, keep: Keep) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let end = after
            .find("}}")
            .ok_or_else(|| format!("`{}` doesn't close", &rest[start..]))?;
        let inner = after[..end].trim();
        let written = &rest[start..start + 3 + end + 2];
        let filled = if let Some(key) = inner.strip_prefix("matrix.") {
            values
                .matrix
                .as_ref()
                .and_then(|m| m.get(key))
                .and_then(text_of)
        } else if let Some(name) = inner.strip_prefix("env.") {
            values
                .env
                .get(name)
                .or_else(|| values.runner.as_ref()?.get(name))
                .cloned()
        } else {
            runner_variable(inner).and_then(|name| values.runner.as_ref()?.get(name).cloned())
        };
        match filled {
            Some(value) => out.push_str(&value),
            None if keep == Keep::SameInAnotherJob && same_in_another_job(inner) => {
                out.push_str(written);
            }
            None => return Err(format!("`{written}`")),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Whether the expression `inner` means the same in another job of the same repository, run for
/// the same commit: a secret, a variable of the repository, the runner's, and GitHub's own words
/// but those of the event (a `workflow_run` isn't a `push`).
fn same_in_another_job(inner: &str) -> bool {
    let plain = inner
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    plain
        && (inner.starts_with("secrets.")
            || inner.starts_with("vars.")
            || inner.starts_with("runner.")
            || (inner.starts_with("github.") && !inner.starts_with("github.event")))
}

/// Whether a `runs-on` names a Linux x64 runner, the platform a bake job runs on.
fn linux_x64(runs_on: &str) -> bool {
    let on = runs_on.to_ascii_lowercase();
    (on.contains("ubuntu") || on.contains("linux"))
        && !on.contains("arm")
        && !on.contains("aarch64")
}

/// A `runs-on` as text: a label, labels, or a group's labels.
fn runs_on_text(value: &Value) -> String {
    match value {
        Value::Array(labels) => labels
            .iter()
            .filter_map(text_of)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(map) => map.get("labels").map(runs_on_text).unwrap_or_default(),
        other => text_of(other).unwrap_or_default(),
    }
}

/// The most combinations a matrix is expanded to before its first Linux one is given up on.
const MOST_COMBINATIONS: usize = 256;

/// B-75: the entry of a GitHub job's `strategy.matrix` a bake stands for, its primary one: the
/// first combination GitHub makes (each key's values in the order written, the first key
/// slowest, less `exclude`, with what `include` adds) whose `runs-on` is a Linux x64 runner,
/// else the first. `Ok(None)`: the job has no matrix. `Err`: values only CI knows (an
/// expression), and why.
pub(crate) fn primary_entry(job: &Value) -> Result<Option<Map<String, Value>>, String> {
    let Some(matrix) = job.get("strategy").and_then(|s| s.get("matrix")) else {
        return Ok(None);
    };
    let Some(matrix) = matrix.as_object() else {
        return Err("its matrix is an expression only CI fills in".into());
    };
    let mut axes: Vec<(&String, Vec<&Value>)> = Vec::new();
    for (key, values) in matrix {
        if key == "include" || key == "exclude" {
            continue;
        }
        let values: Vec<&Value> = match values {
            Value::Array(values) => values.iter().collect(),
            Value::String(text) if text.contains("${{") => {
                return Err(format!(
                    "its matrix's {key} is an expression only CI fills in"
                ));
            }
            other => vec![other],
        };
        if values
            .iter()
            .any(|v| v.as_str().is_some_and(|t| t.contains("${{")))
        {
            return Err(format!(
                "its matrix's {key} holds an expression only CI fills in"
            ));
        }
        axes.push((key, values));
    }
    let entries = |name: &str| -> Result<Vec<Map<String, Value>>, String> {
        match matrix.get(name) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_object()
                        .cloned()
                        .ok_or_else(|| format!("its matrix's {name} lists something not a mapping"))
                })
                .collect(),
            Some(_) => Err(format!(
                "its matrix's {name} is an expression only CI fills in"
            )),
        }
    };
    let include = entries("include")?;
    let exclude = entries("exclude")?;
    // Every combination of the keys' values, the first key slowest.
    let mut combinations: Vec<Map<String, Value>> = vec![Map::new()];
    if !axes.is_empty() {
        for (key, values) in &axes {
            let mut next = Vec::new();
            for combination in &combinations {
                for value in values {
                    if next.len() >= MOST_COMBINATIONS {
                        break;
                    }
                    let mut more = combination.clone();
                    more.insert((*key).clone(), (*value).clone());
                    next.push(more);
                }
            }
            combinations = next;
        }
    }
    let matches =
        |combination: &Map<String, Value>, entry: &Map<String, Value>, keys_only: bool| {
            entry.iter().all(|(key, value)| {
                if keys_only && !axes.iter().any(|(axis, _)| *axis == key) {
                    return true;
                }
                combination.get(key) == Some(value)
            })
        };
    combinations.retain(|c| !exclude.iter().any(|entry| matches(c, entry, false)));
    if axes.is_empty() {
        combinations.clear();
    }
    // `include` extends the combinations the keys made, never one an earlier `include` added.
    let made = combinations.len();
    for entry in &include {
        let mut matched = false;
        for combination in combinations.iter_mut().take(made) {
            if matches(combination, entry, true) {
                matched = true;
                for (key, value) in entry {
                    if !axes.iter().any(|(axis, _)| *axis == key) {
                        combination.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        if !matched {
            combinations.push(entry.clone());
        }
    }
    let runs_on = job.get("runs-on").map(runs_on_text).unwrap_or_default();
    let linux = combinations.iter().find(|combination| {
        let values = Values {
            matrix: Some((*combination).clone()),
            ..Values::default()
        };
        resolve(&runs_on, &values, Keep::Nothing).is_ok_and(|on| linux_x64(&on))
    });
    Ok(linux.or(combinations.first()).cloned())
}

/// B-75: a GitLab job's primary `parallel: matrix:` entry: the first entry, each variable's first
/// value. `None`: no matrix.
fn gitlab_entry(job: &Value) -> Option<Vec<(String, String)>> {
    let first = job
        .get("parallel")?
        .get("matrix")?
        .as_array()?
        .first()?
        .as_object()?;
    Some(
        first
            .iter()
            .filter_map(|(name, values)| {
                let value = match values {
                    Value::Array(values) => values.first()?,
                    other => other,
                };
                Some((name.clone(), text_of(value)?))
            })
            .collect(),
    )
}

/// `text` with GitLab's `$NAME` and `${NAME}` filled in as GitLab's runner fills a variable's
/// value: from the variables set before it (`known`), else the job's own (`runner`: the bake's
/// job, of the same project, with the same predefined variables), `$$` a `$`. `Err` names the
/// first variable neither holds: one only the build job had, or a credential the build never
/// gets. With no `runner` (nothing runs), only `known` counts.
fn gitlab_expanded(
    text: &str,
    known: &BTreeMap<String, String>,
    runner: Option<&BTreeMap<String, String>>,
) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(more) = after.strip_prefix('$') {
            out.push('$');
            rest = more;
            continue;
        }
        let (name, len) = if let Some(inner) = after.strip_prefix('{') {
            match inner.find('}') {
                Some(end) => (&inner[..end], end + 2),
                None => ("", 0),
            }
        } else {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..end], end)
        };
        let named = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !named {
            out.push('$');
            rest = after;
            continue;
        }
        let value = known
            .get(name)
            .or_else(|| runner.and_then(|r| r.get(name)))
            .ok_or_else(|| format!("${name}"))?;
        out.push_str(value);
        rest = &after[len..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The variables one level of a CI file sets (`env` or `variables`), resolved in turn onto
/// `values.env`; each that can't be is left out, with why, and undoes what a level above set.
fn layer(
    level: Option<&Value>,
    host: Host,
    values: &mut Values,
    left_out: &mut Vec<(String, String)>,
    what: &str,
) {
    let Some(level) = level else {
        return;
    };
    let Some(map) = level.as_object() else {
        left_out.push((
            what.to_string(),
            "it is an expression only CI fills in".into(),
        ));
        return;
    };
    for (name, value) in map {
        // GitLab's `NAME: {value: …, description: …, expand: false}`: one that says
        // `expand: false` is taken as written.
        let (value, expands) = match (host, value) {
            (Host::GitLab, Value::Object(entry)) => (
                entry.get("value").unwrap_or(&Value::Null),
                entry.get("expand") != Some(&Value::Bool(false)),
            ),
            (_, value) => (value, true),
        };
        let Some(text) = text_of(value) else {
            values.env.remove(name);
            left_out.push((name.clone(), "its value isn't text".into()));
            continue;
        };
        let filled = match host {
            Host::GitHub => resolve(&text, values, Keep::Nothing),
            Host::GitLab if !expands => Ok(text),
            Host::GitLab => gitlab_expanded(&text, &values.env, values.runner.as_ref())
                .map_err(|name| format!("`{name}`")),
        };
        match filled {
            Ok(text) => {
                values.env.insert(name.clone(), text);
            }
            Err(expression) => {
                values.env.remove(name);
                left_out.push((
                    name.clone(),
                    format!(
                        "it takes {expression}, which only the build job's run fills in (or a credential the build never gets)"
                    ),
                ));
            }
        }
    }
}

/// The variables, in the order a job sets them, from `values` and the names set (`order`).
fn ordered(values: &Values, order: &[String]) -> Vec<(String, String)> {
    let mut seen = std::collections::BTreeSet::new();
    order
        .iter()
        .filter(|name| seen.insert((*name).clone()))
        .filter_map(|name| Some((name.clone(), values.env.get(name)?.clone())))
        .collect()
}

/// The names one level sets, for [`ordered`].
fn names_of(level: Option<&Value>) -> Vec<String> {
    level
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

/// What a job of a CI file gives a step: the values CI fills in, the variables left out (and
/// why), and the variables' names in the order they are set.
pub(crate) struct JobValues {
    pub(crate) values: Values,
    pub(crate) left_out: Vec<(String, String)>,
    pub(crate) order: Vec<String>,
}

/// The values a GitHub job of `file` (read as YAML) gives its step `step`: its primary matrix
/// entry, and the workflow's, the job's and the step's `env`, each resolved on those above.
/// `Err`: the matrix is only CI's to know.
pub(crate) fn github_values(
    file: &Value,
    job: &str,
    step: Option<usize>,
    runner: Option<&BTreeMap<String, String>>,
) -> Result<JobValues, String> {
    let job_value = &file["jobs"][job];
    let mut values = Values {
        matrix: primary_entry(job_value)?,
        env: BTreeMap::new(),
        runner: runner.cloned(),
    };
    let mut left_out = Vec::new();
    let step_env = step
        .and_then(|i| job_value["steps"].get(i))
        .and_then(|s| s.get("env"));
    let mut order = names_of(file.get("env"));
    order.extend(names_of(job_value.get("env")));
    order.extend(names_of(step_env));
    layer(
        file.get("env"),
        Host::GitHub,
        &mut values,
        &mut left_out,
        "the workflow's env",
    );
    layer(
        job_value.get("env"),
        Host::GitHub,
        &mut values,
        &mut left_out,
        "the job's env",
    );
    layer(
        step_env,
        Host::GitHub,
        &mut values,
        &mut left_out,
        "the step's env",
    );
    Ok(JobValues {
        values,
        left_out,
        order,
    })
}

/// One check as one job's step runs it.
fn replay_of(
    file: &Workflow,
    yaml: Option<&Value>,
    job: &Job,
    step: &Step,
    found: &scripts::LineRun,
    runner: Option<&BTreeMap<String, String>>,
) -> Result<Replay, String> {
    let run = found.run.as_ref().map_err(|why| (*why).to_string())?;
    let check = run.name.clone().ok_or("it names no check")?;
    let dir = match (&step.dir, &found.moved) {
        (Err(why), _) => return Err(why.clone()),
        (_, None) => return Err("a `cd` before it moves it where hushrun can't follow".into()),
        (Ok(dir), Some(moved)) => [dir.as_str(), moved.as_str()]
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("/"),
    };
    let written = found
        .written
        .clone()
        .unwrap_or_else(|| crate::cmd::bake_build::shell_words(&run.command));
    let from = format!("{}, job {}", file.path, job.id);
    let yaml = yaml.ok_or("CI's file doesn't read as YAML")?;
    let (values, left_out, env) = match file.host {
        Host::GitHub => {
            let JobValues {
                values,
                left_out,
                order,
            } = github_values(yaml, &job.id, Some(step.index), runner)?;
            let env = ordered(&values, &order);
            (values, left_out, env)
        }
        Host::GitLab => {
            let mut values = Values {
                runner: runner.cloned(),
                ..Values::default()
            };
            let mut left_out = Vec::new();
            let job_value = &yaml[job.id.as_str()];
            let mut order = names_of(yaml.get("variables"));
            order.extend(names_of(job_value.get("variables")));
            layer(
                yaml.get("variables"),
                Host::GitLab,
                &mut values,
                &mut left_out,
                "the file's variables",
            );
            layer(
                job_value.get("variables"),
                Host::GitLab,
                &mut values,
                &mut left_out,
                "the job's variables",
            );
            for (name, value) in gitlab_entry(job_value).unwrap_or_default() {
                order.push(name.clone());
                values.env.insert(name, value);
            }
            let env = ordered(&values, &order);
            (values, left_out, env)
        }
    };
    let line = match file.host {
        Host::GitHub => resolve(&written, &values, Keep::Nothing).map_err(|expression| {
            format!("its command takes {expression}, which only CI fills in")
        })?,
        // GitLab fills in nothing before the shell: its `$NAME`s are the shell's, from `env`.
        Host::GitLab => written,
    };
    let shell = match file.host {
        Host::GitHub => github_shell(yaml, &job.id, step.index)?,
        Host::GitLab => Shell::Image,
    };
    Ok(Replay {
        check,
        line,
        dir,
        env,
        left_out,
        from,
        shell,
    })
}

/// B-75: each check CI's files run through `hushrun run NAME -- COMMAND` for a push to `branch`,
/// as its job runs it (the files that run for it, then those that may), or why it can't be read:
/// the first job on a Linux x64 runner that runs it, else the first that does. A job whose
/// `runs-on` is an expression that, for its primary matrix entry, isn't a Linux x64 runner is
/// never replayed. With `checks`, those alone. CI's own variables are filled in from `runner`,
/// the bake job's environment ([`Values::runner`]).
pub(crate) fn replays(
    pipelines: &[Workflow],
    branch: Option<&str>,
    checks: &[String],
    runner: Option<&BTreeMap<String, String>>,
) -> Vec<(String, Result<Replay, String>)> {
    let mut live = super::live(pipelines, branch);
    live.sort_by_key(|(_, perhaps)| perhaps.is_some());
    let mut found: Vec<(String, bool, Result<Replay, String>)> = Vec::new();
    for (file, _) in &live {
        let yaml: Option<Value> = super::variants::parse(&file.path, file.source().as_bytes());
        for job in &file.jobs {
            let linux = match file.host {
                Host::GitHub => {
                    let job_value = yaml.as_ref().map(|y| &y["jobs"][job.id.as_str()]);
                    let on = job_value
                        .and_then(|j| j.get("runs-on"))
                        .map(runs_on_text)
                        .unwrap_or_default();
                    if on.contains("${{") {
                        // What the expression names for the entry a bake stands for.
                        let values = Values {
                            matrix: job_value.and_then(|j| primary_entry(j).ok().flatten()),
                            ..Values::default()
                        };
                        if !resolve(&on, &values, Keep::Nothing).is_ok_and(|on| linux_x64(&on)) {
                            continue;
                        }
                        true
                    } else {
                        linux_x64(&on)
                    }
                }
                Host::GitLab => true,
            };
            if job.on_macos {
                continue;
            }
            for step in &job.steps {
                for run in scripts::line_runs(&step.commands) {
                    let Some(name) = run.run.as_ref().ok().and_then(|r| r.name.clone()) else {
                        continue;
                    };
                    if !checks.is_empty() && !checks.contains(&name) {
                        continue;
                    }
                    let replay = replay_of(file, yaml.as_ref(), job, step, &run, runner);
                    // A Linux x64 job's first, then one that reads; the first of equals.
                    let score = |linux: bool, replay: &Result<Replay, String>| {
                        u8::from(linux) * 2 + u8::from(replay.is_ok())
                    };
                    match found.iter_mut().find(|(n, ..)| *n == name) {
                        Some(seen) if score(linux, &replay) > score(seen.1, &seen.2) => {
                            *seen = (name, linux, replay);
                        }
                        Some(_) => {}
                        None => found.push((name, linux, replay)),
                    }
                }
            }
        }
    }
    found
        .into_iter()
        .map(|(name, _, replay)| (name, replay))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values(matrix: Value, env: &[(&str, &str)]) -> Values {
        Values {
            runner: None,
            matrix: matrix.as_object().cloned(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    // covers: B-75
    #[test]
    fn ci_s_values_are_filled_in_and_nothing_else_is_guessed() {
        let v = values(json!({"rust": "1.94", "n": 3}), &[("FLAGS", "-D warnings")]);
        assert_eq!(
            resolve(
                "cargo +${{ matrix.rust }} test -j${{matrix.n}} ${{ env.FLAGS }}",
                &v,
                Keep::Nothing
            )
            .unwrap(),
            "cargo +1.94 test -j3 -D warnings"
        );
        // A key, a variable or anything else with no value here is never left empty.
        assert!(resolve("${{ matrix.os }}", &v, Keep::Nothing).is_err());
        assert!(resolve("${{ env.NONE }}", &v, Keep::Nothing).is_err());
        assert!(resolve("${{ secrets.TOKEN }}", &v, Keep::Nothing).is_err());
        assert!(resolve("${{ steps.x.outputs.y }}", &v, Keep::SameInAnotherJob).is_err());
        assert!(resolve("${{ github.event.inputs.x }}", &v, Keep::SameInAnotherJob).is_err());
        assert!(resolve("${{ inputs.toolchain }}", &v, Keep::SameInAnotherJob).is_err());
        assert!(resolve("${{ fromJSON(env.X) }}", &v, Keep::SameInAnotherJob).is_err());
        // In a copied step, what means the same in the bake job stays as written.
        assert_eq!(
            resolve(
                "${{ secrets.GITHUB_TOKEN }} ${{ github.token }}",
                &v,
                Keep::SameInAnotherJob
            )
            .unwrap(),
            "${{ secrets.GITHUB_TOKEN }} ${{ github.token }}"
        );
    }

    // covers: B-75
    #[test]
    fn a_matrix_stands_for_its_first_linux_x64_entry() {
        let job = json!({
            "runs-on": "${{ matrix.os }}",
            "strategy": {"matrix": {
                "os": ["macos-latest", "ubuntu-latest", "ubuntu-24.04-arm"],
                "rust": ["stable", "beta"],
                "exclude": [{"os": "ubuntu-latest", "rust": "stable"}],
                "include": [{"rust": "beta", "features": "full"}],
            }},
        });
        let entry = primary_entry(&job).unwrap().unwrap();
        assert_eq!(entry["os"], "ubuntu-latest");
        assert_eq!(entry["rust"], "beta");
        assert_eq!(entry["features"], "full");
        // A fixed runner: the first combination.
        let job = json!({"runs-on": "ubuntu-latest", "strategy": {"matrix": {"rust": ["1.80", "stable"]}}});
        assert_eq!(primary_entry(&job).unwrap().unwrap()["rust"], "1.80");
        // None, and one only CI knows.
        assert_eq!(
            primary_entry(&json!({"runs-on": "ubuntu-latest"})).unwrap(),
            None
        );
        assert!(
            primary_entry(&json!({"strategy": {"matrix": "${{ fromJSON(needs.a.outputs.m) }}"}}))
                .is_err()
        );
        assert!(
            primary_entry(&json!({"strategy": {"matrix": {"v": "${{ fromJSON(x) }}"}}})).is_err()
        );
        // Only include: its first entry.
        let job = json!({"runs-on": "ubuntu-latest", "strategy": {"matrix": {"include": [{"t": "a"}, {"t": "b"}]}}});
        assert_eq!(primary_entry(&job).unwrap().unwrap()["t"], "a");
    }

    // covers: B-75
    // CI's own variables in CI's file reach the build with the values CI gives them, never as
    // written: GitLab's `$CI_PROJECT_DIR` (its Rust template's `CARGO_HOME:
    // $CI_PROJECT_DIR/.cargo`) and GitHub's `${{ github.workspace }}`, `${{ runner.temp }}` and
    // `${{ env.X }}` take the bake job's (a job of the same repository on the same kind of
    // runner). A variable with anything else, or one that job doesn't have, is left out, with
    // why; GitLab's `expand: false` and `$$` are taken as GitLab takes them.
    #[test]
    fn ci_s_own_variables_are_never_passed_as_written() {
        let runner: BTreeMap<String, String> = [
            ("CI_PROJECT_DIR", "/builds/acme/app"),
            ("GITHUB_WORKSPACE", "/home/runner/work/app/app"),
            ("RUNNER_TEMP", "/home/runner/work/_temp"),
            ("HOME", "/home/runner"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let gl = || {
            Workflow::parse_gitlab(
                "variables:\n  CARGO_HOME: $CI_PROJECT_DIR/.cargo\n  RAW:\n    value: $CI_PROJECT_DIR\n    expand: false\n  PRICE: $$5\ntest:\n  image: rust:1.94\n  variables:\n    CARGO_TARGET_DIR: ${CI_PROJECT_DIR}/target\n    TOKEN: $CI_JOB_TOKEN\n  script:\n    - hushrun run test -- cargo test\n".into(),
            )
        };
        let found = replays(&[gl()], Some("main"), &["test".into()], Some(&runner));
        let replay = found[0].1.as_ref().unwrap();
        assert_eq!(
            replay.env,
            [
                (
                    "CARGO_HOME".to_string(),
                    "/builds/acme/app/.cargo".to_string()
                ),
                ("PRICE".into(), "$5".into()),
                ("RAW".into(), "$CI_PROJECT_DIR".into()),
                ("CARGO_TARGET_DIR".into(), "/builds/acme/app/target".into()),
            ]
        );
        // The job token is a credential the bake's build never gets: its variable is left out.
        assert_eq!(replay.left_out.len(), 1, "{:?}", replay.left_out);
        assert_eq!(replay.left_out[0].0, "TOKEN");
        // Where nothing runs, a variable of CI's own is left out, never passed as written.
        let found = replays(&[gl()], Some("main"), &["test".into()], None);
        let replay = found[0].1.as_ref().unwrap();
        let literal: Vec<_> = replay
            .env
            .iter()
            .filter(|(name, value)| name != "RAW" && value.contains("CI_PROJECT_DIR"))
            .collect();
        assert!(literal.is_empty(), "passed as written: {literal:?}");
        // GitHub.
        let ci = Workflow::parse(
            ".github/workflows/ci.yml".into(),
            "name: CI\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    env:\n      RUSTFLAGS: --remap-path-prefix=${{ github.workspace }}=.\n      TMPDIR: ${{ runner.temp }}/t\n      CACHE: ${{ env.HOME }}/.cache\n      ID: ${{ github.run_id }}\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run test -- cargo test\n".into(),
        );
        let found = replays(&[ci], Some("main"), &[], Some(&runner));
        let replay = found[0].1.as_ref().unwrap();
        assert_eq!(
            replay.env,
            [
                ("CACHE".to_string(), "/home/runner/.cache".to_string()),
                (
                    "RUSTFLAGS".into(),
                    "--remap-path-prefix=/home/runner/work/app/app=.".into()
                ),
                ("TMPDIR".into(), "/home/runner/work/_temp/t".into()),
            ]
        );
        assert_eq!(replay.left_out.len(), 1, "{:?}", replay.left_out);
        assert_eq!(replay.left_out[0].0, "ID");
    }

    // covers: B-75
    // A job whose `runs-on` is an expression that names no Linux x64 runner for its primary
    // entry is never replayed on the bake's Linux runner; a GitHub step runs under bash -e unless
    // its step, job or workflow names another shell.
    #[test]
    fn a_job_is_replayed_only_on_linux_and_under_its_own_shell() {
        let ci = Workflow::parse(
            ".github/workflows/ci.yml".into(),
            "name: CI\non: push\njobs:\n  win:\n    runs-on: ${{ matrix.os }}\n    strategy:\n      matrix:\n        os: [windows-latest]\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run test -- cargo test\n  lint:\n    runs-on: ubuntu-latest\n    defaults:\n      run:\n        shell: sh\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run lint -- cargo clippy\n      - run: hushrun run fmt -- cargo fmt --check\n        shell: bash\n      - run: hushrun run docs -- cargo doc\n        shell: pwsh\n  plain:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run build -- cargo build\n".into(),
        );
        let found = replays(&[ci], Some("main"), &[], None);
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert!(!names.contains(&"test"), "{found:?}");
        let shell = |name: &str| {
            found
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, r)| r.as_ref().map(|r| r.shell.clone()))
                .unwrap()
        };
        let words = |w: &[&str]| Shell::File(w.iter().map(|s| s.to_string()).collect());
        assert_eq!(shell("lint"), Ok(words(&["sh", "-e", "{0}"])));
        assert_eq!(
            shell("fmt"),
            Ok(words(&[
                "bash",
                "--noprofile",
                "--norc",
                "-eo",
                "pipefail",
                "{0}"
            ]))
        );
        assert!(shell("docs").is_err());
        assert_eq!(shell("build"), Ok(words(&["bash", "-e", "{0}"])));
    }

    // covers: B-75
    #[test]
    fn a_check_s_command_and_variables_come_from_ci_s_file() {
        let ci = Workflow::parse(
            ".github/workflows/ci.yml".into(),
            "name: CI\non: push\nenv:\n  CARGO_TERM_COLOR: always\n  RUSTFLAGS: -D warnings\njobs:\n  test:\n    runs-on: ${{ matrix.os }}\n    strategy:\n      matrix:\n        os: [macos-latest, ubuntu-latest]\n        rust: [stable]\n    env:\n      TOOLCHAIN: ${{ matrix.rust }}\n      TOKEN: ${{ secrets.T }}\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run test -- cargo +${{ env.TOOLCHAIN }} test --locked \"$EXTRA\"\n        working-directory: crates\n        env:\n          EXTRA: --all-features\n".into(),
        );
        let found = replays(&[ci], Some("main"), &[], None);
        assert_eq!(found.len(), 1, "{found:?}");
        let (name, replay) = &found[0];
        assert_eq!(name, "test");
        let replay = replay.as_ref().unwrap();
        assert_eq!(replay.line, "cargo +stable test --locked \"$EXTRA\"");
        assert_eq!(replay.dir, "crates");
        assert_eq!(
            replay.env,
            [
                ("CARGO_TERM_COLOR".to_string(), "always".to_string()),
                ("RUSTFLAGS".into(), "-D warnings".into()),
                ("TOOLCHAIN".into(), "stable".into()),
                ("EXTRA".into(), "--all-features".into()),
            ]
        );
        assert_eq!(replay.left_out.len(), 1, "{:?}", replay.left_out);
        assert_eq!(replay.left_out[0].0, "TOKEN");
        // GitLab: the file's and the job's variables, and the matrix's first entry.
        let gl = Workflow::parse_gitlab(
            "variables:\n  BASE: /opt\ntest:\n  image: rust:1.94\n  variables:\n    TOOLS: $BASE/tools\n  parallel:\n    matrix:\n      - FEATURES: [a, b]\n  script:\n    - hushrun run test -- cargo test --features $FEATURES\n".into(),
        );
        let found = replays(&[gl], Some("main"), &["test".into()], None);
        let replay = found[0].1.as_ref().unwrap();
        assert_eq!(replay.line, "cargo test --features $FEATURES");
        assert_eq!(
            replay.env,
            [
                ("BASE".to_string(), "/opt".to_string()),
                ("TOOLS".into(), "/opt/tools".into()),
                ("FEATURES".into(), "a".into()),
            ]
        );
    }
}
