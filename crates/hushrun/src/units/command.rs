//! \[C-32\] What every recognizer reads of a COMMAND the same way, beside the registry
//! ([`super::adapter::RECOGNIZED`]).

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
