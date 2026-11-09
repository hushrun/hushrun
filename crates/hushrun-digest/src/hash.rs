//! SHA-256, behind one set of functions so the implementation can change (plan §12.3).

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::Digest as _;

use crate::Sha256;

/// Files up to this size are read whole; larger ones are streamed.
pub const WHOLE_FILE_LIMIT: u64 = 1 << 20;
/// The chunk size for streamed files.
pub const CHUNK: usize = 256 << 10;

/// SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> Sha256 {
    sha2::Sha256::digest(bytes).into()
}

/// An incremental SHA-256.
#[derive(Default)]
pub struct Hasher(sha2::Sha256);

impl Hasher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn finish(self) -> Sha256 {
        self.0.finalize().into()
    }
}

/// SHA-256 and size of a file's bytes. Files over 1 MiB are streamed in 256 KiB chunks, so
/// memory stays bounded whatever the file size.
pub fn hash_file(path: &Path) -> io::Result<(Sha256, u64)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len <= WHOLE_FILE_LIMIT {
        let mut bytes = Vec::with_capacity(len as usize);
        file.read_to_end(&mut bytes)?;
        return Ok((sha256(&bytes), bytes.len() as u64));
    }
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    let mut size = 0u64;
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hasher.finish(), size))
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: D-3
    #[test]
    fn streamed_and_whole_files_hash_alike() {
        let dir = tempfile::tempdir().unwrap();
        for len in [0usize, 1, 1 << 20, (1 << 20) + 1, 3 * (1 << 20) + 17] {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            let path = dir.path().join(format!("f{len}"));
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(
                hash_file(&path).unwrap(),
                (sha256(&bytes), len as u64),
                "{len}"
            );
        }
    }
}
