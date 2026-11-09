//! What a Cargo manifest takes from its workspace, for a project's scope (C-15). \[ADR 0055\] A
//! crate that inherits, `edition.workspace = true` or `serde = { workspace = true }`, builds with its
//! workspace's manifest, and with the path dependencies it inherits from there: another project
//! that depends on it by path builds with those too. A crate inside a workspace builds as part of it,
//! with its lockfile, its other members and its `[patch]`, so a project there reads all of it.
//!
//! A manifest is read by a small TOML reader of its own, which takes the TOML a manifest is written
//! in and gives up on anything else: what it can't read keeps the repository in one scope.

use std::path::Path;

use super::projects::{joined, normal};

/// A value in a manifest, as far as a scope needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Text(String),
    Bool(bool),
    /// An array of strings.
    Texts(Vec<String>),
    /// Anything else: a number, a date, an array of other values.
    Other,
}

/// A manifest's keys, each by its path from the top, and its tables, headers and inline ones.
#[derive(Debug, Default)]
struct Manifest {
    keys: Vec<(Vec<String>, Value)>,
    tables: Vec<Vec<String>>,
}

/// The tables a crate's dependencies are in, and whether a project that depends on the crate builds
/// them: a dependent never builds its dev-dependencies.
const DEPENDENCIES: &[(&str, bool)] = &[
    ("dependencies", true),
    ("build-dependencies", true),
    ("build_dependencies", true),
    ("dev-dependencies", false),
    ("dev_dependencies", false),
];

impl Manifest {
    fn get(&self, path: &[&str]) -> Option<&Value> {
        self.keys
            .iter()
            .find(|(key, _)| key.iter().map(String::as_str).eq(path.iter().copied()))
            .map(|(_, value)| value)
    }

    fn text(&self, path: &[&str]) -> Option<&str> {
        match self.get(path)? {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Whether it heads a workspace: it has a `[workspace]` table.
    fn heads_a_workspace(&self) -> bool {
        let top = |path: &Vec<String>| path.first().is_some_and(|first| first == "workspace");
        self.tables.iter().any(top) || self.keys.iter().any(|(path, _)| top(path))
    }

    /// Whether anything in it inherits from a workspace.
    fn inherits(&self) -> bool {
        self.keys.iter().any(|(path, value)| {
            path.last().is_some_and(|last| last == "workspace") && *value == Value::Bool(true)
        })
    }

    /// The dependencies it inherits: of every kind when `dev`, else those a dependent builds too.
    fn inherited_dependencies(&self, dev: bool) -> Vec<String> {
        let mut names = Vec::new();
        for (path, value) in &self.keys {
            let parts: Vec<&str> = path.iter().map(String::as_str).collect();
            let (table, name) = match parts.as_slice() {
                [table, name, "workspace"] | ["target", _, table, name, "workspace"] => {
                    (*table, *name)
                }
                _ => continue,
            };
            let builds = DEPENDENCIES
                .iter()
                .find(|(kind, _)| *kind == table)
                .map(|(_, built)| *built || dev);
            if builds == Some(true)
                && *value == Value::Bool(true)
                && !names.iter().any(|n| n == name)
            {
                names.push(name.to_string());
            }
        }
        names
    }

    /// Whether the workspace it heads, at `root_dir`, leaves `dir` out.
    fn excludes(&self, root_dir: &str, dir: &str) -> bool {
        let Some(Value::Texts(paths)) = self.get(&["workspace", "exclude"]) else {
            return false;
        };
        paths.iter().any(|path| {
            normal(&joined(root_dir, path)).is_some_and(|out| {
                dir == out || dir.starts_with(&format!("{out}/")) || out.is_empty()
            })
        })
    }
}

/// `dir`'s manifest, if it has one.
fn manifest_of(root: &Path, dir: &str) -> Option<String> {
    std::fs::read_to_string(root.join(dir).join("Cargo.toml")).ok()
}

/// The paths `dir`'s manifest, `text`, reads from its workspace, as paths from `dir`: the
/// workspace's manifest, and each path dependency it inherits. `own` says `dir` is the project
/// itself, which builds its dev-dependencies too. None when it inherits nothing.
pub(super) fn inherited(
    root: &Path,
    dir: &str,
    text: &str,
    own: bool,
) -> Result<Vec<String>, String> {
    let manifest = read(text).ok_or_else(|| format!("hushrun can't read {dir}/Cargo.toml"))?;
    if !manifest.inherits() {
        return Ok(Vec::new());
    }
    let Some(at) = workspace_of(root, dir, &manifest)? else {
        return Err(format!(
            "{dir}/Cargo.toml inherits from a workspace hushrun can't find"
        ));
    };
    let names = manifest.inherited_dependencies(own);
    let workspace = match at == dir {
        true => manifest,
        false => manifest_of(root, &at)
            .as_deref()
            .and_then(read)
            .ok_or_else(|| format!("hushrun can't read {}", file_of(&at)))?,
    };
    // From `dir` up to the root, then down: `normal` works out the rest.
    let up = "../".repeat(dir.split('/').filter(|part| !part.is_empty()).count());
    let mut paths = Vec::new();
    if at != dir {
        paths.push(format!("{up}{}", file_of(&at)));
    }
    for name in names {
        let given = workspace.keys.iter().any(|(path, _)| {
            path.len() > 2 && path[0] == "workspace" && path[1] == "dependencies" && path[2] == name
        });
        if !given {
            return Err(format!(
                "{dir}/Cargo.toml inherits {name}, which {} doesn't give",
                file_of(&at)
            ));
        }
        if let Some(path) = workspace.text(&["workspace", "dependencies", &name, "path"]) {
            paths.push(format!("{up}{}", joined(&at, path)));
        }
    }
    Ok(paths)
}

/// \[ADR 0055\] The workspace `dir`, a crate, builds in when it is one of its members, the directory
/// of its manifest from the repository's root: the one `package.workspace` names, else the
/// nearest above that heads a workspace and doesn't leave `dir` out, as Cargo finds it. None when
/// it is no member, or heads its own workspace.
pub(super) fn workspace_above(root: &Path, dir: &str) -> Result<Option<String>, String> {
    let Some(text) = manifest_of(root, dir) else {
        return Ok(None);
    };
    // Only a word of a workspace, here or in a manifest above, needs the manifests read: one
    // hushrun can't read changes nothing until then.
    let above = parents(dir)
        .any(|parent| manifest_of(root, &parent).is_some_and(|text| text.contains("workspace")));
    if !above && !text.contains("workspace") {
        return Ok(None);
    }
    let manifest = read(&text).ok_or_else(|| format!("hushrun can't read {dir}/Cargo.toml"))?;
    match workspace_of(root, dir, &manifest)? {
        Some(at) if at != dir => Ok(Some(at)),
        _ => Ok(None),
    }
}

/// Where `dir`'s manifest, `manifest`, finds its workspace, as Cargo does.
fn workspace_of(root: &Path, dir: &str, manifest: &Manifest) -> Result<Option<String>, String> {
    if let Some(path) = manifest.text(&["package", "workspace"]) {
        let at = normal(&joined(dir, path))
            .filter(|at| !(at == ".." || at.starts_with("../")))
            .ok_or_else(|| format!("{dir}/Cargo.toml names a workspace outside the repository"))?;
        return Ok(Some(at));
    }
    if manifest.heads_a_workspace() {
        return Ok(Some(dir.to_string()));
    }
    for parent in parents(dir) {
        let Some(text) = manifest_of(root, &parent) else {
            continue;
        };
        let Some(above) = read(&text) else {
            if text.contains("workspace") {
                return Err(format!("hushrun can't read {}", file_of(&parent)));
            }
            continue;
        };
        if above.heads_a_workspace() && !above.excludes(&parent, dir) {
            return Ok(Some(parent));
        }
    }
    Ok(None)
}

/// The directories above `dir`, nearest first, up to the repository's root, `""`.
fn parents(dir: &str) -> impl Iterator<Item = String> + '_ {
    let mut here = dir;
    std::iter::from_fn(move || {
        if here.is_empty() {
            return None;
        }
        here = here.rsplit_once('/').map_or("", |(parent, _)| parent);
        Some(here.to_string())
    })
}

/// The manifest of the crate or workspace at `dir`.
fn file_of(dir: &str) -> String {
    joined(dir, "Cargo.toml")
}

/// A manifest's keys and tables, or None when it holds what this reader doesn't take.
fn read(text: &str) -> Option<Manifest> {
    let mut reader = Reader {
        text: text.as_bytes(),
        at: 0,
    };
    let mut manifest = Manifest::default();
    let mut table: Vec<String> = Vec::new();
    loop {
        reader.blank();
        let Some(next) = reader.peek() else {
            return Some(manifest);
        };
        if next == b'[' {
            reader.at += 1;
            let array = reader.eat(b'[');
            reader.spaces();
            table = reader.key()?;
            reader.spaces();
            if !reader.eat(b']') || (array && !reader.eat(b']')) {
                return None;
            }
            manifest.tables.push(table.clone());
        } else {
            let mut path = table.clone();
            path.extend(reader.key()?);
            reader.spaces();
            if !reader.eat(b'=') {
                return None;
            }
            reader.spaces();
            reader.value(path, &mut manifest)?;
        }
        reader.line_end()?;
    }
}

struct Reader<'a> {
    text: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        if found {
            self.at += 1;
        }
        found
    }

    fn rest(&self) -> &[u8] {
        &self.text[self.at.min(self.text.len())..]
    }

    /// Spaces and tabs.
    fn spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.at += 1;
        }
    }

    /// Whitespace, line breaks and comments.
    fn blank(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.at += 1,
                Some(b'#') => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.at += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// The end of a line: spaces, perhaps a comment, then a line break or the end.
    fn line_end(&mut self) -> Option<()> {
        self.spaces();
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.at += 1;
            }
        }
        self.eat(b'\r');
        match self.peek() {
            None => Some(()),
            Some(b'\n') => {
                self.at += 1;
                Some(())
            }
            Some(_) => None,
        }
    }

    /// A key, dotted or not: `a`, `a.b`, `"a b".c`.
    fn key(&mut self) -> Option<Vec<String>> {
        let mut parts = vec![self.simple_key()?];
        loop {
            let before = self.at;
            self.spaces();
            if !self.eat(b'.') {
                self.at = before;
                return Some(parts);
            }
            self.spaces();
            parts.push(self.simple_key()?);
        }
    }

    fn simple_key(&mut self) -> Option<String> {
        match self.peek()? {
            b'"' | b'\''
                if self.rest().starts_with(b"\"\"\"") || self.rest().starts_with(b"'''") =>
            {
                None
            }
            b'"' | b'\'' => self.string(),
            _ => {
                let start = self.at;
                while self
                    .peek()
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                {
                    self.at += 1;
                }
                (self.at > start)
                    .then(|| String::from_utf8_lossy(&self.text[start..self.at]).into_owned())
            }
        }
    }

    /// The value of the key at `path`, put in `manifest`.
    fn value(&mut self, path: Vec<String>, manifest: &mut Manifest) -> Option<()> {
        match self.peek()? {
            b'"' | b'\'' => {
                let text = self.string()?;
                manifest.keys.push((path, Value::Text(text)));
            }
            b'{' => {
                self.at += 1;
                manifest.tables.push(path.clone());
                self.blank();
                if self.eat(b'}') {
                    return Some(());
                }
                loop {
                    self.blank();
                    let mut inner = path.clone();
                    inner.extend(self.key()?);
                    self.spaces();
                    if !self.eat(b'=') {
                        return None;
                    }
                    self.spaces();
                    self.value(inner, manifest)?;
                    self.blank();
                    if self.eat(b',') {
                        // TOML 1.1 allows a comma after the last pair.
                        self.blank();
                        if self.eat(b'}') {
                            return Some(());
                        }
                        continue;
                    }
                    if self.eat(b'}') {
                        return Some(());
                    }
                    return None;
                }
            }
            b'[' => {
                self.at += 1;
                let mut texts = Some(Vec::new());
                loop {
                    self.blank();
                    if self.eat(b']') {
                        break;
                    }
                    if matches!(self.peek()?, b'"' | b'\'') {
                        let text = self.string()?;
                        if let Some(texts) = &mut texts {
                            texts.push(text);
                        }
                    } else {
                        let mut ignored = Manifest::default();
                        self.value(Vec::new(), &mut ignored)?;
                        texts = None;
                    }
                    self.blank();
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        break;
                    }
                    return None;
                }
                manifest
                    .keys
                    .push((path, texts.map_or(Value::Other, Value::Texts)));
            }
            _ => {
                let start = self.at;
                while !matches!(
                    self.peek(),
                    None | Some(b',' | b']' | b'}' | b'\n' | b'\r' | b'#')
                ) {
                    self.at += 1;
                }
                let word = std::str::from_utf8(&self.text[start..self.at])
                    .ok()?
                    .trim_end();
                let value = match word {
                    "" => return None,
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    // A number or a date: nothing a scope needs, but it is a value.
                    word if word
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"+-_.: ".contains(&b)) =>
                    {
                        Value::Other
                    }
                    _ => return None,
                };
                manifest.keys.push((path, value));
            }
        }
        Some(())
    }

    /// A string of any of TOML's four kinds.
    fn string(&mut self) -> Option<String> {
        let rest = self.rest();
        if rest.starts_with(b"\"\"\"") {
            self.at += 3;
            return self.long_string(b'"');
        }
        if rest.starts_with(b"'''") {
            self.at += 3;
            return self.long_string(b'\'');
        }
        let quote = self.peek()?;
        self.at += 1;
        let mut out = Vec::new();
        loop {
            match self.peek()? {
                b'\n' => return None,
                byte if byte == quote => {
                    self.at += 1;
                    return String::from_utf8(out).ok();
                }
                b'\\' if quote == b'"' => {
                    self.at += 1;
                    self.escape(&mut out)?;
                }
                byte => {
                    out.push(byte);
                    self.at += 1;
                }
            }
        }
    }

    /// The rest of a multi-line string, after its opening quotes.
    fn long_string(&mut self, quote: u8) -> Option<String> {
        // A line break right after the opening quotes isn't part of it.
        if self.rest().starts_with(b"\r\n") {
            self.at += 2;
        } else {
            self.eat(b'\n');
        }
        let mut out = Vec::new();
        loop {
            let byte = self.peek()?;
            if byte == quote && self.rest().starts_with(&[quote; 3]) {
                let run = self.rest().iter().take_while(|b| **b == quote).count();
                // Up to two quotes before the closing three are the string's own.
                out.extend(std::iter::repeat_n(quote, run.saturating_sub(3).min(2)));
                self.at += run.min(5);
                return String::from_utf8(out).ok();
            }
            if byte == b'\\' && quote == b'"' {
                self.at += 1;
                // A `\` at the end of a line trims what follows, up to the next text.
                let next = self.rest().iter().position(|b| !matches!(b, b' ' | b'\t'));
                if next.is_some_and(|at| matches!(self.rest()[at], b'\n' | b'\r')) {
                    self.blank_only();
                    continue;
                }
                self.escape(&mut out)?;
                continue;
            }
            out.push(byte);
            self.at += 1;
        }
    }

    /// Whitespace and line breaks, without comments.
    fn blank_only(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.at += 1;
        }
    }

    /// An escape in a basic string, after its `\`.
    fn escape(&mut self, out: &mut Vec<u8>) -> Option<()> {
        let byte = self.peek()?;
        self.at += 1;
        let simple = match byte {
            b'b' => Some(b'\x08'),
            b't' => Some(b'\t'),
            b'n' => Some(b'\n'),
            b'f' => Some(b'\x0c'),
            b'r' => Some(b'\r'),
            b'e' => Some(b'\x1b'),
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            _ => None,
        };
        if let Some(simple) = simple {
            out.push(simple);
            return Some(());
        }
        let digits = match byte {
            b'x' => 2,
            b'u' => 4,
            b'U' => 8,
            _ => return None,
        };
        let hex = std::str::from_utf8(self.text.get(self.at..self.at + digits)?).ok()?;
        let code = u32::from_str_radix(hex, 16).ok()?;
        self.at += digits;
        let mut buffer = [0; 4];
        out.extend_from_slice(char::from_u32(code)?.encode_utf8(&mut buffer).as_bytes());
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<(String, Value)> {
        read(text)
            .unwrap_or_else(|| panic!("{text}"))
            .keys
            .into_iter()
            .map(|(path, value)| (path.join("."), value))
            .collect()
    }

    fn text(value: &str) -> Value {
        Value::Text(value.to_string())
    }

    // covers: C-15
    #[test]
    fn a_manifest_reads_as_cargo_writes_it() {
        let found = keys(
            "\
# A crate.
[package]
name = \"a\" # its name
edition.workspace = true
\"quoted key\" = 'literal \\ text'

[dependencies]
serde = { workspace = true, features = [\"derive\"] }
b.workspace = true
c = { version = \"1\", features = [
    \"x\", # one
    \"y\",
] }

[target.'cfg(unix)'.dependencies]
d = {workspace=true}

[dependencies.e]
workspace = true

[[bin]]
name = \"tool\"
path = \"src/tool.rs\"

[workspace]
members = [\"crates/*\"]
exclude = [ ]
version = 1.2
when = 1979-05-27T07:32:00Z
note = \"\"\"
two \"quoted\" lines\"\"\"
",
        );
        assert!(found.contains(&("package.name".to_string(), text("a"))));
        assert!(found.contains(&("package.edition.workspace".to_string(), Value::Bool(true))));
        assert!(found.contains(&("package.quoted key".to_string(), text("literal \\ text"))));
        assert!(found.contains(&(
            "dependencies.serde.workspace".to_string(),
            Value::Bool(true)
        )));
        assert!(found.contains(&(
            "dependencies.serde.features".to_string(),
            Value::Texts(vec!["derive".to_string()])
        )));
        assert!(found.contains(&(
            "dependencies.c.features".to_string(),
            Value::Texts(vec!["x".to_string(), "y".to_string()])
        )));
        assert!(found.contains(&(
            "target.cfg(unix).dependencies.d.workspace".to_string(),
            Value::Bool(true)
        )));
        assert!(found.contains(&("dependencies.e.workspace".to_string(), Value::Bool(true))));
        assert!(found.contains(&("bin.path".to_string(), text("src/tool.rs"))));
        assert!(found.contains(&("workspace.note".to_string(), text("two \"quoted\" lines"))));
        assert!(found.contains(&("workspace.version".to_string(), Value::Other)));
        let manifest = read("[dependencies]\nb.workspace = true\n[dev-dependencies]\nt.workspace = true\n[target.x.build-dependencies]\nbuilt = { workspace = true }\n").unwrap();
        assert_eq!(manifest.inherited_dependencies(false), ["b", "built"]);
        assert_eq!(manifest.inherited_dependencies(true), ["b", "t", "built"]);
        assert!(read("[workspace]\n").unwrap().heads_a_workspace());
        assert!(
            read("workspace.members = []\n")
                .unwrap()
                .heads_a_workspace()
        );
        assert!(
            !read("[package]\nworkspace = \"..\"\n")
                .unwrap()
                .heads_a_workspace()
        );
    }

    // covers: C-15
    /// What isn't TOML, or a key this reader doesn't take, reads as nothing: the repository keeps
    /// one scope.
    #[test]
    fn what_isnt_toml_reads_as_nothing() {
        for text in [
            "this is not toml [[[",
            "[dependencies\na = 1\n",
            "a = \"no end\n",
            "a = { b = 1\n",
            "a = [1, 2\n",
            "a = 1 b = 2\n",
            "a = \"\\q\"\n",
            "= 1\n",
            "a =\n",
            "\"\"\"key\"\"\" = 1\n",
        ] {
            assert!(read(text).is_none(), "{text}");
        }
    }

    fn write(root: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    const WORKSPACE: &str = "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/apart\"]\n\n[workspace.package]\nedition = \"2024\"\n\n[workspace.dependencies]\ncore = { path = \"crates/core\" }\nkit = { path = \"crates/kit\", features = [\"x\"] }\nserde = \"1\"\n";

    // covers: C-15
    #[test]
    fn a_crate_reads_what_it_inherits_from_its_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let crate_text = "[package]\nedition.workspace = true\n\n[dependencies]\ncore.workspace = true\nserde = { workspace = true }\n\n[dev-dependencies]\nkit.workspace = true\n";
        write(
            dir.path(),
            &[
                ("cli/Cargo.toml", WORKSPACE),
                ("cli/crates/lib/Cargo.toml", crate_text),
            ],
        );
        let root = dir.path();
        // A dependent builds the workspace's manifest and what the crate builds with, from there.
        assert_eq!(
            inherited(root, "cli/crates/lib", crate_text, false).unwrap(),
            ["../../../cli/Cargo.toml", "../../../cli/crates/core"]
        );
        // The project itself builds its dev-dependencies too.
        assert_eq!(
            inherited(root, "cli/crates/lib", crate_text, true).unwrap(),
            [
                "../../../cli/Cargo.toml",
                "../../../cli/crates/core",
                "../../../cli/crates/kit"
            ]
        );
        // What it inherits nothing of reads nothing there, whatever a comment says.
        let plain = "[package]\n# edition.workspace = true\n";
        assert!(
            inherited(root, "cli/crates/lib", plain, false)
                .unwrap()
                .is_empty()
        );
        // A dependency the workspace doesn't give, a workspace that isn't there, and one outside
        // the repository: hushrun can't say what it builds with.
        let missing = "[dependencies]\nnone.workspace = true\n";
        assert!(
            inherited(root, "cli/crates/lib", missing, false)
                .unwrap_err()
                .contains("inherits none, which cli/Cargo.toml doesn't give")
        );
        let alone = "[dependencies]\nb.workspace = true\n";
        assert_eq!(
            inherited(root, "elsewhere", alone, false).unwrap_err(),
            "elsewhere/Cargo.toml inherits from a workspace hushrun can't find"
        );
        let outside = "[package]\nworkspace = \"../..\"\nedition.workspace = true\n";
        assert!(
            inherited(root, "cli", outside, false)
                .unwrap_err()
                .contains("outside the repository")
        );
        // A crate a workspace leaves out isn't its member.
        write(root, &[("cli/crates/apart/Cargo.toml", alone)]);
        assert!(inherited(root, "cli/crates/apart", alone, false).is_err());
        // `package.workspace` names its workspace.
        let named = "[package]\nworkspace = \"../../../cli\"\nedition.workspace = true\n";
        write(root, &[("tools/x/y/Cargo.toml", named)]);
        assert_eq!(
            inherited(root, "tools/x/y", named, false).unwrap(),
            ["../../../cli/Cargo.toml"]
        );
        // A workspace's own manifest inherits from itself.
        let both = format!(
            "{WORKSPACE}\n[package]\nname = \"top\"\nedition.workspace = true\n\n[dependencies]\ncore.workspace = true\n"
        );
        assert_eq!(
            inherited(root, "cli", &both, false).unwrap(),
            ["../cli/crates/core"]
        );
    }

    // covers: C-15
    #[test]
    fn a_member_builds_in_its_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            &[
                ("cli/Cargo.toml", WORKSPACE),
                ("cli/crates/lib/Cargo.toml", "[package]\nname = \"lib\"\n"),
                (
                    "cli/crates/apart/Cargo.toml",
                    "[package]\nname = \"apart\"\n",
                ),
                (
                    "cli/crates/own/Cargo.toml",
                    "[package]\nname = \"own\"\n[workspace]\n",
                ),
                ("back/Cargo.toml", "[package]\nname = \"back\"\n"),
            ],
        );
        assert_eq!(
            workspace_above(root, "cli/crates/lib").unwrap().as_deref(),
            Some("cli")
        );
        // Left out, heading its own, at its workspace's top, or in none.
        assert_eq!(workspace_above(root, "cli/crates/apart").unwrap(), None);
        assert_eq!(workspace_above(root, "cli/crates/own").unwrap(), None);
        assert_eq!(workspace_above(root, "cli").unwrap(), None);
        assert_eq!(workspace_above(root, "back").unwrap(), None);
        assert_eq!(workspace_above(root, "nothing").unwrap(), None);
        // A workspace above that can't be read says so.
        write(root, &[("cli/Cargo.toml", "[workspace\n")]);
        assert!(workspace_above(root, "cli/crates/lib").is_err());
    }

    #[test]
    fn parents_go_up_to_the_root() {
        assert_eq!(parents("a/b/c").collect::<Vec<_>>(), ["a/b", "a", ""]);
        assert_eq!(parents("a").collect::<Vec<_>>(), [""]);
        assert!(parents("").next().is_none());
    }
}
