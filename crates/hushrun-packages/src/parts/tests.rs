use std::ffi::OsString;
use std::path::Path;

use crate::part::Part as _;
use crate::part::{self, Reader};
use crate::parts::{gradle::Gradle, hex::Hex, maven::Maven, nuget::Nuget};

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

// covers: K-4, K-30, K-31
#[test]
fn pip_and_poetry_name_their_manifest_sets() {
    use crate::parts::{pip::Pip, poetry::Poetry};
    let tracked = paths(&[
        "requirements.txt",
        "requirements-dev.txt",
        "ci-requirements.txt",
        "requirements/test.txt",
        "docs/requirements.in",
        "notes.txt",
        "poetry.lock",
        "tools/poetry.lock",
        "pyproject.toml",
        "uv.lock",
    ]);
    assert_eq!(
        Pip.manifests(&tracked),
        paths(&[
            "requirements.txt",
            "requirements-dev.txt",
            "ci-requirements.txt",
            "requirements/test.txt",
        ])
    );
    assert_eq!(
        Poetry.manifests(&tracked),
        paths(&["poetry.lock", "tools/poetry.lock"])
    );
}

const SIX: &str = "8abb2f1d86890a2dfb989f9a77cfcfd3e47c2a354b01111771326f8aa26e0254";
const SIX_SDIST: &str = "1e61c37477a1626458e36f7b1d82aa5c9b094fa4802892072e49de9c60c4c926";

// covers: K-3, K-5, K-30, K-32
#[test]
fn pip_keeps_the_files_a_requirements_file_hashes() {
    use crate::parts::pip::{Pip, hashes};
    let text = format!(
        "# a comment --hash=sha256:{SIX_SDIST}\nsix==1.16.0 \\\n    --hash=sha256:{SIX} \\\n    --hash sha256:{SIX_SDIST}\nidna==3.7 --hash=sha512:00\n"
    );
    assert_eq!(hashes(&text), [SIX, SIX_SDIST]);
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(checkout.path().join("requirements.txt"), &text).unwrap();
    std::fs::write(checkout.path().join("requirements-dev.txt"), "pytest\n").unwrap();
    let manifests = paths(&["requirements.txt", "requirements-dev.txt"]);
    let locked = Pip.lockfile(checkout.path(), &manifests).unwrap();
    let six = hushrun_core::blocks::Hex32::parse(SIX).unwrap();
    assert_eq!(
        locked.of("six-1.16.0-py2.py3-none-any.whl", &six),
        Some(format!("sha256:{SIX}").as_str())
    );
    // A download names the hashed file alone; a requirements file with no hashes fetches nothing.
    let Some(Ok(commands)) = Pip.download(checkout.path(), &manifests, Path::new("/d")) else {
        panic!("pip downloads")
    };
    assert_eq!(commands.len(), 1);
    assert!(commands[0].contains(&OsString::from("--require-hashes")));
    assert!(commands[0].contains(&OsString::from("requirements.txt")));
    assert!(matches!(
        Pip.download(
            checkout.path(),
            &paths(&["requirements-dev.txt"]),
            Path::new("/d")
        ),
        Some(Err(_))
    ));
    assert!(Pip.keep("six-1.16.0-py2.py3-none-any.whl"));
    assert!(Pip.keep("six-1.16.0.tar.gz"));
    assert!(!Pip.keep("sub/six-1.16.0.tar.gz"));
    assert!(!Pip.keep("notes.txt"));
    assert_eq!(
        Pip.package("six-1.16.0-py2.py3-none-any.whl").as_deref(),
        Some("six==1.16.0")
    );
    assert_eq!(
        Pip.package("python-dateutil-2.9.0.tar.gz").as_deref(),
        Some("python-dateutil==2.9.0")
    );
}

// covers: K-3, K-5, K-31
#[test]
fn poetry_keeps_the_files_its_lockfile_hashes_and_never_a_built_wheel() {
    use crate::parts::poetry::{Poetry, files};
    let lock = format!(
        "[[package]]\nname = \"six\"\nversion = \"1.16.0\"\nfiles = [\n    {{file = \"six-1.16.0-py2.py3-none-any.whl\", hash = \"sha256:{SIX}\"}},\n    {{file = \"six-1.16.0.tar.gz\", hash = \"sha256:{SIX_SDIST}\"}},\n]\n"
    );
    assert_eq!(
        files(&lock),
        [
            (
                "six-1.16.0-py2.py3-none-any.whl".to_string(),
                SIX.to_string()
            ),
            ("six-1.16.0.tar.gz".to_string(), SIX_SDIST.to_string()),
        ]
    );
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(checkout.path().join("poetry.lock"), &lock).unwrap();
    let locked = Poetry
        .lockfile(checkout.path(), &paths(&["poetry.lock"]))
        .unwrap();
    let six = hushrun_core::blocks::Hex32::parse(SIX).unwrap();
    assert!(locked.of("six-1.16.0-py2.py3-none-any.whl", &six).is_some());
    // The same bytes under another name are no file the lockfile names.
    assert!(locked.of("six-1.16.0-cp313-none-any.whl", &six).is_none());
    // A seed copies only what the lockfile names with these bytes.
    let admit = Poetry.admit(checkout.path(), &paths(&["poetry.lock"]));
    assert_eq!(
        admit(
            "aa/bb/cc/dd/six-1.16.0-py2.py3-none-any.whl",
            b"other bytes"
        ),
        crate::part::Admit::NotLocked
    );
    // The folder Poetry names by a file's URL and hash: 64 hex characters split 2/2/2/58.
    let folder = format!("11/e9/4b/{}", "a".repeat(58));
    assert!(Poetry.keep(&format!("{folder}/six-1.16.0-py2.py3-none-any.whl")));
    for left in [
        "six-1.16.0-py2.py3-none-any.whl".to_string(),
        format!("{folder}/six-1.16.0-py2.py3-none-any.whl.lock"),
        "11/e9/4b/zz/six-1.16.0.tar.gz".to_string(),
    ] {
        assert!(!Poetry.keep(&left), "{left}");
    }
}

// covers: K-10, K-30, K-31
#[test]
fn the_python_readers() {
    use crate::parts::{pip::Pip, poetry::Poetry};
    let home = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let h = home.path().to_path_buf();
    let env = move |name: &str| (name == "HOME").then(|| h.clone().into_os_string());
    let dir = Path::new("/hushrun-packages/parts/pip");
    assert_eq!(
        Pip.reader(dir, checkout.path(), &env),
        Reader::InPlace(vec![("PIP_FIND_LINKS".into(), dir.into())])
    );
    let set = |name: &str| (name == "PIP_FIND_LINKS").then(|| OsString::from("/mine"));
    assert_eq!(
        Pip.reader(dir, checkout.path(), &set),
        Reader::LeftAlone("PIP_FIND_LINKS is set already".into())
    );
    // A pip configuration file's find-links would be replaced: the part is left alone.
    std::fs::create_dir_all(home.path().join(".config/pip")).unwrap();
    std::fs::write(
        home.path().join(".config/pip/pip.conf"),
        "[global]\nfind-links = /wheels\n",
    )
    .unwrap();
    if cfg!(unix) {
        assert!(matches!(
            Pip.reader(dir, checkout.path(), &env),
            Reader::LeftAlone(why) if why.starts_with("find-links is set already")
        ));
    }
    assert!(matches!(
        Pip.reader(
            Path::new("/my packages/parts/pip"),
            checkout.path(),
            &no_env
        ),
        Reader::LeftAlone(_)
    ));
    // Poetry: seeded into its cache's artifacts/, POETRY_CACHE_DIR's or the platform's.
    let named = |name: &str| (name == "POETRY_CACHE_DIR").then(|| OsString::from("/pc"));
    assert_eq!(Poetry.reader(dir, checkout.path(), &named), Reader::Seed);
    assert_eq!(
        Poetry.seed_into(checkout.path(), &named),
        Ok(("/pc/artifacts".into(), "artifacts"))
    );
    let relative = |name: &str| (name == "POETRY_CACHE_DIR").then(|| OsString::from("pc"));
    assert!(matches!(
        Poetry.reader(dir, checkout.path(), &relative),
        Reader::LeftAlone(_)
    ));
    if cfg!(target_os = "linux") {
        assert_eq!(
            Poetry.seed_into(checkout.path(), &env),
            Ok((home.path().join(".cache/pypoetry/artifacts"), "artifacts"))
        );
    }
    // A cache-dir the project's poetry.toml names: hushrun doesn't read it, and leaves Poetry alone.
    std::fs::write(checkout.path().join("poetry.toml"), "cache-dir = \"/x\"\n").unwrap();
    assert!(matches!(
        Poetry.reader(dir, checkout.path(), &env),
        Reader::LeftAlone(why) if why.starts_with("cache-dir is set already")
    ));
    assert_eq!(
        Poetry.fetch_vars(Path::new("/bake/poetry")),
        [("POETRY_CACHE_DIR", OsString::from("/bake/poetry"))]
    );
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

/// A `mix.lock` as Mix 1.18 and Hex 2.7 write it: two Hex packages, a git dependency and a
/// package of an organization's repository.
const MIX_LOCK: &str = r#"%{
  "decimal": {:hex, :decimal, "2.3.0", "3ad6255aa77b4a3c4f818171b12d237500e63525c2fd056699967a3e7ea20f62", [:mix], [], "hexpm", "a4d66355cb29cb47c3cf30e71329e58361cfcb37c34235ef3bf1d7bf3773aeac"},
  "jason": {:hex, :jason, "1.4.4", "b9226785a9aa77b6857ca22832cffa5d5011a667207eb2a0ad56adb5db443b8a", [:mix], [{:decimal, "~> 1.0 or ~> 2.0", [hex: :decimal, repo: "hexpm", optional: true]}], "hexpm", "c5eb0cab91f094599f94d55bc63409236a8ec69a21a67814529e8d5f6cc90b3b"},
  "mine": {:git, "https://example.com/mine.git", "0123456789abcdef0123456789abcdef01234567", [branch: "main"]},
  "secret": {:hex, :secret, "1.0.0", "1111111111111111111111111111111111111111111111111111111111111111", [:mix], [], "hexpm:acme", "2222222222222222222222222222222222222222222222222222222222222222"},
}
"#;

// covers: K-3, K-4, K-5
#[test]
fn hex_keeps_tarballs_and_reads_the_outer_checksum_of_mix_lock() {
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
        locks["jason-1.4.4.tar"],
        ["c5eb0cab91f094599f94d55bc63409236a8ec69a21a67814529e8d5f6cc90b3b"]
    );
    let locked = Hex
        .lockfile(checkout.path(), &paths(&["mix.lock"]))
        .unwrap();
    let sha = hushrun_core::blocks::Hex32::parse(
        "c5eb0cab91f094599f94d55bc63409236a8ec69a21a67814529e8d5f6cc90b3b",
    )
    .unwrap();
    assert_eq!(
        locked.of("jason-1.4.4.tar", &sha),
        Some("sha256:c5eb0cab91f094599f94d55bc63409236a8ec69a21a67814529e8d5f6cc90b3b")
    );
    assert_eq!(locked.of("jason-1.4.3.tar", &sha), None);
    // The seed's verdict: a tarball the lockfile pins, by its bytes.
    let admit = Hex.admit(checkout.path(), &paths(&["mix.lock"]));
    assert_eq!(
        admit("jason-1.4.3.tar", b"x"),
        crate::part::Admit::NotLocked
    );
    assert_eq!(admit("jason-1.4.4.tar", b"x"), crate::part::Admit::Changed);
    assert!(Hex.links());
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
    assert_eq!(
        Hex.reader(Path::new("/p"), Path::new("/c"), &env(&[])),
        Reader::Seed
    );
    let into = |vars| Hex.seed_into(Path::new("/c"), &env(vars));
    let seed = |path: &str| Ok((std::path::PathBuf::from(path), "packages/hexpm"));
    assert_eq!(
        into(&[("HOME", "/home/me")]),
        seed("/home/me/.hex/packages/hexpm")
    );
    assert_eq!(
        into(&[("HOME", "/home/me"), ("HEX_HOME", "/h"), ("MIX_XDG", "1")]),
        seed("/h/packages/hexpm")
    );
    assert_eq!(
        into(&[("HOME", "/home/me"), ("MIX_XDG", "true")]),
        seed("/home/me/.cache/hex/packages/hexpm")
    );
    assert_eq!(
        into(&[
            ("HOME", "/home/me"),
            ("MIX_XDG", "1"),
            ("XDG_CACHE_HOME", "/xc")
        ]),
        seed("/xc/hex/packages/hexpm")
    );
    assert_eq!(
        into(&[
            ("HOME", "/home/me"),
            ("MIX_XDG", "yes"),
            ("XDG_CACHE_HOME", "/xc")
        ]),
        seed("/home/me/.hex/packages/hexpm"),
        "Hex reads only 1 and true"
    );
    assert_eq!(
        into(&[("HOME", "/home/me"), ("HEX_HOME", "")]),
        Err("HEX_HOME is empty, not an absolute folder".into())
    );
    assert_eq!(
        into(&[("HEX_HOME", "rel")]),
        Err("HEX_HOME is `rel`, not an absolute folder".into())
    );
    assert_eq!(into(&[]), Err("neither HEX_HOME nor HOME is set".into()));
}
