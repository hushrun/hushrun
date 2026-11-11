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

use hushrun_core::units::{Facts, NotFollowed, Tree};

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
    let file = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let file = file.strip_suffix(".exe").unwrap_or(file);
    if file != "go" || command.get(1).map(String::as_str) != Some("test") {
        return None;
    }
    let mut out = Recognized {
        go: program.clone(),
        dir: None,
        build: Vec::new(),
        packages: Vec::new(),
    };
    let mut words = command[2..].iter();
    let mut first = true;
    while let Some(word) = words.next() {
        let was_first = std::mem::replace(&mut first, false);
        if word == "-args" || word == "--args" {
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
) -> Result<Vec<(String, OsString)>, String> {
    let output = go(recognized, cwd)
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

fn go(recognized: &Recognized, cwd: &Path) -> Command {
    let mut command = Command::new(&recognized.go);
    command.current_dir(cwd);
    // hushrun's own variables never reach the go commands it asks.
    command.env_remove(runner::UNITS_VAR);
    command
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
pub fn list(recognized: &Recognized, root: &Path, cwd: &Path) -> Result<Listed, String> {
    let mut command = go(recognized, cwd);
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
) -> Result<Observed, String> {
    let listed = list(recognized, root, cwd)?;
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
                },
                reads,
                duration_ms,
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
}
