//! B-69: `hushrun init --bake`: the job that bakes the default branch's compiled code into an
//! image after its build, and on a manual run, for sandboxes and containers that start warm.
//!
//! On GitHub it is a workflow of its own, `.github/workflows/hushrun-bake.yml`, that runs when
//! the workflow of the build check completes on the default branch (`workflow_run`), and by hand
//! (`workflow_dispatch`): a new file, so nothing init didn't write changes. On GitLab it is a job
//! appended to `.gitlab-ci.yml` in the `.post` stage, on pushes to the default branch and on
//! pipelines started from the web. Either way the merge that brings it is itself a push to the
//! default branch, so the first image is baked right after it, with no step of its own.
//!
//! The job bakes from the backend (`--from backend --trusted`): in CI a run shares its blocks
//! only through a backend (B-31), and the backend's trusted scope is what CI on the default branch
//! wrote. It signs keylessly with the job's ID token, and publishes to the registry of hushrun's
//! hosted backend when the repository uses that backend (B-70: the backend makes the image from
//! the blocks it holds, so nothing is pushed), else pushes to the repository's own registry (GHCR,
//! or the GitLab project's) with the job's own token.

use std::path::Path;

use super::workflows::{Host, Workflow};

/// The bake workflow's path on GitHub.
pub(super) const GITHUB_PATH: &str = ".github/workflows/hushrun-bake.yml";
/// GitHub Actions' OIDC issuer, as Fulcio certificates name it.
const GITHUB_ISSUER: &str = "https://token.actions.githubusercontent.com";
/// The GitLab job's name.
const GITLAB_JOB: &str = "hushrun-bake";

/// What `--bake` adds: files (path, new text, what changes), what the plan says besides, what a
/// person adds by hand, and whether the GitLab job needs `ci/install-hushrun.sh` beside it.
#[derive(Debug, Default)]
pub(super) struct Bake {
    pub(super) files: Vec<(String, String, Vec<String>)>,
    pub(super) notes: Vec<String>,
    pub(super) manual: Vec<String>,
    pub(super) installs: bool,
}

/// The check whose workflow the GitHub job follows: `build`, else a check whose name holds
/// `build`, else `test`, else none. The job itself bakes every check (B-63), so it names none.
fn check_of(wrapped: &[String]) -> Option<&str> {
    wrapped
        .iter()
        .find(|c| *c == "build")
        .or_else(|| wrapped.iter().find(|c| c.contains("build")))
        .or_else(|| wrapped.iter().find(|c| *c == "test"))
        .map(String::as_str)
}

/// B-70: the image in hushrun's hosted registry of `repository`, when `backend` (the org's URL)
/// is the hosted backend's: `registry.hushrun.com/<org>/<name>`.
fn hosted_image(backend: &str, repository: &str) -> Option<String> {
    let rest = backend
        .trim()
        .trim_end_matches('/')
        .strip_prefix(crate::hosted::HOSTED_ORIGIN)?
        .strip_prefix("/o/")?;
    let org = rest.split('/').next().filter(|org| !org.is_empty())?;
    let name = hushrun_core::blocks::publish::image_name(repository)?;
    Some(format!("{}/{org}/{name}", crate::hosted::HOSTED_REGISTRY))
}

/// The default branch, as the origin remote's `HEAD` names it; `main` when it names none.
pub(crate) fn default_branch(root: &Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            String::from_utf8(out.stdout)
                .ok()?
                .trim()
                .strip_prefix("origin/")
                .map(str::to_string)
        })
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "main".to_string())
}

/// B-69: the bake job for the repository `repository` (`github.com/acme/app`), whose CI files are
/// `workflows` (as `edited` leaves them, when init edits them too), at `root`, sharing through
/// `backend`. `package` is the npm package a GitHub job runs.
#[allow(clippy::too_many_arguments)]
pub(super) fn plan(
    root: &Path,
    repository: Option<&str>,
    workflows: &[Workflow],
    edited: &[(String, String, Vec<String>)],
    wrapped: &[String],
    backend: Option<&str>,
    package: &str,
    branches: &[String],
) -> Bake {
    let mut bake = Bake::default();
    let Some(backend) = backend else {
        bake.notes.push(
            "--bake needs a backend: in CI a job shares compiled code only through one, and the bake job reads the default branch's build from it. Run hushrun init without --local first".into(),
        );
        return bake;
    };
    let branch = default_branch(root);
    let check = check_of(wrapped);
    let Some(repository) = repository else {
        bake.notes.push(
            "--bake: no origin remote names this repository, so init can't name its registry"
                .into(),
        );
        return bake;
    };
    let github = workflows
        .iter()
        .filter(|w| w.host == Host::GitHub)
        .find(|w| check.is_some_and(|c| w.running(&[c.to_string()]).next().is_some()));
    let github = github.or_else(|| workflows.iter().find(|w| w.host == Host::GitHub));
    let hosted = hosted_image(backend, repository);
    // B-69: the long-lived branches hushrun.yaml names (`build: baked: branches:`), each baked
    // into its own tag; not the default branch, and none where the registry has no branch tags.
    let mut branches: Vec<&str> = branches
        .iter()
        .map(String::as_str)
        .filter(|b| *b != branch)
        .collect();
    if hosted.is_some() && !branches.is_empty() {
        bake.notes.push(format!(
            "build: baked: branches names {}, but hushrun's registry has no branch tags: the job bakes {branch} alone",
            branches.join(", ")
        ));
        branches.clear();
    }
    if let (Some(path), Some(workflow)) = (repository.strip_prefix("github.com/"), github) {
        if root.join(GITHUB_PATH).exists() {
            bake.notes.push(format!(
                "{GITHUB_PATH} is there already: init leaves it as it is"
            ));
        } else {
            let image = hosted
                .clone()
                .unwrap_or_else(|| format!("ghcr.io/{}", path.to_ascii_lowercase()));
            let name = workflow.name().unwrap_or_else(|| workflow.path.clone());
            // B-75, ADR 0102: the build job's setup steps, filled in and pinned, or none and no
            // build in the bake (what the plan says to fix).
            let setup = build_job(workflow, check)
                .map(|job| setup_steps(root, workflow, &job))
                .unwrap_or(Ok((Vec::new(), false)));
            let (setup, builds) = match setup {
                Ok((steps, nested)) => {
                    if nested {
                        bake.notes.push(
                            "The bake job runs the build job's actions of other owners pinned to a commit: an action pinned so can still use actions of its own by tag, which init can't read without the network. Read theirs, or pin them in a copy of your own".into(),
                        );
                    }
                    (steps, true)
                }
                Err(why) => {
                    bake.notes.push(format!(
                        "The bake job doesn't build what a skipped merge left unbuilt (it runs with --no-build, B-75): {}. Then delete {GITHUB_PATH} and run init --bake again",
                        why.join("; ")
                    ));
                    (Vec::new(), false)
                }
            };
            bake.files.push((
                GITHUB_PATH.to_string(),
                github_text(
                    &name,
                    &branch,
                    &image,
                    package,
                    hosted.is_some(),
                    &branches,
                    &setup,
                    builds,
                ),
                vec![format!(
                    "adds the bake job: after {name} passes on {branch}, and when run by hand, it bakes the build's compiled code into {image}, signed as this workflow"
                )],
            ));
            bake.notes.push(format!(
                "A Dockerfile takes the baked image with: COPY --from={image}:hushrun-linux-x64-latest /hushrun /hushrun"
            ));
            let repo_name = path.rsplit('/').next().unwrap_or(path);
            bake.notes.push(format!(
                "Blocks bound to the build's folders (C -sys crates, build scripts, what depends on them) are baked too, at the paths of GitHub's runner: a sandbox reuses them only where HOME=/home/runner, CARGO_HOME=/home/runner/.cargo and the checkout is /home/runner/work/{repo_name}/{repo_name} (the image's baked.json names them, B-76); elsewhere they compile"
            ));
            bake.manual.push(format!(
                "to let a run trust the baked image's signature with no backend, add to hushrun.yaml:\n  build:\n    baked:\n      signers:\n        - issuer: {GITHUB_ISSUER}\n          subject: https://github.com/{path}/{GITHUB_PATH}@refs/heads/{branch}"
            ));
            branch_notes(
                &mut bake,
                &image,
                &branches,
                &format!("https://github.com/{path}/{GITHUB_PATH}@refs/heads/"),
            );
        }
    }
    if let Some(gitlab) = workflows.iter().find(|w| w.host == Host::GitLab) {
        let text = edited
            .iter()
            .find(|(path, ..)| *path == gitlab.path)
            .map_or_else(|| gitlab.source().to_string(), |(_, text, _)| text.clone());
        if text
            .lines()
            .any(|l| l.starts_with(&format!("{GITLAB_JOB}:")))
        {
            bake.notes.push(format!(
                "{} has a {GITLAB_JOB} job already: init leaves it as it is",
                gitlab.path
            ));
        } else {
            let mut changes = edited
                .iter()
                .find(|(path, ..)| *path == gitlab.path)
                .map(|(_, _, changes)| changes.clone())
                .unwrap_or_default();
            changes.push(format!(
                "adds the {GITLAB_JOB} job: after the pipeline of {branch}, and when run from the web, it bakes the build's compiled code into {}, signed as the job",
                hosted.as_deref().unwrap_or("the project's registry")
            ));
            let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
            let mut new = text.clone();
            if !new.ends_with('\n') {
                new.push_str(newline);
            }
            let (image, before) = build_job(gitlab, check)
                .map(|job| gitlab_setup(&text, &job))
                .unwrap_or_default();
            new.push_str(
                &gitlab_text(
                    backend,
                    hosted.as_deref(),
                    &branches,
                    image.as_deref(),
                    before.as_deref(),
                )
                .replace('\n', newline),
            );
            bake.files.push((gitlab.path.clone(), new, changes));
            bake.notes.push(format!(
                "A Dockerfile takes the baked image with: COPY --from={}:hushrun-linux-x64-latest /hushrun /hushrun",
                hosted.as_deref().unwrap_or("<the project's registry image>")
            ));
            bake.notes.push(format!(
                "A pipeline schedule on {branch} (Build > Pipeline schedules, daily) lets an image the re-bake limit held back catch up when no merge follows (B-73): the {GITLAB_JOB} job runs in scheduled pipelines"
            ));
            bake.notes.push(
                "Blocks bound to the build's folders (C -sys crates, build scripts, what depends on them) are baked too, at the job's paths: a sandbox reuses them only where HOME, CARGO_HOME and the checkout are the job's ($CI_PROJECT_DIR, often /builds/<group>/<project>, and the image's cargo home; the image's baked.json names them, B-76); elsewhere they compile".into(),
            );
            if let Some(path) = repository.strip_prefix("gitlab.com/") {
                bake.manual.push(format!(
                    "to let a run trust the baked image's signature with no backend, add to hushrun.yaml:\n  build:\n    baked:\n      signers:\n        - issuer: https://gitlab.com\n          subject: https://gitlab.com/{path}//.gitlab-ci.yml@refs/heads/{branch}"
                ));
                branch_notes(
                    &mut bake,
                    hosted
                        .as_deref()
                        .unwrap_or("<the project's registry image>"),
                    &branches,
                    &format!("https://gitlab.com/{path}//.gitlab-ci.yml@refs/heads/"),
                );
            }
            bake.installs = true;
        }
    }
    // B-72, B-74: before the default branch's first build lands, a first image of the
    // dependencies alone; and how to see what an image saves, measured.
    if !bake.files.is_empty() && root.join("Cargo.toml").is_file() && hosted.is_none() {
        bake.notes.push(
            "Before CI's first build is baked, `hushrun build bake --deps-only --oci <image> -- <the check's cargo command>` in a job on the default branch publishes an image of the dependencies alone (a `cargo test` compiles its tests and runs none, the repository's own crates as stubs); the first full bake replaces it. `--report -- <command>` on any bake measures that command cold and from the image, on the machine it runs on".into(),
        );
    }
    if let Some(image) = &hosted
        && !bake.files.is_empty()
    {
        bake.notes.push(format!(
            "The image lives in hushrun's registry, made from the blocks the backend holds: nothing is pushed. A Dockerfile build pulls it after `docker login {} -u hushrun -p <pull token>`, a read-only token an owner or admin makes for {image}",
            crate::hosted::HOSTED_REGISTRY
        ));
    }
    if bake.files.is_empty() && bake.notes.is_empty() {
        bake.notes.push(
            "--bake: no GitHub workflow or GitLab pipeline here to add the bake job to".into(),
        );
    }
    bake
}

/// B-75: the job whose environment the bake job copies: the one that runs the build check, else
/// the first that runs commands.
fn build_job(workflow: &Workflow, check: Option<&str>) -> Option<String> {
    let wanted: Vec<String> = check.map(str::to_string).into_iter().collect();
    workflow
        .running(&wanted)
        .next()
        .or_else(|| workflow.jobs.iter().find(|job| !job.steps.is_empty()))
        .map(|job| job.id.clone())
}

/// B-75: the setup steps of `job` in the GitHub workflow `workflow` that the bake job copies, so
/// its build of what's missing has the build's toolchain: each step that `uses:` an action other
/// than `actions/checkout` (a toolchain, a cache, a language's setup), re-indented under the bake
/// job's `steps:`, with what CI fills in that the bake job wouldn't filled in from the build job
/// ([`super::replay::resolve`]: its primary matrix entry's values and the workflow's, the job's and
/// the step's variables; secrets and GitHub's own words stay as written). A step that only runs
/// commands isn't copied: what it installs may need what the bake job doesn't have, and the plan
/// says so. `Err` names each step that can't be copied, and how to fix it (ADR 0102): an action of
/// another owner than GitHub's that isn't pinned to a full commit, or a value only the build job's
/// run knows (another step's output, an input, a matrix CI makes as it runs).
fn setup_steps(
    root: &Path,
    workflow: &Workflow,
    job: &str,
) -> Result<(Vec<String>, bool), Vec<String>> {
    use super::replay;
    let source = workflow.source();
    let lines: Vec<&str> = source.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let content = |l: &str| !l.trim().is_empty() && !l.trim_start().starts_with('#');
    let Some(jobs_at) = lines
        .iter()
        .position(|l| indent(l) == 0 && l.trim_end() == "jobs:")
    else {
        return Ok((Vec::new(), false));
    };
    let Some(header) = (jobs_at + 1..lines.len()).find(|&i| {
        content(lines[i]) && indent(lines[i]) > 0 && lines[i].trim() == format!("{job}:")
    }) else {
        return Ok((Vec::new(), false));
    };
    let job_indent = indent(lines[header]);
    let end = (header + 1..lines.len())
        .find(|&i| content(lines[i]) && indent(lines[i]) <= job_indent)
        .unwrap_or(lines.len());
    let Some(steps_at) = (header + 1..end).find(|&i| lines[i].trim_start().starts_with("steps:"))
    else {
        return Ok((Vec::new(), false));
    };
    let steps_indent = indent(lines[steps_at]);
    let mut items: Vec<Vec<&str>> = Vec::new();
    let mut item_indent = None;
    for line in &lines[steps_at + 1..end] {
        if !content(line) {
            continue;
        }
        let at = indent(line);
        let dash = line.trim_start().starts_with("- ");
        if at < steps_indent || (at == steps_indent && !dash) {
            break;
        }
        if dash && item_indent.is_none_or(|i| i == at) {
            item_indent = Some(at);
            items.push(vec![line]);
        } else if let Some(item) = items.last_mut() {
            item.push(line);
        }
    }
    let Some(base) = item_indent else {
        return Ok((Vec::new(), false));
    };
    let yaml = super::variants::parse(&workflow.path, source.as_bytes());
    let mut copied = Vec::new();
    let mut refused = Vec::new();
    // A copied action of another owner than GitHub's, pinned to a commit: it may use others by
    // tag, which the plan says.
    let mut nested = false;
    for (index, item) in items.into_iter().enumerate() {
        let uses = item.iter().enumerate().find_map(|(n, l)| {
            let key = if n == 0 {
                l.trim_start().strip_prefix("- ")?.trim_start()
            } else if indent(l) == base + 2 {
                l.trim_start()
            } else {
                return None;
            };
            key.strip_prefix("uses:").map(|u| {
                strip_comment(u)
                    .trim()
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_string()
            })
        });
        let Some(uses) = uses else {
            continue;
        };
        if uses.starts_with("actions/checkout") {
            continue;
        }
        match pinned_deep(root, &uses, 0) {
            Ok(others) => nested |= others,
            Err(why) => {
                refused.push(why);
                continue;
            }
        }
        let text = item
            .iter()
            .map(|l| format!("      {}", &l[base.min(indent(l))..]))
            .collect::<Vec<_>>()
            .join("\n");
        let values = match &yaml {
            Some(yaml) => replay::github_values(yaml, job, Some(index), None).map(|j| j.values),
            None => Err("the workflow doesn't read as YAML".to_string()),
        };
        let filled = values.and_then(|values| {
            let text = filled_yaml(&text, &values).map_err(|expression| {
                format!("it takes {expression}, which only the build job's run knows")
            })?;
            match &yaml {
                Some(yaml) => with_job_env(&text, yaml, job, &values),
                None => Ok(text),
            }
        });
        match filled {
            Ok(text) => copied.push(text),
            Err(why) => refused.push(format!("the step that uses {uses} can't be copied: {why}")),
        }
    }
    if refused.is_empty() {
        Ok((copied, nested))
    } else {
        Err(refused)
    }
}

/// How many of this repository's actions deep [`pinned_deep`] follows a `uses: ./…`.
const LOCAL_ACTION_DEPTH: usize = 4;

/// ADR 0102: [`pinned`], and for an action of this repository (`./…`, at `root`), the same of
/// every action its `action.yml` (or `action.yaml`) uses, and of its container image, followed
/// into this repository's actions up to [`LOCAL_ACTION_DEPTH`] deep: what a step runs in the job
/// that signs can't change under it however many actions deep it is. One that can't be read
/// (missing, not YAML, outside the repository, or deeper than that) is refused as one not pinned.
/// `Ok(true)` when an action of another owner than GitHub's is among them (pinned to a commit,
/// it may still use others by tag, which init can't read).
fn pinned_deep(root: &Path, uses: &str, depth: usize) -> Result<bool, String> {
    pinned(uses)?;
    let Some(dir) = uses.strip_prefix("./") else {
        return Ok(third_party(uses));
    };
    let dir = dir.trim_end_matches('/');
    let fix = "the bake job holds the image's signing identity, so what a step runs must not change under it: pin what it uses, or the bake job doesn't build";
    if depth >= LOCAL_ACTION_DEPTH {
        return Err(format!(
            "`{uses}` is this repository's actions more than {LOCAL_ACTION_DEPTH} deep, which init reads no further: {fix}"
        ));
    }
    if dir.split('/').any(|part| part == "..") {
        return Err(format!(
            "`{uses}` names a folder outside the repository, which init doesn't read: {fix}"
        ));
    }
    let read = ["action.yml", "action.yaml"].iter().find_map(|name| {
        let path = format!("{dir}/{name}");
        std::fs::read(root.join(&path))
            .ok()
            .map(|bytes| (path, bytes))
    });
    let Some((path, bytes)) = read else {
        return Err(format!(
            "`{uses}` has no action.yml or action.yaml init can read, so what it runs can't be checked: {fix}"
        ));
    };
    let Some(action) = super::variants::parse(&path, &bytes) else {
        return Err(format!(
            "{path} doesn't read as YAML, so what it runs can't be checked: {fix}"
        ));
    };
    let runs = &action["runs"];
    if let Some(image) = runs
        .get("image")
        .and_then(|i| i.as_str())
        .filter(|i| i.starts_with("docker://"))
    {
        pinned(image).map_err(|why| format!("{path}: {why}"))?;
    }
    let mut others = false;
    for step in runs["steps"].as_array().into_iter().flatten() {
        if let Some(inner) = step.get("uses").and_then(|u| u.as_str()) {
            others |= pinned_deep(root, inner.trim(), depth + 1)
                .map_err(|why| format!("{path} uses {inner}: {why}"))?;
        }
    }
    Ok(others)
}

/// Whether a `uses:` is an action of another owner than this repository and GitHub.
fn third_party(uses: &str) -> bool {
    let owner = uses.split('/').next().unwrap_or_default();
    !uses.starts_with("./")
        && !uses.starts_with("docker://")
        && !matches!(owner, "actions" | "github")
}

/// B-75: a copied step's YAML `text` with what CI fills in filled in from `values`
/// ([`super::replay::resolve`]), each value keeping its type: a plain scalar something was filled
/// into becomes a single-quoted string (CI fills an expression in as text, and `1.80` filled in
/// plain would read as the number 1.8), one in quotes gets its quote escaped, and a block's lines
/// take the text as it is. `Err` is the first expression that can't be filled in.
fn filled_yaml(text: &str, values: &super::replay::Values) -> Result<String, String> {
    use super::replay::{Keep, resolve};
    let fill = |s: &str| resolve(s, values, Keep::SameInAnotherJob);
    let mut out = Vec::new();
    let mut block: Option<usize> = None;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        if let Some(at) = block {
            if line.trim().is_empty() || indent > at {
                out.push(fill(line)?);
                continue;
            }
            block = None;
        }
        let body = line.trim_start();
        let item = body.strip_prefix("- ").unwrap_or(body);
        let lead = &line[..line.len() - item.len()];
        let Some((key, value)) = item.split_once(": ").filter(|(k, _)| !k.contains(' ')) else {
            out.push(fill(line)?);
            continue;
        };
        let value_trimmed = value.trim_start();
        if value_trimmed.starts_with('|') || value_trimmed.starts_with('>') {
            block = Some(indent);
            out.push(line.to_string());
            continue;
        }
        // A condition is an expression with or without `${{ }}`: its matrix keys are filled in
        // either way, and it is written back as a quoted string.
        if key == "if" {
            let plain = if let Some(inner) = value_trimmed
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
            {
                inner.replace("''", "'")
            } else if let Some(inner) = value_trimmed
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
            {
                inner.replace("\\\"", "\"").replace("\\\\", "\\")
            } else {
                strip_comment(value_trimmed).trim_end().to_string()
            };
            let filled = filled_condition(&plain, values)?;
            out.push(format!("{lead}{key}: '{}'", filled.replace('\'', "''")));
            continue;
        }
        if !value.contains("${{") {
            out.push(line.to_string());
            continue;
        }
        let filled = if let Some(inner) = value_trimmed
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
        {
            format!("'{}'", fill(inner)?.replace('\'', "''"))
        } else if let Some(inner) = value_trimmed
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
        {
            let done = fill(inner)?;
            if done == inner {
                format!("\"{inner}\"")
            } else {
                format!("\"{}\"", done.replace('\\', "\\\\").replace('"', "\\\""))
            }
        } else {
            let (plain, comment) = match value_trimmed.find(" #") {
                Some(at) => (&value_trimmed[..at], &value_trimmed[at..]),
                None => (value_trimmed, ""),
            };
            let done = fill(plain.trim_end())?;
            if done == plain.trim_end() {
                format!("{plain}{comment}")
            } else {
                format!("'{}'{comment}", done.replace('\'', "''"))
            }
        };
        out.push(format!("{lead}{key}: {filled}"));
    }
    Ok(out.join("\n"))
}

/// B-75: a copied step's `text` with the variables the build job gives every step of its own
/// (the workflow's `env:`, then the job's), filled in as [`super::replay::resolve`] fills them,
/// added to its `env:` where the step doesn't set them itself: the bake job is another workflow,
/// and its job sets none of them. `Err` names a variable that can't be filled in.
fn with_job_env(
    text: &str,
    yaml: &serde_json::Value,
    job: &str,
    values: &super::replay::Values,
) -> Result<String, String> {
    use super::replay::{Keep, resolve, text_of};
    let mut env: Vec<(String, String)> = Vec::new();
    for level in [yaml.get("env"), yaml["jobs"][job].get("env")] {
        let Some(map) = level.and_then(|l| l.as_object()) else {
            continue;
        };
        for (name, value) in map {
            let Some(raw) = text_of(value) else {
                return Err(format!("the build job's variable {name} isn't text"));
            };
            let filled = resolve(&raw, values, Keep::SameInAnotherJob).map_err(|expression| {
                format!(
                    "the build job's variable {name} takes {expression}, which only the build job's run knows"
                )
            })?;
            env.retain(|(n, _)| n != name);
            env.push((name.clone(), filled));
        }
    }
    if env.is_empty() {
        return Ok(text.to_string());
    }
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let at = lines
        .iter()
        .position(|l| indent(l) == 8 && l.trim() == "env:");
    let own: Vec<String> = at.map_or_else(Vec::new, |at| {
        lines[at + 1..]
            .iter()
            .take_while(|l| l.trim().is_empty() || indent(l) > 8)
            .filter(|l| indent(l) == 10)
            .filter_map(|l| l.trim().split(':').next().map(str::to_string))
            .collect()
    });
    let added: Vec<String> = env
        .iter()
        .filter(|(name, _)| !own.contains(name))
        .map(|(name, value)| {
            format!(
                "          {name}: {}",
                serde_json::to_string(value).unwrap_or_default()
            )
        })
        .collect();
    match at {
        Some(at) => {
            lines.splice(at + 1..at + 1, added);
        }
        None => {
            lines.push("        env:".to_string());
            lines.extend(added);
        }
    }
    Ok(lines.join("\n"))
}

/// B-75: a copied step's `if:` condition, a GitHub expression with or without `${{ }}`, with
/// each `matrix.KEY` written as the literal of the build job's primary entry's value, as
/// [`super::replay::resolve`] fills `${{ matrix.KEY }}`. `Err` names a key that has no value, or
/// a context only the build job's run knows (`steps`, `needs`, `inputs`, `strategy`, `job`,
/// `jobs`, `github.event`).
fn filled_condition(condition: &str, values: &super::replay::Values) -> Result<String, String> {
    let mut out = String::new();
    let chars: Vec<char> = condition.chars().collect();
    let mut i = 0;
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.';
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            // A string literal, `''` its quote.
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(chars.len());
            out.extend(&chars[start..i]);
            continue;
        }
        let starts = (c.is_ascii_alphabetic() || c == '_') && (i == 0 || !word(chars[i - 1]));
        if !starts {
            out.push(c);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && word(chars[i]) {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();
        let context = name.split('.').next().unwrap_or_default();
        if let Some(key) = name.strip_prefix("matrix.") {
            let value = values
                .matrix
                .as_ref()
                .and_then(|m| m.get(key))
                .ok_or_else(|| format!("`{name}`"))?;
            out.push_str(&match value {
                serde_json::Value::String(text) => format!("'{}'", text.replace('\'', "''")),
                serde_json::Value::Null => "null".to_string(),
                serde_json::Value::Bool(_) | serde_json::Value::Number(_) => value.to_string(),
                _ => return Err(format!("`{name}`")),
            });
        } else if matches!(
            context,
            "steps" | "needs" | "inputs" | "strategy" | "job" | "jobs" | "matrix"
        ) || name.starts_with("github.event")
        {
            return Err(format!("`{name}`"));
        } else {
            out.push_str(&name);
        }
    }
    Ok(out)
}

/// ADR 0102: whether a step's `uses:` may run in the job that signs the image: an action of this
/// repository (`./…`), one of GitHub's own (`actions/…`, `github/…`, which run its runners), one
/// pinned to a full commit (40 hex), or a container pinned by its digest. `Err` says how to pin
/// it: init reads nothing over the network, so it can't look the commit up itself.
fn pinned(uses: &str) -> Result<(), String> {
    if uses.starts_with("./") {
        return Ok(());
    }
    if let Some(image) = uses.strip_prefix("docker://") {
        return if image.contains("@sha256:") {
            Ok(())
        } else {
            Err(format!(
                "`{uses}` isn't pinned to a digest: write it as docker://<image>@sha256:<digest> in the build job"
            ))
        };
    }
    let (action, reference) = uses.split_once('@').unwrap_or((uses, ""));
    let owner = action.split('/').next().unwrap_or_default();
    if matches!(owner, "actions" | "github") {
        return Ok(());
    }
    let commit = reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit());
    if commit {
        return Ok(());
    }
    let repository = action.split('/').take(2).collect::<Vec<_>>().join("/");
    Err(format!(
        "`{uses}` isn't pinned to a commit, and the bake job holds the image's signing identity: in the build job, write `uses: {action}@<its 40-hex commit> # {reference}` (`git ls-remote https://github.com/{repository} {reference}` prints the commit)"
    ))
}

/// A YAML line's value without a trailing `# comment`.
fn strip_comment(value: &str) -> &str {
    match value.find(" #") {
        Some(at) => &value[..at],
        None => value,
    }
}

/// B-75: what a GitLab build job `job` in `source` says of its environment, which the bake job
/// takes: its `image:` (a scalar), and its `before_script:` as written.
fn gitlab_setup(source: &str, job: &str) -> (Option<String>, Option<String>) {
    let lines: Vec<&str> = source.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let content = |l: &str| !l.trim().is_empty() && !l.trim_start().starts_with('#');
    let Some(header) = lines
        .iter()
        .position(|l| indent(l) == 0 && l.trim_end() == format!("{job}:"))
    else {
        return (None, None);
    };
    let end = (header + 1..lines.len())
        .find(|&i| content(lines[i]) && indent(lines[i]) == 0)
        .unwrap_or(lines.len());
    let key_indent = (header + 1..end)
        .find(|&i| content(lines[i]))
        .map_or(2, |i| indent(lines[i]));
    let mut image = None;
    let mut before = None;
    for i in header + 1..end {
        let l = lines[i];
        if !content(l) || indent(l) != key_indent {
            continue;
        }
        let key = l.trim_start();
        if let Some(value) = key.strip_prefix("image:") {
            let value = value.trim();
            if !value.is_empty() {
                image = Some(value.to_string());
            }
        } else if key.trim_end() == "before_script:" {
            let block_end = (i + 1..end)
                .find(|&j| {
                    content(lines[j])
                        && indent(lines[j]) <= key_indent
                        && !lines[j].trim_start().starts_with("- ")
                })
                .unwrap_or(end);
            before = Some(
                lines[i..block_end]
                    .iter()
                    .map(|l| format!("  {}", &l[key_indent.min(indent(l))..]))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }
    (image, before)
}

/// B-69: what the plan says of the branches the job bakes: each one's tag, and who signs it.
fn branch_notes(bake: &mut Bake, image: &str, branches: &[&str], subject: &str) {
    if !branches.is_empty() && subject.contains(GITHUB_PATH) {
        bake.notes.push(format!(
            "A run by hand on a branch runs that branch's own {GITHUB_PATH}: once this change lands on the default branch, merge the default branch into {} (a branch cut before has no copy, and its bake can't start)",
            branches.join(", ")
        ));
    }
    for name in branches {
        bake.notes.push(format!(
            "{name}'s builds bake into {image}:{}, signed as the job on {name} ({subject}{name}). A run trusts that image only where hushrun.yaml's build: baked: signers names that subject, and naming it lets every run that reads that hushrun.yaml trust {name}'s images, the default branch's runs too",
            hushrun_core::blocks::image::branch_tag("linux-x64", name)
        ));
    }
}

/// The YAML list of `items`, each single-quoted: `['a', 'b']`.
fn yaml_list(items: &[&str]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|i| format!("'{}'", i.replace('\'', "''")))
        .collect();
    format!("[{}]", quoted.join(", "))
}

#[allow(clippy::too_many_arguments)]
fn github_text(
    workflow: &str,
    branch: &str,
    image: &str,
    package: &str,
    hosted: bool,
    branches: &[&str],
    setup: &[String],
    builds: bool,
) -> String {
    let workflow = workflow.replace('\'', "''");
    // B-75: with no setup steps it can trust, the bake job doesn't build.
    let no_build = if builds { "" } else { " --no-build" };
    // B-75: the build's own setup steps, so a build of what's missing has its toolchain.
    let setup: String = setup.iter().map(|step| format!("{step}\n")).collect();
    // B-70: hushrun's registry takes the image from the backend, which the ID token reaches;
    // GHCR takes it pushed, with the job's own token.
    let (packages, env) = if hosted {
        ("", "")
    } else {
        (
            "  packages: write   # pushes the image to this repository's packages\n",
            "        env:\n          GITHUB_TOKEN: ${{ github.token }}\n",
        )
    };
    let default_ref =
        "github.ref == format('refs/heads/{0}', github.event.repository.default_branch)";
    // B-73: a scheduled run's event names no repository (no webhook is behind it), and GitHub runs
    // it only on the default branch's latest commit: that event alone needs no compare.
    let on_default = format!("(github.event_name == 'schedule' || {default_ref})");
    let after_push = "github.event.workflow_run.conclusion == 'success' && github.event.workflow_run.event == 'push' && github.event.workflow_run.head_repository.full_name == github.repository";
    if branches.is_empty() {
        return format!(
            "# hushrun: bakes the compiled code of {branch}'s build into an image that sandboxes and containers
# start warm from (hushrun build bake). It runs after '{workflow}' passes on {branch}, and by hand.
# A Dockerfile takes the image with one line:
#   COPY --from={image}:hushrun-linux-x64-latest /hushrun /hushrun
name: hushrun bake
on:
  workflow_run:
    workflows: ['{workflow}']
    branches: [{branch}]
    types: [completed]
  workflow_dispatch:
  # Daily, so that an image the re-bake limit held back catches up with {branch} (B-73).
  schedule:
    - cron: '17 4 * * *'
# One bake at a time: a later one never lands under an earlier one's tag.
concurrency:
  group: hushrun-bake
  cancel-in-progress: false
permissions:
  contents: read
{packages}  id-token: write   # reads the build from hushrun's backend, and signs the image keylessly
jobs:
  bake:
    # By hand on the default branch alone, or on the schedule (which GitHub runs on the default
    # branch alone); after the build only for a push to it, in this repository (never a fork's
    # pull request whose branch has the same name).
    if: (github.event_name == 'workflow_dispatch' && {default_ref}) || github.event_name == 'schedule' || ({after_push})
    runs-on: ubuntu-latest
    # What the bake published, by digest, for a later job to pull or build a template from.
    outputs:
      hushrun-image: ${{{{ steps.bake.outputs.hushrun-image }}}}
      hushrun-image-digest: ${{{{ steps.bake.outputs.hushrun-image-digest }}}}
      hushrun-image-tag: ${{{{ steps.bake.outputs.hushrun-image-tag }}}}
      hushrun-image-published: ${{{{ steps.bake.outputs.hushrun-image-published }}}}
      hushrun-manifest: ${{{{ steps.bake.outputs.hushrun-manifest }}}}
    steps:
      # The whole history: the bake builds only a commit that comes after the backend's newest
      # build (B-75), which a shallow checkout can't tell.
      - uses: actions/checkout@v5
        with:
          fetch-depth: 0
{setup}      - id: bake
        run: npx --yes {package} build bake --from backend --trusted{no_build} --oci {image}
{env}"
        );
    }
    // B-69: a long-lived branch's build is baked by a run of this workflow by hand on that
    // branch, which the second job starts: `workflow_run` always runs the default branch's file,
    // and a keyless signature names the file and branch that ran, so only a run on the branch
    // itself signs as that branch's workflow, never as the default branch's.
    let mut all = vec![branch];
    all.extend(branches.iter().copied());
    let dispatched: Vec<String> = branches
        .iter()
        .map(|b| format!("github.ref == 'refs/heads/{}'", b.replace('\'', "''")))
        .collect();
    format!(
        "# hushrun: bakes the compiled code of {branch}'s build into an image that sandboxes and containers
# start warm from (hushrun build bake). It runs after '{workflow}' passes on {branch}, and by hand.
# A Dockerfile takes the image with one line:
#   COPY --from={image}:hushrun-linux-x64-latest /hushrun /hushrun
# The builds of {names} bake into hushrun-linux-x64-branch-<name>-latest, each in a run of this
# workflow on its own branch, signed as that branch's workflow: each branch needs this file too.
name: hushrun bake
on:
  workflow_run:
    workflows: ['{workflow}']
    branches: {all}
    types: [completed]
  workflow_dispatch:
  # Daily, so that an image the re-bake limit held back catches up with {branch} (B-73).
  schedule:
    - cron: '17 4 * * *'
permissions:
  contents: read
{packages}  id-token: write   # reads the build from hushrun's backend, and signs the image keylessly
jobs:
  bake:
    # By hand on the default branch or a branch baked here, or on the schedule (which GitHub runs
    # on the default branch alone); after the build only for a push to the default branch, in
    # this repository (never a fork's pull request of the same name).
    if: (github.event_name == 'workflow_dispatch' && ({default_ref} || {dispatched})) || github.event_name == 'schedule' || ({after_push} && github.event.workflow_run.head_branch == github.event.repository.default_branch)
    # One bake at a time per branch, whatever started it: a later one never lands under an
    # earlier one's tag, and one branch's bakes never wait on, nor cancel, another's.
    concurrency:
      group: hushrun-bake-${{{{ github.event.workflow_run.head_branch || github.ref_name }}}}
      cancel-in-progress: false
    runs-on: ubuntu-latest
    # What the bake published, by digest, for a later job to pull or build a template from.
    outputs:
      hushrun-image: ${{{{ steps.bake.outputs.hushrun-image || steps.bake_branch.outputs.hushrun-image }}}}
      hushrun-image-digest: ${{{{ steps.bake.outputs.hushrun-image-digest || steps.bake_branch.outputs.hushrun-image-digest }}}}
      hushrun-image-tag: ${{{{ steps.bake.outputs.hushrun-image-tag || steps.bake_branch.outputs.hushrun-image-tag }}}}
      hushrun-image-published: ${{{{ steps.bake.outputs.hushrun-image-published || steps.bake_branch.outputs.hushrun-image-published }}}}
      hushrun-manifest: ${{{{ steps.bake.outputs.hushrun-manifest || steps.bake_branch.outputs.hushrun-manifest }}}}
    steps:
      # The whole history: the bake builds only a commit that comes after the backend's newest
      # build (B-75), which a shallow checkout can't tell.
      - uses: actions/checkout@v5
        with:
          fetch-depth: 0
{setup}      - id: bake
        if: {on_default}
        run: npx --yes {package} build bake --from backend --trusted{no_build} --oci {image}
{env}      - id: bake_branch
        if: github.event_name != 'schedule' && github.ref != format('refs/heads/{{0}}', github.event.repository.default_branch)
        run: npx --yes {package} build bake --from backend --branch-tag --oci {image}
{env}  branch:
    # After a branch's build passed for a push to it, in this repository: a run of this workflow
    # by hand on that branch bakes it. A branch cut before this file landed has no copy of it,
    # and its run can't start until the default branch is merged into it.
    if: {after_push} && github.event.workflow_run.head_branch != github.event.repository.default_branch
    runs-on: ubuntu-latest
    permissions:
      actions: write   # starts this workflow on the branch
    steps:
      - run: |
          gh workflow run hushrun-bake.yml --repo \"$GITHUB_REPOSITORY\" --ref \"$BRANCH\" || {{
            echo \"::error::$BRANCH has no .github/workflows/hushrun-bake.yml yet: merge the default branch into it, and its builds bake\"
            exit 1
          }}
        env:
          GH_TOKEN: ${{{{ github.token }}}}
          BRANCH: ${{{{ github.event.workflow_run.head_branch }}}}
",
        names = branches.join(", "),
        all = yaml_list(&all),
        dispatched = dispatched.join(" || "),
    )
}

fn gitlab_text(
    backend: &str,
    hosted: Option<&str>,
    branches: &[&str],
    image_of_build: Option<&str>,
    before_script: Option<&str>,
) -> String {
    // B-75: the build job's image and setup, so a build of what's missing has its toolchain.
    let build_image = image_of_build.unwrap_or("rust:latest");
    let before = before_script.map_or(String::new(), |b| format!("{b}\n"));
    let image = hosted.unwrap_or("<the project's registry image>");
    let oci = hosted.unwrap_or("\"$CI_REGISTRY_IMAGE\"");
    // B-69: a long-lived branch's pipeline bakes too, as the job on that branch (its ID token,
    // and so its signature, names the branch), into the branch's tag; one group per branch.
    let (group, rules, bake) = if branches.is_empty() {
        (
            GITLAB_JOB.to_string(),
            String::new(),
            format!("hushrun build bake --from backend --trusted --oci {oci}"),
        )
    } else {
        let rules: String = branches
            .iter()
            .map(|b| {
                format!(
                    "    - if: $CI_COMMIT_BRANCH == \"{b}\" && ($CI_PIPELINE_SOURCE == \"push\" || $CI_PIPELINE_SOURCE == \"web\")\n"
                )
            })
            .collect();
        (
            format!("{GITLAB_JOB}-$CI_COMMIT_REF_SLUG"),
            rules,
            format!(
                "if [ \"$CI_COMMIT_BRANCH\" = \"$CI_DEFAULT_BRANCH\" ]; then hushrun build bake --from backend --trusted --oci {oci}; else hushrun build bake --from backend --branch-tag --oci {oci}; fi"
            ),
        )
    };
    format!(
        "
# hushrun: bakes the compiled code of the default branch's build into an image that sandboxes and
# containers start warm from (hushrun build bake), after the pipeline and when run from the web.
# A Dockerfile takes the image with one line:
#   COPY --from={image}:hushrun-linux-x64-latest /hushrun /hushrun
{GITLAB_JOB}:
  stage: .post
  image: {build_image}
  resource_group: {group}
  rules:
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH && $CI_PIPELINE_SOURCE == \"push\"
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH && $CI_PIPELINE_SOURCE == \"web\"
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH && $CI_PIPELINE_SOURCE == \"schedule\"
{rules}  # The whole history: the bake builds only a commit that comes after the backend's newest
  # build (B-75), which a shallow clone can't tell.
  variables:
    GIT_DEPTH: \"0\"
    # What the bake published, by digest, for a later job (`needs: [{GITLAB_JOB}]`) to read.
    HUSHRUN_BAKE_ENV: hushrun-bake.env
  artifacts:
    reports:
      dotenv: hushrun-bake.env
  id_tokens:
    HUSHRUN_ID_TOKEN:
      aud: {backend}
    SIGSTORE_ID_TOKEN:
      aud: sigstore
{before}  script:
    - export PATH=\"$(dirname \"$(sh ci/install-hushrun.sh)\"):$PATH\"
    - {bake}
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(text: &str) -> Workflow {
        Workflow::parse(".github/workflows/ci.yml".into(), text.to_string())
    }

    // covers: B-69, B-244
    #[test]
    fn a_github_repository_gets_a_bake_workflow_after_its_build() {
        let dir = tempfile::tempdir().unwrap();
        let ci = github(
            "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo build\n",
        );
        let bake = plan(
            dir.path(),
            Some("github.com/Acme/App"),
            &[ci],
            &[],
            &["build".into()],
            Some("https://hushrun.example/acme"),
            "hushrun@^0.18.0",
            &[],
        );
        assert_eq!(bake.files.len(), 1);
        let (path, text, _) = &bake.files[0];
        assert_eq!(path, GITHUB_PATH);
        assert!(text.contains("workflows: ['CI']"), "{text}");
        assert!(text.contains("branches: [main]"), "{text}");
        assert!(text.contains("packages: write"), "{text}");
        assert!(text.contains("id-token: write"), "{text}");
        assert!(
            text.contains(
                "npx --yes hushrun@^0.18.0 build bake --from backend --trusted --oci ghcr.io/acme/app"
            ),
            "{text}"
        );
        assert!(text.contains("GITHUB_TOKEN: ${{ github.token }}"), "{text}");
        // Run by hand only on the default branch; after the build only for a push to it, here.
        assert!(
            text.contains(
                "github.ref == format('refs/heads/{0}', github.event.repository.default_branch)"
            ),
            "{text}"
        );
        assert!(
            text.contains("github.event.workflow_run.event == 'push'"),
            "{text}"
        );
        assert!(
            text.contains(
                "github.event.workflow_run.head_repository.full_name == github.repository"
            ),
            "{text}"
        );
        // It reads as YAML, as GitHub must read it.
        let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        assert_eq!(yaml["jobs"]["bake"]["runs-on"], "ubuntu-latest");
        // The bake's outputs (B-244) reach the job's, for a later job to pull by digest.
        assert_eq!(
            yaml["jobs"]["bake"]["outputs"]["hushrun-image-digest"],
            "${{ steps.bake.outputs.hushrun-image-digest }}"
        );
        assert!(bake.notes.iter().any(|n| {
            n.contains("COPY --from=ghcr.io/acme/app:hushrun-linux-x64-latest /hushrun /hushrun")
        }));
        assert!(bake.manual.iter().any(|n| n.contains(
            "subject: https://github.com/Acme/App/.github/workflows/hushrun-bake.yml@refs/heads/main"
        )));
        // Without a backend there is nothing to bake from.
        let none = plan(
            dir.path(),
            Some("github.com/a/b"),
            &[],
            &[],
            &[],
            None,
            "x",
            &[],
        );
        assert!(none.files.is_empty());
        assert!(none.notes[0].contains("needs a backend"));
    }

    // covers: B-69, B-70
    #[test]
    fn a_repository_on_the_hosted_backend_bakes_into_hushrun_s_registry() {
        let dir = tempfile::tempdir().unwrap();
        let ci = github(
            "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo build\n",
        );
        let bake = plan(
            dir.path(),
            Some("github.com/Acme/Shop"),
            &[ci],
            &[],
            &["build".into()],
            Some("https://api.hushrun.com/o/acme"),
            "hushrun@^0.18.0",
            &[],
        );
        let (path, text, changes) = &bake.files[0];
        // The workflow keeps its file: the backend trusts a publish by that file's identity.
        assert_eq!(path, GITHUB_PATH);
        assert!(
            text.contains(
                "build bake --from backend --trusted --oci registry.hushrun.com/acme/shop\n"
            ),
            "{text}"
        );
        // Nothing is pushed: no packages permission, no GitHub token.
        assert!(!text.contains("packages: write"), "{text}");
        assert!(!text.contains("GITHUB_TOKEN"), "{text}");
        assert!(text.contains("id-token: write"), "{text}");
        assert!(changes[0].contains("registry.hushrun.com/acme/shop"));
        let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        assert_eq!(yaml["permissions"]["id-token"], "write");
        assert!(bake.notes.iter().any(|n| n.contains(
            "COPY --from=registry.hushrun.com/acme/shop:hushrun-linux-x64-latest /hushrun /hushrun"
        )));
        assert!(
            bake.notes.iter().any(
                |n| n.contains("docker login registry.hushrun.com") && n.contains("pull token")
            )
        );
        // A GitLab project there too.
        let ci = Workflow::parse_gitlab("test:\n  script:\n    - cargo test\n".to_string());
        let bake = plan(
            dir.path(),
            Some("gitlab.com/acme/app"),
            &[ci],
            &[],
            &["test".into()],
            Some("https://api.hushrun.com/o/acme/"),
            "hushrun",
            &[],
        );
        let (_, text, _) = &bake.files[0];
        assert!(
            text.contains("--trusted --oci registry.hushrun.com/acme/app\n"),
            "{text}"
        );
        assert!(!text.contains("CI_REGISTRY_IMAGE"), "{text}");
        let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        assert_eq!(yaml["hushrun-bake"]["stage"], ".post");
        // Any other backend, or one not known yet, keeps the repository's own registry.
        assert_eq!(
            hosted_image("https://hushrun.acme.dev/o/acme", "github.com/a/b"),
            None
        );
        assert_eq!(
            hosted_image("<your org's URL, once you sign in>", "github.com/a/b"),
            None
        );
        assert_eq!(
            hosted_image("https://api.hushrun.com/o/acme", "github.com/a/b").as_deref(),
            Some("registry.hushrun.com/acme/b")
        );
    }

    // covers: B-69, B-244
    #[test]
    fn a_gitlab_pipeline_gets_a_bake_job_appended() {
        let dir = tempfile::tempdir().unwrap();
        let ci = Workflow::parse_gitlab("test:\n  script:\n    - cargo test\n".to_string());
        let bake = plan(
            dir.path(),
            Some("gitlab.com/acme/app"),
            &[ci],
            &[],
            &["test".into()],
            Some("https://hushrun.example/acme"),
            "hushrun",
            &[],
        );
        let (path, text, changes) = &bake.files[0];
        assert_eq!(path, ".gitlab-ci.yml");
        assert!(
            text.starts_with("test:\n  script:\n    - cargo test\n"),
            "{text}"
        );
        assert!(text.contains("hushrun-bake:\n  stage: .post"), "{text}");
        assert!(text.contains("    HUSHRUN_BAKE_ENV: hushrun-bake.env\n  artifacts:\n    reports:\n      dotenv: hushrun-bake.env\n"), "{text}");
        assert!(text.contains("aud: https://hushrun.example/acme"), "{text}");
        assert!(
            text.contains("--trusted --oci \"$CI_REGISTRY_IMAGE\"") && !text.contains("--check"),
            "{text}"
        );
        assert!(changes[0].contains("hushrun-bake"));
        assert!(
            text.contains(
                "- if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH && $CI_PIPELINE_SOURCE == \"web\""
            ),
            "{text}"
        );
        assert!(
            !text.contains("- if: $CI_PIPELINE_SOURCE == \"web\"\n"),
            "{text}"
        );
        let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        assert_eq!(yaml["hushrun-bake"]["stage"], ".post");
        assert!(bake.manual.iter().any(|n| {
            n.contains("subject: https://gitlab.com/acme/app//.gitlab-ci.yml@refs/heads/main")
        }));
        assert!(bake.installs);
        // Twice is once.
        let again = Workflow::parse_gitlab(text.clone());
        let twice = plan(
            dir.path(),
            Some("gitlab.com/acme/app"),
            &[again],
            &[],
            &[],
            Some("https://hushrun.example/acme"),
            "hushrun",
            &[],
        );
        assert!(twice.files.is_empty());
    }

    // covers: B-69, B-66
    #[test]
    fn named_branches_bake_into_their_own_tags_signed_as_their_own_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let ci = github(
            "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo build\n",
        );
        let bake = plan(
            dir.path(),
            Some("github.com/Acme/App"),
            &[ci],
            &[],
            &["build".into()],
            Some("https://hushrun.example/acme"),
            "hushrun@^0.18.0",
            &["release".into(), "main".into()],
        );
        let (_, text, _) = &bake.files[0];
        let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        // The build of either branch starts the workflow; the default branch isn't listed twice.
        assert_eq!(
            yaml["on"]["workflow_run"]["branches"],
            serde_json::json!(["main", "release"])
        );
        // The default branch's bake moves -latest; a run on the branch moves the branch's tag.
        let steps = yaml["jobs"]["bake"]["steps"].as_array().unwrap();
        assert!(
            steps[1]["run"]
                .as_str()
                .unwrap()
                .ends_with("--from backend --trusted --oci ghcr.io/acme/app")
        );
        assert!(
            steps[2]["run"]
                .as_str()
                .unwrap()
                .ends_with("--from backend --branch-tag --oci ghcr.io/acme/app")
        );
        let bake_if = yaml["jobs"]["bake"]["if"].as_str().unwrap();
        assert!(
            bake_if.contains("github.ref == 'refs/heads/release'"),
            "{bake_if}"
        );
        assert!(
            bake_if.contains(
                "github.event.workflow_run.head_branch == github.event.repository.default_branch"
            ),
            "{bake_if}"
        );
        // A branch's build is baked by a run by hand on the branch, so its signature names the
        // branch's workflow; the job that starts it can do nothing else.
        let branch = &yaml["jobs"]["branch"];
        assert!(
            branch["if"]
                .as_str()
                .unwrap()
                .contains("head_branch != github.event.repository.default_branch")
        );
        assert_eq!(
            branch["permissions"],
            serde_json::json!({"actions": "write"})
        );
        let run = branch["steps"][0]["run"].as_str().unwrap();
        assert!(
            run.contains("gh workflow run hushrun-bake.yml") && run.contains("--ref \"$BRANCH\""),
            "{run}"
        );
        assert_eq!(
            branch["steps"][0]["env"]["BRANCH"],
            "${{ github.event.workflow_run.head_branch }}"
        );
        // One group per branch, on the bake job alone: a dispatcher never queues behind a bake,
        // and a run of the default branch by hand shares its group with the one after the build.
        assert!(yaml["concurrency"].is_null(), "{text}");
        assert_eq!(
            yaml["jobs"]["bake"]["concurrency"]["group"],
            "hushrun-bake-${{ github.event.workflow_run.head_branch || github.ref_name }}"
        );
        assert!(run.contains("merge the default branch into it"), "{run}");
        assert!(
            bake.notes
                .iter()
                .any(|n| n.contains("merge the default branch into release")),
            "{:?}",
            bake.notes
        );
        assert!(
            bake.notes
                .iter()
                .any(|n| n.contains("hushrun-linux-x64-branch-release-latest")
                    && n.contains("@refs/heads/release"))
        );
        // The signer init prints stays the default branch's alone.
        assert!(
            bake.manual
                .iter()
                .all(|m| !m.contains("refs/heads/release"))
        );
        // No branches: the file is the one it always was.
        let ci = github(
            "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo build\n",
        );
        let plain = plan(
            dir.path(),
            Some("github.com/Acme/App"),
            &[ci],
            &[],
            &["build".into()],
            Some("https://hushrun.example/acme"),
            "hushrun@^0.18.0",
            &["main".into()],
        );
        assert!(!plain.files[0].1.contains("branch-tag"));
        assert!(plain.files[0].1.contains("group: hushrun-bake\n"));
        // GitLab: the branch's own pipeline bakes, into its tag, one group per branch.
        let gl = Workflow::parse_gitlab("test:\n  script:\n    - cargo test\n".to_string());
        let bake = plan(
            dir.path(),
            Some("gitlab.com/acme/app"),
            &[gl],
            &[],
            &["test".into()],
            Some("https://hushrun.example/acme"),
            "hushrun",
            &["release".into()],
        );
        let (_, text, _) = &bake.files[0];
        let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        let job = &yaml["hushrun-bake"];
        assert_eq!(job["resource_group"], "hushrun-bake-$CI_COMMIT_REF_SLUG");
        assert!(
            job["rules"][2]["if"]
                .as_str()
                .unwrap()
                .contains("$CI_PIPELINE_SOURCE == \"schedule\"")
        );
        assert!(
            job["rules"][3]["if"]
                .as_str()
                .unwrap()
                .contains("$CI_COMMIT_BRANCH == \"release\"")
        );
        assert_eq!(job["variables"]["GIT_DEPTH"], "0");
        let script = job["script"][1].as_str().unwrap();
        assert!(
            script.contains("--branch-tag") && script.contains("--trusted"),
            "{script}"
        );
        // The hosted registry has no branch tags.
        let ci = github(
            "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo build\n",
        );
        let hosted = plan(
            dir.path(),
            Some("github.com/Acme/Shop"),
            &[ci],
            &[],
            &["build".into()],
            Some("https://api.hushrun.com/o/acme"),
            "hushrun",
            &["release".into()],
        );
        assert!(!hosted.files[0].1.contains("branch-tag"));
        assert!(hosted.notes.iter().any(|n| n.contains("no branch tags")));
    }

    // covers: B-75
    // ADR 0102 copies a step into the signing job only when what it runs can't change under it.
    // A local composite action (`uses: ./.github/actions/setup`) is held to the same rule as every
    // action it uses, followed into the repository's own actions: one that uses an action by tag,
    // or one init can't read, keeps the bake job from building. One whose actions are all pinned
    // is copied, and the plan says an action pinned by commit may still use others by tag.
    #[test]
    fn a_local_action_s_unpinned_actions_keep_the_bake_job_from_building() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let bake_of = |files: &[(&str, String)]| {
            let dir = tempfile::tempdir().unwrap();
            for (path, text) in files {
                let path = dir.path().join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
            let ci = github(
                "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - uses: ./.github/actions/setup\n      - run: cargo build\n",
            );
            let bake = plan(
                dir.path(),
                Some("github.com/acme/app"),
                &[ci],
                &[],
                &["build".into()],
                Some("https://hushrun.example/acme"),
                "hushrun",
                &[],
            );
            let (_, text, _) = &bake.files[0];
            let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
            let steps = yaml["jobs"]["bake"]["steps"].as_array().unwrap();
            let copied = steps.iter().any(|s| s["uses"] == "./.github/actions/setup");
            let builds = steps
                .iter()
                .filter_map(|s| s["run"].as_str())
                .any(|run| run.contains("build bake") && !run.contains("--no-build"));
            (copied, builds, bake.notes.join("\n"))
        };
        let composite = |uses: &[&str]| {
            let steps: String = uses.iter().map(|u| format!("    - uses: {u}\n")).collect();
            format!("name: setup\nruns:\n  using: composite\n  steps:\n{steps}")
        };
        // By tag, one level down.
        let (copied, builds, notes) = bake_of(&[(
            ".github/actions/setup/action.yml",
            composite(&["dtolnay/rust-toolchain@stable", "Swatinem/rust-cache@v2"]),
        )]);
        assert!(!copied && !builds, "{notes}");
        assert!(notes.contains("dtolnay/rust-toolchain@stable"), "{notes}");
        // By tag, two levels down, through another local action in action.yaml.
        let (copied, builds, notes) = bake_of(&[
            (
                ".github/actions/setup/action.yml",
                composite(&["./.github/actions/rust"]),
            ),
            (
                ".github/actions/rust/action.yaml",
                composite(&["Swatinem/rust-cache@v2"]),
            ),
        ]);
        assert!(!copied && !builds, "{notes}");
        // No action file to read: refused as one not pinned.
        let (copied, builds, notes) = bake_of(&[]);
        assert!(!copied && !builds, "{notes}");
        assert!(notes.contains("no action.yml or action.yaml"), "{notes}");
        // Local actions that use themselves go no deeper than the bound.
        let (copied, builds, _) = bake_of(&[(
            ".github/actions/setup/action.yml",
            composite(&["./.github/actions/setup"]),
        )]);
        assert!(!copied && !builds);
        // All pinned: copied, and the job builds; the plan says what a pinned action may use.
        let (copied, builds, notes) = bake_of(&[(
            ".github/actions/setup/action.yml",
            composite(&[&format!("dtolnay/rust-toolchain@{sha}"), "actions/cache@v4"]),
        )]);
        assert!(copied && builds, "{notes}");
        assert!(
            notes.contains("can still use actions of its own by tag"),
            "{notes}"
        );
    }

    // covers: B-75
    // A copied step's `if:` is an expression with or without `${{ }}`: its matrix keys are
    // filled in either way, so it reads in the bake job as in the build job; and a copied step
    // gets the variables the build job gives every step (the workflow's and the job's `env:`),
    // where it doesn't set them itself.
    #[test]
    fn a_copied_step_keeps_its_condition_and_the_build_job_s_variables() {
        let dir = tempfile::tempdir().unwrap();
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let ci = github(&format!(
            "name: CI\non: push\nenv:\n  CARGO_TERM_COLOR: always\njobs:\n  build:\n    runs-on: ${{{{ matrix.os }}}}\n    strategy:\n      matrix:\n        os: [ubuntu-latest, macos-latest]\n    env:\n      RUSTFLAGS: -D warnings\n      KEY: job\n    steps:\n      - uses: actions/checkout@v5\n      - uses: dtolnay/rust-toolchain@{sha}\n        if: matrix.os == 'ubuntu-latest'\n      - uses: Swatinem/rust-cache@{sha}\n        if: ${{{{ matrix.os != 'macos-latest' }}}}\n        env:\n          KEY: step\n      - run: cargo build\n"
        ));
        let bake = plan(
            dir.path(),
            Some("github.com/acme/app"),
            &[ci],
            &[],
            &["build".into()],
            Some("https://hushrun.example/acme"),
            "hushrun",
            &[],
        );
        let (_, text, _) = &bake.files[0];
        let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        let steps = yaml["jobs"]["bake"]["steps"].as_array().unwrap();
        assert_eq!(
            steps[1]["if"], "'ubuntu-latest' == 'ubuntu-latest'",
            "{text}"
        );
        assert_eq!(
            steps[2]["if"], "${{ 'ubuntu-latest' != 'macos-latest' }}",
            "{text}"
        );
        assert_eq!(steps[1]["env"]["CARGO_TERM_COLOR"], "always", "{text}");
        assert_eq!(steps[1]["env"]["RUSTFLAGS"], "-D warnings", "{text}");
        assert_eq!(steps[1]["env"]["KEY"], "job", "{text}");
        // The step's own value stays.
        assert_eq!(steps[2]["env"]["KEY"], "step", "{text}");
        assert_eq!(steps[2]["env"]["RUSTFLAGS"], "-D warnings", "{text}");
        // A condition on what only the build job's run knows refuses the copy.
        let ci = github(&format!(
            "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - uses: Swatinem/rust-cache@{sha}\n        if: steps.k.outputs.hit != 'true'\n      - run: cargo build\n"
        ));
        let bake = plan(
            dir.path(),
            Some("github.com/acme/app"),
            &[ci],
            &[],
            &["build".into()],
            Some("https://hushrun.example/acme"),
            "hushrun",
            &[],
        );
        assert!(
            bake.files[0].1.contains("--no-build"),
            "{}",
            bake.files[0].1
        );
    }

    // covers: B-73
    // A scheduled run's event names no repository, so `github.event.repository.default_branch` is
    // empty in it: the schedule's arm of the job's `if:`, and the trusted bake step's, stand
    // without that compare (GitHub runs a schedule on the default branch alone), and the other
    // step never runs on it.
    #[test]
    fn a_scheduled_run_bakes_with_no_repository_in_its_event() {
        let dir = tempfile::tempdir().unwrap();
        let ci = || {
            github(
                "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo build\n",
            )
        };
        for branches in [Vec::new(), vec!["release".to_string()]] {
            let bake = plan(
                dir.path(),
                Some("github.com/acme/app"),
                &[ci()],
                &[],
                &["build".into()],
                Some("https://hushrun.example/acme"),
                "hushrun",
                &branches,
            );
            let (_, text, _) = &bake.files[0];
            let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
            let job_if = yaml["jobs"]["bake"]["if"].as_str().unwrap();
            assert!(
                job_if.contains(" || github.event_name == 'schedule' || "),
                "the schedule's arm compares with a branch its event doesn't name: {job_if}"
            );
            let steps = yaml["jobs"]["bake"]["steps"].as_array().unwrap();
            let trusted = steps
                .iter()
                .find(|s| s["run"].as_str().is_some_and(|r| r.contains("--trusted")))
                .unwrap();
            if let Some(step_if) = trusted["if"].as_str() {
                assert!(
                    step_if.starts_with("(github.event_name == 'schedule' || "),
                    "{step_if}"
                );
                let other = steps
                    .iter()
                    .find(|s| {
                        s["run"]
                            .as_str()
                            .is_some_and(|r| r.contains("--branch-tag"))
                    })
                    .unwrap();
                assert!(
                    other["if"]
                        .as_str()
                        .unwrap()
                        .starts_with("github.event_name != 'schedule' && "),
                    "{text}"
                );
            }
        }
    }

    // covers: B-75
    // A filled-in value keeps its type: CI fills an expression in as text.
    #[test]
    fn a_filled_in_step_keeps_each_value_s_type() {
        let values = super::super::replay::Values {
            matrix: serde_json::json!({"rust": "1.80", "q": "it's"})
                .as_object()
                .cloned(),
            ..Default::default()
        };
        let text = "      - uses: a/b@x\n        with:\n          toolchain: ${{ matrix.rust }} # pinned\n          quoted: \"v${{ matrix.rust }}\"\n          single: '${{ matrix.q }}'\n          kept: ${{ secrets.T }}\n          script: |\n            echo ${{ matrix.rust }}\n          plain: 2\n";
        let filled = filled_yaml(text, &values).unwrap();
        let yaml: serde_json::Value = serde_saphyr::from_str(&filled).unwrap();
        let with = &yaml[0]["with"];
        assert_eq!(with["toolchain"], "1.80", "{filled}");
        assert!(filled.contains("toolchain: '1.80' # pinned"), "{filled}");
        assert_eq!(with["quoted"], "v1.80", "{filled}");
        assert_eq!(with["single"], "it's", "{filled}");
        assert_eq!(with["kept"], "${{ secrets.T }}", "{filled}");
        assert_eq!(with["script"], "echo 1.80\n", "{filled}");
        assert_eq!(with["plain"], 2, "{filled}");
    }

    // covers: B-75
    // The bake job builds what a skipped merge left unbuilt: it takes the build job's toolchain,
    // the actions it uses (not its checkout, not its commands) on GitHub, filled in with the
    // build job's primary matrix entry and variables, and only actions pinned to a commit (or
    // GitHub's own); its image and its before_script on GitLab.
    #[test]
    fn the_bake_job_takes_the_build_job_s_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let ci = github(&format!(
            "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    strategy:\n      matrix:\n        rust: ['1.94', beta]\n    env:\n      CACHE_KEY: v1\n    steps:\n      - uses: actions/checkout@v5\n      - name: Rust\n        uses: dtolnay/rust-toolchain@{sha} # stable\n        with:\n          toolchain: ${{{{ matrix.rust }}}}\n          components: clippy\n      - run: sudo apt-get install -y libfoo\n      - uses: actions/setup-python@v5\n      - uses: Swatinem/rust-cache@{sha}\n        with:\n          key: ${{{{ env.CACHE_KEY }}}}-${{{{ runner.os }}}}\n      - run: cargo build\n"
        ));
        let bake = plan(
            dir.path(),
            Some("github.com/acme/app"),
            &[ci],
            &[],
            &["build".into()],
            Some("https://hushrun.example/acme"),
            "hushrun",
            &[],
        );
        let (_, text, _) = &bake.files[0];
        let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        let steps = yaml["jobs"]["bake"]["steps"].as_array().unwrap();
        let uses: Vec<&str> = steps.iter().filter_map(|s| s["uses"].as_str()).collect();
        assert_eq!(
            uses,
            [
                "actions/checkout@v5",
                &format!("dtolnay/rust-toolchain@{sha}"),
                "actions/setup-python@v5",
                &format!("Swatinem/rust-cache@{sha}"),
            ],
            "{text}"
        );
        // The whole history, for B-75's "comes after".
        assert_eq!(steps[0]["with"]["fetch-depth"], 0, "{text}");
        // What CI fills in, filled in as the build job's primary entry and variables have it;
        // what means the same in the bake job stays.
        assert_eq!(steps[1]["with"]["toolchain"], "1.94", "{text}");
        assert_eq!(steps[1]["with"]["components"], "clippy", "{text}");
        assert_eq!(steps[3]["with"]["key"], "v1-${{ runner.os }}", "{text}");
        assert!(!text.contains("apt-get"), "{text}");
        let last = steps.last().unwrap()["run"].as_str().unwrap();
        assert!(
            last.contains("build bake") && !last.contains("--no-build"),
            "{last}"
        );
        // A daily run lets a held image catch up.
        assert_eq!(yaml["on"]["schedule"][0]["cron"], "17 4 * * *", "{text}");
        assert!(
            yaml["jobs"]["bake"]["if"]
                .as_str()
                .unwrap()
                .contains("github.event_name == 'schedule'"),
            "{text}"
        );

        // An action pinned to a tag, or a value only the build job's run knows: nothing is
        // copied, the job doesn't build, and the plan says how to pin it.
        for (step, said) in [
            (
                "      - uses: dtolnay/rust-toolchain@stable\n".to_string(),
                "`git ls-remote https://github.com/dtolnay/rust-toolchain stable` prints the commit",
            ),
            (
                format!(
                    "      - uses: Swatinem/rust-cache@{sha}\n        with:\n          key: ${{{{ steps.k.outputs.key }}}}\n"
                ),
                "`${{ steps.k.outputs.key }}`, which only the build job's run knows",
            ),
            (
                "      - uses: docker://alpine:3\n".to_string(),
                "isn't pinned to a digest",
            ),
        ] {
            let ci = github(&format!(
                "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n{step}      - run: cargo build\n"
            ));
            let bake = plan(
                dir.path(),
                Some("github.com/acme/app"),
                &[ci],
                &[],
                &["build".into()],
                Some("https://hushrun.example/acme"),
                "hushrun",
                &[],
            );
            let (_, text, _) = &bake.files[0];
            let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
            let steps = yaml["jobs"]["bake"]["steps"].as_array().unwrap();
            assert_eq!(steps.len(), 2, "{text}");
            assert!(
                steps[1]["run"]
                    .as_str()
                    .unwrap()
                    .contains("--trusted --no-build --oci"),
                "{text}"
            );
            assert!(
                bake.notes
                    .iter()
                    .any(|n| n.contains("--no-build") && n.contains(said)),
                "{said}: {:?}",
                bake.notes
            );
        }
        // GitLab: the build job's image and before_script.
        let gl = Workflow::parse_gitlab(
            "test:\n  image: rust:1.94\n  before_script:\n    - apt-get update\n    - apt-get install -y libfoo\n  script:\n    - cargo test\n".to_string(),
        );
        let bake = plan(
            dir.path(),
            Some("gitlab.com/acme/app"),
            &[gl],
            &[],
            &["test".into()],
            Some("https://hushrun.example/acme"),
            "hushrun",
            &[],
        );
        let (_, text, _) = &bake.files[0];
        let yaml: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        let job = &yaml["hushrun-bake"];
        assert_eq!(job["image"], "rust:1.94", "{text}");
        assert_eq!(
            job["before_script"][1], "apt-get install -y libfoo",
            "{text}"
        );
    }
}
