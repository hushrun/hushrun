//! Real C and C++ compilers, called through the launcher binary (`examples/hushrun-cc.rs`) on
//! small sources and CMake projects in temp folders (B-50 to B-62). Each test is a case where a
//! reuse would have served the wrong bytes, a build tool would have rebuilt, or the compiler's
//! own output would not have reached the person.
//!
//! The CMake tests need CMake and Ninja: `HUSHRUN_TEST_CMAKE` names the folders holding `cmake`
//! (and `ninja`), separated as `PATH` is; without it, the ones on `PATH`. They say so and pass
//! where those are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use hushrun_build::model::{Mode, Outcome};
use hushrun_build::session::{self, Entry, Session, Start};
use hushrun_build::settings;

fn launcher_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().unwrap().parent().unwrap();
    let launcher = debug.join("examples").join("hushrun-cc");
    assert!(launcher.is_file(), "{} not built", launcher.display());
    launcher
}

/// Variables a compile here must not inherit: cargo's `SOURCE_DATE_EPOCH` among them.
const CLEARED: &[&str] = &[
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
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "CMAKE_GENERATOR",
    "CMAKE_BUILD_TYPE",
    "HUSHRUN_CC",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_BUILD_AUDIT",
    "MAKEFLAGS",
    "CC",
    "CXX",
    "CFLAGS",
    "CXXFLAGS",
];

/// One machine's store and settings.
struct World {
    dir: tempfile::TempDir,
    mode: Mode,
    audit: &'static str,
}

fn world() -> World {
    world_in(Mode::On, "none")
}

fn world_in(mode: Mode, audit: &'static str) -> World {
    let dir = tempfile::tempdir().unwrap();
    World { dir, mode, audit }
}

/// One run: its session, with the launcher for the checkout at `root`.
struct Run {
    session: Session,
}

impl World {
    /// A folder under the world's, resolved.
    fn path(&self, name: &str) -> PathBuf {
        let p = self.dir.path().join(name);
        fs::create_dir_all(&p).unwrap();
        fs::canonicalize(p).unwrap()
    }

    /// A run of the checkout at `root`, with prefix maps or not.
    fn run(&self, root: &Path, maps: bool) -> Run {
        self.run_with(root, maps, &[])
    }

    fn run_with(&self, root: &Path, maps: bool, person: &[(&str, &str)]) -> Run {
        let settings = self.dir.path().join("build.json");
        let mode = self.mode;
        settings::update(&settings, |s| {
            s.mode = mode;
            s.min_free_gb = 0;
        })
        .unwrap();
        let home = std::env::var_os("HOME");
        let audit = self.audit;
        let person: Vec<(String, String)> = person
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let env = move |name: &str| -> Option<OsString> {
            if let Some((_, v)) = person.iter().find(|(n, _)| n == name) {
                return Some(v.into());
            }
            match name {
                "HOME" => home.clone(),
                "HUSHRUN_BUILD_AUDIT" => Some(audit.into()),
                _ => None,
            }
        };
        let mut session = session::begin(&Start {
            settings: &settings,
            cache_dir: Some(&self.dir.path().join("cache")),
            exe: &launcher_exe(),
            in_ci: false,
            repository: None,
            repository_env: &[],
            env: &env,
            now: SystemTime::now(),
        })
        .unwrap();
        session.set_cc(root, maps, &env).unwrap();
        Run { session }
    }

    fn store(&self) -> PathBuf {
        self.dir.path().join("cache").join("build")
    }
}

impl Run {
    /// The variables the run gives COMMAND, with the person's `PATH` and `HOME`.
    fn env(&self) -> Vec<(OsString, OsString)> {
        self.session
            .env()
            .into_iter()
            .filter(|(name, _)| name != "RUSTC_WRAPPER")
            .collect()
    }

    /// The launcher called as the indirection calls it, in `cwd`.
    fn launch(&self, cwd: &Path, compiler: &str, args: &[&str]) -> Output {
        self.launch_with(cwd, compiler, args, &[])
    }

    fn launch_with(
        &self,
        cwd: &Path,
        compiler: &str,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Output {
        // Files written in a test are older than the compile's start (B-11).
        std::thread::sleep(Duration::from_millis(30));
        let mut command = Command::new(self.session.launcher_link().unwrap());
        command.arg(compiler).args(args).current_dir(cwd);
        clean(&mut command);
        command.envs(self.env());
        for (name, value) in env {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    fn entries(&self) -> Vec<Entry> {
        self.session
            .current()
            .entries()
            .into_iter()
            .filter(|e| e.cc.is_some())
            .collect()
    }

    fn last(&self) -> Entry {
        self.entries().pop().expect("a line of the launcher's")
    }
}

fn clean(command: &mut Command) {
    for name in CLEARED {
        command.env_remove(name);
    }
}

/// The compiler as asked, with no launcher: what every call must give byte for byte.
fn plain(cwd: &Path, compiler: &str, args: &[&str]) -> Output {
    let mut command = Command::new(compiler);
    command.args(args).current_dir(cwd);
    clean(&mut command);
    command.output().unwrap()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn why(entry: &Entry) -> Option<String> {
    entry.cc.as_ref().and_then(|cc| cc.why.clone())
}

fn assert_same_output(a: &Output, b: &Output, what: &str) {
    assert_eq!(a.status.code(), b.status.code(), "{what}: status");
    assert_eq!(
        String::from_utf8_lossy(&a.stdout),
        String::from_utf8_lossy(&b.stdout),
        "{what}: stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&a.stderr),
        String::from_utf8_lossy(&b.stderr),
        "{what}: stderr"
    );
}

const C_OBJECT: &[&str] = &[
    "-Ifirst", "-Isecond", "-c", "src/a.c", "-o", "a.o", "-MD", "-MT", "a.o", "-MF", "a.o.d",
];

// covers: B-54
#[test]
fn a_header_created_in_an_earlier_include_folder_is_compiled_not_shadowed() {
    let w = world();
    let root = w.path("ck");
    write(&root.join("src/a.c"), "#include \"h.h\"\nint v = V;\n");
    write(&root.join("second/h.h"), "#define V 200\n");
    fs::create_dir_all(root.join("first")).unwrap();
    let run = w.run(&root, true);
    let out = run.launch(&root, "cc", C_OBJECT);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        run.last().outcome,
        Outcome::BuiltAndStored,
        "{:?}",
        why(&run.last())
    );
    let v200 = fs::read(root.join("a.o")).unwrap();

    // A header named the same, where the search looks first: ccache's direct mode served the
    // object built from second/h.h here.
    write(&root.join("first/h.h"), "#define V 100\n");
    run.launch(&root, "cc", C_OBJECT);
    assert_ne!(run.last().outcome, Outcome::Reused);
    let v100 = fs::read(root.join("a.o")).unwrap();
    assert_ne!(v100, v200);

    // Gone again: the first object is the one reused.
    fs::remove_file(root.join("first/h.h")).unwrap();
    run.launch(&root, "cc", C_OBJECT);
    assert_eq!(run.last().outcome, Outcome::Reused);
    assert_eq!(fs::read(root.join("a.o")).unwrap(), v200);
    // The depfile names this checkout's header.
    let deps = fs::read_to_string(root.join("a.o.d")).unwrap();
    assert!(deps.starts_with("a.o:"), "{deps}");
    assert!(deps.contains("second/h.h"), "{deps}");
}

// covers: B-54
#[test]
fn a_file_has_include_asked_about_appearing_is_compiled() {
    let w = world();
    let root = w.path("ck");
    write(
        &root.join("src/a.c"),
        "#if __has_include(\"opt.h\")\n#define V 107\n#else\n#define V 100\n#endif\nint v = V;\n",
    );
    let run = w.run(&root, true);
    let args = ["-c", "src/a.c", "-o", "a.o", "-MD", "-MF", "a.o.d"];
    assert!(run.launch(&root, "cc", &args).status.success());
    assert_eq!(
        run.last().outcome,
        Outcome::BuiltAndStored,
        "{:?}",
        why(&run.last())
    );
    let without = fs::read(root.join("a.o")).unwrap();
    write(&root.join("src/opt.h"), "\n");
    run.launch(&root, "cc", &args);
    assert_ne!(run.last().outcome, Outcome::Reused);
    assert_ne!(fs::read(root.join("a.o")).unwrap(), without);
    fs::remove_file(root.join("src/opt.h")).unwrap();
    run.launch(&root, "cc", &args);
    assert_eq!(run.last().outcome, Outcome::Reused);
    assert_eq!(fs::read(root.join("a.o")).unwrap(), without);
}

// covers: B-54
#[test]
fn a_read_file_using_the_time_is_never_stored_unless_the_date_is_fixed() {
    let w = world();
    let root = w.path("ck");
    write(&root.join("src/t.c"), "const char *t = __TIME__;\n");
    let run = w.run(&root, true);
    let args = ["-c", "src/t.c", "-o", "t.o", "-MD", "-MF", "t.o.d"];
    for _ in 0..2 {
        assert!(run.launch(&root, "cc", &args).status.success());
        let last = run.last();
        assert!(
            matches!(last.outcome, Outcome::BuiltNotStored(_)),
            "{last:?}"
        );
        assert_eq!(why(&last).as_deref(), Some("time-macro"));
    }
    // SOURCE_DATE_EPOCH gives __TIME__ a value, and is keyed.
    let fixed = [("SOURCE_DATE_EPOCH", "315532800")];
    run.launch_with(&root, "cc", &args, &fixed);
    assert_eq!(
        run.last().outcome,
        Outcome::BuiltAndStored,
        "{:?}",
        why(&run.last())
    );
    run.launch_with(&root, "cc", &args, &fixed);
    assert_eq!(run.last().outcome, Outcome::Reused);
    run.launch_with(&root, "cc", &args, &[("SOURCE_DATE_EPOCH", "315532801")]);
    assert_ne!(run.last().outcome, Outcome::Reused);
}

// covers: B-57, B-58
#[test]
fn a_warning_and_an_error_reach_the_person_byte_for_byte_built_reused_or_not_stored() {
    let w = world();
    let root = w.path("ck");
    write(
        &root.join("src/warn.c"),
        "int f(void) { int unused; return 0; }\n",
    );
    write(&root.join("src/bad.c"), "int f(void) { return nope; }\n");
    let warn = [
        "-Wall",
        "-c",
        "src/warn.c",
        "-o",
        "warn.o",
        "-MD",
        "-MF",
        "warn.o.d",
    ];
    let bad = [
        "-Wall",
        "-c",
        "src/bad.c",
        "-o",
        "bad.o",
        "-MD",
        "-MF",
        "bad.o.d",
    ];
    let plain_warn = plain(&root, "cc", &warn);
    let plain_deps = fs::read_to_string(root.join("warn.o.d")).unwrap();
    let plain_bad = plain(&root, "cc", &bad);
    assert!(!plain_warn.stderr.is_empty() && plain_warn.status.success());
    assert!(!plain_bad.status.success());
    fs::remove_file(root.join("warn.o.d")).unwrap();

    let run = w.run(&root, true);
    let built = run.launch(&root, "cc", &warn);
    assert_eq!(
        run.last().outcome,
        Outcome::BuiltAndStored,
        "{:?}",
        why(&run.last())
    );
    assert_same_output(&built, &plain_warn, "built");
    // The person's depfile is the compiler's own text.
    assert_eq!(
        fs::read_to_string(root.join("warn.o.d")).unwrap(),
        plain_deps
    );
    let reused = run.launch(&root, "cc", &warn);
    assert_eq!(run.last().outcome, Outcome::Reused);
    assert_same_output(&reused, &plain_warn, "reused");
    let failed = run.launch(&root, "cc", &bad);
    assert_same_output(&failed, &plain_bad, "error");
    assert!(matches!(run.last().outcome, Outcome::BuiltNotStored(_)));

    // A store that can't be written: built, not stored, the same bytes.
    let other = world();
    let run = other.run(&root, true);
    let blocks = other.store().join("blocks");
    let tmp = other.store().join("tmp");
    for dir in [&blocks, &tmp] {
        fs::create_dir_all(dir).unwrap();
        fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o500)).unwrap();
    }
    let unstored = run.launch(&root, "cc", &warn);
    for dir in [&blocks, &tmp] {
        fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    }
    assert!(
        matches!(run.last().outcome, Outcome::BuiltNotStored(_)),
        "{:?}",
        run.last()
    );
    assert_same_output(&unstored, &plain_warn, "not stored");

    // A call passed through is the compiler itself.
    let link = ["src/bad.c", "-o", "bad"];
    assert_same_output(
        &run.launch(&root, "cc", &link),
        &plain(&root, "cc", &link),
        "passed through",
    );
    assert_eq!(why(&run.last()).as_deref(), Some("no-compile"));
}

// covers: B-62
#[test]
fn without_a_session_the_launcher_is_the_compiler() {
    let w = world();
    let root = w.path("ck");
    write(&root.join("src/bad.c"), "int f(void) { return nope; }\n");
    let run = w.run(&root, true);
    let mut command = Command::new(run.session.launcher_link().unwrap());
    command
        .args(["cc", "-c", "src/bad.c", "-o", "bad.o"])
        .current_dir(&root);
    clean(&mut command);
    command.env("HUSHRUN_BUILD_SESSION", w.path("gone"));
    let out = command.output().unwrap();
    assert_same_output(
        &out,
        &plain(&root, "cc", &["-c", "src/bad.c", "-o", "bad.o"]),
        "no session",
    );
}

// covers: B-55, B-56
#[test]
fn without_prefix_maps_a_block_holding_its_path_is_tied_to_it() {
    let w = world();
    let a = w.path("a/ck");
    let b = w.path("b/other/ck");
    for root in [&a, &b] {
        write(&root.join("src/f.c"), "const char *f = __FILE__;\n");
    }
    let args = ["-c", "/ROOT/src/f.c", "-o", "f.o", "-MD", "-MF", "f.o.d"];
    let with_root = |root: &Path| -> Vec<String> {
        args.iter()
            .map(|a| a.replace("/ROOT", &root.to_string_lossy()))
            .collect()
    };
    for (maps, reused_in_b) in [(false, false), (true, true)] {
        for (root, expect_reuse) in [(&a, false), (&b, reused_in_b)] {
            let run = w.run(root, maps);
            let words = with_root(root);
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            assert!(run.launch(root, "cc", &words).status.success());
            let last = run.last();
            assert_eq!(
                last.outcome == Outcome::Reused,
                expect_reuse,
                "maps {maps}: {last:?}"
            );
            if !maps && last.outcome != Outcome::Reused {
                assert!(last.cc.unwrap().local.contains(&"tied".to_string()));
            }
        }
    }
}

// --- CMake ---------------------------------------------------------------------------------

struct Tools {
    bin: Vec<PathBuf>,
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn tools() -> Option<Tools> {
    let dirs: Vec<PathBuf> = match std::env::var_os("HUSHRUN_TEST_CMAKE") {
        Some(list) => std::env::split_paths(&list).collect(),
        None => [which("cmake"), which("ninja")]
            .into_iter()
            .flatten()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect(),
    };
    let has = |name: &str| dirs.iter().any(|d| d.join(name).is_file());
    if has("cmake") && has("ninja") {
        return Some(Tools { bin: dirs });
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("CMake and Ninja are missing (HUSHRUN_TEST_CMAKE), and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!(
        "note: skipping: CMake and Ninja are missing (HUSHRUN_TEST_TOOLS=required makes this fail)"
    );
    None
}

impl Tools {
    fn path(&self) -> OsString {
        let mut dirs = self.bin.clone();
        dirs.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(dirs).unwrap()
    }

    fn command(&self, program: &str, cwd: &Path, run: Option<&Run>) -> Command {
        let mut command = Command::new(program);
        command.current_dir(cwd);
        clean(&mut command);
        command.env("PATH", self.path());
        if let Some(run) = run {
            command.envs(run.env());
        }
        command
    }

    fn configure(&self, root: &Path, run: Option<&Run>) -> Output {
        let out = self
            .command("cmake", root, run)
            .args([
                "-S",
                ".",
                "-B",
                "build",
                "-G",
                "Ninja",
                "-DCMAKE_BUILD_TYPE=Release",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn ninja(&self, root: &Path, run: Option<&Run>, args: &[&str]) -> String {
        let out = self
            .command("ninja", &root.join("build"), run)
            .args(args)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{text}");
        text
    }
}

/// A small project: a library of C and C++ with a header folder, a test program, one CTest test.
fn project(root: &Path) {
    write(
        &root.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.16)\nproject(t C CXX)\nenable_testing()\n\
         add_library(lib STATIC src/lib.cc src/util.c)\n\
         target_include_directories(lib PUBLIC include)\n\
         add_executable(t_main tests/t_main.cc)\ntarget_link_libraries(t_main lib)\n\
         add_test(NAME t_main COMMAND t_main)\n",
    );
    write(
        &root.join("include/lib.h"),
        "#pragma once\n#ifdef __cplusplus\nextern \"C\" {\n#endif\nint util(int);\nconst char *where(void);\n#ifdef __cplusplus\n}\n#endif\nint lib(int);\n",
    );
    write(
        &root.join("src/util.c"),
        "#include \"lib.h\"\n#include <stdio.h>\nint util(int x) { return x * 2; }\nconst char *where(void) { return __FILE__; }\n",
    );
    write(
        &root.join("src/lib.cc"),
        "#include \"lib.h\"\n#include <string>\n#include <vector>\nint lib(int x) { std::vector<std::string> v(3, \"a\"); return util(x) + (int)v.size(); }\n",
    );
    write(
        &root.join("tests/t_main.cc"),
        "#include \"lib.h\"\n#include <cstdio>\nint main() { std::printf(\"%s\\n\", where()); return lib(2) == 7 ? 0 : 1; }\n",
    );
}

/// The objects of a build folder, by path from it, with their bytes.
fn objects(build: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![build.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "o") {
                out.push((
                    path.strip_prefix(build)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(&path).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

fn counts(run: &Run) -> (usize, usize) {
    let entries = run.entries();
    let reused = entries
        .iter()
        .filter(|e| e.outcome == Outcome::Reused)
        .count();
    let stored = entries
        .iter()
        .filter(|e| e.outcome == Outcome::BuiltAndStored)
        .count();
    (reused, stored)
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

// covers: B-50, B-55, B-56, B-57
#[test]
fn a_second_worktree_reuses_every_object_byte_for_byte_and_ninja_has_no_work_after() {
    let Some(tools) = tools() else { return };
    let w = world();
    let a = w.path("a/ck");
    let b = w.path("b/a-longer/checkout");
    project(&a);

    let run_a = w.run(&a, true);
    tools.configure(&a, Some(&run_a));
    tools.ninja(&a, Some(&run_a), &[]);
    let (reused, stored) = counts(&run_a);
    assert_eq!((reused, stored), (0, 3), "{:?}", run_a.entries());
    // CMake cached the indirection, not hushrun's link.
    let cache = fs::read_to_string(a.join("build/CMakeCache.txt")).unwrap();
    assert!(
        cache.contains("CMAKE_CXX_COMPILER_LAUNCHER:STRING=/bin/sh;-c;exec"),
        "{cache}"
    );
    assert!(!cache.contains("hushrun-cc"));

    // B's files are written after A's objects: a reused object must be dated now, not when A
    // compiled it, or Ninja and CTest take it for older than its sources.
    std::thread::sleep(Duration::from_millis(1100));
    project(&b);
    let run_b = w.run(&b, true);
    tools.configure(&b, Some(&run_b));
    tools.ninja(&b, Some(&run_b), &[]);
    assert_eq!(counts(&run_b), (3, 0), "{:?}", run_b.entries());
    let reused_objects = objects(&b.join("build"));
    assert_eq!(reused_objects.len(), 3);
    // The program built from reused objects runs and names its file from the root.
    let out = Command::new(b.join("build/t_main")).output().unwrap();
    assert!(out.status.success());
    let file = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        file.ends_with("src/util.c") && !file.starts_with('/'),
        "{file}"
    );

    // CTest's stale-program rule (ADR 0090, Amendment 8): each object newer than every file it
    // was built from, the program newer than its objects.
    let program = mtime(&b.join("build/t_main"));
    for (object, _) in &reused_objects {
        let object_time = mtime(&b.join("build").join(object));
        for source in [
            "src/lib.cc",
            "src/util.c",
            "tests/t_main.cc",
            "include/lib.h",
        ] {
            assert!(
                object_time > mtime(&b.join(source)),
                "{object} older than {source}"
            );
        }
        if !object.contains("t_main") {
            assert!(mtime(&b.join("build/liblib.a")) >= object_time);
        } else {
            assert!(program >= object_time);
        }
    }
    let ctest = tools
        .command("ctest", &b.join("build"), None)
        .output()
        .unwrap();
    assert!(
        ctest.status.success(),
        "{}",
        String::from_utf8_lossy(&ctest.stdout)
    );

    // Ninja has no work, in the run and out of it; the deps name this checkout's headers.
    assert!(
        tools
            .ninja(&b, Some(&run_b), &["-n"])
            .contains("no work to do")
    );
    assert!(tools.ninja(&b, None, &["-n"]).contains("no work to do"));
    let deps = tools.ninja(&b, None, &["-t", "deps"]);
    assert!(
        deps.contains(&format!("{}/include/lib.h", b.display())),
        "{deps}"
    );
    assert!(!deps.contains(&a.to_string_lossy().into_owned()), "{deps}");

    // A plain build and a run take turns: neither rebuilds what the other built.
    std::thread::sleep(Duration::from_millis(1100));
    write(
        &b.join("src/util.c"),
        "#include \"lib.h\"\n#include <stdio.h>\nint util(int x) { return x * 2; }\nconst char *where(void) { return __FILE__; }\n// edited\n",
    );
    tools.ninja(&b, None, &[]);
    assert!(
        tools
            .ninja(&b, Some(&run_b), &["-n"])
            .contains("no work to do")
    );
    write(
        &b.join("src/util.c"),
        "#include \"lib.h\"\n#include <stdio.h>\nint util(int x) { return x * 2; }\nconst char *where(void) { return __FILE__; }\n// edited again\n",
    );
    tools.ninja(&b, Some(&run_b), &[]);
    assert!(tools.ninja(&b, None, &["-n"]).contains("no work to do"));

    // A fresh compile of B with an empty store gives the same bytes the reuse put there.
    let fresh_world = world();
    let fresh_b = fresh_world.path("b/a-longer/checkout");
    project(&fresh_b);
    let fresh = fresh_world.run(&fresh_b, true);
    tools.configure(&fresh_b, Some(&fresh));
    tools.ninja(&fresh_b, Some(&fresh), &[]);
    assert_eq!(counts(&fresh), (0, 3));
    let fresh_objects = objects(&fresh_b.join("build"));
    let names = |v: &[(String, Vec<u8>)]| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    assert_eq!(names(&fresh_objects), names(&reused_objects));
    // The other world's root is another path: the same bytes only if nothing holds a path.
    let a_objects = objects(&a.join("build"));
    assert_eq!(
        a_objects,
        fresh_objects
            .iter()
            .map(|(n, b)| (n.clone(), b.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(reused_objects, a_objects);
}

// covers: B-50, B-62
#[test]
fn a_build_folder_configured_in_a_run_builds_outside_one_with_hushrun_gone() {
    let Some(tools) = tools() else { return };
    let w = world();
    let root = w.path("ck");
    project(&root);
    let run = w.run(&root, true);
    tools.configure(&root, Some(&run));
    tools.ninja(&root, Some(&run), &[]);
    // hushrun's link and binary are gone: outside a run nothing names them.
    let link = run.session.launcher_link().unwrap();
    fs::remove_file(&link).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    write(
        &root.join("src/lib.cc"),
        "#include \"lib.h\"\nint lib(int x) { return util(x) + 3; }\n",
    );
    let built = tools.ninja(&root, None, &[]);
    assert!(built.contains("lib.cc"), "{built}");
    assert!(
        Command::new(root.join("build/t_main"))
            .status()
            .unwrap()
            .success()
    );
    // A CMake re-run (an edited CMakeLists.txt) outside a run keeps the cache's line.
    write(
        &root.join("CMakeLists.txt"),
        &format!(
            "{}# edited\n",
            fs::read_to_string(root.join("CMakeLists.txt")).unwrap()
        ),
    );
    tools.ninja(&root, None, &[]);
    let cache = fs::read_to_string(root.join("build/CMakeCache.txt")).unwrap();
    assert!(cache.contains("CMAKE_C_COMPILER_LAUNCHER:STRING=/bin/sh;-c;exec"));
}

// covers: B-50
#[test]
fn a_launcher_the_person_set_wins_in_the_variables_and_in_the_cache() {
    let w = world();
    let root = w.path("ck");
    // In the variables: the run leaves the person's alone.
    let run = w.run_with(&root, true, &[("CMAKE_CXX_COMPILER_LAUNCHER", "ccache")]);
    let env = run.env();
    let names: Vec<&OsString> = env.iter().map(|(n, _)| n).collect();
    assert!(names.contains(&&OsString::from("CMAKE_C_COMPILER_LAUNCHER")));
    assert!(!names.contains(&&OsString::from("CMAKE_CXX_COMPILER_LAUNCHER")));
    assert_eq!(run.session.cc_launchers(), ["CMAKE_C_COMPILER_LAUNCHER"]);

    // In a build folder's cache: CMake keeps its launcher, and hushrun's never runs there.
    let Some(tools) = tools() else { return };
    project(&root);
    let fake = w.path("bin").join("ccache");
    let marker = w.path("bin").join("called");
    write(
        &fake,
        &format!("#!/bin/sh\necho x >> '{}'\nexec \"$@\"\n", marker.display()),
    );
    fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let out = tools
        .command("cmake", &root, None)
        .args(["-S", ".", "-B", "build", "-G", "Ninja"])
        .arg(format!("-DCMAKE_C_COMPILER_LAUNCHER={}", fake.display()))
        .arg(format!("-DCMAKE_CXX_COMPILER_LAUNCHER={}", fake.display()))
        .output()
        .unwrap();
    assert!(out.status.success());
    let run = w.run(&root, true);
    tools.configure(&root, Some(&run));
    tools.ninja(&root, Some(&run), &[]);
    assert!(run.entries().is_empty(), "{:?}", run.entries());
    assert_eq!(fs::read_to_string(&marker).unwrap().lines().count(), 3);
}

/// The one block's object in a store.
fn only_object(store: &Path) -> PathBuf {
    let mut found = Vec::new();
    for shard in fs::read_dir(store.join("blocks")).unwrap().flatten() {
        for block in fs::read_dir(shard.path()).unwrap().flatten() {
            found.push(block.path().join("files").join("object"));
        }
    }
    assert_eq!(found.len(), 1, "{found:?}");
    found.pop().unwrap()
}

// covers: B-59
#[test]
fn an_audited_reuse_compiles_compares_and_a_difference_suspends_the_key() {
    let mut w = world_in(Mode::On, "every");
    let root = w.path("ck");
    write(&root.join("src/a.c"), "int f(int x) { return x * 3; }\n");
    let args = ["-c", "src/a.c", "-o", "a.o", "-MD", "-MF", "a.o.d"];
    let run = w.run(&root, true);
    run.launch(&root, "cc", &args);
    assert_eq!(run.last().outcome, Outcome::BuiltAndStored);
    let compiled = fs::read(root.join("a.o")).unwrap();
    // Audited: compiled anyway and compared, the same bytes.
    run.launch(&root, "cc", &args);
    let last = run.last();
    assert!(last.found && !last.differed, "{last:?}");
    assert_ne!(last.outcome, Outcome::Reused);
    // A block whose object was altered, its size kept and its record made to match: nothing in
    // the build would notice, and the checks of its bytes pass (B-59).
    let object = only_object(&w.store());
    fs::set_permissions(&object, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    let honest = fs::read(&object).unwrap();
    let mut forged = honest.clone();
    let at = forged.len() / 2;
    forged[at] ^= 0xff;
    fs::write(&object, &forged).unwrap();
    let record = object
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("block.json");
    fs::set_permissions(&record, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    let sha = |b: &[u8]| hushrun_core::Digest::of(b).hex();
    let text = fs::read_to_string(&record).unwrap();
    assert!(text.contains(&sha(&honest)), "{text}");
    fs::write(&record, text.replace(&sha(&honest), &sha(&forged))).unwrap();
    run.launch(&root, "cc", &args);
    let last = run.last();
    assert!(last.found && last.differed, "{last:?}");
    assert_eq!(
        fs::read(root.join("a.o")).unwrap(),
        compiled,
        "the compile is served"
    );
    // The key is suspended: with the audit off, it compiles.
    w.audit = "none";
    let run = w.run(&root, true);
    run.launch(&root, "cc", &args);
    assert_ne!(run.last().outcome, Outcome::Reused, "{:?}", run.last());
    assert_eq!(fs::read(root.join("a.o")).unwrap(), compiled);
}

// covers: B-59
#[test]
fn a_stored_object_whose_bytes_are_not_its_record_s_is_compiled_not_served() {
    let w = world();
    let root = w.path("ck");
    write(&root.join("src/a.c"), "int f(int x) { return x * 5; }\n");
    let args = ["-c", "src/a.c", "-o", "a.o", "-MD", "-MF", "a.o.d"];
    let run = w.run(&root, true);
    run.launch(&root, "cc", &args);
    assert_eq!(run.last().outcome, Outcome::BuiltAndStored);
    let compiled = fs::read(root.join("a.o")).unwrap();
    let object = only_object(&w.store());
    fs::set_permissions(&object, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    let mut forged = compiled.clone();
    let at = forged.len() / 2;
    forged[at] ^= 0xff;
    fs::write(&object, &forged).unwrap();
    fs::remove_file(root.join("a.o")).unwrap();
    run.launch(&root, "cc", &args);
    let last = run.last();
    assert_eq!(last.outcome, Outcome::BuiltAndStored, "{last:?}");
    assert!(
        last.note
            .as_deref()
            .is_some_and(|n| n.contains("wrong-sha256")),
        "{last:?}"
    );
    assert_eq!(fs::read(root.join("a.o")).unwrap(), compiled);
    // The compile took its place: the next call reuses it.
    run.launch(&root, "cc", &args);
    assert_eq!(run.last().outcome, Outcome::Reused);
    assert_eq!(fs::read(root.join("a.o")).unwrap(), compiled);
}

// covers: B-59
#[test]
fn observing_compiles_everything_and_says_what_would_have_been_reused() {
    let w = world_in(Mode::Observe, "none");
    let root = w.path("ck");
    write(&root.join("src/a.c"), "int f(int x) { return x * 3; }\n");
    let args = ["-c", "src/a.c", "-o", "a.o", "-MD", "-MF", "a.o.d"];
    let run = w.run(&root, true);
    run.launch(&root, "cc", &args);
    assert!(!run.last().found);
    run.launch(&root, "cc", &args);
    let last = run.last();
    assert!(last.found && !last.differed, "{last:?}");
    assert_ne!(last.outcome, Outcome::Reused);
    // What the launcher stores is a C block.
    let block = only_object(&w.store());
    let record: serde_json::Value = serde_json::from_slice(
        &fs::read(block.parent().unwrap().parent().unwrap().join("block.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(record["description"]["kind"], "cc/1");
}

/// Asks a program what the identity asks, as the launcher does.
fn ask(program: &Path, args: &[String]) -> Option<hushrun_build::cc::compiler::Said> {
    let out = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .current_dir("/")
        .output()
        .ok()?;
    Some(hushrun_build::cc::compiler::Said {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn identity_of(compiler: &str, store: &Path) -> hushrun_build::cc::compiler::Identity {
    let path = std::env::var_os("PATH");
    let var = move |name: &str| (name == "PATH").then(|| path.clone()).flatten();
    hushrun_build::cc::compiler::identity(
        &OsString::from(compiler),
        hushrun_build::cc::args::Lang::Cxx,
        &[],
        &var,
        &hushrun_build::store::Store::new(store),
        &ask,
    )
    .unwrap()
}

// covers: B-52
#[test]
fn a_compiler_is_known_by_its_content_not_its_name() {
    let w = world();
    let root = w.path("ck");
    let bin = w.path("bin");
    // A compiler that is another's under a new name, then changed in place.
    let real = which("c++").unwrap();
    let fake = bin.join("my-c++");
    write(
        &fake,
        &format!("#!/bin/sh\nexec '{}' \"$@\"\n", real.display()),
    );
    fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let store = w.path("memos");
    let first = identity_of(fake.to_str().unwrap(), &store);
    // The memo answers while the file's stat holds.
    assert_eq!(identity_of(fake.to_str().unwrap(), &store), first);
    std::thread::sleep(Duration::from_millis(20));
    write(
        &fake,
        &format!("#!/bin/sh\n# patched\nexec '{}' \"$@\"\n", real.display()),
    );
    let patched = identity_of(fake.to_str().unwrap(), &store);
    assert_ne!(patched.digest, first.digest);
    assert_eq!(patched.target, first.target);

    // A block built by one isn't reused by the other.
    write(&root.join("src/a.cc"), "int f() { return 1; }\n");
    let args = ["-c", "src/a.cc", "-o", "a.o", "-MD", "-MF", "a.o.d"];
    let run = w.run(&root, true);
    run.launch(&root, real.to_str().unwrap(), &args);
    assert_eq!(run.last().outcome, Outcome::BuiltAndStored);
    run.launch(&root, fake.to_str().unwrap(), &args);
    assert_ne!(run.last().outcome, Outcome::Reused);

    // On macOS, /usr/bin's trampoline is the toolchain's clang that xcrun names.
    if cfg!(target_os = "macos") {
        let named = Command::new("/usr/bin/xcrun")
            .args(["-f", "clang++"])
            .output()
            .unwrap();
        let clang = String::from_utf8(named.stdout).unwrap();
        let through = identity_of("/usr/bin/c++", &store);
        assert!(through.clang && through.sdk.is_some());
        let direct = identity_of(clang.trim(), &store);
        assert_eq!(through.toolchain, direct.toolchain);
    } else if !identity_of("c++", &store).clang {
        // GCC: its cc1plus and as are in the identity, as the driver names them, and the memo
        // answers only while they keep their stat.
        let named: Vec<String> = fs::read_dir(store.join("compilers"))
            .unwrap()
            .flatten()
            .filter_map(|e| fs::read(e.path()).ok())
            .filter_map(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .flat_map(|memo| {
                memo["files"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|f| f["path"].as_str().map(str::to_string))
            })
            .collect();
        assert!(named.iter().any(|p| p.ends_with("/cc1plus")), "{named:?}");
        assert!(
            named
                .iter()
                .any(|p| p.ends_with("/as") || p.contains("-as")),
            "{named:?}"
        );
    }
}
