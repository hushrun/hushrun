//! A part (K-10): one package manager's side of a package store: which files of a checkout are its
//! manifest set, what of its cache a bake keeps, and how a run points the tool at it.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_core::blocks::Hex32;

use crate::store::VerifiedBy;

/// The variables a reader sees: the run's own, before hushrun adds any.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// K-10: how a part reaches its tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reader {
    /// Mechanism A: the tool reads the part in place, as a second, read-only folder, through
    /// these variables. Nothing is copied.
    InPlace(Vec<(String, OsString)>),
    /// Mechanism A for Maven: one word that joins `MAVEN_OPTS`, merged with whatever else of
    /// hushrun builds that variable in the same run, after the person's words.
    MavenOpts(String),
    /// Mechanism C: a run copies these files into the tool's own cache, each checked first
    /// (K-45).
    Seed(Seed),
    /// The tool is left alone, with this line (`packages: <part>: left alone: …`).
    LeftAlone(String),
}

/// K-45: what a run seeds: the files the lockfile names now, and the tool's own cache they go to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    /// The tool's own cache folder.
    pub into: PathBuf,
    /// The files, each once.
    pub files: Vec<SeedFile>,
}

/// K-45: one file a run seeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedFile {
    /// Its row's path, from the part's folder.
    pub path: String,
    /// Its name in the tool's cache folder: one plain name.
    pub name: String,
    /// The SHA-256 the lockfile in the checkout gives it now, when it gives one.
    pub sha256: Option<Hex32>,
}

/// One package manager's part.
pub trait Part: Sync {
    /// Its name, the part's folder's and rows file's.
    fn name(&self) -> &'static str;
    /// The tool, as a person names it in a line.
    fn tool(&self) -> &'static str;
    /// K-4: the files of its manifest set among `tracked` (the checkout's tracked paths, from its
    /// root, `/`-separated).
    fn manifests(&self, tracked: &[String]) -> Vec<String>;
    /// D11: the folder of a cache the job filled (a Gradle user home, a Maven local repository)
    /// whose files a harvest takes, and the folder under the part they go to. `None` when `cache`
    /// isn't one this part reads.
    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)>;
    /// K-5: whether a harvest keeps the file at `path` (from the harvest root).
    fn keep(&self, path: &str) -> bool;
    /// K-3: the package a kept file is of, as the tool names it, where its path says.
    fn package(&self, path: &str) -> Option<String>;
    /// What checks its bytes when a tool reads them.
    fn verified_by(&self) -> VerifiedBy;
    /// K-7: what checks its bytes for a bake whose manifest set is `manifests` (path and text):
    /// a part verified by a lockfile only where its lockfiles hash its packages.
    fn verified_by_for(&self, manifests: &[(String, String)]) -> VerifiedBy {
        let _ = manifests;
        self.verified_by()
    }
    /// K-3: each kept file's `locked`, by its path from the harvest root, as the lockfiles of
    /// `manifests` (path and text) write it.
    fn locked(&self, manifests: &[(String, String)]) -> BTreeMap<String, String> {
        let _ = manifests;
        BTreeMap::new()
    }
    /// K-10: how the run whose variables are `env`, in `checkout`, reads the part at `dir`.
    fn reader(&self, dir: &Path, checkout: &Path, env: Env<'_>) -> Reader;
}

/// Every part this hushrun knows, by name.
pub static ALL: &[&dyn Part] = &[
    &crate::parts::bundler::Bundler,
    &crate::parts::gradle::Gradle,
    &crate::parts::maven::Maven,
];

/// The part named `name`.
pub fn by_name(name: &str) -> Option<&'static dyn Part> {
    ALL.iter().copied().find(|p| p.name() == name)
}

/// The manifest set of every part among `tracked`, for the parts that have one: which package
/// managers a checkout uses.
pub fn manifests(tracked: &[String]) -> Vec<(&'static dyn Part, Vec<String>)> {
    ALL.iter()
        .map(|p| (*p, p.manifests(tracked)))
        .filter(|(_, m)| !m.is_empty())
        .collect()
}

/// The file name of `path`.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether `path` has a component that ends in `-SNAPSHOT`.
pub(crate) fn has_snapshot(path: &str) -> bool {
    path.split('/').any(|c| c.ends_with("-SNAPSHOT"))
}

/// Whether a folder's path can ride in a variable that is split into words: no white space, no
/// quote, no backslash.
pub(crate) fn splits_safely(dir: &Path) -> bool {
    dir.to_str().is_some_and(|s| {
        !s.chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\'))
    })
}
