//! Real cargo, end to end: a tiny offline workspace copied to two paths of different length
//! that share one store, built through the wrapper binary (`examples/hushrun-rustc.rs`).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use hushrun_build::model::{Class, Mode, Outcome};
use hushrun_build::session::{self, Entry, NotBegun, Session, Start};
use hushrun_build::settings;

/// The wrapper binary cargo built beside this test.
fn wrapper_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().unwrap().parent().unwrap();
    let wrapper = debug.join("examples").join("hushrun-rustc");
    assert!(wrapper.is_file(), "{} not built", wrapper.display());
    wrapper
}

const FILES: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"a\", \"b\", \"m\", \"c\", \"d\"]\nresolver = \"2\"\n",
    ),
    (
        "a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "a/src/lib.rs",
        "mod x;\n\npub fn text() -> &'static str {\n    include_str!(\"../data/text.txt\")\n}\n\npub fn var() -> &'static str {\n    env!(\"A_VAR\")\n}\n\npub fn twice() -> u32 {\n    let unused = 1;\n    x::two()\n}\n",
    ),
    ("a/src/x.rs", "pub fn two() -> u32 {\n    2\n}\n"),
    ("a/data/text.txt", "first text"),
    (
        "m/Cargo.toml",
        "[package]\nname = \"m\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n",
    ),
    (
        "m/src/lib.rs",
        "use proc_macro::TokenStream;\n\n#[proc_macro]\npub fn make(_: TokenStream) -> TokenStream {\n    \"pub fn made() -> u32 { 7 }\".parse().unwrap()\n}\n",
    ),
    (
        "b/Cargo.toml",
        "[package]\nname = \"b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nm = { path = \"../m\" }\n",
    ),
    (
        "b/build.rs",
        "fn main() {\n    println!(\"cargo:rerun-if-changed=input.txt\");\n    let input = std::fs::read_to_string(\"input.txt\").unwrap();\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::write(\n        format!(\"{out}/gen.rs\"),\n        format!(\"pub const GEN: &str = {input:?};\\n\"),\n    )\n    .unwrap();\n}\n",
    ),
    ("b/input.txt", "generated one"),
    (
        "b/src/lib.rs",
        "m::make!();\ninclude!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n",
    ),
    (
        "d/Cargo.toml",
        "[package]\nname = \"d\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "d/src/lib.rs",
        "pub fn dir() -> &'static str {\n    env!(\"CARGO_MANIFEST_DIR\")\n}\n",
    ),
    (
        "c/Cargo.toml",
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\na = { path = \"../a\" }\nb = { path = \"../b\" }\nd = { path = \"../d\" }\n",
    ),
    (
        "c/src/main.rs",
        "fn main() {\n    println!(\"{}|{}|{}|{}|{}|{}\", a::text(), a::var(), a::twice(), b::made(), b::GEN, d::dir());\n}\n",
    ),
];

/// A temp folder with the settings, the cache and the checkouts.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> World {
        World {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, rest: &str) -> PathBuf {
        let path = self.dir.path().join(rest);
        fs::create_dir_all(&path).unwrap();
        PathBuf::from(hushrun_build::roots::resolve_on_disk(path.to_str().unwrap()).unwrap())
    }

    /// A fresh copy of the workspace at `rest`.
    fn checkout(&self, rest: &str) -> PathBuf {
        let ws = self.path(rest);
        for (file, text) in FILES {
            let path = ws.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        ws
    }

    fn begin(&self, mode: Mode) -> Result<Session, NotBegun> {
        self.begin_with(mode, |_| {})
    }

    /// A session with the settings changed by `change` too.
    fn begin_with(
        &self,
        mode: Mode,
        change: impl FnOnce(&mut settings::Settings),
    ) -> Result<Session, NotBegun> {
        let settings = self.dir.path().join("build.json");
        settings::update(&settings, |s| {
            s.mode = mode;
            s.min_free_gb = 0;
            change(s);
        })
        .unwrap();
        let exe = wrapper_exe();
        let home = std::env::var_os("HOME");
        let cargo_home = std::env::var_os("CARGO_HOME");
        let rustup_home = std::env::var_os("RUSTUP_HOME");
        let env = move |name: &str| match name {
            "HOME" => home.clone(),
            "CARGO_HOME" => cargo_home.clone(),
            "RUSTUP_HOME" => rustup_home.clone(),
            _ => None,
        };
        session::begin(&Start {
            settings: &settings,
            cache_dir: Some(&self.dir.path().join("cache")),
            exe: &exe,
            in_ci: false,
            repository: Some("test/e2e"),
            env: &env,
            now: SystemTime::now(),
        })
    }
}

/// Runs cargo in `ws` with the outer environment cleaned, and `env` on top.
fn cargo(ws: &Path, args: &[&str], env: &[(OsString, OsString)]) -> Output {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.args(args).arg("--offline").current_dir(ws);
    for (name, _) in std::env::vars_os() {
        let Some(name) = name.to_str() else { continue };
        let drop = matches!(
            name,
            "RUSTC_WRAPPER"
                | "RUSTC_WORKSPACE_WRAPPER"
                | "RUSTC"
                | "RUSTFLAGS"
                | "CARGO_ENCODED_RUSTFLAGS"
                | "CARGO_TARGET_DIR"
                | "CARGO_BUILD_TARGET_DIR"
                | "CARGO_MAKEFLAGS"
                | "CARGO_INCREMENTAL"
                | "OUT_DIR"
                | "HUSHRUN_BUILD_SESSION"
                | "HUSHRUN_BUILD"
                | "HUSHRUN_BUILD_DIR"
        ) || (name.starts_with("CARGO_") && name != "CARGO_HOME")
            || name.starts_with("DYLD_");
        if drop {
            command.env_remove(name);
        }
    }
    command.env("A_VAR", "set one");
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn build(ws: &Path, session: &Session) -> Output {
    let out = cargo(ws, &["build"], &session.env());
    assert!(
        out.status.success(),
        "cargo build in {} failed:\n{}",
        ws.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The last entry for a crate of a class.
fn last(entries: &[Entry], name: &str, class: Class) -> Entry {
    entries
        .iter()
        .rev()
        .find(|e| e.crate_name == name && e.class == Some(class))
        .unwrap_or_else(|| panic!("no entry for {name}: {entries:#?}"))
        .clone()
}

fn run_c(ws: &Path) -> String {
    let out = Command::new(ws.join("target/debug/c")).output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

fn rlib(ws: &Path, name: &str) -> Vec<u8> {
    let deps = ws.join("target/debug/deps");
    let file = fs::read_dir(&deps)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            let n = p.file_name().unwrap().to_str().unwrap();
            n.starts_with(&format!("lib{name}-")) && n.ends_with(".rlib")
        })
        .unwrap();
    fs::read(file).unwrap()
}

/// Reused, or picked by the audit (B-16: one library compile in fifty that could be reused runs
/// anyway, chosen by a hash of the key and the run's start) and found the same.
fn reused(entry: &Entry) -> bool {
    entry.outcome == Outcome::Reused
        || (entry.outcome == Outcome::BuiltAndStored && entry.found && !entry.differed)
}

fn entries(session: &Session) -> Vec<Entry> {
    session.current().entries()
}

// covers: B-1, B-8, B-9, B-10, B-12
#[test]
fn a_second_checkout_reuses_what_the_first_built() {
    let w = World::new();
    let one = w.checkout("one");
    let two = w.checkout("a-second-checkout/with-a-longer-path");
    let session = w.begin(Mode::On).unwrap();

    build(&one, &session);
    let first = entries(&session);
    assert_eq!(
        last(&first, "a", Class::Library).outcome,
        Outcome::BuiltAndStored
    );
    assert_eq!(
        last(&first, "d", Class::Library).outcome,
        Outcome::BuiltAndStored
    );

    // (1) The second checkout: a and b are reused, byte for byte; d is tied and rebuilt.
    let out = build(&two, &session);
    let second = entries(&session)[first.len()..].to_vec();
    assert!(reused(&last(&second, "a", Class::Library)), "{second:#?}");
    assert!(reused(&last(&second, "b", Class::Library)), "{second:#?}");
    // The proc macro and the build script, linked with the Apple fixes, move too; the program
    // names its own target folder in its debug map and is tied (macOS).
    assert_eq!(
        last(&second, "m", Class::ProcMacro).outcome,
        Outcome::Reused,
        "{second:#?}"
    );
    assert_eq!(
        last(&second, "build_script_build", Class::BuildScript).outcome,
        Outcome::Reused,
        "{second:#?}"
    );
    eprintln!(
        "program at the second path: {:?}",
        last(&second, "c", Class::Program)
    );
    assert_eq!(rlib(&one, "a"), rlib(&two, "a"));
    assert_eq!(rlib(&one, "b"), rlib(&two, "b"));
    let d = last(&second, "d", Class::Library);
    assert_eq!(d.outcome, Outcome::BuiltAndStored);
    assert!(!d.found);
    let printed = run_c(&two);
    assert_eq!(
        printed.trim(),
        format!(
            "first text|set one|2|7|generated one|{}",
            two.join("d").display()
        )
    );

    // (4) The warning in a, replayed: this checkout's file, never the first's or a placeholder.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unused variable"), "{stderr}");
    assert!(stderr.contains("a/src/lib.rs"), "{stderr}");
    assert!(!stderr.contains(one.to_str().unwrap()), "{stderr}");
    assert!(!stderr.contains("{hushrun:"), "{stderr}");

    // (3) Built again: cargo's own fingerprints hold, so the wrapper isn't called at all.
    let before = entries(&session).len();
    build(&two, &session);
    assert_eq!(
        entries(&session).len(),
        before,
        "{:#?}",
        &entries(&session)[before..]
    );

    // (2) The included file changes here: the program shows the new text.
    fs::write(two.join("a/data/text.txt"), "second text").unwrap();
    build(&two, &session);
    assert!(run_c(&two).starts_with("second text|"));

    // And back, as a branch switched back: the library and the program, tied to this target
    // folder, are both reused.
    let before = entries(&session).len();
    fs::write(two.join("a/data/text.txt"), "first text").unwrap();
    build(&two, &session);
    let back = entries(&session)[before..].to_vec();
    assert!(reused(&last(&back, "a", Class::Library)), "{back:#?}");
    assert_eq!(
        last(&back, "c", Class::Program).outcome,
        Outcome::Reused,
        "{back:#?}"
    );
    assert!(run_c(&two).starts_with("first text|"));

    // (6) A damaged block still gives a green build.
    let store = &session.current().store;
    for file in walk(&store.join("blocks")) {
        if file.extension().is_some_and(|e| e == "rlib") {
            let mut perms = fs::metadata(&file).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            fs::set_permissions(&file, perms).unwrap();
            fs::write(&file, b"truncated").unwrap();
        }
    }
    let three = w.checkout("three");
    build(&three, &session);
    assert!(run_c(&three).starts_with("first text|"));
    session.finish(SystemTime::now());
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// Every output file in a target's deps folder, by name.
fn outputs(ws: &Path) -> BTreeMap<String, String> {
    let deps = ws.join("target/debug/deps");
    fs::read_dir(&deps)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e == "rlib" || e == "rmeta" || e == "dylib")
        })
        .map(|p| {
            (
                p.file_name().unwrap().to_str().unwrap().to_string(),
                hushrun_core::Digest::of(&fs::read(&p).unwrap()).hex(),
            )
        })
        .collect()
}

// covers: B-1, B-2
#[test]
fn off_builds_exactly_as_without_the_wrapper() {
    let w = World::new();
    let ws = w.checkout("ws");
    assert_eq!(w.begin(Mode::Off).unwrap_err(), NotBegun::Off);
    // Incremental compiles are not reproducible even without a wrapper: compare clean ones.
    let clean = (OsString::from("CARGO_INCREMENTAL"), OsString::from("0"));
    let plain = cargo(&ws, &["build"], std::slice::from_ref(&clean));
    assert!(plain.status.success());
    let without = outputs(&ws);
    assert!(cargo(&ws, &["clean"], &[]).status.success());
    // The wrapper with no session: every call runs rustc as asked.
    let session = w.begin(Mode::On).unwrap();
    let link = session.env()[0].clone();
    let with = cargo(&ws, &["build"], &[link, clean]);
    assert!(
        with.status.success(),
        "{}",
        String::from_utf8_lossy(&with.stderr)
    );
    assert_eq!(outputs(&ws), without);
    assert!(entries(&session).is_empty());
}

// covers: B-13, B-19
#[test]
fn two_checkouts_built_at_once_both_pass() {
    let w = World::new();
    let one = w.checkout("one");
    let two = w.checkout("the-second-one");
    let session = w.begin(Mode::On).unwrap();
    let env = session.env();
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| cargo(&one, &["build"], &env));
        let b = scope.spawn(|| cargo(&two, &["build"], &env));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert!(a.status.success(), "{}", String::from_utf8_lossy(&a.stderr));
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    assert!(run_c(&one).starts_with("first text|set one|2|7|generated one|"));
    assert!(run_c(&two).starts_with("first text|set one|2|7|generated one|"));
}

// covers: B-1, B-16
#[test]
fn observe_reuses_nothing_and_finds_no_difference() {
    let w = World::new();
    let one = w.checkout("one");
    let two = w.checkout("second-path");
    let session = w.begin(Mode::Observe).unwrap();
    build(&one, &session);
    let first = entries(&session).len();
    build(&two, &session);
    let second = entries(&session)[first..].to_vec();
    assert!(second.iter().all(|e| e.outcome != Outcome::Reused));
    assert!(second.iter().all(|e| !e.differed), "{second:#?}");
    assert!(last(&second, "a", Class::Library).found);
    assert!(last(&second, "b", Class::Library).found);
    let summary = session.finish(SystemTime::now());
    assert!(!summary.lines().is_empty());
}

// covers: B-6
#[test]
fn each_probe_changes_what_is_reused() {
    let w = World::new();
    let one = w.checkout("one");
    let session = w.begin(Mode::On).unwrap();
    build(&one, &session);

    let outcome_in = |ws: &Path, env: &[(OsString, OsString)], name: &str| {
        let before = entries(&session).len();
        let mut all = session.env();
        all.extend(env.iter().cloned());
        let out = cargo(ws, &["build"], &all);
        let after = entries(&session)[before..].to_vec();
        (
            out,
            after
                .iter()
                .rev()
                .find(|e| e.crate_name == name && e.class == Some(Class::Library))
                .cloned(),
        )
    };

    // A file beside the included one.
    let beside = w.checkout("beside");
    fs::write(beside.join("a/data/new.txt"), "new").unwrap();
    let (out, a) = outcome_in(&beside, &[], "a");
    assert!(out.status.success());
    assert_eq!(a.unwrap().outcome, Outcome::BuiltAndStored);

    // A `mod.rs` beside `x.rs`: the key changes, so rustc runs and refuses the ambiguity, as a
    // clean build would. (In a warm target cargo itself wouldn't call rustc.)
    let module = w.checkout("module");
    fs::create_dir_all(module.join("a/src/x")).unwrap();
    fs::write(module.join("a/src/x/mod.rs"), "pub fn two() -> u32 { 3 }\n").unwrap();
    let (out, a) = outcome_in(&module, &[], "a");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("E0761"));
    assert_eq!(
        a.unwrap().outcome,
        Outcome::BuiltNotStored(hushrun_build::model::NotStored::RustcFailed)
    );

    // Another value of A_VAR.
    let var = w.checkout("var");
    let (out, a) = outcome_in(&var, &[("A_VAR".into(), "set two".into())], "a");
    assert!(out.status.success());
    assert_eq!(a.unwrap().outcome, Outcome::BuiltAndStored);
    assert!(run_c(&var).contains("|set two|"));

    // The build script's input.
    let input = w.checkout("input");
    fs::write(input.join("b/input.txt"), "generated two").unwrap();
    let (out, b) = outcome_in(&input, &[], "b");
    assert!(out.status.success());
    assert_eq!(b.unwrap().outcome, Outcome::BuiltAndStored);
    assert!(run_c(&input).contains("|generated two|"));
}

// covers: B-5
#[test]
fn a_target_folder_set_by_cargo_target_dir_does_not_change_what_is_reused() {
    let w = World::new();
    let one = w.checkout("one");
    let two = w.checkout("a-second-checkout/longer");
    let target_a = w.path("ta");
    let target_b = w.path("targets/the-second-one");
    let session = w.begin(Mode::On).unwrap();
    let with_target = |dir: &Path| {
        let mut env = session.env();
        env.push(("CARGO_TARGET_DIR".into(), dir.as_os_str().to_owned()));
        env
    };
    let out = cargo(&one, &["build"], &with_target(&target_a));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first = entries(&session);
    assert_eq!(
        last(&first, "a", Class::Library).outcome,
        Outcome::BuiltAndStored
    );
    let out = cargo(&two, &["build"], &with_target(&target_b));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let second = entries(&session)[first.len()..].to_vec();
    assert!(reused(&last(&second, "a", Class::Library)), "{second:#?}");
    assert!(reused(&last(&second, "b", Class::Library)), "{second:#?}");
    let printed = Command::new(target_b.join("debug/c")).output().unwrap();
    assert!(String::from_utf8_lossy(&printed.stdout).starts_with("first text|set one|2|7|"));
}

// covers: B-12
#[test]
fn check_after_build_and_build_after_check_both_work() {
    let w = World::new();
    let one = w.checkout("one");
    let two = w.checkout("two-two");
    let session = w.begin(Mode::On).unwrap();
    let env = session.env();
    for ws in [&one, &two] {
        let built = cargo(ws, &["build"], &env);
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let checked = cargo(ws, &["check"], &env);
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
    }
    let three = w.checkout("three");
    let four = w.checkout("four-four");
    for ws in [&three, &four] {
        let checked = cargo(ws, &["check"], &env);
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
        let built = cargo(ws, &["build"], &env);
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        assert!(run_c(ws).starts_with("first text|"));
    }
    // The check of a fourth checkout reused its metadata.
    assert!(
        entries(&session)
            .iter()
            .any(|e| e.crate_name == "a" && e.class == Some(Class::Library) && reused(e))
    );
}

/// Makes a named pipe at `path`: opening it for reading blocks until a writer comes.
#[cfg(unix)]
fn fifo(path: &Path) {
    let _ = fs::remove_file(path);
    assert!(Command::new("mkfifo").arg(path).status().unwrap().success());
}

/// How a cargo run that may hang ended: its status and stderr, and whether a reader had to be
/// let go from one of `pipes` after `secs` (then cargo is killed by its PID).
#[cfg(unix)]
struct Bounded {
    status: Option<std::process::ExitStatus>,
    stderr: String,
    hung: bool,
}

#[cfg(unix)]
fn cargo_within(
    ws: &Path,
    args: &[&str],
    env: &[(OsString, OsString)],
    pipes: &[PathBuf],
    secs: u64,
) -> Bounded {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::Stdio;
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.args(args).arg("--offline").current_dir(ws);
    for (name, _) in std::env::vars_os() {
        let Some(name) = name.to_str() else { continue };
        if (name.starts_with("CARGO_") && name != "CARGO_HOME")
            || name.starts_with("RUSTC")
            || name.starts_with("HUSHRUN_BUILD")
            || name.starts_with("DYLD_")
            || matches!(name, "RUSTFLAGS" | "OUT_DIR")
        {
            command.env_remove(name);
        }
    }
    command.env("A_VAR", "set one");
    for (name, value) in env {
        command.env(name, value);
    }
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut err = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = err.read_to_string(&mut text);
        text
    });
    let started = std::time::Instant::now();
    let mut hung = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > std::time::Duration::from_secs(secs) {
            hung = true;
            // Let every blocked reader go, then stop cargo by its own PID.
            for pipe in pipes {
                let flags = rustix::fs::OFlags::NONBLOCK.bits();
                let _ = fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(i32::try_from(flags).unwrap())
                    .open(pipe);
            }
            if started.elapsed() > std::time::Duration::from_secs(secs + 30) {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    Bounded {
        status,
        stderr: reader.join().unwrap_or_default(),
        hung,
    }
}

// covers: B-19
#[cfg(unix)]
#[test]
fn a_named_pipe_in_a_package_and_in_out_dir_never_hangs_the_build() {
    let w = World::new();
    let ws = w.checkout("one");
    // d's build script declares nothing: the whole package is its input. b's build script
    // leaves a pipe in its output folder.
    fs::write(ws.join("d/build.rs"), "fn main() {}\n").unwrap();
    fifo(&ws.join("d/pipe"));
    let b_script = fs::read_to_string(ws.join("b/build.rs")).unwrap().replace(
        "    let out = std::env::var(\"OUT_DIR\").unwrap();\n",
        "    let out = std::env::var(\"OUT_DIR\").unwrap();\n    let _ = std::process::Command::new(\"mkfifo\").arg(format!(\"{out}/pipe\")).status();\n",
    );
    fs::write(ws.join("b/build.rs"), b_script).unwrap();
    let session = w.begin(Mode::On).unwrap();
    let mut pipes = vec![ws.join("d/pipe")];
    for entry in fs::read_dir(ws.join("target/debug/build"))
        .into_iter()
        .flatten()
        .flatten()
    {
        pipes.push(entry.path().join("out/pipe"));
    }
    let run = cargo_within(&ws, &["build"], &session.env(), &pipes, 60);
    // The pipe under b's output folder is only known once its script ran.
    assert!(!run.hung, "the build hung:\n{}", run.stderr);
    assert!(run.status.is_some_and(|s| s.success()), "{}", run.stderr);
    assert!(run_c(&ws).starts_with("first text|set one|2|7|generated one|"));
    let all = entries(&session);
    assert_eq!(
        last(&all, "d", Class::Library).outcome,
        Outcome::BuiltNotStored(hushrun_build::model::NotStored::NotAFile)
    );
    assert_eq!(
        last(&all, "b", Class::Library).outcome,
        Outcome::BuiltNotStored(hushrun_build::model::NotStored::NotAFile)
    );
}

// covers: B-6, B-8
#[cfg(unix)]
#[test]
fn a_file_included_through_dots_and_a_link_is_keyed_by_what_the_link_names_here() {
    let w = World::new();
    let one = w.checkout("one");
    let two = w.checkout("two-longer");
    for (ws, target) in [(&one, "A.txt"), (&two, "B.txt")] {
        fs::create_dir_all(ws.join("shared")).unwrap();
        fs::write(ws.join("shared/A.txt"), "first text").unwrap();
        fs::write(ws.join("shared/B.txt"), "OTHER TEXT").unwrap();
        fs::remove_file(ws.join("a/data/text.txt")).unwrap();
        std::os::unix::fs::symlink(format!("../../shared/{target}"), ws.join("a/data/text.txt"))
            .unwrap();
    }
    let session = w.begin(Mode::On).unwrap();
    build(&one, &session);
    assert!(run_c(&one).starts_with("first text|"));
    build(&two, &session);
    let printed = run_c(&two);
    assert!(
        printed.starts_with("OTHER TEXT|"),
        "a wrong reuse: {printed}"
    );
}

/// A proc macro that names its own folder (so its block is tied) and reads a file from it
/// when it runs, which no dep-info lists.
const TIED_MACRO: &str = "use proc_macro::TokenStream;\n\nconst DIR: &str = env!(\"CARGO_MANIFEST_DIR\");\n\n#[proc_macro]\npub fn make(_: TokenStream) -> TokenStream {\n    let v = std::fs::read_to_string(format!(\"{DIR}/value.txt\")).unwrap();\n    format!(\"pub fn made() -> u32 {{ {} }}\", v.trim()).parse().unwrap()\n}\n";

// covers: B-6, B-10, B-15
#[test]
fn a_dependent_of_a_tied_proc_macro_at_a_second_checkout_builds_against_its_own_macro() {
    let w = World::new();
    let one = w.checkout("one");
    let two = w.checkout("two-longer");
    for (ws, value) in [(&one, "7"), (&two, "8")] {
        fs::write(ws.join("m/src/lib.rs"), TIED_MACRO).unwrap();
        fs::write(ws.join("m/value.txt"), value).unwrap();
    }
    let session = w.begin(Mode::On).unwrap();
    build(&one, &session);
    assert!(run_c(&one).starts_with("first text|set one|2|7|"));
    let n = entries(&session).len();
    let out = cargo(&two, &["build"], &session.env());
    let second = entries(&session)[n..].to_vec();
    assert!(
        out.status.success(),
        "{second:#?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!reused(&last(&second, "b", Class::Library)), "{second:#?}");
    let printed = run_c(&two);
    assert!(
        printed.starts_with("first text|set one|2|8|"),
        "a wrong reuse: {printed}"
    );
    // Nothing healthy was suspended on the way: a third checkout like the first reuses b.
    let three = w.checkout("three");
    fs::write(three.join("m/src/lib.rs"), TIED_MACRO).unwrap();
    fs::write(three.join("m/value.txt"), "7").unwrap();
    let n = entries(&session).len();
    build(&three, &session);
    assert!(run_c(&three).starts_with("first text|set one|2|7|"));
    assert!(
        entries(&session)[n..].iter().all(|e| e.note.is_none()),
        "{:#?}",
        &entries(&session)[n..]
    );
}

// covers: B-15
#[test]
fn a_typo_in_a_dependents_own_extern_crate_suspends_nothing() {
    let w = World::new();
    let one = w.checkout("one");
    let two = w.checkout("two-longer");
    let session = w.begin(Mode::On).unwrap();
    build(&one, &session);
    let main = fs::read_to_string(two.join("c/src/main.rs")).unwrap();
    fs::write(
        two.join("c/src/main.rs"),
        format!("extern crate no_such_crate_typo;\n{main}"),
    )
    .unwrap();
    let n = entries(&session).len();
    let out = cargo(&two, &["build"], &session.env());
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("E0463"));
    let after = entries(&session)[n..].to_vec();
    assert!(reused(&last(&after, "a", Class::Library)), "{after:#?}");
    assert!(after.iter().all(|e| e.note.is_none()), "{after:#?}");
    // a's and b's reused files are still there, and a third checkout reuses them.
    let left: Vec<String> = fs::read_dir(two.join("target/debug/deps"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("liba-") || n.starts_with("libb-"))
        .collect();
    assert_eq!(left.len(), 4, "{left:?}");
    let three = w.checkout("three");
    let n = entries(&session).len();
    build(&three, &session);
    let third = entries(&session)[n..].to_vec();
    assert!(reused(&last(&third, "a", Class::Library)), "{third:#?}");
    assert!(reused(&last(&third, "b", Class::Library)), "{third:#?}");
}

/// Adds `n`, whose build script puts a static library `libx.a` in its output folder and names
/// that folder with `rustc-link-search`, and `p`, which bundles it with
/// `#[link(name = "x", kind = "static")]`; `c` prints what `x_value()` returns.
fn with_bundled_static_library(ws: &Path, value: u32) {
    let files = [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\", \"b\", \"m\", \"c\", \"d\", \"n\", \"p\"]\nresolver = \"2\"\n".to_string(),
        ),
        (
            "n/Cargo.toml",
            "[package]\nname = \"n\"\nversion = \"0.1.0\"\nedition = \"2021\"\n".to_string(),
        ),
        ("n/src/lib.rs", String::new()),
        (
            "n/build.rs",
            "fn main() {\n    println!(\"cargo:rerun-if-changed=libx.a\");\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::copy(\"libx.a\", format!(\"{out}/libx.a\")).unwrap();\n    println!(\"cargo:rustc-link-search=native={out}\");\n}\n".to_string(),
        ),
        (
            "p/Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nn = { path = \"../n\" }\n".to_string(),
        ),
        (
            "p/src/lib.rs",
            "#[link(name = \"x\", kind = \"static\")]\nextern \"C\" {\n    fn x_value() -> u32;\n}\n\npub fn value() -> u32 {\n    unsafe { x_value() }\n}\n".to_string(),
        ),
        (
            "c/Cargo.toml",
            "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\na = { path = \"../a\" }\nb = { path = \"../b\" }\nd = { path = \"../d\" }\np = { path = \"../p\" }\n".to_string(),
        ),
        (
            "c/src/main.rs",
            "fn main() {\n    println!(\"{}|{}|{}|{}|{}|{}|{}\", a::text(), a::var(), a::twice(), b::made(), b::GEN, p::value(), d::dir());\n}\n".to_string(),
        ),
        ("n/x.c", format!("unsigned x_value(void) {{ return {value}; }}\n")),
    ];
    for (file, text) in files {
        let path = ws.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let n = ws.join("n");
    let compiled = Command::new("cc")
        .args(["-c", "x.c", "-o", "x.o"])
        .current_dir(&n)
        .status()
        .unwrap();
    assert!(compiled.success());
    let archived = Command::new("ar")
        .args(["rcs", "libx.a", "x.o"])
        .current_dir(&n)
        .status()
        .unwrap();
    assert!(archived.success());
}

// covers: B-6, B-8
#[test]
fn a_static_library_bundled_from_a_dependencys_output_folder_is_keyed() {
    let w = World::new();
    let one = w.checkout("one");
    let two = w.checkout("two-longer");
    let three = w.checkout("three");
    with_bundled_static_library(&one, 7);
    with_bundled_static_library(&two, 8);
    with_bundled_static_library(&three, 7);
    // An archive holds its members' dates: three gets one's very bytes.
    fs::copy(one.join("n/libx.a"), three.join("n/libx.a")).unwrap();
    let session = w.begin(Mode::On).unwrap();
    build(&one, &session);
    assert!(run_c(&one).starts_with("first text|set one|2|7|generated one|7|"));
    let n = entries(&session).len();
    build(&two, &session);
    let printed = run_c(&two);
    assert!(
        printed.starts_with("first text|set one|2|7|generated one|8|"),
        "a wrong reuse: {printed}\n{:#?}",
        &entries(&session)[n..]
    );
    // The same library elsewhere: p is reused.
    let n = entries(&session).len();
    build(&three, &session);
    assert!(run_c(&three).starts_with("first text|set one|2|7|generated one|7|"));
    let third = entries(&session)[n..].to_vec();
    assert!(reused(&last(&third, "p", Class::Library)), "{third:#?}");
}

/// The shape of `libmimalloc-sys` and `mimalloc`: `x`'s build script compiles `x.c` into
/// `libx.a` in its output folder, names that folder with `rustc-link-search`, and `x` bundles the
/// library by attribute; `w` wraps `x`. `bundles` is another library that bundles `libx.a` by
/// attribute from `x`'s folder. The package `c` has two programs: `uses` calls `w`, `plain`
/// doesn't. The package `l` has one program, `linked`, which its build script links with
/// `-l static=x`. Cargo passes `x`'s folder with `-L native=` to every compile of `w`,
/// `bundles`, `c` and `l`.
const NATIVE: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"x\", \"w\", \"bundles\", \"c\", \"l\"]\nresolver = \"2\"\n",
    ),
    (
        "x/Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "x/build.rs",
        "use std::process::Command;\n\nfn main() {\n    println!(\"cargo:rerun-if-changed=x.c\");\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    assert!(Command::new(\"cc\").args([\"-c\", \"x.c\", \"-o\"]).arg(format!(\"{out}/x.o\")).status().unwrap().success());\n    assert!(Command::new(\"ar\").arg(\"rcs\").arg(format!(\"{out}/libx.a\")).arg(format!(\"{out}/x.o\")).status().unwrap().success());\n    std::fs::remove_file(format!(\"{out}/x.o\")).unwrap();\n    println!(\"cargo:rustc-link-search=native={out}\");\n}\n",
    ),
    (
        "x/src/lib.rs",
        "#[link(name = \"x\", kind = \"static\")]\nextern \"C\" {\n    fn x_value() -> u32;\n}\n\npub fn value() -> u32 {\n    unsafe { x_value() }\n}\n",
    ),
    ("x/x.c", "unsigned x_value(void) { return 7; }\n"),
    (
        "w/Cargo.toml",
        "[package]\nname = \"w\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nx = { path = \"../x\" }\n",
    ),
    (
        "w/src/lib.rs",
        "pub fn value() -> u32 {\n    x::value()\n}\n",
    ),
    (
        "bundles/Cargo.toml",
        "[package]\nname = \"bundles\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nx = { path = \"../x\" }\n",
    ),
    (
        "bundles/src/lib.rs",
        "#[link(name = \"x\", kind = \"static\")]\nextern \"C\" {\n    fn x_value() -> u32;\n}\n\npub fn again() -> u32 {\n    unsafe { x_value() }\n}\n",
    ),
    (
        "c/Cargo.toml",
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"uses\"\npath = \"src/uses.rs\"\n\n[[bin]]\nname = \"plain\"\npath = \"src/plain.rs\"\n\n[dependencies]\nw = { path = \"../w\" }\n",
    ),
    (
        "c/src/uses.rs",
        "fn main() {\n    println!(\"{}\", w::value());\n}\n",
    ),
    (
        "c/src/plain.rs",
        "fn main() {\n    println!(\"plain\");\n}\n",
    ),
    (
        "l/Cargo.toml",
        "[package]\nname = \"l\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"linked\"\npath = \"src/main.rs\"\n\n[dependencies]\nx = { path = \"../x\" }\n",
    ),
    (
        "l/build.rs",
        "fn main() {\n    println!(\"cargo:rustc-link-lib=static=x\");\n}\n",
    ),
    (
        "l/src/main.rs",
        "extern \"C\" {\n    fn x_value() -> u32;\n}\n\nfn main() {\n    println!(\"{}\", unsafe { x_value() });\n}\n",
    ),
];

/// What the program `<ws>/target/debug/<name>` prints, trimmed.
fn printed(ws: &Path, name: &str) -> String {
    let out = Command::new(ws.join("target/debug").join(name))
        .output()
        .unwrap();
    assert!(out.status.success(), "{name} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

// covers: B-6, B-18
#[test]
fn a_static_library_that_changes_rebuilds_only_what_links_or_bundles_it() {
    let w = World::new();
    let ws = w.path("one");
    for (file, text) in NATIVE {
        let path = ws.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let session = w.begin(Mode::On).unwrap();
    // `w` stands for a crate from a registry, which cargo never builds incrementally: an
    // incremental compile's bytes are not reproducible, and `plain` depends on `w`'s.
    let mut env = session.env();
    env.push(("CARGO_INCREMENTAL".into(), "0".into()));
    let build = |ws: &Path| {
        let out = cargo(ws, &["build"], &env);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    build(&ws);
    for (name, says) in [("uses", "7"), ("plain", "plain"), ("linked", "7")] {
        assert_eq!(printed(&ws, name), says, "{name}");
    }

    // The library's bytes change, as libmimalloc-sys's do in every build (its C compiler bakes
    // in the date). Cargo builds every dependent of `x` again.
    fs::write(ws.join("x/x.c"), "unsigned x_value(void) { return 8; }\n").unwrap();
    let n = entries(&session).len();
    build(&ws);
    let after = entries(&session)[n..].to_vec();
    // A program that links nothing from the folder is reused: its link line doesn't name the
    // library, and the folder is not its input.
    assert_eq!(
        last(&after, "plain", Class::Program).outcome,
        Outcome::Reused,
        "{after:#?}"
    );
    // One that links it through the rlib that bundles it, or by `-l`, is built again.
    for name in ["uses", "linked"] {
        let entry = last(&after, name, Class::Program);
        assert_eq!(entry.outcome, Outcome::BuiltAndStored, "{name}: {after:#?}");
        assert!(!entry.found, "{name}: {after:#?}");
    }
    // A library that bundles it by attribute is built again: nothing but the folder says so.
    let bundles = last(&after, "bundles", Class::Library);
    assert_eq!(bundles.outcome, Outcome::BuiltAndStored, "{after:#?}");
    assert!(!bundles.found, "{after:#?}");
    for (name, says) in [("uses", "8"), ("plain", "plain"), ("linked", "8")] {
        assert_eq!(printed(&ws, name), says, "{name}");
    }
}

/// A workspace whose library `app` names an extern `sqlx`: a local stand-in, since the table of
/// macros known to read unlisted files goes by the extern's name (B-6 `declared`), and sqlx's
/// read the first `.env` found walking up from the package and the working directory.
const SQLX: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"sqlx\", \"app\"]\nresolver = \"2\"\n",
    ),
    (
        "sqlx/Cargo.toml",
        "[package]\nname = \"sqlx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    ("sqlx/src/lib.rs", "pub fn n() -> u32 {\n    1\n}\n"),
    (
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nsqlx = { path = \"../sqlx\" }\n",
    ),
    (
        "app/src/lib.rs",
        "pub fn n() -> u32 {\n    sqlx::n() + 1\n}\n",
    ),
];

// covers: B-6, B-8
#[test]
fn a_crate_that_uses_sqlx_moves_unless_a_dotenv_above_differs() {
    let w = World::new();
    let checkout = |rest: &str| {
        let ws = w.path(rest);
        for (file, text) in SQLX {
            let path = ws.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        ws
    };
    let one = checkout("first-parent/one");
    let two = checkout("a-second-parent/two");
    let three = checkout("third/three");
    fs::write(w.path("third").join(".env"), "DATABASE_URL=postgres://x\n").unwrap();
    let session = w.begin(Mode::On).unwrap();
    build(&one, &session);
    let first = entries(&session);
    assert_eq!(
        last(&first, "app", Class::Library).outcome,
        Outcome::BuiltAndStored
    );
    // Under another parent with no `.env` above it: reused.
    build(&two, &session);
    let second = entries(&session)[first.len()..].to_vec();
    assert!(reused(&last(&second, "app", Class::Library)), "{second:#?}");
    // A `.env` above the third: built again.
    let n = entries(&session).len();
    build(&three, &session);
    let third = entries(&session)[n..].to_vec();
    let app = last(&third, "app", Class::Library);
    assert_eq!(app.outcome, Outcome::BuiltAndStored, "{third:#?}");
    assert!(!app.found, "{third:#?}");
}

/// A workspace whose programs B-9 makes movable on Apple targets: `lib_a` panics three calls
/// deep on a bad index (`lib_a/src/lib.rs:11:5`), and its unit test fails there; the bin `prog`
/// calls it with its first argument; the bin `where` prints its package's folder, read at compile
/// time; the integration test `t` fails inside `lib_a`; the integration test `bin` runs `prog`
/// through `CARGO_BIN_EXE_prog` and shows what it printed.
#[cfg(target_os = "macos")]
const PROGRAMS: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"lib_a\", \"prog\"]\nresolver = \"2\"\n",
    ),
    (
        "lib_a/Cargo.toml",
        "[package]\nname = \"lib_a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ndoctest = false\n",
    ),
    (
        "lib_a/src/lib.rs",
        "pub fn top(values: &[u32], index: usize) -> u32 {\n    middle(values, index)\n}\n\npub fn middle(values: &[u32], index: usize) -> u32 {\n    deep(values, index) + 1\n}\n\n#[inline(never)]\npub fn deep(values: &[u32], index: usize) -> u32 {\n    values[index]\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn fails_in_a_unit_test() {\n        super::top(&[1, 2], 9);\n    }\n}\n",
    ),
    (
        "prog/Cargo.toml",
        "[package]\nname = \"prog\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"prog\"\npath = \"src/main.rs\"\ntest = false\n\n[[bin]]\nname = \"where\"\npath = \"src/where.rs\"\ntest = false\n\n[dependencies]\nlib_a = { path = \"../lib_a\" }\n",
    ),
    (
        "prog/src/main.rs",
        "fn main() {\n    let index: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(0);\n    println!(\"{}\", lib_a::top(&[10, 20, 30], index));\n}\n",
    ),
    (
        "prog/src/where.rs",
        "fn main() {\n    println!(\"{}\", env!(\"CARGO_MANIFEST_DIR\"));\n}\n",
    ),
    (
        "prog/tests/t.rs",
        "#[test]\nfn passes() {\n    assert_eq!(lib_a::top(&[1, 2], 0), 2);\n}\n\n#[test]\nfn fails_inside_lib_a() {\n    lib_a::top(&[1, 2], 9);\n}\n",
    ),
    (
        "prog/tests/bin.rs",
        "#[test]\nfn the_bin_panics() {\n    let out = std::process::Command::new(env!(\"CARGO_BIN_EXE_prog\"))\n        .arg(\"9\")\n        .env(\"RUST_BACKTRACE\", \"1\")\n        .output()\n        .unwrap();\n    panic!(\"the bin said:\\n{}\", String::from_utf8_lossy(&out.stderr));\n}\n",
    ),
];

#[cfg(target_os = "macos")]
impl World {
    /// A fresh copy of [`PROGRAMS`] at `rest`.
    fn programs(&self, rest: &str) -> PathBuf {
        let ws = self.path(rest);
        for (file, text) in PROGRAMS {
            let path = ws.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        ws
    }
}

/// Runs cargo and insists it succeeded.
#[cfg(target_os = "macos")]
fn cargo_ok(ws: &Path, args: &[&str], env: &[(OsString, OsString)]) -> Output {
    let out = cargo(ws, args, env);
    assert!(
        out.status.success(),
        "cargo {args:?} in {} failed:\n{}",
        ws.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The session's variables, with `CARGO_TARGET_DIR` when given.
#[cfg(target_os = "macos")]
fn env_with(session: &Session, target: Option<&Path>) -> Vec<(OsString, OsString)> {
    let mut env = session.env();
    if let Some(dir) = target {
        env.push(("CARGO_TARGET_DIR".into(), dir.as_os_str().to_owned()));
    }
    env
}

/// The program `<target>/debug/deps/<name>-<16 hex digits>`.
#[cfg(target_os = "macos")]
fn program(target: &Path, name: &str) -> PathBuf {
    let deps = target.join("debug/deps");
    fs::read_dir(&deps)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            let n = p.file_name().unwrap().to_str().unwrap();
            n.strip_prefix(&format!("{name}-"))
                .is_some_and(|hash| hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .unwrap_or_else(|| panic!("no program {name} in {}", deps.display()))
}

/// Every file of a program's debug bundle, by its path in the bundle, with its bytes.
#[cfg(target_os = "macos")]
fn bundle(program: &Path) -> BTreeMap<String, Vec<u8>> {
    let dir = PathBuf::from(format!("{}.dSYM", program.display()));
    let files: BTreeMap<String, Vec<u8>> = walk(&dir)
        .into_iter()
        .map(|file| {
            let rest = file
                .strip_prefix(&dir)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            (rest, fs::read(&file).unwrap())
        })
        .collect();
    assert!(
        files
            .keys()
            .any(|k| k.starts_with("Contents/Resources/DWARF/")),
        "no debug bundle beside {}: {:?}",
        program.display(),
        files.keys().collect::<Vec<_>>()
    );
    files
}

/// The places of the stored blocks of a crate of a class, from each `block.json`.
#[cfg(target_os = "macos")]
fn places(store: &Path, name: &str, class: &str) -> Vec<String> {
    walk(&store.join("blocks"))
        .into_iter()
        .filter(|p| p.ends_with("block.json"))
        .filter_map(|p| {
            let record: serde_json::Value = serde_json::from_slice(&fs::read(p).unwrap()).unwrap();
            let d = &record["description"];
            (d["crate_name"] == name && d["class"] == class)
                .then(|| d["place"].as_str().unwrap_or_default().to_string())
        })
        .collect()
}

/// Whether a backtrace in `text` shows `frame` with `at <place>` on the next line.
#[cfg(target_os = "macos")]
fn frame_at(text: &str, frame: &str, place: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    lines.windows(2).any(|pair| {
        pair[0].trim_end().ends_with(&format!(": {frame}"))
            && pair[1].trim().starts_with(&format!("at {place}"))
    })
}

/// The UUIDs `dwarfdump --uuid` prints for each file, in order.
#[cfg(target_os = "macos")]
fn uuids(files: &[&Path]) -> Vec<String> {
    let out = Command::new("dwarfdump")
        .arg("--uuid")
        .args(files)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| {
            line.split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The checks that follow a reuse at `two` with `one` gone (B-9, B-12): every backtrace in the
/// person's own code keeps its file and line, under `cargo test`, run directly from another
/// folder, through `CARGO_BIN_EXE_prog` and under `cargo run`; the bundles match their
/// programs; nothing is built again, with or without the wrapper.
#[cfg(target_os = "macos")]
fn lines_after_a_move(
    w: &World,
    session: &Session,
    two: &Path,
    target: &Path,
    target_env: Option<&Path>,
) {
    let env = env_with(session, target_env);
    let mut bt = env.clone();
    bt.push(("RUST_BACKTRACE".into(), "1".into()));
    let deep = ("lib_a::deep", "./lib_a/src/lib.rs:11:5");

    // `cargo test` runs each test binary from its package folder. None is built again.
    let before = entries(session).len();
    for (package, which, frame) in [
        (
            "lib_a",
            "--lib",
            (
                "lib_a::tests::fails_in_a_unit_test",
                "./lib_a/src/lib.rs:18:9",
            ),
        ),
        (
            "prog",
            "--test=t",
            ("t::fails_inside_lib_a", "./prog/tests/t.rs:8:5"),
        ),
    ] {
        let out = cargo(two, &["test", "-p", package, which], &bt);
        let text = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "the failing test passed:\n{text}");
        assert!(!stderr.contains("Compiling"), "{stderr}");
        assert!(frame_at(&text, deep.0, deep.1), "{text}");
        assert!(frame_at(&text, frame.0, frame.1), "{text}");
    }
    assert_eq!(entries(session).len(), before, "the wrapper ran again");

    // The test binaries run directly from a folder of no checkout.
    for (t, test) in [
        (program(target, "lib_a"), "fails_in_a_unit_test"),
        (program(target, "t"), "fails_inside_lib_a"),
    ] {
        let out = Command::new(&t)
            .arg(test)
            .env("RUST_BACKTRACE", "1")
            .current_dir(w.path("somewhere-else"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(frame_at(&text, deep.0, deep.1), "{text}");
    }

    // The bin, through the copy cargo puts at `<target>/debug/prog`.
    let out = cargo(two, &["test", "-p", "prog", "--test", "bin"], &bt);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("the bin said:"), "{text}");
    assert!(frame_at(&text, deep.0, deep.1), "{text}");
    assert!(
        frame_at(&text, "prog::main", "./prog/src/main.rs:3:"),
        "{text}"
    );
    let out = cargo(
        two,
        &["run", "-q", "-p", "prog", "--bin", "prog", "--", "9"],
        &bt,
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(frame_at(&text, deep.0, deep.1), "{text}");
    assert!(
        frame_at(&text, "prog::main", "./prog/src/main.rs:3:"),
        "{text}"
    );

    // Each bundle is the one its program needs, for the hashed file and for cargo's copy.
    let prog = program(target, "prog");
    let t = program(target, "lib_a");
    for (exe, dsym) in [
        (t.clone(), PathBuf::from(format!("{}.dSYM", t.display()))),
        (
            prog.clone(),
            PathBuf::from(format!("{}.dSYM", prog.display())),
        ),
        (target.join("debug/prog"), target.join("debug/prog.dSYM")),
    ] {
        let found = uuids(&[&exe, &dsym]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], found[1], "{} and its bundle", exe.display());
    }

    // Built again, through the wrapper or without it: nothing is compiled.
    let before = entries(session).len();
    let out = cargo_ok(two, &["test", "--no-run"], &env);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("Compiling"));
    assert_eq!(entries(session).len(), before);
    let plain: Vec<(OsString, OsString)> = target_env
        .map(|dir| vec![("CARGO_TARGET_DIR".into(), dir.as_os_str().to_owned())])
        .unwrap_or_default();
    let out = cargo_ok(two, &["test", "--no-run"], &plain);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("Compiling"), "{stderr}");

    // An edit built without the wrapper: rustc writes the program as cargo asks, and the bundle
    // left beside it no longer matches its UUID, so readers skip it and use the program's own
    // debug map. Lines stay.
    let lib = two.join("lib_a/src/lib.rs");
    let text = fs::read_to_string(&lib).unwrap();
    fs::write(&lib, format!("{text}// edited\n")).unwrap();
    cargo_ok(two, &["test", "--no-run"], &plain);
    let t = program(target, "lib_a");
    let found = uuids(&[&t, &PathBuf::from(format!("{}.dSYM", t.display()))]);
    assert_ne!(found[0], found[1], "the bundle was not left as it was");
    let out = Command::new(&t)
        .arg("fails_in_a_unit_test")
        .env("RUST_BACKTRACE", "1")
        .current_dir(two)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("lib_a/src/lib.rs:11:5"), "{text}");
}

/// Builds the tests at `one`, then at `two`, and checks what moved (B-5, B-8, B-9, B-10, B-12):
/// the bin and the test binaries are reused, may move, and are `one`'s bytes, bundles included;
/// the bin that holds its folder is built again and prints `two`'s. That holds with
/// `CARGO_TARGET_DIR` outside the checkouts too: the integration test's `CARGO_TARGET_TMPDIR` and
/// `CARGO_BIN_EXE_*` lie under the target folder, which is a root of its own. The test that bakes
/// `env!("CARGO_BIN_EXE_prog")` in is tied, by the search, in either layout.
#[cfg(target_os = "macos")]
fn programs_move(w: &World, one: &Path, two: &Path, targets: Option<(&Path, &Path)>) {
    let session = w.begin(Mode::On).unwrap();
    let (target_one, target_two) = match targets {
        Some((a, b)) => (a.to_path_buf(), b.to_path_buf()),
        None => (one.join("target"), two.join("target")),
    };
    cargo_ok(
        one,
        &["test", "--no-run"],
        &env_with(&session, targets.map(|t| t.0)),
    );
    let n = entries(&session).len();
    cargo_ok(
        two,
        &["test", "--no-run"],
        &env_with(&session, targets.map(|t| t.1)),
    );
    let second = entries(&session)[n..].to_vec();
    for name in ["prog", "lib_a", "t"] {
        assert_eq!(
            last(&second, name, Class::Program).outcome,
            Outcome::Reused,
            "{name}: {second:#?}"
        );
        let places = places(&session.current().store, name, "program");
        assert_eq!(places, ["anywhere"], "{name}");
        let (a, b) = (program(&target_one, name), program(&target_two, name));
        assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap(), "{name}");
        assert_eq!(bundle(&a), bundle(&b), "{name}'s bundle");
    }
    let baked = places(&session.current().store, "bin", "program");
    assert!(
        !baked.is_empty() && baked.iter().all(|place| place == "tied"),
        "{baked:?}"
    );
    let tied = last(&second, "where", Class::Program);
    assert_eq!(tied.outcome, Outcome::BuiltAndStored, "{second:#?}");
    assert!(!tied.found);
    let out = cargo_ok(
        two,
        &["run", "-q", "-p", "prog", "--bin", "where"],
        &env_with(&session, targets.map(|t| t.1)),
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        two.join("prog").display().to_string()
    );

    // The checkout that built them is gone.
    fs::remove_dir_all(one).unwrap();
    if targets.is_some() {
        fs::remove_dir_all(&target_one).unwrap();
    }
    lines_after_a_move(w, &session, two, &target_two, targets.map(|t| t.1));
}

// covers: B-9, B-10, B-12, B-18
#[cfg(target_os = "macos")]
#[test]
fn a_program_moves_with_its_debug_bundle_and_keeps_its_lines() {
    let w = World::new();
    let one = w.programs("one");
    let two = w.programs("a-second-checkout/with-a-longer-path");
    programs_move(&w, &one, &two, None);
}

// covers: B-5, B-8, B-9, B-10, B-12, B-18
#[cfg(target_os = "macos")]
#[test]
fn a_program_moves_with_its_debug_bundle_when_cargo_target_dir_is_elsewhere() {
    let w = World::new();
    let one = w.programs("one");
    let two = w.programs("a-second-checkout/with-a-longer-path");
    let target_one = w.path("targets/one");
    let target_two = w.path("other-targets/the-second-one");
    programs_move(&w, &one, &two, Some((&target_one, &target_two)));
}

// covers: B-9, B-10, B-18, B-21
#[cfg(target_os = "macos")]
#[test]
fn a_program_set_to_tied_has_no_bundle_and_is_reused_where_it_was_built() {
    let w = World::new();
    let one = w.programs("one");
    let target = one.join("target");
    let session = w
        .begin_with(Mode::On, |s| {
            s.programs = hushrun_build::model::Programs::Tied;
        })
        .unwrap();
    let env = session.env();
    let no_bundle = |name: &str| {
        let exe = program(&target, name);
        assert!(
            !PathBuf::from(format!("{}.dSYM", exe.display())).exists(),
            "a bundle beside {name}"
        );
    };
    cargo_ok(&one, &["test", "--no-run"], &env);
    let first = entries(&session);
    let store = session.current().store.clone();
    for name in ["prog", "lib_a", "t"] {
        assert_eq!(
            last(&first, name, Class::Program).outcome,
            Outcome::BuiltAndStored,
            "{name}: {first:#?}"
        );
        assert_eq!(places(&store, name, "program"), ["tied"], "{name}");
        no_bundle(name);
    }

    // An edit and back, as a branch switched and back: the edit is rebuilt as asked, and the
    // way back reuses what this folder built first.
    let lib = one.join("lib_a/src/lib.rs");
    let text = fs::read_to_string(&lib).unwrap();
    fs::write(&lib, format!("{text}// edited\n")).unwrap();
    cargo_ok(&one, &["test", "--no-run"], &env);
    fs::write(&lib, &text).unwrap();
    let n = entries(&session).len();
    cargo_ok(&one, &["test", "--no-run"], &env);
    let back = entries(&session)[n..].to_vec();
    for name in ["prog", "lib_a", "t"] {
        assert_eq!(
            last(&back, name, Class::Program).outcome,
            Outcome::Reused,
            "{name}: {back:#?}"
        );
        no_bundle(name);
    }
    let out = Command::new(program(&target, "t"))
        .arg("passes")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// covers: B-9, B-10
#[cfg(target_os = "macos")]
#[test]
fn two_independent_builds_at_two_paths_give_the_same_programs_and_bundles() {
    let first = World::new();
    let second = World::new();
    let one = first.programs("one");
    let two = second.programs("a-second-checkout/with-a-longer-path");
    for (w, ws) in [(&first, &one), (&second, &two)] {
        let session = w.begin(Mode::On).unwrap();
        cargo_ok(ws, &["test", "--no-run"], &session.env());
        let built = entries(&session);
        assert_eq!(
            last(&built, "t", Class::Program).outcome,
            Outcome::BuiltAndStored
        );
    }
    for name in ["prog", "lib_a", "t"] {
        let (a, b) = (
            program(&one.join("target"), name),
            program(&two.join("target"), name),
        );
        assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap(), "{name}");
        assert_eq!(bundle(&a), bundle(&b), "{name}'s bundle");
    }
}

// covers: B-12, B-19
#[cfg(target_os = "macos")]
#[test]
fn a_truncated_file_in_a_stored_bundle_gives_a_green_build() {
    let w = World::new();
    let one = w.programs("one");
    let session = w.begin(Mode::On).unwrap();
    cargo_ok(&one, &["test", "--no-run"], &session.env());
    let store = session.current().store.clone();
    let dwarf: Vec<PathBuf> = walk(&store.join("blocks"))
        .into_iter()
        .filter(|p| {
            p.to_str()
                .unwrap()
                .contains(".dSYM/Contents/Resources/DWARF/")
        })
        .collect();
    assert!(!dwarf.is_empty(), "no bundle in the store");
    for file in &dwarf {
        let mut perms = fs::metadata(file).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(file, perms).unwrap();
        fs::write(file, b"truncated").unwrap();
    }
    let two = w.programs("two-two");
    let n = entries(&session).len();
    cargo_ok(&two, &["test", "--no-run"], &session.env());
    let second = entries(&session)[n..].to_vec();
    assert_eq!(
        last(&second, "t", Class::Program).outcome,
        Outcome::BuiltAndStored,
        "{second:#?}"
    );
    let t = program(&two.join("target"), "t");
    let found = uuids(&[&t, &PathBuf::from(format!("{}.dSYM", t.display()))]);
    assert_eq!(found[0], found[1]);
}

/// A workspace whose changes cargo can't see (B-23). `embed` stands in for sqlx's `migrate!`: it
/// lists a folder when it runs and puts each file in an `include_str!`, so the files are in the
/// dep-info and the listing is not. `app` embeds every file of `app/data` and its program prints
/// them. `db` names an extern `sqlx` (a local stand-in: the known macros go by the extern's
/// name, B-6) and has a `migrations` folder.
const UNSEEN: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"embed\", \"app\", \"sqlx\", \"db\"]\nresolver = \"2\"\n",
    ),
    (
        "embed/Cargo.toml",
        "[package]\nname = \"embed\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n",
    ),
    (
        "embed/src/lib.rs",
        "use proc_macro::TokenStream;\n\n#[proc_macro]\npub fn embed_dir(input: TokenStream) -> TokenStream {\n    let dir = input.to_string().trim_matches('\"').to_string();\n    let root = std::env::var(\"CARGO_MANIFEST_DIR\").unwrap();\n    let mut names: Vec<String> = std::fs::read_dir(format!(\"{root}/{dir}\"))\n        .unwrap()\n        .map(|e| e.unwrap().file_name().into_string().unwrap())\n        .collect();\n    names.sort();\n    let parts: Vec<String> = names\n        .iter()\n        .map(|n| format!(\"include_str!({:?})\", format!(\"{root}/{dir}/{n}\")))\n        .collect();\n    format!(\"pub fn all() -> String {{ [{}].concat() }}\", parts.join(\", \"))\n        .parse()\n        .unwrap()\n}\n",
    ),
    (
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nembed = { path = \"../embed\" }\n",
    ),
    ("app/src/lib.rs", "embed::embed_dir!(\"data\");\n"),
    (
        "app/src/main.rs",
        "fn main() {\n    println!(\"{}\", app::all());\n}\n",
    ),
    ("app/data/1.txt", "one;"),
    (
        "sqlx/Cargo.toml",
        "[package]\nname = \"sqlx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    ("sqlx/src/lib.rs", "pub fn n() -> u32 {\n    1\n}\n"),
    (
        "db/Cargo.toml",
        "[package]\nname = \"db\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nsqlx = { path = \"../sqlx\" }\n",
    ),
    (
        "db/src/lib.rs",
        "pub fn n() -> u32 {\n    sqlx::n() + 1\n}\n",
    ),
    ("db/migrations/0001_first.sql", "create table a (x int);\n"),
];

impl World {
    /// A fresh copy of [`UNSEEN`] at `rest`.
    fn unseen(&self, rest: &str) -> PathBuf {
        let ws = self.path(rest);
        for (file, text) in UNSEEN {
            let path = ws.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        ws
    }
}

/// What one `hushrun run NAME -- cargo build` did: the check before COMMAND, the compiles, and
/// the summary's lines.
struct HushrunRun {
    checked: hushrun_build::unit::Checked,
    compiled: Vec<Entry>,
    lines: Vec<String>,
}

/// `hushrun run NAME -- cargo build` in `ws`, as the CLI runs it: a session, the check, cargo,
/// the summary.
fn hushrun_run(w: &World, ws: &Path) -> HushrunRun {
    let mut session = w.begin(Mode::On).unwrap();
    let checked = session.check(ws, None);
    build(ws, &session);
    let compiled = entries(&session);
    let lines = session.finish(SystemTime::now()).lines().to_vec();
    HushrunRun {
        checked,
        compiled,
        lines,
    }
}

/// Plain cargo, as cargo alone decides: it compiles nothing.
fn plain_cargo_compiles_nothing(ws: &Path) {
    let out = cargo(ws, &["build"], &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(!stderr.contains("Compiling"), "{stderr}");
}

/// What the program `app` prints.
fn app_says(ws: &Path) -> String {
    printed(ws, "app")
}

/// A file's modification time set back a minute before `before`.
fn date_back(path: &Path, before: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(before - std::time::Duration::from_secs(60))
        .unwrap();
}

/// The first build, through the wrapper; the files are dated before it, so a later change
/// within the same second is a change.
fn first_build(w: &World, ws: &Path) -> SystemTime {
    let built = SystemTime::now();
    for file in ["app/src/lib.rs", "app/data/1.txt", "db/src/lib.rs"] {
        date_back(&ws.join(file), built);
    }
    let first = hushrun_run(w, ws);
    assert!(first.lines[0].contains("built"), "{:?}", first.lines);
    assert_eq!(app_says(ws), "one;");
    built
}

// covers: B-23
#[test]
fn a_file_added_to_a_folder_a_macro_lists_is_rebuilt_transparently() {
    let w = World::new();
    let ws = w.unseen("one");
    first_build(&w, &ws);
    fs::write(ws.join("app/data/2.txt"), "two;").unwrap();
    // The bug: cargo says Fresh, and the program is stale.
    plain_cargo_compiles_nothing(&ws);
    assert_eq!(app_says(&ws), "one;");
    let run = hushrun_run(&w, &ws);
    assert_eq!(app_says(&ws), "one;two;");
    assert!(
        run.compiled
            .iter()
            .any(|e| e.crate_name == "app" && e.class == Some(Class::Library)),
        "{:#?}",
        run.compiled
    );
    assert!(
        run.lines.contains(
            &"build: app is rebuilt: a file was added to app/data, which cargo doesn't watch"
                .to_string()
        ),
        "{:?}",
        run.lines
    );
}

// covers: B-6, B-23
#[test]
fn a_migration_added_beside_a_known_macro_is_rebuilt() {
    let w = World::new();
    let ws = w.unseen("one");
    first_build(&w, &ws);
    fs::write(
        ws.join("db/migrations/0002_second.sql"),
        "create table b (y int);\n",
    )
    .unwrap();
    plain_cargo_compiles_nothing(&ws);
    let run = hushrun_run(&w, &ws);
    let db = last(&run.compiled, "db", Class::Library);
    assert!(!reused(&db), "{:#?}", run.compiled);
    assert!(
        run.lines.contains(
            &"build: db is rebuilt: a file was added to db/migrations, which cargo doesn't watch"
                .to_string()
        ),
        "{:?}",
        run.lines
    );
}

// covers: B-11, B-23
#[test]
fn other_bytes_under_an_older_date_are_rebuilt() {
    let w = World::new();
    let ws = w.unseen("one");
    let built = first_build(&w, &ws);
    // An archive extracted with its original dates: other bytes, a date before the last build.
    let data = ws.join("app/data/1.txt");
    fs::write(&data, "ONE;").unwrap();
    date_back(&data, built);
    plain_cargo_compiles_nothing(&ws);
    assert_eq!(app_says(&ws), "one;");
    let run = hushrun_run(&w, &ws);
    assert_eq!(app_says(&ws), "ONE;");
    assert!(
        run.lines.iter().any(|l| l.starts_with(
            "build: app is rebuilt: app/data/1.txt changed but kept a date older than the last build"
        )),
        "{:?}",
        run.lines
    );
}

// covers: B-9, B-23
#[test]
fn after_an_incremental_rebuild_a_file_added_is_still_seen() {
    let w = World::new();
    let ws = w.unseen("one");
    first_build(&w, &ws);
    let lib = ws.join("app/src/lib.rs");
    fs::write(&lib, "embed::embed_dir!(\"data\");\n// edited\n").unwrap();
    let edit = hushrun_run(&w, &ws);
    assert_eq!(
        last(&edit.compiled, "app", Class::Library).outcome,
        Outcome::PassedThrough(hushrun_build::model::PassedThrough::Incremental),
        "{:#?}",
        edit.compiled
    );
    assert!(edit.checked.rebuilt.is_empty());
    fs::write(ws.join("app/data/3.txt"), "three;").unwrap();
    plain_cargo_compiles_nothing(&ws);
    let run = hushrun_run(&w, &ws);
    assert_eq!(app_says(&ws), "one;three;");
    assert_eq!(run.checked.rebuilt.len(), 1, "{:?}", run.checked);
}

// covers: B-20, B-23
#[test]
fn nothing_changed_removes_nothing_compiles_nothing_and_says_nothing() {
    let w = World::new();
    let ws = w.unseen("one");
    first_build(&w, &ws);
    let run = hushrun_run(&w, &ws);
    assert!(run.checked.units > 0, "{:?}", run.checked);
    assert!(run.checked.rebuilt.is_empty(), "{:?}", run.checked);
    assert_eq!(run.checked.stale, 0, "{:?}", run.checked);
    assert!(run.compiled.is_empty(), "{:#?}", run.compiled);
    assert!(run.lines.is_empty(), "{:?}", run.lines);
}

// covers: B-23
#[test]
fn a_unit_plain_cargo_built_since_loses_its_record_and_keeps_its_outputs() {
    let w = World::new();
    let ws = w.unseen("one");
    first_build(&w, &ws);
    // Plain cargo builds app again: new outputs the record doesn't describe.
    let lib = ws.join("app/src/lib.rs");
    fs::write(&lib, "embed::embed_dir!(\"data\");\n// plain\n").unwrap();
    let out = cargo(&ws, &["build"], &[]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("Compiling app"));
    let units = ws.join("target/debug/deps/.hushrun/units");
    let records = || -> Vec<String> {
        fs::read_dir(&units)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("app-"))
            .collect()
    };
    let before = records();
    assert!(!before.is_empty());
    // A file added now: the stale record is dropped, plain cargo's outputs stay, nothing is
    // compiled and nothing said; no run after it finds anything to do either.
    fs::write(ws.join("app/data/2.txt"), "two;").unwrap();
    let run = hushrun_run(&w, &ws);
    assert!(run.checked.stale >= 1, "{:?}", run.checked);
    assert!(run.checked.rebuilt.is_empty(), "{:?}", run.checked);
    assert!(run.compiled.is_empty(), "{:#?}", run.compiled);
    assert!(run.lines.is_empty(), "{:?}", run.lines);
    assert!(records().len() < before.len(), "{:?}", records());
    let again = hushrun_run(&w, &ws);
    assert_eq!(again.checked.stale, 0, "{:?}", again.checked);
    assert!(again.checked.rebuilt.is_empty() && again.compiled.is_empty());
}

// covers: B-1, B-23
#[test]
fn mode_off_checks_and_removes_nothing() {
    let w = World::new();
    let ws = w.unseen("one");
    first_build(&w, &ws);
    fs::write(ws.join("app/data/2.txt"), "two;").unwrap();
    // Off: no session, so no check; cargo alone decides.
    assert_eq!(w.begin(Mode::Off).unwrap_err(), NotBegun::Off);
    plain_cargo_compiles_nothing(&ws);
    assert_eq!(app_says(&ws), "one;");
    let units = ws.join("target/debug/deps/.hushrun/units");
    assert!(fs::read_dir(&units).unwrap().count() > 0);
    // Turned on again, the next run sees it.
    let run = hushrun_run(&w, &ws);
    assert_eq!(run.checked.rebuilt.len(), 1, "{:?}", run.checked);
    assert_eq!(app_says(&ws), "one;two;");
}
