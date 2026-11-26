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

/// K-3: the files a lockfile names by hash, by their SHA-256: the file's name where the lockfile
/// gives one, and the hash as the lockfile writes it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Locked(BTreeMap<Hex32, (Option<String>, String)>);

impl Locked {
    /// Adds a file the lockfile names: `sha256` as 64 hex characters, `file` its name where the
    /// lockfile gives one. A hash that isn't SHA-256's is left out.
    pub fn add(&mut self, sha256: &str, file: Option<&str>) {
        if let Some(hex) = Hex32::parse(&sha256.to_ascii_lowercase()) {
            self.0
                .insert(hex, (file.map(str::to_string), format!("sha256:{sha256}")));
        }
    }

    /// Adds a file the lockfile names, with its hash as that lockfile writes it (Bundler's
    /// `sha256=<hex>`), where [`Locked::add`]'s `sha256:<hex>` isn't its words.
    pub fn add_as(&mut self, sha256: &Hex32, file: Option<&str>, written: String) {
        self.0.insert(*sha256, (file.map(str::to_string), written));
    }

    /// The hash as the lockfile writes it, when it names a file of this name with these bytes.
    pub fn of(&self, file: &str, sha256: &Hex32) -> Option<&str> {
        let (named, written) = self.0.get(sha256)?;
        named
            .as_deref()
            .is_none_or(|n| n == file)
            .then_some(written.as_str())
    }

    /// What a seed does with the file at `path` whose bytes are `bytes`: copy it when the
    /// lockfile names it with these bytes.
    pub fn admit(&self, path: &str, bytes: &[u8]) -> Admit {
        if self.of(file_name(path), &Hex32::sha256(bytes)).is_some() {
            Admit::Copy
        } else {
            Admit::NotLocked
        }
    }
}

/// K-32: how a bake fetches a part with the tool's own download-only command.
pub type Download = Option<Result<Vec<Vec<OsString>>, String>>;

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
    /// K-10, mechanism C: [`Part::admit`] for a part whose `packages.json` says `verified_by`,
    /// for a part whose class follows its lockfiles: one a lockfile verifies admits no file the
    /// lockfile doesn't hash.
    fn admit_for(&self, checkout: &Path, manifests: &[String], verified_by: VerifiedBy) -> Admits {
        let _ = verified_by;
        self.admit(checkout, manifests)
    }
    /// K-3: the lockfile's own hash of the file at `path`, as a lockfile writes it, where the file
    /// is the artifact that hash is of.
    fn locked(&self, _path: &str) -> Option<String> {
        None
    }
    /// K-3, K-5: for a part whose lockfile names each file by its SHA-256, the files the lockfiles
    /// `manifests` in `checkout` name. A bake keeps only these, and gives each row its `locked`.
    /// `None` for a part whose files the lockfile doesn't name so.
    fn lockfile(&self, _checkout: &Path, _manifests: &[String]) -> Option<Locked> {
        None
    }
    /// K-32: the tool's own download-only commands, which fetch the part into `into` from the
    /// manifest set in `checkout`. A bake runs them with or without its COMMAND. `None` for a tool
    /// that has none, an error saying why it can't fetch here.
    fn download(&self, _checkout: &Path, _manifests: &[String], _into: &Path) -> Download {
        None
    }
    /// K-6: the variables a bake's COMMAND gets so that the tool downloads into `cache`, a folder
    /// of the bake's own.
    fn fetch_vars(&self, _cache: &Path) -> Vec<(&'static str, OsString)> {
        Vec::new()
    }
}

/// Every part this hushrun knows, by name.
pub static ALL: &[&dyn Part] = &[
    &crate::parts::bundler::Bundler,
    &crate::parts::composer::Composer,
    &crate::parts::gradle::Gradle,
    &crate::parts::maven::Maven,
    &crate::parts::npm::Npm,
    &crate::parts::pip::Pip,
    &crate::parts::pnpm::Pnpm,
    &crate::parts::poetry::Poetry,
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

/// K-3: the package a Python wheel or source archive is of, `name==version`, from its file name.
pub(crate) fn python_package(path: &str) -> Option<String> {
    let name = file_name(path);
    let (name, version) = if let Some(stem) = name.strip_suffix(".whl") {
        let mut words = stem.split('-');
        (words.next()?, words.next()?)
    } else {
        let stem = [".tar.gz", ".zip", ".tar.bz2", ".tgz"]
            .iter()
            .find_map(|ext| name.strip_suffix(ext))?;
        stem.rsplit_once('-')?
    };
    (!name.is_empty() && !version.is_empty()).then(|| format!("{name}=={version}"))
}

/// Whether a folder's path can ride in a variable that is split into words: no white space, no
/// quote, no backslash.
pub(crate) fn splits_safely(dir: &Path) -> bool {
    dir.to_str().is_some_and(|s| {
        !s.chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\'))
    })
}
