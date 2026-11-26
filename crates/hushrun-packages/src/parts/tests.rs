use std::ffi::OsString;
use std::path::Path;

use crate::part::Part as _;
use crate::part::{self, Reader};
use crate::parts::{gradle::Gradle, hex::Hex, maven::Maven};

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
    let tracked = paths(&[
        "mix.exs",
        "mix.lock",
        "apps/web/mix.exs",
        "lib/a.ex",
        "mix.lock.bak",
    ]);
    assert_eq!(
        Hex.manifests(&tracked),
        paths(&["mix.exs", "mix.lock", "apps/web/mix.exs"])
    );
    assert!(part::manifests(&paths(&["Cargo.toml"])).is_empty());
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

/// A `mix.lock` as Mix 1.18 and Hex 2.7 write it: two Hex packages, a git dependency and a
/// package of an organization's repository.
const MIX_LOCK: &str = r#"%{
  "decimal": {:hex, :decimal, "2.3.0", "3ad6255aa77b4a3c4f818171b12d237500e63525c2fd056699967a3e7ea20f62", [:mix], [], "hexpm", "a4d66355cb29cb47c3cf30e71329e58361cfcb37c34235ef3bf1d7bf3773aeac"},
  "jason": {:hex, :jason, "1.4.4", "b9226785a9aa77b6857ca22832cffa5d5011a667207eb2a0ad56adb5db443b8a", [:mix], [{:decimal, "~> 1.0 or ~> 2.0", [hex: :decimal, repo: "hexpm", optional: true]}], "hexpm", "c5eb0cab91f094599f94d55bc63409236a8ec69a21a67814529e8d5f6cc90b3b"},
  "mine": {:git, "https://example.com/mine.git", "0123456789abcdef0123456789abcdef01234567", [branch: "main"]},
  "secret": {:hex, :secret, "1.0.0", "1111111111111111111111111111111111111111111111111111111111111111", [:mix], [], "hexpm:acme", "2222222222222222222222222222222222222222222222222222222222222222"},
}
"#;

// covers: K-3, K-5
#[test]
fn hex_keeps_tarballs_and_reads_the_outer_checksum_of_mix_lock() {
    for kept in ["jason-1.4.4.tar", "phoenix_live_view-1.0.0-rc.7.tar"] {
        assert!(Hex.keep(kept), "{kept}");
    }
    for left in [
        "cache.bin",
        ".jason-1.4.4.tar.123.hushrun",
        "jason-1.4.4.tar.tmp",
        "Jason-1.4.4.tar",
        "nested/jason-1.4.4.tar",
        "jason.tar",
    ] {
        assert!(!Hex.keep(left), "{left}");
    }
    assert_eq!(
        Hex.package("hexpm/phoenix_live_view-1.0.0-rc.7.tar")
            .as_deref(),
        Some("phoenix_live_view 1.0.0-rc.7")
    );
    let pinned = crate::parts::hex::lockfile_checksums(MIX_LOCK).unwrap();
    assert_eq!(
        pinned,
        [
            (
                "decimal-2.3.0.tar".to_string(),
                "a4d66355cb29cb47c3cf30e71329e58361cfcb37c34235ef3bf1d7bf3773aeac".to_string()
            ),
            (
                "jason-1.4.4.tar".to_string(),
                "c5eb0cab91f094599f94d55bc63409236a8ec69a21a67814529e8d5f6cc90b3b".to_string()
            ),
        ],
        "a git dependency and an organization's package pin nothing here"
    );
    // The older `"name" => {…}` keys read the same; an empty lockfile pins nothing.
    let older = MIX_LOCK.replace("\": {", "\" => {");
    assert_eq!(
        crate::parts::hex::lockfile_checksums(&older).unwrap(),
        pinned
    );
    for empty in ["%{}", "%{\n}\n"] {
        assert!(
            crate::parts::hex::lockfile_checksums(empty)
                .unwrap()
                .is_empty()
        );
    }
    // A form it doesn't know gives no hashes: an error that names what it is.
    let six = r#"%{"jason": {:hex, :jason, "1.4.4", "b9226785a9aa77b6857ca22832cffa5d5011a667207eb2a0ad56adb5db443b8a", [:mix], []},}"#;
    let why = crate::parts::hex::lockfile_checksums(six).unwrap_err();
    assert!(why.contains("of 6 fields"), "{why}");
    let spread = "%{\n  \"jason\": {:hex, :jason, \"1.4.4\",\n    \"b9\"},\n}\n";
    assert!(crate::parts::hex::lockfile_checksums(spread).is_err());
    assert!(crate::parts::hex::lockfile_checksums("[]").is_err());
    let short = MIX_LOCK.replace("c5eb0cab", "c5eb");
    assert!(crate::parts::hex::lockfile_checksums(&short).is_err());
    // Through the part: a lockfile among the manifests, keyed by the tarball's file name.
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(checkout.path().join("mix.lock"), MIX_LOCK).unwrap();
    let locks = Hex
        .locks(checkout.path(), &paths(&["mix.exs", "mix.lock"]))
        .unwrap();
    assert_eq!(
        Hex.locked("packages/hexpm/jason-1.4.4.tar", &locks)
            .as_deref(),
        Some("c5eb0cab91f094599f94d55bc63409236a8ec69a21a67814529e8d5f6cc90b3b")
    );
    assert!(
        Hex.pins("packages/hexpm/jason-1.4.3.tar", &locks)
            .is_empty()
    );
    std::fs::write(checkout.path().join("mix.lock"), six).unwrap();
    let why = Hex
        .locks(checkout.path(), &paths(&["mix.lock"]))
        .unwrap_err();
    assert!(
        why.starts_with("mix.lock is in a form this hushrun doesn't read ("),
        "{why}"
    );
}

// covers: K-10
#[test]
fn hex_is_seeded_into_the_cache_hex_would_use() {
    let env = |vars: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| OsString::from(v))
        }
    };
    let seed = |path: &str| Reader::Seed(Path::new(path).to_path_buf());
    let reader = |vars| Hex.reader(Path::new("/p"), Path::new("/c"), &env(vars));
    assert_eq!(reader(&[("HOME", "/home/me")]), seed("/home/me/.hex"));
    assert_eq!(
        reader(&[("HOME", "/home/me"), ("HEX_HOME", "/h"), ("MIX_XDG", "1")]),
        seed("/h")
    );
    assert_eq!(
        reader(&[("HOME", "/home/me"), ("MIX_XDG", "true")]),
        seed("/home/me/.cache/hex")
    );
    assert_eq!(
        reader(&[
            ("HOME", "/home/me"),
            ("MIX_XDG", "1"),
            ("XDG_CACHE_HOME", "/xc")
        ]),
        seed("/xc/hex")
    );
    assert_eq!(
        reader(&[
            ("HOME", "/home/me"),
            ("MIX_XDG", "yes"),
            ("XDG_CACHE_HOME", "/xc")
        ]),
        seed("/home/me/.hex"),
        "Hex reads only 1 and true"
    );
    assert_eq!(
        reader(&[("HOME", "/home/me"), ("HEX_HOME", "")]),
        Reader::LeftAlone("HEX_HOME is empty, not an absolute folder".into())
    );
    assert_eq!(
        reader(&[("HEX_HOME", "rel")]),
        Reader::LeftAlone("HEX_HOME is `rel`, not an absolute folder".into())
    );
    assert_eq!(
        reader(&[]),
        Reader::LeftAlone("neither HEX_HOME nor HOME is set".into())
    );
}
