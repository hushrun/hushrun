use std::io::{BufRead as _, Read as _, Write as _};
use std::net::TcpStream;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use super::*;
use crate::origin::{Identity, Vouch, Word};
use crate::session::{Session, Share, Start};
use crate::settings;

/// What Apache's build cache extension 1.2.0 wrote for `demo:core`: it names `core.jar` (1544
/// bytes) and `core-mvn-cache-ext-extra-output-1.zip` (364 bytes).
const BUILDINFO_BYTES: &[u8] = include_bytes!("../../../../../testdata/maven/core.buildinfo.xml");
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const JAR: &str = "core.jar";
const ZIP: &str = "core-mvn-cache-ext-extra-output-1.zip";

fn module() -> Module {
    Module {
        group: "demo".into(),
        artifact: "core".into(),
        checksum: "07b43d4938bbe3e1".into(),
    }
}

fn jar() -> Vec<u8> {
    (0..1544u32).map(|n| (n % 251) as u8).collect()
}

fn zip() -> Vec<u8> {
    vec![7u8; 364]
}

fn canonical() -> Vec<u8> {
    buildinfo::check(BUILDINFO_BYTES, &module())
        .unwrap()
        .canonical
}

fn path(file: &str) -> String {
    format!("/{TOKEN}/v1.1/demo/core/07b43d4938bbe3e1/{file}")
}

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

    /// Waits for a request `matches` names: a notice is handed over and not waited for.
    fn heard(&self, matches: impl Fn(&Request) -> bool) -> bool {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if self.asked().iter().any(&matches) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

/// A machine with a run's session, reuse `on`, and a relay's socket when `shares`.
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
        current.maven = Some(crate::maven::MavenRun {
            roots: vec![self.checkout()],
            repository: Some(self.dir.path().join("m2").to_string_lossy().into_owned()),
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

    /// Stores `files` as a downloaded block of `module()` from `scope`.
    fn downloaded(&self, files: &[(&str, &[u8])], scope: &str, cost_ms: u64) {
        publish_download(&self.store(), self.dir.path(), files, scope, cost_ms);
    }
}

/// Stores `files` in `store` as a downloaded block of `module()` from `scope`, staged in `dir`,
/// as the relay's publish does.
fn publish_download(
    store: &Store,
    dir: &std::path::Path,
    files: &[(&str, &[u8])],
    scope: &str,
    cost_ms: u64,
) {
    let staged = dir.join("staged");
    std::fs::create_dir_all(&staged).unwrap();
    let new: Vec<NewFile> = files
        .iter()
        .map(|(name, bytes)| {
            std::fs::write(staged.join(name), bytes).unwrap();
            NewFile {
                name: name.to_string(),
                source: staged.join(name),
            }
        })
        .collect();
    let outputs = hushrun_core::blocks::outputs_digest(
        &files
            .iter()
            .map(|(name, bytes)| hushrun_core::blocks::OutputFile {
                name,
                size: bytes.len() as u64,
                sha256: hushrun_core::blocks::Hex32(*Digest::of(bytes).as_bytes()),
            })
            .collect::<Vec<_>>(),
    );
    let origin = Origin::downloaded(Vouch {
        host: "https://api.example".into(),
        org: "acme".into(),
        repository: "example.com/acme/shop".into(),
        scope: scope.into(),
        outputs,
        date_ms: epoch_ms(SystemTime::now()),
        word: Word::Served,
    });
    let key = module().key();
    let description = json!({
        "schema": crate::key::BLOCK_SCHEMA, "kind": "maven/1", "step": key,
        "class": "library", "group": "demo", "artifact": "core",
        "checksum": "07b43d4938bbe3e1", "rustc_ms": cost_ms,
    });
    store
        .replace(
            &key,
            &description,
            &new,
            &origin,
            SystemTime::now(),
            |_, _| true,
        )
        .unwrap();
}

fn identity() -> Identity {
    Identity {
        host: "https://api.example".into(),
        org: "acme".into(),
        credential: "c".into(),
    }
}

fn request(method: &str, path: &str, body: &[u8]) -> http::Request {
    http::Request {
        method: method.into(),
        path: path.into(),
        authorization: None,
        body: Body::Bytes(body.to_vec()),
        close: false,
        interim: true,
    }
}

/// Maven's requests for one module it builds: the miss, then its files and its buildinfo.
fn build_and_store(cache: &Cache) {
    assert_eq!(cache.answer(&request("GET", &path(BUILDINFO), b"")).0, 404);
    assert_eq!(cache.answer(&request("PUT", &path(JAR), &jar())).0, 200);
    assert_eq!(cache.answer(&request("PUT", &path(ZIP), &zip())).0, 200);
    assert_eq!(
        cache
            .answer(&request("PUT", &path(BUILDINFO), BUILDINFO_BYTES))
            .0,
        200
    );
}

// covers: B-172, B-174, B-175, B-177
#[test]
fn a_module_maven_stores_is_one_block_served_back_whole() {
    let machine = Machine::new(false);
    let cache = machine.cache();
    // Before its buildinfo was served, no file of the module is.
    assert_eq!(cache.answer(&request("GET", &path(JAR), b"")).0, 404);
    build_and_store(&cache);

    let key = module().key();
    let block = machine.store().block(&key).unwrap().unwrap();
    assert_eq!(Kind::of_description(&block.description), Kind::Maven);
    assert_eq!(block.description["module"], "demo:core");
    assert_eq!(block.description["checksum"], "07b43d4938bbe3e1");
    assert_eq!(block.description["hash"], "XX");
    let names: Vec<&str> = block.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, [BUILDINFO, JAR, ZIP]);
    assert_eq!(std::fs::read(&block.files[0].path).unwrap(), canonical());
    assert!(
        machine
            .store()
            .origin(&key)
            .is_some_and(|o| !o.downloaded_only())
    );

    let (status, body) = cache.answer(&request("GET", &path(BUILDINFO), b""));
    assert_eq!(status, 200);
    assert_eq!(body, canonical());
    assert_eq!(cache.answer(&request("GET", &path(JAR), b"")), (200, jar()));
    assert_eq!(cache.answer(&request("GET", &path(ZIP), b"")), (200, zip()));
    assert_eq!(cache.answer(&request("GET", &path("x.jar"), b"")).0, 404);
    // A HEAD says whether the store holds it, and logs nothing.
    assert_eq!(cache.answer(&request("HEAD", &path(JAR), b"")).0, 200);
    assert_eq!(cache.answer(&request("HEAD", &path("x.jar"), b"")).0, 404);

    // The build and the reuse: the files and the HEADs logged nothing.
    let log = machine.log();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[0].outcome, Outcome::BuiltAndStored);
    assert_eq!(log[0].crate_name, "demo:core");
    assert_eq!(log[0].kind(), Kind::Maven);
    assert_eq!(log[1].outcome, Outcome::Reused);
    let summary = crate::session::Summary::of(Mode::On, 0, &log);
    assert!(
        summary.lines()[0]
            .starts_with("build: maven: 2 modules: 1 from hushrun's cache, 1 built and stored"),
        "{:?}",
        summary.lines()
    );
}

// covers: B-170, B-171, B-175
#[test]
fn only_the_run_s_token_and_the_extension_s_paths_are_answered() {
    let machine = Machine::new(false);
    let cache = machine.cache();
    let other = "f".repeat(64);
    let theirs = path(BUILDINFO).replacen(TOKEN, &other, 1);
    assert_eq!(cache.answer(&request("GET", &theirs, b"")).0, 401);
    assert_eq!(
        cache.answer(&request("PUT", &theirs, BUILDINFO_BYTES)).0,
        401
    );
    assert_eq!(cache.answer(&request("GET", "/", b"")).0, 401);
    let bad = format!("/{TOKEN}/v1.1/demo/core/not-hex/{BUILDINFO}");
    assert_eq!(cache.answer(&request("GET", &bad, b"")).0, 404);
    assert_eq!(
        cache.answer(&request("DELETE", &path(BUILDINFO), b"")).0,
        405
    );
    // The build's report goes nowhere.
    let report = format!("/{TOKEN}/v1.1/demo/parent/8ae602df-1bd0-4070-b118-b5bc46ab35c9/{REPORT}");
    assert_eq!(
        cache.answer(&request("PUT", &report, b"<cacheReport/>")).0,
        200
    );
    assert!(machine.log().is_empty());
}

// covers: B-175
#[test]
fn pieces_come_in_any_order_and_an_entry_that_isn_t_whole_keeps_nothing() {
    let machine = Machine::new(false);
    let cache = machine.cache();
    let key = module().key();
    // The buildinfo first, then the files: whole once the last comes.
    cache.put(&module(), BUILDINFO, BUILDINFO_BYTES);
    cache.put(&module(), ZIP, &zip());
    assert!(machine.store().block(&key).unwrap().is_none());
    cache.put(&module(), JAR, &jar());
    assert!(machine.store().block(&key).unwrap().is_some());

    // A file of another size than its buildinfo gives: nothing kept, and the line says why.
    let machine = Machine::new(false);
    let cache = machine.cache();
    cache.put(&module(), JAR, &jar()[1..]);
    cache.put(&module(), ZIP, &zip());
    cache.put(&module(), BUILDINFO, BUILDINFO_BYTES);
    assert!(machine.store().block(&key).unwrap().is_none());
    let log = machine.log();
    assert_eq!(log.len(), 1);
    assert!(
        log[0]
            .note
            .as_deref()
            .is_some_and(|n| n.contains("core.jar isn't the size")),
        "{log:?}"
    );

    // A buildinfo of another module: refused.
    let machine = Machine::new(false);
    let cache = machine.cache();
    let other = Module {
        artifact: "app".into(),
        ..module()
    };
    cache.put(&other, BUILDINFO, BUILDINFO_BYTES);
    assert!(machine.store().block(&other.key()).unwrap().is_none());
    assert!(
        machine.log()[0]
            .note
            .as_deref()
            .is_some_and(|n| n.contains("another module"))
    );
}

// covers: B-173
#[test]
fn an_entry_that_holds_the_checkout_s_folder_stays_with_maven() {
    let machine = Machine::new(false);
    let cache = machine.cache();
    let mut jar = jar();
    let checkout = machine.checkout();
    jar[100..100 + checkout.len()].copy_from_slice(checkout.as_bytes());
    cache.put(&module(), JAR, &jar);
    cache.put(&module(), ZIP, &zip());
    cache.put(&module(), BUILDINFO, BUILDINFO_BYTES);
    assert!(machine.store().block(&module().key()).unwrap().is_none());
    let log = machine.log();
    assert_eq!(
        log[0].maven.as_ref().and_then(|m| m.local),
        Some(Local::Checkout)
    );
    let summary = crate::session::Summary::of(Mode::On, 0, &log);
    assert_eq!(
        summary.lines()[1],
        "build: maven: 1 module built here stays on this machine (1: its files hold the checkout's folder)"
    );

    // And Maven's local repository, in the buildinfo.
    let machine = Machine::new(false);
    let cache = machine.cache();
    let repository = machine.dir.path().join("m2").to_string_lossy().into_owned();
    let buildinfo = String::from_utf8(BUILDINFO_BYTES.to_vec())
        .unwrap()
        .replacen("<goals>", &format!("<!-- {repository}/x --><goals>"), 1);
    cache.put(&module(), JAR, &self::jar());
    cache.put(&module(), ZIP, &zip());
    cache.put(&module(), BUILDINFO, buildinfo.as_bytes());
    assert_eq!(
        machine.log()[0].maven.as_ref().and_then(|m| m.local),
        Some(Local::Repository)
    );
}

// covers: B-174, B-175
#[test]
fn in_a_run_that_shares_a_download_is_served_and_a_stored_entry_told_to_the_relay() {
    let machine = Machine::new(true);
    let store = machine.store();
    let key = module().key();
    let relay = Relay::start(machine.socket(), {
        let store = store.clone();
        let dir = machine.dir.path().to_path_buf();
        move |request| match request {
            Request::ToolFetch { key, .. } => {
                let (buildinfo, jar, zip) = (canonical(), jar(), zip());
                let files: [(&str, &[u8]); 3] = [(BUILDINFO, &buildinfo), (JAR, &jar), (ZIP, &zip)];
                publish_download(&store, &dir, &files, "protected/1", 900);
                Answer::Ready { key: *key }
            }
            _ => Answer::Ok,
        }
    });
    let cache = machine.cache();
    assert_eq!(cache.get(&module()).unwrap(), canonical());
    assert!(matches!(relay.asked()[0], Request::ToolFetch { .. }));
    assert_eq!(cache.file(&module(), JAR).unwrap(), jar());
    let log = machine.log();
    assert!(log[0].downloaded && log[0].outcome == Outcome::Reused);
    assert_eq!(log[0].saved_ms, 900);
    assert!(store.origin(&key).unwrap().reused);
    let summary = crate::session::Summary::of(Mode::On, 0, &log);
    assert!(
        summary.lines()[0].starts_with(
            "build: maven: 1 module: 1 from hushrun's cache (1 from the shared store)"
        ),
        "{:?}",
        summary.lines()
    );

    // Maven builds another module and stores it: the relay hears of it.
    let app = Module {
        artifact: "app".into(),
        checksum: "e69802bf98d5ffa4".into(),
        ..module()
    };
    let buildinfo = String::from_utf8(BUILDINFO_BYTES.to_vec())
        .unwrap()
        .replace(
            "<artifactId>core</artifactId>",
            "<artifactId>app</artifactId>",
        )
        .replace("07b43d4938bbe3e1", "e69802bf98d5ffa4");
    assert!(cache.get(&app).is_none());
    cache.put(&app, JAR, &jar());
    cache.put(&app, ZIP, &zip());
    cache.put(&app, BUILDINFO, buildinfo.as_bytes());
    let app_key = app.key();
    assert!(relay.heard(|r| matches!(r, Request::ToolBuilt { key, .. } if *key == app_key)));
}

// covers: B-172, B-174
#[test]
fn a_downloaded_block_that_isn_t_whole_is_reported_suspended_and_removed() {
    let machine = Machine::new(true);
    let store = machine.store();
    let key = module().key();
    // The extension's own buildinfo, not the canonical form, and the zip left out: what no
    // honest uploader sends.
    machine.downloaded(&[(BUILDINFO, BUILDINFO_BYTES), (JAR, &jar())], "team", 100);
    let relay = Relay::start(machine.socket(), |_| Answer::Ok);
    let cache = machine.cache();
    assert!(cache.get(&module()).is_none());
    assert!(relay.asked().iter().any(|r| matches!(
        r,
        Request::ToolReport {
            kind: Kind::Maven,
            problem: Problem::Rejected,
            ..
        }
    )));
    assert!(store.block(&key).unwrap().is_none());
    assert!(store.suspended_at(&key, SystemTime::now()).is_some());
    assert!(cache.file(&module(), JAR).is_none());
}

// covers: B-170
#[test]
fn maven_s_requests_over_one_kept_connection_are_answered() {
    let machine = Machine::new(false);
    let (listener, token) = match crate::cache::endpoint::for_run() {
        Ok(bound) => bound,
        Err(_) => return,
    };
    assert_eq!(token.len(), 64);
    let url = crate::cache::endpoint::run_url(&listener, &token).unwrap();
    assert!(url.starts_with("http://127.0.0.1:") && url.ends_with(&token));
    let address = listener.local_addr().unwrap();
    let mut current = machine.session.current().clone();
    current.maven = Some(crate::maven::MavenRun::default());
    let running = start(listener, Cache::new(current, &token)).unwrap();
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
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
    let at = |file: &str| format!("/{token}//v1.1/demo/core/07b43d4938bbe3e1/{file}");
    let get = |file: &str| format!("GET {} HTTP/1.1\r\nHost: x\r\n\r\n", at(file)).into_bytes();
    send(get(BUILDINFO));
    assert!(read_answer(&mut reader).0.starts_with("HTTP/1.1 404"));
    for (file, bytes) in [
        (JAR, jar()),
        (ZIP, zip()),
        (BUILDINFO, BUILDINFO_BYTES.to_vec()),
    ] {
        // As Maven's resolver sends them: waiting for 100 Continue.
        send(
            format!(
                "PUT {} HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n",
                at(file),
                bytes.len()
            )
            .into_bytes(),
        );
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("HTTP/1.1 100"), "{line}");
        reader.read_line(&mut line).unwrap();
        send(bytes);
        assert!(read_answer(&mut reader).0.starts_with("HTTP/1.1 200"));
    }
    send(get(BUILDINFO));
    let (status, body) = read_answer(&mut reader);
    assert!(status.starts_with("HTTP/1.1 200"));
    assert_eq!(body, canonical());
    send(get(JAR));
    assert_eq!(read_answer(&mut reader).1, jar());
    drop(running);
}

// covers: B-175
#[test]
fn a_run_whose_maven_skips_tests_keeps_none_of_its_modules() {
    let machine = Machine::new(false);
    let cache = machine.cache();
    *cache.skips_tests().lock().unwrap() = Some("skipTests".into());
    build_and_store(&cache);
    assert!(machine.store().block(&module().key()).unwrap().is_none());
    let log = machine.log();
    assert_eq!(log.len(), 1);
    assert_eq!(
        log[0].note.as_deref(),
        Some("not kept: Maven skips tests in this run (skipTests)")
    );
}
