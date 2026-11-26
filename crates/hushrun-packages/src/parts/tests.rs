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

// covers: K-10, K-45
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

// covers: K-10, K-45
#[test]
fn bundler_is_verified_by_its_lockfile_only_where_it_has_checksums() {
    use crate::parts::bundler::{Bundler, gems};
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
    let locked = Bundler.locked_in(std::slice::from_ref(&with));
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
            .locked_in(&[with, other])
            .contains_key("rack-3.1.8.gem")
    );
    // What a seed admits: a gem the lockfile hashes, by those bytes; one it names with no hash,
    // only for a part a signature verifies; one it doesn't name, never.
    use crate::part::Admit;
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("Gemfile.lock"), LOCK).unwrap();
    let manifests = paths(&["Gemfile", "Gemfile.lock"]);
    let strict = Bundler.admit(t.path(), &manifests);
    let signed = Bundler.admit_for(t.path(), &manifests, VerifiedBy::Signature);
    assert_eq!(
        strict("rack-3.1.8.gem", b"x"),
        Admit::Changed,
        "other bytes than CHECKSUMS hash"
    );
    assert_eq!(
        strict("racc-1.8.1.gem", b"x"),
        Admit::NotLocked,
        "no hash, a lockfile part"
    );
    assert_eq!(
        signed("racc-1.8.1.gem", b"x"),
        Admit::Copy,
        "no hash, a signed part"
    );
    assert_eq!(
        signed("x-0.1.0.gem", b"x"),
        Admit::NotLocked,
        "a git source's gem"
    );
    assert_eq!(strict("bundler-4.0.18.gem", b"x"), Admit::NotLocked);
}

// covers: K-10, K-45
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
    assert_eq!(
        Bundler.reader(Path::new("/p"), empty.path(), &no_env),
        Reader::Seed
    );
}

// covers: K-6, K-45
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
