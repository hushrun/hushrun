//! Whether a block may leave the machine that built it: [`portable`] is the one place that
//! decides it.
//!
//! A block is **portable** when nothing in its keys, its hint, its record or its bytes names the
//! machine: no folder of this checkout (its place is `anywhere`), no read outside a root or a
//! base (the cargo home, the toolchain), none of the cargo home, the rustup home or the home
//! folder in an output or a record, no variable naming a base that a second compile didn't prove
//! harmless, and no proc macro among its externs that another machine names differently. A
//! linked output (a program, a proc macro, a build-script binary) is portable on the same terms
//! when its link environment is known by content (B-37), and when no output holds the
//! toolchain's own folder either. Whatever the answer, the block is reused on this machine
//! exactly as before: this decides only whether it may leave.
//!
//! A library among the externs is keyed by the SHA-256 of its file, so a dependency that is not
//! portable gives its dependents other keys on another machine, never a wrong block; its own
//! reasons say why. It is not a reason here: cargo starts a dependent as soon as the dependency's
//! `.rmeta` is written, before the dependency's record says whether it is portable.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::key::{Asked, Observed, Place};
use crate::model::{Class, Folder, Located};

/// Why a block may not leave the machine. Stable: a later tool counts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// A program, a proc macro or a build-script binary whose link environment can't be told
    /// by content (B-37): [`Portability::link_environment`] says why.
    LinkEnvironmentUnknown,
    /// A linked output holds the toolchain's own folder (`<sysroot>/lib/rustlib`, B-10).
    ToolchainInOutput,
    /// Its place is its roots (B-10): an output holds a folder of the checkout, a read lies
    /// outside every root, or the outputs can't be searched.
    Tied,
    /// Its outputs can't be searched (B-10: `-C lto`, compressed debug sections): it may hold
    /// a folder nobody saw, so it is `tied` too and never travels, even where every folder a
    /// search could name is the same.
    Unsearchable,
    /// A read lies outside every root, base and shared folder.
    ReadOutside,
    /// A read lies in a shared folder that is not a base (a system folder, the rustup home
    /// outside the toolchain): its path is this machine's.
    ReadInSharedFolder,
    /// An output holds the cargo home.
    CargoHomeInOutput,
    /// An output holds the rustup home.
    RustupHomeInOutput,
    /// An output holds the home folder.
    HomeInOutput,
    /// A key, or what the block records beside its files, holds the cargo home.
    CargoHomeInRecord,
    /// A key, or what the block records beside its files, holds the rustup home.
    RustupHomeInRecord,
    /// A key, or what the block records beside its files, holds the home folder.
    HomeInRecord,
    /// A variable the compile read names a base, and a second compile with it pointing
    /// elsewhere didn't give the same bytes (or couldn't be made).
    VariableNamesBase,
    /// A proc macro among the externs is one another machine would name differently: it is not
    /// named by what it was built from, or that names this machine.
    Extern,
    /// An output holds the value of a variable that looks like a secret and is not in the
    /// block's key ([`unkeyed_candidate`]): anyone who can open the block could read it, and
    /// another machine with another value would reuse this one's bytes.
    UnkeyedValue,
    /// It was compiled for `-C target-cpu=native` (B-5). rustc hands the code generator every
    /// feature of this machine's CPU, more than any of its own words print, so no key can say
    /// what the flag meant here, and another machine could share every printed word and still
    /// run code it can't execute. Such a block never leaves the machine.
    NativeCpu,
}

impl Reason {
    /// The reason as logs and records write it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Reason::LinkEnvironmentUnknown => "link-environment-unknown",
            Reason::ToolchainInOutput => "toolchain-in-output",
            Reason::Tied => "tied",
            Reason::Unsearchable => "unsearchable",
            Reason::ReadOutside => "read-outside",
            Reason::ReadInSharedFolder => "read-in-shared-folder",
            Reason::CargoHomeInOutput => "cargo-home-in-output",
            Reason::RustupHomeInOutput => "rustup-home-in-output",
            Reason::HomeInOutput => "home-in-output",
            Reason::CargoHomeInRecord => "cargo-home-in-record",
            Reason::RustupHomeInRecord => "rustup-home-in-record",
            Reason::HomeInRecord => "home-in-record",
            Reason::VariableNamesBase => "variable-names-base",
            Reason::Extern => "extern",
            Reason::UnkeyedValue => "unkeyed-value",
            Reason::NativeCpu => "native-cpu",
        }
    }

    fn in_output(folder: Folder) -> Reason {
        match folder {
            Folder::CargoHome => Reason::CargoHomeInOutput,
            Folder::RustupHome => Reason::RustupHomeInOutput,
            Folder::Home => Reason::HomeInOutput,
        }
    }

    fn in_record(folder: Folder) -> Reason {
        match folder {
            Folder::CargoHome => Reason::CargoHomeInRecord,
            Folder::RustupHome => Reason::RustupHomeInRecord,
            Folder::Home => Reason::HomeInRecord,
        }
    }
}

/// The answer, with every reason that holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Portability {
    /// Whether the block may leave the machine: no reason holds.
    pub portable: bool,
    /// Every reason that holds, sorted.
    pub reasons: Vec<Reason>,
    /// The externs [`Reason::Extern`] is about, by crate name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub externs: Vec<String>,
    /// The variables [`Reason::UnkeyedValue`] is about, by name only: a value is never
    /// recorded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<String>,
    /// Why a linked output's environment is unknown ([`Reason::LinkEnvironmentUnknown`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_environment: Option<String>,
}

impl Portability {
    /// Whether a dependent on another machine names this block's files as a dependent here
    /// does. A library: when it is portable, since it is named by its bytes. A proc macro: when
    /// it is named by what it was built from (its place is `anywhere`) and that names no
    /// machine: its link environment is left out of that name, and its own bytes may still hold
    /// a machine's folder (a link names the standard library's), since no dependent keys them,
    /// and a folder its expansion writes into a dependent's output is found there. Nothing else
    /// has dependents.
    /// Whether the only things that keep the block on its machine are folders whose values
    /// its keys hold (B-24): a folder of the checkout or of the machine in an output or a
    /// record, a variable that names a base, a program keyed by its target folder, and a read
    /// in a shared folder (a library a crate asks for with `-l`, found in the system's or the
    /// SDK's folder), which the second key holds by its path and its content. Another machine
    /// computes those keys only where each such folder is the same text, so there the block is
    /// as right as it is here. Never when the outputs can't be searched, a read lies outside
    /// every root, base and shared folder, the link environment is unknown, a value no key
    /// holds is in the bytes, the CPU is this machine's, or an extern is named by this machine
    /// alone.
    pub fn bound_only(&self) -> bool {
        !self.portable
            && self.reasons.iter().all(|reason| {
                matches!(
                    reason,
                    Reason::Tied
                        | Reason::ReadInSharedFolder
                        | Reason::ToolchainInOutput
                        | Reason::CargoHomeInOutput
                        | Reason::RustupHomeInOutput
                        | Reason::HomeInOutput
                        | Reason::CargoHomeInRecord
                        | Reason::RustupHomeInRecord
                        | Reason::HomeInRecord
                        | Reason::VariableNamesBase
                )
            })
    }

    pub fn named_portably(&self, class: Class) -> bool {
        match class {
            Class::Library => self.portable,
            Class::ProcMacro => self.reasons.iter().all(|reason| {
                matches!(
                    reason,
                    Reason::LinkEnvironmentUnknown
                        | Reason::CargoHomeInOutput
                        | Reason::RustupHomeInOutput
                        | Reason::HomeInOutput
                        | Reason::ToolchainInOutput
                )
            }),
            Class::BuildScript | Class::Program => false,
        }
    }
}

/// What [`portable`] decides from.
pub struct Facts<'a> {
    /// What the compile produces.
    pub class: Class,
    /// The first key's parts.
    pub asked: &'a Asked,
    /// The second key's parts, with the block's place decided.
    pub observed: &'a Observed,
    /// What else the block records beside its files, as text: its dep-info and messages with
    /// placeholders.
    pub records: &'a [String],
    /// The folders found in an output.
    pub in_outputs: &'a BTreeSet<Folder>,
    /// The folders that name this machine, each form as text ([`crate::roots::Roots::machine_needles`]).
    pub machine: &'a [(Folder, String)],
    /// A variable the compile read names a base and no second compile proved it harmless.
    pub variable_names_base: bool,
    /// Each proc macro among the externs by crate name, and whether another machine names it
    /// the same way.
    pub externs: &'a [(String, bool)],
    /// The variables whose value an output holds, of those [`unkeyed_candidate`] chose: by
    /// name.
    pub unkeyed: &'a [String],
    /// It is compiled for this machine's CPU: `-C target-cpu=native`, or `-mcpu=native` handed to
    /// the code generator.
    pub native_cpu: bool,
    /// A linked output holds the toolchain's own folder (B-10).
    pub toolchain_in_output: bool,
    /// Its first key holds a folder as this machine has it (B-9: a movable program whose
    /// output folder lies outside the workspace): it is reused only where that folder is the
    /// same, which is what `tied` says.
    pub keyed_by_place: bool,
    /// The outputs can be searched for folders as raw bytes (B-10).
    pub searchable: bool,
}

/// Whether the block may leave the machine, and every reason it may not.
pub fn portable(facts: &Facts<'_>) -> Portability {
    let mut reasons = BTreeSet::new();
    // A linked output leaves the machine only with its link environment known (B-37).
    let mut link_environment = None;
    if facts.class != Class::Library {
        let why = match facts.observed.link.as_ref() {
            Some(link) if link.environment.is_some() => None,
            Some(link) => Some(match &link.environment_list {
                Some(crate::linkenv::LinkEnv::Unknown { reason }) => reason.clone(),
                _ => "its link environment wasn't computed".to_owned(),
            }),
            None => Some("its link line wasn't read".to_owned()),
        };
        if let Some(why) = why {
            reasons.insert(Reason::LinkEnvironmentUnknown);
            link_environment = Some(why);
        }
        if facts.toolchain_in_output {
            reasons.insert(Reason::ToolchainInOutput);
        }
    }
    if !matches!(facts.observed.place, Place::Anywhere) || facts.keyed_by_place {
        reasons.insert(Reason::Tied);
    }
    if !facts.searchable {
        reasons.insert(Reason::Tied);
        reasons.insert(Reason::Unsearchable);
    }
    for at in places(facts.observed) {
        match at {
            Located::Rooted(_) => {}
            Located::Shared(_) => {
                reasons.insert(Reason::ReadInSharedFolder);
            }
            Located::Outside(_) => {
                reasons.insert(Reason::ReadOutside);
            }
        }
    }
    for folder in facts.in_outputs {
        reasons.insert(Reason::in_output(*folder));
    }
    // The keys' bodies and the records, as text: a value that holds a machine's folder (a
    // variable the build script set, a flag, a message) names the machine whatever it is.
    let mut texts: Vec<String> = facts.records.to_vec();
    texts.extend(serde_json::to_string(facts.asked).ok());
    texts.extend(serde_json::to_string(facts.observed).ok());
    for (folder, needle) in facts.machine {
        if texts.iter().any(|text| text.contains(needle.as_str())) {
            reasons.insert(Reason::in_record(*folder));
        }
    }
    if facts.variable_names_base {
        reasons.insert(Reason::VariableNamesBase);
    }
    let mut externs: Vec<String> = facts
        .externs
        .iter()
        .filter(|(_, named)| !named)
        .map(|(name, _)| name.clone())
        .collect();
    externs.sort();
    externs.dedup();
    if !externs.is_empty() {
        reasons.insert(Reason::Extern);
    }
    if facts.native_cpu {
        reasons.insert(Reason::NativeCpu);
    }
    let mut variables: Vec<String> = facts.unkeyed.to_vec();
    variables.sort();
    variables.dedup();
    if !variables.is_empty() {
        reasons.insert(Reason::UnkeyedValue);
    }
    Portability {
        portable: reasons.is_empty(),
        reasons: reasons.into_iter().collect(),
        externs,
        variables,
        link_environment,
    }
}

/// A CI system, the variables that say a compile runs in it, and the exact names it documents as
/// its own.
#[derive(Debug)]
pub struct CiSystem {
    pub name: &'static str,
    /// Any of these set, and not empty, says the compile runs in this system.
    pub detected_by: &'static [&'static str],
    /// The variables it documents setting itself, by their exact names: never a prefix, since
    /// `BUILD_ENV`, `CI_REGION` or `AGENT_POOL` is what a project calls its own settings.
    pub names: &'static [&'static str],
}

/// The variables each CI system documents setting itself: GitHub Actions, GitLab, Buildkite,
/// CircleCI, Travis, Jenkins, Azure Pipelines, Bitbucket, Drone, TeamCity, AppVeyor and
/// CodeBuild. Their values (a server's URL, a ref, a run's number) are the same for every job of
/// a repository and turn up in crates by chance: `https://github.com` is in the doc comments of
/// nearly half the libraries on crates.io. A system's names are left out only where that system
/// is detected, so a laptop's `BUILD_NUMBER` or `WORKSPACE` is searched. A name that looks like
/// a secret (`GITHUB_TOKEN`, `CI_JOB_TOKEN`, `CI_COMMIT_AUTHOR`) is searched whatever this list
/// says and so is never in it, nor is a name a system sets under a documented prefix
/// (`BUILDKITE_AGENT_META_DATA_*`).
pub const SET_BY_CI: &[CiSystem] = &[
    CiSystem {
        name: "GitHub Actions",
        detected_by: &["GITHUB_ACTIONS"],
        names: &[
            "CI",
            "GITHUB_ACTION",
            "GITHUB_ACTION_PATH",
            "GITHUB_ACTION_REPOSITORY",
            "GITHUB_ACTIONS",
            "GITHUB_ACTOR",
            "GITHUB_ACTOR_ID",
            "GITHUB_API_URL",
            "GITHUB_BASE_REF",
            "GITHUB_ENV",
            "GITHUB_EVENT_NAME",
            "GITHUB_EVENT_PATH",
            "GITHUB_GRAPHQL_URL",
            "GITHUB_HEAD_REF",
            "GITHUB_JOB",
            "GITHUB_OUTPUT",
            "GITHUB_PATH",
            "GITHUB_REF",
            "GITHUB_REF_NAME",
            "GITHUB_REF_PROTECTED",
            "GITHUB_REF_TYPE",
            "GITHUB_REPOSITORY",
            "GITHUB_REPOSITORY_ID",
            "GITHUB_REPOSITORY_OWNER",
            "GITHUB_REPOSITORY_OWNER_ID",
            "GITHUB_RETENTION_DAYS",
            "GITHUB_RUN_ATTEMPT",
            "GITHUB_RUN_ID",
            "GITHUB_RUN_NUMBER",
            "GITHUB_SERVER_URL",
            "GITHUB_SHA",
            "GITHUB_STATE",
            "GITHUB_STEP_SUMMARY",
            "GITHUB_TRIGGERING_ACTOR",
            "GITHUB_WORKFLOW",
            "GITHUB_WORKFLOW_REF",
            "GITHUB_WORKFLOW_SHA",
            "GITHUB_WORKSPACE",
            "RUNNER_ARCH",
            "RUNNER_DEBUG",
            "RUNNER_ENVIRONMENT",
            "RUNNER_NAME",
            "RUNNER_OS",
            "RUNNER_TEMP",
            "RUNNER_TOOL_CACHE",
            "ACTIONS_RESULTS_URL",
            "ACTIONS_CACHE_URL",
            "ACTIONS_RUNNER_DEBUG",
            "ACTIONS_STEP_DEBUG",
        ],
    },
    CiSystem {
        name: "GitLab",
        detected_by: &["GITLAB_CI"],
        names: &[
            "CI",
            "GITLAB_CI",
            "GITLAB_FEATURES",
            "GITLAB_USER_EMAIL",
            "GITLAB_USER_ID",
            "GITLAB_USER_LOGIN",
            "GITLAB_USER_NAME",
            "CI_API_GRAPHQL_URL",
            "CI_API_V4_URL",
            "CI_BUILDS_DIR",
            "CI_COMMIT_BEFORE_SHA",
            "CI_COMMIT_BRANCH",
            "CI_COMMIT_DESCRIPTION",
            "CI_COMMIT_MESSAGE",
            "CI_COMMIT_REF_NAME",
            "CI_COMMIT_REF_PROTECTED",
            "CI_COMMIT_REF_SLUG",
            "CI_COMMIT_SHA",
            "CI_COMMIT_SHORT_SHA",
            "CI_COMMIT_TAG",
            "CI_COMMIT_TAG_MESSAGE",
            "CI_COMMIT_TIMESTAMP",
            "CI_COMMIT_TITLE",
            "CI_CONCURRENT_ID",
            "CI_CONCURRENT_PROJECT_ID",
            "CI_CONFIG_PATH",
            "CI_DEBUG_SERVICES",
            "CI_DEBUG_TRACE",
            "CI_DEFAULT_BRANCH",
            "CI_DEPENDENCY_PROXY_DIRECT_GROUP_IMAGE_PREFIX",
            "CI_DEPENDENCY_PROXY_GROUP_IMAGE_PREFIX",
            "CI_DEPENDENCY_PROXY_SERVER",
            "CI_DEPENDENCY_PROXY_USER",
            "CI_DEPLOY_FREEZE",
            "CI_DEPLOY_USER",
            "CI_DISPOSABLE_ENVIRONMENT",
            "CI_ENVIRONMENT_ACTION",
            "CI_ENVIRONMENT_NAME",
            "CI_ENVIRONMENT_SLUG",
            "CI_ENVIRONMENT_TIER",
            "CI_ENVIRONMENT_URL",
            "CI_EXTERNAL_PULL_REQUEST_IID",
            "CI_EXTERNAL_PULL_REQUEST_SOURCE_BRANCH_NAME",
            "CI_EXTERNAL_PULL_REQUEST_SOURCE_BRANCH_SHA",
            "CI_EXTERNAL_PULL_REQUEST_SOURCE_REPOSITORY",
            "CI_EXTERNAL_PULL_REQUEST_TARGET_BRANCH_NAME",
            "CI_EXTERNAL_PULL_REQUEST_TARGET_BRANCH_SHA",
            "CI_EXTERNAL_PULL_REQUEST_TARGET_REPOSITORY",
            "CI_GITLAB_FIPS_MODE",
            "CI_HAS_OPEN_REQUIREMENTS",
            "CI_JOB_ID",
            "CI_JOB_IMAGE",
            "CI_JOB_MANUAL",
            "CI_JOB_NAME",
            "CI_JOB_NAME_SLUG",
            "CI_JOB_STAGE",
            "CI_JOB_STARTED_AT",
            "CI_JOB_STATUS",
            "CI_JOB_TIMEOUT",
            "CI_JOB_URL",
            "CI_KUBERNETES_ACTIVE",
            "CI_MERGE_REQUEST_APPROVED",
            "CI_MERGE_REQUEST_ASSIGNEES",
            "CI_MERGE_REQUEST_DESCRIPTION",
            "CI_MERGE_REQUEST_DESCRIPTION_IS_TRUNCATED",
            "CI_MERGE_REQUEST_DIFF_BASE_SHA",
            "CI_MERGE_REQUEST_DIFF_ID",
            "CI_MERGE_REQUEST_DRAFT",
            "CI_MERGE_REQUEST_EVENT_TYPE",
            "CI_MERGE_REQUEST_ID",
            "CI_MERGE_REQUEST_IID",
            "CI_MERGE_REQUEST_LABELS",
            "CI_MERGE_REQUEST_MILESTONE",
            "CI_MERGE_REQUEST_PROJECT_ID",
            "CI_MERGE_REQUEST_PROJECT_PATH",
            "CI_MERGE_REQUEST_PROJECT_URL",
            "CI_MERGE_REQUEST_REF_PATH",
            "CI_MERGE_REQUEST_SOURCE_BRANCH_NAME",
            "CI_MERGE_REQUEST_SOURCE_BRANCH_PROTECTED",
            "CI_MERGE_REQUEST_SOURCE_BRANCH_SHA",
            "CI_MERGE_REQUEST_SOURCE_PROJECT_ID",
            "CI_MERGE_REQUEST_SOURCE_PROJECT_PATH",
            "CI_MERGE_REQUEST_SOURCE_PROJECT_URL",
            "CI_MERGE_REQUEST_SQUASH_ON_MERGE",
            "CI_MERGE_REQUEST_TARGET_BRANCH_NAME",
            "CI_MERGE_REQUEST_TARGET_BRANCH_PROTECTED",
            "CI_MERGE_REQUEST_TARGET_BRANCH_SHA",
            "CI_MERGE_REQUEST_TITLE",
            "CI_NODE_INDEX",
            "CI_NODE_TOTAL",
            "CI_OPEN_MERGE_REQUESTS",
            "CI_PAGES_DOMAIN",
            "CI_PAGES_HOSTNAME",
            "CI_PAGES_URL",
            "CI_PIPELINE_CREATED_AT",
            "CI_PIPELINE_ID",
            "CI_PIPELINE_IID",
            "CI_PIPELINE_NAME",
            "CI_PIPELINE_SCHEDULE_DESCRIPTION",
            "CI_PIPELINE_SOURCE",
            "CI_PIPELINE_TRIGGERED",
            "CI_PIPELINE_URL",
            "CI_PROJECT_CLASSIFICATION_LABEL",
            "CI_PROJECT_DESCRIPTION",
            "CI_PROJECT_DIR",
            "CI_PROJECT_ID",
            "CI_PROJECT_NAME",
            "CI_PROJECT_NAMESPACE",
            "CI_PROJECT_NAMESPACE_ID",
            "CI_PROJECT_NAMESPACE_SLUG",
            "CI_PROJECT_PATH",
            "CI_PROJECT_PATH_SLUG",
            "CI_PROJECT_REPOSITORY_LANGUAGES",
            "CI_PROJECT_ROOT_NAMESPACE",
            "CI_PROJECT_TITLE",
            "CI_PROJECT_URL",
            "CI_PROJECT_VISIBILITY",
            "CI_REGISTRY",
            "CI_REGISTRY_IMAGE",
            "CI_REGISTRY_USER",
            "CI_RELEASE_DESCRIPTION",
            "CI_REPOSITORY_URL",
            "CI_RUNNER_DESCRIPTION",
            "CI_RUNNER_EXECUTABLE_ARCH",
            "CI_RUNNER_ID",
            "CI_RUNNER_REVISION",
            "CI_RUNNER_TAGS",
            "CI_RUNNER_VERSION",
            "CI_SERVER",
            "CI_SERVER_FQDN",
            "CI_SERVER_HOST",
            "CI_SERVER_NAME",
            "CI_SERVER_PORT",
            "CI_SERVER_PROTOCOL",
            "CI_SERVER_REVISION",
            "CI_SERVER_SHELL_SSH_HOST",
            "CI_SERVER_SHELL_SSH_PORT",
            "CI_SERVER_TLS_CA_FILE",
            "CI_SERVER_TLS_CERT_FILE",
            "CI_SERVER_URL",
            "CI_SERVER_VERSION",
            "CI_SERVER_VERSION_MAJOR",
            "CI_SERVER_VERSION_MINOR",
            "CI_SERVER_VERSION_PATCH",
            "CI_SHARED_ENVIRONMENT",
            "CI_TEMPLATE_REGISTRY_HOST",
        ],
    },
    CiSystem {
        name: "Buildkite",
        detected_by: &["BUILDKITE"],
        names: &[
            "CI",
            "BUILDKITE",
            "BUILDKITE_AGENT_ID",
            "BUILDKITE_AGENT_NAME",
            "BUILDKITE_BRANCH",
            "BUILDKITE_BUILD_CHECKOUT_PATH",
            "BUILDKITE_BUILD_CREATOR",
            "BUILDKITE_BUILD_CREATOR_EMAIL",
            "BUILDKITE_BUILD_ID",
            "BUILDKITE_BUILD_NUMBER",
            "BUILDKITE_BUILD_PATH",
            "BUILDKITE_BUILD_URL",
            "BUILDKITE_COMMAND",
            "BUILDKITE_COMMIT",
            "BUILDKITE_JOB_ID",
            "BUILDKITE_LABEL",
            "BUILDKITE_MESSAGE",
            "BUILDKITE_ORGANIZATION_SLUG",
            "BUILDKITE_PARALLEL_JOB",
            "BUILDKITE_PARALLEL_JOB_COUNT",
            "BUILDKITE_PIPELINE_DEFAULT_BRANCH",
            "BUILDKITE_PIPELINE_ID",
            "BUILDKITE_PIPELINE_NAME",
            "BUILDKITE_PIPELINE_PROVIDER",
            "BUILDKITE_PIPELINE_SLUG",
            "BUILDKITE_PULL_REQUEST",
            "BUILDKITE_PULL_REQUEST_BASE_BRANCH",
            "BUILDKITE_PULL_REQUEST_REPO",
            "BUILDKITE_REBUILT_FROM_BUILD_ID",
            "BUILDKITE_REBUILT_FROM_BUILD_NUMBER",
            "BUILDKITE_REPO",
            "BUILDKITE_RETRY_COUNT",
            "BUILDKITE_SOURCE",
            "BUILDKITE_STEP_ID",
            "BUILDKITE_TAG",
            "BUILDKITE_TIMEOUT",
            "BUILDKITE_TRIGGERED_FROM_BUILD_ID",
            "BUILDKITE_TRIGGERED_FROM_BUILD_NUMBER",
            "BUILDKITE_TRIGGERED_FROM_BUILD_PIPELINE_SLUG",
        ],
    },
    CiSystem {
        name: "CircleCI",
        detected_by: &["CIRCLECI"],
        names: &[
            "CI",
            "CIRCLECI",
            "CIRCLE_BRANCH",
            "CIRCLE_BUILD_NUM",
            "CIRCLE_BUILD_URL",
            "CIRCLE_JOB",
            "CIRCLE_NODE_INDEX",
            "CIRCLE_NODE_TOTAL",
            "CIRCLE_PR_NUMBER",
            "CIRCLE_PR_REPONAME",
            "CIRCLE_PR_USERNAME",
            "CIRCLE_PREVIOUS_BUILD_NUM",
            "CIRCLE_PROJECT_REPONAME",
            "CIRCLE_PROJECT_USERNAME",
            "CIRCLE_PULL_REQUEST",
            "CIRCLE_PULL_REQUESTS",
            "CIRCLE_REPOSITORY_URL",
            "CIRCLE_SHA1",
            "CIRCLE_TAG",
            "CIRCLE_USERNAME",
            "CIRCLE_WORKFLOW_ID",
            "CIRCLE_WORKFLOW_JOB_ID",
            "CIRCLE_WORKFLOW_WORKSPACE_ID",
            "CIRCLE_WORKING_DIRECTORY",
        ],
    },
    CiSystem {
        name: "Travis CI",
        detected_by: &["TRAVIS"],
        names: &[
            "CI",
            "CONTINUOUS_INTEGRATION",
            "TRAVIS",
            "TRAVIS_ALLOW_FAILURE",
            "TRAVIS_APP_HOST",
            "TRAVIS_BRANCH",
            "TRAVIS_BUILD_DIR",
            "TRAVIS_BUILD_ID",
            "TRAVIS_BUILD_NUMBER",
            "TRAVIS_BUILD_STAGE_NAME",
            "TRAVIS_BUILD_WEB_URL",
            "TRAVIS_COMMIT",
            "TRAVIS_COMMIT_MESSAGE",
            "TRAVIS_COMMIT_RANGE",
            "TRAVIS_COMPILER",
            "TRAVIS_CPU_ARCH",
            "TRAVIS_DEBUG_MODE",
            "TRAVIS_DIST",
            "TRAVIS_EVENT_TYPE",
            "TRAVIS_JOB_ID",
            "TRAVIS_JOB_NAME",
            "TRAVIS_JOB_NUMBER",
            "TRAVIS_JOB_WEB_URL",
            "TRAVIS_OS_NAME",
            "TRAVIS_OSX_IMAGE",
            "TRAVIS_PULL_REQUEST",
            "TRAVIS_PULL_REQUEST_BRANCH",
            "TRAVIS_PULL_REQUEST_SHA",
            "TRAVIS_PULL_REQUEST_SLUG",
            "TRAVIS_REPO_SLUG",
            "TRAVIS_SECURE_ENV_VARS",
            "TRAVIS_SUDO",
            "TRAVIS_TAG",
            "TRAVIS_TEST_RESULT",
        ],
    },
    CiSystem {
        name: "Jenkins",
        detected_by: &["JENKINS_URL", "JENKINS_HOME"],
        names: &[
            "BRANCH_IS_PRIMARY",
            "BRANCH_NAME",
            "BUILD_DISPLAY_NAME",
            "BUILD_ID",
            "BUILD_NUMBER",
            "BUILD_TAG",
            "BUILD_URL",
            "CHANGE_BRANCH",
            "CHANGE_FORK",
            "CHANGE_ID",
            "CHANGE_TARGET",
            "CHANGE_TITLE",
            "CHANGE_URL",
            "EXECUTOR_NUMBER",
            "GIT_BRANCH",
            "GIT_COMMIT",
            "GIT_LOCAL_BRANCH",
            "GIT_PREVIOUS_COMMIT",
            "GIT_PREVIOUS_SUCCESSFUL_COMMIT",
            "GIT_URL",
            "HUDSON_HOME",
            "HUDSON_URL",
            "JENKINS_HOME",
            "JENKINS_URL",
            "JOB_BASE_NAME",
            "JOB_DISPLAY_URL",
            "JOB_NAME",
            "JOB_URL",
            "NODE_LABELS",
            "NODE_NAME",
            "RUN_ARTIFACTS_DISPLAY_URL",
            "RUN_CHANGES_DISPLAY_URL",
            "RUN_DISPLAY_URL",
            "RUN_TESTS_DISPLAY_URL",
            "STAGE_NAME",
            "TAG_DATE",
            "TAG_NAME",
            "TAG_TIMESTAMP",
            "TAG_UNIXTIME",
            "WORKSPACE",
            "WORKSPACE_TMP",
        ],
    },
    CiSystem {
        name: "Azure Pipelines",
        detected_by: &["TF_BUILD"],
        names: &[
            "TF_BUILD",
            "AGENT_BUILDDIRECTORY",
            "AGENT_CONTAINERMAPPING",
            "AGENT_HOMEDIRECTORY",
            "AGENT_ID",
            "AGENT_ISSELFHOSTED",
            "AGENT_JOBNAME",
            "AGENT_JOBSTATUS",
            "AGENT_MACHINENAME",
            "AGENT_NAME",
            "AGENT_OS",
            "AGENT_OSARCHITECTURE",
            "AGENT_TEMPDIRECTORY",
            "AGENT_TOOLSDIRECTORY",
            "AGENT_WORKFOLDER",
            "BUILD_ARTIFACTSTAGINGDIRECTORY",
            "BUILD_BINARIESDIRECTORY",
            "BUILD_BUILDID",
            "BUILD_BUILDNUMBER",
            "BUILD_BUILDURI",
            "BUILD_CONTAINERID",
            "BUILD_CRONSCHEDULE_DISPLAYNAME",
            "BUILD_DEFINITIONNAME",
            "BUILD_DEFINITIONVERSION",
            "BUILD_QUEUEDBY",
            "BUILD_QUEUEDBYID",
            "BUILD_REASON",
            "BUILD_REPOSITORY_CLEAN",
            "BUILD_REPOSITORY_GIT_SUBMODULECHECKOUT",
            "BUILD_REPOSITORY_ID",
            "BUILD_REPOSITORY_LOCALPATH",
            "BUILD_REPOSITORY_NAME",
            "BUILD_REPOSITORY_PROVIDER",
            "BUILD_REPOSITORY_TFVC_WORKSPACE",
            "BUILD_REPOSITORY_URI",
            "BUILD_REQUESTEDFOR",
            "BUILD_REQUESTEDFOREMAIL",
            "BUILD_REQUESTEDFORID",
            "BUILD_SOURCEBRANCH",
            "BUILD_SOURCEBRANCHNAME",
            "BUILD_SOURCESDIRECTORY",
            "BUILD_SOURCEVERSION",
            "BUILD_SOURCEVERSIONMESSAGE",
            "BUILD_STAGINGDIRECTORY",
            "BUILD_TRIGGEREDBY_BUILDID",
            "BUILD_TRIGGEREDBY_BUILDNUMBER",
            "BUILD_TRIGGEREDBY_DEFINITIONID",
            "BUILD_TRIGGEREDBY_DEFINITIONNAME",
            "BUILD_TRIGGEREDBY_PROJECTID",
            "COMMON_TESTRESULTSDIRECTORY",
            "ENVIRONMENT_ID",
            "ENVIRONMENT_NAME",
            "ENVIRONMENT_RESOURCEID",
            "ENVIRONMENT_RESOURCENAME",
            "PIPELINE_WORKSPACE",
            "STRATEGY_CYCLENAME",
            "STRATEGY_NAME",
            "SYSTEM_ARTIFACTSDIRECTORY",
            "SYSTEM_COLLECTIONID",
            "SYSTEM_COLLECTIONURI",
            "SYSTEM_CULTURE",
            "SYSTEM_DEBUG",
            "SYSTEM_DEFAULTWORKINGDIRECTORY",
            "SYSTEM_DEFINITIONID",
            "SYSTEM_DEFINITIONNAME",
            "SYSTEM_HOSTTYPE",
            "SYSTEM_ISSCHEDULED",
            "SYSTEM_JOBATTEMPT",
            "SYSTEM_JOBDISPLAYNAME",
            "SYSTEM_JOBID",
            "SYSTEM_JOBNAME",
            "SYSTEM_JOBPOSITIONINPHASE",
            "SYSTEM_PARALLELEXECUTIONTYPE",
            "SYSTEM_PHASEATTEMPT",
            "SYSTEM_PHASEDISPLAYNAME",
            "SYSTEM_PHASENAME",
            "SYSTEM_PLANID",
            "SYSTEM_PULLREQUEST_ISFORK",
            "SYSTEM_PULLREQUEST_PULLREQUESTID",
            "SYSTEM_PULLREQUEST_PULLREQUESTNUMBER",
            "SYSTEM_PULLREQUEST_SOURCEBRANCH",
            "SYSTEM_PULLREQUEST_SOURCECOMMITID",
            "SYSTEM_PULLREQUEST_SOURCEREPOSITORYURI",
            "SYSTEM_PULLREQUEST_TARGETBRANCH",
            "SYSTEM_PULLREQUEST_TARGETBRANCHNAME",
            "SYSTEM_SERVERTYPE",
            "SYSTEM_STAGEATTEMPT",
            "SYSTEM_STAGEDISPLAYNAME",
            "SYSTEM_STAGENAME",
            "SYSTEM_TASKDEFINITIONSURI",
            "SYSTEM_TEAMFOUNDATIONCOLLECTIONURI",
            "SYSTEM_TEAMFOUNDATIONSERVERURI",
            "SYSTEM_TEAMPROJECT",
            "SYSTEM_TEAMPROJECTID",
            "SYSTEM_TIMELINEID",
            "SYSTEM_TOTALJOBSINPHASE",
            "SYSTEM_WORKFOLDER",
        ],
    },
    CiSystem {
        name: "Bitbucket Pipelines",
        detected_by: &["BITBUCKET_BUILD_NUMBER"],
        names: &[
            "CI",
            "BITBUCKET_BOOKMARK",
            "BITBUCKET_BRANCH",
            "BITBUCKET_BUILD_NUMBER",
            "BITBUCKET_CLONE_DIR",
            "BITBUCKET_COMMIT",
            "BITBUCKET_DEPLOYMENT_ENVIRONMENT",
            "BITBUCKET_DEPLOYMENT_ENVIRONMENT_UUID",
            "BITBUCKET_DOCKER_HOST_INTERNAL",
            "BITBUCKET_EXIT_CODE",
            "BITBUCKET_GIT_HTTP_ORIGIN",
            "BITBUCKET_GIT_SSH_ORIGIN",
            "BITBUCKET_PARALLEL_STEP",
            "BITBUCKET_PARALLEL_STEP_COUNT",
            "BITBUCKET_PIPELINE_UUID",
            "BITBUCKET_PR_DESTINATION_BRANCH",
            "BITBUCKET_PR_DESTINATION_COMMIT",
            "BITBUCKET_PR_ID",
            "BITBUCKET_PROJECT_UUID",
            "BITBUCKET_REPO_FULL_NAME",
            "BITBUCKET_REPO_OWNER",
            "BITBUCKET_REPO_OWNER_UUID",
            "BITBUCKET_REPO_SLUG",
            "BITBUCKET_REPO_UUID",
            "BITBUCKET_STEP_TRIGGERER_UUID",
            "BITBUCKET_STEP_UUID",
            "BITBUCKET_TAG",
            "BITBUCKET_WORKSPACE",
        ],
    },
    CiSystem {
        name: "Drone",
        detected_by: &["DRONE"],
        names: &[
            "CI",
            "DRONE",
            "DRONE_BRANCH",
            "DRONE_BUILD_ACTION",
            "DRONE_BUILD_CREATED",
            "DRONE_BUILD_EVENT",
            "DRONE_BUILD_FINISHED",
            "DRONE_BUILD_LINK",
            "DRONE_BUILD_NUMBER",
            "DRONE_BUILD_PARENT",
            "DRONE_BUILD_STARTED",
            "DRONE_BUILD_STATUS",
            "DRONE_CALVER",
            "DRONE_COMMIT",
            "DRONE_COMMIT_AFTER",
            "DRONE_COMMIT_BEFORE",
            "DRONE_COMMIT_BRANCH",
            "DRONE_COMMIT_LINK",
            "DRONE_COMMIT_MESSAGE",
            "DRONE_COMMIT_REF",
            "DRONE_COMMIT_SHA",
            "DRONE_DEPLOY_TO",
            "DRONE_FAILED_STAGES",
            "DRONE_FAILED_STEPS",
            "DRONE_GIT_HTTP_URL",
            "DRONE_GIT_SSH_URL",
            "DRONE_PULL_REQUEST",
            "DRONE_PULL_REQUEST_TITLE",
            "DRONE_REMOTE_URL",
            "DRONE_REPO",
            "DRONE_REPO_BRANCH",
            "DRONE_REPO_LINK",
            "DRONE_REPO_NAME",
            "DRONE_REPO_NAMESPACE",
            "DRONE_REPO_OWNER",
            "DRONE_REPO_SCM",
            "DRONE_REPO_VISIBILITY",
            "DRONE_SEMVER",
            "DRONE_SEMVER_BUILD",
            "DRONE_SEMVER_ERROR",
            "DRONE_SEMVER_MAJOR",
            "DRONE_SEMVER_MINOR",
            "DRONE_SEMVER_PATCH",
            "DRONE_SEMVER_PRERELEASE",
            "DRONE_SEMVER_SHORT",
            "DRONE_SOURCE_BRANCH",
            "DRONE_STAGE_ARCH",
            "DRONE_STAGE_DEPENDS_ON",
            "DRONE_STAGE_FINISHED",
            "DRONE_STAGE_KIND",
            "DRONE_STAGE_MACHINE",
            "DRONE_STAGE_NAME",
            "DRONE_STAGE_NUMBER",
            "DRONE_STAGE_OS",
            "DRONE_STAGE_STARTED",
            "DRONE_STAGE_STATUS",
            "DRONE_STAGE_TYPE",
            "DRONE_STAGE_VARIANT",
            "DRONE_STEP_NAME",
            "DRONE_STEP_NUMBER",
            "DRONE_SYSTEM_HOST",
            "DRONE_SYSTEM_HOSTNAME",
            "DRONE_SYSTEM_PROTO",
            "DRONE_SYSTEM_VERSION",
            "DRONE_TAG",
            "DRONE_TARGET_BRANCH",
            "DRONE_WORKSPACE",
        ],
    },
    CiSystem {
        name: "TeamCity",
        detected_by: &["TEAMCITY_VERSION"],
        names: &[
            "BUILD_IS_PERSONAL",
            "BUILD_NUMBER",
            "BUILD_VCS_NUMBER",
            "TEAMCITY_BUILDCONF_NAME",
            "TEAMCITY_BUILD_PROPERTIES_FILE",
            "TEAMCITY_CAPTURE_ENV",
            "TEAMCITY_GIT_PATH",
            "TEAMCITY_PROJECT_NAME",
            "TEAMCITY_VERSION",
        ],
    },
    CiSystem {
        name: "AppVeyor",
        detected_by: &["APPVEYOR"],
        names: &[
            "CI",
            "APPVEYOR",
            "APPVEYOR_ACCOUNT_NAME",
            "APPVEYOR_API_URL",
            "APPVEYOR_BUILD_FOLDER",
            "APPVEYOR_BUILD_ID",
            "APPVEYOR_BUILD_NUMBER",
            "APPVEYOR_BUILD_VERSION",
            "APPVEYOR_BUILD_WORKER_IMAGE",
            "APPVEYOR_FORCED_BUILD",
            "APPVEYOR_JOB_ID",
            "APPVEYOR_JOB_NAME",
            "APPVEYOR_JOB_NUMBER",
            "APPVEYOR_PROJECT_ID",
            "APPVEYOR_PROJECT_NAME",
            "APPVEYOR_PROJECT_SLUG",
            "APPVEYOR_PULL_REQUEST_HEAD_COMMIT",
            "APPVEYOR_PULL_REQUEST_HEAD_REPO_BRANCH",
            "APPVEYOR_PULL_REQUEST_HEAD_REPO_NAME",
            "APPVEYOR_PULL_REQUEST_NUMBER",
            "APPVEYOR_PULL_REQUEST_TITLE",
            "APPVEYOR_REPO_BRANCH",
            "APPVEYOR_REPO_COMMIT",
            "APPVEYOR_REPO_COMMIT_MESSAGE",
            "APPVEYOR_REPO_COMMIT_MESSAGE_EXTENDED",
            "APPVEYOR_REPO_COMMIT_TIMESTAMP",
            "APPVEYOR_REPO_NAME",
            "APPVEYOR_REPO_PROVIDER",
            "APPVEYOR_REPO_SCM",
            "APPVEYOR_REPO_TAG",
            "APPVEYOR_REPO_TAG_NAME",
            "APPVEYOR_RE_BUILD",
            "APPVEYOR_RE_RUN_INCOMPLETE",
            "APPVEYOR_SCHEDULED_BUILD",
            "APPVEYOR_URL",
        ],
    },
    CiSystem {
        name: "AWS CodeBuild",
        detected_by: &["CODEBUILD_BUILD_ID"],
        names: &[
            "CODEBUILD_BATCH_BUILD_IDENTIFIER",
            "CODEBUILD_BUILD_ARN",
            "CODEBUILD_BUILD_ID",
            "CODEBUILD_BUILD_IMAGE",
            "CODEBUILD_BUILD_NUMBER",
            "CODEBUILD_BUILD_SUCCEEDING",
            "CODEBUILD_INITIATOR",
            "CODEBUILD_LOG_PATH",
            "CODEBUILD_PUBLIC_BUILD_URL",
            "CODEBUILD_RESOLVED_SOURCE_VERSION",
            "CODEBUILD_SOURCE_REPO_URL",
            "CODEBUILD_SOURCE_VERSION",
            "CODEBUILD_SRC_DIR",
            "CODEBUILD_START_TIME",
            "CODEBUILD_WEBHOOK_ACTOR_ACCOUNT_ID",
            "CODEBUILD_WEBHOOK_BASE_REF",
            "CODEBUILD_WEBHOOK_EVENT",
            "CODEBUILD_WEBHOOK_HEAD_REF",
            "CODEBUILD_WEBHOOK_MERGE_COMMIT",
            "CODEBUILD_WEBHOOK_PREV_COMMIT",
            "CODEBUILD_WEBHOOK_TRIGGER",
        ],
    },
];

/// The variables the shell sets for every command: `_` (the last command's path), `PWD` and
/// `OLDPWD` (the folders it was in). Their values name where a command was started, not what
/// it builds.
const SET_BY_SHELL: &[&str] = &["_", "PWD", "OLDPWD"];

/// The CI systems a compile's environment says it runs in: whose documented names
/// [`unkeyed_candidate`] leaves out.
#[derive(Debug, Default)]
pub struct SetByCi(Vec<&'static CiSystem>);

impl SetByCi {
    /// The systems detected by `get`, the compile's environment.
    pub fn detect(get: impl Fn(&str) -> Option<String>) -> SetByCi {
        SetByCi(
            SET_BY_CI
                .iter()
                .filter(|system| {
                    system
                        .detected_by
                        .iter()
                        .any(|name| get(name).is_some_and(|value| !value.is_empty()))
                })
                .collect(),
        )
    }

    /// Whether a detected system documents `name` as its own.
    pub fn sets(&self, name: &str) -> bool {
        self.0.iter().any(|system| system.names.contains(&name))
    }
}

/// The prefixes of the variables the build tools set for a compile, and hushrun's own: cargo's
/// (the ones B-5 keys are skipped as keyed; the rest name folders, flags or jobs), rustc's and
/// rustup's. A name among them that looks like a secret is searched all the same.
const SET_BY_TOOLS: &[&str] = &[
    "CARGO", "__CARGO", "RUSTC", "RUSTDOC", "RUSTUP_", "RUST_", "OUT_DIR", "HUSHRUN_",
];

/// The least bytes a value of a variable that looks like a secret must have to be searched
/// for: a shorter one turns up in outputs by chance.
pub const SECRET_MIN_BYTES: usize = 8;
/// The least bytes a value of any other variable must have to be searched for (C3): the same
/// 8, since short config values (a region, an environment's name, a version) are what a build
/// script or a proc macro reads without saying. A shorter value is the documented limit that
/// only the audit and `observe` catch (B-24).
pub const OTHER_MIN_BYTES: usize = 8;

/// Whether an output holding the value `value` of the variable `name`, which is not in the
/// compile's key, keeps its block on this machine (B-24): a name that looks like a secret (E-12)
/// with a value of [`SECRET_MIN_BYTES`] or more, or any variable that neither a detected CI
/// system (`ci`) documents as its own, nor a build tool or the shell set, with a value of
/// [`OTHER_MIN_BYTES`] or more. The caller leaves out values that hold a root or a folder of this
/// machine, which the search already finds by their own rules.
pub fn unkeyed_candidate(name: &str, value: &[u8], ci: &SetByCi) -> bool {
    if hushrun_core::secret_names::is_secret_name(name) {
        return value.len() >= SECRET_MIN_BYTES;
    }
    let set_by_others = ci.sets(name)
        || SET_BY_SHELL.contains(&name)
        || SET_BY_TOOLS.iter().any(|prefix| name.starts_with(prefix));
    !set_by_others && value.len() >= OTHER_MIN_BYTES
}

/// Every place the second key names: reads, native places and folders, shapes, what was
/// declared and, for a linked output, what the linker read.
fn places(observed: &Observed) -> impl Iterator<Item = &Located> {
    let declared = observed.declared.iter().flat_map(|d| {
        d.files
            .iter()
            .map(|r| &r.at)
            .chain(d.above.iter().map(|a| &a.below))
    });
    let link = observed.link.iter().flat_map(|l| {
        l.files
            .iter()
            .map(|r| &r.at)
            .chain(l.libraries.iter().map(|lib| &lib.at))
    });
    observed
        .reads
        .iter()
        .map(|r| &r.at)
        .chain(observed.native.iter().map(|r| &r.at))
        .chain(observed.native_dirs.iter().map(|d| &d.dir))
        .chain(observed.shape.iter().map(|s| &s.dir))
        .chain(declared)
        .chain(link)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hushrun_core::Digest;

    use super::*;
    use crate::key::{Read, ReadState};
    use crate::model::{RootName, Rooted};

    fn asked() -> Asked {
        Asked {
            compiler: Digest::of(b"compiler"),
            args: vec![
                "--crate-name=serde".into(),
                "{hushrun:cargo-home}/x.rs".into(),
            ],
            env: BTreeMap::from([(
                "CARGO_MANIFEST_DIR".into(),
                "{hushrun:cargo-home}/registry/src/i/serde-1.0.0".into(),
            )]),
            added: Vec::new(),
        }
    }

    fn observed() -> Observed {
        Observed {
            step: Digest::of(b"step"),
            reads: vec![Read {
                at: Located::Rooted(Rooted {
                    root: RootName::CargoHome,
                    rest: "registry/src/i/serde-1.0.0/src/lib.rs".into(),
                }),
                state: ReadState::Present(Digest::of(b"lib")),
            }],
            env: vec![],
            externs: vec![],
            native: vec![],
            native_dirs: vec![],
            bundles_nothing: false,
            bound: Vec::new(),
            shape: vec![],
            generated: None,
            declared: None,
            link: None,
            place: Place::Anywhere,
        }
    }

    fn machine() -> Vec<(Folder, String)> {
        vec![
            (Folder::CargoHome, "/home/dev/.cargo".into()),
            (Folder::RustupHome, "/home/dev/.rustup".into()),
            (Folder::Home, "/home/dev".into()),
        ]
    }

    fn decide(
        class: Class,
        asked: &Asked,
        observed: &Observed,
        in_outputs: &[Folder],
        records: &[String],
        externs: &[(String, bool)],
    ) -> Portability {
        let machine = machine();
        let in_outputs: BTreeSet<Folder> = in_outputs.iter().copied().collect();
        portable(&Facts {
            class,
            asked,
            observed,
            records,
            in_outputs: &in_outputs,
            machine: &machine,
            variable_names_base: false,
            externs,
            unkeyed: &[],
            native_cpu: false,
            toolchain_in_output: false,
            keyed_by_place: false,
            searchable: true,
        })
    }

    #[test]
    fn a_library_named_from_its_bases_is_portable() {
        let p = decide(Class::Library, &asked(), &observed(), &[], &[], &[]);
        assert!(p.portable, "{p:?}");
        assert!(p.reasons.is_empty());
        assert!(p.named_portably(Class::Library));
    }

    #[test]
    fn each_reason_holds_on_its_own() {
        let a = asked();
        let o = observed();
        let reasons = |p: Portability| {
            p.reasons
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        assert_eq!(
            reasons(decide(Class::ProcMacro, &a, &o, &[], &[], &[])),
            "link-environment-unknown"
        );
        assert!(decide(Class::ProcMacro, &a, &o, &[], &[], &[]).named_portably(Class::ProcMacro));
        // A proc macro whose bytes hold the rustup home (its link names the standard library)
        // is still named alike elsewhere; a library whose bytes hold it is not.
        let in_bytes = decide(Class::ProcMacro, &a, &o, &[Folder::RustupHome], &[], &[]);
        assert!(!in_bytes.portable && in_bytes.named_portably(Class::ProcMacro));
        let library = decide(Class::Library, &a, &o, &[Folder::RustupHome], &[], &[]);
        assert!(!library.named_portably(Class::Library));
        assert!(!decide(Class::Program, &a, &o, &[], &[], &[]).named_portably(Class::Program));
        let mut tied = observed();
        tied.place = Place::Tied(vec![(RootName::Workspace, "/w".into())]);
        assert!(
            !decide(Class::ProcMacro, &a, &tied, &[], &[], &[]).named_portably(Class::ProcMacro)
        );
        let p = decide(Class::Library, &a, &tied, &[], &[], &[]);
        assert!(!p.portable && !p.named_portably(Class::Library));
        assert_eq!(reasons(p), "tied");
        let mut shared = observed();
        shared.reads[0].at = Located::Shared("/usr/include/zlib.h".into());
        assert_eq!(
            reasons(decide(Class::Library, &a, &shared, &[], &[], &[])),
            "read-in-shared-folder"
        );
        let mut outside = observed();
        outside.reads[0].at = Located::Outside("/etc/hosts".into());
        assert_eq!(
            reasons(decide(Class::Library, &a, &outside, &[], &[], &[])),
            "read-outside"
        );
        assert_eq!(
            reasons(decide(
                Class::Library,
                &a,
                &o,
                &[Folder::CargoHome, Folder::Home],
                &[],
                &[]
            )),
            "cargo-home-in-output,home-in-output"
        );
        // A variable the build script set that holds the home folder, in the first key.
        let mut env = asked();
        env.env
            .insert("PROTOC".into(), "/home/dev/bin/protoc".into());
        assert_eq!(
            reasons(decide(Class::Library, &env, &o, &[], &[], &[])),
            "home-in-record"
        );
        // A message stored with the block that names the rustup home.
        let records = ["/home/dev/.rustup/toolchains/x/lib.rs".to_string()];
        assert_eq!(
            reasons(decide(Class::Library, &a, &o, &[], &records, &[])),
            "rustup-home-in-record,home-in-record"
        );
        let p = decide(
            Class::Library,
            &a,
            &o,
            &[],
            &[],
            &[("serde_derive".into(), false), ("itoa".into(), true)],
        );
        assert_eq!(p.externs, ["serde_derive"]);
        assert_eq!(reasons(p), "extern");
        let machine = machine();
        let p = portable(&Facts {
            class: Class::Library,
            asked: &a,
            observed: &o,
            records: &[],
            in_outputs: &BTreeSet::new(),
            machine: &machine,
            variable_names_base: true,
            externs: &[],
            unkeyed: &[],
            native_cpu: false,
            toolchain_in_output: false,
            keyed_by_place: false,
            searchable: true,
        });
        assert_eq!(reasons(p), "variable-names-base");
    }

    /// `observed()` linked, with E known (`Some`) or unknown with a reason.
    fn linked(environment: Option<u8>) -> Observed {
        let mut o = observed();
        o.link = Some(crate::key::Link {
            libraries: Vec::new(),
            files: Vec::new(),
            linker: String::new(),
            sdk: String::new(),
            environment: environment.map(|byte| Digest::of(&[byte])),
            environment_list: Some(match environment {
                Some(byte) => crate::linkenv::LinkEnv::Known {
                    digest: Digest::of(&[byte]),
                    items: Vec::new(),
                },
                None => crate::linkenv::LinkEnv::Unknown {
                    reason: "LD_PRELOAD is set for the link".into(),
                },
            }),
            linker_path: Located::Shared("/usr/bin/cc".into()),
            ld_path: None,
            sdk_path: None,
            dsymutil_path: None,
            folders: Vec::new(),
            system: Vec::new(),
            query: None,
        });
        o
    }

    // covers: B-24, B-37, B-10
    #[test]
    fn a_linked_output_leaves_with_its_link_environment_known_and_no_machine_folder() {
        let a = asked();
        let machine = machine();
        let decide = |class, observed: &Observed, toolchain_in_output| {
            portable(&Facts {
                class,
                asked: &a,
                observed,
                records: &[],
                in_outputs: &BTreeSet::new(),
                machine: &machine,
                variable_names_base: false,
                externs: &[],
                unkeyed: &[],
                native_cpu: false,
                toolchain_in_output,
                keyed_by_place: false,
                searchable: true,
            })
        };
        for class in [Class::ProcMacro, Class::BuildScript, Class::Program] {
            let known = decide(class, &linked(Some(1)), false);
            assert!(known.portable, "{class:?}: {known:?}");
            assert_eq!(known.link_environment, None);
            let unknown = decide(class, &linked(None), false);
            assert_eq!(unknown.reasons, [Reason::LinkEnvironmentUnknown]);
            assert_eq!(
                unknown.link_environment.as_deref(),
                Some("LD_PRELOAD is set for the link")
            );
            let toolchain = decide(class, &linked(Some(1)), true);
            assert_eq!(toolchain.reasons, [Reason::ToolchainInOutput], "{class:?}");
            // A program that bakes its checkout stays tied (decision 12).
            let mut tied = linked(Some(1));
            tied.place = Place::Tied(vec![(RootName::Workspace, "/w".into())]);
            assert_eq!(decide(class, &tied, false).reasons, [Reason::Tied]);
        }
        // A proc macro is named alike on another machine whatever its own link environment or
        // what its bytes hold of the toolchain: neither is in that name (B-6).
        assert!(decide(Class::ProcMacro, &linked(None), true).named_portably(Class::ProcMacro));
        // A library holding the toolchain's folder is not searched for it: no such fact.
        assert!(decide(Class::Library, &observed(), true).portable);
    }

    // covers: B-24
    #[test]
    fn a_short_home_names_the_machine_in_a_record_wherever_it_is_found() {
        let machine = vec![
            (Folder::CargoHome, "/root/.cargo".to_string()),
            (Folder::Home, "/root".to_string()),
        ];
        let decide = |records: &[String]| {
            portable(&Facts {
                class: Class::Library,
                asked: &asked(),
                observed: &observed(),
                records,
                in_outputs: &BTreeSet::new(),
                machine: &machine,
                variable_names_base: false,
                externs: &[],
                unkeyed: &[],
                native_cpu: false,
                toolchain_in_output: false,
                keyed_by_place: false,
                searchable: true,
            })
        };
        // `/rootfs` may be a crate's own text, or this home and more: the safe side is to say
        // the block names the machine, at the price of reuse elsewhere.
        let p = decide(&["warning: see src/roots.rs and /rootfs".to_string()]);
        assert_eq!(p.reasons, vec![Reason::HomeInRecord]);
        let p = decide(&["warning: wrote /root/out.txt".to_string()]);
        assert_eq!(p.reasons, vec![Reason::HomeInRecord]);
        let p = decide(&["CFLAGS=-I/root/include".to_string()]);
        assert_eq!(p.reasons, vec![Reason::HomeInRecord]);
    }

    // covers: B-24
    #[test]
    fn a_machine_folder_in_a_record_names_the_machine_whatever_follows_it() {
        let machine = vec![(Folder::CargoHome, "/opt/tools/cargo".to_string())];
        let records = ["CACHE=/opt/tools/cargo_cache".to_string()];
        let p = portable(&Facts {
            class: Class::Library,
            asked: &asked(),
            observed: &observed(),
            records: &records,
            in_outputs: &BTreeSet::new(),
            machine: &machine,
            variable_names_base: false,
            externs: &[],
            unkeyed: &[],
            native_cpu: false,
            toolchain_in_output: false,
            keyed_by_place: false,
            searchable: true,
        });
        assert_eq!(p.reasons, vec![Reason::CargoHomeInRecord]);
    }

    #[test]
    fn the_reasons_are_written_as_they_are_named() {
        for reason in [
            Reason::LinkEnvironmentUnknown,
            Reason::ToolchainInOutput,
            Reason::Tied,
            Reason::ReadOutside,
            Reason::ReadInSharedFolder,
            Reason::CargoHomeInOutput,
            Reason::RustupHomeInOutput,
            Reason::HomeInOutput,
            Reason::CargoHomeInRecord,
            Reason::RustupHomeInRecord,
            Reason::HomeInRecord,
            Reason::VariableNamesBase,
            Reason::Extern,
            Reason::UnkeyedValue,
            Reason::NativeCpu,
        ] {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::Value::String(reason.as_str().into())
            );
        }
    }

    // covers: B-24
    #[test]
    fn a_value_is_searched_for_when_it_looks_like_a_secret_or_no_one_set_it() {
        let ci = in_ci(&["GITHUB_ACTIONS", "GITLAB_CI"]);
        let candidate = |name: &str, value: &[u8]| unkeyed_candidate(name, value, &ci);
        // GitHub's own variables stay out, whatever their length; its token doesn't.
        assert!(!candidate("GITHUB_SERVER_URL", b"https://github.com"));
        assert!(!candidate("GITHUB_REF", b"refs/heads/main"));
        assert!(candidate("GITHUB_TOKEN", b"ghs_0123456789"));
        assert!(candidate("CI_JOB_TOKEN", b"glcbt-0123456789"));
        // Any name no CI system or build tool set searches from 8 bytes, a secret's too.
        assert!(candidate("FOO_TOKEN", b"12345678"));
        assert!(!candidate("FOO_TOKEN", b"1234567"));
        assert!(candidate("LICENSE_SERVER", b"01234567"));
        assert!(!candidate("LICENSE_SERVER", b"0123456"));
        assert!(candidate("GREETING", b"prod-eu-west-1"));
        // The build tools' own, and hushrun's, stay out unless they look like a secret.
        assert!(!candidate("CARGO_TARGET_DIR", b"/home/dev/w/target-long"));
        assert!(!candidate("HUSHRUN_BUILD_SESSION", b"/home/dev/.cache/x/y"));
        assert!(candidate("HUSHRUN_TOKEN", b"hru_0123456789"));
    }

    fn in_ci(set: &[&str]) -> SetByCi {
        SetByCi::detect(|name| set.contains(&name).then(|| "true".to_string()))
    }

    // covers: B-24
    #[test]
    fn a_name_under_a_ci_systems_prefix_is_searched_unless_that_system_documents_it() {
        // A project's own settings under the prefixes CI systems use (N1): searched, in CI too.
        let everywhere = in_ci(&[
            "GITHUB_ACTIONS",
            "GITLAB_CI",
            "TF_BUILD",
            "JENKINS_URL",
            "BUILDKITE",
        ]);
        for name in [
            "BUILD_ENV",
            "CI_REGION",
            "AGENT_POOL",
            "SYSTEM_FLAVOR",
            "RUNNER_PROFILE",
            "GITHUB_DEPLOY_TARGET",
            "BUILD_VERSION",
            "BUILDKITE_AGENT_META_DATA_QUEUE",
        ] {
            assert!(
                unkeyed_candidate(name, b"prod-eu-west-1", &everywhere),
                "{name}"
            );
        }
        // A system's documented names stay out only where that system runs: a laptop's
        // `BUILD_NUMBER` or `GITHUB_SHA` is searched.
        let laptop = in_ci(&[]);
        assert!(unkeyed_candidate(
            "GITHUB_SHA",
            b"0123456789abcdef",
            &laptop
        ));
        assert!(!unkeyed_candidate(
            "GITHUB_SHA",
            b"0123456789abcdef",
            &in_ci(&["GITHUB_ACTIONS"])
        ));
        assert!(unkeyed_candidate("BUILD_NUMBER", b"12345678", &laptop));
        assert!(!unkeyed_candidate(
            "BUILD_NUMBER",
            b"12345678",
            &in_ci(&["JENKINS_URL"])
        ));
        assert!(!unkeyed_candidate(
            "BUILD_SOURCEVERSION",
            b"0123456789abcdef",
            &in_ci(&["TF_BUILD"])
        ));
        assert!(!unkeyed_candidate(
            "CI_COMMIT_SHA",
            b"0123456789abcdef",
            &in_ci(&["GITLAB_CI"])
        ));
        // An empty detecting variable detects nothing.
        let empty = SetByCi::detect(|name| (name == "GITHUB_ACTIONS").then(String::new));
        assert!(unkeyed_candidate("GITHUB_SHA", b"0123456789abcdef", &empty));
        // The shell's own, whatever runs it.
        for name in ["_", "PWD", "OLDPWD"] {
            assert!(!unkeyed_candidate(name, b"/usr/bin/env", &laptop), "{name}");
        }
    }

    #[test]
    fn each_ci_systems_names_are_exact_and_none_is_a_prefix() {
        for system in SET_BY_CI {
            assert!(!system.detected_by.is_empty(), "{}", system.name);
            for name in system.names {
                assert!(
                    !name.is_empty() && !name.ends_with('_'),
                    "{}: {name}",
                    system.name
                );
                assert!(
                    !hushrun_core::secret_names::is_secret_name(name),
                    "{}: {name} looks like a secret",
                    system.name
                );
            }
        }
    }

    // covers: B-24
    #[test]
    fn an_unkeyed_value_in_an_output_keeps_the_block_here_and_names_only_its_variable() {
        let unkeyed = ["FOO_TOKEN".to_string(), "FOO_TOKEN".to_string()];
        let machine = machine();
        let p = portable(&Facts {
            class: Class::Library,
            asked: &asked(),
            observed: &observed(),
            records: &[],
            in_outputs: &BTreeSet::new(),
            machine: &machine,
            variable_names_base: false,
            externs: &[],
            unkeyed: &unkeyed,
            native_cpu: false,
            toolchain_in_output: false,
            keyed_by_place: false,
            searchable: true,
        });
        assert!(!p.portable);
        assert_eq!(p.reasons, vec![Reason::UnkeyedValue]);
        assert_eq!(p.variables, ["FOO_TOKEN"]);
        // A record from before the rule reads, and has no variables.
        let old: Portability = serde_json::from_str(r#"{"portable":true,"reasons":[]}"#).unwrap();
        assert!(old.variables.is_empty());
    }
    // covers: B-24
    #[test]
    fn only_folders_the_keys_hold_let_a_block_travel_where_they_are_the_same() {
        let with = |reasons: &[Reason]| Portability {
            portable: reasons.is_empty(),
            reasons: reasons.to_vec(),
            ..Portability::default()
        };
        // Portable: it travels anyway, and is not "bound".
        assert!(!with(&[]).bound_only());
        // Each folder a key holds the value of, alone and together.
        let held = [
            Reason::Tied,
            Reason::ToolchainInOutput,
            Reason::CargoHomeInOutput,
            Reason::RustupHomeInOutput,
            Reason::HomeInOutput,
            Reason::CargoHomeInRecord,
            Reason::RustupHomeInRecord,
            Reason::HomeInRecord,
            Reason::VariableNamesBase,
            Reason::ReadInSharedFolder,
        ];
        for reason in held {
            assert!(with(&[reason]).bound_only(), "{reason:?}");
        }
        assert!(with(&held).bound_only());
        // Anything else keeps it on its machine, whatever comes with it.
        for reason in [
            Reason::LinkEnvironmentUnknown,
            Reason::Unsearchable,
            Reason::ReadOutside,
            Reason::Extern,
            Reason::UnkeyedValue,
            Reason::NativeCpu,
        ] {
            assert!(!with(&[reason]).bound_only(), "{reason:?}");
            assert!(!with(&[Reason::Tied, reason]).bound_only(), "{reason:?}");
        }
    }
}
