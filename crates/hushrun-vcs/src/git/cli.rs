//! The `git` CLI: the second object reader (D-9), worktree snapshots (D-10) and `ls-remote`.
//!
//! git is spawned a bounded number of times per command, never once per file.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use hushrun_digest::Sha256;
use hushrun_digest::hash::{CHUNK, Hasher};

use crate::Error;

/// At most this many `git cat-file --batch` processes run at once.
const MAX_CAT_FILE: usize = 4;
/// Below this many objects per process, fewer processes are faster.
const MIN_OBJECTS_PER_PROCESS: usize = 256;

/// The program a git command runs, with any arguments it takes before git's own.
type Program<'a> = &'a [&'a str];
/// `git` on PATH. Only tests pass another program: a stand-in for a git version this machine
/// doesn't have.
const GIT: Program<'static> = &["git"];

fn command(program: Program) -> Command {
    let (name, args) = program.split_first().expect("a program has a name");
    let mut command = Command::new(name);
    command.args(args);
    command
}

/// Runs `git -C <root> <args>` and returns its stdout.
pub(crate) fn run<I, S>(root: &Path, args: I, envs: &[(&str, &OsStr)]) -> Result<Vec<u8>, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_in(GIT, root, args, envs)
}

fn run_in<I, S>(
    program: Program,
    root: &Path,
    args: I,
    envs: &[(&str, &OsStr)],
) -> Result<Vec<u8>, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let mut command = command(program);
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
    run_text_in(GIT, root, args, envs)
}

fn run_text_in<I, S>(
    program: Program,
    root: &Path,
    args: I,
    envs: &[(&str, &OsStr)],
) -> Result<String, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = run_in(program, root, args, envs)?;
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
    resolve_in(GIT, root, rev)
}

fn resolve_in(program: Program, root: &Path, rev: &str) -> Result<(String, String), Error> {
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
    let commit = run_text_in(
        program,
        root,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
        &no_lazy_fetch(),
    )
    .map_err(|err| unknown(err.to_string()))?;
    if commit.is_empty() || commit.contains('\n') {
        return Err(unknown("not a commit".into()));
    }
    // The commit names its tree. Peeling it to `^{tree}` would read the tree, which a tree-less
    // clone lacks: that is for `ls_tree` to find, and say.
    let body = run_in(
        program,
        root,
        ["cat-file", "commit", &commit],
        &no_lazy_fetch(),
    )?;
    let tree = body
        .strip_prefix(b"tree ")
        .and_then(|rest| rest.split(|b| *b == b'\n').next())
        .and_then(|id| std::str::from_utf8(id).ok())
        .filter(|id| matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| Error::GitOutput {
            args: "cat-file commit".into(),
            detail: format!("commit {commit} names no tree"),
        })?;
    Ok((commit, tree.to_string()))
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
    ls_tree_in(GIT, root, treeish)
}

fn ls_tree_in(program: Program, root: &Path, treeish: &str) -> Result<Vec<TreeRecord>, Error> {
    let out = run_in(
        program,
        root,
        ["ls-tree", "-r", "-z", "--full-tree", treeish],
        &no_lazy_fetch(),
    )
    .map_err(|err| or_missing(program, root, err, &[treeish]))?;
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
    hash_blobs_in(GIT, root, oids, jobs)
}

fn hash_blobs_in(
    program: Program,
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
            .map(|chunk| scope.spawn(move || cat_file(program, root, chunk)))
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
    read_blob_in(GIT, root, oid)
}

fn read_blob_in(program: Program, root: &Path, oid: &[u8]) -> Result<Vec<u8>, Error> {
    let oid = hex::encode(oid);
    run_in(program, root, ["cat-file", "blob", &oid], &no_lazy_fetch())
        .map_err(|err| or_missing(program, root, err, &[&oid]))
}

/// The environment of every git command that reads objects. A partial clone must fail with
/// E_MISSING_OBJECTS, not fetch what's left out one object at a time:
///
/// - `GIT_NO_LAZY_FETCH=1` turns lazy fetching off, in git since the 2024-05 security releases.
/// - An older git ignores it, and `GIT_ALLOW_PROTOCOL` stops it instead: it allows only the
///   transports it names, over any configuration, and this value names none (a transport's name
///   can't hold `_`). Its lazy fetch fails at once, with no network, and git dies as it does on a
///   promisor fetch it can't make.
/// - `LC_ALL=C` keeps git's messages in English, for `promisor_fetch_failed`. The output read
///   (`cat-file`, `ls-tree -z`, `rev-parse`) is never translated.
///
/// Commands that need a transport, `fetch` and `ls-remote`, never run with it.
fn no_lazy_fetch() -> [(&'static str, &'static OsStr); 3] {
    [
        ("GIT_NO_LAZY_FETCH", OsStr::new("1")),
        ("GIT_ALLOW_PROTOCOL", OsStr::new(NO_TRANSPORT)),
        ("LC_ALL", OsStr::new("C")),
    ]
}

/// A `GIT_ALLOW_PROTOCOL` that allows no transport.
const NO_TRANSPORT: &str = "hushrun_no_lazy_fetch";

/// `err`, from a git command that read objects under `no_lazy_fetch`, as E_MISSING_OBJECTS when
/// the repository lacks the object: the one git couldn't fetch from a promisor remote, else the
/// first that `lacks` finds among the ids git's error names and then `wanted`, in order. git
/// words a missing object differently from version to version, and some of the same words
/// ("not a tree object") also mean an object of another type, so the words alone decide nothing.
fn or_missing(program: Program, root: &Path, err: Error, wanted: &[&str]) -> Error {
    let Error::Git { stderr, .. } = &err else {
        return err;
    };
    let object = match promisor_fetch_failed(stderr) {
        Some(object) => Some(object.to_string()),
        None => object_ids(stderr)
            .into_iter()
            .chain(wanted.iter().copied())
            .find(|object| lacks(program, root, object))
            .map(str::to_string),
    };
    match object {
        Some(object) => Error::MissingObjects { object },
        None => err,
    }
}

/// The full object ids in `text`.
fn object_ids(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .filter(|word| matches!(word.len(), 40 | 64))
        .collect()
}

/// Whether the repository lacks `object`, as `git cat-file --batch-check` says under
/// `no_lazy_fetch`: newer git prints `<object> missing`; git 2.43, or one that ignores
/// `GIT_NO_LAZY_FETCH`, dies because it couldn't fetch the object from a promisor remote. An
/// object of another type is there, and anything git can't answer is not known to be missing.
fn lacks(program: Program, root: &Path, object: &str) -> bool {
    if object.is_empty() || object.contains(['\n', '\r']) {
        return false;
    }
    let child = command(program)
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch-check"])
        .envs(no_lazy_fetch())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    // One line fits in the pipe, so this can't wait on git. git that died first just ends early.
    let _ = (child.stdin.take().expect("piped stdin")).write_all(format!("{object}\n").as_bytes());
    let Ok(output) = child.wait_with_output() else {
        return false;
    };
    if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim_end() == format!("{object} missing")
    } else {
        promisor_fetch_failed(&String::from_utf8_lossy(&output.stderr)).is_some()
    }
}

/// The object git died on, when it died because it couldn't fetch the object from a promisor
/// remote. That is how some versions of git, such as 2.43, end when lazy fetching is disabled
/// and a partial clone lacks an object; newer ones say the object is missing. The message is read
/// as git writes it under `no_lazy_fetch`.
fn promisor_fetch_failed(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .find_map(|line| {
            line.trim_end()
                .strip_prefix("fatal: could not fetch ")?
                .strip_suffix(" from promisor remote")
        })
        .filter(|oid| !oid.is_empty() && oid.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Why a reader of `git cat-file --batch` stopped.
#[derive(Debug)]
enum Stop {
    /// The output ended where the header of the object at this index was due: git exited, and
    /// `batch` tells why from its exit status and stderr.
    EndedAt(usize),
    Failed(Error),
}

impl From<Error> for Stop {
    fn from(err: Error) -> Self {
        Stop::Failed(err)
    }
}

fn cat_file(program: Program, root: &Path, oids: &[&[u8]]) -> Result<Vec<(Sha256, u64)>, Error> {
    batch(program, root, oids, |out| read_batch(out, oids))
}

/// \[M8\] Calls `sink` with the bytes of each blob in `oids`, in order, from one
/// `git cat-file --batch`: an isolated run's export (§11.6).
pub(crate) fn read_blobs(
    root: &Path,
    oids: &[&[u8]],
    sink: &mut dyn FnMut(usize, Vec<u8>) -> Result<(), Error>,
) -> Result<(), Error> {
    read_blobs_in(GIT, root, oids, sink)
}

fn read_blobs_in(
    program: Program,
    root: &Path,
    oids: &[&[u8]],
    sink: &mut dyn FnMut(usize, Vec<u8>) -> Result<(), Error>,
) -> Result<(), Error> {
    if oids.is_empty() {
        return Ok(());
    }
    batch(program, root, oids, |mut out| {
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
                return Err(Stop::EndedAt(i));
            }
            let line = String::from_utf8_lossy(&header);
            let fields: Vec<&str> = line.trim_end().split(' ').collect();
            if fields.get(1) == Some(&"missing") {
                return Err(Error::MissingObjects {
                    object: hex::encode(oid),
                }
                .into());
            }
            let [_, "blob", size] = fields[..] else {
                return Err(bad(format!("unexpected header {:?}", line.trim_end())).into());
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
    program: Program,
    root: &Path,
    oids: &[&[u8]],
    read: impl FnOnce(BufReader<std::process::ChildStdout>) -> Result<T, Stop>,
) -> Result<T, Error> {
    let mut child = command(program)
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .envs(no_lazy_fetch())
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
    let value = match read {
        Ok(value) => value,
        Err(Stop::Failed(err)) => return Err(err),
        Err(Stop::EndedAt(i)) => return Err(ended_early(oids[i], status, &stderr)),
    };
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

/// What it means that `git cat-file --batch` ended before it said anything about `oid`. git
/// that died because it couldn't fetch the object from a promisor remote lacks it; anything else
/// is git's own error, in its words.
fn ended_early(oid: &[u8], status: ExitStatus, stderr: &str) -> Error {
    if status.success() {
        Error::GitOutput {
            args: "cat-file --batch".into(),
            detail: "output ended early".into(),
        }
    } else if promisor_fetch_failed(stderr).is_some() {
        Error::MissingObjects {
            object: hex::encode(oid),
        }
    } else {
        Error::Git {
            args: "cat-file --batch".into(),
            status: status.to_string(),
            stderr: stderr.trim().to_string(),
        }
    }
}

fn read_batch(mut out: impl BufRead, oids: &[&[u8]]) -> Result<Vec<(Sha256, u64)>, Stop> {
    let bad = |detail: String| Error::GitOutput {
        args: "cat-file --batch".into(),
        detail,
    };
    let mut hashes = Vec::with_capacity(oids.len());
    let mut header = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    for (i, oid) in oids.iter().enumerate() {
        header.clear();
        let n = out
            .read_until(b'\n', &mut header)
            .map_err(|e| bad(e.to_string()))?;
        if n == 0 {
            return Err(Stop::EndedAt(i));
        }
        let line = String::from_utf8_lossy(&header);
        let line = line.trim_end();
        let fields: Vec<&str> = line.split(' ').collect();
        if fields.get(1) == Some(&"missing") {
            return Err(Error::MissingObjects {
                object: hex::encode(oid),
            }
            .into());
        }
        let [_, kind, size] = fields[..] else {
            return Err(bad(format!("unexpected header {line:?}")).into());
        };
        if kind != "blob" {
            return Err(bad(format!("{} is a {kind}, not a blob", hex::encode(oid))).into());
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
    read_file_at_in(GIT, root, rev, path)
}

fn read_file_at_in(program: Program, root: &Path, rev: &str, path: &str) -> Result<Vec<u8>, Error> {
    run_in(
        program,
        root,
        ["cat-file", "blob", &format!("{rev}:{path}")],
        &no_lazy_fetch(),
    )
    .map_err(|err| {
        let on_the_way = objects_on_the_way(program, root, rev, path);
        let on_the_way: Vec<&str> = on_the_way.iter().map(String::as_str).collect();
        or_missing(program, root, err, &on_the_way)
    })
}

/// The objects git reads to find `path` at `rev`, as far as they can be named: the commit's
/// tree, then the tree of each folder on the way, then the file. A name stops where a tree that
/// should hold it can't be read, and that tree is the last one given.
fn objects_on_the_way(program: Program, root: &Path, rev: &str, path: &str) -> Vec<String> {
    let Ok((_, tree)) = resolve_in(program, root, rev) else {
        return Vec::new();
    };
    let mut objects = vec![tree];
    let ends = path.match_indices('/').map(|(i, _)| i).chain([path.len()]);
    for end in ends {
        let spec = format!("{rev}:{}", &path[..end]);
        let args = ["rev-parse", "--verify", "--quiet", &spec];
        match run_text_in(program, root, args, &no_lazy_fetch()) {
            Ok(id) if !id.is_empty() && !id.contains('\n') => objects.push(id),
            _ => break,
        }
    }
    objects
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
    resolve(root, rev).map(|(_, tree)| tree)
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
        let Err(Stop::Failed(err)) = read_batch(body.as_bytes(), &[missing]) else {
            panic!("a missing object is an error");
        };
        assert_eq!(err.code(), Some("E_MISSING_OBJECTS"));
        // An early end is for `batch` to judge, once git has exited.
        let body = format!("{} blob 5\nhello\n", hex::encode(oid));
        let ended = read_batch(body.as_bytes(), &[oid, missing]);
        assert!(matches!(ended, Err(Stop::EndedAt(1))), "{ended:?}");
    }

    /// Stand-ins for `git`, run by `sh`. git 2.43 with lazy fetching disabled doesn't report an
    /// object a partial clone lacks as missing: it warns, dies on it with this message and exits
    /// 128. Like a translated git, a stand-in says it in English only under `LC_ALL=C`.
    #[cfg(unix)]
    mod stand_in {
        use super::*;

        const PROMISOR: &str = r#"
promisor() {
    echo 'warning: lazy fetching disabled; some objects may not be available' >&2
    if [ "$LC_ALL" = C ] && [ "$GIT_NO_LAZY_FETCH" = 1 ]; then
        echo "fatal: could not fetch $1 from promisor remote" >&2
    else
        echo "Fehler: $1 konnte nicht vom Promisor-Remote abgerufen werden" >&2
    fi
    exit 128
}
"#;

        /// git 2.43 asked for an object a blob-less clone lacks first.
        const DIES_ON_FIRST: &str = "read -r oid; promisor \"$oid\"";
        /// git 2.43 that has the first blob ("hello") and lacks the second.
        const DIES_ON_SECOND: &str = r#"
read -r oid; printf '%s blob 5\nhello\n' "$oid"
read -r oid; promisor "$oid"
"#;
        /// git dying early for another reason.
        const OTHER_FATAL: &str = r#"
read -r oid
echo "fatal: packed object $oid (stored in pack-1.pack) is corrupt" >&2
exit 128
"#;
        /// git exiting 0 without printing a thing.
        const SAYS_NOTHING: &str = "cat >/dev/null; exit 0";

        /// Runs `f` with `script` as the git program, and a folder for the repository.
        fn with<T>(script: &str, f: impl FnOnce(Program, &Path) -> T) -> T {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("git.sh");
            std::fs::write(&path, format!("{PROMISOR}\n{script}\n")).unwrap();
            f(&["sh", path.to_str().unwrap()], dir.path())
        }

        /// What `hash_blobs` and `read_blobs` give when `script` is git, and which blobs
        /// `read_blobs` handed over before it.
        fn both_readers(script: &str, oids: &[&[u8]]) -> (Error, Error, Vec<(usize, Vec<u8>)>) {
            with(script, |git, root| {
                let hashed = hash_blobs_in(git, root, oids, 1).unwrap_err();
                let mut read = Vec::new();
                let streamed = read_blobs_in(git, root, oids, &mut |i, bytes| {
                    read.push((i, bytes));
                    Ok(())
                })
                .unwrap_err();
                (hashed, streamed, read)
            })
        }

        fn oids(n: u32) -> Vec<[u8; 20]> {
            (1..=n)
                .map(|i| {
                    let mut oid = [0x5a; 20];
                    oid[..4].copy_from_slice(&i.to_be_bytes());
                    oid
                })
                .collect()
        }

        fn assert_missing(err: &Error, oid: &[u8]) {
            assert_eq!(err.code(), Some("E_MISSING_OBJECTS"), "{err}");
            assert!(err.to_string().contains(&hex::encode(oid)), "{err}");
            assert!(err.to_string().contains("full or shallow clone"), "{err}");
        }

        // covers: D-9
        #[test]
        fn a_promisor_fetch_failure_on_the_first_object_is_missing_objects() {
            // More ids than a pipe holds: git dies before reading them all, and the write of
            // the rest fails.
            let oids = oids(5000);
            let oids: Vec<&[u8]> = oids.iter().map(|o| o.as_slice()).collect();
            let (hashed, streamed, read) = both_readers(DIES_ON_FIRST, &oids);
            assert_missing(&hashed, oids[0]);
            assert_missing(&streamed, oids[0]);
            assert!(read.is_empty());
        }

        // covers: D-9
        #[test]
        fn a_promisor_fetch_failure_names_the_object_it_stopped_at() {
            let oids = oids(3);
            let oids: Vec<&[u8]> = oids.iter().map(|o| o.as_slice()).collect();
            let (hashed, streamed, read) = both_readers(DIES_ON_SECOND, &oids);
            assert_missing(&hashed, oids[1]);
            assert_missing(&streamed, oids[1]);
            assert_eq!(read, [(0, b"hello".to_vec())]);
        }

        #[test]
        fn other_early_ends_are_not_missing_objects() {
            let oids = oids(2);
            let oids: Vec<&[u8]> = oids.iter().map(|o| o.as_slice()).collect();
            let (hashed, streamed, _) = both_readers(OTHER_FATAL, &oids);
            for err in [hashed, streamed] {
                assert_eq!(err.code(), None, "{err}");
                assert!(err.to_string().contains("is corrupt"), "{err}");
            }
            let (hashed, streamed, _) = both_readers(SAYS_NOTHING, &oids);
            for err in [hashed, streamed] {
                assert_eq!(err.code(), None, "{err}");
                assert!(err.to_string().contains("output ended early"), "{err}");
            }
        }

        // covers: D-9
        #[test]
        fn ls_tree_reports_the_tree_git_could_not_fetch() {
            // A tree-less clone: git names the tree it lacks, here one below the root.
            let subtree = "ab".repeat(20);
            let script = format!("promisor {subtree}");
            let err = with(&script, |git, root| ls_tree_in(git, root, "HEAD^{tree}")).err();
            let err = err.expect("ls-tree fails");
            assert_eq!(err.code(), Some("E_MISSING_OBJECTS"), "{err}");
            assert!(err.to_string().contains(&subtree), "{err}");

            let err = with(OTHER_FATAL, |git, root| ls_tree_in(git, root, "HEAD")).err();
            let err = err.expect("ls-tree fails");
            assert_eq!(err.code(), None, "{err}");
            assert!(err.to_string().contains("is corrupt"), "{err}");
        }

        // covers: D-9
        #[test]
        fn read_blob_reports_a_blob_git_could_not_fetch() {
            let oid = [0xcd; 20];
            // The object is git's last argument.
            let script = r#"for oid; do :; done; promisor "$oid""#;
            let err = with(script, |git, root| read_blob_in(git, root, &oid)).unwrap_err();
            assert_missing(&err, &oid);
        }
    }

    /// The real git, on repositories made here.
    mod real_git {
        use super::*;
        use hushrun_testkit::git;

        /// A repository with `a.txt` and `sub/b.txt`, served over `file://` with filters on, and a
        /// clone of it with `filter` (none for a full clone).
        struct Repos {
            _dir: tempfile::TempDir,
            clone: std::path::PathBuf,
        }

        impl Repos {
            fn new(filter: Option<&str>) -> Repos {
                let dir = tempfile::tempdir().unwrap();
                let origin = dir.path().join("origin");
                std::fs::create_dir_all(origin.join("sub")).unwrap();
                git::init(&origin, "sha1");
                git::run(&origin, &["config", "uploadpack.allowFilter", "true"], None);
                git::run(
                    &origin,
                    &["config", "uploadpack.allowAnySHA1InWant", "true"],
                    None,
                );
                std::fs::write(origin.join("a.txt"), b"a\n").unwrap();
                std::fs::write(origin.join("sub/b.txt"), b"b\n").unwrap();
                git::commit_all(&origin, "one");
                let clone = dir.path().join("clone");
                let url = format!("file://{}", origin.display());
                let mut args = vec!["clone", "-q", "--no-checkout"];
                let filter = filter.map(|f| format!("--filter={f}"));
                args.extend(filter.as_deref());
                args.extend([url.as_str(), clone.to_str().unwrap()]);
                git::run(dir.path(), &args, None);
                Repos { _dir: dir, clone }
            }

            fn id(&self, rev: &str) -> String {
                // The origin has every object, and asking it fetches nothing.
                git::text(&self.clone.with_file_name("origin"), &["rev-parse", rev])
            }

            /// Whether the clone has `object`, asked so that no git fetches it.
            fn has(&self, object: &str) -> bool {
                Command::new("git")
                    .arg("-C")
                    .arg(&self.clone)
                    .args(["cat-file", "-e", object])
                    .env("GIT_NO_LAZY_FETCH", "1")
                    .env("GIT_ALLOW_PROTOCOL", NO_TRANSPORT)
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            }
        }

        #[track_caller]
        fn assert_missing(err: &Error, object: &str) {
            assert_eq!(err.code(), Some("E_MISSING_OBJECTS"), "{err}");
            assert!(err.to_string().contains(object), "{object}: {err}");
        }

        /// git as it was before `GIT_NO_LAZY_FETCH` (the 2024-05 security releases), which
        /// fetches what a partial clone lacks from its promisor remote, one object at a time.
        #[cfg(unix)]
        const UNPATCHED: Program<'static> = &["env", "-u", "GIT_NO_LAZY_FETCH", "git"];

        // covers: D-9
        #[cfg(unix)]
        #[test]
        fn a_git_that_ignores_no_lazy_fetch_fetches_no_blob() {
            let repos = Repos::new(Some("blob:none"));
            let (repo, blob) = (&repos.clone, repos.id("HEAD:a.txt"));
            let oid = hex::decode(&blob).unwrap();
            let err = hash_blobs_in(UNPATCHED, repo, &[&oid], 1).unwrap_err();
            assert_missing(&err, &blob);
            let err = read_blobs_in(UNPATCHED, repo, &[&oid], &mut |_, _| Ok(())).unwrap_err();
            assert_missing(&err, &blob);
            let err = read_blob_in(UNPATCHED, repo, &oid).unwrap_err();
            assert_missing(&err, &blob);
            let err = read_file_at_in(UNPATCHED, repo, "HEAD", "a.txt").unwrap_err();
            assert_missing(&err, &blob);
            assert!(!repos.has(&blob), "the blob was fetched");
        }

        // covers: D-9
        #[cfg(unix)]
        #[test]
        fn a_git_that_ignores_no_lazy_fetch_fetches_no_tree() {
            let repos = Repos::new(Some("tree:0"));
            let (repo, tree) = (&repos.clone, repos.id("HEAD^{tree}"));
            let (_, resolved) = resolve_in(UNPATCHED, repo, "HEAD").unwrap();
            assert_eq!(resolved, tree);
            let err = ls_tree_in(UNPATCHED, repo, &tree).err().expect("no tree");
            assert_missing(&err, &tree);
            assert!(!repos.has(&tree), "the tree was fetched");

            let repos = Repos::new(Some("tree:1"));
            let (repo, tree, sub) = (&repos.clone, repos.id("HEAD^{tree}"), repos.id("HEAD:sub"));
            let err = ls_tree_in(UNPATCHED, repo, &tree)
                .err()
                .expect("no subtree");
            assert_missing(&err, &sub);
            assert!(!repos.has(&sub), "the subtree was fetched");
        }

        #[test]
        fn an_object_of_another_type_is_not_missing() {
            let repos = Repos::new(None);
            let repo = &repos.clone;
            let (tree, blob) = (repos.id("HEAD^{tree}"), repos.id("HEAD:a.txt"));
            let err = ls_tree_in(GIT, repo, &blob)
                .err()
                .expect("a blob is no tree");
            assert_eq!(err.code(), None, "{err}");
            let err = read_blob_in(GIT, repo, &hex::decode(&tree).unwrap()).unwrap_err();
            assert_eq!(err.code(), None, "{err}");
            let err = read_file_at_in(GIT, repo, "HEAD", "sub").unwrap_err();
            assert_eq!(err.code(), None, "{err}");
            let tree = hex::decode(&tree).unwrap();
            let err = hash_blobs_in(GIT, repo, &[&tree], 1).unwrap_err();
            assert_eq!(err.code(), None, "{err}");
        }
    }
}
