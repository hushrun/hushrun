//! Unified diffs of the files init would write, for its plan as JSON (C-15): `git apply` and
//! `patch -p1` take them as they are. Lines are compared whole, line endings included, so a file
//! with `\r\n` keeps them, and a last line without a newline says so.

/// The unified diff that turns `before` into `after`, with three lines of context; `None` for a
/// new file. Empty when they are the same.
pub fn unified(path: &str, before: Option<&str>, after: &str) -> String {
    let old: Vec<&str> = before.map(lines).unwrap_or_default();
    let new: Vec<&str> = lines(after);
    if old == new {
        return String::new();
    }
    let ops = script(&old, &new);
    let mut out = match before {
        Some(_) => format!("--- a/{path}\n+++ b/{path}\n"),
        None => format!("--- /dev/null\n+++ b/{path}\n"),
    };
    for hunk in hunks(&ops, CONTEXT) {
        let (old_start, old_len, new_start, new_len) = hunk.ranges(&ops);
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            range(old_start, old_len),
            range(new_start, new_len)
        ));
        for op in &ops[hunk.from..hunk.to] {
            let (mark, line) = match *op {
                Op::Same(i, _) => (' ', old[i]),
                Op::Gone(i) => ('-', old[i]),
                Op::Added(j) => ('+', new[j]),
            };
            out.push(mark);
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    out
}

const CONTEXT: usize = 3;

/// A diff gives up past this many changed lines, and replaces the whole file: init never changes
/// that much, and the search's memory grows with its square.
const MOST_CHANGES: usize = 4000;

/// `text`'s lines, each with its line ending.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// One step of the edit script: a line of both (old index, new index), one only in the old
/// file, or one only in the new.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Same(usize, usize),
    Gone(usize),
    Added(usize),
}

/// The shortest edit script from `old` to `new` (Myers, 1986), after their common start and end.
fn script(old: &[&str], new: &[&str]) -> Vec<Op> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let a = &old[prefix..old.len() - suffix];
    let b = &new[prefix..new.len() - suffix];
    let mut ops: Vec<Op> = (0..prefix).map(|i| Op::Same(i, i)).collect();
    let middle = myers(a, b).unwrap_or_else(|| {
        (0..a.len())
            .map(Op::Gone)
            .chain((0..b.len()).map(Op::Added))
            .collect()
    });
    ops.extend(middle.into_iter().map(|op| match op {
        Op::Same(i, j) => Op::Same(i + prefix, j + prefix),
        Op::Gone(i) => Op::Gone(i + prefix),
        Op::Added(j) => Op::Added(j + prefix),
    }));
    let (old_end, new_end) = (old.len() - suffix, new.len() - suffix);
    ops.extend((0..suffix).map(|n| Op::Same(old_end + n, new_end + n)));
    ops
}

/// Myers' greedy search, keeping each round's furthest points to walk back from; `None` past
/// [`MOST_CHANGES`].
fn myers(a: &[&str], b: &[&str]) -> Option<Vec<Op>> {
    let (n, m) = (a.len(), b.len());
    let max = (n + m).min(MOST_CHANGES);
    let offset = max + 1;
    let mut v = vec![0usize; 2 * max + 3];
    let mut trace: Vec<Vec<usize>> = Vec::new();
    for d in 0..=max {
        trace.push(v.clone());
        for k in (0..=2 * d).map(|i| i as isize - d as isize).step_by(2) {
            let at = (k + offset as isize) as usize;
            let down = k == -(d as isize) || (k != d as isize && v[at - 1] < v[at + 1]);
            let mut x = if down { v[at + 1] } else { v[at - 1] + 1 };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && a[x] == b[y] {
                x += 1;
                y += 1;
            }
            v[at] = x;
            if x >= n && y >= m {
                return Some(walk_back(&trace, a.len(), b.len(), d, offset));
            }
        }
    }
    None
}

/// The edit script, from the rounds' furthest points.
fn walk_back(trace: &[Vec<usize>], n: usize, m: usize, last: usize, offset: usize) -> Vec<Op> {
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=last).rev() {
        // The furthest points after round d - 1, from which round d moved one step.
        let v = &trace[d];
        let k = x as isize - y as isize;
        let at = (k + offset as isize) as usize;
        let down = k == -(d as isize) || (k != d as isize && v[at - 1] < v[at + 1]);
        let prev_k = if down { k + 1 } else { k - 1 };
        let prev_x = v[(prev_k + offset as isize) as usize];
        let prev_y = (prev_x as isize - prev_k) as usize;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(Op::Same(x, y));
        }
        if down {
            y -= 1;
            ops.push(Op::Added(y));
        } else {
            x -= 1;
            ops.push(Op::Gone(x));
        }
    }
    // Round 0 is the lines both start with.
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push(Op::Same(x, y));
    }
    ops.reverse();
    ops
}

/// A run of the edit script shown together: `ops[from..to]`.
struct Hunk {
    from: usize,
    to: usize,
}

impl Hunk {
    /// Where it starts in each file, 0-based, and how many lines it shows of each.
    fn ranges(&self, ops: &[Op]) -> (usize, usize, usize, usize) {
        let (mut old_len, mut new_len) = (0, 0);
        for op in &ops[self.from..self.to] {
            match op {
                Op::Same(..) => {
                    old_len += 1;
                    new_len += 1;
                }
                Op::Gone(_) => old_len += 1,
                Op::Added(_) => new_len += 1,
            }
        }
        // Lines of each file before the hunk.
        let (mut old_start, mut new_start) = (0, 0);
        for op in &ops[..self.from] {
            match op {
                Op::Same(..) => {
                    old_start += 1;
                    new_start += 1;
                }
                Op::Gone(_) => old_start += 1,
                Op::Added(_) => new_start += 1,
            }
        }
        (old_start, old_len, new_start, new_len)
    }
}

/// The changes, each with `context` lines around it, joined where those touch.
fn hunks(ops: &[Op], context: usize) -> Vec<Hunk> {
    let changed: Vec<usize> = (0..ops.len())
        .filter(|&i| !matches!(ops[i], Op::Same(..)))
        .collect();
    let mut hunks: Vec<Hunk> = Vec::new();
    for i in changed {
        let from = i.saturating_sub(context);
        let to = (i + 1 + context).min(ops.len());
        match hunks.last_mut() {
            Some(last) if from <= last.to => last.to = to,
            _ => hunks.push(Hunk { from, to }),
        }
    }
    hunks
}

/// A hunk's range as unified diffs write it: 1-based, with the count unless it is one, and the
/// line before it when it is empty.
fn range(start: usize, len: usize) -> String {
    match len {
        0 => format!("{start},0"),
        1 => format!("{}", start + 1),
        _ => format!("{},{len}", start + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `diff` makes of `before`, applied back: the patch's own reading, as `patch` does it.
    fn apply(before: &str, diff: &str) -> String {
        let old: Vec<&str> = lines(before);
        let mut out = String::new();
        let mut at = 0;
        let mut body = diff.lines().skip(2).peekable();
        while let Some(header) = body.next() {
            let old_range = header
                .strip_prefix("@@ -")
                .and_then(|r| r.split(' ').next())
                .expect("a hunk header");
            let (start, len) = match old_range.split_once(',') {
                Some((s, l)) => (s.parse::<usize>().unwrap(), l.parse::<usize>().unwrap()),
                None => (old_range.parse::<usize>().unwrap(), 1),
            };
            let first = if len == 0 { start } else { start - 1 };
            out.extend(old[at..first].iter().copied());
            at = first;
            while let Some(line) = body.peek().copied() {
                if line.starts_with("@@") {
                    break;
                }
                body.next();
                let no_newline = body.peek() == Some(&"\\ No newline at end of file");
                if no_newline {
                    body.next();
                }
                let text = &line[1..];
                let end = if no_newline { "" } else { "\n" };
                match line.as_bytes()[0] {
                    b' ' => {
                        at += 1;
                        out.push_str(text);
                        out.push_str(end);
                    }
                    b'-' => at += 1,
                    b'+' => {
                        out.push_str(text);
                        out.push_str(end);
                    }
                    other => panic!("unexpected {}", other as char),
                }
            }
        }
        out.extend(old[at..].iter().copied());
        out
    }

    #[test]
    fn a_diff_applies_back_to_what_it_came_from() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\n";
        for after in [
            "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\n",
            "x\na\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\n",
            "a\nb\nc\nd\nE\nf\ng\nh\ni\nj\nk\nl\n",
            "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk",
            "b\nc\nd\ne\nf\ng\nh\ni\nj\n",
            "",
            "a\nb\nc\nnew\nd\ne\nf\ng\nh\ni\nnew\nj\nk\n",
        ] {
            let diff = unified("ci.yml", Some(before), after);
            if before == after {
                assert_eq!(diff, "");
                continue;
            }
            assert!(
                diff.starts_with("--- a/ci.yml\n+++ b/ci.yml\n@@ -"),
                "{diff}"
            );
            assert_eq!(apply(before, &diff), after, "{diff}");
        }
    }

    #[test]
    fn hunks_are_unified_diffs_hunks() {
        let before = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n";
        let after = "1\n2\n3\n4\n5\nsix\n7\n8\n9\n10\n11\n12\n13\n14\n15\n";
        assert_eq!(
            unified("f", Some(before), after),
            "--- a/f\n+++ b/f\n@@ -3,7 +3,7 @@\n 3\n 4\n 5\n-6\n+six\n 7\n 8\n 9\n"
        );
        // Two changes three lines apart share a hunk; seven apart, they don't.
        let after = "one\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\nfifteen\n";
        let diff = unified("f", Some(before), after);
        assert_eq!(diff.matches("@@ -").count(), 2, "{diff}");
        let after = "one\n2\n3\n4\n5\nsix\n7\n8\n9\n10\n11\n12\n13\n14\n15\n";
        let diff = unified("f", Some(before), after);
        assert_eq!(diff.matches("@@ -").count(), 1, "{diff}");
        // An insertion at the very start says where it goes.
        let diff = unified("f", Some("a\n"), "b\na\n");
        assert_eq!(diff, "--- a/f\n+++ b/f\n@@ -1 +1,2 @@\n+b\n a\n");
    }

    #[test]
    fn a_new_file_and_its_last_line() {
        assert_eq!(
            unified("hushrun.yaml", None, "version: 1\nrepository: x\n"),
            "--- /dev/null\n+++ b/hushrun.yaml\n@@ -0,0 +1,2 @@\n+version: 1\n+repository: x\n"
        );
        let diff = unified("f", Some("a\nb"), "a\nb\n");
        assert_eq!(
            diff,
            "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n"
        );
        assert_eq!(apply("a\nb", &diff), "a\nb\n");
        // CRLF stays as it is.
        let diff = unified("f", Some("a\r\nb\r\n"), "a\r\nx\r\nb\r\n");
        assert_eq!(
            diff,
            "--- a/f\n+++ b/f\n@@ -1,2 +1,3 @@\n a\r\n+x\r\n b\r\n"
        );
    }

    #[test]
    fn a_rewrite_past_the_limit_still_applies() {
        let before: String = (0..MOST_CHANGES * 2).map(|n| format!("{n}\n")).collect();
        let after: String = (0..MOST_CHANGES * 2).map(|n| format!("x{n}\n")).collect();
        let diff = unified("big", Some(&before), &after);
        assert_eq!(apply(&before, &diff), after);
    }
}
