//! \[ADR 0121\] What a result bundle says each test did, as `xcrun xcresulttool get test-results
//! tests` prints it (Xcode 16 and later): a tree of nodes, test plan, test bundle (by its
//! target's name), suites, and test cases with a result each (C-112).

use std::collections::BTreeMap;

use serde_json::Value;

/// What one test did, over every device and configuration it ran on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It passed everywhere it ran; its duration in milliseconds.
    Passed(u64),
    /// It was skipped everywhere it ran (`XCTSkip`, a disabled Swift Testing test).
    Skipped(u64),
    /// It failed somewhere, or a result says nothing hushrun knows.
    Failed(u64),
}

impl Outcome {
    fn millis(self) -> u64 {
        match self {
            Outcome::Passed(ms) | Outcome::Skipped(ms) | Outcome::Failed(ms) => ms,
        }
    }

    /// The outcome of a test seen twice: a failure anywhere is a failure, a skip passes only
    /// beside skips.
    fn and(self, other: Outcome) -> Outcome {
        let ms = self.millis().saturating_add(other.millis());
        match (self, other) {
            (Outcome::Failed(_), _) | (_, Outcome::Failed(_)) => Outcome::Failed(ms),
            (Outcome::Skipped(_), Outcome::Skipped(_)) => Outcome::Skipped(ms),
            _ => Outcome::Passed(ms),
        }
    }
}

/// Each test bundle's tests, by the bundle's target name and each test's id within it.
pub type Results = BTreeMap<String, BTreeMap<String, Outcome>>;

/// Whether a node is a test bundle: `Unit test bundle`, `UI test bundle`.
fn is_bundle(node: &Value) -> bool {
    node["nodeType"]
        .as_str()
        .is_some_and(|t| t.ends_with("test bundle"))
}

fn of_result(result: Option<&str>, ms: u64) -> Outcome {
    match result {
        Some("Passed") | Some("Expected Failure") => Outcome::Passed(ms),
        Some("Skipped") => Outcome::Skipped(ms),
        _ => Outcome::Failed(ms),
    }
}

/// A test case's outcome: its own result, and every device's or configuration's below it.
fn case_outcome(node: &Value) -> Outcome {
    let ms = node["durationInSeconds"]
        .as_f64()
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map_or(0, |s| (s * 1000.0).round() as u64);
    let mut outcome = of_result(node["result"].as_str(), ms);
    for child in node["children"].as_array().into_iter().flatten() {
        if matches!(
            child["nodeType"].as_str(),
            Some("Device" | "Test Plan Configuration" | "Repetition")
        ) && child.get("result").is_some()
        {
            outcome = outcome.and(of_result(child["result"].as_str(), 0));
        }
    }
    outcome
}

/// The test cases below `node`, with the names of the suites above them.
fn cases(node: &Value, suites: &mut Vec<String>, out: &mut BTreeMap<String, Outcome>) {
    for child in node["children"].as_array().into_iter().flatten() {
        match child["nodeType"].as_str() {
            Some("Test Case") => {
                let name = child["name"].as_str().unwrap_or_default();
                let id = child["nodeIdentifier"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        let mut parts = suites.clone();
                        parts.push(name.to_string());
                        parts.join("/")
                    });
                if id.is_empty() {
                    continue;
                }
                let outcome = case_outcome(child);
                out.entry(id)
                    .and_modify(|o| *o = o.and(outcome))
                    .or_insert(outcome);
            }
            Some("Test Suite") => {
                suites.push(child["name"].as_str().unwrap_or_default().to_string());
                cases(child, suites, out);
                suites.pop();
            }
            _ => {}
        }
    }
}

fn bundles(node: &Value, out: &mut Results) {
    if is_bundle(node) {
        let Some(name) = node["name"].as_str() else {
            return;
        };
        let tests = out.entry(name.to_string()).or_default();
        let mut found = BTreeMap::new();
        cases(node, &mut Vec::new(), &mut found);
        for (id, outcome) in found {
            tests
                .entry(id)
                .and_modify(|o| *o = o.and(outcome))
                .or_insert(outcome);
        }
        return;
    }
    for child in node["children"].as_array().into_iter().flatten() {
        bundles(child, out);
    }
}

/// Reads `xcresulttool get test-results tests`'s JSON; `None` when it isn't that.
pub fn read(bytes: &[u8]) -> Option<Results> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let nodes = value.get("testNodes")?.as_array()?;
    let mut out = Results::new();
    for node in nodes {
        bundles(node, &mut out);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TESTS: &str = include_str!("../../../../../testdata/xcodebuild/test-results.json");

    // covers: C-112
    #[test]
    fn each_test_case_is_read_with_its_bundle_suite_and_result() {
        let r = read(TESTS.as_bytes()).unwrap();
        let lib = &r["LibTests"];
        assert_eq!(lib["LibTests/testLib()"], Outcome::Passed(1));
        assert!(matches!(lib["LibTests/testOther()"], Outcome::Failed(_)));
        assert!(matches!(lib["LibSuite/swiftTesting()"], Outcome::Passed(_)));
        assert!(matches!(lib["LibTests/testSkipped()"], Outcome::Skipped(_)));
    }

    // covers: C-112
    #[test]
    fn a_failure_on_one_device_fails_the_test() {
        let json = serde_json::json!({"testNodes": [{"nodeType": "Test Plan", "name": "P", "children": [
            {"nodeType": "Unit test bundle", "name": "T", "children": [
                {"nodeType": "Test Suite", "name": "S", "children": [
                    {"nodeType": "Test Case", "name": "a()", "nodeIdentifier": "S/a()", "result": "Passed",
                     "children": [
                        {"nodeType": "Device", "name": "iPhone", "result": "Passed"},
                        {"nodeType": "Device", "name": "iPad", "result": "Failed"}]},
                    {"nodeType": "Test Case", "name": "b()", "result": "Weird"}]}]}]}]});
        let r = read(json.to_string().as_bytes()).unwrap();
        assert!(matches!(r["T"]["S/a()"], Outcome::Failed(_)));
        // No id given: the suites above it and its name. A result hushrun doesn't know fails.
        assert!(matches!(r["T"]["S/b()"], Outcome::Failed(_)));
    }

    // covers: C-112
    #[test]
    fn what_isnt_a_result_tree_reads_as_none() {
        assert!(read(b"not json").is_none());
        assert!(read(b"{}").is_none());
        assert_eq!(read(br#"{"testNodes": []}"#), Some(Results::new()));
    }
}
