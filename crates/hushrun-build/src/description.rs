//! What a block records about itself beside its files (B-13): the store keeps it as the block's
//! description and a reuse reads it back.
//!
//! Every path in it that lay under a root is a placeholder, so a reuse writes this folder's
//! paths. The top-level `"repository"` is what `hushrun build status` groups blocks by.

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

use crate::depinfo::{DepInfo, EnvDep};
use crate::key::PlaceKind;
use crate::messages;
use crate::model::Class;

/// The schema name every description carries. One with another name is not read.
pub const SCHEMA: &str = "hushrun.build.description/1";

/// What one output file of a block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputKind {
    /// The `.rmeta`.
    Metadata,
    /// The linked output: `.rlib`, a proc macro's library, a program.
    Link,
    /// A debug object a program names (macOS keeps them beside it).
    DebugObject,
}

/// One file of a block, by its name in the output folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// The file's name in the output folder, and in the block.
    pub name: String,
    /// What it is.
    pub kind: OutputKind,
}

/// One `# env-dep:` line, with roots in its value as placeholders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEnv {
    /// The variable's name.
    pub name: String,
    /// Its value, or `None` when it was unset.
    pub value: Option<String>,
}

/// A dep-info file in parsed form, with roots as placeholders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDepInfo {
    /// The outputs it names.
    pub targets: Vec<String>,
    /// The files it lists.
    pub files: Vec<String>,
    /// The variables it lists.
    pub env: Vec<StoredEnv>,
}

impl StoredDepInfo {
    /// The dep-info with every path and value changed by `map` (to placeholders).
    pub fn of(info: &DepInfo, map: impl Fn(&str) -> String) -> StoredDepInfo {
        StoredDepInfo {
            targets: info.targets.iter().map(|t| map(t)).collect(),
            files: info.files.iter().map(|f| map(f)).collect(),
            env: info
                .env
                .iter()
                .map(|e| StoredEnv {
                    name: e.name.clone(),
                    value: e.value.as_deref().map(&map),
                })
                .collect(),
        }
    }

    /// The dep-info for this folder: each placeholder resolved by `map`, which fails for a root
    /// this call doesn't have.
    pub fn render<E>(&self, map: impl Fn(&str) -> Result<String, E>) -> Result<DepInfo, E> {
        Ok(DepInfo {
            targets: self
                .targets
                .iter()
                .map(|t| map(t))
                .collect::<Result<_, _>>()?,
            files: self
                .files
                .iter()
                .map(|f| map(f))
                .collect::<Result<_, _>>()?,
            env: self
                .env
                .iter()
                .map(|e| {
                    Ok(EnvDep {
                        name: e.name.clone(),
                        value: e.value.as_deref().map(&map).transpose()?,
                    })
                })
                .collect::<Result<_, _>>()?,
        })
    }
}

/// A block's description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Description {
    /// [`SCHEMA`].
    pub schema: String,
    /// The repository that built it, when hushrun knew it.
    pub repository: Option<String>,
    /// What the compile produced.
    pub class: Class,
    /// `--crate-name`.
    pub crate_name: String,
    /// `CARGO_PKG_NAME`, when set.
    pub package: Option<String>,
    /// The first key (B-5). A dependent that can't load the block suspends it (B-15).
    pub step: Digest,
    /// The second key computed without its place: what a proc macro is known by to its
    /// dependents (B-6).
    pub origin: Digest,
    /// Whether the block may move (B-10).
    pub place: PlaceKind,
    /// Its files, by name and kind, in the store's order.
    pub outputs: Vec<Output>,
    /// The dep-info file, in parsed form with placeholders. Not a file of the block: it holds
    /// real paths by design and is written for each folder.
    pub dep_info: StoredDepInfo,
    /// The messages rustc wrote, in the form it wrote them (B-12).
    pub messages: messages::Stored,
    /// How long rustc took, in milliseconds.
    pub rustc_ms: u64,
    /// When the block was built, in milliseconds since the epoch.
    pub built_ms: u64,
}

impl Description {
    /// The description as the store keeps it.
    pub fn to_value(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// Reads a description the store gave back. `None` for another schema or a damaged one.
    pub fn from_value(value: &serde_json::Value) -> Option<Description> {
        let description: Description = serde_json::from_value(value.clone()).ok()?;
        (description.schema == SCHEMA).then_some(description)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{Form, Recorder};

    fn description() -> Description {
        let mut recorder = Recorder::new(Vec::new(), Form::new(Some("json"), None, None));
        recorder
            .line(
                r#"{"$message_type":"artifact","artifact":"/w/out/liba.rmeta","emit":"metadata"}"#,
            )
            .unwrap();
        Description {
            schema: SCHEMA.into(),
            repository: Some("hushrun/hushrun".into()),
            class: Class::Library,
            crate_name: "a".into(),
            package: Some("a".into()),
            step: Digest::of(b"step"),
            origin: Digest::of(b"origin"),
            place: PlaceKind::Anywhere,
            outputs: vec![Output {
                name: "liba.rmeta".into(),
                kind: OutputKind::Metadata,
            }],
            dep_info: StoredDepInfo {
                targets: vec!["{hushrun:out-dir}/a.d".into()],
                files: vec!["src/lib.rs".into()],
                env: vec![StoredEnv {
                    name: "X".into(),
                    value: None,
                }],
            },
            messages: recorder
                .finish()
                .store(|p| p.replace("/w/out", "{hushrun:out-dir}")),
            rustc_ms: 12,
            built_ms: 1_800_000_000_000,
        }
    }

    // covers: B-13
    #[test]
    fn round_trips_and_names_its_repository_at_the_top() {
        let value = description().to_value().unwrap();
        assert_eq!(value["repository"], "hushrun/hushrun");
        assert_eq!(Description::from_value(&value), Some(description()));
        let mut other = value.clone();
        other["schema"] = "other/1".into();
        assert_eq!(Description::from_value(&other), None);
        assert_eq!(Description::from_value(&serde_json::json!({})), None);
    }

    #[test]
    fn dep_info_goes_through_placeholders_and_back() {
        let info = DepInfo {
            targets: vec!["/w/out/a.d".into()],
            files: vec!["src/lib.rs".into(), "/w/out/gen.rs".into()],
            env: vec![EnvDep {
                name: "OUT".into(),
                value: Some("/w/out".into()),
            }],
        };
        let stored = StoredDepInfo::of(&info, |p| p.replace("/w/out", "{o}"));
        assert_eq!(stored.files[1], "{o}/gen.rs");
        let back = stored
            .render(|p| Ok::<_, ()>(p.replace("{o}", "/v/out")))
            .unwrap();
        assert_eq!(back.targets, ["/v/out/a.d"]);
        assert_eq!(back.env[0].value.as_deref(), Some("/v/out"));
        assert!(stored.render(|_| Err::<String, _>("no root")).is_err());
    }
}
