//! Finding the content a command works on: the repository or directory, the snapshot,
//! `hushrun.yaml`, and the scope digests.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow};

use hushrun_core::digest::Digest;
use hushrun_core::event::SourceInfo;
use hushrun_core::names::ScopeName;
use hushrun_core::policy::{POLICY_FILE, Policy};
use hushrun_core::scope::{External, Scope, Scopes};
use hushrun_vcs::{Detected, Listing, Snapshot, Vcs, digest_scopes};

use crate::cli::{GlobalOpts, ScopeOpts};
use crate::exit::Failure;
use crate::results::ExternalInput;

/// `hushrun.yaml` as it is at git ref `reference` (`--config-ref`, T-5): for a pull request,
/// as its base branch has it, reviewed, rather than as the change under evaluation says.
fn policy_at(vcs: &dyn Vcs, reference: &str) -> Result<Policy, Failure> {
    if reference.is_empty()
        || reference.starts_with('-')
        || reference.contains([':', '\0'])
        || reference.chars().any(char::is_whitespace)
    {
        return Err(Failure::usage(anyhow!(
            "--config-ref {reference:?} isn't a git ref such as origin/main"
        )));
    }
    let bytes = vcs.read_file_at(reference, POLICY_FILE).map_err(|e| {
        Failure::usage(anyhow!(
            "reading {POLICY_FILE} at {reference} (--config-ref): {e}. In a shallow checkout, fetch the base branch first, such as `git fetch --depth=1 origin main`"
        ))
    })?;
    Ok(Policy::parse(
        &format!("{POLICY_FILE}@{reference}"),
        &bytes,
    )?)
}

/// The directory hushrun runs in: `-C`, or the current directory.
pub fn start_dir(global: &GlobalOpts) -> Result<PathBuf, Failure> {
    let cwd = std::env::current_dir().context("reading the current directory")?;
    Ok(match &global.repo {
        Some(dir) => cwd.join(dir),
        None => cwd,
    })
}

/// `$HUSHRUN_CACHE_DIR`, or `<OS cache dir>/hushrun`. An empty `HUSHRUN_CACHE_DIR` disables
/// caching.
pub fn cache_dir() -> Option<PathBuf> {
    match hushrun_ci::var_os("HUSHRUN_CACHE_DIR") {
        Some(dir) if dir.is_empty() => None,
        Some(dir) => Some(PathBuf::from(dir)),
        None => dirs::cache_dir().map(|d| d.join(hushrun_core::PRODUCT)),
    }
}

/// Where a command reads content from.
pub enum Source<'a> {
    /// The repository at or above the start directory, at a snapshot.
    Repo(Snapshot),
    /// A plain directory.
    Dir(&'a Path),
}

/// The content a command works on.
pub struct Content {
    pub vcs: Box<dyn Vcs>,
    pub listing: Listing,
    /// `hushrun.yaml`, if the content has one (or `--config` names one).
    pub policy: Option<Policy>,
    /// The policy's scopes, or the single scope `default`.
    pub scopes: Scopes,
    options: hushrun_vcs::Options,
}

/// One scope of the content, with its digest.
#[derive(Debug, Clone)]
pub struct Subject {
    pub scope: ScopeName,
    pub digest: Digest,
    pub source: SourceInfo,
}

/// \[ADR 0062\] Names, once per command, what hushrun.yaml says that this release reads past, so
/// that it can be fixed: a file from before 0.9 keeps skipping all the same.
fn say_retired(global: &GlobalOpts, policy: &Policy) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if policy.retired.is_empty()
        || global.quiet > 0
        || SAID.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        return;
    }
    for retired in &policy.retired {
        eprintln!("warning: {}", retired.notice(&policy.file));
    }
}

impl Content {
    /// Opens the VCS, lists the snapshot, and loads `hushrun.yaml` (T-5): `--config` if given,
    /// \[M8\] else as it is at `--config-ref`, else as it is in the content being evaluated.
    pub fn open(global: &GlobalOpts, source: Source<'_>) -> Result<Content, Failure> {
        let options = hushrun_vcs::Options {
            cache_dir: cache_dir(),
            jobs: global.jobs.unwrap_or(0),
            git_reader: global.git_reader,
        };
        let start = start_dir(global)?;
        let (vcs, snapshot): (Box<dyn Vcs>, Snapshot) = match source {
            Source::Dir(dir) => {
                let dir = start.join(dir);
                (
                    Box::new(hushrun_vcs::dir::Dir::open(&dir, options.clone())?),
                    Snapshot::default(),
                )
            }
            Source::Repo(snapshot) => match hushrun_vcs::detect(&start)? {
                Detected::Git { root } => (
                    Box::new(hushrun_vcs::git::Git::open(&root, options.clone())?),
                    snapshot,
                ),
                Detected::None => {
                    return Err(Failure::usage(hushrun_vcs::Error::NoVcs { start }));
                }
            },
        };
        let listing = vcs.list(&snapshot)?;
        let policy = match (&global.config, &global.config_ref) {
            (None, Some(reference)) => Some(policy_at(vcs.as_ref(), reference)?),
            (Some(path), _) => {
                let path = start.join(path);
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("reading {}", path.display()))
                    .map_err(Failure::usage)?;
                Some(Policy::parse(&path.display().to_string(), &bytes)?)
            }
            (None, None) => match listing.find(POLICY_FILE) {
                Some(entry) => {
                    let bytes = vcs.read_entry(entry)?;
                    Some(Policy::parse(POLICY_FILE, &bytes)?)
                }
                None => {
                    tracing::debug!("no {POLICY_FILE}: one scope, `default`, covering everything");
                    None
                }
            },
        };
        if let Some(policy) = &policy {
            say_retired(global, policy);
        }
        let scopes = policy
            .as_ref()
            .map_or_else(|| Scopes::default_scope(Vec::new()), |p| p.scopes.clone());
        Ok(Content {
            vcs,
            listing,
            policy,
            scopes,
            options,
        })
    }

    /// The policy; commands other than `digest` need one.
    pub fn policy(&self) -> Result<&Policy, Failure> {
        self.policy.as_ref().ok_or_else(|| {
            Failure::usage(anyhow!(
                "no {POLICY_FILE} in this revision; `hushrun init --pipeline` writes a starter one"
            ))
        })
    }

    /// The scopes selected by `--scope` and `--all-scopes`. Without either, every scope when
    /// `all_by_default`, else the only scope, or a usage error when there are several.
    pub fn select(&self, opts: &ScopeOpts, all_by_default: bool) -> Result<Vec<&Scope>, Failure> {
        let scopes = &self.scopes;
        if opts.all_scopes || (opts.scopes.is_empty() && (all_by_default || scopes.len() == 1)) {
            return Ok(scopes.iter().collect());
        }
        let names = || {
            scopes
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        if opts.scopes.is_empty() {
            return Err(Failure::usage(anyhow!(
                "this policy has several scopes ({}); pass --scope NAME or --all-scopes",
                names()
            )));
        }
        let mut selected = Vec::new();
        for name in &opts.scopes {
            let scope = scopes.get(name.as_str()).ok_or_else(|| {
                Failure::usage(anyhow!(
                    "unknown scope `{name}`; the policy has {}",
                    names()
                ))
            })?;
            if !selected.contains(&scope) {
                selected.push(scope);
            }
        }
        Ok(selected)
    }

    /// \[ADR 0037\] What `scope` declares outside the repository, each digested as it is now, by
    /// path (C-9). Whatever keeps one from being read is an error: a key without it would say
    /// more than hushrun knows, so the command runs without skipping (C-14).
    pub fn external(&self, scope: &ScopeName) -> anyhow::Result<Vec<ExternalInput>> {
        let Some(scope) = self.scopes.get(scope.as_str()) else {
            return Ok(Vec::new());
        };
        scope
            .external
            .iter()
            .map(|external| self.digest_external(external))
            .collect()
    }

    fn digest_external(&self, external: &External) -> anyhow::Result<ExternalInput> {
        let path = &external.path;
        let dir = self.vcs.root().join(path);
        let dir = std::fs::canonicalize(&dir)
            .with_context(|| format!("the external input {path}: reading {}", dir.display()))?;
        let top = match hushrun_vcs::detect(&dir) {
            Ok(Detected::Git { root }) => Some(root),
            _ => None,
        };
        if top.as_deref() != Some(dir.as_path()) {
            anyhow::bail!(
                "the external input {path}: {} isn't the top of a git checkout",
                dir.display()
            );
        }
        let vcs = hushrun_vcs::git::Git::open(&dir, self.options.clone())
            .with_context(|| format!("the external input {path}"))?;
        let listing = vcs
            .list(&Snapshot::Worktree)
            .with_context(|| format!("the external input {path}"))?;
        let scopes = Scopes::new(
            vec![Scope {
                name: ScopeName::default_scope(),
                include: external.include.clone(),
                exclude: external.exclude.clone(),
                external: Vec::new(),
            }],
            self.scopes.global_exclude().to_vec(),
        );
        let selected: Vec<&Scope> = scopes.iter().collect();
        let run = digest_scopes(&vcs, &listing, &scopes, &selected, false)
            .with_context(|| format!("the external input {path}"))?;
        Ok(ExternalInput {
            path: path.clone(),
            digest: run.scopes[0].digest,
        })
    }

    /// The source information events record (E-4).
    pub fn source_info(&self) -> SourceInfo {
        let r = &self.listing.revision;
        SourceInfo {
            vcs: r.vcs.clone(),
            revision: r.revision.clone(),
            reference: r.reference.clone(),
            tree: r.tree.clone(),
            object_format: r.object_format.clone(),
        }
    }

    /// The one selected scope and its digest: `--digest` if given (no recomputation), else
    /// computed.
    pub fn subject(&self, opts: &ScopeOpts, digest: Option<Digest>) -> Result<Subject, Failure> {
        let selected = self.select(opts, false)?;
        let [scope] = selected.as_slice() else {
            return Err(Failure::usage(anyhow!(
                "this command records events for one scope: pass --scope NAME"
            )));
        };
        let digest = match digest {
            Some(digest) => digest,
            None => {
                let run = digest_scopes(
                    self.vcs.as_ref(),
                    &self.listing,
                    &self.scopes,
                    &[scope],
                    false,
                )?;
                run.scopes[0].digest
            }
        };
        Ok(Subject {
            scope: scope.name.clone(),
            digest,
            source: self.source_info(),
        })
    }
}
