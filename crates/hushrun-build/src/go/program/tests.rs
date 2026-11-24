//! The cache program against a fake `go` command: the protocol, exactly one answer for every
//! request, Go's own cache, and what is served from and kept in hushrun's store.

use std::io::Cursor;
use std::os::unix::net::UnixListener;
use std::sync::mpsc;

use super::*;
use crate::go::archive::tests::archive;
use crate::origin::Identity;
use crate::session::{Session, Share, Start};
use crate::settings;

/// What the program wrote, shared with the test.
#[derive(Clone, Default)]
struct Out(Arc<Mutex<Vec<u8>>>);

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        lock(&self.0).extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Go's side, in order: each step's bytes go once the program has written that many lines, as
/// Go sends a put only after the miss that led to it was answered.
struct Steps {
    out: Out,
    steps: std::collections::VecDeque<(usize, String)>,
    now: Vec<u8>,
}

impl Read for Steps {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.now.is_empty() {
            let Some((lines, text)) = self.steps.pop_front() else {
                return Ok(0);
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.out.count() < lines && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            self.now = text.into_bytes();
        }
        let n = buf.len().min(self.now.len());
        buf[..n].copy_from_slice(&self.now[..n]);
        self.now.drain(..n);
        Ok(n)
    }
}

impl Out {
    fn count(&self) -> usize {
        lock(&self.0).iter().filter(|b| **b == b'\n').count()
    }

    fn lines(&self) -> Vec<Value> {
        String::from_utf8(lock(&self.0).clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn b64s(bytes: &[u8]) -> String {
    b64(bytes).as_str().unwrap().to_string()
}

fn get(id: i64, action: &[u8; 32]) -> String {
    format!(
        "{}\n\n",
        json!({"ID": id, "Command": "get", "ActionID": b64s(action)})
    )
}

/// A put as Go writes it: the request, a blank line, the body's line.
fn put(id: i64, action: &[u8; 32], body: &[u8]) -> String {
    let output = Digest::of(body);
    let mut text = format!(
        "{}\n\n",
        json!({"ID": id, "Command": "put", "ActionID": b64s(action),
               "OutputID": b64s(output.as_bytes()), "BodySize": body.len()})
    );
    if !body.is_empty() {
        text.push_str(&format!("\"{}\"\n", b64s(body)));
    }
    text
}

fn close(id: i64) -> String {
    format!("{}\n\n", json!({"ID": id, "Command": "close"}))
}

/// A machine: a fake toolchain, Go's cache, and maybe a run's session.
struct Machine {
    dir: tempfile::TempDir,
    env: HashMap<String, OsString>,
    session: Option<Session>,
}

impl Machine {
    fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let goroot = dir.path().join("goroot");
        let tools = goroot.join("pkg/tool/darwin_arm64");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::create_dir_all(goroot.join("bin")).unwrap();
        std::fs::write(goroot.join("bin/go"), b"go").unwrap();
        std::fs::write(tools.join("compile"), b"compile").unwrap();
        let gocache = dir.path().join("gocache");
        std::fs::create_dir_all(&gocache).unwrap();
        let mut env = HashMap::new();
        env.insert("GOVERSION".into(), "go1.27.4".into());
        env.insert("GOCACHE".into(), gocache.into_os_string());
        env.insert("GOROOT".into(), goroot.into_os_string());
        env.insert("GOTOOLDIR".into(), tools.into_os_string());
        Machine {
            dir,
            env,
            session: None,
        }
    }

    /// A run's session with reuse `on`, the audit as `audit` says, and the relay at `socket`.
    fn with_session(mut self, audit: Option<&str>, socket: Option<&Path>) -> Machine {
        let settings = self.dir.path().join("build.json");
        settings::update(&settings, |s| s.mode = Mode::On).unwrap();
        let exe = self.dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        let audit = audit.map(OsString::from);
        let env = move |name: &str| (name == crate::audit::VAR).then(|| audit.clone()).flatten();
        let cache = self.dir.path().join("cache");
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
        if let Some(socket) = socket {
            session
                .set_share(Share {
                    socket: socket.to_path_buf(),
                    run: "run-1".into(),
                    by: Identity {
                        host: "https://api.example".into(),
                        org: "acme".into(),
                        credential: "c".into(),
                    },
                    repository: "example.com/acme/shop".into(),
                })
                .unwrap();
        }
        self.env.insert(
            crate::session::SESSION.into(),
            session.current().folder.clone().into_os_string(),
        );
        self.session = Some(session);
        self
    }

    fn current(&self) -> &Current {
        self.session.as_ref().unwrap().current()
    }

    fn store(&self) -> Store {
        Store::new(&self.current().store)
    }

    fn gocache(&self) -> GoCache {
        GoCache::new(Path::new(&self.env["GOCACHE"]))
    }

    fn identity(&self) -> Digest {
        Toolchain::from_env(&|n| self.env.get(n).cloned())
            .unwrap()
            .identity(None)
            .unwrap()
    }

    /// Serves `steps` in order, each sent once that many lines were answered (the first line
    /// counts), with the `go` command's arguments `args`: every answer.
    fn serve_steps(&self, steps: &[(usize, String)], args: &'static str) -> Vec<Value> {
        let out = Out::default();
        let input = Steps {
            out: out.clone(),
            steps: steps.iter().cloned().collect(),
            now: Vec::new(),
        };
        serve_with(
            BufReader::new(input),
            Box::new(out.clone()),
            self.env.clone(),
            Box::new(move |_| Ok(args.to_string())),
        );
        out.lines()
    }

    /// Serves `input` with the `go` command's arguments `args`: every answer.
    fn serve(&self, input: &str, args: &'static str) -> Vec<Value> {
        let out = Out::default();
        serve_with(
            Cursor::new(input.as_bytes().to_vec()),
            Box::new(out.clone()),
            self.env.clone(),
            Box::new(move |_| Ok(args.to_string())),
        );
        out.lines()
    }

    /// A block of `body` under `action` in the store, built here.
    fn seed(&self, action: &[u8; 32], body: &[u8]) -> Digest {
        let key = crate::go::key(action, &self.identity());
        let source = self.dir.path().join(format!("seed-{}", key.hex()));
        std::fs::write(&source, body).unwrap();
        let description = json!({
            "schema": BLOCK_SCHEMA, "kind": "go/1", "step": key, "class": "library",
            "action": Digest::from_bytes(*action).hex(), "rustc_ms": 2000,
        });
        self.store()
            .publish_with_origin(
                &key,
                &description,
                &[NewFile {
                    name: ARCHIVE.into(),
                    source,
                }],
                &Origin::built_here("an-earlier-run", None),
                SystemTime::now(),
            )
            .unwrap();
        key
    }
}

/// Each id answered exactly once, and the answers by id.
fn by_id(lines: &[Value]) -> HashMap<i64, Value> {
    let mut answers = HashMap::new();
    for line in lines {
        let id = line["ID"].as_i64().unwrap();
        assert!(
            answers.insert(id, line.clone()).is_none(),
            "{id} answered twice: {lines:?}"
        );
    }
    answers
}

// covers: B-40, B-41, B-47
#[test]
fn every_request_is_answered_once_and_a_put_lands_in_gos_own_cache() {
    let m = Machine::new();
    let action = [1u8; 32];
    let lines = m.serve_steps(
        &[
            (1, get(1, &action)),
            (2, put(2, &action, b"compiler output")),
            (3, get(3, &action)),
            (4, put(4, &[2u8; 32], b"")),
            (5, close(5)),
        ],
        "go build ./...",
    );
    assert_eq!(
        lines[0],
        json!({"ID": 0, "KnownCommands": ["get", "put", "close"]})
    );
    let answers = by_id(&lines);
    assert_eq!(answers.len(), 6);
    assert_eq!(answers[&1]["Miss"], true);
    let path = PathBuf::from(answers[&2]["DiskPath"].as_str().unwrap());
    assert!(path.starts_with(&m.env["GOCACHE"]));
    assert_eq!(std::fs::read(&path).unwrap(), b"compiler output");
    // Plain Go reads it back, and so does the program.
    assert_eq!(
        m.gocache().get(&action, SystemTime::now()).unwrap().path,
        path
    );
    assert_eq!(
        answers[&3]["DiskPath"].as_str().unwrap(),
        path.to_str().unwrap()
    );
    assert_eq!(answers[&3]["Size"], 15);
    assert!(answers[&3]["Time"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        answers[&3]["OutputID"],
        b64(Digest::of(b"compiler output").as_bytes())
    );
    // An empty body is a file too.
    let empty = PathBuf::from(answers[&4]["DiskPath"].as_str().unwrap());
    assert_eq!(std::fs::read(empty).unwrap(), b"");
    assert_eq!(answers[&5], json!({"ID": 5}));
}

// covers: B-40, B-47
#[test]
fn a_go_version_the_probe_didnt_see_keeps_gos_own_cache_and_leaves_out_the_store() {
    // COMMAND's `go` is another than the one the run asked (a toolchain line, `GOTOOLCHAIN`).
    let mut m = Machine::new().with_session(Some("none"), None);
    let stored = [2u8; 32];
    m.seed(&stored, &archive(&stored, b"in the store", &[]));
    m.env.insert("GOVERSION".into(), "go1.28.0".into());
    let action = [3u8; 32];
    let lines = m.serve_steps(
        &[
            (1, put(1, &action, b"x")),
            (2, get(2, &action)),
            (3, get(3, &stored)),
            (4, close(4)),
        ],
        "go build ./...",
    );
    let answers = by_id(&lines);
    // Go's own cache, whose format is the same since Go 1.10: written and read.
    let path = answers[&1]["DiskPath"].as_str().unwrap();
    assert!(Path::new(path).starts_with(&m.env["GOCACHE"]), "{path}");
    assert_eq!(answers[&2]["DiskPath"].as_str(), Some(path));
    // hushrun's store is not: its keys need a toolchain the program knows.
    assert_eq!(answers[&3]["Miss"], true);
}

// covers: B-40, B-47
#[test]
fn a_panic_while_starting_or_reading_never_ends_the_program_before_close() {
    let mut m = Machine::new().with_session(Some("none"), None);
    m.env.insert(FAULT_VAR.into(), "panic-start".into());
    let action = [4u8; 32];
    let lines = m.serve_steps(
        &[
            (1, put(1, &action, b"y")),
            (2, get(2, &action)),
            (3, close(3)),
        ],
        "go build ./...",
    );
    let answers = by_id(&lines);
    assert_eq!(answers.len(), 4, "{lines:?}");
    // It started again with no session: Go's own cache only.
    assert!(answers[&2]["DiskPath"].is_string());
    let mut m = Machine::new();
    m.env.insert(FAULT_VAR.into(), "panic-read=2".into());
    let lines = m.serve_steps(
        &[
            (1, put(1, &action, b"y")),
            (2, get(2, &action)),
            (3, get(3, &action)),
            (4, close(4)),
        ],
        "go build ./...",
    );
    let answers = by_id(&lines);
    assert_eq!(answers.len(), 5, "{lines:?}");
    assert!(answers[&2]["Err"].is_string());
    assert!(answers[&3]["DiskPath"].is_string());
}

// covers: B-40
#[test]
fn a_handler_that_hangs_or_panics_still_gets_one_answer() {
    // Long enough that a panic's handler, printing a backtrace on a loaded machine, answers well
    // before the watchdog would (CI has seen 938 ms); every wait below is in budgets.
    const BUDGET_MS: u64 = 3000;
    let budget = Duration::from_millis(BUDGET_MS);
    let mut m = Machine::new();
    m.env
        .insert(BUDGET_VAR.into(), BUDGET_MS.to_string().into());
    // Counted by command: the gets are requests 1, 2 and 4.
    m.env.insert(
        FAULT_VAR.into(),
        "hang-get=1,panic-get=2,panic-put=1,slow-get=3".into(),
    );
    let started = Instant::now();
    // The body a panicking put leaves, read before `close`, after which Go reads nothing.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let out = Out::default();
    let input = Steps {
        out: out.clone(),
        steps: [
            (1, get(1, &[1u8; 32])),
            (1, get(2, &[2u8; 32])),
            (1, put(3, &[3u8; 32], b"the body")),
            // Slower than the budget: the watchdog answers it, and its own answer, later, is
            // never written.
            (1, get(4, &[4u8; 32])),
            (5, String::new()),
            (5, close(5)),
        ]
        .into_iter()
        .collect(),
        now: Vec::new(),
    };
    let reader = {
        let out = out.clone();
        let seen = seen.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + budget * 3;
            let mut panicked_at = None;
            while Instant::now() < deadline {
                let lines = out.lines();
                if panicked_at.is_none() && lines.iter().any(|l| l["ID"] == 2) {
                    panicked_at = Some(started.elapsed());
                }
                if let Some(line) = lines.into_iter().find(|l| l["ID"] == 3) {
                    if let Some(path) = line["DiskPath"].as_str() {
                        *lock(&seen) = std::fs::read(path).unwrap_or_default();
                    }
                    return panicked_at.unwrap_or(started.elapsed());
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Duration::MAX
        })
    };
    serve_with(
        BufReader::new(input),
        Box::new(out.clone()),
        m.env.clone(),
        Box::new(|_| Ok("go build".into())),
    );
    // A panic is answered by its handler, not by the watchdog at the budget (never before a
    // tick past it): a loaded machine may take a while to print the panic.
    let panicked_at = reader.join().unwrap();
    assert!(panicked_at < budget - TICK * 2, "{panicked_at:?}");
    // The slow handler answers two budgets in, after `close`'s wait of one: given its time, it
    // writes nothing.
    std::thread::sleep(budget * 3 / 2);
    let lines = out.lines();
    let answers = by_id(&lines);
    assert_eq!(answers.len(), 6, "{lines:?}");
    // Hung: the watchdog's miss, within the budget and a few ticks.
    assert_eq!(answers[&1]["Miss"], true);
    assert!(started.elapsed() < budget * 5);
    // Panicked: a miss, and for a put the body in a file of the program's own.
    assert_eq!(answers[&2]["Miss"], true);
    assert!(answers[&3]["DiskPath"].is_string());
    assert_eq!(*lock(&seen), b"the body".to_vec());
    assert_eq!(answers[&4]["Miss"], true);
    assert_eq!(answers[&5], json!({"ID": 5}));
}

// covers: B-42
#[test]
fn a_block_in_the_store_is_served_only_when_its_bytes_are_what_its_build_id_says() {
    let m = Machine::new().with_session(Some("none"), None);
    let action = [5u8; 32];
    let honest = archive(&action, b"honest code", &[]);
    m.seed(&action, &honest);
    let lines = m.serve(&[get(1, &action), close(2)].concat(), "go build ./...");
    let answers = by_id(&lines);
    let path = PathBuf::from(answers[&1]["DiskPath"].as_str().unwrap());
    // Handed from the session's folder, named by its SHA-256 (F8).
    assert!(
        path.starts_with(m.current().folder.join(BODIES)),
        "{path:?}"
    );
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        Digest::of(&honest).hex()
    );
    assert_eq!(std::fs::read(&path).unwrap(), honest);
    let entries = m.current().entries();
    assert_eq!(entries.last().unwrap().outcome, Outcome::Reused);

    // F2: the content id no longer the bytes': refused, and gone from the store.
    let other = [6u8; 32];
    let mut forged = archive(&other, b"honest code", &[]);
    let last = forged.len() - 2;
    forged[last] ^= 1;
    let key = m.seed(&other, &forged);
    let lines = m.serve(&[get(1, &other), close(2)].concat(), "go build ./...");
    assert_eq!(by_id(&lines)[&1]["Miss"], true);
    assert!(m.store().block(&key).unwrap().is_none());

    // A file changed on disk since it was stored: not served either.
    let third = [7u8; 32];
    let key = m.seed(&third, &archive(&third, b"x", &[]));
    let block = m.store().block(&key).unwrap().unwrap();
    let file = &block.files[0].path;
    let mut perms = std::fs::metadata(file).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(file, perms).unwrap();
    let mut bytes = std::fs::read(file).unwrap();
    bytes[100] ^= 1;
    std::fs::write(file, bytes).unwrap();
    let lines = m.serve(&[get(1, &third), close(2)].concat(), "go build ./...");
    assert_eq!(by_id(&lines)[&1]["Miss"], true);
}

// covers: B-42
#[test]
fn an_altered_command_is_served_nothing_from_the_store_and_keeps_nothing() {
    let m = Machine::new().with_session(Some("none"), None);
    let action = [8u8; 32];
    m.seed(&action, &archive(&action, b"plain build", &[]));
    let lines = m.serve(
        &[get(1, &action), close(2)].concat(),
        "go build -toolexec=/x/tx.sh ./...",
    );
    assert_eq!(by_id(&lines)[&1]["Miss"], true);
}

// covers: B-44
#[test]
fn an_audit_answers_a_miss_once_a_run_and_a_difference_suspends_the_key() {
    let m = Machine::new().with_session(Some("every"), None);
    let action = [9u8; 32];
    let stored = archive(&action, b"what was stored", &[]);
    let key = m.seed(&action, &stored);
    // Go then compiles other bytes under the same action.
    let fresh = archive(&action, b"what Go compiles", &[]);
    let lines = m.serve_steps(
        &[
            (1, get(1, &action)),
            (2, put(2, &action, &fresh)),
            (3, close(3)),
        ],
        "go build ./...",
    );
    let answers = by_id(&lines);
    assert_eq!(answers[&1]["Miss"], true);
    assert!(answers[&2]["DiskPath"].is_string());
    let at = SystemTime::now();
    assert!(m.store().suspended_at(&key, at).is_some());
    let entry = m.current().entries().into_iter().last().unwrap();
    assert!(entry.differed && entry.found, "{entry:?}");
    // The same key is never audited twice in a run: the claim is the session's.
    assert!(!m.serve_claims_again(&key));
}

impl Machine {
    fn serve_claims_again(&self, key: &Digest) -> bool {
        let program = Program::new(
            Box::new(Out::default()),
            self.env.clone(),
            Box::new(|_| Ok("go build".into())),
        );
        program.claim_audit(self.current(), key)
    }
}

/// What the test relay does with each line, once it has passed the line on to the test.
#[derive(Clone, Copy)]
enum Says {
    /// Answers this line.
    Line(&'static str),
    /// Never answers: the asker's wait runs out.
    Nothing,
    /// Closes the connection unanswered: the asker's call fails at once.
    HangsUp,
}

/// A relay that takes every connection and does what `says` says with each line. A line is in
/// the returned channel before the relay answers it or hangs up, so an ask that came back has
/// been heard.
fn relay(dir: &Path, says: Says) -> (PathBuf, mpsc::Receiver<Value>) {
    let socket = dir.join("relay.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_ok() && !line.is_empty() {
                    let _ = tx.send(serde_json::from_str::<Value>(line.trim()).unwrap());
                    match says {
                        Says::Line(answer) => {
                            let _ = (&stream).write_all(format!("{answer}\n").as_bytes());
                        }
                        Says::Nothing => std::thread::sleep(Duration::from_secs(30)),
                        Says::HangsUp => {}
                    }
                }
            });
        }
    });
    (socket, rx)
}

/// What the relay heard until `done` holds for it, or until ten seconds passed. A notice is
/// written and left in the socket (`relay::tell`), so the relay can read it after the command
/// that told it has returned.
fn hear_until(heard: &mpsc::Receiver<Value>, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut told = Vec::new();
    while !done(&told) {
        let left = deadline.saturating_duration_since(Instant::now());
        match heard.recv_timeout(left) {
            Ok(t) => told.push(t),
            Err(_) => break,
        }
    }
    told
}

// covers: B-42, B-45
#[test]
fn a_portable_archive_put_by_a_sharing_run_becomes_a_block_and_the_relay_hears_of_it() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, heard) = relay(dir.path(), Says::Line(r#"{"answer":"none"}"#));
    let m = Machine::new().with_session(Some("none"), Some(&socket));
    let action = [10u8; 32];
    let body = archive(&action, b"shared code", &[]);
    let stdout = crate::go::subkey(&action, "stdout");
    let lines = m.serve_steps(
        &[
            (1, get(1, &action)),
            (2, put(2, &stdout, b"")),
            (3, put(3, &action, &body)),
            (4, close(4)),
        ],
        "go build ./...",
    );
    assert_eq!(by_id(&lines).len(), 5);
    let key = crate::go::key(&action, &m.identity());
    let block = m.store().block(&key).unwrap().expect("a block");
    assert_eq!(Kind::of_description(&block.description), Kind::Go);
    assert_eq!(std::fs::read(&block.files[0].path).unwrap(), body);
    let by = m.current().share.relay().unwrap().by.clone();
    assert!(m.store().origin(&key).is_some_and(|o| o.may_go_up_as(&by)));
    let built = |t: &Value| t["op"] == "go_built" && t["key"] == json!(key);
    let told = hear_until(&heard, |told| told.iter().any(built));
    assert!(told.iter().any(built), "{told:?}");
}

// covers: B-42
#[test]
fn an_archive_whose_compile_printed_or_that_holds_native_code_stays_here() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, heard) = relay(dir.path(), Says::Line(r#"{"answer":"none"}"#));
    let m = Machine::new().with_session(Some("none"), Some(&socket));
    // F4: the compile printed its inlining report.
    let printed = [11u8; 32];
    let native = [12u8; 32];
    let lines = m.serve_steps(
        &[
            (
                1,
                put(1, &crate::go::subkey(&printed, "stdout"), b"can inline F\n"),
            ),
            (2, put(2, &printed, &archive(&printed, b"p", &[]))),
            (3, put(3, &crate::go::subkey(&native, "stdout"), b"")),
            (
                4,
                put(
                    4,
                    &native,
                    &archive(&native, b"c", &[("_x001.o", b"\x7fELF".to_vec())]),
                ),
            ),
            (5, close(5)),
        ],
        "go build ./...",
    );
    assert_eq!(by_id(&lines).len(), 6);
    let identity = m.identity();
    for action in [printed, native] {
        let key = crate::go::key(&action, &identity);
        assert!(m.store().block(&key).unwrap().is_none());
    }
    let locals: Vec<Local> = m
        .current()
        .entries()
        .into_iter()
        .filter_map(|e| e.go.and_then(|g| g.local))
        .collect();
    assert_eq!(locals, vec![Local::Printed, Local::Native]);
    assert!(heard.try_iter().all(|t| t["op"] != "go_built"));
}

// covers: B-40
#[test]
fn after_one_relay_call_times_out_the_command_asks_it_nothing_more() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, heard) = relay(dir.path(), Says::Nothing);
    let mut m = Machine::new().with_session(Some("none"), Some(&socket));
    m.env.insert(BUDGET_VAR.into(), "600".into());
    let started = Instant::now();
    // One after another, as Go asks for a package once its imports are done.
    let lines = m.serve_steps(
        &[
            (1, get(1, &[13u8; 32])),
            (2, get(2, &[14u8; 32])),
            (3, get(3, &[15u8; 32])),
            (4, close(4)),
        ],
        "go build ./...",
    );
    let answers = by_id(&lines);
    for id in 1..=3 {
        assert_eq!(answers[&id]["Miss"], true);
    }
    // One wait for the relay in the command, never one a request.
    assert!(
        started.elapsed() < Duration::from_millis(550),
        "{:?}",
        started.elapsed()
    );
    std::thread::sleep(Duration::from_millis(50));
    // The fetch, and the quick question that told a frozen relay from a slow download.
    let asked: Vec<Value> = heard.try_iter().collect();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[1]["op"], "sharing");
    let lines = m.serve(&[get(1, &[16u8; 32]), close(2)].concat(), "go build ./...");
    assert_eq!(by_id(&lines)[&1]["Miss"], true);
}

// covers: B-40
#[test]
fn go_s_time_is_written_as_rfc_3339_in_utc() {
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(
        rfc3339(1_800_000_000_123_000_000),
        "2027-01-15T08:00:00.123Z"
    );
    assert_eq!(
        rfc3339(951_782_400_000_000_001),
        "2000-02-29T00:00:00.000000001Z"
    );
}

// covers: B-42
#[test]
fn a_body_handed_to_go_keeps_its_bytes_when_the_block_is_replaced() {
    let m = Machine::new().with_session(Some("none"), None);
    let action = [17u8; 32];
    let first = archive(&action, b"first", &[]);
    let key = m.seed(&action, &first);
    let lines = m.serve(&[get(1, &action), close(2)].concat(), "go build ./...");
    let handed = PathBuf::from(by_id(&lines)[&1]["DiskPath"].as_str().unwrap());
    // The block's folder changes places with another (B-32's replace).
    let other = m.dir.path().join("other");
    std::fs::write(&other, archive(&action, b"second", &[])).unwrap();
    m.store()
        .replace(
            &key,
            &m.store().block(&key).unwrap().unwrap().description,
            &[NewFile {
                name: ARCHIVE.into(),
                source: other,
            }],
            &Origin::built_here("later", None),
            SystemTime::now(),
            |_, _| true,
        )
        .unwrap();
    assert_eq!(std::fs::read(&handed).unwrap(), first);
}

// covers: B-40, B-44
#[test]
fn a_store_hit_the_watchdog_answered_first_records_no_reuse() {
    let mut m = Machine::new().with_session(Some("none"), None);
    let action = [20u8; 32];
    let key = m.seed(&action, &archive(&action, b"slow", &[]));
    m.env.insert(BUDGET_VAR.into(), "300".into());
    m.env.insert(FAULT_VAR.into(), "slow-get=1".into());
    let lines = m.serve(&[get(1, &action), close(2)].concat(), "go build ./...");
    assert_eq!(by_id(&lines)[&1]["Miss"], true);
    // The handler finishes after the watchdog's miss: give it its time.
    std::thread::sleep(Duration::from_millis(900));
    assert!(
        m.current()
            .entries()
            .iter()
            .all(|e| e.outcome != Outcome::Reused),
        "{:?}",
        m.current().entries()
    );
    let _ = key;
}

// covers: B-40
#[test]
fn a_relay_that_failed_is_asked_nothing_by_the_runs_later_go_commands() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, heard) = relay(dir.path(), Says::HangsUp);
    // The full budget: the watchdog never answers first, so `close` waits for the handler, which
    // marks the relay down before it is done.
    let m = Machine::new().with_session(Some("none"), Some(&socket));
    let lines = m.serve(&[get(1, &[30u8; 32]), close(2)].concat(), "go build ./...");
    assert_eq!(by_id(&lines)[&1]["Miss"], true);
    // The fetch, and the quick question that found the relay down.
    let first: Vec<Value> = heard.try_iter().map(|t| t["op"].clone()).collect();
    assert_eq!(first, [json!("go_fetch"), json!("sharing")]);
    // Another `go` command of the same run asks it nothing.
    let lines = m.serve(&[get(1, &[31u8; 32]), close(2)].concat(), "go build ./...");
    assert_eq!(by_id(&lines)[&1]["Miss"], true);
    assert_eq!(heard.try_iter().count(), 0);
}

// covers: B-40
#[test]
fn a_fetch_that_outlasts_its_wait_on_a_live_relay_leaves_it_on() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("relay.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, heard) = mpsc::channel::<Value>();
    // Quick questions are answered; a fetch takes longer than the program waits.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_ok() && !line.is_empty() {
                    let asked: Value = serde_json::from_str(line.trim()).unwrap();
                    let fetch = asked["op"] == "go_fetch";
                    let _ = tx.send(asked);
                    if fetch {
                        std::thread::sleep(Duration::from_secs(3));
                    }
                    let _ = (&stream).write_all(b"{\"answer\":\"none\"}\n");
                }
            });
        }
    });
    let mut m = Machine::new().with_session(Some("none"), Some(&socket));
    m.env.insert(BUDGET_VAR.into(), "600".into());
    let lines = m.serve_steps(
        &[
            (1, get(1, &[32u8; 32])),
            (2, get(2, &[33u8; 32])),
            (3, close(3)),
        ],
        "go build ./...",
    );
    assert_eq!(by_id(&lines).len(), 4);
    let fetches = |told: &[Value]| told.iter().filter(|a| a["op"] == "go_fetch").count();
    let asked = hear_until(&heard, |told| fetches(told) >= 2);
    assert_eq!(
        fetches(&asked),
        2,
        "the second get asked the relay too: {asked:?}"
    );
}
