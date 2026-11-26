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

const LOCK: &str = "GEM
  remote: https://rubygems.org/
  specs:
    nokogiri (1.16.7-x86_64-linux-gnu)
      racc (~> 1.4)
    racc (1.8.1)
    rack (3.1.8)

GIT
  remote: https://example.com/x.git
  revision: abc
  specs:
    x (0.1.0)

PATH
  remote: .
  specs:
    shop (1.0.0)

PLATFORMS
  x86_64-linux

DEPENDENCIES
  nokogiri
  rack

CHECKSUMS
  bundler (4.0.18) sha256=02d9a17429de1847b4e0c9f27a9ee4b20c0a74c0a641b4e77195d6019e3618ac
  nokogiri (1.16.7-x86_64-linux-gnu) sha256=3db6795a80634def1cf86fda79d2d83b59b25ce5e186fa675f73c565589d2ad8
  racc (1.8.1)
  rack (3.1.8) sha256=d3fbcbca43dc2b43c9c6d7dfbac01667ae58643c42cea10013d0da970218a1b1
  shop (1.0.0)

BUNDLED WITH
  4.0.18
";

// covers: K-10
#[test]
fn bundler_names_its_manifest_set_and_keeps_only_downloaded_gems() {
    use crate::parts::bundler::Bundler;
    let tracked = paths(&[
        "Gemfile",
        "Gemfile.lock",
        "shop.gemspec",
        "engines/a/gems.rb",
        "engines/a/gems.locked",
        ".bundle/config",
        "lib/shop.rb",
    ]);
    assert_eq!(
        Bundler.manifests(&tracked),
        paths(&[
            "Gemfile",
            "Gemfile.lock",
            "shop.gemspec",
            "engines/a/gems.rb",
            "engines/a/gems.locked",
        ])
    );
    assert_eq!(part::by_name("bundler").map(|p| p.tool()), Some("Bundler"));
    assert!(Bundler.keep("rack-3.1.8.gem"));
    assert!(Bundler.keep("nokogiri-1.16.7-x86_64-linux-gnu.gem"));
    for left in [
        "bundler/git/x-abc/HEAD",
        ".rack-3.1.8.gem.123.hushrun-seed",
        "notes.txt",
    ] {
        assert!(!Bundler.keep(left), "{left}");
    }
    assert_eq!(
        Bundler.package("concurrent-ruby-1.3.4.gem").as_deref(),
        Some("concurrent-ruby (1.3.4)")
    );
    assert_eq!(
        Bundler
            .package("nokogiri-1.16.7-x86_64-linux-gnu.gem")
            .as_deref(),
        Some("nokogiri (1.16.7-x86_64-linux-gnu)")
    );
    assert_eq!(Bundler.package("README"), None);
    // A gem home holds `cache/`; a BUNDLE_PATH holds `ruby/<version>/cache/`.
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("bundle/ruby/3.3.0/cache")).unwrap();
    std::fs::create_dir_all(t.path().join("home/cache")).unwrap();
    assert_eq!(
        Bundler.harvest_root(&t.path().join("bundle")),
        Some((t.path().join("bundle/ruby/3.3.0/cache"), "cache"))
    );
    assert_eq!(
        Bundler.harvest_root(&t.path().join("home")),
        Some((t.path().join("home/cache"), "cache"))
    );
    assert_eq!(Bundler.harvest_root(t.path()), None);
}

// covers: K-10
#[test]
fn bundler_is_verified_by_its_lockfile_only_where_it_has_checksums() {
    use crate::parts::bundler::{Bundler, gems, seed_files};
    use crate::store::VerifiedBy;
    let with = ("Gemfile.lock".to_string(), LOCK.to_string());
    let without = (
        "engines/a/Gemfile.lock".to_string(),
        LOCK.split("CHECKSUMS").next().unwrap().to_string(),
    );
    let gemfile = ("Gemfile".to_string(), "gem 'rack'\n".to_string());
    assert_eq!(
        Bundler.verified_by_for(&[gemfile.clone(), with.clone()]),
        VerifiedBy::Lockfile
    );
    assert_eq!(
        Bundler.verified_by_for(&[with.clone(), without.clone()]),
        VerifiedBy::Signature
    );
    assert_eq!(
        Bundler.verified_by_for(std::slice::from_ref(&gemfile)),
        VerifiedBy::Signature
    );
    // GEM's specs only, never GIT's or PATH's; each with CHECKSUMS' hash where it has one.
    let named: Vec<(String, bool)> = gems(LOCK)
        .into_iter()
        .map(|g| (g.file, g.locked.is_some()))
        .collect();
    assert_eq!(
        named,
        [
            ("nokogiri-1.16.7-x86_64-linux-gnu.gem".to_string(), true),
            ("racc-1.8.1.gem".to_string(), false),
            ("rack-3.1.8.gem".to_string(), true),
        ]
    );
    let locked = Bundler.locked(std::slice::from_ref(&with));
    assert_eq!(
        locked.get("rack-3.1.8.gem").map(String::as_str),
        Some("sha256=d3fbcbca43dc2b43c9c6d7dfbac01667ae58643c42cea10013d0da970218a1b1")
    );
    assert_eq!(
        locked.len(),
        2,
        "bundler itself is no spec of GEM: {locked:?}"
    );
    // Two lockfiles that hash one file differently give it no hash.
    let other = (
        "b/Gemfile.lock".to_string(),
        LOCK.replace("sha256=d3fb", "sha256=0000"),
    );
    assert!(
        !Bundler
            .locked(&[with, other])
            .contains_key("rack-3.1.8.gem")
    );
    let files = seed_files(LOCK);
    assert_eq!(files[2].path, "cache/rack-3.1.8.gem");
    assert_eq!(files[2].name, "rack-3.1.8.gem");
    assert!(files[2].sha256.is_some() && files[1].sha256.is_none());
}

// covers: K-10
#[test]
fn bundler_seeds_from_the_gemfile_bundler_reads() {
    use crate::parts::bundler::{Bundler, lockfile_for};
    let t = tempfile::tempdir().unwrap();
    let checkout = t.path();
    std::fs::create_dir_all(checkout.join("app/lib")).unwrap();
    std::fs::write(checkout.join("Gemfile"), "").unwrap();
    std::fs::write(checkout.join("app/gems.rb"), "").unwrap();
    assert_eq!(
        lockfile_for(&checkout.join("app/lib"), checkout, &no_env),
        Some((checkout.join("app"), checkout.join("app/gems.locked")))
    );
    assert_eq!(
        lockfile_for(checkout, checkout, &no_env),
        Some((checkout.to_path_buf(), checkout.join("Gemfile.lock")))
    );
    let named = |name: &str| (name == "BUNDLE_GEMFILE").then(|| OsString::from("ci/Gemfile"));
    assert_eq!(
        lockfile_for(checkout, checkout, &named),
        Some((checkout.join("ci"), checkout.join("ci/Gemfile.lock")))
    );
    // No Gemfile up to the checkout's root: left alone, and no ruby is asked.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(lockfile_for(empty.path(), empty.path(), &no_env), None);
    assert!(matches!(
        Bundler.reader(Path::new("/p"), empty.path(), &no_env),
        Reader::LeftAlone(_)
    ));
}

// covers: K-6
#[test]
fn bundler_fetches_by_itself_and_installs_nothing() {
    use crate::parts::bundler::Bundler;
    let checkout = Path::new("/c");
    let cache = Path::new("/bake/bundler");
    let fetches = Bundler.fetch(
        checkout,
        &paths(&[
            "Gemfile",
            "Gemfile.lock",
            "a/gems.rb",
            "a/gems.locked",
            "b/Gemfile",
        ]),
        cache,
    );
    assert_eq!(fetches.len(), 2, "one per lockfile: {fetches:?}");
    assert_eq!(fetches[0].dir, Path::new("/c"));
    assert_eq!(fetches[0].argv, ["bundle", "cache", "--no-install"]);
    let env = |i: usize, name: &str| {
        fetches[i]
            .env
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(env(1, "BUNDLE_GEMFILE"), Some("/c/a/gems.rb".into()));
    assert_eq!(env(0, "BUNDLE_PATH"), Some("/bake/bundler".into()));
    assert_eq!(
        env(0, "BUNDLE_CACHE_PATH"),
        Some("/bake/bundler/.app-0".into())
    );
    assert_eq!(env(0, "BUNDLE_FROZEN"), Some("true".into()));
    assert_eq!(env(0, "BUNDLE_GLOBAL_GEM_CACHE"), Some("false".into()));
    assert!(
        crate::parts::gradle::Gradle
            .fetch(checkout, &paths(&["build.gradle"]), cache)
            .is_empty()
    );
}
