//! `hushrun build` (B-1, B-21): the mode of build blocks, one per user, and the store that keeps
//! them. The settings file is the mode's only home ([`hushrun_build::settings`]): this command
//! changes it under the file's lock, and shows what a `hushrun run` from this shell would see.
//! It needs no repository and puts no hook anywhere.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::anyhow;
use serde_json::{Value, json};

use hushrun_build::model::{Mode, Programs};
use hushrun_build::session;
use hushrun_build::settings::{self, Overrides, SettingsError};
use hushrun_build::store::{Status, Store};

use crate::cli::{BuildArgs, BuildCommand, GlobalOpts, ShareWord};
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
    .filter(|access| access.credential.is_some())
    // An agent's token is read from `HUSHRUN_AGENT` (the string `hushrun token create --agent`
    // prints, which the docs name) and, as an `hra_` token, from `HUSHRUN_TOKEN`.
    .ok_or_else(|| {
        failed(
            "no backend is signed in to: set HUSHRUN_AGENT for this phase to the string `hushrun token create --agent` printed (an agent token in HUSHRUN_TOKEN works too)"
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
fn run_here(effective: &settings::Settings) -> hushrun_build::share::Does {
    use hushrun_build::share::{Does, Off, Org, Sharing, what_a_run_does};
    let root = std::env::current_dir().ok();
    let access = match crate::results::Backend::select(root.as_deref()) {
        Ok(crate::results::Selection::On(backend)) => backend.build_access(),
        _ => None,
    };
    let in_ci = hushrun_ci::in_ci(&hushrun_ci::Env::from_process());
    if in_ci && access.is_none() {
        return Does {
            local: Mode::Off,
            sharing: Sharing::Off(Off::SignedOut),
            uploads: false,
        };
    }
    let caller = crate::cmd::build_session::caller(access.as_ref(), in_ci);
    what_a_run_does(caller, effective.mode, effective.share, Org::Shares(None))
}

/// The `ci` line of `hushrun build status` (B-2, B-31): what `hushrun run` does in this CI job,
/// and why, from [`run_here`].
fn ci_here(effective: &settings::Settings) -> String {
    use hushrun_build::share::{Off, Sharing};
    let does = run_here(effective);
    match (does.local, does.sharing) {
        (Mode::Off, Sharing::Off(off)) => {
            let hint = match off {
                Off::NoIdToken => " (give the job `permissions: id-token: write`)",
                _ => "",
            };
            format!(
                "this is CI, where `hushrun run` reuses nothing here: {}{hint}",
                off.reason()
            )
        }
        _ if !does.uploads => "this is CI: `hushrun run NAME -- COMMAND` reuses in the job's own store when the org shares, reading the org's blocks and uploading none (an agent token outside GitHub Actions); no person's settings are read".to_string(),
        _ => "this is CI: `hushrun run NAME -- COMMAND` reuses in the job's own store when the org shares, and shares as the org's mode says; no person's settings are read".to_string(),
    }
}

/// What sharing does here, as far as this machine can tell without asking the backend (B-25):
/// the word, and why.
fn sharing_here(effective: &settings::Settings) -> (&'static str, String) {
    use hushrun_build::share::Sharing;
    let does = run_here(effective);
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

/// `hushrun build on|observe|off`: saves the mode, under the settings file's lock.
fn set(global: &GlobalOpts, place: &Place, mode: Mode) -> CmdResult {
    let mut before = None;
    let saved = settings::update(&place.settings, |settings| {
        before = Some(settings.mode);
        settings.mode = mode;
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
    let store = place.store.as_deref().map(Store::new);
    let held = store.as_ref().map(|store| store.status(now(global)));
    let env = hushrun_ci::Env::from_process();
    let in_ci = hushrun_ci::in_ci(&env);
    let wrapper = std::env::var_os(session::RUSTC_WRAPPER).filter(|v| !v.is_empty());
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
                let (word, why) = sharing_here(s);
                json!({"mode": word, "why": why})
            }),
            "in_ci": in_ci,
            "rustc_wrapper_set": wrapper.is_some(),
            "held": held.as_ref().map(held_json),
        }))?;
        return Ok(exit);
    }
    let mut out = String::new();
    let mut line = |label: &str, value: String| {
        let _ = writeln!(out, "{label:<10}{value}");
    };
    match &saved {
        Ok(saved) => {
            let mode = place.mode_override(saved.mode).unwrap_or(saved.mode);
            line("mode", mode.as_str().to_string());
            if mode != saved.mode {
                line(
                    "",
                    format!(
                        "HUSHRUN_BUILD={} overrides the saved mode, {}, in this shell",
                        mode.as_str(),
                        saved.mode.as_str()
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
            ci_here(effective.as_ref().unwrap_or(&settings::Settings::default())),
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
        let (word, why) = sharing_here(effective);
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
                Programs::Movable => "movable: a program on macOS gets a debug bundle and may be reused in another folder",
                Programs::Tied => "tied: a program is reused only in the target folder that built it",
            }
            .to_string(),
        );
    }
    if let Some(held) = &held {
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
    use super::{blocks, size};

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
