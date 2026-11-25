//! A repository a test of test files runs a check in: committed in a temporary directory, with
//! hushrun's cache beside it, and the environment its COMMAND gets. An in-process test runs the
//! check with `in_process.rs`; an end-to-end one starts the binary with `cli.rs`, which is the
//! only file here that names it.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_core::digest::Digest;
use hushrun_core::policy::Policy;
use hushrun_core::units::Tree;
use hushrun_testkit::git;
use serde_json::Value;

/// Environment variables COMMAND must never inherit from the test's: CI's, a coding agent's,
/// hushrun's own and the build blocks'. [`Repo::clear`] adds a runner's.
pub const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_UNITS",
    "HUSHRUN_TESTS_LEAVE_OUT",
    "HUSHRUN_TESTS_EVENTS",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "npm_lifecycle_event",
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CODEX_SANDBOX",
    "CODEX_CI",
    "CODEX_THREAD_ID",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "CURSOR_AGENT",
    "CURSOR_EXTENSION_HOST_ROLE",
    "GEMINI_CLI",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
    "COPILOT_MODEL",
    "COPILOT_ALLOW_ALL",
    "COPILOT_GITHUB_TOKEN",
    "CLAUDE_CODE_REMOTE",
    "CURSOR_AGENT_SOCKET",
    "COPILOT_AGENT_SESSION_ID",
    "CODEX_PROXY_CERT",
    "HUSHRUN_SANDBOX",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_CC",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "RUSTC_WRAPPER",
];

/// A repository in a temporary directory, committed, with hushrun's cache beside it.
pub struct Repo {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub repo: PathBuf,
    pub cache: PathBuf,
    cleared: Vec<&'static str>,
    given: Vec<(String, OsString)>,
}

impl Repo {
    /// A repository of `files`, each a path from its root and its text, committed.
    pub fn new(files: &[(String, String)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let r = Repo {
            _dir: dir,
            cache: root.join("cache"),
            root,
            repo,
            cleared: CLEARED.to_vec(),
            given: Vec::new(),
        };
        for (path, text) in files {
            r.write(path, text);
        }
        git::commit_all(&r.repo, "init");
        r
    }

    /// Also clears `names` from COMMAND's environment: a runner's own variables.
    pub fn clear(mut self, names: &[&'static str]) -> Repo {
        self.cleared.extend_from_slice(names);
        self
    }

    /// Also gives COMMAND `name`, in every run: a runner's home, as a person's shell would.
    pub fn give(mut self, name: &str, value: impl Into<OsString>) -> Repo {
        self.given.push((name.to_string(), value.into()));
        self
    }

    /// Writes `text` at `path`, from the repository's root.
    pub fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    /// Removes the file at `path`, from the repository's root.
    pub fn remove(&self, path: &str) {
        std::fs::remove_file(self.repo.join(path)).expect("remove");
    }

    /// COMMAND's program, `program`, in the repository's root, with the environment `hushrun
    /// run` would give it but hushrun's own variables ([`Repo::configure`]). It starts through
    /// `hushrun_testkit::tool::command`, which the static map trusts to start none of the
    /// workspace's binaries, and which panics if `program` is one: an in-process test reaches
    /// only what it calls, not the whole binary (CONTRIBUTING.md, "Which tests CI runs again").
    pub fn command(&self, program: &str) -> Command {
        let mut cmd = hushrun_testkit::tool::command(program);
        self.configure(&mut cmd);
        cmd
    }

    /// Gives `cmd` the repository's root as its directory, and the environment `hushrun run`
    /// would give COMMAND but hushrun's own variables: what the developer's shell exports that
    /// changes a runner never reaches it.
    pub fn configure(&self, cmd: &mut Command) {
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        for var in &self.cleared {
            cmd.env_remove(var);
        }
        hushrun_testkit::env::Hermetic::hermetic(cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        for (name, value) in &self.given {
            cmd.env(name, value);
        }
    }

    /// The map this machine's store keeps of the one check, as JSON (`common/store.rs`).
    pub fn map(&self) -> Value {
        super::store::map(&self.cache)
    }

    /// The units whose second run disagreed with their pass, which suspended reuse.
    pub fn disagreements(&self) -> Vec<String> {
        super::store::disagreements(&self.cache)
    }

    /// Every unit result this machine recorded, `pass` or `fail`, by count.
    pub fn unit_results(&self) -> BTreeMap<String, usize> {
        let mut files = Vec::new();
        collect(&self.cache.join("results/units/v0"), &mut files);
        let mut out = BTreeMap::new();
        for f in files {
            let v: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
            let result = v["result"].as_str().unwrap_or("?").to_string();
            *out.entry(result).or_default() += 1;
        }
        out
    }

    /// hushrun.yaml, as the repository has it now.
    pub fn policy(&self) -> Policy {
        let bytes = std::fs::read(self.repo.join("hushrun.yaml")).expect("a hushrun.yaml");
        Policy::parse("hushrun.yaml", &bytes).expect("hushrun.yaml reads")
    }

    /// The scope's digest and files in the worktree now, as `git add -A && git commit` would
    /// record them (D-10): what `hushrun run` keys and observes a run by.
    pub fn scope(&self, policy: &Policy) -> (hushrun_core::names::ScopeName, Digest, Tree) {
        use hushrun_vcs::Vcs as _;
        let options = hushrun_vcs::Options {
            cache_dir: Some(self.cache.clone()),
            jobs: 0,
            git_reader: Default::default(),
        };
        let vcs = hushrun_vcs::git::Git::open(&self.repo, options).expect("the repository opens");
        let listing = vcs
            .list(&hushrun_vcs::Snapshot::Worktree)
            .expect("the files list");
        let scope = policy.scopes.iter().next().expect("a scope");
        let run = hushrun_vcs::digest_scopes(&vcs, &listing, &policy.scopes, &[scope], false)
            .expect("the scope digests");
        let digested = &run.scopes[0];
        let tree = Tree::from_manifest(&digested.manifest).expect("the manifest reads");
        (scope.name.clone(), digested.digest, tree)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        self.clean();
    }
}

impl Repo {
    /// Removes the builds' outputs, which the tests are done with.
    pub fn clean(&self) {
        for entry in std::fs::read_dir(&self.repo)
            .into_iter()
            .flatten()
            .flatten()
        {
            let _ = std::fs::remove_dir_all(entry.path().join("build"));
        }
        let _ = std::fs::remove_dir_all(self.repo.join("build"));
    }
}

pub fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}
