//! \[C-147, C-148, C-149, C-150, C-151\] The static map: every item of the workspace's sources, an
//! edge from each to each item it can reach, and what makes a test that reaches it fall back.
//!
//! An edge is an occurrence the index resolves (a call, a path, a type, a method, a macro), plus
//! what the index can't show, counted on the safe side:
//!
//! - \[C-148\] A trait's method, or its associated const or type, reaches the same of every
//!   `impl` of that trait in the workspace: a call through `dyn Trait`, a generic bound or the trait
//!   resolves to the trait's own. A type reaches its definition and the head of every `impl` that
//!   names it, and the whole of each `impl` of a trait from outside the workspace (`Drop`,
//!   `Display`, an operator, `From`), which the language and other crates call without a name.
//! - \[C-149\] A function's address, or a closure, is an occurrence like a call: where the value
//!   goes is reached through the item that holds it. What no item reaches but that runs all the
//!   same (`#[used]`, `#[no_mangle]`, `#[export_name]`, `#[link_section]`, `#[global_allocator]`,
//!   `#[panic_handler]`, `#[ctor]`, `#[dtor]`, `#[distributed_slice]`, and a module-level
//!   invocation of a macro from outside the workspace, such as `inventory::submit!`) is reached by
//!   every test of a binary built from its file.
//! - \[C-150\] A macro reaches its definition. A `macro_rules!` body of the workspace has no
//!   occurrence, so each of its names reaches every item of that name; one of another crate, each
//!   of its free names ([`crate::external`]). A procedural macro of the workspace can't be followed:
//!   a test that reaches one falls back. One from outside the workspace counts every item of the
//!   module it is invoked in, and every item that module imports by `*`, but for the crates
//!   [`HYGIENIC`] names. An identifier, a word of an attribute's string, or a `{name}` of a macro's
//!   string that the index resolves to nothing reaches every item of that name.
//! - \[C-151\] An item that can start a workspace binary makes a test that reaches it fall back:
//!   `env!("CARGO_BIN_EXE_…")`, `std::env::current_exe`, `cargo_bin`, and `Command::new` of anything
//!   but a string literal that names no binary of the workspace and holds no path, unless it is
//!   in a function marked `#[doc(alias = "hushrun::starts_no_workspace_binary")]`, whose own code
//!   must make sure of it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::ops::Range;

use crate::items::{self, Class, File, Kind};
use crate::scip;
use crate::symbol::{self, Last, Symbol};

/// \[C-150\] Crates of procedural macros whose output names nothing of the place they're invoked
/// but through absolute paths and the tokens they were given: their derives and attributes count
/// what the index resolves of those tokens, and the names it doesn't, as any code does. Every other
/// crate's procedural macros count the whole of the module they're invoked in.
pub const HYGIENIC: &[&str] = &[
    "serde_derive",
    "thiserror-impl",
    "clap_derive",
    "async-trait",
    "tokio-macros",
    "tracing-attributes",
    "zeroize_derive",
    "der_derive",
];

/// \[C-150\] Crates whose `macro_rules!` bodies call methods only on their own crate's types, and
/// on what they're given through their own traits: a method's name in their bodies counts nothing,
/// while a free name still counts every item of that name.
pub const OWN_METHODS: &[&str] = &[
    "anyhow",
    "serde_json",
    "tracing",
    "tracing-core",
    "log",
    "proptest",
    "rusqlite",
    "thiserror",
];

/// \[C-149\] Attributes that make an item run, or be used, with no item that names it.
pub const ROOT_ATTRS: &[&str] = &[
    "used",
    "no_mangle",
    "export_name",
    "link_section",
    "global_allocator",
    "panic_handler",
    "ctor",
    "dtor",
    "distributed_slice",
    "unsafe",
];

/// What the map is made from.
pub struct Inputs<'a> {
    /// Each `.rs` source of the workspace, by its path from the repository's root.
    pub sources: &'a BTreeMap<String, String>,
    pub index: &'a scip::Index,
    /// What the index's paths start with, from the repository's root: the workspace's folder,
    /// with a `/`, or empty when the workspace is the repository's root.
    pub prefix: &'a str,
    /// The packages of the workspace, by the names the index's symbols give them.
    pub packages: &'a BTreeSet<String>,
    /// Of those, the procedural macro packages.
    pub proc_macros: &'a BTreeSet<String>,
    /// The names of the workspace's binary targets.
    pub binaries: &'a BTreeSet<String>,
    /// \[C-150\] The free names of a `macro_rules!` macro of a crate outside the workspace, by its
    /// package, version and name; `None` when it isn't one that can be read.
    pub external: &'a dyn Fn(&str, &str, &str) -> Option<crate::external::Macro>,
    /// \[C-150\] The workspace's packages, for what a name can resolve to; `None`: anything.
    pub layout: Option<&'a Layout>,
}

/// \[C-150\] The workspace's packages as name resolution sees them: a crate's code names items of
/// its own crate, of its package's library, and of the libraries of the packages it depends on
/// (of any kind, closed), never of a crate that depends on it.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// Each package's folder from the repository's root, with a `/` (empty for the root), and
    /// its name.
    pub folders: Vec<(String, String)>,
    /// Each package's library's root file, when it has one.
    pub libraries: BTreeMap<String, String>,
    /// Each package's dependencies in the workspace, closed, itself not included.
    pub sees: BTreeMap<String, BTreeSet<String>>,
}

impl Layout {
    /// The package a file is of: the one whose folder holds it, the deepest.
    pub fn package_of(&self, path: &str) -> Option<&str> {
        self.folders
            .iter()
            .filter(|(folder, _)| path.starts_with(folder.as_str()))
            .max_by_key(|(folder, _)| folder.len())
            .map(|(_, name)| name.as_str())
    }
}

/// One item: its file and its index there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Node {
    file: u32,
    item: u32,
}

/// One file: its path, what it was split into, and its occurrences.
struct Entry {
    path: String,
    split: File,
    /// The first node of its items.
    base: u32,
    /// Its occurrences by start: byte range, symbol, whether a definition.
    occurrences: Vec<(Range<usize>, u32, bool)>,
}

/// The static map of a workspace.
pub struct Graph {
    files: Vec<Entry>,
    nodes: Vec<Node>,
    edges: Vec<Vec<u32>>,
    /// \[C-151, C-150\] Why a test that reaches the node falls back.
    flags: Vec<Option<String>>,
    /// \[C-149\] Nodes every test of a binary built from their file reaches.
    roots: Vec<u32>,
    /// \[C-151\] Nodes whose `Command::new` the map takes to start no workspace binary, as their
    /// function's marker says.
    trusted: Vec<bool>,
    /// Each test function that may be one, by the id libtest names it with (`a::tests::t`).
    fns_by_path: HashMap<String, Vec<u32>>,
    /// \[C-152\] Sources that couldn't be split or have no document in the index, and why.
    unmapped: BTreeMap<String, String>,
    by_path: HashMap<String, usize>,
}

/// \[C-152\] Why a test falls back to its binary's reads.
pub type Fallback = String;

/// What a test reaches: the sources, and \[C-151\] each marked function it reaches whose
/// `Command::new` the map trusted, for people: its file, line and name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reached {
    pub files: BTreeSet<String>,
    pub trusted: Vec<String>,
}

impl Graph {
    /// The map of `inputs`.
    pub fn build(inputs: &Inputs<'_>) -> Graph {
        let mut g = Graph {
            files: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            flags: Vec::new(),
            roots: Vec::new(),
            trusted: Vec::new(),
            fns_by_path: HashMap::new(),
            unmapped: BTreeMap::new(),
            by_path: HashMap::new(),
        };
        let mut symbols: Vec<String> = Vec::new();
        let mut interned: HashMap<String, u32> = HashMap::new();
        let mut intern = |s: &str, symbols: &mut Vec<String>| -> u32 {
            if let Some(id) = interned.get(s) {
                return *id;
            }
            let id = symbols.len() as u32;
            symbols.push(s.to_string());
            interned.insert(s.to_string(), id);
            id
        };
        let docs: HashMap<String, &scip::Document> = inputs
            .index
            .documents
            .values()
            .map(|d| (format!("{}{}", inputs.prefix, d.path), d))
            .collect();
        for (path, text) in inputs.sources {
            let Some(doc) = docs.get(path) else {
                g.unmapped.insert(
                    path.clone(),
                    "rust-analyzer's index has no document for it".into(),
                );
                continue;
            };
            if !matches!(doc.encoding, 0 | 1) {
                g.unmapped.insert(
                    path.clone(),
                    "rust-analyzer's index gives its positions in other units than bytes".into(),
                );
                continue;
            }
            let split = match items::split(text) {
                Ok(split) => split,
                Err(why) => {
                    g.unmapped.insert(
                        path.clone(),
                        format!("tree-sitter can't split it: {}", why.0),
                    );
                    continue;
                }
            };
            let mut occurrences = Vec::with_capacity(doc.occurrences.len());
            let mut bad = false;
            for o in &doc.occurrences {
                let (Some(start), Some(end)) = (
                    split.offset(o.range[0], o.range[1]),
                    split.offset(o.range[2], o.range[3]),
                ) else {
                    bad = true;
                    break;
                };
                if o.is_local() {
                    // A local is an occurrence all the same: its identifier is resolved.
                    occurrences.push((start..end, u32::MAX, o.is_definition()));
                    continue;
                }
                occurrences.push((
                    start..end,
                    intern(&o.symbol, &mut symbols),
                    o.is_definition(),
                ));
            }
            if bad {
                g.unmapped.insert(
                    path.clone(),
                    "rust-analyzer's index has positions past its end: it was made of other bytes"
                        .into(),
                );
                continue;
            }
            occurrences.sort_by_key(|(r, _, _)| (r.start, r.end));
            let file = g.files.len() as u32;
            let base = g.nodes.len() as u32;
            for item in 0..split.items.len() {
                g.nodes.push(Node {
                    file,
                    item: item as u32,
                });
            }
            g.by_path.insert(path.clone(), g.files.len());
            g.files.push(Entry {
                path: path.clone(),
                split,
                base,
                occurrences,
            });
        }
        g.edges = vec![Vec::new(); g.nodes.len()];
        g.flags = vec![None; g.nodes.len()];
        g.trusted = vec![false; g.nodes.len()];
        Linker::new(&mut g, &symbols, inputs).link();
        g
    }

    /// \[C-152\] Why `path` isn't in the map, when it isn't: a binary built from it can't narrow.
    pub fn unmapped(&self, path: &str) -> Option<&str> {
        if self.by_path.contains_key(path) {
            return None;
        }
        Some(self.unmapped.get(path).map_or(
            "it isn't one of the workspace's sources the map was made from",
            String::as_str,
        ))
    }

    /// \[C-152\] The sources a test reaches, by its libtest id (`a::tests::t`), its function in one
    /// of `own` (its binary's own crate), and every root of `linked` (the files its binary is built
    /// from). `Err` why it falls back to its binary's reads.
    pub fn reach(
        &self,
        id: &str,
        own: &BTreeSet<String>,
        linked: &BTreeSet<String>,
    ) -> Result<Reached, Fallback> {
        let start = self.test_node(id, own)?;
        let seen = self.walk(start, linked)?;
        let mut trusted: Vec<String> = seen
            .keys()
            .filter(|n| self.trusted[**n as usize])
            .map(|n| self.describe(*n))
            .collect();
        trusted.sort();
        let path = |n: u32| {
            self.files[self.nodes[n as usize].file as usize]
                .path
                .clone()
        };
        Ok(Reached {
            files: seen.into_keys().map(path).collect(),
            trusted,
        })
    }

    /// \[C-152\] Why a test reaches `file`, for people: the items from the test, or from a root,
    /// to the first of `file`'s it reaches. `None` when it doesn't, or falls back.
    pub fn why_reached(
        &self,
        id: &str,
        own: &BTreeSet<String>,
        linked: &BTreeSet<String>,
        file: &str,
    ) -> Option<String> {
        let start = self.test_node(id, own).ok()?;
        let seen = self.walk(start, linked).ok()?;
        let (n, _) = seen
            .iter()
            .filter(|(n, _)| self.files[self.nodes[**n as usize].file as usize].path == file)
            .min_by_key(|(n, _)| self.depth(&seen, **n))?;
        Some(self.chain(&seen, *n))
    }

    fn depth(&self, seen: &HashMap<u32, u32>, mut n: u32) -> usize {
        let mut d = 0;
        while let Some(from) = seen.get(&n).copied().filter(|f| *f != u32::MAX) {
            n = from;
            d += 1;
            if d > 10_000 {
                break;
            }
        }
        d
    }

    fn chain(&self, seen: &HashMap<u32, u32>, n: u32) -> String {
        let mut chain = vec![self.describe(n)];
        let mut at = seen.get(&n).copied().unwrap_or(u32::MAX);
        while at != u32::MAX && chain.len() < 12 {
            chain.push(self.describe(at));
            at = seen.get(&at).copied().unwrap_or(u32::MAX);
        }
        chain.reverse();
        chain.join(" → ")
    }

    /// The node of a test function, by its libtest id, in one of `own`.
    fn test_node(&self, id: &str, own: &BTreeSet<String>) -> Result<u32, Fallback> {
        let found: Vec<u32> = self
            .fns_by_path
            .get(id)
            .into_iter()
            .flatten()
            .copied()
            .filter(|n| own.contains(&self.files[self.nodes[*n as usize].file as usize].path))
            .collect();
        match found.as_slice() {
            [one] => Ok(*one),
            [] => Err(
                "no function of its name is in rust-analyzer's index of its test binary's crate"
                    .into(),
            ),
            _ => Err("more than one function of its name is in its test binary's crate".into()),
        }
    }

    /// Every node reached from `start` and the roots of `linked`, each with the node it was
    /// reached from; `Err` why, at the first node that falls back.
    fn walk(&self, start: u32, linked: &BTreeSet<String>) -> Result<HashMap<u32, u32>, Fallback> {
        let mut seen: HashMap<u32, u32> = HashMap::new();
        let mut queue: VecDeque<(u32, u32)> = VecDeque::new();
        queue.push_back((start, u32::MAX));
        for root in &self.roots {
            let file = &self.files[self.nodes[*root as usize].file as usize].path;
            if linked.contains(file) {
                queue.push_back((*root, u32::MAX));
            }
        }
        while let Some((n, from)) = queue.pop_front() {
            if seen.contains_key(&n) {
                continue;
            }
            // What isn't built into its binary can't run in its process: a binary it starts is
            // C-151's.
            if from != u32::MAX
                && !linked.contains(&self.files[self.nodes[n as usize].file as usize].path)
            {
                continue;
            }
            seen.insert(n, from);
            if let Some(why) = &self.flags[n as usize] {
                return Err(format!("{why}: {}", self.chain(&seen, n)));
            }
            queue.extend(self.edges[n as usize].iter().map(|t| (*t, n)));
        }
        Ok(seen)
    }

    /// An item for people: its file, line and name.
    fn describe(&self, n: u32) -> String {
        let node = self.nodes[n as usize];
        let entry = &self.files[node.file as usize];
        let it = &entry.split.items[node.item as usize];
        let line = entry.split.lines.partition_point(|l| *l <= it.range.start);
        let name = if it.name.is_empty() {
            format!("{:?}", it.kind).to_lowercase()
        } else {
            it.name.clone()
        };
        format!("{}:{line} `{name}`", entry.path)
    }

    /// How many items the map holds, and edges: what the run's measures say.
    pub fn size(&self) -> (usize, usize) {
        (self.nodes.len(), self.edges.iter().map(Vec::len).sum())
    }
}

/// What links the nodes, once every file is in.
struct Linker<'g, 'i> {
    g: &'g mut Graph,
    symbols: &'g [String],
    inputs: &'g Inputs<'i>,
    /// Each symbol's definitions, as nodes; and as the item they name when the definition is the
    /// item's own name.
    defs: HashMap<u32, Vec<u32>>,
    /// Each macro's item-level invocations, by its name.
    invocations: HashMap<String, Vec<u32>>,
    /// Each symbol's id.
    symbol_ids: HashMap<&'g str, u32>,
    /// Each node's own symbol, when a definition names it.
    own_symbol: HashMap<u32, u32>,
    /// Each name's items.
    by_name: HashMap<String, Vec<u32>>,
    /// \[C-148\] Each type symbol's `impl` blocks that name it: the node, and whether it is of a
    /// trait from outside the workspace.
    impls_naming: HashMap<u32, Vec<(u32, bool)>>,
    /// \[C-148\] Each workspace trait's `impl` blocks.
    impls_of: HashMap<u32, Vec<u32>>,
    /// Item-level macro invocations by each identifier they hold.
    calls_naming: HashMap<String, Vec<u32>>,
    /// \[C-150\] The free names of external macros already read.
    external: HashMap<(String, String, String), Option<crate::external::Macro>>,
    /// \[C-150\] Each module's items and those of the modules it imports by `*`.
    scopes: HashMap<(u32, Option<u32>), Vec<u32>>,
    /// Each module symbol's modules: a file, and its `mod` item when it isn't the file's own.
    namespaces: HashMap<u32, Vec<(u32, Option<u32>)>>,
    /// \[C-147\] Each file's crates, by their root files: those it is a module of, by `mod`
    /// declarations (`#[path]` included), more than it is rather than fewer.
    crates_of: Vec<BTreeSet<u32>>,
    /// Each file's index, by its path.
    by_file: HashMap<String, u32>,
    /// \[C-147\] Each file's symbols that a `use` of it names, and whether it imports by `*` from
    /// anything but its own crate.
    imported: Vec<(HashSet<u32>, bool)>,
}

impl<'g, 'i> Linker<'g, 'i> {
    fn new(g: &'g mut Graph, symbols: &'g [String], inputs: &'g Inputs<'i>) -> Self {
        Linker {
            g,
            symbols,
            inputs,
            invocations: HashMap::new(),
            symbol_ids: symbols
                .iter()
                .enumerate()
                .map(|(i, s)| (s.as_str(), i as u32))
                .collect(),
            defs: HashMap::new(),
            own_symbol: HashMap::new(),
            by_name: HashMap::new(),
            impls_naming: HashMap::new(),
            impls_of: HashMap::new(),
            calls_naming: HashMap::new(),
            external: HashMap::new(),
            scopes: HashMap::new(),
            namespaces: HashMap::new(),
            crates_of: Vec::new(),
            by_file: HashMap::new(),
            imported: Vec::new(),
        }
    }

    fn node(&self, file: usize, item: usize) -> u32 {
        self.g.files[file].base + item as u32
    }

    fn item(&self, n: u32) -> &items::Item {
        let node = self.g.nodes[n as usize];
        &self.g.files[node.file as usize].split.items[node.item as usize]
    }

    fn entry(&self, n: u32) -> &Entry {
        &self.g.files[self.g.nodes[n as usize].file as usize]
    }

    fn workspace(&self, s: &Symbol<'_>) -> bool {
        self.inputs.packages.contains(s.package)
    }

    fn link(mut self) {
        self.tables();
        self.crates();
        for n in 0..self.g.nodes.len() as u32 {
            self.link_node(n);
        }
        for edges in &mut self.g.edges {
            edges.sort_unstable();
            edges.dedup();
        }
    }

    /// Definitions, names, `impl` blocks and test functions, before any edge.
    fn tables(&mut self) {
        for (f, entry) in self.g.files.iter().enumerate() {
            for (range, sym, def) in &entry.occurrences {
                if !*def || *sym == u32::MAX {
                    continue;
                }
                let text = &self.symbols[*sym as usize];
                let Some(parsed) = Symbol::parse(text) else {
                    continue;
                };
                let item = entry.split.innermost(range.start);
                // A module: its `mod` item with a body, or its whole file.
                if parsed.last() == Last::Namespace {
                    let module = item.filter(|m| {
                        let it = &entry.split.items[*m];
                        it.kind == Kind::Mod && it.name == parsed.name() && !it.children.is_empty()
                    });
                    if let Some(m) = module {
                        self.namespaces
                            .entry(*sym)
                            .or_default()
                            .push((f as u32, Some(m as u32)));
                    } else if range.start == 0 && range.end >= entry.split.len.saturating_sub(1) {
                        self.namespaces
                            .entry(*sym)
                            .or_default()
                            .push((f as u32, None));
                    }
                    continue;
                }
                let Some(item) = item else {
                    continue;
                };
                let n = entry.base + item as u32;
                self.defs.entry(*sym).or_default().push(n);
                let it = &entry.split.items[item];
                if parsed.name() == it.name {
                    self.own_symbol.entry(n).or_insert(*sym);
                }
                // A free function libtest may name: its descriptors with `::` between.
                if parsed.last() == Last::Method && !parsed.descriptors.contains('#') {
                    let path = parsed
                        .descriptors
                        .strip_suffix(").")
                        .and_then(|d| d.rsplit_once('(').map(|(d, _)| d))
                        .unwrap_or(parsed.descriptors)
                        .replace('/', "::");
                    self.g.fns_by_path.entry(path).or_default().push(n);
                }
            }
        }
        // Names, once every definition is known.
        for entry in &self.g.files {
            for (i, it) in entry.split.items.iter().enumerate() {
                let n = entry.base + i as u32;
                // A module is a namespace: what it holds is reached item by item.
                if !it.name.is_empty()
                    && !matches!(
                        it.kind,
                        Kind::Use | Kind::Impl | Kind::MacroCall | Kind::Mod
                    )
                {
                    self.by_name.entry(it.name.clone()).or_default().push(n);
                }
                if it.kind == Kind::MacroCall
                    && it
                        .parent
                        .is_none_or(|p| entry.split.items[p].kind == Kind::Mod)
                {
                    // An identifier the index resolves as a reference names something else;
                    // one it defines, or leaves unresolved, may be what the macro defines.
                    let referred: HashSet<usize> = in_ranges(
                        &entry.occurrences,
                        std::slice::from_ref(&it.range),
                        |(r, _, _)| r,
                    )
                    // A local's definition defines no item either.
                    .filter(|(_, sym, def)| !*def || *sym == u32::MAX)
                    .map(|(r, _, _)| r.start)
                    .collect();
                    let impls = self.may_impl(entry, it);
                    for ident in
                        in_ranges(&entry.split.idents, std::slice::from_ref(&it.range), |i| {
                            &i.range
                        })
                    {
                        if impls {
                            self.calls_naming
                                .entry(ident.text.clone())
                                .or_default()
                                .push(n);
                        }
                        if !referred.contains(&ident.range.start) {
                            self.by_name.entry(ident.text.clone()).or_default().push(n);
                        }
                    }
                }
            }
        }
        // `macro_rules!` bodies: a name defined there is defined at every invocation.
        let mut invocations: HashMap<String, Vec<u32>> = HashMap::new();
        for entry in &self.g.files {
            for (i, it) in entry.split.items.iter().enumerate() {
                if it.kind == Kind::MacroCall {
                    let name = it.name.rsplit("::").next().unwrap_or(&it.name).to_string();
                    invocations
                        .entry(name)
                        .or_default()
                        .push(entry.base + i as u32);
                }
            }
        }
        let mut body_names: Vec<(String, u32)> = Vec::new();
        for entry in &self.g.files {
            for (i, it) in entry.split.items.iter().enumerate() {
                if it.kind != Kind::MacroRules {
                    continue;
                }
                let calls = invocations.get(&it.name).cloned().unwrap_or_default();
                for ident in &entry.split.idents {
                    if ident.defines
                        && it.range.start <= ident.range.start
                        && ident.range.end <= it.range.end
                    {
                        for c in &calls {
                            body_names.push((ident.text.clone(), *c));
                        }
                        body_names.push((ident.text.clone(), entry.base + i as u32));
                    }
                }
            }
        }
        for (name, n) in body_names {
            self.by_name.entry(name).or_default().push(n);
        }
        self.invocations = invocations;
        for v in self.by_name.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        // `impl` blocks: what they name, and the trait they implement.
        let mut naming: Vec<(u32, u32, bool)> = Vec::new();
        let mut of: Vec<(u32, u32)> = Vec::new();
        for entry in &self.g.files {
            for (i, it) in entry.split.items.iter().enumerate() {
                if it.kind != Kind::Impl {
                    continue;
                }
                let n = entry.base + i as u32;
                let in_range = |r: &Option<Range<usize>>| -> Vec<u32> {
                    let Some(r) = r else { return Vec::new() };
                    entry
                        .occurrences
                        .iter()
                        .filter(|(o, s, _)| *s != u32::MAX && r.start <= o.start && o.end <= r.end)
                        .map(|(_, s, _)| *s)
                        .collect()
                };
                let trait_syms = in_range(&it.impl_trait);
                let mut workspace_trait = false;
                for s in &trait_syms {
                    let text = &self.symbols[*s as usize];
                    if let Some(p) = Symbol::parse(text)
                        && p.last() == Last::Type
                        && self.inputs.packages.contains(p.package)
                        && self
                            .defs
                            .get(s)
                            .is_some_and(|d| d.iter().any(|d| self.kind_of(*d) == Kind::Trait))
                    {
                        workspace_trait = true;
                        of.push((*s, n));
                    }
                }
                let external_trait = it.impl_trait.is_some() && !workspace_trait;
                // What it is for: its self type, and the types its trait's arguments name
                // (`Foo` of `From<Foo>`); never the trait, whose items reach their `impl`s
                // themselves.
                let traits: HashSet<u32> = trait_syms
                    .iter()
                    .copied()
                    .filter(|s| {
                        self.defs
                            .get(s)
                            .is_some_and(|d| d.iter().any(|d| self.kind_of(*d) == Kind::Trait))
                    })
                    .collect();
                let named_in_trait = trait_syms.into_iter().filter(|s| !traits.contains(s));
                for s in in_range(&it.impl_self).into_iter().chain(named_in_trait) {
                    naming.push((s, n, external_trait));
                }
            }
        }
        for (s, n, external) in naming {
            self.impls_naming.entry(s).or_default().push((n, external));
        }
        for (s, n) in of {
            self.impls_of.entry(s).or_default().push(n);
        }
    }

    /// \[C-148\] Whether an item-level invocation's expansion can hold an `impl` block: a
    /// workspace macro whose body says `impl`, one from outside whose body does, or one that can't
    /// be read; never the standard library's.
    fn may_impl(&self, entry: &Entry, it: &items::Item) -> bool {
        let name = it.name.rsplit("::").next().unwrap_or(&it.name);
        let name_at = entry
            .split
            .macros
            .iter()
            .find(|m| m.range.end == it.range.end)
            .map(|m| m.path.end.saturating_sub(name.len()));
        let sym = entry
            .occurrences
            .iter()
            .find(|(r, s, _)| Some(r.start) == name_at && *s != u32::MAX)
            .map(|(_, s, _)| *s);
        let Some(p) = sym.and_then(|s| Symbol::parse(&self.symbols[s as usize])) else {
            return true;
        };
        if p.is_std() {
            return false;
        }
        if self.inputs.packages.contains(p.package) {
            let bodies: Vec<u32> = sym
                .and_then(|s| self.defs.get(&s))
                .cloned()
                .unwrap_or_default();
            return bodies.is_empty()
                || bodies.iter().any(|d| {
                    let e = self.entry(*d);
                    let range = self.item(*d).range.clone();
                    crate::external::contains_word(&self.inputs.sources[&e.path][range], "impl")
                });
        }
        (self.inputs.external)(p.package, p.version, name).is_none_or(|m| m.impls)
    }

    fn kind_of(&self, n: u32) -> Kind {
        self.item(n).kind
    }

    fn edge(&mut self, from: u32, to: u32) {
        if from != to {
            self.g.edges[from as usize].push(to);
        }
    }

    fn flag(&mut self, n: u32, why: String) {
        if self.g.flags[n as usize].is_none() {
            self.g.flags[n as usize] = Some(why);
        }
    }

    /// Every item of a name; for `.name`, every method of it, an `impl`'s or a trait's.
    fn by_name_edges(&mut self, from: u32, name: &str) -> bool {
        match name.strip_prefix('.') {
            Some(name) => self.by_name_class(from, name, Class::Method),
            None => self.by_name_class(from, name, Class::Tail),
        }
    }

    /// \[C-150\] Every item of a name that an identifier of `class` can name.
    fn by_name_class(&mut self, from: u32, name: &str, class: Class) -> bool {
        self.by_path(from, name, class, None)
    }

    /// \[C-150\] [`Linker::by_name_class`] for a path's last segment after `head` (`String` of
    /// `String::from`): an associated item of a type, trait or enum of that name, a method of an
    /// `impl` that names it, an item of a module of that name, or what a macro made. A head that is
    /// `Self`, `self`, `super`, `crate` or a fragment narrows nothing.
    fn by_path(&mut self, from: u32, name: &str, class: Class, head: Option<&str>) -> bool {
        let head = head.filter(|h| {
            !matches!(*h, "Self" | "self" | "super" | "crate" | "") && !h.starts_with('$')
        });
        let to = self.by_name.get(name).cloned().unwrap_or_default();
        let mut found = false;
        for t in to {
            if !self.can_name(from, t) {
                continue;
            }
            let method = self.is_method(t);
            let kind = self.kind_of(t);
            let made = matches!(kind, Kind::MacroCall | Kind::MacroRules);
            let fits = match class {
                Class::Method => method,
                Class::Head => kind.is_type() || made,
                Class::Tail => match head {
                    None => true,
                    Some(head) => made || self.under(t, head),
                },
                Class::Bare => !method || made,
                Class::Value => made || matches!(kind, Kind::Const | Kind::Static),
            };
            if fits {
                found = true;
                self.edge(from, t);
            }
        }
        found
    }

    /// \[C-150\] `Self::<name>` in a `macro_rules!` body: what an invocation of it defines, an
    /// item of an `impl` that names one of `types` (the identifiers its invocations hold), or a
    /// trait's own item of that name.
    fn by_self(&mut self, from: u32, name: &str, types: &HashSet<String>) -> bool {
        let to = self.by_name.get(name).cloned().unwrap_or_default();
        let mut found = false;
        for t in to {
            let kind = self.kind_of(t);
            let fits = matches!(kind, Kind::MacroCall | Kind::MacroRules) || {
                let file = self.g.nodes[t as usize].file as usize;
                let entry = &self.g.files[file];
                let source = &self.inputs.sources[&entry.path];
                match self.item(t).parent.map(|p| &entry.split.items[p]) {
                    Some(parent) if parent.kind == Kind::Trait => true,
                    Some(parent) if parent.kind == Kind::Impl => {
                        [&parent.impl_self, &parent.impl_trait]
                            .into_iter()
                            .flatten()
                            .any(|r| {
                                symbol::names(&source[r.clone()])
                                    .iter()
                                    .any(|w| types.contains(*w))
                            })
                    }
                    _ => false,
                }
            };
            if fits {
                found = true;
                self.edge(from, t);
            }
        }
        found
    }

    /// Whether node `n` can be what `head::<its name>` names: it is in an `impl` whose head
    /// names `head`, in a trait or a module named `head`, or in a file named `head.rs` or
    /// `head/mod.rs`; or it is a type named `head` (an enum's variant is its enum).
    fn under(&self, n: u32, head: &str) -> bool {
        let file = self.g.nodes[n as usize].file as usize;
        let entry = &self.g.files[file];
        let it = self.item(n);
        if it.name == head && it.kind.is_type() {
            return true;
        }
        let word = |text: &str| symbol::names(text).contains(&head);
        if let Some(p) = it.parent {
            let parent = &entry.split.items[p];
            let names_it = match parent.kind {
                Kind::Impl => {
                    let source = &self.inputs.sources[&entry.path];
                    parent.impl_self.clone().is_some_and(|r| word(&source[r]))
                        || parent.impl_trait.clone().is_some_and(|r| word(&source[r]))
                }
                Kind::Trait | Kind::Mod => parent.name == head,
                _ => false,
            };
            if names_it {
                return true;
            }
        }
        if it.module.is_none() {
            let stem = entry
                .path
                .rsplit('/')
                .next()
                .unwrap_or("")
                .trim_end_matches(".rs");
            let dir = entry.path.rsplit('/').nth(1).unwrap_or("");
            return stem == head || (matches!(stem, "mod" | "lib" | "main") && dir == head);
        }
        false
    }

    /// Whether a node is an item of an `impl` or a trait (a method, an associated const or type),
    /// which only a path or a method call names; or what a macro made, which may be one.
    fn is_method(&self, n: u32) -> bool {
        let it = self.item(n);
        match (it.kind, it.parent) {
            (Kind::MacroCall | Kind::MacroRules, _) => true,
            (_, Some(p)) => {
                let file = self.g.nodes[n as usize].file as usize;
                matches!(
                    self.g.files[file].split.items[p].kind,
                    Kind::Impl | Kind::Trait
                )
            }
            _ => false,
        }
    }

    fn link_node(&mut self, n: u32) {
        let node = self.g.nodes[n as usize];
        let file = node.file as usize;
        let item = node.item as usize;
        let own = self.g.files[file].split.own(item);
        let it = self.g.files[file].split.items[item].clone();
        // The head of what holds it: an `impl`'s generics and trait, a trait's bounds.
        if let Some(parent) = it.parent
            && matches!(
                self.g.files[file].split.items[parent].kind,
                Kind::Impl | Kind::Trait
            )
        {
            let p = self.node(file, parent);
            self.edge(n, p);
        }
        // \[C-149\] What runs with nothing naming it.
        if it.attrs.iter().any(|a| ROOT_ATTRS.contains(&a.as_str())) {
            self.g.roots.push(n);
        }
        // Occurrences in its own bytes.
        let entry = &self.g.files[file];
        let occurrences: Vec<(Range<usize>, u32, bool)> =
            in_ranges(&entry.occurrences, &own, |(r, _, _)| r)
                .cloned()
                .collect();
        let starts: HashSet<usize> = occurrences.iter().map(|(r, _, _)| r.start).collect();
        for (range, sym, def) in &occurrences {
            if *sym == u32::MAX || *def {
                continue;
            }
            self.symbol_edges(n, *sym, range);
        }
        // \[C-150\] Identifiers the index resolved to nothing.
        let entry = &self.g.files[file];
        // An attribute's path the index doesn't resolve: a derive's helper (`#[serde]`,
        // `#[error]`) on an item with a derive, which names nothing; else an attribute macro it
        // couldn't read, whose expansion can name anything its module can.
        let derives = it.attrs.iter().any(|a| a == "derive");
        let mut unknown_attribute = false;
        let unresolved: Vec<(String, Class, Option<String>)> =
            in_ranges(&entry.split.idents, &own, |i| &i.range)
                .filter(|i| {
                    !starts.contains(&i.range.start) && !i.defines && !i.label && i.text != "Self"
                })
                .filter(|i| !File::within(&entry.split.inert, &i.range))
                .filter(|i| {
                    let path = File::within(&entry.split.attr_paths, &i.range);
                    unknown_attribute |= path && !derives;
                    !path
                })
                .map(|i| (i.text.clone(), i.class, i.head.clone()))
                .collect();
        if unknown_attribute {
            self.scope_edges(n);
        }
        // \[C-150\] In a `macro_rules!` body, `Self` is a type an invocation names.
        let self_types: Option<HashSet<String>> = (it.kind == Kind::MacroRules).then(|| {
            let calls = self.invocations.get(&it.name).cloned().unwrap_or_default();
            calls
                .iter()
                .flat_map(|c| {
                    let entry = self.entry(*c);
                    let range = self.item(*c).range.clone();
                    in_ranges(&entry.split.idents, std::slice::from_ref(&range), |i| {
                        &i.range
                    })
                    .map(|i| i.text.clone())
                    .collect::<Vec<_>>()
                })
                .collect()
        });
        for (name, class, head) in unresolved {
            match (&self_types, head.as_deref()) {
                (Some(types), Some("Self")) => {
                    self.by_self(n, &name, types);
                }
                _ => {
                    self.by_path(n, &name, class, head.as_deref());
                }
            }
        }
        // \[C-150\] Words of attributes' strings, and `{name}` of macros' strings.
        let entry = &self.g.files[file];
        let strings: Vec<items::Str> = in_ranges(&entry.split.strings, &own, |s| &s.range)
            .cloned()
            .collect();
        for s in &strings {
            // A lookup of a binary's path: a string that starts with the variable's name
            // (`env!("CARGO_BIN_EXE_x")`, `concat!("CARGO_BIN_EXE_", …)`, `var("CARGO_BIN_EXE_…")`),
            // not a message that mentions it.
            let content = s
                .text
                .trim_start_matches(['r', '#'])
                .trim_start_matches('"');
            if content.starts_with("CARGO_BIN_EXE_") {
                let unmarked = if it.marked {
                    format!(
                        ", which the `{}` marker of its function doesn't cover",
                        items::MARKER
                    )
                } else {
                    String::new()
                };
                self.flag(n, format!("it starts a binary of the workspace (`CARGO_BIN_EXE_…`), and so reads all its binary is built from{unmarked}"));
            }
            // A string shaped as a path names what it says (`#[serde(with = "a::b")]`); any
            // other names its `{captures}` alone.
            if s.in_attr
                && let Some(segments) = path_in(&s.text)
            {
                let mut head: Option<&str> = None;
                let last = segments.len() - 1;
                for (i, segment) in segments.iter().enumerate() {
                    let class = if i == last { Class::Tail } else { Class::Head };
                    self.by_path(n, segment, class, head);
                    head = Some(segment);
                }
            } else if s.in_attr || s.in_macro {
                for capture in captures(&s.text) {
                    self.by_name_class(n, capture, Class::Value);
                }
            }
        }
        // \[C-150\] Macros invoked in it.
        let entry = &self.g.files[file];
        let macros: Vec<items::MacroUse> = in_ranges(&entry.split.macros, &own, |m| &m.range)
            .cloned()
            .collect();
        for m in macros {
            let name_at = m.path.end.saturating_sub(m.name.len());
            let sym = occurrences
                .iter()
                .find(|(r, s, _)| r.start == name_at && *s != u32::MAX)
                .map(|(_, s, _)| *s);
            // The invocation that is this item, where an item can be.
            let item_level = it.kind == Kind::MacroCall && it.range.end == m.range.end;
            match sym.and_then(|s| Symbol::parse(&self.symbols[s as usize]).map(|p| (s, p))) {
                None => {
                    // A macro the index doesn't resolve: by its name, and everything its module can
                    // name.
                    self.by_name_edges(n, &m.name);
                    self.scope_edges(n);
                    if item_level {
                        self.g.roots.push(n);
                    }
                }
                Some((_, p)) if p.is_std() => {}
                Some((_, p)) if self.workspace(&p) => {
                    if self.inputs.proc_macros.contains(p.package) {
                        self.flag(n, format!("it invokes `{}!`, a procedural macro of the workspace, whose expansion hushrun can't follow", m.name));
                    }
                }
                Some((_, p)) => {
                    let key = (p.package.to_string(), p.version.to_string(), m.name.clone());
                    let names = match self.external.get(&key) {
                        Some(names) => names.clone(),
                        None => {
                            let names = (self.inputs.external)(&key.0, &key.1, &key.2);
                            self.external.insert(key.clone(), names.clone());
                            names
                        }
                    };
                    // \[C-149\] Where an item can be, one that registers what it expands to, or
                    // whose expansion can't be read, runs unnamed.
                    match names {
                        Some(found) => {
                            let own_methods = OWN_METHODS.contains(&p.package);
                            for name in &found.names {
                                if own_methods && name.starts_with('.') {
                                    continue;
                                }
                                self.by_name_edges(n, name);
                            }
                            if item_level && found.registers {
                                self.g.roots.push(n);
                            }
                        }
                        None if HYGIENIC.contains(&p.package) => {}
                        None => {
                            self.scope_edges(n);
                            if item_level {
                                self.g.roots.push(n);
                            }
                        }
                    }
                }
            }
        }
    }

    /// The edges of one resolved occurrence of `sym` in `n`.
    fn symbol_edges(&mut self, n: u32, sym: u32, range: &Range<usize>) {
        let text = self.symbols[sym as usize].clone();
        let Some(p) = Symbol::parse(&text) else {
            return;
        };
        let last = p.last();
        if last == Last::Namespace {
            return;
        }
        // An `impl` itself (`m/impl#[T][Tr]`), which only `Self` inside it names: no edge.
        if last == Last::Other && p.descriptors.ends_with(']') && p.descriptors.contains("impl#[") {
            return;
        }
        if !self.workspace(&p) {
            self.spawns(n, &p, range);
            if last == Last::Meta && !p.is_std() && !HYGIENIC.contains(&p.package) {
                // A derive or an attribute from another crate's procedural macro.
                self.scope_edges(n);
            }
            return;
        }
        if (last == Last::Meta || last == Last::Macro)
            && self.inputs.proc_macros.contains(p.package)
        {
            self.flag(n, format!("it uses `{}`, a procedural macro of the workspace, whose expansion hushrun can't follow", p.name()));
            return;
        }
        let defs = self.defs.get(&sym).cloned().unwrap_or_default();
        if defs.is_empty() {
            // A symbol the index names and defines nowhere: what a macro made. An `impl`'s item
            // (`m/impl#[T][Tr]f().`) by the invocations that name its type or trait, and their
            // definitions; any other by its own name.
            let names = symbol::names(p.descriptors);
            let bracketed = bracketed(p.descriptors);
            let mut found = false;
            // A field, a variant's field, an associated item: what holds it, when the index
            // defines that.
            let mut owner = Symbol::owner(&text);
            while let Some(o) = owner {
                if Symbol::parse(&o).is_some_and(|p| p.last() == Last::Namespace) {
                    break;
                }
                let held = self
                    .symbol_ids
                    .get(o.as_str())
                    .and_then(|id| self.defs.get(id))
                    .cloned();
                if let Some(held) = held {
                    for d in held {
                        found = true;
                        self.edge(n, d);
                    }
                    break;
                }
                owner = Symbol::owner(&o);
            }
            let held = found;
            for name in bracketed.iter().filter(|_| !held) {
                for call in self.calls_naming.get(*name).cloned().unwrap_or_default() {
                    found = true;
                    self.edge(n, call);
                }
                found |= self.by_name_class(n, name, Class::Head);
            }
            if !found && let Some(last) = names.last() {
                found = self.by_name_class(n, last, Class::Tail);
            }
            // What a standard derive made (`impl#[T][Clone]clone().`): the standard library's
            // code, which reaches what the type holds through the type.
            if !found
                && bracketed.get(1).is_some_and(|tr| {
                    matches!(
                        *tr,
                        "Clone"
                            | "Copy"
                            | "Debug"
                            | "Default"
                            | "PartialEq"
                            | "Eq"
                            | "PartialOrd"
                            | "Ord"
                            | "Hash"
                    )
                })
            {
                found = true;
            }
            if !found {
                self.flag(
                    n,
                    format!(
                        "rust-analyzer's index refers to `{}` and defines it nowhere",
                        p.descriptors
                    ),
                );
            }
            return;
        }
        // \[C-147\] One symbol defined in more files than one: rust-analyzer names the root items
        // of each target of a package alike (`hushrun 0.19.1 run().` is the binary's and every
        // integration test's). A reference that only its own crate can resolve (unqualified, or
        // through `crate::`, `self::` or `super::`), from a file that imports no such symbol and
        // nothing by `*` from another crate, is to its own crate's, when that crate has one.
        let defs = self.same_crate(n, sym, range, defs);
        for d in defs {
            self.edge(n, d);
            let kind = self.kind_of(d);
            let own = self.own_symbol.get(&d) == Some(&sym);
            // \[C-148\] A type: the `impl` blocks that name it, whole for a trait from outside.
            if last == Last::Type && kind.is_type() && own {
                for (imp, external) in self.impls_naming.get(&sym).cloned().unwrap_or_default() {
                    self.edge(n, imp);
                    if external {
                        let children = self.item(imp).children.clone();
                        let file = self.g.nodes[imp as usize].file as usize;
                        for c in children {
                            let c = self.node(file, c);
                            self.edge(n, c);
                        }
                    }
                }
                let name = self.item(d).name.clone();
                for call in self.calls_naming.get(&name).cloned().unwrap_or_default() {
                    self.edge(n, call);
                }
            }
            // \[C-148\] A trait's item: the same of every `impl` of the trait.
            let trait_node = match (kind, self.item(d).parent) {
                (Kind::Fn, Some(parent)) => {
                    let file = self.g.nodes[d as usize].file as usize;
                    let pn = self.node(file, parent);
                    (self.kind_of(pn) == Kind::Trait).then_some(pn)
                }
                (Kind::Trait, _) if !own => Some(d),
                _ => None,
            };
            if let Some(t) = trait_node
                && let Some(tsym) = self.own_symbol.get(&t).copied()
            {
                let name = p.name().to_string();
                // A trait's function runs an `impl`'s function of its name, or its own default
                // body, which is already reached: only the `impl`s that have one are, and each
                // through it (its head comes with it). Its associated const or type is in every
                // `impl`'s head.
                let method = kind == Kind::Fn;
                for imp in self.impls_of.get(&tsym).cloned().unwrap_or_default() {
                    if !method {
                        self.edge(n, imp);
                    }
                    let file = self.g.nodes[imp as usize].file as usize;
                    let children = self.item(imp).children.clone();
                    for c in children {
                        let c = self.node(file, c);
                        if self.item(c).name == name {
                            self.edge(n, c);
                        }
                    }
                }
            }
        }
    }

    /// \[C-150\] Whether code of node `from` can name node `to` by name: always from a
    /// `macro_rules!` body (it is resolved where it expands) or without a layout; else when `to`
    /// is of a crate `from`'s file is a module of, of its package's library (from another of the
    /// package's crates), or of the library of a package its package depends on.
    fn can_name(&self, from: u32, to: u32) -> bool {
        let Some(layout) = self.inputs.layout else {
            return true;
        };
        if self.kind_of(from) == Kind::MacroRules {
            return true;
        }
        let file_from = self.g.nodes[from as usize].file as usize;
        let file_to = self.g.nodes[to as usize].file as usize;
        if !self.crates_of[file_from].is_disjoint(&self.crates_of[file_to]) {
            return true;
        }
        let path_from = &self.g.files[file_from].path;
        let path_to = &self.g.files[file_to].path;
        let (Some(p), Some(q)) = (layout.package_of(path_from), layout.package_of(path_to)) else {
            return true;
        };
        let in_library = |package: &str| {
            layout.libraries.get(package).is_some_and(|root| {
                self.by_file
                    .get(root.as_str())
                    .is_some_and(|r| self.crates_of[file_to].contains(r))
            })
        };
        let sees = p == q || layout.sees.get(p).is_none_or(|sees| sees.contains(q));
        sees && in_library(q)
    }

    /// \[C-147\] Of the definitions of `sym`, those [`Linker::symbol_edges`] keeps for the
    /// occurrence at `range` in `n`: all, but for a reference only its own crate can resolve.
    fn same_crate(&self, n: u32, sym: u32, range: &Range<usize>, defs: Vec<u32>) -> Vec<u32> {
        let file_of = |d: u32| self.g.nodes[d as usize].file;
        let files: BTreeSet<u32> = defs.iter().map(|d| file_of(*d)).collect();
        if files.len() < 2 {
            return defs;
        }
        let here = file_of(n);
        let (imported, globs) = &self.imported[here as usize];
        if imported.contains(&sym) || *globs {
            return defs;
        }
        let source = &self.inputs.sources[&self.g.files[here as usize].path];
        let before = source[..range.start].trim_end();
        if let Some(path) = before.strip_suffix("::") {
            let head = path
                .rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
                .next()
                .unwrap_or("")
                .trim_start_matches("::");
            let first = head.split("::").next().unwrap_or("");
            if !matches!(first, "crate" | "self" | "super" | "Self") {
                return defs;
            }
        }
        let mine = &self.crates_of[here as usize];
        let kept: Vec<u32> = defs
            .iter()
            .copied()
            .filter(|d| !self.crates_of[file_of(*d) as usize].is_disjoint(mine))
            .collect();
        if kept.is_empty() { defs } else { kept }
    }

    /// \[C-147\] Each file's crates, and what each file imports: the module tree from each root
    /// (a file no `mod` declares), a `mod name;` declaring `name.rs` or `name/mod.rs` beside its file
    /// or in the folder named after it (both, on the safe side), or the file its `#[path]` names.
    fn crates(&mut self) {
        let index: HashMap<&str, u32> = self
            .g
            .files
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.as_str(), i as u32))
            .collect();
        let mut children: Vec<Vec<u32>> = vec![Vec::new(); self.g.files.len()];
        let mut declared: Vec<bool> = vec![false; self.g.files.len()];
        for (f, entry) in self.g.files.iter().enumerate() {
            let source = &self.inputs.sources[&entry.path];
            let (dir, stem) = match entry.path.rsplit_once('/') {
                Some((dir, name)) => (dir.to_string(), name.trim_end_matches(".rs").to_string()),
                None => (
                    String::new(),
                    entry.path.trim_end_matches(".rs").to_string(),
                ),
            };
            let join = |a: &str, b: &str| {
                if a.is_empty() {
                    b.to_string()
                } else {
                    format!("{a}/{b}")
                }
            };
            for it in &entry.split.items {
                if it.kind != Kind::Mod || !source[it.range.clone()].trim_end().ends_with(';') {
                    continue;
                }
                // Each module of a `mod` inside an inline module is below that module's folder.
                let mut inline: Vec<&str> = Vec::new();
                let mut up = it.module;
                while let Some(m) = up {
                    inline.push(entry.split.items[m].name.as_str());
                    up = entry.split.items[m].module;
                }
                inline.reverse();
                let nested = inline.join("/");
                let text = &source[it.range.clone()];
                let mut candidates: Vec<String> = Vec::new();
                if let Some(at) = text.find("#[path")
                    && let Some(value) = text[at..].split('"').nth(1)
                {
                    candidates.push(normalize(&join(&dir, value)));
                } else {
                    for base in [join(&dir, &nested), join(&join(&dir, &stem), &nested)] {
                        let base = base.trim_end_matches('/').to_string();
                        candidates.push(join(&base, &format!("{}.rs", it.name)));
                        candidates.push(join(&base, &format!("{}/mod.rs", it.name)));
                    }
                }
                for c in candidates {
                    if let Some(child) = index.get(c.as_str())
                        && *child as usize != f
                    {
                        children[f].push(*child);
                        declared[*child as usize] = true;
                    }
                }
            }
        }
        let mut crates_of: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); self.g.files.len()];
        for root in (0..self.g.files.len()).filter(|f| !declared[*f]) {
            let mut stack = vec![root as u32];
            while let Some(f) = stack.pop() {
                if crates_of[f as usize].insert(root as u32) {
                    stack.extend(children[f as usize].iter().copied());
                }
            }
        }
        // A file in a cycle of declarations no root reaches is its own crate.
        for (f, crates) in crates_of.iter_mut().enumerate() {
            if crates.is_empty() {
                crates.insert(f as u32);
            }
        }
        self.crates_of = crates_of;
        self.by_file = self
            .g
            .files
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.clone(), i as u32))
            .collect();
        let mut imported: Vec<(HashSet<u32>, bool)> = Vec::new();
        for (f, entry) in self.g.files.iter().enumerate() {
            let source = &self.inputs.sources[&entry.path];
            let mut names: HashSet<u32> = HashSet::new();
            let mut globs = false;
            for it in entry.split.items.iter().filter(|it| it.kind == Kind::Use) {
                let text = &source[it.range.clone()];
                let body = text.trim_start_matches("pub").trim_start();
                let body = body
                    .strip_prefix("use")
                    .map(str::trim_start)
                    .unwrap_or(body);
                // A glob from its own crate: `crate::`, `self::`, `super::`, or a module the index
                // defines in a file of the same crate.
                if text.contains('*')
                    && !["crate::", "self::", "super::"]
                        .iter()
                        .any(|p| body.starts_with(p))
                {
                    let module = entry
                        .occurrences
                        .iter()
                        .filter(|(r, s, _)| {
                            it.range.start <= r.start && r.end <= it.range.end && *s != u32::MAX
                        })
                        .filter(|(_, s, _)| {
                            Symbol::parse(&self.symbols[*s as usize])
                                .is_some_and(|p| p.last() == Last::Namespace)
                        })
                        .map(|(_, s, _)| *s)
                        .next_back();
                    let own = module
                        .and_then(|m| self.namespaces.get(&m))
                        .is_some_and(|defs| {
                            !defs.is_empty()
                                && defs.iter().all(|(file, _)| {
                                    !self.crates_of[*file as usize].is_disjoint(&self.crates_of[f])
                                })
                        });
                    globs |= !own;
                }
                for (r, s, _) in &entry.occurrences {
                    if it.range.start <= r.start && r.end <= it.range.end && *s != u32::MAX {
                        names.insert(*s);
                    }
                }
            }
            imported.push((names, globs));
        }
        self.imported = imported;
    }

    /// \[C-151\] Whether an occurrence of a symbol outside the workspace starts a program that
    /// can be a binary of the workspace.
    fn spawns(&mut self, n: u32, p: &Symbol<'_>, range: &Range<usize>) {
        let d = p.descriptors;
        // \[C-151\] A marked function's own `Command::new` is trusted; never a binary it names.
        let marked = self.item(n).marked && self.item(n).kind == Kind::Fn;
        let unmarked = if marked {
            format!(
                ", which the `{}` marker of its function doesn't cover",
                items::MARKER
            )
        } else {
            String::new()
        };
        if p.package == "std" && d.ends_with("env/current_exe().") {
            self.flag(n, format!("it starts its own executable (`std::env::current_exe`), which can be a binary of the workspace{unmarked}"));
        } else if d.contains("cargo_bin") {
            self.flag(n, format!("it starts a binary of the workspace (`cargo_bin`), and so reads all its binary is built from{unmarked}"));
        } else if p.package == "std" && d.ends_with("process/Command#new().") {
            let entry = self.entry(n);
            let text = &self.inputs.sources[&entry.path];
            let after = text[range.end..].trim_start();
            let literal = after
                .strip_prefix('(')
                .map(str::trim_start)
                .and_then(|a| a.strip_prefix('"'))
                .and_then(|a| a.split_once('"'))
                .map(|(program, rest)| (program, rest.trim_start()))
                .filter(|(_, rest)| rest.starts_with(')'))
                .map(|(program, _)| program);
            let safe = literal.is_some_and(|program| {
                !program.is_empty()
                    && !program.contains(['/', '\\'])
                    && !self.inputs.binaries.contains(program)
            });
            if marked && !safe {
                self.g.trusted[n as usize] = true;
            } else if !safe {
                self.flag(n, "it starts a program (`Command::new`) that can be a binary of the workspace, and so reads all its binary is built from".into());
            }
        }
    }

    /// \[C-150\] Every item the module of `n` holds, and those of each module it imports by `*`:
    /// what a name an expansion makes can be resolved to there.
    fn scope_edges(&mut self, n: u32) {
        let node = self.g.nodes[n as usize];
        let module = self.item(n).module.map(|m| m as u32);
        let items = self.scope(node.file, module);
        for t in items {
            self.edge(n, t);
        }
    }

    fn scope(&mut self, file: u32, module: Option<u32>) -> Vec<u32> {
        if let Some(found) = self.scopes.get(&(file, module)) {
            return found.clone();
        }
        // Guard against an import cycle.
        self.scopes.insert((file, module), Vec::new());
        let entry = &self.g.files[file as usize];
        let direct: Vec<usize> = match module {
            Some(m) => entry.split.items[m as usize].children.clone(),
            None => entry.split.top.clone(),
        };
        let mut out: Vec<u32> = direct.iter().map(|i| entry.base + *i as u32).collect();
        // Globs: the module each `use …::*` names, by its last namespace occurrence.
        let mut globbed: Vec<(u32, Option<u32>)> = Vec::new();
        for i in &direct {
            let it = &entry.split.items[*i];
            if it.kind != Kind::Use {
                continue;
            }
            let text = &self.inputs.sources[&entry.path][it.range.clone()];
            if !text.contains('*') {
                continue;
            }
            let namespace = entry
                .occurrences
                .iter()
                .filter(|(r, s, _)| {
                    it.range.start <= r.start && r.end <= it.range.end && *s != u32::MAX
                })
                .filter_map(|(_, s, _)| {
                    let p = Symbol::parse(&self.symbols[*s as usize])?;
                    (p.last() == Last::Namespace && self.inputs.packages.contains(p.package))
                        .then_some(*s)
                })
                .next_back();
            let Some(ns) = namespace else { continue };
            globbed.extend(self.namespaces.get(&ns).cloned().unwrap_or_default());
        }
        for (f, m) in globbed {
            out.extend(self.scope(f, m));
        }
        out.sort_unstable();
        out.dedup();
        self.scopes.insert((file, module), out.clone());
        out
    }
}

/// A path with its `.` and `..` segments resolved: `crates/a/tests/common/x.rs` of
/// `crates/a/tests/../tests/common/x.rs`.
fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out.join("/")
}

/// The members of `sorted` (by their ranges' starts, as a file keeps them) inside one of `ranges`.
fn in_ranges<'a, T>(
    sorted: &'a [T],
    ranges: &'a [Range<usize>],
    range: impl Fn(&T) -> &Range<usize> + Copy + 'a,
) -> impl Iterator<Item = &'a T> + 'a {
    ranges.iter().flat_map(move |r| {
        let from = sorted.partition_point(|t| range(t).start < r.start);
        sorted[from..]
            .iter()
            .take_while(move |t| range(t).start < r.end)
            .filter(move |t| range(t).end <= r.end)
    })
}

/// The names in brackets of descriptors: `T`, `Tr` of `m/impl#[T][Tr]f().`.
fn bracketed(d: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = d;
    while let Some(at) = rest.find('[') {
        let Some(end) = rest[at..].find(']') else {
            break;
        };
        let inner = rest[at + 1..at + end].trim_matches('`');
        // `Vec<T>`: its first name.
        if let Some(name) = symbol::names(inner).first() {
            out.push(*name);
        }
        rest = &rest[at + end + 1..];
    }
    out
}

/// The segments of a string literal that holds a Rust path and nothing else: `a`, `b` of `"a::b"`.
fn path_in(literal: &str) -> Option<Vec<&str>> {
    let inner = literal
        .trim_start_matches('r')
        .trim_matches('#')
        .strip_prefix('"')?
        .strip_suffix('"')?;
    let segments: Vec<&str> = inner.split("::").collect();
    let ident = |s: &&str| {
        !s.is_empty()
            && s.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !s.chars().next().is_some_and(|c| c.is_ascii_digit())
    };
    segments.iter().all(ident).then_some(segments)
}

/// The `{name}` captures of a format string: `CONST` of `"{CONST} {}"`.
fn captures(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('{') {
        rest = &rest[at + 1..];
        if rest.starts_with('{') {
            rest = &rest[1..];
            continue;
        }
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let name = &rest[..end];
        if !name.is_empty() && !name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests;
