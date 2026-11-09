//! Arbitrary bytes through envelope parsing and every verification step (E-8).
#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use hushrun_attest::{Expected, Location, SigningKey, verify};
use hushrun_core::digest::Digest;
use hushrun_core::event::EventKind;
use hushrun_core::names::{ScopeName, StageName};
use hushrun_core::time::Time;
use hushrun_core::signers::Signers;

fn signers() -> &'static Signers {
    static SIGNERS: OnceLock<Signers> = OnceLock::new();
    SIGNERS.get_or_init(|| {
        let key = SigningKey::from_seed(&[1; 32]).public_key();
        let document = Signers::example("github.com/acme/app", &[(&key, "user:ana@acme.dev")]);
        Signers::from_json(&document).expect("valid signers")
    })
}

fuzz_target!(|data: &[u8]| {
    let scope = ScopeName::default_scope();
    let stage = StageName::new("staging").expect("name");
    let digest = Digest::of(b"fuzz");
    let now = Time::parse("2026-10-13T15:00:00Z").expect("time");
    for location in [
        Location::Events { digest: &digest, kind: EventKind::Check, stage: Some(&stage) },
        Location::Deployed { stage: Some(&stage) },
        Location::Deployed { stage: None },
        Location::File,
    ] {
        let expected = Expected { repository: "github.com/acme/app", scope: &scope, location };
        let _ = verify(data, signers(), &expected, now);
    }
});
