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

// covers: K-4
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

// covers: K-3, K-5
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
    let locked = Pip.locked(checkout.path(), &manifests);
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

// covers: K-3, K-5
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
    let locked = Poetry.locked(checkout.path(), &paths(&["poetry.lock"]));
    let six = hushrun_core::blocks::Hex32::parse(SIX).unwrap();
    assert!(locked.of("six-1.16.0-py2.py3-none-any.whl", &six).is_some());
    // The same bytes under another name are no file the lockfile names.
    assert!(locked.of("six-1.16.0-cp313-none-any.whl", &six).is_none());
    let key = "11/e9/4b/1f155708335621cfc5e1dc8f2ab6eaf871488879d097ee3298480667f5";
    assert!(Poetry.keep(&format!("{key}/six-1.16.0-py2.py3-none-any.whl")));
    for left in [
        "six-1.16.0-py2.py3-none-any.whl".to_string(),
        format!("{key}/six-1.16.0-py2.py3-none-any.whl.lock"),
        "11/e9/4b/zz/six-1.16.0.tar.gz".to_string(),
    ] {
        assert!(!Poetry.keep(&left), "{left}");
    }
}

// covers: K-10
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
    // Poetry: seeded into its cache, POETRY_CACHE_DIR's or the platform's.
    let named = |name: &str| (name == "POETRY_CACHE_DIR").then(|| OsString::from("/pc"));
    assert_eq!(
        Poetry.reader(dir, checkout.path(), &named),
        Reader::Seed("/pc".into())
    );
    let relative = |name: &str| (name == "POETRY_CACHE_DIR").then(|| OsString::from("pc"));
    assert!(matches!(
        Poetry.reader(dir, checkout.path(), &relative),
        Reader::LeftAlone(_)
    ));
    if cfg!(target_os = "linux") {
        assert_eq!(
            Poetry.reader(dir, checkout.path(), &env),
            Reader::Seed(home.path().join(".cache/pypoetry"))
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
