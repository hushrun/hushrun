//! Arbitrary bytes as hushrun.yaml (C-1 … C-8) and as a signers document (L-13).
#![no_main]

use libfuzzer_sys::fuzz_target;
use hushrun_core::policy::Policy;
use hushrun_core::signers::Signers;

fuzz_target!(|data: &[u8]| {
    let _ = Policy::parse("hushrun.yaml", data);
    if let Ok(signers) = Signers::parse(data) {
        // What parses writes back as a document that parses to the same signers.
        let again = Signers::from_json(&signers.to_json()).expect("its own document parses");
        assert_eq!(again.to_json(), signers.to_json());
    }
});
