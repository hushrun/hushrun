//! \[ADR 0110\] What the Python adapters, pytest's and unittest's, share: the observer both
//! drive and how both match a test file's name. Apart from either adapter, so that a test of one
//! reaches nothing of the other's file (CONTRIBUTING.md, "Which tests CI runs again").

/// The observer pytest's adapter drives, which unittest's adapter drives too.
pub const OBSERVE: &str = include_str!("hushrun_observe.py");

/// Python's `fnmatch` on a file name: `*`, `?`, `[seq]` and `[!seq]`; \[ADR 0110\] unittest's
/// discovery matches its pattern so too.
pub(super) fn fnmatch(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| go(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && go(&p[1..], &n[1..]),
            Some('[') => {
                let Some(close) = p.iter().skip(2).position(|c| *c == ']').map(|i| i + 2) else {
                    return n.first() == Some(&'[') && go(&p[1..], &n[1..]);
                };
                let Some(c) = n.first() else { return false };
                let (negate, set) = match p[1] {
                    '!' => (true, &p[2..close]),
                    _ => (false, &p[1..close]),
                };
                let mut hit = false;
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == '-' {
                        hit |= set[i] <= *c && *c <= set[i + 2];
                        i += 3;
                    } else {
                        hit |= set[i] == *c;
                        i += 1;
                    }
                }
                hit != negate && go(&p[close + 1..], &n[1..])
            }
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}
