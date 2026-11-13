//! Command-line definitions (§10).

use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};

use hushrun_core::digest::Digest;
use hushrun_core::names::{CheckName, ScopeName};
use hushrun_core::run::Trigger;
use hushrun_core::time::Dur;
use hushrun_vcs::git::ReaderChoice;

use crate::output::Format;

/// Build and validate each piece of content once, then promote it by digest.
#[derive(Debug, Parser)]
#[command(name = hushrun_core::PRODUCT, version, about, propagate_version = true)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,
    #[command(subcommand)]
    pub command: Command,
}

/// Options every command accepts (§10.1). Precedence: flags, then environment, then
/// `hushrun.yaml`, then defaults. hushrun reads the variables itself once the command line is
/// parsed (`crate::environment`): clap refuses a bad flag, but a variable never stops a command.
#[derive(Debug, Args)]
pub struct GlobalOpts {
    /// Run as if hushrun was started in DIR.
    #[arg(short = 'C', long = "repo", global = true, value_name = "DIR")]
    pub repo: Option<PathBuf>,

    /// Path of hushrun.yaml [default: HUSHRUN_CONFIG, else hushrun.yaml in the content being
    /// evaluated].
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Read hushrun.yaml from this git ref instead, such as a pull request's base branch
    /// (origin/main), as pull-request pipelines must.
    #[arg(long, global = true, value_name = "REF", conflicts_with = "config")]
    pub config_ref: Option<String>,

    /// The repository's URL on hushrun [default: HUSHRUN_REPOSITORY_URL, else hushrun.yaml's
    /// `repository_url`]. In CI only HUSHRUN_REPOSITORY_URL counts.
    #[arg(long, global = true, value_name = "URL")]
    pub repository_url: Option<String>,

    /// The plan this command carries out: a file, or - for stdin [default: HUSHRUN_PLAN_JSON].
    #[arg(long, global = true, value_name = "FILE|-")]
    pub plan: Option<String>,

    /// \[ADR 0062\] Gone with stages: given, the command stops and says so (until 0.10).
    #[arg(long = "stage", global = true, hide = true, value_name = "NAME")]
    pub removed_stage: Option<String>,

    /// Output format on stdout [default: HUSHRUN_FORMAT, else text].
    #[arg(long = "format", global = true, value_enum, value_name = "FORMAT")]
    pub(crate) format_flag: Option<Format>,

    /// The output format: `--format`, else `HUSHRUN_FORMAT`, else text.
    #[arg(skip)]
    pub format: Format,

    /// Also write dotenv-format outputs to FILE (GitLab).
    #[arg(long, global = true, value_name = "FILE")]
    pub dotenv: Option<PathBuf>,

    /// Write no CI outputs, summaries or annotations, as HUSHRUN_NO_CI_OUTPUT=true does.
    #[arg(long, global = true)]
    pub no_ci_output: bool,

    /// More logging on stderr (repeatable).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Less logging on stderr.
    #[arg(short, long, global = true, action = ArgAction::Count, conflicts_with = "verbose")]
    pub quiet: u8,

    /// When to color logs.
    #[arg(long, global = true, value_enum, default_value_t, value_name = "WHEN")]
    pub color: ColorWhen,

    /// Hashing threads [default: available cores].
    #[arg(short = 'j', long, global = true, value_name = "N")]
    pub jobs: Option<usize>,

    /// Use this time instead of the clock (RFC 3339). For tests.
    #[arg(long, global = true, hide = true, value_name = "RFC3339")]
    pub now: Option<String>,

    /// Git object reader: auto, gix or cli [default: HUSHRUN_GIT_READER, else auto]. For tests.
    #[arg(long = "git-reader", global = true, hide = true, value_name = "READER")]
    pub(crate) git_reader_flag: Option<ReaderChoice>,

    /// The git object reader: `--git-reader`, else `HUSHRUN_GIT_READER`, else auto.
    #[arg(skip)]
    pub git_reader: ReaderChoice,

    /// `HUSHRUN_LOG`, the log filter without `-v` or `-q`, when it is one.
    #[arg(skip)]
    pub log: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorWhen {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the source digest of each scope.
    Digest(DigestArgs),
    /// Decide what a deploy does with the digest: promote, refresh, full, current or blocked.
    Plan(PlanArgs),
    /// Refuse to deploy a revision that is no longer the head of the branch it deploys from.
    Guard(GuardArgs),
    /// Show the required checks and their evidence for the current digest.
    Status(StatusArgs),
    /// Say what CI would still run on this exact code, and the commands that run it here first.
    #[command(after_long_help = READY_HELP)]
    Ready(ReadyArgs),
    /// The pre-push hook: says what CI would run again, and refuses a coding agent's push until
    /// that is done.
    #[command(subcommand, after_long_help = HOOK_HELP)]
    Hook(HookCommand),
    /// Print what a coding agent needs to know about this hushrun, and whether hushrun and its
    /// skill are current.
    ///
    /// hushrun's skill for coding agents holds what every hushrun does the same way, and tells
    /// the agent to run this once per session for the rest: what this hushrun's lines mean, how
    /// to set it up, and what is true here, such as a newer release, or a copy of the skill that
    /// is older or newer than this hushrun's.
    Guide,
    /// hushrun's skill for coding agents: update the copies agents read.
    #[command(subcommand)]
    Skill(SkillCommand),
    /// A check's test files, with `tests: reuse` in hushrun.yaml: turn their reuse on again.
    #[command(subcommand)]
    Tests(TestsCommand),
    /// Say what a run of a check would do with one of its test files, and why.
    ///
    /// For the files on disk now: whether the test file runs or is reused, and why, such as a file
    /// it read that changed, with the files between them that hushrun saw. From what this machine
    /// knows: the check's newest run here, its results, and one lookup of the results that count.
    /// It changes nothing.
    #[command(after_long_help = WHY_HELP)]
    Why(WhyArgs),
    /// Reuse compiled Rust crates between worktrees and runs on this machine.
    ///
    /// With reuse on, `hushrun run NAME -- COMMAND` runs COMMAND with cargo's RUSTC_WRAPPER set
    /// to hushrun, which puts a crate in place instead of compiling it when the same inputs were
    /// compiled before, in any worktree. Nothing leaves the machine. It is off until you turn it
    /// on; `hushrun build` alone shows the status.
    Build(BuildArgs),
    /// Run a check's command, unless this exact code already passed it.
    ///
    /// `hushrun run [NAME] -- COMMAND` runs COMMAND unless the repository's files, as they are
    /// now, already passed it, and remembers the result. `hushrun run CHECK…`, with no command,
    /// runs hushrun.yaml's checks instead, and records their evidence with the repository.
    #[command(after_long_help = RUN_HELP)]
    Run(RunArgs),
    /// \[ADR 0062\] Gone with stages: it stops and says so (until 0.10).
    #[command(hide = true)]
    Approve(RemovedArgs),
    /// Sign in to a backend, so that `hushrun run` shares results with your org.
    ///
    /// hushrun asks the backend for a code and opens the page that approves it, where you pick
    /// the org. Once you approve it there, the token is saved, readable by you only, and the
    /// org's URL becomes the backend `hushrun run` uses.
    Login(LoginArgs),
    /// Sign out: revoke the token `hushrun login` saved, and forget it.
    Logout(LogoutArgs),
    /// Show where `hushrun run` records results, with which credential, and who the backend
    /// says you are.
    Whoami,
    /// Agent tokens: let agents in sandboxes, and CI outside GitHub, record results without a
    /// browser (an org admin's).
    ///
    /// An agent token (hra_…) records and reads results of one repository, or of the whole org,
    /// as agent:NAME, until it expires or someone revokes it. Create also shows one string that
    /// holds it, its key and its backend: put that in the sandbox's environment as HUSHRUN_AGENT.
    #[command(subcommand, after_long_help = TOKEN_HELP)]
    Token(TokenCommand),
    /// Record evidence other tools produced: check results and artifacts.
    Attest(AttestArgs),
    /// Record a lifecycle event: started, deployed, passed, failed or superseded.
    Mark(MarkArgs),
    /// Verify event envelopes and show who signed them.
    Verify(VerifyArgs),
    /// Generate signing keys and show public keys.
    #[command(subcommand)]
    Keys(KeysCommand),
    /// Validate and explain hushrun.yaml.
    #[command(subcommand)]
    Policy(PolicyCommand),
    /// Inspect and administer the repository's events on hushrun's server.
    #[command(subcommand)]
    Repository(RepositoryCommand),
    /// Refresh the Sigstore trust root that verifies keyless signatures and timestamps.
    #[command(subcommand)]
    Sigstore(SigstoreCommand),
    /// Artifacts: what to deploy, copies by digest, tag resolution.
    #[command(subcommand)]
    Artifact(ArtifactCommand),
    /// Set up `hushrun run` here: wrap the checks, sign in, and let CI record its results.
    ///
    /// init finds the checks among package.json's scripts and in CI's own files, in any
    /// language, signs in, shows each change it would make, and makes them once you agree. Then
    /// it runs a package.json check twice: the second run skips. With --pipeline, it writes a
    /// starter hushrun.yaml and keys for a deploy pipeline instead.
    #[command(after_long_help = INIT_HELP)]
    Init(InitArgs),
    /// Write a starter hushrun.yaml and workflows that call the reusable workflow, and print
    /// the next steps.
    Adopt(AdoptArgs),
    /// Check the setup: configuration, the repository's URL and signers, keys, git, CI, clock and registries.
    Doctor(DoctorArgs),
    /// Update hushrun to the newest release, where hushrun's installer installed it.
    ///
    /// update asks GitHub which release is the newest, downloads its archive for this platform,
    /// checks it against the SHA-256 published with it, and puts the new hushrun where the
    /// installer put this one. hushrun installed any other way, such as with npm, it leaves
    /// alone, and says how to update it.
    Update,
}

/// What a removed command takes: anything, to say it is gone.
#[derive(Debug, Args)]
pub struct RemovedArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    pub rest: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ReadyArgs {
    /// The branch the code goes to [default: the current branch].
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,

    #[command(flatten)]
    pub scopes: ScopeOpts,

    /// The revision [default: HEAD].
    #[arg(long, value_name = "REV")]
    pub rev: Option<String>,

    /// The worktree as `git add -A && git commit` would record it now.
    #[arg(long, conflicts_with = "rev")]
    pub worktree: bool,

    /// Exit 0 only when a check is known and every check passed: without it, a check left with CI
    /// or another machine doesn't stop `ready`.
    #[arg(long)]
    pub strict: bool,
}

/// `hushrun skill` (C-29).
#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Write this hushrun's skill where agents read it: in this repository, as init does, or
    /// with --global in your home. A copy edited by hand, or written by a newer hushrun, stays.
    Update(SkillUpdateArgs),
}

#[derive(Debug, Args)]
pub struct SkillUpdateArgs {
    /// Your own copies, ~/.claude/skills/hushrun and ~/.agents/skills/hushrun, for repositories
    /// that have none.
    #[arg(long)]
    pub global: bool,

    /// Replace a copy edited by hand, or one a newer hushrun wrote.
    #[arg(long)]
    pub force: bool,
}

/// \[ADR 0069\] `hushrun tests` (C-34).
#[derive(Debug, Subcommand)]
pub enum TestsCommand {
    /// Turn a check's reuse of its test files on again, after an audit run suspended it: a test
    /// file failed twice though it had passed on the same files it read. For people: a coding
    /// agent can't.
    Resume(TestsResumeArgs),
}

#[derive(Debug, Args)]
pub struct TestsResumeArgs {
    /// The check, as `hushrun run NAME` names it.
    pub check: CheckName,
}

/// \[ADR 0073\] `hushrun why` (C-37).
#[derive(Debug, Args)]
pub struct WhyArgs {
    /// The check, as `hushrun run NAME` names it.
    pub check: CheckName,

    /// The test file, from here or the repository's root; for go test, a package's directory or
    /// one of its test files.
    #[arg(value_name = "TEST_FILE")]
    pub unit: String,

    /// With COMMAND, the check's scope [default: the only one].
    #[command(flatten)]
    pub scopes: ScopeOpts,

    /// The check's command [default: the one its newest run here had].
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

const WHY_HELP: &str = "Examples:
  hushrun why unit tests/test_cart.py
      tests/test_cart.py runs: it read shop/pricing.py, which changed
        tests/test_cart.py imports shop/cart.py imports shop/pricing.py
  hushrun why unit tests/test_cart.py -- pytest -q
                                    For the run of this command, not the newest run here
  hushrun why unit tests/test_cart.py --format json
                                    The same as data (hushrun.why/v1)";

/// `hushrun build` (B-1, B-21).
#[derive(Debug, Args)]
pub struct BuildArgs {
    #[command(subcommand)]
    pub command: Option<BuildCommand>,
}

/// `hushrun build`'s commands (B-1, B-21).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum BuildCommand {
    /// Reuse compiled crates in commands `hushrun run` runs.
    On,
    /// Compile everything as usual, keep the crates, and say what would have been reused.
    Observe,
    /// Reuse and keep nothing. The crates already kept stay until `hushrun build clean`.
    Off,
    /// Show the mode, the settings file, where crates are kept, and how much is kept.
    Status,
    /// Delete every crate kept.
    Clean,
    /// How much this machine takes part in the org's shared store of compiled crates: on,
    /// observe or off. It can lower the org's shared mode here, never raise it.
    Share(BuildShareArgs),
    /// For a sandbox's setup, with the network and a token: fetch the crates the org's newest
    /// build of this repository used, so later runs reuse them with neither. Good for 24 hours;
    /// no token is kept.
    Fetch,
}

/// `hushrun build share on|observe|off` (B-33).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Args)]
pub struct BuildShareArgs {
    /// on: share as the org does; observe: upload and compare, never reuse a download; off:
    /// nothing goes to or comes from the shared store.
    #[arg(value_enum)]
    pub mode: ShareWord,
}

/// The share setting's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ShareWord {
    On,
    Observe,
    Off,
}

/// `hushrun hook` (C-23).
#[derive(Debug, Subcommand)]
pub enum HookCommand {
    /// Install the pre-push hook here: in .husky/ when the repository uses husky, else where git
    /// keeps hooks.
    Install(HookInstallArgs),
    /// Take the pre-push hook out, and leave the rest of the file as it was.
    Uninstall,
    /// What the hook runs: git passes the remote, and the refs on stdin.
    #[command(name = "pre-push", hide = true)]
    PrePush(PrePushArgs),
}

/// Who the hook refuses.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("who").args(["refuse", "warn"])))]
pub struct HookMode {
    /// Refuse everyone's push, not only a coding agent's.
    #[arg(long)]
    pub refuse: bool,

    /// Refuse no one's push: only say what CI would run again.
    #[arg(long)]
    pub warn: bool,
}

#[derive(Debug, Args)]
pub struct HookInstallArgs {
    #[command(flatten)]
    pub mode: HookMode,
}

#[derive(Debug, Args)]
pub struct PrePushArgs {
    #[command(flatten)]
    pub mode: HookMode,

    /// The remote's name, as git passes it.
    #[arg(value_name = "REMOTE")]
    pub remote: Option<String>,

    /// The remote's URL, as git passes it.
    #[arg(value_name = "URL")]
    pub url: Option<String>,
}

#[derive(Debug, Args)]
pub struct AdoptArgs {
    /// The repository id, such as github.com/acme/app [default: GITHUB_REPOSITORY in CI, else
    /// the origin remote].
    #[arg(long, value_name = "ID")]
    pub repository: Option<String>,

    /// The branch it deploys from [default: main].
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,

    /// An image it deploys, by repository, such as us-docker.pkg.dev/acme/prod/app; named
    /// `image` unless ARTIFACT= says otherwise (repeatable).
    #[arg(long = "image", value_name = "[ARTIFACT=]REPOSITORY")]
    pub images: Vec<String>,

    /// The organization's base URL, under which each repository's URL is
    /// `<base>/<repository id>` [default: HUSHRUN_REPOSITORY_BASE].
    #[arg(long = "repository-base", value_name = "URL")]
    pub ledger_base: Option<String>,

    /// The reusable workflow to call, pinned by commit [default: this release's].
    #[arg(long, value_name = "OWNER/REPO/PATH@REF")]
    pub workflow: Option<String>,

    /// Pin the workflows to this hushrun release, such as 0.2.0 [default: none, so that they
    /// install the newest release].
    #[arg(long, value_name = "VERSION")]
    pub hushrun_version: Option<String>,

    /// Print what would be written, and write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Replace files that differ from what adopt writes.
    #[arg(long)]
    pub force: bool,
}

/// `hushrun init --help`'s examples.
const INIT_HELP: &str = "\
Examples:
  npx hushrun init          Find the checks, sign in, show the changes, ask, apply
  hushrun init --local          The same for this machine only: no sign-in, no backend
  hushrun init --dry-run        Show what init would change, and change nothing
  hushrun init --yes            Apply without asking, as an agent or a script does

At a terminal, with no login and no --local, init first asks whether to share results with CI and
teammates, which signs in, or to keep them on this machine only. With --local, a run here skips
what passed here, hushrun ready and the hook go by it, and CI runs every check as before; plain
hushrun init later signs in and adds the backend, keeping what --local set up.

init wraps package.json's checks (test, lint, typecheck and the like) as \"hushrun run -- …\",
names the org's backend in package.json, adds hushrun to devDependencies with the repository's
package manager, and gives the GitHub Actions jobs that run those checks `id-token: write`, so
that CI records its results with no secret. It keeps the rest of each file as it was.

In any other language, it wraps the commands it knows in CI's own files, GitHub's workflows and
GitLab's pipeline, with what each job needs, and names the backend in hushrun.yaml. A repository
of several projects gets a scope each. What can't skip safely stays, and the plan says why.

For coding agents, it writes a skill that Claude Code, Codex, OpenCode and Cursor read, a block in
AGENTS.md, and a pre-push hook, with a script in package.json that puts the hook in every clone;
without package.json, the first hushrun ready or hushrun run in a clone installs it. And without
package.json, run through npx with no hushrun on PATH, init installs a release in a directory of
yours that is on PATH, where the hook finds it. Where there is none, it says how to install one.";

/// `hushrun ready --help`'s examples.
const READY_HELP: &str = "\
Examples:
  hushrun ready                     What CI would still run on HEAD, and how to run it here
  hushrun ready --worktree          The same for the files as they would be committed now
  hushrun ready --format json       The same as data: `next` holds the commands
  hushrun ready --strict            Exit 0 only when every check passed, none left with CI

hushrun ready looks at the checks hushrun.yaml requires, and at the `hushrun run`s of
package.json's scripts and CI's own files, each by its scope and directory. Where results stay on
this machine (hushrun init --local, or nobody signed in to the repository's backend), it says what
passed here, failed here or hasn't run here, and CI still runs every check. A check it can do
nothing about here, such as one in a repository that takes CI's evidence only, or whose job has
services or a secret, or that a run here showed doesn't meet hushrun.yaml's counts_from, stays with CI or
another machine and doesn't count against the code. hushrun ready starts no tool: of programs, it
starts git.

Exit codes: 0 ready, 10 not ready, 2 a usage or configuration error.";

/// `hushrun hook --help`'s examples.
const HOOK_HELP: &str = "\
Examples:
  hushrun hook install              Refuse an agent's push, and warn a person, until checks pass
  hushrun hook install --refuse     Refuse everyone's push
  hushrun hook install --warn       Refuse no one's push
  hushrun hook uninstall            Take the hook out

The hook refuses a push only when it knows a check hasn't passed on the code being pushed and the
command that passes it can run here. When it can't tell, it lets the push through: CI still runs
whatever has no pass. It runs no checks itself. `git push --no-verify` skips it.

git doesn't clone hooks, so hushrun ready and hushrun run install this one in a clone that has
none, in a repository set up for hushrun, outside CI. A pre-push hook that is there,
core.hooksPath, husky and lefthook stay as they are. After hushrun hook uninstall, nothing installs
it in that clone until hushrun hook install.";

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("pipeline_flags")
        .multiple(true)
        .args([
            "pipeline",
            "force",
            "repository",
            "branch",
            "image",
        ])
))]
pub struct InitArgs {
    /// Apply the changes without asking. Without a terminal, init needs it.
    #[arg(short = 'y', long, conflicts_with = "pipeline_flags")]
    pub yes: bool,

    /// Print the sign-in page's address without opening a browser.
    #[arg(long, conflicts_with = "pipeline_flags")]
    pub no_browser: bool,

    /// Show what init would change, and change nothing: no sign-in, no files, no install.
    #[arg(long, conflicts_with = "pipeline_flags")]
    pub dry_run: bool,

    /// Don't end by running a check twice to show the skip.
    #[arg(long, conflicts_with = "pipeline_flags")]
    pub no_run: bool,

    /// With --yes, still end by running a check twice to show the skip.
    #[arg(long, conflicts_with_all = ["no_run", "pipeline_flags"])]
    pub run: bool,

    /// The organization this repository records to: one of yours, by its slug. Without it, init
    /// asks when you belong to several. In a repository that records to another, it moves the
    /// repository here.
    #[arg(
        long,
        value_name = "SLUG",
        value_parser = crate::hosted::org_slug,
        conflicts_with = "pipeline_flags"
    )]
    pub org: Option<String>,

    /// Keep results on this machine only: wrap the checks and set agents up, with no sign-in and
    /// no backend. CI runs every check as before; plain hushrun init later shares them.
    #[arg(long, conflicts_with_all = ["org", "pipeline_flags"])]
    pub local: bool,

    /// Write a starter hushrun.yaml for a deploy pipeline instead. The flags below imply it.
    #[arg(long, help_heading = "Pipeline")]
    pub pipeline: bool,

    /// Replace an existing hushrun.yaml.
    #[arg(long, help_heading = "Pipeline")]
    pub force: bool,

    /// The repository id, such as github.com/acme/app [default: from the origin remote].
    #[arg(long, value_name = "ID", help_heading = "Pipeline")]
    pub repository: Option<String>,

    /// The branch it deploys from [default: main].
    #[arg(long, value_name = "BRANCH", help_heading = "Pipeline")]
    pub branch: Option<String>,

    /// The image repository it deploys, such as us-central1-docker.pkg.dev/project/repo/image.
    #[arg(long, value_name = "REPOSITORY", help_heading = "Pipeline")]
    pub image: Option<String>,
}

impl InitArgs {
    /// Whether this is the deploy pipeline's init: `--pipeline`, or any of its flags.
    pub fn pipeline(&self) -> bool {
        self.pipeline
            || self.force
            || self.repository.is_some()
            || self.branch.is_some()
            || self.image.is_some()
    }
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    #[command(flatten)]
    pub registry: RegistryOpts,
}

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    /// The reference to deploy for the plan's decision.
    Resolve(ResolveArgs),
    /// Copy an image or index to another repository by digest.
    Copy {
        /// The source, pinned by digest or tagged (the tag is resolved first).
        source: String,
        /// The destination repository, optionally with a tag: REPOSITORY or REPOSITORY:TAG.
        destination: String,
        #[command(flatten)]
        registry: RegistryOpts,
    },
    /// Print the digest a reference points to.
    Digest {
        reference: String,
        #[command(flatten)]
        registry: RegistryOpts,
    },
}

#[derive(Debug, Args)]
pub struct ResolveArgs {
    /// The scope [default: the policy's only scope].
    #[arg(long, value_name = "NAME")]
    pub scope: Option<ScopeName>,

    /// Without a plan, decide for the worktree as `git add -A && git commit` would record it
    /// now.
    #[arg(long)]
    pub worktree: bool,

    /// The tag written next to the digest [default: the revision].
    #[arg(long)]
    pub tag: Option<String>,

    /// Exit 7 when the plan's artifact can't be reused, instead of falling back to a build.
    #[arg(long)]
    pub no_fallback: bool,

    #[command(flatten)]
    pub registry: RegistryOpts,
}

#[derive(Debug, Default, Args)]
pub struct RegistryOpts {
    /// A registry reached over plain HTTP instead of HTTPS (tests only; repeatable).
    #[arg(long = "insecure-registry", value_name = "HOST")]
    pub insecure: Vec<String>,
}

#[derive(Debug, Args)]
pub struct PlanArgs {
    #[command(flatten)]
    pub scopes: ScopeOpts,

    /// The revision [default: HEAD].
    #[arg(long, value_name = "REV")]
    pub rev: Option<String>,

    /// Plan for the worktree as `git add -A && git commit` would record it now.
    #[arg(long, conflicts_with = "rev")]
    pub worktree: bool,

    /// What started this run [default: from the CI provider, else manual].
    #[arg(long, value_name = "push|manual|schedule")]
    pub trigger: Option<Trigger>,

    /// This run's attempt [default: from the CI provider, else one more than recorded].
    #[arg(long, value_name = "N")]
    pub attempt: Option<u32>,

    /// Build and run every check, whatever the evidence, as HUSHRUN_FORCE_FULL=true does.
    #[arg(long)]
    pub force_full: bool,

    /// Record `started` for every scope that deploys.
    #[arg(long)]
    pub start: bool,

    /// The lease `--start` records [default: hushrun.yaml's `deploy.lease`].
    #[arg(long, value_name = "DURATION")]
    pub lease: Option<Dur>,

    /// Write the plan JSON to FILE.
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,

    /// Write the planner's full input and output to FILE.
    #[arg(long, value_name = "FILE")]
    pub explain_json: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnStale {
    /// Record `superseded`, output stale=true and exit 0.
    Skip,
    /// Record `superseded` and exit 4.
    Fail,
}

#[derive(Debug, Args)]
pub struct GuardArgs {
    #[command(flatten)]
    pub content: ContentOpts,

    /// The branch whose head the revision must be [default: hushrun.yaml's `deploy.branch`,
    /// else the branch the run is on].
    #[arg(long)]
    pub branch: Option<String>,

    #[arg(long, default_value = "origin")]
    pub remote: String,

    #[arg(long, value_enum, default_value_t = OnStale::Skip)]
    pub on_stale: OnStale,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[command(flatten)]
    pub scopes: ScopeOpts,

    /// The revision [default: HEAD].
    #[arg(long, value_name = "REV")]
    pub rev: Option<String>,

    /// The worktree as `git add -A && git commit` would record it now.
    #[arg(long, conflicts_with = "rev")]
    pub worktree: bool,
}

/// The content an event is about.
#[derive(Debug, Args)]
pub struct ContentOpts {
    #[command(flatten)]
    pub scopes: ScopeOpts,

    /// The revision [default: HEAD].
    #[arg(long, value_name = "REV")]
    pub rev: Option<String>,

    /// The worktree as `git add -A && git commit` would record it now: what you are about to
    /// commit.
    #[arg(long, conflicts_with = "rev")]
    pub worktree: bool,

    /// Use this source digest instead of computing it (one scope only).
    #[arg(long, value_name = "sha256:…", conflicts_with = "worktree")]
    pub digest: Option<Digest>,
}

impl ContentOpts {
    /// The snapshot these options name: `--worktree`, else `--rev`, else `HEAD`.
    pub fn snapshot(&self) -> hushrun_vcs::Snapshot {
        snapshot(self.rev.as_deref(), self.worktree)
    }
}

/// The snapshot of `--rev` and `--worktree`.
pub fn snapshot(rev: Option<&str>, worktree: bool) -> hushrun_vcs::Snapshot {
    if worktree {
        hushrun_vcs::Snapshot::Worktree
    } else {
        hushrun_vcs::Snapshot::Rev(rev.unwrap_or("HEAD").to_string())
    }
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Your org's backend, such as `https://hushrun.example/o/acme`, or a server, such as
    /// `https://hushrun.example` [default: HUSHRUN_BACKEND, else the backend in the
    /// repository's package.json or hushrun.yaml, else the last one you signed in to, else
    /// hushrun's hosted backend, `https://api.hushrun.com`].
    #[arg(value_name = "URL")]
    pub url: Option<String>,

    /// Print the approval page's address without opening a browser.
    #[arg(long)]
    pub no_browser: bool,
}

/// `hushrun token` (C-17).
#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Create an agent token, shown once: its results record as agent:NAME.
    Create(TokenCreateArgs),
    /// List the org's agent tokens that aren't revoked, expired ones included.
    List(TokenBackendArgs),
    /// Revoke an agent token: anything that uses it is refused from then on.
    Revoke(TokenRevokeArgs),
}

/// Which org's tokens.
#[derive(Debug, Args)]
pub struct TokenBackendArgs {
    /// Your org's backend, such as `https://api.hushrun.com/o/acme` [default:
    /// HUSHRUN_BACKEND, else the backend in the repository's package.json or hushrun.yaml, else
    /// the one you signed in to].
    #[arg(long, value_name = "URL")]
    pub backend: Option<String>,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("scope").args(["repository", "org_wide"])))]
pub struct TokenCreateArgs {
    /// The agent, a name such as claude-code, fleet-a or gitlab-ci: its results record as
    /// agent:NAME.
    #[arg(long, value_name = "NAME")]
    pub agent: String,

    /// The repository whose results it records and reads, host/owner/name [default: the
    /// repository hushrun runs in].
    #[arg(long, value_name = "REPO")]
    pub repository: Option<String>,

    /// For every repository of the org, instead of one.
    #[arg(long)]
    pub org_wide: bool,

    /// How long it lasts, in whole hours: 12h, 30d, 8w; at most 365d.
    #[arg(long, value_name = "DURATION", default_value = "30d")]
    pub expires: Dur,

    /// The most results it records a minute, 1 to 10000 [default: the backend's, 300 on
    /// hushrun cloud].
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10_000))]
    pub rate: Option<u32>,

    #[command(flatten)]
    pub target: TokenBackendArgs,
}

#[derive(Debug, Args)]
pub struct TokenRevokeArgs {
    /// The token: its id, the prefix `hushrun token list` shows (hra_ and 6 characters), or
    /// its agent's name when only one token has it.
    #[arg(value_name = "TOKEN")]
    pub token: String,

    #[command(flatten)]
    pub target: TokenBackendArgs,
}

/// `hushrun token --help`'s examples.
const TOKEN_HELP: &str = "\
Examples:
  hushrun token create --agent fleet-a              For the repository hushrun runs in, 30 days
  hushrun token create --agent gitlab-ci --org-wide --expires 365d
  hushrun token list
  hushrun token revoke hra_AbC123

create makes the agent's key with the token, and shows both once. The token goes to stdout. At a
terminal, the one string a sandbox needs comes first on stderr, with the key below. Elsewhere and in
CI neither is printed, and --format json gives them. Then, in the sandbox's environment:
  HUSHRUN_AGENT=<the string>           The backend, the token and the key; it means local too
  HUSHRUN_INVOKED_BY=agent:worker-1    Optional: tells a fleet's runs apart

For CI outside GitHub, or a sandbox set up without it:
  HUSHRUN_TOKEN=hra_…                  On hushrun's host it follows the repository's backend
  HUSHRUN_MACHINE_KEY=<the key>        Signs the agent's results; every sandbox uses the same one
  HUSHRUN_PROVIDER=local               Where a sandbox sets CI=true; never in CI";

#[derive(Debug, Args)]
pub struct LogoutArgs {
    /// The backend, or its host, to sign out of [default: the one `hushrun login` signs in to].
    #[arg(value_name = "URL")]
    pub url: Option<String>,
}

/// `hushrun run --help`'s examples.
const RUN_HELP: &str = "\
Examples:
  hushrun run -- vitest run           Run `vitest run`, unless this exact code already passed it
  hushrun run lint -- npm run lint    Any command works: its exit code decides
  \"test\": \"hushrun run -- vitest run\"
                                      In package.json, so `npm test` skips the same way everywhere

Without NAME, the check is named after the package.json script that runs it (npm_lifecycle_event;
test:unit names it test-unit), else after COMMAND's program, such as vitest. The code is the
repository's files as they are now, ignored files left out, or those of the scope --scope names,
with what that scope declares outside the repository, so a run on a laptop and one in CI share
results when the files match.
Results go to HUSHRUN_BACKEND, an https:// server; else to the backend the repository's
package.json names (\"hushrun\": {\"backend\": URL}) or its hushrun.yaml (backend: URL); else to the
one `hushrun login` signed in to; else to this machine's cache directory. An empty HUSHRUN_BACKEND
turns them off.";

#[derive(Debug, Args)]
pub struct RunArgs {
    /// With a COMMAND after --, the check's name [default: the package.json script that runs
    /// it, else COMMAND's program]. Without one, hushrun.yaml's checks to run [default: the
    /// plan's checks_to_run].
    #[arg(value_name = "CHECK")]
    pub checks: Vec<CheckName>,

    #[command(flatten)]
    pub content: ContentOpts,

    /// Every check hushrun.yaml requires, skipping those with valid evidence.
    #[arg(long)]
    pub all: bool,

    /// Run checks whose evidence is valid too.
    #[arg(long)]
    pub force: bool,

    /// Run each check in its pinned image and platform with Docker or Podman, on an export of
    /// exactly the digested files, with no network unless the check declares one, and only its
    /// `env` variables.
    #[arg(long)]
    pub isolated: bool,

    /// Each check's time limit [default: the check's `timeout`, else none].
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<Dur>,

    /// The image this job runs in, pinned by digest (checks that pin `image` need it).
    #[arg(long, value_name = "REF@sha256:…")]
    pub environment_image: Option<String>,

    /// The command, after `--`: run it as the check NAME.
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,

    /// \[ADR 0047\] `HUSHRUN_REPORTS_READ`: whether this CI job says it reads reports (C-26).
    #[arg(skip)]
    pub reports_read: Option<bool>,
}

#[derive(Debug, Args)]
pub struct AttestArgs {
    #[command(flatten)]
    pub content: ContentOpts,

    /// A check result (repeatable).
    #[arg(long = "check", value_name = "NAME=pass|fail")]
    pub checks: Vec<String>,

    /// An artifact this run built, pinned by digest (repeatable).
    #[arg(long = "artifact", value_name = "NAME=REF")]
    pub artifacts: Vec<String>,

    /// A report the checks produced, recorded by digest (repeatable).
    #[arg(long = "report", value_name = "NAME=FILE")]
    pub reports: Vec<String>,

    /// How long the checks ran [default: 0s].
    #[arg(long, value_name = "DURATION")]
    pub duration: Option<Dur>,

    /// Attempts the checks took [default: 1].
    #[arg(long, value_name = "N")]
    pub attempts: Option<u32>,

    /// The image the checks ran in, pinned by digest (checks that pin `image` need it).
    #[arg(long, value_name = "REF@sha256:…")]
    pub environment_image: Option<String>,

    #[command(flatten)]
    pub registry: RegistryOpts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MarkKind {
    Started,
    Deployed,
    Passed,
    Failed,
    Superseded,
}

#[derive(Debug, Args)]
pub struct MarkArgs {
    /// The lifecycle event.
    #[arg(value_enum)]
    pub kind: MarkKind,

    #[command(flatten)]
    pub content: ContentOpts,

    /// Why (failed); stale or cancelled (superseded).
    #[arg(long)]
    pub reason: Option<String>,

    /// An artifact deployed or passed, pinned by digest (repeatable).
    #[arg(long = "artifact", value_name = "NAME=REF")]
    pub artifacts: Vec<String>,

    /// Where it was deployed (deployed).
    #[arg(long, value_name = "NAME[=DETAIL]")]
    pub target: Option<String>,

    /// The revision that took over (superseded).
    #[arg(long, value_name = "REV")]
    pub by_revision: Option<String>,

    #[command(flatten)]
    pub registry: RegistryOpts,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Envelope files to verify.
    #[arg(conflicts_with_all = ["digest", "scope"])]
    pub files: Vec<PathBuf>,

    /// Verify every event the repository holds for this digest.
    #[arg(long, value_name = "sha256:…", requires = "scope")]
    pub digest: Option<Digest>,

    /// The scope of --digest.
    #[arg(long, value_name = "NAME", requires = "digest")]
    pub scope: Option<ScopeName>,

    /// The repository's signers as `hushrun repository signers --out` saved them, to verify
    /// files offline [default: ask the repository's server].
    #[arg(long, value_name = "FILE", conflicts_with_all = ["digest", "scope"])]
    pub signers: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// Generate a signing key: PKCS#8 PEM, mode 0600.
    Generate {
        /// Write the private key here instead of stdout.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// The key's algorithm: ECDSA P-256 is what hardware keys use.
        #[arg(long, value_enum, default_value_t = KeyAlgorithm::Ed25519)]
        algorithm: KeyAlgorithm,
    },
    /// Print the public key and key id of a private key.
    Public {
        /// The private key [default: the signer HUSHRUN_SIGNING_KEY or
        /// HUSHRUN_SIGNING_KEY_FILE names].
        #[arg(long, value_name = "FILE")]
        key: Option<PathBuf>,
    },
}

/// A signing key's algorithm (E-2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum KeyAlgorithm {
    #[default]
    Ed25519,
    #[value(name = "ecdsa-p256")]
    EcdsaP256,
}

#[derive(Debug, Subcommand)]
pub enum PolicyCommand {
    /// Validate hushrun.yaml.
    Validate {
        /// Validate hushrun.yaml at this revision instead of the working tree.
        #[arg(long, value_name = "REV")]
        rev: Option<String>,
    },
    /// Show the required checks and whose events count for each, with whether the repository's
    /// server makes the repository CI-only.
    Explain {
        /// The repository's signers as `hushrun repository signers --out` saved them, to tell
        /// whether the repository is CI-only offline [default: ask the repository's server].
        #[arg(long, value_name = "FILE")]
        signers: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum RepositoryCommand {
    /// List events.
    Ls {
        #[arg(long)]
        scope: Option<ScopeName>,
        #[arg(long, value_name = "sha256:…", requires = "scope")]
        digest: Option<Digest>,
    },
    /// Show one event.
    Show {
        /// The event's key.
        key: String,
    },
    /// The signers whose events count for the repository, as its server names them, with its
    /// statement over them: save them with --out for `hushrun verify --signers`.
    Signers {
        /// Write the answer to FILE.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Delete events older than a duration, except the newest deployment of each scope and the
    /// events of its digest (administrators only).
    Gc {
        /// Delete what was written more than this long ago, such as 180d.
        #[arg(long, value_name = "DURATION")]
        older_than: Dur,
        /// List what would be deleted, and delete nothing.
        #[arg(long)]
        dry_run: bool,
        /// The key that signs each deletion on a server that keeps a log: an org admin's machine
        /// [default: the command's signer].
        #[arg(long, value_name = "FILE")]
        key: Option<PathBuf>,
    },
    /// Replay the repository's transparency log on hushrun's server, and check every checkpoint
    /// and every listing that plans recorded against it. Exits 5 when the server contradicts
    /// its own log.
    Monitor {
        /// Remember the newest checkpoint in FILE, and check the next run against it.
        #[arg(long, value_name = "FILE")]
        state: Option<PathBuf>,
    },
    /// Show the org's plan and this period's recorded checks, against what the plan includes and
    /// its limit, with this repository's share.
    ///
    /// It asks the server `hushrun run` records results to here, as `hushrun whoami` shows it,
    /// with the same credential. Results kept on this machine are metered nowhere.
    Usage,
    /// Count what hushrun recorded, to share volume without telemetry.
    ///
    /// From one listing of the repository's events on its server, and no event read: events by
    /// kind, distinct digests, and check events. With --local, this machine's `hushrun
    /// run` results instead: by verdict, by check, distinct run keys, and the time skips saved;
    /// and so without it where results stay on this machine and no repository URL is set. It
    /// sends nothing else anywhere, and prints what you may choose to paste.
    Stats {
        /// Count what was recorded in this long, such as 7d or 12w.
        #[arg(long, value_name = "DURATION", default_value = "30d")]
        since: Dur,
        /// Count this machine's `hushrun run` results, in hushrun's cache directory, instead of
        /// the repository's events: nothing is sent.
        #[arg(long)]
        local: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum SigstoreCommand {
    /// Update the trust root from its TUF repository, verified, and cache it for offline
    /// verification.
    Refresh,
}

/// Which scopes a command works on.
#[derive(Debug, Args)]
pub struct ScopeOpts {
    /// A scope to work on (repeatable).
    #[arg(long = "scope", value_name = "NAME")]
    pub scopes: Vec<ScopeName>,

    /// Work on every scope.
    #[arg(long, conflicts_with = "scopes")]
    pub all_scopes: bool,
}

#[derive(Debug, Args)]
pub struct DigestArgs {
    #[command(flatten)]
    pub scopes: ScopeOpts,

    /// The revision to digest.
    #[arg(long, value_name = "REV", conflicts_with_all = ["worktree", "dir"])]
    pub rev: Option<String>,

    /// Digest the worktree as `git add -A && git commit` would record it now.
    #[arg(long, conflicts_with = "dir")]
    pub worktree: bool,

    /// Digest a plain directory, such as a clean export.
    #[arg(long, value_name = "PATH")]
    pub dir: Option<PathBuf>,

    /// List every entry (kind, hash, path) and the patterns that decided its scopes.
    #[arg(long)]
    pub explain: bool,

    /// Write the exact manifest bytes to FILE (one scope only).
    #[arg(long, value_name = "FILE")]
    pub manifest_out: Option<PathBuf>,
}
