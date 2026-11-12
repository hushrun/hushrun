//! \[ADR 0087\] What a test program is built from, as its build directory records it, and whether
//! it is up to date.
//!
//! - **Ninja:** the build graph, walked from the program with `ninja -t query` (Ninja 1.10 has no
//!   `-t inputs`), and each object's sources and headers as the compiler reported them, from
//!   `ninja -t deps`.
//! - **Makefiles:** every `CMakeFiles/<target>.dir/build.make`'s rules, walked from the program,
//!   and each object's compiler depfile, `<object>.d`, a precompiled header's included (GCC
//!   writes what the header read only there).
//!   Both gave the same files in the spike.
//! - Up to date, decided once for both by the files' own times ([`Walker::judge`]): no file of
//!   the repository one of its objects was built from is newer than that object, and none of its
//!   objects and libraries is newer than it.
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
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Why it isn't known to be up to date, when it isn't: it is older than what it is built
    /// from, and ran old code; or whether it is can't be told.
    pub stale: Option<Stale>,
    /// Why what it is built from can't be told at all, when it can't: it isn't observed.
    pub refused: Option<String>,
    /// How it was read: `ninja`, `makefile` or `every source`.
    pub method: &'static str,
    /// Programs and modules of its build it may start, from the top of the build directory:
    /// those it waits for (`add_dependencies`) and those its objects' compile commands name.
    pub starts: BTreeSet<String>,
}

/// \[ADR 0087\] Why a test program isn't known to be up to date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stale {
    /// It is older than something it is built from: the build would make it again, and its tests
    /// ran old code (C-32's stale build).
    Old(String),
    /// Whether it is up to date can't be told.
    Unknown(String),
}

impl Stale {
    /// Why, in people's words.
    pub fn why(&self) -> &str {
        match self {
            Stale::Old(why) | Stale::Unknown(why) => why,
        }
    }

    /// Of two, the one that says more: a program known to be old is old.
    pub fn either(a: Option<Stale>, b: Option<Stale>) -> Option<Stale> {
        match (a, b) {
            (Some(Stale::Old(why)), _) | (_, Some(Stale::Old(why))) => Some(Stale::Old(why)),
            (a, b) => a.or(b),
        }
    }
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
    /// What each build directory says of its programs beside its graph.
    extras: HashMap<PathBuf, Extras>,
    /// The programs being walked, for a program another starts that starts it in turn.
    walking: BTreeSet<PathBuf>,
    /// Each file's modification time, once read.
    times: HashMap<PathBuf, Option<SystemTime>>,
}

/// What a build directory says of its programs beside its graph.
#[derive(Debug, Clone, Default)]
struct Extras {
    /// The outputs that are programs or modules (an executable's, a module's or a shared
    /// library's link), and those of phony edges (Ninja).
    programs: BTreeSet<String>,
    /// Every link's output: the programs and modules, and the libraries, static ones included.
    linked: BTreeSet<String>,
    phony: BTreeSet<String>,
    /// Each output's compile command's defines and flags: an object's (Ninja's `DEFINES` and
    /// `FLAGS`), or a target directory's (Makefiles' `flags.make`).
    flags: BTreeMap<String, String>,
    /// Makefiles: each target directory's link output, and the target directories it waits for
    /// (`CMakeFiles/Makefile2`).
    links: BTreeMap<String, String>,
    waits: BTreeMap<String, Vec<String>>,
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
        /// Its make compares times by the second (GNU Make 3.81, macOS's).
        whole_seconds: bool,
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
            extras: HashMap::new(),
            walking: BTreeSet::new(),
            times: HashMap::new(),
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
            Some(Dir::Make {
                graph,
                custom,
                whole_seconds,
            }) if graph.contains_key(&target) => {
                let (graph, custom, whole_seconds) =
                    (graph.clone(), custom.clone(), *whole_seconds);
                self.make(&graph, &custom, whole_seconds, build, &target)
            }
            _ => None,
        };
        let mut built = read.unwrap_or_else(|| self.every_source(program));
        // \[ADR 0087\] A program or module it may start: what that is built from, it reads too.
        self.walking.insert(program.to_path_buf());
        for start in std::mem::take(&mut built.starts) {
            let path = build.join(&start);
            if self.walking.contains(&path) {
                continue;
            }
            let other = self.program(&path, build);
            built.reads.extend(other.reads);
            built.every_source |= other.every_source;
            built.stale = Stale::either(built.stale.take(), other.stale);
            built.refused = built.refused.take().or(other.refused);
        }
        self.walking.remove(program);
        built
    }

    /// What a path a test names beside its program adds to what that program reads: a program a
    /// build rule makes, what it is built from; a directory of the repository, every source; a
    /// directory of a build, every source, and each program the build makes there must be up to
    /// date (which of them the test starts can't be told); a file of the repository, itself; a
    /// file of a build directory no rule makes (data the test reads or writes), nothing.
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
            Named::BuildDir { dir, build } => {
                let mut out = Built {
                    every_source: true,
                    method: "every source",
                    ..Built::default()
                };
                match self.programs_in(dir, build) {
                    Some(programs) => {
                        for program in programs {
                            let built = self.program(&build.join(program), build);
                            out.stale = Stale::either(out.stale.take(), built.stale);
                        }
                    }
                    None => {
                        out.stale = Some(Stale::Unknown(format!(
                            "its test names {}, a directory of a build that can't be read, whose programs it may start can't be told up to date",
                            dir.display()
                        )));
                    }
                }
                out
            }
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

    /// The programs and modules the build in `build` makes in `dir` or under it, from the top of
    /// the build directory; `None` when the build can't be read.
    fn programs_in(&mut self, dir: &Path, build: &Path) -> Option<Vec<String>> {
        if !self.dirs.contains_key(build) {
            let opened = self.open(build);
            self.dirs.insert(build.to_path_buf(), opened);
        }
        let extras = self.extras.get(build)?;
        let programs: Vec<String> = match self.dirs.get(build) {
            Some(Dir::Ninja { .. }) => extras.programs.iter().cloned().collect(),
            Some(Dir::Make { .. }) => extras
                .links
                .values()
                .filter(|l| !l.ends_with(".a") && !l.ends_with(".lib"))
                .cloned()
                .collect(),
            _ => return None,
        };
        Some(
            programs
                .into_iter()
                .filter(|p| build.join(p).starts_with(dir))
                .collect(),
        )
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

    fn open(&mut self, build: &Path) -> Dir {
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
                let mut extras = Extras::default();
                let mut outputs = BTreeSet::new();
                for (output, rule) in out.lines().filter_map(|line| line.rsplit_once(": ")) {
                    if rule.contains("EXECUTABLE_LINKER")
                        || rule.contains("MODULE_LIBRARY_LINKER")
                        || rule.contains("SHARED_LIBRARY_LINKER")
                    {
                        extras.programs.insert(output.to_string());
                        extras.linked.insert(output.to_string());
                    } else if rule.contains("STATIC_LIBRARY_LINKER") {
                        extras.linked.insert(output.to_string());
                    } else if rule == "phony" {
                        extras.phony.insert(output.to_string());
                    }
                    outputs.insert(output.to_string());
                }
                extras.flags = ninja_flags(build);
                self.extras.insert(build.to_path_buf(), extras);
                Dir::Ninja {
                    ninja,
                    outputs,
                    depfiled: ninja_depfiled(build),
                }
            }
            "Unix Makefiles" => {
                let graph = make_graph(build);
                self.extras
                    .insert(build.to_path_buf(), make_extras(build, &graph));
                // GNU Make before 4 compares times by the second; a make that can't say is taken
                // for one that does, which says only more.
                let make = PathBuf::from(&dir.make_program);
                let version = run(&make, build, &["--version"], &self.assigned).unwrap_or_default();
                let whole_seconds =
                    !version.contains("GNU Make ") || version.contains("GNU Make 3.");
                Dir::Make {
                    graph,
                    custom: make_custom_depfiles(build),
                    whole_seconds,
                }
            }
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

    /// A program whose build can't be read: it reads every source, and is old when one is newer
    /// than it, though whether it is built from that one can't be told.
    fn every_source(&mut self, program: &Path) -> Built {
        let name = program.file_name().unwrap_or_default().to_string_lossy();
        let stale = match self.time(program) {
            None => Some(format!("{name} can't be read; build before ctest")),
            Some(built) => sources_under(self.root).into_iter().find_map(|source| {
                let changed = self.time(&source)?;
                (newer(changed, built) != Newer::No).then(|| {
                    let rel = source.strip_prefix(self.root).map_or(source.clone(), Path::to_path_buf);
                    format!(
                        "{} changed after {name} was made, and its build can't be read to tell whether it is built from it; if it is, build before ctest",
                        super::select::slashed(&rel)
                    )
                })
            }),
        };
        Built {
            every_source: true,
            stale: stale.map(Stale::Old),
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
        let mut order: Vec<String> = Vec::new();
        let mut objects: Vec<String> = Vec::new();
        let mut direct: BTreeSet<String> = BTreeSet::new();
        let mut configured = false;
        let mut first = true;
        // Each edge's inputs and each record, for whether it is up to date.
        let mut inputs_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut records: BTreeMap<String, Vec<String>> = BTreeMap::new();
        while !frontier.is_empty() {
            let edges = query(ninja, build, &frontier, &self.assigned)?;
            let mut next = Vec::new();
            for node in std::mem::take(&mut frontier) {
                seen.insert(node.clone());
                let Some(edge) = edges.get(&node) else {
                    continue;
                };
                inputs_of.insert(
                    node.clone(),
                    edge.inputs.iter().map(|(i, _)| i.clone()).collect(),
                );
                order.extend(edge.order_only.iter().cloned());
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
                    out.stale = Some(Stale::Unknown(format!(
                        "{object}'s record of its headers is {state}"
                    )));
                }
                records.insert(object.clone(), files.clone());
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
                    let inputs = edges
                        .get(&node)
                        .map(|e| e.inputs.as_slice())
                        .unwrap_or_default();
                    inputs_of.insert(
                        node.clone(),
                        inputs.iter().map(|(i, _)| i.clone()).collect(),
                    );
                    for (input, _) in inputs {
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
        let linked = self
            .extras
            .get(build)
            .map(|e| e.linked.clone())
            .unwrap_or_default();
        let made = |rel: &str| outputs.contains(rel);
        let judged = self.judge(
            build,
            target,
            &Made {
                inputs: &inputs_of,
                records: &records,
                made: &made,
                linked: &linked,
                whole_seconds: false,
            },
        );
        out.stale = Stale::either(judged, out.stale.take());
        if configured {
            let inputs = query(ninja, build, &["build.ninja".to_string()], &self.assigned)?;
            let mut configure = BTreeSet::new();
            for (input, _) in inputs
                .get("build.ninja")
                .map(|e| e.inputs.as_slice())
                .unwrap_or_default()
            {
                if let Place::Repo(rel) = self.place(input, build) {
                    configure.insert(rel);
                }
            }
            let reconfigured = self.reconfigured(build, &configure, target);
            out.stale = Stale::either(out.stale.take(), reconfigured);
            out.reads.extend(configure);
        }
        // \[ADR 0087\] What it waits for, through phony edges, that is a program or a module,
        // and what its objects' compile commands name of those.
        let extras = self.extras.get(build).cloned().unwrap_or_default();
        let mut phony_seen: BTreeSet<String> = BTreeSet::new();
        while let Some(node) = order.pop() {
            if extras.programs.contains(&node) {
                out.starts.insert(node);
            } else if extras.phony.contains(&node) && phony_seen.insert(node.clone()) {
                let edges = query(ninja, build, std::slice::from_ref(&node), &self.assigned)?;
                if let Some(edge) = edges.get(&node) {
                    order.extend(edge.inputs.iter().map(|(i, _)| i.clone()));
                    order.extend(edge.order_only.iter().cloned());
                }
            }
        }
        for object in &asked {
            for path in absolute_paths(extras.flags.get(object).map_or("", String::as_str)) {
                if let Place::Build(rel) = self.place(&path, build)
                    && extras.programs.contains(&rel)
                {
                    out.starts.insert(rel);
                }
            }
        }
        out.starts.remove(target);
        Some(out)
    }

    fn make(
        &mut self,
        graph: &BTreeMap<String, Vec<String>>,
        custom: &BTreeMap<String, String>,
        whole_seconds: bool,
        build: &Path,
        target: &str,
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
        let mut records: BTreeMap<String, Vec<String>> = BTreeMap::new();
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
            records.insert(object.clone(), files.clone());
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
        let extras = self.extras.get(build).cloned().unwrap_or_default();
        let made = |rel: &str| graph.contains_key(rel);
        out.stale = self.judge(
            build,
            target,
            &Made {
                inputs: graph,
                records: &records,
                made: &made,
                linked: &extras.linked,
                whole_seconds,
            },
        );
        if configured {
            let configure = configure_inputs_make(build, self);
            let reconfigured = self.reconfigured(build, &configure, target);
            out.stale = Stale::either(out.stale.take(), reconfigured);
            out.reads.extend(configure);
        }
        // \[ADR 0087\] The targets its own waits for (`CMakeFiles/Makefile2`) that link a
        // program or a module, and those its compile flags name.
        let own_dir = graph
            .get(target)
            .into_iter()
            .flatten()
            .find_map(|d| d.strip_suffix("/link.txt"))
            .map(str::to_string);
        if let Some(dir) = own_dir {
            for waited in extras.waits.get(&dir).into_iter().flatten() {
                if let Some(link) = extras.links.get(waited) {
                    out.starts.insert(link.clone());
                }
            }
            for path in absolute_paths(extras.flags.get(&dir).map_or("", String::as_str)) {
                if let Place::Build(rel) = self.place(&path, build)
                    && extras.links.values().any(|l| *l == rel)
                {
                    out.starts.insert(rel);
                }
            }
        }
        out.starts.remove(target);
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
    /// Order-only inputs: what is built before it, never fed to it (a target it waits for).
    order_only: Vec<String>,
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
                out.insert(
                    name.clone(),
                    Edge {
                        inputs: Vec::new(),
                        order_only: Vec::new(),
                    },
                );
                current = Some(name);
                inputs = false;
            } else if line.starts_with("  input:") {
                inputs = true;
            } else if line.starts_with("  ") && !line.starts_with("    ") {
                inputs = false;
            } else if inputs && let Some(name) = &current {
                let item = line.trim();
                if let Some(order) = item.strip_prefix("|| ") {
                    if let Some(edge) = out.get_mut(name) {
                        edge.order_only.push(order.to_string());
                    }
                    continue;
                }
                if item.starts_with("|@ ") {
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
                // A build of its own inside this one (an ExternalProject's) has its own top: its
                // rules aren't this build's. FetchContent's `_deps/` are this build's.
                if !path.join("CMakeCache.txt").exists() {
                    stack.push(path);
                }
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
                if !path.join("CMakeCache.txt").exists() {
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

/// The absolute paths a compile command's defines and flags hold: `-DCLI_PATH=\"/b/cli\"`.
fn absolute_paths(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '=' | ','))
        .filter(|w| w.starts_with('/') && w.len() > 1)
        .map(str::to_string)
        .collect()
}

/// Each output's `DEFINES` and `FLAGS` in `build.ninja`: what its compile command holds.
fn ninja_flags(build: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read(build.join("build.ninja")).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("build ") {
            current = ninja_outputs(rest);
        } else if !line.starts_with(' ') {
            current.clear();
        } else if let Some((name, value)) = line.trim_start().split_once(" = ")
            && matches!(name, "DEFINES" | "FLAGS")
        {
            for output in &current {
                let flags = out.entry(output.clone()).or_default();
                flags.push(' ');
                flags.push_str(value);
            }
        }
    }
    out
}

/// What a Makefile build says of its programs beside its graph: each target directory's link
/// output (the rule that depends on its `link.txt`), the target directories each waits for
/// (`CMakeFiles/Makefile2`'s `<dir>/all: <dir>/all`), and each one's `flags.make` defines and
/// flags.
fn make_extras(build: &Path, graph: &BTreeMap<String, Vec<String>>) -> Extras {
    let mut extras = Extras::default();
    for (output, deps) in graph {
        if let Some(dir) = deps.iter().find_map(|d| d.strip_suffix("/link.txt")) {
            extras.links.insert(dir.to_string(), output.clone());
            extras.linked.insert(output.clone());
        }
    }
    let text = std::fs::read(build.join("CMakeFiles/Makefile2")).unwrap_or_default();
    for line in String::from_utf8_lossy(&text).lines() {
        if let Some((from, to)) = line.split_once(": ")
            && let (Some(from), Some(to)) =
                (from.strip_suffix("/all"), to.trim().strip_suffix("/all"))
        {
            extras
                .waits
                .entry(from.to_string())
                .or_default()
                .push(to.to_string());
        }
    }
    for dir in extras.links.keys() {
        let text = std::fs::read(build.join(dir).join("flags.make")).unwrap_or_default();
        let flags: String = String::from_utf8_lossy(&text)
            .lines()
            .filter(|l| l.contains("_DEFINES = ") || l.contains("_FLAGS = "))
            .map(|l| format!(" {l}"))
            .collect();
        extras.flags.insert(dir.clone(), flags);
    }
    extras
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

/// Whether one time is later than another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Newer {
    No,
    Yes,
    /// Later, in the same second: a make that compares whole seconds sees no change.
    SameSecond,
}

/// \[ADR 0087\] Whether `a` is later than `b`, strictly, at the coarser of the two times' own
/// resolutions, read from the times themselves: no fraction of a second and an even second is
/// FAT's two seconds, no fraction one second, whole hundredths exFAT's, whole microseconds a
/// microsecond. Two files written in the same tick of the coarser clock are never one newer than
/// the other: a file system that rounds a later write down isn't taken for a change.
fn newer(a: SystemTime, b: SystemTime) -> Newer {
    let (Ok(a), Ok(b)) = (a.duration_since(UNIX_EPOCH), b.duration_since(UNIX_EPOCH)) else {
        return if a > b { Newer::Yes } else { Newer::No };
    };
    let tick = |t: std::time::Duration| -> u128 {
        match t.subsec_nanos() {
            0 if t.as_secs().is_multiple_of(2) => 2_000_000_000,
            0 => 1_000_000_000,
            n if n % 10_000_000 == 0 => 10_000_000,
            n if n % 1_000 == 0 => 1_000,
            _ => 1,
        }
    };
    let tick = tick(a).max(tick(b));
    if a.as_nanos() / tick <= b.as_nanos() / tick {
        Newer::No
    } else if a.as_secs() == b.as_secs() {
        Newer::SameSecond
    } else {
        Newer::Yes
    }
}

/// \[ADR 0087\] What one build says of the files it makes, for [`Walker::judge`].
struct Made<'g> {
    /// Each made file's inputs as its rule names them (Ninja's edge inputs, a Makefile rule's
    /// prerequisites), from the top of the build directory or absolute.
    inputs: &'g BTreeMap<String, Vec<String>>,
    /// Each made file's record of what it read (Ninja's deps log, a depfile).
    records: &'g BTreeMap<String, Vec<String>>,
    /// Whether a rule makes a file of the build directory.
    made: &'g dyn Fn(&str) -> bool,
    /// The links' outputs: programs, modules and libraries.
    linked: &'g BTreeSet<String>,
    /// Its make compares times by the second.
    whole_seconds: bool,
}

impl Walker<'_> {
    /// A file's modification time, `None` when it can't be read.
    fn time(&mut self, path: &Path) -> Option<SystemTime> {
        *self
            .times
            .entry(path.to_path_buf())
            .or_insert_with(|| std::fs::metadata(path).and_then(|m| m.modified()).ok())
    }

    /// \[ADR 0087\] Whether the program `target` of `build` ran old code, the one rule for both
    /// generators: it is old when a file of the repository one of its objects was built from is
    /// newer than that object, or one of its objects (or a library it links) is newer than it.
    /// A file a rule writes is never the newer side itself: it stands for the repository's files
    /// its rule reads, to the end. Times compare strictly, at their own resolution ([`newer`]).
    /// The reason names the file and the object or program, and says what cures it.
    fn judge(&mut self, build: &Path, target: &str, g: &Made<'_>) -> Option<Stale> {
        let Some(linked_at) = self.time(&build.join(target)) else {
            return Some(Stale::Old(format!(
                "{target} isn't built; build before ctest"
            )));
        };
        // Its objects, and the libraries it links, to the end.
        let mut objects: BTreeSet<String> = BTreeSet::new();
        let mut parts: BTreeSet<String> = BTreeSet::new();
        let mut stack = vec![target.to_string()];
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(node) = stack.pop() {
            if !seen.insert(node.clone()) {
                continue;
            }
            for input in g.inputs.get(&node).into_iter().flatten() {
                if bookkeeping(input) {
                    continue;
                }
                if let Place::Build(rel) = self.place(input, build)
                    && (g.made)(&rel)
                {
                    if is_object(&rel) {
                        objects.insert(rel.clone());
                        parts.insert(rel);
                    } else if g.linked.contains(&rel) {
                        parts.insert(rel.clone());
                        stack.push(rel);
                    }
                }
            }
        }
        let cure = |file: &str, newer: Newer, whole_seconds: bool| {
            if newer == Newer::SameSecond && whole_seconds {
                format!(
                    ", in the same second: make compares whole seconds, and won't make it again; touch {file}, then build before ctest"
                )
            } else {
                "; build before ctest".to_string()
            }
        };
        let mut memo: HashMap<String, BTreeSet<String>> = HashMap::new();
        for object in &objects {
            let Some(made_at) = self.time(&build.join(object)) else {
                return Some(Stale::Old(format!(
                    "{object} isn't made; build before ctest"
                )));
            };
            for file in self.tracked(build, object, g, &mut memo) {
                let Some(changed) = self.time(&self.root.join(&file)) else {
                    return Some(Stale::Old(format!(
                        "{file}, which {object} was made from, is gone; build before ctest"
                    )));
                };
                let newer = newer(changed, made_at);
                if newer != Newer::No {
                    return Some(Stale::Old(format!(
                        "{file} changed after {object} was made{}",
                        cure(&file, newer, g.whole_seconds)
                    )));
                }
            }
        }
        for part in &parts {
            let Some(made_at) = self.time(&build.join(part)) else {
                return Some(Stale::Old(format!("{part} isn't made; build before ctest")));
            };
            let newer = newer(made_at, linked_at);
            if newer != Newer::No {
                return Some(Stale::Old(format!(
                    "{part} was made after {target}{}",
                    cure(part, newer, g.whole_seconds)
                )));
            }
        }
        None
    }

    /// The repository's files `node` was made from: those its rule and its record name, and for
    /// each file a rule of the build makes, those that file was made from, to the end.
    fn tracked(
        &mut self,
        build: &Path,
        node: &str,
        g: &Made<'_>,
        memo: &mut HashMap<String, BTreeSet<String>>,
    ) -> BTreeSet<String> {
        if let Some(done) = memo.get(node) {
            return done.clone();
        }
        memo.insert(node.to_string(), BTreeSet::new());
        let names: Vec<String> = g
            .inputs
            .get(node)
            .into_iter()
            .flatten()
            .chain(g.records.get(node).into_iter().flatten())
            .cloned()
            .collect();
        let mut out = BTreeSet::new();
        for name in names {
            if bookkeeping(&name) {
                continue;
            }
            match self.place(&name, build) {
                Place::Repo(rel) => {
                    out.insert(rel);
                }
                Place::Build(rel) if (g.made)(&rel) => {
                    let more = self.tracked(build, &rel, g, memo);
                    out.extend(more);
                }
                _ => {}
            }
        }
        memo.insert(node.to_string(), out.clone());
        out
    }

    /// \[ADR 0087\] For a program that reads a file CMake wrote when it configured the build:
    /// old when a file configuring read (`configure`) is newer than the build's last configure
    /// (`CMakeFiles/cmake.check_cache`, which every configure writes again), since the build
    /// configures again, and may write that file again, before it builds.
    fn reconfigured(
        &mut self,
        build: &Path,
        configure: &BTreeSet<String>,
        target: &str,
    ) -> Option<Stale> {
        let configured_at = self.time(&build.join("CMakeFiles/cmake.check_cache"))?;
        for file in configure {
            let Some(changed) = self.time(&self.root.join(file)) else {
                continue;
            };
            if newer(changed, configured_at) != Newer::No {
                return Some(Stale::Old(format!(
                    "{file} changed after the build was configured, and {target} reads a file configuring wrote; build before ctest"
                )));
            }
        }
        None
    }
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

    // covers: C-49
    #[test]
    fn times_compare_strictly_at_their_own_resolution() {
        let at = |ms: u64| UNIX_EPOCH + std::time::Duration::from_millis(1_800_000_000_000 + ms);
        let ns =
            |ns: u64| UNIX_EPOCH + std::time::Duration::from_nanos(1_800_000_000_000_000_000 + ns);
        // FAT keeps two seconds: a source saved before its object, in the same tick, isn't newer;
        // one saved a tick later is.
        assert_eq!(newer(at(2000), at(2000)), Newer::No);
        assert_eq!(newer(at(2000), at(4000)), Newer::No);
        assert_eq!(newer(at(4000), at(2000)), Newer::Yes);
        // A source of a finer file system against an object FAT rounded down, written after it:
        // the same two seconds, so not newer.
        assert_eq!(newer(ns(3_300_000_001), at(2000)), Newer::No);
        assert_eq!(newer(ns(4_300_000_001), at(2000)), Newer::Yes);
        // A whole second against a finer time compares by the second.
        assert_eq!(newer(at(1700), at(1000)), Newer::No);
        assert_eq!(newer(at(3000), at(1999)), Newer::Yes);
        // exFAT's hundredths: a later write rounded down within one isn't older.
        assert_eq!(newer(ns(1_234_567_891), at(1230)), Newer::No);
        // Finer times compare whole; later in the same second is said, for make 3.81.
        assert_eq!(
            newer(ns(1_700_000_001), ns(1_200_000_001)),
            Newer::SameSecond
        );
        assert_eq!(newer(ns(2_100_000_001), ns(1_900_000_001)), Newer::Yes);
        assert_eq!(newer(ns(1_200_000_001), ns(1_200_000_001)), Newer::No);
    }
}
