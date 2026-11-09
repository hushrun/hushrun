//! \[ADR 0050\] Tools that reuse their own results across machines (C-15): Turborepo, Nx, Gradle,
//! Bazel and Pants, once a remote cache is set up. Wrapping a command that runs through one
//! caches twice and hides the tool's own reporting, so init leaves it to the tool, and says why.
//! Without a remote cache nothing changes: hushrun adds a verdict every machine shares. A person
//! can still wrap such a line by hand.
//!
//! What sets each up, read from the repository's files and CI's own:
//!
//! - Turborepo: `TURBO_TOKEN` or `TURBO_TEAM` in a CI file's `env` or `variables`, or remote
//!   cache settings, `remoteCache`, in `turbo.json`.
//! - Nx: `nxCloudAccessToken`, `nxCloudId` or Nx Cloud's runner in `nx.json`, or
//!   `NX_CLOUD_ACCESS_TOKEN` in a CI file, unless `nx.json` says `neverConnectToCloud`.
//! - Gradle: a remote `buildCache` in the build's settings file, Develocity's or Gradle
//!   Enterprise's included, or the Develocity plugin applied there, directly or through a
//!   conventions plugin, with caching on: `org.gradle.caching=true` in the build's
//!   `gradle.properties`, or `--build-cache` on the command line.
//! - Bazel: `--remote_cache`, `--remote_executor` or `--bes_backend` in the workspace's
//!   `.bazelrc`, in what it imports, or on the command line.
//! - Pants: `remote_cache_read = true` in `pants.toml` or `pants.ci.toml`,
//!   `PANTS_REMOTE_CACHE_READ` in a CI file, or `--remote-cache-read` on the command line.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::checks::{self, Verdict};
use super::scripts;
use super::variants;
use super::workflows::Workflow;

/// A tool that keeps its own results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Turborepo,
    Nx,
    Gradle,
    Bazel,
    Pants,
}

impl Tool {
    const ALL: [Tool; 5] = [
        Tool::Turborepo,
        Tool::Nx,
        Tool::Gradle,
        Tool::Bazel,
        Tool::Pants,
    ];

    /// Its name, as its people write it.
    fn name(self) -> &'static str {
        match self {
            Tool::Turborepo => "Turborepo",
            Tool::Nx => "Nx",
            Tool::Gradle => "Gradle",
            Tool::Bazel => "Bazel",
            Tool::Pants => "Pants",
        }
    }

    /// Why init leaves a command that runs through it to it.
    pub fn why(self) -> String {
        let name = self.name();
        format!(
            "{name} reuses its own results across machines (remote cache), so init leaves it to {name}"
        )
    }
}

/// Whether `why`, the reason init leaves a command or a script, is that its tool's remote cache
/// keeps its results.
pub fn stepped_aside(why: &str) -> bool {
    Tool::ALL.iter().any(|tool| why == tool.why())
}

/// A command init leaves to its tool's remote cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aside {
    /// The check the table knows it as, when it does.
    pub check: Option<String>,
    pub why: String,
}

/// What a repository's remote caches are: [`Caches::default`] has none.
#[derive(Debug, Default)]
pub struct Caches {
    /// The repository's root, where each build's files say whether Gradle's and Bazel's caches
    /// are remote; none, for no remote cache at all.
    root: Option<PathBuf>,
    turborepo: bool,
    nx: bool,
    pants: bool,
}

/// What a CI file's `env` or `variables` names Turborepo's remote cache by.
const TURBO_VARIABLES: &[&str] = &["TURBO_TOKEN", "TURBO_TEAM"];
/// What names Nx Cloud's.
const NX_VARIABLE: &str = "NX_CLOUD_ACCESS_TOKEN";
/// What turns Pants's on.
const PANTS_VARIABLE: &str = "PANTS_REMOTE_CACHE_READ";

impl Caches {
    /// The remote caches of the repository at `root`, whose CI's files are `files`.
    pub fn read(root: &Path, files: &[Workflow]) -> Caches {
        let variables = variables(files);
        let set = |name: &str| {
            variables
                .iter()
                .any(|(n, value)| n == name && !value.trim().is_empty())
        };
        let turborepo = TURBO_VARIABLES.iter().any(|name| set(name)) || turbo_json(root);
        let nx = nx_json(root).unwrap_or_else(|| set(NX_VARIABLE));
        let pants = pants_toml(root)
            || variables
                .iter()
                .any(|(name, value)| name == PANTS_VARIABLE && truthy(value));
        Caches {
            root: Some(root.to_path_buf()),
            turborepo,
            nx,
            pants,
        }
    }

    /// Why init leaves `words`, a command that runs in `dir`, relative to the root, to its tool,
    /// when that tool's remote cache keeps what it runs; and the check the table knows it as.
    /// `javascript` is what the table takes: see [`checks::recognize`].
    pub fn aside(&self, words: &[&str], dir: &str, javascript: bool) -> Option<Aside> {
        self.aside_by(words, dir, &|words| checks::recognize(words, javascript))
    }

    /// Why init leaves the package.json script `command`, which runs at the root, to its tool's
    /// remote cache, when a command of it runs through one.
    pub fn aside_script(&self, command: &str) -> Option<String> {
        scripts::commands(command, &mut Some(String::new()))
            .iter()
            .find_map(|found| {
                let words: Vec<&str> = found.words.iter().map(String::as_str).collect();
                self.aside(&words, "", false).map(|aside| aside.why)
            })
    }

    /// [`Caches::aside`], with `recognize` for the table: a Gradle command is a check the table
    /// knows, whatever its rows.
    fn aside_by(
        &self,
        words: &[&str],
        dir: &str,
        recognize: &dyn Fn(&[&str]) -> Option<Verdict>,
    ) -> Option<Aside> {
        let root = self.root.as_deref()?;
        let (tool, rest) = tool_of(words)?;
        let verdict = recognize(words);
        if tool == Tool::Gradle && verdict.is_none() {
            return None;
        }
        let remote = match tool {
            Tool::Turborepo => self.turborepo,
            Tool::Nx => self.nx,
            Tool::Gradle => gradle(root, dir, rest),
            Tool::Bazel => bazel(root, dir, rest),
            Tool::Pants => self.pants || rest.contains(&"--remote-cache-read"),
        };
        remote.then(|| Aside {
            check: match verdict {
                Some(Verdict::Check(name)) => Some(name),
                _ => None,
            },
            why: tool.why(),
        })
    }
}

/// Turborepo's commands that run no task.
const TURBO_COMMANDS: &[&str] = &[
    "bin",
    "boundaries",
    "completion",
    "daemon",
    "devtools",
    "docs",
    "gen",
    "generate",
    "help",
    "info",
    "link",
    "login",
    "logout",
    "ls",
    "prune",
    "query",
    "scan",
    "telemetry",
    "unlink",
    "version",
    "watch",
];

/// Gradle, as a build starts it.
const GRADLE: &[&str] = &[
    "gradle",
    "./gradlew",
    "gradlew",
    "./gradlew.bat",
    "gradlew.bat",
];

/// The tool `words` run through, and the words after its program, when it is one whose remote
/// cache keeps what it runs: `turbo run` and `turbo <task>`, `nx run`, `nx run-many` and
/// `nx affected`, Gradle, `bazel` and `bazelisk` `test` and `build`, and Pants's `test`, `lint`
/// and `check`. A launcher, and what starts a package's program (`npx`, `pnpm`, `yarn`, `npm
/// exec`, `bun x`), is looked past.
fn tool_of<'a>(words: &'a [&'a str]) -> Option<(Tool, &'a [&'a str])> {
    let words = checks::without_launchers(past_package_runners(words));
    let (program, rest) = words.split_first()?;
    let first = rest.iter().copied().find(|word| !word.starts_with('-'));
    let name = program
        .strip_prefix("./node_modules/.bin/")
        .or_else(|| program.strip_prefix("node_modules/.bin/"))
        .unwrap_or(program);
    // A package's version, as in `npx turbo@2`.
    let name = match name.split_once('@') {
        Some((name, _)) if !name.is_empty() => name,
        _ => name,
    };
    let tool = match name {
        "turbo" => first
            .is_some_and(|word| !TURBO_COMMANDS.contains(&word))
            .then_some(Tool::Turborepo),
        "nx" => first
            .is_some_and(|word| {
                matches!(word, "run" | "run-many" | "affected") || word.starts_with("affected:")
            })
            .then_some(Tool::Nx),
        "bazel" | "bazelisk" => matches!(first, Some("test" | "build")).then_some(Tool::Bazel),
        "pants" | "./pants" => rest
            .iter()
            .any(|word| matches!(*word, "test" | "lint" | "check"))
            .then_some(Tool::Pants),
        name if GRADLE.contains(&name) => Some(Tool::Gradle),
        _ => None,
    };
    Some((tool?, rest))
}

/// `words` past what runs a package's program, such as `pnpm` in `pnpm turbo run test`, with its
/// own options; `words` themselves when nothing does.
fn past_package_runners<'a>(words: &'a [&'a str]) -> &'a [&'a str] {
    let rest = match words {
        ["npx" | "bunx", rest @ ..]
        | ["pnpm" | "yarn", "exec" | "dlx", rest @ ..]
        | ["npm", "exec" | "x", rest @ ..]
        | ["bun", "x", rest @ ..]
        | ["pnpm" | "yarn", rest @ ..] => rest,
        _ => return words,
    };
    let at = rest
        .iter()
        .position(|word| !word.starts_with('-'))
        .unwrap_or(rest.len());
    &rest[at..]
}

/// Whether Gradle, started in `dir` with `args`, uses a remote build cache: its build's settings
/// file sets one up, and caching is on, by the build's `gradle.properties` and then the command
/// line. The build is the nearest directory with a settings file, from where it starts: `dir`, or
/// the one `-p` or `--project-dir` names.
fn gradle(root: &Path, dir: &str, args: &[&str]) -> bool {
    let start = match project_dir(args) {
        None => Some(dir.to_string()),
        Some(project) => inside(dir, project),
    };
    let Some(start) = start else {
        return false;
    };
    let Some((build, settings)) = ancestors(&start).into_iter().find_map(|build| {
        ["settings.gradle.kts", "settings.gradle"]
            .iter()
            .find_map(|name| std::fs::read_to_string(root.join(&build).join(name)).ok())
            .map(|settings| (build, settings))
    }) else {
        return false;
    };
    let properties =
        std::fs::read_to_string(root.join(&build).join("gradle.properties")).unwrap_or_default();
    (remote_build_cache(&settings) || develocity(&settings)) && caching_on(&properties, args)
}

/// Whether a Gradle settings file applies Develocity, or Gradle Enterprise as it was called: a
/// quoted string in it, comments aside, names one, as a plugin's id does, `com.gradle.develocity`,
/// `com.gradle.enterprise`, or a conventions plugin's such as `io.spring.develocity.conventions`.
/// With caching on, the plan counts its cache as remote (D7): a wrong guess only leaves a line to
/// Gradle, which runs it as before.
fn develocity(settings: &str) -> bool {
    let text = without_comments(settings);
    let mut rest = text.as_str();
    while let Some(open) = rest.find(['"', '\'']) {
        let quote = rest[open..].chars().next().unwrap_or('"');
        let after = &rest[open + 1..];
        let Some(close) = after.find(quote) else {
            break;
        };
        let string = after[..close].to_ascii_lowercase();
        if ["develocity", "gradle.enterprise", "gradle-enterprise"]
            .iter()
            .any(|name| string.contains(name))
        {
            return true;
        }
        rest = &after[close + 1..];
    }
    false
}

/// The directory `-p` or `--project-dir` names among Gradle's `args`.
fn project_dir<'a>(args: &[&'a str]) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(at, arg)| match *arg {
        "-p" | "--project-dir" => args.get(at + 1).copied(),
        arg => arg.strip_prefix("--project-dir="),
    })
}

/// Whether a Gradle settings file sets up a remote build cache: a `buildCache` with a `remote` in
/// its block, as `remote<HttpBuildCache>` or `remote(develocity.buildCache)` are, or right after
/// it, as in `buildCache.remote(...)`. Comments don't count.
fn remote_build_cache(settings: &str) -> bool {
    let text = without_comments(settings);
    let mut from = 0;
    while let Some(at) = text[from..].find("buildCache").map(|at| from + at) {
        from = at + "buildCache".len();
        let word = !text[..at].ends_with(identifier) && !text[from..].starts_with(identifier);
        if !word {
            continue;
        }
        let rest = text[from..].trim_start();
        let remote = match (rest.strip_prefix('.'), rest.strip_prefix('{')) {
            (Some(after), _) => starts_with_word(after.trim_start(), "remote"),
            (_, Some(block)) => holds_word(within_braces(block), "remote"),
            _ => false,
        };
        if remote {
            return true;
        }
    }
    false
}

/// Whether `c` can be part of a Groovy or Kotlin name.
fn identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `block`, what follows a `{`, up to the `}` that closes it.
fn within_braces(block: &str) -> &str {
    let mut depth = 1;
    for (at, c) in block.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &block[..at];
                }
            }
            _ => {}
        }
    }
    block
}

/// Whether `text` starts with `word`, as a word of its own.
fn starts_with_word(text: &str, word: &str) -> bool {
    text.strip_prefix(word)
        .is_some_and(|rest| !rest.starts_with(identifier))
}

/// Whether `text` holds `word`, as a word of its own.
fn holds_word(text: &str, word: &str) -> bool {
    text.match_indices(word)
        .any(|(at, _)| !text[..at].ends_with(identifier) && starts_with_word(&text[at..], word))
}

/// Whether Gradle's build cache is on: `org.gradle.caching` in `properties`, a build's
/// `gradle.properties`, then the command line's `--build-cache`, `--no-build-cache` and
/// `-Dorg.gradle.caching=…`, the last one winning.
fn caching_on(properties: &str, args: &[&str]) -> bool {
    let mut on = property(properties, "org.gradle.caching")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));
    for arg in args {
        match *arg {
            "--build-cache" => on = true,
            "--no-build-cache" => on = false,
            arg => {
                if let Some(value) = arg.strip_prefix("-Dorg.gradle.caching=") {
                    on = value.eq_ignore_ascii_case("true");
                }
            }
        }
    }
    on
}

/// The last value of `key` in `text`, a Java properties file: `key=value`, `key: value` or
/// `key value`, `#` and `!` starting comments.
fn property(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with(['#', '!']) {
                return None;
            }
            let at = line.find(['=', ':', ' ', '\t'])?;
            let value = line[at..].trim_start_matches([' ', '\t']);
            let value = value.strip_prefix(['=', ':']).unwrap_or(value);
            (line[..at].trim() == key).then(|| value.trim().to_string())
        })
        .next_back()
}

/// What makes a directory a Bazel workspace.
const WORKSPACE_FILES: &[&str] = &["MODULE.bazel", "REPO.bazel", "WORKSPACE.bazel", "WORKSPACE"];

/// How deep `.bazelrc`'s imports are followed.
const IMPORTS: usize = 3;

/// Whether Bazel, started in `dir` with `args`, uses a remote cache, executor or build event
/// service: on the command line, or in its workspace's `.bazelrc`, what that imports, or a file
/// `--bazelrc` names. The workspace is the nearest directory, from `dir`, with a `MODULE.bazel`,
/// `REPO.bazel`, `WORKSPACE.bazel` or `WORKSPACE`; else the root.
fn bazel(root: &Path, dir: &str, args: &[&str]) -> bool {
    if remote_flags(args) {
        return true;
    }
    let workspace = ancestors(dir)
        .into_iter()
        .find(|ws| {
            WORKSPACE_FILES
                .iter()
                .any(|f| root.join(ws).join(f).is_file())
        })
        .unwrap_or_default();
    let workspace = root.join(workspace);
    let mut files = vec![workspace.join(".bazelrc")];
    // Startup options, before the command.
    for arg in args.iter().take_while(|arg| arg.starts_with('-')) {
        if let Some(file) = arg.strip_prefix("--bazelrc=").and_then(|f| inside(dir, f)) {
            files.push(root.join(file));
        }
    }
    files.iter().any(|file| bazelrc_remote(&workspace, file, 0))
}

/// Whether the bazelrc at `path` sets a remote cache, executor or build event service, for any
/// command or config, itself or in a file it imports from `workspace`.
fn bazelrc_remote(workspace: &Path, path: &Path, depth: usize) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            [first, ..] if first.starts_with('#') => false,
            ["import" | "try-import", file] => {
                let file = file.strip_prefix("%workspace%/").unwrap_or(file);
                depth < IMPORTS
                    && inside("", file).is_some_and(|file| {
                        bazelrc_remote(workspace, &workspace.join(file), depth + 1)
                    })
            }
            [_, flags @ ..] => remote_flags(flags),
            [] => false,
        }
    })
}

/// Whether `flags` set a remote cache, executor or build event service to a value.
fn remote_flags(flags: &[&str]) -> bool {
    const FLAGS: &[&str] = &["--remote_cache", "--remote_executor", "--bes_backend"];
    flags.iter().enumerate().any(|(at, flag)| {
        FLAGS.iter().any(|name| match flag.strip_prefix(name) {
            Some("") => flags
                .get(at + 1)
                .is_some_and(|value| !value.starts_with('-')),
            Some(rest) => rest
                .strip_prefix('=')
                .is_some_and(|value| !value.trim_matches(['"', '\'']).is_empty()),
            None => false,
        })
    })
}

/// `dir`, then each directory above it, to the root: `/`-separated, `""` for the root.
fn ancestors(dir: &str) -> Vec<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
    let mut found = vec![parts.join("/")];
    while parts.pop().is_some() {
        found.push(parts.join("/"));
    }
    found
}

/// `path`, relative to `dir`, from the root, when it stays inside the repository.
fn inside(dir: &str, path: &str) -> Option<String> {
    if path.starts_with(['/', '~', '$']) {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
    for part in path.split('/') {
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

/// Whether `turbo.json`, or `turbo.jsonc`, has remote cache settings: a `remoteCache` that sets
/// anything, but `enabled: false`.
fn turbo_json(root: &Path) -> bool {
    ["turbo.json", "turbo.jsonc"]
        .iter()
        .filter_map(|name| json_file(&root.join(name)))
        .any(|doc| {
            doc.get("remoteCache")
                .and_then(Value::as_object)
                .is_some_and(|cache| {
                    !cache.is_empty() && cache.get("enabled") != Some(&Value::Bool(false))
                })
        })
}

/// What `nx.json` says of Nx Cloud: `Some(true)` when it names a token, a workspace or Nx Cloud's
/// runner; `Some(false)` when it says never to connect, whatever else does; else `None`.
fn nx_json(root: &Path) -> Option<bool> {
    let doc = json_file(&root.join("nx.json"))?;
    if doc.get("neverConnectToCloud") == Some(&Value::Bool(true)) {
        return Some(false);
    }
    let named = |key: &str| {
        doc.get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    };
    let runner = doc
        .get("tasksRunnerOptions")
        .and_then(Value::as_object)
        .is_some_and(|runners| {
            runners.values().any(|runner| {
                matches!(
                    runner.get("runner").and_then(Value::as_str),
                    Some("nx-cloud" | "@nrwl/nx-cloud")
                )
            })
        });
    (named("nxCloudAccessToken") || named("nxCloudId") || runner).then_some(true)
}

/// Whether `pants.toml` or `pants.ci.toml` turns on reading the remote cache: `remote_cache_read
/// = true` in `[GLOBAL]`.
fn pants_toml(root: &Path) -> bool {
    ["pants.toml", "pants.ci.toml"].iter().any(|name| {
        let Ok(text) = std::fs::read_to_string(root.join(name)) else {
            return false;
        };
        let mut global = false;
        text.lines().any(|line| {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.starts_with('[') {
                global = line == "[GLOBAL]";
                return false;
            }
            global
                && line.split_once('=').is_some_and(|(key, value)| {
                    key.trim() == "remote_cache_read" && value.trim() == "true"
                })
        })
    })
}

/// Whether a variable's value says yes, as Pants reads one.
fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

/// Every variable CI's files set, in any `env` or `variables` mapping, with its value as text:
/// a GitHub workflow's, its jobs' and steps', and GitLab's file's and jobs'.
fn variables(files: &[Workflow]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for file in files {
        if let Some(yaml) = variants::parse(&file.path, file.source().as_bytes()) {
            collect(&yaml, &mut found);
        }
    }
    found
}

fn collect(value: &Value, found: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if let ("env" | "variables", Some(set)) = (key.as_str(), value.as_object()) {
                    for (name, value) in set {
                        // GitLab's long form keeps it in `value`.
                        let value = value.get("value").unwrap_or(value);
                        let text = match value {
                            Value::String(text) => text.clone(),
                            Value::Null => String::new(),
                            other => other.to_string(),
                        };
                        found.push((name.clone(), text));
                    }
                }
                collect(value, found);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect(item, found);
            }
        }
        _ => {}
    }
}

/// The JSON file at `path`, comments allowed, as `turbo.json` and `nx.json` take them.
fn json_file(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text)
        .ok()
        .or_else(|| serde_json::from_str(&without_comments(&text)).ok())
}

/// `text` without its `//` and `/* */` comments, which JSON with comments, Groovy and Kotlin
/// write alike; a string's text stays, quotes and all.
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(open) = quote {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                c if c == open => quote = None,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('/', Some('/')) => while chars.next_if(|c| *c != '\n').is_some() {},
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
                out.push(' ');
            }
            ('"' | '\'', _) => {
                quote = Some(c);
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::setup::workflows;
    use crate::cmd::setup::wrapping::{Proposal, ScriptChecks};

    fn words(line: &str) -> Vec<&str> {
        line.split_whitespace().collect()
    }

    /// A repository of `files`, `(path, text)`, and its caches as init reads them.
    fn repository(files: &[(&str, &str)]) -> (tempfile::TempDir, Caches) {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let caches = Caches::read(dir.path(), &workflows::pipelines(dir.path()));
        (dir, caches)
    }

    /// Why init leaves `line`, run at the root of a repository of `files`, to its tool.
    fn why(files: &[(&str, &str)], line: &str) -> Option<String> {
        why_in(files, line, "")
    }

    fn why_in(files: &[(&str, &str)], line: &str, dir: &str) -> Option<String> {
        let (_dir, caches) = repository(files);
        caches.aside(&words(line), dir, true).map(|aside| aside.why)
    }

    /// A workflow of one job, whose `env` lines are `env`.
    fn workflow(env: &str) -> String {
        format!(
            "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n{env}    steps:\n      - uses: actions/checkout@v5\n      - run: echo\n"
        )
    }

    const CI: &str = ".github/workflows/ci.yml";

    // covers: C-15
    #[test]
    fn what_runs_through_a_cached_tool_is_told_by_its_words() {
        for (line, tool) in [
            ("turbo run test", Tool::Turborepo),
            ("turbo test --filter=web", Tool::Turborepo),
            ("npx turbo run test", Tool::Turborepo),
            ("npx --yes turbo@2 run build", Tool::Turborepo),
            ("pnpm turbo run test", Tool::Turborepo),
            ("pnpm exec turbo lint", Tool::Turborepo),
            ("yarn turbo run test", Tool::Turborepo),
            ("npm exec -- turbo run test", Tool::Turborepo),
            ("bun x turbo run test", Tool::Turborepo),
            ("./node_modules/.bin/turbo run test", Tool::Turborepo),
            ("nx run app:test", Tool::Nx),
            ("npx nx affected -t test", Tool::Nx),
            ("pnpm nx run-many --target=test --all", Tool::Nx),
            ("nx affected:test --base=main", Tool::Nx),
            ("./gradlew test", Tool::Gradle),
            ("gradle check --no-daemon", Tool::Gradle),
            ("bazel test //...", Tool::Bazel),
            ("bazelisk build //...", Tool::Bazel),
            ("bazel --bazelrc=ci.bazelrc test //...", Tool::Bazel),
            ("pants test ::", Tool::Pants),
            ("./pants --changed-since=main lint check", Tool::Pants),
        ] {
            assert_eq!(
                tool_of(&words(line)).map(|(tool, _)| tool),
                Some(tool),
                "{line}"
            );
        }
        for line in [
            "turbo",
            "turbo --version",
            "turbo login",
            "turbo prune web",
            "turbo watch test",
            "nx graph",
            "nx format:check",
            "bazel run //:deploy",
            "bazel query //...",
            "bazel version",
            "pants package ::",
            "pnpm test",
            "npm run turbo",
            "cargo test",
            "",
        ] {
            assert_eq!(tool_of(&words(line)).map(|(tool, _)| tool), None, "{line}");
        }
    }

    // covers: C-15
    #[test]
    fn turborepos_cache_is_set_up_by_a_ci_variable_or_turbo_json() {
        let reason = Some(Tool::Turborepo.why());
        let token = "    env:\n      TURBO_TOKEN: ${{ secrets.TURBO_TOKEN }}\n";
        for files in [
            vec![(CI, workflow(token))],
            vec![(CI, workflow("    env:\n      TURBO_TEAM: acme\n"))],
            vec![(CI, format!("env:\n  TURBO_TEAM: ${{{{ vars.TURBO_TEAM }}}}\n{}", workflow("")))],
            vec![(
                ".gitlab-ci.yml",
                "variables:\n  TURBO_TOKEN: $TURBO_TOKEN\ntest:\n  script:\n    - npx turbo run test\n".to_string(),
            )],
            vec![(
                "turbo.json",
                "{\n  // signed, on Vercel\n  \"remoteCache\": {\"signature\": true}\n}\n".to_string(),
            )],
            vec![("turbo.json", "{\"remoteCache\": {\"teamId\": \"team_1\"}}".to_string())],
        ] {
            let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
            assert_eq!(why(&files, "npx turbo run test"), reason, "{files:?}");
        }
        for files in [
            vec![],
            vec![(CI, workflow("    env:\n      TURBO_TOKEN: \"\"\n"))],
            vec![(
                CI,
                workflow("    env:\n      TURBO_TELEMETRY_DISABLED: 1\n"),
            )],
            vec![("turbo.json", "{\"remoteCache\": {}}".to_string())],
            vec![(
                "turbo.json",
                "{\"remoteCache\": {\"enabled\": false, \"teamId\": \"team_1\"}}".to_string(),
            )],
            vec![("turbo.json", "{\"tasks\": {\"test\": {}}}".to_string())],
        ] {
            let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
            assert_eq!(why(&files, "npx turbo run test"), None, "{files:?}");
        }
        // Another tool's cache isn't Turborepo's.
        let files = [(CI, workflow(token))];
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        assert_eq!(why(&files, "nx run app:test"), None);
    }

    // covers: C-15
    #[test]
    fn nx_clouds_cache_is_set_up_by_nx_json_or_a_token() {
        let reason = Some(Tool::Nx.why());
        let token = workflow(
            "    env:\n      NX_CLOUD_ACCESS_TOKEN: ${{ secrets.NX_CLOUD_ACCESS_TOKEN }}\n",
        );
        for files in [
            vec![("nx.json", "{\"nxCloudAccessToken\": \"abc\"}")],
            vec![("nx.json", "{\"nxCloudId\": \"65f0\"}")],
            vec![(
                "nx.json",
                "{\"tasksRunnerOptions\": {\"default\": {\"runner\": \"nx-cloud\"}}}",
            )],
            vec![(
                "nx.json",
                "{\"tasksRunnerOptions\": {\"default\": {\"runner\": \"@nrwl/nx-cloud\"}}}",
            )],
            vec![(CI, token.as_str())],
            vec![
                (CI, token.as_str()),
                ("nx.json", "{\"targetDefaults\": {}}"),
            ],
        ] {
            assert_eq!(why(&files, "npx nx affected -t test"), reason, "{files:?}");
        }
        for files in [
            vec![],
            vec![("nx.json", "{\"nxCloudAccessToken\": \"\"}")],
            vec![(
                "nx.json",
                "{\"tasksRunnerOptions\": {\"default\": {\"runner\": \"nx/tasks-runners/default\"}}}",
            )],
            // Never connecting wins over a token anywhere.
            vec![
                (CI, token.as_str()),
                (
                    "nx.json",
                    "{\"neverConnectToCloud\": true, \"nxCloudId\": \"65f0\"}",
                ),
            ],
        ] {
            assert_eq!(why(&files, "npx nx affected -t test"), None, "{files:?}");
        }
    }

    const REMOTE: &str = "rootProject.name = \"shop\"\n\nbuildCache {\n    local { isEnabled = true }\n    remote<HttpBuildCache> {\n        url = uri(\"https://cache.example.com/cache/\") // the team's\n        isPush = System.getenv(\"CI\") != null\n    }\n}\n";

    // covers: C-15
    #[test]
    fn gradles_cache_is_remote_with_a_remote_build_cache_and_caching_on() {
        let reason = Some(Tool::Gradle.why());
        let on = (
            "gradle.properties",
            "org.gradle.jvmargs=-Xmx2g\norg.gradle.caching = true\n",
        );
        let settings = ("settings.gradle.kts", REMOTE);
        assert_eq!(why(&[settings, on], "./gradlew test"), reason);
        assert_eq!(why(&[settings], "./gradlew test --build-cache"), reason);
        assert_eq!(
            why(&[settings], "./gradlew test -Dorg.gradle.caching=true"),
            reason
        );
        // Caching off, or no remote: a local cache is no cache across machines.
        assert_eq!(why(&[settings], "./gradlew test"), None);
        assert_eq!(
            why(&[settings, on], "./gradlew test --no-build-cache"),
            None
        );
        let local = (
            "settings.gradle.kts",
            "buildCache {\n    local { isEnabled = true }\n}\n",
        );
        assert_eq!(why(&[local, on], "./gradlew test"), None);
        let commented = (
            "settings.gradle",
            "// buildCache { remote(HttpBuildCache) { url = 'https://x' } }\n/* buildCache.remote */\n",
        );
        assert_eq!(why(&[commented, on], "./gradlew test"), None);
        // Groovy, Develocity's connector, and a remote set on the cache itself.
        for text in [
            "buildCache {\n    remote(HttpBuildCache) {\n        url = 'https://cache.example.com/cache/'\n    }\n}\n",
            "plugins {\n    id \"com.gradle.develocity\" version \"3.18\"\n}\nbuildCache {\n    remote(develocity.buildCache) {\n        enabled = true\n    }\n}\n",
            "buildCache.remote(HttpBuildCache::class) {\n    url = uri(\"https://cache.example.com\")\n}\n",
        ] {
            assert_eq!(
                why(&[("settings.gradle", text), on], "./gradlew check"),
                reason,
                "{text}"
            );
        }
        // The Develocity plugin, or a conventions plugin that applies it, with caching on (D7).
        for settings in [
            "plugins {\n    id(\"com.gradle.develocity\") version \"3.18\"\n}\n",
            "plugins {\n\tid \"io.spring.develocity.conventions\" version \"0.0.25\"\n}\n",
            "plugins {\n    id 'com.gradle.enterprise' version '3.16'\n}\n",
        ] {
            let plugin = ("settings.gradle.kts", settings);
            assert_eq!(why(&[plugin, on], "./gradlew test"), reason, "{settings}");
            assert_eq!(why(&[plugin], "./gradlew test"), None, "{settings}");
        }
        // A plugin it names only in a comment isn't applied.
        let commented = (
            "settings.gradle.kts",
            "// id(\"com.gradle.develocity\")\nrootProject.name = \"shop\"\n",
        );
        assert_eq!(why(&[commented, on], "./gradlew test"), None);
        // The build is the nearest directory with a settings file, where it runs or -p says.
        let android = [
            ("android/settings.gradle.kts", REMOTE),
            ("android/gradle.properties", "org.gradle.caching=true\n"),
        ];
        assert_eq!(why_in(&android, "./gradlew test", "android"), reason);
        assert_eq!(why_in(&android, "./gradlew test", "android/app"), reason);
        assert_eq!(
            why(&android, "./gradlew --project-dir=android test"),
            reason
        );
        // The table reads no check in `-p android test`, but the build is found all the same.
        let (dir, _) = repository(&android);
        assert!(gradle(dir.path(), "", &["-p", "android", "test"]));
        assert!(!gradle(dir.path(), "", &["test"]));
        assert_eq!(why(&android, "./gradlew test"), None);
        assert_eq!(why_in(&android, "./gradlew test", "ios"), None);
        assert_eq!(why(&android, "./gradlew -p ../elsewhere test"), None);
    }

    // covers: C-15
    #[test]
    fn a_gradle_command_steps_aside_once_the_table_knows_it() {
        let (_dir, caches) = repository(&[
            ("settings.gradle.kts", REMOTE),
            ("gradle.properties", "org.gradle.caching=true\n"),
        ]);
        let gradle = |line: &str| caches.aside(&words(line), "", true);
        // The table's own Gradle checks, named as it names them.
        assert_eq!(
            gradle("./gradlew test"),
            Some(Aside {
                check: Some("test".into()),
                why: Tool::Gradle.why(),
            })
        );
        assert_eq!(
            gradle("gradle check").unwrap().check.as_deref(),
            Some("check")
        );
        // A task it doesn't know is no check init would wrap: it says nothing of it, as before.
        assert_eq!(gradle("./gradlew assemble"), None);
        // A row the table learned, such as `./gradlew build`, steps aside the same.
        assert_eq!(
            gradle("./gradlew build --scan"),
            Some(Aside {
                check: Some("build".into()),
                why: Tool::Gradle.why(),
            })
        );
        assert_eq!(gradle("./gradlew build --no-build-cache"), None);
        // And a recognizer given by hand decides as the table would.
        let with_assemble = |words: &[&str]| match words {
            ["./gradlew", "assemble", ..] => Some(Verdict::Check("assemble".into())),
            _ => checks::recognize(words, true),
        };
        assert_eq!(
            caches
                .aside_by(&words("./gradlew assemble"), "", &with_assemble)
                .and_then(|aside| aside.check),
            Some("assemble".into())
        );
    }

    // covers: C-15
    #[test]
    fn bazels_cache_is_remote_by_its_bazelrc_or_its_command_line() {
        let reason = Some(Tool::Bazel.why());
        for rc in [
            "build --remote_cache=grpcs://cache.example.com\n",
            "# CI only\nbuild:ci --remote_executor=grpcs://remote.example.com\n",
            "common --bes_backend=grpcs://remote.buildbuddy.io\n",
            "build --remote_cache grpcs://cache.example.com\n",
        ] {
            assert_eq!(why(&[(".bazelrc", rc)], "bazel test //..."), reason, "{rc}");
        }
        for rc in [
            "build --remote_cache=\n",
            "# build --remote_cache=grpcs://cache.example.com\n",
            "build --remote_cache_header=x-team=shop\n",
            "build --disk_cache=~/.cache/bazel\n",
        ] {
            assert_eq!(why(&[(".bazelrc", rc)], "bazel test //..."), None, "{rc}");
        }
        // What it imports, and what --bazelrc names.
        let imports = [
            (
                ".bazelrc",
                "try-import %workspace%/.bazelrc.user\nimport %workspace%/tools/ci.bazelrc\n",
            ),
            (
                "tools/ci.bazelrc",
                "build:ci --remote_cache=grpcs://cache.example.com\n",
            ),
        ];
        assert_eq!(why(&imports, "bazelisk build //..."), reason);
        let named = [(
            "tools/ci.bazelrc",
            "build --remote_cache=grpcs://cache.example.com\n",
        )];
        assert_eq!(
            why(&named, "bazel --bazelrc=tools/ci.bazelrc test //..."),
            reason
        );
        assert_eq!(why(&named, "bazel test //..."), None);
        // On the command line.
        assert_eq!(
            why(
                &[],
                "bazel test --remote_cache=grpcs://cache.example.com //..."
            ),
            reason
        );
        // A workspace of its own, below the root.
        let below = [
            ("server/MODULE.bazel", "module(name = \"server\")\n"),
            (
                "server/.bazelrc",
                "build --remote_cache=grpcs://cache.example.com\n",
            ),
        ];
        assert_eq!(why_in(&below, "bazel test //...", "server/api"), reason);
        assert_eq!(why(&below, "bazel test //..."), None);
    }

    // covers: C-15
    #[test]
    fn pants_reads_its_remote_cache_when_its_config_says_so() {
        let reason = Some(Tool::Pants.why());
        let on = "[GLOBAL]\npants_version = \"2.21.0\"\nremote_cache_read = true # the team's\n";
        assert_eq!(why(&[("pants.toml", on)], "pants test ::"), reason);
        assert_eq!(why(&[("pants.ci.toml", on)], "pants lint ::"), reason);
        let elsewhere = "[GLOBAL]\npants_version = \"2.21.0\"\n[test]\nremote_cache_read = true\n";
        assert_eq!(why(&[("pants.toml", elsewhere)], "pants test ::"), None);
        let off = "[GLOBAL]\nremote_cache_read = false\n";
        assert_eq!(why(&[("pants.toml", off)], "pants test ::"), None);
        let variable = workflow("    env:\n      PANTS_REMOTE_CACHE_READ: \"true\"\n");
        assert_eq!(why(&[(CI, variable.as_str())], "pants test ::"), reason);
        assert_eq!(why(&[], "pants --remote-cache-read test ::"), reason);
        assert_eq!(why(&[], "pants test ::"), None);
    }

    // covers: C-15
    #[test]
    fn a_script_that_runs_through_a_cached_tool_is_the_tools() {
        let token = workflow("    env:\n      TURBO_TOKEN: ${{ secrets.TURBO_TOKEN }}\n");
        let (_dir, caches) = repository(&[(CI, token.as_str())]);
        let reason = Some(Tool::Turborepo.why());
        assert_eq!(caches.aside_script("turbo run test"), reason);
        assert_eq!(
            caches.aside_script("tsc --noEmit && turbo run lint"),
            reason
        );
        assert_eq!(caches.aside_script("vitest run"), None);
        // With no remote cache, it is wrapped as before.
        let (_dir, none) = repository(&[]);
        assert_eq!(none.aside_script("turbo run test"), None);
        assert_eq!(Caches::default().aside_script("turbo run test"), None);
    }

    // covers: C-15
    #[test]
    fn each_tools_reason_is_told_apart_from_any_other() {
        for tool in Tool::ALL {
            assert!(stepped_aside(&tool.why()), "{tool:?}");
        }
        assert_eq!(
            Tool::Turborepo.why(),
            "Turborepo reuses its own results across machines (remote cache), so init leaves it to Turborepo"
        );
        assert!(!stepped_aside("it pipes one command into another (|)"));
        assert!(!stepped_aside(""));
    }

    /// What init makes of `workflow`, in a repository of `files` with no package.json.
    fn proposal_of(files: &[(&str, &str)], workflow: &str) -> Proposal {
        let (dir, _) = repository(files);
        let files = [Workflow::parse(CI.into(), workflow.to_string())];
        Proposal::read(dir.path(), &files, None, true, &ScriptChecks::new())
    }

    // covers: C-15
    #[test]
    fn the_reason_init_gives_is_the_remote_caches_whatever_else_would_leave_the_line() {
        // The token comes from a secret, in the job's env.
        let text = "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    env:\n      TURBO_TOKEN: ${{ secrets.TURBO_TOKEN }}\n    steps:\n      - uses: actions/checkout@v5\n      - run: npx turbo run test\n      - run: npx turbo run lint --filter=$PACKAGE\n      - run: npx turbo run build | tee build.log\n      - run: cargo test\n";
        let dir = tempfile::tempdir().unwrap();
        let files = [Workflow::parse(CI.into(), text.to_string())];
        let proposal = Proposal::read(dir.path(), &files, None, true, &ScriptChecks::new());
        let left: Vec<(&str, &str)> = proposal
            .left
            .iter()
            .map(|left| (left.command.as_str(), left.why.as_str()))
            .collect();
        let why = Tool::Turborepo.why();
        assert_eq!(
            left,
            [
                ("npx turbo run test", why.as_str()),
                ("npx turbo run lint --filter=$PACKAGE", why.as_str()),
                ("npx turbo run build", why.as_str()),
            ]
        );
        // What no remote cache keeps is wrapped as before.
        assert_eq!(proposal.names(), ["test"]);
        // Without a remote cache nothing changes: init knows no Turborepo, and says nothing.
        let without = text.replace(
            "      TURBO_TOKEN: ${{ secrets.TURBO_TOKEN }}\n",
            "      A: b\n",
        );
        let files = [Workflow::parse(CI.into(), without)];
        let proposal = Proposal::read(dir.path(), &files, None, true, &ScriptChecks::new());
        assert!(
            proposal
                .left
                .iter()
                .all(|left| !left.command.contains("turbo run test")),
            "{:?}",
            proposal.left
        );
    }

    // covers: C-15
    #[test]
    fn a_gradle_check_with_a_remote_cache_is_left_and_still_a_check() {
        let settings = [
            ("settings.gradle.kts", REMOTE),
            ("gradle.properties", "org.gradle.caching=true\n"),
        ];
        let text = "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: ./gradlew test\n      - run: ./gradlew check | tee check.log\n";
        let proposal = proposal_of(&settings, text);
        assert!(proposal.is_empty(), "{:?}", proposal.wrapped());
        let left: Vec<(Option<&str>, &str)> = proposal
            .left
            .iter()
            .map(|left| (left.check.as_deref(), left.why.as_str()))
            .collect();
        let why = Tool::Gradle.why();
        assert_eq!(
            left,
            [(Some("test"), why.as_str()), (Some("check"), why.as_str())]
        );
        // Without the remote cache, init wraps it as before.
        let proposal = proposal_of(&[("gradle.properties", "org.gradle.caching=true\n")], text);
        assert_eq!(proposal.names(), ["test"]);
    }

    // covers: C-15
    #[test]
    fn json_with_comments_reads_and_strings_keep_their_slashes() {
        assert_eq!(
            without_comments("{\"a\": \"https://x/*y*/\", // c\n/* d */ \"b\": 1}"),
            "{\"a\": \"https://x/*y*/\", \n  \"b\": 1}"
        );
        assert_eq!(
            without_comments("url = 'http://a' // b"),
            "url = 'http://a' "
        );
        assert_eq!(
            property("a=1\n# org.gradle.caching=true\n", "org.gradle.caching"),
            None
        );
        assert_eq!(
            property("org.gradle.caching true", "org.gradle.caching").as_deref(),
            Some("true")
        );
        assert_eq!(
            property(
                "org.gradle.caching=true\norg.gradle.caching=false\n",
                "org.gradle.caching"
            )
            .as_deref(),
            Some("false")
        );
        assert_eq!(ancestors("a/b"), ["a/b", "a", ""]);
        assert_eq!(ancestors(""), [""]);
        assert_eq!(inside("a", "../b").as_deref(), Some("b"));
        assert_eq!(inside("a", "../../b"), None);
        assert_eq!(inside("", "/etc"), None);
    }
}
