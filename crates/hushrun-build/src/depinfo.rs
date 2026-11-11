//! rustc's dep-info (`.d`) files: what a compile read (B-6). Parsing, writing them back for
//! another folder, and the places a module could have been and was not.
//!
//! What rustc 1.94 writes, and what this module reads and writes back:
//!
//! ```text
//! /abs/target/debug/deps/foo-1a2b.d: src/lib.rs src/my\ mod.rs
//!
//! /abs/target/debug/deps/libfoo-1a2b.rmeta: src/lib.rs src/my\ mod.rs
//!
//! src/lib.rs:
//! src/my\ mod.rs:
//!
//! # env-dep:CARGO_PKG_VERSION=0.1.0
//! # env-dep:UNSET_ONE
//! ```
//!
//! One line per output (the target), each followed by a blank line. Targets are written as they
//! are; in the dependencies a space is written `\ ` and nothing else is escaped. Then one empty
//! rule per dependency, so `make` doesn't fail on a deleted file. Then, only when the compile read
//! a variable, a blank line and one `# env-dep:` line per variable, sorted, where `\n`, `\r` and
//! `\` are escaped.
//!
//! Cargo reads the file right after rustc exits (`parse_rustc_dep_info` in cargo's
//! `fingerprint/dep_info.rs`): the dependencies of the first line holding `": "`, split on
//! whitespace, a token that ends in `\` joined to the next with a space; and the `# env-dep:`
//! lines. [`DepInfo::check`] says whether cargo reads a [`render`]ed file back as it was meant.

/// What a dep-info file says a compile read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DepInfo {
    /// The outputs the file names, in its order: the `.d` file itself, the `.rlib`, the `.rmeta`.
    pub targets: Vec<String>,
    /// Every file the compile read, each once, in the order rustc listed them.
    pub files: Vec<String>,
    /// Every variable the compile read, in the file's order (rustc sorts them by name).
    pub env: Vec<EnvDep>,
}

/// One `# env-dep:` line: a variable the compile read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvDep {
    /// The variable's name.
    pub name: String,
    /// Its value, or `None` when it was unset when read (`option_env!`).
    pub value: Option<String>,
}

/// A dep-info file this module can't read, or one cargo wouldn't read back as written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A line that is none of a rule, an empty rule, a comment or a blank line.
    #[error("dep-info line {line}: not a rule or a comment")]
    Unexpected {
        /// The line's number, from 1.
        line: usize,
    },
    /// A `\` in an `env-dep` line followed by something rustc never escapes, or by nothing.
    #[error("dep-info line {line}: bad escape in an env-dep line")]
    Escape {
        /// The line's number, from 1.
        line: usize,
    },
    /// A rule whose dependencies end in a `\`: cargo refuses it too.
    #[error("dep-info line {line}: ends in a backslash")]
    TrailingBackslash {
        /// The line's number, from 1.
        line: usize,
    },
    /// No `target: dependencies` line at all.
    #[error("dep-info names no output")]
    NoTarget,
    /// A path or variable rustc's format can't carry so that cargo reads it back.
    #[error("cargo would not read this back from a dep-info file: {0:?}")]
    Unwritable(String),
}

const ENV_DEP: &str = "# env-dep:";

/// Reads a dep-info file as rustc writes it.
///
/// Several target lines repeat the same dependencies; [`DepInfo::files`] holds each once, in
/// first-seen order. The empty rules and comments other than `# env-dep:` (such as
/// `# checksum:`) are ignored.
pub fn parse(text: &str) -> Result<DepInfo, Error> {
    let mut info = DepInfo::default();
    let mut seen = std::collections::HashSet::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        if let Some(rest) = raw.strip_prefix(ENV_DEP) {
            let (name, value) = match rest.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (rest, None),
            };
            let name = unescape_env(name).ok_or(Error::Escape { line })?;
            let value = match value {
                Some(value) => Some(unescape_env(value).ok_or(Error::Escape { line })?),
                None => None,
            };
            info.env.push(EnvDep { name, value });
        } else if raw.is_empty() || raw.starts_with('#') {
            continue;
        } else if let Some((target, deps)) = raw.split_once(": ") {
            // The first ": " is the separator, as for cargo: targets are written unescaped, and
            // a space in a dependency is always escaped.
            info.targets.push(target.to_owned());
            for file in split_deps(deps).ok_or(Error::TrailingBackslash { line })? {
                if seen.insert(file.clone()) {
                    info.files.push(file);
                }
            }
        } else if raw.ends_with(':') {
            // The empty rule rustc writes for each dependency.
            continue;
        } else {
            return Err(Error::Unexpected { line });
        }
    }
    if info.targets.is_empty() {
        return Err(Error::NoTarget);
    }
    Ok(info)
}

/// Writes what rustc would have written for these targets, files and variables.
///
/// `parse(&render(x)) == x` holds when `x.files` has no repeats and `x.env` is in rustc's order,
/// as every [`parse`]d value is. Variables are written sorted, as rustc does. Whether cargo reads
/// the result back as meant is [`DepInfo::check`].
pub fn render(info: &DepInfo) -> String {
    let files: Vec<String> = info.files.iter().map(|f| escape_file(f)).collect();
    let deps = files.join(" ");
    let mut out = String::new();
    for target in &info.targets {
        out.push_str(target);
        out.push_str(": ");
        out.push_str(&deps);
        out.push_str("\n\n");
    }
    for file in &files {
        out.push_str(file);
        out.push_str(":\n");
    }
    if !info.env.is_empty() {
        let mut env: Vec<(String, Option<String>)> = info
            .env
            .iter()
            .map(|e| (escape_env(&e.name), e.value.as_deref().map(escape_env)))
            .collect();
        env.sort_unstable();
        out.push('\n');
        for (name, value) in env {
            out.push_str(ENV_DEP);
            out.push_str(&name);
            if let Some(value) = value {
                out.push('=');
                out.push_str(&value);
            }
            out.push('\n');
        }
    }
    out
}

impl DepInfo {
    /// The same dep-info with every target and file changed by `f`, for writing a block's
    /// dep-info with another folder's paths.
    pub fn map_paths(self, mut f: impl FnMut(&str) -> String) -> DepInfo {
        DepInfo {
            targets: self.targets.iter().map(|t| f(t)).collect(),
            files: self.files.iter().map(|p| f(p)).collect(),
            env: self.env,
        }
    }

    /// The same dep-info with every variable's value changed by `f`. Unset variables stay unset.
    pub fn map_env(self, mut f: impl FnMut(&str) -> String) -> DepInfo {
        DepInfo {
            env: self
                .env
                .into_iter()
                .map(|e| EnvDep {
                    value: e.value.as_deref().map(&mut f),
                    name: e.name,
                })
                .collect(),
            ..self
        }
    }

    /// Whether cargo reads [`render`]'s text back with these very paths and variables.
    ///
    /// rustc's format can't carry everything: a line break in a path, a target holding `": "`,
    /// a file holding whitespace other than a space (cargo splits on it) or ending in `\` (cargo
    /// joins it to the next), a path starting with `#`, an empty path, or a variable name
    /// holding `=`. The error names the first such value. A file rustc itself wrote for such
    /// paths is just as unreadable to cargo, so a wrapper that meets one runs rustc.
    pub fn check(&self) -> Result<(), Error> {
        let bad_path =
            |p: &str| p.is_empty() || p.starts_with('#') || p.contains('\n') || p.contains('\r');
        for target in &self.targets {
            if bad_path(target) || target.contains(": ") {
                return Err(Error::Unwritable(target.clone()));
            }
        }
        for file in &self.files {
            if bad_path(file)
                || file.ends_with('\\')
                || file.chars().any(|c| c != ' ' && c.is_whitespace())
            {
                return Err(Error::Unwritable(file.clone()));
            }
        }
        for env in &self.env {
            if env.name.is_empty() || env.name.contains('=') {
                return Err(Error::Unwritable(env.name.clone()));
            }
        }
        Ok(())
    }
}

/// The other place a module read from `file` could have been (B-6): `…/foo.rs` gives
/// `…/foo/mod.rs` and `…/foo/mod.rs` gives `…/foo.rs`.
///
/// `None` for a file that isn't `.rs`, and for a `mod.rs` whose folder has no name in the path
/// (`mod.rs`, `./mod.rs`, `/mod.rs`).
pub fn module_candidate(file: &str) -> Option<String> {
    let (dir, name) = match file.rsplit_once('/') {
        Some((dir, name)) => (Some(dir), name),
        None => (None, file),
    };
    let stem = name.strip_suffix(".rs")?;
    if stem.is_empty() {
        return None;
    }
    if stem == "mod" {
        let dir = dir?;
        let folder = dir.rsplit('/').next().unwrap_or(dir);
        if matches!(folder, "" | "." | "..") {
            return None;
        }
        return Some(format!("{dir}.rs"));
    }
    Some(match dir {
        Some(dir) => format!("{dir}/{stem}/mod.rs"),
        None => format!("{stem}/mod.rs"),
    })
}

/// Splits a rule's dependencies on unescaped spaces. `\ ` is a space in a path; any other `\`
/// is itself. `None` when the text ends in a `\`.
fn split_deps(text: &str) -> Option<Vec<String>> {
    let mut files = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                Some(' ') => {
                    current.push(' ');
                    chars.next();
                }
                Some(_) => current.push('\\'),
                None => return None,
            },
            ' ' => {
                if !current.is_empty() {
                    files.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        files.push(current);
    }
    Some(files)
}

/// A dependency as rustc writes it: a space becomes `\ `.
fn escape_file(file: &str) -> String {
    file.replace(' ', "\\ ")
}

/// A variable's name or value as rustc writes it on one line.
fn escape_env(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}

/// Undoes [`escape_env`]. `None` for an escape rustc doesn't write.
fn unescape_env(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What rustc 1.94.0 wrote for `--emit=dep-info,metadata` on a crate under `my dir/` with
    /// `mod foo;` (in `foo/mod.rs`), `mod bar;`, `env!("SOME_VAR")`, `option_env!("UNSET_ONE")`,
    /// `include_str!("data.txt")` and `env!("WEIRD")` where WEIRD was `a\b`, newline, `c`,
    /// carriage return, `d`. The `out/` folder has no space: targets are not escaped.
    const RUSTC_RELATIVE: &str =
        "out/demo-abc.d: my\\ dir/lib.rs my\\ dir/foo/mod.rs my\\ dir/bar.rs my\\ dir/data.txt

out/libdemo-abc.rmeta: my\\ dir/lib.rs my\\ dir/foo/mod.rs my\\ dir/bar.rs my\\ dir/data.txt

my\\ dir/lib.rs:
my\\ dir/foo/mod.rs:
my\\ dir/bar.rs:
my\\ dir/data.txt:

# env-dep:SOME_VAR=plain
# env-dep:UNSET_ONE
# env-dep:WEIRD=a\\\\b\\nc\\rd
";

    /// rustc 1.94.0 with `--out-dir "out dir"`: the targets keep their space unescaped.
    const RUSTC_SPACED_TARGET: &str = "out dir/sp-x.d: src/lib.rs

out dir/libsp-x.rmeta: src/lib.rs

src/lib.rs:
";

    /// The shape of what cargo 1.94.0 had rustc write for a workspace member with
    /// `--emit=dep-info,metadata,link`: absolute targets, sources relative to the workspace, no
    /// variables, so no trailing blank line.
    const CARGO_MEMBER: &str = "/w/target/debug/deps/hushrun_build-55ed56aeb232fcd2.d: crates/hushrun-build/src/lib.rs crates/hushrun-build/src/audit.rs

/w/target/debug/deps/libhushrun_build-55ed56aeb232fcd2.rlib: crates/hushrun-build/src/lib.rs crates/hushrun-build/src/audit.rs

/w/target/debug/deps/libhushrun_build-55ed56aeb232fcd2.rmeta: crates/hushrun-build/src/lib.rs crates/hushrun-build/src/audit.rs

crates/hushrun-build/src/lib.rs:
crates/hushrun-build/src/audit.rs:
";

    /// rustc 1.94.0 sorted these variables by name although the source read ZED first; the
    /// value of ALPHA holds `:`, ` ` and `#`, none of them escaped.
    const RUSTC_ENV_ORDER: &str = "/t/out2/demo-abc.d: /t/my\\ dir/lib.rs

/t/out2/libdemo-abc.rlib: /t/my\\ dir/lib.rs

/t/out2/libdemo-abc.rmeta: /t/my\\ dir/lib.rs

/t/my\\ dir/lib.rs:

# env-dep:ALPHA=x:y #z
# env-dep:SOME_VAR=plain
# env-dep:UNSET_ONE
# env-dep:ZED=z
";

    fn env(name: &str, value: Option<&str>) -> EnvDep {
        EnvDep {
            name: name.to_owned(),
            value: value.map(str::to_owned),
        }
    }

    // covers: B-6
    #[test]
    fn reads_files_once_unescaped_and_env_with_escapes() {
        let info = parse(RUSTC_RELATIVE).unwrap();
        assert_eq!(info.targets, ["out/demo-abc.d", "out/libdemo-abc.rmeta"]);
        assert_eq!(
            info.files,
            [
                "my dir/lib.rs",
                "my dir/foo/mod.rs",
                "my dir/bar.rs",
                "my dir/data.txt"
            ]
        );
        assert_eq!(
            info.env,
            [
                env("SOME_VAR", Some("plain")),
                env("UNSET_ONE", None),
                env("WEIRD", Some("a\\b\nc\rd")),
            ]
        );
    }

    #[test]
    fn targets_are_not_unescaped() {
        let info = parse(RUSTC_SPACED_TARGET).unwrap();
        assert_eq!(info.targets, ["out dir/sp-x.d", "out dir/libsp-x.rmeta"]);
        assert_eq!(info.files, ["src/lib.rs"]);
    }

    #[test]
    fn render_gives_rustc_bytes_back() {
        for text in [
            RUSTC_RELATIVE,
            RUSTC_SPACED_TARGET,
            CARGO_MEMBER,
            RUSTC_ENV_ORDER,
        ] {
            let info = parse(text).unwrap();
            assert_eq!(render(&info), text);
            assert_eq!(parse(&render(&info)).unwrap(), info);
            info.check().unwrap();
            assert_eq!(cargo_reads(&render(&info)), (info.files, info.env));
        }
    }

    /// What cargo 1.94.0's `parse_rustc_dep_info` takes from a dep-info file, ported line for
    /// line: the first line holding ": ", split on whitespace, a token ending in `\` joined to
    /// the next; and the `# env-dep:` lines.
    fn cargo_reads(text: &str) -> (Vec<String>, Vec<EnvDep>) {
        let mut files = Vec::new();
        let mut env = Vec::new();
        let mut found_deps = false;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("# env-dep:") {
                let mut parts = rest.splitn(2, '=');
                let name = unescape_env(parts.next().unwrap()).unwrap();
                let value = parts.next().map(|v| unescape_env(v).unwrap());
                env.push(EnvDep { name, value });
            } else if let Some(pos) = line.find(": ") {
                if found_deps {
                    continue;
                }
                found_deps = true;
                let mut deps = line[pos + 2..].split_whitespace();
                while let Some(s) = deps.next() {
                    let mut file = s.to_string();
                    while file.ends_with('\\') {
                        file.pop();
                        file.push(' ');
                        file.push_str(deps.next().unwrap());
                    }
                    if !files.contains(&file) {
                        files.push(file);
                    }
                }
            }
        }
        (files, env)
    }

    #[test]
    fn render_sorts_variables_as_rustc_does() {
        let info = DepInfo {
            targets: vec!["t.d".into()],
            files: vec!["a.rs".into()],
            env: vec![env("ZED", Some("z")), env("ALPHA", None)],
        };
        assert!(render(&info).ends_with("\n\n# env-dep:ALPHA\n# env-dep:ZED=z\n"));
    }

    #[test]
    fn ignores_checksums_and_dep_rules_but_refuses_garbage() {
        let text = "t.d: a.rs\n\na.rs:\n# checksum:sha256=00 file_len:3 a.rs\n";
        let info = parse(text).unwrap();
        assert_eq!(info.files, ["a.rs"]);
        assert!(info.env.is_empty());
        assert_eq!(
            parse("t.d: a.rs\nnonsense\n"),
            Err(Error::Unexpected { line: 2 })
        );
        assert_eq!(parse("a.rs:\n"), Err(Error::NoTarget));
        assert_eq!(
            parse("t.d: a\\\n"),
            Err(Error::TrailingBackslash { line: 1 })
        );
        assert_eq!(
            parse("t.d: a\n# env-dep:X=a\\t\n"),
            Err(Error::Escape { line: 2 })
        );
    }

    #[test]
    fn a_backslash_before_a_space_in_a_path_survives() {
        // A path holding `\ ` is written `\\ `; the first backslash is itself.
        let info = DepInfo {
            targets: vec!["t.d".into()],
            files: vec!["odd\\ name.rs".into(), "c:\\x.rs".into()],
            env: vec![env("A", Some("=\\=")), env("B", Some(""))],
        };
        assert_eq!(parse(&render(&info)).unwrap(), info);
        info.check().unwrap();
        assert_eq!(cargo_reads(&render(&info)), (info.files, info.env));
    }

    #[test]
    fn several_targets_repeat_files_once() {
        let info = parse("a.d: x.rs y.rs\n\nb.rmeta: y.rs z.rs\n").unwrap();
        assert_eq!(info.files, ["x.rs", "y.rs", "z.rs"]);
    }

    #[test]
    fn maps_paths_and_values_for_another_folder() {
        let info = parse(RUSTC_ENV_ORDER)
            .unwrap()
            .map_paths(|p| p.replace("/t/", "/other place/"))
            .map_env(|v| v.to_uppercase());
        assert_eq!(info.targets[0], "/other place/out2/demo-abc.d");
        assert_eq!(info.files, ["/other place/my dir/lib.rs"]);
        assert_eq!(info.env[2], env("UNSET_ONE", None));
        assert_eq!(info.env[3], env("ZED", Some("Z")));
        let text = render(&info);
        assert!(
            text.starts_with("/other place/out2/demo-abc.d: /other\\ place/my\\ dir/lib.rs\n\n")
        );
        assert_eq!(parse(&text).unwrap(), info);
    }

    #[test]
    fn check_names_what_cargo_would_misread() {
        let base = DepInfo {
            targets: vec!["t.d".into()],
            files: vec!["a.rs".into()],
            env: vec![],
        };
        let with_file = |f: &str| DepInfo {
            files: vec![f.to_owned()],
            ..base.clone()
        };
        for bad in [
            "tab\there.rs",
            "nbsp\u{a0}.rs",
            "ends\\",
            "line\nbreak.rs",
            "#x.rs",
            "",
        ] {
            assert_eq!(
                with_file(bad).check(),
                Err(Error::Unwritable(bad.to_owned()))
            );
        }
        let target = DepInfo {
            targets: vec!["a: b".into()],
            ..base.clone()
        };
        assert!(target.check().is_err());
        let name = DepInfo {
            env: vec![env("A=B", None)],
            ..base
        };
        assert!(name.check().is_err());
    }

    // covers: B-6
    #[test]
    fn module_candidates_go_both_ways() {
        assert_eq!(
            module_candidate("crates/a/src/foo.rs").as_deref(),
            Some("crates/a/src/foo/mod.rs")
        );
        assert_eq!(
            module_candidate("/abs/src/foo/mod.rs").as_deref(),
            Some("/abs/src/foo.rs")
        );
        assert_eq!(module_candidate("lib.rs").as_deref(), Some("lib/mod.rs"));
        assert_eq!(module_candidate("foo/mod.rs").as_deref(), Some("foo.rs"));
        for none in [
            "data.txt",
            "src/foo.rsx",
            "mod.rs",
            "./mod.rs",
            "/mod.rs",
            "src/.rs",
        ] {
            assert_eq!(module_candidate(none), None, "{none}");
        }
    }
}
