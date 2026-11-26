use std::ffi::OsString;
use std::path::Path;

use crate::part::Locks;
use crate::part::Part as _;
use crate::part::{self, Reader};
use crate::parts::{cargo::Cargo, go::Go, gradle::Gradle, maven::Maven};

fn paths(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn no_env(_: &str) -> Option<OsString> {
    None
}

// covers: K-4
#[test]
fn each_part_names_its_manifest_set() {
    let tracked = paths(&[
        "settings.gradle.kts",
        "app/build.gradle.kts",
        "gradle/libs.versions.toml",
        "gradle/wrapper/gradle-wrapper.properties",
        "gradle/wrapper/gradle-wrapper.jar",
        "pom.xml",
        "core/pom.xml",
        ".mvn/wrapper/maven-wrapper.properties",
        ".mvn/jvm.config",
        "src/main/java/A.java",
    ]);
    assert_eq!(
        Gradle.manifests(&tracked),
        paths(&[
            "settings.gradle.kts",
            "app/build.gradle.kts",
            "gradle/libs.versions.toml",
            "gradle/wrapper/gradle-wrapper.properties",
        ])
    );
    assert_eq!(
        Maven.manifests(&tracked),
        paths(&[
            "pom.xml",
            "core/pom.xml",
            ".mvn/wrapper/maven-wrapper.properties"
        ])
    );
    let found: Vec<&str> = part::manifests(&tracked)
        .iter()
        .map(|(p, _)| p.name())
        .collect();
    assert_eq!(found, ["gradle", "maven"]);
    assert!(part::manifests(&paths(&["README.md"])).is_empty());
    let tracked = paths(&[
        "Cargo.toml",
        "Cargo.lock",
        "crates/a/Cargo.toml",
        "go.mod",
        "go.sum",
        "tools/go.mod",
        "go.work",
        "go.work.sum",
        "src/lib.rs",
    ]);
    assert_eq!(
        Cargo.manifests(&tracked),
        paths(&["Cargo.toml", "Cargo.lock", "crates/a/Cargo.toml"])
    );
    assert_eq!(
        Go.manifests(&tracked),
        paths(&["go.mod", "go.sum", "tools/go.mod", "go.work", "go.work.sum"])
    );
}

// covers: K-5
#[test]
fn a_harvest_keeps_downloads_and_leaves_out_locks_snapshots_and_the_tools_notes() {
    for kept in [
        "files-2.1/junit/junit/4.13.2/8ac9e16d933b6fb43bc7f576336b8f4d7eb5ba12/junit-4.13.2.jar",
        "metadata-2.107/descriptors/junit/junit/4.13.2/abc/descriptor.bin",
    ] {
        assert!(Gradle.keep(kept), "{kept}");
    }
    for left in [
        "modules-2.lock",
        "gc.properties",
        "files-2.1/g/n/1.0-SNAPSHOT/abc/n-1.0-SNAPSHOT.jar",
    ] {
        assert!(!Gradle.keep(left), "{left}");
    }
    assert!(Maven.keep("junit/junit/4.13.2/junit-4.13.2.jar"));
    assert!(Maven.keep("junit/junit/4.13.2/junit-4.13.2.pom.sha1"));
    for left in [
        "junit/junit/4.13.2/_remote.repositories",
        "junit/junit/4.13.2/junit-4.13.2.jar.lastUpdated",
        "junit/junit/resolver-status.properties",
        "g/n/1.0-SNAPSHOT/n-1.0-SNAPSHOT.jar",
        "g/n/1.0-SNAPSHOT/maven-metadata-central.xml",
        ".locks/junit~junit~4.13.2.lock",
        ".cache/x",
    ] {
        assert!(!Maven.keep(left), "{left}");
    }
    assert!(Cargo.keep("index.crates.io-1949cf8c6b5b557f/itoa-1.0.11.crate"));
    for left in [
        "index.crates.io-1949cf8c6b5b557f/.package-cache",
        "index.crates.io-1949cf8c6b5b557f/itoa-1.0.11.crate.tmp",
        ".global-cache/x.crate",
        "itoa-1.0.11.crate",
        "a/b/itoa-1.0.11.crate",
    ] {
        assert!(!Cargo.keep(left), "{left}");
    }
    for kept in [
        "golang.org/x/text/@v/v0.14.0.zip",
        "golang.org/x/text/@v/v0.14.0.mod",
        "golang.org/x/text/@v/v0.14.0.info",
        "github.com/!burnt!sushi/toml/@v/v1.3.2.zip",
    ] {
        assert!(Go.keep(kept), "{kept}");
    }
    for left in [
        "golang.org/x/text/@v/list",
        "golang.org/x/text/@v/v0.14.0.lock",
        "golang.org/x/text/@v/v0.14.0.ziphash",
        "golang.org/x/text/@v/v0.14.0.zip.partial",
        "golang.org/x/text/@v/v0.14.0.zip.tmp123",
        "golang.org/x/text/@v/v0.14.0.tmp1.zip",
        "sumdb/sum.golang.org/lookup/golang.org/x/text@v0.14.0",
        "sumdb/x/@v/v1.0.0.zip",
        "@v/v1.0.0.zip",
    ] {
        assert!(!Go.keep(left), "{left}");
    }
}

// covers: K-3
#[test]
fn a_file_names_its_package_where_its_path_says() {
    assert_eq!(
        Gradle
            .package("files-2.1/junit/junit/4.13.2/8ac9/junit-4.13.2.jar")
            .as_deref(),
        Some("junit:junit:4.13.2")
    );
    assert_eq!(Gradle.package("metadata-2.107/x.bin"), None);
    assert_eq!(
        Maven
            .package("org/hamcrest/hamcrest-core/1.3/hamcrest-core-1.3.jar")
            .as_deref(),
        Some("org.hamcrest:hamcrest-core:1.3")
    );
    assert_eq!(
        Maven.package("org/hamcrest/hamcrest-core/maven-metadata.xml"),
        None
    );
    for (path, package) in [
        ("index.crates.io-x/itoa-1.0.11.crate", "itoa@1.0.11"),
        (
            "index.crates.io-x/x25519-dalek-2.0.0.crate",
            "x25519-dalek@2.0.0",
        ),
        (
            "index.crates.io-x/foo-2d-1.0.0-beta.1.crate",
            "foo-2d@1.0.0-beta.1",
        ),
    ] {
        assert_eq!(Cargo.package(path).as_deref(), Some(package), "{path}");
    }
    assert_eq!(Cargo.package("index.crates.io-x/itoa.crate"), None);
    assert_eq!(
        Go.package("github.com/!burnt!sushi/toml/@v/v1.3.2.zip")
            .as_deref(),
        Some("github.com/BurntSushi/toml@v1.3.2")
    );
}

// covers: K-3
#[test]
fn a_row_is_locked_by_the_hash_its_lockfile_writes() {
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(
        checkout.path().join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"itoa\"\nversion = \"1.0.11\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"49f1\"\n\n[[package]]\nname = \"mine\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"g\"\nversion = \"0.1.0\"\nsource = \"git+https://example.com/g#abc\"\n",
    )
    .unwrap();
    let locks = Cargo.locks(checkout.path(), &paths(&["Cargo.lock", "Cargo.toml"]));
    assert_eq!(
        locks,
        Locks::from([("itoa-1.0.11.crate".to_string(), vec!["49f1".to_string()])]),
        "a path or git package has no checksum"
    );
    assert_eq!(
        Cargo
            .locked("cache/index.crates.io-x/itoa-1.0.11.crate", &locks)
            .as_deref(),
        Some("49f1")
    );
    std::fs::write(
        checkout.path().join("go.sum"),
        "golang.org/x/text v0.14.0 h1:zip=\ngolang.org/x/text v0.14.0/go.mod h1:mod=\n",
    )
    .unwrap();
    let locks = Go.locks(checkout.path(), &paths(&["go.mod", "go.sum"]));
    let at = "golang.org/x/text/@v/v0.14.0";
    assert_eq!(
        Go.locked(&format!("{at}.zip"), &locks).as_deref(),
        Some("h1:zip=")
    );
    assert_eq!(
        Go.locked(&format!("{at}.mod"), &locks).as_deref(),
        Some("h1:mod=")
    );
    assert_eq!(Go.locked(&format!("{at}.info"), &locks), None);
}

// covers: K-10
#[test]
fn cargo_s_seed_copies_only_the_bytes_cargo_lock_pins_and_never_twice() {
    use crate::part::Seeded;
    use crate::store::row_of;
    let part = tempfile::tempdir().unwrap();
    let crates = part.path().join("registry/cache/index.crates.io-x");
    std::fs::create_dir_all(&crates).unwrap();
    std::fs::write(crates.join("a-1.0.0.crate"), b"a").unwrap();
    std::fs::write(crates.join("b-1.0.0.crate"), b"b, changed").unwrap();
    std::fs::write(crates.join("c-1.0.0.crate"), b"c").unwrap();
    let rows: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|n| {
            let path = format!("registry/cache/index.crates.io-x/{n}-1.0.0.crate");
            row_of(&part.path().join(&path), &path).unwrap().unwrap()
        })
        .collect();
    let sha = |b: &[u8]| hushrun_core::blocks::Hex32::sha256(b).to_string();
    // a is pinned to its bytes, b to other bytes, c not at all.
    let locks = Locks::from([
        ("a-1.0.0.crate".to_string(), vec![sha(b"a")]),
        ("b-1.0.0.crate".to_string(), vec![sha(b"b")]),
    ]);
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        Cargo.seed(part.path(), &rows, home.path(), &locks),
        Seeded {
            copied: 1,
            refused: 1,
            unpinned: 1,
            ..Seeded::default()
        }
    );
    let cache = home.path().join("registry/cache/index.crates.io-x");
    assert_eq!(std::fs::read(cache.join("a-1.0.0.crate")).unwrap(), b"a");
    assert!(!cache.join("b-1.0.0.crate").exists());
    assert!(!cache.join("c-1.0.0.crate").exists());
    assert_eq!(
        std::fs::read_dir(&cache).unwrap().count(),
        1,
        "no file left half written"
    );
    // Again: the cache holds it, nothing is copied or hashed.
    assert_eq!(
        Cargo.seed(part.path(), &rows, home.path(), &locks),
        Seeded {
            had: 1,
            refused: 1,
            unpinned: 1,
            ..Seeded::default()
        }
    );
    let env = |name: &str| match name {
        "HOME" => Some(OsString::from("/home/dev")),
        _ => None,
    };
    assert_eq!(
        Cargo.reader(part.path(), Path::new("/c"), &env),
        Reader::Seed("/home/dev/.cargo".into())
    );
    let env = |name: &str| (name == "CARGO_HOME").then(|| OsString::from("/opt/cargo"));
    assert_eq!(
        Cargo.reader(part.path(), Path::new("/c"), &env),
        Reader::Seed("/opt/cargo".into())
    );
    assert!(matches!(
        Cargo.reader(part.path(), Path::new("/c"), &no_env),
        Reader::LeftAlone(_)
    ));
}

// covers: K-10
#[test]
fn go_s_reader_puts_the_part_before_the_proxies_go_would_use() {
    use crate::parts::go::{Settings, reader_for};
    let dir = Path::new("/hushrun-packages/parts/go");
    let settings = |proxy: &str, unchecked| Settings {
        proxy: proxy.to_string(),
        unchecked,
    };
    assert_eq!(
        reader_for(
            dir,
            Some(&settings("https://proxy.golang.org,direct", None))
        ),
        Reader::InPlace(vec![(
            "GOPROXY".into(),
            "file:///hushrun-packages/parts/go/download,https://proxy.golang.org,direct".into()
        )])
    );
    // A proxy from Go's environment file stays where Go would go next.
    assert_eq!(
        reader_for(
            dir,
            Some(&settings("https://goproxy.example.com|off", None))
        ),
        Reader::InPlace(vec![(
            "GOPROXY".into(),
            "file:///hushrun-packages/parts/go/download,https://goproxy.example.com|off".into()
        )])
    );
    // A module go.sum lacks that Go takes unchecked: left alone.
    assert_eq!(
        reader_for(dir, Some(&settings("direct", Some("GOPRIVATE")))),
        Reader::LeftAlone(
            "GOPRIVATE is set, so Go takes some modules go.sum doesn't name unchecked".into()
        )
    );
    assert!(matches!(reader_for(dir, None), Reader::LeftAlone(_)));
    assert!(matches!(
        reader_for(
            Path::new("/tmp/my packages/parts/go"),
            Some(&settings("direct", None))
        ),
        Reader::LeftAlone(_)
    ));
    // A GOPROXY the person set: left alone, before Go is asked anything.
    let set = |name: &str| (name == "GOPROXY").then(|| OsString::from("https://mine"));
    assert_eq!(
        Go.reader(dir, Path::new("/c"), &set),
        Reader::LeftAlone("GOPROXY is set already".into())
    );
}

// covers: K-10
#[test]
fn the_readers() {
    let dir = Path::new("/hushrun-packages/parts/gradle");
    assert_eq!(
        Gradle.reader(dir, Path::new("/c"), &no_env),
        Reader::InPlace(vec![("GRADLE_RO_DEP_CACHE".into(), dir.into())])
    );
    let set = |name: &str| (name == "GRADLE_RO_DEP_CACHE").then(|| OsString::from("/mine"));
    assert_eq!(
        Gradle.reader(dir, Path::new("/c"), &set),
        Reader::LeftAlone("GRADLE_RO_DEP_CACHE is set already".into())
    );
    let checkout = tempfile::tempdir().unwrap();
    let dir = Path::new("/hushrun-packages/parts/maven");
    assert_eq!(
        Maven.reader(dir, checkout.path(), &no_env),
        Reader::MavenOpts(
            "-Dmaven.repo.local.tail=/hushrun-packages/parts/maven/repository".into()
        )
    );
    // A tail the person named, in any of the four places, keeps the part out.
    let opts = |name: &str| {
        (name == "MAVEN_OPTS").then(|| OsString::from("-Xmx1g -Dmaven.repo.local.tail=/x"))
    };
    assert_eq!(
        Maven.reader(dir, checkout.path(), &opts),
        Reader::LeftAlone("maven.repo.local.tail is set already (MAVEN_OPTS)".into())
    );
    std::fs::create_dir_all(checkout.path().join(".mvn")).unwrap();
    std::fs::write(
        checkout.path().join(".mvn/jvm.config"),
        "-Dmaven.repo.local.tail=/y\n",
    )
    .unwrap();
    assert_eq!(
        Maven.reader(dir, checkout.path(), &no_env),
        Reader::LeftAlone("maven.repo.local.tail is set already (.mvn/jvm.config)".into())
    );
    // A folder with a space can't ride in MAVEN_OPTS; Gradle's variable is not split.
    let spaced = Path::new("/tmp/my packages/parts/maven");
    assert_eq!(
        Maven.reader(spaced, Path::new("/c"), &no_env),
        Reader::LeftAlone("the store's folder, /tmp/my packages/parts/maven, has a space, which MAVEN_OPTS can't hold".into())
    );
    assert!(matches!(
        Gradle.reader(
            Path::new("/tmp/my packages/parts/gradle"),
            Path::new("/c"),
            &no_env
        ),
        Reader::InPlace(_)
    ));
    assert_eq!(
        crate::parts::maven::opts_with(Some(" -Xmx1g "), "-Dw"),
        "-Xmx1g -Dw"
    );
    assert_eq!(crate::parts::maven::opts_with(None, "-Dw"), "-Dw");
}

// covers: K-10
#[test]
fn every_part_module_is_listed() {
    let listed: Vec<&str> = part::ALL.iter().map(|p| p.name()).collect();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/parts");
    let mut modules: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".rs"))
                .map(str::to_string)
        })
        .filter(|n| n != "mod" && n != "tests")
        .collect();
    modules.sort();
    assert_eq!(listed, modules);
}
