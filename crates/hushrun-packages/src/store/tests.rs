use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::write::{self, PartMeta, StoreMeta};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn row(path: &str, kind: &str, sha256: &str) -> Row {
    let kind = match kind {
        "file" => Kind::File,
        "exe" => Kind::Exe,
        _ => Kind::Link,
    };
    Row {
        path: path.into(),
        kind,
        bytes: 0,
        sha256: Hex32::parse(sha256).unwrap(),
        target: None,
        package: None,
        locked: None,
    }
}

// covers: K-2
#[test]
fn the_three_digests_are_the_vectors() {
    let text = fs::read_to_string(repo_root().join("testdata/packages/vectors.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    for vector in doc["vectors"].as_array().unwrap() {
        let expected = &vector["expected"];
        let mut ids = Vec::new();
        for part in vector["parts"].as_array().unwrap() {
            let name = part["part"].as_str().unwrap();
            let class = if part["class"] == "built" {
                Class::Built
            } else {
                Class::Download
            };
            let rows: Vec<Row> = part["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    row(
                        r["path"].as_str().unwrap(),
                        r["kind"].as_str().unwrap(),
                        r["sha256"].as_str().unwrap(),
                    )
                })
                .collect();
            let id = part_identity(name, class, part["runtime"].as_str(), &rows);
            assert_eq!(id.to_string(), expected[format!("part:{name}")], "{name}");
            ids.push((name.to_string(), id));
        }
        let ids: Vec<(&str, Hex32)> = ids.iter().map(|(n, i)| (n.as_str(), *i)).collect();
        assert_eq!(store_identity(&ids).to_string(), expected["store"]);
        for case in vector["inputs"].as_array().unwrap() {
            let manifests: Vec<(String, Hex32)> = case["manifests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| {
                    (
                        m["path"].as_str().unwrap().to_string(),
                        Hex32::parse(m["sha256"].as_str().unwrap()).unwrap(),
                    )
                })
                .collect();
            let name = case["part"].as_str().unwrap();
            let got = inputs_digest(
                name,
                case["tool"].as_str().unwrap(),
                case["platform"].as_str().unwrap(),
                &manifests,
            );
            assert_eq!(
                got.to_string(),
                expected[format!("inputs:{name}")],
                "{name}"
            );
        }
    }
}

// covers: K-2
#[test]
fn the_reference_agrees_with_the_vectors() {
    let Ok(status) = std::process::Command::new("python3")
        .arg("tools/refpackages.py")
        .arg("check")
        .arg("testdata/packages/vectors.json")
        .current_dir(repo_root())
        .status()
    else {
        eprintln!("python3 isn't here: the reference isn't checked");
        return;
    };
    assert!(status.success());
}

/// A store of two parts in a new folder: `gradle` with a file, an executable and a link, and
/// `maven` with one file.
fn baked() -> (tempfile::TempDir, Packages) {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let gradle = store.join(PARTS).join("gradle/modules-2/files-2.1/g/n/1");
    fs::create_dir_all(&gradle).unwrap();
    fs::write(gradle.join("n-1.jar"), b"jar bytes").unwrap();
    fs::write(gradle.join("run.sh"), b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(gradle.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("n-1.jar", gradle.join("same.jar")).unwrap();
    }
    let maven = store.join(PARTS).join("maven/repository/g/n/1");
    fs::create_dir_all(&maven).unwrap();
    fs::write(maven.join("n-1.pom"), b"<project/>").unwrap();
    let meta = |part: &str| PartMeta {
        part: part.into(),
        class: Class::Download,
        tool: format!("{part} 1"),
        runtime: None,
        manifests: vec![("pom.xml".into(), Hex32::sha256(b"pom"))],
        verified_by: VerifiedBy::Signature,
    };
    let packages = write::seal(
        &store,
        &StoreMeta {
            repository: Some("github.com/acme/shop".into()),
            platform: "linux-x64".into(),
            baked_ms: 1,
        },
        &[meta("maven"), meta("gradle")],
        &|_, _| (None, None),
    )
    .unwrap();
    (dir, packages)
}

fn store(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store")
}

fn check(dir: &tempfile::TempDir, depth: Depth) -> Result<(), String> {
    let (packages, _) = Packages::read_bytes(&store(dir)).map_err(|e| e.0)?;
    check_contents(&store(dir), Some(&packages), depth)
        .map(|_| ())
        .map_err(|e| e.0)
}

// covers: K-1, K-2, K-3
#[test]
fn a_sealed_store_checks_out_by_names_and_by_bytes() {
    let (dir, packages) = baked();
    assert_eq!(packages.parts.len(), 2);
    assert_eq!(packages.parts[0].part, "gradle", "sorted by name");
    check(&dir, Depth::Names).unwrap();
    check(&dir, Depth::Bytes).unwrap();
    let rows = check_contents(&store(&dir), Some(&packages), Depth::Names).unwrap();
    #[cfg(unix)]
    {
        let gradle: Vec<(&str, Kind)> = rows[0].iter().map(|r| (r.path.as_str(), r.kind)).collect();
        assert_eq!(
            gradle,
            [
                ("modules-2/files-2.1/g/n/1/n-1.jar", Kind::File),
                ("modules-2/files-2.1/g/n/1/run.sh", Kind::Exe),
                ("modules-2/files-2.1/g/n/1/same.jar", Kind::Link),
            ]
        );
    }
    let _ = rows;
}

// covers: K-2
#[test]
fn the_same_files_seal_to_the_same_bytes() {
    let (a, _) = baked();
    let (b, _) = baked();
    for file in ["packages.json", "rows/gradle.jsonl", "rows/maven.jsonl"] {
        assert_eq!(
            fs::read(store(&a).join(file)).unwrap(),
            fs::read(store(&b).join(file)).unwrap(),
            "{file}"
        );
    }
}

// covers: K-1
#[test]
fn anything_else_in_the_folder_is_refused() {
    type Change<'a> = &'a dyn Fn(&Path);
    let cases: &[(&str, Change<'_>)] = &[
        ("another file", &|s| {
            fs::write(s.join("notes.txt"), b"x").unwrap()
        }),
        ("another folder", &|s| {
            fs::create_dir(s.join("packs")).unwrap()
        }),
        ("a file no row names", &|s| {
            fs::write(s.join("parts/maven/repository/g/n/1/extra.jar"), b"x").unwrap()
        }),
        ("a folder no row is under", &|s| {
            fs::create_dir(s.join("parts/maven/repository/empty")).unwrap()
        }),
        ("a row whose file is missing", &|s| {
            fs::remove_file(s.join("parts/maven/repository/g/n/1/n-1.pom")).unwrap()
        }),
        ("a rows file of no part", &|s| {
            fs::write(s.join("rows/npm.jsonl"), b"").unwrap()
        }),
        ("a part folder of no part", &|s| {
            fs::create_dir(s.join("parts/npm")).unwrap()
        }),
        ("a file of another size", &|s| {
            fs::write(
                s.join("parts/maven/repository/g/n/1/n-1.pom"),
                b"<project></project>",
            )
            .unwrap()
        }),
        ("a rows file edited", &|s| {
            let path = s.join("rows/maven.jsonl");
            let mut bytes = fs::read(&path).unwrap();
            bytes.push(b'\n');
            fs::write(path, bytes).unwrap()
        }),
    ];
    for (name, change) in cases {
        let (dir, _) = baked();
        change(&store(&dir));
        assert!(check(&dir, Depth::Names).is_err(), "{name}");
        assert!(check(&dir, Depth::Bytes).is_err(), "{name}");
    }
}

// covers: K-1
#[test]
fn a_byte_changed_in_place_is_found_only_by_bytes() {
    let (dir, _) = baked();
    let jar = store(&dir).join("parts/gradle/modules-2/files-2.1/g/n/1/n-1.jar");
    fs::write(&jar, b"jar bytEs").unwrap();
    check(&dir, Depth::Names).unwrap();
    let err = check(&dir, Depth::Bytes).unwrap_err();
    assert!(err.contains("isn't the file its row hashes"), "{err}");
}

// covers: K-1, K-3
#[cfg(unix)]
#[test]
fn links_and_special_files() {
    use std::os::unix::fs::PermissionsExt as _;
    // A link no row names.
    let (dir, _) = baked();
    std::os::unix::fs::symlink(
        "n-1.pom",
        store(&dir).join("parts/maven/repository/g/n/1/x"),
    )
    .unwrap();
    assert!(
        check(&dir, Depth::Names)
            .unwrap_err()
            .contains("no row names")
    );
    // A link at the top of the store.
    let (dir, _) = baked();
    std::os::unix::fs::symlink("/etc", store(&dir).join("etc")).unwrap();
    assert!(
        check(&dir, Depth::Names)
            .unwrap_err()
            .contains("link or a special file")
    );
    // A link whose target changed.
    let (dir, _) = baked();
    let link = store(&dir).join("parts/gradle/modules-2/files-2.1/g/n/1/same.jar");
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("run.sh", &link).unwrap();
    assert!(
        check(&dir, Depth::Names)
            .unwrap_err()
            .contains("isn't its row's")
    );
    // A file that may run, and its row says it may not.
    let (dir, _) = baked();
    let pom = store(&dir).join("parts/maven/repository/g/n/1/n-1.pom");
    fs::set_permissions(&pom, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(check(&dir, Depth::Names).unwrap_err().contains("may run"));
    // A special file: a socket.
    let (dir, _) = baked();
    let _socket =
        std::os::unix::net::UnixListener::bind(store(&dir).join("parts/maven/repository/g/n/1/s"))
            .unwrap();
    assert!(
        check(&dir, Depth::Names)
            .unwrap_err()
            .contains("special file")
    );
}

// covers: K-3
#[test]
fn a_link_stays_inside_its_part() {
    assert!(link_stays_inside("a/b/c", "d"));
    assert!(link_stays_inside("a/b/c", "../../d"));
    assert!(!link_stays_inside("a/b/c", "../../../d"));
    assert!(!link_stays_inside("a/b/c", "/etc/passwd"));
    assert!(!link_stays_inside("c", "../c"));
    assert!(link_stays_inside("a/c", "x/../../c"));
    assert!(!link_stays_inside("a/c", "../../a/c"));
    assert!(!is_row_path("a/../b"));
    assert!(!is_row_path("/a"));
    assert!(!is_row_path("a//b"));
    assert!(!is_row_path("a\nb"));
    assert!(is_row_path("modules-2/files-2.1/é/b"));
}

// covers: K-2
#[test]
fn packages_json_is_read_with_its_schema_first() {
    let (dir, packages) = baked();
    let bytes = packages.to_bytes();
    assert_eq!(Packages::parse(&bytes).unwrap(), packages);
    let edit = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        f(&mut value);
        Packages::parse(&serde_json::to_vec(&value).unwrap()).map_err(|e| e.0)
    };
    let err = edit(&|v| {
        v["schema"] = "hushrun.baked-packages/v2".into();
        v["new"] = 1.into();
    })
    .unwrap_err();
    assert!(err.contains("a version this hushrun doesn't read"), "{err}");
    assert!(
        edit(&|v| v["new"] = 1.into())
            .unwrap_err()
            .contains("unknown field")
    );
    assert!(
        edit(&|v| v["parts"][0]["new"] = 1.into())
            .unwrap_err()
            .contains("unknown field")
    );
    assert!(
        edit(&|v| {
            let first = v["parts"][0].clone();
            v["parts"].as_array_mut().unwrap().push(first);
        })
        .unwrap_err()
        .contains("twice")
    );
    assert!(
        edit(&|v| v["identity"] = "0".repeat(64).into())
            .unwrap_err()
            .contains("identity")
    );
    assert!(
        edit(&|v| v["parts"][0]["class"] = "built".into())
            .unwrap_err()
            .contains("no bake writes")
    );
    assert!(
        edit(&|v| v["parts"][0]["runtime"] = "node 22".into())
            .unwrap_err()
            .contains("no bake writes")
    );
    assert!(edit(&|v| v["parts"][0]["part"] = "Gradle".into()).is_err());
    assert!(
        edit(&|v| v["parts"][0]["manifests"][0] = "../pom.xml".into())
            .unwrap_err()
            .contains("manifest")
    );
    // A part's identity that isn't its rows' is found as its rows are read.
    let mut lying = packages.clone();
    lying.parts[0].identity = Hex32::sha256(b"other");
    let ids: Vec<(&str, Hex32)> = lying
        .parts
        .iter()
        .map(|p| (p.part.as_str(), p.identity))
        .collect();
    lying.identity = store_identity(&ids);
    fs::write(store(&dir).join(PACKAGES_JSON), lying.to_bytes()).unwrap();
    assert!(
        check(&dir, Depth::Names)
            .unwrap_err()
            .contains("isn't its rows'")
    );
}
