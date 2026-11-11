//! The two keys (B-5, B-6): what was asked, and what was read.
//!
//! Each key is the SHA-256 of the JCS form of a JSON array whose first element is a schema name
//! and whose second is the parts. Lists are sorted before hashing, so the order a caller found
//! things in never changes a key; two entries for one thing that disagree are an error.
//! Everything here is text: a value that isn't UTF-8 can't be keyed, and the wrapper turns that
//! error into a pass-through.

use std::collections::BTreeMap;
use std::ffi::OsStr;

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

use crate::model::{Located, RootName};

/// The schema of the first key.
pub const STEP_SCHEMA: &str = "hushrun.build-step/v0";

/// The schema of the second key.
pub const BLOCK_SCHEMA: &str = "hushrun.build-block/v0";

/// Why a key can't be computed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// A path, argument or value is not UTF-8.
    #[error("{0:?} is not UTF-8")]
    NotUtf8(String),
    /// Two entries for one thing disagree.
    #[error("two {part} entries for {what} disagree")]
    Conflict {
        /// The part of the key: `reads`, `env`, `externs`, …
        part: &'static str,
        /// The thing both entries name.
        what: String,
    },
    /// The parts have no canonical form.
    #[error("the key has no canonical form: {0}")]
    Canonical(String),
}

/// Text from the operating system, as the keys need it.
pub fn utf8(text: &OsStr) -> Result<String, KeyError> {
    text.to_str()
        .map(str::to_string)
        .ok_or_else(|| KeyError::NotUtf8(text.to_string_lossy().into_owned()))
}

/// What was asked (B-5): the parts of the first key. Roots in `args` and `env` are already
/// placeholders ([`crate::roots::Roots::to_placeholders`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asked {
    /// The compiler's identity (B-17).
    pub compiler: Digest,
    /// The arguments in order ([`crate::invocation::Invocation::key_args`]).
    pub args: Vec<String>,
    /// The keyed variables that are set. An unset one is absent, which differs from empty.
    pub env: BTreeMap<String, String>,
}

impl Asked {
    /// The first key.
    pub fn key(&self) -> Result<Digest, KeyError> {
        digest(STEP_SCHEMA, self)
    }
}

/// What a read found.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadState {
    /// The file, by its SHA-256.
    Present(Digest),
    /// No file: a place a module could have been and wasn't.
    Absent,
}

/// One file the compile read, or could have read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Read {
    /// Where it is.
    pub at: Located,
    /// What was there.
    pub state: ReadState,
}

/// One variable the compile read (an `env-dep` line), or the build script declared.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EnvRead {
    /// Its name.
    pub name: String,
    /// Its value with roots replaced, or `None` when unset.
    pub value: Option<String>,
}

impl EnvRead {
    /// A variable read from the operating system.
    pub fn from_os(name: &OsStr, value: Option<&OsStr>) -> Result<EnvRead, KeyError> {
        Ok(EnvRead {
            name: utf8(name)?,
            value: value.map(utf8).transpose()?,
        })
    }
}

/// How an extern is known.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternBy {
    /// By the SHA-256 of the file passed.
    Content(Digest),
    /// A proc macro, by the key of the block that produced it, without its place.
    ProducedBy(Digest),
    /// No file was passed: `--extern proc_macro`. The name and options are all there is.
    Builtin,
}

/// One `--extern`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExternInput {
    /// The crate name.
    pub name: String,
    /// Its options, such as `priv` or `noprelude`.
    pub options: Vec<String>,
    /// What it is known by.
    pub by: ExternBy,
}

/// The entries of a folder that directly holds a read file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Shape {
    /// The folder.
    pub dir: Located,
    /// The names of its entries.
    pub names: Vec<String>,
}

/// One file under `OUT_DIR`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Generated {
    /// Its path under `OUT_DIR`, `/`-separated.
    pub rest: String,
    /// Its SHA-256.
    pub sha256: Digest,
}

/// What the build script declared, and what macros known to read unlisted files read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declared {
    /// `rerun-if-changed` files (folders in full) and the macros' inputs.
    pub files: Vec<Read>,
    /// `rerun-if-env-changed` variables.
    pub env: Vec<EnvRead>,
}

/// One library the linker was handed (B-18).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LinkLibrary {
    /// Where it is. Kept for the hint; the key holds only the digest.
    pub at: Located,
    /// Its SHA-256.
    pub sha256: Digest,
}

/// A program's link inputs (B-18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Every library the linker was handed.
    pub libraries: Vec<LinkLibrary>,
    /// The linker's identity: its path, size and modification time, as one text.
    pub linker: String,
    /// The SDK, as one text.
    pub sdk: String,
}

/// Where a block may be reused (B-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Place {
    /// No output holds a root: anywhere on this machine.
    Anywhere,
    /// Only where the roots are these ([`crate::roots::Roots::tied`]).
    Tied(Vec<(RootName, String)>),
}

/// What was read (B-6): the parts of the second key, as a compile observed them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed {
    /// The first key.
    pub step: Digest,
    /// The files the dep-info lists, and the places a module could have been, as absent.
    pub reads: Vec<Read>,
    /// The `env-dep` lines.
    pub env: Vec<EnvRead>,
    /// Every `--extern`.
    pub externs: Vec<ExternInput>,
    /// Every folder that directly holds a read file.
    pub shape: Vec<Shape>,
    /// Every file under `OUT_DIR`; `None` when the package has no build script.
    pub generated: Option<Vec<Generated>>,
    /// For a package not from a registry or git; `None` otherwise.
    pub declared: Option<Declared>,
    /// For a program; `None` otherwise.
    pub link: Option<Link>,
    /// Where the block may be reused.
    pub place: Place,
}

impl Observed {
    /// The second key.
    pub fn key(&self) -> Result<Digest, KeyError> {
        let body = BlockBody {
            step: self.step,
            reads: normalize(&self.reads, "reads", |r| r.at.clone())?,
            env: normalize(&self.env, "env", |e| e.name.clone())?,
            externs: normalize(&self.externs, "externs", |e| {
                (e.name.clone(), e.options.clone())
            })?,
            shape: normalize_shape(&self.shape)?,
            generated: self
                .generated
                .as_ref()
                .map(|g| normalize(g, "generated", |g| g.rest.clone()))
                .transpose()?,
            declared: self
                .declared
                .as_ref()
                .map(|d| -> Result<_, KeyError> {
                    Ok(DeclaredBody {
                        files: normalize(&d.files, "declared files", |r| r.at.clone())?,
                        env: normalize(&d.env, "declared env", |e| e.name.clone())?,
                    })
                })
                .transpose()?,
            link: self.link.as_ref().map(|link| {
                let mut libraries: Vec<Digest> =
                    link.libraries.iter().map(|lib| lib.sha256).collect();
                libraries.sort();
                libraries.dedup();
                LinkBody {
                    libraries,
                    linker: link.linker.clone(),
                    sdk: link.sdk.clone(),
                }
            }),
            place: match &self.place {
                Place::Anywhere => Place::Anywhere,
                Place::Tied(roots) => {
                    let mut roots = roots.clone();
                    roots.sort();
                    roots.dedup();
                    Place::Tied(roots)
                }
            },
        };
        digest(BLOCK_SCHEMA, &body)
    }
}

/// The second key's parts, sorted, as they are hashed.
#[derive(Serialize)]
struct BlockBody {
    step: Digest,
    reads: Vec<Read>,
    env: Vec<EnvRead>,
    externs: Vec<ExternInput>,
    shape: Vec<Shape>,
    generated: Option<Vec<Generated>>,
    declared: Option<DeclaredBody>,
    link: Option<LinkBody>,
    place: Place,
}

#[derive(Serialize)]
struct DeclaredBody {
    files: Vec<Read>,
    env: Vec<EnvRead>,
}

#[derive(Serialize)]
struct LinkBody {
    libraries: Vec<Digest>,
    linker: String,
    sdk: String,
}

/// Sorts `items` by what they name, drops exact repeats, and rejects two that name one thing
/// differently.
fn normalize<T, K>(
    items: &[T],
    part: &'static str,
    key: impl Fn(&T) -> K,
) -> Result<Vec<T>, KeyError>
where
    T: Ord + Clone + std::fmt::Debug,
    K: Ord + std::fmt::Debug,
{
    let mut out = items.to_vec();
    out.sort_by(|a, b| key(a).cmp(&key(b)).then_with(|| a.cmp(b)));
    out.dedup();
    for pair in out.windows(2) {
        if key(&pair[0]) == key(&pair[1]) {
            return Err(KeyError::Conflict {
                part,
                what: format!("{:?}", key(&pair[0])),
            });
        }
    }
    Ok(out)
}

fn normalize_shape(shape: &[Shape]) -> Result<Vec<Shape>, KeyError> {
    let sorted: Vec<Shape> = shape
        .iter()
        .map(|s| {
            let mut names = s.names.clone();
            names.sort();
            names.dedup();
            Shape {
                dir: s.dir.clone(),
                names,
            }
        })
        .collect();
    normalize(&sorted, "shape", |s| s.dir.clone())
}

fn digest(schema: &str, body: &impl Serialize) -> Result<Digest, KeyError> {
    let body = serde_json::to_value(body).map_err(|e| KeyError::Canonical(e.to_string()))?;
    let value = serde_json::Value::Array(vec![schema.into(), body]);
    hushrun_core::jcs::digest(&value).map_err(|e| KeyError::Canonical(e.to_string()))
}

/// Whether the block was recorded movable or tied. The hint keeps only this: the roots a tied
/// block needs are always this call's own, never the ones recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlaceKind {
    /// Recorded as [`Place::Anywhere`].
    Anywhere,
    /// Recorded as [`Place::Tied`].
    Tied,
}

impl PlaceKind {
    /// The place to key a lookup with, from this call's roots.
    pub fn place(self, tied: Vec<(RootName, String)>) -> Place {
        match self {
            PlaceKind::Anywhere => Place::Anywhere,
            PlaceKind::Tied => Place::Tied(tied),
        }
    }
}

/// The declared part of a hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredList {
    /// The files to hash.
    pub files: Vec<Located>,
    /// The variables to read.
    pub env: Vec<String>,
}

/// The hint of B-7: what to hash before a lookup, from what an earlier compile of the same
/// first key read. Never trusted: the files are hashed as they are now, and a wrong list gives
/// a key nobody recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadList {
    /// The files, including the places recorded as absent.
    pub reads: Vec<Located>,
    /// The variable names.
    pub env: Vec<String>,
    /// The folders whose entries to list.
    pub shape: Vec<Located>,
    /// Whether to hash every file under `OUT_DIR`.
    pub generated: bool,
    /// What the build script declared, when that part applies.
    pub declared: Option<DeclaredList>,
    /// For a program, the libraries the linker was handed.
    pub link: Option<Vec<Located>>,
    /// How the block was placed.
    pub place: PlaceKind,
}

impl ReadList {
    /// The hint an observed compile leaves for the next one, sorted.
    pub fn of(observed: &Observed) -> ReadList {
        fn sorted<T: Ord>(mut items: Vec<T>) -> Vec<T> {
            items.sort();
            items.dedup();
            items
        }
        ReadList {
            reads: sorted(observed.reads.iter().map(|r| r.at.clone()).collect()),
            env: sorted(observed.env.iter().map(|e| e.name.clone()).collect()),
            shape: sorted(observed.shape.iter().map(|s| s.dir.clone()).collect()),
            generated: observed.generated.is_some(),
            declared: observed.declared.as_ref().map(|d| DeclaredList {
                files: sorted(d.files.iter().map(|r| r.at.clone()).collect()),
                env: sorted(d.env.iter().map(|e| e.name.clone()).collect()),
            }),
            link: observed
                .link
                .as_ref()
                .map(|l| sorted(l.libraries.iter().map(|lib| lib.at.clone()).collect())),
            place: match observed.place {
                Place::Anywhere => PlaceKind::Anywhere,
                Place::Tied(_) => PlaceKind::Tied,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rooted;

    fn d(byte: u8) -> Digest {
        Digest::from_bytes([byte; 32])
    }

    fn at(root: RootName, rest: &str) -> Located {
        Located::Rooted(Rooted {
            root,
            rest: rest.into(),
        })
    }

    fn asked() -> Asked {
        Asked {
            compiler: d(1),
            args: vec![
                "--crate-name=app".into(),
                "--edition=2024".into(),
                "src/lib.rs".into(),
                "--crate-type=lib".into(),
            ],
            env: BTreeMap::from([
                ("CARGO_PKG_NAME".into(), "app".into()),
                ("CARGO_MANIFEST_DIR".into(), "{hushrun:package}".into()),
            ]),
        }
    }

    fn observed() -> Observed {
        Observed {
            step: d(2),
            reads: vec![
                Read {
                    at: at(RootName::Package, "src/lib.rs"),
                    state: ReadState::Present(d(3)),
                },
                Read {
                    at: at(RootName::Package, "src/util.rs"),
                    state: ReadState::Present(d(4)),
                },
                Read {
                    at: at(RootName::Package, "src/util/mod.rs"),
                    state: ReadState::Absent,
                },
                Read {
                    at: Located::Shared("/home/dev/.cargo/registry/src/x/lib.rs".into()),
                    state: ReadState::Present(d(5)),
                },
            ],
            env: vec![
                EnvRead {
                    name: "CARGO_PKG_VERSION".into(),
                    value: Some("0.1.0".into()),
                },
                EnvRead {
                    name: "APP_FLAG".into(),
                    value: None,
                },
            ],
            externs: vec![
                ExternInput {
                    name: "serde".into(),
                    options: vec![],
                    by: ExternBy::Content(d(6)),
                },
                ExternInput {
                    name: "serde_derive".into(),
                    options: vec![],
                    by: ExternBy::ProducedBy(d(7)),
                },
            ],
            shape: vec![Shape {
                dir: at(RootName::Package, "src"),
                names: vec!["lib.rs".into(), "util.rs".into()],
            }],
            generated: Some(vec![Generated {
                rest: "gen.rs".into(),
                sha256: d(8),
            }]),
            declared: Some(Declared {
                files: vec![Read {
                    at: at(RootName::Package, "migrations/0001.sql"),
                    state: ReadState::Present(d(9)),
                }],
                env: vec![EnvRead {
                    name: "DATABASE_URL".into(),
                    value: None,
                }],
            }),
            link: None,
            place: Place::Anywhere,
        }
    }

    // covers: B-5
    #[test]
    fn first_key_is_fixed() {
        assert_eq!(
            asked().key().unwrap().hex(),
            "c415cdb42470252d283dd318f36818ff073659c2acab0ac0b2a2e87b33362b90"
        );
        // The form, spelled out: the schema name first, then the parts in JCS order.
        let text = concat!(
            r#"["hushrun.build-step/v0",{"args":["--crate-name=app","--edition=2024","#,
            r#""src/lib.rs","--crate-type=lib"],"compiler":"sha256:"#,
            "0101010101010101010101010101010101010101010101010101010101010101",
            r#"","env":{"CARGO_MANIFEST_DIR":"{hushrun:package}","CARGO_PKG_NAME":"app"}}]"#,
        );
        assert_eq!(asked().key().unwrap(), Digest::of(text.as_bytes()));
    }

    // covers: B-6
    #[test]
    fn second_key_is_fixed() {
        assert_eq!(
            observed().key().unwrap().hex(),
            "3d319725bf98712468938ebfa260434346a1907616a4b32272419a8758f85df2"
        );
    }

    // covers: B-5
    #[test]
    fn every_part_of_the_first_key_counts() {
        let base = asked().key().unwrap();
        let mut a = asked();
        a.compiler = d(9);
        assert_ne!(a.key().unwrap(), base);
        let mut a = asked();
        a.args.swap(0, 1);
        assert_ne!(a.key().unwrap(), base, "argument order counts");
        let mut a = asked();
        a.env.insert("CARGO_PKG_NAME".into(), "other".into());
        assert_ne!(a.key().unwrap(), base);
        let mut a = asked();
        a.env.insert("SDKROOT".into(), String::new());
        assert_ne!(a.key().unwrap(), base, "set to empty is not unset");
    }

    /// One change to an observed compile.
    type Change = fn(&mut Observed);

    // covers: B-6
    #[test]
    fn every_part_of_the_second_key_counts() {
        let base = observed().key().unwrap();
        let changes: Vec<(&str, Change)> = vec![
            ("step", |o| o.step = d(99)),
            ("a read's bytes", |o| {
                o.reads[0].state = ReadState::Present(d(99))
            }),
            ("a read becoming absent", |o| {
                o.reads[1].state = ReadState::Absent
            }),
            ("an absent candidate appearing", |o| {
                o.reads[2].state = ReadState::Present(d(99))
            }),
            ("a read under another root", |o| {
                o.reads[0].at = at(RootName::Workspace, "src/lib.rs")
            }),
            ("an env value becoming unset", |o| o.env[0].value = None),
            ("an unset env becoming empty", |o| {
                o.env[1].value = Some(String::new())
            }),
            ("an extern's digest", |o| {
                o.externs[0].by = ExternBy::Content(d(99))
            }),
            ("an extern known another way", |o| {
                o.externs[0].by = ExternBy::ProducedBy(d(6))
            }),
            ("an extern's options", |o| {
                o.externs[0].options = vec!["priv".into()]
            }),
            ("an added shape name", |o| {
                o.shape[0].names.push("new.rs".into())
            }),
            ("a generated file", |o| o.generated = Some(vec![])),
            ("no build script", |o| o.generated = None),
            ("a declared file", |o| {
                if let Some(d) = o.declared.as_mut() {
                    d.files[0].state = ReadState::Absent;
                }
            }),
            ("a declared variable", |o| {
                if let Some(d) = o.declared.as_mut() {
                    d.env[0].value = Some("postgres://".into());
                }
            }),
            ("declared not applying", |o| o.declared = None),
            ("link inputs", |o| {
                o.link = Some(Link {
                    libraries: vec![],
                    linker: "cc".into(),
                    sdk: String::new(),
                })
            }),
            ("place", |o| {
                o.place = Place::Tied(vec![(RootName::Workspace, "/w".into())])
            }),
        ];
        let mut keys = std::collections::BTreeSet::from([base]);
        for (what, change) in changes {
            let mut o = observed();
            change(&mut o);
            let key = o.key().unwrap();
            assert_ne!(key, base, "{what}");
            assert!(keys.insert(key), "{what} gives a key seen before");
        }
    }

    // covers: B-18
    #[test]
    fn link_inputs_count_by_content() {
        let link = |libraries: Vec<LinkLibrary>, linker: &str, sdk: &str| {
            let mut o = observed();
            o.link = Some(Link {
                libraries,
                linker: linker.into(),
                sdk: sdk.into(),
            });
            o.key().unwrap()
        };
        let lib = |rest: &str, byte| LinkLibrary {
            at: at(RootName::OutDir, rest),
            sha256: d(byte),
        };
        let base = link(
            vec![lib("a.rlib", 1), lib("b.rlib", 2)],
            "cc 10 5",
            "sdk 15",
        );
        assert_eq!(
            base,
            link(
                vec![lib("b.rlib", 2), lib("a.rlib", 1)],
                "cc 10 5",
                "sdk 15"
            )
        );
        assert_eq!(
            base,
            link(
                vec![lib("moved/a.rlib", 1), lib("b.rlib", 2)],
                "cc 10 5",
                "sdk 15"
            )
        );
        assert_ne!(
            base,
            link(
                vec![lib("a.rlib", 3), lib("b.rlib", 2)],
                "cc 10 5",
                "sdk 15"
            )
        );
        assert_ne!(
            base,
            link(
                vec![lib("a.rlib", 1), lib("b.rlib", 2)],
                "cc 10 6",
                "sdk 15"
            )
        );
        assert_ne!(
            base,
            link(
                vec![lib("a.rlib", 1), lib("b.rlib", 2)],
                "cc 10 5",
                "sdk 16"
            )
        );
    }

    // covers: B-6
    #[test]
    fn order_never_changes_the_key() {
        let base = observed().key().unwrap();
        let mut o = observed();
        o.reads.reverse();
        o.env.reverse();
        o.externs.reverse();
        o.shape[0].names.reverse();
        o.place = Place::Anywhere;
        assert_eq!(o.key().unwrap(), base);
        // A repeat that agrees is dropped.
        let mut o = observed();
        o.reads.push(o.reads[0].clone());
        o.shape[0].names.push("lib.rs".into());
        assert_eq!(o.key().unwrap(), base);
        // Tied roots in any order.
        let tied = |roots: Vec<(RootName, String)>| {
            let mut o = observed();
            o.place = Place::Tied(roots);
            o.key().unwrap()
        };
        assert_eq!(
            tied(vec![
                (RootName::Workspace, "/w".into()),
                (RootName::OutDir, "/w/t".into())
            ]),
            tied(vec![
                (RootName::OutDir, "/w/t".into()),
                (RootName::Workspace, "/w".into())
            ])
        );
    }

    // covers: B-6
    #[test]
    fn repeats_that_disagree_are_errors() {
        let mut o = observed();
        o.reads.push(Read {
            at: o.reads[0].at.clone(),
            state: ReadState::Absent,
        });
        assert!(matches!(
            o.key(),
            Err(KeyError::Conflict { part: "reads", .. })
        ));
        let mut o = observed();
        o.env.push(EnvRead {
            name: "APP_FLAG".into(),
            value: Some("1".into()),
        });
        assert!(matches!(
            o.key(),
            Err(KeyError::Conflict { part: "env", .. })
        ));
        let mut o = observed();
        o.externs.push(ExternInput {
            name: "serde".into(),
            options: vec![],
            by: ExternBy::Content(d(42)),
        });
        assert!(matches!(
            o.key(),
            Err(KeyError::Conflict {
                part: "externs",
                ..
            })
        ));
        let mut o = observed();
        o.shape.push(Shape {
            dir: at(RootName::Package, "src"),
            names: vec!["other.rs".into()],
        });
        assert!(matches!(
            o.key(),
            Err(KeyError::Conflict { part: "shape", .. })
        ));
        // The same name with other options is another extern.
        let mut o = observed();
        o.externs.push(ExternInput {
            name: "serde".into(),
            options: vec!["noprelude".into()],
            by: ExternBy::Content(d(42)),
        });
        assert!(o.key().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn text_that_is_not_utf8_is_an_error() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![b'a', 0xff]);
        assert!(matches!(
            EnvRead::from_os(OsStr::new("X"), Some(&bad)),
            Err(KeyError::NotUtf8(_))
        ));
        assert!(matches!(
            EnvRead::from_os(&bad, None),
            Err(KeyError::NotUtf8(_))
        ));
        assert_eq!(
            EnvRead::from_os(OsStr::new("X"), None).unwrap(),
            EnvRead {
                name: "X".into(),
                value: None
            }
        );
    }

    // covers: B-7
    #[test]
    fn the_hint_lists_what_to_hash_and_never_the_roots() {
        let mut o = observed();
        o.place = Place::Tied(vec![(RootName::Workspace, "/w/ws".into())]);
        o.link = Some(Link {
            libraries: vec![LinkLibrary {
                at: at(RootName::OutDir, "liba.rlib"),
                sha256: d(1),
            }],
            linker: "cc".into(),
            sdk: String::new(),
        });
        let hint = ReadList::of(&o);
        assert_eq!(hint.reads.len(), 4);
        assert!(
            hint.reads
                .contains(&at(RootName::Package, "src/util/mod.rs"))
        );
        assert_eq!(hint.env, ["APP_FLAG", "CARGO_PKG_VERSION"]);
        assert_eq!(hint.shape, [at(RootName::Package, "src")]);
        assert!(hint.generated);
        assert_eq!(
            hint.declared,
            Some(DeclaredList {
                files: vec![at(RootName::Package, "migrations/0001.sql")],
                env: vec!["DATABASE_URL".into()],
            })
        );
        assert_eq!(hint.link, Some(vec![at(RootName::OutDir, "liba.rlib")]));
        assert_eq!(hint.place, PlaceKind::Tied);
        let json = serde_json::to_string(&hint).unwrap();
        assert!(!json.contains("/w/ws"), "{json}");
        assert_eq!(serde_json::from_str::<ReadList>(&json).unwrap(), hint);

        // A lookup in another checkout keys the place with its own roots: a block tied to the
        // first checkout is never found there.
        let here = hint
            .place
            .place(vec![(RootName::Workspace, "/w/ws".into())]);
        let there = hint
            .place
            .place(vec![(RootName::Workspace, "/w/other".into())]);
        let mut a = o.clone();
        a.place = here;
        let mut b = o.clone();
        b.place = there;
        assert_eq!(a.key().unwrap(), o.key().unwrap());
        assert_ne!(b.key().unwrap(), o.key().unwrap());

        let plain = ReadList::of(&observed());
        assert_eq!(plain.place, PlaceKind::Anywhere);
        assert_eq!(plain.link, None);
        let mut none = observed();
        none.generated = None;
        none.declared = None;
        let hint = ReadList::of(&none);
        assert!(!hint.generated);
        assert_eq!(hint.declared, None);
    }
}
