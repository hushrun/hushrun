//! The hushrun command-line interface.
#![forbid(unsafe_code)]

mod app;
mod ci;
mod cli;
mod cmd;
mod coding_agent;
mod context;
mod credentials;
mod environment;
mod events;
mod exit;
mod hosted;
mod isolated;
mod lifecycle;
mod machine;
mod measure;
mod notice;
mod output;
mod planfile;
mod releases;
mod results;
mod signing;
mod skill;
mod units;

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{Cli, ColorWhen, Command, GlobalOpts};
use exit::{Exit, Failure};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    // B-2: called by the wrapper's name, this binary stands in for rustc and does nothing else.
    // Cargo starts it once per compile, about a thousand times in a cold build, so this comes
    // before any setting, the command line, logging, the hook or the notice.
    if std::env::args_os()
        .next()
        .is_some_and(|name| hushrun_build::wrapper::called_as_wrapper(&name))
    {
        return hushrun_build::wrapper::main();
    }
    // \[ADR 0068\] As the target runner Cargo starts a test binary through, hushrun is no command
    // of its own: it runs the binary and passes everything through, before anything else.
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if let Some(code) = units::runner::from_args(&args) {
        return code;
    }
    hushrun_core::set_release(env!("CARGO_PKG_VERSION"));
    // \[ADR 0046\] Before clap, which quotes what it can't read, and before a run, which records
    // its command: a string on the command line goes no further than this.
    if std::env::args_os()
        .skip(1)
        .any(|arg| hushrun_core::agent::is_agent_string(&arg.to_string_lossy()))
    {
        eprintln!(
            "error: the command line holds a HUSHRUN_AGENT string, which holds a private key: set it in the environment as HUSHRUN_AGENT. Nothing ran"
        );
        return Exit::Usage.into();
    }
    let mut cli = Cli::parse();
    // A variable never stops a command: hushrun reads its own once the command line is parsed,
    // and one it can't read costs a line, never the check (§10.1).
    let mut ignored = environment::read(&mut cli, hushrun_ci::var_os);
    let env = hushrun_ci::Env::from_process();
    ignored.extend(hushrun_ci::ignored(&env).iter().map(environment::ignoring));
    ignored.extend(environment::overridden(&env));
    if cli.global.quiet == 0 {
        for line in &ignored {
            eprintln!("{line}");
        }
    }
    init_logging(&cli.global);
    if let Some(jobs) = cli.global.jobs.filter(|j| *j > 0) {
        // Hashing threads (-j); the default pool uses every core.
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build_global();
    }
    // At a terminal, at most once a day, whether a newer release is out (C-21): asked in the
    // background, said after the command.
    let notice = notice::Notice::start(&cli);
    let code = run(&cli);
    notice.finish();
    code
}

fn run(cli: &Cli) -> ExitCode {
    // \[ADR 0043\] What people and agents run in a clone puts the hook there. The hook's own
    // command isn't one of them.
    // \[ADR 0062\] What went with stages says so, and does nothing, not even install the hook.
    if let Command::Approve(_) = &cli.command {
        eprintln!(
            "error: hushrun 0.9 has no approvals: they gated a move into a stage, and stages are gone: https://hushrun.com/docs/deploy. Nothing ran"
        );
        return Exit::Usage.into();
    }
    if let Some(stage) = &cli.global.removed_stage {
        eprintln!(
            "error: hushrun 0.9 has no stages: drop `--stage {stage}`; hushrun.yaml's `deploy` holds what a stage's settings held: https://hushrun.com/docs/deploy. Nothing ran"
        );
        return Exit::Usage.into();
    }
    if matches!(cli.command, Command::Ready(_) | Command::Run(_)) {
        cmd::hook::ensure(&cli.global);
    }
    if let Command::Run(args) = &cli.command {
        // With a command after --, `run` wraps it, and exits with its code.
        if !args.command.is_empty() {
            return cmd::wrap::run(&cli.global, args);
        }
        // Without one, it runs hushrun.yaml's checks: some name them.
        let plan = cli.global.plan.is_some()
            || hushrun_ci::Env::from_process()
                .get("HUSHRUN_PLAN_JSON")
                .is_some_and(|v| !v.is_empty());
        if args.checks.is_empty() && !args.all && !plan {
            eprintln!(
                "error: name what to run: `hushrun run NAME -- COMMAND` runs COMMAND as the check NAME, and `hushrun run CHECK…` or `hushrun run --all` runs hushrun.yaml's checks"
            );
            return Exit::Usage.into();
        }
    }
    let result = match &cli.command {
        Command::Digest(args) => cmd::digest::run(&cli.global, args),
        Command::Login(args) => cmd::login::login(&cli.global, args),
        Command::Logout(args) => cmd::login::logout(&cli.global, args),
        // C-17: results' identity and agent tokens need no hushrun.yaml.
        Command::Whoami => cmd::whoami::run(&cli.global),
        Command::Token(command) => cmd::token::run(&cli.global, command),
        // C-20: updating needs no hushrun.yaml either.
        Command::Update => cmd::update::run(&cli.global),
        // C-22, C-23: both work without hushrun.yaml.
        Command::Ready(args) => cmd::ready::run(&cli.global, args),
        Command::Hook(command) => cmd::hook::run(&cli.global, command),
        // C-28, C-29: neither needs hushrun.yaml, nor a repository.
        Command::Guide => cmd::guide::run(&cli.global),
        Command::Skill(command) => cmd::skill::run(&cli.global, command),
        // C-34: a check's reuse state, where results live; no hushrun.yaml needed.
        Command::Tests(command) => cmd::tests::run(&cli.global, command),
        // C-37: what a run would do with a test file; no hushrun.yaml needed, nothing written.
        Command::Why(args) => cmd::why::run(&cli.global, args),
        // B-21: per user, no repository, no hook.
        Command::Build(args) => cmd::build::run(&cli.global, args),
        // C-15: setting up `hushrun run` needs no hushrun.yaml.
        Command::Init(args) if !args.pipeline() => cmd::setup::run(&cli.global, args),
        command => app::App::new(&cli.global).and_then(|app| match command {
            Command::Plan(args) => cmd::plan::run(&app, args),
            Command::Guard(args) => cmd::guard::run(&app, args),
            Command::Status(args) => cmd::status::run(&app, args),
            Command::Run(args) => cmd::run::run(&app, args),
            Command::Attest(args) => cmd::attest::run(&app, args),
            Command::Mark(args) => cmd::mark::run(&app, args),
            Command::Verify(args) => cmd::verify::run(&app, args),
            Command::Keys(command) => cmd::keys::run(&app, command),
            Command::Policy(command) => cmd::policy::run(&app, command),
            Command::Repository(command) => cmd::repository::run(&app, command),
            Command::Sigstore(command) => cmd::sigstore::run(&app, command),
            Command::Artifact(command) => cmd::artifact::run(&app, command),
            Command::Init(args) => cmd::init::run(&app, args),
            Command::Adopt(args) => cmd::adopt::run(&app, args),
            Command::Doctor(args) => cmd::doctor::run(&app, args),
            Command::Digest(_)
            | Command::Login(_)
            | Command::Logout(_)
            | Command::Whoami
            | Command::Token(_)
            | Command::Update
            | Command::Ready(_)
            | Command::Hook(_)
            | Command::Guide
            | Command::Skill(_)
            | Command::Tests(_)
            | Command::Why(_)
            | Command::Build(_)
            | Command::Approve(_) => {
                unreachable!("handled above")
            }
        }),
    };
    match result {
        Ok(exit) => exit.into(),
        Err(failure) => report(failure),
    }
}

fn report(failure: Failure) -> ExitCode {
    let Failure { exit, error } = failure;
    eprintln!("error: {error:#}");
    if exit == Exit::Ok {
        return Exit::Unexpected.into();
    }
    exit.into()
}

fn init_logging(global: &GlobalOpts) {
    let level = match (global.verbose, global.quiet) {
        (0, 0) => None,
        (_, q) if q > 0 => Some("error"),
        (1, _) => Some("info"),
        (2, _) => Some("debug"),
        _ => Some("trace"),
    };
    let filter = match level.or(global.log.as_deref()) {
        Some(filter) => EnvFilter::new(filter),
        None => EnvFilter::new("warn"),
    };
    let ansi = match global.color {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => {
            std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
        }
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .with_target(false)
        .without_time()
        .try_init();
}
