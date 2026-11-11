//! Location roots and their placeholders (B-8), the remap flags (B-9) and what to search
//! outputs for (B-10).
//!
//! A root is a folder a compile's paths start from: the workspace (rustc's working directory),
//! the package (`CARGO_MANIFEST_DIR`), the build script's output (`OUT_DIR`), the output folder
//! (`--out-dir`) and each `-L dependency=` folder. Each has a given form, as cargo wrote it, and
//! a resolved form, with symlinks resolved (`/tmp` is `/private/tmp` on macOS, and some macros
//! canonicalize). A path under either form is recorded as the root's name and the rest.
//!
//! The constructor is the one place this module touches the file system: it resolves each root
//! once, through a resolver the caller passes, so tests pass their own.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::invocation::Invocation;
use crate::model::{Located, RootName, Rooted};

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
    }
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
    /// The package's `name-version`, for the stand-ins.
    pub package_id: String,
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
            package_id,
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
    /// Each distinct text of a root's forms with the one root it stands for, longest first.
    forms: Vec<(String, RootName)>,
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
    /// the package are roots only when they are not under one. `resolve` gives a folder's
    /// resolved form; [`resolve_on_disk`] is the real one.
    pub fn new(
        paths: &RootPaths,
        shared: &[String],
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
        let is_shared = |path: &str| shared_forms.iter().any(|f| under(path, f).is_some());

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

        let mut roots = Vec::new();
        for (name, path) in candidates {
            let (given, resolved) = both(path)?;
            let movable = matches!(name, RootName::Workspace | RootName::Package);
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
        for root in &roots {
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
            forms,
            shared: shared_forms,
            package_id: paths.package_id.clone(),
        })
    }

    /// Where `path` lies: under a root in either form (the longest wins) → `Rooted`; under a
    /// shared folder → `Shared`; otherwise `Outside`. A relative path starts from the working
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

    /// The remaps of the measured recipe (B-9), in the order rustc is given them: the workspace
    /// as `.`, when the working directory is a root (not under a shared folder), then the build
    /// script's output as its stand-in; each in its given and, when it differs, its resolved
    /// form. rustc applies remaps last to first, so the general one comes first. Nothing else is
    /// remapped: the search of the outputs decides what may move (B-10).
    pub fn remaps(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for name in [RootName::Workspace, RootName::ScriptOut] {
            if let Some(root) = self.root(name) {
                let to = stand_in(name, &self.package_id);
                out.push((root.given.clone(), to.clone()));
                if root.resolved != root.given {
                    out.push((root.resolved.clone(), to));
                }
            }
        }
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
    pub fn needles(&self) -> Vec<Vec<u8>> {
        self.forms
            .iter()
            .map(|(text, _)| text.as_bytes().to_vec())
            .collect()
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
        self.roots.iter().find(|root| root.name == name)
    }
}

/// Which root a text stands for when two roots share it: the more specific one.
fn precedence(name: RootName) -> (u8, u16) {
    match name {
        RootName::Workspace => (0, 0),
        RootName::Package => (1, 0),
        // The first dependency folder wins over a later one with the same text.
        RootName::Deps(i) => (2, 255 - u16::from(i)),
        RootName::OutDir => (3, 0),
        RootName::ScriptOut => (4, 0),
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
            package_id: "app-0.1.0".to_string(),
        }
    }

    fn shared() -> Vec<String> {
        vec![CARGO_HOME.to_string(), "/usr".to_string()]
    }

    fn with_r(path: &str) -> Option<String> {
        Some(format!("/R{path}"))
    }

    fn roots(ws: &str) -> Roots {
        Roots::new(&paths(ws), &shared(), with_r).unwrap()
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
            package_id: "serde-1.0.0".into(),
        };
        let roots = Roots::new(&paths, &shared(), |_| None).unwrap();
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
        let roots = Roots::new(&linked, &shared(), |p: &str| {
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
        let roots = roots("/w/ws");
        let mut fragments: Vec<&str> = JUNK.to_vec();
        fragments.extend(GIVEN);
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
        let one_form = Roots::new(&paths("/w/ws"), &shared(), |_| None).unwrap();
        assert_eq!(one_form.remap_args().len(), 2);
        // No script output: only the workspace.
        let plain = Roots::new(
            &RootPaths {
                script_out: None,
                ..paths("/w/ws")
            },
            &shared(),
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

    #[test]
    fn bad_roots_are_errors() {
        let mut p = paths("/w/ws");
        p.workspace = "relative".into();
        assert!(matches!(
            Roots::new(&p, &[], |_| None),
            Err(RootsError::RelativeWorkspace(_))
        ));
        let mut p = paths("/w/ws");
        p.out_dir = "/".into();
        assert!(matches!(
            Roots::new(&p, &[], |_| None),
            Err(RootsError::TooBroad(_))
        ));
        let mut p = paths("/w/ws");
        p.deps = (0..300).map(|i| format!("/d/{i}")).collect();
        assert_eq!(Roots::new(&p, &[], |_| None), Err(RootsError::TooManyDeps));
        // A relative out dir starts from the working directory.
        let mut p = paths("/w/ws");
        p.out_dir = "target/other".into();
        let roots = Roots::new(&p, &[], |_| None).unwrap();
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
            package_id: "x-1".into(),
        };
        let roots = Roots::new(&p, &[], resolve_on_disk).unwrap();
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
