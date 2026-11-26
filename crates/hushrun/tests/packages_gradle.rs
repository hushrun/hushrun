//! `hushrun packages` with Gradle (K-6, K-10): a store harvested from a filled Gradle user home is
//! read by a build offline with an empty one, through `GRADLE_RO_DEP_CACHE`. Needs Gradle
//! (`HUSHRUN_TEST_GRADLE`) and the tests' user home (`HUSHRUN_TEST_GRADLE_USER_HOME`).

#[path = "common/cli.rs"]
mod cli;
#[path = "common/gradle.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use fixtures::{NEEDS, file, gradle, gradle_user_home, has_gradle, tools};
use repo::Repo;

use cli::Cli as _;

// covers: K-6, K-10
#[test]
fn a_store_harvested_from_a_user_home_is_read_offline_by_an_empty_one() {
    if !tools(NEEDS, has_gradle()) {
        return;
    }
    let home = std::path::PathBuf::from(gradle_user_home().unwrap());
    let r = Repo::new(&[
        file("settings.gradle", "rootProject.name = 'shop'\n"),
        file(
            "build.gradle",
            "plugins { id 'java' }\nrepositories { mavenCentral() }\ndependencies { testImplementation 'junit:junit:4.13.2' }\n",
        ),
        file(
            "src/test/java/shop/CalcTest.java",
            "package shop;\nimport org.junit.Test;\nimport static org.junit.Assert.*;\npublic class CalcTest { @Test public void adds() { assertEquals(3, 1 + 2); } }\n",
        ),
    ])
    .clear(&["GRADLE_OPTS", "JAVA_TOOL_OPTIONS", "GRADLE_RO_DEP_CACHE", "HUSHRUN_PACKAGES_LOWER", "HUSHRUN_PACKAGES"]);
    let build = |user_home: &std::path::Path, store: Option<&std::path::Path>| {
        let _ = std::fs::remove_dir_all(r.repo.join("build"));
        let mut cmd = r.hushrun();
        cmd.args([
            "packages",
            "exec",
            "--",
            &gradle(),
            "-q",
            "--no-daemon",
            "--offline",
            "test",
        ])
        .env("GRADLE_USER_HOME", user_home)
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
        .env(
            "HUSHRUN_PACKAGES_LOWER",
            store.map(|s| s.as_os_str()).unwrap_or_default(),
        );
        cmd.output().unwrap()
    };
    // The control: offline from the tests' user home itself.
    if !build(&home, None).status.success() {
        eprintln!("note: skipping: the tests' Gradle user home lacks this fixture's dependencies");
        return;
    }
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--out"])
        .arg(&store)
        .arg("--harvest")
        .arg(format!("gradle={}", home.display()))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&baked.stdout);
    assert!(
        baked.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    assert!(said.contains("packages: gradle: "), "{said}");
    assert!(said.contains("(build.gradle, settings.gradle)"), "{said}");
    // The wrapper's distribution and the user home's other caches aren't the part's.
    assert!(store.join("parts/gradle/modules-2").is_dir());
    let empty = r.root.join("empty-home");
    let read = build(&empty, Some(&store));
    assert!(
        read.status.success(),
        "{}{}",
        String::from_utf8_lossy(&read.stdout),
        String::from_utf8_lossy(&read.stderr)
    );
    let copied = all_files(&empty.join("caches/modules-2/files-2.1"));
    assert!(
        copied.is_empty(),
        "read in place, nothing copied: {copied:?}"
    );
    // Without the store the same offline build fails.
    let _ = std::fs::remove_dir_all(&empty);
    assert!(!build(&empty, None).status.success());
}

/// Every file under `dir`.
fn all_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            } else {
                out.push(entry.path());
            }
        }
    }
    out
}
