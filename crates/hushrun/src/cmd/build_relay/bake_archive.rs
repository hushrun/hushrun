//! B-71: a baked store as one archive (`--tar`) or one read-only EROFS disk image (`--erofs`),
//! for what can't take an image: a sandbox snapshotted while it runs, and a volume mounted
//! read-only.
//!
//! Both are a pure function of the store's files and their bytes: the tar is B-65's layer bytes
//! (`hushrun_core::blocks::image::chunks`) of the folder under `hushrun/`, the disk image
//! `hushrun_core::blocks::erofs::chunks` of the folder at its root. They hold `baked.json` and
//! `baked.sigstore.json` as the folder does, so a run that reads the extracted or mounted folder
//! has the same evidence a run that reads the image has (B-67).

use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom, Write};
use std::path::{Path, PathBuf};

use hushrun_build::baked::{self, BAKED_JSON, Baked, SIGNATURE};
use hushrun_core::blocks::Hex32;
use hushrun_core::blocks::erofs;
use hushrun_core::blocks::image::{self as layer, File, STORE_ROOT};
use sha2::{Digest as _, Sha256};

/// zstd's level for a `.zst` archive, on one thread: the same bytes for the same tar, for one
/// zstd version.
const ZSTD_LEVEL: i32 = 3;

/// What `--tar` or `--erofs` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
    /// An uncompressed tar, files under `hushrun/`.
    Tar,
    /// The same tar, compressed with zstd.
    TarZst,
    /// An EROFS disk image whose root is the store.
    Erofs,
}

impl Form {
    /// The word `--format json` and the text say.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Form::Tar => "tar",
            Form::TarZst => "tar.zst",
            Form::Erofs => "erofs",
        }
    }

    /// The tar's form for the file `--tar` names: zstd when its name ends in `.zst`.
    pub(crate) fn of_tar(path: &Path) -> Form {
        if path.extension().is_some_and(|e| e == "zst") {
            Form::TarZst
        } else {
            Form::Tar
        }
    }
}

/// Where an archive goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dest {
    /// A file, written whole: under another name beside it, then renamed.
    File(PathBuf),
    /// Standard output (`-`).
    Stdout,
}

impl Dest {
    /// `-` is standard output; any other path a file.
    pub(crate) fn of(path: &Path) -> Dest {
        if path == Path::new("-") {
            Dest::Stdout
        } else {
            Dest::File(path.to_path_buf())
        }
    }
}

/// An archive written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Written {
    pub(crate) form: Form,
    /// The file, or `None` for standard output.
    pub(crate) path: Option<PathBuf>,
    /// The bytes written.
    pub(crate) bytes: u64,
    /// Their SHA-256.
    pub(crate) sha256: Hex32,
    /// The store's files it holds.
    pub(crate) files: usize,
    /// The uncompressed tar's size, for a `.zst`.
    pub(crate) unpacked: u64,
}

/// The files of the baked store in `folder`, as paths from the store's root, their sources and
/// sizes: `baked.json`, its signature when there is one, each pack `baked.json` names and each
/// hint. Sorted by path as bytes.
fn store_files(folder: &Path, store: &Baked) -> Result<Vec<(String, PathBuf, u64)>, String> {
    let size = |path: &Path| {
        fs::symlink_metadata(path)
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .ok_or_else(|| format!("{} is not a file of the store", path.display()))
    };
    let mut out = Vec::new();
    for name in [BAKED_JSON, SIGNATURE] {
        let path = folder.join(name);
        if name == SIGNATURE && fs::symlink_metadata(&path).is_err() {
            continue;
        }
        out.push((name.to_string(), path.clone(), size(&path)?));
    }
    for row in &store.packs {
        let path = baked::pack_path(folder, &row.address);
        let bytes = size(&path)?;
        if bytes != row.bytes {
            return Err(format!(
                "{} has {bytes} bytes, not the {} baked.json says",
                path.display(),
                row.bytes
            ));
        }
        out.push((
            format!("{}/{}.pack", baked::PACKS, row.address),
            path,
            bytes,
        ));
    }
    for hint in baked::hint_files(folder).ok_or("the hints can't be read")? {
        let path = folder.join(&hint);
        out.push((hint, path.clone(), size(&path)?));
    }
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(out)
}

/// A writer that counts and hashes what goes through it.
struct Hashing<W> {
    inner: W,
    hash: Sha256,
    bytes: u64,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Copies exactly `len` bytes of `source` from `offset` into `out`; `Err` when it has fewer, or
/// when the file isn't `size` bytes once they are copied (it changed meanwhile).
fn copy_range(
    source: &Path,
    offset: u64,
    len: u64,
    size: u64,
    out: &mut dyn Write,
) -> Result<(), String> {
    let at = |e: std::io::Error| format!("{}: {e}", source.display());
    let mut file = fs::File::open(source).map_err(at)?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset)).map_err(at)?;
    }
    let copied = std::io::copy(&mut (&mut file).take(len), out).map_err(at)?;
    if copied != len {
        return Err(format!(
            "{} changed while it was written: {copied} bytes where {len} were",
            source.display()
        ));
    }
    // A file that grew meanwhile has bytes past its size: what was copied is part of other
    // bytes, and the archive would name a file that isn't the one baked.
    let now = file.metadata().map_err(at)?.len();
    if now != size {
        return Err(format!(
            "{} changed while it was written: it holds {now} bytes where it held {size}",
            source.display()
        ));
    }
    Ok(())
}

/// Writes the bytes of `form` for `files` into `out`.
fn write_form(
    form: Form,
    files: &[(String, PathBuf, u64)],
    uuid: [u8; 16],
    out: &mut dyn Write,
) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    match form {
        Form::Tar | Form::TarZst => {
            let list: Vec<File> = files
                .iter()
                .map(|(path, _, size)| File {
                    path: format!("{STORE_ROOT}/{path}"),
                    size: *size,
                })
                .collect();
            for chunk in layer::chunks(&list).map_err(|e| e.to_string())? {
                match chunk {
                    layer::Chunk::Header(h) => out.write_all(&h[..]).map_err(io)?,
                    layer::Chunk::File(i) => {
                        copy_range(&files[i].1, 0, files[i].2, files[i].2, out)?
                    }
                    layer::Chunk::Zeros(n) => zeros(out, n)?,
                }
            }
        }
        Form::Erofs => {
            let list: Vec<File> = files
                .iter()
                .map(|(path, _, size)| File {
                    path: path.clone(),
                    size: *size,
                })
                .collect();
            for chunk in erofs::chunks(&list, uuid).map_err(|e| e.to_string())? {
                match chunk {
                    erofs::Chunk::Bytes(b) => out.write_all(&b).map_err(io)?,
                    erofs::Chunk::Range { file, offset, len } => {
                        copy_range(&files[file].1, offset, len, files[file].2, out)?
                    }
                    erofs::Chunk::Zeros(n) => zeros(out, n)?,
                }
            }
        }
    }
    Ok(())
}

/// Writes `n` NULs.
fn zeros(out: &mut dyn Write, mut n: u64) -> Result<(), String> {
    let block = [0u8; 8192];
    while n > 0 {
        let take = n.min(block.len() as u64) as usize;
        out.write_all(&block[..take]).map_err(|e| e.to_string())?;
        n -= take as u64;
    }
    Ok(())
}

/// The disk image's UUID: the first 16 bytes of the SHA-256 of `baked.json`, as an RFC 9562
/// version 8 UUID, so the same store has the same one and another store another.
fn uuid_of(baked_json: &Hex32) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&baked_json.0[..16]);
    uuid[6] = (uuid[6] & 0x0f) | 0x80;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    uuid
}

/// B-71: writes the baked store in `folder` (as `store` reads it, signed or not as it is) as
/// `form` to `dest`. A file is written under a name no other process uses beside it, synced, and
/// renamed into place, so `dest` is always a whole archive or none; standard output is written as
/// it goes.
pub(crate) fn write(
    folder: &Path,
    store: &Baked,
    form: Form,
    dest: &Dest,
) -> Result<Written, String> {
    let files = store_files(folder, store)?;
    let baked_json = baked::baked_json_digest(folder).ok_or("baked.json can't be read")?;
    let uuid = uuid_of(&baked_json);
    let unpacked = match form {
        Form::Erofs => erofs::size(
            &files
                .iter()
                .map(|(p, _, s)| File {
                    path: p.clone(),
                    size: *s,
                })
                .collect::<Vec<_>>(),
        ),
        _ => layer::size(
            &files
                .iter()
                .map(|(p, _, s)| File {
                    path: format!("{STORE_ROOT}/{p}"),
                    size: *s,
                })
                .collect::<Vec<_>>(),
        ),
    }
    .map_err(|e| e.to_string())?;
    let emit = |sink: &mut dyn Write| -> Result<(Hex32, u64), String> {
        let mut hashing = Hashing {
            inner: sink,
            hash: Sha256::new(),
            bytes: 0,
        };
        match form {
            Form::TarZst => {
                let mut encoder = zstd::stream::write::Encoder::new(&mut hashing, ZSTD_LEVEL)
                    .map_err(|e| e.to_string())?;
                write_form(form, &files, uuid, &mut encoder)?;
                encoder.finish().map_err(|e| e.to_string())?;
            }
            _ => write_form(form, &files, uuid, &mut hashing)?,
        }
        hashing.flush().map_err(|e| e.to_string())?;
        Ok((Hex32(hashing.hash.finalize().into()), hashing.bytes))
    };
    let (sha256, bytes, path) = match dest {
        Dest::Stdout => {
            let stdout = std::io::stdout();
            let mut lock = std::io::BufWriter::with_capacity(1 << 20, stdout.lock());
            let (sha, bytes) = emit(&mut lock)?;
            lock.flush().map_err(|e| format!("stdout: {e}"))?;
            (sha, bytes, None)
        }
        Dest::File(path) => {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("{} names no file", path.display()))?;
            let temp = parent.join(baked::unique(&format!(".{name}.tmp")));
            let gone = RemoveUnlessKept(Some(temp.clone()));
            let at = |e: std::io::Error| format!("{}: {e}", temp.display());
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(at)?;
            let mut buffered = std::io::BufWriter::with_capacity(1 << 20, file);
            let (sha, bytes) = emit(&mut buffered)?;
            let file = buffered.into_inner().map_err(|e| at(e.into_error()))?;
            file.sync_all().map_err(at)?;
            drop(file);
            fs::rename(&temp, path).map_err(|e| format!("{}: {e}", path.display()))?;
            gone.keep();
            // The new name on the disk too: a crash after the rename finds the whole archive.
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
            (sha, bytes, Some(path.clone()))
        }
    };
    Ok(Written {
        form,
        path,
        bytes,
        sha256,
        files: files.len(),
        unpacked,
    })
}

/// A temporary file removed however a write ends, unless kept.
struct RemoveUnlessKept(Option<PathBuf>);

impl RemoveUnlessKept {
    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveUnlessKept {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-71
    // A file that shrank, or grew, while an archive copied it fails the archive: either way the
    // bytes written aren't the file baked.
    #[test]
    fn a_file_that_changed_size_while_copied_fails_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        fs::write(&file, b"0123456789").unwrap();
        let mut out = Vec::new();
        copy_range(&file, 0, 10, 10, &mut out).unwrap();
        assert_eq!(out, b"0123456789");
        // A part of it, as the disk image copies a file's blocks and its tail apart.
        let mut part = Vec::new();
        copy_range(&file, 4, 3, 10, &mut part).unwrap();
        assert_eq!(part, b"456");
        // Baked at 9 bytes, 10 now: it grew.
        let grew = copy_range(&file, 0, 9, 9, &mut Vec::new()).unwrap_err();
        assert!(grew.contains("holds 10 bytes where it held 9"), "{grew}");
        let shrank = copy_range(&file, 0, 11, 11, &mut Vec::new()).unwrap_err();
        assert!(shrank.contains("10 bytes where 11"), "{shrank}");
    }

    /// A tar's entries as `tar` would list them: (path, size, mode, uid).
    fn entries(tar: &[u8]) -> Vec<(String, u64, u32, u32)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at + 512 <= tar.len() && tar[at] != 0 {
            let h = &tar[at..at + 512];
            let text = |r: std::ops::Range<usize>| {
                let s = &h[r];
                let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
                String::from_utf8_lossy(&s[..end]).into_owned()
            };
            let octal = |r| u64::from_str_radix(text(r).trim(), 8).unwrap();
            let name = text(0..100);
            let prefix = text(345..500);
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let size = octal(124..136);
            out.push((path, size, octal(100..108) as u32, octal(108..116) as u32));
            at += 512 + size.div_ceil(512) as usize * 512;
        }
        out
    }

    /// A baked store's folder: three packs (one of more than a block), a hint, `baked.json`.
    fn folder() -> (tempfile::TempDir, Baked) {
        use hushrun_build::baked::{BakedCheck, PackRow, Part, Source, Trust};
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path();
        fs::create_dir_all(folder.join(baked::PACKS)).unwrap();
        let mut rows = Vec::new();
        for seed in 1u8..=3 {
            let bytes: Vec<u8> = (0..700 + 5000 * usize::from(seed))
                .map(|n| (n * 31 + usize::from(seed)) as u8)
                .collect();
            let address = Hex32([seed; 32]);
            fs::write(baked::pack_path(folder, &address), &bytes).unwrap();
            rows.push(PackRow {
                kind: "rustc/1".into(),
                address,
                bytes: bytes.len() as u64,
                sha256: Hex32::sha256(&bytes),
                outputs: Hex32([seed + 10; 32]),
                class: hushrun_core::blocks::Class::Library,
                cost_ms: 5,
                scope: None,
                link_environment: None,
            });
        }
        let hint_dir = folder.join(baked::HINTS).join(Hex32([1; 32]).to_string());
        fs::create_dir_all(&hint_dir).unwrap();
        let hint = b"sealed hint".to_vec();
        fs::write(hint_dir.join(Hex32::sha256(&hint).to_string()), &hint).unwrap();
        let checks = vec![BakedCheck {
            check: None,
            parts: vec![Part {
                kind: "rustc/1".into(),
                platform: "x86_64-unknown-linux-gnu".into(),
                manifest: Hex32([9; 32]),
            }],
        }];
        let store = Baked {
            schema: baked::SCHEMA.into(),
            trust: Trust::Untrusted {
                because: hushrun_build::baked::Untrusted::LocalStore,
            },
            manifest: baked::store_manifest(&checks),
            source: Source::Store,
            repository: Some("github.com/acme/shop".into()),
            platform: "linux-x64".into(),
            checks,
            toolchains: Vec::new(),
            packs: rows,
            baked_ms: 1,
            paths: None,
        };
        fs::write(
            folder.join(BAKED_JSON),
            serde_json::to_vec_pretty(&store).unwrap(),
        )
        .unwrap();
        (dir, store)
    }

    // covers: B-71
    #[test]
    fn the_tar_holds_the_store_under_hushrun_and_the_same_store_gives_the_same_bytes() {
        let (dir, store) = folder();
        fs::write(dir.path().join(SIGNATURE), b"{\"signed\":1}").unwrap();
        let out = tempfile::tempdir().unwrap();
        let one = out.path().join("one.tar");
        let two = out.path().join("two.tar");
        let a = write(dir.path(), &store, Form::Tar, &Dest::File(one.clone())).unwrap();
        let b = write(dir.path(), &store, Form::Tar, &Dest::File(two.clone())).unwrap();
        assert_eq!(fs::read(&one).unwrap(), fs::read(&two).unwrap());
        assert_eq!(a.sha256, b.sha256);
        assert_eq!(a.bytes, fs::metadata(&one).unwrap().len());
        assert_eq!(a.bytes, a.unpacked);
        let listed = entries(&fs::read(&one).unwrap());
        let paths: Vec<&str> = listed.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(paths[0], "hushrun/");
        assert!(paths.contains(&"hushrun/baked.json"));
        assert!(paths.contains(&"hushrun/baked.sigstore.json"));
        assert!(
            paths
                .iter()
                .any(|p| p.starts_with("hushrun/packs/") && p.ends_with(".pack"))
        );
        assert!(paths.iter().any(|p| p.starts_with("hushrun/hints/")));
        // Folders 0755, files 0644, all root's: no mode a umask could make group-writable.
        for (path, _, mode, uid) in &listed {
            assert_eq!(
                *mode,
                if path.ends_with('/') { 0o755 } else { 0o644 },
                "{path}"
            );
            assert_eq!(*uid, 0);
        }
        // Nothing left beside it.
        let left: Vec<_> = fs::read_dir(out.path()).unwrap().flatten().collect();
        assert_eq!(left.len(), 2);
    }

    // covers: B-71
    #[cfg(unix)]
    #[test]
    fn a_tar_extracted_by_the_user_who_runs_builds_passes_the_default_folder_check_whatever_the_umask()
     {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, store) = folder();
        let out = tempfile::tempdir().unwrap();
        let tar = out.path().join("s.tar");
        write(dir.path(), &store, Form::Tar, &Dest::File(tar.clone())).unwrap();
        for umask in ["002", "022", "077"] {
            let root = out.path().join(format!("root-{umask}"));
            fs::create_dir(&root).unwrap();
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("umask {umask} && tar -x -C \"$1\" -f \"$2\""))
                .arg("sh")
                .arg(&root)
                .arg(&tar)
                .status()
                .unwrap();
            assert!(status.success());
            let store_dir = root.join(STORE_ROOT);
            assert_eq!(baked::default_refused(&store_dir), None, "umask {umask}");
            // The same bytes, the archive's modes less the umask: never group-writable.
            let mode = fs::metadata(store_dir.join(BAKED_JSON))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o022, 0, "umask {umask}");
            let (read, _) = Baked::read_bytes(&store_dir).unwrap();
            assert_eq!(read, store);
        }
        // Made group-writable afterwards: refused, with the fix.
        let store_dir = out.path().join("root-022").join(STORE_ROOT);
        fs::set_permissions(
            store_dir.join(baked::PACKS),
            fs::Permissions::from_mode(0o775),
        )
        .unwrap();
        let why = baked::default_refused(&store_dir).unwrap();
        assert!(why.contains("chmod -R g-w"), "{why}");
    }

    // covers: B-71
    #[test]
    fn a_zst_archive_is_the_tar_compressed() {
        let (dir, store) = folder();
        let out = tempfile::tempdir().unwrap();
        let tar = out.path().join("s.tar");
        let zst = out.path().join("s.tar.zst");
        assert_eq!(Form::of_tar(&zst), Form::TarZst);
        assert_eq!(Form::of_tar(&tar), Form::Tar);
        write(dir.path(), &store, Form::Tar, &Dest::File(tar.clone())).unwrap();
        let w = write(dir.path(), &store, Form::TarZst, &Dest::File(zst.clone())).unwrap();
        let unpacked = zstd::stream::decode_all(&fs::read(&zst).unwrap()[..]).unwrap();
        assert_eq!(unpacked, fs::read(&tar).unwrap());
        assert_eq!(w.unpacked, unpacked.len() as u64);
        let again = out.path().join("again.tar.zst");
        write(dir.path(), &store, Form::TarZst, &Dest::File(again.clone())).unwrap();
        assert_eq!(fs::read(&zst).unwrap(), fs::read(&again).unwrap());
    }

    // covers: B-71
    #[test]
    fn the_disk_image_is_the_same_bytes_for_the_same_store_and_another_for_another() {
        let (dir, store) = folder();
        let out = tempfile::tempdir().unwrap();
        let one = out.path().join("one.erofs");
        let a = write(dir.path(), &store, Form::Erofs, &Dest::File(one.clone())).unwrap();
        let b = write(
            dir.path(),
            &store,
            Form::Erofs,
            &Dest::File(out.path().join("two.erofs")),
        )
        .unwrap();
        assert_eq!(a.sha256, b.sha256);
        let bytes = fs::read(&one).unwrap();
        assert_eq!(
            u32::from_le_bytes(bytes[1024..1028].try_into().unwrap()),
            erofs::MAGIC
        );
        assert_eq!(bytes.len() as u64 % erofs::BLOCK, 0);
        // The UUID follows baked.json.
        let uuid = uuid_of(&baked::baked_json_digest(dir.path()).unwrap());
        assert_eq!(&bytes[1024 + 48..1024 + 64], &uuid);
        // A pack whose size isn't its row's is refused, and no file is left.
        let row = &store.packs[0];
        fs::write(baked::pack_path(dir.path(), &row.address), b"short").unwrap();
        let three = out.path().join("three.erofs");
        assert!(write(dir.path(), &store, Form::Erofs, &Dest::File(three.clone())).is_err());
        let left: Vec<_> = fs::read_dir(out.path()).unwrap().flatten().collect();
        assert_eq!(left.len(), 2);
    }
}
