//! Location roots and their placeholders (B-8), the remap flags (B-9) and what to search
//! outputs for (B-10).
//!
//! A root is a folder a compile's paths start from: the workspace (rustc's working directory),
//! the package (`CARGO_MANIFEST_DIR`), the build script's output (`OUT_DIR`), the output folder
//! (`--out-dir`), each `-L dependency=` folder, each `-L native=`, `all=` or bare folder that
//! is not under a shared folder, and the locations cargo or the person hand a compile: the
//! target folder `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` names, `CARGO_TARGET_TMPDIR`
//! and the folders of the `CARGO_BIN_EXE_*` values. Each has a given form, as cargo wrote it, and
//! a resolved form, with symlinks resolved (`/tmp` is `/private/tmp` on macOS, and some macros
//! canonicalize). A path under either form is recorded as the root's name and the rest.
//!
//! Two more folders are **bases**: the cargo home and the toolchain. A path under one is recorded
//! from it as from a root (`{hushrun:cargo-home}/registry/src/…`), so that keys and hints name no
//! machine's folder, and a registry or git crate's compile has the cargo home remapped to a fixed
//! stand-in. Unlike a root, a base is the same for every checkout on a machine: it never ties a
//! block and is not what B-10 searches for to decide its place. Together with the rustup home
//! and the home folder, the bases are what an output is searched for to decide whether the block
//! may leave the machine ([`Roots::machine_needles`]).
//!
//! The constructor is the one place this module touches the file system: it resolves each root
//! once, through a resolver the caller passes, so tests pass their own.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::invocation::{Invocation, SearchKind};
use crate::model::{Folder, Located, RootName, Rooted};

/// What a placeholder starts with. A placeholder is `{hushrun:<root name>}`; `{hushrun:}` is
/// the text `{hushrun:` itself, so text that already looks like a placeholder stays distinct.
const OPEN: &str = "{hushrun:";

/// The stand-in `--remap-path-prefix` writes for a root (B-9), in one table: a measurement may
/// change the recipe. The workspace becomes `.`, as cargo's own `trim-paths` does. The others
/// are absolute paths under `/hushrun`, which no real checkout uses, one per root; the build
/// script's output and the package carry the package's `name-version` so two packages never
/// share one.
pub fn stand_in(root: RootName, package_id: &str) -> String {
    match root {
        RootName::Workspace => ".".to_string(),
        RootName::Package => format!("/hushrun/package/{package_id}"),
        RootName::ScriptOut => format!("/hushrun/out/{package_id}"),
        RootName::OutDir => "/hushrun/out-dir".to_string(),
        RootName::Deps(i) => format!("/hushrun/dependency/{i}"),
        RootName::Native(i) => format!("/hushrun/native/{i}"),
        RootName::TargetDir => "/hushrun/target-dir".to_string(),
        RootName::BuildTargetDir => "/hushrun/build-target-dir".to_string(),
        RootName::TargetTmp => "/hushrun/target-tmpdir".to_string(),
        RootName::BinExe(i) => format!("/hushrun/bin-exe/{i}"),
        RootName::CargoHome => "/hushrun/cargo".to_string(),
        RootName::Toolchain => "/hushrun/toolchain".to_string(),
    }
}

/// The folders that name this machine. The cargo home and the toolchain are bases (recorded
/// like roots); all four are searched for in outputs, in both forms, to decide whether a block
/// may leave the machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Machine {
    /// The cargo home: `CARGO_HOME`, else `~/.cargo`.
    pub cargo_home: Option<String>,
    /// The toolchain's folder: the compiler's sysroot (B-17).
    pub toolchain: Option<String>,
    /// The sysroot without sources compiles are given (B-9, B-17): its links lead into the
    /// toolchain, so a path under it is recorded as one under the toolchain, and recorded paths
    /// are looked up under the toolchain itself.
    pub toolchain_view: Option<String>,
    /// The rustup home: `RUSTUP_HOME`, else `~/.rustup`. Not a base: only searched for.
    pub rustup_home: Option<String>,
    /// The home folder, `HOME`. Not a base: only searched for.
    pub home: Option<String>,
}

/// Why the roots of a call can't be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RootsError {
    /// The working directory must be absolute: every relative path starts from it.
    #[error("the working directory {0:?} is not absolute")]
    RelativeWorkspace(String),
    /// A root or shared folder of `/` would hold every path.
    #[error("{0:?} is the file system's root")]
    TooBroad(String),
    /// More `-L dependency=` folders than a root name can number.
    #[error("more than 256 dependency folders")]
    TooManyDeps,
    /// More folders of `CARGO_BIN_EXE_*` values than a root name can number.
    #[error("more than 256 folders of programs")]
    TooManyBinFolders,
    /// A native folder whose place among the `-L` flags a root name can't number.
    #[error("a native folder after the 256th -L flag")]
    TooManySearchFolders,
    /// A placeholder names a root this call doesn't have.
    #[error("{0:?} names no root of this call")]
    UnknownRoot(String),
    /// A placeholder without its closing brace.
    #[error("a placeholder is not closed")]
    Unterminated,
}

/// The folders a call's roots come from, before they are resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootPaths {
    /// rustc's working directory. Always absolute.
    pub workspace: String,
    /// `CARGO_MANIFEST_DIR`.
    pub package: Option<String>,
    /// `OUT_DIR`.
    pub script_out: Option<String>,
    /// `--out-dir`.
    pub out_dir: String,
    /// The `-L dependency=` folders, in order.
    pub deps: Vec<String>,
    /// The `-L native=`, `-L all=` and bare `-L` folders, each with its place among all the
    /// `-L` flags.
    pub native: Vec<(usize, String)>,
    /// The package's `name-version`, for the stand-ins.
    pub package_id: String,
    /// `CARGO_TARGET_DIR`, when absolute.
    pub target_dir: Option<String>,
    /// `CARGO_BUILD_TARGET_DIR`, when absolute.
    pub build_target_dir: Option<String>,
    /// `CARGO_TARGET_TMPDIR`, when absolute.
    pub target_tmpdir: Option<String>,
    /// The folders of the `CARGO_BIN_EXE_*` values, in the order of the variables' names.
    pub bin_exe_dirs: Vec<String>,
}

impl RootPaths {
    /// The folders of one call.
    pub fn of(invocation: &Invocation) -> RootPaths {
        let package = invocation.package();
        let package_id = match (&package.name, &package.version) {
            (Some(name), Some(version)) => format!("{name}-{version}"),
            (Some(name), None) => name.clone(),
            _ => "_".to_string(),
        };
        RootPaths {
            workspace: invocation.cwd().to_string(),
            package: package.manifest_dir.clone(),
            script_out: package.out_dir.clone(),
            out_dir: invocation.out_dir().to_string(),
            deps: invocation.dependency_dirs().map(str::to_string).collect(),
            native: invocation
                .search_paths()
                .iter()
                .enumerate()
                .filter(|(_, search)| matches!(search.kind, SearchKind::Native | SearchKind::All))
                .map(|(i, search)| (i, search.path.clone()))
                .collect(),
            package_id,
            target_dir: package.target_dir.clone(),
            build_target_dir: package.build_target_dir.clone(),
            target_tmpdir: package.target_tmpdir.clone(),
            bin_exe_dirs: package.bin_exe_dirs.clone(),
        }
    }
}

/// One root, in both forms.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Root {
    name: RootName,
    given: String,
    resolved: String,
}

/// The roots of one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    /// The working directory's given form, root or not: relative paths start here.
    workspace: String,
    roots: Vec<Root>,
    /// The bases: the cargo home and the toolchain, when known.
    bases: Vec<Root>,
    /// Each distinct text of a root's or base's forms with the one it stands for, longest
    /// first.
    forms: Vec<(String, RootName)>,
    /// The folders that name this machine, each in its distinct forms (B-10).
    machine: Vec<(Folder, String)>,
    /// The shared folders in both forms, longest first.
    shared: Vec<String>,
    package_id: String,
}

/// Resolves a folder on disk: symlinks followed, `.` and `..` gone. `None` when it can't be
/// resolved or isn't UTF-8; the root then has one form.
pub fn resolve_on_disk(path: &str) -> Option<String> {
    std::fs::canonicalize(path)
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
}

impl Roots {
    /// The roots of a call. `shared` are the folders that are the same for every checkout on
    /// this machine: cargo home, rustup home, the sysroot, system folders. The workspace and
    /// the package are roots only when they are not under one, or under a base. `machine`
    /// names the bases and what else outputs are searched for. `resolve` gives a folder's
    /// resolved form; [`resolve_on_disk`] is the real one.
    pub fn new(
        paths: &RootPaths,
        shared: &[String],
        machine: &Machine,
        resolve: impl Fn(&str) -> Option<String>,
    ) -> Result<Roots, RootsError> {
        let workspace = clean(&paths.workspace);
        if !workspace.starts_with('/') {
            return Err(RootsError::RelativeWorkspace(paths.workspace.clone()));
        }
        let absolute = |path: &str| {
            if path.starts_with('/') {
                clean(path)
            } else {
                clean(&format!("{workspace}/{path}"))
            }
        };
        let both = |path: &str| -> Result<(String, String), RootsError> {
            let given = absolute(path);
            let resolved = resolve(&given)
                .map(|resolved| clean(&resolved))
                .filter(|resolved| resolved.starts_with('/'))
                .unwrap_or_else(|| given.clone());
            if given == "/" || resolved == "/" {
                return Err(RootsError::TooBroad(path.to_string()));
            }
            Ok((given, resolved))
        };

        let mut shared_forms = Vec::new();
        for folder in shared {
            let (given, resolved) = both(folder)?;
            shared_forms.push(given);
            shared_forms.push(resolved);
        }
        sort_longest_first(&mut shared_forms);
        shared_forms.dedup();
        // A base must be absolute and not `/`; any other is left out, never an error.
        let usable = |path: &Option<String>| -> Option<(String, String)> {
            let path = path.as_deref().filter(|p| p.starts_with('/'))?;
            both(path).ok()
        };
        let mut bases = Vec::new();
        // The view comes after the toolchain: a placeholder is resolved under the first base of
        // its name.
        for (name, path) in [
            (RootName::CargoHome, &machine.cargo_home),
            (RootName::Toolchain, &machine.toolchain),
            (RootName::Toolchain, &machine.toolchain_view),
        ] {
            if let Some((given, resolved)) = usable(path) {
                bases.push(Root {
                    name,
                    given,
                    resolved,
                });
            }
        }
        let mut machine_forms = Vec::new();
        for (folder, path) in [
            (Folder::CargoHome, &machine.cargo_home),
            (Folder::RustupHome, &machine.rustup_home),
            (Folder::Home, &machine.home),
        ] {
            if let Some((given, resolved)) = usable(path) {
                machine_forms.push((folder, given));
                machine_forms.push((folder, resolved));
            }
        }
        machine_forms.sort();
        machine_forms.dedup();
        let is_shared = |path: &str| {
            shared_forms.iter().any(|f| under(path, f).is_some())
                || bases.iter().any(|base| {
                    under(path, &base.given).is_some() || under(path, &base.resolved).is_some()
                })
        };

        let mut candidates = vec![(RootName::Workspace, paths.workspace.as_str())];
        if let Some(package) = &paths.package {
            candidates.push((RootName::Package, package));
        }
        if let Some(script_out) = &paths.script_out {
            candidates.push((RootName::ScriptOut, script_out));
        }
        candidates.push((RootName::OutDir, &paths.out_dir));
        for (i, dir) in paths.deps.iter().enumerate() {
            let i = u8::try_from(i).map_err(|_| RootsError::TooManyDeps)?;
            candidates.push((RootName::Deps(i), dir));
        }
        for (i, dir) in &paths.native {
            let i = u8::try_from(*i).map_err(|_| RootsError::TooManySearchFolders)?;
            candidates.push((RootName::Native(i), dir));
        }
        // What cargo or the person gave: never derived from another root by position.
        if let Some(dir) = &paths.target_dir {
            candidates.push((RootName::TargetDir, dir));
        }
        if let Some(dir) = &paths.build_target_dir {
            candidates.push((RootName::BuildTargetDir, dir));
        }
        if let Some(dir) = &paths.target_tmpdir {
            candidates.push((RootName::TargetTmp, dir));
        }
        for (i, dir) in paths.bin_exe_dirs.iter().enumerate() {
            let i = u8::try_from(i).map_err(|_| RootsError::TooManyBinFolders)?;
            candidates.push((RootName::BinExe(i), dir));
        }

        let mut roots = Vec::new();
        for (name, path) in candidates {
            let (given, resolved) = both(path)?;
            // A system library folder (`-L native=/usr/lib`) is the same for every checkout:
            // not a root, or every output naming it would be tied.
            let movable = matches!(
                name,
                RootName::Workspace | RootName::Package | RootName::Native(_)
            );
            if movable && (is_shared(&given) || is_shared(&resolved)) {
                continue;
            }
            roots.push(Root {
                name,
                given,
                resolved,
            });
        }

        let mut by_text: BTreeMap<String, RootName> = BTreeMap::new();
        for root in roots.iter().chain(&bases) {
            for text in [&root.given, &root.resolved] {
                let entry = by_text.entry(text.clone()).or_insert(root.name);
                if precedence(root.name) > precedence(*entry) {
                    *entry = root.name;
                }
            }
        }
        let mut forms: Vec<(String, RootName)> = by_text.into_iter().collect();
        forms.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));

        Ok(Roots {
            workspace,
            roots,
            bases,
            forms,
            machine: machine_forms,
            shared: shared_forms,
            package_id: paths.package_id.clone(),
        })
    }

    /// Where `path` lies: under a root or a base in either form (the longest wins) → `Rooted`;
    /// under another shared folder → `Shared`; otherwise `Outside`. A relative path starts from the working
    /// directory. A path with a `..` in it is `Outside`: its rest could leave the root.
    pub fn locate(&self, path: &str) -> Located {
        let absolute = if path.starts_with('/') {
            clean(path)
        } else {
            clean(&format!("{}/{path}", self.workspace))
        };
        if absolute.split('/').any(|part| part == "..") {
            return Located::Outside(absolute);
        }
        let rooted = self
            .forms
            .iter()
            .find_map(|(form, name)| under(&absolute, form).map(|rest| (form.len(), *name, rest)));
        let shared = self
            .shared
            .iter()
            .find(|form| under(&absolute, form).is_some())
            .map(String::len);
        match (rooted, shared) {
            (Some((len, root, rest)), shared) if shared.is_none_or(|s| len >= s) => {
                Located::Rooted(Rooted {
                    root,
                    rest: rest.to_string(),
                })
            }
            (_, Some(_)) => Located::Shared(absolute),
            _ => Located::Outside(absolute),
        }
    }

    /// The path a rooted record names under this call's root, in its given form. `None` when
    /// this call has no such root: the record came from a call with other roots.
    pub fn resolve(&self, rooted: &Rooted) -> Option<PathBuf> {
        let root = self.root(rooted.root)?;
        Some(if rooted.rest.is_empty() {
            PathBuf::from(&root.given)
        } else {
            PathBuf::from(format!("{}/{}", root.given, rooted.rest))
        })
    }

    /// `text` with every root (both forms, longest first) replaced by a placeholder naming it.
    /// A root counts only where it starts and ends a path: `/ws` is not replaced in `/ws2`.
    /// Text that already looks like a placeholder is escaped, so two inputs that differ other
    /// than in a root's form never give one output.
    pub fn to_placeholders(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while let Some(c) = text[i..].chars().next() {
            let rest = &text[i..];
            if rest.starts_with(OPEN) {
                out.push_str(OPEN);
                out.push('}');
                i += OPEN.len();
                continue;
            }
            if c == '/' && starts_path(text, i) {
                let found = self.forms.iter().find(|(form, _)| {
                    rest.starts_with(form.as_str()) && ends_path(text, i + form.len())
                });
                if let Some((form, name)) = found {
                    out.push_str(OPEN);
                    out.push_str(&name.as_string());
                    out.push('}');
                    i += form.len();
                    continue;
                }
            }
            out.push(c);
            i += c.len_utf8();
        }
        out
    }

    /// The inverse of [`Roots::to_placeholders`]: each placeholder becomes this call's root in
    /// its given form. A placeholder naming a root this call doesn't have is an error, never a
    /// guess.
    pub fn from_placeholders(&self, text: &str) -> Result<String, RootsError> {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find(OPEN) {
            out.push_str(&rest[..at]);
            let after = &rest[at + OPEN.len()..];
            let end = after.find('}').ok_or(RootsError::Unterminated)?;
            let name = &after[..end];
            if name.is_empty() {
                out.push_str(OPEN);
            } else {
                let root = RootName::parse(name)
                    .and_then(|name| self.root(name))
                    .ok_or_else(|| RootsError::UnknownRoot(name.to_string()))?;
                out.push_str(&root.given);
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// The remaps of the measured recipe (B-9), in the order rustc is given them: the cargo
    /// home as `/hushrun/cargo`, when it is known, so that a registry or git crate's paths name
    /// no machine; the workspace as `.`, when the working directory is a root (not under a
    /// shared folder); the build script's output as its stand-in; each in its given and, when it
    /// differs, its resolved form. rustc applies the last remap that holds a path, so they are
    /// ordered by length, shortest first: of two folders that hold a path, the inner one wins,
    /// as [`Roots::locate`] decides. Nothing else is remapped: the search of the outputs decides
    /// what may move (B-10).
    pub fn remaps(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for name in [
            RootName::CargoHome,
            RootName::Workspace,
            RootName::ScriptOut,
        ] {
            if let Some(root) = self.root(name) {
                let to = stand_in(name, &self.package_id);
                out.push((root.given.clone(), to.clone()));
                if root.resolved != root.given {
                    out.push((root.resolved.clone(), to));
                }
            }
        }
        out.sort_by_key(|(from, _)| from.len());
        out
    }

    /// `--remap-path-prefix=<from>=<to>` for each of [`Roots::remaps`], in order.
    pub fn remap_args(&self) -> Vec<OsString> {
        self.remaps()
            .into_iter()
            .map(|(from, to)| OsString::from(format!("--remap-path-prefix={from}={to}")))
            .collect()
    }

    /// What rustc writes for `path` under [`Roots::remaps`]: the last remap whose folder holds
    /// it, by whole parts, replaces that folder; a path no remap holds is unchanged.
    pub fn remapped(&self, path: &str) -> String {
        for (from, to) in self.remaps().iter().rev() {
            if let Some(rest) = under(path, from) {
                return if rest.is_empty() {
                    to.clone()
                } else {
                    format!("{to}/{rest}")
                };
            }
        }
        path.to_string()
    }

    /// The given form of a root this call has.
    pub fn given(&self, name: RootName) -> Option<&str> {
        self.root(name).map(|root| root.given.as_str())
    }

    /// What outputs are searched for (B-10): every root in every distinct form, as raw bytes.
    /// The target folder a variable names is a root, so a macro that read it without telling
    /// rustc and wrote it into an output leaves the block tied. Bases are not searched for here:
    /// they are the same for every checkout on the machine.
    pub fn needles(&self) -> Vec<Vec<u8>> {
        self.forms
            .iter()
            .filter(|(_, name)| !name.is_base())
            .map(|(text, _)| text.as_bytes().to_vec())
            .collect()
    }

    /// The folders that name this machine, each in its distinct forms: the cargo home, the
    /// rustup home and the home folder. An output that holds one may still move between
    /// folders here, but not leave the machine.
    pub fn machine_needles(&self) -> &[(Folder, String)] {
        &self.machine
    }

    /// Each of these folders of the machine with its forms here, sorted; no form when this
    /// machine doesn't have it. What a block that holds them is keyed by (B-10).
    pub fn bound(&self, names: &[crate::key::Bound]) -> Vec<(crate::key::Bound, Vec<String>)> {
        use crate::key::Bound;
        let mut out: Vec<(Bound, Vec<String>)> = names
            .iter()
            .map(|name| {
                let mut forms: Vec<String> = match name {
                    Bound::Toolchain => self
                        .bases
                        .iter()
                        .filter(|base| base.name == RootName::Toolchain)
                        .flat_map(|base| [base.given.clone(), base.resolved.clone()])
                        .collect(),
                    folder => {
                        let folder = match folder {
                            Bound::CargoHome => Folder::CargoHome,
                            Bound::RustupHome => Folder::RustupHome,
                            _ => Folder::Home,
                        };
                        self.machine
                            .iter()
                            .filter(|(f, _)| *f == folder)
                            .map(|(_, form)| form.clone())
                            .collect()
                    }
                };
                forms.sort();
                forms.dedup();
                (*name, forms)
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The roots and bases a text names, by its placeholders once replaced.
    pub fn named(&self, text: &str) -> Vec<RootName> {
        let replaced = self.to_placeholders(text);
        let mut out = Vec::new();
        let mut rest = replaced.as_str();
        while let Some(at) = rest.find(OPEN) {
            let after = &rest[at + OPEN.len()..];
            let Some(end) = after.find('}') else { break };
            if let Some(name) = RootName::parse(&after[..end]) {
                out.push(name);
            }
            rest = &after[end + 1..];
        }
        out
    }

    /// The roots as they are, for a block that may not move (B-10): each root's given form and,
    /// when it differs, its resolved form, sorted. Two calls may share such a block only when
    /// these are equal.
    pub fn tied(&self) -> Vec<(RootName, String)> {
        let mut out = Vec::new();
        for root in &self.roots {
            out.push((root.name, root.given.clone()));
            out.push((root.name, root.resolved.clone()));
        }
        out.sort();
        out.dedup();
        out
    }

    /// Whether this call has the root.
    pub fn has(&self, name: RootName) -> bool {
        self.root(name).is_some()
    }

    fn root(&self, name: RootName) -> Option<&Root> {
        self.roots
            .iter()
            .chain(&self.bases)
            .find(|root| root.name == name)
    }
}

/// Which root a text stands for when two roots share it: the more specific one. A root of the
/// checkout wins over a base.
fn precedence(name: RootName) -> (u8, u16) {
    match name {
        RootName::CargoHome => (0, 0),
        RootName::Toolchain => (0, 1),
        RootName::Workspace => (1, 0),
        RootName::Package => (2, 0),
        // The person's target folder, then cargo's folders under it.
        RootName::BuildTargetDir => (3, 0),
        RootName::TargetDir => (4, 0),
        RootName::BinExe(i) => (5, 255 - u16::from(i)),
        RootName::TargetTmp => (6, 0),
        // The first folder of a kind wins over a later one with the same text.
        RootName::Native(i) => (7, 255 - u16::from(i)),
        RootName::Deps(i) => (8, 255 - u16::from(i)),
        RootName::OutDir => (9, 0),
        // A package's own output named as a native folder too is its output (B-6 `generated`).
        RootName::ScriptOut => (10, 0),
    }
}

fn sort_longest_first(texts: &mut [String]) {
    texts.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
}

/// `path` with repeated slashes, `.` parts and a trailing slash gone. `..` stays: resolving it
/// without the file system could be wrong.
fn clean(path: &str) -> String {
    let parts: Vec<&str> = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let joined = parts.join("/");
    if path.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// The rest of `path` under `folder`, by whole parts: `/ws/src` is under `/ws`, `/ws2` is not.
fn under<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(folder)?;
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix('/')
    }
}

/// A character that may continue a file name, so a root next to it is not a whole path.
fn name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '~' | '@')
}

fn starts_path(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !name_char(c) && c != '/')
}

fn ends_path(text: &str, at: usize) -> bool {
    text[at..].chars().next().is_none_or(|c| !name_char(c))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const CARGO_HOME: &str = "/home/dev/.cargo";

    /// A checkout at `ws`, whose resolved forms add `/R` in front (as `/private` on macOS).
    fn paths(ws: &str) -> RootPaths {
        RootPaths {
            workspace: ws.to_string(),
            package: Some(format!("{ws}/crates/app")),
            script_out: Some(format!("{ws}/target/debug/build/app-1/out")),
            out_dir: format!("{ws}/target/debug/deps"),
            deps: vec![
                format!("{ws}/target/debug/deps"),
                format!("{ws}/vendor/deps"),
            ],
            native: vec![],
            package_id: "app-0.1.0".to_string(),
            target_dir: None,
            build_target_dir: None,
            target_tmpdir: None,
            bin_exe_dirs: vec![],
        }
    }

    fn shared() -> Vec<String> {
        vec![CARGO_HOME.to_string(), "/usr".to_string()]
    }

    fn with_r(path: &str) -> Option<String> {
        Some(format!("/R{path}"))
    }

    fn roots(ws: &str) -> Roots {
        Roots::new(&paths(ws), &shared(), &Machine::default(), with_r).unwrap()
    }

    fn rooted(root: RootName, rest: &str) -> Located {
        Located::Rooted(Rooted {
            root,
            rest: rest.to_string(),
        })
    }

    // covers: B-8
    #[test]
    fn locate_by_longest_root_in_either_form() {
        let roots = roots("/w/ws");
        let cases = [
            (
                "/w/ws/src/main.rs",
                rooted(RootName::Workspace, "src/main.rs"),
            ),
            ("src/main.rs", rooted(RootName::Workspace, "src/main.rs")),
            ("./src//main.rs", rooted(RootName::Workspace, "src/main.rs")),
            ("/w/ws", rooted(RootName::Workspace, "")),
            ("/w/ws/", rooted(RootName::Workspace, "")),
            (
                "/R/w/ws/src/main.rs",
                rooted(RootName::Workspace, "src/main.rs"),
            ),
            (
                "/w/ws/crates/app/src/lib.rs",
                rooted(RootName::Package, "src/lib.rs"),
            ),
            (
                "crates/app/src/lib.rs",
                rooted(RootName::Package, "src/lib.rs"),
            ),
            (
                "/R/w/ws/target/debug/build/app-1/out/gen.rs",
                rooted(RootName::ScriptOut, "gen.rs"),
            ),
            (
                "/w/ws/target/debug/deps/libx.rlib",
                rooted(RootName::OutDir, "libx.rlib"),
            ),
            (
                "/w/ws/vendor/deps/liby.rlib",
                rooted(RootName::Deps(1), "liby.rlib"),
            ),
            (
                "/w/ws2/src/main.rs",
                Located::Outside("/w/ws2/src/main.rs".into()),
            ),
            (
                "/home/dev/.cargo/registry/src/x/lib.rs",
                Located::Shared("/home/dev/.cargo/registry/src/x/lib.rs".into()),
            ),
            (
                "/R/usr/include/a.h",
                Located::Shared("/R/usr/include/a.h".into()),
            ),
            ("/etc/hosts", Located::Outside("/etc/hosts".into())),
            (
                "/w/ws/../other/x.rs",
                Located::Outside("/w/ws/../other/x.rs".into()),
            ),
            (
                "../other/x.rs",
                Located::Outside("/w/ws/../other/x.rs".into()),
            ),
        ];
        for (path, expected) in cases {
            assert_eq!(roots.locate(path), expected, "{path}");
        }
    }

    // covers: B-8
    #[test]
    fn a_record_is_looked_up_under_the_current_root() {
        let here = roots("/w/ws");
        let there = roots("/w/other-checkout");
        for path in [
            "/w/ws/crates/app/migrations/0001.sql",
            "/w/ws/target/debug/build/app-1/out/gen.rs",
            "/R/w/ws/src/main.rs",
        ] {
            let Located::Rooted(record) = here.locate(path) else {
                panic!("{path} is not rooted");
            };
            let looked_up = there.resolve(&record).unwrap();
            let looked_up = looked_up.to_str().unwrap();
            assert!(looked_up.starts_with("/w/other-checkout/"), "{looked_up}");
            assert!(!looked_up.contains("/w/ws/"), "{looked_up}");
        }
        let few = Roots::new(
            &RootPaths {
                deps: vec![],
                script_out: None,
                ..paths("/w/ws")
            },
            &shared(),
            &Machine::default(),
            with_r,
        )
        .unwrap();
        let elsewhere = Rooted {
            root: RootName::Deps(1),
            rest: "x".into(),
        };
        assert_eq!(few.resolve(&elsewhere), None);
        let script = Rooted {
            root: RootName::ScriptOut,
            rest: "x".into(),
        };
        assert_eq!(few.resolve(&script), None);
        assert!(!few.has(RootName::ScriptOut));
    }

    // covers: B-8
    #[test]
    fn workspace_and_package_under_a_shared_folder_are_not_roots() {
        let registry = "/home/dev/.cargo/registry/src/index/serde-1.0.0";
        let paths = RootPaths {
            workspace: registry.to_string(),
            package: Some(registry.to_string()),
            script_out: Some("/w/ws/target/debug/build/serde-1/out".into()),
            out_dir: "/w/ws/target/debug/deps".into(),
            deps: vec!["/w/ws/target/debug/deps".into()],
            native: vec![],
            package_id: "serde-1.0.0".into(),
            target_dir: None,
            build_target_dir: None,
            target_tmpdir: None,
            bin_exe_dirs: vec![],
        };
        let roots = Roots::new(&paths, &shared(), &Machine::default(), |_| None).unwrap();
        assert!(!roots.has(RootName::Workspace));
        assert!(!roots.has(RootName::Package));
        assert_eq!(
            roots.locate("src/lib.rs"),
            Located::Shared(format!("{registry}/src/lib.rs"))
        );
        assert_eq!(
            roots.locate("/w/ws/target/debug/build/serde-1/out/x.rs"),
            rooted(RootName::ScriptOut, "x.rs")
        );
        let remaps = roots.remap_args();
        assert!(
            !remaps
                .iter()
                .any(|arg| arg.to_str().unwrap().ends_with("=."))
        );
        // A workspace that resolves into a shared folder is shared too.
        let linked = RootPaths {
            workspace: "/link/to/registry".into(),
            package: None,
            ..paths.clone()
        };
        let roots = Roots::new(&linked, &shared(), &Machine::default(), |p: &str| {
            (p == "/link/to/registry").then(|| registry.to_string())
        })
        .unwrap();
        assert!(!roots.has(RootName::Workspace));
    }

    // covers: B-8
    #[test]
    fn placeholders_are_the_same_in_every_checkout() {
        let a = roots("/w/ws");
        let b = roots("/elsewhere/checkout-2");
        let text_a = "--cfg=x -Lnative=/w/ws/target/debug/build/app-1/out/lib file:/w/ws/src";
        let text_b = "--cfg=x -Lnative=/elsewhere/checkout-2/target/debug/build/app-1/out/lib \
                      file:/elsewhere/checkout-2/src";
        assert_eq!(a.to_placeholders(text_a), b.to_placeholders(text_b));
        assert_eq!(
            a.to_placeholders(text_a),
            "--cfg=x -Lnative={hushrun:script-out}/lib file:{hushrun:workspace}/src"
        );
        assert_eq!(
            b.from_placeholders(&a.to_placeholders(text_a)).unwrap(),
            text_b
        );
        assert_eq!(a.to_placeholders("/w/ws2 /w/wsx/y"), "/w/ws2 /w/wsx/y");
        assert_eq!(a.to_placeholders("/x/w/ws"), "/x/w/ws");
        assert_eq!(a.to_placeholders("/w/ws"), "{hushrun:workspace}");
    }

    #[test]
    fn placeholders_that_name_no_root_here_are_errors() {
        let few = Roots::new(
            &RootPaths {
                script_out: None,
                ..paths("/w/ws")
            },
            &shared(),
            &Machine::default(),
            with_r,
        )
        .unwrap();
        assert_eq!(
            few.from_placeholders("{hushrun:script-out}/x"),
            Err(RootsError::UnknownRoot("script-out".into()))
        );
        assert_eq!(
            few.from_placeholders("{hushrun:nonsense}"),
            Err(RootsError::UnknownRoot("nonsense".into()))
        );
        assert_eq!(
            few.from_placeholders("{hushrun:workspace"),
            Err(RootsError::Unterminated)
        );
    }

    /// A small deterministic generator: xorshift64.
    struct Gen(u64);

    impl Gen {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
            items[(self.next() % items.len() as u64) as usize]
        }
    }

    /// Fragments with no `R`, so no concatenation makes a resolved form by accident.
    const JUNK: [&str; 18] = [
        "a",
        "/",
        "=",
        "-",
        ":",
        " ",
        "{",
        "}",
        "{hushrun:",
        "{hushrun:}",
        "{hushrun:workspace}",
        "{hushrun:deps-0}",
        "x/",
        "é",
        "ws",
        "2",
        "/w",
        "\"",
    ];

    const GIVEN: [&str; 6] = [
        "/w/ws",
        "/w/ws/crates/app",
        "/w/ws/target/debug/build/app-1/out",
        "/w/ws/target/debug/deps",
        "/w/ws/vendor/deps",
        "/w/ws/src/lib.rs",
    ];

    // covers: B-8
    #[test]
    fn placeholders_round_trip_and_never_collide() {
        // Plain, and with cargo's target folder and its test folders as roots too (B-5).
        let with_target = Roots::new(
            &test_paths("/w/ws", "/w/ws/target"),
            &shared(),
            &Machine::default(),
            with_r,
        )
        .unwrap();
        for (roots, extra) in [
            (roots("/w/ws"), &[][..]),
            (
                with_target,
                &["/w/ws/target", "/w/ws/target/tmp", "/w/ws/target/debug"][..],
            ),
        ] {
            let mut fragments: Vec<&str> = JUNK.to_vec();
            fragments.extend(GIVEN);
            fragments.extend(extra);
            let mut rng = Gen(0x9e37_79b9_7f4a_7c15);
            let mut seen: HashMap<String, String> = HashMap::new();
            for _ in 0..20_000 {
                let len = rng.next() % 8;
                let input: String = (0..len).map(|_| rng.pick(&fragments)).collect();
                let encoded = roots.to_placeholders(&input);
                assert_eq!(
                    roots.from_placeholders(&encoded).unwrap(),
                    input,
                    "{input:?}"
                );
                if let Some(other) = seen.insert(encoded.clone(), input.clone()) {
                    assert_eq!(other, input, "both give {encoded:?}");
                }
            }
            assert!(
                seen.len() > 5_000,
                "too few distinct inputs: {}",
                seen.len()
            );
        }
    }

    // covers: B-8
    #[test]
    fn resolved_forms_come_back_as_given_forms() {
        let roots = roots("/w/ws");
        let resolved: Vec<String> = GIVEN.iter().map(|g| format!("/R{g}")).collect();
        let mut rng = Gen(0x0123_4567_89ab_cdef);
        for _ in 0..5_000 {
            let len = rng.next() % 6;
            let mut input = Vec::new();
            let mut expected = Vec::new();
            for _ in 0..len {
                let pick = (rng.next() % 3) as usize;
                let index = (rng.next() % GIVEN.len() as u64) as usize;
                match pick {
                    0 => {
                        input.push(resolved[index].clone());
                        expected.push(GIVEN[index].to_string());
                    }
                    1 => {
                        input.push(GIVEN[index].to_string());
                        expected.push(GIVEN[index].to_string());
                    }
                    _ => {
                        let junk = rng.pick(&JUNK).to_string();
                        input.push(junk.clone());
                        expected.push(junk);
                    }
                }
            }
            let input = input.join(" ");
            let expected = expected.join(" ");
            let back = roots
                .from_placeholders(&roots.to_placeholders(&input))
                .unwrap();
            assert_eq!(back, expected, "{input:?}");
        }
    }

    // covers: B-9
    #[test]
    fn remaps_are_the_workspace_then_the_script_output() {
        let roots = roots("/w/ws");
        let remaps: Vec<String> = roots
            .remap_args()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert_eq!(
            remaps,
            [
                "--remap-path-prefix=/w/ws=.",
                "--remap-path-prefix=/R/w/ws=.",
                "--remap-path-prefix=/w/ws/target/debug/build/app-1/out=/hushrun/out/app-0.1.0",
                "--remap-path-prefix=/R/w/ws/target/debug/build/app-1/out=/hushrun/out/app-0.1.0",
            ]
        );
        // The last remap that holds a path wins, as in rustc.
        assert_eq!(
            roots.remapped("/w/ws/crates/app/src/lib.rs"),
            "./crates/app/src/lib.rs"
        );
        assert_eq!(
            roots.remapped("/R/w/ws/target/debug/build/app-1/out/gen.rs"),
            "/hushrun/out/app-0.1.0/gen.rs"
        );
        assert_eq!(roots.remapped("/w/ws"), ".");
        assert_eq!(roots.remapped("/w/ws2/x"), "/w/ws2/x");
        let one_form =
            Roots::new(&paths("/w/ws"), &shared(), &Machine::default(), |_| None).unwrap();
        assert_eq!(one_form.remap_args().len(), 2);
        // No script output: only the workspace.
        let plain = Roots::new(
            &RootPaths {
                script_out: None,
                ..paths("/w/ws")
            },
            &shared(),
            &Machine::default(),
            |_| None,
        )
        .unwrap();
        assert_eq!(
            plain.remap_args(),
            [OsString::from("--remap-path-prefix=/w/ws=.")]
        );
    }

    // covers: B-10
    #[test]
    fn needles_are_every_form_of_every_root() {
        let roots = roots("/w/ws");
        let mut needles: Vec<String> = roots
            .needles()
            .into_iter()
            .map(|n| String::from_utf8(n).unwrap())
            .collect();
        needles.sort();
        let mut expected: Vec<String> = GIVEN[..5]
            .iter()
            .flat_map(|g| [g.to_string(), format!("/R{g}")])
            .collect();
        expected.sort();
        assert_eq!(needles, expected);
    }

    /// A checkout at `ws` whose target folder is `target`, outside it, as `CARGO_TARGET_DIR`
    /// names it: an integration test's compile.
    fn test_paths(ws: &str, target: &str) -> RootPaths {
        RootPaths {
            out_dir: format!("{target}/debug/deps"),
            deps: vec![format!("{target}/debug/deps")],
            script_out: None,
            target_dir: Some(target.to_string()),
            target_tmpdir: Some(format!("{target}/tmp")),
            bin_exe_dirs: vec![format!("{target}/debug")],
            ..paths(ws)
        }
    }

    // covers: B-5, B-8, B-10
    #[test]
    fn the_locations_cargo_hands_a_compile_are_roots_wherever_the_target_folder_is() {
        let a = Roots::new(
            &test_paths("/w/ws", "/t/one"),
            &shared(),
            &Machine::default(),
            with_r,
        )
        .unwrap();
        let b = Roots::new(
            &test_paths("/elsewhere/checkout-2", "/other/targets/the-second-one"),
            &shared(),
            &Machine::default(),
            with_r,
        )
        .unwrap();
        // The values cargo sets for the test give one text in both.
        for (text_a, text_b) in [
            ("/t/one/tmp", "/other/targets/the-second-one/tmp"),
            (
                "/t/one/debug/prog",
                "/other/targets/the-second-one/debug/prog",
            ),
            ("/R/t/one/tmp/x", "/other/targets/the-second-one/tmp/x"),
        ] {
            assert_eq!(a.to_placeholders(text_a), b.to_placeholders(text_b));
        }
        assert_eq!(a.to_placeholders("/t/one/tmp"), "{hushrun:target-tmpdir}");
        assert_eq!(
            a.to_placeholders("/t/one/debug/prog"),
            "{hushrun:bin-exe-0}/prog"
        );
        assert_eq!(
            a.to_placeholders("/t/one/release"),
            "{hushrun:target-dir}/release"
        );
        assert_eq!(
            b.from_placeholders("{hushrun:bin-exe-0}/prog").unwrap(),
            "/other/targets/the-second-one/debug/prog"
        );
        assert_eq!(a.to_placeholders("/t/one2/tmp"), "/t/one2/tmp");
        // Each is searched for in outputs, in both forms, and is part of a tied place.
        let needles = a.needles();
        for text in ["/t/one", "/R/t/one", "/t/one/tmp", "/R/t/one/debug"] {
            assert!(needles.contains(&text.as_bytes().to_vec()), "{text}");
        }
        let tied = a.tied();
        assert!(tied.contains(&(RootName::TargetDir, "/t/one".into())));
        assert!(tied.contains(&(RootName::TargetTmp, "/R/t/one/tmp".into())));
        assert!(tied.contains(&(RootName::BinExe(0), "/t/one/debug".into())));
        assert_eq!(
            a.locate("/t/one/tmp/fixture.txt"),
            rooted(RootName::TargetTmp, "fixture.txt")
        );
        assert_eq!(
            a.locate("/t/one/debug/deps/liba.rlib"),
            rooted(RootName::OutDir, "liba.rlib")
        );
        // Inside the workspace too, the more specific root names them.
        let inside = Roots::new(
            &test_paths("/w/ws", "/w/ws/target"),
            &shared(),
            &Machine::default(),
            with_r,
        )
        .unwrap();
        assert_eq!(
            inside.to_placeholders("/w/ws/target/tmp /w/ws/src"),
            "{hushrun:target-tmpdir} {hushrun:workspace}/src"
        );
        // Not set: not roots.
        let plain = roots("/w/ws");
        assert!(!plain.has(RootName::TargetDir));
        assert!(!plain.has(RootName::TargetTmp));
        assert!(!plain.has(RootName::BinExe(0)));
        let mut many = test_paths("/w/ws", "/t/one");
        many.bin_exe_dirs = (0..300).map(|i| format!("/t/one/{i}")).collect();
        assert_eq!(
            Roots::new(&many, &[], &Machine::default(), |_| None),
            Err(RootsError::TooManyBinFolders)
        );
    }

    // covers: B-10
    #[test]
    fn tied_holds_both_forms_and_differs_between_checkouts() {
        let a = roots("/w/ws");
        let tied = a.tied();
        assert!(tied.contains(&(RootName::Workspace, "/w/ws".into())));
        assert!(tied.contains(&(RootName::Workspace, "/R/w/ws".into())));
        assert!(tied.contains(&(RootName::Deps(0), "/w/ws/target/debug/deps".into())));
        assert_eq!(tied.len(), 12);
        assert_ne!(tied, roots("/w/ws2").tied());
        assert_eq!(tied, roots("/w/ws").tied());
    }

    // covers: B-6, B-8
    #[test]
    fn a_native_folder_is_a_root_named_by_its_place_unless_it_is_shared() {
        let p = RootPaths {
            native: vec![
                (2, "/elsewhere/target/debug/build/z-1/out".into()),
                (3, "/usr/lib".into()),
                (4, "/w/ws/target/debug/build/app-1/out".into()),
            ],
            ..paths("/w/ws")
        };
        let roots = Roots::new(&p, &shared(), &Machine::default(), |_| None).unwrap();
        assert_eq!(
            roots.locate("/elsewhere/target/debug/build/z-1/out/libz.a"),
            rooted(RootName::Native(2), "libz.a")
        );
        assert_eq!(
            roots.to_placeholders("native=/elsewhere/target/debug/build/z-1/out"),
            "native={hushrun:native-2}"
        );
        // A system folder stays shared; the package's own output stays its output.
        assert!(!roots.has(RootName::Native(3)));
        assert_eq!(
            roots.locate("/usr/lib/libz.a"),
            Located::Shared("/usr/lib/libz.a".into())
        );
        assert_eq!(
            roots.locate("/w/ws/target/debug/build/app-1/out/libapp.a"),
            rooted(RootName::ScriptOut, "libapp.a")
        );
        assert!(roots.tied().contains(&(
            RootName::Native(2),
            "/elsewhere/target/debug/build/z-1/out".into()
        )));
        let mut far = paths("/w/ws");
        far.native = vec![(300, "/x".into())];
        assert_eq!(
            Roots::new(&far, &[], &Machine::default(), |_| None),
            Err(RootsError::TooManySearchFolders)
        );
    }

    /// The folders of a machine whose home is `home`, its toolchain installed as `toolchain`.
    fn machine(home: &str, toolchain: &str) -> Machine {
        Machine {
            cargo_home: Some(format!("{home}/.cargo")),
            toolchain: Some(format!("{home}/.rustup/toolchains/{toolchain}")),
            toolchain_view: None,
            rustup_home: Some(format!("{home}/.rustup")),
            home: Some(home.to_string()),
        }
    }

    /// A registry crate's call on a machine: its sources under the cargo home, the workspace
    /// at `ws`.
    fn registry_paths(home: &str, ws: &str) -> RootPaths {
        RootPaths {
            package: Some(format!(
                "{home}/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.0"
            )),
            ..paths(ws)
        }
    }

    // covers: B-8
    #[test]
    fn the_cargo_home_and_the_toolchain_are_bases_named_alike_on_every_machine() {
        let shared = |home: &str| vec![format!("{home}/.cargo"), format!("{home}/.rustup")];
        let a = Roots::new(
            &registry_paths("/home/dev", "/home/dev/ws"),
            &shared("/home/dev"),
            &machine("/home/dev", "stable-x"),
            |_| None,
        )
        .unwrap();
        let b = Roots::new(
            &registry_paths("/home/someone-else", "/srv/build/checkout"),
            &shared("/home/someone-else"),
            &machine("/home/someone-else", "1.94.0-x"),
            |_| None,
        )
        .unwrap();
        let index = "registry/src/index.crates.io-1949cf8c6b5b557f";
        let source_a = format!("/home/dev/.cargo/{index}/serde-1.0.0/src/lib.rs");
        let source_b = format!("/home/someone-else/.cargo/{index}/serde-1.0.0/src/lib.rs");
        let named = rooted(
            RootName::CargoHome,
            &format!("{index}/serde-1.0.0/src/lib.rs"),
        );
        assert_eq!(a.locate(&source_a), named);
        assert_eq!(b.locate(&source_b), named);
        let Located::Rooted(record) = named else {
            unreachable!()
        };
        assert_eq!(b.resolve(&record).unwrap(), PathBuf::from(&source_b));
        // In arguments and values too, and back.
        let text_a = format!("--extern x {source_a} CARGO_MANIFEST_DIR=/home/dev/.cargo/{index}");
        let text_b =
            format!("--extern x {source_b} CARGO_MANIFEST_DIR=/home/someone-else/.cargo/{index}");
        assert_eq!(a.to_placeholders(&text_a), b.to_placeholders(&text_b));
        assert!(
            a.to_placeholders(&text_a)
                .contains("{hushrun:cargo-home}/registry/src/")
        );
        assert_eq!(
            b.from_placeholders(&a.to_placeholders(&text_a)).unwrap(),
            text_b
        );
        let std_a = "/home/dev/.rustup/toolchains/stable-x/lib/rustlib/x/lib/libstd.rlib";
        let std_b = "/home/someone-else/.rustup/toolchains/1.94.0-x/lib/rustlib/x/lib/libstd.rlib";
        assert_eq!(a.to_placeholders(std_a), b.to_placeholders(std_b));
        assert_eq!(
            a.to_placeholders(std_a),
            "{hushrun:toolchain}/lib/rustlib/x/lib/libstd.rlib"
        );
        // The rest of the rustup home is a shared folder, as before: its path is the machine's.
        assert_eq!(
            a.locate("/home/dev/.rustup/settings.toml"),
            Located::Shared("/home/dev/.rustup/settings.toml".into())
        );
        // A base never ties a block, and is not what B-10 searches for to decide its place.
        assert_eq!(a.tied(), roots_without_bases(&a));
        assert!(
            !a.needles()
                .iter()
                .any(|n| n.starts_with(b"/home/dev/.cargo") || n.starts_with(b"/home/dev/.rustup"))
        );
        assert_eq!(a.named(&text_a), [RootName::CargoHome, RootName::CargoHome]);
        assert_eq!(
            a.named("/home/dev/ws/src {hushrun:toolchain}"),
            [RootName::Workspace]
        );
        // What decides whether the block may leave the machine is searched for instead.
        let needles: Vec<(Folder, &str)> = a
            .machine_needles()
            .iter()
            .map(|(f, t)| (*f, t.as_str()))
            .collect();
        assert_eq!(
            needles,
            [
                (Folder::CargoHome, "/home/dev/.cargo"),
                (Folder::RustupHome, "/home/dev/.rustup"),
                (Folder::Home, "/home/dev"),
            ]
        );
    }

    /// The tied roots of a call, recomputed without its bases.
    fn roots_without_bases(roots: &Roots) -> Vec<(RootName, String)> {
        roots
            .tied()
            .into_iter()
            .filter(|(name, _)| !name.is_base())
            .collect()
    }

    // covers: B-9
    #[test]
    fn the_cargo_home_is_remapped_and_the_inner_folder_wins() {
        let a = Roots::new(
            &registry_paths("/home/dev", "/w/ws"),
            &[],
            &machine("/home/dev", "stable-x"),
            with_r,
        )
        .unwrap();
        let remaps: Vec<String> = a
            .remap_args()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert_eq!(
            remaps,
            [
                "--remap-path-prefix=/w/ws=.",
                "--remap-path-prefix=/R/w/ws=.",
                "--remap-path-prefix=/home/dev/.cargo=/hushrun/cargo",
                "--remap-path-prefix=/R/home/dev/.cargo=/hushrun/cargo",
                "--remap-path-prefix=/w/ws/target/debug/build/app-1/out=/hushrun/out/app-0.1.0",
                "--remap-path-prefix=/R/w/ws/target/debug/build/app-1/out=/hushrun/out/app-0.1.0",
            ]
        );
        assert_eq!(
            a.remapped("/home/dev/.cargo/registry/src/i/serde-1.0.0/src/lib.rs"),
            "/hushrun/cargo/registry/src/i/serde-1.0.0/src/lib.rs"
        );
        // A cargo home inside the checkout (some CI jobs keep it there): the cargo home is the
        // inner folder, and wins for what lies in it, as `locate` decides.
        let inside = Roots::new(
            &RootPaths {
                package: Some("/w/ws/.cargo/registry/src/i/serde-1.0.0".into()),
                ..paths("/w/ws")
            },
            &[],
            &Machine {
                cargo_home: Some("/w/ws/.cargo".into()),
                ..Machine::default()
            },
            |_| None,
        )
        .unwrap();
        let path = "/w/ws/.cargo/registry/src/i/serde-1.0.0/src/lib.rs";
        assert_eq!(
            inside.remapped(path),
            "/hushrun/cargo/registry/src/i/serde-1.0.0/src/lib.rs"
        );
        assert_eq!(
            inside.locate(path),
            rooted(RootName::CargoHome, "registry/src/i/serde-1.0.0/src/lib.rs")
        );
        assert_eq!(inside.remapped("/w/ws/src/lib.rs"), "./src/lib.rs");
        // The package in the cargo home is not a root; the workspace still is.
        assert!(!inside.has(RootName::Package));
        assert!(inside.has(RootName::Workspace));
        // No cargo home known: no remap for it, and its paths are where they are.
        let unknown = Roots::new(
            &registry_paths("/home/dev", "/w/ws"),
            &[],
            &Machine::default(),
            |_| None,
        )
        .unwrap();
        assert!(
            !unknown
                .remap_args()
                .iter()
                .any(|arg| arg.to_str().unwrap().contains("/hushrun/cargo"))
        );
        assert_eq!(
            unknown.locate("/home/dev/.cargo/x"),
            Located::Outside("/home/dev/.cargo/x".into())
        );
    }

    // covers: B-8
    #[test]
    fn a_base_of_the_file_systems_root_or_relative_is_left_out() {
        let roots = Roots::new(
            &paths("/w/ws"),
            &[],
            &Machine {
                cargo_home: Some("/".into()),
                toolchain: Some("relative/toolchain".into()),
                toolchain_view: None,
                rustup_home: Some(String::new()),
                home: Some("/".into()),
            },
            |_| None,
        )
        .unwrap();
        assert!(!roots.has(RootName::CargoHome));
        assert!(!roots.has(RootName::Toolchain));
        assert!(roots.machine_needles().is_empty());
        assert_eq!(roots.locate("/etc/x"), Located::Outside("/etc/x".into()));
    }

    #[test]
    fn bad_roots_are_errors() {
        let mut p = paths("/w/ws");
        p.workspace = "relative".into();
        assert!(matches!(
            Roots::new(&p, &[], &Machine::default(), |_| None),
            Err(RootsError::RelativeWorkspace(_))
        ));
        let mut p = paths("/w/ws");
        p.out_dir = "/".into();
        assert!(matches!(
            Roots::new(&p, &[], &Machine::default(), |_| None),
            Err(RootsError::TooBroad(_))
        ));
        let mut p = paths("/w/ws");
        p.deps = (0..300).map(|i| format!("/d/{i}")).collect();
        assert_eq!(
            Roots::new(&p, &[], &Machine::default(), |_| None),
            Err(RootsError::TooManyDeps)
        );
        // A relative out dir starts from the working directory.
        let mut p = paths("/w/ws");
        p.out_dir = "target/other".into();
        let roots = Roots::new(&p, &[], &Machine::default(), |_| None).unwrap();
        assert_eq!(
            roots.locate("/w/ws/target/other/x"),
            rooted(RootName::OutDir, "x")
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolves_symlinks_on_disk_once() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let link = link.to_str().unwrap().to_string();
        let real = resolve_on_disk(real.to_str().unwrap()).unwrap();
        let p = RootPaths {
            workspace: link.clone(),
            package: None,
            script_out: None,
            out_dir: format!("{link}/target"),
            deps: vec![],
            native: vec![],
            package_id: "x-1".into(),
            target_dir: None,
            build_target_dir: None,
            target_tmpdir: None,
            bin_exe_dirs: vec![],
        };
        let roots = Roots::new(&p, &[], &Machine::default(), resolve_on_disk).unwrap();
        assert_eq!(
            roots.locate(&format!("{real}/src/lib.rs")),
            rooted(RootName::Workspace, "src/lib.rs")
        );
        assert_eq!(
            roots.to_placeholders(&format!("{real}/a {link}/b")),
            "{hushrun:workspace}/a {hushrun:workspace}/b"
        );
        // The out dir doesn't exist yet: it has one form.
        assert_eq!(
            roots.locate(&format!("{link}/target/x")),
            rooted(RootName::OutDir, "x")
        );
    }

    #[test]
    fn of_reads_an_invocation() {
        use crate::invocation::tests::{WS, registry_lib};
        use crate::invocation::{Parsed, parse};
        let (argv, env) = registry_lib();
        let Parsed::Handled(inv) = parse(&argv, &env, std::path::Path::new(WS)) else {
            panic!("not handled");
        };
        let p = RootPaths::of(&inv);
        assert_eq!(p.workspace, WS);
        assert_eq!(p.package_id, "serde_core-1.0.229");
        assert_eq!(p.out_dir, "/home/dev/ws/target/debug/deps");
        assert_eq!(p.deps, ["/home/dev/ws/target/debug/deps"]);
        assert_eq!(
            p.script_out.as_deref(),
            Some("/home/dev/ws/target/debug/build/serde_core-db522bc2a1d3eab6/out")
        );
    }
}
