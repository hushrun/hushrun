//! `hushrun hook` (C-23): the pre-push hook. It says what CI would run again on the code being
//! pushed, and refuses a coding agent's push until that is done, so that the agent gets its next
//! step from the push itself and CI skips what passed.
//!
//! It isn't a gate: `git push --no-verify` skips it, and CI runs whatever has no pass whatever
//! happens here. So it stops a push only for what `hushrun ready` calls `missing`. A crash, a
//! timeout, a server that doesn't answer or a configuration it can't read all let the push
//! through, and the shell lines it installs only stop one on exit code 10.
//!
//! \[ADR 0043\] git doesn't clone hooks, so `hushrun ready` and `hushrun run` put it in a clone that
//! has none and hasn't said no ([`ensure`]). Nothing here is ever the reason a command fails.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use serde_json::Value;

use hushrun_ci::Env;
use hushrun_core::policy::POLICY_FILE;
use hushrun_vcs::Snapshot;

use crate::cli::{GlobalOpts, HookCommand, HookMode, PrePushArgs};
use crate::cmd::init::{origin_repository, repository_of};
use crate::cmd::ready::{self, Report, Target, git};
use crate::coding_agent::agent;
use crate::context;
use crate::exit::{CmdResult, Exit, Failure};

/// What marks hushrun's lines in a hook file.
const START: &str = "# hushrun:start";
const END: &str = "# hushrun:end";

/// The most the hook may hold a push up.
const BUDGET: Duration = Duration::from_secs(15);

/// The npm script that puts the hook in each clone.
pub(crate) const INSTALL_SCRIPT: &str = "hushrun hook install || exit 0";

/// \[ADR 0043\] The git setting that says this clone doesn't want the hook. `hushrun hook
/// uninstall` sets it, and \[ADR 0061\] only someone running `hushrun hook install` clears it. It
/// is in `.git/config`, so it stays in this clone, and isn't part of the code.
const LEFT_OUT: &str = "hushrun.hook";

/// Who the hook refuses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    /// A coding agent, and no person.
    #[default]
    Agents,
    Everyone,
    NoOne,
}

impl From<&HookMode> for Mode {
    fn from(mode: &HookMode) -> Mode {
        match (mode.refuse, mode.warn) {
            (true, _) => Mode::Everyone,
            (_, true) => Mode::NoOne,
            _ => Mode::Agents,
        }
    }
}

impl Mode {
    fn flag(self) -> &'static str {
        match self {
            Mode::Agents => "",
            Mode::Everyone => " --refuse",
            Mode::NoOne => " --warn",
        }
    }

    /// The mode of hushrun's lines in `text`, when it has them. The hook's mode is kept nowhere
    /// else: it is the flag those lines pass. Lines of a hushrun from before modes pass none, so
    /// they are the default.
    fn of(text: &str) -> Option<Mode> {
        let start = text.find(START)?;
        let block = &text[start..start + text[start..].find(END)?];
        let after = block
            .split_once("hook pre-push")
            .map_or("", |(_, after)| after);
        Some(match after.split_whitespace().next() {
            Some("--refuse") => Mode::Everyone,
            Some("--warn") => Mode::NoOne,
            _ => Mode::Agents,
        })
    }
}

/// \[ADR 0061\] Who wants the hook, which decides what happens to the one that is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    /// Someone ran `hushrun hook install`: the hook gets this mode, and an uninstall is taken
    /// back.
    Explicitly(Mode),
    /// Something puts the hook in on its way, a package manager's `prepare` or `hushrun init`:
    /// hushrun's lines keep the mode they have, a new hook gets this one, and a clone that said
    /// no with `hushrun hook uninstall` stays without.
    Implicitly(Mode),
}

impl Asked {
    /// The mode the hook should have, beside `text`, what its file holds now.
    fn mode(self, text: Option<&str>) -> Mode {
        match self {
            Asked::Explicitly(mode) => mode,
            Asked::Implicitly(new) => text.and_then(Mode::of).unwrap_or(new),
        }
    }
}

pub fn run(global: &GlobalOpts, command: &HookCommand) -> CmdResult {
    match command {
        HookCommand::Install(args) => install(global, Mode::from(&args.mode)),
        HookCommand::Uninstall => uninstall(global),
        HookCommand::PrePush(args) => Ok(pre_push(global, args)),
    }
}

/// Whether this is a CI job, which pushes for its own reasons: it gets no hook.
fn in_ci() -> bool {
    hushrun_ci::context(&Env::from_process()).in_ci
}

/// The lines that run the hook. They stop a push on exit code 10 only, so hushrun can't stop one
/// by failing, and nothing happens where hushrun isn't installed. `exit 1` says it outright: in a
/// shell without `-e`, lines of the team's after these would otherwise decide how the hook ends.
/// \[ADR 0045\] `node_modules/.bin/hushrun` is what an install put there, unless git tracks anything
/// there, by any case or through a link: then it's the repository's own program, and `git push`
/// doesn't start it.
fn lines(mode: Mode) -> String {
    format!(
        r#"{START} (`hushrun hook install` wrote these lines; https://hushrun.com/docs/for-agents)
hushrun=node_modules/.bin/hushrun
if [ ! -x "$hushrun" ] || [ -n "$(git ls-files -- ':(icase)node_modules' 2>/dev/null)" ]; then
  hushrun=hushrun
fi
if command -v "$hushrun" >/dev/null 2>&1; then
  "$hushrun" hook pre-push{} "$@" || [ "$?" -ne 10 ] || exit 1
fi
{END}
"#,
        mode.flag()
    )
}

/// `text` with `new` where hushrun's lines are, when it has them.
fn replace_lines(text: &str, new: &str) -> Option<String> {
    let start = text.find(START)?;
    let end = start + text[start..].find(END)? + END.len();
    let rest = text[end..].strip_prefix('\n').unwrap_or(&text[end..]);
    Some(format!("{}{new}{rest}", &text[..start]))
}

/// Where the hook goes.
pub(crate) enum Place {
    /// `.husky/pre-push`, which the repository keeps: shell lines of the team's, to add to.
    Husky(PathBuf),
    /// A lefthook configuration, for a person to add to: hushrun doesn't edit its YAML.
    Lefthook(String),
    /// A hook file of git's.
    File(PathBuf),
}

/// Where a tool the repository uses keeps its hooks: in a file the repository tracks.
fn tracked_place(root: &Path) -> Option<Place> {
    if root.join(".husky").is_dir() {
        return Some(Place::Husky(root.join(".husky/pre-push")));
    }
    [
        "lefthook.yml",
        "lefthook.yaml",
        ".lefthook.yml",
        ".lefthook.yaml",
    ]
    .into_iter()
    .find(|name| root.join(name).exists())
    .map(|name| Place::Lefthook(name.to_string()))
}

/// Where the repository at `root` keeps its pre-push hook.
pub(crate) fn place(root: &Path) -> anyhow::Result<Place> {
    if let Some(place) = tracked_place(root) {
        return Ok(place);
    }
    let hooks = git(root, &["rev-parse", "--git-path", "hooks"])
        .ok_or_else(|| anyhow!("git doesn't say where this repository's hooks go"))?;
    Ok(Place::File(root.join(hooks).join("pre-push")))
}

/// How a person sees `path`.
fn shown(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// What installing the hook did.
pub(crate) enum Installed {
    Wrote(String),
    Already(String),
    /// \[ADR 0061\] Nothing: `hushrun hook uninstall` said this clone goes without.
    LeftOut,
    /// Nothing: a person adds these lines to the file.
    ByHand {
        file: String,
        lines: String,
    },
}

impl Installed {
    pub(crate) fn say(&self) {
        match self {
            Installed::Wrote(path) => println!("Installed the pre-push hook in {path}."),
            Installed::Already(path) => println!("The pre-push hook is in {path} already."),
            Installed::LeftOut => println!(
                "This clone has no pre-push hook, as hushrun hook uninstall asked: hushrun hook install puts it back."
            ),
            Installed::ByHand { file, lines } => {
                println!("hushrun doesn't edit {file}. Add these lines to it:\n{lines}");
            }
        }
    }
}

/// Whether `hushrun hook uninstall` said this clone goes without the hook.
fn left_out(root: &Path) -> bool {
    git(root, &["config", "--local", "--get", LEFT_OUT]).is_some()
}

/// What installing the hook at `root` would touch, for init's plan.
pub(crate) fn preview(root: &Path, asked: Asked) -> Option<String> {
    if matches!(asked, Asked::Implicitly(_)) && left_out(root) {
        return None;
    }
    let path = match place(root).ok()? {
        Place::Husky(path) | Place::File(path) => path,
        Place::Lefthook(file) => return Some(file),
    };
    let text = std::fs::read_to_string(&path).ok();
    match text {
        Some(text) if text.contains(&lines(asked.mode(Some(&text)))) => None,
        _ => Some(shown(root, &path)),
    }
}

/// Puts the hook in the repository at `root`, as `asked` says: someone asking for it takes back
/// a `hushrun hook uninstall` and chooses the mode, and anything else keeps both as they are.
pub(crate) fn install_in(root: &Path, asked: Asked) -> anyhow::Result<Installed> {
    match asked {
        Asked::Explicitly(_) => {
            configure(root, &["config", "--local", "--unset", LEFT_OUT]);
        }
        Asked::Implicitly(_) if left_out(root) => return Ok(Installed::LeftOut),
        Asked::Implicitly(_) => {}
    }
    let (path, adds_to_what_is_there) = match place(root)? {
        Place::Husky(path) => (path, true),
        Place::File(path) => (path, false),
        Place::Lefthook(file) => {
            let program = match root.join("package.json").exists() {
                true => "node_modules/.bin/hushrun",
                false => "hushrun",
            };
            return Ok(Installed::ByHand {
                file,
                lines: format!(
                    "pre-push:\n  commands:\n    hushrun:\n      use_stdin: true\n      run: {program} hook pre-push{} {{1}} {{2}} || [ \"$?\" -ne 10 ]",
                    asked.mode(None).flag()
                ),
            });
        }
    };
    let name = shown(root, &path);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(anyhow!(err).context(format!("reading {name}"))),
    };
    let mode = asked.mode(text.as_deref());
    let wanted = lines(mode);
    let text = match text {
        Some(text) if text.contains(&wanted) => return Ok(Installed::Already(name)),
        Some(text) => match replace_lines(&text, &wanted) {
            // Another mode's, which someone asked to change, or an older hushrun's: replaced
            // where they are.
            Some(replaced) => replaced,
            None if adds_to_what_is_there => {
                let gap = if text.ends_with('\n') || text.is_empty() {
                    ""
                } else {
                    "\n"
                };
                format!("{text}{gap}{wanted}")
            }
            // A hook of someone else's, in a language hushrun can't tell.
            None => {
                return Ok(Installed::ByHand {
                    file: name,
                    lines: wanted.trim_end().to_string(),
                });
            }
        },
        None => {
            create(&path, mode).with_context(|| format!("writing {name}"))?;
            return Ok(Installed::Wrote(name));
        }
    };
    crate::cmd::setup::write(&path, &text).with_context(|| format!("writing {name}"))?;
    executable(&path).with_context(|| format!("making {name} executable"))?;
    Ok(Installed::Wrote(name))
}

/// Writes a new hook at `path`, unless something has that name by then. It takes the name only
/// once it is whole and executable, so a push never runs half a hook, and runs in parallel leave
/// one hook between them. Whether this call wrote it.
fn create(path: &Path, mode: Mode) -> anyhow::Result<bool> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("{} is in no directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".pre-push.hushrun-")
        .tempfile_in(dir)?;
    temp.write_all(format!("#!/bin/sh\n{}", lines(mode)).as_bytes())?;
    executable(temp.path())?;
    match temp.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(err.error.into()),
    }
}

/// \[ADR 0043\] Puts the hook in this clone, when `hushrun ready` or `hushrun run` finds none and
/// nobody chose that. It never fails a command, and says one line when it does something.
pub(crate) fn ensure(global: &GlobalOpts) {
    if in_ci() {
        return;
    }
    let Ok(start) = context::start_dir(global) else {
        return;
    };
    let Ok(hushrun_vcs::Detected::Git { root }) = hushrun_vcs::detect(&start) else {
        return;
    };
    match ensure_in(&root) {
        Ok(Some(name)) if global.quiet == 0 => {
            eprintln!(
                "hushrun: installed the pre-push hook in {name}; hushrun hook uninstall takes it out"
            );
        }
        Ok(_) => {}
        Err(err) => tracing::debug!("the pre-push hook: {err:#}"),
    }
}

/// What [`ensure`] does at `root`: the file it wrote, if it wrote one. It only ever adds a file
/// where none is, so a hook of someone else's, or hushrun's with `--refuse` or `--warn`, stays as
/// it is, and it stays out of hooks the repository tracks, which are part of the code. `hushrun
/// run` can start many times at once, from every script, so what nearly every run does is the
/// first stat, and git starts only in a clone with no hook.
fn ensure_in(root: &Path) -> anyhow::Result<Option<String>> {
    if exists(&root.join(".git/hooks/pre-push"))
        || !uses_hushrun(root)
        || tracked_place(root).is_some()
    {
        return Ok(None);
    }
    let pattern = format!(r"^(core\.hookspath|{})$", LEFT_OUT.replace('.', r"\."));
    if git(root, &["config", "--get-regexp", &pattern]).is_some() {
        return Ok(None);
    }
    let Place::File(path) = place(root)? else {
        return Ok(None);
    };
    if exists(&path) || !create(&path, Mode::default())? {
        return Ok(None);
    }
    Ok(Some(shown(root, &path)))
}

/// Whether something has this name, a link that leads nowhere included.
fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Whether the repository at `root` is set up for hushrun: it has hushrun.yaml, or its
/// package.json names hushrun, as settings, a dependency or in a script.
fn uses_hushrun(root: &Path) -> bool {
    if exists(&root.join(POLICY_FILE)) {
        return true;
    }
    let Some(package) = std::fs::read(root.join("package.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return false;
    };
    let in_scripts = package["scripts"].as_object().is_some_and(|scripts| {
        scripts
            .values()
            .filter_map(Value::as_str)
            .any(|command| command.contains(hushrun_core::PRODUCT))
    });
    package.get(hushrun_core::PRODUCT).is_some()
        || in_scripts
        || ["dependencies", "devDependencies", "optionalDependencies"]
            .iter()
            .any(|field| package[field].get(hushrun_core::PRODUCT).is_some())
}

/// Runs a `git config` that prints nothing here: whether it worked.
fn configure(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn executable(_: &Path) -> std::io::Result<()> {
    Ok(())
}

fn install(global: &GlobalOpts, mode: Mode) -> CmdResult {
    // A package manager runs this on every install, wherever that is: an image without .git, a
    // CI job. It never fails an install, and says nothing it needn't.
    let prepare = std::env::var("npm_lifecycle_event")
        .is_ok_and(|event| event == "prepare" || event == "postinstall");
    if in_ci() {
        return Ok(Exit::Ok);
    }
    let root = match git_root(global) {
        Ok(root) => root,
        Err(_) if prepare => return Ok(Exit::Ok),
        Err(failure) => return Err(failure),
    };
    // \[ADR 0043, 0061\] A package manager's install isn't someone asking for the hook: it
    // leaves a clone without one that said so, and the mode someone chose.
    let asked = match prepare {
        true => Asked::Implicitly(mode),
        false => Asked::Explicitly(mode),
    };
    match install_in(&root, asked) {
        Ok(Installed::Already(_) | Installed::LeftOut) if prepare => {}
        Ok(installed) => installed.say(),
        Err(err) if prepare => tracing::debug!("the pre-push hook: {err:#}"),
        Err(err) => return Err(Failure::new(Exit::Unexpected, err)),
    }
    Ok(Exit::Ok)
}

fn uninstall(global: &GlobalOpts) -> CmdResult {
    let root = git_root(global)?;
    take_out(&root)?;
    // \[ADR 0043\] Else the next `hushrun ready` would put it back.
    if configure(&root, &["config", "--local", LEFT_OUT, "off"]) {
        println!("hushrun leaves this clone without it until hushrun hook install.");
    } else {
        eprintln!(
            "warning: can't note that in git's configuration, so the next hushrun ready or hushrun run may install the hook again"
        );
    }
    Ok(Exit::Ok)
}

/// Takes hushrun's lines out of the hook, and says so.
fn take_out(root: &Path) -> Result<(), Failure> {
    let path = match place(root).map_err(Failure::usage)? {
        Place::Husky(path) | Place::File(path) => path,
        Place::Lefthook(file) => {
            println!("Take the hushrun commands out of {file}.");
            return Ok(());
        }
    };
    let name = shown(root, &path);
    let kept = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| replace_lines(&text, ""));
    let Some(kept) = kept else {
        println!("{name} has no hushrun lines.");
        return Ok(());
    };
    let nothing_left = kept
        .lines()
        .all(|line| line.trim().is_empty() || line.starts_with("#!"));
    let done = match nothing_left {
        true => std::fs::remove_file(&path).map_err(anyhow::Error::from),
        false => crate::cmd::setup::write(&path, &kept),
    };
    done.map_err(|err| Failure::new(Exit::Unexpected, err.context(format!("changing {name}"))))?;
    println!("Took the pre-push hook out of {name}.");
    Ok(())
}

fn git_root(global: &GlobalOpts) -> Result<PathBuf, Failure> {
    let start = context::start_dir(global)?;
    match hushrun_vcs::detect(&start).map_err(Failure::usage)? {
        hushrun_vcs::Detected::Git { root } => Ok(root),
        hushrun_vcs::Detected::None => Err(Failure::usage(anyhow!(
            "hushrun hook works on a git repository: run it inside one"
        ))),
    }
}

/// One branch a push sends.
#[derive(Debug, PartialEq, Eq)]
struct Push {
    sha: String,
    branch: String,
}

/// The branches git says it is pushing: `<local ref> <local sha> <remote ref> <remote sha>` a
/// line. A tag isn't a branch; a sha of zeros deletes one.
fn pushes(stdin: &str) -> Vec<Push> {
    let mut found: Vec<Push> = Vec::new();
    for line in stdin.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let [_, sha, remote_ref, ..] = words.as_slice() else {
            continue;
        };
        let Some(branch) = remote_ref.strip_prefix("refs/heads/") else {
            continue;
        };
        let push = Push {
            sha: (*sha).to_string(),
            branch: branch.to_string(),
        };
        if !sha.bytes().all(|b| b == b'0') && !found.contains(&push) {
            found.push(push);
        }
    }
    found
}

/// Whether the remote is another repository than the one whose CI hushrun knows about: a fork, a
/// mirror. A URL that names none, such as a path, isn't.
fn another_repository(root: &Path, url: Option<&str>) -> bool {
    let named = url.and_then(repository_of);
    match (named, origin_repository(root)) {
        (Some(named), Some(origin)) => !named.eq_ignore_ascii_case(&origin),
        _ => false,
    }
}

/// `a`, `a and b`, `a, b and c`.
fn listed(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What the hook does. An answer of any kind but "something can run here" lets the push through.
fn pre_push(global: &GlobalOpts, args: &PrePushArgs) -> Exit {
    if in_ci() {
        return Exit::Ok;
    }
    let mut stdin = String::new();
    if std::io::stdin().read_to_string(&mut stdin).is_err() {
        return Exit::Ok;
    }
    let pushes = pushes(&stdin);
    if pushes.is_empty() {
        return Exit::Ok;
    }
    let Ok(root) = git_root(global) else {
        return Exit::Ok;
    };
    if another_repository(&root, args.url.as_deref()) {
        return Exit::Ok;
    }
    let quiet = global.quiet > 0;
    // A push never waits long for an answer that only saves CI some work.
    std::thread::spawn(move || {
        std::thread::sleep(BUDGET);
        if !quiet {
            eprintln!(
                "hushrun: pushing without an answer about the checks: CI runs what has no pass"
            );
        }
        std::process::exit(Exit::Ok as i32);
    });
    let head = git(&root, &["rev-parse", "HEAD"]);
    let mut reports = Vec::new();
    for push in &pushes {
        let target = Target {
            snapshot: Snapshot::Rev(push.sha.clone()),
            flag: (head.as_deref() != Some(push.sha.as_str()))
                .then(|| format!("--rev {}", &push.sha[..push.sha.len().min(12)])),
            branch: Some(push.branch.clone()),
            scopes: None,
        };
        match ready::evaluate(global, &target) {
            Ok(report) => reports.push(report),
            Err(failure) => {
                tracing::debug!("can't tell what CI would run: {:#}", failure.error);
            }
        }
    }
    let env = Env::from_process();
    let agent = agent(&|name| match name {
        // Readable or not, or in another of hushrun's variables.
        hushrun_core::agent::VARIABLE if env.agent_set() => Some("set".to_string()),
        name => env.get(name).map(str::to_string),
    });
    let refused = match Mode::from(&args.mode) {
        Mode::Everyone => true,
        Mode::NoOne => false,
        Mode::Agents => agent.is_some(),
    };
    let mut exit = Exit::Ok;
    for report in &reports {
        if report.ready {
            let passed: Vec<&str> = report.passed().map(|item| item.check.as_str()).collect();
            if !quiet {
                // \[ADR 0040\] A check CI runs outside hushrun leaves a note on a ready report, and
                // then "CI skips" wouldn't be true.
                if !passed.is_empty() && !report.runs_outside {
                    eprintln!(
                        "hushrun: {} is ready: CI skips {}",
                        report.short(),
                        listed(&passed)
                    );
                }
                for note in &report.notes {
                    eprintln!("hushrun: {note}");
                }
            }
        } else if refused {
            eprint!("{}", refusal(report, agent.is_some()));
            exit = Exit::NotReady;
        } else if !quiet {
            eprint!("{}", warning(report));
        }
    }
    exit
}

/// What a refused push prints: the agent's next step.
fn refusal(report: &Report, agent: bool) -> String {
    let names: Vec<&str> = report.missing().map(|item| item.check.as_str()).collect();
    let mut text = format!(
        "hushrun: push refused: CI would run {} again on {}\n",
        listed(&names),
        report.short()
    );
    for item in report.missing() {
        let why = item.why.as_deref().unwrap_or_default();
        text.push_str(&format!("  {}: {why}\n", item.check));
    }
    if !report.next.is_empty() {
        text.push_str("Run these here, then push again:\n");
    }
    for command in &report.next {
        text.push_str(&format!("  {command}\n"));
    }
    for note in &report.notes {
        text.push_str(&format!("{note}\n"));
    }
    if !agent {
        text.push_str("To push anyway, git push --no-verify: CI runs them.\n");
    }
    text
}

/// What a person's push prints instead: it goes on.
fn warning(report: &Report) -> String {
    let names: Vec<&str> = report.missing().map(|item| item.check.as_str()).collect();
    let run_first = match report.next.is_empty() {
        true => "",
        false => " To skip that, run these before you push:",
    };
    let mut text = format!(
        "hushrun: CI will run {} on {}.{run_first}\n",
        listed(&names),
        report.short()
    );
    for command in &report.next {
        text.push_str(&format!("  {command}\n"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-23
    #[test]
    fn a_push_names_its_branches_and_nothing_else() {
        const SHA: &str = "1111111111111111111111111111111111111111";
        const ZERO: &str = "0000000000000000000000000000000000000000";
        let stdin = format!(
            "refs/heads/a {SHA} refs/heads/fix/cart {ZERO}\n\
             refs/tags/v1 {SHA} refs/tags/v1 {ZERO}\n\
             (delete) {ZERO} refs/heads/old {SHA}\n\
             refs/heads/a {SHA} refs/heads/fix/cart {ZERO}\n\
             not a line\n"
        );
        assert_eq!(
            pushes(&stdin),
            [Push {
                sha: SHA.into(),
                branch: "fix/cart".into()
            }]
        );
        assert!(pushes("").is_empty());
    }

    // covers: C-23
    #[test]
    fn the_lines_stop_a_push_on_exit_code_10_only() {
        for (mode, flag) in [
            (Mode::Agents, ""),
            (Mode::Everyone, " --refuse"),
            (Mode::NoOne, " --warn"),
        ] {
            let text = lines(mode);
            assert!(
                text.contains(&format!(
                    "\"$hushrun\" hook pre-push{flag} \"$@\" || [ \"$?\" -ne 10 ] || exit 1"
                )),
                "{text}"
            );
            assert_eq!(
                replace_lines(&format!("a\n{text}b\n"), "").unwrap(),
                "a\nb\n"
            );
        }
        assert_eq!(replace_lines("#!/bin/sh\nmake lint\n", ""), None);
    }

    // covers: C-23
    #[test]
    fn the_mode_is_what_the_lines_pass() {
        for mode in [Mode::Agents, Mode::Everyone, Mode::NoOne] {
            let text = format!("#!/bin/sh\nmake lint\n{}", lines(mode));
            assert_eq!(Mode::of(&text), Some(mode));
            assert_eq!(Asked::Implicitly(Mode::Everyone).mode(Some(&text)), mode);
            assert_eq!(
                Asked::Explicitly(Mode::NoOne).mode(Some(&text)),
                Mode::NoOne
            );
        }
        // An older hushrun's lines pass none, and someone else's hook has none.
        let old = "# hushrun:start\n\"$hushrun\" hook pre-push \"$@\" || [ \"$?\" -ne 10 ]\n# hushrun:end\n";
        assert_eq!(Mode::of(old), Some(Mode::Agents));
        assert_eq!(Mode::of("#!/bin/sh\nhushrun hook pre-push --warn\n"), None);
        assert_eq!(Asked::Implicitly(Mode::NoOne).mode(None), Mode::NoOne);
    }

    // covers: C-23
    #[test]
    fn a_new_hook_never_replaces_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks/pre-push");
        assert!(create(&path, Mode::default()).unwrap());
        let ours = std::fs::read_to_string(&path).unwrap();
        assert_eq!(ours, format!("#!/bin/sh\n{}", lines(Mode::default())));

        std::fs::write(&path, "theirs\n").unwrap();
        assert!(!create(&path, Mode::default()).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n");
        // Nothing of the attempt stays beside it.
        assert_eq!(
            std::fs::read_dir(dir.path().join("hooks")).unwrap().count(),
            1
        );
    }

    #[test]
    fn names_read_as_a_list() {
        assert_eq!(listed(&["a"]), "a");
        assert_eq!(listed(&["a", "b"]), "a and b");
        assert_eq!(listed(&["a", "b", "c"]), "a, b and c");
    }
}
