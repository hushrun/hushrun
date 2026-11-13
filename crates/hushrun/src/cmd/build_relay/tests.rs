//! The relay against the fake build protocol: every row of the outbox's table, and each guard of
//! B-26 to B-34 that a wrong relay would break.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use hushrun_attest::SigningKey;
use hushrun_build::model::{Class as LocalClass, Mode};
use hushrun_build::origin::{Identity, Origin, Vouch, fingerprint};
use hushrun_build::outbox::{Marker, Outbox};
use hushrun_build::relay::{Answer, Request};
use hushrun_build::store::{NewFile, Store};
use hushrun_core::Digest;
use hushrun_core::blocks::{self, Hex32, Pair};
use hushrun_testkit::fake_build::{FakeBuild, route};
use serde_json::json;

use super::{Config, KIND, Relay, Tuning};

/// A machine: a store, and the session folder a check binds its socket in.
struct Machine {
    _dir: tempfile::TempDir,
    store: Store,
    session: PathBuf,
}

fn machine() -> Machine {
    let dir = tempfile::Builder::new().prefix("m").tempdir().unwrap();
    let store = Store::new(&dir.path().join("build"));
    let session = dir.path().join("s");
    std::fs::create_dir_all(&session).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    Machine {
        _dir: dir,
        store,
        session,
    }
}

fn tuning() -> Tuning {
    Tuning {
        open: Duration::from_secs(2),
        lookup: Duration::from_secs(5),
        most_cost: Duration::from_secs(60),
        rest: Duration::from_secs(30),
        end_wait: Duration::from_secs(2),
        close: Duration::from_secs(2),
        ci_open: Duration::from_secs(2),
        ci_trusted_wait: Duration::from_secs(120),
        ci_wait: Duration::from_secs(20),
        downloads: 8,
        tick: Duration::from_millis(5),
        spill: super::pack::IN_MEMORY,
    }
}

fn access(fake: &FakeBuild, token: &str) -> crate::results::BuildAccess {
    crate::results::BuildAccess {
        base: fake.url(),
        host: fake.origin(),
        org: "acme".into(),
        credential: Some(crate::results::BuildCredential::Token(token.into())),
        agent: token.starts_with("hra_"),
        machine: Some(crate::machine::Machine {
            key: SigningKey::generate_p256().unwrap(),
            source: "test".into(),
        }),
        shares: true,
        answered: true,
    }
}

fn start(fake: &FakeBuild, m: &Machine, change: impl FnOnce(&mut Config)) -> Arc<Relay> {
    let mut config = Config {
        store: m.store.clone(),
        access: access(fake, FakeBuild::TOKEN),
        repository: Some("github.com/acme/shop".into()),
        commit: None,
        platform: "aarch64-apple-darwin".into(),
        compiler: None,
        caller: hushrun_build::share::Caller::Person { signed_in: true },
        local: Mode::On,
        own: Mode::On,
        min_free_gb: 0,
        keep_days: 15,
        tuning: tuning(),
        saved: None,
    };
    change(&mut config);
    Arc::new(Relay::start(config).unwrap())
}

fn begin(relay: &Arc<Relay>, m: &Machine) {
    relay.begin_check(&m.session, None, &|_| None).unwrap();
}

/// Waits on `done`, at most 20 seconds.
fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn answering(relay: &Arc<Relay>) -> bool {
    matches!(relay.state().phase, super::Phase::Answering)
}

fn ask(relay: &Arc<Relay>, request: Request) -> Answer {
    relay.runtime.block_on(relay.clone().answer(request))
}

/// A library block in `store`, `size` bytes of `seed`, built here by `by`.
fn block(store: &Store, seed: u8, size: usize, by: Option<Identity>) -> (Digest, Digest) {
    let key = Digest::of(&[seed, 1]);
    let step = Digest::of(&[seed, 2]);
    let dir = store.root().with_file_name(format!("src-{seed}"));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("liba.rlib");
    // Bytes that don't compress to nothing, so sizes on the wire mean something.
    let bytes: Vec<u8> = (0..size)
        .map(|i| Digest::of(&[seed, (i / 32) as u8, (i / 8192) as u8]).as_bytes()[i % 32])
        .collect();
    std::fs::write(&file, &bytes).unwrap();
    let description = json!({"step": step, "class": "library", "repository": "github.com/acme/shop", "rustc_ms": 1000, "built_ms": 1});
    let origin = Origin::built_here("1-1", by);
    store
        .publish_with_origin(
            &key,
            &description,
            &[NewFile {
                name: "liba.rlib".into(),
                source: file,
            }],
            &origin,
            SystemTime::now(),
        )
        .unwrap();
    (key, step)
}

fn me(fake: &FakeBuild) -> Identity {
    Identity {
        host: fake.origin(),
        org: "acme".into(),
        credential: fingerprint(FakeBuild::TOKEN),
    }
}

fn built(key: Digest, step: Digest) -> Request {
    Request::Built {
        key,
        step,
        class: LocalClass::Library,
        portable: true,
        cost_ms: 1000,
    }
}

fn address(key: &Digest) -> Hex32 {
    blocks::address(KIND, key.as_bytes())
}

fn waiting(m: &Machine) -> Vec<Marker> {
    Outbox::new(&m.store).waiting()
}

// covers: B-26
#[test]
fn a_server_that_sleeps_on_open_doesnt_delay_cargo() {
    let fake = FakeBuild::start();
    fake.delay(route::OPEN, Duration::from_secs(10));
    let m = machine();
    let relay = start(&fake, &m, |c| c.tuning.open = Duration::from_secs(10));
    let started = Instant::now();
    begin(&relay, &m);
    // Until `open` answers, every request is a miss, at once.
    let (key, step) = (Digest::of(b"k"), Digest::of(b"s"));
    let answer = ask(
        &relay,
        Request::Fetch {
            step,
            keys: vec![key],
            class: LocalClass::Library,
            local_ms: None,
        },
    );
    assert_eq!(answer, Answer::None);
    assert_eq!(ask(&relay, Request::Hints { step }), Answer::None);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

// covers: B-26, B-28, B-29, B-32
#[test]
fn a_block_built_here_goes_up_and_another_machine_reuses_it() {
    let fake = FakeBuild::start();
    let a = machine();
    let relay = start(&fake, &a, |_| {});
    begin(&relay, &a);
    until("open", || answering(&relay));
    let (key, step) = block(&a.store, 1, 300_000, Some(me(&fake)));
    a.store
        .add_hint(&step, b"{\"hint\":1}", SystemTime::now())
        .unwrap();
    assert_eq!(ask(&relay, built(key, step)), Answer::Ok);
    until("stored", || {
        fake.block(address(&key))
            .is_some_and(|b| b.state == hushrun_testkit::fake_build::BlockState::Stored)
    });
    until("done", || waiting(&a).is_empty());
    let lines = relay.end_check(&[]);
    assert!(lines.is_empty(), "{lines:?}");
    let row = fake.block(address(&key)).unwrap();
    assert_eq!(row.hint_address, Some(address(&step)));
    // Its hint went with it, sealed with the first key.
    let sealed = fake.hints(address(&step));
    assert_eq!(sealed.len(), 1);
    assert_eq!(
        blocks::pack::open_hint(step.as_bytes(), &sealed[0]),
        Some(b"{\"hint\":1}".to_vec())
    );
    // The pack is the same bytes when packed again: what a retry sends.
    let again = super::pack::pack_block(&a.store, &key, &a.session).unwrap();
    assert_eq!(
        again.pack.bytes().unwrap(),
        fake.object(address(&key)).unwrap()
    );

    // Another machine, with an empty store: a wrapper with the key gets it, recorded as a
    // download with the server's word for it.
    let b = machine();
    let relay = start(&fake, &b, |_| {});
    begin(&relay, &b);
    until("open", || answering(&relay));
    let answer = ask(
        &relay,
        Request::Fetch {
            step,
            keys: vec![key],
            class: LocalClass::Library,
            local_ms: None,
        },
    );
    assert_eq!(answer, Answer::Ready { key });
    let block = b.store.block(&key).unwrap().unwrap();
    assert_eq!(
        std::fs::read(&block.files[0].path).unwrap(),
        std::fs::read(a.store.block(&key).unwrap().unwrap().files[0].path.clone()).unwrap()
    );
    let origin = b.store.origin(&key).unwrap();
    assert!(origin.downloaded_only());
    assert_eq!(origin.vouched[0].scope, "person/1");
    // The record in the pack was normalized: no repository, date or compile time of A's.
    assert_eq!(block.description["repository"], "github.com/acme/shop");
    assert_ne!(block.description["built_ms"], 1);
    // A downloaded block never goes up.
    assert_eq!(ask(&relay, built(key, step)), Answer::Ok);
    assert!(waiting(&b).is_empty());
}

// covers: B-29
#[test]
fn every_row_of_the_outbox() {
    struct Row {
        begin: Option<serde_json::Value>,
        commit: Option<&'static str>,
        put_fails: bool,
        /// The marker waits again (true) or is done (false).
        waits: bool,
        discarded: bool,
    }
    let rows = [
        (
            "put, stored",
            Row {
                begin: None,
                commit: None,
                put_fails: false,
                waits: false,
                discarded: false,
            },
        ),
        (
            "have",
            Row {
                begin: Some(json!({"answer":"have","scope":"person/1"})),
                commit: None,
                put_fails: false,
                waits: false,
                discarded: false,
            },
        ),
        (
            "too_large",
            Row {
                begin: Some(json!({"answer":"refused","reason":"too_large"})),
                commit: None,
                put_fails: false,
                waits: false,
                discarded: false,
            },
        ),
        (
            "unsupported",
            Row {
                begin: Some(json!({"answer":"refused","reason":"unsupported"})),
                commit: None,
                put_fails: false,
                waits: false,
                discarded: false,
            },
        ),
        (
            "no_scope",
            Row {
                begin: Some(json!({"answer":"refused","reason":"no_scope"})),
                commit: None,
                put_fails: false,
                waits: false,
                discarded: false,
            },
        ),
        (
            "suspended",
            Row {
                begin: Some(json!({"answer":"refused","reason":"suspended"})),
                commit: None,
                put_fails: false,
                waits: false,
                discarded: false,
            },
        ),
        (
            "differs at begin",
            Row {
                begin: Some(json!({"answer":"refused","reason":"differs"})),
                commit: None,
                put_fails: false,
                waits: false,
                discarded: true,
            },
        ),
        (
            "busy",
            Row {
                begin: Some(json!({"answer":"refused","reason":"busy"})),
                commit: None,
                put_fails: false,
                waits: true,
                discarded: false,
            },
        ),
        (
            "over_allowance",
            Row {
                begin: Some(json!({"answer":"refused","reason":"over_allowance"})),
                commit: None,
                put_fails: false,
                waits: true,
                discarded: false,
            },
        ),
        (
            "too_many_uploads",
            Row {
                begin: Some(json!({"answer":"refused","reason":"too_many_uploads"})),
                commit: None,
                put_fails: false,
                waits: true,
                discarded: false,
            },
        ),
        (
            "a refusal this version doesn't know",
            Row {
                begin: Some(json!({"answer":"refused","reason":"new_word"})),
                commit: None,
                put_fails: false,
                waits: true,
                discarded: false,
            },
        ),
        (
            "an answer this version doesn't know",
            Row {
                begin: Some(json!({"answer":"later"})),
                commit: None,
                put_fails: false,
                waits: true,
                discarded: false,
            },
        ),
        (
            "commit missing",
            Row {
                begin: None,
                commit: Some("missing"),
                put_fails: false,
                waits: true,
                discarded: false,
            },
        ),
        (
            "commit gone",
            Row {
                begin: None,
                commit: Some("gone"),
                put_fails: false,
                waits: true,
                discarded: false,
            },
        ),
        (
            "commit unknown",
            Row {
                begin: None,
                commit: Some("later"),
                put_fails: false,
                waits: true,
                discarded: false,
            },
        ),
        (
            "commit differs",
            Row {
                begin: None,
                commit: Some("differs"),
                put_fails: false,
                waits: false,
                discarded: true,
            },
        ),
        (
            "an expired link",
            Row {
                begin: None,
                commit: None,
                put_fails: true,
                waits: true,
                discarded: false,
            },
        ),
    ];
    for (name, row) in rows {
        let fake = FakeBuild::start();
        let m = machine();
        let (key, step) = block(&m.store, 7, 10_000, Some(me(&fake)));
        if let Some(answer) = &row.begin {
            fake.force_begin(address(&key), answer.clone(), 1);
        }
        if let Some(word) = row.commit {
            fake.force_commit(address(&key), word, 1);
        }
        if row.put_fails {
            fake.fail(route::STORE_PUT, 403, 1);
        }
        let relay = start(&fake, &m, |_| {});
        begin(&relay, &m);
        until("open", || answering(&relay));
        ask(&relay, built(key, step));
        let tried = |fake: &FakeBuild| {
            fake.count(route::BEGIN) >= 1
                && (row.begin.is_some() || fake.count(route::STORE_PUT) >= 1)
                && (row.begin.is_some() || row.put_fails || fake.count(route::COMMIT) >= 1)
        };
        until(name, || {
            tried(&fake)
                && Outbox::new(&m.store).waiting().len() == usize::from(row.waits)
                && no_claims(&m)
        });
        relay.end_check(&[]);
        let left = waiting(&m);
        assert_eq!(left.len(), usize::from(row.waits), "{name}");
        if row.waits {
            assert!(left[0].not_before_ms > 0, "{name}: waits with a date");
        }
        assert_eq!(
            m.store.block(&key).unwrap().is_none(),
            row.discarded,
            "{name}: the local block"
        );
    }
}

fn no_claims(m: &Machine) -> bool {
    let claimed = m.store.root().join("outbox/claimed");
    std::fs::read_dir(&claimed).map_or(true, |runs| {
        runs.flatten()
            .all(|run| std::fs::read_dir(run.path()).map_or(true, |mut e| e.next().is_none()))
    })
}

// covers: B-29
#[test]
fn a_put_the_end_of_the_command_cuts_off_counts_a_cut_off_and_the_third_drops_it() {
    let fake = FakeBuild::start();
    let m = machine();
    let (key, step) = block(&m.store, 9, 400_000, Some(me(&fake)));
    // 400 kB at 20 kB/s: the PUT can't finish in the end's wait.
    fake.throttle_store(20_000);
    for n in 1..=3u32 {
        let relay = start(&fake, &m, |c| {
            c.tuning.end_wait = Duration::from_millis(200)
        });
        begin(&relay, &m);
        until("open", || answering(&relay));
        if n == 1 {
            ask(&relay, built(key, step));
        }
        until("put under way", || !relay.state().puts.is_empty());
        relay.end_check(&[]);
        let left = waiting(&m);
        if n < 3 {
            assert_eq!(left.len(), 1);
            assert_eq!(left[0].cutoffs, n);
        } else {
            assert!(left.is_empty(), "the third cut-off drops the marker");
        }
    }
}

// covers: B-29
#[test]
fn killing_the_relay_after_begin_and_running_again_ends_with_the_block_stored() {
    let fake = FakeBuild::start();
    let m = machine();
    let (key, step) = block(&m.store, 11, 50_000, Some(me(&fake)));
    let marker = Marker::new(
        key,
        step,
        LocalClass::Library,
        1000,
        me(&fake),
        None,
        SystemTime::now(),
    );
    let outbox = Outbox::new(&m.store);
    outbox.add(&marker).unwrap();
    // A relay that died after `begin`: its claim is left, and the server's row is uploading.
    let dead = "1-999999999";
    outbox.claim(dead, &key).unwrap().unwrap();
    let packed = super::pack::pack_block(&m.store, &key, &m.session).unwrap();
    let body = blocks::BeginRequest {
        kind: KIND.into(),
        blocks: vec![blocks::BeginBlock {
            address: address(&key),
            hint_address: Some(address(&step)),
            bytes: packed.sealed.bytes,
            pack_sha256: packed.sealed.sha256,
            outputs: packed.outputs,
            class: blocks::Class::Library,
            cost_ms: 1000,
        }],
        repository: None,
        commit: None,
    };
    let client = super::http::Client::new(
        &fake.url(),
        super::credential::Credential::Token(FakeBuild::TOKEN.into()),
    )
    .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let answered: super::http::Answered<blocks::BeginResponse> = rt
        .block_on(client.post(
            blocks::BEGIN_PATH,
            &body,
            Duration::from_secs(5),
            Some("sha256:00 AAAA".into()),
        ))
        .unwrap();
    assert!(matches!(
        answered.body.blocks[0].answer,
        blocks::Answer::Put { .. }
    ));
    // The next run takes the claim back, packs the same bytes, and the server takes them.
    let relay = start(&fake, &m, |_| {});
    begin(&relay, &m);
    until("stored", || {
        fake.block(address(&key))
            .is_some_and(|b| b.state == hushrun_testkit::fake_build::BlockState::Stored)
    });
    until("done", || waiting(&m).is_empty() && no_claims(&m));
    relay.end_check(&[]);
}

// covers: B-29, B-32
#[test]
fn a_marker_goes_up_only_from_the_host_org_and_credential_that_built_it() {
    let fake = FakeBuild::start();
    let m = machine();
    // Built in another org, under an agent token, and only downloaded: none goes up.
    let other_org = Identity {
        org: "other".into(),
        ..me(&fake)
    };
    let agent = Identity {
        credential: fingerprint(FakeBuild::AGENT_TOKEN),
        ..me(&fake)
    };
    let (k1, s1) = block(&m.store, 21, 5_000, Some(other_org.clone()));
    let (k2, s2) = block(&m.store, 22, 5_000, Some(agent.clone()));
    let (k3, s3) = (Digest::of(b"downloaded"), Digest::of(b"downloaded step"));
    {
        let src = m.store.root().with_file_name("dl");
        std::fs::write(&src, b"bytes").unwrap();
        m.store
            .publish_with_origin(
                &k3,
                &json!({"step": s3}),
                &[NewFile {
                    name: "liba.rlib".into(),
                    source: src,
                }],
                &Origin::downloaded(Vouch {
                    host: fake.origin(),
                    org: "acme".into(),
                    repository: String::new(),
                    scope: "person/2".into(),
                    outputs: Hex32([0; 32]),
                    date_ms: 0,
                }),
                SystemTime::now(),
            )
            .unwrap();
    }
    // Markers an earlier run left in other orgs and under the agent wait in the outbox.
    let outbox = Outbox::new(&m.store);
    outbox
        .add(&Marker::new(
            k1,
            s1,
            LocalClass::Library,
            1,
            other_org,
            None,
            SystemTime::now(),
        ))
        .unwrap();
    outbox
        .add(&Marker::new(
            k2,
            s2,
            LocalClass::Library,
            1,
            agent,
            None,
            SystemTime::now(),
        ))
        .unwrap();
    let relay = start(&fake, &m, |_| {});
    begin(&relay, &m);
    until("open", || answering(&relay));
    for (k, s) in [(k1, s1), (k2, s2), (k3, s3)] {
        ask(&relay, built(k, s));
    }
    // One of ours, sent, shows the loop ran past them.
    let (k4, s4) = block(&m.store, 24, 5_000, Some(me(&fake)));
    ask(&relay, built(k4, s4));
    until("ours stored", || {
        fake.block(address(&k4))
            .is_some_and(|b| b.state == hushrun_testkit::fake_build::BlockState::Stored)
    });
    until("ours done", || no_claims(&m));
    relay.end_check(&[]);
    let begun: Vec<Hex32> = fake
        .begins()
        .iter()
        .flat_map(|b| b.blocks.iter().map(|b| b.address))
        .collect();
    assert_eq!(begun, vec![address(&k4)]);
    assert_eq!(waiting(&m).len(), 2, "the other org's and the agent's stay");
}

// covers: B-30
#[test]
fn a_warm_machine_downloads_nothing() {
    let fake = FakeBuild::start();
    let m = machine();
    let (key, step) = block(&m.store, 31, 50_000, Some(me(&fake)));
    let packed = super::pack::pack_block(&m.store, &key, &m.session).unwrap();
    fake.put_block(
        address(&key),
        "person/1",
        &packed.pack.bytes().unwrap(),
        packed.outputs,
        1000,
        blocks::Class::Library,
    );
    m.store.add_hint(&step, b"hint", SystemTime::now()).unwrap();
    fake.set_manifest(vec![Pair(address(&step), address(&key))]);
    let relay = start(&fake, &m, |_| {});
    begin(&relay, &m);
    until("open", || answering(&relay));
    relay.end_check(&[]);
    assert_eq!(fake.count(route::LOOKUP), 0);
    assert_eq!(fake.count(route::HINTS_LOOKUP), 0);
    assert_eq!(fake.served_bytes(), 0);
}

// covers: B-30
#[test]
fn a_cold_machine_prefetches_the_manifest_s_libraries_and_opens_them_when_asked() {
    let fake = FakeBuild::start();
    let a = machine();
    let (key, step) = block(&a.store, 41, 200_000, Some(me(&fake)));
    let packed = super::pack::pack_block(&a.store, &key, &a.session).unwrap();
    fake.put_block(
        address(&key),
        "person/1",
        &packed.pack.bytes().unwrap(),
        packed.outputs,
        5000,
        blocks::Class::Library,
    );
    let hint = blocks::pack::seal_hint(step.as_bytes(), b"{\"a hint\":1}").unwrap();
    fake.seed_hint(address(&step), &hint);
    fake.set_manifest(vec![Pair(address(&step), address(&key))]);
    let b = machine();
    let relay = start(&fake, &b, |_| {});
    begin(&relay, &b);
    until("open", || answering(&relay));
    // C4: nothing is fetched until a wrapper misses.
    assert_eq!(fake.count(route::HINTS_LOOKUP), 0);
    assert_eq!(fake.count(route::LOOKUP), 0);
    // The first wrapper that misses starts it, and its answer comes from the prefetch's.
    assert_eq!(
        ask(&relay, Request::Hints { step }),
        Answer::Hints { count: 1 }
    );
    until("prefetched", || {
        relay
            .state()
            .fetches
            .get(&address(&key))
            .is_some_and(|f| matches!(f.state, super::Download::Arrived(_)))
    });
    assert_eq!(fake.count(route::HINTS_LOOKUP), 1);
    // Nothing is in the store until a wrapper brings the key.
    assert!(b.store.block(&key).unwrap().is_none());
    assert_eq!(
        b.store.shared_hints(&step),
        vec![b"{\"a hint\":1}".to_vec()]
    );
    assert_eq!(
        fake.count(route::HINTS_LOOKUP),
        1,
        "the prefetch's answer served it"
    );
    let answer = ask(
        &relay,
        Request::Fetch {
            step,
            keys: vec![key],
            class: LocalClass::Library,
            local_ms: None,
        },
    );
    assert_eq!(answer, Answer::Ready { key });
    assert!(b.store.block(&key).unwrap().is_some());
    relay.end_check(&[]);
}

// covers: B-32
#[test]
fn a_second_run_s_download_of_a_block_already_downloaded_leaves_its_folder_in_place() {
    // Two runs on one store fetch one block at once: the second publish found the first's
    // block there and replaced it, moving the folder away while the first run's wrapper copied
    // from it; that compile then ran as asked, without the remaps (the review's B6). The same
    // outputs now stay where they are, with the second run's vouch.
    use std::os::unix::fs::MetadataExt as _;
    let fake = FakeBuild::start();
    let a = machine();
    let (key, step) = block(&a.store, 43, 50_000, Some(me(&fake)));
    let packed = super::pack::pack_block(&a.store, &key, &a.session).unwrap();
    fake.put_block(
        address(&key),
        "person/1",
        &packed.pack.bytes().unwrap(),
        packed.outputs,
        5000,
        blocks::Class::Library,
    );
    let b = machine();
    let fetch = |relay: &Arc<Relay>| {
        ask(
            relay,
            Request::Fetch {
                step,
                keys: vec![key],
                class: LocalClass::Library,
                local_ms: None,
            },
        )
    };
    let first = start(&fake, &b, |_| {});
    begin(&first, &b);
    until("open", || answering(&first));
    assert_eq!(fetch(&first), Answer::Ready { key });
    let folder = |store: &Store| {
        let block = store.block(&key).unwrap().unwrap();
        std::fs::metadata(&block.files[0].path).unwrap().ino()
    };
    let before = folder(&b.store);
    let vouched = b.store.origin(&key).unwrap().vouched[0].date_ms;
    first.end_check(&[]);
    // The second run looked the block up before the first published it, as at once.
    let second = start(&fake, &b, |_| {});
    begin(&second, &b);
    until("open", || answering(&second));
    assert_eq!(fetch(&second), Answer::Ready { key });
    assert_eq!(folder(&b.store), before, "the block's files were moved");
    let origin = b.store.origin(&key).unwrap();
    assert!(origin.downloaded_only());
    assert!(origin.vouched[0].date_ms >= vouched);
    second.end_check(&[]);
}

// covers: B-33
#[test]
fn in_observe_nothing_is_published_and_a_difference_is_counted_reported_and_suspends() {
    let fake = FakeBuild::start();
    fake.set_mode(blocks::Mode::Observe);
    let other = machine();
    let (key, step) = block(&other.store, 51, 20_000, None);
    let theirs = super::pack::pack_block(&other.store, &key, &other.session).unwrap();
    fake.put_block(
        address(&key),
        "person/9",
        &theirs.pack.bytes().unwrap(),
        theirs.outputs,
        1000,
        blocks::Class::Library,
    );
    // It writes nowhere: what is under test is the comparison, not an upload.
    fake.set_scopes(None, &["person/1", "person/9"]);
    // This machine built the same key with other bytes.
    let m = machine();
    let (key_here, _) = {
        let k = block(&m.store, 52, 20_000, Some(me(&fake)));
        // Move it under the other machine's key: same key, other outputs.
        let b = m.store.block(&k.0).unwrap().unwrap();
        let src = m.store.root().with_file_name("mine.rlib");
        std::fs::copy(&b.files[0].path, &src).unwrap();
        m.store
            .publish_with_origin(
                &key,
                &json!({"step": step}),
                &[NewFile {
                    name: "liba.rlib".into(),
                    source: src,
                }],
                &Origin::built_here("1-1", Some(me(&fake))),
                SystemTime::now(),
            )
            .unwrap();
        k
    };
    let _ = key_here;
    let relay = start(&fake, &m, |_| {});
    begin(&relay, &m);
    until("open", || answering(&relay));
    // A wrapper never waits for a download in observe.
    let answer = ask(
        &relay,
        Request::Fetch {
            step,
            keys: vec![key],
            class: LocalClass::Library,
            local_ms: None,
        },
    );
    assert_eq!(answer, Answer::None);
    ask(&relay, built(key, step));
    until("compared", || relay.state().tally.compared == 1);
    until("reported", || !fake.reports().is_empty());
    let tally = relay.state().tally.clone();
    assert_eq!((tally.compared_others, tally.compared_differed), (1, 1));
    let report = &fake.reports()[0];
    assert_eq!(
        (report.scope.as_str(), report.problem),
        ("person/9", blocks::Problem::Differs)
    );
    assert!(m.store.suspended_at(&step, SystemTime::now()).is_some());
    // No file in the store came from the pack.
    let here = m.store.block(&key).unwrap().unwrap();
    assert_ne!(*here.files[0].sha256.as_bytes(), theirs.outputs.0);
    relay.end_check(&[]);
    let closes = fake.closes();
    assert_eq!(closes.len(), 1);
    assert_eq!(
        (
            closes[0].counts.compared,
            closes[0].counts.compared_others,
            closes[0].counts.compared_differed
        ),
        (1, 1, 1)
    );
}

/// A store holding `n` libraries of `size` bytes, each in the fake's object store too.
fn served(fake: &FakeBuild, n: u8, size: usize, cost_ms: u32) -> Vec<(Digest, Digest)> {
    let src = machine();
    (0..n)
        .map(|i| {
            let (key, step) = block(&src.store, 100 + i, size, None);
            let packed = super::pack::pack_block(&src.store, &key, &src.session).unwrap();
            fake.put_block(
                address(&key),
                "person/1",
                &packed.pack.bytes().unwrap(),
                packed.outputs,
                cost_ms,
                blocks::Class::Library,
            );
            (key, step)
        })
        .collect()
}

// covers: B-26
#[test]
fn on_a_slow_link_with_no_manifest_a_build_takes_at_most_a_tenth_longer() {
    // 5 Mbit/s, crates of 300 ms that pack to about 1 MB, as a fast laptop makes them.
    let fake = FakeBuild::start();
    fake.throttle_store(625_000);
    let crates = served(&fake, 8, 1_000_000, 300);
    let m = machine();
    let relay = start(&fake, &m, |_| {});
    begin(&relay, &m);
    until("open", || answering(&relay));
    let compile = Duration::from_millis(300);
    let started = Instant::now();
    for (key, step) in &crates {
        let answer = ask(
            &relay,
            Request::Fetch {
                step: *step,
                keys: vec![*key],
                class: LocalClass::Library,
                local_ms: None,
            },
        );
        if answer == Answer::None {
            std::thread::sleep(compile);
        }
    }
    let took = started.elapsed();
    let off = compile * crates.len() as u32;
    relay.end_check(&[]);
    assert!(
        took <= off + off / 10,
        "{took:?} against {off:?} with sharing off"
    );
    // A fetch expected to lose is never started: only the first, the measurement, went out.
    assert_eq!(fake.count(route::STORE_GET), 1);
}

// covers: B-26
#[test]
fn a_block_whose_stated_cost_is_absurd_waits_at_most_the_ceiling() {
    let fake = FakeBuild::start();
    fake.throttle_store(10_000);
    let crates = served(&fake, 1, 500_000, u32::MAX);
    let m = machine();
    let relay = start(&fake, &m, |c| {
        c.tuning.most_cost = Duration::from_millis(600)
    });
    begin(&relay, &m);
    until("open", || answering(&relay));
    let (key, step) = crates[0];
    let started = Instant::now();
    let answer = ask(
        &relay,
        Request::Fetch {
            step,
            keys: vec![key],
            class: LocalClass::Library,
            local_ms: None,
        },
    );
    assert_eq!(answer, Answer::None);
    assert!(
        started.elapsed() < Duration::from_millis(600),
        "{:?}",
        started.elapsed()
    );
    relay.end_check(&[]);
}

// covers: B-26
#[test]
fn three_failures_rest_the_relay_and_a_refusal_stops_it_for_the_run() {
    let fake = FakeBuild::start();
    let crates = served(&fake, 4, 10_000, 5000);
    fake.fail(route::STORE_GET, 500, 3);
    let m = machine();
    let relay = start(&fake, &m, |_| {});
    begin(&relay, &m);
    until("open", || answering(&relay));
    for (key, step) in &crates[..3] {
        let _ = ask(
            &relay,
            Request::Fetch {
                step: *step,
                keys: vec![*key],
                class: LocalClass::Library,
                local_ms: None,
            },
        );
    }
    until("resting", || {
        matches!(relay.state().phase, super::Phase::Resting(_))
    });
    let (key, step) = crates[3];
    let started = Instant::now();
    let answer = ask(
        &relay,
        Request::Fetch {
            step,
            keys: vec![key],
            class: LocalClass::Library,
            local_ms: None,
        },
    );
    assert_eq!(answer, Answer::None);
    assert!(started.elapsed() < Duration::from_millis(200));
    relay.end_check(&[]);

    // A 401 in the middle of a run: nothing more is asked, markers stay, one line says so.
    let fake = FakeBuild::start();
    let m = machine();
    let relay = start(&fake, &m, |_| {});
    begin(&relay, &m);
    until("open", || answering(&relay));
    fake.fail(route::BEGIN, 401, 10);
    let (key, step) = block(&m.store, 61, 5_000, Some(me(&fake)));
    ask(&relay, built(key, step));
    until("stopped", || {
        matches!(relay.state().phase, super::Phase::Stopped(_))
    });
    let lines = relay.end_check(&[]);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("sharing stopped for this run")),
        "{lines:?}"
    );
    assert_eq!(waiting(&m).len(), 1);
}

// covers: B-34
#[test]
fn a_withdrawn_block_is_not_vouched_for_and_an_unreachable_server_is_offline() {
    let fake = FakeBuild::start();
    let crates = served(&fake, 1, 10_000, 1000);
    let (key, step) = crates[0];
    let b = machine();
    let relay = start(&fake, &b, |_| {});
    begin(&relay, &b);
    until("open", || answering(&relay));
    assert_eq!(
        ask(
            &relay,
            Request::Fetch {
                step,
                keys: vec![key],
                class: LocalClass::Library,
                local_ms: None
            }
        ),
        Answer::Ready { key }
    );
    let outputs = b.store.origin(&key).unwrap().vouched[0].outputs;
    relay.end_check(&[]);
    fake.withdraw(address(&key));
    let relay = start(&fake, &b, |_| {});
    begin(&relay, &b);
    until("open", || answering(&relay));
    assert_eq!(
        ask(&relay, Request::Vouch { key, step, outputs }),
        Answer::None
    );
    relay.end_check(&[]);
    // Unreachable: offline, so an entry of any age counts.
    let gone = FakeBuild::start();
    let m = machine();
    let relay = start(&gone, &m, |c| {
        c.access.base = "http://127.0.0.1:9/o/acme".into();
        c.tuning.open = Duration::from_millis(300);
    });
    begin(&relay, &m);
    until("stopped", || {
        matches!(relay.state().phase, super::Phase::Stopped(_))
    });
    assert_eq!(
        ask(&relay, Request::Vouch { key, step, outputs }),
        Answer::Offline
    );
    let lines = relay.end_check(&[]);
    assert!(
        lines.iter().any(|l| l.contains("can't be reached")),
        "{lines:?}"
    );
}

// covers: B-26
#[test]
fn a_long_session_folder_gets_its_socket_in_a_private_folder_and_a_wrapper_reaches_it() {
    let fake = FakeBuild::start();
    let m = machine();
    let long = m.session.join("x".repeat(80));
    std::fs::create_dir_all(&long).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&long, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let relay = start(&fake, &m, |_| {});
    let share = relay.begin_check(&long, None, &|_| None).unwrap();
    assert!(!share.socket.starts_with(&long));
    assert!(share.socket.as_os_str().len() <= hushrun_build::relay::MOST_SOCKET_PATH);
    until("open", || answering(&relay));
    let step = Digest::of(b"none");
    let answer = hushrun_build::relay::ask(
        &share.socket,
        &Request::Hints { step },
        Duration::from_secs(5),
    );
    assert_eq!(answer, Some(Answer::None));
    relay.end_check(&[]);
    assert!(!Path::new(&share.socket).exists());
}

// covers: B-5, B-24, B-29
#[test]
fn a_block_that_may_not_leave_the_machine_gets_no_marker() {
    // What the wrapper says of a block built for target-cpu=native, or holding an unkeyed
    // value: not portable.
    let fake = FakeBuild::start();
    let m = machine();
    let relay = start(&fake, &m, |_| {});
    begin(&relay, &m);
    until("open", || answering(&relay));
    let (key, step) = block(&m.store, 71, 5_000, Some(me(&fake)));
    let answer = ask(
        &relay,
        Request::Built {
            key,
            step,
            class: LocalClass::Library,
            portable: false,
            cost_ms: 1000,
        },
    );
    assert_eq!(answer, Answer::Ok);
    relay.end_check(&[]);
    assert!(waiting(&m).is_empty());
    assert_eq!(fake.count(route::BEGIN), 0);
}

// covers: B-28, B-29
#[test]
fn a_pack_larger_than_memory_holds_goes_up_streamed_from_its_file() {
    let fake = FakeBuild::start();
    let m = machine();
    // 3 MB of bytes that don't compress, against 1 MiB held in memory: both passes spill.
    let (key, step) = block(&m.store, 81, 3_000_000, Some(me(&fake)));
    let relay = start(&fake, &m, |c| c.tuning.spill = 1 << 20);
    begin(&relay, &m);
    until("open", || answering(&relay));
    ask(&relay, built(key, step));
    until("stored", || {
        fake.block(address(&key))
            .is_some_and(|b| b.state == hushrun_testkit::fake_build::BlockState::Stored)
    });
    relay.end_check(&[]);
    let packed = super::pack::pack_block(&m.store, &key, &m.session).unwrap();
    assert!(packed.sealed.bytes > 1 << 20);
    assert_eq!(
        fake.object(address(&key)).unwrap(),
        packed.pack.bytes().unwrap()
    );
    // The spilled files went with the upload.
    let packs = m.store.root().join("outbox/tmp");
    let left: Vec<_> = walk(&packs);
    assert!(left.is_empty(), "{left:?}");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .flat_map(|e| {
            if e.path().is_dir() {
                walk(&e.path())
            } else {
                vec![e.path()]
            }
        })
        .collect()
}

// covers: B-26
#[cfg(unix)]
#[test]
fn open_names_the_platform_of_the_compiler_s_host() {
    let fake = FakeBuild::start();
    let m = machine();
    // A stand-in compiler whose host is not the one hushrun was built for.
    let rustc = m.session.join("rustc");
    std::fs::write(
        &rustc,
        "#!/bin/sh\nprintf 'rustc 1.94.0\\nhost: x86_64-unknown-linux-gnu\\n'\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let relay = start(&fake, &m, |c| {
        c.compiler = Some((rustc.clone().into_os_string(), m.session.clone()));
    });
    begin(&relay, &m);
    until("open", || answering(&relay));
    relay.end_check(&[]);
    assert_eq!(fake.opens()[0].platform, "x86_64-unknown-linux-gnu");
    assert_eq!(fake.closes()[0].platform, "x86_64-unknown-linux-gnu");
    // Unaskable: the target hushrun was built for, as the config names it.
    let fake = FakeBuild::start();
    let m = machine();
    let relay = start(&fake, &m, |c| {
        c.compiler = Some(("/home/dev/no-such-rustc".into(), m.session.clone()));
    });
    begin(&relay, &m);
    until("open", || answering(&relay));
    relay.end_check(&[]);
    assert_eq!(fake.opens()[0].platform, "aarch64-apple-darwin");
}

// covers: B-26, B-31
#[test]
fn every_request_carries_the_workspace_s_commit_and_an_untrusted_job_says_why_once() {
    let fake = FakeBuild::start();
    fake.set_policy(|p| {
        p.untrusted = Some(blocks::Untrusted::Environment);
        p.writes = false;
        p.write = None;
    });
    let crates = served(&fake, 1, 10_000, 1000);
    let (key, step) = crates[0];
    let m = machine();
    let commit = "c".repeat(40);
    let relay = start(&fake, &m, |c| c.commit = Some(commit.clone()));
    begin(&relay, &m);
    until("open", || answering(&relay));
    ask(&relay, Request::Hints { step });
    ask(
        &relay,
        Request::Fetch {
            step,
            keys: vec![key],
            class: LocalClass::Library,
            local_ms: None,
        },
    );
    let lines = relay.end_check(&[]);
    let says: Vec<&String> = lines.iter().filter(|l| l.contains("writes none")).collect();
    assert_eq!(says.len(), 1, "{lines:?}");
    assert!(
        says[0].contains("it names a deployment environment"),
        "{lines:?}"
    );
    let mut routes = Vec::new();
    for seen in fake.seen() {
        if [
            route::OPEN,
            route::HINTS_LOOKUP,
            route::LOOKUP,
            route::CLOSE,
        ]
        .contains(&seen.route.as_str())
        {
            let body: serde_json::Value = serde_json::from_slice(&seen.body).unwrap();
            assert_eq!(body["commit"], commit, "{}", seen.route);
            routes.push(seen.route.clone());
        }
    }
    for route in [
        route::OPEN,
        route::HINTS_LOOKUP,
        route::LOOKUP,
        route::CLOSE,
    ] {
        assert!(
            routes.iter().any(|r| r == route),
            "no {route} in {routes:?}"
        );
    }
}

fn with_id_token(fake: &FakeBuild) -> impl FnOnce(&mut Config) + '_ {
    move |c: &mut Config| {
        c.access.credential = Some(crate::results::BuildCredential::GithubOidc {
            url: fake.id_token_url(),
            request_token: FakeBuild::ID_TOKEN_REQUEST.into(),
        });
        c.access.agent = false;
        c.access.machine = None;
        c.caller = hushrun_build::share::Caller::Ci {
            id_token: true,
            agent_token: false,
        };
    }
}

fn bearers(fake: &FakeBuild) -> Vec<String> {
    fake.seen()
        .iter()
        .filter(|s| s.path.contains("/v0/build/"))
        .map(|s| s.authorization.clone().unwrap_or_default())
        .collect()
}

// covers: B-31
#[test]
fn in_ci_every_build_request_carries_the_job_s_id_token_and_it_is_minted_again_before_it_expires() {
    // A token that lives ten minutes serves every request.
    let fake = FakeBuild::start();
    fake.set_id_token_seconds(600);
    let m = machine();
    let relay = start(&fake, &m, with_id_token(&fake));
    begin(&relay, &m);
    until("open", || answering(&relay));
    let (key, step) = block(&m.store, 91, 5_000, None);
    ask(&relay, Request::Hints { step });
    let _ = key;
    relay.end_check(&[]);
    let tokens = fake.id_tokens();
    assert_eq!(tokens.len(), 1);
    let sent = bearers(&fake);
    assert!(sent.len() >= 3, "{sent:?}");
    assert!(
        sent.iter().all(|b| *b == format!("Bearer {}", tokens[0])),
        "{sent:?}"
    );
    // Its audience is the backend's URL.
    assert!(
        fake.audiences()[0].contains("127.0.0.1"),
        "{:?}",
        fake.audiences()
    );

    // One within a minute of expiring is replaced before the next request.
    let fake = FakeBuild::start();
    fake.set_id_token_seconds(30);
    let m = machine();
    let relay = start(&fake, &m, with_id_token(&fake));
    begin(&relay, &m);
    until("open", || answering(&relay));
    ask(
        &relay,
        Request::Hints {
            step: Digest::of(b"s"),
        },
    );
    relay.end_check(&[]);
    let sent = bearers(&fake);
    let tokens = fake.id_tokens();
    assert_eq!(tokens.len(), sent.len(), "a fresh one each time: {sent:?}");
    assert_eq!(sent[0], format!("Bearer {}", tokens[0]));
    assert_eq!(sent[1], format!("Bearer {}", tokens[1]));
}

// covers: B-31
#[test]
fn a_401_gets_a_fresh_id_token_and_one_more_try() {
    let fake = FakeBuild::start();
    fake.set_id_token_seconds(600);
    fake.fail(route::OPEN, 401, 1);
    let m = machine();
    let relay = start(&fake, &m, with_id_token(&fake));
    begin(&relay, &m);
    until("open", || answering(&relay));
    assert_eq!(fake.count(route::OPEN), 2);
    assert_eq!(fake.id_tokens().len(), 2);
    relay.end_check(&[]);
    // A person's token isn't minted: a 401 stops the run's sharing as before.
    let fake = FakeBuild::start();
    fake.fail(route::OPEN, 401, 1);
    let m = machine();
    let relay = start(&fake, &m, |_| {});
    begin(&relay, &m);
    until("stopped", || {
        matches!(relay.state().phase, super::Phase::Stopped(_))
    });
    assert_eq!(fake.count(route::OPEN), 1);
    relay.end_check(&[]);
}

// covers: B-31
#[test]
fn a_caller_the_policy_lets_write_nothing_or_ci_without_an_id_token_uploads_nothing() {
    // An agent token the policy gives no scope to write, and an agent token in CI outside
    // GitHub Actions, which reads only though the policy says it writes.
    for (agent_writes, uploads) in [(false, true), (true, false)] {
        let fake = FakeBuild::start();
        if !agent_writes {
            fake.set_scopes(None, &["protected/acme/shop"]);
        }
        let m = machine();
        let relay = start(&fake, &m, |c| {
            c.access.credential = Some(crate::results::BuildCredential::Token(
                FakeBuild::AGENT_TOKEN.into(),
            ));
            c.access.agent = true;
            c.caller = if uploads {
                hushrun_build::share::Caller::Agent { sandbox: false }
            } else {
                hushrun_build::share::Caller::Ci {
                    id_token: false,
                    agent_token: true,
                }
            };
        });
        begin(&relay, &m);
        until("open", || answering(&relay));
        let (key, step) = block(&m.store, 7, 2_000, None);
        assert_eq!(ask(&relay, built(key, step)), Answer::Ok);
        until("the upload loop ended", || !relay.state().uploading);
        relay.end_check(&[]);
        assert_eq!(
            fake.count(route::BEGIN),
            0,
            "writes {agent_writes}, uploads {uploads}"
        );
    }
}

// covers: B-2
#[test]
fn a_ci_job_waits_for_open_at_most_its_wait_and_never_holds_the_relay_meanwhile() {
    let fake = FakeBuild::start();
    let m = machine();
    let relay = start(&fake, &m, with_id_token(&fake));
    begin(&relay, &m);
    let started = Instant::now();
    assert_eq!(
        relay.wait_open(Duration::from_secs(5)),
        super::Opened::Answering
    );
    // `open` itself is quick: the wait took no more than it.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    relay.end_check(&[]);

    let fake = FakeBuild::start();
    fake.delay(route::OPEN, Duration::from_secs(3));
    let m = machine();
    let relay = start(&fake, &m, with_id_token(&fake));
    begin(&relay, &m);
    let started = Instant::now();
    assert_eq!(
        relay.wait_open(Duration::from_millis(500)),
        super::Opened::Unanswered
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    relay.end_check(&[]);

    let fake = FakeBuild::start();
    fake.set_sharing(false);
    let m = machine();
    let relay = start(&fake, &m, with_id_token(&fake));
    begin(&relay, &m);
    assert_eq!(relay.wait_open(Duration::from_secs(5)), super::Opened::Off);
    relay.end_check(&[]);
}

fn built_costing(key: Digest, step: Digest, cost_ms: u64) -> Request {
    Request::Built {
        key,
        step,
        class: LocalClass::Library,
        portable: true,
        cost_ms,
    }
}

fn job_identity(fake: &FakeBuild, relay: &Arc<Relay>) -> Identity {
    let _ = fake;
    relay.by.clone()
}

// covers: B-31
#[test]
fn a_ci_job_s_end_sends_what_it_built_last_and_waits_no_longer_than_its_context_allows() {
    // What the job built just before its command ended still goes up: its store goes with it.
    let fake = FakeBuild::start();
    let m = machine();
    let relay = start(&fake, &m, with_id_token(&fake));
    begin(&relay, &m);
    until("open", || answering(&relay));
    let by = job_identity(&fake, &relay);
    let (key, step) = block(&m.store, 61, 5_000, Some(by.clone()));
    assert_eq!(ask(&relay, built_costing(key, step, 40_000)), Answer::Ok);
    let lines = relay.end_check(&[]);
    assert!(
        fake.block(address(&key))
            .is_some_and(|b| b.state == hushrun_testkit::fake_build::BlockState::Stored),
        "{lines:?}"
    );
    assert!(waiting(&m).is_empty(), "{lines:?}");

    // A stalled upload: the wait is a quarter of what the block's compile took (4 s here), and
    // the job's own most (20 s, or a trusted job's 2 minutes) never comes into it.
    for trusted in [false, true] {
        let fake = FakeBuild::start();
        if !trusted {
            fake.set_policy(|p| p.untrusted = Some(blocks::Untrusted::Event));
        }
        fake.delay(route::STORE_PUT, Duration::from_secs(10));
        let m = machine();
        let relay = start(&fake, &m, with_id_token(&fake));
        begin(&relay, &m);
        until("open", || answering(&relay));
        let by = job_identity(&fake, &relay);
        let (key, step) = block(&m.store, 62, 5_000, Some(by));
        assert_eq!(ask(&relay, built_costing(key, step, 4_000)), Answer::Ok);
        until("its PUT started", || fake.count(route::STORE_PUT) > 0);
        let started = Instant::now();
        relay.end_check(&[]);
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(900),
            "trusted {trusted}: {took:?}"
        );
        assert!(took < Duration::from_secs(3), "trusted {trusted}: {took:?}");
    }

    // A job that isn't trusted waits at most its 20 seconds (shortened to 1 here) whatever the
    // blocks cost; and for the whole `hushrun run` call, so a second check waits what is left.
    let fake = FakeBuild::start();
    fake.set_policy(|p| p.untrusted = Some(blocks::Untrusted::Event));
    fake.delay(route::STORE_PUT, Duration::from_secs(10));
    let m = machine();
    let relay = start(&fake, &m, |c| {
        with_id_token(&fake)(c);
        c.tuning.ci_wait = Duration::from_secs(1);
    });
    begin(&relay, &m);
    until("open", || answering(&relay));
    let by = job_identity(&fake, &relay);
    let (key, step) = block(&m.store, 63, 5_000, Some(by));
    assert_eq!(ask(&relay, built_costing(key, step, 400_000)), Answer::Ok);
    until("its PUT started", || fake.count(route::STORE_PUT) > 0);
    let started = Instant::now();
    relay.end_check(&[]);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(900) && took < Duration::from_secs(3),
        "{took:?}"
    );
    begin(&relay, &m);
    until("open", || answering(&relay));
    let started = Instant::now();
    relay.end_check(&[]);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
}

// covers: B-26, B-34
#[test]
fn a_stale_vouch_is_answered_within_the_block_s_budget_while_open_is_slow() {
    let fake = FakeBuild::start();
    fake.delay(route::OPEN, Duration::from_secs(10));
    let m = machine();
    // A block only downloaded, vouched for two days ago, whose compile took 2 seconds.
    let (key, step) = (Digest::of(b"stale"), Digest::of(b"stale step"));
    let src = m.store.root().with_file_name("stale");
    std::fs::write(&src, b"bytes").unwrap();
    m.store
        .publish_with_origin(
            &key,
            &json!({"step": step, "class": "library", "rustc_ms": 2000}),
            &[NewFile {
                name: "liba.rlib".into(),
                source: src,
            }],
            &Origin::downloaded(Vouch {
                host: fake.origin(),
                org: "acme".into(),
                repository: "github.com/acme/shop".into(),
                scope: "person/2".into(),
                outputs: Hex32([1; 32]),
                date_ms: 0,
            }),
            SystemTime::now(),
        )
        .unwrap();
    let relay = start(&fake, &m, |c| c.tuning.open = Duration::from_secs(10));
    begin(&relay, &m);
    let started = Instant::now();
    let answer = ask(
        &relay,
        Request::Vouch {
            key,
            step,
            outputs: Hex32([1; 32]),
        },
    );
    let took = started.elapsed();
    // `open` hasn't answered within half the compile: as unreachable, the entry counts.
    assert_eq!(answer, Answer::Offline);
    assert!(took < Duration::from_millis(1_500), "{took:?}");
    assert!(took >= Duration::from_millis(900), "{took:?}");
}

// covers: B-30
#[test]
fn a_warm_target_with_an_empty_store_downloads_nothing() {
    // The manifest's blocks are served and this store has none of them; cargo compiles nothing,
    // so no wrapper asks.
    let fake = FakeBuild::start();
    let pairs: Vec<Pair> = served(&fake, 5, 50_000, 1_000)
        .iter()
        .map(|(key, step)| Pair(address(step), address(key)))
        .collect();
    fake.set_manifest(pairs);
    let m = machine();
    let relay = start(&fake, &m, |_| {});
    begin(&relay, &m);
    until("open", || answering(&relay));
    relay.end_check(&[]);
    assert_eq!(fake.count(route::LOOKUP), 0);
    assert_eq!(fake.count(route::HINTS_LOOKUP), 0);
    assert_eq!(fake.served_bytes(), 0);
}
