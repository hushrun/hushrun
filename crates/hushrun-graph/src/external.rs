//! \[C-150\] What a `macro_rules!` macro of a crate outside the workspace can name at the place
//! it is invoked. A macro's body is resolved where it expands for everything but its locals and
//! labels: a free name in it (one that no `$crate::`, `::`, `std::`, `core::` or `alloc::` path
//! holds, and no `$` makes a fragment) may be an item of the invoking module, and a method name
//! may be any trait's. The index has no occurrence for either, since their tokens are in no file
//! of the workspace, so the walk counts every workspace item of each such name: a method's name,
//! written `.name` in what [`Macros::free_names`] gives, every method of that name.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The free names of one crate's macros, read from its sources once.
#[derive(Debug, Default)]
pub struct Macros {
    /// Each `macro_rules!` of the crate by name: the free names of its transcribers, the macros
    /// they invoke, and whether they hold an attribute that makes code run unnamed.
    by_name: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>, bool, bool)>,
    /// Each macro's matchers' literal identifiers: the words of its own syntax (`target:` of
    /// tracing's `event!`).
    literals: BTreeMap<String, BTreeSet<String>>,
    /// Each macro's identifiers handed to another macro of the crate, with that macro: a name
    /// only when that macro's matchers don't hold it literally.
    handed: BTreeMap<String, Vec<(String, String)>>,
}

/// \[C-149, C-150\] What one macro can name where it expands, and whether what it expands to
/// can run with no item naming it (`#[used]`, `#[link_section]`, a constructor), as
/// `inventory::submit!` does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Macro {
    pub names: BTreeSet<String>,
    pub registers: bool,
    /// \[C-148\] Its expansion can hold an `impl` block, for a type its invocation names.
    pub impls: bool,
}

impl Macros {
    /// Reads every `.rs` file below `dir`, a package's folder. A file that doesn't parse is
    /// skipped: a macro it defines isn't found, which counts the most, never the least.
    pub fn of_package(dir: &Path) -> Macros {
        let mut out = Macros::default();
        let mut pending: Vec<PathBuf> = vec![dir.to_path_buf()];
        let mut files = 0usize;
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    let name = entry.file_name();
                    if name != "target" && !name.to_string_lossy().starts_with('.') {
                        pending.push(path);
                    }
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files += 1;
                    if files > 2000 {
                        return out;
                    }
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        out.read(&text);
                    }
                }
            }
        }
        out
    }

    /// Adds the macros of one source.
    pub fn read(&mut self, source: &str) {
        let mut parser = tree_sitter::Parser::new();
        if parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .is_err()
        {
            return;
        }
        let Some(tree) = parser.parse(source, None) else {
            return;
        };
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "macro_definition" {
                let Some(name) = node.child_by_field_name("name") else {
                    continue;
                };
                let name = source[name.byte_range()].to_string();
                let entry = self.by_name.entry(name.clone()).or_default();
                let literals = self.literals.entry(name.clone()).or_default();
                let handed = self.handed.entry(name).or_default();
                let mut cursor = node.walk();
                for rule in node.named_children(&mut cursor) {
                    if rule.kind() != "macro_rule" {
                        continue;
                    }
                    if let Some(left) = rule.child_by_field_name("left") {
                        matcher_words(left, source, literals);
                    }
                    if let Some(right) = rule.child_by_field_name("right") {
                        free(
                            right,
                            source,
                            None,
                            &BTreeSet::new(),
                            &mut entry.0,
                            &mut entry.1,
                            handed,
                        );
                        let text = &source[right.byte_range()];
                        entry.2 |= crate::graph::ROOT_ATTRS
                            .iter()
                            .any(|attr| contains_word(text, attr));
                        entry.3 |= contains_word(text, "impl");
                    }
                }
                continue;
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }

    /// The free names of macro `name` and of the macros of this crate it invokes, closed; `None`
    /// when the crate defines no `macro_rules!` of that name (a procedural macro, or one this
    /// reader didn't find).
    pub fn free_names(&self, name: &str) -> Option<Macro> {
        self.by_name.get(name)?;
        let mut out = Macro::default();
        let mut seen = BTreeSet::new();
        let mut pending = vec![name.to_string()];
        while let Some(next) = pending.pop() {
            if !seen.insert(next.clone()) {
                continue;
            }
            if let Some((names, invoked, registers, impls)) = self.by_name.get(&next) {
                out.names.extend(names.iter().cloned());
                for (word, to) in self.handed.get(&next).into_iter().flatten() {
                    let bare = word.trim_start_matches('.');
                    if !self.literals.get(to).is_some_and(|l| l.contains(bare)) {
                        out.names.insert(word.clone());
                    }
                }
                out.registers |= *registers;
                out.impls |= *impls;
                pending.extend(invoked.iter().cloned());
            }
        }
        Some(out)
    }
}

/// Whether `word` is in `text`, as a word.
pub fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        let part = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        !part(before) && !part(after)
    })
}

/// The identifiers of a matcher, but its fragments' names and kinds: the words a macro's own
/// syntax matches literally.
fn matcher_words(tree: tree_sitter::Node<'_>, source: &str, out: &mut BTreeSet<String>) {
    let mut cursor = tree.walk();
    let children: Vec<tree_sitter::Node<'_>> = tree.children(&mut cursor).collect();
    for child in children {
        match child.kind() {
            "identifier" => {
                out.insert(source[child.byte_range()].to_string());
            }
            "metavariable" | "fragment_specifier" | "token_binding_pattern" => {}
            _ => matcher_words(child, source, out),
        }
    }
}

/// The names a token tree binds itself, not those of the trees in it: each `use` declaration's
/// last segment or its `as` name (each of a group's, `use a::{b, C as D};`), and each name after
/// `fn`, `struct`, `enum`, `union`, `trait`, `type`, `const`, `static`, `mod` or `let` (and
/// their `mut` or `ref`). In that tree and those in it, they are the expansion's own, wherever it
/// expands.
fn bound(tree: tree_sitter::Node<'_>, source: &str, out: &mut BTreeSet<String>) {
    let mut cursor = tree.walk();
    let tokens: Vec<tree_sitter::Node<'_>> = tree.children(&mut cursor).collect();
    let text = |n: &tree_sitter::Node<'_>| &source[n.byte_range()];
    let mut in_use = false;
    let mut last: Option<String> = None;
    for (i, token) in tokens.iter().enumerate() {
        let word = text(token);
        if matches!(token.kind(), "token_tree" | "token_repetition") {
            if in_use && i > 0 && text(&tokens[i - 1]) == "::" {
                use_group(*token, source, out);
                last = None;
            }
            continue;
        }
        if word == "use" {
            in_use = true;
            last = None;
            continue;
        }
        if in_use {
            match word {
                ";" => {
                    out.extend(last.take());
                    in_use = false;
                }
                _ if token.kind() == "identifier" => last = Some(word.to_string()),
                _ => {}
            }
            continue;
        }
        // The keyword before it, past `mut` and `ref`.
        let mut k = i;
        while k > 0 && matches!(text(&tokens[k - 1]), "mut" | "ref") {
            k -= 1;
        }
        if token.kind() == "identifier"
            && k > 0
            && matches!(
                text(&tokens[k - 1]),
                "fn" | "struct"
                    | "enum"
                    | "union"
                    | "trait"
                    | "type"
                    | "const"
                    | "static"
                    | "mod"
                    | "let"
            )
        {
            out.insert(word.to_string());
        }
    }
}

/// The names a `use` group binds (`{b, c::D, E as F, g::{h}}`): each element's last segment or
/// its `as` name, and those of the groups in it.
fn use_group(group: tree_sitter::Node<'_>, source: &str, out: &mut BTreeSet<String>) {
    let mut cursor = group.walk();
    let tokens: Vec<tree_sitter::Node<'_>> = group.children(&mut cursor).collect();
    let mut last: Option<String> = None;
    for (i, token) in tokens.iter().enumerate() {
        let word = &source[token.byte_range()];
        if matches!(token.kind(), "token_tree" | "token_repetition") {
            if i > 0 && &source[tokens[i - 1].byte_range()] == "::" {
                use_group(*token, source, out);
                last = None;
            }
            continue;
        }
        match word {
            "," | "}" => out.extend(last.take()),
            _ if token.kind() == "identifier" && word != "self" => last = Some(word.to_string()),
            _ => {}
        }
    }
    out.extend(last);
}

/// The free names under a transcriber, and the macros it invokes. Inside the input of a macro it
/// invokes (`inside`), a name goes to `handed`, with that macro.
#[allow(clippy::too_many_arguments)]
fn free(
    tree: tree_sitter::Node<'_>,
    source: &str,
    inside: Option<&str>,
    scope: &BTreeSet<String>,
    names: &mut BTreeSet<String>,
    invoked: &mut BTreeSet<String>,
    handed: &mut Vec<(String, String)>,
) {
    let mut scope = scope.clone();
    bound(tree, source, &mut scope);
    let mut cursor = tree.walk();
    let tokens: Vec<tree_sitter::Node<'_>> = tree.children(&mut cursor).collect();
    let text = |n: &tree_sitter::Node<'_>| &source[n.byte_range()];
    for (i, token) in tokens.iter().enumerate() {
        match token.kind() {
            "token_tree" | "token_repetition" => {
                // `a::{b, c}`: a group of a path, whose names the path resolves.
                if i > 0 && text(&tokens[i - 1]) == "::" {
                    continue;
                }
                // `name!(…)`: the tree is that macro's input.
                let macro_name = (i >= 2 && text(&tokens[i - 1]) == "!")
                    .then(|| text(&tokens[i - 2]))
                    .filter(|_| tokens[i - 2].kind() == "identifier");
                free(
                    *token,
                    source,
                    macro_name.or(inside),
                    &scope,
                    names,
                    invoked,
                    handed,
                );
            }
            "identifier" => {
                let word = text(token);
                let before = i.checked_sub(1).map(|b| text(&tokens[b]));
                let after = tokens.get(i + 1).map(&text);
                // `name!`: a macro, which only a macro of that name is; written `name!`.
                if after == Some("!") {
                    invoked.insert(word.to_string());
                    if before != Some("::") && before != Some("$") {
                        let name = format!("{word}!");
                        match inside {
                            Some(to) => handed.push((name, to.to_string())),
                            None => {
                                names.insert(name);
                            }
                        }
                    }
                    continue;
                }
                // Inside a path: what the path's head names is resolved, not this.
                if before == Some("::") || before == Some("$") {
                    continue;
                }
                if after == Some("::") && matches!(word, "std" | "core" | "alloc" | "crate") {
                    continue;
                }
                // `name: …`: a field, a binding or a macro's own label, never an item; and what
                // the expansion binds itself here.
                if after == Some(":") || scope.contains(word) {
                    continue;
                }
                // A method's name, `.name`, apart from a path's head.
                let name = if before == Some(".") {
                    format!(".{word}")
                } else {
                    word.to_string()
                };
                match inside {
                    Some(to) => handed.push((name, to.to_string())),
                    None => {
                        names.insert(name);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-150
    #[test]
    fn a_macros_free_names_are_its_transcribers_heads_and_methods() {
        let mut m = Macros::default();
        m.read(
            r#"
            #[macro_export]
            macro_rules! json {
                ($($t:tt)+) => { $crate::json_internal!($($t)+) };
            }
            macro_rules! json_internal {
                (null) => { $crate::Value::Null };
                ($e:expr) => { $crate::to_value(&$e).unwrap() };
                (call) => { helper(::std::vec::Vec::new()) };
            }
        "#,
        );
        let names = m.free_names("json").unwrap().names;
        // `null` is a matcher's token, `Value` and `to_value` are `$crate::`'s, `Vec` and `new`
        // `::std`'s; `unwrap` is a method, `helper` is resolved where it expands.
        assert!(names.contains(".unwrap"), "{names:?}");
        assert!(names.contains("helper"), "{names:?}");
        assert!(!names.contains("Value") && !names.contains("Vec") && !names.contains("null"));
        assert_eq!(m.free_names("missing"), None);
        assert!(!m.free_names("json").unwrap().registers);
        m.read("macro_rules! submit { ($e:expr) => { #[used] #[link_section = \".init_array\"] static S: fn() = $e; } }");
        // A word of another macro's own syntax is no name: `target:` of `event!`.
        m.read("macro_rules! event { (target: $t:expr, $($a:tt)*) => { helper2($t) }; } macro_rules! debug { ($($a:tt)*) => { $crate::event!(target: module_path!(), named_arg) }; }");
        let debug = m.free_names("debug").unwrap().names;
        assert!(!debug.contains("target"), "{debug:?}");
        // A macro's name is a macro's alone: `module_path!`, never a function `module_path`.
        assert!(
            debug.contains("module_path!") && !debug.contains("module_path"),
            "{debug:?}"
        );
        assert!(
            debug.contains("named_arg") && debug.contains("helper2"),
            "{debug:?}"
        );
        m.read("macro_rules! meta { () => { Metadata { target: here(), level: 1 } } }");
        let meta = m.free_names("meta").unwrap().names;
        assert!(
            meta.contains("here") && !meta.contains("target") && !meta.contains("level"),
            "{meta:?}"
        );
        assert!(m.free_names("submit").unwrap().registers);
        // What the expansion binds itself: `let mut`, and each name of a `use` group.
        m.read("macro_rules! object { () => {{ let mut object = new_map(); fill!(object); use $crate::field::{debug, Value as V}; object } } }");
        let object = m.free_names("object").unwrap().names;
        assert!(object.contains("new_map"), "{object:?}");
        assert!(
            !object.contains("object") && !object.contains("debug") && !object.contains("V"),
            "{object:?}"
        );
        assert!(
            !object.contains("Value") && !object.contains("field"),
            "{object:?}"
        );
    }
}
