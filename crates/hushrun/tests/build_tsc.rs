//! TypeScript build blocks (B-100 to B-109), end to end: `hushrun run NAME -- COMMAND` running
//! the real `tsc` of TypeScript 6 through the hook, in two checkouts of one machine and on two
//! machines (two checkouts at different paths, two stores) against the fake build protocol.
//!
//! They need Node and TypeScript 6, installed from npm outside the repository, with
//! `HUSHRUN_TEST_TSC` naming its `node_modules` (CI: `npm install --prefix <dir>
//! typescript@6.0.3`). They say so and pass where it is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`.

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
    "HUSHRUN_CC",
    "HUSHRUN_TSC",
    "HUSHRUN_UNITS",
    "HUSHRUN_JEST",
    "HUSHRUN_VITEST",
    "NODE_OPTIONS",
    "NO_COLOR",
    "FORCE_COLOR",
    "NODE_ENV",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "RUSTC_WRAPPER",
];

/// The `node_modules` holding TypeScript 6, when this machine has it.
fn typescript() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("HUSHRUN_TEST_TSC")?);
    let package = std::fs::read_to_string(dir.join("typescript/package.json")).ok()?;
    let package: serde_json::Value = serde_json::from_str(&package).ok()?;
    let version = package["version"].as_str()?.to_string();
    let node = Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    (node && version.starts_with("6.")).then_some(dir)
}

fn have_tsc() -> Option<PathBuf> {
    if let Some(dir) = typescript() {
        return Some(dir);
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("no TypeScript 6 in HUSHRUN_TEST_TSC, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!(
        "note: skipping: no TypeScript 6 (HUSHRUN_TEST_TSC; HUSHRUN_TEST_TOOLS=required makes this fail)"
    );
    None
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let kind = entry.file_type().unwrap();
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest);
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path()).unwrap(), &dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// Two packages with a reference between them, a type-check of their own, and `tsc` itself in
/// the checkout's `node_modules`, as `npm install` leaves it.
fn project(root: &Path, modules: &Path) -> PathBuf {
    let repo = root.join("repo");
    let write = |path: &str, text: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("package.json", "{\"name\":\"m\",\"private\":true}\n");
    write(
        "tsconfig.base.json",
        "{\"compilerOptions\":{\"strict\":true,\"module\":\"nodenext\",\"composite\":true,\"sourceMap\":true}}\n",
    );
    write(
        "packages/core/tsconfig.json",
        "{\"extends\":\"../../tsconfig.base.json\",\"compilerOptions\":{\"outDir\":\"dist\",\"rootDir\":\"src\"},\"include\":[\"src\"]}\n",
    );
    write(
        "packages/core/src/index.ts",
        "export const n: number = 1;\n",
    );
    write(
        "packages/app/tsconfig.json",
        "{\"extends\":\"../../tsconfig.base.json\",\"compilerOptions\":{\"outDir\":\"dist\",\"rootDir\":\"src\"},\"include\":[\"src\"],\"references\":[{\"path\":\"../core\"}]}\n",
    );
    write(
        "packages/app/src/index.ts",
        "import { n } from \"../../core/src/index.js\";\nexport const m = n + 1;\n",
    );
    write(
        "tsconfig.json",
        "{\"files\":[],\"references\":[{\"path\":\"packages/core\"},{\"path\":\"packages/app\"}]}\n",
    );
    write(".gitignore", "node_modules/\ndist/\n*.tsbuildinfo\n");
    git::init(&repo, "sha1");
    git::commit_all(&repo, "init");
    copy_tree(modules, &repo.join("node_modules"));
    repo
}

/// A machine: its own checkout, hushrun cache (and so store) and machine key.
struct Machine {
    root: PathBuf,
    repo: PathBuf,
}

impl Machine {
    fn new(dir: &Path, name: &str, modules: &Path, from: Option<&Path>) -> Machine {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let repo = match from {
            None => project(&root, modules),
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
                copy_tree(modules, &repo.join("node_modules"));
                repo
            }
        };
        std::fs::write(
            root.join("machine.pem"),
            hushrun_testkit::world::test_key(7).to_pem().as_str(),
        )
        .unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        Machine { root, repo }
    }

    /// Another checkout of this machine: its own folder, this machine's cache.
    fn worktree(&self, name: &str, modules: &Path) -> Machine {
        let repo = self.root.join(name);
        git::run(
            &self.root,
            &[
                "clone",
                "-q",
                "--no-hardlinks",
                self.repo.to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
            None,
        );
        copy_tree(modules, &repo.join("node_modules"));
        Machine {
            root: self.root.clone(),
            repo,
        }
    }

    fn hushrun(&self, fake: Option<&FakeBuild>) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.current_dir(&self.repo)
            .env("PWD", &self.repo)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_MACHINE_KEY_FILE", self.root.join("machine.pem"))
            .env("HUSHRUN_HOST", "http://127.0.0.1:9")
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_BUILD_AUDIT", "none")
            .env("HUSHRUN_CC_MANIFEST_WAIT_MS", "15000")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"));
        if let Some(fake) = fake {
            cmd.env("HUSHRUN_BACKEND", fake.url())
                .env("HUSHRUN_TOKEN", FakeBuild::TOKEN);
        }
        cmd
    }

    /// `hushrun run build -- sh -c SCRIPT`: its exit code, stdout and stderr.
    fn run(&self, fake: Option<&FakeBuild>, script: &str) -> (i32, String, String) {
        let mut cmd = self.hushrun(fake);
        let held = if fake.is_some() { " && sleep 1" } else { "" };
        cmd.args([
            "run",
            "build",
            "--force",
            "--",
            "sh",
            "-c",
            &format!("{script}{held}"),
        ]);
        let output = cmd.output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.repo.join(path)).unwrap_or_default()
    }
}

/// The TypeScript line of a run's summary.
fn tsc_line(stderr: &str) -> String {
    stderr
        .lines()
        .find(|l| l.contains("build: typescript: ") && l.contains("compile"))
        .unwrap_or_default()
        .to_string()
}

const BUILD: &str = "node node_modules/typescript/bin/tsc -b";
const OUTPUTS: &[&str] = &[
    "packages/core/dist/index.js",
    "packages/core/dist/index.d.ts",
    "packages/core/dist/index.js.map",
    "packages/core/tsconfig.tsbuildinfo",
    "packages/app/dist/index.js",
    "packages/app/dist/index.d.ts",
    "packages/app/tsconfig.tsbuildinfo",
];

// covers: B-100, B-103, B-104, B-105
#[test]
fn a_build_of_project_references_is_put_in_place_in_another_checkout() {
    let Some(modules) = have_tsc() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", &modules, None);
    let (code, _, stderr) = a.run(None, BUILD);
    assert_eq!(code, 0, "{stderr}");
    assert!(tsc_line(&stderr).contains("1 built"), "{stderr}");
    let b = a.worktree("b", &modules);
    let (code, _, stderr) = b.run(None, BUILD);
    assert_eq!(code, 0, "{stderr}");
    assert!(tsc_line(&stderr).contains("1 reused"), "{stderr}");
    for out in OUTPUTS {
        assert!(!a.read(out).is_empty(), "{out}");
        assert_eq!(a.read(out), b.read(out), "{out}");
    }
    // Plain `tsc -b` after the reuse has nothing to do: what was put in place is newer than its
    // sources.
    let verbose = Command::new("node")
        .args(["node_modules/typescript/bin/tsc", "-b", "--verbose"])
        .current_dir(&b.repo)
        .env_remove("NODE_OPTIONS")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&verbose.stdout);
    assert!(!said.contains("Building project"), "{said}");
    // A changed source compiles, in the checkout it changed in, and writes what it should.
    std::fs::write(
        b.repo.join("packages/core/src/index.ts"),
        "export const n: number = 41;\n",
    )
    .unwrap();
    for out in OUTPUTS {
        let _ = std::fs::remove_file(b.repo.join(out));
    }
    let (code, _, stderr) = b.run(None, BUILD);
    assert_eq!(code, 0, "{stderr}");
    assert!(tsc_line(&stderr).contains("1 built"), "{stderr}");
    assert!(b.read("packages/core/dist/index.js").contains("41"));
    // A `package.json` that appears where the compiler looked for one, and changes the
    // module format: another compile.
    let c = a.worktree("c", &modules);
    std::fs::write(
        c.repo.join("packages/core/package.json"),
        "{\"type\":\"module\"}\n",
    )
    .unwrap();
    // (The app, CommonJS, may now fail to import it: the status is the compiler's.)
    let (_, _, stderr) = c.run(None, BUILD);
    assert!(tsc_line(&stderr).contains("1 built"), "{stderr}");
    assert!(
        c.read("packages/core/dist/index.js")
            .contains("export const")
    );
}

// covers: B-107
#[test]
fn a_type_check_s_errors_and_status_are_reused() {
    let Some(modules) = have_tsc() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", &modules, None);
    std::fs::write(
        a.repo.join("packages/core/src/index.ts"),
        "export const n: number = \"one\";\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "broken");
    let check = "node node_modules/typescript/bin/tsc --noEmit -p packages/core";
    let (code, first, stderr) = a.run(None, check);
    assert_ne!(code, 0, "{stderr}");
    assert!(first.contains("TS2322"), "{first}");
    let b = a.worktree("b", &modules);
    let (code, again, stderr) = b.run(None, check);
    assert_ne!(code, 0, "{stderr}");
    assert!(tsc_line(&stderr).contains("1 reused"), "{stderr}");
    assert_eq!(first, again);
    assert!(!b.repo.join("packages/core/dist").exists());
}

// covers: B-101
#[test]
fn a_watch_and_typescript_s_own_output_pass_through() {
    let Some(modules) = have_tsc() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", &modules, None);
    let (code, out, stderr) = a.run(None, "node node_modules/typescript/bin/tsc --version");
    assert_eq!(code, 0, "{stderr}");
    assert!(out.starts_with("Version 6."), "{out}");
    assert!(stderr.contains("1 prints"), "{stderr}");
    // Other Node processes are left alone.
    let (code, out, stderr) = a.run(None, "node -e 'console.log(42)'");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(out.trim(), "42");
    assert!(tsc_line(&stderr).is_empty(), "{stderr}");
}

// covers: B-108
#[test]
fn a_build_one_machine_stored_is_reused_by_another_at_another_path() {
    let Some(modules) = have_tsc() else {
        return;
    };
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", &modules, None);
    let (code, _, stderr) = a.run(Some(&fake), BUILD);
    assert_eq!(code, 0, "{stderr}");
    assert!(tsc_line(&stderr).contains("1 built"), "{stderr}");
    let uploaded: usize = fake
        .begins()
        .iter()
        .filter(|b| b.kind == "tsc/1")
        .map(|b| b.blocks.len())
        .sum();
    assert_eq!(uploaded, 1, "{stderr}");
    fake.set_costs(60_000);
    let b = Machine::new(dir.path(), "b", &modules, Some(&a.repo));
    assert_ne!(a.repo, b.repo);
    let (code, _, stderr) = b.run(Some(&fake), BUILD);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        tsc_line(&stderr).contains("1 reused (1 from the shared store)"),
        "{stderr}"
    );
    for out in OUTPUTS {
        assert_eq!(a.read(out), b.read(out), "{out}");
    }
    assert!(
        fake.closes().iter().any(|c| c.kind == "tsc/1"
            && c.platform
                == if cfg!(target_os = "macos") {
                    "darwin"
                } else {
                    std::env::consts::OS
                }),
        "a tsc/1 close for the platform"
    );
}
