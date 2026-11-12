//! `hushrun why NAME UNIT [-- COMMAND]` (C-37, \[ADR 0073\]): what a run of the check would do with
//! one test file, for the files on disk now, and why, from what this machine knows (RFC 0003
//! §1.3). The check's COMMAND is the one its newest run here had, as this machine's store keeps
//! it (ADR 0072), unless one is given after `--`. It reads `hushrun.yaml`, the store and one
//! batch lookup of results, read-only (the outbox isn't sent), and writes nothing (ADR 0061):
//! agents may run it. Nothing of what it reads or says goes anywhere but the lookup's keys (ADR
//! 0048).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde_json::json;

use hushrun_ci::Env;
use hushrun_core::counts_from::Rule;
use hushrun_core::names::ScopeName;
use hushrun_core::units::KeyBase;

use crate::cli::{GlobalOpts, WhyArgs};
use crate::cmd::wrap::{relative, repository_of, scope_tree};
use crate::context::{Content, Source};
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};
use crate::results::{self, Backend, Place, RunResult, Selection};
use crate::units::{self, db, store, why};
use hushrun_vcs::Snapshot;

/// The schema of `--format json`'s answer.
const SCHEMA: &str = "hushrun.why/v1";

pub fn run(global: &GlobalOpts, args: &WhyArgs) -> CmdResult {
    let check = &args.check;
    let content = Content::open(global, Source::Repo(Snapshot::Worktree)).map_err(|_| {
        Failure::new(
            Exit::Usage,
            anyhow!("this isn't a git repository: run it in the repository whose check it is"),
        )
    })?;
    let root = content.vcs.root().to_path_buf();
    let here = std::env::current_dir().unwrap_or_else(|_| root.clone());
    // \[C-39\] `UNIT::TEST`: the test file's path from the root, and the test's id as given.
    let unit = match args.unit.split_once("::") {
        Some((file, id)) => format!("{}::{id}", from_root(&root, &here, file)),
        None => from_root(&root, &here, &args.unit),
    };
    let repository = hushrun_ci::context(&Env::from_process())
        .repository
        .or_else(|| repository_of(&content));
    let tests = content
        .policy
        .as_ref()
        .and_then(|policy| policy.tests.get(check))
        .cloned();
    let cache = crate::context::cache_dir();
    let store_path = cache
        .as_deref()
        .map(|cache| db::path(cache, repository.as_deref()));

    // Which command, cwd and scope: COMMAND here, or the check's newest run here.
    let mut notes = Vec::new();
    let (command, cwd, scope) = if args.command.is_empty() {
        let rows = match &store_path {
            Some(path) => db::checks_named(path, check.as_str()).map_err(|unread| unread.why()),
            None => Err("there is no cache directory".to_string()),
        };
        match rows {
            Ok(rows) if !rows.is_empty() => {
                let newest = &rows[0];
                if rows.len() > 1 {
                    notes.push(format!(
                        "{check} ran here with {} commands: this is its newest, `{}`; name another after --",
                        rows.len(),
                        newest.command.join(" ")
                    ));
                }
                let scope = ScopeName::new(newest.scope.clone()).map_err(|e| {
                    Failure::new(Exit::Unexpected, anyhow!("the store names a scope: {e}"))
                })?;
                (newest.command.clone(), newest.cwd.clone(), scope)
            }
            found => {
                let why = match found {
                    Err(why) => format!(
                        "this machine's record of what test files read can't be read here ({why})"
                    ),
                    Ok(_) if tests.as_ref().is_none_or(|t| !t.is_on()) => format!(
                        "{check}'s `tests` is off in hushrun.yaml: its test files run with it, whole, whenever it runs"
                    ),
                    Ok(_) => format!(
                        "this machine has never seen {check} run with its test files observed, so it doesn't know its command: run it with `hushrun run {check} -- COMMAND`, or name COMMAND after --"
                    ),
                };
                return answer(global, check.as_str(), &[], &unit, None, &why, &notes);
            }
        }
    } else {
        let cwd = relative(&root, &here).map_err(|e| Failure::new(Exit::Usage, e))?;
        let subject = content
            .subject(&args.scopes, None)
            .map_err(|f| Failure::new(Exit::Usage, f.error))?;
        (args.command.clone(), cwd, subject.scope)
    };

    let (digest, tree) = scope_tree(global, &scope).map_err(|e| Failure::new(Exit::Usage, e))?;
    let external = content
        .external(&scope)
        .map_err(|e| Failure::new(Exit::Usage, e))?;
    let (_, rule) = results::run_key(
        content.policy.as_ref(),
        check,
        &command,
        &cwd,
        &scope,
        &digest,
        &external,
    )
    .map_err(|e| Failure::new(Exit::Usage, e))?;
    let base = KeyBase {
        check: check.clone(),
        command: command.clone(),
        cwd: cwd.clone(),
        scope,
        external: (!external.is_empty()).then(|| json!(external)),
        counts_from: rule.as_ref().map(Rule::normal),
    };

    // Where results are found, read-only: a server's batch lookup that sends nothing waiting in
    // the outbox, or this machine's directory.
    let backend = Backend::select(Some(&root));
    let job = hushrun_ci::result_job(&Env::from_process());
    let counts = |run: &RunResult| run.counts_in(job.as_deref(), None);
    let local;
    let server;
    let held: Result<&dyn store::UnitResults, String> = match (&backend, &cache) {
        (Ok(Selection::On(backend)), Some(cache)) => match backend.place() {
            Place::Server { base: url, .. } => {
                server = store::Server {
                    backend,
                    base: url,
                    check: check.to_string(),
                    repository: repository.clone(),
                    job: job.clone(),
                    variant: None,
                    cache: cache.clone(),
                    read_only: true,
                };
                Ok(&server)
            }
            Place::Directory { .. } => {
                match units::local_store(repository.as_deref(), check.as_str()) {
                    Some(found) => {
                        local = found;
                        Ok(&local)
                    }
                    None => Err("there is no results directory here".to_string()),
                }
            }
        },
        (Ok(Selection::On(_)), None) => Err("there is no cache directory".to_string()),
        (Ok(Selection::Off(off)), _) => Err(format!("results are off: {}", off.reason())),
        (Err(err), _) => Err(format!("{err:#}")),
    };
    let store_path = store_path.unwrap_or_else(|| PathBuf::from("-"));
    let explained = why::explain(&why::WhyInput {
        base,
        repository: repository.as_deref(),
        tests: tests.as_ref(),
        unit: &unit,
        tree: &tree,
        root: &root,
        cwd: &root.join(&cwd),
        store: &store_path,
        results: held,
        counts: &counts,
    });
    let says = explained.says.clone();
    answer(
        global,
        check.as_str(),
        &command,
        &unit,
        Some(&explained),
        &says,
        &notes,
    )
}

/// `unit`, given from `here`, from the repository's `root`, `/`-separated.
fn from_root(root: &Path, here: &Path, unit: &str) -> String {
    let given = Path::new(unit);
    let joined = if given.is_absolute() {
        given.to_path_buf()
    } else {
        here.join(given)
    };
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let joined = match (std::fs::canonicalize(&joined), joined.parent()) {
        (Ok(real), _) => real,
        // A file that is gone: its directory, as it is, and its name.
        (Err(_), Some(parent)) => std::fs::canonicalize(parent)
            .map(|p| p.join(joined.file_name().unwrap_or_default()))
            .unwrap_or(joined.clone()),
        (Err(_), None) => joined.clone(),
    };
    match joined.strip_prefix(&root) {
        Ok(inside) => {
            let parts: Vec<String> = inside
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            parts.join("/")
        }
        Err(_) => unit.replace('\\', "/"),
    }
}

/// Prints the answer, as text or as `hushrun.why/v1`.
fn answer(
    global: &GlobalOpts,
    check: &str,
    command: &[String],
    unit: &str,
    explained: Option<&why::Explanation>,
    says: &str,
    notes: &[String],
) -> CmdResult {
    let detail: Vec<String> = explained
        .map(|e| e.detail.clone())
        .unwrap_or_default()
        .into_iter()
        .chain(notes.iter().cloned())
        .collect();
    match global.format {
        Format::Json => {
            let path: Vec<serde_json::Value> = explained
                .map(|e| {
                    e.path
                        .iter()
                        .map(|hop| json!({"file": hop.file, "then": hop.then.map(|k| k.word())}))
                        .collect()
                })
                .unwrap_or_default();
            output::print_json(&json!({
                "schema": SCHEMA,
                "check": check,
                "command": command,
                "unit": explained.map_or(unit, |e| e.unit.as_str()),
                "test": explained.and_then(|e| e.test.clone()),
                "decision": explained.map_or("unknown", |e| e.decision.word()),
                "reason": explained.map_or("unknown", |e| e.reason),
                "says": says,
                "detail": detail,
                "asFarAsThisMachineKnows": explained.is_none_or(|e| e.here_only),
                "file": explained.and_then(|e| e.file.clone()),
                "path": path,
            }))
            .map_err(|e| Failure::new(Exit::Unexpected, e))?;
        }
        _ => {
            let mut text = format!("{says}\n");
            for line in &detail {
                let _ = writeln!(text, "  {line}");
            }
            output::print(&text).map_err(|e| Failure::new(Exit::Unexpected, e))?;
        }
    }
    Ok(Exit::Ok)
}
