//! Go build blocks (B-36 to B-47), end to end: `hushrun run NAME -- go build` on two machines
//! (two checkouts, two stores, two Go caches) against the fake build protocol, with the real Go
//! toolchain. They need Go 1.24 or later, and say so and pass where it is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use hushrun_testkit::fake_build::{FakeBuild, route};
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
    "RUSTC_WRAPPER",
    "GOCACHEPROG",
    "GOFLAGS",
    "GOOS",
    "GOARCH",
    "GOWORK",
    "GOCACHE",
    "GOENV",
];

/// Whether Go 1.24 or later is here; else the test passes with a note, unless
/// `HUSHRUN_TEST_TOOLS=required`.
fn have_go() -> bool {
    let version = Command::new("go")
        .args(["env", "GOVERSION"])
        .env("GOTOOLCHAIN", "local")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if hushrun_build::go::command::version_known(&version) {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("Go 1.24 to 1.27 is missing ({version:?}), and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!(
        "note: skipping: Go 1.24 to 1.27 is missing (HUSHRUN_TEST_TOOLS=required makes this fail)"
    );
    false
}

/// A module whose `lib` returns `V`, a command that prints it, committed in a repository. Only
/// the runtime's packages, so a cold build is short.
fn module(root: &Path, files: &[(&str, &str)]) -> PathBuf {
    let repo = root.join("repo");
    let write = |path: &str, text: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("go.mod", "module example.com/m\n\ngo 1.24\n");
    write(
        "lib/lib.go",
        "package lib\n\nconst V = 1\n\nfunc F() int { return V * 1 }\n",
    );
    write(
        "cmd/app/main.go",
        "package main\n\nimport \"example.com/m/lib\"\n\nfunc main() { println(lib.F()) }\n",
    );
    for (path, text) in files {
        write(path, text);
    }
    git::init(&repo, "sha1");
    git::commit_all(&repo, "init");
    repo
}

/// A machine: its own checkout, hushrun cache (and so store), Go cache and machine key.
struct Machine {
    root: PathBuf,
    repo: PathBuf,
}

impl Machine {
    fn new(dir: &Path, name: &str, from: Option<&Path>, files: &[(&str, &str)]) -> Machine {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let repo = match from {
            None => module(&root, files),
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
        Machine { root, repo }
    }

    fn gocache(&self) -> PathBuf {
        self.root.join("gocache")
    }

    /// Go as these tests run it: this machine's own cache, the local toolchain, no
    /// configuration file of the person's, `-trimpath` so that two checkouts share the module's
    /// own packages too.
    fn go_env(&self, cmd: &mut Command) {
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.env("GOCACHE", self.gocache())
            .env("GOTOOLCHAIN", "local")
            .env("GOENV", "off")
            .env("GOFLAGS", "-trimpath")
            .env("CGO_ENABLED", "0");
    }

    fn hushrun(&self, fake: &FakeBuild) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        self.go_env(&mut cmd);
        cmd.current_dir(&self.repo)
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

    /// `hushrun run build -- sh -c SCRIPT`, held a moment after the script so that the uploads
    /// of a build this short get their turn.
    fn run(&self, fake: &FakeBuild, script: &str) -> (i32, String, String) {
        self.run_with(fake, script, |_| {})
    }

    fn run_with(
        &self,
        fake: &FakeBuild,
        script: &str,
        change: impl FnOnce(&mut Command),
    ) -> (i32, String, String) {
        let mut cmd = self.hushrun(fake);
        cmd.args([
            "run",
            "build",
            "--force",
            "--",
            "sh",
            "-c",
            &format!("{script} && sleep 1"),
        ]);
        change(&mut cmd);
        let output = cmd.output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    /// Plain `go`, with no hushrun: how many packages it compiled (`-x` shows each compile).
    fn plain_go_compiles(&self, args: &[&str]) -> usize {
        let mut cmd = Command::new("go");
        self.go_env(&mut cmd);
        let output = cmd
            .current_dir(&self.repo)
            .arg(args[0])
            .arg("-x")
            .args(&args[1..])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|l| l.contains("/compile ") && l.contains(" -o "))
            .count()
    }
}

/// The Go line of a run's summary.
fn go_line(stderr: &str) -> String {
    stderr
        .lines()
        .find(|l| l.contains("build: go: "))
        .unwrap_or_default()
        .to_string()
}

/// The `go/1` blocks the fake holds.
fn go_blocks(fake: &FakeBuild) -> usize {
    fake.begins()
        .iter()
        .filter(|b| b.kind == "go/1")
        .map(|b| b.blocks.len())
        .sum()
}

// covers: B-36, B-41, B-43
#[test]
fn a_go_package_one_machine_built_is_reused_on_another_from_the_shared_store() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    let (code, _, stderr) = a.run(&fake, "go build -o ../app ./cmd/app && ../app");
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("1\n"), "{stderr}");
    assert!(go_line(&stderr).contains("built"), "{stderr}");
    assert!(go_blocks(&fake) > 0, "nothing went up: {stderr}");
    // A Go open and close of their own, kind `go/1`, with the Go platform.
    let go_open = fake.opens().into_iter().find(|o| o.kind == "go/1");
    assert!(
        go_open.is_some_and(|o| o.platform.contains('_')),
        "{:?}",
        fake.opens()
    );
    let close = fake
        .closes()
        .into_iter()
        .find(|c| c.kind == "go/1")
        .expect("a Go close");
    assert!(!close.manifest.is_empty());
    // Go's own cache holds what was built: plain `go` compiles nothing (F3, F9).
    assert_eq!(
        a.plain_go_compiles(&["build", "-o", "/dev/null", "./cmd/app"]),
        0
    );

    // B: another checkout, another store, another Go cache.
    let b = Machine::new(dir.path(), "b", Some(&a.repo), &[]);
    let (code, _, stderr) = b.run(&fake, "go build -o ../app ./cmd/app && ../app");
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("1\n"), "{stderr}");
    let line = go_line(&stderr);
    assert!(line.contains("from the shared store"), "{stderr}");
    // What B reused stays in its store, and is not in its Go cache: a later plain `go` there
    // compiles what hushrun served (Q1: a download never goes into Go's own cache).
    let opens_before = fake.count(route::OPEN);
    let (code, _, stderr) = b.run(&fake, "go build -o ../app ./cmd/app");
    assert_eq!(code, 0, "{stderr}");
    assert!(fake.count(route::OPEN) > opens_before);
}

// covers: B-36, B-41
#[test]
fn inside_hushrun_run_go_reads_the_persons_own_warm_cache_and_audits_nothing_there() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    // Plain Go first: a warm cache of the person's.
    assert!(a.plain_go_compiles(&["build", "-o", "/dev/null", "./cmd/app"]) > 0);
    // F3: the first run inside hushrun is as warm as plain Go, with the audit at its default.
    let started = Instant::now();
    let (code, _, stderr) = a.run_with(&fake, "go build -x -o /dev/null ./cmd/app", |cmd| {
        cmd.env_remove("HUSHRUN_BUILD_AUDIT");
    });
    let took = started.elapsed();
    assert_eq!(code, 0, "{stderr}");
    let compiles = stderr
        .lines()
        .filter(|l| l.contains("/compile ") && l.contains(" -o "))
        .count();
    assert_eq!(compiles, 0, "{stderr}");
    assert!(took < Duration::from_secs(20), "{took:?}");
    assert_eq!(go_blocks(&fake), 0, "nothing was built, so nothing goes up");
}

// covers: B-40
#[test]
fn a_cgo_package_and_assembly_including_a_header_from_elsewhere_stay_on_the_machine() {
    if !have_go() {
        return;
    }
    let arch = std::env::consts::ARCH;
    let (goarch, mov, reg) = match arch {
        "aarch64" => ("arm64", "MOVD", "R0"),
        "x86_64" => ("amd64", "MOVQ", "AX"),
        _ => return,
    };
    // F1: assembly whose header lies outside its package's folder: Go's id holds the include's
    // text, not the header's bytes.
    let asm = format!(
        "#include \"textflag.h\"\n#include \"../inc/val.h\"\n\nTEXT ·V(SB),NOSPLIT,$0-8\n\t{mov} $VAL, {reg}\n\t{mov} {reg}, ret+0(FP)\n\tRET\n"
    );
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let files = [
        ("inc/val.h", "#define VAL 1\n"),
        ("p/p.go", "package p\n\nfunc V() int\n"),
        (
            "cmd/asm/main.go",
            "package main\n\nimport \"example.com/m/p\"\n\nfunc main() { println(p.V()) }\n",
        ),
    ];
    let a = Machine::new(dir.path(), "a", None, &files);
    std::fs::write(a.repo.join(format!("p/p_{goarch}.s")), &asm).unwrap();
    let (code, _, stderr) = a.run(&fake, "go build -o ../asm ./cmd/asm && ../asm");
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("assembly outside the standard library"),
        "{stderr}"
    );
    // B has another header under the same path: it must print its own value.
    let b = Machine::new(dir.path(), "b", Some(&a.repo), &[]);
    std::fs::write(b.repo.join(format!("p/p_{goarch}.s")), &asm).unwrap();
    std::fs::write(b.repo.join("inc/val.h"), "#define VAL 2\n").unwrap();
    let (code, _, stderr) = b.run(&fake, "go build -o ../asm ./cmd/asm && ../asm");
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.lines().any(|l| l.trim() == "2"),
        "B printed A's value: {stderr}"
    );
}

// covers: B-40
#[test]
fn a_build_through_toolexec_uploads_nothing() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    let tx = a.root.join("tx.sh");
    std::fs::write(&tx, "#!/bin/sh\nexec \"$@\"\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tx, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (code, _, stderr) = a.run(
        &fake,
        &format!("go build -toolexec={} -o /dev/null ./cmd/app", tx.display()),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(go_blocks(&fake), 0, "{stderr}");
    assert!(stderr.contains("the go command was altered"), "{stderr}");
}

// covers: B-40
#[test]
fn a_compile_that_printed_stays_here_and_the_other_machine_prints_it_too() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    let build = "go build -gcflags=example.com/m/lib=-m -o /dev/null ./cmd/app";
    let (code, _, stderr) = a.run(&fake, build);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("can inline F"), "{stderr}");
    assert!(stderr.contains("the compile printed something"), "{stderr}");
    let b = Machine::new(dir.path(), "b", Some(&a.repo), &[]);
    let (code, _, stderr) = b.run(&fake, build);
    assert_eq!(code, 0, "{stderr}");
    // F4: B was served the runtime from the shared store, never `lib`, whose report it prints.
    assert!(stderr.contains("can inline F"), "{stderr}");
}

// covers: B-36
#[test]
fn a_cache_program_the_person_set_stays_and_none_is_set_outside_a_module() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    // F12: set in the user's go env file, where `go env` reads it.
    let goenv = a.root.join("goenv");
    std::fs::write(&goenv, "GOCACHEPROG=/their/cache-program\n").unwrap();
    let (code, stdout, stderr) = a.run_with(&fake, "go env GOCACHEPROG", |cmd| {
        cmd.env("GOENV", &goenv);
    });
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "/their/cache-program", "{stderr}");
    // In the environment.
    let (code, stdout, stderr) = a.run_with(&fake, "printf %s \"$GOCACHEPROG\"", |cmd| {
        cmd.env("GOCACHEPROG", "their-program --flag");
    });
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "their-program --flag");
    // A folder with no go.mod: nothing is set.
    let elsewhere = a.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut cmd = a.hushrun(&fake);
    cmd.current_dir(&elsewhere).args([
        "run",
        "env",
        "--force",
        "--",
        "sh",
        "-c",
        "printf %s \"$GOCACHEPROG\"",
    ]);
    let output = cmd.output().unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
}

// covers: B-36
#[test]
fn a_store_under_a_folder_with_a_space_still_builds() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    let (code, _, stderr) = a.run_with(&fake, "go build -o /dev/null ./cmd/app", |cmd| {
        cmd.env("HUSHRUN_CACHE_DIR", a.root.join("Jane Doe").join("cache"));
    });
    assert_eq!(code, 0, "{stderr}");
    assert!(go_line(&stderr).contains("built"), "{stderr}");
}

// covers: B-37
#[test]
fn a_hung_or_panicking_handler_never_stalls_or_fails_go() {
    if !have_go() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    let bin = a.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let program = bin.join("hushrun-gocache");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_hushrun"), &program).unwrap();
    let mut cmd = Command::new("go");
    a.go_env(&mut cmd);
    let started = Instant::now();
    let output = cmd
        .current_dir(&a.repo)
        .args(["run", "./cmd/app"])
        .env("GOCACHEPROG", &program)
        .env("HUSHRUN_GOCACHE_BUDGET_MS", "1500")
        .env(
            "HUSHRUN_GOCACHE_FAULT",
            "hang-get=3,panic-get=7,panic-put=5",
        )
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "1");
    assert!(started.elapsed() < Duration::from_secs(60));
}

// covers: B-41
#[test]
fn go_clean_cache_inside_hushrun_run_clears_what_the_program_kept() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    // F9: Go's package index is keyed by a file's size and date, not its contents.
    let files = [
        ("x/base.go", "package x\n\nvar X = 1\n"),
        (
            "x/f2.go",
            "//go:build xx\n\npackage x\n\nfunc init() { X = 2 }\n",
        ),
        (
            "cmd/x/main.go",
            "package main\n\nimport \"example.com/m/x\"\n\nfunc main() { println(x.X) }\n",
        ),
    ];
    let a = Machine::new(dir.path(), "a", None, &files);
    let script = "go build -o ../x ./cmd/x && ../x \
        && go clean -cache \
        && cp x/f2.go ../f2.was \
        && printf '//go:build !x\\n\\npackage x\\n\\nfunc init() { X = 2 }\\n' > x/f2.go \
        && touch -r ../f2.was x/f2.go \
        && go build -o ../x ./cmd/x && ../x";
    let (code, _, stderr) = a.run(&fake, script);
    assert_eq!(code, 0, "{stderr}");
    let printed: Vec<&str> = stderr
        .lines()
        .filter(|l| matches!(l.trim(), "1" | "2"))
        .collect();
    assert_eq!(printed, vec!["1", "2"], "{stderr}");
}
