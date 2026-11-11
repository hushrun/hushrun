//! Arbitrary bytes as a hint from a server (C-38): as a zlib stream and as a content already
//! inflated, never a panic, within libFuzzer's memory limit, and nothing read that names a path
//! outside the scope's files and their directories. The key and scope are those of the `pytest`
//! vector in `testdata/digest/unit-hints.json`, whose bytes seed the corpus
//! (`tools/refdigest.py unit-hints … --corpus`), so that mutations keep tokens that match.
#![no_main]

use std::collections::BTreeSet;

use hushrun_core::hints::{Hint, Key, read, read_content};
use libfuzzer_sys::fuzz_target;

const SOURCES: [&str; 4] = [
    "shop/__init__.py",
    "shop/cart.py",
    "shop/pricing.py",
    "tests/conftest.py",
];
const SCOPE: [&str; 7] = [
    "shop/__init__.py",
    "shop/cart.py",
    "shop/pricing.py",
    "tests/conftest.py",
    "tests/test_cart.py",
    "tests/test_pricing.py",
    "uv.lock",
];

fn within(hint: &Hint, files: &BTreeSet<&str>) {
    let dirs: BTreeSet<&str> = ["", ".", "shop", "tests"].into();
    for file in &hint.unit_files {
        assert!(files.contains(file.as_str()), "{file}");
    }
    for (own, unit) in &hint.units {
        assert!(
            files.contains(own.as_str()) || dirs.contains(own.as_str()),
            "{own}"
        );
        for read in &unit.reads {
            assert!(files.contains(read.as_str()), "{read}");
        }
        for dir in &unit.dirs {
            assert!(dirs.contains(dir.as_str()), "{dir}");
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let key = Key::derive(SOURCES);
    let files: BTreeSet<&str> = SCOPE.into();
    for read in [read(data, &key, SCOPE), read_content(data, &key, SCOPE)]
        .into_iter()
        .flatten()
    {
        within(&read.hint, &files);
        assert!(read.dropped <= read.units);
    }
});
