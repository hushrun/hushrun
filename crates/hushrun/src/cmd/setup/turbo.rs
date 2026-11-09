//! \[ADR 0051\] Turborepo's strict mode, its default from Turborepo 2, gives a task only the
//! variables turbo.json lists and a few of its own: a package's wrapped script that `turbo run`
//! starts in CI gets neither CI's ID token nor hushrun's variables, and runs every time. Before
//! Turborepo 2.7 it gets none of GitHub's either. init lists them in turbo.json's
//! `globalPassThroughEnv`, which passes them on without making them part of any task's cache key.

use std::path::Path;

use serde_json::{Value, json};

use super::json::{self, Doc, Place};
use super::workspaces;

/// The files Turborepo reads its settings from, at the root.
const FILES: &[&str] = &["turbo.json", "turbo.jsonc"];

/// Where Prettier reads its settings at the root, after package.json's `prettier`, in its order.
const PRETTIER: &[&str] = &[
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.yaml",
    ".prettierrc.yml",
    ".prettierrc.json5",
    ".prettierrc.js",
    "prettier.config.js",
    ".prettierrc.mjs",
    "prettier.config.mjs",
    ".prettierrc.cjs",
    "prettier.config.cjs",
    ".prettierrc.toml",
];

/// Prettier's and Biome's line width when nothing sets one.
const WIDTH: usize = 80;

/// What the root's turbo.json needs so that the scripts turbo runs get hushrun's variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Nothing: no turbo.json, a mode that passes every variable, or every one passed already.
    None,
    /// The file's new text, and what changed.
    Text {
        path: String,
        text: String,
        changes: Vec<String>,
    },
    /// What to change by hand, since init can't.
    Manual(String),
}

/// The variables a wrapped script needs in a job of GitHub's, or of GitLab's: hushrun's, and
/// those that tell the run where it is and get its ID token.
pub fn needed(github: bool, gitlab: bool) -> Vec<&'static str> {
    let mut names = Vec::new();
    if github {
        names.extend(["ACTIONS_ID_TOKEN_REQUEST_*", "GITHUB_*"]);
    }
    if gitlab {
        names.extend(["GITLAB_CI", "CI_*"]);
    }
    names.push("HUSHRUN_*");
    names
}

/// The edit to the root's turbo.json that passes `needed` on to every task.
pub fn edit(root: &Path, needed: &[&str]) -> Edit {
    let Some((path, text)) = FILES.iter().find_map(|name| {
        let text = std::fs::read_to_string(root.join(name)).ok()?;
        Some(((*name).to_string(), text))
    }) else {
        return Edit::None;
    };
    let by_hand = |why: &str| {
        Edit::Manual(format!(
            "{path}: {why}; add {} to its \"globalPassThroughEnv\", since turbo passes the scripts it runs only the variables it lists, and hushrun needs these there in CI",
            quoted(needed)
        ))
    };
    // JSON with comments, which turbo reads too, keeps them through the edit.
    let (doc, plain) = match Doc::parse(&text) {
        Ok(doc) => (doc, true),
        Err(_) => match Doc::parse_jsonc(&text) {
            Ok(doc) => (doc, false),
            Err(_) => return by_hand("hushrun can't read it"),
        },
    };
    let read = |text: &str| match plain {
        true => json::value(text).ok(),
        false => workspaces::jsonc(text),
    };
    let top = doc.root();
    let Some(mut expected) = read(&text).filter(|_| top.members().is_some()) else {
        return by_hand("it isn't an object");
    };
    if expected.get("envMode").and_then(Value::as_str) == Some("loose") {
        return Edit::None;
    }
    let listed = top.get("globalPassThroughEnv");
    // Turborepo 1 runs a task strictly only where turbo.json passes some variable through: a new
    // list would make every task strict.
    if listed.is_none() && expected.get("tasks").is_none() && expected.get("pipeline").is_some() {
        return Edit::None;
    }
    let mut editor = doc.editor();
    // A format check in CI would fail on a list laid out otherwise.
    editor.wrap_at(line_width(root));
    let missing: Vec<&str> = match listed {
        None => {
            let place = match (top.index_of("globalEnv"), top.index_of("tasks")) {
                (Some(at), _) => Place::After(at),
                (None, Some(0)) => Place::First,
                (None, Some(at)) => Place::After(at - 1),
                (None, None) => Place::Last,
            };
            editor.insert(top, place, "globalPassThroughEnv", json!(needed));
            expected["globalPassThroughEnv"] = json!(needed);
            needed.to_vec()
        }
        Some(list) => {
            let Some(items) = list.items() else {
                return by_hand("its \"globalPassThroughEnv\" isn't a list");
            };
            let entries: Vec<&str> = items.iter().filter_map(json::Node::as_str).collect();
            let missing: Vec<&str> = needed
                .iter()
                .copied()
                .filter(|name| !entries.iter().any(|entry| covers(entry, name)))
                .collect();
            if missing.is_empty() {
                return Edit::None;
            }
            for name in &missing {
                editor.push(list, json!(name));
                if let Some(values) = expected["globalPassThroughEnv"].as_array_mut() {
                    values.push(json!(name));
                }
            }
            missing
        }
    };
    let edited = editor.finish();
    if read(&edited).as_ref() != Some(&expected) {
        return by_hand("hushrun can't edit it safely");
    }
    Edit::Text {
        path,
        text: edited,
        changes: vec![format!(
            "globalPassThroughEnv: {}, since turbo passes the scripts it runs only the variables it lists, and hushrun needs these there in CI",
            quoted(&missing)
        )],
    }
}

/// The line width the repository's formatter keeps JSON to: Prettier's `printWidth`, from
/// package.json's `prettier` or the first of its files, else Biome's `lineWidth`, else 80.
fn line_width(root: &Path) -> usize {
    let read = |name: &str| std::fs::read_to_string(root.join(name)).ok();
    let number = |value: &Value, path: &[&str]| {
        let found = path.iter().try_fold(value, |value, key| value.get(key))?;
        usize::try_from(found.as_u64()?).ok()
    };
    if let Some(manifest) = read("package.json").and_then(|text| json::value(&text).ok())
        && manifest.get("prettier").is_some_and(Value::is_object)
    {
        return number(&manifest, &["prettier", "printWidth"]).unwrap_or(WIDTH);
    }
    if let Some(text) = PRETTIER.iter().find_map(|name| read(name)) {
        return said_width(&text).unwrap_or(WIDTH);
    }
    for name in ["biome.json", "biome.jsonc"] {
        if let Some(settings) = read(name).and_then(|text| workspaces::jsonc(&text)) {
            return number(&settings, &["json", "formatter", "lineWidth"])
                .or_else(|| number(&settings, &["formatter", "lineWidth"]))
                .unwrap_or(WIDTH);
        }
    }
    WIDTH
}

/// The number a settings file gives `printWidth`, as JSON, YAML, TOML or JavaScript write it:
/// `"printWidth": 100`, `printWidth: 100` or `printWidth = 100`.
fn said_width(text: &str) -> Option<usize> {
    let (_, rest) = text.split_once("printWidth")?;
    let rest = rest.trim_start_matches(['"', '\'']).trim_start();
    let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix('='))?;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Whether `entry` of a pass-through list passes every variable `name` stands for: the same name,
/// or a pattern that ends in `*` and starts as `name` does.
fn covers(entry: &str, name: &str) -> bool {
    match entry.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => entry == name,
    }
}

fn quoted(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| json::quote(name))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit_of(text: &str) -> Edit {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("turbo.json"), text).unwrap();
        edit(dir.path(), &needed(true, false))
    }

    fn text_of(edit: Edit) -> String {
        match edit {
            Edit::Text { text, .. } => text,
            other => panic!("{other:?}"),
        }
    }

    // covers: C-14, C-15
    #[test]
    fn strict_mode_gets_hushruns_variables_listed() {
        // A new list goes after globalEnv, else before the tasks.
        assert_eq!(
            text_of(edit_of(
                "{\n  \"$schema\": \"https://turborepo.com/schema.json\",\n  \"tasks\": {\n    \"test\": {}\n  }\n}\n"
            )),
            "{\n  \"$schema\": \"https://turborepo.com/schema.json\",\n  \"globalPassThroughEnv\": [\n    \"ACTIONS_ID_TOKEN_REQUEST_*\",\n    \"GITHUB_*\",\n    \"HUSHRUN_*\"\n  ],\n  \"tasks\": {\n    \"test\": {}\n  }\n}\n"
        );
        assert_eq!(
            text_of(edit_of(
                "{\n  \"globalEnv\": [\"NODE_ENV\"],\n  \"ui\": \"tui\",\n  \"tasks\": {}\n}"
            )),
            "{\n  \"globalEnv\": [\"NODE_ENV\"],\n  \"globalPassThroughEnv\": [\n    \"ACTIONS_ID_TOKEN_REQUEST_*\",\n    \"GITHUB_*\",\n    \"HUSHRUN_*\"\n  ],\n  \"ui\": \"tui\",\n  \"tasks\": {}\n}"
        );
        // A list there gets what it lacks, after its own, comments and all, an item a line past
        // 80 columns.
        let made = edit_of(
            "{\n  // CI's\n  \"globalPassThroughEnv\": [\"GITHUB_TOKEN\", \"HUSHRUN_*\"],\n  \"tasks\": {},\n}\n",
        );
        let Edit::Text { text, changes, .. } = made else {
            panic!("{made:?}");
        };
        assert_eq!(
            text,
            "{\n  // CI's\n  \"globalPassThroughEnv\": [\n    \"GITHUB_TOKEN\",\n    \"HUSHRUN_*\",\n    \"ACTIONS_ID_TOKEN_REQUEST_*\",\n    \"GITHUB_*\"\n  ],\n  \"tasks\": {},\n}\n"
        );
        assert_eq!(
            changes,
            [
                "globalPassThroughEnv: \"ACTIONS_ID_TOKEN_REQUEST_*\", \"GITHUB_*\", since turbo passes the scripts it runs only the variables it lists, and hushrun needs these there in CI"
            ]
        );
        // GitLab's job needs GitLab's.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("turbo.jsonc"), "{\"tasks\": {}}").unwrap();
        assert_eq!(
            text_of(edit(dir.path(), &needed(false, true))),
            "{\"globalPassThroughEnv\": [\"GITLAB_CI\", \"CI_*\", \"HUSHRUN_*\"], \"tasks\": {}}"
        );
    }

    // covers: C-14, C-15
    #[test]
    fn what_passes_every_variable_already_stays() {
        for text in [
            "{\"envMode\": \"loose\", \"tasks\": {}}",
            "{\"globalPassThroughEnv\": [\"*\"], \"tasks\": {}}",
            "{\"globalPassThroughEnv\": [\"ACTIONS_*\", \"GITHUB_*\", \"HUSH*\"], \"tasks\": {}}",
            // Turborepo 1 passes every variable where turbo.json lists none.
            "{\"pipeline\": {\"test\": {}}}",
        ] {
            assert_eq!(edit_of(text), Edit::None, "{text}");
        }
        assert_eq!(
            edit(tempfile::tempdir().unwrap().path(), &needed(true, false)),
            Edit::None
        );
        // Turborepo 1 with a list of its own is strict already.
        assert!(matches!(
            edit_of("{\"globalPassThroughEnv\": [], \"pipeline\": {}}"),
            Edit::Text { .. }
        ));
    }

    // covers: C-15
    #[test]
    fn a_list_is_laid_out_as_the_formatter_would() {
        let one_line = "{\n  \"globalPassThroughEnv\": [\"RUSTC_WRAPPER\", \"SCCACHE_*\"],\n  \"tasks\": {}\n}\n";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("turbo.json"), one_line).unwrap();
        // Past Prettier's 80 columns, an item a line.
        assert_eq!(
            text_of(edit(dir.path(), &needed(true, false))),
            "{\n  \"globalPassThroughEnv\": [\n    \"RUSTC_WRAPPER\",\n    \"SCCACHE_*\",\n    \"ACTIONS_ID_TOKEN_REQUEST_*\",\n    \"GITHUB_*\",\n    \"HUSHRUN_*\"\n  ],\n  \"tasks\": {}\n}\n"
        );
        // Within the repository's own width, one line.
        std::fs::write(
            dir.path().join("package.json"),
            "{\"prettier\": {\"printWidth\": 120}}",
        )
        .unwrap();
        assert_eq!(
            text_of(edit(dir.path(), &needed(true, false))),
            "{\n  \"globalPassThroughEnv\": [\"RUSTC_WRAPPER\", \"SCCACHE_*\", \"ACTIONS_ID_TOKEN_REQUEST_*\", \"GITHUB_*\", \"HUSHRUN_*\"],\n  \"tasks\": {}\n}\n"
        );
        // A new list that fits stays on one line too.
        std::fs::write(dir.path().join("turbo.json"), "{\n  \"tasks\": {}\n}\n").unwrap();
        assert_eq!(
            text_of(edit(dir.path(), &needed(false, true))),
            "{\n  \"globalPassThroughEnv\": [\"GITLAB_CI\", \"CI_*\", \"HUSHRUN_*\"],\n  \"tasks\": {}\n}\n"
        );
    }

    #[test]
    fn the_width_comes_from_prettier_then_biome() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(line_width(root), 80);
        std::fs::write(
            root.join("biome.jsonc"),
            "{\n  // ours\n  \"formatter\": {\"lineWidth\": 100},\n}",
        )
        .unwrap();
        assert_eq!(line_width(root), 100);
        std::fs::write(
            root.join("biome.json"),
            "{\"json\": {\"formatter\": {\"lineWidth\": 90}}, \"formatter\": {\"lineWidth\": 100}}",
        )
        .unwrap();
        assert_eq!(line_width(root), 90);
        std::fs::write(
            root.join(".prettierrc.yaml"),
            "semi: false\nprintWidth: 110\n",
        )
        .unwrap();
        assert_eq!(line_width(root), 110);
        std::fs::write(root.join(".prettierrc"), "{}").unwrap();
        assert_eq!(line_width(root), 80);
        std::fs::write(
            root.join("package.json"),
            "{\"prettier\": {\"printWidth\": 120}}",
        )
        .unwrap();
        assert_eq!(line_width(root), 120);
        // A shared config by name: its width is the package's, which init doesn't read.
        std::fs::write(
            root.join("package.json"),
            "{\"prettier\": \"@acme/prettier\"}",
        )
        .unwrap();
        assert_eq!(line_width(root), 80);
        for (text, width) in [
            ("module.exports = { printWidth: 100 };", Some(100)),
            ("printWidth = 96", Some(96)),
            ("{\"printWidth\": 72}", Some(72)),
            ("{\"semi\": false}", None),
        ] {
            assert_eq!(said_width(text), width, "{text}");
        }
    }

    // covers: C-14
    #[test]
    fn what_init_cant_edit_it_asks_for_by_hand() {
        for (text, why) in [
            (
                "{\"tasks\": {",
                "turbo.json: hushrun can't read it; add \"ACTIONS_ID_TOKEN_REQUEST_*\", \"GITHUB_*\", \"HUSHRUN_*\" to its \"globalPassThroughEnv\"",
            ),
            ("[1]", "turbo.json: it isn't an object;"),
            (
                "{\"globalPassThroughEnv\": \"HUSHRUN_*\", \"tasks\": {}}",
                "turbo.json: its \"globalPassThroughEnv\" isn't a list;",
            ),
        ] {
            match edit_of(text) {
                Edit::Manual(line) => assert!(line.starts_with(why), "{line}"),
                other => panic!("{text}: {other:?}"),
            }
        }
    }
}
