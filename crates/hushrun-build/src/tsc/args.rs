//! Which compiler calls the helper handles (B-101), from their arguments alone.
//!
//! The compiler is the authority on what an option means: every argument is keyed by its text
//! (B-102), and these are the calls whose outcome isn't what a block can hold: they watch, print
//! something that isn't a function of what was read (timings, help), write files the hook doesn't
//! follow (traces, profiles), delete outputs, or decide by dates.

/// What a call the helper handles is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asked {
    /// `tsc --build` (`-b`): it decides by the dates of outputs too (B-104).
    pub build: bool,
}

/// What the helper does with a call: handle it, or pass it through with the reason.
pub fn classify(args: &[String]) -> Result<Asked, &'static str> {
    let names: Vec<String> = args
        .iter()
        .filter(|a| a.starts_with('-'))
        .map(|a| {
            let name = a.trim_start_matches('-');
            name.split_once(':').map_or(name, |(n, _)| n).to_ascii_lowercase()
        })
        .collect();
    let has = |words: &[&str]| names.iter().any(|n| words.contains(&n.as_str()));
    let build = args
        .first()
        .is_some_and(|a| matches!(a.to_ascii_lowercase().as_str(), "-b" | "--build"));
    if has(&["watch", "w"]) {
        return Err("watch");
    }
    if has(&["init", "help", "h", "?", "all", "version", "v", "showconfig"]) {
        return Err("prints");
    }
    if has(&["diagnostics", "extendeddiagnostics"]) {
        return Err("timings");
    }
    if has(&["generatetrace", "generatecpuprofile"]) {
        return Err("trace");
    }
    if build && has(&["verbose", "dry", "d", "clean"]) {
        return Err("build-mode");
    }
    // A response file (`@args.txt`) is read through `fs` and keyed as any read.
    Ok(Asked { build })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(words: &[&str]) -> Result<Asked, &'static str> {
        classify(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    // covers: B-101
    #[test]
    fn plain_compiles_and_type_checks_are_handled() {
        assert_eq!(of(&[]), Ok(Asked { build: false }));
        assert_eq!(of(&["--noEmit"]), Ok(Asked { build: false }));
        assert_eq!(of(&["-p", "tsconfig.build.json"]), Ok(Asked { build: false }));
        assert_eq!(of(&["-b"]), Ok(Asked { build: true }));
        assert_eq!(of(&["--build", "packages/a", "--force"]), Ok(Asked { build: true }));
        assert_eq!(of(&["--pretty", "false", "--listFiles"]), Ok(Asked { build: false }));
    }

    // covers: B-101
    #[test]
    fn calls_a_block_cant_hold_pass_through_with_their_reason() {
        for (words, why) in [
            (&["--watch"][..], "watch"),
            (&["-w"], "watch"),
            (&["-b", "-w"], "watch"),
            (&["--init"], "prints"),
            (&["-v"], "prints"),
            (&["--showConfig"], "prints"),
            (&["--extendedDiagnostics"], "timings"),
            (&["--generateTrace", "t"], "trace"),
            (&["-b", "--verbose"], "build-mode"),
            (&["-b", "--clean"], "build-mode"),
            (&["--build", "--dry"], "build-mode"),
        ] {
            assert_eq!(of(words), Err(why), "{words:?}");
        }
        // `--verbose` means nothing outside `--build`.
        assert!(of(&["--verbose"]).is_ok());
    }
}
