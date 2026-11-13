//! Whether a block may leave the machine that built it: [`portable`] is the one place that
//! decides it.
//!
//! A block is **portable** when nothing in its keys, its hint, its record or its bytes names the
//! machine: no folder of this checkout (its place is `anywhere`), no read outside a root or a
//! base (the cargo home, the toolchain), none of the cargo home, the rustup home or the home
//! folder in an output or a record, no variable naming a base that a second compile didn't prove
//! harmless, and no proc macro among its externs that another machine names differently. A
//! linked output (a program, a proc macro, a build-script binary) is never portable yet: the
//! linker, the C runtime and the SDK it was linked with have no content identity. Whatever the
//! answer, the block is reused on this machine exactly as before: this decides only whether it
//! may leave.
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
    /// A program, a proc macro or a build-script binary: its link environment has no content
    /// identity yet.
    Linked,
    /// Its place is its roots (B-10): an output holds a folder of the checkout, a read lies
    /// outside every root, or the outputs can't be searched.
    Tied,
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
    /// It was compiled for `-C target-cpu=native` and the compiler couldn't say what that is
    /// here: the key holds the flag's text, which means another CPU on another machine (B-5).
    NativeCpu,
}

impl Reason {
    /// The reason as logs and records write it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Reason::Linked => "linked",
            Reason::Tied => "tied",
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
}

impl Portability {
    /// Whether a dependent on another machine names this block's files as a dependent here
    /// does. A library: when it is portable, since it is named by its bytes. A proc macro: when
    /// it is named by what it was built from (its place is `anywhere`) and that names no
    /// machine; its own bytes may still hold one (a link names the standard library's folder),
    /// since no dependent keys them, and a folder its expansion writes into a dependent's output
    /// is found there. Nothing else has dependents.
    pub fn named_portably(&self, class: Class) -> bool {
        match class {
            Class::Library => self.portable,
            Class::ProcMacro => self.reasons.iter().all(|reason| {
                matches!(
                    reason,
                    Reason::Linked
                        | Reason::CargoHomeInOutput
                        | Reason::RustupHomeInOutput
                        | Reason::HomeInOutput
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
    /// It is compiled for `-C target-cpu=native`, and the compiler couldn't say what that is.
    pub native_unresolved: bool,
}

/// Whether the block may leave the machine, and every reason it may not.
pub fn portable(facts: &Facts<'_>) -> Portability {
    let mut reasons = BTreeSet::new();
    if facts.class != Class::Library {
        reasons.insert(Reason::Linked);
    }
    if !matches!(facts.observed.place, Place::Anywhere) {
        reasons.insert(Reason::Tied);
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
    if facts.native_unresolved {
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
    }
}

/// The prefixes of the variables a CI system sets itself, documented by each: GitHub Actions,
/// GitLab, Buildkite, CircleCI, Travis, Jenkins, Azure Pipelines, Bitbucket, Drone, TeamCity,
/// AppVeyor and Codebuild. Their values (a server's URL, a ref, a run's number) are the same for
/// every job of a repository and turn up in crates by chance: `https://github.com` is in the
/// doc comments of nearly half the libraries on crates.io. A name among them that looks like a
/// secret (`GITHUB_TOKEN`, `CI_JOB_TOKEN`) is searched all the same.
pub const SET_BY_CI: &[&str] = &[
    "GITHUB_",
    "RUNNER_",
    "ACTIONS_",
    "CI_",
    "GITLAB_",
    "BUILDKITE_",
    "BUILDKITE",
    "CIRCLE_",
    "CIRCLECI",
    "TRAVIS_",
    "TRAVIS",
    "JENKINS_",
    "BUILD_",
    "SYSTEM_",
    "AGENT_",
    "TF_BUILD",
    "BITBUCKET_",
    "DRONE_",
    "DRONE",
    "TEAMCITY_",
    "APPVEYOR_",
    "APPVEYOR",
    "CODEBUILD_",
];

/// The prefixes of the variables the build tools set for a compile, and hushrun's own: cargo's
/// (the ones B-5 keys are skipped as keyed; the rest name folders, flags or jobs), rustc's and
/// rustup's. A name among them that looks like a secret is searched all the same.
const SET_BY_TOOLS: &[&str] = &[
    "CARGO", "__CARGO", "RUSTC", "RUSTDOC", "RUSTUP_", "RUST_", "OUT_DIR", "HUSHRUN_",
];

/// The least bytes a value of a variable that looks like a secret must have to be searched
/// for: a shorter one turns up in outputs by chance.
pub const SECRET_MIN_BYTES: usize = 8;
/// The least bytes a value of any other variable must have to be searched for.
pub const OTHER_MIN_BYTES: usize = 16;

/// Whether an output holding the value `value` of the variable `name`, which is not in the
/// compile's key, keeps its block on this machine (B-24): a name that looks like a secret (E-12)
/// with a value of [`SECRET_MIN_BYTES`] or more, or any variable no CI system or build tool set
/// itself with a value of [`OTHER_MIN_BYTES`] or more. The caller leaves out values that hold a
/// root or a folder of this machine, which the search already finds by their own rules.
pub fn unkeyed_candidate(name: &str, value: &[u8]) -> bool {
    if hushrun_core::secret_names::is_secret_name(name) {
        return value.len() >= SECRET_MIN_BYTES;
    }
    let set_by_others = SET_BY_CI
        .iter()
        .chain(SET_BY_TOOLS)
        .any(|prefix| name.starts_with(prefix));
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
            native_unresolved: false,
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
            "linked"
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
            native_unresolved: false,
        });
        assert_eq!(reasons(p), "variable-names-base");
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
                native_unresolved: false,
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
            native_unresolved: false,
        });
        assert_eq!(p.reasons, vec![Reason::CargoHomeInRecord]);
    }

    #[test]
    fn the_reasons_are_written_as_they_are_named() {
        for reason in [
            Reason::Linked,
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
        // GitHub's own variables stay out, whatever their length; its token doesn't.
        assert!(!unkeyed_candidate(
            "GITHUB_SERVER_URL",
            b"https://github.com"
        ));
        assert!(!unkeyed_candidate("GITHUB_REF", b"refs/heads/main"));
        assert!(unkeyed_candidate("GITHUB_TOKEN", b"ghs_0123456789"));
        assert!(unkeyed_candidate("CI_JOB_TOKEN", b"glcbt-0123456789"));
        // A secret's name searches from 8 bytes; any other name from 16.
        assert!(unkeyed_candidate("FOO_TOKEN", b"12345678"));
        assert!(!unkeyed_candidate("FOO_TOKEN", b"1234567"));
        assert!(unkeyed_candidate("LICENSE_SERVER", b"0123456789abcdef"));
        assert!(!unkeyed_candidate("LICENSE_SERVER", b"0123456789abcde"));
        // The build tools' own, and hushrun's, stay out unless they look like a secret.
        assert!(!unkeyed_candidate(
            "CARGO_TARGET_DIR",
            b"/home/dev/w/target-long"
        ));
        assert!(!unkeyed_candidate(
            "HUSHRUN_BUILD_SESSION",
            b"/home/dev/.cache/x/y"
        ));
        assert!(unkeyed_candidate("HUSHRUN_TOKEN", b"hru_0123456789"));
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
            native_unresolved: false,
        });
        assert!(!p.portable);
        assert_eq!(p.reasons, vec![Reason::UnkeyedValue]);
        assert_eq!(p.variables, ["FOO_TOKEN"]);
        // A record from before the rule reads, and has no variables.
        let old: Portability = serde_json::from_str(r#"{"portable":true,"reasons":[]}"#).unwrap();
        assert!(old.variables.is_empty());
    }
}
