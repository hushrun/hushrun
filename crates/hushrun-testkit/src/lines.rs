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

#[cfg(test)]
mod tests {
    use super::*;

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
