//! `hushrun packages` with Maven (K-6, K-10): a store baked from a filled local repository is read
//! by a build offline with an empty one, as a read-only tail in `MAVEN_OPTS`. Needs Maven
//! (`HUSHRUN_TEST_MAVEN`) and the tests' filled repository (`HUSHRUN_TEST_MAVEN_REPO`); with
//! `HUSHRUN_TEST_MAVEN_ONLINE=1` the fixture's build first fills it.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/maven.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use fixtures::{file, maven, module_pom, online, parent};
use repo::Repo;

use cli::Cli as _;

fn fixture() -> Repo {
    Repo::new(&[
        file(".gitignore", "target/\n"),
        file("pom.xml", &parent(&["core"], "4", "3.5.4")),
        file("core/pom.xml", &module_pom("core", &[])),
        file(
            "core/src/test/java/shop/core/CalcTest.java",
            "package shop.core;\n\nimport org.junit.Test;\nimport static org.junit.Assert.*;\n\npublic class CalcTest {\n  @Test public void adds() { assertEquals(3, 1 + 2); }\n}\n",
        ),
    ])
    .clear(&["MAVEN_OPTS", "MAVEN_ARGS", "JAVA_TOOL_OPTIONS", "HUSHRUN_PACKAGES_LOWER", "HUSHRUN_PACKAGES"])
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

// covers: K-6, K-10
#[test]
fn a_store_harvested_from_a_filled_repository_is_read_offline_through_maven_opts() {
    let Some(m) = maven() else { return };
    let r = fixture();
    if online() {
        let out = r
            .command(&m.mvn)
            .args(["-B", "-q", "test"])
            .arg(format!("-Dmaven.repo.local={}", m.m2.display()))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    // The control: the build passes offline from the tests' repository itself.
    let control = r
        .command(&m.mvn)
        .args(["-B", "-q", "-o", "test"])
        .arg(format!("-Dmaven.repo.local={}", m.m2.display()))
        .output()
        .unwrap();
    if !control.status.success() {
        eprintln!(
            "note: skipping: the tests' Maven repository lacks this fixture's plugins (HUSHRUN_TEST_MAVEN_ONLINE=1 fills it)"
        );
        return;
    }
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--out"])
        .arg(&store)
        .arg("--harvest")
        .arg(format!("maven={}", m.m2.display()))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&baked.stdout);
    assert!(
        baked.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    assert!(said.contains("packages: maven: "), "{said}");
    assert!(said.contains("(core/pom.xml, pom.xml)"), "{said}");
    assert!(store.join("packages.json").is_file());
    assert!(!r.root.join(".packages.bake.lock").exists() || store.is_dir());
    // Offline, an empty repository and the store: it passes, and the empty one gains no package.
    let empty = r.root.join("empty-m2");
    let exec = |with_store: bool| {
        let mut cmd = r.hushrun();
        cmd.args(["packages", "exec", "--", &m.mvn, "-B", "-q", "-o", "test"])
            .arg(format!("-Dmaven.repo.local={}", empty.display()))
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1");
        if with_store {
            cmd.env("HUSHRUN_PACKAGES_LOWER", &store);
        } else {
            cmd.env("HUSHRUN_PACKAGES_LOWER", "");
        }
        cmd.output().unwrap()
    };
    let read = exec(true);
    assert!(
        read.status.success(),
        "{}{}",
        String::from_utf8_lossy(&read.stdout),
        String::from_utf8_lossy(&read.stderr)
    );
    assert!(
        all_files(&empty)
            .iter()
            .all(|f| !f.to_string_lossy().contains("/junit/")),
        "junit came from the store"
    );
    // covers: K-10
    // In `hushrun run` too; and the check's key is the same with and without the store: the
    // run without it skips.
    let run = |store_var: &std::path::Path| {
        let mut cmd = r.hushrun();
        cmd.args(["run", "t", "--", &m.mvn, "-B", "-q", "-o", "test"])
            .arg(format!("-Dmaven.repo.local={}", empty.display()))
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
            .env("HUSHRUN_PACKAGES_LOWER", store_var);
        cmd.output().unwrap()
    };
    let first = run(&store);
    let said = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(first.status.success(), "{said}");
    assert!(said.contains("t passed"), "{said}");
    let second = run(std::path::Path::new(""));
    let said = String::from_utf8_lossy(&second.stderr).into_owned();
    assert!(said.contains("t skipped"), "{said}");
    // Without the store the same offline build fails: the store is what it read.
    let _ = std::fs::remove_dir_all(&empty);
    let _ = std::fs::remove_dir_all(r.repo.join("core/target"));
    assert!(!exec(false).status.success());
    // `env` prints MAVEN_OPTS with the tail; `verify` hashes every file.
    let env = r
        .hushrun()
        .args(["packages", "env"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&env.stdout),
        format!(
            "export MAVEN_OPTS='-Dmaven.repo.local.tail={}/parts/maven/repository'\n",
            store.display()
        )
    );
    let verify = r
        .hushrun()
        .args(["packages", "verify"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );
}

// covers: K-7
#[test]
fn an_unsigned_store_is_read_only_with_the_persons_say_so_and_never_in_ci() {
    let r = fixture();
    let cache = r.root.join("m2");
    let jar = cache.join("g/n/1/n-1.jar");
    std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
    std::fs::write(&jar, b"jar").unwrap();
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--out"])
        .arg(&store)
        .arg("--harvest")
        .arg(format!("maven={}", cache.display()))
        .output()
        .unwrap();
    assert!(
        baked.status.success(),
        "{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    let env = |vars: &[(&str, &str)]| {
        let mut cmd = r.hushrun();
        cmd.args(["packages", "env"])
            .env("HUSHRUN_PACKAGES_LOWER", &store);
        for (n, v) in vars {
            cmd.env(n, v);
        }
        let out = cmd.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (out, err) = env(&[]);
    assert_eq!(out, "");
    assert!(err.contains("nothing here verifies it"), "{err}");
    let (out, _) = env(&[("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")]);
    assert!(out.contains("maven.repo.local.tail="), "{out}");
    let (out, _) = env(&[("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1"), ("CI", "true")]);
    assert_eq!(out, "", "CI never reads an unverified store");
    // A store at a path with a space: the maven part is left alone, with a line.
    let spaced = r.root.join("my packages");
    std::fs::rename(&store, &spaced).unwrap();
    let mut cmd = r.hushrun();
    cmd.args(["packages", "env"])
        .env("HUSHRUN_PACKAGES_LOWER", &spaced)
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1");
    let out = cmd.output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("packages: maven: left alone: the store's folder")
    );
}

// covers: K-6
#[test]
fn exec_exits_with_commands_code_and_a_bake_keeps_no_withheld_credential() {
    let r = fixture();
    let out = r
        .hushrun()
        .args(["packages", "exec", "--", "sh", "-c", "exit 7"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
    // A fetch whose command writes its environment into the tools' caches: no withheld
    // credential reaches a file of the store.
    let store = r.root.join("packages");
    let canary = "canary-7f3a";
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--out"])
        .arg(&store)
        .args([
            "--",
            "sh",
            "-c",
            "d=$GRADLE_USER_HOME/caches/modules-2/files-2.1/g/n/1/abc; mkdir -p $d; env > $d/n-1.jar",
        ])
        .env("HUSHRUN_TOKEN", canary)
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", canary)
        .env("SIGSTORE_ID_TOKEN", canary)
        .output()
        .unwrap();
    assert!(
        baked.status.success(),
        "{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    let files = all_files(&store);
    assert!(files.iter().any(|f| f.ends_with("n-1.jar")), "{files:?}");
    for f in files {
        let text = std::fs::read(&f).unwrap();
        assert!(
            !String::from_utf8_lossy(&text).contains(canary),
            "{} holds a withheld credential",
            f.display()
        );
    }
}

// covers: K-6, K-7
#[test]
fn a_store_signed_by_a_signer_hushrun_yaml_names_is_read_in_ci_and_not_once_edited() {
    let r = fixture();
    let key = r.root.join("key.pem");
    let made = r
        .hushrun()
        .args(["keys", "generate", "--algorithm", "ecdsa-p256", "--out"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let public = r
        .hushrun()
        .args(["keys", "public", "--key"])
        .arg(&key)
        .output()
        .unwrap();
    let public = String::from_utf8_lossy(&public.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("public key: ").map(str::to_string))
        .expect("a public key");
    r.write(
        "hushrun.yaml",
        &format!(
            "version: 1\nrepository: github.com/acme/shop\nbuild:\n  baked:\n    signers:\n      - key: {public}\nchecks:\n  test:\n    run: mvn test\n"
        ),
    );
    let cache = r.root.join("m2");
    let jar = cache.join("g/n/1/n-1.jar");
    std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
    std::fs::write(&jar, b"jar").unwrap();
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--out"])
        .arg(&store)
        .arg("--harvest")
        .arg(format!("maven={}", cache.display()))
        .arg("--signing-key")
        .arg(&key)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&baked.stdout).into_owned();
    assert!(
        baked.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    assert!(said.contains("packages: signed by "), "{said}");
    assert!(store.join("packages.sigstore.json").is_file());
    let env = || {
        let out = r
            .hushrun()
            .args(["packages", "env"])
            .env("HUSHRUN_PACKAGES_LOWER", &store)
            .env("CI", "true")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert!(
        env().contains("maven.repo.local.tail="),
        "a signed store is read in CI"
    );
    // packages.json edited after signing: the signature is for other bytes.
    let path = store.join("packages.json");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("\"baked_ms\": ", "\"baked_ms\":  ")).unwrap();
    assert_eq!(env(), "", "a store edited after signing is untrusted");
}

// covers: K-6
#[test]
fn no_build_force_and_tar() {
    let r = fixture();
    let cache = r.root.join("m2");
    let jar = cache.join("g/n/1/n-1.jar");
    std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
    std::fs::write(&jar, b"jar").unwrap();
    let bake = |args: &[&str]| {
        let out = r
            .hushrun()
            .args(["packages", "bake", "--no-sign"])
            .args(args)
            .env("SOURCE_DATE_EPOCH", "1700000000")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let store = r.root.join("packages");
    let out = store.to_str().unwrap().to_string();
    let harvest = format!("maven={}", cache.display());
    // --no-build with no harvest: the part the build files name is left out, and nothing bakes.
    let (ok, _, err) = bake(&["--no-build", "--out", &out]);
    assert!(!ok);
    assert!(
        err.contains("packages: maven: left out: --no-build runs no build"),
        "{err}"
    );
    // A bake, then the same again: nothing to bake, unless --force.
    let (ok, said, err) = bake(&["--no-build", "--out", &out, "--harvest", &harvest]);
    assert!(ok, "{err}");
    assert!(said.contains("packages: baked 1 parts"), "{said}");
    let (ok, said, _) = bake(&["--out", &out, "--harvest", &harvest]);
    assert!(ok);
    assert!(said.contains("packages: nothing to bake: "), "{said}");
    let (ok, said, _) = bake(&["--out", &out, "--harvest", &harvest, "--force"]);
    assert!(ok);
    assert!(said.contains("packages: baked 1 parts"), "{said}");
    // A build file changed: it bakes again.
    r.write("core/pom.xml", "<project/>\n");
    let (ok, said, _) = bake(&["--out", &out, "--harvest", &harvest]);
    assert!(ok);
    assert!(said.contains("packages: baked 1 parts"), "{said}");
    // --tar alone: an archive under hushrun-packages/, the same bytes twice, that extracts to a
    // store every file of which is its row's.
    let tar = r.root.join("packages.tar");
    let tar_arg = tar.to_str().unwrap().to_string();
    let (ok, said, err) = bake(&["--tar", &tar_arg, "--harvest", &harvest]);
    assert!(ok, "{err}");
    assert!(
        said.contains("`tar -x -C /` puts the store at /hushrun-packages"),
        "{said}"
    );
    let first = std::fs::read(&tar).unwrap();
    let (ok, _, _) = bake(&["--tar", &tar_arg, "--harvest", &harvest]);
    assert!(ok);
    assert_eq!(
        std::fs::read(&tar).unwrap(),
        first,
        "the same store, the same bytes"
    );
    let into = r.root.join("extracted");
    std::fs::create_dir_all(&into).unwrap();
    let untar = std::process::Command::new("tar")
        .arg("-xf")
        .arg(&tar)
        .arg("-C")
        .arg(&into)
        .status()
        .unwrap();
    assert!(untar.success());
    let verify = r
        .hushrun()
        .args(["packages", "verify"])
        .env("HUSHRUN_PACKAGES_LOWER", into.join("hushrun-packages"))
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(
        !r.root.join(".packages.tar.store").exists(),
        "the scratch folder is gone"
    );
}
