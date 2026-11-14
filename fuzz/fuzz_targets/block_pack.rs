//! Arbitrary bytes as a pack or a sealed hint from the shared store (B-28): never a panic, and
//! nothing opens that a writer didn't seal with the key. Since random bytes never pass the cipher,
//! the same bytes are also sealed with the key, as a compressed stream and as the plaintext
//! inside one, so the stream and the record behind the seal are fuzzed too; whatever opens then
//! holds only names inside the block, each file the size and SHA-256 its record says.
#![no_main]

use std::io;

use hushrun_core::blocks::Hex32;
use hushrun_core::blocks::pack::{Expect, Sink, open, open_hint, seal};
use libfuzzer_sys::fuzz_target;

const KEY: [u8; 32] = [0x5a; 32];

#[derive(Default)]
struct Count {
    files: Vec<(String, u64, u64)>,
}

impl Sink for Count {
    fn file(&mut self, _index: usize, name: &str, size: u64) -> io::Result<()> {
        assert!(!name.is_empty() && !name.starts_with('/') && !name.contains(".."));
        self.files.push((name.to_string(), size, 0));
        Ok(())
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let last = self.files.last_mut().expect("a file before its bytes");
        last.2 += bytes.len() as u64;
        assert!(last.2 <= last.1);
        Ok(())
    }
}

fn try_open(pack: &[u8]) {
    let expect = Expect {
        key: KEY,
        step: None,
        pack_sha256: None,
        bytes: None,
        outputs: None,
    };
    let mut sink = Count::default();
    if let Ok(record) = open(&mut &pack[..], &expect, &mut sink) {
        assert_eq!(record.files.len(), sink.files.len());
        for (file, (_, size, written)) in record.files.iter().zip(&sink.files) {
            assert_eq!(file.size, *size);
            assert_eq!(size, written);
        }
    }
}

fn sealed(stream: &[u8]) -> Vec<u8> {
    let mut pack = Vec::new();
    seal(&KEY, &Hex32::sha256(stream), &mut &stream[..], &mut pack).expect("sealed");
    pack
}

fuzz_target!(|data: &[u8]| {
    try_open(data);
    let _ = open_hint(&KEY, data);
    // As a compressed stream behind a good seal.
    try_open(&sealed(data));
    // As the plaintext of the stream: zstd'd here, so the record parser sees these bytes.
    if let Ok(stream) = zstd::stream::encode_all(data, 1) {
        try_open(&sealed(&stream));
    }
});
