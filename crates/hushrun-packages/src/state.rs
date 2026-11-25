//! A package store's state for a run (K-7, K-11, K-12): one function's answer, [`state`], which a
//! run, `hushrun packages status`, `env`, `exec` and `restore` all call.
//!
//! | State | Means | A run |
//! |---|---|---|
//! | absent | no store is named, or `HUSHRUN_PACKAGES=off` | reads nothing, says nothing |
//! | invalid | the folder or a file in it can't be read, is of a version this one doesn't read, or holds what a store never does | ignores it, one line |
//! | wrong repository | baked for another repository than the run's | ignores it, one line |
//! | wrong platform | baked on another platform | ignores it, one line |
//! | untrusted | it has parts only a signature verifies, and nothing verifies it | ignores those parts, one line |
//! | behind | a part's inputs aren't the checkout's now | reads it: what it holds is still what the signer signed |
//! | current | none of the above | reads it |
//!
//! Trust is never read from the file. What lets a run read a part is evidence (K-7): for a part a
//! lockfile verifies, the lockfile in the checkout, checked file by file as it is seeded; for any
//! other, a signature by a signer the repository names over these very `packages.json` bytes, or
//! the person's say-so in a run that writes nothing others trust.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use hushrun_core::blocks::Hex32;

use crate::store::{self, Depth, PACKAGES_JSON, PARTS, Packages, PartEntry, ROWS, Row, VerifiedBy};
use crate::{DEFAULT_LOWER, LOWER, LOWER_UNTRUSTED, OFF};

/// K-7: a signature the run verified as it began, by a signer its repository names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    /// The signer, in words.
    pub signer: String,
    /// The SHA-256 of the `packages.json` it covers.
    pub packages_json: Hex32,
}

/// K-7: what lets a run read a part only a signature verifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    /// A signature by a signer the repository names, over these `packages.json` bytes.
    Signed(Signed),
    /// The person's say-so (`HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1`), in a run that writes nothing
    /// others trust.
    Accepted,
}

/// What a run knows of itself, for [`state`].
pub struct Here<'a> {
    /// The run's repository, as a run names it.
    pub repository: Option<&'a str>,
    /// This machine's platform, in a tag's words.
    pub platform: &'a str,
    /// The signature the run verified, if any ([`Signed::packages_json`] says for which bytes).
    pub signed: Option<&'a Signed>,
    /// Whether the run accepts a store nothing verifies ([`accepts_untrusted`]).
    pub untrusted_ok: bool,
    /// K-8: whether the run may write what others trust; then every file of a part is hashed
    /// before a tool reads it.
    pub may_write_trusted: bool,
    /// K-11: the store is the one at the default folder, which no variable named.
    pub default: bool,
    /// A part's inputs as the checkout has them now, where the part knows them (K-12).
    pub inputs: &'a dyn Fn(&PartEntry) -> Option<Hex32>,
}

/// A part a run reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPart {
    /// What `packages.json` says of it.
    pub entry: PartEntry,
    /// Its rows.
    pub rows: Vec<Row>,
    /// Its folder.
    pub dir: PathBuf,
    /// K-12: its inputs aren't the checkout's now.
    pub behind: bool,
}

/// A store a run reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lower {
    /// Its folder.
    pub folder: PathBuf,
    /// Its `packages.json`.
    pub packages: Packages,
    /// The SHA-256 of `packages.json`'s bytes.
    pub packages_json: Hex32,
    /// What verifies its signature parts, if anything.
    pub evidence: Option<Evidence>,
    /// The parts a run reads.
    pub parts: Vec<ReadPart>,
    /// What a run says about it: parts it left out, and why.
    pub notes: Vec<String>,
}

impl Lower {
    /// The part named `name`, when the run reads it.
    pub fn part(&self, name: &str) -> Option<&ReadPart> {
        self.parts.iter().find(|p| p.entry.part == name)
    }
}

/// K-12: a store's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// No store is named, or readers are off.
    Absent,
    /// It can't be read, or holds what a store never does.
    Invalid {
        /// Its folder.
        folder: PathBuf,
        /// Why.
        why: String,
    },
    /// Baked for another repository.
    WrongRepository {
        /// Its folder.
        folder: PathBuf,
        /// The repository it was baked for.
        baked: Option<String>,
        /// The run's.
        here: Option<String>,
    },
    /// Baked on another platform.
    WrongPlatform {
        /// Its folder.
        folder: PathBuf,
        /// Its platform.
        baked: String,
        /// This one.
        here: String,
    },
    /// Its parts need a signature and nothing verifies it.
    Untrusted {
        /// Its folder.
        folder: PathBuf,
    },
    /// Read, but a part's inputs aren't the checkout's now.
    Behind(Lower),
    /// Read.
    Current(Lower),
}

impl State {
    /// The store a run reads, if any.
    pub fn lower(&self) -> Option<&Lower> {
        match self {
            State::Behind(lower) | State::Current(lower) => Some(lower),
            _ => None,
        }
    }

    /// The word `hushrun packages status --format json` writes.
    pub fn word(&self) -> &'static str {
        match self {
            State::Absent => "absent",
            State::Invalid { .. } => "invalid",
            State::WrongRepository { .. } => "wrong_repository",
            State::WrongPlatform { .. } => "wrong_platform",
            State::Untrusted { .. } => "untrusted",
            State::Behind(_) => "behind",
            State::Current(_) => "current",
        }
    }

    /// What `hushrun packages status` says of it.
    pub fn words(&self) -> String {
        match self {
            State::Absent => format!("none ({LOWER} names none, and {DEFAULT_LOWER} holds none)"),
            State::Invalid { folder, why } => format!("{} is ignored: {why}", folder.display()),
            State::WrongRepository {
                folder,
                baked,
                here,
            } => format!(
                "{} is ignored: it was baked for {}, and this run is {}",
                folder.display(),
                baked.as_deref().unwrap_or("no repository"),
                here.as_deref().unwrap_or("no repository"),
            ),
            State::WrongPlatform {
                folder,
                baked,
                here,
            } => format!(
                "{} is ignored: it was baked for {baked}, and this is {here}",
                folder.display()
            ),
            State::Untrusted { folder } => format!(
                "{} is ignored: nothing here verifies it (no signature by a signer hushrun.yaml names); a run that writes nothing others trust reads it unverified with {LOWER_UNTRUSTED}=1",
                folder.display()
            ),
            State::Behind(lower) => format!(
                "{}, behind: the build files changed since it was baked ({})",
                lower.folder.display(),
                evidence_words(lower)
            ),
            State::Current(lower) => format!(
                "{}, current ({})",
                lower.folder.display(),
                evidence_words(lower)
            ),
        }
    }

    /// The lines a run says about it: why it is ignored, or what of it isn't read.
    pub fn lines(&self) -> Vec<String> {
        match self {
            State::Absent => Vec::new(),
            State::Behind(lower) | State::Current(lower) => {
                let mut lines: Vec<String> = lower
                    .notes
                    .iter()
                    .map(|note| format!("packages: {note}"))
                    .collect();
                for part in lower.parts.iter().filter(|p| p.behind) {
                    lines.push(format!(
                        "packages: {}: behind: the build files changed since the bake, so {} downloads what the baked packages lack",
                        part.entry.part, part.entry.part
                    ));
                }
                lines
            }
            _ => vec![format!("packages: the baked packages {}", self.words())],
        }
    }
}

fn evidence_words(lower: &Lower) -> String {
    match &lower.evidence {
        Some(Evidence::Signed(signed)) => {
            format!("signed by {}, a signer the repository names", signed.signer)
        }
        Some(Evidence::Accepted) => format!("unverified, read because {LOWER_UNTRUSTED}=1"),
        None => "only the parts a lockfile verifies".to_string(),
    }
}

/// K-11: the store a run's variables name: `HUSHRUN_PACKAGES_LOWER`, set and empty for none;
/// unset, [`DEFAULT_LOWER`] when it holds a `packages.json`. `HUSHRUN_PACKAGES=off`: none. The
/// second value says whether it is the default folder, which no variable named.
pub fn named(env: &dyn Fn(&str) -> Option<OsString>) -> Option<(PathBuf, bool)> {
    if env(OFF).is_some_and(|v| v == "off") {
        return None;
    }
    match env(LOWER) {
        Some(value) if value.is_empty() => None,
        Some(value) => Some((PathBuf::from(value), false)),
        None => default_lower(Path::new(DEFAULT_LOWER)).map(|p| (p, true)),
    }
}

/// `folder` when it holds a `packages.json`: one `stat` when nothing is there.
pub fn default_lower(folder: &Path) -> Option<PathBuf> {
    (cfg!(unix) && folder.join(PACKAGES_JSON).is_file()).then(|| folder.to_path_buf())
}

/// K-7: whether a run accepts a store nothing verifies: `HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1`, and
/// only in a run that writes nothing others trust.
pub fn accepts_untrusted(env: &dyn Fn(&str) -> Option<OsString>, may_write_trusted: bool) -> bool {
    !may_write_trusted && env(LOWER_UNTRUSTED).is_some_and(|v| v == "1")
}

/// K-11: why the store at the default folder, which nobody named, isn't read: it, its
/// `packages.json`, `rows/`, `parts/`, or a folder above any row is a link, belongs to another user
/// than root or this one, or another user may write it. A read-only mount needs neither. `None`
/// when it may be read.
pub fn default_refused(folder: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        refused_for(folder, rustix::process::geteuid().as_raw(), &|p| {
            rustix::fs::statvfs(p)
                .is_ok_and(|s| s.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY))
        })
    }
    #[cfg(not(unix))]
    {
        let _ = folder;
        Some("a package store is read from the default folder only on Unix".into())
    }
}

#[cfg(unix)]
fn refused_for(folder: &Path, me: u32, read_only: &dyn Fn(&Path) -> bool) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    let mut paths = vec![
        folder.to_path_buf(),
        folder.join(PACKAGES_JSON),
        folder.join(ROWS),
        folder.join(PARTS),
    ];
    folders_under(&folder.join(PARTS), &mut paths);
    for path in paths {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            return Some(format!("{} can't be read", path.display()));
        };
        if meta.file_type().is_symlink() {
            return Some(format!("{} is a link", path.display()));
        }
        if read_only(&path) {
            continue;
        }
        if meta.uid() != 0 && meta.uid() != me {
            return Some(format!(
                "{} belongs to another user, neither root nor this one: extract it as root or as the user who runs builds, or `chown -R` it to one of them; a volume mounted read-only is read whoever owns it, and {LOWER}={} reads it anyway",
                path.display(),
                folder.display()
            ));
        }
        if meta.mode() & 0o002 != 0 {
            return Some(format!(
                "anyone may write {}: `chmod -R go-w {}` fixes it",
                path.display(),
                folder.display()
            ));
        }
        if meta.mode() & 0o020 != 0 {
            return Some(format!(
                "its group may write {}: `chmod -R g-w {}` fixes it (or name it with {LOWER} to read it anyway)",
                path.display(),
                folder.display()
            ));
        }
    }
    None
}

/// Every folder under `dir`, never through a link.
#[cfg(unix)]
fn folders_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            out.push(entry.path());
            folders_under(&entry.path(), out);
        }
    }
}

/// K-12: the store at `folder` for this run. Checks the folder by names and sizes (K-1), and when
/// the run may write what others trust, every file of every part it reads by its SHA-256 (K-8),
/// leaving out a part on its first difference.
pub fn state(folder: Option<&Path>, here: &Here<'_>) -> State {
    let Some(folder) = folder else {
        return State::Absent;
    };
    let invalid = |why: String| State::Invalid {
        folder: folder.to_path_buf(),
        why,
    };
    if !folder.is_absolute() {
        return invalid("a package store is named by an absolute path".into());
    }
    if here.default
        && let Some(why) = default_refused(folder)
    {
        return invalid(why);
    }
    let (packages, digest) = match Packages::read_bytes(folder) {
        Ok(read) => read,
        Err(why) => return invalid(why.0),
    };
    if packages.repository.is_some()
        && here.repository.is_some()
        && packages.repository.as_deref() != here.repository
    {
        return State::WrongRepository {
            folder: folder.to_path_buf(),
            baked: packages.repository,
            here: here.repository.map(str::to_string),
        };
    }
    if packages.platform != here.platform {
        return State::WrongPlatform {
            folder: folder.to_path_buf(),
            baked: packages.platform,
            here: here.platform.to_string(),
        };
    }
    let all_rows = match store::check_contents(folder, Some(&packages), Depth::Names) {
        Ok(rows) => rows,
        Err(why) => return invalid(why.0),
    };
    let evidence = match here.signed {
        Some(signed) if signed.packages_json == digest => Some(Evidence::Signed(signed.clone())),
        _ if here.untrusted_ok => Some(Evidence::Accepted),
        _ => None,
    };
    let mut parts = Vec::new();
    let mut notes = Vec::new();
    let mut untrusted = false;
    for (entry, rows) in packages.parts.iter().zip(all_rows) {
        if entry.verified_by == VerifiedBy::Signature && evidence.is_none() {
            untrusted = true;
            continue;
        }
        let dir = store::part_dir(folder, &entry.part);
        if here.may_write_trusted
            && let Err(why) = store::verify_part(folder, &entry.part, &rows)
        {
            notes.push(format!(
                "{}: ignored: {why}, so it isn't the file the baked packages were signed with",
                entry.part
            ));
            continue;
        }
        let behind = (here.inputs)(entry).is_some_and(|now| now != entry.inputs);
        parts.push(ReadPart {
            entry: entry.clone(),
            rows,
            dir,
            behind,
        });
    }
    if untrusted && parts.is_empty() {
        return State::Untrusted {
            folder: folder.to_path_buf(),
        };
    }
    if untrusted {
        notes.push(format!(
            "the baked packages {}: only the parts a lockfile verifies are read; nothing here verifies the others (no signature by a signer hushrun.yaml names)",
            folder.display()
        ));
    }
    let behind = parts.iter().any(|p| p.behind);
    let lower = Lower {
        folder: folder.to_path_buf(),
        packages,
        packages_json: digest,
        evidence,
        parts,
        notes,
    };
    if behind {
        State::Behind(lower)
    } else {
        State::Current(lower)
    }
}

#[cfg(test)]
mod tests;
