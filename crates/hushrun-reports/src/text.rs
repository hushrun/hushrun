//! The text formats: LCOV, and Go's coverprofile.

use std::collections::HashMap;

use crate::{Counts, Deadline, Error, Format, Ratio, Read};

/// How many lines go by between looks at the clock.
const LINES_PER_LOOK: usize = 4096;

/// What a text report says: Go's when its first line is `mode: set`, `count` or `atomic`;
/// LCOV's when it starts with `TN:` or `SF:`.
pub(crate) fn read(text: &str, deadline: Deadline) -> Result<(Format, Read), Error> {
    match sniff(text) {
        Some(Format::Go) => Ok((Format::Go, Read::Coverage(go(text, deadline)?))),
        Some(Format::Lcov) => Ok((Format::Lcov, Read::Coverage(lcov(text, deadline)?))),
        _ => Err(Error::NotAReport),
    }
}

pub(crate) fn sniff(text: &str) -> Option<Format> {
    let first = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    if matches!(first, "mode: set" | "mode: count" | "mode: atomic") {
        Some(Format::Go)
    } else if first.starts_with("TN:") || first.starts_with("SF:") {
        Some(Format::Lcov)
    } else {
        None
    }
}

fn whole(text: &str, what: &str) -> Result<u64, Error> {
    text.trim()
        .parse()
        .map_err(|_| Error::Malformed(format!("{what} isn't a count: {text:?}")))
}

/// LCOV, record by record (`SF:` to `end_of_record`): `LF`/`LH` and `BRF`/`BRH` when a record
/// has them, else its `DA` and `BRDA` lines. A line is covered when its count is above 0, a
/// branch when it was taken.
fn lcov(text: &str, deadline: Deadline) -> Result<Counts, Error> {
    #[derive(Default)]
    struct Record {
        lf: Option<u64>,
        lh: Option<u64>,
        brf: Option<u64>,
        brh: Option<u64>,
        da: HashMap<u64, u64>,
        brda: HashMap<(u64, String, String), bool>,
    }
    let mut counts = Counts::default();
    let mut open: Option<Record> = None;
    let close = |counts: &mut Counts, record: Record| {
        let lines = match (record.lf, record.lh) {
            (Some(total), Some(covered)) => Ratio::some(covered, total),
            _ => Ratio::some(
                record.da.values().filter(|hits| **hits > 0).count() as u64,
                record.da.len() as u64,
            ),
        };
        let branches = match (record.brf, record.brh) {
            (Some(total), Some(covered)) => Ratio::some(covered, total),
            _ => Ratio::some(
                record.brda.values().filter(|taken| **taken).count() as u64,
                record.brda.len() as u64,
            ),
        };
        counts.lines = Ratio::add(counts.lines, lines);
        counts.branches = Ratio::add(counts.branches, branches);
    };
    for (n, line) in text.lines().enumerate() {
        if n % LINES_PER_LOOK == LINES_PER_LOOK - 1 {
            deadline.check()?;
        }
        let line = line.trim();
        if line == "end_of_record" {
            if let Some(record) = open.take() {
                close(&mut counts, record);
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key == "SF" {
            if let Some(record) = open.replace(Record::default()) {
                // A record that didn't end counts as it stands.
                close(&mut counts, record);
            }
            continue;
        }
        let Some(record) = open.as_mut() else {
            continue;
        };
        match key {
            "LF" => record.lf = Some(whole(value, "LF")?),
            "LH" => record.lh = Some(whole(value, "LH")?),
            "BRF" => record.brf = Some(whole(value, "BRF")?),
            "BRH" => record.brh = Some(whole(value, "BRH")?),
            "DA" => {
                let mut parts = value.split(',');
                let at = whole(parts.next().unwrap_or_default(), "a DA line")?;
                let hits = parts.next().map_or(Ok(0), |hits| {
                    // Some writers give a negative or fractional count: covered is above 0.
                    hits.trim()
                        .parse::<f64>()
                        .map(|hits| u64::from(hits > 0.0))
                        .map_err(|_| {
                            Error::Malformed(format!("DA's count isn't a number: {hits:?}"))
                        })
                })?;
                let seen = record.da.entry(at).or_insert(0);
                *seen = (*seen).max(hits);
            }
            "BRDA" => {
                let parts: Vec<&str> = value.split(',').map(str::trim).collect();
                let [at, block, branch, taken] = parts[..] else {
                    return Err(Error::Malformed(format!(
                        "a BRDA line needs four parts: {value:?}"
                    )));
                };
                let at = whole(at, "a BRDA line")?;
                let taken = taken != "-" && taken.parse::<f64>().is_ok_and(|t| t > 0.0);
                let seen = record
                    .brda
                    .entry((at, block.to_string(), branch.to_string()))
                    .or_insert(false);
                *seen |= taken;
            }
            _ => {}
        }
    }
    if let Some(record) = open.take() {
        close(&mut counts, record);
    }
    Ok(counts)
}

/// Go's coverprofile: each block `file:line.col,line.col statements count` once, with the highest
/// count it is given, since `-coverpkg` lists a block once per package that ran it. Statements,
/// not lines, are counted.
fn go(text: &str, deadline: Deadline) -> Result<Counts, Error> {
    let mut blocks: HashMap<&str, (u64, u64)> = HashMap::new();
    for (n, line) in text.lines().enumerate() {
        if n % LINES_PER_LOOK == LINES_PER_LOOK - 1 {
            deadline.check()?;
        }
        let line = line.trim();
        // Profiles joined end to end repeat the mode.
        if line.is_empty() || line.starts_with("mode:") {
            continue;
        }
        let mut parts = line.rsplitn(3, ' ');
        let (Some(count), Some(statements), Some(block)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(Error::Malformed(format!(
                "not a coverprofile block: {line:?}"
            )));
        };
        if !block.contains(':') || !block.contains(',') {
            return Err(Error::Malformed(format!(
                "not a coverprofile block: {line:?}"
            )));
        }
        let statements = whole(statements, "a block's statements")?;
        let count = whole(count, "a block's count")?;
        let seen = blocks.entry(block).or_insert((statements, 0));
        seen.0 = seen.0.max(statements);
        seen.1 = seen.1.max(count);
    }
    let total: u64 = blocks.values().map(|(statements, _)| statements).sum();
    let covered: u64 = blocks
        .values()
        .filter(|(_, count)| *count > 0)
        .map(|(statements, _)| statements)
        .sum();
    Ok(Counts {
        lines: None,
        statements: Ratio::some(covered, total),
        branches: None,
    })
}
