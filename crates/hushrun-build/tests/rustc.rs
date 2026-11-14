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

/// Links `m` in `ws` as a proc macro (or, with `program`, as a program) through the wrapper,
/// with `env`, and gives the log entry and the linked file's bytes.
#[cfg(target_os = "macos")]
fn linked_with(w: &World, ws: &Path, program: bool, env: &[(&str, &str)]) -> (Entry, Vec<u8>) {
    let (kind, source, file, args): (_, _, _, &[&str]) = if program {
        ("bin", "fn main() { println!(\"hi\"); }\n", "out/m-x", &[])
    } else {
        (
            "proc-macro",
            "use proc_macro::TokenStream;\n\n#[proc_macro]\npub fn m(i: TokenStream) -> TokenStream {\n    i\n}\n",
            "out/libm-x.dylib",
            &["--extern", "proc_macro"],
        )
    };
    write(&ws.join("m/main.rs"), source);
    let (out, entry) = w.call_as(ws, "m", "m/main.rs", kind, args, env);
    ok(&out);
    (entry, fs::read(ws.join(file)).unwrap())
}

// covers: B-5
#[cfg(target_os = "macos")]
#[test]
fn a_variable_apples_linker_reads_by_itself_changes_a_linked_outputs_key() {
    // Apple's `ld` reads `LD_APPLICATION_EXTENSION_SAFE` (a dynamic library's header flag) and
    // `RC_UUID_SALT` (every output's UUID) from its own environment; nothing on the link line
    // says so.
    for (program, name, value) in [
        (false, "LD_APPLICATION_EXTENSION_SAFE", "1"),
        (true, "RC_UUID_SALT", "hello"),
    ] {
        let w = world();
        let ws = w.checkout("ws");
        let (first, plain) = linked_with(&w, &ws, program, &[]);
        assert_eq!(first.outcome, Outcome::BuiltAndStored, "{first:?}");
        let (second, with) = linked_with(&w, &ws, program, &[(name, value)]);
        assert_ne!(plain, with, "{name} changes the linker's output");
        assert!(!second.found, "{name}: the key must differ: {second:?}");
        assert_eq!(second.outcome, Outcome::BuiltAndStored, "{second:?}");
        assert_eq!(blocks(&w), 2, "{name}");
        // Without it again: the first block.
        let (third, again) = linked_with(&w, &ws, program, &[]);
        assert!(reused(&third), "{name}: {third:?}");
        assert_eq!(again, plain, "{name}");
    }
}

const PROC_MACRO: &str = "use proc_macro::TokenStream;\n\n#[proc_macro]\npub fn m(i: TokenStream) -> TokenStream {\n    i\n}\n";

/// A proc macro `m` linked in `ws` through the wrapper with `more` arguments and `env`: the log
/// entry.
fn proc_macro_with(w: &World, ws: &Path, more: &[&str], env: &[(&str, &str)]) -> Entry {
    write(&ws.join("m/lib.rs"), PROC_MACRO);
    let mut args = vec!["--extern", "proc_macro"];
    args.extend(more);
    let (out, entry) = w.call_as(ws, "m", "m/lib.rs", "proc-macro", &args, env);
    ok(&out);
    entry
}

/// The first `name` on `PATH`.
fn on_path(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("no {name} on PATH"))
}

/// The folder of its own that `rustc` hands the C driver to take programs from (`-B`), when it
/// links a proc macro with `more`: where rustc links with its own lld (x86-64 Linux since 1.90)
/// that is the toolchain's `gcc-ld`, and the driver takes the `ld` from there whatever `PATH`
/// says. Read from the link's own line, never told from the target.
fn own_ld_folder(rustc: &Path, more: &[&str]) -> Option<PathBuf> {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("m.rs"), PROC_MACRO);
    let out = Command::new(rustc)
        .args(["--crate-name", "m", "--edition=2021", "m.rs"])
        .args(["--crate-type", "proc-macro", "--extern", "proc_macro"])
        .args(["--out-dir", "out", "--print=link-args"])
        .args(more)
        .current_dir(dir.path())
        // As the wrapper's calls here: cargo's library folders would hand a copied rustc the
        // real toolchain's driver library, and its sysroot with it.
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .unwrap();
    ok(&out);
    let line = hushrun_build::link::parse(std::str::from_utf8(&out.stdout).unwrap()).unwrap();
    line.inputs(dir.path()).prefixes.into_iter().next()
}

/// A program at `path` that leaves `marker` and then runs `real` with what it was handed.
fn runs_and_marks(path: &Path, real: &Path, marker: &Path) {
    write(
        path,
        &format!(
            "#!/bin/sh\n: > '{}'\nexec '{}' \"$@\"\n",
            marker.display(),
            real.display()
        ),
    );
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// `PATH` with `first` before everything it holds.
fn path_after(first: &Path) -> String {
    let path = std::env::join_paths(
        std::iter::once(first.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

/// Puts in `dir` an `ld` and an `ld.lld` that leave `marker` and run the real `ld`.
fn put_an_ld(dir: &Path, marker: &Path) {
    let real = on_path("ld");
    for name in ["ld", "ld.lld"] {
        runs_and_marks(&dir.join(name), &real, marker);
    }
}

/// Whether an `ld` first on `PATH` is the one that runs when `rustc` links a proc macro with
/// `more`: seen from a link by rustc alone, no wrapper, with a marking `ld` put there. It is not
/// where rustc hands the driver its own folder, nor where the driver has its `ld` built in
/// (a gcc configured with one, as Ubuntu's since 26.04) or in a folder of its own.
fn the_ld_on_path_runs(rustc: &Path, more: &[&str]) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let early = dir.path().join("early");
    fs::create_dir_all(&early).unwrap();
    let marker = dir.path().join("ran");
    put_an_ld(&early, &marker);
    write(&dir.path().join("m.rs"), PROC_MACRO);
    let out = Command::new(rustc)
        .args(["--crate-name", "m", "--edition=2021", "m.rs"])
        .args(["--crate-type", "proc-macro", "--extern", "proc_macro"])
        .args(["--out-dir", "out"])
        .args(more)
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", path_after(&early))
        .output()
        .unwrap();
    ok(&out);
    marker.exists()
}

/// Links the proc macro with `more` twice with an empty folder first on `PATH`, puts there an
/// `ld` (and an `ld.lld`) that runs the real `ld`, and links again: that last link's entry, and
/// whether a program put there ran.
fn linked_after_an_ld_is_put_first_on_path(more: &[&str]) -> (Entry, bool) {
    let w = world();
    let ws = w.checkout("ws");
    let early = w.dir.path().join("early");
    fs::create_dir_all(&early).unwrap();
    let path = path_after(&early);
    let first = proc_macro_with(&w, &ws, more, &[("PATH", &path)]);
    assert_eq!(first.outcome, Outcome::BuiltAndStored, "{first:?}");
    let again = proc_macro_with(&w, &ws, more, &[("PATH", &path)]);
    assert!(reused(&again), "{again:?}");
    let marker = w.dir.path().join("ran");
    put_an_ld(&early, &marker);
    let after = proc_macro_with(&w, &ws, more, &[("PATH", &path)]);
    (after, marker.exists())
}

/// An `ld` put first on `PATH` between two links with `more` is another link exactly where
/// that `ld` is the one that runs (`from_path`); where it is not, the block is reused.
fn the_ld_on_path_counts_only_where_it_runs(more: &[&str], from_path: bool) {
    let (after, ran) = linked_after_an_ld_is_put_first_on_path(more);
    if from_path {
        assert!(!after.found, "the ld on PATH changed: {after:?}");
        assert_eq!(after.outcome, Outcome::BuiltAndStored, "{after:?}");
        assert!(ran, "the ld on PATH never ran");
    } else {
        assert!(reused(&after), "the ld on PATH never runs: {after:?}");
        assert!(!ran, "the ld on PATH ran");
    }
}

// covers: B-18
#[test]
fn an_ld_put_earlier_on_path_is_another_link_where_the_driver_takes_it_from_path() {
    // gcc runs `collect2`, which takes `ld` from `PATH` when gcc has none built in and its own
    // folders have none: an `ld` put earlier on `PATH` (here a script that runs the real one) is
    // another linker, with the same driver, line and files. Apple's driver runs the selected
    // Xcode's `ld` whatever `PATH` says; where rustc links with its own lld the driver takes it
    // from the folder rustc hands it; a gcc configured with its `ld` runs that one. There the
    // `ld` on `PATH` never runs and the block is still reused. Which it is here is seen from a
    // link, never told from the system.
    let from_path = !cfg!(target_os = "macos") && the_ld_on_path_runs(&real_rustc(), &[]);
    the_ld_on_path_counts_only_where_it_runs(&[], from_path);
}

// covers: B-18
#[test]
fn with_rustcs_own_lld_turned_off_the_ld_on_path_counts_where_the_driver_takes_it_from_path() {
    // `-C linker-features=-lld` is how a build asks for the system's linker where rustc would
    // link with its own: the line has no folder of rustc's then, and the driver decides as it
    // does for any other link.
    const OFF: &[&str] = &["-C", "linker-features=-lld"];
    let rustc = real_rustc();
    if own_ld_folder(&rustc, &[]).is_none() {
        // Nothing to turn off here, and a rustc that has none refuses the option.
        return;
    }
    assert_eq!(own_ld_folder(&rustc, OFF), None);
    assert!(!the_ld_on_path_runs(&rustc, &[]), "rustc's own lld is on");
    the_ld_on_path_counts_only_where_it_runs(OFF, the_ld_on_path_runs(&rustc, OFF));
}

/// `from`, a folder of a toolchain, again at `to`: every file a link to the real one, but
/// rustc's driver library and the files of `own`, which are copies.
fn mirror(from: &Path, to: &Path, own: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let (path, dest) = (entry.path(), to.join(entry.file_name()));
        if own.starts_with(&path) {
            mirror(&path, &dest, own);
        } else if from == own
            || entry
                .file_name()
                .to_string_lossy()
                .starts_with("librustc_driver")
        {
            fs::copy(&path, &dest).unwrap();
        } else {
            std::os::unix::fs::symlink(&path, &dest).unwrap();
        }
    }
}

// covers: B-18
#[test]
fn an_ld_put_in_the_folder_rustc_hands_the_driver_is_another_link() {
    // The counterpart of the `ld` on `PATH`, where rustc links with its own lld: the linker that
    // runs is the one in the toolchain's folder, so another one there is another link.
    let real = real_rustc();
    let Some(real_folder) = own_ld_folder(&real, &[]) else {
        return;
    };
    // A toolchain of this test's own, the real one but for that folder. rustc takes its sysroot
    // from where its driver library really lies, so that file and rustc are copies.
    let mut w = world();
    let sysroot = real.parent().unwrap().parent().unwrap();
    let own = w.dir.path().join("toolchain");
    mirror(&sysroot.join("lib"), &own.join("lib"), &real_folder);
    fs::create_dir_all(own.join("bin")).unwrap();
    fs::copy(&real, own.join("bin/rustc")).unwrap();
    w.rustc = own.join("bin/rustc");
    let folder = own_ld_folder(&w.rustc, &[]).expect("a folder of the copied toolchain");
    assert!(folder.starts_with(&own), "{}", folder.display());

    let ws = w.checkout("ws");
    let first = proc_macro_with(&w, &ws, &[], &[]);
    assert_eq!(first.outcome, Outcome::BuiltAndStored, "{first:?}");
    let again = proc_macro_with(&w, &ws, &[], &[]);
    assert!(reused(&again), "{again:?}");
    // rustc's `ld.lld` runs the `rust-lld` one folder up from its own: the real one is kept
    // beside the folder, under its name.
    let kept = folder.parent().unwrap().join("kept");
    fs::create_dir_all(&kept).unwrap();
    fs::rename(folder.join("ld.lld"), kept.join("ld.lld")).unwrap();
    let marker = w.dir.path().join("ran");
    runs_and_marks(&folder.join("ld.lld"), &kept.join("ld.lld"), &marker);
    let after = proc_macro_with(&w, &ws, &[], &[]);
    assert!(!after.found, "the toolchain's ld changed: {after:?}");
    assert_eq!(after.outcome, Outcome::BuiltAndStored, "{after:?}");
    assert!(marker.exists(), "the ld put there never ran");
}

/// `<dir>/libhrq` as a shared library whose `hrq()` returns `value`, named `version` inside
/// (its soname, or its install name on macOS), under the file name `file`.
fn shared_library(dir: &Path, file: &str, value: u32, version: u32) {
    fs::create_dir_all(dir).unwrap();
    write(
        &dir.join("hrq.c"),
        &format!("int hrq(void) {{ return {value}; }}\n"),
    );
    let name = if cfg!(target_os = "macos") {
        format!("-Wl,-install_name,@rpath/libhrq.{version}.dylib")
    } else {
        format!("-Wl,-soname,libhrq.so.{version}")
    };
    let made = Command::new("cc")
        .args(["-shared", "-fPIC", "hrq.c", &name, "-o", file])
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(made.success());
}

// covers: B-6, B-18
#[test]
fn a_library_found_in_a_folder_the_driver_hands_ld_is_keyed_by_its_bytes() {
    // `LIBRARY_PATH`'s text is keyed, but not what its folders hold: a `-l` found there (and in
    // the system's folders the same way) was keyed by nothing.
    let w = world();
    let ws = w.checkout("ws");
    let lib = w.dir.path().join("lp");
    let file = if cfg!(target_os = "macos") {
        "libhrq.dylib"
    } else {
        "libhrq.so"
    };
    shared_library(&lib, file, 1, 1);
    write(
        &ws.join("p/main.rs"),
        "#[link(name = \"hrq\")]\nunsafe extern \"C\" { fn hrq() -> i32; }\n\
         fn main() { println!(\"{}\", unsafe { hrq() }); }\n",
    );
    let lib_text = lib.to_str().unwrap().to_owned();
    let link = || {
        let (out, entry) = w.call_as(
            &ws,
            "p",
            "p/main.rs",
            "bin",
            &[],
            &[("LIBRARY_PATH", &lib_text)],
        );
        ok(&out);
        (entry, fs::read(ws.join("out/p-x")).unwrap())
    };
    let (first, before) = link();
    assert_eq!(first.outcome, Outcome::BuiltAndStored, "{first:?}");
    let (again, _) = link();
    assert!(reused(&again), "{again:?}");
    // Another library under the same name: the program the linker writes is another.
    shared_library(&lib, file, 2, 2);
    let (swapped, after) = link();
    assert!(before != after, "the program names the other library");
    assert!(
        !swapped.found,
        "the library -lhrq finds changed: {swapped:?}"
    );
    assert_eq!(swapped.outcome, Outcome::BuiltAndStored, "{swapped:?}");
    if cfg!(target_os = "macos") {
        return;
    }
    // GNU `ld` reads a text file there as a linker script, and takes the files it names.
    fs::remove_file(lib.join(file)).unwrap();
    shared_library(&lib, "libhrq_impl.so", 3, 3);
    write(
        &lib.join(file),
        &format!(
            "/* GNU ld script */\nGROUP ( {}/libhrq_impl.so )\n",
            lib.display()
        ),
    );
    let (scripted, _) = link();
    assert_eq!(scripted.outcome, Outcome::BuiltAndStored, "{scripted:?}");
    let (again, _) = link();
    assert!(reused(&again), "{again:?}");
    shared_library(&lib, "libhrq_impl.so", 4, 4);
    let (named, _) = link();
    assert!(!named.found, "the file the script names changed: {named:?}");
    // A script that does more than name files: built, not stored.
    write(
        &lib.join(file),
        &format!("SEARCH_DIR({})\nINPUT ( libhrq_impl.so )\n", lib.display()),
    );
    let (other, _) = link();
    assert_eq!(
        other.outcome,
        Outcome::BuiltNotStored(hushrun_build::model::NotStored::LinkInputs),
        "{other:?}"
    );
}

/// The stored description of the block a log entry names.
fn description_of(w: &World, entry: &Entry) -> serde_json::Value {
    let store = hushrun_build::store::Store::new(&w.session.current().store);
    store
        .block(&entry.key.expect("a block"))
        .unwrap()
        .expect("stored")
        .description
}

/// The roles of a known link environment's list, in order, each once.
fn link_roles(description: &serde_json::Value) -> Vec<String> {
    let items = description["link_environment"]["known"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("E unknown: {}", description["link_environment"]));
    let mut roles: Vec<String> = Vec::new();
    for item in items {
        let role = item["role"].as_str().unwrap().to_owned();
        if !roles.contains(&role) {
            roles.push(role);
        }
    }
    roles
}

// covers: B-37
#[test]
fn a_real_links_environment_is_known_and_lists_what_answered() {
    // The system's own C driver: gcc on Linux, Apple's clang on macOS.
    let w = world();
    let ws = w.checkout("ws");
    let entry = proc_macro_with(&w, &ws, &[], &[]);
    assert_eq!(entry.outcome, Outcome::BuiltAndStored, "{entry:?}");
    let description = description_of(&w, &entry);
    let roles = link_roles(&description);
    eprintln!("{}", description["link_environment"]);
    let want: &[&str] = if cfg!(target_os = "macos") {
        &[
            "dry-run",
            "program",
            "loads",
            "library",
            "toolchain",
            "sdk",
            "platform-version",
        ]
    } else {
        &[
            "dry-run",
            "driver",
            "program",
            "ld",
            "plugin",
            "file",
            "library",
            "script",
            "toolchain",
        ]
    };
    for role in want {
        assert!(roles.iter().any(|r| r == role), "{role} in {roles:?}");
    }
    // A loader variable set for the link: unknown, and why.
    let entry = proc_macro_with(&w, &ws, &[], &[("LD_PRELOAD", "/nonexistent/libhook.so")]);
    let description = description_of(&w, &entry);
    assert!(
        description["link_environment"]["unknown"]["reason"]
            .as_str()
            .is_some_and(|why| why.contains("LD_PRELOAD")),
        "{description}"
    );
}

// covers: B-24, B-10, B-37
#[test]
fn a_real_proc_macro_leaves_the_machine_where_its_environment_is_known_and_its_bytes_name_none() {
    // The system's own driver (gcc on Linux, Apple's clang on macOS) and a toolchain whose
    // path no output holds: on macOS because the link drops the debug map (`-Wl,-S`), which
    // named the standard library's objects by it.
    let w = world();
    let ws = w.checkout("ws");
    let entry = proc_macro_with(&w, &ws, &[], &[]);
    assert_eq!(entry.outcome, Outcome::BuiltAndStored, "{entry:?}");
    let portable = entry.portable.clone().unwrap();
    assert!(portable.portable, "{portable:?}");
}

// covers: B-9, B-16
#[cfg(target_os = "macos")]
#[test]
fn a_toolchain_installed_elsewhere_links_the_same_program_proc_macro_and_build_script() {
    // The same toolchain at another path (another user's rustup home, another machine): what it
    // links must be byte for byte what the first one linked, or a block could not go from one
    // to the other. In `observe` every block found is compiled anyway and compared (B-16).
    let mut w = world_in(Mode::Observe);
    let real = real_rustc();
    let sysroot = real.parent().unwrap().parent().unwrap();
    let build = |w: &World, ws: &Path| {
        write(&ws.join("p/main.rs"), "fn main() { println!(\"hi\"); }\n");
        write(&ws.join("m/lib.rs"), PROC_MACRO);
        write(
            &ws.join("b/build.rs"),
            "fn main() { println!(\"cargo:rerun-if-changed=x\"); }\n",
        );
        // As cargo's dev profile asks on macOS.
        let debug = ["-C", "debuginfo=2", "-C", "split-debuginfo=unpacked"];
        let (out, program) = w.call_as(ws, "p", "p/main.rs", "bin", &debug, &[]);
        ok(&out);
        let (out, proc_macro) = w.call_as(
            ws,
            "m",
            "m/lib.rs",
            "proc-macro",
            &[
                "-C",
                "debuginfo=2",
                "-C",
                "split-debuginfo=unpacked",
                "--extern",
                "proc_macro",
            ],
            &[],
        );
        ok(&out);
        let (out, script) = w.call_as(
            ws,
            "build_script_build",
            "b/build.rs",
            "bin",
            &debug,
            &[("CARGO_PKG_NAME", "b")],
        );
        ok(&out);
        [program, proc_macro, script]
    };
    let one = w.checkout("one");
    for entry in build(&w, &one) {
        assert_eq!(entry.outcome, Outcome::BuiltAndStored, "{entry:?}");
        let portable = entry.portable.clone().unwrap();
        assert!(portable.portable, "{entry:?}");
    }
    // The toolchain again, in this test's folder: rustc and its driver library are copies (it
    // takes its sysroot from where that library really lies), the rest links to the real one.
    let own = w.dir.path().join("another-home/toolchains/copy");
    mirror(
        &sysroot.join("lib"),
        &own.join("lib"),
        Path::new("/nonexistent"),
    );
    fs::create_dir_all(own.join("bin")).unwrap();
    fs::copy(&real, own.join("bin/rustc")).unwrap();
    w.rustc = own.join("bin/rustc");
    let two = w.checkout("a-second/longer-checkout");
    for entry in build(&w, &two) {
        assert!(entry.found, "the same key: {entry:?}");
        assert!(!entry.differed, "the same bytes: {entry:?}");
    }
}
