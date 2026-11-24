//! Meson (B-236, B-237, ADR 0119): `meson setup` inside a run names its compilers through the
//! same indirection CMake caches (B-50), so the build folder's `build.ninja` starts
//! `/bin/sh -c '…' <compiler> <arguments>`, which is the compiler outside a run and the launcher
//! inside one.
//!
//! Meson has no launcher variable: it reads its compilers from `CC` and `CXX` when it sets a
//! build folder up, splits them as a shell would, and keeps them for the folder's life (a
//! `--reconfigure` keeps them too). So only `meson setup` gets them, from hushrun's own `meson`
//! on `PATH` ([`setup_vars`]), never the rest of COMMAND: `CC` reaches every tool, and the Rust
//! `cc` crate reruns every build script when it changes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The compilers Meson looks for when `CC` and `CXX` are unset, in its order, on every system
/// hushrun's launcher runs on.
const DEFAULTS: [(&str, &[&str]); 2] = [
    ("CC", &["cc", "gcc", "clang"]),
    ("CXX", &["c++", "g++", "clang++"]),
];

/// Programs that are launchers themselves: a `CC` naming one is left as the person set it.
const LAUNCHERS: &[&str] = &["ccache", "sccache", "distcc", "icecc", "buildcache"];

/// The shell line of the indirection (B-50), as Meson splits `CC`: `/bin/sh`, `-c`, the script
/// in single quotes.
fn indirection() -> String {
    let script = super::INDIRECTION.split(';').nth(2).unwrap_or_default();
    format!("/bin/sh -c '{script}'")
}

/// A word quoted for a shell, as Meson's split reads it back.
fn quoted(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+=,@%".contains(&b))
    {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Whether `args`, a `meson` command's arguments, set a build folder up: `meson setup …`.
pub fn is_setup(args: &[OsString]) -> bool {
    args.first().is_some_and(|a| a == "setup")
}

/// The variables a `meson setup` with `args` starts with inside a run (B-236): `CC` and `CXX`,
/// each the indirection and a compiler's absolute path, when the run has a launcher
/// (`HUSHRUN_CC` names a program it can run). A variable the person set to one word (a
/// compiler's name or path, not a launcher's) keeps that compiler behind the indirection; one of
/// several words (`ccache gcc`, `gcc -m32`) is left as it is, as is a compiler not found. Unset,
/// Meson's first default found on `PATH`, past every folder of hushrun's programs. Empty for
/// anything else.
pub fn setup_vars(
    args: &[OsString],
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(OsString, OsString)> {
    if cfg!(windows) || !is_setup(args) {
        return Vec::new();
    }
    let runnable = var(super::HUSHRUN_CC)
        .filter(|v| !v.is_empty())
        .is_some_and(|v| runnable(Path::new(&v)));
    if !runnable {
        return Vec::new();
    }
    let path = var("PATH");
    let mut out = Vec::new();
    for (name, defaults) in DEFAULTS {
        let compiler = match var(name).filter(|v| !v.is_empty()) {
            Some(theirs) => {
                let Some(theirs) = theirs.to_str().map(str::trim) else {
                    continue;
                };
                if theirs.split_whitespace().count() != 1
                    || theirs.starts_with("/bin/sh")
                    || LAUNCHERS.contains(&file_name(theirs))
                {
                    continue;
                }
                find(theirs, path.as_ref())
            }
            None => defaults.iter().find_map(|d| find(d, path.as_ref())),
        };
        let Some(compiler) = compiler.and_then(|c| c.to_str().map(str::to_string)) else {
            continue;
        };
        out.push((
            OsString::from(name),
            OsString::from(format!("{} {}", indirection(), quoted(&compiler))),
        ));
    }
    out
}

fn file_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// The program `word` reaches: a path as given, a name the first on `path` past folders of
/// hushrun's programs (both kinds), resolved to an absolute path.
fn find(word: &str, path: Option<&OsString>) -> Option<PathBuf> {
    if word.contains('/') {
        let given = PathBuf::from(word);
        let given = if given.is_absolute() {
            given
        } else {
            std::env::current_dir().ok()?.join(given)
        };
        return runnable(&given).then_some(given);
    }
    std::env::split_paths(path?)
        .filter(|dir| {
            !super::programs::is_programs_folder(dir) && !dir.join(".hushrun-programs").exists()
        })
        .map(|dir| {
            if dir.as_os_str().is_empty() {
                PathBuf::from(".").join(word)
            } else {
                dir.join(word)
            }
        })
        .find(|candidate| runnable(candidate))
        .and_then(|found| {
            if found.is_absolute() {
                Some(found)
            } else {
                std::env::current_dir().ok().map(|cwd| cwd.join(found))
            }
        })
}

#[cfg(unix)]
fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn runnable(_path: &Path) -> bool {
    false
}

/// A build folder COMMAND names that Meson set up before the launcher (B-237): its compilers,
/// as Meson keeps them (`meson-info/intro-compilers.json`), name neither the indirection nor a
/// launcher of the person's.
pub fn set_up_before(folder: &Path) -> bool {
    let Ok(text) = std::fs::read(folder.join("meson-info/intro-compilers.json")) else {
        return false;
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&text) else {
        return false;
    };
    let Some(host) = json.get("host").and_then(|h| h.as_object()) else {
        return false;
    };
    let mut any = false;
    for (lang, compiler) in host {
        if !matches!(lang.as_str(), "c" | "cpp") {
            continue;
        }
        let Some(first) = compiler
            .get("exelist")
            .and_then(|e| e.as_array())
            .and_then(|e| e.first())
            .and_then(|e| e.as_str())
        else {
            continue;
        };
        if first == "/bin/sh" || LAUNCHERS.contains(&file_name(first)) {
            return false;
        }
        any = true;
    }
    any
}

/// The folders a COMMAND may name to Meson as build folders: `meson compile|test|install|… -C
/// <dir>`, and each word of `meson setup …` that isn't an option (an option's value among them,
/// which the caller's look for `meson-private` leaves out), each from `cwd`.
pub fn named_folders(words: &[String], cwd: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if file_name(&words[i]) != "meson" {
            i += 1;
            continue;
        }
        let setup = words.get(i + 1).is_some_and(|w| w == "setup");
        let mut j = i + 1;
        while j < words.len() && !matches!(words[j].as_str(), "&&" | "||" | ";" | "|") {
            let word = words[j].as_str();
            let dir = match word {
                "-C" => words.get(j + 1).inspect(|_| j += 1).map(String::as_str),
                w if w.starts_with("-C") && w.len() > 2 => Some(&w[2..]),
                w if setup && j > i + 1 && !w.starts_with('-') => Some(w),
                _ => None,
            };
            if let Some(dir) = dir {
                let path = cwd.join(dir);
                if !found.contains(&path) {
                    found.push(path);
                }
            }
            j += 1;
        }
        i = j;
    }
    found
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn exe(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    // covers: B-236
    #[test]
    fn setup_gets_the_indirection_in_front_of_the_compiler_meson_would_have_found() {
        let tmp = tempfile::tempdir().unwrap();
        let programs = super::super::programs::make(&tmp.path().join("s")).unwrap();
        let bin = tmp.path().join("bin");
        let gcc = exe(&bin, "gcc");
        let gxx = exe(&bin, "g++");
        let launcher = exe(tmp.path(), "hushrun-cc");
        let path = std::env::join_paths([programs.clone(), bin.clone()]).unwrap();
        let env = |cc: Option<&str>, launcher: Option<&Path>| {
            let path = path.clone();
            let cc = cc.map(OsString::from);
            let launcher = launcher.map(|l| l.as_os_str().to_os_string());
            move |name: &str| match name {
                "PATH" => Some(path.clone()),
                "CC" => cc.clone(),
                "HUSHRUN_CC" => launcher.clone(),
                _ => None,
            }
        };
        let vars = setup_vars(&os(&["setup", "build"]), &env(None, Some(&launcher)));
        let ind = indirection();
        assert_eq!(
            vars,
            [
                ("CC".into(), format!("{ind} {}", gcc.display()).into()),
                ("CXX".into(), format!("{ind} {}", gxx.display()).into()),
            ]
        );
        // As Meson splits it: four words, the last the compiler.
        let value = vars[0].1.to_str().unwrap();
        assert!(
            value.starts_with("/bin/sh -c '[ -x \"$HUSHRUN_CC\" ]"),
            "{value}"
        );

        // The person's one-word compiler stays behind it; several words, or a launcher, stay
        // as they are.
        let clang = exe(&bin, "clang-19");
        let vars = setup_vars(
            &os(&["setup", "b"]),
            &env(Some("clang-19"), Some(&launcher)),
        );
        assert_eq!(
            vars[0].1,
            OsString::from(format!("{ind} {}", clang.display()))
        );
        for theirs in ["ccache gcc", "gcc -m32", "/usr/bin/ccache"] {
            let vars = setup_vars(&os(&["setup", "b"]), &env(Some(theirs), Some(&launcher)));
            assert!(vars.iter().all(|(n, _)| n != "CC"), "{theirs}: {vars:?}");
        }

        // No launcher, a gone one, or not a setup: nothing.
        assert!(setup_vars(&os(&["setup", "b"]), &env(None, None)).is_empty());
        let gone = tmp.path().join("gone/hushrun-cc");
        assert!(setup_vars(&os(&["setup", "b"]), &env(None, Some(&gone))).is_empty());
        assert!(setup_vars(&os(&["compile", "-C", "b"]), &env(None, Some(&launcher))).is_empty());
    }

    // covers: B-236
    #[test]
    fn a_compiler_path_with_a_space_is_quoted_as_meson_splits_it() {
        assert_eq!(quoted("/usr/bin/cc"), "/usr/bin/cc");
        assert_eq!(quoted("/opt/my cc/gcc"), "'/opt/my cc/gcc'");
        assert_eq!(quoted("/o'k/gcc"), "'/o'\\''k/gcc'");
    }

    // covers: B-237
    #[test]
    fn a_folder_whose_compilers_name_no_indirection_was_set_up_before() {
        let tmp = tempfile::tempdir().unwrap();
        let info = tmp.path().join("meson-info");
        std::fs::create_dir_all(&info).unwrap();
        let write = |exelist: &str| {
            std::fs::write(
                info.join("intro-compilers.json"),
                format!("{{\"host\": {{\"c\": {{\"exelist\": {exelist}}}}}, \"build\": {{}}}}"),
            )
            .unwrap();
        };
        write("[\"cc\"]");
        assert!(set_up_before(tmp.path()));
        write("[\"/bin/sh\", \"-c\", \"…\", \"/usr/bin/cc\"]");
        assert!(!set_up_before(tmp.path()));
        write("[\"ccache\", \"cc\"]");
        assert!(!set_up_before(tmp.path()));
        assert!(!set_up_before(&tmp.path().join("none")));
    }

    // covers: B-237
    #[test]
    fn the_folders_meson_commands_name() {
        let words = |t: &str| -> Vec<String> { t.split_whitespace().map(str::to_string).collect() };
        let cwd = Path::new("/w");
        assert_eq!(
            named_folders(
                &words("meson setup --buildtype=release b && meson test -C b2"),
                cwd
            ),
            [PathBuf::from("/w/b"), PathBuf::from("/w/b2")]
        );
        assert_eq!(
            named_folders(&words("meson compile -Cout"), cwd),
            [PathBuf::from("/w/out")]
        );
        assert!(named_folders(&words("make -C b"), cwd).is_empty());
    }
}
