//! B-2, B-19: the build blocks' session for a command hushrun is about to run, the one
//! definition both forms of `hushrun run` use: `NAME -- COMMAND` (`cmd::wrap`) and each check of
//! hushrun.yaml that runs here (`cmd::run`). An isolated check runs in a container where the link
//! and the store don't exist, so it never asks for one. Only `NAME -- COMMAND` hands [`begin`] a
//! backend to share through ([`ShareStart`]): `CHECK…` hands none, so it shares nothing (B-25),
//! reads only a saved fetch outside CI, and in CI begins no session.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use hushrun_build::session::{self, NotBegun, Session};
use hushrun_build::share::Caller;
use hushrun_ci::Env;
use hushrun_core::time::Time;

use crate::context;
use crate::credentials::Credentials;

/// The longest the check before COMMAND may take (B-23): past it, the crates left are not
/// checked this run.
const CHECK_BUDGET: Duration = Duration::from_secs(2);

/// What every line that says why a run has no session starts with.
const OFF: &str = "build reuse is off for this run";

/// The variable that names the store instead of the cache directory (B-13).
const STORE_DIR: &str = "HUSHRUN_BUILD_DIR";

/// Starts the session for a command hushrun is about to run, when reuse is on here. With none,
/// the command runs as usual (B-19), and `Err` holds the one line the person should read,
/// without hushrun's prefix: settings or a variable that can't be used, or a binary hushrun
/// can't find. Every other reason for none is silent. `repository_env` is the repository's
/// `build: env:` (B-35), which every compile of the run is keyed by. `place` gives the run's
/// backend, which a saved fetch must be from (B-36), asked only when the store holds one.
pub(crate) fn begin(
    now: SystemTime,
    repository: Option<&str>,
    repository_env: &[String],
    share: Option<ShareStart>,
    place: &dyn Fn() -> Option<crate::results::BuildPlace>,
) -> Result<Session, Option<String>> {
    // B-2: a compiler wrapper set already passes through. That is worth its line only where the
    // run's results lookup said the org shares: there the person may expect the shared store.
    let org_shares = share.as_ref().is_some_and(|start| start.access.shares);
    begin_here(now, repository, repository_env, share, place)
        .map_err(|line| line.filter(|line| line != WRAPPER_SET || org_shares))
}

/// The line of a run that begins no session because `RUSTC_WRAPPER` is set already (B-2), which
/// [`begin`] says only in an org that shares.
const WRAPPER_SET: &str =
    "build reuse is off for this run: another compiler wrapper is set (RUSTC_WRAPPER)";

/// [`begin`], with the wrapper's line whatever the org said.
fn begin_here(
    now: SystemTime,
    repository: Option<&str>,
    repository_env: &[String],
    share: Option<ShareStart>,
    place: &dyn Fn() -> Option<crate::results::BuildPlace>,
) -> Result<Session, Option<String>> {
    let Some(settings) = Credentials::build_settings_path() else {
        return Err(None);
    };
    let cache_dir = context::cache_dir();
    let env = |name: &str| hushrun_ci::var_os(name);
    let here = Here {
        settings: &settings,
        cache_dir: cache_dir.as_deref(),
        in_ci: hushrun_ci::in_ci(&Env::from_process()),
        repository,
        repository_env,
        env: &env,
        now,
        folder: std::env::current_dir().ok(),
        sysroot: &|folder| rustc_sysroot(folder, &env),
    };
    if here.in_ci {
        return begin_in_ci(&here, share, repository, place);
    }
    // B-36: the run's backend answered nothing, and the store holds a fresh fetch from it, of
    // this repository.
    if let Some(found) = saved_fetch(&here, place) {
        return begin_from_saved(&here, repository, found, None);
    }
    // B-25: an agent token on a machine where no person is signed in (a sandbox), whose org
    // shares: reuse is on here, and sharing is the org's mode.
    let sandbox_shares = share.as_ref().is_some_and(|start| {
        caller(Some(&start.access), false) == (Caller::Agent { sandbox: true })
            && (start.access.shares || start.open_decides)
    });
    let share = match share {
        Some(start) if sandbox_shares => return begin_sandbox_sharing(&here, start, repository),
        other => other,
    };
    let mut session = begin_with(&here, std::env::current_exe(), None)?;
    if let Some(start) = share {
        // B-34: the run would share, and its backend answered nothing it was asked.
        let unreached = unreached(&session, &start);
        match start_sharing(&mut session, start, repository, &env) {
            Ok(_) => {}
            Err(line) => {
                if let Some(line) = line {
                    session.add_line(line);
                }
                if unreached {
                    let _ = session.set_unreached();
                }
            }
        }
    }
    Ok(session)
}

/// B-34: whether this run has no relay because its backend couldn't be reached, and for no
/// other reason: the run's results lookup got no answer of any kind (so nothing said whether
/// the org shares), and what the run does would share had it said yes. A person who turned
/// sharing off here, a local mode of `off`, or an answer without `Hushrun-Build: on` is a
/// decision, and so is every run [`begin`] is handed no backend for.
fn unreached(session: &Session, start: &ShareStart) -> bool {
    use hushrun_build::share::{Org, Sharing, what_a_run_does};
    if start.access.answered || start.access.shares || start.open_decides || cfg!(not(unix)) {
        return false;
    }
    let current = session.current();
    let caller = caller(Some(&start.access), false);
    let does = what_a_run_does(
        caller,
        current.mode,
        current.share_setting,
        Org::Shares(None),
    );
    !matches!(does.sharing, Sharing::Off(_))
}

/// The store's fresh fetch (B-36), with its folder, when it may serve this run: see
/// [`Saved::serves`](crate::cmd::build_relay::saved::Saved::serves).
fn saved_fetch(
    here: &Here<'_>,
    place: &dyn Fn() -> Option<crate::results::BuildPlace>,
) -> Option<(crate::cmd::build_relay::saved::Saved, PathBuf)> {
    let overrides = hushrun_build::settings::Overrides::from(here.env).ok()?;
    let store = session::store_dir(here.cache_dir, &overrides)?;
    crate::cmd::build_relay::saved::Saved::load(&hushrun_build::store::Store::new(&store), here.now)
        .filter(|(saved, _)| saved.serves(place().as_ref(), here.repository))
}

/// B-36: a run whose store holds a fresh fetch that serves it: a relay that reads only the
/// fetch, opens its packs as wrappers bring their keys, and sends nothing. A machine where no
/// person is signed in to the fetch's backend is a sandbox ([`signed_in_here`]; the fetch stands
/// for the agent token that made it, which a later phase no longer holds): reuse is on there;
/// else the person's mode and share setting decide, as for any run. In CI (`ci`: the job's store and who the job is) the session
/// is the job's, in its own store, as B-31 makes any.
fn begin_from_saved(
    here: &Here<'_>,
    repository: Option<&str>,
    (saved, folder): (crate::cmd::build_relay::saved::Saved, PathBuf),
    ci: Option<(&Path, hushrun_build::share::Caller)>,
) -> Result<Session, Option<String>> {
    use hushrun_build::share::{Sharing, what_a_run_does};
    let sandbox = !signed_in_here(&saved.host);
    let overrides = hushrun_build::settings::Overrides::from(here.env)
        .map_err(|err| Some(format!("{OFF}: {err}")))?;
    let mut session = match (ci, session::store_dir(here.cache_dir, &overrides)) {
        (Some((store, _)), _) => begin_with(here, std::env::current_exe(), Some(store))?,
        (None, Some(store)) if sandbox => begin_sandbox(here, &store)?,
        (None, _) => begin_with(here, std::env::current_exe(), None)?,
    };
    let current = session.current().clone();
    let caller = match ci {
        Some((_, caller)) => caller,
        None if sandbox => Caller::Agent { sandbox: true },
        None => Caller::Person { signed_in: true },
    };
    let does = what_a_run_does(caller, current.mode, current.share_setting, saved.org());
    if matches!(does.sharing, Sharing::Off(_)) || cfg!(not(unix)) {
        return Ok(session);
    }
    let access = crate::results::BuildAccess {
        base: saved.host.clone(),
        host: saved.host.clone(),
        org: saved.org.clone(),
        credential: None,
        agent: sandbox && ci.is_none(),
        machine: None,
        shares: true,
        answered: false,
    };
    let config = || {
        Some(crate::cmd::build_relay::Config {
            store: hushrun_build::store::Store::new(&current.store),
            access,
            repository: repository.map(str::to_string),
            commit: None,
            platform: platform(),
            compiler: None,
            caller,
            local: current.mode,
            own: current.share_setting,
            min_free_gb: current.min_free_gb,
            keep_days: current.keep_days,
            tuning: crate::cmd::build_relay::Tuning::default(),
            saved: Some((saved, folder)),
            open_decides: false,
        })
    };
    let Some(relay) = crate::cmd::build_relay::Relay::get_or_start(config) else {
        return Ok(session);
    };
    match relay.begin_check(&current.folder, None, here.env) {
        Ok(share) => {
            if let Err(err) = session.set_share(share) {
                session.add_line(format!("build: sharing is off for this run: {err}"));
            }
        }
        Err(line) => session.add_line(line),
    }
    Ok(session)
}

/// A sandbox's session in `store` (B-36), as `begin_with` makes any.
fn begin_sandbox(here: &Here<'_>, store: &Path) -> Result<Session, Option<String>> {
    let exe = std::env::current_exe()
        .map_err(|err| Some(format!("{OFF}: hushrun can't find its own binary: {err}")))?;
    let start = here.start(&exe, here.cache_dir, here.env);
    let mut session = session::begin_in_sandbox(&start, store).or_else(said)?;
    if let Some(folder) = &here.folder {
        session.check(folder, Some(Instant::now() + CHECK_BUDGET));
        if let Some(sysroot) = (here.sysroot)(folder) {
            session.hide_sources(&sysroot);
        }
    }
    Ok(session)
}

/// B-25: an agent token's run in a sandbox (no person signed in here) whose org shares: reuse is
/// on here, in the store, unless `HUSHRUN_BUILD` says otherwise, and the relay shares as the org's
/// mode says. A run that made no results lookup (`open_decides`) learns whether the org shares
/// only from `open`, which it waits for as a CI job does: an org that doesn't share then leaves
/// no session, silently, and one that doesn't answer in time leaves none with one line.
fn begin_sandbox_sharing(
    here: &Here<'_>,
    start: ShareStart,
    repository: Option<&str>,
) -> Result<Session, Option<String>> {
    let overrides = hushrun_build::settings::Overrides::from(here.env)
        .map_err(|err| Some(format!("{OFF}: {err}")))?;
    let Some(store) = session::store_dir(here.cache_dir, &overrides) else {
        return Err(None);
    };
    let mut session = begin_sandbox(here, &store)?;
    let open_decides = start.open_decides;
    let relay = match start_sharing(&mut session, start, repository, here.env) {
        Ok(relay) => relay,
        // The org shares, as the results lookup said: reuse here stays on, sharing is off.
        Err(line) if !open_decides => {
            if let Some(line) = line {
                session.add_line(line);
            }
            return Ok(session);
        }
        Err(line) => {
            session.finish(here.now);
            return Err(line);
        }
    };
    if !open_decides {
        return Ok(session);
    }
    let wait = crate::cmd::build_relay::Tuning::default().ci_open;
    match relay.wait_open(wait) {
        crate::cmd::build_relay::Opened::Answering => Ok(session),
        opened => {
            let said = relay.end_check(&[]);
            let line = match opened {
                crate::cmd::build_relay::Opened::Unanswered => Some(NO_ANSWER.to_string()),
                crate::cmd::build_relay::Opened::Stopped(_) => said.into_iter().next(),
                // Silent, but for an org whose plan sets its storage by contract.
                crate::cmd::build_relay::Opened::Off => said
                    .into_iter()
                    .find(|line| line.ends_with(crate::cmd::build_relay::BY_CONTRACT)),
                crate::cmd::build_relay::Opened::Answering => None,
            };
            session.finish(here.now);
            Err(line)
        }
    }
}

/// The line a CI job reads when the backend didn't answer in time, the results lookup or `open`
/// (B-2): COMMAND runs as it would without hushrun.
const NO_ANSWER: &str = "build reuse is off for this run: the backend didn't answer in time";

/// What every CI store's folder is named with.
const CI_STORE: &str = "hushrun-build";

/// B-31: the GitHub Actions job this process runs in, as a digest nobody else can make: of the
/// repository, the run, its attempt and the job (`GITHUB_REPOSITORY_ID` or `GITHUB_REPOSITORY`,
/// `GITHUB_RUN_ID`, `GITHUB_RUN_ATTEMPT`, `GITHUB_JOB`), and of the job's own request token
/// (`ACTIONS_ID_TOKEN_REQUEST_TOKEN`), which is the same in every step of one job and which no
/// other job has. `None` without that token: then nothing names the job that another couldn't.
/// The one definition the job's store and its credential's fingerprint are named by.
pub(crate) fn github_job(env: &dyn Fn(&str) -> Option<OsString>) -> Option<GithubJob> {
    let var = |name: &str| {
        env(name)
            .map(|value| value.to_string_lossy().trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let request = var("ACTIONS_ID_TOKEN_REQUEST_TOKEN")?;
    let repository = var("GITHUB_REPOSITORY_ID")
        .or_else(|| var("GITHUB_REPOSITORY"))
        .unwrap_or_default();
    let run = var("GITHUB_RUN_ID").unwrap_or_default();
    let attempt = var("GITHUB_RUN_ATTEMPT").unwrap_or_default();
    let job = var("GITHUB_JOB").unwrap_or_default();
    let digest = hushrun_build::origin::fingerprint(&format!(
        "hushrun.ci-job/v1\n{repository}\n{run}\n{attempt}\n{job}\n{request}"
    ));
    let plain = |text: &str| -> String {
        text.chars()
            .filter(char::is_ascii_alphanumeric)
            .take(20)
            .collect()
    };
    Some(GithubJob {
        folder: format!(
            "{CI_STORE}-{}-{}-{}",
            plain(&run),
            plain(&attempt),
            &digest[..16]
        ),
        digest,
    })
}

/// A GitHub Actions job (B-31): see [`github_job`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubJob {
    /// What tells this job from every other, in hex.
    pub(crate) digest: String,
    /// Its store's folder name: `hushrun-build-<run>-<attempt>-<digest>`.
    pub(crate) folder: String,
}

/// Where a CI job's store is (B-31), and who removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CiStore {
    /// Under the job's own temporary folder (`RUNNER_TEMP`), named by the job ([`github_job`]):
    /// the runner empties the folder when the job ends, and a folder it kept is another job's,
    /// whose name this job can't have.
    Job(PathBuf),
    /// No such folder, or no job to name it by: a folder of this process's under the system's
    /// temporary folder, made by this run and removed when its session ends.
    Run(PathBuf),
}

impl CiStore {
    fn of(env: &dyn Fn(&str) -> Option<OsString>) -> CiStore {
        match (env("RUNNER_TEMP").map(PathBuf::from), github_job(env)) {
            (Some(temp), Some(job)) if temp.is_absolute() => CiStore::Job(temp.join(job.folder)),
            _ => CiStore::Run(
                std::env::temp_dir().join(format!("{CI_STORE}-{}", std::process::id())),
            ),
        }
    }

    fn path(&self) -> &Path {
        match self {
            CiStore::Job(path) | CiStore::Run(path) => path,
        }
    }

    /// B-31: makes sure the store is this job's before anything in it is read. Beside a job's
    /// store, every other store folder in the job's temporary folder is another job's (a
    /// runner that kept it): it is removed, and never read. A run's store found at the start is
    /// another process's that died with the same id: it is removed, and the store is made anew
    /// by this run; where it can't be, there is no store. `Err` is the line to say.
    fn prepare(&self) -> Result<(), String> {
        match self {
            CiStore::Job(path) => {
                let (Some(parent), Some(own)) = (path.parent(), path.file_name()) else {
                    return Ok(());
                };
                for entry in std::fs::read_dir(parent).into_iter().flatten().flatten() {
                    let name = entry.file_name();
                    if name != own && name.to_string_lossy().starts_with(CI_STORE) {
                        let other = entry.path();
                        let _ = if other.is_dir() && !other.is_symlink() {
                            std::fs::remove_dir_all(&other)
                        } else {
                            std::fs::remove_file(&other)
                        };
                    }
                }
                Ok(())
            }
            CiStore::Run(path) => {
                if std::fs::symlink_metadata(path).is_ok() {
                    let _ = std::fs::remove_dir_all(path).or_else(|_| std::fs::remove_file(path));
                }
                // The guard is in the write: only a folder this call creates is the run's.
                std::fs::create_dir(path).map_err(|err| {
                    format!(
                        "{OFF}: another run's store at {} can't be removed: {err}",
                        path.display()
                    )
                })
            }
        }
    }
}

/// B-31, B-36: a CI job's own store, made ready as a run's is, when the job has one that a
/// later step finds again (GitHub Actions with a request token): where `hushrun build fetch`
/// saves in CI. `None` elsewhere in CI, where each run's store is its own.
pub(crate) fn ci_job_store(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    match CiStore::of(env) {
        CiStore::Job(path) => {
            CiStore::Job(path.clone()).prepare().ok()?;
            Some(path)
        }
        CiStore::Run(_) => None,
    }
}

/// B-25: whether a person is signed in on this machine to the backend at `host` (its origin):
/// the credentials file holds a login for it, whatever credential this command carries. The one
/// definition of what tells a person's machine from a sandbox.
pub(crate) fn signed_in_here(host: &str) -> bool {
    Credentials::load().is_ok_and(|credentials| credentials.person_at(host))
}

/// Who the run is, for B-25's table: the one mapping from what the backend gives the relay's
/// requests to `hushrun_build::share::Caller`, for the session, the relay and `hushrun build
/// status`. An agent token's run is in a sandbox only when no person is signed in on this machine
/// to that backend ([`signed_in_here`]); where one is, it is an agent on a person's machine, and
/// follows that person's settings. A settings file, or none, says nothing about it.
pub(crate) fn caller(access: Option<&crate::results::BuildAccess>, in_ci: bool) -> Caller {
    use crate::results::BuildCredential;
    let credential = access.and_then(|access| access.credential.as_ref());
    let agent = access.is_some_and(|access| access.agent);
    match (credential, access) {
        _ if in_ci => Caller::Ci {
            id_token: matches!(credential, Some(BuildCredential::GithubOidc { .. })),
            agent_token: matches!(credential, Some(BuildCredential::Token(_))) && agent,
        },
        (Some(_), Some(access)) if agent => Caller::Agent {
            sandbox: !signed_in_here(&access.host),
        },
        (Some(_), _) => Caller::Person { signed_in: true },
        (None, _) => Caller::Person { signed_in: false },
    }
}

/// B-2, B-31: a CI job's session. Only when the run's results lookup said the org shares
/// (`Hushrun-Build: on`), the job can be asked as itself, and `open` answers within 5 seconds:
/// then the wrapper is set, in a store under the job's own temporary folder. Else COMMAND runs as
/// it always has, silent when the org doesn't share, one line when the backend didn't answer
/// or the job has no credential.
fn begin_in_ci(
    here: &Here<'_>,
    share: Option<ShareStart>,
    repository: Option<&str>,
    place: &dyn Fn() -> Option<crate::results::BuildPlace>,
) -> Result<Session, Option<String>> {
    use hushrun_build::share::{Org, Sharing, what_a_run_does};
    let Some(start) = share else {
        return Err(None);
    };
    // B-25: a run that made no results lookup lets `open` say whether the org shares, waited for
    // below as any job's.
    if !start.access.shares && !start.open_decides {
        if start.access.answered {
            return Err(None);
        }
        // B-36: the backend didn't answer, and an earlier step of this job fetched from it.
        let store = CiStore::of(here.env);
        if let CiStore::Job(path) = &store
            && let Some((saved, folder)) = crate::cmd::build_relay::saved::Saved::load(
                &hushrun_build::store::Store::new(path),
                here.now,
            )
            .filter(|(saved, _)| saved.serves(place().as_ref(), repository))
        {
            store.prepare().map_err(Some)?;
            let caller = caller(Some(&start.access), true);
            return begin_from_saved(here, repository, (saved, folder), Some((path, caller)));
        }
        return Err(Some(NO_ANSWER.to_string()));
    }
    let does = what_a_run_does(
        caller(Some(&start.access), true),
        hushrun_build::model::Mode::On,
        hushrun_build::model::Mode::On,
        Org::Shares(None),
    );
    if let Sharing::Off(off) = does.sharing {
        let hint = match off {
            hushrun_build::share::Off::NoIdToken => {
                " (give the job `permissions: id-token: write`)"
            }
            _ => "",
        };
        return Err(Some(format!("{OFF}: {}{hint}", off.reason())));
    }
    let store = CiStore::of(here.env);
    store.prepare().map_err(Some)?;
    let mut session = match begin_with(here, std::env::current_exe(), Some(store.path())) {
        Ok(session) => session,
        Err(line) => {
            if let CiStore::Run(path) = &store {
                let _ = std::fs::remove_dir_all(path);
            }
            return Err(line);
        }
    };
    let abandon = |session: Session, line: Option<String>| {
        session.finish(here.now);
        if let CiStore::Run(path) = &store {
            let _ = std::fs::remove_dir_all(path);
        }
        Err(line)
    };
    let relay = match start_sharing(&mut session, start, repository, here.env) {
        Ok(relay) => relay,
        Err(line) => return abandon(session, line),
    };
    let wait = crate::cmd::build_relay::Tuning::default().ci_open;
    match relay.wait_open(wait) {
        crate::cmd::build_relay::Opened::Answering => Ok(session),
        opened => {
            let said = relay.end_check(&[]);
            let line = match opened {
                crate::cmd::build_relay::Opened::Unanswered => Some(NO_ANSWER.to_string()),
                crate::cmd::build_relay::Opened::Stopped(_) => said.into_iter().next(),
                // Silent, but for an org whose plan sets its storage by contract (B-25).
                crate::cmd::build_relay::Opened::Off => said
                    .into_iter()
                    .find(|line| line.ends_with(crate::cmd::build_relay::BY_CONTRACT)),
                crate::cmd::build_relay::Opened::Answering => None,
            };
            abandon(session, line)
        }
    }
}

/// What sharing through a backend needs from the command that runs (B-26).
pub(crate) struct ShareStart {
    /// The backend the run's results went to, and how to reach it.
    pub(crate) access: crate::results::BuildAccess,
    /// The workspace's commit, when there is one.
    pub(crate) commit: Option<String>,
    /// The check, by name: its id is a digest of the repository and the name.
    pub(crate) check: String,
    /// B-25: no results lookup asked the backend whether the org shares (the run can't tell
    /// which code it is, as with several scopes and no `--scope`): `open` does, and an org that
    /// doesn't share says nothing.
    pub(crate) open_decides: bool,
}

/// B-25, B-26: when the run's results lookup said the org shares and what the run does allows
/// it, the process's relay begins the check, and the session names its socket. Every reason for
/// no relay but a failure is silent (`Err(None)`); a failure is its line.
fn start_sharing(
    session: &mut Session,
    start: ShareStart,
    repository: Option<&str>,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<std::sync::Arc<crate::cmd::build_relay::Relay>, Option<String>> {
    use hushrun_build::share::{Org, Sharing, what_a_run_does};
    if !(start.access.shares || start.open_decides) || cfg!(not(unix)) {
        return Err(None);
    }
    let current = session.current().clone();
    let caller = caller(Some(&start.access), hushrun_ci::in_ci(&Env::from_process()));
    let does = what_a_run_does(
        caller,
        current.mode,
        current.share_setting,
        Org::Shares(None),
    );
    if matches!(does.sharing, Sharing::Off(_)) {
        return Err(None);
    }
    let check = check_id(repository, &start.check);
    let store = hushrun_build::store::Store::new(&current.store);
    let config = || {
        Some(crate::cmd::build_relay::Config {
            store,
            access: start.access,
            repository: repository.map(str::to_string),
            commit: start.commit.filter(|c| hushrun_core::blocks::is_commit(c)),
            platform: platform(),
            caller,
            compiler: std::env::current_dir().ok().map(|folder| {
                let rustc = env("RUSTC")
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| OsString::from("rustc"));
                (rustc, folder)
            }),
            local: current.mode,
            own: current.share_setting,
            min_free_gb: current.min_free_gb,
            keep_days: current.keep_days,
            tuning: crate::cmd::build_relay::Tuning::default(),
            saved: None,
            open_decides: start.open_decides,
        })
    };
    let Some(relay) = crate::cmd::build_relay::Relay::get_or_start(config) else {
        return Err(Some(
            "build: sharing is off for this run: the relay couldn't start".into(),
        ));
    };
    let share = relay.begin_check(&current.folder, Some(check), env)?;
    if let Err(err) = session.set_share(share) {
        relay.end_check(&[]);
        return Err(Some(format!("build: sharing is off for this run: {err}")));
    }
    Ok(relay)
}

/// A check's id, as the build protocol names it (B-26): a digest of the repository and the
/// check's name, so that the name itself never leaves the machine. The one definition a run and
/// `hushrun build fetch` use.
pub(crate) fn check_id(repository: Option<&str>, check: &str) -> hushrun_core::blocks::Hex32 {
    hushrun_core::blocks::Hex32::sha256(
        format!(
            "hushrun.build-check/v1\n{}\n{check}",
            repository.unwrap_or_default()
        )
        .as_bytes(),
    )
}

/// The target triple this hushrun was built for: the run's platform when the compiler's own host
/// can't be asked. The wrapper's keys are what tell two platforms' blocks apart; this only
/// chooses the manifest.
pub(crate) fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" if cfg!(target_env = "musl") => "unknown-linux-musl",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        other => other,
    };
    format!("{}-{os}", std::env::consts::ARCH)
}

/// Ends a run's session (B-20, B-26): the relay ends the check first (uploads in flight get
/// their wait, `close` goes with the manifest and the counts), then the session's summary,
/// with what the relay said.
pub(crate) fn finish(mut session: Session, now: SystemTime) -> hushrun_build::session::Summary {
    if session.current().share.relay().is_some()
        && let Some(relay) = crate::cmd::build_relay::Relay::running()
    {
        let entries = session.current().entries();
        for line in relay.end_check(&entries) {
            session.add_line(line);
        }
    }
    let store = session.current().store.clone();
    let summary = session.finish(now);
    // B-31: a CI job's store with no job folder to go with goes with the run.
    if hushrun_ci::in_ci(&Env::from_process())
        && CiStore::of(&|name| hushrun_ci::var_os(name)) == CiStore::Run(store.clone())
    {
        let _ = std::fs::remove_dir_all(&store);
    }
    summary
}

/// Whether a toolchain under rustup's home could have sources (`lib/rustlib/src` or
/// `lib/rustlib/rustc-src`), or there is no rustup home to look in (then cargo's `rustc` is
/// something else, and is asked). A few `stat`s.
fn rustup_has_sources(env: &dyn Fn(&str) -> Option<OsString>) -> bool {
    let home = env("RUSTUP_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".rustup"))
        });
    let Some(Ok(toolchains)) = home.map(|home| std::fs::read_dir(home.join("toolchains"))) else {
        return true;
    };
    toolchains.flatten().any(|toolchain| {
        let rustlib = toolchain.path().join("lib").join("rustlib");
        ["src", "rustc-src"]
            .iter()
            .any(|name| std::fs::symlink_metadata(rustlib.join(name)).is_ok())
    })
}

/// The sysroot of the rustc cargo will call in `folder`: `RUSTC`, else `rustc` on `PATH` (the
/// rustup proxy picks the folder's toolchain), as `rustc --print sysroot` says. `None` when it
/// can't be run.
fn rustc_sysroot(folder: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let rustc = env("RUSTC").filter(|value| !value.is_empty());
    // Asking the rustup proxy costs a process and rustup's start (about 100 ms). When rustup's
    // own toolchains are what cargo will call and none has sources, there is nothing to make.
    if rustc.is_none() && !rustup_has_sources(env) {
        return None;
    }
    let rustc = rustc.unwrap_or_else(|| OsString::from("rustc"));
    let out = std::process::Command::new(rustc)
        .args(["--print", "sysroot"])
        .current_dir(folder)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let sysroot = PathBuf::from(text.trim());
    (out.status.success() && sysroot.is_absolute()).then_some(sysroot)
}

/// `time` as the build blocks take it.
pub(crate) fn system_time(time: Time) -> SystemTime {
    u64::try_from(time.millis()).map_or(SystemTime::UNIX_EPOCH, |ms| {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(ms)
    })
}

/// What [`begin`] reads from the process, but the binary's path.
struct Here<'a> {
    settings: &'a Path,
    cache_dir: Option<&'a Path>,
    in_ci: bool,
    repository: Option<&'a str>,
    /// The repository's `build: env:` (B-35).
    repository_env: &'a [String],
    env: &'a dyn Fn(&str) -> Option<OsString>,
    now: SystemTime,
    /// The folder `hushrun run` runs in: the check before COMMAND looks at the target folders
    /// of its crates (B-23).
    folder: Option<PathBuf>,
    /// The sysroot of the compiler cargo will call in a folder, to make the toolchain's view
    /// without sources before COMMAND starts (B-9).
    sysroot: &'a dyn Fn(&Path) -> Option<PathBuf>,
}

impl<'a> Here<'a> {
    fn start(
        &self,
        exe: &'a Path,
        cache_dir: Option<&'a Path>,
        env: &'a dyn Fn(&str) -> Option<OsString>,
    ) -> session::Start<'a> {
        session::Start {
            settings: self.settings,
            cache_dir,
            exe,
            in_ci: self.in_ci,
            repository: self.repository,
            repository_env: self.repository_env,
            env,
            now: self.now,
        }
    }
}

/// Begins the session; in CI, `ci_store` is the job's own store, and nothing else begins one.
fn begin_with(
    here: &Here<'_>,
    exe: std::io::Result<PathBuf>,
    ci_store: Option<&Path>,
) -> Result<Session, Option<String>> {
    let exe = match exe {
        Ok(exe) => exe,
        Err(err) => {
            // Nothing can link to a binary with no path. Asked with no store, `begin` still
            // decides whether reuse is wanted here, and creates nothing: it stops at the store.
            let no_store = |name: &str| (name != STORE_DIR).then(|| (here.env)(name)).flatten();
            let lost = format!("{OFF}: hushrun can't find its own binary: {err}");
            if ci_store.is_some() {
                return Err(Some(lost));
            }
            return match session::begin(&here.start(Path::new(""), None, &no_store)) {
                Err(NotBegun::NoCache) => Err(Some(lost)),
                Ok(session) => {
                    session.finish(here.now);
                    Err(Some(lost))
                }
                Err(not) => said(not),
            };
        }
    };
    let start = here.start(&exe, here.cache_dir, here.env);
    let mut session = match ci_store {
        Some(store) => session::begin_in_ci(&start, store),
        None => session::begin(&start),
    }
    .or_else(said)?;
    // B-23: before COMMAND, the crates whose inputs changed in ways cargo can't see lose their
    // outputs, so that cargo builds them. What it removed is said with the run's summary.
    if let Some(folder) = &here.folder {
        session.check(folder, Some(Instant::now() + CHECK_BUDGET));
        // B-9: the view without sources is made now, before any compile, so wrappers only
        // read it.
        if let Some(sysroot) = (here.sysroot)(folder) {
            session.hide_sources(&sysroot);
        }
    }
    Ok(session)
}

/// What the person reads of a run with no session: settings or a variable that can't be used
/// is worth a line, and so is another compiler wrapper, which [`begin`] says only in an org
/// that shares.
fn said(not: NotBegun) -> Result<Session, Option<String>> {
    match not {
        NotBegun::Unusable(why) => Err(Some(format!("{OFF}: {why}"))),
        NotBegun::WrapperSet => Err(Some(WRAPPER_SET.to_string())),
        NotBegun::Off | NotBegun::Ci | NotBegun::Windows | NotBegun::NoCache => Err(None),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use hushrun_build::model::Mode;
    use hushrun_build::settings;

    fn here<'a>(
        dir: &'a Path,
        settings: &'a Path,
        env: &'a dyn Fn(&str) -> Option<OsString>,
    ) -> Here<'a> {
        Here {
            settings,
            cache_dir: Some(dir),
            in_ci: false,
            repository: None,
            repository_env: &[],
            env,
            now: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000),
            folder: None,
            sysroot: &|_| None,
        }
    }

    // covers: B-17
    #[test]
    fn the_compiler_is_asked_for_its_sysroot_only_where_a_toolchain_may_have_sources() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("rustup");
        let named = home.clone().into_os_string();
        let env = move |name: &str| (name == "RUSTUP_HOME").then(|| named.clone());
        std::fs::create_dir_all(home.join("toolchains/stable-x/lib/rustlib/x/lib")).unwrap();
        assert!(!rustup_has_sources(&env));
        // No rustup home at all: cargo's rustc is something else, and is asked.
        let none = |_: &str| None::<OsString>;
        assert!(rustup_has_sources(&none));
        std::fs::create_dir_all(home.join("toolchains/nightly-x/lib/rustlib/src/rust")).unwrap();
        assert!(rustup_has_sources(&env));
    }

    fn lost() -> std::io::Result<PathBuf> {
        Err(std::io::Error::other("no such process"))
    }

    // covers: B-2, B-19
    #[test]
    fn a_binary_hushrun_cant_find_is_one_line_and_no_session() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("build.json");
        let store = dir.path().join("build");
        let none = |_: &str| None;

        // Off: nothing to say, found or not.
        assert_eq!(
            begin_with(&here(dir.path(), &settings, &none), lost(), None).err(),
            Some(None)
        );

        settings::update(&settings, |s| s.mode = Mode::On).unwrap();
        assert_eq!(
            begin_with(&here(dir.path(), &settings, &none), lost(), None).err(),
            Some(Some(
                "build reuse is off for this run: hushrun can't find its own binary: no such process"
                    .to_string()
            ))
        );
        // Nothing was made for a session that never began, here or in a store a variable names.
        let elsewhere = dir.path().join("elsewhere");
        let named = elsewhere.clone().into_os_string();
        let variable = move |name: &str| (name == STORE_DIR).then(|| named.clone());
        assert!(begin_with(&here(dir.path(), &settings, &variable), lost(), None).is_err());
        assert!(!store.exists());
        assert!(!elsewhere.exists());

        // A wrapper the person set: its own line, which `begin` says only where the org
        // shares. CI: silent, as when the binary is found.
        let set = |name: &str| (name == "RUSTC_WRAPPER").then(|| OsString::from("sccache"));
        assert_eq!(
            begin_with(&here(dir.path(), &settings, &set), lost(), None).err(),
            Some(Some(WRAPPER_SET.to_string()))
        );
        let mut ci = here(dir.path(), &settings, &none);
        ci.in_ci = true;
        assert_eq!(begin_with(&ci, lost(), None).err(), Some(None));
        // A variable that can't be used says so, whatever the binary.
        let nonsense = |name: &str| (name == "HUSHRUN_BUILD").then(|| OsString::from("nonsense"));
        let said = begin_with(&here(dir.path(), &settings, &nonsense), lost(), None)
            .err()
            .flatten()
            .unwrap();
        assert!(
            said.starts_with("build reuse is off for this run: HUSHRUN_BUILD="),
            "{said}"
        );

        // Found, it begins.
        let exe = dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        let session = begin_with(&here(dir.path(), &settings, &none), Ok(exe), None).unwrap();
        assert!(store.join("sessions").is_dir());
        session.finish(SystemTime::UNIX_EPOCH);
    }

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    // covers: B-31
    #[test]
    fn a_github_job_is_named_by_its_repository_run_attempt_job_and_request_token() {
        let job = |run: &str, attempt: &str, request: &str| {
            github_job(&vars(&[
                ("GITHUB_REPOSITORY_ID", "4242"),
                ("GITHUB_RUN_ID", run),
                ("GITHUB_RUN_ATTEMPT", attempt),
                ("GITHUB_JOB", "build"),
                ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", request),
            ]))
        };
        let one = job("100", "1", "request-one").unwrap();
        assert!(one.folder.starts_with("hushrun-build-100-1-"), "{one:?}");
        assert!(!one.folder.contains("request"));
        assert_eq!(job("100", "1", "request-one"), Some(one.clone()));
        for other in [
            job("101", "1", "request-one"),
            job("100", "2", "request-one"),
            job("100", "1", "request-two"),
        ] {
            let other = other.unwrap();
            assert_ne!(other.digest, one.digest);
            assert_ne!(other.folder, one.folder);
        }
        // Without the job's own request token, nothing names it that another job couldn't.
        assert_eq!(github_job(&vars(&[("GITHUB_RUN_ID", "100")])), None);
        // And without one, there is no job store: the run makes its own.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().to_str().unwrap().to_string();
        assert!(matches!(
            CiStore::of(&vars(&[("RUNNER_TEMP", &path)])),
            CiStore::Run(_)
        ));
        assert!(matches!(
            CiStore::of(&vars(&[
                ("RUNNER_TEMP", &path),
                ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "r")
            ])),
            CiStore::Job(_)
        ));
    }

    // covers: B-31
    #[test]
    fn a_job_reads_no_store_it_did_not_make() {
        let temp = tempfile::tempdir().unwrap();
        // Another job's store, and the store from before stores were named by their job.
        let other = temp.path().join("hushrun-build-99-1-0123456789abcdef");
        let legacy = temp.path().join("hushrun-build");
        let unrelated = temp.path().join("cache");
        for dir in [&other, &legacy, &unrelated] {
            std::fs::create_dir_all(dir.join("blocks")).unwrap();
        }
        let own = temp.path().join("hushrun-build-100-1-fedcba9876543210");
        std::fs::create_dir_all(own.join("blocks")).unwrap();
        CiStore::Job(own.clone()).prepare().unwrap();
        assert!(own.join("blocks").is_dir(), "the job's own store stays");
        assert!(!other.exists() && !legacy.exists());
        assert!(unrelated.exists());

        // A run's store found at the start died with another process of the same id.
        let run = temp.path().join("hushrun-build-12345");
        std::fs::create_dir_all(run.join("blocks/x")).unwrap();
        CiStore::Run(run.clone()).prepare().unwrap();
        assert!(run.is_dir());
        assert_eq!(std::fs::read_dir(&run).unwrap().count(), 0);
        // One that can't be removed means no store.
        let stuck = temp.path().join("stuck");
        std::fs::create_dir_all(&stuck).unwrap();
        let run = stuck.join("hushrun-build-12346");
        std::fs::create_dir_all(run.join("blocks")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o555)).unwrap();
        let said = CiStore::Run(run.clone()).prepare();
        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o755)).unwrap();
        let said = said.unwrap_err();
        assert!(
            said.starts_with("build reuse is off for this run: another run's store"),
            "{said}"
        );
    }

    // covers: B-23
    #[test]
    fn a_run_checks_the_crates_of_its_folder_before_command_and_says_what_it_removed() {
        use hushrun_build::unit::{self, Item, Seen};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let settings = root.join("build.json");
        settings::update(&settings, |s| s.mode = Mode::On).unwrap();
        let exe = root.join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        // A crate the wrapper saw compiled: its output, its record, its folder registered.
        let ws = root.join("ws");
        let out = ws.join("target/debug/deps");
        let data = ws.join("app/data");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(ws.join("target/debug/.cargo-lock"), b"").unwrap();
        let rlib = out.join("libapp-1.rlib");
        std::fs::write(&rlib, b"rlib").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let seen = Seen {
            crate_name: "app".into(),
            package: Some("app".into()),
            cwd: ws.clone(),
            outputs: vec![rlib.clone()],
            items: vec![Item::Dir(data.clone())],
            since: SystemTime::now(),
        };
        assert!(unit::settle(&out, "app-1", Some(&seen)));
        hushrun_build::store::Store::new(&root.join("build"))
            .register_out_dir(&out, &ws, SystemTime::now())
            .unwrap();
        // A file added where cargo doesn't look.
        std::fs::write(data.join("2.sql"), b"").unwrap();

        let none = |_: &str| None;
        let mut elsewhere = here(&root, &settings, &none);
        elsewhere.folder = Some(root.join("elsewhere"));
        std::fs::create_dir_all(root.join("elsewhere")).unwrap();
        let session = begin_with(&elsewhere, Ok(exe.clone()), None).unwrap();
        assert!(rlib.exists(), "another folder's crates are not checked");
        assert!(session.finish(SystemTime::UNIX_EPOCH).lines().is_empty());

        let mut in_ws = here(&root, &settings, &none);
        in_ws.folder = Some(ws.join("app"));
        let session = begin_with(&in_ws, Ok(exe), None).unwrap();
        assert!(!rlib.exists());
        assert_eq!(
            session.finish(SystemTime::UNIX_EPOCH).lines(),
            ["build: app is rebuilt: a file was added to data, which cargo doesn't watch"]
        );
    }
}
