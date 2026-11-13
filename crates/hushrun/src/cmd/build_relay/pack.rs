//! B-28: a block from the store into a pack, and a downloaded pack into the store. The format
//! is `hushrun_core::blocks::pack`'s; this is where its bytes live while they are made or read.
//! Packing takes two passes (the salt and `begin` both need a hash of the whole stream first):
//! the compressed stream, then the sealed pack, each held in memory under 64 MiB and in a file
//! under the run's `outbox/tmp/<run>/` above it.

use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use hushrun_build::origin::Origin;
use hushrun_build::store::{NewFile, Replaced, Store};
use hushrun_core::Digest;
use hushrun_core::blocks::Hex32;
use hushrun_core::blocks::pack::{self, Expect, PackError, PackFile, PackRecord, Sealed, Sink};

/// Bytes held in memory below this, and in a file above it.
pub(crate) const IN_MEMORY: usize = 64 << 20;

/// Bytes being written: in memory until they pass [`IN_MEMORY`], then in a file.
pub(crate) enum Spool {
    Memory(Vec<u8>),
    File(PathBuf, File),
}

impl Spool {
    fn new() -> Spool {
        Spool::Memory(Vec::new())
    }

    /// A reader from the start.
    pub(crate) fn reader(&self) -> io::Result<Box<dyn Read + Send + '_>> {
        match self {
            Spool::Memory(bytes) => Ok(Box::new(Cursor::new(bytes.as_slice()))),
            Spool::File(path, _) => Ok(Box::new(File::open(path)?)),
        }
    }

    /// The bytes, read whole: for tests and small packs.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> io::Result<Vec<u8>> {
        match self {
            Spool::Memory(bytes) => Ok(bytes.clone()),
            Spool::File(path, _) => fs::read(path),
        }
    }

    /// A PUT's body (B-29): the bytes held in memory as they are, or the file streamed a chunk
    /// at a time, never read whole. The file, to remove once the PUT is done.
    pub(crate) fn into_body(self) -> io::Result<(reqwest::Body, Option<PathBuf>)> {
        match self {
            Spool::Memory(bytes) => Ok((reqwest::Body::from(bytes), None)),
            Spool::File(path, _) => {
                let file = File::open(&path)?;
                let chunks = futures_util::stream::unfold(Some(file), |file| async move {
                    let mut file = file?;
                    let read = tokio::task::spawn_blocking(move || {
                        let mut chunk = vec![0u8; PUT_CHUNK];
                        let n = file.read(&mut chunk)?;
                        chunk.truncate(n);
                        Ok::<_, io::Error>((file, chunk))
                    })
                    .await
                    .unwrap_or_else(|err| Err(io::Error::other(err)));
                    match read {
                        Ok((_, chunk)) if chunk.is_empty() => None,
                        Ok((file, chunk)) => Some((Ok(chunk), Some(file))),
                        Err(err) => Some((Err(err), None)),
                    }
                });
                Ok((reqwest::Body::wrap_stream(chunks), Some(path)))
            }
        }
    }

    /// Removes the file, when there is one.
    pub(crate) fn remove(self) {
        if let Spool::File(path, _) = self {
            let _ = fs::remove_file(path);
        }
    }
}

/// How much of a spilled pack a PUT reads at once.
const PUT_CHUNK: usize = 1 << 20;

/// A writer that spills into `dir` past `limit` bytes ([`IN_MEMORY`] but in tests).
struct Spilling<'a> {
    spool: Spool,
    dir: &'a Path,
    name: String,
    limit: usize,
}

impl Write for Spilling<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Spool::Memory(bytes) = &mut self.spool {
            if bytes.len() + buf.len() <= self.limit {
                bytes.extend_from_slice(buf);
                return Ok(buf.len());
            }
            fs::create_dir_all(self.dir)?;
            let path = self.dir.join(&self.name);
            let mut file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)?;
            file.write_all(bytes)?;
            self.spool = Spool::File(path, file);
        }
        match &mut self.spool {
            Spool::File(_, file) => file.write(buf),
            Spool::Memory(_) => unreachable!("spilled above"),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.spool {
            Spool::File(_, file) => file.flush(),
            Spool::Memory(_) => Ok(()),
        }
    }
}

/// A packed block, ready to go up.
pub(crate) struct Packed {
    pub(crate) sealed: Sealed,
    pub(crate) outputs: Hex32,
    pub(crate) pack: Spool,
}

/// Why a block couldn't be packed.
#[derive(Debug)]
pub(crate) enum NotPacked {
    /// The block is gone from the store, or its record can't be read.
    Gone,
    /// A file changed under the packer, or the pack would be too large: it never goes up.
    Never(String),
    /// Reading or writing failed: try again later.
    Io(String),
}

/// Packs the block under `key` from `store`, spilling into `dir`. Each file's size and SHA-256
/// are checked against the store's record while it is read (B-29).
#[cfg(test)]
pub(crate) fn pack_block(store: &Store, key: &Digest, dir: &Path) -> Result<Packed, NotPacked> {
    pack_block_within(store, key, dir, IN_MEMORY)
}

/// [`pack_block`], holding at most `limit` bytes of each pass in memory.
pub(crate) fn pack_block_within(
    store: &Store,
    key: &Digest,
    dir: &Path,
    limit: usize,
) -> Result<Packed, NotPacked> {
    let block = match store.block(key) {
        Ok(Some(block)) => block,
        _ => return Err(NotPacked::Gone),
    };
    let step = block
        .description
        .get("step")
        .and_then(|v| serde_json::from_value::<Digest>(v.clone()).ok())
        .ok_or(NotPacked::Gone)?;
    let record = PackRecord {
        key: Hex32(*key.as_bytes()),
        step: Hex32(*step.as_bytes()),
        description: pack::normalize(&block.description),
        files: block
            .files
            .iter()
            .map(|f| PackFile {
                name: f.name.clone(),
                size: f.size,
                sha256: Hex32(*f.sha256.as_bytes()),
            })
            .collect(),
    };
    let outputs = pack::outputs_of(&record);
    let paths: Vec<PathBuf> = block.files.iter().map(|f| f.path.clone()).collect();
    let mut stream = Spilling {
        spool: Spool::new(),
        dir,
        name: format!("{}.stream", key.hex()),
        limit,
    };
    let stream_sha = pack::compress(
        &record,
        &mut |i| Ok(Box::new(File::open(&paths[i])?) as Box<dyn Read>),
        &mut stream,
    )
    .map_err(|err| match err {
        PackError::Io(err) => NotPacked::Io(err.to_string()),
        other => NotPacked::Never(other.to_string()),
    })?;
    let stream = stream.spool;
    let mut sealed_out = Spilling {
        spool: Spool::new(),
        dir,
        name: format!("{}.pack", key.hex()),
        limit,
    };
    let sealed = (|| {
        let mut reader = stream.reader().map_err(PackError::Io)?;
        pack::seal(key.as_bytes(), &stream_sha, &mut reader, &mut sealed_out)
    })();
    stream.remove();
    let sealed = sealed.map_err(|err| match err {
        PackError::Io(err) => NotPacked::Io(err.to_string()),
        other => NotPacked::Never(other.to_string()),
    })?;
    if let Spool::File(_, file) = &mut sealed_out.spool {
        let _ = file.flush();
    }
    Ok(Packed {
        sealed,
        outputs,
        pack: sealed_out.spool,
    })
}

/// Writes a pack's files into a folder.
struct Unpack {
    dir: PathBuf,
    files: Vec<(String, PathBuf)>,
    current: Option<File>,
}

impl Sink for Unpack {
    fn file(&mut self, index: usize, name: &str, _size: u64) -> io::Result<()> {
        if let Some(mut file) = self.current.take() {
            file.flush()?;
        }
        // Each file under a numbered name: the record's names were checked, but the store
        // copies them in under their names itself.
        let path = self.dir.join(format!("{index}"));
        self.current = Some(File::create(&path)?);
        self.files.push((name.to_string(), path));
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.current {
            Some(file) => file.write_all(bytes),
            None => Err(io::Error::other("bytes before a file")),
        }
    }
}

/// What opening a pack gave.
pub(crate) struct Opened {
    pub(crate) record: PackRecord,
    /// The files, by name and where they were written.
    pub(crate) files: Vec<(String, PathBuf)>,
    /// The folder they are in, removed by [`Opened::remove`].
    pub(crate) dir: PathBuf,
}

impl Opened {
    pub(crate) fn remove(&self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Opens the pack at `path` with `expect` into a new folder `dir`. The checks run in the
/// format's order; nothing is published here.
pub(crate) fn open_pack(
    path: &Path,
    expect: &Expect,
    room: Option<pack::Room>,
    dir: &Path,
) -> Result<Opened, PackError> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(PackError::Io)?;
    let mut sink = Unpack {
        dir: dir.to_path_buf(),
        files: Vec::new(),
        current: None,
    };
    let opened = (|| {
        let mut input = io::BufReader::new(File::open(path).map_err(PackError::Io)?);
        let record = pack::open_within(&mut input, expect, room, &mut sink)?;
        if let Some(mut file) = sink.current.take() {
            file.flush().map_err(PackError::Io)?;
        }
        Ok(record)
    })();
    match opened {
        Ok(record) => Ok(Opened {
            record,
            files: sink.files,
            dir: dir.to_path_buf(),
        }),
        Err(err) => {
            let _ = fs::remove_dir_all(dir);
            Err(err)
        }
    }
}

/// Publishes an opened pack into the store under `key` (B-32): the record's description with
/// this machine's repository, date and the stated cost filled in, and its record of where it
/// came from. A block there that was only downloaded gives way; one built here, or with no
/// record, stays.
pub(crate) fn publish(
    store: &Store,
    key: &Digest,
    opened: &Opened,
    repository: Option<&str>,
    cost_ms: u64,
    origin: &Origin,
    now: SystemTime,
) -> Result<Replaced, String> {
    let mut description = opened.record.description.clone();
    if let Some(object) = description.as_object_mut() {
        object.insert(
            "repository".into(),
            repository.map_or(serde_json::Value::Null, |r| r.into()),
        );
        let ms = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        object.insert("built_ms".into(), ms.into());
        object.insert("rustc_ms".into(), cost_ms.into());
    }
    let files: Vec<NewFile> = opened
        .files
        .iter()
        .map(|(name, path)| NewFile {
            name: name.clone(),
            source: path.clone(),
        })
        .collect();
    store
        .replace(key, &description, &files, origin, now, |there| {
            there.is_some_and(Origin::downloaded_only)
        })
        .map_err(|err| err.to_string())
}

/// The SHA-256s of an opened pack's files, by name, against a stored block's: whether they
/// are the same outputs (shared `observe`'s comparison, B-33).
pub(crate) fn same_outputs(record: &PackRecord, block: &hushrun_build::store::Block) -> bool {
    record.files.len() == block.files.len()
        && record.files.iter().all(|f| {
            block
                .files
                .iter()
                .any(|b| b.name == f.name && b.size == f.size && *b.sha256.as_bytes() == f.sha256.0)
        })
}
