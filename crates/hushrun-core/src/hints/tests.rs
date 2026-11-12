use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use proptest::prelude::*;
use serde_json::Value;

use super::*;
use crate::units::shape;

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| p.to_string()).collect()
}

fn unit(reads: &[&str], dirs: &[&str]) -> Unit {
    Unit {
        reads: set(reads),
        dirs: set(dirs),
    }
}

/// A pytest check of two test files and a Go-like unit named by its directory.
fn sample() -> (Hint, Vec<&'static str>) {
    let scope = vec![
        "shop/__init__.py",
        "shop/cart.py",
        "shop/pricing.py",
        "tests/test_cart.py",
        "tests/test_pricing.py",
        "tests/data/prices.json",
        "uv.lock",
    ];
    let mut units = BTreeMap::new();
    units.insert(
        "tests/test_cart.py".to_string(),
        unit(
            &[
                "shop/__init__.py",
                "shop/cart.py",
                "shop/pricing.py",
                "tests/test_cart.py",
            ],
            &[],
        ),
    );
    units.insert(
        "tests/test_pricing.py".to_string(),
        unit(
            &[
                "shop/__init__.py",
                "shop/pricing.py",
                "tests/test_pricing.py",
            ],
            &["tests/data"],
        ),
    );
    let hint = Hint {
        tests: BTreeMap::new(),
        adapter: "pytest".to_string(),
        units,
        unit_files: BTreeSet::new(),
    };
    (hint, scope)
}

fn sources() -> Vec<&'static str> {
    vec!["shop/__init__.py", "shop/cart.py", "shop/pricing.py"]
}

/// RFC 4231's test cases for HMAC-SHA-256, but case 5, whose output is cut to 128 bits.
#[test]
fn hmac_matches_rfc_4231() {
    let cases: [(Vec<u8>, Vec<u8>, &str); 6] = [
        (
            vec![0x0b; 20],
            b"Hi There".to_vec(),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        ),
        (
            b"Jefe".to_vec(),
            b"what do ya want for nothing?".to_vec(),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
        ),
        (
            vec![0xaa; 20],
            vec![0xdd; 50],
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
        ),
        (
            hex("0102030405060708090a0b0c0d0e0f10111213141516171819"),
            vec![0xcd; 50],
            "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
        ),
        (
            vec![0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First".to_vec(),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        ),
        (
            vec![0xaa; 131],
            b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.".to_vec(),
            "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
        ),
    ];
    for (key, data, mac) in cases {
        assert_eq!(to_hex(&hmac_sha256(&key, &data)), mac);
    }
    // Case 5, cut to 128 bits.
    let mac = hmac_sha256(&[0x0c; 20], b"Test With Truncation");
    assert_eq!(to_hex(&mac[..16]), "a3b6167473100ee06e0c796c2955552b");
}

// covers: C-38
#[test]
fn the_os_word_is_one_of_three_or_none() {
    let word = Os::this_word();
    assert!(["linux", "macos", "windows", "none"].contains(&word));
    if cfg!(target_os = "macos") {
        assert_eq!(Os::this(), Some(Os::Macos));
    }
    for os in [Os::Linux, Os::Macos, Os::Windows] {
        assert_eq!(Os::from_word(os.word()), Some(os));
    }
    assert_eq!(Os::from_word("none"), None);
    assert_eq!(Os::from_word("Linux"), None);
}

// covers: C-38
#[test]
fn the_key_comes_from_the_sources_paths_not_from_the_shape() {
    let key = Key::derive(sources());
    // The same set, in another order and with a repeat, is the same key.
    let again = Key::derive([
        "shop/pricing.py",
        "shop/cart.py",
        "shop/__init__.py",
        "shop/cart.py",
    ]);
    assert_eq!(key.as_bytes(), again.as_bytes());
    // One path more is another key.
    let other = Key::derive([
        "shop/__init__.py",
        "shop/cart.py",
        "shop/pricing.py",
        "shop/tax.py",
    ]);
    assert_ne!(key.as_bytes(), other.as_bytes());
    // Not the shape digest, nor an HMAC of it.
    let shape = shape(sources());
    assert_ne!(key.as_bytes(), shape.as_bytes());
    assert_ne!(
        key.as_bytes(),
        &hmac_sha256(KEY_DOMAIN.as_bytes(), shape.as_bytes())
    );
    assert_eq!(format!("{key:?}"), "Key(..)");
}

// covers: C-38
#[test]
fn writing_then_reading_gives_the_hint_back() {
    let (hint, scope) = sample();
    let key = Key::derive(sources());
    let written = write(&hint, &key).unwrap();
    assert_eq!(write(&hint, &key).unwrap(), written, "deterministic");
    let read = read(&written.bytes, &key, scope.iter().copied()).unwrap();
    assert_eq!(read.hint, hint);
    assert_eq!(read.content_id, written.content_id);
    assert_eq!((read.units, read.dropped), (2, 0));
    // The content id is the SHA-256 of the content, before compression.
    assert_eq!(
        written.content_id,
        Digest::of(&content(&hint, &key).unwrap())
    );
    // No path is in the bytes or the content.
    let content = content(&hint, &key).unwrap();
    for path in &scope {
        let needle = path.as_bytes();
        assert!(
            !content.windows(needle.len()).any(|w| w == needle),
            "{path}"
        );
    }
}

// covers: C-38
#[test]
fn another_key_reads_nothing() {
    let (hint, scope) = sample();
    let written = write(&hint, &Key::derive(sources())).unwrap();
    let read = read(&written.bytes, &Key::derive(["shop/cart.py"]), scope).unwrap();
    assert!(read.hint.units.is_empty());
    assert_eq!((read.units, read.dropped), (2, 2));
}

// covers: C-38
#[test]
fn a_file_gone_drops_the_test_files_that_read_it() {
    let (hint, scope) = sample();
    let key = Key::derive(sources());
    let written = write(&hint, &key).unwrap();
    // shop/cart.py is gone: only test_cart.py read it.
    let without: Vec<&str> = scope
        .iter()
        .copied()
        .filter(|p| *p != "shop/cart.py")
        .collect();
    let read = read(&written.bytes, &key, without).unwrap();
    assert_eq!(
        read.hint.units.keys().collect::<Vec<_>>(),
        ["tests/test_pricing.py"]
    );
    assert_eq!(read.dropped, 1);
    // tests/data is gone with its only file: test_pricing.py listed it.
    let without: Vec<&str> = scope
        .iter()
        .copied()
        .filter(|p| *p != "tests/data/prices.json")
        .collect();
    let read = super::read(&written.bytes, &key, without).unwrap();
    assert_eq!(
        read.hint.units.keys().collect::<Vec<_>>(),
        ["tests/test_cart.py"]
    );
}

// covers: C-38
#[test]
fn a_directory_names_a_go_package_and_the_root_is_dot_or_empty() {
    let key = Key::derive(["go.mod", "cart/cart.go"]);
    let mut units = BTreeMap::new();
    units.insert(
        "cart".to_string(),
        unit(
            &["cart/cart.go", "cart/cart_test.go"],
            &["cart/testdata", ""],
        ),
    );
    units.insert(".".to_string(), unit(&["main_test.go"], &["."]));
    let hint = Hint {
        tests: BTreeMap::new(),
        adapter: "go".to_string(),
        units,
        unit_files: BTreeSet::new(),
    };
    let scope = [
        "go.mod",
        "main_test.go",
        "cart/cart.go",
        "cart/cart_test.go",
        "cart/testdata/a.txt",
    ];
    let written = write(&hint, &key).unwrap();
    assert_eq!(read(&written.bytes, &key, scope).unwrap().hint, hint);
    // A directory read as a file, or a file listed as a directory, matches nothing.
    let mut wrong = hint.clone();
    wrong
        .units
        .get_mut(".")
        .unwrap()
        .reads
        .insert("cart".to_string());
    let written = write(&wrong, &key).unwrap();
    let read = read(&written.bytes, &key, scope).unwrap();
    assert_eq!(read.hint.units.keys().collect::<Vec<_>>(), ["cart"]);
}

// covers: C-38
#[test]
fn a_unit_file_gone_leaves_the_list_and_drops_no_unit() {
    let key = Key::derive(["src/a.ts"]);
    let mut units = BTreeMap::new();
    units.insert(
        "src/a.test.ts".to_string(),
        unit(&["src/a.ts", "src/a.test.ts"], &[]),
    );
    let hint = Hint {
        tests: BTreeMap::new(),
        adapter: "vitest".to_string(),
        units,
        unit_files: set(&["src/a.test.ts", "src/b.test.ts"]),
    };
    let written = write(&hint, &key).unwrap();
    let read = read(&written.bytes, &key, ["src/a.ts", "src/a.test.ts"]).unwrap();
    assert_eq!(read.hint.unit_files, set(&["src/a.test.ts"]));
    assert_eq!(read.hint.units, hint.units);
}

/// A token of one byte, so that paths collide on purpose.
fn weak(path: &str) -> Token {
    let mut token = [0u8; 16];
    token[0] = path.bytes().fold(0u8, u8::wrapping_add);
    Token(token)
}

// covers: C-38
#[test]
fn two_paths_of_the_scope_with_one_token_drop_every_test_file_that_uses_it() {
    // "ab" and "ba" have one weak token.
    let mut units = BTreeMap::new();
    units.insert("t1".to_string(), unit(&["t1", "ab"], &[]));
    units.insert("t2".to_string(), unit(&["t2", "x"], &[]));
    let hint = Hint {
        tests: BTreeMap::new(),
        adapter: "pytest".to_string(),
        units,
        unit_files: set(&["ab", "x"]),
    };
    let content = encode(&hint, &weak, &|_, _| None).unwrap();
    // Without "ba" in the scope, every unit reads.
    let read = decode(&content, &weak, &|_, _| None, ["t1", "t2", "ab", "x"]).unwrap();
    assert_eq!(read.hint, hint);
    // With it, t1 goes, and so does "ab" from the unit files.
    let read = decode(&content, &weak, &|_, _| None, ["t1", "t2", "ab", "ba", "x"]).unwrap();
    assert_eq!(read.hint.units.keys().collect::<Vec<_>>(), ["t2"]);
    assert_eq!(read.hint.unit_files, set(&["x"]));
    // A path given twice is one path.
    let read = decode(&content, &weak, &|_, _| None, ["t1", "t2", "ab", "ab", "x"]).unwrap();
    assert_eq!(read.hint, hint);
}

// covers: C-38
#[test]
fn of_two_test_files_with_one_token_the_writer_keeps_the_first_and_its_paths_only() {
    // "ab" and "ba" have one weak token; "q" is read by "ba" alone.
    let mut units = BTreeMap::new();
    units.insert("ab".to_string(), unit(&["ab"], &[]));
    units.insert("ba".to_string(), unit(&["ba", "q"], &[]));
    let hint = Hint {
        tests: BTreeMap::new(),
        adapter: "pytest".to_string(),
        units,
        unit_files: BTreeSet::new(),
    };
    let content = encode(&hint, &weak, &|_, _| None).unwrap();
    // The table holds no token for "q", so the content is canonical and reads.
    let read = decode(&content, &weak, &|_, _| None, ["ab", "q"]).unwrap();
    assert_eq!(read.hint.units.keys().collect::<Vec<_>>(), ["ab"]);
}

// covers: C-38
#[test]
fn the_writer_says_too_large_past_a_bound() {
    let key = Key::derive(sources());
    let mut hint = Hint {
        tests: BTreeMap::new(),
        adapter: "pytest".to_string(),
        ..Hint::default()
    };
    for n in 0..=MOST_UNITS {
        hint.units
            .insert(format!("tests/test_{n}.py"), Unit::default());
    }
    assert!(matches!(
        write(&hint, &key),
        Err(Unwritable::TooLarge {
            what: "test files",
            size: 5_001,
            most: 5_000
        })
    ));
    // Paths that compress poorly: 5,000 units each reading 30 files of their own is past 2 MiB.
    let mut hint = Hint {
        tests: BTreeMap::new(),
        adapter: "pytest".to_string(),
        ..Hint::default()
    };
    for n in 0..MOST_UNITS {
        let reads = (0..30).map(|m| format!("src/{n}/{m}.py")).collect();
        hint.units.insert(
            format!("tests/test_{n}.py"),
            Unit {
                reads,
                dirs: BTreeSet::new(),
            },
        );
    }
    match write(&hint, &key) {
        Err(Unwritable::TooLarge { what, size, most }) => {
            assert_eq!(what, "bytes compressed");
            assert_eq!(most, MOST_BYTES);
            assert!(size > MOST_BYTES);
        }
        other => panic!("{other:?}"),
    }
    for word in ["", "PyTest", "py test", &"a".repeat(33)] {
        let hint = Hint {
            tests: BTreeMap::new(),
            adapter: word.to_string(),
            ..Hint::default()
        };
        assert_eq!(
            write(&hint, &key),
            Err(Unwritable::Adapter(word.to_string()))
        );
    }
}

fn zlib(content: &[u8]) -> Vec<u8> {
    let mut deflate = Deflate::new(1, true, WINDOW_BITS);
    let mut out = vec![0u8; content.len() / 100 + 1024];
    let status = deflate
        .compress(content, &mut out, DeflateFlush::Finish)
        .unwrap();
    assert_eq!(status, Status::StreamEnd);
    out.truncate(usize::try_from(deflate.total_out()).unwrap());
    out
}

// covers: C-38
#[test]
fn the_reader_refuses_what_isnt_one_hint() {
    let (hint, scope) = sample();
    let key = Key::derive(sources());
    let written = write(&hint, &key).unwrap();
    let read = |bytes: &[u8]| super::read(bytes, &key, scope.iter().copied());

    assert_eq!(read(&[]), Err(Unread::Empty));
    assert_eq!(
        read(&vec![0; MOST_BYTES + 1]),
        Err(Unread::TooLarge(MOST_BYTES + 1))
    );
    assert!(matches!(read(b"not zlib"), Err(Unread::Compression(_))));
    let mut trailing = written.bytes.clone();
    trailing.push(0);
    assert_eq!(read(&trailing), Err(Unread::Trailing));
    let truncated = &written.bytes[..written.bytes.len() - 1];
    assert_eq!(read(truncated), Err(Unread::Truncated));
    // A bomb: 17 MiB of zeros in a few KiB stops at 16 MiB.
    let bomb = zlib(&vec![0; MOST_CONTENT + 1024 * 1024]);
    assert!(bomb.len() < MOST_BYTES);
    assert_eq!(read(&bomb), Err(Unread::Inflates));

    let good = content(&hint, &key).unwrap();
    let mut other = good.clone();
    other[SCHEMA.len() - 1] = b'7';
    assert_eq!(read(&zlib(&other)), Err(Unread::Schema));
    assert_eq!(read(&zlib(b"hushrun.unit-hints/v0")), Err(Unread::Schema));
    // Every cut of the content short of its end is damaged, never a panic.
    for end in SCHEMA.len() + 1..good.len() {
        assert!(
            matches!(read(&zlib(&good[..end])), Err(Unread::Damaged(_))),
            "{end}"
        );
    }
    // Bytes after its end.
    let mut longer = good.clone();
    longer.push(0);
    assert_eq!(
        read(&zlib(&longer)),
        Err(Unread::Damaged("bytes after its end"))
    );
}

/// [`sample`] with tests: two of `tests/test_cart.py`'s with lists of their own, one that read
/// its test file's, and a test of `tests/test_pricing.py`'s.
fn sample_with_tests() -> (Hint, Vec<&'static str>) {
    let (mut hint, scope) = sample();
    let mut cart = BTreeMap::new();
    cart.insert(
        "test_total".to_string(),
        Some(unit(&["shop/cart.py", "tests/test_cart.py"], &[])),
    );
    cart.insert(
        "test_discount[10%]".to_string(),
        Some(unit(
            &["shop/pricing.py", "tests/test_cart.py"],
            &["tests/data"],
        )),
    );
    cart.insert("test_empty".to_string(), None);
    hint.tests.insert("tests/test_cart.py".to_string(), cart);
    let mut pricing = BTreeMap::new();
    pricing.insert(
        "TestPrice::test_rounds".to_string(),
        Some(unit(&["shop/cart.py", "tests/test_cart.py"], &[])),
    );
    hint.tests
        .insert("tests/test_pricing.py".to_string(), pricing);
    (hint, scope)
}

// covers: C-41
#[test]
fn a_hint_with_tests_reads_back_and_seals_their_ids() {
    let (hint, scope) = sample_with_tests();
    let key = Key::derive(sources());
    let written = write(&hint, &key).unwrap();
    // The same hint seals to the same bytes: its content id stays one.
    assert_eq!(write(&hint, &key).unwrap(), written, "deterministic");
    let content = content(&hint, &key).unwrap();
    assert!(content.starts_with(format!("{SCHEMA_V1}\n").as_bytes()));
    assert_eq!(written.content_id, Digest::of(&content));
    let read = read(&written.bytes, &key, scope.iter().copied()).unwrap();
    assert_eq!(read.hint, hint);
    // No path, and no test's id, is in the content.
    for needle in
        scope
            .iter()
            .copied()
            .chain(["test_total", "test_discount", "test_empty", "TestPrice"])
    {
        let needle = needle.as_bytes();
        assert!(
            !content.windows(needle.len()).any(|w| w == needle),
            "{}",
            String::from_utf8_lossy(needle)
        );
    }
    assert_ne!(key.seal_bytes(), key.as_bytes());
    // Without tests it is a hint of version 0, as it was; with them, the same units follow v1.
    let (plain, _) = sample();
    let v0 = super::content(&plain, &key).unwrap();
    assert!(v0.starts_with(format!("{SCHEMA}\n").as_bytes()));
    assert!(content[SCHEMA.len()..].starts_with(&v0[SCHEMA.len()..]));
}

// covers: C-41
#[test]
fn a_seal_that_doesnt_open_reads_as_no_tests_never_an_error() {
    let (hint, scope) = sample_with_tests();
    let key = Key::derive(sources());
    let good = content(&hint, &key).unwrap();
    let units_only = Hint {
        tests: BTreeMap::new(),
        ..hint.clone()
    };
    // Every byte of the seal flipped: the units stand, the tests are gone.
    let seal_at = (0..good.len())
        .rev()
        .find(|&at| {
            at + 4 <= good.len()
                && usize::try_from(u32::from_be_bytes(good[at..at + 4].try_into().unwrap()))
                    .unwrap()
                    == good.len() - at - 4
        })
        .unwrap();
    for at in seal_at + 4..good.len() {
        let mut flipped = good.clone();
        flipped[at] ^= 0x01;
        let read = read_content(&flipped, &key, scope.iter().copied()).unwrap();
        assert_eq!(read.hint, units_only, "{at}");
    }
    // A byte of the clear part the seal covers, flipped where the content still reads: no tests.
    let mut adapter = good.clone();
    adapter[SCHEMA_V1.len() + 2] = b'q';
    let changed = read_content(&adapter, &key, scope.iter().copied()).unwrap();
    assert!(changed.hint.tests.is_empty());
    // Another key reads nothing.
    let bytes = write(&hint, &key).unwrap().bytes;
    let other = read(&bytes, &Key::derive(["x"]), scope.iter().copied()).unwrap();
    assert!(other.hint.units.is_empty() && other.hint.tests.is_empty());
}

// covers: C-41
#[test]
fn a_test_whose_list_names_a_file_gone_is_dropped_and_the_others_stay() {
    let (hint, mut scope) = sample_with_tests();
    let key = Key::derive(sources());
    let written = write(&hint, &key).unwrap();
    // tests/data is gone: test_discount's list and its test file's unit name it.
    scope.retain(|path| *path != "tests/data/prices.json");
    let read = read(&written.bytes, &key, scope.iter().copied()).unwrap();
    assert_eq!(
        read.hint.units.keys().collect::<Vec<_>>(),
        ["tests/test_cart.py"]
    );
    let cart = &read.hint.tests["tests/test_cart.py"];
    assert_eq!(
        cart.keys().collect::<Vec<_>>(),
        ["test_empty", "test_total"]
    );
    assert!(!read.hint.tests.contains_key("tests/test_pricing.py"));
}

// covers: C-41
#[test]
fn a_hint_too_large_with_tests_leaves_out_lists_first_then_tests() {
    let key = Key::derive(sources());
    let (small, _) = sample_with_tests();
    let (written, trimmed) = write_fitting(&small, &key).unwrap();
    assert_eq!(trimmed, Trimmed::Nothing);
    assert_eq!(written, write(&small, &key).unwrap());

    // 40,000 tests, each with a list of its own of 4 files that compress poorly: too large with
    // lists, fits with ids alone.
    let mut hint = Hint {
        adapter: "pytest".to_string(),
        ..Hint::default()
    };
    hint.units.insert(
        "tests/test_a.py".to_string(),
        unit(&["tests/test_a.py"], &[]),
    );
    let mut tests = BTreeMap::new();
    for n in 0..40_000 {
        tests.insert(
            format!("t{n}"),
            Some(unit(
                &[
                    "tests/test_a.py",
                    &format!("src/{n}/a.py"),
                    &format!("src/{n}/b.py"),
                    &format!("src/{n}/c.py"),
                ],
                &[],
            )),
        );
    }
    hint.tests.insert("tests/test_a.py".to_string(), tests);
    assert!(matches!(
        write(&hint, &key),
        Err(Unwritable::TooLarge { .. })
    ));
    let (written, trimmed) = write_fitting(&hint, &key).unwrap();
    assert_eq!(trimmed, Trimmed::Lists);
    let scope = ["tests/test_a.py"];
    let lists_left = read(&written.bytes, &key, scope).unwrap();
    assert_eq!(lists_left.hint.tests["tests/test_a.py"].len(), 40_000);
    assert!(
        lists_left.hint.tests["tests/test_a.py"]
            .values()
            .all(Option::is_none)
    );

    // Ids that alone are too large: the tests are left out.
    let mut hint = Hint {
        adapter: "pytest".to_string(),
        ..Hint::default()
    };
    hint.units.insert(
        "tests/test_a.py".to_string(),
        unit(&["tests/test_a.py"], &[]),
    );
    let mut tests = BTreeMap::new();
    for n in 0..MOST_TESTS {
        tests.insert(
            format!("{n}-{}", Digest::of(n.to_string().as_bytes())),
            None,
        );
    }
    hint.tests.insert("tests/test_a.py".to_string(), tests);
    let (written, trimmed) = write_fitting(&hint, &key).unwrap();
    assert_eq!(trimmed, Trimmed::Tests);
    let read = read(&written.bytes, &key, scope).unwrap();
    assert!(read.hint.tests.is_empty());
    assert_eq!(read.hint.units.len(), 1);

    // Past the most tests is too large, as is an id past 4,096 bytes.
    let mut hint = sample_with_tests().0;
    hint.tests
        .get_mut("tests/test_cart.py")
        .unwrap()
        .insert("x".repeat(MOST_ID + 1), None);
    assert!(matches!(
        write(&hint, &key),
        Err(Unwritable::TooLarge {
            what: "bytes of a test's id",
            ..
        })
    ));
}

/// A content written by hand: two tokens, then the lists given.
fn by_hand(tokens: &[[u8; 16]], unit_files: &[u32], units: &[(u32, &[u32], &[u32])]) -> Vec<u8> {
    let mut out = format!("{SCHEMA}\n").into_bytes();
    out.push(6);
    out.extend_from_slice(b"pytest");
    out.extend_from_slice(&u32::try_from(tokens.len()).unwrap().to_be_bytes());
    for token in tokens {
        out.extend_from_slice(token);
    }
    let list = |out: &mut Vec<u8>, items: &[u32]| {
        out.extend_from_slice(&u32::try_from(items.len()).unwrap().to_be_bytes());
        for item in items {
            out.extend_from_slice(&item.to_be_bytes());
        }
    };
    list(&mut out, unit_files);
    out.extend_from_slice(&u32::try_from(units.len()).unwrap().to_be_bytes());
    for (own, reads, dirs) in units {
        out.extend_from_slice(&own.to_be_bytes());
        list(&mut out, reads);
        list(&mut out, dirs);
    }
    out
}

// covers: C-38
#[test]
fn the_reader_takes_only_the_canonical_content() {
    let key = Key::derive(["a"]);
    let (a, b) = ([1u8; 16], [2u8; 16]);
    let read = |content: &[u8]| read_content(content, &key, ["a", "b"]);
    assert!(read(&by_hand(&[a, b], &[], &[(0, &[1], &[])])).is_ok());
    let damaged = |content: Vec<u8>, why: &'static str| {
        assert_eq!(read(&content), Err(Unread::Damaged(why)), "{why}");
    };
    damaged(
        by_hand(&[b, a], &[], &[(0, &[1], &[])]),
        "tokens out of order",
    );
    damaged(
        by_hand(&[a, a], &[], &[(0, &[1], &[])]),
        "tokens out of order",
    );
    damaged(
        by_hand(&[a, b], &[], &[(0, &[], &[])]),
        "a token no index names",
    );
    damaged(
        by_hand(&[a, b], &[], &[(0, &[2], &[])]),
        "an index past the tokens",
    );
    damaged(
        by_hand(&[a, b], &[], &[(0, &[1, 1], &[])]),
        "indices out of order",
    );
    damaged(by_hand(&[a, b], &[1, 0], &[]), "indices out of order");
    damaged(
        by_hand(&[a, b], &[], &[(1, &[], &[]), (0, &[], &[])]),
        "units out of order",
    );
    damaged(
        by_hand(&[a, b], &[], &[(0, &[], &[]), (0, &[1], &[])]),
        "units out of order",
    );
    // A count past what is left, which would allocate, is refused first.
    let mut huge = by_hand(&[a, b], &[], &[]);
    let at = SCHEMA.len() + 1 + 1 + 6;
    huge[at..at + 4].copy_from_slice(&900_000u32.to_be_bytes());
    damaged(huge, "truncated");
    let mut huge = by_hand(&[a, b], &[], &[]);
    huge[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    damaged(huge, "too many");
}

/// The vectors `tools/refdigest.py unit-hints` checks independently.
// covers: C-38
#[test]
fn hints_match_the_golden_vectors() {
    let doc: Value =
        serde_json::from_str(include_str!("../../../../testdata/digest/unit-hints.json")).unwrap();
    let strings = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let base64 = base64::engine::general_purpose::STANDARD;
    let vectors = doc["vectors"].as_array().unwrap();
    assert!(vectors.len() >= 4);
    for vector in vectors {
        let name = vector["name"].as_str().unwrap();
        let expect = &vector["expect"];
        let sources = strings(&vector["sources"]);
        let scope = strings(&vector["scope"]);
        let key = Key::derive(sources.iter().map(String::as_str));
        assert_eq!(to_hex(key.as_bytes()), expect["key"], "{name}: key");
        if let Some(seal) = expect.get("sealKey") {
            assert_eq!(&to_hex(key.seal_bytes()), seal, "{name}: seal key");
        }
        let shape = shape(sources.iter().map(String::as_str));
        assert_eq!(shape.to_string(), expect["shape"], "{name}: shape");
        let check_id = Digest::parse(vector["checkId"].as_str().unwrap()).unwrap();
        assert_eq!(
            address(&check_id, &shape).to_string(),
            expect["address"],
            "{name}: address"
        );
        for (path, token) in expect["tokens"].as_object().unwrap() {
            assert_eq!(&key.token(path).hex(), token, "{name}: token of {path}");
        }
        let hint = hint_of(&vector["hint"]);
        let content = content(&hint, &key).unwrap();
        assert_eq!(to_hex(&content), expect["content"], "{name}: content");
        assert_eq!(
            content_id(&content).to_string(),
            expect["contentId"],
            "{name}"
        );
        // The writer's bytes inflate to the content; the vector's, written by another zlib, read.
        assert_eq!(
            inflate(&write(&hint, &key).unwrap().bytes).unwrap(),
            content
        );
        let bytes = base64.decode(vector["bytes"].as_str().unwrap()).unwrap();
        let read = read(&bytes, &key, scope.iter().map(String::as_str)).unwrap();
        assert_eq!(read.hint, hint_of(&expect["read"]), "{name}: read");
        assert_eq!(read.content_id.to_string(), expect["contentId"], "{name}");
        assert_eq!(
            read.dropped,
            usize::try_from(expect["dropped"].as_u64().unwrap()).unwrap()
        );
    }
    for vector in doc["unread"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap();
        let sources = strings(&vector["sources"]);
        let scope = strings(&vector["scope"]);
        let key = Key::derive(sources.iter().map(String::as_str));
        let bytes = base64.decode(vector["bytes"].as_str().unwrap()).unwrap();
        let unread = read(&bytes, &key, scope.iter().map(String::as_str));
        assert!(unread.is_err(), "{name}: {unread:?}");
    }
    // [C-41] Bytes whose test files read, and whose tests don't.
    let no_tests = doc["noTests"].as_array().unwrap();
    assert!(no_tests.len() >= 3);
    for vector in no_tests {
        let name = vector["name"].as_str().unwrap();
        let sources = strings(&vector["sources"]);
        let scope = strings(&vector["scope"]);
        let key = Key::derive(sources.iter().map(String::as_str));
        let bytes = base64.decode(vector["bytes"].as_str().unwrap()).unwrap();
        let read = read(&bytes, &key, scope.iter().map(String::as_str)).unwrap();
        assert!(read.hint.tests.is_empty(), "{name}");
        assert_eq!(read.hint, hint_of(&vector["expect"]["read"]), "{name}");
    }
}

fn hint_of(value: &Value) -> Hint {
    let strings = |value: &Value| -> BTreeSet<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let tests = value.get("tests").map_or_else(BTreeMap::new, |tests| {
        tests
            .as_object()
            .unwrap()
            .iter()
            .map(|(path, tests)| {
                let tests = tests
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(id, list)| {
                        let list = (!list.is_null()).then(|| Unit {
                            reads: strings(&list["reads"]),
                            dirs: strings(&list["dirs"]),
                        });
                        (id.clone(), list)
                    })
                    .collect();
                (path.clone(), tests)
            })
            .collect()
    });
    Hint {
        tests,
        adapter: value["adapter"].as_str().unwrap().to_string(),
        units: value["units"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(path, unit)| {
                (
                    path.clone(),
                    Unit {
                        reads: strings(&unit["reads"]),
                        dirs: strings(&unit["dirs"]),
                    },
                )
            })
            .collect(),
        unit_files: strings(&value["unitFiles"]),
    }
}

/// Directories from a small alphabet, so that hints share them: `""` and `"."` are the root.
fn dir_path() -> impl Strategy<Value = String> {
    prop_oneof![
        1 => Just(".".to_string()),
        6 => proptest::collection::vec(prop_oneof!["a", "b", "dé"], 0..3).prop_map(|parts| parts.join("/")),
    ]
}

/// Files below those directories, whose names no directory has.
fn file_path() -> impl Strategy<Value = String> {
    (
        proptest::collection::vec(prop_oneof!["a", "b", "dé"], 0..3),
        prop::sample::select(vec!["x.py", "y.ts", "é.go"]),
    )
        .prop_map(|(mut parts, leaf)| {
            parts.push(leaf.to_string());
            parts.join("/")
        })
}

fn unit_of_paths() -> impl Strategy<Value = Unit> {
    (
        proptest::collection::btree_set(file_path(), 0..6),
        proptest::collection::btree_set(dir_path(), 0..3),
    )
        .prop_map(|(reads, dirs)| Unit { reads, dirs })
}

/// A unit's tests: a few ids, each with a list of its own or its test file's.
fn tests_of_unit() -> impl Strategy<Value = BTreeMap<String, Option<Unit>>> {
    let id = prop_oneof!["t", "test_a", "test_b[1]", "Tést::x", "a b"];
    proptest::collection::btree_map(id, proptest::option::of(unit_of_paths()), 1..4)
}

fn hint() -> impl Strategy<Value = Hint> {
    (
        prop_oneof!["pytest", "go", "vitest"],
        proptest::collection::btree_map(
            prop_oneof![file_path(), dir_path()],
            unit_of_paths(),
            0..8,
        ),
        proptest::collection::btree_set(file_path(), 0..4),
    )
        .prop_flat_map(move |(adapter, units, unit_files)| {
            let owns: Vec<String> = units.keys().cloned().collect();
            let tests =
                proptest::collection::vec(proptest::option::of(tests_of_unit()), owns.len());
            (Just((adapter, units, unit_files)), tests).prop_map(
                move |((adapter, units, unit_files), tests)| Hint {
                    tests: owns
                        .iter()
                        .cloned()
                        .zip(tests)
                        .filter_map(
                            |(own, tests): (String, Option<BTreeMap<String, Option<Unit>>>)| {
                                tests.map(|tests| (own, tests))
                            },
                        )
                        .collect(),
                    adapter,
                    units,
                    unit_files,
                },
            )
        })
}

/// Every directory that holds one of `files`, and the root's two names.
fn dirs_of(files: &BTreeSet<String>) -> BTreeSet<String> {
    let mut dirs = set(&["", "."]);
    for file in files {
        for (at, byte) in file.bytes().enumerate() {
            if byte == b'/' {
                dirs.insert(file[..at].to_string());
            }
        }
    }
    dirs
}

/// Whether `path` is a directory of the generators': no file has its name.
fn is_dir(path: &str) -> bool {
    !(path.ends_with(".py") || path.ends_with(".ts") || path.ends_with(".go"))
}

/// The files a scope needs for every path `hint` names to be in it: its files, and one file
/// below each directory.
fn files_of(hint: &Hint) -> BTreeSet<String> {
    let below = |dir: &str| match dir {
        "" | "." => "z.py".to_string(),
        dir => format!("{dir}/z.py"),
    };
    let mut files: BTreeSet<String> = hint.unit_files.clone();
    for (own, unit) in &hint.units {
        files.extend(unit.reads.iter().cloned());
        files.extend(unit.dirs.iter().map(|d| below(d)));
        files.insert(if is_dir(own) { below(own) } else { own.clone() });
    }
    for list in hint.tests.values().flat_map(BTreeMap::values).flatten() {
        files.extend(list.reads.iter().cloned());
        files.extend(list.dirs.iter().map(|d| below(d)));
    }
    files
}

/// Whether everything `hint` names is a file of `files` or a directory holding one.
fn within(hint: &Hint, files: &BTreeSet<String>) -> bool {
    let dirs = dirs_of(files);
    let list = |unit: &Unit| {
        unit.reads.iter().all(|r| files.contains(r)) && unit.dirs.iter().all(|d| dirs.contains(d))
    };
    hint.unit_files.iter().all(|f| files.contains(f))
        && hint
            .units
            .iter()
            .all(|(own, unit)| (files.contains(own) || dirs.contains(own)) && list(unit))
        && hint
            .tests
            .iter()
            .all(|(own, tests)| hint.units.contains_key(own) && tests.values().flatten().all(list))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // covers: C-38
    #[test]
    fn writing_then_reading_with_the_same_scope_gives_the_hint(
        hint in hint(),
        sources in proptest::collection::btree_set(file_path(), 0..5),
    ) {
        let key = Key::derive(sources.iter().map(String::as_str));
        let mut files = files_of(&hint);
        files.extend(sources.iter().cloned());
        let written = write(&hint, &key).unwrap();
        let read = read(&written.bytes, &key, files.iter().map(String::as_str)).unwrap();
        prop_assert_eq!(&read.hint, &hint);
        prop_assert_eq!(read.content_id, written.content_id);
        prop_assert_eq!((read.units, read.dropped), (hint.units.len(), 0));
    }

    // covers: C-38
    #[test]
    fn a_scope_without_a_file_drops_exactly_the_test_files_that_named_it(
        hint in hint(),
        which in any::<prop::sample::Index>(),
    ) {
        let key = Key::derive(["a/x.py"]);
        let bytes = write(&hint, &key).unwrap().bytes;
        let mut files = files_of(&hint);
        prop_assume!(!files.is_empty());
        let gone = files.iter().nth(which.index(files.len())).unwrap().clone();
        files.remove(&gone);
        let read = read(&bytes, &key, files.iter().map(String::as_str)).unwrap();
        let dirs = dirs_of(&files);
        let named = |own: &String, unit: &Unit| {
            own == &gone
                || (is_dir(own) && !dirs.contains(own))
                || unit.reads.contains(&gone)
                || unit.dirs.iter().any(|d| !dirs.contains(d))
        };
        let kept: BTreeMap<String, Unit> = hint
            .units
            .iter()
            .filter(|(own, unit)| !named(own, unit))
            .map(|(own, unit)| (own.clone(), unit.clone()))
            .collect();
        prop_assert_eq!(&read.hint.units, &kept);
        // A kept unit's tests stay, but those whose own list named it.
        let tests: BTreeMap<String, BTreeMap<String, Option<Unit>>> = hint
            .tests
            .iter()
            .filter(|(own, _)| kept.contains_key(*own))
            .map(|(own, tests)| {
                let tests: BTreeMap<String, Option<Unit>> = tests
                    .iter()
                    .filter(|(_, list)| list.as_ref().is_none_or(|list| !named(&String::new(), list)))
                    .map(|(id, list)| (id.clone(), list.clone()))
                    .collect();
                (own.clone(), tests)
            })
            .filter(|(_, tests)| !tests.is_empty())
            .collect();
        prop_assert_eq!(&read.hint.tests, &tests);
        let unit_files: BTreeSet<String> = hint.unit_files.iter().filter(|f| **f != gone).cloned().collect();
        prop_assert_eq!(&read.hint.unit_files, &unit_files);
        prop_assert!(within(&read.hint, &files));
    }

    // covers: C-38
    #[test]
    fn random_bytes_never_panic_nor_name_a_path_outside_the_scope(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let (_, scope) = sample();
        let files = scope.iter().map(|p| p.to_string()).collect();
        let key = Key::derive(sources());
        let results = [
            read(&bytes, &key, scope.iter().copied()),
            read_content(&bytes, &key, scope.iter().copied()),
        ];
        for read in results.into_iter().flatten() {
            prop_assert!(within(&read.hint, &files));
        }
    }

    // covers: C-38
    #[test]
    fn cuts_and_flips_of_a_hint_never_panic_nor_name_a_path_outside_the_scope(
        cut in any::<prop::sample::Index>(),
        flips in proptest::collection::vec((any::<prop::sample::Index>(), 0u8..8), 0..4),
        compressed in any::<bool>(),
        with_tests in any::<bool>(),
    ) {
        let (hint, scope) = if with_tests { sample_with_tests() } else { sample() };
        let key = Key::derive(sources());
        let mut bytes = if compressed { write(&hint, &key).unwrap().bytes } else { content(&hint, &key).unwrap() };
        bytes.truncate(cut.index(bytes.len() + 1).max(1));
        for (at, bit) in flips {
            let at = at.index(bytes.len());
            bytes[at] ^= 1 << bit;
        }
        // The whole scope, and one without shop/cart.py: smaller than the hint's paths.
        let files: BTreeSet<String> = scope.iter().map(|p| p.to_string()).collect();
        let fewer: BTreeSet<String> = files.iter().filter(|p| *p != "shop/cart.py").cloned().collect();
        for scope in [&files, &fewer] {
            let result = if compressed {
                read(&bytes, &key, scope.iter().map(String::as_str))
            } else {
                read_content(&bytes, &key, scope.iter().map(String::as_str))
            };
            if let Ok(read) = result {
                prop_assert!(within(&read.hint, scope));
            }
        }
    }
}
