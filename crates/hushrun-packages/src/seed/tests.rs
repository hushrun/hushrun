use std::fs;
use std::path::Path;

use hushrun_core::blocks::Hex32;

use crate::part::{Seed, SeedFile};
use crate::seed::seed;
use crate::store::{Kind, Row, VerifiedBy};

const STORE: Hex32 = Hex32([7; 32]);

fn row(path: &str, bytes: &[u8]) -> Row {
    Row {
        path: path.into(),
        kind: Kind::File,
        bytes: bytes.len() as u64,
        sha256: Hex32::sha256(bytes),
        target: None,
        package: None,
        locked: None,
    }
}

fn part(dir: &Path, files: &[(&str, &[u8])]) -> Vec<Row> {
    files
        .iter()
        .map(|(path, bytes)| {
            let to = dir.join(path);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::write(&to, bytes).unwrap();
            row(path, bytes)
        })
        .collect()
}

fn wanted(name: &str, sha256: Option<Hex32>) -> SeedFile {
    SeedFile {
        path: format!("cache/{name}"),
        name: name.into(),
        sha256,
    }
}

// covers: K-45
#[test]
fn a_seed_copies_what_the_lockfile_hashes_and_nothing_else() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("part");
    let rows = part(
        &dir,
        &[
            ("cache/a-1.gem", b"a"),
            ("cache/b-1.gem", b"b"),
            ("cache/c-1.gem", b"c"),
        ],
    );
    let into = t.path().join("gems/cache");
    let plan = Seed {
        into: into.clone(),
        files: vec![
            wanted("a-1.gem", Some(Hex32::sha256(b"a"))),
            // The lockfile hashes other bytes: never seeded, so the tool downloads its own.
            wanted("b-1.gem", Some(Hex32::sha256(b"other"))),
            // No hash in the lockfile, and only the lockfile verifies the part.
            wanted("c-1.gem", None),
            // The store lacks it: the tool downloads it.
            wanted("d-1.gem", Some(Hex32::sha256(b"d"))),
        ],
    };
    let done = seed(&dir, &rows, VerifiedBy::Lockfile, &plan, &STORE).unwrap();
    assert_eq!((done.copied, done.present, done.lacking), (1, 0, 1));
    assert_eq!(done.refused.len(), 2, "{:?}", done.refused);
    assert_eq!(fs::read(into.join("a-1.gem")).unwrap(), b"a");
    assert!(!into.join("b-1.gem").exists() && !into.join("c-1.gem").exists());
    let names: Vec<_> = fs::read_dir(&into).unwrap().flatten().collect();
    assert_eq!(names.len(), 1, "no file of the seed's own is left");
    assert_eq!(
        done.lines("bundler"),
        [
            "packages: bundler: 2 files not seeded: b-1.gem: the lockfile here hashes other bytes than the store holds, and 1 more"
        ]
    );
    // A part a signature verifies seeds a file the lockfile has no hash of, by its row's hash.
    let done = seed(&dir, &rows, VerifiedBy::Signature, &plan, &STORE).unwrap();
    assert_eq!((done.copied, done.present), (1, 1));
    assert_eq!(fs::read(into.join("c-1.gem")).unwrap(), b"c");
}

// covers: K-45
#[test]
fn a_seed_never_writes_over_a_file_and_never_copies_a_changed_one() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("part");
    let rows = part(&dir, &[("cache/a-1.gem", b"a"), ("cache/b-1.gem", b"b")]);
    let into = t.path().join("cache");
    fs::create_dir_all(&into).unwrap();
    fs::write(into.join("a-1.gem"), b"the tool's own").unwrap();
    // Changed in place, same size: the row's hash finds it as it is copied.
    fs::write(dir.join("cache/b-1.gem"), b"X").unwrap();
    let plan = Seed {
        into: into.clone(),
        files: vec![wanted("a-1.gem", None), wanted("b-1.gem", None)],
    };
    let done = seed(&dir, &rows, VerifiedBy::Signature, &plan, &STORE).unwrap();
    assert_eq!((done.copied, done.present), (0, 1));
    assert_eq!(fs::read(into.join("a-1.gem")).unwrap(), b"the tool's own");
    assert!(!into.join("b-1.gem").exists());
    assert_eq!(
        done.refused,
        ["b-1.gem: the store's file isn't the bytes its row hashes"]
    );
    // A name that isn't one file's is never joined to the cache folder.
    let plan = Seed {
        into: into.clone(),
        files: vec![SeedFile {
            path: "cache/a-1.gem".into(),
            name: "../a-1.gem".into(),
            sha256: None,
        }],
    };
    let done = seed(&dir, &rows, VerifiedBy::Signature, &plan, &STORE).unwrap();
    assert_eq!(done.refused.len(), 1);
    assert!(!t.path().join("a-1.gem").exists());
}

// covers: K-45
#[test]
fn a_seed_links_where_it_can_and_its_stamp_ends_the_next_one_at_one_read() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("part");
    let rows = part(&dir, &[("cache/a-1.gem", b"a")]);
    let into = t.path().join("cache");
    let plan = Seed {
        into: into.clone(),
        files: vec![wanted("a-1.gem", Some(Hex32::sha256(b"a")))],
    };
    let done = seed(&dir, &rows, VerifiedBy::Lockfile, &plan, &STORE).unwrap();
    assert_eq!((done.copied, done.stamped), (1, false));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let (store, cache) = (
            fs::metadata(dir.join("cache/a-1.gem")).unwrap(),
            fs::metadata(into.join("a-1.gem")).unwrap(),
        );
        assert_eq!(
            store.ino(),
            cache.ino(),
            "one file system: linked, not copied"
        );
    }
    assert!(into.join(crate::seed::stamp_name(&STORE)).is_file());
    // The same files again: the stamp answers, and the part isn't looked at.
    fs::remove_file(dir.join("cache/a-1.gem")).unwrap();
    let again = seed(&dir, &rows, VerifiedBy::Lockfile, &plan, &STORE).unwrap();
    assert!(again.stamped && again.refused.is_empty());
    // Other files asked for, or another store: the stamp doesn't answer.
    let other = Seed {
        into: into.clone(),
        files: vec![wanted("b-1.gem", None)],
    };
    assert!(
        !seed(&dir, &rows, VerifiedBy::Signature, &other, &STORE)
            .unwrap()
            .stamped
    );
    assert!(
        !seed(&dir, &rows, VerifiedBy::Lockfile, &plan, &Hex32([8; 32]))
            .unwrap()
            .stamped
    );
}
