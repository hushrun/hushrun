//! The golden vectors through every adapter and object reader. `tools/refdigest.py` computed
//! the expected values independently (D-13).
//!
// covers: D-1, D-2, D-3, D-4, D-5, D-6, D-7, D-9, D-10, D-11, D-13

use std::path::Path;

use hushrun_testkit::git;
use hushrun_testkit::vectors::{self, Expect, Vector};
use hushrun_vcs::dir::Dir;
use hushrun_vcs::git::{Git, ReaderChoice};
use hushrun_vcs::{Options, Snapshot, Vcs, digest_scopes};

fn digests(vcs: &dyn Vcs, at: &Snapshot, vector: &Vector) -> Result<Expect, String> {
    let scopes = vector.scopes();
    let selected: Vec<_> = scopes.iter().collect();
    let result = vcs
        .list(at)
        .and_then(|listing| digest_scopes(vcs, &listing, &scopes, &selected, false));
    match result {
        Ok(run) => Ok(Expect::Digests(
            run.scopes
                .iter()
                .map(|s| (s.scope.to_string(), (s.digest.to_string(), s.files)))
                .collect(),
        )),
        Err(err) => match err.code() {
            Some(code) => Ok(Expect::Error(code.to_string())),
            None => Err(err.to_string()),
        },
    }
}

fn check_all(mode: &str, mut run: impl FnMut(&Vector, &Path) -> Option<Result<Expect, String>>) {
    let mut failures = Vec::new();
    let mut checked = 0;
    for vector in vectors::load() {
        let dir = tempfile::tempdir().unwrap();
        let Some(got) = run(&vector, dir.path()) else {
            continue;
        };
        checked += 1;
        match got {
            Ok(got) if got == vector.expect => {}
            Ok(got) => failures.push(format!(
                "{mode} {}: got {got:?}, expected {:?}",
                vector.name, vector.expect
            )),
            Err(err) => failures.push(format!("{mode} {}: {err}", vector.name)),
        }
    }
    assert!(checked > 0, "{mode}: no vector applies");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn git_options(reader: ReaderChoice) -> Options {
    Options {
        cache_dir: None,
        jobs: 2,
        git_reader: reader,
    }
}

#[test]
fn git_revision_mode_with_gix() {
    check_all("gix", |vector, dir| {
        if vector.no_git || vector.object_format != "sha1" {
            return None;
        }
        vector.materialize_git(dir);
        let vcs = Git::open(dir, git_options(ReaderChoice::Gix)).unwrap();
        assert_eq!(vcs.reader_name(), "gix");
        Some(digests(&vcs, &Snapshot::default(), vector))
    });
}

#[test]
fn git_revision_mode_with_the_git_cli() {
    check_all("cli", |vector, dir| {
        if vector.no_git {
            return None;
        }
        vector.materialize_git(dir);
        let vcs = Git::open(dir, git_options(ReaderChoice::Cli)).unwrap();
        assert_eq!(vcs.reader_name(), "cli");
        Some(digests(&vcs, &Snapshot::default(), vector))
    });
}

#[test]
fn sha256_repositories_use_the_git_cli_by_default() {
    check_all("auto", |vector, dir| {
        if vector.no_git || vector.object_format != "sha256" {
            return None;
        }
        vector.materialize_git(dir);
        let vcs = Git::open(dir, git_options(ReaderChoice::Auto)).unwrap();
        assert_eq!(vcs.object_format(), "sha256");
        assert_eq!(vcs.reader_name(), "cli");
        Some(digests(&vcs, &Snapshot::default(), vector))
    });
}

#[test]
fn warm_object_cache_gives_the_same_digests() {
    let cache = tempfile::tempdir().unwrap();
    check_all("cached", |vector, dir| {
        if vector.no_git {
            return None;
        }
        vector.materialize_git(dir);
        let options = Options {
            cache_dir: Some(cache.path().to_path_buf()),
            ..git_options(ReaderChoice::Auto)
        };
        let vcs = Git::open(dir, options).unwrap();
        let cold = digests(&vcs, &Snapshot::default(), vector);
        let warm = digests(&vcs, &Snapshot::default(), vector);
        assert_eq!(cold, warm, "{}", vector.name);
        Some(warm)
    });
}

#[test]
fn git_worktree_mode() {
    check_all("worktree", |vector, dir| {
        if vector.no_git || vector.no_dir || (cfg!(windows) && vector.posix_only) {
            return None;
        }
        // A tree path stored in NFD, checked out on a normalization-insensitive filesystem
        // (macOS), is seen twice by `git add -A`: once from the index and once, precomposed,
        // from the directory. The commit would hold both, so E_PATH_COLLISION is the right
        // answer there (D-2, D-10); the vector expects a Linux checkout.
        let all_nfc =
            vector
                .entries
                .iter()
                .all(|e| match hushrun_digest::normalize_path(&e.raw_path) {
                    Ok(Some(normalized)) => normalized.as_bytes() == e.raw_path.as_slice(),
                    _ => true,
                });
        if cfg!(target_os = "macos") && !all_nfc {
            return None;
        }
        vector.materialize_git(dir);
        // Write the files, then digest what `git add -A && git commit` would record.
        git::run(dir, &["checkout-index", "-a", "-f"], None);
        let vcs = Git::open(dir, git_options(ReaderChoice::Auto)).unwrap();
        Some(digests(&vcs, &Snapshot::Worktree, vector))
    });
}

#[test]
fn dir_mode() {
    check_all("dir", |vector, dir| {
        let has_gitlink = vector.entries.iter().any(|e| e.kind == 'g');
        if vector.no_dir || has_gitlink || (cfg!(windows) && vector.posix_only) {
            return None;
        }
        vector.materialize_dir(dir);
        let vcs = Dir::open(dir, Options::default()).unwrap();
        Some(digests(&vcs, &Snapshot::default(), vector))
    });
}

#[test]
fn warm_stat_cache_gives_the_same_digests() {
    let cache = tempfile::tempdir().unwrap();
    check_all("dir-cached", |vector, dir| {
        let has_gitlink = vector.entries.iter().any(|e| e.kind == 'g');
        if vector.no_dir || has_gitlink || (cfg!(windows) && vector.posix_only) {
            return None;
        }
        vector.materialize_dir(dir);
        let options = Options {
            cache_dir: Some(cache.path().to_path_buf()),
            ..Options::default()
        };
        let vcs = Dir::open(dir, options).unwrap();
        let cold = digests(&vcs, &Snapshot::default(), vector);
        let warm = digests(&vcs, &Snapshot::default(), vector);
        assert_eq!(cold, warm, "{}", vector.name);
        Some(warm)
    });
}
