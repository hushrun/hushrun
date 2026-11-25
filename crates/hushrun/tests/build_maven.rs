//! Maven's modules as build blocks (B-170 to B-177), end to end: `hushrun run NAME -- mvn …` on
//! two machines (two checkouts, two stores, two local repositories and so two local build caches
//! of the extension) against the fake build protocol, with the real Maven and Apache's build cache
//! extension.
//!
//! They need Maven 3.9 or later (`HUSHRUN_TEST_MAVEN`, the path of its `mvn`), a JDK, and a local
//! repository that holds the extension and the plugins the fixture pins
//! (`HUSHRUN_TEST_MAVEN_REPO`), as the Maven adapter's tests do: Maven runs offline, reading that
//! repository as a tail below one of each machine's own. With `HUSHRUN_TEST_MAVEN_ONLINE=1` it
//! runs online and fills that repository instead, which is how CI fills it before its test run.
//! The tests say so and pass where Maven is missing, unless `HUSHRUN_TEST_TOOLS=required`, which
//! CI sets. (`-o` leaves the extension's remote on: it isn't Maven's repository transport.)

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::fake_build::FakeBuild;
use hushrun_testkit::git;

const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_LOG",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_BUILD_SHARE",
    "HUSHRUN_BUILD_AUDIT",
    "HUSHRUN_UNITS",
    "RUSTC_WRAPPER",
    "MAVEN_OPTS",
    "MAVEN_ARGS",
    "MAVEN_CONFIG",
];

/// Maven and its repository, when the tests have them; else the test passes with a note, unless
/// `HUSHRUN_TEST_TOOLS=required`.
fn maven() -> Option<(String, PathBuf)> {
    let mvn = std::env::var("HUSHRUN_TEST_MAVEN")
        .ok()
        .filter(|m| !m.is_empty());
    let repo = std::env::var("HUSHRUN_TEST_MAVEN_REPO")
        .ok()
        .filter(|r| !r.is_empty());
    let found = mvn.zip(repo).filter(|(mvn, _)| {
        Command::new(mvn)
            .arg("--version")
            .env_remove("MAVEN_OPTS")
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if found.is_none() {
        if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
            panic!(
                "Maven (HUSHRUN_TEST_MAVEN and HUSHRUN_TEST_MAVEN_REPO) is missing, and HUSHRUN_TEST_TOOLS=required"
            );
        }
        eprintln!(
            "note: skipping: Maven (HUSHRUN_TEST_MAVEN and HUSHRUN_TEST_MAVEN_REPO) is missing (HUSHRUN_TEST_TOOLS=required makes this fail)"
        );
    }
    found.map(|(mvn, repo)| (mvn, PathBuf::from(repo)))
}

fn online() -> bool {
    std::env::var("HUSHRUN_TEST_MAVEN_ONLINE").as_deref() == Ok("1")
}

const PARENT: &str = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>demo</groupId>
  <artifactId>shop</artifactId>
  <version>1.0</version>
  <packaging>pom</packaging>
  <properties>
    <maven.compiler.release>17</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  <modules>
    <module>core</module>
    <module>app</module>
  </modules>
  <build>
    <pluginManagement>
      <plugins>
        <plugin><artifactId>maven-compiler-plugin</artifactId><version>3.13.0</version></plugin>
        <plugin><artifactId>maven-resources-plugin</artifactId><version>3.3.1</version></plugin>
        <plugin><artifactId>maven-jar-plugin</artifactId><version>3.4.1</version></plugin>
        <plugin><artifactId>maven-surefire-plugin</artifactId><version>3.5.2</version></plugin>
      </plugins>
    </pluginManagement>
  </build>
</project>
"#;

const EXTENSIONS: &str = r#"<extensions>
  <extension>
    <groupId>org.apache.maven.extensions</groupId>
    <artifactId>maven-build-cache-extension</artifactId>
    <version>1.2.0</version>
  </extension>
</extensions>
"#;

/// A reactor of two modules, `app` depending on `core`, loading the extension when `extension`,
/// committed in a repository.
fn project(root: &Path, extension: bool) -> PathBuf {
    let repo = root.join("repo");
    let write = |path: &str, text: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("pom.xml", PARENT);
    if extension {
        write(".mvn/extensions.xml", EXTENSIONS);
    }
    write(".gitignore", "target/\n");
    write(
        "core/pom.xml",
        r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>demo</groupId><artifactId>shop</artifactId><version>1.0</version></parent>
  <artifactId>core</artifactId>
</project>
"#,
    );
    write(
        "app/pom.xml",
        r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>demo</groupId><artifactId>shop</artifactId><version>1.0</version></parent>
  <artifactId>app</artifactId>
  <dependencies>
    <dependency><groupId>demo</groupId><artifactId>core</artifactId><version>1.0</version></dependency>
  </dependencies>
</project>
"#,
    );
    write(
        "core/src/main/java/shop/Price.java",
        "package shop;\n\npublic class Price {\n    public static int of(int n) { return n * 7; }\n}\n",
    );
    write(
        "app/src/main/java/shop/Cart.java",
        "package shop;\n\npublic class Cart {\n    public static int total() { return Price.of(2) * 3; }\n}\n",
    );
    git::init(&repo, "sha1");
    git::commit_all(&repo, "init");
    repo
}

/// A machine: its own checkout, hushrun cache (and so store), home, local repository (and so the
/// extension's local cache, beside it) and machine key.
struct Machine {
    root: PathBuf,
    repo: PathBuf,
    mvn: String,
    m2: PathBuf,
}

impl Machine {
    fn new(dir: &Path, name: &str, from: Option<&Path>, extension: bool) -> Option<Machine> {
        let (mvn, m2) = maven()?;
        let root = dir.join(name);
        std::fs::create_dir_all(root.join("home")).unwrap();
        let repo = match from {
            None => project(&root, extension),
            Some(from) => {
                let repo = root.join("checkout");
                git::run(
                    &root,
                    &[
                        "clone",
                        "-q",
                        "--no-hardlinks",
                        from.to_str().unwrap(),
                        repo.to_str().unwrap(),
                    ],
                    None,
                );
                repo
            }
        };
        std::fs::write(
            root.join("machine.pem"),
            hushrun_testkit::world::test_key(7).to_pem().as_str(),
        )
        .unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        Some(Machine {
            root,
            repo,
            mvn,
            m2,
        })
    }

    fn hushrun(&self, fake: &FakeBuild) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.current_dir(&self.repo)
            .env("HOME", self.root.join("home"))
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_MACHINE_KEY_FILE", self.root.join("machine.pem"))
            .env("HUSHRUN_HOST", "http://127.0.0.1:9")
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HUSHRUN_BACKEND", fake.url())
            .env("HUSHRUN_TOKEN", FakeBuild::TOKEN)
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_BUILD_AUDIT", "none")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"));
        cmd
    }

    /// `hushrun run build -- mvn -B ARGS`: offline, this machine's own local repository with the
    /// shared one below it.
    fn run(&self, fake: &FakeBuild, args: &[&str]) -> (i32, String, String) {
        let mut cmd = self.hushrun(fake);
        cmd.args(["run", "build", "--force", "--", &self.mvn, "-B"]);
        if online() {
            cmd.arg(format!("-Dmaven.repo.local={}", self.m2.display()))
                // The extension's local cache, which is beside the local repository otherwise.
                .arg(format!(
                    "-Dmaven.build.cache.location={}",
                    self.root.join("build-cache").display()
                ));
        } else {
            cmd.arg("-o")
                .arg(format!(
                    "-Dmaven.repo.local={}",
                    self.root.join("m2").display()
                ))
                .arg(format!("-Dmaven.repo.local.tail={}", self.m2.display()));
        }
        let output = cmd.args(args).output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

/// The Maven line of a run's summary.
fn maven_line(stderr: &str) -> String {
    stderr
        .lines()
        .find(|l| l.contains("build: maven: "))
        .unwrap_or_default()
        .to_string()
}

/// The `maven/1` blocks the fake holds.
fn maven_blocks(fake: &FakeBuild) -> usize {
    fake.begins()
        .iter()
        .filter(|b| b.kind == "maven/1")
        .map(|b| b.blocks.len())
        .sum()
}

// covers: B-170, B-174, B-175, B-176, B-177
#[test]
fn a_module_one_machine_built_comes_from_the_shared_store_on_another() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let Some(a) = Machine::new(dir.path(), "a", None, true) else {
        return;
    };
    let (code, out, err) = a.run(&fake, &["package"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("BUILD SUCCESS"), "{out}");
    assert!(
        maven_line(&err).contains("2 modules: 2 built and stored"),
        "{out}\n{err}"
    );
    assert_eq!(maven_blocks(&fake), 2, "{err}");
    let open = fake.opens().into_iter().find(|o| o.kind == "maven/1");
    assert!(
        open.is_some_and(|o| o.platform == "jvm"),
        "{:?}",
        fake.opens()
    );
    let close = fake
        .closes()
        .into_iter()
        .find(|c| c.kind == "maven/1")
        .expect("a Maven close");
    assert_eq!(close.manifest.len(), 2);

    // B: another checkout at another path, another store, another local repository and so
    // another local build cache: both modules come from what A built.
    let b = Machine::new(dir.path(), "b", Some(&a.repo), true).unwrap();
    let (code, out, err) = b.run(&fake, &["package"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        out.contains("Found cached build, restoring demo:core from cache"),
        "{out}\n{err}"
    );
    assert!(
        out.contains("Found cached build, restoring demo:app from cache"),
        "{out}\n{err}"
    );
    let line = maven_line(&err);
    assert!(
        line.contains("2 modules: 2 from hushrun's cache (2 from the shared store)"),
        "{err}"
    );
    assert!(b.repo.join("app/target/app-1.0.jar").is_file());

    // A source changes on B: core is built again, and app, whose checksum holds core's.
    std::fs::write(
        b.repo.join("core/src/main/java/shop/Price.java"),
        "package shop;\n\npublic class Price {\n    public static int of(int n) { return n * 8; }\n}\n",
    )
    .unwrap();
    let (code, out, err) = b.run(&fake, &["package"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        maven_line(&err).contains("2 modules: 2 built and stored"),
        "{out}\n{err}"
    );
}

// covers: B-170
#[test]
fn a_build_without_the_extension_is_said_and_shares_nothing() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let Some(a) = Machine::new(dir.path(), "a", None, false) else {
        return;
    };
    let (code, out, err) = a.run(&fake, &["package"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        err.contains("build: Maven's modules aren't shared in this run: the build doesn't load Apache's build cache extension"),
        "{err}"
    );
    assert_eq!(maven_blocks(&fake), 0);
    assert!(!fake.opens().iter().any(|o| o.kind == "maven/1"));
}

// covers: B-170
#[test]
fn tests_hushrun_observes_run_where_maven_builds_and_not_where_the_extension_restores() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let Some(a) = Machine::new(dir.path(), "a", None, true) else {
        return;
    };
    let write = |path: &str, text: &str| {
        let path = a.repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "hushrun.yaml",
        "version: 1\nrepository: example.com/acme/shop\nchecks:\n  build:\n    tests: reuse\n",
    );
    write(
        "app/pom.xml",
        r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>demo</groupId><artifactId>shop</artifactId><version>1.0</version></parent>
  <artifactId>app</artifactId>
  <dependencies>
    <dependency><groupId>demo</groupId><artifactId>core</artifactId><version>1.0</version></dependency>
    <dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.13.2</version><scope>test</scope></dependency>
  </dependencies>
</project>
"#,
    );
    write(
        "app/src/test/java/shop/CartTest.java",
        "package shop;\n\npublic class CartTest {\n    @org.junit.Test public void totals() { org.junit.Assert.assertEquals(42, Cart.total()); }\n}\n",
    );
    git::commit_all(&a.repo, "tests");
    // A: the Maven adapter's MAVEN_OPTS carries the cache's properties too: the test class runs
    // and is observed, and both modules go up.
    let (code, out, err) = a.run(&fake, &["verify"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("Tests run: 1, Failures: 0"), "{out}");
    assert!(
        err.contains("build passed in") && err.contains("1 test;"),
        "{err}"
    );
    assert!(
        maven_line(&err).contains("2 modules: 2 built and stored"),
        "{out}\n{err}"
    );
    // B: the extension restores app and skips its tests, as it does; hushrun counts no test.
    let b = Machine::new(dir.path(), "b", Some(&a.repo), true).unwrap();
    let (code, out, err) = b.run(&fake, &["verify"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        out.contains("Found cached build, restoring demo:app from cache"),
        "{out}"
    );
    assert!(!out.contains("Tests run:"), "{out}");
    assert!(
        maven_line(&err).contains("2 from hushrun's cache (2 from the shared store)"),
        "{err}"
    );
    assert!(!err.contains("1 test"), "{err}");
}

// covers: B-175
#[test]
fn a_build_that_skips_tests_shares_none_of_its_modules() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let Some(a) = Machine::new(dir.path(), "a", None, true) else {
        return;
    };
    let (code, out, err) = a.run(&fake, &["-DskipTests", "package"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        err.contains(
            "build: maven: the command sets skipTests, so the modules this run builds aren't shared"
        ),
        "{err}"
    );
    assert_eq!(maven_blocks(&fake), 0, "{err}");
}
