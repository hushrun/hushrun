use std::ffi::OsString;
use std::path::Path;

use crate::part::Part as _;
use crate::part::{self, Reader};
use crate::parts::{gradle::Gradle, maven::Maven};

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
