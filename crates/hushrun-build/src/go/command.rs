//! The `go` command the program serves (B-36, B-40): which Go versions it is set for, and whether
//! the command was altered in a way Go's action id doesn't show.
//!
//! The program is always the direct child of the `go` command (`exec.CommandContext` in Go's
//! `startCacheProg`), with that command's environment, into which `go` exports its effective
//! `GOROOT`, `GOFLAGS`, `GOCACHE` and the rest. So the command line is read from the parent
//! process, and `GOFLAGS` from the environment. Arguments that can't be read, or a parent that is
//! not the `go` binary of the `GOROOT` the environment names (the program was reparented), count
//! as altered: that only keeps blocks off the shared store, which is the safe side.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The oldest and newest Go minor versions whose cache protocol (`GOCACHEPROG`, standard from Go
/// 1.24) and `GOCACHE` format the program knows. A newer one gets no program (B-36): `go` runs
/// as it would without hushrun.
pub const OLDEST_MINOR: u32 = 24;
pub const NEWEST_MINOR: u32 = 27;

/// Whether `version`, as `go env GOVERSION` says it (`go1.27.4`, `go1.25rc1`), is one whose
/// cache the program knows: `go1.24` to `go1.27`, any patch, release candidates and betas
/// included. A development build (`devel …`) or anything else is not.
pub fn version_known(version: &str) -> bool {
    let Some(rest) = version.trim().strip_prefix("go1.") else {
        return false;
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let Ok(minor) = digits.parse::<u32>() else {
        return false;
    };
    let tail = &rest[digits.len()..];
    let tail_ok = tail.is_empty()
        || tail
            .strip_prefix('.')
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        || ["rc", "beta"].iter().any(|word| {
            tail.strip_prefix(word)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
    (OLDEST_MINOR..=NEWEST_MINOR).contains(&minor) && tail_ok
}

/// How a `go` command was altered so that Go's action id no longer says what was compiled
/// (B-40). Any of them keeps every block of the command on the machine, and the hushrun store
/// serves the command nothing: only Go's own cache does, as plain `go` would.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Altered {
    /// `-toolexec`: a program runs each tool, and only the version lines it passes on are in
    /// the id.
    Toolexec,
    /// `-overlay`: files replaced for the build.
    Overlay,
    /// `-compiler` or `-gccgoflags`: another compiler.
    Compiler,
    /// `-linkshared` or `-buildmode=shared|plugin`: not tried.
    Shared,
    /// `-gcflags` or `-asmflags` naming a file the tool then reads (`-I`, `-importcfg`,
    /// `-embedcfg`, `-pgoprofile`, `-coveragecfg`, `-symabis`, `-asmhdr`, `-std`): Go hashes the
    /// flag's text, not the file (F1).
    FlagsNameFiles,
    /// The command's arguments couldn't be read, or its process is not the `go` binary of the
    /// `GOROOT` the environment names.
    Unreadable,
}

/// What in a command line or `GOFLAGS` alters a command, matched as text anywhere, so that
/// `--toolexec x`, `-toolexec=x` and quoting can't slip past.
const ALTERING: &[(&str, Altered)] = &[
    ("toolexec", Altered::Toolexec),
    ("overlay", Altered::Overlay),
    ("-compiler", Altered::Compiler),
    ("gccgoflags", Altered::Compiler),
    ("linkshared", Altered::Shared),
    ("buildmode=shared", Altered::Shared),
    ("buildmode=plugin", Altered::Shared),
    ("buildmode shared", Altered::Shared),
    ("buildmode plugin", Altered::Shared),
];

/// What, after `-gcflags` or `-asmflags`, names a file the compiler or assembler reads.
const NAMING_FILES: &[&str] = &["-I", "-std", "cfg", "profile", "symabis", "asmhdr"];

/// How `command` (the `go` command's arguments, joined by spaces) and `goflags` alter the
/// command, or `None`.
pub fn altered(command: &str, goflags: &str) -> Option<Altered> {
    let text = format!("{command} {goflags}");
    if let Some((_, how)) = ALTERING.iter().find(|(word, _)| text.contains(word)) {
        return Some(*how);
    }
    for flag in ["gcflags", "asmflags"] {
        let mut rest = text.as_str();
        while let Some(at) = rest.find(flag) {
            rest = &rest[at + flag.len()..];
            if NAMING_FILES.iter().any(|word| rest.contains(word)) {
                return Some(Altered::FlagsNameFiles);
            }
        }
    }
    None
}

/// The parent process's command line, as one string, when its executable is `go_binary`. Read
/// from `/proc` on Linux, and on macOS with `lsof` and `ps` (with no stdin, their stdout read
/// through a pipe, never the program's own: Go reads the program's stdout until it closes).
/// `Err` is [`Altered::Unreadable`].
pub fn parent_command(go_binary: &Path) -> Result<String, Altered> {
    #[cfg(unix)]
    {
        let ppid = rustix::process::getppid()
            .map(|pid| pid.as_raw_nonzero().get())
            .ok_or(Altered::Unreadable)?;
        parent_of(ppid, go_binary)
    }
    #[cfg(not(unix))]
    {
        let _ = go_binary;
        Err(Altered::Unreadable)
    }
}

#[cfg(target_os = "linux")]
fn parent_of(pid: i32, go_binary: &Path) -> Result<String, Altered> {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).map_err(|_| Altered::Unreadable)?;
    if !same_file(&exe, go_binary) {
        return Err(Altered::Unreadable);
    }
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).map_err(|_| Altered::Unreadable)?;
    Ok(raw
        .split(|b| *b == 0)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect::<Vec<_>>()
        .join(" "))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn parent_of(pid: i32, go_binary: &Path) -> Result<String, Altered> {
    // `ps` names the command as it was called (`go`, from `PATH`, and still `go` after a
    // toolchain switch execs another binary), so the executable comes from `lsof`: the first
    // file it lists as the process's text is the binary it runs.
    let run = |program: &str, args: &[&str]| -> Result<String, Altered> {
        let out = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|_| Altered::Unreadable)?;
        if !out.status.success() {
            return Err(Altered::Unreadable);
        }
        String::from_utf8(out.stdout).map_err(|_| Altered::Unreadable)
    };
    let pid = pid.to_string();
    let listed = run("/usr/sbin/lsof", &["-a", "-p", &pid, "-d", "txt", "-Fn"])?;
    let exe = listed
        .lines()
        .find_map(|line| line.strip_prefix('n'))
        .map(PathBuf::from)
        .ok_or(Altered::Unreadable)?;
    if !same_file(&exe, go_binary) {
        return Err(Altered::Unreadable);
    }
    Ok(run("/bin/ps", &["-ww", "-o", "args=", "-p", &pid])?
        .trim()
        .to_string())
}

/// Whether two paths name one file, through links.
#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// The `go` binary of `goroot`.
pub fn go_binary(goroot: &Path) -> PathBuf {
    goroot
        .join("bin")
        .join(if cfg!(windows) { "go.exe" } else { "go" })
}

/// `GOFLAGS` from the environment, which `go` exports with its effective value.
pub fn goflags(env: &dyn Fn(&str) -> Option<OsString>) -> String {
    env("GOFLAGS")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-36
    #[test]
    fn only_the_go_versions_whose_cache_the_program_knows_get_it() {
        for known in [
            "go1.24",
            "go1.24.0",
            "go1.25.14",
            "go1.27.4",
            "go1.26rc1",
            "go1.27beta2",
        ] {
            assert!(version_known(known), "{known}");
        }
        for unknown in [
            "go1.23.12",
            "go1.28",
            "go1.28.1",
            "go1.21",
            "devel go1.28-abcdef",
            "go1.27.x",
            "go1.27.",
            "go2.0",
            "",
            "go1.27rc",
        ] {
            assert!(!version_known(unknown), "{unknown}");
        }
    }

    // covers: B-40
    #[test]
    fn a_command_that_alters_what_its_ids_name_is_seen_in_every_spelling() {
        assert_eq!(altered("go build ./...", ""), None);
        assert_eq!(altered("go test -race -cover ./...", "-trimpath"), None);
        assert_eq!(altered("go build -gcflags=all=-N -l ./...", ""), None);
        assert_eq!(
            altered("go build -toolexec=/x/tx.sh ./...", ""),
            Some(Altered::Toolexec)
        );
        assert_eq!(
            altered("go build --toolexec /x ./...", ""),
            Some(Altered::Toolexec)
        );
        assert_eq!(
            altered("go build ./...", "-toolexec=x"),
            Some(Altered::Toolexec)
        );
        assert_eq!(
            altered("go build -overlay o.json .", ""),
            Some(Altered::Overlay)
        );
        assert_eq!(
            altered("go build -compiler=gccgo .", ""),
            Some(Altered::Compiler)
        );
        assert_eq!(
            altered("go build -buildmode=plugin .", ""),
            Some(Altered::Shared)
        );
        assert_eq!(
            altered("go build -asmflags=example.com/m/p=-I=/inc .", ""),
            Some(Altered::FlagsNameFiles)
        );
        assert_eq!(
            altered("go build .", "-gcflags=all=-importcfg=/tmp/x"),
            Some(Altered::FlagsNameFiles)
        );
        assert_eq!(
            altered("go build -gcflags 'all=-pgoprofile=/p' .", ""),
            Some(Altered::FlagsNameFiles)
        );
    }

    // covers: B-40
    #[cfg(unix)]
    #[test]
    fn a_parent_that_is_not_the_go_binary_is_unreadable() {
        // The test's own parent is cargo or a shell, never this file.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("go");
        std::fs::write(&fake, b"not go").unwrap();
        assert_eq!(parent_command(&fake), Err(Altered::Unreadable));
    }
}
