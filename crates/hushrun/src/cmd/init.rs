//! `hushrun init --pipeline` (§10.4): writes a starter `hushrun.yaml` that deploys, and prints
//! what to do next. \[ADR 0031\] No keys and no trust: the repository's CI signs keylessly,
//! and members' signed-in machines count as they are (T-15). `hushrun init` without it sets up
//! `hushrun run` (`cmd::setup`, C-15).

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context as _, anyhow};
use serde_json::json;

use hushrun_core::policy::Policy;

use crate::app::App;
use crate::cli::InitArgs;
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};

pub fn run(app: &App<'_>, args: &InitArgs) -> CmdResult {
    let start = match &app.global.repo {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().context("finding the current directory")?,
    };
    let root = match hushrun_vcs::detect(&start).map_err(Failure::usage)? {
        hushrun_vcs::Detected::Git { root } => root,
        hushrun_vcs::Detected::None => {
            return Err(Failure::usage(anyhow!(
                "hushrun init sets up a git repository; run it inside one"
            )));
        }
    };
    let policy_path = root.join("hushrun.yaml");
    if policy_path.exists() && !args.force {
        return Err(Failure::usage(anyhow!(
            "{} exists; pass --force to replace it",
            policy_path.display()
        )));
    }
    let repository = match &args.repository {
        Some(repository) => repository.clone(),
        None => origin_repository(&root).ok_or_else(|| {
            Failure::usage(anyhow!(
                "can't tell the repository from the origin remote: pass --repository host/owner/name"
            ))
        })?,
    };
    let image = args
        .image
        .clone()
        .unwrap_or_else(|| "registry.example.com/acme/app".into());
    let ci = Ci::of(&root, &repository);

    let policy_text = policy_text(
        ci,
        &repository,
        &test_command(&root),
        &image,
        args.branch.as_deref().unwrap_or("main"),
    );
    Policy::parse("hushrun.yaml", policy_text.as_bytes())
        .map_err(|e| Failure::usage(anyhow!("the starter hushrun.yaml doesn't validate: {e}")))?;
    std::fs::write(&policy_path, &policy_text)
        .with_context(|| format!("writing {}", policy_path.display()))?;

    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.init/v1",
            "policy": policy_path,
        }))?,
        _ => output::print(&next_steps(ci, &policy_path))?,
    }
    Ok(Exit::Ok)
}

/// The CI the starter files are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ci {
    Github,
    Gitlab,
}

impl Ci {
    /// GitLab when the repository has a `.gitlab-ci.yml` or lives on a GitLab host; else GitHub.
    fn of(root: &Path, repository: &str) -> Ci {
        let host = repository.split('/').next().unwrap_or_default();
        if root.join(".gitlab-ci.yml").exists() || host.split('.').any(|label| label == "gitlab") {
            Ci::Gitlab
        } else {
            Ci::Github
        }
    }
}

/// `host/owner/name` of the origin remote, as `hushrun run`'s results name a repository
/// (C-10).
pub(crate) fn origin_repository(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get", "remote.origin.url"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    repository_of(String::from_utf8(output.stdout).ok()?.trim())
}

/// `host/owner/name` from a git remote URL: `https://github.com/acme/shop.git`,
/// `git@github.com:acme/shop.git` and `ssh://git@github.com:22/acme/shop` all give
/// `github.com/acme/shop`. Local paths give nothing.
pub(crate) fn repository_of(remote: &str) -> Option<String> {
    let rest = match remote.split_once("://") {
        Some((_, rest)) => rest.to_string(),
        // scp-like syntax, [user@]host:path, unless it is a path.
        None => {
            let (host, path) = remote.split_once(':')?;
            if host.contains('/') {
                return None;
            }
            format!("{host}/{path}")
        }
    };
    let (authority, path) = rest.split_once('/')?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    if host.is_empty() {
        return None;
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // C-2: a lowercase host; the path keeps its case.
    (segments.len() >= 2).then(|| format!("{}/{}", host.to_ascii_lowercase(), segments.join("/")))
}

/// A test command for the stack the repository looks like.
fn test_command(root: &Path) -> String {
    let has = |file: &str| root.join(file).exists();
    if has("go.mod") {
        "go test ./...".into()
    } else if has("Cargo.toml") {
        "cargo test --locked".into()
    } else if has("package.json") {
        "npm test".into()
    } else if has("pyproject.toml") || has("requirements.txt") {
        "python -m pytest".into()
    } else {
        "echo 'replace with your test command' && false".into()
    }
}

fn policy_text(ci: Ci, repository: &str, test: &str, image: &str, branch: &str) -> String {
    let guide = match ci {
        Ci::Github => "https://hushrun.com/docs/deploy/github",
        Ci::Gitlab => "https://hushrun.com/docs/deploy/gitlab",
    };
    format!(
        "# hushrun.yaml: what a deploy requires ({guide}).
version: 1
repository: {repository}
# repository_url: where this repository's events go; `npx hushrun init` sets it.
# repository_url: https://HOST/l/ORG/REPOSITORY

checks:
  unit:
    run: {test}

artifacts:
  image:
    type: oci
    location: {image}

deploy:
  branch: {branch}
  require: [unit]
"
    )
}

fn next_steps(ci: Ci, policy: &Path) -> String {
    let mut steps: Vec<&[&str]> = vec![
        &["Edit hushrun.yaml: the image repository, the test command, the branch."],
        &[
            "Add the repository to hushrun with `npx hushrun init`, or name your own URL in `repository_url:`.",
        ],
    ];
    match ci {
        Ci::Github => {
            steps.push(&[
                "Run `hushrun adopt` to write the workflow, or copy it from https://hushrun.com/docs/deploy/github",
                "to .github/workflows/: with `id-token: write`, its events are signed as this repository's CI, with no key to keep.",
            ]);
            steps.push(&[
                "Commit hushrun.yaml. Events count when a member's signed-in machine, an agent of the org or this repository's CI signs them.",
                "To have deploys take evidence from this repository's CI only, turn it on in the dashboard, on the repository's page.",
            ]);
        }
        // Only GitHub Actions is a repository's keyless CI: a GitLab pipeline signs as an agent,
        // with its machine key, and its token takes it to the repository on hushrun's host.
        Ci::Gitlab => {
            steps.push(&[
                "Make the pipeline an agent token, with its machine key, for a year: a token lasts 30 days unless --expires says otherwise.",
                "  hushrun token create --agent gitlab-ci --expires 365d",
                "Store the token as HUSHRUN_TOKEN, a protected, masked CI/CD variable, and the key as HUSHRUN_MACHINE_KEY_FILE,",
                "a protected variable of type File, since GitLab can't mask a key: the pipeline signs with it, as agent:gitlab-ci.",
                "Let no one set pipeline variables, which override HUSHRUN_REPOSITORY_URL: `hushrun doctor` with",
                "HUSHRUN_GITLAB_TOKEN, a project access token with read_api, checks it.",
            ]);
            steps.push(&[
                "Copy the .gitlab-ci.yml from https://hushrun.com/docs/deploy/gitlab to the project's root, and the release's install-hushrun.sh to ci/.",
            ]);
            steps.push(&[
                "Commit hushrun.yaml. Events count when a member's signed-in machine, an agent of the org or this repository's CI signs them.",
                "A GitLab project can't take evidence from its CI only: its pipeline signs as an agent, whose events don't count then.",
            ]);
        }
    }
    steps.push(&["Run `hushrun doctor`."]);
    let mut text = String::new();
    let _ = writeln!(text, "wrote {}\n", policy.display());
    let _ = writeln!(text, "Next:");
    for (n, lines) in steps.iter().enumerate() {
        for (i, line) in lines.iter().enumerate() {
            let _ = if i == 0 {
                writeln!(text, "  {}. {line}", n + 1)
            } else {
                writeln!(text, "     {line}")
            };
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitlab_repositories_get_gitlab_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Ci::of(dir.path(), "github.com/acme/app"), Ci::Github);
        assert_eq!(Ci::of(dir.path(), "gitlab.com/acme/app"), Ci::Gitlab);
        assert_eq!(Ci::of(dir.path(), "gitlab.acme.dev/group/app"), Ci::Gitlab);
        assert_eq!(Ci::of(dir.path(), "notgitlab.com/acme/app"), Ci::Github);
        std::fs::write(dir.path().join(".gitlab-ci.yml"), "").unwrap();
        assert_eq!(Ci::of(dir.path(), "git.acme.dev/group/app"), Ci::Gitlab);
        for ci in [Ci::Github, Ci::Gitlab] {
            let text = policy_text(ci, "gitlab.com/acme/app", "go test ./...", "r/a", "main");
            let policy = Policy::parse("hushrun.yaml", text.as_bytes()).unwrap();
            assert!(policy.retired.is_empty(), "{text}");
            assert_eq!(
                policy.deploy.unwrap().branch.as_deref(),
                Some("main"),
                "{text}"
            );
            assert_eq!(
                text.contains("https://hushrun.com/docs/deploy/gitlab"),
                ci == Ci::Gitlab,
                "{text}"
            );
            assert!(!text.contains("stage"), "{text}");
        }
        let steps = next_steps(Ci::Gitlab, Path::new("p"));
        for says in [
            "hushrun token create --agent gitlab-ci --expires 365d",
            "HUSHRUN_TOKEN, a protected, masked CI/CD variable",
            "HUSHRUN_MACHINE_KEY_FILE",
            "HUSHRUN_GITLAB_TOKEN, a project access token with read_api",
            "https://hushrun.com/docs/deploy/gitlab",
            "can't take evidence from its CI only",
        ] {
            assert!(steps.contains(says), "{says}: {steps}");
        }
        let github = next_steps(Ci::Github, Path::new("p"));
        assert!(github.contains("turn it on in the dashboard"), "{github}");
        // Steps are numbered one after the other, and cite nothing a user can't open: no rule
        // of the spec, no path in hushrun's repository.
        for (text, last) in [(&steps, 6), (&github, 5)] {
            let numbers: Vec<usize> = text
                .lines()
                .filter_map(|l| l.trim_start().split_once(". "))
                .filter_map(|(n, _)| n.parse().ok())
                .collect();
            assert_eq!(numbers, (1..=last).collect::<Vec<_>>(), "{text}");
            for internal in ["(T-", "docs/guides", "recipes/"] {
                assert!(!text.contains(internal), "{internal}: {text}");
            }
        }
    }

    #[test]
    fn repositories_from_remote_urls() {
        for (url, expected) in [
            ("git@github.com:Acme/api.git", Some("github.com/Acme/api")),
            ("https://GitHub.com/Acme/api", Some("github.com/Acme/api")),
            (
                "https://x-token:secret@github.com/a/b.git",
                Some("github.com/a/b"),
            ),
            (
                "ssh://git@gitlab.example.com/group/sub/app.git",
                Some("gitlab.example.com/group/sub/app"),
            ),
            (
                "ssh://git@github.com:22/acme/shop",
                Some("github.com/acme/shop"),
            ),
            ("github.com:acme/shop.git", Some("github.com/acme/shop")),
            ("/srv/git/app.git", None),
            ("file:///srv/git/shop", None),
            ("C:\\repos\\shop", None),
            ("https://github.com/only-owner", None),
        ] {
            assert_eq!(repository_of(url).as_deref(), expected, "{url}");
        }
    }
}
