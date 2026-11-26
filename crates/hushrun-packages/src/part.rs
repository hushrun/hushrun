//! A part (K-10): one package manager's side of a package store: which files of a checkout are its
//! manifest set, what of its cache a bake keeps, and how a run points the tool at it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
    /// Mechanism C: hushrun copies the part's files into the tool's own cache, where
    /// [`Part::seed_into`] says, before an install (`hushrun packages exec` and `restore`).
    /// `hushrun run` gives the tool nothing: by the time a check runs, the install is done.
    Seed,
    /// The tool is left alone, with this line (`packages: <part>: left alone: …`).
    LeftAlone(String),
}

/// K-10, mechanism C: what a seed does with one file of a part, by its path under the part's
/// folder the seed copies and its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    /// Copy it.
    Copy,
    /// The lockfile in the checkout doesn't name it now: leave it.
    NotLocked,
    /// Its bytes aren't what the lockfile says: leave it, with a line.
    Changed,
}

/// What [`Part::admit`] gives a seed: for a file's path and bytes, what it does with it.
pub type Admits = Box<dyn Fn(&str, &[u8]) -> Admit>;

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
    /// K-10: how the run whose variables are `env`, in `checkout`, reads the part at `dir`.
    fn reader(&self, dir: &Path, checkout: &Path, env: Env<'_>) -> Reader;
    /// K-10, mechanism C: the tool's own cache a seed copies into, for the run whose variables
    /// are `env` in `checkout`, and the folder of the part whose files go there. Only a part whose
    /// reader is [`Reader::Seed`] has one.
    fn seed_into(
        &self,
        _checkout: &Path,
        _env: Env<'_>,
    ) -> Result<(PathBuf, &'static str), String> {
        Err(format!("{} is read in place", self.tool()))
    }
    /// K-10, mechanism C: which files a seed copies, for the lockfiles `manifests` of `checkout`.
    /// Every file, for a part a signature verifies.
    fn admit(&self, _checkout: &Path, _manifests: &[String]) -> Admits {
        Box::new(|_, _| Admit::Copy)
    }
    /// K-3: the lockfile's own hash of the file at `path`, as a lockfile writes it, where the file
    /// is the artifact that hash is of.
    fn locked(&self, _path: &str) -> Option<String> {
        None
    }
}

/// Every part this hushrun knows, by name.
pub static ALL: &[&dyn Part] = &[
    &crate::parts::gradle::Gradle,
    &crate::parts::maven::Maven,
    &crate::parts::npm::Npm,
    &crate::parts::pnpm::Pnpm,
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
