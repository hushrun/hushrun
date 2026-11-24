//! C and C++ builds without CMake (B-230 to B-235, ADR 0119): GNU Make, the Makefiles autotools
//! writes, and a `build.ninja` no generator wrote reach the launcher through programs named as
//! the compilers they start, in a folder of the run's session that only make's and such a
//! ninja's commands find first on `PATH`.
//!
//! - **The folder** ([`make`]) holds `cc`, `c++`, `gcc`, `g++`, `clang` and `clang++`
//!   ([`COMPILERS`]), each a `/bin/sh` script that finds the next program of its name on `PATH`,
//!   past every folder like this one ([`MARKER`]), and replaces itself with
//!   `$HUSHRUN_CC <that program's path> <arguments>`, or with the program itself when
//!   `HUSHRUN_CC` names nothing it can run. The compiler gets `PATH` without the folder. It also
//!   holds `cmake` and `meson` ([`GUARDED`]), which start the real ones with `PATH` without the
//!   folder: both write down the absolute path of the compiler they find, and a build folder
//!   configured with one of these scripts as its compiler would fail once the run is gone.
//! - **GNU Make** reads the makefiles `MAKEFILES` names before any other: the run names
//!   [`HOOK`] there, which puts the folder first on `PATH` for make's commands ([`hook_text`]),
//!   but not in a build folder CMake configured, whose rules name their compilers by path. Make
//!   compares times, not commands, so nothing rebuilds between `make` in a run and out of one.
//! - **Ninja** reads no variable, and keys each output by its command's text, so nothing is
//!   written into `build.ninja`: a COMMAND that is `ninja …` itself, on a `build.ninja` neither
//!   CMake nor Meson wrote, gets the folder first on its `PATH` ([`ninja_folder`]).
//!
//! Nothing here is written outside the run's session, and nothing it writes is read once the run
//! is over: a plain `make` or `ninja` after it is the compiler's, byte for byte.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

/// The folder of the session that holds the programs (B-231).
pub const FOLDER: &str = "cc-programs";
/// A file in every folder of these programs, which they pass over when they look for the real
/// one: so one run's program never starts another's, or itself, for ever.
pub const MARKER: &str = ".hushrun-cc-programs";
/// The makefile `MAKEFILES` names (B-232), in [`FOLDER`].
pub const HOOK: &str = "hushrun.mk";
/// The variable GNU Make reads extra makefiles from, before the others.
pub const MAKEFILES: &str = "MAKEFILES";
/// The compilers make's commands start by name, and GNU Make's own defaults for `CC` and `CXX`
/// (`cc`, `g++`) among them (B-231).
pub const COMPILERS: &[&str] = &["cc", "c++", "gcc", "g++", "clang", "clang++"];
/// The tools that write down where they found a compiler on `PATH`, which never see the folder
/// (B-233). `meson` reaches hushrun's own `meson` when the run put one on `PATH` (B-234).
pub const GUARDED: &[&str] = &["cmake", "meson"];

/// The script of one program in `FOLDER`: `name` the program it finds, `launches` whether it
/// starts the launcher. POSIX `sh` only, the same on every machine and in every run.
pub fn script(name: &str, launches: bool) -> String {
    let end = if launches {
        "[ -x \"${HUSHRUN_CC-}\" ] && exec \"$HUSHRUN_CC\" \"$r\" \"$@\"\nexec \"$r\" \"$@\"\n"
    } else {
        "exec \"$r\" \"$@\"\n"
    };
    let what = if launches {
        "starts the next one on PATH through hushrun's C and C++ launcher (B-231)"
    } else {
        "starts the next one on PATH, which never sees this folder (B-233)"
    };
    format!(
        "#!/bin/sh\n\
         # hushrun run: {name} for make's commands, put on PATH by the run's makefile; {what}.\n\
         n={name}\n\
         r= p= k=\n\
         set -f\n\
         o=$IFS; IFS=:\n\
         for d in $PATH; do\n\
         \x20 e=$d; [ -n \"$d\" ] || d=.\n\
         \x20 [ -f \"$d/{MARKER}\" ] && continue\n\
         \x20 if [ -n \"$k\" ]; then p=$p:$e; else p=$e; k=1; fi\n\
         \x20 [ -z \"$r\" ] && [ -f \"$d/$n\" ] && [ -x \"$d/$n\" ] && r=$d/$n\n\
         done\n\
         IFS=$o\n\
         set +f\n\
         case $PATH in *:) p=$p: ;; esac\n\
         PATH=$p; export PATH\n\
         [ -n \"$r\" ] || {{ echo \"$n: not found\" >&2; exit 127; }}\n\
         {end}"
    )
}

/// The makefile `MAKEFILES` names (B-232): `dir` first on `PATH` for make's commands, once
/// however deep make starts make, and not where CMake configured the build folder.
pub fn hook_text(dir: &Path) -> String {
    let dir = dir.to_string_lossy();
    format!(
        "# hushrun run (B-232): make's commands find the C and C++ compilers of this run's folder\n\
         # first on PATH. Read only while the run names it in MAKEFILES.\n\
         ifeq ($(wildcard CMakeCache.txt),)\n\
         ifeq ($(filter {dir},$(subst :, ,$(PATH))),)\n\
         export PATH := {dir}:$(PATH)\n\
         endif\n\
         # GNU Make 3 starts a command line with no shell metacharacters itself, finding the\n\
         # program on its own PATH; it starts every line through the shell, which reads the PATH\n\
         # above, when SHELL isn't the text /bin/sh. A Makefile's own SHELL replaces this one\n\
         # (GNU Make 4 and later read the PATH above for every line).\n\
         ifneq ($(filter 3.%,$(MAKE_VERSION)),)\n\
         SHELL := /bin//sh\n\
         endif\n\
         endif\n"
    )
}

/// Writes the programs and the makefile in `session_folder` (B-231, B-232), replacing what an
/// earlier write left; the folder they are in.
#[cfg(unix)]
pub fn make(session_folder: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = session_folder.join(FOLDER);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(MARKER), b"")?;
    let write = |name: &str, text: &str, mode: u32| -> io::Result<()> {
        let temp = dir.join(format!(".{name}.tmp"));
        std::fs::write(&temp, text)?;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(mode))?;
        std::fs::rename(&temp, dir.join(name))
    };
    for name in COMPILERS {
        write(name, &script(name, true), 0o755)?;
    }
    for name in GUARDED {
        write(name, &script(name, false), 0o755)?;
    }
    write(HOOK, &hook_text(&dir), 0o644)?;
    Ok(dir)
}

/// Nothing on Windows, where the launcher is off.
#[cfg(not(unix))]
pub fn make(_session_folder: &Path) -> io::Result<PathBuf> {
    Err(io::Error::other("not on Windows"))
}

/// What `MAKEFILES` becomes (B-232): the hook first, then what the person set. `None` when the
/// hook's path holds a space, which `MAKEFILES` would split, or a `:`, which `PATH` would.
pub fn makefiles(hook: &Path, theirs: Option<&OsStr>) -> Option<OsString> {
    let text = hook.to_str()?;
    if text
        .chars()
        .any(|c| c.is_whitespace() || c == ':' || c == '$')
    {
        return None;
    }
    let mut value = OsString::from(text);
    if let Some(theirs) = theirs.filter(|t| !t.is_empty()) {
        value.push(" ");
        value.push(theirs);
    }
    Some(value)
}

/// Whether a folder is one CMake or Meson configured, whose rules name their compilers by path
/// and reach the launcher their own way (B-50, B-234).
pub fn generated(folder: &Path) -> bool {
    folder.join("CMakeCache.txt").is_file() || folder.join("meson-private").is_dir()
}

/// The folder COMMAND's `ninja` builds in, when COMMAND is `ninja …` itself, started by name, on
/// a `build.ninja` neither CMake nor Meson wrote (B-235): `-C <dir>` (the last one) from `cwd`,
/// else `cwd`. `None` for anything else.
pub fn ninja_folder(words: &[String], cwd: &Path) -> Option<PathBuf> {
    let (first, rest) = words.split_first()?;
    if first != "ninja" {
        return None;
    }
    let mut folder = cwd.to_path_buf();
    let mut file = PathBuf::from("build.ninja");
    let mut i = 0;
    while i < rest.len() {
        let word = rest[i].as_str();
        match word {
            "-C" => {
                folder = cwd.join(rest.get(i + 1)?);
                i += 1;
            }
            "-f" => {
                file = PathBuf::from(rest.get(i + 1)?);
                i += 1;
            }
            "--" => break,
            w if w.starts_with("-C") && w.len() > 2 => folder = cwd.join(&w[2..]),
            w if w.starts_with("-f") && w.len() > 2 => file = PathBuf::from(&w[2..]),
            _ => {}
        }
        i += 1;
    }
    let manifest = folder.join(&file);
    (manifest.is_file() && !generated(&folder) && !generated_manifest(&manifest)).then_some(folder)
}

/// Whether a `build.ninja` says CMake or Meson wrote it, in a folder that doesn't show it.
fn generated_manifest(manifest: &Path) -> bool {
    let Ok(text) = std::fs::read(manifest) else {
        return true;
    };
    let head = &text[..text.len().min(4096)];
    let head = String::from_utf8_lossy(head);
    head.contains("# CMAKE generated file")
        || head.contains("autogenerated by the Meson build system")
}

/// `PATH` with `dir` first (B-235); `None` when `dir` can't be on it.
pub fn path_with(dir: &Path, path: Option<&OsStr>) -> Option<OsString> {
    let mut parts = vec![dir.to_path_buf()];
    if let Some(path) = path {
        parts.extend(std::env::split_paths(path));
    }
    std::env::join_paths(parts).ok()
}

/// Whether `dir` is a folder of these programs, which a lookup of the real compiler passes over
/// (the link's linker among them, B-18).
pub fn is_programs_folder(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    /// A program on a folder of `PATH` that prints its own path, its arguments and `PATH`.
    fn fake(dir: &Path, name: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(
            &path,
            "#!/bin/sh\necho \"$0\"; for a in \"$@\"; do echo \"arg:$a\"; done; echo \"path:$PATH\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn start(programs: &Path, name: &str, path: &str, hushrun_cc: Option<&Path>) -> (String, i32) {
        let mut command = Command::new(programs.join(name));
        command.args(["-c", "a b.c"]).env("PATH", path);
        command.env_remove("HUSHRUN_CC");
        if let Some(cc) = hushrun_cc {
            command.env("HUSHRUN_CC", cc);
        }
        let out = command.output().unwrap();
        (
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            out.status.code().unwrap_or(-1),
        )
    }

    // covers: B-231
    #[test]
    fn a_compiler_program_starts_the_next_compiler_through_the_launcher_or_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session");
        let programs = make(&session).unwrap();
        let other = make(&tmp.path().join("other")).unwrap();
        let real = tmp.path().join("real");
        fake(&real, "gcc");
        let launcher = tmp.path().join("launcher");
        fake(&launcher, "hushrun-cc");
        // Nothing else on PATH: the scripts use the shell's own commands alone.
        let path = format!(
            "{}:{}:{}",
            programs.display(),
            other.display(),
            real.display()
        );
        let bare = real.display().to_string();

        // Inside a run: the launcher, with the real compiler's path, the arguments as given and
        // PATH without any folder of programs.
        let (out, code) = start(&programs, "gcc", &path, Some(&launcher.join("hushrun-cc")));
        assert_eq!(code, 0, "{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            [
                launcher.join("hushrun-cc").to_str().unwrap(),
                &format!("arg:{}", real.join("gcc").display()),
                "arg:-c",
                "arg:a b.c",
                &format!("path:{bare}"),
            ]
        );

        // No launcher, or one that is gone: the compiler itself.
        for gone in [None, Some(tmp.path().join("gone/hushrun-cc"))] {
            let (out, code) = start(&programs, "gcc", &path, gone.as_deref());
            assert_eq!(code, 0, "{out}");
            let lines: Vec<&str> = out.lines().collect();
            assert_eq!(
                lines,
                [
                    real.join("gcc").to_str().unwrap(),
                    "arg:-c",
                    "arg:a b.c",
                    &format!("path:{bare}"),
                ]
            );
        }

        // None on PATH: 127, as a shell says it.
        let (out, code) = start(&programs, "clang", &path, None);
        assert_eq!(code, 127);
        assert!(out.contains("clang: not found"), "{out}");
    }

    // covers: B-231
    #[test]
    fn empty_and_trailing_entries_of_path_stay_as_they_were() {
        let tmp = tempfile::tempdir().unwrap();
        let programs = make(&tmp.path().join("s")).unwrap();
        let real = tmp.path().join("real");
        fake(&real, "cc");
        let path = format!("{}::{}:/bin:", programs.display(), real.display());
        let (out, _) = start(&programs, "cc", &path, None);
        assert!(
            out.contains(&format!("path::{}:/bin:\n", real.display())),
            "{out}"
        );
    }

    // covers: B-233
    #[test]
    fn cmake_and_meson_never_see_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let programs = make(&tmp.path().join("s")).unwrap();
        let real = tmp.path().join("real");
        fake(&real, "cmake");
        let launcher = tmp.path().join("launcher");
        fake(&launcher, "hushrun-cc");
        let path = format!("{}:{}:/bin", programs.display(), real.display());
        let (out, _) = start(
            &programs,
            "cmake",
            &path,
            Some(&launcher.join("hushrun-cc")),
        );
        assert!(
            out.starts_with(&format!("{}\n", real.join("cmake").display())),
            "{out}"
        );
        assert!(
            out.contains(&format!("path:{}:/bin", real.display())),
            "{out}"
        );
        assert!(!out.contains("hushrun-cc"), "{out}");
    }

    // covers: B-232
    #[test]
    fn makefiles_names_the_hook_first_unless_its_path_would_split() {
        let hook = Path::new("/s/cc-programs/hushrun.mk");
        assert_eq!(makefiles(hook, None).unwrap(), "/s/cc-programs/hushrun.mk");
        assert_eq!(
            makefiles(hook, Some(OsStr::new("/x/mine.mk"))).unwrap(),
            "/s/cc-programs/hushrun.mk /x/mine.mk"
        );
        assert_eq!(makefiles(Path::new("/a b/hushrun.mk"), None), None);
        assert_eq!(makefiles(Path::new("/a:b/hushrun.mk"), None), None);
    }

    // covers: B-235
    #[test]
    fn only_a_ninja_command_on_a_build_no_generator_wrote_gets_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::write(root.join("build.ninja"), "rule cc\n  command = cc -c $in\n").unwrap();
        std::fs::write(root.join("out/build.ninja"), "rule cc\n").unwrap();
        assert_eq!(
            ninja_folder(&words("ninja"), root),
            Some(root.to_path_buf())
        );
        assert_eq!(
            ninja_folder(&words("ninja -j4 -C out all"), root),
            Some(root.join("out"))
        );
        assert_eq!(
            ninja_folder(&words("ninja -Cout"), root),
            Some(root.join("out"))
        );
        assert_eq!(ninja_folder(&words("/usr/bin/ninja"), root), None);
        assert_eq!(ninja_folder(&words("make"), root), None);
        assert_eq!(ninja_folder(&words("ninja -C missing"), root), None);
        // CMake's and Meson's, by their folder or their own first line.
        std::fs::write(root.join("out/CMakeCache.txt"), "").unwrap();
        assert_eq!(ninja_folder(&words("ninja -C out"), root), None);
        std::fs::write(
            root.join("build.ninja"),
            "# This is the build file for project \"p\"\n# It is autogenerated by the Meson build system.\nrule cc\n",
        )
        .unwrap();
        assert_eq!(ninja_folder(&words("ninja"), root), None);
    }

    // covers: B-232
    #[test]
    fn the_hook_puts_the_folder_first_once_and_not_in_a_cmake_build_folder() {
        let Ok(make_out) = Command::new("make").arg("--version").output() else {
            eprintln!("note: skipping: no make");
            return;
        };
        if !String::from_utf8_lossy(&make_out.stdout).contains("GNU Make") {
            eprintln!("note: skipping: not GNU Make");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let programs = make(&tmp.path().join("s")).unwrap();
        let work = tmp.path().join("w");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::write(
            work.join("Makefile"),
            "all:\n\t@echo \"top:$$PATH\"\n\t@$(MAKE) -s -C sub\n",
        )
        .unwrap();
        std::fs::write(work.join("sub/Makefile"), "all:\n\t@echo \"sub:$$PATH\"\n").unwrap();
        let run = |dir: &Path| {
            let out = Command::new("make")
                .arg("-s")
                .current_dir(dir)
                .env("PATH", "/usr/bin:/bin")
                .env(MAKEFILES, programs.join(HOOK))
                .env_remove("MAKEFLAGS")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let first = format!("{}:/usr/bin:/bin", programs.display());
        assert_eq!(run(&work), format!("top:{first}\nsub:{first}\n"));
        std::fs::write(work.join("CMakeCache.txt"), "").unwrap();
        std::fs::write(work.join("sub/Makefile"), "all:\n\t@echo \"sub:$$PATH\"\n").unwrap();
        assert!(run(&work).starts_with("top:/usr/bin:/bin\n"));
    }
}
