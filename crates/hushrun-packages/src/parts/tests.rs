use std::ffi::OsString;
use std::path::Path;

use crate::part::Locks;
use crate::part::Part as _;
use crate::part::{self, Reader};
use crate::parts::{cargo::Cargo, go::Go, gradle::Gradle, maven::Maven, nuget::Nuget};

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
            .locked_by("index.crates.io-x/itoa-1.0.11.crate", &locks)
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
        Go.locked_by(&format!("{at}.zip"), &locks).as_deref(),
        Some("h1:zip=")
    );
    assert_eq!(
        Go.locked_by(&format!("{at}.mod"), &locks).as_deref(),
        Some("h1:mod=")
    );
    assert_eq!(Go.locked_by(&format!("{at}.info"), &locks), None);
}

// covers: K-10
#[test]
fn cargo_s_seed_admits_only_the_bytes_cargo_lock_pins_and_runs_in_a_run() {
    use crate::part::Admit;
    let checkout = tempfile::tempdir().unwrap();
    let sha = |b: &[u8]| hushrun_core::blocks::Hex32::sha256(b).to_string();
    let entry = |name: &str, checksum: &str| {
        format!(
            "[[package]]\nname = \"{name}\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{checksum}\"\n\n"
        )
    };
    // a is pinned to its bytes, b to other bytes, c not at all.
    std::fs::write(
        checkout.path().join("Cargo.lock"),
        format!(
            "version = 4\n\n{}{}",
            entry("a", &sha(b"a")),
            entry("b", &sha(b"b"))
        ),
    )
    .unwrap();
    let admit = Cargo.admit(checkout.path(), &paths(&["Cargo.lock"]));
    assert_eq!(admit("index.crates.io-x/a-1.0.0.crate", b"a"), Admit::Copy);
    assert_eq!(
        admit("index.crates.io-x/b-1.0.0.crate", b"b, changed"),
        Admit::Changed
    );
    assert_eq!(
        admit("index.crates.io-x/c-1.0.0.crate", b"c"),
        Admit::NotLocked
    );
    // Into the run's cargo home, at the same paths below `registry/cache`.
    let env = |name: &str| match name {
        "HOME" => Some(OsString::from("/home/dev")),
        _ => None,
    };
    assert_eq!(
        Cargo.seed_into(Path::new("/c"), &env),
        Ok(("/home/dev/.cargo/registry/cache".into(), "registry/cache"))
    );
    let env = |name: &str| (name == "CARGO_HOME").then(|| OsString::from("/opt/cargo"));
    assert_eq!(
        Cargo.seed_into(Path::new("/c"), &env),
        Ok(("/opt/cargo/registry/cache".into(), "registry/cache"))
    );
    assert_eq!(
        Cargo.reader(Path::new("/p"), Path::new("/c"), &env),
        Reader::Seed
    );
    assert!(matches!(
        Cargo.reader(Path::new("/p"), Path::new("/c"), &no_env),
        Reader::LeftAlone(_)
    ));
    // Cargo fetches inside the check's own build: a run seeds it; npm installs before one.
    assert!(Cargo.seeds_in_a_run());
    assert!(!crate::parts::npm::Npm.seeds_in_a_run());
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
    // The lockfile's hashes, as it writes them, by the bytes they are of.
    let lock_dir = tempfile::tempdir().unwrap();
    let rack = hushrun_core::blocks::Hex32::parse(
        "d3fbcbca43dc2b43c9c6d7dfbac01667ae58643c42cea10013d0da970218a1b1",
    )
    .unwrap();
    std::fs::write(lock_dir.path().join("Gemfile.lock"), LOCK).unwrap();
    let one = paths(&["Gemfile.lock"]);
    let locked = Bundler.lockfile(lock_dir.path(), &one).expect("CHECKSUMS");
    assert_eq!(
        locked.of("rack-3.1.8.gem", &rack),
        Some("sha256=d3fbcbca43dc2b43c9c6d7dfbac01667ae58643c42cea10013d0da970218a1b1")
    );
    assert_eq!(
        locked.of("other-1.0.gem", &rack),
        None,
        "another file's name"
    );
    // Two lockfiles that hash one file differently give it no hash.
    std::fs::create_dir_all(lock_dir.path().join("b")).unwrap();
    std::fs::write(
        lock_dir.path().join("b/Gemfile.lock"),
        LOCK.replace("sha256=d3fb", "sha256=0000"),
    )
    .unwrap();
    let both = paths(&["Gemfile.lock", "b/Gemfile.lock"]);
    let locked = Bundler.lockfile(lock_dir.path(), &both).expect("CHECKSUMS");
    assert_eq!(locked.of("rack-3.1.8.gem", &rack), None);
    // A lockfile with no CHECKSUMS: the part is no lockfile's.
    std::fs::write(lock_dir.path().join("b/Gemfile.lock"), &without.1).unwrap();
    assert!(Bundler.lockfile(lock_dir.path(), &both).is_none());
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
    let into = Path::new("/bake/bundler");
    let manifests = paths(&[
        "Gemfile",
        "Gemfile.lock",
        "a/gems.rb",
        "a/gems.locked",
        "b/Gemfile",
    ]);
    // The commands name `bundle` from PATH; where there is none, the part says so.
    let commands = match Bundler.download(checkout, &manifests, into) {
        Some(Ok(commands)) => commands,
        Some(Err(why)) => {
            assert_eq!(why, "there's no bundle here to fetch with");
            return;
        }
        None => panic!("a lockfile is there"),
    };
    assert_eq!(commands.len(), 2, "one per lockfile: {commands:?}");
    let words = |c: &[OsString]| {
        c.iter()
            .map(|w| w.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        words(&commands[1]),
        [
            "bundle",
            "cache",
            "--no-install",
            "--gemfile",
            "/c/a/gems.rb",
            "--cache-path",
            "/bake/bundler/cache"
        ]
    );
    assert_eq!(
        Bundler.fetch_vars(into),
        [
            ("BUNDLE_PATH", OsString::from("/bake/bundler/.path")),
            ("BUNDLE_GLOBAL_GEM_CACHE", OsString::from("false")),
            ("BUNDLE_FROZEN", OsString::from("true")),
        ]
    );
    assert!(
        Bundler
            .download(checkout, &paths(&["Gemfile", "x.gemspec"]), into)
            .is_none()
    );
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
