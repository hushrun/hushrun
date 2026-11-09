//! \[ADR 0051\] A JavaScript workspace: its packages, what each one depends on among the others,
//! and which of their scripts a command runs (C-15).
//!
//! A package is a scope of its own, so that a change runs only the checks of the packages that can
//! reach it. Its scope holds its directory and those of the workspace packages it depends on, as
//! its package.json, a `turbo.json` or a tsconfig names them; `projects.rs` adds the root's files
//! and what the package's files name by relative path. What hushrun can't read keeps one scope for
//! all, which costs a run and never a wrong skip.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use hushrun_core::glob::Pattern;

/// The fields of a package.json that name what it depends on.
pub const DEPENDENCIES: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

/// What pnpm leaves out of a workspace, whatever its patterns say.
const PNPM_IGNORES: &[&str] = &[
    "**/node_modules/**",
    "**/bower_components/**",
    "**/test/**",
    "**/tests/**",
];

/// A workspace's packages, and how they depend on each other.
#[derive(Debug, Clone, Default)]
pub struct Workspace {
    /// The file that lists its packages: `pnpm-workspace.yaml`, `package.json` or `lerna.json`.
    pub file: String,
    /// Its packages, the root apart, by directory.
    pub members: Vec<Member>,
    /// Why hushrun can't tell what each package depends on, when it can't: one scope then holds
    /// them all.
    pub unreadable: Option<String>,
    /// What every package's checks read beside the root's files, from the root: `turbo.json`'s
    /// `globalDependencies` and `$TURBO_ROOT$` inputs, and where the root's tsconfig maps imports.
    pub everywhere: BTreeSet<String>,
}

/// A package of a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Its directory, relative to the root.
    pub dir: String,
    /// Its package.json's `name`.
    pub name: Option<String>,
    /// Its package.json, as read.
    pub text: String,
    /// The packages of the workspace it depends on itself, by directory.
    pub needs: BTreeSet<String>,
}

/// What a command runs of the workspace's scripts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runs {
    /// The scripts it runs in each package, in order.
    pub scripts: Vec<String>,
    /// The packages it runs them in, by directory.
    pub packages: BTreeSet<String>,
    /// It runs the root's script of each name too.
    pub root: bool,
    /// It runs them through Turborepo, whose strict mode gives each only the variables
    /// turbo.json passes on.
    pub turbo: bool,
}

/// The workspace at `root`, when its package.json heads one.
pub fn read(root: &Path) -> Option<Workspace> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let manifest: Value = serde_json::from_str(&text).ok()?;
    let (file, patterns) = match listed(root, &manifest) {
        Listed::None => return None,
        Listed::Unreadable(file, why) => {
            return Some(Workspace {
                file: file.to_string(),
                unreadable: Some(why),
                ..Workspace::default()
            });
        }
        Listed::Patterns(file, patterns) => (file, patterns),
    };
    let mut workspace = Workspace {
        file: file.to_string(),
        ..Workspace::default()
    };
    let tracked = match super::projects::tracked(root) {
        Ok(tracked) => tracked,
        Err(why) => {
            workspace.unreadable = Some(why);
            return Some(workspace);
        }
    };
    match members(root, file, &patterns, &tracked) {
        Ok(found) => workspace.members = found,
        Err(why) => {
            workspace.unreadable = Some(why);
            return Some(workspace);
        }
    }
    if let Err(why) = workspace.link(root, &tracked) {
        workspace.unreadable = Some(why);
    }
    Some(workspace)
}

/// Where the root's package.json and its neighbours list the packages.
enum Listed {
    None,
    Unreadable(&'static str, String),
    Patterns(&'static str, Vec<String>),
}

fn listed(root: &Path, manifest: &Value) -> Listed {
    const PNPM: &str = "pnpm-workspace.yaml";
    if let Ok(bytes) = std::fs::read(root.join(PNPM)) {
        #[derive(Deserialize)]
        struct Pnpm {
            packages: Option<Vec<String>>,
        }
        // pnpm's own settings, with no `packages`, make no workspace of one package.
        return match hushrun_core::yaml::parse::<Option<Pnpm>>(PNPM, &bytes) {
            Ok(Some(Pnpm {
                packages: Some(packages),
            })) => Listed::Patterns(PNPM, packages),
            Ok(_) => Listed::None,
            Err(_) => Listed::Unreadable(
                PNPM,
                format!("{PNPM} doesn't parse, so hushrun can't tell its packages"),
            ),
        };
    }
    const NPM: &str = "package.json";
    let unreadable = || {
        Listed::Unreadable(
            NPM,
            "package.json's workspaces aren't a list of patterns, so hushrun can't tell its packages"
                .to_string(),
        )
    };
    match manifest.get("workspaces") {
        Some(Value::Array(items)) => {
            return strings(items).map_or_else(unreadable, |found| Listed::Patterns(NPM, found));
        }
        Some(Value::Object(map)) => {
            return match map.get("packages") {
                Some(Value::Array(items)) => {
                    strings(items).map_or_else(unreadable, |found| Listed::Patterns(NPM, found))
                }
                None => Listed::None,
                Some(_) => unreadable(),
            };
        }
        Some(_) => return unreadable(),
        None => {}
    }
    const LERNA: &str = "lerna.json";
    if let Ok(text) = std::fs::read_to_string(root.join(LERNA)) {
        let cant = || {
            Listed::Unreadable(
                LERNA,
                format!("{LERNA} doesn't parse, so hushrun can't tell its packages"),
            )
        };
        return match jsonc(&text) {
            // Lerna's own default.
            Some(value) => match value.get("packages") {
                None => Listed::Patterns(LERNA, vec!["packages/*".to_string()]),
                Some(Value::Array(items)) => {
                    strings(items).map_or_else(cant, |found| Listed::Patterns(LERNA, found))
                }
                Some(_) => cant(),
            },
            None => cant(),
        };
    }
    Listed::None
}

fn strings(items: &[Value]) -> Option<Vec<String>> {
    items
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect()
}

/// The packages `patterns` match among the tracked files, each with its package.json.
fn members(
    root: &Path,
    file: &str,
    patterns: &[String],
    tracked: &BTreeSet<String>,
) -> Result<Vec<Member>, String> {
    let cant = |written: &str| format!("{file} names {written}, a pattern hushrun can't read");
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    for written in patterns {
        let (negated, rest) = match written.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, written.as_str()),
        };
        for expanded in braces(rest).ok_or_else(|| cant(written))? {
            // `.` is the root, which is no package of its own here.
            let Some(pattern) = normalized(&expanded) else {
                continue;
            };
            if pattern.split('/').any(|part| part == "..") {
                return Err(format!(
                    "{file} names {written}, outside the repository, which hushrun can't place"
                ));
            }
            let compiled =
                Pattern::new(&format!("{pattern}/package.json")).map_err(|_| cant(written))?;
            match negated {
                true => exclude.push(compiled),
                false => include.push(compiled),
            }
        }
    }
    let ignores: &[&str] = match file {
        "pnpm-workspace.yaml" => PNPM_IGNORES,
        _ => &PNPM_IGNORES[..1],
    };
    for ignore in ignores {
        exclude.push(Pattern::new(ignore).expect("a valid pattern"));
    }
    let mut found = Vec::new();
    for path in tracked
        .iter()
        .filter(|path| path.ends_with("/package.json"))
    {
        if !include.iter().any(|pattern| pattern.is_match(path))
            || exclude.iter().any(|pattern| pattern.is_match(path))
        {
            continue;
        }
        let dir = path.trim_end_matches("/package.json").to_string();
        // One git tracks that the worktree lacks is gone, or not checked out: no package here.
        let Some(text) = read_tracked(root, path)? else {
            continue;
        };
        found.push(Member {
            dir,
            name: None,
            text,
            needs: BTreeSet::new(),
        });
    }
    Ok(found)
}

/// A tracked file's text: None when the worktree doesn't have it, an error when it can't be read.
fn read_tracked(root: &Path, path: &str) -> Result<Option<String>, String> {
    match std::fs::read_to_string(root.join(path)) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(format!("hushrun can't read {path}")),
    }
}

/// A workspace pattern as a directory pattern: no `./`, no trailing `/` or `/package.json`. None
/// for the root.
fn normalized(pattern: &str) -> Option<String> {
    let mut rest = pattern.trim();
    while let Some(after) = rest.strip_prefix("./") {
        rest = after;
    }
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix("/package.json").unwrap_or(rest);
    (!matches!(rest, "" | ".")).then(|| rest.to_string())
}

/// `pattern` with each `{a,b}` written out; None for braces in braces, or one left open.
fn braces(pattern: &str) -> Option<Vec<String>> {
    let Some(open) = pattern.find('{') else {
        return (!pattern.contains('}')).then(|| vec![pattern.to_string()]);
    };
    let close = open + pattern[open..].find('}')?;
    let inside = &pattern[open + 1..close];
    if inside.contains('{') {
        return None;
    }
    let mut found = Vec::new();
    for rest in braces(&pattern[close + 1..])? {
        for choice in inside.split(',') {
            found.push(format!("{}{choice}{rest}", &pattern[..open]));
        }
    }
    Some(found)
}

impl Workspace {
    /// The deepest package whose directory holds `dir`.
    pub fn member_of(&self, dir: &str) -> Option<&Member> {
        self.members
            .iter()
            .filter(|member| holds(&member.dir, dir))
            .max_by_key(|member| member.dir.len())
    }

    /// The package named `name`.
    pub fn named(&self, name: &str) -> Option<&Member> {
        self.members
            .iter()
            .find(|member| member.name.as_deref() == Some(name))
    }

    /// `dir`'s package and every package it depends on, transitively, by directory.
    pub fn closure(&self, dir: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut next = vec![dir.to_string()];
        while let Some(here) = next.pop() {
            if !found.insert(here.clone()) {
                continue;
            }
            if let Some(member) = self.members.iter().find(|member| member.dir == here) {
                next.extend(member.needs.iter().cloned());
            }
        }
        found
    }

    /// The packages that depend on `dir`'s, transitively, and it.
    fn dependents(&self, dir: &str) -> BTreeSet<String> {
        self.members
            .iter()
            .filter(|member| self.closure(&member.dir).contains(dir))
            .map(|member| member.dir.clone())
            .collect()
    }

    /// Reads each package's name and what it depends on.
    fn link(&mut self, root: &Path, tracked: &BTreeSet<String>) -> Result<(), String> {
        let mut by_name: BTreeMap<String, String> = BTreeMap::new();
        // The root is a package too, which a package may depend on: all of the repository.
        let root_name = std::fs::read_to_string(root.join("package.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| value.get("name")?.as_str().map(str::to_string));
        if let Some(name) = root_name {
            by_name.insert(name, String::new());
        }
        let mut manifests = Vec::new();
        for member in &mut self.members {
            let value: Value = serde_json::from_str(&member.text)
                .map_err(|_| format!("{}/package.json isn't JSON", member.dir))?;
            member.name = value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(name) = &member.name
                && by_name
                    .insert(name.clone(), member.dir.clone())
                    .is_some_and(|other| other != member.dir)
            {
                return Err(format!("two packages of the workspace are named {name}"));
            }
            manifests.push(value);
        }
        for (member, manifest) in self.members.iter_mut().zip(&manifests) {
            for section in DEPENDENCIES {
                let Some(dependencies) = manifest.get(*section) else {
                    continue;
                };
                let Some(dependencies) = dependencies.as_object() else {
                    return Err(format!(
                        "{}/package.json's {section} isn't an object",
                        member.dir
                    ));
                };
                for (name, spec) in dependencies {
                    match needs(name, spec.as_str().unwrap_or_default(), &by_name) {
                        Need::Nothing => {}
                        Need::Dir(dir) if dir == member.dir => {}
                        Need::Dir(dir) => {
                            member.needs.insert(dir);
                        }
                        Need::Unknown(why) => {
                            return Err(format!("{}/package.json {why}", member.dir));
                        }
                    }
                }
            }
        }
        self.turbo(root, tracked, &by_name)?;
        self.tsconfigs(root, tracked, &by_name)?;
        Ok(())
    }

    /// What `turbo.json` files add: a task of one package that needs another's, and what every
    /// task reads.
    fn turbo(
        &mut self,
        root: &Path,
        tracked: &BTreeSet<String>,
        by_name: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let dirs: Vec<String> = self.members.iter().map(|m| m.dir.clone()).collect();
        let files: Vec<&String> = tracked
            .iter()
            .filter(|path| {
                *path == "turbo.json"
                    || path
                        .strip_suffix("/turbo.json")
                        .is_some_and(|dir| dirs.iter().any(|d| d == dir))
            })
            .collect();
        for path in files {
            let owner = path.strip_suffix("/turbo.json").map(str::to_string);
            let Some(text) = read_tracked(root, path)? else {
                continue;
            };
            let value = jsonc(&text).ok_or_else(|| format!("{path} doesn't parse"))?;
            if owner.is_none()
                && let Some(Value::Array(globals)) = value.get("globalDependencies")
            {
                for global in globals.iter().filter_map(Value::as_str) {
                    self.add_everywhere(path, global)?;
                }
            }
            let tasks = value
                .get("tasks")
                .or_else(|| value.get("pipeline"))
                .and_then(Value::as_object);
            for (key, task) in tasks.into_iter().flatten() {
                let from: Vec<String> = match key.split_once('#') {
                    Some((package, _)) => by_name.get(package).cloned().into_iter().collect(),
                    None => match &owner {
                        Some(dir) => vec![dir.clone()],
                        None => dirs.clone(),
                    },
                };
                let needed = task
                    .get("dependsOn")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter_map(|need| need.trim_start_matches('^').split_once('#'))
                    .filter_map(|(package, _)| by_name.get(package));
                for need in needed {
                    for member in self.members.iter_mut().filter(|m| from.contains(&m.dir)) {
                        if member.dir != *need {
                            member.needs.insert(need.clone());
                        }
                    }
                }
                let inputs = task.get("inputs").and_then(Value::as_array);
                for input in inputs.into_iter().flatten().filter_map(Value::as_str) {
                    if let Some(rest) = input.strip_prefix("$TURBO_ROOT$/") {
                        self.add_everywhere(path, rest)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// What tsconfigs add: a package's `extends` that names another package, and where the
    /// root's `paths` map imports, which every package that extends it reads.
    fn tsconfigs(
        &mut self,
        root: &Path,
        tracked: &BTreeSet<String>,
        by_name: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let is_tsconfig = |path: &str| {
            let name = path.rsplit('/').next().unwrap_or(path);
            name.starts_with("tsconfig") && name.ends_with(".json")
        };
        for path in tracked.iter().filter(|path| is_tsconfig(path)) {
            let Some(text) = read_tracked(root, path)? else {
                continue;
            };
            let value = jsonc(&text).ok_or_else(|| format!("{path} doesn't parse"))?;
            if !path.contains('/') {
                self.root_paths(path, &value)?;
                continue;
            }
            let Some(owner) = self.member_of(path).map(|m| m.dir.clone()) else {
                continue;
            };
            let extends = match value.get("extends") {
                Some(Value::String(one)) => vec![one.clone()],
                Some(Value::Array(many)) => strings(many).unwrap_or_default(),
                _ => Vec::new(),
            };
            for name in extends.iter().filter_map(|written| package_part(written)) {
                if let Some(dir) = by_name.get(name)
                    && *dir != owner
                    && let Some(member) = self.members.iter_mut().find(|m| m.dir == owner)
                {
                    member.needs.insert(dir.clone());
                }
            }
        }
        Ok(())
    }

    /// Where a root tsconfig's `compilerOptions.paths` point, which every scope then holds: a
    /// package's whole directory when it is in one.
    fn root_paths(&mut self, file: &str, value: &Value) -> Result<(), String> {
        let options = value.get("compilerOptions");
        let base = options
            .and_then(|o| o.get("baseUrl"))
            .and_then(Value::as_str)
            .unwrap_or(".");
        let paths = options
            .and_then(|o| o.get("paths"))
            .and_then(Value::as_object);
        let targets: Vec<String> = paths
            .into_iter()
            .flatten()
            .flat_map(|(_, targets)| targets.as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        for target in targets {
            let joined = super::projects::joined(base, &target);
            let plain: Vec<&str> = joined
                .split('/')
                .take_while(|part| !part.contains(['*', '?', '[', '{']))
                .collect();
            let plain = super::projects::normal(&plain.join("/")).unwrap_or_else(|| "..".into());
            if plain == ".." || plain.starts_with("../") {
                return Err(format!(
                    "{file} maps imports to {target}, outside the repository"
                ));
            }
            let pattern = match self.member_of(&plain) {
                Some(member) => format!("{}/**", member.dir),
                None if plain.is_empty() => {
                    return Err(format!(
                        "{file} maps imports to {target}, anywhere in the repository"
                    ));
                }
                None => format!("{plain}/**"),
            };
            self.add_everywhere(file, &pattern)?;
        }
        Ok(())
    }

    fn add_everywhere(&mut self, file: &str, pattern: &str) -> Result<(), String> {
        let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
        // A negation narrows what a tool hashes; hushrun's scope stays as wide as it was.
        if pattern.starts_with('!') || pattern.is_empty() {
            return Ok(());
        }
        Pattern::new(pattern)
            .map_err(|_| format!("{file} names {pattern}, a pattern hushrun can't read"))?;
        self.everywhere.insert(pattern.to_string());
        Ok(())
    }

    /// What `words`, one simple command, runs of the packages' scripts, when it runs some through
    /// a package manager's workspace options, Turborepo, Nx or Lerna. None for any other command,
    /// a root script among them, which the caller reads from the root's package.json.
    pub fn fans_out(&self, words: &[&str]) -> Option<Runs> {
        let words = launched(words);
        let (first, rest) = words.split_first()?;
        match *first {
            "pnpm" => self.pnpm(rest),
            "npm" => self.npm(rest),
            "yarn" => self.yarn(rest),
            "bun" => self.bun(rest),
            "lerna" => self.lerna(rest),
            "turbo" => self.turbo_run(rest),
            "nx" => self.nx(rest),
            _ => None,
        }
    }

    /// `script` of the package at `written`: none for the root's, which the caller reads, and no
    /// package at all for a directory that isn't one of the workspace's, whose scripts init
    /// doesn't read.
    fn in_dir(&self, script: String, written: &str) -> Option<Runs> {
        let dir = normalized(written)?;
        Some(match self.members.iter().find(|member| member.dir == dir) {
            Some(member) => one(script, &member.dir),
            None => Runs {
                scripts: vec![script],
                packages: BTreeSet::new(),
                root: false,
                turbo: false,
            },
        })
    }

    fn all(&self) -> BTreeSet<String> {
        self.members.iter().map(|m| m.dir.clone()).collect()
    }

    /// The packages `filters` pick, as pnpm and Turborepo read `--filter`: all, with none.
    fn filtered(&self, filters: &[&str]) -> BTreeSet<String> {
        let (out, picks): (Vec<&str>, Vec<&str>) =
            filters.iter().partition(|filter| filter.starts_with('!'));
        let mut found = match picks.is_empty() {
            true => self.all(),
            false => picks.iter().flat_map(|pick| self.pick(pick)).collect(),
        };
        for filter in out {
            let gone = self.pick(&filter[1..]);
            // A filter hushrun can't read leaves every package in, not out.
            if gone.len() < self.members.len() {
                found.retain(|dir| !gone.contains(dir));
            }
        }
        found
    }

    /// One `--filter`'s packages: by name, with `*`, or by directory, with `...` for what they
    /// depend on or what depends on them, and `^` without themselves. Every package for anything
    /// else, such as a range of commits.
    fn pick(&self, filter: &str) -> BTreeSet<String> {
        let mut rest = filter.trim();
        if rest.contains('[') {
            return self.all();
        }
        let dependents = rest.starts_with("...");
        rest = rest.trim_start_matches("...");
        let skip_self_dependents = dependents && rest.starts_with('^');
        rest = rest.trim_start_matches('^');
        let dependencies = rest.ends_with("...");
        rest = rest.trim_end_matches("...");
        let skip_self_dependencies = dependencies && rest.ends_with('^');
        rest = rest.trim_end_matches('^');
        let matched: BTreeSet<String> =
            match rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
                Some(dir) => self.by_dir(dir),
                None if rest.starts_with('.') || rest.starts_with('/') => self.by_dir(rest),
                None => self.by_name(rest),
            };
        let mut found = BTreeSet::new();
        for dir in &matched {
            if dependencies {
                found.extend(self.closure(dir));
                if skip_self_dependencies {
                    found.remove(dir);
                }
            }
            if dependents {
                found.extend(self.dependents(dir));
                if skip_self_dependents {
                    found.remove(dir);
                }
            }
            if !dependencies && !dependents {
                found.insert(dir.clone());
            }
        }
        found
    }

    fn by_name(&self, written: &str) -> BTreeSet<String> {
        let Ok(pattern) = Pattern::new(&written.replace('/', "\u{1}")) else {
            return self.all();
        };
        self.members
            .iter()
            .filter(|member| {
                member
                    .name
                    .as_deref()
                    .is_some_and(|name| pattern.is_match(&name.replace('/', "\u{1}")))
            })
            .map(|member| member.dir.clone())
            .collect()
    }

    fn by_dir(&self, written: &str) -> BTreeSet<String> {
        let Some(dir) = normalized(written) else {
            return BTreeSet::new();
        };
        if dir.contains(['*', '?', '[']) {
            let Ok(pattern) = Pattern::new(&dir) else {
                return self.all();
            };
            return self
                .members
                .iter()
                .filter(|member| pattern.is_match(&member.dir))
                .map(|member| member.dir.clone())
                .collect();
        }
        self.members
            .iter()
            .filter(|member| holds(&dir, &member.dir))
            .map(|member| member.dir.clone())
            .collect()
    }

    /// `pnpm -r X`, `pnpm --filter A X`, `pnpm -C dir X`.
    fn pnpm(&self, rest: &[&str]) -> Option<Runs> {
        let mut filters = Vec::new();
        let (mut recursive, mut root) = (false, false);
        let mut dir = None;
        let mut words = rest.iter().copied();
        let mut script = None;
        while let Some(word) = words.next() {
            match word {
                "-r" | "--recursive" => recursive = true,
                "--include-workspace-root" => root = true,
                "-w" | "--workspace-root" => return None,
                "--filter" | "-F" | "--filter-prod" => filters.push(words.next()?),
                "-C" | "--dir" => dir = Some(words.next()?),
                "--workspace-concurrency"
                | "--reporter"
                | "--resume-from"
                | "--changed-files-ignore-pattern"
                | "--test-pattern" => {
                    words.next()?;
                }
                "run" | "run-script" if script.is_none() => {}
                "t" | "tst" | "test" if script.is_none() => script = Some("test"),
                "exec" | "dlx" | "install" | "i" | "add" | "remove" | "update" | "up"
                    if script.is_none() =>
                {
                    return None;
                }
                word if word.starts_with('-') => {
                    if let Some(value) = word
                        .strip_prefix("--filter=")
                        .or_else(|| word.strip_prefix("--filter-prod="))
                    {
                        filters.push(value);
                    } else if let Some(value) = word.strip_prefix("--dir=") {
                        dir = Some(value);
                    }
                }
                word if script.is_none() => script = Some(word),
                _ => break,
            }
        }
        let script = script?.to_string();
        if let Some(dir) = dir {
            return self.in_dir(script, dir);
        }
        if !recursive && filters.is_empty() {
            return None;
        }
        Some(Runs {
            scripts: vec![script],
            packages: self.filtered(&filters),
            root,
            turbo: false,
        })
    }

    /// `npm test --workspaces`, `npm run X -w A`, `npm --prefix dir run X`.
    fn npm(&self, rest: &[&str]) -> Option<Runs> {
        let mut picks: Vec<&str> = Vec::new();
        let (mut every, mut root) = (false, false);
        let mut prefix = None;
        let mut words = rest.iter().copied();
        let mut script = None;
        let mut command = None;
        while let Some(word) = words.next() {
            match word {
                "--workspaces" | "-ws" | "--ws" => every = true,
                "--include-workspace-root" => root = true,
                "--workspace" | "-w" => picks.push(words.next()?),
                "--prefix" => prefix = Some(words.next()?),
                "--" => break,
                word if word.starts_with('-') => {
                    if let Some(value) = word
                        .strip_prefix("--workspace=")
                        .or_else(|| word.strip_prefix("-w="))
                    {
                        picks.push(value);
                    } else if let Some(value) = word.strip_prefix("--prefix=") {
                        prefix = Some(value);
                    }
                }
                word if command.is_none() => command = Some(word),
                word if script.is_none()
                    && matches!(command, Some("run" | "run-script" | "rum" | "urn")) =>
                {
                    script = Some(word);
                }
                _ => {}
            }
        }
        let script = match command? {
            "test" | "t" | "tst" => "test",
            "run" | "run-script" | "rum" | "urn" => script?,
            _ => return None,
        }
        .to_string();
        if let Some(prefix) = prefix {
            return self.in_dir(script, prefix);
        }
        if !every && picks.is_empty() {
            return None;
        }
        let packages = match every {
            true => self.all(),
            // npm takes a package's name, its directory, or a directory above several.
            false => picks
                .iter()
                .flat_map(|pick| {
                    let named = self.by_name(pick);
                    match named.is_empty() {
                        true => self.by_dir(pick),
                        false => named,
                    }
                })
                .collect(),
        };
        Some(Runs {
            scripts: vec![script],
            packages,
            root,
            turbo: false,
        })
    }

    /// `yarn workspaces run X`, `yarn workspaces foreach … run X`, `yarn workspace A X`.
    fn yarn(&self, rest: &[&str]) -> Option<Runs> {
        match rest {
            ["workspace", name, script, ..] => {
                let member = self.named(name)?;
                Some(one(script_name(script)?, &member.dir))
            }
            ["workspaces", "run", script, ..] => Some(Runs {
                scripts: vec![script_name(script)?],
                packages: self.all(),
                root: false,
                turbo: false,
            }),
            ["workspaces", "foreach", options @ ..] => {
                let mut include = Vec::new();
                let mut exclude = Vec::new();
                let mut root = false;
                let mut words = options.iter().copied();
                let mut script = None;
                while let Some(word) = words.next() {
                    match word {
                        "--include" => include.push(words.next()?),
                        "--exclude" => exclude.push(words.next()?),
                        "-j" | "--jobs" | "--from" => {
                            words.next()?;
                        }
                        "-W" | "--worktree" => root = true,
                        "run" => {}
                        word if word.starts_with('-') => {
                            if let Some(value) = word.strip_prefix("--include=") {
                                include.push(value);
                            } else if let Some(value) = word.strip_prefix("--exclude=") {
                                exclude.push(value);
                            }
                        }
                        word => {
                            script = Some(word);
                            break;
                        }
                    }
                }
                let mut packages = match include.is_empty() {
                    true => self.all(),
                    false => include.iter().flat_map(|glob| self.glob(glob)).collect(),
                };
                for glob in exclude {
                    let gone = self.glob(glob);
                    packages.retain(|dir| !gone.contains(dir));
                }
                Some(Runs {
                    scripts: vec![script_name(script?)?],
                    packages,
                    root,
                    turbo: false,
                })
            }
            _ => None,
        }
    }

    /// A package's name or directory, as Yarn's `--include` matches either.
    fn glob(&self, written: &str) -> BTreeSet<String> {
        let mut found = self.by_name(written);
        found.extend(self.by_dir(written));
        found
    }

    /// `bun run --filter A X`, `bun --filter A run X`.
    fn bun(&self, rest: &[&str]) -> Option<Runs> {
        let mut filters = Vec::new();
        let mut every = false;
        let mut words = rest.iter().copied();
        let mut script = None;
        while let Some(word) = words.next() {
            match word {
                "--filter" | "-F" => filters.push(words.next()?),
                "--workspaces" => every = true,
                "run" if script.is_none() => {}
                word if word.starts_with('-') => {
                    if let Some(value) = word.strip_prefix("--filter=") {
                        filters.push(value);
                    }
                }
                word if script.is_none() => script = Some(word),
                _ => break,
            }
        }
        if filters.is_empty() && !every {
            return None;
        }
        Some(Runs {
            scripts: vec![script_name(script?)?],
            packages: self.filtered(&filters),
            root: false,
            turbo: false,
        })
    }

    /// `lerna run X`, with `--scope` and `--ignore`.
    fn lerna(&self, rest: &[&str]) -> Option<Runs> {
        let ["run", options @ ..] = rest else {
            return None;
        };
        let mut scopes = Vec::new();
        let mut ignores = Vec::new();
        let mut words = options.iter().copied();
        let mut script = None;
        while let Some(word) = words.next() {
            match word {
                "--scope" => scopes.push(words.next()?),
                "--ignore" => ignores.push(words.next()?),
                "--concurrency" | "--npm-client" | "--loglevel" => {
                    words.next()?;
                }
                "--" => break,
                word if word.starts_with('-') => {
                    if let Some(value) = word.strip_prefix("--scope=") {
                        scopes.push(value);
                    } else if let Some(value) = word.strip_prefix("--ignore=") {
                        ignores.push(value);
                    }
                }
                word if script.is_none() => script = Some(word),
                _ => {}
            }
        }
        let mut packages = match scopes.is_empty() {
            true => self.all(),
            false => scopes.iter().flat_map(|glob| self.by_name(glob)).collect(),
        };
        for glob in ignores {
            let gone = self.by_name(glob);
            packages.retain(|dir| !gone.contains(dir));
        }
        let scripts = script?
            .split(',')
            .map(script_name)
            .collect::<Option<Vec<_>>>()?;
        Some(Runs {
            scripts,
            packages,
            root: false,
            turbo: false,
        })
    }

    /// `turbo run X Y`, `turbo X`, with `--filter`, and `pkg#task` for one package's.
    fn turbo_run(&self, rest: &[&str]) -> Option<Runs> {
        const VALUED: &[&str] = &[
            "--filter",
            "-F",
            "--concurrency",
            "--cache-dir",
            "--output-logs",
            "--log-order",
            "--log-prefix",
            "--env-mode",
            "--cache",
            "--profile",
            "--team",
            "--token",
            "--api",
            "--cwd",
            "--heap",
            "--ui",
            "--remote-cache-timeout",
            "--scope",
            "--ignore",
            "--since",
            "--global-deps",
        ];
        let rest = match rest {
            ["run", rest @ ..] => rest,
            [first, ..] if TURBO_COMMANDS.contains(first) => return None,
            rest => rest,
        };
        let mut filters = Vec::new();
        let mut tasks = Vec::new();
        let mut env_mode = None;
        let mut words = rest.iter().copied();
        while let Some(word) = words.next() {
            match word {
                "--" => break,
                "--filter" | "-F" => filters.push(words.next()?),
                "--env-mode" => env_mode = Some(words.next()?),
                word if VALUED.contains(&word) => {
                    words.next()?;
                }
                word if word.starts_with('-') => {
                    if let Some(value) = word.strip_prefix("--filter=") {
                        filters.push(value);
                    }
                    if let Some(value) = word.strip_prefix("--env-mode=") {
                        env_mode = Some(value);
                    }
                }
                word => tasks.push(word),
            }
        }
        let mut scripts = Vec::new();
        let mut packages = BTreeSet::new();
        let mut everywhere = false;
        for task in &tasks {
            match task.split_once('#') {
                Some((package, script)) => {
                    let member = self.named(package)?;
                    packages.insert(member.dir.clone());
                    scripts.push(script_name(script)?);
                }
                None => {
                    everywhere = true;
                    scripts.push(script_name(task)?);
                }
            }
        }
        if everywhere {
            packages.extend(self.filtered(&filters));
        }
        (!scripts.is_empty()).then_some(Runs {
            scripts,
            packages,
            root: false,
            turbo: env_mode != Some("loose"),
        })
    }

    /// `nx run-many -t X`, `nx affected -t X`, `nx run A:X`, when each package's target is its
    /// script: a project.json can make a target run something else.
    fn nx(&self, rest: &[&str]) -> Option<Runs> {
        let (first, options) = rest.split_first()?;
        if let ("run", [target, ..]) = (*first, options) {
            let (project, script) = target.split_once(':')?;
            let script = script.split(':').next()?;
            let member = self.named(project)?;
            return Some(one(script_name(script)?, &member.dir));
        }
        if !matches!(*first, "run-many" | "affected") {
            return None;
        }
        let mut targets = Vec::new();
        let mut projects = Vec::new();
        let mut excluded = Vec::new();
        let mut words = options.iter().copied();
        while let Some(word) = words.next() {
            match word {
                "-t" | "--target" | "--targets" => targets.push(words.next()?),
                "-p" | "--projects" => projects.push(words.next()?),
                "--exclude" => excluded.push(words.next()?),
                "--" => break,
                word => {
                    let value =
                        |flags: &[&str]| flags.iter().find_map(|flag| word.strip_prefix(flag));
                    if let Some(value) = value(&["--target=", "--targets=", "-t="]) {
                        targets.push(value);
                    } else if let Some(value) = value(&["--projects=", "-p="]) {
                        projects.push(value);
                    } else if let Some(value) = value(&["--exclude="]) {
                        excluded.push(value);
                    }
                }
            }
        }
        let scripts = targets
            .iter()
            .flat_map(|list| list.split(','))
            .map(script_name)
            .collect::<Option<Vec<_>>>()?;
        if scripts.is_empty() {
            return None;
        }
        let mut packages = match projects.is_empty() {
            true => self.all(),
            false => projects
                .iter()
                .flat_map(|list| list.split(','))
                .flat_map(|glob| self.by_name(glob))
                .collect(),
        };
        for list in excluded {
            for glob in list.split(',') {
                let gone = self.by_name(glob);
                packages.retain(|dir| !gone.contains(dir));
            }
        }
        Some(Runs {
            scripts,
            packages,
            root: false,
            turbo: false,
        })
    }
}

/// Turborepo's own commands, which run no task.
const TURBO_COMMANDS: &[&str] = &[
    "login",
    "logout",
    "link",
    "unlink",
    "prune",
    "gen",
    "generate",
    "info",
    "ls",
    "query",
    "daemon",
    "telemetry",
    "boundaries",
    "watch",
    "scan",
    "bin",
    "completion",
    "devtools",
];

/// One package's script.
fn one(script: String, dir: &str) -> Runs {
    Runs {
        scripts: vec![script],
        packages: BTreeSet::from([dir.to_string()]),
        root: false,
        turbo: false,
    }
}

/// A script's name, as a command names it: none for an option.
fn script_name(word: &str) -> Option<String> {
    (!word.is_empty() && !word.starts_with('-')).then(|| word.to_string())
}

/// `words` past what starts a package's program: `npx`, `pnpm exec`, `yarn`, a bare `pnpm`.
fn launched<'w, 'a>(words: &'w [&'a str]) -> &'w [&'a str] {
    const TOOLS: &[&str] = &["turbo", "nx", "lerna"];
    let skip = match words {
        [
            "npx" | "bunx",
            "--yes" | "-y" | "--no-install" | "--no",
            tool,
            ..,
        ] if TOOLS.contains(tool) => 2,
        ["npx" | "bunx", tool, ..] if TOOLS.contains(tool) => 1,
        ["pnpm" | "yarn" | "npm" | "bun", "exec" | "x", tool, ..] if TOOLS.contains(tool) => 2,
        ["pnpm" | "yarn" | "bun", tool, ..] if TOOLS.contains(tool) => 1,
        _ => 0,
    };
    &words[skip..]
}

/// The package a bare specifier names, `@scope/name` or `name`, without what follows in it.
fn package_part(written: &str) -> Option<&str> {
    if written.starts_with('.') || written.starts_with('/') {
        return None;
    }
    let mut parts = written.splitn(3, '/');
    let first = parts.next()?;
    match first.starts_with('@') {
        true => {
            let second = parts.next()?;
            Some(&written[..first.len() + 1 + second.len()])
        }
        false => Some(first),
    }
}

/// Whether `dir`, relative to the root, is `above` or below it.
fn holds(above: &str, dir: &str) -> bool {
    above.is_empty()
        || dir == above
        || dir
            .strip_prefix(above)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// What a dependency on a package of the workspace means.
enum Need {
    Nothing,
    Dir(String),
    Unknown(String),
}

fn needs(name: &str, spec: &str, by_name: &BTreeMap<String, String>) -> Need {
    if let Some(alias) = spec.strip_prefix("npm:") {
        // `npm:@scope/name@range` or `npm:name@range`: the name before the range's `@`.
        let after_scope = usize::from(alias.starts_with('@'));
        let target = match alias[after_scope..].find('@') {
            Some(at) => &alias[..after_scope + at],
            None => alias,
        };
        return match by_name.get(target) {
            Some(dir) => Need::Dir(dir.clone()),
            None => Need::Nothing,
        };
    }
    let member = by_name.get(name);
    if spec.starts_with("workspace:") {
        return match member {
            Some(dir) => Need::Dir(dir.clone()),
            None => Need::Unknown(format!(
                "depends on {name} from its workspace, and no package of the workspace has that name"
            )),
        };
    }
    let Some(dir) = member else {
        return Need::Nothing;
    };
    // A path is read as what it names, by the scope's reading of manifests.
    if ["file:", "link:", "portal:", "./", "../"]
        .iter()
        .any(|scheme| spec.starts_with(scheme))
    {
        return Need::Nothing;
    }
    // A range or a tag may come from the registry: counting the package widens the scope only.
    if spec.starts_with("catalog:") || !spec.contains([':', '/']) {
        return Need::Dir(dir.clone());
    }
    Need::Unknown(format!(
        "gets {name} from {spec}, which hushrun can't place"
    ))
}

/// JSON with the comments and trailing commas a tsconfig, `turbo.json` or `lerna.json` may have.
pub fn jsonc(text: &str) -> Option<Value> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for next in chars.by_ref() {
                    if last == '*' && next == '/' {
                        break;
                    }
                    last = next;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    // A comma before `}` or `]` ends nothing.
    let mut clean = String::with_capacity(out.len());
    let mut in_string = false;
    let bytes: Vec<char> = out.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            clean.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                clean.push(bytes[i + 1]);
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            clean.push(c);
        } else if c == ',' {
            let next = bytes[i + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}' | ']')) {
                clean.push(c);
            }
        } else {
            clean.push(c);
        }
        i += 1;
    }
    serde_json::from_str(&clean).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(dir: &str, name: &str, needs: &[&str]) -> Member {
        Member {
            dir: dir.to_string(),
            name: Some(name.to_string()),
            text: String::new(),
            needs: needs.iter().map(|need| (*need).to_string()).collect(),
        }
    }

    fn workspace() -> Workspace {
        Workspace {
            file: "pnpm-workspace.yaml".to_string(),
            members: vec![
                member("packages/a", "@x/a", &["packages/b"]),
                member("packages/b", "@x/b", &[]),
                member("apps/web", "web", &["packages/a"]),
            ],
            unreadable: None,
            everywhere: BTreeSet::new(),
        }
    }

    fn dirs(runs: &Option<Runs>) -> Vec<&str> {
        runs.iter()
            .flat_map(|runs| &runs.packages)
            .map(String::as_str)
            .collect()
    }

    // covers: C-15
    #[test]
    fn a_package_reaches_what_it_depends_on_through_others() {
        let ws = workspace();
        assert_eq!(
            ws.closure("apps/web"),
            BTreeSet::from(["apps/web", "packages/a", "packages/b"].map(String::from))
        );
        assert_eq!(ws.dependents("packages/b").len(), 3);
        assert_eq!(
            ws.member_of("packages/a/src").map(|m| m.dir.as_str()),
            Some("packages/a")
        );
        assert_eq!(ws.member_of("tools"), None);
    }

    // covers: C-15
    #[test]
    fn fan_outs_name_the_packages_they_run() {
        let ws = workspace();
        let runs = |line: &str| ws.fans_out(&line.split_whitespace().collect::<Vec<_>>());
        assert_eq!(dirs(&runs("pnpm -r test")).len(), 3);
        assert_eq!(runs("pnpm -r test").unwrap().scripts, ["test"]);
        assert_eq!(runs("pnpm --recursive run lint").unwrap().scripts, ["lint"]);
        assert_eq!(dirs(&runs("pnpm --filter @x/a test")), ["packages/a"]);
        assert_eq!(
            dirs(&runs("pnpm --filter=@x/* test")),
            ["packages/a", "packages/b"]
        );
        assert_eq!(dirs(&runs("pnpm --filter ./apps/** test")), ["apps/web"]);
        assert_eq!(
            dirs(&runs("pnpm --filter web... test")),
            ["apps/web", "packages/a", "packages/b"]
        );
        assert_eq!(
            dirs(&runs("pnpm --filter ...@x/b test")),
            ["apps/web", "packages/a", "packages/b"]
        );
        assert_eq!(
            dirs(&runs("pnpm --filter ...^@x/b test")),
            ["apps/web", "packages/a"]
        );
        assert_eq!(
            dirs(&runs("pnpm -r --filter !web test")),
            ["packages/a", "packages/b"]
        );
        assert_eq!(dirs(&runs("pnpm -C packages/b test")), ["packages/b"]);
        // Another package's script, which init doesn't read: no package of the workspace's.
        assert_eq!(
            dirs(&runs("npm test --prefix=.github/scripts")),
            Vec::<&str>::new()
        );
        assert_eq!(runs("npm --prefix . test"), None);
        assert_eq!(runs("pnpm test"), None);
        assert_eq!(runs("pnpm -r exec vitest"), None);
        assert_eq!(dirs(&runs("npm test --workspaces")).len(), 3);
        assert_eq!(dirs(&runs("npm run lint -w @x/b")), ["packages/b"]);
        assert_eq!(
            dirs(&runs("npm run lint --workspace=packages/a")),
            ["packages/a"]
        );
        assert_eq!(runs("npm run lint"), None);
        assert_eq!(dirs(&runs("yarn workspace web test")), ["apps/web"]);
        assert_eq!(dirs(&runs("yarn workspaces run test")).len(), 3);
        assert_eq!(
            dirs(&runs("yarn workspaces foreach -A --exclude web run test")),
            ["packages/a", "packages/b"]
        );
        assert_eq!(dirs(&runs("bun run --filter @x/a test")), ["packages/a"]);
        assert_eq!(
            dirs(&runs("lerna run test --scope @x/*")),
            ["packages/a", "packages/b"]
        );
        assert_eq!(
            runs("npx lerna run test,lint").unwrap().scripts,
            ["test", "lint"]
        );
        assert_eq!(dirs(&runs("turbo run test --filter=web")), ["apps/web"]);
        assert_eq!(
            runs("pnpm turbo test lint").unwrap().scripts,
            ["test", "lint"]
        );
        assert_eq!(dirs(&runs("turbo run web#test")), ["apps/web"]);
        assert_eq!(runs("turbo prune web"), None);
        assert_eq!(
            dirs(&runs("nx run-many -t test -p @x/a,web")),
            ["apps/web", "packages/a"]
        );
        assert_eq!(dirs(&runs("nx run web:test:ci")), ["apps/web"]);
        assert_eq!(runs("nx graph"), None);
    }

    // covers: C-14
    #[test]
    fn a_dependency_hushrun_cant_place_stops_the_graph() {
        let by_name = BTreeMap::from([("@x/b".to_string(), "packages/b".to_string())]);
        let need = |name, spec| match needs(name, spec, &by_name) {
            Need::Nothing => "nothing".to_string(),
            Need::Dir(dir) => dir,
            Need::Unknown(why) => why,
        };
        assert_eq!(need("@x/b", "workspace:*"), "packages/b");
        assert_eq!(need("@x/b", "^1.2.0"), "packages/b");
        assert_eq!(need("@x/b", "catalog:"), "packages/b");
        assert_eq!(need("alias", "npm:@x/b@^1"), "packages/b");
        assert_eq!(need("@x/b", "file:../b"), "nothing");
        assert_eq!(need("react", "^19.0.0"), "nothing");
        assert!(need("@x/c", "workspace:*").contains("no package of the workspace"));
        assert!(need("@x/b", "github:x/b").contains("can't place"));
    }

    // covers: C-15
    #[test]
    fn patterns_read_as_the_tools_read_them() {
        assert_eq!(
            braces("packages/{a,b}").unwrap(),
            ["packages/a", "packages/b"]
        );
        assert_eq!(braces("{a,b}/{c,d}").unwrap().len(), 4);
        assert_eq!(braces("a/{b,{c}}"), None);
        assert_eq!(normalized("./packages/*/"), Some("packages/*".to_string()));
        assert_eq!(normalized("."), None);
        assert_eq!(
            package_part("@repo/tsconfig/base.json"),
            Some("@repo/tsconfig")
        );
        assert_eq!(package_part("tsconfig/base"), Some("tsconfig"));
        assert_eq!(package_part("../tsconfig.base.json"), None);
        let value =
            jsonc("{\n  // a comment\n  \"a\": [1, 2,], /* b */ \"c\": \"//x\",\n}").unwrap();
        assert_eq!(value, serde_json::json!({"a": [1, 2], "c": "//x"}));
    }
}
