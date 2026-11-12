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
    /// Write the output folder's path into the debug bundle's DWARF file, as debug info that
    /// names a file under a root does.
    bundle_root: bool,
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
        let mut packed = false;
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
                    if let Some(split) = v.strip_prefix("split-debuginfo=") {
                        packed = split == "packed";
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
        // dsymutil's bundle beside the program, as rustc writes it with packed debug info: it
        // adds to a bundle that is there, and its relocations name the program by its path.
        if packed && emits.iter().any(|e| e == "link") {
            let bundle = out.join(format!("{stem}.dSYM/Contents"));
            fs::create_dir_all(bundle.join("Resources/DWARF")).unwrap();
            fs::create_dir_all(bundle.join("Resources/Relocations/aarch64")).unwrap();
            fs::write(
                bundle.join("Info.plist"),
                format!("<plist>{stem}</plist>\n"),
            )
            .unwrap();
            let mut dwarf = format!("dwarf\n{content}");
            if plan.bundle_root {
                dwarf.push_str(&format!("object {out_dir}/x.o\n"));
            }
            fs::write(bundle.join(format!("Resources/DWARF/{stem}")), dwarf).unwrap();
            fs::write(
                bundle.join(format!("Resources/Relocations/aarch64/{stem}.yml")),
                format!("binary-path: '{out_dir}/{stem}'\n"),
            )
            .unwrap();
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
        // The sysroot rustc uses: the one `--sysroot` names, else the toolchain's.
        let sysroot = args
            .iter()
            .find_map(|a| a.strip_prefix("--sysroot=").map(str::to_string))
            .or_else(|| {
                Some(format!(
                    "{}/toolchains/{}",
                    self.vars.get("RUSTUP_HOME")?,
                    self.vars.get("RUSTUP_TOOLCHAIN")?
                ))
            })
            .unwrap_or_default();
        text.replace("{ws}", self.cwd.to_str().unwrap())
            .replace("{sysroot}", &sysroot)
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
        self.call_args(ws, crate_name, self.args(ws, crate_name, more), change)
    }

    /// A call with these arguments.
    fn call_args(
        &self,
        ws: &Path,
        crate_name: &str,
        args: Vec<OsString>,
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
        let code = call(&mut fake, &mut stderr, &args);
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

/// rustc ran once, exactly as asked: as a child (a crate with something local to watch,
/// B-23) or replacing the process. Its arguments, without the compiler's path.
fn ran_as_asked(r: &Result) -> Vec<String> {
    let runs: Vec<&Vec<String>> = r.seen.execs.iter().chain(&r.seen.compiles).collect();
    assert_eq!(runs.len(), 1, "{:?}", runs);
    assert!(!runs[0].iter().any(|a| a.starts_with("--remap-path-prefix")));
    runs[0].clone()
}

/// A crate whose package lies in a shared folder, as one from a registry does: nothing local to
/// watch (B-23).
fn from_a_registry(fake: &mut Fake) {
    fake.vars.insert(
        "CARGO_MANIFEST_DIR".into(),
        "/usr/registry/src/a-0.1.0".into(),
    );
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
    ran_as_asked(&r);
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
    // Written just now: on Linux, within the clock's slack of the compile (B-11).
    settle();
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
        &crate::roots::Machine::default(),
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
    let asked = ran_as_asked(&rebuild);
    assert!(asked.contains(&incremental));
    assert!(!asked.iter().any(|a| a.starts_with("--remap")));
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
    ran_as_asked(&r);
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

/// A checkout at `rest` whose `--extern sqlx` file has the same bytes in every checkout. The
/// table of known macros goes by the extern's name: sqlx's read the first `.env` found walking up
/// from the package and from the working directory (B-6 `declared`).
fn sqlx_checkout(w: &World, rest: &str) -> PathBuf {
    let ws = w.checkout(rest);
    fs::write(ws.join("target/debug/deps/libsqlx-s.rlib"), "the same sqlx").unwrap();
    ws
}

fn sqlx_call(w: &World, ws: &Path, change: impl FnOnce(&mut Fake)) -> Result {
    let ext = format!(
        "sqlx={}",
        ws.join("target/debug/deps/libsqlx-s.rlib").display()
    );
    w.call_with(ws, "a", &["--extern", &ext], change)
}

// covers: B-6, B-8
#[test]
fn a_dotenv_search_above_the_checkout_is_keyed_by_the_file_it_finds_not_by_where() {
    let w = world(Mode::On);
    w.plan("a", Plan::default());
    let one = sqlx_checkout(&w, "first-parent/one");
    let two = sqlx_checkout(&w, "the-second-parent/two");
    settle();
    assert_eq!(sqlx_call(&w, &one, |_| {}).outcome(), STORED);
    // No `.env` above either checkout: the block moves.
    assert_eq!(sqlx_call(&w, &two, |_| {}).outcome(), REUSED);

    // A `.env` in a parent of one checkout: another key.
    let three = sqlx_checkout(&w, "third/three");
    fs::write(
        w.dir.path().join("third/.env"),
        "DATABASE_URL=postgres://x\n",
    )
    .unwrap();
    settle();
    let r = sqlx_call(&w, &three, |_| {});
    assert_eq!(r.outcome(), STORED);
    assert!(!r.entry.unwrap().found);

    // The same bytes above another checkout, two folders up: that block, not where it lies.
    let four = sqlx_checkout(&w, "fourth/and-deeper/four");
    fs::write(
        w.dir.path().join("fourth/.env"),
        "DATABASE_URL=postgres://x\n",
    )
    .unwrap();
    settle();
    assert_eq!(sqlx_call(&w, &four, |_| {}).outcome(), REUSED);

    // Other bytes: another key.
    let five = sqlx_checkout(&w, "fifth/five");
    fs::write(
        w.dir.path().join("fifth/.env"),
        "DATABASE_URL=postgres://y\n",
    )
    .unwrap();
    settle();
    let r = sqlx_call(&w, &five, |_| {});
    assert_eq!(r.outcome(), STORED);
    assert!(!r.entry.unwrap().found);

    // A folder named `.env` is not what the search finds: it goes on up, as dotenvy's does.
    let six = sqlx_checkout(&w, "sixth/six");
    fs::create_dir_all(w.dir.path().join("sixth/.env")).unwrap();
    settle();
    assert_eq!(sqlx_call(&w, &six, |_| {}).outcome(), REUSED);
}

// covers: B-6, B-11
#[test]
fn a_dotenv_appearing_above_the_checkout_while_compiling_is_not_stored() {
    let w = world(Mode::On);
    w.plan("a", Plan::default());
    let ws = sqlx_checkout(&w, "parent/ws");
    settle();
    let env = w.dir.path().join("parent/.env");
    let r = sqlx_call(&w, &ws, move |fake| {
        fake.during = Some(Box::new(move || {
            fs::write(&env, "DATABASE_URL=postgres://late\n").unwrap();
        }));
    });
    assert_eq!(r.outcome(), not_stored(NotStored::Changed));
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
    ran_as_asked(&r);
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

    // Without a reused extern, the rebuild of a crate with nothing local to watch replaces the
    // process as before (B-23).
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    settle();
    fs::write(ws.join("target/debug/deps/a-xa.d"), "").unwrap();
    let r = w.call_with(&ws, "a", &["-C", &incremental], from_a_registry);
    assert_eq!(
        r.outcome(),
        Outcome::PassedThrough(PassedThrough::Incremental)
    );
    assert_eq!(r.seen.execs.len(), 1);
    assert!(r.seen.compiles.is_empty());
}

/// A reason to pass through: its name, the settings it needs and the arguments it adds.
type Reason = (
    &'static str,
    fn(&mut settings::Settings),
    &'static [&'static str],
);

// covers: B-15
#[test]
fn every_pass_through_after_the_arguments_were_read_heals_a_rejected_reused_block() {
    let reasons: [Reason; 2] = [
        ("never-store", |s| s.never_store = vec!["b".into()], &[]),
        // Not one of the compiler's own targets: refused after the parse.
        ("target", |_| {}, &["--target", "thumbv0-none-fake"]),
    ];
    for (name, settings, more) in reasons {
        let w = world_with(Mode::On, settings);
        let (two, a, z) = two_reused(&w);
        w.plan(
            "b",
            Plan {
                fail: Some("E0463"),
                fail_message: Some("can't find crate for `m` which `a` depends on".into()),
                ..Plan::default()
            },
        );
        let mut args = externs(&two);
        args.extend(more.iter().map(|s| s.to_string()));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let r = w.call(&two, "b", &args);
        assert_eq!(r.code, 1, "{name}");
        assert!(
            matches!(r.outcome(), Outcome::PassedThrough(_)),
            "{name}: {:?}",
            r.outcome()
        );
        // Run as asked, as a child: the same arguments, nothing added.
        assert!(r.seen.execs.is_empty(), "{name}");
        assert_eq!(r.seen.compiles.len(), 1, "{name}");
        assert_eq!(r.seen.compiles[0], strings(&w.args(&two, "b", &args)[1..]));
        assert!(r.stderr.contains("E0463"), "{name}");
        assert!(w.store().suspended(&a).is_some(), "{name}");
        assert!(w.store().suspended(&z).is_none(), "{name}");
        assert!(
            !two.join("target/debug/deps/liba-xa.rmeta").exists(),
            "{name}"
        );
        assert!(r.entry.unwrap().note.is_some(), "{name}");
    }

    // Built as asked: b's own block can't be put in place.
    let w = world(Mode::On);
    let (two, a, _) = two_reused(&w);
    let one = two.parent().unwrap().join("one");
    let ext_one = externs(&one);
    let ext_one: Vec<&str> = ext_one.iter().map(String::as_str).collect();
    assert_eq!(w.call(&one, "b", &ext_one).outcome(), STORED);
    let json = walk(&w.session.current().store.join("blocks"))
        .into_iter()
        .filter(|p| p.ends_with("block.json"))
        .find(|p| {
            fs::read_to_string(p)
                .unwrap()
                .contains(r#""crate_name":"b""#)
        })
        .unwrap();
    let mut perms = fs::metadata(&json).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&json, perms).unwrap();
    let text = fs::read_to_string(&json).unwrap();
    fs::write(&json, text.replace("{hushrun:out-dir}", "{hushrun:deps-9}")).unwrap();
    w.plan(
        "b",
        Plan {
            fail: Some("E0463"),
            fail_message: Some("can't find crate for `m` which `a` depends on".into()),
            ..Plan::default()
        },
    );
    let ext_two = externs(&two);
    let ext_two: Vec<&str> = ext_two.iter().map(String::as_str).collect();
    let r = w.call(&two, "b", &ext_two);
    assert_eq!(r.outcome(), not_stored(NotStored::Restore));
    assert!(r.seen.execs.is_empty());
    assert_eq!(r.seen.compiles.len(), 1);
    assert!(w.store().suspended(&a).is_some());
    assert!(!two.join("target/debug/deps/liba-xa.rmeta").exists());

    // No reused extern, and nothing local to watch: the process is replaced as before.
    let w = world_with(Mode::On, |s| s.never_store = vec!["b".into()]);
    let ws = w.checkout("ws");
    fs::create_dir_all(ws.join("b/src")).unwrap();
    fs::write(ws.join("b/src/lib.rs"), "").unwrap();
    settle();
    assert_eq!(w.call(&ws, "a", &[]).outcome(), STORED);
    let ext = format!("a={}", ws.join("target/debug/deps/liba-xa.rmeta").display());
    let r = w.call_with(&ws, "b", &["--extern", &ext], from_a_registry);
    assert_eq!(
        r.outcome(),
        Outcome::PassedThrough(PassedThrough::NeverStore)
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
            &crate::roots::Machine::default(),
            crate::roots::resolve_on_disk,
        )
        .unwrap();
        let cx = crate::inputs::Context {
            invocation: &inv,
            roots: &roots,
            env: &env,
            step,
            script: None,
            run_memo: None,
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

// covers: B-6, B-8
#[test]
fn a_native_folder_is_an_input_and_a_root_of_its_own() {
    // Another package's build-script output: `#[link(name = "x", kind = "static")]` in the
    // source bundles `libx.a` from it, and the dep-info never says so.
    let native_in = |ws: &Path| ws.join("target/debug/build/z-1/out");
    let setup = |w: &World, name: &str, native: &dyn Fn(&Path) -> PathBuf, bytes: &[u8]| {
        let ws = w.checkout(name);
        let dir = native(&ws);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("libx.a"), bytes).unwrap();
        fs::write(dir.join("x.h"), "").unwrap();
        let dir = crate::roots::resolve_on_disk(dir.to_str().unwrap()).unwrap();
        (ws, format!("native={dir}"), PathBuf::from(dir))
    };
    let run = |w: &World, (ws, native, _): &(PathBuf, String, PathBuf)| {
        settle();
        w.call(ws, "a", &["-L", native]).outcome()
    };
    let w = world(Mode::On);
    let one = setup(&w, "one", &native_in, b"x one");
    assert_eq!(run(&w, &one), STORED);
    assert!(
        !one.2.join(crate::sidecar::DIR).exists(),
        "a record in another package's folder"
    );
    assert_eq!(
        run(&w, &setup(&w, "two-longer", &native_in, b"x one")),
        REUSED
    );
    assert_eq!(
        run(&w, &setup(&w, "three", &native_in, b"x two")),
        STORED,
        "a static library changed"
    );
    let four = setup(&w, "four", &native_in, b"x one");
    fs::write(four.2.join("liby.a"), b"y").unwrap();
    assert_eq!(run(&w, &four), STORED, "a static library appeared");
    let five = setup(&w, "five", &native_in, b"x one");
    fs::write(five.2.join("notes.txt"), b"").unwrap();
    assert_eq!(run(&w, &five), STORED, "the folder's names changed");

    // A target folder outside the workspace: the folder is a root of its own, named by its
    // place among the `-L` flags, so another checkout's folder of the same contents reuses.
    let w = world(Mode::On);
    let dir = w.dir.path().to_path_buf();
    let outside = move |ws: &Path| {
        dir.join("targets")
            .join(ws.file_name().unwrap())
            .join("debug/build/z-1/out")
    };
    assert_eq!(run(&w, &setup(&w, "one", &outside, b"x one")), STORED);
    assert_eq!(
        run(&w, &setup(&w, "two-longer", &outside, b"x one")),
        REUSED
    );
    assert_eq!(run(&w, &setup(&w, "three", &outside, b"x two")), STORED);
}

// covers: B-11
#[test]
fn a_native_library_replaced_during_the_compile_is_not_stored() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let dir = ws.join("target/debug/build/z-1/out");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("libx.a"), b"x one").unwrap();
    settle();
    let native = format!("native={}", dir.display());
    let lib = dir.join("libx.a");
    let old = fs::metadata(&lib).unwrap().modified().unwrap();
    let r = w.call_with(&ws, "a", &["-L", &native], move |fake| {
        fake.during = Some(Box::new(move || {
            let temp = lib.with_extension("new");
            fs::write(&temp, b"x two").unwrap();
            fs::File::options()
                .write(true)
                .open(&temp)
                .unwrap()
                .set_modified(old)
                .unwrap();
            fs::rename(&temp, &lib).unwrap();
        }));
    });
    assert_eq!(r.outcome(), not_stored(NotStored::Changed));
    assert_eq!(w.blocks(), 0);
}

/// The outcome of a proc macro whose compile printed `line` as its link line.
fn linked_with(w: &World, ws: &Path, line: &str) -> Outcome {
    w.plan(
        "a",
        Plan {
            stdout: Some(format!("{line}\n")),
            ..Plan::default()
        },
    );
    settle();
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

// covers: B-6, B-18
#[test]
fn every_file_the_link_line_names_is_keyed() {
    let line = r#""{ws}/tools/cc" "{ws}/extra/glue.o" "{ws}/extra/blob.bin" "-Wl,-bundle_loader,{ws}/extra/host" "-Wl,-dylib_file,@rpath/libq.dylib:{ws}/extra/libq.dylib" "-o" "{ws}/target/debug/deps/liba-xa.dylib""#;
    let setup = |w: &World, name: &str| {
        let ws = w.checkout(name);
        with_tools(&ws);
        fs::create_dir_all(ws.join("extra")).unwrap();
        for file in ["glue.o", "blob.bin", "host", "libq.dylib"] {
            fs::write(ws.join("extra").join(file), file).unwrap();
        }
        ws
    };
    let w = world(Mode::On);
    assert_eq!(linked_with(&w, &setup(&w, "one"), line), STORED);
    assert_eq!(linked_with(&w, &setup(&w, "two-longer"), line), REUSED);
    for file in ["glue.o", "blob.bin", "host", "libq.dylib"] {
        let ws = setup(&w, &format!("changed-{file}"));
        fs::write(ws.join("extra").join(file), "other bytes").unwrap();
        assert_eq!(linked_with(&w, &ws, line), STORED, "{file} changed");
    }
}

// covers: B-18
#[test]
fn a_driver_prefix_outside_the_compilers_sysroot_is_not_stored() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    with_tools(&ws);
    let sysroot_bin = w
        .rustc
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("lib/rustlib/x/bin/gcc-ld");
    fs::create_dir_all(&sysroot_bin).unwrap();
    let inside = format!(
        r#""{{ws}}/tools/cc" "-B{}" "-o" "{{ws}}/target/debug/deps/liba-xa.dylib""#,
        sysroot_bin.display()
    );
    assert_eq!(linked_with(&w, &ws, &inside), STORED);
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    with_tools(&ws);
    let outside = r#""{ws}/tools/cc" "-B{ws}/tools" "-o" "{ws}/target/debug/deps/liba-xa.dylib""#;
    assert_eq!(
        linked_with(&w, &ws, outside),
        not_stored(NotStored::LinkInputs)
    );
}

/// The arguments of the program `a` as cargo's dev profile gives them on macOS: a bin with
/// debug info in unpacked form; `more` at the end.
fn program_args(w: &World, ws: &Path, more: &[&str]) -> Vec<OsString> {
    let mut args = w.args(
        ws,
        "a",
        &["-C", "debuginfo=2", "-C", "split-debuginfo=unpacked"],
    );
    let at = args.iter().position(|a| a == "lib").unwrap();
    args[at] = "bin".into();
    args.extend(more.iter().map(OsString::from));
    args
}

/// A checkout whose program `a` is linked by `tools/cc`, and whose `PATH` finds `tools/dsymutil`.
fn program_checkout(w: &World, name: &str) -> PathBuf {
    let ws = w.checkout(name);
    with_tools(&ws);
    let dsymutil = ws.join("tools/dsymutil");
    fs::write(&dsymutil, b"dsymutil").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dsymutil, fs::Permissions::from_mode(0o755)).unwrap();
    }
    ws
}

/// The link line rustc prints for `a`.
const PROGRAM_LINK: &str =
    r#""{ws}/tools/cc" "{ws}/vendor/libfoo.a" "-o" "{ws}/target/debug/deps/a-xa""#;

/// Calls the program `a` in `ws` with `more` after cargo's arguments.
fn call_program(w: &World, ws: &Path, more: &[&str]) -> Result {
    let tools = ws.join("tools").to_str().unwrap().to_string();
    w.call_args(ws, "a", program_args(w, ws, more), move |fake| {
        fake.vars.insert("PATH".into(), tools);
    })
}

/// Every file of `a`'s bundle in `ws`, by its path in the bundle, with its bytes.
fn bundle_of(ws: &Path) -> BTreeMap<String, Vec<u8>> {
    let dir = ws.join("target/debug/deps/a-xa.dSYM");
    walk(&dir)
        .into_iter()
        .map(|f| {
            let rest = f.strip_prefix(&dir).unwrap().to_str().unwrap().to_string();
            (rest, fs::read(&f).unwrap())
        })
        .collect()
}

fn program_plan(bundle_root: bool) -> Plan {
    Plan {
        stdout: Some(format!("{PROGRAM_LINK}\n")),
        bundle_root,
        ..Plan::default()
    }
}

/// Begins the world's next session in the same store, with its settings changed.
fn next_session(w: &mut World, change: impl FnOnce(&mut settings::Settings)) {
    let settings = w.dir.path().join("build.json");
    settings::update(&settings, change).unwrap();
    let exe = w.dir.path().join("hushrun");
    let cache = w.dir.path().join("cache");
    w.session = session::begin(&Start {
        settings: &settings,
        cache_dir: Some(&cache),
        exe: &exe,
        in_ci: false,
        repository: Some("test/repo"),
        env: &|_| None,
        now: SystemTime::now(),
    })
    .unwrap();
}

// covers: B-5, B-9, B-21
#[test]
fn a_program_set_to_tied_is_compiled_as_asked_and_never_takes_a_movable_block() {
    let mut w = world(Mode::On);
    w.plan("a", program_plan(false));
    let one = program_checkout(&w, "one");
    let two = program_checkout(&w, "two-longer");
    settle();
    assert_eq!(call_program(&w, &one, &[]).outcome(), STORED);
    assert!(one.join("target/debug/deps/a-xa.dSYM").is_dir());

    next_session(&mut w, |s| s.programs = crate::model::Programs::Tied);
    // Another checkout: a movable block would be reused here, but the setting is part of the
    // first key, so a tied program never takes it.
    let r = call_program(&w, &two, &[]);
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    assert!(!r.entry.as_ref().unwrap().found);
    // Nothing is added to the compile but what B-18 needs, and no bundle is made.
    let asked = &r.seen.compiles[0];
    assert!(
        !asked.iter().any(|a| a.starts_with("split-debuginfo=packed")
            || a.starts_with("strip=")
            || a.contains("oso_prefix")),
        "{asked:?}"
    );
    assert!(!two.join("target/debug/deps/a-xa.dSYM").exists());
    // The first checkout's bundle, left by the movable compile, matches no program compiled
    // tied: an edit there is compiled with the bundle removed first, and stored.
    fs::write(one.join("a/src/lib.rs"), "mod foo;\nfn edited() {}\n").unwrap();
    settle();
    let r = call_program(&w, &one, &[]);
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    assert!(!one.join("target/debug/deps/a-xa.dSYM").exists());
    // And reused where it was built.
    settle();
    assert_eq!(call_program(&w, &two, &[]).outcome(), REUSED);
    assert!(!two.join("target/debug/deps/a-xa.dSYM").exists());
}

// covers: B-9, B-12, B-18
#[test]
fn a_programs_debug_bundle_is_stored_and_put_back_whole() {
    let w = world(Mode::On);
    w.plan("a", program_plan(false));
    let one = program_checkout(&w, "one");
    let two = program_checkout(&w, "two-longer");
    settle();
    let r = call_program(&w, &one, &[]);
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    // What the wrapper adds to a program's compile on an Apple target (B-9).
    let asked = &r.seen.compiles[0];
    let tail: Vec<&str> = asked
        .iter()
        .rev()
        .take(6)
        .rev()
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        [
            "-C",
            "split-debuginfo=packed",
            "-C",
            "strip=debuginfo",
            "-C",
            &format!("link-arg=-Wl,-oso_prefix,{}/", one.display()),
        ]
    );
    // The relocations dsymutil wrote name the program's folder: they are not kept.
    let made = bundle_of(&one);
    assert!(!made.keys().any(|k| k.contains("Relocations")), "{made:?}");
    assert!(made.contains_key("Contents/Resources/DWARF/a-xa"));
    let names: Vec<String> = walk(&w.session.current().store.join("blocks"))
        .into_iter()
        .filter_map(|p| {
            let p = p.to_str().unwrap().to_string();
            p.split_once("/files/").map(|(_, name)| name.to_string())
        })
        .collect();
    assert!(
        names.contains(&"a-xa.dSYM/Contents/Resources/DWARF/a-xa".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"a-xa.dSYM/Contents/Info.plist".to_string()));

    // An earlier bundle at two, with a file of its own: replaced whole.
    let old = two.join("target/debug/deps/a-xa.dSYM/Contents/Resources");
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("left-over"), b"old").unwrap();
    let r = call_program(&w, &two, &[]);
    assert_eq!(r.outcome(), REUSED, "{}", r.stderr);
    assert_eq!(bundle_of(&two), made);
    let deps = two.join("target/debug/deps");
    let hidden: Vec<String> = fs::read_dir(&deps)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with(".hushrun-restore") || n.starts_with(".hushrun-old"))
        .collect();
    assert!(hidden.is_empty(), "{hidden:?}");
    assert!(
        !walk(&deps.join("a-xa.dSYM"))
            .iter()
            .any(|p| p.to_str().unwrap().contains(".hushrun"))
    );

    // A program the person strips or keeps from packing, one with no debug info, one for
    // another target, and a library: none is changed.
    for more in [
        &["-C", "strip=symbols"][..],
        &["-C", "strip=debuginfo"],
        &["-C", "split-debuginfo=off"],
        &["-C", "debuginfo=0"],
        &["--target", "x86_64-unknown-linux-gnu"],
    ] {
        let ws = program_checkout(&w, &format!("left-{}", more[1]));
        let r = call_program(&w, &ws, more);
        let asked = r.seen.compiles.first().or(r.seen.execs.first()).unwrap();
        assert!(
            !asked
                .iter()
                .any(|a| a == "split-debuginfo=packed" || a.contains("oso_prefix")),
            "{more:?}: {asked:?}"
        );
        assert!(!ws.join("target/debug/deps/a-xa.dSYM").exists(), "{more:?}");
    }
    let ws = w.checkout("library");
    let r = w.call(&ws, "a", &["-C", "debuginfo=2"]);
    assert!(
        !r.seen.compiles[0]
            .iter()
            .any(|a| a == "split-debuginfo=packed")
    );
}

// covers: B-10
#[test]
fn a_bundle_file_that_holds_a_root_ties_the_program() {
    let w = world(Mode::On);
    w.plan("a", program_plan(true));
    let one = program_checkout(&w, "one");
    let two = program_checkout(&w, "two-longer");
    settle();
    assert_eq!(call_program(&w, &one, &[]).outcome(), STORED);
    let r = call_program(&w, &two, &[]);
    assert_eq!(r.outcome(), STORED, "a bundle naming one's folder moved");
    assert!(!r.entry.unwrap().found);
    // Tied, it is still reused where it was built.
    fs::remove_dir_all(one.join("target/debug/deps/a-xa.dSYM")).unwrap();
    assert_eq!(call_program(&w, &one, &[]).outcome(), REUSED);
    assert!(bundle_of(&one).contains_key("Contents/Resources/DWARF/a-xa"));
}

// covers: B-12, B-19
#[test]
fn a_kill_between_placing_a_program_and_its_bundle_leaves_nothing_taken_for_whole() {
    let w = world(Mode::On);
    w.plan("a", program_plan(false));
    let one = program_checkout(&w, "one");
    settle();
    assert_eq!(call_program(&w, &one, &[]).outcome(), STORED);
    let made = bundle_of(&one);
    let json = walk(&w.session.current().store.join("blocks"))
        .into_iter()
        .find(|p| p.ends_with("block.json"))
        .unwrap();
    let hex = json
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let key = Digest::parse(&format!("sha256:{hex}")).unwrap();
    let block = w.store().block(&key).unwrap().unwrap();
    let description = crate::description::Description::from_value(&block.description).unwrap();
    // A kill after each rename but the last: the bundle, then each file, then the dep-info.
    for renamed in 0..3 {
        let two = program_checkout(&w, &format!("two-{renamed}"));
        let deps = two.join("target/debug/deps");
        let tools = two.join("tools").to_str().unwrap().to_string();
        let mut vars = w.vars(&two, "a");
        vars.insert("PATH".into(), tools);
        let env = crate::invocation::Env::from_vars(vars);
        let crate::invocation::Parsed::Handled(inv) =
            crate::invocation::parse(&program_args(&w, &two, &[]), &env, &two)
        else {
            panic!("not handled");
        };
        let roots = crate::roots::Roots::new(
            &crate::roots::RootPaths::of(&inv),
            &[],
            &crate::roots::Machine::default(),
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
        assert_eq!(temps.len(), 4, "{temps:?}");
        assert!(
            temps[0].to_str().unwrap().ends_with(".a-xa.dSYM"),
            "{temps:?}"
        );
        prepared.place_first(renamed).unwrap();
        // A kill now: nothing runs after this point.
        std::mem::forget(prepared);
        for temp in &temps[renamed..] {
            let name = temp.file_name().unwrap().to_str().unwrap();
            assert!(
                name.starts_with(".hushrun") && !name.starts_with("lib"),
                "{name}"
            );
        }
        // The bundle is there whole or not at all; the dep-info cargo reads is not there; no
        // file is recorded as reused.
        let bundle = deps.join("a-xa.dSYM");
        assert!(!bundle.exists() || bundle_of(&two) == made, "{renamed}");
        assert_eq!(bundle.exists(), renamed >= 1);
        assert!(!deps.join("a-xa.d").exists(), "{renamed}");
        assert!(crate::sidecar::reused_in(&deps).unwrap().is_empty());
        // Cargo asks again: the block is reused, whole.
        let r = call_program(&w, &two, &[]);
        assert_eq!(r.outcome(), REUSED, "{renamed}: {}", r.stderr);
        assert_eq!(bundle_of(&two), made, "{renamed}");
        assert_eq!(
            fs::read(deps.join("liba-xa.rlib")).unwrap(),
            fs::read(one.join("target/debug/deps/liba-xa.rlib")).unwrap()
        );
    }
}

// covers: B-18
#[test]
fn the_dsymutil_that_writes_a_programs_bundle_is_keyed() {
    let w = world(Mode::On);
    w.plan("a", program_plan(false));
    let one = program_checkout(&w, "one");
    let two = program_checkout(&w, "two-longer");
    let three = program_checkout(&w, "three");
    fs::write(three.join("tools/dsymutil"), b"another dsymutil").unwrap();
    settle();
    assert_eq!(call_program(&w, &one, &[]).outcome(), STORED);
    assert_eq!(call_program(&w, &two, &[]).outcome(), REUSED);
    let r = call_program(&w, &three, &[]);
    assert_eq!(r.outcome(), STORED, "another dsymutil wrote this bundle");
    assert!(!r.entry.unwrap().found);
}

/// Where the unit record of `a` lies in `ws`.
fn unit_record(ws: &Path) -> PathBuf {
    crate::unit::record_path(&ws.join("target/debug/deps"), "a-xa")
}

// covers: B-23
#[test]
fn every_call_leaves_a_record_of_its_unit_or_none() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    settle();
    // A compile seen through: a record, and the output folder registered once.
    assert_eq!(w.call(&ws, "a", &[]).outcome(), STORED);
    let text = fs::read_to_string(unit_record(&ws)).unwrap();
    assert!(
        text.contains("\"sha256\":\"sha256:"),
        "a digest for what was hashed: {text}"
    );
    assert_eq!(w.store().out_dirs().len(), 1);
    // A call the parser refuses: rustc runs as asked unseen, so the record goes first.
    let r = w.call(&ws, "a", &["@more-args"]);
    assert_eq!(r.seen.execs.len(), 1);
    assert!(!unit_record(&ws).exists());
    // A reuse writes one too.
    settle();
    assert_eq!(w.call(&ws, "a", &[]).outcome(), REUSED);
    assert!(unit_record(&ws).is_file());
    // An incremental rebuild runs as a child and writes one.
    fs::write(ws.join("a/src/lib.rs"), "mod foo;\nfn edited() {}\n").unwrap();
    settle();
    let r = w.call(&ws, "a", &["-C", "incremental=/tmp/inc"]);
    assert_eq!(
        r.outcome(),
        Outcome::PassedThrough(PassedThrough::Incremental)
    );
    assert_eq!(r.seen.compiles.len(), 1);
    let text = fs::read_to_string(unit_record(&ws)).unwrap();
    // Stats only: the rebuild hashes nothing.
    assert!(text.contains("a/src/lib.rs\",\"stat\":{"), "{text}");
    assert!(text.contains("\"sha256\":null"), "{text}");
    // A crate with nothing local to watch replaces the process and has none.
    let r = w.call_with(&ws, "a", &["-C", "incremental=/tmp/inc"], from_a_registry);
    assert_eq!(r.seen.execs.len(), 1);
    assert!(!unit_record(&ws).exists());
    // So does one compiled whole: what its build script wrote is cargo's to watch.
    fs::write(ws.join("a/src/lib.rs"), "mod foo;\nfn registry() {}\n").unwrap();
    settle();
    let r = w.call_with(&ws, "a", &[], from_a_registry);
    assert_eq!(r.outcome(), STORED);
    assert!(!unit_record(&ws).exists());
    // A failed compile leaves none.
    w.plan(
        "a",
        Plan {
            fail: Some("E0308"),
            ..Plan::default()
        },
    );
    assert_eq!(w.call(&ws, "a", &[]).code, 1);
    assert!(!unit_record(&ws).exists());
    assert_eq!(w.store().out_dirs().len(), 1);
}

// Two machines: other users, other homes (so other cargo and rustup homes), the toolchain
// installed under another name, the checkout at another path, each with its own store.

/// The index folder crates.io's packages are unpacked under in every cargo home.
const INDEX: &str = "registry/src/index.crates.io-1949cf8c6b5b557f";

/// One machine: a [`World`] whose session knows `home`, a toolchain at
/// `<home>/.rustup/toolchains/<toolchain>` with the same bytes on every machine, and a key log.
struct Box2 {
    w: World,
    home: PathBuf,
    toolchain: String,
    key_log: PathBuf,
}

fn a_machine(user: &str, toolchain: &str) -> Box2 {
    let dir = tempfile::tempdir().unwrap();
    let root = PathBuf::from(crate::roots::resolve_on_disk(dir.path().to_str().unwrap()).unwrap());
    let home = root.join("home").join(user);
    let key_log = root.join("keys.jsonl");
    let settings = root.join("build.json");
    settings::update(&settings, |s| {
        s.mode = Mode::On;
        s.min_free_gb = 0;
    })
    .unwrap();
    let exe = root.join("hushrun");
    fs::write(&exe, b"binary").unwrap();
    let env = {
        let home = home.clone();
        let key_log = key_log.clone();
        move |name: &str| match name {
            "HOME" => Some(home.clone().into_os_string()),
            session::KEY_LOG => Some(key_log.clone().into_os_string()),
            _ => None,
        }
    };
    let session = session::begin(&Start {
        settings: &settings,
        cache_dir: Some(&root.join("cache")),
        exe: &exe,
        in_ci: false,
        repository: Some("test/repo"),
        env: &env,
        now: SystemTime::now(),
    })
    .unwrap();
    // The same toolchain, installed under this machine's own name.
    let sysroot = home.join(".rustup/toolchains").join(toolchain);
    let rustc = sysroot.join("bin/rustc");
    for (rest, bytes) in [
        ("bin/rustc", &b"fake rustc"[..]),
        ("lib/librustc_driver-1.dylib", b"driver"),
        ("lib/rustlib/aarch64-apple-darwin/lib/libstd-1.rlib", b"std"),
    ] {
        fs::create_dir_all(sysroot.join(rest).parent().unwrap()).unwrap();
        fs::write(sysroot.join(rest), bytes).unwrap();
    }
    // A registry package unpacked in this machine's cargo home.
    let package = home.join(".cargo").join(INDEX).join("reg-1.0.0");
    fs::create_dir_all(package.join("src")).unwrap();
    fs::write(package.join("src/lib.rs"), "mod util;\n").unwrap();
    fs::write(package.join("src/util.rs"), "pub fn util() {}\n").unwrap();
    fs::write(package.join("data.txt"), "data\n").unwrap();
    Box2 {
        w: World {
            dir,
            session,
            rustc,
            plans: Rc::default(),
        },
        home,
        toolchain: toolchain.to_string(),
        key_log,
    }
}

impl Box2 {
    fn package(&self) -> PathBuf {
        self.home.join(".cargo").join(INDEX).join("reg-1.0.0")
    }

    /// The variables cargo and rustup hand every compile on this machine.
    fn toolchain_vars(&self, fake: &mut Fake) {
        fake.vars.insert(
            "RUSTUP_HOME".into(),
            self.home.join(".rustup").to_str().unwrap().into(),
        );
        fake.vars
            .insert("RUSTUP_TOOLCHAIN".into(), self.toolchain.clone());
    }

    /// The registry crate `reg`, built in the checkout `ws`.
    fn call_reg(&self, ws: &Path) -> Result {
        let source = self.package().join("src/lib.rs");
        let args: Vec<OsString> = self
            .w
            .args(ws, "reg", &[])
            .into_iter()
            .map(|arg| {
                if arg == "reg/src/lib.rs" {
                    source.clone().into_os_string()
                } else {
                    arg
                }
            })
            .collect();
        self.w.call_args(ws, "reg", args, |fake| {
            self.toolchain_vars(fake);
            fake.vars.insert(
                "CARGO_MANIFEST_DIR".into(),
                self.package().to_str().unwrap().into(),
            );
            fake.vars.insert("CARGO_PKG_VERSION".into(), "1.0.0".into());
        })
    }

    /// The workspace crate `a`, which depends on `reg`.
    fn call_a(&self, ws: &Path) -> Result {
        let reg = ws.join("target/debug/deps/libreg-xreg.rlib");
        let extern_reg = format!("reg={}", reg.display());
        self.w
            .call_with(ws, "a", &["--extern", &extern_reg], |fake| {
                self.toolchain_vars(fake)
            })
    }

    /// The key log's lines, by crate name: the last one of each.
    fn keys(&self) -> BTreeMap<String, serde_json::Value> {
        let text = fs::read_to_string(&self.key_log).unwrap_or_default();
        text.lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .map(|line| (line["crate_name"].as_str().unwrap().to_string(), line))
            .collect()
    }
}

/// The library files a crate's compile left in `ws`'s output folder.
fn library_files(ws: &Path, crate_name: &str) -> Vec<(String, Vec<u8>)> {
    let deps = ws.join("target/debug/deps");
    ["rlib", "rmeta"]
        .iter()
        .map(|ext| {
            let name = format!("lib{crate_name}-x{crate_name}.{ext}");
            let bytes = fs::read(deps.join(&name)).unwrap();
            (name, bytes)
        })
        .collect()
}

// covers: B-5, B-6, B-8, B-9, B-17
#[test]
fn a_crate_graph_built_on_two_machines_has_the_same_keys_and_bytes_and_moves_between_them() {
    let a = a_machine("dev", "stable-aarch64-apple-darwin");
    let b = a_machine("someone-else", "1.94.0-aarch64-apple-darwin");
    let plan = |m: &Box2| {
        m.w.plan(
            "reg",
            Plan {
                reads: vec![m.package().join("src/util.rs").to_str().unwrap().into()],
                env_reads: vec!["CARGO_PKG_VERSION".into()],
                ..Plan::default()
            },
        );
        m.w.plan(
            "a",
            Plan {
                reads: vec!["a/src/foo.rs".into()],
                ..Plan::default()
            },
        );
    };
    plan(&a);
    plan(&b);
    let ws_a = a.w.checkout("ws");
    let ws_b = b.w.checkout("a-checkout-at-a-longer-path");
    settle();
    for (m, ws) in [(&a, &ws_a), (&b, &ws_b)] {
        let reg = m.call_reg(ws);
        assert_eq!(reg.outcome(), STORED, "{}", reg.stderr);
        // The cargo home was remapped like the workspace.
        let args = &reg.seen.compiles[0];
        let cargo_home = m.home.join(".cargo");
        assert!(
            args.contains(&format!(
                "--remap-path-prefix={}=/hushrun/cargo",
                cargo_home.display()
            )),
            "{args:?}"
        );
        let a_crate = m.call_a(ws);
        assert_eq!(a_crate.outcome(), STORED, "{}", a_crate.stderr);
        for name in ["reg", "a"] {
            let portable = m.keys()[name]["portable"].clone();
            assert_eq!(portable["portable"], true, "{name}: {portable}");
        }
    }
    // Equal first keys, equal second keys, the same bytes.
    let (keys_a, keys_b) = (a.keys(), b.keys());
    for name in ["reg", "a"] {
        assert_eq!(
            keys_a[name]["keys"]["step"], keys_b[name]["keys"]["step"],
            "{name}: {} / {}",
            keys_a[name]["keys"]["first"], keys_b[name]["keys"]["first"]
        );
        assert_eq!(
            keys_a[name]["keys"]["block"], keys_b[name]["keys"]["block"],
            "{name}: {} / {}",
            keys_a[name]["keys"]["second_parts"], keys_b[name]["keys"]["second_parts"]
        );
        assert!(keys_a[name]["keys"]["step"].is_string());
        assert_eq!(library_files(&ws_a, name), library_files(&ws_b, name));
    }
    let rlib = String::from_utf8(library_files(&ws_a, "reg")[0].1.clone()).unwrap();
    assert!(
        rlib.contains(&format!(
            "source /hushrun/cargo/{INDEX}/reg-1.0.0/src/lib.rs"
        )),
        "{rlib}"
    );
    assert!(!rlib.contains(a.home.to_str().unwrap()), "{rlib}");

    // A third machine with machine A's blocks in its store: both are reused there, and what is
    // put in place names the third machine's own folders.
    let c = a_machine("third", "nightly-aarch64-apple-darwin");
    plan(&c);
    let ws_c = c.w.checkout("elsewhere");
    let store_a = &a.w.session.current().store;
    let store_c = &c.w.session.current().store;
    for part in ["blocks", "steps"] {
        copy_tree(&store_a.join(part), &store_c.join(part));
    }
    settle();
    let reg = c.call_reg(&ws_c);
    assert_eq!(reg.outcome(), REUSED, "{}", reg.stderr);
    assert_eq!(c.call_a(&ws_c).outcome(), REUSED);
    assert_eq!(library_files(&ws_a, "reg"), library_files(&ws_c, "reg"));
    assert_eq!(library_files(&ws_a, "a"), library_files(&ws_c, "a"));
    let dep_info = fs::read_to_string(ws_c.join("target/debug/deps/reg-xreg.d")).unwrap();
    assert!(
        dep_info.contains(c.package().join("src/util.rs").to_str().unwrap()),
        "{dep_info}"
    );
    assert!(!dep_info.contains(a.home.to_str().unwrap()), "{dep_info}");
    assert_eq!(c.keys()["reg"]["portable"]["portable"], true);
}

/// Gives machine `m`'s toolchain the standard library's sources, as rustup's `rust-src` does.
fn with_rust_src(m: &Box2) {
    let sysroot = m.home.join(".rustup/toolchains").join(&m.toolchain);
    for rest in [
        "lib/rustlib/src/rust/library/std/src/lib.rs",
        "lib/rustlib/src/rust/library/core/src/macros/mod.rs",
    ] {
        fs::create_dir_all(sysroot.join(rest).parent().unwrap()).unwrap();
        fs::write(sysroot.join(rest), "// the source").unwrap();
    }
    fs::write(sysroot.join("lib/rustlib/manifest-rust-src"), "file:x\n").unwrap();
}

/// The `--sysroot` a compile was given, if any.
fn sysroot_arg(args: &[String]) -> Option<PathBuf> {
    args.iter()
        .find_map(|a| a.strip_prefix("--sysroot=").map(PathBuf::from))
}

// covers: B-5, B-9, B-17
#[test]
fn a_machine_with_the_standard_librarys_sources_compiles_as_one_without_them() {
    // rustc reads a standard library macro's text from `lib/rustlib/src` when it is there, and
    // a crate that expands `unreachable!("…")` gets another crate hash: on two machines, one
    // with rust-src and one without, one key would name two different libraries. Every compile
    // gets a sysroot without the sources instead, and the keys stay equal.
    let a = a_machine("runner", "stable-aarch64-apple-darwin");
    let b = a_machine("agent", "1.94.0-aarch64-apple-darwin");
    with_rust_src(&b);
    for m in [&a, &b] {
        m.w.plan(
            "reg",
            Plan {
                reads: vec![m.package().join("src/util.rs").to_str().unwrap().into()],
                ..Plan::default()
            },
        );
        m.w.plan("a", Plan::default());
    }
    let ws_a = a.w.checkout("ws");
    let ws_b = b.w.checkout("another/ws");
    settle();
    let (reg_a, reg_b) = (a.call_reg(&ws_a), b.call_reg(&ws_b));
    assert_eq!((reg_a.outcome(), reg_b.outcome()), (STORED, STORED));
    assert_eq!(sysroot_arg(&reg_a.seen.compiles[0]), None, "nothing to hide");
    let view = sysroot_arg(&reg_b.seen.compiles[0]).expect("a sysroot without the sources");
    let toolchain_b = b.home.join(".rustup/toolchains").join(&b.toolchain);
    assert!(toolchain_b.join("lib/rustlib/src").is_dir());
    assert!(!view.join("lib/rustlib/src").exists(), "{}", view.display());
    assert_eq!(
        fs::read(view.join("lib/rustlib/aarch64-apple-darwin/lib/libstd-1.rlib")).unwrap(),
        b"std"
    );
    assert!(!view.starts_with(&toolchain_b));
    // What was asked and what was read are the same on both machines.
    let (a_crate, b_crate) = (a.call_a(&ws_a), b.call_a(&ws_b));
    assert_eq!((a_crate.outcome(), b_crate.outcome()), (STORED, STORED));
    assert!(sysroot_arg(&b_crate.seen.compiles[0]).is_some());
    let (keys_a, keys_b) = (a.keys(), b.keys());
    for name in ["reg", "a"] {
        assert_eq!(keys_a[name]["keys"]["step"], keys_b[name]["keys"]["step"], "{name}");
        assert_eq!(keys_a[name]["keys"]["block"], keys_b[name]["keys"]["block"], "{name}");
        assert_eq!(library_files(&ws_a, name), library_files(&ws_b, name));
    }
}

// covers: B-8, B-9, B-18
#[test]
fn a_proc_macro_linked_through_the_sysroot_without_sources_names_the_toolchain_and_moves() {
    // The link line names the standard library under the sysroot the compile was given. Those
    // paths are the toolchain's: the block is not tied by them, and moves between checkouts.
    let m = a_machine("agent", "1.94.0-aarch64-apple-darwin");
    with_rust_src(&m);
    let line = r#""{ws}/tools/cc" "{ws}/target/debug/deps/x.o" "{sysroot}/lib/rustlib/aarch64-apple-darwin/lib/libstd-1.rlib" "-o" "{ws}/target/debug/deps/liba-xa.dylib""#;
    m.w.plan(
        "a",
        Plan {
            stdout: Some(format!("{line}\n")),
            ..Plan::default()
        },
    );
    let call = |ws: &Path| {
        let mut args = m.w.args(ws, "a", &[]);
        let at = args.iter().position(|a| a == "lib").unwrap();
        args[at] = "proc-macro".into();
        m.w.call_args(ws, "a", args, |fake| m.toolchain_vars(fake))
    };
    let checkout = |name: &str| {
        let ws = m.w.checkout(name);
        fs::create_dir_all(ws.join("tools")).unwrap();
        fs::write(ws.join("tools/cc"), b"driver").unwrap();
        ws
    };
    let (one, two) = (checkout("one"), checkout("two-longer"));
    settle();
    let first = call(&one);
    assert_eq!(first.outcome(), STORED, "{}", first.stderr);
    assert!(sysroot_arg(&first.seen.compiles[0]).is_some());
    let portable = first.entry.unwrap().portable.unwrap();
    assert!(
        !portable.reasons.contains(&crate::portable::Reason::Tied),
        "{portable:?}"
    );
    assert_eq!(call(&two).outcome(), REUSED);
}

/// Copies every file under `from` to the same place under `to`.
fn copy_tree(from: &Path, to: &Path) {
    for file in walk(from) {
        let dest = to.join(file.strip_prefix(from).unwrap());
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(&file, &dest).unwrap();
    }
}

// covers: B-8, B-10
#[test]
fn a_registry_crate_whose_output_holds_the_cargo_home_is_reused_here_but_stays_here() {
    let m = a_machine("dev", "stable-aarch64-apple-darwin");
    m.w.plan(
        "reg",
        Plan {
            embed_var: Some("CARGO_MANIFEST_DIR".into()),
            ..Plan::default()
        },
    );
    let one = m.w.checkout("one");
    let two = m.w.checkout("two");
    settle();
    let first = m.call_reg(&one);
    assert_eq!(first.outcome(), STORED);
    let portable = &first.entry.as_ref().unwrap().portable.as_ref().unwrap();
    assert!(!portable.portable);
    assert!(
        portable
            .reasons
            .contains(&crate::portable::Reason::CargoHomeInOutput)
    );
    assert!(
        portable
            .reasons
            .contains(&crate::portable::Reason::HomeInOutput)
    );
    assert!(!portable.reasons.contains(&crate::portable::Reason::Tied));
    // The cargo home is the same for every checkout here: reused in another, as before.
    let second = m.call_reg(&two);
    assert_eq!(second.outcome(), REUSED);
    assert_eq!(second.entry.unwrap().portable.unwrap(), **portable);
    // A dependent keys it by its bytes: elsewhere they differ, and so does the dependent's key,
    // which is no reason of the dependent's own.
    let dependent = m.call_a(&two);
    assert_eq!(dependent.outcome(), STORED);
    assert!(dependent.entry.unwrap().portable.unwrap().portable);
}

// covers: B-10
#[test]
fn a_variable_naming_the_cargo_home_is_proved_before_a_block_may_leave() {
    for (hash_var, portable) in [(false, true), (true, false)] {
        let m = a_machine("dev", "stable-aarch64-apple-darwin");
        // `include!(concat!(env!("CARGO_MANIFEST_DIR"), "/data.txt"))`: the path is remapped
        // where it lands, unless the macro also hashes it.
        m.w.plan(
            "reg",
            Plan {
                include_var: Some(("CARGO_MANIFEST_DIR".into(), "data.txt".into())),
                hash_var,
                ..Plan::default()
            },
        );
        let one = m.w.checkout("one");
        let two = m.w.checkout("two");
        settle();
        let first = m.call_reg(&one);
        assert_eq!(first.outcome(), STORED, "{}", first.stderr);
        // A second compile with the variable pointing elsewhere decided it.
        assert_eq!(first.seen.compiles.len(), 2);
        let found = first.entry.unwrap().portable.unwrap();
        assert_eq!(found.portable, portable, "{found:?}");
        assert_eq!(
            found
                .reasons
                .contains(&crate::portable::Reason::VariableNamesBase),
            !portable
        );
        // Either way it moves between checkouts here, and the proof is not made again.
        let second = m.call_reg(&two);
        assert_eq!(second.outcome(), REUSED);
        assert_eq!(second.entry.unwrap().portable.unwrap(), found);
    }
}
