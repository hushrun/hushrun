//! What init changes in CI's files (C-15): `hushrun run` in front of each check the table knows,
//! in place, and what the job needs to run it. Nothing else of a file changes: a command the table
//! doesn't know, or that can't skip safely, stays as it is, and the plan says why.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use hushrun_core::policy::Policy;

use crate::cmd::wrap::made_name;

use super::caches::Caches;
use super::checks::{self, Verdict};
use super::expansions;
use super::policy::RuleLevel;
use super::projects::{self, Layout};
use super::scripts;
use super::suggestions::{self, Suggestion};
use super::variants;
use super::workflows::{self, ActionUse, Changes, Host, Job, Runs, Step, Workflow};

/// What the root's package.json runs through hushrun, each command's words joined by spaces, and
/// the check it is there. \[ADR 0040\] CI running one of these itself runs that check: under the
/// script's name `hushrun run` gives both one key (C-9), so a pass of either skips the other.
pub type ScriptChecks = BTreeMap<String, String>;

/// One command init wraps.
#[derive(Debug, Clone)]
struct Wrap {
    /// The file the line is in.
    file: usize,
    /// \[ADR 0050\] The file of the job that runs it: `file`, unless the line is in a composite
    /// action the job uses.
    job_file: usize,
    job: String,
    step_line: usize,
    step_dash: usize,
    /// Its job installs hushrun itself.
    installed: bool,
    line: usize,
    at: usize,
    command: String,
    name: String,
    dir: String,
    /// A wrapped script runs this command, and `name` is that script's check.
    script: bool,
    /// \[ADR 0045\] It means little away from a Mac: it earns its level a rule, when it runs on one,
    /// which the check `Seen` of it says.
    earns_a_rule: bool,
    /// \[ADR 0050\] It sets variables first, `NAME=value`, which go through `env` so that they are
    /// part of the command, and of its key.
    env: bool,
    /// \[ADR 0055\] What CI sets of the variables its check reads to decide what it builds, as
    /// the shell words `NAME=value` that carry them into the line, through `env`.
    carried: Vec<String>,
}

/// \[ADR 0045\] A check in CI's files, wrapped or left: a command left today can go through hushrun
/// tomorrow, as the plan says of a job with variants, and a rule would hold it to a Mac.
#[derive(Debug, Clone)]
struct Seen {
    /// Where it runs, relative to the root; `None` when that can't be told.
    dir: Option<String>,
    /// It can stand under a rule: Xcode or Swift, in a job that can only be on a Mac.
    fits: bool,
}

/// A command of CI's files init leaves as it is, and why.
#[derive(Debug, Clone)]
pub struct Left {
    /// The file, as [`Workflow::path`] names it.
    pub file: String,
    /// \[ADR 0050\] The file of the job that runs it: `file`, unless the line is in a composite
    /// action the job uses.
    pub job_file: String,
    pub job: String,
    /// The line it starts on, 0-based, when init read it from one.
    pub line: Option<usize>,
    pub command: String,
    /// The check it would be, when the table knows it as one.
    pub check: Option<String>,
    pub why: String,
}

impl Left {
    /// What the plan says of it.
    pub fn text(&self) -> String {
        format!(
            "{}, job {}: {}: {}",
            self.file, self.job, self.command, self.why
        )
    }
}

/// A command init wraps, as its plan shows it.
#[derive(Debug, Clone)]
pub struct Wrapped {
    /// Its file, an index into the files [`Proposal::read_with`] read.
    pub file: usize,
    /// \[ADR 0050\] The file of the job that runs it: `file`, unless the line is in a composite
    /// action the job uses.
    pub job_file: usize,
    pub job: String,
    /// The line it starts on, 0-based.
    pub line: usize,
    pub check: String,
    /// The directory it runs in, relative to the root, `/`-separated: `""` at the root.
    pub dir: String,
    pub before: String,
    pub after: String,
}

/// \[ADR 0051\] What else places checks in scopes: the directories of package.json's wrapped
/// scripts, and the root's JavaScript workspace.
#[derive(Debug, Default)]
pub struct Placing {
    pub dirs: BTreeSet<String>,
    pub workspace: Option<super::workspaces::Workspace>,
}

/// What init would do to CI's files.
#[derive(Debug)]
pub struct Proposal {
    wraps: Vec<Wrap>,
    /// \[ADR 0049\] The files a running workflow calls, `uses: ./<file>`, and those they call.
    called: BTreeSet<String>,
    /// \[ADR 0049\] Each file a job of this repository calls, by the caller's file.
    calls: Vec<(String, String)>,
    /// \[ADR 0049\] The package.json scripts that run through hushrun once init is done, whose
    /// jobs say which variant and call each run is too.
    pub scripts: Vec<String>,
    /// \[ADR 0051\] The jobs that start one of those, or a script of a workspace package that runs
    /// through hushrun, by their file's index and id.
    pub script_jobs: BTreeSet<(usize, String)>,
    /// Commands left as they are, and why.
    pub left: Vec<Left>,
    /// \[ADR 0050\] Test scripts a person could wrap, with the line to use: init applies none.
    pub suggestions: Vec<Suggestion>,
    pub layout: Layout,
    /// \[ADR 0045\] Where `counts_from: {os: macos}` fits, as far as CI's files can tell.
    pub macos: Vec<RuleLevel>,
}

/// The step that installs hushrun in a GitHub job: the setup action of this release, which
/// installs the newest hushrun unless told otherwise (ADR 0035).
fn github_setup() -> String {
    format!(
        "uses: hushrun/hushrun/action/setup@v{}",
        env!("CARGO_PKG_VERSION")
    )
}

/// What puts hushrun on a GitLab job's PATH, from the script init writes beside the pipeline.
const GITLAB_SETUP: &str = "export PATH=\"$(dirname \"$(sh ci/install-hushrun.sh)\"):$PATH\"";

impl Proposal {
    /// [`Proposal::read_with`] with nothing to place: no package.json's wrapped scripts, and no
    /// JavaScript workspace at the root.
    #[cfg(test)]
    pub fn read(
        root: &Path,
        files: &[Workflow],
        existing: Option<&Policy>,
        javascript: bool,
        scripts: &ScriptChecks,
    ) -> Proposal {
        Proposal::read_with(
            root,
            files,
            existing,
            javascript,
            scripts,
            &Placing::default(),
        )
    }

    /// Reads `files`. `javascript` says whether JavaScript's commands count: a repository with a
    /// package.json at its root wraps those there. `scripts` are the commands it wraps there.
    /// \[ADR 0051\] `placing` holds the directories of package.json's wrapped scripts and the
    /// root's JavaScript workspace, which the scopes take too.
    pub fn read_with(
        root: &Path,
        files: &[Workflow],
        existing: Option<&Policy>,
        javascript: bool,
        scripts: &ScriptChecks,
        placing: &Placing,
    ) -> Proposal {
        let mut wraps = Vec::new();
        let mut left = Vec::new();
        let mut seen = Vec::new();
        // \[ADR 0050\] What the other jobs that run a composite action's line make of it: those
        // whose workflow doesn't run for a push or a pull request, and those that reach it deeper
        // than `ACTION_DEPTH`. They don't make it wanted, and it has to suit them too.
        let mut idle_wraps = Vec::new();
        let mut idle_left = Vec::new();
        // \[ADR 0050\] The jobs that reach each action, by its file: their files and ids.
        let mut users: BTreeMap<usize, BTreeSet<(usize, String)>> = BTreeMap::new();
        // \[ADR 0049\] A file only a call starts runs when a running workflow calls it.
        let called = called_files(files);
        let caches = Caches::read(root, files);
        // \[ADR 0050\] The repository's files, where a task runner's recipes are read.
        let read = |path: &str| std::fs::read(root.join(path)).ok();
        for (index, file) in files.iter().enumerate() {
            let runs = file.runs_for(None) != Runs::No || called.contains(&file.path);
            for job in &file.jobs {
                if runs {
                    // What it runs isn't here to read, or isn't on a Mac, and may be anywhere.
                    if job.calls_a_workflow || (job.uses_a_local_action && !job.on_macos) {
                        seen.push(Seen {
                            dir: None,
                            fits: false,
                        });
                    }
                    for step in &job.steps {
                        read_step(
                            index, file, job, step, None, javascript, &read, scripts, &caches,
                            &mut wraps, &mut left, &mut seen,
                        );
                        seen.extend(wrapped_already(job, step));
                    }
                }
                // \[ADR 0050\] The steps of this repository's composite actions it uses run in it.
                for step in workflows::in_actions(job, files) {
                    for (action, _) in &step.inside {
                        if let Some(at) = files.iter().position(|file| std::ptr::eq(file, *action))
                        {
                            users.entry(at).or_default().insert((index, job.id.clone()));
                        }
                    }
                    let through = Through {
                        index,
                        file,
                        uses: step.uses,
                        inside: &step.inside,
                    };
                    let near = runs && step.inside.len() <= workflows::ACTION_DEPTH;
                    let mut unseen = Vec::new();
                    let (into_wraps, into_left, into_seen) = match near {
                        true => (&mut wraps, &mut left, &mut seen),
                        false => (&mut idle_wraps, &mut idle_left, &mut unseen),
                    };
                    read_step(
                        step.file,
                        step.action,
                        job,
                        step.step,
                        Some(&through),
                        javascript,
                        &read,
                        scripts,
                        &caches,
                        into_wraps,
                        into_left,
                        into_seen,
                    );
                }
            }
        }
        settle_actions(&mut wraps, &mut left, idle_wraps, &idle_left, &users, files);
        let mut dirs: BTreeSet<String> = wraps.iter().map(|wrap| wrap.dir.clone()).collect();
        dirs.extend(placing.dirs.iter().cloned());
        let layout = projects::layout_with(root, &dirs, existing, placing.workspace.as_ref());
        let calls = files
            .iter()
            .flat_map(|file| {
                file.jobs
                    .iter()
                    .filter_map(|job| Some((file.path.clone(), job.calls.clone()?)))
            })
            .collect();
        let mut proposal = Proposal {
            wraps,
            called,
            calls,
            scripts: Vec::new(),
            script_jobs: BTreeSet::new(),
            left,
            suggestions: Vec::new(),
            layout,
            macos: Vec::new(),
        };
        proposal.settle_names(files);
        // \[ADR 0050\] A script a person may wrap tomorrow is a check a rule would hold to a Mac.
        let (called, js) = (&proposal.called, javascript);
        proposal.suggestions = suggestions::read(files, called, &proposal, existing, js, scripts);
        seen.extend(proposal.suggestions.iter().map(|s| Seen {
            dir: Some(s.dir.clone()),
            fits: false,
        }));
        proposal.macos = proposal.macos_levels(&seen, existing);
        proposal
    }

    /// \[ADR 0045\] The levels where this wraps a check that earns a rule, and every check fits one:
    /// the top, for a repository of one scope, or each scope. A wrong rule costs skips for ever, so
    /// anything unsure says no, and a person can always add one. A file that deploys can't hold a
    /// rule (C-8), nor one with `stages` its author is to move to `deploy`, one that writes `counts_from` anywhere has said what it wants, and one that lists
    /// checks may hold a check CI's files don't show.
    fn macos_levels(&self, seen: &[Seen], existing: Option<&Policy>) -> Vec<RuleLevel> {
        let theirs = existing.is_some_and(|policy| {
            policy.deploys() || !policy.counts_from.is_empty() || !policy.checks.is_empty()
        });
        if theirs {
            return Vec::new();
        }
        let level_of = |dir: &str| match (&self.layout, existing) {
            // Of the directories a check is left in too: `Layout` holds only what it wraps.
            (Layout::Declared(_), Some(policy)) => {
                projects::the_scope_of(policy, dir).map(RuleLevel::Scope)
            }
            (Layout::One(_), _) => Some(RuleLevel::Top),
            (layout, _) => layout.scope_of(dir).ok().flatten().map(RuleLevel::Scope),
        };
        let others: Vec<Option<RuleLevel>> = seen
            .iter()
            .filter(|check| !check.fits)
            .map(|check| check.dir.as_deref().and_then(level_of))
            .collect();
        // What can't be placed could be at any level.
        if others.contains(&None) {
            return Vec::new();
        }
        let earned = self.wraps.iter().filter(|wrap| wrap.earns_a_rule);
        let mut levels = Vec::new();
        for level in earned.filter_map(|wrap| level_of(&wrap.dir)) {
            if !others.contains(&Some(level.clone())) && !levels.contains(&level) {
                levels.push(level);
            }
        }
        levels
    }

    /// Keeps what a repository already set up gets: the commands its wrapped scripts run, in
    /// GitHub's workflows. The rest of CI's files was its people's to leave as it is.
    pub fn keep_script_runs(&mut self, files: &[Workflow]) {
        self.wraps
            .retain(|wrap| wrap.script && files[wrap.file].host == Host::GitHub);
        self.left.clear();
        self.suggestions.clear();
    }

    /// A check's name takes its job's when two jobs run the same command: they may run on
    /// different systems, or with different variables, which a key doesn't tell apart (C-9). A
    /// script's check keeps its name however many jobs run it, as `npm test` in two jobs does.
    fn settle_names(&mut self, files: &[Workflow]) {
        let mut jobs: BTreeMap<(String, String), BTreeSet<(usize, String)>> = BTreeMap::new();
        for wrap in &self.wraps {
            jobs.entry((wrap.dir.clone(), wrap.command.clone()))
                .or_default()
                .insert((wrap.file, wrap.job.clone()));
        }
        // \[ADR 0050\] A composite action's line is one line, whichever jobs run it: its passes are
        // each job's (ADR 0044), so it keeps its name.
        for wrap in self
            .wraps
            .iter_mut()
            .filter(|wrap| !wrap.script && wrap.file == wrap.job_file)
        {
            let several = jobs[&(wrap.dir.clone(), wrap.command.clone())].len() > 1;
            if several && let Some(name) = made_name(&format!("{}-{}", wrap.job, wrap.name)) {
                wrap.name = name.to_string();
            }
        }
        // One name, one command, so that a line of `hushrun ready` or the dashboard is one check:
        // a script's check keeps its, then the first keeps it, the next take their job's, then a
        // number.
        let mut taken: BTreeMap<(String, String), String> = self
            .wraps
            .iter()
            .filter(|wrap| wrap.script)
            .map(|wrap| ((wrap.dir.clone(), wrap.name.clone()), wrap.command.clone()))
            .collect();
        let mut named: BTreeMap<(usize, usize, usize), String> = BTreeMap::new();
        for wrap in self.wraps.iter_mut().filter(|wrap| !wrap.script) {
            let place = (wrap.file, wrap.line, wrap.at);
            if let Some(name) = named.get(&place) {
                wrap.name.clone_from(name);
                continue;
            }
            let dir = wrap.dir.clone();
            let free = |name: &str, taken: &BTreeMap<(String, String), String>| {
                taken
                    .get(&(dir.clone(), name.to_string()))
                    .is_none_or(|command| *command == wrap.command)
            };
            let mut name = wrap.name.clone();
            if !free(&name, &taken) {
                let with_job = made_name(&format!("{}-{name}", wrap.job))
                    .map_or_else(|| name.clone(), |made| made.to_string());
                let mut n = 1;
                name.clone_from(&with_job);
                while !free(&name, &taken) {
                    n += 1;
                    name = format!("{with_job}-{n}");
                }
            }
            taken.insert((dir.clone(), name.clone()), wrap.command.clone());
            named.insert(place, name.clone());
            wrap.name = name;
        }
        let (wraps, unscoped): (Vec<Wrap>, Vec<Wrap>) = std::mem::take(&mut self.wraps)
            .into_iter()
            .partition(|wrap| self.layout.scope_of(&wrap.dir).is_ok());
        self.wraps = wraps;
        for wrap in unscoped {
            let why = self.layout.scope_of(&wrap.dir).err().unwrap_or_default();
            self.left.push(Left {
                file: files[wrap.file].path.clone(),
                job_file: files[wrap.job_file].path.clone(),
                job: wrap.job,
                line: Some(wrap.line),
                command: wrap.command,
                check: Some(wrap.name),
                why,
            });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.wraps.is_empty()
    }

    /// The commands it wraps, in the order CI's files have them, with the line each becomes.
    pub fn wrapped(&self) -> Vec<Wrapped> {
        self.wraps
            .iter()
            .map(|wrap| Wrapped {
                file: wrap.file,
                job_file: wrap.job_file,
                job: wrap.job.clone(),
                line: wrap.line,
                check: wrap.name.clone(),
                dir: wrap.dir.clone(),
                before: wrap.command.clone(),
                after: format!("{}{}", self.inserted(wrap), wrap.command),
            })
            .collect()
    }

    /// Whether init installs hushrun in job `job` of file `index`, ahead of a check it wraps.
    pub fn installs_in(&self, index: usize, job: &str) -> bool {
        self.wraps
            .iter()
            .any(|wrap| wrap.job_file == index && wrap.job == job && !wrap.installed)
    }

    /// What goes in front of `wrap`'s command.
    fn inserted(&self, wrap: &Wrap) -> String {
        let scope = self
            .layout
            .scope_of(&wrap.dir)
            .ok()
            .flatten()
            .map_or_else(String::new, |scope| format!("--scope {scope} "));
        let env = match (wrap.env, wrap.carried.as_slice()) {
            (false, []) => String::new(),
            (_, carried) => carried
                .iter()
                .fold("env ".to_string(), |env, word| format!("{env}{word} ")),
        };
        format!("hushrun run {scope}{} -- {env}", wrap.name)
    }

    /// The checks, in the order CI's files have them, each once.
    pub fn names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for wrap in &self.wraps {
            if !names.contains(&wrap.name) {
                names.push(wrap.name.clone());
            }
        }
        names
    }

    /// Whether a GitLab job gets a wrapped check.
    pub fn touches_gitlab(&self, files: &[Workflow]) -> bool {
        self.wraps
            .iter()
            .any(|wrap| files[wrap.file].host == Host::GitLab)
    }

    /// \[ADR 0049\] The jobs of file `index` that need an ID token: those that run a check, and
    /// those that call a file of this repository whose jobs need one, since a called workflow
    /// can't have more permissions than its caller gives it.
    pub fn token_jobs(&self, index: usize, files: &[Workflow]) -> Vec<String> {
        let mut jobs = self.jobs_of(index);
        for job in &files[index].jobs {
            let calls = job
                .calls
                .as_deref()
                .is_some_and(|path| self.needs_a_token(path, files, &mut BTreeSet::new()));
            if calls && !jobs.contains(&job.id) {
                jobs.push(job.id.clone());
            }
        }
        jobs
    }

    /// Whether a job of the file at `path` runs a check, or calls a file that does.
    fn needs_a_token(&self, path: &str, files: &[Workflow], seen: &mut BTreeSet<String>) -> bool {
        if !seen.insert(path.to_string()) {
            return false;
        }
        let Some(index) = files.iter().position(|file| file.path == path) else {
            return false;
        };
        !self.checking_jobs(index, &files[index]).is_empty()
            || files[index]
                .jobs
                .iter()
                .filter_map(|job| job.calls.as_deref())
                .any(|called| self.needs_a_token(called, files, seen))
    }

    /// The jobs of file `index` that run a check, in order.
    pub fn jobs_of(&self, index: usize) -> Vec<String> {
        let mut jobs = Vec::new();
        for wrap in self.wraps.iter().filter(|wrap| wrap.job_file == index) {
            if !jobs.contains(&wrap.job) {
                jobs.push(wrap.job.clone());
            }
        }
        jobs
    }

    /// \[ADR 0049\] The jobs of file `index` that run a check through hushrun: a line init wraps,
    /// a package.json script that runs through hushrun, or a line through hushrun already.
    fn checking_jobs(&self, index: usize, file: &Workflow) -> Vec<String> {
        let mut jobs = self.jobs_of(index);
        let through_hushrun = file.jobs.iter().filter(|job| {
            job.actions_run_hushrun
                || job
                    .steps
                    .iter()
                    .any(|step| !scripts::line_runs(&step.commands).is_empty())
        });
        for job in file.running(&self.scripts).chain(through_hushrun) {
            if !jobs.contains(&job.id) {
                jobs.push(job.id.clone());
            }
        }
        // \[ADR 0051\] And those that start a workspace package's wrapped script.
        for job in &file.jobs {
            if self.script_jobs.contains(&(index, job.id.clone())) && !jobs.contains(&job.id) {
                jobs.push(job.id.clone());
            }
        }
        jobs
    }

    /// The checks a job of file `index` runs.
    pub fn checks_of(&self, index: usize, job: &str) -> Vec<String> {
        let mut names = Vec::new();
        for wrap in self
            .wraps
            .iter()
            .filter(|wrap| wrap.job_file == index && wrap.job == job)
        {
            if !names.contains(&wrap.name) {
                names.push(wrap.name.clone());
            }
        }
        names
    }

    /// What changes in file `index`.
    pub fn changes_of(&self, index: usize, file: &Workflow) -> Changes {
        let mut changes = Changes::default();
        let mut installs: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        // \[ADR 0050\] A composite action's line is edited once, whichever jobs run it.
        let mut edited: BTreeMap<(usize, usize), Vec<&str>> = BTreeMap::new();
        for wrap in self.wraps.iter().filter(|wrap| wrap.file == index) {
            let jobs = edited.entry((wrap.line, wrap.at)).or_default();
            if !jobs.contains(&wrap.job.as_str()) {
                jobs.push(&wrap.job);
            }
        }
        for wrap in self.wraps.iter().filter(|wrap| wrap.file == index) {
            let Some(jobs) = edited.remove(&(wrap.line, wrap.at)) else {
                continue;
            };
            let inserted = self.inserted(wrap);
            let who = match jobs.as_slice() {
                [one] => format!("job {one}"),
                many => format!("jobs {}", many.join(", ")),
            };
            changes.said.push(format!(
                "{who}: {} → {inserted}{}",
                wrap.command, wrap.command
            ));
            changes.inserts.push((wrap.line, wrap.at, inserted));
        }
        for wrap in self.wraps.iter().filter(|wrap| wrap.job_file == index) {
            if !wrap.installed {
                let first = installs
                    .entry(&wrap.job)
                    .or_insert((wrap.step_line, wrap.step_dash));
                first.0 = first.0.min(wrap.step_line);
            }
        }
        for (job, (line, dash)) in installs {
            let pad = " ".repeat(dash);
            let text = match file.host {
                Host::GitHub => github_setup(),
                Host::GitLab => GITLAB_SETUP.to_string(),
            };
            changes.before.push((line, vec![format!("{pad}- {text}")]));
            changes
                .said
                .push(format!("job {job}: installs hushrun first"));
        }
        if file.host == Host::GitHub {
            self.identities(index, file, &mut changes);
        }
        changes
    }

    /// \[ADR 0049\] What each job of file `index` that runs a check says of itself in its `env`, so
    /// that CI skips it per variant and per call (C-14): its variant, in a job of several, and in
    /// a file another calls, that file and its inputs.
    fn identities(&self, index: usize, file: &Workflow, changes: &mut Changes) {
        let called = self.called.contains(&file.path);
        let inputs = called
            && variants::parse(&file.path, file.source().as_bytes())
                .is_some_and(|yaml| variants::takes_inputs(&yaml));
        let holds = |name: &str, text: &str| match name {
            variants::VARIANT => variants::is_expression(text, "toJSON(matrix)"),
            variants::INPUTS => variants::is_expression(text, "toJSON(inputs)"),
            _ => text.trim().strip_prefix("./").unwrap_or(text.trim()) == file.path,
        };
        let mut says_its_file = false;
        let checking = self.checking_jobs(index, file);
        for job in file.jobs.iter().filter(|job| checking.contains(&job.id)) {
            let mut wanted: Vec<(&str, String)> = Vec::new();
            if job.matrix {
                wanted.push((variants::VARIANT, "${{ toJSON(matrix) }}".to_string()));
            }
            if called {
                wanted.push((variants::WORKFLOW, file.path.clone()));
                if inputs {
                    wanted.push((variants::INPUTS, "${{ toJSON(inputs) }}".to_string()));
                }
            }
            if wanted.is_empty() {
                continue;
            }
            let (added, manual) = file.env_needs(job, &wanted, &holds);
            changes.manual.extend(
                manual
                    .into_iter()
                    .map(|why| format!("job {}: {why}", job.id)),
            );
            let Some((line, lines)) = added else {
                continue;
            };
            let names: Vec<&str> = wanted
                .iter()
                .map(|(name, _)| *name)
                .filter(|name| {
                    lines
                        .iter()
                        .any(|added| added.trim_start().starts_with(name))
                })
                .collect();
            if names.contains(&variants::VARIANT) {
                changes.said.push(format!(
                    "Each variant of {} gets its own pass (HUSHRUN_VARIANT in its env).",
                    job.id
                ));
            }
            says_its_file |= names.contains(&variants::WORKFLOW);
            changes.after.push((line, lines));
        }
        if says_its_file {
            let callers: Vec<String> = self.callers_of(&file.path);
            changes.said.push(format!(
                "{} is called by {}: its jobs say which file they are, so CI can skip there too.",
                file.path,
                callers.join(", ")
            ));
        }
    }

    /// The files whose jobs call `path`, as the plan says them.
    fn callers_of(&self, path: &str) -> Vec<String> {
        self.calls
            .iter()
            .filter(|(_, called)| called == path)
            .map(|(caller, _)| caller.clone())
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect()
    }
}

/// \[ADR 0049\] The files of this repository that a running workflow calls, `uses: ./<file>`, and
/// those they call in turn: their jobs run, though only a call starts them.
fn called_files(files: &[Workflow]) -> BTreeSet<String> {
    let mut called = BTreeSet::new();
    let mut next: Vec<&Workflow> = files
        .iter()
        .filter(|file| file.host == Host::GitHub && file.runs_for(None) != Runs::No)
        .collect();
    while let Some(file) = next.pop() {
        for path in file.jobs.iter().filter_map(|job| job.calls.as_deref()) {
            if called.insert(path.to_string()) {
                next.extend(files.iter().filter(|other| other.path == path));
            }
        }
    }
    called
}

/// \[ADR 0040\] What CI runs itself that `scripts` run through hushrun, a line for each: it runs
/// every time, whatever passed elsewhere. `program` is how to start hushrun here.
pub fn outside_hushrun(files: &[&Workflow], scripts: &ScriptChecks, program: &str) -> Vec<String> {
    let (mut wraps, mut left, mut seen) = (Vec::new(), Vec::new(), Vec::new());
    // Only the lines a wrapped script runs count here, which no cache sets aside.
    let none = Caches::default();
    for (index, file) in files.iter().enumerate() {
        for job in &file.jobs {
            for step in &job.steps {
                // A target is never a script's command: no recipe to read.
                read_step(
                    index,
                    file,
                    job,
                    step,
                    None,
                    false,
                    &|_| None,
                    scripts,
                    &none,
                    &mut wraps,
                    &mut left,
                    &mut seen,
                );
            }
        }
    }
    wraps
        .iter()
        .filter(|wrap| wrap.script)
        .map(|wrap| {
            let file = files[wrap.file];
            let fix = match file.host {
                Host::GitHub => format!("run {program} init to put it through hushrun"),
                Host::GitLab => format!("put hushrun run {} -- in front of it", wrap.name),
            };
            format!(
                "CI runs {} itself in {} (job {}), so it won't skip there: {fix}.",
                wrap.command, file.path, wrap.job
            )
        })
        .collect()
}

/// \[ADR 0045\] The checks of `step` that go through hushrun already. A run names its own scope, so
/// where it stands isn't its directory's to say.
fn wrapped_already(job: &Job, step: &Step) -> Vec<Seen> {
    scripts::line_runs(&step.commands)
        .into_iter()
        .map(|found| {
            let tool = found.run.is_ok_and(|run| {
                let words: Vec<&str> = run.command.iter().map(String::as_str).collect();
                checks::fits_a_rule(&words)
            });
            Seen {
                dir: None,
                fits: job.on_macos && tool,
            }
        })
        .collect()
}

/// \[ADR 0050\] Where a step of a composite action of this repository runs: in a job of a
/// workflow, through the job's step that uses the action.
struct Through<'a> {
    /// The job's file, an index into the files read, and the file.
    index: usize,
    file: &'a Workflow,
    /// The job's step that uses the outermost action, which hushrun's setup goes above.
    uses: &'a ActionUse,
    /// The actions the step runs inside, innermost first, as [`workflows::InAction`] says.
    inside: &'a [(&'a Workflow, usize)],
}

/// \[ADR 0050\] Where a line is: its file, the line it starts on, and its command.
type Place = (String, usize, String);

/// \[ADR 0050\] A composite action's line is one line, whichever jobs run it, so it suits every
/// one of them, or is left for all.
///
/// - It is wanted when a job that runs for a push or a pull request reaches it within
///   `ACTION_DEPTH` actions. A line no such job wants stays as it is, saying nothing.
/// - Every job init reads that runs it then runs it through hushrun, a nightly's too, and gets
///   what any job that runs a check gets: `idle` joins `wraps`.
/// - It is left, saying why, when one of them leaves it; when a job init can't edit may run it,
///   since that job would run it without hushrun; and when the action may be another
///   repository's to use.
fn settle_actions(
    wraps: &mut Vec<Wrap>,
    left: &mut Vec<Left>,
    idle: Vec<Wrap>,
    idle_left: &[Left],
    users: &BTreeMap<usize, BTreeSet<(usize, String)>>,
    files: &[Workflow],
) {
    let of_wrap = |wrap: &Wrap| -> Place {
        (
            files[wrap.file].path.clone(),
            wrap.line,
            wrap.command.clone(),
        )
    };
    let of_left = |l: &Left| -> Option<Place> {
        l.line
            .filter(|_| l.file != l.job_file)
            .map(|line| (l.file.clone(), line, l.command.clone()))
    };
    let mut wanted: BTreeSet<Place> = left.iter().filter_map(of_left).collect();
    wanted.extend(
        wraps
            .iter()
            .filter(|wrap| wrap.file != wrap.job_file)
            .map(of_wrap),
    );
    // Why each is left, and in which job: one that leaves it first, then what no job says.
    let mut why: BTreeMap<Place, (String, Option<(String, String)>)> = BTreeMap::new();
    for l in left.iter().chain(idle_left) {
        if let Some(place) = of_left(l).filter(|place| wanted.contains(place)) {
            why.entry(place)
                .or_insert_with(|| (l.why.clone(), Some((l.job.clone(), l.job_file.clone()))));
        }
    }
    let mut unreachable: BTreeMap<String, Option<String>> = BTreeMap::new();
    for place in &wanted {
        let reason = unreachable
            .entry(place.0.clone())
            .or_insert_with(|| not_every_job(&place.0, users, files));
        if let Some(reason) = reason {
            why.entry(place.clone())
                .or_insert_with(|| (reason.clone(), None));
        }
    }
    let (kept, shared): (Vec<Wrap>, Vec<Wrap>) = std::mem::take(wraps)
        .into_iter()
        .partition(|wrap| wrap.file == wrap.job_file || !why.contains_key(&of_wrap(wrap)));
    *wraps = kept;
    wraps.extend(idle.into_iter().filter(|wrap| {
        let place = of_wrap(wrap);
        wanted.contains(&place) && !why.contains_key(&place)
    }));
    for wrap in shared {
        let (reason, from) = &why[&of_wrap(&wrap)];
        let job_file = files[wrap.job_file].path.clone();
        let reason = match from {
            Some((job, file)) if *job != wrap.job || *file != job_file => {
                format!("where job {job} of {file} runs it, {reason}")
            }
            _ => reason.clone(),
        };
        left.push(Left {
            file: files[wrap.file].path.clone(),
            job_file,
            line: Some(wrap.line),
            check: Some(wrap.name),
            why: reason,
            job: wrap.job,
            command: wrap.command,
        });
    }
}

/// \[ADR 0050\] Why a line of the composite action at `path` can't go through hushrun in every
/// job that runs it, when it can't: the action is outside `.github/`, where another repository may
/// use it; or a workflow's job may use it, as the workflow read as YAML says, that isn't among
/// the jobs init reads as reaching it, `users`, so that init can't give it hushrun.
fn not_every_job(
    path: &str,
    users: &BTreeMap<usize, BTreeSet<(usize, String)>>,
    files: &[Workflow],
) -> Option<String> {
    if !path.starts_with(".github/") {
        return Some(
            "its action isn't under .github/, so another repository may use it, where nothing installs hushrun"
                .to_string(),
        );
    }
    let index = files.iter().position(|file| file.path == path)?;
    let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let known = users.get(&index);
    let workflows = files
        .iter()
        .enumerate()
        .filter(|(_, file)| file.host == Host::GitHub && file.composite.is_none());
    for (at, file) in workflows {
        let Some(yaml) = variants::parse(&file.path, file.source().as_bytes()) else {
            if file.source().contains(dir) {
                return Some(format!(
                    "{} doesn't read as YAML, and may use its action",
                    file.path
                ));
            }
            continue;
        };
        for (id, job) in yaml["jobs"].as_object().into_iter().flatten() {
            let mut used = job["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|step| step["uses"].as_str()?.trim().strip_prefix("./"));
            let uses_it = used
                .any(|used| reaches(files, used.trim_end_matches('/'), dir, &mut BTreeSet::new()));
            if uses_it && !known.is_some_and(|known| known.contains(&(at, id.clone()))) {
                return Some(format!(
                    "job {id} of {} may use its action, and init can't give that job hushrun",
                    file.path
                ));
            }
        }
    }
    None
}

/// \[ADR 0050\] Whether a step's `uses: ./<used>` is the action in `dir`, with a checkout's
/// directory in front or not, or an action that uses it.
fn reaches(files: &[Workflow], used: &str, dir: &str, seen: &mut BTreeSet<String>) -> bool {
    if used == dir || used.ends_with(&format!("/{dir}")) {
        return true;
    }
    if !seen.insert(used.to_string()) {
        return false;
    }
    let Some((_, action)) = workflows::action_at(files, used) else {
        return false;
    };
    action
        .composite
        .iter()
        .flat_map(|composite| &composite.uses)
        .filter_map(|inner| inner.dir.as_ref().ok())
        .any(|inner| reaches(files, inner, dir, seen))
}

#[allow(clippy::too_many_arguments)]
fn read_step(
    index: usize,
    file: &Workflow,
    job: &Job,
    step: &Step,
    through: Option<&Through>,
    javascript: bool,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    scripts: &ScriptChecks,
    caches: &Caches,
    wraps: &mut Vec<Wrap>,
    left: &mut Vec<Left>,
    seen: &mut Vec<Seen>,
) {
    let mut here = Some(String::new());
    // \[ADR 0050\] The job's file, which a composite action's step runs in.
    let job_file = through.map_or(file, |through| through.file);
    // \[ADR 0050\] The job's file as YAML, and the actions the step runs inside, which say what
    // CI and the shell fill into a line: read once, when a line needs them.
    type Read = Option<(serde_json::Value, Vec<(serde_json::Value, usize)>)>;
    let yaml: std::cell::OnceCell<Read> = std::cell::OnceCell::new();
    for source in &step.sources {
        for found in scripts::init_commands(&source.shell(), &mut here) {
            let words: Vec<&str> = found.words.iter().map(String::as_str).collect();
            let command = words.join(" ");
            let tool: Vec<&str> = words
                .iter()
                .copied()
                .skip_while(|word| scripts::assignment(word))
                .collect();
            let words = tool;
            let starts = source.place(found.at).0;
            let leave = |left: &mut Vec<Left>, check: Option<&str>, why: &str| {
                left.push(Left {
                    file: file.path.clone(),
                    job_file: job_file.path.clone(),
                    job: job.id.clone(),
                    line: Some(starts),
                    command: command.clone(),
                    check: check.map(str::to_string),
                    why: why.to_string(),
                });
            };
            let at_root = matches!(
                (&step.dir, &found.moved),
                (Ok(dir), Some(moved)) if projects::joined(dir, moved).is_empty()
            );
            let script = scripts.get(&command).filter(|_| at_root);
            let here = match (&step.dir, &found.moved) {
                (Ok(dir), Some(moved)) => Some(projects::joined(dir, moved)),
                _ => None,
            };
            // \[ADR 0055\] A person's `# hushrun: leave` comment leaves the line, whatever else
            // would decide it. A command the table knows, or a wrapped script's, says so in the plan,
            // with the comment's reason.
            if let Some(why) = &source.leave {
                let verdict = match script {
                    Some(name) => Some(Verdict::Check(name.clone())),
                    None => checks::recognize_in(&words, javascript, here.as_deref(), read),
                };
                let check = match &verdict {
                    Some(Verdict::Check(name)) => Some(name.as_str()),
                    _ => None,
                };
                if verdict.is_some() {
                    leave(left, check, &left_by_comment(why));
                }
                continue;
            }
            // \[ADR 0050\] What a tool's remote cache reuses across machines is the tool's, before
            // any other reason to leave a line.
            if let (None, Some(dir)) = (script, &here)
                && let Some(aside) = caches.aside(&words, dir, javascript)
            {
                let (dir, fits) = (here.clone(), false);
                seen.extend(aside.check.as_ref().map(|_| Seen { dir, fits }));
                leave(left, aside.check.as_deref(), &aside.why);
                continue;
            }
            // \[ADR 0050\] A task runner's target, by its recipe, where the step runs.
            let verdict = checks::recognize_in(&words, javascript, here.as_deref(), read);
            let name = match (script, verdict) {
                (Some(name), _) => name.clone(),
                (None, None) => {
                    // JavaScript's, which a package.json at the root keeps out of CI's files:
                    // init doesn't wrap it, and it is still a check where it runs.
                    if matches!(checks::recognize(&words, true), Some(Verdict::Check(_))) {
                        seen.push(Seen {
                            dir: here,
                            fits: false,
                        });
                    }
                    continue;
                }
                (None, Some(Verdict::Leave(why))) => {
                    leave(left, None, why);
                    continue;
                }
                (None, Some(Verdict::Check(name))) => name,
            };
            seen.push(Seen {
                dir: here,
                fits: job.on_macos && checks::fits_a_rule(&words),
            });
            if let Some(why) = &found.leave {
                leave(left, Some(&name), why);
                continue;
            }
            // \[ADR 0055\] What the check reads from CI's env to decide what it builds, but what
            // the line sets itself.
            let sets_itself = |name: &&str| {
                found
                    .words
                    .iter()
                    .take_while(|word| scripts::assignment(word))
                    .any(|word| word.split('=').next() == Some(*name))
            };
            let reads: Vec<&str> = expansions::read_from_env(&words)
                .iter()
                .copied()
                .filter(|name| !sets_itself(name))
                .collect();
            let mut carry = Vec::new();
            // \[ADR 0050\] Words CI or the shell fills in wrap when each is safe to record, and
            // variables set first, through `env`, in a bash step. \[ADR 0055\] So do the values
            // CI sets of what the check reads, carried into the line.
            if found.prefixed || found.fills_in || !reads.is_empty() {
                let read = yaml.get_or_init(|| {
                    if job_file.host != Host::GitHub {
                        return None;
                    }
                    let workflow = variants::parse(&job_file.path, job_file.source().as_bytes())?;
                    let inside = through.map_or(&[][..], |through| through.inside);
                    let actions = inside
                        .iter()
                        .map(|(action, at)| {
                            variants::parse(&action.path, action.source().as_bytes())
                                .map(|yaml| (yaml, *at))
                        })
                        .collect::<Option<Vec<_>>>()?;
                    Some((workflow, actions))
                });
                let (workflow, actions) = match read {
                    Some((workflow, actions)) => (
                        Some(workflow),
                        actions.iter().map(|(yaml, at)| (yaml, *at)).collect(),
                    ),
                    None => (None, Vec::new()),
                };
                let at = through.map_or(step.index, |through| through.uses.index);
                if (found.prefixed || found.fills_in)
                    && let Err(why) = expansions::judged(&found, workflow, &job.id, at, &actions)
                {
                    leave(left, Some(&name), &why);
                    continue;
                }
                let here = Here {
                    file: job_file,
                    yaml: workflow,
                    job: &job.id,
                    step: at,
                    actions: &actions,
                };
                match carried(&reads, &here, source.scalar) {
                    Ok(words) => carry = words,
                    Err(why) => {
                        leave(left, Some(&name), &why);
                        continue;
                    }
                }
            }
            // \[ADR 0047\] A line that writes a report is wrapped: CI runs it every time when a later
            // step reads the report (C-26), and a laptop or an agent skips it on a pass.
            // \[ADR 0049\] So is one of a job with variants: each says which it is (C-14). A file
            // that doesn't read as YAML can't say it, nor can init write it there.
            if !file.reads_as_yaml || !job_file.reads_as_yaml {
                leave(
                    left,
                    Some(&name),
                    "its file doesn't read as YAML, so init can't tell its job's variants",
                );
                continue;
            }
            let dir = match (&step.dir, &found.moved) {
                (Err(_), _) => continue,
                (_, None) => {
                    leave(
                        left,
                        Some(&name),
                        "a `cd` before it moves it where init can't follow",
                    );
                    continue;
                }
                (Ok(dir), Some(moved)) => projects::joined(dir, moved),
            };
            let (line, at) = source.place(found.at);
            wraps.push(Wrap {
                file: index,
                job_file: through.map_or(index, |through| through.index),
                job: job.id.clone(),
                // \[ADR 0050\] hushrun's setup goes above the job's step that uses the action.
                step_line: through.map_or(step.line, |through| through.uses.line),
                step_dash: through.map_or(step.dash, |through| through.uses.dash),
                installed: job.installs_hushrun,
                line,
                at,
                command,
                name,
                dir,
                script: script.is_some(),
                earns_a_rule: checks::earns_a_rule(&words),
                env: found.prefixed,
                carried: carry,
            });
        }
    }
}

/// \[ADR 0055\] Why init leaves a line a `# hushrun: leave` comment marks, `why` being the reason
/// the comment gives.
fn left_by_comment(why: &str) -> String {
    match why {
        "" => "a comment says to leave it".to_string(),
        why => format!("a comment says to leave it: {why}"),
    }
}

/// \[ADR 0055\] Where a line runs, for what CI sets there: its job's file, read as YAML when it
/// is GitHub's and reads, the job, its step, and the composite actions it runs inside.
struct Here<'a> {
    file: &'a Workflow,
    yaml: Option<&'a serde_json::Value>,
    job: &'a str,
    step: usize,
    actions: &'a [(&'a serde_json::Value, usize)],
}

/// \[ADR 0055\] The words that carry into a line what CI sets of `names`, the variables its
/// check reads to decide what it builds, as `NAME=value`s quoted to fit `scalar`, its YAML; or why
/// init leaves the line. A run elsewhere then runs with them too, and a pass without them never
/// skips CI's. GitHub's values are read as [`expansions::set_by_ci`] reads them, and go through
/// `env`, which only a bash step has. A GitLab job that sets one leaves the line, as a line that
/// sets variables first is left there.
fn carried(names: &[&str], here: &Here, scalar: workflows::Scalar) -> Result<Vec<String>, String> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let Some(yaml) = here.yaml else {
        if here.file.host != Host::GitLab {
            // A file that doesn't read as YAML is left for that.
            return Ok(Vec::new());
        }
        let doc = variants::parse(&here.file.path, here.file.source().as_bytes());
        let sets = names.iter().find(|name| {
            doc.as_ref().is_none_or(|doc| {
                !doc[here.job]["variables"][**name].is_null() || !doc["variables"][**name].is_null()
            })
        });
        return match sets {
            Some(name) => Err(format!(
                "its job sets {name}, which init can't carry into a GitLab line"
            )),
            None => Ok(Vec::new()),
        };
    };
    let scope = expansions::Scope {
        file: yaml,
        job: here.job,
        step: Some(here.step),
        actions: here.actions,
    };
    let (mut words, mut set) = (Vec::new(), Vec::new());
    for name in names {
        let Some(value) = expansions::set_by_ci(name, &scope)? else {
            continue;
        };
        let word = shell_word(&format!("{name}={value}"), scalar).ok_or_else(|| {
            format!("its job sets {name} to a value init can't write into its line")
        })?;
        words.push(word);
        set.push(*name);
    }
    if !words.is_empty() && !expansions::bash(&scope) {
        return Err(format!(
            "its job sets {}, which only a bash step can carry into the line",
            set.join(" and ")
        ));
    }
    Ok(words)
}

/// \[ADR 0055\] `word` as a shell reads it back, written to fit in `scalar`: a word with a space
/// or another character the shell reads goes between `'`s, or `"`s in a line between `'`s. None
/// when no way of writing it fits.
fn shell_word(word: &str, scalar: workflows::Scalar) -> Option<String> {
    use workflows::Scalar;
    if word.contains(['\n', '\r']) {
        return None;
    }
    let plain = word
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    let written = match (plain, scalar) {
        (true, _) => word.to_string(),
        // Between `"`s the shell reads `$`, `` ` ``, `\\` and `!`: only a word with none of them.
        (false, Scalar::Single) if !word.contains(['"', '$', '`', '\\', '!', '\'']) => {
            format!("\"{word}\"")
        }
        (false, Scalar::Single) => return None,
        (false, _) => format!("'{}'", word.replace('\'', "'\\''")),
    };
    let fits = match scalar {
        Scalar::Plain => !written.contains(": ") && !written.contains(" #"),
        Scalar::Double => !written.contains(['"', '\\']),
        Scalar::Single | Scalar::Block => true,
    };
    fits.then_some(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::setup::checks::ROWS;

    fn edited(root: &Path, text: &str, scripts: bool) -> (String, Proposal) {
        let file = Workflow::parse(".github/workflows/ci.yml".into(), text.to_string());
        let files = [file];
        let proposal = Proposal::read(root, &files, None, scripts, &ScriptChecks::new());
        let jobs = proposal.jobs_of(0);
        let jobs: Vec<&str> = jobs.iter().map(String::as_str).collect();
        let edit = files[0].edit_with(&jobs, &proposal.changes_of(0, &files[0]));
        (edit.text.unwrap_or_else(|| text.to_string()), proposal)
    }

    const CHECKOUT: &str = "      - uses: actions/checkout@v5\n";

    fn job(run: &str) -> String {
        format!("on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}{run}")
    }

    const SETUP: &str = concat!(
        "      - uses: hushrun/hushrun/action/setup@v",
        env!("CARGO_PKG_VERSION"),
        "\n"
    );

    /// Every language's row, through a workflow: this is what "adding a row" buys.
    // covers: C-15
    #[test]
    fn every_row_wraps_or_leaves_its_example_in_a_workflow() {
        let dir = tempfile::tempdir().unwrap();
        for row in ROWS {
            let run = format!("      - run: {}\n", row.example);
            let text = job(&run);
            let (out, proposal) = edited(dir.path(), &text, true);
            if row.leave.is_empty() {
                let name = proposal.names().remove(0);
                let wrapped = format!("      - run: hushrun run {name} -- {}\n", row.example);
                assert_eq!(
                    out,
                    job(&format!("{SETUP}{wrapped}")).replace(
                        "    runs-on: ubuntu-latest\n",
                        "    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n      id-token: write\n",
                    ),
                    "{} {}",
                    row.language,
                    row.example
                );
            } else {
                assert_eq!(out, text, "{}", row.example);
                assert!(
                    proposal.left[0].text().contains(row.leave),
                    "{:?}",
                    proposal.left
                );
            }
        }
    }

    // covers: C-15
    #[test]
    fn a_block_keeps_every_line_it_doesnt_wrap() {
        let dir = tempfile::tempdir().unwrap();
        let text = job(
            "      - name: Everything\n        run: |\n          set -e\n          cargo fetch\n          cargo test --locked && cargo clippy\n          # cargo check\n          echo done\n",
        );
        let (out, proposal) = edited(dir.path(), &text, false);
        assert!(
            out.contains(
                "          cargo fetch\n          hushrun run test -- cargo test --locked && hushrun run clippy -- cargo clippy\n          # cargo check\n"
            ),
            "{out}"
        );
        assert_eq!(proposal.names(), ["test", "clippy"]);
        // What goes before it goes above its step's name.
        assert!(
            out.contains(&format!("{CHECKOUT}{SETUP}      - name: Everything\n")),
            "{out}"
        );
    }

    // covers: C-15
    #[test]
    fn quotes_and_comments_stay() {
        let dir = tempfile::tempdir().unwrap();
        let (out, _) = edited(
            dir.path(),
            &job("      - run: \"cargo test\" # all of it\n"),
            false,
        );
        assert!(
            out.contains("      - run: \"hushrun run test -- cargo test\" # all of it\n"),
            "{out}"
        );
        let (out, _) = edited(dir.path(), &job("      - run: 'go test ./...'\n"), false);
        assert!(
            out.contains("      - run: 'hushrun run test -- go test ./...'\n"),
            "{out}"
        );
    }

    // covers: C-15
    #[test]
    fn what_cant_skip_safely_stays_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        for (run, why) in [
            ("      - run: cargo test | tee out.log\n", "pipes"),
            ("      - run: cargo deny check\n", "advisory database"),
            (
                "      - run: go test ./... -count=1 *.go\n",
                "the shell makes",
            ),
            (
                "      - run: |\n          cd \"$DIR\"\n          cargo test\n",
                "`cd`",
            ),
            // [ADR 0050] What may put a secret in the record.
            (
                "      - run: cargo test --features ${{ secrets.FEATURES }}\n",
                "it passes a secret on its command line (${{ secrets.FEATURES }}), which a result would record",
            ),
            (
                "      - run: cargo test $FEATURES\n",
                "an earlier step sets FEATURES, and hushrun can't tell it holds no secret",
            ),
            (
                "      - run: go test $(go list ./...)\n",
                "a command fills in its words",
            ),
        ] {
            let text = job(run);
            let (out, proposal) = edited(dir.path(), &text, false);
            assert_eq!(out, text, "{run}");
            assert!(
                proposal.left[0].text().contains(why),
                "{run}: {:?}",
                proposal.left
            );
        }
        // Words that only look like a check, and a here-document's lines.
        for run in [
            "      - run: cargo build --locked\n",
            "      - run: |\n          cat <<EOF\n          cargo test\n          EOF\n",
        ] {
            let text = job(run);
            let (out, proposal) = edited(dir.path(), &text, false);
            assert_eq!(out, text, "{run}");
            assert!(proposal.is_empty() && proposal.left.is_empty(), "{run}");
        }
    }

    /// \[ADR 0050\] What CI fills in that is safe to record wraps, and the key then differs per value;
    /// variables set first go through `env`, in a bash step.
    // covers: C-15
    #[test]
    fn what_ci_fills_in_wraps_when_it_is_safe_to_record() {
        let dir = tempfile::tempdir().unwrap();
        for (run, wrapped) in [
            (
                "      - run: go test ${{ matrix.pkg }}\n",
                "      - run: hushrun run test -- go test ${{ matrix.pkg }}\n",
            ),
            (
                "      - run: pytest --shard=${{ strategy.job-index }} $GITHUB_WORKSPACE/tests\n",
                "      - run: hushrun run test -- pytest --shard=${{ strategy.job-index }} $GITHUB_WORKSPACE/tests\n",
            ),
            (
                "      - run: GOARCH=386 go test ./...\n",
                "      - run: hushrun run test -- env GOARCH=386 go test ./...\n",
            ),
            (
                "      - run: RUST_LOG=debug CARGO_TERM_COLOR=always cargo test\n",
                "      - run: hushrun run test -- env RUST_LOG=debug CARGO_TERM_COLOR=always cargo test\n",
            ),
        ] {
            let (out, proposal) = edited(dir.path(), &job(run), false);
            assert!(out.contains(wrapped), "{run}: {out}");
            assert!(proposal.left.is_empty(), "{run}: {:?}", proposal.left);
        }
        // A variable the job's env sets from a literal, or GitHub fills in safely.
        let text = job("      - run: cargo test $FEATURES --target $TARGET\n").replace(
            "    steps:\n",
            "    env:\n      FEATURES: --all-features\n      TARGET: ${{ matrix.target }}\n    steps:\n",
        );
        let (out, proposal) = edited(dir.path(), &text, false);
        assert!(
            out.contains(
                "      - run: hushrun run test -- cargo test $FEATURES --target $TARGET\n"
            ),
            "{out}"
        );
        assert!(proposal.left.is_empty(), "{:?}", proposal.left);
        // On Windows a step runs pwsh, which sets no variable in front of a command.
        let windows = job("      - run: GOARCH=386 go test ./...\n")
            .replace("runs-on: ubuntu-latest", "runs-on: windows-latest");
        let (out, proposal) = edited(dir.path(), &windows, false);
        assert_eq!(out, windows);
        assert!(
            proposal.left[0].text().contains("it sets GOARCH=386 first"),
            "{:?}",
            proposal.left
        );
    }

    /// \[ADR 0055\] A `# hushrun: leave` comment leaves its line, with the comment's reason, and a
    /// job with nothing else to wrap gets nothing: no setup, no ID token.
    // covers: C-15
    #[test]
    fn a_leave_comment_keeps_init_off_its_line() {
        let dir = tempfile::tempdir().unwrap();
        let text = "on: push\njobs:\n  sigstore:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      # hushrun: leave, public-good Sigstore changes while the code doesn't\n      - run: cargo test --test sigstore\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo deny check # hushrun: leave\n      - run: cargo test --locked\n";
        let (out, proposal) = edited(dir.path(), text, false);
        assert!(
            out.contains("      - run: cargo test --test sigstore\n"),
            "{out}"
        );
        assert!(
            out.contains("      - run: hushrun run test -- cargo test --locked\n"),
            "{out}"
        );
        assert_eq!(proposal.jobs_of(0), ["test"]);
        let left: Vec<String> = proposal.left.iter().map(Left::text).collect();
        assert_eq!(
            left,
            [
                ".github/workflows/ci.yml, job sigstore: cargo test --test sigstore: a comment says to leave it: public-good Sigstore changes while the code doesn't",
                ".github/workflows/ci.yml, job test: cargo deny check: a comment says to leave it",
            ]
        );
    }

    /// \[ADR 0055\] What CI sets of the variables Cargo reads to decide what it builds goes into a
    /// wrapped cargo line through `env`, quoted to fit the line, so that a run elsewhere has it
    /// too. What the line sets itself stays its own, and what can't be carried leaves the line.
    // covers: C-15
    #[test]
    fn what_cargo_reads_from_cis_env_goes_into_the_line() {
        let dir = tempfile::tempdir().unwrap();
        let with_env = |run: &str, env: &str| {
            job(run).replace("    steps:\n", &format!("    env:\n{env}    steps:\n"))
        };
        for (run, env, wrapped) in [
            (
                "      - run: cargo doc --workspace --no-deps\n",
                "      RUSTDOCFLAGS: -D warnings\n",
                "      - run: hushrun run rustdoc -- env 'RUSTDOCFLAGS=-D warnings' cargo doc --workspace --no-deps\n",
            ),
            (
                "      - run: cargo test --locked\n",
                "      RUSTFLAGS: -Dwarnings\n      RUSTDOCFLAGS: --cfg docsrs\n",
                "      - run: hushrun run test -- env RUSTFLAGS=-Dwarnings 'RUSTDOCFLAGS=--cfg docsrs' cargo test --locked\n",
            ),
            // A line between single quotes takes double ones.
            (
                "      - run: 'cargo clippy -- -D warnings'\n",
                "      RUSTFLAGS: -C debuginfo=0\n",
                "      - run: 'hushrun run clippy -- env \"RUSTFLAGS=-C debuginfo=0\" cargo clippy -- -D warnings'\n",
            ),
            // What the line sets itself is its own, and `cargo fmt` builds nothing.
            (
                "      - run: RUSTDOCFLAGS=-Dwarnings cargo doc\n",
                "      RUSTDOCFLAGS: -D warnings\n",
                "      - run: hushrun run rustdoc -- env RUSTDOCFLAGS=-Dwarnings cargo doc\n",
            ),
            (
                "      - run: cargo fmt --check\n",
                "      RUSTFLAGS: -Dwarnings\n",
                "      - run: hushrun run fmt -- cargo fmt --check\n",
            ),
            // A block holds any text, and a value CI fills in safely is part of the key.
            (
                "      - run: |\n          cargo doc --no-deps\n",
                "      RUSTDOCFLAGS: \"-D warnings # all\"\n",
                "          hushrun run rustdoc -- env 'RUSTDOCFLAGS=-D warnings # all' cargo doc --no-deps\n",
            ),
            (
                "      - run: cargo test\n",
                "      RUSTFLAGS: ${{ matrix.flags }}\n",
                "      - run: hushrun run test -- env 'RUSTFLAGS=${{ matrix.flags }}' cargo test\n",
            ),
        ] {
            let text = with_env(run, env);
            let (out, proposal) = edited(dir.path(), &text, false);
            assert!(out.contains(wrapped), "{run}: {out}");
            assert!(proposal.left.is_empty(), "{run}: {:?}", proposal.left);
        }
        // What can't be carried leaves the line.
        let earlier = "      - run: echo \"RUSTFLAGS=$(cat flags)\" >> \"$GITHUB_ENV\"\n      - run: cargo test\n";
        for (text, why) in [
            (
                with_env(
                    "      - run: cargo doc\n",
                    "      RUSTDOCFLAGS: ${{ secrets.FLAGS }}\n",
                ),
                "it passes a secret on its command line (RUSTDOCFLAGS)",
            ),
            (
                with_env(
                    "      - run: cargo doc\n",
                    "      RUSTDOCFLAGS: \"-D warnings # all\"\n",
                ),
                "its job sets RUSTDOCFLAGS to a value init can't write into its line",
            ),
            (
                job(earlier),
                "an earlier step may set RUSTFLAGS, to what only run time knows",
            ),
            (
                with_env("      - run: cargo test\n", "      RUSTFLAGS: -Dwarnings\n")
                    .replace("runs-on: ubuntu-latest", "runs-on: windows-latest"),
                "its job sets RUSTFLAGS, which only a bash step can carry into the line",
            ),
        ] {
            let (out, proposal) = edited(dir.path(), &text, false);
            assert_eq!(out, text);
            assert!(
                proposal.left[0].text().contains(why),
                "{text}: {:?}",
                proposal.left
            );
        }
    }

    // covers: C-15, C-26
    #[test]
    fn a_line_that_writes_a_report_is_wrapped() {
        let dir = tempfile::tempdir().unwrap();
        for (run, wrapped) in [
            (
                "      - run: go test -coverprofile=c.out ./...\n",
                "      - run: hushrun run test -- go test -coverprofile=c.out ./...\n",
            ),
            (
                "      - run: pytest --junitxml=out.xml\n",
                "      - run: hushrun run test -- pytest --junitxml=out.xml\n",
            ),
        ] {
            let (out, proposal) = edited(dir.path(), &job(run), false);
            assert!(out.contains(wrapped), "{run}: {out}");
            assert!(proposal.left.is_empty(), "{run}: {:?}", proposal.left);
        }
    }

    // covers: C-14, C-15
    #[test]
    fn a_matrix_of_one_is_no_matrix_and_a_matrix_of_several_says_which_variant_runs() {
        let dir = tempfile::tempdir().unwrap();
        let of = |matrix: &str| {
            format!(
                "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    strategy:\n      fail-fast: false\n      matrix:\n{matrix}    steps:\n{CHECKOUT}      - run: cargo test\n"
            )
        };
        let (_, one) = edited(dir.path(), &of("        os: [ubuntu-latest]\n"), false);
        assert_eq!(one.names(), ["test"]);
        let (out, _) = edited(dir.path(), &of("        os: [ubuntu-latest]\n"), false);
        assert!(!out.contains("HUSHRUN_VARIANT"), "{out}");
        // \[ADR 0049\] Each variant says which it is, so the line is wrapped too.
        for several in [
            "        os: [ubuntu-latest, macos-latest]\n",
            "        os:\n          - a\n          - b\n",
            "        os: [a]\n        include:\n          - os: b\n",
            "        os: ${{ fromJSON(needs.a.outputs.os) }}\n",
            "        os: [\n          a,\n          b\n        ]\n",
        ] {
            let (out, proposal) = edited(dir.path(), &of(several), false);
            assert!(
                out.contains("      - run: hushrun run test -- cargo test\n"),
                "{out}"
            );
            assert!(
                out.contains(
                    "    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n      id-token: write\n    env:\n      HUSHRUN_VARIANT: ${{ toJSON(matrix) }}\n    strategy:\n"
                ),
                "{several}: {out}"
            );
            assert!(proposal.left.is_empty(), "{:?}", proposal.left);
        }
    }

    /// What init writes of `text`, a workflow, then what it says of it and leaves to a person.
    fn changed(text: &str) -> (String, Vec<String>, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let files = [Workflow::parse(
            ".github/workflows/ci.yml".into(),
            text.to_string(),
        )];
        let proposal = Proposal::read(dir.path(), &files, None, false, &ScriptChecks::new());
        let jobs = proposal.token_jobs(0, &files);
        let jobs: Vec<&str> = jobs.iter().map(String::as_str).collect();
        let edit = files[0].edit_with(&jobs, &proposal.changes_of(0, &files[0]));
        (
            edit.text.unwrap_or_else(|| text.to_string()),
            edit.changes,
            edit.manual,
        )
    }

    /// A job of two variants that runs `cargo test`, with `env` for what its env is.
    fn in_variants(env: &str) -> String {
        format!(
            "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n{env}    strategy:\n      matrix:\n        os: [a, b]\n    steps:\n{CHECKOUT}      - run: cargo test\n"
        )
    }

    const VARIANT_LINE: &str = "HUSHRUN_VARIANT: ${{ toJSON(matrix) }}";

    // covers: C-14, C-15
    #[test]
    fn a_job_with_variants_says_which_each_run_is_once() {
        let (out, said, manual) = changed(&in_variants(""));
        assert!(
            out.contains(&format!("    env:\n      {VARIANT_LINE}\n    strategy:\n")),
            "{out}"
        );
        assert!(
            said.contains(
                &"Each variant of test gets its own pass (HUSHRUN_VARIANT in its env).".to_string()
            ),
            "{said:?}"
        );
        assert!(manual.is_empty(), "{manual:?}");
        // Again, on what init wrote: nothing more.
        let (again, said, manual) = changed(&out);
        assert_eq!(again, out);
        assert!(said.is_empty() && manual.is_empty(), "{said:?} {manual:?}");
        // An env of its own gets the line first, at its entries' indentation.
        let (out, _, manual) = changed(&in_variants("    env:\n        A: b\n"));
        assert!(
            out.contains(&format!("    env:\n        {VARIANT_LINE}\n        A: b\n")),
            "{out}"
        );
        assert!(manual.is_empty(), "{manual:?}");
        // Spaces and case inside the expression are the same expression.
        let same = in_variants("    env:\n      HUSHRUN_VARIANT: ${{toJson( matrix )}}\n");
        let (out, said, manual) = changed(&same);
        assert!(!out.contains(VARIANT_LINE), "{out}");
        assert!(manual.is_empty() && !said.iter().any(|s| s.starts_with("Each variant")));
    }

    // covers: C-14, C-15
    #[test]
    fn a_value_a_person_set_stays_and_the_plan_says_what_it_should_be() {
        let text = in_variants("    env:\n      HUSHRUN_VARIANT: ${{ matrix.os }}\n");
        let (out, _, manual) = changed(&text);
        assert!(
            out.contains("      HUSHRUN_VARIANT: ${{ matrix.os }}\n    strategy:\n"),
            "{out}"
        );
        assert!(!out.contains(VARIANT_LINE), "{out}");
        assert_eq!(
            manual,
            [
                "job test: its env sets HUSHRUN_VARIANT to ${{ matrix.os }}: make it ${{ toJSON(matrix) }}, or no pass skips it there"
            ]
        );
    }

    // covers: C-15
    #[test]
    fn an_env_init_cant_edit_as_lines_gets_the_line_to_add() {
        for (env, why) in [
            (
                "    env: {A: b}\n",
                "its env is written on one line: add HUSHRUN_VARIANT: ${{ toJSON(matrix) }} to it",
            ),
            (
                "    env: &common\n      A: b\n",
                "its env is shared through &common: add HUSHRUN_VARIANT: ${{ toJSON(matrix) }} to it",
            ),
        ] {
            let text = in_variants(env);
            let (out, _, manual) = changed(&text);
            assert!(!out.contains(VARIANT_LINE), "{env}: {out}");
            assert_eq!(manual, [format!("job test: {why}")], "{env}");
        }
        // Another job's env, through an alias, or a merge.
        let shared = "on: push\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    env: &common\n      A: b\n    steps:\n      - run: echo\n  test:\n    runs-on: ubuntu-latest\n    env: *common\n    strategy:\n      matrix:\n        os: [a, b]\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo test\n";
        let (out, _, manual) = changed(shared);
        assert!(!out.contains(VARIANT_LINE), "{out}");
        assert_eq!(
            manual,
            [
                "job test: its env is shared through *common: add HUSHRUN_VARIANT: ${{ toJSON(matrix) }} to it"
            ]
        );
        let merged = "on: push\nx-job: &job\n  env:\n    A: b\njobs:\n  test:\n    <<: *job\n    runs-on: ubuntu-latest\n    strategy:\n      matrix:\n        os: [a, b]\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo test\n";
        let (out, _, manual) = changed(merged);
        assert!(!out.contains(VARIANT_LINE), "{out}");
        assert_eq!(
            manual,
            [
                "job test: its env comes through <<, and an env of its own would replace it: add HUSHRUN_VARIANT: ${{ toJSON(matrix) }} to it"
            ]
        );
    }

    // covers: C-15
    #[test]
    fn two_jobs_of_one_command_are_two_checks() {
        // They may run on two systems, which a key doesn't tell apart.
        let dir = tempfile::tempdir().unwrap();
        let text = format!(
            "on: push\njobs:\n  linux:\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}      - run: cargo test\n  mac:\n    runs-on: macos-latest\n    steps:\n{CHECKOUT}      - run: cargo test\n"
        );
        let (out, proposal) = edited(dir.path(), &text, false);
        assert_eq!(proposal.names(), ["linux-test", "mac-test"]);
        assert!(out.contains("hushrun run mac-test -- cargo test"), "{out}");
    }

    // covers: C-15
    #[test]
    fn one_name_is_one_command() {
        let dir = tempfile::tempdir().unwrap();
        let text = format!(
            "on: push\njobs:\n  unit:\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}      - run: cargo test --lib\n      - run: cargo test --doc\n  it:\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}      - run: cargo test --test a\n"
        );
        let (_, proposal) = edited(dir.path(), &text, false);
        assert_eq!(proposal.names(), ["test", "unit-test", "it-test"]);
    }

    /// `command` in a job `test` of each of a staging and a production workflow.
    fn two_pipelines(command: &str) -> Vec<Workflow> {
        ["staging", "production"]
            .map(|name| {
                Workflow::parse(
                    format!(".github/workflows/{name}.yml"),
                    job(&format!("      - run: {command}\n")),
                )
            })
            .into()
    }

    fn scripts(pairs: &[(&str, &str)]) -> ScriptChecks {
        pairs
            .iter()
            .map(|(command, name)| ((*command).to_string(), (*name).to_string()))
            .collect()
    }

    // covers: C-15
    #[test]
    fn a_command_a_script_runs_is_that_scripts_check_however_many_jobs_run_it() {
        let dir = tempfile::tempdir().unwrap();
        let files = two_pipelines("bun test");
        // Under the script's name, a pass of `bun run test` skips what CI runs.
        let shared = scripts(&[("bun test", "test")]);
        let proposal = Proposal::read(dir.path(), &files, None, false, &shared);
        assert_eq!(proposal.names(), ["test"]);
        assert_eq!(
            proposal.changes_of(1, &files[1]).said[0],
            "job test: bun test → hushrun run test -- bun test"
        );
        // With no script to share a key with, two jobs stay two checks.
        let proposal = Proposal::read(dir.path(), &files, None, false, &ScriptChecks::new());
        assert_eq!(proposal.names(), ["test-test"]);
    }

    // covers: C-15
    #[test]
    fn another_command_isnt_the_scripts_check() {
        let dir = tempfile::tempdir().unwrap();
        let files = two_pipelines("bun test");
        let other = scripts(&[("bun test --timeout 20000", "test")]);
        let proposal = Proposal::read(dir.path(), &files, None, false, &other);
        assert_eq!(proposal.names(), ["test-test"]);
    }

    // covers: C-15
    #[test]
    fn a_scripts_check_keeps_its_name_from_a_command_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let files = [Workflow::parse(
            "ci.yml".into(),
            job("      - run: cargo test --locked\n      - run: bun test\n"),
        )];
        let shared = scripts(&[("bun test", "test")]);
        let proposal = Proposal::read(dir.path(), &files, None, false, &shared);
        assert_eq!(proposal.names(), ["test-test", "test"]);
        let said = proposal.changes_of(0, &files[0]).said;
        assert!(
            said.contains(&"job test: bun test → hushrun run test -- bun test".to_string()),
            "{said:?}"
        );
    }

    // covers: C-15
    #[test]
    fn what_a_script_runs_counts_only_at_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let shared = scripts(&[("vitest run", "test")]);
        let read = |text: String| {
            let files = [Workflow::parse("ci.yml".into(), text)];
            Proposal::read(dir.path(), &files, None, false, &shared).names()
        };
        assert_eq!(read(job("      - run: vitest run\n")), ["test"]);
        assert!(
            read(job(
                "      - run: vitest run\n        working-directory: web\n"
            ))
            .is_empty()
        );
        assert!(read(job("      - run: npx vitest run\n")).is_empty());
        // Without a script, a repository with a package.json keeps these out of CI's files.
        let files = [Workflow::parse(
            "ci.yml".into(),
            job("      - run: vitest run\n"),
        )];
        assert!(Proposal::read(dir.path(), &files, None, false, &ScriptChecks::new()).is_empty());
    }

    // covers: C-15
    #[test]
    fn a_repository_set_up_gets_only_what_its_scripts_run() {
        let dir = tempfile::tempdir().unwrap();
        let files = [Workflow::parse(
            "ci.yml".into(),
            job("      - run: bun test\n      - run: cargo test --locked\n"),
        )];
        let shared = scripts(&[("bun test", "test")]);
        let mut proposal = Proposal::read(dir.path(), &files, None, false, &shared);
        assert_eq!(proposal.names().len(), 2);
        proposal.keep_script_runs(&files);
        assert_eq!(proposal.names(), ["test"]);
    }

    // covers: C-22
    #[test]
    fn what_ci_runs_outside_hushrun_is_said_once_a_job() {
        let shared = scripts(&[("bun test", "test")]);
        let files = two_pipelines("bun test");
        let files: Vec<&Workflow> = files.iter().collect();
        assert_eq!(
            outside_hushrun(&files, &shared, "npx hushrun"),
            [
                "CI runs bun test itself in .github/workflows/staging.yml (job test), so it won't skip there: run npx hushrun init to put it through hushrun.",
                "CI runs bun test itself in .github/workflows/production.yml (job test), so it won't skip there: run npx hushrun init to put it through hushrun.",
            ]
        );
        let gitlab = Workflow::parse_gitlab("test:\n  script:\n    - bun test\n".into());
        assert_eq!(
            outside_hushrun(&[&gitlab], &shared, "hushrun"),
            [
                "CI runs bun test itself in .gitlab-ci.yml (job test), so it won't skip there: put hushrun run test -- in front of it."
            ]
        );
        let wrapped = two_pipelines("hushrun run test -- bun test");
        let wrapped: Vec<&Workflow> = wrapped.iter().collect();
        assert!(outside_hushrun(&wrapped, &shared, "hushrun").is_empty());
    }

    // covers: C-15
    #[test]
    fn a_workflow_that_no_push_starts_stays() {
        let dir = tempfile::tempdir().unwrap();
        let text = job("      - run: cargo test\n")
            .replace("on: push", "on:\n  schedule:\n    - cron: '0 0 * * *'");
        let (out, proposal) = edited(dir.path(), &text, false);
        assert_eq!(out, text);
        assert!(proposal.is_empty());
    }

    // covers: C-15
    #[test]
    fn a_job_that_installs_hushrun_gets_no_second_install() {
        let dir = tempfile::tempdir().unwrap();
        let text =
            job("      - uses: hushrun/hushrun/action/setup@v0.3.0\n      - run: cargo test\n");
        let (out, _) = edited(dir.path(), &text, false);
        assert_eq!(out.matches("action/setup").count(), 1, "{out}");
    }

    /// A workflow of these jobs, each `(runs-on, command)`.
    fn jobs_of(jobs: &[(&str, &str)]) -> Workflow {
        let mut text = String::from("on: push\njobs:\n");
        for (index, (runs_on, command)) in jobs.iter().enumerate() {
            text.push_str(&format!(
                "  job{index}:\n    runs-on: {runs_on}\n    steps:\n{CHECKOUT}      - run: {command}\n"
            ));
        }
        Workflow::parse(".github/workflows/ci.yml".into(), text)
    }

    /// Where a rule goes for these jobs, in a repository of one scope with no package.json.
    fn rule_for(jobs: &[(&str, &str)]) -> Vec<RuleLevel> {
        let dir = tempfile::tempdir().unwrap();
        let files = [jobs_of(jobs)];
        Proposal::read(dir.path(), &files, None, true, &ScriptChecks::new()).macos
    }

    /// The same, with a package.json at the root, which keeps JavaScript out of what init wraps.
    fn rule_beside_a_package(jobs: &[(&str, &str)]) -> Vec<RuleLevel> {
        let dir = tempfile::tempdir().unwrap();
        let files = [jobs_of(jobs)];
        Proposal::read(dir.path(), &files, None, false, &ScriptChecks::new()).macos
    }

    const XCODE: (&str, &str) = ("macos-15", "xcodebuild test -scheme App");

    // covers: C-15, C-25
    #[test]
    fn a_rule_needs_every_check_to_fit_and_one_to_earn_it() {
        for jobs in [
            &[XCODE][..],
            &[("macos-latest", "xcodebuild -scheme App test")],
            &[("macOS-14", "swift test")],
            &[("macos-15", "bundle exec swiftlint"), XCODE],
            &[("macos-15", "swiftlint"), ("macos-14", "swift test")],
            // What hushrun doesn't run is no check of its.
            &[XCODE, ("ubuntu-latest", "./deploy.sh")],
            &[XCODE, ("ubuntu-latest", "flutter build apk")],
            &[XCODE, ("ubuntu-latest", "npm audit")],
            // What stays for good earns nothing, and stops nothing.
            &[XCODE, ("macos-15", "bundle exec fastlane scan")],
        ] {
            assert_eq!(rule_for(jobs), [RuleLevel::Top], "{jobs:?}");
        }
        for jobs in [
            // Node, on a Mac.
            &[("macos-latest", "npm test")][..],
            &[XCODE, ("macos-15", "npm test")],
            &[("macos-15", "make test")],
            // What reads the same anywhere earns nothing, and neither does a lane.
            &[("macos-15", "swiftlint --strict")],
            &[
                ("macos-15", "swiftlint --strict"),
                ("macos-14", "swiftlint"),
            ],
            &[("macos-15", "fastlane test"), ("macos-15", "swiftlint")],
            &[("macos-15", "fastlane scan")],
            &[
                ("macos-15", "bundle exec fastlane scan"),
                ("macos-15", "swiftlint"),
            ],
            // Apple's tools, elsewhere too.
            &[("ubuntu-latest", "swift test")],
            &[("macos-15", "swift test"), ("ubuntu-latest", "swift test")],
            &[("macos-15", "swift test"), ("windows-latest", "swift test")],
            &[XCODE, ("ubuntu-latest", "swiftlint")],
            // Another language's tests, on Linux.
            &[XCODE, ("ubuntu-latest", "flutter test")],
            // A system nothing can tell.
            &[("${{ matrix.os }}", "swift test")],
            &[XCODE, ("${{ vars.RUNNER }}", "swift test")],
            &[("[self-hosted, macOS]", "swift test")],
            // Nothing wrapped, so nothing to say.
            &[("ubuntu-latest", "./test.sh")],
        ] {
            assert_eq!(rule_for(jobs), [], "{jobs:?}");
        }
    }

    // covers: C-15, C-25
    #[test]
    fn what_a_package_json_keeps_out_of_ci_is_still_a_check() {
        // With a package.json at the root, init wraps none of these: they are checks all the same.
        for other in [
            "npm test",
            "npm run lint",
            "npx jest",
            "npx vitest run",
            "pnpm test",
            "yarn test",
            "eslint .",
            "jest --ci",
            "node --test",
        ] {
            for runs_on in ["ubuntu-latest", "macos-15"] {
                let jobs = [XCODE, (runs_on, other)];
                assert_eq!(rule_for(&jobs), [], "no package.json: {other} on {runs_on}");
                assert_eq!(rule_beside_a_package(&jobs), [], "{other} on {runs_on}");
            }
        }
        // What isn't a check, or never goes through hushrun, stays out of it, whatever the root has.
        for other in ["npm ci", "npm run build", "npm audit", "./test.sh"] {
            let jobs = [XCODE, ("ubuntu-latest", other)];
            assert_eq!(rule_for(&jobs), [RuleLevel::Top], "{other}");
            assert_eq!(rule_beside_a_package(&jobs), [RuleLevel::Top], "{other}");
        }
        // And nothing else changes: a rule for what earns one is as it was.
        assert_eq!(rule_beside_a_package(&[XCODE]), [RuleLevel::Top]);
    }

    // covers: C-15, C-25
    #[test]
    fn what_a_job_hands_off_says_no() {
        let dir = tempfile::tempdir().unwrap();
        let read = |text: String| {
            let files = [Workflow::parse("ci.yml".into(), text)];
            Proposal::read(dir.path(), &files, None, true, &ScriptChecks::new()).macos
        };
        let test = format!(
            "  test:\n    runs-on: macos-15\n    steps:\n{CHECKOUT}      - run: xcodebuild test\n"
        );
        let action = |runs_on: &str, uses: &str| {
            format!(
                "on: push\njobs:\n{test}  more:\n    runs-on: {runs_on}\n    steps:\n{CHECKOUT}      - uses: {uses}\n"
            )
        };
        assert_eq!(read(format!("on: push\njobs:\n{test}")), [RuleLevel::Top]);
        for called in [
            "./.github/workflows/l.yml",
            "acme/ci/.github/workflows/go.yml@v1",
        ] {
            let text = format!("on: push\njobs:\n{test}  more:\n    uses: {called}\n");
            assert_eq!(read(text), [], "{called}");
        }
        assert_eq!(read(action("ubuntu-latest", "./.github/actions/t")), []);
        assert_eq!(read(action("${{ matrix.os }}", "./.github/actions/t")), []);
        // On a Mac, a local action runs on a Mac, and anyone's action runs no check of this file's.
        assert_eq!(
            read(action("macos-15", "./.github/actions/t")),
            [RuleLevel::Top]
        );
        assert_eq!(
            read(action("ubuntu-latest", "actions/setup-go@v5")),
            [RuleLevel::Top]
        );
    }

    // covers: C-15, C-25
    #[test]
    fn a_check_left_as_it_is_still_says_no() {
        let dir = tempfile::tempdir().unwrap();
        let read = |text: &str| {
            let files = [Workflow::parse("ci.yml".into(), text.to_string())];
            Proposal::read(dir.path(), &files, None, true, &ScriptChecks::new()).macos
        };
        let test = format!(
            "  test:\n    runs-on: macos-15\n    steps:\n{CHECKOUT}      - run: xcodebuild test\n"
        );
        // A matrix's commands are left, and its runs-on is what makes the system unknowable.
        let matrix = format!(
            "on: push\njobs:\n{test}  more:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, macos-latest]\n    runs-on: ${{{{ matrix.os }}}}\n    steps:\n{CHECKOUT}      - run: swift test\n"
        );
        assert_eq!(read(&matrix), []);
        // A matrix over something else leaves the system as it says.
        let xcodes = format!(
            "on: push\njobs:\n{test}  more:\n    strategy:\n      matrix:\n        xcode: ['15.4', '16.2']\n    runs-on: macos-15\n    steps:\n{CHECKOUT}      - run: swift test\n"
        );
        assert_eq!(read(&xcodes), [RuleLevel::Top]);
        // Left for a pipe, on Linux.
        let piped = format!(
            "on: push\njobs:\n{test}  more:\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}      - run: swift test | tee log\n"
        );
        assert_eq!(read(&piped), []);
        // What goes through hushrun already counts as what it starts, and where.
        let wrapped = |runs_on: &str, command: &str| {
            format!(
                "on: push\njobs:\n{test}  more:\n    runs-on: {runs_on}\n    steps:\n{CHECKOUT}      - run: {command}\n"
            )
        };
        assert_eq!(
            read(&wrapped("ubuntu-latest", "hushrun run t -- swift test")),
            []
        );
        assert_eq!(read(&wrapped("macos-15", "hushrun run t -- npm test")), []);
        assert_eq!(
            read(&wrapped(
                "macos-15",
                "hushrun run t-${{ matrix.a }} -- swift test"
            )),
            []
        );
        assert_eq!(
            read(&wrapped("macos-15", "hushrun run t -- swift test")),
            [RuleLevel::Top]
        );
        // What earns a rule has to go through hushrun: left, it leaves nothing to hold to a Mac.
        let lint = format!(
            "on: push\njobs:\n  lint:\n    runs-on: macos-15\n    steps:\n{CHECKOUT}      - run: swiftlint\n      - run: xcodebuild test | tee test.log\n"
        );
        assert_eq!(read(&lint), []);
    }

    // covers: C-15, C-25
    #[test]
    fn gitlab_never_says_a_job_is_on_a_mac() {
        let dir = tempfile::tempdir().unwrap();
        let macos = "test:\n  tags: [saas-macos-medium-m1]\n  script:\n    - swift test\n";
        let read = |files: &[Workflow]| {
            Proposal::read(dir.path(), files, None, false, &ScriptChecks::new()).macos
        };
        let gitlab = Workflow::parse_gitlab(macos.to_string());
        assert_eq!(read(&[gitlab]), []);
        // And a job it can't tell of says no for the GitHub job beside it.
        let github = Workflow::parse(
            ".github/workflows/ci.yml".into(),
            format!(
                "on: push\njobs:\n  test:\n    runs-on: macos-15\n    steps:\n{CHECKOUT}      - run: swift test\n"
            ),
        );
        let gitlab = Workflow::parse_gitlab(macos.to_string());
        assert_eq!(read(&[github, gitlab]), []);
    }

    // covers: C-15, C-25
    #[test]
    fn a_workflow_that_doesnt_parse_says_no() {
        let dir = tempfile::tempdir().unwrap();
        let text = format!(
            "on: push\non: push\njobs:\n  test:\n    runs-on: macos-15\n    steps:\n{CHECKOUT}      - run: swift test\n"
        );
        assert!(variants::parse("ci.yml", text.as_bytes()).is_none());
        let broken = Workflow::parse("ci.yml".into(), text);
        let good = Workflow::parse(
            "test.yml".into(),
            format!(
                "on: push\njobs:\n  test:\n    runs-on: macos-15\n    steps:\n{CHECKOUT}      - run: xcodebuild test\n"
            ),
        );
        let proposal = Proposal::read(
            dir.path(),
            &[broken, good],
            None,
            true,
            &ScriptChecks::new(),
        );
        // `xcodebuild test` is wrapped, and nothing says where `swift test` runs.
        assert_eq!(proposal.names(), ["test"]);
        assert_eq!(proposal.macos, []);
    }

    // covers: C-15, C-25
    #[test]
    fn a_file_that_said_what_it_wants_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let files = [Workflow::parse(
            "ci.yml".into(),
            format!(
                "on: push\njobs:\n  test:\n    runs-on: macos-15\n    steps:\n{CHECKOUT}      - run: xcodebuild test\n"
            ),
        )];
        let read = |extra: &str| {
            let text = format!("version: 1\nrepository: github.com/a/b\n{extra}");
            let policy = Policy::parse("hushrun.yaml", text.as_bytes()).unwrap();
            Proposal::read(
                dir.path(),
                &files,
                Some(&policy),
                true,
                &ScriptChecks::new(),
            )
            .macos
        };
        assert_eq!(read(""), [RuleLevel::Top]);
        for theirs in [
            "counts_from: {os: linux}\n",
            "counts_from: anywhere\n",
            "scopes:\n  a:\n    include: [\"**\"]\n    counts_from: {arch: arm64}\n",
            "scopes:\n  a:\n    include: [\"**\"]\n    counts_from: anywhere\n",
            "checks:\n  other:\n    counts_from: {os: linux}\n",
            "checks:\n  swiftlint:\n    counts_from: anywhere\n",
            // Checks CI's files don't show, and a deploy, which can't hold one.
            "checks:\n  unit: {kind: source}\n",
            "checks:\n  unit: {run: \"true\"}\ndeploy:\n  require: [unit]\n",
            "deploy:\n  branch: main\n",
        ] {
            assert_eq!(read(theirs), [], "{theirs}");
        }
        // ADR 0062: an older file's `stages` are to move to `deploy`, which couldn't hold one.
        assert_eq!(read("stages:\n  staging:\n    branch: main\n"), []);
    }

    /// `lines` as a `run: |` of a job: what init would write, and what it leaves.
    fn wrapped_block(lines: &[&str]) -> (String, Proposal) {
        let dir = tempfile::tempdir().unwrap();
        let body: String = lines.iter().map(|l| format!("          {l}\n")).collect();
        edited(dir.path(), &job(&format!("      - run: |\n{body}")), false)
    }

    // covers: C-15
    #[test]
    fn a_line_that_goes_on_is_one_command() {
        // Read whole, and wrapped where it starts.
        let (out, proposal) = wrapped_block(&[
            "xcodebuild \\",
            "  -workspace App.xcworkspace \\",
            "  -scheme App \\",
            "  test",
        ]);
        assert!(
            out.contains(
                "          hushrun run test -- xcodebuild \\\n            -workspace App.xcworkspace \\\n            -scheme App \\\n            test\n"
            ),
            "{out}"
        );
        assert_eq!(proposal.names(), ["test"]);
        // A command that starts on a line another goes on to is wrapped on that line.
        let (out, _) = wrapped_block(&["cargo fetch && \\", "  cargo test \\", "  --locked"]);
        assert!(
            out.contains(
                "          cargo fetch && \\\n            hushrun run test -- cargo test \\\n            --locked\n"
            ),
            "{out}"
        );
        // What a later line says counts: a coverage flag there wraps the whole command (C-26).
        let (out, proposal) = wrapped_block(&["swift test \\", "  --enable-code-coverage"]);
        assert!(out.contains("hushrun run test -- swift test \\"), "{out}");
        assert!(proposal.left.is_empty(), "{:?}", proposal.left);
        // What is fed to another program isn't a check.
        let (out, proposal) = wrapped_block(&["docker run --rm swift:6 \\", "  swift test"]);
        assert!(!out.contains("hushrun run"), "{out}");
        assert!(proposal.is_empty());
    }

    // covers: C-15
    #[test]
    fn only_a_backslash_at_the_end_of_a_line_goes_on() {
        // An escaped backslash, and a space after one.
        for first in ["echo a\\\\", "echo a\\ "] {
            let (out, _) = wrapped_block(&[first, "cargo test"]);
            assert!(
                out.contains("hushrun run test -- cargo test\n"),
                "{first}: {out}"
            );
        }
        // A blank line ends it, as a comment does, and neither is read as more of a command.
        let (out, _) = wrapped_block(&["echo a \\", "", "cargo test"]);
        assert!(out.contains("hushrun run test -- cargo test\n"), "{out}");
        let (out, _) = wrapped_block(&["# echo a \\", "cargo test"]);
        assert!(out.contains("hushrun run test -- cargo test\n"), "{out}");
        // A word that ends a line and goes on is still one command of its own.
        let (out, _) = wrapped_block(&["cargo test \\", "  --locked", "cargo clippy"]);
        assert!(out.contains("hushrun run test -- cargo test \\\n"), "{out}");
        assert!(
            out.contains("hushrun run clippy -- cargo clippy\n"),
            "{out}"
        );
    }

    // covers: C-15
    #[test]
    fn a_gitlab_block_goes_on_too() {
        let dir = tempfile::tempdir().unwrap();
        let text = "unit:\n  script:\n    - |\n      cargo test \\\n        --locked\n";
        let files = [Workflow::parse_gitlab(text.to_string())];
        let proposal = Proposal::read(dir.path(), &files, None, false, &ScriptChecks::new());
        let edit = files[0].edit_with(&[], &proposal.changes_of(0, &files[0]));
        assert!(
            edit.text
                .unwrap()
                .contains("      hushrun run test -- cargo test \\\n        --locked\n")
        );
    }

    // covers: C-15
    #[test]
    fn a_pipe_that_only_shows_the_output_is_wrapped_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let (out, _) = edited(
            dir.path(),
            &job("      - run: set -o pipefail && xcodebuild test -scheme App | xcpretty\n"),
            false,
        );
        assert!(
            out.contains(
                "      - run: set -o pipefail && hushrun run test -- xcodebuild test -scheme App | xcpretty\n"
            ),
            "{out}"
        );
    }

    // covers: C-15
    #[test]
    fn gitlab_jobs_get_the_same_edits() {
        let dir = tempfile::tempdir().unwrap();
        let text = "stages: [test]\n\nunit:\n  stage: test\n  script:\n    - cargo fetch\n    - cargo test --locked # all of it\n    - |\n      cargo clippy\n";
        let files = [Workflow::parse_gitlab(text.to_string())];
        let proposal = Proposal::read(dir.path(), &files, None, false, &ScriptChecks::new());
        assert_eq!(proposal.names(), ["test", "clippy"]);
        let edit = files[0].edit_with(&[], &proposal.changes_of(0, &files[0]));
        assert_eq!(
            edit.text.unwrap(),
            format!(
                "stages: [test]\n\nunit:\n  stage: test\n  script:\n    - {GITLAB_SETUP}\n    - cargo fetch\n    - hushrun run test -- cargo test --locked # all of it\n    - |\n      hushrun run clippy -- cargo clippy\n"
            )
        );
    }
}
