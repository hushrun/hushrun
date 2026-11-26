//! `hushrun packages` with pip (K-6, K-10): a requirements file with hashes is fetched by `pip
//! download` into a part, which pip reads through `PIP_FIND_LINKS` where it reaches no index, and
//! checks against the requirements file itself. Needs pip (`HUSHRUN_TEST_PIP`); no network: the
//! wheel is built by hand and every pip here has `PIP_NO_INDEX=1`.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/pypackages.rs"]
mod pypackages;
#[path = "common/python.rs"]
mod python;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::Path;

use cli::Cli as _;
use pypackages::{WHEEL, path_with, pip, tamper, write_wheel};
use repo::Repo;

// covers: K-6, K-10, K-3
#[test]
fn a_part_pip_downloaded_is_read_where_no_index_is() {
    let Some((pip, bin)) = pip() else { return };
    let wheels = tempfile::tempdir().unwrap();
    let sha = write_wheel(&bin, wheels.path());
    let r = Repo::new(&[(
        "requirements.txt".to_string(),
        format!("hushdemo==1.0 \\\n    --hash=sha256:{sha}\n"),
    )])
    .clear(&[
        "PIP_FIND_LINKS",
        "PIP_INDEX_URL",
        "PIP_EXTRA_INDEX_URL",
        "HUSHRUN_PACKAGES_LOWER",
        "HUSHRUN_PACKAGES",
        "HUSHRUN_PACKAGES_LOWER_UNTRUSTED",
    ])
    .give("PIP_NO_INDEX", "1")
    .give(
        "PIP_CONFIG_FILE",
        if cfg!(windows) { "nul" } else { "/dev/null" },
    )
    .give("PIP_DISABLE_PIP_VERSION_CHECK", "1");
    // The bake runs pip's own download: here from the hand-built wheel's folder.
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .env("PATH", path_with(&bin))
        .env("PIP_FIND_LINKS", wheels.path())
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&baked.stdout).into_owned();
    assert!(
        baked.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    assert!(
        said.contains("packages: pip: 1 files")
            && said.contains("pip download` (requirements.txt)"),
        "{said}"
    );
    assert!(said.contains("so any run reads it"), "{said}");
    let rows = std::fs::read_to_string(store.join("rows/pip.jsonl")).unwrap();
    assert!(
        rows.contains(&format!("\"locked\":\"sha256:{sha}\"")) && rows.contains("hushdemo==1.0"),
        "{rows}"
    );
    let install = |target: &Path, lower: &Path| {
        r.hushrun()
            .args([
                "packages", "exec", "--", &pip, "install", "--quiet", "--target",
            ])
            .arg(target)
            .args(["-r", "requirements.txt"])
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .output()
            .unwrap()
    };
    // Unsigned, and read without the person's say-so: pip checks each file against the hashes.
    let read = install(&r.root.join("t1"), &store);
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert!(r.root.join("t1/hushdemo/__init__.py").is_file());
    // Without the store the same install finds nothing.
    assert!(!install(&r.root.join("t2"), Path::new("")).status.success());
    // `env` names the part.
    let env = r
        .hushrun()
        .args(["packages", "env"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&env.stdout),
        format!("export PIP_FIND_LINKS='{}/parts/pip'\n", store.display())
    );

    // covers: K-10
    // In `hushrun run` too, and the check's key is the same with and without the store: the run
    // without it skips.
    let run = |lower: &Path, target: &str| {
        r.hushrun()
            .args(["run", "t", "--", &pip, "install", "--quiet", "--target"])
            .arg(r.root.join(target))
            .args(["-r", "requirements.txt"])
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .output()
            .unwrap()
    };
    let first = run(&store, "t3");
    let said = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(said.contains("t passed"), "{said}");
    let second = run(Path::new(""), "t3");
    let said = String::from_utf8_lossy(&second.stderr).into_owned();
    assert!(said.contains("t skipped"), "{said}");

    // A file changed in place, its size the same: the check by names passes it, pip refuses it,
    // and `verify` finds it.
    tamper(&store.join("parts/pip").join(WHEEL));
    let read = install(&r.root.join("t4"), &store);
    assert!(!read.status.success());
    assert!(
        String::from_utf8_lossy(&read.stderr).contains("HASHES"),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    let verify = r
        .hushrun()
        .args(["packages", "verify"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    assert!(!verify.status.success());
}

// covers: K-5, K-6
#[test]
fn only_a_file_the_requirements_hash_is_baked() {
    let Some((_, bin)) = pip() else { return };
    let wheels = tempfile::tempdir().unwrap();
    write_wheel(&bin, wheels.path());
    std::fs::write(wheels.path().join("other-2.0.tar.gz"), "not named").unwrap();
    // No hash: nothing to fetch with, and nothing to harvest.
    let r = Repo::new(&[(
        "requirements.txt".to_string(),
        "hushdemo==1.0\n".to_string(),
    )])
    .clear(&[
        "PIP_FIND_LINKS",
        "HUSHRUN_PACKAGES_LOWER",
        "HUSHRUN_PACKAGES",
    ]);
    let bake = |harvest: bool| {
        let mut cmd = r.hushrun();
        cmd.args(["packages", "bake", "--no-sign", "--out"])
            .arg(r.root.join("packages"))
            .env("PATH", path_with(&bin));
        if harvest {
            cmd.arg("--harvest")
                .arg(format!("pip={}", wheels.path().display()));
        }
        cmd.output().unwrap()
    };
    let out = bake(false);
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success());
    assert!(
        said.contains("packages: pip: left out: no requirements file names its packages' hashes"),
        "{said}"
    );
    let out = bake(true);
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success());
    assert!(
        said.contains("its lockfile (requirements.txt) hashes none of the 2 files"),
        "{said}"
    );
}
