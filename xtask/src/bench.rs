//! `xtask bench-digest`: times `hushrun digest` against the budgets in plan §12.5.
//!
//! It builds a synthetic git repository (deterministic content), then runs the release binary:
//! revision mode with a cold cache, with a warm cache, and worktree mode after changing a few
//! files. The budgets are for a 4-core CI runner; the report prints the machine's core count.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

struct Params {
    files: usize,
    megabytes: usize,
    changed: usize,
    runs: usize,
    dir: Option<PathBuf>,
}

pub fn run(root: &Path, args: &[String]) -> Result<bool> {
    let mut p = Params {
        files: 5_000,
        megabytes: 100,
        changed: 10,
        runs: 5,
        dir: None,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().context("missing value");
        match arg.as_str() {
            "--files" => p.files = value()?.parse()?,
            "--mb" => p.megabytes = value()?.parse()?,
            "--changed" => p.changed = value()?.parse()?,
            "--runs" => p.runs = value()?.parse()?,
            "--dir" => p.dir = Some(PathBuf::from(value()?)),
            other => bail!("unexpected argument {other:?}"),
        }
    }

    let status = Command::new("cargo")
        .current_dir(root)
        .args(["build", "--release", "--quiet", "--package", "hushrun"])
        .status()?;
    if !status.success() {
        bail!("cargo build --release failed");
    }
    let binary = root.join("target/release/hushrun");

    let base = match &p.dir {
        Some(dir) => dir.clone(),
        None => std::env::temp_dir().join(format!("hushrun-bench-{}-{}", p.files, p.megabytes)),
    };
    let repo = base.join("repo");
    if !repo.join(".git").exists() {
        println!(
            "creating {} files, {} MB in {}",
            p.files,
            p.megabytes,
            repo.display()
        );
        create_repo(&repo, p.files, p.megabytes * 1024 * 1024)?;
    }
    let cache = base.join("cache");

    let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
    println!("{} files, {} MB, {cores} cores", p.files, p.megabytes);

    let cold = time_runs(p.runs, || {
        let _ = fs::remove_dir_all(&cache);
        digest(&binary, &repo, &cache, &[])
    })?;
    report("digest --rev HEAD, cold cache", &cold);
    digest(&binary, &repo, &cache, &[])?;
    let warm = time_runs(p.runs, || digest(&binary, &repo, &cache, &[]))?;
    report("digest --rev HEAD, warm cache", &warm);

    for i in 0..p.changed {
        let path = repo.join(file_path(i * (p.files / p.changed.max(1))));
        let mut bytes = fs::read(&path)?;
        bytes.extend_from_slice(b"changed\n");
        fs::write(&path, bytes)?;
    }
    digest(&binary, &repo, &cache, &["--worktree"])?;
    let worktree = time_runs(p.runs, || digest(&binary, &repo, &cache, &["--worktree"]))?;
    report(
        &format!("digest --worktree, {} changed, warm", p.changed),
        &worktree,
    );
    let version = time_runs(p.runs, || {
        let started = Instant::now();
        let status = Command::new(&binary).arg("--version").output()?;
        if !status.status.success() {
            bail!("--version failed");
        }
        Ok(started.elapsed())
    })?;
    report("--version", &version);
    git(&repo, &["checkout", "--", "."])?;
    Ok(true)
}

fn report(name: &str, times: &[Duration]) {
    let mut sorted = times.to_vec();
    sorted.sort();
    let median = sorted[sorted.len() / 2];
    println!(
        "  {name:<44} median {:>7.1} ms   min {:>7.1} ms",
        median.as_secs_f64() * 1e3,
        sorted[0].as_secs_f64() * 1e3
    );
}

fn time_runs(runs: usize, mut f: impl FnMut() -> Result<Duration>) -> Result<Vec<Duration>> {
    (0..runs.max(1)).map(|_| f()).collect()
}

fn digest(binary: &Path, repo: &Path, cache: &Path, extra: &[&str]) -> Result<Duration> {
    let started = Instant::now();
    let output = Command::new(binary)
        .current_dir(repo)
        .env("HUSHRUN_CACHE_DIR", cache)
        .arg("digest")
        .args(extra)
        .output()?;
    let elapsed = started.elapsed();
    if !output.status.success() {
        bail!(
            "hushrun digest failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(elapsed)
}

fn file_path(i: usize) -> String {
    // About 20 files per directory, as in real repositories.
    format!("d{:03}/s{:02}/f{i:06}.dat", (i / 20) % 250, (i / 5000) % 20)
}

fn create_repo(repo: &Path, files: usize, total_bytes: usize) -> Result<()> {
    fs::create_dir_all(repo)?;
    git(repo, &["init", "-q", "-b", "main"])?;
    let per_file = total_bytes / files.max(1);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for i in 0..files {
        let path = repo.join(file_path(i));
        fs::create_dir_all(path.parent().expect("parent"))?;
        // Sizes vary between half and one and a half times the average.
        let size = per_file / 2 + (i * 7919) % per_file.max(1);
        let mut bytes = Vec::with_capacity(size);
        while bytes.len() < size {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            bytes.extend_from_slice(&state.to_le_bytes());
        }
        bytes.truncate(size);
        fs::write(path, bytes)?;
    }
    git(repo, &["add", "-A"])?;
    git(
        repo,
        &[
            "-c",
            "user.name=bench",
            "-c",
            "user.email=bench@hushrun.invalid",
            "commit",
            "-q",
            "-m",
            "bench",
        ],
    )?;
    git(repo, &["gc", "-q"])?;
    Ok(())
}

fn git(repo: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()?;
    if !status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(())
}
