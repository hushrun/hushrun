//! `hushrun adopt` (M14): writes a starter `hushrun.yaml`, from what the repository looks like
//! (its language, its Dockerfiles, its deploy workflows); one short workflow, `deploy.yml`, that
//! calls the reusable workflow, whose jobs sign keylessly as the repository's CI (T-15); and the next
//! steps. It is idempotent: a file that differs from what it would write
//! is kept, unless `--force`. With `--dry-run` it prints instead of writing.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow};
use serde_json::{Value, json};

use hushrun_core::names::is_valid_name;
use hushrun_core::policy::Policy;

use crate::app::App;
use crate::cli::AdoptArgs;
use crate::cmd::init::origin_repository;
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};

/// The reusable workflow, when `--workflow` names no commit.
const DEFAULT_WORKFLOW: &str = "hushrun/hushrun/.github/workflows/hushrun-deploy.yml@main";
/// The first line of every file adopt writes.
const MARKER: &str = "Written by `hushrun adopt`";

/// What the repository looks like.
#[derive(Debug, Clone, Default)]
struct Detected {
    language: Option<&'static str>,
    test: String,
    /// The reusable workflow's `toolchain` input.
    toolchain: &'static str,
    /// Directories with a Dockerfile, relative to the root; `.` for the root.
    dockerfiles: Vec<String>,
    workflows: Vec<DeployWorkflow>,
}

/// A workflow that already deploys.
#[derive(Debug, Clone)]
struct DeployWorkflow {
    file: String,
    branches: Vec<String>,
    /// Deploy commands and actions it runs.
    deploys: Vec<String>,
}

/// A deployable: a scope and its image.
#[derive(Debug, Clone)]
struct Deployable {
    scope: Option<String>,
    /// Its directory, for the build command.
    dir: String,
    artifact: String,
}

/// One file adopt writes.
struct File {
    path: String,
    content: String,
}

pub fn run(app: &App<'_>, args: &AdoptArgs) -> CmdResult {
    let start = match &app.global.repo {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().context("finding the current directory")?,
    };
    let root = match hushrun_vcs::detect(&start).map_err(Failure::usage)? {
        hushrun_vcs::Detected::Git { root } => root,
        hushrun_vcs::Detected::None => {
            return Err(Failure::usage(anyhow!(
                "hushrun adopt sets up a git repository; run it inside one"
            )));
        }
    };
    let repository = match &args.repository {
        Some(id) => id.clone(),
        None => app
            .run
            .repository
            .clone()
            .or_else(|| origin_repository(&root))
            .ok_or_else(|| {
                Failure::usage(anyhow!(
                    "can't tell the repository from CI or the origin remote: pass --repository host/owner/name"
                ))
            })?,
    };

    let detected = detect(&root);
    let branch = branch(args, &detected);
    let deployables = deployables(&detected);

    let ledger_base = args
        .ledger_base
        .clone()
        .or_else(|| app.env.get("HUSHRUN_REPOSITORY_BASE").map(str::to_string));
    let ledger = match &ledger_base {
        Some(base) => Some(
            hushrun_core::ledger_url::LedgerUrl::parse(base)
                .map_err(Failure::usage)?
                .under(&repository)
                .map_err(Failure::usage)?
                .to_string(),
        ),
        None => None,
    };
    let workflow = args
        .workflow
        .clone()
        .unwrap_or_else(|| DEFAULT_WORKFLOW.to_string());
    // [ADR 0035] Unpinned, the workflows install the newest release.
    let version = args.hushrun_version.clone();
    if let Some(version) = version.as_deref().filter(|v| !is_version(v)) {
        return Err(Failure::usage(anyhow!(
            "--hushrun-version {version:?} isn't a release: it pins one, such as 0.2.0, and without it the workflows install the newest"
        )));
    }

    let images = images(args, &deployables)?;
    let policy_text = policy_text(
        &repository,
        ledger.as_deref(),
        &detected,
        &branch,
        &deployables,
        &images,
    );
    Policy::parse("hushrun.yaml", policy_text.as_bytes())
        .map_err(|e| Failure::usage(anyhow!("the starter hushrun.yaml doesn't validate: {e}")))?;
    let mut files = vec![File {
        path: "hushrun.yaml".into(),
        content: policy_text,
    }];
    files.push(File {
        path: format!(".github/workflows/{WORKFLOW_FILE}"),
        content: workflow_text(
            &branch,
            &deployables,
            &detected,
            &workflow,
            version.as_deref(),
        ),
    });

    // Write what is missing; keep what differs, unless --force.
    let mut statuses = Vec::new();
    for file in &files {
        let path = root.join(&file.path);
        let existing = std::fs::read_to_string(&path).ok();
        let status = match (&existing, args.force, args.dry_run) {
            (Some(text), _, _) if *text == file.content => "unchanged",
            (Some(_), false, _) => "kept",
            (Some(_), true, true) => "would-replace",
            (Some(_), true, false) => "replaced",
            (None, _, true) => "would-write",
            (None, _, false) => "written",
        };
        if matches!(status, "written" | "replaced") {
            write(&path, &file.content)?;
        }
        statuses.push(status);
    }

    let steps = next_steps(&files, &statuses, &detected, ledger.is_some());
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.adopt/v2",
            "repository": repository,
            "dryRun": args.dry_run,
            "detected": {
                "language": detected.language,
                "test": detected.test,
                "toolchain": detected.toolchain,
                "dockerfiles": detected.dockerfiles,
                "workflows": detected.workflows.iter().map(|w| json!({
                    "file": w.file, "branches": w.branches, "deploys": w.deploys,
                })).collect::<Vec<_>>(),
            },
            "branch": branch,
            "ledger": ledger,
            "workflow": workflow,
            "version": version,
            "files": files.iter().zip(&statuses).map(|(f, status)| {
                let mut file = json!({"path": f.path, "status": status});
                if args.dry_run {
                    file["content"] = json!(f.content);
                }
                file
            }).collect::<Vec<_>>(),
            "nextSteps": steps,
        }))?,
        _ => {
            let mut text = String::new();
            for (file, status) in files.iter().zip(&statuses) {
                if args.dry_run && matches!(*status, "would-write" | "would-replace") {
                    let _ = writeln!(text, "==> {} ({status})", file.path);
                    text.push_str(&file.content);
                    text.push('\n');
                } else {
                    let _ = writeln!(text, "{status:<13} {}", file.path);
                }
            }
            let _ = writeln!(text, "\n{}", describe(&detected));
            let _ = writeln!(text, "\nNext:");
            for (i, step) in steps.iter().enumerate() {
                let _ = writeln!(text, "  {}. {}", i + 1, step.replace('\n', "\n     "));
            }
            output::print(&text)?;
        }
    }
    Ok(Exit::Ok)
}

fn write(path: &Path, content: &str) -> Result<(), Failure> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// --- Detection -------------------------------------------------------------------------------

fn detect(root: &Path) -> Detected {
    let has = |file: &str| root.join(file).exists();
    let (language, test, toolchain): (Option<&'static str>, String, &'static str) = if has("go.mod")
    {
        (Some("go"), "go test ./...".into(), "go")
    } else if has("Cargo.toml") {
        (Some("rust"), "cargo test --locked".into(), "none")
    } else if has("package.json") {
        let runner = if has("pnpm-lock.yaml") {
            "pnpm test"
        } else if has("yarn.lock") {
            "yarn test"
        } else {
            "npm test"
        };
        (Some("node"), runner.into(), "node")
    } else if has("pyproject.toml") || has("requirements.txt") || has("setup.py") {
        (Some("python"), "python -m pytest".into(), "python")
    } else if has("pom.xml") {
        (Some("java"), "mvn -B verify".into(), "java")
    } else if has("build.gradle") || has("build.gradle.kts") {
        (Some("java"), "./gradlew test".into(), "java")
    } else {
        (
            None,
            "echo 'replace with your test command' && false".into(),
            "none",
        )
    };
    let mut dockerfiles = Vec::new();
    find_dockerfiles(root, root, 0, &mut dockerfiles);
    dockerfiles.sort();
    Detected {
        language,
        test,
        toolchain,
        dockerfiles,
        workflows: deploy_workflows(root),
    }
}

/// Directories with a `Dockerfile`, three levels deep at most, skipping dependencies.
fn find_dockerfiles(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if dir.join("Dockerfile").is_file() {
        let relative = dir
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        out.push(if relative.is_empty() {
            ".".into()
        } else {
            relative
        });
    }
    if depth >= 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            !name.starts_with('.')
                && !["node_modules", "vendor", "target", "dist", "build"].contains(&name.as_ref())
        })
        .collect();
    dirs.sort();
    for sub in dirs {
        find_dockerfiles(root, &sub, depth + 1, out);
    }
}

/// Commands and actions that deploy, as existing workflows run them.
const DEPLOY_COMMANDS: [&str; 16] = [
    "gcloud run deploy",
    "gcloud app deploy",
    "gcloud functions deploy",
    "kubectl apply",
    "kubectl set image",
    "helm upgrade",
    "aws ecs update-service",
    "aws lambda update-function-code",
    "az webapp",
    "az containerapp update",
    "fly deploy",
    "flyctl deploy",
    "firebase deploy",
    "serverless deploy",
    "vercel",
    "terraform apply",
];
const DEPLOY_ACTIONS: [&str; 5] = [
    "google-github-actions/deploy-cloudrun",
    "aws-actions/amazon-ecs-deploy-task-definition",
    "azure/webapps-deploy",
    "azure/container-apps-deploy-action",
    "superfly/flyctl-actions",
];

/// The workflows in `.github/workflows/` that deploy, except those adopt wrote.
fn deploy_workflows(root: &Path) -> Vec<DeployWorkflow> {
    let dir = root.join(".github/workflows");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "yml" || e == "yaml"))
        .collect();
    paths.sort();
    let mut found = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.contains(MARKER) {
            continue;
        }
        let Ok(value) = hushrun_core::yaml::parse::<Value>("workflow", text.as_bytes()) else {
            continue;
        };
        let mut deploys = Vec::new();
        for job in value["jobs"]
            .as_object()
            .into_iter()
            .flat_map(|j| j.values())
        {
            for step in job["steps"].as_array().into_iter().flatten() {
                if let Some(run) = step["run"].as_str() {
                    for line in run.lines().map(str::trim) {
                        if DEPLOY_COMMANDS.iter().any(|c| line.starts_with(c)) {
                            deploys.push(line.to_string());
                        }
                    }
                }
                if let Some(uses) = step["uses"].as_str()
                    && DEPLOY_ACTIONS.iter().any(|a| uses.starts_with(a))
                {
                    deploys.push(uses.to_string());
                }
            }
        }
        if deploys.is_empty() {
            continue;
        }
        let on = &value["on"];
        let branches = on["push"]["branches"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b.as_str().map(str::to_string))
            .collect();
        found.push(DeployWorkflow {
            file: format!(
                ".github/workflows/{}",
                path.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            ),
            branches,
            deploys,
        });
    }
    found
}

fn describe(detected: &Detected) -> String {
    let mut parts = vec![match detected.language {
        Some(language) => format!("{language} ({})", detected.test),
        None => "no language detected (set the test command in hushrun.yaml)".to_string(),
    }];
    parts.push(match detected.dockerfiles.as_slice() {
        [] => "no Dockerfile (set the image in hushrun.yaml, or remove `artifacts`)".to_string(),
        [one] if one == "." => "a Dockerfile at the root".to_string(),
        many => format!("Dockerfiles in {}", many.join(", ")),
    });
    for workflow in &detected.workflows {
        parts.push(format!(
            "{} deploys{} with {}",
            workflow.file,
            if workflow.branches.is_empty() {
                String::new()
            } else {
                format!(" on pushes to {}", workflow.branches.join(", "))
            },
            workflow.deploys.join("; ")
        ));
    }
    format!("Detected: {}", parts.join("; "))
}

// --- What to write ---------------------------------------------------------------------------

/// The branch it deploys from: `--branch`, else the main branch the deploy workflows push to,
/// else main.
fn branch(args: &AdoptArgs, detected: &Detected) -> String {
    if let Some(branch) = &args.branch {
        return branch.clone();
    }
    let pushed: Vec<&str> = detected
        .workflows
        .iter()
        .flat_map(|w| w.branches.iter().map(String::as_str))
        .collect();
    ["main", "master"]
        .into_iter()
        .find(|b| pushed.contains(b))
        .unwrap_or("main")
        .to_string()
}

/// One deployable per Dockerfile directory; one `image` for the root, or for none.
fn deployables(detected: &Detected) -> Vec<Deployable> {
    match detected.dockerfiles.as_slice() {
        [] => vec![Deployable {
            scope: None,
            dir: ".".into(),
            artifact: "image".into(),
        }],
        dirs if dirs.iter().any(|d| d == ".") => vec![Deployable {
            scope: None,
            dir: ".".into(),
            artifact: "image".into(),
        }],
        dirs => {
            let mut out: Vec<Deployable> = Vec::new();
            for dir in dirs {
                let base: String = dir
                    .rsplit('/')
                    .next()
                    .unwrap_or(dir)
                    .to_ascii_lowercase()
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect();
                let mut name = if base.starts_with(|c: char| c.is_ascii_lowercase()) {
                    base
                } else {
                    format!("s-{base}")
                };
                name.truncate(56);
                let mut unique = name.clone();
                let mut n = 2;
                while out
                    .iter()
                    .any(|d| d.scope.as_deref() == Some(unique.as_str()))
                {
                    unique = format!("{name}-{n}");
                    n += 1;
                }
                if !is_valid_name(&unique) {
                    continue;
                }
                out.push(Deployable {
                    artifact: format!("{unique}-image"),
                    scope: Some(unique),
                    dir: dir.clone(),
                });
            }
            out
        }
    }
}

/// `--image [ARTIFACT=]REPOSITORY`, by artifact: a bare repository is every deployable's, under
/// its scope's name when there are several.
fn images(
    args: &AdoptArgs,
    deployables: &[Deployable],
) -> Result<BTreeMap<String, String>, Failure> {
    let mut out = BTreeMap::new();
    for text in &args.images {
        let named = text
            .split_once('=')
            .filter(|(name, _)| deployables.iter().any(|d| d.artifact == *name));
        match named {
            Some((name, repository)) => {
                out.insert(
                    name.to_string(),
                    repository.trim_end_matches('/').to_string(),
                );
            }
            None if text.contains('=') => {
                let names: Vec<&str> = deployables.iter().map(|d| d.artifact.as_str()).collect();
                return Err(Failure::usage(anyhow!(
                    "--image {text:?} names no artifact adopt writes; they are {}",
                    names.join(", ")
                )));
            }
            None => {
                let repository = text.trim_end_matches('/');
                for d in deployables {
                    let location = match (&d.scope, deployables.len()) {
                        (Some(scope), n) if n > 1 => format!("{repository}/{scope}"),
                        _ => repository.to_string(),
                    };
                    out.insert(d.artifact.clone(), location);
                }
            }
        }
    }
    Ok(out)
}

/// A string as a YAML scalar: plain when it reads back as the same string, else single-quoted.
fn scalar(text: &str) -> String {
    let reserved = ["true", "false", "null", "yes", "no", "on", "off", "~"];
    let plain = !text.is_empty()
        && text.trim() == text
        && !text.starts_with([
            '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%',
            '@', '`',
        ])
        && !text.contains(": ")
        && !text.contains(" #")
        && !text.ends_with(':')
        && !text.contains(['\n', '\r', '\t'])
        && !reserved.contains(&text.to_ascii_lowercase().as_str())
        && text.parse::<f64>().is_err();
    if plain {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "''"))
    }
}

/// The repository's name, the last segment of its id.
fn short_name(repository: &str) -> &str {
    repository.rsplit('/').next().unwrap_or(repository)
}

fn policy_text(
    repository: &str,
    ledger: Option<&str>,
    detected: &Detected,
    branch: &str,
    deployables: &[Deployable],
    images: &BTreeMap<String, String>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# hushrun.yaml: what a deploy requires (https://hushrun.com/docs/deploy/github). {MARKER}; review it."
    );
    let _ = writeln!(out, "version: 1");
    let _ = writeln!(out, "repository: {repository}");
    match ledger {
        Some(url) => {
            let _ = writeln!(
                out,
                "repository_url: {url}   # CI derives it from HUSHRUN_REPOSITORY_BASE, and checks it is this"
            );
        }
        None => {
            let _ = writeln!(
                out,
                "# repository_url: where this repository's events go. CI reads it from the HUSHRUN_REPOSITORY_BASE or HUSHRUN_REPOSITORY_URL variable."
            );
            let _ = writeln!(out, "# repository_url: https://HOST/l/ORG/REPOSITORY");
        }
    }

    // Scopes, for a monorepo.
    if deployables.iter().any(|d| d.scope.is_some()) {
        let _ = writeln!(out, "\nscopes:");
        for d in deployables {
            let _ = writeln!(out, "  {}:", d.scope.as_deref().unwrap_or("default"));
            let _ = writeln!(
                out,
                "    include: [{}, \".github/workflows/{WORKFLOW_FILE}\", \"hushrun.yaml\"]",
                scalar_double(&format!("{}/**", d.dir))
            );
        }
    }

    // Checks: the detected test.
    let _ = writeln!(out, "\nchecks:");
    let _ = writeln!(out, "  unit:");
    let _ = writeln!(out, "    run: {}", scalar(&detected.test));

    // Artifacts: one image per deployable, at its location.
    let _ = writeln!(out, "\nartifacts:");
    for d in deployables {
        let _ = writeln!(out, "  {}:", d.artifact);
        let _ = writeln!(out, "    type: oci");
        if let Some(scope) = &d.scope {
            let _ = writeln!(out, "    scope: {scope}");
        }
        let location = images
            .get(&d.artifact)
            .cloned()
            .unwrap_or_else(|| match &d.scope {
                Some(scope) => format!("registry.example.com/{}/{scope}", short_name(repository)),
                None => format!("registry.example.com/{}", short_name(repository)),
            });
        let _ = writeln!(out, "    location: {}", scalar(&location));
    }

    // The deploy.
    let _ = writeln!(out, "\ndeploy:");
    let _ = writeln!(out, "  branch: {}", scalar(branch));
    let _ = writeln!(out, "  require: [unit]");
    out
}

/// An exact release: `0.2.0`, or `1.0.0-rc.1`.
fn is_version(text: &str) -> bool {
    let core = text.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn scalar_double(text: &str) -> String {
    serde_json::to_string(text).expect("strings serialize")
}

/// The workflow adopt writes.
const WORKFLOW_FILE: &str = "hushrun.yml";

fn workflow_text(
    branch: &str,
    deployables: &[Deployable],
    detected: &Detected,
    workflow: &str,
    version: Option<&str>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Deploys from {branch} through the reusable workflow. {MARKER}: review it;"
    );
    let _ = writeln!(out, "# running adopt again leaves a changed file alone.");
    let _ = writeln!(out, "name: Deploy");
    let _ = writeln!(out, "on:");
    let _ = writeln!(out, "  push:");
    let _ = writeln!(out, "    branches: [{}]", scalar(branch));
    let _ = writeln!(out, "  workflow_dispatch:");
    let _ = writeln!(out, "    inputs:");
    let _ = writeln!(out, "      force_full:");
    let _ = writeln!(
        out,
        "        description: Build and run every check, whatever the evidence"
    );
    let _ = writeln!(out, "        type: boolean");
    let _ = writeln!(out, "        default: false");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "# One deploy at a time; runs started off {branch} get a group of their own."
    );
    let _ = writeln!(out, "concurrency:");
    let _ = writeln!(
        out,
        "  group: ${{{{ github.ref == 'refs/heads/{branch}' && 'deploy' || format('deploy-refused-{{0}}', github.run_id) }}}}"
    );
    let _ = writeln!(out, "  cancel-in-progress: false");
    let _ = writeln!(out);
    let _ = writeln!(out, "permissions:");
    let _ = writeln!(out, "  contents: read");
    let _ = writeln!(
        out,
        "  id-token: write          # workload identity federation; ID tokens for hushrun's server"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "jobs:");
    let current = detected
        .workflows
        .iter()
        .find(|w| w.branches.iter().any(|b| b == branch))
        .or(detected.workflows.first());
    for d in deployables {
        let job = d.scope.as_deref().unwrap_or("deploy");
        let _ = writeln!(out, "  {job}:");
        let pinned = if workflow.ends_with("@main") {
            "   # pin to a release commit"
        } else {
            ""
        };
        let _ = writeln!(out, "    uses: {workflow}{pinned}");
        let _ = writeln!(out, "    with:");
        match version {
            Some(version) => {
                let _ = writeln!(out, "      version: {version}");
            }
            None => {
                let _ = writeln!(
                    out,
                    "      # version: {}          # pins hushrun; without it, each run installs the newest release",
                    env!("CARGO_PKG_VERSION")
                );
            }
        }
        if let Some(scope) = &d.scope {
            let _ = writeln!(out, "      scope: {scope}");
            let _ = writeln!(out, "      image: {}", d.artifact);
        }
        let _ = writeln!(
            out,
            "      repository-base: ${{{{ vars.HUSHRUN_REPOSITORY_BASE }}}}"
        );
        let _ = writeln!(
            out,
            "      repository-url: ${{{{ vars.HUSHRUN_REPOSITORY_URL }}}}"
        );
        let _ = writeln!(
            out,
            "      force-full: ${{{{ inputs.force_full || false }}}}"
        );
        if detected.toolchain != "none" {
            let _ = writeln!(out, "      toolchain: {}", detected.toolchain);
        }
        let build = if detected.dockerfiles.is_empty() {
            "echo 'set build in this workflow to push the image to $REF' >&2; exit 1".to_string()
        } else {
            let dir = if d
                .dir
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
            {
                d.dir.clone()
            } else {
                scalar_double(&d.dir)
            };
            format!("docker build -t \"$REF\" {dir} && docker push \"$REF\"")
        };
        let _ = writeln!(out, "      build: {}", scalar(&build));
        if let Some(current) = current {
            for command in current.deploys.iter().take(3) {
                let _ = writeln!(
                    out,
                    "      # {} deploys with: {}",
                    current.file,
                    command.replace('\n', " ")
                );
            }
        }
        let deploy = format!(
            "echo 'set deploy in .github/workflows/{WORKFLOW_FILE} to deploy $REF' >&2; exit 1"
        );
        let _ = writeln!(out, "      deploy: {}", scalar(&deploy));
        let _ = writeln!(
            out,
            "      # Cloud credentials for the registry and the deploy, through workload identity federation (no secrets):"
        );
        let _ = writeln!(
            out,
            "      # gcp-workload-identity-provider: projects/NUMBER/locations/global/workloadIdentityPools/POOL/providers/PROVIDER"
        );
        let _ = writeln!(
            out,
            "      # gcp-service-account: SERVICE_ACCOUNT@PROJECT.iam.gserviceaccount.com"
        );
        let _ = writeln!(
            out,
            "      # or aws-role-to-assume and aws-region, or azure-client-id, azure-tenant-id and azure-subscription-id"
        );
        let _ = writeln!(
            out,
            "      signing: sigstore      # keylessly, as this repository's CI: no key to keep"
        );
    }
    out
}

fn next_steps(
    files: &[File],
    statuses: &[&str],
    detected: &Detected,
    has_ledger: bool,
) -> Vec<String> {
    let mut steps = vec![
        "Review hushrun.yaml: the test command, the image repositories, the branch.".to_string(),
    ];
    let workflows: Vec<&str> = files
        .iter()
        .filter(|f| f.path.starts_with(".github/"))
        .map(|f| f.path.as_str())
        .collect();
    steps.push(format!(
        "In {}, set `build` and `deploy` (they get the image as $REF) and the cloud credentials.",
        workflows.join(" and ")
    ));
    for (file, status) in files.iter().zip(statuses) {
        if *status == "kept" {
            steps.push(format!(
                "{} exists and differs from what adopt writes: it was kept. Compare with `hushrun adopt --dry-run`, or pass --force.",
                file.path
            ));
        }
    }
    if !has_ledger {
        steps.push(
            "Set the HUSHRUN_REPOSITORY_BASE organization variable, or this repository's HUSHRUN_REPOSITORY_URL, and name the same URL in hushrun.yaml's `repository_url:`."
                .to_string(),
        );
    }
    for workflow in &detected.workflows {
        steps.push(format!(
            "Remove {} once the new workflow deploys, so nothing deploys around hushrun.",
            workflow.file
        ));
    }
    steps.push(
        "Commit on a branch, open a pull request, and run `hushrun doctor` in it.".to_string(),
    );
    steps
}
