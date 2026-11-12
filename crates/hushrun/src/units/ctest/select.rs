//! \[ADR 0087\] The tests a `ctest` command line runs, as `--show-only=json-v1` lists them; each
//! test's test program and id; which of the tests the plan lists can be left out; and how, on
//! the command line, so that `ctest` leaves out exactly those.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// CMake's regular expressions compile to at most 65,535 bytes, and one that doesn't compile is
/// dropped silently, so that every test runs (ran, CMake 3.22 and 4.4). hushrun's stays well
/// under: about a name's length and 7 bytes a branch (ran, bisected on 3.22).
pub const REGEX_BUDGET: usize = 60_000;
/// The longest argument hushrun gives `ctest`: Linux refuses one of 131,072 bytes (ran).
pub const MOST_ARGUMENT: usize = 100_000;
/// The folders a multi-config generator puts a configuration's programs in.
const CONFIG_DIRS: &[&str] = &["Debug", "Release", "RelWithDebInfo", "MinSizeRel"];

/// `ctest --show-only=json-v1`, the fields hushrun reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Listing {
    #[serde(default)]
    pub tests: Vec<Listed>,
}

/// One test as the listing has it.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
pub struct Listed {
    pub name: String,
    /// Absent when its program isn't built or can't be found.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub properties: Vec<Property>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
pub struct Property {
    pub name: String,
    pub value: serde_json::Value,
}

impl Listed {
    fn property(&self, name: &str) -> Option<&serde_json::Value> {
        self.properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| &p.value)
    }

    /// A property that is a list of words, or one word.
    pub fn words(&self, name: &str) -> Vec<String> {
        match self.property(name) {
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            Some(serde_json::Value::String(one)) => vec![one.clone()],
            _ => Vec::new(),
        }
    }

    /// `DISABLED`: it never runs.
    pub fn disabled(&self) -> bool {
        matches!(
            self.property("DISABLED"),
            Some(serde_json::Value::Bool(true))
        )
    }

    /// The listing's identity of it, which a second listing must give back.
    pub fn identity(&self) -> (String, String) {
        (
            self.name.clone(),
            serde_json::to_string(&self.command).unwrap_or_default(),
        )
    }
}

/// The listing in `stdout`: CTest prints a regular expression it couldn't compile before the
/// JSON (ran), and such a line means it dropped that filter.
pub fn parse_listing(stdout: &[u8]) -> Result<(Listing, Vec<String>), String> {
    let text = String::from_utf8_lossy(stdout);
    let start = if text.starts_with('{') {
        0
    } else {
        text.find("\n{")
            .map(|i| i + 1)
            .ok_or("ctest listed no JSON")?
    };
    let warnings = text[..start]
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    let listing: Listing =
        serde_json::from_str(&text[start..]).map_err(|err| format!("ctest's listing: {err}"))?;
    Ok((listing, warnings))
}

/// What a test's program is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    /// Built in a CMake build directory: its real path and that directory's, and the paths its
    /// test names besides (in its arguments and its `ENVIRONMENT`), which it may start or read.
    Built {
        path: PathBuf,
        build: PathBuf,
        named: Vec<Named>,
    },
    /// Anything else (`sh`, `python3`, a program outside any build directory): what it reads
    /// can't be told.
    Elsewhere(String),
    /// The listing names none: it isn't built, or can't be found.
    None,
}

/// A path a test names beside its program, that exists: what its program may start or read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Named {
    /// A file of a CMake build directory, by its real path and that directory's: a program the
    /// test starts when a build rule makes it (`$<TARGET_FILE:server>`), else a file it reads.
    Built { path: PathBuf, build: PathBuf },
    /// A directory of a build directory: which of its programs the test starts, and whether they
    /// are up to date, can't be told.
    BuildDir(PathBuf),
    /// A directory of the repository: which of its files the test reads can't be told.
    RepoDir(PathBuf),
    /// A file of the repository, from its root: one the test reads.
    Repo(String),
}

/// The paths `listed` names besides its program: each argument after the first, or its value
/// after `=` (`--server=/b/server`), and each value of its `ENVIRONMENT` and
/// `ENVIRONMENT_MODIFICATION`, split as `PATH` is. Only paths that exist, absolute or from where the
/// test runs, in a build directory or the repository at `root`, and not the program itself.
pub fn named(listed: &Listed, program: &Path, root: &Path) -> Vec<Named> {
    let mut words: Vec<String> = Vec::new();
    for arg in listed.command.iter().flatten().skip(1) {
        words.push(arg.clone());
        if let Some((_, value)) = arg.split_once('=') {
            words.push(value.to_string());
        }
    }
    for property in ["ENVIRONMENT", "ENVIRONMENT_MODIFICATION"] {
        for entry in listed.words(property) {
            let value = entry.split_once('=').map_or(entry.as_str(), |(_, v)| v);
            // `NAME=op:value` for a modification.
            let value = match property {
                "ENVIRONMENT_MODIFICATION" => value.split_once(':').map_or(value, |(_, v)| v),
                _ => value,
            };
            words.push(value.to_string());
            if cfg!(unix) {
                words.extend(value.split([':', ';']).map(str::to_string));
            } else {
                words.extend(value.split(';').map(str::to_string));
            }
        }
    }
    // \[ADR 0087\] A relative path is from where the test runs: its WORKING_DIRECTORY, else its
    // own directory, which the listing doesn't give: the program's, and its build directory's.
    let mut bases: Vec<PathBuf> = listed
        .words("WORKING_DIRECTORY")
        .into_iter()
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .collect();
    if bases.is_empty() {
        bases.extend(program.parent().map(Path::to_path_buf));
        bases.extend(build_root(program));
    }
    let mut out: Vec<Named> = Vec::new();
    for word in words {
        let path = Path::new(&word);
        if word.is_empty() || word.starts_with('-') {
            continue;
        }
        let real = if path.is_absolute() {
            std::fs::canonicalize(path).ok()
        } else {
            bases
                .iter()
                .find_map(|base| std::fs::canonicalize(base.join(path)).ok())
        };
        let Some(real) = real else {
            continue;
        };
        if real == program {
            continue;
        }
        let in_build = if real.join("CMakeCache.txt").is_file() {
            Some(real.clone())
        } else {
            build_root(&real)
        };
        let found = if real.is_dir() {
            if in_build.is_some() {
                Some(Named::BuildDir(real))
            } else {
                real.starts_with(root).then_some(Named::RepoDir(real))
            }
        } else if let Some(build) = in_build {
            Some(Named::Built { path: real, build })
        } else {
            real.strip_prefix(root)
                .ok()
                .map(|inside| Named::Repo(slashed(inside)))
        };
        if let Some(found) = found
            && !out.contains(&found)
        {
            out.push(found);
        }
    }
    out
}

/// A listed test with its unit and its id.
#[derive(Debug, Clone)]
pub struct Test {
    pub listed: Listed,
    /// Its test program's name: the program's path from its build directory, without a
    /// configuration's folder or `.exe`, and with the configuration it was built in when the
    /// build names one: `tests/t_add (Release)`.
    pub unit: String,
    /// Its id within its unit.
    pub id: String,
    pub program: Program,
}

/// The configuration of a build directory: its cache's `CMAKE_BUILD_TYPE`, and whether its
/// generator builds several configurations.
#[derive(Debug, Clone, Default)]
pub struct BuildDir {
    pub build_type: String,
    pub multi_config: bool,
    pub generator: String,
    pub make_program: String,
}

/// What a build directory's `CMakeCache.txt` says, the entries hushrun reads.
pub fn build_dir(build: &Path) -> BuildDir {
    let text = std::fs::read(build.join("CMakeCache.txt")).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let value = |key: &str| -> String {
        text.lines()
            .find_map(|line| {
                let (name, rest) = line.split_once(':')?;
                (name == key).then(|| rest.split_once('=').map(|(_, v)| v.trim().to_string()))?
            })
            .unwrap_or_default()
    };
    let generator = value("CMAKE_GENERATOR");
    BuildDir {
        build_type: value("CMAKE_BUILD_TYPE"),
        multi_config: generator.contains("Multi-Config")
            || generator.starts_with("Visual Studio")
            || generator == "Xcode",
        make_program: value("CMAKE_MAKE_PROGRAM"),
        generator,
    }
}

/// The CMake build directory a file is in: the nearest directory above it with a
/// `CMakeCache.txt`.
pub fn build_root(file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d.join("CMakeCache.txt").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Each listed test with its unit and id. `config` is `-C`'s value, which names a multi-config
/// build's configuration; `root` the repository's real root.
pub fn identify(listed: Vec<Listed>, root: &Path, config: Option<&str>) -> Vec<Test> {
    let mut builds: BTreeMap<PathBuf, BuildDir> = BTreeMap::new();
    let mut tests: Vec<Test> = listed
        .into_iter()
        .map(|listed| {
            let (unit, program) = unit_of(&listed, root, config, &mut builds);
            let id = gtest_name(&listed).unwrap_or_else(|| listed.name.clone());
            Test {
                listed,
                unit,
                id,
                program,
            }
        })
        .collect();
    // An id met twice in a unit (one name defined in two directories, one program): the second
    // gets `#2`, the third `#3`. A duplicated name is left out only when every copy is.
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    for test in &mut tests {
        let n = seen
            .entry((test.unit.clone(), test.id.clone()))
            .or_default();
        *n += 1;
        if *n > 1 {
            test.id = format!("{}#{n}", test.id);
        }
    }
    tests
}

/// A test `gtest_discover_tests` made: its `--gtest_filter` names one GoogleTest test exactly,
/// which is its id, since its CTest name changes with the CMake version and the standard library
/// (ran: `Container/0.StartsEmpty` is `Container/std::vector<int,…>.StartsEmpty` on 3.22 and
/// `Container.StartsEmpty<std::vector<int,…>>` on 4.4).
pub fn gtest_name(listed: &Listed) -> Option<String> {
    let command = listed.command.as_ref()?;
    command.iter().skip(1).find_map(|arg| {
        let value = arg.strip_prefix("--gtest_filter=")?;
        (!value.is_empty() && !value.contains(['*', '?', ':', '-'])).then(|| value.to_string())
    })
}

fn unit_of(
    listed: &Listed,
    root: &Path,
    config: Option<&str>,
    builds: &mut BTreeMap<PathBuf, BuildDir>,
) -> (String, Program) {
    let Some(first) = listed.command.as_ref().and_then(|c| c.first()) else {
        return (listed.name.clone(), Program::None);
    };
    let path = Path::new(first);
    let real = path
        .is_absolute()
        .then(|| std::fs::canonicalize(path).ok())
        .flatten();
    let Some(real) = real else {
        return (first.clone(), Program::Elsewhere(first.clone()));
    };
    let Some(build) = build_root(&real) else {
        let name = match real.strip_prefix(root) {
            Ok(inside) => slashed(inside),
            Err(_) => real
                .file_name()
                .map_or_else(|| first.clone(), |n| n.to_string_lossy().into_owned()),
        };
        return (name, Program::Elsewhere(first.clone()));
    };
    let dir = builds
        .entry(build.clone())
        .or_insert_with(|| build_dir(&build))
        .clone();
    let inside = real.strip_prefix(&build).unwrap_or(&real);
    let parts: Vec<String> = inside
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut found_config = None;
    let kept: Vec<&String> = parts
        .iter()
        .enumerate()
        .filter(|(i, part)| {
            let is_config = dir.multi_config
                && *i + 1 < parts.len()
                && (config.is_some_and(|c| c == part.as_str())
                    || CONFIG_DIRS.contains(&part.as_str()));
            if is_config && found_config.is_none() {
                found_config = Some((*part).clone());
            }
            !is_config
        })
        .map(|(_, part)| part)
        .collect();
    let mut name = kept
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("/");
    if let Some(stripped) = name.strip_suffix(".exe") {
        name = stripped.to_string();
    }
    let configuration = if dir.multi_config {
        config.map(str::to_string).or(found_config)
    } else {
        Some(dir.build_type.clone())
    }
    .filter(|c| !c.is_empty());
    if let Some(configuration) = configuration {
        name = format!("{name} ({configuration})");
    }
    let named = named(listed, &real, root);
    (
        name,
        Program::Built {
            path: real,
            build,
            named,
        },
    )
}

/// A path's parts joined with `/`.
pub fn slashed(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The tests to leave out, by their index: of those `planned` says the plan lists, every one
/// that can be, and only those. `planned(test)` is true for a test whose unit `reuse` lists or
/// whose id `reuseTests` lists, and whose unit hushrun's checks let go; `whole(unit)` is true for
/// a unit `reuse` lists, which is left out whole or not at all. Kept although listed:
///
/// - a test whose name ctest can't be told exactly (a line break in it);
/// - a fixture's setup or cleanup that a kept test needs (ran: `-E` removes a setup for real, and
///   the tests that need it then run without it);
/// - every copy of a name when one copy is kept: ctest leaves out by name;
/// - every test of a unit `reuse` lists when one of them is kept.
pub fn left_out(
    tests: &[Test],
    planned: &dyn Fn(&Test) -> bool,
    whole: &dyn Fn(&str) -> bool,
) -> BTreeSet<usize> {
    let mut leave: BTreeSet<usize> = tests
        .iter()
        .enumerate()
        .filter(|(_, t)| planned(t) && !t.listed.name.contains(['\n', '\r']))
        .map(|(i, _)| i)
        .collect();
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut by_unit: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, t) in tests.iter().enumerate() {
        by_name.entry(&t.listed.name).or_default().push(i);
        by_unit.entry(&t.unit).or_default().push(i);
    }
    loop {
        let before = leave.len();
        let needed: BTreeSet<String> = tests
            .iter()
            .enumerate()
            .filter(|(i, _)| !leave.contains(i))
            .flat_map(|(_, t)| t.listed.words("FIXTURES_REQUIRED"))
            .collect();
        leave.retain(|i| {
            let t = &tests[*i];
            let provides = t
                .listed
                .words("FIXTURES_SETUP")
                .into_iter()
                .chain(t.listed.words("FIXTURES_CLEANUP"));
            !provides.into_iter().any(|f| needed.contains(&f))
        });
        for copies in by_name.values() {
            if copies.iter().any(|i| !leave.contains(i)) {
                for i in copies {
                    leave.remove(i);
                }
            }
        }
        for (unit, members) in &by_unit {
            if whole(unit) && members.iter().any(|i| !leave.contains(i)) {
                for i in members {
                    leave.remove(i);
                }
            }
        }
        if leave.len() == before {
            return leave;
        }
    }
}

/// A name as a CMake regular expression that matches it alone (cmsys, Henry Spencer's): a
/// backslash and any character is that character, so each metacharacter is escaped, and the
/// name anchored, since matching is a search (ran: `weird name+(1)[x].*?|$^ \end` is matched by
/// `^weird name\+\(1\)\[x\]\.\*\?\|\$\^ \\end$`). `{` and `}` are no metacharacters.
pub fn anchored(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    out.push('^');
    for c in name.chars() {
        if matches!(
            c,
            '^' | '$' | '.' | '[' | ']' | '|' | '(' | ')' | '*' | '+' | '?' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('$');
    out
}

/// Names as one alternation with no group: CMake 3.22 takes at most 9 (ran: 10 is "Too many
/// parentheses", and the filter is dropped).
pub fn alternation<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    names
        .into_iter()
        .map(anchored)
        .collect::<Vec<_>>()
        .join("|")
}

/// What one alternation of `names`, after `user`'s own expression, costs CMake's compiler: about
/// each name's escaped length and 7 bytes (ran), with the user's counted twice.
pub fn regex_cost<'a>(names: impl IntoIterator<Item = &'a str>, user: Option<&str>) -> usize {
    names
        .into_iter()
        .map(|n| anchored(n).len() + 7)
        .sum::<usize>()
        + user.map_or(0, |u| 2 * u.len() + 1)
        + 16
}

/// How hushrun's `ctest` tells the real one what to leave out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mechanism {
    /// `--exclude-from-file` (CMake 3.29 and later): exact names, one a line.
    ExcludeFile,
    /// One `-E`, the user's (or the preset's) expression joined to the names.
    Exclude,
    /// `-R` with the names kept: ctest adds the fixtures they need itself.
    Include,
}

impl Mechanism {
    pub fn word(&self) -> &'static str {
        match self {
            Mechanism::ExcludeFile => "--exclude-from-file",
            Mechanism::Exclude => "-E",
            Mechanism::Include => "-R",
        }
    }
}

/// What to give the real `ctest` for one mechanism: the options of the user's it replaces, and
/// the words it appends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub mechanism: Mechanism,
    pub drop: Vec<&'static str>,
    pub add: Vec<String>,
}

/// What a mechanism has to go by.
pub struct SelectAt<'a> {
    /// The real ctest's version.
    pub version: (u32, u32, u32),
    /// The names to leave out, and those kept, each once.
    pub left: &'a BTreeSet<String>,
    pub kept: &'a BTreeSet<String>,
    /// The user's last `-E`, or the preset's exclude when none is given.
    pub user_exclude: Option<&'a str>,
    /// The lines of the user's last `--exclude-from-file`, when there is one.
    pub user_file: Option<&'a [String]>,
    /// The preset sets `filter.exclude.fromFile`.
    pub preset_file: bool,
    /// Where hushrun writes its file of names.
    pub file: &'a Path,
}

/// The mechanisms that fit, in the spike's order: `--exclude-from-file` on 3.29 and later (not
/// beside a preset's own file, which a command line's replaces); one merged `-E` while it stays
/// inside the regex budget and the longest argument; `-R` with the names kept, likewise. Each is
/// tried in turn by the caller, which lists again with it and takes the first that gives exactly
/// the tests expected.
pub fn mechanisms(at: &SelectAt<'_>) -> Vec<Selection> {
    let mut out = Vec::new();
    if at.version >= (3, 29, 0) && !at.preset_file {
        let mut text = String::new();
        for line in at.user_file.unwrap_or_default() {
            text.push_str(line);
            text.push('\n');
        }
        for name in at.left {
            text.push_str(name);
            text.push('\n');
        }
        if super::super::facts::write_whole(at.file, text.as_bytes()).is_ok() {
            out.push(Selection {
                mechanism: Mechanism::ExcludeFile,
                drop: vec!["--exclude-from-file"],
                add: vec![
                    "--exclude-from-file".into(),
                    at.file.to_string_lossy().into_owned(),
                ],
            });
        }
    }
    let names = at.left.iter().map(String::as_str);
    if regex_cost(names.clone(), at.user_exclude) <= REGEX_BUDGET {
        let mut regex = alternation(names);
        if let Some(user) = at.user_exclude {
            // A union: a top-level `|` binds loosest.
            regex = format!("{user}|{regex}");
        }
        if regex.len() <= MOST_ARGUMENT {
            out.push(Selection {
                mechanism: Mechanism::Exclude,
                drop: vec!["-E", "--exclude-regex"],
                add: vec!["-E".into(), regex],
            });
        }
    }
    let kept = at.kept.iter().map(String::as_str);
    if !at.kept.is_empty() && regex_cost(kept.clone(), None) <= REGEX_BUDGET {
        let regex = alternation(kept);
        if regex.len() <= MOST_ARGUMENT {
            out.push(Selection {
                mechanism: Mechanism::Include,
                drop: vec!["-R", "--tests-regex"],
                add: vec!["-R".into(), regex],
            });
        }
    }
    out
}

/// `ctest version 3.22.1` as numbers.
pub fn version(stdout: &[u8]) -> Option<(u32, u32, u32)> {
    let text = String::from_utf8_lossy(stdout);
    let rest = text.split("version ").nth(1)?;
    let mut parts = rest
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u32>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(name: &str, props: &[(&str, serde_json::Value)]) -> Listed {
        Listed {
            name: name.into(),
            command: Some(vec![format!("/b/{name}")]),
            properties: props
                .iter()
                .map(|(n, v)| Property {
                    name: (*n).into(),
                    value: v.clone(),
                })
                .collect(),
        }
    }

    fn test(name: &str, unit: &str, props: &[(&str, serde_json::Value)]) -> Test {
        Test {
            listed: listed(name, props),
            unit: unit.into(),
            id: name.into(),
            program: Program::None,
        }
    }

    // covers: C-49
    #[test]
    fn names_are_escaped_as_cmake_reads_them() {
        assert_eq!(
            anchored("weird name+(1)[x].*?|$^ \\end"),
            "^weird name\\+\\(1\\)\\[x\\]\\.\\*\\?\\|\\$\\^ \\\\end$"
        );
        // Braces are literal in CMake's expressions, and stay as they are.
        assert_eq!(anchored("a{2}"), "^a{2}$");
        assert_eq!(anchored("add.x"), "^add\\.x$");
        // Catch2's names as the spike captured them: brackets, a backslash, Unicode, spaces at
        // both ends, all kept.
        assert_eq!(
            anchored(" spaced [tag] a\\b, ü "),
            "^ spaced \\[tag\\] a\\\\b, ü $"
        );
        assert_eq!(
            anchored("templated - std::vector<int>"),
            "^templated - std::vector<int>$"
        );
        assert_eq!(alternation(["a", "b c"]), "^a$|^b c$");
        // No group, whatever the names: 3.22 takes nine.
        let many: Vec<String> = (0..20).map(|i| format!("t({i})")).collect();
        let regex = alternation(many.iter().map(String::as_str));
        assert!(!regex.contains("(?") && regex.matches("\\(").count() == 20);
        assert_eq!(
            version(b"ctest version 3.22.1\n\nCMake suite maintained"),
            Some((3, 22, 1))
        );
        assert_eq!(version(b"ctest version 4.4.5-rc1"), Some((4, 4, 5)));
        assert_eq!(version(b"nothing"), None);
    }

    // covers: C-49
    #[test]
    fn mechanisms_follow_the_version_the_budget_and_the_preset() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("exclude.txt");
        let left: BTreeSet<String> = ["add", " spaced "].iter().map(|s| s.to_string()).collect();
        let kept: BTreeSet<String> = ["mul"].iter().map(|s| s.to_string()).collect();
        let user = vec!["old".to_string()];
        let at = SelectAt {
            version: (4, 4, 5),
            left: &left,
            kept: &kept,
            user_exclude: Some("sleep|crash"),
            user_file: Some(&user),
            preset_file: false,
            file: &file,
        };
        let all = mechanisms(&at);
        assert_eq!(
            all.iter().map(|s| s.mechanism.clone()).collect::<Vec<_>>(),
            [
                Mechanism::ExcludeFile,
                Mechanism::Exclude,
                Mechanism::Include
            ]
        );
        // The user's file's lines come first, and names keep their spaces.
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "old\n spaced \nadd\n"
        );
        assert_eq!(all[1].add, ["-E", "sleep|crash|^ spaced $|^add$"]);
        assert_eq!(all[2].add, ["-R", "^mul$"]);
        // Before 3.29, or beside a preset's own file: no file.
        let old = SelectAt {
            version: (3, 22, 1),
            ..at
        };
        assert_eq!(mechanisms(&old)[0].mechanism, Mechanism::Exclude);
        // Past the budget: -R with the names kept; nothing kept: nothing fits.
        let huge: BTreeSet<String> = (0..2000).map(|i| format!("Suite.Test_{i:040}")).collect();
        let none = BTreeSet::new();
        let at = SelectAt {
            version: (3, 22, 1),
            left: &huge,
            kept: &kept,
            user_exclude: None,
            user_file: None,
            preset_file: true,
            file: &file,
        };
        let fits = mechanisms(&at);
        assert_eq!(fits.len(), 1);
        assert_eq!(fits[0].mechanism, Mechanism::Include);
        let at = SelectAt { kept: &none, ..at };
        assert!(mechanisms(&at).is_empty());
    }

    // covers: C-49
    #[test]
    fn a_fixture_a_kept_test_needs_a_duplicated_name_and_a_whole_unit_stay() {
        let tests = vec![
            test(
                "db_setup",
                "t_db",
                &[("FIXTURES_SETUP", serde_json::json!(["DB"]))],
            ),
            test(
                "db_query",
                "t_db",
                &[("FIXTURES_REQUIRED", serde_json::json!(["DB"]))],
            ),
            test(
                "db_write",
                "t_db",
                &[("FIXTURES_REQUIRED", serde_json::json!("DB"))],
            ),
            test("add", "t_add", &[]),
            test("add", "sub/t_add", &[]),
            test("solo", "t_solo", &[]),
            test("a", "t_whole", &[]),
            test("b\nc", "t_whole", &[]),
        ];
        let planned = |t: &Test| t.listed.name != "db_write";
        let leave = left_out(&tests, &planned, &|unit| unit == "t_whole");
        // db_setup stays for db_write; add's two copies are left out together; t_whole can't be
        // left out whole (a name with a line break), so none of it is.
        let names: Vec<&str> = leave
            .iter()
            .map(|i| tests[*i].listed.name.as_str())
            .collect();
        assert_eq!(names, ["db_query", "add", "add", "solo"]);
        let planned = |t: &Test| t.unit != "sub/t_add";
        let leave = left_out(&tests, &planned, &|_| false);
        assert!(!leave.iter().any(|i| tests[*i].listed.name == "add"));
    }

    // covers: C-49
    #[test]
    fn ids_are_names_but_gtest_filters_and_repeats_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let build = root.join("build");
        std::fs::create_dir_all(build.join("sub")).unwrap();
        std::fs::write(
            build.join("CMakeCache.txt"),
            "CMAKE_BUILD_TYPE:STRING=Release\nCMAKE_GENERATOR:INTERNAL=Ninja\n",
        )
        .unwrap();
        for p in ["calc", "sub/t_add"] {
            std::fs::write(build.join(p), "").unwrap();
        }
        let cmd = |p: &str, rest: &[&str]| {
            let mut c = vec![build.join(p).to_string_lossy().into_owned()];
            c.extend(rest.iter().map(|s| s.to_string()));
            Some(c)
        };
        let listed = vec![
            Listed {
                name: "Container/std::vector<int,std::__1::allocator<int>>.StartsEmpty".into(),
                command: cmd(
                    "calc",
                    &[
                        "--gtest_filter=Container/0.StartsEmpty",
                        "--gtest_also_run_disabled_tests",
                    ],
                ),
                properties: vec![],
            },
            Listed {
                name: "all".into(),
                command: cmd("calc", &["--gtest_filter=*"]),
                properties: vec![],
            },
            Listed {
                name: "add".into(),
                command: cmd("sub/t_add", &[]),
                properties: vec![],
            },
            Listed {
                name: "add".into(),
                command: cmd("sub/t_add", &["2"]),
                properties: vec![],
            },
            Listed {
                name: "lint".into(),
                command: Some(vec!["sh".into(), "lint.sh".into()]),
                properties: vec![],
            },
            Listed {
                name: "t_x_NOT_BUILT".into(),
                command: None,
                properties: vec![],
            },
        ];
        let tests = identify(listed, &root, None);
        let got: Vec<(&str, &str)> = tests
            .iter()
            .map(|t| (t.unit.as_str(), t.id.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("calc (Release)", "Container/0.StartsEmpty"),
                ("calc (Release)", "all"),
                ("sub/t_add (Release)", "add"),
                ("sub/t_add (Release)", "add#2"),
                ("sh", "lint"),
                ("t_x_NOT_BUILT", "t_x_NOT_BUILT"),
            ]
        );
        assert!(matches!(tests[0].program, Program::Built { .. }));
        assert!(matches!(tests[4].program, Program::Elsewhere(_)));
        assert_eq!(tests[5].program, Program::None);
        // A multi-config build: the configuration's folder leaves the name, and comes back after.
        std::fs::write(
            build.join("CMakeCache.txt"),
            "CMAKE_GENERATOR:INTERNAL=Ninja Multi-Config\n",
        )
        .unwrap();
        std::fs::create_dir_all(build.join("Debug")).unwrap();
        std::fs::write(build.join("Debug/t_mul"), "").unwrap();
        let tests = identify(
            vec![Listed {
                name: "mul".into(),
                command: cmd("Debug/t_mul", &[]),
                properties: vec![],
            }],
            &root,
            Some("Debug"),
        );
        assert_eq!(tests[0].unit, "t_mul (Debug)");
    }

    // covers: C-49
    #[test]
    fn a_listing_after_a_regex_ctest_dropped_still_reads_and_says_so() {
        let (listing, warnings) = parse_listing(
            b"RegularExpression::compile(): Expression too big.\nRegularExpression::compile(): Error in compile.\n{\"kind\":\"ctestInfo\",\"tests\":[{\"name\":\"a\",\"command\":[\"/b/a\"],\"properties\":[{\"name\":\"DISABLED\",\"value\":true}]}]}",
        )
        .unwrap();
        assert_eq!(warnings.len(), 2);
        assert_eq!(listing.tests.len(), 1);
        assert!(listing.tests[0].disabled());
        assert!(parse_listing(b"nothing").is_err());
    }
}
