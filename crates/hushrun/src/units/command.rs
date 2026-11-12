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
}
