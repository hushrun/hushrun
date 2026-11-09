//! How a repository divides into projects, so that a check skips until its own files change
//! (C-15). A scope that left out something a check reads would skip a check that should run, so
//! a split has to be sure. A project reads more than its manifest says: a config at the root, a
//! neighbour's file its tests open. So every scope holds the root's files, and what a project
//! names, in its manifests, as a `../` path in any file it tracks, or as a link it tracks, joins
//! its scope or becomes `external`. Whatever hushrun can't read keeps the repository in one scope,
//! which costs a run and never a wrong skip. A wrong hit only widens a scope, which is the safe
//! direction.
//!
//! What it can't see is a path built at run time (ADR 0037), which the plan says whenever it
//! splits.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use hushrun_core::glob::Pattern;
use hushrun_core::policy::Policy;

use crate::cmd::wrap::made_name;

use super::cargo;
use super::workspaces::Workspace;

/// A scope init proposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    /// Its directory, relative to the repository root; `""` for the whole repository.
    pub dir: String,
    /// What the scope includes.
    pub include: Vec<String>,
    /// What it leaves out of that.
    pub exclude: Vec<String>,
    /// What it builds against outside the repository, relative to its root.
    pub external: Vec<String>,
}

/// How checks map to scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// One scope: `hushrun run` needs no `--scope`. Why, when it isn't the obvious.
    One(Option<String>),
    /// A scope per project, which init adds to hushrun.yaml.
    Projects(Vec<Project>),
    /// The scopes hushrun.yaml has: a directory, relative to the root, is in the one named.
    Declared(BTreeMap<String, String>),
}

impl Layout {
    /// The scope a check in `dir` runs under; `Ok(None)` when it needs none. An error says why
    /// no scope fits.
    pub fn scope_of(&self, dir: &str) -> Result<Option<String>, String> {
        match self {
            Layout::One(_) => Ok(None),
            Layout::Projects(projects) => Ok(projects
                .iter()
                .filter(|project| project.holds(dir))
                .max_by_key(|project| project.dir.len())
                .map(|project| project.name.clone())),
            Layout::Declared(scopes) => match scopes.get(dir) {
                Some(scope) => Ok(Some(scope.clone())),
                None => Err(format!(
                    "no one scope of hushrun.yaml covers {}",
                    shown(dir)
                )),
            },
        }
    }
}

impl Project {
    fn holds(&self, dir: &str) -> bool {
        self.dir.is_empty()
            || dir == self.dir
            || dir
                .strip_prefix(self.dir.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

fn shown(dir: &str) -> &str {
    if dir.is_empty() { "the root" } else { dir }
}

/// Every scope holds the root's files, dotfiles included: a project's tools read configs there
/// (`extends`, a shared lint or format config, `rust-toolchain.toml`, `Directory.Build.props`)
/// that no manifest names. `*` stops at a `/`, so this is the root and nothing below it.
const ROOT_FILES: &str = "*";

/// The root's files no check reads, which change with every release: without this, a changelog's
/// edit would run every check. These stop at a `/` too, so a project's own docs still count.
const ROOT_PROSE: &[&str] = &["*.md", "LICENSE*", "NOTICE*"];

/// What every scope holds when it exists: what CI runs, and how it builds.
const SHARED: &[&str] = &[".github/workflows/**", ".github/actions/**", ".cargo/**"];

/// A file past this isn't read: it is data or generated, not where a project names a path.
const MAX_FILE_BYTES: u64 = 1 << 20;
/// A project past either of these keeps one scope: hushrun can't say what it reads.
const MAX_PROJECT_FILES: usize = 20_000;
const MAX_PROJECT_BYTES: u64 = 256 << 20;
/// The longest path a scan reads.
const MAX_PATH_BYTES: usize = 512;

/// What in a repository's root ties its projects together: one lockfile or one build for all of
/// them, so a change in one can be a change in another.
const WORKSPACES: &[&str] = &[
    "pnpm-workspace.yaml",
    "go.work",
    "settings.gradle",
    "settings.gradle.kts",
    "lerna.json",
    "nx.json",
    "turbo.json",
];

/// Manifests hushrun can't read yet: a project with one of these has dependencies it can't see.
const UNREAD: &[&str] = &[
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "composer.json",
    "mix.exs",
    "Package.swift",
    "requirements.txt",
    "setup.py",
];

/// How the checks in `dirs` map to scopes, given `existing`, hushrun.yaml as it is.
#[cfg(test)]
pub fn layout(root: &Path, dirs: &BTreeSet<String>, existing: Option<&Policy>) -> Layout {
    layout_with(root, dirs, existing, None)
}

/// How the checks in `dirs` map to scopes, given `existing`, hushrun.yaml as it is, and
/// `workspace`, the root's JavaScript workspace when it heads one (ADR 0051).
pub fn layout_with(
    root: &Path,
    dirs: &BTreeSet<String>,
    existing: Option<&Policy>,
    workspace: Option<&Workspace>,
) -> Layout {
    if let Some(policy) = existing {
        if policy.scopes.len() > 1 {
            return declared(dirs, policy);
        }
        // Declared, or with events and artifacts that name `default`: not init's to change.
        let declares = policy.text.lines().any(|line| line.starts_with("scopes:"));
        if declares {
            return Layout::One(None);
        }
        if policy.deploys() || !policy.artifacts.is_empty() {
            return Layout::One(split(root, dirs, workspace).ok().filter(|p| !p.is_empty()).map(|_| {
                "hushrun.yaml deploys, and a deploy's evidence is per scope: add scopes to it by hand"
                    .to_string()
            }));
        }
    }
    match split(root, dirs, workspace) {
        Ok(projects) if projects.is_empty() => Layout::One(None),
        Ok(projects) => Layout::Projects(projects),
        Err(why) => Layout::One(Some(why)),
    }
}

/// The one scope of `policy` each directory is in.
fn declared(dirs: &BTreeSet<String>, policy: &Policy) -> Layout {
    let found = dirs
        .iter()
        .filter_map(|dir| Some((dir.clone(), the_scope_of(policy, dir)?)))
        .collect();
    Layout::Declared(found)
}

/// The one scope of `policy` that holds `dir`, when one does.
pub fn the_scope_of(policy: &Policy, dir: &str) -> Option<String> {
    // A name nothing has, so that only a pattern for the directory matches it.
    let probe = match dir {
        "" => "hushrun-probe".to_string(),
        dir => format!("{dir}/hushrun-probe"),
    };
    let scopes: Vec<_> = policy
        .scopes
        .iter()
        .filter(|scope| policy.scopes.contains(scope, &probe))
        .collect();
    match scopes.as_slice() {
        [scope] => Some(scope.name.to_string()),
        _ => None,
    }
}

/// One project per directory with checks, or why the repository can't be split. None when every
/// check is at the root. \[ADR 0051\] In a JavaScript workspace, a check's project is its
/// package, which reaches the packages it depends on.
fn split(
    root: &Path,
    dirs: &BTreeSet<String>,
    js: Option<&Workspace>,
) -> Result<Vec<Project>, String> {
    if dirs.iter().all(String::is_empty) {
        return Ok(Vec::new());
    }
    if let Some(why) = js.and_then(|js| js.unreadable.clone()) {
        return Err(why);
    }
    if let Some(file) = workspace(root, js.is_some()) {
        return Err(format!("{file} ties its projects together"));
    }
    let mut homes: BTreeSet<String> = BTreeSet::new();
    let mut whole = false;
    for dir in dirs {
        if dir.is_empty() {
            whole = true;
        } else if let Some(member) = js.and_then(|js| js.member_of(dir)) {
            homes.insert(member.dir.clone());
        } else {
            homes.insert(home_of(root, dir).ok_or_else(|| {
                format!("{dir} has no manifest hushrun can read what it depends on from")
            })?);
        }
    }
    let tracked = tracked(root)?;
    let shared: Vec<String> = SHARED
        .iter()
        .filter(|path| root.join(path.trim_end_matches("/**")).exists())
        .map(|path| (*path).to_string())
        .collect();
    let mut projects = Vec::new();
    for dir in &homes {
        // \[ADR 0051\] A package that depends on the root's reads the whole repository.
        let member = js.and_then(|js| js.members.iter().find(|member| member.dir == *dir));
        if member.is_some_and(|member| member.needs.contains("")) {
            projects.push(Project {
                name: String::new(),
                dir: dir.clone(),
                include: vec!["**".to_string()],
                exclude: Vec::new(),
                external: Vec::new(),
            });
            continue;
        }
        let reached = reach(root, dir, &tracked, js)?;
        let mut include = vec![directory_pattern(dir)?];
        for path in reached.dirs.iter().filter(|path| *path != dir) {
            include.push(directory_pattern(path)?);
        }
        for file in reached.files.iter().filter(|file| {
            file.contains('/')
                && !reached
                    .dirs
                    .iter()
                    .any(|dir| file.starts_with(&format!("{dir}/")))
        }) {
            include.push(file_pattern(file)?);
        }
        include.push(ROOT_FILES.to_string());
        include.extend(shared.iter().cloned());
        // \[ADR 0051\] What a workspace's tools say every package reads.
        include.extend(js.into_iter().flat_map(|js| js.everywhere.iter().cloned()));
        include.dedup();
        projects.push(Project {
            name: String::new(),
            dir: dir.clone(),
            include,
            exclude: exclude_for(&reached.files),
            external: reached.external.into_iter().collect(),
        });
    }
    name_them(&mut projects, js)?;
    if whole {
        projects.push(Project {
            name: unused("all", &projects),
            dir: String::new(),
            include: vec!["**".to_string()],
            exclude: Vec::new(),
            external: Vec::new(),
        });
    }
    Ok(projects)
}

/// What the scope leaves out of the root: all of [`ROOT_PROSE`], but what something names. A
/// file a project reads counts, whatever it is called.
fn exclude_for(reached: &BTreeSet<String>) -> Vec<String> {
    ROOT_PROSE
        .iter()
        .filter(|pattern| {
            let Ok(compiled) = Pattern::new(pattern) else {
                return false;
            };
            !reached.iter().any(|file| compiled.is_match(file))
        })
        .map(|pattern| (*pattern).to_string())
        .collect()
}

/// Whether a pattern can hold `path` as it is: it has no escape for its own syntax, and a scope's
/// patterns go between double quotes.
fn plain(path: &str) -> bool {
    !path.contains(['*', '?', '[', ']', '{', '}', '"', '\\'])
}

/// The pattern for all of `dir`.
fn directory_pattern(dir: &str) -> Result<String, String> {
    match plain(dir) {
        true => Ok(format!("{dir}/**")),
        false => above(dir),
    }
}

/// The pattern for `file`, or for the nearest directory above it a pattern can name.
fn file_pattern(file: &str) -> Result<String, String> {
    match plain(file) {
        true => Ok(file.to_string()),
        false => above(file),
    }
}

/// All of the deepest directory above `path` that a pattern can name: a wider scope, never a
/// narrower one.
fn above(path: &str) -> Result<String, String> {
    let parts: Vec<&str> = path.split('/').take_while(|part| plain(part)).collect();
    match parts.as_slice() {
        [] => Err(format!("no pattern can name {path}")),
        parts => Ok(format!("{}/**", parts.join("/"))),
    }
}

/// The files git tracks, so that `target` and `node_modules` never count.
pub(super) fn tracked(root: &Path) -> Result<BTreeSet<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .ok_or_else(|| "git doesn't say which files it tracks".to_string())?;
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|path| std::str::from_utf8(path).ok())
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

fn unused(name: &str, projects: &[Project]) -> String {
    match projects.iter().any(|project| project.name == name) {
        false => name.to_string(),
        true => format!("{name}-of-it"),
    }
}

/// A scope's name is its directory's, or its path's where two share one. \[ADR 0051\] A package
/// of a workspace takes its package's name.
fn name_them(projects: &mut [Project], js: Option<&Workspace>) -> Result<(), String> {
    let last = |dir: &str| {
        js.and_then(|js| js.members.iter().find(|member| member.dir == dir))
            .and_then(|member| member.name.clone())
            .unwrap_or_else(|| dir.rsplit('/').next().unwrap_or(dir).to_string())
    };
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for project in projects.iter() {
        *seen.entry(last(&project.dir)).or_default() += 1;
    }
    for project in projects.iter_mut() {
        let short = last(&project.dir);
        let text = match seen[&short] {
            1 => short,
            _ => project.dir.clone(),
        };
        project.name = made_name(&text)
            .map(|name| name.to_string())
            .ok_or_else(|| format!("{} gives no scope name", project.dir))?;
    }
    let mut names = BTreeSet::new();
    for project in projects.iter() {
        if !names.insert(project.name.clone()) {
            return Err(format!("two projects would be scope {}", project.name));
        }
    }
    Ok(())
}

/// The workspace file at the root, if any. \[ADR 0051\] `js` says hushrun read the root's
/// JavaScript workspace, whose files and Turborepo's then tie nothing more.
fn workspace(root: &Path, js: bool) -> Option<String> {
    const READ: &[&str] = &["pnpm-workspace.yaml", "lerna.json", "turbo.json"];
    for file in WORKSPACES
        .iter()
        .filter(|file| !(js && READ.contains(file)))
    {
        if root.join(file).exists() {
            return Some((*file).to_string());
        }
    }
    let text = |file: &str| std::fs::read_to_string(root.join(file)).ok();
    let has = |file: &str, table: &str| {
        text(file).is_some_and(|text| text.lines().any(|line| line.trim() == table))
    };
    if has("Cargo.toml", "[workspace]") {
        return Some("Cargo.toml".to_string());
    }
    if has("pyproject.toml", "[tool.uv.workspace]") {
        return Some("pyproject.toml".to_string());
    }
    let npm = text("package.json")
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|manifest| manifest.get("workspaces").is_some());
    (npm && !js).then(|| "package.json".to_string())
}

/// `dir`, or the nearest directory above it with a manifest: the project it is in. None at the
/// root: the root's manifest is the repository's.
fn home_of(root: &Path, dir: &str) -> Option<String> {
    let mut here = dir.to_string();
    loop {
        if !here.is_empty() && has_a_manifest(&root.join(&here)) {
            return Some(here);
        }
        here = match here.rsplit_once('/') {
            Some((parent, _)) => parent.to_string(),
            None => return None,
        };
    }
}

fn has_a_manifest(dir: &Path) -> bool {
    const READ: &[&str] = &["Cargo.toml", "package.json", "pyproject.toml", "go.mod"];
    READ.iter()
        .chain(UNREAD)
        .any(|file| dir.join(file).exists())
        || files_in(dir).iter().any(|file| {
            [".tf", ".csproj", ".fsproj", ".sln"]
                .iter()
                .any(|end| file.ends_with(end))
        })
}

fn files_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// What a project reaches.
#[derive(Debug, Default)]
struct Reached {
    /// Directories in the repository, the project's own first among them.
    dirs: BTreeSet<String>,
    /// Single files in the repository.
    files: BTreeSet<String>,
    /// Checkouts outside it.
    external: BTreeSet<String>,
}

impl Reached {
    fn holds(&self, target: &str) -> bool {
        self.dirs
            .iter()
            .any(|dir| target == dir || target.starts_with(&format!("{dir}/")))
            || self.files.contains(target)
    }
}

/// What's left to read.
enum Waiting {
    Dir(String),
    File(String),
}

/// How much of a project has been read.
#[derive(Debug, Default)]
struct Budget {
    files: usize,
    bytes: u64,
}

/// Everything `dir` reaches: what its manifests name, and what its files and links name, each of
/// those in turn. \[ADR 0051\] A package of `js` reaches the packages it depends on.
fn reach(
    root: &Path,
    dir: &str,
    tracked: &BTreeSet<String>,
    js: Option<&Workspace>,
) -> Result<Reached, String> {
    let mut reached = Reached::default();
    reached.dirs.insert(dir.to_string());
    let mut waiting = vec![Waiting::Dir(dir.to_string())];
    // \[ADR 0055\] A crate inside a workspace builds as part of it, with its lockfile, its other
    // members and its `[patch]`: all of it.
    if let Some(workspace) = cargo::workspace_above(root, dir)? {
        place(root, workspace, tracked, &mut reached, &mut waiting)?;
    }
    let mut budget = Budget::default();
    while let Some(next) = waiting.pop() {
        let targets = match &next {
            Waiting::Dir(here) => {
                let mut targets = Vec::new();
                for path in dependencies(root, here, js.is_some(), here == dir)? {
                    targets.push(normal(&format!("{here}/{path}")).ok_or_else(|| {
                        format!("{here} depends on {path}, which hushrun can't place")
                    })?);
                }
                let member = js.and_then(|js| js.members.iter().find(|m| m.dir == *here));
                let needs = member.into_iter().flat_map(|member| &member.needs);
                // The root's own package is everything: a scope of `**` stands for it above.
                targets.extend(needs.filter(|need| !need.is_empty()).cloned());
                let prefix = format!("{here}/");
                for file in tracked
                    .range(prefix.clone()..)
                    .take_while(|file| file.starts_with(&prefix))
                {
                    targets.extend(named_in(root, file, here, tracked, &mut budget, dir)?);
                }
                targets
            }
            Waiting::File(file) => {
                let here = file.rsplit_once('/').map_or("", |(parent, _)| parent);
                named_in(root, file, here, tracked, &mut budget, dir)?
            }
        };
        for target in targets {
            place(root, target, tracked, &mut reached, &mut waiting)?;
        }
    }
    Ok(reached)
}

/// Puts `target`, a path from the root, where it belongs.
fn place(
    root: &Path,
    target: String,
    tracked: &BTreeSet<String>,
    reached: &mut Reached,
    waiting: &mut Vec<Waiting>,
) -> Result<(), String> {
    if target.is_empty() {
        return Err("a project names the repository's root, all of it".to_string());
    }
    if outside(&target) {
        if target.split('/').all(|part| part == "..") {
            return Err(format!("a project names {target}, above the repository"));
        }
        reached.external.insert(checkout_of(root, &target));
        return Ok(());
    }
    if reached.holds(&target) {
        return Ok(());
    }
    let (dir, files) = find(tracked, &target);
    if dir {
        reached.dirs.insert(target.clone());
        waiting.push(Waiting::Dir(target));
    }
    for file in files {
        if reached.files.insert(file.clone()) {
            waiting.push(Waiting::File(file));
        }
    }
    Ok(())
}

/// Whether `target`, a path from the root, leaves the repository.
fn outside(target: &str) -> bool {
    target == ".." || target.starts_with("../")
}

/// What `target` is among the tracked files: whether it's a directory, and the files it names:
/// itself, or, as an import names one, the files of that name with any extension.
fn find(tracked: &BTreeSet<String>, target: &str) -> (bool, Vec<String>) {
    let inside = format!("{target}/");
    let dir = tracked
        .range(inside.clone()..)
        .next()
        .is_some_and(|file| file.starts_with(&inside));
    if tracked.contains(target) {
        return (dir, vec![target.to_string()]);
    }
    // A tracked path with more below it is a link, or a submodule: this path goes through it.
    let through = target
        .match_indices('/')
        .rev()
        .map(|(at, _)| &target[..at])
        .find(|above| tracked.contains(*above));
    if let Some(above) = through {
        return (false, vec![above.to_string()]);
    }
    let dotted = format!("{target}.");
    let files = tracked
        .range(dotted.clone()..)
        .take_while(|file| file.starts_with(&dotted))
        .filter(|file| !file[dotted.len()..].contains('/'))
        .cloned()
        .collect();
    (dir, files)
}

/// The paths `file` names that leave the project: each resolved from the file, and from `here`,
/// the directory it counts from, which is how `join("../site/x")` in a test reads. Only what
/// exists counts: a wrong hit widens a scope, but a path to nothing says nothing. A file too big or
/// binary says nothing, and too much of it all is an error: hushrun can't say what the project
/// reads. A link names where it leads, and one that can't be placed is an error too.
fn named_in(
    root: &Path,
    file: &str,
    here: &str,
    tracked: &BTreeSet<String>,
    budget: &mut Budget,
    project: &str,
) -> Result<Vec<String>, String> {
    let base = file.rsplit_once('/').map_or("", |(parent, _)| parent);
    let leaves = |target: &str| target != project && !target.starts_with(&format!("{project}/"));
    let there = |target: &str| match outside(target) {
        true => root.join(target).exists(),
        false => {
            let (dir, files) = find(tracked, target);
            dir || !files.is_empty()
        }
    };
    let path = root.join(file);
    // The link itself, not what it leads to: the digest holds only its text.
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return Ok(Vec::new());
    };
    if meta.is_symlink() {
        let link =
            std::fs::read_link(&path).map_err(|_| format!("hushrun can't read the link {file}"))?;
        let cant = || {
            format!(
                "{file} links to {}, which hushrun can't place",
                link.display()
            )
        };
        let written = link
            .to_str()
            .filter(|_| link.is_relative())
            .ok_or_else(cant)?;
        let target = normal(&joined(base, written)).ok_or_else(cant)?;
        return match leaves(&target) {
            false => Ok(Vec::new()),
            true if target.is_empty() || outside(&target) || there(&target) => Ok(vec![target]),
            true => Err(cant()),
        };
    }
    // Prose: a README's `../admin` reads nothing, and would join every project to its neighbours.
    if file.ends_with(".md") {
        return Ok(Vec::new());
    }
    let size = meta.len();
    budget.files += 1;
    if size <= MAX_FILE_BYTES {
        budget.bytes += size;
    }
    if budget.files > MAX_PROJECT_FILES || budget.bytes > MAX_PROJECT_BYTES {
        return Err(format!("{project} is too big to read what its files name"));
    }
    if size > MAX_FILE_BYTES {
        return Ok(Vec::new());
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(Vec::new());
    };
    if bytes.contains(&0) {
        return Ok(Vec::new());
    }
    let mut targets = Vec::new();
    for named in quoted_parents(&bytes)
        .into_iter()
        .chain(bare_parents(&bytes))
    {
        let Some(from_file) = normal(&joined(base, &named)) else {
            continue;
        };
        let names_something = named
            .split('/')
            .any(|part| !matches!(part, "" | "." | ".."));
        // `"../.."` from a file is a module beside it, or the root: from `here`, it means nothing.
        if !names_something {
            if leaves(&from_file) && !from_file.is_empty() && !outside(&from_file) {
                targets.push(from_file);
            } else if from_file.is_empty() || outside(&from_file) {
                return Err(format!(
                    "{file} names {named}, which is the whole repository"
                ));
            }
            continue;
        }
        let as_written = there(&from_file);
        if as_written && leaves(&from_file) {
            targets.push(from_file);
        }
        let Some(from_here) = normal(&joined(here, &named)) else {
            continue;
        };
        // A path that works from its own file isn't one that leaves the repository from `here`:
        // an `external` that isn't one would stop this project skipping at all.
        if there(&from_here) && leaves(&from_here) && !(as_written && outside(&from_here)) {
            targets.push(from_here);
        }
    }
    Ok(targets)
}

/// The strings in `text` that start with `../`, up to where a template fills something in.
fn quoted_parents(text: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    for (at, byte) in text.iter().enumerate() {
        if !matches!(byte, b'"' | b'\'' | b'`') {
            continue;
        }
        let rest = &text[at + 1..];
        let rest = rest.strip_prefix(b"./").unwrap_or(rest);
        if !rest.starts_with(b"../") {
            continue;
        }
        let end = rest
            .iter()
            .take(MAX_PATH_BYTES)
            .position(|next| next == byte || *next == b'\n');
        let Some(Ok(path)) = end.map(|end| std::str::from_utf8(&rest[..end])) else {
            continue;
        };
        found.extend(path_of(path));
    }
    found
}

/// The paths in `text` that start with `../` or `./../` without quotes, each line's code alone: a
/// shell's `cat ../lib/gen.txt`, a Compose file's `context: ../lib`, make's `-C ../lib`.
fn bare_parents(text: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.split(|byte| *byte == b'\n') {
        let line = code_of(line);
        for at in 0..line.len() {
            let rest = &line[at..];
            if !(rest.starts_with(b"../") || rest.starts_with(b"./../"))
                || !starts_a_path(&line[..at])
            {
                continue;
            }
            let rest = rest.strip_prefix(b"./").unwrap_or(rest);
            let end = rest.iter().position(|byte| ends_a_path(*byte));
            let end = end.unwrap_or(rest.len());
            if end > MAX_PATH_BYTES {
                continue;
            }
            let Ok(path) = std::str::from_utf8(&rest[..end]) else {
                continue;
            };
            // The dots that end a sentence aren't its own; `../..`'s are.
            let path = match path
                .rsplit('/')
                .next()
                .is_some_and(|last| last.bytes().all(|byte| byte == b'.'))
            {
                true => path,
                false => path.trim_end_matches('.'),
            };
            found.extend(path_of(path));
        }
    }
    found
}

/// What of `line` a program reads. A comment is prose, as a README is: a usage note's
/// `cd ../../cli` reads nothing, and would tie a project to a checkout for good. Only what is
/// surely a comment goes; what isn't sure stays, and costs a run at most.
fn code_of(line: &[u8]) -> &[u8] {
    let spaces = line.iter().take_while(|byte| byte.is_ascii_whitespace());
    let text = &line[spaces.count()..];
    let hashes = text.iter().take_while(|byte| **byte == b'#').count();
    let shell_comment = hashes > 0
        && text
            .get(hashes)
            .is_none_or(|byte| byte.is_ascii_whitespace());
    // `//go:` is a directive, and `#include` isn't a comment.
    let comment = shell_comment
        || (text.starts_with(b"//") && !text.starts_with(b"//go:"))
        || text.starts_with(b"/*")
        || text.starts_with(b"* ");
    if comment {
        return &[];
    }
    // `# ` and `// ` after code: `color: #fff` and `https://` aren't comments.
    let tail = (1..line.len()).find(|at| {
        line[at - 1].is_ascii_whitespace()
            && (line[*at..].starts_with(b"# ") || line[*at..].starts_with(b"// "))
    });
    &line[..tail.unwrap_or(line.len())]
}

/// Whether a path can start after `before`, the line up to it: what's before it isn't more of a
/// path. A flag holds its value (`-I../include`). A `/` can follow what stands for a directory:
/// a variable, a command or a string (`$DIR/../lib`, `$(dirname "$0")/../lib`, `dir + "/../lib"`).
fn starts_a_path(before: &[u8]) -> bool {
    match before {
        [] => true,
        [rest @ .., b'/'] => stands_for_a_directory(rest),
        [rest @ .., b'-', letter] if letter.is_ascii_alphabetic() => {
            rest.last().is_none_or(|byte| !path_byte(*byte))
        }
        [.., last] => !path_byte(*last),
    }
}

/// Whether `text` ends in a variable, a command or a string.
fn stands_for_a_directory(text: &[u8]) -> bool {
    if matches!(text.last(), Some(b')' | b'}' | b'"' | b'\'' | b'`')) {
        return true;
    }
    let word = text
        .iter()
        .rev()
        .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
        .count();
    word > 0 && text[..text.len() - word].last() == Some(&b'$')
}

/// Whether `byte` can be part of a path without quotes, so that a path can't start right after it.
fn path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || !byte.is_ascii()
        || matches!(byte, b'_' | b'.' | b'/' | b'~' | b'@' | b'%' | b'+' | b'-')
}

/// Whether `byte` ends a path without quotes: whitespace, or what a shell, YAML or a sentence puts
/// after one. What a template or a glob starts with doesn't: [`path_of`] needs to see it.
fn ends_a_path(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'"' | b'\''
                | b'`'
                | b';'
                | b'&'
                | b'|'
                | b'('
                | b')'
                | b'>'
                | b','
                | b'='
                | b':'
                | b']'
                | b'}'
                | b'!'
                | b'\\'
        )
}

/// The path `written` names, if it names one. A query or a fragment isn't part of it, and what a
/// template or a glob fills in leaves the directory before it.
fn path_of(written: &str) -> Option<String> {
    let path = written.trim_end_matches('\\');
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = match path.find(['$', '{', '*', '[', '<', '%']) {
        Some(at) => path[..at].rsplit_once('/').map_or("", |(dir, _)| dir),
        None => path,
    };
    (!path.is_empty()).then(|| path.to_string())
}

/// `path` without `.` and with `..` worked out as far as it goes: `a/../../b` is `../b`.
pub(super) fn normal(path: &str) -> Option<String> {
    if path.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The checkout `target`, a path outside the repository, is in: the top of its git repository
/// when it is here to look at, else the first directory past the root's parents.
fn checkout_of(root: &Path, target: &str) -> String {
    let climbs = target.split('/').take_while(|part| *part == "..").count();
    let parts: Vec<&str> = target.split('/').collect();
    for end in (climbs + 1..=parts.len()).rev() {
        let path = parts[..end].join("/");
        if root.join(&path).join(".git").exists() {
            return path;
        }
    }
    parts[..(climbs + 1).min(parts.len())].join("/")
}

/// The paths `dir`'s manifests name, relative to it. `own` says `dir` is the project itself, not
/// one it reaches.
fn dependencies(root: &Path, dir: &str, js: bool, own: bool) -> Result<Vec<String>, String> {
    let at = root.join(dir);
    for file in UNREAD {
        if at.join(file).exists() {
            return Err(format!("hushrun can't read what {dir}/{file} depends on"));
        }
    }
    let files = files_in(&at);
    if let Some(file) = files.iter().find(|file| {
        [".csproj", ".fsproj", ".sln"]
            .iter()
            .any(|end| file.ends_with(end))
    }) {
        return Err(format!("hushrun can't read what {dir}/{file} depends on"));
    }
    let read = |file: &str| std::fs::read_to_string(at.join(file)).ok();
    let inherits = |text: &str| text.replace([' ', '\t'], "").contains("workspace=true");
    let mut paths = Vec::new();
    if let Some(text) = read("Cargo.toml") {
        // \[ADR 0055\] What a crate inherits is read from its workspace's manifest.
        if inherits(&text) {
            paths.extend(cargo::inherited(root, dir, &text, own)?);
        }
        paths.extend(toml_paths(&text));
    }
    if let Some(text) = read("pyproject.toml") {
        if inherits(&text) {
            return Err(format!("{dir}/pyproject.toml inherits from a workspace"));
        }
        paths.extend(toml_paths(&text));
    }
    if let Some(text) = read("package.json") {
        paths.extend(package_paths(&text, js).map_err(|why| format!("{dir}/package.json {why}"))?);
    }
    if let Some(text) = read("go.mod") {
        paths.extend(go_paths(&text));
    }
    for file in files.iter().filter(|file| file.ends_with(".tf")) {
        if let Some(text) = read(file) {
            paths.extend(terraform_paths(&text));
        }
    }
    Ok(paths)
}

/// The values of `path = "…"` keys, which is how Cargo and Python's tools name a local dependency.
fn toml_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in text.lines() {
        let mut rest = line;
        while let Some(at) = rest.find("path") {
            let before = rest[..at].chars().next_back();
            let after = rest[at + 4..].trim_start();
            rest = &rest[at + 4..];
            let starts = before.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'));
            if let (true, Some(value)) = (starts, after.strip_prefix('=')) {
                let value = value.trim_start();
                if let Some(quoted) = value.strip_prefix('"')
                    && let Some(end) = quoted.find('"')
                {
                    paths.push(quoted[..end].to_string());
                }
            }
        }
    }
    paths
}

/// The local packages a package.json depends on. `workspace:` names no path: an error, unless
/// `js`, when the workspace's graph has placed it (ADR 0051).
fn package_paths(text: &str, js: bool) -> Result<Vec<String>, String> {
    let manifest: Value = serde_json::from_str(text).map_err(|_| "isn't JSON".to_string())?;
    let mut paths = Vec::new();
    for section in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        let Some(dependencies) = manifest[section].as_object() else {
            continue;
        };
        for value in dependencies.values().filter_map(Value::as_str) {
            if value.starts_with("workspace:") {
                if js {
                    continue;
                }
                return Err("depends on its workspace".to_string());
            }
            for scheme in ["file:", "link:", "portal:"] {
                if let Some(path) = value.strip_prefix(scheme) {
                    paths.push(path.to_string());
                }
            }
            // npm takes a bare path too.
            if value.starts_with("./") || value.starts_with("../") {
                paths.push(value.to_string());
            }
            if value.starts_with('/') || value.starts_with("~/") {
                return Err(format!("depends on {value}, which hushrun can't place"));
            }
        }
    }
    Ok(paths)
}

/// The directories a go.mod's `replace`s point at.
fn go_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.split_once("=>"))
        .map(|(_, target)| target.trim())
        .filter(|target| target.starts_with('.'))
        .filter_map(|target| target.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// The local modules Terraform files `source`.
fn terraform_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("source"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .filter_map(|value| value.trim().strip_prefix('"'))
        .filter_map(|value| value.split('"').next())
        .filter(|value| value.starts_with('.'))
        .map(str::to_string)
        .collect()
}

/// The directory a CI step's path and a `cd` end up in.
pub fn joined(dir: &str, moved: &str) -> String {
    [dir, moved]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    /// A repository of `files`, all committed.
    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        hushrun_testkit::git::init(dir.path(), "sha1");
        write(dir.path(), files);
        write(dir.path(), &[("first.txt", "")]);
        hushrun_testkit::git::commit_all(dir.path(), "init");
        dir
    }

    const PROSE: [&str; 3] = ["*.md", "LICENSE*", "NOTICE*"];

    fn dirs(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|dir| (*dir).to_string()).collect()
    }

    fn scopes(repo: &tempfile::TempDir, list: &[&str]) -> Vec<Project> {
        match layout(repo.path(), &dirs(list), None) {
            Layout::Projects(projects) => projects,
            other => panic!("{other:?}"),
        }
    }

    fn one(repo: &tempfile::TempDir, list: &[&str]) -> Option<String> {
        match layout(repo.path(), &dirs(list), None) {
            Layout::One(why) => why,
            other => panic!("{other:?}"),
        }
    }

    // covers: C-15
    #[test]
    fn a_repository_of_projects_gets_a_scope_each_with_what_ci_and_tools_read() {
        let r = repo(&[
            (
                "back/Cargo.toml",
                "[package]\nname = \"back\"\n\n[[bin]]\npath = \"src/main.rs\"\n\n[dependencies]\ncore = { path = \"../../cli/crates/core\", features = [\"x\"] }\nserde = \"1\"\n",
            ),
            ("ui/package.json", "{\"dependencies\": {\"next\": \"16\"}}"),
            (
                "deploy/gcp/main.tf",
                "module \"a\" {\n  source = \"./modules/a\"\n}\n",
            ),
            (".github/workflows/ci.yml", "on: push\n"),
            ("rust-toolchain.toml", "[toolchain]\n"),
        ]);
        std::fs::create_dir_all(r.path().join("deploy/gcp/modules/a")).unwrap();
        let found = scopes(&r, &["back", "ui", "deploy/gcp"]);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["back", "gcp", "ui"]);
        // The root's files come with `*`, what CI runs by name, and the root's prose stays out.
        assert_eq!(found[0].include, ["back/**", "*", ".github/workflows/**"]);
        assert_eq!(found[0].exclude, PROSE);
        // Not here to look at: the first directory past the root.
        assert_eq!(found[0].external, ["../cli"]);
        assert!(found[2].external.is_empty());
    }

    // covers: C-15
    #[test]
    fn a_project_reaches_what_its_manifest_names() {
        let r = repo(&[
            (
                "api/Cargo.toml",
                "[dependencies]\nlib = { path = \"../lib\" }\n",
            ),
            (
                "lib/Cargo.toml",
                "[dependencies]\nbase = { path = \"../base\" }\n",
            ),
            ("base/Cargo.toml", "[package]\n"),
            (
                "web/package.json",
                "{\"dependencies\": {\"ui\": \"file:../ui\"}}",
            ),
            ("ui/package.json", "{}"),
            ("svc/go.mod", "module svc\n\nreplace acme/lib => ../golib\n"),
            ("golib/go.mod", "module lib\n"),
            (
                "py/pyproject.toml",
                "[tool.uv.sources]\nshared = { path = \"../pyshared\" }\n",
            ),
            ("pyshared/pyproject.toml", "[project]\n"),
        ]);
        let found = scopes(&r, &["api", "web", "svc", "py"]);
        let of = |name: &str| found.iter().find(|p| p.name == name).unwrap();
        assert_eq!(of("api").include, ["api/**", "base/**", "lib/**", "*"]);
        assert_eq!(of("web").include, ["web/**", "ui/**", "*"]);
        assert_eq!(of("svc").include, ["svc/**", "golib/**", "*"]);
        assert_eq!(of("py").include, ["py/**", "pyshared/**", "*"]);
    }

    // covers: C-15
    #[test]
    fn a_checkout_beside_the_repository_is_found_by_its_top() {
        let top = tempfile::tempdir().unwrap();
        let root = top.path().join("cloud");
        std::fs::create_dir_all(top.path().join("cli/.git")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        hushrun_testkit::git::init(&root, "sha1");
        write(
            &root,
            &[(
                "back/Cargo.toml",
                "[dependencies]\ncore = { path = \"../../cli/crates/core\" }\n",
            )],
        );
        hushrun_testkit::git::commit_all(&root, "init");
        let Layout::Projects(found) = layout(&root, &dirs(&["back"]), None) else {
            panic!();
        };
        assert_eq!(found[0].external, ["../cli"]);
    }

    // covers: C-15
    /// \[ADR 0055\] hushrun's own repository: back depends on crates of cli's workspace by path,
    /// which inherit from it. back builds with cli's manifest and the crates those inherit, never
    /// their dev-dependencies; a member of the workspace builds as all of it.
    #[test]
    fn a_crate_that_inherits_from_its_workspace_brings_what_it_builds_with() {
        let r = repo(&[
            (
                "back/Cargo.toml",
                "[package]\nname = \"back\"\n\n[dependencies]\ncore = { path = \"../cli/crates/core\" }\nlog = { path = \"../cli/crates/log\" }\n",
            ),
            (
                "cli/Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nedition = \"2024\"\n\n[workspace.dependencies]\ncore = { path = \"crates/core\" }\ndigest = { path = \"crates/digest\" }\nkit = { path = \"crates/kit\" }\nlog = { path = \"crates/log\" }\nserde = \"1\"\n",
            ),
            (
                "cli/crates/core/Cargo.toml",
                "[package]\nname = \"core\"\nedition.workspace = true\n\n[lints]\nworkspace = true\n\n[dependencies]\nserde.workspace = true\n",
            ),
            (
                "cli/crates/log/Cargo.toml",
                "[package]\nname = \"log\"\nedition.workspace = true\n\n[dependencies]\ncore = { workspace = true }\n\n[dev-dependencies]\nkit.workspace = true\n",
            ),
            (
                "cli/crates/kit/Cargo.toml",
                "[package]\nname = \"kit\"\n\n[dependencies]\ndigest.workspace = true\n",
            ),
            (
                "cli/crates/digest/Cargo.toml",
                "[package]\nname = \"digest\"\n",
            ),
            (
                "cli/crates/tool/Cargo.toml",
                "[package]\nname = \"tool\"\n\n[dependencies]\nserde = \"1\"\n",
            ),
        ]);
        let found = scopes(&r, &["back", "cli", "cli/crates/tool"]);
        let of = |name: &str| found.iter().find(|p| p.name == name).unwrap();
        assert_eq!(
            of("back").include,
            [
                "back/**",
                "cli/crates/core/**",
                "cli/crates/log/**",
                "cli/Cargo.toml",
                "*"
            ]
        );
        assert_eq!(of("cli").include, ["cli/**", "*"]);
        // A member reads its workspace's lockfile and manifests, whatever it inherits.
        assert_eq!(of("tool").include, ["cli/crates/tool/**", "cli/**", "*"]);
    }

    // covers: C-15 (review case A)
    #[test]
    fn a_python_workspace_keeps_one_scope() {
        let r = repo(&[
            (
                "pyproject.toml",
                "[tool.uv.workspace]\nmembers = [\"a\", \"b\"]\n",
            ),
            (
                "a/pyproject.toml",
                "[tool.uv.sources]\nb = { workspace = true }\n",
            ),
            ("b/pyproject.toml", "[project]\n"),
        ]);
        assert_eq!(
            one(&r, &["a", "b"]),
            Some("pyproject.toml ties its projects together".to_string())
        );
        // A member's own word is enough, however it's spaced.
        for line in ["b = { workspace = true }", "b = {workspace=true}"] {
            let r = repo(&[
                ("a/pyproject.toml", &format!("[tool.uv.sources]\n{line}\n")),
                ("b/pyproject.toml", "[project]\n"),
            ]);
            assert_eq!(
                one(&r, &["a", "b"]),
                Some("a/pyproject.toml inherits from a workspace".to_string())
            );
        }
    }

    // covers: C-15 (review case B)
    #[test]
    fn a_bare_relative_dependency_of_npms_joins_the_scope() {
        let r = repo(&[
            (
                "web/package.json",
                "{\"dependencies\": {\"lib\": \"../lib\", \"own\": \"./own\"}}",
            ),
            ("web/own/package.json", "{}"),
            ("lib/package.json", "{}"),
        ]);
        let found = scopes(&r, &["web"]);
        assert_eq!(found[0].include, ["web/**", "lib/**", "*"]);
        for path in ["/opt/lib", "~/lib"] {
            let manifest = format!("{{\"dependencies\": {{\"lib\": \"{path}\"}}}}");
            let r = repo(&[("web/package.json", &manifest)]);
            assert!(one(&r, &["web"]).unwrap().contains(path));
        }
    }

    // covers: C-15 (review case C)
    #[test]
    fn a_config_a_project_extends_is_in_its_scope() {
        let r = repo(&[
            ("web/package.json", "{}"),
            (
                "web/tsconfig.json",
                "{\n  \"extends\": \"../tsconfig.base.json\",\n  \"references\": [{ \"path\": \"../shared\" }]\n}\n",
            ),
            (
                "web/.eslintrc.json",
                "{\"extends\": \"../config/eslint.json\"}",
            ),
            ("tsconfig.base.json", "{}"),
            ("config/eslint.json", "{}"),
            ("config/other.json", "{}"),
            ("shared/index.ts", ""),
        ]);
        let found = scopes(&r, &["web"]);
        assert_eq!(
            found[0].include,
            ["web/**", "shared/**", "config/eslint.json", "*"]
        );
        // The root's file comes with `*`, which matches it and nothing below the root.
        for (pattern, path, member) in [
            ("*", "tsconfig.base.json", true),
            ("*", ".prettierrc", true),
            ("*", "Directory.Build.props", true),
            ("*", "config/other.json", false),
        ] {
            assert_eq!(
                Pattern::new(pattern).unwrap().is_match(path),
                member,
                "{path}"
            );
        }
    }

    // covers: C-15 (review case D)
    #[test]
    fn a_file_a_test_opens_joins_the_scope_as_that_file() {
        let r = repo(&[
            ("back/Cargo.toml", "[package]\n"),
            (
                "back/tests/limits.rs",
                "let site = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../site/data/prices.json\");\n",
            ),
            ("site/package.json", "{}"),
            ("site/data/prices.json", "{}"),
            ("site/data/site.ts", ""),
        ]);
        let found = scopes(&r, &["back", "site"]);
        let back = found.iter().find(|p| p.name == "back").unwrap();
        // The file, not the directory it is in: a change to the rest of site runs nothing here.
        assert_eq!(back.include, ["back/**", "site/data/prices.json", "*"]);
    }

    // covers: C-15
    #[test]
    fn a_project_outside_its_own_directory_is_seen_however_its_files_say_so() {
        // An import names a file without its extension, and leaves nothing out.
        let r = repo(&[
            ("web/package.json", "{}"),
            ("web/src/a.ts", "import { x } from \"../../shared/foo\";\n"),
            ("shared/foo.ts", ""),
            ("shared/foo.test.ts", ""),
            ("shared/foot.ts", ""),
            ("shared/foo/deep.ts", ""),
        ]);
        let found = scopes(&r, &["web"]);
        assert_eq!(
            found[0].include,
            [
                "web/**",
                "shared/foo/**",
                "shared/foo.test.ts",
                "shared/foo.ts",
                "*"
            ]
        );
        // What a template fills in leaves the directory before it; so does a glob.
        let r = repo(&[
            ("web/package.json", "{}"),
            (
                "web/a.ts",
                "read(`../data/${name}.json`);\nread(\"../more/*.json\");\n",
            ),
            ("data/a.json", ""),
            ("more/a.json", ""),
        ]);
        assert_eq!(
            scopes(&r, &["web"])[0].include,
            ["web/**", "data/**", "more/**", "*"]
        );
        // What it reaches, it reaches in turn.
        let r = repo(&[
            ("web/package.json", "{}"),
            ("web/a.ts", "read('../one/a.json');\n"),
            ("one/a.json", "{\"next\": \"../two/b.json\"}"),
            ("two/b.json", ""),
        ]);
        assert_eq!(
            scopes(&r, &["web"])[0].include,
            ["web/**", "one/a.json", "two/b.json", "*"]
        );
    }

    // covers: C-15
    #[test]
    fn a_file_at_the_root_a_project_names_stays_in_its_scope() {
        // `*.md` leaves out the root's prose, unless a project reads it.
        let r = repo(&[
            ("back/Cargo.toml", "[package]\n"),
            (
                "back/src/lib.rs",
                "#![doc = include_str!(\"../README.md\")]\n",
            ),
            ("README.md", ""),
            ("ui/package.json", "{}"),
        ]);
        let found = scopes(&r, &["back", "ui"]);
        assert_eq!(found[0].exclude, ["LICENSE*", "NOTICE*"]);
        assert_eq!(found[1].exclude, PROSE);
    }

    // covers: C-15
    #[test]
    fn what_only_looks_like_a_path_out_stays_out() {
        // An import inside the project, a path to nothing, a build's output, a README's prose.
        let r = repo(&[
            (".gitignore", "target/\n"),
            ("web/package.json", "{}"),
            (
                "web/src/lib/x.ts",
                "import \"../site\";\nimport \"../..\";\nread(\"../nothing/here\");\n",
            ),
            ("web/src/site.ts", ""),
            ("web/README.md", "See `../api`, and \"../api\".\n"),
            ("api/go.mod", "module api\n"),
        ]);
        write(r.path(), &[("web/target/gen.ts", "read(\"../../api\");\n")]);
        let found = scopes(&r, &["web", "api"]);
        let web = found.iter().find(|p| p.name == "web").unwrap();
        assert_eq!(web.include, ["web/**", "*"]);
    }

    // covers: C-15
    #[test]
    fn a_path_that_works_from_its_file_isnt_a_checkout_beside_the_repository() {
        // As a web app's tests import `../../lib/site`: from the project it would leave the
        // repository, and something of that name is there. Taken for an `external`, it would stop
        // the project skipping for good.
        let top = tempfile::tempdir().unwrap();
        let root = top.path().join("cloud");
        std::fs::create_dir_all(top.path().join("lib/site")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        hushrun_testkit::git::init(&root, "sha1");
        write(
            &root,
            &[
                ("web/package.json", "{}"),
                ("web/lib/site.ts", ""),
                ("web/tests/e2e/a.ts", "import \"../../lib/site\";\n"),
            ],
        );
        hushrun_testkit::git::commit_all(&root, "init");
        let Layout::Projects(found) = layout(&root, &dirs(&["web"]), None) else {
            panic!();
        };
        assert!(found[0].external.is_empty(), "{:?}", found[0].external);
    }

    // covers: C-15
    #[test]
    fn a_project_that_names_the_whole_repository_keeps_one_scope() {
        let r = repo(&[("web/package.json", "{}"), ("web/a.ts", "read(\"../\");\n")]);
        assert!(
            one(&r, &["web"]).unwrap().contains("the whole repository"),
            "{r:?}"
        );
        // A template that names nothing before it says the same.
        let r = repo(&[
            ("web/package.json", "{}"),
            ("web/a.ts", "read(`../${x}/a`);\n"),
        ]);
        assert!(one(&r, &["web"]).unwrap().contains("the whole repository"));
    }

    // covers: C-15 (review: a link out of a project)
    #[cfg(unix)]
    #[test]
    fn a_tracked_link_names_where_it_leads() {
        use std::os::unix::fs::symlink;

        let linked = |link: &str, target: &str, more: &[(&str, &str)]| {
            let dir = tempfile::tempdir().unwrap();
            hushrun_testkit::git::init(dir.path(), "sha1");
            write(dir.path(), &[("web/package.json", "{}")]);
            write(dir.path(), more);
            symlink(target, dir.path().join(link)).unwrap();
            hushrun_testkit::git::commit_all(dir.path(), "init");
            dir
        };
        // A directory: the digest holds the link's text, so this is the whole of what it reads.
        let r = linked("web/shared", "../lib/src", &[("lib/src/a.ts", "")]);
        assert_eq!(
            scopes(&r, &["web"])[0].include,
            ["web/**", "lib/src/**", "*"]
        );
        // A file, whatever it is called, and a link it leads to, in turn.
        let r = linked("web/guide.md", "../docs/guide.md", &[("docs/guide.md", "")]);
        assert_eq!(
            scopes(&r, &["web"])[0].include,
            ["web/**", "docs/guide.md", "*"]
        );
        let r = linked("web/a", "../one", &[("one", "")]);
        std::fs::remove_file(r.path().join("one")).unwrap();
        symlink("two/b.txt", r.path().join("one")).unwrap();
        write(r.path(), &[("two/b.txt", "")]);
        hushrun_testkit::git::commit_all(r.path(), "a link to a link");
        assert_eq!(
            scopes(&r, &["web"])[0].include,
            ["web/**", "two/b.txt", "*"]
        );
        // A path through a link is a path to where it leads. The link at the root comes with `*`.
        let r = linked(
            "root",
            "lib/src",
            &[
                ("lib/src/a.ts", ""),
                ("web/b.ts", "read(\"../root/a.ts\");\n"),
            ],
        );
        assert_eq!(
            scopes(&r, &["web"])[0].include,
            ["web/**", "lib/src/**", "*"]
        );
        // In its own project, it leads nowhere new, even to what git doesn't track.
        let r = linked("web/latest", "./dist/v1", &[]);
        assert_eq!(scopes(&r, &["web"])[0].include, ["web/**", "*"]);
        // Out of the repository, it's what's beside it.
        let r = linked("web/core", "../../cli/src", &[]);
        assert_eq!(scopes(&r, &["web"])[0].external, ["../cli"]);
        // What can't be placed keeps one scope, and says why.
        for (target, why) in [
            ("/opt/lib", "web/shared links to /opt/lib"),
            ("../gone", "web/shared links to ../gone"),
            ("..", "the repository's root"),
            ("../..", "above the repository"),
        ] {
            let r = linked("web/shared", target, &[]);
            let found = one(&r, &["web"]).unwrap();
            assert!(found.contains(why), "{target}: {found}");
        }
    }

    // covers: C-15 (review: paths without quotes)
    #[test]
    fn a_path_without_quotes_counts() {
        for (file, text) in [
            ("web/prep.sh", "cat ../lib/gen.txt > out.txt\n"),
            (
                "web/compose.yml",
                "services:\n  a:\n    build:\n      context: ../lib\n",
            ),
            ("web/Makefile", "all:\n\t$(MAKE) -C ../lib build\n"),
            ("web/x.sh", "./../lib/gen.txt\n"),
            ("web/x.sh", "cd ../lib && make\n"),
            ("web/x.sh", "PATHS=../other:../lib/gen.txt\n"),
            ("web/x.sh", "tool --dir=../lib\n"),
            ("web/x.sh", "source $(dirname \"$0\")/../lib/gen.txt\n"),
            ("web/x.sh", "source $DIR/../lib/gen.txt\n"),
            ("web/x.js", "read(__dirname + '/../lib/gen.txt');\n"),
            ("web/Makefile", "CFLAGS += -I../lib\n"),
            (
                "web/a.css",
                "a { color: #fff; background: url(../lib/gen.txt); }\n",
            ),
            ("web/Makefile", "include ../lib/%.mk\n"),
            ("web/x.sh", "echo see ../lib.\n"),
            ("web/x.sh", "cp ../lib/$NAME/gen.txt .\n"),
            ("web/x.sh", "cat ../lib/gen.txt # and that's all\n"),
        ] {
            let r = repo(&[
                ("web/package.json", "{}"),
                (file, text),
                ("lib/gen.txt", ""),
            ]);
            let include = scopes(&r, &["web"]).remove(0).include;
            assert!(
                include == ["web/**", "lib/gen.txt", "*"] || include == ["web/**", "lib/**", "*"],
                "{text}: {include:?}"
            );
        }
        // A file at the root, reached by name, reads from the root.
        let top = tempfile::tempdir().unwrap();
        let root = top.path().join("shop");
        std::fs::create_dir_all(top.path().join("sibling/.git")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        hushrun_testkit::git::init(&root, "sha1");
        write(
            &root,
            &[
                ("web/package.json", "{}"),
                ("web/x.sh", "cat ../setup.sh\n"),
                ("setup.sh", "cat ../sibling/.git\n"),
            ],
        );
        hushrun_testkit::git::commit_all(&root, "init");
        let Layout::Projects(found) = layout(&root, &dirs(&["web"]), None) else {
            panic!();
        };
        assert_eq!(found[0].external, ["../sibling"]);
    }

    // covers: C-15
    #[test]
    fn a_path_in_a_comment_or_part_of_another_stays_out() {
        // As a README's usage notes, which would otherwise tie a project to `../api`.
        for text in [
            "#   (cd ../api && cargo build)\n",
            "## ../api\n",
            "#\t../api\n",
            "// ../api\n",
            "/// run `x > ../api/go.mod`\n",
            "/* ../api */\n",
            " * ../api\n",
            "    # ../api\n",
            "code # ../api\n",
            "code // ../api\n",
            // Part of another path, which is relative to what hushrun can't tell.
            "vendor/../api\n",
            "see foo../api\n",
            "http://example.com/../api\n",
            "x-y-I../api\n",
            // A regular expression that says `../`, and a word that ends in it.
            "\\.\\./api\n",
            "wait.../api\n",
        ] {
            let r = repo(&[
                ("web/package.json", "{}"),
                ("web/notes.sh", text),
                ("api/go.mod", "module api\n"),
            ]);
            let found = scopes(&r, &["web", "api"]);
            let web = found.iter().find(|p| p.name == "web").unwrap();
            assert_eq!(web.include, ["web/**", "*"], "{text}");
        }
        // What isn't surely a comment counts: a wrong hit costs a run.
        for text in [
            "#include <../api/go.mod>\n",
            "//go:generate sh ../api/go.mod\n",
        ] {
            let r = repo(&[
                ("web/package.json", "{}"),
                ("web/a.h", text),
                ("api/go.mod", "module api\n"),
            ]);
            let include = scopes(&r, &["web"]).remove(0).include;
            assert_eq!(include, ["web/**", "api/go.mod", "*"], "{text}");
        }
        // A README is prose, quoted or not.
        let r = repo(&[
            ("web/package.json", "{}"),
            ("web/README.md", "cat ../api/go.mod\n"),
            ("api/go.mod", "module api\n"),
        ]);
        assert_eq!(scopes(&r, &["web"])[0].include, ["web/**", "*"]);
    }

    // covers: C-15
    #[test]
    fn a_big_file_says_nothing_and_a_big_project_keeps_one_scope() {
        let big = format!("{}\"../api\"\n", " ".repeat(MAX_FILE_BYTES as usize));
        let r = repo(&[
            ("web/package.json", "{}"),
            ("web/data.js", &big),
            ("web/blob.bin", "\0\"../api\"\n"),
            ("api/go.mod", "module api\n"),
        ]);
        assert_eq!(scopes(&r, &["web"])[0].include, ["web/**", "*"]);

        let tracked = tracked(r.path()).unwrap();
        let mut budget = Budget {
            files: MAX_PROJECT_FILES,
            bytes: 0,
        };
        let too_big = named_in(
            r.path(),
            "web/package.json",
            "web",
            &tracked,
            &mut budget,
            "web",
        );
        assert!(too_big.unwrap_err().contains("too big"));
    }

    // covers: C-15
    #[test]
    fn a_pattern_never_says_less_than_its_path() {
        assert_eq!(file_pattern("a/b.json").unwrap(), "a/b.json");
        assert_eq!(file_pattern("app/[slug]/x.json").unwrap(), "app/**");
        assert_eq!(directory_pattern("a/b").unwrap(), "a/b/**");
        assert_eq!(directory_pattern("a/{b}/c").unwrap(), "a/**");
        assert!(file_pattern("[x]/a").is_err());
    }

    // covers: C-15
    #[test]
    fn whatever_hushrun_cant_read_keeps_one_scope() {
        // Every check at the root: nothing to divide.
        let r = repo(&[("Cargo.toml", "[package]\n")]);
        assert_eq!(one(&r, &[""]), None);
        // A workspace: a change in one project can be a change in another.
        for (file, text) in [
            ("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n"),
            ("pnpm-workspace.yaml", "packages: [a]\n"),
            ("go.work", "go 1.24\n"),
            ("package.json", "{\"workspaces\": [\"a\"]}"),
        ] {
            let r = repo(&[(file, text), ("a/Cargo.toml", "[package]\n")]);
            assert_eq!(
                one(&r, &["a"]),
                Some(format!("{file} ties its projects together"))
            );
        }
        // A manifest hushrun can't read, or none.
        for file in [
            "pom.xml",
            "build.gradle.kts",
            "Gemfile",
            "composer.json",
            "App.csproj",
        ] {
            let r = repo(&[(&format!("a/{file}"), "")]);
            let why = one(&r, &["a"]).unwrap();
            assert!(why.contains(file), "{why}");
        }
        let r = repo(&[("a/x.txt", "")]);
        assert!(one(&r, &["a"]).unwrap().contains("no manifest"));
        // A dependency that names no path.
        let r = repo(&[(
            "a/package.json",
            "{\"dependencies\": {\"b\": \"workspace:*\"}}",
        )]);
        assert!(
            one(&r, &["a"])
                .unwrap()
                .contains("depends on its workspace")
        );
        let r = repo(&[("a/Cargo.toml", "[dependencies]\nb.workspace = true\n")]);
        assert!(
            one(&r, &["a"])
                .unwrap()
                .contains("inherits from a workspace")
        );
    }

    // covers: C-15
    #[test]
    fn a_directory_inside_a_project_is_that_projects() {
        let r = repo(&[("web/package.json", "{}"), ("web/e2e/x.txt", "")]);
        let found = scopes(&r, &["web/e2e"]);
        assert_eq!(found[0].dir, "web");
        let layout = Layout::Projects(found);
        assert_eq!(layout.scope_of("web/e2e").unwrap().as_deref(), Some("web"));
    }

    // covers: C-15
    #[test]
    fn checks_at_the_root_of_a_divided_repository_read_all_of_it() {
        let r = repo(&[("a/go.mod", "module a\n")]);
        let found = scopes(&r, &["", "a"]);
        assert_eq!(found[1].name, "all");
        assert_eq!(found[1].include, ["**"]);
        assert!(found[1].exclude.is_empty());
        let layout = Layout::Projects(found);
        assert_eq!(layout.scope_of("").unwrap().as_deref(), Some("all"));
        assert_eq!(layout.scope_of("a").unwrap().as_deref(), Some("a"));
    }

    // covers: C-15
    #[test]
    fn two_projects_of_one_name_go_by_their_paths() {
        let r = repo(&[
            ("x/api/go.mod", "module a\n"),
            ("y/api/go.mod", "module b\n"),
        ]);
        let found = scopes(&r, &["x/api", "y/api"]);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["x-api", "y-api"]);
    }

    // covers: C-15, C-25
    #[test]
    fn a_directory_two_scopes_hold_is_in_neither() {
        let text = "version: 1\nrepository: github.com/a/b\nscopes:\n  ios:\n    include: [\"ios/**\", \"shared/**\"]\n  api:\n    include: [\"api/**\", \"shared/**\"]\n";
        let policy = Policy::parse("hushrun.yaml", text.as_bytes()).unwrap();
        assert_eq!(the_scope_of(&policy, "ios").as_deref(), Some("ios"));
        assert_eq!(the_scope_of(&policy, "ios/sub").as_deref(), Some("ios"));
        assert_eq!(the_scope_of(&policy, "shared"), None);
        assert_eq!(the_scope_of(&policy, "docs"), None);
    }

    // covers: C-15
    #[test]
    fn scopes_hushrun_yaml_has_are_kept() {
        let policy = Policy::parse(
            "hushrun.yaml",
            b"version: 1\nrepository: github.com/acme/shop\nscopes:\n  api:\n    include: [\"api/**\"]\n  web:\n    include: [\"web/**\"]\n",
        )
        .unwrap();
        let r = repo(&[]);
        let found = layout(r.path(), &dirs(&["api", "other"]), Some(&policy));
        assert_eq!(found.scope_of("api").unwrap().as_deref(), Some("api"));
        assert!(found.scope_of("other").is_err());
        // A deploy's events name `default`: scopes aren't init's to add.
        let deploys = Policy::parse(
            "hushrun.yaml",
            b"version: 1\nrepository: github.com/acme/shop\nchecks:\n  unit: {run: \"true\"}\ndeploy:\n  require: [unit]\n",
        )
        .unwrap();
        let r = repo(&[("a/go.mod", "module a\n")]);
        let found = layout(r.path(), &dirs(&["a"]), Some(&deploys));
        assert!(matches!(found, Layout::One(Some(why)) if why.contains("deploys")));
    }

    #[test]
    fn paths_work_out() {
        assert_eq!(normal("back/../../cli/x").as_deref(), Some("../cli/x"));
        assert_eq!(normal("a/./b/").as_deref(), Some("a/b"));
        assert_eq!(normal("a/../b").as_deref(), Some("b"));
        assert_eq!(normal("/abs"), None);
        assert_eq!(
            toml_paths("path = \"a\"\nx = { version = \"1\", path=\"b\" }\nmypath = \"c\"\n"),
            ["a", "b"]
        );
    }
}
