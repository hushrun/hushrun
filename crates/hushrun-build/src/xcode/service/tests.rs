//! The service's answers, as Xcode's plugin would ask: objects, values, the cache on this
//! machine, blocks in hushrun's store and the audit.

use std::ffi::OsString;

use super::*;
use crate::session::{Session, Start};
use crate::settings;
use crate::xcode::proto::Object as Wire;

/// One machine: a store, and a session when the test asks for one.
struct Machine {
    dir: tempfile::TempDir,
    session: Option<Session>,
}

impl Machine {
    fn new() -> Machine {
        Machine {
            dir: tempfile::tempdir().unwrap(),
            session: None,
        }
    }

    /// A run's session with reuse `on`, and the audit as `audit` says.
    fn with_session(mut self, audit: Option<&str>) -> Machine {
        let settings = self.dir.path().join("build.json");
        settings::update(&settings, |s| s.mode = Mode::On).unwrap();
        let exe = self.dir.path().join("hushrun");
        std::fs::write(&exe, b"binary").unwrap();
        let audit = audit.map(OsString::from);
        let env = move |name: &str| (name == crate::audit::VAR).then(|| audit.clone()).flatten();
        let cache = self.dir.path().join("cache");
        let session = crate::session::begin(&Start {
            settings: &settings,
            cache_dir: Some(&cache),
            exe: &exe,
            in_ci: false,
            repository: Some("example.com/acme/app"),
            repository_env: &[],
            env: &env,
            now: SystemTime::now(),
        })
        .unwrap();
        self.session = Some(session);
        self
    }

    fn store_dir(&self) -> PathBuf {
        match &self.session {
            Some(session) => session.current().store.clone(),
            None => self.dir.path().join("store"),
        }
    }

    fn service(&self, folders: &[&str]) -> Service {
        Service::new(Config {
            store: self.store_dir(),
            current: self.session.as_ref().map(|s| s.current().clone()),
            platform: Some("xcode-27c5032a".into()),
            folders: folders.iter().map(|f| f.to_string()).collect(),
            staging: self.dir.path().join("staging"),
        })
    }

    fn log(&self) -> Vec<Entry> {
        let path = self
            .session
            .as_ref()
            .unwrap()
            .current()
            .folder
            .join("log.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn now() -> SystemTime {
    SystemTime::now()
}

/// What Xcode's plugin does for one compile it made: puts its outputs, then the value naming
/// them. The value.
fn compile(service: &Service, output: &[u8]) -> (Vec<u8>, Value) {
    let (reply, _) = service.answer(Request::CasSave(Bytes::Data(output.to_vec())), now());
    let Reply::CasStored(Ok(object)) = reply else {
        panic!("save: {reply:?}");
    };
    let (reply, _) = service.answer(
        Request::CasPut(proto::Object {
            blob: Bytes::Data(b"outputs".to_vec()),
            references: vec![object],
        }),
        now(),
    );
    let Reply::CasStored(Ok(outputs)) = reply else {
        panic!("put: {reply:?}");
    };
    let result = Wire {
        blob: Bytes::Data(b"result".to_vec()),
        references: vec![outputs],
    };
    let value: Value = [("value".to_string(), proto::encode_object(&result))].into();
    let key = Digest::of(output).as_bytes().to_vec();
    let (reply, after) = service.answer(
        Request::KvPut {
            key: key.clone(),
            value: value.clone(),
        },
        now(),
    );
    assert_eq!(reply, Reply::KvPut(Ok(())));
    service.after(after, now());
    (key, value)
}

fn get(service: &Service, key: &[u8]) -> Option<Value> {
    let (reply, after) = service.answer(Request::KvGet { key: key.to_vec() }, now());
    service.after(after, now());
    match reply {
        Reply::KvGot(Ok(value)) => value,
        other => panic!("get: {other:?}"),
    }
}

// covers: B-78, B-79
#[test]
fn objects_are_named_by_their_content_and_got_back() {
    let machine = Machine::new();
    let service = machine.service(&[]);
    let (reply, _) = service.answer(Request::CasSave(Bytes::Data(b"blob".to_vec())), now());
    let Reply::CasStored(Ok(id)) = reply else {
        panic!("{reply:?}");
    };
    assert_eq!(id, cas::id(b"blob", &[]).to_vec());
    let (reply, _) = service.answer(
        Request::CasLoad {
            id: id.clone(),
            write_to_disk: true,
        },
        now(),
    );
    assert_eq!(
        reply,
        Reply::CasLoaded(Ok(Some(Bytes::Data(b"blob".to_vec()))))
    );
    let (reply, _) = service.answer(
        Request::CasPut(proto::Object {
            blob: Bytes::Data(b"tree".to_vec()),
            references: vec![id.clone()],
        }),
        now(),
    );
    let Reply::CasStored(Ok(tree)) = reply else {
        panic!("{reply:?}");
    };
    let (reply, _) = service.answer(
        Request::CasGet {
            id: tree,
            write_to_disk: false,
        },
        now(),
    );
    assert_eq!(
        reply,
        Reply::CasGot(Ok(Some(proto::Object {
            blob: Bytes::Data(b"tree".to_vec()),
            references: vec![id],
        })))
    );
    // An id it never gave, and a reference to an object it doesn't hold.
    let (reply, _) = service.answer(
        Request::CasGet {
            id: vec![1; 5],
            write_to_disk: false,
        },
        now(),
    );
    assert_eq!(reply, Reply::CasGot(Ok(None)));
    let (reply, _) = service.answer(
        Request::CasPut(proto::Object {
            blob: Bytes::Data(b"x".to_vec()),
            references: vec![vec![2; 32]],
        }),
        now(),
    );
    assert!(matches!(reply, Reply::CasStored(Err(_))));
}

// covers: B-78
#[test]
fn a_put_may_name_a_file_the_plugin_wrote() {
    let machine = Machine::new();
    let service = machine.service(&[]);
    let file = machine.dir.path().join("out.blob");
    std::fs::write(&file, b"from a file").unwrap();
    let (reply, _) = service.answer(
        Request::CasSave(Bytes::FilePath(file.to_string_lossy().into_owned())),
        now(),
    );
    assert_eq!(
        reply,
        Reply::CasStored(Ok(cas::id(b"from a file", &[]).to_vec()))
    );
    let (reply, _) = service.answer(
        Request::CasSave(Bytes::FilePath("/no/such/file".into())),
        now(),
    );
    assert!(matches!(reply, Reply::CasStored(Err(_))));
}

// covers: B-79
#[test]
fn another_checkouts_compile_is_found_in_this_machines_cache() {
    let machine = Machine::new();
    let first = machine.service(&[]);
    let (key, value) = compile(&first, b"object code");
    // Another service on the same store: another worktree's build, or the Xcode app.
    let second = machine.service(&[]);
    assert_eq!(get(&second, &key), Some(value));
    assert_eq!(second.counts().local, 1);
    // A key nobody put.
    assert_eq!(get(&second, b"other key"), None);
}

// covers: B-79
#[test]
fn a_value_whose_outputs_are_gone_is_a_miss() {
    let machine = Machine::new();
    let service = machine.service(&[]);
    let (key, _) = compile(&service, b"object code");
    let objects = service.cache().root().join("objects");
    std::fs::remove_dir_all(&objects).unwrap();
    assert_eq!(get(&service, &key), None);
}

// covers: B-80, B-82
#[test]
fn a_value_put_in_a_run_becomes_a_block_and_serves_a_fresh_cache() {
    let machine = Machine::new().with_session(Some("none"));
    let service = machine.service(&["/Users/ana"]);
    let (key, value) = compile(&service, b"object code");
    assert_eq!(service.counts().stored, 1);
    let store = Store::new(&machine.store_dir());
    let block_key = crate::xcode::key(&key);
    let block = store.block(&block_key).unwrap().unwrap();
    assert_eq!(Kind::of_description(&block.description), Kind::Xcode);
    // value, the outputs object, and the object it names: the value's closure.
    assert_eq!(block.files.len(), 3);

    // The cache emptied (a fresh DerivedData is not this: the cache is the service's; a CI job's
    // store that came from a baked image, or a machine whose cache was trimmed, is).
    std::fs::remove_dir_all(service.cache().root()).unwrap();
    let again = machine.service(&["/Users/ana"]);
    assert_eq!(get(&again, &key), Some(value));
    assert_eq!(again.counts().reused, 1);
    let log = machine.log();
    assert!(log.iter().any(|e| e.outcome == Outcome::Reused
        && e.kind() == Kind::Xcode
        && e.xcode.as_ref().is_some_and(|x| x.objects == 2)));
    // And its objects are this machine's cache's from then on.
    let (reply, _) = again.answer(
        Request::CasLoad {
            id: cas::id(b"object code", &[]).to_vec(),
            write_to_disk: false,
        },
        now(),
    );
    assert_eq!(
        reply,
        Reply::CasLoaded(Ok(Some(Bytes::Data(b"object code".to_vec()))))
    );
}

// covers: B-82
#[test]
fn an_output_holding_a_folder_of_this_machine_stays_here() {
    let machine = Machine::new().with_session(Some("none"));
    let service = machine.service(&["/Users/ana"]);
    let (key, _) = compile(&service, b"debug info: /Users/ana/src/app/main.swift");
    assert_eq!(service.counts().stored, 0);
    let store = Store::new(&machine.store_dir());
    assert!(store.block(&crate::xcode::key(&key)).unwrap().is_none());
    assert!(machine.log().iter().any(|e| {
        e.xcode
            .as_ref()
            .is_some_and(|x| x.local.as_deref() == Some("holds-path"))
    }));
    // Still this machine's: another checkout here is served from the cache, and its line says it
    // stays here, so no manifest names a block no server has.
    assert!(get(&machine.service(&["/Users/ana"]), &key).is_some());
    let served = machine
        .log()
        .into_iter()
        .rfind(|e| e.outcome == Outcome::Reused)
        .unwrap();
    let xcode = served.xcode.unwrap();
    assert!(xcode.local_cache);
    assert_eq!(xcode.local.as_deref(), Some("cache"));
}

// covers: B-84
#[test]
fn the_audit_compares_what_xcode_compiles_again() {
    let machine = Machine::new().with_session(Some("every"));
    let service = machine.service(&[]);
    let (key, value) = compile(&service, b"object code");
    std::fs::remove_dir_all(service.cache().root()).unwrap();
    let audited = machine.service(&[]);
    // Picked: a miss on purpose.
    assert_eq!(get(&audited, &key), None);
    // Xcode compiles it again, to the same value: nothing differs, and the block stays.
    let (reply, after) = audited.answer(
        Request::KvPut {
            key: key.clone(),
            value: value.clone(),
        },
        now(),
    );
    assert_eq!(reply, Reply::KvPut(Ok(())));
    audited.after(after, now());
    assert_eq!(audited.counts().differed, 0);
    let store = Store::new(&machine.store_dir());
    let block_key = crate::xcode::key(&key);
    assert!(store.suspended_at(&block_key, now()).is_none());

    // Another key, compiled again to another value: suspended.
    let (other_key, _) = compile(&service, b"other code");
    std::fs::remove_dir_all(service.cache().root()).unwrap();
    let auditing = machine.service(&[]);
    assert_eq!(get(&auditing, &other_key), None);
    let differing: Value = [("value".to_string(), b"not the same".to_vec())].into();
    let (_, after) = auditing.answer(
        Request::KvPut {
            key: other_key.clone(),
            value: differing,
        },
        now(),
    );
    auditing.after(after, now());
    assert_eq!(auditing.counts().differed, 1);
    assert!(
        store
            .suspended_at(&crate::xcode::key(&other_key), now())
            .is_some()
    );
    // Never twice for one key in a run: the next service of the run serves it.
    std::fs::remove_dir_all(service.cache().root()).unwrap();
    assert!(get(&machine.service(&[]), &key).is_some());
}

// covers: B-83
#[test]
fn with_no_relay_a_miss_is_a_miss() {
    let machine = Machine::new().with_session(Some("none"));
    let service = machine.service(&[]);
    assert_eq!(get(&service, b"never put"), None);
    assert_eq!(service.counts().asked, 1);
}
