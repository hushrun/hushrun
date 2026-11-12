//! \[ADR 0070\] The Go adapter, compiler-given (C-30, C-32): a unit is a package's tests, named by
//! the package's directory, and what it read is what `go list -deps -test` says its test binary
//! is built from, with what Go's own test log says the binary opened while its tests ran.
//!
//! - Before COMMAND: when COMMAND is `go test …` in a form hushrun reads, hushrun adds
//!   `-exec=<hushrun> __hushrun-test-runner go <N> <runner>…` to `GOFLAGS` (keeping what `go env
//!   GOFLAGS` says, and chaining to an `-exec` it holds), so that `go test` starts every test
//!   binary through hushrun's target runner ([`super::runner`]). The runner starts it in the
//!   package's directory, as `go test` would have, with `-test.testlogfile`, the log `go test`
//!   itself asks for when it caches a result, so that what the tests open is known.
//! - With `-exec`, `go test` neither stores nor finds a cached result (cmd/go writes no test log
//!   then, and its cache key holds the exec command): every package runs, or is reused by hushrun.
//!   A package `go test` reports `(cached)` never reaches the runner, so it would have no start,
//!   and be `not run`: it records nothing.
//! - The plan asks `go list` with COMMAND's build flags (`-tags`, `-race`, `-mod`, …) in COMMAND's
//!   environment, before COMMAND; after it, hushrun asks again, and reads the runner's starts.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use hushrun_core::units::{self, Facts, NotFollowed, Sets, Sources, Tree};

use super::adapter::{
    Adapter, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::facts::{FactsFile, Invocation};
use super::runner;

/// The adapter's name in a facts file.
pub const ADAPTER: &str = "go";
/// What a compiler-given adapter's sources are, of the files `go list` names (C-30).
pub const SOURCE_KINDS: &[&str] = &[".go"];
/// The runner's word for Go, where Cargo's target runner has a triple's.
pub const RUNNER_WORD: &str = "go";
/// The variable that tells the runner what `GOFLAGS` was in COMMAND's environment: `=` and its
/// value when it was set, absent when it wasn't.
pub const GOFLAGS_WAS: &str = "HUSHRUN_GOFLAGS";
/// The longest hushrun waits for `go list`.
const LIST_BUDGET: Duration = Duration::from_secs(120);
/// The most lines of a test log hushrun reads: past it, the package is `not observed`.
pub const MOST_LOG_LINES: usize = 200_000;

/// COMMAND, when it is `go test …` in a form hushrun reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    /// `go`, as COMMAND starts it.
    pub go: String,
    /// `-C <dir>`, which `go list` takes first too.
    pub dir: Option<String>,
    /// The build flags that choose which files `go test` compiles, as `go list` takes them.
    pub build: Vec<String>,
    /// The packages, as COMMAND names them; none is the package in the directory.
    pub packages: Vec<String>,
    /// \[ADR 0083\] COMMAND's own `-run` or `-skip`, by its flag: which tests ran is then Go's
    /// matching, and its tests are reused by package.
    pub chooses: Option<&'static str>,
}

/// Build flags `go test` takes with no value, and whether `go list` takes them too because they
/// choose files (`-race` sets the `race` build tag).
const BOOL_FLAGS: &[(&str, bool)] = &[
    ("a", false),
    ("n", false),
    ("race", true),
    ("msan", true),
    ("asan", true),
    ("cover", false),
    ("v", false),
    ("work", false),
    ("x", false),
    ("modcacherw", false),
    ("trimpath", false),
    ("json", false),
    ("benchmem", false),
    ("failfast", false),
    ("fullpath", false),
    ("short", false),
    ("artifacts", false),
];

/// Flags that take a value, and whether `go list` takes them too.
const VALUE_FLAGS: &[(&str, bool)] = &[
    ("p", false),
    ("covermode", false),
    ("coverpkg", false),
    ("asmflags", false),
    ("buildvcs", false),
    ("gccgoflags", false),
    ("gcflags", false),
    ("installsuffix", false),
    ("ldflags", false),
    ("mod", true),
    ("modfile", true),
    ("pgo", false),
    ("pkgdir", false),
    ("tags", true),
    ("o", false),
    ("vet", false),
    ("bench", false),
    ("benchtime", false),
    ("blockprofile", false),
    ("blockprofilerate", false),
    ("count", false),
    ("coverprofile", false),
    ("cpu", false),
    ("cpuprofile", false),
    ("list", false),
    ("memprofile", false),
    ("memprofilerate", false),
    ("mutexprofile", false),
    ("mutexprofilefraction", false),
    ("outputdir", false),
    ("parallel", false),
    ("run", false),
    ("shuffle", false),
    ("skip", false),
    ("timeout", false),
    ("trace", false),
];

/// What `command` is for the Go adapter: `None` when it isn't `go test`; `Some(Err(why))` when it
/// is, in a form hushrun doesn't read, which the run says.
pub fn recognize(command: &[String]) -> Option<Result<Recognized, String>> {
    let program = command.first()?;
    let file = super::command::program_name(program);
    let file = &*file;
    if file != "go" || command.get(1).map(String::as_str) != Some("test") {
        return None;
    }
    let mut out = Recognized {
        go: program.clone(),
        dir: None,
        build: Vec::new(),
        packages: Vec::new(),
        chooses: None,
    };
    let mut words = command[2..].iter();
    let mut first = true;
    while let Some(word) = words.next() {
        let was_first = std::mem::replace(&mut first, false);
        if word == "-args" || word == "--args" {
            // \[ADR 0083\] What follows goes to each test binary as it is: its own `-test.run`.
            for word in words.by_ref() {
                let name = word.trim_start_matches('-');
                let name = name.split_once('=').map_or(name, |(name, _)| name);
                match name {
                    "test.run" => out.chooses = out.chooses.or(Some("-run")),
                    "test.skip" => out.chooses = out.chooses.or(Some("-skip")),
                    _ => {}
                }
            }
            break;
        }
        let Some(flag) = word.strip_prefix("--").or_else(|| word.strip_prefix('-')) else {
            if word.ends_with(".go") {
                return Some(Err(
                    "go test names .go files, which make a package of their own".into(),
                ));
            }
            out.packages.push(word.clone());
            continue;
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (flag, None),
        };
        match name {
            "C" if was_first => {
                let Some(value) = inline.or_else(|| words.next().cloned()) else {
                    return Some(Err("go test's -C has no value".into()));
                };
                out.dir = Some(value);
                continue;
            }
            "c" => return Some(Err("go test -c builds test binaries and runs none".into())),
            "exec" => {
                return Some(Err(
                    "COMMAND starts test binaries through a program of its own (-exec)".into(),
                ));
            }
            "fuzz" => {
                return Some(Err(
                    "go test -fuzz writes to the repository's testdata".into()
                ));
            }
            "overlay" => {
                return Some(Err(
                    "go test -overlay compiles other files than the repository's".into(),
                ));
            }
            "toolexec" | "compiler" | "buildmode" | "linkshared" => {
                return Some(Err(format!(
                    "go test -{name} builds in a way hushrun doesn't read"
                )));
            }
            _ => {}
        }
        if let Some((_, listed)) = BOOL_FLAGS.iter().find(|(n, _)| *n == name) {
            if *listed {
                out.build.push(word.clone());
            }
            continue;
        }
        if let Some((_, listed)) = VALUE_FLAGS.iter().find(|(n, _)| *n == name) {
            let value = match inline {
                Some(value) => value,
                None => match words.next() {
                    Some(value) => value.clone(),
                    None => return Some(Err(format!("go test's -{name} has no value"))),
                },
            };
            if *listed {
                out.build.push(format!("-{name}={value}"));
            }
            match name {
                "run" => out.chooses = out.chooses.or(Some("-run")),
                "skip" => out.chooses = out.chooses.or(Some("-skip")),
                _ => {}
            }
            continue;
        }
        return Some(Err(format!(
            "go test's flag {word} isn't one hushrun reads"
        )));
    }
    Some(Ok(out))
}

/// What `go env` says, in COMMAND's environment.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct GoEnv {
    #[serde(default)]
    goflags: String,
    #[serde(default)]
    goos: String,
    #[serde(default)]
    goarch: String,
    #[serde(default)]
    gohostos: String,
    #[serde(default)]
    gohostarch: String,
}

/// The variables that put hushrun's runner in front of every test binary `go test` starts, or
/// why it can't: `exe` is this hushrun, `cwd` where COMMAND runs, and `goflags` what `GOFLAGS`
/// is in COMMAND's environment (`None` when unset).
pub fn variables(
    recognized: &Recognized,
    exe: &Path,
    cwd: &Path,
    goflags: Option<OsString>,
    assigned: &[(String, String)],
) -> Result<Vec<(String, OsString)>, String> {
    let output = go(recognized, cwd, assigned)
        .args([
            "env",
            "-json",
            "GOFLAGS",
            "GOOS",
            "GOARCH",
            "GOHOSTOS",
            "GOHOSTARCH",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|err| format!("can't start {}: {err}", recognized.go))?;
    if !output.status.success() {
        return Err("go env failed".into());
    }
    let env: GoEnv =
        serde_json::from_slice(&output.stdout).map_err(|err| format!("go env: {err}"))?;
    if env.goos != env.gohostos || env.goarch != env.gohostarch {
        return Err(format!(
            "it tests for {}/{} on {}/{}, through a program hushrun doesn't chain to",
            env.goos, env.goarch, env.gohostos, env.gohostarch
        ));
    }
    let value = goflags_with_runner(&env.goflags, exe)?;
    let was = match goflags {
        Some(flags) => {
            let mut was = OsString::from("=");
            was.push(flags);
            was
        }
        None => OsString::new(),
    };
    let mut out = vec![("GOFLAGS".to_string(), OsString::from(value))];
    if !was.is_empty() {
        out.push((GOFLAGS_WAS.to_string(), was));
    }
    Ok(out)
}

/// `goflags`, `go env GOFLAGS` as it is, with hushrun's runner last, so that it wins as a flag
/// given twice does, chained to an `-exec` it already holds: `'-exec=<exe> __hushrun-test-runner
/// go <N> <word>…'`, split by Go as it splits `GOFLAGS` and then `-exec`'s value.
pub fn goflags_with_runner(goflags: &str, exe: &Path) -> Result<String, String> {
    let entries = split_quoted(goflags).ok_or("GOFLAGS has a quote that doesn't close")?;
    let mut chain: Vec<String> = Vec::new();
    for entry in &entries {
        let flag = entry.strip_prefix("--").or_else(|| entry.strip_prefix('-'));
        if let Some(value) = flag.and_then(|f| f.strip_prefix("exec=")) {
            chain = split_quoted(value).ok_or("GOFLAGS's -exec has a quote that doesn't close")?;
        }
    }
    let exe = exe.to_str().ok_or("this hushrun's path isn't UTF-8")?;
    let mut words = vec![
        exe.to_string(),
        runner::SUBCOMMAND.to_string(),
        RUNNER_WORD.to_string(),
        chain.len().to_string(),
    ];
    words.extend(chain);
    let mut value = String::from("-exec=");
    for (i, word) in words.iter().enumerate() {
        if word.contains(['"', '\'']) || word.chars().any(|c| c.is_control()) {
            return Err(format!("{word} holds a quote, which GOFLAGS can't carry"));
        }
        if i > 0 {
            value.push(' ');
        }
        if word.contains(char::is_whitespace) || word.is_empty() {
            value.push('"');
            value.push_str(word);
            value.push('"');
        } else {
            value.push_str(word);
        }
    }
    let entry = format!("'{value}'");
    Ok(if goflags.trim().is_empty() {
        entry
    } else {
        format!("{goflags} {entry}")
    })
}

/// Go's `cmd/internal/quoted.Split`: fields at spaces, a field may be all in `'` or `"`, with
/// nothing unescaped inside. `None` for a quote that doesn't close.
pub fn split_quoted(text: &str) -> Option<Vec<String>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let space = |b: u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r');
    while i < bytes.len() {
        while i < bytes.len() && space(bytes[i]) {
            i += 1;
        }
        if i == bytes.len() {
            break;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            let start = i + 1;
            let end = start + bytes[start..].iter().position(|b| *b == quote)?;
            out.push(text[start..end].to_string());
            i = end + 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && !space(bytes[i]) {
            i += 1;
        }
        out.push(text[start..i].to_string());
    }
    Some(out)
}

/// \[C-32\] A `go` command hushrun asks, as COMMAND's `go` will run: in `cwd`, with COMMAND's own
/// `env NAME=value` assignments (`assigned`: a `GOARCH`, a `GOFLAGS` with build tags), and none
/// of hushrun's own variables.
fn go(recognized: &Recognized, cwd: &Path, assigned: &[(String, String)]) -> Command {
    super::command::tool(&recognized.go, cwd, assigned)
}

/// One package as `go list -json` writes it, the fields hushrun reads.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Package {
    import_path: String,
    dir: PathBuf,
    for_test: String,
    standard: bool,
    incomplete: bool,
    error: Option<serde_json::Value>,
    deps_errors: Vec<serde_json::Value>,
    deps: Vec<String>,
    /// \[ADR 0072\] What it imports, for `hushrun why`'s path.
    imports: Vec<String>,
    go_files: Vec<String>,
    cgo_files: Vec<String>,
    c_files: Vec<String>,
    c_x_x_files: Vec<String>,
    m_files: Vec<String>,
    h_files: Vec<String>,
    f_files: Vec<String>,
    s_files: Vec<String>,
    swig_files: Vec<String>,
    swig_c_x_x_files: Vec<String>,
    syso_files: Vec<String>,
    embed_files: Vec<String>,
    test_go_files: Vec<String>,
    x_test_go_files: Vec<String>,
    test_embed_files: Vec<String>,
    x_test_embed_files: Vec<String>,
}

/// The fields hushrun asks `go list` for.
const FIELDS: &str = "ImportPath,Dir,ForTest,Standard,Incomplete,Error,DepsErrors,Deps,Imports,GoFiles,CgoFiles,CFiles,CXXFiles,MFiles,HFiles,FFiles,SFiles,SwigFiles,SwigCXXFiles,SysoFiles,EmbedFiles,TestGoFiles,XTestGoFiles,TestEmbedFiles,XTestEmbedFiles";

/// What `go list -deps -test` says of COMMAND's packages, read: each package's tests, what its
/// test binary is built from, and what can't be told of it.
#[derive(Debug, Default)]
pub struct Listed {
    /// Each tested package, by its directory from the repository root (`.` for the root).
    pub units: BTreeMap<String, ListedUnit>,
    /// The unit rule: every test file of a tested package (C-30).
    pub unit_files: BTreeSet<String>,
    /// The `.go` files `go list` says a listed package of the repository is built from: the
    /// sources (C-30).
    pub named: BTreeSet<String>,
    /// \[ADR 0072\] Which package of the repository imports which, by their directories.
    pub edges: BTreeSet<super::db::Edge>,
}

/// One package's tests.
#[derive(Debug, Clone, Default)]
pub struct ListedUnit {
    /// Its directory.
    pub dir: PathBuf,
    /// Its test files, from the repository root.
    pub own: Vec<String>,
    /// The files of the repository its test binary is built from.
    pub reads: BTreeSet<String>,
    pub not_followed: BTreeSet<NotFollowed>,
}

/// `go list -e -deps -test -json` with COMMAND's build flags and packages, in `cwd`, with
/// hushrun's environment, which is COMMAND's but for what hushrun adds; read for the repository
/// at `root`. `Err` says why nothing can be told.
pub fn list(
    recognized: &Recognized,
    root: &Path,
    cwd: &Path,
    assigned: &[(String, String)],
) -> Result<Listed, String> {
    let mut command = go(recognized, cwd, assigned);
    command.arg("list");
    if let Some(dir) = &recognized.dir {
        // `-C` must come first.
        command.args(["-C", dir]);
    }
    command
        .args(["-e", "-deps", "-test"])
        .arg(format!("-json={FIELDS}"))
        .args(&recognized.build)
        .args(&recognized.packages)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|err| format!("can't start {}: {err}", recognized.go))?;
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > LIST_BUDGET => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("go list took too long to say what go test builds".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(err) => return Err(format!("go list: {err}")),
        }
    };
    let bytes = reader.join().unwrap_or_default();
    if !status.success() {
        return Err("go list failed".into());
    }
    let mut packages = Vec::new();
    for item in serde_json::Deserializer::from_slice(&bytes).into_iter::<Package>() {
        packages.push(item.map_err(|err| format!("go list: {err}"))?);
    }
    Ok(read_list(packages, root))
}

fn read_list(packages: Vec<Package>, root: &Path) -> Listed {
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root = real(root);
    let relative = |path: &Path| -> Option<String> {
        let path = real(path);
        let inside = path.strip_prefix(&root).ok()?;
        let parts: Vec<String> = inside
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        Some(parts.join("/"))
    };
    let by_path: BTreeMap<&str, &Package> = packages
        .iter()
        .map(|p| (p.import_path.as_str(), p))
        .collect();
    let in_repo = |p: &Package| !p.standard && relative(&p.dir).is_some();
    let files_of = |p: &Package| -> Vec<PathBuf> {
        [
            &p.go_files,
            &p.cgo_files,
            &p.c_files,
            &p.c_x_x_files,
            &p.m_files,
            &p.h_files,
            &p.f_files,
            &p.s_files,
            &p.swig_files,
            &p.swig_c_x_x_files,
            &p.syso_files,
            &p.embed_files,
            &p.test_embed_files,
            &p.x_test_embed_files,
        ]
        .into_iter()
        .flatten()
        .map(|name| p.dir.join(name))
        .collect()
    };

    let mut out = Listed::default();
    let unit_of = |dir: String| if dir.is_empty() { ".".to_string() } else { dir };
    for p in packages.iter().filter(|p| in_repo(p)) {
        for name in p.go_files.iter().chain(&p.cgo_files) {
            out.named.extend(relative(&p.dir.join(name)));
        }
        let Some(from) = relative(&p.dir).map(unit_of) else {
            continue;
        };
        for import in &p.imports {
            let Some(to) = by_path
                .get(import.as_str())
                .filter(|q| in_repo(q))
                .and_then(|q| relative(&q.dir))
                .map(unit_of)
            else {
                continue;
            };
            if to != from {
                out.edges
                    .insert((from.clone(), to, super::db::EdgeKind::Import));
            }
        }
    }
    // Each test main, `<package>.test`, of a package some variant is for.
    let tested: BTreeSet<&str> = packages
        .iter()
        .filter(|p| !p.for_test.is_empty())
        .map(|p| p.for_test.as_str())
        .collect();
    for path in tested {
        let Some(package) = by_path.get(path) else {
            continue;
        };
        let Some(dir) = relative(&package.dir) else {
            continue;
        };
        let unit = if dir.is_empty() { ".".to_string() } else { dir };
        let own: Vec<String> = package
            .test_go_files
            .iter()
            .chain(&package.x_test_go_files)
            .filter_map(|name| relative(&package.dir.join(name)))
            .collect();
        out.unit_files.extend(own.iter().cloned());
        let mut not_followed = BTreeSet::new();
        let mut reads = BTreeSet::new();
        let main = by_path.get(format!("{path}.test").as_str());
        let Some(main) = main else {
            not_followed.insert(NotFollowed {
                why: "go-list".into(),
                detail: format!("go list names no test binary for {path}"),
            });
            out.units.insert(
                unit,
                ListedUnit {
                    dir: package.dir.clone(),
                    own,
                    reads,
                    not_followed,
                },
            );
            continue;
        };
        if main.error.is_some() || !main.deps_errors.is_empty() || main.incomplete {
            not_followed.insert(NotFollowed {
                why: "go-list".into(),
                detail: format!("go list can't load all of {path}'s tests"),
            });
        }
        // The test main go generates calls the package's TestMain, which runs before Go's test
        // log starts: what it opens or starts isn't seen.
        let generated = main
            .go_files
            .first()
            .and_then(|f| std::fs::read_to_string(main.dir.join(f)).ok());
        match generated {
            Some(text) if text.contains(".TestMain(m)") => {
                not_followed.insert(NotFollowed {
                    why: "test-main".into(),
                    detail: path.to_string(),
                });
            }
            Some(_) => {}
            None => {
                not_followed.insert(NotFollowed {
                    why: "go-list".into(),
                    detail: format!("go's test main for {path} can't be read"),
                });
            }
        }
        for dep in &main.deps {
            let Some(dep) = by_path.get(dep.as_str()) else {
                continue;
            };
            if !in_repo(dep) {
                continue;
            }
            if !dep.cgo_files.is_empty()
                || !dep.swig_files.is_empty()
                || !dep.swig_c_x_x_files.is_empty()
                || !dep.syso_files.is_empty()
            {
                not_followed.insert(NotFollowed {
                    why: "native".into(),
                    detail: dep.import_path.clone(),
                });
            }
            reads.extend(files_of(dep).iter().filter_map(|f| relative(f)));
            for name in dep.go_files.iter().chain(&dep.cgo_files) {
                out.named.extend(relative(&dep.dir.join(name)));
            }
        }
        out.units.insert(
            unit,
            ListedUnit {
                dir: package.dir.clone(),
                own,
                reads,
                not_followed,
            },
        );
    }
    out
}

/// What the Go adapter observed of a run, before hushrun decides any state.
#[derive(Debug, Default)]
pub struct Observed {
    pub units: BTreeMap<String, super::Seen>,
    pub unit_files: BTreeSet<String>,
    pub named: BTreeSet<String>,
    /// Each unit's test files.
    pub own: BTreeMap<String, Vec<String>>,
    /// The directories each unit's tests listed, from the root.
    pub dirs: BTreeMap<String, BTreeSet<String>>,
    /// What each unit's test binary is built from, `go list` says: what a plan keys it by, with
    /// the map's hint (C-34).
    pub compiled: BTreeMap<String, BTreeSet<String>>,
    /// \[ADR 0072\] Which package imports which, and what each package's tests opened.
    pub edges: BTreeSet<super::db::Edge>,
}

/// Reads what the Go adapter saw: `files`' starts, and `go list` asked after COMMAND, in `cwd`,
/// for the repository at `root` whose files are `tree`. `command_passed`: COMMAND exited 0.
pub fn observe(
    recognized: &Recognized,
    files: &[FactsFile],
    root: &Path,
    cwd: &Path,
    tree: &Tree,
    assigned: &[(String, String)],
) -> Result<Observed, String> {
    let listed = list(recognized, root, cwd, assigned)?;
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let real_root = real(root);
    let mut starts: BTreeMap<PathBuf, Vec<&Invocation>> = BTreeMap::new();
    for invocation in files
        .iter()
        .filter(|f| f.adapter == ADAPTER)
        .filter_map(|f| f.invocation.as_ref())
    {
        let Some(dir) = &invocation.dir else {
            return Err("the runner didn't say where a test binary ran".into());
        };
        starts.entry(real(dir)).or_default().push(invocation);
    }
    // A start in a directory of the repository that go list doesn't name as a tested package: go
    // test ran something hushrun didn't ask about, so nothing it says can be trusted.
    let known: BTreeSet<PathBuf> = listed.units.values().map(|u| real(&u.dir)).collect();
    if let Some(stray) = starts
        .keys()
        .find(|dir| dir.starts_with(&real_root) && !known.contains(*dir))
    {
        return Err(format!(
            "go test ran the tests of {}, which go list didn't name",
            stray.display()
        ));
    }
    let mut out = Observed {
        unit_files: listed.unit_files,
        named: listed.named,
        edges: listed.edges,
        ..Observed::default()
    };
    for (unit, listed_unit) in listed.units {
        let mine = starts
            .get(&real(&listed_unit.dir))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let ran: Vec<&&Invocation> = mine.iter().filter(|s| !s.reused).collect();
        let ok = |s: &Invocation| s.exit == Some(0) && s.signal.is_none();
        let failed = ran.iter().any(|s| !ok(s));
        let complete = !ran.is_empty();
        let deselected = !mine.is_empty() && ran.is_empty();
        let duration_ms = ran
            .iter()
            .map(|s| s.duration_ms)
            .fold(0, u64::saturating_add);
        let mut not_followed = listed_unit.not_followed;
        let mut reads = listed_unit.reads;
        let mut opened = BTreeSet::new();
        let mut dirs = BTreeSet::new();
        for start in &ran {
            match &start.testlog {
                Some(lines) if !start.testlog_truncated => {
                    let log = read_log(lines, &listed_unit.dir, &real_root);
                    for program in log.programs {
                        not_followed.insert(NotFollowed {
                            why: "subprocess".into(),
                            detail: program,
                        });
                    }
                    opened.extend(log.files.iter().cloned());
                    dirs.extend(log.dirs);
                }
                Some(_) => {
                    not_followed.insert(NotFollowed {
                        why: "test-log".into(),
                        detail: "its test log was too long to read".into(),
                    });
                }
                // A binary that failed before its tests began wrote no log, and failed.
                None if ok(start) => {
                    not_followed.insert(NotFollowed {
                        why: "test-log".into(),
                        detail: "it wrote no test log".into(),
                    });
                }
                None => {}
            }
        }
        out.compiled.insert(unit.clone(), reads.clone());
        out.edges.extend(
            opened
                .iter()
                .map(|file| (unit.clone(), file.clone(), super::db::EdgeKind::Open)),
        );
        reads.extend(opened);
        reads.extend(under(tree, &dirs));
        out.own.insert(unit.clone(), listed_unit.own);
        out.dirs.insert(unit.clone(), dirs);
        out.units.insert(
            unit,
            super::Seen {
                facts: Facts {
                    complete,
                    failed,
                    not_followed,
                    deselected,
                    ..Facts::default()
                },
                reads,
                duration_ms,
                tests: None,
            },
        );
    }
    Ok(out)
}

/// Every file of `tree` in one of `dirs` or below it: what listing a directory read.
pub fn under(tree: &Tree, dirs: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for dir in dirs {
        if dir.is_empty() || dir == "." {
            out.extend(tree.paths().map(str::to_string));
            continue;
        }
        let prefix = format!("{dir}/");
        out.extend(
            tree.paths()
                .filter(|p| p.starts_with(&prefix))
                .map(str::to_string),
        );
    }
    out
}

/// What a test log says a package's tests did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Log {
    /// Files of the repository they opened or looked at, from its root.
    pub files: BTreeSet<String>,
    /// Directories of the repository they listed, from its root.
    pub dirs: BTreeSet<String>,
    /// Programs they started, anywhere: `os.StartProcess` logs an open of the program.
    pub programs: Vec<String>,
}

/// Reads a test log (`# test log`, then `open`, `stat`, `chdir` and `getenv` lines), whose relative
/// paths start from `dir`, the package's directory, for the repository at `root`. An `open` of an
/// executable file is a program started, wherever it is. \[ADR 0074\] So is what
/// [`started_from`] reads as one: a program found from `exec.Cmd.Dir`, or one gone by now.
pub fn read_log(lines: &[String], dir: &Path, root: &Path) -> Log {
    let mut out = Log::default();
    let mut pwd = dir.to_path_buf();
    for (n, line) in lines.iter().enumerate() {
        let Some((op, name)) = line.split_once(' ') else {
            continue;
        };
        let path = normalize(&pwd.join(name));
        match op {
            "chdir" => {
                pwd = path;
                continue;
            }
            "open" | "stat" => {}
            _ => continue,
        }
        if op == "open"
            && let Some(program) = started_from(
                name,
                n.checked_sub(1).and_then(|before| lines.get(before)),
                lines.get(n + 1),
                &pwd,
            )
        {
            out.programs.push(program);
            continue;
        }
        let real = std::fs::canonicalize(&path).unwrap_or(path);
        let Ok(meta) = std::fs::metadata(&real) else {
            continue;
        };
        if op == "open" && meta.is_file() && executable(&meta) {
            out.programs.push(real.display().to_string());
            continue;
        }
        let Ok(inside) = real.strip_prefix(root) else {
            continue;
        };
        let parts: Vec<String> = inside
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let relative = parts.join("/");
        if meta.is_dir() {
            if op == "open" {
                out.dirs.insert(relative);
            }
        } else if !relative.is_empty() {
            out.files.insert(relative);
        }
    }
    out
}

/// \[ADR 0074\] The program an `open` of `name` started, for what the plain rule (an `open` of
/// an executable file, from the test's directory `pwd`) can't see. The log is read after the
/// tests, when a program they wrote to a temporary directory is gone, and `os.StartProcess` logs
/// the program as given, before the child moves to `exec.Cmd.Dir`. So it is a program:
///
/// - when the line after it, `next`, is a `stat` of a directory (`os.StartProcess` looks at
///   `Dir` right after it logs the program), and the relative `name` is an executable file from
///   there, or that directory is gone, so nobody can say it wasn't one;
/// - when the file is gone, and the line before it, `before`, is an `open` of `/dev/null`, which
///   `exec.Cmd` opens for a child's standard input, output or error just before it starts it.
///
/// A file that is gone and was only opened costs its package its reuse, never a wrong one.
fn started_from(
    name: &str,
    before: Option<&String>,
    next: Option<&String>,
    pwd: &Path,
) -> Option<String> {
    let relative = !Path::new(name).is_absolute();
    if relative && let Some(("stat", dir)) = next.and_then(|line| line.split_once(' ')) {
        let dir = normalize(&pwd.join(dir));
        match std::fs::metadata(&dir) {
            Ok(meta) if meta.is_dir() => {
                let program = normalize(&dir.join(name));
                if std::fs::metadata(&program).is_ok_and(|m| m.is_file() && executable(&m)) {
                    return Some(program.display().to_string());
                }
            }
            Ok(_) => {}
            Err(_) => return Some(normalize(&dir.join(name)).display().to_string()),
        }
    }
    let path = normalize(&pwd.join(name));
    let gone = std::fs::symlink_metadata(&path).is_err();
    let after_null = before.is_some_and(|line| line == "open /dev/null");
    (gone && after_null && cfg!(unix)).then(|| path.display().to_string())
}

#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_: &std::fs::Metadata) -> bool {
    false
}

/// `a/b/../c` as `a/c`, without asking the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// \[ADR 0083\] The longest a test binary's line may be, read as it goes by: past it, what the
/// binary printed can't be read, and no test of it records a pass.
const MOST_LINE: usize = 1 << 20;
/// \[ADR 0083\] The longest `-test.skip` pattern hushrun writes: past it, it leaves out no test.
const MOST_SKIP: usize = 64 << 10;
/// \[ADR 0083\] The most a binary's `-test.list` may print.
const MOST_LIST: usize = 16 << 20;

/// \[ADR 0083\] What a test binary's own arguments, as `go test` gave them, say of its tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BinaryFlags {
    /// COMMAND chose tests itself (`-run`, `-skip`, `-list`, `-fuzz`, or a `-count` that isn't a
    /// positive number): which ran is Go's matching, which hushrun doesn't reproduce.
    pub chooses: bool,
    /// `-test.v=test2json`, which `go test -json` asks for: Go marks its own lines.
    pub json: bool,
    /// \[ADR 0083, amended\] `-test.v`, true or `test2json` (`go test -v`, `-json`): Go prints a
    /// line for each test's outcome, a skip's included.
    pub names_each: bool,
    pub failfast: bool,
    /// `-test.paniconexit0`, which `go test` passes: `os.Exit(0)` in a test is a panic.
    pub panic_on_exit0: bool,
    /// COMMAND's own `-memprofile`, which would take the place of the one hushrun reads.
    pub memprofile: bool,
}

/// Go's `strconv.ParseBool`, as the `flag` package reads a boolean flag's value.
fn go_bool(value: Option<&str>) -> Option<bool> {
    match value {
        None | Some("1" | "t" | "T" | "true" | "TRUE" | "True") => Some(true),
        Some("0" | "f" | "F" | "false" | "FALSE" | "False") => Some(false),
        Some(_) => None,
    }
}

/// \[ADR 0083\] What `args`, a test binary's arguments, say of its tests. Every argument is read,
/// those past the first that isn't a flag too, which `flag` wouldn't parse: a selection anywhere
/// is a selection.
pub fn binary_flags(args: &[OsString]) -> BinaryFlags {
    let mut out = BinaryFlags::default();
    let mut words = args.iter().map(|a| a.to_string_lossy().into_owned());
    while let Some(word) = words.next() {
        let Some(flag) = word.strip_prefix("--").or_else(|| word.strip_prefix('-')) else {
            continue;
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (flag, None),
        };
        let Some(name) = name.strip_prefix("test.") else {
            continue;
        };
        match name {
            "run" | "skip" | "list" | "fuzz" => out.chooses = true,
            "memprofile" => out.memprofile = true,
            "count" => {
                let value = inline.or_else(|| words.next());
                if value
                    .and_then(|v| v.parse::<u64>().ok())
                    .is_none_or(|n| n < 1)
                {
                    out.chooses = true;
                }
            }
            "v" => {
                out.json = inline.as_deref() == Some("test2json");
                out.names_each = out.json || go_bool(inline.as_deref()) == Some(true);
            }
            // A malformed boolean stops the binary before any test: no pass either way.
            "failfast" => out.failfast = go_bool(inline.as_deref()).unwrap_or(true),
            "paniconexit0" => out.panic_on_exit0 = go_bool(inline.as_deref()).unwrap_or(false),
            _ => {}
        }
    }
    out
}

/// \[ADR 0083\] Whether the test binary at `path` has a `TestMain`: the name of the function Go's
/// test main calls, `<package>.TestMain`, which the binary's own table of function names holds
/// even when it is stripped. `None` when the binary can't be read. Seen where there is none, it
/// only costs the package its tests one by one.
pub fn has_test_main(path: &Path) -> Option<bool> {
    let bytes = std::fs::read(path).ok()?;
    let needle = b".TestMain\0";
    Some(bytes.windows(needle.len()).any(|w| w == needle))
}

/// \[ADR 0083\] The names a test binary's `-test.list=.` printed, in order: tests, benchmarks,
/// fuzz targets, examples. `None` when any line isn't a Go identifier: a package's `init` printed
/// something, and the list can't be told from it.
pub fn read_listed(stdout: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(stdout).ok()?;
    let mut out = Vec::new();
    for line in text.lines() {
        let first = line.chars().next()?;
        if first.is_ascii_digit() || !line.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        out.push(line.to_string());
    }
    Some(out)
}

/// \[ADR 0083\] Whether a listed name is a test, as RFC 0004 counts them: a top-level `Test…`,
/// `Example…` or `Fuzz…` function. Benchmarks aren't.
pub fn is_test(name: &str) -> bool {
    ["Test", "Example", "Fuzz"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// \[ADR 0083\] `-test.skip`'s value that leaves out exactly `names`, top-level tests: one
/// element (no `/`), so Go matches it against top-level names alone, anchored at both ends, each
/// name quoted as Go's `regexp.QuoteMeta` does.
pub fn skip_pattern<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let quoted: Vec<String> = names
        .into_iter()
        .map(|name| {
            let mut out = String::with_capacity(name.len());
            for c in name.chars() {
                if "\\.+*?()|[]{}^$".contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
            out
        })
        .collect();
    format!("^({})$", quoted.join("|"))
}

/// \[ADR 0083\] What the runner knows of a test binary's tests before it starts it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestsAt {
    /// The package, by its directory from the root.
    pub unit: String,
    /// Every name the binary listed, benchmarks included.
    pub listed: BTreeSet<String>,
    /// Of those, its tests, in the order it listed them.
    pub tests: Vec<String>,
    /// The tests hushrun leaves out with `-test.skip`.
    pub skipped: BTreeSet<String>,
    pub flags: BinaryFlags,
}

impl TestsAt {
    /// The argument that leaves out [`Self::skipped`], when there are any.
    pub fn skip_arg(&self) -> Option<OsString> {
        (!self.skipped.is_empty()).then(|| {
            OsString::from(format!(
                "-test.skip={}",
                skip_pattern(self.skipped.iter().map(String::as_str))
            ))
        })
    }
}

/// \[ADR 0083\] Prepares a start of the test binary at `binary`, of the package `unit`, with
/// `args`, test by test: `None` when its tests can't be told one by one, and it runs as a package.
/// `reuse` is what `hushrun.json`'s `reuseTests` lists for the package. `list(extra)` starts the
/// binary as the run would, with `extra` for arguments, and gives what it printed when it exited
/// 0. `old` is called when the binary can't leave out a test (a Go before 1.20).
pub fn prepare(
    unit: &str,
    binary: &Path,
    args: &[OsString],
    reuse: &[String],
    list: &mut dyn FnMut(&[&str]) -> Option<Vec<u8>>,
    old: &mut dyn FnMut(),
) -> Option<TestsAt> {
    let flags = binary_flags(args);
    // COMMAND chose its own tests, or a TestMain would run once more only to list them.
    if flags.chooses || flags.memprofile || has_test_main(binary) != Some(false) {
        return None;
    }
    // `-test.skip` is Go 1.20's: a binary from before refuses it, and still lists.
    let (printed, skips) = match list(&["-test.list=.", "-test.skip=^$"]) {
        Some(printed) => (printed, true),
        None => (list(&["-test.list=."])?, false),
    };
    if printed.len() > MOST_LIST {
        return None;
    }
    let listed = read_listed(&printed)?;
    let tests: Vec<String> = listed.iter().filter(|n| is_test(n)).cloned().collect();
    let mut skipped: BTreeSet<String> = reuse
        .iter()
        .filter(|id| tests.contains(id))
        .cloned()
        .collect();
    if !skips && !skipped.is_empty() {
        old();
        skipped.clear();
    }
    if skip_pattern(skipped.iter().map(String::as_str)).len() > MOST_SKIP {
        skipped.clear();
    }
    Some(TestsAt {
        unit: unit.to_string(),
        listed: listed.into_iter().collect(),
        tests,
        skipped,
        flags,
    })
}

/// \[ADR 0083\] Go's own summary line, `PASS` or `FAIL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Summary {
    Pass,
    Fail,
}

/// \[ADR 0083\] A test binary's output, read as it goes by: what it named failed, the times it
/// gave, and whether it ended on Go's own summary.
#[derive(Debug, Clone, Default)]
pub struct Output {
    /// `-test.v=test2json`: Go's own lines start with `\x16`, and its summary must.
    json: bool,
    line: Vec<u8>,
    /// The line being read is longer than hushrun reads.
    too_long: bool,
    /// A line was longer than hushrun reads.
    unreadable: bool,
    /// The top-level name of everything a `--- FAIL: ` named, wherever in a line.
    pub failed: BTreeSet<String>,
    /// A `--- FAIL: ` was seen.
    pub any_failure: bool,
    /// Each top-level test's time, from Go's `--- PASS`, `--- FAIL` and `--- SKIP` lines.
    pub durations: BTreeMap<String, u64>,
    /// \[C-32\] The top-level name of everything a `--- SKIP: ` line named, a subtest's included:
    /// Go prints them with `-v` and `-json` only.
    pub skipped: BTreeSet<String>,
    /// The summary, while nothing but coverage lines came after it.
    pub summary: Option<Summary>,
    /// The output ended with a whole line.
    ended_whole: bool,
}

impl Output {
    pub fn new(json: bool) -> Output {
        Output {
            json,
            ended_whole: true,
            ..Output::default()
        }
    }

    /// Reads `bytes`, the next of the binary's output.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut rest = bytes;
        while let Some(at) = rest.iter().position(|b| *b == b'\n') {
            if self.too_long {
                // The end of a line too long to read: the lines after it are read again.
                self.too_long = false;
            } else {
                self.line.extend_from_slice(&rest[..at]);
                let line = std::mem::take(&mut self.line);
                self.read_line(&line);
            }
            self.line.clear();
            rest = &rest[at + 1..];
        }
        if !self.too_long {
            if self.line.len() + rest.len() > MOST_LINE {
                self.too_long = true;
                self.unreadable = true;
                self.summary = None;
                self.line.clear();
            } else {
                self.line.extend_from_slice(rest);
            }
        }
        self.ended_whole = rest.is_empty();
    }

    /// The output ended: a last line with no newline is read for failures, and is no summary.
    pub fn end(&mut self) {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.read_line(&line);
        }
        if !self.ended_whole {
            self.summary = None;
        }
    }

    /// Whether everything it printed could be read, and ended on a whole line.
    pub fn readable(&self) -> bool {
        !self.unreadable && self.ended_whole
    }

    fn read_line(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let text = text.strip_suffix('\r').unwrap_or(&text);
        // Every `--- FAIL: ` names a failure, wherever it is: a test's output with no newline
        // before Go's line, an example's `got:`, or a test that prints Go's words itself. Reading
        // too many failures only runs a test again.
        let mut from = 0;
        while let Some(at) = text[from..].find("--- FAIL: ") {
            let start = from + at + "--- FAIL: ".len();
            self.any_failure = true;
            self.failed.insert(top_level(&text[start..]));
            from = start;
        }
        let own = text.strip_prefix('\u{16}').unwrap_or(text);
        // A skip, a subtest's indented under its test: part of the test's body didn't run.
        let indented = text.trim_start();
        let indented = indented.strip_prefix('\u{16}').unwrap_or(indented);
        if let Some(rest) = indented.strip_prefix("--- SKIP: ") {
            self.skipped.insert(top_level(rest));
        }
        for word in ["--- PASS: ", "--- FAIL: ", "--- SKIP: "] {
            if let Some(rest) = own.strip_prefix(word)
                && let Some((name, time)) = rest.split_once(" (")
                && !name.contains('/')
                && let Some(ms) = time
                    .strip_suffix("s)")
                    .and_then(|s| s.parse::<f64>().ok())
                    .filter(|s| s.is_finite() && *s >= 0.0)
                    .map(|s| (s * 1000.0).round() as u64)
            {
                let entry = self.durations.entry(top_level(name)).or_default();
                *entry = entry.saturating_add(ms);
            }
        }
        // Go's summary is its last line but coverage. A test's output with no newline may come
        // before it on the line; with `-json` Go marks it, after anything before it.
        let tail = if self.json {
            text.rsplit_once('\u{16}').map(|(_, own)| own)
        } else {
            Some(text)
        };
        let summary = tail.and_then(|tail| {
            if tail.ends_with("PASS") {
                Some(Summary::Pass)
            } else if tail.ends_with("FAIL") {
                Some(Summary::Fail)
            } else {
                None
            }
        });
        let exact = tail.is_some_and(|tail| tail == "PASS" || tail == "FAIL");
        if let Some(summary) = summary
            && (exact || !self.json)
        {
            self.summary = Some(summary);
        } else if !own.starts_with("coverage: ") {
            self.summary = None;
        }
    }
}

/// The top-level test of a name Go printed: up to the first character no Go identifier has (a
/// subtest's `/`, `-test.cpu`'s `-4`, a duplicate's `#01`, the space before the time).
fn top_level(name: &str) -> String {
    name.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// \[ADR 0083\] How a test binary ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended {
    pub exit: Option<i32>,
    pub signal: Option<i32>,
    /// Its output was read to its end.
    pub read_whole: bool,
    /// Go's test main returned from running the tests: `M.Run` wrote the `-test.memprofile`
    /// hushrun asked for, which it does only in the end it defers, after its summary. A test that
    /// calls `os.Exit`, a panic, a timeout or a signal never gets there.
    pub returned: bool,
}

/// \[ADR 0083\] Whether a test binary's run ended normally, as Go's test main ends one, so that
/// every test it ran and didn't name failed passed: `None` when it did; else \[C-37\] why not, in
/// people's words, which each test it left without a result keeps in this machine's store for
/// `hushrun why`. Normally: `M.Run` [returned](Ended::returned), its output was read to the end and
/// readable, the last line Go's own summary (marked, with `-json`) with nothing after it but
/// coverage, and
///
/// - `PASS` with exit 0, `os.Exit(0)` in a test being a panic (`-test.paniconexit0`);
/// - or `FAIL` with exit 1, a `--- FAIL: ` seen, every name a `--- FAIL: ` gave one the binary
///   listed, and no `-failfast`, which leaves tests unstarted that nothing names.
fn not_normal(at: &TestsAt, out: &Output, ended: Ended) -> Option<&'static str> {
    if !ended.returned || ended.signal.is_some() {
        return Some(ENDED_EARLY);
    }
    if !ended.read_whole
        || !out.readable()
        || !out.failed.iter().all(|name| at.listed.contains(name))
    {
        return Some(UNREAD);
    }
    let normal = match out.summary {
        Some(Summary::Pass) => ended.exit == Some(0) && at.flags.panic_on_exit0,
        Some(Summary::Fail) if at.flags.failfast => return Some(FAILFAST),
        Some(Summary::Fail) => ended.exit == Some(1) && out.any_failure,
        None => false,
    };
    (!normal).then_some(UNREAD)
}

/// \[C-37\] Why no test of a binary that didn't get back to Go's summary kept a result.
pub const ENDED_EARLY: &str = "its package's test binary ended before Go's test main returned (a panic, a timeout, os.Exit or a signal), so no test of it that didn't fail kept a result";
/// \[C-37\] Why no test of a binary run with `-failfast` that failed kept a result.
pub const FAILFAST: &str = "its package failed under -failfast, which leaves tests unstarted that go test doesn't name, so no test of it that didn't fail kept a result";
/// \[C-37\] Why no test of a binary whose output hushrun couldn't read as Go's kept a result.
pub const UNREAD: &str = "hushrun couldn't read its package's test output to Go's own summary, so no test of it that didn't fail kept a result";

/// \[ADR 0083\] Each test of a start, as its facts say it (C-39): failed when the output named it
/// failed; finished when the run [ended normally](not_normal) and hushrun didn't leave it out;
/// deselected when hushrun left it out. Its time is the one Go gave it, or 0: without `-v` Go times
/// only the tests that failed.
///
/// \[ADR 0083, amended\] A binary whose run failed while its output named only the tests that
/// failed (no `-test.v`): a test that passed can't be told from one that skipped, perhaps because
/// another failed, so no test it didn't name failed finished, and [`UNNAMED`] says why.
pub fn outcome(
    at: &TestsAt,
    out: &Output,
    ended: Ended,
) -> (Vec<super::facts::TestFacts>, Option<&'static str>) {
    let not_normal = not_normal(at, out, ended);
    let normal = not_normal.is_none();
    let unnamed = normal && out.summary == Some(Summary::Fail) && !at.flags.names_each;
    // \[C-37\] Why a test left unfinished kept no result: decided here, with its state, and kept.
    let why = not_normal.or(unnamed.then_some(UNNAMED));
    let tests = at
        .tests
        .iter()
        .map(|id| {
            let deselected = at.skipped.contains(id);
            let failed = out.failed.contains(id);
            let finished = normal && !deselected && (!unnamed || failed);
            super::facts::TestFacts {
                id: id.clone(),
                finished,
                not_kept: why
                    .filter(|_| !finished && !failed && !deselected)
                    .map(str::to_string),
                failed,
                deselected,
                duration_ms: out.durations.get(id).copied().unwrap_or(0),
                skipped: !deselected && out.skipped.contains(id),
                ..super::facts::TestFacts::default()
            }
        })
        .collect();
    (tests, unnamed.then_some(UNNAMED))
}

/// \[ADR 0083, amended\] Why a package that failed kept no pass of its other tests, which the run
/// says once and `hushrun why` says for each such test, from its row.
pub const UNNAMED: &str = "a package that failed keeps its other tests' results only when go test names each one: add -v to keep them";

/// \[ADR 0070\] The Go adapter for `go test …` (C-35); `None` for any other COMMAND.
pub fn recognized(command: &[String], _: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    Some(recognize(command)?.map(|r| Box::new(r) as Box<dyn Adapter>))
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        match self.chooses {
            Some("-run") => "go test -run",
            Some(_) => "go test -skip",
            None => "go test",
        }
    }

    fn test_file(&self) -> &'static str {
        "package"
    }

    /// \[ADR 0083\] Each test one by one, unless COMMAND chooses its tests itself; what each read is
    /// its package's.
    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: self.chooses.is_none(),
            reads: false,
        }
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let exe = at.exe()?;
        variables(self, &exe, at.cwd, (at.env)("GOFLAGS"), at.assigned)
    }

    fn plan(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let listed = list(self, at.root, at.cwd, at.assigned)?;
        let sets = units::classify(
            at.tree.paths(),
            &|path| listed.unit_files.contains(path),
            Sources::Named(&listed.named, SOURCE_KINDS),
            at.ignore,
        );
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let known = listed
            .units
            .iter()
            .map(|(unit, u)| {
                let whole = u.not_followed.is_empty() && u.own.iter().all(|f| at.tree.contains(f));
                let lists = whole
                    .then(|| {
                        super::shared::lists(
                            unit,
                            at.map.units.get(unit),
                            &shared,
                            at.tree,
                            Some(&u.reads),
                        )
                    })
                    .flatten();
                (unit.clone(), lists)
            })
            .collect();
        Ok(Some(Planning {
            sets,
            known,
            received: shared.received,
            ..Planning::default()
        }))
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        if !at.files.iter().any(|f| f.adapter == ADAPTER) {
            return Ok(None);
        }
        let mut observed = observe(self, at.files, at.root, at.cwd, at.tree, at.assigned)?;
        // \[ADR 0083\] Tests one by one, as the runner told them (C-39). A package of which the
        // runner left a test out didn't run whole. One it didn't start, reused whole, tells none:
        // the core counts the tests this machine knows of it (C-39).
        let told = super::facts::merged(at.files, ADAPTER);
        for (unit, seen) in &mut observed.units {
            if let Some(tests) = told.get(unit).and_then(|m| m.tests.clone()) {
                if tests.values().any(|t| t.deselected) {
                    seen.facts.complete = false;
                }
                seen.tests = Some(tests);
            }
        }
        let sets = units::classify(
            at.tree.paths(),
            &|path| observed.unit_files.contains(path),
            Sources::Named(&observed.named, SOURCE_KINDS),
            at.ignore,
        );
        let unit_files = Some(observed.unit_files.clone());
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = observed
            .compiled
            .iter()
            .filter_map(|(unit, compiled)| {
                let own = at.map.units.get(unit);
                let lists = super::shared::lists(unit, own, &shared, at.tree, Some(compiled))?;
                Some((unit.clone(), lists))
            })
            .collect();
        Ok(Some(Read {
            seen: observed.units,
            failures_by_unit: true,
            sets,
            planned,
            own: observed.own,
            dirs: observed.dirs,
            edges: observed.edges,
            unit_files,
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras::default()
    }

    fn why_unit(&self, asked: &str) -> String {
        if asked.ends_with("_test.go") {
            return package_of(asked);
        }
        if asked.is_empty() {
            return ".".to_string();
        }
        asked.to_string()
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        units::classify(
            at.tree.paths(),
            &|p| {
                p.ends_with("_test.go")
                    && (at.map.unit_files.contains(p) || at.map.units.contains_key(&package_of(p)))
            },
            Sources::Named(&at.map.sources, SOURCE_KINDS),
            at.ignore,
        )
    }

    fn why_is_unit(&self, at: &WhyAt<'_>, _: &Sets, unit: &str) -> bool {
        at.map.units.contains_key(unit)
            || at.shared.knows(unit)
            || at.map.unit_files.iter().any(|f| package_of(f) == unit)
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!(
            "{unit} isn't a package {check} tests as far as this machine knows: go list didn't name it in its last run here"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some(
            "its packages and what each is built from are those go list named in its last run here",
        )
    }

    fn why_asks_hints(&self, map: &super::store::Map) -> bool {
        // Go's sources are those of its last run here: with none, there is no address to ask.
        !map.sources.is_empty()
    }

    fn why_units_are_directories(&self) -> bool {
        true
    }
}

/// A Go test file's package: its directory, `.` for the root.
pub fn package_of(file: &str) -> String {
    match file.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => ".".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-32
    #[test]
    fn go_test_is_recognized_with_the_flags_that_choose_files() {
        let r = recognize(&words(
            "go test -race -tags=integration,slow -run TestX -count=1 -v ./... -args -x",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(r.build, ["-race", "-tags=integration,slow"]);
        assert_eq!(r.packages, ["./..."]);
        let r = recognize(&words("go test -C sub -mod vendor -json ."))
            .unwrap()
            .unwrap();
        assert_eq!(r.dir.as_deref(), Some("sub"));
        assert_eq!(r.build, ["-mod=vendor"]);
        assert_eq!(r.packages, ["."]);
        assert!(
            recognize(&words("go test"))
                .unwrap()
                .unwrap()
                .packages
                .is_empty()
        );
        for refused in [
            "go test -c ./x",
            "go test -exec qemu ./...",
            "go test -fuzz=Fuzz .",
            "go test -overlay=o.json ./...",
            "go test -toolexec=x ./...",
            "go test a_test.go",
            "go test -mystery ./...",
            "go test -run",
        ] {
            assert!(
                matches!(recognize(&words(refused)), Some(Err(_))),
                "{refused}"
            );
        }
        assert!(recognize(&words("go vet ./...")).is_none());
        assert!(recognize(&words("gotestsum -- ./...")).is_none());
        assert!(recognize(&words("make test")).is_none());
    }

    // covers: C-32
    #[test]
    fn goflags_keep_what_they_said_and_chain_to_their_exec() {
        let exe = Path::new("/opt/hr/hushrun");
        assert_eq!(
            goflags_with_runner("", exe).unwrap(),
            "'-exec=/opt/hr/hushrun __hushrun-test-runner go 0'"
        );
        let with = goflags_with_runner("-mod=mod '-tags=a b' -exec=qemu-x", exe).unwrap();
        assert_eq!(
            with,
            "-mod=mod '-tags=a b' -exec=qemu-x '-exec=/opt/hr/hushrun __hushrun-test-runner go 1 qemu-x'"
        );
        // Go splits it back as it was written: the last -exec wins, and names hushrun first.
        let fields = split_quoted(&with).unwrap();
        assert_eq!(fields[1], "-tags=a b");
        let exec = fields.last().unwrap().strip_prefix("-exec=").unwrap();
        assert_eq!(
            split_quoted(exec).unwrap(),
            [
                "/opt/hr/hushrun",
                "__hushrun-test-runner",
                "go",
                "1",
                "qemu-x"
            ]
        );
        // A path with a space is quoted inside the value.
        let spaced = goflags_with_runner("", Path::new("/My Tools/hushrun")).unwrap();
        let exec = split_quoted(&spaced).unwrap()[0].clone();
        assert_eq!(
            split_quoted(exec.strip_prefix("-exec=").unwrap()).unwrap()[0],
            "/My Tools/hushrun"
        );
        assert!(goflags_with_runner("'-tags=x", exe).is_err());
        assert!(goflags_with_runner("", Path::new("/it's/hushrun")).is_err());
    }

    // covers: C-32
    #[test]
    fn a_test_log_says_what_was_opened_listed_and_started() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("pkg/testdata")).unwrap();
        std::fs::write(root.join("pkg/testdata/in.txt"), "x").unwrap();
        std::fs::write(root.join("other.go"), "package other").unwrap();
        let tool = root.join("tool.sh");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let lines: Vec<String> = [
            "# test log",
            "getenv HOME",
            "open testdata/in.txt",
            "stat ../other.go",
            "open testdata",
            "open /nonexistent/file",
            "chdir ..",
            "open tool.sh",
            "stat tool.sh",
            "open /etc",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let log = read_log(&lines, &root.join("pkg"), &root);
        assert_eq!(
            log.files.iter().map(String::as_str).collect::<Vec<_>>(),
            ["other.go", "pkg/testdata/in.txt", "tool.sh"]
        );
        assert_eq!(
            log.dirs.iter().map(String::as_str).collect::<Vec<_>>(),
            ["pkg/testdata"]
        );
        if cfg!(unix) {
            assert_eq!(log.programs.len(), 1, "{log:?}");
        }
    }

    // covers: C-35, C-32
    #[cfg(unix)]
    #[test]
    fn a_program_started_by_a_relative_path_from_another_directory_is_a_program() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("pkg/sub")).unwrap();
        let tool = root.join("pkg/sub/tool");
        std::fs::write(&tool, "#!/bin/sh\ncat ../../other.go\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        // What Go 1.27's os/exec logs for `exec.Command("./tool")` with `Dir: "sub"`: the program
        // as given, then the directory it starts in, which `os.StartProcess` looks at.
        let log = |lines: &[&str]| {
            let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            read_log(&lines, &root.join("pkg"), &root)
        };
        let started = log(&["# test log", "open ./tool", "stat sub", "getenv PATH"]);
        assert_eq!(started.programs.len(), 1, "{started:?}");
        // A file of that name where the test runs, which isn't the program, changes nothing.
        std::fs::write(root.join("pkg/tool"), "data").unwrap();
        let started = log(&["# test log", "open ./tool", "stat sub"]);
        assert_eq!(started.programs.len(), 1, "{started:?}");
        // A file the test only opened, and a directory it only looked at, are no program.
        let read = log(&["# test log", "open ./tool", "stat .."]);
        assert!(read.programs.is_empty(), "{read:?}");
        assert_eq!(
            read.files.iter().map(String::as_str).collect::<Vec<_>>(),
            ["pkg/tool"]
        );
        let missing = log(&["# test log", "open ./missing", "stat sub"]);
        assert!(missing.programs.is_empty(), "{missing:?}");
        // A program the test wrote to a temporary directory, gone by the time the log is read:
        // started from that directory, or by its whole path, after exec.Cmd's /dev/null.
        let gone = root.join("gone");
        let temporary = log(&[
            "# test log",
            &format!("open {}/read.sh", gone.display()),
            "open /dev/null",
            "open ./read.sh",
            &format!("stat {}", gone.display()),
        ]);
        assert_eq!(temporary.programs.len(), 1, "{temporary:?}");
        let whole = log(&[
            "# test log",
            "open /dev/null",
            &format!("open {}/read.sh", gone.display()),
        ]);
        assert_eq!(whole.programs.len(), 1, "{whole:?}");
        // A temporary file written and read again is no program.
        let written = log(&[
            "# test log",
            &format!("open {}/out.txt", gone.display()),
            &format!("open {}/out.txt", gone.display()),
        ]);
        assert!(written.programs.is_empty(), "{written:?}");
    }

    fn os(words: &str) -> Vec<OsString> {
        words.split(' ').map(OsString::from).collect()
    }

    // covers: C-45
    #[test]
    fn a_test_binarys_arguments_say_whether_command_chose_its_tests() {
        let flags = binary_flags(&os(
            "-test.paniconexit0 -test.timeout=10m0s -test.v=test2json -test.count=1 -test.failfast=true -test.shuffle=on",
        ));
        assert_eq!(
            flags,
            BinaryFlags {
                chooses: false,
                json: true,
                names_each: true,
                failfast: true,
                panic_on_exit0: true,
                memprofile: false,
            }
        );
        for chooses in [
            "-test.run=^TestA$",
            "-test.skip=TestB",
            "-test.list=.",
            "-test.count=0",
            "-test.count=x",
            "-test.count",
            "--test.run TestA",
            "-test.paniconexit0 x -test.run=A",
        ] {
            assert!(binary_flags(&os(chooses)).chooses, "{chooses}");
        }
        assert!(!binary_flags(&os("-test.count 3")).chooses);
        // \[ADR 0083, amended\] Go names each test's outcome with -v and -json, not without.
        assert!(binary_flags(&os("-test.v=true")).names_each);
        assert!(!binary_flags(&os("-test.v=false")).names_each);
        assert!(!binary_flags(&os("-test.paniconexit0")).names_each);
        assert!(!binary_flags(&os("-test.failfast=false")).failfast);
        assert!(!binary_flags(&os("-test.v=true")).json);
        assert!(!binary_flags(&os("-test.paniconexit0=false")).panic_on_exit0);
        assert!(binary_flags(&os("-test.memprofile=m.out")).memprofile);
    }

    // covers: C-45
    #[test]
    fn a_listing_is_read_only_when_every_line_is_a_go_name() {
        let listed =
            read_listed(b"TestA\nTestAB\nBenchmarkX\nFuzzF\nExample_out\nTest\xc3\x84\n").unwrap();
        let tests: Vec<&String> = listed.iter().filter(|n| is_test(n)).collect();
        assert_eq!(tests, ["TestA", "TestAB", "FuzzF", "Example_out", "TestÄ"]);
        assert_eq!(read_listed(b"").unwrap(), Vec::<String>::new());
        // A package's init printed something: the list can't be told.
        assert!(read_listed(b"connecting to db...\nTestA\n").is_none());
        assert!(read_listed(b"TestA\n\nTestB\n").is_none());
        assert!(read_listed(b"9Test\n").is_none());
    }

    // covers: C-45
    #[test]
    fn the_skip_pattern_anchors_and_quotes_every_name() {
        assert_eq!(skip_pattern(["TestA"]), "^(TestA)$");
        assert_eq!(skip_pattern(["TestA", "Example_b"]), "^(TestA|Example_b)$");
        assert_eq!(skip_pattern(["A.b+c$"]), r"^(A\.b\+c\$)$");
    }

    fn fake_binary(main: bool) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let body: &[u8] = if main {
            b"\x7fELF...\0example.com/p_test.TestMain\0..."
        } else {
            b"\x7fELF...\0example.com/p.TestA\0..."
        };
        std::io::Write::write_all(&mut file, body).unwrap();
        file
    }

    // covers: C-45
    #[test]
    fn a_start_is_prepared_from_the_binarys_own_list() {
        let binary = fake_binary(false);
        let listing = b"TestA\nTestAB\nBenchmarkB\nExample_e\n".to_vec();
        let reuse = vec!["TestA".to_string(), "TestGone".to_string()];
        let mut asked: Vec<Vec<String>> = Vec::new();
        let mut said = 0;
        let at = prepare(
            "p",
            binary.path(),
            &os("-test.paniconexit0 -test.timeout=10m0s"),
            &reuse,
            &mut |extra: &[&str]| {
                asked.push(extra.iter().map(|s| s.to_string()).collect());
                Some(listing.clone())
            },
            &mut || said += 1,
        )
        .unwrap();
        assert_eq!(at.tests, ["TestA", "TestAB", "Example_e"]);
        // Only what the binary has is left out: a test the plan lists that is gone isn't asked.
        assert_eq!(at.skipped.iter().collect::<Vec<_>>(), ["TestA"], "{at:?}");
        assert_eq!(at.skip_arg().unwrap(), "-test.skip=^(TestA)$");
        assert_eq!(asked, [vec!["-test.list=.", "-test.skip=^$"]]);
        assert_eq!(said, 0);

        // A Go before 1.20 refuses -test.skip: it lists without, leaves nothing out, and says so.
        let at = prepare(
            "p",
            binary.path(),
            &os("-test.paniconexit0"),
            &reuse,
            &mut |extra: &[&str]| (extra.len() == 1).then(|| listing.clone()),
            &mut || said += 1,
        )
        .unwrap();
        assert!(at.skipped.is_empty() && at.skip_arg().is_none());
        assert_eq!(at.tests.len(), 3);
        assert_eq!(said, 1);

        // Nothing one by one: COMMAND chose its tests, a TestMain would run only to list them,
        // the binary can't be read or listed, or its list isn't names.
        let mut list_ok = |_: &[&str]| Some(listing.clone());
        let mut quiet = || {};
        let chose = prepare(
            "p",
            binary.path(),
            &os("-test.run=TestA"),
            &reuse,
            &mut list_ok,
            &mut quiet,
        );
        assert!(chose.is_none());
        let main = fake_binary(true);
        assert!(prepare("p", main.path(), &[], &reuse, &mut list_ok, &mut quiet).is_none());
        let gone = std::path::Path::new("/nonexistent/p.test");
        assert!(prepare("p", gone, &[], &reuse, &mut list_ok, &mut quiet).is_none());
        assert!(
            prepare(
                "p",
                binary.path(),
                &[],
                &reuse,
                &mut |_: &[&str]| None,
                &mut quiet
            )
            .is_none()
        );
        let mut noisy = |_: &[&str]| Some(b"hello from init\nTestA\n".to_vec());
        assert!(prepare("p", binary.path(), &[], &reuse, &mut noisy, &mut quiet).is_none());
    }

    fn read(json: bool, chunks: &[&[u8]]) -> Output {
        let mut out = Output::new(json);
        for chunk in chunks {
            out.feed(chunk);
        }
        out.end();
        out
    }

    // covers: C-45
    #[test]
    fn output_is_read_as_it_goes_by() {
        // Failures named wherever they are, by their top-level test: a subtest's, a duplicate's,
        // -test.cpu's suffix, after output with no newline, split across reads.
        let out = read(
            false,
            &[
                b"--- FAIL: TestA (0.01s)\n    --- FAIL: TestA/sub#01 (0.00s)\nno newline--- FA",
                b"IL: TestB-4 (1.50s)\n--- PASS: TestC (0.25s)\n--- SKIP: TestD (0.00s)\nFAIL\n",
            ],
        );
        assert_eq!(out.failed.iter().collect::<Vec<_>>(), ["TestA", "TestB"]);
        assert_eq!(out.summary, Some(Summary::Fail));
        assert!(out.readable());
        // Only Go's own lines give times: a glued one doesn't.
        assert_eq!(out.durations.get("TestA"), Some(&10));
        assert_eq!(out.durations.get("TestB"), None);
        assert_eq!(out.durations.get("TestC"), Some(&250));
        assert_eq!(out.durations.get("TestD"), Some(&0));
        // \[C-32\] A skip, a subtest's by its test, with or without -json's mark.
        assert_eq!(out.skipped.iter().collect::<Vec<_>>(), ["TestD"]);
        let out = read(
            true,
            &[b"=== RUN   TestE\n    \x16--- SKIP: TestE/one (0.00s)\n\x16--- PASS: TestE (0.00s)\n"],
        );
        assert_eq!(out.skipped.iter().collect::<Vec<_>>(), ["TestE"]);
        // The summary is the last line but coverage, after output with no newline too.
        assert_eq!(
            read(false, &[b"PASS\ncoverage: 50.0% of statements\n"]).summary,
            Some(Summary::Pass)
        );
        assert_eq!(
            read(false, &[b"no newlinePASS\n"]).summary,
            Some(Summary::Pass)
        );
        assert_eq!(read(false, &[b"PASS\nleaked goroutine\n"]).summary, None);
        assert_eq!(read(false, &[b"PASS"]).summary, None);
        assert!(!read(false, &[b"PASS"]).readable());
        // With -json, Go marks its own lines: a test's PASS isn't Go's.
        assert_eq!(read(true, &[b"\x16PASS\n"]).summary, Some(Summary::Pass));
        assert_eq!(read(true, &[b"x\x16FAIL\n"]).summary, Some(Summary::Fail));
        assert_eq!(read(true, &[b"\x16PASS\nPASS\n"]).summary, None);
        assert_eq!(read(true, &[b"PASS\n"]).summary, None);
        let out = read(
            true,
            &[b"\x16--- FAIL: TestA (0.00s)\n--- FAIL: TestB (0.00s)\n"],
        );
        assert_eq!(out.failed.len(), 2);
        // A line too long to read: nothing passes, and later lines are still read for failures.
        let long = vec![b'x'; MOST_LINE + 10];
        let out = read(false, &[&long, b"\n--- FAIL: TestZ (0.00s)\nPASS\n"]);
        assert!(!out.readable());
        assert!(out.failed.contains("TestZ"));
    }

    // covers: C-45
    #[test]
    fn a_test_passes_only_when_the_binary_ended_normally() {
        let at = TestsAt {
            unit: "p".into(),
            listed: ["TestA", "TestB", "TestC", "BenchmarkX"]
                .map(String::from)
                .into(),
            tests: ["TestA", "TestB", "TestC"].map(String::from).to_vec(),
            skipped: ["TestC".to_string()].into(),
            flags: BinaryFlags {
                panic_on_exit0: true,
                names_each: true,
                ..BinaryFlags::default()
            },
        };
        let ended = |exit: i32| Ended {
            exit: Some(exit),
            signal: None,
            read_whole: true,
            returned: true,
        };
        let facts = |out: &Output, ended: Ended| -> Vec<(String, bool, bool, bool)> {
            outcome(&at, out, ended)
                .0
                .into_iter()
                .map(|t| (t.id, t.finished, t.failed, t.deselected))
                .collect()
        };
        let row =
            |id: &str, finished, failed, deselected| (id.to_string(), finished, failed, deselected);
        let fail = read(false, &[b"--- FAIL: TestB (0.00s)\nFAIL\n"]);
        assert_eq!(
            facts(&fail, ended(1)),
            [
                row("TestA", true, false, false),
                row("TestB", true, true, false),
                row("TestC", false, false, true)
            ]
        );
        // \[ADR 0083, amended\] Where Go names only what failed (no -v), a run that failed
        // finishes only what it named, and says why; one that passed is as before.
        let unnamed = TestsAt {
            flags: BinaryFlags {
                names_each: false,
                ..at.flags
            },
            ..at.clone()
        };
        let (told, note) = outcome(&unnamed, &fail, ended(1));
        assert_eq!(
            told.iter()
                .map(|t| (t.id.as_str(), t.finished, t.failed))
                .collect::<Vec<_>>(),
            [
                ("TestA", false, false),
                ("TestB", true, true),
                ("TestC", false, false)
            ]
        );
        assert_eq!(note, Some(UNNAMED));
        // \[C-37\] Each test left unfinished, unfailed and not left out keeps why, decided here.
        let why = |told: &[super::super::facts::TestFacts]| -> Vec<Option<String>> {
            told.iter().map(|t| t.not_kept.clone()).collect()
        };
        assert_eq!(why(&told), [Some(UNNAMED.to_string()), None, None]);
        // A panic or a timeout: the binary never returned, whatever -v says.
        let early = Ended {
            returned: false,
            ..ended(2)
        };
        assert_eq!(
            why(&outcome(&at, &fail, early).0),
            [Some(ENDED_EARLY.to_string()), None, None]
        );
        let (told, note) = outcome(&unnamed, &read(false, &[b"PASS\n"]), ended(0));
        assert!(told[0].finished && told[1].finished);
        assert!(told.iter().all(|t| t.not_kept.is_none()));
        assert_eq!(note, None);
        // Any other end: the failures named, and nothing finished.
        let nothing = [
            row("TestA", false, false, false),
            row("TestB", false, true, false),
            row("TestC", false, false, true),
        ];
        for other in [
            Ended {
                returned: false,
                ..ended(1)
            },
            Ended {
                read_whole: false,
                ..ended(1)
            },
            Ended {
                signal: Some(9),
                exit: None,
                ..ended(1)
            },
            ended(2),
            ended(0),
        ] {
            assert_eq!(facts(&fail, other), nothing, "{other:?}");
        }
        // -failfast with a failure leaves tests unstarted.
        let failfast = TestsAt {
            flags: BinaryFlags {
                failfast: true,
                ..at.flags
            },
            ..at.clone()
        };
        assert_eq!(not_normal(&failfast, &fail, ended(1)), Some(FAILFAST));
        // FAIL with nothing named, or a name the binary doesn't have.
        assert!(not_normal(&at, &read(false, &[b"FAIL\n"]), ended(1)).is_some());
        assert!(
            not_normal(
                &at,
                &read(false, &[b"--- FAIL: TestGone (0.00s)\nFAIL\n"]),
                ended(1)
            )
            .is_some()
        );
        // A benchmark's failure is one the binary listed.
        assert!(
            not_normal(
                &at,
                &read(false, &[b"--- FAIL: BenchmarkX\nFAIL\n"]),
                ended(1)
            )
            .is_none()
        );
        // PASS needs exit 0, and os.Exit(0) a panic.
        let pass = read(false, &[b"PASS\n"]);
        assert!(not_normal(&at, &pass, ended(0)).is_none());
        assert!(not_normal(&at, &pass, ended(1)).is_some());
        let lax = TestsAt {
            flags: BinaryFlags::default(),
            ..at.clone()
        };
        assert!(not_normal(&lax, &pass, ended(0)).is_some());
    }

    /// Go, for the tests that prove a pattern against it: `None`, said, where it is missing,
    /// unless `HUSHRUN_TEST_TOOLS=required`.
    fn go_here() -> bool {
        let here = std::process::Command::new("go")
            .arg("version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !here {
            assert!(
                std::env::var("HUSHRUN_TEST_TOOLS").as_deref() != Ok("required"),
                "Go is missing, and HUSHRUN_TEST_TOOLS=required"
            );
            eprintln!(
                "note: skipping: Go is missing (HUSHRUN_TEST_TOOLS=required makes this fail)"
            );
        }
        here
    }

    // covers: C-45
    #[test]
    fn the_skip_pattern_leaves_out_exactly_the_tests_it_names_in_a_real_go_binary() {
        if !go_here() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("go.mod"), "module example.com/p\n\ngo 1.21\n").unwrap();
        // Names that are prefixes of others, a subtest named as a top-level test, an example
        // and a fuzz target, and a name outside ASCII.
        std::fs::write(
            root.join("p_test.go"),
            "package p\n\nimport (\n\t\"fmt\"\n\t\"testing\"\n)\n\n\
             func TestA(t *testing.T) {}\nfunc TestAB(t *testing.T) {}\nfunc TestA_B(t *testing.T) {}\n\
             func TestA2(t *testing.T) {}\nfunc TestÄ(t *testing.T) {}\n\
             func TestB(t *testing.T) { t.Run(\"TestA\", func(t *testing.T) {}) }\n\
             func FuzzA(f *testing.F) { f.Add(1); f.Fuzz(func(t *testing.T, i int) {}) }\n\
             func Example_a() {\n\tfmt.Println(\"a\")\n\t// Output: a\n}\n",
        )
        .unwrap();
        let build = std::process::Command::new("go")
            .args(["test", "-c", "-o", "p.test", "."])
            .current_dir(root)
            .env("GOTOOLCHAIN", "local")
            .env("GOFLAGS", "")
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let binary = root.join("p.test");
        let ran = |skip: &[&str]| -> BTreeSet<String> {
            let mut command = std::process::Command::new(&binary);
            command.current_dir(root).arg("-test.v");
            if !skip.is_empty() {
                command.arg(format!("-test.skip={}", skip_pattern(skip.iter().copied())));
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|l| l.strip_prefix("=== RUN   "))
                .map(str::to_string)
                .collect()
        };
        let all = ran(&[]);
        assert_eq!(all.len(), 10, "{all:?}");
        for skip in [
            &["TestA"][..],
            &["TestAB", "TestA2"],
            &["TestÄ", "Example_a", "FuzzA"],
            &["TestB"],
            &[
                "TestA",
                "TestAB",
                "TestA_B",
                "TestA2",
                "TestÄ",
                "TestB",
                "FuzzA",
                "Example_a",
            ],
        ] {
            let left: BTreeSet<String> = ran(skip);
            // Exactly the named top-level tests are gone, with their subtests and seeds.
            let want: BTreeSet<String> = all
                .iter()
                .filter(|name| {
                    let top = name.split('/').next().unwrap();
                    !skip.contains(&top)
                })
                .cloned()
                .collect();
            assert_eq!(left, want, "{skip:?}");
        }
    }
}
