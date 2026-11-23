//! `hushrun build` (B-1, B-21): the mode of build blocks, one per user, and the store that keeps
//! them. The settings file is the mode's only home ([`hushrun_build::settings`]): this command
//! changes it under the file's lock, and shows what a `hushrun run` from this shell would see.
//! It needs no repository and puts no hook anywhere.

use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::anyhow;
use serde_json::{Value, json};

use hushrun_build::model::{Mode, ProgramsSetting};
use hushrun_build::session;
use hushrun_build::settings::{self, Overrides, SettingsError};
use hushrun_build::store::{Status, Store};

use crate::cli::{
    BakeFrom, BuildArgs, BuildBakeArgs, BuildCommand, BuildManifestArgs, GlobalOpts, ShareWord,
};
use crate::context;
use crate::credentials::Credentials;
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};

pub fn run(global: &GlobalOpts, args: &BuildArgs) -> CmdResult {
    let place = Place::read()?;
    match args.command.clone().unwrap_or(BuildCommand::Status) {
        BuildCommand::On => set(global, &place, Mode::On),
        BuildCommand::Observe => set(global, &place, Mode::Observe),
        BuildCommand::Off => set(global, &place, Mode::Off),
        BuildCommand::Status => status(global, &place),
        BuildCommand::Clean => clean(global, &place),
        BuildCommand::Fetch(fetch_args) => fetch(global, &place, &fetch_args.checks),
        BuildCommand::Bake(bake_args) => bake(global, &place, &bake_args),
        BuildCommand::Manifest(manifest_args) => manifest(global, &place, &manifest_args),
        BuildCommand::Share(share) => set_share(
            global,
            &place,
            match share.mode {
                ShareWord::On => Mode::On,
                ShareWord::Observe => Mode::Observe,
                ShareWord::Off => Mode::Off,
            },
        ),
    }
}

/// `hushrun build fetch [CHECK]...` (B-36): for a sandbox's setup phase, which has the network
/// and a token that later phases don't. The relay sends `open` for this repository and platform,
/// once for each check named (by its id, as `hushrun run` names it), or once naming none, which
/// asks for the repository's newest manifest; downloads what the manifests name that the store
/// lacks; and saves the sealed packs, the sealed hints, the policy and the rows into the store's
/// `prefetched/`, good for 24 hours. Nothing is uploaded or closed, and no token or link is
/// written.
fn fetch(global: &GlobalOpts, place: &Place, checks: &[String]) -> CmdResult {
    use crate::context::{Content, Source};
    let failed = |why: String| Failure::new(Exit::Unexpected, anyhow!("build fetch: {why}"));
    let in_ci = hushrun_ci::in_ci(&hushrun_ci::Env::from_process());
    // B-31: in CI the fetch goes into the job's own store, which a later step of the job reads,
    // never the cache directory, which outlives the job.
    let store = if in_ci {
        crate::cmd::build_session::ci_job_store(&|name| hushrun_ci::var_os(name)).ok_or_else(
            || {
                failed(
                    "in CI a fetch is kept in the job's own store, under RUNNER_TEMP, which a GitHub Actions job with an ID token has (`permissions: id-token: write`)"
                        .into(),
                )
            },
        )?
    } else {
        place
            .store
            .clone()
            .ok_or_else(|| failed("hushrun has no cache directory to keep crates in".into()))?
    };
    let content = Content::open(global, Source::Repo(hushrun_vcs::Snapshot::Worktree))
        .map_err(|f| f.error)
        .map_err(|err| failed(format!("{err:#}")))?;
    let repository = hushrun_ci::context(&hushrun_ci::Env::from_process())
        .repository
        .or_else(|| crate::cmd::wrap::repository_of(&content));
    let root = std::env::current_dir().ok();
    let access = match crate::results::Backend::select(root.as_deref()) {
        Ok(crate::results::Selection::On(backend)) => backend.build_access(),
        _ => None,
    }
    .filter(|access| access.sends())
    // An agent's token is read from `HUSHRUN_AGENT` (the string `hushrun token create --agent`
    // prints, which the docs name) and, as an `hra_` token, from `HUSHRUN_TOKEN`.
    .ok_or_else(|| {
        failed(
            "no backend is signed in to. `hushrun build fetch` takes an agent token in HUSHRUN_AGENT (in a sandbox, set HUSHRUN_AGENT for this phase to the string `hushrun token create --agent` printed), an agent or a personal token in HUSHRUN_TOKEN, a person's sign-in (`hushrun login`), or in GitHub Actions the job's ID token"
                .into(),
        )
    })?;
    let config = crate::cmd::build_relay::Config {
        store: Store::new(&store),
        caller: crate::cmd::build_session::caller(Some(&access), in_ci),
        access,
        repository,
        commit: None,
        platform: crate::cmd::build_session::platform(),
        compiler: std::env::current_dir()
            .ok()
            .map(|folder| (std::ffi::OsString::from("rustc"), folder)),
        local: Mode::On,
        own: Mode::On,
        min_free_gb: if in_ci {
            session::CI_MIN_FREE_GB
        } else {
            settings::MIN_FREE_GB
        },
        keep_days: hushrun_build::KEEP_DAYS,
        tuning: crate::cmd::build_relay::Tuning::default(),
        saved: None,
        open_decides: false,
    };
    let checks: Vec<Option<hushrun_core::blocks::Hex32>> = if checks.is_empty() {
        vec![None]
    } else {
        checks
            .iter()
            .map(|name| {
                Some(crate::cmd::build_session::check_id(
                    config.repository.as_deref(),
                    name,
                ))
            })
            .collect()
    };
    let relay = std::sync::Arc::new(crate::cmd::build_relay::Relay::start(config).map_err(failed)?);
    let fetched = relay.fetch_for_later(&checks);
    // What the relay said on the way, such as a store's host it couldn't reach (B-26).
    if global.quiet == 0 {
        for line in relay.take_lines() {
            eprintln!("hushrun: {line}");
        }
    }
    let fetched = fetched.map_err(failed)?;
    if global.format == Format::Json {
        output::print_json(&json!({
            "blocks": fetched.blocks,
            "bytes": fetched.bytes,
            "hints": fetched.hints,
            "store": store,
            "good_for_hours": 24,
        }))?;
        return Ok(Exit::Ok);
    }
    if global.quiet == 0 {
        output::print(&format!(
            "Fetched {} compiled {} ({}) and {} {} for later runs here, good for 24 hours. No token was kept.\n",
            fetched.blocks,
            if fetched.blocks == 1 {
                "crate"
            } else {
                "crates"
            },
            size(fetched.bytes),
            fetched.hints,
            if fetched.hints == 1 { "hint" } else { "hints" },
        ))?;
    }
    Ok(Exit::Ok)
}

/// `hushrun build status --format json`'s `baked`: the state of the baked store named, its word
/// and what it means.
fn lower_json(state: &Option<hushrun_build::baked::State>) -> Value {
    let Some(state) = state else {
        return Value::Null;
    };
    let lower = state.lower();
    json!({
        "state": state.word(),
        "words": state.words(),
        "folder": lower.map(|l| l.folder.display().to_string()),
        "manifest": lower.map(|l| l.manifest.to_string()),
        "checks": lower.map(|l| l.checks.iter().map(|c| c.map(|c| c.to_string())).collect::<Vec<_>>()),
        "claim": lower.map(|l| l.claim),
        "evidence": lower.map(|l| l.evidence.word()),
        "parts": lower.map(|l| l.parts.iter().map(|p| json!({"kind": p.kind, "platform": p.platform})).collect::<Vec<_>>()),
        "notes": lower.map(|l| l.notes.clone()),
    })
}

/// The upper store a run in this shell would use: in CI the job's own (B-31), else the one
/// `HUSHRUN_BUILD_DIR` or the cache directory names.
/// The store a bake, `build manifest` or `build status` reads: in CI the job's, or one of the
/// command's own that goes with the guard (B-31); elsewhere this machine's.
fn run_store(
    place: &Place,
    in_ci: bool,
) -> Option<(PathBuf, Option<crate::cmd::build_session::CommandStore>)> {
    if in_ci {
        let held = crate::cmd::build_session::ci_command_store(&|name| hushrun_ci::var_os(name))?;
        Some((held.path.clone(), Some(held)))
    } else {
        place.store.clone().map(|path| (path, None))
    }
}

/// B-31: why `--from store` reads nothing in a CI job whose commands each get a store of their
/// own (no job folder to share, as in GitLab CI): no run's blocks are in this command's.
fn own_store_refusal(held: Option<&crate::cmd::build_session::CommandStore>) -> Option<String> {
    held.filter(|held| held.own()).map(|_| {
        "in this CI job each hushrun command has a store of its own that goes with it (there is no job folder, such as GitHub's RUNNER_TEMP, to share), so no run's blocks are in this one: `--from backend` reads what the job's runs shared".to_string()
    })
}

/// What a bake bakes, the one decision the refusal of a command's own store and the bake itself
/// read: with `--deps-only`, the store its dependencies-only build fills (B-72), whatever
/// `--from` says (it takes no `--from backend`); else `--from`'s, this machine's (or the CI
/// command's) store by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BakeSource {
    Deps,
    Store,
    Backend,
}

impl BakeSource {
    fn of(args: &BuildBakeArgs) -> BakeSource {
        match args.from {
            BakeFrom::Backend => BakeSource::Backend,
            BakeFrom::Store if args.deps_only => BakeSource::Deps,
            BakeFrom::Store => BakeSource::Store,
        }
    }
}

/// A folder removed when it goes out of scope, however the command ends.
struct RemovedAtEnd(PathBuf);

impl Drop for RemovedAtEnd {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `hushrun build bake --out DIR` (B-63): a self-contained store of one repository's compiled
/// crates, Go packages and C and C++ objects for this platform, from this machine's store (no
/// network, untrusted) or from the org's backend (as `fetch` reads it). Nothing but sealed packs,
/// sealed hints and `baked.json` goes in. A partial store (blocks the manifests named that
/// couldn't be had) is written and said; it exits 11 when others start from it ([`Exit::Partial`]).
fn bake(global: &GlobalOpts, place: &Place, args: &BuildBakeArgs) -> CmdResult {
    use crate::cmd::build_relay::baked::{self as write, Facts};
    use crate::context::{Content, Source};
    use hushrun_build::baked;
    let failed = |why: String| Failure::new(Exit::Unexpected, anyhow!("build bake: {why}"));
    let refused = |why: String| Failure::new(Exit::Usage, anyhow!("build bake: {why}"));
    let in_ci = crate::gitlab_job::in_ci(&hushrun_ci::Env::from_process());
    let env = |name: &str| hushrun_ci::var_os(name);
    let folder = std::env::current_dir().map_err(|err| failed(err.to_string()))?;
    let absolute = |path: &std::path::Path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            folder.join(path)
        }
    };
    let content = Content::open(global, Source::Repo(hushrun_vcs::Snapshot::Worktree))
        .map_err(|f| f.error)
        .map_err(|err| failed(format!("{err:#}")))?;
    let repository = hushrun_ci::context(&hushrun_ci::Env::from_process())
        .repository
        .or_else(|| crate::cmd::wrap::repository_of(&content));
    let named = named_checks(repository.as_deref(), &args.checks);
    let checkout = crate::cmd::build_session::checkout_of(&folder, &env);
    let checkout_text = checkout.to_str().map(str::to_string);
    let go = hushrun_build::go::probe::env_of(&folder);
    let (rustc_host, mut toolchains) = toolchains(&folder);
    let cc_platform = cc_target(&folder);
    if let Some(go) = go_version(&folder) {
        toolchains.push(go);
    }
    // B-72, B-74: what --deps-only and --report build, decided before anything is baked.
    let lockfile = crate::cmd::build_relay::bake_image::ThirdParty::read_near(&folder, &checkout);
    let go_module = go.is_some() && hushrun_build::go::probe::is_module(&folder);
    let cargo_here = folder
        .ancestors()
        .take_while(|dir| dir.starts_with(&checkout))
        .any(|dir| dir.join("Cargo.toml").is_file());
    let own_crates = deps_build_plan(args, cargo_here, go_module, lockfile.as_ref())?;
    // B-71: the archive and the disk image, each to a file or (one of them) to stdout.
    let archives = archive_targets(args, &absolute)?;
    let to_stdout = archives
        .iter()
        .any(|(_, dest)| *dest == crate::cmd::build_relay::bake_archive::Dest::Stdout);
    // B-67, B-71: what signs what this bake publishes, decided before it bakes: an image, an
    // archive or a disk image carries the folder's signature, made once.
    let publishes = args.oci.is_some() || args.oci_dir.is_some() || !archives.is_empty();
    // One decision, with the same `in_ci` the tags and the lines below read (B-66).
    let signing = if publishes && !args.no_sign {
        match crate::app::App::new(global)?.bake_signing(args.signing_key.as_deref(), in_ci)? {
            crate::signing::BakeSigning::Signed(signing) => Some(signing),
            crate::signing::BakeSigning::Unsigned(why) => {
                if global.quiet == 0 {
                    eprintln!("hushrun: the store is published unsigned: {why}");
                }
                None
            }
        }
    } else {
        None
    };
    let facts = Facts {
        repository: repository.clone(),
        checks: backend_checks(repository.as_deref(), &named, &content),
        toolchains,
        rustc_host: rustc_host.clone(),
        go_platform: go.clone(),
        cc_platform,
        trusted_only: args.trusted,
        third_party: lockfile.clone(),
        now: baked_at(signing.is_some(), &env),
        // B-76: CI's folders, recorded only where CI bakes its trusted scope (CI's paths name
        // nobody; a person's bake records none).
        paths: (in_ci && args.from == BakeFrom::Backend && args.trusted)
            .then(|| built_paths(&content, &args.checks, &checkout, &env))
            .flatten(),
    };
    let target = crate::cmd::build_session::platform();
    let target_triple = target.clone();
    let (store, held) =
        run_store(place, in_ci).ok_or_else(|| failed("there is no store here".into()))?;
    let source = BakeSource::of(args);
    if source == BakeSource::Store
        && let Some(why) = own_store_refusal(held.as_ref())
    {
        return Err(refused(why));
    }
    let _held = held;
    let store = Store::new(&store);
    // B-65: with an image and no --out, the store is written aside, in this machine's store's
    // own temporary folder, and removed once published. The folder is this bake's alone (a name
    // no other bake uses, never a process id: two containers on one store are both process 1),
    // so nothing removes another bake's, and it goes however the bake ends (with --out too, for
    // what it holds besides the store: the state's session and the image's staging). A bake
    // killed before that leaves it to the store's sweep of old leftovers.
    let aside = store
        .root()
        .join("tmp")
        .join(hushrun_build::baked::unique("bake-image"));
    let _aside = RemovedAtEnd(aside.clone());
    let out = match &args.out {
        Some(out) => absolute(out),
        None => aside.join("hushrun"),
    };
    let image = image_target(
        args,
        &absolute,
        args.oci
            .as_ref()
            .and_then(|_| backend_origin(&folder))
            .as_deref(),
    )?;
    // B-72: a dependencies-only store never sets a full image back: asked before it builds.
    if args.deps_only
        && let Some(ImageAsk::Ready(target)) = &image
    {
        let platform = baked::platform_of(rustc_host.as_deref().unwrap_or(&target_triple));
        let tags = moving_tags(args, &folder, &platform);
        if let Some(why) = crate::cmd::build_relay::bake_image::deps_only_waits(target, &tags)
            .map_err(|why| failed(format!("the image's tags couldn't be read: {why}")))?
        {
            if global.format == Format::Json {
                output::print(
                    &serde_json::to_string(&json!({
                        "deps_only": true,
                        "image": {"state": "skipped", "skipped": format!("{why}: nothing was built or published")},
                    }))
                    .map_err(|e| failed(e.to_string()))?,
                )?;
            } else if global.quiet == 0 {
                output::print(&format!(
                    "Not baked: {why}: nothing was built or published.\n"
                ))?;
            }
            return Ok(Exit::Ok);
        }
    }
    // B-65: a store baked from a backend keeps its relay until it is published, which asks it
    // about the blocks an image carries.
    let mut kept_relay: Option<std::sync::Arc<crate::cmd::build_relay::Relay>> = None;
    let mut deps_built: Option<crate::cmd::bake_build::DepsBuilt> = None;
    let mut built_missing: Option<(String, Vec<String>)> = None;
    let repository_env: Vec<String> = content
        .policy
        .as_ref()
        .map(|p| p.build_env.clone())
        .unwrap_or_default();
    let repo = crate::cmd::bake_build::Repo {
        folder: &folder,
        checkout: &checkout,
        repository: repository.as_deref(),
        repository_env: &repository_env,
        go: go_module,
    };
    let written = match source {
        // B-72: the dependencies-only build, in a store of the bake's own, and that store baked.
        BakeSource::Deps => {
            let own = crate::cmd::bake_build::Own::under(&aside, "deps");
            if global.quiet == 0 {
                eprintln!(
                    "hushrun: building the dependencies only{}{}",
                    if args.command.is_empty() {
                        String::new()
                    } else {
                        format!(
                            ": `{}` with the repository's {} own {} as stubs",
                            crate::cmd::bake_build::shell_words(&args.command),
                            own_crates.len(),
                            if own_crates.len() == 1 {
                                "crate"
                            } else {
                                "crates"
                            }
                        )
                    },
                    if go_module {
                        ", and the Go module's third-party packages"
                    } else {
                        ""
                    }
                );
            }
            let built = crate::cmd::bake_build::deps_only(
                &repo,
                &own,
                &args.command,
                own_crates.clone(),
                named.first().copied(),
                SystemTime::now(),
            )
            .map_err(|why| failed(format!("the dependencies-only build: {why}")))?;
            let _ = std::fs::remove_dir_all(&own.target);
            let deps_store = Store::new(&own.store);
            let saved = hushrun_build::manifests::of_checks(
                &deps_store,
                repository.as_deref(),
                checkout_text.as_deref(),
                &named,
                &target,
            );
            if saved.is_empty() {
                return Err(failed(
                    "the dependencies-only build compiled nothing hushrun keeps: is the command a cargo build of this repository?".into(),
                ));
            }
            let written = write::from_store(&deps_store, &saved, &facts, &out).map_err(failed)?;
            deps_built = Some(built);
            written
        }
        BakeSource::Store => {
            let saved = hushrun_build::manifests::of_checks(
                &store,
                repository.as_deref(),
                checkout_text.as_deref(),
                &named,
                &target,
            );
            if saved.is_empty() {
                return Err(refused(format!(
                    "no run of {} here has left a manifest for {target}{}: run the build through `hushrun run NAME -- COMMAND` first, with build reuse on",
                    repository.as_deref().unwrap_or("this repository"),
                    if args.checks.is_empty() {
                        String::new()
                    } else {
                        format!(" and check `{}`", args.checks.join("`, `"))
                    }
                )));
            }
            if args.trusted {
                return Err(refused(
                    "a store baked from this machine's store is never trusted: `--trusted` takes `--from backend`, whose blocks CI on the default branch wrote"
                        .into(),
                ));
            }
            // Past the flags, what goes wrong is the disk's or the store's, not a usage error.
            write::from_store(&store, &saved, &facts, &out).map_err(failed)?
        }
        BakeSource::Backend => {
            let access = match crate::results::Backend::select(Some(&folder)) {
                Ok(crate::results::Selection::On(backend)) => backend.build_access(),
                _ => None,
            }
            .filter(|access| access.sends())
            .ok_or_else(|| {
                refused(
                    "no backend is signed in to. `--from backend` takes the credentials `hushrun build fetch` does; `--from store` needs none"
                        .into(),
                )
            })?;
            let head = head_commit(&folder);
            let config = crate::cmd::build_relay::Config {
                store: store.clone(),
                caller: crate::cmd::build_session::caller(Some(&access), in_ci),
                access,
                repository: repository.clone(),
                commit: head.clone(),
                platform: target.clone(),
                compiler: Some((std::ffi::OsString::from("rustc"), folder.clone())),
                local: Mode::On,
                own: Mode::On,
                min_free_gb: if in_ci {
                    session::CI_MIN_FREE_GB
                } else {
                    settings::MIN_FREE_GB
                },
                keep_days: hushrun_build::KEEP_DAYS,
                tuning: crate::cmd::build_relay::Tuning::default(),
                saved: None,
                open_decides: false,
            };
            // The process's relay: a build of what's missing (B-75) shares through it.
            let relay = crate::cmd::build_relay::Relay::get_or_start(|| Some(config))
                .ok_or_else(|| failed("the relay couldn't start".into()))?
                .map_err(failed)?;
            // B-75: in CI on the default branch, a backend whose newest build is of another
            // commit (a merge whose checks CI skipped) gets what's missing built first.
            let context = hushrun_ci::context(&hushrun_ci::Env::from_process());
            if !args.no_build
                && in_ci
                && crate::cmd::build_relay::bake_image::moves_latest(&context).is_ok()
            {
                built_missing = build_what_is_missing(
                    global,
                    &relay,
                    &facts.checks,
                    head.as_deref(),
                    &content,
                    &args.checks,
                    &repo,
                    store.root(),
                    &folder,
                    context.branch.as_deref(),
                )?;
            }
            let written = write::from_backend(&relay, &facts, &out);
            if global.quiet == 0 {
                for line in relay.take_lines() {
                    eprintln!("hushrun: {line}");
                }
            }
            kept_relay = Some(relay.clone());
            written.map_err(failed)?
        }
    };
    // B-70: a partial store's image would take the backend registry's fixed tag for good:
    // refused before anything is signed (a keyless signature writes a public timestamp).
    if matches!(image, Some(ImageAsk::Backend { .. })) && written.missing > 0 {
        return Err(Failure::new(
            Exit::Partial,
            anyhow!(
                "build bake: the store is partial ({} {} the manifests named couldn't be had), and the backend's registry would give its image the fixed tag for good: nothing was signed or published. Bake again once the backend serves them",
                written.missing,
                if written.missing == 1 {
                    "block"
                } else {
                    "blocks"
                }
            ),
        ));
    }
    // B-67: the folder's own signature, once, before an image, an archive or a disk image of it
    // is made: each holds it as the folder does.
    if let Some(signing) = &signing {
        crate::cmd::build_relay::bake_image::sign_folder(&out, &written.baked, signing)
            .map_err(failed)?;
    }
    // As a run here would read it, with nothing to verify it but what this shell accepts: the
    // one function's answer.
    let here_state = {
        let lower_named = |name: &str| {
            if name == baked::LOWER {
                Some(out.clone().into_os_string())
            } else {
                env(name)
            }
        };
        let signers = content
            .policy
            .as_ref()
            .map(|policy| policy.build_baked.clone())
            .unwrap_or_default();
        crate::cmd::build_session::lower_state(
            &lower_named,
            in_ci,
            store.root(),
            Some(&folder),
            repository.as_deref(),
            go.as_deref(),
            baked::Asked::NotAsked,
            &signers,
            &aside.join("check"),
        )
        .0
    };
    let baked = &written.baked;
    let partial = written.missing > 0;
    // B-63: a partial store is routine for one baked for this machine (blocks get swept), and
    // exits 0 with a line; one others start from (an image, or the trusted blocks of a
    // backend) exits 11, never a registry's 7.
    let publishing =
        image.is_some() || !archives.is_empty() || (args.from == BakeFrom::Backend && args.trusted);
    let done = if partial && publishing {
        Exit::Partial
    } else {
        Exit::Ok
    };
    let published = match image {
        None => None,
        Some(ask) => {
            let target = match ask {
                ImageAsk::Ready(target) => target,
                // B-70: a partial store's image would take the fixed tag there for good.
                ImageAsk::Backend { .. } if partial => {
                    return Err(Failure::new(
                        Exit::Partial,
                        anyhow!(
                            "build bake: the store is partial ({} {} the manifests named couldn't be had), and the backend's registry would give its image the fixed tag for good: nothing was published. Bake again once the backend serves them",
                            written.missing,
                            if written.missing == 1 {
                                "block"
                            } else {
                                "blocks"
                            }
                        ),
                    ));
                }
                ImageAsk::Backend { given } => hosted_target(
                    args,
                    given,
                    kept_relay.as_ref(),
                    repository.as_deref(),
                    &folder,
                )?,
            };
            let signers = content
                .policy
                .as_ref()
                .map(|policy| policy.build_baked.clone())
                .unwrap_or_default();
            let settings = content
                .policy
                .as_ref()
                .map(|policy| policy.build_bake.clone())
                .unwrap_or_default();
            let published = publish_image(
                global,
                args,
                signing.as_ref(),
                &out,
                &written,
                &target,
                &aside,
                &signers,
                &settings,
                kept_relay.as_ref(),
                &folder,
            );
            let published = published?;
            // B-66: said on stderr whatever the format, so a CI log shows it.
            if global.quiet == 0 {
                for warning in &published.warnings {
                    eprintln!("hushrun: warning: {warning}");
                }
            }
            Some(published)
        }
    };
    // B-71: the archive and the disk image of the folder as it is now, signature and all.
    let mut wrote = Vec::new();
    for (form, dest) in &archives {
        wrote.push(
            crate::cmd::build_relay::bake_archive::write(&out, baked, *form, dest).map_err(
                |why| {
                    // What was published already is said, so a failed archive loses no tag.
                    let image = published.as_ref().map_or(String::new(), |p| {
                        format!(
                            "; the image was published already: {} (tags {})",
                            p.reference,
                            if p.tags.is_empty() {
                                "none".to_string()
                            } else {
                                p.tags.join(", ")
                            }
                        )
                    });
                    failed(format!("writing the {}: {why}{image}", form.word()))
                },
            )?,
        );
    }
    // B-74: what the store saves on this machine, measured: COMMAND cold, and from the store.
    let report = if args.report {
        let signers = content
            .policy
            .as_ref()
            .map(|policy| policy.build_baked.clone())
            .unwrap_or_default();
        Some(measure(
            global,
            &repo,
            &aside,
            &args.command,
            deps_built.as_ref().map(|built| (built, own_crates.clone())),
            &out,
            go.as_deref(),
            &signers,
        )?)
    } else {
        None
    };
    // With the archive on stdout, what bake says goes to stderr.
    let say = |text: &str| -> anyhow::Result<()> {
        if to_stdout {
            eprint!("{text}");
            if !text.ends_with('\n') {
                eprintln!();
            }
            Ok(())
        } else {
            output::print(text)
        }
    };
    if global.format == Format::Json {
        say(&serde_json::to_string(&json!({
            "out": out,
            "blocks": baked.packs.len(),
            "bytes": written.bytes,
            "hints": written.hints,
            "manifest": baked.manifest.to_string(),
            "platform": baked.platform,
            "checks": baked.checks.iter().map(|c| json!({
                "check": c.check.map(|c| c.to_string()),
                "parts": c.parts.iter().map(|p| json!({
                    "kind": p.kind,
                    "platform": p.platform,
                    "manifest": p.manifest.to_string(),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "parts": baked.platforms().iter().map(|(kind, platform)| json!({
                "kind": kind,
                "platform": platform,
                "blocks": baked.packs.iter().filter(|r| r.kind == *kind).count(),
            })).collect::<Vec<_>>(),
            "trust": baked.trust,
            "left_out": written.left_out,
            "missing": written.missing,
            "partial": partial,
            "here": here_state.word(),
            "image": published.as_ref().map(image_json),
            "archives": wrote.iter().map(archive_json).collect::<Vec<_>>(),
            "deps_only": deps_built.as_ref().map(|built| json!({
                "wall_ms": built.ran.wall.as_millis() as u64,
                "compiles": built.ran.counts.compiles,
                "stored": built.ran.counts.built_and_stored,
                "stubbed": own_crates.len(),
                "go_packages": built.go_packages,
            })),
            "report": report.as_ref().map(report_json),
            "built_missing": built_missing.as_ref().map(|(why, lines)| json!({
                "why": why,
                "lines": lines,
            })),
        }))
        .map_err(|e| failed(e.to_string()))?)?;
        return Ok(done);
    }
    if global.quiet == 0 {
        let by_kind: Vec<String> = baked
            .platforms()
            .iter()
            .map(|(kind, platform)| {
                let n = baked.packs.iter().filter(|r| r.kind == *kind).count();
                format!("{n} {kind} for {platform}")
            })
            .collect();
        let into = if args.out.is_some() {
            format!(" into {}", out.display())
        } else {
            String::new()
        };
        let mut text = format!(
            "Baked {} {} ({}: {}) and {} {}{}, from the manifest {} of {} {}.\n",
            baked.packs.len(),
            if baked.packs.len() == 1 {
                "block"
            } else {
                "blocks"
            },
            size(written.bytes),
            by_kind.join(", "),
            written.hints,
            if written.hints == 1 { "hint" } else { "hints" },
            into,
            &baked.manifest.to_string()[..12],
            baked.checks.len(),
            if baked.checks.len() == 1 {
                "check"
            } else {
                "checks"
            },
        );
        if !written.left_out.is_empty() {
            let why: Vec<String> = written
                .left_out
                .iter()
                .map(|(why, n)| format!("{n} {why}"))
                .collect();
            let _ = writeln!(
                text,
                "Left out, as they may not leave a machine: {}.",
                why.join(", ")
            );
        }
        if go.is_some() && args.from == BakeFrom::Store && !baked.parts().any(|p| p.kind == "go/1")
        {
            let _ = writeln!(
                text,
                "No Go packages: a run keeps them in this machine's store only while it shares or reads a baked store (B-39); `--from backend` bakes the shared ones."
            );
        }
        if partial {
            let _ = writeln!(
                text,
                "Partial: {} {} the manifests named couldn't be had (no longer in the store, or not downloaded in time); they compile where it is read.",
                written.missing,
                if written.missing == 1 {
                    "block"
                } else {
                    "blocks"
                }
            );
        }
        match baked.trust {
            baked::Trust::Trusted => {
                let _ = writeln!(
                    text,
                    "It claims to be trusted: every block came from the repository's trusted scope. A run trusts it only as its backend vouches for each block."
                );
            }
            baked::Trust::Untrusted { because } if signing.is_some() => {
                let _ = writeln!(
                    text,
                    "It claims to be untrusted ({}), and it is signed: a run trusts it where hushrun.yaml names the signer under build: baked: signers:.",
                    because.words()
                );
            }
            baked::Trust::Untrusted { because } => {
                let _ = writeln!(
                    text,
                    "Untrusted: {}. It is for you alone: a run reads it only with {}=1, and never in CI.",
                    because.words(),
                    baked::LOWER_UNTRUSTED
                );
            }
        }
        match &published {
            None => {
                let _ = writeln!(
                    text,
                    "A run reads it underneath its own store with {}={}. Here: {}.",
                    baked::LOWER,
                    out.display(),
                    here_state.words()
                );
            }
            Some(published) => image_text(&mut text, published),
        }
        for archive in &wrote {
            archive_text(&mut text, archive, written.baked.platform.as_str());
        }
        if let Some(built) = &deps_built {
            let _ = writeln!(
                text,
                "Dependencies only: built in {} ({} compiles kept{}, the repository's {} own {} as stubs). A full bake replaces this image; this one never replaces a full one.",
                duration_words(built.ran.wall),
                built.ran.counts.built_and_stored,
                if built.go_packages > 0 {
                    format!(", {} Go packages", built.go_packages)
                } else {
                    String::new()
                },
                own_crates.len(),
                if own_crates.len() == 1 {
                    "crate"
                } else {
                    "crates"
                },
            );
        }
        if let Some(report) = &report {
            report_text(&mut text, report);
        }
        say(&text)?;
    }
    Ok(done)
}

/// B-72, B-74: what `--deps-only` and `--report` need, checked before anything is baked: the
/// repository's own crates (the lockfile's path packages) a dependencies-only build stubs. A
/// COMMAND goes with one of them; a dependencies-only build of a Cargo workspace needs its cargo
/// command (whose profile, features and target the third-party crates are compiled for) and its
/// `Cargo.lock`; it bakes one check, or none; C and C++ have no dependencies apart from the
/// project's own sources, so a repository with neither Cargo nor Go has nothing to build.
fn deps_build_plan(
    args: &BuildBakeArgs,
    cargo: bool,
    go_module: bool,
    lockfile: Option<&crate::cmd::build_relay::bake_image::ThirdParty>,
) -> Result<Vec<String>, Failure> {
    let refused = |why: String| Failure::new(Exit::Usage, anyhow!("build bake: {why}"));
    if !args.deps_only && !args.report && !args.command.is_empty() {
        return Err(refused(
            "a COMMAND after -- goes with --deps-only or --report".into(),
        ));
    }
    if !args.deps_only {
        return Ok(Vec::new());
    }
    if args.from == BakeFrom::Backend {
        return Err(refused(
            "--deps-only builds the dependencies here, in a store of its own: it takes no --from backend".into(),
        ));
    }
    if args.checks.len() > 1 {
        return Err(refused(
            "--deps-only builds one check's dependencies: name one --check, or none".into(),
        ));
    }
    if !cargo && !go_module {
        return Err(refused(
            "--deps-only builds the third-party Rust crates of a Cargo workspace and the third-party packages of a Go module, and there is neither here; C and C++ have no dependencies apart from the project's own sources for it to build".into(),
        ));
    }
    if cargo && args.command.is_empty() {
        return Err(refused(
            "--deps-only needs the check's cargo command after --, such as `-- cargo test --locked` (whose tests are compiled, and none run): the third-party crates are compiled for its profile, features and target, as the real build compiles them".into(),
        ));
    }
    // A Go module's command (`go test -race ./...`) gives its flags to the third-party build.
    if !args.command.is_empty() && args.command.first().map(String::as_str) != Some("go") {
        let Some(lockfile) = lockfile else {
            return Err(refused(
                "--deps-only reads Cargo.lock to tell the repository's own crates from third-party ones, and there is none: commit one, or run `cargo generate-lockfile`".into(),
            ));
        };
        return Ok(lockfile.own_only());
    }
    Ok(Vec::new())
}

/// The moving tags a bake here would move (B-66): `-latest` from CI on the default branch, the
/// branch's with `--branch-tag`.
fn moving_tags(args: &BuildBakeArgs, folder: &std::path::Path, platform: &str) -> Vec<String> {
    use hushrun_core::blocks::image as tags;
    let context = hushrun_ci::context(&hushrun_ci::Env::from_process());
    let mut out = Vec::new();
    if crate::cmd::build_relay::bake_image::moves_latest(&context).is_ok() {
        out.push(tags::latest_tag(platform));
    }
    if let Some(branch) = tagged_branch(args, folder, &context) {
        let tag = tags::branch_tag(platform, &branch);
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

/// B-66: the branch whose tag a bake moves, with `--branch-tag`: CI's branch, else the one
/// checked out. The one definition [`moving_tags`] and [`publish_image`] read.
fn tagged_branch(
    args: &BuildBakeArgs,
    folder: &std::path::Path,
    context: &hushrun_ci::RunContext,
) -> Option<String> {
    args.branch_tag
        .then(|| context.branch.clone().or_else(|| current_branch(folder)))
        .flatten()
}

/// B-75: asks the backend for each check's newest build and, when one isn't of `head`
/// ([`crate::cmd::bake_build::behind`], the one decision), builds the checks as CI's build job
/// runs them ([`ci_builds`]), through the relay as CI on the default branch, in the bake's store,
/// within [`crate::cmd::bake_build::BUILD_WITHIN`]. What it did, said on stderr, comes back for
/// the JSON; `None` when nothing was built.
#[allow(clippy::too_many_arguments)]
fn build_what_is_missing(
    global: &GlobalOpts,
    relay: &std::sync::Arc<crate::cmd::build_relay::Relay>,
    checks: &[Option<hushrun_core::blocks::Hex32>],
    head: Option<&str>,
    content: &crate::context::Content,
    named: &[String],
    repo: &crate::cmd::bake_build::Repo<'_>,
    store: &std::path::Path,
    folder: &std::path::Path,
    branch: Option<&str>,
) -> Result<Option<(String, Vec<String>)>, Failure> {
    use crate::cmd::bake_build::{self, BUILD_WITHIN, Behind};
    let failed = |why: String| Failure::new(Exit::Unexpected, anyhow!("build bake: {why}"));
    let say = |line: &str| {
        if global.quiet == 0 {
            eprintln!("hushrun: {line}");
        }
    };
    let newest = relay.newest_manifests(checks).map_err(|why| {
        failed(format!(
            "the backend's newest builds couldn't be read: {why}"
        ))
    })?;
    let behind = match bake_build::behind(&newest, head, &|older| {
        bake_build::descends(repo.checkout, older)
    }) {
        Behind::Current(words) => {
            say(&words);
            return Ok(None);
        }
        Behind::Build(behind) => behind,
    };
    // Only the checks that are behind are built, each said by its name.
    let id = |name: &str| crate::cmd::build_session::check_id(repo.repository, name);
    let names = if named.is_empty() {
        backend_check_names(content)
    } else {
        named.to_vec()
    };
    let name_of = |check: &Option<hushrun_core::blocks::Hex32>| match check {
        Some(check) => names
            .iter()
            .find(|name| id(name) == *check)
            .map_or_else(|| format!("check {check}"), |name| format!("`{name}`")),
        None => "the repository's newest build".to_string(),
    };
    let why = behind
        .iter()
        .map(|(check, why)| format!("{}: {why}", name_of(check)))
        .collect::<Vec<_>>()
        .join("; ");
    let all = behind.iter().any(|(check, _)| check.is_none());
    let (mut runs, mut lines) = ci_builds(content, named, branch);
    runs.retain(|run| {
        all || behind
            .iter()
            .any(|(check, _)| *check == Some(id(&run.check)))
    });
    for line in &lines {
        say(line);
    }
    if runs.is_empty() {
        let line = format!(
            "{why}, and neither CI's files nor hushrun.yaml give a check's command to build it with: baking what the backend holds"
        );
        say(&line);
        lines.push(line);
        return Ok(Some((why, lines)));
    }
    say(&format!(
        "{why}: building what no trusted store holds with {}, as CI builds them (at most {} minutes; --no-build skips it)",
        runs.iter()
            .map(|r| format!("`{}`", r.check))
            .collect::<Vec<_>>()
            .join(", "),
        BUILD_WITHIN.as_secs() / 60
    ));
    // ADR 0102: the backend's token is the bake's, had before the build starts; the build gets
    // no credential of the job's.
    if let Err(why) = relay.bearer_now(std::time::Duration::from_secs(30)) {
        say(&format!(
            "the backend's token couldn't be had before the build: {why}"
        ));
    }
    let policy = content.policy.as_ref();
    let cc_maps = policy.is_none_or(|p| p.build_cc_maps);
    let start = |name: &str| {
        let access = match crate::results::Backend::select(Some(folder)) {
            Ok(crate::results::Selection::On(backend)) => backend.build_access(),
            _ => None,
        }?;
        Some(crate::cmd::build_session::ShareStart {
            access,
            commit: head.map(str::to_string),
            check: name.to_string(),
            open_decides: true,
        })
    };
    let built = bake_build::build_missing(repo, &runs, store, cc_maps, &start, BUILD_WITHIN)
        .map_err(|why| failed(format!("the build of what's missing: {why}")))?;
    for line in &built {
        say(line);
    }
    lines.extend(built);
    Ok(Some((why, lines)))
}

/// B-75: the checks a bake builds and how, each as CI's build job runs it: CI's files at the
/// commit checked out, read for a push to `branch` ([`crate::cmd::setup::replay::replays`]: the
/// command of each `hushrun run NAME -- COMMAND`, its folder and its job's variables), then each
/// check hushrun.yaml gives a `run` that CI's files don't (at the repository's root). With
/// `named` (`--check`), those alone. The lines say what can't be built as CI builds it, and the
/// variables of CI's left out.
fn ci_builds(
    content: &crate::context::Content,
    named: &[String],
    branch: Option<&str>,
) -> (Vec<crate::cmd::setup::replay::Replay>, Vec<String>) {
    use crate::cmd::setup::replay::{self, Replay};
    let pipelines = ci_pipelines(content);
    let mut runs: Vec<Replay> = Vec::new();
    let mut lines = Vec::new();
    // CI's own variables are the bake job's: a job of the same repository on the same kind of
    // runner. The credentials its build never gets aren't among them.
    let runner: std::collections::BTreeMap<String, String> = std::env::vars()
        .filter(|(name, _)| !crate::cmd::bake_build::WITHHELD.contains(&name.as_str()))
        .collect();
    for (check, found) in replay::replays(&pipelines, branch, named, Some(&runner)) {
        match found {
            Ok(found) => {
                for (name, why) in &found.left_out {
                    lines.push(format!(
                        "check `{check}`: {name} of {} is left out of its build: {why}",
                        found.from
                    ));
                }
                runs.push(found);
            }
            Err(why) => lines.push(format!(
                "check `{check}` isn't built as CI builds it: {why}"
            )),
        }
    }
    if let Some(policy) = content.policy.as_ref() {
        for (name, def) in &policy.checks {
            let name = name.as_ref();
            if (!named.is_empty() && !named.iter().any(|n| n == name))
                || runs.iter().any(|r| r.check == name)
            {
                continue;
            }
            if let Some(run) = &def.run {
                runs.push(Replay {
                    check: name.to_string(),
                    line: run.clone(),
                    dir: String::new(),
                    env: Vec::new(),
                    left_out: Vec::new(),
                    from: "hushrun.yaml".to_string(),
                    shell: replay::Shell::Inline(
                        def.shell
                            .unwrap_or(hushrun_core::policy::Shell::Sh)
                            .as_str()
                            .to_string(),
                    ),
                });
            }
        }
    }
    (runs, lines)
}

/// B-76: the folders the default branch's build ran with, which a baked store records: the first
/// check's variables as CI's files give its build job, filled in as a bake's build fills them
/// ([`ci_builds`]), over the bake's own environment (`env`); a relative folder is under the
/// check's folder.
fn built_paths(
    content: &crate::context::Content,
    named: &[String],
    checkout: &std::path::Path,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<hushrun_build::baked::BakedPaths> {
    let branch = hushrun_ci::context(&hushrun_ci::Env::from_process()).branch;
    let (runs, _) = ci_builds(content, named, branch.as_deref());
    let first = runs.first();
    let of = |name: &str| {
        first
            .and_then(|run| {
                let (_, value) = run.env.iter().rev().find(|(n, _)| n == name)?;
                Some(if std::path::Path::new(value).is_absolute() {
                    std::ffi::OsString::from(value)
                } else {
                    checkout.join(&run.dir).join(value).into_os_string()
                })
            })
            .or_else(|| env(name))
    };
    hushrun_build::baked::BakedPaths::of(&of, checkout)
}

/// B-74: what `--report` measured.
struct Report {
    command: String,
    cold: crate::cmd::bake_build::Ran,
    warm: crate::cmd::bake_build::Ran,
    /// The baked store's state for the warm build, in words.
    state: String,
    /// Both builds stubbed the repository's own crates (`--deps-only`).
    stubbed: bool,
}

/// B-74: COMMAND timed on this machine, in fresh target folders beside the bake: cold, with
/// nothing to reuse (with `--deps-only`, the dependencies-only build itself, which had nothing to
/// reuse, unless it also built Go packages COMMAND doesn't), then from the store at `out`, as a
/// sandbox started from its image runs it. Each build's folders go once it is measured.
#[allow(clippy::too_many_arguments)]
fn measure(
    global: &GlobalOpts,
    repo: &crate::cmd::bake_build::Repo<'_>,
    aside: &std::path::Path,
    command: &[String],
    deps: Option<(&crate::cmd::bake_build::DepsBuilt, Vec<String>)>,
    out: &std::path::Path,
    go: Option<&str>,
    signers: &hushrun_core::policy::BakedTrust,
) -> Result<Report, Failure> {
    use crate::cmd::bake_build::{self, Own};
    let failed =
        |why: String| Failure::new(Exit::Unexpected, anyhow!("build bake: --report: {why}"));
    let say = |what: &str| {
        if global.quiet == 0 {
            eprintln!(
                "hushrun: --report: {what} `{}`",
                bake_build::shell_words(command)
            );
        }
    };
    let gone = |own: &Own| {
        let _ = std::fs::remove_dir_all(&own.target);
        let _ = std::fs::remove_dir_all(&own.store);
    };
    let stub = deps.as_ref().map(|(_, own)| own.clone());
    let cold = match &deps {
        Some((built, _)) if built.go_packages == 0 => built.ran.clone(),
        _ => {
            say("building cold, with nothing to reuse:");
            let own = Own::under(aside, "cold");
            let ran = bake_build::cold(repo, &own, command, stub.clone());
            gone(&own);
            ran.map_err(failed)?
        }
    };
    say("building from the baked store:");
    let own = Own::under(aside, "warm");
    let warm = bake_build::warm(repo, &own, command, stub.clone(), (out, go, signers));
    gone(&own);
    let (warm, state) = warm.map_err(failed)?;
    Ok(Report {
        command: bake_build::shell_words(command),
        cold,
        warm,
        state,
        stubbed: stub.is_some(),
    })
}

fn report_json(report: &Report) -> Value {
    json!({
        "command": report.command,
        "cold_ms": report.cold.wall.as_millis() as u64,
        "warm_ms": report.warm.wall.as_millis() as u64,
        "cold_compiles": report.cold.counts.compiles,
        "warm_compiles": report.warm.counts.compiles,
        "warm_reused": report.warm.counts.reused,
        "warm_from_baked": report.warm.counts.baked,
        "stubbed": report.stubbed,
        "baked_state": report.state,
    })
}

/// B-74: the measurement in words: both times, what the warm build took from the store, and,
/// when it took nothing, the store's state.
fn report_text(text: &mut String, report: &Report) {
    let cold = report.cold.wall.as_secs_f64();
    let warm = report.warm.wall.as_secs_f64();
    let _ = writeln!(
        text,
        "Measured on this machine: `{}` took {} with nothing to reuse and {} from the baked store{} ({} of {} compiles came from it).",
        report.command,
        duration_words(report.cold.wall),
        duration_words(report.warm.wall),
        if warm > 0.0 && cold > warm {
            format!(", {:.1} times faster", cold / warm)
        } else {
            String::new()
        },
        report.warm.counts.baked,
        report.warm.counts.compiles,
    );
    if report.stubbed {
        let _ = writeln!(
            text,
            "Both builds compiled the repository's own crates as stubs: this is the dependencies' share of the build."
        );
    }
    if report.warm.counts.baked == 0 {
        let _ = writeln!(
            text,
            "The warm build read nothing from the store: {}.",
            report.state
        );
    }
}

/// A span of time as a person reads it: `9.8 s`, `2m 31s`.
fn duration_words(span: std::time::Duration) -> String {
    let secs = span.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        let whole = span.as_secs();
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}

/// B-71: what `--tar` and `--erofs` ask for, each with where it goes. Refused: both on stdout.
fn archive_targets(
    args: &BuildBakeArgs,
    absolute: &dyn Fn(&std::path::Path) -> PathBuf,
) -> Result<
    Vec<(
        crate::cmd::build_relay::bake_archive::Form,
        crate::cmd::build_relay::bake_archive::Dest,
    )>,
    Failure,
> {
    use crate::cmd::build_relay::bake_archive::{Dest, Form};
    let refused = |why: String| Failure::new(Exit::Usage, anyhow!("build bake: {why}"));
    let dest = |path: &std::path::Path| match Dest::of(path) {
        Dest::Stdout => Dest::Stdout,
        Dest::File(path) => Dest::File(absolute(&path)),
    };
    let mut out = Vec::new();
    if let Some(path) = &args.tar {
        out.push((Form::of_tar(path), dest(path)));
    }
    if let Some(path) = &args.erofs {
        out.push((Form::Erofs, dest(path)));
    }
    if out.iter().filter(|(_, d)| *d == Dest::Stdout).count() > 1 {
        return Err(refused(
            "only one of --tar and --erofs can write to stdout (`-`)".into(),
        ));
    }
    if let (Some((_, Dest::File(a))), Some((_, Dest::File(b)))) = (out.first(), out.get(1))
        && resolved(a) == resolved(b)
    {
        return Err(refused("--tar and --erofs name the same file".into()));
    }
    // An archive's bytes are no use on a terminal, and would garble it.
    if out.iter().any(|(_, d)| *d == Dest::Stdout) && std::io::stdout().is_terminal() {
        return Err(refused(
            "`-` writes the archive to stdout, which is a terminal here: pipe it (`| tar -x -C /`) or name a file".into(),
        ));
    }
    Ok(out)
}

/// Where `path` names, as far as the disk can say: its folder's real path (links and `..`
/// resolved) and its name, so two spellings of one file compare equal; a folder that doesn't
/// exist yet is taken as written, with `.` and `..` folded.
fn resolved(path: &std::path::Path) -> PathBuf {
    let folded = |path: &std::path::Path| {
        let mut out = PathBuf::new();
        for part in path.components() {
            match part {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other),
            }
        }
        out
    };
    let real = |path: &std::path::Path| match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent).ok().map(|r| r.join(name)),
        _ => None,
    };
    real(path).unwrap_or_else(|| {
        let folded = folded(path);
        real(&folded).unwrap_or(folded)
    })
}

/// B-63: when a store is baked, as its `baked.json` says: now, or, for a store this bake signs
/// nothing of, `SOURCE_DATE_EPOCH` when it is set (seconds since 1970), so that two bakes of the
/// same store give the same `baked.json` and the same archive, disk image and layers, byte for
/// byte. A signed store's time is the clock's: `build: baked: not_before` compares it (B-67).
fn baked_at(signs: bool, env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> SystemTime {
    // A time past what the clock can hold (or past year 9999's milliseconds) is no time: the
    // clock's, as for a value that isn't a number.
    const MOST_SECONDS: u64 = 253_402_300_799;
    if !signs
        && let Some(seconds) = env("SOURCE_DATE_EPOCH")
            .and_then(|v| v.into_string().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|s| *s <= MOST_SECONDS)
        && let Some(at) =
            SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds))
    {
        return at;
    }
    SystemTime::now()
}

/// An archive a bake wrote, as `--format json` writes it.
fn archive_json(archive: &crate::cmd::build_relay::bake_archive::Written) -> Value {
    json!({
        "format": archive.form.word(),
        "path": archive.path,
        "bytes": archive.bytes,
        "sha256": archive.sha256.to_string(),
        "files": archive.files,
        "unpacked": archive.unpacked,
    })
}

/// What a bake says of an archive or a disk image it wrote, and how a sandbox takes it.
fn archive_text(
    text: &mut String,
    archive: &crate::cmd::build_relay::bake_archive::Written,
    platform: &str,
) {
    use crate::cmd::build_relay::bake_archive::Form;
    let place = archive
        .path
        .as_ref()
        .map_or("stdout".to_string(), |p| p.display().to_string());
    let _ = writeln!(
        text,
        "Wrote {place}: a {} of {} files, {} (sha256 {}), for {platform}.",
        match archive.form {
            Form::Tar => "tar".to_string(),
            Form::TarZst => format!("zstd tar ({} unpacked)", self::size(archive.unpacked)),
            Form::Erofs => "read-only EROFS disk image".to_string(),
        },
        archive.files,
        self::size(archive.bytes),
        &archive.sha256.to_string()[..16],
    );
    let _ = match archive.form {
        Form::Tar => writeln!(
            text,
            "A sandbox takes it with `tar -x -C / -f FILE` (as root, or as the user who runs builds): the store lands at /hushrun, where a run reads it."
        ),
        Form::TarZst => writeln!(
            text,
            "A sandbox takes it with `zstd -dc FILE | tar -x -C /` (as root, or as the user who runs builds): the store lands at /hushrun, where a run reads it."
        ),
        Form::Erofs => writeln!(
            text,
            "Mount it read-only at /hushrun (`mount -t erofs -o ro FILE /hushrun`), or anywhere else with {}=<mount point>.",
            hushrun_build::baked::LOWER
        ),
    };
}

/// `--oci`'s word for the backend's registry (B-70), whatever its host.
const BACKEND_REGISTRY: &str = "hushrun";

/// Where `--oci` or `--oci-dir` asks for the image to go.
enum ImageAsk {
    /// A registry's repository, or a folder (B-65).
    Ready(crate::cmd::build_relay::bake_image::Target),
    /// B-70: the backend's registry (`--oci hushrun`, or a repository on its host), published
    /// through the backend once the bake's relay is up.
    Backend {
        given: Option<hushrun_artifact::Repository>,
    },
}

/// B-65, B-70: where `--oci` or `--oci-dir` sends the image; `None` with neither. A repository on
/// the host of the registry this repository's backend serves (`backend_origin`'s,
/// `hosted::registry_host`), or the word `hushrun`, is the backend's registry, which takes the
/// image's description from CI on the default branch instead of blobs, and only of blocks its
/// trusted scope serves: `--from backend --trusted`, and no branch tag. Any other repository is
/// pushed to with docker's credentials.
fn image_target(
    args: &BuildBakeArgs,
    absolute: &dyn Fn(&std::path::Path) -> PathBuf,
    backend_origin: Option<&str>,
) -> Result<Option<ImageAsk>, Failure> {
    use crate::cmd::build_relay::bake_image::Target;
    let refused = |why: String| Failure::new(Exit::Usage, anyhow!("build bake: {why}"));
    if let Some(dir) = &args.oci_dir {
        return Ok(Some(ImageAsk::Ready(Target::Layout(absolute(dir)))));
    }
    let Some(text) = &args.oci else {
        return Ok(None);
    };
    let backend_registry = backend_origin.map(crate::hosted::registry_host);
    let given = if text == BACKEND_REGISTRY {
        if backend_registry.is_none() {
            return Err(refused(
                "--oci hushrun publishes to the registry of the repository's backend, and no backend is named here (hushrun.yaml's `backend`, or HUSHRUN_BACKEND)".into(),
            ));
        }
        None
    } else {
        let reference =
            hushrun_artifact::ImageRef::parse(text).map_err(|e| refused(e.to_string()))?;
        if reference.tag.is_some() || reference.digest.is_some() {
            return Err(refused(format!(
                "--oci takes a repository, such as ghcr.io/acme/app, not {text}: hushrun names the tags"
            )));
        }
        if Some(reference.repository.host.as_str()) != backend_registry.as_deref() {
            if reference.repository.host == crate::hosted::HOSTED_REGISTRY {
                return Err(refused(format!(
                    "{} is the registry of hushrun's hosted backend, which this repository doesn't use: publish to its own backend's registry with --oci hushrun, or to a registry of your own",
                    crate::hosted::HOSTED_REGISTRY
                )));
            }
            // ADR 0102: the registry's credential is settled now, before the bake builds
            // anything: in CI the job's own first; else the Docker configuration's, read and its
            // helper run now. Nothing the build could write later is consulted.
            let host = reference.repository.host.clone();
            let config = hushrun_artifact::credentials::DockerConfig::load()
                .map_err(|e| refused(e.to_string()))?;
            let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
            let credential =
                hushrun_artifact::credentials::settled(&host, &config, &env).map_err(|e| {
                    Failure::new(
                        Exit::Unexpected,
                        anyhow!("build bake: the credential for {host}: {e}"),
                    )
                })?;
            let artifacts = hushrun_artifact::Artifacts::connect_with(
                hushrun_artifact::credentials::DockerConfig::default(),
                &hushrun_artifact::Options {
                    insecure_registries: args.registry.insecure.clone(),
                    credentials: vec![(host, credential)],
                },
            )
            .map_err(|e| refused(e.to_string()))?;
            return Ok(Some(ImageAsk::Ready(Target::Registry {
                repository: reference.repository,
                artifacts: Box::new(artifacts),
            })));
        }
        Some(reference.repository)
    };
    if args.from != BakeFrom::Backend || !args.trusted {
        return Err(refused(
            "the backend's registry takes only blocks its repository's trusted scope serves: bake with --from backend --trusted".into(),
        ));
    }
    if args.branch_tag {
        return Err(refused(
            "the backend's registry has no branch tags: only its moving tag and fixed tags".into(),
        ));
    }
    Ok(Some(ImageAsk::Backend { given }))
}

/// The origin of the backend this repository's runs use (B-70), when one is named.
fn backend_origin(folder: &std::path::Path) -> Option<String> {
    match crate::results::Backend::select(Some(folder)) {
        Ok(crate::results::Selection::On(backend)) => backend.build_access().map(|a| a.host),
        _ => None,
    }
}

/// B-70: the backend's registry as a bake's target: the image `<registry host>/<org>/<name>`
/// (`name` the repository's, `hushrun_core::blocks::publish::image_name`), read with the relay's
/// own credential (in CI the job's ID token, for its own repository), and published through the
/// relay with the workspace's commit.
fn hosted_target(
    args: &BuildBakeArgs,
    given: Option<hushrun_artifact::Repository>,
    relay: Option<&std::sync::Arc<crate::cmd::build_relay::Relay>>,
    repository: Option<&str>,
    folder: &std::path::Path,
) -> Result<crate::cmd::build_relay::bake_image::Target, Failure> {
    use crate::cmd::build_relay::bake_image::{Hosted, Target};
    let refused = |why: String| Failure::new(Exit::Usage, anyhow!("build bake: {why}"));
    let failed = |why: String| Failure::new(Exit::Unexpected, anyhow!("build bake: {why}"));
    let relay = relay
        .ok_or_else(|| refused("the backend's registry takes --from backend".into()))?
        .clone();
    let access = relay.access();
    let name = repository
        .and_then(hushrun_core::blocks::publish::image_name)
        .ok_or_else(|| {
            refused(format!(
                "the backend's registry names an image after its repository, and {} has no name it takes",
                repository.unwrap_or("this repository")
            ))
        })?;
    let host = crate::hosted::registry_host(&access.host);
    let image = hushrun_artifact::Repository::parse(&format!("{host}/{}/{name}", access.org))
        .map_err(|e| refused(e.to_string()))?;
    if let Some(given) = &given
        && given.path != image.path
    {
        return Err(refused(format!(
            "the backend's registry names this repository's image {image}, not {given}"
        )));
    }
    let token = relay
        .bearer_now(std::time::Duration::from_secs(30))
        .map_err(|why| {
            failed(format!(
                "no credential to read the backend's registry with: {why}"
            ))
        })?;
    let mut insecure = args.registry.insecure.clone();
    if access.host.starts_with("http://") {
        insecure.push(image.host.clone());
    }
    // ADR 0102: the backend's token alone, had before the bake built anything: no Docker
    // configuration is read after the build.
    let artifacts = hushrun_artifact::Artifacts::connect_with(
        hushrun_artifact::credentials::DockerConfig::default(),
        &hushrun_artifact::Options {
            insecure_registries: insecure,
            credentials: vec![(
                image.host.clone(),
                hushrun_artifact::credentials::Credential::Basic {
                    username: "hushrun".into(),
                    password: token,
                },
            )],
        },
    )
    .map_err(|e| failed(e.to_string()))?;
    Ok(Target::Hosted(Box::new(Hosted {
        repository: image,
        artifacts: Box::new(artifacts),
        relay,
        commit: head_commit(folder),
    })))
}

/// The commit checked out in `folder`, as `git rev-parse HEAD` names it.
fn head_commit(folder: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(["rev-parse", "HEAD"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|c| hushrun_core::blocks::is_commit(c))
}

/// B-65 to B-67: publishes the baked store at `out`, whose folder `signing` signed already, and
/// signs its image with the same signer (the one `--signing-key`, the signing variables or the CI
/// job's ID token give, or none). The moving
/// tag moves only from CI on the default branch (B-66); the image a bake builds on must be signed
/// by one of `signers`, and the blocks it carries vouched for by `relay`'s backend.
#[allow(clippy::too_many_arguments)]
fn publish_image(
    global: &GlobalOpts,
    args: &BuildBakeArgs,
    signing: Option<&crate::signing::Signing>,
    out: &std::path::Path,
    written: &crate::cmd::build_relay::baked::Written,
    target: &crate::cmd::build_relay::bake_image::Target,
    aside: &std::path::Path,
    signers: &hushrun_core::policy::BakedTrust,
    settings: &hushrun_core::policy::BakeSettings,
    relay: Option<&std::sync::Arc<crate::cmd::build_relay::Relay>>,
    folder: &std::path::Path,
) -> Result<crate::cmd::build_relay::bake_image::Published, Failure> {
    use crate::cmd::build_relay::bake_image;
    let failed = |why: String| Failure::new(Exit::Unexpected, anyhow!("build bake: {why}"));
    let context = hushrun_ci::context(&hushrun_ci::Env::from_process());
    let latest = bake_image::moves_latest(&context);
    let branch = tagged_branch(args, folder, &context);
    if args.branch_tag && branch.is_none() && global.quiet == 0 {
        eprintln!("hushrun: --branch-tag: no branch is checked out, so no branch tag moves");
    }
    let env = |name: &str| hushrun_ci::var_os(name);
    let root = bake_image::run_root(&env);
    let vouch = relay.map(|relay| {
        let relay = relay.clone();
        move |rows: &[hushrun_build::baked::PackRow]| {
            relay
                .vouched_rows(rows, std::time::Duration::from_secs(30))
                .map(|(rows, _)| rows)
        }
    });
    let publishing = bake_image::Publishing {
        latest: latest.is_ok(),
        branch,
        complete: written.missing == 0,
        signers: &signers.signers,
        root: root.as_ref(),
        not_before: signers.not_before,
        vouch: vouch
            .as_ref()
            .map(|v| v as &dyn Fn(&[hushrun_build::baked::PackRow]) -> Option<_>),
        compact_every_ms: compact_every_ms(settings),
        // B-73: a scheduled bake is the way out of a held one: it publishes whatever the limit.
        rebake: rebake(
            settings,
            args.force || context.trigger == Some(hushrun_core::run::Trigger::Schedule),
        ),
        deps_only: args.deps_only,
    };
    let mut published = bake_image::publish(
        out,
        &written.baked,
        &written.pack_groups,
        &written.hint_groups,
        target,
        signing,
        args.compact,
        &aside.join("staging"),
        &publishing,
    )
    .map_err(failed)?;
    if let Err(why) = latest
        && !matches!(target, bake_image::Target::Hosted(_))
    {
        published.notes.push(format!(
            "{} moves only from CI on the default branch, and this bake runs {why}",
            hushrun_core::blocks::image::latest_tag(&written.baked.platform)
        ));
    }
    Ok(published)
}

/// B-65: how old the image's workspace layer may grow before a bake writes it again: hushrun.yaml's
/// `build: baked: compact_every`, else a week; `0s` turns the age rule off.
fn compact_every_ms(settings: &hushrun_core::policy::BakeSettings) -> Option<u64> {
    let every = settings
        .compact_every
        .map_or(hushrun_artifact::image::COMPACT_EVERY_MS, |d| d.millis());
    (every > 0).then_some(every)
}

/// B-73: the re-bake limit, from hushrun.yaml's `build: baked: rebake_every` (an hour by default,
/// `0s` turns it off) and `rebake_blocks` (50); none with `--force`.
fn rebake(
    settings: &hushrun_core::policy::BakeSettings,
    force: bool,
) -> Option<crate::cmd::build_relay::bake_image::Rebake> {
    use crate::cmd::build_relay::bake_image::{REBAKE_BLOCKS, REBAKE_EVERY_MS, Rebake};
    let every_ms = settings
        .rebake_every
        .map_or(REBAKE_EVERY_MS, |d| d.millis());
    (!force && every_ms > 0).then(|| Rebake {
        every_ms,
        blocks: settings.rebake_blocks.unwrap_or(REBAKE_BLOCKS),
    })
}

/// The branch checked out in `folder`, as git names it; `None` on a detached head.
fn current_branch(folder: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|b| !b.is_empty())
}

/// The image a bake published, as `--format json` writes it.
fn image_json(published: &crate::cmd::build_relay::bake_image::Published) -> Value {
    let uploaded = |digest: &str| {
        published
            .pushed
            .blobs
            .iter()
            .any(|b| b.digest == digest && b.uploaded)
    };
    // B-73: nothing published, and why.
    if let Some(why) = &published.skipped {
        return json!({
            "reference": published.reference,
            "state": "skipped",
            "skipped": why,
            "tags": [],
            "uploaded_bytes": 0,
        });
    }
    json!({
        "reference": published.reference,
        "state": "published",
        "digest": published.digest.to_string(),
        "tags": published.tags,
        "layers": published.layers.iter().map(|(role, digest, size, files, reused)| json!({
            "role": role.word(),
            "digest": digest.to_string(),
            "size": size,
            "files": files,
            "reused": reused,
            "uploaded": uploaded(&digest.to_string()),
        })).collect::<Vec<_>>(),
        "blobs": published.pushed.blobs,
        "uploaded_bytes": published.pushed.uploaded_bytes(),
        "stale": published.stale,
        "compacted": published.compacted,
        "signed": published.signed,
        "notes": published.notes,
        "warnings": published.warnings,
        "hosted": published.hosted.as_ref().map(|h| json!({
            "image": h.image,
            "state": h.state,
            "published_before": h.before,
            "sent_bytes": h.sent,
            "requests": h.tries.requests,
            "sent_total_bytes": h.tries.bytes,
            "replanned": h.tries.replanned,
        })),
    })
}

/// What a bake says of the image it published: its reference and tags, what was uploaded, and
/// the Dockerfile line that takes it.
fn image_text(text: &mut String, published: &crate::cmd::build_relay::bake_image::Published) {
    if let Some(why) = &published.skipped {
        let _ = writeln!(text, "Not published to {}: {why}.", published.reference);
        return;
    }
    if let Some(hosted) = &published.hosted {
        return hosted_text(text, published, hosted);
    }
    let layers: Vec<String> = published
        .layers
        .iter()
        .map(|(role, _, size, files, reused)| {
            format!(
                "{} {} ({} files{})",
                role.word(),
                self::size(*size),
                files,
                if *reused { ", kept" } else { "" }
            )
        })
        .collect();
    let uploaded: Vec<String> = published
        .pushed
        .blobs
        .iter()
        .filter(|b| b.uploaded)
        .map(|b| format!("{} {}", b.role, self::size(b.size)))
        .collect();
    let _ = writeln!(
        text,
        "Published {}: layers {}. Uploaded {} ({}).",
        published.reference,
        layers.join(", "),
        self::size(published.pushed.uploaded_bytes()),
        if uploaded.is_empty() {
            "nothing new".to_string()
        } else {
            uploaded.join(", ")
        }
    );
    if let Some(why) = &published.compacted {
        let _ = writeln!(text, "The workspace layer was written again: {why}.");
    } else if published.stale > 0 {
        let _ = writeln!(
            text,
            "The workspace layer keeps {} {} this build no longer uses, until it is compacted.",
            published.stale,
            if published.stale == 1 {
                "block"
            } else {
                "blocks"
            }
        );
    }
    match &published.signed {
        Some(signer) => {
            let _ = writeln!(text, "Signed by {signer}.");
        }
        None => {
            let _ = writeln!(text, "Unsigned.");
        }
    }
    let repository = published
        .reference
        .split_once('@')
        .map_or(published.reference.as_str(), |(r, _)| r);
    let _ = writeln!(text, "Tags: {}.", published.tags.join(", "));
    for note in &published.notes {
        let _ = writeln!(text, "Note: {note}.");
    }
    if let Some(moving) = published.tags.last()
        && published.registry
    {
        let _ = writeln!(
            text,
            "A Dockerfile takes it with:\n  COPY --from={repository}:{moving} /hushrun /hushrun"
        );
    }
}

/// B-70: what a bake says of an image it published into the backend's registry, or of the newer
/// bake that won, and the Dockerfile line that takes it there.
fn hosted_text(
    text: &mut String,
    published: &crate::cmd::build_relay::bake_image::Published,
    hosted: &crate::cmd::build_relay::bake_image::HostedAnswer,
) {
    if hosted.state == "not_newer" {
        let _ = writeln!(text, "Not published to {}.", hosted.image);
        for note in &published.notes {
            let _ = writeln!(text, "Note: {note}.");
        }
        return;
    }
    let layers: Vec<String> = published
        .layers
        .iter()
        .map(|(role, _, size, files, reused)| {
            format!(
                "{} {} ({} files{})",
                role.word(),
                self::size(*size),
                files,
                if *reused { ", kept" } else { "" }
            )
        })
        .collect();
    let _ = writeln!(
        text,
        "Published {}{}: layers {}. Sent its description ({}) to the backend, which made the layers from the blocks it holds; no blob was pushed.",
        published.reference,
        if hosted.state == "unchanged" {
            " (the moving tag named it already)"
        } else {
            ""
        },
        layers.join(", "),
        self::size(hosted.sent),
    );
    if let Some(why) = &published.compacted {
        let _ = writeln!(text, "The workspace layer was written again: {why}.");
    } else if published.stale > 0 {
        let _ = writeln!(
            text,
            "The workspace layer keeps {} {} this build no longer uses, until it is compacted.",
            published.stale,
            if published.stale == 1 {
                "block"
            } else {
                "blocks"
            }
        );
    }
    match &published.signed {
        Some(signer) => {
            let _ = writeln!(text, "Signed by {signer}.");
        }
        None => {
            let _ = writeln!(text, "Unsigned.");
        }
    }
    let _ = writeln!(text, "Tags: {}.", published.tags.join(", "));
    for note in &published.notes {
        let _ = writeln!(text, "Note: {note}.");
    }
    if let Some(moving) = published.tags.last() {
        let host = hosted.image.split('/').next().unwrap_or_default();
        let _ = writeln!(
            text,
            "A Dockerfile takes it with:\n  COPY --from={}:{moving} /hushrun /hushrun\nafter `docker login {host} -u hushrun -p <pull token>` (a read-only token an owner or admin makes for this repository).",
            hosted.image
        );
    }
}

/// The checks `--check` names, by id (B-26).
fn named_checks(repository: Option<&str>, names: &[String]) -> Vec<hushrun_core::blocks::Hex32> {
    names
        .iter()
        .map(|name| crate::cmd::build_session::check_id(repository, name))
        .collect()
}

/// B-63: the checks a bake or `build manifest` asks a backend for: those `--check` named, else
/// every check hushrun.yaml names and every check CI's files run through `hushrun run NAME --`
/// on the default branch (B-75, [`ci_pipelines`]), else none in particular (the repository's
/// newest build).
fn backend_checks(
    repository: Option<&str>,
    named: &[hushrun_core::blocks::Hex32],
    content: &crate::context::Content,
) -> Vec<Option<hushrun_core::blocks::Hex32>> {
    if !named.is_empty() {
        return named.iter().copied().map(Some).collect();
    }
    let names = backend_check_names(content);
    if names.is_empty() {
        vec![None]
    } else {
        names
            .iter()
            .map(|name| Some(crate::cmd::build_session::check_id(repository, name)))
            .collect()
    }
}

/// B-63: the names of the checks a bake asks a backend for when `--check` names none: every
/// check hushrun.yaml names and every check CI's files run through `hushrun run NAME --` on the
/// default branch.
fn backend_check_names(content: &crate::context::Content) -> Vec<String> {
    let mut names: Vec<String> = content
        .policy
        .as_ref()
        .map(|policy| {
            policy
                .checks
                .keys()
                .map(|name| name.as_ref().to_string())
                .collect()
        })
        .unwrap_or_default();
    let branch = default_branch(content);
    for (name, _) in
        crate::cmd::setup::replay::replays(&ci_pipelines(content), branch.as_deref(), &[], None)
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The repository's default branch: CI's name for it, else the origin remote's.
fn default_branch(content: &crate::context::Content) -> Option<String> {
    hushrun_ci::context(&hushrun_ci::Env::from_process())
        .source
        .and_then(|s| s.default_branch)
        .or_else(|| Some(crate::cmd::setup::default_branch(content.vcs.root())))
}

/// CI's files at the commit checked out, as `hushrun ready` reads them.
fn ci_pipelines(content: &crate::context::Content) -> Vec<crate::cmd::setup::workflows::Workflow> {
    let read = |path: &str| {
        content
            .listing
            .find(path)
            .and_then(|entry| content.vcs.read_entry(entry).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };
    crate::cmd::setup::workflows::pipelines_among(
        content.listing.entries.iter().map(|e| e.path.as_str()),
        read,
    )
}

/// `hushrun build manifest` (B-68): the newest manifests of this repository's compiled code, as
/// a bake would read them now, every check's (or those named), the store's identity, its checks'
/// parts and platform, and the tags an image of it gets. Nothing is written or downloaded. The
/// identity is the one a bake writes: the same checks, from the same function.
fn manifest(global: &GlobalOpts, place: &Place, args: &BuildManifestArgs) -> CmdResult {
    use crate::cmd::build_relay::baked as write;
    use crate::context::{Content, Source};
    use hushrun_build::baked;
    use hushrun_build::model::Kind;
    use hushrun_core::blocks::image as tags;
    let failed = |why: String| Failure::new(Exit::Unexpected, anyhow!("build manifest: {why}"));
    let refused = |why: String| Failure::new(Exit::Usage, anyhow!("build manifest: {why}"));
    let in_ci = crate::gitlab_job::in_ci(&hushrun_ci::Env::from_process());
    let env = |name: &str| hushrun_ci::var_os(name);
    let folder = std::env::current_dir().map_err(|err| failed(err.to_string()))?;
    let content = Content::open(global, Source::Repo(hushrun_vcs::Snapshot::Worktree))
        .map_err(|f| f.error)
        .map_err(|err| failed(format!("{err:#}")))?;
    let repository = hushrun_ci::context(&hushrun_ci::Env::from_process())
        .repository
        .or_else(|| crate::cmd::wrap::repository_of(&content));
    let named = named_checks(repository.as_deref(), &args.checks);
    let checkout = crate::cmd::build_session::checkout_of(&folder, &env);
    let (rustc_host, _) = toolchains(&folder);
    let target = crate::cmd::build_session::platform();
    let (store, held) =
        run_store(place, in_ci).ok_or_else(|| failed("there is no store here".into()))?;
    if args.from == BakeFrom::Store
        && let Some(why) = own_store_refusal(held.as_ref())
    {
        return Err(Failure::new(Exit::Usage, anyhow!("build manifest: {why}")));
    }
    let _held = held;
    let store = Store::new(&store);
    let (checks, blocks): (Vec<baked::BakedCheck>, Vec<usize>) = match args.from {
        BakeFrom::Store => {
            let saved = hushrun_build::manifests::of_checks(
                &store,
                repository.as_deref(),
                checkout.to_str(),
                &named,
                &target,
            );
            if saved.is_empty() {
                return Err(refused(format!(
                    "no run of {} here has left a manifest for {target}",
                    repository.as_deref().unwrap_or("this repository")
                )));
            }
            let checks = write::checks_of_store(&saved, rustc_host.as_deref());
            let blocks = saved
                .iter()
                .flat_map(|s| s.parts.iter())
                .filter(|p| baked::KINDS.iter().any(|k| k.word() == p.kind))
                .map(|p| p.pairs.len())
                .collect();
            (checks, blocks)
        }
        BakeFrom::Backend => {
            let access = match crate::results::Backend::select(Some(&folder)) {
                Ok(crate::results::Selection::On(backend)) => backend.build_access(),
                _ => None,
            }
            .filter(|access| access.sends())
            .ok_or_else(|| refused("no backend is signed in to".into()))?;
            let config = crate::cmd::build_relay::Config {
                store: store.clone(),
                caller: crate::cmd::build_session::caller(Some(&access), in_ci),
                access,
                repository: repository.clone(),
                commit: None,
                platform: target.clone(),
                compiler: Some((std::ffi::OsString::from("rustc"), folder.clone())),
                local: Mode::On,
                own: Mode::On,
                min_free_gb: settings::MIN_FREE_GB,
                keep_days: hushrun_build::KEEP_DAYS,
                tuning: crate::cmd::build_relay::Tuning::default(),
                saved: None,
                open_decides: false,
            };
            let relay =
                std::sync::Arc::new(crate::cmd::build_relay::Relay::start(config).map_err(failed)?);
            let mut wanted = vec![(Kind::Rustc, rustc_host.clone().unwrap_or(target.clone()))];
            if let Some(platform) = hushrun_build::go::probe::env_of(&folder) {
                wanted.push((Kind::Go, platform));
            }
            if let Some(cc) = cc_target(&folder) {
                wanted.push((Kind::Cc, cc));
            }
            let checks_asked = backend_checks(repository.as_deref(), &named, &content);
            let manifests = relay.newest_parts(&checks_asked, &wanted).map_err(failed)?;
            let blocks = manifests
                .iter()
                .filter(|(_, _, _, pairs)| !pairs.is_empty())
                .map(|(_, _, _, pairs)| pairs.len())
                .collect();
            (write::checks_of_backend(&manifests), blocks)
        }
    };
    let identity = baked::store_manifest(&checks).to_string();
    let platform = baked::platform_of(rustc_host.as_deref().unwrap_or(&target));
    let mut counts = blocks.into_iter();
    let value = json!({
        "schema": "hushrun.build-manifest/v1",
        "source": match args.from { BakeFrom::Store => "store", BakeFrom::Backend => "backend" },
        "repository": repository,
        "manifest": identity,
        "short": tags::short_manifest(&identity),
        "platform": platform,
        "checks": checks.iter().map(|c| json!({
            "check": c.check.map(|c| c.to_string()),
            "parts": c.parts.iter().map(|p| json!({
                "kind": p.kind,
                "platform": p.platform,
                "manifest": p.manifest.to_string(),
                "blocks": counts.next().unwrap_or(0),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "tags": {
            "fixed": tags::fixed_tag(&platform, &identity),
            "latest": tags::latest_tag(&platform),
        },
    });
    if args.json || global.format == Format::Json {
        output::print_json(&value)?;
    } else if global.quiet == 0 {
        let mut text = format!(
            "The newest manifests of {} for {platform} are {}, of {} {}.\n",
            repository.as_deref().unwrap_or("this repository"),
            identity,
            checks.len(),
            if checks.len() == 1 { "check" } else { "checks" }
        );
        for (check, entry) in checks
            .iter()
            .zip(value["checks"].as_array().into_iter().flatten())
        {
            let _ = writeln!(
                text,
                "  check {}:",
                check
                    .check
                    .map_or_else(|| "(any)".to_string(), |c| c.to_string()[..12].to_string())
            );
            for part in entry["parts"].as_array().into_iter().flatten() {
                let _ = writeln!(
                    text,
                    "    {} for {}: {}, {} blocks",
                    part["kind"].as_str().unwrap_or_default(),
                    part["platform"].as_str().unwrap_or_default(),
                    part["manifest"].as_str().unwrap_or_default(),
                    part["blocks"]
                );
            }
        }
        let _ = writeln!(
            text,
            "An image of it is tagged {} and, once published from the default branch, {}.",
            tags::fixed_tag(&platform, &identity),
            tags::latest_tag(&platform)
        );
        output::print(&text)?;
    }
    Ok(Exit::Ok)
}

/// What `rustc -vV` says in `folder`: its `host:`, and for `baked.json` its first line and host.
fn toolchains(folder: &std::path::Path) -> (Option<String>, Vec<String>) {
    let rustc = hushrun_ci::var_os("RUSTC")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| std::ffi::OsString::from("rustc"));
    let Ok(out) = std::process::Command::new(rustc)
        .arg("-vV")
        .current_dir(folder)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return (None, Vec::new());
    };
    if !out.status.success() {
        return (None, Vec::new());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().map(str::to_string);
    let host = text
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|h| h.trim().to_string());
    let line = match (&first, &host) {
        (Some(first), Some(host)) => vec![format!("{first}; host: {host}")],
        (Some(first), None) => vec![first.clone()],
        _ => Vec::new(),
    };
    (host, line)
}

/// The C compiler's target triple in `folder` (`CC`, else `cc`, `-dumpmachine`), as the launcher
/// names its blocks' platform (B-60). `None` with no C compiler.
fn cc_target(folder: &std::path::Path) -> Option<String> {
    let cc = hushrun_ci::var_os("CC")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| std::ffi::OsString::from("cc"));
    let out = std::process::Command::new(cc)
        .arg("-dumpmachine")
        .current_dir(folder)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && hushrun_core::blocks::is_platform(&text)).then_some(text)
}

/// `go version` in a Go module's `folder`, for `baked.json`.
fn go_version(folder: &std::path::Path) -> Option<String> {
    hushrun_build::go::probe::env_of(folder)?;
    let out = std::process::Command::new("go")
        .arg("version")
        .current_dir(folder)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `hushrun build share on|observe|off` (B-33): saves the share setting, under the settings
/// file's lock.
fn set_share(global: &GlobalOpts, place: &Place, mode: Mode) -> CmdResult {
    let mut before = None;
    let saved = settings::update(&place.settings, |settings| {
        before = Some(settings.share);
        settings.share = mode;
    })
    .map_err(settings_failure)?;
    let changed = before != Some(mode);
    let overridden = place
        .overrides()
        .and_then(|o| o.share)
        .filter(|share| *share != saved.share);
    if global.format == Format::Json {
        output::print_json(&json!({
            "share": saved.share.as_str(),
            "changed": changed,
            "settings": place.settings,
            "overridden_by": overridden.map(|_| "HUSHRUN_BUILD_SHARE"),
        }))?;
        return Ok(Exit::Ok);
    }
    if global.quiet > 0 {
        return Ok(Exit::Ok);
    }
    let already = if changed { "" } else { "already " };
    let mut text = match mode {
        Mode::On => format!(
            "Sharing is {already}on here: when the org shares compiled crates, this machine shares as the org's mode says.\n"
        ),
        Mode::Observe => format!(
            "Sharing is {already}in observe here: this machine uploads what it builds and compares what it downloads, and reuses no download.\n"
        ),
        Mode::Off => format!(
            "Sharing is {already}off here: nothing goes to or comes from the org's shared store. Reuse on this machine is as the mode says.\n"
        ),
    };
    if let Some(other) = overridden {
        let _ = writeln!(
            text,
            "HUSHRUN_BUILD_SHARE={} overrides it for commands run from this shell.",
            other.as_str()
        );
    }
    output::print(&text)?;
    Ok(Exit::Ok)
}

/// What a `hushrun run NAME -- COMMAND` from this shell does (B-25), as far as this machine can
/// tell without asking the backend: the one function's answer, the org taken as sharing at the
/// most it can. A CI job with no backend to ask has nothing to share with.
fn run_here(
    place: &Place,
    effective: &settings::Settings,
) -> (
    hushrun_build::share::Does,
    Option<crate::results::BuildAccess>,
) {
    use hushrun_build::share::{Caller, Does, Off, Org, Sharing, what_a_run_does};
    let root = std::env::current_dir().ok();
    let access = match crate::results::Backend::select(root.as_deref()) {
        Ok(crate::results::Selection::On(backend)) => backend.build_access(),
        _ => None,
    };
    let in_ci = crate::gitlab_job::in_ci(&hushrun_ci::Env::from_process());
    if in_ci && access.is_none() {
        let none = Does {
            local: Mode::Off,
            sharing: Sharing::Off(Off::SignedOut),
            uploads: false,
        };
        return (none, None);
    }
    let caller = crate::cmd::build_session::caller(access.as_ref(), in_ci);
    // In a sandbox nobody chose a mode: reuse is on unless the settings file says `off`, and
    // `HUSHRUN_BUILD` sets this shell's over either, as the run reads them.
    let local = match caller {
        Caller::Agent { sandbox: true } => place
            .overrides()
            .and_then(|o| o.mode)
            .or_else(|| settings::sandbox_mode(&place.settings).ok())
            .unwrap_or(Mode::Off),
        _ => effective.mode,
    };
    let does = what_a_run_does(caller, local, effective.share, Org::Shares(None));
    (does, access)
}

/// The `ci` line of `hushrun build status` (B-2, B-31): what `hushrun run` does in this CI job,
/// and why, from [`run_here`].
fn ci_here(place: &Place, effective: &settings::Settings) -> String {
    use hushrun_build::share::{Off, Sharing};
    let (does, access) = run_here(place, effective);
    match (does.local, does.sharing) {
        (Mode::Off, Sharing::Off(off)) => {
            let hint = match off {
                Off::NoIdToken => crate::cmd::build_session::id_token_hint(access.as_ref()),
                _ => String::new(),
            };
            format!(
                "this is CI, where `hushrun run` reuses nothing here: {}{hint}",
                off.reason()
            )
        }
        _ if !does.uploads => "this is CI: `hushrun run NAME -- COMMAND` reuses in the job's own store when the org shares, reading the org's blocks and uploading none (an agent token outside GitHub Actions and GitLab CI); no person's settings are read".to_string(),
        _ => "this is CI: `hushrun run NAME -- COMMAND` reuses in the job's own store when the org shares, and shares as the org's mode says; no person's settings are read".to_string(),
    }
}

/// What sharing does here, as far as this machine can tell without asking the backend (B-25):
/// the word, and why.
fn sharing_here(place: &Place, effective: &settings::Settings) -> (&'static str, String) {
    use hushrun_build::share::Sharing;
    let (does, _) = run_here(place, effective);
    match does.sharing {
        Sharing::Off(off) => ("off", off.reason().to_string()),
        Sharing::Observe => (
            "observe",
            "at most: uploads what it builds and compares what it downloads, when the org shares"
                .to_string(),
        ),
        Sharing::On => (
            "on",
            "at most: the org's mode decides, observe or on, when the org shares".to_string(),
        ),
    }
}

/// Where this person's build settings and store are, and what this shell's variables change.
struct Place {
    /// `build.json`, beside the credentials.
    settings: PathBuf,
    /// The store's folder; `None` when hushrun has no cache directory, so nothing is kept.
    store: Option<PathBuf>,
    /// `HUSHRUN_BUILD`, `HUSHRUN_BUILD_KEEP_DAYS` and `HUSHRUN_BUILD_DIR`, or why they can't be
    /// used, which turns reuse off in `hushrun run`.
    overrides: Result<Overrides, SettingsError>,
}

impl Place {
    fn read() -> Result<Place, Failure> {
        let settings = Credentials::build_settings_path().ok_or_else(|| {
            Failure::new(
                Exit::Unexpected,
                anyhow!(
                    "there is no configuration directory to keep build settings in: set HUSHRUN_CREDENTIALS, and build.json goes beside it"
                ),
            )
        })?;
        // As `hushrun run` reads them, so both see one thing.
        let env = |name: &str| hushrun_ci::var_os(name);
        let overrides = Overrides::from(&env);
        let store = session::store_dir(
            context::cache_dir().as_deref(),
            overrides.as_ref().unwrap_or(&Overrides::default()),
        );
        Ok(Place {
            settings,
            store,
            overrides,
        })
    }

    /// The variables, when they can be used.
    fn overrides(&self) -> Option<&Overrides> {
        self.overrides.as_ref().ok()
    }

    /// `HUSHRUN_BUILD`, when it sets a mode other than `saved`.
    fn mode_override(&self, saved: Mode) -> Option<Mode> {
        self.overrides()
            .and_then(|o| o.mode)
            .filter(|mode| *mode != saved)
    }
}

/// What `hushrun build status` says first, and `hushrun guide` says under `## Here` (C-28): the
/// mode a `hushrun run` from this shell takes on a person's machine, and what the store holds.
pub(crate) struct Here {
    /// The mode, `HUSHRUN_BUILD` applied; or why the settings file can't be read, which turns
    /// reuse off in `hushrun run`.
    pub mode: Result<Mode, String>,
    /// The mode the settings file names, when `HUSHRUN_BUILD` overrides it.
    pub saved: Option<Mode>,
    /// `build.json`.
    pub settings: PathBuf,
    /// The store's folder; `None` when hushrun has no cache directory.
    pub store: Option<PathBuf>,
    /// What the store holds.
    pub held: Option<Status>,
    /// Whether this is CI, where no person's settings are read.
    pub in_ci: bool,
}

impl Here {
    /// This machine's, as `hushrun build status` reads it; `None` with no configuration
    /// directory to keep the settings in.
    pub(crate) fn read(global: &GlobalOpts) -> Option<Here> {
        let place = Place::read().ok()?;
        Some(Here::of(global, &place, &settings::load(&place.settings)))
    }

    fn of(
        global: &GlobalOpts,
        place: &Place,
        saved: &Result<settings::Settings, SettingsError>,
    ) -> Here {
        let (mode, overridden) = match saved {
            Ok(saved) => match place.mode_override(saved.mode) {
                Some(mode) => (Ok(mode), Some(saved.mode)),
                None => (Ok(saved.mode), None),
            },
            Err(err) => (Err(err.to_string()), None),
        };
        Here {
            mode,
            saved: overridden,
            settings: place.settings.clone(),
            store: place.store.clone(),
            held: place
                .store
                .as_deref()
                .map(|store| Store::new(store).status(now(global))),
            // B-48: a GitLab job is CI here too, whatever else says so.
            in_ci: crate::gitlab_job::in_ci(&hushrun_ci::Env::from_process()),
        }
    }

    /// The store and its size, in words: `<folder>, empty`, `<folder>, 118 MB in 36 blocks`.
    pub(crate) fn store_words(&self) -> String {
        match (&self.store, &self.held) {
            (None, _) => "none: HUSHRUN_CACHE_DIR is empty, so nothing is kept".to_string(),
            (Some(store), Some(held)) if held.blocks > 0 => format!(
                "{}, {} in {}",
                store.display(),
                size(held.bytes),
                blocks(held.blocks)
            ),
            (Some(store), _) => format!("{}, empty", store.display()),
        }
    }
}

/// `hushrun build on|observe|off`: saves the mode, under the settings file's lock.
fn set(global: &GlobalOpts, place: &Place, mode: Mode) -> CmdResult {
    let mut before = None;
    let saved = settings::update(&place.settings, |settings| {
        before = Some(settings.mode);
        settings.choose(mode);
    })
    .map_err(settings_failure)?;
    let changed = before != Some(mode);
    let effective = place
        .overrides()
        .map_or_else(|| saved.clone(), |o| saved.with(o));
    if global.format == Format::Json {
        output::print_json(&json!({
            "mode": saved.mode.as_str(),
            "changed": changed,
            "settings": place.settings,
            "store": place.store,
            "keep_days": effective.keep_days,
            "overridden_by": place.mode_override(saved.mode).map(|_| "HUSHRUN_BUILD"),
        }))?;
        return Ok(Exit::Ok);
    }
    if let Err(err) = &place.overrides {
        eprintln!("warning: {err}: `hushrun run` reuses nothing while it is set");
    }
    if global.quiet > 0 {
        return Ok(Exit::Ok);
    }
    let already = if changed { "" } else { "already " };
    let days = effective.keep_days;
    let mut text = String::new();
    match (mode, &place.store) {
        (Mode::On, Some(store)) => {
            let _ = writeln!(
                text,
                "Build reuse is {already}on: commands run by `hushrun run` reuse compiled crates kept in {}.",
                store.display()
            );
            let _ = writeln!(text, "A block not used for {days} days is deleted.");
        }
        (Mode::Observe, Some(store)) => {
            let _ = writeln!(
                text,
                "Build reuse is {already}in observe: everything still compiles, and each `hushrun run` says what would have been reused."
            );
            let _ = writeln!(
                text,
                "Compiled crates are kept in {}, and a block not used for {days} days is deleted.",
                store.display()
            );
        }
        (Mode::On | Mode::Observe, None) => {
            let word = if mode == Mode::On { "on" } else { "in observe" };
            let _ = writeln!(text, "Build reuse is {already}{word}.");
            let _ = writeln!(
                text,
                "hushrun has no cache directory here (HUSHRUN_CACHE_DIR is empty), so `hushrun run` keeps and reuses nothing."
            );
        }
        (Mode::Off, store) => {
            let _ = writeln!(
                text,
                "Build reuse is {already}off: nothing is reused or stored."
            );
            if let Some(store) = store {
                let _ = writeln!(
                    text,
                    "{} is left as it is: `hushrun build clean` empties it.",
                    store.display()
                );
            }
        }
    }
    if let Some(other) = place.mode_override(saved.mode) {
        let _ = writeln!(
            text,
            "HUSHRUN_BUILD={} overrides it for commands run from this shell.",
            other.as_str()
        );
    }
    output::print(&text)?;
    Ok(Exit::Ok)
}

/// `hushrun build status`, and `hushrun build` alone.
fn status(global: &GlobalOpts, place: &Place) -> CmdResult {
    let saved = settings::load(&place.settings);
    let effective = saved.as_ref().ok().map(|saved| {
        place
            .overrides()
            .map_or_else(|| saved.clone(), |o| saved.with(o))
    });
    let here = Here::of(global, place, &saved);
    let held = &here.held;
    let in_ci = here.in_ci;
    // B-63: the baked store this shell's variables name, as a run here would read it.
    let content = crate::context::Content::open(
        global,
        crate::context::Source::Repo(hushrun_vcs::Snapshot::Worktree),
    )
    .ok();
    let repository = content.as_ref().and_then(|content| {
        hushrun_ci::context(&hushrun_ci::Env::from_process())
            .repository
            .or_else(|| crate::cmd::wrap::repository_of(content))
    });
    let signers = content
        .as_ref()
        .and_then(|content| content.policy.as_ref())
        .map(|policy| policy.build_baked.clone())
        .unwrap_or_default();
    let lower_store = run_store(place, in_ci);
    let lower = lower_store.as_ref().map(|(store, _)| {
        let folder = std::env::current_dir().ok();
        let go = folder.as_deref().and_then(hushrun_build::go::probe::env_of);
        let check = store
            .join("tmp")
            .join(hushrun_build::baked::unique("status"));
        let (state, unsigned) = crate::cmd::build_session::lower_state(
            &|name| hushrun_ci::var_os(name),
            in_ci,
            store,
            folder.as_deref(),
            repository.as_deref(),
            go.as_deref(),
            hushrun_build::baked::Asked::NotAsked,
            &signers,
            &check,
        );
        let _ = std::fs::remove_dir_all(&check);
        (state, unsigned)
    });
    let wrapper = std::env::var_os(session::RUSTC_WRAPPER).filter(|v| !v.is_empty());
    // B-50: a C or C++ launcher of the person's, which `hushrun run` leaves as it is.
    let launchers: Vec<&str> = hushrun_build::cc::CMAKE_LAUNCHERS
        .iter()
        .copied()
        .filter(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()))
        .collect();
    // Settings `hushrun run` can't read turn reuse off there: a configuration error.
    let exit = if saved.is_err() {
        Exit::Usage
    } else {
        Exit::Ok
    };
    if global.format == Format::Json {
        output::print_json(&json!({
            "mode": effective.as_ref().map(|s| s.mode.as_str()),
            "saved_mode": saved.as_ref().ok().map(|s| s.mode.as_str()),
            "overridden_by": saved
                .as_ref()
                .ok()
                .and_then(|s| place.mode_override(s.mode))
                .map(|_| "HUSHRUN_BUILD"),
            "settings": place.settings,
            "error": saved.as_ref().err().map(ToString::to_string),
            "variables_error": place.overrides.as_ref().err().map(ToString::to_string),
            "store": place.store,
            "keep_days": effective.as_ref().map(|s| s.keep_days),
            "programs": effective.as_ref().map(|s| s.programs.as_str()),
            "share": effective.as_ref().map(|s| s.share.as_str()),
            "sharing": effective.as_ref().map(|s| {
                let (word, why) = sharing_here(place, s);
                json!({"mode": word, "why": why})
            }),
            "in_ci": in_ci,
            "rustc_wrapper_set": wrapper.is_some(),
            "cmake_launchers_set": launchers,
            "cc_maps": effective.as_ref().map(|s| s.cc_maps),
            "held": held.as_ref().map(held_json),
            "baked": lower_json(&lower.as_ref().map(|(state, _)| state.clone())),
            "baked_signature": lower.as_ref().and_then(|(_, why)| why.clone()),
        }))?;
        return Ok(exit);
    }
    let mut out = String::new();
    let mut line = |label: &str, value: String| {
        let _ = writeln!(out, "{label:<10}{value}");
    };
    match &here.mode {
        Ok(mode) => {
            line("mode", mode.as_str().to_string());
            if let Some(saved) = here.saved {
                line(
                    "",
                    format!(
                        "HUSHRUN_BUILD={} overrides the saved mode, {}, in this shell",
                        mode.as_str(),
                        saved.as_str()
                    ),
                );
            }
        }
        Err(err) => {
            line("mode", format!("unknown: {err}"));
            line(
                "",
                "`hushrun run` reuses nothing until it can read the settings".to_string(),
            );
        }
    }
    if let Err(err) = &place.overrides {
        line(
            "variable",
            format!("{err}: `hushrun run` reuses nothing while it is set"),
        );
    }
    if in_ci {
        // No person's settings are read in CI: what applies there is the job's, whatever the
        // file here says.
        line(
            "ci",
            ci_here(
                place,
                effective.as_ref().unwrap_or(&settings::Settings::default()),
            ),
        );
    }
    if let Some(value) = &wrapper {
        line(
            "wrapper",
            format!(
                "RUSTC_WRAPPER is {} in this shell, so `hushrun run` leaves it and reuses nothing",
                value.to_string_lossy()
            ),
        );
    }
    if !launchers.is_empty() {
        line(
            "launcher",
            format!(
                "{} {} set in this shell, so `hushrun run` leaves {} and reuses no C or C++ object of a build folder CMake configures from {}",
                launchers.join(" and "),
                if launchers.len() == 1 { "is" } else { "are" },
                if launchers.len() == 1 { "it" } else { "them" },
                if launchers.len() == 1 { "it" } else { "them" },
            ),
        );
    }
    if effective.as_ref().is_some_and(|s| !s.cc_maps) {
        line(
            "c/c++",
            "objects keep absolute paths (cc_maps: false), so each is reused only in its own checkout"
                .to_string(),
        );
    }
    line("settings", place.settings.display().to_string());
    let named = |set: bool, name: &str| {
        if set {
            format!(" ({name})")
        } else {
            String::new()
        }
    };
    match &place.store {
        Some(store) => line(
            "store",
            format!(
                "{}{}",
                store.display(),
                named(
                    place.overrides().is_some_and(|o| o.dir.is_some()),
                    "HUSHRUN_BUILD_DIR"
                )
            ),
        ),
        None => line(
            "store",
            "none: HUSHRUN_CACHE_DIR is empty, so nothing is kept".to_string(),
        ),
    }
    if let Some((state, unsigned)) = &lower
        && *state != hushrun_build::baked::State::Absent
    {
        line("baked", state.words());
        if let Some(why) = unsigned {
            line("baked signature", format!("not evidence here: {why}"));
        }
    }
    if let Some(effective) = &effective {
        line(
            "keeps",
            format!(
                "a block {} days after its last use{}",
                effective.keep_days,
                named(
                    place.overrides().is_some_and(|o| o.keep_days.is_some()),
                    "HUSHRUN_BUILD_KEEP_DAYS"
                )
            ),
        );
        let (word, why) = sharing_here(place, effective);
        line(
            "share",
            format!(
                "{} on this machine{}",
                effective.share.as_str(),
                named(
                    place.overrides().is_some_and(|o| o.share.is_some()),
                    "HUSHRUN_BUILD_SHARE"
                )
            ),
        );
        line("sharing", format!("{word}: {why}"));
        line(
            "programs",
            match effective.programs {
                ProgramsSetting::Follow => "follow sharing: tied while runs here share nothing; a run that shares gives each macOS program a debug bundle so it can be reused elsewhere",
                ProgramsSetting::Tied => "tied: a program is reused only in the target folder that built it",
            }
            .to_string(),
        );
    }
    if let Some(held) = held {
        if held.blocks == 0 {
            line("size", "empty".to_string());
        } else {
            line(
                "size",
                format!("{} in {}", size(held.bytes), blocks(held.blocks)),
            );
            line(
                "used",
                format!(
                    "{} today, {} this week, {} before",
                    held.used_today, held.used_this_week, held.used_before
                ),
            );
            for (n, usage) in held.by_repository.iter().enumerate() {
                let label = if n == 0 { "by repo" } else { "" };
                line(
                    label,
                    format!(
                        "{}: {} in {}",
                        usage.repository.as_deref().unwrap_or("no repository"),
                        size(usage.bytes),
                        blocks(usage.blocks)
                    ),
                );
            }
        }
        if held.damaged > 0 {
            line(
                "damaged",
                format!(
                    "{} whose record can't be read; `hushrun build clean` removes them",
                    blocks(held.damaged)
                ),
            );
        }
    }
    output::print(&out)?;
    Ok(exit)
}

/// `hushrun build clean`: empties the store.
fn clean(global: &GlobalOpts, place: &Place) -> CmdResult {
    // Without the variables, the store could be another folder than the one meant.
    if let Err(err) = &place.overrides {
        return Err(Failure::usage(anyhow!(
            "{err}: fix it or unset it, so that the right store is emptied. Nothing was removed"
        )));
    }
    let Some(root) = &place.store else {
        if global.format == Format::Json {
            output::print_json(
                &json!({"store": Value::Null, "removed_blocks": 0, "removed_bytes": 0}),
            )?;
        } else if global.quiet == 0 {
            output::print("There is no store: HUSHRUN_CACHE_DIR is empty, so nothing is kept.")?;
        }
        return Ok(Exit::Ok);
    };
    let store = Store::new(root);
    let before = store.status(now(global));
    store.clean().map_err(|err| {
        Failure::new(
            Exit::Unexpected,
            anyhow!("emptying {}: {err}", root.display()),
        )
    })?;
    let removed = before.blocks + before.damaged;
    if global.format == Format::Json {
        output::print_json(&json!({
            "store": root,
            "removed_blocks": removed,
            "removed_bytes": before.bytes,
        }))?;
    } else if global.quiet > 0 {
        // Said by the exit code alone.
    } else if removed == 0 {
        output::print(&format!("{} held no blocks.", root.display()))?;
    } else {
        output::print(&format!(
            "Removed {} ({}) from {}.",
            blocks(removed),
            size(before.bytes),
            root.display()
        ))?;
    }
    Ok(Exit::Ok)
}

/// A settings error, mapped to its exit code: a file hushrun can't read or understand is
/// configuration; one it can't write is unexpected.
fn settings_failure(err: SettingsError) -> Failure {
    let exit = match err {
        SettingsError::Invalid { .. } | SettingsError::Newer { .. } => Exit::Usage,
        SettingsError::Read { .. }
        | SettingsError::Write { .. }
        | SettingsError::Variable { .. } => Exit::Unexpected,
    };
    Failure::new(exit, err)
}

fn held_json(held: &Status) -> Value {
    serde_json::to_value(held).unwrap_or(Value::Null)
}

/// The command's time, as the store takes it.
fn now(global: &GlobalOpts) -> SystemTime {
    let ms = crate::cmd::token::now(global);
    u64::try_from(ms).map_or(SystemTime::UNIX_EPOCH, |ms| {
        SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
    })
}

/// "1 block", "372 blocks".
fn blocks(n: u64) -> String {
    if n == 1 {
        "1 block".to_string()
    } else {
        format!("{n} blocks")
    }
}

/// A size as people read it: bytes, KB, MB, GB or TB, in thousands as file managers count.
pub(crate) fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    // Tenths of the unit, in whole numbers.
    let mut unit = 0;
    let mut scaled = bytes;
    while scaled >= 1_000_000 && unit < UNITS.len() - 1 {
        scaled /= 1000;
        unit += 1;
    }
    let tenths = scaled / 100;
    if tenths < 100 {
        format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
    } else {
        format!("{} {}", tenths / 10, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::{SystemTime, baked_at, blocks, resolved, size};

    // covers: B-70
    #[test]
    fn oci_on_the_backend_s_registry_host_or_the_word_hushrun_publishes_through_the_backend() {
        use super::{BakeFrom, BuildBakeArgs, ImageAsk, image_target};
        let args = |oci: &str, from: BakeFrom, trusted: bool, branch_tag: bool| BuildBakeArgs {
            out: None,
            from,
            checks: Vec::new(),
            trusted,
            oci: Some(oci.into()),
            oci_dir: None,
            tar: None,
            erofs: None,
            signing_key: None,
            no_sign: false,
            branch_tag,
            compact: false,
            force: false,
            deps_only: false,
            report: false,
            no_build: false,
            command: Vec::new(),
            registry: crate::cli::RegistryOpts::default(),
        };
        let absolute = |p: &std::path::Path| p.to_path_buf();
        let hosted = Some("https://api.hushrun.com");
        let ask = |oci: &str, backend: Option<&str>| {
            image_target(
                &args(oci, BakeFrom::Backend, true, false),
                &absolute,
                backend,
            )
        };
        // The hosted backend's registry, by its host or by the word.
        for oci in ["registry.hushrun.com/acme/shop", "hushrun"] {
            assert!(matches!(
                ask(oci, hosted),
                Ok(Some(ImageAsk::Backend { .. }))
            ));
        }
        match ask("registry.hushrun.com/acme/shop", hosted) {
            Ok(Some(ImageAsk::Backend { given: Some(given) })) => {
                assert_eq!(given.path, "acme/shop");
            }
            _ => panic!("not the backend's registry"),
        }
        // A backend of one's own serves it on its own origin.
        assert!(matches!(
            ask(
                "hushrun.acme.dev/acme/shop",
                Some("https://hushrun.acme.dev")
            ),
            Ok(Some(ImageAsk::Backend { .. }))
        ));
        // Any other registry is pushed to as before.
        assert!(matches!(
            ask("ghcr.io/acme/shop", hosted),
            Ok(Some(ImageAsk::Ready(_)))
        ));
        // Refused before anything is baked: hushrun's registry for another backend, the word
        // with no backend, a store baked from this machine or untrusted blocks, a branch tag.
        for (oci, backend) in [
            (
                "registry.hushrun.com/acme/shop",
                Some("https://hushrun.acme.dev"),
            ),
            ("registry.hushrun.com/acme/shop", None),
            ("hushrun", None),
        ] {
            let err = ask(oci, backend).err().unwrap();
            assert_eq!(err.exit, crate::exit::Exit::Usage, "{oci}");
        }
        for (from, trusted, branch_tag) in [
            (BakeFrom::Store, false, false),
            (BakeFrom::Backend, false, false),
            (BakeFrom::Backend, true, true),
        ] {
            let err = image_target(
                &args("hushrun", from, trusted, branch_tag),
                &absolute,
                hosted,
            )
            .err()
            .unwrap();
            assert_eq!(err.exit, crate::exit::Exit::Usage);
        }
    }

    // covers: B-71
    // Two spellings of one file are one file: `..`, `.` and a linked folder resolved.
    #[test]
    fn the_same_file_spelled_two_ways_is_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("a")).unwrap();
        assert_eq!(
            resolved(&d.join("a/../same.bin")),
            resolved(&d.join("same.bin"))
        );
        assert_eq!(
            resolved(&d.join("x/../same.bin")),
            resolved(&d.join("same.bin"))
        );
        assert_eq!(resolved(&d.join("./a/./f")), resolved(&d.join("a/f")));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.join("a"), d.join("link")).unwrap();
            assert_eq!(resolved(&d.join("link/f")), resolved(&d.join("a/f")));
        }
        assert_ne!(resolved(&d.join("a/f")), resolved(&d.join("f")));
    }

    // covers: B-71
    // A SOURCE_DATE_EPOCH past what the clock holds is no time (it panicked); a fair one is it.
    #[test]
    fn a_source_date_epoch_past_the_clock_is_the_clock_s_time() {
        let env = |value: &'static str| {
            move |name: &str| (name == "SOURCE_DATE_EPOCH").then(|| value.into())
        };
        let fair = baked_at(false, &env("1700000000"));
        assert_eq!(
            fair,
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)
        );
        let before = SystemTime::now();
        for huge in ["18446744073709551615", "253402300800", "nope"] {
            let at = baked_at(false, &env(huge));
            assert!(at >= before, "{huge}");
        }
        // A signed bake's time is the clock's whatever the variable says.
        assert!(baked_at(true, &env("1700000000")) >= before);
    }

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(size(0), "0 bytes");
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1000), "1.0 KB");
        assert_eq!(size(1550), "1.5 KB");
        assert_eq!(size(12_345), "12 KB");
        assert_eq!(size(999_999), "999 KB");
        assert_eq!(size(1_000_000), "1.0 MB");
        assert_eq!(size(21_400_000_000), "21 GB");
        assert_eq!(size(3_200_000_000_000_000), "3200 TB");
        assert_eq!(blocks(1), "1 block");
        assert_eq!(blocks(2), "2 blocks");
    }
}
