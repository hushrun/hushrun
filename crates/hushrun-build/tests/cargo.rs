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
        let settings = self.dir.path().join("build.json");
        settings::update(&settings, |s| {
            s.mode = mode;
            s.min_free_gb = 0;
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
