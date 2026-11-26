//! \[ADR 0147\] What a run says of its failed test files and tests from what the server that
//! recorded it holds of them (C-165, C-166): the lines after the record, and the step summary's
//! section. Pure: built from the answer and the run's names, then printed once by `wrap`.

use std::collections::BTreeMap;

use hushrun_core::digest::Digest;

use crate::results::Told;
use crate::units::words::thousands;

/// \[C-165\] The most failed units the lines name one by one; the rest are counted.
pub const MOST_NAMED: usize = 5;

/// One failed unit of the run, as the run names it.
#[derive(Debug, Clone)]
pub struct Failed<'a> {
    /// A test file, or `<test file>::<test id>`.
    pub name: &'a str,
    /// The key its fail was recorded under, when it was sent with a level (C-160).
    pub key: Option<Digest>,
    /// It is a test, not a test file.
    pub test: bool,
    /// Its second run passed (C-34): the run says it may be flaky, whatever the server holds.
    pub again: bool,
}

/// What the run says: lines after `hushrun: `, and the step summary's section.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Said {
    pub lines: Vec<String>,
    pub summary: Option<String>,
}

/// \[C-165, C-166\] The lines for `failed`, from what the server `told` of their keys. A unit whose
/// second run passed gets today's line, with how often it did this added once the server holds
/// more than this run's flake; another failed unit the server holds a flake of gets a line of its
/// own. At most [`MOST_NAMED`] such lines, the most often first, then one that counts the rest.
pub fn said(check: &str, failed: &[Failed<'_>], told: &BTreeMap<Digest, Told>) -> Said {
    let history = |f: &Failed<'_>| {
        let told = told.get(f.key.as_ref()?)?;
        let enough = if f.again { 2 } else { 1 };
        (told.events >= enough).then_some(told.events)
    };
    let mut with: Vec<(&Failed<'_>, u64)> = failed
        .iter()
        .filter_map(|f| Some((f, history(f)?)))
        .collect();
    with.sort_by(|(a, x), (b, y)| y.cmp(x).then_with(|| a.name.cmp(b.name)));
    let (named, rest) = with.split_at(with.len().min(MOST_NAMED));
    let mut out = Said::default();
    for f in failed.iter().filter(|f| f.again) {
        if !named.iter().any(|(n, _)| n.name == f.name) {
            out.lines.push(format!(
                "{check}: {} failed, then passed when it ran again: it may be flaky",
                f.name
            ));
        }
    }
    for (f, n) in named {
        out.lines.push(if f.again {
            format!(
                "{check}: {} failed, then passed when it ran again: it may be flaky; that makes {} in the last 30 days",
                f.name,
                times(*n)
            )
        } else {
            format!(
                "{check}: {} failed; it has failed and passed on the same files {} in the last 30 days, so it may be flaky: run it again before you change code for it",
                f.name,
                times(*n)
            )
        });
    }
    if !rest.is_empty() {
        let k = rest.len() as u64;
        let what = match (k, rest.iter().any(|(f, _)| f.test)) {
            (1, true) => "failed test has",
            (1, false) => "failed test file has",
            (_, true) => "failed tests have",
            (_, false) => "failed test files have",
        };
        out.lines.push(format!(
            "{check}: {} more {what} done the same",
            thousands(k)
        ));
    }
    if !named.is_empty() {
        let mut summary = format!(
            "### {}: tests that may be flaky\n\n",
            super::markdown(check)
        );
        for (f, n) in named {
            summary.push_str(&format!(
                "- {}: failed and passed on the same files {} in the last 30 days\n",
                super::markdown(f.name),
                times(*n)
            ));
        }
        out.summary = Some(summary);
    }
    out
}

/// `1 time`, `2 times`, `1,000 times`.
fn times(n: u64) -> String {
    if n == 1 {
        "1 time".into()
    } else {
        format!("{} times", thousands(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> Digest {
        Digest::of(&[n])
    }

    fn told(pairs: &[(u8, u64)]) -> BTreeMap<Digest, Told> {
        pairs
            .iter()
            .map(|(n, events)| {
                (
                    key(*n),
                    Told {
                        events: *events,
                        in_one_run: 0,
                        last_at: "2026-11-25T10:00:00.000Z".into(),
                    },
                )
            })
            .collect()
    }

    fn failed(name: &str, n: u8, test: bool, again: bool) -> Failed<'_> {
        Failed {
            name,
            key: Some(key(n)),
            test,
            again,
        }
    }

    // covers: C-165
    #[test]
    fn a_unit_that_passed_when_it_ran_again_says_how_often_once_the_server_holds_more() {
        let units = [failed("tests/test_a.py::test_x", 1, true, true)];
        assert_eq!(
            said("unit", &units, &told(&[(1, 3)])).lines,
            [
                "unit: tests/test_a.py::test_x failed, then passed when it ran again: it may be flaky; that makes 3 times in the last 30 days"
            ]
        );
        // Only this run's own flake: today's line, as it was.
        for told in [told(&[(1, 1)]), BTreeMap::new()] {
            let out = said("unit", &units, &told);
            assert_eq!(
                out.lines,
                [
                    "unit: tests/test_a.py::test_x failed, then passed when it ran again: it may be flaky"
                ]
            );
            assert_eq!(out.summary, None);
        }
    }

    // covers: C-165
    #[test]
    fn a_failed_unit_with_flakes_and_no_second_run_says_to_run_it_again() {
        let units = [
            failed("test_b.py", 2, false, false),
            failed("test_c.py", 3, false, false),
        ];
        let out = said("unit", &units, &told(&[(2, 1)]));
        assert_eq!(
            out.lines,
            [
                "unit: test_b.py failed; it has failed and passed on the same files 1 time in the last 30 days, so it may be flaky: run it again before you change code for it"
            ]
        );
        assert_eq!(
            out.summary.as_deref(),
            Some(
                "### unit: tests that may be flaky\n\n- test\\_b\\.py: failed and passed on the same files 1 time in the last 30 days\n"
            )
        );
    }

    // covers: C-165, C-166
    #[test]
    fn five_are_named_the_most_often_first_and_the_rest_are_counted() {
        let names: Vec<String> = (0..8).map(|i| format!("t.py::test_{i}")).collect();
        let units: Vec<Failed<'_>> = names
            .iter()
            .enumerate()
            .map(|(i, name)| failed(name, i as u8, true, i == 7))
            .collect();
        let told = told(&(0u8..8).map(|i| (i, 10 + u64::from(i))).collect::<Vec<_>>());
        let out = said("unit", &units, &told);
        assert_eq!(out.lines.len(), 6, "{:#?}", out.lines);
        assert!(out.lines[0].starts_with("unit: t.py::test_7 failed, then passed when it ran again: it may be flaky; that makes 17 times"));
        assert!(out.lines[4].starts_with(
            "unit: t.py::test_3 failed; it has failed and passed on the same files 13 times"
        ));
        assert_eq!(out.lines[5], "unit: 3 more failed tests have done the same");
        assert_eq!(out.summary.unwrap().matches("\n- ").count(), 5);
        // The rest, test files alone, one of them.
        let files = [
            failed("a.py", 1, false, false),
            failed("b.py", 2, false, false),
            failed("c.py", 3, false, false),
            failed("d.py", 4, false, false),
            failed("e.py", 5, false, false),
            failed("f.py", 6, false, false),
        ];
        let held: BTreeMap<Digest, Told> =
            super::tests::told(&[(1, 1_000), (2, 9), (3, 9), (4, 9), (5, 9), (6, 1)]);
        let out = said("unit", &files, &held);
        assert!(out.lines[0].contains("1,000 times"), "{:#?}", out.lines);
        assert_eq!(
            out.lines[5],
            "unit: 1 more failed test file has done the same"
        );
    }

    // covers: C-165
    #[test]
    fn a_unit_the_server_holds_nothing_of_or_sent_with_no_level_says_nothing_new() {
        let units = [Failed {
            name: "x.py",
            key: None,
            test: false,
            again: false,
        }];
        assert_eq!(said("unit", &units, &told(&[(1, 5)])), Said::default());
        assert_eq!(
            said("unit", &[failed("y.py", 9, false, false)], &told(&[(1, 5)])),
            Said::default()
        );
    }
}
