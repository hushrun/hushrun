//! \[C-32\] What every recognizer reads of a COMMAND the same way, beside the registry
//! ([`super::adapter::RECOGNIZED`]).

use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

/// The program `word` names: its file name, without its directory and without a Windows `.exe`,
/// `.cmd` or `.bat`, in any case; \[C-32\] on Windows, whose file system ignores case, in lower
/// case, so that `MVN.CMD` is `mvn` there.
pub fn program_name(word: &str) -> Cow<'_, str> {
    program_name_on(word, cfg!(windows))
}

/// [`program_name`] on a file system that ignores case (`folds`) or not.
fn program_name_on(word: &str, folds: bool) -> Cow<'_, str> {
    let file = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let mut name = file;
    for ext in [".exe", ".cmd", ".bat"] {
        if let Some(at) = file.len().checked_sub(ext.len())
            && at > 0
            && file.is_char_boundary(at)
            && file[at..].eq_ignore_ascii_case(ext)
        {
            name = &file[..at];
            break;
        }
    }
    if folds && name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    }
}

/// COMMAND after `env NAME=value …`, which starts what follows with those variables added: what
/// recognizers read. COMMAND itself when it doesn't start so, or when `env` has an option of its
/// own (`-i`, `-u`, `-S`, `--`…), which can change what reaches the command, hushrun's variables
/// included. Only recognizing reads it: a unit's key holds COMMAND as written (C-31).
pub fn without_env(command: &[String]) -> &[String] {
    let Some(first) = command.first() else {
        return command;
    };
    if program_name(first) != "env" {
        return command;
    }
    let rest = &command[1..];
    let at = rest
        .iter()
        .position(|w| !(w.contains('=') && !w.starts_with('-') && !w.starts_with('=')))
        .unwrap_or(rest.len());
    match rest.get(at) {
        Some(next) if !next.starts_with('-') => &rest[at..],
        _ => command,
    }
}

/// \[C-32\] The `NAME=value` assignments COMMAND makes with `env NAME=value …` before the program
/// [`without_env`] reads through, in order: what that program's environment has beside hushrun's.
/// None when COMMAND doesn't start so.
pub fn assignments(command: &[String]) -> Vec<(String, String)> {
    let after = without_env(command);
    if after.len() == command.len() {
        return Vec::new();
    }
    command[1..command.len() - after.len()]
        .iter()
        .filter_map(|word| word.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// \[C-32\] The one way an adapter starts a tool of its own for COMMAND (Cargo's plan build and
/// `cargo metadata`, `go env` and `go list`, `swift package describe`, CTest's listing): `program`
/// in `cwd`, with COMMAND's own assignments (`assigned`, [`assignments`]) over hushrun's
/// environment, as COMMAND's program will have them, and without hushrun's `HUSHRUN_UNITS`. The
/// caller adds the tool's arguments and what else it needs.
pub fn tool(program: impl AsRef<OsStr>, cwd: &Path, assigned: &[(String, String)]) -> Command {
    let mut command = Command::new(program);
    command.current_dir(cwd);
    command.envs(assigned.iter().map(|(name, value)| (name, value)));
    command.env_remove(super::runner::UNITS_VAR);
    command
}

/// A test runner a wrapper COMMAND's text can start by name: pytest, which any COMMAND nobody else
/// takes gets, and the two hushrun can only get into through `PATH` ([`super::program`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    /// `pytest`, `py.test`, `tox` or `nox`.
    Pytest,
    /// `ctest`.
    Ctest,
    /// `swift test` or `swift-test`.
    SwiftTest,
}

const RUNNERS: [Runner; 3] = [Runner::Pytest, Runner::Ctest, Runner::SwiftTest];

impl Runner {
    /// Whether `words` name this runner: pytest's in any form, a program on `PATH` by its name
    /// alone (a path to it goes round hushrun's program).
    fn started_by(self, words: &[&str]) -> bool {
        match self {
            Runner::Pytest => words
                .iter()
                .any(|w| matches!(&*program_name(w), "pytest" | "py.test" | "tox" | "nox")),
            Runner::Ctest => words.contains(&"ctest"),
            Runner::SwiftTest => {
                words.contains(&"swift-test") || words.windows(2).any(|w| w == ["swift", "test"])
            }
        }
    }
}

/// The text of a **wrapper** COMMAND, one that starts other commands from text of its own: a
/// shell's `-c` line (`sh`, `bash`, `zsh`, `dash`, `ksh`); a script that starts with `#!`, named by
/// its path (its first 256 KiB); `make` or `gmake` with its Makefile (`GNUmakefile`, `makefile` or
/// `Makefile`, in `-C`'s directory or COMMAND's). `None` for any other COMMAND.
pub fn wrapper_text(command: &[String], cwd: &Path) -> Option<String> {
    let first = command.first()?;
    let rest = &command[1..];
    match &*program_name(first) {
        "sh" | "bash" | "zsh" | "dash" | "ksh" => {
            let at = rest
                .iter()
                .position(|w| w.starts_with('-') && !w.starts_with("--") && w.contains('c'))?;
            rest.get(at + 1).cloned()
        }
        "make" | "gmake" => {
            let dir = rest
                .windows(2)
                .find(|w| w[0] == "-C")
                .map_or_else(|| cwd.to_path_buf(), |w| cwd.join(&w[1]));
            let makefile = ["GNUmakefile", "makefile", "Makefile"]
                .iter()
                .map(|f| dir.join(f))
                .find(|p| p.is_file())?;
            Some(String::from_utf8_lossy(&std::fs::read(makefile).ok()?).into_owned())
        }
        _ => {
            let path = cwd.join(first);
            if !first.contains('/') || !path.is_file() {
                return None;
            }
            let mut bytes = std::fs::read(&path).ok()?;
            bytes.truncate(256 * 1024);
            bytes
                .starts_with(b"#!")
                .then(|| String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}

/// \[C-32\] The one rule for a wrapper COMMAND ([`wrapper_text`]) and an adapter that loads through
/// `PATH`: it is that adapter's when the wrapper's own text starts `runner` by name and no other
/// runner of [`Runner`]'s. Any other wrapper stays with the adapter every other COMMAND gets.
pub fn wrapper_starts(command: &[String], cwd: &Path, runner: Runner) -> bool {
    let Some(text) = wrapper_text(command, cwd) else {
        return false;
    };
    let words: Vec<&str> = text
        .split(|c: char| {
            c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`' | '"' | '\'')
        })
        .filter(|w| !w.is_empty())
        .collect();
    runner.started_by(&words)
        && RUNNERS
            .iter()
            .filter(|other| **other != runner)
            .all(|other| !other.started_by(&words))
}

/// \[ADR 0089, 0090\] The path a wrapper COMMAND's text starts `runner` by, when it starts it by
/// a path and never by its name, and no other runner: it goes round the program hushrun puts on
/// `PATH`, as a COMMAND that starts it by its path does.
pub fn wrapper_starts_by_path(command: &[String], cwd: &Path, runner: Runner) -> Option<String> {
    let text = wrapper_text(command, cwd)?;
    let words: Vec<&str> = text
        .split(|c: char| {
            c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`' | '"' | '\'')
        })
        .filter(|w| !w.is_empty())
        .collect();
    if runner.started_by(&words) || RUNNERS.iter().any(|other| other.started_by(&words)) {
        return None;
    }
    let by_path = |at: usize| {
        let word = words[at];
        word.contains(['/', '\\']).then(|| program_name(word))
    };
    (0..words.len())
        .find(|&at| match (runner, by_path(at).as_deref()) {
            (Runner::Ctest, Some("ctest")) => true,
            (Runner::SwiftTest, Some("swift")) => words.get(at + 1) == Some(&"test"),
            (Runner::SwiftTest, Some("swift-test")) => true,
            _ => false,
        })
        .map(|at| words[at].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_programs_name_is_its_file_without_a_windows_extension() {
        assert_eq!(program_name("cargo"), "cargo");
        assert_eq!(program_name("/usr/bin/go"), "go");
        assert_eq!(program_name("C:\\tools\\gradlew.BAT"), "gradlew");
        assert_eq!(program_name("vitest.Cmd"), "vitest");
        assert_eq!(program_name("ctest.exe"), "ctest");
        assert_eq!(program_name("jest.js"), "jest.js");
        assert_eq!(program_name(".exe"), ".exe");
        assert_eq!(program_name("tox.ini"), "tox.ini");
        // Where the file system ignores case, so does a program's name.
        assert_eq!(program_name_on("C:\\tools\\MVN.CMD", true), "mvn");
        assert_eq!(program_name_on("Gradlew.Bat", true), "gradlew");
        assert_eq!(program_name_on("MVN.CMD", false), "MVN");
    }

    #[test]
    fn a_tool_gets_commands_own_assignments() {
        let words = |text: &str| text.split(' ').map(str::to_string).collect::<Vec<_>>();
        let assigned = assignments(&words("env GOARCH=386 GOFLAGS=-tags=x go test ./..."));
        assert_eq!(
            assigned,
            [
                ("GOARCH".to_string(), "386".to_string()),
                ("GOFLAGS".to_string(), "-tags=x".to_string())
            ]
        );
        assert!(assignments(&words("go test")).is_empty());
        assert!(assignments(&words("env -i A=1 go test")).is_empty());
        let tool = tool("go", Path::new("/r"), &assigned);
        let envs: Vec<(String, Option<String>)> = tool
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(envs.contains(&("GOARCH".into(), Some("386".into()))));
        assert!(envs.contains(&(super::super::runner::UNITS_VAR.into(), None)));
        assert_eq!(tool.get_current_dir(), Some(Path::new("/r")));
    }

    #[test]
    fn env_with_variables_is_read_through_and_env_with_options_is_not() {
        let words = |text: &str| text.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(without_env(&words("env A=1 B= go test")), words("go test"));
        assert_eq!(
            without_env(&words("/usr/bin/env go test")),
            words("go test")
        );
        assert_eq!(without_env(&words("go test")), words("go test"));
        for kept in [
            "env -i go test",
            "env A=1 -u B go test",
            "env A=1",
            "env",
            "env -- go",
        ] {
            assert_eq!(without_env(&words(kept)), words(kept), "{kept}");
        }
    }
}
