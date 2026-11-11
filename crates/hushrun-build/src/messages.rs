//! What rustc writes for cargo on stderr (B-12): the notices that an output is ready, and the
//! diagnostics. Read as they arrive, kept with the block, written back for another folder.
//!
//! With `--error-format=json` rustc writes one JSON object per line. Cargo starts a crate's
//! dependents as soon as it reads the notice that the `.rmeta` is ready, so [`Recorder`] passes
//! each line on at once and keeps it. [`Recorded::store`] turns the lines' paths into what the
//! caller wants kept (placeholders), and [`Stored::replay`] writes them back for this folder,
//! the notices last.

use std::io;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One line rustc wrote on stderr.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "line", rename_all = "lowercase")]
pub enum Line {
    /// `{"$message_type":"artifact","artifact":"…","emit":"…"}`: an output is ready.
    Notice {
        /// The output's path.
        artifact: String,
        /// Its kind, as in `--emit`: `dep-info`, `metadata`, `link`.
        emit: String,
    },
    /// Any other JSON object: a diagnostic, the future-incompatibility report, and the like.
    Diagnostic(Value),
    /// A line that isn't a JSON object, such as what a proc macro printed itself.
    Text(String),
}

impl Line {
    /// Reads one line, without its newline.
    pub fn parse(line: &str) -> Line {
        let Ok(Value::Object(object)) = serde_json::from_str::<Value>(line) else {
            return Line::Text(line.to_owned());
        };
        if object.get("$message_type").and_then(Value::as_str) == Some("artifact")
            && let (Some(Value::String(artifact)), Some(Value::String(emit))) =
                (object.get("artifact"), object.get("emit"))
        {
            return Line::Notice {
                artifact: artifact.clone(),
                emit: emit.clone(),
            };
        }
        Line::Diagnostic(Value::Object(object))
    }

    fn is_notice(&self) -> bool {
        matches!(self, Line::Notice { .. })
    }
}

/// How messages are rendered (B-12): `--error-format`, `--json` and `--color`. Messages are kept
/// per form, and two spellings of one form are equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Form(String);

impl Form {
    /// The form of a compile given these flags' values, or `None` for a flag not given.
    ///
    /// An absent flag is its default (`human`, no `--json` kinds, `auto`). `--json` kinds are a
    /// set: their order and repeats don't count. A caller that saw `--json` more than once joins
    /// the values with `,`, as rustc does.
    pub fn new(error_format: Option<&str>, json: Option<&str>, color: Option<&str>) -> Form {
        let mut kinds: Vec<&str> = json
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .collect();
        kinds.sort_unstable();
        kinds.dedup();
        Form(format!(
            "error-format={} json={} color={}",
            error_format.unwrap_or("human"),
            kinds.join(","),
            color.unwrap_or("auto"),
        ))
    }

    /// The form as one stable text, for naming what is kept per form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Passes rustc's stderr lines on as they come and keeps them.
#[derive(Debug)]
pub struct Recorder<W: io::Write> {
    out: W,
    form: Form,
    lines: Vec<Line>,
}

impl<W: io::Write> Recorder<W> {
    /// A recorder that writes to `out` (the wrapper's stderr) lines rendered in `form`.
    pub fn new(out: W, form: Form) -> Recorder<W> {
        Recorder {
            out,
            form,
            lines: Vec::new(),
        }
    }

    /// Writes `line` to the output at once, with its newline, and keeps it.
    ///
    /// The line is kept even when the write fails, so the record says what rustc wrote. A
    /// trailing `\n` in `line` is not doubled.
    pub fn line(&mut self, line: &str) -> io::Result<()> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let written = self
            .out
            .write_all(line.as_bytes())
            .and_then(|()| self.out.write_all(b"\n"))
            .and_then(|()| self.out.flush());
        self.lines.push(Line::parse(line));
        written
    }

    /// Everything kept, in the order rustc wrote it.
    pub fn finish(self) -> Recorded {
        Recorded {
            form: self.form,
            lines: self.lines,
        }
    }
}

/// The lines one compile wrote on stderr, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    form: Form,
    lines: Vec<Line>,
}

impl Recorded {
    /// The form the lines were rendered in.
    pub fn form(&self) -> &Form {
        &self.form
    }

    /// The lines, in the order rustc wrote them.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// True when the compile wrote nothing but notices: such a block serves every form (B-12).
    pub fn only_notices(&self) -> bool {
        self.lines.iter().all(Line::is_notice)
    }

    /// The lines with every path changed by `map`, to be kept in a block.
    ///
    /// In a notice, the `artifact` path. In any other JSON line, every `file_name`, at any
    /// depth (spans, their macro expansions, children, the future-incompatibility report), and
    /// those same paths where they appear in `rendered` and `message` texts. Text lines are kept
    /// as they are.
    pub fn store(&self, map: impl Fn(&str) -> String) -> Stored {
        Stored {
            form: self.form.clone(),
            lines: self.lines.iter().map(|line| map_line(line, &map)).collect(),
        }
    }
}

/// A compile's messages as kept in a block, with the form they were rendered in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    form: Form,
    lines: Vec<Line>,
}

impl Stored {
    /// The form the lines were rendered in.
    pub fn form(&self) -> &Form {
        &self.form
    }

    /// True when the compile wrote nothing but notices: such a block serves every form (B-12).
    pub fn only_notices(&self) -> bool {
        self.lines.iter().all(Line::is_notice)
    }

    /// Writes the lines for this folder, with every path changed by `map` as in
    /// [`Recorded::store`]: diagnostics and text first, in their order, then the notices, in
    /// their order (B-12). Each JSON line is valid JSON; the output is flushed.
    pub fn replay(&self, map: impl Fn(&str) -> String, out: &mut impl io::Write) -> io::Result<()> {
        let (notices, others): (Vec<&Line>, Vec<&Line>) =
            self.lines.iter().partition(|line| line.is_notice());
        for line in others.into_iter().chain(notices) {
            let text = render(&map_line(line, &map))?;
            out.write_all(text.as_bytes())?;
            out.write_all(b"\n")?;
        }
        out.flush()
    }
}

/// A line as rustc writes it. A notice takes rustc's own layout.
fn render(line: &Line) -> io::Result<String> {
    Ok(match line {
        Line::Notice { artifact, emit } => format!(
            r#"{{"$message_type":"artifact","artifact":{},"emit":{}}}"#,
            serde_json::to_string(artifact)?,
            serde_json::to_string(emit)?,
        ),
        Line::Diagnostic(value) => serde_json::to_string(value)?,
        Line::Text(text) => text.clone(),
    })
}

fn map_line(line: &Line, map: &impl Fn(&str) -> String) -> Line {
    match line {
        Line::Notice { artifact, emit } => Line::Notice {
            artifact: map(artifact),
            emit: emit.clone(),
        },
        Line::Diagnostic(value) => Line::Diagnostic(map_diagnostic(value, map)),
        Line::Text(text) => Line::Text(text.clone()),
    }
}

/// The diagnostic with each `file_name` mapped, and those paths replaced in its texts.
fn map_diagnostic(value: &Value, map: &impl Fn(&str) -> String) -> Value {
    let mut names = Vec::new();
    collect_file_names(value, &mut names);
    names.sort_unstable();
    names.dedup();
    let mut pairs: Vec<(String, String)> = names
        .into_iter()
        .filter_map(|name| {
            let mapped = map(&name);
            (mapped != name).then_some((name, mapped))
        })
        .collect();
    if pairs.is_empty() {
        return value.clone();
    }
    // Longest first, so a path is never replaced by the start of a longer one.
    pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    let mut value = value.clone();
    rewrite(&mut value, &pairs);
    value
}

fn collect_file_names(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, item) in object {
                match item {
                    Value::String(name) if key == "file_name" => names.push(name.clone()),
                    _ => collect_file_names(item, names),
                }
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_file_names(item, names)),
        _ => {}
    }
}

fn rewrite(value: &mut Value, pairs: &[(String, String)]) {
    match value {
        Value::Object(object) => rewrite_object(object, pairs),
        Value::Array(items) => items.iter_mut().for_each(|item| rewrite(item, pairs)),
        _ => {}
    }
}

fn rewrite_object(object: &mut Map<String, Value>, pairs: &[(String, String)]) {
    for (key, item) in object.iter_mut() {
        match item {
            Value::String(text) if key == "file_name" => {
                if let Some((_, mapped)) = pairs.iter().find(|(name, _)| name == text) {
                    *text = mapped.clone();
                }
            }
            Value::String(text) if key == "rendered" || key == "message" => {
                *text = replace_all(text, pairs);
            }
            _ => rewrite(item, pairs),
        }
    }
}

/// `text` with every occurrence of each pair's first path replaced by its second, in one pass
/// so a replacement is never replaced again. `pairs` is sorted longest first.
fn replace_all(text: &str, pairs: &[(String, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'scan: while let Some(c) = rest.chars().next() {
        for (name, mapped) in pairs {
            if let Some(after) = rest.strip_prefix(name.as_str()) {
                out.push_str(mapped);
                rest = after;
                continue 'scan;
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    // What rustc 1.94.0 wrote on stderr for `rustc --edition 2021 --error-format=json
    // --json=diagnostic-rendered-ansi,artifacts,future-incompat --emit=dep-info,metadata,link
    // --crate-type lib` on a crate given by absolute path, with that path's folder shortened to
    // `/w`. The source: a `macro_rules! make` whose body binds an unused variable, called from
    // `f`; an unused `let x`; in `mod m`, a call to a deprecated function and two dead ones.
    // Lines 4 to 6 (the plain unused-variable and dead-code warnings) are left out.
    const DEP_INFO_READY: &str =
        r#"{"$message_type":"artifact","artifact":"/w/out/msg-abc.d","emit":"dep-info"}"#;
    /// A diagnostic with a child note.
    const DEPRECATED: &str = r#"{"$message_type":"diagnostic","message":"use of deprecated function `m::old`: use other","code":{"code":"deprecated","explanation":null},"level":"warning","spans":[{"file_name":"/w/src/m.rs","byte_start":119,"byte_end":122,"line_start":4,"line_end":4,"column_start":14,"column_end":17,"is_primary":true,"text":[{"text":"pub fn k() { old(); }","highlight_start":14,"highlight_end":17}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[{"message":"`#[warn(deprecated)]` on by default","code":null,"level":"note","spans":[],"children":[],"rendered":null}],"rendered":"\u001b[1m\u001b[33mwarning\u001b[0m\u001b[1m: use of deprecated function `m::old`: use other\u001b[0m\n \u001b[1m\u001b[94m--> \u001b[0m/w/src/m.rs:4:14\n  \u001b[1m\u001b[94m|\u001b[0m\n\u001b[1m\u001b[94m4\u001b[0m \u001b[1m\u001b[94m|\u001b[0m pub fn k() { old(); }\n  \u001b[1m\u001b[94m|\u001b[0m              \u001b[1m\u001b[33m^^^\u001b[0m\n  \u001b[1m\u001b[94m|\u001b[0m\n  \u001b[1m\u001b[94m= \u001b[0m\u001b[1mnote\u001b[0m: `#[warn(deprecated)]` on by default\n\n"}"#;
    /// A diagnostic in a macro expansion, with a `help:` child that has its own spans.
    const IN_MACRO: &str = r#"{"$message_type":"diagnostic","message":"unused variable: `unused_in_macro`","code":{"code":"unused_variables","explanation":null},"level":"warning","spans":[{"file_name":"/w/src/lib.rs","byte_start":43,"byte_end":58,"line_start":3,"line_end":3,"column_start":17,"column_end":32,"is_primary":true,"text":[{"text":"    () => { let unused_in_macro = 1; };","highlight_start":17,"highlight_end":32}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":{"span":{"file_name":"/w/src/lib.rs","byte_start":86,"byte_end":93,"line_start":6,"line_end":6,"column_start":5,"column_end":12,"is_primary":false,"text":[{"text":"    make!();","highlight_start":5,"highlight_end":12}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null},"macro_decl_name":"make!","def_site_span":{"file_name":"/w/src/lib.rs","byte_start":7,"byte_end":24,"line_start":2,"line_end":2,"column_start":1,"column_end":18,"is_primary":false,"text":[{"text":"macro_rules! make {","highlight_start":1,"highlight_end":18}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}}}],"children":[{"message":"`unused_in_macro` is captured in macro and introduced a unused variable","code":null,"level":"help","spans":[{"file_name":"/w/src/lib.rs","byte_start":43,"byte_end":58,"line_start":3,"line_end":3,"column_start":17,"column_end":32,"is_primary":true,"text":[{"text":"    () => { let unused_in_macro = 1; };","highlight_start":17,"highlight_end":32}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":{"span":{"file_name":"/w/src/lib.rs","byte_start":86,"byte_end":93,"line_start":6,"line_end":6,"column_start":5,"column_end":12,"is_primary":false,"text":[{"text":"    make!();","highlight_start":5,"highlight_end":12}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null},"macro_decl_name":"make!","def_site_span":{"file_name":"/w/src/lib.rs","byte_start":7,"byte_end":24,"line_start":2,"line_end":2,"column_start":1,"column_end":18,"is_primary":false,"text":[{"text":"macro_rules! make {","highlight_start":1,"highlight_end":18}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}}}],"children":[],"rendered":null},{"message":"`#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default","code":null,"level":"note","spans":[],"children":[],"rendered":null}],"rendered":"\u001b[1m\u001b[33mwarning\u001b[0m\u001b[1m: unused variable: `unused_in_macro`\u001b[0m\n \u001b[1m\u001b[94m--> \u001b[0m/w/src/lib.rs:3:17\n  \u001b[1m\u001b[94m|\u001b[0m\n\u001b[1m\u001b[94m3\u001b[0m \u001b[1m\u001b[94m|\u001b[0m     () => { let unused_in_macro = 1; };\n  \u001b[1m\u001b[94m|\u001b[0m                 \u001b[1m\u001b[33m^^^^^^^^^^^^^^^\u001b[0m\n\u001b[1m\u001b[94m...\u001b[0m\n\u001b[1m\u001b[94m6\u001b[0m \u001b[1m\u001b[94m|\u001b[0m     make!();\n  \u001b[1m\u001b[94m|\u001b[0m     \u001b[1m\u001b[94m-------\u001b[0m \u001b[1m\u001b[94min this macro invocation\u001b[0m\n  \u001b[1m\u001b[94m|\u001b[0m\n\u001b[1m\u001b[96mhelp\u001b[0m: `unused_in_macro` is captured in macro and introduced a unused variable\n \u001b[1m\u001b[94m--> \u001b[0m/w/src/lib.rs:3:17\n  \u001b[1m\u001b[94m|\u001b[0m\n\u001b[1m\u001b[94m3\u001b[0m \u001b[1m\u001b[94m|\u001b[0m     () => { let unused_in_macro = 1; };\n  \u001b[1m\u001b[94m|\u001b[0m                 \u001b[1m\u001b[96m^^^^^^^^^^^^^^^\u001b[0m\n\u001b[1m\u001b[94m...\u001b[0m\n\u001b[1m\u001b[94m6\u001b[0m \u001b[1m\u001b[94m|\u001b[0m     make!();\n  \u001b[1m\u001b[94m|\u001b[0m     \u001b[1m\u001b[94m-------\u001b[0m \u001b[1m\u001b[94min this macro invocation\u001b[0m\n  \u001b[1m\u001b[94m= \u001b[0m\u001b[1mnote\u001b[0m: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default\n  \u001b[1m\u001b[94m= \u001b[0m\u001b[1mnote\u001b[0m: this warning originates in the macro `make` (in Nightly builds, run with -Z macro-backtrace for more info)\n\n"}"#;
    const RMETA_READY: &str =
        r#"{"$message_type":"artifact","artifact":"/w/out/libmsg-abc.rmeta","emit":"metadata"}"#;
    const RLIB_READY: &str =
        r#"{"$message_type":"artifact","artifact":"/w/out/libmsg-abc.rlib","emit":"link"}"#;
    /// rustc writes the summary after the notices.
    const SUMMARY: &str = r#"{"$message_type":"diagnostic","message":"5 warnings emitted","code":null,"level":"warning","spans":[],"children":[],"rendered":"\u001b[1m\u001b[33mwarning\u001b[0m\u001b[1m: 5 warnings emitted\u001b[0m\n\n"}"#;

    /// The lines in the order rustc wrote them.
    const RUSTC_ORDER: [&str; 6] = [
        DEP_INFO_READY,
        DEPRECATED,
        IN_MACRO,
        RMETA_READY,
        RLIB_READY,
        SUMMARY,
    ];

    fn form() -> Form {
        Form::new(
            Some("json"),
            Some("diagnostic-rendered-ansi,artifacts,future-incompat"),
            None,
        )
    }

    fn to_placeholder(path: &str) -> String {
        match path.strip_prefix("/w/") {
            Some(rest) => format!("{{root}}/{rest}"),
            None => path.to_owned(),
        }
    }

    fn record(lines: &[&str]) -> Recorded {
        let mut recorder = Recorder::new(Vec::new(), form());
        for line in lines {
            recorder.line(line).unwrap();
        }
        recorder.finish()
    }

    /// A writer whose bytes the test can read while the recorder still holds it.
    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Vec<u8>>>);

    impl io::Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8(self.0.borrow().clone()).unwrap()
        }
    }

    #[test]
    fn parses_notices_diagnostics_and_text() {
        assert_eq!(
            Line::parse(RMETA_READY),
            Line::Notice {
                artifact: "/w/out/libmsg-abc.rmeta".into(),
                emit: "metadata".into()
            }
        );
        assert!(matches!(
            Line::parse(IN_MACRO),
            Line::Diagnostic(Value::Object(_))
        ));
        let other = r#"{"$message_type":"future_incompat","future_incompat_report":[]}"#;
        assert!(matches!(Line::parse(other), Line::Diagnostic(_)));
        for text in ["warning: plain text", "[1,2]", "\"string\"", "", "{broken"] {
            assert_eq!(Line::parse(text), Line::Text(text.to_owned()));
        }
    }

    // covers: B-12
    #[test]
    fn each_line_reaches_cargo_before_the_next_is_read() {
        let out = Shared::default();
        let mut recorder = Recorder::new(out.clone(), form());
        recorder.line(DEP_INFO_READY).unwrap();
        recorder.line(&format!("{RMETA_READY}\n")).unwrap();
        // Cargo may start dependents now: the notice is out before the compile ends.
        assert_eq!(out.text(), format!("{DEP_INFO_READY}\n{RMETA_READY}\n"));
        recorder.line(SUMMARY).unwrap();
        let recorded = recorder.finish();
        assert_eq!(recorded.lines().len(), 3);
        assert_eq!(recorded.form(), &form());
    }

    // covers: B-12
    #[test]
    fn a_compile_with_only_notices_serves_every_form() {
        assert!(record(&[DEP_INFO_READY, RMETA_READY, RLIB_READY]).only_notices());
        assert!(!record(&[DEP_INFO_READY, SUMMARY]).only_notices());
        assert!(!record(&[RMETA_READY, "printed by a proc macro"]).only_notices());
        let stored = record(&[RMETA_READY]).store(to_placeholder);
        assert!(stored.only_notices());
    }

    // covers: B-12
    #[test]
    fn forms_compare_by_meaning() {
        let a = Form::new(
            Some("json"),
            Some("artifacts,diagnostic-rendered-ansi"),
            None,
        );
        let b = Form::new(
            Some("json"),
            Some("diagnostic-rendered-ansi,artifacts,artifacts"),
            Some("auto"),
        );
        assert_eq!(a, b);
        assert_ne!(a, Form::new(Some("json"), Some("artifacts"), None));
        assert_ne!(
            a,
            Form::new(
                Some("json"),
                Some("artifacts,diagnostic-rendered-ansi"),
                Some("always")
            )
        );
        assert_eq!(
            Form::new(None, None, None),
            Form::new(Some("human"), Some(""), Some("auto"))
        );
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Form>(&json).unwrap(), a);
    }

    #[test]
    fn stored_lines_hold_no_path_of_this_folder() {
        let stored = record(&RUSTC_ORDER).store(to_placeholder);
        let kept = serde_json::to_string(&stored).unwrap();
        assert!(!kept.contains("/w/"), "{kept}");
        // Spans, the expansion's call site and definition, the child's spans and the rendered
        // text all carry the placeholder.
        assert_eq!(kept.matches("{root}/src/lib.rs").count(), 8, "{kept}");
        assert!(kept.contains(r"\u001b[0m{root}/src/m.rs:4:14"));
        assert!(kept.contains("{root}/out/libmsg-abc.rmeta"));
        let back: Stored = serde_json::from_str(&kept).unwrap();
        assert_eq!(back, stored);
        assert_eq!(back.form(), &form());
    }

    // covers: B-12
    #[test]
    fn replay_writes_this_folders_paths_with_the_notices_last() {
        let stored = record(&RUSTC_ORDER).store(to_placeholder);
        let mut out = Vec::new();
        stored
            .replay(|p| p.replace("{root}/", "/other/"), &mut out)
            .unwrap();
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        let expected_order = [
            DEPRECATED,
            IN_MACRO,
            SUMMARY,
            DEP_INFO_READY,
            RMETA_READY,
            RLIB_READY,
        ];
        assert_eq!(lines.len(), expected_order.len());
        for (line, original) in lines.iter().zip(expected_order) {
            let replayed: Value = serde_json::from_str(line).unwrap();
            let expected: Value =
                serde_json::from_str(&original.replace("/w/", "/other/")).unwrap();
            assert_eq!(replayed, expected);
        }
        // A notice keeps rustc's own layout.
        assert_eq!(lines[3], DEP_INFO_READY.replace("/w/", "/other/"));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn replay_into_the_same_folder_gives_rustcs_lines_back() {
        let stored = record(&RUSTC_ORDER).store(to_placeholder);
        let mut out = Vec::new();
        stored
            .replay(|p| p.replace("{root}/", "/w/"), &mut out)
            .unwrap();
        let out = String::from_utf8(out).unwrap();
        let mut replayed: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let mut original: Vec<Value> = RUSTC_ORDER
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let key = |v: &Value| v.to_string();
        replayed.sort_by_key(key);
        original.sort_by_key(key);
        assert_eq!(replayed, original);
    }

    #[test]
    fn text_lines_are_kept_as_written() {
        let stored = record(&["note: /w/printed by a macro", RMETA_READY]).store(to_placeholder);
        let mut out = Vec::new();
        stored.replay(to_placeholder, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "note: /w/printed by a macro\n{\"$message_type\":\"artifact\",\"artifact\":\"{root}/out/libmsg-abc.rmeta\",\"emit\":\"metadata\"}\n"
        );
    }

    #[test]
    fn a_longer_path_is_not_replaced_by_its_start() {
        let pairs = vec![
            ("/w/a.rs.in".to_owned(), "{x}".to_owned()),
            ("/w/a.rs".to_owned(), "/w/a.rs/b".to_owned()),
        ];
        assert_eq!(
            replace_all("--> /w/a.rs.in:1 and /w/a.rs:2 é", &pairs),
            "--> {x}:1 and /w/a.rs/b:2 é"
        );
    }
}
