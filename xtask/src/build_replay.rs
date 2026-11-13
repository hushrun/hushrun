//! `xtask build-replay`: replays a repository's commits through hushrun's rustc wrapper and
//! reports what was, or would have been, reused (build blocks, docs/spec/10-build-blocks.md).
//!
//! Each commit is exported with `git archive` into a folder of its own under `--work`, built
//! there with COMMAND in a cleaned environment, and the wrapper's log for that run
//! ([`Current::entries`], read before [`Session::finish`]) is counted. The settings file, the
//! store, the wrapper's link and every session live under `--work`. The tool never runs
//! hushrun's own commands: the binary is only ever started by cargo, as the wrapper, through the
//! session's link. Its numbers decide whether reuse can be turned on, so a difference between a
//! block and a fresh compile makes the command exit 1.
//!
//! [`Current::entries`]: hushrun_build::session::Current::entries
//! [`Session::finish`]: hushrun_build::session::Session::finish

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use hushrun_build::model::{Class, Mode, NotStored, Outcome};
use hushrun_build::session::{self, Entry, Start};
use hushrun_build::settings::{self, Overrides};
use hushrun_build::store::Store;
use serde_json::{Value, json};

const USAGE: &str = "\
usage: cargo xtask build-replay --repo PATH --work PATH [options]

Replays a repository's commits through hushrun's rustc wrapper and reports what was, or would
have been, reused (build blocks, docs/spec/10-build-blocks.md). Each commit is exported with
`git archive` into its own folder under --work and built there with COMMAND; the wrapper's log of
that run is counted. Nothing is written outside --work (cargo's own home aside), the hushrun
binary is only ever started by cargo as the wrapper, and --repo is only read with git.

options:
  --repo PATH          the git repository to replay (required)
  --dir REL            the folder inside it to export and build in (default: the root)
  --last N             the last N commits that touch --dir, oldest first (default: 10)
  --commits A..B       the commits in A..B that touch --dir, oldest first
  --commit SHA         this commit; repeat for more, replayed in the order given
  --command \"CMD\"      run with `sh -c` in the exported folder
                       (default: cargo test --no-run --locked --offline)
  --mode MODE          observe, on or none (default: observe); none runs without the wrapper:
                       the baseline
  --hushrun PATH       the built hushrun binary whose wrapper cargo calls (required unless
                       --mode none)
  --work PATH          scratch folder for exports, targets, the store and the report
                       (required; not inside --repo)
  --target KIND        fresh: a new target folder per commit; shared: one for all of them, as a
                       gate has; inside: cargo's own, <export>[/<dir>]/target, with
                       CARGO_TARGET_DIR not set, as in a worktree (default: fresh)
  --paths KIND         rotate: each commit exported to a new, empty path of another length;
                       fixed: one path, updated in place as a checkout is (unchanged files keep
                       their date) (default: rotate)
  --keep               keep the exports and fresh targets (default: delete each after its
                       commit's run, a fixed export after the last, a target inside an export
                       with the export; the store is always kept)
  -h, --help           this text

Exported files are dated when they are written, never with the commit's date, so a shared target
never takes a changed file for an old one. Each replay starts with no exports and an empty shared
target; the store is kept, and the report says what it held at the start.

Commands run without what cargo hands its children or what changes how rustc is called
(RUSTC_WRAPPER, RUSTC_WORKSPACE_WRAPPER, RUSTC, RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS,
CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR, CARGO_MAKEFLAGS, CARGO_INCREMENTAL, CARGO_PKG_*,
CARGO_MANIFEST_*, CARGO_CRATE_*, CARGO_BIN_*, CARGO_PRIMARY_PACKAGE, OUT_DIR), without
RUSTUP_TOOLCHAIN (the export's own rust-toolchain.toml decides) and without any HUSHRUN_*
variable, with CARGO_TARGET_DIR set to the commit's target folder (not set with --target
inside). A fixed export keeps a target inside it from one commit to the next, as a worktree does.

Writes, rewritten whole after each commit:
  WORK/replay.jsonl    one object per commit: its counts by outcome, reason and class, times,
                       bytes stored and the store's size after it
  WORK/replay.md       a table per commit with totals, every crate that differed, and every
                       `built, not stored` and `passed through` reason with three examples
  WORK/logs/           each commit's command output, times and the wrapper's raw entries

Exits 1 when any compile differed from its block: the bar is zero. A commit whose command fails
is recorded and the replay goes on.
";

/// What runs when nothing else is asked: every test binary of the workspace, built.
const DEFAULT_COMMAND: &str = "cargo test --no-run --locked --offline";

/// How many commits `--last` takes when no selection is given.
const DEFAULT_LAST: usize = 10;

/// Variables removed from the commands' environment by name, besides [`REMOVED_PREFIXES`]: what
/// cargo hands its children, what changes how rustc is called, and the toolchain that `cargo
/// xtask` itself runs under (the export's own `rust-toolchain.toml` decides, as in a checkout).
const REMOVED: &[&str] = &[
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTC",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_MAKEFLAGS",
    "CARGO_INCREMENTAL",
    "CARGO_PRIMARY_PACKAGE",
    "OUT_DIR",
    "RUSTUP_TOOLCHAIN",
];

/// Prefixes of variables removed from the commands' environment. `HUSHRUN_` keeps the owner's
/// real results, credentials, backend and build settings out of every run.
const REMOVED_PREFIXES: &[&str] = &[
    "CARGO_PKG_",
    "CARGO_MANIFEST_",
    "CARGO_CRATE_",
    "CARGO_BIN_",
    "HUSHRUN_",
];

/// Variables that point git at another repository. A hook running this tool may set them.
const GIT_LOCATION: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// The outcomes as the run's log writes them, in the order the report lists them.
const OUTCOMES: [&str; 4] = [
    "reused",
    "built-and-stored",
    "built-not-stored",
    "passed-through",
];

/// A crate with no name in the log.
const UNNAMED: &str = "(unnamed)";

/// A compile whose class wasn't read.
const UNKNOWN_CLASS: &str = "unknown";

/// A command's environment, by name.
type Env = BTreeMap<OsString, OsString>;

/// Whether the wrapper runs, and in which mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wrap {
    /// No wrapper at all: the baseline.
    None,
    /// Every compile runs; blocks are stored and compared (B-1).
    Observe,
    /// A compile whose block exists is not run (B-1).
    On,
}

impl Wrap {
    fn as_str(self) -> &'static str {
        match self {
            Wrap::None => "none",
            Wrap::Observe => "observe",
            Wrap::On => "on",
        }
    }

    /// The mode the session is started in, when there is one.
    fn mode(self) -> Option<Mode> {
        match self {
            Wrap::None => None,
            Wrap::Observe => Some(Mode::Observe),
            Wrap::On => Some(Mode::On),
        }
    }
}

/// Which commits are replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Selection {
    /// The last N commits that touch the folder, oldest first.
    Last(usize),
    /// The commits of a range that touch the folder, oldest first.
    Range(String),
    /// These commits, in this order.
    List(Vec<String>),
}

/// Where each commit's build writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Targets {
    /// A new folder beside each export, named by `CARGO_TARGET_DIR`.
    Fresh,
    /// One folder for every commit, named by `CARGO_TARGET_DIR`, as a gate has.
    Shared,
    /// Cargo's own, `target` in the folder it builds in, with no `CARGO_TARGET_DIR`: a worktree.
    Inside,
}

impl Targets {
    fn as_str(self) -> &'static str {
        match self {
            Targets::Fresh => "fresh",
            Targets::Shared => "shared",
            Targets::Inside => "inside",
        }
    }
}

/// Where each commit is exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Paths {
    Rotate,
    Fixed,
}

impl Paths {
    fn as_str(self) -> &'static str {
        match self {
            Paths::Rotate => "rotate",
            Paths::Fixed => "fixed",
        }
    }
}

/// The command line, read but not yet checked against the file system.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    repo: PathBuf,
    dir: Option<PathBuf>,
    selection: Selection,
    command: String,
    hushrun: Option<PathBuf>,
    wrap: Wrap,
    work: PathBuf,
    targets: Targets,
    paths: Paths,
    keep: bool,
}

#[derive(Debug)]
enum Parsed {
    Help,
    Run(Options),
}

pub fn run(args: &[String]) -> Result<bool> {
    let options = match parse(args)? {
        Parsed::Help => {
            print!("{USAGE}");
            return Ok(true);
        }
        Parsed::Run(options) => options,
    };
    let env = cleaned_env(std::env::vars_os());
    let plan = Plan::new(options, &env)?;
    let o = &plan.options;
    let shas = select(&o.repo, o.dir.as_deref(), &o.selection)?;
    plan.prepare()?;
    let header = plan.header(&env)?;
    println!(
        "replaying {} commit(s) of {} ({}) in mode {} into {}",
        shas.len(),
        o.repo.display(),
        header.dir,
        o.wrap.as_str(),
        o.work.display()
    );

    let mut runs = Vec::with_capacity(shas.len());
    for (i, sha) in shas.iter().enumerate() {
        let run = plan.replay(&env, i + 1, sha)?;
        println!("[{}/{}] {}", i + 1, shas.len(), run.line());
        runs.push(run);
        write_reports(&o.work, &header, &runs)?;
    }
    plan.finish()?;

    let total = Totals::of(&runs);
    println!("\nreport: {}", o.work.join("replay.md").display());
    println!("{}", total.verdict());
    Ok(total.counts.differed.is_empty())
}

/// Reads the arguments. Refuses what can be refused without looking at the disk.
fn parse(args: &[String]) -> Result<Parsed> {
    let mut repo = None;
    let mut dir = None;
    let mut last = None;
    let mut range = None;
    let mut list = Vec::new();
    let mut command = None;
    let mut hushrun = None;
    let mut wrap = Wrap::Observe;
    let mut work = None;
    let mut targets = Targets::Fresh;
    let mut paths = Paths::Rotate;
    let mut keep = false;

    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--repo" => repo = Some(PathBuf::from(value()?)),
            "--dir" => dir = Some(value()?),
            "--last" => {
                let text = value()?;
                let n: usize = text
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .with_context(|| format!("--last takes a number above 0, not {text:?}"))?;
                last = Some(n);
            }
            "--commits" => range = Some(revision("--commits", value()?)?),
            "--commit" => list.push(revision("--commit", value()?)?),
            "--command" => command = Some(value()?),
            "--hushrun" => hushrun = Some(PathBuf::from(value()?)),
            "--mode" => {
                wrap = match value()?.as_str() {
                    "observe" => Wrap::Observe,
                    "on" => Wrap::On,
                    "none" => Wrap::None,
                    other => bail!("--mode is observe, on or none, not {other:?}"),
                }
            }
            "--work" => work = Some(PathBuf::from(value()?)),
            "--target" => {
                targets = match value()?.as_str() {
                    "fresh" => Targets::Fresh,
                    "shared" => Targets::Shared,
                    "inside" => Targets::Inside,
                    other => bail!("--target is fresh, shared or inside, not {other:?}"),
                }
            }
            "--paths" => {
                paths = match value()?.as_str() {
                    "rotate" => Paths::Rotate,
                    "fixed" => Paths::Fixed,
                    other => bail!("--paths is rotate or fixed, not {other:?}"),
                }
            }
            "--keep" => keep = true,
            other => bail!("unexpected argument {other:?}; see `cargo xtask build-replay --help`"),
        }
    }

    let chosen =
        usize::from(last.is_some()) + usize::from(range.is_some()) + usize::from(!list.is_empty());
    if chosen > 1 {
        bail!("choose one of --last, --commits and --commit");
    }
    let selection = match (last, range) {
        (Some(n), _) => Selection::Last(n),
        (None, Some(range)) => {
            if !range.contains("..") {
                bail!("--commits takes a range A..B, not {range:?}");
            }
            Selection::Range(range)
        }
        (None, None) if !list.is_empty() => Selection::List(list),
        (None, None) => Selection::Last(DEFAULT_LAST),
    };
    let repo = repo.context("--repo PATH is required: the git repository to replay")?;
    let work = work.context(
        "--work PATH is required: the scratch folder for exports, targets, the store and the report",
    )?;
    if wrap != Wrap::None && hushrun.is_none() {
        bail!(
            "--mode {} needs --hushrun PATH: the built hushrun binary whose wrapper cargo calls",
            wrap.as_str()
        );
    }
    let command = command.unwrap_or_else(|| DEFAULT_COMMAND.to_string());
    if command.trim().is_empty() {
        bail!("--command is empty");
    }
    let dir = match dir {
        None => None,
        Some(dir) => folder_in_repo(&dir)?,
    };
    Ok(Parsed::Run(Options {
        repo,
        dir,
        selection,
        command,
        hushrun,
        wrap,
        work,
        targets,
        paths,
        keep,
    }))
}

/// A revision as given, refused when git would read it as an option.
fn revision(flag: &str, text: String) -> Result<String> {
    if text.is_empty() || text.starts_with('-') {
        bail!("{flag} takes a revision, not {text:?}");
    }
    Ok(text)
}

/// `--dir`: a relative folder that stays inside the repository. `.` and empty are the root.
fn folder_in_repo(text: &str) -> Result<Option<PathBuf>> {
    let path = Path::new(text);
    let mut clean = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => clean.push(name),
            Component::CurDir => {}
            _ => bail!("--dir takes a folder inside the repository, like back, not {text:?}"),
        }
    }
    Ok((!clean.as_os_str().is_empty()).then_some(clean))
}

/// The process environment without what would change, or leak into, the replayed builds.
fn cleaned_env(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Env {
    vars.into_iter()
        .filter(|(name, _)| !removed(name))
        .collect()
}

fn removed(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    REMOVED.contains(&name.as_ref())
        || REMOVED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// An absolute, resolved form of `path`, which need not exist yet: its nearest existing folder
/// resolved, and the rest added. A `..` in the part that doesn't exist is refused.
fn resolve(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut out = real;
            out.extend(rest.iter().rev());
            return Ok(out);
        }
        let name = existing
            .file_name()
            .with_context(|| format!("can't resolve {}", path.display()))?;
        rest.push(name.to_os_string());
        existing = existing
            .parent()
            .with_context(|| format!("can't resolve {}", path.display()))?;
    }
}

/// hushrun's own folders on this machine: the tool must never write there.
fn real_hushrun_folders(env: &Env) -> Vec<PathBuf> {
    let get = |name: &str| {
        env.get(OsStr::new(name))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let mut folders = Vec::new();
    if let Some(home) = get("HOME") {
        for rest in [
            "Library/Caches/hushrun",
            "Library/Application Support/hushrun",
            ".cache/hushrun",
            ".config/hushrun",
            ".local/share/hushrun",
        ] {
            folders.push(home.join(rest));
        }
    }
    for variable in ["XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
        if let Some(base) = get(variable) {
            folders.push(base.join("hushrun"));
        }
    }
    folders
        .into_iter()
        .map(|folder| resolve(&folder).unwrap_or(folder))
        .collect()
}

/// The checked options and the folders a replay uses, all under `--work`.
#[derive(Debug)]
struct Plan {
    options: Options,
    /// The repository's folder name: blocks record it (B-13).
    repository: String,
    /// The per-user settings file of this replay (B-21).
    settings: PathBuf,
    /// The cache directory the store lives in (B-13).
    cache: PathBuf,
    /// Each commit's output, times and raw entries.
    logs: PathBuf,
}

impl Plan {
    /// Checks the options against the disk and resolves every path. Writes nothing.
    fn new(mut options: Options, env: &Env) -> Result<Plan> {
        let repo = options
            .repo
            .canonicalize()
            .with_context(|| format!("--repo {}", options.repo.display()))?;
        ensure!(repo.is_dir(), "--repo {} is not a folder", repo.display());
        let work = resolve(&options.work)?;
        if work.starts_with(&repo) {
            bail!(
                "--work {} is inside --repo {}: the replay never writes in the repository",
                work.display(),
                repo.display()
            );
        }
        if repo.starts_with(&work) {
            bail!(
                "--repo {} is inside --work {}: --work is deleted from",
                repo.display(),
                work.display()
            );
        }
        if let Some(real) = real_hushrun_folders(env)
            .into_iter()
            .find(|real| work.starts_with(real) || real.starts_with(&work))
        {
            bail!(
                "--work {} overlaps hushrun's own folder {}: the replay never touches the real store or settings",
                work.display(),
                real.display()
            );
        }
        if let Some(hushrun) = &options.hushrun {
            let binary = hushrun
                .canonicalize()
                .with_context(|| format!("--hushrun {}", hushrun.display()))?;
            ensure!(
                binary.is_file(),
                "--hushrun {} is not a file",
                binary.display()
            );
            options.hushrun = Some(binary);
        }
        let repository = repo
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        options.repo = repo;
        options.work = work.clone();
        Ok(Plan {
            repository,
            settings: work.join("build.json"),
            cache: work.join("cache"),
            logs: work.join("logs"),
            options,
        })
    }

    /// Makes `--work`, writes the replay's settings with its mode, and clears the last
    /// replay's report and logs. The store is kept: the report says what it held at the start.
    fn prepare(&self) -> Result<()> {
        let work = &self.options.work;
        std::fs::create_dir_all(work).with_context(|| format!("creating {}", work.display()))?;
        for name in ["replay.jsonl", "replay.md"] {
            let path = work.join(name);
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        }
        // Each replay starts with no exports and, for `--target shared`, an empty target.
        for leftover in [&self.logs, &work.join("exports"), &work.join("target")] {
            remove_under(work, leftover)?;
        }
        std::fs::create_dir_all(&self.logs)?;
        if let Some(mode) = self.options.wrap.mode() {
            settings::update(&self.settings, |s| s.mode = mode)
                .with_context(|| format!("writing {}", self.settings.display()))?;
        }
        Ok(())
    }

    /// The store's folder, as a session in this replay finds it (B-13).
    fn store(&self, env: &Env) -> Result<PathBuf> {
        let lookup = |name: &str| env.get(OsStr::new(name)).cloned();
        let overrides = Overrides::from(&lookup)?;
        let store = session::store_dir(Some(&self.cache), &overrides)
            .context("no store folder for this replay")?;
        ensure!(
            store.starts_with(&self.options.work),
            "the store {} would be outside --work",
            store.display()
        );
        Ok(store)
    }

    fn header(&self, env: &Env) -> Result<Header> {
        let o = &self.options;
        let store_at_start = match o.wrap {
            Wrap::None => None,
            Wrap::Observe | Wrap::On => Some(StoreSize::of(&self.store(env)?)),
        };
        Ok(Header {
            repo: o.repo.display().to_string(),
            dir: o
                .dir
                .as_ref()
                .map_or_else(|| ".".to_string(), |dir| dir.display().to_string()),
            command: o.command.clone(),
            mode: o.wrap.as_str(),
            targets: o.targets.as_str(),
            paths: o.paths.as_str(),
            hushrun: o.hushrun.as_ref().map(|path| path.display().to_string()),
            store_at_start,
        })
    }

    /// Exports, builds and counts one commit. A failure of the commit itself (its export or
    /// its command) is recorded in what comes back; an error is a failure of the replay.
    fn replay(&self, env: &Env, index: usize, sha: &str) -> Result<CommitRun> {
        let o = &self.options;
        let root = export_root(&o.work, index, o.paths);
        let export = root.join("src");
        let build_dir = match &o.dir {
            Some(dir) => export.join(dir),
            None => export.clone(),
        };
        let target = match o.targets {
            Targets::Fresh => root.join("target"),
            Targets::Shared => o.work.join("target"),
            Targets::Inside => build_dir.join("target"),
        };
        let stem = format!("c{index:03}-{}", &sha[..sha.len().min(12)]);
        let mut run = CommitRun {
            index,
            sha: sha.to_string(),
            export: build_dir.clone(),
            target: target.clone(),
            log: self.logs.join(format!("{stem}.log")),
            error: None,
            exit_code: None,
            wall_ms: 0,
            user_ms: None,
            sys_ms: None,
            counts: Counts::default(),
            store: None,
        };

        // A fresh target starts empty. A rotated export starts empty, with any target inside it;
        // a fixed one is updated in place, as a checkout is, and keeps a target inside it.
        if o.targets == Targets::Fresh {
            remove_under(&o.work, &target)?;
        }
        let exported = match o.paths {
            Paths::Rotate => {
                remove_under(&o.work, &root)?;
                export_commit(&o.repo, sha, o.dir.as_deref(), &export)
            }
            Paths::Fixed => {
                let staging = o.work.join("exports").join("staging");
                remove_under(&o.work, &staging)?;
                let keep = (o.targets == Targets::Inside).then_some(target.as_path());
                let result = export_commit(&o.repo, sha, o.dir.as_deref(), &staging)
                    .and_then(|()| sync_tree(&staging, &export, keep));
                remove_under(&o.work, &staging)?;
                result
            }
        };
        let exported = exported.and_then(|()| {
            ensure!(
                build_dir.is_dir(),
                "{} is not in the commit",
                build_dir.display()
            );
            Ok(())
        });
        if let Err(err) = exported {
            run.error = Some(format!("export: {err:#}"));
            self.clean_up(&root, &target)?;
            return Ok(run);
        }

        let session = match o.wrap.mode() {
            None => None,
            Some(_) => {
                let hushrun = o.hushrun.as_deref().context("no --hushrun")?;
                let lookup = |name: &str| env.get(OsStr::new(name)).cloned();
                let start = Start {
                    settings: &self.settings,
                    cache_dir: Some(&self.cache),
                    exe: hushrun,
                    in_ci: false,
                    repository: Some(&self.repository),
                    repository_env: &[],
                    env: &lookup,
                    now: SystemTime::now(),
                };
                let session = session::begin(&start).map_err(|why| {
                    anyhow::anyhow!("the wrapper's session didn't start: {why:?}")
                })?;
                ensure!(
                    session.current().store.starts_with(&o.work),
                    "the session's store {} is outside --work",
                    session.current().store.display()
                );
                Some(session)
            }
        };
        let mut child_env = env.clone();
        if let Some(session) = &session {
            child_env.extend(session.env());
        }
        if o.targets != Targets::Inside {
            child_env.insert("CARGO_TARGET_DIR".into(), target.clone().into_os_string());
        }

        let ran = run_command(
            &o.command,
            &build_dir,
            &child_env,
            &run.log,
            &self.logs.join(format!("{stem}.time")),
        );
        // The log is read before `finish`, which removes the session's folder.
        let entries = session
            .as_ref()
            .map(|session| session.current().entries())
            .unwrap_or_default();
        if let Some(session) = session {
            let store = session.current().store.clone();
            session.finish(SystemTime::now());
            run.store = Some(StoreSize::of(&store));
        }
        match ran {
            Ok(ran) => {
                run.exit_code = ran.exit_code;
                run.wall_ms = ran.wall_ms;
                run.user_ms = ran.user_ms;
                run.sys_ms = ran.sys_ms;
            }
            Err(err) => run.error = Some(format!("command: {err:#}")),
        }
        let mut raw = String::new();
        for entry in &entries {
            raw.push_str(&serde_json::to_string(entry)?);
            raw.push('\n');
        }
        write_whole(
            &self.logs.join(format!("{stem}.entries.jsonl")),
            raw.as_bytes(),
        )?;
        run.counts = Counts::of(&entries);
        self.clean_up(&root, &target)?;
        Ok(run)
    }

    /// After a commit, unless `--keep`: a rotated export goes with its fresh target or the
    /// target inside it; a fixed export stays for the next commit to update ([`Plan::finish`]
    /// removes it, with a target inside it) and only its fresh target goes.
    fn clean_up(&self, root: &Path, target: &Path) -> Result<()> {
        let o = &self.options;
        if o.keep {
            return Ok(());
        }
        match o.paths {
            Paths::Rotate => remove_under(&o.work, root),
            Paths::Fixed if o.targets == Targets::Fresh => remove_under(&o.work, target),
            Paths::Fixed => Ok(()),
        }
    }

    /// After the last commit, unless `--keep`: the fixed export goes too.
    fn finish(&self) -> Result<()> {
        let o = &self.options;
        if o.keep || o.paths == Paths::Rotate {
            return Ok(());
        }
        remove_under(&o.work, &export_root(&o.work, 0, Paths::Fixed))
    }
}

/// The folder commit `index` (from 1) is exported into, with its fresh target beside the
/// export. Rotated, each commit gets a new path whose length differs from the previous one's,
/// so a block tied to a folder can't pass for one that isn't.
fn export_root(work: &Path, index: usize, paths: Paths) -> PathBuf {
    let exports = work.join("exports");
    match paths {
        Paths::Fixed => exports.join("fixed"),
        Paths::Rotate => exports.join(format!("c{index:03}-{}", "p".repeat(1 + (index % 8) * 5))),
    }
}

/// Removes `path`, which must lie strictly under `work`, if it exists.
fn remove_under(work: &Path, path: &Path) -> Result<()> {
    ensure!(
        path.starts_with(work) && path != work,
        "refusing to remove {}: not under {}",
        path.display(),
        work.display()
    );
    if path.symlink_metadata().is_ok() {
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

/// Makes `dest` hold exactly what `src` holds, as a checkout does: an entry whose bytes and
/// permissions (or link target) are unchanged keeps its date; every other one is moved in from
/// `src`, with the date it was extracted with, and what `src` doesn't have is removed, but
/// `keep`, a folder git doesn't track (a target inside the checkout).
fn sync_tree(src: &Path, dest: &Path, keep: Option<&Path>) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let existing = to.symlink_metadata().ok();
        names.insert(entry.file_name());
        if entry.file_type()?.is_dir() {
            if existing.as_ref().is_some_and(|meta| !meta.is_dir()) {
                std::fs::remove_file(&to)?;
            }
            sync_tree(&from, &to, keep)?;
            continue;
        }
        if same_entry(&from, &to)? {
            continue;
        }
        if existing.as_ref().is_some_and(|meta| meta.is_dir()) {
            std::fs::remove_dir_all(&to)?;
        }
        std::fs::rename(&from, &to).with_context(|| format!("moving in {}", to.display()))?;
    }
    for entry in std::fs::read_dir(dest)? {
        let entry = entry?;
        if names.contains(&entry.file_name()) || keep == Some(entry.path().as_path()) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Whether two entries hold the same: both links to one target, or both files with the same
/// permissions and bytes.
fn same_entry(a: &Path, b: &Path) -> Result<bool> {
    let (Ok(meta_a), Ok(meta_b)) = (a.symlink_metadata(), b.symlink_metadata()) else {
        return Ok(false);
    };
    let (link_a, link_b) = (
        meta_a.file_type().is_symlink(),
        meta_b.file_type().is_symlink(),
    );
    if link_a || link_b {
        return Ok(link_a && link_b && std::fs::read_link(a)? == std::fs::read_link(b)?);
    }
    if !meta_b.is_file()
        || meta_a.len() != meta_b.len()
        || meta_a.permissions() != meta_b.permissions()
    {
        return Ok(false);
    }
    Ok(std::fs::read(a)? == std::fs::read(b)?)
}

/// Writes a file whole: a temporary file beside it, then a rename.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .context("a file name")?
        .to_string_lossy()
        .into_owned();
    let temp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&temp, bytes).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// `git -C repo`, never pointed at another repository by the environment.
fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo);
    for name in GIT_LOCATION {
        command.env_remove(name);
    }
    command
}

/// The lines git prints, or an error with what it said.
fn git_lines(repo: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = git(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("running git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// The commits to replay, as full SHAs, oldest first (or in the order given).
fn select(repo: &Path, dir: Option<&Path>, selection: &Selection) -> Result<Vec<String>> {
    let dir_text = dir.map(|dir| dir.to_string_lossy().into_owned());
    let pathspec: Vec<&str> = match &dir_text {
        Some(dir) => vec!["--", dir.as_str()],
        None => vec!["--"],
    };
    let shas = match selection {
        Selection::Last(n) => {
            let max = format!("--max-count={n}");
            let mut args = vec!["rev-list", max.as_str(), "HEAD"];
            args.extend(&pathspec);
            let mut shas = git_lines(repo, &args)?;
            shas.reverse();
            shas
        }
        Selection::Range(range) => {
            let mut args = vec!["rev-list", "--reverse", range.as_str()];
            args.extend(&pathspec);
            git_lines(repo, &args)?
        }
        Selection::List(list) => {
            let mut shas = Vec::with_capacity(list.len());
            for given in list {
                let spec = format!("{given}^{{commit}}");
                let found = git_lines(repo, &["rev-parse", "--verify", "--quiet", &spec])
                    .with_context(|| format!("--commit {given} is not a commit"))?;
                shas.push(
                    found
                        .into_iter()
                        .next()
                        .context("git rev-parse printed nothing")?,
                );
            }
            shas
        }
    };
    ensure!(
        !shas.is_empty(),
        "no commits selected in {}",
        repo.display()
    );
    Ok(shas)
}

/// `git archive SHA [DIR] | tar -x -m` into `into`.
fn export_commit(repo: &Path, sha: &str, dir: Option<&Path>, into: &Path) -> Result<()> {
    std::fs::create_dir_all(into).with_context(|| format!("creating {}", into.display()))?;
    let mut archive = git(repo);
    archive.args(["archive", "--format=tar", sha]);
    if let Some(dir) = dir {
        archive.arg("--").arg(dir);
    }
    let mut archive = archive
        .env("COPYFILE_DISABLE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running git archive")?;
    let tar_input = archive.stdout.take().context("git archive's output")?;
    let tar = Command::new("tar")
        // Every file is dated now, as a checkout dates what it writes: a date from the commit
        // would be older than an earlier build's outputs, and cargo would call those fresh.
        .args(["-x", "-m", "-f", "-", "-C"])
        .arg(into)
        .env("COPYFILE_DISABLE", "1")
        .stdin(Stdio::from(tar_input))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("running tar")?;
    let archived = archive
        .wait_with_output()
        .context("waiting for git archive")?;
    if !archived.status.success() {
        bail!(
            "git archive {sha} failed: {}",
            String::from_utf8_lossy(&archived.stderr).trim()
        );
    }
    if !tar.status.success() {
        bail!(
            "tar -x failed: {}",
            String::from_utf8_lossy(&tar.stderr).trim()
        );
    }
    Ok(())
}

/// What one command's run took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ran {
    /// `None` when a signal ended it.
    exit_code: Option<i32>,
    wall_ms: u64,
    /// The child tree's CPU, when `/usr/bin/time` could measure it.
    user_ms: Option<u64>,
    sys_ms: Option<u64>,
}

/// Runs `command` with `sh -c` in `cwd`, with exactly `env`, its output into `log`. Under
/// `/usr/bin/time -p` when there is one, which counts the CPU of every process the command
/// waited for (cargo waits for each rustc).
fn run_command(command: &str, cwd: &Path, env: &Env, log: &Path, times: &Path) -> Result<Ran> {
    let out = File::create(log).with_context(|| format!("creating {}", log.display()))?;
    let err = out.try_clone()?;
    let timer = Path::new("/usr/bin/time");
    let timed = timer.is_file();
    if times.exists() {
        std::fs::remove_file(times)?;
    }
    let mut child = if timed {
        let mut child = Command::new(timer);
        child.arg("-p").arg("-o").arg(times).arg("sh");
        child
    } else {
        Command::new("sh")
    };
    child
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let started = Instant::now();
    let status = child
        .status()
        .with_context(|| format!("running {command:?}"))?;
    let wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (user_ms, sys_ms) = if timed {
        std::fs::read_to_string(times)
            .map(|text| parse_times(&text))
            .unwrap_or((None, None))
    } else {
        (None, None)
    };
    Ok(Ran {
        exit_code: status.code(),
        wall_ms,
        user_ms,
        sys_ms,
    })
}

/// User and system CPU from `time -p`'s output (`user 1.23`, `sys 0.45`), in milliseconds.
fn parse_times(text: &str) -> (Option<u64>, Option<u64>) {
    let mut user = None;
    let mut sys = None;
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(label), Some(value), None) = (words.next(), words.next(), words.next()) else {
            continue;
        };
        let Some(ms) = seconds_to_ms(value) else {
            continue;
        };
        match label {
            "user" => user = Some(ms),
            "sys" => sys = Some(ms),
            _ => {}
        }
    }
    (user, sys)
}

/// `12.34` seconds as 12340 milliseconds, exactly: the digits are read, not a float.
fn seconds_to_ms(text: &str) -> Option<u64> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut ms: u64 = whole.parse::<u64>().ok()?.checked_mul(1000)?;
    let digits: Vec<u64> = fraction.bytes().map(|b| u64::from(b - b'0')).collect();
    for (i, scale) in [100u64, 10, 1].into_iter().enumerate() {
        ms += digits.get(i).copied().unwrap_or(0) * scale;
    }
    // Round half up on the fourth digit.
    if digits.get(3).is_some_and(|d| *d >= 5) {
        ms += 1;
    }
    Some(ms)
}

/// The store's size at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct StoreSize {
    blocks: u64,
    bytes: u64,
}

impl StoreSize {
    fn of(store: &Path) -> StoreSize {
        let status = Store::new(store).status(SystemTime::now());
        StoreSize {
            blocks: status.blocks,
            bytes: status.bytes,
        }
    }
}

/// What a run's compiles came to: the wrapper's log, counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Counts {
    compiles: u64,
    by_outcome: BTreeMap<&'static str, u64>,
    /// By outcome and reason, for the two outcomes that carry one.
    by_reason: BTreeMap<(&'static str, &'static str), u64>,
    by_class: BTreeMap<&'static str, u64>,
    /// Compiles that found a block before they ran.
    found: u64,
    /// The crate of every compile whose outputs differed from its block, in log order.
    differed: Vec<String>,
    rustc_ms: u64,
    saved_ms: u64,
    stored_bytes: u64,
    /// Up to three distinct named crates per outcome and reason, first seen first.
    examples: BTreeMap<(&'static str, &'static str), Vec<String>>,
}

/// Whether a compile's outputs differed from its block: `observe`'s comparison (B-1) or an
/// audit's (B-16).
fn differs(entry: &Entry) -> bool {
    entry.differed || entry.outcome == Outcome::BuiltNotStored(NotStored::Differed)
}

/// A crate as the report names it: `crate (package)`, the crate alone when the log doesn't name
/// the package, empty when it names no crate. Every build script is `build_script_build`.
fn named(entry: &Entry) -> String {
    match (entry.crate_name.is_empty(), entry.package.is_empty()) {
        (true, _) => String::new(),
        (false, true) => entry.crate_name.clone(),
        (false, false) => format!("{} ({})", entry.crate_name, entry.package),
    }
}

fn class_name(class: Option<Class>) -> &'static str {
    class.map_or(UNKNOWN_CLASS, Class::as_str)
}

impl Counts {
    fn of(entries: &[Entry]) -> Counts {
        let mut counts = Counts::default();
        for entry in entries {
            counts.compiles += 1;
            *counts.by_outcome.entry(entry.outcome.as_str()).or_default() += 1;
            *counts.by_class.entry(class_name(entry.class)).or_default() += 1;
            if let Some(reason) = entry.outcome.reason() {
                let key = (entry.outcome.as_str(), reason);
                *counts.by_reason.entry(key).or_default() += 1;
                add_example(counts.examples.entry(key).or_default(), &named(entry));
            }
            if entry.found {
                counts.found += 1;
            }
            if differs(entry) {
                counts.differed.push(if entry.crate_name.is_empty() {
                    UNNAMED.to_string()
                } else {
                    named(entry)
                });
            }
            counts.rustc_ms = counts.rustc_ms.saturating_add(entry.rustc_ms);
            counts.saved_ms = counts.saved_ms.saturating_add(entry.saved_ms);
            counts.stored_bytes = counts.stored_bytes.saturating_add(entry.stored_bytes);
        }
        counts
    }

    fn add(&mut self, other: &Counts) {
        self.compiles += other.compiles;
        for (key, n) in &other.by_outcome {
            *self.by_outcome.entry(*key).or_default() += n;
        }
        for (key, n) in &other.by_reason {
            *self.by_reason.entry(*key).or_default() += n;
        }
        for (key, n) in &other.by_class {
            *self.by_class.entry(*key).or_default() += n;
        }
        self.found += other.found;
        self.differed.extend(other.differed.iter().cloned());
        self.rustc_ms = self.rustc_ms.saturating_add(other.rustc_ms);
        self.saved_ms = self.saved_ms.saturating_add(other.saved_ms);
        self.stored_bytes = self.stored_bytes.saturating_add(other.stored_bytes);
        for (key, names) in &other.examples {
            let mine = self.examples.entry(*key).or_default();
            for name in names {
                add_example(mine, name);
            }
        }
    }

    fn outcome(&self, kind: &str) -> u64 {
        self.by_outcome.get(kind).copied().unwrap_or(0)
    }

    /// The reasons of one outcome with their counts, the most frequent first.
    fn reasons(&self, outcome: &str) -> Vec<(&'static str, u64)> {
        let mut reasons: Vec<(&'static str, u64)> = self
            .by_reason
            .iter()
            .filter(|((kind, _), _)| *kind == outcome)
            .map(|((_, reason), n)| (*reason, *n))
            .collect();
        reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        reasons
    }
}

fn add_example(examples: &mut Vec<String>, name: &str) {
    if !name.is_empty() && examples.len() < 3 && !examples.iter().any(|seen| seen == name) {
        examples.push(name.to_string());
    }
}

/// One commit's replay.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommitRun {
    /// From 1, in replay order.
    index: usize,
    sha: String,
    /// The folder the command ran in.
    export: PathBuf,
    target: PathBuf,
    /// The command's output.
    log: PathBuf,
    /// Why the commit couldn't be exported or its command couldn't start.
    error: Option<String>,
    /// The command's exit code; `None` when it didn't run or a signal ended it.
    exit_code: Option<i32>,
    wall_ms: u64,
    user_ms: Option<u64>,
    sys_ms: Option<u64>,
    counts: Counts,
    /// The store after this commit; `None` without a wrapper.
    store: Option<StoreSize>,
}

impl CommitRun {
    fn failed(&self) -> bool {
        self.error.is_some() || self.exit_code != Some(0)
    }

    fn cpu_ms(&self) -> Option<u64> {
        Some(self.user_ms?.saturating_add(self.sys_ms?))
    }

    fn short(&self) -> &str {
        &self.sha[..self.sha.len().min(10)]
    }

    fn exit_text(&self) -> String {
        match (&self.error, self.exit_code) {
            (Some(_), _) => "error".to_string(),
            (None, Some(code)) => code.to_string(),
            (None, None) => "signal".to_string(),
        }
    }

    /// The progress line printed after it.
    fn line(&self) -> String {
        let c = &self.counts;
        let mut line = format!(
            "{} exit {} wall {}s compiles {} reused {} stored {} found {} differed {}",
            self.short(),
            self.exit_text(),
            seconds(self.wall_ms),
            c.compiles,
            c.outcome("reused"),
            c.outcome("built-and-stored"),
            c.found,
            c.differed.len()
        );
        if let Some(error) = &self.error {
            line.push_str(&format!(" ({error})"));
        } else if self.failed() {
            line.push_str(&format!(" (see {})", self.log.display()));
        }
        line
    }

    fn json(&self) -> Value {
        let c = &self.counts;
        let by_outcome: BTreeMap<&str, u64> = OUTCOMES
            .iter()
            .map(|kind| (*kind, c.outcome(kind)))
            .collect();
        let mut by_reason: BTreeMap<&str, BTreeMap<&str, u64>> = BTreeMap::new();
        for ((kind, reason), n) in &c.by_reason {
            by_reason.entry(*kind).or_default().insert(*reason, *n);
        }
        json!({
            "index": self.index,
            "sha": self.sha,
            "export": self.export.display().to_string(),
            "target": self.target.display().to_string(),
            "log": self.log.display().to_string(),
            "error": self.error,
            "exit_code": self.exit_code,
            "wall_ms": self.wall_ms,
            "user_ms": self.user_ms,
            "sys_ms": self.sys_ms,
            "cpu_ms": self.cpu_ms(),
            "compiles": c.compiles,
            "by_outcome": by_outcome,
            "by_reason": by_reason,
            "by_class": c.by_class,
            "found": c.found,
            "differed": c.differed.len(),
            "differed_crates": c.differed,
            "rustc_ms": c.rustc_ms,
            "saved_ms": c.saved_ms,
            "stored_bytes": c.stored_bytes,
            "store_blocks": self.store.map(|s| s.blocks),
            "store_bytes": self.store.map(|s| s.bytes),
        })
    }
}

/// What the replay was asked to do, for the report's head.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Header {
    repo: String,
    dir: String,
    command: String,
    mode: &'static str,
    targets: &'static str,
    paths: &'static str,
    hushrun: Option<String>,
    store_at_start: Option<StoreSize>,
}

/// Every commit's counts together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Totals {
    counts: Counts,
    commits: usize,
    failed: usize,
    wall_ms: u64,
    /// Summed over the commits that measured it; `None` when one didn't.
    cpu_ms: Option<u64>,
}

impl Totals {
    fn of(runs: &[CommitRun]) -> Totals {
        let mut totals = Totals {
            cpu_ms: Some(0),
            ..Totals::default()
        };
        for run in runs {
            totals.counts.add(&run.counts);
            totals.commits += 1;
            totals.failed += usize::from(run.failed());
            totals.wall_ms = totals.wall_ms.saturating_add(run.wall_ms);
            totals.cpu_ms = match (totals.cpu_ms, run.cpu_ms()) {
                (Some(sum), Some(cpu)) => Some(sum.saturating_add(cpu)),
                _ => None,
            };
        }
        totals
    }

    fn verdict(&self) -> String {
        let differed = self.counts.differed.len();
        let mut text = format!(
            "differences: {differed} in {} compiles over {} commits ({})",
            self.counts.compiles,
            self.commits,
            if differed == 0 {
                "the bar is zero: met"
            } else {
                "the bar is zero: NOT met"
            }
        );
        if self.failed > 0 {
            text.push_str(&format!(
                "; {} commit(s) failed, their numbers are partial",
                self.failed
            ));
        }
        text
    }
}

/// Milliseconds as seconds, exactly: `12.345`.
fn seconds(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

fn optional_seconds(ms: Option<u64>) -> String {
    ms.map_or_else(|| "–".to_string(), seconds)
}

/// The JSON lines, one object per commit.
fn report_jsonl(runs: &[CommitRun]) -> String {
    let mut out = String::new();
    for run in runs {
        out.push_str(&run.json().to_string());
        out.push('\n');
    }
    out
}

/// The Markdown report.
fn report_md(header: &Header, runs: &[CommitRun]) -> String {
    let totals = Totals::of(runs);
    let t = &totals.counts;
    let mut md = String::new();
    md.push_str("# Build replay\n\n");
    md.push_str(&format!(
        "- repository: `{}`, folder `{}`\n",
        header.repo, header.dir
    ));
    md.push_str(&format!("- command: `{}`\n", header.command));
    md.push_str(&format!(
        "- mode: {}; targets: {}; paths: {}\n",
        header.mode, header.targets, header.paths
    ));
    if let Some(hushrun) = &header.hushrun {
        md.push_str(&format!("- hushrun: `{hushrun}`\n"));
    }
    if let Some(store) = header.store_at_start {
        md.push_str(&format!(
            "- store at the start: {} blocks, {} bytes\n",
            store.blocks, store.bytes
        ));
    }
    md.push_str(&format!("\n**{}.**\n\n", capitalize(&totals.verdict())));

    md.push_str("## Commits\n\n");
    md.push_str(
        "| # | commit | exit | wall s | cpu s | compiles | reused | built, stored | built, not stored | passed through | found a block | differed | rustc s | saved s | bytes stored | store bytes after |\n",
    );
    md.push_str(
        "|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for run in runs {
        let c = &run.counts;
        md.push_str(&format!(
            "| {} | `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            run.index,
            run.short(),
            run.exit_text(),
            seconds(run.wall_ms),
            optional_seconds(run.cpu_ms()),
            c.compiles,
            c.outcome("reused"),
            c.outcome("built-and-stored"),
            c.outcome("built-not-stored"),
            c.outcome("passed-through"),
            c.found,
            c.differed.len(),
            seconds(c.rustc_ms),
            seconds(c.saved_ms),
            c.stored_bytes,
            run.store
                .map_or_else(|| "–".to_string(), |s| s.bytes.to_string()),
        ));
    }
    let last_store = runs
        .iter()
        .rev()
        .find_map(|run| run.store)
        .map_or_else(|| "–".to_string(), |s| s.bytes.to_string());
    md.push_str(&format!(
        "| **total** | {} commits | {} failed | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n\n",
        totals.commits,
        totals.failed,
        seconds(totals.wall_ms),
        optional_seconds(totals.cpu_ms),
        t.compiles,
        t.outcome("reused"),
        t.outcome("built-and-stored"),
        t.outcome("built-not-stored"),
        t.outcome("passed-through"),
        t.found,
        t.differed.len(),
        seconds(t.rustc_ms),
        seconds(t.saved_ms),
        t.stored_bytes,
        last_store,
    ));

    let failed: Vec<&CommitRun> = runs.iter().filter(|run| run.failed()).collect();
    if !failed.is_empty() {
        md.push_str("## Commits that failed\n\n");
        for run in failed {
            let why = run
                .error
                .clone()
                .unwrap_or_else(|| format!("exit {}", run.exit_text()));
            md.push_str(&format!(
                "- #{} `{}`: {why}; output in `{}`\n",
                run.index,
                run.short(),
                run.log.display()
            ));
        }
        md.push('\n');
    }

    md.push_str("## By class\n\n| class | compiles |\n|---|---:|\n");
    for (class, n) in &t.by_class {
        md.push_str(&format!("| {class} | {n} |\n"));
    }
    md.push('\n');

    md.push_str("## Crates that differed\n\n");
    let mut any = false;
    for run in runs {
        for name in &run.counts.differed {
            any = true;
            md.push_str(&format!("- `{name}` at #{} `{}`\n", run.index, run.short()));
        }
    }
    if !any {
        md.push_str("None.\n");
    }
    md.push('\n');

    for (outcome, title) in [
        ("built-not-stored", "Built, not stored"),
        ("passed-through", "Passed through"),
    ] {
        md.push_str(&format!("## {title}\n\n"));
        let reasons = t.reasons(outcome);
        if reasons.is_empty() {
            md.push_str("None.\n\n");
            continue;
        }
        md.push_str("| reason | compiles | examples |\n|---|---:|---|\n");
        for (reason, n) in reasons {
            let examples = t
                .examples
                .get(&(outcome, reason))
                .filter(|names| !names.is_empty())
                .map_or_else(
                    || "–".to_string(),
                    |names| {
                        names
                            .iter()
                            .map(|name| format!("`{name}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                );
            md.push_str(&format!("| {reason} | {n} | {examples} |\n"));
        }
        md.push('\n');
    }
    md
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn write_reports(work: &Path, header: &Header, runs: &[CommitRun]) -> Result<()> {
    write_whole(&work.join("replay.jsonl"), report_jsonl(runs).as_bytes())?;
    write_whole(&work.join("replay.md"), report_md(header, runs).as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hushrun_build::model::PassedThrough;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn options(list: &[&str]) -> Options {
        match parse(&args(list)).unwrap() {
            Parsed::Run(options) => options,
            Parsed::Help => panic!("help"),
        }
    }

    fn refused(list: &[&str]) -> String {
        format!("{:#}", parse(&args(list)).unwrap_err())
    }

    /// git with this test's own identity and no machine configuration, signing nothing.
    fn git_test(repo: &Path, list: &[&str]) {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.name=Replay Test",
                "-c",
                "user.email=replay@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(list)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for name in GIT_LOCATION {
            command.env_remove(name);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "git {list:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn commit(repo: &Path, message: &str) -> String {
        git_test(repo, &["add", "-A"]);
        git_test(repo, &["commit", "-q", "-m", message]);
        git_lines(repo, &["rev-parse", "HEAD"]).unwrap().remove(0)
    }

    #[test]
    fn arguments_are_read_with_their_defaults() {
        let o = options(&["--repo", "r", "--work", "w", "--hushrun", "h"]);
        assert_eq!(o.selection, Selection::Last(10));
        assert_eq!(o.command, DEFAULT_COMMAND);
        assert_eq!(o.wrap, Wrap::Observe);
        assert_eq!(o.targets, Targets::Fresh);
        assert_eq!(o.paths, Paths::Rotate);
        assert!(!o.keep);
        assert_eq!(o.dir, None);

        let o = options(&[
            "--repo",
            "r",
            "--work",
            "w",
            "--mode",
            "none",
            "--dir",
            "./back/",
            "--commit",
            "abc",
            "--commit",
            "def",
            "--target",
            "shared",
            "--paths",
            "fixed",
            "--keep",
            "--command",
            "cargo build",
        ]);
        assert_eq!(o.wrap, Wrap::None);
        assert_eq!(o.hushrun, None);
        assert_eq!(o.dir, Some(PathBuf::from("back")));
        assert_eq!(
            o.selection,
            Selection::List(vec!["abc".into(), "def".into()])
        );
        assert_eq!(o.targets, Targets::Shared);
        assert_eq!(o.paths, Paths::Fixed);
        assert!(o.keep);
        assert_eq!(o.command, "cargo build");
        assert_eq!(
            options(&[
                "--repo", "r", "--work", "w", "--mode", "none", "--target", "inside"
            ])
            .targets,
            Targets::Inside
        );
        assert_eq!(
            options(&[
                "--repo",
                "r",
                "--work",
                "w",
                "--mode",
                "none",
                "--commits",
                "a..b"
            ])
            .selection,
            Selection::Range("a..b".into())
        );
        assert_eq!(
            options(&["--repo", "r", "--work", "w", "--mode", "none", "--dir", "."]).dir,
            None
        );
        assert!(matches!(parse(&args(&["--help"])).unwrap(), Parsed::Help));
    }

    #[test]
    fn arguments_that_cant_work_are_refused() {
        let base = ["--repo", "r", "--work", "w"];
        let with = |extra: &[&str]| refused(&[&base[..], extra].concat());
        assert!(with(&["--mode", "on"]).contains("--mode on needs --hushrun"));
        assert!(with(&[]).contains("--mode observe needs --hushrun"));
        assert!(refused(&["--work", "w", "--mode", "none"]).contains("--repo PATH is required"));
        assert!(refused(&["--repo", "r", "--mode", "none"]).contains("--work PATH is required"));
        assert!(with(&["--mode", "none", "--last", "3", "--commit", "a"]).contains("choose one"));
        assert!(with(&["--mode", "none", "--last", "0"]).contains("above 0"));
        assert!(with(&["--mode", "none", "--commits", "main"]).contains("a range"));
        assert!(with(&["--mode", "none", "--commit", "--all"]).contains("a revision"));
        assert!(with(&["--mode", "none", "--dir", "../x"]).contains("inside the repository"));
        assert!(with(&["--mode", "none", "--dir", "/abs"]).contains("inside the repository"));
        assert!(with(&["--mode", "sometimes", "--hushrun", "h"]).contains("observe, on or none"));
        assert!(with(&["--mode", "none", "--target", "x"]).contains("fresh, shared or inside"));
        assert!(with(&["--mode", "none", "--paths", "x"]).contains("rotate or fixed"));
        assert!(with(&["--mode", "none", "--command", " "]).contains("empty"));
        assert!(with(&["--mode", "none", "--bogus"]).contains("unexpected argument"));
        assert!(with(&["--mode", "none", "--last"]).contains("needs a value"));
    }

    #[test]
    fn work_inside_the_repository_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let env = Env::new();
        let plan = |work: &Path| {
            let o = options(&[
                "--repo",
                repo.to_str().unwrap(),
                "--work",
                work.to_str().unwrap(),
                "--mode",
                "none",
            ]);
            Plan::new(o, &env)
        };
        let inside = repo.join("not-yet/made");
        let err = format!("{:#}", plan(&inside).unwrap_err());
        assert!(err.contains("inside --repo"), "{err}");
        assert!(
            !repo.join("not-yet").exists(),
            "nothing is made before the checks"
        );
        let err = format!("{:#}", plan(temp.path()).unwrap_err());
        assert!(err.contains("inside --work"), "{err}");
        let beside = temp.path().join("work");
        let plan = plan(&beside).unwrap();
        assert_eq!(plan.options.work, resolve(&beside).unwrap());
        assert!(!beside.exists(), "Plan::new writes nothing");

        // Never hushrun's own folders.
        let home = temp.path().join("home");
        let env: Env = [(OsString::from("HOME"), home.clone().into_os_string())].into();
        let real = home.join("Library/Caches/hushrun/replay");
        let o = options(&[
            "--repo",
            repo.to_str().unwrap(),
            "--work",
            real.to_str().unwrap(),
            "--mode",
            "none",
        ]);
        let err = format!("{:#}", Plan::new(o, &env).unwrap_err());
        assert!(err.contains("hushrun's own folder"), "{err}");

        // A missing binary is refused too.
        let o = options(&[
            "--repo",
            repo.to_str().unwrap(),
            "--work",
            beside.to_str().unwrap(),
            "--hushrun",
            temp.path().join("no-such-hushrun").to_str().unwrap(),
        ]);
        assert!(Plan::new(o, &Env::new()).is_err());
    }

    #[test]
    fn last_n_takes_the_commits_that_touch_the_folder_oldest_first() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        git_test(repo, &["init", "-q"]);
        write(&repo.join("back/a.txt"), "1");
        let one = commit(repo, "one");
        write(&repo.join("front/b.txt"), "1");
        let front = commit(repo, "front only");
        write(&repo.join("back/a.txt"), "2");
        let two = commit(repo, "two");
        write(&repo.join("back/c.txt"), "1");
        write(&repo.join("front/b.txt"), "2");
        let three = commit(repo, "three");

        let back = Some(Path::new("back"));
        assert_eq!(
            select(repo, back, &Selection::Last(2)).unwrap(),
            vec![two.clone(), three.clone()]
        );
        assert_eq!(
            select(repo, back, &Selection::Last(10)).unwrap(),
            vec![one.clone(), two.clone(), three.clone()]
        );
        assert_eq!(
            select(repo, None, &Selection::Last(2)).unwrap(),
            vec![two.clone(), three.clone()]
        );
        assert_eq!(
            select(repo, None, &Selection::Last(10)).unwrap(),
            vec![one.clone(), front.clone(), two.clone(), three.clone()]
        );
        assert_eq!(
            select(repo, back, &Selection::Range(format!("{one}..HEAD"))).unwrap(),
            vec![two.clone(), three.clone()]
        );
        assert_eq!(
            select(
                repo,
                None,
                &Selection::List(vec![three[..8].to_string(), "HEAD~3".into()])
            )
            .unwrap(),
            vec![three.clone(), one.clone()]
        );
        assert!(select(repo, Some(Path::new("nowhere")), &Selection::Last(3)).is_err());
        assert!(select(repo, None, &Selection::List(vec!["no-such-rev".into()])).is_err());
    }

    #[test]
    fn the_environment_loses_what_would_change_or_leak_into_a_build() {
        let given = [
            ("HUSHRUN_X", "secret"),
            ("HUSHRUN_BUILD_DIR", "/real/store"),
            ("CARGO_PKG_NAME", "xtask"),
            ("CARGO_MANIFEST_DIR", "/x"),
            ("CARGO_CRATE_NAME", "xtask"),
            ("CARGO_BIN_NAME", "xtask"),
            ("CARGO_PRIMARY_PACKAGE", "1"),
            ("RUSTC_WRAPPER", "sccache"),
            ("RUSTC_WORKSPACE_WRAPPER", "w"),
            ("RUSTC", "rustc"),
            ("RUSTFLAGS", "-Cx"),
            ("CARGO_ENCODED_RUSTFLAGS", "-Cx"),
            ("CARGO_TARGET_DIR", "/t"),
            ("CARGO_BUILD_TARGET_DIR", "/t"),
            ("CARGO_MAKEFLAGS", "-j"),
            ("CARGO_INCREMENTAL", "0"),
            ("OUT_DIR", "/o"),
            ("RUSTUP_TOOLCHAIN", "1.94.0"),
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/dev"),
            ("CARGO_HOME", "/home/dev/.cargo"),
            ("CARGO", "/bin/cargo"),
        ];
        let env = cleaned_env(
            given
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        let names: Vec<&str> = env.keys().map(|name| name.to_str().unwrap()).collect();
        assert_eq!(names, vec!["CARGO", "CARGO_HOME", "HOME", "PATH"]);
        assert_eq!(env[OsStr::new("PATH")], "/usr/bin:/bin");
    }

    #[test]
    fn rotated_paths_differ_in_length_and_never_repeat() {
        let work = Path::new("/w");
        let paths: Vec<PathBuf> = (1..=40)
            .map(|i| export_root(work, i, Paths::Rotate))
            .collect();
        let distinct: std::collections::BTreeSet<&PathBuf> = paths.iter().collect();
        assert_eq!(distinct.len(), paths.len());
        for pair in paths.windows(2) {
            assert_ne!(
                pair[0].as_os_str().len(),
                pair[1].as_os_str().len(),
                "{pair:?}"
            );
        }
        assert!(paths.iter().all(|path| path.starts_with("/w/exports")));
        assert_eq!(
            export_root(work, 1, Paths::Fixed),
            export_root(work, 2, Paths::Fixed)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fixed_export_is_updated_in_place_as_a_checkout() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let (src, dest) = (temp.path().join("src"), temp.path().join("dest"));
        write(&dest.join("same.rs"), "same");
        write(&dest.join("changed.rs"), "old");
        write(&dest.join("gone/old.rs"), "x");
        write(&dest.join("mode.sh"), "run");
        write(&dest.join("was-file"), "f");
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        for name in ["same.rs", "changed.rs", "mode.sh"] {
            File::options()
                .write(true)
                .open(dest.join(name))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        write(&src.join("same.rs"), "same");
        write(&src.join("changed.rs"), "new");
        write(&src.join("new/added.rs"), "y");
        write(&src.join("mode.sh"), "run");
        std::fs::set_permissions(src.join("mode.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        write(&src.join("was-file/inner"), "d");

        write(&dest.join("target/debug/x"), "built");
        write(&dest.join("new/target/y"), "not kept");
        sync_tree(&src, &dest, Some(&dest.join("target"))).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("target/debug/x")).unwrap(),
            "built",
            "the folder kept is left as it is"
        );
        assert!(!dest.join("new/target").exists());
        let modified = |name: &str| {
            std::fs::metadata(dest.join(name))
                .unwrap()
                .modified()
                .unwrap()
        };
        assert_eq!(modified("same.rs"), old, "unchanged keeps its date");
        assert_ne!(modified("changed.rs"), old);
        assert_ne!(modified("mode.sh"), old, "a new mode is a change");
        assert_eq!(
            std::fs::read_to_string(dest.join("changed.rs")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("new/added.rs")).unwrap(),
            "y"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("was-file/inner")).unwrap(),
            "d"
        );
        assert!(!dest.join("gone").exists());
        let mut names: Vec<String> = std::fs::read_dir(&dest)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "changed.rs",
                "mode.sh",
                "new",
                "same.rs",
                "target",
                "was-file"
            ]
        );
    }

    #[test]
    fn times_are_read_exactly() {
        assert_eq!(
            parse_times("real 3.21\nuser 12.34\nsys 0.05\n"),
            (Some(12_340), Some(50))
        );
        assert_eq!(
            parse_times("Command exited with non-zero status 101\nreal 1.00\nuser 2\nsys 0.1234\n"),
            (Some(2_000), Some(123))
        );
        assert_eq!(parse_times("nothing here"), (None, None));
        assert_eq!(seconds_to_ms("0.0005"), Some(1));
        assert_eq!(seconds_to_ms("x.1"), None);
        assert_eq!(seconds(12_345), "12.345");
        assert_eq!(seconds(7), "0.007");
    }

    fn entry(outcome: Outcome, class: Option<Class>, name: &str) -> Entry {
        Entry {
            outcome,
            class,
            crate_name: name.to_string(),
            package: String::new(),
            rustc_ms: 0,
            saved_ms: 0,
            stored_bytes: 0,
            found: false,
            differed: false,
            note: None,
            portable: None,
            step: None,
            key: None,
            downloaded: false,
        }
    }

    fn commit_run(index: usize, entries: &[Entry], exit_code: Option<i32>) -> CommitRun {
        CommitRun {
            index,
            sha: format!("{index:0>40}"),
            export: PathBuf::from(format!("/w/exports/c{index:03}/src")),
            target: PathBuf::from("/w/target"),
            log: PathBuf::from(format!("/w/logs/c{index:03}.log")),
            error: None,
            exit_code,
            wall_ms: 1_500,
            user_ms: Some(1_000),
            sys_ms: Some(250),
            counts: Counts::of(entries),
            store: Some(StoreSize {
                blocks: index as u64,
                bytes: 1_000 * index as u64,
            }),
        }
    }

    fn header() -> Header {
        Header {
            repo: "/r".into(),
            dir: "back".into(),
            command: DEFAULT_COMMAND.into(),
            mode: "observe",
            targets: "fresh",
            paths: "rotate",
            hushrun: Some("/h".into()),
            store_at_start: Some(StoreSize::default()),
        }
    }

    #[test]
    fn the_report_counts_exactly_and_a_difference_fails_it() {
        let lib = Some(Class::Library);
        let changed = Outcome::BuiltNotStored(NotStored::Changed);
        let probe = Outcome::PassedThrough(PassedThrough::Probe);
        let first = vec![
            Entry {
                rustc_ms: 200,
                stored_bytes: 1_000,
                ..entry(Outcome::BuiltAndStored, lib, "serde")
            },
            Entry {
                rustc_ms: 50,
                ..entry(changed, Some(Class::Program), "hushrun")
            },
            entry(probe, None, ""),
        ];
        let second = vec![
            Entry {
                saved_ms: 180,
                found: true,
                ..entry(Outcome::Reused, lib, "serde")
            },
            Entry {
                rustc_ms: 30,
                found: true,
                differed: true,
                ..entry(Outcome::BuiltAndStored, lib, "hushrun_testkit")
            },
            Entry {
                rustc_ms: 10,
                ..entry(changed, lib, "a")
            },
            Entry {
                rustc_ms: 10,
                ..entry(changed, lib, "b")
            },
            Entry {
                rustc_ms: 10,
                ..entry(changed, lib, "c")
            },
            entry(
                Outcome::PassedThrough(PassedThrough::NotRustc),
                Some(Class::ProcMacro),
                "clippy_me",
            ),
        ];
        let clean = vec![commit_run(1, &first, Some(0))];
        let runs = vec![
            commit_run(1, &first, Some(0)),
            commit_run(2, &second, Some(101)),
        ];

        let totals = Totals::of(&runs);
        let t = &totals.counts;
        assert_eq!(t.compiles, 9);
        assert_eq!(t.outcome("reused"), 1);
        assert_eq!(t.outcome("built-and-stored"), 2);
        assert_eq!(t.outcome("built-not-stored"), 4);
        assert_eq!(t.outcome("passed-through"), 2);
        assert_eq!(t.found, 2);
        assert_eq!(t.differed, vec!["hushrun_testkit".to_string()]);
        assert_eq!(t.rustc_ms, 310);
        assert_eq!(t.saved_ms, 180);
        assert_eq!(t.stored_bytes, 1_000);
        assert_eq!(t.by_class["library"], 6);
        assert_eq!(t.by_class["program"], 1);
        assert_eq!(t.by_class["proc-macro"], 1);
        assert_eq!(t.by_class[UNKNOWN_CLASS], 1);
        assert_eq!(t.reasons("built-not-stored"), vec![("changed", 4)]);
        assert_eq!(
            t.reasons("passed-through"),
            vec![("not-rustc", 1), ("probe", 1)]
        );
        // Three examples, first seen first, unnamed crates left out.
        assert_eq!(
            t.examples[&("built-not-stored", "changed")],
            vec!["hushrun", "a", "b"]
        );
        assert!(t.examples[&("passed-through", "probe")].is_empty());
        assert_eq!(totals.failed, 1);
        // Two build scripts, told apart by their packages.
        let scripts = |package: &str| Entry {
            package: package.to_string(),
            differed: true,
            ..entry(
                Outcome::BuiltNotStored(NotStored::Changed),
                Some(Class::BuildScript),
                "build_script_build",
            )
        };
        let c = Counts::of(&[
            scripts("libc-0.2.190"),
            scripts("serde-1.0.229"),
            scripts(""),
        ]);
        assert_eq!(
            c.differed,
            [
                "build_script_build (libc-0.2.190)",
                "build_script_build (serde-1.0.229)",
                "build_script_build",
            ]
        );
        assert_eq!(
            c.examples[&("built-not-stored", "changed")],
            [
                "build_script_build (libc-0.2.190)",
                "build_script_build (serde-1.0.229)",
                "build_script_build",
            ]
        );
        assert_eq!(totals.wall_ms, 3_000);
        assert_eq!(totals.cpu_ms, Some(2_500));

        let jsonl = report_jsonl(&runs);
        let lines: Vec<Value> = jsonl
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["sha"], runs[1].sha);
        assert_eq!(lines[1]["exit_code"], 101);
        assert_eq!(lines[1]["compiles"], 6);
        assert_eq!(lines[1]["by_outcome"]["reused"], 1);
        assert_eq!(lines[1]["by_outcome"]["passed-through"], 1);
        assert_eq!(lines[0]["by_outcome"]["reused"], 0);
        assert_eq!(lines[1]["by_reason"]["built-not-stored"]["changed"], 3);
        assert_eq!(lines[1]["by_class"]["library"], 5);
        assert_eq!(lines[1]["found"], 2);
        assert_eq!(lines[1]["differed"], 1);
        assert_eq!(lines[1]["differed_crates"][0], "hushrun_testkit");
        assert_eq!(lines[1]["rustc_ms"], 60);
        assert_eq!(lines[1]["saved_ms"], 180);
        assert_eq!(lines[0]["stored_bytes"], 1_000);
        assert_eq!(lines[1]["store_bytes"], 2_000);
        assert_eq!(lines[1]["cpu_ms"], 1_250);

        let md = report_md(&header(), &runs);
        assert!(
            md.contains("Differences: 1 in 9 compiles over 2 commits"),
            "{md}"
        );
        assert!(md.contains("NOT met"));
        assert!(md.contains("1 commit(s) failed"));
        assert!(md.contains("- `hushrun_testkit` at #2"));
        assert!(md.contains("| changed | 4 | `hushrun`, `a`, `b` |"), "{md}");
        assert!(md.contains("| probe | 1 | – |"), "{md}");
        assert!(md.contains("| **total** | 2 commits | 1 failed | 3.000 | 2.500 | 9 | 1 | 2 | 4 | 2 | 2 | 1 | 0.310 | 0.180 | 1000 | 2000 |"), "{md}");
        assert!(md.contains("## Commits that failed"));

        // A difference is what fails the replay; with none it passes.
        assert!(!totals.counts.differed.is_empty());
        let totals = Totals::of(&clean);
        assert!(totals.counts.differed.is_empty());
        assert!(report_md(&header(), &clean).contains("Crates that differed\n\nNone."));
        // An audit's difference counts too, even without the flag.
        let audit = Counts::of(&[entry(
            Outcome::BuiltNotStored(NotStored::Differed),
            lib,
            "x",
        )]);
        assert_eq!(audit.differed, vec!["x".to_string()]);
    }

    /// The workspace's pinned toolchain, so the test crate builds with what is installed.
    fn toolchain_file() -> String {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../rust-toolchain.toml"),
        )
        .unwrap();
        let channel = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("channel"))
            .unwrap()
            .trim_start_matches([' ', '='])
            .trim();
        format!("[toolchain]\nchannel = {channel}\nprofile = \"minimal\"\n")
    }

    /// A git repository with a tiny crate under `app/`: two commits that touch it and one that
    /// doesn't.
    fn tiny_repo(repo: &Path) -> Vec<String> {
        git_test(repo, &["init", "-q"]);
        write(&repo.join("app/rust-toolchain.toml"), &toolchain_file());
        write(
            &repo.join("app/Cargo.toml"),
            "[package]\nname = \"replay-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        );
        write(&repo.join("app/src/main.rs"), "fn main() {}\n");
        let one = commit(repo, "one");
        write(
            &repo.join("app/src/main.rs"),
            "fn main() {\n    println!(\"two\");\n}\n",
        );
        let two = commit(repo, "two");
        write(&repo.join("README.md"), "not the app\n");
        commit(repo, "readme");
        vec![one, two]
    }

    fn read_jsonl(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    // Real cargo, no wrapper: export, build and report work end to end.
    #[test]
    fn a_baseline_replay_exports_builds_and_reports() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&repo).unwrap();
        let shas = tiny_repo(&repo);

        let passed = run(&args(&[
            "--repo",
            repo.to_str().unwrap(),
            "--dir",
            "app",
            "--last",
            "5",
            "--mode",
            "none",
            "--work",
            work.to_str().unwrap(),
            "--keep",
            "--command",
            "cargo build --offline --quiet",
        ]))
        .unwrap();
        assert!(passed);

        let lines = read_jsonl(&work.join("replay.jsonl"));
        assert_eq!(lines.len(), 2);
        let mut exports = Vec::new();
        for (line, sha) in lines.iter().zip(&shas) {
            assert_eq!(line["sha"], sha.as_str());
            assert_eq!(line["exit_code"], 0, "{line}");
            assert_eq!(line["error"], Value::Null);
            assert_eq!(line["compiles"], 0);
            assert_eq!(line["store_bytes"], Value::Null);
            let export = PathBuf::from(line["export"].as_str().unwrap());
            assert!(export.starts_with(resolve(&work).unwrap()));
            assert!(export.ends_with("src/app"), "{}", export.display());
            assert!(export.join("Cargo.toml").is_file());
            assert!(!export.parent().unwrap().join("README.md").exists());
            let target = PathBuf::from(line["target"].as_str().unwrap());
            assert!(target.join("debug/replay-probe").is_file());
            exports.push(export);
        }
        assert_ne!(exports[0], exports[1]);
        assert_ne!(exports[0].as_os_str().len(), exports[1].as_os_str().len());
        let md = std::fs::read_to_string(work.join("replay.md")).unwrap();
        assert!(
            md.contains("Differences: 0 in 0 compiles over 2 commits"),
            "{md}"
        );
        assert!(md.contains("- mode: none; targets: fresh; paths: rotate"));
    }

    // `--target inside`: no CARGO_TARGET_DIR, so cargo writes `target` in the folder it builds
    // in, as in a worktree; the target goes with its export, and a fixed export keeps it from one
    // commit to the next.
    #[test]
    fn a_replay_with_the_target_inside_builds_as_a_worktree_does() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let shas = tiny_repo(&repo);
        let replay = |work: &Path, paths: &str, keep: bool| {
            let mut list = vec![
                "--repo",
                repo.to_str().unwrap(),
                "--dir",
                "app",
                "--commit",
                &shas[0],
                "--commit",
                &shas[1],
                "--mode",
                "none",
                "--work",
                work.to_str().unwrap(),
                "--target",
                "inside",
                "--paths",
                paths,
                "--command",
                "cargo build --offline --quiet && env > seen-env.txt && ls target > seen-target.txt",
            ];
            if keep {
                list.push("--keep");
            }
            assert!(run(&args(&list)).unwrap());
            read_jsonl(&work.join("replay.jsonl"))
        };

        // Rotated and kept: each export holds its own target, and cargo was given no other.
        let work = temp.path().join("kept");
        let lines = replay(&work, "rotate", true);
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert_eq!(line["exit_code"], 0, "{line}");
            let export = PathBuf::from(line["export"].as_str().unwrap());
            let target = PathBuf::from(line["target"].as_str().unwrap());
            assert_eq!(target, export.join("target"));
            assert!(target.join("debug/replay-probe").is_file());
            let env = std::fs::read_to_string(export.join("seen-env.txt")).unwrap();
            assert!(
                !env.lines().any(|l| l.starts_with("CARGO_TARGET_DIR=")),
                "{env}"
            );
        }
        let md = std::fs::read_to_string(work.join("replay.md")).unwrap();
        assert!(
            md.contains("- mode: none; targets: inside; paths: rotate"),
            "{md}"
        );

        // Rotated, not kept: each export goes with its target.
        let work = temp.path().join("rotated");
        replay(&work, "rotate", false);
        let work = resolve(&work).unwrap();
        assert_eq!(std::fs::read_dir(work.join("exports")).unwrap().count(), 0);

        // Fixed: the second commit builds in the first one's target, updated in place, which
        // goes with the export after the last commit.
        let work = temp.path().join("fixed");
        let lines = replay(&work, "fixed", false);
        assert_eq!(lines[0]["target"], lines[1]["target"]);
        let work = resolve(&work).unwrap();
        assert!(!work.join("exports/fixed").exists());
        assert!(!work.join("target").exists());
    }

    // A fake wrapper, as cargo calls it through the session's link: it logs one entry per
    // compile to the session and runs rustc. The replay passes the session on, reads the log
    // before the session ends, keeps everything under --work and fails on a difference.
    #[cfg(unix)]
    #[test]
    fn a_wrapped_replay_reads_the_wrappers_log_and_fails_on_a_difference() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&repo).unwrap();
        let shas = tiny_repo(&repo);
        let fake = temp.path().join("fake-hushrun");
        write(
            &fake,
            r#"#!/bin/sh
rustc="$1"; shift
name=""; prev=""; probe=""
for a in "$@"; do
  if [ "$prev" = "--crate-name" ]; then name="$a"; fi
  case "$a" in --print*|-vV|-) probe=1 ;; esac
  prev="$a"
done
if [ -n "$name" ] && [ -z "$probe" ] && [ -n "$HUSHRUN_BUILD_SESSION" ]; then
  printf '{"outcome":{"outcome":"built-and-stored"},"class":"program","crate_name":"%s","rustc_ms":7,"saved_ms":0,"stored_bytes":3,"found":true,"differed":true,"note":null}\n' "$name" >> "$HUSHRUN_BUILD_SESSION/log.jsonl"
fi
exec "$rustc" "$@"
"#,
        );
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let passed = run(&args(&[
            "--repo",
            repo.to_str().unwrap(),
            "--dir",
            "app",
            "--commit",
            &shas[0],
            "--commit",
            &shas[1],
            "--mode",
            "observe",
            "--hushrun",
            fake.to_str().unwrap(),
            "--work",
            work.to_str().unwrap(),
            "--target",
            "shared",
            "--paths",
            "fixed",
            "--command",
            "cargo build --offline --quiet",
        ]))
        .unwrap();
        assert!(!passed, "a difference fails the replay");

        let lines = read_jsonl(&work.join("replay.jsonl"));
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert_eq!(line["exit_code"], 0, "{line}");
            assert_eq!(line["compiles"], 1, "{line}");
            assert_eq!(line["by_outcome"]["built-and-stored"], 1);
            assert_eq!(line["differed_crates"][0], "replay_probe");
            assert_eq!(line["rustc_ms"], 7);
            assert_eq!(line["stored_bytes"], 3);
            assert_eq!(line["store_blocks"], 0);
            assert_eq!(line["export"], lines[0]["export"], "fixed paths");
        }
        // Exports are gone, the shared target and the store stay, sessions are finished.
        let work = resolve(&work).unwrap();
        assert!(!work.join("exports/fixed").exists());
        assert!(work.join("target/debug/replay-probe").is_file());
        let store = work.join("cache/build");
        let link_dirs: Vec<_> = std::fs::read_dir(store.join("bin")).unwrap().collect();
        assert_eq!(link_dirs.len(), 1);
        let sessions = std::fs::read_dir(store.join("sessions")).unwrap().count();
        assert_eq!(sessions, 0);
        let raw = std::fs::read_dir(work.join("logs"))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".entries.jsonl")
            })
            .count();
        assert_eq!(raw, 2);
        let settings = settings::load(&work.join("build.json")).unwrap();
        assert_eq!(settings.mode, Mode::Observe);
        let md = std::fs::read_to_string(work.join("replay.md")).unwrap();
        assert!(md.contains("Differences: 2 in 2 compiles"), "{md}");
        assert!(md.contains("- `replay_probe` at #1"));
    }
}
