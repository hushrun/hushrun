//! `xtask corpus`: measures init on the survey's 100 repositories (plan ci-reach §0.3).
//!
//! It builds hushrun (or takes `--hushrun`), sets up a venv with a pinned PyYAML, fetches each
//! repository's CI files at its pinned commit (cached), runs `hushrun init --dry-run --format
//! json` on a copy of each, runs it again on its own edits (A6), reads every job with the
//! harness's own reader, and scores: one outcome per test job, the assertions A1 to A10, the
//! delta against `tools/corpus/baseline.json`. It exits 1 when an assertion fails or a job loses
//! its skip, unless `tools/corpus/known.json` lists the case. `tools/corpus/README.md` says what
//! each part measures.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The PyYAML the harness reads YAML with: pinned, so that a run's numbers don't drift.
const PYYAML: &str = "6.0.3";

struct Options {
    hushrun: Option<PathBuf>,
    only: Option<String>,
    update_baseline: bool,
    refresh: bool,
    jobs: usize,
}

pub fn run(root: &Path, args: &[String]) -> Result<bool> {
    let mut o = Options {
        hushrun: None,
        only: None,
        update_baseline: false,
        refresh: false,
        jobs: 8,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().context("missing value");
        match arg.as_str() {
            "--hushrun" => o.hushrun = Some(PathBuf::from(value()?)),
            "--only" => o.only = Some(value()?.clone()),
            "--update-baseline" => o.update_baseline = true,
            "--refresh" => o.refresh = true,
            "--jobs" => o.jobs = value()?.parse().context("--jobs")?,
            other => bail!("unexpected argument {other:?}"),
        }
    }
    if o.update_baseline && o.only.is_some() {
        bail!("--update-baseline measures all 100 repositories: leave out --only");
    }
    let tools = root.join("tools/corpus");
    let out = root.join("target/corpus");
    let repos = match std::env::var_os("HUSHRUN_CORPUS_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => out.join("repos"),
    };
    std::fs::create_dir_all(&out)?;

    let hushrun = match &o.hushrun {
        Some(path) => path
            .canonicalize()
            .with_context(|| format!("--hushrun {}", path.display()))?,
        None => {
            step("building hushrun");
            let status = Command::new("cargo")
                .current_dir(root)
                .args(["build", "--release", "--locked", "--quiet", "-p", "hushrun"])
                .status()?;
            if !status.success() {
                bail!("cargo build --release failed");
            }
            root.join("target/release/hushrun")
        }
    };
    let python = venv(&out.join("venv"))?;
    let corpus = tools.join("corpus.json");
    let only: Vec<String> = o
        .only
        .iter()
        .flat_map(|only| ["--only".to_string(), only.clone()])
        .collect();
    let jobs = o.jobs.to_string();

    step("fetching the corpus");
    let mut fetch = vec!["fetch", "--corpus"];
    let corpus_s = corpus.to_str().context("a UTF-8 path")?;
    let repos_s = repos.to_str().context("a UTF-8 path")?;
    fetch.extend([corpus_s, "--dir", repos_s, "--jobs", &jobs]);
    if o.refresh {
        fetch.push("--refresh");
    }
    python_run(&python, &tools.join("fetch.py"), &fetch, &only)?;

    let first = fresh(&out.join("init/first"))?;
    let again = fresh(&out.join("init/again"))?;
    let hushrun_s = hushrun.to_str().context("a UTF-8 path")?;
    let first_s = first.to_str().context("a UTF-8 path")?;
    let again_s = again.to_str().context("a UTF-8 path")?;
    let init_args = [
        "--hushrun",
        hushrun_s,
        "--corpus",
        corpus_s,
        "--repos",
        repos_s,
        "--jobs",
        &jobs,
    ];
    step("running init on each repository");
    python_run(
        &python,
        &tools.join("run_init.py"),
        &[&init_args[..], &["--out", first_s]].concat(),
        &only,
    )?;
    step("running init again on its own edits (A6)");
    python_run(
        &python,
        &tools.join("run_init.py"),
        &[&init_args[..], &["--out", again_s, "--edits", first_s]].concat(),
        &only,
    )?;

    step("reading each job's CI");
    let facts = out.join("facts.json");
    let facts_s = facts.to_str().context("a UTF-8 path")?;
    python_run(
        &python,
        &tools.join("analyze.py"),
        &["--corpus", corpus_s, "--repos", repos_s, "--out", facts_s],
        &only,
    )?;

    step("scoring");
    let baseline = tools.join("baseline.json");
    let known = tools.join("known.json");
    let out_s = out.to_str().context("a UTF-8 path")?;
    let mut score = vec![
        "--corpus",
        corpus_s,
        "--repos",
        repos_s,
        "--facts",
        facts_s,
        "--init",
        first_s,
        "--again",
        again_s,
        "--out",
        out_s,
        "--baseline",
        baseline.to_str().context("a UTF-8 path")?,
        "--known",
        known.to_str().context("a UTF-8 path")?,
    ];
    if o.update_baseline {
        score.push("--update-baseline");
    }
    let status = Command::new(&python)
        .arg("-I")
        .arg(tools.join("score.py"))
        .args(&score)
        .args(&only)
        .status()?;
    println!("\nscoreboard: {}", out.join("scoreboard.md").display());
    Ok(status.success())
}

fn step(what: &str) {
    println!("\n== {what}");
}

/// An empty directory at `path`, for a run's records.
fn fresh(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
    }
    std::fs::create_dir_all(path)?;
    Ok(path.to_path_buf())
}

/// The venv's python, made at `dir` with the pinned PyYAML unless it is there.
fn venv(dir: &Path) -> Result<PathBuf> {
    let python = dir.join("bin/python");
    if !python.exists() {
        step("making the venv");
        let status = Command::new("python3")
            .args(["-m", "venv"])
            .arg(dir)
            .status()
            .context("python3 -m venv")?;
        if !status.success() {
            bail!("python3 -m venv failed");
        }
    }
    let has = Command::new(&python)
        .args([
            "-I",
            "-c",
            "import yaml, sys; sys.exit(yaml.__version__ != sys.argv[1])",
        ])
        .arg(PYYAML)
        .status()?;
    if !has.success() {
        step(&format!("installing PyYAML {PYYAML}"));
        let status = Command::new(&python)
            .args(["-m", "pip", "install", "--disable-pip-version-check", "-q"])
            .arg(format!("PyYAML=={PYYAML}"))
            .status()?;
        if !status.success() {
            bail!("pip install PyYAML=={PYYAML} failed");
        }
    }
    Ok(python)
}

/// `script` with `args`, then `only`, failing when it does.
fn python_run(python: &Path, script: &Path, args: &[&str], only: &[String]) -> Result<()> {
    let status = Command::new(python)
        .arg("-I")
        .arg(script)
        .args(args)
        .args(only)
        .status()
        .with_context(|| format!("running {}", script.display()))?;
    if !status.success() {
        bail!("{} failed", script.display());
    }
    Ok(())
}
