//! Real rustc, called through the wrapper binary (`examples/hushrun-rustc.rs`) on tiny crates in
//! temp folders: inputs that change rustc's output must change the key, or the call must pass
//! through. Each test is a case where a reuse would have served the wrong bytes.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use hushrun_build::model::{Mode, Outcome, PassedThrough};
use hushrun_build::portable::Reason;
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
    world_in(Mode::On)
}

fn world_in(mode: Mode) -> World {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("build.json");
    settings::update(&settings, |s| {
        s.mode = mode;
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
        repository_env: &[],
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
        self.call_as(ws, name, source, "lib", more, env)
    }

    /// [`World::call`] for a crate of type `crate_type`.
    fn call_as(
        &self,
        ws: &Path,
        name: &str,
        source: &str,
        crate_type: &str,
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
                crate_type,
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

/// The blocks in the store.
fn blocks(w: &World) -> usize {
    let blocks = w.dir.path().join("cache/build/blocks");
    fs::read_dir(blocks)
        .map(|dirs| {
            dirs.flatten()
                .map(|d| fs::read_dir(d.path()).map_or(0, Iterator::count))
                .sum()
        })
        .unwrap_or(0)
}

// covers: B-18
#[test]
fn a_linked_output_compiled_again_from_the_same_inputs_has_the_same_key() {
    // On Linux rustc hands the linker a search folder of its own temporary folder
    // (`-L <out>/rustcXXXXXX/raw-dylibs`), new at each compile: it is no input, or every
    // compile of a program or a proc macro had a key of its own.
    let w = world_in(Mode::Observe);
    let ws = w.checkout("ws");
    write(
        &ws.join("m/lib.rs"),
        "use proc_macro::TokenStream;\n\n#[proc_macro]\npub fn m(_: TokenStream) -> TokenStream {\n    TokenStream::new()\n}\n",
    );
    let args = ["--extern", "proc_macro"];
    let (out, first) = w.call_as(&ws, "m", "m/lib.rs", "proc-macro", &args, &[]);
    ok(&out);
    assert_eq!(first.outcome, Outcome::BuiltAndStored, "{first:?}");
    assert_eq!(blocks(&w), 1);
    let (out, second) = w.call_as(&ws, "m", "m/lib.rs", "proc-macro", &args, &[]);
    ok(&out);
    assert!(second.found && !second.differed, "{second:?}");
    assert_eq!(
        blocks(&w),
        1,
        "the second compile's key differs from the first's"
    );
}

// covers: B-6
#[test]
fn a_proc_macros_metadata_is_found_and_is_the_same_whichever_way_it_was_linked() {
    // What a dependent names a movable proc macro by includes the crate metadata in it
    // (`hushrun_build::metadata`): it must be found in what rustc and the platform's linker
    // really write, stay the same when only the link changes, and change with the crate.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("m.rs");
    let compile = |name: &str, text: &str, link_arg: Option<&str>| -> PathBuf {
        write(&src, text);
        let out = dir.path().join(name);
        fs::create_dir_all(&out).unwrap();
        let mut command = Command::new(real_rustc());
        command
            .args([
                "--crate-name",
                "m",
                "--edition=2021",
                "--crate-type",
                "proc-macro",
            ])
            .args(["--extern", "proc_macro", "-C", "prefer-dynamic"])
            .arg(&src)
            .arg("--out-dir")
            .arg(&out);
        if let Some(arg) = link_arg {
            command.arg(format!("-Clink-arg={arg}"));
        }
        ok(&command.output().unwrap());
        out.join(format!("libm.{}", std::env::consts::DLL_EXTENSION))
    };
    const MACRO: &str = "use proc_macro::TokenStream;\n\n#[proc_macro]\npub fn m(_: TokenStream) -> TokenStream {\n    unreachable!(\"never {}\", 1)\n}\n";
    // A link option that changes the library's bytes, and no crate hash: another linker's work.
    let other_link = if cfg!(target_os = "macos") {
        "-Wl,-headerpad,0x4000"
    } else {
        "-Wl,--hash-style=sysv"
    };
    let one = compile("one", MACRO, None);
    let two = compile("two", MACRO, Some(other_link));
    assert_ne!(fs::read(&one).unwrap(), fs::read(&two).unwrap());
    let meta = hushrun_build::metadata::of(&one).expect("the metadata of a real proc macro");
    assert_eq!(hushrun_build::metadata::of(&two), Some(meta));
    let other = compile("other", &MACRO.replace("never", "not ever"), None);
    assert_ne!(hushrun_build::metadata::of(&other), Some(meta));
}

// covers: B-5, B-24
#[test]
fn target_cpu_native_is_resolved_by_the_real_compiler_and_still_reused() {
    let w = world();
    let call = |ws: &Path| {
        write(&ws.join("n.rs"), "pub fn n() -> u32 { 7 }\n");
        w.call(ws, "n", "n.rs", &["-C", "target-cpu=native"], &[])
    };
    let one = w.checkout("one");
    let (out, first) = call(&one);
    ok(&out);
    assert_eq!(first.outcome, Outcome::BuiltAndStored, "{first:?}");
    // Stored and reused here, never portable: rustc gives the code generator more of the CPU
    // than it prints (B-24 `native-cpu`).
    let portable = first.portable.unwrap();
    assert!(!portable.portable, "{portable:?}");
    assert_eq!(portable.reasons, [Reason::NativeCpu], "{portable:?}");
    // What `native` is here was asked once and kept, with a CPU's name in it.
    let memos = w.session.current().store.join("compilers");
    let native: Vec<String> = fs::read_dir(&memos)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("native-"))
        .map(|e| fs::read_to_string(e.path()).unwrap())
        .collect();
    assert_eq!(native.len(), 1, "{native:?}");
    let memo: serde_json::Value = serde_json::from_str(&native[0]).unwrap();
    assert!(
        !memo["native"]["cpu"].as_str().unwrap().is_empty(),
        "{memo}"
    );
    let two = w.checkout("two-longer");
    let (out, second) = call(&two);
    ok(&out);
    assert!(reused(&second), "{second:?}");
}
