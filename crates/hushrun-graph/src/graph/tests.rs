use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::scip::tests::{Occ, index_bytes};

const W: &str = "rust-analyzer cargo w 0.1.0 ";

/// A fake of rust-analyzer's index of `files`: each identifier whose text `symbols` names gets
/// that symbol (after the package `w`, unless it names its own), the `n`th of a text in a file
/// `at` gives; one that follows `fn`, `struct`, `enum`, `trait`, `const`, `static`, `type`,
/// `union`, `mod` or `macro_rules!` defines it. Every other identifier gets none, as one the
/// index left unresolved.
struct Fake<'a> {
    files: &'a [(&'a str, &'a str)],
    symbols: &'a [(&'a str, &'a str)],
    at: &'a [(&'a str, &'a str, usize, &'a str)],
}

/// A document's occurrences, their symbols owned.
type Owned = Vec<([u32; 4], String, i32)>;

fn full(symbol: &str) -> String {
    if symbol.starts_with("rust-analyzer ") || symbol.starts_with("local ") {
        symbol.to_string()
    } else {
        format!("{W}{symbol}")
    }
}

impl Fake<'_> {
    fn index(&self) -> scip::Index {
        let mut docs: Vec<(&str, Owned)> = Vec::new();
        for (path, text) in self.files {
            let split = items::split(text).unwrap();
            let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
            let mut occurrences = Vec::new();
            for ident in &split.idents {
                let word = ident.text.as_str();
                let nth = seen.entry(word).or_default();
                *nth += 1;
                let symbol = self
                    .at
                    .iter()
                    .find(|(f, t, n, _)| f == path && *t == word && *n == *nth)
                    .map(|(_, _, _, s)| *s)
                    .or_else(|| {
                        self.symbols
                            .iter()
                            .find(|(t, _)| *t == word)
                            .map(|(_, s)| *s)
                    });
                let Some(symbol) = symbol else { continue };
                if symbol.is_empty() {
                    continue;
                }
                let before = text[..ident.range.start].trim_end();
                let def = [
                    "fn",
                    "struct",
                    "enum",
                    "trait",
                    "const",
                    "static",
                    "type",
                    "union",
                    "mod",
                    "macro_rules!",
                ]
                .iter()
                .any(|k| before.ends_with(k));
                let line = text[..ident.range.start].matches('\n').count() as u32;
                let col = (ident.range.start
                    - text[..ident.range.start].rfind('\n').map_or(0, |a| a + 1))
                    as u32;
                let len = (ident.range.end - ident.range.start) as u32;
                occurrences.push(([line, col, line, col + len], full(symbol), i32::from(def)));
            }
            docs.push((path, occurrences));
        }
        let docs: Vec<(&str, Vec<Occ<'_>>)> = docs
            .iter()
            .map(|(p, o)| (*p, o.iter().map(|(r, s, d)| (*r, s.as_str(), *d)).collect()))
            .collect();
        scip::parse(&index_bytes("file:///w", &docs)).unwrap()
    }

    fn graph(
        &self,
        external: &dyn Fn(&str, &str, &str) -> Option<crate::external::Macro>,
    ) -> Graph {
        let sources: BTreeMap<String, String> = self
            .files
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect();
        let index = self.index();
        let packages = BTreeSet::from(["w".to_string()]);
        let proc_macros = BTreeSet::from(["wmacros".to_string()]);
        let binaries = BTreeSet::from(["hushrun".to_string()]);
        Graph::build(&Inputs {
            sources: &sources,
            index: &index,
            prefix: "",
            packages: &packages,
            proc_macros: &proc_macros,
            binaries: &binaries,
            external,
            layout: None,
        })
    }

    fn reach(&self, test: &str) -> Result<BTreeSet<String>, String> {
        self.reach_with(test, &|_, _, _| None)
    }

    fn reach_with(
        &self,
        test: &str,
        external: &dyn Fn(&str, &str, &str) -> Option<crate::external::Macro>,
    ) -> Result<BTreeSet<String>, String> {
        self.reached_with(test, external).map(|r| r.files)
    }

    fn reached_with(
        &self,
        test: &str,
        external: &dyn Fn(&str, &str, &str) -> Option<crate::external::Macro>,
    ) -> Result<Reached, String> {
        let g = self.graph(external);
        let all: BTreeSet<String> = self.files.iter().map(|(p, _)| p.to_string()).collect();
        g.reach(test, &all, &all)
    }
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| p.to_string()).collect()
}

// covers: C-147
#[test]
fn a_test_reaches_what_it_calls_and_not_the_rest_of_its_crate() {
    let fake = Fake {
        files: &[
            (
                "src/lib.rs",
                "mod a; mod b;\n#[cfg(test)]\nmod tests { #[test] fn t() { crate::a::a(); } }\n",
            ),
            ("src/a.rs", "pub fn a() { helper(); }\nfn helper() {}\n"),
            ("src/b.rs", "pub fn b() {}\n"),
        ],
        symbols: &[
            ("a", "a/a()."),
            ("helper", "a/helper()."),
            ("b", "b/b()."),
            ("t", "tests/t()."),
            ("tests", "tests/"),
            ("crate", "crate/"),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
            ("cfg", "rust-analyzer cargo core 1.0 cfg:"),
        ],
        at: &[
            ("src/lib.rs", "a", 1, "a/"),
            ("src/lib.rs", "b", 1, "b/"),
            ("src/lib.rs", "a", 2, "a/"),
        ],
    };
    assert_eq!(
        fake.reach("tests::t").unwrap(),
        set(&["src/lib.rs", "src/a.rs"])
    );
    assert!(fake.reach("tests::missing").is_err());
}

// covers: C-148
#[test]
fn a_call_through_a_trait_reaches_every_impl_of_its_method() {
    let fake = Fake {
        files: &[
            (
                "src/lib.rs",
                "pub trait Run { fn run(&self); }\n#[test]\nfn t() { let x: &dyn Run = make(); x.run(); }\nfn make() -> &'static dyn Run { &One }\n",
            ),
            (
                "src/one.rs",
                "pub struct One;\nimpl Run for One { fn run(&self) { one(); } }\nfn one() {}\n",
            ),
            (
                "src/two.rs",
                "pub struct Two;\nimpl Run for Two { fn run(&self) { two(); } }\nfn two() {}\n",
            ),
            (
                "src/three.rs",
                "pub struct Three;\nimpl Three { fn run(&self) {} }\n",
            ),
        ],
        symbols: &[
            ("Run", "Run#"),
            ("run", "Run#run()."),
            ("t", "t()."),
            ("make", "make()."),
            ("One", "one/One#"),
            ("Two", "two/Two#"),
            ("Three", "three/Three#"),
            ("one", "one/one()."),
            ("two", "two/two()."),
            ("x", "local 1"),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
        ],
        at: &[
            ("src/one.rs", "run", 1, "one/impl#[One][Run]run()."),
            ("src/two.rs", "run", 1, "two/impl#[Two][Run]run()."),
            ("src/three.rs", "run", 1, "three/impl#[Three]run()."),
        ],
    };
    let reached = fake.reach("t").unwrap();
    // Both impls of `Run::run`, and what each calls; not an inherent method of the same name.
    assert_eq!(reached, set(&["src/lib.rs", "src/one.rs", "src/two.rs"]));
}

// covers: C-148
#[test]
fn a_type_reaches_its_impls_of_traits_from_outside_and_its_constants() {
    let fake = Fake {
        files: &[
            (
                "src/lib.rs",
                "pub struct S { n: u8 }\nconst LIMIT: u8 = 3;\n#[test]\nfn t() { let s = S { n: LIMIT }; }\n",
            ),
            (
                "src/drop.rs",
                "impl Drop for S { fn drop(&mut self) { cleanup(); } }\nfn cleanup() {}\n",
            ),
            (
                "src/inherent.rs",
                "impl S { pub fn unused(&self) { far(); } }\n",
            ),
            ("src/far.rs", "pub fn far() {}\n"),
        ],
        symbols: &[
            ("S", "S#"),
            ("n", "S#n."),
            ("LIMIT", "LIMIT."),
            ("t", "t()."),
            ("s", "local 1"),
            ("Drop", "rust-analyzer cargo core 1.0 ops/drop/Drop#"),
            ("drop", "drop/impl#[S][Drop]drop()."),
            ("cleanup", "drop/cleanup()."),
            ("unused", "inherent/impl#[S]unused()."),
            ("far", "far/far()."),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
        ],
        at: &[],
    };
    // `Drop` runs with no name for it; `unused` isn't called.
    let reached = fake.reach("t").unwrap();
    assert!(reached.contains("src/drop.rs"), "{reached:?}");
    assert!(
        reached.contains("src/inherent.rs"),
        "an impl's head is reached: {reached:?}"
    );
    assert!(!reached.contains("src/far.rs"), "{reached:?}");
}

// covers: C-149
#[test]
fn a_function_value_counts_where_it_is_named_and_what_runs_unnamed_counts_for_every_test() {
    let fake = Fake {
        files: &[
            (
                "src/lib.rs",
                "#[test]\nfn t() { run(callback); }\nfn run(f: fn()) { f() }\n",
            ),
            ("src/cb.rs", "pub fn callback() {}\n"),
            (
                "src/hook.rs",
                "#[no_mangle]\npub extern \"C\" fn hook() { hooked(); }\nfn hooked() {}\n",
            ),
            ("src/other.rs", "pub fn other() {}\n"),
        ],
        symbols: &[
            ("t", "t()."),
            ("run", "run()."),
            ("f", "local 1"),
            ("callback", "cb/callback()."),
            ("hook", "hook/hook()."),
            ("hooked", "hook/hooked()."),
            ("no_mangle", "rust-analyzer cargo core 1.0 no_mangle:"),
            ("other", "other/other()."),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
        ],
        at: &[],
    };
    assert_eq!(
        fake.reach("t").unwrap(),
        set(&["src/lib.rs", "src/cb.rs", "src/hook.rs"])
    );
}

// covers: C-150
#[test]
fn a_macro_counts_its_definition_and_every_name_its_body_holds() {
    let fake = Fake {
        files: &[
            ("src/lib.rs", "#[test]\nfn t() { check!(1); }\n"),
            (
                "src/m.rs",
                "#[macro_export]\nmacro_rules! check { ($e:expr) => { helper($e) } }\n",
            ),
            ("src/h.rs", "pub fn helper(_: u8) {}\n"),
            ("src/other.rs", "pub fn other() {}\n"),
        ],
        symbols: &[
            ("t", "t()."),
            ("check", "check!"),
            ("helper", "h/helper()."),
            ("other", "other/other()."),
            ("macro_export", "rust-analyzer cargo core 1.0 macro_export:"),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
        ],
        // The body's `helper` has no occurrence, as rust-analyzer writes none there.
        at: &[("src/m.rs", "helper", 1, "")],
    };
    assert_eq!(
        fake.reach("t").unwrap(),
        set(&["src/lib.rs", "src/m.rs", "src/h.rs"])
    );
}

// covers: C-150
#[test]
fn what_the_index_leaves_unresolved_counts_by_name() {
    let fake = Fake {
        files: &[
            (
                "src/lib.rs",
                "#[test]\nfn t() {\n    #[cfg(feature = \"x\")]\n    gated();\n    let _ = format!(\"{LIMIT}\");\n}\n",
            ),
            ("src/g.rs", "pub fn gated() {}\n"),
            ("src/c.rs", "pub const LIMIT: u8 = 1;\n"),
            ("src/other.rs", "pub fn other() {}\n"),
        ],
        symbols: &[
            ("t", "t()."),
            ("gated", "g/gated()."),
            ("LIMIT", "c/LIMIT."),
            ("other", "other/other()."),
            ("format", "rust-analyzer cargo alloc 1.0 macros/format!"),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
            ("cfg", "rust-analyzer cargo core 1.0 cfg:"),
        ],
        // Code `cfg` leaves out has no occurrence.
        at: &[("src/lib.rs", "gated", 1, "")],
    };
    assert_eq!(
        fake.reach("t").unwrap(),
        set(&["src/lib.rs", "src/g.rs", "src/c.rs"])
    );
}

// covers: C-150
#[test]
fn macros_from_outside_count_their_free_names_or_their_whole_module() {
    let files: &[(&str, &str)] = &[
        (
            "src/lib.rs",
            "mod near;\n#[test]\nfn t() { ext::go!(); }\n#[test]\nfn d() { let _ = near::N; }\n",
        ),
        (
            "src/near.rs",
            "#[derive(ext_derive::Make)]\npub struct N;\nfn beside() { far(); }\n",
        ),
        ("src/name.rs", "pub fn named() {}\n"),
        ("src/far.rs", "pub fn far() {}\n"),
    ];
    let symbols: &[(&str, &str)] = &[
        ("t", "t()."),
        ("d", "d()."),
        ("near", "near/"),
        ("N", "near/N#"),
        ("beside", "near/beside()."),
        ("far", "far/far()."),
        ("named", "name/named()."),
        ("go", "rust-analyzer cargo ext 1.0 go!"),
        ("ext", "rust-analyzer cargo ext 1.0 crate/"),
        ("Make", "rust-analyzer cargo ext_derive 1.0 Make:"),
        ("ext_derive", "rust-analyzer cargo ext_derive 1.0 crate/"),
        (
            "derive",
            "rust-analyzer cargo core 1.0 macros/builtin/derive:",
        ),
        ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
    ];
    let fake = Fake {
        files,
        symbols,
        at: &[],
    };
    // `go!`'s body names `named`.
    let named = |_: &str, _: &str, name: &str| {
        (name == "go").then(|| crate::external::Macro {
            names: BTreeSet::from(["named".to_string()]),
            registers: false,
            impls: false,
        })
    };
    let reached = fake.reach_with("t", &named).unwrap();
    assert!(
        reached.contains("src/name.rs") && !reached.contains("src/far.rs"),
        "{reached:?}"
    );
    // A derive of a crate not known to be hygienic: everything of its module, `beside` too.
    let reached = fake.reach_with("d", &named).unwrap();
    assert!(reached.contains("src/far.rs"), "{reached:?}");
}

// covers: C-150
#[test]
fn a_procedural_macro_of_the_workspace_falls_back() {
    let fake = Fake {
        files: &[("src/lib.rs", "#[test]\nfn t() { wmacros::make!(); }\n")],
        symbols: &[
            ("t", "t()."),
            ("make", "rust-analyzer cargo wmacros 0.1.0 make!"),
            ("wmacros", "rust-analyzer cargo wmacros 0.1.0 crate/"),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
        ],
        at: &[],
    };
    let packages = BTreeSet::from(["w".to_string(), "wmacros".to_string()]);
    let sources: BTreeMap<String, String> = fake
        .files
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
    let index = fake.index();
    let g = Graph::build(&Inputs {
        sources: &sources,
        index: &index,
        prefix: "",
        packages: &packages,
        proc_macros: &BTreeSet::from(["wmacros".to_string()]),
        binaries: &BTreeSet::new(),
        external: &|_, _, _| None,
        layout: None,
    });
    let all = set(&["src/lib.rs"]);
    let why = g.reach("t", &all, &all).unwrap_err();
    assert!(why.contains("procedural macro of the workspace"), "{why}");
}

// covers: C-151
#[test]
fn a_marked_function_s_own_command_is_trusted_but_never_a_binary_it_names() {
    let files: &[(&str, &str)] = &[(
        "tests/run.rs",
        "#[doc(alias = \"hushrun::starts_no_workspace_binary\")]\nfn tool(p: &str) { Command::new(p); }\n#[doc(alias = \"hushrun::starts_no_workspace_binary\")]\nfn named() { Command::new(env!(\"CARGO_BIN_EXE_hushrun\")); }\n#[doc(alias = \"hushrun::starts_no_workspace_binary\")]\nfn itself() { std::env::current_exe(); }\nfn bare(p: &str) { Command::new(p); }\n#[test]\nfn uses_tool() { tool(\"gradle\"); }\n#[test]\nfn uses_named() { named(); }\n#[test]\nfn uses_itself() { itself(); }\n#[test]\nfn uses_bare() { bare(\"x\"); }\n",
    )];
    let symbols: &[(&str, &str)] = &[
        ("tool", "tool()."),
        ("named", "named()."),
        ("itself", "itself()."),
        ("bare", "bare()."),
        ("uses_tool", "uses_tool()."),
        ("uses_named", "uses_named()."),
        ("uses_itself", "uses_itself()."),
        ("uses_bare", "uses_bare()."),
        ("p", "local 1"),
        ("new", "rust-analyzer cargo std 1.0 process/Command#new()."),
        ("Command", "rust-analyzer cargo std 1.0 process/Command#"),
        (
            "current_exe",
            "rust-analyzer cargo std 1.0 env/current_exe().",
        ),
        ("env", "rust-analyzer cargo core 1.0 macros/builtin/env!"),
        ("doc", "rust-analyzer cargo core 1.0 doc:"),
        ("alias", ""),
        ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
    ];
    let fake = Fake {
        files,
        symbols,
        at: &[],
    };
    let reached = fake.reached_with("uses_tool", &|_, _, _| None).unwrap();
    assert_eq!(reached.trusted, ["tests/run.rs:1 `tool`"]);
    for test in ["uses_named", "uses_itself"] {
        let why = fake.reach(test).unwrap_err();
        assert!(
            why.contains("marker of its function doesn't cover"),
            "{test}: {why}"
        );
    }
    assert!(fake.reach("uses_bare").is_err());
}

// covers: C-151
#[test]
fn a_test_that_can_start_a_workspace_binary_falls_back() {
    let files: &[(&str, &str)] = &[(
        "tests/run.rs",
        "#[test]\nfn spawns() { std::process::Command::new(env!(\"CARGO_BIN_EXE_hushrun\")); }\n#[test]\nfn by_variable() { let p = \"x\"; Command::new(p); }\n#[test]\nfn git() { Command::new(\"git\"); panic!(\"start it with env!(CARGO_BIN_EXE_…)\"); }\n#[test]\nfn named() { Command::new(\"hushrun\"); }\n#[test]\nfn itself() { std::env::current_exe(); }\n",
    )];
    let symbols: &[(&str, &str)] = &[
        ("spawns", "spawns()."),
        ("by_variable", "by_variable()."),
        ("git", "git()."),
        ("named", "named()."),
        ("itself", "itself()."),
        ("p", "local 1"),
        ("new", "rust-analyzer cargo std 1.0 process/Command#new()."),
        ("Command", "rust-analyzer cargo std 1.0 process/Command#"),
        (
            "current_exe",
            "rust-analyzer cargo std 1.0 env/current_exe().",
        ),
        ("env", "rust-analyzer cargo core 1.0 macros/builtin/env!"),
        ("panic", "rust-analyzer cargo core 1.0 macros/panic!"),
        ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
    ];
    let fake = Fake {
        files,
        symbols,
        at: &[],
    };
    // A message that mentions `CARGO_BIN_EXE_…` looks nothing up.
    for test in ["spawns", "by_variable", "named", "itself"] {
        assert!(fake.reach(test).is_err(), "{test}");
    }
    assert_eq!(fake.reach("git").unwrap(), set(&["tests/run.rs"]));
}

// covers: C-152
#[test]
fn a_file_the_index_lacks_is_unmapped_and_says_why() {
    let fake = Fake {
        files: &[("src/lib.rs", "#[test]\nfn t() {}\n")],
        symbols: &[("t", "t().")],
        at: &[],
    };
    let mut sources: BTreeMap<String, String> = fake
        .files
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
    sources.insert("src/new.rs".into(), "pub fn n() {}\n".into());
    sources.insert("src/broken.rs".into(), "fn (".into());
    let index = fake.index();
    let g = Graph::build(&Inputs {
        sources: &sources,
        index: &index,
        prefix: "",
        packages: &BTreeSet::from(["w".to_string()]),
        proc_macros: &BTreeSet::new(),
        binaries: &BTreeSet::new(),
        external: &|_, _, _| None,
        layout: None,
    });
    assert_eq!(g.unmapped("src/lib.rs"), None);
    assert!(g.unmapped("src/new.rs").unwrap().contains("no document"));
    assert!(g.unmapped("src/elsewhere.rs").is_some());
}

// covers: C-150
#[test]
fn a_symbol_defined_nowhere_counts_by_its_names_or_falls_back() {
    let fake = Fake {
        files: &[
            (
                "src/lib.rs",
                "#[test]\nfn t() { Made::new(); Gone::f(); }\n",
            ),
            ("src/made.rs", "make!(Made);\n"),
        ],
        symbols: &[
            ("t", "t()."),
            ("Made", "made/Made#"),
            ("new", "made/impl#[Made]new()."),
            ("Gone", "gone/Missing#"),
            ("f", "gone/impl#[Missing]vanished()."),
            ("make", "make!"),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
        ],
        at: &[("src/made.rs", "Made", 1, "made/Made#")],
    };
    let why = fake.reach("t").unwrap_err();
    assert!(why.contains("defines it nowhere"), "{why}");
}

// covers: C-150
#[test]
fn format_captures_are_names() {
    assert_eq!(captures("\"{CONST} {} {{x}} {y:?} {0}\""), ["CONST", "y"]);
}

// covers: C-150
#[test]
fn an_impls_bracketed_names_are_its_type_and_trait() {
    assert_eq!(
        bracketed("yaml/impl#[`MapVisitor<K, V>`][`Visitor<'de>`]"),
        ["MapVisitor", "Visitor"]
    );
}

// covers: C-147
#[test]
fn a_root_item_two_crates_share_a_symbol_of_is_its_own_crates() {
    // rust-analyzer names the root `run` of the binary and of each integration test alike.
    let files: &[(&str, &str)] = &[
        ("src/main.rs", "fn run() { far(); }\nfn main() { run(); }\n"),
        ("src/far.rs", "pub fn far() {}\n"),
        (
            "tests/a.rs",
            "#[path = \"common/help.rs\"]\nmod help;\nfn run() { help::h(); }\n#[test]\nfn t() { run(); }\n",
        ),
        ("tests/common/help.rs", "pub fn h() {}\n"),
        (
            "tests/b.rs",
            "use w::run;\nfn run_here() {}\n#[test]\nfn u() { run(); }\n",
        ),
    ];
    let symbols: &[(&str, &str)] = &[
        ("run", "run()."),
        ("main", "main()."),
        ("far", "far/far()."),
        ("help", "help/"),
        ("h", "help/h()."),
        ("t", "t()."),
        ("u", "u()."),
        ("w", "crate/"),
        ("run_here", "run_here()."),
        ("path", ""),
        ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
    ];
    let fake = Fake {
        files,
        symbols,
        at: &[],
    };
    let all: BTreeSet<String> = files.iter().map(|(p, _)| p.to_string()).collect();
    let g = fake.graph(&|_, _, _| None);
    let own = set(&["tests/a.rs", "tests/common/help.rs"]);
    let reached = g.reach("t", &own, &all).unwrap().files;
    assert!(reached.contains("tests/common/help.rs"), "{reached:?}");
    assert!(
        !reached.contains("src/far.rs") && !reached.contains("src/main.rs"),
        "{reached:?}"
    );
    // A file that imports the symbol keeps every definition of it.
    let reached = g.reach("u", &set(&["tests/b.rs"]), &all).unwrap().files;
    assert!(
        reached.contains("src/main.rs") && reached.contains("tests/a.rs"),
        "{reached:?}"
    );
    assert_eq!(
        normalize("crates/a/tests/../tests/common/x.rs"),
        "crates/a/tests/common/x.rs"
    );
}

// covers: C-148
#[test]
fn a_trait_methods_call_reaches_its_overrides_alone_and_a_trait_reaches_no_impl_head() {
    let files: &[(&str, &str)] = &[
        (
            "src/lib.rs",
            "pub trait Ad { fn go(&self) -> u8 { 0 } fn other(&self); }\n#[test]\nfn t() { One.go(); }\n",
        ),
        (
            "src/one.rs",
            "pub struct One;\nimpl Ad for One { fn other(&self) {} }\n",
        ),
        (
            "src/two.rs",
            "pub struct Two;\nimpl Ad for Two { fn go(&self) -> u8 { two() } fn other(&self) {} }\nfn two() -> u8 { 2 }\n",
        ),
        (
            "src/three.rs",
            "pub struct Three;\nimpl Ad for Three { fn other(&self) { far() } }\n",
        ),
        ("src/far.rs", "pub fn far() {}\n"),
    ];
    let symbols: &[(&str, &str)] = &[
        ("Ad", "Ad#"),
        ("go", "Ad#go()."),
        ("other", "Ad#other()."),
        ("t", "t()."),
        ("One", "one/One#"),
        ("Two", "two/Two#"),
        ("Three", "three/Three#"),
        ("two", "two/two()."),
        ("far", "far/far()."),
        ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
    ];
    let at: &[(&str, &str, usize, &str)] = &[
        ("src/one.rs", "other", 1, "one/impl#[One][Ad]other()."),
        ("src/two.rs", "go", 1, "two/impl#[Two][Ad]go()."),
        ("src/two.rs", "other", 1, "two/impl#[Two][Ad]other()."),
        ("src/three.rs", "other", 1, "three/impl#[Three][Ad]other()."),
    ];
    let fake = Fake { files, symbols, at };
    let reached = fake.reach("t").unwrap();
    // `go` resolves to the trait's own: its default body, and Two's override; not Three, which
    // doesn't override it, nor what Three's other method calls.
    assert!(reached.contains("src/two.rs"), "{reached:?}");
    assert!(
        !reached.contains("src/three.rs") && !reached.contains("src/far.rs"),
        "{reached:?}"
    );
}

// covers: C-150
#[test]
fn a_name_is_matched_only_among_what_its_crate_can_see() {
    let files: &[(&str, &str)] = &[
        (
            "lower/src/lib.rs",
            "#[test]\nfn t() {\n    #[cfg(feature = \"x\")]\n    target();\n}\n",
        ),
        ("upper/src/lib.rs", "pub fn target() {}\n"),
        ("lower/src/local.rs", "pub fn target() {}\n"),
    ];
    let symbols: &[(&str, &str)] = &[
        ("t", "t()."),
        ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
        ("cfg", "rust-analyzer cargo core 1.0 cfg:"),
    ];
    let fake = Fake {
        files,
        symbols,
        at: &[],
    };
    let sources: BTreeMap<String, String> = files
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
    let index = fake.index();
    let layout = Layout {
        folders: vec![
            ("lower/".into(), "lower".into()),
            ("upper/".into(), "upper".into()),
        ],
        libraries: BTreeMap::from([
            ("lower".to_string(), "lower/src/lib.rs".to_string()),
            ("upper".to_string(), "upper/src/lib.rs".to_string()),
        ]),
        // `upper` depends on `lower`, never the other way.
        sees: BTreeMap::from([
            ("lower".to_string(), BTreeSet::new()),
            ("upper".to_string(), BTreeSet::from(["lower".to_string()])),
        ]),
    };
    let g = Graph::build(&Inputs {
        sources: &sources,
        index: &index,
        prefix: "",
        packages: &BTreeSet::from(["w".to_string()]),
        proc_macros: &BTreeSet::new(),
        binaries: &BTreeSet::new(),
        external: &|_, _, _| None,
        layout: Some(&layout),
    });
    let all: BTreeSet<String> = sources.keys().cloned().collect();
    let reached = g
        .reach("t", &set(&["lower/src/lib.rs"]), &all)
        .unwrap()
        .files;
    // An unresolved `target` in `lower` can't be `upper`'s, which depends on it.
    assert!(!reached.contains("upper/src/lib.rs"), "{reached:?}");
}

// covers: C-150
#[test]
fn what_a_derive_made_reaches_its_own_type_not_every_type_of_its_name() {
    let files: &[(&str, &str)] = &[
        (
            "src/lib.rs",
            "#[derive(Default)]\npub struct Observed { n: u8 }\n#[test]\nfn t() { let _ = Observed::default(); }\n",
        ),
        (
            "src/other.rs",
            "#[derive(Default)]\npub struct Observed { far: Far }\npub struct Far;\n",
        ),
    ];
    let symbols: &[(&str, &str)] = &[
        ("t", "t()."),
        ("Observed", "Observed#"),
        ("n", "Observed#n."),
        ("default", "impl#[Observed][Default]default()."),
        ("far", "other/Observed#far."),
        ("Far", "other/Far#"),
        ("Default", "rust-analyzer cargo core 1.0 default/Default:"),
        (
            "derive",
            "rust-analyzer cargo core 1.0 macros/builtin/derive:",
        ),
        ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
    ];
    let at: &[(&str, &str, usize, &str)] = &[("src/other.rs", "Observed", 1, "other/Observed#")];
    let fake = Fake { files, symbols, at };
    assert_eq!(fake.reach("t").unwrap(), set(&["src/lib.rs"]));
}

// covers: C-150
#[test]
fn a_name_with_no_path_is_its_own_modules_item_first() {
    let fake = Fake {
        files: &[
            (
                "src/lib.rs",
                "const LIMIT: u8 = 1;\nstruct Shape {}\n#[test]\nfn t() {\n    let _ = format!(\"{LIMIT}\");\n    #[cfg(feature = \"x\")]\n    Shape();\n}\nmod inner {\n    #[test]\n    fn u() {\n        let _ = format!(\"{LIMIT}\");\n    }\n}\n",
            ),
            // Another module's constant of that name, which the local one shadows.
            ("src/c.rs", "pub const LIMIT: u8 = 2;\n"),
            // A function of the name of a local braced struct, which shadows no value.
            ("src/s.rs", "pub fn Shape() {}\n"),
        ],
        symbols: &[
            ("t", "t()."),
            ("u", "inner/u()."),
            ("format", "rust-analyzer cargo alloc 1.0 macros/format!"),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
            ("cfg", "rust-analyzer cargo core 1.0 cfg:"),
        ],
        at: &[],
    };
    assert_eq!(fake.reach("t").unwrap(), set(&["src/lib.rs", "src/s.rs"]));
    // In a module that defines none, every constant of the name counts.
    let reached = fake.reach("inner::u").unwrap();
    assert!(reached.contains("src/c.rs"), "{reached:?}");
    // A path before the name (`super::`) resolves it elsewhere: the local item shadows nothing.
    let fake = Fake {
        files: &[
            (
                "src/lib.rs",
                "mod inner;
pub fn gated() {}
",
            ),
            (
                "src/inner.rs",
                "fn gated() {}
#[test]
fn u() {
    #[cfg(feature = \"x\")]
    super::gated();
}
",
            ),
        ],
        symbols: &[
            ("u", "inner/u()."),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
            ("cfg", "rust-analyzer cargo core 1.0 cfg:"),
        ],
        at: &[],
    };
    let reached = fake.reach("inner::u").unwrap();
    assert!(reached.contains("src/lib.rs"), "{reached:?}");
}

// covers: C-150
#[test]
fn a_macro_rules_body_names_what_the_crates_invoking_it_can_see() {
    let run = |upper_invokes: bool| {
        let upper = if upper_invokes {
            "mod r;\nlower::name_type!(Other);\n"
        } else {
            "mod r;\npub fn unrelated() {}\n"
        };
        let files: Vec<(&str, &str)> = vec![
            (
                "lower/src/lib.rs",
                "mod o;\n#[macro_export]\nmacro_rules! name_type {\n    ($t:ident) => {\n        pub struct $t;\n        impl $t {\n            pub fn ok() -> Outcome { Outcome::new() }\n        }\n    };\n}\n#[test]\nfn t() {\n    name_type!(Name);\n}\n",
            ),
            ("lower/src/o.rs", "pub struct Outcome;\n"),
            ("upper/src/lib.rs", upper),
            ("upper/src/r.rs", "pub struct Outcome;\n"),
        ];
        let symbols: &[(&str, &str)] = &[
            ("t", "t()."),
            ("name_type", "name_type!"),
            ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
        ];
        let fake = Fake {
            files: &files,
            symbols,
            at: &[],
        };
        let sources: BTreeMap<String, String> = files
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect();
        let index = fake.index();
        let layout = Layout {
            folders: vec![
                ("lower/".into(), "lower".into()),
                ("upper/".into(), "upper".into()),
            ],
            libraries: BTreeMap::from([
                ("lower".to_string(), "lower/src/lib.rs".to_string()),
                ("upper".to_string(), "upper/src/lib.rs".to_string()),
            ]),
            sees: BTreeMap::from([
                ("lower".to_string(), BTreeSet::new()),
                ("upper".to_string(), BTreeSet::from(["lower".to_string()])),
            ]),
        };
        let g = Graph::build(&Inputs {
            sources: &sources,
            index: &index,
            prefix: "",
            packages: &BTreeSet::from(["w".to_string()]),
            proc_macros: &BTreeSet::new(),
            binaries: &BTreeSet::new(),
            external: &|_, _, _| None,
            layout: Some(&layout),
        });
        let all: BTreeSet<String> = sources.keys().cloned().collect();
        g.reach("t", &set(&["lower/src/lib.rs"]), &all)
            .unwrap()
            .files
    };
    // Invoked in `lower` alone, its `Outcome` is `lower`'s.
    let reached = run(false);
    assert!(reached.contains("lower/src/o.rs"), "{reached:?}");
    assert!(!reached.contains("upper/src/r.rs"), "{reached:?}");
    // Invoked in `upper` too, it can be `upper`'s.
    let reached = run(true);
    assert!(reached.contains("upper/src/r.rs"), "{reached:?}");
}

// covers: C-148
#[test]
fn an_impl_another_crate_writes_for_its_own_type_is_reached_only_from_where_it_is_seen() {
    let files: &[(&str, &str)] = &[
        (
            "lower/src/lib.rs",
            "pub struct Error;\n#[test]\nfn t() {\n    let _ = Error;\n}\n",
        ),
        ("upper/src/lib.rs", "mod f;\npub struct Theirs;\n"),
        (
            "upper/src/f.rs",
            "impl From<lower::Error> for Theirs {\n    fn from(_: lower::Error) -> Self {\n        Theirs\n    }\n}\n",
        ),
    ];
    let symbols: &[(&str, &str)] = &[
        ("t", "t()."),
        ("Error", "Error#"),
        ("Theirs", "Theirs#"),
        ("From", "rust-analyzer cargo core 1.0 convert/From#"),
        ("from", "rust-analyzer cargo core 1.0 convert/From#from()."),
        ("test", "rust-analyzer cargo core 1.0 macros/builtin/test:"),
    ];
    let fake = Fake {
        files,
        symbols,
        at: &[],
    };
    let sources: BTreeMap<String, String> = files
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
    let index = fake.index();
    let layout = Layout {
        folders: vec![
            ("lower/".into(), "lower".into()),
            ("upper/".into(), "upper".into()),
        ],
        libraries: BTreeMap::from([
            ("lower".to_string(), "lower/src/lib.rs".to_string()),
            ("upper".to_string(), "upper/src/lib.rs".to_string()),
        ]),
        sees: BTreeMap::from([
            ("lower".to_string(), BTreeSet::new()),
            ("upper".to_string(), BTreeSet::from(["lower".to_string()])),
        ]),
    };
    let build = |layout: Option<&Layout>| {
        Graph::build(&Inputs {
            sources: &sources,
            index: &index,
            prefix: "",
            packages: &BTreeSet::from(["w".to_string()]),
            proc_macros: &BTreeSet::new(),
            binaries: &BTreeSet::new(),
            external: &|_, _, _| None,
            layout,
        })
    };
    let all: BTreeSet<String> = sources.keys().cloned().collect();
    let own = set(&["lower/src/lib.rs"]);
    // `lower` can't see `upper`, so its code never converts into `Theirs`.
    let reached = build(Some(&layout)).reach("t", &own, &all).unwrap().files;
    assert!(!reached.contains("upper/src/f.rs"), "{reached:?}");
    // Without the packages' layout, every `impl` naming the type counts.
    let reached = build(None).reach("t", &own, &all).unwrap().files;
    assert!(reached.contains("upper/src/f.rs"), "{reached:?}");
}

// covers: C-147
#[test]
fn a_crates_files_are_its_root_and_the_modules_it_declares() {
    let files: &[(&str, &str)] = &[
        (
            "tests/a.rs",
            "#[path = \"common/x.rs\"]\nmod fixtures;\nmod inner;\n",
        ),
        ("tests/inner.rs", "pub fn i() {}\n"),
        ("tests/common/x.rs", "pub fn x() {}\n"),
        ("tests/common/y.rs", "pub fn y() {}\n"),
        ("tests/b.rs", "#[path = \"common/y.rs\"]\nmod fixtures;\n"),
    ];
    let fake = Fake {
        files,
        symbols: &[("path", "")],
        at: &[],
    };
    let g = fake.graph(&|_, _, _| None);
    assert_eq!(
        g.crate_files("tests/a.rs"),
        set(&["tests/a.rs", "tests/common/x.rs", "tests/inner.rs"])
    );
    assert_eq!(
        g.crate_files("tests/b.rs"),
        set(&["tests/b.rs", "tests/common/y.rs"])
    );
    assert!(g.crate_files("tests/missing.rs").is_empty());
}
