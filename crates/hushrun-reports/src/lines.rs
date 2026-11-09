//! How a summary reads (C-27): `1,204 tests, 3 failed, 14 skipped, 87.3% of lines covered`.

use crate::{Ratio, Reports};

/// `n` with its thousands grouped by commas: `1,204`.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `ratio` as a percentage with one decimal, rounded half up, but never `100.0%` for less than
/// all, nor `0.0%` for more than none.
pub fn percent(ratio: Ratio) -> String {
    if ratio.total == 0 {
        return "0.0%".to_string();
    }
    let covered = u128::from(ratio.covered.min(ratio.total));
    let total = u128::from(ratio.total);
    let mut tenths = (covered * 2000 + total) / (2 * total);
    if covered < total {
        tenths = tenths.min(999);
    }
    if covered > 0 {
        tenths = tenths.max(1);
    }
    format!("{}.{}%", tenths / 10, tenths % 10)
}

impl Reports {
    /// The summary as a run's line says it: the tests, then the coverage, joined by `, `.
    /// `None` when there is neither.
    pub fn summary(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(tests) = &self.tests {
            parts.push(match tests.total {
                1 => "1 test".to_string(),
                n => format!("{} tests", count(n)),
            });
            let failed = tests.failed.saturating_add(tests.errors);
            if failed > 0 {
                parts.push(format!("{} failed", count(failed)));
            }
            if tests.skipped > 0 {
                parts.push(format!("{} skipped", count(tests.skipped)));
            }
        }
        if let Some(coverage) = &self.coverage {
            if let Some(lines) = coverage.lines {
                parts.push(format!("{} of lines covered", percent(lines)));
            } else if let Some(statements) = coverage.statements {
                parts.push(format!("{} of statements covered", percent(statements)));
            }
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}
