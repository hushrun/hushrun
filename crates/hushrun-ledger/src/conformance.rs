//! The backend contract (L-3, L-7), as checks any backend must pass. The unit tests run them on
//! the file and mem backends, and on the HTTP backend against the testkit's fake ledger.
//!
//! Each check gets a fresh, empty ledger from `open`. Keys have the shapes of L-2, which an
//! HTTP ledger insists on (L-10).

use crate::{Ledger, LedgerError};

const D1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const D2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// An event key of `scope` and `digest`; `n` orders its ULID.
fn event(scope: &str, digest: &str, n: u32) -> String {
    format!("v1/events/{scope}/{digest}/01JA{n:022}.check._.json")
}

/// Runs every check, each on a fresh ledger from `open`.
pub fn run(open: &dyn Fn() -> Box<dyn Ledger>) {
    append_is_create_only_and_idempotent(open().as_ref());
    retried_appends_succeed(open().as_ref());
    listings_are_sorted_and_prefixed(open().as_ref());
    deletion(open().as_ref());
    keys_are_validated(open().as_ref());
    concurrent_appends_are_all_listed_and_read(open().as_ref());
}

pub fn retried_appends_succeed(ledger: &dyn Ledger) {
    let key = event("a", D1, 1);
    ledger.append(&key, b"one").unwrap();
    ledger.append(&key, b"one").unwrap();
    assert_eq!(ledger.get(&key).unwrap(), b"one");
}

pub fn append_is_create_only_and_idempotent(ledger: &dyn Ledger) {
    let key = event("a", D1, 1);
    ledger.append(&key, b"one").unwrap();
    // A retried create with identical bytes succeeds.
    ledger.append(&key, b"one").unwrap();
    assert_eq!(
        ledger.append(&key, b"two"),
        Err(LedgerError::AlreadyExists(key.clone())),
        "{ledger:?}"
    );
    assert_eq!(ledger.get(&key).unwrap(), b"one");
}

pub fn listings_are_sorted_and_prefixed(ledger: &dyn Ledger) {
    let (a1, a3, a2, ab) = (
        event("a", D1, 1),
        event("a", D1, 3),
        event("a", D2, 2),
        event("ab", D1, 9),
    );
    let deployed = "v1/deployed/a/production/9999999999999.01JA0000000000000000000000.json";
    for key in [&a3, &a1, &a2, &ab, deployed] {
        ledger.append(key, key.as_bytes()).unwrap();
    }
    assert_eq!(
        ledger.list(&format!("v1/events/a/{D1}/")).unwrap(),
        [a1.clone(), a3.clone()],
        "{ledger:?}"
    );
    assert_eq!(
        ledger.list("v1/events/a/").unwrap(),
        [a1.clone(), a3.clone(), a2.clone()]
    );
    assert_eq!(
        ledger.list("v1/events/a").unwrap().len(),
        4,
        "a string prefix also matches ab/"
    );
    assert_eq!(
        ledger.list_first("v1/events/a/", 2).unwrap(),
        [a1.clone(), a3.clone()]
    );
    assert_eq!(ledger.list("v1/deployed/").unwrap(), [deployed]);
    assert_eq!(ledger.list("v1/").unwrap().len(), 5);
    assert!(ledger.list("v1/events/none/").unwrap().is_empty());
    let many = ledger.get_many(&[a1.clone(), "v1/events/none".into()]);
    assert_eq!(many[0].as_deref().unwrap(), a1.as_bytes());
    assert_eq!(many[1], Err(LedgerError::NotFound("v1/events/none".into())));
}

pub fn deletion(ledger: &dyn Ledger) {
    let key = event("x", D1, 1);
    ledger.append(&key, b"1").unwrap();
    ledger.delete(&key).unwrap();
    assert_eq!(ledger.get(&key), Err(LedgerError::NotFound(key.clone())));
}

pub fn keys_are_validated(ledger: &dyn Ledger) {
    for bad in ["", "/abs", "a/../b", "a//b", "a\\b", "./a"] {
        assert!(
            matches!(ledger.append(bad, b"x"), Err(LedgerError::Invalid(_))),
            "{bad:?}"
        );
        assert!(
            matches!(ledger.get(bad), Err(LedgerError::Invalid(_))),
            "{bad:?}"
        );
    }
}

/// L-7: no locks are needed. Keys never collide, and a listing after the writes sees every one.
pub fn concurrent_appends_are_all_listed_and_read(ledger: &dyn Ledger) {
    std::thread::scope(|scope| {
        for t in 0..8 {
            scope.spawn(move || {
                for i in 0..10 {
                    let key = event("s", D1, t * 100 + i);
                    ledger.append(&key, key.as_bytes()).unwrap();
                }
            });
        }
    });
    let keys = ledger.list(&format!("v1/events/s/{D1}/")).unwrap();
    assert_eq!(keys.len(), 80, "{ledger:?}");
    let bytes = ledger.get_many(&keys);
    for (key, bytes) in keys.iter().zip(bytes) {
        assert_eq!(bytes.unwrap(), key.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::run;
    use crate::{FileLedger, Ledger, MemLedger};

    // covers: L-3, L-6, L-7
    #[test]
    fn the_file_backend_conforms() {
        let dir = tempfile::tempdir().unwrap();
        let n = AtomicUsize::new(0);
        run(&|| {
            let root = dir
                .path()
                .join(n.fetch_add(1, Ordering::SeqCst).to_string());
            // A file ledger exists once its directory does (L-5).
            std::fs::create_dir_all(&root).unwrap();
            Box::new(FileLedger::new(root))
        });
    }

    // covers: L-3, L-7
    #[test]
    fn the_mem_backend_conforms() {
        run(&|| Box::new(MemLedger::new()));
    }

    // covers: L-3, L-7, L-10
    #[cfg(feature = "cloud")]
    #[test]
    fn the_http_backend_conforms() {
        use hushrun_testkit::fake_ledger::FakeLedger;
        // Every check gets its own server, so its ledger starts empty.
        let servers = std::sync::Mutex::new(Vec::new());
        run(&|| {
            let server = FakeLedger::start();
            let url = hushrun_core::ledger_url::LedgerUrl::parse(&server.url()).unwrap();
            let options = crate::Options {
                token: Some(crate::Secret::new(FakeLedger::ADMIN)),
                ..crate::Options::default()
            };
            let ledger = crate::HttpLedger::open(&url, &options).unwrap();
            servers.lock().unwrap().push(server);
            Box::new(ledger)
        });
    }

    // covers: L-5
    #[test]
    fn a_missing_file_ledger_is_unavailable_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = FileLedger::new(dir.path().join("typo"));
        assert!(ledger.list("v1/events/").unwrap_err().is_unavailable());
        assert!(ledger.get("v1/x").unwrap_err().is_unavailable());
        let mem = MemLedger::new();
        mem.set_unavailable(true);
        assert!(mem.list("v1/").unwrap_err().is_unavailable());
    }

    #[test]
    fn mem_ledgers_are_shared_by_name() {
        let a = MemLedger::named("conformance-shared");
        let b = MemLedger::named("conformance-shared");
        a.append("k/1", b"x").unwrap();
        assert_eq!(b.get("k/1").unwrap(), b"x");
    }
}
