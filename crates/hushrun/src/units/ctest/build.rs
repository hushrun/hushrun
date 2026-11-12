//! \[ADR 0087\] What a test program is built from, as its build directory records it, and whether
//! it is up to date.
//!
//! - **Ninja:** the build graph, walked from the program with `ninja -t query` (Ninja 1.10 has no
//!   `-t inputs`), and each object's sources and headers as the compiler reported them, from
//!   `ninja -t deps`. Up to date when `ninja -n <program>` has no work.
//! - **Makefiles:** every `CMakeFiles/<target>.dir/build.make`'s rules, walked from the program,
//!   and each object's compiler depfile, `<object>.d`, a precompiled header's included (GCC
//!   writes what the header read only there). Up to date, as make decides, when no object is older
//!   than a file its depfile names, the program older than no object, and no file it read newer
//!   than the program.
//!   Both gave the same files in the spike.
//! - A header or source a build rule writes (a generator the project builds and runs) reads what
//!   that rule reads, to the end, in both: its script, its template, the program it runs, and that
//!   program's own objects' headers from their records.
//! - A file in the build directory with no rule that makes it was written when CMake configured
//!   the build (`configure_file`): the program reads every file configuring read.
//! - Sources fetched into the build directory (`FetchContent`, under `_deps/`) aren't the
//!   repository's: their identity is their declaration, in a CMake file, in the rest set.
//! - An in-source build (its build directory holds the repository) isn't read: what the build
//!   wrote can't be told from the repository's files, and its programs aren't observed.
//! - With neither readable (another generator, a program the graph doesn't know, an object with
//!   no record of its headers), the program reads every C and C++ file of the scope.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

/// The suffixes of C, C++, Objective-C and CUDA sources and headers: a test program's sources
/// (C-30). Every other file a build names (a CMake file, a template, data) is in the rest set.
pub const SOURCE_KINDS: &[&str] = &[
    ".c", ".cc", ".cpp", ".cxx", ".c++", ".cp", ".C", ".h", ".hh", ".hpp", ".hxx", ".h++", ".H",
    ".inl", ".ipp", ".tpp", ".tcc", ".txx", ".inc", ".def", ".m", ".mm", ".M", ".cu", ".cuh",
    ".ixx", ".cppm", ".mpp",
];

/// Whether `path` is of a source kind.
pub fn is_source(path: &str) -> bool {
    SOURCE_KINDS.iter().any(|s| path.ends_with(s))
}

/// What a test program is built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Built {
    /// Files of the repository, from its root.
    pub reads: BTreeSet<String>,
    /// Of those, the sources of the program's own objects: what it is made of (C-30's units).
    pub own: BTreeSet<String>,
    /// Its build couldn't be read: it reads every C and C++ file of the scope.
    pub every_source: bool,
    /// Why it isn't up to date, when it isn't: it ran old code.
    pub stale: Option<String>,
    /// Why what it is built from can't be told at all, when it can't: it isn't observed.
    pub refused: Option<String>,
    /// How it was read: `ninja`, `makefile` or `every source`.
    pub method: &'static str,
}

/// What reading one build directory keeps between its programs.
pub struct Walker<'a> {
    /// The repository's real root.
    root: &'a Path,
    /// `PATH`, to find `ninja` with when the cache's is gone.
    path: Option<OsString>,
    dirs: HashMap<PathBuf, Dir>,
    real: HashMap<PathBuf, Option<PathBuf>>,
    /// \[C-32\] COMMAND's own `env NAME=value` assignments, which `ninja` gets.
    assigned: Vec<(String, String)>,
}

enum Dir {
    Ninja {
        ninja: PathBuf,
        outputs: BTreeSet<String>,
        /// The outputs of rules with a depfile of their own (a custom command's `DEPFILE`).
        depfiled: BTreeSet<String>,
    },
    Make {
        graph: BTreeMap<String, Vec<String>>,
        /// A custom command's output to its `DEPFILE`, from the top of the build directory.
        custom: BTreeMap<String, String>,
    },
    Unread,
}

/// Where a path the build names is.
enum Place {
    Repo(String),
    Build(String),
    Outside,
}

impl<'a> Walker<'a> {
    pub fn new(
        root: &'a Path,
        path: Option<OsString>,
        assigned: &[(String, String)],
    ) -> Walker<'a> {
        Walker {
            root,
            path,
            dirs: HashMap::new(),
            real: HashMap::new(),
            assigned: assigned.to_vec(),
        }
    }

    /// What `program`, in the build directory `build`, is built from.
    pub fn program(&mut self, program: &Path, build: &Path) -> Built {
        // An in-source build: its build directory holds the repository, so a file the build
        // writes can't be told from one of the repository's, and its records name both alike.
        if self.root.starts_with(build) {
            return Built {
                refused: Some(format!(
                    "its build directory, {}, is the repository's: what it is built from can't be told from the repository's own files; a build directory of its own (cmake -B build) can be read",
                    build.display()
                )),
                method: "every source",
                ..Built::default()
            };
        }
        let target = super::select::slashed(program.strip_prefix(build).unwrap_or(program));
        if !self.dirs.contains_key(build) {
            let dir = self.open(build);
            self.dirs.insert(build.to_path_buf(), dir);
        }
        let read = match self.dirs.get(build) {
            Some(Dir::Ninja {
                ninja,
                outputs,
                depfiled,
            }) if outputs.contains(&target) => {
                let (ninja, outputs, depfiled) = (ninja.clone(), outputs.clone(), depfiled.clone());
                self.ninja(&ninja, &outputs, &depfiled, build, &target)
            }
            Some(Dir::Make { graph, custom }) if graph.contains_key(&target) => {
                let (graph, custom) = (graph.clone(), custom.clone());
                self.make(&graph, &custom, build, &target, program)
            }
            _ => None,
        };
        read.unwrap_or_else(|| self.every_source(program))
    }

    /// What a path a test names beside its program adds to what that program reads: a program a
    /// build rule makes, what it is built from; a directory of the repository, every source; a
    /// directory of a build, every source and a program that can't be told up to date (which of
    /// its programs the test starts can't be told); a file of the repository, itself; a file of a
    /// build directory no rule makes (data the test reads or writes), nothing.
    pub fn named(&mut self, named: &super::select::Named) -> Built {
        use super::select::Named;
        match named {
            Named::Built { path, build } => {
                if self.makes(path, build) {
                    let mut built = self.program(path, build);
                    built.own.clear();
                    built
                } else {
                    Built::default()
                }
            }
            Named::BuildDir(dir) => Built {
                every_source: true,
                stale: Some(format!(
                    "its test names {}, a directory of a build, whose programs it may start can't be told up to date",
                    dir.display()
                )),
                method: "every source",
                ..Built::default()
            },
            Named::RepoDir(_) => Built {
                every_source: true,
                method: "every source",
                ..Built::default()
            },
            Named::Repo(rel) => Built {
                reads: [rel.clone()].into(),
                ..Built::default()
            },
        }
    }

    /// Whether a rule of the build directory `build` makes `path`; true where the build can't be
    /// read, which can't tell.
    fn makes(&mut self, path: &Path, build: &Path) -> bool {
        let target = super::select::slashed(path.strip_prefix(build).unwrap_or(path));
        if !self.dirs.contains_key(build) {
            let dir = self.open(build);
            self.dirs.insert(build.to_path_buf(), dir);
        }
        match self.dirs.get(build) {
            Some(Dir::Ninja { outputs, .. }) => outputs.contains(&target),
            Some(Dir::Make { graph, .. }) => graph.contains_key(&target),
            _ => true,
        }
    }

    fn open(&self, build: &Path) -> Dir {
        let dir = super::select::build_dir(build);
        match dir.generator.as_str() {
            "Ninja" => {
                let ninja = Some(PathBuf::from(&dir.make_program))
                    .filter(|p| p.is_file())
                    .or_else(|| {
                        super::super::program::real(
                            "ninja",
                            self.path.as_deref().unwrap_or_default(),
                        )
                    });
                let Some(ninja) = ninja else {
                    return Dir::Unread;
                };
                let Some(out) = run(&ninja, build, &["-t", "targets", "all"], &self.assigned)
                else {
                    return Dir::Unread;
                };
                let outputs = out
                    .lines()
                    .filter_map(|line| line.rsplit_once(": ").map(|(o, _)| o.to_string()))
                    .collect();
                Dir::Ninja {
                    ninja,
                    outputs,
                    depfiled: ninja_depfiled(build),
                }
            }
            "Unix Makefiles" => Dir::Make {
                graph: make_graph(build),
                custom: make_custom_depfiles(build),
            },
            _ => Dir::Unread,
        }
    }

    /// Where `name`, as the build in `build` spells it, is.
    fn place(&mut self, name: &str, build: &Path) -> Place {
        let joined = if Path::new(name).is_absolute() {
            PathBuf::from(name)
        } else {
            build.join(name)
        };
        let joined = normalize(&joined);
        let real = self
            .real
            .entry(joined.clone())
            .or_insert_with(|| std::fs::canonicalize(&joined).ok())
            .clone()
            .unwrap_or(joined);
        if let Ok(inside) = real.strip_prefix(build) {
            return Place::Build(super::select::slashed(inside));
        }
        if let Ok(inside) = real.strip_prefix(self.root) {
            return Place::Repo(super::select::slashed(inside));
        }
        Place::Outside
    }

    fn every_source(&self, program: &Path) -> Built {
        let stale = newest_source(self.root, program);
        Built {
            every_source: true,
            stale,
            method: "every source",
            ..Built::default()
        }
    }

    fn ninja(
        &mut self,
        ninja: &Path,
        outputs: &BTreeSet<String>,
        depfiled: &BTreeSet<String>,
        build: &Path,
        target: &str,
    ) -> Option<Built> {
        let mut out = Built {
            method: "ninja",
            ..Built::default()
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut frontier = vec![target.to_string()];
        let mut objects: Vec<String> = Vec::new();
        let mut direct: BTreeSet<String> = BTreeSet::new();
        let mut configured = false;
        let mut first = true;
        while !frontier.is_empty() {
            let edges = query(ninja, build, &frontier, &self.assigned)?;
            let mut next = Vec::new();
            for node in std::mem::take(&mut frontier) {
                seen.insert(node.clone());
                let Some(edge) = edges.get(&node) else {
                    continue;
                };
                let object = is_object(&node);
                if object
                    || node.ends_with(".pch")
                    || node.ends_with(".gch")
                    || depfiled.contains(&node)
                {
                    // ran on Linux: what GCC's precompiled header read is in its own record; a
                    // custom command's depfile is in its output's.
                    objects.push(node.clone());
                }
                for (input, explicit) in &edge.inputs {
                    if first && is_object(input) {
                        direct.insert(input.clone());
                    }
                    if seen.contains(input) {
                        continue;
                    }
                    match self.place(input, build) {
                        Place::Repo(rel) => {
                            if object && *explicit && direct.contains(&node) && is_source(&rel) {
                                out.own.insert(rel.clone());
                            }
                            out.reads.insert(rel);
                            seen.insert(input.clone());
                        }
                        Place::Outside => {
                            seen.insert(input.clone());
                        }
                        Place::Build(rel) if outputs.contains(&rel) => next.push(rel),
                        Place::Build(rel) => {
                            // A source CMake wrote when it configured the build, which no rule
                            // makes; but a fetched one, whose identity is its declaration.
                            if object && !fetched(&rel) {
                                configured = true;
                            }
                        }
                    }
                }
            }
            first = false;
            next.sort();
            next.dedup();
            next.retain(|n| !seen.contains(n));
            frontier = next;
        }
        if objects.is_empty() {
            return None;
        }
        // Each object's headers, from its record; a header a build rule writes reads what that
        // rule reads, to the end: its script, its template, and the program it runs, whose own
        // objects' headers are read again from their records, and so on (a generated header the
        // generator's own sources include among them).
        let mut asked: BTreeSet<String> = BTreeSet::new();
        let mut pending = objects;
        while !pending.is_empty() {
            pending.retain(|o| asked.insert(o.clone()));
            if pending.is_empty() {
                break;
            }
            let deps = deps(ninja, build, &pending, &self.assigned)?;
            let mut generated: Vec<String> = Vec::new();
            for object in std::mem::take(&mut pending) {
                let Some((state, files)) = deps.get(&object).filter(|(_, f)| !f.is_empty()) else {
                    // No record of what it included: its headers can't be told.
                    return None;
                };
                if state != "VALID" {
                    out.stale = Some(format!("{object}'s record of its headers is {state}"));
                }
                for file in files {
                    match self.place(file, build) {
                        Place::Repo(rel) => {
                            out.reads.insert(rel);
                        }
                        Place::Outside => {}
                        Place::Build(rel) if outputs.contains(&rel) => generated.push(rel),
                        Place::Build(rel) => configured |= !fetched(&rel) && !cmake_own(&rel),
                    }
                }
            }
            let mut frontier: Vec<String> = generated
                .into_iter()
                .filter(|g| !seen.contains(g))
                .collect();
            frontier.sort();
            frontier.dedup();
            while !frontier.is_empty() {
                let edges = query(ninja, build, &frontier, &self.assigned)?;
                let mut next = Vec::new();
                for node in std::mem::take(&mut frontier) {
                    seen.insert(node.clone());
                    if is_object(&node)
                        || node.ends_with(".pch")
                        || node.ends_with(".gch")
                        || depfiled.contains(&node)
                    {
                        pending.push(node.clone());
                    }
                    for (input, _) in edges
                        .get(&node)
                        .map(|e| e.inputs.as_slice())
                        .unwrap_or_default()
                    {
                        match self.place(input, build) {
                            Place::Repo(rel) => {
                                out.reads.insert(rel);
                            }
                            Place::Outside => {}
                            Place::Build(rel) if outputs.contains(&rel) => {
                                if !seen.contains(&rel) {
                                    next.push(rel);
                                }
                            }
                            Place::Build(rel) => {
                                configured |= !fetched(&rel) && !cmake_own(&rel);
                            }
                        }
                    }
                }
                next.sort();
                next.dedup();
                next.retain(|n| !seen.contains(n));
                frontier = next;
            }
        }
        if configured {
            let inputs = query(ninja, build, &["build.ninja".to_string()], &self.assigned)?;
            for (input, _) in inputs
                .get("build.ninja")
                .map(|e| e.inputs.as_slice())
                .unwrap_or_default()
            {
                if let Place::Repo(rel) = self.place(input, build) {
                    out.reads.insert(rel);
                }
            }
        }
        if out.stale.is_none() {
            match run(ninja, build, &["-n", target], &self.assigned) {
                Some(text) if text.contains("no work to do") => {}
                Some(_) => out.stale = Some("ninja would build it again".into()),
                None => out.stale = Some("ninja can't say whether it is up to date".into()),
            }
        }
        Some(out)
    }

    fn make(
        &mut self,
        graph: &BTreeMap<String, Vec<String>>,
        custom: &BTreeMap<String, String>,
        build: &Path,
        target: &str,
        program: &Path,
    ) -> Option<Built> {
        let mut out = Built {
            method: "makefile",
            ..Built::default()
        };
        let direct: BTreeSet<String> = graph
            .get(target)
            .into_iter()
            .flatten()
            .filter(|d| is_object(d))
            .cloned()
            .collect();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut stack = vec![target.to_string()];
        let mut objects: Vec<String> = Vec::new();
        let mut configured = false;
        while let Some(node) = stack.pop() {
            if !seen.insert(node.clone()) {
                continue;
            }
            let object = is_object(&node);
            if object
                || node.ends_with(".pch")
                || node.ends_with(".gch")
                || custom.contains_key(&node)
            {
                objects.push(node.clone());
            }
            for dep in graph.get(&node).into_iter().flatten() {
                if bookkeeping(dep) {
                    continue;
                }
                match self.place(dep, build) {
                    Place::Repo(rel) => {
                        if object && direct.contains(&node) && is_source(&rel) {
                            out.own.insert(rel.clone());
                        }
                        out.reads.insert(rel);
                    }
                    Place::Outside => {}
                    Place::Build(rel) if graph.contains_key(&rel) => stack.push(rel),
                    Place::Build(rel) => {
                        configured |= object && !fetched(&rel) && !cmake_own(&rel);
                    }
                }
            }
        }
        if objects.is_empty() {
            return None;
        }
        // Each object's headers, from its depfile; a header a build rule writes reads what that
        // rule reads, to the end: its script, its template, and the program it runs, whose own
        // objects' depfiles are read in turn.
        let mut asked: BTreeSet<String> = BTreeSet::new();
        let mut stale: Option<String> = None;
        let mut made_files: Vec<PathBuf> = Vec::new();
        let mut pending = objects;
        while let Some(object) = pending.pop() {
            if !asked.insert(object.clone()) {
                continue;
            }
            // A custom command's own depfile, else the compiler's beside the object.
            let depfile = match custom.get(&object) {
                Some(depfile) => build.join(depfile),
                None => build.join(format!("{object}.d")),
            };
            let files = depfile_deps(&depfile)?;
            // As make decides: the object (or the custom command's output) is older than a file
            // its depfile names, so the build would make it again, and the program ran old code.
            if stale.is_none() {
                let made = build.join(&object);
                stale = newer_than(&made, files.iter().map(|f| build.join(f)))
                    .map(|why| format!("{why}, after {object} was made"));
            }
            made_files.push(build.join(&object));
            for file in files {
                match self.place(&file, build) {
                    Place::Repo(rel) => {
                        out.reads.insert(rel);
                    }
                    Place::Outside => {}
                    Place::Build(rel) if graph.contains_key(&rel) => {
                        let mut stack = vec![rel];
                        while let Some(node) = stack.pop() {
                            if !seen.insert(node.clone()) {
                                continue;
                            }
                            if is_object(&node)
                                || node.ends_with(".pch")
                                || node.ends_with(".gch")
                                || custom.contains_key(&node)
                            {
                                pending.push(node.clone());
                            }
                            for dep in graph.get(&node).into_iter().flatten() {
                                if bookkeeping(dep) {
                                    continue;
                                }
                                match self.place(dep, build) {
                                    Place::Repo(r) => {
                                        out.reads.insert(r);
                                    }
                                    Place::Outside => {}
                                    Place::Build(r) if graph.contains_key(&r) => stack.push(r),
                                    Place::Build(r) => {
                                        configured |= !fetched(&r) && !cmake_own(&r);
                                    }
                                }
                            }
                        }
                    }
                    Place::Build(rel) => {
                        configured |= !fetched(&rel) && !cmake_own(&rel);
                    }
                }
            }
        }
        if configured {
            out.reads.extend(configure_inputs_make(build, self));
        }
        out.stale = stale
            .or_else(|| newer_than(program, made_files.into_iter()))
            .or_else(|| newer_than(program, out.reads.iter().map(|r| self.root.join(r))));
        Some(out)
    }
}

/// A source `FetchContent` put in the build directory: not the repository's, and its identity
/// is its declaration, in a CMake file of the rest set.
fn fetched(rel: &str) -> bool {
    rel.starts_with("_deps/") || rel.contains("/_deps/")
}

/// A file CMake writes for a precompiled header or a unity build, whose contents the compiler's
/// record names.
fn cmake_own(rel: &str) -> bool {
    rel.contains("/cmake_pch") || rel.contains("/Unity/unity_")
}

fn is_object(name: &str) -> bool {
    name.ends_with(".o") || name.ends_with(".obj")
}

/// A file of CMake's own that every object of a Makefile build depends on.
fn bookkeeping(dep: &str) -> bool {
    let file = dep.rsplit('/').next().unwrap_or(dep);
    matches!(
        file,
        "flags.make"
            | "build.make"
            | "link.txt"
            | "compiler_depend.ts"
            | "depend.make"
            | "compiler_depend.make"
            | "progress.make"
            | "cmake_force"
    ) || file.ends_with(".rsp")
}

/// One build edge as `ninja -t query` says it: its inputs, explicit or implicit (order-only
/// inputs are left out: they don't feed what it makes).
struct Edge {
    inputs: Vec<(String, bool)>,
}

fn query(
    ninja: &Path,
    build: &Path,
    targets: &[String],
    assigned: &[(String, String)],
) -> Option<BTreeMap<String, Edge>> {
    let mut out = BTreeMap::new();
    for chunk in targets.chunks(500) {
        let mut args = vec!["-t".to_string(), "query".to_string()];
        args.extend(chunk.iter().cloned());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let text = run(ninja, build, &args, assigned)?;
        let mut current: Option<String> = None;
        let mut inputs = false;
        for line in text.lines() {
            if !line.starts_with(' ') {
                let name = line.strip_suffix(':').unwrap_or(line).to_string();
                out.insert(name.clone(), Edge { inputs: Vec::new() });
                current = Some(name);
                inputs = false;
            } else if line.starts_with("  input:") {
                inputs = true;
            } else if line.starts_with("  ") && !line.starts_with("    ") {
                inputs = false;
            } else if inputs && let Some(name) = &current {
                let item = line.trim();
                if item.starts_with("|| ") || item.starts_with("|@ ") {
                    continue;
                }
                let (item, explicit) = match item.strip_prefix("| ") {
                    Some(implicit) => (implicit, false),
                    None => (item, true),
                };
                if let Some(edge) = out.get_mut(name) {
                    edge.inputs.push((item.to_string(), explicit));
                }
            }
        }
    }
    Some(out)
}

/// Each object's record in `ninja -t deps`: its state and the files it read.
fn deps(
    ninja: &Path,
    build: &Path,
    objects: &[String],
    assigned: &[(String, String)],
) -> Option<BTreeMap<String, (String, Vec<String>)>> {
    let mut out: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for chunk in objects.chunks(500) {
        let mut args = vec!["-t".to_string(), "deps".to_string()];
        args.extend(chunk.iter().cloned());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let text = run(ninja, build, &args, assigned)?;
        let mut current: Option<String> = None;
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            if !line.starts_with(' ') {
                current = None;
                let Some((name, rest)) = line.split_once(": #deps ") else {
                    continue;
                };
                let state = rest
                    .rsplit_once('(')
                    .and_then(|(_, s)| s.strip_suffix(')'))
                    .unwrap_or("")
                    .to_string();
                out.insert(name.to_string(), (state, Vec::new()));
                current = Some(name.to_string());
            } else if let Some(name) = &current
                && let Some(entry) = out.get_mut(name)
            {
                entry.1.push(line.trim().to_string());
            }
        }
    }
    Some(out)
}

/// `program`'s output in `dir`, when it exits 0, with COMMAND's environment and its own
/// assignments (`assigned`).
fn run(program: &Path, dir: &Path, args: &[&str], assigned: &[(String, String)]) -> Option<String> {
    let output = super::run::tool(program, dir, assigned)
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Every rule of every `CMakeFiles/<target>.dir/build.make` under `build`: `out: in`, paths from
/// the top of the build directory.
pub fn make_graph(build: &Path) -> BTreeMap<String, Vec<String>> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut stack = vec![build.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if path.file_name().is_some_and(|n| n == "_deps") && dir == build {
                    continue;
                }
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "build.make")
                && path
                    .parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.file_name())
                    .is_some_and(|n| n == "CMakeFiles")
            {
                let text = std::fs::read(&path).unwrap_or_default();
                for line in String::from_utf8_lossy(&text).lines() {
                    if line.is_empty()
                        || line.starts_with(['\t', '#', ' ', '.', '$', '%'])
                        || line.contains("$(")
                    {
                        continue;
                    }
                    let Some((target, dep)) = split_rule(line) else {
                        continue;
                    };
                    if target.contains('=') {
                        continue;
                    }
                    graph
                        .entry(unescape_make(target))
                        .or_default()
                        .push(unescape_make(dep.trim()));
                }
            }
        }
    }
    graph
}

/// \[ADR 0087\] The outputs of `build.ninja`'s edges that set a `depfile` of their own: a custom
/// command's `DEPFILE`, whose inputs are in Ninja's record of the output (`deps = gcc`).
fn ninja_depfiled(build: &Path) -> BTreeSet<String> {
    let text = std::fs::read(build.join("build.ninja")).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let mut out = BTreeSet::new();
    let mut current: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("build ") {
            current = ninja_outputs(rest);
        } else if !line.starts_with(' ') {
            current.clear();
        } else if line.trim_start().starts_with("depfile =") {
            out.extend(current.iter().cloned());
        }
    }
    out
}

/// The outputs of a `build` line, its text after `build `: up to its first `:` that isn't
/// escaped, implicit outputs after `|` included, those named from `${cmake_ninja_workdir}` left
/// out (the same files by another name).
fn ninja_outputs(rest: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '$' => match chars.next() {
                Some(next @ (' ' | ':' | '$')) => word.push(next),
                Some(next) => {
                    word.push('$');
                    word.push(next);
                }
                None => {}
            },
            ':' => break,
            ' ' => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            other => word.push(other),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
        .into_iter()
        .filter(|w| w != "|" && !w.starts_with("${"))
        .collect()
}

/// \[ADR 0087\] A Makefile build's custom commands with a `DEPFILE`: each output, to its depfile,
/// from every `CMakeFiles/<target>.dir/DependInfo.cmake`'s `CMAKE_DEPENDS_DEPENDENCY_FILES`
/// (`"" "<output>" "custom" "<depfile>"`).
fn make_custom_depfiles(build: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![build.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !(path.file_name().is_some_and(|n| n == "_deps") && dir == build) {
                    stack.push(path);
                }
                continue;
            }
            if path.file_name().is_none_or(|n| n != "DependInfo.cmake") {
                continue;
            }
            let text = std::fs::read(&path).unwrap_or_default();
            let text = String::from_utf8_lossy(&text);
            let Some(start) = text.find("set(CMAKE_DEPENDS_DEPENDENCY_FILES") else {
                continue;
            };
            let body = &text[start..];
            let body = &body[..body.find("\n  )").unwrap_or(body.len())];
            for line in body.lines() {
                let quoted: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
                if let [_, output, "custom", depfile] = quoted.as_slice() {
                    out.insert(output.to_string(), depfile.to_string());
                }
            }
        }
    }
    out
}

/// `out: in` at its first `: ` that isn't escaped.
fn split_rule(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b':' && bytes[i + 1] == b' ' {
            return Some((&line[..i], &line[i + 2..]));
        }
        i += 1;
    }
    None
}

fn unescape_make(text: &str) -> String {
    text.replace("\\ ", " ")
        .replace("\\#", "#")
        .replace("$$", "$")
}

/// The files a compiler depfile names, every rule's; `None` when there is none, or it is empty.
pub fn depfile_deps(path: &Path) -> Option<Vec<String>> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes)
        .replace("\\\r\n", " ")
        .replace("\\\n", " ");
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((_, rhs)) = split_depfile_rule(line) else {
            continue;
        };
        let mut word = String::new();
        let mut chars = rhs.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&' ') => {
                    word.push(' ');
                    chars.next();
                }
                '\\' if chars.peek() == Some(&'#') => {
                    word.push('#');
                    chars.next();
                }
                '$' if chars.peek() == Some(&'$') => {
                    word.push('$');
                    chars.next();
                }
                ' ' | '\t' => {
                    if !word.is_empty() {
                        out.push(std::mem::take(&mut word));
                    }
                }
                other => word.push(other),
            }
        }
        if !word.is_empty() {
            out.push(word);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A depfile's rule, `targets: prerequisites`, at its first `:` followed by a space or the line's
/// end.
fn split_depfile_rule(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b':'
            && (i + 1 == bytes.len() || bytes[i + 1] == b' ' || bytes[i + 1] == b'\t')
        {
            return Some((&line[..i], &line[i + 1..]));
        }
        i += 1;
    }
    None
}

/// What configuring a Makefile build read: `CMAKE_MAKEFILE_DEPENDS` in
/// `CMakeFiles/Makefile.cmake`, the repository's files of it.
fn configure_inputs_make(build: &Path, walker: &mut Walker<'_>) -> BTreeSet<String> {
    let text = std::fs::read(build.join("CMakeFiles/Makefile.cmake")).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let mut out = BTreeSet::new();
    let Some(start) = text.find("set(CMAKE_MAKEFILE_DEPENDS") else {
        return out;
    };
    let body = &text[start..];
    let body = &body[..body.find(')').unwrap_or(body.len())];
    for quoted in body.split('"').skip(1).step_by(2) {
        if let Place::Repo(rel) = walker.place(quoted, build) {
            out.insert(rel);
        }
    }
    out
}

/// Why `program` is older than one of `inputs`, when it is.
fn newer_than(program: &Path, inputs: impl Iterator<Item = PathBuf>) -> Option<String> {
    let built = match std::fs::metadata(program).and_then(|m| m.modified()) {
        Ok(built) => built,
        Err(_) => return Some("it can't be read".into()),
    };
    for input in inputs {
        match std::fs::metadata(&input).and_then(|m| m.modified()) {
            Ok(changed) if changed > built => {
                return Some(format!(
                    "{} changed after it was built",
                    input.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            Ok(_) => {}
            Err(_) => {
                return Some(format!(
                    "{} it was built from is gone",
                    input.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
    }
    None
}

/// Why `program` may be older than a C or C++ file of the repository, when it is: for a build
/// hushrun can't read, any of them.
fn newest_source(root: &Path, program: &Path) -> Option<String> {
    newer_than(program, sources_under(root).into_iter())
}

/// Every C and C++ file under `root`, but in `.git` and in build directories.
fn sources_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir.join("CMakeCache.txt").exists() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            if kind.is_dir() {
                if name != ".git" {
                    stack.push(path);
                }
            } else if kind.is_file() && is_source(&name.to_string_lossy()) {
                out.push(path);
            }
        }
    }
    out
}

/// `a/b/../c` as `a/c`, without asking the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-49
    #[test]
    fn depfiles_and_build_make_rules_read_as_make_writes_them() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("t.c.o.d");
        std::fs::write(
            &d,
            "CMakeFiles/t.dir/t.c.o: \\\n  /repo/t.c /repo/my\\ dir/a.h \\\n  /usr/include/stdio.h\n/repo/a.h:\n",
        )
        .unwrap();
        assert_eq!(
            depfile_deps(&d).unwrap(),
            ["/repo/t.c", "/repo/my dir/a.h", "/usr/include/stdio.h"]
        );
        std::fs::write(&d, "").unwrap();
        assert!(depfile_deps(&d).is_none());
        assert_eq!(
            split_rule("CMakeFiles/t\\ x.dir/a.c.o: ../a b.c"),
            Some(("CMakeFiles/t\\ x.dir/a.c.o", "../a b.c"))
        );
        assert!(split_rule("t_OBJECTS = \\").is_none());
        assert!(bookkeeping("CMakeFiles/t.dir/flags.make"));
        assert!(!bookkeeping("../t.c"));
        assert!(is_source("a/b.hpp") && is_source("x.mm") && !is_source("CMakeLists.txt"));
    }
}
