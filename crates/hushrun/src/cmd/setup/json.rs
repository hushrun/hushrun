//! A JSON document that keeps where each value is, for edits that change only what they must
//! (C-15): the rest of the file keeps its bytes, so its indentation, key order, line endings and
//! final newline, or the lack of one, stay as they were.
//!
//! It reads RFC 8259 JSON, as package.json is, or, for a file whose tool reads them, JSON with
//! comments and trailing commas, as `turbo.json` may be. It makes three kinds of edit: replacing a
//! string, inserting members into an object and appending items to an array, laid out like the
//! members or items around them.

use std::ops::Range;

use anyhow::{Result, anyhow};
use serde_json::Value;

/// How deep values may nest: package.json needs a handful of levels.
const MAX_DEPTH: usize = 128;

/// A parsed document.
#[derive(Debug)]
pub struct Doc {
    text: String,
    root: Node,
    /// `\r\n` when the file uses it, else `\n`.
    newline: &'static str,
    /// One level of indentation: the first indented line's whitespace, else two spaces.
    unit: String,
    /// What separates a key from its value, `: ` unless the file says otherwise.
    colon: String,
}

/// A value, and the bytes it spans.
#[derive(Debug, Clone)]
pub struct Node {
    pub span: Range<usize>,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub enum Kind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    String(String),
    /// A number, `true`, `false` or `null`.
    Scalar,
}

/// An object's member: its key, quotes included, and its value.
#[derive(Debug, Clone)]
pub struct Member {
    pub key: String,
    pub key_span: Range<usize>,
    pub value: Node,
}

impl Node {
    /// An object's members.
    pub fn members(&self) -> Option<&[Member]> {
        match &self.kind {
            Kind::Object(members) => Some(members),
            _ => None,
        }
    }

    /// The index of `key` in an object: its last occurrence, which is the one JSON parsers
    /// read.
    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.members()?.iter().rposition(|m| m.key == key)
    }

    /// The value of `key` in an object.
    pub fn get(&self, key: &str) -> Option<&Node> {
        let index = self.index_of(key)?;
        self.members().map(|members| &members[index].value)
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            Kind::String(text) => Some(text),
            _ => None,
        }
    }

    /// An array's items.
    pub fn items(&self) -> Option<&[Node]> {
        match &self.kind {
            Kind::Array(items) => Some(items),
            _ => None,
        }
    }
}

impl Doc {
    pub fn parse(text: &str) -> Result<Doc> {
        Doc::parse_with(text, false)
    }

    /// JSON with `//` and `/* */` comments and trailing commas, which an edit keeps.
    pub fn parse_jsonc(text: &str) -> Result<Doc> {
        Doc::parse_with(text, true)
    }

    fn parse_with(text: &str, jsonc: bool) -> Result<Doc> {
        // A byte order mark stays where it is, outside the value.
        let start = if text.starts_with('\u{feff}') { 3 } else { 0 };
        let mut parser = Parser {
            text,
            pos: start,
            depth: 0,
            jsonc,
        };
        parser.skip_ws();
        let root = parser.value()?;
        parser.skip_ws();
        if parser.pos != text.len() {
            return Err(parser.error("unexpected text after the value"));
        }
        let colon = first_colon(text, &root).unwrap_or_else(|| ": ".into());
        Ok(Doc {
            newline: if text.contains("\r\n") { "\r\n" } else { "\n" },
            unit: indent_unit(text),
            colon,
            text: text.to_string(),
            root,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn root(&self) -> &Node {
        &self.root
    }

    pub fn editor(&self) -> Editor<'_> {
        Editor {
            doc: self,
            replaced: Vec::new(),
            inserted: Vec::new(),
            pushed: Vec::new(),
            width: None,
        }
    }

    /// How an object lays out its members.
    fn layout(&self, object: &Node) -> Layout {
        let text = &self.text;
        let Some([first, rest @ ..]) = object.members() else {
            // An empty object takes the file's layout: on several lines unless the file is
            // one line.
            let open = object.span.start;
            let line_start = text[..open].rfind('\n').map_or(0, |i| i + 1);
            let outer: String = text[line_start..open]
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let multiline =
                text[self.root.span.clone()].contains('\n') || object.span == self.root.span;
            return Layout {
                multiline,
                indent: format!("{outer}{}", self.unit),
                outer,
                after_comma: " ".into(),
                colon: self.colon.clone(),
            };
        };
        let before = &text[object.span.start + 1..first.key_span.start];
        let multiline = before.contains('\n');
        let indent = if multiline {
            before.rsplit('\n').next().unwrap_or_default().to_string()
        } else {
            String::new()
        };
        let colon = text[first.key_span.end..first.value.span.start].to_string();
        let after_comma = match rest.first() {
            Some(second) => text[first.value.span.end..second.key_span.start]
                .trim_start_matches(char::is_whitespace)
                .trim_start_matches(',')
                .to_string(),
            None if colon.ends_with(' ') => " ".into(),
            None => String::new(),
        };
        Layout {
            multiline,
            outer: indent
                .strip_suffix(self.unit.as_str())
                .unwrap_or_default()
                .to_string(),
            indent,
            after_comma,
            colon,
        }
    }

    /// `value` as JSON, laid out as `layout` says, its members and items one level in from
    /// `indent`.
    fn render(&self, value: &Value, layout: &Layout) -> String {
        match value {
            Value::Object(map) if !map.is_empty() => {
                let members: Vec<(String, Value)> =
                    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                self.render_members(&members, layout)
            }
            Value::Array(items) if !items.is_empty() => {
                let inner = format!("{}{}", layout.indent, self.unit);
                let nested = Layout {
                    indent: inner.clone(),
                    outer: layout.indent.clone(),
                    ..layout.clone()
                };
                let items: Vec<String> = items
                    .iter()
                    .map(|item| self.render(item, &nested))
                    .collect();
                if layout.multiline {
                    let nl = self.newline;
                    format!(
                        "[{nl}{inner}{}{nl}{}]",
                        items.join(&format!(",{nl}{inner}")),
                        layout.indent
                    )
                } else {
                    format!("[{}]", items.join(&format!(",{}", layout.after_comma)))
                }
            }
            other => serde_json::to_string(other).unwrap_or_default(),
        }
    }

    /// An object of `members`, in their order.
    fn render_members(&self, members: &[(String, Value)], layout: &Layout) -> String {
        let inner = format!("{}{}", layout.indent, self.unit);
        let nested = Layout {
            indent: inner.clone(),
            outer: layout.indent.clone(),
            ..layout.clone()
        };
        let members: Vec<String> = members
            .iter()
            .map(|(key, value)| {
                format!(
                    "{}{}{}",
                    quote(key),
                    layout.colon,
                    self.render(value, &nested)
                )
            })
            .collect();
        if layout.multiline {
            let nl = self.newline;
            format!(
                "{{{nl}{inner}{}{nl}{}}}",
                members.join(&format!(",{nl}{inner}")),
                layout.indent
            )
        } else {
            format!("{{{}}}", members.join(&format!(",{}", layout.after_comma)))
        }
    }
}

/// How an object's members are laid out.
#[derive(Debug, Clone)]
struct Layout {
    /// One member per line.
    multiline: bool,
    /// The members' indentation, on several lines.
    indent: String,
    /// The indentation of the line that holds the object's braces.
    outer: String,
    /// What follows a comma on one line: a space, or nothing.
    after_comma: String,
    colon: String,
}

/// Where a member goes in its object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Before the first member.
    First,
    /// After the member at this index.
    After(usize),
    /// After the last member.
    Last,
}

/// Edits to a document, made on its text when finished.
pub struct Editor<'a> {
    doc: &'a Doc,
    replaced: Vec<(Range<usize>, String)>,
    inserted: Vec<(&'a Node, Place, String, Value)>,
    pushed: Vec<(&'a Node, Value)>,
    /// The line width the repository's formatter keeps, when a list's layout follows it.
    width: Option<usize>,
}

impl<'a> Editor<'a> {
    /// Replaces a value with the string `value`.
    pub fn set_string(&mut self, node: &Node, value: &str) {
        self.replaced.push((node.span.clone(), quote(value)));
    }

    /// Inserts `key` into `object`, a node of this document.
    pub fn insert(&mut self, object: &'a Node, place: Place, key: &str, value: Value) {
        self.inserted.push((object, place, key.to_string(), value));
    }

    /// Appends `value` to `array`, a node of this document, after its last item.
    pub fn push(&mut self, array: &'a Node, value: Value) {
        self.pushed.push((array, value));
    }

    /// Lays out a new list, or one that grows, as Prettier and Biome do within `width`: on one
    /// line when it fits there, else an item a line. Without it, a new list takes its object's
    /// layout, and a list grows on its own lines or its line.
    pub fn wrap_at(&mut self, width: usize) {
        self.width = Some(width);
    }

    /// The edited text.
    pub fn finish(self) -> String {
        let doc = self.doc;
        let text = doc.text();
        let mut edits: Vec<(Range<usize>, String)> = self.replaced;
        // On several lines, what follows a value goes past its line's comma and comment, which
        // stay beside it; the value gets a comma when it has none.
        let mut opened: Vec<usize> = Vec::new();
        let mut after = |edits: &mut Vec<(Range<usize>, String)>,
                         end: usize,
                         multiline: bool,
                         gap: &str,
                         new: &str| {
            match multiline.then(|| line_end(text, end)).flatten() {
                Some((at, true)) => edits.push((at..at, format!("{gap}{new},"))),
                Some((at, false)) if at != end => {
                    if opened.contains(&end) {
                        edits.push((at..at, format!(",{gap}{new}")));
                    } else {
                        opened.push(end);
                        edits.push((end..end, ",".into()));
                        edits.push((at..at, format!("{gap}{new}")));
                    }
                }
                _ => edits.push((end..end, format!(",{gap}{new}"))),
            }
        };
        // An empty object is written whole, once, with every member inserted into it.
        let mut filled: Vec<(&Node, Vec<(String, Value)>)> = Vec::new();
        for (object, place, key, value) in self.inserted {
            let members = object.members().unwrap_or_default();
            if members.is_empty() {
                match filled.iter_mut().find(|(o, _)| o.span == object.span) {
                    Some((_, map)) => map.push((key, value)),
                    None => filled.push((object, vec![(key, value)])),
                }
                continue;
            }
            let layout = doc.layout(object);
            let last = match place {
                Place::First => false,
                Place::After(index) => index + 1 >= members.len(),
                Place::Last => true,
            };
            let rendered = match (&value, self.width) {
                (Value::Array(items), Some(width)) if !items.is_empty() && layout.multiline => {
                    let one = one_line(items.iter().map(|item| item.to_string()));
                    let line = layout.indent.len()
                        + quote(&key).len()
                        + layout.colon.len()
                        + one.len()
                        + usize::from(!last);
                    match line <= width {
                        true => one,
                        false => doc.render(&value, &layout),
                    }
                }
                _ => doc.render(&value, &layout),
            };
            let member = format!("{}{}{rendered}", quote(&key), layout.colon);
            let (gap, indent) = if layout.multiline {
                (doc.newline, layout.indent.as_str())
            } else {
                (layout.after_comma.as_str(), "")
            };
            let end = match place {
                Place::First => {
                    let at = members[0].key_span.start;
                    edits.push((at..at, format!("{member},{gap}{indent}")));
                    continue;
                }
                Place::After(index) => members[index.min(members.len() - 1)].value.span.end,
                Place::Last => members[members.len() - 1].value.span.end,
            };
            after(
                &mut edits,
                end,
                layout.multiline,
                &format!("{gap}{indent}"),
                &member,
            );
        }
        for (object, members) in filled {
            let layout = doc.layout(object);
            let outside = Layout {
                indent: layout.outer.clone(),
                ..layout
            };
            edits.push((object.span.clone(), doc.render_members(&members, &outside)));
        }
        // An array's new items follow its last, on its lines or on its line. One on one line that
        // grows past the width, or an empty one, is written whole: an item a line past it.
        let mut lists: Vec<(&Node, Vec<String>)> = Vec::new();
        for (array, value) in self.pushed {
            let item = serde_json::to_string(&value).unwrap_or_default();
            match lists.iter_mut().find(|(a, _)| a.span == array.span) {
                Some((_, added)) => added.push(item),
                None => lists.push((array, vec![item])),
            }
        }
        for (array, added) in lists {
            let items = array.items().unwrap_or_default();
            let inside = &text[array.span.start + 1..array.span.end - 1];
            if let [first, ..] = items {
                let before = &text[array.span.start + 1..first.span.start];
                if before.contains('\n') {
                    let indent = before.rsplit('\n').next().unwrap_or_default();
                    let gap = format!("{}{indent}", doc.newline);
                    let end = items[items.len() - 1].span.end;
                    for item in &added {
                        after(&mut edits, end, true, &gap, item);
                    }
                    continue;
                }
            }
            let gap = match items {
                [first, second, ..] => text[first.span.end..second.span.start]
                    .trim_start_matches(char::is_whitespace)
                    .trim_start_matches(',')
                    .to_string(),
                _ => " ".into(),
            };
            let all: Vec<String> = items
                .iter()
                .map(|item| text[item.span.clone()].to_string())
                .chain(added.iter().cloned())
                .collect();
            let line_start = text[..array.span.start].rfind('\n').map_or(0, |at| at + 1);
            let line_end = text[array.span.end..]
                .find('\n')
                .map_or(text.len(), |at| array.span.end + at);
            let one = format!("[{}]", all.join(&format!(",{gap}")));
            let line = text[line_start..line_end].trim_end_matches('\r').len()
                - (array.span.end - array.span.start)
                + one.len();
            // Only a list on its member's own line breaks: one inside a line of more is the
            // whole line's to lay out.
            let prefix = text[line_start..array.span.start].trim_start();
            let suffix = text[array.span.end..line_end].trim_end_matches('\r');
            let suffix = suffix.trim_start().trim_start_matches(',').trim_start();
            let own_line = prefix.starts_with('"')
                && !prefix.contains(['{', '['])
                && (suffix.is_empty() || suffix.starts_with("//"));
            let comments = inside.contains("//") || inside.contains("/*");
            if self.width.is_some_and(|width| line > width) && own_line && !comments {
                let outer: String = text[line_start..array.span.start]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let inner = format!("{outer}{}", doc.unit);
                let nl = doc.newline;
                edits.push((
                    array.span.clone(),
                    format!(
                        "[{nl}{inner}{}{nl}{outer}]",
                        all.join(&format!(",{nl}{inner}"))
                    ),
                ));
            } else if let Some(last) = items.last() {
                let end = last.span.end;
                for item in &added {
                    edits.push((end..end, format!(",{gap}{item}")));
                }
            } else {
                edits.push((array.span.clone(), one));
            }
        }
        // Insertions at the same place keep their order.
        edits.sort_by_key(|(range, _)| (range.start, range.end));
        let mut out = String::with_capacity(text.len() + 256);
        let mut at = 0;
        for (range, replacement) in edits {
            debug_assert!(range.start >= at, "overlapping edits");
            out.push_str(&text[at..range.start.max(at)]);
            out.push_str(&replacement);
            at = range.end.max(at);
        }
        out.push_str(&text[at..]);
        out
    }
}

/// `items`, JSON already, as a list on one line.
fn one_line(items: impl Iterator<Item = String>) -> String {
    format!("[{}]", items.collect::<Vec<_>>().join(", "))
}

/// The end of the line of a value that ends at `end`, past its comma and a comment there, and
/// whether that comma is there: none when anything else follows on the line.
fn line_end(text: &str, end: usize) -> Option<(usize, bool)> {
    let bytes = text.as_bytes();
    let blank = |mut at: usize| {
        while matches!(bytes.get(at), Some(b' ' | b'\t')) {
            at += 1;
        }
        at
    };
    let mut at = blank(end);
    let comma = bytes.get(at) == Some(&b',');
    if comma {
        at = blank(at + 1);
    }
    let rest = &text[at..];
    if rest.starts_with("//") {
        at += rest.find('\n')?;
        if bytes[at - 1] == b'\r' {
            at -= 1;
        }
    } else if let Some(comment) = rest.strip_prefix("/*") {
        let close = comment.find("*/")? + 4;
        if rest[..close].contains('\n') {
            return None;
        }
        at = blank(at + close);
    }
    matches!(bytes.get(at), Some(b'\n' | b'\r')).then_some((at, comma))
}

/// `value` as a JSON string, escaped as `JSON.stringify` escapes it.
pub fn quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// The value a text holds, read by serde_json: what checks an edit.
pub fn value(text: &str) -> Result<Value> {
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(text)).map_err(|e| anyhow!("{e}"))
}

/// The whitespace of the first indented line, else two spaces.
fn indent_unit(text: &str) -> String {
    text.split('\n')
        .skip(1)
        .map(|line| line.trim_end_matches('\r'))
        .find_map(|line| {
            let indent: String = line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            (!indent.is_empty() && indent.len() < line.len()).then_some(indent)
        })
        .unwrap_or_else(|| "  ".into())
}

/// What separates the first key in the document from its value.
fn first_colon(text: &str, node: &Node) -> Option<String> {
    match &node.kind {
        Kind::Object(members) => members.iter().find_map(|m| {
            Some(text[m.key_span.end..m.value.span.start].to_string())
                .filter(|colon| !colon.contains('\n'))
                .or_else(|| first_colon(text, &m.value))
        }),
        Kind::Array(items) => items.iter().find_map(|item| first_colon(text, item)),
        _ => None,
    }
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
    depth: usize,
    /// It reads comments as whitespace, and a comma before `}` or `]`.
    jsonc: bool,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.pos += 1,
                Some(b'/') if self.jsonc => {
                    let rest = &self.text[self.pos..];
                    if rest.starts_with("//") {
                        self.pos += rest.find('\n').unwrap_or(rest.len());
                    } else if let Some(comment) = rest.strip_prefix("/*") {
                        // A comment that never ends runs to the end, where the value is missing.
                        self.pos += comment.find("*/").map_or(rest.len(), |end| end + 4);
                    } else {
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    fn error(&self, what: &str) -> anyhow::Error {
        let before = &self.text[..self.pos.min(self.text.len())];
        let line = before.matches('\n').count() + 1;
        let column = before
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count()
            + 1;
        anyhow!("line {line}, column {column}: {what}")
    }

    fn value(&mut self) -> Result<Node> {
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => {
                let start = self.pos;
                let text = self.string()?;
                Ok(Node {
                    span: start..self.pos,
                    kind: Kind::String(text),
                })
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b't') => self.literal("true"),
            Some(b'f') => self.literal("false"),
            Some(b'n') => self.literal("null"),
            Some(_) => Err(self.error("expected a value")),
            None => Err(self.error("the file ends before the value does")),
        }
    }

    fn nest(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("values nest too deep"));
        }
        Ok(())
    }

    fn object(&mut self) -> Result<Node> {
        let start = self.pos;
        self.nest()?;
        self.pos += 1;
        let mut members = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
        } else {
            loop {
                self.skip_ws();
                if self.jsonc && self.peek() == Some(b'}') {
                    self.pos += 1;
                    break;
                }
                if self.peek() != Some(b'"') {
                    return Err(self.error("expected a key in double quotes"));
                }
                let key_start = self.pos;
                let key = self.string()?;
                let key_span = key_start..self.pos;
                self.skip_ws();
                if self.peek() != Some(b':') {
                    return Err(self.error("expected `:` after the key"));
                }
                self.pos += 1;
                self.skip_ws();
                let value = self.value()?;
                members.push(Member {
                    key,
                    key_span,
                    value,
                });
                self.skip_ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b'}') => {
                        self.pos += 1;
                        break;
                    }
                    _ => return Err(self.error("expected `,` or `}`")),
                }
            }
        }
        self.depth -= 1;
        Ok(Node {
            span: start..self.pos,
            kind: Kind::Object(members),
        })
    }

    fn array(&mut self) -> Result<Node> {
        let start = self.pos;
        self.nest()?;
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
        } else {
            loop {
                self.skip_ws();
                if self.jsonc && self.peek() == Some(b']') {
                    self.pos += 1;
                    break;
                }
                items.push(self.value()?);
                self.skip_ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b']') => {
                        self.pos += 1;
                        break;
                    }
                    _ => return Err(self.error("expected `,` or `]`")),
                }
            }
        }
        self.depth -= 1;
        Ok(Node {
            span: start..self.pos,
            kind: Kind::Array(items),
        })
    }

    /// A string, at its opening quote, unescaped.
    fn string(&mut self) -> Result<String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let rest = &self.text[self.pos..];
            let Some(i) = rest.find(|c: char| c == '"' || c == '\\' || c < ' ') else {
                return Err(self.error("a string doesn't end"));
            };
            out.push_str(&rest[..i]);
            self.pos += i;
            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    self.escape(&mut out)?;
                }
                _ => return Err(self.error("a control character in a string")),
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<()> {
        let Some(c) = self.peek() else {
            return Err(self.error("a string doesn't end"));
        };
        self.pos += 1;
        match c {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let first = self.hex4()?;
                let mut decoded = char::from_u32(first);
                // A high surrogate takes the low one that follows it.
                if (0xD800..0xDC00).contains(&first) && self.text[self.pos..].starts_with("\\u") {
                    let save = self.pos;
                    self.pos += 2;
                    let second = self.hex4()?;
                    if (0xDC00..0xE000).contains(&second) {
                        decoded =
                            char::from_u32(0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00));
                    } else {
                        self.pos = save;
                    }
                }
                out.push(decoded.unwrap_or('\u{fffd}'));
            }
            _ => return Err(self.error("an unknown escape in a string")),
        }
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32> {
        let digits = self
            .text
            .get(self.pos..self.pos + 4)
            .filter(|d| d.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| self.error("expected four hex digits after \\u"))?;
        self.pos += 4;
        Ok(u32::from_str_radix(digits, 16)?)
    }

    fn number(&mut self) -> Result<Node> {
        let start = self.pos;
        let bytes = self.text.as_bytes();
        let digits = |mut i: usize| {
            let from = i;
            while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            (i > from).then_some(i)
        };
        let mut i = start;
        if bytes.get(i) == Some(&b'-') {
            i += 1;
        }
        i = match bytes.get(i) {
            Some(b'0') => i + 1,
            Some(b'1'..=b'9') => digits(i).unwrap_or(i),
            _ => return Err(self.error("expected a number")),
        };
        if bytes.get(i) == Some(&b'.') {
            i = digits(i + 1).ok_or_else(|| self.error("expected digits after `.`"))?;
        }
        if matches!(bytes.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(bytes.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            i = digits(i).ok_or_else(|| self.error("expected an exponent"))?;
        }
        self.pos = i;
        Ok(Node {
            span: start..i,
            kind: Kind::Scalar,
        })
    }

    fn literal(&mut self, word: &str) -> Result<Node> {
        if !self.text[self.pos..].starts_with(word) {
            return Err(self.error("expected a value"));
        }
        let start = self.pos;
        self.pos += word.len();
        Ok(Node {
            span: start..self.pos,
            kind: Kind::Scalar,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Sets `scripts.test`, adds `scripts.lint` after it, `hushrun` at the end and a
    /// devDependency first, as init does.
    fn edit(text: &str) -> String {
        let doc = Doc::parse(text).unwrap();
        let root = doc.root();
        let scripts = root.get("scripts").unwrap();
        let dev = root.get("devDependencies").unwrap();
        let mut editor = doc.editor();
        editor.set_string(scripts.get("test").unwrap(), "hushrun run -- vitest run");
        editor.insert(scripts, Place::Last, "lint", json!("eslint ."));
        editor.insert(dev, Place::First, "a", json!("^1.0.0"));
        editor.insert(
            root,
            Place::Last,
            "hushrun",
            json!({"backend": "https://h/o/acme"}),
        );
        let edited = editor.finish();
        // What serde_json reads is what the edits meant, whatever the layout.
        let mut expected = value(text).unwrap();
        expected["scripts"]["test"] = json!("hushrun run -- vitest run");
        expected["scripts"]["lint"] = json!("eslint .");
        expected["devDependencies"]["a"] = json!("^1.0.0");
        expected["hushrun"] = json!({"backend": "https://h/o/acme"});
        assert_eq!(value(&edited).unwrap(), expected, "{edited}");
        edited
    }

    // covers: C-15
    #[test]
    fn edits_keep_two_space_indentation_and_the_rest_of_the_bytes() {
        let text = "{\n  \"name\": \"shop\",\n  \"scripts\": {\n    \"test\": \"vitest\"\n  },\n  \"devDependencies\": {\n    \"vitest\": \"^2.0.0\"\n  }\n}\n";
        assert_eq!(
            edit(text),
            "{\n  \"name\": \"shop\",\n  \"scripts\": {\n    \"test\": \"hushrun run -- vitest run\",\n    \"lint\": \"eslint .\"\n  },\n  \"devDependencies\": {\n    \"a\": \"^1.0.0\",\n    \"vitest\": \"^2.0.0\"\n  },\n  \"hushrun\": {\n    \"backend\": \"https://h/o/acme\"\n  }\n}\n"
        );
    }

    // covers: C-15
    #[test]
    fn edits_keep_four_spaces_tabs_crlf_and_a_missing_final_newline() {
        let four = "{\n    \"scripts\": {\"test\": \"vitest\"},\n    \"devDependencies\": {\n        \"z\": \"1\"\n    }\n}";
        assert_eq!(
            edit(four),
            "{\n    \"scripts\": {\"test\": \"hushrun run -- vitest run\", \"lint\": \"eslint .\"},\n    \"devDependencies\": {\n        \"a\": \"^1.0.0\",\n        \"z\": \"1\"\n    },\n    \"hushrun\": {\n        \"backend\": \"https://h/o/acme\"\n    }\n}"
        );
        let tabs = "{\r\n\t\"scripts\": {\r\n\t\t\"test\": \"vitest\"\r\n\t},\r\n\t\"devDependencies\": {}\r\n}\r\n";
        assert_eq!(
            edit(tabs),
            "{\r\n\t\"scripts\": {\r\n\t\t\"test\": \"hushrun run -- vitest run\",\r\n\t\t\"lint\": \"eslint .\"\r\n\t},\r\n\t\"devDependencies\": {\r\n\t\t\"a\": \"^1.0.0\"\r\n\t},\r\n\t\"hushrun\": {\r\n\t\t\"backend\": \"https://h/o/acme\"\r\n\t}\r\n}\r\n"
        );
    }

    // covers: C-15
    #[test]
    fn a_one_line_file_stays_on_one_line() {
        assert_eq!(
            edit(r#"{"scripts":{"test":"vitest"},"devDependencies":{}}"#),
            r#"{"scripts":{"test":"hushrun run -- vitest run","lint":"eslint ."},"devDependencies":{"a":"^1.0.0"},"hushrun":{"backend":"https://h/o/acme"}}"#
        );
    }

    // covers: C-15
    #[test]
    fn escapes_and_nested_values_are_read_and_kept() {
        let text = "{\n  \"scripts\": {\n    \"test\": \"vitest\",\n    \"e2e\": \"run \\\"a\\\" \\u00e9\\ud83d\\ude00 \\/x\"\n  },\n  \"nested\": {\"a\": [1, {\"b\": null}], \"c\": -1.5e3},\n  \"devDependencies\": {\"x\": \"1\"}\n}\n";
        let doc = Doc::parse(text).unwrap();
        assert_eq!(
            doc.root()
                .get("scripts")
                .unwrap()
                .get("e2e")
                .unwrap()
                .as_str(),
            Some("run \"a\" é😀 /x")
        );
        let edited = edit(text);
        // Untouched values keep their escapes.
        assert!(
            edited.contains(r#""e2e": "run \"a\" \u00e9\ud83d\ude00 \/x""#),
            "{edited}"
        );
        assert!(
            edited.contains(r#""nested": {"a": [1, {"b": null}], "c": -1.5e3}"#),
            "{edited}"
        );
        let mut editor = doc.editor();
        editor.set_string(
            doc.root().get("scripts").unwrap().get("test").unwrap(),
            "say \"hi\"\t\\",
        );
        assert!(editor.finish().contains(r#""test": "say \"hi\"\t\\""#));
    }

    // covers: C-15
    #[test]
    fn an_existing_hushrun_object_gets_its_backend() {
        let text = "{\n  \"hushrun\": {\n    \"other\": true\n  }\n}\n";
        let doc = Doc::parse(text).unwrap();
        let hushrun = doc.root().get("hushrun").unwrap();
        let mut editor = doc.editor();
        editor.insert(hushrun, Place::Last, "backend", json!("https://h/o/acme"));
        assert_eq!(
            editor.finish(),
            "{\n  \"hushrun\": {\n    \"other\": true,\n    \"backend\": \"https://h/o/acme\"\n  }\n}\n"
        );
        let empty = Doc::parse("{\"hushrun\": {}}").unwrap();
        let mut editor = empty.editor();
        editor.insert(
            empty.root().get("hushrun").unwrap(),
            Place::Last,
            "backend",
            json!("https://h/o/acme"),
        );
        assert_eq!(
            editor.finish(),
            "{\"hushrun\": {\"backend\": \"https://h/o/acme\"}}"
        );
        // An empty file's object takes members on their own lines.
        let bare = Doc::parse("{}\n").unwrap();
        let mut editor = bare.editor();
        editor.insert(bare.root(), Place::Last, "hushrun", json!({"backend": "u"}));
        editor.insert(
            bare.root(),
            Place::Last,
            "devDependencies",
            json!({"p": "1"}),
        );
        assert_eq!(
            editor.finish(),
            "{\n  \"hushrun\": {\n    \"backend\": \"u\"\n  },\n  \"devDependencies\": {\n    \"p\": \"1\"\n  }\n}\n"
        );
    }

    // covers: C-15
    #[test]
    fn members_go_where_they_are_put() {
        let text = "{\n  \"a\": 1,\n  \"c\": 3\n}";
        let doc = Doc::parse(text).unwrap();
        let mut editor = doc.editor();
        editor.insert(doc.root(), Place::After(0), "b", json!(2));
        editor.insert(doc.root(), Place::First, "first", json!(0));
        assert_eq!(
            editor.finish(),
            "{\n  \"first\": 0,\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}"
        );
        // Duplicate keys: the last one counts, as for JSON parsers.
        let dup = Doc::parse(r#"{"a": 1, "a": "two"}"#).unwrap();
        assert_eq!(dup.root().get("a").unwrap().as_str(), Some("two"));
        assert_eq!(dup.root().index_of("a"), Some(1));
    }

    #[test]
    fn invalid_json_is_refused_with_its_place() {
        for (text, message) in [
            (
                "{\n  \"a\": 1,\n}",
                "line 3, column 1: expected a key in double quotes",
            ),
            ("{\"a\": tru}", "expected a value"),
            ("{\"a\": 01}", "expected `,` or `}`"),
            ("{\"a\": \"x\ny\"}", "a control character in a string"),
            ("{\"a\": 1} x", "unexpected text after the value"),
            ("{\"a\": \"\\q\"}", "an unknown escape in a string"),
            ("[1, 2", "expected `,` or `]`"),
            ("", "the file ends before the value does"),
        ] {
            let err = Doc::parse(text).unwrap_err().to_string();
            assert!(err.contains(message), "{text:?}: {err}");
        }
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert!(Doc::parse(&deep).unwrap_err().to_string().contains("nest"));
        // A byte order mark is kept, outside the value.
        let bom = Doc::parse("\u{feff}{\"a\": \"b\"}").unwrap();
        assert_eq!(bom.root().get("a").unwrap().as_str(), Some("b"));
        // Comments and trailing commas are JSON with comments', not JSON's.
        for text in ["{\"a\": 1, // x\n\"b\": 2}", "{\"a\": [1,]}", "{\"a\": 1,}"] {
            assert!(Doc::parse(text).is_err(), "{text:?}");
            assert!(Doc::parse_jsonc(text).is_ok(), "{text:?}");
        }
        assert!(Doc::parse_jsonc("{\"a\": 1 /* never ends").is_err());
        assert!(Doc::parse_jsonc("{,}").is_err());
    }

    // covers: C-15
    #[test]
    fn items_follow_the_last_on_its_lines_or_on_its_line() {
        let push = |text: &str| {
            let doc = Doc::parse_jsonc(text).unwrap();
            let list = doc.root().get("list").unwrap();
            let mut editor = doc.editor();
            editor.push(list, json!("X_*"));
            editor.push(list, json!("Y"));
            editor.finish()
        };
        assert_eq!(
            push("{\n  \"list\": [\n    \"a\",\n    \"b\"\n  ]\n}\n"),
            "{\n  \"list\": [\n    \"a\",\n    \"b\",\n    \"X_*\",\n    \"Y\"\n  ]\n}\n"
        );
        assert_eq!(
            push("{\"list\": [\"a\", \"b\"]}"),
            "{\"list\": [\"a\", \"b\", \"X_*\", \"Y\"]}"
        );
        assert_eq!(
            push("{\"list\": [\"a\",\"b\"]}"),
            "{\"list\": [\"a\",\"b\",\"X_*\",\"Y\"]}"
        );
        assert_eq!(
            push("{\"list\": [\"a\"]}"),
            "{\"list\": [\"a\", \"X_*\", \"Y\"]}"
        );
        assert_eq!(push("{\"list\": []}"), "{\"list\": [\"X_*\", \"Y\"]}");
        // Within a width, a list on its own line breaks past it, and one inside a line of more
        // stays on it.
        let wrap = |text: &str| {
            let doc = Doc::parse(text).unwrap();
            let list = doc.root().get("list").unwrap();
            let mut editor = doc.editor();
            editor.wrap_at(24);
            editor.push(list, json!("X_*"));
            editor.finish()
        };
        assert_eq!(
            wrap("{\n  \"list\": [\"a\", \"b\"],\n  \"c\": 1\n}"),
            "{\n  \"list\": [\n    \"a\",\n    \"b\",\n    \"X_*\"\n  ],\n  \"c\": 1\n}"
        );
        assert_eq!(
            wrap("{\n  \"list\": [\"a\"]\n}"),
            "{\n  \"list\": [\"a\", \"X_*\"]\n}"
        );
        assert_eq!(
            wrap("{\"list\": [\"a\", \"b\"], \"c\": 1}"),
            "{\"list\": [\"a\", \"b\", \"X_*\"], \"c\": 1}"
        );
        // A comment stays beside its value, and a trailing comma after the new last item.
        assert_eq!(
            push("{\n  \"list\": [\n    \"a\", // why\n  ],\n}"),
            "{\n  \"list\": [\n    \"a\", // why\n    \"X_*\",\n    \"Y\",\n  ],\n}"
        );
        assert_eq!(
            push("{\r\n  \"list\": [\r\n    \"a\" /* why */\r\n  ]\r\n}"),
            "{\r\n  \"list\": [\r\n    \"a\", /* why */\r\n    \"X_*\",\r\n    \"Y\"\r\n  ]\r\n}"
        );
    }

    // covers: C-15
    #[test]
    fn json_with_comments_keeps_them_and_takes_a_list() {
        let text = "{\n  // Turborepo's settings.\n  \"$schema\": \"https://turborepo.com/schema.json\",\n  /* the tasks */\n  \"tasks\": {\n    \"test\": {}, // every package's\n  },\n}\n";
        let doc = Doc::parse_jsonc(text).unwrap();
        let mut editor = doc.editor();
        editor.insert(
            doc.root(),
            Place::After(0),
            "globalPassThroughEnv",
            json!(["HUSHRUN_*", "GITHUB_*"]),
        );
        assert_eq!(
            editor.finish(),
            "{\n  // Turborepo's settings.\n  \"$schema\": \"https://turborepo.com/schema.json\",\n  \"globalPassThroughEnv\": [\n    \"HUSHRUN_*\",\n    \"GITHUB_*\"\n  ],\n  /* the tasks */\n  \"tasks\": {\n    \"test\": {}, // every package's\n  },\n}\n"
        );
        // On one line, a list stays on it.
        let doc = Doc::parse("{\"tasks\": {}}").unwrap();
        let mut editor = doc.editor();
        editor.insert(doc.root(), Place::First, "list", json!(["a", "b"]));
        assert_eq!(editor.finish(), "{\"list\": [\"a\", \"b\"], \"tasks\": {}}");
    }
}
