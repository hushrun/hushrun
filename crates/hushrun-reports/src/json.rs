//! The JSON formats: istanbul's `coverage-summary.json`, coverage.py's JSON report and
//! SimpleCov's `.resultset.json`.

use std::collections::HashMap;

use serde_json::Value;

use crate::{Counts, Deadline, Error, Format, Ratio, Read};

/// What a JSON report says, by the keys it has.
pub(crate) fn read(bytes: &[u8], deadline: Deadline) -> Result<(Format, Read), Error> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| Error::Malformed(format!("not JSON: {e}")))?;
    deadline.check()?;
    let counts = if value
        .pointer("/total/lines/total")
        .is_some_and(Value::is_number)
    {
        (Format::Istanbul, istanbul(&value)?)
    } else if value
        .pointer("/totals/num_statements")
        .is_some_and(Value::is_number)
    {
        (Format::Coveragepy, coveragepy(&value)?)
    } else if is_simplecov(&value) {
        (Format::Simplecov, simplecov(&value, deadline)?)
    } else {
        return Err(Error::NotAReport);
    };
    Ok((counts.0, Read::Coverage(counts.1)))
}

/// Which JSON format a file's first bytes look like, by the keys they name. coverage.py writes
/// its totals after every file, so its `meta`, which comes first and names `show_contexts`,
/// tells it.
pub(crate) fn sniff(text: &str) -> Option<Format> {
    if text.contains("\"total\"") && text.contains("\"lines\"") {
        Some(Format::Istanbul)
    } else if text.contains("\"show_contexts\"")
        || (text.contains("\"totals\"") && text.contains("\"num_statements\""))
    {
        Some(Format::Coveragepy)
    } else if text.contains("\"coverage\"") {
        Some(Format::Simplecov)
    } else {
        None
    }
}

fn count(value: &Value, pointer: &str) -> Result<u64, Error> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Malformed(format!("no count at {pointer}")))
}

/// istanbul: `total.lines` and `total.branches`, each `{total, covered}`.
fn istanbul(value: &Value) -> Result<Counts, Error> {
    let lines = Ratio::some(
        count(value, "/total/lines/covered")?,
        count(value, "/total/lines/total")?,
    );
    let branches = match (
        value
            .pointer("/total/branches/covered")
            .and_then(Value::as_u64),
        value
            .pointer("/total/branches/total")
            .and_then(Value::as_u64),
    ) {
        (Some(covered), Some(total)) => Ratio::some(covered, total),
        _ => None,
    };
    Ok(Counts {
        lines,
        statements: None,
        branches,
    })
}

/// coverage.py: `totals.covered_lines` of `num_statements`, and `covered_branches` of
/// `num_branches` when it measured branches.
fn coveragepy(value: &Value) -> Result<Counts, Error> {
    let lines = Ratio::some(
        count(value, "/totals/covered_lines")?,
        count(value, "/totals/num_statements")?,
    );
    let branches = match (
        value
            .pointer("/totals/covered_branches")
            .and_then(Value::as_u64),
        value
            .pointer("/totals/num_branches")
            .and_then(Value::as_u64),
    ) {
        (Some(covered), Some(total)) => Ratio::some(covered, total),
        _ => None,
    };
    Ok(Counts {
        lines,
        statements: None,
        branches,
    })
}

/// A SimpleCov result set: an object whose entries, one per command that ran, hold `coverage`.
fn is_simplecov(value: &Value) -> bool {
    let Some(entries) = value.as_object() else {
        return false;
    };
    !entries.is_empty()
        && entries
            .values()
            .all(|entry| entry.get("coverage").is_some_and(Value::is_object))
}

/// A file's lines, whether each is covered (`None` when it isn't code), and its branches, by
/// condition and arm.
type FileCoverage<'a> = (Vec<Option<bool>>, HashMap<(&'a str, &'a str), bool>);

/// SimpleCov: each file's lines, `null` for a line that isn't code and a count for one that is,
/// and its branches when it measured them. The commands' results are merged, as SimpleCov
/// merges them: a line or branch is covered when any command covered it.
fn simplecov(value: &Value, deadline: Deadline) -> Result<Counts, Error> {
    let mut files: HashMap<&str, FileCoverage<'_>> = HashMap::new();
    for entry in value.as_object().into_iter().flat_map(|o| o.values()) {
        deadline.check()?;
        let Some(coverage) = entry.get("coverage").and_then(Value::as_object) else {
            continue;
        };
        for (path, file) in coverage {
            let (lines, branches) = files.entry(path.as_str()).or_default();
            // SimpleCov before 0.18 kept the lines alone.
            let these = match file {
                Value::Array(lines) => Some(lines),
                Value::Object(file) => file.get("lines").and_then(Value::as_array),
                _ => {
                    return Err(Error::Malformed(format!(
                        "{path}'s coverage is neither lines nor an object"
                    )));
                }
            };
            for (at, hits) in these.into_iter().flatten().enumerate() {
                if lines.len() <= at {
                    lines.resize(at + 1, None);
                }
                let covered = match hits {
                    Value::Null => continue,
                    Value::Number(n) => n.as_f64().is_some_and(|n| n > 0.0),
                    // "ignored", for lines SimpleCov was told to leave out.
                    _ => continue,
                };
                let line = &mut lines[at];
                *line = Some(line.unwrap_or(false) || covered);
            }
            let Some(conditions) = file.get("branches").and_then(Value::as_object) else {
                continue;
            };
            for (condition, arms) in conditions {
                for (arm, hits) in arms.as_object().into_iter().flatten() {
                    let covered = hits.as_f64().is_some_and(|n| n > 0.0);
                    let seen = branches
                        .entry((condition.as_str(), arm.as_str()))
                        .or_insert(false);
                    *seen |= covered;
                }
            }
        }
    }
    let lines = files.values().flat_map(|(lines, _)| lines.iter().flatten());
    let (covered, total) = lines.fold((0u64, 0u64), |(c, t), covered| {
        (c + u64::from(*covered), t + 1)
    });
    let branches = files.values().flat_map(|(_, branches)| branches.values());
    let (b_covered, b_total) = branches.fold((0u64, 0u64), |(c, t), covered| {
        (c + u64::from(*covered), t + 1)
    });
    Ok(Counts {
        lines: Ratio::some(covered, total),
        statements: None,
        branches: Ratio::some(b_covered, b_total),
    })
}
