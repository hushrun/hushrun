//! \[ADR 0045\] What a machine says about itself, for a `counts_from` rule (C-25). It is the machine's
//! own word: this stops honest mistakes, not determined ones, and only ever runs for a check with a
//! rule.
//!
//! Only `hushrun run` starts a tool, in the check's directory, with the PATH the command gets, so a
//! version manager answers as it would for the check. The repository's code runs there anyway.
//! `hushrun ready` and the pre-push hook start nothing but git: [`unmet_here`] is all they ask.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use hushrun_core::counts_from::{Rule, version_matches};
use hushrun_core::policy::is_env_name;

use crate::cmd::setup::checks::LAUNCHERS;
use crate::cmd::wrap::program;

/// How long a tool has to answer.
const TIMEOUT: Duration = Duration::from_secs(10);

/// One tool a rule can name. Adding a tool is adding a row, and its name to
/// `hushrun_core::counts_from::TOOLS`.
struct Row {
    /// Its key in `counts_from`.
    name: &'static str,
    program: &'static str,
    args: &'static [&'static str],
    /// What a line writes before its version: `go1.25.14`, `node 24.1.0`.
    written: &'static str,
    /// Takes the version number out of what it printed.
    version: fn(&str) -> Option<String>,
    /// What a command starts this tool by.
    programs: &'static [&'static str],
    /// The exec forms that pick this tool's version.
    managers: &'static [&'static [&'static str]],
    /// The variables that pick its version for one command, beside `PATH`.
    variables: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        name: "go",
        program: "go",
        args: &["env", "GOVERSION"],
        written: "go",
        version: go_version,
        programs: &["go", "gofmt"],
        managers: &[&["goenv", "exec"], &["gvm", "use"]],
        variables: &[
            "GOTOOLCHAIN",
            "GOROOT",
            "GOENV_VERSION",
            "ASDF_GOLANG_VERSION",
            "MISE_GO_VERSION",
        ],
    },
    Row {
        name: "rust",
        program: "rustc",
        args: &["--version"],
        written: "rust ",
        version: rust_version,
        programs: &["cargo", "rustc", "rustdoc", "rustfmt", "rustup"],
        managers: &[&["rustup", "run"]],
        variables: &[
            "RUSTUP_TOOLCHAIN",
            "RUSTC",
            "ASDF_RUST_VERSION",
            "MISE_RUST_VERSION",
        ],
    },
    Row {
        name: "node",
        program: "node",
        args: &["--version"],
        written: "node ",
        version: node_version,
        programs: &["node", "nodejs"],
        managers: &[
            &["fnm", "exec"],
            &["fnm", "use"],
            &["volta", "run"],
            &["nvm", "exec"],
            &["nvm", "run"],
            &["nvm", "use"],
            &["nodenv", "exec"],
            &["n", "exec"],
            &["n", "run"],
            &["n", "use"],
            &["bunx", "--bun"],
        ],
        variables: &["NODENV_VERSION", "ASDF_NODEJS_VERSION", "MISE_NODE_VERSION"],
    },
    Row {
        name: "xcode",
        program: "xcodebuild",
        args: &["-version"],
        written: "xcode ",
        version: xcode_version,
        programs: &["xcodebuild", "xcrun", "swift", "swiftc"],
        managers: &[],
        variables: &["DEVELOPER_DIR"],
    },
];

/// Exec forms of version managers that hold every tool.
const ANY_TOOL: &[&[&str]] = &[
    &["mise", "exec"],
    &["mise", "x"],
    &["mise", "use"],
    &["asdf", "exec"],
];

/// Launchers `init` doesn't read past, beside its `LAUNCHERS`.
const MORE_LAUNCHERS: &[&[&str]] = &[&["npm", "exec"], &["pnpm", "dlx"], &["yarn", "dlx"]];

/// What only starts the words after it.
const PREFIXES: &[&str] = &["env", "nice", "time", "timeout", "export"];

/// `go1.25.14` from `go env GOVERSION`; a distribution may add words after it.
fn go_version(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.split_whitespace().next()?.strip_prefix("go"))
        .and_then(number)
}

/// `rustc 1.94.0 (4a4ef493e 2026-03-02)`.
fn rust_version(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("rustc ")?
                .split_whitespace()
                .next()
        })
        .and_then(number)
}

/// `v24.1.0`.
fn node_version(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix('v'))
        .and_then(number)
}

/// `Xcode 16.2`, and a line of its build.
fn xcode_version(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Xcode "))
        .and_then(number)
}

/// `text`, when it reads as a version: what a tool prints gets into lines and marks.
fn number(text: &str) -> Option<String> {
    let text = text.trim();
    let plain = !text.chars().any(char::is_control);
    (plain && text.len() <= 64 && text.starts_with(|c: char| c.is_ascii_digit()))
        .then(|| text.to_string())
}

/// What a tool answered.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    Version(String),
    /// It isn't on the PATH.
    Absent,
    /// It is there and didn't say, for this reason.
    Failed(String),
}

/// How a machine stands against a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judged {
    /// It meets it. `line` is what a person reads, `linux, go1.25.14`, and `tools` what the result
    /// keeps.
    Met {
        line: String,
        tools: BTreeMap<String, String>,
    },
    /// It doesn't, and this says why.
    NotMet(String),
    /// A tool the rule names didn't say, and this says which. It doesn't count either, but that
    /// isn't worth saying before a run.
    Unknown(String),
}

/// \[ADR 0045\] Why this machine doesn't meet what of `rule` needs nothing started: `os`, `arch` and
/// `env`. It is all `hushrun ready` and the hook ask, and they start no tool to find out more.
pub fn unmet_here(rule: &Rule) -> Option<String> {
    let unmet = system_unmet(rule);
    (!unmet.is_empty()).then(|| unmet.join("; "))
}

fn system_unmet(rule: &Rule) -> Vec<String> {
    let platform = hushrun_ci::platform();
    let (os, arch) = os_and_arch(&platform);
    let mut unmet = Vec::new();
    if !rule.os.is_empty() && !rule.os.iter().any(|named| named == os) {
        unmet.push(format!("needs {}, this is {os}", either(&rule.os)));
    }
    if !rule.arch.is_empty() && !rule.arch.iter().any(|named| named == arch) {
        unmet.push(format!("needs {}, this is {arch}", either(&rule.arch)));
    }
    for (name, values) in &rule.env {
        // Control characters can be written in YAML, and this goes to a terminal.
        let needs: String = format!("needs {name} {}", either(values))
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        // What it holds is never said: it may be anything. And \[ADR 0046\] it's read as hushrun
        // reads its own variables, so a string dropped from one isn't there.
        match hushrun_ci::var(name) {
            Ok(value) if values.contains(&value) => {}
            Err(std::env::VarError::NotPresent) => {
                unmet.push(format!("{needs}, and it isn't set"));
            }
            _ => unmet.push(format!("{needs}, and it holds something else")),
        }
    }
    unmet
}

/// How this machine stands against `rule` for a run of `command` in `dir`, under the repository's
/// `root`. It starts the tools the rule names.
pub fn judge(rule: &Rule, command: &[String], dir: &Path, root: &Path) -> Judged {
    let platform = hushrun_ci::platform();
    let (os, arch) = os_and_arch(&platform);
    let mut unmet = system_unmet(rule);
    let mut unknown = Vec::new();
    let mut line = vec![os.to_string()];
    let mut tools = BTreeMap::new();
    if !rule.arch.is_empty() {
        line.push(arch.to_string());
    }
    for row in ROWS {
        let Some(prefixes) = rule.tools.get(row.name) else {
            continue;
        };
        let needs = format!("needs {} {}", row.name, either(prefixes));
        if let Some(how) = picks_its_own(command, row) {
            unmet.push(format!("{needs}, and {how} picks its own toolchain"));
            continue;
        }
        // Every PATH it may run by has to meet the rule.
        for path in paths_of(row, dir, root) {
            match ask(row, dir, &path, TIMEOUT) {
                Answer::Version(version)
                    if prefixes.iter().any(|p| version_matches(p, &version)) =>
                {
                    let shown = format!("{}{version}", row.written);
                    if tools.insert(row.name.to_string(), version.clone()) != Some(version) {
                        line.push(shown);
                    }
                }
                Answer::Version(version) => {
                    unmet.push(format!("{needs}, this is {}{version}", row.written));
                    break;
                }
                Answer::Absent => {
                    unmet.push(format!("{needs}, and {} isn't here", row.program));
                    break;
                }
                Answer::Failed(why) => {
                    unknown.push(format!(
                        "{needs}, and hushrun can't tell which {} this is: {why}",
                        row.name
                    ));
                    break;
                }
            }
        }
    }
    if !unmet.is_empty() {
        unmet.extend(unknown);
        Judged::NotMet(unmet.join("; "))
    } else if !unknown.is_empty() {
        Judged::Unknown(unknown.join("; "))
    } else {
        Judged::Met {
            line: line.join(", "),
            tools,
        }
    }
}

/// The PATHs `row`'s tool may be found by in a run: this process's, which is the command's, and for
/// `node`, that with each `node_modules/.bin` that holds one first, from `dir` up to `root`, as npm
/// runs what a script starts. Nothing here says whether COMMAND goes through npm: `npm test` runs on
/// that node and `node t.js` on the other, so a rule has to hold for both.
fn paths_of(row: &Row, dir: &Path, root: &Path) -> Vec<OsString> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let installed = match row.name {
        "node" => installed_node(dir, root),
        _ => Vec::new(),
    };
    let dirs = installed
        .iter()
        .cloned()
        .chain(std::env::split_paths(&path));
    let first = (!installed.is_empty())
        .then(|| std::env::join_paths(dirs).ok())
        .flatten();
    std::iter::once(path).chain(first).collect()
}

fn installed_node(dir: &Path, root: &Path) -> Vec<PathBuf> {
    let real = |path: &Path| std::fs::canonicalize(path).ok();
    let (Some(dir), Some(root)) = (real(dir), real(root)) else {
        return Vec::new();
    };
    let holds_node = |bin: &PathBuf| {
        ["node", "node.exe", "node.cmd"]
            .iter()
            .any(|name| bin.join(name).exists())
    };
    dir.ancestors()
        .take_while(|dir| dir.starts_with(&root))
        .map(|dir| dir.join("node_modules/.bin"))
        .filter(holds_node)
        .collect()
}

/// What `row`'s tool says in `dir`, found by `path`, within `timeout`.
fn ask(row: &Row, dir: &Path, path: &OsStr, timeout: Duration) -> Answer {
    if !dir.is_dir() {
        return Answer::Failed(format!("{} isn't here", dir.display()));
    }
    let mut command = Command::new(program(row.program, dir, path));
    command
        .args(row.args)
        .current_dir(dir)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Answer::Absent,
        Err(err) => return Answer::Failed(err.to_string()),
    };
    // A reader of its own: a tool that hangs, or leaves a child holding its stdout, can't hold
    // this up.
    let (send, receive) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut text = Vec::new();
            let _ = stdout.take(1 << 16).read_to_end(&mut text);
            let _ = send.send(text);
        });
    }
    let deadline = Instant::now() + timeout;
    let Ok(text) = receive.recv_timeout(timeout) else {
        let _ = child.kill();
        let _ = child.wait();
        return Answer::Failed(format!("{} didn't answer", row.program));
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Answer::Failed(format!("{} didn't answer", row.program));
            }
        }
    };
    if !status.success() {
        return Answer::Failed(format!("{} {status}", row.program));
    }
    match (row.version)(&String::from_utf8_lossy(&text)) {
        Some(version) => Answer::Version(version),
        None => Answer::Failed(format!("{} said no version", row.program)),
    }
}

/// What `counts_from` calls the system `platform` names, as E-7 writes it.
fn os_and_arch(platform: &str) -> (&str, &str) {
    match platform.split_once('/') {
        Some(("darwin", arch)) => ("macos", arch),
        Some((os, arch)) => (os, arch),
        None => (platform, ""),
    }
}

/// A program's name, without its directory or a Windows extension.
fn name_of(word: &str) -> &str {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    match name.rsplit_once('.') {
        Some((stem, ext))
            if ["exe", "cmd", "bat"]
                .iter()
                .any(|e| ext.eq_ignore_ascii_case(e)) =>
        {
            stem
        }
        _ => name,
    }
}

/// What in `command` chooses `row`'s tool for itself, so that what this machine's PATH finds says
/// nothing of the run. These are words, read for honest mistakes: ADR 0045 lists what they miss.
fn picks_its_own(command: &[String], row: &Row) -> Option<String> {
    let words: Vec<&str> = command.iter().map(String::as_str).collect();
    scan(&words, row)
}

fn scan(words: &[&str], row: &Row) -> Option<String> {
    let mut rest = words;
    while let Some((word, after)) = rest.split_first() {
        match word.split_once('=') {
            Some((name, _)) if is_env_name(name) => {
                if name == "PATH" || row.variables.contains(&name) {
                    return Some(name.to_string());
                }
                rest = after;
            }
            _ if PREFIXES.contains(&name_of(word)) => rest = past_options(after),
            _ => break,
        }
    }
    let (program, args) = rest.split_first()?;
    let name = name_of(program);
    if let Some(text) = shell_string(name, args) {
        return commands_in(&text).iter().find_map(|words| scan(words, row));
    }
    // By its path, it is whatever that file is, not what PATH finds.
    if row.programs.contains(&name) && program.contains(['/', '\\']) {
        return Some((*program).to_string());
    }
    let form_of = |form: &&&[&str]| form[0] == name && args.starts_with(&form[1..]);
    if let Some(form) = row.managers.iter().chain(ANY_TOOL).find(form_of) {
        return Some(form.join(" "));
    }
    if row.programs.contains(&name) {
        return args
            .first()
            .filter(|word| word.starts_with('+'))
            .map(|word| format!("{name} {word}"));
    }
    // A version as all that is left of the name: `go1.24.3`, not `cargo2junit`.
    let versioned = |program: &&str| {
        name.strip_prefix(*program).is_some_and(|rest| {
            rest.starts_with(|c: char| c.is_ascii_digit())
                && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
    };
    if row.programs.iter().any(versioned) {
        return Some(name.to_string());
    }
    let launcher = LAUNCHERS
        .iter()
        .chain(MORE_LAUNCHERS)
        .find(|launcher| launcher[0] == name && args.starts_with(&launcher[1..]))?;
    let started = |name: &&str| {
        let name = name.split('@').next().unwrap_or(name);
        row.programs.contains(&name_of(name))
    };
    launched(&args[launcher.len() - 1..])
        .iter()
        .any(started)
        .then(|| launcher.join(" "))
}

/// What a launcher's `words` start: their first word, and what its options name as packages, with
/// any `@version`. No other word counts: `--project=node@22` is the program's own option.
fn launched<'a>(words: &[&'a str]) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        if let Some(package) = word.strip_prefix("--package=") {
            found.push(package);
        } else if matches!(*word, "-p" | "--package") {
            found.extend(words.next().copied());
        } else if !word.starts_with('-') {
            found.push(word);
            break;
        }
    }
    found
}

/// `words` past a prefix's options and their values, and a number such as `timeout`'s.
fn past_options<'a, 'b>(mut words: &'a [&'b str]) -> &'a [&'b str] {
    while let Some((word, after)) = words.split_first() {
        if matches!(*word, "-n" | "-k" | "-s" | "-u") {
            words = after.get(1..).unwrap_or_default();
        } else if word.starts_with(|c: char| c == '-' || c.is_ascii_digit()) {
            words = after;
        } else {
            break;
        }
    }
    words
}

/// The string a shell `name` was given to run with `args`, as `sh -c '…'` and `cmd /c …` do.
fn shell_string(name: &str, args: &[&str]) -> Option<String> {
    if matches!(name, "sh" | "bash" | "zsh" | "dash") {
        let flag = args
            .iter()
            .position(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.ends_with('c'))?;
        return args.get(flag + 1).map(|text| (*text).to_string());
    }
    (name == "cmd"
        && args
            .first()
            .is_some_and(|arg| arg.eq_ignore_ascii_case("/c")))
    .then(|| args[1..].join(" "))
}

/// The commands of a shell's string, each as its words: `cd x && cargo test; true`.
fn commands_in(text: &str) -> Vec<Vec<&str>> {
    text.split(['\n', ';', '&', '|', '(', ')'])
        .map(|command| {
            command
                .split_whitespace()
                .map(|word| word.trim_matches(['"', '\'']))
                .collect::<Vec<_>>()
        })
        .filter(|words| !words.is_empty())
        .collect()
}

/// `a`, `a or b`, `a, b or c`.
fn either(values: &[String]) -> String {
    match values {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use hushrun_core::counts_from::TOOLS;

    fn row(name: &str) -> &'static Row {
        ROWS.iter().find(|row| row.name == name).expect("a row")
    }

    fn picks(line: &str, tool: &str) -> Option<String> {
        let words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        picks_its_own(&words, row(tool))
    }

    fn of(words: &[&str], tool: &str) -> Option<String> {
        let words: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
        picks_its_own(&words, row(tool))
    }

    /// `dir` first on this process's PATH.
    fn path_with(dir: &Path) -> OsString {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let dirs = std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&path));
        std::env::join_paths(dirs).unwrap()
    }

    #[cfg(unix)]
    fn tool(dir: &Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // covers: C-25
    #[test]
    fn every_tool_a_rule_can_name_has_a_row() {
        let names: Vec<&str> = ROWS.iter().map(|row| row.name).collect();
        assert_eq!(names, TOOLS);
    }

    // covers: C-25
    #[test]
    fn each_tool_says_its_version_its_own_way() {
        // The real outputs of go 1.26.1, rustc 1.94.0, node 20 and Xcode 27.2, and of the forms
        // others ship.
        assert_eq!(go_version("go1.26.1\n").as_deref(), Some("1.26.1"));
        assert_eq!(
            go_version("go1.22.3 (Red Hat 1.22.3-1.fc40)\n").as_deref(),
            Some("1.22.3")
        );
        assert_eq!(go_version("go1.25rc2\n").as_deref(), Some("1.25rc2"));
        assert_eq!(go_version("devel go1.26-abcdef\n"), None);
        assert_eq!(go_version("go: downloading\n"), None);
        assert_eq!(
            rust_version("rustc 1.94.0 (4a4ef493e 2026-03-02)\n").as_deref(),
            Some("1.94.0")
        );
        assert_eq!(
            rust_version("rustc 1.85.0 (4d91de4e4 2025-02-17) (Homebrew)\n").as_deref(),
            Some("1.85.0")
        );
        assert_eq!(
            rust_version("rustc 1.96.0-nightly (abc 2026-05-01)\n").as_deref(),
            Some("1.96.0-nightly")
        );
        assert_eq!(rust_version("error: no toolchain\n"), None);
        assert_eq!(node_version("v20.20.2\n").as_deref(), Some("20.20.2"));
        assert_eq!(node_version("20.20.2\n"), None);
        assert_eq!(
            xcode_version("Xcode 27.2\nBuild version 27B52\n").as_deref(),
            Some("27.2")
        );
        assert_eq!(xcode_version("xcode-select: error: …\n"), None);
        // What a tool prints goes to a terminal and into a mark.
        assert_eq!(go_version("go1.25\u{1b}[2J\n"), None);
        assert_eq!(go_version(&format!("go1.{}\n", "2".repeat(100))), None);
    }

    // covers: C-25
    #[test]
    fn commands_that_pick_their_own_toolchain() {
        for (line, tool, how) in [
            ("cargo +nightly test", "rust", "cargo +nightly"),
            ("rustc +1.80 --version", "rust", "rustc +1.80"),
            ("rustup run nightly cargo test", "rust", "rustup run"),
            ("go1.24.3 test ./...", "go", "go1.24.3"),
            ("/usr/bin/env cargo +nightly test", "rust", "cargo +nightly"),
            ("mise exec node@22 -- node t.js", "node", "mise exec"),
            ("mise x -- go test", "go", "mise x"),
            ("asdf exec go test", "go", "asdf exec"),
            ("fnm exec --using=22 npm test", "node", "fnm exec"),
            ("volta run --node 22 npm test", "node", "volta run"),
            ("nvm exec 22 node t.js", "node", "nvm exec"),
            ("npx node@22 t.js", "node", "npx"),
            ("npx -y -p node@22 vitest run", "node", "npx"),
            ("npx -p vitest -p node@22 vitest run", "node", "npx"),
            ("npx --package=node@22 vitest run", "node", "npx"),
            (
                "npm exec --package=node@22 -- node t.js",
                "node",
                "npm exec",
            ),
            ("pnpm dlx node@22 t.js", "node", "pnpm dlx"),
            ("yarn dlx node@22 t.js", "node", "yarn dlx"),
            ("bunx node t.js", "node", "bunx"),
            ("bunx --bun vitest run", "node", "bunx --bun"),
            ("pnpm exec node t.js", "node", "pnpm exec"),
            ("yarn exec node t.js", "node", "yarn exec"),
            ("uv run cargo test", "rust", "uv run"),
            ("poetry run go test", "go", "poetry run"),
            ("bundle exec xcodebuild test", "xcode", "bundle exec"),
            ("python -m go", "go", "python -m"),
            ("nvm use 22", "node", "nvm use"),
            ("fnm use 22", "node", "fnm use"),
            ("mise use go@1.24", "go", "mise use"),
        ] {
            assert_eq!(picks(line, tool).as_deref(), Some(how), "{line}");
        }
    }

    // covers: C-25
    #[test]
    fn a_tool_given_by_its_path_is_whatever_that_file_is() {
        for (line, tool) in [
            ("/abs/old/bin/go test", "go"),
            ("./go test", "go"),
            ("../tools/cargo test", "rust"),
            (
                "/Applications/Xcode_15.4.app/Contents/Developer/usr/bin/xcodebuild test",
                "xcode",
            ),
            ("node_modules/.bin/node t.js", "node"),
            ("C:\\go1\\bin\\go.exe test", "go"),
        ] {
            let word = line.split(' ').next().unwrap();
            assert_eq!(picks(line, tool).as_deref(), Some(word), "{line}");
        }
        assert_eq!(picks("./scripts/test.sh", "go"), None);
        assert_eq!(picks("/usr/local/bin/npm test", "node"), None);
    }

    // covers: C-25
    #[test]
    fn a_variable_in_the_command_picks_too() {
        for (line, tool, how) in [
            ("env PATH=/old/bin go test", "go", "PATH"),
            ("PATH=/old/bin go test", "go", "PATH"),
            ("env GOTOOLCHAIN=go1.24.3 go test", "go", "GOTOOLCHAIN"),
            ("env -i GOTOOLCHAIN=go1.24.3 go test", "go", "GOTOOLCHAIN"),
            (
                "env -u FOO GOTOOLCHAIN=go1.24.3 go test",
                "go",
                "GOTOOLCHAIN",
            ),
            ("GOROOT=/old go test", "go", "GOROOT"),
            ("GOENV_VERSION=1.24 go test", "go", "GOENV_VERSION"),
            (
                "RUSTUP_TOOLCHAIN=nightly cargo test",
                "rust",
                "RUSTUP_TOOLCHAIN",
            ),
            ("DEVELOPER_DIR=/x xcodebuild test", "xcode", "DEVELOPER_DIR"),
            (
                "ASDF_NODEJS_VERSION=18 node t.js",
                "node",
                "ASDF_NODEJS_VERSION",
            ),
            ("NODENV_VERSION=18 node t.js", "node", "NODENV_VERSION"),
            (
                "MISE_NODE_VERSION=18 node t.js",
                "node",
                "MISE_NODE_VERSION",
            ),
        ] {
            assert_eq!(picks(line, tool).as_deref(), Some(how), "{line}");
        }
    }

    // covers: C-25
    #[test]
    fn what_only_starts_a_command_is_read_past() {
        for (line, tool, how) in [
            ("nice cargo +nightly test", "rust", "cargo +nightly"),
            ("nice -n 5 cargo +nightly test", "rust", "cargo +nightly"),
            ("time cargo +nightly test", "rust", "cargo +nightly"),
            ("timeout 30 cargo +nightly test", "rust", "cargo +nightly"),
            (
                "timeout -s KILL -k 5 1m cargo +nightly test",
                "rust",
                "cargo +nightly",
            ),
            (
                "nice env GOTOOLCHAIN=x timeout 9 go test",
                "go",
                "GOTOOLCHAIN",
            ),
        ] {
            assert_eq!(picks(line, tool).as_deref(), Some(how), "{line}");
        }
    }

    // covers: C-25
    #[test]
    fn a_shells_string_is_read_the_same_way() {
        for (shell, flag, text, tool, how) in [
            ("sh", "-c", "cargo +nightly test", "rust", "cargo +nightly"),
            ("bash", "-lc", "fnm exec node t", "node", "fnm exec"),
            (
                "/bin/sh",
                "-c",
                "cd x && cargo +nightly test",
                "rust",
                "cargo +nightly",
            ),
            ("sh", "-c", "PATH=old:$PATH go test", "go", "PATH"),
            ("sh", "-c", "export PATH=old; go test", "go", "PATH"),
            ("sh", "-c", "cd src && go1.20 test; true", "go", "go1.20"),
            (
                "sh",
                "-c",
                "true && GOTOOLCHAIN=go1.20.0 go test",
                "go",
                "GOTOOLCHAIN",
            ),
            ("sh", "-c", "cd 'a b' || /old/go test", "go", "/old/go"),
            ("bash", "-lc", "nvm use 22 && npm test", "node", "nvm use"),
            ("bash", "-lc", "fnm use 22; npm test", "node", "fnm use"),
        ] {
            assert_eq!(
                of(&[shell, flag, text], tool).as_deref(),
                Some(how),
                "{text}"
            );
        }
        assert_eq!(
            of(&["sh", "-e", "-c", "GOTOOLCHAIN=x go test"], "go").as_deref(),
            Some("GOTOOLCHAIN")
        );
        assert_eq!(
            of(&["timeout", "9", "sh", "-c", "cargo +nightly test"], "rust").as_deref(),
            Some("cargo +nightly")
        );
        assert_eq!(
            of(&["cmd", "/c", "cargo", "+nightly", "test"], "rust").as_deref(),
            Some("cargo +nightly")
        );
        assert_eq!(
            of(&["cmd.exe", "/C", "go.exe test && go1.20.exe test"], "go").as_deref(),
            Some("go1.20")
        );
        assert_eq!(of(&["sh", "-c", "cd x && cargo test"], "rust"), None);
        assert_eq!(of(&["sh", "-c", "cargo test | cargo2junit"], "rust"), None);
        assert_eq!(of(&["sh", "check.sh"], "rust"), None);
    }

    // covers: C-25
    #[test]
    fn what_runs_on_the_machines_own_tool_still_counts() {
        for (line, tool) in [
            ("cargo test --locked", "rust"),
            ("go test ./...", "go"),
            ("go.exe test ./...", "go"),
            ("npx vitest run", "node"),
            ("npx eslint .", "node"),
            ("npm exec vitest", "node"),
            ("pnpm dlx vitest", "node"),
            ("pnpm exec vitest run", "node"),
            ("yarn exec tsc", "node"),
            ("npm test", "node"),
            ("node --test", "node"),
            ("cargo +nightly test", "go"),
            ("fnm exec --using=22 npm test", "go"),
            ("volta run node t.js", "rust"),
            ("uv run pytest", "go"),
            ("bundle exec rspec", "xcode"),
            ("goreleaser check", "go"),
            ("xcodebuild test -scheme App", "xcode"),
            ("FOO=bar go test", "go"),
            ("env FOO=bar go test", "go"),
            ("timeout 30 go test", "go"),
            // Whole words, at command position.
            ("npx playwright test --project=node@22", "node"),
            ("npx playwright test node@22", "node"),
            ("npm exec vitest -- --config=node@22", "node"),
            ("cargo2junit", "rust"),
            ("go2struct gen", "go"),
            ("node2nix", "node"),
        ] {
            assert_eq!(picks(line, tool), None, "{line}");
        }
    }

    fn os_rule(os: &[&str]) -> Rule {
        Rule {
            os: os.iter().map(|os| (*os).to_string()).collect(),
            ..Rule::default()
        }
    }

    fn here() -> &'static str {
        match std::env::consts::OS {
            "macos" => "macos",
            "windows" => "windows",
            _ => "linux",
        }
    }

    fn other() -> &'static str {
        if here() == "windows" {
            "linux"
        } else {
            "windows"
        }
    }

    // covers: C-25
    #[test]
    fn the_system_and_its_variables_need_nothing_started() {
        assert_eq!(unmet_here(&os_rule(&[here()])), None);
        assert_eq!(unmet_here(&os_rule(&[other(), here()])), None);
        assert_eq!(
            unmet_here(&os_rule(&[other()])),
            Some(format!("needs {}, this is {}", other(), here()))
        );
        let unset = Rule {
            env: BTreeMap::from([(
                "HUSHRUN_TEST_NOT_SET_ANYWHERE".to_string(),
                vec!["a".to_string(), "b".to_string()],
            )]),
            ..Rule::default()
        };
        assert_eq!(
            unmet_here(&unset).as_deref(),
            Some("needs HUSHRUN_TEST_NOT_SET_ANYWHERE a or b, and it isn't set")
        );
        // A value can hold what YAML can write, and only the line goes to a terminal.
        let raw = Rule {
            env: BTreeMap::from([(
                "HUSHRUN_TEST_NOT_SET_ANYWHERE".to_string(),
                vec!["a\u{1b}[2Jb\nc".to_string()],
            )]),
            ..Rule::default()
        };
        let said = unmet_here(&raw).unwrap();
        assert!(!said.chars().any(char::is_control), "{said:?}");
        // A tool's rule is the run's to judge.
        let tool = Rule {
            tools: BTreeMap::from([("go".to_string(), vec!["1".to_string()])]),
            ..Rule::default()
        };
        assert_eq!(unmet_here(&tool), None);
    }

    // covers: C-25
    #[cfg(unix)]
    #[test]
    fn a_tool_is_asked_by_the_path_given_in_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("version"), "24.1.0\n").unwrap();
        // A version manager's shim answers by where it is, and stdin is closed.
        tool(
            &bin,
            "node",
            "read -r line; [ -z \"$line\" ] && echo v$(cat version)\n",
        );
        assert_eq!(
            ask(row("node"), &work, &path_with(&bin), TIMEOUT),
            Answer::Version("24.1.0".into())
        );
    }

    // covers: C-25
    #[cfg(unix)]
    #[test]
    fn a_tool_that_cant_be_measured_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_with(dir.path());
        let failed =
            |timeout: Duration, needle: &str| match ask(row("xcode"), dir.path(), &path, timeout) {
                Answer::Failed(why) => assert!(why.contains(needle), "{why}"),
                answer => panic!("{answer:?}"),
            };
        tool(
            dir.path(),
            "xcodebuild",
            "echo 'requires Xcode' >&2; exit 1\n",
        );
        failed(TIMEOUT, "exit status: 1");
        tool(dir.path(), "xcodebuild", "echo nonsense\n");
        failed(TIMEOUT, "said no version");
        // Hung, and with a child that keeps its stdout open.
        tool(dir.path(), "xcodebuild", "sleep 30 &\nsleep 30\n");
        let started = Instant::now();
        failed(Duration::from_millis(200), "didn't answer");
        assert!(started.elapsed() < Duration::from_secs(5));
        // A directory that isn't there isn't a missing tool.
        let gone = dir.path().join("gone");
        assert!(matches!(
            ask(row("go"), &gone, &path, TIMEOUT),
            Answer::Failed(_)
        ));
    }

    // covers: C-25
    #[test]
    fn a_tool_that_isnt_there_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        // Run as a tool nobody has: this table's programs may be on the machine's PATH.
        let nobody = Row {
            name: "nobody",
            program: "hushrun-test-no-such-tool",
            args: &[],
            written: "",
            version: number,
            programs: &[],
            managers: &[],
            variables: &[],
        };
        assert_eq!(
            ask(&nobody, dir.path(), &path_with(dir.path()), TIMEOUT),
            Answer::Absent
        );
    }

    // covers: C-25
    #[test]
    fn node_is_asked_by_each_path_it_may_run_by() {
        let dir = tempfile::tempdir().unwrap();
        let top = std::fs::canonicalize(dir.path()).unwrap();
        let root = top.join("repo");
        let web = root.join("packages/web");
        for bin in [&top, &root, &web] {
            let bin = bin.join("node_modules/.bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("node"), "").unwrap();
        }
        // An install with no node of its own changes nothing.
        std::fs::create_dir_all(root.join("packages/node_modules/.bin")).unwrap();
        let path = std::env::var_os("PATH").unwrap_or_default();

        let paths = paths_of(row("node"), &web, &root);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], path);
        let dirs: Vec<PathBuf> = std::env::split_paths(&paths[1]).collect();
        // Nearest first, and nothing above the repository.
        assert_eq!(dirs[0], web.join("node_modules/.bin"));
        assert_eq!(dirs[1], root.join("node_modules/.bin"));
        assert_eq!(dirs[2..], std::env::split_paths(&path).collect::<Vec<_>>());

        assert_eq!(
            paths_of(row("go"), &web, &root),
            std::slice::from_ref(&path)
        );
        let bare = root.join("packages");
        assert_eq!(paths_of(row("node"), &bare, &bare), [path]);
    }
}
