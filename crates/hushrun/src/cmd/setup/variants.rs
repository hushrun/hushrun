//! \[ADR 0044\] Whether a pass can skip anything in this CI job. A run's key holds no environment
//! (C-9), so a job that runs its steps several times can't let one pass stand for them all: the
//! first variant to pass would skip the rest. GitHub's workflow file says how many variants a job
//! has, read here as YAML and never written; GitLab says it in its variables. Whenever that can't
//! be told, hushrun runs (C-14).

use std::collections::{BTreeMap, BTreeSet};

use hushrun_ci::Env;
use hushrun_core::run::Provider;
use serde_json::{Map, Value};

use super::workflows::{Host, Job, Workflow};
use super::{checks, readers, scripts};
use crate::results::Variant;

/// What `hushrun run` sets on the command it starts: a run that sees it is inside another, whose
/// key is what tells variants apart.
pub const INSIDE_A_RUN: &str = "HUSHRUN_INSIDE_RUN";

/// \[ADR 0049\] What a job of several variants sets to `${{ toJSON(matrix) }}`, so that each run
/// says which variant it is.
pub const VARIANT: &str = "HUSHRUN_VARIANT";

/// \[ADR 0049\] What each job of a workflow another calls sets to its own file's path, since
/// GitHub names the caller's file in `GITHUB_WORKFLOW_REF`.
pub const WORKFLOW: &str = "HUSHRUN_WORKFLOW";

/// \[ADR 0049\] What a called workflow's job that takes inputs sets to `${{ toJSON(inputs) }}`,
/// so that each call is told apart: hashed, never shown.
pub const INPUTS: &str = "HUSHRUN_INPUTS";

/// \[ADR 0049\] The most of `HUSHRUN_VARIANT` hushrun reads.
const MOST_VARIANT_BYTES: usize = 64 << 10;
/// \[ADR 0049\] The longest a variant's label is, in characters.
const LONGEST_LABEL: usize = 100;

/// What a job's `strategy.matrix` makes of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matrix {
    /// It runs once: no matrix, or a matrix of one.
    One,
    /// It runs several times, or only run time knows. What each key can fill into a name, when
    /// that can be told for them all.
    Several(Option<Values>),
}

/// Each key of a matrix, with the values it lists.
pub type Values = BTreeMap<String, BTreeSet<String>>;

/// The file at `path` as YAML, or `None` when it doesn't parse.
pub fn parse(path: &str, bytes: &[u8]) -> Option<Value> {
    hushrun_core::yaml::parse(path, bytes).ok()
}

/// What `job`, a workflow's `jobs.<id>`, says about its variants.
pub fn matrix_of(job: &Value) -> Matrix {
    let Some(strategy) = job.get("strategy") else {
        return Matrix::One;
    };
    let Some(strategy) = strategy.as_object() else {
        return Matrix::Several(None);
    };
    let Some(matrix) = strategy.get("matrix") else {
        return Matrix::One;
    };
    let Some(matrix) = matrix.as_object() else {
        return Matrix::Several(None);
    };
    let mut dimensions: Vec<(&str, &[Value])> = Vec::new();
    let mut countable = !holds_an_expression(&Value::Object(matrix.clone()));
    for (key, value) in matrix {
        if matches!(key.as_str(), "include" | "exclude") {
            continue;
        }
        match value.as_array() {
            Some(values) => dimensions.push((key, values)),
            None => countable = false,
        }
    }
    let include = match matrix.get("include") {
        None => Vec::new(),
        Some(list) => {
            let items: Option<Vec<_>> = list
                .as_array()
                .and_then(|items| items.iter().map(Value::as_object).collect());
            let Some(items) = items else {
                return Matrix::Several(None);
            };
            items
        }
    };
    if countable && !several(&dimensions, &include) {
        return Matrix::One;
    }
    Matrix::Several(values_of(matrix, &include))
}

/// \[ADR 0045\] Whether `job`, a workflow's `jobs.<id>`, can only be on a Mac: its `runs-on` is one
/// `macos-*` runner, as GitHub names them, in any case. Anything else says nothing: an expression
/// is known at run time, and a list or a group is a self-hosted runner's own words.
pub fn on_macos(job: &Value) -> bool {
    let runner = match &job["runs-on"] {
        Value::Array(list) if list.len() == 1 => &list[0],
        value => value,
    };
    runner.as_str().is_some_and(|runner| {
        !runner.contains("${{") && runner.to_ascii_lowercase().starts_with("macos-")
    })
}

/// What each key of `matrix` lists, `include`'s too, or `None` when a value can't be told: only run
/// time knows it, or it has no plain text, as an object doesn't.
fn values_of(matrix: &Map<String, Value>, include: &[&Map<String, Value>]) -> Option<Values> {
    let mut found = Values::new();
    for (key, value) in matrix {
        if !matches!(key.as_str(), "include" | "exclude") {
            for value in value.as_array()? {
                found
                    .entry(key.clone())
                    .or_default()
                    .insert(text_of(value)?);
            }
        }
    }
    for (key, value) in include.iter().flat_map(|item| item.iter()) {
        found
            .entry(key.clone())
            .or_default()
            .insert(text_of(value)?);
    }
    Some(found)
}

/// What CI fills into a name for a matrix's `value`, when that can be told.
fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => (!text.contains("${{")).then(|| text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Whether a matrix has more than one variant: more than one combination of `dimensions`, or an
/// `include` item that is a variant of its own, since it overwrites a dimension's value, or there
/// are none. `exclude` could leave one, or two values could be one, such as `20` and `'20'`: that
/// counts as several, which only runs more.
fn several(dimensions: &[(&str, &[Value])], include: &[&Map<String, Value>]) -> bool {
    let overwrites = |item: &Map<String, Value>| {
        dimensions.iter().any(|(key, values)| {
            item.get(*key)
                .is_some_and(|value| Some(value) != values.first())
        })
    };
    dimensions.iter().any(|(_, values)| values.len() != 1)
        || (dimensions.is_empty() && include.len() > 1)
        || include.iter().any(|item| overwrites(item))
}

fn holds_an_expression(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains("${{"),
        Value::Array(items) => items.iter().any(holds_an_expression),
        Value::Object(members) => members.values().any(holds_an_expression),
        _ => false,
    }
}

/// \[ADR 0049\] What a CI job's variants make of a run of one of its checks (C-14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Variants {
    /// The job runs once, or the check's name tells its variants apart: a pass of the job skips
    /// it.
    Alike,
    /// The job runs several times, and this run is the variant it says: only that variant's
    /// passes skip it.
    Variant(Variant),
    /// No pass skips it, and why.
    NoSkip(NoSkip),
}

impl Variants {
    /// Why no pass skips the run, when none does.
    pub fn no_skip(self) -> Option<NoSkip> {
        match self {
            Variants::NoSkip(no) => Some(no),
            _ => None,
        }
    }
}

/// \[ADR 0049\] The variant `matrix`, what `${{ toJSON(matrix) }}` gave `HUSHRUN_VARIANT`, names
/// (C-14): its key is the `sha256:` of its JCS form, so key order and spaces don't matter, and its
/// label its values in key order. An object, or `null` for a job without one; `None` for anything
/// else, or past 64 KiB.
pub fn variant_of(matrix: &str) -> Option<Variant> {
    let matrix = object_of(matrix)?;
    let key = hushrun_core::jcs::digest(&Value::Object(matrix.clone())).ok()?;
    Some(Variant {
        key: format!("sha256:{}", key.hex()),
        label: label_of(&matrix),
    })
}

/// `text` as a JSON object, `null` read as `{}`.
fn object_of(text: &str) -> Option<Map<String, Value>> {
    if text.len() > MOST_VARIANT_BYTES {
        return None;
    }
    match serde_json::from_str::<Value>(text).ok()? {
        Value::Null => Some(Map::new()),
        Value::Object(members) => Some(members),
        _ => None,
    }
}

/// A variant's values in key order, joined with `, `: a scalar as written, anything else as
/// compact JSON, at most [`LONGEST_LABEL`] characters.
fn label_of(values: &Map<String, Value>) -> String {
    let parts: Vec<String> = values
        .values()
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect();
    shortened_label(parts.join(", "))
}

/// `label`, cut to [`LONGEST_LABEL`] characters, the last `…` when cut.
fn shortened_label(label: String) -> String {
    if label.chars().count() <= LONGEST_LABEL {
        return label;
    }
    let mut cut: String = label.chars().take(LONGEST_LABEL - 1).collect();
    cut.push('…');
    cut
}

/// Whether `text` is the expression `${{ <expression> }}`, whatever its spaces and the case of its
/// words.
pub fn is_expression(text: &str, expression: &str) -> bool {
    let Some(inner) = text
        .trim()
        .strip_prefix("${{")
        .and_then(|rest| rest.strip_suffix("}}"))
    else {
        return false;
    };
    let inner: String = inner.chars().filter(|c| !c.is_whitespace()).collect();
    inner.eq_ignore_ascii_case(expression)
}

/// \[ADR 0049\] Whether `job`, a workflow's `jobs.<id>`, sets `env.HUSHRUN_VARIANT` to
/// `${{ toJSON(matrix) }}`, as init writes it.
pub fn sets_variant(job: &Value) -> bool {
    env_value(job, VARIANT).is_some_and(|value| is_expression(&value, "toJSON(matrix)"))
}

/// Why no pass skips a check in this CI job, and which of the walls it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoSkip {
    pub wall: Wall,
    /// What the run says, after `<NAME> running: `.
    pub why: String,
}

/// What stops a skip in a CI job, as init's plan names it (`ciWhy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wall {
    /// The job has several variants or instances, which a key doesn't tell apart.
    Variants,
    /// The job may be one of a reusable workflow's, which CI names by its caller.
    Reusable,
    /// hushrun can't find or read the job.
    CantTell,
    /// \[ADR 0047\] A later step of the job reads the check's reports, which a skip doesn't write.
    Reports,
}

impl Wall {
    /// The word init's plan says (`ciWhy`).
    pub fn word(self) -> &'static str {
        match self {
            Wall::Variants => "variants",
            Wall::Reusable => "reusable",
            Wall::CantTell => "cant-tell",
            Wall::Reports => "reports",
        }
    }
}

/// \[C-14\] In a CI job, why no pass skips the check `check`, whatever the key: the job has several
/// variants that its name doesn't tell apart, or hushrun can't tell. `named` says the command line
/// gives that name. It reads the repository's files with `read` (a path from the root, `/`
/// separated): the run reads them from the checkout, and init's plan from what it would write, so
/// that its prediction is the run's decision (C-15). The run and init read the job once, with
/// [`Ci`], for this rule and the reports rule.
#[cfg(test)]
pub fn no_skip(
    env: &Env,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    check: &str,
    named: bool,
) -> Option<NoSkip> {
    Ci::read(env, read).variants(env, check, named).no_skip()
}

/// \[ADR 0047\] The GitHub Actions job a run is in, as its workflow file says.
#[derive(Debug, Clone)]
pub struct CiJob {
    /// The workflow's path from the root.
    pub path: String,
    pub id: String,
    /// The workflow, as YAML.
    pub file: Value,
    /// \[ADR 0049\] It is a job of a workflow another calls, as its `HUSHRUN_WORKFLOW` says.
    pub called: bool,
}

impl CiJob {
    /// Its `jobs.<id>`.
    pub fn job(&self) -> &Value {
        &self.file["jobs"][self.id.as_str()]
    }
}

/// What a CI run reads of its job, once, for every rule that reads it.
#[derive(Debug, Clone)]
pub struct Ci {
    provider: Provider,
    /// On GitHub Actions, its job, or why hushrun can't tell which it is.
    github: Option<Result<CiJob, NoSkip>>,
}

impl Ci {
    /// Where the run is, from `env`, and on GitHub Actions its job, read with `read`.
    pub fn read(env: &Env, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Ci {
        let provider = hushrun_ci::detect(env);
        let github = (provider == Provider::Github).then(|| find_on_github(env, read));
        Ci { provider, github }
    }

    /// \[ADR 0044\] What this job's variants make of a run of `check` (C-14): no pass skips it, for
    /// its variants or since hushrun can't tell which job it is; \[ADR 0049\] or it is the variant
    /// it says, whose passes alone skip it.
    pub fn variants(&self, env: &Env, check: &str, named: bool) -> Variants {
        match self.provider {
            Provider::Github => match self.github.as_ref() {
                None => Variants::Alike,
                Some(Err(no)) => Variants::NoSkip(no.clone()),
                Some(Ok(job)) => variants_on_github(job, env, check, named),
            },
            Provider::Gitlab => on_gitlab(env).map_or(Variants::Alike, Variants::Variant),
            _ => Variants::Alike,
        }
    }

    /// \[ADR 0048\] The paths the job's readers name: where its reports may be (C-27).
    pub fn reader_paths(&self, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Vec<String> {
        let Some(Ok(job)) = &self.github else {
            return Vec::new();
        };
        readers::readers(job.job(), read)
            .into_iter()
            .flat_map(|reader| reader.paths)
            .collect()
    }

    /// \[ADR 0047\] What in this job reads reports a check of `kind` may write, which a skip
    /// doesn't write (C-26), so that a pass that counts here doesn't skip it: the steps that read
    /// them, or `HUSHRUN_REPORTS_READ`, which says it on any CI. `skips_anyway` says the
    /// repository's readers handle a skip (`reports.ci: skip`). Outside CI nothing reads them.
    pub fn reports(
        &self,
        read: &dyn Fn(&str) -> Option<Vec<u8>>,
        kind: checks::Kind,
        skips_anyway: bool,
        reports_read: Option<bool>,
    ) -> Option<ReportsRead> {
        if skips_anyway || self.provider == Provider::Local {
            return None;
        }
        if reports_read == Some(true) {
            return Some(ReportsRead {
                readers: Vec::new(),
                sources: Vec::new(),
            });
        }
        let Some(Ok(job)) = &self.github else {
            return None;
        };
        // A linter's skip leaves coverage and test results as they were.
        let readers: Vec<readers::Reader> = readers::readers(job.job(), read)
            .into_iter()
            .filter(|reader| !reader.only_on_failure)
            .filter(|reader| kind == checks::Kind::Test || !reader.coverage_or_tests_only())
            .collect();
        if readers.is_empty() {
            return None;
        }
        // What the readers are read from: a change to any of them may change what they read.
        let mut sources = vec![job.path.clone()];
        for step in job.job()["steps"].as_array().into_iter().flatten() {
            let local = step
                .get("uses")
                .and_then(Value::as_str)
                .and_then(|uses| uses.strip_prefix("./"));
            if let Some(dir) = local {
                let dir = dir.trim_end_matches('/');
                for name in ["action.yml", "action.yaml"] {
                    let path = if dir.is_empty() {
                        name.to_string()
                    } else {
                        format!("{dir}/{name}")
                    };
                    if read(&path).is_some() {
                        sources.push(path);
                    }
                }
            }
        }
        Some(ReportsRead { readers, sources })
    }
}

/// \[ADR 0047\] What in a CI job reads reports a check writes, which a skip doesn't write (C-26).
#[derive(Debug, Clone)]
pub struct ReportsRead {
    /// The steps that read them; none when `HUSHRUN_REPORTS_READ` says the job reads reports,
    /// which says no more.
    readers: Vec<readers::Reader>,
    /// The repository's files the readers are read from: the workflow, and the local actions its
    /// steps use.
    pub sources: Vec<String>,
}

impl ReportsRead {
    /// Why a pass that counts here doesn't skip the check, whatever it wrote.
    pub fn no_skip(&self) -> NoSkip {
        NoSkip {
            wall: Wall::Reports,
            why: match self.readers.first() {
                Some(reader) => reads(reader),
                None => "HUSHRUN_REPORTS_READ says this job reads reports, and a skip writes none"
                    .to_string(),
            },
        }
    }

    /// \[ADR 0048\] Why a pass that counts here doesn't skip the check, when `wrote` is every
    /// report file this job's own pass of it wrote (C-26): a reader that reads one of them, or one
    /// whose reading hushrun can't tell: it names no path, or one CI or a shell fills in, or one
    /// outside the repository, or it reads other reports. `None` when no reader reads what the
    /// pass wrote, so that a skip leaves them all as a run would.
    pub fn despite(&self, wrote: &[hushrun_reports::File]) -> Option<NoSkip> {
        if self.readers.is_empty() {
            return Some(self.no_skip());
        }
        let reader = self
            .readers
            .iter()
            .find(|reader| reads_any(reader, wrote))?;
        Some(NoSkip {
            wall: Wall::Reports,
            why: reads(reader),
        })
    }
}

/// What the run says of `reader`, after `<NAME> running: `.
fn reads(reader: &readers::Reader) -> String {
    format!(
        "a later step reads its reports ({}), and a skip writes none",
        reader.name
    )
}

/// Whether `reader` may read one of `wrote`, files by path from the root.
fn reads_any(reader: &readers::Reader, wrote: &[hushrun_reports::File]) -> bool {
    if !reader.bounded || reader.kinds.contains(&readers::Kind::Other) {
        return true;
    }
    reader.paths.iter().any(|path| match from_the_root(path) {
        None => true,
        Some((itself, below)) => wrote
            .iter()
            .any(|file| itself.is_match(&file.path) || below.is_match(&file.path)),
    })
}

/// A reader's `path` as patterns from the root, of itself and of what is below it, as a
/// directory; `None` when hushrun can't follow it: CI fills in a part other than the workspace
/// at its start, or a shell a variable, it is absolute or outside the repository, it is the whole
/// repository, or it isn't a pattern hushrun reads.
fn from_the_root(path: &str) -> Option<(hushrun_core::glob::Pattern, hushrun_core::glob::Pattern)> {
    let path = path.trim().replace('\\', "/");
    let path = match path.strip_prefix("${{") {
        Some(rest) => {
            let (inside, after) = rest.split_once("}}")?;
            if inside.trim() != "github.workspace" || !after.starts_with('/') {
                return None;
            }
            after.to_string()
        }
        None if path.starts_with('/') => return None,
        None => path,
    };
    if path.contains('$') || path.starts_with('~') || path.as_bytes().get(1) == Some(&b':') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return None;
    }
    let itself = parts.join("/");
    let pattern = |text: &str| hushrun_core::glob::Pattern::new(text).ok();
    Some((pattern(&itself)?, pattern(&format!("{itself}/**"))?))
}

/// The job GitHub names in `env`, read from its workflow file; or why hushrun can't tell which it
/// is, and the wall that is.
fn find_on_github(env: &Env, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<CiJob, NoSkip> {
    let cant_tell = |wall: Wall, what: String| NoSkip {
        wall,
        why: format!("{what}, so hushrun can't tell its variants, and no pass skips it"),
    };
    let Some((workflow, id)) = hushrun_ci::github::workflow_job(env) else {
        return Err(cant_tell(
            Wall::CantTell,
            "GITHUB_WORKFLOW_REF and GITHUB_JOB don't name this job".to_string(),
        ));
    };
    // \[ADR 0049\] A called workflow's job says its own file, which GitHub doesn't.
    if let Some(named) = env.get(WORKFLOW) {
        return called_job(read, named, id);
    }
    let Some(path) = own_workflow(env, workflow) else {
        return Err(cant_tell(
            Wall::CantTell,
            format!("{workflow} isn't in this checkout"),
        ));
    };
    let Some(bytes) = read(path) else {
        return Err(cant_tell(
            Wall::CantTell,
            format!("{path} can't be read here"),
        ));
    };
    let Some(file) = parse(path, &bytes) else {
        return Err(cant_tell(
            Wall::CantTell,
            format!("{path} can't be read as YAML"),
        ));
    };
    let Some(job) = file.get("jobs").and_then(|jobs| jobs.get(id)) else {
        return Err(cant_tell(
            Wall::Reusable,
            format!("{path} has no job {id}, as with a reusable workflow's"),
        ));
    };
    if job.get("uses").is_some() {
        return Err(cant_tell(
            Wall::Reusable,
            format!("job {id} of {path} calls a reusable workflow"),
        ));
    }
    if let Some(what) = called_with_this_id(read, path, &file, id) {
        return Err(cant_tell(Wall::Reusable, what));
    }
    Ok(CiJob {
        path: path.to_string(),
        id: id.to_string(),
        file,
        called: false,
    })
}

/// \[ADR 0049\] The job `id` of the file `named`, as `HUSHRUN_WORKFLOW` names it: a workflow of
/// this checkout that another calls (`on: workflow_call`), whose job `id` sets
/// `env.HUSHRUN_WORKFLOW` to that very path. Anything else can't be told (C-14).
fn called_job(
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    named: &str,
    id: &str,
) -> Result<CiJob, NoSkip> {
    let named = named.trim();
    let doesnt_name = |path: &str| NoSkip {
        wall: Wall::Reusable,
        why: format!(
            "HUSHRUN_WORKFLOW names {path}, whose job {id} doesn't name that file, so hushrun can't tell this job, and no pass skips it"
        ),
    };
    let path = named.strip_prefix("./").unwrap_or(named);
    let Some(path) = in_workflows(path) else {
        return Err(NoSkip {
            wall: Wall::Reusable,
            why: format!(
                "HUSHRUN_WORKFLOW names {named}, which isn't under .github/workflows/, so hushrun can't tell this job, and no pass skips it"
            ),
        });
    };
    let Some(file) = read(path).and_then(|bytes| parse(path, &bytes)) else {
        return Err(NoSkip {
            wall: Wall::Reusable,
            why: format!(
                "HUSHRUN_WORKFLOW names {path}, which can't be read here, so hushrun can't tell this job, and no pass skips it"
            ),
        });
    };
    if !names_its_file(&file["jobs"][id], path) || !has_trigger(&file, "workflow_call") {
        return Err(doesnt_name(path));
    }
    Ok(CiJob {
        path: path.to_string(),
        id: id.to_string(),
        file,
        called: true,
    })
}

/// Whether `file`, a workflow, starts on the event `event`, however `on:` writes it.
fn has_trigger(file: &Value, event: &str) -> bool {
    match file.get("on").or_else(|| file.get("true")) {
        Some(Value::String(one)) => one == event,
        Some(Value::Array(many)) => many.iter().any(|one| one.as_str() == Some(event)),
        Some(Value::Object(map)) => map.contains_key(event),
        _ => false,
    }
}

/// \[ADR 0044\] What `job`'s variants make of a run of `check`: with several, which a run's key
/// doesn't tell apart, no pass skips it, unless the command line names each; \[ADR 0049\] or
/// `HUSHRUN_VARIANT` says which variant it is, when the job sets it to `${{ toJSON(matrix) }}`,
/// which decides over a name.
fn variants_on_github(job: &CiJob, env: &Env, check: &str, named: bool) -> Variants {
    if job.called {
        return called_variants(job, env);
    }
    let (path, id) = (&job.path, &job.id);
    let Matrix::Several(values) = matrix_of(job.job()) else {
        return Variants::Alike;
    };
    let no_skip = |why: String| {
        Variants::NoSkip(NoSkip {
            wall: Wall::Variants,
            why,
        })
    };
    if env.get(INSIDE_A_RUN).is_some() {
        return no_skip(format!(
            "job {id} of {path} has several variants, which a run's key doesn't tell apart, and this run is inside another, so no pass skips it"
        ));
    }
    if let Some(value) = env.get(VARIANT) {
        if !sets_variant(job.job()) {
            return no_skip(format!(
                "job {id} of {path} has several variants, and its HUSHRUN_VARIANT isn't ${{{{ toJSON(matrix) }}}}, so no pass skips it"
            ));
        }
        return match variant_of(value) {
            Some(variant) => Variants::Variant(variant),
            None => no_skip(
                "HUSHRUN_VARIANT isn't JSON, so hushrun can't tell this variant, and no pass skips it"
                    .to_string(),
            ),
        };
    }
    if named && !in_a_script(env) && names_each(job.job(), values.as_ref(), check) {
        return Variants::Alike;
    }
    no_skip(format!(
        "job {id} of {path} has several variants, and its HUSHRUN_VARIANT isn't ${{{{ toJSON(matrix) }}}}, so no pass skips it"
    ))
}

/// \[ADR 0049\] What a called workflow's job makes of a run: its call, told apart by the inputs
/// it was called with and the variant of its matrix it is, each `null` when the file takes none
/// or the job runs once. Its key is over `{"inputs": …, "matrix": …}`; its label holds only the
/// matrix's values, since inputs never show.
fn called_variants(job: &CiJob, env: &Env) -> Variants {
    let (path, id) = (&job.path, &job.id);
    let no_skip = |why: String| {
        Variants::NoSkip(NoSkip {
            wall: Wall::Variants,
            why,
        })
    };
    // What the running job's env holds, the file's job must set too: a variable it doesn't set is
    // another file's, as a caller's checkout can hold one at the same path, and a value it does
    // tells the run apart, whatever the file says of its variants.
    let untold = |name: &str| {
        Variants::NoSkip(NoSkip {
            wall: Wall::Reusable,
            why: format!(
                "{name} is set, and job {id} of {path} doesn't set it, so hushrun can't tell this job, and no pass skips it"
            ),
        })
    };
    let several = matrix_of(job.job()) != Matrix::One;
    if several && env.get(INSIDE_A_RUN).is_some() {
        return no_skip(format!(
            "job {id} of {path} has several variants, which a run's key doesn't tell apart, and this run is inside another, so no pass skips it"
        ));
    }
    let matrix = match env.get(VARIANT) {
        Some(_) if !sets_variant(job.job()) && several => {
            return no_skip(format!(
                "job {id} of {path} has several variants, and its HUSHRUN_VARIANT isn't ${{{{ toJSON(matrix) }}}}, so no pass skips it"
            ));
        }
        Some(_) if !sets_variant(job.job()) => return untold(VARIANT),
        Some(value) => match object_of(value) {
            Some(matrix) => Value::Object(matrix),
            None => {
                return no_skip(
                    "HUSHRUN_VARIANT isn't JSON, so hushrun can't tell this variant, and no pass skips it"
                        .to_string(),
                );
            }
        },
        None if several => {
            return no_skip(format!(
                "job {id} of {path} has several variants, and its HUSHRUN_VARIANT isn't ${{{{ toJSON(matrix) }}}}, so no pass skips it"
            ));
        }
        None => Value::Null,
    };
    let takes = takes_inputs(&job.file);
    let inputs = match env.get(INPUTS) {
        Some(_) if !sets_inputs(job.job()) && takes => {
            return no_skip(format!(
                "job {id} of {path} takes inputs, and its HUSHRUN_INPUTS isn't ${{{{ toJSON(inputs) }}}}, so no pass skips it"
            ));
        }
        Some(_) if !sets_inputs(job.job()) => return untold(INPUTS),
        Some(value) => match object_of(value) {
            Some(inputs) => Value::Object(inputs),
            None => {
                return no_skip(
                    "HUSHRUN_INPUTS isn't JSON, so hushrun can't tell this call, and no pass skips it"
                        .to_string(),
                );
            }
        },
        None if takes => {
            return no_skip(format!(
                "job {id} of {path} takes inputs, and its HUSHRUN_INPUTS isn't ${{{{ toJSON(inputs) }}}}, so no pass skips it"
            ));
        }
        None => Value::Null,
    };
    if matrix.is_null() && inputs.is_null() {
        return Variants::Alike;
    }
    let label = matrix.as_object().map(label_of).unwrap_or_default();
    let identity = serde_json::json!({"inputs": inputs, "matrix": matrix});
    match hushrun_core::jcs::digest(&identity) {
        Ok(key) => Variants::Variant(Variant {
            key: format!("sha256:{}", key.hex()),
            label,
        }),
        Err(_) => no_skip(
            "HUSHRUN_INPUTS or HUSHRUN_VARIANT can't be read as JSON's canonical form, so no pass skips it"
                .to_string(),
        ),
    }
}

/// Whether `file`, a workflow another calls, declares inputs (`on.workflow_call.inputs`).
pub fn takes_inputs(file: &Value) -> bool {
    file.get("on")
        .and_then(|on| on.get("workflow_call"))
        .and_then(|call| call.get("inputs"))
        .and_then(Value::as_object)
        .is_some_and(|inputs| !inputs.is_empty())
}

/// What says this may not be the job of `path`'s `file` that holds the id `id`, as hushrun can read
/// it: in a reusable workflow, CI names the caller's file and the called file's job, so a file this
/// one calls, or a file that calls in turn, may hold that id too. One that can't be read may as well.
/// A call to another repository's file can't be read, and says nothing (C-14).
fn called_with_this_id(
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    path: &str,
    file: &Value,
    id: &str,
) -> Option<String> {
    let mut seen = BTreeSet::from([path.to_string()]);
    let mut next = local_calls(file);
    while let Some(called) = next.pop() {
        if !seen.insert(called.clone()) {
            continue;
        }
        let read = in_workflows(&called)
            .and_then(read)
            .and_then(|bytes| parse(&called, &bytes));
        let Some(read) = read else {
            return Some(format!("{called}, which {path} calls, can't be read here"));
        };
        if read.get("jobs").and_then(|jobs| jobs.get(id)).is_some() {
            return Some(format!("{called}, which {path} calls, has a job {id} too"));
        }
        next.extend(local_calls(&read));
    }
    None
}

/// The files of this repository that jobs of `file` call, as `uses:` writes them: `./<path>`.
fn local_calls(file: &Value) -> Vec<String> {
    let jobs = file.get("jobs").and_then(Value::as_object);
    jobs.into_iter()
        .flat_map(Map::values)
        .filter_map(|job| job.get("uses")?.as_str()?.strip_prefix("./"))
        .map(str::to_string)
        .collect()
}

/// `path`, when it is under `.github/workflows/`, the only files hushrun reads to tell.
fn in_workflows(path: &str) -> Option<&str> {
    (path.starts_with(".github/workflows/") && !path.split('/').any(|part| part == ".."))
        .then_some(path)
}

/// The path of a workflow named `<owner>/<name>/<file>`, when it is this repository's own.
fn own_workflow<'w>(env: &Env, workflow: &'w str) -> Option<&'w str> {
    let repository = env.get("GITHUB_REPOSITORY")?;
    let path = workflow
        .get(..repository.len())
        .filter(|named| named.eq_ignore_ascii_case(repository))
        .and_then(|_| workflow[repository.len()..].strip_prefix('/'))?;
    in_workflows(path)
}

/// \[ADR 0044\] On GitLab, the instance of a job that runs several times, `parallel` or
/// `parallel:matrix`, as its variables say; \[ADR 0049\] as a variant, `None` for a job that runs
/// once.
fn on_gitlab(env: &Env) -> Option<Variant> {
    let total = env
        .get("CI_NODE_TOTAL")
        .and_then(|total| total.trim().parse::<u32>().ok())
        .filter(|total| *total > 1);
    let name = env.get("CI_JOB_NAME").unwrap_or_default();
    let matrix = name
        .strip_suffix(']')
        .and_then(|name| name.rsplit_once(": ["))
        .map(|(_, values)| values.to_string());
    // \[ADR 0049\] GitLab names each instance apart, so its passes are its own: the variant only
    // keeps a pass from outside CI from standing in for one (D3).
    let label = match (matrix, total) {
        (Some(values), _) => values,
        (None, Some(total)) => format!("{}/{total}", env.get("CI_NODE_INDEX").unwrap_or("?")),
        (None, None) => return None,
    };
    let key = hushrun_core::jcs::digest(&serde_json::json!({"instance": name})).ok()?;
    Some(Variant {
        key: format!("sha256:{}", key.hex()),
        label: shortened_label(label),
    })
}

/// Whether a package.json script started this run, which so isn't a line of the workflow. npx, `npm
/// exec` and bunx name what they start `npx` or `bunx`, and yarn by its binary's name.
fn in_a_script(env: &Env) -> bool {
    env.get("npm_lifecycle_event")
        .is_some_and(|script| !matches!(script.trim(), "npx" | "bunx" | "hushrun"))
}

/// Whether a `run:` of `job` holds a `hushrun run` of the name `check` that differs per variant:
/// only NAME counts, and only by `strategy.job-index` or a `matrix.<key>` of each of `values`' keys.
/// A line that writes that very name doesn't differ, and may be the one running.
fn names_each(job: &Value, values: Option<&Values>, check: &str) -> bool {
    let steps = job.get("steps").and_then(Value::as_array);
    let names: Vec<String> = steps
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("run").and_then(Value::as_str))
        .flat_map(scripts::written_names)
        .collect();
    !names.iter().any(|name| name == check)
        && names
            .iter()
            .any(|name| differs_per_variant(name, values) && can_be(name, check, values))
}

/// \[ADR 0044\] Whether a `run:` of `job` names a `hushrun run` per variant, by
/// `strategy.job-index` or the matrix's keys: what init's plan calls a job's `named` identity.
pub fn names_its_variants(job: &Value) -> bool {
    let Matrix::Several(values) = matrix_of(job) else {
        return false;
    };
    let steps = job.get("steps").and_then(Value::as_array);
    steps
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("run").and_then(Value::as_str))
        .flat_map(scripts::written_names)
        .any(|name| differs_per_variant(&name, values.as_ref()))
}

fn differs_per_variant(name: &str, values: Option<&Values>) -> bool {
    let holds = |expression: &str| name.contains(&format!("${{{{{expression}}}}}"));
    holds("strategy.job-index")
        || values.is_some_and(|values| {
            !values.is_empty() && values.keys().all(|key| holds(&format!("matrix.{key}")))
        })
}

/// Whether `check` is what CI can make of `name`. A `strategy.job-index` fills in digits, and a
/// `matrix.<key>` one of the values `values` lists for it. So a name claims only its own runs, not
/// a script's run inside it, such as `test-unit` under `test-${{ strategy.job-index }}`. Any other
/// expression can't be told, so nothing is what CI makes of it.
fn can_be(name: &str, check: &str, values: Option<&Values>) -> bool {
    let mut left = BTreeSet::from([check]);
    let mut written = name;
    loop {
        let (literal, after) = written.split_once("${{").unwrap_or((written, ""));
        left = left
            .into_iter()
            .filter_map(|rest| rest.strip_prefix(literal))
            .collect();
        if !written.contains("${{") {
            return left.contains("");
        }
        let Some((expression, next)) = after.split_once("}}") else {
            return false;
        };
        left = left
            .into_iter()
            .flat_map(|rest| filled(expression, rest, values))
            .collect();
        written = next;
    }
}

/// What can be left of `rest` once `expression` fills in its start, never with nothing.
fn filled<'r>(expression: &str, rest: &'r str, values: Option<&Values>) -> Vec<&'r str> {
    if expression == "strategy.job-index" {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        return (1..=digits).map(|digits| &rest[digits..]).collect();
    }
    let listed = expression
        .strip_prefix("matrix.")
        .and_then(|key| values?.get(key));
    listed
        .into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
        .filter_map(|value| rest.strip_prefix(value.as_str()))
        .collect()
}

/// What `ready` and `doctor` say of a wrapped check that job `job` of `file` runs, when a pass from
/// a laptop or an agent's sandbox doesn't skip it there: CI runs it, which is safe. \[ADR 0049\] A
/// job that says which variant or call each run is runs each until it passes; one that doesn't,
/// every time.
pub fn runs_every_time(file: &Workflow, job: &Job) -> Option<String> {
    if file.host == Host::GitLab {
        return job.matrix.then(|| EACH_VARIANT.to_string());
    }
    let (path, id) = (&file.path, &job.id);
    let yaml = parse(path, file.source().as_bytes());
    let found = yaml.as_ref().map(|yaml| &yaml["jobs"][id.as_str()]);
    let init = format!(
        "To skip there too, run {} init",
        crate::cmd::ready::hushrun()
    );
    // A workflow only a call starts runs as its caller names it, so its job says which file it
    // is, or no run there can tell. One that starts on its own too runs as itself then, as a
    // plain workflow does: what calls it elsewhere doesn't change those runs.
    let only_called = yaml
        .as_ref()
        .is_some_and(|yaml| starts_only_on(yaml, "workflow_call"));
    let says_its_file = found.is_some_and(|found| names_its_file(found, path));
    if only_called && !says_its_file {
        return Some(format!(
            "CI runs it every time: job {id} of {path}, a workflow another calls, doesn't set HUSHRUN_WORKFLOW to that file. {init}"
        ));
    }
    if job.matrix {
        return Some(match found.is_some_and(sets_variant) {
            true => EACH_VARIANT.to_string(),
            false => format!(
                "CI runs it every time: job {id} of {path} has several variants, and its HUSHRUN_VARIANT isn't ${{{{ toJSON(matrix) }}}}. {init}"
            ),
        });
    }
    // A job that names its file takes what its env says of its call, even with one variant.
    if says_its_file && found.is_some_and(|found| sets_inputs(found) || sets_variant(found)) {
        return Some(EACH_CALL.to_string());
    }
    if only_called && yaml.as_ref().is_some_and(takes_inputs) {
        return Some(format!(
            "CI runs it every time: job {id} of {path} takes inputs, and its HUSHRUN_INPUTS isn't ${{{{ toJSON(inputs) }}}}. {init}"
        ));
    }
    None
}

/// Whether `file`, a workflow, starts on the event `event` alone, however `on:` writes it.
fn starts_only_on(file: &Value, event: &str) -> bool {
    match file.get("on").or_else(|| file.get("true")) {
        Some(Value::String(one)) => one == event,
        Some(Value::Array(many)) => {
            !many.is_empty() && many.iter().all(|one| one.as_str() == Some(event))
        }
        Some(Value::Object(map)) => !map.is_empty() && map.keys().all(|key| key == event),
        _ => false,
    }
}

/// \[ADR 0049\] What `ready` says of a check CI runs in each variant of a job that tells them apart.
const EACH_VARIANT: &str = "CI runs each variant until that variant passes on this code";

/// \[ADR 0049\] What `ready` says of a check CI runs in each call of a workflow that takes inputs.
const EACH_CALL: &str = "CI runs each call until that call passes on this code";

/// \[ADR 0049\] Whether `job`, a workflow's `jobs.<id>`, sets `env.HUSHRUN_INPUTS` to
/// `${{ toJSON(inputs) }}`, as init writes it.
pub fn sets_inputs(job: &Value) -> bool {
    env_value(job, INPUTS).is_some_and(|value| is_expression(&value, "toJSON(inputs)"))
}

/// \[ADR 0049\] Whether `job`, a workflow's `jobs.<id>`, sets `env.HUSHRUN_WORKFLOW` to `path`,
/// its own file, as init writes it.
pub fn names_its_file(job: &Value, path: &str) -> bool {
    env_value(job, WORKFLOW).is_some_and(|value| {
        let value = value.trim();
        value.strip_prefix("./").unwrap_or(value) == path
    })
}

/// What `job`'s own `env` sets `name` to, as text.
fn env_value(job: &Value, name: &str) -> Option<String> {
    let value = job.get("env")?.get(name)?;
    Some(
        value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_string),
    )
}

/// \[ADR 0049\] What `doctor` warns of job `job` of `file`: a value it sets that isn't what tells
/// its runs apart, so that no pass skips it there. A value it doesn't set at all is `doctor`'s
/// `ok`: CI runs the check every time, which is safe, and init sets it.
pub fn identity_warning(file: &Workflow, job: &Job) -> Option<String> {
    if file.host != Host::GitHub {
        return None;
    }
    let (path, id) = (&file.path, &job.id);
    let yaml = parse(path, file.source().as_bytes())?;
    let found = &yaml["jobs"][id.as_str()];
    let wrong = |name: &str, value: String, should: &str| {
        Some(format!(
            "job {id} of {path} sets {name} to {value}, not {should}, so no pass skips it there"
        ))
    };
    if let Some(value) = env_value(found, WORKFLOW)
        && !names_its_file(found, path)
    {
        return wrong(WORKFLOW, value, "its own file");
    }
    if let Some(value) = env_value(found, VARIANT)
        && matrix_of(found) != Matrix::One
        && !sets_variant(found)
    {
        return wrong(VARIANT, value, "${{ toJSON(matrix) }}");
    }
    if let Some(value) = env_value(found, INPUTS)
        && names_its_file(found, path)
        && takes_inputs(&yaml)
        && !sets_inputs(found)
    {
        return wrong(INPUTS, value, "${{ toJSON(inputs) }}");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(matrix: &str) -> Matrix {
        let text = format!("runs-on: x\nstrategy:\n  matrix: {matrix}\n");
        matrix_of(&parse("job.yml", text.as_bytes()).unwrap())
    }

    fn several_over(keys: &[(&str, &[&str])]) -> Matrix {
        Matrix::Several(Some(values(keys)))
    }

    fn values(keys: &[(&str, &[&str])]) -> Values {
        keys.iter()
            .map(|(key, values)| {
                let values = values.iter().map(ToString::to_string).collect();
                (key.to_string(), values)
            })
            .collect()
    }

    // covers: C-14
    #[test]
    fn a_matrix_says_whether_it_has_several_variants() {
        assert_eq!(matrix_of(&Value::Null), Matrix::One);
        assert_eq!(of("{os: [a]}"), Matrix::One);
        assert_eq!(of("{os: [a, b]}"), several_over(&[("os", &["a", "b"])]));
        assert_eq!(
            of("{os: [a, b], node: [20]}"),
            several_over(&[("node", &["20"]), ("os", &["a", "b"])])
        );
        // A partial `include` joins every variant it fits, and adds none.
        assert_eq!(of("{os: [a], include: [{color: red}]}"), Matrix::One);
        assert_eq!(of("{os: [a], include: [{os: a, node: 20}]}"), Matrix::One);
        // One that overwrites a value is a variant of its own.
        assert_eq!(
            of("{os: [a], include: [{os: b}]}"),
            several_over(&[("os", &["a", "b"])])
        );
        // Alone, each is a variant of its own.
        assert_eq!(of("{include: [{os: a, node: 20}]}"), Matrix::One);
        assert_eq!(
            of("{include: [{os: a}, {os: b, node: 20}]}"),
            several_over(&[("node", &["20"]), ("os", &["a", "b"])])
        );
        // What could be one still counts as several, which only runs more.
        assert_eq!(
            of("{os: [a, b], exclude: [{os: b}]}"),
            several_over(&[("os", &["a", "b"])])
        );
        assert_eq!(
            of("{node: [20], include: [{node: '20'}]}"),
            several_over(&[("node", &["20"])])
        );
    }

    // covers: C-15, C-25
    #[test]
    fn only_one_macos_runner_says_a_job_is_on_a_mac() {
        let on = |runs_on: &str| on_macos(&parse("job.yml", runs_on.as_bytes()).unwrap());
        for mac in [
            "runs-on: macos-15\n",
            "runs-on: macos-latest\n",
            "runs-on: macOS-14\n",
            "runs-on: macos-15-xlarge\n",
            "runs-on: 'macos-13'\n",
            "runs-on: [macos-15]\n",
            "runs-on:\n  - macos-15\n",
            "runs-on: &mac macos-15\n",
        ] {
            assert!(on(mac), "{mac}");
        }
        for not in [
            "name: x\n",
            "runs-on: ubuntu-latest\n",
            "runs-on: windows-latest\n",
            "runs-on: macos\n",
            "runs-on: my-macos-15\n",
            "runs-on: ${{ matrix.os }}\n",
            "runs-on: macos-${{ matrix.version }}\n",
            "runs-on: ${{ vars.RUNNER }}\n",
            "runs-on: [self-hosted, macos-15]\n",
            "runs-on: [macos-15, self-hosted]\n",
            "runs-on: [macos-15, ARM64]\n",
            "runs-on: [self-hosted, macOS, ARM64]\n",
            "runs-on: {group: macs, labels: [macos-15]}\n",
            "runs-on: 15\n",
        ] {
            assert!(!on(not), "{not}");
        }
    }

    // covers: C-14
    #[test]
    fn a_matrix_only_run_time_knows_has_several_variants() {
        let unknown = Matrix::Several(None);
        assert_eq!(of("${{ fromJSON(needs.a.outputs.m) }}"), unknown);
        assert_eq!(
            of("{include: ${{ x }}}"
                .replace("${{ x }}", "'${{ x }}'")
                .as_str()),
            unknown
        );
        assert_eq!(of("{include: ['${{ x }}']}"), unknown);
        assert_eq!(of("{exclude: ['${{ x }}']}"), several_over(&[]));
        // A key's values can't be told, so a name by keys can't claim anything.
        assert_eq!(of("{os: '${{ x }}'}"), unknown);
        assert_eq!(of("{os: ['${{ x }}']}"), unknown);
        assert_eq!(of("{os: [a, '${{ x }}']}"), unknown);
        assert_eq!(
            of("{os: [a], include: [{os: a, flag: '${{ x }}'}]}"),
            unknown
        );
        // An object has no name: `matrix.cfg` is `Object` in every variant.
        assert_eq!(of("{cfg: [{a: 1}, {a: 2}]}"), unknown);
        assert_eq!(of("{os: [a, b], include: [{os: a, cfg: {x: 1}}]}"), unknown);
        assert_eq!(of("{os: [a, null]}"), unknown);
        let strategy = parse("job.yml", b"strategy: '${{ x }}'").unwrap();
        assert_eq!(matrix_of(&strategy), unknown);
    }

    // covers: C-14
    #[test]
    fn a_matrixs_values_are_what_ci_fills_in() {
        assert_eq!(
            of("{node: [18, 20.5, '18.x'], flag: [true, false]}"),
            several_over(&[
                ("flag", &["false", "true"]),
                ("node", &["18", "18.x", "20.5"]),
            ])
        );
    }

    // covers: C-14
    #[test]
    fn a_name_differs_per_variant_by_the_index_or_every_key() {
        let differs = |line: &str, keys: &Option<Values>| {
            scripts::written_names(line)
                .iter()
                .any(|name| differs_per_variant(name, keys.as_ref()))
        };
        let both = Some(values(&[("node", &["20"]), ("os", &["a"])]));
        assert!(differs(
            "hushrun run t-${{ strategy.job-index }} -- x",
            &both
        ));
        assert!(differs(
            "hushrun run t-${{ strategy.job-index }} -- x",
            &None
        ));
        assert!(differs(
            "hushrun run ${{ matrix.os }}-${{matrix.node}} -- x",
            &both
        ));
        assert!(!differs("hushrun run t-${{ matrix.os }} -- x", &both));
        assert!(!differs("hushrun run t-${{ matrix.os }} -- x", &None));
        // An expression may give every variant one name.
        assert!(!differs(
            "hushrun run t-${{ matrix.os == 'a' && 'x' || 'x' }}-${{ matrix.node }} -- x",
            &both
        ));
        assert!(!differs(
            "hushrun run t-${{ strategy.job-total }} -- x",
            &both
        ));
        assert!(!differs("hushrun run t -- x", &Some(Values::new())));
    }

    // covers: C-14
    #[test]
    fn a_written_name_can_be_what_ci_makes_of_it() {
        let listed = values(&[("a", &["1", "12"]), ("b", &["2", "x-2"])]);
        let can_be = |name, check| can_be(name, check, Some(&listed));
        assert!(can_be("test", "test"));
        assert!(!can_be("test", "tests"));
        assert!(can_be("test-${{strategy.job-index}}", "test-3"));
        assert!(can_be("test-${{strategy.job-index}}", "test-12"));
        assert!(can_be("t${{strategy.job-index}}-${{matrix.b}}", "t0-2"));
        assert!(can_be("${{matrix.a}}-x-${{matrix.b}}", "1-x-2"));
        assert!(can_be("${{matrix.a}}${{matrix.b}}", "12"));
        // Either split of what is written can be the one.
        assert!(can_be("${{matrix.a}}${{matrix.b}}", "122"));
        assert!(can_be("${{matrix.b}}-${{strategy.job-index}}", "x-2-1"));
        assert!(!can_be("${{matrix.a}}-x-${{matrix.b}}", "1-y-2"));
        assert!(!can_be("test-${{matrix.a}}", "lint-1"));
        // An expression fills in something: a run inside it, `test`, isn't `test${{ … }}`.
        assert!(!can_be("test${{strategy.job-index}}", "test"));
        assert!(!can_be("test-${{strategy.job-index}}", "test-"));
        assert!(!can_be("${{matrix.a}}${{matrix.b}}", "1"));
        // A shell's variable isn't an expression: nothing says what it holds.
        assert!(!can_be("test-$OS", "test-a"));
        assert!(!can_be("test-${{strategy.job-index", "test-1"));
    }

    /// Finding 1 of the second review: a wrapped `test:unit` runs inside `test-<n>`, and `test-unit`
    /// isn't a variant's name.
    // covers: C-14
    #[test]
    fn a_name_claims_only_what_ci_can_make_of_it() {
        let listed = values(&[("node", &["18", "20"]), ("empty", &[""])]);
        let can_be = |name, check| can_be(name, check, Some(&listed));
        // The index is digits, and nothing else.
        assert!(!can_be("test-${{strategy.job-index}}", "test-unit"));
        assert!(!can_be("test-${{strategy.job-index}}", "test-e2e"));
        assert!(!can_be("test-${{strategy.job-index}}", "test-1a"));
        // A key fills in what it lists, and no other.
        assert!(can_be("test-${{matrix.node}}", "test-18"));
        assert!(!can_be("test-${{matrix.node}}", "test-unit"));
        assert!(!can_be("test-${{matrix.node}}", "test-22"));
        assert!(!can_be("test-${{matrix.node}}", "test-180"));
        assert!(!can_be("test-${{matrix.other}}", "test-18"));
        assert!(!can_be("test-${{matrix.empty}}", "test-"));
        // Any other expression can't be told, even beside the index.
        assert!(!can_be("test-${{github.sha}}", "test-abc"));
        assert!(!can_be(
            "test-${{strategy.job-index}}-${{github.sha}}",
            "test-1-abc"
        ));
        assert!(!can_be(
            "test-${{strategy.job-index}}-${{matrix.os}}",
            "test-1-a"
        ));
        // With values that can't be told, only the index fills in.
        assert!(super::can_be("t-${{strategy.job-index}}", "t-1", None));
        assert!(!super::can_be("t-${{matrix.node}}", "t-18", None));
    }

    /// How long it takes doesn't grow with the ways to split a name.
    // covers: C-14
    #[test]
    fn a_long_name_of_many_expressions_ends() {
        let name = "${{strategy.job-index}}".repeat(40);
        assert!(can_be(&name, &"1".repeat(90), None));
        assert!(!can_be(&name, &format!("{}a", "1".repeat(90)), None));
    }

    fn env(pairs: &[(&str, &str)]) -> Env {
        Env::from_pairs(pairs.iter().copied())
    }

    // covers: C-14
    #[test]
    fn a_ref_that_names_no_workflow_cant_be_told() {
        let why = find_on_github(
            &env(&[
                ("GITHUB_WORKFLOW_REF", "@refs/heads/main"),
                ("GITHUB_JOB", "test"),
                ("GITHUB_REPOSITORY", "acme/shop"),
            ]),
            &|_| None,
        )
        .expect_err("no pass skips");
        assert!(why.why.contains("can't tell"), "{why:?}");
        assert_eq!(why.wall, Wall::CantTell);
    }

    /// `jobs.test` of a workflow `text`, at `.github/workflows/ci.yml`, beside `others`.
    fn decided(text: &str, others: &[(&str, &str)], named: bool) -> Option<NoSkip> {
        let files: BTreeMap<String, String> = std::iter::once((".github/workflows/ci.yml", text))
            .chain(others.iter().copied())
            .map(|(path, text)| (path.to_string(), text.to_string()))
            .collect();
        let read = |path: &str| files.get(path).map(|text| text.as_bytes().to_vec());
        no_skip(
            &env(&[
                ("GITHUB_ACTIONS", "true"),
                (
                    "GITHUB_WORKFLOW_REF",
                    "acme/shop/.github/workflows/ci.yml@refs/heads/main",
                ),
                ("GITHUB_JOB", "test"),
                ("GITHUB_REPOSITORY", "acme/shop"),
            ]),
            &read,
            "unit",
            named,
        )
    }

    // covers: C-14
    #[test]
    fn each_reason_names_its_wall() {
        let job = |more: &str| format!("on: push\njobs:\n  test:\n    runs-on: x\n{more}");
        assert_eq!(decided(&job("    steps: []\n"), &[], true), None);
        let matrix = decided(
            &job("    strategy:\n      matrix:\n        os: [a, b]\n"),
            &[],
            true,
        )
        .expect("no pass skips");
        assert_eq!(matrix.wall, Wall::Variants);
        assert!(
            matrix
                .why
                .starts_with("job test of .github/workflows/ci.yml has several variants"),
            "{matrix:?}"
        );
        let called = decided(
            "on: push\njobs:\n  other:\n    uses: ./.github/workflows/b.yml\n",
            &[],
            true,
        )
        .expect("no pass skips");
        assert_eq!(called.wall, Wall::Reusable);
        assert_eq!(
            called.why,
            ".github/workflows/ci.yml has no job test, as with a reusable workflow's, so hushrun can't tell its variants, and no pass skips it"
        );
        let calls_one = decided(
            &format!(
                "{}  call:\n    uses: ./.github/workflows/b.yml\n",
                job("    steps: []\n")
            ),
            &[(
                ".github/workflows/b.yml",
                "on: workflow_call\njobs:\n  test:\n    runs-on: x\n",
            )],
            true,
        )
        .expect("no pass skips");
        assert_eq!(calls_one.wall, Wall::Reusable, "{calls_one:?}");
        let unreadable = decided("jobs: [", &[], true).expect("no pass skips");
        assert_eq!(unreadable.wall, Wall::CantTell);
        assert_eq!(
            unreadable.why,
            ".github/workflows/ci.yml can't be read as YAML, so hushrun can't tell its variants, and no pass skips it"
        );
        assert_eq!(Wall::Variants.word(), "variants");
        assert_eq!(Wall::Reusable.word(), "reusable");
        assert_eq!(Wall::CantTell.word(), "cant-tell");
    }

    // covers: C-14
    #[test]
    fn gitlab_says_which_jobs_have_instances() {
        let several = |pairs| on_gitlab(&env(pairs)).is_some();
        assert!(several(&[("CI_NODE_TOTAL", "3")]));
        assert!(several(&[("CI_JOB_NAME", "test: [ubuntu, 20]")]));
        assert!(several(&[
            ("CI_NODE_TOTAL", "2"),
            ("CI_JOB_NAME", "test 1/2")
        ]));
        assert!(!several(&[("CI_NODE_TOTAL", "1"), ("CI_JOB_NAME", "test")]));
        assert!(!several(&[("CI_JOB_NAME", "build:linux")]));
        assert!(!several(&[("CI_NODE_TOTAL", "many")]));
        assert!(!several(&[]));
    }

    // covers: C-14
    #[test]
    fn only_this_repositorys_workflows_are_read() {
        let own = |workflow| {
            own_workflow(&env(&[("GITHUB_REPOSITORY", "acme/shop")]), workflow).map(str::to_string)
        };
        let path = ".github/workflows/ci.yml";
        assert_eq!(
            own("acme/shop/.github/workflows/ci.yml").as_deref(),
            Some(path)
        );
        assert_eq!(
            own("Acme/Shop/.github/workflows/ci.yml").as_deref(),
            Some(path)
        );
        assert_eq!(own("acme/rules/.github/workflows/ci.yml"), None);
        assert_eq!(own("acme/shop/../../etc/passwd"), None);
        assert_eq!(own("acme/shop/.github/workflows/../../secret"), None);
        assert_eq!(own("acme/shop"), None);
        assert_eq!(own("acme/shop/ci.yml"), None);
        assert_eq!(own("acme/shop/.github/ci.yml"), None);
        assert_eq!(own("acme/shop/.github/workflows-old/ci.yml"), None);
        // `@refs/heads/main`, with nothing before it.
        assert_eq!(own(""), None);
        assert_eq!(own("é/é/.github/workflows/ci.yml"), None);
    }

    // covers: C-26
    #[test]
    fn a_readers_path_names_what_it_reads_from_the_root() {
        let names = |path: &str, file: &str| {
            let (itself, below) = from_the_root(path).expect(path);
            itself.is_match(file) || below.is_match(file)
        };
        assert!(names("coverage.xml", "coverage.xml"));
        assert!(names("./coverage.xml", "coverage.xml"));
        assert!(names("coverage/", "coverage/lcov.info"));
        assert!(names("test-results", "test-results/unit/a.xml"));
        assert!(names("**/TEST-*.xml", "a/b/TEST-x.xml"));
        assert!(names("**/TEST-*.xml", "TEST-x.xml"));
        assert!(names(
            "${{ github.workspace }}/coverage.xml",
            "coverage.xml"
        ));
        assert!(names("pkg/../coverage.xml", "coverage.xml"));
        assert!(!names("coverage.xml", "pkg/coverage.xml"));
        assert!(!names("coverage/", "coverage.xml"));
        // What hushrun can't follow.
        for path in [
            "${{ env.REPORT }}",
            "${{ github.workspace }}",
            "reports/${{ matrix.os }}.xml",
            "$HOME/coverage.xml",
            "/tmp/coverage.xml",
            "~/coverage.xml",
            "C:\\coverage.xml",
            "../coverage.xml",
            ".",
            "./",
            "{a,b}.xml",
        ] {
            assert!(from_the_root(path).is_none(), "{path}");
        }
    }

    // covers: C-26
    #[test]
    fn a_reader_hushrun_cant_bound_reads_whatever_a_pass_wrote() {
        let reader = |kinds: Vec<readers::Kind>, paths: &[&str], bounded: bool| readers::Reader {
            step: 2,
            name: "r".into(),
            kinds,
            paths: paths.iter().map(ToString::to_string).collect(),
            only_on_failure: false,
            bounded,
        };
        let file = |path: &str| hushrun_reports::File {
            path: path.into(),
            kind: "tests".into(),
            format: "junit".into(),
        };
        let junit = [file("junit.xml")];
        let tests = || vec![readers::Kind::Tests];
        assert!(reads_any(&reader(tests(), &["junit.xml"], true), &junit));
        assert!(!reads_any(
            &reader(tests(), &["coverage.xml"], true),
            &junit
        ));
        assert!(!reads_any(&reader(tests(), &["coverage.xml"], true), &[]));
        // It looks for more than it names, or names nothing.
        assert!(reads_any(&reader(tests(), &["coverage.xml"], false), &[]));
        assert!(reads_any(&reader(tests(), &[], false), &[]));
        // It reads other reports, which no summary names.
        let other = vec![readers::Kind::Other];
        assert!(reads_any(&reader(other, &["results.sarif"], true), &[]));
        // One of its paths can't be followed.
        assert!(reads_any(
            &reader(tests(), &["coverage.xml", "${{ env.OUT }}"], true),
            &junit
        ));
    }

    // covers: C-14
    #[test]
    fn a_variants_key_is_its_values_whatever_their_order_and_spaces() {
        let key = |text: &str| variant_of(text).expect(text).key;
        let a = key(r#"{"os": "ubuntu-latest", "node": 20}"#);
        assert_eq!(a, key(r#"{"node":20,"os":"ubuntu-latest"}"#));
        assert_eq!(a, key("{\n  \"node\": 20,\n  \"os\": \"ubuntu-latest\"\n}"));
        assert!(a.starts_with("sha256:") && a.len() == 71, "{a}");
        // Numbers as JCS writes them: 20 and 20.0 are one value, 20 and "20" two.
        assert_eq!(a, key(r#"{"node": 20.0, "os": "ubuntu-latest"}"#));
        assert_ne!(a, key(r#"{"node": "20", "os": "ubuntu-latest"}"#));
        assert_ne!(a, key(r#"{"node": 22, "os": "ubuntu-latest"}"#));
        // An `include` object, nested.
        let nested = key(r#"{"os": "linux", "target": {"arch": "arm64", "abi": "gnu"}}"#);
        assert_eq!(
            nested,
            key(r#"{"target": {"abi": "gnu", "arch": "arm64"}, "os": "linux"}"#)
        );
        // `null`, a job without a matrix, is `{}`.
        assert_eq!(key("null"), key("{}"));
        // Anything else names no variant.
        for bad in ["", "[1, 2]", "\"linux\"", "20", "{", "true"] {
            assert_eq!(variant_of(bad), None, "{bad}");
        }
        assert_eq!(
            variant_of(&format!("{{\"a\": \"{}\"}}", "x".repeat(70_000))),
            None
        );
    }

    // covers: C-14
    #[test]
    fn a_variants_label_is_its_values_in_key_order() {
        let label = |text: &str| variant_of(text).expect(text).label;
        assert_eq!(
            label(r#"{"os": "ubuntu-latest", "node": 20}"#),
            "20, ubuntu-latest"
        );
        assert_eq!(
            label(r#"{"b": true, "a": null, "c": [1, "x"], "d": {"e": 1}}"#),
            r#"null, true, [1,"x"], {"e":1}"#
        );
        assert_eq!(label("null"), "");
        let long = label(&format!(r#"{{"a": "{}"}}"#, "é".repeat(150)));
        assert_eq!(long.chars().count(), 100);
        assert!(long.ends_with('…'), "{long}");
    }

    // covers: C-14
    #[test]
    fn a_job_sets_its_variant_with_the_expression_only() {
        let job = |value: &str| {
            let text = format!("runs-on: x\nenv:\n  HUSHRUN_VARIANT: {value}\n");
            parse("job.yml", text.as_bytes()).unwrap()
        };
        for good in [
            "${{ toJSON(matrix) }}",
            "${{toJSON(matrix)}}",
            "'${{ tojson( matrix ) }}'",
            "\"${{ TOJSON(MATRIX) }}\"",
        ] {
            assert!(sets_variant(&job(good)), "{good}");
        }
        for bad in [
            "${{ toJSON(matrix.os) }}",
            "${{ toJSON(strategy) }}",
            "${{ matrix }}",
            "toJSON(matrix)",
            "x${{ toJSON(matrix) }}",
            "${{ toJSON(matrix) }}x",
        ] {
            assert!(!sets_variant(&job(bad)), "{bad}");
        }
        let flow = parse("job.yml", b"runs-on: x\nenv: ${{ fromJSON(inputs.env) }}\n").unwrap();
        assert!(!sets_variant(&flow));
    }

    // covers: C-14
    #[test]
    fn a_gitlab_instance_is_its_own_variant() {
        let env = |pairs: &[(&'static str, &'static str)]| Env::from_pairs(pairs.to_vec());
        let one = on_gitlab(&env(&[("CI_JOB_NAME", "test"), ("CI_NODE_TOTAL", "1")]));
        assert_eq!(one, None);
        let first = on_gitlab(&env(&[
            ("CI_JOB_NAME", "test 1/3"),
            ("CI_NODE_TOTAL", "3"),
            ("CI_NODE_INDEX", "1"),
        ]))
        .unwrap();
        let second = on_gitlab(&env(&[
            ("CI_JOB_NAME", "test 2/3"),
            ("CI_NODE_TOTAL", "3"),
            ("CI_NODE_INDEX", "2"),
        ]))
        .unwrap();
        assert_eq!(first.label, "1/3");
        assert_ne!(first.key, second.key);
        let matrix = on_gitlab(&env(&[
            ("CI_JOB_NAME", "test: [linux, 20]"),
            ("CI_NODE_TOTAL", "4"),
        ]))
        .unwrap();
        assert_eq!(matrix.label, "linux, 20");
    }
}
