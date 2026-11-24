//! \[ADR 0112\] Doctests as units (C-74 to C-77): for `cargo test --doc`, hushrun stands in for
//! rustdoc, as its target runner stands in for a test binary ([`super::runner`]).
//!
//! Cargo runs one `rustdoc --test` per library whose doctests it runs, and hands it the library
//! itself, `--extern <crate>=<its .rlib>`. hushrun sets `RUSTDOC` for COMMAND to a program of its
//! own ([`super::program`]) that starts the toolchain's rustdoc, the one Cargo would have started,
//! with COMMAND's environment, and passes stdio, signals and the exit status through untouched. A
//! library `hushrun.json`'s `binaries` lists by that file's path and the SHA-256 of its bytes isn't
//! handed to rustdoc at all: its doctests are neither compiled nor run, and the stand-in exits 0.
//! Every `--test` start writes one file of facts, as the runner's do, with that file as its
//! binary; any other start of rustdoc (`-vV`) is passed through and writes nothing.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use super::facts::Invocation;
use super::program::{self, Call, Program};
use super::runner;

/// The program's name, and the file Cargo is told to start for rustdoc.
pub const NAME: &str = "rustdoc";
/// The variable that names the rustdoc the stand-in starts: the toolchain's, beside the sysroot's
/// `rustc`, as Cargo finds it. Unset, the first `rustdoc` on COMMAND's `PATH`.
pub const REAL_VAR: &str = "HUSHRUN_RUSTDOC";
/// The rustdoc the plan's build and the build read after COMMAND hand Cargo (C-75): one that
/// runs nothing and exits 0, whatever it is handed.
pub const RUNS_NOTHING: &str = "true";
/// The variables Cargo takes rustdoc's flags from, whose values a doctest's key holds (C-75).
pub const FLAG_VARS: [&str; 3] = [
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTDOCFLAGS",
    "CARGO_BUILD_RUSTDOCFLAGS",
];
/// The name the configuration files of Cargo's home are keyed under, in a doctest's `env`.
pub const HOME_CONFIG: &str = "hushrun:cargo-home-config";

/// The stand-in, as [`program::PROGRAMS`] lists it.
pub struct Rustdoc;

/// \[ADR 0112\] The one stand-in for rustdoc.
pub const RUSTDOC: Rustdoc = Rustdoc;

impl Program for Rustdoc {
    fn name(&self) -> &'static str {
        NAME
    }

    fn args(&self, call: &Call<'_>) -> Vec<OsString> {
        call.args.to_vec()
    }

    fn run(&self, call: &Call<'_>) -> Option<ExitCode> {
        let real = std::env::var_os(REAL_VAR)
            .filter(|real| !real.is_empty())
            .map_or_else(|| call.real.to_path_buf(), PathBuf::from);
        Some(stand_in(&real, call.args, call.units))
    }
}

/// The variables COMMAND gets so that Cargo starts hushrun's rustdoc: the directory of programs
/// with it in the run's directory `dir`, `RUSTDOC` naming it, and [`REAL_VAR`] the rustdoc it
/// starts (`real`, when known). `Err` says why doctests can't be observed: COMMAND's environment
/// or a Cargo configuration already names a rustdoc, which hushrun would hide, or there is no
/// rustdoc that runs nothing to build with (C-75), as on Windows.
pub fn variables(
    exe: &Path,
    dir: &Path,
    cwd: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
    real: Option<PathBuf>,
) -> Result<Vec<(String, OsString)>, String> {
    if !cfg!(unix) {
        return Err("hushrun can't build doctests without running them on this system".into());
    }
    if env("RUSTDOC").is_some_and(|value| !value.is_empty()) {
        return Err(
            "COMMAND's environment sets RUSTDOC, which this version doesn't chain to".into(),
        );
    }
    let text = |name: &str| env(name).and_then(|value| value.into_string().ok());
    if let Some(file) = config_with_rustdoc(cwd, &text) {
        return Err(format!(
            "{} sets a rustdoc, which this version doesn't chain to",
            file.display()
        ));
    }
    let programs = program::make(dir, exe, &[NAME])
        .map_err(|err| format!("can't put hushrun's rustdoc in place: {err}"))?;
    let mut out = program::variables(&programs, env("PATH"))?;
    out.push(("RUSTDOC".into(), programs.join(NAME).into_os_string()));
    if let Some(real) = real {
        out.push((REAL_VAR.into(), real.into_os_string()));
    }
    Ok(out)
}

/// A Cargo configuration file that names a rustdoc (`build.rustdoc`), from `cwd` up and in
/// Cargo's home.
fn config_with_rustdoc(cwd: &Path, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    config_files(cwd, env).into_iter().find(|path| {
        std::fs::read_to_string(path).is_ok_and(|text| {
            text.lines().any(|line| {
                let line = line.trim_start();
                let line = line.strip_prefix("build.").unwrap_or(line);
                line.strip_prefix("rustdoc")
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            })
        })
    })
}

/// Cargo's configuration files, from `cwd` up and then in Cargo's home, that exist.
fn config_files(cwd: &Path, env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = cwd.ancestors().map(|d| d.join(".cargo")).collect();
    dirs.extend(cargo_home(env));
    dirs.iter()
        .flat_map(|dir| ["config.toml", "config"].map(|name| dir.join(name)))
        .filter(|path| path.is_file())
        .collect()
}

/// Cargo's home: `CARGO_HOME`, else `.cargo` in the home folder.
fn cargo_home(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env("CARGO_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")))
}

/// \[ADR 0112\] What a library's doctests read of their flags that no file of the scope holds, as
/// C-75's `env` entries: each of [`FLAG_VARS`] with its value in COMMAND's environment (`var`),
/// or unset; and [`HOME_CONFIG`], the SHA-256 of each configuration file of Cargo's home
/// (`config.toml`, then `config`) that exists, joined, or unset when none does, since one may set
/// `build.rustdocflags`. A configuration file in the repository is a file of the rest set,
/// which every key holds. Values are raw: the caller replaces the roots.
pub fn flags_env(var: &dyn Fn(&str) -> Option<String>) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = FLAG_VARS
        .iter()
        .map(|name| ((*name).to_string(), var(name).filter(|v| !v.is_empty())))
        .collect();
    let home: Vec<String> = cargo_home(var)
        .into_iter()
        .flat_map(|dir| ["config.toml", "config"].map(|name| dir.join(name)))
        .filter_map(|path| std::fs::read(path).ok())
        .map(|bytes| hushrun_core::Digest::of(&bytes).to_string())
        .collect();
    out.push((
        HOME_CONFIG.to_string(),
        (!home.is_empty()).then(|| home.join(" ")),
    ));
    out
}

/// The file rustdoc is handed for the library whose doctests a start runs: with `--test`, the
/// `--extern` of the crate `--crate-name` names. `None` for any other start.
pub fn own_library(args: &[OsString]) -> Option<PathBuf> {
    let args: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    if !args.contains(&"--test") {
        return None;
    }
    let value = |flag: &str| -> Vec<&str> {
        let joined = format!("{flag}=");
        let mut out = Vec::new();
        let mut words = args.iter();
        while let Some(word) = words.next() {
            if *word == flag {
                out.extend(words.next().copied());
            } else if let Some(value) = word.strip_prefix(&joined) {
                out.push(value);
            }
        }
        out
    };
    let name = *value("--crate-name").first()?;
    value("--extern").into_iter().find_map(|ext| {
        let (ext_name, path) = ext.split_once('=')?;
        (ext_name == name && !path.is_empty()).then(|| PathBuf::from(path))
    })
}

/// One start of rustdoc for COMMAND: the real one, `real`, with `args`, or none for a library the
/// run's directory `units` reuses. Writes the start's facts there.
fn stand_in(real: &Path, args: &[OsString], units: Option<&Path>) -> ExitCode {
    let own = own_library(args);
    let listed = |dir: &Path, own: &Path| runner::reused(dir, own);
    if let (Some(dir), Some(own)) = (units, own.as_deref())
        && listed(dir, own)
    {
        runner::record(dir, invocation(own, args, Some(0), None, 0, true), None);
        return ExitCode::SUCCESS;
    }
    let mut command = program::real_command(real);
    command.args(args);
    // What COMMAND's environment had: no RUSTDOC (C-74), and none of hushrun's.
    command.env_remove("RUSTDOC").env_remove(REAL_VAR);
    let started = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            eprintln!("error: can't start {}: {err}", real.display());
            return ExitCode::from(if err.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            });
        }
    };
    drop(command);
    let forwarding = runner::forward_signals(child.id());
    let status = loop {
        match child.wait() {
            Ok(status) => break Some(status),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break None,
        }
    };
    drop(forwarding);
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (exit, signal) = match &status {
        Some(status) => (status.code(), runner::signal_of(status)),
        None => (None, None),
    };
    if let (Some(dir), Some(own)) = (units, own.as_deref()) {
        runner::record(
            dir,
            invocation(own, args, exit, signal, duration_ms, false),
            None,
        );
    }
    runner::end_with(exit, signal)
}

fn invocation(
    own: &Path,
    args: &[OsString],
    exit: Option<i32>,
    signal: Option<i32>,
    duration_ms: u64,
    reused: bool,
) -> Invocation {
    Invocation {
        binary: own.to_path_buf(),
        args: args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        listed: None,
        exit,
        signal,
        duration_ms,
        reused,
        dir: None,
        testlog: None,
        testlog_truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(words: &str) -> Vec<OsString> {
        words.split(' ').map(OsString::from).collect()
    }

    // covers: C-74
    #[test]
    fn a_doctest_start_is_known_by_the_library_cargo_hands_it() {
        let args = os(
            "--edition=2021 --crate-type lib --crate-name core_lib --test crates/core/src/lib.rs --extern dep=/t/deps/libdep-1.rlib --extern core_lib=/t/deps/libcore_lib-2.rlib -L dependency=/t/deps",
        );
        assert_eq!(
            own_library(&args),
            Some(PathBuf::from("/t/deps/libcore_lib-2.rlib"))
        );
        let joined = os("--crate-name=mac --test src/lib.rs --extern=mac=/t/deps/libmac-3.so");
        assert_eq!(
            own_library(&joined),
            Some(PathBuf::from("/t/deps/libmac-3.so"))
        );
        // No `--test`: documentation, or `-vV`, which the stand-in passes through.
        assert_eq!(
            own_library(&os(
                "--crate-name core_lib src/lib.rs --extern core_lib=/x.rlib"
            )),
            None
        );
        assert_eq!(own_library(&os("-vV")), None);
        // A crate not handed to itself: nothing to know it by.
        assert_eq!(
            own_library(&os("--crate-name a --test src/lib.rs --extern b=/b.rlib")),
            None
        );
    }

    // covers: C-75
    #[test]
    fn a_doctests_flags_are_keyed_from_the_environment_and_cargos_home() {
        let home = tempfile::tempdir().unwrap();
        let home_text = home.path().to_string_lossy().into_owned();
        let var = |name: &str| match name {
            "RUSTDOCFLAGS" => Some("--cfg x".to_string()),
            "CARGO_BUILD_RUSTDOCFLAGS" => Some(String::new()),
            "CARGO_HOME" => Some(home_text.clone()),
            _ => None,
        };
        let env = flags_env(&var);
        assert_eq!(env[0], ("RUSTDOCFLAGS".into(), Some("--cfg x".into())));
        // Set to nothing is unset, as Cargo reads it.
        assert_eq!(env[2], ("CARGO_BUILD_RUSTDOCFLAGS".into(), None));
        assert_eq!(env[3], (HOME_CONFIG.into(), None));
        std::fs::write(
            home.path().join("config.toml"),
            "[build]\nrustdocflags = []\n",
        )
        .unwrap();
        let again = flags_env(&var);
        assert!(
            again[3]
                .1
                .as_deref()
                .is_some_and(|v| v.starts_with("sha256:"))
        );
    }

    // covers: C-74
    #[test]
    fn a_configured_rustdoc_is_found_from_the_folder_up() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("ws/crates/a");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(root.path().join("ws/.cargo")).unwrap();
        let home = root.path().join("home");
        let home_text = home.to_string_lossy().into_owned();
        let env = |name: &str| (name == "CARGO_HOME").then(|| home_text.clone());
        let config = root.path().join("ws/.cargo/config.toml");
        std::fs::write(&config, "[build]\nrustdocflags = [\"-Dwarnings\"]\n").unwrap();
        assert_eq!(config_with_rustdoc(&cwd, &env), None);
        std::fs::write(&config, "[build]\nrustdoc = \"/opt/rustdoc\"\n").unwrap();
        assert_eq!(config_with_rustdoc(&cwd, &env), Some(config.clone()));
        std::fs::write(&config, "build.rustdoc = \"/opt/rustdoc\"\n").unwrap();
        assert_eq!(config_with_rustdoc(&cwd, &env), Some(config));
    }
}
