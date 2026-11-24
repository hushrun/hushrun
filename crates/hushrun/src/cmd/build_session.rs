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

/// B-25, B-31: why a CI job with no backend to share through keeps no compiled code, in the job's
/// store or anywhere: the one text `hushrun run`, `hushrun build status`, a bake and `hushrun
/// build manifest` say it with ([`ci_without_backend`]).
pub(crate) const CI_NO_BACKEND: &str = "in CI hushrun keeps compiled code only in the job's own store, for a backend that shares it, and nothing names one here (hushrun.yaml's `backend`, or HUSHRUN_BACKEND)";

/// B-25's "CI otherwise" row as far as it is known before anything is asked: in CI (`in_ci`)
/// with no backend named, a run keeps nothing, so a later command of the job finds nothing a run
/// kept. Its reason, else `None` (a backend is named: whether the org shares is its to say).
pub(crate) fn ci_without_backend(in_ci: bool, backend_named: bool) -> Option<&'static str> {
    (in_ci && !backend_named).then_some(CI_NO_BACKEND)
}

/// The variable that names the store instead of the cache directory (B-13).
const STORE_DIR: &str = "HUSHRUN_BUILD_DIR";

/// Starts the session for a command hushrun is about to run, when reuse is on here. With none,
/// the command runs as usual (B-19), and `Err` holds the one line the person should read,
/// without hushrun's prefix: settings or a variable that can't be used, or a binary hushrun
/// can't find. Every other reason for none is silent. `repository_env` is the repository's
/// `build: env:` (B-35), which every compile of the run is keyed by. `place` gives the run's
/// backend, which a saved fetch must be from (B-36), asked only when the store holds one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn begin(
    now: SystemTime,
    repository: Option<&str>,
    repository_env: &[String],
    cc_maps: bool,
    signers: &hushrun_core::policy::BakedTrust,
    check: Option<&str>,
    share: Option<ShareStart>,
    place: &dyn Fn() -> Option<crate::results::BuildPlace>,
) -> Result<Session, Option<String>> {
    // B-2: a compiler wrapper set already passes through. That is worth its line only where the
    // run's results lookup said the org shares: there the person may expect the shared store.
    let org_shares = share.as_ref().is_some_and(|start| start.access.shares);
    let mut session = begin_here(now, repository, repository_env, cc_maps, share, place)
        .map_err(|line| line.filter(|line| line != WRAPPER_SET || org_shares))?;
    let env = |name: &str| hushrun_ci::var_os(name);
    let folder = std::env::current_dir().ok();
    let checkout = folder.as_deref().map(|folder| checkout_root(folder, &env));
    // B-63, B-64: the baked store the run reads, decided once its relay, if any, began. Go's
    // platform, for its part of the store, is the program's when the relay started it, else
    // what `go env` says.
    let go_platform = match session.go() {
        Some(go) => Some(go.platform.clone()),
        None if !session.go_asked() && hushrun_build::baked::named(&env).is_some() => {
            folder.as_deref().and_then(hushrun_build::go::probe::env_of)
        }
        None => None,
    };
    give_lower(
        &mut session,
        repository,
        folder.as_deref(),
        signers,
        go_platform.as_deref(),
    );
    // B-39, B-63: Go's cache program, asked once: as the relay started, in a run that shares;
    // here, in one that doesn't, only where the run reads a baked store (never one its state
    // ignores, such as `/hushrun` in CI with no signer named). A Go module's run that does
    // neither leaves Go as plain go runs it.
    if !session.go_asked() && session.current().lower.is_some() {
        let go = go_program(&mut session, &env);
        session.set_go(go);
    }
    // B-63: the check's manifest stays in the store when the run ends, for `hushrun build bake`.
    if let Some(check) = check {
        session.set_manifest_slot(hushrun_build::manifests::Slot {
            check: Some(check_id(repository, check)),
            target: platform(),
            checkout: checkout
                .as_deref()
                .and_then(|root| root.to_str().map(str::to_string)),
            go: session.go().map(|go| go.platform.clone()),
        });
    }
    // B-77: Xcode's compilation cache, through a service of this run, last: the service reads
    // the session as it now stands (its relay, its baked store).
    if let (Some(folder), Some(root)) = (folder.as_deref(), checkout.as_deref()) {
        let in_ci = crate::gitlab_job::in_ci(&Env::from_process());
        crate::cmd::build_xcode::give(&mut session, folder, root, in_ci, &env);
    }
    let cargo = std::env::current_dir().ok().is_some_and(|folder| {
        folder
            .ancestors()
            .any(|dir| dir.join("Cargo.toml").is_file())
    });
    if session.rustc_left_out() && org_shares && cargo {
        session.add_line(RUST_LEFT_OUT.to_string());
    }
    Ok(session)
}

/// B-75: the session of a build a bake runs in CI on the default branch, because the backend's
/// newest build is of another commit: in the bake's own store `store`, sharing through the
/// process's relay (the bake's), as a CI job's run shares, so what it compiles goes to the
/// repository's trusted scope with the job's own ID token, and what the trusted scope holds is
/// reused. No results lookup, nothing recorded. `Err` is the line that says why there is none.
#[allow(clippy::too_many_arguments)]
pub(crate) fn begin_for_bake(
    now: SystemTime,
    repository: Option<&str>,
    repository_env: &[String],
    cc_maps: bool,
    check: &str,
    store: &Path,
    start: ShareStart,
) -> Result<Session, String> {
    let settings = Credentials::build_settings_path()
        .ok_or_else(|| "there is no folder for hushrun's settings".to_string())?;
    let cache_dir = context::cache_dir();
    let env = |name: &str| hushrun_ci::var_os(name);
    let here = Here {
        settings: &settings,
        cache_dir: cache_dir.as_deref(),
        in_ci: true,
        repository,
        repository_env,
        cc_maps,
        env: &env,
        now,
        folder: std::env::current_dir().ok(),
        sysroot: &|folder| rustc_sysroot(folder, &env),
    };
    let mut session = begin_with(&here, std::env::current_exe(), Some(store))
        .map_err(|line| line.unwrap_or_else(|| OFF.to_string()))?;
    let relay = match start_sharing(&mut session, start, repository, &env) {
        Ok(relay) => relay,
        Err(line) => {
            session.finish(now);
            return Err(line.unwrap_or_else(|| format!("{OFF}: the backend doesn't share")));
        }
    };
    match relay.wait_open(crate::cmd::build_relay::Tuning::default().ci_open) {
        crate::cmd::build_relay::Opened::Answering => {}
        _ => {
            relay.end_check(&[]);
            session.finish(now);
            return Err(NO_ANSWER.to_string());
        }
    }
    session.set_manifest_slot(hushrun_build::manifests::Slot {
        check: Some(check_id(repository, check)),
        target: platform(),
        checkout: None,
        go: session.go().map(|go| go.platform.clone()),
    });
    Ok(session)
}

/// B-72, B-74: a session in `store`, a folder of a bake's own, for a build the bake runs itself
/// (the dependencies-only build, or a measurement): no person's settings, no relay, nothing at
/// `/hushrun` or `HUSHRUN_BUILD_LOWER` read (a caller that wants a baked store underneath gives
/// it, [`give_own_lower`]), Go's cache program in a Go module's folder (`go`), and the toolchain's
/// view without sources made before COMMAND. `Err` is the line that says why there is none.
pub(crate) fn begin_own(
    now: SystemTime,
    repository: Option<&str>,
    repository_env: &[String],
    store: &Path,
    folder: &Path,
    go: bool,
) -> Result<Session, String> {
    let settings = Credentials::build_settings_path()
        .ok_or_else(|| "there is no folder for hushrun's settings".to_string())?;
    let exe = std::env::current_exe()
        .map_err(|err| format!("hushrun can't find its own binary: {err}"))?;
    let env = |name: &str| hushrun_ci::var_os(name);
    let cache_dir = context::cache_dir();
    let in_ci = crate::gitlab_job::in_ci(&Env::from_process());
    let start = session::Start {
        settings: &settings,
        cache_dir: cache_dir.as_deref(),
        exe: &exe,
        in_ci,
        repository,
        repository_env,
        env: &env,
        now,
    };
    let floor = if in_ci {
        session::CI_MIN_FREE_GB
    } else {
        hushrun_build::settings::MIN_FREE_GB
    };
    let mut session = session::begin_isolated(&start, store, floor).map_err(|not| match not {
        NotBegun::Unusable(why) => why,
        NotBegun::WrapperSet => {
            "RUSTC_WRAPPER is set, and hushrun's wrapper doesn't run beside another".to_string()
        }
        NotBegun::Off => "HUSHRUN_BUILD turns build reuse off".to_string(),
        other => format!("build reuse can't run here ({other:?})"),
    })?;
    if let Some(sysroot) = rustc_sysroot(folder, &env) {
        session.hide_sources(&sysroot);
    }
    if go {
        let program = go_program(&mut session, &env);
        session.set_go(program);
    }
    Ok(session)
}

/// B-74: gives a measurement's session the baked store at `lower` underneath, as a sandbox started
/// from its image reads it: on the signature `signers` names when it verifies, else as accepted,
/// since the bake that made it from its own store is the one reading it, and nothing this session
/// builds leaves its own store. The state's words, for the report.
pub(crate) fn give_own_lower(
    session: &mut Session,
    lower: &Path,
    repository: Option<&str>,
    folder: &Path,
    go: Option<&str>,
    signers: &hushrun_core::policy::BakedTrust,
) -> String {
    use hushrun_build::baked;
    let named = lower.as_os_str().to_os_string();
    let env = |name: &str| -> Option<OsString> {
        if name == baked::LOWER {
            Some(named.clone())
        } else if name == baked::LOWER_UNTRUSTED {
            Some("1".into())
        } else {
            hushrun_ci::var_os(name)
        }
    };
    let store = session.current().store.clone();
    let session_folder = session.current().folder.clone();
    let (state, _) = lower_state(
        &env,
        false,
        &store,
        Some(folder),
        repository,
        go,
        baked::Asked::NoBackend,
        signers,
        &session_folder,
    );
    let words = state.words();
    let _ = session.set_lower(&state);
    words
}

/// B-2, B-39, B-50: whether the run's own `RUSTC_WRAPPER` (sccache) is set in a Go module's
/// folder or a CMake project's: hushrun's wrapper stays out, and the session still begins, for
/// Go's cache program and the C and C++ launcher. Elsewhere a wrapper set begins no session, as
/// before.
fn go_beside_another_wrapper(here: &Here<'_>) -> bool {
    (here.env)(session::RUSTC_WRAPPER).is_some_and(|v| !v.is_empty())
        && here.folder.as_deref().is_some_and(|folder| {
            hushrun_build::go::probe::is_module(folder) || cmake_project(folder)
        })
}

/// Whether `folder` or a folder above it, up to its checkout, holds a `CMakeLists.txt`, or
/// `folder` is a CMake build folder: where COMMAND may build C or C++ with CMake (B-50).
fn cmake_project(folder: &Path) -> bool {
    if folder.join("CMakeCache.txt").is_file() {
        return true;
    }
    for dir in folder.ancestors() {
        if dir.join("CMakeLists.txt").is_file() {
            return true;
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    false
}

/// The checkout `folder` is in (B-50): the nearest folder at or above it holding `.git` (a
/// worktree's is a file), in the form the shell named it (`PWD`) when that is the same folder;
/// else `folder` itself.
fn checkout_root(folder: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> PathBuf {
    checkout_of(folder, env)
}

/// [`checkout_root`], for `hushrun build bake`: the checkout a run in `folder` names in its
/// manifest and compares a baked store's paths with (B-63).
pub(crate) fn checkout_of(folder: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> PathBuf {
    let given = env("PWD")
        .map(PathBuf::from)
        .filter(|pwd| {
            pwd.is_absolute()
                && std::fs::canonicalize(pwd).ok() == std::fs::canonicalize(folder).ok()
        })
        .unwrap_or_else(|| folder.to_path_buf());
    given
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map_or(given.clone(), Path::to_path_buf)
}

/// B-63, B-64, B-67: the state of the baked store the run's variables name, as this machine
/// would read it: the one place that gathers what [`hushrun_build::baked::state`] decides from,
/// for the run, `hushrun build status` and `hushrun build bake`. `may_write_trusted` is the run's
/// authority ([`hushrun_build::baked::authority`]); `upper` is the run's own store; `folder` the
/// folder the run is in; `server` what the store's backend answered as the run began (status and
/// bake ask none); `signers` the repository's `build: baked: signers:`, whose signature over the
/// store ([`signed_evidence`]) is evidence, its covered hints listed in `session`. The second
/// value says why a signature there isn't evidence, when it isn't.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_state(
    env: &dyn Fn(&str) -> Option<OsString>,
    may_write_trusted: bool,
    upper: &Path,
    folder: Option<&Path>,
    repository: Option<&str>,
    go: Option<&str>,
    server: hushrun_build::baked::Asked<'_>,
    signers: &hushrun_core::policy::BakedTrust,
    session: &Path,
) -> (hushrun_build::baked::State, Option<String>) {
    use hushrun_build::baked;
    use hushrun_build::model::Kind;
    let named = baked::named(env);
    let Some(named_folder) = named.as_deref() else {
        return (baked::State::Absent, None);
    };
    let (signed, unsigned) = match signed_evidence(env, named_folder, signers, session) {
        Ok(signed) => (signed, None),
        Err(why) => (None, Some(why)),
    };
    // B-63: the compiler's own target (a GNU and a musl hushrun read one store), else hushrun's.
    let target = folder
        .and_then(|folder| compiler_host(folder, env))
        .unwrap_or_else(platform);
    let mut platforms = vec![(Kind::Rustc, target.clone()), (Kind::Cc, target)];
    if let Some(go) = go {
        platforms.push((Kind::Go, go.to_string()));
    }
    // B-76: the folders this run's compiles are keyed by, which a store's bound blocks need.
    let paths = folder.and_then(|folder| baked::BakedPaths::of(env, &checkout_of(folder, env)));
    let state = baked::state(
        named.as_deref(),
        &baked::Here {
            repository,
            platforms: &platforms,
            untrusted_ok: baked::accepts_untrusted(env, may_write_trusted),
            server,
            signed: signed.as_ref(),
            upper,
            paths: paths.as_ref(),
            // B-65: at `/hushrun` because no variable named a store.
            default: env(baked::LOWER).is_none(),
        },
    );
    (state, unsigned)
}

/// B-67: the signature beside the baked store in `folder`, verified against `signers`, as the
/// run's evidence: `Ok(None)` when there is no signature or no signer is named; `Err` says why
/// one there isn't evidence. The hint files it covers are written to `session`'s
/// [`hushrun_build::baked::SIGNED_HINTS`].
pub(crate) fn signed_evidence(
    env: &dyn Fn(&str) -> Option<OsString>,
    folder: &Path,
    signers: &hushrun_core::policy::BakedTrust,
    session: &Path,
) -> Result<Option<hushrun_build::baked::Signed>, String> {
    use crate::cmd::build_relay::bake_image;
    use hushrun_build::baked;
    if signers.signers.is_empty() || !folder.join(baked::SIGNATURE).is_file() {
        return Ok(None);
    }
    let root = bake_image::run_root(env);
    let verified = bake_image::verify_store(folder, &signers.signers, root.as_ref())?;
    // B-67: an old signed store, replayed where no backend says it was replaced, is evidence only
    // from `not_before` on: by its timestamp when signed keylessly, else by the time its signed
    // baked.json says it was baked.
    bake_image::not_before_holds(
        signers.not_before,
        verified.signed_ms,
        i64::try_from(verified.baked_ms).ok(),
    )?;
    let hints = session.join(baked::SIGNED_HINTS);
    std::fs::create_dir_all(session).map_err(|e| format!("{}: {e}", session.display()))?;
    baked::write_signed_hints(&hints, &verified.hints)
        .map_err(|e| format!("{}: {e}", hints.display()))?;
    Ok(Some(baked::Signed {
        signer: verified.signer,
        baked_json: verified.baked_json,
        hints,
        current: false,
    }))
}

/// What `rustc -vV` names as its `host:` in `folder` (`RUSTC`, else `rustc`): the compiler's own
/// target triple. `None` when it can't be asked.
pub(crate) fn compiler_host(
    folder: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Option<String> {
    let rustc = env("RUSTC")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("rustc"));
    let out = std::process::Command::new(rustc)
        .arg("-vV")
        .current_dir(folder)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(|host| host.trim().to_string())
        .filter(|host| hushrun_core::blocks::is_platform(host))
}

/// B-63 how long a run waits, as it begins, for its relay's answer about its baked store: the
/// same as a CI job waits for `open`.
const BAKED_CHECK_WAIT: Duration = Duration::from_secs(5);

/// B-63, B-64: records the baked store the run reads, before COMMAND starts. When the run's
/// relay talks to the backend the store was baked from, for its repository, the relay first
/// checks it (within [`BAKED_CHECK_WAIT`]): what the backend vouches for in the trusted scope,
/// for the very bytes of `baked.json` read here, is the run's evidence, and a newer manifest of
/// one of its checks marks it replaced before anything is reused. A store the run ignores costs a
/// line, never the run. `go` is Go's platform (`GOOS_GOARCH`) in a Go module's run.
fn give_lower(
    session: &mut Session,
    repository: Option<&str>,
    folder: Option<&Path>,
    signers: &hushrun_core::policy::BakedTrust,
    go: Option<&str>,
) {
    use hushrun_build::baked;
    let env = |name: &str| hushrun_ci::var_os(name);
    let Some(named) = baked::named(&env) else {
        return;
    };
    let in_ci = crate::gitlab_job::in_ci(&Env::from_process());
    let store = session.current().store.clone();
    let relay = session
        .current()
        .share
        .relay()
        .cloned()
        .zip(crate::cmd::build_relay::Relay::running());
    let mut vouched = None;
    if let Some((share, relay)) = &relay
        && let Ok((baked, contents)) = baked::Baked::read_bytes(&named)
    {
        let server = baked::Server {
            host: share.by.host.clone(),
            org: share.by.org.clone(),
            repository: share.repository.clone(),
            scope: String::new(),
        };
        if baked::server_verifies(&baked, &server)
            && let Some(checked) = relay.check_baked(&baked, BAKED_CHECK_WAIT)
        {
            vouched = Some(baked::Vouched {
                server: baked::Server {
                    scope: checked.scope,
                    ..server
                },
                baked_json: contents,
                rows: checked.verified,
            });
        }
    }
    // B-63: whether this run may read a store nothing verifies is decided on its authority,
    // asked only of a run that would: what its server lets it write.
    let accepting = env(baked::LOWER_UNTRUSTED).is_some_and(|v| v == "1");
    let may_write_trusted = !accepting
        || baked::authority(
            in_ci,
            relay
                .as_ref()
                .map(|(_, relay)| relay.writes_trusted_within(BAKED_CHECK_WAIT)),
        );
    let session_folder = session.current().folder.clone();
    let (state, unsigned) = lower_state(
        &env,
        may_write_trusted,
        &store,
        folder,
        repository,
        go,
        match (&vouched, &relay) {
            (Some(vouched), _) => baked::Asked::Answered(vouched),
            (None, Some(_)) => baked::Asked::NoAnswer,
            // B-63: no backend to ask: a signature is all the run can know.
            (None, None) => baked::Asked::NoBackend,
        },
        signers,
        &session_folder,
    );
    if let Some(why) = unsigned {
        session.add_line(format!(
            "build: the baked store's signature isn't evidence here: {why}"
        ));
    }
    if let Err(err) = session.set_lower(&state) {
        session.add_line(format!(
            "build: the baked store isn't read in this run: {err}"
        ));
    }
}

/// B-50: gives COMMAND the C and C++ launcher, for the checkout the run is in. A launcher that
/// can't be set up costs a line, never the run.
fn give_cc(session: &mut Session, here: &Here<'_>) {
    let Some(folder) = &here.folder else {
        return;
    };
    let root = checkout_root(folder, here.env);
    if let Err(err) = session.set_cc(&root, here.cc_maps, here.env) {
        session.add_line(format!(
            "build: C and C++ objects aren't reused in this run: {err}"
        ));
    }
}

/// The line of a Go module's run whose Rust crates aren't reused because `RUSTC_WRAPPER` is set
/// already, said in an org that shares, where the folder is a cargo project's too.
const RUST_LEFT_OUT: &str =
    "build: Rust crates aren't reused in this run: another compiler wrapper is set (RUSTC_WRAPPER)";

/// The line of a run that begins no session because `RUSTC_WRAPPER` is set already (B-2), which
/// [`begin`] says only in an org that shares.
const WRAPPER_SET: &str =
    "build reuse is off for this run: another compiler wrapper is set (RUSTC_WRAPPER)";

/// [`begin`], with the wrapper's line whatever the org said.
fn begin_here(
    now: SystemTime,
    repository: Option<&str>,
    repository_env: &[String],
    cc_maps: bool,
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
        in_ci: crate::gitlab_job::in_ci(&Env::from_process()),
        repository,
        repository_env,
        cc_maps,
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
/// other reason: the run's results lookup got no answer of any kind (no server gave an HTTP
/// response, as `HttpLedger::answered_any` says it; so nothing said whether the org shares),
/// and what the run does would share had it said yes. A person who turned
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
    let Some(Ok(relay)) = crate::cmd::build_relay::Relay::get_or_start(config) else {
        return Ok(session);
    };
    match relay.begin_check(&current.folder, None, None, here.env) {
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
    let beside = go_beside_another_wrapper(here);
    let hidden = |name: &str| {
        (name != session::RUSTC_WRAPPER)
            .then(|| (here.env)(name))
            .flatten()
    };
    let env: &dyn Fn(&str) -> Option<OsString> = if beside { &hidden } else { here.env };
    let start = here.start(&exe, here.cache_dir, env);
    let mut session = session::begin_in_sandbox(&start, store).or_else(said)?;
    if beside {
        session.leave_out_rustc();
    }
    give_cc(&mut session, here);
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
    /// No such folder, or no job to name it by (GitLab CI has none): a folder under the system's
    /// temporary folder named by the job's id (`CI_JOB_ID`, else this process's) and a random
    /// part ([`CiStore::of`] picks a new one at each call), which only this run makes and uses.
    /// Its states:
    ///
    /// | The folder | Moved by | To |
    /// |---|---|---|
    /// | absent | [`CiStore::prepare`]: created exclusively (0700), then its lock taken and held for the run ([`RUN_STORE`]) | this run's |
    /// | there already | nothing: never adopted, never removed by this run | no store, one line |
    /// | this run's | the session's end, on every outcome ([`end_run_store`]); a CI command's, when its guard goes ([`CommandStore`], which takes the lock from [`RUN_STORE`], so no session it begins there removes it) | absent |
    /// | a dead run's (its lock free, taken over an hour ago) | any run's [`CiStore::prepare`] in the same folder | absent |
    Run(PathBuf),
}

/// The file in a run's store whose lock the run holds while it lives (B-31).
const RUN_LOCK: &str = "ci-run.lock";

/// A store whose lock is free is a dead run's only once the lock was made this long ago: a run
/// takes it at once, so a younger one may be a run between making the folder and locking it.
const DEAD_AFTER: Duration = Duration::from_secs(3600);

/// The run store this process made and the lock it holds on it (B-31): one per process, as the
/// session is.
static RUN_STORE: std::sync::Mutex<Option<(PathBuf, std::fs::File)>> = std::sync::Mutex::new(None);

/// B-31: the end of a run's own store, on every outcome of its session: the lock goes, then the
/// folder. A path this process didn't make is left alone.
fn end_run_store(path: &Path) {
    let mut held = RUN_STORE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if held.as_ref().is_some_and(|(store, _)| store == path) {
        *held = None;
        let _ = std::fs::remove_dir_all(path);
    }
}

/// The system's temporary folder: `TMPDIR` when it is an absolute path, else the platform's.
fn temp_dir(env: &dyn Fn(&str) -> Option<OsString>) -> PathBuf {
    env("TMPDIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
}

impl CiStore {
    fn of(env: &dyn Fn(&str) -> Option<OsString>) -> CiStore {
        match (env("RUNNER_TEMP").map(PathBuf::from), github_job(env)) {
            (Some(temp), Some(job)) if temp.is_absolute() => CiStore::Job(temp.join(job.folder)),
            _ => {
                let job: String = env("CI_JOB_ID")
                    .map(|id| id.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .chars()
                    .filter(char::is_ascii_digit)
                    .take(20)
                    .collect();
                let job = if job.is_empty() {
                    std::process::id().to_string()
                } else {
                    job
                };
                let mut random = [0u8; 8];
                if getrandom::fill(&mut random).is_err() {
                    let digest = hushrun_build::origin::fingerprint(&format!(
                        "{:?}\n{}",
                        SystemTime::now(),
                        std::process::id()
                    ));
                    random.copy_from_slice(&digest.as_bytes()[..8]);
                }
                CiStore::Run(
                    temp_dir(env).join(format!("{CI_STORE}-{job}-{}", hex::encode(random))),
                )
            }
        }
    }

    fn path(&self) -> &Path {
        match self {
            CiStore::Job(path) | CiStore::Run(path) => path,
        }
    }

    /// B-31: makes sure the store is this job's before anything in it is read. Beside a job's
    /// store, every other store folder in the job's temporary folder is another job's (a
    /// runner that kept it): it is removed, and never read. A run's store is made by this run,
    /// exclusively, and its lock held until the session ends; a folder already at its path is
    /// never adopted, and the run has no store. Dead runs' stores beside it go first
    /// ([`sweep_dead_runs`]). `Err` is the line to say.
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
                if let (Some(parent), Some(own)) = (path.parent(), path.file_name()) {
                    sweep_dead_runs(parent, own, SystemTime::now());
                }
                // The guard is in the write: only a folder this call creates is the run's.
                let mut folder = std::fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt as _;
                    folder.mode(0o700);
                }
                folder.create(path).map_err(|err| {
                    format!(
                        "{OFF}: the run's store at {} can't be made, and a run never takes a folder it didn't make: {err}",
                        path.display()
                    )
                })?;
                let lock = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path.join(RUN_LOCK))
                    .and_then(|file| file.try_lock().map(|()| file).map_err(io_error));
                match lock {
                    Ok(file) => {
                        *RUN_STORE
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            Some((path.clone(), file));
                        Ok(())
                    }
                    Err(err) => {
                        let _ = std::fs::remove_dir_all(path);
                        Err(format!(
                            "{OFF}: the run's store at {} can't be locked: {err}",
                            path.display()
                        ))
                    }
                }
            }
        }
    }
}

fn io_error(err: std::fs::TryLockError) -> std::io::Error {
    match err {
        std::fs::TryLockError::Error(err) => err,
        std::fs::TryLockError::WouldBlock => std::io::Error::from(std::io::ErrorKind::WouldBlock),
    }
}

/// B-31: removes the run stores in `parent` that a dead run left (a crash, a kill): this
/// user's, other than `own`, whose lock file was made over an hour before `now` and whose lock
/// nobody holds. A store whose lock is held is a live run's, whatever its age; one with no lock
/// file is left alone.
fn sweep_dead_runs(parent: &Path, own: &std::ffi::OsStr, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == own || !name.to_string_lossy().starts_with(&format!("{CI_STORE}-")) {
            continue;
        }
        let dir = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&dir) else {
            continue;
        };
        if !meta.is_dir() || !owned_here(&meta) {
            continue;
        }
        let lock = dir.join(RUN_LOCK);
        let old = std::fs::symlink_metadata(&lock)
            .ok()
            .filter(|meta| meta.is_file())
            .and_then(|meta| meta.modified().ok())
            .is_some_and(|made| made + DEAD_AFTER < now);
        let free = old && std::fs::File::open(&lock).is_ok_and(|file| file.try_lock().is_ok());
        if free {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(unix)]
fn owned_here(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    meta.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn owned_here(_: &std::fs::Metadata) -> bool {
    true
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

/// B-31: the store a CI job's command other than `hushrun run` uses (a bake, `hushrun build
/// manifest`, `hushrun build status`): the job's own store under `RUNNER_TEMP` where the job has
/// one, else a store of this command's own, made as a run's is ([`CiStore::Run`]: GitLab CI has
/// no `RUNNER_TEMP`) and removed when the guard goes, on every outcome. `None` when neither can be
/// made.
pub(crate) fn ci_command_store(env: &dyn Fn(&str) -> Option<OsString>) -> Option<CommandStore> {
    match CiStore::of(env) {
        CiStore::Job(path) => {
            CiStore::Job(path.clone()).prepare().ok()?;
            Some(CommandStore { path, run: None })
        }
        CiStore::Run(path) => {
            CiStore::Run(path.clone()).prepare().ok()?;
            // The guard holds the store's lock from here on, not the process's run entry: a
            // session the command begins in it (a bake's build of what's missing, B-75) ends
            // without removing it ([`end_run_store`] leaves a path it doesn't hold alone), and
            // the store goes once, with the guard.
            let lock = {
                let mut held = RUN_STORE
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match held.take() {
                    Some((store, lock)) if store == path => Some(lock),
                    other => {
                        *held = other;
                        None
                    }
                }
            };
            Some(CommandStore { path, run: lock })
        }
    }
}

/// A CI command's store ([`ci_command_store`]): one of the command's own (with the lock it
/// holds while it lives) goes when this does, on every outcome.
pub(crate) struct CommandStore {
    pub(crate) path: PathBuf,
    run: Option<std::fs::File>,
}

impl CommandStore {
    /// Whether the store is the command's own, made for it and gone with it (no job folder to
    /// share, as in GitLab CI): it holds no other command's runs.
    pub(crate) fn own(&self) -> bool {
        self.run.is_some()
    }
}

impl Drop for CommandStore {
    fn drop(&mut self) {
        // The store goes while its lock is still held, so nothing that waits on the lock finds a
        // half-removed store; the lock goes after.
        if let Some(lock) = self.run.take() {
            let _ = std::fs::remove_dir_all(&self.path);
            drop(lock);
        }
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
            id_token: matches!(
                credential,
                Some(
                    BuildCredential::GithubOidc { .. }
                        | BuildCredential::Gitlab(crate::results::GitlabCi::Usable { .. })
                )
            ),
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
        // B-25, B-31: with no backend to share through, the job keeps nothing; said in one line
        // when the job asked for reuse (HUSHRUN_BUILD on or observe), and otherwise not at all.
        let asked = hushrun_build::settings::Overrides::from(here.env)
            .ok()
            .and_then(|o| o.mode)
            .is_some_and(|mode| mode != hushrun_build::model::Mode::Off);
        return Err(ci_without_backend(here.in_ci, place().is_some())
            .filter(|_| asked)
            .map(|why| format!("{OFF}: {why}")));
    };
    // B-48: a GitLab job's HUSHRUN_ID_TOKEN that can't be sent, said whatever the org does: the
    // job asked to share by declaring it.
    if let Some(crate::results::BuildCredential::Gitlab(crate::results::GitlabCi::Unusable(why))) =
        &start.access.credential
    {
        return Err(Some(format!("{OFF}: {why}")));
    }
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
            hushrun_build::share::Off::NoIdToken => id_token_hint(Some(&start.access)),
            _ => String::new(),
        };
        return Err(Some(format!("{OFF}: {}{hint}", off.reason())));
    }
    let store = CiStore::of(here.env);
    store.prepare().map_err(Some)?;
    let mut session = match begin_with(here, std::env::current_exe(), Some(store.path())) {
        Ok(session) => session,
        Err(line) => {
            end_run_store(store.path());
            return Err(line);
        }
    };
    let abandon = |session: Session, line: Option<String>| {
        session.finish(here.now);
        end_run_store(store.path());
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

/// B-49: whether a stop signal must end this run gently: its session shares through a relay
/// whose credential is a GitLab job's, whose trusted uploads count only once the run closes.
/// GitLab cancels and times out a job with SIGTERM, then kills it after a grace period.
pub(crate) fn closes_on_stop(session: Option<&Session>) -> bool {
    session.is_some_and(|session| session.current().share.relay().is_some())
        && crate::cmd::build_relay::Relay::running().is_some_and(|relay| relay.is_gitlab_job())
}

/// Runs `command` to its end. With `gently` ([`closes_on_stop`]), a stop signal (SIGTERM,
/// SIGINT, SIGHUP) no longer ends hushrun at once: it goes to COMMAND, once, the relay's end is
/// made short (`build_relay::stopping`: no upload waited for, `close` within 2 seconds), and the
/// run ends as COMMAND does, its session finished and its store removed. Without, as
/// `Command::status`.
pub(crate) fn run_command(
    command: &mut std::process::Command,
    gently: bool,
) -> std::io::Result<std::process::ExitStatus> {
    if !gently || cfg!(not(unix)) {
        return command.status();
    }
    let held = crate::isolated::Held::hold();
    let mut child = command.spawn()?;
    let mut forwarded = false;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if !forwarded && let Some(signal) = held.signal() {
            forwarded = true;
            crate::cmd::build_relay::stopping();
            #[cfg(unix)]
            if let (Some(pid), Some(signal)) = (
                i32::try_from(child.id())
                    .ok()
                    .and_then(rustix::process::Pid::from_raw),
                rustix::process::Signal::from_named_raw(signal),
            ) {
                let _ = rustix::process::kill_process(pid, signal);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What a line says after why a CI job shares nothing for want of an ID token (B-31, B-48): how
/// to give the job one, in GitHub Actions or in GitLab CI, or why the one it has can't be sent.
pub(crate) fn id_token_hint(access: Option<&crate::results::BuildAccess>) -> String {
    use crate::results::{BuildCredential, GitlabCi};
    match access.and_then(|access| access.credential.as_ref()) {
        Some(BuildCredential::Gitlab(GitlabCi::Absent { declare })) => format!(" ({declare})"),
        Some(BuildCredential::Gitlab(GitlabCi::Unusable(why))) => format!(": {why}"),
        _ => " (give the job `permissions: id-token: write`)".to_string(),
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
    let caller = caller(
        Some(&start.access),
        crate::gitlab_job::in_ci(&Env::from_process()),
    );
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
    let relay = match crate::cmd::build_relay::Relay::get_or_start(config) {
        Some(Ok(relay)) => relay,
        Some(Err(why)) => {
            return Err(Some(format!(
                "build: sharing is off for this run: the relay couldn't start: {why}"
            )));
        }
        None => {
            return Err(Some(
                "build: sharing is off for this run: the relay couldn't start".into(),
            ));
        }
    };
    // B-36: a Go module's COMMAND gets Go's cache program, and the check asks for its Go
    // manifest apart.
    // B-39: asked once the relay is starting, as it always was in a run that shares.
    if !session.go_asked() {
        let go = go_program(session, env);
        session.set_go(go);
    }
    let go = session.go().map(|go| go.platform.clone());
    let share = relay.begin_check(&current.folder, Some(check), go, env)?;
    if let Err(err) = session.set_share(share) {
        relay.end_check(&[]);
        return Err(Some(format!("build: sharing is off for this run: {err}")));
    }
    Ok(relay)
}

/// B-39: Go's cache program for COMMAND, when the folder COMMAND runs in is a Go module's whose
/// `go` is a version the program knows and has no cache program of the person's: what
/// `hushrun_build::go::probe` decides, asked here once. A path Go can't be given is said in one
/// line; every other reason for none is silent.
fn go_program(
    session: &mut Session,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Option<hushrun_build::go::probe::Go> {
    use hushrun_build::go::probe::{self, NotSet};
    let folder = std::env::current_dir().ok()?;
    let link = session.program_link().ok()?;
    match probe::probe(&folder, &link, env) {
        Ok(go) => Some(go),
        Err(NotSet::Unquotable(path)) => {
            session.add_line(format!(
                "build: Go packages aren't reused in this run: the store's path holds a space and both kinds of quote, which Go can't be given ({})",
                path.display()
            ));
            None
        }
        Err(why) => {
            tracing::debug!("no Go cache program: {why:?}");
            None
        }
    }
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
pub(crate) fn finish(session: Session, now: SystemTime) -> hushrun_build::session::Summary {
    finish_as(session, now, crate::cmd::build_relay::Ending::Usual)
}

/// [`finish`], the relay ending the check as `ending` says (B-75: a bake's build closes only a
/// whole build).
pub(crate) fn finish_as(
    mut session: Session,
    now: SystemTime,
    ending: crate::cmd::build_relay::Ending,
) -> hushrun_build::session::Summary {
    // B-84: the Xcode service first, so the check's `close` holds what it stored.
    crate::cmd::build_xcode::end(&mut session);
    if session.current().share.relay().is_some()
        && let Some(relay) = crate::cmd::build_relay::Relay::running()
    {
        let entries = session.current().entries();
        for line in relay.end_check_as(&entries, ending) {
            session.add_line(line);
        }
    }
    let store = session.current().store.clone();
    let summary = session.finish(now);
    // B-31: a CI job's store with no job folder to go with goes with the run.
    end_run_store(&store);
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
    /// The repository's `build: cc: maps:` (B-55).
    cc_maps: bool,
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
    fn start<'b>(
        &'b self,
        exe: &'b Path,
        cache_dir: Option<&'b Path>,
        env: &'b dyn Fn(&str) -> Option<OsString>,
    ) -> session::Start<'b> {
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
    let beside = go_beside_another_wrapper(here);
    let hidden = |name: &str| {
        (name != session::RUSTC_WRAPPER)
            .then(|| (here.env)(name))
            .flatten()
    };
    let env: &dyn Fn(&str) -> Option<OsString> = if beside { &hidden } else { here.env };
    let start = here.start(&exe, here.cache_dir, env);
    let mut session = match ci_store {
        Some(store) => session::begin_in_ci(&start, store),
        None => session::begin(&start),
    }
    .or_else(said)?;
    if beside {
        session.leave_out_rustc();
    }
    give_cc(&mut session, here);
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
            cc_maps: true,
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

        // A folder found where a run's store would be is another run's: never adopted, never
        // removed, and the run has no store.
        let run = temp.path().join("hushrun-build-12345-0123456789abcdef");
        std::fs::create_dir_all(run.join("blocks/x")).unwrap();
        let said = CiStore::Run(run.clone()).prepare().unwrap_err();
        assert!(
            said.starts_with("build reuse is off for this run: the run's store at"),
            "{said}"
        );
        assert!(run.join("blocks/x").is_dir());
        assert!(!run.join(RUN_LOCK).exists());
    }

    /// The tests that make run stores, one at a time: a process holds one (`RUN_STORE`).
    static RUN_STORES: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // covers: B-31
    #[test]
    fn two_runs_of_one_process_id_never_remove_or_read_each_others_store() {
        let _one = RUN_STORES.lock().unwrap_or_else(|p| p.into_inner());
        // A shared temporary folder (a shell runner, a Docker volume): two jobs whose hushrun has
        // the same process id, as in two containers.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().to_str().unwrap().to_string();
        let env = vars(&[
            ("TMPDIR", &path),
            ("GITLAB_CI", "true"),
            ("CI_JOB_ID", "77"),
        ]);
        let first = CiStore::of(&env);
        first.prepare().unwrap();
        std::fs::write(first.path().join("left-by-the-first"), b"").unwrap();
        let second = CiStore::of(&env);
        let prepared = second.prepare();
        let kept = first.path().join("left-by-the-first").exists();
        let apart = first.path() != second.path();
        for store in [&first, &second] {
            let _ = std::fs::remove_dir_all(store.path());
        }
        prepared.unwrap();
        assert!(kept, "the second run removed the first's store");
        assert!(apart, "both runs use {}", first.path().display());
    }

    // covers: B-31
    #[test]
    fn a_run_s_store_goes_with_its_session_and_a_dead_run_s_with_the_next_run() {
        let _one = RUN_STORES.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().to_str().unwrap().to_string();
        let env = vars(&[("TMPDIR", &path), ("CI_JOB_ID", "88")]);
        let store = CiStore::of(&env);
        let name = store
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with("hushrun-build-88-"), "{name}");
        assert_eq!(name.len(), "hushrun-build-88-".len() + 16, "{name}");
        store.prepare().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        // Its lock is held while it lives: another run's sweep leaves it, however old.
        let lock = store.path().join(RUN_LOCK);
        let long_ago = SystemTime::now() - Duration::from_secs(7200);
        std::fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        // A dead run's store, whose lock nobody holds, made over an hour ago; a young one; one
        // with no lock file; and something else.
        let dead = temp.path().join("hushrun-build-87-00000000000000aa");
        let young = temp.path().join("hushrun-build-86-00000000000000bb");
        let unlocked = temp.path().join("hushrun-build-85-00000000000000cc");
        let other = temp.path().join("not-a-store");
        for dir in [&dead, &young, &unlocked, &other] {
            std::fs::create_dir_all(dir.join("blocks")).unwrap();
        }
        for (dir, made) in [(&dead, long_ago), (&young, SystemTime::now())] {
            std::fs::File::create(dir.join(RUN_LOCK))
                .unwrap()
                .set_modified(made)
                .unwrap();
        }
        let next = CiStore::of(&env);
        next.prepare().unwrap();
        assert!(store.path().is_dir(), "a live run's store stays");
        assert!(!dead.exists(), "a dead run's store goes");
        for kept in [&young, &unlocked, &other] {
            assert!(kept.exists(), "{}", kept.display());
        }
        // The first run's session ends: its store goes, on every outcome. A path this process
        // didn't make (the next run's lock is the one held now) is left alone.
        end_run_store(store.path());
        assert!(
            store.path().exists(),
            "only the store this process holds goes"
        );
        end_run_store(next.path());
        assert!(!next.path().exists());
        let _ = std::fs::remove_dir_all(store.path());
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
