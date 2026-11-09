//! \[ADR 0050\] A `make`, `just` or `task` target the table doesn't name is a check by its recipe
//! (C-15): when a command of the recipe is one the table knows, and none of it changes anything
//! outside the machine, which a skip wouldn't do. Its prerequisites, and the targets it runs,
//! count too, three levels down. A target that runs a check and does such a thing, or that runs
//! a check beside something that can't be read, stays as it is, and says why; one that runs no
//! check says nothing, as a command the table doesn't know says nothing.
//!
//! This reads a recipe the way a person does, not the way make runs it: a variable holds what its
//! file sets it to, every branch of a conditional counts, and a program it can't name makes the
//! recipe unreadable. So the most a mistake here can cost is a skip: a target it can't tell is
//! left as it is, never wrapped. Nothing here runs: files are only read.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde_json::Value;

use super::checks::{self, Kind, Verdict};
use super::scripts;
use super::variants;
use crate::cmd::wrap::made_name;

/// How deep a target's prerequisites, and the targets its recipe runs, are read: its own recipe
/// is level 0. What is deeper isn't read, and a check there doesn't count.
const DEPTH: usize = 3;
/// At most this many files are read for one target, each at most 1 MiB; more can't be read.
const FILES: usize = 32;
const FILE_BYTES: usize = 1 << 20;
/// At most this many recipes are read for one target; more can't be read.
const RECIPES: usize = 512;
/// What a variable holds when its file doesn't say: an unknown word, or part of one.
const UNKNOWN: char = '\u{1}';

/// Why a target stays when hushrun can't tell what its recipe runs.
pub const CANT_READ: &str = "its recipe can't be read";

/// What a recipe mustn't do for its target to skip: change something outside the machine. A
/// program, `|` between the ways to write it; a word after it, `*` at its end for any word that
/// starts so, or nothing for any use of the program; and what the recipe does then.
const DENIED: &[(&str, &str, &str)] = &[
    ("docker", "push", "its recipe pushes an image (docker push)"),
    (
        "docker",
        "--push",
        "its recipe pushes an image (docker build --push)",
    ),
    ("podman", "push", "its recipe pushes an image (podman push)"),
    (
        "git",
        "push",
        "its recipe pushes to a git remote (git push)",
    ),
    (
        "npm",
        "publish",
        "its recipe publishes a package (npm publish)",
    ),
    (
        "pnpm",
        "publish",
        "its recipe publishes a package (pnpm publish)",
    ),
    (
        "yarn",
        "publish",
        "its recipe publishes a package (yarn publish)",
    ),
    (
        "bun",
        "publish",
        "its recipe publishes a package (bun publish)",
    ),
    (
        "cargo",
        "publish",
        "its recipe publishes a crate (cargo publish)",
    ),
    (
        "twine",
        "upload",
        "its recipe uploads a package (twine upload)",
    ),
    (
        "poetry",
        "publish",
        "its recipe publishes a package (poetry publish)",
    ),
    (
        "uv",
        "publish",
        "its recipe publishes a package (uv publish)",
    ),
    ("gem", "push", "its recipe publishes a gem (gem push)"),
    (
        "rake",
        "release",
        "its recipe publishes a gem (rake release)",
    ),
    (
        "dotnet",
        "push",
        "its recipe publishes a package (dotnet nuget push)",
    ),
    (
        "mvn|mvnw",
        "deploy*",
        "its recipe deploys a build (mvn deploy)",
    ),
    (
        "gradle|gradlew",
        "publish*",
        "its recipe publishes a build (gradle publish)",
    ),
    (
        "gh",
        "release",
        "its recipe makes a GitHub release (gh release)",
    ),
    (
        "kubectl",
        "apply",
        "its recipe changes a cluster (kubectl apply)",
    ),
    (
        "terraform",
        "apply",
        "its recipe changes infrastructure (terraform apply)",
    ),
    (
        "helm",
        "upgrade",
        "its recipe deploys a chart (helm upgrade)",
    ),
    (
        "helm",
        "install",
        "its recipe deploys a chart (helm install)",
    ),
    ("aws", "", "its recipe calls AWS (aws)"),
    ("gcloud", "", "its recipe calls Google Cloud (gcloud)"),
    ("gsutil", "", "its recipe calls Google Cloud (gsutil)"),
    ("az", "", "its recipe calls Azure (az)"),
    (
        "scp",
        "",
        "its recipe copies files to another machine (scp)",
    ),
];
const RSYNC: &str = "its recipe copies files to another machine (rsync)";
const CURL_METHOD: &str =
    "its recipe sends a request that changes something (curl -X POST, PUT or DELETE)";
const CURL_DATA: &str = "its recipe sends data to a server (curl -d)";

/// What a task runner's target is, by its name or its recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipe {
    /// A check: its name, and its kind, a linter's when every check it runs is one.
    Check(String, Kind),
    /// A target init leaves as it is, and why.
    Leave(&'static str),
}

/// What `words`, a command run in `dir`, are, where `read` reads the repository's files by their
/// paths from its root, `/`-separated, as `dir` is: a `make`, `just` or `task` target the table
/// names, or one whose recipe runs a check; `None` when they are none, or a target whose recipe
/// runs no check the table knows, with nothing that can't be read: it could be no check.
pub fn target(words: &[&str], dir: &str, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Option<Recipe> {
    let call = call(words, dir)?;
    let first = call.targets.first()?;
    // The names the table knows are checks without a recipe to read.
    if let Some(kind) = checks::target_kind(first) {
        return Some(Recipe::Check(first.clone(), kind));
    }
    // Several targets are several checks, as are several of just's recipes (below). A name the
    // shell fills in names no target here.
    if (call.runner != Runner::Just && call.targets.len() > 1)
        || first.contains(['$', '`', '*', '?', '[', '{', '~'])
    {
        return None;
    }
    let name = made_name(first)?.to_string();
    let mut walk = Walk {
        read,
        files: BTreeMap::new(),
        reads: 0,
        seen: BTreeSet::new(),
        recipes: 0,
        several: false,
        found: Found::default(),
    };
    walk.run(&call, 0);
    if walk.several {
        return None;
    }
    walk.found.recipe(name)
}

/// A task runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Runner {
    Make,
    Just,
    Task,
}

fn runner(program: &str) -> Option<Runner> {
    match program {
        "make" | "gmake" => Some(Runner::Make),
        "just" => Some(Runner::Just),
        "task" | "go-task" => Some(Runner::Task),
        _ => None,
    }
}

/// A task runner's command line: where it starts, the file it reads, and what it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    runner: Runner,
    /// The directory it reads its file from, from the root; `None` when that is outside the
    /// repository, or can't be told.
    dir: Option<String>,
    /// The file its command line names, from where it starts.
    file: Option<String>,
    /// Its targets, in order, and for `just` each recipe's arguments after it.
    targets: Vec<String>,
}

/// The call `words` make, run in `here`; `None` when they are no task runner's, or one that runs
/// nothing: a dry run, a list, its help or its version.
fn call(words: &[&str], here: &str) -> Option<Call> {
    let (program, args) = words.split_first()?;
    let runner = runner(program)?;
    let mut call = Call {
        runner,
        dir: Some(here.to_string()),
        file: None,
        targets: Vec::new(),
    };
    let mut args = args.iter().copied().peekable();
    let into =
        |dir: &mut Option<String>, to: &str| *dir = dir.as_deref().and_then(|d| joined(d, to));
    while let Some(arg) = args.next() {
        // just reads its options before the first recipe: what follows are its arguments.
        if runner == Runner::Just && !call.targets.is_empty() {
            call.targets.push(arg.to_string());
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            if long.is_empty() {
                // What follows is task's CLI_ARGS, or arguments of just's recipe.
                if runner == Runner::Just {
                    call.targets.extend(args.by_ref().map(str::to_string));
                }
                break;
            }
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let mut value = || inline.or_else(|| args.next());
            match (runner, name) {
                (Runner::Make, "directory") | (Runner::Task, "dir") => {
                    into(&mut call.dir, value()?)
                }
                (Runner::Make, "file" | "makefile")
                | (Runner::Just, "justfile")
                | (Runner::Task, "taskfile") => call.file = Some(value()?.to_string()),
                (Runner::Just, "working-directory") => {
                    value()?;
                }
                (
                    Runner::Make,
                    "include-dir" | "old-file" | "assume-old" | "new-file" | "assume-new"
                    | "what-if" | "eval",
                )
                | (
                    Runner::Just,
                    "set" | "shell" | "shell-arg" | "dotenv-filename" | "dotenv-path" | "color"
                    | "chooser" | "command-color" | "list-heading" | "list-prefix"
                    | "timestamp-format" | "tempdir",
                )
                | (
                    Runner::Task,
                    "output" | "concurrency" | "output-group-begin" | "output-group-end" | "sort"
                    | "interval",
                ) => {
                    value()?;
                    if (runner, name) == (Runner::Just, "set") {
                        args.next();
                    }
                }
                (Runner::Make, "jobs" | "load-average" | "max-load") if inline.is_none() => {
                    args.next_if(|word| word.parse::<f64>().is_ok());
                }
                (
                    Runner::Make,
                    "just-print" | "dry-run" | "recon" | "question" | "touch" | "print-data-base"
                    | "version" | "help",
                )
                | (
                    Runner::Just,
                    "dry-run" | "list" | "summary" | "show" | "evaluate" | "dump" | "fmt"
                    | "choose" | "edit" | "init" | "variables" | "completions" | "groups"
                    | "changelog" | "help" | "version" | "command",
                )
                | (
                    Runner::Task,
                    "dry" | "list" | "list-all" | "summary" | "status" | "init" | "version"
                    | "help" | "watch" | "completion" | "json",
                ) => return None,
                _ => {}
            }
            continue;
        }
        if let Some(flags) = arg.strip_prefix('-').filter(|flags| !flags.is_empty()) {
            for (at, flag) in flags.char_indices() {
                let rest = &flags[at + flag.len_utf8()..];
                let mut value = || match rest {
                    "" => args.next(),
                    rest => Some(rest),
                };
                match (runner, flag) {
                    (Runner::Make, 'C') | (Runner::Task, 'd') => {
                        into(&mut call.dir, value()?);
                        break;
                    }
                    (Runner::Make | Runner::Just, 'f') | (Runner::Task, 't') => {
                        call.file = Some(value()?.to_string());
                        break;
                    }
                    (Runner::Make, 'I' | 'o' | 'W')
                    | (Runner::Just, 'd' | 'E')
                    | (Runner::Task, 'o' | 'C') => {
                        value()?;
                        break;
                    }
                    (Runner::Make, 'j' | 'l') => {
                        if rest.is_empty() {
                            args.next_if(|word| word.parse::<f64>().is_ok());
                        }
                        break;
                    }
                    // Its output's type, which only goes with it.
                    (Runner::Make, 'O') => break,
                    (Runner::Make, 'n' | 'q' | 't' | 'p' | 'v' | 'h')
                    | (Runner::Just, 'n' | 'l' | 's' | 'e' | 'h' | 'V' | 'c')
                    | (Runner::Task, 'n' | 'l' | 'a' | 'i' | 'h' | 'w' | 'j') => return None,
                    _ => {}
                }
            }
            continue;
        }
        // `NAME=value`, before a target or among them, sets a variable.
        if scripts::assignment(arg) {
            continue;
        }
        if runner == Runner::Just {
            // `just dir/recipe` reads the justfile of `dir`.
            if let Some((dir, recipe)) = arg.rsplit_once('/') {
                into(&mut call.dir, dir);
                call.targets.push(recipe.to_string());
                continue;
            }
        }
        call.targets.push(arg.to_string());
    }
    Some(call)
}

/// `to` from `from`, both from the root; `None` when it leaves the repository, or can't be told.
fn joined(from: &str, to: &str) -> Option<String> {
    if to.is_empty() || to.starts_with(['/', '~']) || to.contains(['$', '`', UNKNOWN]) {
        return None;
    }
    let mut parts: Vec<&str> = from.split('/').filter(|part| !part.is_empty()).collect();
    for part in to.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The directory of `path`, a file's path from the root.
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// What one target's recipes say.
#[derive(Debug, Default)]
struct Found {
    /// The first thing it does that a skip wouldn't.
    denied: Option<&'static str>,
    /// The first command the table leaves as it is, as said of a recipe.
    left: Option<&'static str>,
    /// Something of it can't be read.
    unreadable: bool,
    /// The kind of each check it runs.
    kinds: Vec<Kind>,
}

impl Found {
    /// What the target is. A reason to leave it is said only of one that runs a check the table
    /// knows: one that runs none says nothing, as a command the table doesn't know says
    /// nothing, whatever else it does or hides.
    fn recipe(self, name: String) -> Option<Recipe> {
        if self.kinds.is_empty() {
            return None;
        }
        if let Some(why) = self.denied.or(self.left) {
            return Some(Recipe::Leave(why));
        }
        if self.unreadable {
            return Some(Recipe::Leave(CANT_READ));
        }
        let kind = match self.kinds.iter().all(|kind| *kind == Kind::Lint) {
            true => Kind::Lint,
            false => Kind::Test,
        };
        Some(Recipe::Check(name, kind))
    }
}

/// A file a task runner reads.
#[derive(Debug)]
enum File {
    Make(Makefile),
    Just(Justfile),
    Task(Taskfile),
}

/// A read of one target: the files it read, and what it found.
struct Walk<'r> {
    read: &'r dyn Fn(&str) -> Option<Vec<u8>>,
    /// Each file read, by its path: `None` for one that doesn't read.
    files: BTreeMap<String, Option<Rc<File>>>,
    reads: usize,
    /// The targets read, each once: its file's path, and its name.
    seen: BTreeSet<(String, String)>,
    recipes: usize,
    /// just was asked for several recipes on CI's line.
    several: bool,
    found: Found,
}

impl Walk<'_> {
    /// The text of the file at `path`, when there is one to read.
    fn text(&mut self, path: &str) -> Option<String> {
        if self.reads >= FILES {
            self.found.unreadable = true;
            return None;
        }
        let bytes = (self.read)(path)?;
        self.reads += 1;
        if bytes.len() > FILE_BYTES {
            self.found.unreadable = true;
            return None;
        }
        String::from_utf8(bytes).ok()
    }

    /// The first of `names` in `dir` there is, or, `upward`, in each directory above it too, as
    /// just and task look for theirs.
    fn find(&mut self, dir: &str, names: &[&str], upward: bool) -> Option<String> {
        let mut dir = Some(dir.to_string());
        while let Some(here) = dir {
            for name in names {
                let path = joined(&here, name)?;
                if self.files.contains_key(&path) || (self.read)(&path).is_some() {
                    return Some(path);
                }
            }
            if !upward || here.is_empty() {
                break;
            }
            dir = Some(dir_of(&here).to_string());
        }
        None
    }

    /// The file at `path`, read as `runner`'s.
    fn file(&mut self, path: &str, runner: Runner) -> Option<Rc<File>> {
        if let Some(file) = self.files.get(path) {
            return file.clone();
        }
        let file = self.text(path).and_then(|text| match runner {
            Runner::Make => {
                let mut makefile = Makefile::default();
                self.make_lines(&text, dir_of(path), &mut makefile, 0);
                Some(File::Make(makefile))
            }
            Runner::Just => {
                let mut justfile = Justfile::default();
                self.just_lines(&text, dir_of(path), &mut justfile, 0);
                Some(File::Just(justfile))
            }
            Runner::Task => Taskfile::parse(path, &text).map(File::Task),
        });
        let file = file.map(Rc::new);
        self.files.insert(path.to_string(), file.clone());
        file
    }

    /// Reads what `call` runs, at `level`.
    fn run(&mut self, call: &Call, level: usize) {
        let Some(dir) = call.dir.as_deref() else {
            self.found.unreadable = true;
            return;
        };
        let path = match (&call.file, call.runner) {
            (Some(file), Runner::Task) => joined(dir, file).and_then(|path| {
                let names = [
                    "Taskfile.yml",
                    "taskfile.yml",
                    "Taskfile.yaml",
                    "taskfile.yaml",
                ];
                match (self.read)(&path) {
                    Some(_) => Some(path),
                    None => self.find(&path, &names, false),
                }
            }),
            (Some(file), _) => joined(dir, file),
            (None, Runner::Make) => self.find(dir, &["GNUmakefile", "makefile", "Makefile"], false),
            (None, Runner::Just) => self.find(
                dir,
                &["justfile", "Justfile", "JUSTFILE", ".justfile", ".Justfile"],
                true,
            ),
            (None, Runner::Task) => self.find(
                dir,
                &[
                    "Taskfile.yml",
                    "taskfile.yml",
                    "Taskfile.yaml",
                    "taskfile.yaml",
                    "Taskfile.dist.yml",
                    "taskfile.dist.yml",
                    "Taskfile.dist.yaml",
                    "taskfile.dist.yaml",
                ],
                true,
            ),
        };
        let Some((path, file)) =
            path.and_then(|path| Some((path.clone(), self.file(&path, call.runner)?)))
        else {
            self.found.unreadable = true;
            return;
        };
        match &*file {
            File::Make(makefile) => {
                let goals = match call.targets.is_empty() {
                    true => makefile.default_goal.iter().cloned().collect(),
                    false => call.targets.clone(),
                };
                if goals.is_empty() {
                    self.found.unreadable = true;
                }
                for goal in goals {
                    self.make(&path, dir, &goal, level, true);
                }
            }
            File::Just(justfile) => {
                let mut targets = call.targets.iter();
                let mut recipes = Vec::new();
                match targets.next() {
                    None => recipes.extend(justfile.recipes.first().map(|r| r.name.clone())),
                    Some(first) => {
                        let mut next = Some(first);
                        while let Some(name) = next {
                            recipes.push(name.clone());
                            // A recipe takes the words after it as its arguments, as many as it
                            // has parameters; any other word is another recipe.
                            let params = justfile.recipe(name).map_or(0, |r| r.params);
                            let variadic = justfile.recipe(name).is_some_and(|r| r.variadic);
                            if variadic {
                                break;
                            }
                            next = targets.by_ref().nth(params);
                        }
                    }
                }
                if recipes.is_empty() {
                    self.found.unreadable = true;
                }
                if level == 0 && recipes.len() > 1 {
                    self.several = true;
                }
                for recipe in recipes {
                    self.just(&path, &recipe, level, true);
                }
            }
            File::Task(_) => {
                let tasks = match call.targets.is_empty() {
                    true => vec!["default".to_string()],
                    false => call.targets.clone(),
                };
                for task in tasks {
                    self.task(&path, &task, level, true);
                }
            }
        }
    }

    /// Whether the target `name` of `path`, at `level`, is one more to read: read once, no deeper
    /// than [`DEPTH`], and within the budget.
    fn visit(&mut self, path: &str, name: &str, level: usize) -> bool {
        if level > DEPTH || !self.seen.insert((path.to_string(), name.to_string())) {
            return false;
        }
        self.recipes += 1;
        if self.recipes > RECIPES {
            self.found.unreadable = true;
            return false;
        }
        true
    }

    /// The target `name` of the Makefile at `path`, which make runs in `dir`. `required`: make
    /// is asked for it, so it must have a rule; a prerequisite without one is a file.
    fn make(&mut self, path: &str, dir: &str, name: &str, level: usize, required: bool) {
        let Some(file) = self.file(path, Runner::Make) else {
            self.found.unreadable = true;
            return;
        };
        let File::Make(makefile) = &*file else {
            return;
        };
        let Some(rule) = makefile.rule(name, required) else {
            self.found.unreadable |= required;
            return;
        };
        if !self.visit(path, name, level) {
            return;
        }
        let auto = Auto {
            target: name,
            prereqs: &rule.prereqs,
            stem: rule.stem.as_deref(),
        };
        for line in &rule.recipe {
            let text = makefile.expand(prefixed(line), &auto);
            self.shell(prefixed(&text), dir, level);
        }
        for prereq in &rule.prereqs {
            self.make(path, dir, prereq, level + 1, false);
        }
    }

    /// The recipe `name` of the justfile at `path`.
    fn just(&mut self, path: &str, name: &str, level: usize, required: bool) {
        let Some(file) = self.file(path, Runner::Just) else {
            self.found.unreadable = true;
            return;
        };
        let File::Just(justfile) = &*file else {
            return;
        };
        let Some(recipe) = justfile.recipe(name) else {
            self.found.unreadable |= required;
            return;
        };
        if !self.visit(path, &recipe.name, level) {
            return;
        }
        // A script's own interpreter decides, else the file's shell.
        if !recipe.shell || (justfile.not_a_shell && !recipe.script) {
            self.found.unreadable |= !recipe.body.is_empty();
        } else {
            for line in &recipe.body {
                let text = justfile.expand(line);
                self.shell(prefixed(&text), dir_of(path), level);
            }
        }
        for dep in &recipe.deps {
            self.just(path, dep, level + 1, true);
        }
    }

    /// The task `name` of the Taskfile at `path`.
    fn task(&mut self, path: &str, name: &str, level: usize, required: bool) {
        let Some(file) = self.file(path, Runner::Task) else {
            self.found.unreadable = true;
            return;
        };
        let File::Task(taskfile) = &*file else {
            return;
        };
        let Some((key, task)) = taskfile.task(name) else {
            self.found.unreadable |= required;
            return;
        };
        let (cmds, deps) = Taskfile::parts(task);
        if !self.visit(path, key, level) {
            return;
        }
        let vars = taskfile.vars_of(task);
        let dir = match task.get("dir") {
            None => Some(dir_of(path).to_string()),
            Some(Value::String(dir)) => joined(dir_of(path), dir),
            Some(_) => None,
        };
        let Some(dir) = dir else {
            self.found.unreadable = true;
            return;
        };
        for cmd in cmds {
            match cmd {
                Value::String(text) => self.task_shell(text, &vars, &dir, level),
                Value::Object(cmd) => {
                    let defer = cmd.get("defer");
                    if let Some(Value::String(called)) = cmd
                        .get("task")
                        .or_else(|| defer.and_then(|defer| defer.get("task")))
                    {
                        self.task(path, called, level + 1, true);
                    } else if let Some(Value::String(text)) =
                        cmd.get("cmd").or(defer.filter(|defer| defer.is_string()))
                    {
                        self.task_shell(text, &vars, &dir, level);
                    } else {
                        self.found.unreadable = true;
                    }
                }
                _ => self.found.unreadable = true,
            }
        }
        for dep in deps {
            match dep
                .as_str()
                .or_else(|| dep.get("task").and_then(Value::as_str))
            {
                Some(dep) => self.task(path, dep, level + 1, true),
                None => self.found.unreadable = true,
            }
        }
    }

    fn task_shell(&mut self, text: &str, vars: &BTreeMap<String, String>, dir: &str, level: usize) {
        for line in text.lines() {
            self.shell(&Taskfile::expand(line, vars), dir, level);
        }
    }

    /// The commands of `text`, a shell line run in `dir` at `level`.
    fn shell(&mut self, text: &str, dir: &str, level: usize) {
        let mut here = Some(dir.to_string());
        for words in split_commands(text) {
            let words = past_prefixes(&words);
            match words {
                [] => continue,
                [cd, to, ..] if matches!(cd.as_str(), "cd" | "pushd") => {
                    here = here.and_then(|here| joined(&here, to));
                    continue;
                }
                [cd] if matches!(cd.as_str(), "cd" | "pushd") => {
                    here = None;
                    continue;
                }
                _ => self.command(words, here.as_deref(), level),
            }
        }
    }

    /// One command of a recipe, run in `here`, when that can be told.
    fn command(&mut self, words: &[String], here: Option<&str>, level: usize) {
        let unknown = |word: &str| word.chars().all(|c| c == UNKNOWN);
        // A program a variable holds, which its file doesn't say: what follows may still say
        // what it does, as with `$(SUDO) docker push`.
        if unknown(&words[0]) {
            let rest: Vec<&str> = words
                .iter()
                .map(String::as_str)
                .skip_while(|word| unknown(word))
                .collect();
            let rest: Vec<String> = rest.iter().map(|word| known(word)).collect();
            let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
            match denied(&rest) {
                Some(why) => self.deny(why),
                None => self.found.unreadable = true,
            }
            return;
        }
        // A tool by its path, such as `$(GOBIN)/golangci-lint` or `./node_modules/.bin/eslint`,
        // is that tool.
        let program = words[0].rsplit('/').next().unwrap_or(&words[0]);
        if program.is_empty() || program.contains(['$', '`', UNKNOWN]) {
            self.found.unreadable = true;
            return;
        }
        // Another target, of this file or another: where it is and what it is called must be
        // known, but for the variables it sets.
        if runner(program).is_some() {
            let unsure = words[1..]
                .iter()
                .any(|word| !scripts::assignment(word) && word.contains(['$', '`', UNKNOWN]));
            let mut words: Vec<&str> = words.iter().map(String::as_str).collect();
            words[0] = program;
            match (here, unsure) {
                (Some(here), false) => {
                    if let Some(call) = call(&words, here) {
                        self.run(&call, level + 1);
                    }
                }
                _ => self.found.unreadable = true,
            }
            return;
        }
        let words: Vec<String> = std::iter::once(program.to_string())
            .chain(
                words[1..]
                    .iter()
                    .filter(|word| !unknown(word.as_str()))
                    .map(|word| known(word)),
            )
            .collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        if let Some(why) = denied(&words) {
            self.deny(why);
            return;
        }
        match checks::recognize(&words, true) {
            Some(Verdict::Check(_)) => self.found.kinds.push(checks::kind_of(&words)),
            Some(Verdict::Leave(why)) => {
                self.found.left.get_or_insert(checks::in_a_recipe(why));
            }
            None => {}
        }
    }

    fn deny(&mut self, why: &'static str) {
        self.found.denied.get_or_insert(why);
    }
}

/// `word` with what isn't known of it taken out: a variable its file doesn't set makes nothing.
fn known(word: &str) -> String {
    word.replace(UNKNOWN, "")
}

/// A recipe's line without what says how make or just runs it: `@` (quietly), `-` (whatever it
/// exits with) and `+` (even in a dry run).
fn prefixed(line: &str) -> &str {
    line.trim_start_matches(|c: char| matches!(c, '@' | '-' | '+') || c.is_whitespace())
}

/// What `words` do outside the machine, when they do something a skip wouldn't.
fn denied(words: &[&str]) -> Option<&'static str> {
    let words = checks::without_launchers(words);
    let (program, rest) = words.split_first()?;
    for &(programs, word, why) in DENIED {
        let named = programs.split('|').any(|name| name == *program);
        let does = word.is_empty()
            || rest.iter().any(|w| match word.strip_suffix('*') {
                Some(start) => w.starts_with(start),
                None => *w == word,
            });
        if named && does {
            return Some(why);
        }
    }
    match *program {
        "rsync"
            if rest
                .iter()
                .any(|word| !word.starts_with('-') && remote(word)) =>
        {
            Some(RSYNC)
        }
        "curl" => curl(rest),
        _ => None,
    }
}

/// Whether `word`, an argument of rsync, is on another machine: `host:path`, `host::module` or
/// `rsync://`. A Windows drive, `C:`, isn't.
fn remote(word: &str) -> bool {
    word.starts_with("rsync://")
        || word
            .split_once(':')
            .is_some_and(|(host, _)| host.len() > 1 && !host.contains('/'))
}

/// What curl's `words` send: a request that changes something, or data.
fn curl(words: &[&str]) -> Option<&'static str> {
    for (at, word) in words.iter().enumerate() {
        let method = match *word {
            "-X" | "--request" => words.get(at + 1).copied(),
            word => word
                .strip_prefix("--request=")
                .or_else(|| word.strip_prefix("-X").filter(|method| !method.is_empty())),
        };
        let changes = ["POST", "PUT", "PATCH", "DELETE"];
        if method.is_some_and(|method| changes.iter().any(|c| method.eq_ignore_ascii_case(c))) {
            return Some(CURL_METHOD);
        }
        let sends = ["--data", "--json", "--form", "--upload-file"]
            .iter()
            .any(|long| word.starts_with(*long))
            || (["-d", "-F", "-T"]
                .iter()
                .any(|short| word.starts_with(*short))
                && !word.starts_with("--"));
        if sends {
            return Some(CURL_DATA);
        }
    }
    None
}

/// Shell words that start another command, as the shell reads them: `if cmd`, `time cmd`.
const KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "do", "while", "until", "!", "{", "time", "exec", "command",
    "nohup", "builtin", "noglob",
];
/// What starts a construct, whose words are no program's.
const CONSTRUCTS: &[&str] = &[
    "for", "case", "select", "function", "fi", "done", "esac", "}", ";;",
];
/// Programs that run the command after their options, and their options that take a value.
const WRAPPERS: &[(&str, &[&str])] = &[
    (
        "env",
        &[
            "-u",
            "--unset",
            "-C",
            "--chdir",
            "-S",
            "--split-string",
            "-P",
        ],
    ),
    (
        "sudo",
        &[
            "-u",
            "--user",
            "-g",
            "--group",
            "-C",
            "-D",
            "--chdir",
            "-h",
            "--host",
            "-p",
            "--prompt",
            "-r",
            "--role",
            "-t",
            "--type",
            "-U",
            "--other-user",
            "-T",
            "--command-timeout",
        ],
    ),
    ("nice", &["-n", "--adjustment"]),
    (
        "ionice",
        &["-c", "--class", "-n", "--classdata", "-p", "--pid"],
    ),
    ("stdbuf", &["-i", "-o", "-e"]),
    (
        "xvfb-run",
        &[
            "-e",
            "--error-file",
            "-f",
            "--auth-file",
            "-n",
            "--server-num",
            "-p",
            "--xauth-protocol",
            "-s",
            "--server-args",
            "-w",
            "--wait",
        ],
    ),
    ("timeout", &["-s", "--signal", "-k", "--kill-after"]),
    ("caffeinate", &["-t", "-w"]),
];

/// `words` from the program they run: past the variables they set, the shell's keywords, and the
/// programs that run another, such as `sudo` and `env`; none for a construct, such as `for`.
fn past_prefixes(words: &[String]) -> &[String] {
    let mut at = 0;
    loop {
        while words.get(at).is_some_and(|word| scripts::assignment(word)) {
            at += 1;
        }
        // A wrapper's last option, or its duration, can be missing.
        let Some(word) = words.get(at).map(String::as_str) else {
            return &[];
        };
        if CONSTRUCTS.contains(&word) {
            return &[];
        }
        if KEYWORDS.contains(&word) {
            at += 1;
            continue;
        }
        let Some((wrapper, takes_a_value)) = WRAPPERS.iter().find(|(name, _)| *name == word) else {
            return &words[at..];
        };
        at += 1;
        while let Some(option) = words.get(at).filter(|word| word.starts_with('-')) {
            at += 1;
            if takes_a_value.contains(&option.as_str()) {
                at += 1;
            }
        }
        if *wrapper == "timeout" {
            // Its duration.
            at += 1;
        }
    }
}

/// The commands of a shell line, each its words, unquoted. A list, a pipe, a subshell and a
/// substitution each start another, and a redirection's file is no word of its command. It reads
/// what a shell might run, and never less.
fn split_commands(text: &str) -> Vec<Vec<String>> {
    let mut commands: Vec<Vec<String>> = vec![Vec::new()];
    let mut word: Option<String> = None;
    // Substitutions inside double quotes, read as commands of their own.
    let mut inner: Vec<String> = Vec::new();
    // The next word is a redirection's file.
    let mut redirected = false;
    let mut chars = text.chars().peekable();
    let end =
        |word: &mut Option<String>, commands: &mut Vec<Vec<String>>, redirected: &mut bool| {
            if let Some(word) = word.take()
                && !std::mem::take(redirected)
            {
                commands.last_mut().expect("a command").push(word);
            }
        };
    let next = |commands: &mut Vec<Vec<String>>| {
        if !commands.last().expect("a command").is_empty() {
            commands.push(Vec::new());
        }
    };
    let substitution = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>, close: char| {
        let (mut text, mut depth) = (String::new(), 1);
        for c in chars.by_ref() {
            match c {
                '(' if close == ')' => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            text.push(c);
        }
        text
    };
    while let Some(c) = chars.next() {
        match c {
            '\n' | ';' | '&' | '|' | '(' | ')' | '`' => {
                end(&mut word, &mut commands, &mut redirected);
                redirected = false;
                next(&mut commands);
            }
            '$' if chars.peek() == Some(&'(') => {
                chars.next();
                end(&mut word, &mut commands, &mut redirected);
                redirected = false;
                next(&mut commands);
            }
            '$' if chars.peek() == Some(&'{') => {
                let w = word.get_or_insert_with(String::new);
                w.push('$');
                for c in chars.by_ref() {
                    w.push(c);
                    if c == '}' {
                        break;
                    }
                }
            }
            '<' | '>' => {
                // A file descriptor's number, as in `2>`, is the redirection's.
                if word
                    .as_deref()
                    .is_some_and(|w| w.chars().all(|c| c.is_ascii_digit()))
                {
                    word = None;
                }
                end(&mut word, &mut commands, &mut redirected);
                while chars.next_if(|&c| matches!(c, '<' | '>' | '|')).is_some() {}
                if chars.next_if_eq(&'&').is_some() {
                    while chars.next_if(char::is_ascii_digit).is_some() {}
                    chars.next_if_eq(&'-');
                } else {
                    redirected = true;
                }
            }
            c if c.is_whitespace() => end(&mut word, &mut commands, &mut redirected),
            '#' if word.is_none() => while chars.next_if(|c| *c != '\n').is_some() {},
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    w.push(c);
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => w.extend(chars.next()),
                        '`' => {
                            inner.push(substitution(&mut chars, '`'));
                            w.push(UNKNOWN);
                        }
                        '$' if chars.peek() == Some(&'(') => {
                            chars.next();
                            inner.push(substitution(&mut chars, ')'));
                            w.push(UNKNOWN);
                        }
                        c => w.push(c),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') | None => end(&mut word, &mut commands, &mut redirected),
                Some(c) => word.get_or_insert_with(String::new).push(c),
            },
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    end(&mut word, &mut commands, &mut redirected);
    for text in inner {
        commands.extend(split_commands(&text));
    }
    commands.retain(|command| !command.is_empty());
    commands
}

// --- make ---------------------------------------------------------------------------------------

/// A Makefile, with what it includes, as far as it can be read without running make.
#[derive(Debug, Default)]
struct Makefile {
    vars: BTreeMap<String, Var>,
    rules: Vec<Rule>,
    default_goal: Option<String>,
}

/// A variable: its value, expanded when it was set (`:=`) or each time it is used (`=`).
#[derive(Debug, Clone)]
struct Var {
    value: String,
    simple: bool,
}

#[derive(Debug, Clone)]
struct Rule {
    targets: Vec<String>,
    prereqs: Vec<String>,
    recipe: Vec<String>,
    double: bool,
}

/// What make knows of a target: its prerequisites, its recipe, and the stem of the pattern it
/// matched.
#[derive(Debug)]
struct Lookup {
    prereqs: Vec<String>,
    recipe: Vec<String>,
    stem: Option<String>,
}

/// make's automatic variables in a recipe.
struct Auto<'a> {
    target: &'a str,
    prereqs: &'a [String],
    stem: Option<&'a str>,
}

impl Makefile {
    /// What its rules say of `name`: those that name it, or else the first pattern it matches.
    /// `anything`: a pattern of `%` alone matches it too, as it does a target make is asked for,
    /// though not a prerequisite, which is a file.
    fn rule(&self, name: &str, anything: bool) -> Option<Lookup> {
        let named: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|rule| rule.targets.iter().any(|target| target == name))
            .collect();
        if !named.is_empty() {
            let mut prereqs = Vec::new();
            for rule in &named {
                for prereq in &rule.prereqs {
                    if !prereqs.contains(prereq) {
                        prereqs.push(prereq.clone());
                    }
                }
            }
            // A target of `::` rules runs each recipe; of `:` rules, the last.
            let recipe = match named.iter().any(|rule| rule.double) {
                true => named.iter().flat_map(|rule| rule.recipe.clone()).collect(),
                false => named
                    .iter()
                    .rev()
                    .find(|rule| !rule.recipe.is_empty())
                    .map(|rule| rule.recipe.clone())
                    .unwrap_or_default(),
            };
            return Some(Lookup {
                prereqs,
                recipe,
                stem: None,
            });
        }
        self.rules.iter().find_map(|rule| {
            let stem = rule.targets.iter().find_map(|target| {
                if target == "%" && !anything {
                    return None;
                }
                let (start, end) = target.split_once('%')?;
                name.strip_prefix(start)?
                    .strip_suffix(end)
                    .filter(|stem| !stem.is_empty())
            })?;
            Some(Lookup {
                prereqs: rule
                    .prereqs
                    .iter()
                    .map(|prereq| prereq.replacen('%', stem, 1))
                    .collect(),
                recipe: rule.recipe.clone(),
                stem: Some(stem.to_string()),
            })
        })
    }

    /// `text` with its variables filled in, as far as this file says.
    fn expand(&self, text: &str, auto: &Auto<'_>) -> String {
        expand(text, &self.vars, Some(auto), 0)
    }
}

/// `text` with make's variables filled in from `vars`, and in a recipe, `auto`; what isn't known
/// is [`UNKNOWN`].
fn expand(
    text: &str,
    vars: &BTreeMap<String, Var>,
    auto: Option<&Auto<'_>>,
    depth: usize,
) -> String {
    if depth > 16 {
        return UNKNOWN.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek().map(|(_, c)| *c) {
            Some('$') => {
                chars.next();
                out.push('$');
            }
            Some(open @ ('(' | '{')) => {
                chars.next();
                let close = if open == '(' { ')' } else { '}' };
                let mut nested = 1;
                let mut end = None;
                for (i, c) in chars.by_ref() {
                    if c == open {
                        nested += 1;
                    } else if c == close {
                        nested -= 1;
                        if nested == 0 {
                            end = Some(i);
                            break;
                        }
                    }
                }
                let Some(end) = end else {
                    out.push(UNKNOWN);
                    break;
                };
                out.push_str(&reference(&text[at + 2..end], vars, auto, depth));
            }
            Some(name) => {
                chars.next();
                out.push_str(&reference(&name.to_string(), vars, auto, depth));
            }
            None => out.push('$'),
        }
    }
    out
}

/// What `$(inner)` expands to.
fn reference(
    inner: &str,
    vars: &BTreeMap<String, Var>,
    auto: Option<&Auto<'_>>,
    depth: usize,
) -> String {
    // A function, its name then a space: those that print or set something make nothing here,
    // and `call` is a variable of the file with its arguments in `$(1)`, `$(2)`…
    if let Some((function, args)) = inner.split_once([' ', '\t']) {
        return match function {
            "info" | "warning" | "error" | "eval" => String::new(),
            "call" => {
                let args = arguments(args);
                let name = expand(args[0].trim(), vars, auto, depth + 1);
                let Some(var) = vars.get(&name) else {
                    return UNKNOWN.to_string();
                };
                let mut scope = vars.clone();
                for n in 1..10 {
                    let value = args
                        .get(n)
                        .map_or_else(String::new, |arg| expand(arg, vars, auto, depth + 1));
                    scope.insert(
                        n.to_string(),
                        Var {
                            value,
                            simple: true,
                        },
                    );
                }
                expand(&var.value, &scope, auto, depth + 1)
            }
            _ => UNKNOWN.to_string(),
        };
    }
    // A substitution, `$(VAR:a=b)`.
    if inner.contains([':', '=']) {
        return UNKNOWN.to_string();
    }
    let name = expand(inner, vars, auto, depth + 1);
    if name.contains(UNKNOWN) {
        return UNKNOWN.to_string();
    }
    match name.as_str() {
        "MAKE" => return "make".to_string(),
        "CURDIR" => return ".".to_string(),
        "MAKEFLAGS" | "MFLAGS" => return String::new(),
        _ => {}
    }
    if let Some(auto) = auto {
        match name.as_str() {
            "@" => return auto.target.to_string(),
            "<" => return auto.prereqs.first().cloned().unwrap_or_default(),
            "^" | "+" | "?" => return auto.prereqs.join(" "),
            "*" => return auto.stem.unwrap_or_default().to_string(),
            "|" => return String::new(),
            _ => {}
        }
    }
    match vars.get(&name) {
        Some(Var {
            value,
            simple: true,
        }) => value.clone(),
        Some(Var {
            value,
            simple: false,
        }) => expand(value, vars, auto, depth + 1),
        // What make sets itself, unless the file does.
        None => match name.as_str() {
            "RM" => "rm -f",
            "CC" => "cc",
            "CXX" => "g++",
            "CPP" => "cc -E",
            "AR" => "ar",
            "AS" => "as",
            "LD" => "ld",
            _ => return UNKNOWN.to_string(),
        }
        .to_string(),
    }
}

/// A function's arguments: `text` split at its commas outside a `$(…)`.
fn arguments(text: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut rest = text;
    while let Some(comma) = top_level(rest, ',') {
        args.push(&rest[..comma]);
        rest = &rest[comma + 1..];
    }
    args.push(rest);
    args
}

/// Where `c` first stands in `text` outside a `$(…)` or `${…}`.
fn top_level(text: &str, c: char) -> Option<usize> {
    let mut nested = 0usize;
    let mut previous = '\0';
    for (at, here) in text.char_indices() {
        match here {
            '(' | '{' if previous == '$' || nested > 0 => nested += 1,
            ')' | '}' if nested > 0 => nested -= 1,
            here if here == c && nested == 0 => return Some(at),
            _ => {}
        }
        previous = here;
    }
    None
}

/// Whether `line`, trimmed, is one of make's conditionals, whose every branch counts.
fn conditional(line: &str) -> bool {
    let word = line.split([' ', '\t', '(']).next().unwrap_or_default();
    matches!(
        word,
        "ifeq" | "ifneq" | "ifdef" | "ifndef" | "else" | "endif"
    )
}

/// `line` without its comment, and with `\#` made `#`.
fn uncommented(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'#') => {
                chars.next();
                out.push('#');
            }
            '#' => break,
            c => out.push(c),
        }
    }
    out
}

impl Walk<'_> {
    /// Reads `text`, a Makefile or one it includes, into `file`; make runs in `dir`.
    fn make_lines(&mut self, text: &str, dir: &str, file: &mut Makefile, depth: usize) {
        let mut lines = text.lines();
        // The rule whose recipe the next lines that start with a tab are.
        let mut current: Option<usize> = None;
        while let Some(first) = lines.next() {
            let in_recipe = first.starts_with('\t') && current.is_some();
            let mut line = first.to_string();
            // A line that ends in an odd number of `\` goes on on the next.
            while (line.len() - line.trim_end_matches('\\').len()) % 2 == 1 {
                line.pop();
                let Some(next) = lines.next() else {
                    break;
                };
                if in_recipe {
                    line.push(' ');
                    line.push_str(next.strip_prefix('\t').unwrap_or(next));
                } else {
                    line.truncate(line.trim_end().len());
                    line.push(' ');
                    line.push_str(next.trim_start());
                }
            }
            if in_recipe {
                if let Some(rule) = current {
                    file.rules[rule].recipe.push(line[1..].to_string());
                }
                continue;
            }
            let trimmed = line.trim();
            // Blank lines, comments and conditionals don't end a recipe.
            if trimmed.is_empty() || trimmed.starts_with('#') || conditional(trimmed) {
                continue;
            }
            current = None;
            let line = uncommented(&line);
            let mut statement = line.trim();
            if statement.is_empty() {
                continue;
            }
            // What `define` holds is lines of its own, which can't be known as a value.
            if let Some(rest) = statement.strip_prefix("define")
                && (rest.is_empty() || rest.starts_with([' ', '\t']))
            {
                let name = rest
                    .trim()
                    .trim_end_matches(['=', ':', '?', '+', '!'])
                    .trim();
                let name = expand(name, &file.vars, None, 0);
                file.vars.insert(
                    name,
                    Var {
                        value: UNKNOWN.to_string(),
                        simple: true,
                    },
                );
                for line in lines.by_ref() {
                    if line.trim() == "endef" {
                        break;
                    }
                }
                continue;
            }
            let include = ["include ", "-include ", "sinclude "]
                .iter()
                .find_map(|directive| statement.strip_prefix(directive));
            if let Some(names) = include {
                if depth < 4 {
                    for name in expand(names, &file.vars, None, 0).split_whitespace() {
                        if name.contains(['*', '?', '[', UNKNOWN]) {
                            continue;
                        }
                        let Some(path) = joined(dir, name) else {
                            continue;
                        };
                        if let Some(text) = self.text(&path) {
                            self.make_lines(&text, dir, file, depth + 1);
                        }
                    }
                }
                continue;
            }
            while let Some(rest) = ["export ", "override ", "private ", "unexport "]
                .iter()
                .find_map(|word| statement.strip_prefix(word))
            {
                statement = rest.trim_start();
            }
            if let Some((name, op, value)) = assignment(statement) {
                let name = expand(name, &file.vars, None, 0);
                if name.contains(UNKNOWN) {
                    continue;
                }
                let had = file.vars.get(&name).cloned();
                let var = match (op, had) {
                    ("?=", Some(had)) => had,
                    ("+=", Some(Var { value: had, simple })) => Var {
                        value: match simple {
                            true => format!("{had} {}", expand(value, &file.vars, None, 0)),
                            false => format!("{had} {value}"),
                        },
                        simple,
                    },
                    ("!=", _) => Var {
                        value: UNKNOWN.to_string(),
                        simple: true,
                    },
                    (":=" | "::=" | ":::=", _) => Var {
                        value: expand(value, &file.vars, None, 0),
                        simple: true,
                    },
                    _ => Var {
                        value: value.to_string(),
                        simple: false,
                    },
                };
                if name == ".DEFAULT_GOAL" {
                    file.default_goal =
                        Some(expand(&var.value, &file.vars, None, 0).trim().to_string());
                }
                file.vars.insert(name, var);
                continue;
            }
            if let Some((rule, recipe)) = rule(statement, &file.vars) {
                if file.default_goal.is_none() {
                    file.default_goal = rule
                        .targets
                        .iter()
                        .find(|target| !target.starts_with('.') && !target.contains('%'))
                        .cloned();
                }
                file.rules.push(rule);
                let at = file.rules.len() - 1;
                file.rules[at].recipe.extend(recipe);
                current = Some(at);
            }
        }
    }
}

/// `statement` as a variable's assignment: its name, its operator and its value.
fn assignment(statement: &str) -> Option<(&str, &'static str, &str)> {
    let equals = top_level(statement, '=')?;
    let head = &statement[..equals];
    let value = statement[equals + 1..].trim_start();
    let (name, op) = [
        (":::", ":::="),
        ("::", "::="),
        (":", ":="),
        ("?", "?="),
        ("+", "+="),
        ("!", "!="),
    ]
    .iter()
    .find_map(|(suffix, op)| Some((head.strip_suffix(suffix)?, *op)))
    .unwrap_or((head, "="));
    let name = name.trim();
    if name.is_empty() || name.contains(char::is_whitespace) || top_level(name, ':').is_some() {
        return None;
    }
    Some((name, op, value))
}

/// `statement` as a rule: its targets and prerequisites, with what its line runs after a `;`.
/// A line that sets a variable for a target, `target: VAR = value`, is none.
fn rule(statement: &str, vars: &BTreeMap<String, Var>) -> Option<(Rule, Option<String>)> {
    let colon = top_level(statement, ':')?;
    let targets = statement[..colon].trim_end_matches('&');
    let mut rest = &statement[colon + 1..];
    let double = rest.starts_with(':');
    if double {
        rest = &rest[1..];
    }
    let (prereqs, recipe) = match rest.split_once(';') {
        Some((prereqs, recipe)) => (prereqs, Some(recipe.trim_start().to_string())),
        None => (rest, None),
    };
    if top_level(prereqs, '=').is_some() {
        return None;
    }
    // A static pattern's prerequisites, `targets: %.o: %.c`, are files.
    let prereqs = match top_level(prereqs, ':') {
        Some(_) => "",
        None => prereqs,
    };
    let words = |text: &str| -> Vec<String> {
        expand(text, vars, None, 0)
            .split_whitespace()
            .filter(|word| *word != "|" && !word.contains(UNKNOWN))
            .map(str::to_string)
            .collect()
    };
    let targets = words(targets);
    if targets.is_empty() {
        return None;
    }
    Some((
        Rule {
            targets,
            prereqs: words(prereqs),
            recipe: Vec::new(),
            double,
        },
        recipe,
    ))
}

// --- just ---------------------------------------------------------------------------------------

/// A justfile, with what it imports.
#[derive(Debug, Default)]
struct Justfile {
    /// Its variables whose values are written out.
    vars: BTreeMap<String, String>,
    aliases: BTreeMap<String, String>,
    recipes: Vec<JustRecipe>,
    /// `set shell` names a program that isn't a shell, so no line is a shell's.
    not_a_shell: bool,
}

#[derive(Debug, Clone)]
struct JustRecipe {
    name: String,
    /// How many arguments it takes, all of those after it when `variadic`.
    params: usize,
    variadic: bool,
    deps: Vec<String>,
    body: Vec<String>,
    /// Its lines are a shell's: not a script in another language.
    shell: bool,
    /// It is a script, with a shebang or `[script]`: its interpreter, not the file's shell, runs
    /// it.
    script: bool,
}

impl Justfile {
    fn recipe(&self, name: &str) -> Option<&JustRecipe> {
        let name = self.aliases.get(name).map_or(name, String::as_str);
        self.recipes.iter().rev().find(|recipe| recipe.name == name)
    }

    /// `line` with its `{{…}}` filled in, as far as the file says.
    fn expand(&self, line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            if rest[start..].starts_with("{{{{") {
                out.push_str("{{");
                rest = &rest[start + 4..];
                continue;
            }
            let Some(end) = rest[start..].find("}}") else {
                out.push(UNKNOWN);
                return out;
            };
            let inner = rest[start + 2..start + end].trim();
            match self.vars.get(inner) {
                Some(value) => out.push_str(value),
                None if inner == "just_executable()" => out.push_str("just"),
                None => out.push(UNKNOWN),
            }
            rest = &rest[start + end + 2..];
        }
        out.push_str(rest);
        out
    }
}

/// Whether a script's interpreter, the program of its shebang or of `[script(…)]`, is a shell's.
fn a_shell(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let mut program = words.next().unwrap_or_default();
    if program.rsplit('/').next() == Some("env") {
        program = words
            .find(|word| !word.starts_with('-'))
            .unwrap_or_default();
    }
    matches!(
        program.rsplit('/').next().unwrap_or(program),
        "sh" | "bash" | "zsh" | "dash" | "ksh"
    )
}

/// A recipe's first line, `@name params: deps`: its name, its parameters and its dependencies.
fn header(line: &str) -> Option<(String, usize, bool, Vec<String>)> {
    let line = line.strip_prefix('@').unwrap_or(line);
    let name_end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(line.len());
    let name = &line[..name_end];
    if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }
    // The first `:` outside quotes that isn't `:=`.
    let mut quote = None;
    let mut colon = None;
    for (at, c) in line.char_indices().skip(name_end) {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, ':') if !line[at + 1..].starts_with('=') => {
                colon = Some(at);
                break;
            }
            (None, ':') => return None,
            _ => {}
        }
    }
    let colon = colon?;
    let params: Vec<&str> = line[name_end..colon].split_whitespace().collect();
    if params.iter().any(|param| param.starts_with('=')) {
        return None;
    }
    let variadic = params.iter().any(|param| param.starts_with(['+', '*']));
    let mut deps = Vec::new();
    let mut words = line[colon + 1..].split_whitespace();
    while let Some(word) = words.next() {
        match word.strip_prefix('(') {
            Some(dep) => {
                deps.push(dep.trim_end_matches(')').to_string());
                if !word.ends_with(')') {
                    words.by_ref().find(|word| word.ends_with(')'));
                }
            }
            None if word == "&&" => {}
            None => deps.push(word.to_string()),
        }
    }
    Some((name.to_string(), params.len(), variadic, deps))
}

impl Walk<'_> {
    /// Reads `text`, a justfile or one it imports, in `dir`, into `file`.
    fn just_lines(&mut self, text: &str, dir: &str, file: &mut Justfile, depth: usize) {
        let lines: Vec<&str> = text.lines().collect();
        let mut at = 0;
        // What an attribute says of the next recipe's lines: a script, and whether a shell's.
        let mut script: Option<bool> = None;
        while at < lines.len() {
            let line = lines[at].trim_end();
            at += 1;
            if line.trim().is_empty() || line.starts_with(['#', ' ', '\t']) {
                continue;
            }
            if let Some(attributes) = line.strip_prefix('[') {
                if let Some(start) = attributes.find("script") {
                    let argument = attributes[start + 6..]
                        .strip_prefix('(')
                        .and_then(|rest| rest.split(')').next())
                        .map(|argument| argument.trim_matches(|c| matches!(c, '\'' | '"' | ' ')));
                    script = Some(argument.is_none_or(a_shell));
                }
                continue;
            }
            let first = line.split_whitespace().next().unwrap_or_default();
            if first == "set" {
                // `set shell := ["python3", "-c", …]` runs every line with that program.
                let setting = line["set".len()..].trim_start();
                if let Some(value) = setting.strip_prefix("shell").map(str::trim_start)
                    && let Some(list) = value.strip_prefix(":=")
                {
                    let program = list
                        .trim()
                        .trim_start_matches('[')
                        .split(',')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .trim_matches(|c| matches!(c, '\'' | '"'));
                    file.not_a_shell = !a_shell(program);
                }
                continue;
            }
            if matches!(first, "mod" | "mod?") {
                continue;
            }
            if matches!(first, "import" | "import?") {
                let path = line[first.len()..]
                    .trim()
                    .trim_matches(|c| matches!(c, '\'' | '"'));
                if depth < 4
                    && let Some(path) = joined(dir, path)
                    && let Some(text) = self.text(&path)
                {
                    self.just_lines(&text, dir_of(&path), file, depth + 1);
                }
                continue;
            }
            if first == "alias" {
                if let Some((name, target)) = line["alias".len()..].split_once(":=") {
                    file.aliases
                        .insert(name.trim().to_string(), target.trim().to_string());
                }
                continue;
            }
            let statement = line.strip_prefix("export ").unwrap_or(line);
            if let Some((name, value)) = statement.split_once(":=")
                && !name.trim().contains(char::is_whitespace)
                && !name.contains(':')
            {
                let value = value.trim();
                let literal = ['\'', '"'].iter().find_map(|quote| {
                    let inner = value.strip_prefix(*quote)?.strip_suffix(*quote)?;
                    (!inner.contains(*quote)).then(|| inner.to_string())
                });
                match literal {
                    Some(value) => file.vars.insert(name.trim().to_string(), value),
                    None => file.vars.remove(name.trim()),
                };
                continue;
            }
            let Some((name, params, variadic, deps)) = header(line) else {
                continue;
            };
            let mut body = Vec::new();
            while at < lines.len()
                && (lines[at].trim().is_empty() || lines[at].starts_with([' ', '\t']))
            {
                if !lines[at].trim().is_empty() {
                    body.push(lines[at].trim().to_string());
                }
                at += 1;
            }
            let (shell, is_a_script) = match body.first().and_then(|line| line.strip_prefix("#!")) {
                Some(interpreter) => {
                    let shell = a_shell(interpreter);
                    body.remove(0);
                    (shell, true)
                }
                None => (script.unwrap_or(true), script.is_some()),
            };
            script = None;
            file.recipes.push(JustRecipe {
                name,
                params,
                variadic,
                deps,
                body,
                shell,
                script: is_a_script,
            });
        }
    }
}

// --- task ---------------------------------------------------------------------------------------

/// A Taskfile.
#[derive(Debug)]
struct Taskfile {
    /// Its variables whose values are written out.
    vars: BTreeMap<String, String>,
    tasks: serde_json::Map<String, Value>,
}

impl Taskfile {
    fn parse(path: &str, text: &str) -> Option<Taskfile> {
        let doc = variants::parse(path, text.as_bytes())?;
        let tasks = doc.get("tasks")?.as_object()?.clone();
        Some(Taskfile {
            vars: strings(doc.get("vars")),
            tasks,
        })
    }

    /// The task `name`, by its key or one of its aliases.
    fn task(&self, name: &str) -> Option<(&str, &Value)> {
        if let Some((key, task)) = self.tasks.get_key_value(name) {
            return Some((key, task));
        }
        self.tasks.iter().find_map(|(key, task)| {
            let aliases = task.get("aliases")?.as_array()?;
            aliases
                .iter()
                .any(|alias| alias.as_str() == Some(name))
                .then_some((key.as_str(), task))
        })
    }

    /// A task's commands and dependencies: it can be one command, or a list of them.
    fn parts(task: &Value) -> (Vec<&Value>, Vec<&Value>) {
        match task {
            Value::String(_) => (vec![task], Vec::new()),
            Value::Array(cmds) => (cmds.iter().collect(), Vec::new()),
            Value::Object(task) => {
                let mut cmds: Vec<&Value> = match task.get("cmds") {
                    Some(Value::Array(cmds)) => cmds.iter().collect(),
                    Some(other) => vec![other],
                    None => Vec::new(),
                };
                cmds.extend(task.get("cmd"));
                let deps = match task.get("deps") {
                    Some(Value::Array(deps)) => deps.iter().collect(),
                    Some(other) => vec![other],
                    None => Vec::new(),
                };
                (cmds, deps)
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// The variables a task's commands see: the file's, then its own.
    fn vars_of(&self, task: &Value) -> BTreeMap<String, String> {
        let mut vars = self.vars.clone();
        vars.extend(strings(task.get("vars")));
        vars
    }

    /// `line` with its `{{.NAME}}` filled in from `vars`.
    fn expand(line: &str, vars: &BTreeMap<String, String>) -> String {
        let mut out = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let Some(end) = rest[start..].find("}}") else {
                out.push(UNKNOWN);
                return out;
            };
            let inner = rest[start + 2..start + end].trim();
            match inner.strip_prefix('.').and_then(|name| vars.get(name)) {
                Some(value) => out.push_str(value),
                None => out.push(UNKNOWN),
            }
            rest = &rest[start + end + 2..];
        }
        out.push_str(rest);
        out
    }
}

/// The entries of a `vars` mapping whose values are written out.
fn strings(vars: Option<&Value>) -> BTreeMap<String, String> {
    vars.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| match value {
            Value::String(text) => Some((name.clone(), text.clone())),
            Value::Number(n) => Some((name.clone(), n.to_string())),
            Value::Bool(b) => Some((name.clone(), b.to_string())),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository of `files`, read as `target` reads one.
    fn reading(files: &[(&str, &str)]) -> impl Fn(&str) -> Option<Vec<u8>> + use<> {
        let files: BTreeMap<String, Vec<u8>> = files
            .iter()
            .map(|(path, text)| (path.to_string(), text.as_bytes().to_vec()))
            .collect();
        move |path: &str| files.get(path).cloned()
    }

    /// What `line`, run at the root of a repository of `files`, is.
    fn of(files: &[(&str, &str)], line: &str) -> Option<Recipe> {
        of_in(files, line, "")
    }

    fn of_in(files: &[(&str, &str)], line: &str, dir: &str) -> Option<Recipe> {
        let read = reading(files);
        let words: Vec<&str> = line.split_whitespace().collect();
        target(&words, dir, &read)
    }

    fn check(name: &str) -> Option<Recipe> {
        Some(Recipe::Check(name.to_string(), Kind::Test))
    }

    fn left(why: &'static str) -> Option<Recipe> {
        Some(Recipe::Leave(why))
    }

    // covers: C-15
    #[test]
    fn a_target_whose_recipe_runs_a_check_is_one() {
        let makefile = "\
GO ?= go
PYTEST ?= pytest
GOTEST_FLAGS ?= -timeout 40m

.PHONY: test-backend test-ci lint-go ui-test

test-backend: ## test backend files
\t@echo \"Running go test with $(GOTEST_FLAGS) -tags '$(TAGS)'...\"
\t@$(GO) test $(GOTEST_FLAGS) -tags='$(TAGS)' $(GO_TEST_PACKAGES)

test-ci:
\t$(PYTEST) -q $(PYTEST_ARGS)

lint-go:
\t$(GOBIN)/golangci-lint run

ui-test: ui-build
\tcd web/ui && CI=true pnpm run test
\t# The old app, which has its own:
\tcd web/ui/react-app && CI=true pnpm run test

ui-build:
\tcd web/ui && pnpm run build
";
        let files = [("Makefile", makefile)];
        assert_eq!(of(&files, "make test-backend"), check("test-backend"));
        assert_eq!(of(&files, "make test-ci"), check("test-ci"));
        assert_eq!(of(&files, "make ui-test"), check("ui-test"));
        // A tool at a path a variable holds is that tool, and a linter's target is a linter.
        assert_eq!(
            of(&files, "make lint-go"),
            Some(Recipe::Check("lint-go".into(), Kind::Lint))
        );
        // Options, with their values, and variables on the line change nothing.
        for line in [
            "make -j4 test-ci",
            "make -j 4 test-ci",
            "make -k -s test-ci",
            "make test-ci V=1 PYTEST_ARGS=-x",
            "make --no-print-directory test-ci",
            "gmake test-ci",
        ] {
            assert_eq!(of(&files, line), check("test-ci"), "{line}");
        }
        // A name the table knows needs no recipe, wherever make starts.
        assert_eq!(
            of(&[], "make -C build test"),
            Some(Recipe::Check("test".into(), Kind::Test))
        );
        assert_eq!(
            of(&[], "make -f ci.mk lint"),
            Some(Recipe::Check("lint".into(), Kind::Lint))
        );
    }

    // covers: C-15
    #[test]
    fn make_reads_where_it_is_told() {
        let files = [
            ("web/Makefile", "ui-test:\n\tpnpm exec vitest run\n"),
            ("ci/checks.mk", "test-unit:\n\tcargo test --locked\n"),
            ("GNUmakefile", "test-gnu:\n\tgo test ./...\n"),
            ("Makefile", "test-gnu:\n\techo the other one\n"),
            (
                "app/Makefile",
                "test-app:\n\tcd ../lib && $(MAKE) test-lib\n",
            ),
            ("lib/Makefile", "test-lib:\n\tmix test\n"),
        ];
        assert_eq!(of(&files, "make -C web ui-test"), check("ui-test"));
        assert_eq!(of(&files, "make --directory=web ui-test"), check("ui-test"));
        assert_eq!(of(&files, "make -Cweb ui-test"), check("ui-test"));
        assert_eq!(
            of(&files, "make -f ci/checks.mk test-unit"),
            check("test-unit")
        );
        assert_eq!(
            of(&files, "make --file=ci/checks.mk test-unit"),
            check("test-unit")
        );
        // GNUmakefile first, as make reads it.
        assert_eq!(of(&files, "make test-gnu"), check("test-gnu"));
        // From where the step runs, and a `cd` in a recipe moves the make it runs.
        assert_eq!(of_in(&files, "make test-app", "app"), check("test-app"));
        assert_eq!(
            of_in(&files, "make -C ../web ui-test", "app"),
            check("ui-test")
        );
        // Outside the repository, nothing to read, and so no check to tell of.
        assert_eq!(of(&files, "make -C ../elsewhere test-x"), None);
        assert_eq!(of(&files, "make -C /opt/x test-x"), None);
    }

    // covers: C-15
    #[test]
    fn prerequisites_and_the_targets_a_recipe_runs_count_three_levels_down() {
        let makefile = "\
.PHONY: test-ci test-two test-three test-four check-deep a b c d

# Node's: the checks are what the prerequisites run.
test-ci: | a
\techo all done

a: b
b: c
c:
\tpytest -q

test-two:
\t$(MAKE) b

test-three: a

# The recipe of d is four levels down from deep.
test-four: deep1
deep1: deep2
deep2: deep3
deep3: d
d:
\tpytest -q

push-late: a
\t$(MAKE) push

push:
\tdocker push acme/shop
";
        let files = [("Makefile", makefile)];
        // a (1), b (2), c (3): read.
        assert_eq!(of(&files, "make test-ci"), check("test-ci"));
        assert_eq!(of(&files, "make test-two"), check("test-two"));
        assert_eq!(of(&files, "make test-three"), check("test-three"));
        // d is level 4, past what is read: its check doesn't count.
        assert_eq!(of(&files, "make test-four"), None);
        // What a recipe runs with make counts as its own.
        assert_eq!(
            of(&files, "make push-late"),
            left("its recipe pushes an image (docker push)")
        );
        // A prerequisite without a rule is a file.
        let files = [(
            "Makefile",
            "test-built: out/app.bin\n\t./out/app.bin --self-test && go test ./...\n",
        )];
        assert_eq!(of(&files, "make test-built"), check("test-built"));
    }

    // covers: C-15
    #[test]
    fn a_recipe_that_changes_something_outside_stays_with_what_it_does() {
        for (recipe, why) in [
            (
                "docker push $(IMAGE)",
                "its recipe pushes an image (docker push)",
            ),
            (
                "docker buildx build --push -t acme/shop .",
                "its recipe pushes an image (docker build --push)",
            ),
            (
                "sudo -E docker push x",
                "its recipe pushes an image (docker push)",
            ),
            (
                "$(SUDO) docker push x",
                "its recipe pushes an image (docker push)",
            ),
            (
                "git push origin HEAD:main",
                "its recipe pushes to a git remote (git push)",
            ),
            (
                "npm publish --access public",
                "its recipe publishes a package (npm publish)",
            ),
            (
                "pnpm publish -r",
                "its recipe publishes a package (pnpm publish)",
            ),
            (
                "cargo publish -p core",
                "its recipe publishes a crate (cargo publish)",
            ),
            (
                "python -m twine upload dist/*",
                "its recipe uploads a package (twine upload)",
            ),
            (
                "gh release create v1",
                "its recipe makes a GitHub release (gh release)",
            ),
            (
                "kubectl apply -f k8s/",
                "its recipe changes a cluster (kubectl apply)",
            ),
            (
                "terraform -chdir=infra apply -auto-approve",
                "its recipe changes infrastructure (terraform apply)",
            ),
            (
                "helm upgrade --install shop ./chart",
                "its recipe deploys a chart (helm upgrade)",
            ),
            ("aws s3 sync dist s3://shop", "its recipe calls AWS (aws)"),
            (
                "gcloud run deploy shop",
                "its recipe calls Google Cloud (gcloud)",
            ),
            ("az webapp up", "its recipe calls Azure (az)"),
            (
                "scp dist/app host:/srv",
                "its recipe copies files to another machine (scp)",
            ),
            ("rsync -av dist/ deploy@host:/srv/www", RSYNC),
            ("curl -X POST https://hooks.example/deploy", CURL_METHOD),
            ("curl --request=DELETE https://api.example/x", CURL_METHOD),
            (
                "curl -fsS -d @payload.json https://api.example/x",
                CURL_DATA,
            ),
            (
                "./gradlew publishToMavenCentral",
                "its recipe publishes a build (gradle publish)",
            ),
            ("mvn -B deploy", "its recipe deploys a build (mvn deploy)"),
            // Anywhere in the line, a substitution included, which make writes `$$(`.
            (
                "pytest && docker push x",
                "its recipe pushes an image (docker push)",
            ),
            (
                "echo \"$$(git push)\"",
                "its recipe pushes to a git remote (git push)",
            ),
            (
                "echo `git push`",
                "its recipe pushes to a git remote (git push)",
            ),
            (
                "if aws sts get-caller-identity; then pytest; fi",
                "its recipe calls AWS (aws)",
            ),
        ] {
            let makefile = format!("test-deploy: image\n\tpytest -q\n\nimage:\n\t{recipe}\n");
            let files = [("Makefile", makefile.as_str())];
            assert_eq!(of(&files, "make test-deploy"), left(why), "{recipe}");
        }
        // What only looks like it.
        for recipe in [
            "rsync -a src/ build/",
            "rsync -a C:/src/ build/",
            "curl -fsSL https://example.com/install.sh -o install.sh",
            "curl -X GET https://api.example/health",
            "docker build -t acme/shop .",
            "docker run --rm acme/shop pytest",
            "npm run build",
        ] {
            let makefile = format!("test-ci:\n\t{recipe}\n\tpytest -q\n");
            let files = [("Makefile", makefile.as_str())];
            assert_eq!(of(&files, "make test-ci"), check("test-ci"), "{recipe}");
        }
    }

    // covers: C-15
    #[test]
    fn a_recipe_that_cant_be_read_says_so() {
        // Each runs a check, and something hushrun can't tell: what that does could undo it.
        let makefile = "\
test-env:
\tpytest -q
\t$(DEPLOY) -q

test-shell-var:
\tpytest -q
\t$$PUBLISH --all

test-shell:
\tpytest -q
\t$(shell which tool) -q

test-gone: missing-target-with-no-rule
\tpytest

test-call: lint
\t$(MAKE) not-a-target

test-dir: lint
\t$(MAKE) -C $(SUBDIR) release

test-cd: lint
\tcd $(SOMEWHERE) && $(MAKE) release

test-elsewhere: lint
\t$(MAKE) -C nowhere release

lint:
\truff check .
";
        let files = [("Makefile", makefile)];
        for line in [
            "make test-env",
            "make test-shell-var",
            "make test-shell",
            "make test-call",
            "make test-dir",
            "make test-cd",
            "make test-elsewhere",
        ] {
            assert_eq!(of(&files, line), left(CANT_READ), "{line}");
        }
        // A prerequisite with no rule is a file, so `test-gone` runs pytest.
        assert_eq!(of(&files, "make test-gone"), check("test-gone"));
        // Without a check to tell of, what can't be read says nothing, as a command the table
        // doesn't know says nothing: no Makefile, no target, a program a variable holds.
        let unknown = [("Makefile", "test-unknown:\n\t$(TEST_RUNNER) ./...\n")];
        assert_eq!(of(&unknown, "make test-unknown"), None);
        for line in [
            "make test-nothing-by-this-name",
            "make -C nowhere test-x",
            "make -f missing.mk test-x",
        ] {
            assert_eq!(of(&files, line), None, "{line}");
        }
        assert_eq!(of(&[], "make test-ci"), None);
        assert_eq!(of(&[], "just test-ci"), None);
        assert_eq!(of(&[], "task test-ci"), None);
    }

    // covers: C-15
    #[test]
    fn what_runs_no_check_or_nothing_says_nothing() {
        let makefile = "\
build:
\tgo build -o bin/app ./cmd/app

docs:
\tmkdocs build

deploy-docs: docs
\t@echo built

# What pushes and runs no check is no check, whatever else it does.
release: build
\tgit push --tags
\t$(PUBLISH) dist
";
        let files = [("Makefile", makefile)];
        for line in [
            "make build",
            "make docs",
            "make deploy-docs",
            "make release",
            // No target, or several: that is make's default goal, or several checks.
            "make",
            "make -C .",
            "make build docs",
            // A target the shell names.
            "make $TARGET",
            "make test-${{ matrix.suite }}",
            "just ${RECIPE}",
            // It runs nothing: a dry run, a question, its version.
            "make -n test-ci",
            "make --dry-run test-ci",
            "make -q test-ci",
            "make --version",
            "just --list",
            "just -n test-ci",
            "task --list",
            "task --dry test-ci",
            // Not a task runner.
            "maker test-ci",
            "./make test-ci",
        ] {
            assert_eq!(of(&files, line), None, "{line}");
        }
    }

    // covers: C-15
    #[test]
    fn variables_conditionals_and_includes_are_read_as_written() {
        let makefile = "\
include ci.mk
-include missing.mk

PY := python3
TEST_CMD = $(PY) -m pytest
FLAGS += -q
ifeq ($(OS),Windows_NT)
RUNNER = $(PY) -m unittest
else
RUNNER = $(TEST_CMD)
endif

test-unit:
ifeq ($(CI),true)
\t$(RUNNER) $(FLAGS)
else
\t@echo no tests here
endif

test-all: test-unit test-included

test-%:
\tgo test ./$*/...

test-lint: ; ruff check .

test-twice::
\tpytest -q
test-twice::
\tgit push

# A function of the file's own, and what make sets itself.
PYTHON ?= python3
run-tests = $(PYTHON) -m pytest $(1) $(2)
test-call:
\t$(call run-tests,tests/unit)
test-clean:
\t$(RM) -r build && $(CC) --version && go test ./...
";
        let files = [
            ("Makefile", makefile),
            ("ci.mk", "test-included:\n\tcargo test\n"),
        ];
        assert_eq!(of(&files, "make test-call"), check("test-call"));
        assert_eq!(of(&files, "make test-clean"), check("test-clean"));
        assert_eq!(of(&files, "make test-unit"), check("test-unit"));
        assert_eq!(of(&files, "make test-included"), check("test-included"));
        assert_eq!(of(&files, "make test-all"), check("test-all"));
        // A pattern rule's recipe, with its stem.
        assert_eq!(of(&files, "make test-api"), check("test-api"));
        // A recipe on the rule's own line.
        assert_eq!(
            of(&files, "make test-lint"),
            Some(Recipe::Check("test-lint".into(), Kind::Lint))
        );
        // Each recipe of a `::` target runs.
        assert_eq!(
            of(&files, "make test-twice"),
            left("its recipe pushes to a git remote (git push)")
        );
    }

    // covers: C-15
    #[test]
    fn a_rule_for_anything_takes_what_make_is_asked_for_and_no_file() {
        let files = [
            (
                "Makefile",
                "test-unit: go.mod\n\tgo test ./...\n\n%:\n\t@$(MAKE) -f other.mk $@\n",
            ),
            ("other.mk", "test-other:\n\tcargo test\n"),
        ];
        // go.mod is a file, which the rule for anything doesn't make.
        assert_eq!(of(&files, "make test-unit"), check("test-unit"));
        // A target of no rule of its own is the rule for anything's, which hands it on.
        assert_eq!(of(&files, "make test-other"), check("test-other"));
    }

    // covers: C-15
    #[test]
    fn just_recipes_are_read_by_their_lines_and_dependencies() {
        let justfile = "\
set shell := [\"bash\", \"-cu\"]

pytest := 'uv run pytest'

alias t := test-python

# Runs the tests.
test-python *args: lint-python
    {{pytest}} -q {{args}}

lint-python:
    ruff check .

[private]
@build:
    cargo build

test-script:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo test --locked

test-node: lint-python
    #!/usr/bin/env node
    console.log('hi')

[script('python3')]
test-py-script: lint-python
    print('hi')

test-release: test-python
    git push --tags

test-param target:
    cargo test -p {{target}}

test-variable: lint-python
    {{runner}} --all
";
        let files = [("justfile", justfile)];
        assert_eq!(of(&files, "just test-python"), check("test-python"));
        assert_eq!(of(&files, "just test-python -k slow"), check("test-python"));
        assert_eq!(of(&files, "just t"), check("t"));
        assert_eq!(of(&files, "just test-script"), check("test-script"));
        assert_eq!(of(&files, "just test-param core"), check("test-param"));
        assert_eq!(
            of(&files, "just lint-python"),
            Some(Recipe::Check("lint-python".into(), Kind::Lint))
        );
        assert_eq!(
            of(&files, "just test-release"),
            left("its recipe pushes to a git remote (git push)")
        );
        // A script in another language, or a program a variable holds, beside a check.
        for line in [
            "just test-node",
            "just test-py-script",
            "just test-variable",
        ] {
            assert_eq!(of(&files, line), left(CANT_READ), "{line}");
        }
        assert_eq!(of(&files, "just test-missing"), None);
        assert_eq!(of(&files, "just build"), None);
        // A shell that isn't one runs every line, as Codex's justfile has it: none is a shell's,
        // so no check is found, but in a script with its own.
        let python = "set shell := [\"python3\", \"-c\"]\n\ntest-ci: lint\n    pytest -q\n\nlint:\n    ruff check .\n\ntest-script:\n    #!/usr/bin/env bash\n    cargo test\n\ntest-both: test-script\n    pytest -q\n";
        let files = [("justfile", python)];
        assert_eq!(of(&files, "just test-ci"), None);
        assert_eq!(of(&files, "just test-script"), check("test-script"));
        assert_eq!(of(&files, "just test-both"), left(CANT_READ));
        let files = [("justfile", justfile)];
        // Two recipes are two checks.
        assert_eq!(of(&files, "just lint-python test-script"), None);
        // A justfile above, as just looks for it, or one the line names.
        assert_eq!(
            of_in(&files, "just test-script", "crates/core"),
            check("test-script")
        );
        let named = [("ci/Justfile", "test-ci:\n  go test ./...\n")];
        assert_eq!(
            of(&named, "just --justfile ci/Justfile test-ci"),
            check("test-ci")
        );
        assert_eq!(of(&named, "just ci/test-ci"), check("test-ci"));
        let hidden = [(".justfile", "test-ci:\n  go test ./...\n")];
        assert_eq!(of(&hidden, "just test-ci"), check("test-ci"));
    }

    // covers: C-15
    #[test]
    fn task_targets_are_read_by_their_commands_and_dependencies() {
        let taskfile = "\
version: '3'
vars:
  GOTEST: go test
tasks:
  test-go:
    desc: Runs the tests.
    deps: [lint-go]
    cmds:
      - '{{.GOTEST}} ./... {{.CLI_ARGS}}'
  lint-go:
    cmds:
      - golangci-lint run
  test-all:
    cmds:
      - task: test-go
      - cmd: echo done
  test-short: go test -short ./...
  test-release:
    cmds:
      - task: test-go
      - git push --tags
  test-env:
    deps: [lint-go]
    cmds:
      - '{{.RUNNER}} --all'
  build:
    aliases: [b]
    cmds:
      - go build ./...
  test-in-dir:
    dir: web
    cmds:
      - pnpm run test
";
        let files = [("Taskfile.yml", taskfile)];
        assert_eq!(of(&files, "task test-go"), check("test-go"));
        assert_eq!(of(&files, "task test-go -- -run TestX"), check("test-go"));
        assert_eq!(of(&files, "task test-all"), check("test-all"));
        assert_eq!(of(&files, "task test-short"), check("test-short"));
        assert_eq!(of(&files, "task test-in-dir"), check("test-in-dir"));
        assert_eq!(
            of(&files, "task lint-go"),
            Some(Recipe::Check("lint-go".into(), Kind::Lint))
        );
        assert_eq!(
            of(&files, "task test-release"),
            left("its recipe pushes to a git remote (git push)")
        );
        assert_eq!(of(&files, "task test-env"), left(CANT_READ));
        assert_eq!(of(&files, "task test-missing"), None);
        assert_eq!(of(&files, "task build"), None);
        assert_eq!(of(&files, "task b"), None);
        // Two tasks are two checks.
        assert_eq!(of(&files, "task lint-go test-go"), None);
        // One above, or one the line names; a Taskfile that isn't one has no check to tell of.
        assert_eq!(of_in(&files, "task test-go", "cmd/app"), check("test-go"));
        let named = [(
            "ci/tasks.yml",
            "version: '3'\ntasks:\n  test-ci: cargo test\n",
        )];
        assert_eq!(of(&named, "task -t ci/tasks.yml test-ci"), check("test-ci"));
        let broken = [("Taskfile.yml", "version: '3'\ntasks: [not, a, mapping]\n")];
        assert_eq!(of(&broken, "task test-ci"), None);
        // A target another file holds, which can't be read, beside a check.
        let elsewhere = [(
            "Taskfile.yml",
            "version: '3'\ntasks:\n  test-ci:\n    cmds:\n      - go test ./...\n      - task -d gone release\n",
        )];
        assert_eq!(of(&elsewhere, "task test-ci"), left(CANT_READ));
    }

    // covers: C-15
    #[test]
    fn one_runner_follows_another_and_an_advisory_check_stays() {
        let files = [
            ("Makefile", "test-all:\n\tjust test-rust\n\ttask test-go\n"),
            ("justfile", "test-rust:\n  cargo nextest run\n"),
            (
                "Taskfile.yml",
                "version: '3'\ntasks:\n  test-go: go test ./...\n",
            ),
            ("audit.mk", "test-audit:\n\tcargo test\n\tcargo audit\n"),
        ];
        assert_eq!(of(&files, "make test-all"), check("test-all"));
        assert_eq!(
            of(&files, "make -f audit.mk test-audit"),
            left("its recipe's answer changes with an advisory database, not only with the code")
        );
    }

    // covers: C-15
    #[test]
    fn a_shell_line_is_read_command_by_command() {
        let commands = |text: &str| -> Vec<Vec<String>> { split_commands(text) };
        assert_eq!(
            commands("cd web && CI=true pnpm run test 2>&1 | tee log.txt"),
            vec![
                vec!["cd".to_string(), "web".to_string()],
                vec!["CI=true".into(), "pnpm".into(), "run".into(), "test".into()],
                vec!["tee".into(), "log.txt".into()],
            ]
        );
        assert_eq!(
            commands("pytest -q > out.txt; echo \"$(git push)\" # a comment"),
            vec![
                vec!["pytest".to_string(), "-q".to_string()],
                vec!["echo".into(), UNKNOWN.to_string()],
                vec!["git".into(), "push".into()],
            ]
        );
        assert_eq!(
            commands("go test -tags='a b' \"${PKG}\" ./..."),
            vec![vec![
                "go".to_string(),
                "test".into(),
                "-tags=a b".into(),
                "${PKG}".into(),
                "./...".into()
            ]]
        );
        let past = |line: &str| -> Vec<String> {
            let words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
            past_prefixes(&words).to_vec()
        };
        assert_eq!(
            past("sudo -u ci env A=1 docker push x"),
            ["docker", "push", "x"]
        );
        assert_eq!(past("timeout -k 5 600 xvfb-run -a pytest"), ["pytest"]);
        assert_eq!(past("if FOO=1 pytest"), ["pytest"]);
        assert!(past("for f in a b").is_empty());
    }
}
