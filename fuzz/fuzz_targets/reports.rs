//! Arbitrary bytes as a test or coverage report (C-27): in any format, never a panic, within
//! libFuzzer's memory limit, and coverage that never counts more covered than there is.
#![no_main]

use hushrun_reports::{Read, read, sniff};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sniff(data);
    if let Ok((_, Read::Coverage(counts))) = read(data) {
        for ratio in [counts.lines, counts.statements, counts.branches].into_iter().flatten() {
            assert!(ratio.covered <= ratio.total && ratio.total > 0);
        }
    }
});
