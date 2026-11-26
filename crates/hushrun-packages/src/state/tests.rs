use std::ffi::OsString;
use std::fs;
use std::path::Path;

use super::*;
use crate::store::Class;
use crate::write::{self, PartMeta, StoreMeta};

/// A store at `dir/store` with a `maven` part verified by signature and a `go` part verified by a
/// lockfile.
fn baked(dir: &Path) -> PathBuf {
    let store = dir.join("store");
    for (part, file) in [
        ("maven", "repository/g/n/1/n-1.jar"),
        ("go", "download/m/@v/v1.zip"),
    ] {
        let path = store.join(PARTS).join(part).join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"bytes").unwrap();
    }
    let meta = |part: &str, verified_by| PartMeta {
        part: part.into(),
        class: Class::Download,
        tool: "tool 1".into(),
        runtime: None,
        manifests: vec![],
        verified_by,
    };
    write::seal(
        &store,
        &StoreMeta {
            repository: Some("github.com/acme/shop".into()),
            platform: "linux-x64".into(),
            baked_ms: 1,
        },
        &[
            meta("maven", VerifiedBy::Signature),
            meta("go", VerifiedBy::Lockfile),
        ],
        &|_, _| (None, None),
    )
    .unwrap();
    store
}

fn here<'a>(signed: Option<&'a Signed>, untrusted_ok: bool, trusted: bool) -> Here<'a> {
    Here {
        repository: Some("github.com/acme/shop"),
        platform: "linux-x64",
        signed,
        untrusted_ok,
        may_write_trusted: trusted,
        default: false,
        inputs: &|_| None,
    }
}

fn names(state: &State) -> Vec<String> {
    state
        .lower()
        .map(|l| l.parts.iter().map(|p| p.entry.part.clone()).collect())
        .unwrap_or_default()
}

// covers: K-12
#[test]
fn absent_invalid_wrong_repository_and_platform() {
    assert_eq!(state(None, &here(None, false, false)), State::Absent);
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        state(Some(&dir.path().join("none")), &here(None, false, false)).word(),
        "invalid"
    );
    assert_eq!(
        state(Some(Path::new("relative")), &here(None, false, false)).word(),
        "invalid"
    );
    let store = baked(dir.path());
    let mut other = here(None, true, false);
    other.repository = Some("github.com/acme/other");
    let s = state(Some(&store), &other);
    assert_eq!(s.word(), "wrong_repository");
    assert!(
        s.lines()[0].contains(
            "it was baked for github.com/acme/shop, and this run is github.com/acme/other"
        )
    );
    let mut other = here(None, true, false);
    other.platform = "macos-arm64";
    let s = state(Some(&store), &other);
    assert_eq!(s.word(), "wrong_platform");
    assert_eq!(
        s.lines(),
        [format!(
            "packages: the baked packages {} is ignored: it was baked for linux-x64, and this is macos-arm64",
            store.display()
        )]
    );
    fs::write(store.join("extra"), b"x").unwrap();
    assert_eq!(
        state(Some(&store), &here(None, true, false)).word(),
        "invalid"
    );
}

// covers: K-7
#[test]
fn evidence_decides_which_parts_are_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = baked(dir.path());
    // Nothing verifies it: only the part a lockfile verifies is read, with a line.
    let s = state(Some(&store), &here(None, false, false));
    assert_eq!(s.word(), "current");
    assert_eq!(names(&s), ["go"]);
    assert!(s.lines()[0].contains("only the parts a lockfile verifies are read"));
    // The person's say-so, outside CI: both.
    let s = state(Some(&store), &here(None, true, false));
    assert_eq!(names(&s), ["go", "maven"]);
    assert_eq!(s.lower().unwrap().evidence, Some(Evidence::Accepted));
    // A signature over these bytes: both, in CI too.
    let (_, digest) = Packages::read_bytes(&store).unwrap();
    let signed = Signed {
        signer: "ci@acme".into(),
        packages_json: digest,
    };
    let s = state(Some(&store), &here(Some(&signed), false, true));
    assert_eq!(names(&s), ["go", "maven"]);
    assert!(s.words().contains("signed by ci@acme"));
    // A signature over other bytes is none.
    let other = Signed {
        signer: "ci@acme".into(),
        packages_json: Hex32::sha256(b"x"),
    };
    assert_eq!(
        names(&state(Some(&store), &here(Some(&other), false, true))),
        ["go"]
    );
}

// covers: K-7, K-12
#[test]
fn a_store_only_a_signature_verifies_is_untrusted_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let path = store.join("parts/maven/repository/a");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"a").unwrap();
    write::seal(
        &store,
        &StoreMeta {
            repository: None,
            platform: "linux-x64".into(),
            baked_ms: 0,
        },
        &[PartMeta {
            part: "maven".into(),
            class: Class::Download,
            tool: "t".into(),
            runtime: None,
            manifests: vec![],
            verified_by: VerifiedBy::Signature,
        }],
        &|_, _| (None, None),
    )
    .unwrap();
    let s = state(Some(&store), &here(None, false, false));
    assert_eq!(s.word(), "untrusted");
    assert!(s.lines()[0].contains("reads it unverified with HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1"));
    // In CI the person's say-so counts for nothing.
    let env = |name: &str| (name == LOWER_UNTRUSTED).then(|| OsString::from("1"));
    assert!(accepts_untrusted(&env, false));
    assert!(!accepts_untrusted(&env, true));
}

// covers: K-8
#[test]
fn a_run_that_may_write_what_others_trust_hashes_every_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = baked(dir.path());
    fs::write(store.join("parts/maven/repository/g/n/1/n-1.jar"), b"bytEs").unwrap();
    // Same size: a run that writes nothing others trust reads it.
    assert_eq!(
        names(&state(Some(&store), &here(None, true, false))),
        ["go", "maven"]
    );
    let (_, digest) = Packages::read_bytes(&store).unwrap();
    let signed = Signed {
        signer: "ci".into(),
        packages_json: digest,
    };
    let s = state(Some(&store), &here(Some(&signed), false, true));
    assert_eq!(names(&s), ["go"]);
    assert!(
        s.lines()[0].starts_with("packages: maven: ignored: "),
        "{:?}",
        s.lines()
    );
}

// covers: K-12
#[test]
fn a_part_whose_inputs_changed_is_behind_and_still_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = baked(dir.path());
    let mut h = here(None, true, false);
    let changed = |e: &PartEntry| (e.part == "maven").then(|| Hex32::sha256(b"now"));
    h.inputs = &changed;
    let s = state(Some(&store), &h);
    assert_eq!(s.word(), "behind");
    assert_eq!(names(&s), ["go", "maven"]);
    assert!(
        s.lines()
            .iter()
            .any(|l| l.starts_with("packages: maven: behind: "))
    );
    let same = |e: &PartEntry| Some(e.inputs);
    h.inputs = &same;
    assert_eq!(state(Some(&store), &h).word(), "current");
}

// covers: K-11
#[test]
fn where_a_run_finds_the_store() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| OsString::from(v))
        }
    };
    assert_eq!(
        named(&env(&[(LOWER, "/p")])),
        Some((PathBuf::from("/p"), false))
    );
    assert_eq!(named(&env(&[(LOWER, "")])), None);
    assert_eq!(named(&env(&[(LOWER, "/p"), (OFF, "off")])), None);
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(default_lower(dir.path()), None);
    let store = baked(dir.path());
    assert_eq!(default_lower(&store).is_some(), cfg!(unix));
}

// covers: K-11
#[cfg(unix)]
#[test]
fn the_default_folder_is_read_only_when_nobody_else_may_write_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let store = baked(dir.path());
    let me = rustix::process::geteuid().as_raw();
    let writable = |_: &Path| false;
    for path in [
        &store,
        &store.join("parts"),
        &store.join("parts/maven/repository/g"),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(refused_for(&store, me, &writable), None);
    // A folder above a row its group may write.
    let deep = store.join("parts/maven/repository/g");
    fs::set_permissions(&deep, fs::Permissions::from_mode(0o775)).unwrap();
    assert!(
        refused_for(&store, me, &writable)
            .unwrap()
            .contains("its group may write")
    );
    // On a read-only mount, nobody may.
    assert_eq!(refused_for(&store, me, &|_| true), None);
    fs::set_permissions(&deep, fs::Permissions::from_mode(0o755)).unwrap();
    // Another owner (as root, every file is root's, which is read).
    if me != 0 {
        let why = refused_for(&store, me + 1, &writable).unwrap();
        assert!(why.contains("belongs to another user"), "{why}");
    }
    // A link.
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&store, &link).unwrap();
    assert!(
        refused_for(&link, me, &writable)
            .unwrap()
            .contains("is a link")
    );
    // Anyone may write it.
    fs::set_permissions(&store, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(
        refused_for(&store, me, &writable)
            .unwrap()
            .contains("anyone may write")
    );
}
