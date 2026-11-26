use std::io::{BufRead as _, Read as _, Write as _};
use std::net::TcpStream;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::gradle::entry::{self, Checked};
use crate::model::Mode;
use crate::origin::{Identity, Vouch, Word};
use crate::relay::{Answer, Problem};
use crate::session::{Session, Share, Start};
use crate::settings;

const ENTRY_BYTES: &[u8] = include_bytes!("../../../../../testdata/gradle/compile-java.entry");
const GRADLE_KEY: &str = "49b7acd36407da65ca900b05e84a6241";
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A fake relay on a socket: every request it gets, and an answer from `answer`, which may act
/// on the store first (a fetch's publish).
struct Relay {
    asked: Arc<Mutex<Vec<Request>>>,
}

impl Relay {
    fn start(socket: PathBuf, answer: impl Fn(&Request) -> Answer + Send + 'static) -> Relay {
        let listener = UnixListener::bind(&socket).unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let mut line = String::new();
                if std::io::BufReader::new(&stream)
                    .read_line(&mut line)
                    .is_err()
                {
                    continue;
                }
                let Ok(request) = serde_json::from_str::<Request>(line.trim()) else {
                    continue;
                };
                let reply = answer(&request);
                seen.lock().unwrap().push(request);
                let mut bytes = serde_json::to_vec(&reply).unwrap();
                bytes.push(b'\n');
                let _ = (&stream).write_all(&bytes);
            }
        });
        Relay { asked }
    }

    fn asked(&self) -> Vec<Request> {
        self.asked.lock().unwrap().clone()
    }
}

/// A machine with a run's session, reuse `on`, the checkout at `checkout`, and a relay's socket
/// when `shares`.
struct Machine {
    dir: tempfile::TempDir,
    session: Session,
}

impl Machine {
    fn new(shares: bool) -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("build.json");
        settings::update(&settings, |s| s.mode = Mode::On).unwrap();
        let exe = dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        let cache = dir.path().join("cache");
        let home = dir.path().join("home").into_os_string();
        let env = move |name: &str| (name == "HOME").then(|| home.clone());
        let mut session = crate::session::begin(&Start {
            settings: &settings,
            cache_dir: Some(&cache),
            exe: &exe,
            in_ci: false,
            repository: Some("example.com/acme/shop"),
            repository_env: &[],
            env: &env,
            now: SystemTime::now(),
        })
        .unwrap();
        if shares {
            session
                .set_share(Share {
                    socket: dir.path().join("relay.sock"),
                    run: "run-1".into(),
                    by: identity(),
                    repository: "example.com/acme/shop".into(),
                })
                .unwrap();
        }
        Machine { dir, session }
    }

    fn checkout(&self) -> String {
        self.dir.path().join("shop").to_string_lossy().into_owned()
    }

    fn cache(&self) -> Cache {
        let mut current = self.session.current().clone();
        current.gradle = Some(crate::gradle::GradleRun {
            roots: vec![self.checkout()],
            user_home: None,
        });
        Cache::new(current, TOKEN)
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("relay.sock")
    }

    fn store(&self) -> Store {
        Store::new(&self.session.current().store)
    }

    fn log(&self) -> Vec<Entry> {
        let path = self.session.current().folder.join("log.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn identity() -> Identity {
    Identity {
        host: "https://api.example".into(),
        org: "acme".into(),
        credential: "c".into(),
    }
}

fn request(method: &str, path: &str, token: &str, body: &[u8]) -> http::Request {
    http::Request {
        method: method.into(),
        path: path.into(),
        authorization: Some(http::basic(USER_FOR_TESTS, token)),
        body: Body::Bytes(body.to_vec()),
        close: false,
        interim: true,
    }
}

const USER_FOR_TESTS: &str = crate::gradle::USER;

fn path() -> String {
    format!("{}{GRADLE_KEY}", crate::gradle::PATH)
}

fn canonical() -> Vec<u8> {
    Checked::from_gradle(ENTRY_BYTES, GRADLE_KEY)
        .unwrap()
        .canonical()
}

// covers: B-80, B-81
#[test]
fn an_entry_gradle_stores_is_a_block_served_back_in_its_canonical_form() {
    let machine = Machine::new(false);
    let cache = machine.cache();
    assert_eq!(cache.answer(&request("GET", &path(), TOKEN, b"")).0, 404);
    assert_eq!(
        cache.answer(&request("PUT", &path(), TOKEN, ENTRY_BYTES)).0,
        200
    );

    let key = crate::gradle::key(GRADLE_KEY).unwrap();
    let block = machine.store().block(&key).unwrap().unwrap();
    assert_eq!(Kind::of_description(&block.description), Kind::Gradle);
    assert_eq!(block.description["gradle_key"], GRADLE_KEY);
    assert_eq!(block.description["task"], ":compileJava");
    assert_eq!(std::fs::read(&block.files[0].path).unwrap(), canonical());
    assert!(
        machine
            .store()
            .origin(&key)
            .is_some_and(|o| !o.downloaded_only())
    );

    let (status, body) = cache.answer(&request("GET", &path(), TOKEN, b""));
    assert_eq!(status, 200);
    assert_eq!(entry::gunzip(&body, u64::MAX).unwrap(), canonical());
    // A HEAD says whether the store holds it, and logs nothing.
    assert_eq!(cache.answer(&request("HEAD", &path(), TOKEN, b"")).0, 200);
    let other = format!("{}{}", crate::gradle::PATH, "ab".repeat(16));
    assert_eq!(cache.answer(&request("HEAD", &other, TOKEN, b"")).0, 404);

    // The PUT and the GET: the HEADs logged nothing.
    let log = machine.log();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[0].outcome, Outcome::BuiltAndStored);
    assert_eq!(log[0].crate_name, ":compileJava");
    assert_eq!(log[0].kind(), Kind::Gradle);
    assert_eq!(log[1].outcome, Outcome::Reused);
    assert!(log[1].saved_ms > 0);
    let summary = crate::session::Summary::of(Mode::On, 0, &log);
    assert!(
        summary.lines()[0]
            .starts_with("build: gradle: 2 tasks: 1 from hushrun's cache, 1 run and stored"),
        "{:?}",
        summary.lines()
    );
}

// covers: B-77
#[test]
fn only_gradle_with_the_run_s_token_and_a_key_is_answered() {
    let machine = Machine::new(false);
    let cache = machine.cache();
    let other = "f".repeat(64);
    assert_eq!(cache.answer(&request("GET", &path(), &other, b"")).0, 401);
    let mut anonymous = request("GET", &path(), TOKEN, b"");
    anonymous.authorization = None;
    assert_eq!(cache.answer(&anonymous).0, 401);
    assert_eq!(cache.answer(&request("GET", "/other/x", TOKEN, b"")).0, 404);
    assert_eq!(
        cache
            .answer(&request("GET", "/cache/../../etc/passwd", TOKEN, b""))
            .0,
        404
    );
    assert_eq!(cache.answer(&request("POST", &path(), TOKEN, b"")).0, 405);
    // A body that isn't an entry is answered 200 and kept nowhere.
    assert_eq!(
        cache.answer(&request("PUT", &path(), TOKEN, b"junk")).0,
        200
    );
    let key = crate::gradle::key(GRADLE_KEY).unwrap();
    assert!(machine.store().block(&key).unwrap().is_none());
    assert!(
        machine.log()[0]
            .note
            .as_deref()
            .unwrap()
            .contains("wasn't kept")
    );
}

// covers: B-79
#[test]
fn an_entry_that_holds_the_checkout_s_folder_stays_with_gradle() {
    let machine = Machine::new(false);
    let cache = machine.cache();
    let key = "ab".repeat(16);
    let report = format!("<testsuite file=\"{}/src/A.kt\"/>", machine.checkout());
    let tar = tar_with(&key, &[("tree-reports/a.xml", report.as_bytes())]);
    cache.put(&key, &entry::to_gradle(&tar));
    let block_key = crate::gradle::key(&key).unwrap();
    assert!(machine.store().block(&block_key).unwrap().is_none());
    let log = machine.log();
    assert_eq!(
        log[0].gradle.as_ref().unwrap().local,
        Some(crate::gradle::Local::Checkout)
    );
    let summary = crate::session::Summary::of(Mode::On, 0, &log);
    assert!(
        summary.lines()[1].contains("stays on this machine"),
        "{:?}",
        summary.lines()
    );
    // covers: B-210
    // The line names the task, by Gradle's name, as kapt's stubs would be named.
    assert!(
        summary.lines()[1].ends_with("(1: its outputs hold the checkout's folder): :app:test"),
        "{:?}",
        summary.lines()
    );
}

/// A tar with Gradle's METADATA for `key` and `files`.
fn tar_with(key: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut members = vec![entry::Member {
        name: entry::METADATA.into(),
        folder: false,
        mode: 0o644,
        bytes: format!(
            "buildCacheKey={key}\nbuildInvocationId=x\ncreationTime=1\nexecutionTime=1500\ngradleVersion=8.14.3\nidentity=\\:app\\:test\n"
        )
        .into_bytes(),
    }];
    for (name, bytes) in files {
        members.push(entry::Member {
            name: (*name).into(),
            folder: false,
            mode: 0o644,
            bytes: bytes.to_vec(),
        });
    }
    Checked {
        members,
        key: key.into(),
        identity: String::new(),
        gradle_version: String::new(),
        execution_ms: 0,
    }
    .canonical()
}

// covers: B-80, B-81
#[test]
fn in_a_run_that_shares_a_download_is_served_and_a_stored_entry_told_to_the_relay() {
    let machine = Machine::new(true);
    let store = machine.store();
    let key = crate::gradle::key(GRADLE_KEY).unwrap();
    let canonical = canonical();
    let staged = machine.dir.path().join("downloaded.tar");
    std::fs::write(&staged, &canonical).unwrap();
    let relay = Relay::start(machine.socket(), {
        let store = store.clone();
        move |request| match request {
            // As the relay's publish does: the block in the store, downloaded.
            Request::ToolFetch { key, .. } => {
                let description = json!({
                    "schema": crate::key::BLOCK_SCHEMA, "kind": "gradle/1", "step": key,
                    "class": "library", "gradle_key": GRADLE_KEY, "rustc_ms": 900,
                });
                let size = std::fs::metadata(&staged).unwrap().len();
                let sha = Digest::of(&std::fs::read(&staged).unwrap());
                let origin = Origin::downloaded(Vouch {
                    host: "https://api.example".into(),
                    org: "acme".into(),
                    repository: "example.com/acme/shop".into(),
                    scope: "protected/1".into(),
                    outputs: crate::gradle::outputs_of(size, &sha),
                    date_ms: epoch_ms(SystemTime::now()),
                    word: Word::Served,
                });
                let files = [NewFile {
                    name: ENTRY.into(),
                    source: staged.clone(),
                }];
                store
                    .replace(
                        key,
                        &description,
                        &files,
                        &origin,
                        SystemTime::now(),
                        |_, _| true,
                    )
                    .unwrap();
                Answer::Ready { key: *key }
            }
            _ => Answer::Ok,
        }
    });
    let cache = machine.cache();
    let served = cache.get(GRADLE_KEY).unwrap();
    assert_eq!(entry::gunzip(&served, u64::MAX).unwrap(), canonical);
    assert!(matches!(relay.asked()[0], Request::ToolFetch { .. }));
    let log = machine.log();
    assert!(log[0].downloaded && log[0].outcome == Outcome::Reused);
    assert_eq!(log[0].saved_ms, 900);
    assert!(store.origin(&key).unwrap().reused);

    // Gradle runs another task and stores it: the relay hears of it.
    let other = "cd".repeat(16);
    cache.put(
        &other,
        &entry::to_gradle(&tar_with(&other, &[("tree-out/x", b"x")])),
    );
    let other_key = crate::gradle::key(&other).unwrap();
    // A notice is handed over and not waited for: the relay reads it when it reads it.
    let told = || {
        relay.asked().iter().any(|r| {
            matches!(
                r,
                Request::ToolBuilt { kind: Kind::Gradle, key, cost_ms: 1500 } if *key == other_key
            )
        })
    };
    let until = std::time::Instant::now() + Duration::from_secs(5);
    while !told() && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(told());
}

// covers: B-78, B-80
#[test]
fn a_downloaded_block_that_isn_t_a_canonical_entry_is_reported_suspended_and_removed() {
    let machine = Machine::new(true);
    let store = machine.store();
    let key = crate::gradle::key(GRADLE_KEY).unwrap();
    // Gradle's own bytes, not the canonical form: what no honest uploader sends.
    let staged = machine.dir.path().join("downloaded.tar");
    std::fs::write(&staged, entry::gunzip(ENTRY_BYTES, u64::MAX).unwrap()).unwrap();
    let size = std::fs::metadata(&staged).unwrap().len();
    let sha = Digest::of(&std::fs::read(&staged).unwrap());
    let origin = Origin::downloaded(Vouch {
        host: "https://api.example".into(),
        org: "acme".into(),
        repository: "example.com/acme/shop".into(),
        scope: "team".into(),
        outputs: crate::gradle::outputs_of(size, &sha),
        date_ms: epoch_ms(SystemTime::now()),
        word: Word::Served,
    });
    let description = json!({
        "schema": crate::key::BLOCK_SCHEMA, "kind": "gradle/1", "step": key,
        "class": "library", "gradle_key": GRADLE_KEY,
    });
    store
        .replace(
            &key,
            &description,
            &[NewFile {
                name: ENTRY.into(),
                source: staged,
            }],
            &origin,
            SystemTime::now(),
            |_, _| true,
        )
        .unwrap();
    let relay = Relay::start(machine.socket(), |_| Answer::Ok);
    let cache = machine.cache();
    assert!(cache.get(GRADLE_KEY).is_none());
    assert!(relay.asked().iter().any(|r| matches!(
        r,
        Request::ToolReport {
            kind: Kind::Gradle,
            problem: Problem::Rejected,
            ..
        }
    )));
    assert!(store.block(&key).unwrap().is_none());
    assert!(store.suspended_at(&key, SystemTime::now()).is_some());
}

// covers: B-77
#[test]
fn gradle_s_requests_over_one_kept_connection_are_answered() {
    let machine = Machine::new(false);
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(_) => return,
    };
    let address = listener.local_addr().unwrap();
    let running = start(listener, machine.cache()).unwrap();
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let auth = http::basic(USER_FOR_TESTS, TOKEN);
    let send = |text: Vec<u8>| (&stream).write_all(&text).unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let read_answer = |reader: &mut std::io::BufReader<TcpStream>| -> (String, Vec<u8>) {
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(n) = line.strip_prefix("Content-Length: ") {
                length = n.trim().parse().unwrap();
            }
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).unwrap();
        (status, body)
    };
    send(format!("GET {} HTTP/1.1\r\nAuthorization: {auth}\r\n\r\n", path()).into_bytes());
    assert!(read_answer(&mut reader).0.starts_with("HTTP/1.1 404"));
    let mut put = format!(
        "PUT {} HTTP/1.1\r\nAuthorization: {auth}\r\nContent-Length: {}\r\n\r\n",
        path(),
        ENTRY_BYTES.len()
    )
    .into_bytes();
    put.extend_from_slice(ENTRY_BYTES);
    send(put);
    assert!(read_answer(&mut reader).0.starts_with("HTTP/1.1 200"));
    send(format!("GET {} HTTP/1.1\r\nAuthorization: {auth}\r\n\r\n", path()).into_bytes());
    let (status, body) = read_answer(&mut reader);
    assert!(status.starts_with("HTTP/1.1 200"));
    assert_eq!(entry::gunzip(&body, u64::MAX).unwrap(), canonical());
    drop(running);
}
