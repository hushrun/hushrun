//! What a run's lines say about reuse, read one way by every suite.
//!
//! Two lines say how long reuse saved, which comes from how long the reused test files took when
//! they last ran: on a busy machine that passes a second, and the line takes its other form. A
//! test checks the counts, never which form a run happened to take.

/// Whether `stderr` says, in the line a run ends with, that `n` of `what` (`test files`, `tests`)
/// were reused: `; <n> <what> reused` when reuse saved less than a second, `; reusing <n> <what>
/// saved about <time>` from a second on.
pub fn says_reused(stderr: &str, n: u64, what: &str) -> bool {
    stderr.contains(&format!("; {n} {what} reused"))
        || stderr.contains(&format!("; reusing {n} {what} saved about "))
}

/// `observe`'s line (`with `tests: reuse`, …`) without the `, saving about <time>` it gains when
/// reuse would have saved a second or more: what is left are the counts, to compare whole.
pub fn without_saving(line: &str) -> String {
    const SAVING: &str = ", saving about ";
    match line.find(SAVING) {
        Some(at) => {
            let rest = &line[at + SAVING.len()..];
            let end = rest.find(';').unwrap_or(rest.len());
            format!("{}{}", &line[..at], &rest[end..])
        }
        None => line.to_string(),
    }
}

/// A plan's line with what the audit's draw can move as `_`, the rest as said. With tests as the
/// unit, the audit runs 2% of the tests a plan could leave out, a count the draw doesn't move;
/// which test files they sit in, it does. So with tests counted (`<x> of <n> tests run, in <f> of
/// …`), `<f>` is masked; with test files counted (`<f> of <n> <units> run; <r> already passed …,
/// and so did <k> tests of the others`), `<f>`, `<r>` and `<k>`, since a test the draw lands on in
/// a test file reused whole leaves that file out test by test.
pub fn drawn(line: &str) -> String {
    let all: Vec<&str> = line.split(' ').collect();
    let tests_counted = line.contains(" tests run, in ") || line.contains(" test run, in ");
    let at = |i: usize| all.get(i).copied();
    all.iter()
        .enumerate()
        .map(|(i, word)| {
            let before = i.checked_sub(1).and_then(at);
            let next = at(i + 1);
            let moves = if tests_counted {
                // `in <f> of`
                before == Some("in") && next == Some("of")
            } else {
                (before.is_some_and(|w| w.ends_with(':')) && next == Some("of"))
                    || next == Some("already")
                    || (before == Some("did") && i.checked_sub(2).and_then(at) == Some("so"))
            };
            if moves && word.chars().all(|c| c.is_ascii_digit() || c == ',') {
                "_"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_an_audits_draw_can_move_is_masked() {
        assert_eq!(
            drawn(
                "hushrun: unit: 3 of 13 tests run, in 2 of 5 test files; 10 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
            ),
            "hushrun: unit: 3 of 13 tests run, in _ of 5 test files; 10 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        );
        assert_eq!(
            drawn(
                "hushrun: unit: 2 of 5 test classes run; 3 already passed on the files they read, and so did 2 tests of the others, and 1 more test that already passed runs anyway, as a check"
            ),
            "hushrun: unit: _ of 5 test classes run; _ already passed on the files they read, and so did _ tests of the others, and 1 more test that already passed runs anyway, as a check"
        );
    }

    #[test]
    fn either_form_of_what_reuse_saved_says_the_same_count() {
        let short =
            "hushrun: unit passed in 0.5s; 4 test files reused; it won't run again on this code";
        let long = "hushrun: unit passed in 3s; reusing 4 test files saved about 1.2s; it won't run again on this code";
        for line in [short, long] {
            assert!(says_reused(line, 4, "test files"), "{line}");
            assert!(!says_reused(line, 3, "test files"), "{line}");
            assert!(!says_reused(line, 4, "tests"), "{line}");
        }
        let observed = "hushrun: unit: with `tests: reuse`, 1 of 3 test files would have run; 2 already passed on the files they read";
        assert_eq!(without_saving(observed), observed);
        assert_eq!(
            without_saving(&format!("{observed}, saving about 1s")),
            observed
        );
        assert_eq!(
            without_saving(&format!(
                "{observed}, saving about 2m 3s; hushrun couldn't follow all that 1 of them read"
            )),
            format!("{observed}; hushrun couldn't follow all that 1 of them read")
        );
    }
}
