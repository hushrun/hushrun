//! \[C-32\] What every recognizer reads of a COMMAND the same way, beside the registry
//! ([`super::adapter::RECOGNIZED`]).

use std::path::Path;

/// The program `word` names: its file name, without its directory and without a Windows `.exe`,
/// `.cmd` or `.bat`, in any case.
pub fn program_name(word: &str) -> &str {
    let file = word.rsplit(['/', '\\']).next().unwrap_or(word);
    for ext in [".exe", ".cmd", ".bat"] {
        if let Some(at) = file.len().checked_sub(ext.len())
            && at > 0
            && file.is_char_boundary(at)
            && file[at..].eq_ignore_ascii_case(ext)
        {
            return &file[..at];
        }
    }
    file
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
                .any(|w| matches!(program_name(w), "pytest" | "py.test" | "tox" | "nox")),
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
    match program_name(first) {
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
