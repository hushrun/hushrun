//! `hushrun packages` (K-6 to K-12, `docs/spec/11-baked-packages.md`): a package store of the
//! repository's downloaded packages, written by `bake` and read by each package manager beside
//! its own cache, in `hushrun run` and through `env` and `exec`.

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::anyhow;
use hushrun_core::blocks::Hex32;
use hushrun_packages::part::{self, Part, Reader};
use hushrun_packages::state::{self, Here, Signed, State};
use hushrun_packages::store::{self, Class, Depth, PartEntry};
use hushrun_packages::write::{PartMeta, StoreMeta};
use serde_json::json;

use crate::cli::{GlobalOpts, PackagesArgs, PackagesBakeArgs, PackagesCommand};
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::Format;

/// `hushrun packages` with a subcommand.
pub fn run(global: &GlobalOpts, args: &PackagesArgs) -> CmdResult {
    match args.command.clone().unwrap_or(PackagesCommand::Status) {
        PackagesCommand::Status => status(global),
        PackagesCommand::Env => env_command(global),
        PackagesCommand::Restore { parts } => restore(global, &parts),
        PackagesCommand::Verify => verify(),
        PackagesCommand::Bake(bake_args) => bake(global, &bake_args),
        PackagesCommand::Exec { command } => {
            // `exec` returns COMMAND's own code from `main`; this arm only runs without one.
            if command.is_empty() {
                return Err(Failure::usage(anyhow!(
                    "name the command: `hushrun packages exec -- COMMAND`"
                )));
            }
            Ok(Exit::Ok)
        }
    }
}

fn var(name: &str) -> Option<OsString> {
    hushrun_ci::var_os(name)
}

/// What a run here knows of itself, for the store's state.
pub(crate) struct Context {
    checkout: PathBuf,
    tracked: Vec<String>,
    repository: Option<String>,
    signers: hushrun_core::policy::BakedTrust,
    platform: String,
}

impl Context {
    fn here(global: &GlobalOpts) -> Context {
        let folder = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let checkout = crate::cmd::build_session::checkout_of(&folder, &var);
        let content = crate::context::Content::open(
            global,
            crate::context::Source::Repo(hushrun_vcs::Snapshot::Worktree),
        )
        .ok();
        let repository = hushrun_ci::context(&hushrun_ci::Env::from_process())
            .repository
            .or_else(|| content.as_ref().and_then(crate::cmd::wrap::repository_of));
        let signers = content
            .as_ref()
            .and_then(|c| c.policy.as_ref())
            .map(|p| p.build_baked.clone())
            .unwrap_or_default();
        Context {
            tracked: tracked(&checkout),
            checkout,
            repository,
            signers,
            platform: hushrun_build::baked::platform_of(&crate::cmd::build_session::platform()),
        }
    }

    /// K-12: the store's state for a run here, `may_write_trusted` its authority.
    fn state(&self, may_write_trusted: bool) -> State {
        let Some((folder, default)) = state::named(&var) else {
            return State::Absent;
        };
        let signed = signed(&folder, &self.signers);
        let inputs = |entry: &PartEntry| self.inputs(entry);
        state::state(
            Some(&folder),
            &Here {
                repository: self.repository.as_deref(),
                platform: &self.platform,
                signed: signed.as_ref(),
                untrusted_ok: state::accepts_untrusted(&var, may_write_trusted),
                may_write_trusted,
                default,
                inputs: &inputs,
            },
        )
    }

    /// K-2: a part's inputs as the checkout has them now, with the tool's line the bake wrote.
    fn inputs(&self, entry: &PartEntry) -> Option<Hex32> {
        let part = part::by_name(&entry.part)?;
        let manifests = hashed(&self.checkout, &part.manifests(&self.tracked));
        Some(store::inputs_digest(
            &entry.part,
            &entry.tool,
            &self.platform,
            &manifests,
        ))
    }
}

/// The paths git tracks in `checkout`, from its root.
fn tracked(checkout: &Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(checkout)
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    out.stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .filter_map(|p| std::str::from_utf8(p).ok().map(str::to_string))
        .collect()
}

/// Each of `paths` with the SHA-256 of its bytes; a file that can't be read is left out.
fn hashed(checkout: &Path, paths: &[String]) -> Vec<(String, Hex32)> {
    paths
        .iter()
        .filter_map(|p| Some((p.clone(), store::sha256_file(&checkout.join(p)).ok()?)))
        .collect()
}

/// K-7: the signature beside the store, verified against `signers`, as evidence.
fn signed(folder: &Path, signers: &hushrun_core::policy::BakedTrust) -> Option<Signed> {
    use crate::cmd::build_relay::bake_image;
    if signers.signers.is_empty() || !folder.join(store::SIGNATURE).is_file() {
        return None;
    }
    let root = bake_image::run_root(&var);
    let verified = bake_image::verify_packages(folder, &signers.signers, root.as_ref()).ok()?;
    bake_image::not_before_holds(
        signers.not_before,
        verified.signed_ms,
        i64::try_from(verified.baked_ms).ok(),
    )
    .ok()?;
    Some(Signed {
        signer: verified.signer,
        packages_json: verified.packages_json,
    })
}

/// What a store gives COMMAND: the variables of the parts read in place, the one word Maven's
/// part adds to `MAVEN_OPTS`, and the lines to say.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Given {
    pub(crate) vars: Vec<(OsString, OsString)>,
    pub(crate) maven: Option<String>,
    pub(crate) lines: Vec<String>,
}

impl Given {
    /// K-10: `env`, the variables a run gives COMMAND already, with the store's: its variables
    /// first, and Maven's word joined to the one `MAVEN_OPTS` there is, after the person's words
    /// and the build cache's.
    pub(crate) fn merged(&self, env: Vec<(OsString, OsString)>) -> Vec<(OsString, OsString)> {
        let mut out = self.vars.clone();
        let opts = OsString::from(hushrun_build::maven::OPTS_VARIABLE);
        let mut joined = false;
        for (name, value) in env {
            if name == opts
                && let Some(word) = &self.maven
            {
                out.push((
                    name,
                    maven_opts(Some(&value.to_string_lossy()), word).into(),
                ));
                joined = true;
            } else {
                out.push((name, value));
            }
        }
        if let (Some(word), false) = (&self.maven, joined) {
            let person = var(hushrun_build::maven::OPTS_VARIABLE);
            let person = person.as_ref().map(|v| v.to_string_lossy());
            out.push((opts, maven_opts(person.as_deref(), word).into()));
        }
        out
    }
}

/// `opts` with `word`, once.
fn maven_opts(opts: Option<&str>, word: &str) -> String {
    if opts.is_some_and(|o| o.split_whitespace().any(|w| w == word)) {
        return opts.unwrap_or_default().trim().to_string();
    }
    hushrun_packages::parts::maven::opts_with(opts, word)
}

/// K-10: what the store a run here reads gives COMMAND. `may_write_trusted` is the run's
/// authority. Nothing when no store is named.
pub(crate) fn given(global: &GlobalOpts, may_write_trusted: bool) -> Given {
    // K-11: no store named, nothing to read: one `stat`.
    if state::named(&var).is_none() {
        return Given::default();
    }
    let context = Context::here(global);
    given_by(&context, &context.state(may_write_trusted))
}

fn given_by(context: &Context, state: &State) -> Given {
    let mut given = Given {
        lines: state.lines(),
        ..Given::default()
    };
    let Some(lower) = state.lower() else {
        return given;
    };
    for read in &lower.parts {
        let Some(part) = part::by_name(&read.entry.part) else {
            given.lines.push(format!(
                "packages: {}: left alone: this hushrun has no reader for it",
                read.entry.part
            ));
            continue;
        };
        match part.reader(&read.dir, &context.checkout, &var) {
            Reader::InPlace(vars) => given
                .vars
                .extend(vars.into_iter().map(|(n, v)| (OsString::from(n), v))),
            Reader::MavenOpts(word) => given.maven = Some(word),
            // K-10: a seed is an install's, never a run's: COMMAND gets nothing from it.
            Reader::Seed => {}
            Reader::LeftAlone(why) => given
                .lines
                .push(format!("packages: {}: left alone: {why}", part.name())),
        }
    }
    given
}

/// The folder a `maven` part would be read from, for the `maven/1` cache's roots: an entry that
/// holds it stays on the machine, as one that holds `~/.m2` does. No check: one `stat`.
pub(crate) fn maven_part_dir(env: &dyn Fn(&str) -> Option<OsString>) -> Option<String> {
    let (folder, _) = state::named(env)?;
    let dir = store::part_dir(&folder, "maven");
    dir.is_dir().then(|| dir.to_string_lossy().into_owned())
}

fn in_ci() -> bool {
    crate::gitlab_job::in_ci(&hushrun_ci::Env::from_process())
}

fn status(global: &GlobalOpts) -> CmdResult {
    let context = Context::here(global);
    let state = context.state(in_ci());
    let parts: Vec<serde_json::Value> = state
        .lower()
        .map(|lower| {
            lower
                .parts
                .iter()
                .map(|p| {
                    json!({
                        "part": p.entry.part,
                        "class": p.entry.class.word(),
                        "files": p.rows.len(),
                        "bytes": p.entry.bytes,
                        "verified_by": match p.entry.verified_by {
                            store::VerifiedBy::Lockfile => "lockfile",
                            store::VerifiedBy::Signature => "signature",
                        },
                        "behind": p.behind,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if global.format == Format::Json {
        let out = json!({
            "schema": "hushrun.packages-status/v1",
            "state": state.word(),
            "store": state.lower().map(|l| l.folder.display().to_string()),
            "words": state.words(),
            "parts": parts,
        });
        println!("{out}");
        return Ok(Exit::Ok);
    }
    println!("baked packages: {}", state.words());
    if let Some(lower) = state.lower() {
        for p in &lower.parts {
            println!(
                "  {}: {} files, {}{}",
                p.entry.part,
                p.rows.len(),
                size(p.entry.bytes),
                if p.behind { ", behind" } else { "" }
            );
        }
        for note in &lower.notes {
            println!("  {note}");
        }
    }
    Ok(Exit::Ok)
}

/// `export NAME='value'`, with `'` written as `'\''`.
fn export_line(name: &OsString, value: &OsString) -> String {
    format!(
        "export {}='{}'",
        name.to_string_lossy(),
        value.to_string_lossy().replace('\'', r"'\''")
    )
}

fn env_command(global: &GlobalOpts) -> CmdResult {
    let mut given = Given::default();
    if state::named(&var).is_some() {
        let context = Context::here(global);
        let state = context.state(in_ci());
        given = given_by(&context, &state);
        given.lines.extend(seed_all(&context, &state, &[]));
    }
    for line in &given.lines {
        eprintln!("hushrun: {line}");
    }
    let vars = given.merged(Vec::new());
    if global.format == Format::Json {
        let map: serde_json::Map<String, serde_json::Value> = vars
            .iter()
            .map(|(n, v)| (n.to_string_lossy().into_owned(), v.to_string_lossy().into()))
            .collect();
        println!(
            "{}",
            json!({"schema": "hushrun.packages-env/v1", "variables": map})
        );
    } else {
        for (name, value) in &vars {
            println!("{}", export_line(name, value));
        }
    }
    Ok(Exit::Ok)
}

/// `hushrun packages exec -- COMMAND`: COMMAND with the store's readers, and its exit code. It
/// records nothing and looks nothing up.
pub fn exec(global: &GlobalOpts, command: &[String]) -> ExitCode {
    let Some((program, args)) = command.split_first() else {
        eprintln!("error: name the command: `hushrun packages exec -- COMMAND`");
        return Exit::Usage.into();
    };
    let mut given = Given::default();
    if state::named(&var).is_some() {
        let context = Context::here(global);
        let state = context.state(in_ci());
        given = given_by(&context, &state);
        given.lines.extend(seed_all(&context, &state, &[]));
    }
    for line in &given.lines {
        eprintln!("hushrun: {line}");
    }
    let status = std::process::Command::new(program)
        .args(args)
        .envs(given.merged(Vec::new()))
        .status();
    match status {
        Ok(status) => match status.code() {
            Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
            None => ExitCode::from(1),
        },
        Err(err) => {
            eprintln!("error: `{program}` couldn't start: {err}");
            ExitCode::from(127)
        }
    }
}

fn restore(global: &GlobalOpts, parts: &[String]) -> CmdResult {
    let context = Context::here(global);
    let state = context.state(in_ci());
    for line in state.lines() {
        eprintln!("hushrun: {line}");
    }
    let Some(lower) = state.lower() else {
        return Ok(Exit::Ok);
    };
    for read in &lower.parts {
        if !parts.is_empty() && !parts.contains(&read.entry.part) {
            continue;
        }
        if part::by_name(&read.entry.part)
            .is_some_and(|p| p.reader(&read.dir, &context.checkout, &var) == Reader::Seed)
        {
            continue;
        }
        println!(
            "packages: {}: read in place, nothing to seed ({} files)",
            read.entry.part,
            read.rows.len()
        );
    }
    for line in seed_all(&context, &state, parts) {
        println!("{line}");
    }
    Ok(Exit::Ok)
}

/// K-10, mechanism C: seeds every part of the store whose reader is a seed (of `only`, when it
/// names any) into the tool's own cache, and says what it did.
fn seed_all(context: &Context, state: &State, only: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(lower) = state.lower() else {
        return lines;
    };
    for read in &lower.parts {
        if !only.is_empty() && !only.contains(&read.entry.part) {
            continue;
        }
        let Some(part) = part::by_name(&read.entry.part) else {
            continue;
        };
        if part.reader(&read.dir, &context.checkout, &var) != Reader::Seed {
            continue;
        }
        let name = part.name();
        let (dest, under) = match part.seed_into(&context.checkout, &var) {
            Ok(found) => found,
            Err(why) => {
                lines.push(format!("packages: {name}: left alone: {why}"));
                continue;
            }
        };
        let admit = part.admit(&context.checkout, &part.manifests(&context.tracked));
        match hushrun_packages::seed::seed(read, under, &dest, &*admit) {
            Ok(done) if done.stamped => lines.push(format!(
                "packages: {name}: seeded already into {}",
                dest.display()
            )),
            Ok(done) => {
                lines.push(format!(
                    "packages: {name}: seeded {} files into {} ({} there already)",
                    done.copied,
                    dest.display(),
                    done.there
                ));
                if done.not_locked > 0 {
                    lines.push(format!(
                        "packages: {name}: left out {} files no lockfile here names now",
                        done.not_locked
                    ));
                }
                if !done.changed.is_empty() {
                    lines.push(format!(
                        "packages: {name}: left out {} files whose bytes changed since the bake, such as {}: {} downloads what they were of",
                        done.changed.len(),
                        done.changed[0],
                        part.tool()
                    ));
                }
            }
            Err(err) => lines.push(format!(
                "packages: {name}: seeding {} stopped: {err}",
                dest.display()
            )),
        }
    }
    lines
}

fn verify() -> CmdResult {
    let Some((folder, _)) = state::named(&var) else {
        println!(
            "baked packages: none ({} names none)",
            hushrun_packages::LOWER
        );
        return Ok(Exit::Ok);
    };
    let (packages, _) = store::Packages::read_bytes(&folder)
        .map_err(|why| Failure::new(Exit::VerifyFailed, anyhow!("{why}")))?;
    match store::check_contents(&folder, Some(&packages), Depth::Bytes) {
        Ok(rows) => {
            let files: usize = rows.iter().map(Vec::len).sum();
            println!(
                "baked packages: {}: every file is the one its row hashes ({} parts, {files} files)",
                folder.display(),
                packages.parts.len()
            );
            Ok(Exit::Ok)
        }
        Err(why) => Err(Failure::new(
            Exit::Unexpected,
            anyhow!("the baked packages {}: {why}", folder.display()),
        )),
    }
}

/// A size in the words of a line: bytes, KiB, MiB or GiB.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Where a bake fetches one part: the tool's cache, a folder of the bake's own, given to COMMAND.
fn fetch_cache(cache: &Path, part: &dyn Part) -> PathBuf {
    cache.join(part.name())
}

/// K-6: `hushrun packages bake`.
fn bake(global: &GlobalOpts, args: &PackagesBakeArgs) -> CmdResult {
    let context = Context::here(global);
    let fail = |why: String| Failure::new(Exit::Unexpected, anyhow!(why));
    let mut harvests: Vec<(&'static dyn Part, PathBuf)> = Vec::new();
    for given in &args.harvest {
        let (name, folder) = given.split_once('=').ok_or_else(|| {
            Failure::usage(anyhow!(
                "--harvest takes PART=FOLDER, such as gradle=$HOME/.gradle"
            ))
        })?;
        let part = part::by_name(name)
            .ok_or_else(|| Failure::usage(anyhow!("--harvest names {name}, which is no part")))?;
        harvests.push((part, PathBuf::from(folder)));
    }
    for name in &args.only {
        if part::by_name(name).is_none() {
            return Err(Failure::usage(anyhow!(
                "--only names {name}, which is no part"
            )));
        }
    }
    let wanted =
        |part: &dyn Part| args.only.is_empty() || args.only.iter().any(|o| o == part.name());
    // --tar alone bakes into a folder of its own beside the archive, removed after.
    let (out, scratch) = match (&args.out, &args.tar) {
        (Some(out), _) => (absolute(out), None),
        (None, Some(tar)) => {
            let tar = absolute(tar);
            let dir = tar.with_file_name(format!(
                ".{}.store",
                tar.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ));
            (dir.clone(), Some(Cleanup(vec![dir])))
        }
        (None, None) => return Err(Failure::usage(anyhow!("name --out DIR or --tar FILE"))),
    };
    // K-13's "nothing to do", on this machine: the store at --out holds the parts this bake would
    // take, each for the build files the checkout has now, from the same tool line.
    if !args.force
        && args.tar.is_none()
        && let Some(words) = already_baked(&context, &out, args, &harvests, &wanted)
    {
        println!("{words}");
        return Ok(Exit::Ok);
    }
    let lock = hushrun_build::baked::lock_bake(&out).map_err(|e| fail(e.to_string()))?;
    hushrun_build::baked::reconcile_marked(&out, &lock, store::PACKAGES_JSON)
        .map_err(|e| fail(e.to_string()))?;
    let new = hushrun_build::baked::new_folder(&out).map_err(|e| fail(e.to_string()))?;
    let cleanup = Cleanup(vec![new.clone()]);
    let mut lines = Vec::new();
    let mut metas = Vec::new();
    // The fetch: COMMAND, with each tool's cache at a folder of the bake's own (K-6).
    let mut fetched: Vec<(&'static dyn Part, PathBuf, String)> = Vec::new();
    if !args.command.is_empty() {
        let cache = hushrun_build::baked::new_folder(&out).map_err(|e| fail(e.to_string()))?;
        let _cache = Cleanup(vec![cache.clone()]);
        run_fetch(&context.checkout, &args.command, &cache).map_err(fail)?;
        let words = shell_words(&args.command);
        for part in part::ALL.iter().copied().filter(|p| wanted(*p)) {
            fetched.push((
                part,
                fetch_cache(&cache, part),
                format!("fetched by `{words}`"),
            ));
        }
        take_all(
            &context, &new, &fetched, &harvests, &wanted, &mut metas, &mut lines,
        )?;
    } else {
        // No fetch: each part the checkout has build files for and no harvest names is left out.
        for part in part::ALL.iter().copied().filter(|p| wanted(*p)) {
            if !part.manifests(&context.tracked).is_empty()
                && !harvests.iter().any(|(h, _)| h.name() == part.name())
            {
                lines.push(format!(
                    "packages: {}: left out: {}, and no --harvest names a {} cache",
                    part.name(),
                    if args.no_build {
                        "--no-build runs no build".to_string()
                    } else {
                        format!("no build was named to fetch with (`-- COMMAND`, such as the check that runs {})", part.tool())
                    },
                    part.tool()
                ));
            }
        }
        take_all(
            &context, &new, &fetched, &harvests, &wanted, &mut metas, &mut lines,
        )?;
    }
    if metas.is_empty() {
        for line in &lines {
            eprintln!("hushrun: {line}");
        }
        return Err(Failure::new(
            Exit::Unexpected,
            anyhow!(
                "packages: nothing to bake: name a fetch with `-- COMMAND`, the check's own command, or a filled cache with --harvest PART=FOLDER"
            ),
        ));
    }
    let baked_ms = baked_ms();
    let packages = hushrun_packages::write::seal(
        &new,
        &StoreMeta {
            repository: context.repository.clone(),
            platform: context.platform.clone(),
            baked_ms,
        },
        &metas,
        &|name, path| {
            let found = part::by_name(name);
            let inner = path.split_once('/').map_or(path, |(_, rest)| rest);
            (
                found.and_then(|p| p.package(inner)),
                found.and_then(|p| p.locked(inner)),
            )
        },
    )
    .map_err(|e| fail(e.to_string()))?;
    store::check_contents(&new, Some(&packages), Depth::Bytes).map_err(|why| fail(why.0))?;
    let total: u64 = packages.parts.iter().map(|p| p.bytes).sum();
    let most = args.max_bytes.unwrap_or(4 << 30);
    if total > most {
        let mut largest: Vec<&PartEntry> = packages.parts.iter().collect();
        largest.sort_by_key(|p| std::cmp::Reverse(p.bytes));
        let named: Vec<String> = largest
            .iter()
            .take(2)
            .map(|p| format!("{} ({})", p.part, size(p.bytes)))
            .collect();
        return Err(fail(format!(
            "packages: the store is {}, over --max-bytes {}; the largest parts are {}; --only names the parts to bake",
            size(total),
            size(most),
            named.join(" and ")
        )));
    }
    // K-6: signed as a baked store is (B-67): a key, the signing variables, a CI job's ID token,
    // or unsigned with a line.
    let signed_by = if args.no_sign {
        Err("--no-sign".to_string())
    } else {
        match crate::app::App::new(global)
            .and_then(|app| app.bake_signing(args.signing_key.as_deref(), in_ci()))
        {
            Ok(crate::signing::BakeSigning::Signed(signing)) => {
                crate::cmd::build_relay::bake_image::sign_packages(&new, &signing).map_err(fail)?;
                Ok(signing.describe())
            }
            Ok(crate::signing::BakeSigning::Unsigned(why)) => Err(why.to_string()),
            Err(failure) if args.signing_key.is_some() => return Err(failure),
            Err(failure) => Err(format!("{:#}", failure.error)),
        }
    };
    store::check_contents(&new, Some(&packages), Depth::Names).map_err(|why| fail(why.0))?;
    sync_tree(&new);
    hushrun_build::baked::put_in_place(&new, &out).map_err(|e| fail(e.to_string()))?;
    drop(cleanup);
    if let Some(tar) = &args.tar {
        let tar = absolute(tar);
        write_tar(&out, &tar).map_err(fail)?;
        lines.push(format!(
            "packages: wrote {}: `tar -x -C /` puts the store at /{}",
            tar.display(),
            hushrun_packages::tar::ROOT
        ));
    }
    let shown = if scratch.is_some() {
        args.tar.as_deref().map(absolute).unwrap_or_default()
    } else {
        out.clone()
    };
    for part in &packages.parts {
        lines.push(format!(
            "packages: {}: {} files, {}, from {} ({})",
            part.part,
            part.rows.count,
            size(part.bytes),
            part.tool,
            if part.manifests.is_empty() {
                "no build file".to_string()
            } else {
                part.manifests.join(", ")
            }
        ));
    }
    lines.push(match &signed_by {
        Ok(signer) => format!("packages: signed by {signer}"),
        Err(why) => format!(
            "packages: the store is unsigned ({why}): a run reads its parts only with HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1, outside CI"
        ),
    });
    lines.push(format!(
        "packages: baked {} parts, {}, to {}",
        packages.parts.len(),
        size(total),
        shown.display()
    ));
    if global.format == Format::Json {
        for line in &lines {
            eprintln!("hushrun: {line}");
        }
        let parts: Vec<serde_json::Value> = packages
            .parts
            .iter()
            .map(|p| json!({"part": p.part, "files": p.rows.count, "bytes": p.bytes, "tool": p.tool}))
            .collect();
        println!(
            "{}",
            json!({
                "schema": "hushrun.packages-bake/v1",
                "out": shown.display().to_string(),
                "identity": packages.identity.to_string(),
                "parts": parts,
            })
        );
    } else {
        let mut stdout = std::io::stdout().lock();
        for line in &lines {
            let _ = writeln!(stdout, "{line}");
        }
    }
    drop(lock);
    if scratch.is_some()
        && let (Some(parent), Some(name)) = (out.parent(), out.file_name())
    {
        let _ = fs::remove_file(parent.join(format!(".{}.bake.lock", name.to_string_lossy())));
    }
    drop(scratch);
    Ok(Exit::Ok)
}

/// K-6: the archive of the store at `folder`, written beside `tar` and renamed into place.
fn write_tar(folder: &Path, tar: &Path) -> Result<(), String> {
    let temp = tar.with_file_name(format!(
        ".{}.{}",
        hushrun_build::baked::unique("tar"),
        "part"
    ));
    let written = (|| {
        let mut file = std::io::BufWriter::new(fs::File::create(&temp)?);
        hushrun_packages::tar::write(folder, &mut file)?;
        file.into_inner().map_err(|e| e.into_error())?.sync_all()
    })();
    if let Err(err) = written.and_then(|()| fs::rename(&temp, tar)) {
        let _ = fs::remove_file(&temp);
        return Err(format!("packages: {}: {err}", tar.display()));
    }
    Ok(())
}

/// The line a bake says when the store at `out` already holds every part it would take, each for
/// the build files the checkout has now and from the same tool line; `None` when it would bake.
fn already_baked(
    context: &Context,
    out: &Path,
    args: &PackagesBakeArgs,
    harvests: &[(&'static dyn Part, PathBuf)],
    wanted: &dyn Fn(&dyn Part) -> bool,
) -> Option<String> {
    let (old, _) = store::Packages::read_bytes(out).ok()?;
    store::check_contents(out, Some(&old), Depth::Names).ok()?;
    let tool_of = |part: &dyn Part| -> Option<String> {
        if harvests.iter().any(|(h, _)| h.name() == part.name()) {
            Some("harvested".to_string())
        } else if !args.command.is_empty() && !part.manifests(&context.tracked).is_empty() {
            Some(format!("fetched by `{}`", shell_words(&args.command)))
        } else {
            None
        }
    };
    let mut taken = 0;
    for part in part::ALL.iter().copied().filter(|p| wanted(*p)) {
        let Some(tool) = tool_of(part) else { continue };
        taken += 1;
        let entry = old.part(part.name())?;
        let manifests = hashed(&context.checkout, &part.manifests(&context.tracked));
        let now = store::inputs_digest(part.name(), &tool, &context.platform, &manifests);
        if entry.tool != tool || entry.inputs != now {
            return None;
        }
    }
    (taken > 0).then(|| {
        format!(
            "packages: nothing to bake: {} already holds these parts for these build files (--force bakes)",
            out.display()
        )
    })
}

/// Takes every part a fetch or a harvest gives, for the parts whose manifest set the checkout
/// holds, into `new/parts/<part>/`.
#[allow(clippy::too_many_arguments)]
fn take_all(
    context: &Context,
    new: &Path,
    fetched: &[(&'static dyn Part, PathBuf, String)],
    harvests: &[(&'static dyn Part, PathBuf)],
    wanted: &dyn Fn(&dyn Part) -> bool,
    metas: &mut Vec<PartMeta>,
    lines: &mut Vec<String>,
) -> Result<(), Failure> {
    let fail = |why: String| Failure::new(Exit::Unexpected, anyhow!(why));
    let mut sources: Vec<(&'static dyn Part, PathBuf, String)> = Vec::new();
    for (part, folder) in harvests {
        if !wanted(*part) {
            continue;
        }
        if part.harvest_root(folder).is_none() {
            lines.push(format!(
                "packages: {}: not harvested: {} isn't a {} cache hushrun reads",
                part.name(),
                folder.display(),
                part.tool()
            ));
            continue;
        }
        sources.push((*part, folder.clone(), "harvested".to_string()));
    }
    for (part, cache, tool) in fetched {
        if sources.iter().any(|(p, _, _)| p.name() == part.name()) {
            continue;
        }
        sources.push((*part, cache.clone(), tool.clone()));
    }
    for (part, folder, tool) in sources {
        let manifests = part.manifests(&context.tracked);
        let Some((root, under)) = part.harvest_root(&folder) else {
            continue;
        };
        let dest = new.join(store::PARTS).join(part.name()).join(under);
        let (kept, left) = copy_kept(&root, &dest, part).map_err(|e| fail(e.to_string()))?;
        if kept == 0 {
            let _ = fs::remove_dir_all(new.join(store::PARTS).join(part.name()));
            continue;
        }
        if left > 0 {
            lines.push(format!(
                "packages: {}: left out {left} files of the tool's own (locks, snapshots, notes of its use)",
                part.name()
            ));
        }
        if tool == "harvested" {
            lines.push(format!(
                "packages: {}: {kept} files harvested from {}",
                part.name(),
                folder.display()
            ));
        }
        metas.push(PartMeta {
            part: part.name().to_string(),
            class: Class::Download,
            tool,
            runtime: None,
            manifests: hashed(&context.checkout, &manifests),
            verified_by: part.verified_by(),
        });
    }
    Ok(())
}

/// Copies every file under `root` the part keeps into `dest`, never following a link out; returns
/// how many it kept and how many it left out.
fn copy_kept(root: &Path, dest: &Path, part: &dyn Part) -> std::io::Result<(usize, usize)> {
    let mut kept = 0;
    let mut left = 0;
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(&dir)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                left += 1;
                continue;
            };
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push((entry.path(), path));
                continue;
            }
            if !part.keep(&path) || !store::is_row_path(&path) {
                left += 1;
                continue;
            }
            let to = dest.join(&path);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            if kind.is_symlink() {
                let target = fs::read_link(entry.path())?;
                let text = target.to_string_lossy().into_owned();
                if !store::link_stays_inside(&path, &text) {
                    left += 1;
                    continue;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &to)?;
            } else if kind.is_file() {
                fs::copy(entry.path(), &to)?;
                normalize_mode(&to)?;
            } else {
                left += 1;
                continue;
            }
            kept += 1;
        }
    }
    Ok((kept, left))
}

/// A copied file is `0644`, or `0755` when its owner may run it: no other bit of a mode is kept.
fn normalize_mode(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let exe = store::is_exe(&fs::metadata(path)?);
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if exe { 0o755 } else { 0o644 }),
        )?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// K-6: runs the fetch, COMMAND, the way a bake runs anything of the repository's: its
/// credentials of hushrun, CI and signing withheld, no results recorded, with Gradle's user home
/// Maven's local repository, pnpm's store and npm's cache at folders of the bake's own under `cache`.
fn run_fetch(checkout: &Path, command: &[String], cache: &Path) -> Result<(), String> {
    let (program, args) = command.split_first().ok_or("name the fetch's command")?;
    let gradle = fetch_cache(cache, &hushrun_packages::parts::gradle::Gradle);
    let maven = fetch_cache(cache, &hushrun_packages::parts::maven::Maven);
    let pnpm = fetch_cache(cache, &hushrun_packages::parts::pnpm::Pnpm);
    let npm = fetch_cache(cache, &hushrun_packages::parts::npm::Npm);
    let opts = hushrun_build::maven::OPTS_VARIABLE;
    let person = var(opts).map(|v| v.to_string_lossy().into_owned());
    let maven_opts = hushrun_packages::parts::maven::opts_with(
        person.as_deref(),
        &format!("-Dmaven.repo.local={}", maven.display()),
    );
    let mut built = std::process::Command::new(program);
    built.args(args);
    let built = crate::cmd::bake_build::in_bake(
        built,
        checkout,
        &[],
        None,
        &[
            ("GRADLE_USER_HOME", gradle.into_os_string()),
            (opts, OsString::from(maven_opts)),
            (
                hushrun_packages::parts::pnpm::STORE_DIR,
                pnpm.into_os_string(),
            ),
            (hushrun_packages::parts::npm::CACHE, npm.into_os_string()),
            (hushrun_packages::LOWER, OsString::new()),
        ],
    );
    crate::cmd::bake_build::run_in_bake(built, None).map_err(|ended| {
        format!(
            "packages: the fetch `{}` {}",
            shell_words(command),
            ended.words()
        )
    })
}

fn shell_words(command: &[String]) -> String {
    command.join(" ")
}

/// When the store was baked: `SOURCE_DATE_EPOCH` when set, else now.
fn baked_ms() -> u64 {
    if let Some(secs) =
        var("SOURCE_DATE_EPOCH").and_then(|v| v.to_str()?.trim().parse::<u64>().ok())
    {
        return secs.saturating_mul(1000);
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Writes every file and folder of `dir` through to the disk.
fn sync_tree(dir: &Path) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(t) if t.is_file() => {
                    let _ = fs::File::open(entry.path()).and_then(|f| f.sync_all());
                }
                _ => {}
            }
        }
        let _ = fs::File::open(&dir).and_then(|f| f.sync_all());
    }
}

/// Removes its folders when dropped, unless they were moved away.
struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for dir in &self.0 {
            if fs::symlink_metadata(dir).is_ok() {
                #[cfg(unix)]
                make_writable(dir);
                let _ = fs::remove_dir_all(dir);
            }
        }
    }
}

/// A fetch's cache may hold read-only folders (Go's module cache does): made writable to remove.
#[cfg(unix)]
fn make_writable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o755));
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: K-10
    #[test]
    fn maven_s_word_joins_the_one_maven_opts() {
        let given = Given {
            vars: vec![("GRADLE_RO_DEP_CACHE".into(), "/p/parts/gradle".into())],
            maven: Some("-Dmaven.repo.local.tail=/p/parts/maven/repository".into()),
            lines: Vec::new(),
        };
        // The build cache's MAVEN_OPTS: the person's words, the cache's, then the tail, once.
        let merged = given.merged(vec![(
            "MAVEN_OPTS".into(),
            "-Xmx1g -Dmaven.build.cache.enabled=true".into(),
        )]);
        let opts: Vec<_> = merged.iter().filter(|(n, _)| n == "MAVEN_OPTS").collect();
        assert_eq!(opts.len(), 1);
        assert_eq!(
            opts[0].1,
            "-Xmx1g -Dmaven.build.cache.enabled=true -Dmaven.repo.local.tail=/p/parts/maven/repository"
        );
        assert_eq!(
            merged[0].0, "GRADLE_RO_DEP_CACHE",
            "a reader's pairs come first"
        );
        // Twice merged, still once.
        let again = given.merged(merged.clone());
        let opts: Vec<_> = again.iter().filter(|(n, _)| n == "MAVEN_OPTS").collect();
        assert_eq!(
            opts[0]
                .1
                .to_string_lossy()
                .matches("maven.repo.local.tail")
                .count(),
            1
        );
        // No store: the variables pass unchanged.
        let none = Given::default();
        let env = vec![("MAVEN_OPTS".into(), "-Dx".into())];
        assert_eq!(none.merged(env.clone()), env);
    }

    #[test]
    fn export_lines_quote_their_values() {
        assert_eq!(
            export_line(&"A".into(), &"it's".into()),
            r"export A='it'\''s'"
        );
        assert_eq!(size(10), "10 bytes");
        assert_eq!(size(3 << 20), "3.0 MiB");
    }
}
