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
    /// that is done. And what a hosted agent's session-start hook runs.
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
    /// Reuse compiled Rust crates and C and C++ objects between worktrees and runs on this machine.
    ///
    /// With reuse on, the builds `hushrun run` runs put in place what this machine compiled before
    /// from the same inputs, in any worktree, instead of compiling it again: Rust crates through
    /// cargo's RUSTC_WRAPPER, and the objects of a CMake build through its compiler launcher. A
    /// build outside `hushrun run` neither reads nor fills the store. On your machine it is off
    /// until you turn it on; `hushrun build` alone shows the status. Nothing leaves the machine
    /// unless your org shares compiled code through its backend and this machine takes part
    /// (`hushrun build share`); Go packages are reused only that way, since Go's own cache already
    /// serves one machine.
    Build(BuildArgs),
    /// Give each package manager the repository's baked packages: a package store beside the
    /// tool's own cache, read in place, so an install or a build downloads only what it lacks.
    ///
    /// `hushrun packages bake --out DIR -- COMMAND` runs COMMAND (the check's own build, such as
    /// `./gradlew test` or `mvn -B verify`) with Gradle's and Maven's caches at folders of its
    /// own, and keeps what they downloaded; `--harvest gradle=$HOME/.gradle` or
    /// `--harvest maven=$HOME/.m2/repository` takes a cache the job already filled. Bundler's part
    /// fetches itself with `bundle cache --no-install`, and `hushrun packages exec` copies each gem into the
    /// gem cache Bundler already uses before COMMAND starts, checked first against Gemfile.lock. A run reads
    /// the store at HUSHRUN_PACKAGES_LOWER, or /hushrun-packages; `hushrun packages exec --
    /// COMMAND` gives it to a command outside `hushrun run`. `hushrun packages bake --oci
    /// REPOSITORY` publishes the store as an image, and `hushrun packages pull` puts the newest one
    /// there in a sandbox with no Dockerfile. A store nothing signed is read only
    /// with HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1, outside CI. hushrun never sets a tool's offline
    /// switch: what the store lacks is downloaded as usual.
    #[command(verbatim_doc_comment)]
    Packages(PackagesArgs),
    /// Run a check's command, unless this exact code already passed it.
    ///
    /// `hushrun run [NAME] -- COMMAND` runs COMMAND unless the repository's files, as they are
    /// now, already passed it, and remembers the result. `hushrun run CHECK…`, with no command,
    /// runs hushrun.yaml's checks instead, and records their evidence with the repository. With
    /// build reuse on (`hushrun build`), the builds either form runs reuse the compiled code this
    /// machine kept.
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
    /// Fetch what a sandbox whose network goes after setup needs: the newest result of each
    /// check of this tree, and the compiled code of the checks named.
    ///
    /// For a setup script, which has the network and a credential that later phases may lack.
    /// What it fetches is kept for 24 hours, and later runs use it while the backend is out of
    /// reach. `hushrun build fetch` fetches only the compiled code.
    Fetch(FetchArgs),
    /// Send the results that waited on this machine while the backend was out of reach.
    ///
    /// A result `hushrun run` couldn't record waits in the outbox, in hushrun's cache directory,
    /// for 7 days. The next run and `hushrun ready` send it; so does this, at the end of an
    /// agent's session.
    Flush,
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

    /// The test file, from here, or from the repository's root when nothing is at that path from
    /// here; for go test, a package's directory or one of its test files. With `tests.unit: test`,
    /// one of its tests: `TEST_FILE::TEST`.
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
                                    The same as data (hushrun.why/v1)
  hushrun why unit tests/test_cart.py::test_total
                                    One test, with `tests.unit: test`";

/// `hushrun packages` (K-6 to K-12).
#[derive(Debug, Args)]
pub struct PackagesArgs {
    #[command(subcommand)]
    pub command: Option<PackagesCommand>,
}

/// `hushrun packages`'s commands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum PackagesCommand {
    /// The package store's state, what verifies it, and each part with its files and size.
    Status,
    /// Write the repository's package store: what COMMAND downloaded, or a filled cache.
    Bake(PackagesBakeArgs),
    /// Print `export NAME='value'` for the variables that point each tool at its part.
    Env,
    /// Run COMMAND with each tool pointed at its part, and exit with its code. Records nothing.
    Exec {
        /// The command, after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Copy the parts a tool can't read in place into its own cache, and say what was done.
    Restore {
        /// Only these parts.
        parts: Vec<String>,
    },
    /// Hash every file of the store against its row; exit 1 on the first that differs.
    Verify,
    /// Put this repository's newest package store where runs read one (HUSHRUN_PACKAGES_LOWER, or
    /// /hushrun-packages), from its registry: for a sandbox with no Dockerfile.
    ///
    /// It pulls `<registry>/<org>/<name>:hushrun-packages-<platform>-latest` (or the image
    /// HUSHRUN_PACKAGES_IMAGE names) with a pull token in HUSHRUN_PACKAGES_REGISTRY_TOKEN, else
    /// HUSHRUN_REGISTRY_TOKEN, else the backend's credential here, or nothing for a public
    /// repository; checks every file, and the signature
    /// as K-7 asks when hushrun.yaml names signers of baked stores; and only then replaces what was
    /// there. When the folder holds that store already, it downloads nothing.
    Pull(PackagesPullArgs),
}

/// `hushrun packages pull` (K-15).
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct PackagesPullArgs {
    /// The image, `<host>/<org>/<name>` [default: HUSHRUN_PACKAGES_IMAGE, else this repository's
    /// on its backend's registry].
    #[arg(long, value_name = "REPOSITORY")]
    pub image: Option<String>,
    /// The tag, by default `hushrun-packages-<platform>-latest`, or `sha256:<64 hex>` for one
    /// image.
    #[arg(long, value_name = "TAG")]
    pub tag: Option<String>,
    /// The platform, as tags name it, such as linux-x64 [default: this machine's].
    #[arg(long, value_name = "PLATFORM", conflicts_with = "tag")]
    pub platform: Option<String>,
    /// Where to put it [default: HUSHRUN_PACKAGES_LOWER, else /hushrun-packages].
    #[arg(long, value_name = "FOLDER")]
    pub to: Option<std::path::PathBuf>,
    /// Only say which image the tag names now (`<image>@<digest>`): download and change nothing.
    #[arg(long, conflicts_with_all = ["to", "force", "require_signed"])]
    pub resolve: bool,
    /// Download the store even when the folder holds it already.
    #[arg(long)]
    pub force: bool,
    /// Refuse a store no signer hushrun.yaml names signed, also one whose every part a lockfile
    /// checks and when hushrun.yaml names no signer: for a template many sandboxes start from.
    #[arg(long)]
    pub require_signed: bool,
    /// The registry is reached over plain HTTP (tests only).
    #[arg(long, hide = true)]
    pub insecure: bool,
}

/// `hushrun packages bake --out DIR` (K-6).
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct PackagesBakeArgs {
    /// The folder to write the store to, replaced in one step.
    #[arg(long, value_name = "DIR", required_unless_present_any = ["tar", "oci", "oci_dir"])]
    pub out: Option<std::path::PathBuf>,
    /// Also, or instead, write the store as one archive whose entries are under
    /// `hushrun-packages/`, so `tar -x -C /` puts it where a run reads one. The same store is the
    /// same bytes.
    #[arg(long, value_name = "FILE")]
    pub tar: Option<std::path::PathBuf>,
    /// Run no build: take only the caches --harvest names, and leave out every other part.
    #[arg(long, conflicts_with = "command")]
    pub no_build: bool,
    /// Bake even when --out already holds these parts for these build files.
    #[arg(long)]
    pub force: bool,
    /// Bake only this part (gradle, maven); repeat for more.
    #[arg(long = "only", value_name = "PART")]
    pub only: Vec<String>,
    /// Take the part from a cache the job already filled: `gradle=` a Gradle user home,
    /// `maven=` a local repository. Repeat for more.
    #[arg(long = "harvest", value_name = "PART=FOLDER")]
    pub harvest: Vec<String>,
    /// Sign the store with this key (PEM), as `hushrun build bake --signing-key` does. In a CI
    /// job with an ID token it signs keylessly with none.
    #[arg(long, value_name = "FILE")]
    pub signing_key: Option<std::path::PathBuf>,
    /// Write no signature.
    #[arg(long, conflicts_with = "signing_key")]
    pub no_sign: bool,
    /// Fail when the store is larger than this many bytes [default: 4 GiB].
    #[arg(long, value_name = "BYTES")]
    pub max_bytes: Option<u64>,
    /// Also publish the store as an image to this repository of an OCI registry, such as
    /// ghcr.io/acme/app, with the credentials docker uses: one layer, the archive --tar writes,
    /// tagged hushrun-packages-PLATFORM-IDENTITY, and hushrun-packages-PLATFORM-latest from CI on
    /// the default branch. `hushrun packages pull --image REPOSITORY` puts it in a sandbox.
    #[arg(long, value_name = "REPOSITORY", conflicts_with = "oci_dir")]
    pub oci: Option<String>,
    /// Write that image into this folder as an OCI image layout instead, no registry.
    #[arg(long, value_name = "DIR")]
    pub oci_dir: Option<std::path::PathBuf>,
    #[command(flatten)]
    pub registry: RegistryOpts,
    /// The fetch: the check's own command, run with the tools' caches at folders of the bake's.
    #[arg(last = true)]
    pub command: Vec<String>,
}

/// `hushrun build` (B-1, B-21).
#[derive(Debug, Args)]
pub struct BuildArgs {
    #[command(subcommand)]
    pub command: Option<BuildCommand>,
}

/// `hushrun build`'s commands (B-1, B-21).
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum BuildCommand {
    /// Reuse compiled code in the builds `hushrun run` runs.
    On,
    /// Compile everything as usual, keep what compiles, and say what would have been reused.
    Observe,
    /// Reuse and keep nothing. What is kept already stays until `hushrun build clean`.
    Off,
    /// Show the mode, the settings file, where compiled code is kept, and how much is kept.
    Status,
    /// Delete all the compiled code kept.
    Clean,
    /// How much this machine takes part in the org's shared store of compiled code: on,
    /// observe or off. It can lower the org's shared mode here, never raise it.
    Share(BuildShareArgs),
    /// Fetch the crates the org's newest build of each check named used (of this repository,
    /// with none named), so later runs reuse them with no network and no token.
    ///
    /// For a sandbox's setup phase, which has the network and a credential. It takes an agent
    /// token in HUSHRUN_AGENT, an agent or a personal token in HUSHRUN_TOKEN, a person's sign-in
    /// (`hushrun login`), or in GitHub Actions the job's ID token. Good for 24 hours; no token is
    /// kept.
    Fetch(BuildFetchArgs),
    /// Put this repository's newest baked store where runs read one (HUSHRUN_BUILD_LOWER, or
    /// /hushrun), from the backend's registry: for a sandbox whose image is fixed.
    ///
    /// It pulls `<registry>/<org>/<name>:hushrun-<platform>-latest` with a pull token in
    /// HUSHRUN_REGISTRY_TOKEN, the backend's credential here, or nothing for a public
    /// repository; checks the store, and its signature when hushrun.yaml names signers of baked
    /// stores; and only then replaces what was there. When the folder holds that store already, it
    /// downloads nothing.
    Pull(BuildPullArgs),
    /// Write a self-contained store of one repository's compiled crates, Go packages and C and
    /// C++ objects for this machine's platforms: sealed packs, sealed hints and baked.json,
    /// nothing else.
    ///
    /// For an image or a read-only volume a sandbox starts from: a run reads it underneath its
    /// own store when HUSHRUN_BUILD_LOWER names its folder, reusing a block once the backend it
    /// was baked from vouches for it (or, outside CI, with HUSHRUN_BUILD_LOWER_UNTRUSTED=1),
    /// until its source has a newer manifest. From this machine's store (the default) it needs no
    /// network, holds only what may leave any machine and is untrusted, for you alone; from the
    /// org's backend it needs a credential, as `fetch` does. A store missing blocks its manifests
    /// named is still written and said; it exits 11 when others start from it (an image, an
    /// archive, a disk image or --from backend --trusted).
    ///
    /// With --oci REPOSITORY it also publishes the store as a layered image, built without
    /// docker: its third-party code, its own code and what changed since are separate layers, so
    /// a change to the code uploads only the last. It is tagged hushrun-PLATFORM-MANIFEST and
    /// hushrun-PLATFORM-latest, signed keylessly in a CI job with an ID token (or with
    /// --signing-key), and a Dockerfile takes it with one line:
    /// COPY --from=REPOSITORY:hushrun-linux-x64-latest /hushrun /hushrun
    ///
    /// With --tar FILE it writes the same store as one archive, for a sandbox snapshotted while
    /// it runs (`curl -fsSL URL | tar -x -C /`, then snapshot); with --erofs FILE as a read-only
    /// disk image, for a volume mounted read-only. Both are the same bytes for the same store.
    ///
    /// A bake within the hour of the image its tag names publishes nothing unless 50 blocks
    /// changed (--force publishes). With --deps-only -- COMMAND it bakes the third-party code
    /// alone, compiled by the check's cargo command with the repository's own crates as stubs,
    /// for a new repository's first image; with --report -- COMMAND it times COMMAND cold and
    /// from the store on this machine.
    #[command(verbatim_doc_comment)]
    Bake(BuildBakeArgs),
    /// Print the newest manifest of this repository's compiled code: the identity a bake would
    /// publish now, its parts, its platform and the image tags it maps to. For a program that
    /// watches for new images.
    Manifest(BuildManifestArgs),
    /// Xcode's compilation cache for the Xcode app on this Mac. `hushrun run` serves it to
    /// xcodebuild on its own; this is for the builds the app starts.
    #[command(subcommand)]
    Xcode(BuildXcodeCommand),
}

/// `hushrun build xcode` (B-92).
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum BuildXcodeCommand {
    /// Make the Xcode app's builds use hushrun's compilation cache, the one `hushrun run` fills:
    /// a LaunchAgent serves it, and sets XCODE_XCCONFIG_FILE for the apps you open. Xcode 27.
    On,
    /// Stop it: the agent goes, and XCODE_XCCONFIG_FILE is unset when it is hushrun's.
    Off,
    /// Show whether it is on, where its files are and how much is cached.
    Status,
    /// Serve the cache until stopped: what the LaunchAgent runs.
    #[command(hide = true)]
    Serve,
}

/// `hushrun build manifest` (B-68).
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct BuildManifestArgs {
    /// Where the manifest comes from, as `bake` reads it.
    #[arg(long, value_enum, default_value_t = BakeFrom::Store)]
    pub from: BakeFrom,
    /// Name only these checks, as `bake --check` takes them; repeat for more.
    #[arg(long = "check", value_name = "NAME")]
    pub checks: Vec<String>,
    /// Print JSON (hushrun.build-manifest/v1), as --format json does.
    #[arg(long)]
    pub json: bool,
}

/// `hushrun build bake --out DIR` (B-63).
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct BuildBakeArgs {
    /// The folder to write. It must not exist, be empty, or hold a baked store, which is replaced
    /// whole. With --oci, --oci-dir, --tar or --erofs it may be left out: the store is written
    /// aside and removed.
    #[arg(long, value_name = "DIR", required_unless_present_any = ["oci", "oci_dir", "tar", "erofs"])]
    pub out: Option<std::path::PathBuf>,
    /// Where the compiled code comes from: this machine's store (what the last run of each check
    /// here compiled or reused), or the org's backend (its newest build of each check).
    #[arg(long, value_enum, default_value_t = BakeFrom::Store)]
    pub from: BakeFrom,
    /// Bake only this check, by the name `hushrun run` runs it under; repeat for more. With none,
    /// every check: from this machine's store, each check a run here left a manifest of; from a
    /// backend, each check hushrun.yaml names (else the repository's newest build).
    #[arg(long = "check", value_name = "NAME")]
    pub checks: Vec<String>,
    /// Bake only what a published image may hold: blocks from the repository's trusted scope,
    /// which only CI on the default branch writes. Refused for this machine's store.
    #[arg(long)]
    pub trusted: bool,
    /// Publish the store as an image to this repository of any OCI registry, such as
    /// ghcr.io/acme/app, with the credentials docker uses. hushrun names the tags. `hushrun`, or a
    /// repository on the registry host of the repository's backend (registry.hushrun.com for
    /// hushrun's hosted backend), publishes into that backend's registry instead, from CI on the
    /// default branch, with --from backend --trusted.
    #[arg(long, value_name = "REPOSITORY", conflicts_with = "oci_dir")]
    pub oci: Option<String>,
    /// Write the image into this folder as an OCI image layout instead, no registry.
    #[arg(long, value_name = "DIR")]
    pub oci_dir: Option<std::path::PathBuf>,
    /// Also write the store as one tar archive whose files are under hushrun/, so that `tar -x
    /// -C /` puts it at /hushrun, where a run reads it: for a sandbox snapshotted while it runs.
    /// Signed as an image is. A name ending in .zst is compressed with zstd; `-` writes the tar to
    /// stdout (and what bake says to stderr), for `| tar -x -C /`.
    #[arg(long, value_name = "FILE")]
    pub tar: Option<std::path::PathBuf>,
    /// Also write the store as a read-only EROFS disk image whose root is the store, for a volume
    /// mounted read-only: at /hushrun a run reads it as it is, elsewhere name the mount with
    /// HUSHRUN_BUILD_LOWER. Built by hushrun: no mkfs.erofs, no root.
    #[arg(long, value_name = "FILE")]
    pub erofs: Option<std::path::PathBuf>,
    /// Sign the image, archive or disk image with this private key, PKCS#8 PEM, such as `hushrun
    /// keys generate --algorithm ecdsa-p256` writes, instead of as the CI job.
    #[arg(long, value_name = "FILE", conflicts_with = "no_sign")]
    pub signing_key: Option<std::path::PathBuf>,
    /// Publish unsigned, even where hushrun could sign.
    #[arg(long)]
    pub no_sign: bool,
    /// Also move the tag of the branch checked out (in CI, the run's branch),
    /// hushrun-PLATFORM-branch-NAME-latest. The moving tag hushrun-PLATFORM-latest moves only
    /// from CI on the default branch.
    #[arg(long)]
    pub branch_tag: bool,
    /// Write the image's workspace layer again from this store, dropping blocks it keeps unused.
    #[arg(long)]
    pub compact: bool,
    /// Publish even while the image the tag names is younger than an hour and few blocks changed
    /// (hushrun.yaml's build: baked: rebake_every and rebake_blocks).
    #[arg(long)]
    pub force: bool,
    /// Compile only the third-party code, like cargo chef, and bake that: COMMAND (the check's
    /// cargo command, given after --) runs with the repository's own crates stubbed, in a store
    /// and target folder of its own, and a Go module's third-party packages are built. For a new
    /// repository's first image, before CI has built anything. Never replaces a full image.
    #[arg(long, conflicts_with_all = ["trusted"])]
    pub deps_only: bool,
    /// Measure what the store saves on this machine: COMMAND (after --) cold, in a fresh target
    /// folder with nothing to reuse, then from the store, as a sandbox started from the image
    /// runs it. With --deps-only the cold time is the dependencies' build itself.
    #[arg(long, requires = "command")]
    pub report: bool,
    /// In CI on the default branch, bake what the backend holds even when its newest build is of
    /// an older commit (a merge whose checks CI skipped), instead of first building what's missing
    /// with the checks hushrun.yaml names.
    #[arg(long)]
    pub no_build: bool,
    /// The build command --deps-only and --report run, such as `cargo test --locked`.
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
    #[command(flatten)]
    pub registry: RegistryOpts,
}

/// Where `hushrun build bake` reads blocks from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum BakeFrom {
    /// This machine's store: no network.
    Store,
    /// The org's backend, as `hushrun build fetch` reads it.
    Backend,
}

/// `hushrun fetch [CHECK]...` (C-144).
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct FetchArgs {
    /// The checks whose newest builds to fetch, by the names `hushrun run` runs them under. Every
    /// check of the tree has its newest result fetched whatever is named.
    pub checks: Vec<String>,
}

/// `hushrun build pull` (B-240).
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct BuildPullArgs {
    /// The image, `<host>/<org>/<name>` [default: this repository's on its backend's registry].
    #[arg(long, value_name = "REPOSITORY")]
    pub image: Option<String>,
    /// The tag, by default `hushrun-<platform>-latest`, or `sha256:<64 hex>` for one image.
    #[arg(long, value_name = "TAG")]
    pub tag: Option<String>,
    /// The platform, as tags name it, such as linux-x64 [default: this machine's].
    #[arg(long, value_name = "PLATFORM", conflicts_with = "tag")]
    pub platform: Option<String>,
    /// Where to put it [default: HUSHRUN_BUILD_LOWER, else /hushrun].
    #[arg(long, value_name = "FOLDER")]
    pub to: Option<std::path::PathBuf>,
    /// Only say which image the tag names now (`<image>@<digest>`): download and change nothing.
    #[arg(long, conflicts_with_all = ["to", "force", "require_signed"])]
    pub resolve: bool,
    /// Download the store even when the folder holds it already.
    #[arg(long)]
    pub force: bool,
    /// Refuse a store no signer hushrun.yaml names signed, also when it names none: for a
    /// template many sandboxes start from.
    #[arg(long)]
    pub require_signed: bool,
    /// The registry is reached over plain HTTP (tests only).
    #[arg(long, hide = true)]
    pub insecure: bool,
}

/// `hushrun build fetch [CHECK]...` (B-36).
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct BuildFetchArgs {
    /// The checks whose newest builds to fetch, by the names `hushrun run` runs them under
    /// (`hushrun run build -- cargo build` is `build`). With none, the repository's newest build.
    pub checks: Vec<String>,
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
    /// What a hosted agent's session-start hook runs: warm the sandbox, then tell the agent what
    /// this code already passed. Elsewhere it does nothing; it always exits 0.
    ///
    /// In a hosted agent's sandbox (Claude Code on the web, Cursor's and Copilot's cloud agents,
    /// Codex cloud) it pulls the baked compiled code (`hushrun build pull`), fetches the newest
    /// results and compiled code with the sandbox's credential (`hushrun fetch`), then prints a
    /// few lines on stdout for the agent: which checks already passed on this code, which are
    /// left to run here and which to CI, and whether results stay on this machine. A step the
    /// budget cuts off goes on in the background, and the steps after it are skipped.
    ///
    /// It reads the hook's JSON on stdin, when there is some, for the workspace's folder. A
    /// second start within 10 minutes on the same commit skips the steps and says the same.
    /// With `--format antigravity` the lines go out as Google Antigravity's `injectSteps` JSON,
    /// and `{}` where there is nothing to say.
    #[command(name = "session-start")]
    SessionStart(SessionStartArgs),
    /// What an agent's stop hook runs: sends the results that wait on this machine once the
    /// agent is idle, says nothing on stdout (`{}` with `--format antigravity`), and always exits
    /// 0.
    #[command(name = "stop", hide = true)]
    Stop,
}

/// `hushrun hook session-start` (C-168).
#[derive(Debug, Args)]
pub struct SessionStartArgs {
    /// The most seconds it takes, steps and context together: past it, it skips what is left
    /// and prints the context it has. Keep it under the hook's own timeout.
    #[arg(long, value_name = "SECONDS", default_value_t = crate::cmd::session_start::BUDGET_SECONDS,
          value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub budget: u64,
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
  hushrun hook session-start        In a hosted agent's session-start hook: warm it, say what passed

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

    /// Also add the bake job: after the default branch's build passes, and when run by hand, CI
    /// bakes its compiled code into an image in the repository's registry, signed, which a
    /// Dockerfile takes with one line. Needs a backend; the merge itself bakes the first image.
    #[arg(long, conflicts_with_all = ["local", "pipeline_flags"])]
    pub bake: bool,

    /// The agent clouds your team uses, comma-separated: claude-code, cursor, copilot, codex,
    /// jules, devin, devcontainer, or none. init writes their files: hooks that send what a
    /// session passed before it ends, Copilot's setup steps, a dev container's feature. Without
    /// it, init writes none of them, and at a terminal says which this repository has files for.
    #[arg(
        long,
        value_name = "LIST",
        value_delimiter = ',',
        conflicts_with = "pipeline_flags"
    )]
    pub agent_clouds: Option<Vec<String>>,

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
    /// Checks the hosts hushrun needs from here, and what an allowlist must hold, also outside
    /// a coding agent's sandbox, where doctor checks them anyway.
    #[arg(long)]
    pub hosts: bool,
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

#[derive(Debug, Default, Args, Clone, PartialEq, Eq)]
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

    /// Swap this sandbox's credential for a short session of one repository, and save it: an
    /// agent token (HUSHRUN_TOKEN), a Cursor cloud agent's ID token, or a GitHub Actions job's
    /// ID token. For agents, with no browser.
    #[arg(long, conflicts_with = "no_browser")]
    pub session: bool,

    /// With --session: how long it lasts, 1 to 12 hours [default: the server's, 4].
    #[arg(long, value_name = "HOURS", requires = "session", value_parser = clap::value_parser!(u32).range(1..=12))]
    pub hours: Option<u32>,

    /// With --session: the repository, for an agent token of the whole org, such as
    /// github.com/acme/shop [default: the repository hushrun runs in].
    #[arg(long, value_name = "REPO", requires = "session")]
    pub repository: Option<String>,
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

    /// A bearer token, with no machine key: for a sandbox whose network adds the token to
    /// hushrun's requests (HUSHRUN_AUTH=proxy), or one that swaps it for a session. The server
    /// records its results as the agent's, unsigned.
    #[arg(long)]
    pub bearer: bool,

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
