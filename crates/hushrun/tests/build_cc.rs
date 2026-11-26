//! C and C++ build blocks shared between machines (B-59, B-60), end to end: `hushrun run NAME --
//! COMMAND` on two machines (two checkouts at different paths, two stores) against the fake build
//! protocol, COMMAND calling the system's C compiler through the launcher as CMake's indirection
//! does. They need a C compiler (`cc`), and say so and pass where it is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use hushrun_testkit::fake_build::FakeBuild;
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
    "HUSHRUN_BUILD_SHARE",
    "HUSHRUN_BUILD_AUDIT",
    "HUSHRUN_CC",
    "HUSHRUN_CC_RELAY_WAIT_MS",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "RUSTC_WRAPPER",
    "SOURCE_DATE_EPOCH",
    "CPATH",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "DEPENDENCIES_OUTPUT",
    "SUNPRO_DEPENDENCIES",
    "CC_PRINT_HEADERS",
    "CC_PRINT_HEADERS_FILE",
    "CC_PRINT_HEADERS_FORMAT",
    "CC_PRINT_HEADERS_FILTERING",
    "CC",
    "CFLAGS",
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

/// Whether a C compiler is here; else the test passes with a note, unless
/// `HUSHRUN_TEST_TOOLS=required`.
fn have_cc() -> bool {
    let works = Command::new("cc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if works {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("no C compiler (`cc`), and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: no C compiler (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

/// The sources: three units that include one header, each printing where it is from
/// (`__FILE__`, which the launcher's prefix map makes the same at any path), and a warning.
const SOURCES: &[&str] = &["one", "two", "three"];

fn project(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    let write = |path: &str, text: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "CMakeLists.txt",
        "cmake_minimum_required(VERSION 3.17)\nproject(p C)\n",
    );
    write(
        "include/p.h",
        "#define P_VALUE 7\nconst char *p_where(void);\n",
    );
    for name in SOURCES {
        write(
            &format!("src/{name}.c"),
            &format!(
                "#include \"p.h\"\nconst char *{name}_where(void) {{ int unused; return __FILE__; }}\nint {name}(void) {{ return P_VALUE; }}\n"
            ),
        );
    }
    write(".gitignore", "build/\n");
    git::init(&repo, "sha1");
    git::commit_all(&repo, "init");
    repo
}

/// A machine: its own checkout, hushrun cache (and so store) and machine key.
struct Machine {
    root: PathBuf,
    repo: PathBuf,
}

impl Machine {
    fn new(dir: &Path, name: &str, from: Option<&Path>) -> Machine {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let repo = match from {
            None => project(&root),
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

    fn hushrun(&self, fake: &FakeBuild) -> Command {
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
    fn run(&self, fake: &FakeBuild, script: &str, env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = self.hushrun(fake);
        cmd.args(["run", "build", "--force", "--", "sh", "-c", script]);
        for (name, value) in env {
            cmd.env(name, value);
        }
        let output = cmd.output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    /// Builds every source through the launcher, as CMake's indirection calls it.
    fn build(&self, fake: &FakeBuild, env: &[(&str, &str)]) -> (i32, String) {
        self.run(fake, &compile_script(), env)
    }

    fn object(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.repo.join("build").join(format!("{name}.o"))).unwrap()
    }

    fn store(&self) -> PathBuf {
        self.root.join("cache").join("build")
    }
}

/// A script that compiles each source into `build/` through `$HUSHRUN_CC`, with a depfile.
fn compile_script() -> String {
    let mut script = String::from("set -e; rm -rf build; mkdir -p build");
    for name in SOURCES {
        script.push_str(&format!(
            "; ${{HUSHRUN_CC:?}} cc -Iinclude -Wall -O1 -MD -MF build/{name}.o.d -c src/{name}.c -o build/{name}.o"
        ));
    }
    script
}

/// The C and C++ line of a run's summary.
fn cc_line(stderr: &str) -> String {
    stderr
        .lines()
        .find(|l| l.contains("build: c/c++: ") && l.contains(" objects: "))
        .unwrap_or_default()
        .to_string()
}

fn cc_blocks(fake: &FakeBuild) -> usize {
    fake.begins()
        .iter()
        .filter(|b| b.kind == "cc/1")
        .map(|b| b.blocks.len())
        .sum()
}

/// A machine's C blocks: each block's folder and the source its description names.
fn cc_blocks_in(store: &Path) -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "block.json") {
                let text = std::fs::read_to_string(&p).unwrap();
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                let description = &value["description"];
                if description["kind"] == "cc/1" {
                    out.push((
                        p.parent().unwrap().to_path_buf(),
                        description["source"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    ));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&store.join("blocks"), &mut out);
    out
}

/// Puts `bytes` in place of the object of the block of `source` in `store`; with `record`, its
/// record names their size and SHA-256 too.
fn forge(store: &Path, source: &str, bytes: &[u8], record: bool) {
    forge_file(store, source, "object", bytes, record);
}

/// [`forge`], of the block's file `name`.
fn forge_file(store: &Path, source: &str, name: &str, bytes: &[u8], record: bool) {
    let (dir, _) = cc_blocks_in(store)
        .into_iter()
        .find(|(_, s)| s.ends_with(source))
        .unwrap_or_else(|| panic!("no block of {source}"));
    let object = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find_map(|p| {
            let o = p.join(name);
            if o.is_file() {
                Some(o)
            } else if p.file_name().is_some_and(|n| n == name) {
                Some(p)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("the block's {name}"));
    let json = dir.join("block.json");
    let old = std::fs::read(&object).unwrap();
    for p in [
        &object,
        &json,
        &dir,
        &object.parent().unwrap().to_path_buf(),
    ] {
        Command::new("chmod").arg("u+w").arg(p).status().unwrap();
    }
    std::fs::write(&object, bytes).unwrap();
    if record {
        let sha = |b: &[u8]| hushrun_core::Digest::of(b).hex();
        let text = std::fs::read_to_string(&json).unwrap();
        assert!(text.contains(&sha(&old)), "{text}");
        let text = text.replace(&sha(&old), &sha(bytes)).replace(
            &format!("\"size\":{}", old.len()),
            &format!("\"size\":{}", bytes.len()),
        );
        std::fs::write(&json, text).unwrap();
    }
}

/// A and B, B served all of A's objects once from the shared store.
fn two_machines(dir: &Path, fake: &FakeBuild) -> (Machine, Machine) {
    let a = Machine::new(dir, "a", None);
    let (code, stderr) = a.build(fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(cc_line(&stderr).contains("3 built"), "{stderr}");
    assert_eq!(cc_blocks(fake), 3, "{stderr}");
    // What a download costs is weighed against what the compile took: these take milliseconds.
    fake.set_costs(60_000);
    let b = Machine::new(dir, "b", Some(&a.repo));
    let (code, stderr) = b.build(fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        cc_line(&stderr).contains("3 reused (3 from the shared store)"),
        "{stderr}"
    );
    (a, b)
}

// covers: B-59, B-60
#[test]
fn objects_one_machine_built_are_reused_byte_for_byte_by_another_at_another_path() {
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = two_machines(dir.path(), &fake);
    assert_ne!(a.repo, b.repo);
    for name in SOURCES {
        assert_eq!(a.object(name), b.object(name), "{name}.o");
    }
    // A `cc/1` open and close of their own, with the compiler's target triple.
    let close = fake
        .closes()
        .into_iter()
        .find(|c| c.kind == "cc/1")
        .expect("a cc close");
    assert_eq!(close.manifest.len(), 3);
    assert!(
        fake.opens()
            .iter()
            .any(|o| o.kind == "cc/1" && o.platform == close.platform)
    );
    // B's depfile names B's own files, which B hashed.
    let depfile = std::fs::read_to_string(b.repo.join("build/one.o.d")).unwrap();
    assert!(
        depfile.contains(&b.repo.join("include/p.h").display().to_string())
            || depfile.contains("include/p.h"),
        "{depfile}"
    );
    assert!(
        !depfile.contains(&a.repo.display().to_string()),
        "{depfile}"
    );
    // The warning the compile printed on A reaches the person on B.
    let (code, stderr) = b.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("unused"), "{stderr}");
    assert!(cc_line(&stderr).contains("3 reused"), "{stderr}");
}

// covers: B-59
#[test]
fn a_forged_download_is_rejected_suspended_and_compiled_whatever_is_wrong_with_it() {
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = two_machines(dir.path(), &fake);
    let honest = a.object("one");
    // Wrong SHA-256: the bytes changed, the record didn't.
    let mut changed = honest.clone();
    let last = changed.len() - 1;
    changed[last] ^= 0xff;
    forge(&b.store(), "src/one.c", &changed, false);
    // Wrong format: garbage under a matching record.
    forge(&b.store(), "src/two.c", b"#!/bin/sh\necho owned\n", true);
    // Wrong machine: a relocatable object of another architecture, under a matching record.
    let mut other = a.object("three");
    if other.starts_with(b"\x7fELF") {
        let machine = if other[18] == 62 { 183u16 } else { 62u16 };
        other[18..20].copy_from_slice(&machine.to_le_bytes());
    } else {
        // Mach-O: arm64 and x86_64 swapped.
        let cpu = u32::from_le_bytes([other[4], other[5], other[6], other[7]]);
        let swapped: u32 = if cpu == 0x0100_000c {
            0x0100_0007
        } else {
            0x0100_000c
        };
        other[4..8].copy_from_slice(&swapped.to_le_bytes());
    }
    forge(&b.store(), "src/three.c", &other, true);
    let reports_before = fake.reports().len();
    let (code, stderr) = b.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(cc_line(&stderr).contains("0 reused, 3 built"), "{stderr}");
    for name in SOURCES {
        assert_eq!(a.object(name), b.object(name), "{name}.o is a compile's");
    }
    let reports: Vec<_> = fake.reports().into_iter().skip(reports_before).collect();
    assert_eq!(reports.len(), 3, "{reports:?}");
    assert!(
        reports
            .iter()
            .all(|r| r.kind == "cc/1"
                && matches!(r.problem, hushrun_core::blocks::Problem::Rejected)),
        "{reports:?}"
    );
    // Suspended for the kept days: the next run compiles all three again, and neither reuses
    // nor fetches what it compiled.
    let (code, stderr) = b.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(cc_line(&stderr).contains("0 reused, 3 built"), "{stderr}");
}

// covers: B-59
#[test]
fn an_audit_of_a_download_that_differs_reports_it_and_serves_the_compile() {
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = two_machines(dir.path(), &fake);
    // A well-formed object of the right machine, of other code: only the audit can tell.
    forge(&b.store(), "src/one.c", &a.object("two"), true);
    let reports_before = fake.reports().len();
    let (code, stderr) = b.build(&fake, &[("HUSHRUN_BUILD_AUDIT", "every")]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(a.object("one"), b.object("one"));
    let line = cc_line(&stderr);
    assert!(
        line.contains("3 compared with their blocks, 1 differed"),
        "{stderr}"
    );
    let reports: Vec<_> = fake.reports().into_iter().skip(reports_before).collect();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].kind, "cc/1");
    assert!(matches!(
        reports[0].problem,
        hushrun_core::blocks::Problem::Differs
    ));
}

// covers: B-58, B-60
#[test]
fn a_frozen_relay_costs_a_build_one_wait_and_every_unit_compiles() {
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    // C: nothing of its own, so every unit would ask the relay. Each unit is timed by marks it
    // leaves on either side of its compile, by their modification times, so that the test
    // counts the units that waited for the relay, whatever a compile takes on this machine.
    const WAIT: Duration = Duration::from_millis(4000);
    let c = Machine::new(dir.path(), "c", Some(&a.repo));
    let mut script = String::from("sleep 3; set -e; rm -rf build; mkdir -p build");
    for name in SOURCES {
        script.push_str(&format!(
            "; : > ../{name}.start; ${{HUSHRUN_CC:?}} cc -Iinclude -Wall -O1 -MD -MF build/{name}.o.d -c src/{name}.c -o build/{name}.o; : > ../{name}.end"
        ));
    }
    script.push_str("; : > ../t1");
    let mut cmd = c.hushrun(&fake);
    cmd.args(["run", "build", "--force", "--", "sh", "-c", &script])
        .env("HUSHRUN_CC_RELAY_WAIT_MS", WAIT.as_millis().to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn().unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    // Frozen, not gone: the relay is a thread of hushrun's own process.
    let pid = child.id().to_string();
    Command::new("kill").args(["-STOP", &pid]).status().unwrap();
    let t1 = c.root.join("t1");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !t1.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    Command::new("kill").args(["-CONT", &pid]).status().unwrap();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(t1.exists(), "the build hung with a frozen relay: {stderr}");
    let mark = |n: String| {
        std::fs::metadata(c.root.join(n))
            .unwrap()
            .modified()
            .unwrap()
    };
    let took: Vec<Duration> = SOURCES
        .iter()
        .map(|name| {
            mark(format!("{name}.end"))
                .duration_since(mark(format!("{name}.start")))
                .unwrap_or_default()
        })
        .collect();
    // One wait: the first unit's question and the quick one after it go unanswered, and no
    // unit asks the relay again. A unit that waited took the wait at least, however fast the
    // machine; one that didn't takes a compile, a fraction of it however slow.
    assert!(took[0] >= WAIT, "{took:?}: {stderr}");
    let waited = took.iter().filter(|t| **t >= WAIT).count();
    assert_eq!(waited, 1, "{took:?}: {stderr}");
    for name in SOURCES {
        assert_eq!(a.object(name), c.object(name), "{name}.o");
    }
}

// covers: B-59
#[test]
fn a_downloaded_block_s_messages_reach_the_person_with_their_colour_alone() {
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (_a, b) = two_machines(dir.path(), &fake);
    let forged = b"\x1b]0;owned\x07\x1b[1;35mwarning:\x1b[0m unused\x1b[2J\x1b[5;5H\x07\n";
    forge_file(&b.store(), "src/one.c", "stderr", forged, true);
    let (code, stderr) = b.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(cc_line(&stderr).contains("3 reused"), "{stderr}");
    assert!(
        stderr.contains("]0;owned\x1b[1;35mwarning:\x1b[0m unused[2J[5;5H\n"),
        "{stderr:?}"
    );
    assert!(
        !stderr.contains('\x07') && !stderr.contains("\x1b]"),
        "{stderr:?}"
    );
}

// covers: B-60
#[test]
fn a_run_that_reuses_every_object_closes_a_manifest_naming_them() {
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    let after_a = fake
        .closes()
        .into_iter()
        .filter(|c| c.kind == "cc/1")
        .count();
    let (code, stderr) = a.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(cc_line(&stderr).contains("3 reused"), "{stderr}");
    let closes: Vec<_> = fake
        .closes()
        .into_iter()
        .filter(|c| c.kind == "cc/1")
        .collect();
    assert!(
        closes.len() > after_a,
        "a run that reused all three closed nothing"
    );
    assert_eq!(closes.last().unwrap().manifest.len(), 3);
}

// covers: B-60
#[test]
fn an_object_changed_in_an_incremental_build_reaches_another_machine() {
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    std::fs::write(
        a.repo.join("src/one.c"),
        "#include \"p.h\"\nconst char *one_where(void) { int unused; return __FILE__; }\nint one(void) { return P_VALUE + 1; }\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "change one");
    let (code, stderr) = a.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    fake.set_costs(60_000);
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let (code, stderr) = b.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        cc_line(&stderr).contains("3 reused (3 from the shared store)"),
        "{stderr}"
    );
}

// covers: B-59
#[test]
fn an_audit_that_finds_the_same_bytes_under_a_key_tied_here_reports_nothing() {
    // B's root is in the object (a short path, as a container's), so B's compile is tied and
    // its key isn't the download's: the bytes are the same, and the download is honest.
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let b_root = std::fs::canonicalize(dir.path())
        .unwrap()
        .join("b")
        .join("checkout");
    let a = Machine::new(dir.path(), "a", None);
    std::fs::write(
        a.repo.join("src/one.c"),
        format!(
            "#include \"p.h\"\nconst char *one_where(void) {{ return \"{}\"; }}\nint one(void) {{ return P_VALUE; }}\n",
            b_root.display()
        ),
    )
    .unwrap();
    git::commit_all(&a.repo, "literal");
    let (code, stderr) = a.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    fake.set_costs(60_000);
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    assert_eq!(b.repo, b_root);
    let before = fake.reports().len();
    let (code, stderr) = b.build(&fake, &[("HUSHRUN_BUILD_AUDIT", "every")]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(a.object("one"), b.object("one"));
    let reports: Vec<_> = fake.reports().into_iter().skip(before).collect();
    assert!(
        reports.is_empty(),
        "an honest block was reported: {reports:?}"
    );
}

// covers: B-59
#[test]
fn a_download_s_messages_stay_filtered_after_an_audit_agrees_on_its_object() {
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (_a, b) = two_machines(dir.path(), &fake);
    let forged = b"\x1b]0;owned\x07warning: forged\n";
    forge_file(&b.store(), "src/one.c", "stderr", forged, true);
    let (code, stderr) = b.build(&fake, &[("HUSHRUN_BUILD_AUDIT", "every")]);
    assert_eq!(code, 0, "{stderr}");
    let (code, stderr) = b.build(&fake, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("\x1b]0;owned"), "{stderr:?}");
}

// covers: B-59
#[test]
fn an_uploader_s_control_sequences_are_never_served_raw_after_an_audit() {
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v cc"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    // A compiler that writes a window title into its messages when asked to.
    let script = format!(
        "#!/bin/sh\n{} \"$@\"; s=$?\ncase \"$*\" in *-MD*) [ -n \"$EVIL\" ] && printf '\\033]0;owned\\007forged\\n' >&2;; esac\nexit $s\n",
        real.trim()
    );
    let cc = bin.join("cc");
    std::fs::write(&cc, script).unwrap();
    Command::new("chmod").arg("+x").arg(&cc).status().unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake, &[("PATH", &path), ("EVIL", "1")]);
    assert_eq!(code, 0, "{stderr}");
    fake.set_costs(60_000);
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    for audit in ["none", "every", "none"] {
        let (code, stderr) = b.build(&fake, &[("PATH", &path), ("HUSHRUN_BUILD_AUDIT", audit)]);
        assert_eq!(code, 0, "{stderr}");
        assert!(!stderr.contains("\x1b]0;owned"), "{audit}: {stderr:?}");
    }
}

// covers: B-56, B-60
#[test]
fn a_root_in_a_wide_string_keeps_the_object_on_the_machine() {
    if !have_cc() {
        return;
    }
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    std::fs::write(
        a.repo.join("src/one.c"),
        "#include <stddef.h>\nconst wchar_t *one_where(void) { return DATA_DIR; }\nint one(void) { return 1; }\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "wide");
    let script = "set -e; rm -rf build; mkdir -p build; ${HUSHRUN_CC:?} cc \"-DDATA_DIR=L\\\"$PWD/data\\\"\" -MD -MF build/one.o.d -c src/one.c -o build/one.o";
    let (code, stderr) = a.run(&fake, script, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("stays on this machine (1 tied)"),
        "{stderr}"
    );
    assert_eq!(cc_blocks(&fake), 0, "uploaded: {stderr}");
}

/// Not a check: a fake build backend for a measurement between two containers that share a
/// network. `HUSHRUN_CC_SERVE` names a folder: the fake's address goes in `url`, a token in
/// `token` and a machine key in `machine.pem`; what it holds and moved goes in `moved`, written
/// whole every second, until a file `done` appears there. Run with `--ignored --nocapture`.
#[test]
#[ignore]
fn serve_a_fake_backend_for_a_measurement() {
    let Some(dir) = std::env::var_os("HUSHRUN_CC_SERVE").map(PathBuf::from) else {
        return;
    };
    let fake = FakeBuild::start();
    std::fs::write(
        dir.join("machine.pem"),
        hushrun_testkit::world::test_key(7).to_pem().as_str(),
    )
    .unwrap();
    std::fs::write(dir.join("token"), FakeBuild::TOKEN).unwrap();
    std::fs::write(dir.join("url"), fake.url()).unwrap();
    loop {
        let stored = fake.stored();
        let moved = serde_json::json!({
            "cc_begun": cc_blocks(&fake),
            "stored": stored.len(),
            "stored_bytes": stored.iter().map(|b| b.bytes).sum::<u64>(),
            "served_bytes": fake.served_bytes(),
            "lookups": fake.count(hushrun_testkit::fake_build::route::LOOKUP),
            "hint_lookups": fake.count(hushrun_testkit::fake_build::route::HINTS_LOOKUP),
            "hints": fake.count(hushrun_testkit::fake_build::route::HINTS),
            "opens": fake.opens().iter().map(|o| format!("{} {}", o.kind, o.platform)).collect::<Vec<_>>(),
            "closes": fake.closes().iter().map(|c| format!("{} {} {}", c.kind, c.platform, c.manifest.len())).collect::<Vec<_>>(),
            "requests": fake.seen().len(),
        });
        let temp = dir.join("moved.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(&moved).unwrap()).unwrap();
        std::fs::rename(&temp, dir.join("moved")).unwrap();
        if dir.join("done").exists() {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
