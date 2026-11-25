//! The `hushrun` binary, for an end-to-end test (`tests/e2e_*.rs`): the one file of `common/`
//! that names it. A test that includes this reaches every line of hushrun, so only an end-to-end
//! test includes it (CONTRIBUTING.md, "Which tests CI runs again").

#![allow(dead_code)]

use std::process::Command;

use serde_json::Value;

use super::repo::{Repo, collect};

/// The binary in a [`Repo`].
pub trait Cli {
    /// The binary, as a person runs it in the repository's root. Its environment is COMMAND's
    /// ([`Repo::command`]), with hushrun's own settings for a machine of its own: no backend, no
    /// update check.
    fn hushrun(&self) -> Command;
    /// `hushrun ARGS`, with `env` besides, to its end.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output;
    /// The newest result of a check this machine recorded, as the binary records it.
    fn newest_result(&self) -> Value;
}

impl Cli for Repo {
    fn hushrun(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        self.configure(&mut cmd);
        cmd.env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", "http://127.0.0.1:9")
            .env("HUSHRUN_NO_UPDATE_CHECK", "1");
        cmd
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = self.hushrun();
        for (name, value) in env {
            cmd.env(name, value);
        }
        let output = cmd.args(args).output().expect("hushrun");
        Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn newest_result(&self) -> Value {
        let mut files = Vec::new();
        collect(&self.cache.join("results/results/v0"), &mut files);
        files.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
        let newest = files.last().expect("a result");
        serde_json::from_slice(&std::fs::read(newest).unwrap()).unwrap()
    }
}

/// What a start of the binary came to.
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// The line of hushrun's that starts `start`.
    pub fn line(&self, start: &str) -> &str {
        self.stderr
            .lines()
            .find(|l| l.starts_with(start))
            .unwrap_or_else(|| {
                panic!(
                    "no line starting {start:?} in:\n{}\n{}",
                    self.stderr, self.stdout
                )
            })
    }
}
