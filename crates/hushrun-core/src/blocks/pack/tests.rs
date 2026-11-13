use proptest::prelude::*;
use serde_json::json;

use super::*;

const KEY: [u8; 32] = [0x11; 32];
const OTHER_KEY: [u8; 32] = [0x22; 32];

/// The files written to a sink, by name.
#[derive(Default)]
struct Files {
    files: Vec<(String, u64, Vec<u8>)>,
}

impl Sink for Files {
    fn file(&mut self, _index: usize, name: &str, size: u64) -> io::Result<()> {
        self.files.push((name.to_string(), size, Vec::new()));
        Ok(())
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.files
            .last_mut()
            .expect("a file before its bytes")
            .2
            .extend_from_slice(bytes);
        Ok(())
    }
}

fn record_of(key: &[u8; 32], files: &[(&str, &[u8])]) -> PackRecord {
    PackRecord {
        key: Hex32(*key),
        step: Hex32([0x33; 32]),
        description: json!({
            "schema": "hushrun.build.description/4",
            "class": "library",
            "crate_name": "serde",
            "repository": "github.com/acme/private",
            "built_ms": 1_800_000_000_000u64,
            "rustc_ms": 1234,
        }),
        files: files
            .iter()
            .map(|(name, bytes)| PackFile {
                name: name.to_string(),
                size: bytes.len() as u64,
                sha256: Hex32::sha256(bytes),
            })
            .collect(),
    }
}

fn pack(key: &[u8; 32], files: &[(&str, &[u8])]) -> (PackRecord, Vec<u8>) {
    let record = record_of(key, files);
    let bytes: Vec<&[u8]> = files.iter().map(|(_, b)| *b).collect();
    let packed = pack_in_memory(key, &record, &bytes).unwrap();
    (record, packed)
}

fn expect(record: &PackRecord, packed: &[u8]) -> Expect {
    Expect {
        key: record.key.0,
        step: Some(record.step),
        pack_sha256: Some(Hex32::sha256(packed)),
        bytes: Some(packed.len() as u64),
        outputs: Some(outputs_of(record)),
    }
}

fn open_bytes(packed: &[u8], expect: &Expect) -> Result<(PackRecord, Files), PackError> {
    let mut files = Files::default();
    let record = open(&mut &packed[..], expect, &mut files)?;
    Ok((record, files))
}

const SMALL: &[(&str, &[u8])] = &[
    ("libserde-1.rlib", b"!<arch>\nthe library's bytes"),
    ("libserde-1.rmeta", b"rust metadata"),
];

// covers: B-28
#[test]
fn a_block_opens_to_its_files_and_its_record_with_what_differs_per_machine_left_out() {
    let (record, packed) = pack(&KEY, SMALL);
    assert_eq!(&packed[..4], b"HRBP");
    assert_eq!(&packed[4..7], &[1, 1, 1]);
    let (opened, files) = open_bytes(&packed, &expect(&record, &packed)).unwrap();
    assert_eq!(opened.files, record.files);
    assert_eq!(opened.description, normalize(&record.description));
    for name in ["repository", "built_ms", "rustc_ms"] {
        assert!(opened.description.get(name).is_none(), "{name}");
    }
    assert_eq!(opened.description["crate_name"], "serde");
    let got: Vec<(String, Vec<u8>)> = files
        .files
        .into_iter()
        .map(|(name, size, bytes)| {
            assert_eq!(size, bytes.len() as u64);
            (name, bytes)
        })
        .collect();
    let want: Vec<(String, Vec<u8>)> = SMALL
        .iter()
        .map(|(n, b)| (n.to_string(), b.to_vec()))
        .collect();
    assert_eq!(got, want);
}

// covers: B-27, B-28
#[test]
fn the_same_block_packs_to_the_same_bytes_every_time_and_wherever_it_was_built() {
    let (record, first) = pack(&KEY, SMALL);
    let (_, second) = pack(&KEY, SMALL);
    assert_eq!(first, second);
    // Another machine's description differs in what is left out: the same pack.
    let mut elsewhere = record.clone();
    elsewhere.description["repository"] = json!("github.com/other/repo");
    elsewhere.description["built_ms"] = json!(42);
    elsewhere.description["rustc_ms"] = json!(99);
    let bytes: Vec<&[u8]> = SMALL.iter().map(|(_, b)| *b).collect();
    assert_eq!(pack_in_memory(&KEY, &elsewhere, &bytes).unwrap(), first);
}

// covers: B-27
#[test]
fn two_contents_under_one_key_never_share_a_salt() {
    let (_, a) = pack(&KEY, SMALL);
    let (_, b) = pack(&KEY, &[("libserde-1.rlib", b"other bytes"), SMALL[1]]);
    assert_ne!(a[7..39], b[7..39]);
    // And one content under two keys has two salts too: the salt is keyed.
    let (_, c) = pack(&OTHER_KEY, SMALL);
    assert_ne!(a[7..39], c[7..39]);
}

// covers: B-27, B-28
#[test]
fn one_flipped_bit_anywhere_fails_closed() {
    let (record, packed) = pack(&KEY, SMALL);
    let with_digests = expect(&record, &packed);
    let without = Expect {
        pack_sha256: None,
        bytes: None,
        ..with_digests
    };
    for at in 0..packed.len() {
        for bit in [0x01u8, 0x80] {
            let mut bad = packed.clone();
            bad[at] ^= bit;
            assert!(
                matches!(open_bytes(&bad, &with_digests), Err(PackError::PackSha)),
                "byte {at}"
            );
            let err = open_bytes(&bad, &without)
                .err()
                .unwrap_or_else(|| panic!("byte {at}"));
            assert!(err.unreadable(), "byte {at}: {err}");
        }
    }
}

// covers: B-27, B-28
#[test]
fn a_pack_cut_short_at_any_length_fails() {
    let (record, packed) = pack(&KEY, SMALL);
    let without = Expect {
        pack_sha256: None,
        bytes: None,
        ..expect(&record, &packed)
    };
    for len in 0..packed.len() {
        let err = open_bytes(&packed[..len], &without)
            .err()
            .unwrap_or_else(|| panic!("length {len}"));
        assert!(err.unreadable(), "length {len}: {err}");
        assert!(matches!(
            open_bytes(&packed[..len], &expect(&record, &packed)),
            Err(PackError::PackSize)
        ));
    }
}

// covers: B-28
#[test]
fn data_after_the_last_chunk_fails() {
    let (record, packed) = pack(&KEY, SMALL);
    let without = Expect {
        pack_sha256: None,
        bytes: None,
        ..expect(&record, &packed)
    };
    for extra in [&[0u8][..], &[0, 0, 0, 0], &packed[39..]] {
        let mut longer = packed.clone();
        longer.extend_from_slice(extra);
        assert!(
            matches!(open_bytes(&longer, &without), Err(PackError::Seal(_))),
            "{}",
            extra.len()
        );
    }
}

// covers: B-27, B-28
#[test]
fn a_pack_opens_only_with_its_key_its_first_key_and_its_outputs() {
    let (record, packed) = pack(&KEY, SMALL);
    let right = expect(&record, &packed);
    assert!(matches!(
        open_bytes(
            &packed,
            &Expect {
                key: OTHER_KEY,
                ..right
            }
        ),
        Err(PackError::Seal(_))
    ));
    assert!(matches!(
        open_bytes(
            &packed,
            &Expect {
                step: Some(Hex32([9; 32])),
                ..right
            }
        ),
        Err(PackError::Step)
    ));
    assert!(matches!(
        open_bytes(
            &packed,
            &Expect {
                outputs: Some(Hex32([9; 32])),
                ..right
            }
        ),
        Err(PackError::Outputs)
    ));
    assert!(matches!(
        open_bytes(
            &packed,
            &Expect {
                pack_sha256: Some(Hex32([9; 32])),
                ..right
            }
        ),
        Err(PackError::PackSha)
    ));
    // Sealed with the right key, a record naming another key is still refused.
    let mut lying = record.clone();
    lying.key = Hex32(OTHER_KEY);
    let bytes: Vec<&[u8]> = SMALL.iter().map(|(_, b)| *b).collect();
    let sealed = pack_in_memory(&KEY, &lying, &bytes).unwrap();
    assert!(matches!(
        open_bytes(
            &sealed,
            &Expect {
                pack_sha256: None,
                bytes: None,
                ..right
            }
        ),
        Err(PackError::Key)
    ));
}

// covers: B-28, B-29
#[test]
fn a_file_that_changed_under_the_writer_is_refused() {
    let record = record_of(&KEY, SMALL);
    for files in [
        vec![&b"!<arch>\nthe library's bytez"[..], SMALL[1].1],
        vec![&b"short"[..], SMALL[1].1],
        vec![SMALL[0].1, &b"rust metadata and more"[..]],
    ] {
        assert!(matches!(
            pack_in_memory(&KEY, &record, &files),
            Err(PackError::Changed { .. })
        ));
    }
    // A stream that isn't the one its SHA-256 names is never sealed.
    let mut out = Vec::new();
    assert!(matches!(
        seal(&KEY, &Hex32([0; 32]), &mut &b"a stream"[..], &mut out),
        Err(PackError::Changed { .. })
    ));
}

// covers: B-28
#[test]
fn a_record_naming_a_file_outside_the_block_is_neither_written_nor_read() {
    for name in ["", "/etc/passwd", "a/../b", "a//b", "./a", "a\\b", "a\0b"] {
        let record = record_of(&KEY, &[(name, b"x")]);
        assert!(matches!(
            pack_in_memory(&KEY, &record, &[b"x"]),
            Err(PackError::Record(_))
        ));
    }
    let twice = record_of(&KEY, &[("a", b"x"), ("a", b"x")]);
    assert!(pack_in_memory(&KEY, &twice, &[b"x", b"x"]).is_err());
    let folder = record_of(&KEY, &[("a", b"x"), ("a/b", b"y")]);
    assert!(pack_in_memory(&KEY, &folder, &[b"x", b"y"]).is_err());
    let nested = record_of(&KEY, &[("p.dSYM/Contents/Info.plist", b"x"), ("p", b"y")]);
    assert!(pack_in_memory(&KEY, &nested, &[b"x", b"y"]).is_ok());
}

// covers: B-28
#[test]
fn a_block_of_several_chunks_round_trips() {
    // Bytes that don't compress, so the stream spans three chunks.
    let mut big = Vec::with_capacity(2 * CHUNK + 100);
    let mut state = 0x1234_5678u32;
    while big.len() < 2 * CHUNK + 100 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        big.extend_from_slice(&state.to_le_bytes());
    }
    let files: &[(&str, &[u8])] = &[("big.rlib", &big), ("empty.rmeta", b"")];
    let (record, packed) = pack(&KEY, files);
    assert!(packed.len() > 2 * CHUNK);
    let (_, opened) = open_bytes(&packed, &expect(&record, &packed)).unwrap();
    assert_eq!(opened.files[0].2, big);
    assert!(opened.files[1].2.is_empty());
    // A chunk taken out of the middle fails.
    let first_len = u32::from_be_bytes(packed[39..43].try_into().unwrap()) as usize;
    let mut missing = packed[..39].to_vec();
    missing.extend_from_slice(&packed[39 + 4 + first_len..]);
    let without = Expect {
        pack_sha256: None,
        bytes: None,
        ..expect(&record, &packed)
    };
    assert!(matches!(
        open_bytes(&missing, &without),
        Err(PackError::Seal(_))
    ));
}

// covers: B-28
#[test]
fn a_pack_past_the_largest_is_not_written() {
    let mut out = Vec::new();
    let stream = vec![0u8; 3 * CHUNK];
    let err = seal_with(
        &PACK,
        &KEY,
        &Hex32::sha256(&stream),
        &mut stream.as_slice(),
        &mut out,
        2 * CHUNK as u64,
    );
    assert!(matches!(err, Err(PackError::TooLarge)));
}

// covers: B-27
#[test]
fn a_hint_opens_only_with_its_first_key() {
    let hint = br#"{"files":["a","b"]}"#;
    let sealed = seal_hint(&KEY, hint).unwrap();
    assert_eq!(&sealed[..4], b"HRBH");
    assert_eq!(seal_hint(&KEY, hint).unwrap(), sealed);
    assert_eq!(open_hint(&KEY, &sealed).as_deref(), Some(&hint[..]));
    assert_eq!(open_hint(&OTHER_KEY, &sealed), None);
    // A hint is not a pack, nor a pack a hint, under the same key.
    let (record, packed) = pack(&KEY, SMALL);
    assert_eq!(open_hint(&KEY, &packed), None);
    assert!(open_bytes(&sealed, &expect(&record, &packed)).is_err());
    for at in 0..sealed.len() {
        let mut bad = sealed.clone();
        bad[at] ^= 1;
        assert_eq!(open_hint(&KEY, &bad), None, "byte {at}");
    }
    // Too large to share.
    let mut state = 1u32;
    let noise: Vec<u8> = (0..MOST_HINT_BYTES + 10)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (state >> 16) as u8
        })
        .collect();
    assert!(matches!(seal_hint(&KEY, &noise), Err(PackError::TooLarge)));
}

// covers: B-28
#[test]
fn what_a_reader_reports_as_unreadable() {
    assert!(PackError::Seal("x").unreadable());
    assert!(PackError::Outputs.unreadable());
    assert!(!PackError::Changed { name: "a".into() }.unreadable());
    assert!(!PackError::Io(io::Error::other("disk")).unreadable());
    assert_eq!(PackError::PackSha.word(), "pack-sha256");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn any_block_round_trips(
        files in proptest::collection::btree_map("[a-z]{1,8}", proptest::collection::vec(any::<u8>(), 0..2000), 0..6),
        key in any::<[u8; 32]>(),
    ) {
        let files: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
        let (record, packed) = pack(&key, &files);
        let (opened, sink) = open_bytes(&packed, &expect(&record, &packed)).unwrap();
        prop_assert_eq!(opened.files, record.files);
        prop_assert_eq!(sink.files.len(), files.len());
    }

    #[test]
    fn arbitrary_bytes_never_open(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
        let expect = Expect { key: KEY, step: None, pack_sha256: None, bytes: None, outputs: None };
        prop_assert!(open_bytes(&bytes, &expect).is_err());
        prop_assert!(open_hint(&KEY, &bytes).is_none());
    }
}

// covers: B-28
#[test]
fn a_pack_that_would_unpack_past_its_room_is_refused_before_any_file_is_written() {
    // 16 MiB of zeros packs to a few kilobytes: far more than 64 times the pack.
    let zeros = vec![0u8; 16 << 20];
    let (record, packed) = pack(&KEY, &[("libbig.rlib", &zeros)]);
    assert!((packed.len() as u64) * MOST_EXPANSION < zeros.len() as u64);
    let expect = expect(&record, &packed);
    let mut files = Files::default();
    let refused = open_within(
        &mut &packed[..],
        &expect,
        Some(Room::for_pack(packed.len() as u64, u64::MAX)),
        &mut files,
    );
    assert!(matches!(refused, Err(PackError::NoRoom(_))), "{refused:?}");
    assert!(files.files.is_empty(), "a file was written");
    assert!(!refused.unwrap_err().unreadable());
    // Within 64 times, but more than the disk can take above its floor: refused the same way.
    let (record, packed) = pack(&KEY, SMALL);
    let expect = self::expect(&record, &packed);
    let mut files = Files::default();
    let refused = open_within(
        &mut &packed[..],
        &expect,
        Some(Room {
            most: u64::MAX,
            free: 10,
        }),
        &mut files,
    );
    assert!(matches!(refused, Err(PackError::NoRoom(_))), "{refused:?}");
    assert!(files.files.is_empty());
    // With room, it opens.
    let mut files = Files::default();
    open_within(
        &mut &packed[..],
        &expect,
        Some(Room::for_pack(packed.len() as u64, u64::MAX)),
        &mut files,
    )
    .unwrap();
    assert_eq!(files.files.len(), 2);
}
