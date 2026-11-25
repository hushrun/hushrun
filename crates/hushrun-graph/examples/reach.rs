//! What each test of one test binary reaches, by the static map (ADR 0145): a measure for people
//! changing tests or code, which hushrun itself never runs.
//!
//! ```text
//! rust-analyzer scip <checkout> --output /tmp/index.scip
//! cargo test -p hushrun --test units_gradle -- --list --format terse | sed 's/: test$//' > /tmp/ids
//! cargo run --release -p hushrun-graph --example reach -- \
//!     --root <checkout> --index /tmp/index.scip --tests /tmp/ids \
//!     --own crates/hushrun/tests/units_gradle.rs \
//!     --show crates/hushrun/src/units/ --why crates/hushrun/src/units/cargo.rs
//! ```
//!
//! - `--root`: the checkout, the tree rust-analyzer indexed (a changed file makes the map stale).
//! - `--index`: rust-analyzer's SCIP index of it.
//! - `--tests`: one test id per line, as the binary lists them (`module::name`).
//! - `--own`: the test binary's crate root (`crates/hushrun/tests/units_gradle.rs`). The crate is
//!   that file and the modules its `mod` declarations reach, `#[path]` included. An entry that is
//!   no crate root is a path prefix: every file under it is the crate's.
//! - `--linked`: what the binary is built from. By default, its own crate and every workspace
//!   library's crate, as the build's dep-info gives an integration test; never another test's or
//!   binary's files. Given, it is path prefixes, and `!` leaves one out. A library's
//!   `#[cfg(test)]` module shows up here, though a build of another crate doesn't link it.
//! - `--show`: prints, for each test, the files it reaches under these prefixes.
//! - `--why`: prints, for each test, the path of items that brings in each of these files.
//! - `--changed`: counts the tests that reach each of these files.
//!
//! It prints each test's fallback reason (C-152), then
//! `tests <n>, fallback <n>, median reach <n> of <n> linked files`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_graph::external::{Macro, Macros};
use hushrun_graph::graph::{Graph, Inputs, Layout};

fn main() {
    if let Err(err) = run() {
        eprintln!("reach: {err}");
        std::process::exit(2);
    }
}

/// The value of each `--flag value`.
fn flags() -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let name = flag
            .strip_prefix("--")
            .ok_or_else(|| format!("`{flag}` isn't a flag"))?;
        let value = args
            .next()
            .ok_or_else(|| format!("`{flag}` needs a value"))?;
        out.insert(name.to_string(), value);
    }
    Ok(out)
}

fn list(flags: &BTreeMap<String, String>, name: &str) -> Vec<String> {
    flags
        .get(name)
        .map(|v| {
            v.split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `path` is under one of `prefixes` and none of its `!` ones.
fn under(path: &str, prefixes: &[String]) -> bool {
    let (out, ins): (Vec<&String>, Vec<&String>) =
        prefixes.iter().partition(|p| p.starts_with('!'));
    ins.iter().any(|p| path.starts_with(p.as_str()))
        && !out.iter().any(|p| path.starts_with(&p[1..]))
}

fn run() -> Result<(), String> {
    let flags = flags()?;
    let need = |name: &str| {
        flags
            .get(name)
            .cloned()
            .ok_or_else(|| format!("--{name} is needed"))
    };
    let root = PathBuf::from(need("root")?);
    let index = std::fs::read(need("index")?).map_err(|e| format!("the index: {e}"))?;
    let index = hushrun_graph::scip::parse(&index).map_err(|e| format!("the index: {e:?}"))?;
    let ids: Vec<String> = std::fs::read_to_string(need("tests")?)
        .map_err(|e| format!("the tests: {e}"))?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    let own_prefixes = list(&flags, "own");
    let linked_prefixes = list(&flags, "linked");
    let workspace = Workspace::of(&root)?;
    let sources = sources(&root)?;
    let cache: RefCell<BTreeMap<PathBuf, Macros>> = RefCell::new(BTreeMap::new());
    let external = |package: &str, version: &str, name: &str| -> Option<Macro> {
        let dir = workspace
            .dirs
            .get(&(package.to_string(), version.to_string()))?;
        cache
            .borrow_mut()
            .entry(dir.clone())
            .or_insert_with(|| Macros::of_package(dir))
            .free_names(name)
    };
    let started = std::time::Instant::now();
    let graph = Graph::build(&Inputs {
        sources: &sources,
        index: &index,
        prefix: "",
        packages: &workspace.packages,
        proc_macros: &workspace.proc_macros,
        binaries: &workspace.binaries,
        external: &external,
        layout: Some(&workspace.layout),
    });
    eprintln!("map built in {:?}: {:?}", started.elapsed(), graph.size());
    // A crate root names its crate; anything else is a prefix of the crate's files.
    let mut own: BTreeSet<String> = BTreeSet::new();
    for entry in &own_prefixes {
        let files = graph.crate_files(entry);
        if files.is_empty() {
            own.extend(
                sources
                    .keys()
                    .filter(|k| k.starts_with(entry.as_str()))
                    .cloned(),
            );
        } else {
            own.extend(files);
        }
    }
    let linked: BTreeSet<String> = if linked_prefixes.is_empty() {
        let mut linked = own.clone();
        for library in workspace.layout.libraries.values() {
            linked.extend(graph.crate_files(library));
        }
        linked
    } else {
        sources
            .keys()
            .filter(|k| under(k, &linked_prefixes) || own.contains(*k))
            .cloned()
            .collect()
    };
    let show = list(&flags, "show");
    let why = list(&flags, "why");
    let changed = list(&flags, "changed");
    let mut fallbacks = 0;
    let mut sizes: Vec<usize> = Vec::new();
    let mut selected: BTreeMap<&str, usize> = BTreeMap::new();
    for id in &ids {
        match graph.reach(id, &own, &linked) {
            Ok(reached) => {
                sizes.push(reached.files.len());
                if !show.is_empty() {
                    let shown: Vec<&String> = reached
                        .files
                        .iter()
                        .filter(|f| show.iter().any(|p| f.starts_with(p.as_str())))
                        .collect();
                    println!("{id}: {} files; {shown:?}", reached.files.len());
                }
                for t in &reached.trusted {
                    println!("{id}: trusts {t}");
                }
                for file in &why {
                    if let Some(path) = graph.why_reached(id, &own, &linked, file) {
                        println!("{id}: {file} ← {path}");
                    }
                }
                for file in &changed {
                    if reached.files.contains(file) {
                        *selected.entry(file).or_default() += 1;
                    }
                }
            }
            Err(reason) => {
                fallbacks += 1;
                println!("{id}: falls back: {reason}");
            }
        }
    }
    sizes.sort_unstable();
    println!(
        "tests {}, fallback {fallbacks}, median reach {} of {} linked files",
        ids.len(),
        sizes.get(sizes.len() / 2).copied().unwrap_or(0),
        linked.len()
    );
    for file in &changed {
        println!(
            "change to {file}: {} narrowed tests reach it (+{fallbacks} fallbacks run)",
            selected.get(file.as_str()).copied().unwrap_or(0)
        );
    }
    Ok(())
}

/// Every tracked `.rs` file of the checkout, by its path.
fn sources(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let out = Command::new("git")
        .args(["ls-files", "*.rs"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    let mut sources = BTreeMap::new();
    for path in String::from_utf8_lossy(&out.stdout).lines() {
        if let Ok(text) = std::fs::read_to_string(root.join(path)) {
            sources.insert(path.to_string(), text);
        }
    }
    Ok(sources)
}

/// What `cargo metadata` says of the workspace, as the map takes it.
struct Workspace {
    packages: BTreeSet<String>,
    proc_macros: BTreeSet<String>,
    binaries: BTreeSet<String>,
    layout: Layout,
    /// Each package's folder, by its name and version, for macros of other crates.
    dirs: BTreeMap<(String, String), PathBuf>,
}

impl Workspace {
    fn of(root: &Path) -> Result<Workspace, String> {
        let out = Command::new("cargo")
            .args(["metadata", "--format-version", "1"])
            .current_dir(root)
            .output()
            .map_err(|e| format!("cargo metadata: {e}"))?;
        let meta: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
        let root = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
        let relative = |path: &str| -> Option<String> {
            let path = std::fs::canonicalize(path).ok()?;
            Some(
                path.strip_prefix(&root)
                    .ok()?
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        let strings = |v: &serde_json::Value| -> Vec<String> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        };
        let members: BTreeSet<String> = strings(&meta["workspace_members"]).into_iter().collect();
        let mut ws = Workspace {
            packages: BTreeSet::new(),
            proc_macros: BTreeSet::new(),
            binaries: BTreeSet::new(),
            layout: Layout::default(),
            dirs: BTreeMap::new(),
        };
        let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for package in meta["packages"].as_array().into_iter().flatten() {
            let field = |key: &str| package[key].as_str().unwrap_or_default().to_string();
            let name = field("name");
            let manifest = PathBuf::from(field("manifest_path"));
            let dir = manifest.parent().unwrap_or(Path::new("")).to_path_buf();
            ws.dirs
                .insert((name.clone(), field("version")), dir.clone());
            if !members.contains(&field("id")) {
                continue;
            }
            ws.packages.insert(name.clone());
            let folder = relative(&dir.to_string_lossy()).unwrap_or_default();
            let folder = if folder.is_empty() {
                folder
            } else {
                format!("{folder}/")
            };
            ws.layout.folders.push((folder, name.clone()));
            for target in package["targets"].as_array().into_iter().flatten() {
                let kinds = strings(&target["kind"]);
                let src = target["src_path"].as_str().unwrap_or_default();
                if kinds.iter().any(|k| k.contains("lib") || k == "proc-macro")
                    && let Some(src) = relative(src)
                {
                    ws.layout.libraries.insert(name.clone(), src);
                }
                if kinds.iter().any(|k| k == "bin") {
                    ws.binaries
                        .insert(target["name"].as_str().unwrap_or_default().to_string());
                }
                if kinds.iter().any(|k| k == "proc-macro") {
                    ws.proc_macros.insert(name.clone());
                }
            }
            for dep in package["dependencies"].as_array().into_iter().flatten() {
                if dep["path"].is_string() {
                    deps.entry(name.clone())
                        .or_default()
                        .insert(dep["name"].as_str().unwrap_or_default().to_string());
                }
            }
        }
        // What each package sees: the workspace packages it depends on, of any kind, closed.
        for name in &ws.packages {
            let mut seen = BTreeSet::new();
            let mut stack: Vec<String> = deps.get(name).into_iter().flatten().cloned().collect();
            while let Some(next) = stack.pop() {
                if seen.insert(next.clone()) {
                    stack.extend(deps.get(&next).into_iter().flatten().cloned());
                }
            }
            ws.layout.sees.insert(name.clone(), seen);
        }
        Ok(ws)
    }
}
