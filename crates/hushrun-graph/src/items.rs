//! \[C-147\] A `.rs` file split into items with tree-sitter's Rust grammar: functions, `impl`
//! blocks and their functions, traits and theirs, structs, enums, unions, type aliases, consts,
//! statics, modules, `macro_rules!` definitions, macro invocations where an item can be, `use`
//! declarations and `extern` blocks. A function's body is its own: what is nested in it (a closure,
//! an inner function, an `impl` inside it) is the function's. An item holds its outer attributes.
//!
//! Beside the items, a file keeps every identifier, every macro invocation and every string
//! literal, by byte offset: what the reach walk asks of a range that the index left unresolved.

use std::ops::Range;

/// What an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Fn,
    Impl,
    Trait,
    Struct,
    Enum,
    Union,
    TypeAlias,
    Const,
    Static,
    Mod,
    MacroRules,
    /// A macro invocation where an item can be: what it expands to can't be seen here.
    MacroCall,
    Use,
    ExternCrate,
    ForeignMod,
}

impl Kind {
    /// Whether it names a type: reaching it reaches the `impl` blocks for it (C-148).
    pub fn is_type(self) -> bool {
        matches!(
            self,
            Kind::Struct | Kind::Enum | Kind::Union | Kind::TypeAlias | Kind::Trait
        )
    }
}

/// One item of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: Kind,
    /// Its name; for an `impl`, its self type's text; for a macro invocation, the macro's path;
    /// empty for a `use` or an `extern` block.
    pub name: String,
    /// Its bytes, its outer attributes included.
    pub range: Range<usize>,
    pub parent: Option<usize>,
    /// The items it holds that are reached on their own: an `impl`'s or a trait's functions, a
    /// module's items. Never what is nested in a function.
    pub children: Vec<usize>,
    /// The innermost module it is in, as an item; `None` for the file's own.
    pub module: Option<usize>,
    /// The paths of its outer attributes, as written: `test`, `derive`, `serde`, `no_mangle`.
    pub attrs: Vec<String>,
    /// For an `impl`, the bytes of its trait, when it has one.
    pub impl_trait: Option<Range<usize>>,
    /// For an `impl`, the bytes of its self type.
    pub impl_self: Option<Range<usize>>,
    /// \[C-151\] Its attributes hold `#[doc(alias = "hushrun::starts_no_workspace_binary")]`.
    pub marked: bool,
}

/// \[C-151\] The marker a function's attributes hold when the programs it starts are never a
/// binary of the workspace, which its own code makes sure of.
pub const MARKER: &str = "hushrun::starts_no_workspace_binary";

/// An identifier of a file: a name, a type's name, a field's, a macro's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub range: Range<usize>,
    pub text: String,
    /// In a macro's tokens, it follows `fn`, `struct`, `enum`, `union`, `trait`, `type`,
    /// `const`, `static`, `mod` or `let`: it names what the expansion defines, and refers to
    /// nothing.
    pub defines: bool,
    /// Where it stands in a path, which says what it can name.
    pub class: Class,
    /// For a [`Class::Tail`], the segment before it: `String` of `String::from`.
    pub head: Option<String>,
}

/// \[C-150\] What an identifier can name, by where it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// After `.`: a method, or a field.
    Method,
    /// Before `::`: a module, a type, a trait, an enum or a crate.
    Head,
    /// After `::`: anything a path ends in, an associated function or const too.
    Tail,
    /// Alone: a function, a const, a static, a type, a macro; never a method.
    Bare,
    /// A format string's `{name}`: a const or a static (or a local, or a field).
    Value,
}

/// A macro invocation anywhere in a file: its path's last name, and the bytes of the whole
/// invocation and of its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroUse {
    pub name: String,
    pub path: Range<usize>,
    pub range: Range<usize>,
}

/// A string literal of a file, its content as written, and whether it is inside an attribute or
/// a macro invocation, where a word of it may name an item (`#[serde(with = "m")]`, `"{CONST}"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Str {
    pub range: Range<usize>,
    pub text: String,
    pub in_attr: bool,
    pub in_macro: bool,
}

/// A file, split.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct File {
    pub items: Vec<Item>,
    /// The file's own items, in order.
    pub top: Vec<usize>,
    pub idents: Vec<Ident>,
    pub macros: Vec<MacroUse>,
    pub strings: Vec<Str>,
    /// The bytes of built-in attributes that name no item (`#[cfg(…)]`, `#[allow(…)]`,
    /// `#[doc = …]`): their identifiers are no references.
    pub inert: Vec<Range<usize>>,
    /// The bytes of every outer or inner attribute's path: `serde` of `#[serde(…)]`.
    pub attr_paths: Vec<Range<usize>>,
    /// Where each line starts, by byte.
    pub lines: Vec<usize>,
    pub len: usize,
}

/// Why a file couldn't be split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsplit(pub String);

/// Splits `source`. A file tree-sitter can't parse whole (a syntax error, a node it couldn't
/// read) is an error: what its items are can't be told.
pub fn split(source: &str) -> Result<File, Unsplit> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|err| Unsplit(format!("tree-sitter's Rust grammar won't load: {err}")))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Unsplit("tree-sitter returned no tree".into()))?;
    let root = tree.root_node();
    if root.has_error() {
        return Err(Unsplit("it doesn't parse as Rust".into()));
    }
    let mut file = File {
        len: source.len(),
        lines: std::iter::once(0)
            .chain(source.match_indices('\n').map(|(at, _)| at + 1))
            .collect(),
        ..File::default()
    };
    let top = items_in(root, source, None, None, &mut file);
    file.top = top;
    tokens(root, source, false, false, &mut file);
    let mut locals: Vec<Range<usize>> = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "macro_rule"
            && let Some(right) = node.child_by_field_name("right")
        {
            macro_locals(right, source, &mut locals);
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    let locals: std::collections::HashSet<usize> = locals.iter().map(|r| r.start).collect();
    for ident in &mut file.idents {
        if locals.contains(&ident.range.start) {
            ident.defines = true;
        }
    }
    Ok(file)
}

/// \[C-150\] In a `macro_rules!` transcriber, the identifiers that are a function's locals where
/// they stand: bound earlier in the same function by a parameter (`name:`), `let`, a closure's
/// `|name|` or `for name in`. A macro's locals are its own (hygiene), and name no item.
fn macro_locals(tree: tree_sitter::Node<'_>, source: &str, out: &mut Vec<Range<usize>>) {
    let mut cursor = tree.walk();
    let tokens: Vec<tree_sitter::Node<'_>> = tree.children(&mut cursor).collect();
    let text = |n: &tree_sitter::Node<'_>| &source[n.byte_range()];
    let mut i = 0;
    while i < tokens.len() {
        if text(&tokens[i]) == "fn" {
            // `fn name (params) … {body}`.
            let params = tokens[i..]
                .iter()
                .position(|t| t.kind() == "token_tree" && text(t).starts_with('('));
            let body = tokens[i..]
                .iter()
                .position(|t| t.kind() == "token_tree" && text(t).starts_with('{'));
            if let (Some(p), Some(b)) = (params, body)
                && p < b
            {
                let mut bound: Vec<(String, usize)> = Vec::new();
                let params = tokens[i + p];
                let mut c = params.walk();
                let ps: Vec<tree_sitter::Node<'_>> = params.children(&mut c).collect();
                for (k, t) in ps.iter().enumerate() {
                    if t.kind() == "identifier" && ps.get(k + 1).is_some_and(|n| text(n) == ":") {
                        bound.push((text(t).to_string(), 0));
                        out.push(t.byte_range());
                    }
                }
                let body = tokens[i + b];
                bind_and_mark(body, source, &mut bound, out);
                i += b + 1;
                continue;
            }
        }
        if tokens[i].kind() == "token_tree" {
            macro_locals(tokens[i], source, out);
        }
        i += 1;
    }
}

/// Binds what `let`, closures and `for` bind under `tree`, in order, and marks each identifier
/// bound before it.
fn bind_and_mark(
    tree: tree_sitter::Node<'_>,
    source: &str,
    bound: &mut Vec<(String, usize)>,
    out: &mut Vec<Range<usize>>,
) {
    let mut cursor = tree.walk();
    let tokens: Vec<tree_sitter::Node<'_>> = tree.children(&mut cursor).collect();
    let text = |n: &tree_sitter::Node<'_>| &source[n.byte_range()];
    let mut in_closure = false;
    for (k, t) in tokens.iter().enumerate() {
        let word = text(t);
        if t.kind() == "token_tree" {
            bind_and_mark(*t, source, bound, out);
            continue;
        }
        if word == "|" {
            in_closure = !in_closure
                && tokens.get(k.wrapping_sub(1)).is_none_or(|b| {
                    matches!(text(b), "(" | "," | "=" | "move" | "{" | ";")
                        || b.kind() == "token_tree"
                });
            continue;
        }
        if t.kind() != "identifier" {
            continue;
        }
        let before = k.checked_sub(1).map(|b| text(&tokens[b]));
        let binds = in_closure
            || before == Some("let")
            || (before == Some("mut") && k >= 2 && text(&tokens[k - 2]) == "let")
            || (before == Some("for") && tokens.get(k + 1).is_some_and(|n| text(n) == "in"));
        if binds {
            bound.push((word.to_string(), t.start_byte()));
            out.push(t.byte_range());
            continue;
        }
        if bound
            .iter()
            .any(|(name, at)| name == word && *at <= t.start_byte())
        {
            out.push(t.byte_range());
        }
    }
}

/// An item's kind, its name and, for an `impl`, its trait's and self type's bytes.
type Of = (Kind, String, Option<Range<usize>>, Option<Range<usize>>);

/// The item a node is, when it is one, with its name and, for an `impl`, its trait's and self
/// type's bytes.
fn item_of(node: tree_sitter::Node<'_>, source: &str) -> Option<Of> {
    let name = |field: &str| {
        node.child_by_field_name(field)
            .map(|n| source[n.byte_range()].to_string())
            .unwrap_or_default()
    };
    let kind = match node.kind() {
        "function_item" | "function_signature_item" => Kind::Fn,
        "impl_item" => {
            let tr = node.child_by_field_name("trait").map(|n| n.byte_range());
            let ty = node.child_by_field_name("type").map(|n| n.byte_range());
            let text = ty
                .clone()
                .map(|r| source[r].to_string())
                .unwrap_or_default();
            return Some((Kind::Impl, text, tr, ty));
        }
        "trait_item" => Kind::Trait,
        "struct_item" => Kind::Struct,
        "enum_item" => Kind::Enum,
        "union_item" => Kind::Union,
        "type_item" => Kind::TypeAlias,
        "const_item" => Kind::Const,
        "static_item" => Kind::Static,
        "mod_item" => Kind::Mod,
        "macro_definition" => Kind::MacroRules,
        "macro_invocation" => {
            return Some((Kind::MacroCall, name("macro"), None, None));
        }
        "use_declaration" => return Some((Kind::Use, String::new(), None, None)),
        "extern_crate_declaration" => Kind::ExternCrate,
        "foreign_mod_item" => return Some((Kind::ForeignMod, String::new(), None, None)),
        _ => return None,
    };
    Some((kind, name("name"), None, None))
}

/// The items directly in `parent` (a file, a module's or an `impl`'s or a trait's body), each
/// with its outer attributes, recursing into modules, `impl`s and traits.
fn items_in(
    parent: tree_sitter::Node<'_>,
    source: &str,
    owner: Option<usize>,
    module: Option<usize>,
    file: &mut File,
) -> Vec<usize> {
    let mut out = Vec::new();
    let mut attrs: Vec<tree_sitter::Node<'_>> = Vec::new();
    let mut cursor = parent.walk();
    let children: Vec<tree_sitter::Node<'_>> = parent.named_children(&mut cursor).collect();
    for child in children {
        if child.kind() == "attribute_item" {
            attrs.push(child);
            continue;
        }
        if matches!(child.kind(), "line_comment" | "block_comment") {
            continue;
        }
        // A macro invocation where an item can be is an expression statement of one.
        let node = if child.kind() == "expression_statement"
            && child.named_child_count() == 1
            && child
                .named_child(0)
                .is_some_and(|n| n.kind() == "macro_invocation")
        {
            child.named_child(0).unwrap_or(child)
        } else {
            child
        };
        let Some((kind, name, impl_trait, impl_self)) = item_of(node, source) else {
            attrs.clear();
            continue;
        };
        let start = attrs.first().map_or(child.start_byte(), |a| a.start_byte());
        let marked = attrs.iter().any(|a| {
            let text = &source[a.byte_range()];
            text.contains("alias") && text.contains(&format!("\"{MARKER}\""))
        });
        let attr_paths = attrs
            .iter()
            .filter_map(|a| a.named_child(0))
            .filter_map(|attr| attr.named_child(0))
            .map(|path| source[path.byte_range()].to_string())
            .collect();
        attrs.clear();
        let at = file.items.len();
        file.items.push(Item {
            kind,
            name,
            range: start..child.end_byte(),
            parent: owner,
            children: Vec::new(),
            module,
            attrs: attr_paths,
            impl_trait,
            impl_self,
            marked,
        });
        out.push(at);
        let body = match kind {
            Kind::Mod | Kind::Impl | Kind::Trait => node.child_by_field_name("body"),
            _ => None,
        };
        if let Some(body) = body {
            let inner_module = if kind == Kind::Mod { Some(at) } else { module };
            let children = items_in(body, source, Some(at), inner_module, file);
            // An `impl`'s or a trait's associated types and consts are its own: only its
            // functions are reached on their own.
            let children: Vec<usize> = if kind == Kind::Mod {
                children
            } else {
                children
                    .into_iter()
                    .filter(|c| file.items[*c].kind == Kind::Fn)
                    .collect()
            };
            file.items[at].children = children;
        }
    }
    out
}

/// Rust's keywords, which a macro's tokens hold as identifiers: they name nothing.
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

/// \[C-150\] Built-in attributes whose tokens name no item.
const INERT: &[&str] = &[
    "cfg",
    "allow",
    "warn",
    "deny",
    "forbid",
    "expect",
    "doc",
    "inline",
    "cold",
    "must_use",
    "repr",
    "non_exhaustive",
    "track_caller",
    "deprecated",
    "ignore",
    "should_panic",
    "path",
    "macro_use",
    "macro_export",
    "rustfmt::skip",
    "test",
    "automatically_derived",
    "no_std",
    "recursion_limit",
    "crate_type",
    "crate_name",
    "feature",
    "unsafe_code",
    "diagnostic::on_unimplemented",
];

/// Every identifier, macro invocation and string literal under `node`.
fn tokens(
    node: tree_sitter::Node<'_>,
    source: &str,
    in_attr: bool,
    in_macro: bool,
    file: &mut File,
) {
    tokens_after(node, [None, None], source, in_attr, in_macro, file);
}

/// [`tokens`], with the token before `node` among its siblings.
fn tokens_after(
    node: tree_sitter::Node<'_>,
    [before, before2]: [Option<&str>; 2],
    source: &str,
    in_attr: bool,
    in_macro: bool,
    file: &mut File,
) {
    let text = || source[node.byte_range()].to_string();
    match node.kind() {
        "identifier" | "type_identifier" | "field_identifier" | "shorthand_field_identifier"
            if KEYWORDS.contains(&&source[node.byte_range()]) => {}
        "identifier" | "type_identifier" | "field_identifier" | "shorthand_field_identifier" => {
            let parent = node.parent();
            let in_tokens = parent.is_some_and(|p| p.kind() == "token_tree");
            let after = node.next_sibling().map(|n| &source[n.byte_range()]);
            let class = if in_tokens {
                match (before, after) {
                    (Some("."), _) => Class::Method,
                    (Some("::"), _) => Class::Tail,
                    (_, Some("::")) => Class::Head,
                    _ => Class::Bare,
                }
            } else if node.kind() == "field_identifier" {
                Class::Method
            } else {
                match parent.map(|p| (p.kind(), p.child_by_field_name("path").map(|n| n.id()))) {
                    Some(("scoped_identifier" | "scoped_type_identifier", Some(path)))
                        if path == node.id() =>
                    {
                        Class::Head
                    }
                    Some(("scoped_identifier" | "scoped_type_identifier", _)) => Class::Tail,
                    _ => Class::Bare,
                }
            };
            let head = match class {
                Class::Tail if in_tokens => before2
                    .filter(|h| h.chars().all(|c| c.is_alphanumeric() || c == '_'))
                    .map(str::to_string),
                Class::Tail => parent
                    .and_then(|p| p.child_by_field_name("path"))
                    .map(|path| {
                        let text = &source[path.byte_range()];
                        text.rsplit("::").next().unwrap_or(text).trim().to_string()
                    }),
                _ => None,
            };
            file.idents.push(Ident {
                class,
                head,
                range: node.byte_range(),
                text: text(),
                defines: in_tokens
                    && before.is_some_and(|b| {
                        matches!(
                            b,
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
                    }),
            });
        }
        // A `macro_rules!` matcher's tokens are matched, and name nothing.
        "token_tree_pattern" => return,
        "string_literal" | "raw_string_literal" => {
            file.strings.push(Str {
                range: node.byte_range(),
                text: text(),
                in_attr,
                in_macro,
            });
            return;
        }
        "attribute_item" | "inner_attribute_item" => {
            let path = node
                .named_child(0)
                .and_then(|attr| attr.named_child(0))
                .map(|p| &source[p.byte_range()]);
            if path.is_some_and(|p| INERT.contains(&p)) {
                file.inert.push(node.byte_range());
            }
            if let Some(p) = node.named_child(0).and_then(|attr| attr.named_child(0)) {
                file.attr_paths.push(p.byte_range());
            }
        }
        "macro_invocation" => {
            if let Some(path) = node.child_by_field_name("macro") {
                let whole = &source[path.byte_range()];
                let name = whole.rsplit("::").next().unwrap_or(whole).to_string();
                file.macros.push(MacroUse {
                    name,
                    path: path.byte_range(),
                    range: node.byte_range(),
                });
            }
        }
        _ => {}
    }
    let in_attr = in_attr || matches!(node.kind(), "attribute_item" | "inner_attribute_item");
    let in_macro = in_macro || node.kind() == "macro_invocation";
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node<'_>> = node.children(&mut cursor).collect();
    let mut before: [Option<&str>; 2] = [None, None];
    for child in children {
        tokens_after(child, before, source, in_attr, in_macro, file);
        before = [Some(&source[child.byte_range()]), before[0]];
    }
}

impl File {
    /// The byte a SCIP position (zero-based line, UTF-8 byte in it) is at, when the file has it.
    pub fn offset(&self, line: u32, column: u32) -> Option<usize> {
        let start = *self.lines.get(line as usize)?;
        let at = start.checked_add(column as usize)?;
        (at <= self.len).then_some(at)
    }

    /// The innermost item that holds `at`, reached on its own: a function inside an `impl` rather
    /// than the `impl`; never an item nested in a function, which is the function's.
    pub fn innermost(&self, at: usize) -> Option<usize> {
        let mut level = &self.top;
        let mut found = None;
        loop {
            let next = level
                .iter()
                .copied()
                .find(|i| self.items[*i].range.contains(&at));
            match next {
                Some(i) => {
                    found = Some(i);
                    level = &self.items[i].children;
                }
                None => return found,
            }
        }
    }

    /// An item's own bytes: its range less its children's, which are reached on their own.
    pub fn own(&self, item: usize) -> Vec<Range<usize>> {
        let it = &self.items[item];
        let mut out = Vec::new();
        let mut from = it.range.start;
        let mut children: Vec<Range<usize>> = it
            .children
            .iter()
            .map(|c| self.items[*c].range.clone())
            .collect();
        children.sort_by_key(|r| r.start);
        for child in children {
            if child.start > from {
                out.push(from..child.start);
            }
            from = from.max(child.end);
        }
        if it.range.end > from {
            out.push(from..it.range.end);
        }
        out
    }

    /// Whether `at` is in one of `ranges`.
    pub fn within(ranges: &[Range<usize>], at: &Range<usize>) -> bool {
        ranges
            .iter()
            .any(|r| r.start <= at.start && at.end <= r.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct S { #[serde(with = "m")] a: u32 }
impl<T: X> Tr<T> for S where T: Y { type O = u8; fn m(&self) { foo::bar(); println!("{CONST}"); fn inner() {} } }
trait Tr2 { fn d(&self) -> u8 { 1 } fn r(&self); }
macro_rules! mk { ($n:ident) => { fn $n() { helper() } } }
mk!(gen);
mod inner { use super::*; #[test] fn t() { gen(); } }
"#;

    // covers: C-147
    #[test]
    fn a_file_is_split_into_items_with_their_attributes() {
        let file = split(SOURCE).unwrap();
        let kinds: Vec<(Kind, &str)> = file
            .top
            .iter()
            .map(|i| (file.items[*i].kind, file.items[*i].name.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (Kind::Struct, "S"),
                (Kind::Impl, "S"),
                (Kind::Trait, "Tr2"),
                (Kind::MacroRules, "mk"),
                (Kind::MacroCall, "mk"),
                (Kind::Mod, "inner"),
            ]
        );
        let s = &file.items[file.top[0]];
        assert_eq!(s.attrs, ["derive", "serde"]);
        assert_eq!(s.range.start, 0);
        let imp = &file.items[file.top[1]];
        assert_eq!(&SOURCE[imp.impl_trait.clone().unwrap()], "Tr<T>");
        // Its function is reached on its own; its associated type and the function nested in
        // `m` aren't.
        assert_eq!(imp.children.len(), 1);
        let m = &file.items[imp.children[0]];
        assert_eq!((m.kind, m.name.as_str()), (Kind::Fn, "m"));
        assert_eq!(
            file.innermost(SOURCE.find("foo::bar").unwrap()),
            Some(imp.children[0])
        );
        assert_eq!(
            file.innermost(SOURCE.find("fn inner").unwrap()),
            Some(imp.children[0])
        );
        assert_eq!(
            file.innermost(SOURCE.find("type O").unwrap()),
            Some(file.top[1])
        );
        let own = file.own(file.top[1]);
        assert!(!File::within(
            &own,
            &(SOURCE.find("foo").unwrap()..SOURCE.find("foo").unwrap() + 3)
        ));
        let tr = &file.items[file.top[2]];
        assert_eq!(tr.children.len(), 2);
        let t = file.innermost(SOURCE.find("gen();").unwrap()).unwrap();
        assert_eq!(file.items[t].attrs, ["test"]);
        assert_eq!(file.items[t].module, Some(file.top[5]));
        assert!(file.strings.iter().any(|s| s.text == "\"m\"" && s.in_attr));
        assert!(
            file.strings
                .iter()
                .any(|s| s.text == "\"{CONST}\"" && s.in_macro)
        );
        assert!(file.macros.iter().any(|m| m.name == "println"));
        assert!(file.idents.iter().any(|i| i.text == "helper"));
        assert_eq!(
            file.offset(1, 2),
            Some(SOURCE.find("serde(rename").unwrap())
        );
    }

    // covers: C-150
    #[test]
    fn a_macros_locals_name_nothing_after_they_are_bound() {
        let source = "macro_rules! m { ($t:ident) => { fn f(arg: u8) { item(); let item = arg; item; |c| c; for x in v { x; } } } }";
        let file = split(source).unwrap();
        let refers: Vec<&str> = file
            .idents
            .iter()
            .filter(|i| !i.defines && i.text != "m")
            .map(|i| i.text.as_str())
            .collect();
        // The first `item` is a call, before `let` binds it; `v` is free.
        assert_eq!(
            refers.iter().filter(|w| **w == "item").count(),
            1,
            "{refers:?}"
        );
        assert!(
            refers.contains(&"v") && !refers.contains(&"arg") && !refers.contains(&"x"),
            "{refers:?}"
        );
    }

    // covers: C-147
    #[test]
    fn a_file_that_doesnt_parse_isnt_split() {
        assert!(split("fn a( {").is_err());
    }
}
