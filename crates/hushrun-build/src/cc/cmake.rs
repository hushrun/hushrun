//! The build folders a COMMAND names, and whether CMake configured them before the launcher
//! (B-50): such a folder's cache holds no launcher, and CMake reads one from the environment only
//! when it configures it again.

use std::path::{Path, PathBuf};

/// The folders `words` name as build folders: `cmake --build <dir>`, `cmake -B <dir>` (or
/// `-B<dir>`), `ninja -C <dir>`, `make -C <dir>`, each from `cwd`.
pub fn named_folders(words: &[String], cwd: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let program = |w: &str| {
        Path::new(w)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut current = String::new();
    let mut i = 0;
    while i < words.len() {
        let word = words[i].as_str();
        let name = program(word);
        if matches!(name.as_str(), "cmake" | "ninja" | "make" | "gmake") {
            current = name;
            i += 1;
            continue;
        }
        let next = words.get(i + 1).map(String::as_str);
        let dir = match (current.as_str(), word) {
            ("cmake", "--build" | "-B") => next.inspect(|_| i += 1),
            ("cmake", w) if w.starts_with("-B") && w.len() > 2 => Some(&w[2..]),
            ("cmake", w) if w.starts_with("--build=") => Some(&w[8..]),
            ("ninja" | "make" | "gmake", "-C") => next.inspect(|_| i += 1),
            ("ninja" | "make" | "gmake", w) if w.starts_with("-C") && w.len() > 2 => Some(&w[2..]),
            _ => None,
        };
        if let Some(dir) = dir {
            let path = cwd.join(dir);
            if !found.contains(&path) {
                found.push(path);
            }
        }
        i += 1;
    }
    found
}

/// Whether a CMake cache's text names a launcher for C or C++.
fn names_a_launcher(cache: &str) -> bool {
    cache.lines().any(|line| {
        ["CMAKE_C_COMPILER_LAUNCHER:", "CMAKE_CXX_COMPILER_LAUNCHER:"]
            .iter()
            .any(|p| line.starts_with(p))
            && line
                .split_once('=')
                .is_some_and(|(_, v)| !v.trim().is_empty())
    })
}

/// The build folders `words` name whose `CMakeCache.txt` names neither launcher and whose
/// generator is Ninja or Makefiles: configured before the launcher (B-50).
pub fn configured_before(words: &[String], cwd: &Path) -> Vec<PathBuf> {
    named_folders(words, cwd)
        .into_iter()
        .filter(|dir| {
            let Ok(cache) = std::fs::read_to_string(dir.join("CMakeCache.txt")) else {
                return false;
            };
            let generator = cache
                .lines()
                .find_map(|l| l.strip_prefix("CMAKE_GENERATOR:INTERNAL="))
                .unwrap_or_default();
            let known = generator.starts_with("Ninja") || generator.ends_with("Makefiles");
            known && !names_a_launcher(&cache)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: B-50
    #[test]
    fn the_build_folders_a_command_names_are_found() {
        let cwd = Path::new("/r");
        assert_eq!(
            named_folders(&words("cmake --build build -j8"), cwd),
            [PathBuf::from("/r/build")]
        );
        assert_eq!(
            named_folders(&words("cmake -S . -Bout && ninja -C out2"), cwd),
            [PathBuf::from("/r/out"), PathBuf::from("/r/out2")]
        );
        assert_eq!(
            named_folders(&words("make -C /abs test"), cwd),
            [PathBuf::from("/abs")]
        );
        assert!(named_folders(&words("cargo build -C x"), cwd).is_empty());
    }

    // covers: B-50
    #[test]
    fn a_folder_whose_cache_names_no_launcher_was_configured_before() {
        let dir = tempfile::tempdir().unwrap();
        let build = dir.path().join("build");
        std::fs::create_dir_all(&build).unwrap();
        let cmd = words("cmake --build build");
        assert!(configured_before(&cmd, dir.path()).is_empty(), "no cache");
        std::fs::write(
            build.join("CMakeCache.txt"),
            "CMAKE_GENERATOR:INTERNAL=Ninja\n",
        )
        .unwrap();
        assert_eq!(
            configured_before(&cmd, dir.path()),
            std::slice::from_ref(&build)
        );
        std::fs::write(
            build.join("CMakeCache.txt"),
            "CMAKE_GENERATOR:INTERNAL=Unix Makefiles\nCMAKE_CXX_COMPILER_LAUNCHER:STRING=/bin/sh;-c;exec\n",
        )
        .unwrap();
        assert!(configured_before(&cmd, dir.path()).is_empty());
        std::fs::write(
            build.join("CMakeCache.txt"),
            "CMAKE_GENERATOR:INTERNAL=Xcode\n",
        )
        .unwrap();
        assert!(
            configured_before(&cmd, dir.path()).is_empty(),
            "another generator"
        );
    }
}
