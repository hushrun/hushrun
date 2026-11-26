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
    "CLAUDE_CODE_REMOTE",
    "CURSOR_AGENT_SOCKET",
    "COPILOT_AGENT_SESSION_ID",
    "CODEX_PROXY_CERT",
    "HUSHRUN_SANDBOX",
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
    "HUSHRUN_CC",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
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
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CODEX_SANDBOX",
    "CODEX_CI",
    "CODEX_THREAD_ID",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "CURSOR_AGENT",
    "CURSOR_EXTENSION_HOST_ROLE",
    "GEMINI_CLI",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
    "COPILOT_MODEL",
    "COPILOT_ALLOW_ALL",
    "COPILOT_GITHUB_TOKEN",
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

    /// `hushrun run build -- sh -c SCRIPT`: the run ends once every upload it built went up
    /// (ADR 0144), so the other machine's reuse never depends on how fast this one was.
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
        cmd.args(["run", "build", "--force", "--", "sh", "-c", script]);
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

// covers: B-39, B-41, B-45
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

// covers: B-39, B-41
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

// covers: B-42
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

// covers: B-42
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

// covers: B-42
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

// covers: B-39, B-63
#[test]
fn go_s_cache_program_is_set_only_where_a_run_shares_or_reads_a_baked_store() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    let program = |change: &dyn Fn(&mut Command)| {
        let (code, stdout, stderr) =
            a.run_with(&fake, "printf %s \"$GOCACHEPROG\"", |cmd| change(cmd));
        assert_eq!(code, 0, "{stderr}");
        stdout
    };
    // Neither sharing nor a baked store: Go runs as plain go does, as before 0.19.
    let plain = program(&|cmd| {
        cmd.env("HUSHRUN_BACKEND", "")
            .env("HUSHRUN_BUILD_LOWER", "");
    });
    assert_eq!(plain, "");
    // A store named that the run's state ignores (here an empty folder): as plain go.
    let ignored = dir.path().join("ignored");
    std::fs::create_dir_all(&ignored).unwrap();
    let with_ignored = program(&|cmd| {
        cmd.env("HUSHRUN_BACKEND", "")
            .env("HUSHRUN_BUILD_LOWER", &ignored)
            .env("HUSHRUN_BUILD_LOWER_UNTRUSTED", "1");
    });
    assert_eq!(with_ignored, "");
    // A baked store the run reads, though it shares nothing: the program reads it.
    let baked = dir.path().join("baked");
    empty_go_store(&baked);
    let with_store = program(&|cmd| {
        cmd.env("HUSHRUN_BACKEND", "")
            .env("HUSHRUN_BUILD_LOWER", &baked)
            .env("HUSHRUN_BUILD_LOWER_UNTRUSTED", "1");
    });
    assert!(with_store.contains("hushrun-gocache"), "{with_store:?}");
    // A run that shares: set, as it always was.
    let sharing = program(&|cmd| {
        cmd.env("HUSHRUN_BUILD_LOWER", "");
    });
    assert!(sharing.contains("hushrun-gocache"), "{sharing:?}");
}

// covers: B-39
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

// covers: B-39
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

// covers: B-40
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

/// Plain `go`, with no hushrun, through `sh -c SCRIPT` in the machine's checkout.
fn plain(m: &Machine, script: &str) -> std::process::Output {
    let mut cmd = Command::new("sh");
    m.go_env(&mut cmd);
    cmd.current_dir(&m.repo)
        .args(["-c", script])
        .output()
        .unwrap()
}

/// B, served A's blocks once: two machines that share a store's worth of the runtime and `lib`.
fn two_machines(dir: &Path, fake: &FakeBuild, files: &[(&str, &str)]) -> (Machine, Machine) {
    let a = Machine::new(dir, "a", None, files);
    let (code, _, stderr) = a.run(fake, "go build -o /dev/null ./cmd/app");
    assert_eq!(code, 0, "{stderr}");
    let b = Machine::new(dir, "b", Some(&a.repo), &[]);
    let (code, _, stderr) = b.run(fake, "go build -o /dev/null ./cmd/app");
    assert_eq!(code, 0, "{stderr}");
    assert!(
        go_line(&stderr).contains("from the shared store"),
        "{stderr}"
    );
    (a, b)
}

fn executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// covers: B-42, B-150
#[test]
fn every_package_of_a_pure_go_command_is_reused_on_the_other_machine_runtime_too() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    let (code, _, stderr) = a.run(&fake, "go build -o /dev/null ./cmd/app");
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("stay on this machine"), "{stderr}");
    let b = Machine::new(dir.path(), "b", Some(&a.repo), &[]);
    let (code, _, stderr) = b.run(&fake, "go build -o /dev/null ./cmd/app");
    assert_eq!(code, 0, "{stderr}");
    // On macOS the runtime's header runs past 4096 bytes of `cgo_import_dynamic` lines.
    let line = go_line(&stderr);
    assert!(line.contains("from the shared store"), "{stderr}");
    assert!(!line.contains("built"), "{stderr}");
}

// covers: B-26, B-29, B-45
#[test]
fn every_package_is_reused_on_the_other_machine_however_slowly_the_backend_answers() {
    if !have_go() {
        return;
    }
    // Each pause is longer than a bound a release waits within: A's uploads outlast its end (one
    // PUT at a time would take the run past `end_wait`), B's `open` outlasts the second a Go
    // fetch waits for it, and B's downloads outlast half of what compiling a runtime package
    // takes. A settled relay waits for each to be over, so B reuses every package whatever the
    // pauses, as it would on a machine slower by as much.
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    fake.delay(route::STORE_PUT, Duration::from_millis(1000));
    let (code, _, stderr) = a.run(&fake, "go build -o /dev/null ./cmd/app");
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("for the next run"), "{stderr}");
    fake.delay(route::STORE_PUT, Duration::ZERO);
    fake.delay(route::OPEN, Duration::from_millis(1500));
    fake.delay(route::STORE_GET, Duration::from_millis(300));
    let b = Machine::new(dir.path(), "b", Some(&a.repo), &[]);
    let (code, _, stderr) = b.run(&fake, "go build -o /dev/null ./cmd/app");
    assert_eq!(code, 0, "{stderr}");
    let line = go_line(&stderr);
    assert!(line.contains("from the shared store"), "{stderr}");
    assert!(!line.contains("built"), "{stderr}");
}

// covers: B-44
#[test]
fn a_toolexec_build_in_gos_own_cache_reports_and_suspends_nothing() {
    if !have_go() {
        return;
    }
    // A step that compiles with other flags and passes `-V=full` through, so that Go's action
    // ids are the same: as a coverage or obfuscation step would.
    const TX: &str = "#!/bin/sh\ntool=$1; shift\ncase \"$(basename \"$tool\")\" in\n  compile)\n    for a in \"$@\"; do [ \"$a\" = \"-V=full\" ] && exec \"$tool\" \"$@\"; done\n    exec \"$tool\" -l \"$@\" ;;\nesac\nexec \"$tool\" \"$@\"\n";
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (_, b) = two_machines(dir.path(), &fake, &[]);
    let tx = b.root.join("tx.sh");
    executable(&tx, TX);
    let out = plain(
        &b,
        &format!("go build -toolexec={} -o /dev/null ./cmd/app", tx.display()),
    );
    assert!(out.status.success(), "{out:?}");
    let reports_before = fake.reports().len();
    let (code, _, stderr) = b.run(
        &fake,
        "go build -o /dev/null ./cmd/app && go build -o /dev/null ./cmd/app",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(fake.reports().len(), reports_before, "{stderr}");
    // Counted, apart from what the audit compares.
    assert!(
        stderr.contains("in Go's own cache with other bytes"),
        "{stderr}"
    );
    // And B's blocks are still served once Go's own cache is gone: no key was suspended.
    assert!(plain(&b, "go clean -cache").status.success());
    let (code, _, stderr) = b.run(&fake, "go build -o /dev/null ./cmd/app");
    assert_eq!(code, 0, "{stderr}");
    let line = go_line(&stderr);
    assert!(
        line.contains("reused") && !line.contains("built"),
        "{stderr}"
    );
}

// covers: B-42
#[test]
fn a_forged_archive_in_the_store_is_reported_suspended_and_compiled() {
    use hushrun_build::go::archive;
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = two_machines(dir.path(), &fake, &[]);
    let export_of = |m: &Machine| -> Vec<u8> {
        let out = plain(m, "go list -export -f '{{.Export}}' ./lib");
        assert!(out.status.success(), "{out:?}");
        std::fs::read(String::from_utf8_lossy(&out.stdout).trim()).unwrap()
    };
    // `lib` with V = 2, built elsewhere, under the honest archive's build ID.
    let z = Machine::new(
        dir.path(),
        "z",
        None,
        &[(
            "lib/lib.go",
            "package lib\n\nconst V = 2\n\nfunc F() int { return V * 1 }\n",
        )],
    );
    let honest = export_of(&a);
    let variant = export_of(&z);
    let honest_id = archive::build_id(&honest).unwrap();
    let variant_id = archive::build_id(&variant).unwrap();
    assert_eq!(honest_id.len(), variant_id.len());
    let mut forged = variant.clone();
    let needle = variant_id.as_bytes();
    let mut i = 0;
    while i + needle.len() <= forged.len() {
        if &forged[i..i + needle.len()] == needle {
            forged[i..i + needle.len()].copy_from_slice(honest_id.as_bytes());
            i += needle.len();
        } else {
            i += 1;
        }
    }
    // In B's store in place of the lib it downloaded, with a record that matches.
    fn files_named(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                files_named(&p, name, out);
            } else if p.file_name().is_some_and(|n| n == name) {
                out.push(p);
            }
        }
    }
    let mut archives = Vec::new();
    files_named(&b.root.join("cache"), "archive", &mut archives);
    let target = archives
        .into_iter()
        .find(|p| archive::build_id(&std::fs::read(p).unwrap()).as_deref() == Some(&honest_id))
        .expect("B's store holds A's lib");
    let old = std::fs::read(&target).unwrap();
    let json = target.parent().unwrap().join("block.json");
    let json = if json.exists() {
        json
    } else {
        target
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("block.json")
    };
    let sha = |b: &[u8]| hushrun_core::Digest::of(b).hex();
    let text = std::fs::read_to_string(&json).unwrap();
    assert!(text.contains(&sha(&old)), "{text}");
    let text = text.replace(&sha(&old), &sha(&forged)).replace(
        &format!("\"size\":{}", old.len()),
        &format!("\"size\":{}", forged.len()),
    );
    for p in [&target, &json] {
        Command::new("chmod").arg("u+w").arg(p).status().unwrap();
        Command::new("chmod")
            .arg("u+w")
            .arg(p.parent().unwrap())
            .status()
            .unwrap();
    }
    std::fs::write(&target, &forged).unwrap();
    std::fs::write(&json, text).unwrap();
    assert!(plain(&b, "go clean -cache").status.success());
    let reports_before = fake.reports().len();
    let (code, _, stderr) = b.run(&fake, "go build -o ../app ./cmd/app && ../app");
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.lines().any(|l| l.trim() == "1") && !stderr.lines().any(|l| l.trim() == "2"),
        "{stderr}"
    );
    let reports: Vec<_> = fake.reports().into_iter().skip(reports_before).collect();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].kind, "go/1");
    assert!(
        matches!(reports[0].problem, hushrun_core::blocks::Problem::Rejected),
        "{reports:?}"
    );
    // Suspended: with Go's own cache gone again, `lib` is compiled, not downloaded again.
    assert!(plain(&b, "go clean -cache").status.success());
    let (code, _, stderr) = b.run(&fake, "go build -o ../app ./cmd/app && ../app");
    assert_eq!(code, 0, "{stderr}");
    assert!(go_line(&stderr).contains("1 built"), "{stderr}");
}

// covers: B-41
#[test]
fn gos_own_cache_is_trimmed_inside_hushrun_run_as_plain_go_trims_it() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    let plant = || -> PathBuf {
        let shard = a.gocache().join("ff");
        std::fs::create_dir_all(&shard).unwrap();
        let old = shard.join(format!("{}-d", "f".repeat(64)));
        std::fs::write(&old, b"old").unwrap();
        // Ten days old, and no trim.txt: Go trims at the end of its next build.
        let ten_days = std::time::SystemTime::now() - Duration::from_secs(10 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(ten_days)
            .unwrap();
        let _ = std::fs::remove_file(a.gocache().join("trim.txt"));
        old
    };
    assert!(
        plain(&a, "go build -o /dev/null ./cmd/app")
            .status
            .success()
    );
    let old = plant();
    assert!(plain(&a, "go build -o /dev/null ./lib").status.success());
    assert!(!old.exists(), "plain Go trims");
    let old = plant();
    let (code, _, stderr) = a.run(&fake, "go build -o /dev/null ./lib");
    assert_eq!(code, 0, "{stderr}");
    assert!(!old.exists(), "{stderr}");
    let stamp = std::fs::read_to_string(a.gocache().join("trim.txt")).unwrap();
    assert!(stamp.trim().parse::<u64>().is_ok(), "{stamp:?}");
}

// covers: B-39
#[test]
fn another_compiler_wrapper_leaves_go_sharing_on_and_only_rust_out() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let files = [(
        "Cargo.toml",
        "[package]\nname = \"m\"\nversion = \"0.1.0\"\n",
    )];
    let a = Machine::new(dir.path(), "a", None, &files);
    let (code, _, stderr) = a.run(&fake, "go build -o /dev/null ./cmd/app");
    assert_eq!(code, 0, "{stderr}");
    let b = Machine::new(dir.path(), "b", Some(&a.repo), &[]);
    let (code, stdout, stderr) = b.run_with(
        &fake,
        "printf '%s\\n' \"$RUSTC_WRAPPER\"; go build -o /dev/null ./cmd/app",
        |cmd| {
            cmd.env("RUSTC_WRAPPER", "sccache");
        },
    );
    assert_eq!(code, 0, "{stderr}");
    // The person's wrapper passes through, hushrun's stays out.
    assert_eq!(stdout.trim(), "sccache");
    assert!(
        go_line(&stderr).contains("from the shared store"),
        "{stderr}"
    );
    assert_eq!(
        stderr
            .matches("Rust crates aren't reused in this run: another compiler wrapper is set")
            .count(),
        1,
        "{stderr}"
    );
}

// covers: B-40
#[test]
fn a_frozen_relay_costs_a_run_one_wait_not_one_per_go_command() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (a, _) = two_machines(dir.path(), &fake, &[]);
    // A third machine, with nothing of its own yet: every package asks the relay.
    let b = Machine::new(dir.path(), "c", Some(&a.repo), &[]);
    let stamp = |name: &str| format!("date +%s > ../{name}");
    let hires =
        |name: &str| format!("perl -MTime::HiRes=time -e 'printf \"%.3f\\n\", time' > ../{name}");
    let script = format!(
        "{}; sleep 3; {}; go build -o /dev/null ./cmd/app; {}; go build -o /dev/null ./lib; {}; \
         go build -o /dev/null ./cmd/app; {}",
        hires("ts"),
        stamp("t0"),
        stamp("t1"),
        stamp("t2"),
        stamp("t3"),
    );
    let epoch = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    };
    let mut cmd = b.hushrun(&fake);
    cmd.args(["run", "build", "--force", "--", "sh", "-c", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    // DIAGNOSTIC: a group of its own, so that no SIGHUP unfreezes it.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let spawned_at = epoch();
    let child = cmd.spawn().unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    // Frozen, not gone: the relay is a thread of hushrun's own process.
    let pid = child.id().to_string();
    let stopped_at = epoch();
    Command::new("kill").args(["-STOP", &pid]).status().unwrap();
    let t3 = b.root.join("t3");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !t3.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut diag = String::new();
    let ts_frozen = std::fs::read_to_string(b.root.join("ts")).unwrap_or_default();
    diag += &format!(
        "DIAG spawned_at={spawned_at:.3} stopped_at={stopped_at:.3} ts_while_frozen={:?} t0_while_frozen={:?} t3={}\n",
        ts_frozen.trim(),
        std::fs::read_to_string(b.root.join("t0"))
            .unwrap_or_default()
            .trim(),
        t3.exists()
    );
    if !t3.exists() {
        let sh = |c: &str| -> String {
            let o = Command::new("sh").args(["-c", c]).output().unwrap();
            format!(
                "$ {c}\n{}{}\n",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        };
        diag += &sh(&format!(
            "ps -A -o pid,ppid,pgid,stat,etime,command | awk '$3=={pid}'"
        ));
        diag += &sh(&format!(
            "find {} -name 'go-relay-down' -o -name 'relay.sock' 2>/dev/null",
            b.root.display()
        ));
        diag += &sh(&format!(
            "find /tmp /private/tmp /var/folders -name 'relay.sock' -newer {} 2>/dev/null | head",
            b.repo.display()
        ));
        diag += &sh(&format!(
            "for p in $(ps -A -o pid=,pgid=,command= | awk '$1=={pid} || $2=={pid} && ($3 ~ /gocache/ || $3 ~ /go$/ || $3 ~ /bin\\/go/ || $4 == \"build\") {{print $1}}'); do echo \"== $p\"; ps -o pid,stat,command -p $p; sample $p 2 -mayDie 2>&1 | grep -v '^$' | head -150; done"
        ));
        diag += &sh(&format!(
            "lsof -p $(pgrep -g {pid} -f gocache | head -1) 2>&1 | head -40"
        ));
    }
    Command::new("kill").args(["-CONT", &pid]).status().unwrap();
    let out = child.wait_with_output().unwrap();
    let read = |n: &str| -> u64 {
        std::fs::read_to_string(b.root.join(n))
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0)
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr = format!("{stderr}\n{diag}");
    eprintln!("{diag}");
    assert!(t3.exists(), "go hung with a frozen relay: {stderr}");
    // The first command waits for the relay once; the run's later ones don't ask it.
    let (t0, t1, t2, t3) = (read("t0"), read("t1"), read("t2"), read("t3"));
    assert!(t1 - t0 < 60, "{}s: {stderr}", t1 - t0);
    assert!(t2 - t1 < 10, "{}s: {stderr}", t2 - t1);
    assert!(t3 - t2 < 10, "{}s: {stderr}", t3 - t2);
}

/// Not a check: what two machines save on a real module, for the record. `HUSHRUN_GO_MEASURE`
/// names a module's folder (its dependencies already in the module cache `GOMODCACHE` names),
/// `HUSHRUN_GO_MEASURE_BIN` the hushrun binary to measure (else this test's), and
/// `HUSHRUN_GO_MEASURE_FLAGS` Go's flags (default `-trimpath`). Run with `--ignored
/// --nocapture`.
#[test]
#[ignore]
fn measure_two_machines_on_a_real_module() {
    let Some(module) = std::env::var_os("HUSHRUN_GO_MEASURE").map(PathBuf::from) else {
        return;
    };
    let bin = std::env::var_os("HUSHRUN_GO_MEASURE_BIN").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_hushrun")),
        PathBuf::from,
    );
    let flags = std::env::var("HUSHRUN_GO_MEASURE_FLAGS").unwrap_or_else(|_| "-trimpath".into());
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let copy = |name: &str| -> Machine {
        let root = dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        let repo = root.join("src").join(name).join("module");
        std::fs::create_dir_all(repo.parent().unwrap()).unwrap();
        let status = Command::new("cp")
            .arg("-R")
            .arg(&module)
            .arg(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        if !repo.join(".git").exists() {
            git::init(&repo, "sha1");
            git::commit_all(&repo, "init");
        }
        std::fs::write(
            root.join("machine.pem"),
            hushrun_testkit::world::test_key(7).to_pem().as_str(),
        )
        .unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        Machine { root, repo }
    };
    let go = |m: &Machine, cmd: &mut Command| {
        for var in CLEARED {
            if *var != "GOFLAGS" {
                cmd.env_remove(var);
            }
        }
        cmd.env("GOCACHE", m.gocache())
            .env("GOTOOLCHAIN", "local")
            .env("GOENV", "off")
            .env("GOFLAGS", &flags);
    };
    let build = "go build -o /dev/null ./...";
    let timed_plain = |m: &Machine| {
        let mut cmd = Command::new("sh");
        go(m, &mut cmd);
        let started = Instant::now();
        let out = cmd
            .current_dir(&m.repo)
            .args(["-c", build])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        started.elapsed()
    };
    let timed_script = |m: &Machine, audit: &str, build: &str| {
        let mut cmd = Command::new(&bin);
        go(m, &mut cmd);
        cmd.current_dir(&m.repo)
            .env("HUSHRUN_CACHE_DIR", m.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", m.root.join("credentials.json"))
            .env("HUSHRUN_MACHINE_KEY_FILE", m.root.join("machine.pem"))
            .env("HUSHRUN_HOST", "http://127.0.0.1:9")
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HUSHRUN_BACKEND", fake.url())
            .env("HUSHRUN_TOKEN", FakeBuild::TOKEN)
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_BUILD_AUDIT", audit)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", m.root.join("gitconfig"))
            .args(["run", "build", "--force", "--", "sh", "-c", build]);
        let started = Instant::now();
        let out = cmd.output().unwrap();
        let took = started.elapsed();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{stderr}");
        let lines: Vec<String> = stderr
            .lines()
            .filter(|l| l.contains("build: "))
            .map(str::to_string)
            .collect();
        (took, lines)
    };
    let timed_run = |m: &Machine, audit: &str| timed_script(m, audit, build);
    let cold = copy("cold");
    let cold_time = timed_plain(&cold);
    let warm_time = timed_plain(&cold);
    let a = copy("a");
    let (a_time, a_lines) = timed_run(&a, "none");
    let (a_warm, a_warm_lines) = timed_run(&a, "none");
    // Warm, three times each, alternating: plain Go, Go inside `hushrun run`, and `hushrun run`
    // around nothing, which is its own cost.
    for round in 1..=3 {
        let plain = timed_plain(&a);
        let (inside, _) = timed_run(&a, "none");
        let (nothing, _) = timed_script(&a, "none", "true");
        println!(
            "warm round {round}: plain go {plain:?}, inside hushrun run {inside:?}, hushrun run of nothing {nothing:?}"
        );
    }
    let b = copy("b");
    let (b_time, b_lines) = timed_run(&b, "none");
    let c = copy("c");
    let (c_time, c_lines) = timed_run(&c, "every");
    println!("plain go, cold: {cold_time:?}; warm: {warm_time:?}");
    println!("A, cold, hushrun run: {a_time:?} {a_lines:?}");
    println!("A, warm, hushrun run: {a_warm:?} {a_warm_lines:?}");
    println!("B, from A's blocks: {b_time:?} {b_lines:?}");
    println!("C, from A's blocks, every one audited: {c_time:?} {c_lines:?}");
    let go_begins: usize = fake
        .begins()
        .iter()
        .filter(|b| b.kind == "go/1")
        .map(|b| b.blocks.len())
        .sum();
    println!(
        "go/1 blocks begun {go_begins}, stored {}, lookups {}, bytes served {}",
        fake.stored().len(),
        fake.count(route::LOOKUP),
        fake.served_bytes()
    );
}

/// B-48: `hushrun run build -- sh -c SCRIPT` as GitLab job `job` of machine `m`: its ID token
/// from the fake, its job token, GitLab's job variables, the agent token beside them for results,
/// and the machine's own temporary folder.
fn gitlab_go_run(
    m: &Machine,
    fake: &FakeBuild,
    job: i64,
    token: &str,
    script: &str,
) -> (i32, String) {
    let temp = m.root.join("tmp");
    std::fs::create_dir_all(&temp).unwrap();
    let mut cmd = m.hushrun(fake);
    cmd.env("CI", "true")
        .env("GITLAB_CI", "true")
        .env("CI_JOB_ID", job.to_string())
        .env("CI_PIPELINE_ID", "900")
        .env("CI_PROJECT_URL", "https://gitlab.com/acme/shop")
        .env("CI_SERVER_URL", "https://gitlab.com")
        .env("CI_JOB_TOKEN", format!("glcbt-job-token-of-{job}"))
        .env("TMPDIR", &temp)
        .env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN)
        .env("HUSHRUN_ID_TOKEN", token)
        .args(["run", "build", "--force", "--", "sh", "-c", script]);
    let output = cmd.output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// covers: B-45, B-48, B-49
#[test]
fn a_gitlab_job_s_go_requests_are_signed_with_the_job_s_one_key_beside_rust_s() {
    if !have_go() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None, &[]);
    let token = fake.gitlab_token(7101);
    let (code, stderr) = gitlab_go_run(
        &a,
        &fake,
        7101,
        &token,
        "go build -o ../app ./cmd/app && ../app",
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(go_line(&stderr).contains("built"), "{stderr}");
    assert!(go_blocks(&fake) > 0, "nothing went up: {stderr}");
    // Go's `open` and `close` (kind go/1) beside Rust's: every build request carried the job's ID
    // token and one key, and the job stayed bound to it.
    let kinds: Vec<_> = fake.opens().iter().map(|o| o.kind.clone()).collect();
    assert!(kinds.iter().any(|k| k == "go/1"), "{kinds:?}");
    let build: Vec<_> = fake
        .seen()
        .into_iter()
        .filter(|s| s.path.contains("/v0/build/"))
        .collect();
    let key = build[0].job_key.clone().expect("a job key");
    for seen in &build {
        assert_eq!(
            seen.authorization.as_deref(),
            Some(format!("Bearer {token}").as_str()),
            "{}",
            seen.path
        );
        assert_eq!(seen.job_key.as_deref(), Some(key.as_str()), "{}", seen.path);
    }
    let job = &fake.gitlab_jobs()[&7101];
    assert!(!job.withdrawn, "{job:?}");
    assert!(
        job.signed[route::OPEN] >= 2,
        "Rust's and Go's open: {job:?}"
    );
    assert!(
        job.signed[route::CLOSE] >= 2,
        "Rust's and Go's close: {job:?}"
    );
    assert!(
        job.signed.get(route::BEGIN).is_some_and(|n| *n >= 1),
        "{job:?}"
    );
    // The job's next run opens and closes both again with the same key.
    let (code, stderr) = gitlab_go_run(&a, &fake, 7101, &token, "go build -o ../app ./cmd/app");
    assert_eq!(code, 0, "{stderr}");
    assert!(!fake.gitlab_jobs()[&7101].withdrawn, "{stderr}");

    // Another job, on another machine: Go's packages come from the shared store.
    let b = Machine::new(dir.path(), "b", Some(&a.repo), &[]);
    let token = fake.gitlab_token(7102);
    let (code, stderr) = gitlab_go_run(
        &b,
        &fake,
        7102,
        &token,
        "go build -o ../app ./cmd/app && ../app",
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        go_line(&stderr).contains("from the shared store"),
        "{stderr}"
    );
    assert!(!fake.gitlab_jobs()[&7102].withdrawn);
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
