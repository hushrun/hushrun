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

/// What a run says about a build folder COMMAND named, after COMMAND (B-50).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// Its `CMakeCache.txt` names neither launcher and its generator is Ninja or Makefiles:
    /// CMake configured it before the launcher.
    ConfiguredBefore {
        /// The build folder.
        folder: PathBuf,
        /// CMake's version from the cache, when older than 3.17, which reads no launcher from
        /// the environment at all.
        old_cmake: Option<(u32, u32)>,
    },
    /// Its generated rules start a launcher of their own (`RULE_LAUNCH_COMPILE`) in front of
    /// hushrun's indirection, which that launcher then takes for the compiler.
    LauncherBefore {
        /// The build folder.
        folder: PathBuf,
    },
}

impl Note {
    /// The line the run says.
    pub fn line(&self) -> String {
        match self {
            Note::ConfiguredBefore {
                folder,
                old_cmake: Some((major, minor)),
            } => format!(
                "build: C and C++ objects aren't reused in {}: CMake {major}.{minor} reads no launcher from the environment (3.17 and later do)",
                folder.display()
            ),
            Note::ConfiguredBefore { folder, .. } => format!(
                "build: C and C++ objects aren't reused in {}: it was configured before hushrun's launcher; configure it again inside hushrun run",
                folder.display()
            ),
            Note::LauncherBefore { folder } => format!(
                "build: {} starts a launcher of its own before hushrun's (RULE_LAUNCH_COMPILE), which sees /bin/sh as the compiler: a ccache there compiles every time, in and out of a run; name it in CMAKE_C_COMPILER_LAUNCHER and CMAKE_CXX_COMPILER_LAUNCHER instead",
                folder.display()
            ),
        }
    }
}

/// CMake's version from a cache's text.
fn cache_version(cache: &str) -> Option<(u32, u32)> {
    let field = |name: &str| {
        cache
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().parse::<u32>().ok())
    };
    Some((
        field("CMAKE_CACHE_MAJOR_VERSION:INTERNAL=")?,
        field("CMAKE_CACHE_MINOR_VERSION:INTERNAL=")?,
    ))
}

/// What the run says about the build folders `words` name (B-50): each configured before the
/// launcher, and each whose rules put a launcher of their own in front of it.
pub fn notes(words: &[String], cwd: &Path) -> Vec<Note> {
    let mut out = Vec::new();
    for folder in named_folders(words, cwd) {
        let Ok(cache) = std::fs::read_to_string(folder.join("CMakeCache.txt")) else {
            continue;
        };
        let generator = cache
            .lines()
            .find_map(|l| l.strip_prefix("CMAKE_GENERATOR:INTERNAL="))
            .unwrap_or_default();
        let known = generator.starts_with("Ninja") || generator.ends_with("Makefiles");
        if !known {
            continue;
        }
        if !names_a_launcher(&cache) {
            let old_cmake = cache_version(&cache).filter(|v| *v < (3, 17));
            out.push(Note::ConfiguredBefore { folder, old_cmake });
        } else if cache.contains("HUSHRUN_CC") && launcher_before(&folder) {
            out.push(Note::LauncherBefore { folder });
        }
    }
    out
}

/// The build folders `words` name that CMake configured before the launcher (B-50).
pub fn configured_before(words: &[String], cwd: &Path) -> Vec<PathBuf> {
    notes(words, cwd)
        .into_iter()
        .filter_map(|n| match n {
            Note::ConfiguredBefore { folder, .. } => Some(folder),
            Note::LauncherBefore { .. } => None,
        })
        .collect()
}

/// Whether a rule line of CMake's runs something before the indirection: the text before its
/// `/bin/sh` (CMake 3, which writes the launcher into the rule) or its `${LAUNCHER}` (CMake 4,
/// which writes it into a variable of each build statement), less Ninja's `command =` and
/// Make's `cd … &&`, isn't empty.
fn wraps_the_indirection(line: &str) -> bool {
    let at = if line.contains("HUSHRUN_CC") {
        line.find("/bin/sh")
    } else {
        line.find("${LAUNCHER}").or_else(|| line.find("$LAUNCHER"))
    };
    let Some(at) = at else {
        return false;
    };
    let before = line[..at].trim();
    let before = before.strip_prefix("command =").unwrap_or(before);
    let before = before.rsplit("&&").next().unwrap_or(before);
    !before.trim().is_empty()
}

/// Whether a build folder's generated rules (Ninja's `CMakeFiles/rules.ninja`, Make's
/// `build.make` files) put a launcher before the indirection.
fn launcher_before(folder: &Path) -> bool {
    let mut files = vec![folder.join("CMakeFiles").join("rules.ninja")];
    let mut stack = vec![(folder.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < 6 {
                    stack.push((path, depth + 1));
                }
            } else if path.file_name().is_some_and(|n| n == "build.make") {
                files.push(path);
            }
        }
        if files.len() > 64 {
            break;
        }
    }
    files.iter().any(|file| {
        std::fs::read_to_string(file).is_ok_and(|text| text.lines().any(wraps_the_indirection))
    })
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

    // covers: B-50
    #[test]
    fn a_cmake_older_than_3_17_is_named_not_told_to_configure_again() {
        let dir = tempfile::tempdir().unwrap();
        let build = dir.path().join("build");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(
            build.join("CMakeCache.txt"),
            "CMAKE_GENERATOR:INTERNAL=Ninja\nCMAKE_CACHE_MAJOR_VERSION:INTERNAL=3\nCMAKE_CACHE_MINOR_VERSION:INTERNAL=16\n",
        )
        .unwrap();
        let found = notes(&words("cmake --build build"), dir.path());
        assert_eq!(found.len(), 1);
        let line = found[0].line();
        assert!(line.contains("CMake 3.16 reads no launcher"), "{line}");
        assert!(!line.contains("configure it again"), "{line}");
        std::fs::write(
            build.join("CMakeCache.txt"),
            "CMAKE_GENERATOR:INTERNAL=Ninja\nCMAKE_CACHE_MAJOR_VERSION:INTERNAL=3\nCMAKE_CACHE_MINOR_VERSION:INTERNAL=25\n",
        )
        .unwrap();
        let line = notes(&words("cmake --build build"), dir.path())[0].line();
        assert!(line.contains("configure it again"), "{line}");
    }

    // covers: B-50
    #[test]
    fn a_launcher_of_the_project_s_own_before_the_indirection_is_said() {
        // The lines CMake 3.25 writes with RULE_LAUNCH_COMPILE set, for Ninja and for Make.
        let ninja = r#"  command = /usr/bin/env /bin/sh -c "[ -x \"\$$HUSHRUN_CC\" ] || HUSHRUN_CC= && exec \$${HUSHRUN_CC:+\"\$$HUSHRUN_CC\"} \"\$$0\" \"\$$@\"" /usr/bin/cc $DEFINES $INCLUDES $FLAGS -MD -MT $out -MF $DEP_FILE -o $out -c $in"#;
        let make = "\t/usr/bin/env /bin/sh -c \"[ -x \\\"\\$$HUSHRUN_CC\\\" ] || HUSHRUN_CC= && exec \\$${HUSHRUN_CC:+...}\" /usr/bin/cc -o CMakeFiles/l.dir/a.c.o -c /p/a.c";
        // CMake 4 writes the launcher into a variable of each build statement.
        let ninja4 = "  command = /usr/bin/env ${LAUNCHER}${CODE_CHECK}/usr/bin/cc $DEFINES -c $in";
        assert!(wraps_the_indirection(ninja));
        assert!(wraps_the_indirection(make));
        assert!(wraps_the_indirection(ninja4));
        assert!(!wraps_the_indirection(
            "  command = ${LAUNCHER}${CODE_CHECK}/usr/bin/cc $DEFINES -c $in"
        ));
        for alone in [
            ninja.replace("/usr/bin/env ", ""),
            make.replace("/usr/bin/env ", ""),
            make.replace("\t/usr/bin/env ", "\tcd /p/build/sub && "),
        ] {
            assert!(!wraps_the_indirection(&alone), "{alone}");
        }
        let dir = tempfile::tempdir().unwrap();
        let build = dir.path().join("build");
        std::fs::create_dir_all(build.join("CMakeFiles")).unwrap();
        std::fs::write(
            build.join("CMakeCache.txt"),
            "CMAKE_GENERATOR:INTERNAL=Ninja\nCMAKE_C_COMPILER_LAUNCHER:STRING=/bin/sh;-c;[ -x \"$HUSHRUN_CC\" ]\n",
        )
        .unwrap();
        std::fs::write(build.join("CMakeFiles/rules.ninja"), ninja).unwrap();
        assert_eq!(
            notes(&words("cmake --build build"), dir.path()),
            [Note::LauncherBefore {
                folder: build.clone()
            }]
        );
    }
}
