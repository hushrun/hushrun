use std::ffi::OsString;
use std::path::Path;

use crate::part::Part as _;
use crate::part::{self, Reader};
use crate::parts::{gradle::Gradle, maven::Maven, nuget::Nuget};

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

// covers: K-4
#[test]
fn nugets_manifest_set_is_its_projects_lockfiles_and_configs() {
    let tracked = paths(&[
        "Calc.sln",
        "nuget.config",
        "global.json",
        "Directory.Packages.props",
        "Directory.Build.props",
        ".config/dotnet-tools.json",
        "src/Calc/Calc.csproj",
        "src/Calc/packages.lock.json",
        "src/Calc/Ops.cs",
        "src/Fs/Fs.fsproj",
        "src/Vb/Vb.vbproj",
        "legacy/packages.config",
        "legacy/NuGet.Config",
    ]);
    assert_eq!(
        Nuget.manifests(&tracked),
        paths(&[
            "nuget.config",
            "global.json",
            "Directory.Packages.props",
            "Directory.Build.props",
            ".config/dotnet-tools.json",
            "src/Calc/Calc.csproj",
            "src/Calc/packages.lock.json",
            "src/Fs/Fs.fsproj",
            "src/Vb/Vb.vbproj",
            "legacy/packages.config",
            "legacy/NuGet.Config",
        ])
    );
}

// covers: K-5
#[test]
fn a_nuget_harvest_keeps_package_folders_and_their_mark() {
    for kept in [
        "newtonsoft.json/13.0.3/newtonsoft.json.13.0.3.nupkg",
        "newtonsoft.json/13.0.3/newtonsoft.json.13.0.3.nupkg.sha512",
        "newtonsoft.json/13.0.3/newtonsoft.json.nuspec",
        // NuGet's mark that the folder is complete, and the package's signature: dot files a
        // package's folder needs.
        "newtonsoft.json/13.0.3/.nupkg.metadata",
        "newtonsoft.json/13.0.3/.signature.p7s",
        "newtonsoft.json/13.0.3/lib/net6.0/Newtonsoft.Json.dll",
        "xunit/2.9.2-beta.1/xunit.nuspec",
    ] {
        assert!(Nuget.keep(kept), "{kept}");
    }
    for left in [
        ".tools/dotnet-ef/8.0.0/x.dll",
        "newtonsoft.json/13.0.3",
        "newtonsoft.json/tmp1a2b.c3d/newtonsoft.json.13.0.3.nupkg",
        "newtonsoft.json/13.0.3/x.lock",
        "stray.txt",
    ] {
        assert!(!Nuget.keep(left), "{left}");
    }
    assert_eq!(
        Nuget
            .package("newtonsoft.json/13.0.3/lib/net6.0/Newtonsoft.Json.dll")
            .as_deref(),
        Some("newtonsoft.json/13.0.3")
    );
    assert_eq!(Nuget.package("newtonsoft.json/13.0.3"), None);
}

// covers: K-10
#[test]
fn nugets_reader_carries_the_persons_and_the_configs_fallback_folders_first() {
    let checkout = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let home_path = home.path().as_os_str().to_owned();
    let dir = Path::new("/hushrun-packages/parts/nuget");
    let ours = "/hushrun-packages/parts/nuget/packages";
    let only_home = |name: &str| (name == "HOME").then(|| home_path.clone());
    assert_eq!(
        Nuget.reader(dir, checkout.path(), &only_home),
        Reader::InPlace(vec![("NUGET_FALLBACK_PACKAGES".into(), ours.into())])
    );
    // The variable replaces every folder a config names: the person's own come first.
    let theirs = |name: &str| match name {
        "NUGET_FALLBACK_PACKAGES" => Some(OsString::from("/mine;/theirs;")),
        _ => None,
    };
    assert_eq!(
        Nuget.reader(dir, checkout.path(), &theirs),
        Reader::InPlace(vec![(
            "NUGET_FALLBACK_PACKAGES".into(),
            format!("/mine;/theirs;{ours}").into()
        )])
    );
    let already = |name: &str| {
        (name == "NUGET_FALLBACK_PACKAGES").then(|| OsString::from(format!("/mine;{ours}")))
    };
    assert_eq!(
        Nuget.reader(dir, checkout.path(), &already),
        Reader::InPlace(vec![(
            "NUGET_FALLBACK_PACKAGES".into(),
            format!("/mine;{ours}").into()
        )])
    );
    // The variable replaces every folder a config names, so the configs' list is carried first, as
    // NuGet merges it (confirmed with the SDK): the closest config's own first, a key it named
    // skipped further out, a relative folder from its config's folder.
    let user = home.path().join(".nuget/NuGet");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(
        user.join("NuGet.Config"),
        "<configuration><packageSources><clear /></packageSources><fallbackPackageFolders><add key=\"a\" value=\"/f1\" /><add key='b' value='/f2'/></fallbackPackageFolders></configuration>\n",
    )
    .unwrap();
    std::fs::write(
        checkout.path().join("NuGet.Config"),
        "<configuration>\n  <fallbackPackageFolders>\n    <!-- the team's -->\n    <add key=\"c\" value=\"rel\" />\n    <add key=\"a\" value=\"/f3\" />\n  </fallbackPackageFolders>\n</configuration>\n",
    )
    .unwrap();
    let rel = checkout.path().join("rel");
    assert_eq!(
        Nuget.reader(dir, checkout.path(), &only_home),
        Reader::InPlace(vec![(
            "NUGET_FALLBACK_PACKAGES".into(),
            format!("{};/f3;/f2;{ours}", rel.display()).into()
        )])
    );
    // `<clear />` ends the list at its config.
    std::fs::write(
        checkout.path().join("NuGet.Config"),
        "<configuration><fallbackPackageFolders><add key=\"x\" value=\"/x\" /><clear /><add key=\"c\" value=\"/f3\" /></fallbackPackageFolders></configuration>\n",
    )
    .unwrap();
    assert_eq!(
        Nuget.reader(dir, checkout.path(), &only_home),
        Reader::InPlace(vec![(
            "NUGET_FALLBACK_PACKAGES".into(),
            format!("/f3;{ours}").into()
        )])
    );
    // A list hushrun can't read exactly: the part is left alone, and no folder is dropped.
    std::fs::write(
        checkout.path().join("NuGet.Config"),
        "<configuration><fallbackPackageFolders><add key=\"x\" value=\"%TEAM%/pkgs\" /></fallbackPackageFolders></configuration>\n",
    )
    .unwrap();
    assert!(matches!(
        Nuget.reader(dir, checkout.path(), &only_home),
        Reader::LeftAlone(why) if why.contains("names fallback folders with a variable or an entity")
    ));
    // A folder with a `;` can't ride in the variable, which NuGet splits on it.
    assert!(matches!(
        Nuget.reader(Path::new("/tmp/a;b/parts/nuget"), Path::new("/nonexistent-c"), &no_env),
        Reader::LeftAlone(why) if why.contains("has a `;`")
    ));
}

// covers: K-4, K-5, K-18
#[test]
fn pnpm_keeps_a_stores_files_and_indexes_and_names_each_index_s_package() {
    use crate::parts::pnpm::Pnpm;
    let tracked = [
        "pnpm-lock.yaml",
        "web/pnpm-lock.yaml",
        "package.json",
        "yarn.lock",
    ]
    .map(String::from);
    assert_eq!(
        Pnpm.manifests(&tracked),
        vec![
            "pnpm-lock.yaml".to_string(),
            "web/pnpm-lock.yaml".to_string()
        ]
    );
    assert!(Pnpm.keep("v10/files/aa/0123"));
    assert!(Pnpm.keep("v10/files/aa/0123-exec"));
    assert!(Pnpm.keep("v10/index/e8/5973-ms@2.1.3.json"));
    assert!(!Pnpm.keep("v10/projects/dabb"));
    assert!(!Pnpm.keep("v10/tmp/x/y"));
    assert!(!Pnpm.keep("v10/files"));
    assert!(!Pnpm.keep("other/files/aa/x"));
    assert_eq!(
        Pnpm.package("v10/index/e8/5973-ms@2.1.3.json").as_deref(),
        Some("ms@2.1.3")
    );
    assert_eq!(
        Pnpm.package("v10/index/e8/5973-@types+node@26.6.6.json")
            .as_deref(),
        Some("@types+node@26.6.6")
    );
    assert_eq!(Pnpm.package("v10/files/aa/0123"), None);
    let no_env = |_: &str| None;
    assert_eq!(
        Pnpm.reader(Path::new("/p"), Path::new("/c"), &no_env),
        Reader::Seed
    );
    let named = |name: &str| (name == "npm_config_store_dir").then(|| "/s".into());
    assert_eq!(
        Pnpm.seed_into(Path::new("/c"), &named),
        Ok((std::path::PathBuf::from("/s"), "store"))
    );
    assert!(Maven.seed_into(Path::new("/c"), &no_env).is_err());
    let t = tempfile::tempdir().unwrap();
    assert_eq!(Pnpm.harvest_root(t.path()), None);
    std::fs::create_dir_all(t.path().join("v10/files")).unwrap();
    assert_eq!(
        Pnpm.harvest_root(t.path()),
        Some((t.path().to_path_buf(), "store"))
    );
}

// covers: K-3, K-4, K-5, K-19
#[test]
fn npm_keeps_only_tarballs_and_seeds_only_what_the_lockfile_names_by_its_bytes() {
    use crate::part::Admit;
    use crate::parts::npm::{Npm, locked_sha512};
    use sha2_hw::Digest as _;
    let tracked = [
        "package-lock.json",
        "a/npm-shrinkwrap.json",
        "pnpm-lock.yaml",
    ]
    .map(String::from);
    assert_eq!(
        Npm.manifests(&tracked),
        vec![
            "package-lock.json".to_string(),
            "a/npm-shrinkwrap.json".to_string()
        ]
    );
    let tarball = b"a tarball";
    let hex: String = sha2_hw::Sha512::digest(tarball)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let path = format!(
        "content-v2/sha512/{}/{}/{}",
        &hex[..2],
        &hex[2..4],
        &hex[4..]
    );
    assert!(Npm.keep(&path));
    assert!(!Npm.keep("index-v5/aa/bb/cc"));
    assert!(!Npm.keep("content-v2/sha1/aa/bb/cc"));
    assert!(!Npm.keep("_logs/x.log"));
    let integrity = Npm.locked(&path).unwrap();
    assert!(integrity.starts_with("sha512-"), "{integrity}");
    // Every version of the lockfile: `packages` and nested `dependencies`.
    let lock = format!(
        "{{\"lockfileVersion\":3,\"packages\":{{\"node_modules/a\":{{\"integrity\":\"{integrity}\"}}}},\"dependencies\":{{\"b\":{{\"dependencies\":{{\"c\":{{\"integrity\":\"sha1-xx sha512-bad\"}}}}}}}}}}"
    );
    assert_eq!(
        locked_sha512(&lock),
        std::collections::BTreeSet::from([hex.clone()])
    );
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("package-lock.json"), &lock).unwrap();
    let admit = Npm.admit(t.path(), &["package-lock.json".to_string()]);
    assert_eq!(admit(&path, tarball), Admit::Copy);
    // A store whose rows were rewritten to match other bytes: the lockfile still catches it.
    assert_eq!(admit(&path, b"another tarball"), Admit::Changed);
    // A tarball the lockfile doesn't name now is left.
    let other = format!("content-v2/sha512/00/00/{}", "0".repeat(124));
    assert_eq!(admit(&other, b"x"), Admit::NotLocked);
    let no_env = |_: &str| None;
    assert_eq!(
        Npm.reader(Path::new("/p"), Path::new("/c"), &no_env),
        Reader::Seed
    );
    let named = |name: &str| (name == "npm_config_cache").then(|| "/c".into());
    assert_eq!(
        Npm.seed_into(Path::new("/x"), &named),
        Ok((std::path::PathBuf::from("/c/_cacache"), "_cacache"))
    );
}
