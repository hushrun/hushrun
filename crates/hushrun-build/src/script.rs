//! What a build script told cargo (B-5, B-6): the variables it set for the compile, what it asked
//! to be rerun for, and the files it left in its output folder.
//!
//! Cargo runs a package's build script itself and saves what the script printed on stdout. The
//! wrapper only sees rustc's side, so it reads that saved file to learn the `rustc-env` variables
//! (part of the first key) and the `rerun-if-*` lines (part of `declared` in the second).
//!
//! Where the saved stdout is, for the script whose `OUT_DIR` a compile was given:
//!
//! ```text
//! cargo 1.94, default layout     target/debug/build/PKG-HASH/out      .../PKG-HASH/output
//! cargo 1.94, -Zbuild-dir-new-layout  .../build/PKG/HASH/build-script/out  .../build-script/output
//! cargo master (new layout)      .../build/PKG/HASH/run/out           .../run/stdout
//! ```
//!
//! In every case the file sits beside `OUT_DIR`. Cargo 1.94 (`custom_build.rs`) writes
//! `script_run_dir.join("output")`; cargo master (`src/compiler/custom_build.rs`,
//! `BuildScriptRunFiles::for_unit`) writes `root.join("stdout")` under the new layout and
//! `root.join("output")` otherwise. `OUT_DIR` is `build_script_run_dir(unit).join("out")` in both
//! (`compilation_files.rs`), and the run folder is `build/PKG-HASH` (old),
//! `build/PKG/HASH/build-script` (1.94 new) or `build/PKG/HASH/run` (master new, `layout.rs`
//! `build_script_execution`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hushrun_core::Digest;

/// What one build script printed that changes a compile's key.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Script {
    /// Each `rustc-env=NAME=VALUE`, in the order printed. Cargo sets them in that order, so for a
    /// name printed twice the last value is the one rustc sees. `RUSTC_BOOTSTRAP` is left out,
    /// since cargo never passes it on.
    pub rustc_env: Vec<(String, String)>,
    /// Each `rerun-if-changed=PATH` as written (relative to the package folder unless absolute),
    /// sorted, each once.
    pub rerun_if_changed: Vec<String>,
    /// Each `rerun-if-env-changed=NAME`, sorted, each once.
    pub rerun_if_env_changed: Vec<String>,
}

impl Script {
    /// Whether the script printed any `rerun-if-changed` or `rerun-if-env-changed` line. Cargo's
    /// rule (`fingerprint/mod.rs`): a script that printed neither depends on the whole package
    /// folder.
    pub fn declared_anything(&self) -> bool {
        !self.rerun_if_changed.is_empty() || !self.rerun_if_env_changed.is_empty()
    }
}

/// One file under a folder, for B-6's `generated` and for a `rerun-if-changed` folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeFile {
    /// The path from the folder, `/`-separated.
    pub rest: String,
    /// The SHA-256 of the file's bytes, or of a symlink's target text.
    pub sha256: Digest,
    /// The file's size, or the length of a symlink's target text.
    pub size: u64,
    /// Whether this entry is a symlink, recorded by its target and not followed. A file and a
    /// symlink whose target text equals the file's bytes have the same digest; this tells them
    /// apart.
    pub symlink: bool,
}

/// Why a folder couldn't be listed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading the folder, an entry or a file failed. A missing folder is this too.
    #[error("{}: {source}", path.display())]
    Io {
        /// What was being read.
        path: PathBuf,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// A name that isn't UTF-8 can't be written as a key part.
    #[error("{}: name is not UTF-8", path.display())]
    NotUtf8 {
        /// The entry.
        path: PathBuf,
    },
    /// Something that is neither a file, a folder nor a symlink (a socket, a pipe): reading it
    /// could block or mean nothing.
    #[error("{}: not a file, a folder or a symlink", path.display())]
    Special {
        /// The entry.
        path: PathBuf,
    },
}

/// The names cargo saves a build script's stdout under, beside `OUT_DIR`: `output` (cargo 1.94,
/// both layouts; the old layout on master) and `stdout` (the new layout on master).
const SAVED_NAMES: [&str; 2] = ["output", "stdout"];

/// Where cargo saved the stdout of the build script whose `OUT_DIR` is `out_dir`: the first of
/// `<OUT_DIR>/../output` and `<OUT_DIR>/../stdout` that is a file. `None` when neither is.
pub fn saved_output(out_dir: &Path) -> Option<PathBuf> {
    let run_dir = out_dir.parent()?;
    SAVED_NAMES
        .iter()
        .map(|name| run_dir.join(name))
        .find(|path| path.is_file())
}

/// The keys cargo's old `cargo:` syntax treats as directives. Any other `cargo:KEY=VALUE` is
/// metadata for dependents (`custom_build.rs`, `RESERVED_PREFIXES`).
const RESERVED_PREFIXES: &[&str] = &[
    "rustc-flags=",
    "rustc-link-lib=",
    "rustc-link-search=",
    "rustc-link-arg-cdylib=",
    "rustc-cdylib-link-arg=",
    "rustc-link-arg-bins=",
    "rustc-link-arg-bin=",
    "rustc-link-arg-tests=",
    "rustc-link-arg-benches=",
    "rustc-link-arg-examples=",
    "rustc-link-arg=",
    "rustc-cfg=",
    "rustc-check-cfg=",
    "rustc-env=",
    "warning=",
    "rerun-if-changed=",
    "rerun-if-env-changed=",
];

/// Reads a saved build script output as text. See [`parse_bytes`].
pub fn parse(text: &str) -> Script {
    parse_bytes(text.as_bytes())
}

/// Reads a saved build script output the way cargo does (`BuildOutput::parse`): lines split on
/// `\n`, a line that isn't UTF-8 skipped, each line trimmed, `cargo::KEY=VALUE` and the old
/// `cargo:KEY=VALUE` for the reserved keys, the value trimmed at its end. Only `rustc-env`,
/// `rerun-if-changed` and `rerun-if-env-changed` are kept; a line cargo would refuse (no `=`)
/// is skipped, since cargo would have failed the build before this compile.
pub fn parse_bytes(bytes: &[u8]) -> Script {
    let mut script = Script::default();
    for line in bytes.split(|b| *b == b'\n') {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        let line = line.trim();
        let data = if let Some(data) = line.strip_prefix("cargo::") {
            data
        } else if let Some(data) = line.strip_prefix("cargo:") {
            if !RESERVED_PREFIXES.iter().any(|p| data.starts_with(p)) {
                continue;
            }
            data
        } else {
            continue;
        };
        let Some((key, value)) = data.split_once('=') else {
            continue;
        };
        let value = value.trim_end();
        match key {
            "rustc-env" => {
                if let Some((name, val)) = value.split_once('=')
                    && name != "RUSTC_BOOTSTRAP"
                {
                    script.rustc_env.push((name.to_owned(), val.to_owned()));
                }
            }
            "rerun-if-changed" => script.rerun_if_changed.push(value.to_owned()),
            "rerun-if-env-changed" => script.rerun_if_env_changed.push(value.to_owned()),
            _ => {}
        }
    }
    script.rerun_if_changed.sort();
    script.rerun_if_changed.dedup();
    script.rerun_if_env_changed.sort();
    script.rerun_if_env_changed.dedup();
    script
}

/// Every file under `dir`, with `/`-separated paths from it, sorted by path. Folders are walked,
/// symlinks are recorded by their target text and not followed, an empty folder adds nothing.
/// `dir` itself is followed if it is a symlink. A missing `dir` is an error, as is a name that
/// isn't UTF-8 or an entry that is not a file, folder or symlink.
pub fn tree(dir: &Path) -> Result<Vec<TreeFile>, Error> {
    let meta = fs::metadata(dir).map_err(io_at(dir))?;
    if !meta.is_dir() {
        return Err(Error::Io {
            path: dir.to_path_buf(),
            source: io::Error::new(io::ErrorKind::NotADirectory, "not a folder"),
        });
    }
    let mut files = Vec::new();
    walk(dir, "", &mut files)?;
    files.sort_by(|a, b| a.rest.cmp(&b.rest));
    Ok(files)
}

fn walk(dir: &Path, prefix: &str, files: &mut Vec<TreeFile>) -> Result<(), Error> {
    for entry in fs::read_dir(dir).map_err(io_at(dir))? {
        let entry = entry.map_err(io_at(dir))?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::NotUtf8 { path: path.clone() })?;
        let rest = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry.file_type().map_err(io_at(&path))?;
        if kind.is_symlink() {
            let target = fs::read_link(&path).map_err(io_at(&path))?;
            let text = target
                .to_str()
                .ok_or_else(|| Error::NotUtf8 { path: path.clone() })?;
            files.push(TreeFile {
                rest,
                sha256: Digest::of(text.as_bytes()),
                size: text.len() as u64,
                symlink: true,
            });
        } else if kind.is_dir() {
            walk(&path, &rest, files)?;
        } else if kind.is_file() {
            let (sha256, size) = match crate::regular::hash_file(&path) {
                Err(err) if crate::regular::is_not_regular(&err) => {
                    return Err(Error::Special { path });
                }
                other => other.map_err(io_at(&path))?,
            };
            files.push(TreeFile {
                rest,
                sha256,
                size,
                symlink: false,
            });
        } else {
            return Err(Error::Special { path });
        }
    }
    Ok(())
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines from real saved outputs under a cargo 1.94 target (libc, libmimalloc-sys, ring,
    /// proc-macro2, serde), with the `cargo:INCLUDE_DIR` path made up.
    const REAL: &str = "\
cargo:rerun-if-changed=build.rs
cargo:rerun-if-env-changed=LIBC_BUILD_VERBOSE
cargo:rerun-if-env-changed=RUST_LIBC_UNSTABLE_FREEBSD_VERSION
cargo:rustc-check-cfg=cfg(freebsd10)
cargo:rustc-check-cfg=cfg(target_os,values(\"switch\",\"aix\",\"ohos\"))
AR_aarch64-apple-darwin = None
cargo:rerun-if-env-changed=CC_aarch64-apple-darwin
CC_aarch64-apple-darwin = None
cargo:INCLUDE_DIR=/home/someone/.cargo/registry/src/x/libmimalloc-sys-0.1.49/c_src/include
cargo::rustc-check-cfg=cfg(slow_mm_packus_epi16)
cargo:rerun-if-changed=src/probe/proc_macro_span.rs
cargo:rustc-cfg=wrap_proc_macro
cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH
cargo:rerun-if-changed=build.rs
";

    #[test]
    // covers: B-6
    fn real_output_keeps_only_rerun_lines() {
        let script = parse(REAL);
        assert!(script.rustc_env.is_empty());
        assert_eq!(
            script.rerun_if_changed,
            ["build.rs", "src/probe/proc_macro_span.rs"]
        );
        assert_eq!(
            script.rerun_if_env_changed,
            [
                "CARGO_CFG_TARGET_ARCH",
                "CC_aarch64-apple-darwin",
                "LIBC_BUILD_VERBOSE",
                "RUST_LIBC_UNSTABLE_FREEBSD_VERSION"
            ]
        );
        assert!(script.declared_anything());
    }

    #[test]
    // covers: B-5
    fn rustc_env_in_both_syntaxes_split_at_the_first_equals() {
        let script = parse(
            "cargo:rustc-env=GIT_HASH=abc123\n\
             cargo::rustc-env=OPTS=a=b c  \n\
             cargo:rustc-env=EMPTY=\n\
             cargo:rustc-env=GIT_HASH=def456\n",
        );
        assert_eq!(
            script.rustc_env,
            [
                ("GIT_HASH".to_owned(), "abc123".to_owned()),
                ("OPTS".to_owned(), "a=b c".to_owned()),
                ("EMPTY".to_owned(), String::new()),
                ("GIT_HASH".to_owned(), "def456".to_owned()),
            ]
        );
        assert!(!script.declared_anything());
    }

    #[test]
    // covers: B-5
    fn rustc_bootstrap_and_malformed_rustc_env_are_dropped() {
        let script = parse(
            "cargo:rustc-env=RUSTC_BOOTSTRAP=1\n\
             cargo:rustc-env=NO_VALUE\n\
             cargo::rustc-env\n",
        );
        assert!(script.rustc_env.is_empty());
    }

    #[test]
    // covers: B-6
    fn metadata_and_other_lines_are_not_directives() {
        // Old syntax: an unreserved key is metadata, even one that looks like a directive.
        // New syntax: metadata is spelled out, and its value is never a directive.
        let script = parse(
            "cargo:rerun-if-changedx=a\n\
             cargo:root=/x\n\
             cargo::metadata=rerun-if-changed=b\n\
             cargo::metadata=rustc-env=A=B\n\
             not cargo:rerun-if-changed=c\n\
             CARGO:rerun-if-changed=d\n\
             cargo:warning=rerun-if-changed=e\n",
        );
        assert_eq!(script, Script::default());
        assert!(!script.declared_anything());
    }

    #[test]
    // covers: B-6
    fn lines_are_trimmed_and_crlf_handled_like_cargo() {
        let script =
            parse("  cargo:rerun-if-changed=src/a b.rs  \r\n\tcargo::rerun-if-env-changed=X\r\n");
        assert_eq!(script.rerun_if_changed, ["src/a b.rs"]);
        assert_eq!(script.rerun_if_env_changed, ["X"]);
    }

    #[test]
    // covers: B-6
    fn a_line_that_is_not_utf8_is_skipped_not_the_rest() {
        let mut bytes = b"cargo:rerun-if-changed=\xff\xfe\n".to_vec();
        bytes.extend_from_slice(b"cargo:rerun-if-env-changed=KEEP\n");
        let script = parse_bytes(&bytes);
        assert!(script.rerun_if_changed.is_empty());
        assert_eq!(script.rerun_if_env_changed, ["KEEP"]);
    }

    #[test]
    fn saved_output_finds_both_layouts() {
        let dir = tempfile::tempdir().unwrap();
        // cargo 1.94: build/PKG-HASH/{out,output}
        let old = dir.path().join("build/foo-1234");
        fs::create_dir_all(old.join("out")).unwrap();
        assert_eq!(saved_output(&old.join("out")), None);
        fs::write(old.join("output"), "cargo:rerun-if-changed=build.rs\n").unwrap();
        assert_eq!(saved_output(&old.join("out")), Some(old.join("output")));
        // cargo master, new layout: build/PKG/HASH/run/{out,stdout}
        let new = dir.path().join("build/foo/1234/run");
        fs::create_dir_all(new.join("out")).unwrap();
        fs::write(new.join("stdout"), "").unwrap();
        assert_eq!(saved_output(&new.join("out")), Some(new.join("stdout")));
        // A folder by that name is not the file.
        let odd = dir.path().join("odd");
        fs::create_dir_all(odd.join("out")).unwrap();
        fs::create_dir_all(odd.join("output")).unwrap();
        assert_eq!(saved_output(&odd.join("out")), None);
        assert_eq!(saved_output(Path::new("/")), None);
    }

    #[test]
    // covers: B-6
    fn tree_lists_every_file_sorted_with_digests() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("b/c")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("z.rs"), "z").unwrap();
        fs::write(root.join("b/c/deep.txt"), "deep").unwrap();
        fs::write(root.join("a b.bin"), [0u8, 1, 2]).unwrap();
        let files = tree(root).unwrap();
        let rests: Vec<&str> = files.iter().map(|f| f.rest.as_str()).collect();
        assert_eq!(rests, ["a b.bin", "b/c/deep.txt", "z.rs"]);
        assert_eq!(files[1].sha256, Digest::of(b"deep"));
        assert_eq!(files[1].size, 4);
        assert!(!files[1].symlink);
        assert_eq!(files[0].size, 3);
    }

    #[test]
    fn tree_of_an_empty_folder_is_empty_and_a_missing_one_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert!(tree(dir.path()).unwrap().is_empty());
        assert!(matches!(
            tree(&dir.path().join("gone")),
            Err(Error::Io { .. })
        ));
        fs::write(dir.path().join("file"), "x").unwrap();
        assert!(matches!(
            tree(&dir.path().join("file")),
            Err(Error::Io { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    // covers: B-6
    fn tree_records_a_symlink_by_its_target_and_does_not_follow_it() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("big.txt"), "content").unwrap();
        symlink(&elsewhere, root.join("dir-link")).unwrap();
        symlink("missing/target", root.join("dangling")).unwrap();
        let files = tree(&root).unwrap();
        let target = elsewhere.to_str().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].rest, "dangling");
        assert_eq!(files[0].sha256, Digest::of(b"missing/target"));
        assert_eq!(files[0].size, 14);
        assert!(files[0].symlink);
        assert_eq!(files[1].rest, "dir-link");
        assert_eq!(files[1].sha256, Digest::of(target.as_bytes()));
        // Changing what the link points at changes nothing here: it is not followed.
        fs::write(elsewhere.join("big.txt"), "changed").unwrap();
        assert_eq!(tree(&root).unwrap(), files);
    }

    #[cfg(unix)]
    #[test]
    fn tree_refuses_a_name_that_is_not_utf8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join(OsStr::from_bytes(b"bad\xff"));
        // Some file systems (APFS) refuse such names; then there is nothing to test.
        if fs::write(&bad, "x").is_err() {
            return;
        }
        assert!(matches!(tree(dir.path()), Err(Error::NotUtf8 { .. })));
    }
}
