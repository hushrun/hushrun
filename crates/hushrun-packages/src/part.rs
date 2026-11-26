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

/// What [`Part::locked_by`] gives a bake: for a file's path under the part's folder and its
/// bytes, the lockfile's own hash of it, as the lockfile writes it.
pub type LockedBy = Box<dyn Fn(&str, &[u8]) -> Option<String>>;

/// K-20: one package a bake downloads by the lockfile, with no tool run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    /// Where the registry serves it.
    pub url: String,
    /// Its path under the part's folder, first folder included.
    pub path: String,
    /// The package, as the lockfile names it.
    pub package: String,
    /// The lockfile's own hash of it, as the lockfile writes it.
    pub locked: String,
}

/// K-20: what a bake downloads for a part by the lockfile, and what it leaves out, by why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Downloads {
    /// The packages to download.
    pub list: Vec<Download>,
    /// Packages the lockfile names that the bake doesn't download: how many, and why.
    pub left_out: Vec<(usize, String)>,
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
    /// K-3: for a part the lockfile verifies, the lockfile's own hash of each file, from the
    /// lockfiles `manifests` of `checkout`: what a bake writes as a row's `locked` where the file's
    /// path alone doesn't say it.
    fn locked_by(&self, _checkout: &Path, _manifests: &[String]) -> Option<LockedBy> {
        None
    }
    /// K-5: why a bake leaves this part out though its tool's cache holds files: a lockfile among
    /// `manifests` this hushrun doesn't read, whose hashes would verify nothing. `None` to take it.
    fn lockfile_unread(&self, _checkout: &Path, _manifests: &[String]) -> Option<String> {
        None
    }
    /// K-20: what a bake downloads for this part by the lockfiles `manifests` of `checkout`, with
    /// no tool run. `None` for a part a fetch or a harvest fills; an error names a lockfile this
    /// hushrun doesn't read.
    fn downloads(
        &self,
        _checkout: &Path,
        _manifests: &[String],
    ) -> Option<Result<Downloads, String>> {
        None
    }
    /// K-10, mechanism C: seeds `read`'s files under its folder `under` into `dest`, each checked
    /// by `admit`. Most parts copy each file to the same path below `dest`.
    fn seed(
        &self,
        read: &crate::state::ReadPart,
        under: &str,
        dest: &Path,
        admit: &dyn Fn(&str, &[u8]) -> Admit,
    ) -> std::io::Result<crate::seed::Seeded> {
        crate::seed::seed(read, under, dest, admit)
    }
    /// K-10, mechanism C: the variables an install needs to read what a seed put in `dest`, for
    /// `checkout`: none where the seed fills the tool's own cache.
    fn seed_env(&self, _checkout: &Path, _dest: &Path) -> Vec<(String, OsString)> {
        Vec::new()
    }
}

/// Every part this hushrun knows, by name.
pub static ALL: &[&dyn Part] = &[
    &crate::parts::bun::Bun,
    &crate::parts::gradle::Gradle,
    &crate::parts::maven::Maven,
    &crate::parts::npm::Npm,
    &crate::parts::pnpm::Pnpm,
    &crate::parts::yarn::Yarn,
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
