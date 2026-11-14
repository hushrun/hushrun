//! \[C-38, ADR 0075\] CI's cache step for what test files read, which init proposes and never
//! writes. Only the store travels, `units/*.sqlite` ([`crate::units::db::is_store_file`]'s
//! main file, which every write leaves whole by checkpointing its WAL; \[C-36\] named without its
//! schema, so that the next schema needs no edit, and an older one is imported from): never the outbox of
//! suspensions or the local reuse state beside it, which an old cache restored by prefix would
//! bring back after a person resumed the check. for a check with `tests` on, at a backend that keeps no hints (the plan's §2.8), a CI
//! job's runs keep what their test files read between them only in a cache of its own. Where the
//! backend keeps hints, a fresh runner gets the same lists from it; with `--local` or no backend,
//! CI keeps no results (ADR 0067 item 3), so the step would spare nothing; and `--dry-run` asks
//! nothing, so it says nothing of it.

use super::workflows::{Host, Workflow};

/// What a GitHub Actions job needs: the cache directory as its runner sees it, and a key that
/// holds the runner's system (C-38's lists are per system).
pub const GITHUB: &str = "GitHub Actions: in each job that runs a check with `tests` on, add a step `uses: actions/cache@v4` before hushrun's, with `path: ~/.cache/hushrun/units/*.sqlite` (on macOS `~/Library/Caches/hushrun/units/*.sqlite`, on Windows `~\\AppData\\Local\\hushrun\\units\\*.sqlite`), `key: hushrun-units-${{ runner.os }}-${{ github.sha }}` and `restore-keys: hushrun-units-${{ runner.os }}-`";

/// What a GitLab job needs: GitLab caches paths below the project only, so the cache directory
/// moves there, and the key holds the runner's system.
pub const GITLAB: &str = "GitLab CI: in each job that runs a check with `tests` on, set `HUSHRUN_CACHE_DIR: .hushrun-cache` in its `variables` and add `cache: {key: hushrun-units-$CI_RUNNER_EXECUTABLE_ARCH, paths: [.hushrun-cache/units/*.sqlite]}`, with `.hushrun-cache/` in .gitignore";

/// \[C-38\] The lines init's notes say, under "Change by hand", for a repository some check of which
/// has `tests` on (`tests_on`), once init talks to its backend at `backend`: none unless the
/// backend answers 404 or 405 to the hint lookup, asked once. GitHub's line for a repository with
/// a GitHub workflow, GitLab's for one with a GitLab pipeline, both for one with neither.
pub fn notes(
    tests_on: bool,
    workflows: &[Workflow],
    backend: &str,
    repository: Option<&str>,
) -> Vec<String> {
    if !tests_on || crate::hosted::keeps_hints(backend, repository) != Some(false) {
        return Vec::new();
    }
    lines(
        workflows.iter().any(|w| w.host == Host::GitHub),
        workflows.iter().any(|w| w.host == Host::GitLab),
    )
}

/// The lines for a repository with GitHub workflows (`github`), a GitLab pipeline (`gitlab`),
/// both, or neither, which gets both.
fn lines(github: bool, gitlab: bool) -> Vec<String> {
    let why = ", so that its runs keep what their test files read between them: its backend doesn't keep that for other machines";
    let mut out = Vec::new();
    if github || !gitlab {
        out.push(format!("{GITHUB}{why}"));
    }
    if gitlab || !github {
        out.push(format!("{GITLAB}{why}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-15, C-38
    #[test]
    fn the_cache_step_is_said_for_the_ci_the_repository_has_and_both_for_none() {
        let said = |github, gitlab| -> Vec<bool> {
            let out = lines(github, gitlab);
            vec![
                out.iter().any(|l| l.starts_with(GITHUB)),
                out.iter().any(|l| l.starts_with(GITLAB)),
            ]
        };
        assert_eq!(said(true, false), [true, false]);
        assert_eq!(said(false, true), [false, true]);
        assert_eq!(said(true, true), [true, true]);
        assert_eq!(said(false, false), [true, true]);
        // Only the store travels: the outbox and the reuse state stay out of the cached paths.
        for line in [GITHUB, GITLAB] {
            assert!(
                !line.contains("units/`") && !line.contains("units/]"),
                "{line}"
            );
            assert_eq!(
                line.matches("*.sqlite").count(),
                if line == GITHUB { 3 } else { 1 }
            );
            // \[C-36\] Named without the schema: no edit when the next one comes.
            assert!(!line.contains(".v2.") && !line.contains(".v1."), "{line}");
        }
        // The glob takes a store's file and nothing else `units/` holds: not its WAL, its lock,
        // the outbox, the reuse state or a run's directory.
        let glob = hushrun_core::glob::Pattern::new("units/*.sqlite").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let store = crate::units::db::path(cache.path(), Some("example.com/acme/shop"));
        let name = store.file_name().unwrap().to_string_lossy().into_owned();
        assert!(glob.is_match(&format!("units/{name}")));
        assert!(glob.is_match(&format!("units/{}", name.replace(".v2.", ".v1."))));
        for other in [
            format!("units/{name}-wal"),
            format!("units/{name}-shm"),
            format!("units/{name}.lock"),
            format!("units/outbox/{}.json", "a".repeat(64)),
            "units/reuse/x.json".to_string(),
            "units/reuse/x.json.lock".to_string(),
            "units/runs/01J/hushrun.json".to_string(),
            "units/.import-x/new.sqlite".to_string(),
        ] {
            assert!(!glob.is_match(&other), "{other}");
        }
        // Each key holds the runner's system: C-38's lists are per system.
        assert!(GITHUB.contains("${{ runner.os }}"));
        assert!(GITLAB.contains("$CI_RUNNER_EXECUTABLE_ARCH"));
    }
}
