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
    /// A variable whose raw value is written into the outputs without the dep-info naming it,
    /// as a proc macro that reads the environment while it expands does.
    leak_var: Option<String>,
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
    /// For a proc macro: what the linker writes around the metadata, such as its own version.
    link_salt: Option<String>,
    /// For a proc macro: something more in its metadata, as a crate hash that moved does.
    metadata_salt: Option<String>,
    /// For a library: its `.rlib` as an `ar` archive with these members (`lib.rmeta` holds
    /// what the compile read), as rustc writes one; a bundled static library's objects are
    /// members of their own.
    rlib_members: Option<Vec<String>>,
}

type During = Box<dyn FnMut()>;

/// What the fake world saw.
#[derive(Default)]
struct Seen {
    compiles: Vec<Vec<String>>,
    execs: Vec<Vec<String>>,
    /// What the wrapper wrote to its stdout.
    stdout: Vec<u8>,
    /// How many times the audit was asked about a downloaded library's first reuse here.
    first_audits: usize,
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
    /// What the compiler says `native` is (B-5), shared between calls to count the asks; none:
    /// it can't be asked.
    native: Option<Rc<RefCell<NativeStandIn>>>,
    /// The run's relay (B-26), shared between calls; none: the real client, which finds no
    /// socket and misses.
    relay: Option<Rc<RefCell<RelayStandIn>>>,
}

type Answering = Box<dyn FnMut(&crate::relay::Request) -> Option<crate::relay::Answer>>;

/// A relay that answers as a test says and keeps what it was asked.
struct RelayStandIn {
    answer: Answering,
    asked: Vec<crate::relay::Request>,
}

fn relay_answering(
    answer: impl FnMut(&crate::relay::Request) -> Option<crate::relay::Answer> + 'static,
) -> Rc<RefCell<RelayStandIn>> {
    Rc::new(RefCell::new(RelayStandIn {
        answer: Box::new(answer),
        asked: Vec::new(),
    }))
}

/// A compiler's answer to what `-C target-cpu=native` is, and how often it was asked.
#[derive(Default)]
struct NativeStandIn {
    /// What `--print target-cpus` and `--print cfg -C target-cpu=native` print; none: they fail.
    answer: Option<(String, String)>,
    asked: usize,
}

/// A stand-in whose `native` is `cpu`, with `features`.
fn native_answer(cpu: &str, features: &[&str]) -> Rc<RefCell<NativeStandIn>> {
    let cfg: String = std::iter::once("target_arch=\"aarch64\"\n".to_string())
        .chain(
            features
                .iter()
                .map(|feature| format!("target_feature=\"{feature}\"\n")),
        )
        .collect();
    Rc::new(RefCell::new(NativeStandIn {
        answer: Some((
            format!(
                "Available CPUs for this target:\n    native - Select the CPU of the current host (currently {cpu}).\n    generic\n"
            ),
            cfg,
        )),
        asked: 0,
    }))
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
        let mut crate_type = String::new();
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
                "--crate-type" => crate_type = value(),
                "-L" | "-l" | "--json" | "--color" | "--target" => {
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
        if let Some(name) = &plan.leak_var {
            let value = self.var(name, env).unwrap_or_default();
            content.push_str(&format!("leaked {value}\n"));
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
                    Some(format!("rmeta\n{content}").into_bytes()),
                ),
                // A proc macro: a linked library, its metadata in a section of its own between
                // what the linker wrote (`metadata`).
                _ if crate_type == "proc-macro" => {
                    let mut inner = b"rust\0\0\0\x0a".to_vec();
                    inner.extend_from_slice(content.as_bytes());
                    inner.extend_from_slice(plan.metadata_salt.as_deref().unwrap_or("").as_bytes());
                    let mut bytes = format!("linked by {:?}\n", plan.link_salt).into_bytes();
                    bytes.extend_from_slice(b"rust\0\0\0\x0a");
                    bytes.extend_from_slice(&(inner.len() as u64).to_le_bytes());
                    bytes.extend_from_slice(&inner);
                    bytes.extend_from_slice(b"\nsymbols");
                    let name = format!("lib{stem}.{}", std::env::consts::DLL_EXTENSION);
                    (out.join(name), Some(bytes))
                }
                _ if plan.rlib_members.is_some() => {
                    let path = out.join(format!("lib{stem}.rlib"));
                    let rlib = format!("rlib\n{content}");
                    // `lib.rmeta` holds the crate's metadata after rustc's header; a member
                    // written `name<path` holds that file's bytes, as a bundled object does.
                    let mut meta = b"rust\0\0\0\x0a".to_vec();
                    let mut inner = b"rust\0\0\0\x0a".to_vec();
                    inner.extend_from_slice(rlib.as_bytes());
                    meta.extend_from_slice(&(inner.len() as u64).to_le_bytes());
                    meta.extend_from_slice(&inner);
                    let members: Vec<(String, Vec<u8>)> = plan
                        .rlib_members
                        .iter()
                        .flatten()
                        .map(|spec| match spec.split_once('<') {
                            Some((name, from)) => (name.to_string(), fs::read(from).unwrap()),
                            None if spec == "lib.rmeta" => (spec.clone(), meta.clone()),
                            None => (spec.clone(), rlib.as_bytes().to_vec()),
                        })
                        .collect();
                    let members: Vec<(&str, &[u8])> = members
                        .iter()
                        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
                        .collect();
                    let temp = out.join(format!(".{stem}.ar"));
                    crate::rlib::write_gnu(&temp, &members).unwrap();
                    let bytes = fs::read(&temp).unwrap();
                    fs::remove_file(&temp).unwrap();
                    (path, Some(bytes))
                }
                _ => (
                    out.join(format!("lib{stem}.rlib")),
                    Some(format!("rlib\n{content}").into_bytes()),
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

    fn rustc_native(&self, _rustc: &Path, _target: Option<&str>) -> io::Result<(String, String)> {
        let Some(native) = &self.native else {
            return Err(io::Error::other("no such print request"));
        };
        let mut native = native.borrow_mut();
        native.asked += 1;
        native
            .answer
            .clone()
            .ok_or_else(|| io::Error::other("no such print request"))
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

    fn audited_first(&self, key: &Digest, started_ms: u64) -> bool {
        self.seen.borrow_mut().first_audits += 1;
        self.audited(key, started_ms)
    }

    fn relay(
        &self,
        socket: &Path,
        request: &crate::relay::Request,
        wait: Duration,
    ) -> Option<crate::relay::Answer> {
        let Some(relay) = &self.relay else {
            return crate::relay::ask(socket, request, wait);
        };
        let mut relay = relay.borrow_mut();
        relay.asked.push(request.clone());
        (relay.answer)(request)
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
        repository_env: &[],
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
    /// Gives the run a relay (B-26): `session.json` names one, run by `org` on a fake host.
    fn share(&mut self, org: &str) {
        let socket = self.dir.path().join("no-relay.sock");
        self.session
            .set_share(session::Share {
                socket,
                run: "1-1".into(),
                by: crate::origin::Identity {
                    host: "https://api.example.com".into(),
                    org: org.into(),
                    credential: crate::origin::fingerprint("hru_test"),
                },
                repository: "test/repo".into(),
            })
            .unwrap();
    }

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
            native: None,
            relay: None,
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
        native: None,
        relay: None,
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

// covers: B-19, B-24
#[test]
fn a_dependent_of_a_crate_compiled_as_asked_stays_on_this_machine() {
    // `a` runs as asked, without the remaps (here: never stored); its bytes hold this machine's
    // folders, so a dependent's key names bytes no other machine makes, and uploading it is
    // waste (the review's B6: 13 such blocks went up).
    let w = world_with(Mode::On, |s| s.never_store = vec!["a".into()]);
    let ws = w.checkout("ws");
    let r = w.call(&ws, "a", &[]);
    assert_eq!(
        r.outcome(),
        Outcome::PassedThrough(PassedThrough::NeverStore)
    );
    fs::create_dir_all(ws.join("b/src")).unwrap();
    fs::write(ws.join("b/src/lib.rs"), "").unwrap();
    settle();
    let rmeta = ws.join("target/debug/deps/liba-xa.rmeta");
    let ext = format!("a={}", rmeta.display());
    let r = w.call(&ws, "b", &["--extern", &ext]);
    assert_eq!(r.outcome(), STORED);
    let portable = r.entry.as_ref().unwrap().portable.clone().unwrap();
    assert!(!portable.portable, "{portable:?}");
    assert_eq!(portable.externs, ["a"]);
    // Once `a` is compiled with the wrapper's changes, its dependents may go again.
    let w2 = world(Mode::On);
    let ws2 = w2.checkout("ws");
    settle();
    assert_eq!(w2.call(&ws2, "a", &[]).outcome(), STORED);
    fs::create_dir_all(ws2.join("b/src")).unwrap();
    fs::write(ws2.join("b/src/lib.rs"), "").unwrap();
    settle();
    let rmeta = ws2.join("target/debug/deps/liba-xa.rmeta");
    let ext = format!("a={}", rmeta.display());
    let r = w2.call(&ws2, "b", &["--extern", &ext]);
    assert_eq!(r.outcome(), STORED);
    assert!(r.entry.unwrap().portable.unwrap().portable);
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
fn a_block_that_cant_be_put_in_place_compiles_with_the_remaps_and_stores_nothing() {
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
    let blocks = w.blocks();
    let r = w.call(&two, "a", &[]);
    assert_eq!(r.outcome(), not_stored(NotStored::Restore));
    // rustc ran once, with the wrapper's changes: as asked, without the remaps, the outputs
    // would hold this checkout's folders and every dependent would get a key no other machine
    // computes (the review's B6).
    assert!(r.seen.execs.is_empty(), "{:?}", r.seen.execs);
    assert_eq!(r.seen.compiles.len(), 1, "{:?}", r.seen.compiles);
    assert!(
        r.seen.compiles[0]
            .iter()
            .any(|a| *a == format!("--remap-path-prefix={}=.", two.display())),
        "{:?}",
        r.seen.compiles[0]
    );
    assert_eq!(w.blocks(), blocks, "nothing stored");
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
        native: None,
        relay: None,
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
            native: None,
            relay: None,
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
        native: None,
        relay: None,
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

    // b's own block can't be put in place: b compiles (with the wrapper's changes, B-19) and
    // fails to load a, which heals a as any compile does.
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
    assert_eq!(r.outcome(), not_stored(NotStored::RustcFailed));
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
            store: None,
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
        native: None,
        relay: None,
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
            native: None,
            relay: None,
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
            native: None,
            relay: None,
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
    let dll = std::env::consts::DLL_EXTENSION;
    fs::remove_file(one.join(format!("target/debug/deps/liba-xa.{dll}"))).unwrap();
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

// covers: B-6
#[test]
fn a_library_that_bundled_nothing_is_not_keyed_by_the_static_libraries_beside_it() {
    // `ring` builds `libring_core.a` with the cargo home's path in its objects, and cargo hands
    // its folder (`-L native=`) to every crate above it. Those that bundle nothing from it
    // (their `.rlib` holds only rustc's own members) don't hold its bytes: another machine's
    // `libring_core.a` must not give them another key. One that bundles it still does.
    for (members, other_bytes_reuse) in [
        (vec!["lib.rmeta", "a-xa.a.1-cgu.0.rcgu.o"], true),
        (
            vec!["lib.rmeta", "a-xa.a.1-cgu.0.rcgu.o", "1f2e-curve25519.o"],
            false,
        ),
        // A bundled object named as rustc names its own (a build script's `inner.rcgu.o`), and
        // a bundled Rust static library's objects (another crate's prefix): bundled, whatever
        // the suffix.
        (
            vec!["lib.rmeta", "a-xa.a.1-cgu.0.rcgu.o", "inner.rcgu.o"],
            false,
        ),
        (
            vec![
                "lib.rmeta",
                "a-xa.a.1-cgu.0.rcgu.o",
                "std-5f49f7b3.std.2c1a-cgu.0.rcgu.o",
            ],
            false,
        ),
    ] {
        let w = world(Mode::On);
        w.plan(
            "a",
            Plan {
                rlib_members: Some(members.iter().map(|m| m.to_string()).collect()),
                ..Plan::default()
            },
        );
        let setup = |name: &str, bytes: &[u8]| {
            let ws = w.checkout(name);
            let dir = ws.join("target/debug/build/ring-1/out");
            fs::create_dir_all(&dir).unwrap();
            crate::rlib::write_gnu(&dir.join("libring_core.a"), &[("curve25519.o", bytes)])
                .unwrap();
            let dir = crate::roots::resolve_on_disk(dir.to_str().unwrap()).unwrap();
            (ws, format!("native={dir}"))
        };
        let run = |(ws, native): &(PathBuf, String)| {
            settle();
            w.call(ws, "a", &["-L", native]).outcome()
        };
        assert_eq!(
            run(&setup("one", b"objects naming /home/runner/.cargo")),
            STORED
        );
        let other = run(&setup("two", b"objects naming /opt/tools/cargo"));
        let expected = if other_bytes_reuse { REUSED } else { STORED };
        assert_eq!(other, expected, "members {members:?}");
        // A static library that appears, or a folder's names that change, still count.
        let ws = setup("three", b"objects naming /home/runner/.cargo");
        fs::write(ws.0.join("target/debug/build/ring-1/out/new.txt"), b"").unwrap();
        assert_eq!(run(&ws), STORED, "the folder's names changed");
    }
}

// covers: B-6
#[test]
fn a_bundled_object_under_rustcs_own_names_is_found_by_its_bytes() {
    // A build script's C object bundled as a second `lib.rmeta`, or named with the dependent's
    // own prefix (which a build script can read from `target/debug/.fingerprint/`): the names
    // look like rustc's, the bytes are the static library's. A changed C file must not reuse
    // the library built from the old one.
    // The third: the archive under a name of its own (`inner.archive`), which a
    // `#[link(..., modifiers = "+verbatim")]` bundles as it is named.
    for (forged, archive) in [
        ("lib.rmeta", "libinner.a"),
        ("a-xa.inner.rcgu.o", "libinner.a"),
        ("a-xa.inner.rcgu.o", "inner.archive"),
    ] {
        let w = world(Mode::On);
        let setup = |name: &str, object: &[u8]| {
            let ws = w.checkout(name);
            let dir = ws.join("target/debug/build/u-1/out");
            fs::create_dir_all(&dir).unwrap();
            crate::rlib::write_gnu(&dir.join(archive), &[("inner.o", object)]).unwrap();
            fs::write(ws.join("inner.o"), object).unwrap();
            let dir = crate::roots::resolve_on_disk(dir.to_str().unwrap()).unwrap();
            (ws, format!("native={dir}"))
        };
        let run = |(ws, native): &(PathBuf, String)| {
            w.plan(
                "a",
                Plan {
                    rlib_members: Some(vec![
                        "lib.rmeta".into(),
                        "a-xa.a.1-cgu.0.rcgu.o".into(),
                        format!("{forged}<{}", ws.join("inner.o").display()),
                    ]),
                    ..Plan::default()
                },
            );
            settle();
            w.call(ws, "a", &["-L", native]).outcome()
        };
        assert_eq!(run(&setup("one", b"inner_value returns 1")), STORED);
        assert_eq!(
            run(&setup("two", b"inner_value returns 2")),
            STORED,
            "{forged} in {archive}: the C changed"
        );
        assert_eq!(
            run(&setup("three", b"inner_value returns 1")),
            REUSED,
            "{forged} in {archive}: the same C"
        );
    }
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
        native: None,
        relay: None,
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
        repository_env: &[],
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
    /// `CARGO_HOME`, or `<home>/.cargo`.
    cargo_home: PathBuf,
    /// `RUSTUP_HOME`, or `<home>/.rustup`.
    rustup_home: PathBuf,
    toolchain: String,
    key_log: PathBuf,
}

fn a_machine(user: &str, toolchain: &str) -> Box2 {
    a_machine_with(user, toolchain, None)
}

/// [`a_machine`], with `CARGO_HOME` and `RUSTUP_HOME` set to these folders under the machine's
/// root, as an image that installs Rust under a system folder does.
fn a_machine_with(user: &str, toolchain: &str, homes: Option<(&str, &str)>) -> Box2 {
    a_machine_on(user, toolchain, homes, None)
}

/// [`a_machine`], another user of a machine whose store is `store` (`HUSHRUN_BUILD_DIR`), as a
/// group-writable store two people share.
fn a_user_of(user: &str, toolchain: &str, store: &Path) -> Box2 {
    a_machine_on(user, toolchain, None, Some(store.to_path_buf()))
}

fn a_machine_on(
    user: &str,
    toolchain: &str,
    homes: Option<(&str, &str)>,
    store: Option<PathBuf>,
) -> Box2 {
    let dir = tempfile::tempdir().unwrap();
    let root = PathBuf::from(crate::roots::resolve_on_disk(dir.path().to_str().unwrap()).unwrap());
    let home = root.join("home").join(user);
    let (cargo_home, rustup_home) = match homes {
        Some((cargo, rustup)) => (root.join(cargo), root.join(rustup)),
        None => (home.join(".cargo"), home.join(".rustup")),
    };
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
        let set = homes.map(|_| (cargo_home.clone(), rustup_home.clone()));
        move |name: &str| match name {
            "HOME" => Some(home.clone().into_os_string()),
            session::KEY_LOG => Some(key_log.clone().into_os_string()),
            "CARGO_HOME" => set.as_ref().map(|s| s.0.clone().into_os_string()),
            "RUSTUP_HOME" => set.as_ref().map(|s| s.1.clone().into_os_string()),
            "HUSHRUN_BUILD_DIR" => store.clone().map(PathBuf::into_os_string),
            _ => None,
        }
    };
    let session = session::begin(&Start {
        settings: &settings,
        cache_dir: Some(&root.join("cache")),
        exe: &exe,
        in_ci: false,
        repository: Some("test/repo"),
        repository_env: &[],
        env: &env,
        now: SystemTime::now(),
    })
    .unwrap();
    // The same toolchain, installed under this machine's own name.
    let sysroot = rustup_home.join("toolchains").join(toolchain);
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
    let package = cargo_home.join(INDEX).join("reg-1.0.0");
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
        cargo_home,
        rustup_home,
        toolchain: toolchain.to_string(),
        key_log,
    }
}

impl Box2 {
    fn package(&self) -> PathBuf {
        self.cargo_home.join(INDEX).join("reg-1.0.0")
    }

    /// The variables cargo and rustup hand every compile on this machine.
    fn toolchain_vars(&self, fake: &mut Fake) {
        fake.vars.insert(
            "RUSTUP_HOME".into(),
            self.rustup_home.to_str().unwrap().into(),
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
        let cargo_home = m.cargo_home.clone();
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
    // Each line says how many compiles the run audited (B-16).
    assert_eq!(keys_a["reg"]["audit"], "one-in");
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
    let sysroot = m.rustup_home.join("toolchains").join(&m.toolchain);
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
    assert_eq!(
        sysroot_arg(&reg_a.seen.compiles[0]),
        None,
        "nothing to hide"
    );
    let view = sysroot_arg(&reg_b.seen.compiles[0]).expect("a sysroot without the sources");
    let toolchain_b = b.rustup_home.join("toolchains").join(&b.toolchain);
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
        assert_eq!(
            keys_a[name]["keys"]["step"], keys_b[name]["keys"]["step"],
            "{name}"
        );
        assert_eq!(
            keys_a[name]["keys"]["block"], keys_b[name]["keys"]["block"],
            "{name}"
        );
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

// covers: B-6, B-15
#[test]
fn a_dependent_of_a_proc_macro_whose_metadata_differs_elsewhere_is_built_not_reused() {
    // Machine B builds the proc macro itself (another linker: another key), and the library
    // that uses it could come from machine A. When B's macro has A's metadata, it does; when the
    // metadata differs (a crate hash that moved, whatever the reason), what A's library was
    // compiled against is not what B has, and reusing it would fail to load (E0463).
    for (metadata_differs, expected) in [(false, REUSED), (true, STORED)] {
        let a = a_machine("runner", "stable-aarch64-apple-darwin");
        let b = a_machine("agent", "1.94.0-aarch64-apple-darwin");
        let line = r#""{ws}/tools/cc" "{ws}/target/debug/deps/x.o" "-o" "{ws}/target/debug/deps/libm-xm.so""#;
        let setup = |m: &Box2, linker: &str, salt: Option<&str>| {
            m.w.plan(
                "m",
                Plan {
                    stdout: Some(format!("{line}\n")),
                    link_salt: Some(linker.into()),
                    metadata_salt: salt.map(str::to_string),
                    ..Plan::default()
                },
            );
            m.w.plan("a", Plan::default());
            let ws = m.w.checkout(&format!("ws-{linker}"));
            fs::create_dir_all(ws.join("tools")).unwrap();
            fs::write(ws.join("tools/cc"), linker).unwrap();
            fs::create_dir_all(ws.join("m/src")).unwrap();
            fs::write(ws.join("m/src/lib.rs"), "// the macro\n").unwrap();
            ws
        };
        let call_m = |m: &Box2, ws: &Path| {
            let mut args = m.w.args(ws, "m", &[]);
            let at = args.iter().position(|a| a == "lib").unwrap();
            args[at] = "proc-macro".into();
            m.w.call_args(ws, "m", args, |fake| m.toolchain_vars(fake))
        };
        let call_a = |m: &Box2, ws: &Path| {
            let so = ws.join(format!(
                "target/debug/deps/libm-xm.{}",
                std::env::consts::DLL_EXTENSION
            ));
            let ext = format!("m={}", so.display());
            m.w.call_with(ws, "a", &["--extern", &ext], |fake| m.toolchain_vars(fake))
        };
        let ws_a = setup(&a, "linker one", None);
        settle();
        assert_eq!(call_m(&a, &ws_a).outcome(), STORED);
        assert_eq!(call_a(&a, &ws_a).outcome(), STORED);
        // The macro's block records the digest of its metadata from the bytes it stored: what
        // a reuse names the file by, with no second read.
        let so = ws_a.join(format!(
            "target/debug/deps/libm-xm.{}",
            std::env::consts::DLL_EXTENSION
        ));
        let recorded = crate::metadata::of(&so).unwrap().to_string();
        let store_blocks = walk(&a.w.session.current().store.join("blocks"));
        assert!(
            store_blocks
                .iter()
                .any(|f| f.ends_with("block.json")
                    && fs::read_to_string(f).unwrap().contains(&recorded)),
            "no block records {recorded}"
        );
        // A's blocks in B's store.
        let (store_a, store_b) = (&a.w.session.current().store, &b.w.session.current().store);
        for part in ["blocks", "steps"] {
            copy_tree(&store_a.join(part), &store_b.join(part));
        }
        let ws_b = setup(&b, "linker two", metadata_differs.then_some("moved"));
        settle();
        let macro_b = call_m(&b, &ws_b);
        assert_eq!(macro_b.outcome(), STORED, "another linker, another key");
        let dependent = call_a(&b, &ws_b);
        assert_eq!(
            dependent.outcome(),
            expected,
            "metadata differs: {metadata_differs}"
        );
        assert_eq!(dependent.entry.unwrap().found, !metadata_differs);
        // The name is the same on both machines exactly when the metadata is.
        let named = |m: &Box2| m.keys()["a"]["keys"]["externs"][0]["by"].clone();
        assert_eq!(named(&a) == named(&b), !metadata_differs);
        assert!(named(&b)["produced-by"].is_string(), "{}", named(&b));
    }
}

// covers: B-6, B-8
#[test]
fn a_dotenv_search_above_a_cargo_home_in_a_shared_folder_is_keyed_as_above_a_home() {
    // One machine keeps the cargo home in its home folder, the other under a shared folder (the
    // rustup home here; `/opt/tools/cargo` or the official image's `/usr/local/cargo` for
    // real). A macro that reads the first `.env` walking up from a registry package must give
    // the same key on both: the shared folders above the cargo home are that machine's layout,
    // not places the block was read from.
    let a = a_machine("runner", "stable-aarch64-apple-darwin");
    let b = a_machine_with(
        "agent",
        "1.94.0-aarch64-apple-darwin",
        Some(("opt/tools/cargo", "opt/tools")),
    );
    assert!(b.cargo_home.starts_with(&b.rustup_home));
    let call = |m: &Box2, ws: &Path| {
        let sqlx = ws.join("target/debug/deps/libsqlx-s.rlib");
        fs::write(&sqlx, "the same sqlx").unwrap();
        let source = m.package().join("src/lib.rs");
        let ext = format!("sqlx={}", sqlx.display());
        let args: Vec<OsString> =
            m.w.args(ws, "reg", &["--extern", &ext])
                .into_iter()
                .map(|arg| {
                    if arg == "reg/src/lib.rs" {
                        source.clone().into_os_string()
                    } else {
                        arg
                    }
                })
                .collect();
        m.w.call_args(ws, "reg", args, |fake| {
            m.toolchain_vars(fake);
            fake.vars.insert(
                "CARGO_MANIFEST_DIR".into(),
                m.package().to_str().unwrap().into(),
            );
            fake.vars.insert("CARGO_PKG_VERSION".into(), "1.0.0".into());
        })
    };
    for m in [&a, &b] {
        m.w.plan("reg", Plan::default());
    }
    let (ws_a, ws_b) = (a.w.checkout("ws"), b.w.checkout("workspace/project"));
    settle();
    for (m, ws) in [(&a, &ws_a), (&b, &ws_b)] {
        let r = call(m, ws);
        assert_eq!(r.outcome(), STORED, "{}", r.stderr);
        let portable = r.entry.unwrap().portable.unwrap();
        assert!(portable.portable, "{portable:?}");
    }
    let (keys_a, keys_b) = (a.keys(), b.keys());
    assert_eq!(keys_a["reg"]["keys"]["step"], keys_b["reg"]["keys"]["step"]);
    assert_eq!(
        keys_a["reg"]["keys"]["block"], keys_b["reg"]["keys"]["block"],
        "{}\n{}",
        keys_a["reg"]["keys"]["second_parts"], keys_b["reg"]["keys"]["second_parts"]
    );
    // A `.env` in the shared folder above B's cargo home is what the search finds there: another
    // key, by its bytes.
    fs::write(b.rustup_home.join(".env"), "DATABASE_URL=postgres://x\n").unwrap();
    settle();
    let r = call(&b, &ws_b);
    assert_eq!(r.outcome(), STORED);
    assert!(!r.entry.unwrap().found);
}

// covers: B-9, B-11, B-18
#[test]
fn a_link_through_the_sysroot_without_sources_is_judged_by_the_toolchains_files() {
    // The view's links are new whenever a run made them, possibly in the very tick a linked
    // compile starts (build scripts start together on a cold build). What a link through them
    // reads is the toolchain's file, which the compiler's identity holds by content: links made
    // during the compile don't make it "changed while compiling".
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
    let ws = m.w.checkout("ws");
    fs::create_dir_all(ws.join("tools")).unwrap();
    fs::write(ws.join("tools/cc"), b"driver").unwrap();
    settle();
    let sysroots = m.w.session.current().store.join("compilers/sysroots");
    let mut args = m.w.args(&ws, "a", &[]);
    let at = args.iter().position(|a| a == "lib").unwrap();
    args[at] = "proc-macro".into();
    let r = m.w.call_args(&ws, "a", args, |fake| {
        m.toolchain_vars(fake);
        // While it compiles, the view's links are made again (the same targets).
        fake.during = Some(Box::new(move || {
            for view in fs::read_dir(&sysroots).unwrap().flatten() {
                let rustlib = view.path().join("lib/rustlib");
                let Ok(entries) = fs::read_dir(&rustlib) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let link = entry.path();
                    let target = fs::read_link(&link).unwrap();
                    fs::remove_file(&link).unwrap();
                    std::os::unix::fs::symlink(&target, &link).unwrap();
                }
            }
        }));
    });
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    assert!(sysroot_arg(&r.seen.compiles[0]).is_some());
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

// covers: B-10, B-24
#[test]
fn a_block_that_holds_one_users_cargo_home_is_not_another_users_on_a_shared_store() {
    // Two people on one machine share a store; each has a cargo home of their own, and a
    // library writes the cargo home into its bytes (`env!("CARGO_HOME")`, kept for run time).
    let one = a_machine("dev", "stable-aarch64-apple-darwin");
    let store = one.w.session.current().store.clone();
    let two = a_user_of("dev2", "stable-aarch64-apple-darwin", &store);
    assert_eq!(two.w.session.current().store, store);
    let call = |m: &Box2, ws: &Path| {
        m.w.plan(
            "a",
            Plan {
                embed_var: Some("CARGO_HOME".into()),
                ..Plan::default()
            },
        );
        m.w.call_with(ws, "a", &[], |fake| {
            m.toolchain_vars(fake);
            fake.vars
                .insert("CARGO_HOME".into(), m.cargo_home.to_str().unwrap().into());
        })
    };
    let embedded = |ws: &Path| {
        let rlib = fs::read_to_string(ws.join("target/debug/deps/liba-xa.rlib")).unwrap();
        rlib.lines()
            .find_map(|line| line.strip_prefix("embedded ").map(str::to_string))
            .unwrap()
    };
    let ws1 = one.w.checkout("ws");
    let ws2 = two.w.checkout("ws");
    settle();
    let first = call(&one, &ws1);
    assert_eq!(first.outcome(), STORED, "{}", first.stderr);
    assert!(
        !first
            .entry
            .as_ref()
            .unwrap()
            .portable
            .as_ref()
            .unwrap()
            .portable,
        "the block holds the cargo home"
    );
    // The other user's compile is not the first user's block: it holds their own cargo home.
    let second = call(&two, &ws2);
    assert_eq!(second.outcome(), STORED, "{}", second.stderr);
    assert_eq!(embedded(&ws2), two.cargo_home.to_str().unwrap());
    // And each finds their own block from then on.
    for ws in [&ws1, &ws2] {
        fs::remove_dir_all(ws.join("target")).unwrap();
        fs::create_dir_all(ws.join("target/debug/deps")).unwrap();
    }
    assert_eq!(call(&one, &ws1).outcome(), REUSED);
    assert_eq!(embedded(&ws1), one.cargo_home.to_str().unwrap());
    assert_eq!(call(&two, &ws2).outcome(), REUSED);
    assert_eq!(embedded(&ws2), two.cargo_home.to_str().unwrap());
}

/// One compile of library `a` whose output holds `name`'s value, `value`, without the dep-info
/// naming it (`leak`), or named by it (`env!`, tracked).
fn with_value_in_output(name: &str, value: &str, tracked: bool) -> (World, Result) {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    let plan = if tracked {
        Plan {
            env_reads: vec![name.into()],
            ..Plan::default()
        }
    } else {
        Plan {
            leak_var: Some(name.into()),
            ..Plan::default()
        }
    };
    w.plan("a", plan);
    settle();
    let r = w.call_with(&ws, "a", &[], |fake| {
        from_a_registry(fake);
        fake.vars.insert(name.into(), value.into());
        // A GitHub Actions job's, and a GitLab job's, among others.
        for (name, value) in [
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITLAB_CI", "true"),
            ("TF_BUILD", "True"),
        ] {
            fake.vars.entry(name.into()).or_insert_with(|| value.into());
        }
    });
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    (w, r)
}

// covers: B-24
#[test]
fn a_projects_own_value_under_a_ci_prefix_holds_its_block_back_in_ci() {
    // Fourteen bytes of a project's own setting, in a job of each system whose prefix it has.
    for name in [
        "BUILD_ENV",
        "CI_REGION",
        "AGENT_POOL",
        "GITHUB_DEPLOY_TARGET",
    ] {
        let (_, r) = with_value_in_output(name, "prod-eu-west-1", false);
        let portable = r.entry.unwrap().portable.unwrap();
        assert!(!portable.portable, "{name}: {portable:?}");
        assert_eq!(portable.variables, [name], "{portable:?}");
    }
    // A value of the shell's own never does.
    let (_, r) = with_value_in_output("OLDPWD", "/srv/deploy/prod-eu-west-1", false);
    assert!(r.entry.unwrap().portable.unwrap().portable);
}

// covers: B-24
#[test]
fn an_output_holding_an_unkeyed_secret_stays_here_and_only_its_name_is_written() {
    let value = "s3cr3t-value-1234";
    let (w, r) = with_value_in_output("FOO_TOKEN", value, false);
    let portable = r.entry.as_ref().unwrap().portable.clone().unwrap();
    assert!(!portable.portable, "{portable:?}");
    assert!(
        portable
            .reasons
            .contains(&crate::portable::Reason::UnkeyedValue)
    );
    assert_eq!(portable.variables, ["FOO_TOKEN"]);
    // The output holds the value, as the compile wrote it; nothing hushrun writes does.
    let mut written = fs::read_to_string(w.session.current().folder.join("log.jsonl")).unwrap();
    for (_, dir) in w.store().stored() {
        written.push_str(&fs::read_to_string(dir.join("block.json")).unwrap());
    }
    assert!(written.contains("FOO_TOKEN"));
    assert!(!written.contains(value));
    let counts = session::Counts::of(Mode::On, 0, &w.session.current().entries());
    assert_eq!(counts.unkeyed, 1);
    assert_eq!(counts.unkeyed_variables, ["FOO_TOKEN"]);
}

// covers: B-24
#[test]
fn a_long_value_no_ci_set_holds_its_block_back_and_ci_values_and_keyed_ones_dont() {
    let (_, r) = with_value_in_output("LICENSE_SERVER", "licence.example.internal", false);
    let portable = r.entry.unwrap().portable.unwrap();
    assert_eq!(portable.variables, ["LICENSE_SERVER"], "{portable:?}");
    // GitHub's own variable, in every step of a job, and in the doc comments of half the
    // libraries: the block may leave.
    let (_, r) = with_value_in_output("GITHUB_SERVER_URL", "https://github.com", false);
    let portable = r.entry.unwrap().portable.unwrap();
    assert!(portable.portable, "{portable:?}");
    // A secret's value the dep-info names is in the second key: another value is another block.
    let (_, r) = with_value_in_output("FOO_TOKEN", "s3cr3t-value-1234", true);
    let portable = r.entry.unwrap().portable.unwrap();
    assert!(portable.portable, "{portable:?}");
    // Too short to search for.
    let (_, r) = with_value_in_output("FOO_TOKEN", "short", false);
    assert!(r.entry.unwrap().portable.unwrap().portable);
}

// covers: B-35, B-5
#[test]
fn a_variable_the_repository_names_in_build_env_is_in_every_key() {
    let mut w = world(Mode::On);
    let settings = w.dir.path().join("build.json");
    let exe = w.dir.path().join("hushrun");
    let cache = w.dir.path().join("cache");
    let named = ["APP_FLAVOR".to_string()];
    w.session = session::begin(&Start {
        settings: &settings,
        cache_dir: Some(&cache),
        exe: &exe,
        in_ci: false,
        repository: Some("test/repo"),
        repository_env: &named,
        env: &|_| None,
        now: SystemTime::now(),
    })
    .unwrap();
    // A build that reads it without telling the compiler: the dep-info doesn't name it.
    w.plan(
        "a",
        Plan {
            leak_var: Some("APP_FLAVOR".into()),
            ..Plan::default()
        },
    );
    let ws = w.checkout("ws");
    settle();
    let with = |value: &'static str| {
        move |fake: &mut Fake| {
            from_a_registry(fake);
            fake.vars.insert("APP_FLAVOR".into(), value.into());
        }
    };
    let first = w.call_with(&ws, "a", &[], with("one-flavour-of-it"));
    assert_eq!(first.outcome(), STORED, "{}", first.stderr);
    // Keyed, its value in the output keeps nothing here.
    assert!(first.entry.unwrap().portable.unwrap().portable);
    let other = w.call_with(&ws, "a", &[], with("another-flavour-of-it"));
    assert_eq!(other.outcome(), STORED, "{}", other.stderr);
    assert!(!other.entry.unwrap().found);
    let again = w.call_with(&ws, "a", &[], with("one-flavour-of-it"));
    assert_eq!(again.outcome(), REUSED, "{}", again.stderr);
}

/// Library `a` compiled for `-C target-cpu=native` in a fresh world, with `native` as the
/// compiler's answer.
fn native_compile(native: &Rc<RefCell<NativeStandIn>>) -> (World, Result) {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    settle();
    let native = native.clone();
    let r = w.call_with(&ws, "a", &["-C", "target-cpu=native"], move |fake| {
        from_a_registry(fake);
        fake.native = Some(native);
    });
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    (w, r)
}

// covers: B-5, B-24
#[test]
fn target_cpu_native_is_keyed_by_what_it_means_on_the_machine() {
    let (m4, _) = native_compile(&native_answer("apple-m4", &["neon", "sme"]));
    let (m4_again, r) = native_compile(&native_answer("apple-m4", &["neon", "sme"]));
    // Answered or not, it never leaves the machine: the printed features are not all of them.
    let portable = r.entry.unwrap().portable.unwrap();
    assert!(!portable.portable);
    assert!(
        portable
            .reasons
            .contains(&crate::portable::Reason::NativeCpu)
    );
    let (m1, _) = native_compile(&native_answer("apple-m1", &["neon"]));
    let (same_name, _) = native_compile(&native_answer("apple-m4", &["neon"]));
    // One CPU gives one local key; another CPU, or the same name with other features (a store
    // carried to another machine), another: the flag's text alone would give one.
    assert_eq!(only_step(&m4), only_step(&m4_again));
    assert_ne!(only_step(&m4), only_step(&m1));
    assert_ne!(only_step(&m4), only_step(&same_name));
}

// covers: B-5
#[test]
fn what_native_means_is_asked_once_per_compiler_not_once_per_compile() {
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    for name in ["b", "c"] {
        fs::create_dir_all(ws.join(format!("{name}/src"))).unwrap();
        fs::write(ws.join(format!("{name}/src/lib.rs")), "pub fn f() {}\n").unwrap();
    }
    settle();
    let native = native_answer("apple-m4", &["neon"]);
    for name in ["a", "b", "c", "a"] {
        let native = native.clone();
        let r = w.call_with(&ws, name, &["-C", "target-cpu=native"], move |fake| {
            from_a_registry(fake);
            fake.native = Some(native);
        });
        assert!(matches!(r.outcome(), STORED | REUSED), "{}", r.stderr);
    }
    assert_eq!(native.borrow().asked, 1);
    // A call without the flag never asks.
    let r = w.call_with(&ws, "b", &[], from_a_registry);
    assert_eq!(r.outcome(), STORED);
    assert_eq!(native.borrow().asked, 1);
}

// covers: B-5, B-24
#[test]
fn a_compiler_that_cant_say_what_native_is_keeps_its_block_here_keyed_by_the_text() {
    let unanswered = Rc::new(RefCell::new(NativeStandIn::default()));
    let (w, r) = native_compile(&unanswered);
    let portable = r.entry.unwrap().portable.unwrap();
    assert!(!portable.portable);
    assert!(
        portable
            .reasons
            .contains(&crate::portable::Reason::NativeCpu)
    );
    assert_eq!(unanswered.borrow().asked, 1);
    // Keyed by the text: the same in every such world, and other than any answer's key.
    let (again, _) = native_compile(&Rc::new(RefCell::new(NativeStandIn::default())));
    assert_eq!(only_step(&w), only_step(&again));
    let (answered, _) = native_compile(&native_answer("apple-m4", &["neon"]));
    assert_ne!(only_step(&w), only_step(&answered));
    // A later `-C target-cpu` wins, as in rustc: nothing is asked.
    let unasked = Rc::new(RefCell::new(NativeStandIn::default()));
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    settle();
    let stand_in = unasked.clone();
    let r = w.call_with(
        &ws,
        "a",
        &["-C", "target-cpu=native", "-C", "target-cpu=apple-m1"],
        move |fake| {
            from_a_registry(fake);
            fake.native = Some(stand_in);
        },
    );
    assert!(r.entry.unwrap().portable.unwrap().portable);
    assert_eq!(unasked.borrow().asked, 0);
}

// --- sharing through a relay (B-26, B-30, B-32, B-34) -------------------------------------

use crate::origin::{Origin, Vouch};
use crate::relay::{Answer, Problem, Request};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn vouch(date_ms: u64) -> Vouch {
    Vouch {
        host: "https://api.example.com".into(),
        org: "acme".into(),
        repository: "test/repo".into(),
        scope: "person/2".into(),
        outputs: hushrun_core::blocks::Hex32([0; 32]),
        date_ms,
        word: crate::origin::Word::Served,
    }
}

/// Copies the block under `key` from `from` into `to` as a download vouched for at `date_ms`.
fn download(from: &Store, to: &Store, key: &Digest, date_ms: u64) -> bool {
    let Ok(Some(block)) = from.block(key) else {
        return false;
    };
    let files: Vec<crate::store::NewFile> = block
        .files
        .iter()
        .map(|f| crate::store::NewFile {
            name: f.name.clone(),
            source: f.path.clone(),
        })
        .collect();
    to.publish_with_origin(
        key,
        &block.description,
        &files,
        &Origin::downloaded(vouch(date_ms)),
        SystemTime::now(),
    )
    .is_ok()
}

/// A relay that serves what `from` holds, publishing into `to`, and answers `vouch` to a
/// vouch request.
fn serving(from: Store, to: Store, vouched: Option<Answer>) -> Rc<RefCell<RelayStandIn>> {
    relay_answering(move |request| match request {
        Request::Hints { step } => {
            let hints = from.hints(step);
            for hint in &hints {
                to.add_shared_hint(step, hint, SystemTime::now()).unwrap();
            }
            Some(Answer::Hints {
                count: hints.len() as u32,
            })
        }
        Request::Fetch { keys, .. } => Some(
            keys.iter()
                .find(|key| download(&from, &to, key, now_ms()))
                .map_or(Answer::None, |key| Answer::Ready { key: *key }),
        ),
        Request::Vouch { .. } => vouched.clone(),
        _ => Some(Answer::Ok),
    })
}

/// Another machine's store, with library `a` built there.
fn built_elsewhere() -> (World, Digest) {
    let other = world(Mode::On);
    let ws = other.checkout("ws");
    settle();
    let r = other.call_with(&ws, "a", &[], from_a_registry);
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    let key = r.entry.unwrap().key.unwrap();
    (other, key)
}

// covers: B-26, B-30, B-32, B-34
#[test]
fn a_block_the_relay_fetched_is_reused_recorded_as_downloaded_and_audited_one_in_ten_at_first() {
    let (other, key) = built_elsewhere();
    let mut w = world(Mode::On);
    w.share("acme");
    let ws = w.checkout("ws");
    settle();
    let relay = serving(other.store(), w.store(), None);
    let stand_in = relay.clone();
    let r = w.call_with(&ws, "a", &[], move |fake| {
        from_a_registry(fake);
        fake.relay = Some(stand_in);
    });
    assert_eq!(r.outcome(), REUSED, "{}", r.stderr);
    let entry = r.entry.unwrap();
    assert!(entry.downloaded);
    assert_eq!(entry.key, Some(key));
    // The first reuse of a downloaded library asks the one-in-ten audit, and is noted.
    assert_eq!(r.seen.first_audits, 1);
    let origin = w.store().origin(&key).unwrap();
    assert!(origin.downloaded_only() && origin.reused);
    assert!(matches!(
        relay.borrow().asked[..],
        [Request::Hints { .. }, Request::Fetch { .. }]
    ));
    // A second reuse is audited as any block is, and asks the relay nothing: the entry is fresh.
    fs::remove_file(w.rlib(&ws, "a")).unwrap();
    let stand_in = relay.clone();
    let r = w.call_with(&ws, "a", &[], move |fake| {
        from_a_registry(fake);
        fake.relay = Some(stand_in);
    });
    assert_eq!(r.outcome(), REUSED, "{}", r.stderr);
    assert!(!r.entry.unwrap().downloaded);
    assert_eq!(r.seen.first_audits, 0);
    assert_eq!(relay.borrow().asked.len(), 2);
}

// covers: B-34
#[test]
fn a_downloaded_block_is_reused_without_asking_for_a_day_then_only_while_a_server_vouches() {
    let (other, key) = built_elsewhere();
    let mut w = world(Mode::On);
    w.share("acme");
    let ws = w.checkout("ws");
    settle();
    let run = |answer: Option<Answer>| {
        let relay = serving(other.store(), w.store(), answer);
        let stand_in = relay.clone();
        let _ = fs::remove_file(w.rlib(&ws, "a"));
        let r = w.call_with(&ws, "a", &[], move |fake| {
            from_a_registry(fake);
            fake.relay = Some(stand_in);
        });
        let asked_vouch = relay
            .borrow()
            .asked
            .iter()
            .any(|r| matches!(r, Request::Vouch { .. }));
        (r, asked_vouch)
    };
    let set_date = |date_ms: u64| {
        w.store()
            .update_origin(&key, |o| {
                let mut o = o?;
                o.vouched = vec![vouch(date_ms)];
                Some(o)
            })
            .unwrap();
    };
    // Fetched in this run: reused.
    assert_eq!(run(None).0.outcome(), REUSED);
    // Under a day old: reused, nothing asked.
    set_date(now_ms() - DAY_MS / 2);
    let (r, asked) = run(None);
    assert_eq!(r.outcome(), REUSED);
    assert!(!asked);
    // A day old: asked. Served, or no server reachable in this run: reused.
    set_date(now_ms() - DAY_MS - 1);
    let (r, asked) = run(Some(Answer::Served));
    assert_eq!((r.outcome(), asked), (REUSED, true));
    let (r, _) = run(Some(Answer::Offline));
    assert_eq!(r.outcome(), REUSED);
    // Anything else: a miss. rustc runs, and its block takes the downloaded one's place.
    let (r, asked) = run(Some(Answer::None));
    assert!(asked);
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    let origin = w.store().origin(&key).unwrap();
    assert!(origin.built.is_some() && origin.vouched.is_empty());
    // A block built here is never asked about.
    let (r, asked) = run(Some(Answer::None));
    assert_eq!((r.outcome(), asked), (REUSED, false));
}

// covers: B-34
#[test]
fn a_block_a_server_said_no_to_is_never_reused_as_a_download_whatever_the_network_does() {
    let (other, key) = built_elsewhere();
    let withdrawn = |date_ms: u64| Vouch {
        word: crate::origin::Word::Withdrawn,
        ..vouch(date_ms)
    };
    // The run's server is down (the relay answers that none can be reached), and the entry is
    // fresh: the "no" a server said once still holds, and nothing is asked.
    let mut w = world(Mode::On);
    w.share("acme");
    let ws = w.checkout("ws");
    settle();
    assert!(download(&other.store(), &w.store(), &key, now_ms()));
    w.store()
        .update_origin(&key, |o| {
            let mut o = o?;
            o.renew(withdrawn(now_ms()));
            Some(o)
        })
        .unwrap();
    let relay = serving(other.store(), w.store(), Some(Answer::Offline));
    let stand_in = relay.clone();
    let r = w.call_with(&ws, "a", &[], move |fake| {
        from_a_registry(fake);
        fake.relay = Some(stand_in);
    });
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    assert!(
        !relay
            .borrow()
            .asked
            .iter()
            .any(|r| matches!(r, Request::Vouch { .. }))
    );
    // A run with no relay at all, which asks nothing, doesn't reuse it either.
    let w = world(Mode::On);
    let ws = w.checkout("ws");
    settle();
    assert!(download(&other.store(), &w.store(), &key, now_ms()));
    w.store()
        .update_origin(&key, |o| {
            let mut o = o?;
            o.vouched = vec![withdrawn(now_ms())];
            Some(o)
        })
        .unwrap();
    let r = w.call_with(&ws, "a", &[], from_a_registry);
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
}

// covers: B-26, B-29
#[test]
fn a_compile_tells_the_relay_what_it_built_and_with_no_relay_it_is_a_plain_compile() {
    let mut w = world(Mode::On);
    w.share("acme");
    let ws = w.checkout("ws");
    settle();
    // No socket and no stand-in: every request misses, and the compile goes on as without one.
    let r = w.call_with(&ws, "a", &[], from_a_registry);
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    let key = r.entry.as_ref().unwrap().key.unwrap();
    let origin = w.store().origin(&key).unwrap();
    let built = origin.built.unwrap();
    assert_eq!(built.run, "1-1");
    assert_eq!(built.by.unwrap().org, "acme");
    // With a relay: it hears of the block, portable and with its cost.
    let w = {
        let mut w = world(Mode::On);
        w.share("acme");
        w
    };
    let ws = w.checkout("ws");
    settle();
    let relay = relay_answering(|_| Some(Answer::None));
    let stand_in = relay.clone();
    let r = w.call_with(&ws, "a", &[], move |fake| {
        from_a_registry(fake);
        fake.relay = Some(stand_in);
    });
    assert_eq!(r.outcome(), STORED);
    let asked = relay.borrow().asked.clone();
    let built: Vec<&Request> = asked
        .iter()
        .filter(|r| matches!(r, Request::Built { .. }))
        .collect();
    assert!(
        matches!(built[..], [Request::Built { key: k, portable: true, class: crate::model::Class::Library, .. }] if *k == key),
        "{asked:?}"
    );
    // A run without a relay records a build with no backend.
    let plain = world(Mode::On);
    let ws = plain.checkout("ws");
    settle();
    let r = plain.call_with(&ws, "a", &[], from_a_registry);
    let origin = plain
        .store()
        .origin(&r.entry.unwrap().key.unwrap())
        .unwrap();
    assert!(origin.built.unwrap().by.is_none());
}

// covers: B-32
#[test]
fn a_compile_whose_publish_finds_a_downloaded_block_with_other_bytes_takes_its_place() {
    let mut w = world(Mode::On);
    w.share("acme");
    let ws = w.checkout("ws");
    settle();
    let r = w.call_with(&ws, "a", &[], from_a_registry);
    let entry = r.entry.unwrap();
    let (key, step) = (entry.key.unwrap(), entry.step.unwrap());
    let block = w.store().block(&key).unwrap().unwrap();
    let ours = fs::read(&block.files[0].path).unwrap();
    // Gone from the store; the hint stays, so the next compile asks the relay for that key.
    w.store().discard(&key).unwrap();
    fs::remove_file(w.rlib(&ws, "a")).unwrap();
    let elsewhere = w.dir.path().join("elsewhere.rlib");
    fs::write(&elsewhere, b"other bytes").unwrap();
    let store = w.store();
    let description = block.description.clone();
    let name = block.files[0].name.clone();
    let relay = relay_answering(|_| Some(Answer::None));
    let stand_in = relay.clone();
    let r = w.call_with(&ws, "a", &[], move |fake| {
        from_a_registry(fake);
        fake.relay = Some(stand_in);
        // While it compiles, a download of the same key with other bytes lands.
        fake.during = Some(Box::new(move || {
            let files = [crate::store::NewFile {
                name: name.clone(),
                source: elsewhere.clone(),
            }];
            store
                .publish_with_origin(
                    &key,
                    &description,
                    &files,
                    &Origin::downloaded(vouch(now_ms())),
                    SystemTime::now(),
                )
                .unwrap();
        }));
    });
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    let block = w.store().block(&key).unwrap().unwrap();
    assert_eq!(fs::read(&block.files[0].path).unwrap(), ours);
    assert!(w.store().origin(&key).unwrap().built.is_some());
    assert!(
        relay.borrow().asked.iter().any(
            |r| matches!(r, Request::Report { key: k, problem: Problem::Differs } if *k == key)
        ),
        "{:?}",
        relay.borrow().asked
    );
    // C2, B-34: the difference suspends the first key for the kept days, as an audit's does.
    let now = SystemTime::now();
    assert!(w.store().suspended_at(&step, now).is_some());
    assert!(
        w.store()
            .suspended_at(&step, now + Duration::from_secs(16 * 86_400))
            .is_none()
    );
}

// covers: B-16, B-34
#[test]
fn an_audit_of_a_downloaded_block_that_differs_reports_it_and_suspends_for_the_kept_days() {
    let (other, key) = built_elsewhere();
    // The other machine's block, with other bytes than a compile here makes.
    let mut w = world(Mode::On);
    w.share("acme");
    let ws = w.checkout("ws");
    settle();
    let block = other.store().block(&key).unwrap().unwrap();
    let wrong = w.dir.path().join("wrong.rlib");
    fs::write(&wrong, b"not what rustc makes").unwrap();
    let mut files: Vec<crate::store::NewFile> = block
        .files
        .iter()
        .map(|f| crate::store::NewFile {
            name: f.name.clone(),
            source: f.path.clone(),
        })
        .collect();
    let rlib = files
        .iter_mut()
        .find(|f| f.name.ends_with(".rlib"))
        .unwrap();
    rlib.source = wrong;
    w.store()
        .publish_with_origin(
            &key,
            &block.description,
            &files,
            &Origin::downloaded(vouch(now_ms())),
            SystemTime::now(),
        )
        .unwrap();
    for hint in other.store().hints(&only_step(&other)) {
        w.store()
            .add_shared_hint(&only_step(&other), &hint, SystemTime::now())
            .unwrap();
    }
    let relay = relay_answering(|_| Some(Answer::Ok));
    let stand_in = relay.clone();
    let r = w.call_with(&ws, "a", &[], move |fake| {
        from_a_registry(fake);
        fake.relay = Some(stand_in);
        fake.audit = true;
    });
    assert_eq!(r.outcome(), not_stored(NotStored::Differed), "{}", r.stderr);
    assert_eq!(r.seen.first_audits, 1);
    assert!(
        relay.borrow().asked.iter().any(
            |r| matches!(r, Request::Report { key: k, problem: Problem::Differs } if *k == key)
        )
    );
    let step = only_step(&other);
    let now = SystemTime::now();
    assert!(w.store().suspended_at(&step, now).is_some());
    assert!(
        w.store()
            .suspended_at(&step, now + Duration::from_secs(16 * 86_400))
            .is_none(),
        "a downloaded block's suspension lasts the kept days"
    );
}

// covers: B-15, B-34
#[test]
fn a_dependent_that_cant_load_a_downloaded_block_reports_it_rejected() {
    let (other, key) = built_elsewhere();
    let mut w = world(Mode::On);
    w.share("acme");
    let ws = w.checkout("ws");
    settle();
    let relay = serving(other.store(), w.store(), None);
    let stand_in = relay.clone();
    let r = w.call_with(&ws, "a", &[], move |fake| {
        from_a_registry(fake);
        fake.relay = Some(stand_in);
    });
    assert_eq!(r.outcome(), REUSED, "{}", r.stderr);
    let step = only_step(&other);
    w.plan(
        "b",
        Plan {
            fail: Some("E0463"),
            fail_message: Some("can't find crate for `core` which `a` depends on".into()),
            ..Plan::default()
        },
    );
    fs::create_dir_all(ws.join("b/src")).unwrap();
    fs::write(ws.join("b/src/lib.rs"), "").unwrap();
    let rmeta = ws.join("target/debug/deps/liba-xa.rmeta");
    let ext = format!("a={}", rmeta.display());
    let stand_in = relay.clone();
    let r = w.call_with(&ws, "b", &["--extern", &ext], move |fake| {
        fake.relay = Some(stand_in);
    });
    assert_eq!(r.code, 1);
    assert!(
        relay.borrow().asked.iter().any(
            |r| matches!(r, Request::Report { key: k, problem: Problem::Rejected } if *k == key)
        )
    );
    let now = SystemTime::now();
    assert!(w.store().suspended_at(&step, now).is_some());
    assert!(
        w.store()
            .suspended_at(&step, now + Duration::from_secs(16 * 86_400))
            .is_none()
    );
}

/// Machine `m`'s plans for the proc macro `m` and the library `a` that uses it.
fn macro_plans(m: &Box2) {
    let line =
        r#""{ws}/tools/cc" "{ws}/target/debug/deps/x.o" "-o" "{ws}/target/debug/deps/libm-xm.so""#;
    m.w.plan(
        "m",
        Plan {
            stdout: Some(format!("{line}\n")),
            ..Plan::default()
        },
    );
    m.w.plan("a", Plan::default());
}

/// A checkout on machine `m` with the proc macro `m` beside the library `a`.
fn macro_checkout(m: &Box2, name: &str) -> PathBuf {
    let ws = m.w.checkout(name);
    fs::create_dir_all(ws.join("tools")).unwrap();
    fs::write(ws.join("tools/cc"), "linker").unwrap();
    fs::create_dir_all(ws.join("m/src")).unwrap();
    fs::write(ws.join("m/src/lib.rs"), "// the macro\n").unwrap();
    ws
}

/// The proc macro's file in `ws`'s output folder.
fn macro_file(ws: &Path) -> PathBuf {
    ws.join(format!(
        "target/debug/deps/libm-xm.{}",
        std::env::consts::DLL_EXTENSION
    ))
}

fn call_macro(m: &Box2, ws: &Path) -> Result {
    let mut args = m.w.args(ws, "m", &[]);
    let at = args.iter().position(|a| a == "lib").unwrap();
    args[at] = "proc-macro".into();
    m.w.call_args(ws, "m", args, |fake| m.toolchain_vars(fake))
}

/// The library `a`, with the proc macro as its extern, telling `relay` what it built.
fn call_user(m: &Box2, ws: &Path, relay: &Rc<RefCell<RelayStandIn>>) -> Result {
    let ext = format!("m={}", macro_file(ws).display());
    let relay = relay.clone();
    m.w.call_with(ws, "a", &["--extern", &ext], move |fake| {
        m.toolchain_vars(fake);
        fake.relay = Some(relay);
    })
}

/// Whether the relay heard that a block it could send was built: the outbox marker's cause.
fn told_portable(relay: &Rc<RefCell<RelayStandIn>>) -> Option<bool> {
    relay.borrow().asked.iter().rev().find_map(|r| match r {
        Request::Built { portable, .. } => Some(*portable),
        _ => None,
    })
}

// covers: B-6, B-24
#[test]
fn a_restored_proc_macro_whose_record_the_store_cant_confirm_is_named_by_its_bytes() {
    let mut m = a_machine("dev", "stable-aarch64-apple-darwin");
    m.w.share("acme");
    macro_plans(&m);
    let honest = macro_checkout(&m, "honest");
    settle();
    assert_eq!(call_macro(&m, &honest).outcome(), STORED);
    let relay = relay_answering(|_| Some(Answer::None));
    let r = call_user(&m, &honest, &relay);
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    let honest_key = r.entry.unwrap().key.unwrap();
    assert!(
        m.keys()["a"]["keys"]["externs"][0]["by"]["produced-by"].is_string(),
        "{}",
        m.keys()["a"]
    );
    assert_eq!(told_portable(&relay), Some(true));

    // Another checkout whose `target/` came back from a cache: the macro doctored, and its record
    // rewritten to match the doctored bytes while it names the honest macro.
    let restored = macro_checkout(&m, "restored");
    copy_tree(&honest.join("target"), &restored.join("target"));
    let so = macro_file(&restored);
    let mut doctored = fs::read(&so).unwrap();
    doctored.extend_from_slice(b"\ndoctored expansion");
    fs::write(&so, &doctored).unwrap();
    let record = crate::sidecar::record_path(&so).unwrap();
    let mut text: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    assert!(text["produced_by"].is_string(), "{text}");
    text["sha256"] = serde_json::to_value(Digest::of(&doctored)).unwrap();
    fs::write(&record, serde_json::to_vec(&text).unwrap()).unwrap();
    settle();
    let relay = relay_answering(|_| Some(Answer::None));
    let r = call_user(&m, &restored, &relay);
    // Built against the doctored macro, under a key of its own, and kept on this machine.
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    let entry = r.entry.unwrap();
    assert_ne!(entry.key, Some(honest_key));
    assert_eq!(
        m.keys()["a"]["keys"]["externs"][0]["by"]["content"],
        serde_json::to_value(Digest::of(&doctored)).unwrap()
    );
    let portable = entry.portable.unwrap();
    assert!(!portable.portable, "{portable:?}");
    assert!(portable.reasons.contains(&crate::portable::Reason::Extern));
    assert_eq!(portable.externs, ["m"]);
    assert_eq!(told_portable(&relay), Some(false), "no outbox marker");
    // Still reused here, by its bytes.
    fs::remove_file(m.w.rlib(&restored, "a")).unwrap();
    assert_eq!(call_user(&m, &restored, &relay).outcome(), REUSED);

    // A CI job's store is its own and starts empty: even the honest `target/` proves nothing there.
    let ci = a_machine("runner", "stable-aarch64-apple-darwin");
    macro_plans(&ci);
    let job = macro_checkout(&ci, "job");
    copy_tree(&honest.join("target"), &job.join("target"));
    settle();
    let relay = relay_answering(|_| Some(Answer::None));
    let r = call_user(&ci, &job, &relay);
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    assert!(ci.keys()["a"]["keys"]["externs"][0]["by"]["content"].is_string());
    let portable = r.entry.unwrap().portable.unwrap();
    assert!(portable.reasons.contains(&crate::portable::Reason::Extern));
}

// covers: B-6, B-12
#[test]
fn a_proc_macro_whose_record_the_store_confirms_keeps_its_name_and_reuse_across_checkouts() {
    let m = a_machine("dev", "stable-aarch64-apple-darwin");
    macro_plans(&m);
    let (one, two, three) = (
        macro_checkout(&m, "one"),
        macro_checkout(&m, "two-longer"),
        macro_checkout(&m, "three"),
    );
    settle();
    let relay = relay_answering(|_| Some(Answer::None));
    assert_eq!(call_macro(&m, &one).outcome(), STORED);
    let r = call_user(&m, &one, &relay);
    assert_eq!(r.outcome(), STORED, "{}", r.stderr);
    assert!(r.entry_portable());
    let key = r.entry.unwrap().key.unwrap();
    let by = m.keys()["a"]["keys"]["externs"][0]["by"].clone();
    assert!(by["produced-by"].is_string(), "{by}");
    // Another checkout reuses the macro, then its dependent under the same key.
    assert_eq!(call_macro(&m, &two).outcome(), REUSED);
    let r = call_user(&m, &two, &relay);
    assert_eq!(r.outcome(), REUSED, "{}", r.stderr);
    assert_eq!(r.entry.unwrap().key, Some(key));
    // A copy of the first checkout's `target/` on this machine: its records name blocks this
    // store holds, with the same bytes.
    copy_tree(&one.join("target"), &three.join("target"));
    fs::remove_file(m.w.rlib(&three, "a")).unwrap();
    settle();
    let r = call_user(&m, &three, &relay);
    assert_eq!(r.outcome(), REUSED, "{}", r.stderr);
    assert_eq!(r.entry.unwrap().key, Some(key));
}

impl Result {
    /// Whether the call's block may leave the machine.
    fn entry_portable(&self) -> bool {
        self.entry
            .as_ref()
            .and_then(|e| e.portable.as_ref())
            .is_some_and(|p| p.portable)
    }
}
