//! `hushrun init` (C-15): sets up `hushrun run` in a repository with one command, in any language.
//! It finds the checks among package.json's scripts and in CI's own files, signs in, shows each
//! change and, once agreed, wraps the checks, names the org's backend, gives a repository of
//! projects a scope each, installs hushrun, gives CI's jobs an ID token, connects the repository,
//! makes sure the org has its ledger, and shows a check skipping. While the sign-in waits, it
//! tells how often CI ran on code it had already passed (C-16). \[ADR 0041\] Someone with several
//! orgs says which one the repository is for; `--org` says it, and moves one that records to
//! another. \[ADR 0067\] `--local`, or the answer to the one question a person at a terminal with
//! no login is asked, sets up this machine only: the same wraps and agents' files, and nothing of
//! an org's, so that a plain init later adds only what sharing needs.
//!
//! `hushrun init --pipeline`, the promotion pipeline's starter files, is `cmd::init`.

mod agents;
mod binary;
mod caches;
mod cargo;
pub(crate) mod checks;
mod diff;
mod expansions;
mod json;
mod ledger;
pub(crate) mod manager;
mod orgs;
mod other_ci;
mod policy;
mod projects;
pub(crate) mod readers;
mod recipes;
mod repeats;
mod report;
mod scripts;
mod started;
mod suggestions;
mod turbo;
pub(crate) mod variants;
pub(crate) mod workflows;
mod workspaces;
mod wrapping;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, anyhow};
use serde_json::json;

use hushrun_core::ledger_url::LedgerUrl;
use hushrun_core::names::CheckName;
use hushrun_core::policy::{POLICY_FILE, Policy, ReportsCi};

use crate::cli::{GlobalOpts, InitArgs};
use crate::cmd::{hook, update};
use crate::context;
use crate::credentials::{self, Credentials};
use crate::exit::{CmdResult, Exit, Failure};
use crate::hosted::{self, Connected};
use crate::output::{self, Format};
use crate::releases;

use self::json::{Doc, Place};
use self::manager::program;
use self::scripts::Decision;
use self::workflows::Host;
use self::wrapping::{Proposal, ScriptChecks};

/// Where the org's URL goes in the plan before a sign-in names it.
const UNKNOWN_BACKEND: &str = "<your org's URL, once you sign in>";

pub fn run(global: &GlobalOpts, args: &InitArgs) -> CmdResult {
    let start = context::start_dir(global)?;
    let root = match hushrun_vcs::detect(&start).map_err(Failure::usage)? {
        hushrun_vcs::Detected::Git { root } => root,
        hushrun_vcs::Detected::None => {
            return Err(Failure::usage(anyhow!(
                "hushrun init sets up a git repository: run it inside one"
            )));
        }
    };
    // A dry run's plan as one JSON document (C-15), in place of all of its text.
    let json = args.dry_run && global.format == Format::Json;
    let mut look = Look::at(&root)?;
    if !json {
        look.print();
    }
    // \[ADR 0046\] Every login reads as signed out here, so a plan is what signing out would show.
    if hushrun_ci::Env::from_process().agent_set() {
        eprintln!("hushrun: HUSHRUN_AGENT is set, so init uses no login");
    }
    let origin = match &look.backend {
        Some(backend) => origin_of(backend)?,
        None => hosted::origin(),
    };
    let moves_to = look.moves_to(args).map(str::to_string);
    if moves_to.is_some() {
        look.can_move()?;
    }
    // \[ADR 0067\] Shared or this machine only, decided once, before anything signs in.
    if sharing(&look, &origin, args)? == Sharing::Local {
        return local(global, args, &look, json);
    }

    if look.set_up() {
        let backend = look.backend.as_deref().unwrap_or_default();
        let already =
            format!("This repository already uses hushrun: its checks share results at {backend}.");
        if json {
            let mut notes = vec![already];
            notes.extend(signs_in_and(&look, moves_to.as_deref(), backend));
            return print_plan(&look, set_up_writes(&look, notes));
        }
        println!("\n{already}");
        if args.dry_run {
            if let Some(line) = signs_in_and(&look, moves_to.as_deref(), backend) {
                println!("{line}");
            }
            let tools = AgentTools::of(&look);
            let ci = CiEdits::of(&look);
            if tools.writes() || ci.writes() {
                println!("It would also write:");
                ci.print();
                tools.print();
            }
            ci.print_manual();
            println!("Nothing changed (--dry-run).");
            return Ok(Exit::Ok);
        }
        if moves_to.is_some() && !args.yes && !std::io::stdin().is_terminal() {
            return Err(Failure::usage(anyhow!(
                "init can't ask without a terminal: add --yes to move the repository"
            )));
        }
        let github_id = ledger::GithubId::start(look.repository.as_deref());
        let session = sign_in(&look, &origin, args)?;
        if look.repository.is_none() {
            eprintln!(
                "warning: no origin remote names this repository, so init can't add it to the org: run hushrun init again once it has one"
            );
        }
        let github = github_id.and_then(ledger::GithubId::finish);
        let moved = match move_repository(&mut look, &session, args, github.as_ref())? {
            Moved::Declined => return Ok(Exit::Ok),
            Moved::Yes(ledger) => Some(ledger),
            Moved::Stayed => None,
        };
        // Every repository with a URL on hushrun gets hushrun.yaml naming it.
        let promotion = Promotion::new(&look, github.clone());
        let is_moved = moved.is_some();
        let ledger = moved.or_else(|| {
            ensure_ledger(&look, &session, github.as_ref(), promotion.default_branch())
        });
        if !session.member {
            println!("\n{}", not_a_member_next_step(&session));
            return Ok(Exit::Ok);
        }
        register_machine(&session.backend);
        let tools = AgentTools::of(&look);
        let ci = CiEdits::of(&look);
        if promotion.writes() || tools.writes() || ci.writes() {
            println!("\nhushrun init can also write:");
            if promotion.writes() {
                promotion.print();
            }
            ci.print();
            tools.print();
            let agreed = if args.yes {
                true
            } else if std::io::stdin().is_terminal() {
                ask("\nWrite it? [Y/n] ")?
            } else {
                println!("\nRun hushrun init --yes to write it.");
                false
            };
            if agreed {
                ci.write(&look.root)?;
                let wrote = tools.write(global, &look.root)?;
                if ((promotion.writes() && promotion.finish(&look, ledger.as_ref()))
                    || wrote
                    || ci.writes())
                    && !is_moved
                {
                    println!("\n{}", next_step(&look));
                }
            }
        }
        ci.print_manual();
        if let Some(line) = bun_test_line(&look) {
            println!("{line}");
        }
        if is_moved {
            println!("\n{}", next_step(&look));
        }
        return Ok(Exit::Ok);
    }

    // Without a terminal to ask on, or with --dry-run, init shows what it would do: it signs in
    // to nothing, and changes nothing.
    let asks = std::io::stdin().is_terminal();
    if args.dry_run || (!asks && !args.yes) {
        let mut notes = Vec::new();
        if let Some(org) = &moves_to {
            let line =
                format!("init moves this repository to {org}: its backend, URL and log key.");
            if json {
                notes.push(line);
            } else {
                println!("\n{line}");
            }
        }
        let backend = named_backend(&look, &origin, args).or_else(|| {
            let (backend, said) = orgs::preview(look.repository.as_deref(), &origin);
            match said {
                Some(said) if json => notes.push(said),
                Some(said) => println!("\n{said}"),
                None => {}
            }
            backend
        });
        let mut plan = Plan::new(
            &look,
            Backing::Org(backend.as_deref().unwrap_or(UNKNOWN_BACKEND)),
        )?;
        // Nothing asks gh without a sign-in: the plan says what init would derive. For a move,
        // the line above says what hushrun.yaml gets.
        if moves_to.is_none() {
            plan.promotion = Promotion::new(&look, None).blocks();
        }
        if json {
            notes.extend(plan.notes());
            return print_plan(&look, plan.writes(notes));
        }
        plan.print();
        if args.dry_run {
            println!("\nNothing changed (--dry-run).");
            return Ok(Exit::Ok);
        }
        return Err(Failure::usage(anyhow!(
            "init can't ask without a terminal: run hushrun init --yes to apply"
        )));
    }

    let repeats = repeats::start(look.repository.as_deref());
    let github_id = ledger::GithubId::start(look.repository.as_deref());
    let session = sign_in(&look, &origin, args)?;
    if let Some(line) = repeats.and_then(repeats::Pending::finish) {
        println!("{line}");
    }

    let github = github_id.and_then(ledger::GithubId::finish);
    let moved = match move_repository(&mut look, &session, args, github.as_ref())? {
        Moved::Declined => return Ok(Exit::Ok),
        Moved::Yes(ledger) => Some(ledger),
        Moved::Stayed => None,
    };
    let is_moved = moved.is_some();
    let mut plan = Plan::new(&look, Backing::Org(&session.backend))?;
    let promotion = Promotion::new(&look, github.clone());
    // ADR 0026 item 19: the plan shows the ledger before it exists.
    if let Some(repository) = look.repository.as_ref().filter(|_| session.member) {
        plan.ledger = ledger::preview(
            &session.backend,
            hosted::LedgerAsk {
                repository,
                github_repository_id: github.as_ref().map(|g| g.id),
                default_branch: promotion.default_branch(),
                dry_run: true,
            },
        );
    }
    if session.member {
        plan.promotion = promotion.blocks();
    } else {
        // Nothing of the org's, nor hushrun.yaml, which needs the URL the org gives: sign_in said
        // who can let the person in.
        plan.repository = None;
    }
    plan.print();
    if plan.changes() {
        if args.yes {
            println!("\nApplying (--yes).");
        } else if !ask("\nApply? [Y/n] ")? {
            if is_moved {
                println!("Nothing else changed: commit and push the move.");
            } else {
                println!("Nothing changed.");
            }
            return Ok(Exit::Ok);
        }
        plan.write()?;
    }
    let installed = match &plan.dependency {
        Some(spec) => install(&look, spec),
        None => true,
    };
    install_hook(&look.root);
    // A move did these first.
    let ledger = moved.or_else(|| {
        connect(&look, &session);
        ensure_ledger(&look, &session, github.as_ref(), promotion.default_branch())
    });
    if session.member {
        register_machine(&session.backend);
    }
    let wrote = session.member && promotion.finish(&look, ledger.as_ref());
    let demo = !args.no_run && (!args.yes || args.run);
    if demo && installed {
        show_a_skip(&look, &plan);
    }
    plan.tools.install_binary(global);
    println!();
    let start = session
        .dashboard
        .as_deref()
        .map(|dashboard| format!("{}/-/start", dashboard.trim_end_matches('/')));
    if look.package.is_none() {
        println!("{}", hook_in_each_clone());
    }
    if !session.member {
        println!("{}", not_a_member_next_step(&session));
    } else if wrote || is_moved || plan.files() {
        println!("{}", next_step(&look));
        if let Some(start) = &start {
            println!("  {start}");
        }
    } else if let Some(start) = &start {
        println!("Your org's start page: {start}");
    }
    if let Some(line) = bun_test_line(&look) {
        println!("{line}");
    }
    if !installed {
        let command = look.manager().map(|m| m.add(&npm_package()).join(" "));
        return Err(Failure::new(
            Exit::Unexpected,
            anyhow!(
                "hushrun isn't installed: run {} once the error above is fixed",
                command.unwrap_or_default()
            ),
        ));
    }
    Ok(Exit::Ok)
}

/// What init finds in the repository.
struct Look {
    root: PathBuf,
    /// `host/owner/name`, as run results name it.
    repository: Option<String>,
    package: Option<Package>,
    /// CI's files.
    workflows: Vec<workflows::Workflow>,
    /// What init would do to them.
    proposal: Proposal,
    /// hushrun.yaml is there, and doesn't parse: init leaves it, and what depends on it, alone.
    unreadable_policy: bool,
    /// The backend package.json or hushrun.yaml names.
    backend: Option<String>,
    /// \[ADR 0047\] The checks hushrun.yaml lets CI skip when a later step reads their reports
    /// (`reports.ci: skip`).
    reports_skip: BTreeSet<String>,
    /// \[ADR 0051\] The JavaScript workspace the root's package.json heads.
    workspace: Option<workspaces::Workspace>,
    /// \[ADR 0051\] Its packages' package.json files, by directory, read as the root's is.
    members: Vec<(String, Package)>,
    /// \[ADR 0051\] The scripts each CI job starts, the packages' too, by its file's index and id.
    job_runs: JobRuns,
    /// \[ADR 0051\] Of those, the ones each job starts under Turborepo.
    turbo_runs: JobRuns,
    /// \[ADR 0051\] What turbo.json needs so that the wrapped scripts turbo runs in CI get
    /// hushrun's variables.
    turbo: turbo::Edit,
}

/// The scripts each CI job starts, by its file's index and its id.
type JobRuns = BTreeMap<(usize, String), BTreeSet<started::Ran>>;

/// The repository root's package.json, or a package's of its workspace.
struct Package {
    doc: Doc,
    manager: manager::Detected,
    /// The scripts init looks at, in package.json's order, and what it does with each.
    checks: Vec<Check>,
}

struct Check {
    name: String,
    command: String,
    decision: Decision,
}

impl Check {
    /// What its script runs through hushrun once init is done.
    fn runs(&self) -> Vec<scripts::Run> {
        let line = match &self.decision {
            Decision::Wrap { command, .. } => command,
            _ => &self.command,
        };
        scripts::runs(line).unwrap_or_default()
    }
}

impl Look {
    fn at(root: &Path) -> Result<Look, Failure> {
        let workflows = workflows::pipelines(root);
        // \[ADR 0050\] What a tool's remote cache keeps is the tool's, in scripts too.
        let caches = caches::Caches::read(root, &workflows);
        // \[ADR 0051\] The workspace the root's package.json heads, whose packages' scripts init
        // reads as the root's.
        let workspace = workspaces::read(root);
        let file = root.join("package.json");
        let package = match std::fs::read_to_string(&file) {
            Ok(text) => Some(
                Package::read(root, &text, &caches, &|_| false)
                    .map_err(|e| Failure::usage(anyhow!("{}: {e:#}", file.display())))?,
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(Failure::usage(
                    anyhow!(err).context(format!("reading {}", file.display())),
                ));
            }
        };
        let policy = std::fs::read(root.join(POLICY_FILE))
            .ok()
            .and_then(|bytes| Policy::parse(POLICY_FILE, &bytes).ok());
        let unreadable_policy = policy.is_none() && root.join(POLICY_FILE).exists();
        let reports_skip = reports_skip(policy.as_ref());
        let mut members = Vec::new();
        let mut package = package;
        if let (Some(root_package), Some(workspace)) = (&mut package, &workspace) {
            // A package.json that isn't JSON keeps one scope, and the workspace says why.
            for member in &workspace.members {
                if let Ok(found) =
                    Package::member(&member.text, &caches, &root_package.manager, workspace)
                {
                    members.push((member.dir.clone(), found));
                }
            }
            // \[ADR 0051\] The root's script that runs each package's script stays as it is when
            // it hands off to scripts init wraps there; else it is read as before, so that a
            // package init can't read leaves the root's wrap where it was.
            let wrapped: BTreeSet<started::Ran> = members
                .iter()
                .flat_map(|(dir, member): &(String, Package)| {
                    member
                        .checks
                        .iter()
                        .filter(|check| {
                            matches!(check.decision, Decision::Wrap { .. } | Decision::Wrapped)
                        })
                        .map(move |check| started::Ran::new(dir, &check.name))
                })
                .collect();
            let hands_off = |words: &[&str]| {
                workspace.fans_out(words).is_some_and(|runs| {
                    runs.scripts.iter().any(|script| {
                        runs.packages
                            .iter()
                            .any(|dir| wrapped.contains(&started::Ran::new(dir, script)))
                    })
                })
            };
            let text = std::fs::read_to_string(&file).map_err(|e| {
                Failure::usage(anyhow!(e).context(format!("reading {}", file.display())))
            })?;
            *root_package = Package::read(root, &text, &caches, &hands_off)
                .map_err(|e| Failure::usage(anyhow!("{}: {e:#}", file.display())))?;
        }
        // \[ADR 0051\] Where package.json's wrapped scripts run, which takes a scope as CI's lines do.
        let mut dirs = BTreeSet::new();
        if package.as_ref().is_some_and(Package::wraps) {
            dirs.insert(String::new());
        }
        dirs.extend(
            members
                .iter()
                .filter(|(_, member)| member.wraps())
                .map(|(dir, _)| dir.clone()),
        );
        let placing = wrapping::Placing {
            dirs,
            workspace: workspace.clone().filter(|_| package.is_some()),
        };
        let proposal = Proposal::read_with(
            root,
            &workflows,
            policy.as_ref(),
            package.is_none(),
            &package
                .as_ref()
                .map(Package::script_checks)
                .unwrap_or_default(),
            &placing,
        );
        let mut look = Look {
            root: root.to_path_buf(),
            repository: repository_id(root),
            workspace: placing.workspace,
            package,
            workflows,
            proposal,
            unreadable_policy,
            backend: hosted::repository_backend(root),
            reports_skip,
            members,
            job_runs: BTreeMap::new(),
            turbo_runs: BTreeMap::new(),
            turbo: turbo::Edit::None,
        };
        look.scope_scripts();
        (look.job_runs, look.turbo_runs) = look.job_runs_of();
        look.turbo = look.turbo_edit();
        if look.set_up() {
            look.proposal.keep_script_runs(&look.workflows);
        }
        look.proposal.scripts = look.wrapped();
        look.proposal.script_jobs = look.script_jobs();
        Ok(look)
    }

    /// \[ADR 0051\] Puts the scope each wrapped script runs under in its `hushrun run`, when the
    /// repository has several: or leaves it, when no one scope holds its package.
    fn scope_scripts(&mut self) {
        let layout = &self.proposal.layout;
        let scoped = |dir: &str, package: &mut Package| {
            for check in &mut package.checks {
                let Decision::Wrap { command, .. } = &mut check.decision else {
                    continue;
                };
                match layout.scope_of(dir) {
                    Ok(None) => {}
                    Ok(Some(scope)) => {
                        *command = command.replace(
                            "hushrun run -- ",
                            &format!("hushrun run --scope {scope} -- "),
                        );
                    }
                    Err(why) => check.decision = Decision::Leave(why),
                }
            }
        };
        if let Some(package) = &mut self.package {
            scoped("", package);
        }
        for (dir, member) in &mut self.members {
            scoped(dir, member);
        }
    }

    /// \[ADR 0051\] Each package.json's scripts and their commands, by its directory: `""` for the
    /// root's.
    fn script_index(&self) -> started::Scripts<'_> {
        let mut commands = BTreeMap::new();
        if let Some(package) = &self.package {
            commands.insert(String::new(), script_commands(package.doc.root()));
        }
        for (dir, member) in &self.members {
            commands.insert(dir.clone(), script_commands(member.doc.root()));
        }
        started::Scripts {
            commands,
            workspace: self.workspace.as_ref(),
        }
    }

    /// \[ADR 0051\] The scripts each job starts, the root's and the packages', through what its
    /// steps and its actions' steps run; and of those, the ones it starts under Turborepo.
    fn job_runs_of(&self) -> (JobRuns, JobRuns) {
        let scripts = self.script_index();
        let mut runs = BTreeMap::new();
        let mut turbo = BTreeMap::new();
        for (index, file) in self.workflows.iter().enumerate() {
            for job in &file.jobs {
                let inside = workflows::in_actions(job, &self.workflows);
                let steps = job.steps.iter().chain(inside.iter().map(|step| step.step));
                let mut found = BTreeMap::new();
                for step in steps {
                    if let Ok(dir) = step.dir.as_deref() {
                        for (ran, under) in scripts.started_under(&step.commands, dir) {
                            *found.entry(ran).or_insert(false) |= under;
                        }
                    }
                }
                let under: BTreeSet<started::Ran> = found
                    .iter()
                    .filter(|(_, under)| **under)
                    .map(|(ran, _)| ran.clone())
                    .collect();
                if !under.is_empty() {
                    turbo.insert((index, job.id.clone()), under);
                }
                if !found.is_empty() {
                    runs.insert((index, job.id.clone()), found.into_keys().collect());
                }
            }
        }
        (runs, turbo)
    }

    /// \[ADR 0051\] What turbo.json needs, when a CI job starts a wrapped script under Turborepo:
    /// the variables of that job's CI.
    fn turbo_edit(&self) -> turbo::Edit {
        let wrapped = self.wrapped_runs();
        let (mut github, mut gitlab) = (false, false);
        for ((index, _), runs) in &self.turbo_runs {
            if runs.iter().any(|ran| wrapped.contains(ran)) {
                match self.workflows[*index].host {
                    Host::GitHub => github = true,
                    Host::GitLab => gitlab = true,
                }
            }
        }
        if !github && !gitlab {
            return turbo::Edit::None;
        }
        turbo::edit(&self.root, &turbo::needed(github, gitlab))
    }

    /// \[ADR 0051\] Whether the job starts `ran` under Turborepo, whose turbo.json keeps hushrun's
    /// variables from it until someone changes it by hand.
    fn turbo_keeps_from(&self, index: usize, job: &str, ran: &started::Ran) -> bool {
        matches!(self.turbo, turbo::Edit::Manual(_))
            && self
                .turbo_runs
                .get(&(index, job.to_string()))
                .is_some_and(|runs| runs.contains(ran))
    }

    /// \[ADR 0051\] The scripts that run through hushrun once init is done, the packages' too.
    fn wrapped_runs(&self) -> BTreeSet<started::Ran> {
        let wraps =
            |check: &&Check| matches!(check.decision, Decision::Wrap { .. } | Decision::Wrapped);
        let root = self
            .package
            .iter()
            .flat_map(|package| package.checks.iter().filter(wraps))
            .map(|check| started::Ran::new("", &check.name));
        let members = self.members.iter().flat_map(|(dir, member)| {
            member
                .checks
                .iter()
                .filter(wraps)
                .map(move |check| started::Ran::new(dir, &check.name))
        });
        root.chain(members).collect()
    }

    /// \[ADR 0051\] The jobs that start a wrapped script, by their file's index and id.
    fn script_jobs(&self) -> BTreeSet<(usize, String)> {
        let wrapped = self.wrapped_runs();
        self.job_runs
            .iter()
            .filter(|(_, runs)| runs.iter().any(|ran| wrapped.contains(ran)))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// \[ADR 0051\] The package.json at `dir`, `""` being the root's.
    fn package_at(&self, dir: &str) -> Option<&Package> {
        match dir {
            "" => self.package.as_ref(),
            dir => self
                .members
                .iter()
                .find(|(at, _)| at == dir)
                .map(|(_, member)| member),
        }
    }

    /// The org `--org` moves the repository to, when its files name another (ADR 0041).
    fn moves_to<'a>(&self, args: &'a InitArgs) -> Option<&'a str> {
        let backend = self.backend.as_deref()?;
        args.org
            .as_deref()
            .filter(|org| hosted::org_of(backend).as_deref() != Some(*org))
    }

    /// Refuses a move that can't happen, before anyone signs in or a plan promises it.
    fn can_move(&self) -> Result<(), Failure> {
        let backend = self.backend.as_deref().unwrap_or_default();
        let why = if hosted::org_of(backend).is_none() {
            format!(
                "{backend} isn't an organization's URL: change this repository's backend by hand"
            )
        } else if self.unreadable_policy {
            format!("{POLICY_FILE} doesn't parse, and it names this repository's org: fix it first")
        } else if self.repository.is_none() {
            "no origin remote names this repository, so no organization can add it: add one first"
                .to_string()
        } else {
            return Ok(());
        };
        Err(Failure::usage(anyhow!(
            "init can't move this repository: {why}"
        )))
    }

    /// Whether the repository names a backend and runs a check through hushrun already.
    fn set_up(&self) -> bool {
        let in_ci = || {
            self.workflows.iter().any(|file| {
                file.jobs.iter().any(|job| {
                    job.actions_run_hushrun
                        || job
                            .steps
                            .iter()
                            .any(|step| !scripts::line_runs(&step.commands).is_empty())
                })
            })
        };
        self.backend.is_some()
            && (self
                .names(|d| matches!(d, Decision::Wrapped))
                .next()
                .is_some()
                || in_ci())
    }

    fn manager(&self) -> Option<&manager::Detected> {
        self.package.as_ref().map(|p| &p.manager)
    }

    /// \[ADR 0043\] Where init installs hushrun: with package.json, the hook runs the dependency.
    fn binary_directory(&self) -> Option<PathBuf> {
        self.package.is_none().then(binary::directory).flatten()
    }

    /// The names of the checks whose decision `pick` takes.
    fn names(&self, pick: impl Fn(&Decision) -> bool) -> impl Iterator<Item = &str> {
        self.package
            .iter()
            .flat_map(|p| &p.checks)
            .filter(move |c| pick(&c.decision))
            .map(|c| c.name.as_str())
    }

    /// The scripts that run through hushrun once init is done.
    fn wrapped(&self) -> Vec<String> {
        self.names(|d| matches!(d, Decision::Wrap { .. } | Decision::Wrapped))
            .map(str::to_string)
            .collect()
    }

    /// \[ADR 0045\] Whether a script of package.json would come under a rule at `level`: it isn't
    /// in CI's files for init to tell where it runs, or what with.
    fn script_at(&self, level: &policy::RuleLevel) -> bool {
        match level {
            policy::RuleLevel::Top => !self.wrapped().is_empty(),
            policy::RuleLevel::Scope(scope) => self
                .package
                .iter()
                .flat_map(|package| &package.checks)
                .flat_map(Check::runs)
                .any(|run| run.scope.as_ref() == Some(scope)),
        }
    }

    /// The tool the script `name` starts that isn't installed here, if one isn't.
    fn missing_tool(&self, name: &str) -> Option<String> {
        self.package
            .iter()
            .flat_map(|package| &package.checks)
            .filter(|check| check.name == name)
            .flat_map(Check::runs)
            .filter_map(|run| run.command.into_iter().next())
            .find(|program| !Tool::of_a_script(program).found(&self.root))
    }

    fn print(&self) {
        println!("Looking at {}", self.root.display());
        let row = |label: &str, text: &str| println!("  {label:<11} {text}");
        row(
            "repository",
            self.repository
                .as_deref()
                .unwrap_or("unknown: no origin remote"),
        );
        let Some(package) = &self.package else {
            if self.proposal.is_empty() {
                row("checks", "no package.json");
                return;
            }
            row("checks", &self.proposal.names().join(", "));
            self.print_runs(&row, &[]);
            return;
        };
        let manager = &package.manager;
        row(
            "packages",
            &match &manager.because {
                Some(because) => format!("{} ({because})", manager.manager.name()),
                None => format!("{} (no lockfile)", manager.manager.name()),
            },
        );
        // \[ADR 0051\] A workspace's packages, and their checks beside the root's.
        if let Some(workspace) = &self.workspace {
            row(
                "workspace",
                &format!("{} packages ({})", workspace.members.len(), workspace.file),
            );
        }
        let mut checks: Vec<String> = package.checks.iter().map(|c| c.name.clone()).collect();
        for (dir, member) in &self.members {
            checks.extend(member.checks.iter().map(|c| format!("{} ({dir})", c.name)));
        }
        if checks.is_empty() {
            row("checks", "none among package.json's scripts");
        } else {
            row("checks", &checks.join(", "));
        }
        self.print_runs(&row, &self.wrapped());
    }

    /// \[ADR 0051\] The wrapped scripts job `job` of file `index` starts: the root's its lines
    /// name, first and in their order, as before, then those these start and the workspace
    /// packages'.
    fn job_scripts(&self, index: usize, job: &workflows::Job) -> Vec<started::Ran> {
        let wrapped = self.wrapped_runs();
        let names = self.wrapped();
        let mut order: Vec<started::Ran> = job
            .scripts
            .iter()
            .filter(|script| names.contains(script))
            .map(|script| started::Ran::new("", script))
            .collect();
        for ran in self
            .job_runs
            .get(&(index, job.id.clone()))
            .into_iter()
            .flatten()
        {
            if wrapped.contains(ran) && !order.contains(ran) {
                order.push(ran.clone());
            }
        }
        order
    }

    /// Which files run the checks, `wrapped` being package.json's.
    fn print_runs(&self, row: &dyn Fn(&str, &str), wrapped: &[String]) {
        let mut runs = Vec::new();
        for (index, workflow) in self.workflows.iter().enumerate() {
            let github = workflow.host == Host::GitHub;
            for job in workflow.jobs.iter().filter(|_| github) {
                let scripts: Vec<String> = self
                    .job_scripts(index, job)
                    .iter()
                    .map(|ran| match ran.dir.as_str() {
                        "" => ran.script.clone(),
                        dir => format!("{} ({dir})", ran.script),
                    })
                    .collect();
                if scripts.is_empty() {
                    continue;
                }
                runs.push(format!(
                    "{} runs {} (job {})",
                    workflow.path,
                    scripts.join(", "),
                    job.id
                ));
            }
            for job in self.proposal.jobs_of(index) {
                runs.push(format!(
                    "{} runs {} (job {job})",
                    workflow.path,
                    self.proposal.checks_of(index, &job).join(", ")
                ));
            }
        }
        if policy::gitlab_pipeline(&self.root, wrapped).is_some() {
            runs.push(format!("{} runs a wrapped check", policy::GITLAB_CI));
        }
        if runs.is_empty() {
            row("workflows", "none run these checks");
        }
        for (i, text) in runs.iter().enumerate() {
            row(if i == 0 { "workflows" } else { "" }, text);
        }
    }
}

impl Package {
    /// \[ADR 0051\] Whether a script of it runs through hushrun once init is done.
    fn wraps(&self) -> bool {
        self.checks
            .iter()
            .any(|check| matches!(check.decision, Decision::Wrap { .. } | Decision::Wrapped))
    }

    /// What its scripts run through hushrun once init is done.
    fn script_checks(&self) -> ScriptChecks {
        script_checks(
            self.checks
                .iter()
                .map(|check| (check.name.as_str(), check.runs())),
        )
    }

    /// `fans_out` tells a command that runs scripts of the workspace's packages that init wraps
    /// there (ADR 0051).
    fn read(
        root: &Path,
        text: &str,
        caches: &caches::Caches,
        fans_out: &dyn Fn(&[&str]) -> bool,
    ) -> anyhow::Result<Package> {
        let doc = Doc::parse(text)?;
        let top = doc.root();
        if top.members().is_none() {
            anyhow::bail!("isn't a JSON object");
        }
        let manager = manager::detect(
            root,
            top.get("packageManager").and_then(json::Node::as_str),
            top.get("workspaces").is_some(),
        );
        // \[ADR 0047\] CI whose files init doesn't edit, and that reads reports, with the scripts
        // its lines start, themselves or through the scripts they start.
        let commands = script_commands(top);
        let others: Vec<(String, BTreeSet<String>)> = other_ci::reading(root)
            .into_iter()
            .map(|other| {
                let started = scripts_started(&other.scripts, &commands);
                (other.path, started)
            })
            .collect();
        let checks = checks_of(top, caches, &others, fans_out);
        Ok(Package {
            doc,
            manager,
            checks,
        })
    }

    /// \[ADR 0051\] A package of the root's workspace: its scripts, read as the root's are, and the
    /// root's package manager.
    fn member(
        text: &str,
        caches: &caches::Caches,
        manager: &manager::Detected,
        workspace: &workspaces::Workspace,
    ) -> anyhow::Result<Package> {
        let doc = Doc::parse(text)?;
        if doc.root().members().is_none() {
            anyhow::bail!("isn't a JSON object");
        }
        let fans_out = |words: &[&str]| workspace.fans_out(words).is_some();
        let checks = checks_of(doc.root(), caches, &[], &fans_out);
        Ok(Package {
            doc,
            manager: manager.clone(),
            checks,
        })
    }
}

/// What init does with each script of `top`, a package.json, that is a check: `others` are CI
/// files init doesn't edit that read reports, with the scripts they start, and `fans_out` tells a
/// command that runs the workspace packages' scripts, which init wraps there.
fn checks_of(
    top: &json::Node,
    caches: &caches::Caches,
    others: &[(String, BTreeSet<String>)],
    fans_out: &dyn Fn(&[&str]) -> bool,
) -> Vec<Check> {
    let mut checks: Vec<Check> = Vec::new();
    {
        if let Some(members) = top.get("scripts").and_then(json::Node::members) {
            for (i, member) in members.iter().enumerate() {
                // A key given twice counts once, as its last value.
                let last = members[i + 1..].iter().all(|m| m.key != member.key);
                if let (Some(_), Some(command), true) =
                    (scripts::kind(&member.key), member.value.as_str(), last)
                {
                    // \[ADR 0051\] What runs each package's script is wrapped there, not here.
                    let plain = scripts::decide(command);
                    let mut decision = scripts::decide_with(command, fans_out);
                    // A run there can't see the step that reads what the script writes (C-26).
                    let words: Vec<&str> = command.split_whitespace().collect();
                    if let (Decision::Wrap { .. }, Some(_), Some((path, _))) = (
                        &decision,
                        checks::writes_a_report(&words),
                        others
                            .iter()
                            .find(|(_, started)| started.contains(&member.key)),
                    ) {
                        decision = Decision::Leave(format!(
                            "{path} reads reports this script may write, and hushrun can't see a skip there"
                        ));
                    }
                    // \[ADR 0050\] What a tool's remote cache reuses across machines is the tool's.
                    if let (Decision::Wrap { .. }, Some(why)) =
                        (&plain, caches.aside_script(command))
                    {
                        decision = Decision::Leave(why);
                    }
                    checks.push(Check {
                        name: member.key.clone(),
                        command: command.to_string(),
                        decision,
                    });
                }
            }
        }
    }
    checks
}

/// What init will change.
struct Plan {
    root: PathBuf,
    /// package.json's new text, and each change.
    package: Option<(String, Vec<String>)>,
    /// \[ADR 0051\] Each workspace package's package.json, from the root: its new text, and each
    /// change.
    members: Vec<(String, String, Vec<String>)>,
    /// \[ADR 0051\] How a root script that runs each package's script skips, package by package.
    fans: Vec<String>,
    /// Scripts left as they are, and why.
    left: Vec<String>,
    /// Each workflow's new text, and each change.
    workflows: Vec<(String, String, Vec<String>)>,
    /// What to change by hand.
    manual: Vec<String>,
    /// The npm package to install, when package.json gets it.
    dependency: Option<String>,
    /// How to start, when init changes no files.
    instead: Option<String>,
    /// The scripts that run through hushrun once applied.
    wrapped: Vec<String>,
    /// What it adds to hushrun.yaml: the backend, for a repository with no package.json to name
    /// it, and its scopes.
    setup: policy::Setup,
    /// The repository's id, which a new hushrun.yaml opens with. It stays when someone outside the
    /// org has nothing of the org's to change.
    written_repository: Option<String>,
    /// A file a CI file needs that init writes: its path, its text, and what it is for.
    vendored: Option<(&'static str, &'static str, &'static str)>,
    /// CI's commands left as they are, and why.
    left_in_ci: Vec<String>,
    /// \[ADR 0050\] The test scripts a person could wrap, as the text says them.
    suggestions: Vec<String>,
    /// Why the repository keeps one scope, when that isn't the obvious.
    one_scope: Option<String>,
    /// The repository init connects and gets a ledger for, in the org of `org`.
    repository: Option<String>,
    /// The org the backend's URL names, when it does.
    org: Option<String>,
    /// \[ADR 0026\] The repository's ledger, as a dry run found it.
    ledger: ledger::Preview,
    /// hushrun.yaml: a title and its lines.
    promotion: Vec<(String, Vec<String>)>,
    /// What coding agents get, package.json's script apart.
    tools: AgentTools,
    /// \[ADR 0047\] The checks CI runs every time, since a later step reads their reports, and
    /// what Codecov's file means for them.
    every_time: Vec<String>,
}

/// What init adds for coding agents (C-24): files and the hook, and, in a repository already set
/// up, the script that installs the hook, which the plan of one that isn't edits with the rest of
/// package.json.
#[derive(Default)]
struct AgentTools {
    files: Vec<agents::File>,
    /// The file the hook goes in, unless it is there already.
    hook: Option<String>,
    /// package.json's new text, and the change.
    prepare: Option<(String, String)>,
    /// \[ADR 0043\] The directory hushrun goes in, for the hook to find it.
    binary: Option<PathBuf>,
}

impl AgentTools {
    fn of(look: &Look) -> AgentTools {
        let prepare = look.package.as_ref().and_then(|package| {
            let doc = &package.doc;
            let mut editor = doc.editor();
            let mut expected = json::value(doc.text()).ok()?;
            let script = package.manager.after_install();
            let change = add_install_script(doc, script, &mut editor, &mut expected)?;
            let text = editor.finish();
            (json::value(&text).ok()? == expected).then_some((text, change))
        });
        AgentTools {
            files: agents::plan(&look.root),
            hook: hook::preview(&look.root, hook::Asked::Implicitly(hook::Mode::default())),
            prepare,
            binary: look.binary_directory(),
        }
    }

    fn writes(&self) -> bool {
        !self.files.is_empty()
            || self.hook.is_some()
            || self.prepare.is_some()
            || self.binary.is_some()
    }

    fn print(&self) {
        for file in &self.files {
            println!("  {}", file.path);
            println!("    {}", file.change);
        }
        if let Some((_, change)) = &self.prepare {
            println!("  package.json");
            println!("    {change}");
        }
        if let Some(path) = &self.hook {
            println!("  {path}");
            println!("    {HOOK_PLAN}");
        }
        if let Some(dir) = &self.binary {
            println!("  {}", dir.join("hushrun").display());
            println!("    {BINARY_PLAN}");
        }
    }

    /// Installs hushrun, when the plan says so. It never fails init: the most it costs is a
    /// warning, and the installer's command. Whether nothing is missing.
    fn install_binary(&self, global: &GlobalOpts) -> bool {
        let Some(dir) = &self.binary else {
            return true;
        };
        match update::install_newest(global, dir) {
            Ok(path) => {
                println!("Installed hushrun in {}.", path.display());
                true
            }
            Err(err) => {
                eprintln!("warning: hushrun isn't installed on this machine: {err:#}");
                false
            }
        }
    }

    /// Writes the files, and installs the hook and hushrun. Whether a file to commit changed.
    fn write(&self, global: &GlobalOpts, root: &Path) -> Result<bool, Failure> {
        if let Some((text, _)) = &self.prepare {
            write(&root.join("package.json"), text)
                .map_err(|e| Failure::new(Exit::Unexpected, e.context("writing package.json")))?;
        }
        write_agent_files(root, &self.files)?;
        if self.hook.is_some() {
            install_hook(root);
        }
        if !self.install_binary(global) {
            println!("{}", install_line());
        }
        Ok(self.prepare.is_some() || !self.files.is_empty())
    }
}

/// What the plan says of the hook.
const HOOK_PLAN: &str = "the pre-push hook: says what CI would run again, and refuses a coding agent's push until that is done";
/// What the plan says of hushrun.
const BINARY_PLAN: &str = "hushrun's newest release, on your PATH, so that the hook runs on this machine: hushrun update keeps it up to date";

/// Writes `files` under `root`, and says so.
fn write_agent_files(root: &Path, files: &[agents::File]) -> Result<(), Failure> {
    for file in files {
        let path = root.join(&file.path);
        let wrote = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(anyhow::Error::from)
            .and_then(|()| write(&path, &file.text));
        wrote.map_err(|e| {
            Failure::new(
                Exit::Unexpected,
                e.context(format!("writing {}", file.path)),
            )
        })?;
    }
    if let [first, rest @ ..] = files {
        let more = match rest.len() {
            0 => String::new(),
            n => format!(" and {n} more"),
        };
        println!("Wrote {}{more}, for coding agents.", first.path);
    }
    Ok(())
}

/// Installs the hook. It never fails init: the most it costs is a warning. \[ADR 0061\] init
/// isn't someone asking for the hook, so a hook of hushrun's keeps its mode, and a clone where
/// `hushrun hook uninstall` ran stays without.
fn install_hook(root: &Path) {
    match hook::install_in(root, hook::Asked::Implicitly(hook::Mode::default())) {
        Ok(installed) => installed.say(),
        Err(err) => eprintln!("warning: the pre-push hook isn't installed: {err:#}"),
    }
}

/// Puts each wrapped script of `package` in `editor`, and in `expected`, the value it should
/// leave: each change, as the plan says it.
fn wrap_scripts<'d>(
    package: &'d Package,
    editor: &mut json::Editor<'d>,
    expected: &mut serde_json::Value,
) -> Vec<String> {
    let scripts = package.doc.root().get("scripts");
    let mut changes = Vec::new();
    for check in &package.checks {
        let Decision::Wrap {
            command,
            vitest_run,
        } = &check.decision
        else {
            continue;
        };
        if let Some(node) = scripts.and_then(|s| s.get(&check.name)) {
            editor.set_string(node, command);
            expected["scripts"][&check.name] = json!(command);
            let why = if *vitest_run {
                " (vitest alone watches on a terminal)"
            } else {
                ""
            };
            changes.push(format!(
                "{}: {} → {}{why}",
                json::quote(&check.name),
                json::quote(&check.command),
                json::quote(command)
            ));
        }
    }
    changes
}

/// \[ADR 0051\] For each root script init leaves since it runs a script of each package, which
/// init wraps there: how the root's script skips now.
fn fan_out_lines(look: &Look) -> Vec<String> {
    let (Some(package), Some(_)) = (&look.package, &look.workspace) else {
        return Vec::new();
    };
    let wrapped = look.wrapped_runs();
    let scripts = look.script_index();
    let mut lines = Vec::new();
    for check in &package.checks {
        if !matches!(check.decision, Decision::Leave(_)) {
            continue;
        }
        let started: Vec<(started::Ran, bool)> = scripts
            .started_under(&check.command, "")
            .into_iter()
            .filter(|(ran, _)| !ran.dir.is_empty() && wrapped.contains(ran))
            .collect();
        let names: BTreeSet<&str> = started.iter().map(|(ran, _)| ran.script.as_str()).collect();
        if names.is_empty() {
            continue;
        }
        let names: Vec<&str> = names.into_iter().collect();
        // \[ADR 0051\] Under turbo, once turbo.json passes hushrun's variables on.
        let kept =
            matches!(look.turbo, turbo::Edit::Manual(_)) && started.iter().any(|(_, under)| *under);
        lines.push(format!(
            "package.json \"{}\" runs each package's {}, which init wraps there, so it skips package by package{}",
            check.name,
            names.join(" and "),
            if kept {
                ", once turbo.json is changed by hand as below"
            } else {
                ""
            }
        ));
    }
    lines
}

/// Adds the script that puts the hook in each clone (C-24), `script` being the one the package
/// manager runs after an install: the change, when it does. So a fresh sandbox gets the hook with
/// its dependencies. An install must not fail for it where hushrun isn't installed: `--omit=dev`.
fn add_install_script<'d>(
    doc: &'d Doc,
    script: &str,
    editor: &mut json::Editor<'d>,
    expected: &mut serde_json::Value,
) -> Option<String> {
    let scripts = doc.root().get("scripts")?;
    scripts.members()?;
    match scripts.get(script) {
        None => {
            editor.insert(scripts, Place::Last, script, json!(hook::INSTALL_SCRIPT));
            expected["scripts"][script] = json!(hook::INSTALL_SCRIPT);
            Some(format!(
                "{}: {}, so that every clone gets the pre-push hook",
                json::quote(script),
                json::quote(hook::INSTALL_SCRIPT)
            ))
        }
        Some(node) => {
            let command = node.as_str()?;
            if command.contains("hushrun hook") {
                return None;
            }
            let chained = format!("{command} && ({})", hook::INSTALL_SCRIPT);
            editor.set_string(node, &chained);
            expected["scripts"][script] = json!(chained);
            Some(format!(
                "{}: {} → {}",
                json::quote(script),
                json::quote(command),
                json::quote(&chained)
            ))
        }
    }
}

/// \[ADR 0067\] Where results go, as init sets the repository up: the one choice its plan, its
/// sign-in and its last line read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sharing {
    /// With CI and teammates, at the org's backend, once signed in.
    Shared,
    /// On this machine only: no sign-in, nothing names a backend, and CI runs every check.
    Local,
}

/// What a plan names for results: the org's backend, or nothing (\[ADR 0067\] `--local`).
#[derive(Debug, Clone, Copy)]
enum Backing<'a> {
    Org(&'a str),
    ThisMachine,
}

impl Plan {
    fn new(look: &Look, backing: Backing<'_>) -> Result<Plan, Failure> {
        let backend = match backing {
            Backing::Org(backend) => Some(backend),
            Backing::ThisMachine => None,
        };
        let mut plan = Plan {
            root: look.root.clone(),
            package: None,
            members: Vec::new(),
            fans: Vec::new(),
            left: Vec::new(),
            workflows: Vec::new(),
            manual: Vec::new(),
            dependency: None,
            instead: None,
            wrapped: look.wrapped(),
            setup: policy::Setup::default(),
            written_repository: look.repository.clone(),
            vendored: None,
            left_in_ci: Vec::new(),
            suggestions: suggestion_lines(look),
            one_scope: None,
            // Nothing of an org's for a repository that names none.
            repository: look.repository.clone().filter(|_| backend.is_some()),
            org: backend.and_then(hosted::org_of),
            ledger: ledger::Preview::Unknown,
            promotion: Vec::new(),
            tools: AgentTools {
                files: agents::plan(&look.root),
                hook: hook::preview(&look.root, hook::Asked::Implicitly(hook::Mode::default())),
                prepare: None,
                binary: look.binary_directory(),
            },
            every_time: Vec::new(),
        };
        // hushrun.yaml holds the scopes and the backend: without an id it can't be written, and
        // one that doesn't parse is the person's to fix.
        let wraps = ci_left_as_is(look).is_none();
        let in_ci = wraps && !look.proposal.is_empty();
        // \[ADR 0049\] A job that runs a check through hushrun already may only need to say which
        // variant or call it is.
        let identifies = wraps
            && look
                .workflows
                .iter()
                .enumerate()
                .any(|(index, workflow)| !look.proposal.changes_of(index, workflow).is_empty());
        // \[ADR 0051\] A workspace package's wrapped script counts as the root's do.
        let scripts_wrap = !look.wrapped_runs().is_empty();
        // With nothing wrapped, why is all the plan has to say of CI's files.
        if in_ci || (wraps && !scripts_wrap) {
            plan.left_in_ci = look.proposal.left.iter().map(|left| left.text()).collect();
        }
        if look.unreadable_policy && !look.proposal.is_empty() {
            plan.manual.push(format!(
                "{POLICY_FILE} doesn't parse, so init leaves it and CI's files as they are: fix it, then run hushrun init again"
            ));
        }
        match &look.package {
            None if look.repository.is_none() => {
                let share = match backend {
                    Some(backend) => format!(
                        "\nand set HUSHRUN_BACKEND={backend} wherever checks run, CI included, so that they share results."
                    ),
                    None => String::new(),
                };
                plan.instead = Some(format!(
                    "No origin remote names this repository, so init can't write hushrun.yaml, and wraps no checks. Put hushrun run NAME -- in front of a check's command, such as\n  hushrun run test -- go test ./...{share}"
                ));
                return Ok(plan);
            }
            None if !in_ci && !look.proposal.left.is_empty() => {
                plan.instead = Some(
                    "There is no package.json here, and init can't wrap the checks in CI's files, so it wraps none: each says why below.".to_string(),
                );
            }
            None if !in_ci && !identifies => {
                plan.instead = Some(
                    "There is no package.json here, and no check in CI's files that init knows, so init wraps no checks. Put hushrun run NAME -- in front of a check's command wherever it runs, CI included, such as\n  hushrun run test -- go test ./...".to_string(),
                );
            }
            None => {}
            Some(package) => {
                for check in &package.checks {
                    if let Decision::Leave(reason) = &check.decision {
                        plan.left.push(format!("\"{}\": {reason}", check.name));
                    }
                }
                for (dir, member) in &look.members {
                    for check in &member.checks {
                        if let Decision::Leave(reason) = &check.decision {
                            plan.left
                                .push(format!("{dir}/package.json \"{}\": {reason}", check.name));
                        }
                    }
                }
                plan.fans = fan_out_lines(look);
                if !scripts_wrap && !in_ci {
                    plan.instead = Some(
                        "No script in package.json can be wrapped, so init wraps no checks. Put hushrun run -- in front of a check's command, such as\n  \"test\": \"hushrun run -- vitest run\"\nthen run hushrun init again.".into(),
                    );
                    return Ok(plan);
                }
                if scripts_wrap {
                    plan.edit_package(look, package, backend)?;
                    plan.edit_members(look)?;
                    // \[ADR 0051\] turbo.json passes hushrun's variables to the scripts turbo runs.
                    match &look.turbo {
                        turbo::Edit::None => {}
                        turbo::Edit::Text {
                            path,
                            text,
                            changes,
                        } => plan
                            .members
                            .push((path.clone(), text.clone(), changes.clone())),
                        turbo::Edit::Manual(line) => plan.manual.push(line.clone()),
                    }
                }
            }
        }
        let package_names_it = look.package.is_some() && scripts_wrap;
        if let Some(backend) = backend
            && look.backend.is_none()
            && !package_names_it
            && look.repository.is_some()
            && !look.unreadable_policy
        {
            plan.setup.backend = Some(backend.to_string());
        }
        // \[ADR 0051\] Scopes hold a workspace package's wrapped scripts as they do CI's lines.
        if in_ci || (wraps && scripts_wrap) {
            match &look.proposal.layout {
                projects::Layout::Projects(found) => plan.setup.projects.clone_from(found),
                projects::Layout::One(why) => plan.one_scope.clone_from(why),
                projects::Layout::Declared(_) => {}
            }
        }
        if in_ci {
            plan.rule_macos(look);
            if look.proposal.touches_gitlab(&look.workflows)
                && !look.root.join(GITLAB_INSTALL.0).exists()
            {
                plan.vendored = Some(GITLAB_INSTALL);
            }
            // A token to record with is for a repository that shares its results.
            if backend.is_some() && look.proposal.touches_gitlab(&look.workflows) {
                plan.manual.push(format!(
                    "{}: its jobs have no ID token to record with: create a token with hushrun token create --agent gitlab-ci --expires 365d, and set HUSHRUN_TOKEN and HUSHRUN_MACHINE_KEY_FILE as CI/CD variables",
                    policy::GITLAB_CI
                ));
            }
        }
        for (index, workflow) in look.workflows.iter().enumerate() {
            let mut jobs: Vec<&str> = workflow
                .running(&plan.wrapped)
                .map(|job| job.id.as_str())
                .collect();
            // \[ADR 0067\] An ID token is for recording at a backend: a repository that names none
            // gets its wraps, and no token.
            let tokens = backend.is_some();
            // \[ADR 0051\] A job that starts a workspace package's wrapped script needs its ID
            // token too.
            for job in &workflow.jobs {
                let starts = look.proposal.script_jobs.contains(&(index, job.id.clone()));
                if starts && !jobs.contains(&job.id.as_str()) {
                    jobs.push(&job.id);
                }
            }
            // \[ADR 0049\] A job that calls a file whose jobs need an ID token gives one too, as
            // when the called file runs a wrapped script.
            let wrapped_jobs = if wraps {
                look.proposal.token_jobs(index, &look.workflows)
            } else {
                Vec::new()
            };
            for job in &wrapped_jobs {
                if !jobs.contains(&job.as_str()) {
                    jobs.push(job);
                }
            }
            // \[ADR 0049\] A job that runs a check through hushrun, a wrapped script's or a line
            // through it already, may still need to say which variant or call it is.
            let changes = if wraps {
                look.proposal.changes_of(index, workflow)
            } else {
                workflows::Changes::default()
            };
            if !tokens {
                jobs.clear();
            }
            if jobs.is_empty() && changes.is_empty() {
                continue;
            }
            let edit = workflow.edit_with(&jobs, &changes);
            plan.manual.extend(
                edit.manual
                    .iter()
                    .map(|why| format!("{}, {why}", workflow.path)),
            );
            if let Some(text) = edit.text {
                plan.workflows
                    .push((workflow.path.clone(), text, edit.changes));
            }
        }
        // \[ADR 0047\] Read from the files as init leaves them, as the run reads them.
        if wraps {
            plan.every_time = report::every_time(look, &plan.writes(Vec::new()));
        }
        Ok(plan)
    }

    /// \[ADR 0045\] Adds `counts_from: {os: macos}` where it fits, once the scopes are known. A
    /// `hushrun.yaml` needs the repository's id, so with no id there is no file to hold it, and a
    /// file with no line for it stays as it is: either way, the plan says so and a person adds it.
    fn rule_macos(&mut self, look: &Look) {
        let existing = std::fs::read_to_string(look.root.join(POLICY_FILE)).ok();
        for level in &look.proposal.macos {
            if look.script_at(level) {
                continue;
            }
            let setup = look.repository.as_deref().and_then(|repository| {
                self.setup
                    .with_macos(level.clone(), existing.as_deref(), repository)
            });
            let at = match level {
                policy::RuleLevel::Top => String::new(),
                policy::RuleLevel::Scope(scope) => format!(" in scope {scope}"),
            };
            let why = match look.repository {
                Some(_) => format!("{POLICY_FILE} has no line for it"),
                None => format!(
                    "no origin remote names this repository, so init can't write {POLICY_FILE}"
                ),
            };
            match setup {
                Some(setup) => self.setup = setup,
                None => self.manual.push(format!(
                    "counts_from: {{os: macos}}{at} fits, as these checks run on macOS in CI, with Xcode or Swift, but {why}: add it if a pass on another system shouldn't count"
                )),
            }
        }
    }

    /// Wraps the scripts, names the backend, unless there is none (\[ADR 0067\] `--local`), and
    /// adds the npm package, as minimal edits.
    fn edit_package(
        &mut self,
        look: &Look,
        package: &Package,
        backend: Option<&str>,
    ) -> Result<(), Failure> {
        let doc = &package.doc;
        let top = doc.root();
        let mut editor = doc.editor();
        let mut expected =
            json::value(doc.text()).map_err(|e| Failure::usage(anyhow!("package.json: {e}")))?;
        let mut changes = wrap_scripts(package, &mut editor, &mut expected);
        if let Some(backend) = backend.filter(|_| look.backend.is_none()) {
            let named = match top.get("hushrun") {
                None => {
                    editor.insert(top, Place::Last, "hushrun", json!({"backend": backend}));
                    expected["hushrun"] = json!({"backend": backend});
                    true
                }
                Some(hushrun) if hushrun.members().is_some() => {
                    // An empty or unreadable backend, which names none, is replaced.
                    match hushrun.get("backend") {
                        Some(node) => editor.set_string(node, backend),
                        None => editor.insert(hushrun, Place::Last, "backend", json!(backend)),
                    }
                    expected["hushrun"]["backend"] = json!(backend);
                    true
                }
                Some(_) => {
                    self.manual.push(format!(
                        "package.json: \"hushrun\" isn't an object; make it {{\"backend\": {}}}",
                        json::quote(backend)
                    ));
                    false
                }
            };
            if named {
                changes.push(format!(
                    "\"hushrun\": {{\"backend\": {}}}, where CI, teammates and agents share results",
                    json::quote(backend)
                ));
            }
        }
        let spec = npm_package();
        let (name, range) = name_and_range(&spec);
        let declared = ["devDependencies", "dependencies"]
            .iter()
            .any(|field| top.get(field).and_then(|deps| deps.get(&name)).is_some());
        if !declared {
            let added = match top.get("devDependencies") {
                Some(deps) if deps.members().is_some() => {
                    editor.insert(deps, sorted_place(deps, &name), &name, json!(range));
                    expected["devDependencies"][&name] = json!(range);
                    true
                }
                Some(_) => {
                    self.manual.push(format!(
                        "package.json: \"devDependencies\" isn't an object; add {} to it",
                        json::quote(&name)
                    ));
                    false
                }
                None => {
                    let place = top
                        .index_of("dependencies")
                        .map_or(Place::Last, Place::After);
                    editor.insert(top, place, "devDependencies", json!({ &name: range }));
                    expected["devDependencies"] = json!({ &name: range });
                    true
                }
            };
            if added {
                changes.push(format!(
                    "devDependencies: {}: {}",
                    json::quote(&name),
                    json::quote(&range)
                ));
                self.dependency = Some(spec);
            }
        }
        let script = package.manager.after_install();
        changes.extend(add_install_script(doc, script, &mut editor, &mut expected));
        let text = editor.finish();
        // The edits must mean what they say, and nothing more.
        match json::value(&text) {
            Ok(value) if value == expected => {}
            Ok(_) => {
                return Err(Failure::new(
                    Exit::Unexpected,
                    anyhow!(
                        "editing package.json would change more than it should; please report this"
                    ),
                ));
            }
            Err(e) => {
                return Err(Failure::new(
                    Exit::Unexpected,
                    anyhow!("editing package.json would break it ({e}); please report this"),
                ));
            }
        }
        if text != doc.text() {
            self.package = Some((text, changes));
        }
        Ok(())
    }

    /// \[ADR 0051\] Wraps each workspace package's scripts in its own package.json. Where a
    /// package's scripts find only its own dependencies' programs, Yarn's from 2 on, hushrun joins
    /// its devDependencies too.
    fn edit_members(&mut self, look: &Look) -> Result<(), Failure> {
        for (dir, member) in &look.members {
            let path = format!("{dir}/package.json");
            let doc = &member.doc;
            let top = doc.root();
            let mut editor = doc.editor();
            let mut expected =
                json::value(doc.text()).map_err(|e| Failure::usage(anyhow!("{path}: {e}")))?;
            let mut changes = wrap_scripts(member, &mut editor, &mut expected);
            if changes.is_empty() {
                continue;
            }
            let (name, range) = name_and_range(&npm_package());
            let declared = ["devDependencies", "dependencies"]
                .iter()
                .any(|field| top.get(field).and_then(|deps| deps.get(&name)).is_some());
            if member.manager.own_programs() && !declared {
                match top.get("devDependencies") {
                    Some(deps) if deps.members().is_some() => {
                        editor.insert(deps, sorted_place(deps, &name), &name, json!(range));
                        expected["devDependencies"][&name] = json!(range);
                    }
                    Some(_) => {
                        self.manual.push(format!(
                            "{path}: \"devDependencies\" isn't an object; add {} to it",
                            json::quote(&name)
                        ));
                    }
                    None => {
                        let place = top
                            .index_of("dependencies")
                            .map_or(Place::Last, Place::After);
                        editor.insert(top, place, "devDependencies", json!({ &name: range }));
                        expected["devDependencies"] = json!({ &name: range });
                    }
                }
                if expected["devDependencies"].get(&name).is_some() {
                    changes.push(format!(
                        "devDependencies: {}: {}, since Yarn gives a package's scripts only its own programs",
                        json::quote(&name),
                        json::quote(&range)
                    ));
                }
            }
            let text = editor.finish();
            match json::value(&text) {
                Ok(value) if value == expected => {}
                _ => {
                    return Err(Failure::new(
                        Exit::Unexpected,
                        anyhow!(
                            "editing {path} would change more than it should; please report this"
                        ),
                    ));
                }
            }
            if text != doc.text() {
                self.members.push((path, text, changes));
            }
        }
        Ok(())
    }

    /// Whether init writes files.
    fn files(&self) -> bool {
        self.package.is_some()
            || !self.members.is_empty()
            || !self.workflows.is_empty()
            || !self.tools.files.is_empty()
            || !self.setup.is_empty()
    }

    /// Whether init changes anything: files, the org's repositories and ledgers, hushrun.yaml
    /// or the repository's rules.
    fn changes(&self) -> bool {
        self.files()
            || self.repository.is_some()
            || !self.promotion.is_empty()
            || self.tools.writes()
    }

    /// What its text says beside its lists, for the plan as JSON: also what the org gets and
    /// gives once signed in, which no edit can show, under the text's own headings.
    fn notes(&self) -> Vec<String> {
        let mut notes: Vec<String> = self.instead.iter().cloned().collect();
        if self.changes() {
            if let Some(repository) = &self.repository {
                let org = self.org.as_deref().unwrap_or("your org");
                if repository.starts_with("github.com/") {
                    notes.push(format!(
                        "{org}: connects {repository}, so that its CI records results with its own ID token"
                    ));
                }
                notes.push(format!("{org}: {}", self.ledger.line(repository, org)));
            }
            for (title, lines) in &self.promotion {
                let kept = |line: &&String| *line == policy::KEPT && !self.setup.is_empty();
                for line in lines.iter().filter(|line| !kept(line)) {
                    notes.push(format!("{title}: {line}"));
                }
            }
        }
        notes.extend(self.every_time.iter().cloned());
        notes.extend(self.fans.iter().cloned());
        notes.extend(self.one_scope.as_ref().map(|why| one_scope_line(why)));
        notes.extend(
            self.manual
                .iter()
                .map(|manual| format!("Change by hand: {manual}")),
        );
        notes
    }

    /// The files it writes, in the order it writes them, for the plan as JSON.
    fn writes(&self, notes: Vec<String>) -> report::Writes {
        let mut files: Vec<(String, String)> = Vec::new();
        files.extend(
            self.package
                .iter()
                .map(|(text, _)| ("package.json".to_string(), text.clone())),
        );
        files.extend(
            self.members
                .iter()
                .map(|(path, text, _)| (path.clone(), text.clone())),
        );
        files.extend(
            self.workflows
                .iter()
                .map(|(path, text, _)| (path.clone(), text.clone())),
        );
        files.extend(
            self.vendored
                .iter()
                .map(|(path, text, _)| ((*path).to_string(), (*text).to_string())),
        );
        if let (false, Some(repository)) = (self.setup.is_empty(), &self.written_repository) {
            files.push((
                POLICY_FILE.to_string(),
                self.setup.new_text(&self.root, repository),
            ));
        }
        report::Writes {
            files,
            agents: self
                .tools
                .files
                .iter()
                .map(|file| (file.path.clone(), file.change.clone()))
                .collect(),
            notes,
        }
    }

    fn print(&self) {
        if let Some(instead) = &self.instead {
            println!("\n{instead}");
        }
        if self.changes() {
            println!("\nhushrun init will change:");
            if let Some((_, changes)) = &self.package {
                println!("  package.json");
                for change in changes {
                    println!("    {change}");
                }
            }
            for (path, _, changes) in self.members.iter().chain(&self.workflows) {
                println!("  {path}");
                for change in changes {
                    println!("    {change}");
                }
            }
            if let Some((path, _, change)) = &self.vendored {
                println!("  {path}");
                println!("    {change}");
            }
            self.tools.print();
            if let Some(repository) = &self.repository {
                let org = self.org.as_deref().unwrap_or("your org");
                println!("  {org}");
                if repository.starts_with("github.com/") {
                    println!(
                        "    connects {repository}, so that its CI records results with its own ID token"
                    );
                }
                println!("    {}", self.ledger.line(repository, org));
            }
            for (title, lines) in &self.promotion {
                println!("  {title}");
                // A file that gets anything isn't kept as it is, whatever it has of the URL.
                let kept = |line: &&String| *line == policy::KEPT && !self.setup.is_empty();
                for line in lines.iter().filter(|line| !kept(line)) {
                    println!("    {line}");
                }
                if title == POLICY_FILE {
                    for line in self.setup.lines() {
                        println!("    {line}");
                    }
                }
            }
            if self.promotion.is_empty() && !self.setup.is_empty() {
                println!("  {POLICY_FILE}");
                for line in self.setup.lines() {
                    println!("    {line}");
                }
            }
        }
        if !self.every_time.is_empty() {
            println!();
            for line in &self.every_time {
                println!("{line}");
            }
        }
        if let Some(why) = &self.one_scope {
            println!("\n{}", one_scope_line(why));
        }
        if !self.left.is_empty() {
            println!("\nIt leaves these scripts as they are:");
            for left in &self.left {
                println!("  {left}");
            }
            for fan in &self.fans {
                println!("  {fan}");
            }
        }
        if !self.left_in_ci.is_empty() {
            println!("\nIt leaves these commands in CI's files as they are:");
            for left in &self.left_in_ci {
                println!("  {left}");
            }
        }
        if !self.suggestions.is_empty() {
            println!("\n{COULD_ALSO_WRAP}");
            for line in &self.suggestions {
                println!("  {line}");
            }
        }
        if !self.manual.is_empty() {
            println!("\nChange by hand:");
            for manual in &self.manual {
                println!("  {manual}");
            }
        }
    }

    /// Writes the files, package.json first.
    fn write(&self) -> Result<(), Failure> {
        let mut written = Vec::new();
        let files = self
            .package
            .iter()
            .map(|(text, _)| ("package.json".to_string(), text))
            .chain(
                self.members
                    .iter()
                    .chain(&self.workflows)
                    .map(|(path, text, _)| (path.clone(), text)),
            );
        for (path, text) in files {
            write(&self.root.join(&path), text).map_err(|e| {
                let done = if written.is_empty() {
                    String::new()
                } else {
                    format!(" (wrote {} first)", written.join(", "))
                };
                Failure::new(Exit::Unexpected, e.context(format!("writing {path}{done}")))
            })?;
            written.push(path);
        }
        if let Some((path, text, _)) = &self.vendored {
            let file = self.root.join(path);
            let wrote = file
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .map_err(anyhow::Error::from)
                .and_then(|()| write(&file, text));
            wrote.map_err(|e| {
                Failure::new(Exit::Unexpected, e.context(format!("writing {path}")))
            })?;
            written.push((*path).to_string());
        }
        if let (false, Some(repository)) = (self.setup.is_empty(), &self.written_repository) {
            self.setup.write(&self.root, repository)?;
            written.push(POLICY_FILE.to_string());
        }
        if !written.is_empty() {
            println!("Wrote {}.", written.join(" and "));
        }
        write_agent_files(&self.root, &self.tools.files)
    }

    /// The wrapped scripts that can show a skip: lint-like ones, then typecheck-like, then
    /// test-like.
    fn demo_scripts(&self) -> Vec<&str> {
        let mut found: Vec<_> = self
            .wrapped
            .iter()
            .filter_map(|name| scripts::kind(name).map(|kind| (kind, name.as_str())))
            .collect();
        found.sort_by_key(|(kind, _)| *kind);
        found.into_iter().map(|(_, name)| name).collect()
    }
}

/// \[ADR 0067\] Where results go: `--local` says this machine; a repository whose files name a
/// backend shares already, and `--local` there is refused; at a terminal, with no login for
/// `origin`, nothing that names an org and no agent set, the person says; otherwise, as ever,
/// they are shared.
fn sharing(look: &Look, origin: &str, args: &InitArgs) -> Result<Sharing, Failure> {
    if args.local {
        if let Some(backend) = &look.backend {
            return Err(Failure::usage(anyhow!(
                "this repository shares its results at {backend} already, so --local has nothing to set up: hushrun login signs in to it, and until then results stay on this machine"
            )));
        }
        return Ok(Sharing::Local);
    }
    let terminal = std::io::stdin().is_terminal();
    let asks = asks_where(
        args,
        look.backend.is_some(),
        terminal,
        || Credentials::load().is_ok_and(|saved| saved.login_at(origin).is_some()),
        hushrun_ci::Env::from_process().agent_set(),
    );
    if !asks {
        return Ok(Sharing::Shared);
    }
    println!();
    print!("{WHERE_QUESTION}");
    std::io::stdout()
        .flush()
        .context("writing to the terminal")?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("reading the answer")?;
    Ok(answered(&answer))
}

/// \[ADR 0067\] The one question init asks before a sign-in no login makes for it.
const WHERE_QUESTION: &str =
    "Share results with CI and teammates (signs in), or keep them on this machine only? [S/l] ";

/// \[ADR 0067\] Whether init asks [`WHERE_QUESTION`]: only a person at a terminal who chose
/// nothing yet. `--yes`, `--dry-run` and `--org` choose sharing as before; a repository that names
/// a backend, a login and an agent set leave nothing to ask.
fn asks_where(
    args: &InitArgs,
    named: bool,
    terminal: bool,
    signed_in: impl FnOnce() -> bool,
    agent: bool,
) -> bool {
    !args.yes && !args.dry_run && args.org.is_none() && terminal && !named && !agent && !signed_in()
}

/// \[ADR 0067\] What an answer to [`WHERE_QUESTION`] chooses: `l` keeps results here; Enter, or
/// anything else, shares them, as init did before it asked.
fn answered(answer: &str) -> Sharing {
    match answer.trim().to_ascii_lowercase().starts_with('l') {
        true => Sharing::Local,
        false => Sharing::Shared,
    }
}

/// \[ADR 0067\] What the plan of `--local` says first.
const LOCAL_PLAN: &str = "Results stay on this machine (--local): init signs in to nothing, names no backend and gives CI's jobs no ID token, so CI runs every check as before.";

/// \[ADR 0067\] What a finished `--local` says to do next.
const LOCAL_NEXT: &str = "Commit these changes. From now on a check skips here when this exact code already passed here, and hushrun ready and the pre-push hook go by what passed here. To share results with CI and teammates, run hushrun init again, without --local.";

/// \[ADR 0067\] `hushrun init --local`: init's plan and edits with no org in them. Nothing signs
/// in, nothing names a backend, CI's jobs get no ID token and no machine key is made; the checks
/// are wrapped, the agents' files written, the hook and the package installed, as ever, so that a
/// plain init later only adds what sharing needs.
fn local(global: &GlobalOpts, args: &InitArgs, look: &Look, json: bool) -> CmdResult {
    let plan = Plan::new(look, Backing::ThisMachine)?;
    if json {
        let mut notes = vec![LOCAL_PLAN.to_string()];
        notes.extend(plan.notes());
        return print_plan(look, plan.writes(notes));
    }
    println!("\n{LOCAL_PLAN}");
    plan.print();
    if args.dry_run {
        println!("\nNothing changed (--dry-run).");
        return Ok(Exit::Ok);
    }
    if !args.yes && !std::io::stdin().is_terminal() {
        return Err(Failure::usage(anyhow!(
            "init can't ask without a terminal: run hushrun init --local --yes to apply"
        )));
    }
    if plan.changes() {
        if args.yes {
            println!("\nApplying (--yes).");
        } else if !ask("\nApply? [Y/n] ")? {
            println!("Nothing changed.");
            return Ok(Exit::Ok);
        }
        plan.write()?;
    }
    let installed = match &plan.dependency {
        Some(spec) => install(look, spec),
        None => true,
    };
    install_hook(&look.root);
    let demo = !args.no_run && (!args.yes || args.run);
    if demo && installed {
        show_a_skip(look, &plan);
    }
    plan.tools.install_binary(global);
    println!();
    if look.package.is_none() {
        println!("{}", hook_in_each_clone());
    }
    if plan.files() {
        println!("{LOCAL_NEXT}");
    }
    if let Some(line) = bun_test_line(look) {
        println!("{line}");
    }
    if !installed {
        let command = look.manager().map(|m| m.add(&npm_package()).join(" "));
        return Err(Failure::new(
            Exit::Unexpected,
            anyhow!(
                "hushrun isn't installed: run {} once the error above is fixed",
                command.unwrap_or_default()
            ),
        ));
    }
    Ok(Exit::Ok)
}

/// What a dry run in a repository set up already says init would do with the org, when it does
/// anything there.
fn signs_in_and(look: &Look, moves_to: Option<&str>, backend: &str) -> Option<String> {
    match (moves_to, &look.repository) {
        (Some(org), Some(repository)) => Some(format!(
            "init signs in, moves it to {org}, and adds {repository} there unless it is there already."
        )),
        (Some(org), None) => Some(format!("init signs in, and moves it to {org}.")),
        (None, Some(repository)) => {
            let org = hosted::org_of(backend).unwrap_or_else(|| "its org".into());
            Some(format!(
                "init signs in, and adds {repository} to {org} unless it is there already."
            ))
        }
        (None, None) => None,
    }
}

/// Why init leaves CI's files as they are, whatever it would wrap there: a hushrun.yaml that
/// doesn't parse is the person's to fix, and with neither an origin remote nor a package.json
/// nothing can name where results go.
fn ci_left_as_is(look: &Look) -> Option<&'static str> {
    if look.unreadable_policy {
        Some("hushrun.yaml doesn't parse, so init leaves it and CI's files as they are")
    } else if look.repository.is_none() && look.package.is_none() {
        Some(
            "no origin remote names this repository, so init can't write hushrun.yaml, and wraps no checks",
        )
    } else {
        None
    }
}

/// \[ADR 0050\] What the plan says before the test scripts a person could wrap.
const COULD_ALSO_WRAP: &str = "Could also wrap these scripts, if they only run checks: a script can deploy or write reports, so init leaves them to you.";

/// \[ADR 0050\] Each test script a person could wrap, as the text says it: where it is and the
/// line to use, then, when its job won't have hushrun, that it needs it first.
fn suggestion_lines(look: &Look) -> Vec<String> {
    let mut lines = Vec::new();
    for suggestion in &look.proposal.suggestions {
        lines.push(format!(
            "{}, job {}: {} → {}",
            look.workflows[suggestion.file].path,
            suggestion.job,
            suggestion.before,
            suggestion.after
        ));
        if suggestion_setup(look, suggestion) == "none" {
            lines.push(
                "  install hushrun in its job first, as init does in the jobs it wraps".into(),
            );
        }
    }
    lines
}

/// \[ADR 0050\] Whether the job of `suggestion` has hushrun once init is done, as a job's `setup`
/// says it: `added` when init installs it there for a check it wraps, `present` when the job
/// installs it itself, else `none`.
fn suggestion_setup(look: &Look, suggestion: &suggestions::Suggestion) -> &'static str {
    if ci_left_as_is(look).is_none() && look.proposal.installs_in(suggestion.file, &suggestion.job)
    {
        "added"
    } else if suggestion.installed {
        "present"
    } else {
        "none"
    }
}

/// Why the repository keeps one scope, as the plan says it.
fn one_scope_line(why: &str) -> String {
    format!(
        "One scope for the whole repository: {why}. Any file's change runs every check; add scopes to hushrun.yaml by hand to narrow that."
    )
}

/// What init would write in a repository set up already, for the plan as JSON: as its dry run says.
fn set_up_writes(look: &Look, mut notes: Vec<String>) -> report::Writes {
    let tools = AgentTools::of(look);
    let ci = CiEdits::of(look);
    let mut files: Vec<(String, String)> = ci
        .files
        .iter()
        .map(|(path, text, _)| (path.clone(), text.clone()))
        .collect();
    files.extend(
        tools
            .prepare
            .iter()
            .map(|(text, _)| ("package.json".to_string(), text.clone())),
    );
    notes.extend(
        ci.manual
            .iter()
            .map(|manual| format!("Change by hand: {manual}")),
    );
    report::Writes {
        files,
        agents: tools
            .files
            .iter()
            .map(|file| (file.path.clone(), file.change.clone()))
            .collect(),
        notes,
    }
}

/// Prints the plan as `hushrun.init-plan/v1`, and changes nothing.
fn print_plan(look: &Look, writes: report::Writes) -> CmdResult {
    output::print_json(&report::document(look, &writes))
        .map_err(|e| Failure::new(Exit::Unexpected, e))?;
    Ok(Exit::Ok)
}

/// Where a key goes among an object's keys: in order when they are sorted, else last.
fn sorted_place(object: &json::Node, key: &str) -> Place {
    let keys: Vec<&str> = object
        .members()
        .unwrap_or_default()
        .iter()
        .map(|m| m.key.as_str())
        .collect();
    if !keys.windows(2).all(|pair| pair[0] <= pair[1]) {
        return Place::Last;
    }
    match keys.iter().rposition(|k| *k < key) {
        Some(i) => Place::After(i),
        None if keys.is_empty() => Place::Last,
        None => Place::First,
    }
}

/// A `hushrun run` that a package.json script or a CI file starts, for `hushrun ready` to look up
/// (C-22).
pub(crate) struct WrappedRun {
    /// What the report calls it.
    pub check: String,
    /// What its key is made of, or why that can't be told from here.
    pub key: Result<RunKey, String>,
    /// How to run it here, such as `npm run test`.
    pub with: String,
    /// Why it can't run here, when it can't: a pass from elsewhere still counts.
    pub only_ci: Option<String>,
    /// The program that must be here for `with` to work.
    pub needs: Option<Tool>,
    /// \[ADR 0044\] The names of the CI jobs that start it, itself or through the scripts they start
    /// (C-10).
    pub jobs: Vec<String>,
    /// \[ADR 0044\] Why CI runs it every time, when a job with several variants starts it; \[ADR
    /// 0049\] or that CI runs each variant until it passes, when the job tells them apart.
    pub variants: Option<String>,
    /// \[ADR 0049\] What a job that starts it sets wrong to tell its runs apart, so that no pass
    /// skips it there.
    pub variants_warning: Option<String>,
    /// \[ADR 0047\] The step that reads its reports in a job that starts it, when one does: CI runs
    /// it every time (C-26).
    pub reports: Option<String>,
}

/// A program a run has to find to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tool {
    pub program: String,
    /// A package.json script starts it, and a package manager adds `node_modules/.bin` to the
    /// PATH of what it runs. A CI file's command gets no such help.
    in_a_script: bool,
}

impl Tool {
    fn of_a_script(program: &str) -> Tool {
        Tool {
            program: program.to_string(),
            in_a_script: true,
        }
    }

    /// Whether a run in `dir` would find it: a path from there, or a name on `PATH` or, for a
    /// script, among what was installed. With no `node_modules/.bin`, nothing says what was: the
    /// dependencies may be waiting for an install, or be in Yarn's Plug'n'Play. So it counts as
    /// found, as on Windows, where a name may have any of several extensions: a run that can't
    /// start says so.
    pub fn found(&self, dir: &Path) -> bool {
        if cfg!(windows) {
            return true;
        }
        if self.program.contains('/') {
            return dir.join(&self.program).is_file();
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        if std::env::split_paths(&path).any(|entry| entry.join(&self.program).is_file()) {
            return true;
        }
        let installed = dir.join("node_modules/.bin");
        self.in_a_script && (!installed.is_dir() || installed.join(&self.program).is_file())
    }
}

/// What a run's key is made of, but the code (C-9).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RunKey {
    pub check: CheckName,
    pub command: Vec<String>,
    pub cwd: String,
    /// `--scope`'s.
    pub scope: Option<String>,
}

/// The `hushrun run`s CI would start for a push to `branch`, as `pipelines` and `package`, the
/// root's package.json, say at the revision looked at. `program` is how to start hushrun here.
/// \[ADR 0047\] `read` reads the repository's files at that revision, for the actions a job uses,
/// and `reports_skip` names the checks hushrun.yaml lets CI skip whatever reads their reports.
///
/// A script counts when CI's workflows run it, itself or through the scripts they start, or, when
/// they run none, whatever it is. A script whose words aren't sure is left out, since hushrun can't tell which key it runs under. A CI
/// file's run counts as it stands, with what its job needs.
pub(crate) fn wrapped_runs(
    root: &Path,
    package: Option<&str>,
    pipelines: &[workflows::Workflow],
    branch: Option<&str>,
    program: &str,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    reports_skip: &BTreeSet<String>,
) -> Vec<WrappedRun> {
    let live = live(pipelines, branch);
    let mut wrapped = package
        .map(|text| script_runs(root, text, &live, read))
        .unwrap_or_default();
    for (file, perhaps) in &live {
        for job in &file.jobs {
            // \[ADR 0050\] The steps of the composite actions it uses run in it, under its rules.
            let in_actions = workflows::in_actions(job, pipelines);
            let steps = job.steps.iter().map(|step| (step, None)).chain(
                in_actions
                    .iter()
                    .map(|step| (step.step, step.uses.only_ci.clone())),
            );
            for (step, uses) in steps {
                let needs = perhaps
                    .clone()
                    .or_else(|| job.only_ci())
                    .or(uses)
                    .or_else(|| step.only_ci.clone());
                for found in scripts::line_runs(&step.commands) {
                    let mut run = ci_run(file, job, step, found, needs.clone(), program);
                    if let Ok(key) = &run.key {
                        run.reports = reports_in(file, job, &key.command, &key.cwd, read);
                    }
                    wrapped.push(run);
                }
            }
        }
    }
    for run in &mut wrapped {
        if reports_skip.contains(&run.check) {
            run.reports = None;
        }
    }
    wrapped
}

/// \[ADR 0047\] The checks `policy` lets CI skip whatever reads their reports (`reports.ci: skip`).
pub(crate) fn reports_skip(policy: Option<&Policy>) -> BTreeSet<String> {
    policy
        .into_iter()
        .flat_map(|policy| &policy.reports)
        .filter(|(_, reports)| reports.ci == ReportsCi::Skip)
        .map(|(name, _)| name.to_string())
        .collect()
}

/// \[ADR 0047\] The step of `job` of `file` that reads the reports of `command`, a check run in
/// `dir`, which a skip wouldn't write, so that CI runs it every time (C-26). On GitLab, the job's
/// own variables say so.
fn reports_in(
    file: &workflows::Workflow,
    job: &workflows::Job,
    command: &[String],
    dir: &str,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Option<String> {
    let doc = variants::parse(&file.path, file.source().as_bytes())?;
    let reader = match file.host {
        workflows::Host::GitHub => {
            let words: Vec<&str> = command.iter().map(String::as_str).collect();
            let kind = checks::kind_in(&words, Some(dir), read);
            readers::readers(&doc["jobs"][job.id.as_str()], read)
                .into_iter()
                .filter(|reader| !reader.only_on_failure)
                .find(|reader| kind == checks::Kind::Test || !reader.coverage_or_tests_only())?
                .name
        }
        workflows::Host::GitLab => {
            if report::says_reports_read(&doc, &job.id) != Some(true) {
                return None;
            }
            readers::gitlab_reads(&doc[job.id.as_str()])
                .unwrap_or_else(|| "HUSHRUN_REPORTS_READ".to_string())
        }
    };
    Some(reader)
}

/// CI's files a push to `branch` starts, each with why that isn't sure.
fn live<'a>(
    pipelines: &'a [workflows::Workflow],
    branch: Option<&str>,
) -> Vec<(&'a workflows::Workflow, Option<String>)> {
    let mut live: Vec<(&workflows::Workflow, Option<String>)> = pipelines
        .iter()
        .filter_map(|file| match file.runs_for(branch) {
            workflows::Runs::Yes => Some((file, None)),
            workflows::Runs::Perhaps(why) => Some((file, Some(why))),
            workflows::Runs::No => None,
        })
        .collect();
    // \[ADR 0049\] A workflow of this repository that a running one calls with `uses: ./<file>`
    // runs too, when its caller does. \[ADR 0055\] Its checks take the condition of the job that
    // calls it, unless another job calls it without one.
    let mut changed = true;
    while changed {
        changed = false;
        for next in 0..live.len() {
            let (file, perhaps) = live[next].clone();
            for job in &file.jobs {
                let Some(path) = job.calls.as_deref() else {
                    continue;
                };
                let Some(called) = pipelines
                    .iter()
                    .find(|other| other.path == path && other.host == workflows::Host::GitHub)
                else {
                    continue;
                };
                let why = perhaps.clone().or_else(|| {
                    job.condition.then(|| {
                        format!(
                            "job {} of {}, which calls its workflow, has a condition",
                            job.id, file.path
                        )
                    })
                });
                match live.iter().position(|(seen, _)| seen.path == called.path) {
                    None => {
                        live.push((called, why));
                        changed = true;
                    }
                    Some(at) if live[at].1.is_some() && why.is_none() => {
                        live[at].1 = None;
                        changed = true;
                    }
                    Some(_) => {}
                }
            }
        }
    }
    live
}

/// \[ADR 0040\] What CI would run itself on a push to `branch` that `package`, the root's
/// package.json, runs through hushrun, a line for each.
pub(crate) fn outside_hushrun(
    package: Option<&str>,
    pipelines: &[workflows::Workflow],
    branch: Option<&str>,
    program: &str,
) -> Vec<String> {
    let Some(text) = package else {
        return Vec::new();
    };
    let files: Vec<&workflows::Workflow> = live(pipelines, branch)
        .into_iter()
        .map(|(f, _)| f)
        .collect();
    let scripts = package_runs(text);
    let scripts = script_checks(
        scripts
            .iter()
            .map(|(script, runs)| (script.as_str(), runs.clone())),
    );
    wrapping::outside_hushrun(&files, &scripts, program)
}

/// One `hushrun run` of a CI step.
fn ci_run(
    file: &workflows::Workflow,
    job: &workflows::Job,
    step: &workflows::Step,
    found: scripts::LineRun,
    only_ci: Option<String>,
    program: &str,
) -> WrappedRun {
    let cwd = match (&step.dir, &found.moved) {
        (Err(why), _) => Err(why.clone()),
        (_, None) => Err("a `cd` before it moves it where hushrun can't follow".to_string()),
        (Ok(dir), Some(moved)) => Ok([dir.as_str(), moved.as_str()]
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("/")),
    };
    let jobs = vec![job.id.clone()];
    let run = match found.run {
        Ok(run) => run,
        Err(why) => {
            return WrappedRun {
                check: format!("job {}", job.id),
                key: Err(format!("its key can't be told here: {why}")),
                with: String::new(),
                only_ci,
                needs: None,
                jobs,
                variants: None,
                variants_warning: None,
                reports: None,
            };
        }
    };
    let program_name = run.command.first().map(String::as_str).unwrap_or_default();
    let check = match run.name.as_deref() {
        Some(name) => CheckName::new(name).ok(),
        None => crate::cmd::wrap::derived_name(None, program_name),
    };
    let label = check.as_ref().map_or_else(
        || format!("job {}", job.id),
        |check| check.as_str().to_string(),
    );
    let key = match (check, cwd) {
        (None, _) => Err("it has no check name hushrun can read".to_string()),
        (_, Err(why)) => Err(why),
        // \[ADR 0050\] What CI fills into its command decides its key, which no one knows ahead.
        (Some(_), Ok(_)) if run.filled_in => Err(CI_VALUES.to_string()),
        (Some(check), Ok(cwd)) => Ok(RunKey {
            check,
            command: run.command.clone(),
            cwd,
            scope: run.scope.clone(),
        }),
    };
    let line = run.line(program);
    let with = match &key {
        Ok(key) if !key.cwd.is_empty() => format!("cd {} && {line}", key.cwd),
        _ => line,
    };
    WrappedRun {
        check: label,
        key,
        with,
        only_ci,
        needs: run.command.first().map(|program| Tool {
            program: program.clone(),
            in_a_script: false,
        }),
        jobs,
        variants: variants::runs_every_time(file, job),
        variants_warning: variants::identity_warning(file, job),
        reports: None,
    }
}

/// \[ADR 0050\] Why `ready` leaves to CI a check whose command CI fills in, such as
/// `${{ matrix.args }}`: only the run knows its key (C-22).
pub(crate) const CI_VALUES: &str = "its command takes values only CI knows";

/// The `hushrun run`s of package.json's scripts: of those CI's workflows run, when they run any;
/// else of every script.
fn script_runs(
    root: &Path,
    text: &str,
    live: &[(&workflows::Workflow, Option<String>)],
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Vec<WrappedRun> {
    let Ok(doc) = Doc::parse(text) else {
        return Vec::new();
    };
    let top = doc.root();
    let manager = manager::detect(
        root,
        top.get("packageManager").and_then(json::Node::as_str),
        top.get("workspaces").is_some(),
    );
    let found = package_runs(text);
    let names: Vec<String> = found.iter().map(|(name, _)| name.clone()).collect();
    let commands = script_commands(top);
    // \[ADR 0044\] What CI starts inside a run whose name it fills in: nothing here changes those.
    let in_named_runs: Vec<String> = live
        .iter()
        .flat_map(|(file, _)| &file.jobs)
        .flat_map(|job| &job.steps)
        .filter(|step| step.dir.as_deref() == Ok(""))
        .flat_map(|step| scripts::scripts_in_named_runs(&step.commands))
        .collect();
    let in_named_runs = scripts_started(&in_named_runs, &commands);
    // The jobs that run each script, and why each needs CI.
    let jobs_of = |script: &str| -> Vec<(&workflows::Workflow, &workflows::Job, Option<String>)> {
        live.iter()
            .flat_map(|(file, perhaps)| {
                let commands = &commands;
                file.jobs
                    .iter()
                    .filter(move |job| scripts_started(&job.scripts, commands).contains(script))
                    .map(move |job| (*file, job, perhaps.clone().or_else(|| job.only_ci())))
            })
            .collect()
    };
    // \[ADR 0051\] The workspace's packages, whose scripts CI may start from the root's or its own
    // lines, each run in its package's directory.
    let workspace = workspaces::read(root);
    let mut index = started::Scripts {
        commands: BTreeMap::from([(String::new(), commands.clone())]),
        workspace: workspace.as_ref(),
    };
    let mut members = Vec::new();
    for member in workspace.iter().flat_map(|workspace| &workspace.members) {
        if let Ok(doc) = Doc::parse(&member.text) {
            index
                .commands
                .insert(member.dir.clone(), script_commands(doc.root()));
            members.push((member.dir.clone(), package_runs(&member.text)));
        }
    }
    let member_jobs =
        |ran: &started::Ran| -> Vec<(&workflows::Workflow, &workflows::Job, Option<String>)> {
            live.iter()
                .flat_map(|(file, perhaps)| {
                    let index = &index;
                    file.jobs
                        .iter()
                        .filter(move |job| {
                            job.steps.iter().any(|step| {
                                step.dir.as_deref().is_ok_and(|dir| {
                                    index.started(&step.commands, dir).contains(ran)
                                })
                            })
                        })
                        .map(move |job| (*file, job, perhaps.clone().or_else(|| job.only_ci())))
                })
                .collect()
        };
    let in_ci = names.iter().any(|name| !jobs_of(name).is_empty())
        || members.iter().any(|(dir, runs)| {
            runs.iter()
                .any(|(script, _)| !member_jobs(&started::Ran::new(dir, script)).is_empty())
        });
    let mut wrapped = Vec::new();
    let in_packages = members.iter().flat_map(|(dir, runs)| {
        runs.iter()
            .map(move |(script, runs)| (dir.clone(), script.clone(), runs.clone()))
    });
    let at_root = found
        .iter()
        .map(|(script, runs)| (String::new(), script.clone(), runs.clone()));
    for (dir, script, runs) in at_root.chain(in_packages) {
        let jobs = match dir.as_str() {
            "" => jobs_of(&script),
            dir => member_jobs(&started::Ran::new(dir, &script)),
        };
        if jobs.is_empty() && (in_ci || (dir.is_empty() && in_named_runs.contains(&script))) {
            continue;
        }
        // A job that can run it here is enough.
        let only_ci = match jobs.iter().any(|(.., why)| why.is_none()) {
            true => None,
            false => jobs.iter().find_map(|(.., why)| why.clone()),
        };
        let named: Vec<String> = jobs.iter().map(|(_, job, _)| job.id.clone()).collect();
        let with = match dir.as_str() {
            "" => manager.run(&script).join(" "),
            dir => format!("cd {dir} && {}", manager.run(&script).join(" ")),
        };
        for run in &runs {
            let program = run.command.first().map(String::as_str).unwrap_or_default();
            if let Some(check) = script_check(&script, run) {
                let needs = Some(Tool::of_a_script(program));
                let variants = jobs
                    .iter()
                    .find_map(|(file, job, _)| variants::runs_every_time(file, job));
                let variants_warning = jobs
                    .iter()
                    .find_map(|(file, job, _)| variants::identity_warning(file, job));
                let reports = jobs
                    .iter()
                    .find_map(|(file, job, _)| reports_in(file, job, &run.command, &dir, read));
                wrapped.push(WrappedRun {
                    check: check.as_str().to_string(),
                    key: Ok(RunKey {
                        check,
                        command: run.command.clone(),
                        cwd: dir.clone(),
                        scope: run.scope.clone(),
                    }),
                    with: with.clone(),
                    only_ci: only_ci.clone(),
                    needs,
                    jobs: named.clone(),
                    variants,
                    variants_warning,
                    reports,
                });
            }
        }
    }
    wrapped
}

/// `started`, the scripts a CI job runs, and every script those start in turn, by `commands`.
fn scripts_started(started: &[String], commands: &BTreeMap<String, String>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut next = started.to_vec();
    while let Some(script) = next.pop() {
        if let (Some(command), false) = (commands.get(&script), found.contains(&script)) {
            next.extend(scripts::scripts_run(command));
        }
        found.insert(script);
    }
    found
}

/// Each script of `top`, a package.json, and its command. A key given twice counts once, as its
/// last value.
fn script_commands(top: &json::Node) -> BTreeMap<String, String> {
    let members = top.get("scripts").and_then(json::Node::members);
    members
        .into_iter()
        .flatten()
        .filter_map(|member| Some((member.key.clone(), member.value.as_str()?.to_string())))
        .collect()
}

/// The scripts of `text`, a package.json, that run something through hushrun, and those runs.
fn package_runs(text: &str) -> Vec<(String, Vec<scripts::Run>)> {
    let Ok(doc) = Doc::parse(text) else {
        return Vec::new();
    };
    let Some(members) = doc.root().get("scripts").and_then(json::Node::members) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (i, member) in members.iter().enumerate() {
        // A key given twice counts once, as its last value.
        let last = members[i + 1..].iter().all(|m| m.key != member.key);
        if let (Some(command), true) = (member.value.as_str(), last)
            && let Some(runs) = scripts::runs(command).filter(|runs| !runs.is_empty())
        {
            found.push((member.key.to_string(), runs));
        }
    }
    found
}

/// The check a run of `script` is: its own name, else the script's (C-9).
fn script_check(script: &str, run: &scripts::Run) -> Option<CheckName> {
    let program = run.command.first().map(String::as_str).unwrap_or_default();
    match run.name.as_deref() {
        Some(name) => CheckName::new(name).ok(),
        None => crate::cmd::wrap::derived_name(Some(script), program),
    }
}

/// What `scripts` run through hushrun, at the top directory of the repository's one scope.
fn script_checks<'a>(scripts: impl Iterator<Item = (&'a str, Vec<scripts::Run>)>) -> ScriptChecks {
    let mut checks = ScriptChecks::new();
    for (script, runs) in scripts {
        for run in runs.iter().filter(|run| run.scope.is_none()) {
            if let Some(check) = script_check(script, run) {
                checks
                    .entry(run.command.join(" "))
                    .or_insert_with(|| check.as_str().to_string());
            }
        }
    }
    checks
}

/// \[ADR 0040\] What a repository whose tests go through a script needs to hear: `bun test`, the
/// command its people know, never reads package.json, so it can't skip.
fn bun_test_line(look: &Look) -> Option<String> {
    let package = look.package.as_ref()?;
    let check = package.checks.iter().find(|check| {
        check.runs().iter().any(|run| {
            run.command
                .starts_with(&["bun".to_string(), "test".to_string()])
        })
    })?;
    Some(format!(
        "Run your tests with {}: bun test is Bun's own runner, which never reads package.json, so it can't skip them.",
        package.manager.run(&check.name).join(" ")
    ))
}

/// What init changes in CI's files of a repository already set up.
#[derive(Default)]
struct CiEdits {
    /// Each file's path, its new text, and what changes in it.
    files: Vec<(String, String, Vec<String>)>,
    /// What a person has to change.
    manual: Vec<String>,
}

impl CiEdits {
    fn of(look: &Look) -> CiEdits {
        let mut edits = CiEdits::default();
        if look.unreadable_policy {
            return edits;
        }
        for (index, workflow) in look.workflows.iter().enumerate() {
            let jobs = look.proposal.token_jobs(index, &look.workflows);
            let changes = look.proposal.changes_of(index, workflow);
            // \[ADR 0049\] A job a repository set up already runs a check in may still need to say
            // which variant or call it is.
            if jobs.is_empty() && changes.is_empty() {
                continue;
            }
            let jobs: Vec<&str> = jobs.iter().map(String::as_str).collect();
            let edit = workflow.edit_with(&jobs, &changes);
            edits.manual.extend(
                edit.manual
                    .iter()
                    .map(|why| format!("{}, {why}", workflow.path)),
            );
            if let Some(text) = edit.text {
                edits
                    .files
                    .push((workflow.path.clone(), text, edit.changes));
            }
        }
        edits
    }

    fn writes(&self) -> bool {
        !self.files.is_empty()
    }

    fn print(&self) {
        for (path, _, changes) in &self.files {
            println!("  {path}");
            for change in changes {
                println!("    {change}");
            }
        }
    }

    fn print_manual(&self) {
        if !self.manual.is_empty() {
            println!("\nChange by hand:");
            for manual in &self.manual {
                println!("  {manual}");
            }
        }
    }

    fn write(&self, root: &Path) -> Result<(), Failure> {
        for (path, text, _) in &self.files {
            write(&root.join(path), text).map_err(|e| {
                Failure::new(Exit::Unexpected, e.context(format!("writing {path}")))
            })?;
        }
        if self.writes() {
            let paths: Vec<&str> = self.files.iter().map(|(p, ..)| p.as_str()).collect();
            println!("Wrote {}.", paths.join(" and "));
        }
        Ok(())
    }
}

/// What installs hushrun in a GitLab job, for init to put beside the pipeline: GitLab has no action
/// to pin, so the recipes vendor it, and so does init.
const GITLAB_INSTALL: (&str, &str, &str) = (
    "ci/install-hushrun.sh",
    include_str!("../../../../../recipes/gitlab/install-hushrun.sh"),
    "installs hushrun in the pipeline's jobs",
);

/// The npm package init adds: `HUSHRUN_NPM_PACKAGE`, else `hushrun@^<this version>`.
fn npm_package() -> String {
    hushrun_ci::var("HUSHRUN_NPM_PACKAGE")
        .ok()
        .map(|spec| caret(spec.trim()))
        .filter(|spec| !spec.is_empty())
        .unwrap_or_else(|| format!("hushrun@^{}", env!("CARGO_PKG_VERSION")))
}

/// `hushrun@0.1.0` as `hushrun@^0.1.0`: the npm package names its exact version, and
/// package managers save an exact version with a caret, so the plan says what the file will hold.
fn caret(spec: &str) -> String {
    let exact = spec.rsplit_once('@').filter(|(name, version)| {
        let core = version.split_once('-').map_or(*version, |(core, _)| core);
        !name.is_empty()
            && core.split('.').count() == 3
            && core
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    });
    match exact {
        Some((name, version)) => format!("{name}@^{version}"),
        None => spec.to_string(),
    }
}

/// A package spec's name, and the range package.json keeps for it: `hushrun@^0.1.0` is
/// `hushrun` at `^0.1.0`; a tarball or a URL is hushrun's package at `file:` its path, or
/// at the URL.
fn name_and_range(spec: &str) -> (String, String) {
    let name_ok = |name: &str| {
        let bare = match name.strip_prefix('@') {
            Some(scoped) => match scoped.split_once('/') {
                Some((scope, rest)) if !scope.is_empty() => rest,
                _ => return false,
            },
            None => name,
        };
        !bare.is_empty()
            && !bare.starts_with('.')
            && bare
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._~".contains(c))
    };
    let at = match spec.strip_prefix('@') {
        Some(rest) => rest.find('@').map(|i| i + 1),
        None => spec.find('@'),
    };
    match at {
        Some(at) if name_ok(&spec[..at]) => (spec[..at].to_string(), spec[at + 1..].to_string()),
        None if name_ok(spec) => (spec.to_string(), "latest".into()),
        _ => {
            // `file:…`, `https://…` or `git+ssh://…` as they are; a path, `C:\…` included, as
            // `file:` it.
            let scheme = spec.split_once(':').is_some_and(|(scheme, _)| {
                scheme.len() > 1
                    && scheme
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
            });
            let range = if scheme {
                spec.to_string()
            } else {
                format!("file:{spec}")
            };
            ("hushrun".into(), range)
        }
    }
}

/// The repository as run results name it (C-10): the CI provider's, else hushrun.yaml's, else
/// the `origin` remote's.
pub(crate) fn repository_id(root: &Path) -> Option<String> {
    let ci = hushrun_ci::context(&hushrun_ci::Env::from_process()).repository;
    ci.or_else(|| {
        let bytes = std::fs::read(root.join(POLICY_FILE)).ok()?;
        Policy::parse(POLICY_FILE, &bytes)
            .ok()
            .map(|p| p.repository)
    })
    .or_else(|| crate::cmd::init::origin_repository(root))
}

/// `scheme://host[:port]` of a backend URL.
fn origin_of(backend: &str) -> Result<String, Failure> {
    let url = LedgerUrl::parse(backend).map_err(|e| {
        Failure::usage(anyhow!(
            "package.json's hushrun.backend, {backend}, isn't a backend's URL: {e}"
        ))
    })?;
    Ok(credentials::origin(&url))
}

/// Who init is signed in as, and where results go.
struct Session {
    backend: String,
    dashboard: Option<String>,
    /// Whether the login may add the repository to the backend's org. False when the repository
    /// names an org that the sign-in, approved for another, can't reach: init then sets up the
    /// files and says who can let it in, instead of a refusal per request.
    member: bool,
    /// For someone who isn't a member: their request to join, or what else they do.
    joining: Joining,
}

/// The org's URL when something names it: `--org`, else the repository's files.
fn named_backend(look: &Look, origin: &str, args: &InitArgs) -> Option<String> {
    args.org
        .as_deref()
        .map(|org| hosted::org_url(origin, org))
        .or_else(|| look.backend.clone())
}

/// Signs in to `origin` unless a login for it exists and the org is known, and says who to. The
/// org is `--org`'s, else the repository's; else, for someone with a login, the one of theirs they
/// choose (ADR 0041).
///
/// A fresh sign-in comes back with the org it was approved for, which the approval page lets a
/// newcomer join or create there and then, so init carries on into connecting the repository
/// without a second run. Approved for no org (a server from before that page), init can't add the
/// repository anywhere, and says where to create or join one.
fn sign_in(look: &Look, origin: &str, args: &InitArgs) -> Result<Session, Failure> {
    let login = Credentials::load()?
        .login_at(origin)
        .cloned()
        // Naming the org needs no list, but a login that stopped working still signs in again.
        .filter(|_| {
            args.org.is_none() || hosted::my_orgs(origin, None) != hosted::OrgList::SignedOut
        });
    let named = named_backend(look, origin, args);
    let known = match &named {
        Some(_) => named.clone(),
        None => orgs::choose(look.repository.as_deref(), origin, args)?,
    };
    let (email, backend, fresh) = match (login, known) {
        (Some(login), Some(backend)) => (login.email, backend, None),
        (_, known) => {
            let hint = known.as_deref().and_then(hosted::org_of);
            println!();
            let signed = hosted::sign_in(origin, hint.as_deref(), args.no_browser)?;
            let backend = named
                .clone()
                .or_else(|| signed.backend.clone())
                .or_else(|| hosted::saved_backend(origin))
                .ok_or_else(|| {
                    let place = signed.dashboard.as_deref().map_or_else(
                        || format!("in {origin}'s dashboard"),
                        |dashboard| format!("at {dashboard}/new"),
                    );
                    Failure::new(
                        Exit::Unexpected,
                        anyhow!(
                            "signed in as {}, but the sign-in wasn't approved for an organization, so init can't add this repository to one: create or join an organization {place}, then run npx hushrun init again",
                            signed.email
                        ),
                    )
                })?;
            (signed.email.clone(), backend, Some(signed))
        }
    };
    let mut access = hosted::org_info(&backend);
    // \[ADR 0061\] A saved login the backend no longer takes is no login: init signs in again,
    // once, instead of going on as a member and having every request refused.
    let stale = fresh.is_none()
        && matches!(&access, Err(err) if err.downcast_ref::<hosted::SignedOut>().is_some());
    let (email, fresh) = match stale {
        true => {
            println!("\nYour login for {origin} no longer works, so init signs in again.");
            let hint = hosted::org_of(&backend);
            let signed = hosted::sign_in(origin, hint.as_deref(), args.no_browser)?;
            access = hosted::org_info(&backend);
            (signed.email.clone(), Some(signed))
        }
        false => (email, fresh),
    };
    let repository_org = look.backend.as_deref().and_then(hosted::org_of);
    let chose_here =
        args.org.is_some() && (look.backend.is_none() || look.moves_to(args).is_some());
    match access {
        Ok(info) => {
            println!("Signed in to {} as {email}.", info.org);
            Ok(Session {
                backend,
                dashboard: info.dashboard,
                member: true,
                joining: Joining::Member,
            })
        }
        // `--org` chooses among the person's own orgs: for one that won't have them, nothing of
        // the repository changes, so a mistyped slug writes no URL into it.
        Err(err) if chose_here => {
            let org = args.org.as_deref().unwrap_or_default();
            Err(match err.downcast_ref::<hosted::NotAMember>() {
                Some(_) => orgs::not_yours(org, &email, origin),
                None => Failure::new(Exit::Ledger, err),
            })
        }
        // The repository is another org's, and the sign-in was approved for one of the person's
        // own, or for none; or a saved login that the repository's org refuses: they aren't a
        // member of the repository's org.
        Err(err)
            if repository_org.is_some()
                && (err.downcast_ref::<hosted::NotAMember>().is_some()
                    || fresh.as_ref().is_some_and(|signed| {
                        signed.org.as_deref() != repository_org.as_deref()
                    })) =>
        {
            tracing::debug!("{backend} refused the login: {err:#}");
            let org = repository_org.expect("checked above");
            let approved = fresh
                .as_ref()
                .and_then(|signed| signed.org.as_deref())
                .map_or_else(String::new, |own| format!(" to {own}"));
            eprintln!(
                "\nSigned in{approved} as {email}, but this repository records to {org}, and {email} isn't a member of {org}."
            );
            let dashboard = fresh.as_ref().and_then(|signed| signed.dashboard.clone());
            let joining = ask_to_join(look, &backend, &org, &email, dashboard.as_deref(), args)?;
            Ok(Session {
                backend,
                dashboard: None,
                member: false,
                joining,
            })
        }
        Err(err) => {
            eprintln!("warning: signed in as {email}, but {backend} refused it: {err:#}");
            Ok(Session {
                backend,
                dashboard: None,
                member: true,
                joining: Joining::Member,
            })
        }
    }
}

/// \[ADR 0041\] Registers this machine in the org init set the repository up in, which it may have
/// chosen without a sign-in to do it: a pipeline's signed events need it before any result is
/// written. It never fails init, and a login with no key, from before machines, gets one at its
/// next sign-in.
fn register_machine(backend: &str) {
    let registered = match crate::machine::load() {
        Ok(Some(_)) => crate::results::Backend::register_machine(backend),
        Ok(None) => Ok(()),
        Err(err) => Err(format!("{err:#}")),
    };
    if let Err(why) = registered {
        eprintln!("warning: {why}");
    }
}

/// Where someone outside the repository's org stands with it, once init asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Joining {
    /// A member: nothing to ask.
    Member,
    /// Their request to join waits for an admin: filed now, or before (on the approval page, or
    /// by an earlier init).
    Requested,
    /// They're invited: they accept the invitation.
    Invited,
    /// Nothing filed: the backend has no requests, refused one, or the person said no. They ask
    /// an admin for an invitation.
    Ask,
}

/// For someone outside the repository's org `org`: their request to join it, filed unless one
/// waits already (results-v0 §3, `{base}/v0/access-requests`), after asking on the terminal
/// unless `--yes`. Says where it stands, and returns it. Nothing is filed twice: the backend keeps
/// one open request per person and org, the approval page's included.
fn ask_to_join(
    look: &Look,
    backend: &str,
    org: &str,
    email: &str,
    dashboard: Option<&str>,
    args: &InitArgs,
) -> Result<Joining, Failure> {
    use hosted::Access;
    let cant = format!("init sets up the files, but can't add the repository to {org}");
    let invite = || {
        eprintln!(
            "{cant}: ask an admin of {org} to invite {email}, then run npx hushrun init again."
        );
        Joining::Ask
    };
    let waiting = |created: bool| {
        if created {
            eprintln!("Asked the admins of {org} to let {email} in.");
        } else {
            eprintln!("{email} asked to join {org} already; none of its admins has answered yet.");
        }
        match dashboard {
            Some(dashboard) => eprintln!(
                "{cant} until one of them approves, on {org}'s members page: {}/{org}/-/members",
                dashboard.trim_end_matches('/')
            ),
            None => eprintln!("{cant} until one of them approves."),
        }
        Joining::Requested
    };
    let refused = |code: &str, why: &str| match code {
        "invited" => {
            let place = dashboard.map_or_else(
                || "in the dashboard".to_string(),
                |dashboard| format!("at {}/account/invitations", dashboard.trim_end_matches('/')),
            );
            eprintln!(
                "{email} is invited to {org}: accept the invitation {place}, then run npx hushrun init again."
            );
            Joining::Invited
        }
        _ => {
            eprintln!("{backend} didn't take the request to join: {why}");
            invite()
        }
    };
    match hosted::access_state(backend) {
        Ok(Access::Known {
            request: Some(request),
            ..
        }) if request.status == "pending" => return Ok(waiting(false)),
        Ok(Access::Unsupported) => return Ok(invite()),
        Ok(_) => {}
        Err(err) => {
            tracing::debug!("{backend}: reading the request to join: {err:#}");
            return Ok(invite());
        }
    }
    let agreed = if args.yes {
        true
    } else if std::io::stdin().is_terminal() {
        ask(&format!(
            "Ask the admins of {org} to let {email} in? [Y/n] "
        ))?
    } else {
        eprintln!(
            "{cant}: run npx hushrun init --yes to ask its admins to let {email} in, or ask one of them for an invitation."
        );
        return Ok(Joining::Ask);
    };
    if !agreed {
        return Ok(invite());
    }
    Ok(
        match hosted::request_access(backend, look.repository.as_deref()) {
            Ok(Access::Requested { created }) => waiting(created),
            Ok(Access::Refused { code, why }) => refused(&code, &why),
            Ok(_) => invite(),
            Err(err) => {
                eprintln!("warning: couldn't ask {org}'s admins to let {email} in: {err:#}");
                invite()
            }
        },
    )
}

/// What someone outside the repository's org does next.
fn not_a_member_next_step(session: &Session) -> String {
    let org = hosted::org_of(&session.backend).unwrap_or_else(|| "its org".into());
    let after = match session.joining {
        Joining::Requested => format!("Once an admin of {org} approves your request"),
        Joining::Invited => format!("Once you accept the invitation to {org}"),
        Joining::Ask | Joining::Member => format!("Once an admin of {org} invites you"),
    };
    format!(
        "{after}, run npx hushrun init again: it adds the repository to {org}, and CI records there."
    )
}

/// What `--org` did to a repository whose files name another org.
enum Moved {
    /// Nothing to move.
    Stayed,
    /// Its files name the new org now, `look` shows them, and the org gave this ledger.
    Yes(hosted::Ledger),
    /// The person said no.
    Declined,
}

/// \[ADR 0041\] For `--org` in a repository that records to another org: moves it, once the person
/// agrees. The new org comes first: it connects the repository and gives its ledger before a file
/// changes, so one that won't take it leaves the repository where it was. The old org keeps what
/// it recorded.
fn move_repository(
    look: &mut Look,
    session: &Session,
    args: &InitArgs,
    github: Option<&ledger::GithubRepo>,
) -> Result<Moved, Failure> {
    let (Some(org), Some(from), Some(repository)) = (
        look.moves_to(args),
        look.backend.as_deref().and_then(hosted::org_of),
        look.repository.as_deref(),
    ) else {
        return Ok(Moved::Stayed);
    };
    let backend = &session.backend;
    let package = look.package.as_ref().and_then(|package| {
        let doc = &package.doc;
        let node = doc.root().get("hushrun")?.get("backend")?;
        node.as_str()?;
        let mut editor = doc.editor();
        editor.set_string(node, backend);
        Some(editor.finish())
    });
    let file = look.root.join(POLICY_FILE);
    let policy = match std::fs::read_to_string(&file) {
        Ok(text) => policy::moved(&text, backend, &from)?,
        Err(_) => None,
    };
    let promotion = Promotion::new(look, github.cloned());
    let wanted = hosted::LedgerAsk {
        repository,
        github_repository_id: github.map(|g| g.id),
        default_branch: promotion.default_branch(),
        dry_run: false,
    };
    // Better said now than after the person agreed: a member who isn't an admin can't add it.
    if let ledger::Preview::Refused(why) = ledger::preview(backend, wanted) {
        return Err(Failure::usage(anyhow!(
            "the repository stays in {from}, and nothing changed: {why}"
        )));
    }
    println!("\nThis moves the repository from {from} to {org}:");
    if package.is_some() {
        println!(
            "  package.json\n    \"hushrun\": {{\"backend\": {}}}",
            json::quote(backend)
        );
    }
    if policy.is_some() {
        println!(
            "  {POLICY_FILE}\n    the backend, the repository's URL on hushrun and the key that verifies hushrun's log"
        );
    }
    let connects = if repository.starts_with("github.com/") {
        "connects it, and adds it to the dashboard"
    } else {
        "adds it to the dashboard"
    };
    println!("  {org}\n    {repository}: {connects}");
    println!("{from} keeps what it recorded, and CI's results go to {org} once you push this.");
    if !args.yes && !ask("\nMove it? [Y/n] ")? {
        println!("Nothing changed.");
        return Ok(Moved::Declined);
    }
    // Each says why, and neither touched a file. The ledger goes first, as the likelier refusal,
    // and because the files can't lose the old org's URL without the new one's.
    let stays = || {
        Failure::new(
            Exit::Ledger,
            anyhow!("the repository stays in {from}, and no file changed"),
        )
    };
    let Some(given) = ledger::ensure(backend, wanted).filter(|ledger| !ledger.url.is_empty())
    else {
        return Err(stays());
    };
    if !connect(look, session) {
        return Err(stays());
    }
    let wrote = |path: &Path, text: &str| {
        write(path, text).map_err(|e| {
            Failure::new(
                Exit::Unexpected,
                e.context(format!("writing {}", path.display())),
            )
        })
    };
    // hushrun.yaml first: package.json's backend wins, so a stop between the two still reads as a
    // move to run again, and the other order would leave hushrun.yaml behind for good.
    if let Some(text) = &policy {
        wrote(&file, text)?;
    }
    if let Some(text) = &package {
        wrote(&look.root.join("package.json"), text)?;
    }
    let root = look.root.clone();
    *look = Look::at(&root)?;
    Promotion::new(look, github.cloned()).finish(look, Some(&given));
    Ok(Moved::Yes(given))
}

/// Asks on the terminal; yes unless the answer starts with n.
fn ask(question: &str) -> Result<bool, Failure> {
    print!("{question}");
    std::io::stdout()
        .flush()
        .context("writing to the terminal")?;
    let mut answer = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("reading the answer")?;
    let answer = answer.trim().to_ascii_lowercase();
    Ok(read > 0 && !answer.starts_with('n'))
}

/// Writes `text` to `path` through a temporary file beside it, keeping its permissions.
pub(crate) fn write(path: &Path, text: &str) -> anyhow::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let temp = path.with_file_name(format!(".{name}.hushrun-{}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        std::fs::write(&temp, text)?;
        if let Ok(meta) = std::fs::metadata(path) {
            std::fs::set_permissions(&temp, meta.permissions())?;
        }
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    Ok(written?)
}

/// Installs hushrun's npm package with the repository's package manager, so that the lockfile
/// and `node_modules/.bin/hushrun` match package.json.
fn install(look: &Look, spec: &str) -> bool {
    let Some(manager) = look.manager() else {
        return true;
    };
    let command = manager.add(spec);
    println!("Installing hushrun: {}", command.join(" "));
    match Command::new(program(&command[0]))
        .args(&command[1..])
        .current_dir(&look.root)
        .status()
    {
        Ok(status) if status.success() => true,
        Ok(status) => {
            eprintln!("error: {} failed ({status})", command.join(" "));
            false
        }
        Err(err) => {
            eprintln!("error: can't run {}: {err}", command[0]);
            false
        }
    }
}

/// Lets the repository's CI record results at the backend with its own ID token. False when the
/// backend refused, or didn't answer: the rest is for a person to do, and says so.
fn connect(look: &Look, session: &Session) -> bool {
    if !session.member {
        return true;
    }
    let Some(repository) = &look.repository else {
        eprintln!(
            "warning: no origin remote names this repository, so its CI isn't connected and it isn't added to the org: run hushrun init again once it has one"
        );
        return true;
    };
    if !repository.starts_with("github.com/") {
        eprintln!(
            "warning: {repository} isn't on GitHub: give its CI a token the backend accepts, as HUSHRUN_TOKEN"
        );
        return true;
    }
    match hosted::connect_repository(&session.backend, repository) {
        Ok(Connected::New | Connected::Already) => {
            println!("{repository} can record results from GitHub Actions.");
            true
        }
        Ok(Connected::Unsupported) => {
            eprintln!(
                "warning: {} can't connect repositories itself: connect {repository} in its settings",
                session.backend
            );
            true
        }
        Ok(Connected::Refused(why)) => {
            eprintln!("warning: {repository} isn't connected: {why}");
            false
        }
        Err(err) => {
            eprintln!("warning: {repository} isn't connected: {err:#}");
            false
        }
    }
}

/// Makes sure the org has the repository's ledger, with GitHub's id for the repository when
/// `gh` named it while the sign-in waited, and its default branch (ADR 0026 item 10).
fn ensure_ledger(
    look: &Look,
    session: &Session,
    github: Option<&ledger::GithubRepo>,
    default_branch: Option<&str>,
) -> Option<hosted::Ledger> {
    if !session.member {
        return None;
    }
    let repository = look.repository.as_ref()?;
    ledger::ensure(
        &session.backend,
        hosted::LedgerAsk {
            repository,
            github_repository_id: github.map(|g| g.id),
            default_branch,
            dry_run: false,
        },
    )
}

/// What a repository with no package.json needs on every clone, with no install script to do it:
/// git doesn't clone hooks, and the hook only runs where hushrun is on PATH.
fn hook_in_each_clone() -> String {
    let install = match binary::on_path() {
        true => String::new(),
        false => format!("{}\n", install_line()),
    };
    format!(
        "{install}git doesn't clone hooks, and this repository has no package.json to install one: in each clone, an agent's sandbox included, the first hushrun ready or hushrun run installs it. A clone where nobody has run hushrun has no hook."
    )
}

/// How someone gets hushrun when init didn't put it on their PATH.
fn install_line() -> String {
    format!(
        "Install hushrun where you push from, so that the hook runs:\n  {}",
        releases::installer_command()
    )
}

/// What every finished init says to do next, which only says what CI will do when something goes
/// through hushrun.
fn next_step(look: &Look) -> &'static str {
    let in_ci_files = || {
        let mut jobs = look.workflows.iter().flat_map(|file| &file.jobs);
        jobs.any(|job| {
            job.actions_run_hushrun
                || job
                    .steps
                    .iter()
                    .any(|step| !scripts::line_runs(&step.commands).is_empty())
        })
    };
    match !look.wrapped().is_empty() || !look.proposal.is_empty() || in_ci_files() {
        true => {
            "Commit and push: CI records its first results. From then on a job skips what this exact code already passed on a laptop, in an agent's sandbox or in that job; each variant of a job with several skips only on its own pass."
        }
        false => "Commit and push these changes.",
    }
}

/// What init does for promotion: hushrun.yaml naming the repository's URL on hushrun and its log
/// key, once the repository has one (C-15). \[ADR 0031\] No trust and no ruleset: what counts is
/// what members' machines and the repository's CI sign (T-15).
struct Promotion {
    repository: Option<String>,
    /// hushrun.yaml as it is, when there is one.
    existing: Option<Policy>,
    /// Whether hushrun.yaml is there but doesn't parse: init leaves it alone.
    unreadable: bool,
    /// The default branch, as `gh` or the clone names it, which the ledger's ask carries.
    default_branch: Option<String>,
}

impl Promotion {
    fn new(look: &Look, github: Option<ledger::GithubRepo>) -> Promotion {
        let existing = std::fs::read(look.root.join(POLICY_FILE))
            .ok()
            .and_then(|bytes| match Policy::parse(POLICY_FILE, &bytes) {
                Ok(policy) => Some(policy),
                Err(e) => {
                    eprintln!("warning: init leaves hushrun.yaml alone: {e}");
                    None
                }
            });
        let unreadable = look.root.join(POLICY_FILE).exists() && existing.is_none();
        let default_branch = github
            .and_then(|g| g.default_branch)
            .or_else(|| hushrun_vcs::git::remote::recorded_default_branch(&look.root, "origin"));
        Promotion {
            repository: look.repository.clone(),
            existing,
            unreadable,
            default_branch,
        }
    }

    /// The repository's default branch, when init can tell.
    fn default_branch(&self) -> Option<&str> {
        self.default_branch.as_deref()
    }

    /// The plan's blocks.
    fn blocks(&self) -> Vec<(String, Vec<String>)> {
        match &self.repository {
            Some(repository) if !self.unreadable => vec![(
                POLICY_FILE.to_string(),
                policy::plan(repository, self.existing.as_ref()),
            )],
            _ => Vec::new(),
        }
    }

    /// Whether there is anything to write.
    fn writes(&self) -> bool {
        self.repository.is_some() && !self.unreadable && policy::changes(self.existing.as_ref())
    }

    fn print(&self) {
        for (title, lines) in self.blocks() {
            println!("  {title}");
            for line in lines {
                println!("    {line}");
            }
        }
    }

    /// Writes hushrun.yaml with the repository's URL and log key. Whether it changed it.
    fn finish(&self, look: &Look, ledger: Option<&hosted::Ledger>) -> bool {
        let Some(repository) = self.repository.as_deref().filter(|_| !self.unreadable) else {
            return false;
        };
        let Some(ledger) = ledger.filter(|l| !l.url.is_empty()) else {
            let has = look.root.join(POLICY_FILE).exists();
            println!(
                "{}, which it gets once an org admin's hushrun init adds it.",
                match has {
                    true => "hushrun.yaml doesn't name the repository's URL on hushrun yet",
                    false => "No hushrun.yaml yet: it names the repository's URL on hushrun",
                }
            );
            return false;
        };
        let log = policy::log_key(&ledger.url);
        if log.is_none() {
            eprintln!(
                "note: {} didn't tell its log key to this sign-in, so hushrun.yaml names none yet: hushrun doctor prints the `transparency_log` lines to add",
                ledger.url
            );
        }
        match policy::write_policy(&look.root, repository, &ledger.url, log.as_ref()) {
            Ok(policy::Written::New) => {
                println!("Wrote hushrun.yaml, with the repository's URL.");
                true
            }
            Ok(policy::Written::Updated) => {
                println!("Added the repository's URL and log key to hushrun.yaml.");
                true
            }
            Ok(policy::Written::Kept) => false,
            Err(failure) => {
                eprintln!("warning: hushrun.yaml wasn't written: {:#}", failure.error);
                false
            }
        }
    }
}

/// Runs a wrapped check twice through the package manager: it runs, then skips. Many repositories
/// keep a script for a tool they never installed, which is no failure of theirs: the next script
/// shows the skip.
fn show_a_skip(look: &Look, plan: &Plan) {
    let Some(manager) = look.manager() else {
        return;
    };
    let scripts = plan.demo_scripts();
    let not_run: Vec<String> = scripts
        .iter()
        .map_while(|script| {
            let tool = look.missing_tool(script)?;
            Some(format!(
                "Not running {script}: {tool} isn't installed here."
            ))
        })
        .collect();
    if !not_run.is_empty() {
        println!("\n{}", not_run.join("\n"));
    }
    let Some(script) = scripts.get(not_run.len()) else {
        return;
    };
    let command = manager.run(script);
    println!(
        "\nRunning {script} twice, to show a skip: {}",
        command.join(" ")
    );
    for _ in 0..2 {
        let status = Command::new(program(&command[0]))
            .args(&command[1..])
            .current_dir(&look.root)
            .status();
        match status {
            Ok(status) if status.success() => {}
            Ok(_) => {
                println!("{script} failed: once it passes, it skips on the same code.");
                return;
            }
            Err(err) => {
                eprintln!("warning: can't run {}: {err}", command[0]);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_args() -> InitArgs {
        InitArgs {
            yes: false,
            no_browser: false,
            dry_run: false,
            no_run: false,
            run: false,
            org: None,
            local: false,
            pipeline: false,
            force: false,
            repository: None,
            branch: None,
            image: None,
        }
    }

    // covers: C-15
    #[test]
    fn init_asks_where_results_go_only_a_person_who_chose_nothing() {
        let asks = |args: &InitArgs, named, terminal, signed_in, agent| {
            asks_where(args, named, terminal, || signed_in, agent)
        };
        let plain = init_args();
        assert!(asks(&plain, false, true, false, false));
        // Without a terminal, signed in, in a repository that names a backend, or for an agent.
        assert!(!asks(&plain, false, false, false, false));
        assert!(!asks(&plain, false, true, true, false));
        assert!(!asks(&plain, true, true, false, false));
        assert!(!asks(&plain, false, true, false, true));
        // --yes, --dry-run and --org chose already.
        for args in [
            InitArgs {
                yes: true,
                ..init_args()
            },
            InitArgs {
                dry_run: true,
                ..init_args()
            },
            InitArgs {
                org: Some("acme".into()),
                ..init_args()
            },
        ] {
            assert!(!asks(&args, false, true, false, false));
        }
        // `l` keeps results here; Enter and anything else share them, as init did before.
        assert_eq!(answered("l\n"), Sharing::Local);
        assert_eq!(answered(" Local\n"), Sharing::Local);
        for answer in ["\n", "s\n", "share\n", "", "y\n"] {
            assert_eq!(answered(answer), Sharing::Shared, "{answer:?}");
        }
    }

    #[test]
    fn an_exact_version_gets_the_caret_package_managers_save() {
        // covers: C-15
        assert_eq!(caret("hushrun@0.1.0"), "hushrun@^0.1.0");
        assert_eq!(caret("hushrun@1.2.3-beta.1"), "hushrun@^1.2.3-beta.1");
        assert_eq!(caret("@acme/hushrun@1.2.3"), "@acme/hushrun@^1.2.3");
        for unchanged in [
            "hushrun@^0.1.0",
            "hushrun@~0.1.0",
            "hushrun@latest",
            "hushrun",
            "/tmp/hushrun-0.1.0.tgz",
            "hushrun@1.2",
        ] {
            assert_eq!(caret(unchanged), unchanged);
        }
    }

    #[test]
    fn package_specs_name_their_package_and_range() {
        for (spec, name, range) in [
            ("hushrun@^0.1.0", "hushrun", "^0.1.0"),
            ("hushrun@0.1.0", "hushrun", "0.1.0"),
            ("@acme/hushrun@1.2.3", "@acme/hushrun", "1.2.3"),
            ("hushrun", "hushrun", "latest"),
            (
                "/tmp/hushrun-0.1.0.tgz",
                "hushrun",
                "file:/tmp/hushrun-0.1.0.tgz",
            ),
            (
                "./dist/hushrun-0.1.0.tgz",
                "hushrun",
                "file:./dist/hushrun-0.1.0.tgz",
            ),
            ("file:../hushrun.tgz", "hushrun", "file:../hushrun.tgz"),
            (
                "https://example.com/hushrun-0.1.0.tgz",
                "hushrun",
                "https://example.com/hushrun-0.1.0.tgz",
            ),
            (
                "C:\\dist\\hushrun.tgz",
                "hushrun",
                "file:C:\\dist\\hushrun.tgz",
            ),
        ] {
            assert_eq!(
                name_and_range(spec),
                (name.to_string(), range.to_string()),
                "{spec}"
            );
        }
    }

    #[test]
    fn dependencies_go_in_order_when_they_are_sorted() {
        let doc = Doc::parse(r#"{"a": {"eslint": "1", "vitest": "2"}, "b": {"z": "1", "a": "2"}, "c": {}, "d": {"zod": "1"}, "e": {"a": "1"}}"#).unwrap();
        let place = |field: &str| sorted_place(doc.root().get(field).unwrap(), "hushrun");
        assert_eq!(place("a"), Place::After(0));
        assert_eq!(place("b"), Place::Last);
        assert_eq!(place("c"), Place::Last);
        assert_eq!(place("d"), Place::First);
        assert_eq!(place("e"), Place::After(0));
    }

    // covers: C-26
    #[test]
    fn a_script_other_ci_starts_through_another_is_left_when_it_writes_reports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".circleci")).unwrap();
        std::fs::write(
            dir.path().join(".circleci/config.yml"),
            "jobs:\n  test:\n    steps:\n      - run: npm run ci\n      - store_test_results:\n          path: reports\n",
        )
        .unwrap();
        let package = Package::read(
            dir.path(),
            r#"{"scripts": {"ci": "npm run lint && npm test", "lint": "eslint .", "test": "jest --ci --coverage"}}"#,
            &caches::Caches::default(),
            &|_| false,
        )
        .unwrap();
        let decision = |name: &str| {
            let check = package.checks.iter().find(|check| check.name == name);
            check.map(|check| check.decision.clone())
        };
        assert_eq!(
            decision("test"),
            Some(Decision::Leave(
                ".circleci/config.yml reads reports this script may write, and hushrun can't see a skip there".into()
            ))
        );
        // A script that writes no report is wrapped as before.
        assert!(matches!(decision("lint"), Some(Decision::Wrap { .. })));
    }
}
