//! The HTTP backend against the testkit's fake ledger and a fake GitHub ID token endpoint, on
//! the loopback.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use hushrun_testkit::fake_ledger::{FakeLedger, Fault, Role};
use hushrun_testkit::http::{Reply, Response, Server};

use super::*;
use crate::{OidcEndpoint, Secret};

const D: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn key(n: u32) -> String {
    format!("v1/events/default/{D}/01JA{n:022}.check._.json")
}

/// Fast retries, so that the tests don't sleep.
fn quick() -> Tuning {
    Tuning {
        request_timeout: Duration::from_millis(500),
        connect_timeout: Duration::from_millis(500),
        attempts: 4,
        backoff: Duration::from_millis(1),
        max_retry_after: Duration::from_secs(60),
        max_list_pages: 20_000,
    }
}

fn with_token(token: &str) -> Options {
    Options {
        token: Some(Secret::new(token)),
        ..Options::default()
    }
}

fn open(fake: &FakeLedger, options: &Options) -> HttpLedger {
    let url = LedgerUrl::parse(&fake.url()).unwrap();
    HttpLedger::open_tuned(&url, options, quick()).unwrap()
}

fn requests_to(fake: &FakeLedger, method: &str, path: &str) -> usize {
    fake.requests()
        .iter()
        .filter(|r| r.method == method && r.path.starts_with(path))
        .count()
}

// covers: L-3, L-10
#[test]
fn speaks_the_protocol() {
    let fake = FakeLedger::start();
    let ledger = open(&fake, &with_token(FakeLedger::ADMIN));
    let info = ledger.info().unwrap();
    assert_eq!(info.protocol, PROTOCOL);
    assert_eq!(info.repository.as_deref(), Some(FakeLedger::REPOSITORY));
    assert_eq!(info.ledger.as_deref(), Some(fake.url().as_str()));

    ledger.append(&key(1), b"one").unwrap();
    assert_eq!(fake.object(&key(1)).unwrap(), b"one");
    assert_eq!(ledger.get(&key(1)).unwrap(), b"one");
    ledger.append(&key(2), b"two").unwrap();

    // Creates send If-None-Match: *, and every request carries the token.
    let seen = fake.requests();
    let creates: Vec<_> = seen
        .iter()
        .filter(|r| r.method == "PUT")
        .map(|r| (r.path.as_str(), r.if_none_match.as_deref()))
        .collect();
    assert_eq!(
        creates,
        [
            (format!("objects/{}", key(1)).as_str(), Some("*")),
            (format!("objects/{}", key(2)).as_str(), Some("*")),
        ]
    );
    let bearer = format!("Bearer {}", FakeLedger::ADMIN);
    assert!(
        seen.iter()
            .all(|r| r.authorization.as_deref() == Some(bearer.as_str())),
        "{seen:?}"
    );
    // The token never shows in Debug output.
    let shown = format!("{ledger:?} {:?}", with_token(FakeLedger::ADMIN));
    assert!(!shown.contains(FakeLedger::ADMIN), "{shown}");
}

// covers: L-3, L-10
#[test]
fn listings_follow_pages() {
    let fake = FakeLedger::start();
    for n in 0..7 {
        fake.put_object(&key(n), b"x");
    }
    fake.put_object(
        "v1/deployed/default/production/9999999999999.01JA0000000000000000000000.json",
        b"d",
    );
    fake.set_page_limit(3);
    let ledger = open(&fake, &with_token(FakeLedger::READER));
    let prefix = format!("v1/events/default/{D}/");
    let keys = ledger.list(&prefix).unwrap();
    assert_eq!(keys, (0..7).map(key).collect::<Vec<_>>());
    assert_eq!(
        requests_to(&fake, "GET", "objects"),
        3,
        "pages of 3, 3 and 1"
    );
    let pages: Vec<_> = fake
        .requests()
        .iter()
        .filter(|r| r.method == "GET" && r.path == "objects")
        .map(|r| {
            r.query
                .iter()
                .find(|(n, _)| n == "after")
                .map(|(_, v)| v.clone())
        })
        .collect();
    assert_eq!(pages, [None, Some(key(2)), Some(key(5))]);

    // `list_first` asks for one page of n keys.
    fake.set_page_limit(1000);
    let before = requests_to(&fake, "GET", "objects");
    assert_eq!(ledger.list_first(&prefix, 2).unwrap(), [key(0), key(1)]);
    assert_eq!(requests_to(&fake, "GET", "objects"), before + 1);
    let last = fake.requests().pop().unwrap();
    assert!(
        last.query.contains(&("limit".into(), "2".into())),
        "{last:?}"
    );
    assert!(ledger.list_first(&prefix, 0).unwrap().is_empty());
    // A string prefix, the empty prefix, and a prefix with no keys.
    assert_eq!(ledger.list("v1/").unwrap().len(), 8);
    assert_eq!(ledger.list("").unwrap().len(), 8);
    assert!(ledger.list("v1/events/other/").unwrap().is_empty());
}

// covers: L-10, L-3
#[test]
fn get_many_batches_a_hundred_keys_sixteen_at_a_time() {
    let fake = FakeLedger::start();
    let keys: Vec<String> = (0..1700).map(key).collect();
    for k in &keys {
        fake.put_object(k, k.as_bytes());
    }
    fake.set_batch_delay(Duration::from_millis(100));
    let ledger = open(&fake, &with_token(FakeLedger::READER));
    let mut wanted = keys.clone();
    wanted.push(key(9999));
    wanted.push("not a key".into());
    let results = ledger.get_many(&wanted);
    assert_eq!(results.len(), wanted.len());
    for (k, result) in keys.iter().zip(&results) {
        assert_eq!(result.as_deref().unwrap(), k.as_bytes());
    }
    assert_eq!(results[1700], Err(LedgerError::NotFound(key(9999))));
    assert!(matches!(results[1701], Err(LedgerError::Invalid(_))));
    assert_eq!(
        requests_to(&fake, "POST", "batch-get"),
        18,
        "1701 valid keys"
    );
    let in_flight = fake.max_batches_in_flight();
    assert!((2..=16).contains(&in_flight), "{in_flight} batches at once");
    // Up to 100 keys is one request.
    let before = requests_to(&fake, "POST", "batch-get");
    assert_eq!(ledger.get_many(&keys[..100]).len(), 100);
    assert_eq!(requests_to(&fake, "POST", "batch-get"), before + 1);
}

// covers: L-4, L-10
#[test]
fn retries_429_and_5xx_then_gives_up() {
    let fake = FakeLedger::start();
    fake.put_object(&key(0), b"t");
    let ledger = open(&fake, &with_token(FakeLedger::READER));
    for status in [429, 500, 502, 503, 504] {
        fake.inject(Fault::Status(status, None), 3);
        assert_eq!(ledger.get(&key(0)).unwrap(), b"t", "{status}");
    }
    // Four attempts in all: the first and three retries (L-4).
    let before = fake.requests().len();
    fake.inject(Fault::Status(503, None), 4);
    let err = ledger.get(&key(0)).unwrap_err();
    assert!(err.is_unavailable(), "{err}");
    assert!(err.to_string().contains("503"), "{err}");
    assert_eq!(fake.requests().len() - before, 4);
    // Dropped connections and timeouts are retried too.
    fake.inject(Fault::Drop, 2);
    assert!(ledger.get(&key(0)).is_ok());
    fake.inject(Fault::Delay(Duration::from_secs(2)), 1);
    assert!(ledger.get(&key(0)).is_ok());
    // Other errors aren't retried.
    let before = fake.requests().len();
    fake.inject(Fault::Status(400, None), 1);
    assert!(matches!(ledger.get(&key(0)), Err(LedgerError::Invalid(_))));
    assert_eq!(fake.requests().len() - before, 1);
}

// covers: L-4, L-10
#[test]
fn honors_retry_after_within_the_budget() {
    let fake = FakeLedger::start();
    fake.put_object(&key(0), b"t");
    let ledger = open(&fake, &with_token(FakeLedger::READER));
    fake.inject(Fault::Status(429, Some("1".into())), 1);
    let started = Instant::now();
    assert!(ledger.get(&key(0)).is_ok());
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "waited {:?}",
        started.elapsed()
    );
    // An HTTP date works too.
    let soon = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(1);
    let date = jiff::fmt::rfc2822::DateTimePrinter::new()
        .timestamp_to_rfc9110_string(&soon)
        .unwrap();
    fake.inject(Fault::Status(503, Some(date)), 1);
    assert!(ledger.get(&key(0)).is_ok());
    // A wait beyond what is left of the budget fails at once (L-4).
    let url = LedgerUrl::parse(&fake.url()).unwrap();
    let budgeted = Options {
        budget: Some(Duration::from_secs(5)),
        ..with_token(FakeLedger::READER)
    };
    let ledger = HttpLedger::open_tuned(&url, &budgeted, quick()).unwrap();
    fake.inject(Fault::Status(503, Some("30".into())), 1);
    let started = Instant::now();
    let err = ledger.get(&key(0)).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(err.is_unavailable(), "{err}");
    assert!(err.to_string().contains("retry after 30 s"), "{err}");
    // So does a request that would outlast the budget.
    let short = Options {
        budget: Some(Duration::from_millis(300)),
        ..with_token(FakeLedger::READER)
    };
    let ledger = HttpLedger::open_tuned(&url, &short, quick()).unwrap();
    fake.inject(Fault::Delay(Duration::from_secs(2)), 4);
    let err = ledger.get(&key(0)).unwrap_err();
    assert!(err.to_string().contains("budget"), "{err}");
}

// covers: L-3, L-4, L-10
#[test]
fn a_create_whose_answer_was_lost_is_recognized_by_hash() {
    let fake = FakeLedger::start();
    let ledger = open(&fake, &with_token(FakeLedger::WRITER));
    // The object is stored, but the answer never arrives; the retry finds the same bytes.
    fake.inject_on(Some("PUT"), Some("objects/"), Fault::DropAfter, 1);
    ledger.append(&key(1), b"one").unwrap();
    assert_eq!(requests_to(&fake, "PUT", "objects/"), 2);
    // The same after a timeout.
    fake.inject_on(
        Some("PUT"),
        Some("objects/"),
        Fault::Delay(Duration::from_secs(2)),
        1,
    );
    ledger.append(&key(2), b"two").unwrap();
    assert_eq!(fake.object(&key(2)).unwrap(), b"two");
    // A create of identical bytes succeeds; other bytes are a conflict, not a success.
    ledger.append(&key(1), b"one").unwrap();
    assert_eq!(
        ledger.append(&key(1), b"other"),
        Err(LedgerError::AlreadyExists(key(1)))
    );
    assert_eq!(fake.object(&key(1)).unwrap(), b"one");
}

// covers: L-10
#[test]
fn already_exists_hashes_are_read_wherever_they_are() {
    let ours = Digest::of(b"x").hex();
    for body in [
        format!(r#"{{"error":{{"code":"already_exists","message":"m"}},"sha256":"{ours}"}}"#),
        format!(
            r#"{{"error":{{"code":"already_exists","sha256":"{}"}}}}"#,
            ours.to_uppercase()
        ),
    ] {
        assert_eq!(stored_sha256(body.as_bytes()), Some(ours.clone()), "{body}");
    }
    for body in [
        r#"{"error":{"code":"already_exists"}}"#,
        r#"{"sha256":"abc"}"#,
        "nope",
    ] {
        assert_eq!(stored_sha256(body.as_bytes()), None, "{body}");
    }
    // Without a hash, the object is read back and compared.
    let server = Server::start(|request| match request.method.as_str() {
        "PUT" => Reply::Send(Response::json(
            412,
            &serde_json::json!({"error": {"code": "already_exists", "message": "exists"}}),
        )),
        _ => Reply::Send(Response::bytes(200, "stored")),
    });
    let url = LedgerUrl::parse(&format!("{}/l/a/b", server.url())).unwrap();
    let ledger = HttpLedger::open_tuned(&url, &Options::default(), quick()).unwrap();
    assert!(ledger.append(&key(1), b"stored").is_ok());
    assert_eq!(
        ledger.append(&key(1), b"other"),
        Err(LedgerError::AlreadyExists(key(1)))
    );
}

// covers: L-5, L-6, L-10
#[test]
fn maps_errors_as_the_protocol_says() {
    let fake = FakeLedger::start();
    let writer = open(&fake, &with_token(FakeLedger::WRITER));
    let reader = open(&fake, &with_token(FakeLedger::READER));
    let nobody = open(&fake, &Options::default());
    let wrong = open(&fake, &with_token("hrl_revoked"));
    // 401 without a token says where tokens come from; with one, that it was refused.
    let err = nobody.list("v1/").unwrap_err();
    assert!(
        matches!(err, LedgerError::Denied(_)) && err.is_unavailable(),
        "{err}"
    );
    assert!(
        err.to_string().contains("HUSHRUN_REPOSITORY_TOKEN"),
        "{err}"
    );
    assert!(err.to_string().contains("id-token: write"), "{err}");
    let err = wrong.list("v1/").unwrap_err();
    assert!(
        err.to_string()
            .contains("refused the token from HUSHRUN_REPOSITORY_TOKEN"),
        "{err}"
    );
    assert!(!err.to_string().contains("hrl_revoked"), "{err}");
    // 403: a role that can't write or delete, or a key outside v1/.
    assert!(matches!(
        reader.append(&key(1), b"x"),
        Err(LedgerError::Denied(_))
    ));
    assert!(matches!(
        writer.delete(&key(1)),
        Err(LedgerError::Denied(_))
    ));
    assert!(matches!(
        writer.append("other/x.json", b"x"),
        Err(LedgerError::Denied(_))
    ));
    // 400: a key under v1/ in none of its shapes; 413: an object over 1 MiB.
    assert!(matches!(
        writer.append("v1/x/1.json", b"x"),
        Err(LedgerError::Invalid(_))
    ));
    let big = vec![b'x'; MAX_OBJECT_BYTES + 1];
    assert!(matches!(
        writer.append(&key(2), &big),
        Err(LedgerError::Invalid(_))
    ));
    for status in [428, 418] {
        fake.inject(Fault::Status(status, None), 1);
        assert!(
            matches!(writer.append(&key(3), b"x"), Err(LedgerError::Invalid(_))),
            "{status}"
        );
    }
    // 404.
    assert_eq!(reader.get(&key(9)), Err(LedgerError::NotFound(key(9))));
    // DELETE of a missing object succeeds: GC retries (L-9).
    let admin = open(&fake, &with_token(FakeLedger::ADMIN));
    assert_eq!(admin.delete(&key(9)), Ok(()));
    // Keys outside L-10's alphabet never reach the ledger.
    let before = fake.requests().len();
    for bad in ["v1/a b", "v1/é", "v1/a%2Fb", "", "v1//a", "/v1/a"] {
        assert!(
            matches!(writer.append(bad, b"x"), Err(LedgerError::Invalid(_))),
            "{bad}"
        );
        assert!(
            matches!(writer.get(bad), Err(LedgerError::Invalid(_))),
            "{bad}"
        );
    }
    assert!(check_http_key(&"a".repeat(1025)).is_err());
    assert!(check_http_key(&format!("v1/{}", "a".repeat(1021))).is_ok());
    assert_eq!(fake.requests().len(), before);
    // A URL where no ledger answers is unavailable, not empty (L-5).
    let url = LedgerUrl::parse(&fake.url().replace("/l/acme/back", "/l/acme/nope")).unwrap();
    let typo = HttpLedger::open_tuned(&url, &with_token(FakeLedger::READER), quick()).unwrap();
    for err in [
        typo.list("v1/").unwrap_err(),
        typo.info().unwrap_err(),
        typo.get_many(&[key(1)]).remove(0).unwrap_err(),
    ] {
        assert!(matches!(err, LedgerError::Unavailable(_)), "{err}");
        assert!(err.to_string().contains("no repository answers"), "{err}");
    }
    // A ledger that isn't there is unavailable (L-5).
    drop(fake);
    let gone = LedgerUrl::parse("http://127.0.0.1:9/l/a/b").unwrap();
    let ledger = HttpLedger::open_tuned(&gone, &Options::default(), quick()).unwrap();
    let err = ledger.list("v1/").unwrap_err();
    assert!(matches!(err, LedgerError::Unavailable(_)), "{err}");
}

// covers: L-3, L-4, L-10
#[test]
fn a_write_the_orgs_plan_refuses_is_a_quota_error_and_isnt_retried() {
    let fake = FakeLedger::start();
    let writer = open(&fake, &with_token(FakeLedger::WRITER));
    let upgrade = "https://app.hushrun.com/acme/-/billing";
    let body = hushrun_testkit::fake_results::quota_exceeded(
        "acme used the 2,000 checks of its Free plan this month",
        upgrade,
    );
    // Once: a retry would store the object and succeed.
    fake.inject_on(Some("PUT"), Some(".check."), Fault::Answer(402, body), 1);
    let err = writer.append(&key(1), b"check").unwrap_err();
    let LedgerError::Quota(quota) = &err else {
        panic!("{err:?}");
    };
    assert_eq!(
        quota.message,
        "acme used the 2,000 checks of its Free plan this month"
    );
    assert_eq!(quota.upgrade_url.as_deref(), Some(upgrade));
    assert_eq!(quota.plan.as_deref(), Some("free"));
    assert_eq!((quota.used, quota.limit), (Some(2000), Some(2000)));
    assert_eq!(quota.period_end.as_deref(), Some("2026-11-01T00:00:00Z"));
    assert!(!err.is_unavailable(), "{err}");
    assert!(err.to_string().contains(upgrade), "{err}");
    // One request: a 402 is never retried (L-4), and nothing was stored.
    assert_eq!(requests_to(&fake, "PUT", "objects/"), 1);
    assert_eq!(fake.object(&key(1)), None);
    // Another event is written as before.
    let started = format!("v1/events/default/{D}/01JA{:022}.started.staging.json", 2);
    writer.append(&started, b"started").unwrap();
    assert_eq!(fake.object(&started).unwrap(), b"started");

    // A 402 with another code, or none, keeps today's mapping: an invalid request.
    for other in [
        serde_json::json!({"error": {"code": "payment_required", "message": "pay"}}),
        serde_json::json!({"plan": "free"}),
    ] {
        fake.inject_on(Some("PUT"), None, Fault::Answer(402, other.clone()), 1);
        let err = writer.append(&key(3), b"x").unwrap_err();
        assert!(matches!(err, LedgerError::Invalid(_)), "{other}: {err:?}");
    }
    fake.inject(Fault::Status(402, None), 1);
    assert!(matches!(
        writer.append(&key(4), b"x"),
        Err(LedgerError::Invalid(_))
    ));
}

// covers: L-10
#[test]
fn never_follows_a_redirect_to_another_host() {
    let fake = FakeLedger::start();
    fake.put_object(&key(0), b"t");
    let elsewhere_hits = Arc::new(AtomicUsize::new(0));
    let hits = elsewhere_hits.clone();
    let elsewhere = Server::start(move |_| {
        hits.fetch_add(1, Ordering::SeqCst);
        Reply::Send(Response::bytes(200, "stolen"))
    });
    // `localhost` is another host than 127.0.0.1, even on the same machine.
    let port = elsewhere.addr().port();
    let ledger = open(&fake, &with_token(FakeLedger::READER));
    for target in [
        format!("http://localhost:{port}/objects/{}?sig=secret", key(0)),
        format!("{}/objects/{}", elsewhere.url(), key(0)),
    ] {
        fake.inject(Fault::Redirect(target), 1);
        let err = ledger.get(&key(0)).unwrap_err();
        assert!(err.is_unavailable(), "{err}");
        assert!(err.to_string().contains("redirect"), "{err}");
        assert!(!err.to_string().contains("secret"), "{err}");
    }
    assert_eq!(elsewhere_hits.load(Ordering::SeqCst), 0);
    // Within the ledger's own origin, a redirect is followed, with the token.
    fake.inject(
        Fault::Redirect(format!("{}/objects/{}", fake.url(), key(0))),
        1,
    );
    assert_eq!(ledger.get(&key(0)).unwrap(), b"t");
    let bearer = format!("Bearer {}", FakeLedger::READER);
    assert!(
        fake.requests()
            .iter()
            .all(|r| r.authorization.as_deref() == Some(bearer.as_str()))
    );
}

/// A JWT whose `exp` is `ttl` from now (unsigned: only the cache reads it).
fn id_token(ttl: Duration) -> String {
    let exp = (SystemTime::now() + ttl)
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let b64 = |s: String| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
    format!(
        "{}.{}.sig",
        b64(r#"{"alg":"RS256","typ":"JWT"}"#.into()),
        b64(format!(r#"{{"aud":"x","exp":{exp}}}"#))
    )
}

/// GitHub's ID token endpoint: it checks the request token and the audience, and counts.
fn oidc_endpoint(audience: String, token: String) -> (Server, Arc<AtomicUsize>) {
    let issued = Arc::new(AtomicUsize::new(0));
    let count = issued.clone();
    let server = Server::start(move |request| {
        if request.header("authorization") != Some("Bearer request-token") {
            return Reply::Send(Response::new(401));
        }
        if request.query("api-version") != Some("2.0")
            || request.query("audience") != Some(audience.as_str())
        {
            return Reply::Send(Response::new(400));
        }
        count.fetch_add(1, Ordering::SeqCst);
        Reply::Send(Response::json(
            200,
            &serde_json::json!({"count": 1, "value": token}),
        ))
    });
    (server, issued)
}

// covers: L-10
#[test]
fn github_id_tokens_are_requested_for_the_ledger_and_cached() {
    let fake = FakeLedger::start();
    fake.put_object(&key(0), b"t");
    for (ttl, expected_requests) in [(Duration::from_secs(600), 1), (Duration::from_secs(30), 3)] {
        let token = id_token(ttl);
        fake.add_token(&token, Role::Writer);
        let (endpoint, issued) = oidc_endpoint(fake.url(), token.clone());
        let options = Options {
            github_oidc: Some(OidcEndpoint {
                url: format!("{}/idtoken?api-version=2.0", endpoint.url()),
                token: Secret::new("request-token"),
            }),
            ..Options::default()
        };
        let ledger = open(&fake, &options);
        for _ in 0..3 {
            assert_eq!(ledger.get(&key(0)).unwrap(), b"t");
        }
        // Cached until a minute before it expires.
        assert_eq!(
            issued.load(Ordering::SeqCst),
            expected_requests,
            "ttl {ttl:?}"
        );
        let last = fake.requests().pop().unwrap();
        assert_eq!(last.authorization, Some(format!("Bearer {token}")));
        // The request token never goes to the ledger.
        assert!(fake.requests().iter().all(|r| {
            !r.authorization
                .as_deref()
                .unwrap_or("")
                .contains("request-token")
        }));
    }
    // A refused request token is denied, with the permission to check.
    let (endpoint, _) = oidc_endpoint(fake.url(), "unused".into());
    let refused = Options {
        github_oidc: Some(OidcEndpoint {
            url: format!("{}/idtoken?api-version=2.0", endpoint.url()),
            token: Secret::new("wrong"),
        }),
        ..Options::default()
    };
    let err = open(&fake, &refused).get(&key(0)).unwrap_err();
    assert!(matches!(err, LedgerError::Denied(_)), "{err}");
    assert!(err.to_string().contains("id-token: write"), "{err}");
    assert!(!err.to_string().contains("wrong"), "{err}");
    // Only https endpoints, or http on the loopback, get the request token.
    let plain = Options {
        github_oidc: Some(OidcEndpoint {
            url: "http://token.example.com/idtoken?api-version=2.0".into(),
            token: Secret::new("request-token"),
        }),
        ..Options::default()
    };
    let err = open(&fake, &plain).get(&key(0)).unwrap_err();
    assert!(err.to_string().contains("https://"), "{err}");
}

// covers: L-10
#[test]
fn tokens_come_from_the_options_in_order() {
    let fake = FakeLedger::start();
    fake.put_object(&key(0), b"t");
    // The static token wins over the others.
    let options = Options {
        token: Some(Secret::new(FakeLedger::READER)),
        token_command: Some("exit 1".into()),
        github_oidc: Some(OidcEndpoint {
            url: "https://unused.example/x".into(),
            token: Secret::new("r"),
        }),
        ..Options::default()
    };
    assert!(open(&fake, &options).get(&key(0)).is_ok());
    #[cfg(unix)]
    {
        let command = Options {
            token_command: Some(format!("echo {}", FakeLedger::READER)),
            ..Options::default()
        };
        assert!(open(&fake, &command).get(&key(0)).is_ok());
        let failing = Options {
            token_command: Some("echo leaked; echo nope >&2; exit 3".into()),
            ..Options::default()
        };
        let err = open(&fake, &failing).get(&key(0)).unwrap_err();
        assert!(matches!(err, LedgerError::Denied(_)), "{err}");
        assert!(!err.to_string().contains("leaked"), "{err}");
    }
}

// covers: L-10
#[test]
fn describes_the_ledger_and_its_clock() {
    let fake = FakeLedger::start();
    let url = LedgerUrl::parse(&fake.url()).unwrap();
    let info = crate::describe(&url, &with_token(FakeLedger::READER)).unwrap();
    assert_eq!(info.protocol, "hushrun-ledger-http/1");
    assert_eq!(info.org.as_deref(), Some("acme"));
    assert_eq!(info.name.as_deref(), Some("back"));
    // The clock needs no token.
    let server = crate::server_time(&url).unwrap().unwrap();
    let skew = server
        .duration_since(SystemTime::now())
        .unwrap_or_else(|e| e.duration());
    assert!(skew < Duration::from_secs(5), "{skew:?}");
}

/// A ledger at `server`, which answers every request with `reply`.
fn served_by(
    reply: impl Fn(&hushrun_testkit::http::Request) -> Reply + Send + Sync + 'static,
) -> (Server, HttpLedger) {
    let server = Server::start(move |request| reply(&request));
    let url = LedgerUrl::parse(&format!("{}/l/a/b", server.url())).unwrap();
    let ledger = HttpLedger::open_tuned(&url, &Options::default(), quick()).unwrap();
    (server, ledger)
}

// covers: L-3, L-8, L-10
#[test]
fn list_first_follows_short_pages() {
    let fake = FakeLedger::start();
    for n in 0..7 {
        fake.put_object(&key(n), b"x");
    }
    fake.set_page_limit(3);
    let ledger = open(&fake, &with_token(FakeLedger::READER));
    let prefix = format!("v1/events/default/{D}/");
    assert_eq!(
        ledger.list_first(&prefix, 5).unwrap(),
        (0..5).map(key).collect::<Vec<_>>()
    );
    let limits: Vec<String> = fake
        .requests()
        .iter()
        .filter(|r| r.path == "objects")
        .map(|r| {
            r.query
                .iter()
                .find(|(n, _)| n == "limit")
                .map(|(_, v)| v.clone())
                .unwrap()
        })
        .collect();
    assert_eq!(
        limits,
        ["5", "2"],
        "a page of 5, then what the short page left"
    );
}

// covers: L-10
#[test]
fn listings_that_would_never_end_are_refused() {
    for (page, why) in [
        (r#"{"keys": [], "next": "v1/z"}"#, "empty page"),
        (r#"{"keys": ["v1/a"], "next": "x/z"}"#, "outside the prefix"),
        (r#"{"keys": ["v1/b"], "next": "v1/a"}"#, "doesn't advance"),
    ] {
        let (_server, ledger) = served_by(move |_| {
            Reply::Send(Response::json(200, &serde_json::from_str(page).unwrap()))
        });
        let err = ledger.list("v1/").unwrap_err();
        assert!(
            err.is_unavailable() && err.to_string().contains(why),
            "{err}"
        );
    }
    // A server that keeps advancing, a key at a time, still stops at the cap.
    let server = Server::start(|request| {
        let n: u64 = request
            .query("after")
            .and_then(|a| a.strip_prefix("v1/k"))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let next = format!("v1/k{:08}", n + 1);
        Reply::Send(Response::json(
            200,
            &serde_json::json!({"keys": [next.clone()], "next": next}),
        ))
    });
    let url = LedgerUrl::parse(&format!("{}/l/a/b", server.url())).unwrap();
    let tuning = Tuning {
        max_list_pages: 25,
        ..quick()
    };
    let ledger = HttpLedger::open_tuned(&url, &Options::default(), tuning).unwrap();
    let err = ledger.list("v1/").unwrap_err();
    assert!(err.to_string().contains("more than 25 pages"), "{err}");
}

// covers: L-3, L-10
#[test]
fn writes_count_only_with_the_protocols_answers() {
    // A proxy's page, a redirect elsewhere on the host: nothing is known to be stored.
    let (_server, ledger) = served_by(|_| Reply::Send(Response::bytes(200, "maintenance")));
    for result in [ledger.append(&key(1), b"x"), ledger.delete(&key(1))] {
        let err = result.unwrap_err();
        assert!(err.is_unavailable(), "{err}");
        assert!(
            err.to_string().contains("nothing is known to be written"),
            "{err}"
        );
    }
    // A 201 must name the stored bytes' hash.
    let (_server, ledger) = served_by(|_| Reply::Send(Response::new(201)));
    assert!(ledger.append(&key(1), b"x").unwrap_err().is_unavailable());
    // A 303 would turn the PUT into a GET of another page: it isn't followed.
    let gets = Arc::new(AtomicUsize::new(0));
    let count = gets.clone();
    let (server, _) = served_by(|_| Reply::Send(Response::new(500)));
    let target = format!("{}/maintenance", server.url());
    let (_server, ledger) = served_by(move |request| {
        if request.method == "GET" {
            count.fetch_add(1, Ordering::SeqCst);
            return Reply::Send(Response::bytes(200, "ok"));
        }
        Reply::Send(Response::new(303).header("location", target.clone()))
    });
    let err = ledger.append(&key(1), b"x").unwrap_err();
    assert!(err.to_string().contains("redirect"), "{err}");
    assert_eq!(gets.load(Ordering::SeqCst), 0);
}

// --- [M18] The transparency log (L-11) -----------------------------------------------------------

use hushrun_testkit::fake_ledger::ReceiptFault;
use hushrun_tlog::listing::SignedListing;
use hushrun_tlog::{Replay, SignedCheckpoint, proof::verify_consistency};

use crate::log::LogKey;

/// The fake's log key, as hushrun.yaml names it (T-9).
fn log_key(fake: &FakeLedger) -> LogKey {
    let (origin, key) = fake.log_key();
    let key = hushrun_core::signers::PublicKey::parse(&key).unwrap();
    LogKey {
        origin,
        key: key.ed25519().expect("an Ed25519 log key"),
    }
}

fn verifying(fake: &FakeLedger, token: &str) -> HttpLedger {
    let ledger = open(fake, &with_token(token));
    ledger.configure(Some(log_key(fake))).unwrap();
    ledger
}

// covers: L-11, T-9
#[test]
fn creates_are_proven_by_their_receipts() {
    let fake = FakeLedger::start();
    let ledger = verifying(&fake, FakeLedger::WRITER);
    assert_eq!(ledger.checkpoint().unwrap().unwrap().size(), 0);
    ledger.append(&key(1), b"one").unwrap();
    ledger.append(&key(2), b"two").unwrap();
    // A retried create, answered 412, is proven by the receipt in that answer.
    ledger.append(&key(2), b"two").unwrap();
    let newest = ledger.checkpoint().unwrap().unwrap();
    assert_eq!(newest.size(), 2);
    assert_eq!(newest.note, fake.checkpoint());

    // A receipt that proves nothing: the write isn't known to be done (exit 6).
    fake.set_receipt_fault(ReceiptFault::Corrupt);
    let err = ledger.append(&key(3), b"three").unwrap_err();
    assert!(err.is_unavailable(), "{err}");
    assert!(err.to_string().contains("receipt doesn't prove"), "{err}");
    fake.set_receipt_fault(ReceiptFault::Missing);
    let err = ledger.append(&key(4), b"four").unwrap_err();
    assert!(err.to_string().contains("no receipt"), "{err}");
}

// covers: L-11, T-9
#[test]
fn what_arrives_before_trust_is_checked_once_it_is_read() {
    let fake = FakeLedger::start();
    fake.set_receipt_fault(ReceiptFault::Corrupt);
    let ledger = open(&fake, &with_token(FakeLedger::WRITER));
    ledger.append(&key(1), b"one").unwrap();
    let err = ledger.configure(Some(log_key(&fake))).unwrap_err();
    assert!(err.to_string().contains("receipt"), "{err}");

    // Without a log key in hushrun.yaml nothing is checked: the migration warns instead (T-9).
    let ledger = open(&fake, &with_token(FakeLedger::WRITER));
    ledger.configure(None).unwrap();
    ledger.append(&key(2), b"two").unwrap();
    assert!(!ledger.verifying());
    assert_eq!(ledger.checkpoint().unwrap(), None);
}

// covers: L-11
#[test]
fn listings_carry_signed_statements_that_plans_record() {
    let fake = FakeLedger::start();
    for n in 0..5 {
        fake.put_object(&key(n), b"x");
    }
    fake.set_page_limit(2);
    let ledger = verifying(&fake, FakeLedger::READER);
    let prefix = format!("v1/events/default/{D}/");
    assert_eq!(ledger.list(&prefix).unwrap().len(), 5);
    let recorded = ledger.take_listings();
    assert_eq!(recorded.len(), 3, "three pages");
    let verifier = log_key(&fake).verifier().unwrap();
    let mut seen = Vec::new();
    for page in &recorded {
        let statement = SignedListing::open(&page.statement, &verifier).unwrap();
        assert_eq!(statement.listing.prefix, prefix);
        assert!(statement.listing.matches(&page.keys));
        seen.extend(page.keys.clone());
    }
    assert_eq!(seen, (0..5).map(key).collect::<Vec<_>>());
    assert!(ledger.take_listings().is_empty());
    assert_eq!(ledger.checkpoint().unwrap().unwrap().size(), 5);
}

// covers: L-11, T-9
#[test]
fn a_listing_without_a_statement_is_refused_once_trust_names_the_key() {
    let fake = FakeLedger::start();
    fake.put_object(&key(1), b"x");
    fake.set_no_statements(true);
    let ledger = verifying(&fake, FakeLedger::READER);
    let err = ledger.list("v1/").unwrap_err();
    assert!(err.is_unavailable(), "{err}");
    assert!(err.to_string().contains("signed statement"), "{err}");
    // A ledger that keeps no log at all, once hushrun.yaml names its key.
    let fake = FakeLedger::start();
    fake.set_log(false);
    let ledger = verifying(&fake, FakeLedger::WRITER);
    assert!(ledger.list("v1/").unwrap_err().is_unavailable());
    assert!(ledger.append(&key(1), b"x").unwrap_err().is_unavailable());
    assert!(ledger.checkpoint().unwrap_err().is_unavailable());
    // Without the key, it is a Phase 1 ledger.
    let ledger = open(&fake, &with_token(FakeLedger::WRITER));
    ledger.configure(None).unwrap();
    ledger.append(&key(2), b"x").unwrap();
    assert_eq!(ledger.list("v1/").unwrap().len(), 2);
}

// covers: L-11
#[test]
fn a_listing_that_hides_a_key_is_signed_for_and_found_by_a_replay() {
    let fake = FakeLedger::start();
    for n in 0..3 {
        fake.put_object(&key(n), format!("{n}").as_bytes());
    }
    fake.hide_from_listings(&key(1));
    let ledger = verifying(&fake, FakeLedger::READER);
    let prefix = format!("v1/events/default/{D}/");
    // The reader can't tell: the statement verifies.
    assert_eq!(ledger.list(&prefix).unwrap(), [key(0), key(2)]);
    let recorded = ledger.take_listings().pop().unwrap();
    // The log says otherwise.
    let mut replay = Replay::new();
    for entry in fake.log_entries() {
        replay.push(entry);
    }
    let statement =
        SignedListing::open(&recorded.statement, &log_key(&fake).verifier().unwrap()).unwrap();
    let check = replay.check_listing(&statement.listing, Some(&recorded.keys));
    assert_eq!(check.omitted, [(key(1), 1)]);
}

// covers: L-11
#[test]
fn two_histories_fail_the_consistency_check_and_name_both_checkpoints() {
    let fake = FakeLedger::start();
    let writer = verifying(&fake, FakeLedger::WRITER);
    writer.append(&key(1), b"one").unwrap();
    let writers_checkpoint = writer.checkpoint().unwrap().unwrap();
    fake.put_object(&key(2), b"two");
    fake.put_object(&key(3), b"three");
    // The reader is shown a history in which the writer's object never was.
    fake.fork_for(FakeLedger::READER, &[key(1)]);
    let reader = verifying(&fake, FakeLedger::READER);
    assert_eq!(reader.list("v1/").unwrap(), [key(2), key(3)]);
    let err = reader
        .check_checkpoint(&writers_checkpoint.note, "the event v1/x")
        .unwrap_err();
    assert!(err.is_unavailable(), "{err}");
    let text = err.to_string();
    assert!(text.contains("two histories"), "{text}");
    assert!(text.contains("the event v1/x"), "{text}");
    let ours = reader.checkpoint().unwrap().unwrap();
    for note in [&writers_checkpoint.note, &ours.note] {
        for line in note.lines().filter(|l| !l.is_empty()) {
            assert!(text.contains(line), "{line} in {text}");
        }
    }
    // A checkpoint from the same history passes, and so does one nobody signed.
    let honest = verifying(&fake, FakeLedger::WRITER);
    honest.list("v1/").unwrap();
    honest
        .check_checkpoint(&writers_checkpoint.note, "the event")
        .unwrap();
    honest
        .check_checkpoint("not a note\n\n— x AAAA\n", "the event")
        .unwrap();
}

// covers: L-11
#[test]
fn a_ledger_that_changes_history_mid_command_is_caught() {
    let fake = FakeLedger::start();
    fake.put_object(&key(1), b"one");
    fake.put_object(&key(2), b"two");
    let reader = verifying(&fake, FakeLedger::READER);
    reader.list("v1/").unwrap();
    fake.fork_for(FakeLedger::READER, &[key(1)]);
    fake.put_object(&key(3), b"three");
    let err = reader.list("v1/").unwrap_err();
    assert!(err.to_string().contains("two histories"), "{err}");
    // A newer checkpoint of a smaller tree is a rollback.
    let fake = FakeLedger::start();
    fake.put_object(&key(1), b"one");
    let reader = verifying(&fake, FakeLedger::READER);
    reader.list("v1/").unwrap();
    fake.fork_for(FakeLedger::READER, &[key(1)]);
    let err = reader.latest().unwrap_err();
    assert!(err.to_string().contains("smaller"), "{err}");
}

// covers: L-11, L-9
#[test]
fn monitors_read_leaves_proofs_and_gc_statements() {
    let fake = FakeLedger::start();
    let admin = verifying(&fake, FakeLedger::ADMIN);
    admin.append(&key(1), b"one").unwrap();
    admin.append(&key(2), b"two").unwrap();
    admin.append(&key(3), b"three").unwrap();
    admin
        .delete_signed(&key(1), b"a signed GC statement")
        .unwrap();
    admin.delete(&key(2)).unwrap();
    let leaves = admin.leaves(0, 1000).unwrap();
    assert_eq!(
        leaves.iter().map(|l| l.entry.clone()).collect::<Vec<_>>(),
        fake.log_entries()
    );
    assert_eq!(
        leaves[3].gc_statement.as_deref(),
        Some(&b"a signed GC statement"[..])
    );
    assert_eq!(
        leaves[3].entry.gc,
        Some(hushrun_tlog::Hash::of(b"a signed GC statement"))
    );
    assert_eq!(leaves[4].entry.gc, None);
    assert_eq!(admin.leaves(3, 1).unwrap().len(), 1);
    let latest = admin.latest().unwrap();
    assert_eq!(latest.size(), 5);
    let first =
        SignedCheckpoint::open(&fake.checkpoint(), &log_key(&fake).verifier().unwrap()).unwrap();
    let proof = admin.consistency_proof(2, 5).unwrap();
    let mut replay = Replay::new();
    for leaf in &leaves {
        replay.push(leaf.entry.clone());
    }
    verify_consistency(2, 5, &proof, &replay.root_at(2).unwrap(), first.root()).unwrap();
    let info = admin.info().unwrap();
    let log = info.log.unwrap();
    assert_eq!(log.origin, log_key(&fake).origin);
    assert_eq!(log.size, Some(5));
    assert!(log.key.unwrap().starts_with("ed25519:"));
}

// covers: L-11
#[test]
fn objects_are_proven_by_the_bytes_the_log_holds() {
    let fake = FakeLedger::start();
    fake.put_object(&key(1), b"the real event");
    let reader = verifying(&fake, FakeLedger::READER);
    reader.prove_object(&key(1), b"the real event").unwrap();
    // Other bytes than the leaf records: the ledger can't prove them.
    let err = reader.prove_object(&key(1), b"garbage").unwrap_err();
    assert!(err.is_unavailable(), "{err}");
    assert!(err.to_string().contains("doesn't hold"), "{err}");
    // A key the log doesn't hold.
    assert!(
        reader
            .prove_object(&key(2), b"x")
            .unwrap_err()
            .is_unavailable()
    );
    // Without a log key there is nothing to check.
    let unchecked = open(&fake, &with_token(FakeLedger::READER));
    unchecked.configure(None).unwrap();
    unchecked.prove_object(&key(1), b"garbage").unwrap();
}

// --- [M19] The prefix map (L-12) ---------------------------------------------------------------

/// A fake whose every write ends in a map leaf, and whose listings of map prefixes carry proofs,
/// as the hosted ledger's do.
fn mapped() -> FakeLedger {
    let fake = FakeLedger::start();
    fake.set_maps(true);
    fake
}

// covers: L-11, L-12
#[test]
fn listings_with_maps_whole_and_paged() {
    let fake = mapped();
    let writer = verifying(&fake, FakeLedger::WRITER);
    for n in 0..7 {
        writer.append(&key(n), b"x").unwrap();
    }
    // Each write is a batch of its own, which ends with a map leaf.
    assert_eq!(writer.checkpoint().unwrap().unwrap().size(), 14);
    let prefix = format!("v1/events/default/{D}/");
    let reader = verifying(&fake, FakeLedger::READER);
    assert_eq!(
        reader.list(&prefix).unwrap(),
        (0..7).map(key).collect::<Vec<_>>()
    );
    // In pages, and a first page.
    fake.set_page_limit(3);
    assert_eq!(reader.list(&prefix).unwrap().len(), 7);
    assert_eq!(reader.list_first(&prefix, 2).unwrap(), [key(0), key(1)]);
    // An empty prefix, and a prefix that isn't in the map.
    let other = format!("v1/events/default/{}/", "e".repeat(64));
    assert!(reader.list(&other).unwrap().is_empty());
    assert_eq!(reader.list("v1/").unwrap().len(), 7);
}

// covers: L-12
#[test]
fn a_lying_map_passes_the_reader_and_fails_the_replay() {
    let fake = mapped();
    for n in 0..3 {
        fake.put_object(&key(n), format!("{n}").as_bytes());
    }
    fake.leave_out_of_map(&key(1));
    fake.hide_from_listings(&key(1));
    let reader = verifying(&fake, FakeLedger::READER);
    let prefix = format!("v1/events/default/{D}/");
    // The listing agrees with the map the ledger committed to...
    assert_eq!(reader.list(&prefix).unwrap(), [key(0), key(2)]);
    // ...and the map doesn't follow from the log.
    let mut replay = Replay::new();
    for leaf in reader.leaves(0, 1000).unwrap() {
        replay.push(leaf.entry);
    }
    assert!(
        replay
            .anomalies()
            .iter()
            .any(|a| matches!(a, hushrun_tlog::replay::Anomaly::MapMismatch { .. })),
        "{:?}",
        replay.anomalies()
    );
}

// covers: L-11
#[test]
fn a_listing_at_an_older_checkpoint_of_this_history_is_refused() {
    let fake = FakeLedger::start();
    fake.put_object(&key(1), b"one");
    let ledger = verifying(&fake, FakeLedger::WRITER);
    let prefix = format!("v1/events/default/{D}/");
    assert_eq!(ledger.list(&prefix).unwrap(), [key(1)]);
    // The command writes, and verifies the newer checkpoint of its receipt.
    ledger.append(&key(2), b"two").unwrap();
    // A listing read at the checkpoint the command saw first would hide its own write.
    fake.serve_listings_at(Some(1));
    let err = ledger.list(&prefix).unwrap_err();
    assert!(err.is_unavailable(), "{err}");
    assert!(err.to_string().contains("older than"), "{err}");
    fake.serve_listings_at(None);
    assert_eq!(ledger.list(&prefix).unwrap(), [key(1), key(2)]);
}

// covers: L-11, T-9
#[test]
fn what_arrives_before_trust_is_checked_in_the_order_it_came() {
    let fake = FakeLedger::start();
    fake.put_object(&key(1), b"one");
    let ledger = open(&fake, &with_token(FakeLedger::WRITER));
    let prefix = format!("v1/events/default/{D}/");
    // Before hushrun.yaml is read, the command writes, then a listing read at the tree before
    // that write hides it.
    ledger.append(&key(2), b"two").unwrap();
    fake.serve_listings_at(Some(1));
    assert_eq!(ledger.list(&prefix).unwrap(), [key(1)]);
    fake.serve_listings_at(None);
    // Once the key is read, that listing is refused, as it would have been with the key set.
    let err = ledger.configure(Some(log_key(&fake))).unwrap_err();
    assert!(err.is_unavailable(), "{err}");
    assert!(err.to_string().contains("older than"), "{err}");
}

// covers: L-11
#[test]
fn answers_checked_at_once_join_the_newest_history() {
    let fake = FakeLedger::start();
    let ledger = verifying(&fake, FakeLedger::WRITER);
    ledger.append(&key(1), b"one").unwrap();
    // One write's proof comes late, while another write's receipt is checked.
    fake.inject_on(
        Some("GET"),
        Some("log/consistency"),
        Fault::Delay(Duration::from_millis(300)),
        1,
    );
    std::thread::scope(|s| {
        let slow = s.spawn(|| ledger.append(&key(2), b"two"));
        std::thread::sleep(Duration::from_millis(100));
        ledger.append(&key(3), b"three").unwrap();
        slow.join().unwrap().unwrap();
    });
    // The newest checkpoint the command verified never moves back: the late answer is joined
    // to the newer one instead.
    assert_eq!(ledger.checkpoint().unwrap().unwrap().size(), 3);
}
