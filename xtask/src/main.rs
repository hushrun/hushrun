//! Repository automation. Run with `cargo xtask <command>`.
#![forbid(unsafe_code)]

mod bench;
mod build_replay;
mod corpus;
mod e2e;
mod installer;
mod jars;
mod notices;
mod trace;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  trace [--milestone Mn] [--verbose]
      Check that every normative rule of a started milestone is covered by a test.
  bench-digest [--files N] [--mb M] [--changed K] [--runs R] [--dir DIR]
      Time `hushrun digest` on a synthetic repository against the plan's budgets.
  build-replay --repo PATH --work PATH [--hushrun PATH] [--mode observe|on|none] [...]
      Replay a repository's commits through hushrun's rustc wrapper, each exported to its own
      folder under --work, and report what was, or would have been, reused (build blocks).
      Exits 1 when any compile differed from its block. --help lists every option.
  corpus [--hushrun PATH] [--only OWNER/NAME,...] [--update-baseline] [--refresh] [--jobs N]
      Measure `hushrun init` on the survey's 100 repositories (tools/corpus/README.md): one
      outcome per test job, assertions A1 to A10, and the delta against the baseline. Needs the
      network on its first run, then reads its cache ($HUSHRUN_CORPUS_DIR, else target/corpus).
  e2e (--repo OWNER/NAME | --gitlab GROUP/PROJECT) [--scenario NAME] [--dry-run]
      Drive a sandbox GitHub repository (needs gh) or GitLab project (needs glab) through
      hushrun's lifecycle, with git.
  installer-pins --version VERSION --from DIR [--script PATH] [--out PATH]
      Pin a release's archive checksums (DIR holds its *.tar.xz.sha256 files) in
      recipes/gitlab/install-hushrun.sh.
  jars [--check]
      Build the JVM jars hushrun embeds from their Java sources with javac 21.0.11, against API
      jars fetched from Maven Central and checked by SHA-256 (ADR 0085). --check fails when a
      checked-in jar isn't what its sources build.
  notices [--check]
      Write THIRD-PARTY-NOTICES.md: the crates the release binaries are built with, for
      dist-workspace.toml's targets, and their license texts. --check fails when it is stale.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("trace") => trace::run(&workspace_root(), &args[1..]),
        Some("bench-digest") => bench::run(&workspace_root(), &args[1..]),
        Some("build-replay") => build_replay::run(&args[1..]),
        Some("corpus") => corpus::run(&workspace_root(), &args[1..]),
        Some("e2e") => e2e::run(&workspace_root(), &args[1..]),
        Some("installer-pins") => installer::run(&workspace_root(), &args[1..]),
        Some("jars") => jars::run(&workspace_root(), &args[1..]),
        Some("notices") => notices::run(&workspace_root(), &args[1..]),
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(PathBuf::from)
        .unwrap_or(manifest_dir)
}
