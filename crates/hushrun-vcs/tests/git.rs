//! git adapter behavior beyond the vectors: line endings, partial clones, the real index,
//! linked worktrees, detection.

use std::path::Path;

use hushrun_testkit::git;
use hushrun_vcs::git::{Git, ReaderChoice};
use hushrun_vcs::{Detected, Options, Snapshot, Vcs, detect, digest_scopes};

fn digest(vcs: &dyn Vcs, at: &Snapshot) -> Result<String, hushrun_vcs::Error> {
    let scopes = hushrun_core::scope::Scopes::default_scope(Vec::new());
    let selected: Vec<_> = scopes.iter().collect();
    let listing = vcs.list(at)?;
    let run = digest_scopes(vcs, &listing, &scopes, &selected, false)?;
    Ok(run.scopes[0].digest.to_string())
}

fn open(dir: &Path, reader: ReaderChoice) -> Git {
    Git::open(
        dir,
        Options {
            git_reader: reader,
            ..Options::default()
        },
    )
    .unwrap()
}

fn write(dir: &Path, path: &str, bytes: &[u8]) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

// covers: D-3, D-9, D-10, D-13
#[test]
fn autocrlf_checkouts_digest_like_the_committed_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git::init(repo, "sha1");
    write(repo, "a.txt", b"one\ntwo\n");
    write(repo, "b/c.txt", b"three\n");
    git::commit_all(repo, "lf");
    let committed = digest(&open(repo, ReaderChoice::Auto), &Snapshot::default()).unwrap();

    // A Windows-style checkout: the worktree gets CRLF, the objects keep LF.
    git::run(repo, &["config", "core.autocrlf", "true"], None);
    std::fs::remove_file(repo.join("a.txt")).unwrap();
    std::fs::remove_file(repo.join("b/c.txt")).unwrap();
    git::run(repo, &["checkout", "--", "."], None);
    assert_eq!(
        std::fs::read(repo.join("a.txt")).unwrap(),
        b"one\r\ntwo\r\n"
    );

    for reader in [ReaderChoice::Gix, ReaderChoice::Cli] {
        let vcs = open(repo, reader);
        assert_eq!(digest(&vcs, &Snapshot::default()).unwrap(), committed);
        // `git add -A` converts CRLF back, as a commit would.
        assert_eq!(digest(&vcs, &Snapshot::Worktree).unwrap(), committed);
    }
}

// covers: D-10
#[test]
fn worktree_mode_never_touches_the_real_index() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git::init(repo, "sha1");
    write(repo, "a.txt", b"a\n");
    git::commit_all(repo, "one");
    let head = digest(&open(repo, ReaderChoice::Auto), &Snapshot::default()).unwrap();

    write(repo, "a.txt", b"changed\n");
    write(repo, "new.txt", b"untracked\n");
    write(repo, "ignored.log", b"ignored\n");
    write(repo, ".gitignore", b"*.log\n");
    let index_before = std::fs::read(repo.join(".git/index")).unwrap();
    let status_before = git::text(repo, &["status", "--porcelain"]);

    let vcs = open(repo, ReaderChoice::Auto);
    let worktree = digest(&vcs, &Snapshot::Worktree).unwrap();
    assert_ne!(worktree, head);
    assert_eq!(
        std::fs::read(repo.join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(git::text(repo, &["status", "--porcelain"]), status_before);

    // The same digest as the commit `git add -A && git commit` creates.
    git::commit_all(repo, "two");
    assert_eq!(
        digest(&open(repo, ReaderChoice::Auto), &Snapshot::default()).unwrap(),
        worktree
    );
    // .gitignore applies to worktree mode, as it does to `git add -A`.
    let listing = open(repo, ReaderChoice::Auto)
        .list(&Snapshot::Worktree)
        .unwrap();
    assert!(listing.find("ignored.log").is_none());
    assert!(listing.find("new.txt").is_some());
}

// covers: D-10
#[test]
fn an_edit_of_the_same_size_in_the_second_of_the_commit_counts() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git::init(repo, "sha1");
    // Nothing sets a file's change time, and here it is all that tells the two writes apart.
    git::run(repo, &["config", "core.trustctime", "false"], None);
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    let date = |path: &str| {
        let file = std::fs::File::options()
            .write(true)
            .open(repo.join(path))
            .unwrap();
        file.set_modified(then).unwrap();
    };
    write(repo, "a.txt", b"one\n");
    date("a.txt");
    git::commit_all(repo, "one");
    write(repo, "a.txt", b"two\n");
    date("a.txt");
    date(".git/index");

    // File and index share a second, so only the file's bytes can say it changed.
    let vcs = open(repo, ReaderChoice::Auto);
    assert_ne!(
        digest(&vcs, &Snapshot::Worktree).unwrap(),
        digest(&vcs, &Snapshot::default()).unwrap()
    );
}

// covers: D-10
#[test]
fn worktree_mode_with_a_split_index_and_an_unborn_head() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git::init(repo, "sha1");
    write(repo, "a.txt", b"a\n");
    // No commit yet: HEAD is unborn.
    let vcs = open(repo, ReaderChoice::Auto);
    let unborn = digest(&vcs, &Snapshot::Worktree).unwrap();
    git::commit_all(repo, "one");
    assert_eq!(digest(&vcs, &Snapshot::default()).unwrap(), unborn);

    git::run(repo, &["config", "core.splitIndex", "true"], None);
    git::run(repo, &["update-index", "--split-index"], None);
    write(repo, "b.txt", b"b\n");
    let split = digest(&open(repo, ReaderChoice::Auto), &Snapshot::Worktree).unwrap();
    git::commit_all(repo, "two");
    assert_eq!(
        digest(&open(repo, ReaderChoice::Auto), &Snapshot::default()).unwrap(),
        split
    );
}

// covers: D-10
#[test]
fn linked_worktrees_find_their_own_index() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("main");
    std::fs::create_dir(&repo).unwrap();
    git::init(&repo, "sha1");
    write(&repo, "a.txt", b"a\n");
    git::commit_all(&repo, "one");
    let linked = dir.path().join("linked");
    git::run(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            linked.to_str().unwrap(),
        ],
        None,
    );
    write(&linked, "side.txt", b"side\n");
    assert!(linked.join(".git").is_file());
    let vcs = open(&linked, ReaderChoice::Auto);
    let worktree = digest(&vcs, &Snapshot::Worktree).unwrap();
    git::commit_all(&linked, "side");
    assert_eq!(
        digest(&open(&linked, ReaderChoice::Auto), &Snapshot::default()).unwrap(),
        worktree
    );
    // The main worktree's index and HEAD are untouched.
    assert_eq!(git::text(&repo, &["status", "--porcelain"]), "");
}

// covers: D-9
#[test]
fn blobless_partial_clones_fail_with_missing_objects() {
    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git::init(&origin, "sha1");
    git::run(&origin, &["config", "uploadpack.allowFilter", "true"], None);
    write(&origin, "a.txt", b"a\n");
    git::commit_all(&origin, "one");
    let url = format!("file://{}", origin.display());
    let clone = dir.path().join("clone");
    git::run(
        dir.path(),
        &[
            "clone",
            "-q",
            "--no-checkout",
            "--filter=blob:none",
            &url,
            clone.to_str().unwrap(),
        ],
        None,
    );
    for reader in [ReaderChoice::Gix, ReaderChoice::Cli] {
        let err = digest(&open(&clone, reader), &Snapshot::default()).unwrap_err();
        assert_eq!(err.code(), Some("E_MISSING_OBJECTS"), "{reader:?}: {err}");
        assert!(err.to_string().contains("full or shallow clone"), "{err}");
    }
    // A shallow clone has every blob of its commit.
    let shallow = dir.path().join("shallow");
    git::run(
        dir.path(),
        &["clone", "-q", "--depth=1", &url, shallow.to_str().unwrap()],
        None,
    );
    digest(&open(&shallow, ReaderChoice::Auto), &Snapshot::default()).unwrap();
}

// covers: D-9
#[test]
fn revisions_resolve_to_commit_tree_and_ref() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git::init(repo, "sha1");
    write(repo, "a.txt", b"a\n");
    let first = git::commit_all(repo, "one");
    write(repo, "a.txt", b"b\n");
    let second = git::commit_all(repo, "two");
    for reader in [ReaderChoice::Gix, ReaderChoice::Cli] {
        let vcs = open(repo, reader);
        let head = vcs.list(&Snapshot::default()).unwrap().revision;
        assert_eq!(head.revision.as_deref(), Some(second.as_str()));
        assert_eq!(head.reference.as_deref(), Some("refs/heads/main"));
        assert_eq!(head.object_format.as_deref(), Some("sha1"));
        let older = vcs.list(&Snapshot::Rev(first.clone())).unwrap().revision;
        assert_eq!(older.revision.as_deref(), Some(first.as_str()));
        assert_eq!(older.reference, None);
        assert_ne!(
            digest(&vcs, &Snapshot::Rev(first.clone())).unwrap(),
            digest(&vcs, &Snapshot::default()).unwrap()
        );
        let err = vcs.list(&Snapshot::Rev("no-such-rev".into())).unwrap_err();
        assert!(err.is_usage(), "{err}");
        let err = vcs.list(&Snapshot::Rev("--output=x".into())).unwrap_err();
        assert!(err.is_usage(), "{err}");
    }
}

#[test]
fn reads_files_at_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git::init(repo, "sha1");
    write(repo, "hushrun.yaml", b"version: 1\n");
    let first = git::commit_all(repo, "one");
    write(repo, "hushrun.yaml", b"version: 2\n");
    git::commit_all(repo, "two");
    let vcs = open(repo, ReaderChoice::Auto);
    assert_eq!(
        vcs.read_file_at(&first, "hushrun.yaml").unwrap(),
        b"version: 1\n"
    );
    let listing = vcs.list(&Snapshot::default()).unwrap();
    let entry = listing.find("hushrun.yaml").unwrap();
    assert_eq!(vcs.read_entry(entry).unwrap(), b"version: 2\n");
}

#[test]
fn reads_remote_heads() {
    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git::init(&origin, "sha1");
    write(&origin, "a.txt", b"a\n");
    let head = git::commit_all(&origin, "one");
    let clone = dir.path().join("clone");
    git::run(
        dir.path(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
        None,
    );
    let vcs = open(&clone, ReaderChoice::Auto);
    assert_eq!(vcs.remote_head("origin", "main").unwrap(), Some(head));
    assert_eq!(vcs.remote_head("origin", "nope").unwrap(), None);
}

#[test]
fn reads_the_default_branch_a_clone_recorded() {
    use hushrun_vcs::git::remote;
    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git::init(&origin, "sha1");
    write(&origin, "hushrun.yaml", b"version: 1\n");
    git::commit_all(&origin, "one");
    let clone = dir.path().join("clone");
    let path = |p: &Path| p.to_str().unwrap().to_string();
    git::run(
        dir.path(),
        &["clone", "-q", &path(&origin), &path(&clone)],
        None,
    );
    assert_eq!(
        remote::recorded_default_branch(&clone, "origin").as_deref(),
        Some("main")
    );
    assert_eq!(remote::recorded_default_branch(&clone, "nope"), None);
    assert_eq!(remote::recorded_default_branch(&clone, "-x"), None);
}

// covers: D-2, D-11
#[test]
fn detection_finds_git_and_refuses_other_vcs() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("sub/deeper")).unwrap();
    git::init(&repo, "sha1");
    assert_eq!(
        detect(&repo.join("sub/deeper")).unwrap(),
        Detected::Git { root: repo.clone() }
    );
    let hg = dir.path().join("hg");
    std::fs::create_dir_all(hg.join(".hg")).unwrap();
    let err = detect(&hg).unwrap_err();
    assert!(err.is_usage());
    assert!(err.to_string().contains("Mercurial"), "{err}");
}

#[cfg(unix)]
// covers: D-3, D-11
#[test]
fn dir_mode_refuses_special_files() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", b"a\n");
    let fifo = dir.path().join("pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());
    let vcs = hushrun_vcs::dir::Dir::open(dir.path(), Options::default()).unwrap();
    let err = digest(&vcs, &Snapshot::default()).unwrap_err();
    assert_eq!(err.code(), Some("E_UNSUPPORTED_FILE"), "{err}");
    assert!(err.to_string().contains("FIFO"), "{err}");
}
