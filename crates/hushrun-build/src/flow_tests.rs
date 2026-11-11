//! The wrapper's whole flow, driven in-process with a fake compiler behind its ports. Each test
//! names the transition it guards and fails on a naive version of it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use hushrun_core::Digest;

use crate::model::{Mode, NotStored, Outcome, PassedThrough};
use crate::session::{self, Entry, Session, Start};
use crate::settings;
use crate::store::Store;
use crate::wrapper::{Ports, Ran, Run, call};

/// What the fake compiler does for one crate.
#[derive(Clone, Default)]
struct Plan {
    /// Files read besides the source, as given to the compiler (relative to the working
    /// directory).
    reads: Vec<String>,
    /// Variables read with `env!` / `option_env!`; their values go into the outputs.
    env_reads: Vec<String>,
    /// A variable whose raw value is written into the outputs (a path kept for run time).
    embed_var: Option<String>,
    /// A variable naming a folder and a file in it that is included, as
    /// `include!(concat!(env!("OUT_DIR"), "/gen.rs"))` does.
    include_var: Option<(String, String)>,
    /// Hash the raw value of the included variable into the outputs: a path that changes the
    /// bytes even when remapped.
    hash_var: bool,
    /// Write a warning about the source.
    warn: bool,
    /// Fail with this error code.
    fail: Option<&'static str>,
    /// The failure's message, `failed` when unset.
    fail_message: Option<String>,
    /// What the compiler prints on stdout, with `{ws}` for the working directory: a proc
    /// macro's own printing, the linker command `--print=link-args` asks for.
    stdout: Option<String>,
}

type During = Box<dyn FnMut()>;

/// What the fake world saw.
#[derive(Default)]
struct Seen {
    compiles: Vec<Vec<String>>,
    execs: Vec<Vec<String>>,
    /// What the wrapper wrote to its stdout.
    stdout: Vec<u8>,
}

struct Fake {
    vars: BTreeMap<String, String>,
    cwd: PathBuf,
    plans: Rc<RefCell<BTreeMap<String, Plan>>>,
    during: Option<During>,
    seen: Rc<RefCell<Seen>>,
    version_fails: bool,
    audit: bool,
    /// Runs when the lookup found a block: between the lookup and the compile.
    on_found: RefCell<Option<During>>,
}

/// Applies `--remap-path-prefix` pairs as rustc does: the last one that holds the path wins.
fn remap(path: &str, remaps: &[(String, String)]) -> String {
    for (from, to) in remaps.iter().rev() {
        if let Some(rest) = path.strip_prefix(from.as_str())
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return format!("{to}{rest}");
        }
    }
    path.to_string()
}

impl Fake {
    fn var(&self, name: &str, env: &[(String, String)]) -> Option<String> {
        env.iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .or_else(|| self.vars.get(name).cloned())
    }

    fn absolute(&self, path: &str) -> PathBuf {
        if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.cwd.join(path)
        }
    }

    /// A compiler that writes outputs derived from what it read, a dep-info and JSON lines.
    fn rustc(&mut self, args: &[String], env: &[(String, String)]) -> (Vec<String>, i32) {
        let mut crate_name = String::new();
        let mut out_dir = String::new();
        let mut extra = String::new();
        let mut emits = vec!["link".to_string()];
        let mut source = String::new();
        let mut remaps = Vec::new();
        let mut externs = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();
            let mut value = || {
                i += 1;
                args.get(i).cloned().unwrap_or_default()
            };
            match arg {
                "--crate-name" => crate_name = value(),
                "--out-dir" => out_dir = value(),
                "--crate-type" | "-L" | "-l" | "--json" | "--color" | "--target" => {
                    value();
                }
                "-C" => {
                    let v = value();
                    if let Some(e) = v.strip_prefix("extra-filename=") {
                        extra = e.to_string();
                    }
                }
                "--extern" => {
                    let v = value();
                    if let Some((name, path)) = v.split_once('=') {
                        externs.push((name.to_string(), path.to_string()));
                    }
                }
                _ if arg.starts_with("--emit=") => {
                    emits = arg["--emit=".len()..]
                        .split(',')
                        .map(str::to_string)
                        .collect();
                }
                _ if arg.starts_with("--remap-path-prefix=") => {
                    let pair = &arg["--remap-path-prefix=".len()..];
                    if let Some((from, to)) = pair.split_once('=') {
                        remaps.push((from.to_string(), to.to_string()));
                    }
                }
                _ if arg.starts_with('-') => {}
                _ => source = arg.to_string(),
            }
            i += 1;
        }
        if crate_name.is_empty() {
            return (Vec::new(), 0);
        }
        let plan = self
            .plans
            .borrow()
            .get(&crate_name)
            .cloned()
            .unwrap_or_default();
        let mut lines = Vec::new();
        if let Some(code) = plan.fail {
            let message = serde_json::Value::from(plan.fail_message.as_deref().unwrap_or("failed"));
            let rendered = serde_json::Value::from(format!(
                "error[{code}]: {}\n",
                message.as_str().unwrap_or("")
            ));
            lines.push(format!(
                r#"{{"$message_type":"diagnostic","message":{message},"code":{{"code":"{code}","explanation":null}},"level":"error","spans":[],"children":[],"rendered":{rendered}}}"#
            ));
            return (lines, 1);
        }
        let mut content = format!(
            "crate {crate_name}\nsource {}\n",
            remap(self.absolute(&source).to_str().unwrap(), &remaps)
        );
        let mut deps = vec![source.clone()];
        deps.extend(plan.reads.iter().cloned());
        for file in &deps {
            match fs::read_to_string(self.absolute(file)) {
                Ok(text) => content.push_str(&text),
                Err(_) => return (vec![format!("error: can't read {file}")], 1),
            }
        }
        for (name, path) in &externs {
            let bytes = fs::read(path).unwrap_or_default();
            content.push_str(&format!("extern {name} {}\n", Digest::of(&bytes).hex()));
        }
        let mut env_deps: Vec<(String, Option<String>)> = Vec::new();
        for name in &plan.env_reads {
            let value = self.var(name, env);
            content.push_str(&format!("env {name}={value:?}\n"));
            env_deps.push((name.clone(), value));
        }
        if let Some(name) = &plan.embed_var {
            let value = self.var(name, env).unwrap_or_default();
            content.push_str(&format!("embedded {value}\n"));
            env_deps.push((name.clone(), Some(value)));
        }
        if let Some((name, file)) = &plan.include_var {
            let value = self.var(name, env).unwrap_or_default();
            let path = format!("{value}/{file}");
            content.push_str(&fs::read_to_string(&path).unwrap_or_default());
            content.push_str(&format!("included {}\n", remap(&path, &remaps)));
            if plan.hash_var {
                content.push_str(&format!("hashed {}\n", Digest::of(value.as_bytes()).hex()));
            }
            deps.push(path);
            env_deps.push((name.clone(), Some(value)));
        }
        if let Some(during) = self.during.as_mut() {
            during();
        }
        let out = PathBuf::from(&out_dir);
        let stem = format!("{crate_name}{extra}");
        let mut targets = Vec::new();
        let mut notices = Vec::new();
        for emit in &emits {
            let (path, bytes) = match emit.as_str() {
                "dep-info" => (out.join(format!("{stem}.d")), None),
                "metadata" => (
                    out.join(format!("lib{stem}.rmeta")),
                    Some(format!("rmeta\n{content}")),
                ),
                _ => (
                    out.join(format!("lib{stem}.rlib")),
                    Some(format!("rlib\n{content}")),
                ),
            };
            if let Some(bytes) = bytes {
                // rustc writes a new file each time.
                let _ = fs::remove_file(&path);
                fs::write(&path, bytes).unwrap();
            }
            targets.push(path.to_str().unwrap().to_string());
            notices.push(format!(
                r#"{{"$message_type":"artifact","artifact":"{}","emit":"{emit}"}}"#,
                path.display()
            ));
        }
        let info = crate::depinfo::DepInfo {
            targets,
            files: deps,
            env: env_deps
                .into_iter()
                .map(|(name, value)| crate::depinfo::EnvDep { name, value })
                .collect(),
        };
        if emits.iter().any(|e| e == "dep-info") {
            fs::write(out.join(format!("{stem}.d")), crate::depinfo::render(&info)).unwrap();
        }
        if plan.warn {
            lines.push(format!(
                r#"{{"$message_type":"diagnostic","message":"unused variable: `x`","code":{{"code":"unused_variables","explanation":null}},"level":"warning","spans":[{{"file_name":"{source}","byte_start":0,"byte_end":1,"line_start":1,"line_end":1,"column_start":1,"column_end":2,"is_primary":true,"text":[],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}}],"children":[],"rendered":"warning: unused variable: `x`\n --> {source}:1:1\n"}}"#
            ));
        }
        lines.extend(notices);
        (lines, 0)
    }
}

impl Fake {
    /// The planned stdout of the crate `args` compile, when it asked for one.
    fn planned_stdout(&self, args: &[String]) -> Vec<u8> {
        let name = args
            .iter()
            .position(|a| a == "--crate-name")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default();
        let plans = self.plans.borrow();
        let Some(text) = plans.get(&name).and_then(|p| p.stdout.clone()) else {
            return Vec::new();
        };
        text.replace("{ws}", self.cwd.to_str().unwrap())
            .into_bytes()
    }
}

fn strings(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_str().unwrap().to_string())
        .collect()
}

impl Ports for Fake {
    fn var(&self, name: &str) -> Option<OsString> {
        self.vars.get(name).map(OsString::from)
    }

    fn vars(&self) -> Vec<(OsString, OsString)> {
        self.vars
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    fn cwd(&self) -> io::Result<PathBuf> {
        Ok(self.cwd.clone())
    }

    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn pid(&self) -> u32 {
        4242
    }

    fn rustc_version(&self, _rustc: &Path) -> io::Result<String> {
        if self.version_fails {
            return Err(io::Error::other("no"));
        }
        Ok(
            "rustc 1.94.0 (fake)\nbinary: rustc\nhost: aarch64-apple-darwin\nrelease: 1.94.0\n"
                .into(),
        )
    }

    fn rustc_targets(&self, _rustc: &Path) -> io::Result<String> {
        Ok("aarch64-apple-darwin\naarch64-apple-ios\nx86_64-unknown-linux-gnu\n".into())
    }

    fn stdout(&mut self, bytes: &[u8]) {
        self.seen.borrow_mut().stdout.extend_from_slice(bytes);
    }

    fn exec(&mut self, _rustc: &OsStr, args: &[OsString]) -> i32 {
        let args = strings(args);
        self.seen.borrow_mut().execs.push(args.clone());
        self.rustc(&args, &[]).1
    }

    fn compile(&mut self, run: &Run<'_>, line: &mut dyn FnMut(&str)) -> io::Result<Ran> {
        let args = strings(run.args);
        let env: Vec<(String, String)> = run
            .env
            .iter()
            .map(|(k, v)| (k.to_str().unwrap().into(), v.to_str().unwrap().into()))
            .collect();
        self.seen.borrow_mut().compiles.push(args.clone());
        let (lines, code) = self.rustc(&args, &env);
        for l in &lines {
            line(l);
        }
        let stdout = self.planned_stdout(&args);
        Ok(Ran {
            code,
            success: code == 0,
            stdout,
        })
    }

    fn audited(&self, _key: &Digest, _started_ms: u64) -> bool {
        if let Some(found) = self.on_found.borrow_mut().as_mut() {
            found();
        }
        self.audit
    }
}

/// A store, a session, a fake toolchain and checkouts.
struct World {
    dir: tempfile::TempDir,
    session: Session,
    rustc: PathBuf,
    plans: Rc<RefCell<BTreeMap<String, Plan>>>,
}

/// What one call did.
struct Result {
    code: i32,
    entry: Option<Entry>,
    stderr: String,
    seen: Seen,
}

impl Result {
    fn outcome(&self) -> Outcome {
        self.entry.as_ref().unwrap().outcome
    }
}

fn world(mode: Mode) -> World {
    world_with(mode, |_| {})
}

fn world_with(mode: Mode, change: impl FnOnce(&mut settings::Settings)) -> World {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("build.json");
    settings::update(&settings, |s| {
        s.mode = mode;
        s.min_free_gb = 0;
        change(s);
    })
    .unwrap();
    let exe = dir.path().join("hushrun");
    fs::write(&exe, b"binary").unwrap();
    let cache = dir.path().join("cache");
    let session = session::begin(&Start {
        settings: &settings,
        cache_dir: Some(&cache),
        exe: &exe,
        in_ci: false,
        repository: Some("test/repo"),
        env: &|_| None,
        now: SystemTime::now(),
    })
    .unwrap();
    let rustc = dir.path().join("toolchain").join("bin").join("rustc");
    fs::create_dir_all(rustc.parent().unwrap()).unwrap();
    fs::write(&rustc, b"fake rustc").unwrap();
    World {
        dir,
        session,
        rustc,
        plans: Rc::default(),
    }
}

/// Sets file dates far enough back that the compile's start is after them (B-11).
fn settle() {
    std::thread::sleep(Duration::from_millis(30));
}

impl World {
    fn store(&self) -> Store {
        Store::new(&self.session.current().store)
    }

    fn plan(&self, crate_name: &str, plan: Plan) {
        self.plans.borrow_mut().insert(crate_name.into(), plan);
    }

    /// A checkout with library `a`: `a/src/lib.rs`, `a/src/foo.rs`, `a/data/text.txt`.
    fn checkout(&self, name: &str) -> PathBuf {
        let ws = self.dir.path().join(name);
        fs::create_dir_all(ws.join("a/src")).unwrap();
        fs::create_dir_all(ws.join("a/data")).unwrap();
        fs::create_dir_all(ws.join("target/debug/deps")).unwrap();
        fs::write(ws.join("a/src/lib.rs"), "mod foo;\n").unwrap();
        fs::write(ws.join("a/src/foo.rs"), "pub fn f() {}\n").unwrap();
        fs::write(ws.join("a/data/text.txt"), "hello\n").unwrap();
        let ws = crate::roots::resolve_on_disk(ws.to_str().unwrap()).unwrap();
        PathBuf::from(ws)
    }

    fn args(&self, ws: &Path, crate_name: &str, more: &[&str]) -> Vec<OsString> {
        let deps = ws.join("target/debug/deps");
        let deps = deps.to_str().unwrap();
        let mut args: Vec<String> = vec![
            self.rustc.to_str().unwrap().into(),
            "--crate-name".into(),
            crate_name.into(),
            "--edition=2021".into(),
            format!("{crate_name}/src/lib.rs"),
            "--error-format=json".into(),
            "--json=diagnostic-rendered-ansi,artifacts,future-incompat".into(),
            "--crate-type".into(),
            "lib".into(),
            "--emit=dep-info,metadata,link".into(),
            "-C".into(),
            "embed-bitcode=no".into(),
            "-C".into(),
            format!("metadata=m{crate_name}"),
            "-C".into(),
            format!("extra-filename=-x{crate_name}"),
            "--out-dir".into(),
            deps.into(),
            "-L".into(),
            format!("dependency={deps}"),
        ];
        args.extend(more.iter().map(|s| s.to_string()));
        args.into_iter().map(OsString::from).collect()
    }

    fn vars(&self, ws: &Path, crate_name: &str) -> BTreeMap<String, String> {
        let current = self.session.current();
        BTreeMap::from([
            (
                session::SESSION.to_string(),
                current.folder.to_str().unwrap().to_string(),
            ),
            ("CARGO_PKG_NAME".into(), crate_name.into()),
            ("CARGO_PKG_VERSION".into(), "0.1.0".into()),
            ("CARGO_CRATE_NAME".into(), crate_name.into()),
            (
                "CARGO_MANIFEST_DIR".into(),
                ws.join(crate_name).to_str().unwrap().into(),
            ),
        ])
    }

    fn call(&self, ws: &Path, crate_name: &str, more: &[&str]) -> Result {
        self.call_with(ws, crate_name, more, |_| {})
    }

    fn call_with(
        &self,
        ws: &Path,
        crate_name: &str,
        more: &[&str],
        change: impl FnOnce(&mut Fake),
    ) -> Result {
        let seen = Rc::new(RefCell::new(Seen::default()));
        let mut fake = Fake {
            vars: self.vars(ws, crate_name),
            cwd: ws.to_path_buf(),
            plans: self.plans.clone(),
            during: None,
            seen: seen.clone(),
            version_fails: false,
            audit: false,
            on_found: RefCell::new(None),
        };
        change(&mut fake);
        let before = self.session.current().entries().len();
        let mut stderr = Vec::new();
        let code = call(&mut fake, &mut stderr, &self.args(ws, crate_name, more));
        drop(fake);
        let entries = self.session.current().entries();
        assert!(entries.len() <= before + 1, "one entry per call");
        Result {
            code,
            entry: entries.get(before).cloned(),
            stderr: String::from_utf8(stderr).unwrap(),
            seen: Rc::try_unwrap(seen).ok().unwrap().into_inner(),
        }
    }

    fn rlib(&self, ws: &Path, crate_name: &str) -> PathBuf {
        ws.join(format!(
            "target/debug/deps/lib{crate_name}-x{crate_name}.rlib"
        ))
    }

    fn blocks(&self) -> u64 {
        self.store().status(SystemTime::now()).blocks
    }
}

const STORED: Outcome = Outcome::BuiltAndStored;
const REUSED: Outcome = Outcome::Reused;

fn not_stored(why: NotStored) -> Outcome {
    Outcome::BuiltNotStored(why)
}

// covers: B-2
#[test]
fn without_a_session_rustc_runs_as_asked_and_nothing_is_logged() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let r = w.call_with(&ws, "a", &[], |fake| {
        fake.vars.remove(session::SESSION);
    });
    assert_eq!(r.code, 0);
    assert!(r.entry.is_none());
    assert_eq!(r.seen.execs.len(), 1);
    assert!(r.seen.compiles.is_empty());
    assert_eq!(w.blocks(), 0);
}

// covers: B-4
#[test]
fn probes_pass_through_before_anything_and_are_not_logged() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let seen = Rc::new(RefCell::new(Seen::default()));
    let mut fake = Fake {
        vars: w.vars(&ws, "a"),
        cwd: ws.clone(),
        plans: w.plans.clone(),
        during: None,
        seen: seen.clone(),
        version_fails: false,
        audit: false,
        on_found: RefCell::new(None),
    };
    // The session's file is gone: a probe must not need it.
    fs::remove_file(w.session.current().folder.join("session.json")).unwrap();
    let rustc = OsString::from(w.rustc.to_str().unwrap());
    for probe in [
        vec![rustc.clone(), "-vV".into()],
        vec![
            rustc.clone(),
            "-".into(),
            "--crate-name".into(),
            "___".into(),
            "--print=file-names".into(),
        ],
    ] {
        call(&mut fake, &mut Vec::new(), &probe);
    }
    assert_eq!(seen.borrow().execs.len(), 2);
    assert!(w.session.current().entries().is_empty());
}

// covers: B-4, B-19
#[test]
fn calls_the_wrapper_cant_key_pass_through_and_are_logged() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let r = w.call(&ws, "a", &["--remap-path-prefix=/x=/y"]);
    assert_eq!(
        r.outcome(),
        Outcome::PassedThrough(PassedThrough::CallerRemap)
    );
    assert_eq!(r.entry.unwrap().crate_name, "a");
    assert_eq!(r.seen.execs.len(), 1);

    let r = w.call_with(&ws, "a", &[], |fake| fake.version_fails = true);
    assert_eq!(r.outcome(), Outcome::PassedThrough(PassedThrough::Compiler));
    assert_eq!(r.seen.execs.len(), 1);
    assert_eq!(w.blocks(), 0);
}

// covers: B-4, B-21
#[test]
fn a_package_never_stored_runs_as_asked() {
    let w = world_with(Mode::On, |s| s.never_store = vec!["a".into()]);
    let ws = w.checkout("ws");
    let r = w.call(&ws, "a", &[]);
    assert_eq!(
        r.outcome(),
        Outcome::PassedThrough(PassedThrough::NeverStore)
    );
    assert_eq!(w.blocks(), 0);
}

// covers: B-1, B-8, B-9, B-12, B-13
#[test]
fn a_block_built_in_one_checkout_is_reused_in_another() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            reads: vec!["a/src/foo.rs".into(), "a/data/text.txt".into()],
            env_reads: vec!["CARGO_PKG_VERSION".into()],
            ..Plan::default()
        },
    );
    let one = w.checkout("one");
    let two = w.checkout("second-checkout-longer");
    settle();
    let first = w.call(&one, "a", &[]);
    assert_eq!(first.outcome(), STORED);
    assert_eq!(first.code, 0);
    // The compile was remapped: the workspace became `.`.
    let args = &first.seen.compiles[0];
    assert!(args.contains(&format!("--remap-path-prefix={}=.", one.display())));
    assert!(
        fs::read_to_string(w.rlib(&one, "a"))
            .unwrap()
            .contains("source ./a/src/lib.rs")
    );
    assert_eq!(w.blocks(), 1);

    let second = w.call(&two, "a", &[]);
    assert_eq!(second.outcome(), REUSED);
    assert_eq!(second.code, 0);
    assert!(second.seen.compiles.is_empty() && second.seen.execs.is_empty());
    assert_eq!(
        fs::read(w.rlib(&one, "a")).unwrap(),
        fs::read(w.rlib(&two, "a")).unwrap()
    );
    // The notices name this checkout's files, last; the dep-info names this checkout.
    let deps = two.join("target/debug/deps");
    assert!(
        second
            .stderr
            .contains(&format!("{}/liba-xa.rmeta", deps.display()))
    );
    assert!(!second.stderr.contains(one.to_str().unwrap()));
    assert!(!second.stderr.contains("{hushrun:"));
    let dep_info = fs::read_to_string(deps.join("a-xa.d")).unwrap();
    assert!(dep_info.contains(&format!("{}/liba-xa.rlib", deps.display())));
    assert!(!dep_info.contains(one.to_str().unwrap()));
    // A reused file is a new, writable file dated the wrapper's start.
    let meta = fs::metadata(w.rlib(&two, "a")).unwrap();
    assert!(!meta.permissions().readonly());
    let entry = second.entry.unwrap();
    assert!(entry.found);
    // The record beside it says a reuse put it there.
    let known = crate::sidecar::lookup(&w.rlib(&two, "a")).unwrap();
    assert!(known.reused && known.step.is_some());
}

// covers: B-11
#[test]
fn a_read_swapped_during_the_compile_with_its_old_date_is_not_stored() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            reads: vec!["a/data/text.txt".into()],
            ..Plan::default()
        },
    );
    let ws = w.checkout("ws");
    let data = ws.join("a/data/text.txt");
    let old = fs::metadata(&data).unwrap().modified().unwrap();
    settle();
    let swap = data.clone();
    let r = w.call_with(&ws, "a", &[], move |fake| {
        fake.during = Some(Box::new(move || {
            let temp = swap.with_extension("new");
            fs::write(&temp, "changed\n").unwrap();
            fs::File::options()
                .write(true)
                .open(&temp)
                .unwrap()
                .set_modified(old)
                .unwrap();
            fs::rename(&temp, &swap).unwrap();
        }));
    });
    assert_eq!(fs::metadata(&data).unwrap().modified().unwrap(), old);
    assert_eq!(r.outcome(), not_stored(NotStored::Changed));
    assert_eq!(w.blocks(), 0);
}

/// Builds `a` in `one`, then changes `two` with `probe`, and says what the call in `two` did.
fn probe(plan: Plan, change: impl FnOnce(&Path), vars: impl FnOnce(&mut Fake)) -> Result {
    let w = world(Mode::On);
    w.plan("a", plan);
    let one = w.checkout("one");
    let two = w.checkout("two-longer");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    change(&two);
    settle();
    w.call_with(&two, "a", &[], vars)
}

// covers: B-6
#[test]
fn a_module_candidate_appearing_changes_the_key() {
    let plan = Plan {
        reads: vec!["a/src/foo.rs".into()],
        ..Plan::default()
    };
    let same = probe(plan.clone(), |_| {}, |_| {});
    assert_eq!(same.outcome(), REUSED);
    let r = probe(
        plan,
        |two| {
            fs::create_dir_all(two.join("a/src/foo")).unwrap();
            fs::write(two.join("a/src/foo/mod.rs"), "").unwrap();
        },
        |_| {},
    );
    assert_eq!(r.outcome(), STORED);
    assert!(!r.entry.unwrap().found);
}

// covers: B-6
#[test]
fn a_file_added_beside_an_included_one_changes_the_key() {
    let plan = Plan {
        reads: vec!["a/data/text.txt".into()],
        ..Plan::default()
    };
    let r = probe(
        plan,
        |two| fs::write(two.join("a/data/new.sql"), "").unwrap(),
        |_| {},
    );
    assert_eq!(r.outcome(), STORED);
}

// covers: B-6
#[test]
fn an_env_read_going_from_set_to_unset_changes_the_key() {
    let plan = Plan {
        env_reads: vec!["A_VAR".into()],
        ..Plan::default()
    };
    let w = world(Mode::On);
    w.plan("a", plan);
    let ws = w.checkout("ws");
    settle();
    let set = |fake: &mut Fake| {
        fake.vars.insert("A_VAR".into(), "1".into());
    };
    assert_eq!(w.call_with(&ws, "a", &[], set).outcome(), STORED);
    let other = w.checkout("ws2");
    assert_eq!(w.call_with(&other, "a", &[], set).outcome(), REUSED);
    let third = w.checkout("ws3");
    let unset = w.call(&third, "a", &[]);
    assert_eq!(unset.outcome(), STORED);
    assert!(!unset.entry.unwrap().found);
}

// covers: B-7, B-8
#[test]
fn a_hint_is_followed_under_this_checkout_and_never_under_the_one_that_wrote_it() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            reads: vec!["a/data/text.txt".into()],
            ..Plan::default()
        },
    );
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    // The first checkout's files can't be opened at all now: a read there would give an
    // absent file and another key, or fail.
    fs::remove_dir_all(&one).unwrap();
    let r = w.call(&two, "a", &[]);
    assert_eq!(r.outcome(), REUSED);
    // And the hint holds no checkout's path.
    let current = w.session.current();
    let hints_dir = current.store.join("steps");
    for entry in walk(&hints_dir) {
        let text = fs::read_to_string(&entry).unwrap_or_default();
        assert!(!text.contains(one.to_str().unwrap()), "{text}");
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

// covers: B-12
#[test]
fn a_width_change_reuses_and_a_missing_message_form_compiles() {
    // Only notices: the block serves every form.
    let w = world(Mode::On);
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    let r = w.call(&two, "a", &["--color=always", "--diagnostic-width=80"]);
    assert_eq!(r.outcome(), REUSED);

    // A warning: the width doesn't count, the colour does.
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            warn: true,
            ..Plan::default()
        },
    );
    let one = w.checkout("one");
    let two = w.checkout("two");
    let three = w.checkout("three");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    let r = w.call(&two, "a", &["--diagnostic-width=80"]);
    assert_eq!(r.outcome(), REUSED);
    assert!(r.stderr.contains("unused variable"));
    let r = w.call(&three, "a", &["--color=always"]);
    assert_eq!(r.outcome(), STORED);
    assert!(r.entry.unwrap().found);
    assert_eq!(r.seen.compiles.len(), 1);
}

// covers: B-3, B-19
#[test]
fn a_failed_compile_stores_nothing_and_keeps_rustcs_exit_code() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            fail: Some("E0425"),
            ..Plan::default()
        },
    );
    let ws = w.checkout("ws");
    let r = w.call(&ws, "a", &[]);
    assert_eq!(r.code, 1);
    assert_eq!(r.outcome(), not_stored(NotStored::RustcFailed));
    assert!(r.stderr.contains("E0425"));
    assert_eq!(w.blocks(), 0);
}

// covers: B-12, B-19
#[test]
fn a_kill_between_the_copy_and_the_rename_leaves_nothing_taken_for_a_block() {
    let w = world(Mode::On);
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    // Find the block and copy it beside two's outputs, then stop before the renames.
    let store = w.store();
    let step_dir = walk(&w.session.current().store.join("blocks"));
    let json = step_dir
        .iter()
        .find(|p| p.ends_with("block.json"))
        .unwrap()
        .clone();
    let key = Digest::parse(
        json.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap_or_else(|_| {
        let hex = json
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        Digest::parse(&format!("sha256:{hex}")).unwrap()
    });
    let block = store.block(&key).unwrap().unwrap();
    let description = crate::description::Description::from_value(&block.description).unwrap();
    let env = crate::invocation::Env::from_vars(w.vars(&two, "a"));
    let crate::invocation::Parsed::Handled(inv) =
        crate::invocation::parse(&w.args(&two, "a", &[]), &env, &two)
    else {
        panic!("not handled");
    };
    let roots = crate::roots::Roots::new(
        &crate::roots::RootPaths::of(&inv),
        &[],
        crate::roots::resolve_on_disk,
    )
    .unwrap();
    let prepared = crate::restore::Restore {
        block: &block,
        description: &description,
        invocation: &inv,
        roots: &roots,
        start: SystemTime::now(),
        pid: 7,
    }
    .prepare()
    .unwrap();
    let temps = prepared.temporary_files();
    // A kill now: nothing runs after this point.
    std::mem::forget(prepared);
    let deps = two.join("target/debug/deps");
    for temp in &temps {
        let name = temp.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(".hushrun"), "{name}");
        assert!(!name.starts_with("lib"));
    }
    assert!(!w.rlib(&two, "a").exists());
    assert!(!deps.join("a-xa.d").exists());
    assert!(crate::sidecar::reused_in(&deps).unwrap().is_empty());
    // Cargo asks again: the block is reused and the leftovers change nothing.
    let r = w.call(&two, "a", &[]);
    assert_eq!(r.outcome(), REUSED);
    assert_eq!(
        fs::read(w.rlib(&one, "a")).unwrap(),
        fs::read(w.rlib(&two, "a")).unwrap()
    );
}

// covers: B-9
#[test]
fn a_first_build_drops_incremental_and_a_rebuild_keeps_it() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    settle();
    let incremental = format!(
        "incremental={}",
        ws.join("target/debug/incremental").display()
    );
    let first = w.call(&ws, "a", &["-C", &incremental]);
    assert_eq!(first.outcome(), STORED);
    assert!(
        !first.seen.compiles[0]
            .iter()
            .any(|a| a.starts_with("incremental="))
    );
    // An edit: no block, a dep-info file already there.
    fs::write(ws.join("a/src/lib.rs"), "mod foo;\nfn edited() {}\n").unwrap();
    settle();
    let rebuild = w.call(&ws, "a", &["-C", &incremental]);
    assert_eq!(
        rebuild.outcome(),
        Outcome::PassedThrough(PassedThrough::Incremental)
    );
    assert_eq!(rebuild.seen.execs.len(), 1);
    assert!(rebuild.seen.execs[0].contains(&incremental));
    assert!(
        !rebuild.seen.execs[0]
            .iter()
            .any(|a| a.starts_with("--remap"))
    );
    // Back to the stored version: reused even with incremental asked.
    fs::write(ws.join("a/src/lib.rs"), "mod foo;\n").unwrap();
    settle();
    let back = w.call(&ws, "a", &["-C", &incremental]);
    assert_eq!(back.outcome(), REUSED);
}

fn only_step(w: &World) -> Digest {
    let steps = walk(&w.session.current().store.join("steps"));
    let hint = steps
        .iter()
        .find(|p| p.parent().unwrap().ends_with("hints"))
        .unwrap();
    let step_dir = hint.parent().unwrap().parent().unwrap();
    let hex = step_dir.file_name().unwrap().to_str().unwrap();
    Digest::parse(hex)
        .or_else(|_| Digest::parse(&format!("sha256:{hex}")))
        .unwrap()
}

// covers: B-15
#[test]
fn a_suspended_key_is_never_reused() {
    let w = world(Mode::On);
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    w.store()
        .suspend(&only_step(&w), "test", SystemTime::now())
        .unwrap();
    let r = w.call(&two, "a", &[]);
    assert_eq!(r.outcome(), not_stored(NotStored::Suspended));
    assert_eq!(r.seen.compiles.len(), 1);
}

// covers: B-13, B-19
#[test]
fn a_truncated_block_compiles() {
    let w = world(Mode::On);
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    let files = walk(&w.session.current().store.join("blocks"));
    let rlib = files
        .iter()
        .find(|p| p.extension().is_some_and(|e| e == "rlib"))
        .unwrap();
    let mut perms = fs::metadata(rlib).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(rlib, perms).unwrap();
    fs::write(rlib, b"short").unwrap();
    let r = w.call(&two, "a", &[]);
    assert_eq!(r.code, 0);
    assert_eq!(r.outcome(), STORED);
    assert!(
        fs::read_to_string(w.rlib(&two, "a"))
            .unwrap()
            .starts_with("rlib\n")
    );
}

// covers: B-12, B-19
#[test]
fn a_block_that_cant_be_put_in_place_runs_rustc_as_asked() {
    let w = world(Mode::On);
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    // Its messages name a root no call here has.
    let json = walk(&w.session.current().store.join("blocks"))
        .into_iter()
        .find(|p| p.ends_with("block.json"))
        .unwrap();
    let mut perms = fs::metadata(&json).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&json, perms).unwrap();
    let text = fs::read_to_string(&json).unwrap();
    fs::write(&json, text.replace("{hushrun:out-dir}", "{hushrun:deps-9}")).unwrap();
    let r = w.call(&two, "a", &[]);
    assert_eq!(r.outcome(), not_stored(NotStored::Restore));
    assert_eq!(r.seen.execs.len(), 1);
    assert!(!r.stderr.contains("hushrun:"));
}

// covers: B-15
#[test]
fn a_crate_loading_error_suspends_and_removes_the_reused_files_and_no_other_error_does() {
    for (code, rejects) in [("E0463", true), ("E0425", false)] {
        let w = world(Mode::On);
        let one = w.checkout("one");
        let two = w.checkout("two");
        settle();
        assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
        assert_eq!(w.call(&two, "a", &[]).outcome(), REUSED);
        let step = only_step(&w);
        w.plan(
            "b",
            Plan {
                fail: Some(code),
                fail_message: Some("can't find crate for `core` which `a` depends on".into()),
                ..Plan::default()
            },
        );
        fs::create_dir_all(two.join("b/src")).unwrap();
        fs::write(two.join("b/src/lib.rs"), "").unwrap();
        let rmeta = two.join("target/debug/deps/liba-xa.rmeta");
        let ext = format!("a={}", rmeta.display());
        let r = w.call(&two, "b", &["--extern", &ext]);
        assert_eq!(r.code, 1);
        assert_eq!(r.outcome(), not_stored(NotStored::RustcFailed));
        assert_eq!(w.store().suspended(&step).is_some(), rejects, "{code}");
        assert_eq!(!rmeta.exists(), rejects, "{code}");
        assert_eq!(!w.rlib(&two, "a").exists(), rejects, "{code}");
        assert_eq!(r.entry.unwrap().note.is_some(), rejects, "{code}");
    }
}

// covers: B-1, B-16
#[test]
fn observe_never_reuses_and_compares() {
    let w = world(Mode::Observe);
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    let r = w.call(&two, "a", &[]);
    assert_eq!(r.outcome(), STORED);
    assert_eq!(r.seen.compiles.len(), 1);
    let entry = r.entry.unwrap();
    assert!(entry.found && !entry.differed);
}

// covers: B-16
#[test]
fn a_block_that_differs_from_a_fresh_compile_suspends_its_key() {
    let w = world(Mode::Observe);
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    // The compiler now writes other bytes for the same inputs.
    w.plan(
        "a",
        Plan {
            env_reads: vec!["NOT_IN_THE_KEY".into()],
            ..Plan::default()
        },
    );
    // Its dep-info names a variable the hint doesn't, so look up with the first hint.
    let r = w.call(&two, "a", &[]);
    let entry = r.entry.unwrap();
    assert!(entry.found);
    assert!(entry.differed);
    assert_eq!(entry.outcome, not_stored(NotStored::Differed));
    assert!(w.store().suspended(&only_step(&w)).is_some());
}

// covers: B-22
#[test]
fn low_free_space_stores_nothing() {
    let w = world_with(Mode::On, |s| s.min_free_gb = u32::MAX);
    let ws = w.checkout("ws");
    settle();
    let r = w.call(&ws, "a", &[]);
    assert_eq!(r.outcome(), not_stored(NotStored::LowSpace));
    assert!(r.entry.unwrap().note.is_none());
    assert_eq!(w.blocks(), 0);
}

// covers: B-10
#[test]
fn an_output_holding_a_root_is_tied_to_its_checkout() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            embed_var: Some("CARGO_MANIFEST_DIR".into()),
            ..Plan::default()
        },
    );
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    let r = w.call(&two, "a", &[]);
    assert_eq!(r.outcome(), STORED);
    assert!(!r.entry.unwrap().found);
    // In the same checkout, with its outputs gone, it is reused.
    fs::remove_file(w.rlib(&one, "a")).unwrap();
    let again = w.call(&one, "a", &[]);
    assert_eq!(again.outcome(), REUSED);
}

/// A checkout whose build script left `gen.rs` in `OUT_DIR`, and cargo's saved output beside it.
fn with_script(ws: &Path) -> (String, String) {
    let run = ws.join("target/debug/build/a-1");
    fs::create_dir_all(run.join("out")).unwrap();
    fs::write(run.join("out/gen.rs"), "pub const X: u8 = 1;\n").unwrap();
    fs::write(run.join("output"), "cargo:rerun-if-changed=build.rs\n").unwrap();
    fs::write(ws.join("a/build.rs"), "fn main() {}\n").unwrap();
    ("OUT_DIR".into(), run.join("out").to_str().unwrap().into())
}

// covers: B-10
#[test]
fn a_root_read_through_a_variable_moves_only_when_a_second_compile_proves_it() {
    for (hash_var, moves) in [(false, true), (true, false)] {
        let w = world(Mode::On);
        w.plan(
            "a",
            Plan {
                include_var: Some(("OUT_DIR".into(), "gen.rs".into())),
                hash_var,
                ..Plan::default()
            },
        );
        let one = w.checkout("one");
        let two = w.checkout("two-longer");
        let out_one = with_script(&one);
        let out_two = with_script(&two);
        settle();
        let first = w.call_with(&one, "a", &[], |f| {
            f.vars.insert(out_one.0.clone(), out_one.1.clone());
        });
        assert_eq!(first.outcome(), STORED);
        // The proof ran once, into a scratch folder that is gone again.
        assert_eq!(first.seen.compiles.len(), 2, "{hash_var}");
        assert!(
            !one.join("target/debug/deps/.hushrun")
                .read_dir()
                .unwrap()
                .any(|e| {
                    e.unwrap()
                        .file_name()
                        .to_str()
                        .unwrap()
                        .starts_with("proof")
                })
        );
        let second = w.call_with(&two, "a", &[], |f| {
            f.vars.insert(out_two.0.clone(), out_two.1.clone());
        });
        assert_eq!(second.outcome() == REUSED, moves, "{hash_var}");
    }
}

// covers: B-5, B-6
#[test]
fn a_build_scripts_output_and_declared_inputs_count() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            include_var: Some(("OUT_DIR".into(), "gen.rs".into())),
            ..Plan::default()
        },
    );
    let one = w.checkout("one");
    let two = w.checkout("two");
    let three = w.checkout("three");
    let (name, out_one) = with_script(&one);
    let (_, out_two) = with_script(&two);
    let (_, out_three) = with_script(&three);
    // Three changes its build script's input; two changes nothing.
    fs::write(three.join("a/build.rs"), "fn main() { /* edited */ }\n").unwrap();
    settle();
    let set = |out: String| {
        let name = name.clone();
        move |f: &mut Fake| {
            f.vars.insert(name, out);
        }
    };
    assert_eq!(w.call_with(&one, "a", &[], set(out_one)).outcome(), STORED);
    assert_eq!(w.call_with(&two, "a", &[], set(out_two)).outcome(), REUSED);
    let r = w.call_with(&three, "a", &[], set(out_three));
    assert_eq!(r.outcome(), STORED);
    assert!(!r.entry.unwrap().found);
}

// covers: B-4, B-5
#[test]
fn a_build_script_whose_output_cant_be_read_is_built_and_not_stored() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let out = ws.join("target/debug/build/a-1/out");
    fs::create_dir_all(&out).unwrap();
    settle();
    let r = w.call_with(&ws, "a", &[], |f| {
        f.vars
            .insert("OUT_DIR".into(), out.to_str().unwrap().into());
    });
    assert_eq!(r.outcome(), not_stored(NotStored::ScriptOutputMissing));
    assert_eq!(w.blocks(), 0);
}

// covers: B-16
#[test]
fn a_compile_the_audit_picks_runs_and_is_compared_in_on() {
    let w = world(Mode::On);
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    let r = w.call_with(&two, "a", &[], |f| f.audit = true);
    assert_eq!(r.outcome(), STORED);
    assert_eq!(r.seen.compiles.len(), 1);
    let entry = r.entry.unwrap();
    assert!(entry.found && !entry.differed);
}

// covers: B-18
#[test]
fn a_linked_output_whose_link_inputs_cant_be_read_is_built_and_not_stored() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    settle();
    let mut args = w.args(&ws, "a", &[]);
    let at = args.iter().position(|a| a == "lib").unwrap();
    args[at] = "proc-macro".into();
    let seen = Rc::new(RefCell::new(Seen::default()));
    let mut fake = Fake {
        vars: w.vars(&ws, "a"),
        cwd: ws.clone(),
        plans: w.plans.clone(),
        during: None,
        seen: seen.clone(),
        version_fails: false,
        audit: false,
        on_found: RefCell::new(None),
    };
    let code = call(&mut fake, &mut Vec::new(), &args);
    assert_eq!(code, 0);
    let entries = w.session.current().entries();
    assert_eq!(
        entries.last().unwrap().outcome,
        not_stored(NotStored::LinkInputs)
    );
    // The compile was asked for its link line.
    assert!(
        seen.borrow().compiles[0]
            .iter()
            .any(|a| a == "--print=link-args")
    );
    assert_eq!(w.blocks(), 0);
}

// covers: B-6, B-18
#[test]
fn files_the_linker_finds_through_its_search_folders_are_keyed() {
    let w = world(Mode::On);
    // The driver and the linker are the repository's own: under the workspace, so the block
    // may move (a linker outside every root ties it, B-8).
    let line = r#""{ws}/tools/cc" "{ws}/target/debug/deps/x.o" "-L" "{ws}/vendor" "-lfoo" "-Tscript.ld" "-fuse-ld={ws}/tools/ld.fake" "-o" "{ws}/target/debug/deps/liba-xa.dylib""#;
    let setup = |name: &str| {
        let ws = w.checkout(name);
        fs::create_dir_all(ws.join("tools")).unwrap();
        fs::write(ws.join("tools/cc"), b"driver").unwrap();
        fs::write(ws.join("tools/ld.fake"), b"linker one").unwrap();
        fs::create_dir_all(ws.join("vendor")).unwrap();
        fs::write(ws.join("vendor/libfoo.a"), b"foo one").unwrap();
        fs::write(ws.join("script.ld"), b"SECTIONS { }").unwrap();
        ws
    };
    let as_proc_macro = |w: &World, ws: &Path| {
        let mut args = w.args(ws, "a", &[]);
        let at = args.iter().position(|a| a == "lib").unwrap();
        args[at] = "proc-macro".into();
        args
    };
    let run = |ws: &Path| -> Outcome {
        w.plan(
            "a",
            Plan {
                stdout: Some(format!("{line}\n")),
                ..Plan::default()
            },
        );
        settle();
        let seen = Rc::new(RefCell::new(Seen::default()));
        let mut fake = Fake {
            vars: w.vars(ws, "a"),
            cwd: ws.to_path_buf(),
            plans: w.plans.clone(),
            during: None,
            seen,
            version_fails: false,
            audit: false,
            on_found: RefCell::new(None),
        };
        call(&mut fake, &mut Vec::new(), &as_proc_macro(&w, ws));
        w.session.current().entries().last().unwrap().outcome
    };
    let one = setup("one");
    assert_eq!(run(&one), STORED);
    let two = setup("two-longer");
    assert_eq!(run(&two), REUSED, "the same inputs elsewhere");

    let lib = setup("lib");
    fs::write(lib.join("vendor/libfoo.a"), b"foo two").unwrap();
    assert_eq!(run(&lib), STORED, "a library found through -L changed");

    let earlier = setup("earlier");
    fs::write(
        earlier.join("vendor/libfoo.dylib"),
        b"a dylib the linker prefers",
    )
    .unwrap();
    assert_eq!(
        run(&earlier),
        STORED,
        "a library appeared that the linker picks first"
    );

    let script = setup("script");
    fs::write(script.join("script.ld"), b"SECTIONS { .x : { } }").unwrap();
    assert_eq!(run(&script), STORED, "a linker script changed");

    let linker = setup("linker");
    fs::write(linker.join("tools/ld.fake"), b"linker two").unwrap();
    assert_eq!(run(&linker), STORED, "the linker -fuse-ld names changed");
}

// covers: B-4
#[test]
fn a_target_that_is_not_one_of_the_compilers_own_passes_through() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let r = w.call(&ws, "a", &["--target", "mytgt"]);
    assert_eq!(
        r.outcome(),
        Outcome::PassedThrough(PassedThrough::TargetFile)
    );
    assert_eq!(r.seen.execs.len(), 1);
    settle();
    let r = w.call(&ws, "a", &["--target", "aarch64-apple-ios"]);
    assert_eq!(r.outcome(), STORED);
}

// covers: B-18
#[test]
fn what_a_linked_compile_prints_goes_on_but_the_linker_command() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let tools = w.dir.path().join("tools");
    fs::create_dir_all(&tools).unwrap();
    fs::write(tools.join("cc"), b"driver").unwrap();
    w.plan(
        "a",
        Plan {
            stdout: Some(format!(
                "printed by a proc macro\n\"{}\" \"-o\" \"{{ws}}/x\"\nand after\n",
                tools.join("cc").display()
            )),
            ..Plan::default()
        },
    );
    settle();
    let mut args = w.args(&ws, "a", &[]);
    let at = args.iter().position(|a| a == "lib").unwrap();
    args[at] = "proc-macro".into();
    let seen = Rc::new(RefCell::new(Seen::default()));
    let mut fake = Fake {
        vars: w.vars(&ws, "a"),
        cwd: ws.clone(),
        plans: w.plans.clone(),
        during: None,
        seen: seen.clone(),
        version_fails: false,
        audit: false,
        on_found: RefCell::new(None),
    };
    assert_eq!(call(&mut fake, &mut Vec::new(), &args), 0);
    assert_eq!(
        String::from_utf8(seen.borrow().stdout.clone()).unwrap(),
        "printed by a proc macro\nand after\n"
    );
    assert_eq!(
        w.session.current().entries().last().unwrap().outcome,
        STORED
    );
}

// covers: B-20
#[test]
fn each_entry_names_its_package() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let r = w.call(&ws, "a", &[]);
    assert_eq!(r.entry.unwrap().package, "a-0.1.0");
    // Refused by the parser: still named.
    let r = w.call(&ws, "a", &["--remap-path-prefix=/x=/y"]);
    assert_eq!(r.entry.unwrap().package, "a-0.1.0");
    let r = w.call_with(&ws, "a", &[], |fake| {
        fake.vars.remove("CARGO_PKG_VERSION");
    });
    assert_eq!(r.entry.unwrap().package, "a");
}

/// Makes a named pipe at `path`: opening it for reading blocks until a writer comes.
#[cfg(unix)]
fn fifo(path: &Path) {
    let _ = fs::remove_file(path);
    let made = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success());
}

/// Opens each pipe for writing once `secs` have passed without [`Watchdog::finish`]: a reader
/// blocked on one goes on, and the test says it hung instead of hanging.
#[cfg(unix)]
struct Watchdog {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    hung: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(unix)]
impl Watchdog {
    fn new(pipes: Vec<PathBuf>, secs: u64) -> Watchdog {
        use std::os::unix::fs::OpenOptionsExt;
        use std::sync::atomic::Ordering;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hung = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (d, h) = (done.clone(), hung.clone());
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            while !d.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
                if start.elapsed() < Duration::from_secs(secs) {
                    continue;
                }
                h.store(true, Ordering::SeqCst);
                // Non-blocking: succeeds only while a reader waits on the pipe.
                for pipe in &pipes {
                    let flags = rustix::fs::OFlags::NONBLOCK.bits();
                    let _ = fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(i32::try_from(flags).unwrap())
                        .open(pipe);
                }
            }
        });
        Watchdog { done, hung }
    }

    /// Stops watching; whether a reader had to be let go.
    fn finish(self) -> bool {
        use std::sync::atomic::Ordering;
        self.done.store(true, Ordering::SeqCst);
        self.hung.load(Ordering::SeqCst)
    }
}

// covers: B-19
#[cfg(unix)]
#[test]
fn a_named_pipe_in_a_package_whose_script_declares_nothing_is_never_opened() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let run = ws.join("target/debug/build/a-1");
    fs::create_dir_all(run.join("out")).unwrap();
    // The build script declared nothing: the whole package is its input (cargo's rule).
    fs::write(run.join("output"), "").unwrap();
    let pipe = ws.join("a/pipe");
    fifo(&pipe);
    settle();
    let out = run.join("out");
    let dog = Watchdog::new(vec![pipe], 5);
    let r = w.call_with(&ws, "a", &[], |f| {
        f.vars
            .insert("OUT_DIR".into(), out.to_str().unwrap().into());
    });
    assert!(!dog.finish(), "the wrapper opened a named pipe");
    assert_eq!(r.code, 0);
    assert_eq!(r.outcome(), not_stored(NotStored::NotAFile));
    assert_eq!(w.blocks(), 0);
}

// covers: B-19
#[cfg(unix)]
#[test]
fn a_named_pipe_under_out_dir_or_where_a_native_library_may_be_is_never_opened() {
    // In the build script's output.
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let out = with_script(&ws).1;
    fifo(&Path::new(&out).join("pipe"));
    settle();
    let dog = Watchdog::new(vec![Path::new(&out).join("pipe")], 5);
    let r = w.call_with(&ws, "a", &[], |f| {
        f.vars.insert("OUT_DIR".into(), out.clone());
    });
    assert!(!dog.finish(), "the wrapper opened a named pipe");
    assert_eq!(r.code, 0);
    assert_eq!(r.outcome(), not_stored(NotStored::NotAFile));

    // Where `-l static=foo` may be found.
    let ws = w.checkout("ws2");
    fs::create_dir_all(ws.join("vendor")).unwrap();
    let pipe = ws.join("vendor/libfoo.a");
    fifo(&pipe);
    settle();
    let native = format!("native={}", ws.join("vendor").display());
    let dog = Watchdog::new(vec![pipe], 5);
    let r = w.call(&ws, "a", &["-L", &native, "-l", "static=foo"]);
    assert!(!dog.finish(), "the wrapper opened a named pipe");
    assert_eq!(r.code, 0);
    assert_eq!(r.outcome(), not_stored(NotStored::NotAFile));
    assert_eq!(w.blocks(), 0);
}

// covers: B-7, B-19
#[cfg(unix)]
#[test]
fn a_named_pipe_at_a_hinted_place_is_never_opened() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            reads: vec!["a/src/foo.rs".into()],
            ..Plan::default()
        },
    );
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    // The hint names `a/src/foo/mod.rs` as absent; in two it is a named pipe.
    fs::create_dir_all(two.join("a/src/foo")).unwrap();
    let pipe = two.join("a/src/foo/mod.rs");
    fifo(&pipe);
    settle();
    let dog = Watchdog::new(vec![pipe], 5);
    let r = w.call(&two, "a", &[]);
    assert!(!dog.finish(), "the wrapper opened a named pipe");
    assert_eq!(r.code, 0);
    assert_eq!(r.outcome(), not_stored(NotStored::NotAFile));
}

// covers: B-6, B-8
#[cfg(unix)]
#[test]
fn a_read_through_dots_whose_last_part_is_a_link_is_keyed_by_what_the_path_names_now() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            // As rustc writes `include_str!("../data/text.txt")` from `a/src/lib.rs`.
            reads: vec!["a/src/../data/text.txt".into()],
            ..Plan::default()
        },
    );
    let link_to = |ws: &Path, target: &str| {
        fs::create_dir_all(ws.join("shared")).unwrap();
        fs::write(ws.join("shared/A.txt"), "AAA\n").unwrap();
        fs::write(ws.join("shared/B.txt"), "BBB\n").unwrap();
        fs::remove_file(ws.join("a/data/text.txt")).unwrap();
        std::os::unix::fs::symlink(format!("../../shared/{target}"), ws.join("a/data/text.txt"))
            .unwrap();
    };
    let one = w.checkout("one");
    let two = w.checkout("two-longer");
    link_to(&one, "A.txt");
    link_to(&two, "B.txt");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    let r = w.call(&two, "a", &[]);
    assert_eq!(r.outcome(), STORED, "a wrong reuse");
    assert!(
        fs::read_to_string(w.rlib(&two, "a"))
            .unwrap()
            .contains("BBB")
    );
    // The same link target elsewhere is reused.
    let three = w.checkout("three");
    link_to(&three, "A.txt");
    settle();
    assert_eq!(w.call(&three, "a", &[]).outcome(), REUSED);
}

// covers: B-8
#[cfg(unix)]
#[test]
fn a_read_through_dots_after_a_linked_folder_is_not_stored() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            reads: vec!["a/src/../data/text.txt".into()],
            ..Plan::default()
        },
    );
    let ws = w.checkout("ws");
    // `a/src` is a link to another package's folder: `a/src/..` is not `a`.
    fs::create_dir_all(ws.join("lib/data")).unwrap();
    fs::write(ws.join("lib/data/text.txt"), "lib\n").unwrap();
    fs::rename(ws.join("a/src"), ws.join("lib/src")).unwrap();
    std::os::unix::fs::symlink("../lib/src", ws.join("a/src")).unwrap();
    settle();
    let r = w.call(&ws, "a", &[]);
    assert_eq!(r.outcome(), not_stored(NotStored::Unkeyable));
    assert_eq!(w.blocks(), 0);
}

// covers: B-6, B-10
#[test]
fn only_a_block_that_may_move_names_its_origin_to_dependents() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            embed_var: Some("CARGO_MANIFEST_DIR".into()),
            ..Plan::default()
        },
    );
    let ws = w.checkout("ws");
    settle();
    assert_eq!(w.call(&ws, "a", &[]).outcome(), STORED);
    let known = crate::sidecar::lookup(&w.rlib(&ws, "a")).unwrap();
    assert_eq!(known.produced_by, None, "a tied block named by its origin");
    assert!(known.step.is_some());
    // Reused in place: still not named by its origin.
    fs::remove_file(w.rlib(&ws, "a")).unwrap();
    assert_eq!(w.call(&ws, "a", &[]).outcome(), REUSED);
    let known = crate::sidecar::lookup(&w.rlib(&ws, "a")).unwrap();
    assert_eq!(known.produced_by, None, "a tied block named by its origin");
    assert!(known.reused);

    // A block that may move is.
    let w = world(Mode::On);
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    assert!(
        crate::sidecar::lookup(&w.rlib(&one, "a"))
            .unwrap()
            .produced_by
            .is_some()
    );
    assert_eq!(w.call(&two, "a", &[]).outcome(), REUSED);
    assert!(
        crate::sidecar::lookup(&w.rlib(&two, "a"))
            .unwrap()
            .produced_by
            .is_some()
    );
}

/// Two checkouts where `a` and `z` were built in one and reused in two, and `b`'s source.
fn two_reused(w: &World) -> (PathBuf, Digest, Digest) {
    let one = w.checkout("one");
    let two = w.checkout("two");
    for ws in [&one, &two] {
        fs::create_dir_all(ws.join("z/src")).unwrap();
        fs::write(ws.join("z/src/lib.rs"), "pub fn z() {}\n").unwrap();
        fs::create_dir_all(ws.join("b/src")).unwrap();
        fs::write(ws.join("b/src/lib.rs"), "").unwrap();
    }
    settle();
    let steps = |w: &World| -> std::collections::BTreeSet<Digest> {
        walk(&w.session.current().store.join("steps"))
            .iter()
            .filter(|p| p.parent().unwrap().ends_with("hints"))
            .map(|p| {
                let hex = p.parent().unwrap().parent().unwrap().file_name().unwrap();
                let hex = hex.to_str().unwrap();
                Digest::parse(hex)
                    .or_else(|_| Digest::parse(&format!("sha256:{hex}")))
                    .unwrap()
            })
            .collect()
    };
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    let a = *steps(w).iter().next().unwrap();
    assert_eq!(w.call(&one, "z", &[]).outcome(), STORED);
    let z = *steps(w).iter().find(|s| **s != a).unwrap();
    assert_eq!(w.call(&two, "a", &[]).outcome(), REUSED);
    assert_eq!(w.call(&two, "z", &[]).outcome(), REUSED);
    (two, a, z)
}

fn externs(ws: &Path) -> Vec<String> {
    let deps = ws.join("target/debug/deps");
    vec![
        "--extern".into(),
        format!("a={}", deps.join("liba-xa.rmeta").display()),
        "--extern".into(),
        format!("z={}", deps.join("libz-xz.rmeta").display()),
    ]
}

// covers: B-15
#[test]
fn a_crate_loading_error_suspends_only_the_reused_externs_it_names() {
    let cases: [(&str, &str, bool, bool); 5] = [
        // An error in the dependent's own code: a crate that is no extern at all.
        (
            "E0463",
            "can't find crate for `no_such_crate_typo`",
            false,
            false,
        ),
        // A crate one of the externs depends on: that extern.
        (
            "E0463",
            "can't find crate for `m` which `z` depends on",
            false,
            true,
        ),
        (
            "E0460",
            "found possibly newer version of crate `std` which `a` depends on",
            true,
            false,
        ),
        (
            "E0464",
            "multiple candidates for `rlib` dependency `z` found",
            false,
            true,
        ),
        (
            "E0786",
            "found invalid metadata files for crate `a`",
            true,
            false,
        ),
    ];
    for (code, message, rejects_a, rejects_z) in cases {
        let w = world(Mode::On);
        let (two, a, z) = two_reused(&w);
        w.plan(
            "b",
            Plan {
                fail: Some(code),
                fail_message: Some(message.into()),
                ..Plan::default()
            },
        );
        let more = externs(&two);
        let more: Vec<&str> = more.iter().map(String::as_str).collect();
        let r = w.call(&two, "b", &more);
        assert_eq!(r.code, 1);
        let deps = two.join("target/debug/deps");
        assert_eq!(w.store().suspended(&a).is_some(), rejects_a, "{message}");
        assert_eq!(!deps.join("liba-xa.rmeta").exists(), rejects_a, "{message}");
        assert_eq!(w.store().suspended(&z).is_some(), rejects_z, "{message}");
        assert_eq!(!deps.join("libz-xz.rlib").exists(), rejects_z, "{message}");
        assert_eq!(
            r.entry.unwrap().note.is_some(),
            rejects_a || rejects_z,
            "{message}"
        );
    }
}

// covers: B-15
#[test]
fn a_crate_loading_error_names_a_crate_that_was_not_reused_and_nothing_happens() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    fs::create_dir_all(ws.join("b/src")).unwrap();
    fs::write(ws.join("b/src/lib.rs"), "").unwrap();
    settle();
    // Built here, not reused.
    assert_eq!(w.call(&ws, "a", &[]).outcome(), STORED);
    let step = only_step(&w);
    w.plan(
        "b",
        Plan {
            fail: Some("E0463"),
            fail_message: Some("can't find crate for `x` which `a` depends on".into()),
            ..Plan::default()
        },
    );
    let ext = format!("a={}", ws.join("target/debug/deps/liba-xa.rmeta").display());
    let r = w.call(&ws, "b", &["--extern", &ext]);
    assert_eq!(r.code, 1);
    assert!(w.store().suspended(&step).is_none());
    assert!(w.rlib(&ws, "a").exists());
    assert!(r.entry.unwrap().note.is_none());
}

// covers: B-9, B-15
#[test]
fn an_incremental_rebuild_against_a_reused_extern_still_heals_a_rejected_block() {
    let w = world(Mode::On);
    let (two, a, z) = two_reused(&w);
    let incremental = format!(
        "incremental={}",
        two.join("target/debug/incremental").display()
    );
    // A warm target: b's dep-info is there, so b is an incremental rebuild.
    fs::write(two.join("target/debug/deps/b-xb.d"), "").unwrap();
    w.plan(
        "b",
        Plan {
            fail: Some("E0463"),
            fail_message: Some("can't find crate for `m` which `a` depends on".into()),
            ..Plan::default()
        },
    );
    let mut more = externs(&two);
    more.extend(["-C".to_string(), incremental.clone()]);
    let more: Vec<&str> = more.iter().map(String::as_str).collect();
    let r = w.call(&two, "b", &more);
    assert_eq!(r.code, 1);
    assert_eq!(
        r.outcome(),
        Outcome::PassedThrough(PassedThrough::Incremental)
    );
    // Run as asked, as a child: the same arguments, incremental kept, nothing added.
    assert!(r.seen.execs.is_empty());
    assert_eq!(r.seen.compiles.len(), 1);
    let asked: Vec<String> = strings(&w.args(&two, "b", &more)[1..]);
    assert_eq!(r.seen.compiles[0], asked);
    assert!(r.stderr.contains("E0463"));
    assert!(w.store().suspended(&a).is_some());
    assert!(w.store().suspended(&z).is_none());
    assert!(!two.join("target/debug/deps/liba-xa.rmeta").exists());
    assert!(r.entry.unwrap().note.is_some());

    // Without a reused extern, the rebuild replaces the process as before.
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    settle();
    fs::write(ws.join("target/debug/deps/a-xa.d"), "").unwrap();
    let r = w.call(&ws, "a", &["-C", &incremental]);
    assert_eq!(
        r.outcome(),
        Outcome::PassedThrough(PassedThrough::Incremental)
    );
    assert_eq!(r.seen.execs.len(), 1);
    assert!(r.seen.compiles.is_empty());
}

// covers: B-11
#[cfg(unix)]
#[test]
fn a_symlink_retargeted_during_the_compile_to_an_older_file_is_not_stored() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            reads: vec!["a/data/text.txt".into()],
            ..Plan::default()
        },
    );
    let ws = w.checkout("ws");
    fs::create_dir_all(ws.join("a/shared")).unwrap();
    fs::write(ws.join("a/shared/A.txt"), "AAA\n").unwrap();
    fs::write(ws.join("a/shared/B.txt"), "BBB\n").unwrap();
    let data = ws.join("a/data/text.txt");
    fs::remove_file(&data).unwrap();
    std::os::unix::fs::symlink("../shared/A.txt", &data).unwrap();
    settle();
    let swap = data.clone();
    let r = w.call_with(&ws, "a", &[], move |fake| {
        fake.during = Some(Box::new(move || {
            let temp = swap.with_extension("new");
            std::os::unix::fs::symlink("../shared/B.txt", &temp).unwrap();
            fs::rename(&temp, &swap).unwrap();
        }));
    });
    // rustc read A; the link now names B, whose dates are older than the compile.
    assert!(
        fs::read_to_string(w.rlib(&ws, "a"))
            .unwrap()
            .contains("AAA")
    );
    assert_eq!(r.outcome(), not_stored(NotStored::Changed));
    assert_eq!(w.blocks(), 0);

    // A folder on the way retargeted the same way.
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            reads: vec!["a/data/text.txt".into()],
            ..Plan::default()
        },
    );
    let ws = w.checkout("ws");
    fs::create_dir_all(ws.join("d1")).unwrap();
    fs::create_dir_all(ws.join("d2")).unwrap();
    fs::write(ws.join("d1/text.txt"), "one\n").unwrap();
    fs::write(ws.join("d2/text.txt"), "two\n").unwrap();
    fs::remove_dir_all(ws.join("a/data")).unwrap();
    std::os::unix::fs::symlink("../d1", ws.join("a/data")).unwrap();
    settle();
    let dir = ws.join("a/data");
    let r = w.call_with(&ws, "a", &[], move |fake| {
        fake.during = Some(Box::new(move || {
            let temp = dir.with_extension("new");
            std::os::unix::fs::symlink("../d2", &temp).unwrap();
            fs::rename(&temp, &dir).unwrap();
        }));
    });
    assert_eq!(r.outcome(), not_stored(NotStored::Changed));
}

// covers: B-11, B-16
#[test]
fn a_read_changed_between_the_lookup_and_the_compile_suspends_nothing() {
    let w = world(Mode::Observe);
    w.plan(
        "a",
        Plan {
            reads: vec!["a/data/text.txt".into()],
            ..Plan::default()
        },
    );
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    let data = two.join("a/data/text.txt");
    let r = w.call_with(&two, "a", &[], move |fake| {
        *fake.on_found.borrow_mut() = Some(Box::new(move || {
            // Replaced after the lookup hashed it, before the compile started.
            let temp = data.with_extension("new");
            fs::write(&temp, "changed\n").unwrap();
            fs::rename(&temp, &data).unwrap();
        }));
    });
    let entry = r.entry.unwrap();
    assert!(entry.found);
    assert_eq!(entry.outcome, not_stored(NotStored::Changed));
    assert!(w.store().suspended(&only_step(&w)).is_none());
}

// covers: B-8
#[cfg(unix)]
#[test]
fn a_tied_hint_from_another_checkout_is_skipped_before_anything_is_opened() {
    let w = world(Mode::On);
    w.plan(
        "a",
        Plan {
            // A file beside the workspace: outside every root, so the block is tied.
            reads: vec!["../proto/x.txt".into()],
            ..Plan::default()
        },
    );
    let one = w.checkout("one/rust");
    let two = w.checkout("two-longer/rust");
    for ws in [&one, &two] {
        let proto = ws.parent().unwrap().join("proto");
        fs::create_dir_all(&proto).unwrap();
        fs::write(proto.join("x.txt"), "proto\n").unwrap();
    }
    settle();
    assert_eq!(w.call(&one, "a", &[]).outcome(), STORED);
    // One's file becomes a pipe: opening it from two would block.
    let pipe = one.parent().unwrap().join("proto/x.txt");
    fifo(&pipe);
    let dog = Watchdog::new(vec![pipe], 5);
    let r = w.call(&two, "a", &[]);
    assert!(!dog.finish(), "two opened a file of checkout one");
    assert_eq!(r.outcome(), STORED);
    assert!(!r.entry.unwrap().found);

    // With one's file a plain file again, two still doesn't follow one's hint: it is skipped
    // for its roots, not for what it finds.
    fs::remove_file(one.parent().unwrap().join("proto/x.txt")).unwrap();
    fs::write(one.parent().unwrap().join("proto/x.txt"), "proto\n").unwrap();
    let step = only_step(&w);
    let hints: Vec<crate::key::ReadList> = w
        .store()
        .hints(&step)
        .iter()
        .map(|bytes| serde_json::from_slice(bytes).unwrap())
        .collect();
    assert_eq!(hints.len(), 2, "one hint per checkout");
    let follows = |ws: &Path| -> usize {
        let env = crate::invocation::Env::from_vars(w.vars(ws, "a"));
        let crate::invocation::Parsed::Handled(inv) =
            crate::invocation::parse(&w.args(ws, "a", &[]), &env, ws)
        else {
            panic!("not handled");
        };
        let roots = crate::roots::Roots::new(
            &crate::roots::RootPaths::of(&inv),
            &[],
            crate::roots::resolve_on_disk,
        )
        .unwrap();
        let cx = crate::inputs::Context {
            invocation: &inv,
            roots: &roots,
            env: &env,
            step,
            script: None,
        };
        let common = cx.common().unwrap();
        hints
            .iter()
            .filter(|hint| cx.from_hint(hint, &common).is_some())
            .count()
    };
    assert_eq!(follows(&one), 1);
    assert_eq!(follows(&two), 1);
}

/// A linked output whose link line names `tools/cc` as the linker and `vendor/libfoo.a` as a
/// library, both under the workspace.
fn linked_in(w: &World, ws: &Path) -> Outcome {
    let line = r#""{ws}/tools/cc" "{ws}/target/debug/deps/x.o" "{ws}/vendor/libfoo.a" "-o" "{ws}/target/debug/deps/liba-xa.dylib""#;
    w.plan(
        "a",
        Plan {
            reads: vec!["a/data/text.txt".into()],
            stdout: Some(format!("{line}\n")),
            ..Plan::default()
        },
    );
    let mut args = w.args(ws, "a", &[]);
    let at = args.iter().position(|a| a == "lib").unwrap();
    args[at] = "proc-macro".into();
    let mut fake = Fake {
        vars: w.vars(ws, "a"),
        cwd: ws.to_path_buf(),
        plans: w.plans.clone(),
        during: None,
        seen: Rc::default(),
        version_fails: false,
        audit: false,
        on_found: RefCell::new(None),
    };
    call(&mut fake, &mut Vec::new(), &args);
    w.session.current().entries().last().unwrap().outcome
}

fn with_tools(ws: &Path) {
    fs::create_dir_all(ws.join("tools")).unwrap();
    fs::write(ws.join("tools/cc"), b"driver").unwrap();
    fs::create_dir_all(ws.join("vendor")).unwrap();
    fs::write(ws.join("vendor/libfoo.a"), b"foo").unwrap();
}

// covers: B-7, B-8
#[test]
fn a_hint_is_followed_under_this_checkout_while_the_other_one_differs() {
    type Change = fn(&Path);
    let plain = Plan {
        reads: vec!["a/src/foo.rs".into(), "a/data/text.txt".into()],
        include_var: Some(("OUT_DIR".into(), "gen.rs".into())),
        ..Plan::default()
    };
    let kinds: [(&str, Change); 5] = [
        ("a read", |one| {
            fs::write(one.join("a/data/text.txt"), "other\n").unwrap()
        }),
        ("an absent place", |one| {
            fs::create_dir_all(one.join("a/src/foo")).unwrap();
            fs::write(one.join("a/src/foo/mod.rs"), "").unwrap();
        }),
        ("a shape folder", |one| {
            fs::write(one.join("a/data/more.txt"), "").unwrap()
        }),
        ("a declared input", |one| {
            fs::write(one.join("a/input.txt"), "other").unwrap()
        }),
        ("a generated file", |one| {
            fs::write(
                one.join("target/debug/build/a-1/out/gen.rs"),
                "pub const X: u8 = 2;\n",
            )
            .unwrap()
        }),
    ];
    for (what, change) in kinds {
        let w = world(Mode::On);
        w.plan("a", plain.clone());
        let one = w.checkout("one");
        let two = w.checkout("two-longer");
        let mut outs = Vec::new();
        for ws in [&one, &two] {
            let (name, out) = with_script(ws);
            fs::write(
                out.clone() + "/../output",
                "cargo:rerun-if-changed=input.txt\n",
            )
            .unwrap();
            fs::write(ws.join("a/input.txt"), "input").unwrap();
            outs.push((name, out));
        }
        settle();
        let set = |(name, out): (String, String)| {
            move |f: &mut Fake| {
                f.vars.insert(name, out);
            }
        };
        assert_eq!(
            w.call_with(&one, "a", &[], set(outs[0].clone())).outcome(),
            STORED,
            "{what}"
        );
        change(&one);
        settle();
        let r = w.call_with(&two, "a", &[], set(outs[1].clone()));
        assert_eq!(r.outcome(), REUSED, "{what}: two read checkout one");
    }
    // The linker and a library the linker was handed.
    let kinds: [(&str, Change); 2] = [
        ("a link library", |one| {
            fs::write(one.join("vendor/libfoo.a"), b"other foo").unwrap()
        }),
        ("the linker", |one| {
            fs::write(one.join("tools/cc"), b"another driver").unwrap()
        }),
    ];
    for (what, change) in kinds {
        let w = world(Mode::On);
        let one = w.checkout("one");
        let two = w.checkout("two-longer");
        with_tools(&one);
        with_tools(&two);
        settle();
        assert_eq!(linked_in(&w, &one), STORED, "{what}");
        change(&one);
        settle();
        assert_eq!(linked_in(&w, &two), REUSED, "{what}: two read checkout one");
        // And a change here is seen.
        let three = w.checkout("three");
        with_tools(&three);
        change(&three);
        settle();
        assert_eq!(linked_in(&w, &three), STORED, "{what}: a change missed");
    }
}

// covers: B-11, B-18
#[test]
fn a_link_input_replaced_during_the_compile_is_not_stored() {
    for file in ["vendor/libfoo.a", "script.ld"] {
        let w = world(Mode::On);
        let ws = w.checkout("ws");
        with_tools(&ws);
        fs::write(ws.join("script.ld"), b"SECTIONS { }").unwrap();
        let line = r#""{ws}/tools/cc" "{ws}/vendor/libfoo.a" "-T{ws}/script.ld" "-o" "{ws}/target/debug/deps/liba-xa.dylib""#;
        w.plan(
            "a",
            Plan {
                stdout: Some(format!("{line}\n")),
                ..Plan::default()
            },
        );
        settle();
        let mut args = w.args(&ws, "a", &[]);
        let at = args.iter().position(|a| a == "lib").unwrap();
        args[at] = "proc-macro".into();
        let path = ws.join(file);
        let old = fs::metadata(&path).unwrap().modified().unwrap();
        let mut fake = Fake {
            vars: w.vars(&ws, "a"),
            cwd: ws.clone(),
            plans: w.plans.clone(),
            during: Some(Box::new(move || {
                let temp = path.with_extension("new");
                fs::write(&temp, b"replaced, same date").unwrap();
                fs::File::options()
                    .write(true)
                    .open(&temp)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
                fs::rename(&temp, &path).unwrap();
            })),
            seen: Rc::default(),
            version_fails: false,
            audit: false,
            on_found: RefCell::new(None),
        };
        call(&mut fake, &mut Vec::new(), &args);
        let outcome = w.session.current().entries().last().unwrap().outcome;
        assert_eq!(outcome, not_stored(NotStored::Changed), "{file}");
        assert_eq!(w.blocks(), 0, "{file}");
    }
}

// covers: B-8, B-18
#[test]
fn a_linker_outside_every_root_ties_the_block() {
    let w = world(Mode::On);
    let tools = w.dir.path().join("tools");
    fs::create_dir_all(&tools).unwrap();
    fs::write(tools.join("cc"), b"driver").unwrap();
    let line = format!(
        r#""{}" "{{ws}}/target/debug/deps/x.o" "-o" "{{ws}}/target/debug/deps/liba-xa.dylib""#,
        tools.join("cc").display()
    );
    w.plan(
        "a",
        Plan {
            stdout: Some(format!("{line}\n")),
            ..Plan::default()
        },
    );
    let run = |ws: &Path| {
        let mut args = w.args(ws, "a", &[]);
        let at = args.iter().position(|a| a == "lib").unwrap();
        args[at] = "proc-macro".into();
        let mut fake = Fake {
            vars: w.vars(ws, "a"),
            cwd: ws.to_path_buf(),
            plans: w.plans.clone(),
            during: None,
            seen: Rc::default(),
            version_fails: false,
            audit: false,
            on_found: RefCell::new(None),
        };
        call(&mut fake, &mut Vec::new(), &args);
        w.session.current().entries().last().unwrap().outcome
    };
    let one = w.checkout("one");
    let two = w.checkout("two");
    settle();
    assert_eq!(run(&one), STORED);
    assert_eq!(run(&two), STORED, "a block tied to its linker moved");
    // In its own checkout it is reused.
    fs::remove_file(one.join("target/debug/deps/liba-xa.rlib")).unwrap();
    assert_eq!(run(&one), REUSED);
}
