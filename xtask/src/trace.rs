//! `xtask trace`: traceability from the normative rules in `docs/spec/` to the tests that
//! cover them.
//!
//! A rule is a bold run in a spec file that starts with an ID such as `D-3` or `P-11`. A test
//! covers a rule by naming it after a `covers:` marker, in a comment or in a data file: a Rust
//! comment reading "covers" + ": D-3, D-4", `covers: [P-11]` in YAML, or `"covers": ["D-7"]`
//! in JSON.
//!
//! Each rule belongs to a milestone: the `[Mn]` tag in its title if it has one, else the
//! milestone of its family (`base_milestone`). `docs/validation/milestone` names the current
//! milestone, and every rule of a started milestone must be covered.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const MARKERS: &[&str] = &["covers:", "\"covers\":"];
const SCANNED_DIRS: &[&str] = &["crates", "tools", "testdata", "fuzz"];
const SCANNED_EXTENSIONS: &[&str] = &["rs", "py", "yaml", "yml", "json", "sh"];
/// Order in which rule families appear in the spec, for the report.
const FAMILIES: &[char] = &['D', 'E', 'L', 'C', 'T', 'O', 'P', 'I', 'A'];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RuleId {
    family: char,
    number: u32,
}

impl RuleId {
    fn sort_key(self) -> (usize, u32) {
        let family = FAMILIES
            .iter()
            .position(|f| *f == self.family)
            .unwrap_or(FAMILIES.len());
        (family, self.number)
    }
}

impl Ord for RuleId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl PartialOrd for RuleId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.family, self.number)
    }
}

#[derive(Debug)]
struct Rule {
    title: String,
    file: String,
    milestone: u32,
}

/// Where a test names a rule.
#[derive(Debug, Clone)]
struct Reference {
    file: String,
    line: usize,
}

pub fn run(root: &Path, args: &[String]) -> Result<bool> {
    let mut milestone = None;
    let mut verbose = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--milestone" => {
                let value = args
                    .next()
                    .context("--milestone needs a value such as M3")?;
                milestone = Some(parse_milestone(value)?);
            }
            "--verbose" | "-v" => verbose = true,
            other => bail!("unexpected argument {other:?}"),
        }
    }
    let current = match milestone {
        Some(m) => m,
        None => {
            let path = root.join("docs/validation/milestone");
            let text =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            parse_milestone(text.trim())?
        }
    };

    let rules = read_rules(&root.join("docs/spec"))?;
    let references = read_references(root)?;

    let mut missing = Vec::new();
    let mut pending = 0;
    let mut covered = 0;
    for (id, rule) in &rules {
        let refs = references.get(id);
        let required = rule.milestone <= current;
        match (required, refs) {
            (true, Some(_)) => covered += 1,
            (true, None) => missing.push(*id),
            (false, _) => pending += 1,
        }
        if verbose {
            let status = match (required, refs) {
                (true, Some(_)) => "covered",
                (true, None) => "MISSING",
                (false, Some(_)) => "pending (covered)",
                (false, None) => "pending",
            };
            let files: BTreeSet<&str> = refs
                .map(|r| r.iter().map(|r| r.file.as_str()).collect())
                .unwrap_or_default();
            let files: Vec<&str> = files.into_iter().collect();
            println!(
                "M{:<2} {status:<18} {}  {}",
                rule.milestone,
                rule.title,
                files.join(", ")
            );
        }
    }

    let unknown: Vec<(RuleId, &Reference)> = references
        .iter()
        .filter(|(id, _)| !rules.contains_key(id))
        .flat_map(|(id, refs)| refs.iter().map(move |r| (*id, r)))
        .collect();

    println!(
        "traceability at M{current}: {} rules, {} required, {covered} covered, {} missing, {pending} pending",
        rules.len(),
        covered + missing.len(),
        missing.len(),
    );
    for id in &missing {
        let rule = &rules[id];
        println!("  missing: {} ({})", rule.title, rule.file);
    }
    for (id, reference) in &unknown {
        println!(
            "  unknown rule {id} referenced at {}:{}",
            reference.file, reference.line
        );
    }
    Ok(missing.is_empty() && unknown.is_empty())
}

fn parse_milestone(text: &str) -> Result<u32> {
    text.strip_prefix('M')
        .and_then(|n| n.parse().ok())
        .with_context(|| format!("{text:?} is not a milestone such as M3"))
}

/// The milestone a rule belongs to when its title carries no `[Mn]` tag: the milestone that
/// first needs it (plan §16).
fn base_milestone(id: RuleId) -> Option<u32> {
    Some(match (id.family, id.number) {
        ('C', 4) | ('D', _) => 1,
        ('L', 4 | 6 | 7 | 9) => 4,
        ('L', 8) => 3,
        ('E' | 'L' | 'C', _) => 2,
        ('T', 5) => 8,
        ('T', _) => 2,
        ('P' | 'I', _) => 3,
        ('A', 7) => 6,
        ('A', _) => 5,
        ('O', _) => 13,
        _ => return None,
    })
}

fn read_rules(dir: &Path) -> Result<BTreeMap<RuleId, Rule>> {
    let mut rules = BTreeMap::new();
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    files.sort();
    for path in files {
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let file = relative(dir.parent().and_then(Path::parent).unwrap_or(dir), &path);
        for line in text.lines() {
            let mut rest = line;
            while let Some(start) = rest.find("**") {
                let after = &rest[start + 2..];
                let Some(end) = after.find("**") else { break };
                let bold = &after[..end];
                rest = &after[end + 2..];
                let Some((id, _)) = leading_rule_id(bold) else {
                    continue;
                };
                let milestone = match tag_milestone(bold) {
                    Some(m) => m,
                    None => base_milestone(id)
                        .with_context(|| format!("rule {id} has no known milestone"))?,
                };
                let title = bold.trim_end_matches('.').to_string();
                if let Some(previous) = rules.insert(
                    id,
                    Rule {
                        title,
                        file: file.clone(),
                        milestone,
                    },
                ) {
                    bail!("rule {id} is defined twice ({} and {file})", previous.file);
                }
            }
        }
    }
    if rules.is_empty() {
        bail!("no rules found under {}", dir.display());
    }
    Ok(rules)
}

/// A rule ID at the very start of `text`, such as `P-11` in `P-11 Upstream`.
fn leading_rule_id(text: &str) -> Option<(RuleId, usize)> {
    let bytes = text.as_bytes();
    let family = *bytes.first()?;
    if !family.is_ascii_uppercase() || bytes.get(1) != Some(&b'-') {
        return None;
    }
    let digits = bytes[2..].iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let end = 2 + digits;
    if bytes.get(end).is_some_and(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let number = text[2..end].parse().ok()?;
    Some((
        RuleId {
            family: family as char,
            number,
        },
        end,
    ))
}

/// The `n` of an `[Mn]` tag in a rule title.
fn tag_milestone(title: &str) -> Option<u32> {
    let start = title.find("[M")?;
    let rest = &title[start + 2..];
    let end = rest.find(']')?;
    rest[..end].parse().ok()
}

fn read_references(root: &Path) -> Result<BTreeMap<RuleId, Vec<Reference>>> {
    let mut references: BTreeMap<RuleId, Vec<Reference>> = BTreeMap::new();
    for dir in SCANNED_DIRS {
        let dir = root.join(dir);
        if dir.is_dir() {
            scan_dir(root, &dir, &mut references)?;
        }
    }
    Ok(references)
}

fn scan_dir(root: &Path, dir: &Path, out: &mut BTreeMap<RuleId, Vec<Reference>>) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if path.is_dir() {
            scan_dir(root, &path, out)?;
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !SCANNED_EXTENSIONS.contains(&ext) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let file = relative(root, &path);
        for (index, line) in text.lines().enumerate() {
            let Some((pos, marker)) = MARKERS
                .iter()
                .find_map(|m| line.find(m).map(|pos| (pos, *m)))
            else {
                continue;
            };
            for id in rule_ids_in(&line[pos + marker.len()..]) {
                out.entry(id).or_default().push(Reference {
                    file: file.clone(),
                    line: index + 1,
                });
            }
        }
    }
    Ok(())
}

/// Every rule ID in `text`, such as both IDs in `[P-11, P-7]`.
fn rule_ids_in(text: &str) -> Vec<RuleId> {
    let mut ids = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if boundary && let Some((id, len)) = leading_rule_id(&text[i..]) {
            ids.push(id);
            i += len;
            continue;
        }
        i += 1;
    }
    ids
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_ids_after_marker() {
        let ids: Vec<String> = rule_ids_in(" [P-11, P-7] and D-3; X-a, AB-1")
            .into_iter()
            .map(|id| id.to_string())
            .collect();
        assert_eq!(ids, ["P-11", "P-7", "D-3"]);
    }

    #[test]
    fn reads_tags_from_titles() {
        assert_eq!(tag_milestone("D-12 exec mode [M11]."), Some(11));
        assert_eq!(tag_milestone("P-11 Upstream"), None);
        assert!(leading_rule_id("C-1").is_some());
        assert!(leading_rule_id("Rationale").is_none());
    }
}
