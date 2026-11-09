//! The CI files that run a repository's checks, GitHub Actions workflows and GitLab's pipeline,
//! what they run, and the edits init makes to them (C-15): `hushrun run` in front of a check, what
//! installs it, and `id-token: write` for each GitHub job that runs one, so that `hushrun run`
//! there records results with the job's own ID token, no secret.
//!
//! \[ADR 0037\] They are also how `hushrun ready` knows a repository's checks in any language
//! (C-22): CI's own files list them, so there is no second list to keep in step. What a job needs
//! that this machine doesn't have says that its checks stay with CI, so that doing what `ready`
//! says always ends.
//!
//! A file is read and edited as lines, never parsed and written back: comments, quotes and the
//! layout of everything init doesn't touch keep their bytes. It reads what these files hold in
//! practice (block mappings and sequences, plain, quoted and block scalars); a job written in
//! flow style, `test: {runs-on: …}`, is one init doesn't see.

use std::path::Path;

use super::variants::{self, Matrix};
use super::{readers, report, scripts};

/// Which CI reads a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    GitHub,
    GitLab,
}

/// A CI file.
#[derive(Debug)]
pub struct Workflow {
    /// The file, relative to the repository root, `/`-separated.
    pub path: String,
    pub host: Host,
    text: String,
    lines: Vec<Line>,
    pub jobs: Vec<Job>,
    permissions: Option<Permissions>,
    newline: &'static str,
    /// One level of indentation.
    unit: usize,
    triggers: Triggers,
    /// The workflow's `defaults.run.working-directory`.
    default_dir: Option<String>,
    /// Why nothing in this file can run elsewhere, when something file-wide says so.
    only_ci: Option<String>,
    /// \[ADR 0049\] The file reads as YAML, which a GitHub workflow must to run: a job's variants
    /// and its env are known only then.
    pub reads_as_yaml: bool,
    /// \[ADR 0050\] It is a composite action of this repository that a job uses, `uses: ./<dir>`,
    /// not a workflow: it has no jobs, and these are its steps.
    pub composite: Option<Composite>,
}

/// \[ADR 0050\] What a composite action's `runs.steps` hold, as a job's steps are read.
#[derive(Debug, Default)]
pub struct Composite {
    /// Its steps that run shell commands, each with its own `shell:`.
    pub steps: Vec<Step>,
    /// Its steps that use another action of this repository.
    pub uses: Vec<ActionUse>,
}

/// \[ADR 0050\] A step that uses an action of this repository, `uses: ./<dir>`.
#[derive(Debug, Clone)]
pub struct ActionUse {
    /// The action's directory from the root, `/`-separated; or why it isn't in this repository's
    /// checkout.
    pub dir: Result<String, String>,
    /// The step's line, which what goes before it goes above, and where its `-` is.
    pub line: usize,
    pub dash: usize,
    /// Where it is among its job's, or its action's, steps, from 0.
    pub index: usize,
    /// Why it needs CI: a condition, or a secret in its `env`.
    pub only_ci: Option<String>,
}

/// \[ADR 0050\] How deep in a job's actions init wraps a line: its own actions', then theirs.
/// Every action a job reaches is read, deeper ones too, so that every job that runs a line init
/// wraps is known, and gets what it needs.
pub const ACTION_DEPTH: usize = 2;

/// \[ADR 0050\] A step of a composite action of this repository that a job runs, through its own
/// step that uses the action, and perhaps another action that uses this one.
#[derive(Debug, Clone)]
pub struct InAction<'a> {
    /// The action the step is in, an index into the files read, and the file.
    pub file: usize,
    pub action: &'a Workflow,
    pub step: &'a Step,
    /// The job's step that uses the outermost action, which hushrun's setup goes above.
    pub uses: &'a ActionUse,
    /// The actions it runs inside, innermost first, each with its step that runs it or uses the
    /// next: the step's own action, then the one that uses it, if any.
    pub inside: Vec<(&'a Workflow, usize)>,
}

/// What starts a GitHub workflow, as far as a push goes: `on:`.
#[derive(Debug, Default)]
struct Triggers {
    /// `on:` was there, and read.
    read: bool,
    pull_request: bool,
    push: bool,
    /// `on.push.branches`, and `branches-ignore`.
    branches: Vec<String>,
    ignored: Vec<String>,
    /// `on.push.tags` with no `branches`: a branch's push doesn't start it.
    tags_only: bool,
    /// `paths` or `paths-ignore`, on a push or a pull request: whether it runs depends on what
    /// changed.
    paths: bool,
}

/// Whether CI runs a file's jobs for a push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runs {
    Yes,
    /// Perhaps: why hushrun can't tell.
    Perhaps(String),
    No,
}

#[derive(Debug, Clone)]
struct Line {
    /// Where the line starts, and ends without its line break.
    start: usize,
    end: usize,
    /// Where the next line starts.
    next: usize,
    indent: usize,
    kind: LineKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Blank,
    Comment,
    Content,
    /// A line of a block scalar's text (`run: |`).
    Text,
}

/// A job, and the scripts its steps run.
#[derive(Debug)]
pub struct Job {
    pub id: String,
    /// Its key's line.
    header: usize,
    /// Its own keys' indentation.
    child_indent: Option<usize>,
    permissions: Option<Permissions>,
    /// The last line of its `runs-on`.
    runs_on_end: Option<usize>,
    /// What its `runs-on` says.
    runs_on: String,
    /// The scripts its steps run through a package manager, at the repository root.
    pub scripts: Vec<String>,
    /// Its steps that run shell commands.
    pub steps: Vec<Step>,
    /// Why what it runs needs CI: services, a container, a secret, a condition.
    only_ci: Option<String>,
    /// It runs several variants of its steps, which a run's key doesn't tell apart (C-9).
    pub matrix: bool,
    /// \[ADR 0045\] Its `runs-on` says it can only be on a Mac. GitLab's jobs never do: a runner's
    /// tags are whatever its owner chose.
    pub on_macos: bool,
    /// \[ADR 0045\] It is another workflow, `uses:` in place of its steps: what that runs isn't here.
    pub calls_a_workflow: bool,
    /// \[ADR 0049\] The file of this repository it calls, `uses: ./<file>`, from the root.
    pub calls: Option<String>,
    /// \[ADR 0049\] Its `env` key's line, and where its block ends.
    env_at: Option<(usize, usize)>,
    /// \[ADR 0045\] A step uses an action of this repository, which can run anything, `hushrun run`
    /// included.
    pub uses_a_local_action: bool,
    /// \[ADR 0050\] Its steps that use an action of this repository.
    pub actions: Vec<ActionUse>,
    /// \[ADR 0050\] A composite action of this repository it uses runs a check through hushrun
    /// already.
    pub actions_run_hushrun: bool,
    /// hushrun's already installed by a step of its own.
    pub installs_hushrun: bool,
    /// \[ADR 0055\] Its `if:` may keep it from running for a push: it reads more than other
    /// jobs' outputs and results.
    pub condition: bool,
}

impl Job {
    /// Why what this job runs can't run here, when it can't.
    pub fn only_ci(&self) -> Option<String> {
        if let Some(why) = &self.only_ci {
            return Some(why.clone());
        }
        let on = self.runs_on.to_ascii_lowercase();
        let other = (on.contains("windows") && !cfg!(windows))
            || (on.contains("macos") && !cfg!(target_os = "macos"));
        other.then(|| format!("its job runs on {}", self.runs_on))
    }
}

/// A step's shell commands.
#[derive(Debug)]
pub struct Step {
    /// The line it starts on, which what goes before it goes above.
    pub line: usize,
    /// Where its `-` is.
    pub dash: usize,
    /// The commands, as a shell reads them.
    pub commands: String,
    /// The same, line by line, where init can edit them: none when it can't.
    pub sources: Vec<Source>,
    /// The directory they start in, relative to the repository root, `""` at the root; or why
    /// that isn't this repository's, or can't be told.
    pub dir: Result<String, String>,
    /// Why it needs CI.
    pub only_ci: Option<String>,
    /// \[ADR 0050\] Where it is among its job's steps, from 0: its `steps[index]` in the file
    /// read as YAML.
    pub index: usize,
}

/// Why a step's directory can't be told: only run time knows it, and it is in the checkout.
const EXPRESSION_DIR: &str = "its working directory depends on an expression";

impl Step {
    /// Why its commands don't run in this repository's checkout, when they don't: its job checks
    /// nothing out, or its directory is outside the checkout. One only run time knows is in it.
    pub fn outside_the_checkout(&self) -> Option<&str> {
        match &self.dir {
            Err(why) if why != EXPRESSION_DIR => Some(why),
            _ => None,
        }
    }
}

/// One line of shell commands in a file, with the lines it goes on to when it ends in `\`, or
/// \[ADR 0055\] that a folded block (`run: >`) folds into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub line: usize,
    /// Where the commands start in the line, and in those it goes on to.
    pub at: usize,
    /// The commands, as many lines as they take, each line break kept: a shell reads them as one.
    pub text: String,
    /// \[ADR 0055\] Where in `text` the line breaks are that a folded block makes spaces.
    pub folds: Vec<usize>,
    /// \[ADR 0055\] The kind of YAML scalar the commands are in, which says what an edit can add.
    pub scalar: Scalar,
    /// \[ADR 0055\] What a `# hushrun: leave` comment says of these commands, which init leaves
    /// as they are: why, as the comment gives it, empty when it gives none.
    pub leave: Option<String>,
}

/// \[ADR 0055\] How a line's YAML holds its commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    /// As they are, on the key's line: `: ` and ` #` would end them.
    Plain,
    /// Between `'`s, which one more would end.
    Single,
    /// Between `"`s, where `"` and `\\` mean something else.
    Double,
    /// In a block, `|` or `>`, which holds any text on a line.
    Block,
}

impl Source {
    /// One line of commands in a scalar of kind `scalar`, which goes on to no other.
    fn one(line: usize, at: usize, text: String, scalar: Scalar) -> Source {
        Source {
            line,
            at,
            text,
            folds: Vec::new(),
            scalar,
            leave: None,
        }
    }

    /// \[ADR 0055\] The commands as the shell gets them: a folded line break is a space. Each
    /// offset in it is the same in `text`, so [`Source::place`] finds it in the file.
    pub fn shell(&self) -> std::borrow::Cow<'_, str> {
        if self.folds.is_empty() {
            return std::borrow::Cow::Borrowed(&self.text);
        }
        let mut bytes = self.text.clone().into_bytes();
        for at in &self.folds {
            bytes[*at] = b' ';
        }
        // A line break and a space are each one byte: the text stays UTF-8.
        std::borrow::Cow::Owned(String::from_utf8(bytes).unwrap_or_else(|_| self.text.clone()))
    }

    /// The line of the file that holds `offset`, an offset in `text`, and where in it.
    pub fn place(&self, offset: usize) -> (usize, usize) {
        let before = &self.text[..offset];
        match before.rfind('\n') {
            Some(newline) => (
                self.line + before.matches('\n').count(),
                self.at + offset - newline - 1,
            ),
            None => (self.line, self.at + offset),
        }
    }
}

/// A `permissions:` key, and its value: on its line, or a mapping below it.
#[derive(Debug, Clone)]
struct Permissions {
    line: usize,
    indent: usize,
    /// The value on the key's line, without a comment: `read-all`, `{ contents: read }`.
    inline: Option<String>,
    /// The mapping's entries: their line, key and value.
    entries: Vec<(usize, String, String)>,
}

/// What init changes in a CI file beside the ID token: where `hushrun run` goes in lines, and
/// what goes above a line.
#[derive(Debug, Default)]
pub struct Changes {
    /// A line, where in it, and what goes there.
    pub inserts: Vec<(usize, usize, String)>,
    /// A line, and the lines that go above it.
    pub before: Vec<(usize, Vec<String>)>,
    /// \[ADR 0049\] A line, and the lines that go below it.
    pub after: Vec<(usize, Vec<String>)>,
    /// What the plan says of them.
    pub said: Vec<String>,
    /// \[ADR 0049\] What a person changes by hand, and why.
    pub manual: Vec<String>,
}

impl Changes {
    /// Whether they change nothing, and say nothing for a person to change.
    pub fn is_empty(&self) -> bool {
        self.inserts.is_empty()
            && self.before.is_empty()
            && self.after.is_empty()
            && self.manual.is_empty()
    }
}

/// \[ADR 0047\] What a GitLab job needs to say it reads reports.
enum ReportsRead {
    /// The lines to put after a line.
    Add(usize, Vec<String>),
    /// It says so already, or says otherwise on purpose.
    Has,
    /// Why init can't write it, for a person to.
    Manual(String),
}

/// What init does to a workflow.
#[derive(Debug, Default)]
pub struct Edit {
    /// The new text, when it changes.
    pub text: Option<String>,
    /// What changed, for the plan.
    pub changes: Vec<String>,
    /// What to change by hand, and why.
    pub manual: Vec<String>,
}

/// The workflows in `.github/workflows/` under `root`, by name. Files that can't be read are
/// left out.
pub fn find(root: &Path) -> Vec<Workflow> {
    let dir = root.join(".github").join("workflows");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("yml" | "yaml")
                )
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let name = path.file_name()?.to_str()?;
            Some(Workflow::parse(format!(".github/workflows/{name}"), text))
        })
        .collect()
}

/// GitLab's one pipeline definition.
pub const GITLAB_CI: &str = ".gitlab-ci.yml";

/// Every CI file under `root`, \[ADR 0050\] and the composite actions of the repository its
/// GitHub jobs use.
pub fn pipelines(root: &Path) -> Vec<Workflow> {
    let mut files = find(root);
    if let Ok(text) = std::fs::read_to_string(root.join(GITLAB_CI)) {
        files.push(Workflow::parse_gitlab(text));
    }
    with_actions(files, |path| std::fs::read_to_string(root.join(path)).ok())
}

/// \[ADR 0050\] `files`, and after them the composite actions of this repository their jobs use,
/// `uses: ./<dir>`, then those these use, and so on, read with `read`.
fn with_actions(mut files: Vec<Workflow>, read: impl Fn(&str) -> Option<String>) -> Vec<Workflow> {
    let mut next: Vec<String> = files
        .iter()
        .filter(|file| file.host == Host::GitHub)
        .flat_map(|file| &file.jobs)
        .flat_map(|job| &job.actions)
        .filter_map(|uses| uses.dir.clone().ok())
        .collect();
    let mut tried = std::collections::BTreeSet::new();
    while let Some(dir) = next.pop() {
        if !tried.insert(dir.clone()) {
            continue;
        }
        let found = ["action.yml", "action.yaml"].iter().find_map(|name| {
            let path = match dir.as_str() {
                "" => (*name).to_string(),
                dir => format!("{dir}/{name}"),
            };
            read(&path).map(|text| (path, text))
        });
        let Some(action) = found.and_then(|(path, text)| Workflow::parse_action(path, text)) else {
            continue;
        };
        if let Some(composite) = &action.composite {
            next.extend(composite.uses.iter().filter_map(|u| u.dir.clone().ok()));
        }
        files.push(action);
    }
    // What a job's actions run, the job runs: the scripts at the root, and checks through
    // hushrun. A job's `defaults.run` doesn't reach an action's steps.
    let mut ran = Vec::new();
    for (index, file) in files.iter().enumerate() {
        for (at, job) in file.jobs.iter().enumerate() {
            let steps = in_actions(job, &files);
            let scripts: Vec<String> = steps
                .iter()
                .filter(|step| step.step.dir.as_deref() == Ok(""))
                .flat_map(|step| scripts::scripts_run(&step.step.commands))
                .collect();
            let hushrun = steps
                .iter()
                .any(|step| !scripts::line_runs(&step.step.commands).is_empty());
            ran.push((index, at, scripts, hushrun));
        }
    }
    for (index, at, scripts, hushrun) in ran {
        let job = &mut files[index].jobs[at];
        for script in scripts {
            if !job.scripts.contains(&script) {
                job.scripts.push(script);
            }
        }
        job.actions_run_hushrun = hushrun;
    }
    files
}

/// \[ADR 0050\] The composite action in `dir`, among `files`, with its index.
pub fn action_at<'a>(files: &'a [Workflow], dir: &str) -> Option<(usize, &'a Workflow)> {
    files.iter().enumerate().find(|(_, file)| {
        file.composite.is_some()
            && ["action.yml", "action.yaml"].iter().any(|name| {
                file.path
                    == match dir {
                        "" => (*name).to_string(),
                        dir => format!("{dir}/{name}"),
                    }
            })
    })
}

/// \[ADR 0050\] The steps of this repository's composite actions that `job` runs, in the order
/// it runs them, through its steps that use one, and the actions those use, however deep: a
/// step's `inside` says how deep it is. An action that isn't among `files`, or doesn't run its
/// steps itself, adds none, and one that uses itself, through others or not, is walked once.
pub fn in_actions<'a>(job: &'a Job, files: &'a [Workflow]) -> Vec<InAction<'a>> {
    fn walk<'a>(
        files: &'a [Workflow],
        dir: &str,
        uses: &'a ActionUse,
        outer: &[(&'a Workflow, usize)],
        found: &mut Vec<InAction<'a>>,
    ) {
        let Some((index, action)) = action_at(files, dir) else {
            return;
        };
        let Some(composite) = &action.composite else {
            return;
        };
        let mut steps: Vec<(usize, Option<&Step>, Option<&ActionUse>)> = composite
            .steps
            .iter()
            .map(|step| (step.index, Some(step), None))
            .chain(
                composite
                    .uses
                    .iter()
                    .map(|inner| (inner.index, None, Some(inner))),
            )
            .collect();
        steps.sort_by_key(|(at, _, _)| *at);
        for (at, step, inner) in steps {
            let mut inside = vec![(action, at)];
            inside.extend(outer.iter().copied());
            match (step, inner) {
                (Some(step), _) => found.push(InAction {
                    file: index,
                    action,
                    step,
                    uses,
                    inside,
                }),
                (None, Some(inner)) => {
                    let again = |dir: &str| {
                        action_at(files, dir).is_some_and(|(_, next)| {
                            inside.iter().any(|(seen, _)| std::ptr::eq(*seen, next))
                        })
                    };
                    if let Ok(dir) = &inner.dir
                        && !again(dir)
                    {
                        walk(files, dir, uses, &inside, found);
                    }
                }
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    for uses in &job.actions {
        if let Ok(dir) = &uses.dir {
            walk(files, dir, uses, &[], &mut found);
        }
    }
    found
}

/// Every CI file among `paths`, a listing's, with `read` reading one: so that `hushrun ready`
/// reads the revision it looks at.
pub fn pipelines_among<'a>(
    paths: impl Iterator<Item = &'a str>,
    read: impl Fn(&str) -> Option<String>,
) -> Vec<Workflow> {
    let mut files = Vec::new();
    let mut gitlab = None;
    for path in paths {
        if path == GITLAB_CI {
            gitlab = read(path);
            continue;
        }
        let in_workflows = path
            .strip_prefix(".github/workflows/")
            .is_some_and(|name| !name.contains('/'));
        if in_workflows
            && (path.ends_with(".yml") || path.ends_with(".yaml"))
            && let Some(text) = read(path)
        {
            files.push(Workflow::parse(path.to_string(), text));
        }
    }
    files.extend(gitlab.map(Workflow::parse_gitlab));
    with_actions(files, read)
}

impl Workflow {
    pub fn parse(path: String, text: String) -> Workflow {
        let mut workflow = Workflow::empty(path, Host::GitHub, text);
        workflow.read();
        workflow.read_variants();
        workflow
    }

    /// Which jobs have several variants, and which can only be on a Mac, as the file says read as
    /// YAML, which sees what lines can't: anchors and flow style. A file that doesn't parse can't
    /// say, so each job may have variants, and none is sure to be on a Mac.
    fn read_variants(&mut self) {
        let file = variants::parse(&self.path, self.text.as_bytes());
        self.reads_as_yaml = file.is_some();
        for job in &mut self.jobs {
            job.matrix = file
                .as_ref()
                .is_none_or(|file| variants::matrix_of(&file["jobs"][&job.id]) != Matrix::One);
            job.on_macos = file
                .as_ref()
                .is_some_and(|file| variants::on_macos(&file["jobs"][&job.id]));
        }
    }

    /// \[ADR 0050\] A composite action of this repository, `action.yml` at `path`: its
    /// `runs.steps`, read as a job's are. `None` for another kind of action, which runs no steps
    /// of its own.
    pub fn parse_action(path: String, text: String) -> Option<Workflow> {
        let mut action = Workflow::empty(path, Host::GitHub, text);
        action.reads_as_yaml = variants::parse(&action.path, action.text.as_bytes()).is_some();
        let mut i = 0;
        while i < action.lines.len() {
            if action.lines[i].kind != LineKind::Content || action.lines[i].indent != 0 {
                i += 1;
                continue;
            }
            let end = action.block(i + 1, 0);
            if entry(action.text(i), 0).key.as_deref() == Some("runs") {
                action.composite = action.read_runs(i + 1, end);
            }
            i = end;
        }
        action.composite.is_some().then_some(action)
    }

    /// \[ADR 0050\] An action's `runs:` mapping, from `from` to `to`, when it says `using:
    /// composite`. A composite step runs in the workspace, or its own `working-directory`, which
    /// counts from there, as a job's does.
    fn read_runs(&self, from: usize, to: usize) -> Option<Composite> {
        let first = self.first_content(from, to)?;
        let indent = self.lines[first].indent;
        let mut composite = false;
        let mut read = Vec::new();
        let mut i = first;
        while i < to {
            let line = &self.lines[i];
            if line.kind != LineKind::Content || line.indent != indent {
                i += 1;
                continue;
            }
            let mut key_end = self.block(i + 1, indent);
            let found = entry(self.text(i), indent);
            match found.key.as_deref() {
                Some("using") => {
                    composite = unquote(&found.value).eq_ignore_ascii_case("composite");
                }
                Some("steps") => {
                    key_end = self.steps_end(i + 1, to, indent);
                    read = self.steps(i + 1, key_end);
                }
                _ => {}
            }
            i = key_end;
        }
        if !composite {
            return None;
        }
        let workspace = String::new();
        let uses = read
            .iter()
            .filter_map(|step| step.action_use(Some(&workspace)))
            .collect();
        let steps = read
            .into_iter()
            .filter_map(|step| {
                let dir = step.dir.clone();
                step.finish(Some(&workspace), dir.as_deref())
            })
            .collect();
        Some(Composite { steps, uses })
    }

    /// `.gitlab-ci.yml`.
    pub fn parse_gitlab(text: String) -> Workflow {
        let mut workflow = Workflow::empty(GITLAB_CI.to_string(), Host::GitLab, text);
        workflow.read_gitlab();
        workflow
    }

    fn empty(path: String, host: Host, text: String) -> Workflow {
        Workflow {
            path,
            host,
            newline: if text.contains("\r\n") { "\r\n" } else { "\n" },
            unit: 2,
            lines: lines(&text),
            text,
            jobs: Vec::new(),
            permissions: None,
            triggers: Triggers::default(),
            default_dir: None,
            only_ci: None,
            reads_as_yaml: true,
            composite: None,
        }
    }

    /// Whether CI runs this file's jobs for a push to `branch`.
    pub fn runs_for(&self, branch: Option<&str>) -> Runs {
        if self.host == Host::GitLab {
            return Runs::Yes;
        }
        let on = &self.triggers;
        if !on.read {
            return Runs::Perhaps("hushrun can't read what starts its workflow".into());
        }
        if on.paths {
            return Runs::Perhaps("its workflow runs only for some paths".into());
        }
        let pushed = on.push
            && !on.tags_only
            && match (branch, on.branches.is_empty(), on.ignored.is_empty()) {
                (_, true, true) => true,
                (None, _, _) => {
                    return Runs::Perhaps("its workflow runs only for some branches".into());
                }
                (Some(branch), true, false) => !on.ignored.iter().any(|p| matches(p, branch)),
                (Some(branch), false, _) => on.branches.iter().any(|p| matches(p, branch)),
            };
        if pushed || on.pull_request {
            Runs::Yes
        } else {
            Runs::No
        }
    }

    /// The file's text, as read.
    pub fn source(&self) -> &str {
        &self.text
    }

    /// The jobs whose steps run one of `wrapped`.
    pub fn running<'a>(&'a self, wrapped: &'a [String]) -> impl Iterator<Item = &'a Job> + 'a {
        self.jobs
            .iter()
            .filter(|job| job.scripts.iter().any(|s| wrapped.contains(s)))
    }

    fn text(&self, line: usize) -> &str {
        let l = &self.lines[line];
        &self.text[l.start..l.end]
    }

    /// The content lines from `from` on while their indentation is above `indent`.
    fn block(&self, from: usize, indent: usize) -> usize {
        let mut end = from;
        while end < self.lines.len() {
            let line = &self.lines[end];
            if line.kind == LineKind::Content && line.indent <= indent {
                break;
            }
            end += 1;
        }
        end
    }

    /// Reads the top-level keys and the jobs, which need what the others say.
    fn read(&mut self) {
        let mut defaults_elsewhere = false;
        let mut jobs = None;
        let mut i = 0;
        while i < self.lines.len() {
            if self.lines[i].kind != LineKind::Content || self.lines[i].indent != 0 {
                i += 1;
                continue;
            }
            let end = self.block(i + 1, 0);
            let entry = entry(self.text(i), 0);
            match entry.key.as_deref() {
                Some("permissions") => self.permissions = Some(self.permissions_at(i, end)),
                Some("defaults") => {
                    defaults_elsewhere = self.moves_runs(i + 1, end);
                    self.default_dir = self.working_directory(i + 1, end);
                }
                Some("env") => {
                    self.only_ci = self.secret_in(i, end);
                }
                Some("on" | "true") => self.read_triggers(i, end),
                Some("jobs") => jobs = Some((i + 1, end)),
                _ => {}
            }
            i = end;
        }
        if let Some((from, to)) = jobs {
            self.read_jobs(from, to);
        }
        if defaults_elsewhere {
            // Every step runs in another directory: another package's scripts.
            for job in &mut self.jobs {
                job.scripts.clear();
            }
        }
    }

    fn read_jobs(&mut self, from: usize, to: usize) {
        let Some(job_indent) = self.first_content(from, to).map(|l| self.lines[l].indent) else {
            return;
        };
        let mut i = from;
        while i < to {
            let line = &self.lines[i];
            if line.kind != LineKind::Content || line.indent != job_indent {
                i += 1;
                continue;
            }
            let end = self.block(i + 1, job_indent);
            let header = entry(self.text(i), job_indent);
            if let (Some(id), false) = (header.key, header.dash) {
                let job = self.read_job(id, i, end);
                if let Some(child) = job.child_indent {
                    self.unit = child - job_indent;
                }
                self.jobs.push(job);
            }
            i = end;
        }
    }

    /// `on:`, as a word, a list or a mapping.
    fn read_triggers(&mut self, at: usize, end: usize) {
        let value = entry(self.text(at), 0).value;
        let mut names = flow_list(&value);
        let mut filters: Vec<(String, usize, usize)> = Vec::new();
        if value.trim().is_empty() {
            let indent = self
                .first_content(at + 1, end)
                .map_or(0, |line| self.lines[line].indent);
            let mut i = at + 1;
            while i < end {
                let line = &self.lines[i];
                if line.kind != LineKind::Content || line.indent != indent {
                    i += 1;
                    continue;
                }
                let key_end = self.block(i + 1, indent);
                if let Some(key) = entry(self.text(i), indent).key {
                    names.push(key.clone());
                    filters.push((key, i + 1, key_end));
                }
                i = key_end;
            }
        }
        let mut on = Triggers {
            read: !names.is_empty(),
            ..Triggers::default()
        };
        for name in &names {
            match name.as_str() {
                "push" => on.push = true,
                "pull_request" | "merge_group" => on.pull_request = true,
                _ => {}
            }
        }
        for (event, from, to) in filters {
            let mut has_branches = false;
            let mut has_tags = false;
            let mut i = from;
            while i < to {
                let line = &self.lines[i];
                if line.kind != LineKind::Content {
                    i += 1;
                    continue;
                }
                let indent = line.indent;
                let found = entry(self.text(i), indent);
                let key_end = self.block(i + 1, indent);
                let list = || self.list_at(i, &found.value, key_end);
                match (event.as_str(), found.key.as_deref()) {
                    ("push" | "pull_request", Some("paths" | "paths-ignore")) => on.paths = true,
                    ("push", Some("branches")) => {
                        on.branches = list();
                        has_branches = true;
                    }
                    ("push", Some("branches-ignore")) => {
                        on.ignored = list();
                        has_branches = true;
                    }
                    ("push", Some("tags" | "tags-ignore")) => has_tags = true,
                    _ => {}
                }
                i = key_end;
            }
            if event == "push" {
                on.tags_only = has_tags && !has_branches;
            }
        }
        self.triggers = on;
    }

    /// A key's list: on its line, or as a sequence below it.
    fn list_at(&self, at: usize, value: &str, end: usize) -> Vec<String> {
        if !value.trim().is_empty() {
            return flow_list(value);
        }
        (at + 1..end)
            .filter(|i| self.lines[*i].kind == LineKind::Content)
            .filter_map(|i| {
                let line = self.text(i).trim();
                line.strip_prefix("- ")
                    .map(|item| unquote(strip_comment(item)))
            })
            .collect()
    }

    /// The first variable of an `env:` block that a secret fills: why what reads it stays in CI.
    fn secret_in(&self, at: usize, end: usize) -> Option<String> {
        let inline = secret_of(self.text(at));
        inline
            .or_else(|| (at + 1..end).find_map(|i| secret_of(self.text(i))))
            .map(|name| format!("it needs secrets.{name}, which stays in CI"))
    }

    /// `working-directory:` in a `defaults:` block.
    fn working_directory(&self, from: usize, to: usize) -> Option<String> {
        (from..to).find_map(|i| {
            let line = &self.lines[i];
            if line.kind != LineKind::Content {
                return None;
            }
            let found = entry(self.text(i), line.indent);
            (found.key.as_deref() == Some("working-directory")).then(|| unquote(&found.value))
        })
    }

    fn read_job(&self, id: String, header: usize, end: usize) -> Job {
        let mut job = Job {
            id,
            header,
            child_indent: None,
            permissions: None,
            runs_on_end: None,
            runs_on: String::new(),
            scripts: Vec::new(),
            steps: Vec::new(),
            only_ci: self.only_ci.clone(),
            matrix: false,
            on_macos: false,
            calls_a_workflow: false,
            calls: None,
            env_at: None,
            uses_a_local_action: false,
            actions: Vec::new(),
            actions_run_hushrun: false,
            installs_hushrun: false,
            condition: false,
        };
        let Some(first) = self.first_content(header + 1, end) else {
            return job;
        };
        let indent = self.lines[first].indent;
        job.child_indent = Some(indent);
        let mut elsewhere = false;
        let mut default_dir = self.default_dir.clone();
        let mut read: Vec<StepRead> = Vec::new();
        let mut i = first;
        while i < end {
            let line = &self.lines[i];
            if line.kind != LineKind::Content || line.indent != indent {
                i += 1;
                continue;
            }
            let mut key_end = self.block(i + 1, indent);
            let found = entry(self.text(i), indent);
            let needs = |job: &mut Job, why: String| {
                job.only_ci.get_or_insert(why);
            };
            match found.key.as_deref() {
                Some("permissions") => job.permissions = Some(self.permissions_at(i, key_end)),
                Some("runs-on") => {
                    job.runs_on_end = self.last_content(i, key_end);
                    job.runs_on = unquote(&found.value);
                }
                Some("defaults") => {
                    elsewhere = self.moves_runs(i + 1, key_end);
                    default_dir = self.working_directory(i + 1, key_end).or(default_dir);
                }
                Some("uses") => {
                    job.calls_a_workflow = true;
                    let uses = unquote(&found.value);
                    job.calls = uses
                        .strip_prefix("./")
                        .map(|path| path.split('@').next().unwrap_or(path).to_string());
                }
                Some("services") => needs(&mut job, "its job has services".to_string()),
                Some("container") => needs(&mut job, "its job runs in a container".to_string()),
                // \[ADR 0055\] A condition on other jobs' outputs and results, as a changes filter
                // writes it, runs the job when they say so, such as when its area changed: the job
                // may run. Any other may keep it from running.
                Some("if") if !on_other_jobs(&self.scalar(i, &found, key_end)) => {
                    job.condition = true;
                    needs(&mut job, "its job has a condition".to_string());
                }
                Some("env") => {
                    job.env_at = Some((i, key_end));
                    if let Some(why) = self.secret_in(i, key_end) {
                        needs(&mut job, why);
                    }
                }
                Some("steps") => {
                    // A sequence may sit at its key's indentation.
                    key_end = self.steps_end(i + 1, end, indent);
                    read = self.steps(i + 1, key_end);
                }
                _ => {}
            }
            i = key_end;
        }
        job.scripts = read.iter().flat_map(|step| step.scripts.clone()).collect();
        if elsewhere {
            job.scripts.clear();
        }
        // Where the job checks this repository out, when it says: its working directories count
        // from there, and it may check out other repositories beside it.
        let checkout = read.iter().find_map(|step| step.checkout.clone());
        job.installs_hushrun = read.iter().any(|step| step.installs);
        job.uses_a_local_action = read.iter().any(|step| step.local_action);
        job.actions = read
            .iter()
            .filter_map(|step| step.action_use(checkout.as_ref()))
            .collect();
        job.steps = read
            .into_iter()
            .filter_map(|step| {
                let dir = step.dir.clone().or_else(|| default_dir.clone());
                step.finish(checkout.as_ref(), dir.as_deref())
            })
            .collect();
        job
    }

    /// Where the sequence of a key at `indent` ends, from `from` on.
    fn steps_end(&self, from: usize, to: usize, indent: usize) -> usize {
        let mut i = from;
        while i < to {
            let line = &self.lines[i];
            if line.kind == LineKind::Content
                && (line.indent < indent
                    || (line.indent == indent && !self.text(i)[indent..].starts_with('-')))
            {
                break;
            }
            i += 1;
        }
        i
    }

    /// What the steps from `from` to `end` run.
    fn steps(&self, from: usize, end: usize) -> Vec<StepRead> {
        let Some(first) = self.first_content(from, end) else {
            return Vec::new();
        };
        let dash = self.lines[first].indent;
        let mut steps = Vec::new();
        let mut i = first;
        while i < end {
            let line = &self.lines[i];
            if line.kind != LineKind::Content
                || line.indent != dash
                || !self.text(i)[dash..].starts_with('-')
            {
                i += 1;
                continue;
            }
            // One step: its dash line, then lines indented past the dash.
            let mut item_end = i + 1;
            while item_end < end {
                let l = &self.lines[item_end];
                if l.kind == LineKind::Content && l.indent <= dash {
                    break;
                }
                item_end += 1;
            }
            let mut read = self.step(i, item_end, dash);
            read.index = steps.len();
            steps.push(read);
            i = item_end;
        }
        steps
    }

    /// One step: its `run`, the scripts that runs, and what else decides where and whether.
    fn step(&self, from: usize, to: usize, dash: usize) -> StepRead {
        let first = entry(self.text(from), dash);
        let column = first.column;
        let mut run = None;
        let mut sources = Vec::new();
        // \[ADR 0055\] A `# hushrun: leave` comment right above the step, or at the end of its
        // `run:` line, leaves all of its commands.
        let mut leave = self.leave_above(from);
        let mut elsewhere = false;
        let mut dir = None;
        let mut only_ci = None;
        let mut uses = None;
        let mut with = (None, None);
        let mut plain_shell = true;
        let mut i = from;
        while i < to {
            let line = &self.lines[i];
            let at = if i == from { Some(first.clone()) } else { None };
            let entry = match at {
                Some(entry) => entry,
                None if line.kind == LineKind::Content && line.indent == column => {
                    entry(self.text(i), column)
                }
                None => {
                    i += 1;
                    continue;
                }
            };
            let key_end = self.block_after(i, column, to);
            match entry.key.as_deref() {
                Some("run") => {
                    run = Some(self.scalar(i, &entry, to));
                    sources = self.sources(i, &entry, to);
                    leave = leave.or_else(|| end_comment(self.text(i)).and_then(leave_marker));
                }
                Some("working-directory") => {
                    let value = unquote(&entry.value);
                    elsewhere = !matches!(value.trim(), "." | "./" | "");
                    dir = Some(value);
                }
                Some("uses") => uses = Some(unquote(&entry.value)),
                Some("with") => with = self.checkout_inputs(i, key_end),
                Some("if") => only_ci = Some("its step has a condition".to_string()),
                Some("env") => only_ci = self.secret_in(i, key_end).or(only_ci),
                Some("shell") => {
                    plain_shell = matches!(
                        unquote(&entry.value).split_whitespace().next(),
                        Some("bash" | "sh" | "pwsh" | "powershell" | "cmd")
                    );
                }
                _ => {}
            }
            i += 1;
        }
        if let Some(why) = &leave {
            for source in &mut sources {
                source.leave.get_or_insert_with(|| why.clone());
            }
        }
        let commands = run.clone();
        let needs_secret = commands.as_deref().and_then(secret_of);
        if let Some(name) = needs_secret {
            only_ci = Some(format!("it needs secrets.{name}, which stays in CI"));
        }
        // actions/checkout of this repository, and where to; of another, it says nothing.
        let (path, repository) = with;
        let checkout = uses
            .as_deref()
            .filter(|uses| uses.starts_with("actions/checkout@") && repository.is_none())
            .map(|_| path.unwrap_or_default());
        let installs = uses
            .as_deref()
            .is_some_and(|uses| uses.starts_with("hushrun/hushrun/action/setup"))
            || commands.as_deref().is_some_and(|text| {
                text.contains("hushrun-installer") || text.contains("install-hushrun")
            });
        StepRead {
            line: from,
            dash,
            index: 0,
            scripts: match (&run, elsewhere) {
                (Some(text), false) => scripts::scripts_run(text),
                _ => Vec::new(),
            },
            commands,
            sources: if plain_shell { sources } else { Vec::new() },
            dir,
            only_ci,
            checkout,
            installs,
            local_action: uses.as_deref().is_some_and(|uses| uses.starts_with("./")),
            action: uses
                .as_deref()
                .and_then(|uses| uses.strip_prefix("./"))
                .map(|path| path.trim_end_matches('/').to_string()),
        }
    }

    /// Where a key at `column` on line `at` ends, before `to`.
    fn block_after(&self, at: usize, column: usize, to: usize) -> usize {
        let mut end = at + 1;
        while end < to {
            let line = &self.lines[end];
            if line.kind == LineKind::Content && line.indent <= column {
                break;
            }
            end += 1;
        }
        end
    }

    /// `path` and `repository` in a step's `with:` block.
    fn checkout_inputs(&self, at: usize, end: usize) -> (Option<String>, Option<String>) {
        let mut inputs = (None, None);
        for i in at + 1..end {
            let line = &self.lines[i];
            if line.kind != LineKind::Content {
                continue;
            }
            let found = entry(self.text(i), line.indent);
            match found.key.as_deref() {
                Some("path") => inputs.0 = Some(unquote(&found.value)),
                Some("repository") => inputs.1 = Some(unquote(&found.value)),
                _ => {}
            }
        }
        inputs
    }

    /// The lines of the `run:` on line `at`, where init can edit them: its own line, or a block
    /// scalar's. A scalar that goes on over lines, or a quoted one with escapes, has none.
    fn sources(&self, at: usize, found: &Entry, to: usize) -> Vec<Source> {
        let value = found.value.trim();
        if let Some(style) = block_style(value) {
            let end = self.block_after(at, found.column, to);
            let mut indent = None;
            let mut sources: Vec<Source> = Vec::new();
            // The last line ended in `\`, so a shell reads this one as more of it.
            let mut goes_on = false;
            // \[ADR 0055\] The last line was at a folded block's indentation: YAML makes the line
            // break after it a space when this one is too, so a shell reads them as one line.
            let mut folds_on = false;
            // \[ADR 0055\] What a `# hushrun: leave` comment right above the next command says.
            let mut marked: Option<String> = None;
            for i in at + 1..end {
                // \[ADR 0055\] A comment indented no further than the key ends the block: YAML
                // gives it the lines `lines` marked as its text.
                if !matches!(self.lines[i].kind, LineKind::Text | LineKind::Blank) {
                    break;
                }
                let line = self.text(i);
                if line.trim().is_empty() {
                    goes_on = false;
                    folds_on = false;
                    marked = None;
                    continue;
                }
                let start = *indent.get_or_insert(line.len() - line.trim_start().len());
                let raw = line.get(start..).unwrap_or_default();
                let text = raw.trim_end();
                // A line indented further keeps its line breaks, as a blank line does.
                let at_indent = style == '>' && !raw.starts_with([' ', '\t']);
                let folds = folds_on && at_indent;
                folds_on = at_indent;
                let joined = (goes_on || folds).then(|| sources.last_mut()).flatten();
                // A comment on a line of its own reads as nothing. One a fold joins to the lines
                // after it stays: it is the shell's to read, as it reads those as comment too.
                if joined.is_none() && !at_indent && text.trim_start().starts_with('#') {
                    marked = leave_marker(text.trim_start()).or(marked);
                    continue;
                }
                goes_on = (raw.len() - raw.trim_end_matches('\\').len()) % 2 == 1;
                match joined {
                    Some(source) => {
                        if folds {
                            source.folds.push(source.text.len());
                        }
                        source.text.push('\n');
                        source.text.push_str(text);
                    }
                    None => {
                        let mut source = Source::one(i, start, text.to_string(), Scalar::Block);
                        source.leave = marked.take();
                        sources.push(source);
                    }
                }
            }
            // A here-document's lines aren't commands.
            if sources.iter().any(|source| source.text.contains("<<")) {
                return Vec::new();
            }
            return sources;
        }
        if self.block_after(at, found.column, to) != at + 1
            && (at + 1..self.block_after(at, found.column, to))
                .any(|i| self.lines[i].kind == LineKind::Content)
        {
            return Vec::new();
        }
        let line = self.text(at);
        let Some(start) = line[found.column..]
            .find(':')
            .map(|colon| found.column + colon + 1)
            .map(|after| after + (line[after..].len() - line[after..].trim_start().len()))
        else {
            return Vec::new();
        };
        match value.chars().next() {
            Some('"') if value.len() >= 2 && value.ends_with('"') && !value.contains('\\') => {
                vec![Source::one(
                    at,
                    start + 1,
                    value[1..value.len() - 1].to_string(),
                    Scalar::Double,
                )]
            }
            Some('\'')
                if value.len() >= 2
                    && value.ends_with('\'')
                    && !value[1..value.len() - 1].contains('\'') =>
            {
                vec![Source::one(
                    at,
                    start + 1,
                    value[1..value.len() - 1].to_string(),
                    Scalar::Single,
                )]
            }
            Some('"' | '\'') | None => Vec::new(),
            Some(_) => vec![Source::one(at, start, value.to_string(), Scalar::Plain)],
        }
    }

    /// \[ADR 0055\] What a `# hushrun: leave` comment says among the comment lines right above line
    /// `at`, the nearest first.
    fn leave_above(&self, at: usize) -> Option<String> {
        (0..at)
            .rev()
            .take_while(|i| self.lines[*i].kind == LineKind::Comment)
            .find_map(|i| leave_marker(self.text(i).trim()))
    }

    /// Reads `.gitlab-ci.yml`: every top-level key is a job, but its keywords and the hidden
    /// `.templates`.
    fn read_gitlab(&mut self) {
        const KEYWORDS: &[&str] = &[
            "stages",
            "variables",
            "default",
            "include",
            "workflow",
            "image",
            "services",
            "before_script",
            "after_script",
            "cache",
            "spec",
        ];
        let mut jobs = Vec::new();
        let mut i = 0;
        while i < self.lines.len() {
            if self.lines[i].kind != LineKind::Content || self.lines[i].indent != 0 {
                i += 1;
                continue;
            }
            let end = self.block(i + 1, 0);
            let found = entry(self.text(i), 0);
            match found.key.as_deref() {
                Some("image" | "services") => {
                    self.only_ci = Some("its job runs in an image".to_string());
                }
                Some("default") => {
                    let image = (i + 1..end).any(|at| {
                        self.lines[at].kind == LineKind::Content
                            && matches!(
                                entry(self.text(at), self.lines[at].indent).key.as_deref(),
                                Some("image" | "services")
                            )
                    });
                    if image {
                        self.only_ci = Some("its job runs in an image".to_string());
                    }
                }
                Some("workflow") => {
                    self.only_ci = Some("its pipeline's `workflow` rules decide".to_string());
                }
                Some(key) if !KEYWORDS.contains(&key) && !key.starts_with('.') => {
                    jobs.push((key.to_string(), i, end));
                }
                _ => {}
            }
            i = end;
        }
        for (id, header, end) in jobs {
            let job = self.read_gitlab_job(id, header, end);
            if !job.steps.is_empty() {
                self.jobs.push(job);
            }
        }
    }

    fn read_gitlab_job(&self, id: String, header: usize, end: usize) -> Job {
        let mut job = Job {
            id,
            header,
            child_indent: None,
            permissions: None,
            runs_on_end: None,
            runs_on: String::new(),
            scripts: Vec::new(),
            steps: Vec::new(),
            only_ci: self.only_ci.clone(),
            matrix: false,
            on_macos: false,
            calls_a_workflow: false,
            calls: None,
            env_at: None,
            uses_a_local_action: false,
            actions: Vec::new(),
            actions_run_hushrun: false,
            installs_hushrun: false,
            condition: false,
        };
        let Some(first) = self.first_content(header + 1, end) else {
            return job;
        };
        let indent = self.lines[first].indent;
        job.child_indent = Some(indent);
        let mut i = first;
        while i < end {
            let line = &self.lines[i];
            if line.kind != LineKind::Content || line.indent != indent {
                i += 1;
                continue;
            }
            let key_end = self.block(i + 1, indent);
            let found = entry(self.text(i), indent);
            let needs = |job: &mut Job, why: &str| {
                job.only_ci.get_or_insert(why.to_string());
            };
            match found.key.as_deref() {
                Some("image") => needs(&mut job, "its job runs in an image"),
                Some("services") => needs(&mut job, "its job has services"),
                Some("rules" | "only" | "except") => needs(&mut job, "its job has rules"),
                Some("when") if unquote(&found.value) == "manual" => {
                    needs(&mut job, "its job runs by hand");
                }
                Some("extends") => needs(&mut job, "it extends another job"),
                Some("parallel") => job.matrix = true,
                Some("script") => {
                    // A sequence may sit at its key's indentation.
                    let script_end = self.steps_end(i + 1, end, indent);
                    if let Some(step) = self.gitlab_script(i, &found, script_end) {
                        job.scripts = scripts::scripts_run(&step.commands);
                        job.installs_hushrun = step.commands.contains("install-hushrun");
                        job.steps.push(step);
                    }
                    i = script_end;
                    continue;
                }
                _ => {}
            }
            i = key_end;
        }
        job
    }

    /// A job's `script:` as one step: GitLab runs its lines in one shell.
    fn gitlab_script(&self, at: usize, found: &Entry, end: usize) -> Option<Step> {
        let first = self.first_content(at + 1, end)?;
        if !found.value.trim().is_empty() {
            return None;
        }
        let dash = self.lines[first].indent;
        let mut sources = Vec::new();
        let mut i = first;
        while i < end {
            let line = &self.lines[i];
            if line.kind != LineKind::Content
                || line.indent != dash
                || !self.text(i)[dash..].starts_with("- ")
            {
                i += 1;
                continue;
            }
            let text = self.text(i);
            let start = dash + 2 + (text[dash + 2..].len() - text[dash + 2..].trim_start().len());
            let value = strip_comment(&text[start..]);
            let item_end = self.block_after(i, dash, end);
            // \[ADR 0055\] A `# hushrun: leave` comment right above the item, or at the end of its
            // line, leaves all of its commands.
            let leave = self
                .leave_above(i)
                .or_else(|| end_comment(&text[start..]).and_then(leave_marker));
            let item_from = sources.len();
            if block_style(value).is_some() {
                let indented = Entry {
                    dash: true,
                    column: dash,
                    key: None,
                    value: value.to_string(),
                };
                sources.extend(self.sources(i, &indented, item_end));
            } else if value.starts_with(['"', '\'']) {
                let text = unquote(value);
                if !text.contains('\\') && value.len() == text.len() + 2 {
                    let scalar = match value.starts_with('"') {
                        true => Scalar::Double,
                        false => Scalar::Single,
                    };
                    sources.push(Source::one(i, start + 1, text, scalar));
                }
            } else {
                sources.push(Source::one(i, start, value.to_string(), Scalar::Plain));
            }
            if let Some(why) = &leave {
                for source in &mut sources[item_from..] {
                    source.leave.get_or_insert_with(|| why.clone());
                }
            }
            i = item_end;
        }
        let commands = sources
            .iter()
            .map(|source| source.shell())
            .collect::<Vec<_>>()
            .join("\n");
        Some(Step {
            line: first,
            dash,
            commands,
            sources,
            dir: Ok(String::new()),
            only_ci: None,
            index: 0,
        })
    }

    /// The text of the scalar that starts on line `at`: one line, continuation lines, or a block
    /// scalar's lines.
    fn scalar(&self, at: usize, entry: &Entry, to: usize) -> String {
        let value = entry.value.trim();
        let below = |i: usize| {
            let line = &self.lines[i];
            matches!(
                line.kind,
                LineKind::Text | LineKind::Blank | LineKind::Comment
            ) || (line.kind == LineKind::Content && line.indent > entry.column)
        };
        let mut end = at + 1;
        while end < to && below(end) {
            end += 1;
        }
        if let Some(style) = block_style(value) {
            // \[ADR 0055\] The lines `lines` marked as its text: a comment after it, indented no
            // further than the key, isn't.
            let text_end = (at + 1..end)
                .find(|i| !matches!(self.lines[*i].kind, LineKind::Text | LineKind::Blank))
                .unwrap_or(end);
            let lines: Vec<&str> = (at + 1..text_end).map(|i| self.text(i)).collect();
            let indent = lines
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.len() - l.trim_start().len())
                .min()
                .unwrap_or(0);
            let lines = lines
                .iter()
                .map(|l| l.get(indent..).unwrap_or("").trim_end());
            if style != '>' {
                return lines.collect::<Vec<_>>().join("\n");
            }
            // \[ADR 0055\] Folded: the line break between two lines at the block's indentation is
            // a space; one next to a blank line, or to a line indented further, stays a break.
            let mut text = String::new();
            let mut last: Option<bool> = None;
            let mut blank = false;
            for line in lines {
                if line.is_empty() {
                    blank = true;
                    continue;
                }
                let at_indent = !line.starts_with([' ', '\t']);
                if let Some(was) = last {
                    text.push(if was && at_indent && !blank {
                        ' '
                    } else {
                        '\n'
                    });
                }
                text.push_str(line);
                last = Some(at_indent);
                blank = false;
            }
            return text;
        }
        // A plain or quoted scalar, which may go on over lines that fold into spaces.
        let mut text = value.to_string();
        for i in at + 1..end {
            let line = self.text(i).trim();
            if self.lines[i].kind == LineKind::Content && !line.is_empty() {
                text.push(' ');
                text.push_str(line);
            }
        }
        unquote(&text)
    }

    /// Whether a `defaults:` block from `from` to `to` moves `run` steps elsewhere.
    fn moves_runs(&self, from: usize, to: usize) -> bool {
        (from..to).any(|i| {
            let line = &self.lines[i];
            line.kind == LineKind::Content && {
                let entry = entry(self.text(i), line.indent);
                entry.key.as_deref() == Some("working-directory")
                    && !matches!(unquote(&entry.value).trim(), "." | "./" | "")
            }
        })
    }

    fn permissions_at(&self, line: usize, end: usize) -> Permissions {
        let indent = self.lines[line].indent;
        let value = entry(self.text(line), indent).value;
        let mut entries = Vec::new();
        if value.trim().is_empty() {
            for i in line + 1..end {
                let l = &self.lines[i];
                if l.kind != LineKind::Content {
                    continue;
                }
                let e = entry(self.text(i), l.indent);
                if let Some(key) = e.key {
                    entries.push((i, key, unquote(&e.value)));
                }
            }
        }
        Permissions {
            line,
            indent,
            inline: Some(value.trim().to_string()).filter(|v| !v.is_empty()),
            entries,
        }
    }

    fn first_content(&self, from: usize, to: usize) -> Option<usize> {
        (from..to.min(self.lines.len())).find(|i| self.lines[*i].kind == LineKind::Content)
    }

    fn last_content(&self, from: usize, to: usize) -> Option<usize> {
        (from..to.min(self.lines.len()))
            .rev()
            .find(|i| matches!(self.lines[*i].kind, LineKind::Content | LineKind::Text))
    }

    /// Gives `jobs` an ID token: `id-token: write` in each job's own permissions; else in the
    /// workflow's, when the job has none; else in a new block of the job's own, with `contents:
    /// read`, which leaves its token only those two. `write-all` needs nothing; `read-all` is
    /// for a person to change.
    #[cfg(test)]
    pub fn edit(&self, jobs: &[&str]) -> Edit {
        self.edit_with(jobs, &Changes::default())
    }

    /// Gives `jobs` an ID token as `edit` says, with `changes` made to the same text.
    pub fn edit_with(&self, jobs: &[&str], changes: &Changes) -> Edit {
        let mut edit = Edit::default();
        // Lines to insert after a line, and line values to replace.
        let mut after: Vec<(usize, Vec<String>)> = Vec::new();
        let mut replace: Vec<(usize, String)> = Vec::new();
        let mut inherit: Vec<&str> = Vec::new();
        for job in self
            .jobs
            .iter()
            .filter(|j| self.host == Host::GitHub && jobs.contains(&j.id.as_str()))
        {
            let who = format!("job {}", job.id);
            match &job.permissions {
                Some(permissions) => match self.grant(permissions, &mut after, &mut replace) {
                    Grant::Added => edit
                        .changes
                        .push(format!("{who}: adds id-token: write to its permissions")),
                    Grant::Has => {}
                    Grant::Manual(why) => edit.manual.push(format!("{who}: {why}")),
                },
                None if self.permissions.is_some() => inherit.push(&job.id),
                None => {
                    let indent = job.child_indent.unwrap_or(self.unit);
                    let pad = " ".repeat(indent);
                    let inner = " ".repeat(indent + self.unit);
                    let at = job.runs_on_end.unwrap_or(job.header);
                    after.push((
                        at,
                        vec![
                            format!("{pad}permissions:"),
                            format!("{inner}contents: read"),
                            format!("{inner}id-token: write"),
                        ],
                    ));
                    edit.changes.push(format!(
                        "{who}: adds permissions contents: read and id-token: write. The job's token then has only these two: add any other it needs"
                    ));
                }
            }
        }
        // \[ADR 0047\] Nothing reads `.gitlab-ci.yml` at run time: a job that keeps its checks'
        // reports says so, so that they run every time (C-26).
        if self.host == Host::GitLab {
            let file: Option<serde_json::Value> =
                hushrun_core::yaml::parse(&self.path, self.text.as_bytes()).ok();
            for job in self.jobs.iter().filter(|j| jobs.contains(&j.id.as_str())) {
                let Some(file) = &file else {
                    continue;
                };
                let Some(reader) = readers::gitlab_reads(&file[job.id.as_str()]) else {
                    continue;
                };
                let who = format!("job {}", job.id);
                match self.reports_read(job, file) {
                    ReportsRead::Add(at, lines) => {
                        after.push((at, lines));
                        edit.changes.push(format!(
                            "{who}: adds HUSHRUN_REPORTS_READ: \"true\" to its variables, since {reader} reads its checks' reports"
                        ));
                    }
                    ReportsRead::Has => {}
                    ReportsRead::Manual(why) => edit.manual.push(format!("{who}: {why}")),
                }
            }
        }
        if let (Some(permissions), false) = (&self.permissions, inherit.is_empty()) {
            let who = match inherit.split_last() {
                Some((last, [])) => format!("job {last}"),
                Some((last, rest)) => format!("jobs {} and {last}", rest.join(", ")),
                None => String::new(),
            };
            match self.grant(permissions, &mut after, &mut replace) {
                Grant::Added => edit.changes.push(format!(
                    "adds id-token: write to the workflow's permissions, for {who}"
                )),
                Grant::Has => {}
                Grant::Manual(why) => edit
                    .manual
                    .push(format!("the workflow's permissions, for {who}: {why}")),
            }
        }
        edit.changes.extend(changes.said.iter().cloned());
        edit.manual.extend(changes.manual.iter().cloned());
        after.extend(changes.after.iter().cloned());
        // What goes into a line, last first, so that earlier offsets hold.
        let mut lines: Vec<usize> = changes.inserts.iter().map(|(line, ..)| *line).collect();
        lines.sort_unstable();
        lines.dedup();
        for line in lines {
            let mut inserts: Vec<&(usize, usize, String)> = changes
                .inserts
                .iter()
                .filter(|(at, ..)| *at == line)
                .collect();
            inserts.sort_by_key(|(_, at, _)| std::cmp::Reverse(*at));
            let mut text = self.text(line).to_string();
            for (_, at, inserted) in inserts {
                text.insert_str(*at, inserted);
            }
            replace.push((line, text));
        }
        if after.is_empty() && replace.is_empty() && changes.before.is_empty() {
            return edit;
        }
        let mut out = String::with_capacity(self.text.len() + 128);
        for (i, line) in self.lines.iter().enumerate() {
            for (_, inserted) in changes.before.iter().filter(|(at, _)| *at == i) {
                for text in inserted {
                    out.push_str(text);
                    out.push_str(self.newline);
                }
            }
            match replace.iter().find(|(at, _)| *at == i) {
                Some((_, text)) => {
                    out.push_str(text);
                    out.push_str(&self.text[line.end..line.next]);
                }
                None => out.push_str(&self.text[line.start..line.next]),
            }
            for (_, inserted) in after.iter().filter(|(at, _)| *at == i) {
                if line.next == line.end {
                    // The last line, without a line break: it gets one, and the file still
                    // ends without.
                    out.push_str(self.newline);
                    out.push_str(&inserted.join(self.newline));
                } else {
                    for text in inserted {
                        out.push_str(text);
                        out.push_str(self.newline);
                    }
                }
            }
        }
        edit.text = Some(out);
        edit
    }

    /// \[ADR 0049\] What `job`, a GitHub job, needs for its `env` to hold each of `wanted`, a name
    /// and the value init writes, which `holds` says a value it has already stands for: the lines
    /// to add after a line, and what a person must change by hand. A value it holds otherwise is
    /// for a person, never overwritten. An `env` init can't edit as lines, in flow style, an anchor
    /// or an alias, or one that comes through `<<`, gets the line to add instead.
    pub(crate) fn env_needs(
        &self,
        job: &Job,
        wanted: &[(&str, String)],
        holds: &dyn Fn(&str, &str) -> bool,
    ) -> (Option<(usize, Vec<String>)>, Vec<String>) {
        let file = variants::parse(&self.path, self.text.as_bytes());
        let env = file
            .as_ref()
            .map(|file| file["jobs"][job.id.as_str()]["env"].clone())
            .unwrap_or_default();
        let mut manual = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for (name, value) in wanted {
            match env.get(*name) {
                Some(found) => {
                    let text = found
                        .as_str()
                        .map_or_else(|| found.to_string(), str::to_string);
                    if !holds(name, &text) {
                        manual.push(format!(
                            "its env sets {name} to {text}: make it {value}, or no pass skips it there"
                        ));
                    }
                }
                None => missing.push(format!("{name}: {value}")),
            }
        }
        if missing.is_empty() {
            return (None, manual);
        }
        let own = self.lines[job.header].indent;
        let indent = job.child_indent.unwrap_or(own + self.unit);
        let lines = missing.join(", ");
        match job.env_at {
            Some((at, end)) => match entry(self.text(at), indent).value.trim() {
                "" => {
                    let inner = self
                        .first_content(at + 1, end)
                        .map_or(indent + self.unit, |first| self.lines[first].indent);
                    let pad = " ".repeat(inner);
                    let added = missing.iter().map(|line| format!("{pad}{line}")).collect();
                    (Some((at, added)), manual)
                }
                shared if shared.starts_with(['&', '*']) => {
                    manual.push(format!(
                        "its env is shared through {shared}: add {lines} to it"
                    ));
                    (None, manual)
                }
                _ => {
                    manual.push(format!("its env is written on one line: add {lines} to it"));
                    (None, manual)
                }
            },
            None if !env.is_null() => {
                manual.push(format!(
                    "its env comes through <<, and an env of its own would replace it: add {lines} to it"
                ));
                (None, manual)
            }
            None => {
                let pad = " ".repeat(indent);
                let inner = " ".repeat(indent + self.unit);
                let mut added = vec![format!("{pad}env:")];
                added.extend(missing.iter().map(|line| format!("{inner}{line}")));
                (Some((job.runs_on_end.unwrap_or(job.header), added)), manual)
            }
        }
    }

    /// \[ADR 0047\] Where `HUSHRUN_REPORTS_READ: "true"` goes in GitLab's `job`, as `file`, the
    /// workflow read as YAML, has it: after its `variables:`, a block's first entry; else a block
    /// of its own, after its key. Variables a job takes through `<<` would be lost to a block of
    /// its own, and what goes into an anchor goes to every job that takes it, so those are for a
    /// person.
    fn reports_read(&self, job: &Job, file: &serde_json::Value) -> ReportsRead {
        const LINE: &str = "HUSHRUN_REPORTS_READ: \"true\"";
        // Said already, by the job, an anchor or the file's variables, whatever it says.
        if report::reports_read_value(file, &job.id).is_some() {
            return ReportsRead::Has;
        }
        let parsed = &file[job.id.as_str()];
        let own = self.lines[job.header].indent;
        match entry(self.text(job.header), own).value.trim() {
            "" => {}
            anchor if anchor.starts_with('&') => {
                return ReportsRead::Manual(format!(
                    "other jobs can take its keys through {anchor}: add {LINE} to its variables"
                ));
            }
            _ => {
                return ReportsRead::Manual(format!(
                    "it is written on one line: add {LINE} to its variables"
                ));
            }
        }
        let indent = job.child_indent.unwrap_or(own + self.unit);
        let end = self.block(job.header + 1, own);
        for at in job.header + 1..end {
            let line = &self.lines[at];
            if line.kind != LineKind::Content || line.indent != indent {
                continue;
            }
            let found = entry(self.text(at), indent);
            if found.key.as_deref() != Some("variables") {
                continue;
            }
            return match found.value.trim() {
                "" => {
                    let inner_end = self.block(at + 1, indent);
                    let inner = self
                        .first_content(at + 1, inner_end)
                        .map_or(indent + self.unit, |first| self.lines[first].indent);
                    ReportsRead::Add(at, vec![format!("{}{LINE}", " ".repeat(inner))])
                }
                shared if shared.starts_with(['&', '*']) => ReportsRead::Manual(format!(
                    "other jobs can share its variables through {shared}: add {LINE} to them"
                )),
                _ => ReportsRead::Manual(format!(
                    "its variables are written on one line: add {LINE} to them"
                )),
            };
        }
        if parsed.get("variables").is_some() {
            return ReportsRead::Manual(format!(
                "its variables come through <<, and variables of its own would replace them: add {LINE} to them"
            ));
        }
        ReportsRead::Add(
            job.header,
            vec![
                format!("{}variables:", " ".repeat(indent)),
                format!("{}{LINE}", " ".repeat(indent + self.unit)),
            ],
        )
    }

    /// Gives `permissions` `id-token: write`, recording the edit.
    fn grant(
        &self,
        permissions: &Permissions,
        after: &mut Vec<(usize, Vec<String>)>,
        replace: &mut Vec<(usize, String)>,
    ) -> Grant {
        let line = self.text(permissions.line);
        match permissions.inline.as_deref() {
            Some("write-all") => return Grant::Has,
            Some("read-all") => {
                return Grant::Manual(
                    "permissions: read-all gives no ID token. Replace it with the permissions the job reads, plus id-token: write, such as { contents: read, id-token: write }".into(),
                );
            }
            Some(flow) if flow.starts_with('{') && flow.ends_with('}') => {
                let inside = flow[1..flow.len() - 1].trim();
                let entries: Vec<(&str, &str)> = inside
                    .split(',')
                    .filter(|e| !e.trim().is_empty())
                    .filter_map(|e| e.split_once(':'))
                    .map(|(k, v)| (k.trim(), v.trim()))
                    .collect();
                if entries
                    .iter()
                    .any(|(k, v)| *k == "id-token" && *v == "write")
                {
                    return Grant::Has;
                }
                if entries.iter().any(|(k, _)| *k == "id-token") {
                    return Grant::Manual(
                        "its permissions give id-token another value: make it id-token: write"
                            .into(),
                    );
                }
                // The closing brace, before any comment.
                let Some(close) = strip_comment(line).rfind('}') else {
                    return Grant::Manual("its permissions can't be read".into());
                };
                let before = line[..close].trim_end();
                let text = if before.ends_with('{') {
                    format!("{before}id-token: write{}", &line[close..])
                } else {
                    let space = if line[..close].ends_with(' ') {
                        " "
                    } else {
                        ""
                    };
                    format!("{before}, id-token: write{space}{}", &line[close..])
                };
                replace.push((permissions.line, text));
                return Grant::Added;
            }
            Some(other) => {
                return Grant::Manual(format!(
                    "permissions: {other} isn't a mapping init can add id-token: write to"
                ));
            }
            None => {}
        }
        if let Some((at, _, value)) = permissions.entries.iter().find(|(_, k, _)| k == "id-token") {
            if value == "write" {
                return Grant::Has;
            }
            let text = self.text(*at);
            let Some(colon) = text.find(':') else {
                return Grant::Manual("its permissions can't be read".into());
            };
            let comment = text[colon..]
                .find(" #")
                .map(|i| &text[colon + i..])
                .unwrap_or("");
            replace.push((*at, format!("{}: write{comment}", &text[..colon])));
            return Grant::Added;
        }
        let indent = match permissions.entries.first() {
            Some((at, _, _)) => self.lines[*at].indent,
            None => permissions.indent + self.unit,
        };
        let last = permissions
            .entries
            .last()
            .map_or(permissions.line, |(at, _, _)| *at);
        after.push((last, vec![format!("{}id-token: write", " ".repeat(indent))]));
        Grant::Added
    }
}

enum Grant {
    Added,
    Has,
    Manual(String),
}

/// What a step says, before its job's say where it runs.
struct StepRead {
    line: usize,
    dash: usize,
    /// Where it is among its job's steps, from 0.
    index: usize,
    scripts: Vec<String>,
    commands: Option<String>,
    sources: Vec<Source>,
    dir: Option<String>,
    only_ci: Option<String>,
    /// This repository's checkout: its `path`.
    checkout: Option<String>,
    installs: bool,
    local_action: bool,
    /// \[ADR 0050\] The action of this repository it uses, as `uses: ./<path>` says it.
    action: Option<String>,
}

impl StepRead {
    /// \[ADR 0050\] The action of this repository it uses, with `checkout` where its job checks
    /// this repository out, from the workspace, as `uses: ./<path>` counts.
    fn action_use(&self, checkout: Option<&String>) -> Option<ActionUse> {
        let path = self.action.as_deref()?;
        let dir = match checkout {
            None => Err("its job doesn't check this repository out".to_string()),
            Some(checkout) => directory(checkout, path),
        };
        Some(ActionUse {
            dir,
            line: self.line,
            dash: self.dash,
            index: self.index,
            only_ci: self.only_ci.clone(),
        })
    }

    /// The step, with `dir` the working directory that applies, and `checkout` where its job
    /// checks this repository out.
    fn finish(self, checkout: Option<&String>, dir: Option<&str>) -> Option<Step> {
        let commands = self.commands?;
        let dir = match checkout {
            None => Err("its job doesn't check this repository out".to_string()),
            Some(checkout) => directory(checkout, dir.unwrap_or_default()),
        };
        Some(Step {
            line: self.line,
            dash: self.dash,
            commands,
            sources: self.sources,
            dir,
            only_ci: self.only_ci,
            index: self.index,
        })
    }
}

/// `dir`, a working directory, relative to the repository checked out at `checkout`, both
/// relative to the workspace.
fn directory(checkout: &str, dir: &str) -> Result<String, String> {
    if dir.contains("${{") || dir.contains('$') {
        return Err(EXPRESSION_DIR.to_string());
    }
    let clean = |text: &str| -> Option<Vec<String>> {
        let mut parts = Vec::new();
        for part in text.split('/') {
            match part {
                "" | "." => {}
                ".." => return None,
                part => parts.push(part.to_string()),
            }
        }
        (!text.starts_with('/')).then_some(parts)
    };
    let (Some(checkout), Some(dir)) = (clean(checkout), clean(dir)) else {
        return Err("its working directory is outside the repository".to_string());
    };
    match dir.strip_prefix(checkout.as_slice()) {
        Some(inside) => Ok(inside.join("/")),
        None => Err("its working directory is outside the repository's checkout".to_string()),
    }
}

/// The items of a one-line list, `[a, b]`, or the one word that stands for it.
fn flow_list(value: &str) -> Vec<String> {
    let value = value.trim();
    if value.is_empty() {
        return Vec::new();
    }
    value
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(value)
        .split(',')
        .map(unquote)
        .filter(|item| !item.is_empty())
        .collect()
}

/// The first secret `text` reads.
fn secret_of(text: &str) -> Option<String> {
    let (_, after) = text.split_once("secrets.")?;
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Whether `branch` matches one of GitHub's branch patterns: `*` stops at a `/`, `**` doesn't. A
/// pattern with anything else, or a `!`, matches for sure only when it says the name.
fn matches(pattern: &str, branch: &str) -> bool {
    fn from(pattern: &[u8], branch: &[u8]) -> bool {
        match pattern {
            [] => branch.is_empty(),
            [b'*', b'*', rest @ ..] => (0..=branch.len()).any(|at| from(rest, &branch[at..])),
            [b'*', rest @ ..] => {
                let stop = branch
                    .iter()
                    .position(|byte| *byte == b'/')
                    .unwrap_or(branch.len());
                (0..=stop).any(|at| from(rest, &branch[at..]))
            }
            [first, rest @ ..] => branch.first() == Some(first) && from(rest, &branch[1..]),
        }
    }
    from(pattern.as_bytes(), branch.as_bytes())
}

/// A line's key and value: `key: value`, or `- key: value` in a sequence.
#[derive(Debug, Clone)]
struct Entry {
    /// The line starts a sequence item.
    dash: bool,
    /// Where the key starts.
    column: usize,
    key: Option<String>,
    /// What follows the key's colon, without a comment.
    value: String,
}

fn entry(line: &str, indent: usize) -> Entry {
    let mut column = indent;
    let mut rest = line.get(indent..).unwrap_or_default();
    let dash = rest == "-" || rest.starts_with("- ");
    if dash {
        let after = rest[1..].trim_start();
        column = line.len() - after.len();
        rest = after;
    }
    let (key, value) = match key_of(rest) {
        Some((key, value)) => (Some(key), value),
        None => (None, rest),
    };
    Entry {
        dash,
        column,
        key,
        value: strip_comment(value).to_string(),
    }
}

/// `key` and what follows its colon, for `key: value` or `key:`.
fn key_of(text: &str) -> Option<(String, &str)> {
    let (key, rest) = match text.chars().next()? {
        quote @ ('"' | '\'') => {
            let close = text[1..].find(quote)? + 1;
            (&text[1..close], &text[close + 1..])
        }
        _ => {
            let at = text
                .match_indices(':')
                .map(|(i, _)| i)
                .find(|i| matches!(text[i + 1..].chars().next(), None | Some(' ' | '\t')))?;
            if text[..at].contains(" #") || text.starts_with('#') {
                return None;
            }
            (&text[..at], &text[at..])
        }
    };
    let value = rest.strip_prefix(':')?;
    if !(value.is_empty() || value.starts_with([' ', '\t'])) {
        return None;
    }
    Some((key.trim().to_string(), value))
}

/// \[ADR 0055\] Whether `condition`, a job's `if:`, reads only other jobs' outputs and results:
/// `needs.<job>.outputs.<name>` and `needs.<job>.result`, with literals, comparisons, `&&`, `||`,
/// `!`, `success()`, `always()`, `!cancelled()` and the functions that only work on values. Such a
/// job runs when the jobs it needs say so, as a changes filter does when its area changed. Any
/// other context, `failure()`, a `cancelled()` not negated, or a word this doesn't know is no.
pub(super) fn on_other_jobs(condition: &str) -> bool {
    const PURE: &[&str] = &[
        "success",
        "always",
        "contains",
        "startswith",
        "endswith",
        "format",
        "join",
        "tojson",
        "fromjson",
    ];
    let text = condition.trim();
    let text = text
        .strip_prefix("${{")
        .and_then(|inner| inner.strip_suffix("}}"))
        .unwrap_or(text);
    // GitHub fills in a `${{ … }}` inside it first, with what can be anything.
    if text.contains("${{") {
        return false;
    }
    let bytes = text.as_bytes();
    // What the token before this one was: a `.`, a `!`, or anything else.
    let (mut after_dot, mut after_not, mut any) = (false, false, false);
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        any = true;
        let (dot, not) = (after_dot, after_not);
        after_dot = false;
        after_not = false;
        match byte {
            b'\'' => {
                // A string, `''` for a quote in it.
                at += 1;
                loop {
                    match bytes.get(at) {
                        None => return false,
                        Some(b'\'') if bytes.get(at + 1) == Some(&b'\'') => at += 2,
                        Some(b'\'') => break,
                        Some(_) => at += 1,
                    }
                }
                at += 1;
            }
            b'.' => {
                after_dot = true;
                at += 1;
            }
            b'*' if dot => at += 1,
            b'!' if bytes.get(at + 1) != Some(&b'=') => {
                after_not = true;
                at += 1;
            }
            b'=' | b'!' | b'<' | b'>' => {
                at += if bytes.get(at + 1) == Some(&b'=') {
                    2
                } else {
                    1
                };
            }
            b'&' | b'|' => {
                if bytes.get(at + 1) != Some(&byte) {
                    return false;
                }
                at += 2;
            }
            b'(' | b')' | b'[' | b']' | b',' => at += 1,
            b'0'..=b'9' | b'-' => {
                at += 1;
                while bytes
                    .get(at)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
                {
                    at += 1;
                }
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = at;
                while bytes
                    .get(at)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                {
                    at += 1;
                }
                let word = text[start..at].to_ascii_lowercase();
                if dot {
                    continue;
                }
                let call = text[at..].trim_start().starts_with('(');
                let fits = match call {
                    true => PURE.contains(&word.as_str()) || (word == "cancelled" && not),
                    false => matches!(word.as_str(), "needs" | "true" | "false" | "null"),
                };
                if !fits {
                    return false;
                }
            }
            _ => return false,
        }
    }
    any
}

/// \[ADR 0055\] What a comment, `#` and its text, says when it is `# hushrun: leave`: why init
/// leaves the line, as it follows (`# hushrun: leave, <why>`), empty when nothing does. `None` for
/// any other comment.
fn leave_marker(comment: &str) -> Option<String> {
    let text = comment.strip_prefix('#')?.trim_start_matches('#').trim();
    let rest = text.strip_prefix("hushrun:")?.trim_start();
    let rest = rest.strip_prefix("leave")?;
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let why = rest.trim_start().trim_start_matches([',', ':', ';']).trim();
    Some(why.to_string())
}

/// The comment at the end of `line`, from its `#`: what [`strip_comment`] takes off.
fn end_comment(line: &str) -> Option<&str> {
    let kept = strip_comment(line);
    let rest = line[kept.len()..].trim_start();
    rest.starts_with('#').then_some(rest)
}

/// A value without its comment: ` #` and what follows, outside quotes.
fn strip_comment(value: &str) -> &str {
    let mut quote = None;
    let mut previous = ' ';
    for (i, c) in value.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') if previous == ' ' || i == 0 => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') if previous == ' ' || previous == '\t' => return value[..i].trim_end(),
            _ => {}
        }
        previous = c;
    }
    value.trim_end()
}

/// A scalar's text without its quotes.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1].replace("''", "'");
    }
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let mut out = String::new();
        let mut chars = value[1..value.len() - 1].chars();
        while let Some(c) = chars.next() {
            match (c, chars.as_str().chars().next()) {
                ('\\', Some(next)) => {
                    chars.next();
                    out.push(match next {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                }
                (c, _) => out.push(c),
            }
        }
        return out;
    }
    value.to_string()
}

/// `|` or `>` for a block scalar's header, such as `|`, `>-` or `|2+`.
fn block_style(value: &str) -> Option<char> {
    let first = value.chars().next().filter(|c| matches!(c, '|' | '>'))?;
    value[1..]
        .chars()
        .all(|c| c.is_ascii_digit() || c == '-' || c == '+')
        .then_some(first)
}

/// The lines of `text`, with block scalars' text marked.
fn lines(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let (end, next) = match text[start..].find('\n') {
            Some(i) => {
                let newline = start + i;
                let end = if text[..newline].ends_with('\r') {
                    newline - 1
                } else {
                    newline
                };
                (end, newline + 1)
            }
            None => (text.len(), text.len()),
        };
        let line = &text[start..end];
        let trimmed = line.trim_start_matches(' ');
        let kind = if trimmed.trim().is_empty() {
            LineKind::Blank
        } else if trimmed.starts_with('#') {
            LineKind::Comment
        } else {
            LineKind::Content
        };
        lines.push(Line {
            start,
            end,
            next,
            indent: line.len() - trimmed.len(),
            kind,
        });
        start = next;
    }
    // Block scalars: the lines below `key: |` that are indented past the key are its text.
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        if line.kind == LineKind::Content {
            let entry = entry(&text[line.start..line.end], line.indent);
            if block_style(entry.value.trim()).is_some() {
                // `- |` has no key: its text only needs to be past the dash.
                let column = match (entry.dash, &entry.key) {
                    (true, None) => line.indent,
                    _ => entry.column,
                };
                let mut j = i + 1;
                while j < lines.len()
                    && (lines[j].kind == LineKind::Blank || lines[j].indent > column)
                {
                    if lines[j].kind != LineKind::Blank {
                        lines[j].kind = LineKind::Text;
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-15
    #[test]
    fn a_place_in_a_source_is_a_place_in_the_file() {
        let source = Source::one(7, 10, "a b \\\n  c d \\\n    e".to_string(), Scalar::Block);
        assert_eq!(source.place(0), (7, 10));
        assert_eq!(source.place(2), (7, 12));
        assert_eq!(source.text[8..].chars().next(), Some('c'));
        assert_eq!(source.place(8), (8, 12));
        assert_eq!(source.text[10..].chars().next(), Some('d'));
        assert_eq!(source.place(10), (8, 14));
        let e = source.text.find('e').unwrap();
        assert_eq!(source.place(e), (9, 14));
    }

    fn edited(text: &str, jobs: &[&str]) -> (String, Edit) {
        let workflow = Workflow::parse("ci.yml".into(), text.into());
        let edit = workflow.edit(jobs);
        (edit.text.clone().unwrap_or_else(|| text.to_string()), edit)
    }

    const CI: &str = "\
name: CI # the main one
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # Install, then test.
      - run: npm ci
      - name: Test
        run: npm test
  lint:
    runs-on: ubuntu-latest
    steps:
    - run: |
        npm ci
        npm run lint -- --max-warnings 0
  build:
    runs-on: ubuntu-latest
    steps:
      - run: npm run build
";

    // covers: C-15
    /// The line reader takes what the YAML parser refuses, so a job can be there with nothing to
    /// say about its variants.
    // covers: C-14
    #[test]
    fn a_file_that_doesnt_parse_may_hold_variants() {
        let job = |file: &str| Workflow::parse("ci.yml".into(), file.into()).jobs[0].matrix;
        let file = "on: push\njobs:\n  test:\n    runs-on: a\n    steps:\n      - run: npm test\n";
        assert!(!job(file));
        assert!(job(&format!("{file}    runs-on: b\n")));
    }

    #[test]
    fn jobs_name_the_scripts_their_steps_run() {
        let workflow = Workflow::parse("ci.yml".into(), CI.into());
        let scripts: Vec<(&str, Vec<String>)> = workflow
            .jobs
            .iter()
            .map(|j| (j.id.as_str(), j.scripts.clone()))
            .collect();
        assert_eq!(
            scripts,
            [
                ("test", vec!["test".to_string()]),
                ("lint", vec!["lint".to_string()]),
                ("build", vec!["build".to_string()]),
            ]
        );
        let wrapped = ["test".to_string(), "lint".to_string()];
        let running: Vec<&str> = workflow.running(&wrapped).map(|j| j.id.as_str()).collect();
        assert_eq!(running, ["test", "lint"]);
    }

    // covers: C-15
    #[test]
    fn without_permissions_a_job_gets_a_block_of_its_own() {
        let (text, edit) = edited(CI, &["test", "lint"]);
        assert_eq!(
            text,
            CI.replace(
                "  test:\n    runs-on: ubuntu-latest\n",
                "  test:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n      id-token: write\n",
            )
            .replace(
                "  lint:\n    runs-on: ubuntu-latest\n",
                "  lint:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n      id-token: write\n",
            )
        );
        assert_eq!(edit.changes.len(), 2);
        assert!(
            edit.changes[0].contains("The job's token then has only these two"),
            "{:?}",
            edit.changes
        );
    }

    // covers: C-15
    #[test]
    fn a_job_mapping_gets_id_token_write() {
        let text = "\
jobs:
  test:
    runs-on: ubuntu-latest
    permissions:
      contents: read # to check out
      pull-requests: write
    steps:
      - run: pnpm test
";
        let (out, edit) = edited(text, &["test"]);
        assert_eq!(
            out,
            text.replace(
                "      pull-requests: write\n",
                "      pull-requests: write\n      id-token: write\n"
            )
        );
        assert_eq!(
            edit.changes,
            ["job test: adds id-token: write to its permissions"]
        );
        // id-token: read becomes write; write needs nothing.
        let read = text.replace("pull-requests: write", "id-token: read # for now");
        let (out, _) = edited(&read, &["test"]);
        assert_eq!(
            out,
            text.replace("pull-requests: write", "id-token: write # for now")
        );
        let has = text.replace("pull-requests: write", "id-token: write");
        let (out, edit) = edited(&has, &["test"]);
        assert_eq!(out, has);
        assert!(edit.changes.is_empty() && edit.text.is_none());
    }

    // covers: C-15
    #[test]
    fn a_job_without_its_own_takes_the_workflows_permissions() {
        let text = "\
permissions:
  contents: read

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - run: yarn test
  e2e:
    runs-on: ubuntu-latest
    steps:
      - run: yarn run test
";
        let workflow = Workflow::parse("ci.yml".into(), text.into());
        let edit = workflow.edit(&["test", "e2e"]);
        assert_eq!(
            edit.text.as_deref(),
            Some(&*text.replace(
                "  contents: read\n",
                "  contents: read\n  id-token: write\n"
            ))
        );
        assert_eq!(
            edit.changes,
            ["adds id-token: write to the workflow's permissions, for jobs test and e2e"]
        );
        // On one line, the mapping gets it on that line.
        let flow = text.replace(
            "permissions:\n  contents: read\n",
            "permissions: { contents: read } # least\n",
        );
        let (out, _) = edited(&flow, &["test"]);
        assert!(
            out.starts_with("permissions: { contents: read, id-token: write } # least\n"),
            "{out}"
        );
        let empty = text.replace("permissions:\n  contents: read\n", "permissions: {}\n");
        let (out, _) = edited(&empty, &["test"]);
        assert!(out.starts_with("permissions: {id-token: write}\n"), "{out}");
    }

    // covers: C-15
    #[test]
    fn write_all_needs_nothing_and_read_all_is_for_a_person() {
        let text = "\
permissions: write-all
jobs:
  test:
    steps:
      - run: npm test
";
        let (out, edit) = edited(text, &["test"]);
        assert_eq!(out, text);
        assert!(edit.changes.is_empty() && edit.manual.is_empty());
        let read_all = text.replace("write-all", "read-all");
        let (out, edit) = edited(&read_all, &["test"]);
        assert_eq!(out, read_all);
        assert_eq!(edit.manual.len(), 1);
        assert!(
            edit.manual[0].starts_with(
                "the workflow's permissions, for job test: permissions: read-all gives no ID token"
            ),
            "{:?}",
            edit.manual
        );
        let job = "jobs:\n  test:\n    permissions: read-all\n    steps:\n      - run: npm test\n";
        let (_, edit) = edited(job, &["test"]);
        assert!(edit.manual[0].starts_with("job test: permissions: read-all"));
    }

    // covers: C-15
    #[test]
    fn comments_line_endings_and_a_missing_final_newline_stay() {
        let text = "# Tests.\r\njobs:\r\n  # The only job.\r\n  test:\r\n    runs-on: ubuntu-latest # Linux\r\n    steps:\r\n      - run: \"npm test\" # quoted\r\n";
        let (out, _) = edited(text, &["test"]);
        assert_eq!(
            out,
            "# Tests.\r\njobs:\r\n  # The only job.\r\n  test:\r\n    runs-on: ubuntu-latest # Linux\r\n    permissions:\r\n      contents: read\r\n      id-token: write\r\n    steps:\r\n      - run: \"npm test\" # quoted\r\n"
        );
        let open = "jobs:\n  test:\n    steps:\n      - run: npm test\n    permissions:\n      contents: read";
        let (out, _) = edited(open, &["test"]);
        assert_eq!(out, format!("{open}\n      id-token: write"));
        // Four-space indentation, and a job without runs-on.
        let four = "jobs:\n    test:\n        steps:\n            - run: npm test\n";
        let (out, _) = edited(four, &["test"]);
        assert_eq!(
            out,
            "jobs:\n    test:\n        permissions:\n            contents: read\n            id-token: write\n        steps:\n            - run: npm test\n"
        );
    }

    // covers: C-15
    #[test]
    fn block_runs_one_line_runs_and_runs_elsewhere() {
        let text = "\
jobs:
  a:
    steps:
      - name: All
        run: >-
          npm ci &&
          npm test
  b:
    steps:
      - run: bun test
      - run: npm test
        working-directory: web
  c:
    defaults:
      run:
        working-directory: web
    steps:
      - run: npm run lint
  d:
    steps:
      - run: 'npm run typecheck'
      - run: |
          echo \"key: value\"
          cd web
          npm test
      - uses: actions/setup-node@v4
        with:
          run: npm test
";
        let workflow = Workflow::parse("ci.yml".into(), text.into());
        let scripts: Vec<(&str, Vec<String>)> = workflow
            .jobs
            .iter()
            .map(|j| (j.id.as_str(), j.scripts.clone()))
            .collect();
        assert_eq!(
            scripts,
            [
                ("a", vec!["test".to_string()]),
                ("b", vec![]),
                ("c", vec![]),
                ("d", vec!["typecheck".to_string()]),
            ]
        );
    }
    // covers: C-15
    /// \[ADR 0055\] A folded block reads as the shell gets it: the line break between two lines
    /// at its indentation is a space, and one beside a blank line or a line indented further stays.
    /// Each command is still placed in the file, and a comment a fold joins swallows what follows.
    #[test]
    fn a_folded_block_reads_as_the_shell_gets_it() {
        let text = "\
jobs:
  a:
    steps:
      - run: >-
          hushrun run --scope cli registries --
          env \"X=1 2\"
          cargo test --locked

          cargo fmt --check
            --all
          npm test
      # A comment between steps, which isn't the block's.
      - run: >
          # not this:
          cargo test
";
        let workflow = Workflow::parse("ci.yml".into(), text.into());
        let steps = &workflow.jobs[0].steps;
        let shell: Vec<String> = steps[0]
            .sources
            .iter()
            .map(|source| source.shell().into_owned())
            .collect();
        assert_eq!(
            shell,
            [
                "hushrun run --scope cli registries -- env \"X=1 2\" cargo test --locked",
                "cargo fmt --check",
                "  --all",
                "npm test",
            ]
        );
        assert_eq!(
            steps[0].commands,
            "hushrun run --scope cli registries -- env \"X=1 2\" cargo test --locked\ncargo fmt --check\n  --all\nnpm test"
        );
        let first = &steps[0].sources[0];
        assert_eq!((first.line, first.at), (4, 10));
        assert_eq!(first.place(first.text.find("cargo").unwrap()), (6, 10));
        assert_eq!(scripts::line_runs(&steps[0].commands).len(), 1);
        // The shell reads `# not this: cargo test` as a comment, all of it.
        assert_eq!(steps[1].commands, "# not this: cargo test");
        let mut here = Some(String::new());
        assert!(scripts::init_commands(&steps[1].sources[0].shell(), &mut here).is_empty());
    }

    // covers: C-15
    /// \[ADR 0055\] A `# hushrun: leave` comment marks the commands init leaves: right above a
    /// step or a GitLab script item, at the end of its line, or right above a command in a block,
    /// with the reason that follows it.
    #[test]
    fn a_leave_comment_marks_what_init_leaves() {
        let text = "\
jobs:
  a:
    steps:
      # Sigstore's services change while the code doesn't.
      # hushrun: leave, public-good Sigstore
      - run: cargo test --test sigstore
      - run: cargo test --test registries # hushrun: leave
      - run: |
          cargo fmt --check
          # hushrun: leave: not this one
          cargo clippy

          cargo test
      # hushrun: leaves, which says nothing
      - run: cargo test --doc
      # hushrun: leave

      - run: cargo doc
";
        let workflow = Workflow::parse("ci.yml".into(), text.into());
        let marks: Vec<(String, Option<String>)> = workflow.jobs[0]
            .steps
            .iter()
            .flat_map(|step| &step.sources)
            .map(|source| (source.shell().into_owned(), source.leave.clone()))
            .collect();
        let mark = |text: &str, why: Option<&str>| (text.to_string(), why.map(str::to_string));
        assert_eq!(
            marks,
            [
                mark("cargo test --test sigstore", Some("public-good Sigstore")),
                mark("cargo test --test registries", Some("")),
                mark("cargo fmt --check", None),
                mark("cargo clippy", Some("not this one")),
                mark("cargo test", None),
                mark("cargo test --doc", None),
                // A blank line between them leaves the comment above nothing.
                mark("cargo doc", None),
            ]
        );
        let gitlab = "test:\n  script:\n    # hushrun: leave, by hand\n    - cargo test\n    - cargo clippy # hushrun: leave\n    - cargo fmt --check\n";
        let file = Workflow::parse_gitlab(gitlab.into());
        let marks: Vec<Option<&str>> = file.jobs[0].steps[0]
            .sources
            .iter()
            .map(|source| source.leave.as_deref())
            .collect();
        assert_eq!(marks, [Some("by hand"), Some(""), None]);
        for not in [
            "# hushrun leave",
            "# hushrun: leaves",
            "# hushrun: leave-me",
            "# not hushrun: leave",
        ] {
            assert_eq!(leave_marker(not), None, "{not}");
        }
        assert_eq!(
            leave_marker("## hushrun:leave; why").as_deref(),
            Some("why")
        );
    }

    // covers: C-22
    /// \[ADR 0055\] A job behind a condition on other jobs' outputs and results, as a changes
    /// filter writes it, may run; one behind any other condition may not.
    #[test]
    fn a_condition_on_other_jobs_lets_a_job_run() {
        for condition in [
            "needs.changes.outputs.cloud == 'true'",
            "${{ needs.changes.outputs.cli == 'true' }}",
            "needs.changes.outputs.cloud == 'true' || needs.changes.outputs.cli == 'true'",
            "always() && needs.build.result == 'success'",
            "!cancelled() && !contains(needs.*.result, 'failure')",
            "fromJSON(needs.plan.outputs.run)",
            "needs['build-linux'].outputs.ok != 'it''s off'",
            "success()",
        ] {
            assert!(on_other_jobs(condition), "{condition}");
        }
        for condition in [
            "github.ref == 'refs/heads/main'",
            "needs.changes.outputs.api == 'true' && github.event_name == 'push'",
            "failure()",
            "cancelled()",
            "matrix.os == 'linux'",
            "env.RUN == 'true'",
            "secrets.TOKEN != ''",
            "vars.ENABLED",
            "inputs.run",
            "steps.filter.outputs.api",
            "needs[matrix.job].result == 'success'",
            "needs.changes.outputs.api == \"true\"",
            "needs.changes.outputs.api == '${{ x }}'",
            "",
        ] {
            assert!(!on_other_jobs(condition), "{condition}");
        }
        let text = "on: push\njobs:\n  filtered:\n    needs: changes\n    if: >-\n      needs.changes.outputs.api == 'true' ||\n      needs.changes.outputs.web == 'true'\n    runs-on: ubuntu-latest\n    steps:\n      - run: hushrun run t -- x\n  guarded:\n    if: github.ref == 'refs/heads/main'\n    runs-on: ubuntu-latest\n    steps:\n      - run: hushrun run t -- x\n";
        let workflow = Workflow::parse("ci.yml".into(), text.into());
        let jobs: Vec<(bool, Option<String>)> = workflow
            .jobs
            .iter()
            .map(|job| (job.condition, job.only_ci()))
            .collect();
        assert_eq!(
            jobs,
            [
                (false, None),
                (true, Some("its job has a condition".to_string()))
            ]
        );
    }

    /// \[ADR 0050\] The composite actions of this repository a job uses are read with it, and
    /// those they use, however deep, and their steps run in the job: its scripts and its checks
    /// through hushrun. Another kind of action, one that isn't there, or one outside the job's
    /// checkout, adds none, and one that uses itself through another is walked once.
    // covers: C-15
    #[test]
    fn a_jobs_composite_actions_are_read_two_levels_deep() {
        let actions: std::collections::BTreeMap<&str, &str> = [
            (
                ".github/actions/a/action.yml",
                "runs:\n  using: composite\n  steps:\n    - run: npm test\n      shell: bash\n    - uses: ./.github/actions/b\n",
            ),
            (
                ".github/actions/b/action.yaml",
                "runs:\n  using: \"composite\"\n  steps:\n    - uses: ./.github/actions/c\n    - run: hushrun run lint -- ruff check .\n      shell: bash\n      working-directory: src\n",
            ),
            (
                ".github/actions/c/action.yml",
                "runs:\n  using: composite\n  steps:\n    - run: mypy .\n      shell: bash\n    - uses: ./.github/actions/b\n",
            ),
            (
                ".github/actions/node/action.yml",
                "runs:\n  using: node20\n  main: index.js\n",
            ),
        ]
        .into();
        let workflow = Workflow::parse(
            ".github/workflows/ci.yml".into(),
            "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - uses: ./.github/actions/a\n      - uses: ./.github/actions/node\n      - uses: ./.github/actions/missing/\n".into(),
        );
        let files = with_actions(vec![workflow], |path| {
            actions.get(path).map(ToString::to_string)
        });
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                ".github/workflows/ci.yml",
                ".github/actions/a/action.yml",
                ".github/actions/b/action.yaml",
                ".github/actions/c/action.yml"
            ]
        );
        let job = &files[0].jobs[0];
        assert_eq!(job.actions.len(), 3);
        assert_eq!(job.actions[2].dir.as_deref(), Ok(".github/actions/missing"));
        let steps = in_actions(job, &files);
        let ran: Vec<(&str, &str, Vec<usize>)> = steps
            .iter()
            .map(|step| {
                (
                    step.action.path.as_str(),
                    step.step.commands.trim(),
                    step.inside.iter().map(|(_, at)| *at).collect(),
                )
            })
            .collect();
        // c, three levels in, uses b again, which is walked once.
        assert_eq!(
            ran,
            [
                (".github/actions/a/action.yml", "npm test", vec![0]),
                (".github/actions/c/action.yml", "mypy .", vec![0, 0, 1]),
                (
                    ".github/actions/b/action.yaml",
                    "hushrun run lint -- ruff check .",
                    vec![1, 1]
                ),
            ]
        );
        assert_eq!(steps[2].step.dir.as_deref(), Ok("src"));
        assert!(steps.iter().all(|step| step.uses.index == 1));
        assert_eq!(job.scripts, ["test"]);
        assert!(job.actions_run_hushrun);
        // `uses: ./` counts from the workspace, where the job checks this repository out.
        let moved = Workflow::parse(
            ".github/workflows/ci.yml".into(),
            "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n        with:\n          path: src\n      - uses: ./src/.github/actions/a\n      - uses: ./.github/actions/a\n  bare:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: ./.github/actions/a\n".into(),
        );
        let uses = &moved.jobs[0].actions;
        assert_eq!(uses[0].dir.as_deref(), Ok(".github/actions/a"));
        assert!(uses[1].dir.is_err(), "{uses:?}");
        assert!(moved.jobs[1].actions[0].dir.is_err());
    }

    #[test]
    fn workflows_are_found_in_their_directory() {
        let dir = tempfile::tempdir().unwrap();
        let workflows = dir.path().join(".github/workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(workflows.join("b.yaml"), "jobs: {}\n").unwrap();
        std::fs::write(workflows.join("a.yml"), CI).unwrap();
        std::fs::write(workflows.join("notes.md"), "").unwrap();
        let found: Vec<String> = find(dir.path()).into_iter().map(|w| w.path).collect();
        assert_eq!(
            found,
            [".github/workflows/a.yml", ".github/workflows/b.yaml"]
        );
        assert!(find(&dir.path().join("none")).is_empty());
    }

    // covers: C-15, C-26
    #[test]
    fn a_gitlab_job_that_keeps_reports_says_so_in_its_variables() {
        const RUN: &str = "  script:\n    - pytest --junitxml=report.xml\n";
        const KEEPS: &str = "  artifacts:\n    reports:\n      junit: report.xml\n";
        const SAYS: &str = "HUSHRUN_REPORTS_READ: \"true\"";
        let edited = |text: &str| {
            let edit = Workflow::parse_gitlab(text.to_string()).edit(&["test"]);
            (edit.text.clone().unwrap_or_else(|| text.to_string()), edit)
        };
        // A job with no variables gets a block of its own.
        let (text, edit) = edited(&format!("test:\n{RUN}{KEEPS}"));
        assert_eq!(
            text,
            format!("test:\n  variables:\n    HUSHRUN_REPORTS_READ: \"true\"\n{RUN}{KEEPS}")
        );
        assert_eq!(
            edit.changes,
            [
                "job test: adds HUSHRUN_REPORTS_READ: \"true\" to its variables, since artifacts:reports:junit reads its checks' reports"
            ]
        );
        // One with variables gets one more, as they are indented.
        let (text, _) = edited(&format!(
            "test:\n  variables:\n      CI_DEBUG: 1\n{RUN}{KEEPS}"
        ));
        assert_eq!(
            text,
            format!(
                "test:\n  variables:\n      HUSHRUN_REPORTS_READ: \"true\"\n      CI_DEBUG: 1\n{RUN}{KEEPS}"
            )
        );
        // What a job says already stays, true or false: its own, through an anchor, or the
        // file's.
        for said in [
            format!("test:\n  variables:\n    HUSHRUN_REPORTS_READ: \"false\"\n{RUN}{KEEPS}"),
            format!(
                ".vars: &vars\n  variables:\n    HUSHRUN_REPORTS_READ: \"true\"\ntest:\n  <<: *vars\n{RUN}{KEEPS}"
            ),
            format!("variables:\n  HUSHRUN_REPORTS_READ: \"false\"\ntest:\n{RUN}{KEEPS}"),
        ] {
            let (text, edit) = edited(&said);
            assert_eq!(text, said);
            assert!(
                edit.changes.is_empty() && edit.manual.is_empty(),
                "{edit:?}"
            );
        }
        // A job that doesn't inherit the file's variables needs its own.
        let (text, _) = edited(&format!(
            "variables:\n  HUSHRUN_REPORTS_READ: \"true\"\ntest:\n  inherit:\n    variables: false\n{RUN}{KEEPS}"
        ));
        assert_eq!(
            text,
            format!(
                "variables:\n  HUSHRUN_REPORTS_READ: \"true\"\ntest:\n  variables:\n    HUSHRUN_REPORTS_READ: \"true\"\n  inherit:\n    variables: false\n{RUN}{KEEPS}"
            )
        );
        // Variables that come through << would be lost to a block of its own, and what goes
        // into an anchor goes to every job that takes it: those are for a person.
        let cases = [
            (
                format!(
                    ".defaults: &defaults\n  variables:\n    PIP_CACHE_DIR: .pip\ntest:\n  <<: *defaults\n{RUN}{KEEPS}"
                ),
                format!(
                    "job test: its variables come through <<, and variables of its own would replace them: add {SAYS} to them"
                ),
            ),
            (
                format!("test: &test\n{RUN}{KEEPS}"),
                format!(
                    "job test: other jobs can take its keys through &test: add {SAYS} to its variables"
                ),
            ),
            (
                format!("test:\n  variables: &vars\n    CI_DEBUG: 1\n{RUN}{KEEPS}"),
                format!(
                    "job test: other jobs can share its variables through &vars: add {SAYS} to them"
                ),
            ),
            (
                format!("test:\n  variables: {{CI_DEBUG: 1}}\n{RUN}{KEEPS}"),
                format!("job test: its variables are written on one line: add {SAYS} to them"),
            ),
        ];
        for (file, why) in cases {
            let (text, edit) = edited(&file);
            assert_eq!(text, file);
            assert_eq!(edit.manual, [why], "{file}");
        }
        // A job that keeps no reports says nothing, nor one that keeps only a dotenv report,
        // which passes variables to later jobs.
        for none in [
            format!("test:\n{RUN}"),
            format!("test:\n{RUN}  artifacts:\n    reports:\n      dotenv: build.env\n"),
        ] {
            let (text, edit) = edited(&none);
            assert_eq!(text, none);
            assert!(
                edit.changes.is_empty() && edit.manual.is_empty(),
                "{edit:?}"
            );
        }
    }
}
