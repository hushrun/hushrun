//! \[ADR 0145\] Rust's tests one by one (C-146, C-152): each test's own list, from the static map
//! of what it can reach ([`hushrun_graph`]), made from rust-analyzer's index of the workspace.
//!
//! A test's list is what its test binary read (its key's files, C-31), less the `.rs` sources of
//! its own crate, of the workspace libraries it may link and of its package's other targets that
//! the map shows it reaches nothing of. Every other file its binary read stays: a build script's,
//! a procedural macro's, a file a build script watches, every file that isn't a `.rs` source. What
//! its binary's key holds of what isn't a file (`env`, `scripts`) its key holds too. A test the map
//! can't narrow (C-147 to C-151 say when), or a binary built from a source the map lacks, is taken
//! to have read what its binary read, and the run says why once.
//!
//! The index is rust-analyzer's (`rust-analyzer scip`), started with COMMAND's toolchain and
//! features, and kept in `<cache dir>/units/rust-analyzer/` by the SHA-256 of what it is made
//! from: rust-analyzer's version, its configuration and every file of the workspace in the scope.
//! A plan and the reading after COMMAND share it, and a later run on the same files reads it
//! without starting rust-analyzer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hushrun_core::digest::Digest;
use hushrun_core::units::Tree;
use hushrun_graph::external::{Macro, Macros};
use hushrun_graph::graph::{Graph, Inputs};

use super::adapter::TestLists;
use super::cargo::{Recognized, Workspace};

/// The longest rust-analyzer may take to index a workspace before every test falls back.
const INDEX_BUDGET: Duration = Duration::from_secs(30 * 60);
/// How many indexes the cache keeps, newest first.
const KEPT: usize = 4;
/// What the run says names the fix when rust-analyzer can't be started.
pub const INSTALL: &str = "rustup component add rust-analyzer";

/// One test binary, as the map needs it.
pub struct Binary<'a> {
    /// What it read: its key's files.
    pub reads: &'a BTreeSet<String>,
    /// Its own crate's files.
    pub own: &'a BTreeSet<String>,
    /// The `.rs` files its tests' lists may leave out.
    pub mappable: &'a BTreeSet<String>,
    /// The ids of the tests to map.
    pub tests: Vec<String>,
}

/// What mapping came to: each test's list or why not, and what the run says once.
#[derive(Debug, Default)]
pub struct Mapped {
    pub lists: TestLists,
    pub notes: Vec<String>,
    /// \[C-151\] Of each unit's tests, those that start a program through a marked function, with
    /// the functions, for people.
    pub trusted: BTreeMap<String, BTreeMap<String, String>>,
}

/// \[C-146\] Each test's list of each of `binaries`, by its unit. \[C-152\] When rust-analyzer is
/// missing, fails, takes longer than [`INDEX_BUDGET`] or gives an index that doesn't hold a
/// binary's sources, those tests have no list of their own, and a note says why.
pub fn lists(
    recognized: &Recognized,
    root: &Path,
    tree: &Tree,
    workspace: &Workspace,
    binaries: &BTreeMap<String, Binary<'_>>,
    assigned: &[(String, String)],
) -> Mapped {
    let mut out = Mapped::default();
    if binaries.values().all(|b| b.tests.is_empty()) {
        return out;
    }
    let started = Instant::now();
    let graph = match graph(recognized, root, tree, workspace, binaries, assigned) {
        Ok(graph) => graph,
        Err(why) => {
            out.notes.push(format!(
                "each Rust test is taken to have read what its test binary read: {why}"
            ));
            for (unit, b) in binaries {
                out.lists.insert(
                    unit.clone(),
                    b.tests
                        .iter()
                        .map(|id| (id.clone(), Err(why.clone())))
                        .collect(),
                );
            }
            return out;
        }
    };
    let mut fell_back: Vec<(String, String)> = Vec::new();
    let mut trusted_tests: Vec<String> = Vec::new();
    let mut total = 0usize;
    for (unit, b) in binaries {
        let mut tests: BTreeMap<String, Result<BTreeSet<String>, String>> = BTreeMap::new();
        let unmapped = b
            .mappable
            .iter()
            .chain(b.own.iter())
            .filter(|f| f.ends_with(".rs"))
            .find_map(|f| graph.unmapped(f).map(|why| (f.clone(), why.to_string())));
        let linked: BTreeSet<String> = b.mappable.union(b.own).cloned().collect();
        for id in &b.tests {
            total += 1;
            let list = match &unmapped {
                Some((file, why)) => Err(format!(
                    "its test binary is built from {file}, which the static map lacks: {why}"
                )),
                None => graph.reach(id, b.own, &linked).map(|reached| {
                    if !reached.trusted.is_empty() {
                        trusted_tests.push(format!("{unit} {id}"));
                        out.trusted
                            .entry(unit.clone())
                            .or_default()
                            .insert(id.clone(), reached.trusted.join(", "));
                    }
                    // What its binary read, less what it may leave out and doesn't reach.
                    b.reads
                        .iter()
                        .filter(|f| !b.mappable.contains(*f) || reached.files.contains(*f))
                        .cloned()
                        .collect()
                }),
            };
            if let Err(why) = &list {
                fell_back.push((format!("{unit} {id}"), why.clone()));
            }
            tests.insert(id.clone(), list);
        }
        out.lists.insert(unit.clone(), tests);
    }
    if let Some((test, why)) = fell_back.first() {
        out.notes.push(format!(
            "{} of {} Rust tests are taken to have read what their test binary read, as the static map can't narrow them: {test}: {why}",
            fell_back.len(),
            total
        ));
    }
    // \[C-151\] What the marker made the map trust, once.
    if let Some(first) = trusted_tests.first()
        && let Some(by) = out.trusted.values().flat_map(|t| t.values()).next()
    {
        out.notes.push(format!(
            "{} of {total} Rust tests start programs through a function marked `{}` ({by}), which the static map takes to start no binary of the workspace, as that function's own code must make sure of: {first}",
            trusted_tests.len(),
            hushrun_graph::items::MARKER,
        ));
    }
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        tests = total,
        fell_back = fell_back.len(),
        "the static map of Rust tests"
    );
    out
}

/// The process's map, by the key of its index: a plan and the reading after COMMAND share it.
static GRAPH: Mutex<Option<(Digest, Arc<Graph>)>> = Mutex::new(None);

/// The static map of the workspace: from the cached index of these files, or rust-analyzer's.
fn graph(
    recognized: &Recognized,
    root: &Path,
    tree: &Tree,
    workspace: &Workspace,
    binaries: &BTreeMap<String, Binary<'_>>,
    assigned: &[(String, String)],
) -> Result<Arc<Graph>, String> {
    let config = config(recognized);
    let version = version(recognized, &workspace.root, assigned)?;
    let in_workspace: Vec<&str> = tree
        .paths()
        .filter(|p| p.starts_with(&workspace.prefix))
        .collect();
    let key = Digest::of(
        format!(
            "hushrun.rust-map/v1\n{version}\n{config}\n{}",
            tree.digest(in_workspace.iter().copied()).hex()
        )
        .as_bytes(),
    );
    if let Ok(held) = GRAPH.lock()
        && let Some((k, graph)) = held.as_ref()
        && *k == key
    {
        return Ok(graph.clone());
    }
    let bytes = index(recognized, workspace, assigned, &key, &config)?;
    let index = hushrun_graph::scip::parse(&bytes).map_err(|err| err.to_string())?;
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    for b in binaries.values() {
        for file in b.mappable.iter().chain(b.own.iter()) {
            if file.ends_with(".rs")
                && !sources.contains_key(file)
                && let Ok(text) = std::fs::read_to_string(root.join(file))
            {
                sources.insert(file.clone(), text);
            }
        }
    }
    let macros: Mutex<BTreeMap<PathBuf, Arc<Macros>>> = Mutex::new(BTreeMap::new());
    let external = |package: &str, version: &str, name: &str| -> Option<Macro> {
        let dir = workspace
            .external
            .get(&(package.to_string(), version.to_string()))?;
        let read = {
            let mut held = macros.lock().ok()?;
            held.entry(dir.clone())
                .or_insert_with(|| Arc::new(Macros::of_package(dir)))
                .clone()
        };
        read.free_names(name)
    };
    let graph = Arc::new(Graph::build(&Inputs {
        sources: &sources,
        index: &index,
        prefix: &workspace.prefix,
        packages: &workspace.packages,
        proc_macros: &workspace.proc_macros,
        binaries: &workspace.binaries,
        external: &external,
    }));
    if let Ok(mut held) = GRAPH.lock() {
        *held = Some((key, graph.clone()));
    }
    Ok(graph)
}

/// rust-analyzer's configuration for COMMAND's build: its features and its first target.
fn config(recognized: &Recognized) -> String {
    let mut features: Vec<String> = Vec::new();
    let mut all = false;
    let mut no_default = false;
    let mut target: Option<String> = recognized.targets.first().cloned();
    let mut words = recognized.build.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--all-features" => all = true,
            "--no-default-features" => no_default = true,
            "-F" | "--features" => {
                if let Some(value) = words.next() {
                    features.extend(
                        value
                            .split([',', ' '])
                            .filter(|f| !f.is_empty())
                            .map(str::to_string),
                    );
                }
            }
            "--target" => target = target.or_else(|| words.next().cloned()),
            _ => {}
        }
    }
    let mut cargo = serde_json::Map::new();
    cargo.insert(
        "features".into(),
        if all {
            serde_json::Value::from("all")
        } else {
            serde_json::Value::from(features)
        },
    );
    cargo.insert("noDefaultFeatures".into(), no_default.into());
    if let Some(target) = target {
        cargo.insert("target".into(), target.into());
    }
    serde_json::json!({ "cargo": cargo }).to_string()
}

/// rust-analyzer with COMMAND's toolchain, for a start in `cwd`.
fn rust_analyzer(
    recognized: &Recognized,
    cwd: &Path,
    assigned: &[(String, String)],
) -> std::process::Command {
    let mut command = super::command::tool("rust-analyzer", cwd, assigned);
    if let Some(toolchain) = &recognized.toolchain {
        command.env("RUSTUP_TOOLCHAIN", toolchain);
    }
    command
}

/// rust-analyzer's version, which keys its index; `Err` why it can't be started.
fn version(
    recognized: &Recognized,
    cwd: &Path,
    assigned: &[(String, String)],
) -> Result<String, String> {
    let output = rust_analyzer(recognized, cwd, assigned)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => format!("rust-analyzer isn't installed (`{INSTALL}`)"),
            _ => format!("rust-analyzer can't be started ({err}; `{INSTALL}`)"),
        })?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        let said = said
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        return Err(format!(
            "rust-analyzer isn't installed for this toolchain (`{INSTALL}`){}",
            if said.is_empty() {
                String::new()
            } else {
                format!(": {said}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The index's bytes, from the cache or a run of `rust-analyzer scip`.
fn index(
    recognized: &Recognized,
    workspace: &Workspace,
    assigned: &[(String, String)],
    key: &Digest,
    config: &str,
) -> Result<Vec<u8>, String> {
    let dir = crate::context::cache_dir()
        .map(|cache| cache.join("units").join("rust-analyzer"))
        .ok_or_else(|| "hushrun has no cache directory for rust-analyzer's index".to_string())?;
    let path = dir.join(format!("{}.scip", key.hex()));
    if let Ok(bytes) = std::fs::read(&path) {
        return Ok(bytes);
    }
    std::fs::create_dir_all(&dir).map_err(|err| format!("can't make {} ({err})", dir.display()))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let partial = dir.join(format!(".{}-{nanos}.scip", std::process::id()));
    let config_path = dir.join(format!(".{}-{nanos}.json", std::process::id()));
    std::fs::write(&config_path, config)
        .map_err(|err| format!("can't write rust-analyzer's configuration ({err})"))?;
    let started = Instant::now();
    let child = rust_analyzer(recognized, &workspace.root, assigned)
        .arg("scip")
        .arg(&workspace.root)
        .arg("--output")
        .arg(&partial)
        .arg("--config-path")
        .arg(&config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let result = match child {
        Err(err) => Err(format!(
            "rust-analyzer can't be started ({err}; `{INSTALL}`)"
        )),
        Ok(child) => wait(child, INDEX_BUDGET),
    };
    let _ = std::fs::remove_file(&config_path);
    if let Err(why) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(why);
    }
    let bytes =
        std::fs::read(&partial).map_err(|err| format!("rust-analyzer wrote no index ({err})"))?;
    let _ = std::fs::rename(&partial, &path);
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        bytes = bytes.len(),
        "rust-analyzer indexed the workspace"
    );
    sweep(&dir);
    Ok(bytes)
}

/// Waits for rust-analyzer, at most `budget`; `Err` why its index can't be used.
fn wait(mut child: std::process::Child, budget: Duration) -> Result<(), String> {
    let stderr = child.stderr.take();
    // Its log goes nowhere but the last lines, read as it writes, so that it never blocks.
    let tail = std::thread::spawn(move || {
        let mut lines: Vec<String> = Vec::new();
        if let Some(stderr) = stderr {
            use std::io::BufRead as _;
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                lines.push(line);
                if lines.len() > 20 {
                    lines.remove(0);
                }
            }
        }
        lines
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let lines = tail.join().unwrap_or_default();
                let last = lines.iter().rev().find(|l| !l.trim().is_empty()).cloned();
                return Err(format!(
                    "rust-analyzer couldn't index the workspace ({status}{})",
                    last.map(|l| format!(": {}", l.trim())).unwrap_or_default()
                ));
            }
            Ok(None) if started.elapsed() > budget => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "rust-analyzer took longer than {} minutes to index the workspace",
                    budget.as_secs() / 60
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(err) => return Err(format!("can't wait for rust-analyzer ({err})")),
        }
    }
}

/// Keeps the [`KEPT`] newest indexes, and removes what a stopped run left.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let day = Duration::from_secs(24 * 60 * 60);
    let mut kept = 0;
    for (modified, path) in found {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with('.') {
            if modified.elapsed().is_ok_and(|age| age > day) {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        kept += 1;
        if kept > KEPT {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-152
    #[test]
    fn rust_analyzer_is_configured_with_commands_features_and_target() {
        let r = super::super::cargo::recognize(&words(
            "cargo nextest run --workspace --features a,b -F c --no-default-features --target x86_64-unknown-linux-gnu",
        ))
        .unwrap();
        let said: serde_json::Value = serde_json::from_str(&config(&r)).unwrap();
        assert_eq!(
            said["cargo"]["features"],
            serde_json::json!(["a", "b", "c"])
        );
        assert_eq!(said["cargo"]["noDefaultFeatures"], true);
        assert_eq!(said["cargo"]["target"], "x86_64-unknown-linux-gnu");
        let r = super::super::cargo::recognize(&words("cargo nextest run --all-features")).unwrap();
        let said: serde_json::Value = serde_json::from_str(&config(&r)).unwrap();
        assert_eq!(said["cargo"]["features"], "all");
    }

    // covers: C-152
    #[test]
    fn a_missing_rust_analyzer_falls_back_every_test_and_says_how_to_install_it() {
        let r = super::super::cargo::recognize(&words("cargo nextest run")).unwrap();
        let empty = tempfile::tempdir().unwrap();
        // No rust-analyzer on this `PATH`.
        let assigned = vec![("PATH".to_string(), empty.path().display().to_string())];
        let reads = BTreeSet::from(["src/lib.rs".to_string()]);
        let binaries = BTreeMap::from([(
            "src/lib.rs".to_string(),
            Binary {
                reads: &reads,
                own: &reads,
                mappable: &reads,
                tests: vec!["tests::t".to_string()],
            },
        )]);
        let tree = Tree::default();
        let workspace = Workspace {
            root: empty.path().to_path_buf(),
            ..Workspace::default()
        };
        let mapped = lists(&r, empty.path(), &tree, &workspace, &binaries, &assigned);
        assert_eq!(mapped.notes.len(), 1, "{:?}", mapped.notes);
        assert!(mapped.notes[0].contains(INSTALL), "{}", mapped.notes[0]);
        let why = mapped.lists["src/lib.rs"]["tests::t"].as_ref().unwrap_err();
        assert!(why.contains("rust-analyzer"), "{why}");
    }
}
