//! Real rustc, called through the wrapper binary (`examples/hushrun-rustc.rs`) on tiny crates in
//! temp folders: inputs that change rustc's output must change the key, or the call must pass
//! through. Each test is a case where a reuse would have served the wrong bytes.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use hushrun_build::model::{Mode, Outcome, PassedThrough};
use hushrun_build::session::{self, Entry, Session, Start};
use hushrun_build::settings;

fn wrapper_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().unwrap().parent().unwrap();
    let wrapper = debug.join("examples").join("hushrun-rustc");
    assert!(wrapper.is_file(), "{} not built", wrapper.display());
    wrapper
}

/// The toolchain's own rustc, not the rustup proxy: the wrapper refuses a proxy with no
/// toolchain named.
fn real_rustc() -> PathBuf {
    let out = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--print", "sysroot"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let sysroot = String::from_utf8(out.stdout).unwrap();
    PathBuf::from(sysroot.trim()).join("bin").join("rustc")
}

struct World {
    dir: tempfile::TempDir,
    rustc: PathBuf,
    session: Session,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("build.json");
    settings::update(&settings, |s| {
        s.mode = Mode::On;
        s.min_free_gb = 0;
    })
    .unwrap();
    let home = std::env::var_os("HOME");
    let cargo_home = std::env::var_os("CARGO_HOME");
    let rustup_home = std::env::var_os("RUSTUP_HOME");
    let env = move |name: &str| match name {
        "HOME" => home.clone(),
        "CARGO_HOME" => cargo_home.clone(),
        "RUSTUP_HOME" => rustup_home.clone(),
        _ => None,
    };
    let session = session::begin(&Start {
        settings: &settings,
        cache_dir: Some(&dir.path().join("cache")),
        exe: &wrapper_exe(),
        in_ci: false,
        repository: None,
        env: &env,
        now: SystemTime::now(),
    })
    .unwrap();
    World {
        dir,
        rustc: real_rustc(),
        session,
    }
}

impl World {
    /// An empty checkout with an output folder.
    fn checkout(&self, name: &str) -> PathBuf {
        let ws = self.dir.path().join(name);
        fs::create_dir_all(ws.join("out")).unwrap();
        PathBuf::from(hushrun_build::roots::resolve_on_disk(ws.to_str().unwrap()).unwrap())
    }

    /// Runs the wrapper as cargo would, in `ws`, for crate `name` from `source`, with `more`
    /// arguments and `env` on top of a clean environment. Returns the process's output and the
    /// call's log entry.
    fn call(
        &self,
        ws: &Path,
        name: &str,
        source: &str,
        more: &[&str],
        env: &[(&str, &str)],
    ) -> (Output, Entry) {
        let link = &self.session.env()[0].1;
        let before = self.session.current().entries().len();
        // Files written in this test are older than the compile's start (B-11).
        std::thread::sleep(std::time::Duration::from_millis(30));
        let mut command = Command::new(link);
        command
            .arg(&self.rustc)
            .args(["--crate-name", name, "--edition=2021", source])
            .args([
                "--error-format=json",
                "--json=artifacts",
                "--crate-type",
                "lib",
            ])
            .args(["--emit=dep-info,metadata,link", "-C", "extra-filename=-x"])
            .arg("--out-dir")
            .arg(ws.join("out"))
            .args(more)
            .current_dir(ws)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("CARGO_PKG_NAME", name)
            .env("CARGO_PKG_VERSION", "0.1.0")
            .env("CARGO_MANIFEST_DIR", ws)
            .env(&self.session.env()[1].0, &self.session.env()[1].1);
        for (key, value) in env {
            command.env(key, value);
        }
        let out = command.output().unwrap();
        let entries = self.session.current().entries();
        assert_eq!(entries.len(), before + 1, "{entries:#?}");
        (out, entries[before].clone())
    }
}

/// Reused, or picked by the audit (B-16: one library compile in fifty that could be reused runs
/// anyway) and found the same.
fn reused(entry: &Entry) -> bool {
    entry.outcome == Outcome::Reused
        || (entry.outcome == Outcome::BuiltAndStored && entry.found && !entry.differed)
}

fn rlib(ws: &Path, name: &str) -> Vec<u8> {
    fs::read(ws.join(format!("out/lib{name}-x.rlib"))).unwrap()
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// `vendor/libfoo.a` in `ws`, whose `foo()` returns `value`.
fn static_library(ws: &Path, value: u32) {
    let vendor = ws.join("vendor");
    write(
        &vendor.join("foo.c"),
        &format!("int foo(void) {{ return {value}; }}\n"),
    );
    let cc = Command::new("cc")
        .args(["-c", "foo.c", "-o", "foo.o"])
        .current_dir(&vendor)
        .status()
        .unwrap();
    assert!(cc.success());
    let ar = Command::new("ar")
        .args(["crs", "libfoo.a", "foo.o"])
        .current_dir(&vendor)
        .status()
        .unwrap();
    assert!(ar.success());
}

const BUNDLE: &str = "unsafe extern \"C\" { pub fn foo() -> i32; }\n\
                      pub fn g() -> i32 { unsafe { foo() } }\n";

// covers: B-6
#[test]
fn a_static_library_found_through_dash_l_is_keyed_by_its_bytes() {
    let w = world();
    let args = ["-L", "native=vendor", "-l", "static=foo"];
    let one = w.checkout("one");
    write(&one.join("b/lib.rs"), BUNDLE);
    static_library(&one, 1);
    let (out, first) = w.call(&one, "b", "b/lib.rs", &args, &[]);
    ok(&out);
    assert_eq!(first.outcome, Outcome::BuiltAndStored, "{first:?}");

    // The library rustc bundles into the rlib changed: the rlib changes, so the block can't serve.
    let two = w.checkout("two-longer");
    write(&two.join("b/lib.rs"), BUNDLE);
    static_library(&two, 2);
    let (out, second) = w.call(&two, "b", "b/lib.rs", &args, &[]);
    ok(&out);
    assert_ne!(second.outcome, Outcome::Reused, "{second:?}");
    assert_ne!(rlib(&one, "b"), rlib(&two, "b"));

    // The same library bytes elsewhere: reused.
    let three = w.checkout("three");
    write(&three.join("b/lib.rs"), BUNDLE);
    fs::create_dir_all(three.join("vendor")).unwrap();
    // The folder's names count too (B-6's native folders): the same entries, one's bytes.
    for file in ["libfoo.a", "foo.c", "foo.o"] {
        fs::copy(
            one.join("vendor").join(file),
            three.join("vendor").join(file),
        )
        .unwrap();
    }
    let (out, third) = w.call(&three, "b", "b/lib.rs", &args, &[]);
    ok(&out);
    assert!(reused(&third), "{third:?}");
    assert_eq!(rlib(&one, "b"), rlib(&three, "b"));

    // A form whose file can't be found with certainty is built and not stored.
    let four = w.checkout("four");
    write(&four.join("b/lib.rs"), BUNDLE);
    static_library(&four, 1);
    let verbatim = ["-L", "native=vendor", "-l", "static:+verbatim=libfoo.a"];
    let (out, fourth) = w.call(&four, "b", "b/lib.rs", &verbatim, &[]);
    ok(&out);
    assert_eq!(
        fourth.outcome,
        Outcome::BuiltNotStored(hushrun_build::model::NotStored::NativeOutside),
        "{fourth:?}"
    );
}

const NO_CORE: &str = "#![feature(no_core, lang_items)]\n#![no_core]\n\
    #[lang = \"pointee_sized\"] pub trait PointeeSized {}\n\
    #[lang = \"meta_sized\"] pub trait MetaSized: PointeeSized {}\n\
    #[lang = \"sized\"] pub trait Sized: MetaSized {}\n\
    #[unsafe(no_mangle)] pub extern \"C\" fn f() {}\n";

// covers: B-5
#[test]
fn a_deployment_target_rustc_reads_without_telling_is_keyed() {
    let w = world();
    let args = ["--target", "aarch64-apple-ios", "--cap-lints", "allow"];
    let call = |ws: &Path, version: &str| {
        write(&ws.join("nc.rs"), NO_CORE);
        w.call(
            ws,
            "nc",
            "nc.rs",
            &args,
            &[
                ("RUSTC_BOOTSTRAP", "1"),
                ("IPHONEOS_DEPLOYMENT_TARGET", version),
            ],
        )
    };
    let one = w.checkout("one");
    let (out, first) = call(&one, "12.0");
    ok(&out);
    assert_eq!(first.outcome, Outcome::BuiltAndStored, "{first:?}");
    let two = w.checkout("two-longer");
    let (out, second) = call(&two, "16.0");
    ok(&out);
    assert_ne!(second.outcome, Outcome::Reused, "{second:?}");
    // rustc did read it: the outputs differ.
    assert_ne!(rlib(&one, "nc"), rlib(&two, "nc"));
    let three = w.checkout("three");
    let (out, third) = call(&three, "12.0");
    ok(&out);
    assert!(reused(&third), "{third:?}");
}

// covers: B-4
#[test]
fn a_target_that_is_not_one_of_the_compilers_own_passes_through() {
    let w = world();
    let ws = w.checkout("ws");
    write(&ws.join("nc.rs"), NO_CORE);
    // A target file next to the source, named by its bare name: rustc loads `./mytgt.json`.
    let spec = Command::new(&w.rustc)
        .args(["-Z", "unstable-options", "--print", "target-spec-json"])
        .args(["--target", "aarch64-apple-ios"])
        .env("RUSTC_BOOTSTRAP", "1")
        .output()
        .unwrap();
    assert!(spec.status.success());
    fs::write(ws.join("mytgt.json"), &spec.stdout).unwrap();
    let args = ["--target", "mytgt", "-Z", "unstable-options"];
    let (_, entry) = w.call(&ws, "nc", "nc.rs", &args, &[("RUSTC_BOOTSTRAP", "1")]);
    assert_eq!(
        entry.outcome,
        Outcome::PassedThrough(PassedThrough::TargetFile),
        "{entry:?}"
    );
}

// covers: B-6
#[test]
fn an_extern_found_through_a_search_folder_passes_through() {
    let w = world();
    let build = |ws: &Path, n: u32| {
        write(
            &ws.join("dir/foo.rs"),
            &format!("pub const N: u32 = {n};\n"),
        );
        let out = Command::new(&w.rustc)
            .args(["--crate-type", "lib", "--crate-name", "foo", "foo.rs"])
            .current_dir(ws.join("dir"))
            .output()
            .unwrap();
        ok(&out);
        write(&ws.join("user.rs"), "pub fn n() -> u32 { foo::N }\n");
    };
    let args = ["--extern", "foo", "-L", "crate=dir"];
    let one = w.checkout("one");
    build(&one, 2);
    let (out, first) = w.call(&one, "user", "user.rs", &args, &[]);
    ok(&out);
    let two = w.checkout("two-longer");
    build(&two, 3);
    let (out, second) = w.call(&two, "user", "user.rs", &args, &[]);
    ok(&out);
    for entry in [&first, &second] {
        assert_eq!(
            entry.outcome,
            Outcome::PassedThrough(PassedThrough::ExternWithoutPath),
            "{entry:?}"
        );
    }
    assert_ne!(rlib(&one, "user"), rlib(&two, "user"));
    // `--extern proc_macro` names the compiler's own crate: still handled.
    let three = w.checkout("three");
    write(&three.join("user.rs"), "pub fn n() -> u32 { 1 }\n");
    let (out, third) = w.call(&three, "user", "user.rs", &["--extern", "proc_macro"], &[]);
    ok(&out);
    assert_eq!(third.outcome, Outcome::BuiltAndStored, "{third:?}");
}
