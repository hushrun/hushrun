//! The helper's flow, with the hook played by the test: a lookup, the record a compile would
//! leave, its store, and a reuse in another checkout.

use super::*;

/// One machine's store and a session over checkouts under `dir`.
struct World {
    dir: PathBuf,
}

/// A checkout: a compiler file and two sources.
struct Checkout {
    root: PathBuf,
    session: PathBuf,
}

impl World {
    fn new() -> (tempfile::TempDir, World) {
        let temp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(temp.path()).unwrap();
        (temp, World { dir })
    }

    fn checkout(&self, name: &str, mode: &str) -> Checkout {
        let root = self.dir.join(name);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/typescript/lib")).unwrap();
        std::fs::write(root.join("node_modules/typescript/lib/_tsc.js"), "compiler").unwrap();
        std::fs::write(root.join("src/a.ts"), "export const a = 1;\n").unwrap();
        std::fs::write(root.join("tsconfig.json"), "{}").unwrap();
        let session = self.dir.join(format!("session-{name}"));
        std::fs::create_dir_all(&session).unwrap();
        let current = serde_json::json!({
            "mode": mode,
            "store": self.dir.join("store"),
            "keep_days": 15,
            "never_store": [],
            "env": [],
            "min_free_gb": 0,
            "repository": null,
            "started_ms": 0,
            "shared": [],
            "folder": session,
            "audit": "never",
            "tsc": {"roots": [root.to_string_lossy()]},
        });
        std::fs::write(session.join("session.json"), current.to_string()).unwrap();
        Checkout { root, session }
    }
}

impl Checkout {
    fn var(&self) -> impl Fn(&str) -> Option<OsString> + '_ {
        move |name: &str| (name == session::SESSION).then(|| self.session.clone().into_os_string())
    }

    fn path(&self, rel: &str) -> String {
        self.root.join(rel).to_string_lossy().into_owned()
    }

    fn call(&self, args: &[&str]) -> Vec<u8> {
        serde_json::to_vec(&Call {
            schema: CALL_SCHEMA.into(),
            program: "tsc".into(),
            package: self.path("node_modules/typescript"),
            version: "6.0.3".into(),
            compiler: self.path("node_modules/typescript/lib/_tsc.js"),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: self.root.to_string_lossy().into_owned(),
            tty: false,
            platform: "linux".into(),
            env: Default::default(),
            pid: 7,
        })
        .unwrap()
    }

    fn lookup(&self, args: &[&str]) -> Answer {
        lookup(&self.var(), SystemTime::now(), &self.call(args))
    }

    /// What the hook would record of `tsc` compiling `src/a.ts` into `dist/a.js`: the compile
    /// itself is played here, its output written.
    fn record(&self) -> Record {
        let read = |rel: &str| {
            let bytes = std::fs::read(self.root.join(rel)).unwrap();
            (self.path(rel), Digest::of(&bytes).hex(), bytes.len() as u64)
        };
        std::fs::create_dir_all(self.root.join("dist")).unwrap();
        let source = std::fs::read_to_string(self.root.join("src/a.ts")).unwrap();
        std::fs::write(self.root.join("dist/a.js"), format!("// out\n{source}")).unwrap();
        Record {
            schema: RECORD_SCHEMA.into(),
            code: 0,
            reads: vec![read("tsconfig.json"), read("src/a.ts")],
            looks: vec![
                (self.path("src/package.json"), "-".into()),
                (self.path("dist"), "-".into()),
            ],
            lists: vec![(self.path("src"), Some(vec![("a.ts".into(), "f".into())]))],
            links: vec![],
            writes: vec![self.path("dist/a.js")],
            stdout: format!("{}/src/a.ts: fine\n", self.root.display()),
            ..Record::default()
        }
    }

    /// Plays a compile after a lookup that answered `Record`, and stores it.
    fn compile(&self, answer: Answer, record: Record) {
        let Answer::Record { record: at, plan } = answer else {
            panic!("expected a record, got {answer:?}");
        };
        std::fs::write(&at, serde_json::to_vec(&record).unwrap()).unwrap();
        store(&self.var(), SystemTime::now(), Path::new(&plan));
        assert!(!Path::new(&plan).exists(), "the plan is removed");
        assert!(!Path::new(&at).exists(), "the record is removed");
    }

    fn log(&self) -> Vec<Entry> {
        std::fs::read_to_string(self.session.join("log.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn last_why(&self) -> Option<String> {
        self.log().last().and_then(|e| e.tsc.clone()).and_then(|t| t.why)
    }
}

// covers: B-103, B-105
#[test]
fn a_compile_stored_in_one_checkout_is_put_in_place_in_another() {
    let (_t, world) = World::new();
    let a = world.checkout("a", "on");
    a.compile(a.lookup(&[]), a.record());
    assert_eq!(a.log().last().unwrap().outcome, Outcome::BuiltAndStored);
    let b = world.checkout("b", "on");
    let Answer::Reused { stdout, code, .. } = b.lookup(&[]) else {
        panic!("expected a reuse: {:?}", b.log());
    };
    assert_eq!(code, 0);
    // The messages name this checkout's files.
    assert_eq!(stdout, format!("{}/src/a.ts: fine\n", b.root.display()));
    assert_eq!(
        std::fs::read_to_string(b.root.join("dist/a.js")).unwrap(),
        "// out\nexport const a = 1;\n"
    );
    let entry = b.log().pop().unwrap();
    assert_eq!(entry.outcome, Outcome::Reused);
    assert_eq!(entry.tsc.unwrap().files, 1);
}

// covers: B-103
#[test]
fn what_changed_since_is_a_compile() {
    let (_t, world) = World::new();
    let a = world.checkout("a", "on");
    a.compile(a.lookup(&[]), a.record());
    // Another source.
    let b = world.checkout("b", "on");
    std::fs::write(b.root.join("src/a.ts"), "export const a = 2;\n").unwrap();
    assert!(matches!(b.lookup(&[]), Answer::Record { .. }));
    // A file where one was looked for and not found.
    let c = world.checkout("c", "on");
    std::fs::write(c.root.join("src/package.json"), "{\"type\":\"module\"}").unwrap();
    assert!(matches!(c.lookup(&[]), Answer::Record { .. }));
    // A new file in a folder the compiler listed.
    let d = world.checkout("d", "on");
    std::fs::write(d.root.join("src/b.ts"), "").unwrap();
    assert!(matches!(d.lookup(&[]), Answer::Record { .. }));
    // Other arguments.
    let e = world.checkout("e", "on");
    assert!(matches!(e.lookup(&["--noEmit"]), Answer::Record { .. }));
    // Another compiler.
    let f = world.checkout("f", "on");
    std::fs::write(f.root.join("node_modules/typescript/lib/_tsc.js"), "other").unwrap();
    assert!(matches!(f.lookup(&[]), Answer::Record { .. }));
}

// covers: B-101, B-106
#[test]
fn calls_it_doesn_t_handle_pass_through_and_say_why() {
    let (_t, world) = World::new();
    let a = world.checkout("a", "on");
    assert_eq!(a.lookup(&["--watch"]), Answer::Pass);
    assert_eq!(a.last_why().as_deref(), Some("watch"));
    // No session: nothing at all.
    let none = |_: &str| None;
    assert_eq!(lookup(&none, SystemTime::now(), &a.call(&[])), Answer::Pass);
    // Not a call.
    assert_eq!(lookup(&a.var(), SystemTime::now(), b"{"), Answer::Pass);
    // A compiler that can't be read.
    std::fs::remove_file(a.root.join("node_modules/typescript/lib/_tsc.js")).unwrap();
    assert_eq!(a.lookup(&[]), Answer::Pass);
    assert_eq!(a.last_why().as_deref(), Some("compiler"));
}

// covers: B-103, B-104
#[test]
fn what_a_block_can_t_hold_is_not_stored_with_its_reason() {
    let (_t, world) = World::new();
    let a = world.checkout("a", "on");
    let cases: Vec<(Box<dyn Fn(&mut Record)>, &str)> = vec![
        (Box::new(|r| r.other.push("unlink".into())), "unlink"),
        (Box::new(|r| r.errors.push("boom".into())), "hook-error"),
        (Box::new(|r| r.code = 7), "status"),
        (
            Box::new(|r| r.writes.push("/elsewhere/x.js".into())),
            "write-outside-roots",
        ),
        (Box::new(|r| r.stdout.push_str("{root}")), "placeholder"),
    ];
    for (change, why) in cases {
        let mut record = a.record();
        change(&mut record);
        a.compile(a.lookup(&[]), record);
        assert_eq!(a.last_why().as_deref(), Some(why));
    }
    // `--build` over a build's state decides by dates.
    let mut record = a.record();
    std::fs::write(a.root.join("tsconfig.tsbuildinfo"), "{}").unwrap();
    record.reads.push((a.path("tsconfig.tsbuildinfo"), Digest::of(b"{}").hex(), 2));
    a.compile(a.lookup(&["-b"]), record);
    assert_eq!(a.last_why().as_deref(), Some("build-over-outputs"));
    assert!(Store::new(&world.dir.join("store")).keys().is_empty());
}

// covers: B-104
#[test]
fn an_output_that_holds_the_checkout_s_path_is_tied_to_it() {
    let (_t, world) = World::new();
    let a = world.checkout("a", "on");
    let record = a.record();
    std::fs::write(
        a.root.join("dist/a.js"),
        format!("//# sourceRoot={}\n", a.root.display()),
    )
    .unwrap();
    let answer = a.lookup(&[]);
    let Answer::Record { record: at, plan } = answer else {
        panic!()
    };
    std::fs::write(&at, serde_json::to_vec(&record).unwrap()).unwrap();
    store(&a.var(), SystemTime::now(), Path::new(&plan));
    let tsc = a.log().pop().unwrap().tsc.unwrap();
    assert_eq!(tsc.local, vec!["tied"]);
    // Another checkout finds nothing; this one does.
    let b = world.checkout("b", "on");
    assert!(matches!(b.lookup(&[]), Answer::Record { .. }));
    std::fs::remove_dir_all(a.root.join("dist")).unwrap();
    assert!(matches!(a.lookup(&[]), Answer::Reused { .. }));
}

// covers: B-107
#[test]
fn a_type_check_that_writes_nothing_is_reused_with_its_messages_and_status() {
    let (_t, world) = World::new();
    let a = world.checkout("a", "on");
    let mut record = a.record();
    record.writes.clear();
    record.code = 2;
    record.stdout = "src/a.ts(1,1): error TS2322\n".into();
    a.compile(a.lookup(&["--noEmit"]), record);
    let b = world.checkout("b", "on");
    assert_eq!(
        b.lookup(&["--noEmit"]),
        Answer::Reused {
            stdout: "src/a.ts(1,1): error TS2322\n".into(),
            stderr: String::new(),
            code: 2
        }
    );
    assert!(!b.root.join("dist").exists());
}

// covers: B-105
#[test]
fn in_observe_a_block_found_is_compared_and_a_difference_suspends_the_key() {
    let (_t, world) = World::new();
    let a = world.checkout("a", "on");
    a.compile(a.lookup(&[]), a.record());
    let b = world.checkout("b", "observe");
    let answer = b.lookup(&[]);
    assert!(matches!(answer, Answer::Record { .. }), "observe never reuses");
    let mut record = b.record();
    std::fs::write(b.root.join("dist/a.js"), "other").unwrap();
    record.stdout = format!("{}/src/a.ts: fine\n", b.root.display());
    b.compile(answer, record);
    let entry = b.log().pop().unwrap();
    assert!(entry.found && entry.differed, "{entry:?}");
    let c = world.checkout("c", "on");
    assert!(matches!(c.lookup(&[]), Answer::Record { .. }), "suspended");
}

// covers: B-105
#[test]
fn a_block_whose_file_changed_in_the_store_is_a_compile() {
    let (_t, world) = World::new();
    let a = world.checkout("a", "on");
    a.compile(a.lookup(&[]), a.record());
    let store = Store::new(&world.dir.join("store"));
    let key = store.keys()[0];
    let block = store.block(&key).unwrap().unwrap();
    let file = &block.files.iter().find(|f| f.name == "o/0").unwrap().path;
    let mut perms = std::fs::metadata(file).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(file, perms).unwrap();
    std::fs::write(file, "// bad\nexport const a = 1;\n").unwrap();
    let b = world.checkout("b", "on");
    assert_eq!(b.lookup(&[]), Answer::Pass);
    assert_eq!(b.last_why().as_deref(), Some("restore"));
}
