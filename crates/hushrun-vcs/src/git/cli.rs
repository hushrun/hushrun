//! The `git` CLI: the second object reader (D-9), worktree snapshots (D-10) and `ls-remote`.
//!
//! git is spawned a bounded number of times per command, never once per file.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use hushrun_digest::Sha256;
use hushrun_digest::hash::{CHUNK, Hasher};

use crate::Error;

/// At most this many `git cat-file --batch` processes run at once.
const MAX_CAT_FILE: usize = 4;
/// Below this many objects per process, fewer processes are faster.
const MIN_OBJECTS_PER_PROCESS: usize = 256;

/// Runs `git -C <root> <args>` and returns its stdout.
pub(crate) fn run<I, S>(root: &Path, args: I, envs: &[(&str, &OsStr)]) -> Result<Vec<u8>, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args(&args);
    command.env("GIT_OPTIONAL_LOCKS", "0");
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitSpawn)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::Git {
            args: display_args(&args),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// `run`, with the output as trimmed UTF-8.
pub(crate) fn run_text<I, S>(root: &Path, args: I, envs: &[(&str, &OsStr)]) -> Result<String, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = run(root, args, envs)?;
    String::from_utf8(out)
        .map(|s| s.trim().to_string())
        .map_err(|_| Error::GitOutput {
            args: "…".into(),
            detail: "output is not UTF-8".into(),
        })
}

fn display_args(args: &[std::ffi::OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `sha1` or `sha256`.
pub(crate) fn object_format(root: &Path) -> Result<String, Error> {
    let format = run_text(root, ["rev-parse", "--show-object-format"], &[])?;
    match format.as_str() {
        "sha1" | "sha256" => Ok(format),
        other => Err(Error::GitOutput {
            args: "rev-parse --show-object-format".into(),
            detail: format!("unknown object format {other:?}"),
        }),
    }
}

/// The commit and tree of `rev`.
pub(crate) fn resolve(root: &Path, rev: &str) -> Result<(String, String), Error> {
    let unknown = |detail: String| Error::UnknownRevision {
        rev: rev.to_string(),
        detail,
    };
    // `git rev-parse` echoes `--end-of-options`, so refuse option-like revisions instead.
    if rev.is_empty() || rev.starts_with('-') {
        return Err(unknown(
            "a revision can't be empty or start with `-`".into(),
        ));
    }
    let commit = run_text(
        root,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
        &[],
    )
    .map_err(|err| unknown(err.to_string()))?;
    if commit.is_empty() || commit.contains('\n') {
        return Err(unknown("not a commit".into()));
    }
    let tree = run_text(
        root,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{commit}^{{tree}}"),
        ],
        &[],
    )?;
    Ok((commit, tree))
}

/// The full ref name `rev` refers to, such as `refs/heads/main`, if it is a ref.
pub(crate) fn symbolic_full_name(root: &Path, rev: &str) -> Option<String> {
    run_text(root, ["rev-parse", "--symbolic-full-name", rev], &[])
        .ok()
        .filter(|name| name.starts_with("refs/"))
}

/// One record of `git ls-tree -r -z`.
pub(crate) struct TreeRecord {
    pub mode: u32,
    pub oid: Vec<u8>,
    pub path: Vec<u8>,
}

/// Every non-tree entry of `treeish`, recursively.
pub(crate) fn ls_tree(root: &Path, treeish: &str) -> Result<Vec<TreeRecord>, Error> {
    let out = run(
        root,
        ["ls-tree", "-r", "-z", "--full-tree", treeish],
        &[no_lazy_fetch()],
    )
    .map_err(|err| match &err {
        Error::Git { stderr, .. } if stderr.contains("missing") || stderr.contains("bad tree") => {
            Error::MissingObjects {
                object: treeish.to_string(),
            }
        }
        _ => err,
    })?;
    let bad = |detail: &str| Error::GitOutput {
        args: "ls-tree -r -z".into(),
        detail: detail.to_string(),
    };
    let mut records = Vec::new();
    for record in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        // "<mode> SP <type> SP <object> TAB <path>"
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(|| bad("no tab"))?;
        let (meta, path) = (&record[..tab], &record[tab + 1..]);
        let mut fields = meta.split(|b| *b == b' ');
        let mode = fields.next().ok_or_else(|| bad("no mode"))?;
        let _kind = fields.next().ok_or_else(|| bad("no type"))?;
        let oid = fields.next().ok_or_else(|| bad("no object id"))?;
        let mode = std::str::from_utf8(mode)
            .ok()
            .and_then(|m| u32::from_str_radix(m, 8).ok())
            .ok_or_else(|| bad("bad mode"))?;
        let oid = std::str::from_utf8(oid)
            .ok()
            .and_then(|o| hex::decode(o).ok())
            .ok_or_else(|| bad("bad object id"))?;
        records.push(TreeRecord {
            mode,
            oid,
            path: path.to_vec(),
        });
    }
    Ok(records)
}

/// SHA-256 and size of each blob, read with `git cat-file --batch`, in order.
pub(crate) fn hash_blobs(
    root: &Path,
    oids: &[&[u8]],
    jobs: usize,
) -> Result<Vec<(Sha256, u64)>, Error> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let processes = jobs
        .clamp(1, MAX_CAT_FILE)
        .min(oids.len().div_ceil(MIN_OBJECTS_PER_PROCESS))
        .max(1);
    let per_process = oids.len().div_ceil(processes);
    std::thread::scope(|scope| {
        let handles: Vec<_> = oids
            .chunks(per_process)
            .map(|chunk| scope.spawn(move || cat_file(root, chunk)))
            .collect();
        let mut out = Vec::with_capacity(oids.len());
        for handle in handles {
            out.extend(handle.join().expect("cat-file reader panicked")?);
        }
        Ok(out)
    })
}

/// The bytes of one blob.
pub(crate) fn read_blob(root: &Path, oid: &[u8]) -> Result<Vec<u8>, Error> {
    let oid = hex::encode(oid);
    run(root, ["cat-file", "blob", &oid], &[no_lazy_fetch()])
}

/// A partial clone must fail with E_MISSING_OBJECTS, not fetch what's left out one object at a time.
fn no_lazy_fetch() -> (&'static str, &'static OsStr) {
    ("GIT_NO_LAZY_FETCH", OsStr::new("1"))
}

fn cat_file(root: &Path, oids: &[&[u8]]) -> Result<Vec<(Sha256, u64)>, Error> {
    batch(root, oids, |out| read_batch(out, oids))
}

/// \[M8\] Calls `sink` with the bytes of each blob in `oids`, in order, from one
/// `git cat-file --batch`: an isolated run's export (§11.6).
pub(crate) fn read_blobs(
    root: &Path,
    oids: &[&[u8]],
    sink: &mut dyn FnMut(usize, Vec<u8>) -> Result<(), Error>,
) -> Result<(), Error> {
    if oids.is_empty() {
        return Ok(());
    }
    batch(root, oids, |mut out| {
        let bad = |detail: String| Error::GitOutput {
            args: "cat-file --batch".into(),
            detail,
        };
        let mut header = Vec::new();
        for (i, oid) in oids.iter().enumerate() {
            header.clear();
            if out
                .read_until(b'\n', &mut header)
                .map_err(|e| bad(e.to_string()))?
                == 0
            {
                return Err(bad("output ended early".into()));
            }
            let line = String::from_utf8_lossy(&header);
            let fields: Vec<&str> = line.trim_end().split(' ').collect();
            if fields.get(1) == Some(&"missing") {
                return Err(Error::MissingObjects {
                    object: hex::encode(oid),
                });
            }
            let [_, "blob", size] = fields[..] else {
                return Err(bad(format!("unexpected header {:?}", line.trim_end())));
            };
            let size: usize = size
                .parse()
                .map_err(|_| bad(format!("bad size in {:?}", line.trim_end())))?;
            let mut bytes = vec![0u8; size];
            out.read_exact(&mut bytes).map_err(|e| bad(e.to_string()))?;
            let mut newline = [0u8; 1];
            out.read_exact(&mut newline)
                .map_err(|e| bad(e.to_string()))?;
            sink(i, bytes)?;
        }
        Ok(())
    })
}

/// Runs one `git cat-file --batch` that is asked for `oids`, and reads its output with `read`.
fn batch<T>(
    root: &Path,
    oids: &[&[u8]],
    read: impl FnOnce(BufReader<std::process::ChildStdout>) -> Result<T, Error>,
) -> Result<T, Error> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .envs([no_lazy_fetch()])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::GitSpawn)?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    let result = std::thread::scope(|scope| {
        let writer = scope.spawn(move || -> std::io::Result<()> {
            let mut buf = Vec::with_capacity(oids.len() * 65);
            for oid in oids {
                buf.extend_from_slice(hex::encode(oid).as_bytes());
                buf.push(b'\n');
            }
            stdin.write_all(&buf)
            // stdin drops here, which ends git's input.
        });
        let errors = scope.spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        let read = read(BufReader::with_capacity(CHUNK, stdout));
        let write = writer.join().expect("writer panicked");
        let stderr = errors.join().expect("stderr reader panicked");
        (read, write, stderr)
    });
    let (read, write, stderr) = result;
    let status = child.wait().map_err(Error::GitSpawn)?;
    let value = read?;
    if let Err(err) = write {
        return Err(Error::Git {
            args: "cat-file --batch".into(),
            status: status.to_string(),
            stderr: format!("{err}; {}", stderr.trim()),
        });
    }
    if !status.success() {
        return Err(Error::Git {
            args: "cat-file --batch".into(),
            status: status.to_string(),
            stderr: stderr.trim().to_string(),
        });
    }
    Ok(value)
}

fn read_batch(mut out: impl BufRead, oids: &[&[u8]]) -> Result<Vec<(Sha256, u64)>, Error> {
    let bad = |detail: String| Error::GitOutput {
        args: "cat-file --batch".into(),
        detail,
    };
    let mut hashes = Vec::with_capacity(oids.len());
    let mut header = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    for oid in oids {
        header.clear();
        let n = out
            .read_until(b'\n', &mut header)
            .map_err(|e| bad(e.to_string()))?;
        if n == 0 {
            return Err(bad("output ended early".into()));
        }
        let line = String::from_utf8_lossy(&header);
        let line = line.trim_end();
        let fields: Vec<&str> = line.split(' ').collect();
        if fields.get(1) == Some(&"missing") {
            return Err(Error::MissingObjects {
                object: hex::encode(oid),
            });
        }
        let [_, kind, size] = fields[..] else {
            return Err(bad(format!("unexpected header {line:?}")));
        };
        if kind != "blob" {
            return Err(bad(format!("{} is a {kind}, not a blob", hex::encode(oid))));
        }
        let size: u64 = size
            .parse()
            .map_err(|_| bad(format!("bad size in {line:?}")))?;
        let mut hasher = Hasher::new();
        let mut left = size;
        while left > 0 {
            let want = left.min(buf.len() as u64) as usize;
            out.read_exact(&mut buf[..want])
                .map_err(|e| bad(e.to_string()))?;
            hasher.update(&buf[..want]);
            left -= want as u64;
        }
        let mut newline = [0u8; 1];
        out.read_exact(&mut newline)
            .map_err(|e| bad(e.to_string()))?;
        hashes.push((hasher.finish(), size));
    }
    Ok(hashes)
}

/// The bytes of `path` at `rev`.
pub(crate) fn read_file_at(root: &Path, rev: &str, path: &str) -> Result<Vec<u8>, Error> {
    run(root, ["cat-file", "blob", &format!("{rev}:{path}")], &[])
}

/// The commit `refs/heads/<branch>` points to on `remote`.
pub(crate) fn remote_head(
    root: &Path,
    remote: &str,
    branch: &str,
) -> Result<Option<String>, Error> {
    let name = format!("refs/heads/{branch}");
    if remote.starts_with('-') {
        return Err(Error::GitOutput {
            args: "ls-remote".into(),
            detail: format!("remote {remote:?} can't start with `-`"),
        });
    }
    let out = run_text(root, ["ls-remote", remote, &name], &[])?;
    Ok(out.lines().find_map(|line| {
        let (oid, reference) = line.split_once('\t')?;
        (reference == name).then(|| oid.to_string())
    }))
}

/// The tree of commit `rev`.
pub(crate) fn tree(root: &Path, rev: &str) -> Result<String, Error> {
    if rev.starts_with('-') || rev.is_empty() {
        return Err(Error::UnknownRevision {
            rev: rev.into(),
            detail: "not a revision".into(),
        });
    }
    run_text(
        root,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{tree}}"),
        ],
        &[],
    )
}

/// `git fetch --depth=1 <remote> <sha>`: one commit, without prompting for credentials.
pub(crate) fn fetch_commit(root: &Path, remote: &str, sha: &str) -> Result<(), Error> {
    let hex = (sha.len() == 40 || sha.len() == 64) && sha.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex || remote.starts_with('-') {
        return Err(Error::GitOutput {
            args: "fetch".into(),
            detail: format!("fetch takes a remote and a full commit id, not {remote:?} {sha:?}"),
        });
    }
    let env: [(&str, &OsStr); 1] = [("GIT_TERMINAL_PROMPT", OsStr::new("0"))];
    run(
        root,
        ["fetch", "--quiet", "--no-tags", "--depth=1", remote, sha],
        &env,
    )
    .map(|_| ())
}

/// The tree `git add -A && git commit` would record now, built in a temporary index so the
/// real index is never modified (D-10).
pub(crate) fn worktree_tree(root: &Path) -> Result<String, Error> {
    let index = run_text(
        root,
        ["rev-parse", "--path-format=absolute", "--git-path", "index"],
        &[],
    )?;
    let dir = tempfile::tempdir().map_err(|source| Error::Io {
        path: std::env::temp_dir(),
        source,
    })?;
    let temp_index = dir.path().join("index");
    let env: [(&str, &OsStr); 1] = [("GIT_INDEX_FILE", temp_index.as_os_str())];

    let split = run_text(
        root,
        ["config", "--type=bool", "--get", "core.splitIndex"],
        &[],
    )
    .map(|v| v == "true")
    .unwrap_or(false);
    let has_head = run_text(
        root,
        ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        &[],
    )
    .map(|v| !v.is_empty())
    .unwrap_or(false);
    let index = Path::new(&index);
    if split {
        // A split index refers to shared files: start from HEAD instead (every file is
        // re-hashed, which is slower but correct).
        let tree: &[&str] = if has_head {
            &["read-tree", "HEAD"]
        } else {
            &["read-tree", "--empty"]
        };
        run(root, tree, &env)?;
    } else if index.exists() {
        copy_index(index, &temp_index)?;
    }
    // Never write a new split index next to the real one.
    run(root, ["-c", "core.splitIndex=false", "add", "-A"], &env)?;
    let tree = run_text(root, ["-c", "core.splitIndex=false", "write-tree"], &env)?;
    if tree.is_empty() {
        return Err(Error::GitOutput {
            args: "write-tree".into(),
            detail: "no tree id".into(),
        });
    }
    Ok(tree)
}

/// A copy of `index` that keeps its modification time. Git believes a file whose size and time
/// match its entry, unless that entry is as new as the index: only then does it read the bytes,
/// which is all that shows an edit of the same size in the second the file was added. A copy dated
/// now, as a plain one is on Linux, tells git to believe the entry, and `add -A` keeps the old
/// bytes in the tree.
fn copy_index(index: &Path, copy: &Path) -> Result<(), Error> {
    let write = || -> std::io::Result<()> {
        let mut from = std::fs::File::open(index)?;
        let written = from.metadata()?.modified()?;
        let mut to = std::fs::File::create(copy)?;
        std::io::copy(&mut from, &mut to)?;
        to.set_modified(written)
    };
    write().map_err(|source| Error::Io {
        path: index.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_batch_output() {
        let oid: &[u8] = &[0xab; 20];
        let missing: &[u8] = &[0xcd; 20];
        let body = format!("{} blob 5\nhello\n", hex::encode(oid));
        let hashes = read_batch(body.as_bytes(), &[oid]).unwrap();
        assert_eq!(hashes, [(hushrun_digest::hash::sha256(b"hello"), 5)]);
        let body = format!("{} missing\n", hex::encode(missing));
        let err = read_batch(body.as_bytes(), &[missing]).unwrap_err();
        assert_eq!(err.code(), Some("E_MISSING_OBJECTS"));
    }
}
