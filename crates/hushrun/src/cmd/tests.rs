//! `hushrun tests resume NAME` (C-34, \[ADR 0069\]): a person turns a check's reuse of its test
//! files on again, once an audit run's disagreement suspended it. It reads the check's reuse state
//! where it lives (the server's, or this machine's in local mode), says who suspended it and when,
//! and turns it on again naming the suspension it read: a check suspended again since, or turned
//! on by someone else, is left as it is, and said. A coding agent can't: a person decides that a
//! disagreement was understood (C-23's rule of who is an agent).

use anyhow::anyhow;

use hushrun_ci::Env;

use crate::cli::{GlobalOpts, TestsCommand, TestsResumeArgs};
use crate::cmd::skill::repository_root;
use crate::coding_agent::agent;
use crate::exit::{CmdResult, Exit, Failure};
use crate::results::{Backend, Place, Selection};
use crate::units::store::{self, Resumed, UnitResults};

pub fn run(global: &GlobalOpts, command: &TestsCommand) -> CmdResult {
    match command {
        TestsCommand::Resume(args) => resume(global, args),
    }
}

fn resume(global: &GlobalOpts, args: &TestsResumeArgs) -> CmdResult {
    let check = args.check.as_str();
    let env = Env::from_process();
    if let Some(agent) = agent(&|name| env.get(name).map(str::to_string)) {
        return Err(Failure::new(
            Exit::Usage,
            anyhow!(
                "a person turns reuse of {check}'s test files on again, once they understand why a test file failed twice ({agent} is a coding agent): ask them to run `hushrun tests resume {check}`"
            ),
        ));
    }
    let root = repository_root(global).ok_or_else(|| {
        Failure::new(
            Exit::Usage,
            anyhow!("this isn't a git repository: run it in the repository whose check it is"),
        )
    })?;
    let repository = crate::cmd::wrap::results_repository(global);
    let backend = match Backend::select(Some(&root)) {
        Ok(Selection::On(backend)) => backend,
        Ok(Selection::Off(off)) => {
            return Err(Failure::new(
                Exit::Usage,
                anyhow!(
                    "results are off ({}), so nothing reuses test files",
                    off.reason()
                ),
            ));
        }
        Err(err) => return Err(Failure::new(Exit::Usage, err)),
    };
    let cache = crate::context::cache_dir();
    let held: Box<dyn UnitResults + '_> = match backend.place() {
        Place::Server { base, .. } => Box::new(store::Server {
            backend: &backend,
            base,
            check: check.to_string(),
            repository: repository.clone(),
            job: None,
            variant: None,
            cache: cache
                .ok_or_else(|| Failure::new(Exit::Usage, anyhow!("there is no cache directory")))?,
        }),
        Place::Directory { .. } => Box::new(
            crate::units::local_store(repository.as_deref(), check)
                .ok_or_else(|| Failure::new(Exit::Usage, anyhow!("there is no cache directory")))?,
        ),
    };
    let now = held
        .lookup(&[], &|_| true)
        .map_err(|why| Failure::new(Exit::Ledger, anyhow!("can't read {check}'s reuse: {why}")))?
        .reuse;
    let place = repository.as_deref().unwrap_or("this repository");
    if now.allows_reuse() {
        println!("Reuse of {check}'s test files is on for {place}.");
        return Ok(Exit::Ok);
    }
    println!(
        "Reuse of {check}'s test files for {place} was suspended{}{}{}.",
        now.suspended_by
            .as_deref()
            .map(|by| format!(" by {by}"))
            .unwrap_or_default(),
        now.suspended_at
            .as_deref()
            .map(|at| format!(" on {at}"))
            .unwrap_or_default(),
        now.unit
            .as_deref()
            .map(|unit| format!(
                ", when {unit} failed twice though it had passed on the same files it read"
            ))
            .unwrap_or_default(),
    );
    let by = match backend.credential() {
        Some(crate::results::Credential::Login { email }) => format!("user:{email}"),
        _ => "a person".to_string(),
    };
    match held.resume(now.generation, &by).map_err(|why| {
        Failure::new(
            Exit::Ledger,
            anyhow!("{check}'s reuse wasn't turned on: {why}"),
        )
    })? {
        Resumed::Done => {
            println!("Turned it on again: the next run of {check} reuses test files that passed.");
            Ok(Exit::Ok)
        }
        Resumed::Stale(after) if after.allows_reuse() => {
            println!(
                "Someone turned it on already{}.",
                after
                    .resumed_by
                    .as_deref()
                    .map(|by| format!(": {by}"))
                    .unwrap_or_default()
            );
            Ok(Exit::Ok)
        }
        Resumed::Stale(_) => Err(Failure::new(
            Exit::Unexpected,
            anyhow!(
                "it was suspended again since you read it, so it stays suspended: run `hushrun tests resume {check}` again to see why"
            ),
        )),
    }
}
