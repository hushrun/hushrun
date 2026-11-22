//! Baked stores of Go packages and C objects (B-63, B-64): a run with no backend keeps its
//! portable Go archives and its C objects in the store, `hushrun build bake` bakes them with the
//! crates, and a fresh machine reads them from a read-only folder underneath its own store, in
//! this machine's store, then the baked store, then the relay.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;
use serde_json::Value;

const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_LOG",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_BUILD_SHARE",
    "HUSHRUN_BUILD_AUDIT",
    "HUSHRUN_BUILD_LOWER",
    "HUSHRUN_BUILD_LOWER_UNTRUSTED",
    "HUSHRUN_CC",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "RUSTC_WRAPPER",
    "GOCACHEPROG",
    "GOFLAGS",
    "GOOS",
    "GOARCH",
    "GOWORK",
    "GOCACHE",
    "GOENV",
];

fn have(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .env("GOTOOLCHAIN", "local")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn have_go() -> bool {
    let version = Command::new("go")
        .args(["env", "GOVERSION"])
        .env("GOTOOLCHAIN", "local")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let known = hushrun_build::go::command::version_known(&version);
    if !known {
        eprintln!("note: skipping: Go 1.24 to 1.27 is missing");
    }
    known
}

/// One checkout, at one path: every machine builds it there, as an image and the sandboxes
/// started from it do.
fn repo(root: &Path, files: &[(&str, &str)]) -> PathBuf {
    let repo = root.join("repo");
    for (path, text) in files {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    git::init(&repo, "sha1");
    git::commit_all(&repo, "init");
    std::fs::canonicalize(repo).unwrap()
}

/// A machine: its own hushrun cache (so its own store), its own Go cache, no backend.
struct Machine {
    root: PathBuf,
    repo: PathBuf,
}

impl Machine {
    fn new(dir: &Path, name: &str, repo: &Path) -> Machine {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        Machine {
            root,
            repo: repo.to_path_buf(),
        }
    }

    fn hushrun(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.current_dir(&self.repo)
            .env("PWD", &self.repo)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", "http://127.0.0.1:9")
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            // No backend: nothing is recorded, nothing is shared.
            .env("HUSHRUN_BACKEND", "")
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_BUILD_AUDIT", "none")
            .env("GOCACHE", self.root.join("gocache"))
            .env("GOTOOLCHAIN", "local")
            .env("GOENV", "off")
            .env("CGO_ENABLED", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"));
        cmd
    }

    fn run(&self, script: &str, lower: Option<&Path>) -> (i32, String) {
        let mut cmd = self.hushrun();
        cmd.args(["run", "build", "--force", "--", "sh", "-c", script]);
        if let Some(lower) = lower {
            cmd.env("HUSHRUN_BUILD_LOWER", lower)
                .env("HUSHRUN_BUILD_LOWER_UNTRUSTED", "1");
        }
        let output = cmd.output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn bake(&self, out: &Path) -> Value {
        let output = self
            .hushrun()
            .args([
                "build", "bake", "--check", "build", "--format", "json", "--out",
            ])
            .arg(out)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn line<'a>(stderr: &'a str, start: &str) -> &'a str {
    stderr
        .lines()
        .find(|l| l.contains(start))
        .unwrap_or_default()
}

fn chmod_tree(dir: &Path, writable: bool) {
    let mut all = vec![dir.to_path_buf()];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            }
            all.push(entry.path());
        }
    }
    if writable {
        all.sort();
    } else {
        all.sort_by(|a, b| b.cmp(a));
    }
    for path in all {
        let dir = std::fs::metadata(&path).unwrap().is_dir();
        let mode = match (dir, writable) {
            (true, true) => 0o755,
            (true, false) => 0o555,
            (false, true) => 0o644,
            (false, false) => 0o444,
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
}

fn part<'a>(said: &'a Value, kind: &str) -> Option<&'a Value> {
    said["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == kind)
}

// covers: B-39, B-42, B-63, B-64
#[test]
fn go_packages_a_run_with_no_backend_kept_are_baked_and_read_from_a_read_only_folder() {
    if !have_go() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(
        dir.path(),
        &[
            ("go.mod", "module example.com/m\n\ngo 1.24\n"),
            (
                "lib/lib.go",
                "package lib\n\nconst V = 1\n\nfunc F() int { return V * 1 }\n",
            ),
            (
                "cmd/app/main.go",
                "package main\n\nimport \"example.com/m/lib\"\n\nfunc main() { println(lib.F()) }\n",
            ),
        ],
    );
    let laptop = Machine::new(dir.path(), "laptop", &repo);
    // B-39: a run with no backend and no baked store leaves Go as plain go runs it: nothing is
    // kept, and a bake has no Go part to write.
    let (code, stderr) = laptop.run("go build -o ../app ./cmd/app", None);
    assert_eq!(code, 0, "{stderr}");
    assert!(line(&stderr, "build: go: ").is_empty(), "{stderr}");
    // B-39, B-63: a store named that the run's state ignores (an empty folder) changes nothing.
    let ignored = dir.path().join("ignored");
    std::fs::create_dir_all(&ignored).unwrap();
    let (code, stderr) = laptop.run("go build -o ../app ./cmd/app", Some(&ignored));
    assert_eq!(code, 0, "{stderr}");
    assert!(line(&stderr, "build: go: ").is_empty(), "{stderr}");
    // B-39, B-42: one that reads a baked store (here one with nothing for it yet) gets the cache
    // program, which keeps its portable archives in the store.
    let earlier = dir.path().join("earlier");
    empty_go_store(&earlier);
    let _ = std::fs::remove_dir_all(laptop.root.join("gocache"));
    let (code, stderr) = laptop.run("go build -o ../app ./cmd/app", Some(&earlier));
    assert_eq!(code, 0, "{stderr}");
    assert!(line(&stderr, "build: go: ").contains("built"), "{stderr}");
    let baked = dir.path().join("baked");
    let said = laptop.bake(&baked);
    let go = part(&said, "go/1").unwrap_or_else(|| panic!("{said}"));
    let n = go["blocks"].as_u64().unwrap();
    assert!(n >= 2, "{said}");
    assert!(go["platform"].as_str().unwrap().contains('_'));

    chmod_tree(&baked, false);
    // A fresh machine: an empty store and an empty Go cache, at the same checkout path.
    let sandbox = Machine::new(dir.path(), "sandbox", &repo);
    let (code, stderr) = sandbox.run("go build -o ../app2 ./cmd/app && ../app2", Some(&baked));
    assert_eq!(code, 0, "{stderr}");
    let go_line = line(&stderr, "build: go: ");
    assert!(go_line.contains("from the baked store"), "{stderr}");
    assert!(stderr.contains("1\n"), "{stderr}");
    chmod_tree(&baked, true);
}

const SOURCES: &[&str] = &["one", "two", "three"];

fn c_files() -> Vec<(String, String)> {
    let mut files = vec![
        (
            "CMakeLists.txt".to_string(),
            "cmake_minimum_required(VERSION 3.17)\nproject(p C)\n".to_string(),
        ),
        (
            "include/p.h".to_string(),
            "#define P_VALUE 7\nint p_value(void);\n".to_string(),
        ),
        (".gitignore".to_string(), "build/\n".to_string()),
    ];
    for name in SOURCES {
        files.push((
            format!("src/{name}.c"),
            format!("#include \"p.h\"\nint {name}(void) {{ return P_VALUE; }}\n"),
        ));
    }
    files
}

fn compile_script() -> String {
    let mut script = String::from("set -e; rm -rf build; mkdir -p build");
    for name in SOURCES {
        script.push_str(&format!(
            "; ${{HUSHRUN_CC:?}} cc -Iinclude -O1 -MD -MF build/{name}.o.d -c src/{name}.c -o build/{name}.o"
        ));
    }
    script
}

// covers: B-60, B-63, B-64
#[test]
fn c_objects_are_baked_and_read_from_a_read_only_folder() {
    if !have("cc", &["--version"]) {
        eprintln!("note: skipping: no C compiler");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let files = c_files();
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let repo = repo(dir.path(), &files);
    let laptop = Machine::new(dir.path(), "laptop", &repo);
    let (code, stderr) = laptop.run(&compile_script(), None);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        line(&stderr, "build: c/c++: ").contains("3 built"),
        "{stderr}"
    );
    let baked = dir.path().join("baked");
    let said = laptop.bake(&baked);
    let cc = part(&said, "cc/1").unwrap_or_else(|| panic!("{said}"));
    assert_eq!(cc["blocks"], 3, "{said}");

    chmod_tree(&baked, false);
    let sandbox = Machine::new(dir.path(), "sandbox", &repo);
    let (code, stderr) = sandbox.run(&compile_script(), Some(&baked));
    assert_eq!(code, 0, "{stderr}");
    let cc_line = line(&stderr, "build: c/c++: ");
    assert!(
        cc_line.contains("3 reused (3 from the baked store), 0 built"),
        "{stderr}"
    );
    assert!(repo.join("build/two.o").is_file());
    chmod_tree(&baked, true);
}

/// B-63: a baked store with nothing in it but a Go part for this machine's platform, untrusted,
/// for no repository: what a run reads under `HUSHRUN_BUILD_LOWER_UNTRUSTED=1`, where an empty
/// folder is a store its state ignores.
fn empty_go_store(folder: &Path) {
    use hushrun_build::baked::{self, Baked, BakedCheck, Part, Source, Trust, Untrusted};
    let out = Command::new("go")
        .args(["env", "GOOS", "GOARCH"])
        .env("GOTOOLCHAIN", "local")
        .output()
        .unwrap();
    let said = String::from_utf8(out.stdout).unwrap();
    let mut lines = said.lines();
    let platform = format!("{}_{}", lines.next().unwrap(), lines.next().unwrap());
    let checks = vec![BakedCheck {
        check: None,
        parts: vec![Part {
            kind: "go/1".into(),
            platform,
            manifest: hushrun_core::blocks::Hex32([7; 32]),
        }],
    }];
    let store = Baked {
        schema: baked::SCHEMA.into(),
        trust: Trust::Untrusted {
            because: Untrusted::LocalStore,
        },
        manifest: baked::store_manifest(&checks),
        source: Source::Store,
        repository: None,
        platform: "linux-x64".into(),
        checks,
        toolchains: Vec::new(),
        packs: Vec::new(),
        baked_ms: 0,
        paths: None,
    };
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(
        folder.join(baked::BAKED_JSON),
        serde_json::to_vec_pretty(&store).unwrap(),
    )
    .unwrap();
}

// covers: B-72
// A Go module's dependencies-only bake builds its third-party packages (here a module the main
// module requires, replaced by a folder of its own) with nothing of the main module's, and a fresh
// machine building the module reads them from the store.
#[test]
fn a_go_module_s_dependencies_only_bake_builds_its_third_party_packages() {
    if !have_go() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(
        dir.path(),
        &[
            (
                "go.mod",
                "module example.com/m\n\ngo 1.24\n\nrequire example.com/far v0.0.0\n\nreplace example.com/far => ./far\n",
            ),
            ("far/go.mod", "module example.com/far\n\ngo 1.24\n"),
            ("far/far.go", "package far\n\nfunc F() int { return 7 }\n"),
            (
                "lib/lib.go",
                "package lib\n\nimport \"example.com/far\"\n\nfunc F() int { return far.F() + 1 }\n",
            ),
            (
                "cmd/app/main.go",
                "package main\n\nimport \"example.com/m/lib\"\n\nfunc main() { println(lib.F()) }\n",
            ),
        ],
    );
    let laptop = Machine::new(dir.path(), "laptop", &repo);
    let deps = dir.path().join("deps");
    let output = laptop
        .hushrun()
        .args([
            "build",
            "bake",
            "--deps-only",
            "--check",
            "build",
            "--format",
            "json",
            "--out",
        ])
        .arg(&deps)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let said: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(said["deps_only"]["go_packages"], 1, "{said}");
    let go = part(&said, "go/1").unwrap_or_else(|| panic!("{said}"));
    assert!(go["blocks"].as_u64().unwrap() >= 1, "{said}");
    // Nothing of the main module was built: a build of it compiles its own two packages.
    let sandbox = Machine::new(dir.path(), "sandbox", &repo);
    let (code, stderr) = sandbox.run("go build -o ../app ./cmd/app && ../app", Some(&deps));
    assert_eq!(code, 0, "{stderr}");
    let go_line = line(&stderr, "build: go: ");
    assert!(go_line.contains("from the baked store"), "{stderr}");
    assert!(stderr.contains("8\n"), "{stderr}");
}
