//! One `hushrun run`: what the parent hands the wrapper, the log of outcomes, and the summary
//! line (B-20).
//!
//! `hushrun run` calls [`begin`] before it starts COMMAND, sets [`Session::env`] on the child,
//! and calls [`Session::finish`] after it exits. The wrapper finds the session through
//! [`SESSION`] and reads no setting of its own, so every compile of one run sees one mode.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::audit::Audit;
use crate::model::{Class, Mode, NotStored, Outcome, PassedThrough, Programs, ProgramsSetting};
use crate::settings::{self, Overrides, Settings};
use crate::store::{DAY, Store};
use crate::unit::Checked;

/// The variable that names the session's folder for the wrapper.
pub const SESSION: &str = "HUSHRUN_BUILD_SESSION";

/// The variable that names a file each compile of the run appends its keys' parts to, one JSON
/// line per compile ([`Current::key_log`]). Read once, as `hushrun run` begins; a path that is
/// not absolute is ignored, since compiles run in other folders.
pub const KEY_LOG: &str = "HUSHRUN_BUILD_KEY_LOG";

/// The variable cargo reads the wrapper from.
pub const RUSTC_WRAPPER: &str = "RUSTC_WRAPPER";

/// What `hushrun run` knows when it starts COMMAND.
pub struct Start<'a> {
    /// The per-user settings file (B-21).
    pub settings: &'a Path,
    /// hushrun's cache directory. `None` when the person turned caches off.
    pub cache_dir: Option<&'a Path>,
    /// The hushrun binary that is running.
    pub exe: &'a Path,
    /// Whether this is CI (C-14's detection).
    pub in_ci: bool,
    /// The repository's name, when hushrun knows it. Blocks record it (B-13).
    pub repository: Option<&'a str>,
    /// The repository's `build: env:` (B-35): variables every compile is keyed by.
    pub repository_env: &'a [String],
    /// The environment of `hushrun run`.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    /// The command's time.
    pub now: SystemTime,
}

/// Why a run has no session: COMMAND runs with nothing set (B-2, B-19).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotBegun {
    /// The mode is `off`.
    Off,
    /// CI: nothing is reused there yet.
    Ci,
    /// Windows: not yet.
    Windows,
    /// `RUSTC_WRAPPER` is set already, and stays as it is.
    WrapperSet,
    /// hushrun has no cache directory.
    NoCache,
    /// The settings, a variable or the store's folder can't be used. The text says which and
    /// why, for one line on stderr.
    Unusable(String),
}

/// The cache of a build tool a run serves (B-77, B-170): what COMMAND gets for it, and its
/// listener, which stops when the session drops it.
#[derive(Debug)]
struct ToolService {
    kind: crate::model::Kind,
    /// What COMMAND gets for the tool: Gradle's script, URL and token; Maven's `MAVEN_OPTS`.
    env: Vec<(OsString, OsString)>,
    /// Where the cache learns why this run keeps nothing it builds, once the run knows: the
    /// property by which COMMAND's Maven skips tests (B-175).
    keeps_nothing: Option<std::sync::Arc<std::sync::Mutex<Option<String>>>>,
    _running: crate::cache::service::Running,
}

/// One run's session.
#[derive(Debug)]
pub struct Session {
    current: Current,
    link: PathBuf,
    /// What the check before COMMAND removed, as the summary says it (B-23).
    rebuilt: Vec<String>,
    /// Lines the run adds to its summary: what its relay said (B-26).
    more: Vec<String>,
    /// The Go cache program COMMAND gets (B-39), when the run is a Go module's and shares or
    /// reads a baked store.
    go: Option<crate::go::probe::Go>,
    /// Whether the run asked for Go's cache program already, given one or not: it asks once.
    go_asked: bool,
    /// The xcconfig file COMMAND's Xcode reads (B-84), when the run serves Xcode's cache.
    xcode: Option<PathBuf>,
    /// Another compiler wrapper is set (B-2): COMMAND gets no `RUSTC_WRAPPER` of hushrun's, and
    /// the session serves Go alone.
    rustc_left_out: bool,
    /// What COMMAND gets for C and C++ (B-50): the launcher variables CMake reads, those the
    /// person hadn't set, and `HUSHRUN_CC`.
    cc_env: Vec<(OsString, OsString)>,
    /// The folder of the compiler programs make's commands find first on `PATH` (B-231).
    cc_programs: Option<PathBuf>,
    /// The person's `cc_maps` setting as the run began (B-55).
    cc_maps_setting: bool,
    /// Which manifest the run leaves in the store when it ends (B-63), when it is a check's.
    slot: Option<crate::manifests::Slot>,
    /// The caches of build tools this run serves (B-77, B-170), each stopping with the session.
    tools: Vec<ToolService>,
    /// What COMMAND gets for TypeScript (B-100): `NODE_OPTIONS` with the hook, and `HUSHRUN_TSC`.
    tsc_env: Vec<(OsString, OsString)>,
}

/// What the wrapper reads at each compiler call: written once by [`begin`], so every compile of
/// one run sees the same mode and options, and none reads the settings itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Current {
    /// The mode of this run: `observe` or `on`.
    pub mode: Mode,
    /// The store's folder (B-13).
    pub store: PathBuf,
    /// How long blocks are kept (B-14).
    pub keep_days: u32,
    /// Packages never stored (B-4).
    pub never_store: Vec<String>,
    /// The person's extra variables for the first key (B-5).
    pub env: Vec<String>,
    /// The repository's `build: env:` (B-35): variables every compile of the repository is
    /// keyed by, on every machine.
    #[serde(default)]
    pub repository_env: Vec<String>,
    /// Nothing is stored with less free space than this (B-22). 0 turns the rule off.
    pub min_free_gb: u32,
    /// What the person chose for programs on an Apple target (B-21), as the settings said when
    /// the run began. What the wrapper does with a program is [`Current::programs`]. Written
    /// only when `tied`, and a `movable` an earlier hushrun wrote reads as nobody's choice.
    #[serde(
        default,
        rename = "programs",
        skip_serializing_if = "ProgramsSetting::is_follow"
    )]
    pub programs_setting: ProgramsSetting,
    /// The repository's name, when hushrun knows it (B-13).
    pub repository: Option<String>,
    /// When the run started, in milliseconds since the epoch. The audit's pick uses it (B-16).
    pub started_ms: u64,
    /// Folders that are the same for every checkout on this machine (B-8): the cargo home, the
    /// rustup home and the system's own.
    pub shared: Vec<String>,
    /// The cargo home, as `hushrun run` found it: a base (B-8), and searched for in outputs.
    #[serde(default)]
    pub cargo_home: Option<String>,
    /// The rustup home, as `hushrun run` found it: searched for in outputs.
    #[serde(default)]
    pub rustup_home: Option<String>,
    /// The home folder, `HOME`: searched for in outputs.
    #[serde(default)]
    pub home: Option<String>,
    /// Where each compile appends one line of its keys' parts, when `HUSHRUN_BUILD_KEY_LOG`
    /// named a file as `hushrun run` began: what tells which part of a key differs between two
    /// machines. Nothing is written otherwise.
    #[serde(default)]
    pub key_log: Option<PathBuf>,
    /// How many of the compiles that could be reused this run audits (B-16), from
    /// `HUSHRUN_BUILD_AUDIT` as `hushrun run` began.
    #[serde(default)]
    pub audit: Audit,
    /// The session's folder.
    pub folder: PathBuf,
    /// How the run stands with its backend's shared store (B-26, B-34).
    #[serde(default)]
    pub share: Shared,
    /// The person's share setting for this command (B-33): it lowers the org's shared mode on
    /// this machine and never raises it.
    #[serde(default = "share_on")]
    pub share_setting: Mode,
    /// What the C and C++ launcher reads (B-50): absent, it passes every call through (B-62).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<crate::cc::CcRun>,
    /// What hushrun's Gradle cache reads (B-79): absent, the run serves Gradle nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradle: Option<crate::gradle::GradleRun>,
    /// What hushrun's Maven cache reads (B-173): absent, the run serves Maven nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maven: Option<crate::maven::MavenRun>,
    /// What the TypeScript helper reads (B-100): absent, it passes every call through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tsc: Option<crate::tsc::TscRun>,
    /// The baked store the run reads underneath its store (B-63, B-64), as its state said when
    /// the run began. Absent: none, or one the run ignores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lower: Option<Reading>,
    /// B-72: the repository's own crates, compiled as empty stubs in this run (`hushrun build
    /// bake --deps-only`): never stored, never reused, never recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stub: Option<Stub>,
}

/// B-72: which compiles a dependencies-only run stubs: a crate of one of `packages` (the
/// lockfile's path packages, which no registry or git source names) whose `CARGO_MANIFEST_DIR`
/// is inside `under` (the checkout). Its one source file is replaced by an empty one in the
/// session's folder, so its dependents compile and the third-party crates under them compile as
/// the real build compiles them, with the same keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stub {
    pub packages: Vec<String>,
    pub under: PathBuf,
}

/// The stub sources in a session's folder (B-72): a library's, and a program's.
pub const STUB_LIB: &str = "stub-lib.rs";
pub const STUB_MAIN: &str = "stub-main.rs";

impl Stub {
    /// B-72: the arguments to run instead of `args` for a compile of the process whose variables
    /// `var` gives, when it is one of the repository's own crates: its one `.rs` argument replaced
    /// by the session folder's stub (a program's for a `bin` crate that is no test). `None` for
    /// any other compile, or one whose source isn't one argument ending in `.rs`: that compiles
    /// as asked.
    pub fn args(
        &self,
        folder: &Path,
        args: &[OsString],
        var: &dyn Fn(&str) -> Option<OsString>,
    ) -> Option<Vec<OsString>> {
        let package = var("CARGO_PKG_NAME")?;
        if !self.packages.iter().any(|p| OsStr::new(p) == package) {
            return None;
        }
        let dir = PathBuf::from(var("CARGO_MANIFEST_DIR")?);
        if !dir.starts_with(&self.under) {
            return None;
        }
        let sources: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                a.to_str()
                    .is_some_and(|a| a.ends_with(".rs") && !a.starts_with('-'))
            })
            .map(|(i, _)| i)
            .collect();
        let [at] = sources[..] else {
            return None;
        };
        let bin = args
            .windows(2)
            .any(|w| w[0] == "--crate-type" && w[1] == "bin")
            || args.iter().any(|a| a == "--crate-type=bin");
        let test = args.iter().any(|a| a == "--test");
        let stub = folder.join(if bin && !test { STUB_MAIN } else { STUB_LIB });
        let mut out = args.to_vec();
        out[at] = stub.into_os_string();
        Some(out)
    }
}

/// How a run reads its baked store (B-63): the two states of [`crate::baked::State`] a run reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reading {
    /// Its blocks are reused without asking.
    Current(crate::baked::Lower),
    /// Its source named a newer manifest: its blocks stand as downloads no server vouched for
    /// yet (B-34).
    Replaced(crate::baked::Lower),
}

impl Reading {
    /// The baked store.
    pub fn lower(&self) -> &crate::baked::Lower {
        match self {
            Reading::Current(lower) | Reading::Replaced(lower) => lower,
        }
    }

    fn lower_mut(&mut self) -> &mut crate::baked::Lower {
        match self {
            Reading::Current(lower) | Reading::Replaced(lower) => lower,
        }
    }

    /// The reading of a state, when the run reads the store at all.
    pub fn of(state: &crate::baked::State) -> Option<Reading> {
        match state {
            crate::baked::State::Current(lower) => Some(Reading::Current(lower.clone())),
            crate::baked::State::Replaced(lower) => Some(Reading::Replaced(lower.clone())),
            _ => None,
        }
    }
}

fn share_on() -> Mode {
    Mode::On
}

/// How a run stands with its backend's shared store (B-26, B-34): the one state every wrapper of
/// the run reads to know whether there is a relay to ask, and whether a block that was only
/// downloaded may be reused.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shared {
    /// The run shares nothing, by a decision: the org doesn't share, the person turned sharing
    /// off on this machine, nobody is signed in, or the command is one that never shares.
    /// Nothing goes to or comes from a shared store, and a block that was only downloaded is
    /// not reused (B-34).
    #[default]
    Off,
    /// The run would share, and its backend was asked in this run and couldn't be reached: no
    /// relay, and a downloaded block's entry of any age counts (B-34).
    Unreached,
    /// The run's relay: where its socket is and who the run is.
    Relay(Share),
}

impl Current {
    /// Whether the run shares or would (B-34): it has a relay, or its backend was asked and
    /// couldn't be reached. The two are one answer, so that no key turns with the network.
    pub fn shares(&self) -> bool {
        self.share != Shared::Off
    }

    /// What the wrapper does with this run's programs on an Apple target (B-9, B-21).
    pub fn programs(&self) -> Programs {
        self.programs_setting.in_a_run(self.shares())
    }

    /// B-64: the baked store a wrapper, the Go cache program or the launcher of this run looks
    /// in for blocks of `kind`, after this machine's own store and before a relay: a current one
    /// always; a replaced one only where a block from it could be reused (the run shares or
    /// would, B-34), since elsewhere opening it would only be undone.
    pub fn lower_for(&self, kind: crate::model::Kind) -> Option<&crate::baked::Lower> {
        match &self.lower {
            Some(Reading::Current(lower)) if lower.reads(kind) => Some(lower),
            Some(Reading::Replaced(lower)) if self.shares() && lower.reads(kind) => Some(lower),
            _ => None,
        }
    }

    /// B-16, B-34, B-63: how the audit samples a reuse of a block with record `origin`, the one
    /// definition the `rustc` wrapper, Go's cache program and the C and C++ launcher pick by. A
    /// block only downloaded and not reused here yet is at its first reuse here: one in ten
    /// ([`crate::audit::Reuse::FirstDownload`]), unless it was opened from the run's baked store,
    /// current (not replaced), under evidence that verifies where it came from and is this run's
    /// (`baked::Evidence::current`: the server's word for its row in the repository's trusted
    /// scope given in this run, or a signature by a signer the repository names that passes
    /// `not_before` where the run has no backend to ask): then never
    /// ([`crate::audit::Reuse::FirstVerified`]). A block from a store the person accepted
    /// unverified, or signed where the backend didn't answer or isn't its source, keeps the one in
    /// ten; every other reuse is one in fifty.
    pub fn reuse_pick(&self, origin: Option<&crate::origin::Origin>) -> crate::audit::Reuse {
        let Some(origin) = origin else {
            return crate::audit::Reuse::Any;
        };
        if !origin.downloaded_only() || origin.reused {
            return crate::audit::Reuse::Any;
        }
        // Verified now: the store is current (not replaced), and its evidence for this block is
        // this run's (`Evidence::current`).
        let verified = self.lower.as_ref().is_some_and(|reading| {
            let lower = reading.lower();
            matches!(reading, Reading::Current(_))
                && lower.evidence.current()
                && origin.baked_in(&lower.contents)
        });
        if verified {
            crate::audit::Reuse::FirstVerified
        } else {
            crate::audit::Reuse::FirstDownload
        }
    }

    /// B-16, B-63: whether the audit picks this reuse of a block with record `origin`: every one
    /// with `HUSHRUN_BUILD_AUDIT=every`, none with `none`, else as [`Current::reuse_pick`] says,
    /// `sample` asked for the pick (the caller's port) except at a verified baked block's first
    /// reuse here, which is never audited. The one place the three callers decide it.
    pub fn audits_reuse(
        &self,
        origin: Option<&crate::origin::Origin>,
        sample: impl FnOnce(crate::audit::Reuse) -> bool,
    ) -> bool {
        let reuse = self.reuse_pick(origin);
        self.audit
            .picks(|| reuse != crate::audit::Reuse::FirstVerified && sample(reuse))
    }

    /// Whether a block may be reused as far as who vouches for it goes (B-34, B-63): a block
    /// only opened from the run's baked store, while that store is current and is the very
    /// `baked.json` its record names, and the person accepted it unverified (in a run that writes
    /// nothing others trust) or a
    /// signature by a signer the repository names covers it (B-67), may without asking; every
    /// other block as [`Shared::may_reuse`] says, which a block the
    /// backend vouched for as the run began passes on its fresh word.
    pub fn may_reuse(
        &self,
        origin: Option<&crate::origin::Origin>,
        now_ms: u64,
        vouch: crate::relay::Request,
        ask: &mut dyn FnMut(&Path, &crate::relay::Request) -> Option<crate::relay::Answer>,
    ) -> bool {
        if let (Some(Reading::Current(lower)), Some(origin)) = (&self.lower, origin)
            && lower.evidence.reuses_unasked()
            && origin.baked_in(&lower.contents)
        {
            return true;
        }
        self.share.may_reuse(origin, now_ms, vouch, ask)
    }
}

impl Shared {
    /// The run's relay, when it has one.
    pub fn relay(&self) -> Option<&Share> {
        match self {
            Shared::Relay(share) => Some(share),
            Shared::Off | Shared::Unreached => None,
        }
    }

    /// Whether a block may be reused as far as who vouches for it goes (B-34): the one
    /// definition the `rustc` wrapper, the Go cache program and the C and C++ launcher decide
    /// by, from the block's
    /// record ([`Origin::standing`](crate::origin::Origin::standing)) and this state. A block
    /// built here, or with no record, may. One only downloaded never may once a server said it
    /// no longer serves it, and never in a run that isn't sharing by a decision. In a run whose
    /// backend couldn't be reached, its entry of any age counts. With a relay: on an entry for
    /// this run's host, org and repository under a day old it may unless the relay says sharing
    /// stopped by a decision; else the relay asks the server (`vouch`, the question), and
    /// "served" or "no server could be reached in this run" lets it. `ask` is the relay's
    /// client: a relay that doesn't answer is no decision for a fresh entry, and a miss for a
    /// stale one.
    pub fn may_reuse(
        &self,
        origin: Option<&crate::origin::Origin>,
        now_ms: u64,
        vouch: crate::relay::Request,
        ask: &mut dyn FnMut(&Path, &crate::relay::Request) -> Option<crate::relay::Answer>,
    ) -> bool {
        use crate::origin::Standing;
        use crate::relay::{Answer, Request};
        let Some(origin) = origin else {
            return true;
        };
        let run = self
            .relay()
            .map(|s| (s.by.host.as_str(), s.by.org.as_str(), s.repository.as_str()));
        match (origin.standing(run, now_ms), self) {
            (Standing::Any, _) => true,
            (Standing::Withdrawn, _) | (_, Shared::Off) => false,
            (_, Shared::Unreached) => true,
            // Only an answer that says so stops a reuse: a relay that is gone is no decision.
            (Standing::Fresh, Shared::Relay(share)) => {
                !matches!(ask(&share.socket, &Request::Sharing), Some(Answer::None))
            }
            (Standing::Stale, Shared::Relay(share)) => matches!(
                ask(&share.socket, &vouch),
                Some(Answer::Served | Answer::Offline)
            ),
        }
    }
}

/// What a wrapper needs of its run's relay (B-26, B-32).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Share {
    /// The relay's socket.
    pub socket: PathBuf,
    /// The relay's run: the name of its folders in the store.
    pub run: String,
    /// Who the run is: the backend's host, the org and the credential's fingerprint.
    pub by: crate::origin::Identity,
    /// The run's repository as the server reads it; empty when none.
    #[serde(default)]
    pub repository: String,
}

/// One compile's line in the run's log (B-20).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// How the call ended (B-3).
    pub outcome: Outcome,
    /// What it produces, when the call was read that far.
    pub class: Option<Class>,
    /// The crate's name, or empty.
    pub crate_name: String,
    /// The package's `name-version`, or empty when unknown: every build script's crate is
    /// `build_script_build`, and this tells them apart.
    #[serde(default)]
    pub package: String,
    /// How long rustc ran, in milliseconds. 0 when it didn't.
    pub rustc_ms: u64,
    /// For a reuse: how long rustc took when the block was built, less what the reuse took.
    pub saved_ms: u64,
    /// Bytes this call added to the store.
    pub stored_bytes: u64,
    /// Whether a block existed for this compile before it ran.
    pub found: bool,
    /// Whether the fresh outputs differed from that block's (B-1 `observe`, B-16).
    pub differed: bool,
    /// One line for the person, when something needs saying (a suspension, low space).
    pub note: Option<String>,
    /// Whether the block may leave the machine, and why not: for a compile that got as far as
    /// searching its outputs, and for a reuse (from the block's record). `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable: Option<crate::portable::Portability>,
    /// The compile's first key, when computed: with `key`, the manifest's pair (B-30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<hushrun_core::Digest>,
    /// The block's key: of the block reused, or of the block this compile stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<hushrun_core::Digest>,
    /// The block was downloaded from the shared store for this compile (B-26).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub downloaded: bool,
    /// The block was opened from the run's baked store for this compile (B-64).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub baked: bool,
    /// A Go package archive's line, logged by the Go cache program (B-46): absent for the
    /// `rustc` wrapper's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go: Option<GoEntry>,
    /// A C or C++ object's line, logged by the launcher (B-61): absent for the others'.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<crate::cc::launcher::CcEntry>,
    /// The Xcode service's line (B-91), when it logged it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xcode: Option<XcodeEntry>,
    /// A Gradle task's entry's line, logged by hushrun's Gradle cache (B-83): absent for the
    /// others'. Its task is in `crate_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradle: Option<GradleEntry>,
    /// A Maven module's line, logged by hushrun's Maven cache (B-177): absent for the others'.
    /// Its module is in `crate_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maven: Option<MavenEntry>,
    /// A TypeScript compile's line, logged by the helper (B-109): absent for the others'.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tsc: Option<crate::tsc::helper::TscEntry>,
}

/// What a Maven module's line says beside the rest (B-177).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MavenEntry {
    /// Why it stays on this machine, when Maven stored it and it doesn't go up (B-173).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<crate::maven::Local>,
}

/// What a Gradle entry's line says beside the rest (B-83).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GradleEntry {
    /// Why it stays on this machine, when Gradle stored it and it doesn't go up (B-79).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<crate::gradle::Local>,
}

/// What a Go package archive's line says beside the rest (B-46).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoEntry {
    /// It was served from Go's own cache (`GOCACHE`), as plain `go` would serve it: no saving
    /// of hushrun's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub go_cache: bool,
    /// Why it stays on this machine, when it was compiled and doesn't go up (B-42).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<crate::go::archive::Local>,
}

/// What an Xcode value's line says beside the rest (B-91).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct XcodeEntry {
    /// It was served from the service's own cache on this machine ([`crate::xcode::cas`]): what
    /// another checkout, branch or the Xcode app compiled here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local_cache: bool,
    /// Why it stays on this machine, when Xcode put it and it doesn't go up (B-89).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    /// How many objects its block holds.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub objects: u64,
    /// The Xcode build it was compiled with, as its manifest's platform (B-90).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub platform: String,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Entry {
    /// Which adapter logged the line.
    pub fn kind(&self) -> crate::model::Kind {
        if self.xcode.is_some() {
            crate::model::Kind::Xcode
        } else if self.go.is_some() {
            crate::model::Kind::Go
        } else if self.cc.is_some() {
            crate::model::Kind::Cc
        } else if self.gradle.is_some() {
            crate::model::Kind::Gradle
        } else if self.maven.is_some() {
            crate::model::Kind::Maven
        } else if self.tsc.is_some() {
            crate::model::Kind::Tsc
        } else {
            crate::model::Kind::Rustc
        }
    }
}

const SESSION_FILE: &str = "session.json";
const LOG_FILE: &str = "log.jsonl";
/// The store's folder of sessions: one per run, removed when the run ends.
const SESSIONS: &str = "sessions";
/// The store's folder of wrapper links: one per hushrun binary.
const BIN: &str = "bin";
/// A session untouched for longer belongs to a run that was killed.
const STALE_SESSION: Duration = Duration::from_secs(2 * DAY.as_secs());
/// The most note lines a summary writes.
const MAX_NOTES: usize = 5;

/// System folders that every checkout on a machine shares (B-8).
const SYSTEM: &[&str] = &[
    "/usr",
    "/opt",
    "/Library",
    "/System",
    "/Applications",
    "/nix",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
];

/// The session of the run this process belongs to, if any. `None` is a reason to pass through:
/// no session, or one that can't be read.
pub fn current(env: &dyn Fn(&str) -> Option<OsString>) -> Option<Current> {
    let folder = PathBuf::from(env(SESSION).filter(|value| !value.is_empty())?);
    let text = std::fs::read(folder.join(SESSION_FILE)).ok()?;
    serde_json::from_slice(&text).ok()
}

impl Current {
    /// Appends one line to the run's log. A log that can't be written never fails a compile.
    pub fn log(&self, entry: &Entry) {
        use std::io::Write;
        let Ok(mut line) = serde_json::to_vec(entry) else {
            return;
        };
        line.push(b'\n');
        // One write of one whole line, to a file opened for appending: lines from compiles
        // running at once don't mix.
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.folder.join(LOG_FILE))
        {
            let _ = file.write_all(&line);
        }
    }

    /// The lines logged so far, in the order they were written. A line that can't be read is
    /// left out.
    pub fn entries(&self) -> Vec<Entry> {
        let Ok(text) = std::fs::read_to_string(self.folder.join(LOG_FILE)) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

/// A folder a variable names, else `default` under the home folder, as cargo and rustup find
/// theirs.
pub(crate) fn under_home(
    env: &dyn Fn(&str) -> Option<OsString>,
    variable: &str,
    default: &str,
) -> Option<String> {
    let home = env("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    env(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(default)))
        .and_then(|path| path.to_str().map(str::to_string))
}

fn shared_folders(env: &dyn Fn(&str) -> Option<OsString>) -> Vec<String> {
    let mut shared: Vec<String> = SYSTEM.iter().map(|s| s.to_string()).collect();
    shared.extend(under_home(env, "CARGO_HOME", ".cargo"));
    shared.extend(under_home(env, "RUSTUP_HOME", ".rustup"));
    shared
}

/// What a run's compiles came to, counted from its log (B-20). Calls that compile none of the
/// build's own crates count nowhere: cargo's probes (`--print`, `-vV`, no `--crate-name`), and
/// the probes a build script runs through the wrapper (autocfg's, with `-o`, or without
/// `-C extra-filename`, `--out-dir` or `--json=artifacts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// The run's mode.
    pub mode: Mode,
    /// Compiles: every call but the probes.
    pub compiles: u64,
    /// Compiles whose block was put in place, with rustc not started.
    pub reused: u64,
    /// Of those, the ones whose block was downloaded from the shared store for them (B-26).
    pub downloaded: u64,
    /// Of those, the ones whose block was opened from the run's baked store for them (B-64).
    pub baked: u64,
    /// Compiles that ran and were stored.
    pub built_and_stored: u64,
    /// Compiles that ran and were not stored, for any reason.
    pub built_not_stored: u64,
    /// Compiles that ran exactly as asked: an incremental rebuild, a `cdylib`, clippy's driver.
    pub passed_through: u64,
    /// Compiles that found a block. In `observe`, the ones that would have been reused.
    pub found: u64,
    /// Compiles whose fresh outputs differed from the block found (B-1 `observe`, B-16).
    pub differed: u64,
    /// Milliseconds reuse saved.
    pub saved_ms: u64,
    /// Bytes the run added to the store.
    pub stored_bytes: u64,
    /// Compiles not stored because the store's volume had too little free space (B-22).
    pub low_space: u64,
    /// The free space, in GB, below which nothing is stored (B-22).
    pub min_free_gb: u32,
    /// The distinct notes the compiles left, in the order first seen.
    pub notes: Vec<String>,
    /// Compiles whose block may not leave the machine because an output holds the value of a
    /// variable not in its key (B-24).
    pub unkeyed: u64,
    /// Those variables, by name only, sorted.
    pub unkeyed_variables: Vec<String>,
    /// What the Go cache program's lines came to (B-46), counted apart from the crates.
    pub go: GoCounts,
    /// What the C and C++ launcher's lines came to (B-61), counted apart from the crates.
    pub cc: CcCounts,
    /// What the TypeScript helper's lines came to (B-109), counted apart from the crates.
    pub tsc: TscCounts,
    /// What hushrun's Gradle cache's lines came to (B-83), counted apart from the crates.
    pub gradle: ToolCounts,
    /// What hushrun's Maven cache's lines came to (B-177), counted apart from the crates.
    pub maven: ToolCounts,
}

/// The words of a build tool's summary lines (B-83, B-177).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Units {
    /// The tool, after `build: `: `gradle`.
    pub tool: &'static str,
    /// One unit and several: `task`, `tasks`.
    pub one: &'static str,
    pub many: &'static str,
    /// What the tool did with one it didn't find: `run and stored`.
    pub stored: &'static str,
    /// Where one that stays here was made: `run here`.
    pub made_here: &'static str,
    /// \[B-210\] Whether the line of those that stay here names them.
    pub names_kept: bool,
}

impl Units {
    /// Gradle's tasks (B-83, B-210).
    pub const GRADLE: Units = Units {
        tool: "gradle",
        one: "task",
        many: "tasks",
        stored: "run and stored",
        made_here: "run here",
        names_kept: true,
    };

    /// Maven's modules (B-177).
    pub const MAVEN: Units = Units {
        tool: "maven",
        one: "module",
        many: "modules",
        stored: "built and stored",
        made_here: "built here",
        names_kept: false,
    };

    fn count(&self, n: u64) -> String {
        if n == 1 {
            format!("1 {}", self.one)
        } else {
            format!("{n} {}", self.many)
        }
    }
}

/// What a run's entries of one build tool's cache came to (B-83, B-177).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ToolCounts {
    /// Entries the tool asked hushrun's cache for and found, or stored in it.
    pub units: u64,
    /// Served to the tool from hushrun's store.
    pub reused: u64,
    /// Of those, downloaded from the shared store for it.
    pub downloaded: u64,
    /// Stored by the tool after it made the unit.
    pub built: u64,
    /// Milliseconds the units served took where they were made.
    pub saved_ms: u64,
    /// Units the tool stored that stay on this machine, by the words of their reason, in the
    /// order first seen.
    pub local: Vec<(&'static str, u64)>,
    /// \[B-210\] The units of those, by the tool's name, in the order first seen, each once.
    #[serde(skip)]
    pub local_names: Vec<String>,
}

impl ToolCounts {
    /// Counts `entry`, which stays on this machine for the reason `local` says, if any.
    fn add(&mut self, entry: &Entry, local: Option<&'static str>) {
        self.units += 1;
        match entry.outcome {
            Outcome::Reused => {
                self.reused += 1;
                self.downloaded += u64::from(entry.downloaded);
                self.saved_ms = self.saved_ms.saturating_add(entry.saved_ms);
            }
            Outcome::BuiltAndStored | Outcome::BuiltNotStored(_) | Outcome::PassedThrough(_) => {
                self.built += 1;
            }
        }
        if let Some(local) = local {
            match self.local.iter_mut().find(|(seen, _)| *seen == local) {
                Some((_, n)) => *n += 1,
                None => self.local.push((local, 1)),
            }
            if !entry.crate_name.is_empty() && !self.local_names.contains(&entry.crate_name) {
                self.local_names.push(entry.crate_name.clone());
            }
        }
    }

    /// B-83's and B-177's lines for the tool `units` names, without a prefix. Empty when the
    /// tool asked nothing.
    pub fn lines(&self, units: &Units) -> Vec<String> {
        if self.units == 0 {
            return Vec::new();
        }
        let mut parts = Vec::new();
        if self.reused > 0 {
            let mut part = format!("{} from hushrun's cache", self.reused);
            part.push_str(&from_where(self.downloaded, 0));
            parts.push(part);
        }
        if self.built > 0 {
            parts.push(format!("{} {}", self.built, units.stored));
        }
        let mut first = format!(
            "build: {}: {}: {}",
            units.tool,
            units.count(self.units),
            parts.join(", ")
        );
        if self.saved_ms >= 1_000 {
            first.push_str(&format!(", saving about {}", took(self.saved_ms)));
        }
        let mut lines = vec![first];
        let kept: u64 = self.local.iter().map(|(_, n)| n).sum();
        if kept > 0 {
            let why: Vec<String> = self
                .local
                .iter()
                .map(|(words, n)| format!("{n}: {words}"))
                .collect();
            let mut line = format!(
                "build: {}: {} {} {} on this machine ({})",
                units.tool,
                units.count(kept),
                units.made_here,
                if kept == 1 { "stays" } else { "stay" },
                why.join(", ")
            );
            // \[B-210\] Which, so that a build can tell its own task from a plugin's (kapt's
            // stubs hold their sources' paths): three at most, and how many more.
            if units.names_kept && !self.local_names.is_empty() {
                let shown: Vec<&str> = self
                    .local_names
                    .iter()
                    .take(3)
                    .map(String::as_str)
                    .collect();
                line.push_str(&format!(": {}", shown.join(", ")));
                let more = self.local_names.len() - shown.len();
                if more > 0 {
                    line.push_str(&format!(" and {more} more"));
                }
            }
            lines.push(line);
        }
        lines
    }
}

/// What a run's TypeScript compiles came to (B-109).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TscCounts {
    /// Compiler calls the helper saw.
    pub calls: u64,
    /// Put in place without a compile.
    pub reused: u64,
    /// Of those, downloaded from the shared store for it (B-108).
    pub downloaded: u64,
    /// Compiles that ran, stored or not, passed through or not.
    pub built: u64,
    /// Compiles compared with a block found for them (an audit, `observe`).
    pub compared: u64,
    /// Of those, the ones that wrote other files: the key is suspended.
    pub differed: u64,
    /// Compiles that found a block: in `observe`, the ones that would have been reused.
    pub found: u64,
    /// Milliseconds reuse saved.
    pub saved_ms: u64,
    /// Passed through or not stored, by the helper's reason, in the order first seen.
    pub reasons: Vec<(String, u64)>,
}

impl TscCounts {
    fn add(&mut self, entry: &Entry) {
        let Some(tsc) = &entry.tsc else {
            return;
        };
        self.calls += 1;
        match entry.outcome {
            Outcome::Reused => {
                self.reused += 1;
                self.downloaded += u64::from(entry.downloaded);
                self.saved_ms = self.saved_ms.saturating_add(entry.saved_ms);
            }
            _ => self.built += 1,
        }
        self.found += u64::from(entry.found);
        if entry.found && entry.outcome != Outcome::Reused {
            self.compared += 1;
        }
        self.differed += u64::from(entry.differed);
        let counted = matches!(
            entry.outcome,
            Outcome::PassedThrough(_) | Outcome::BuiltNotStored(_)
        );
        if let Some(why) = tsc.why.as_deref().filter(|_| counted) {
            match self.reasons.iter_mut().find(|(seen, _)| seen == why) {
                Some((_, n)) => *n += 1,
                None => self.reasons.push((why.to_string(), 1)),
            }
        }
    }

    /// B-109's lines for TypeScript, without a prefix. Empty when the helper logged nothing.
    pub fn lines(&self, mode: Mode) -> Vec<String> {
        if self.calls == 0 {
            return Vec::new();
        }
        let compiles = |n: u64| {
            if n == 1 {
                "1 compile".to_string()
            } else {
                format!("{n} compiles")
            }
        };
        let mut first = if mode == Mode::Observe {
            format!(
                "build: typescript: observing: {} of {} would have been reused",
                self.found,
                compiles(self.calls)
            )
        } else {
            format!(
                "build: typescript: {}: {} reused{}, {} built",
                compiles(self.calls),
                self.reused,
                from_where(self.downloaded, 0),
                self.built
            )
        };
        if mode != Mode::Observe && self.saved_ms >= 1_000 {
            first.push_str(&format!(", saving about {}", took(self.saved_ms)));
        }
        if self.compared > 0 {
            first.push_str(&format!(
                ", {} compared with their blocks, {} differed",
                self.compared, self.differed
            ));
        }
        let mut lines = vec![first];
        if !self.reasons.is_empty() {
            let shown: Vec<String> = self
                .reasons
                .iter()
                .take(3)
                .map(|(why, n)| format!("{n} {why}"))
                .collect();
            let all: u64 = self.reasons.iter().map(|(_, n)| n).sum();
            let more = if self.reasons.len() > 3 { ", …" } else { "" };
            lines.push(format!(
                "build: typescript: {} run as asked or not stored ({}{more})",
                compiles(all),
                shown.join(", ")
            ));
        }
        lines
    }
}

/// What a run's C and C++ objects came to (B-61).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CcCounts {
    /// Compiles the launcher saw, but CMake's own configure checks.
    pub objects: u64,
    /// Objects put in place without a compile.
    pub reused: u64,
    /// Of those, downloaded from the shared store for it.
    pub downloaded: u64,
    /// Of those, opened from the run's baked store for it (B-64).
    pub baked: u64,
    /// Compiles that ran, stored or not, passed through or not.
    pub built: u64,
    /// Compiles that ran exactly as asked.
    pub passed_through: u64,
    /// Compiles compared with a block found for them (an audit, `observe`).
    pub compared: u64,
    /// Of those, the ones whose object differed: the key is suspended.
    pub differed: u64,
    /// Compiles that found a block: in `observe`, the ones that would have been reused.
    pub found: u64,
    /// Milliseconds reuse saved.
    pub saved_ms: u64,
    /// Passed through or not stored, by the launcher's reason, in the order first seen.
    pub reasons: Vec<(String, u64)>,
    /// Objects stored in a run that shares that stay on this machine (B-56, B-60), by reason,
    /// in the order first seen; an object with two reasons counts under each.
    pub kept: u64,
    pub kept_reasons: Vec<(String, u64)>,
}

impl CcCounts {
    fn add(&mut self, entry: &Entry) {
        let Some(cc) = &entry.cc else {
            return;
        };
        if cc.probe {
            return;
        }
        self.objects += 1;
        match entry.outcome {
            Outcome::Reused => {
                self.reused += 1;
                self.downloaded += u64::from(entry.downloaded);
                self.baked += u64::from(entry.baked);
                self.saved_ms = self.saved_ms.saturating_add(entry.saved_ms);
            }
            Outcome::PassedThrough(_) => {
                self.built += 1;
                self.passed_through += 1;
            }
            Outcome::BuiltAndStored | Outcome::BuiltNotStored(_) => self.built += 1,
        }
        self.found += u64::from(entry.found);
        if entry.found && entry.outcome != Outcome::Reused {
            self.compared += 1;
        }
        self.differed += u64::from(entry.differed);
        let counted = matches!(
            entry.outcome,
            Outcome::PassedThrough(_) | Outcome::BuiltNotStored(_)
        );
        if let Some(why) = cc.why.as_deref().filter(|_| counted) {
            match self.reasons.iter_mut().find(|(seen, _)| seen == why) {
                Some((_, n)) => *n += 1,
                None => self.reasons.push((why.to_string(), 1)),
            }
        }
        if cc.shared && entry.outcome == Outcome::BuiltAndStored && !cc.local.is_empty() {
            self.kept += 1;
            for why in &cc.local {
                match self.kept_reasons.iter_mut().find(|(seen, _)| seen == why) {
                    Some((_, n)) => *n += 1,
                    None => self.kept_reasons.push((why.clone(), 1)),
                }
            }
        }
    }

    /// B-61's lines for C and C++, without a prefix. Empty when the launcher logged no object.
    pub fn lines(&self, mode: Mode) -> Vec<String> {
        if self.objects == 0 {
            return Vec::new();
        }
        let objects = |n: u64| {
            if n == 1 {
                "1 object".to_string()
            } else {
                format!("{n} objects")
            }
        };
        let mut first = if mode == Mode::Observe {
            format!(
                "build: c/c++: observing: {} of {} would have been reused",
                self.found,
                objects(self.objects)
            )
        } else {
            let mut parts = Vec::new();
            let mut reused = format!("{} reused", self.reused);
            reused.push_str(&from_where(self.downloaded, self.baked));
            parts.push(reused);
            parts.push(format!("{} built", self.built));
            format!(
                "build: c/c++: {}: {}",
                objects(self.objects),
                parts.join(", ")
            )
        };
        if mode != Mode::Observe && self.saved_ms >= 1_000 {
            first.push_str(&format!(", saving about {}", took(self.saved_ms)));
        }
        if self.compared > 0 {
            first.push_str(&format!(
                ", {} compared with their blocks, {} differed",
                self.compared, self.differed
            ));
        }
        let mut lines = vec![first];
        if !self.reasons.is_empty() {
            let mut reasons = self.reasons.clone();
            reasons.sort_by(|a, b| b.1.cmp(&a.1));
            let shown: Vec<String> = reasons
                .iter()
                .take(3)
                .map(|(why, n)| format!("{n} {why}"))
                .collect();
            let all: u64 = reasons.iter().map(|(_, n)| n).sum();
            let more = if reasons.len() > 3 { ", …" } else { "" };
            lines.push(format!(
                "build: c/c++: {} compiled as asked or not stored ({}{more})",
                objects(all),
                shown.join(", ")
            ));
        }
        if self.kept > 0 {
            let why: Vec<String> = self
                .kept_reasons
                .iter()
                .map(|(why, n)| format!("{n} {why}"))
                .collect();
            lines.push(format!(
                "build: c/c++: {} compiled here {} on this machine ({})",
                objects(self.kept),
                if self.kept == 1 { "stays" } else { "stay" },
                why.join(", ")
            ));
        }
        lines
    }
}

/// What a run's Go package archives came to (B-46).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GoCounts {
    /// Package archives the run's `go` commands asked for or compiled, as the program logged
    /// them.
    pub packages: u64,
    /// Served from Go's own cache, as plain `go` would serve them.
    pub go_cache: u64,
    /// Served from hushrun's store.
    pub reused: u64,
    /// Of those, downloaded from the shared store for it.
    pub downloaded: u64,
    /// Of those, opened from the run's baked store for it (B-64).
    pub baked: u64,
    /// Compiled by Go.
    pub built: u64,
    /// Compiled and compared with a block of the same key (an audit, a block there already).
    pub compared: u64,
    /// Of those, the ones whose bytes differed.
    pub differed: u64,
    /// Served from Go's own cache with other bytes than hushrun's block of the same key: only
    /// counted, since Go's cache is no witness against a block (it keeps an altered command's
    /// builds under plain ids).
    pub cache_differed: u64,
    /// Milliseconds reuse from hushrun's store saved.
    pub saved_ms: u64,
    /// Compiled archives kept on this machine, by reason, in the order first seen.
    pub local: Vec<(crate::go::archive::Local, u64)>,
}

impl GoCounts {
    fn add(&mut self, entry: &Entry) {
        let Some(go) = &entry.go else {
            return;
        };
        self.packages += 1;
        match entry.outcome {
            Outcome::Reused if go.go_cache => self.go_cache += 1,
            Outcome::Reused => {
                self.reused += 1;
                self.downloaded += u64::from(entry.downloaded);
                self.baked += u64::from(entry.baked);
                self.saved_ms = self.saved_ms.saturating_add(entry.saved_ms);
            }
            Outcome::BuiltAndStored | Outcome::BuiltNotStored(_) | Outcome::PassedThrough(_) => {
                self.built += 1;
                if entry.found {
                    self.compared += 1;
                }
            }
        }
        if go.go_cache {
            self.cache_differed += u64::from(entry.differed);
        } else {
            self.differed += u64::from(entry.differed);
        }
        if let Some(local) = go.local
            && local != crate::go::archive::Local::NotSharing
        {
            match self.local.iter_mut().find(|(seen, _)| *seen == local) {
                Some((_, n)) => *n += 1,
                None => self.local.push((local, 1)),
            }
        }
    }

    /// B-46's lines for Go, without a prefix. Empty when no archive was logged.
    pub fn lines(&self) -> Vec<String> {
        if self.packages == 0 {
            return Vec::new();
        }
        let packages = |n: u64| {
            if n == 1 {
                "1 package".to_string()
            } else {
                format!("{n} packages")
            }
        };
        let mut parts = Vec::new();
        if self.go_cache > 0 {
            parts.push(format!("{} from Go's own cache", self.go_cache));
        }
        if self.reused > 0 {
            let mut part = format!("{} reused", self.reused);
            part.push_str(&from_where(self.downloaded, self.baked));
            parts.push(part);
        }
        if self.built > 0 {
            parts.push(format!("{} built", self.built));
        }
        let mut first = format!(
            "build: go: {}: {}",
            packages(self.packages),
            parts.join(", ")
        );
        if self.saved_ms >= 1_000 {
            first.push_str(&format!(", saving about {}", took(self.saved_ms)));
        }
        if self.compared > 0 {
            first.push_str(&format!(
                ", {} compared with their blocks, {} differed",
                self.compared, self.differed
            ));
        } else if self.differed > 0 {
            first.push_str(&format!(
                ", {} differed from their blocks and are not reused",
                self.differed
            ));
        }
        if self.cache_differed > 0 {
            first.push_str(&format!(
                ", {} in Go's own cache with other bytes than hushrun's blocks (not reported)",
                self.cache_differed
            ));
        }
        let mut lines = vec![first];
        let kept: u64 = self.local.iter().map(|(_, n)| n).sum();
        if kept > 0 {
            let why: Vec<String> = self
                .local
                .iter()
                .map(|(local, n)| format!("{n} {}", local.words()))
                .collect();
            lines.push(format!(
                "build: go: {} compiled here {} on this machine ({})",
                packages(kept),
                if kept == 1 { "stays" } else { "stay" },
                why.join(", ")
            ));
        }
        lines
    }
}

impl Counts {
    /// Counts a run's log.
    pub fn of(mode: Mode, min_free_gb: u32, entries: &[Entry]) -> Counts {
        let mut counts = Counts {
            mode,
            min_free_gb,
            ..Counts::default()
        };
        for entry in entries.iter().filter(|entry| !is_probe(entry.outcome)) {
            if entry.go.is_some() {
                counts.go.add(entry);
                continue;
            }
            if entry.cc.is_some() {
                counts.cc.add(entry);
                continue;
            }
            if let Some(gradle) = &entry.gradle {
                counts
                    .gradle
                    .add(entry, gradle.local.map(crate::gradle::Local::words));
                continue;
            }
            if let Some(maven) = &entry.maven {
                counts
                    .maven
                    .add(entry, maven.local.map(crate::maven::Local::words));
                continue;
            }
            if entry.tsc.is_some() {
                counts.tsc.add(entry);
                continue;
            }
            counts.compiles += 1;
            match entry.outcome {
                Outcome::Reused => {
                    counts.reused += 1;
                    counts.downloaded += u64::from(entry.downloaded);
                    counts.baked += u64::from(entry.baked);
                }
                Outcome::BuiltAndStored => counts.built_and_stored += 1,
                Outcome::BuiltNotStored(why) => {
                    counts.built_not_stored += 1;
                    if why == NotStored::LowSpace {
                        counts.low_space += 1;
                    }
                }
                Outcome::PassedThrough(_) => counts.passed_through += 1,
            }
            counts.found += u64::from(entry.found);
            counts.differed += u64::from(entry.differed);
            counts.saved_ms = counts.saved_ms.saturating_add(entry.saved_ms);
            if let Some(portability) = &entry.portable
                && portability
                    .reasons
                    .contains(&crate::portable::Reason::UnkeyedValue)
            {
                counts.unkeyed += 1;
                counts
                    .unkeyed_variables
                    .extend(portability.variables.iter().cloned());
                counts.unkeyed_variables.sort();
                counts.unkeyed_variables.dedup();
            }
            counts.stored_bytes = counts.stored_bytes.saturating_add(entry.stored_bytes);
            if let Some(note) = entry.note.as_deref().map(str::trim)
                && !note.is_empty()
                && !counts.notes.iter().any(|seen| seen == note)
            {
                counts.notes.push(note.to_string());
            }
        }
        counts
    }

    /// Compiles where rustc ran: stored, not stored and passed through.
    pub fn built(&self) -> u64 {
        self.compiles - self.reused
    }
}

/// Probes: calls that compile none of the build's own crates. Cargo always passes a crate of
/// the build `--crate-name`, `-C extra-filename`, `--out-dir` and `--json=artifacts`, and never
/// `-o`; a call without them is cargo's probe or a build script's own.
fn is_probe(outcome: Outcome) -> bool {
    matches!(
        outcome,
        Outcome::PassedThrough(
            PassedThrough::Probe
                | PassedThrough::NoCrateName
                | PassedThrough::NoExtraFilename
                | PassedThrough::NoOutDir
                | PassedThrough::OutputFile
                | PassedThrough::NoArtifacts
        )
    )
}

/// The line a run whose audit is off ends with (B-16).
const AUDIT_OFF: &str = "build: the audit is off for this run (HUSHRUN_BUILD_AUDIT=none): no \
                         reused block was compared with a fresh compile";

/// What a run's compiles came to (B-20).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    counts: Counts,
    lines: Vec<String>,
}

impl Summary {
    /// The summary of a run's log.
    pub fn of(mode: Mode, min_free_gb: u32, entries: &[Entry]) -> Summary {
        Summary::of_run(mode, min_free_gb, entries, &[])
    }

    /// The summary of a run's log, with what the check before COMMAND removed (B-23).
    pub fn of_run(mode: Mode, min_free_gb: u32, entries: &[Entry], rebuilt: &[String]) -> Summary {
        let counts = Counts::of(mode, min_free_gb, entries);
        let lines = lines(&counts, rebuilt);
        Summary { counts, lines }
    }

    /// The lines `hushrun run` writes after COMMAND, without a prefix. Empty when nothing was
    /// compiled.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// The numbers the lines come from.
    pub fn counts(&self) -> &Counts {
        &self.counts
    }
}

/// B-20's lines: the totals, then what the check before COMMAND removed (B-23), then at most
/// [`MAX_NOTES`] notes, then low space once (B-22).
fn lines(counts: &Counts, rebuilt: &[String]) -> Vec<String> {
    let mut lines = rust_lines(counts, rebuilt);
    lines.extend(counts.go.lines());
    lines.extend(counts.cc.lines(counts.mode));
    lines.extend(counts.gradle.lines(&Units::GRADLE));
    lines.extend(counts.maven.lines(&Units::MAVEN));
    lines.extend(counts.tsc.lines(counts.mode));
    lines
}

fn rust_lines(counts: &Counts, rebuilt: &[String]) -> Vec<String> {
    let rebuilt = rebuilt.iter().map(|line| format!("build: {line}"));
    if counts.compiles == 0 {
        return rebuilt.collect();
    }
    let all = crates(counts.compiles);
    let mut first = match counts.mode {
        Mode::Observe => {
            let mut line = format!(
                "build: observing: {} of {all} would have been reused",
                counts.found
            );
            if counts.differed > 0 {
                line.push_str(&format!(
                    ", {} differed from what a reuse would have given",
                    counts.differed
                ));
            }
            line
        }
        Mode::On | Mode::Off if counts.reused == 0 => {
            format!("build: {} built, none reused", crates(counts.built()))
        }
        Mode::On | Mode::Off => {
            let mut line = format!("build: {} of {all} reused", counts.reused);
            line.push_str(&from_where(counts.downloaded, counts.baked));
            if counts.built() > 0 {
                line.push_str(&format!(", {} built", counts.built()));
            }
            line
        }
    };
    if counts.mode != Mode::Observe && counts.saved_ms >= 1_000 {
        first.push_str(&format!(", saving about {}", took(counts.saved_ms)));
    }
    let mut lines = vec![first];
    lines.extend(rebuilt);
    lines.extend(
        counts
            .notes
            .iter()
            .take(MAX_NOTES)
            .map(|note| format!("build: {note}")),
    );
    if counts.low_space > 0 {
        let what = if counts.built_and_stored == 0 {
            "nothing was stored".to_string()
        } else if counts.low_space == 1 {
            "1 crate was not stored".to_string()
        } else {
            format!("{} crates were not stored", counts.low_space)
        };
        lines.push(format!(
            "build: {what} because the disk has less than {} GB free",
            counts.min_free_gb
        ));
    }
    lines
}

/// ` (2 from the shared store, 3 from the baked store)`, or what of it applies, or nothing.
fn from_where(downloaded: u64, baked: u64) -> String {
    match (downloaded, baked) {
        (0, 0) => String::new(),
        (d, 0) => format!(" ({d} from the shared store)"),
        (0, b) => format!(" ({b} from the baked store)"),
        (d, b) => format!(" ({d} from the shared store, {b} from the baked store)"),
    }
}

/// `1 crate`, `41 crates`.
fn crates(n: u64) -> String {
    if n == 1 {
        "1 crate".to_string()
    } else {
        format!("{n} crates")
    }
}

/// A length of time, rounded for people: `0.4s`, `41s`, `3m 12s`, `1h 4m`. The same form as the
/// time a skipped check saved (`took` in the hushrun crate's `cmd/wrap.rs`).
fn took(ms: u64) -> String {
    let s = ms / 1000;
    match ms {
        0..1_000 => format!("{:.1}s", ms as f64 / 1000.0),
        1_000..60_000 => format!("{s}s"),
        60_000..3_600_000 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3_600, s / 60 % 60),
    }
}

/// The store's folder (B-13): `HUSHRUN_BUILD_DIR`, or `build` under hushrun's cache directory.
pub fn store_dir(cache_dir: Option<&Path>, overrides: &Overrides) -> Option<PathBuf> {
    match &overrides.dir {
        Some(dir) => Some(dir.clone()),
        None => cache_dir.map(|dir| dir.join("build")),
    }
}

/// Starts a session, or says why there is none. Never in CI: there a session begins only in the
/// job's own store, with [`begin_in_ci`].
pub fn begin(start: &Start<'_>) -> Result<Session, NotBegun> {
    if cfg!(windows) {
        return Err(NotBegun::Windows);
    }
    let overrides =
        Overrides::from(start.env).map_err(|err| NotBegun::Unusable(err.to_string()))?;
    let settings: Settings = settings::load(start.settings)
        .map_err(|err| NotBegun::Unusable(err.to_string()))?
        .with(&overrides);
    if settings.mode == Mode::Off {
        return Err(NotBegun::Off);
    }
    if start.in_ci {
        return Err(NotBegun::Ci);
    }
    if (start.env)(RUSTC_WRAPPER).is_some_and(|value| !value.is_empty()) {
        return Err(NotBegun::WrapperSet);
    }
    let Some(store) = store_dir(start.cache_dir, &overrides) else {
        return Err(NotBegun::NoCache);
    };
    begin_at(start, settings, store)
}

/// The free-space floor of a store in CI, in gigabytes (B-31): a runner's disk is small, and the
/// store goes with the job.
pub const CI_MIN_FREE_GB: u32 = 2;

/// B-2, B-31: starts a CI job's session in `store`, a folder under the job's own temporary
/// folder that goes with the job, for a job whose org shares build blocks. No person's settings
/// file is read there: reuse is on unless `HUSHRUN_BUILD` says otherwise, the floor is
/// [`CI_MIN_FREE_GB`], and `HUSHRUN_BUILD_DIR` is not followed, so nothing outlives the job.
pub fn begin_in_ci(start: &Start<'_>, store: &Path) -> Result<Session, NotBegun> {
    begin_without_settings(start, store, Mode::On, CI_MIN_FREE_GB)
}

/// B-72: starts a session in `store`, a folder of the caller's own (a bake's dependencies-only
/// build): no person's settings file is read, reuse is on unless `HUSHRUN_BUILD` says otherwise,
/// and the floor is `min_free_gb`.
pub fn begin_isolated(
    start: &Start<'_>,
    store: &Path,
    min_free_gb: u32,
) -> Result<Session, NotBegun> {
    begin_without_settings(start, store, Mode::On, min_free_gb)
}

/// B-25, B-36: starts a session in `store` on a machine where no person is signed in (a
/// sandbox): reuse is on unless the machine's settings file says `off`
/// ([`settings::sandbox_mode`]), and `HUSHRUN_BUILD` sets this command's mode over either.
pub fn begin_in_sandbox(start: &Start<'_>, store: &Path) -> Result<Session, NotBegun> {
    let mode = settings::sandbox_mode(start.settings)
        .map_err(|err| NotBegun::Unusable(err.to_string()))?;
    begin_without_settings(start, store, mode, settings::MIN_FREE_GB)
}

/// A session whose settings are the defaults but for `mode` and the floor, and the command's
/// variables.
fn begin_without_settings(
    start: &Start<'_>,
    store: &Path,
    mode: Mode,
    min_free_gb: u32,
) -> Result<Session, NotBegun> {
    if cfg!(windows) {
        return Err(NotBegun::Windows);
    }
    let overrides =
        Overrides::from(start.env).map_err(|err| NotBegun::Unusable(err.to_string()))?;
    let settings = Settings {
        mode,
        min_free_gb,
        ..Settings::default()
    }
    .with(&overrides);
    if settings.mode == Mode::Off {
        return Err(NotBegun::Off);
    }
    if (start.env)(RUSTC_WRAPPER).is_some_and(|value| !value.is_empty()) {
        return Err(NotBegun::WrapperSet);
    }
    begin_at(start, settings, store.to_path_buf())
}

fn begin_at(start: &Start<'_>, settings: Settings, store: PathBuf) -> Result<Session, NotBegun> {
    let millis = start
        .now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    let folder = own_folder(&store.join(SESSIONS), millis, std::process::id())
        .and_then(|folder| private(&folder).map(|()| folder))
        .map_err(|err| NotBegun::Unusable(format!("{}: {err}", store.display())))?;
    let link = link_to(&store, start.exe)
        .map_err(|err| NotBegun::Unusable(format!("{}: {err}", store.display())))?;
    let current = Current {
        mode: settings.mode,
        store,
        keep_days: settings.keep_days,
        never_store: settings.never_store,
        env: settings.env,
        repository_env: start.repository_env.to_vec(),
        min_free_gb: settings.min_free_gb,
        programs_setting: settings.programs,
        repository: start.repository.map(str::to_string),
        started_ms: millis,
        shared: shared_folders(start.env),
        cargo_home: under_home(start.env, "CARGO_HOME", ".cargo"),
        rustup_home: under_home(start.env, "RUSTUP_HOME", ".rustup"),
        home: (start.env)("HOME")
            .filter(|value| !value.is_empty())
            .and_then(|value| value.into_string().ok()),
        key_log: (start.env)(KEY_LOG)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute()),
        audit: Audit::from_var(
            (start.env)(crate::audit::VAR)
                .as_deref()
                .and_then(OsStr::to_str),
        ),
        folder,
        share: Shared::Off,
        share_setting: settings.share,
        cc: None,
        gradle: None,
        maven: None,
        tsc: None,
        lower: None,
        stub: None,
    };
    write_current(&current)
        .map_err(|err| NotBegun::Unusable(format!("{}: {err}", current.folder.display())))?;
    Ok(Session {
        current,
        link,
        rebuilt: Vec::new(),
        more: Vec::new(),
        go: None,
        go_asked: false,
        xcode: None,
        rustc_left_out: false,
        cc_env: Vec::new(),
        cc_programs: None,
        cc_maps_setting: settings.cc_maps,
        slot: None,
        tools: Vec::new(),
        tsc_env: Vec::new(),
    })
}

/// Makes a session's folder under `sessions`, one no other session has: `<millis>-<pid>`, or
/// with `-<n>` after it when that name is taken. The folder is made, never adopted: two runs
/// that begin in the same millisecond with the same process id (two containers on one store,
/// each with its own process ids) would otherwise share one `session.json`, and each wrapper
/// would read the cargo home, the home folder and the mode of whichever run wrote it last.
fn own_folder(sessions: &Path, millis: u64, pid: u32) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(sessions)?;
    let mut name = format!("{millis}-{pid}");
    for n in 1u32.. {
        let folder = sessions.join(&name);
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                name = format!("{millis}-{pid}-{n}");
            }
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("no free session folder"))
}

/// Writes `session.json` whole: a temporary file in the session's folder, then a rename, so a
/// wrapper never reads half of it.
fn write_current(current: &Current) -> std::io::Result<()> {
    let file = current.folder.join(SESSION_FILE);
    let temp = current
        .folder
        .join(format!(".{SESSION_FILE}.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec(current).map_err(std::io::Error::other)?;
    std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, &file))
}

/// Makes `folder` this user's alone (mode 0700): the relay's socket lives in it (B-26).
#[cfg(unix)]
fn private(folder: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn private(_folder: &Path) -> std::io::Result<()> {
    Ok(())
}

impl Session {
    /// Names the run's relay in `session.json` (B-26), before COMMAND starts, so every wrapper
    /// of the run finds it. The file is rewritten whole.
    pub fn set_share(&mut self, share: Share) -> std::io::Result<()> {
        self.current.share = Shared::Relay(share);
        write_current(&self.current)
    }

    /// Records in `session.json`, before COMMAND starts, the baked store the run reads (B-63,
    /// B-64), as [`crate::baked::state`] said; a state the run ignores leaves none, and its line
    /// goes into the run's summary.
    pub fn set_lower(&mut self, state: &crate::baked::State) -> std::io::Result<()> {
        for line in state.lines() {
            self.add_line(line);
        }
        // B-64: the rows the run may open, as the state read them, go into the session's index,
        // which every wrapper reads instead of `baked.json`.
        let mut reading = Reading::of(state);
        if let Some(reading) = &mut reading {
            let lower = reading.lower_mut();
            let index = self.current.folder.join(crate::baked::INDEX);
            crate::baked::write_index(&index, &lower.openable)?;
            lower.index = Some(index);
        }
        self.current.lower = reading;
        write_current(&self.current)
    }

    /// B-72: records in `session.json`, before COMMAND starts, that this run compiles the
    /// repository's own crates as stubs, and writes the stub sources into the session's folder.
    pub fn set_stub(&mut self, stub: Stub) -> std::io::Result<()> {
        let folder = &self.current.folder;
        std::fs::write(folder.join(STUB_LIB), "#![allow(warnings)]\n")?;
        std::fs::write(
            folder.join(STUB_MAIN),
            "#![allow(warnings)]\nfn main() {}\n",
        )?;
        self.current.stub = Some(stub);
        write_current(&self.current)
    }

    /// Names the manifest the run leaves in the store when it ends (B-63): its check's.
    pub fn set_manifest_slot(&mut self, slot: crate::manifests::Slot) {
        self.slot = Some(slot);
    }

    /// Records in `session.json`, before COMMAND starts, that the run would share and its
    /// backend couldn't be reached (B-34). A run with a relay keeps it: the relay is what knows
    /// whether its server can be reached.
    pub fn set_unreached(&mut self) -> std::io::Result<()> {
        if self.current.share != Shared::Off {
            return Ok(());
        }
        self.current.share = Shared::Unreached;
        write_current(&self.current)
    }

    /// Makes, before COMMAND starts, the sysroot without sources the compiles of this run will
    /// be given for the toolchain at `sysroot`, when it has sources (B-9, B-17), so that the
    /// wrappers find it whole and only read it. `None` when it has none or the view can't be
    /// made: a wrapper then makes it, or passes its compile through.
    pub fn hide_sources(&self, sysroot: &Path) -> Option<PathBuf> {
        let used = UNIX_EPOCH + Duration::from_millis(self.current.started_ms);
        let dir = self
            .current
            .store
            .join(crate::compiler::MEMOS)
            .join(crate::compiler::SYSROOTS);
        crate::compiler::hidden_sources(sysroot, &dir, Some(used))
            .ok()
            .flatten()
    }

    /// What to set on COMMAND: the wrapper for cargo, the session for the wrapper, and Go's
    /// cache program when [`Session::set_go`] gave one (B-39): in a run that shares or reads a
    /// baked store; elsewhere the program would only do what Go's own cache does.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        let mut env = vec![(SESSION.into(), self.current.folder.clone().into_os_string())];
        if !self.rustc_left_out {
            env.insert(
                0,
                (RUSTC_WRAPPER.into(), self.link.clone().into_os_string()),
            );
        }
        if let Some(go) = &self.go {
            env.push((crate::go::GOCACHEPROG.into(), go.value.clone().into()));
        }
        env.extend(self.cc_env.iter().cloned());
        for tool in &self.tools {
            env.extend(tool.env.iter().cloned());
        }
        env.extend(self.tsc_env.iter().cloned());
        if let Some(xcconfig) = &self.xcode {
            env.push((
                crate::xcode::XCCONFIG_VAR.into(),
                xcconfig.clone().into_os_string(),
            ));
        }
        env
    }

    /// Gives COMMAND the xcconfig file that points Xcode's compilation cache at the run's
    /// service (B-84).
    pub fn set_xcode(&mut self, xcconfig: PathBuf) {
        self.xcode = Some(xcconfig);
    }

    /// The xcconfig file COMMAND gets, when the run serves Xcode's cache.
    pub fn xcode(&self) -> Option<&Path> {
        self.xcode.as_deref()
    }

    /// B-77: serves Gradle hushrun's cache for this run, on `bound`, and gives COMMAND what
    /// loads it: `HUSHRUN_GRADLE` naming `script` unless `env` has one already (ADR 0087's
    /// adapter sets its own, whose script ends with the same text), the cache's URL and its
    /// token. `run` is what the cache searches entries for (B-79), written into `session.json`
    /// before COMMAND starts. Called once the session shares (B-26): the cache asks its relay.
    /// The URL Gradle is given.
    pub fn set_gradle(
        &mut self,
        run: crate::gradle::GradleRun,
        script: &Path,
        bound: crate::cache::endpoint::Bound,
        env: &dyn Fn(&str) -> Option<OsString>,
    ) -> std::io::Result<String> {
        self.current.gradle = Some(run);
        write_current(&self.current)?;
        let url = bound.url();
        let token = bound.endpoint.token.clone();
        let cache = crate::gradle::cache::Cache::new(self.current.clone(), &token);
        let running = crate::cache::service::start(bound.listener, cache)?;
        let mut gradle_env: Vec<(OsString, OsString)> = Vec::new();
        if env(crate::gradle::SCRIPT_VARIABLE).is_none_or(|v| v.is_empty()) {
            gradle_env.push((
                crate::gradle::SCRIPT_VARIABLE.into(),
                script.as_os_str().to_os_string(),
            ));
        }
        gradle_env.push((crate::gradle::CACHE_VARIABLE.into(), url.clone().into()));
        gradle_env.push((crate::gradle::TOKEN_VARIABLE.into(), token.into()));
        self.set_tool(ToolService {
            kind: crate::model::Kind::Gradle,
            env: gradle_env,
            keeps_nothing: None,
            _running: running,
        });
        Ok(url)
    }

    /// The service of the tool cache of `kind`, in place of any the run had.
    fn set_tool(&mut self, service: ToolService) {
        self.tools.retain(|tool| tool.kind != service.kind);
        self.tools.push(service);
    }

    /// Whether the run serves hushrun's cache to the build tool of `kind` (B-77, B-170).
    pub fn serves(&self, kind: crate::model::Kind) -> bool {
        self.tools.iter().any(|tool| tool.kind == kind)
    }

    /// B-170: serves Maven's build cache extension hushrun's cache for this run, on a port and
    /// under a token of the run's own, and gives COMMAND `MAVEN_OPTS`: what `env` has, then the
    /// extension's properties for it. `run` is what the cache searches entries for (B-173),
    /// written into `session.json` before COMMAND starts. Called once the session shares (B-26):
    /// the cache asks its relay. The URL Maven is given.
    pub fn set_maven(
        &mut self,
        run: crate::maven::MavenRun,
        env: &dyn Fn(&str) -> Option<OsString>,
    ) -> std::io::Result<String> {
        let (listener, token) = crate::cache::endpoint::for_run()?;
        let url = crate::cache::endpoint::run_url(&listener, &token)?;
        self.current.maven = Some(run);
        write_current(&self.current)?;
        let cache = crate::maven::cache::Cache::new(self.current.clone(), &token);
        let keeps_nothing = Some(cache.skips_tests());
        let running = crate::cache::service::start(listener, cache)?;
        let mut opts = env(crate::maven::OPTS_VARIABLE)
            .map(|v| v.to_string_lossy().trim().to_string())
            .unwrap_or_default();
        if !opts.is_empty() {
            opts.push(' ');
        }
        opts.push_str(&crate::maven::properties(&url));
        self.set_tool(ToolService {
            kind: crate::model::Kind::Maven,
            env: vec![(crate::maven::OPTS_VARIABLE.into(), opts.into())],
            keeps_nothing,
            _running: running,
        });
        Ok(url)
    }

    /// B-175: Maven skips tests in this run, by `property` (`skipTests` or `maven.test.skip`) in
    /// `place` (COMMAND, `MAVEN_OPTS`, a file of `.mvn`): the cache keeps none of the modules it
    /// builds, and the run says so once. Nothing when the run doesn't serve Maven.
    pub fn maven_skips_tests(&mut self, property: &str, place: &str) {
        let Some(skips) = self
            .tools
            .iter()
            .find(|tool| tool.kind == crate::model::Kind::Maven)
            .and_then(|tool| tool.keeps_nothing.as_ref())
        else {
            return;
        };
        if let Ok(mut skips) = skips.lock() {
            *skips = Some(property.to_string());
        }
        self.add_line(format!(
            "build: maven: {place} sets {property}, so the modules this run builds aren't shared: another build would skip tests that never ran"
        ));
    }

    /// Gives COMMAND the C and C++ launcher (B-50): `checkout` is the repository's folder as the
    /// run found it (its given form; the resolved one is added), `maps` whether the repository
    /// lets the launcher map it (`build: cc: maps:`; the person's setting can turn it off too),
    /// `env` the run's environment, where the person's own
    /// launcher would be. Each of `CMAKE_C_COMPILER_LAUNCHER` and `CMAKE_CXX_COMPILER_LAUNCHER`
    /// gets [`crate::cc::INDIRECTION`] unless the person set it; `HUSHRUN_CC` gets the link
    /// `hushrun-cc`. Written into `session.json` before COMMAND starts. Nothing on Windows.
    pub fn set_cc(
        &mut self,
        checkout: &Path,
        maps: bool,
        env: &dyn Fn(&str) -> Option<OsString>,
    ) -> std::io::Result<()> {
        if cfg!(windows) {
            return Ok(());
        }
        let link = self.launcher_link()?;
        let given = checkout.to_string_lossy().into_owned();
        let mut roots = vec![given.clone()];
        if let Some(resolved) = crate::roots::resolve_on_disk(&given)
            && resolved != given
        {
            roots.push(resolved);
        }
        let maps = maps && self.cc_maps_setting;
        self.current.cc = Some(crate::cc::CcRun { roots, maps });
        write_current(&self.current)?;
        let mut cc_env: Vec<(OsString, OsString)> = crate::cc::CMAKE_LAUNCHERS
            .iter()
            .filter(|name| env(name).is_none_or(|v| v.is_empty()))
            .map(|name| ((*name).into(), crate::cc::INDIRECTION.into()))
            .collect();
        cc_env.push((crate::cc::HUSHRUN_CC.into(), link.into_os_string()));
        // B-230, B-232: GNU Make's commands find the compiler programs first on PATH.
        let programs = crate::cc::programs::make(&self.current.folder)?;
        let hook = programs.join(crate::cc::programs::HOOK);
        match crate::cc::programs::makefiles(
            &hook,
            env(crate::cc::programs::MAKEFILES).as_deref(),
        ) {
            Some(value) => cc_env.push((crate::cc::programs::MAKEFILES.into(), value)),
            None => self.more.push(format!(
                "build: C and C++ objects of GNU Make builds aren't reused in this run: the run's folder, {}, has a space or a `:` in its path",
                programs.display()
            )),
        }
        self.cc_programs = Some(programs);
        self.cc_env = cc_env;
        Ok(())
    }

    /// B-235: what COMMAND `words`, started in `cwd`, gets beside [`Session::env`]: `PATH` with
    /// the compiler programs first when it is `ninja …` itself on a `build.ninja` neither CMake
    /// nor Meson wrote. `path` is `PATH` as COMMAND would have it.
    pub fn cc_command_env(
        &self,
        words: &[String],
        cwd: &Path,
        path: Option<&std::ffi::OsStr>,
    ) -> Vec<(OsString, OsString)> {
        let Some(programs) = &self.cc_programs else {
            return Vec::new();
        };
        if crate::cc::programs::ninja_folder(words, cwd).is_none() {
            return Vec::new();
        }
        crate::cc::programs::path_with(programs, path)
            .map(|path| vec![(OsString::from("PATH"), path)])
            .unwrap_or_default()
    }

    /// Adds a variable COMMAND gets with the launcher's, in place of one of the same name: hushrun's
    /// own `meson` first on `PATH` for a Meson repository (B-236).
    pub fn add_cc_env(&mut self, name: OsString, value: OsString) {
        self.cc_env.retain(|(n, _)| *n != name);
        self.cc_env.push((name, value));
    }

    /// The folder of the compiler programs (B-231), when the run gives COMMAND the launcher.
    pub fn cc_programs(&self) -> Option<&Path> {
        self.cc_programs.as_deref()
    }

    /// Gives COMMAND the TypeScript hook (B-100): `checkout` is the repository's folder as the
    /// run found it (its given form; the resolved one is added), `env` the run's environment,
    /// where COMMAND's `NODE_OPTIONS` would be. The hook is written into the store's folder,
    /// `NODE_OPTIONS` gets `--require <hook>` after what it had, and `HUSHRUN_TSC` the link
    /// `hushrun-tsc`. Written into `session.json` before COMMAND starts. Nothing on Windows.
    pub fn set_tsc(
        &mut self,
        checkout: &Path,
        env: &dyn Fn(&str) -> Option<OsString>,
    ) -> std::io::Result<()> {
        if cfg!(windows) {
            return Ok(());
        }
        let link = self.sibling_link(crate::tsc::PROGRAM_NAME)?;
        let hook = crate::tsc::install_preload(&self.current.store)?;
        let options =
            crate::tsc::node_options(env("NODE_OPTIONS").as_deref(), &hook).ok_or_else(|| {
                std::io::Error::other(format!(
                    "the hook's path, {}, has a quote, which NODE_OPTIONS can't hold",
                    hook.display()
                ))
            })?;
        let given = checkout.to_string_lossy().into_owned();
        let mut roots = vec![given.clone()];
        if let Some(resolved) = crate::roots::resolve_on_disk(&given)
            && resolved != given
        {
            roots.push(resolved);
        }
        self.current.tsc = Some(crate::tsc::TscRun { roots });
        write_current(&self.current)?;
        self.tsc_env = vec![
            ("NODE_OPTIONS".into(), options),
            (crate::tsc::HUSHRUN_TSC.into(), link.into_os_string()),
        ];
        Ok(())
    }

    /// Whether the run gives COMMAND the C and C++ launcher, and for which languages.
    pub fn cc_launchers(&self) -> Vec<&str> {
        crate::cc::CMAKE_LAUNCHERS
            .iter()
            .copied()
            .filter(|name| self.cc_env.iter().any(|(n, _)| n == name))
            .collect()
    }

    /// The link the launcher variables lead to, `hushrun-cc` beside the wrapper's link and
    /// pointing at the same binary (B-50), made when first asked.
    pub fn launcher_link(&self) -> std::io::Result<PathBuf> {
        self.sibling_link(crate::cc::LAUNCHER_NAME)
    }

    fn sibling_link(&self, name: &str) -> std::io::Result<PathBuf> {
        let dir = self
            .link
            .parent()
            .ok_or_else(|| std::io::Error::other("the wrapper's link has no folder"))?;
        let exe = std::fs::read_link(&self.link)?;
        let link = dir.join(name);
        if std::fs::read_link(&link).is_ok_and(|target| target == exe) {
            return Ok(link);
        }
        let temp = dir.join(crate::baked::unique(&format!(".{name}.tmp")));
        symlink(&exe, &temp)?;
        std::fs::rename(&temp, &link)?;
        Ok(link)
    }

    /// The link Go starts as its cache program, `hushrun-gocache` beside the wrapper's link and
    /// pointing at the same binary (B-39), made when first asked.
    pub fn program_link(&self) -> std::io::Result<PathBuf> {
        self.sibling_link(crate::go::PROGRAM_NAME)
    }

    /// Leaves hushrun's `RUSTC_WRAPPER` out of COMMAND's environment, for a run whose own
    /// wrapper (sccache) is set: the session then serves Go's cache program alone (B-2, B-39).
    pub fn leave_out_rustc(&mut self) {
        self.rustc_left_out = true;
    }

    /// Whether hushrun's `RUSTC_WRAPPER` is left out ([`Session::leave_out_rustc`]).
    pub fn rustc_left_out(&self) -> bool {
        self.rustc_left_out
    }

    /// Gives COMMAND Go's cache program (B-39), as [`crate::go::probe::probe`] decided it, or
    /// none; either way the run has asked, and asks no more ([`Session::go_asked`]).
    pub fn set_go(&mut self, go: Option<crate::go::probe::Go>) {
        self.go = go;
        self.go_asked = true;
    }

    /// Whether the run asked for Go's cache program already.
    pub fn go_asked(&self) -> bool {
        self.go_asked
    }

    /// The Go cache program the run sets, when it sets one: in a Go module's session that
    /// shares or reads a baked store (B-39), which keeps its portable archives in the store
    /// (B-42).
    pub fn go(&self) -> Option<&crate::go::probe::Go> {
        self.go.as_ref()
    }

    /// Adds a line to the run's summary, once: what its relay said (B-26).
    pub fn add_line(&mut self, line: String) {
        if !self.more.contains(&line) {
            self.more.push(line);
        }
    }

    /// The mode every compile of this run sees.
    pub fn mode(&self) -> Mode {
        self.current.mode
    }

    /// What the wrapper reads for this run.
    pub fn current(&self) -> &Current {
        &self.current
    }

    /// Before COMMAND starts (B-23): removes the outputs of every crate the wrapper saw compiled
    /// in the target folders of `folder` (the folder `hushrun run` runs in) whose inputs changed
    /// in a way cargo can't see, so that cargo builds them. What it removed goes into the run's
    /// summary. It stops at `deadline`, leaving the rest unchecked.
    pub fn check(&mut self, folder: &Path, deadline: Option<std::time::Instant>) -> Checked {
        let checked = crate::unit::check(&Store::new(&self.current.store), folder, deadline);
        self.rebuilt = checked.lines();
        checked
    }

    /// After COMMAND: reads the log, removes the session's folder and, at most once a day,
    /// sweeps the store (B-14). Then it removes what other runs left: sessions untouched for
    /// more than two days (a run that was killed) and wrapper links whose binary is gone. Nothing
    /// here fails the run: every step tolerates a missing file and another process doing the
    /// same at once.
    pub fn finish(self, now: SystemTime) -> Summary {
        let current = &self.current;
        let entries = current.entries();
        let mut summary =
            Summary::of_run(current.mode, current.min_free_gb, &entries, &self.rebuilt);
        // B-63: the check's manifest stays in the store, for `hushrun build bake`.
        if let Some(slot) = &self.slot {
            match crate::manifests::save(
                &Store::new(&current.store),
                current.repository.as_deref(),
                slot,
                &entries,
                now,
                current.keep_days,
            ) {
                Ok(None) => {}
                Ok(Some(line)) => summary.lines.push(line),
                Err(err) => summary
                    .lines
                    .push(format!("build: this check's manifest wasn't saved: {err}")),
            }
        }
        // B-16: whoever turned the audit off turned a guard off; the run says so every time.
        if current.audit == Audit::Never {
            summary.lines.push(AUDIT_OFF.to_string());
        }
        // B-24: with a shared store, what the search for unkeyed values held back, by name.
        if current.share.relay().is_some() && summary.counts.unkeyed > 0 {
            let n = summary.counts.unkeyed;
            summary.lines.push(format!(
                "build: {} {} on this machine: {} the value of {}, which no key holds (a project names such a variable under build: env:)",
                crates(n),
                if n == 1 { "stays" } else { "stay" },
                if n == 1 { "its outputs hold" } else { "their outputs hold" },
                summary.counts.unkeyed_variables.join(", ")
            ));
        }
        summary.lines.extend(self.more.iter().cloned());
        let _ = remove_dir(&current.folder);
        let _ = crate::sweep::sweep(&Store::new(&current.store), now, current.keep_days);
        remove_stale_sessions(&current.store, now);
        remove_dangling_links(&current.store);
        summary
    }
}

/// Removes a folder and what it holds. Already gone is fine.
fn remove_dir(path: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Removes the sessions of runs that were killed: folders under `sessions/` where nothing changed
/// for more than two days. A session's log is written to as long as its run compiles, so a long
/// run is not taken for a killed one.
fn remove_stale_sessions(store: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(store.join(SESSIONS)) else {
        return;
    };
    for entry in entries.flatten() {
        let folder = entry.path();
        let Ok(meta) = fs::symlink_metadata(&folder) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let changed = [SESSION_FILE, LOG_FILE]
            .iter()
            .filter_map(|name| {
                fs::metadata(folder.join(name))
                    .and_then(|m| m.modified())
                    .ok()
            })
            .chain(meta.modified().ok())
            .max();
        // A date after `now` is a clock set back: not stale.
        if changed.is_some_and(|changed| {
            now.duration_since(changed)
                .is_ok_and(|age| age > STALE_SESSION)
        }) {
            let _ = remove_dir(&folder);
        }
    }
}

/// Removes the wrapper links whose binary no longer exists (an uninstalled or rebuilt hushrun),
/// each with its folder under `bin/`. A folder with anything else in it stays.
fn remove_dangling_links(store: &Path) {
    let Ok(entries) = fs::read_dir(store.join(BIN)) else {
        return;
    };
    for entry in entries.flatten() {
        let folder = entry.path();
        let link = folder.join(crate::WRAPPER_NAME);
        let is_link = fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink());
        let dangling = matches!(
            fs::metadata(&link),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound
        );
        if is_link && dangling {
            let _ = fs::remove_file(&link);
            // Go's cache program's link and the C launcher's point at the same binary.
            for name in [
                crate::go::PROGRAM_NAME,
                crate::cc::LAUNCHER_NAME,
                crate::tsc::PROGRAM_NAME,
            ] {
                let program = folder.join(name);
                if fs::symlink_metadata(&program).is_ok_and(|meta| meta.file_type().is_symlink()) {
                    let _ = fs::remove_file(&program);
                }
            }
            let _ = fs::remove_dir(&folder);
        }
    }
}

/// The link cargo runs (B-2): `bin/<a name for this binary>/hushrun-rustc`, pointing at the
/// binary. Each binary has its own, so two versions of hushrun running at once don't take each
/// other's.
#[cfg(unix)]
fn link_to(store: &Path, exe: &Path) -> std::io::Result<PathBuf> {
    let meta = std::fs::metadata(exe)?;
    let stat = hushrun_digest::cache::Stat::of(&meta);
    let name = hushrun_core::Digest::of(
        format!("{}\n{}\n{}", exe.display(), stat.size, stat.mtime_ns).as_bytes(),
    )
    .hex();
    let dir = store.join(BIN).join(&name[..16]);
    let link = dir.join(crate::WRAPPER_NAME);
    if std::fs::read_link(&link).is_ok_and(|target| target == exe) {
        return Ok(link);
    }
    std::fs::create_dir_all(&dir)?;
    let temp = dir.join(crate::baked::unique(&format!(
        ".{}.tmp",
        crate::WRAPPER_NAME
    )));
    std::os::unix::fs::symlink(exe, &temp)?;
    std::fs::rename(&temp, &link)?;
    Ok(link)
}

#[cfg(not(unix))]
fn link_to(_store: &Path, _exe: &Path) -> std::io::Result<PathBuf> {
    Err(std::io::Error::other("no wrapper link on this platform"))
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("no links on this platform"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    // covers: B-83, B-177, B-210
    #[test]
    fn one_set_of_counts_says_gradle_s_and_maven_s_lines_word_for_word() {
        let entry = |name: &str, outcome: Outcome, downloaded: bool| Entry {
            outcome,
            class: Some(Class::Library),
            crate_name: name.to_string(),
            package: String::new(),
            rustc_ms: 0,
            saved_ms: if outcome == Outcome::Reused { 1_500 } else { 0 },
            stored_bytes: 0,
            found: false,
            differed: false,
            note: None,
            portable: None,
            step: None,
            key: None,
            downloaded,
            baked: false,
            go: None,
            cc: None,
            xcode: None,
            gradle: None,
            maven: None,
            tsc: None,
        };
        let mut counts = ToolCounts::default();
        counts.add(&entry(":a", Outcome::Reused, true), None);
        counts.add(&entry(":b", Outcome::BuiltAndStored, false), None);
        for name in [":c", ":d", ":e", ":f"] {
            counts.add(
                &entry(name, Outcome::BuiltAndStored, false),
                Some("its outputs hold the checkout's folder"),
            );
        }
        assert_eq!(
            counts.lines(&Units::GRADLE),
            [
                "build: gradle: 6 tasks: 1 from hushrun's cache (1 from the shared store), 5 run and stored, saving about 1s",
                "build: gradle: 4 tasks run here stay on this machine (4: its outputs hold the checkout's folder): :c, :d, :e and 1 more",
            ]
        );
        let mut counts = ToolCounts::default();
        counts.add(
            &entry("core", Outcome::BuiltAndStored, false),
            Some("its files hold the checkout's folder"),
        );
        assert_eq!(
            counts.lines(&Units::MAVEN),
            [
                "build: maven: 1 module: 1 built and stored",
                "build: maven: 1 module built here stays on this machine (1: its files hold the checkout's folder)",
            ]
        );
        assert!(ToolCounts::default().lines(&Units::GRADLE).is_empty());
    }

    // covers: B-72
    #[test]
    fn a_dependencies_only_run_stubs_the_repository_s_own_crates_alone() {
        let stub = Stub {
            packages: vec!["app".into(), "core".into()],
            under: PathBuf::from("/work/repo"),
        };
        let folder = Path::new("/store/sessions/1");
        let args = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        let vars = |pkg: &'static str, dir: &'static str| {
            move |name: &str| match name {
                "CARGO_PKG_NAME" => Some(OsString::from(pkg)),
                "CARGO_MANIFEST_DIR" => Some(OsString::from(dir)),
                _ => None,
            }
        };
        let lib = args(&[
            "--crate-name",
            "core",
            "--edition=2021",
            "core/src/lib.rs",
            "--crate-type",
            "lib",
        ]);
        let out = stub
            .args(folder, &lib, &vars("core", "/work/repo/core"))
            .unwrap();
        assert_eq!(out[3], folder.join(STUB_LIB));
        assert_eq!(out.len(), lib.len());
        // A program and a build script get a main; a test harness makes its own.
        let bin = args(&["--crate-name", "app", "src/main.rs", "--crate-type", "bin"]);
        let out = stub.args(folder, &bin, &vars("app", "/work/repo")).unwrap();
        assert_eq!(out[2], folder.join(STUB_MAIN));
        let script = args(&[
            "--crate-name",
            "build_script_build",
            "build.rs",
            "--crate-type=bin",
        ]);
        let out = stub
            .args(folder, &script, &vars("app", "/work/repo"))
            .unwrap();
        assert_eq!(out[2], folder.join(STUB_MAIN));
        let test = args(&[
            "--crate-name",
            "app",
            "src/main.rs",
            "--test",
            "--crate-type",
            "bin",
        ]);
        let out = stub
            .args(folder, &test, &vars("app", "/work/repo"))
            .unwrap();
        assert_eq!(out[2], folder.join(STUB_LIB));
        // Third-party code, a path package outside the checkout, or no single source: as asked.
        let dep = args(&["--crate-name", "serde", "src/lib.rs"]);
        assert!(
            stub.args(folder, &dep, &vars("serde", "/home/.cargo/registry/serde"))
                .is_none()
        );
        assert!(
            stub.args(folder, &lib, &vars("core", "/elsewhere/core"))
                .is_none()
        );
        let two = args(&["--crate-name", "core", "a.rs", "b.rs"]);
        assert!(
            stub.args(folder, &two, &vars("core", "/work/repo/core"))
                .is_none()
        );
    }

    fn start<'a>(
        dir: &'a Path,
        settings: &'a Path,
        exe: &'a Path,
        env: &'a dyn Fn(&str) -> Option<OsString>,
    ) -> Start<'a> {
        Start {
            settings,
            cache_dir: Some(dir),
            exe,
            in_ci: false,
            repository: None,
            repository_env: &[],
            env,
            now: UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000),
        }
    }

    // covers: B-1, B-2, B-21
    #[test]
    fn a_run_has_a_session_only_when_reuse_is_on_here() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("build.json");
        let exe = dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        let none = |_: &str| None;

        // Off until the person turns it on.
        assert_eq!(
            begin(&start(dir.path(), &settings, &exe, &none)).unwrap_err(),
            NotBegun::Off
        );

        settings::update(&settings, |s| s.mode = Mode::Observe).unwrap();
        let session = begin(&start(dir.path(), &settings, &exe, &none)).unwrap();
        assert_eq!(session.mode(), Mode::Observe);
        let env = session.env();
        let wrapper = PathBuf::from(&env[0].1);
        assert_eq!(env[0].0, RUSTC_WRAPPER);
        assert!(crate::wrapper::called_as_wrapper(wrapper.as_os_str()));
        assert_eq!(std::fs::read_link(&wrapper).unwrap(), exe);
        assert_eq!(env[1].0, SESSION);
        assert!(Path::new(&env[1].1).is_dir());
        // The wrapper finds the same session through the variable, and logs to it.
        let folder = env[1].1.clone();
        let found = current(&|name| (name == SESSION).then(|| folder.clone())).unwrap();
        assert_eq!(&found, session.current());
        assert_eq!(found.mode, Mode::Observe);
        assert_eq!(found.programs_setting, ProgramsSetting::Follow);
        assert_eq!(found.programs(), Programs::Tied);
        assert!(found.shared.iter().any(|s| s == "/usr"));
        let entry = Entry {
            outcome: crate::decide::passed_through(crate::model::PassedThrough::Probe),
            class: None,
            crate_name: String::new(),
            package: String::new(),
            rustc_ms: 0,
            saved_ms: 0,
            stored_bytes: 0,
            found: false,
            differed: false,
            note: None,
            portable: None,
            step: None,
            key: None,
            downloaded: false,
            baked: false,
            go: None,
            xcode: None,
            cc: None,
            gradle: None,
            maven: None,
            tsc: None,
        };
        found.log(&entry);
        found.log(&entry);
        assert_eq!(found.entries(), vec![entry.clone(), entry]);
        session.finish(UNIX_EPOCH);

        // What happens to programs is the person's, and every compile of the run sees it.
        settings::update(&settings, |s| s.programs = ProgramsSetting::Tied).unwrap();
        let session = begin(&start(dir.path(), &settings, &exe, &none)).unwrap();
        let folder = session.env()[1].1.clone();
        let found = current(&|name| (name == SESSION).then(|| folder.clone())).unwrap();
        assert_eq!(found.programs_setting, ProgramsSetting::Tied);
        assert_eq!(found.programs(), Programs::Tied);
        assert_eq!(found.audit, Audit::OneIn);
        session.finish(UNIX_EPOCH);

        // The view of a toolchain with sources is made as the run begins, before any compile
        // (B-9), and found by the wrappers whole; a toolchain without sources has none.
        let toolchain = dir.path().join("toolchain");
        std::fs::create_dir_all(toolchain.join("lib/rustlib/x86_64-unknown-linux-gnu/lib"))
            .unwrap();
        let session = begin(&start(dir.path(), &settings, &exe, &none)).unwrap();
        assert_eq!(session.hide_sources(&toolchain), None);
        std::fs::create_dir_all(toolchain.join("lib/rustlib/src/rust/library")).unwrap();
        let view = session.hide_sources(&toolchain).unwrap();
        assert!(view.starts_with(&session.current().store));
        assert!(view.join(crate::compiler::VIEW_MARKER).is_file());
        assert!(
            view.join("lib/rustlib/x86_64-unknown-linux-gnu/lib")
                .is_dir()
        );
        assert!(!view.join("lib/rustlib/src").exists());
        session.finish(UNIX_EPOCH);

        // How many compiles the run audits is read once, as it begins (B-16).
        let every = |name: &str| (name == crate::audit::VAR).then(|| OsString::from("every"));
        let session = begin(&start(dir.path(), &settings, &exe, &every)).unwrap();
        let folder = session.env()[1].1.clone();
        let found = current(&|name| (name == SESSION).then(|| folder.clone())).unwrap();
        assert_eq!(found.audit, Audit::Every);
        assert!(
            session
                .finish(UNIX_EPOCH)
                .lines()
                .iter()
                .all(|l| !l.contains("audit"))
        );
        // Turned off, the guard it is gone for the run, and the run says so (B-16).
        let none = |name: &str| (name == crate::audit::VAR).then(|| OsString::from("none"));
        let session = begin(&start(dir.path(), &settings, &exe, &none)).unwrap();
        let lines = session.finish(UNIX_EPOCH).lines().to_vec();
        assert_eq!(
            lines,
            vec![
                "build: the audit is off for this run (HUSHRUN_BUILD_AUDIT=none): no reused block \
                 was compared with a fresh compile"
                    .to_string()
            ]
        );

        // Never in CI, and never over a wrapper the person set.
        let mut ci = start(dir.path(), &settings, &exe, &none);
        ci.in_ci = true;
        assert_eq!(begin(&ci).unwrap_err(), NotBegun::Ci);
        let set = |name: &str| (name == RUSTC_WRAPPER).then(|| OsString::from("sccache"));
        assert_eq!(
            begin(&start(dir.path(), &settings, &exe, &set)).unwrap_err(),
            NotBegun::WrapperSet
        );
        // One command can turn it off.
        let off = |name: &str| (name == "HUSHRUN_BUILD").then(|| OsString::from("off"));
        assert_eq!(
            begin(&start(dir.path(), &settings, &exe, &off)).unwrap_err(),
            NotBegun::Off
        );
    }

    // covers: B-2
    #[test]
    fn each_binary_has_its_own_link() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one");
        let two = dir.path().join("two");
        std::fs::write(&one, b"1").unwrap();
        std::fs::write(&two, b"22").unwrap();
        let a = link_to(dir.path(), &one).unwrap();
        let b = link_to(dir.path(), &two).unwrap();
        assert_ne!(a, b);
        assert_eq!(link_to(dir.path(), &one).unwrap(), a);
        assert_eq!(std::fs::read_link(&a).unwrap(), one);
        assert_eq!(std::fs::read_link(&b).unwrap(), two);
    }

    // The summary's lines.

    fn entry(outcome: Outcome) -> Entry {
        Entry {
            outcome,
            class: Some(Class::Library),
            crate_name: "c".into(),
            package: String::new(),
            rustc_ms: 0,
            saved_ms: 0,
            stored_bytes: 0,
            found: false,
            differed: false,
            note: None,
            portable: None,
            step: None,
            key: None,
            downloaded: false,
            baked: false,
            go: None,
            xcode: None,
            cc: None,
            gradle: None,
            maven: None,
            tsc: None,
        }
    }

    fn reused(saved_ms: u64) -> Entry {
        Entry {
            found: true,
            saved_ms,
            ..entry(Outcome::Reused)
        }
    }

    // covers: B-20, B-26
    #[test]
    fn the_summary_says_how_many_reuses_came_from_the_shared_store() {
        let downloaded = Entry {
            downloaded: true,
            ..reused(500)
        };
        let summary = Summary::of(
            Mode::On,
            10,
            &[downloaded.clone(), downloaded, reused(500), built()],
        );
        assert_eq!(
            summary.lines()[0],
            "build: 3 of 4 crates reused (2 from the shared store), 1 built, saving about 1s"
        );
        let summary = Summary::of(Mode::On, 10, &[reused(500), built()]);
        assert_eq!(summary.lines()[0], "build: 1 of 2 crates reused, 1 built");
    }

    fn built() -> Entry {
        Entry {
            stored_bytes: 100,
            ..entry(Outcome::BuiltAndStored)
        }
    }

    fn probe(why: PassedThrough) -> Entry {
        Entry {
            class: None,
            crate_name: String::new(),
            ..entry(Outcome::PassedThrough(why))
        }
    }

    fn noted(note: &str) -> Entry {
        Entry {
            note: Some(note.into()),
            ..entry(Outcome::BuiltNotStored(NotStored::Suspended))
        }
    }

    fn lines_of(mode: Mode, entries: &[Entry]) -> Vec<String> {
        Summary::of(mode, 10, entries).lines().to_vec()
    }

    fn many(n: usize, make: impl Fn() -> Entry) -> Vec<Entry> {
        (0..n).map(|_| make()).collect()
    }

    // covers: B-20
    #[test]
    fn nothing_compiled_says_nothing() {
        assert!(lines_of(Mode::On, &[]).is_empty());
        let probes = [
            probe(PassedThrough::Probe),
            probe(PassedThrough::NoCrateName),
        ];
        let summary = Summary::of(Mode::On, 10, &probes);
        assert!(summary.lines().is_empty());
        assert_eq!(summary.counts().compiles, 0);
        assert!(lines_of(Mode::Observe, &probes).is_empty());
    }

    // covers: B-20
    #[test]
    fn all_built_says_none_reused() {
        assert_eq!(
            lines_of(Mode::On, &many(41, built)),
            ["build: 41 crates built, none reused"]
        );
    }

    // covers: B-20
    #[test]
    fn some_reused_says_how_many_and_the_time_saved() {
        let mut entries = many(331, || reused(0));
        entries[0].saved_ms = 100_000;
        entries[1].saved_ms = 60_000;
        entries.extend(many(41, built));
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(
            summary.lines(),
            ["build: 331 of 372 crates reused, 41 built, saving about 2m 40s"]
        );
        let counts = summary.counts();
        assert_eq!(
            (counts.compiles, counts.reused, counts.built_and_stored),
            (372, 331, 41)
        );
        assert_eq!(counts.found, 331);
        assert_eq!(counts.saved_ms, 160_000);
        assert_eq!(counts.stored_bytes, 4_100);
        assert_eq!(counts.built(), 41);

        // Under a second saved: no time.
        let small = [reused(300), reused(100), built()];
        assert_eq!(
            lines_of(Mode::On, &small),
            ["build: 2 of 3 crates reused, 1 built"]
        );
        // Exactly a second is said.
        let second = [reused(1_000), built()];
        assert_eq!(
            lines_of(Mode::On, &second),
            ["build: 1 of 2 crates reused, 1 built, saving about 1s"]
        );
        // All reused: nothing built to count.
        assert_eq!(
            lines_of(Mode::On, &many(3, || reused(41_000))),
            ["build: 3 of 3 crates reused, saving about 2m 3s"]
        );
    }

    // covers: B-1, B-20
    #[test]
    fn observe_says_what_would_have_been_reused_and_what_differed() {
        let mut entries = many(2, || Entry {
            found: true,
            ..built()
        });
        entries.push(built());
        assert_eq!(
            lines_of(Mode::Observe, &entries),
            ["build: observing: 2 of 3 crates would have been reused"]
        );
        entries[1].differed = true;
        entries[1].outcome = Outcome::BuiltNotStored(NotStored::Differed);
        let summary = Summary::of(Mode::Observe, 10, &entries);
        assert_eq!(
            summary.lines(),
            [
                "build: observing: 2 of 3 crates would have been reused, 1 differed from what a \
                 reuse would have given"
            ]
        );
        assert_eq!(summary.counts().differed, 1);
        assert_eq!(summary.counts().built_not_stored, 1);
        // Nothing would have been reused.
        assert_eq!(
            lines_of(Mode::Observe, &many(4, built)),
            ["build: observing: 0 of 4 crates would have been reused"]
        );
    }

    // covers: B-20
    #[test]
    fn one_crate_is_singular() {
        assert_eq!(
            lines_of(Mode::On, &[built()]),
            ["build: 1 crate built, none reused"]
        );
        assert_eq!(
            lines_of(Mode::On, &[reused(5_000)]),
            ["build: 1 of 1 crate reused, saving about 5s"]
        );
        assert_eq!(
            lines_of(Mode::Observe, &[built()]),
            ["build: observing: 0 of 1 crate would have been reused"]
        );
    }

    // covers: B-20
    #[test]
    fn cargo_probes_are_not_compiles() {
        let entries = [
            probe(PassedThrough::Probe),
            reused(0),
            probe(PassedThrough::NoCrateName),
            built(),
            probe(PassedThrough::Probe),
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(summary.lines(), ["build: 1 of 2 crates reused, 1 built"]);
        assert_eq!(summary.counts().compiles, 2);
        assert_eq!(summary.counts().passed_through, 0);
    }

    // covers: B-20
    #[test]
    fn a_build_scripts_own_rustc_probes_are_not_crates() {
        // autocfg and friends call rustc through the wrapper with `-o`, or without
        // `-C extra-filename`, `--out-dir` or `--json=artifacts`: not the build's own crates.
        let entries = [
            reused(0),
            built(),
            entry(Outcome::PassedThrough(PassedThrough::NoExtraFilename)),
            entry(Outcome::PassedThrough(PassedThrough::NoOutDir)),
            entry(Outcome::PassedThrough(PassedThrough::OutputFile)),
            entry(Outcome::PassedThrough(PassedThrough::NoArtifacts)),
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(summary.lines(), ["build: 1 of 2 crates reused, 1 built"]);
        assert_eq!(summary.counts().compiles, 2);
        assert_eq!(summary.counts().passed_through, 0);
    }

    // covers: B-9, B-20
    #[test]
    fn a_compile_passed_through_counts_as_built() {
        // A crate type that is never stored, and clippy's driver: rustc ran, so the crate was
        // built. An incremental rebuild is passed through the same way.
        let entries = [
            reused(0),
            entry(Outcome::PassedThrough(PassedThrough::CrateType)),
            entry(Outcome::PassedThrough(PassedThrough::NotRustc)),
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(summary.lines(), ["build: 1 of 3 crates reused, 2 built"]);
        assert_eq!(summary.counts().passed_through, 2);
    }

    // covers: B-15, B-20
    #[test]
    fn notes_are_said_once_each_and_at_most_five() {
        let entries = [
            noted("a"),
            noted("b"),
            noted("a"),
            built(),
            noted("c"),
            noted("d"),
            noted("e"),
            noted("f"),
            noted("b"),
            noted("  "),
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(
            summary.lines(),
            [
                "build: 10 crates built, none reused",
                "build: a",
                "build: b",
                "build: c",
                "build: d",
                "build: e",
            ]
        );
        assert_eq!(summary.counts().notes, ["a", "b", "c", "d", "e", "f"]);
    }

    // covers: B-20, B-23
    #[test]
    fn what_the_check_removed_comes_after_the_totals() {
        let rebuilt = [
            "app is rebuilt: a file was added to migrations, which cargo doesn't watch".to_string(),
        ];
        assert_eq!(
            Summary::of_run(Mode::On, 10, &[built(), noted("a")], &rebuilt).lines(),
            [
                "build: 2 crates built, none reused",
                "build: app is rebuilt: a file was added to migrations, which cargo doesn't watch",
                "build: a",
            ]
        );
        // Said even when COMMAND compiled nothing.
        assert_eq!(
            Summary::of_run(Mode::On, 10, &[], &rebuilt).lines().len(),
            1
        );
        assert!(Summary::of_run(Mode::On, 10, &[], &[]).lines().is_empty());
    }

    // covers: B-22
    #[test]
    fn low_space_is_said_once() {
        let low = || entry(Outcome::BuiltNotStored(NotStored::LowSpace));
        let entries = [reused(0), low(), low()];
        assert_eq!(
            lines_of(Mode::On, &entries),
            [
                "build: 1 of 3 crates reused, 2 built",
                "build: nothing was stored because the disk has less than 10 GB free",
            ]
        );
        // Some were stored before the disk filled.
        let entries = [built(), low(), low()];
        assert_eq!(
            Summary::of(Mode::On, 20, &entries).lines(),
            [
                "build: 3 crates built, none reused",
                "build: 2 crates were not stored because the disk has less than 20 GB free",
            ]
        );
        let entries = [built(), low()];
        assert_eq!(
            lines_of(Mode::On, &entries)[1],
            "build: 1 crate was not stored because the disk has less than 10 GB free"
        );
    }

    #[test]
    fn durations_read_like_the_time_a_skip_saved() {
        assert_eq!(took(400), "0.4s");
        assert_eq!(took(41_000), "41s");
        assert_eq!(took(192_000), "3m 12s");
        assert_eq!(took(3_840_000), "1h 4m");
    }

    // finish.

    /// 2026-11-11T00:00:00Z.
    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_794_355_200)
    }

    fn days(n: u64) -> Duration {
        Duration::from_secs(n * 86_400)
    }

    struct Run {
        dir: tempfile::TempDir,
        settings: PathBuf,
        exe: PathBuf,
    }

    impl Run {
        fn new(mode: Mode) -> Run {
            let dir = tempfile::tempdir().unwrap();
            let settings = dir.path().join("build.json");
            let exe = dir.path().join("hushrun");
            std::fs::write(&exe, b"binary").unwrap();
            settings::update(&settings, |s| s.mode = mode).unwrap();
            Run { dir, settings, exe }
        }

        fn begin(&self) -> Session {
            let none = |_: &str| None;
            let mut start = start(self.dir.path(), &self.settings, &self.exe, &none);
            start.now = now();
            begin(&start).unwrap()
        }

        fn store(&self) -> Store {
            Store::new(&self.dir.path().join("build"))
        }
    }

    fn set_modified(path: &Path, when: SystemTime) {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    // covers: B-20
    #[test]
    fn finish_reads_the_log_and_removes_the_session() {
        let run = Run::new(Mode::On);
        let session = run.begin();
        let folder = session.current().folder.clone();
        session.current().log(&reused(2_000));
        session.current().log(&built());
        // A line cut short, and one that is not JSON: left out, the rest still counts.
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(folder.join(LOG_FILE))
            .unwrap();
        std::io::Write::write_all(&mut log, b"{\"outcome\":\nnot json\n").unwrap();
        drop(log);
        session.current().log(&built());
        let summary = session.finish(now());
        assert_eq!(
            summary.lines(),
            ["build: 1 of 3 crates reused, 2 built, saving about 2s"]
        );
        assert_eq!(summary.counts().mode, Mode::On);
        assert!(!folder.exists());
        // The other runs' folder stays.
        assert!(folder.parent().unwrap().is_dir());
    }

    // covers: B-20
    #[test]
    fn a_run_with_no_log_says_nothing() {
        let run = Run::new(Mode::Observe);
        let session = run.begin();
        let folder = session.current().folder.clone();
        let summary = session.finish(now());
        assert!(summary.lines().is_empty());
        assert_eq!(summary.counts().mode, Mode::Observe);
        assert!(!folder.exists());
    }

    // covers: B-14
    #[test]
    fn finish_sweeps_the_store_once_a_day() {
        let run = Run::new(Mode::On);
        let store = run.store();
        let key = |n: u32| hushrun_core::Digest::of(&n.to_le_bytes());
        let publish = |n: u32, used: SystemTime| {
            let source = run.dir.path().join(format!("source-{n}"));
            std::fs::write(&source, n.to_le_bytes()).unwrap();
            let file = crate::store::NewFile {
                name: "lib.rlib".into(),
                source,
            };
            let description = serde_json::json!({ "repository": "r" });
            store.publish(&key(n), &description, &[file], used).unwrap();
        };
        let has = |n: u32| store.block(&key(n)).unwrap().is_some();
        publish(1, now() - days(20));
        store.used(&key(1), now() - days(16)).unwrap();
        publish(2, now() - days(20));
        store.used(&key(2), now() - days(14)).unwrap();
        run.begin().finish(now());
        assert!(!has(1), "last used 16 days before");
        assert!(has(2), "last used 14 days before");

        // Within the day, a second run sweeps nothing.
        publish(3, now() - days(16));
        run.begin().finish(now() + Duration::from_secs(3_600));
        assert!(has(3));
        // The next day's run does.
        run.begin().finish(now() + days(1));
        assert!(!has(3));
    }

    // covers: B-9, B-21
    // covers: B-63, B-64
    #[test]
    fn a_block_from_the_baked_store_is_reused_without_asking_only_while_the_store_is_current() {
        use crate::baked::{Evidence, Lower, LowerPart, Source, State, Trust, Untrusted};
        use crate::model::Kind;
        use crate::origin::Origin;
        use hushrun_core::blocks::Hex32;
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("build.json");
        let exe = dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        let none = |_: &str| None;
        settings::update(&settings, |s| s.mode = Mode::On).unwrap();
        let mut session = begin(&start(dir.path(), &settings, &exe, &none)).unwrap();
        let read = |session: &Session| {
            let folder = session.env()[1].1.clone();
            current(&|name| (name == SESSION).then(|| folder.clone())).unwrap()
        };
        let accepted = Lower {
            folder: dir.path().join("baked"),
            manifest: Hex32([9; 32]),
            contents: Hex32([7; 32]),
            claim: Trust::Untrusted {
                because: Untrusted::LocalStore,
            },
            source: Source::Store,
            repository: None,
            checks: vec![None],
            parts: vec![LowerPart {
                kind: "rustc/1".into(),
                platform: "aarch64-unknown-linux-gnu".into(),
            }],
            evidence: Evidence::Accepted,
            notes: Vec::new(),
            openable: Vec::new(),
            index: None,
        };
        // What session.json keeps of it: everything but its rows, which are in the index.
        let bare = |lower: Option<&Lower>| {
            lower.map(|lower| {
                assert!(lower.index.as_ref().is_some_and(|i| i.is_file()));
                Lower {
                    index: None,
                    ..lower.clone()
                }
            })
        };
        let vouch = || crate::relay::Request::Sharing;
        let mut asked = 0;
        let mut ask = |_: &Path, _: &crate::relay::Request| {
            asked += 1;
            None
        };
        let from_store = Origin::baked(Hex32([7; 32]), Hex32([1; 32]));
        let from_older = Origin::baked(Hex32([6; 32]), Hex32([1; 32]));
        let built = Origin::built_here("1-1", None);

        // No baked store: a block from one stands as a download no server vouched for, in a run
        // that shares nothing by a decision.
        assert!(read(&session).lower_for(Kind::Rustc).is_none());
        assert!(!read(&session).may_reuse(Some(&from_store), 0, vouch(), &mut ask));
        assert!(read(&session).may_reuse(Some(&built), 0, vouch(), &mut ask));

        // Current and accepted unverified: its own blocks without asking, an older store's not;
        // its parts only.
        session
            .set_lower(&State::Current(accepted.clone()))
            .unwrap();
        assert_eq!(
            bare(read(&session).lower_for(Kind::Rustc)),
            Some(accepted.clone())
        );
        assert!(read(&session).lower_for(Kind::Go).is_none());
        assert!(read(&session).may_reuse(Some(&from_store), 0, vouch(), &mut ask));
        assert!(!read(&session).may_reuse(Some(&from_older), 0, vouch(), &mut ask));

        // Current with the server's evidence: never unasked on the store's word, only on the
        // server's (a vouch the record holds), which a run with no relay can't have.
        let verified = Lower {
            evidence: Evidence::Server(crate::baked::Server {
                host: "https://api.example.com".into(),
                org: "acme".into(),
                repository: "github.com/acme/shop".into(),
                scope: "protected/4242".into(),
            }),
            ..accepted.clone()
        };
        session.set_lower(&State::Current(verified)).unwrap();
        assert!(!read(&session).may_reuse(Some(&from_store), 0, vouch(), &mut ask));

        // Replaced: read only where the run shares or would; its blocks as stale downloads.
        session
            .set_lower(&State::Replaced(accepted.clone()))
            .unwrap();
        assert!(read(&session).lower_for(Kind::Rustc).is_none());
        assert!(!read(&session).may_reuse(Some(&from_store), 0, vouch(), &mut ask));
        session.set_unreached().unwrap();
        assert_eq!(
            bare(read(&session).lower_for(Kind::Rustc)),
            Some(accepted.clone())
        );
        assert!(read(&session).may_reuse(Some(&from_store), 0, vouch(), &mut ask));

        // A state the run ignores leaves no reading, and its line.
        session
            .set_lower(&State::WrongPlatform {
                folder: accepted.folder.clone(),
                baked: "rustc/1 x86_64-unknown-linux-gnu".into(),
                here: "rustc/1 aarch64-unknown-linux-gnu".into(),
            })
            .unwrap();
        assert!(read(&session).lower.is_none());
        assert_eq!(asked, 0);
        let summary = session.finish(SystemTime::now());
        assert!(
            summary.lines().iter().any(
                |l| l.contains("is ignored: it was baked for rustc/1 x86_64-unknown-linux-gnu")
            ),
            "{:?}",
            summary.lines()
        );
    }

    // covers: B-16, B-63
    #[test]
    fn a_verified_baked_block_s_first_reuse_is_never_audited() {
        use crate::audit::Reuse;
        use crate::baked::{Evidence, Lower, LowerPart, Server, Source, State, Trust};
        use crate::origin::Origin;
        use hushrun_core::blocks::Hex32;
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("build.json");
        let exe = dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        let none = |_: &str| None;
        settings::update(&settings, |s| s.mode = Mode::On).unwrap();
        let mut session = begin(&start(dir.path(), &settings, &exe, &none)).unwrap();
        let read = |session: &Session| {
            let folder = session.env()[1].1.clone();
            current(&|name| (name == SESSION).then(|| folder.clone())).unwrap()
        };
        let lower = |evidence: Evidence| Lower {
            folder: dir.path().join("baked"),
            manifest: Hex32([9; 32]),
            contents: Hex32([7; 32]),
            claim: Trust::Trusted,
            source: Source::Store,
            repository: None,
            checks: vec![None],
            parts: vec![LowerPart {
                kind: "rustc/1".into(),
                platform: "aarch64-unknown-linux-gnu".into(),
            }],
            evidence,
            notes: Vec::new(),
            openable: Vec::new(),
            index: None,
        };
        let from_store = Origin::baked(Hex32([7; 32]), Hex32([1; 32]));
        let from_other = Origin::baked(Hex32([6; 32]), Hex32([1; 32]));
        // No baked store: a block from one is a download, audited one time in ten.
        assert_eq!(
            read(&session).reuse_pick(Some(&from_store)),
            Reuse::FirstDownload
        );
        // Accepted unverified: still.
        session
            .set_lower(&State::Current(lower(Evidence::Accepted)))
            .unwrap();
        assert_eq!(
            read(&session).reuse_pick(Some(&from_store)),
            Reuse::FirstDownload
        );
        // Verified by the server or a signature: never at its first reuse, for that store's
        // blocks only.
        let server = Evidence::Server(Server {
            host: "https://api.example.com".into(),
            org: "acme".into(),
            repository: "github.com/acme/shop".into(),
            scope: "protected/4242".into(),
        });
        session.set_lower(&State::Current(lower(server))).unwrap();
        assert_eq!(
            read(&session).reuse_pick(Some(&from_store)),
            Reuse::FirstVerified
        );
        assert_eq!(
            read(&session).reuse_pick(Some(&from_other)),
            Reuse::FirstDownload
        );
        let signed = |current: bool| {
            Evidence::Signed(crate::baked::Signed {
                signer: "key x".into(),
                baked_json: Hex32([7; 32]),
                hints: dir.path().join("hints"),
                current,
            })
        };
        // Signed, and no backend to ask: as verified as it can be now.
        session
            .set_lower(&State::Current(lower(signed(true))))
            .unwrap();
        assert_eq!(
            read(&session).reuse_pick(Some(&from_store)),
            Reuse::FirstVerified
        );
        // Signed, where a backend didn't answer for it or isn't its source: a withdrawal can't
        // reach the run, so its first reuse keeps the audit; and a store replaced since does too.
        session
            .set_lower(&State::Current(lower(signed(false))))
            .unwrap();
        assert_eq!(
            read(&session).reuse_pick(Some(&from_store)),
            Reuse::FirstDownload
        );
        session
            .set_lower(&State::Replaced(lower(signed(true))))
            .unwrap();
        assert_eq!(
            read(&session).reuse_pick(Some(&from_store)),
            Reuse::FirstDownload
        );
        session
            .set_lower(&State::Current(lower(signed(true))))
            .unwrap();
        // What the callers decide by: never sampled, whatever the sample would say, unless the
        // run audits every compile.
        let mut current = read(&session);
        assert!(!current.audits_reuse(Some(&from_store), |_| panic!("sampled")));
        assert!(current.audits_reuse(Some(&from_other), |r| r == Reuse::FirstDownload));
        current.audit = crate::audit::Audit::Every;
        assert!(current.audits_reuse(Some(&from_store), |_| false));
        current.audit = crate::audit::Audit::Never;
        assert!(!current.audits_reuse(Some(&from_other), |_| true));
        // A block built here, reused here already (a verified one's second reuse among them),
        // or with no record, is audited as any reuse is.
        assert_eq!(
            read(&session).reuse_pick(Some(&Origin::built_here("1-1", None))),
            Reuse::Any
        );
        let mut reused = from_store.clone();
        reused.reused = true;
        assert_eq!(read(&session).reuse_pick(Some(&reused)), Reuse::Any);
        let mut reused = from_other.clone();
        reused.reused = true;
        assert_eq!(read(&session).reuse_pick(Some(&reused)), Reuse::Any);
        assert_eq!(read(&session).reuse_pick(None), Reuse::Any);
    }

    #[test]
    fn a_runs_programs_are_decided_from_its_setting_and_whether_it_shares() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("build.json");
        let exe = dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        let none = |_: &str| None;
        let share = || Share {
            socket: dir.path().join("relay.sock"),
            run: "1-1".into(),
            by: crate::origin::Identity {
                host: "https://api.example.com".into(),
                org: "acme".into(),
                credential: crate::origin::fingerprint("hru_test"),
            },
            repository: String::new(),
        };
        let read = |session: &Session| {
            let folder = session.env()[1].1.clone();
            current(&|name| (name == SESSION).then(|| folder.clone())).unwrap()
        };
        let written = |session: &Session| -> serde_json::Value {
            let file = session.current().folder.join(SESSION_FILE);
            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
        };

        // Nobody chose: tied while the run shares nothing, movable once it shares or would,
        // as every wrapper of the run reads it.
        settings::update(&settings, |s| s.mode = Mode::On).unwrap();
        let mut session = begin(&start(dir.path(), &settings, &exe, &none)).unwrap();
        assert!(!read(&session).shares());
        assert_eq!(read(&session).programs(), Programs::Tied);
        // Nobody's choice is not written, so a wrapper of an earlier hushrun reads its default.
        assert!(written(&session).get("programs").is_none());
        session.set_unreached().unwrap();
        assert!(read(&session).shares());
        assert_eq!(read(&session).programs(), Programs::Movable);
        session.set_share(share()).unwrap();
        assert!(read(&session).shares());
        assert_eq!(read(&session).programs(), Programs::Movable);
        assert!(written(&session).get("programs").is_none());

        // A session as an earlier hushrun wrote it: `movable` whoever chose or didn't, or no
        // field at all. Both read as nobody's choice.
        let file = session.current().folder.join(SESSION_FILE);
        let mut old = written(&session);
        old["programs"] = "movable".into();
        std::fs::write(&file, old.to_string()).unwrap();
        assert_eq!(read(&session).programs_setting, ProgramsSetting::Follow);
        assert_eq!(read(&session), *session.current());
        old.as_object_mut().unwrap().remove("programs");
        old.as_object_mut().unwrap().remove("share");
        std::fs::write(&file, old.to_string()).unwrap();
        assert_eq!(read(&session).programs_setting, ProgramsSetting::Follow);
        assert_eq!(read(&session).programs(), Programs::Tied);
        session.finish(UNIX_EPOCH);

        // `tied` is tied whatever the run shares, and is written.
        settings::update(&settings, |s| s.programs = ProgramsSetting::Tied).unwrap();
        let mut session = begin(&start(dir.path(), &settings, &exe, &none)).unwrap();
        assert_eq!(written(&session)["programs"], "tied");
        assert_eq!(read(&session).programs(), Programs::Tied);
        session.set_unreached().unwrap();
        assert_eq!(read(&session).programs(), Programs::Tied);
        session.set_share(share()).unwrap();
        assert_eq!(read(&session).programs(), Programs::Tied);
        session.finish(UNIX_EPOCH);
    }

    // covers: B-25, B-31
    #[test]
    fn in_ci_no_persons_programs_setting_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("build.json");
        let exe = dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        let none = |_: &str| None;
        // A file on the runner that says `tied`: CI reads only its `mode`.
        std::fs::write(&settings, br#"{"version": 1, "programs": "tied"}"#).unwrap();
        let ci = Start {
            in_ci: true,
            ..start(dir.path(), &settings, &exe, &none)
        };
        let mut session = begin_in_ci(&ci, &dir.path().join("job")).unwrap();
        assert_eq!(session.current().programs_setting, ProgramsSetting::Follow);
        assert_eq!(session.current().programs(), Programs::Tied);
        session.set_unreached().unwrap();
        assert_eq!(session.current().programs(), Programs::Movable);
        session.finish(UNIX_EPOCH);
    }

    // covers: B-2
    #[test]
    fn two_runs_that_begin_in_the_same_millisecond_have_a_session_each() {
        // One store, one process id, one millisecond: two containers that share a store, or two
        // sessions of one test. Each is a person with a home of their own.
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("build.json");
        let exe = dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        settings::update(&settings, |s| s.mode = Mode::On).unwrap();
        let home =
            |home: &'static str| move |name: &str| (name == "HOME").then(|| OsString::from(home));
        let (one, two) = (home("/home/dev"), home("/home/dev2"));
        let first = begin(&start(dir.path(), &settings, &exe, &one)).unwrap();
        let second = begin(&start(dir.path(), &settings, &exe, &two)).unwrap();
        assert_ne!(first.current().folder, second.current().folder);
        // What each run's wrappers read is that run's own.
        let read = |session: &Session| {
            let folder = session.current().folder.clone();
            current(&|name| (name == SESSION).then(|| folder.clone().into_os_string())).unwrap()
        };
        assert_eq!(read(&first).cargo_home.as_deref(), Some("/home/dev/.cargo"));
        assert_eq!(
            read(&second).cargo_home.as_deref(),
            Some("/home/dev2/.cargo")
        );
        // One finishing leaves the other's session in place.
        second.finish(SystemTime::now());
        assert_eq!(read(&first).home.as_deref(), Some("/home/dev"));
    }

    // covers: B-19
    #[test]
    fn finish_removes_the_sessions_of_killed_runs() {
        let run = Run::new(Mode::On);
        let sessions = run.dir.path().join("build").join(SESSIONS);
        let killed = |name: &str, age: Duration| {
            let folder = sessions.join(name);
            std::fs::create_dir_all(&folder).unwrap();
            let file = folder.join(SESSION_FILE);
            std::fs::write(&file, b"{}").unwrap();
            set_modified(&file, now() - age);
            set_modified(&folder, now() - age);
            folder
        };
        let old = killed("1-1", days(3));
        let recent = killed("2-2", days(1));
        // A log written to a day ago: a run still compiling, though it began long before.
        let long = killed("3-3", days(5));
        let log = long.join(LOG_FILE);
        std::fs::write(&log, b"").unwrap();
        set_modified(&log, now() - days(1));
        set_modified(&long, now() - days(5));
        run.begin().finish(now());
        assert!(!old.exists());
        assert!(recent.is_dir());
        assert!(long.is_dir());
    }

    // covers: B-2
    #[test]
    fn finish_removes_links_to_a_binary_that_is_gone() {
        let run = Run::new(Mode::On);
        let store = run.dir.path().join("build");
        let gone = run.dir.path().join("uninstalled");
        std::fs::write(&gone, b"old").unwrap();
        let dangling = link_to(&store, &gone).unwrap();
        std::fs::remove_file(&gone).unwrap();
        let session = run.begin();
        let live = PathBuf::from(&session.env()[0].1);
        session.finish(now());
        assert!(std::fs::symlink_metadata(&dangling).is_err());
        assert!(!dangling.parent().unwrap().exists());
        assert_eq!(std::fs::read_link(&live).unwrap(), run.exe);
        // A folder with no link in it is not this rule's.
        let other = store.join(BIN).join("other");
        std::fs::create_dir_all(&other).unwrap();
        run.begin().finish(now());
        assert!(other.is_dir());
        assert!(live.exists());
    }

    // covers: B-109
    #[test]
    fn typescript_compiles_are_counted_apart_and_said_in_their_own_lines() {
        use crate::tsc::helper::TscEntry;
        let tsc = |outcome: Outcome, why: Option<&str>| Entry {
            tsc: Some(TscEntry {
                why: why.map(str::to_string),
                ..TscEntry::default()
            }),
            ..entry(outcome)
        };
        let entries = vec![
            entry(Outcome::BuiltAndStored),
            Entry {
                saved_ms: 2_500,
                downloaded: true,
                ..tsc(Outcome::Reused, None)
            },
            tsc(Outcome::BuiltAndStored, None),
            tsc(
                Outcome::PassedThrough(PassedThrough::UnknownArgument),
                Some("watch"),
            ),
            tsc(
                Outcome::BuiltNotStored(NotStored::Unkeyable),
                Some("build-over-outputs"),
            ),
            tsc(
                Outcome::BuiltNotStored(NotStored::Unkeyable),
                Some("build-over-outputs"),
            ),
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(
            summary.counts().compiles,
            1,
            "one crate, the compiles apart"
        );
        let lines: Vec<&String> = summary
            .lines()
            .iter()
            .filter(|l| l.contains("typescript"))
            .collect();
        assert_eq!(
            lines,
            [
                "build: typescript: 5 compiles: 1 reused (1 from the shared store), 4 built, saving about 2s",
                "build: typescript: 3 compiles run as asked or not stored (1 watch, 2 build-over-outputs)",
            ]
        );
        let observing = Summary::of(Mode::Observe, 10, &entries);
        assert!(
            observing.lines().iter().any(
                |l| l == "build: typescript: observing: 0 of 5 compiles would have been reused"
            ),
            "{:?}",
            observing.lines()
        );
    }

    // covers: B-61
    #[test]
    fn c_objects_are_counted_apart_from_crates_and_said_in_their_own_lines() {
        use crate::cc::launcher::CcEntry;
        let cc = |outcome: Outcome, why: Option<&str>| Entry {
            xcode: None,
            cc: Some(CcEntry {
                why: why.map(str::to_string),
                ..CcEntry::default()
            }),
            ..entry(outcome)
        };
        let probe = Entry {
            xcode: None,
            cc: Some(CcEntry {
                why: Some("cmake-probe".into()),
                probe: true,
                ..CcEntry::default()
            }),
            ..entry(Outcome::PassedThrough(PassedThrough::UnknownArgument))
        };
        let entries = vec![
            entry(Outcome::BuiltAndStored),
            Entry {
                saved_ms: 2_500,
                ..cc(Outcome::Reused, None)
            },
            Entry {
                saved_ms: 1_000,
                ..cc(Outcome::Reused, None)
            },
            cc(Outcome::BuiltAndStored, None),
            cc(
                Outcome::PassedThrough(PassedThrough::UnknownArgument),
                Some("pch"),
            ),
            cc(
                Outcome::PassedThrough(PassedThrough::UnknownArgument),
                Some("pch"),
            ),
            cc(
                Outcome::BuiltNotStored(NotStored::Unkeyable),
                Some("time-macro"),
            ),
            cc(
                Outcome::PassedThrough(PassedThrough::UnknownArgument),
                Some("modules"),
            ),
            cc(
                Outcome::BuiltNotStored(NotStored::RustcFailed),
                Some("compile-failed"),
            ),
            Entry {
                found: true,
                differed: true,
                ..cc(
                    Outcome::BuiltNotStored(NotStored::Differed),
                    Some("differed"),
                )
            },
            probe,
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(summary.counts().compiles, 1, "one crate, the objects apart");
        assert_eq!(
            summary.counts().cc.objects,
            9,
            "CMake's configure check counts nowhere"
        );
        assert_eq!(
            summary.lines(),
            [
                "build: 1 crate built, none reused",
                "build: c/c++: 9 objects: 2 reused, 7 built, saving about 3s, 1 compared with their blocks, 1 differed",
                "build: c/c++: 6 objects compiled as asked or not stored (2 pch, 1 time-macro, 1 modules, …)",
            ]
        );
        let observed = Summary::of(Mode::Observe, 10, &entries[3..4]);
        assert_eq!(
            observed.lines(),
            ["build: c/c++: observing: 0 of 1 object would have been reused"]
        );
        assert!(
            Summary::of(Mode::On, 10, &entries[..1])
                .counts()
                .cc
                .lines(Mode::On)
                .is_empty()
        );
    }

    // covers: B-60, B-61
    #[test]
    fn objects_a_run_that_shares_keeps_here_are_said_with_why_and_downloads_counted() {
        use crate::cc::launcher::CcEntry;
        let stored = |local: &[&str], shared: bool| Entry {
            xcode: None,
            cc: Some(CcEntry {
                local: local.iter().map(|s| s.to_string()).collect(),
                shared,
                ..CcEntry::default()
            }),
            ..entry(Outcome::BuiltAndStored)
        };
        let entries = vec![
            stored(&[], true),
            stored(&["tied"], true),
            stored(&["tied", "home-in-output"], true),
            stored(&["native-cpu"], true),
            // A run without a relay keeps everything here, and says nothing of it.
            stored(&["tied"], false),
            Entry {
                downloaded: true,
                saved_ms: 4_000,
                xcode: None,
                cc: Some(CcEntry::default()),
                ..entry(Outcome::Reused)
            },
        ];
        assert_eq!(
            Summary::of(Mode::On, 10, &entries).lines(),
            [
                "build: c/c++: 6 objects: 1 reused (1 from the shared store), 5 built, saving about 4s",
                "build: c/c++: 3 objects compiled here stay on this machine (2 tied, 1 home-in-output, 1 native-cpu)",
            ]
        );
    }

    // covers: B-60
    #[test]
    fn a_c_object_s_line_is_of_kind_cc_and_its_blocks_name_it() {
        let line = Entry {
            xcode: None,
            cc: Some(crate::cc::launcher::CcEntry::default()),
            ..entry(Outcome::Reused)
        };
        assert_eq!(line.kind(), crate::model::Kind::Cc);
        assert_eq!(crate::model::Kind::Cc.word(), "cc/1");
        assert_eq!(
            crate::model::Kind::of_description(&serde_json::json!({"kind": "cc/1"})),
            crate::model::Kind::Cc
        );
        // A line read back keeps its kind; one from before the launcher has none.
        let back: Entry = serde_json::from_slice(&serde_json::to_vec(&line).unwrap()).unwrap();
        assert_eq!(back.kind(), crate::model::Kind::Cc);
        assert_eq!(entry(Outcome::Reused).kind(), crate::model::Kind::Rustc);
    }

    // covers: B-46
    #[test]
    fn go_archives_are_counted_apart_from_crates_and_said_in_their_own_lines() {
        use crate::go::archive::Local;
        let go = |outcome: Outcome, go: GoEntry| Entry {
            go: Some(go),
            ..entry(outcome)
        };
        let entries = vec![
            entry(Outcome::BuiltAndStored),
            go(
                Outcome::Reused,
                GoEntry {
                    go_cache: true,
                    local: None,
                },
            ),
            Entry {
                downloaded: true,
                saved_ms: 3_000,
                ..go(Outcome::Reused, GoEntry::default())
            },
            go(Outcome::BuiltAndStored, GoEntry::default()),
            go(
                Outcome::BuiltNotStored(NotStored::StoreSkipped),
                GoEntry {
                    go_cache: false,
                    local: Some(Local::Native),
                },
            ),
            go(
                Outcome::BuiltNotStored(NotStored::StoreSkipped),
                GoEntry {
                    go_cache: false,
                    local: Some(Local::NotSharing),
                },
            ),
        ];
        let summary = Summary::of(Mode::On, 10, &entries);
        assert_eq!(summary.counts().compiles, 1, "a crate, and no Go archive");
        assert_eq!(
            summary.lines(),
            [
                "build: 1 crate built, none reused".to_string(),
                "build: go: 5 packages: 1 from Go's own cache, 1 reused (1 from the shared store), 3 built, saving about 3s".to_string(),
                "build: go: 1 package compiled here stays on this machine (1 native code (cgo or .syso))".to_string(),
            ]
        );
    }
}
