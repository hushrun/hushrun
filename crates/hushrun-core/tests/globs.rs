//! The glob golden vectors (`testdata/digest/globs.json`), shared with `tools/refdigest.py`.
//!
// covers: D-7, C-8

use hushrun_core::glob::{Pattern, PatternErrorKind};

fn kind_name(kind: &PatternErrorKind) -> &'static str {
    match kind {
        PatternErrorKind::Empty => "Empty",
        PatternErrorKind::EmptySegment => "EmptySegment",
        PatternErrorKind::RecursiveWithOther => "RecursiveWithOther",
        PatternErrorKind::Brace => "Brace",
        PatternErrorKind::UnclosedClass => "UnclosedClass",
        PatternErrorKind::CaretClass => "CaretClass",
        PatternErrorKind::InvalidRange(..) => "InvalidRange",
        PatternErrorKind::AmbiguousRange => "AmbiguousRange",
        PatternErrorKind::Regex(_) => "Regex",
    }
}

#[test]
fn glob_vectors() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testdata/digest/globs.json"
    );
    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut failures = Vec::new();
    let cases = data["cases"].as_array().unwrap();
    for case in cases {
        let pattern = case["pattern"].as_str().unwrap();
        let expected_error = case["error"].as_str();
        let compiled = Pattern::new(pattern);
        let error = compiled.as_ref().err().map(|e| kind_name(&e.kind));
        if error != expected_error {
            failures.push(format!(
                "{pattern:?}: error {error:?}, expected {expected_error:?}"
            ));
            continue;
        }
        let Ok(compiled) = compiled else { continue };
        for path in case["matches"].as_array().into_iter().flatten() {
            let path = path.as_str().unwrap();
            if !compiled.is_match(path) {
                failures.push(format!("{pattern:?} should match {path:?}"));
            }
        }
        for path in case["rejects"].as_array().into_iter().flatten() {
            let path = path.as_str().unwrap();
            if compiled.is_match(path) {
                failures.push(format!("{pattern:?} should not match {path:?}"));
            }
        }
    }
    assert!(cases.len() > 50);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
