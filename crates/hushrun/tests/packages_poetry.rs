//! `hushrun packages` with Poetry (K-6, K-10): a bake's COMMAND, `poetry install`, downloads into
//! a cache of the bake's own, whose `artifacts/` becomes the part; a run seeds Poetry's own cache
//! from it, each file checked against `poetry.lock` first, and Poetry installs without
//! downloading a file. Needs Poetry (`HUSHRUN_TEST_POETRY`) and pip's `python3`; no network: the
//! index is a folder served on 127.0.0.1, and its files are taken away before the read.

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

use std::path::{Path, PathBuf};
use std::process::{Child, Output};

use cli::Cli as _;
use pypackages::{WHEEL, path_with, pip, poetry, tamper, write_wheel};
use repo::Repo;

/// A folder served over HTTP on 127.0.0.1 until dropped.
struct Served(Child);

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Serves `dir` with the `python3` in `bin`, and returns the server and its port.
fn serve(bin: &Path, dir: &Path) -> (Served, u16) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let child = hushrun_testkit::tool::command(bin.join("python3").to_str().unwrap())
        .args([
            "-I",
            "-m",
            "http.server",
            "--bind",
            "127.0.0.1",
            "--directory",
        ])
        .arg(dir)
        .arg(port.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let served = Served(child);
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return (served, port);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("the index on port {port} didn't start");
}

/// Every file under `dir`.
fn all_files(dir: &Path) -> Vec<PathBuf> {
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

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// covers: K-6, K-10, K-3, K-5, K-31
#[test]
fn a_part_baked_from_poetry_install_seeds_poetrys_cache() {
    let Some((_, bin)) = pip() else { return };
    let Some(poetry) = poetry() else { return };
    // The index: one wheel, a PEP 503 page that names it with its hash.
    let index = tempfile::tempdir().unwrap();
    let sha = write_wheel(&bin, &index.path().join("files"));
    let page = index.path().join("simple/hushdemo");
    std::fs::create_dir_all(&page).unwrap();
    std::fs::write(
        page.join("index.html"),
        format!("<a href=\"../../files/{WHEEL}#sha256={sha}\">{WHEEL}</a>\n"),
    )
    .unwrap();
    let (_served, port) = serve(&bin, index.path());
    let r = Repo::new(&[
        (".gitignore".to_string(), ".venv/\n".to_string()),
        (
            "pyproject.toml".to_string(),
            format!(
                "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nrequires-python = \">=3.9\"\ndependencies = [\"hushdemo==1.0\"]\n\n[tool.poetry]\npackage-mode = false\n\n[[tool.poetry.source]]\nname = \"local\"\nurl = \"http://127.0.0.1:{port}/simple/\"\npriority = \"primary\"\n"
            ),
        ),
    ])
    .clear(&[
        "POETRY_CACHE_DIR",
        "POETRY_CONFIG_DIR",
        "VIRTUAL_ENV",
        "HUSHRUN_PACKAGES_LOWER",
        "HUSHRUN_PACKAGES",
        "HUSHRUN_PACKAGES_LOWER_UNTRUSTED",
    ])
    .give("POETRY_VIRTUALENVS_IN_PROJECT", "true")
    .give("POETRY_NO_INTERACTION", "1")
    .give("PATH", path_with(&bin));
    let locked = r
        .command(&poetry)
        .arg("lock")
        .env("POETRY_CACHE_DIR", r.root.join("lock-cache"))
        .output()
        .unwrap();
    assert!(locked.status.success(), "{}", said(&locked));
    hushrun_testkit::git::commit_all(&r.repo, "lock");

    // The bake: `poetry install` with Poetry's cache at a folder of the bake's own.
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .args(["--", &poetry, "install"])
        .output()
        .unwrap();
    assert!(baked.status.success(), "{}", said(&baked));
    assert!(
        said(&baked).contains("packages: poetry: 1 files")
            && said(&baked).contains("(poetry.lock)"),
        "{}",
        said(&baked)
    );
    let part: Vec<PathBuf> = all_files(&store.join("parts/poetry/artifacts"));
    assert_eq!(part.len(), 1, "{part:?}");
    assert!(part[0].ends_with(WHEEL));
    let rows = std::fs::read_to_string(store.join("rows/poetry.jsonl")).unwrap();
    assert!(
        rows.contains(&format!("\"locked\":\"sha256:{sha}\"")),
        "{rows}"
    );

    // The index keeps its page, and loses its file: only a seeded cache installs now.
    std::fs::remove_dir_all(index.path().join("files")).unwrap();
    let install = |cache: &str, lower: &Path| {
        let _ = std::fs::remove_dir_all(r.repo.join(".venv"));
        r.hushrun()
            .args(["packages", "exec", "--", &poetry, "install"])
            .env("POETRY_CACHE_DIR", r.root.join(cache))
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .output()
            .unwrap()
    };
    let read = install("c1", &store);
    assert!(read.status.success(), "{}", said(&read));
    assert!(
        said(&read).contains("packages: poetry: seeded 1 files into"),
        "{}",
        said(&read)
    );
    let seeded = all_files(&r.root.join("c1/artifacts"));
    assert_eq!(seeded.len(), 1, "{seeded:?}");
    assert_eq!(
        seeded[0].strip_prefix(r.root.join("c1")).unwrap(),
        part[0].strip_prefix(store.join("parts/poetry")).unwrap(),
        "at the path Poetry names by the file's URL and hash"
    );
    // Without the store the same install fails: the file was the store's.
    assert!(!install("c2", Path::new("")).status.success());
    // `restore` seeds too, and says so when there is nothing left to copy.
    let restore = r
        .hushrun()
        .args(["packages", "restore"])
        .env("POETRY_CACHE_DIR", r.root.join("c1"))
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    assert!(
        said(&restore).contains("seeded 0 files into")
            && said(&restore).contains("1 there already"),
        "{}",
        said(&restore)
    );

    // covers: K-10
    // In `hushrun run` too, and the check's key is the same with and without the store: the run
    // without it skips.
    let run = |lower: &Path| {
        r.hushrun()
            .args(["run", "t", "--", &poetry, "install"])
            .env("POETRY_CACHE_DIR", r.root.join("c3"))
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .output()
            .unwrap()
    };
    let first = run(&store);
    assert!(said(&first).contains("t passed"), "{}", said(&first));
    let second = run(Path::new(""));
    assert!(said(&second).contains("t skipped"), "{}", said(&second));

    // A file changed in place, its size the same: the seed refuses it, so Poetry never gets it.
    tamper(&part[0]);
    let read = install("c4", &store);
    assert!(!read.status.success());
    assert!(
        said(&read).contains("left out 1 files whose bytes aren't the ones the lockfile hashes"),
        "{}",
        said(&read)
    );
    assert!(all_files(&r.root.join("c4")).is_empty());
}
