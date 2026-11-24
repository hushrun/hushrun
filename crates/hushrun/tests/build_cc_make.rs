//! C and C++ builds without CMake reuse their objects (B-230 to B-238, ADR 0119), end to end:
//! `hushrun run NAME -- COMMAND` in two worktrees at different paths, one store, COMMAND a plain
//! Makefile, an autotools project, a Meson project and a handwritten `build.ninja`. Each test is
//! a case where the second worktree would have compiled again, or where a plain build after the
//! run would have rebuilt or failed.
//!
//! They need a C compiler and GNU Make; the autotools test `autoreconf` (`HUSHRUN_TEST_AUTOTOOLS`
//! names the folder holding it), the Meson test `meson` (`HUSHRUN_TEST_MESON` names it), the
//! Ninja tests `ninja` and `cmake` (`HUSHRUN_TEST_CMAKE`, as the CTest tests). Without them they
//! say so and pass, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    "HUSHRUN_CC",
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "CMAKE_GENERATOR",
    "RUSTC_WRAPPER",
    "SOURCE_DATE_EPOCH",
    "CPATH",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "DEPENDENCIES_OUTPUT",
    "SUNPRO_DEPENDENCIES",
    "MAKEFILES",
    "MAKEFLAGS",
    "MFLAGS",
    "MAKELEVEL",
    "CC",
    "CXX",
    "CFLAGS",
    "CXXFLAGS",
];

/// A tool on `PATH` (or in the folders `var` names, separated as `PATH` is); `None`, said, when
/// it is missing and the tools aren't required.
fn tool(name: &str, var: &str) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = match std::env::var_os(var) {
        Some(named) => {
            let named = PathBuf::from(named);
            if named.is_file() {
                return Some(named);
            }
            std::env::split_paths(named.as_os_str()).collect()
        }
        None => std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect(),
    };
    if let Some(found) = dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()) {
        return Some(found);
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{name} is missing ({var}), and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {name} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    None
}

/// GNU Make and a C compiler, as `make` and `cc` on `PATH`.
fn have_make() -> bool {
    let gnu = Command::new("make")
        .arg("--version")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("GNU Make"));
    let cc = Command::new("cc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if gnu && cc {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("GNU Make or a C compiler is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!(
        "note: skipping: GNU Make or cc is missing (HUSHRUN_TEST_TOOLS=required makes this fail)"
    );
    false
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The sources every project builds: two units of a library that include a header, and a
/// program. `__FILE__` makes an object hold its path, which the prefix map makes the same at any.
fn sources(repo: &Path) {
    write(
        repo,
        "include/p.h",
        "#define P_VALUE 7\nint one(void);\nint two(void);\n",
    );
    write(
        repo,
        "src/one.c",
        "#include \"p.h\"\nconst char *one_where(void) { return __FILE__; }\nint one(void) { return P_VALUE; }\n",
    );
    write(
        repo,
        "src/two.c",
        "#include \"p.h\"\nconst char *two_where(void) { return __FILE__; }\nint two(void) { return P_VALUE + 1; }\n",
    );
    write(
        repo,
        "src/main.c",
        "#include \"p.h\"\nint main(void) { return one() + two() == 15 ? 0 : 1; }\n",
    );
}

/// Two worktrees of one repository, at different paths, sharing a hushrun cache.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    fn new(make: impl Fn(&Path)) -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let a = root.join("a");
        std::fs::create_dir_all(&a).unwrap();
        make(&a);
        git::init(&a, "sha1");
        git::commit_all(&a, "init");
        git::run(
            &root,
            &["clone", "-q", "--no-hardlinks", a.to_str().unwrap(), "b"],
            None,
        );
        std::fs::write(root.join("gitconfig"), "").unwrap();
        World { _dir: dir, root }
    }

    fn repo(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// `hushrun run build -- COMMAND` in worktree `name`, with `path` first on `PATH`.
    fn run(&self, name: &str, command: &[&str], path: &[PathBuf]) -> Output {
        let repo = self.repo(name);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.current_dir(&repo)
            .env("PWD", &repo)
            .env("PATH", with_path(path))
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", "http://127.0.0.1:9")
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_BUILD_AUDIT", "none")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .args(["run", "build", "--force", "--"])
            .args(command);
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{}", text(&out));
        out
    }

    /// COMMAND in worktree `name` with no hushrun: what a person types after the run.
    fn plain(&self, name: &str, command: &[&str], path: &[PathBuf]) -> Output {
        let mut cmd = Command::new(command[0]);
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.current_dir(self.repo(name))
            .env("PATH", with_path(path))
            .args(&command[1..]);
        cmd.output().unwrap()
    }
}

fn with_path(first: &[PathBuf]) -> std::ffi::OsString {
    let mut dirs = first.to_vec();
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(dirs).unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The `reused` and `built` counts of the run's C and C++ line.
fn counts(out: &Output) -> (usize, usize) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find(|l| l.contains("build: c/c++: ") && l.contains(" reused, "))
        .unwrap_or_else(|| panic!("no C and C++ line in:\n{stderr}"));
    let number = |word: &str| -> usize {
        let before = line.split(word).next().unwrap();
        before
            .rsplit(|c: char| !c.is_ascii_digit())
            .find(|w| !w.is_empty())
            .unwrap()
            .parse()
            .unwrap()
    };
    (number(" reused"), number(" built"))
}

/// How many of the run's compiles were stored: those built less those compiled as asked.
fn stored(out: &Output) -> usize {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let asked = stderr
        .lines()
        .find(|l| l.contains("build: c/c++: ") && l.contains(" compiled as asked"))
        .map_or(0, |line| {
            let at = line.find("build: c/c++: ").unwrap() + "build: c/c++: ".len();
            line[at..].split(' ').next().unwrap().parse().unwrap()
        });
    counts(out).1 - asked
}

/// The objects under `dir`, by path from it, with their bytes.
fn objects(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !path.ends_with(".git") && !path.ends_with("meson-private") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "o") {
                out.push((
                    path.strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

const MAKEFILE: &str = "CFLAGS = -O1 -Iinclude\n\
OBJ = src/one.o src/two.o src/main.o\n\
app: $(OBJ)\n\t$(CC) -o $@ $(OBJ)\n\
clean:\n\trm -f $(OBJ) app\n";

// covers: B-230, B-231, B-232
#[test]
fn a_plain_makefile_reuses_every_object_in_a_second_worktree_and_make_has_no_work_after() {
    if !have_make() {
        return;
    }
    let w = World::new(|repo| {
        sources(repo);
        write(repo, "Makefile", MAKEFILE);
        write(repo, ".gitignore", "*.o\napp\n");
    });
    let first = w.run("a", &["make"], &[]);
    assert_eq!(counts(&first), (0, 4), "{}", text(&first));
    let second = w.run("b", &["make"], &[]);
    // Three compiles reused; the link is the compiler's, as asked.
    assert_eq!(counts(&second), (3, 1), "{}", text(&second));
    assert_eq!(objects(&w.repo("a")), objects(&w.repo("b")));
    // Outside a run: make has nothing to do, and a clean build is the compiler's own.
    let q = w.plain("b", &["make", "-q"], &[]);
    assert!(q.status.success(), "{}", text(&q));
    let clean = w.plain("b", &["sh", "-c", "make clean && make"], &[]);
    assert!(clean.status.success(), "{}", text(&clean));
    assert!(!text(&clean).contains("hushrun"), "{}", text(&clean));
}

// covers: B-232
#[test]
fn make_started_by_a_script_and_by_make_reaches_the_launcher_and_a_makefiles_of_the_person_s_stays()
{
    if !have_make() {
        return;
    }
    let w = World::new(|repo| {
        sources(repo);
        write(repo, "Makefile", "all:\n\t$(MAKE) -C src -f ../inner.mk\n");
        write(
            repo,
            "inner.mk",
            "CFLAGS = -O1 -I../include\nall: one.o two.o\n\t@echo \"mine:$(MINE)\"\n",
        );
        write(repo, "mine.mk", "MINE = read\n");
        write(repo, ".gitignore", "*.o\n");
    });
    let mine = w.repo("a").join("mine.mk");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
    for var in CLEARED {
        cmd.env_remove(var);
    }
    let out = cmd
        .current_dir(w.repo("a"))
        .env("PWD", w.repo("a"))
        .env("MAKEFILES", &mine)
        .env("HUSHRUN_CACHE_DIR", w.root.join("cache"))
        .env("HUSHRUN_HOST", "http://127.0.0.1:9")
        .env("HUSHRUN_NO_UPDATE_CHECK", "1")
        .env("HUSHRUN_BUILD", "on")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", w.root.join("gitconfig"))
        .args(["run", "build", "--force", "--", "sh", "-c", "make"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("mine:read"), "{}", text(&out));
    assert_eq!(counts(&out), (0, 2), "{}", text(&out));
}

// covers: B-233
#[test]
fn cmake_started_by_make_names_the_real_compiler_in_its_cache() {
    if !have_make() {
        return;
    }
    let Some(cmake) = tool("cmake", "HUSHRUN_TEST_CMAKE") else {
        return;
    };
    let Some(ninja) = tool("ninja", "HUSHRUN_TEST_CMAKE") else {
        return;
    };
    let bin = vec![
        cmake.parent().unwrap().to_path_buf(),
        ninja.parent().unwrap().to_path_buf(),
    ];
    let w = World::new(|repo| {
        sources(repo);
        write(
            repo,
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.17)\nproject(p C)\ninclude_directories(include)\nadd_executable(app src/one.c src/two.c src/main.c)\n",
        );
        write(
            repo,
            "Makefile",
            "all:\n\tcmake -G Ninja -S . -B build\n\tcmake --build build\n",
        );
        write(repo, ".gitignore", "build/\n");
    });
    let out = w.run("a", &["make"], &bin);
    // Three compiles; CMake's link names the compiler by its path, without the launcher.
    assert_eq!(counts(&out).1, 3, "{}", text(&out));
    let cache = std::fs::read_to_string(w.repo("a").join("build/CMakeCache.txt")).unwrap();
    let compiler = cache
        .lines()
        .find_map(|l| l.strip_prefix("CMAKE_C_COMPILER:FILEPATH="))
        .unwrap();
    assert!(!compiler.contains("cc-programs"), "{compiler}");
    assert!(
        !compiler.contains(w.root.join("cache").to_str().unwrap()),
        "{compiler}"
    );
    // And it builds with hushrun's folders gone.
    std::fs::remove_dir_all(w.root.join("cache")).unwrap();
    std::fs::remove_file(w.repo("a").join("build/app")).unwrap();
    let plain = w.plain("a", &["cmake", "--build", "build"], &bin);
    assert!(plain.status.success(), "{}", text(&plain));
}

// covers: B-230, B-238
#[test]
fn an_autotools_project_reuses_its_objects_and_configure_names_the_compiler_by_name() {
    if !have_make() {
        return;
    }
    let Some(autoreconf) = tool("autoreconf", "HUSHRUN_TEST_AUTOTOOLS") else {
        return;
    };
    let bin = vec![autoreconf.parent().unwrap().to_path_buf()];
    let w = World::new(|repo| {
        sources(repo);
        write(
            repo,
            "configure.ac",
            "AC_INIT([p],[1])\nAM_INIT_AUTOMAKE([foreign subdir-objects])\nAC_PROG_CC\nAC_CONFIG_FILES([Makefile])\nAC_OUTPUT\n",
        );
        write(
            repo,
            "Makefile.am",
            "AM_CPPFLAGS = -I$(srcdir)/include\nbin_PROGRAMS = app\napp_SOURCES = src/one.c src/two.c src/main.c include/p.h\n",
        );
        let out = Command::new(&autoreconf)
            .arg("-fi")
            .current_dir(repo)
            .env("PATH", with_path(&bin))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out));
    });
    let build = ["sh", "-c", "./configure -q && make"];
    let first = w.run("a", &build, &bin);
    assert_eq!(counts(&first), (0, 4), "{}", text(&first));
    // A clone dates every file anew, so make in the second worktree runs automake, autoconf and
    // configure again first: their checks compile as asked, none stored or reused.
    let second = w.run("b", &build, &bin);
    assert_eq!(counts(&second).0, 3, "{}", text(&second));
    assert_eq!(stored(&second), 0, "{}", text(&second));
    let makefile = std::fs::read_to_string(w.repo("b").join("Makefile")).unwrap();
    let cc = makefile.lines().find(|l| l.starts_with("CC = ")).unwrap();
    assert!(!cc.contains('/'), "configure wrote down a path: {cc}");
    let q = w.plain("b", &["make", "-q"], &bin);
    assert!(q.status.success(), "{}", text(&q));
}

// covers: B-236, B-237
#[test]
fn a_meson_build_set_up_in_a_run_reuses_its_objects_and_builds_outside_one() {
    let Some(meson) = tool("meson", "HUSHRUN_TEST_MESON") else {
        return;
    };
    let Some(ninja) = tool("ninja", "HUSHRUN_TEST_CMAKE") else {
        return;
    };
    let bin = vec![
        meson.parent().unwrap().to_path_buf(),
        ninja.parent().unwrap().to_path_buf(),
    ];
    let w = World::new(|repo| {
        sources(repo);
        write(
            repo,
            "meson.build",
            "project('p', 'c')\ninc = include_directories('include')\nlib = static_library('l', 'src/one.c', 'src/two.c', include_directories: inc)\nexecutable('app', 'src/main.c', link_with: lib, include_directories: inc)\n",
        );
        write(repo, ".gitignore", "build/\nother/\n");
    });
    let build = ["sh", "-c", "meson setup build >/dev/null && ninja -C build"];
    let first = w.run("a", &build, &bin);
    assert!(counts(&first).1 >= 3, "{}", text(&first));
    let second = w.run("b", &build, &bin);
    assert_eq!(counts(&second).0, 3, "{}", text(&second));
    let compilers =
        std::fs::read_to_string(w.repo("b").join("build/meson-info/intro-compilers.json")).unwrap();
    assert!(compilers.contains("HUSHRUN_CC"), "{compilers}");
    assert_eq!(
        objects(&w.repo("a").join("build")),
        objects(&w.repo("b").join("build"))
    );

    // Outside a run, with hushrun's folders gone: no work, and a rebuild is the compiler's.
    std::fs::remove_dir_all(w.root.join("cache")).unwrap();
    let n = w.plain("b", &["ninja", "-C", "build", "-n"], &bin);
    assert!(text(&n).contains("no work to do"), "{}", text(&n));
    std::fs::remove_file(w.repo("b").join("build/app")).unwrap();
    let rebuilt = w.plain(
        "b",
        &["sh", "-c", "touch src/one.c && ninja -C build"],
        &bin,
    );
    assert!(rebuilt.status.success(), "{}", text(&rebuilt));

    // A build folder set up before the launcher is named.
    let set_up = w.plain("b", &[meson.to_str().unwrap(), "setup", "other"], &bin);
    assert!(set_up.status.success(), "{}", text(&set_up));
    let out = w.run("b", &["ninja", "-C", "other"], &bin);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Meson set it up before hushrun's launcher"),
        "{}",
        text(&out)
    );
}

// covers: B-235
#[test]
fn a_handwritten_build_ninja_reuses_its_objects_and_ninja_has_no_work_after() {
    let Some(ninja) = tool("ninja", "HUSHRUN_TEST_CMAKE") else {
        return;
    };
    let bin = vec![ninja.parent().unwrap().to_path_buf()];
    let w = World::new(|repo| {
        sources(repo);
        write(
            repo,
            "build.ninja",
            "cflags = -O1 -Iinclude\n\
             rule cc\n  command = cc -MD -MF $out.d $cflags -c $in -o $out\n  depfile = $out.d\n  deps = gcc\n\
             rule link\n  command = cc $in -o $out\n\
             build out/one.o: cc src/one.c\nbuild out/two.o: cc src/two.c\nbuild out/main.o: cc src/main.c\n\
             build out/app: link out/one.o out/two.o out/main.o\n",
        );
        write(repo, ".gitignore", "out/\n.ninja_*\n");
    });
    let first = w.run("a", &["ninja"], &bin);
    assert_eq!(counts(&first), (0, 4), "{}", text(&first));
    let second = w.run("b", &["ninja"], &bin);
    assert_eq!(counts(&second), (3, 1), "{}", text(&second));
    assert_eq!(objects(&w.repo("a")), objects(&w.repo("b")));
    let n = w.plain("b", &["ninja", "-n"], &bin);
    assert!(text(&n).contains("no work to do"), "{}", text(&n));
}
