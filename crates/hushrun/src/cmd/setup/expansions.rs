//! \[ADR 0050\] Which words of a GitHub Actions line CI or the shell fills in, and whether each is
//! safe to record (C-15). A wrapped line's key is its words as the shell passes them to `hushrun
//! run`, so a value only CI knows, such as `${{ matrix.args }}`, gives each value a key of its own:
//! the key stays exact. A result keeps the command, though, so a secret among its words would go to
//! the backend. This reads the workflow file to tell what each word holds, and anything it can't
//! vouch for is left, with why (C-14).

use std::collections::BTreeSet;

use serde_json::Value;

use super::scripts::{Found, SUBSTITUTED};

/// Why a word the shell makes into others is left: a positional parameter, an arithmetic
/// expansion, or a variable written in a way hushrun doesn't read.
const CHANGES: &str = "the shell makes its words what they are";

/// Where a line of a GitHub Actions workflow runs: the file read as YAML, its job, and its step;
/// and the composite actions of this repository it runs inside.
pub struct Scope<'a> {
    pub file: &'a Value,
    pub job: &'a str,
    /// Its place among the job's steps, from 0: the line's own, or the one that uses the outermost
    /// action. `None` for a line that isn't a step's.
    pub step: Option<usize>,
    /// The actions it runs inside, innermost first, each read as YAML, with the place among its
    /// `runs.steps` of the step that runs the line, or uses the next action.
    pub actions: &'a [(&'a Value, usize)],
}

/// What a step reads where it has nothing.
static NOTHING: Value = Value::Null;

/// Why a line whose words hold a secret is left.
pub fn secret(shown: &str) -> String {
    format!("it passes a secret on its command line ({shown}), which a result would record")
}

/// Why a line whose words hold a variable hushrun can't vouch for is left.
pub fn unknown(name: &str) -> String {
    format!("an earlier step sets {name}, and hushrun can't tell it holds no secret")
}

/// Whether `found`, a command of step `step` of job `job` in `file`, the workflow read as YAML,
/// can be wrapped for what CI and the shell fill into its words, and for the variables it sets
/// first: every value safe to record, and the step a bash one, which `env` takes them through.
/// `actions` are the composite actions it runs inside, as [`Scope`] says, where `step` uses the
/// outermost. A file that doesn't read as YAML, or isn't GitHub's, says nothing, and the line is
/// left.
pub fn judged(
    found: &Found,
    file: Option<&Value>,
    job: &str,
    step: usize,
    actions: &[(&Value, usize)],
) -> Result<(), String> {
    let prefix = || format!("it sets {} first", found.words[0]);
    let Some(file) = file else {
        return Err(match found.prefixed {
            true => prefix(),
            false => CHANGES.to_string(),
        });
    };
    let scope = Scope {
        file,
        job,
        step: Some(step),
        actions,
    };
    verdict(&found.words, &scope)?;
    if found.prefixed && !bash(&scope) {
        return Err(prefix());
    }
    Ok(())
}

/// Whether every word CI or the shell fills in among `words` is safe to record, or why not.
pub fn verdict(words: &[String], scope: &Scope) -> Result<(), String> {
    for word in words {
        in_word(word, scope)?;
    }
    Ok(())
}

fn in_word(word: &str, scope: &Scope) -> Result<(), String> {
    let mut rest = word;
    while let Some(at) = rest.find(['$', '`']) {
        let after = &rest[at..];
        if after.starts_with('`') || after.starts_with("$(") {
            return Err(SUBSTITUTED.to_string());
        }
        if after.starts_with("${{") {
            let Some(end) = after.find("}}").map(|end| end + 2) else {
                return Err(CHANGES.to_string());
            };
            let expression = &after[..end];
            in_expression(expression, scope, 0, &mut BTreeSet::new(), expression)?;
            rest = &after[end..];
            continue;
        }
        if let Some(inner) = after.strip_prefix("${") {
            let Some(end) = inner.find('}') else {
                return Err(CHANGES.to_string());
            };
            let name = &inner[..end];
            if !is_name(name) {
                return Err(CHANGES.to_string());
            }
            let shown = &after[..end + 3];
            variable(name, scope, 0, &mut BTreeSet::new(), shown)?;
            rest = &after[end + 3..];
            continue;
        }
        let next = after[1..].chars().next();
        match next {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let name: String = after[1..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                let shown = format!("${name}");
                variable(&name, scope, 0, &mut BTreeSet::new(), &shown)?;
                rest = &after[1 + name.len()..];
            }
            // `$1`, `$@`, `$?` and the rest: what the shell has, not the file.
            Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => {
                return Err(CHANGES.to_string());
            }
            // A `$` the shell keeps as it is, as at the end of a word.
            _ => rest = &after[1..],
        }
    }
    Ok(())
}

/// Whether `expression`, a `${{ … }}` at `level`, is safe to record: no secret, no token, and
/// every variable of `env` and input it reads safe too. A level counts from the line's own
/// action, 0, out to its job, the number of actions. `shown` is how the command wrote what led
/// here.
fn in_expression(
    expression: &str,
    scope: &Scope,
    level: usize,
    seen: &mut BTreeSet<String>,
    shown: &str,
) -> Result<(), String> {
    let inner = expression.trim_start_matches("${{").trim_end_matches("}}");
    let lower = inner.to_ascii_lowercase();
    if mentions(&lower, "secrets") || token_of_github(&lower) {
        return Err(secret(shown));
    }
    for name in references(inner, "env") {
        variable(&name, scope, level, seen, shown)?;
    }
    for name in references(inner, "inputs") {
        input(&name, scope, level, seen, shown)?;
    }
    Ok(())
}

/// Whether the input `name`, read at `level`, is safe to record. An action's comes from its
/// default and what the step that uses it gives in `with:`, read where that step is. A workflow's
/// comes from a caller, whose `with:` GitHub doesn't let hold a secret, or a person, and from its
/// default, which may be the job's token.
fn input(
    name: &str,
    scope: &Scope,
    level: usize,
    seen: &mut BTreeSet<String>,
    shown: &str,
) -> Result<(), String> {
    let Some((action, _)) = scope.actions.get(level) else {
        let on = scope.file.get("on").or_else(|| scope.file.get("true"));
        for trigger in ["workflow_call", "workflow_dispatch"] {
            let declared = on
                .and_then(|on| on.get(trigger))
                .and_then(|trigger| entry(&trigger["inputs"], name));
            if let Some(declared) = declared {
                in_value(&declared["default"], scope, level, seen, shown)?;
            }
        }
        return Ok(());
    };
    if let Some(declared) = entry(&action["inputs"], name) {
        in_value(&declared["default"], scope, level, seen, shown)?;
    }
    let outer = level + 1;
    let uses = match scope.actions.get(outer) {
        Some((action, index)) => &action["runs"]["steps"][*index],
        None => match scope.step {
            Some(index) => &scope.file["jobs"][scope.job]["steps"][index],
            None => &NOTHING,
        },
    };
    match entry(&uses["with"], name) {
        Some(value) => in_value(value, scope, outer, seen, shown),
        None => Ok(()),
    }
}

/// Whether the variable `name`, read at `level`, is safe to record, by what sets it, nearest
/// first: in each action from there out, its step's `env`, then what its earlier steps write to
/// `$GITHUB_ENV`; then in the job the same, its own `env` and the workflow's; then the runner.
fn variable(
    name: &str,
    scope: &Scope,
    level: usize,
    seen: &mut BTreeSet<String>,
    shown: &str,
) -> Result<(), String> {
    if !seen.insert(format!("{level} {}", name.to_ascii_uppercase())) {
        return Err(unknown(name));
    }
    for (at, (action, index)) in scope.actions.iter().enumerate().skip(level) {
        let steps = &action["runs"]["steps"];
        if let Some(value) = entry(&steps[*index]["env"], name) {
            return in_value(value, scope, at, seen, shown);
        }
        match written_before(steps, Some(*index), name) {
            Written::Literal => return Ok(()),
            Written::Otherwise => return Err(unknown(name)),
            Written::No => {}
        }
    }
    let top = scope.actions.len();
    let job = &scope.file["jobs"][scope.job];
    let step = scope.step.map(|index| &job["steps"][index]);
    if let Some(value) = step.and_then(|step| entry(&step["env"], name)) {
        return in_value(value, scope, top, seen, shown);
    }
    match written_before(&job["steps"], scope.step, name) {
        Written::Literal => return Ok(()),
        Written::Otherwise => return Err(unknown(name)),
        Written::No => {}
    }
    for env in [&job["env"], &scope.file["env"]] {
        if let Some(value) = entry(env, name) {
            return in_value(value, scope, top, seen, shown);
        }
    }
    if name.eq_ignore_ascii_case("GITHUB_TOKEN") {
        return Err(secret(shown));
    }
    let upper = name.to_ascii_uppercase();
    if upper.starts_with("GITHUB_") || upper.starts_with("RUNNER_") || upper == "HOME" {
        return Ok(());
    }
    Err(unknown(name))
}

/// Whether `value`, an `env` value, an input's default, or what a step gives an input, read at
/// `level`, is safe to record: what GitHub fills into it is.
fn in_value(
    value: &Value,
    scope: &Scope,
    level: usize,
    seen: &mut BTreeSet<String>,
    shown: &str,
) -> Result<(), String> {
    let Some(text) = value.as_str() else {
        return Ok(());
    };
    let mut rest = text;
    while let Some(start) = rest.find("${{") {
        let Some(end) = rest[start..].find("}}").map(|end| start + end + 2) else {
            return Err(unknown(shown.trim_start_matches('$')));
        };
        in_expression(&rest[start..end], scope, level, seen, shown)?;
        rest = &rest[end..];
    }
    Ok(())
}

/// The value `env`, a mapping, gives `name`, the case of its letters aside, as GitHub reads names.
fn entry<'v>(env: &'v Value, name: &str) -> Option<&'v Value> {
    let env = env.as_object()?;
    env.get(name).or_else(|| {
        env.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

/// What `steps`, a job's or an action's, write of a variable to `$GITHUB_ENV` before step
/// `step`.
#[derive(Debug, PartialEq, Eq)]
enum Written {
    /// Nothing.
    No,
    /// Only literal values.
    Literal,
    /// A value only run time knows, or something hushrun doesn't read, which may set anything.
    Otherwise,
}

fn written_before(steps: &Value, step: Option<usize>, name: &str) -> Written {
    match last_written(steps, step, name) {
        Ok(None) => Written::No,
        Ok(Some(_)) => Written::Literal,
        Err(()) => Written::Otherwise,
    }
}

/// \[ADR 0055\] The literal value the last of `steps` before step `step` writes of a variable to
/// `$GITHUB_ENV`, `None` when none writes it; an error when one writes it, or may, in a way that
/// only run time knows.
fn last_written(steps: &Value, step: Option<usize>, name: &str) -> Result<Option<String>, ()> {
    let Some(step) = step else {
        return Ok(None);
    };
    let mut found = None;
    let steps = steps.as_array().map(Vec::as_slice).unwrap_or_default();
    for earlier in steps.iter().take(step) {
        let Some(run) = earlier["run"].as_str() else {
            continue;
        };
        for line in run.lines().filter(|line| line.contains("GITHUB_ENV")) {
            match env_write(line) {
                None => return Err(()),
                Some((written, _)) if !written.eq_ignore_ascii_case(name) => {}
                Some((_, Some(value))) => found = Some(value),
                Some((_, None)) => return Err(()),
            }
        }
    }
    Ok(found)
}

/// \[ADR 0055\] The variables a check reads from its environment to decide what it builds, as
/// `words`, its command, run it: what Cargo passes to the compiler and to rustdoc, but for
/// `cargo fmt`, which builds nothing. A run elsewhere without CI's values could pass where CI's
/// fails, so a wrap carries them into the line, through `env`.
pub fn read_from_env(words: &[&str]) -> &'static [&'static str] {
    let mut words = words
        .iter()
        .skip_while(|word| super::scripts::assignment(word));
    if words.next() != Some(&"cargo") {
        return &[];
    }
    match words.find(|word| !word.starts_with(['-', '+'])) {
        None | Some(&"fmt") => &[],
        Some(_) => &["RUSTFLAGS", "RUSTDOCFLAGS"],
    }
}

/// \[ADR 0055\] What CI sets the variable `name` to where `scope`'s line runs, found as
/// [`variable`] finds it, nearest first: `None` when nothing in the file sets it, else its value
/// as the file writes it, `${{ … }}` and all. A value an earlier step writes to `$GITHUB_ENV`
/// counts when it is a literal; any other write may set it to anything.
pub fn set_by_ci(name: &str, scope: &Scope) -> Result<Option<String>, String> {
    let at_run_time = || format!("an earlier step may set {name}, to what only run time knows");
    // What GitHub fills into a value is judged where the value is, as a line's words are.
    let safe = |value: &Value, level: usize| -> Result<Option<String>, String> {
        in_value(value, scope, level, &mut BTreeSet::new(), name)?;
        text_of(name, value).map(Some)
    };
    for (at, (action, index)) in scope.actions.iter().enumerate() {
        let steps = &action["runs"]["steps"];
        if let Some(value) = entry(&steps[*index]["env"], name) {
            return safe(value, at);
        }
        match last_written(steps, Some(*index), name) {
            Ok(None) => {}
            Ok(Some(value)) => return Ok(Some(value)),
            Err(()) => return Err(at_run_time()),
        }
    }
    let top = scope.actions.len();
    let job = &scope.file["jobs"][scope.job];
    let step = scope.step.map(|index| &job["steps"][index]);
    if let Some(value) = step.and_then(|step| entry(&step["env"], name)) {
        return safe(value, top);
    }
    match last_written(&job["steps"], scope.step, name) {
        Ok(None) => {}
        Ok(Some(value)) => return Ok(Some(value)),
        Err(()) => return Err(at_run_time()),
    }
    for env in [&job["env"], &scope.file["env"]] {
        if let Some(value) = entry(env, name) {
            return safe(value, top);
        }
    }
    Ok(None)
}

/// An `env` value as the process gets it.
fn text_of(name: &str, value: &Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(bool) => Ok(bool.to_string()),
        Value::Null => Ok(String::new()),
        _ => Err(format!("its env sets {name} to something other than text")),
    }
}

/// The variable a line writes to `$GITHUB_ENV`, and its value when that is a literal: `echo
/// "NAME=value" >> "$GITHUB_ENV"`, or `printf` of one. `None` for any other line that names the
/// file, which may write anything.
fn env_write(line: &str) -> Option<(String, Option<String>)> {
    let (left, right) = line.split_once(">>")?;
    let target = right.trim().trim_matches(['"', '\'']);
    if !matches!(target, "$GITHUB_ENV" | "${GITHUB_ENV}") {
        return None;
    }
    let mut words = left.split_whitespace().peekable();
    let program = words.next()?;
    if !matches!(program, "echo" | "printf") {
        return None;
    }
    while words.next_if(|word| word.starts_with('-')).is_some() {}
    let rest: Vec<&str> = words.collect();
    let [argument] = rest.as_slice() else {
        return None;
    };
    let quoted = (argument.starts_with('"') && argument.ends_with('"') && argument.len() >= 2)
        || (argument.starts_with('\'') && argument.ends_with('\'') && argument.len() >= 2);
    let text = match quoted {
        true => &argument[1..argument.len() - 1],
        false => argument,
    };
    let text = match program {
        "printf" => text.strip_suffix("\\n").unwrap_or(text),
        _ => text,
    };
    let (name, value) = text.split_once('=')?;
    if !is_name(name) || value.contains('<') {
        return None;
    }
    let filled_in = value.contains(['$', '`']);
    let formatted = program == "printf" && value.contains('%');
    let literal = !filled_in && !formatted;
    Some((name.to_string(), literal.then(|| value.to_string())))
}

/// Whether `lower`, an expression in lower case, reads the context `context`: its name as a word,
/// and not a part of another.
fn mentions(lower: &str, context: &str) -> bool {
    lower.match_indices(context).any(|(at, _)| {
        let before = lower[..at].chars().next_back();
        let after = lower[at + context.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

/// Whether `lower`, an expression in lower case, reads `github.token`.
fn token_of_github(lower: &str) -> bool {
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    ["github.token", "github['token']", "github[\"token\"]"]
        .iter()
        .any(|form| {
            compact.match_indices(form).any(|(at, _)| {
                let before = compact[..at].chars().next_back();
                let after = compact[at + form.len()..].chars().next();
                !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                    && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        })
}

/// The names `inner`, an expression, reads of the context `context`: `context.NAME`, or
/// `context['NAME']`.
fn references(inner: &str, context: &str) -> Vec<String> {
    let lower = inner.to_ascii_lowercase();
    let mut names = Vec::new();
    for (at, _) in lower.match_indices(context) {
        let before = lower[..at].chars().next_back();
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
            continue;
        }
        let rest = &inner[at + context.len()..];
        let name: String = if let Some(rest) = rest.strip_prefix('.') {
            rest.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect()
        } else if let Some(rest) = rest.strip_prefix("['").or_else(|| rest.strip_prefix("[\"")) {
            rest.chars()
                .take_while(|c| !matches!(c, '\'' | '"'))
                .collect()
        } else {
            continue;
        };
        if !name.is_empty() {
            names.push(name);
        }
    }
    names
}

/// Whether `name` is a shell variable's name.
fn is_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether a step runs in bash, or a POSIX shell, which reads `NAME=value` in front of a command:
/// its own `shell:`, else the job's and the workflow's `defaults.run.shell`, else GitHub's default,
/// bash, unless the job may run on Windows, where the default is pwsh. A composite action's step
/// names its shell itself.
pub fn bash(scope: &Scope) -> bool {
    if let Some((action, index)) = scope.actions.first() {
        let shell = action["runs"]["steps"][*index]["shell"].as_str();
        return shell
            .is_some_and(|shell| matches!(shell.split_whitespace().next(), Some("bash" | "sh")));
    }
    let job = &scope.file["jobs"][scope.job];
    let step = scope.step.map(|index| &job["steps"][index]);
    let shell = step
        .and_then(|step| step["shell"].as_str())
        .or_else(|| job["defaults"]["run"]["shell"].as_str())
        .or_else(|| scope.file["defaults"]["run"]["shell"].as_str());
    if let Some(shell) = shell {
        return matches!(shell.split_whitespace().next(), Some("bash" | "sh"));
    }
    !may_be_windows(job)
}

/// Whether `job`'s `runs-on` may be a Windows runner: one that names Windows, or none that names
/// another system, or what only run time knows.
fn may_be_windows(job: &Value) -> bool {
    let labels: Vec<String> = match &job["runs-on"] {
        Value::String(label) => match matrix_values(job, label) {
            Some(values) => values,
            None => vec![label.clone()],
        },
        Value::Array(labels) => labels
            .iter()
            .map(|label| label.as_str().unwrap_or("${{").to_string())
            .collect(),
        _ => return true,
    };
    if labels.is_empty() {
        return true;
    }
    let names = |label: &str, system: &[&str]| {
        let lower = label.to_ascii_lowercase();
        system.iter().any(|name| lower.contains(name))
    };
    let windows = labels
        .iter()
        .any(|label| label.contains("${{") || names(label, &["windows", "win-"]));
    let other = labels
        .iter()
        .any(|label| names(label, &["ubuntu", "linux", "macos", "mac-", "debian"]));
    match job["runs-on"] {
        // A list's labels are all one runner's: it is on Linux or a Mac when one says so.
        Value::Array(_) => windows || !other,
        // Each value of the matrix is a runner of its own: every one has to name another system.
        _ => {
            windows
                || !labels
                    .iter()
                    .all(|label| names(label, &["ubuntu", "linux", "macos", "mac-", "debian"]))
        }
    }
}

/// The values a `runs-on` of `${{ matrix.<key> }}` takes, when the job's matrix lists them all.
fn matrix_values(job: &Value, label: &str) -> Option<Vec<String>> {
    let inner = label.trim().strip_prefix("${{")?.strip_suffix("}}")?.trim();
    let key = inner.strip_prefix("matrix.")?;
    let matrix = &job["strategy"]["matrix"];
    let mut values: Vec<String> = match &matrix[key] {
        Value::Array(values) => values
            .iter()
            .map(|value| value.as_str().map(str::to_string))
            .collect::<Option<_>>()?,
        Value::String(value) if !value.contains("${{") => vec![value.clone()],
        Value::Null => Vec::new(),
        _ => return None,
    };
    if let Some(include) = matrix["include"].as_array() {
        for item in include {
            match &item[key] {
                Value::String(value) if !value.contains("${{") => values.push(value.clone()),
                Value::Null => {}
                _ => return None,
            }
        }
    } else if !matrix["include"].is_null() {
        return None;
    }
    (!values.is_empty()).then_some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The YAML `text`'s job `test`, with its step `step`.
    fn judge(text: &str, step: usize, words: &[&str]) -> Result<(), String> {
        let file = hushrun_core::yaml::parse("ci.yml", text.as_bytes()).expect("YAML");
        let scope = Scope {
            file: &file,
            job: "test",
            step: Some(step),
            actions: &[],
        };
        let words: Vec<String> = words.iter().map(ToString::to_string).collect();
        verdict(&words, &scope)
    }

    const JOB: &str = "on: push\nenv:\n  LEVEL: high\n  KEY: ${{ secrets.KEY }}\njobs:\n  test:\n    runs-on: ubuntu-latest\n    env:\n      ARGS: -q\n      FROM_TOP: ${{ env.LEVEL }}\n      TOKEN: ${{ secrets.NPM_TOKEN }}\n      SHA: ${{ github.sha }}\n    steps:\n      - run: echo \"WHEN=now\" >> \"$GITHUB_ENV\"\n      - run: echo \"WHERE=$(pwd)\" >> $GITHUB_ENV\n      - run: pytest\n        env:\n          MODE: fast\n";

    // covers: C-15
    #[test]
    fn what_ci_and_the_runner_fill_in_is_safe() {
        for word in [
            "${{ matrix.args }}",
            "${{ strategy.job-index }}",
            "${{ runner.os }}",
            "${{ inputs.level }}",
            "${{ github.sha }}",
            "${{ github.event.pull_request.number }}",
            "${{ vars.LEVEL }}",
            "${{ steps.pick.outputs.shard }}",
            "${{ needs.setup.outputs.matrix }}",
            "${{ env.ARGS }}",
            "${{ env.FROM_TOP }}",
            "--level=${{ env.LEVEL }}",
            "$ARGS",
            "${ARGS}",
            "$MODE",
            "$SHA",
            "$GITHUB_WORKSPACE/tests",
            "$RUNNER_TEMP",
            "$HOME/.cache",
            "$WHEN",
            "cost$",
            "${{ format('{0}-{1}', matrix.os, matrix.node) }}",
        ] {
            assert_eq!(judge(JOB, 2, &["pytest", word]), Ok(()), "{word}");
        }
    }

    // covers: C-15
    #[test]
    fn a_secret_or_what_may_hold_one_is_left_with_why() {
        let secret_of = |shown: &str| Err(secret(shown));
        assert_eq!(
            judge(
                JOB,
                2,
                &["npm", "publish", "--token", "${{ secrets.NPM_TOKEN }}"]
            ),
            secret_of("${{ secrets.NPM_TOKEN }}")
        );
        assert_eq!(
            judge(JOB, 2, &["x", "${{ github.token }}"]),
            secret_of("${{ github.token }}")
        );
        assert_eq!(
            judge(JOB, 2, &["x", "${{ toJSON(secrets) }}"]),
            secret_of("${{ toJSON(secrets) }}")
        );
        assert_eq!(judge(JOB, 2, &["x", "$TOKEN"]), secret_of("$TOKEN"));
        assert_eq!(
            judge(JOB, 2, &["x", "${{ env.TOKEN }}"]),
            secret_of("${{ env.TOKEN }}")
        );
        assert_eq!(judge(JOB, 2, &["x", "$KEY"]), secret_of("$KEY"));
        assert_eq!(
            judge(JOB, 2, &["x", "$GITHUB_TOKEN"]),
            secret_of("$GITHUB_TOKEN")
        );
        // Set from a command's output, by an earlier step, or nowhere at all.
        assert_eq!(judge(JOB, 2, &["x", "$WHERE"]), Err(unknown("WHERE")));
        assert_eq!(judge(JOB, 2, &["x", "$NOWHERE"]), Err(unknown("NOWHERE")));
        // What the shell or a command fills in.
        assert_eq!(
            judge(JOB, 2, &["x", "$(date)"]),
            Err(SUBSTITUTED.to_string())
        );
        assert_eq!(
            judge(JOB, 2, &["x", "`date`"]),
            Err(SUBSTITUTED.to_string())
        );
        for word in ["$1", "$@", "$?", "${ARGS:-x}", "${{ matrix.x"] {
            assert_eq!(
                judge(JOB, 2, &["x", word]),
                Err(CHANGES.to_string()),
                "{word}"
            );
        }
        // A secret names its context: words that only hold the letters don't.
        assert_eq!(judge(JOB, 2, &["x", "${{ vars.secrets_dir }}"]), Ok(()));
        assert_eq!(judge(JOB, 2, &["x", "${{ github.tokenizer }}"]), Ok(()));
    }

    // covers: C-15
    /// \[ADR 0055\] What CI sets a variable to where a line runs: the step's env, a literal an
    /// earlier step writes to `$GITHUB_ENV`, the job's, the workflow's, nearest first.
    #[test]
    fn what_ci_sets_a_variable_to_is_found_nearest_first() {
        let text = "on: push\nenv:\n  RUSTFLAGS: -D warnings\n  RUSTDOCFLAGS: --cfg docsrs\njobs:\n  test:\n    runs-on: ubuntu-latest\n    env:\n      RUSTDOCFLAGS: -D warnings\n      PROPTEST_CASES: 256\n    steps:\n      - run: echo \"CARGO_X=1\" >> \"$GITHUB_ENV\"\n      - run: cargo doc\n      - run: cargo test\n        env:\n          RUSTFLAGS: ${{ matrix.flags }}\n      - run: echo \"RUSTDOCFLAGS=$(cat flags)\" >> $GITHUB_ENV\n      - run: cargo doc\n";
        let file = hushrun_core::yaml::parse("ci.yml", text.as_bytes()).expect("YAML");
        let at = |step: usize| Scope {
            file: &file,
            job: "test",
            step: Some(step),
            actions: &[],
        };
        let set = |step: usize, name: &str| set_by_ci(name, &at(step));
        assert_eq!(set(1, "RUSTDOCFLAGS"), Ok(Some("-D warnings".into())));
        assert_eq!(set(1, "RUSTFLAGS"), Ok(Some("-D warnings".into())));
        assert_eq!(set(1, "CARGO_X"), Ok(Some("1".into())));
        assert_eq!(set(1, "PROPTEST_CASES"), Ok(Some("256".into())));
        assert_eq!(set(1, "NOTHING"), Ok(None));
        assert_eq!(set(2, "RUSTFLAGS"), Ok(Some("${{ matrix.flags }}".into())));
        assert!(
            set(4, "RUSTDOCFLAGS")
                .unwrap_err()
                .contains("only run time knows")
        );
        // A value a secret fills in would go into the line, and so to the backend.
        let secret = text.replace("RUSTFLAGS: -D warnings", "RUSTFLAGS: ${{ secrets.FLAGS }}");
        let file = hushrun_core::yaml::parse("ci.yml", secret.as_bytes()).expect("YAML");
        let scope = Scope {
            file: &file,
            job: "test",
            step: Some(1),
            actions: &[],
        };
        assert_eq!(
            set_by_ci("RUSTFLAGS", &scope),
            Err(super::secret("RUSTFLAGS"))
        );
        assert_eq!(
            set_by_ci("RUSTDOCFLAGS", &scope),
            Ok(Some("-D warnings".into()))
        );
        // What Cargo reads to decide what it builds, but for what builds nothing.
        assert_eq!(
            read_from_env(&["cargo", "doc", "--no-deps"]),
            ["RUSTFLAGS", "RUSTDOCFLAGS"]
        );
        assert_eq!(
            read_from_env(&["A=1", "cargo", "+nightly", "test"]),
            ["RUSTFLAGS", "RUSTDOCFLAGS"]
        );
        assert!(read_from_env(&["cargo", "fmt", "--check"]).is_empty());
        assert!(read_from_env(&["pytest"]).is_empty());
    }

    // covers: C-15
    #[test]
    fn what_an_earlier_step_writes_to_github_env_is_read_line_by_line() {
        assert_eq!(
            env_write("echo \"A=1\" >> \"$GITHUB_ENV\""),
            Some(("A".into(), Some("1".into())))
        );
        assert_eq!(
            env_write("echo A=1 >> $GITHUB_ENV"),
            Some(("A".into(), Some("1".into())))
        );
        assert_eq!(
            env_write("printf 'A=1\\n' >> ${GITHUB_ENV}"),
            Some(("A".into(), Some("1".into())))
        );
        assert_eq!(
            env_write("echo \"A=$(date)\" >> $GITHUB_ENV"),
            Some(("A".into(), None))
        );
        assert_eq!(
            env_write("echo \"A=$B\" >> $GITHUB_ENV"),
            Some(("A".into(), None))
        );
        // Anything else may write anything.
        for line in [
            "cat >> $GITHUB_ENV <<EOF",
            "{ echo A=1; echo B=2; } >> $GITHUB_ENV",
            "echo A=1 | tee -a $GITHUB_ENV",
            "env >> $GITHUB_ENV",
        ] {
            assert_eq!(env_write(line), None, "{line}");
        }
        let job = "jobs:\n  test:\n    runs-on: ubuntu-latest\n    env:\n      A: plain\n    steps:\n      - run: cat >> $GITHUB_ENV <<EOF\n      - run: pytest\n";
        assert_eq!(judge(job, 1, &["pytest", "$A"]), Err(unknown("A")));
        // A step's own env comes first.
        let own = job.replace(
            "      - run: pytest\n",
            "      - run: pytest\n        env:\n          A: mine\n",
        );
        assert_eq!(judge(&own, 1, &["pytest", "$A"]), Ok(()));
    }

    // covers: C-15
    #[test]
    fn variables_set_first_need_a_bash_step() {
        let scope = |text: &str| {
            let file = hushrun_core::yaml::parse("ci.yml", text.as_bytes()).expect("YAML");
            bash(&Scope {
                file: &file,
                job: "test",
                step: Some(0),
                actions: &[],
            })
        };
        let on = |runs_on: &str, more: &str| {
            format!(
                "jobs:\n  test:\n    runs-on: {runs_on}\n{more}    steps:\n      - run: GOARCH=386 go test ./...\n"
            )
        };
        assert!(scope(&on("ubuntu-latest", "")));
        assert!(scope(&on("macos-14", "")));
        assert!(scope(&on("[self-hosted, linux]", "")));
        assert!(!scope(&on("windows-latest", "")));
        assert!(!scope(&on("[self-hosted]", "")));
        assert!(!scope(&on("large-runner", "")));
        let matrix = "    strategy:\n      matrix:\n        os: [ubuntu-latest, macos-latest]\n";
        assert!(scope(&on("${{ matrix.os }}", matrix)));
        let windows = "    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n";
        assert!(!scope(&on("${{ matrix.os }}", windows)));
        // A shell said outright decides.
        assert!(scope(&on(
            "windows-latest",
            "    defaults:\n      run:\n        shell: bash\n"
        )));
        assert!(!scope(&on(
            "ubuntu-latest",
            "    defaults:\n      run:\n        shell: pwsh\n"
        )));
    }

    /// \[ADR 0050\] A composite action's line reads its variables from its own step out to the job
    /// that uses the action, and an input from its default and from what the job's step gives it,
    /// read in the job.
    // covers: C-15
    #[test]
    fn a_line_in_an_action_reads_what_its_job_gives_it() {
        let workflow = hushrun_core::yaml::parse(
            "ci.yml",
            b"on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    env:\n      LEVEL: high\n      TOKEN: ${{ secrets.NPM_TOKEN }}\n    steps:\n      - uses: actions/checkout@v5\n      - uses: ./.github/actions/tests\n        with:\n          args: -q\n          token: ${{ secrets.NPM_TOKEN }}\n          level: ${{ env.LEVEL }}\n          hidden: ${{ env.TOKEN }}\n",
        )
        .expect("YAML");
        let action = hushrun_core::yaml::parse(
            "action.yml",
            b"inputs:\n  args: {}\n  own:\n    default: ${{ github.token }}\n  plain:\n    default: fast\nruns:\n  using: composite\n  steps:\n    - run: echo \"WHERE=$(pwd)\" >> $GITHUB_ENV\n      shell: bash\n    - run: pytest\n      shell: bash\n      env:\n        MODE: ${{ inputs.level }}\n",
        )
        .expect("YAML");
        let actions = [(&action, 1)];
        let scope = Scope {
            file: &workflow,
            job: "test",
            step: Some(1),
            actions: &actions,
        };
        let judge = |word: &str| verdict(&["pytest".to_string(), word.to_string()], &scope);
        for word in [
            "${{ inputs.args }}",
            "${{ inputs.level }}",
            "${{ inputs.plain }}",
            "${{ inputs.unset }}",
            "${{ github.action_path }}/tests",
            "$LEVEL",
            "$MODE",
        ] {
            assert_eq!(judge(word), Ok(()), "{word}");
        }
        for word in [
            "${{ inputs.token }}",
            "${{ inputs.hidden }}",
            "${{ inputs.own }}",
            "$TOKEN",
        ] {
            assert_eq!(judge(word), Err(secret(word)), "{word}");
        }
        assert_eq!(judge("$WHERE"), Err(unknown("WHERE")));
        // A composite step names its shell.
        assert!(bash(&scope));
        let pwsh = hushrun_core::yaml::parse(
            "action.yml",
            b"runs:\n  using: composite\n  steps:\n    - run: GOARCH=386 go test ./...\n      shell: pwsh\n",
        )
        .expect("YAML");
        let actions = [(&pwsh, 0)];
        assert!(!bash(&Scope {
            file: &workflow,
            job: "test",
            step: Some(1),
            actions: &actions,
        }));
        // A workflow's input with the job's token for its default holds a secret.
        let called = hushrun_core::yaml::parse(
            "test.yml",
            b"on:\n  workflow_call:\n    inputs:\n      token:\n        type: string\n        default: ${{ github.token }}\n      level:\n        type: string\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: pytest\n",
        )
        .expect("YAML");
        let scope = Scope {
            file: &called,
            job: "test",
            step: Some(0),
            actions: &[],
        };
        let judge = |word: &str| verdict(&["pytest".to_string(), word.to_string()], &scope);
        assert_eq!(
            judge("${{ inputs.token }}"),
            Err(secret("${{ inputs.token }}"))
        );
        assert_eq!(judge("${{ inputs.level }}"), Ok(()));
    }
}
