//! hushrun's identity of a Go toolchain (B-39, F5): what Go's own action id doesn't hold. For a
//! release Go names each tool by its version line alone (`toolID`), so a patched toolchain that
//! reports the same version shares every id with the official one, with other bytes (proved for
//! `compile` and for `cover`). The identity is a hash of the toolchain's files by content: the
//! `go` binary, every regular file in its tool folder (`GOTOOLDIR`: `compile`, `asm`, `cgo`,
//! `cover`, `link`, `vet` and the rest), every file under `pkg/include`, and every `.h` under
//! `src` (the standard library's assembly includes `runtime/cgo`'s headers, F1).
//!
//! Hashing the tool folder is tens of megabytes, so each file's digest is remembered for the run
//! ([`crate::memo`], by path, size, both dates, inode and device), and so is the list of files
//! the walk found, under the session's folder: the second `go` command of a run reads the memo
//! and a few `stat`s.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

/// The schema the identity is taken under.
const SCHEMA: &str = "hushrun.go-toolchain/v1";
/// The memo's folder in the session.
const MEMO: &str = "go-toolchains";

/// What the identity is computed from, as the `go` command passes it to the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    /// `GOROOT`.
    pub goroot: PathBuf,
    /// `GOTOOLDIR`, or `<GOROOT>/pkg/tool/<GOHOSTOS>_<GOHOSTARCH>`.
    pub tooldir: PathBuf,
    /// `GOVERSION`.
    pub version: String,
}

impl Toolchain {
    /// The toolchain the environment names, or `None` without `GOROOT` and `GOVERSION`.
    pub fn from_env(env: &dyn Fn(&str) -> Option<OsString>) -> Option<Toolchain> {
        let var = |name: &str| env(name).filter(|v| !v.is_empty());
        let goroot = PathBuf::from(var("GOROOT")?);
        let version = var("GOVERSION")?.to_string_lossy().into_owned();
        let tooldir = match var("GOTOOLDIR") {
            Some(dir) => PathBuf::from(dir),
            None => {
                let os = var("GOHOSTOS")?.to_string_lossy().into_owned();
                let arch = var("GOHOSTARCH")?.to_string_lossy().into_owned();
                goroot.join("pkg").join("tool").join(format!("{os}_{arch}"))
            }
        };
        Some(Toolchain {
            goroot,
            tooldir,
            version,
        })
    }

    /// The files the identity hashes, each with the name it is hashed under: relative to
    /// `GOROOT`, or `GOTOOLDIR/<name>` for a tool folder outside it. Sorted.
    fn files(&self) -> std::io::Result<Vec<(String, PathBuf)>> {
        let mut files = Vec::new();
        files.push((
            "bin/go".to_string(),
            super::command::go_binary(&self.goroot),
        ));
        let tool_prefix = match self.tooldir.strip_prefix(&self.goroot) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => "GOTOOLDIR".to_string(),
        };
        for entry in fs::read_dir(&self.tooldir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                files.push((
                    format!("{tool_prefix}/{}", entry.file_name().to_string_lossy()),
                    entry.path(),
                ));
            }
        }
        walk(
            &self.goroot,
            &self.goroot.join("pkg").join("include"),
            &mut |_| true,
            &mut files,
        )?;
        walk(
            &self.goroot,
            &self.goroot.join("src"),
            &mut |name| name.ends_with(".h"),
            &mut files,
        )?;
        files.sort();
        files.dedup_by(|a, b| a.0 == b.0);
        Ok(files)
    }

    /// The identity: the SHA-256 of the JCS form of the schema, the version and each file's
    /// name and SHA-256. `session`: the run's folder, where file digests and the walk are
    /// remembered.
    pub fn identity(&self, session: Option<&Path>) -> Result<Digest, String> {
        let memo = session.map(|s| {
            s.join(MEMO).join(format!(
                "{}.json",
                Digest::of(
                    format!("{}\n{}", self.goroot.display(), self.tooldir.display()).as_bytes()
                )
                .hex()
            ))
        });
        let files = match memo.as_deref().and_then(read_walk) {
            Some(files) => files,
            None => {
                let files = self
                    .files()
                    .map_err(|err| format!("{}: {err}", self.goroot.display()))?;
                if let Some(memo) = &memo {
                    let _ = write_walk(memo, &files);
                }
                files
            }
        };
        let mut listed = Vec::with_capacity(files.len());
        for (name, path) in &files {
            let (sha, _) = crate::memo::digest(session, path)
                .map_err(|err| format!("{}: {err}", path.display()))?;
            listed.push(serde_json::json!([name, sha.hex()]));
        }
        let value = serde_json::json!({
            "schema": SCHEMA,
            "version": self.version,
            "files": listed,
        });
        let canonical = hushrun_core::jcs::canonicalize(&value).map_err(|err| err.to_string())?;
        Ok(Digest::of(canonical.as_bytes()))
    }
}

/// Every regular file under `dir` that `keep` keeps by name, named relative to `root`. Links are
/// not followed. A folder that isn't there adds nothing.
fn walk(
    root: &Path,
    dir: &Path,
    keep: &mut dyn FnMut(&str) -> bool,
    out: &mut Vec<(String, PathBuf)>,
) -> std::io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            walk(root, &path, keep, out)?;
        } else if kind.is_file() && keep(&entry.file_name().to_string_lossy()) {
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            out.push((name, path));
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Walk {
    schema: String,
    files: Vec<(String, PathBuf)>,
}

fn read_walk(memo: &Path) -> Option<Vec<(String, PathBuf)>> {
    let walk: Walk = serde_json::from_slice(&fs::read(memo).ok()?).ok()?;
    (walk.schema == SCHEMA).then_some(walk.files)
}

fn write_walk(memo: &Path, files: &[(String, PathBuf)]) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&Walk {
        schema: SCHEMA.into(),
        files: files.to_vec(),
    })
    .map_err(std::io::Error::other)?;
    super::gocache::write_whole(memo, &bytes, std::time::SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toolchain(dir: &Path) -> Toolchain {
        let goroot = dir.join("go");
        let tooldir = goroot.join("pkg/tool/darwin_arm64");
        fs::create_dir_all(&tooldir).unwrap();
        fs::create_dir_all(goroot.join("bin")).unwrap();
        fs::create_dir_all(goroot.join("pkg/include")).unwrap();
        fs::create_dir_all(goroot.join("src/runtime/cgo")).unwrap();
        fs::write(goroot.join("bin/go"), b"go").unwrap();
        for tool in ["compile", "asm", "cgo", "cover", "link"] {
            fs::write(tooldir.join(tool), tool).unwrap();
        }
        fs::write(goroot.join("pkg/include/textflag.h"), b"#define NOSPLIT 4").unwrap();
        fs::write(goroot.join("src/runtime/cgo/abi_arm64.h"), b"// abi").unwrap();
        fs::write(goroot.join("src/runtime/proc.go"), b"package runtime").unwrap();
        Toolchain {
            goroot,
            tooldir,
            version: "go1.27.4".into(),
        }
    }

    // covers: B-39
    #[test]
    fn a_patched_tool_with_the_same_version_line_is_another_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        let tc = toolchain(dir.path());
        let official = tc.identity(None).unwrap();
        assert_eq!(tc.identity(None).unwrap(), official);
        // F5: `cover`, which the design's identity left out, patched.
        fs::write(tc.tooldir.join("cover"), b"cover, patched").unwrap();
        let patched = tc.identity(None).unwrap();
        assert_ne!(patched, official);
        // A header under src, which the standard library's assembly includes (F1).
        fs::write(tc.goroot.join("src/runtime/cgo/abi_arm64.h"), b"// other").unwrap();
        assert_ne!(tc.identity(None).unwrap(), patched);
        // A Go source of the standard library is not in it: Go's id holds it by content.
        let before = tc.identity(None).unwrap();
        fs::write(
            tc.goroot.join("src/runtime/proc.go"),
            b"package runtime // x",
        )
        .unwrap();
        assert_eq!(tc.identity(None).unwrap(), before);
    }

    // covers: B-39
    #[test]
    fn the_runs_memo_sees_a_tool_rewritten_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let tc = toolchain(dir.path());
        let session = dir.path().join("session");
        fs::create_dir_all(&session).unwrap();
        let first = tc.identity(Some(&session)).unwrap();
        assert_eq!(tc.identity(Some(&session)).unwrap(), first);
        // Same size, new bytes: the memo's stat no longer holds.
        fs::write(tc.tooldir.join("compile"), b"COMPILE").unwrap();
        assert_ne!(tc.identity(Some(&session)).unwrap(), first);
        assert_eq!(
            tc.identity(None).unwrap(),
            tc.identity(Some(&session)).unwrap()
        );
    }
}
