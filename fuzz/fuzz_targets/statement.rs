//! Arbitrary bytes as an in-toto Statement and its event predicate (E-3, E-4).
#![no_main]

use libfuzzer_sys::fuzz_target;
use hushrun_core::event::{Event, Statement};

fuzz_target!(|data: &[u8]| {
    if let Ok(statement) = serde_json::from_slice::<Statement>(data) {
        if let Ok(event) = serde_json::from_value::<Event>(statement.predicate) {
            // Round-trips must not panic either.
            let _ = serde_json::to_vec(&event);
            let _ = event.kind_objects();
        }
    }
});
