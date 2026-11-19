//! Depfiles in Make's syntax, as GCC and clang write them (B-55, B-57): read to learn what a
//! compile read, and written for a reuse with this checkout's paths.

use super::args::{DepAsked, DepMode, DepTarget};

/// The prerequisites of the first rule of a depfile, unescaped, in order, each once. The targets
/// and any later rule (`-MP`'s phony targets) are left out. `None` when the text has no rule.
pub fn reads(text: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut past_target = false;
    let mut chars = text.chars().peekable();
    // Pushes the word read so far: a target until the first unescaped `:` that ends a word.
    let end_word = |cur: &mut String, past_target: &mut bool, out: &mut Vec<String>| {
        if cur.is_empty() {
            return;
        }
        let word = std::mem::take(cur);
        if *past_target {
            if !out.contains(&word) {
                out.push(word);
            }
        } else if word.ends_with(':') {
            *past_target = true;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek().copied() {
                Some('\n') => {
                    chars.next();
                    end_word(&mut cur, &mut past_target, &mut out);
                }
                Some('\r') => {
                    chars.next();
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    end_word(&mut cur, &mut past_target, &mut out);
                }
                Some(' ') | Some('#') | Some('\\') => {
                    cur.push(chars.next().unwrap_or(' '));
                }
                _ => cur.push('\\'),
            },
            '$' if chars.peek() == Some(&'$') => {
                chars.next();
                cur.push('$');
            }
            ':' if !past_target
                && matches!(chars.peek(), Some(' ' | '\t' | '\n' | '\r') | None) =>
            {
                cur.push(':');
                end_word(&mut cur, &mut past_target, &mut out);
            }
            ' ' | '\t' => end_word(&mut cur, &mut past_target, &mut out),
            '\n' | '\r' => {
                end_word(&mut cur, &mut past_target, &mut out);
                if past_target {
                    // The first rule ends at its first unescaped line break.
                    return Some(out);
                }
            }
            _ => cur.push(c),
        }
    }
    end_word(&mut cur, &mut past_target, &mut out);
    past_target.then_some(out)
}

/// A path as Make reads it: spaces and `#` escaped with a backslash, `$` doubled, as GCC and
/// clang write prerequisites.
pub fn escape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            ' ' => out.push_str("\\ "),
            '#' => out.push_str("\\#"),
            '$' => out.push_str("$$"),
            _ => out.push(c),
        }
    }
    out
}

/// One read of a depfile to write: its path as the compiler would write it, and whether it lies
/// in a system folder, which `-MMD` leaves out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    /// The path, as the compiler wrote it.
    pub path: String,
    /// Whether it was found in a system folder.
    pub system: bool,
}

/// The depfile a compile would have written for `asked`, `output` and `reads` (the source
/// first), in clang's layout: the targets `-MT` and `-MQ` named (else the output), each read on
/// a line of its own, and with `-MP` an empty rule per read after the source (B-57).
pub fn render(asked: &DepAsked, output: &str, reads: &[Read]) -> String {
    let targets: Vec<String> = if asked.targets.is_empty() {
        vec![escape(output)]
    } else {
        asked
            .targets
            .iter()
            .map(|t| match t {
                DepTarget::Plain(t) => t.clone(),
                DepTarget::Quoted(t) => escape(t),
            })
            .collect()
    };
    let kept: Vec<&Read> = reads
        .iter()
        .enumerate()
        .filter(|(i, r)| *i == 0 || asked.mode != DepMode::User || !r.system)
        .map(|(_, r)| r)
        .collect();
    let mut text = format!("{}:", targets.join(" "));
    for read in &kept {
        text.push_str(" \\\n  ");
        text.push_str(&escape(&read.path));
    }
    text.push('\n');
    if asked.phony {
        for read in kept.iter().skip(1) {
            text.push('\n');
            text.push_str(&escape(&read.path));
            text.push_str(":\n");
        }
    }
    text
}

/// Leaves out of a depfile the compiler wrote with `-MD` the reads in system folders, as `-MMD`
/// would have: the rule rewritten, any later rule kept as written (B-57).
pub fn without_system(
    text: &str,
    asked: &DepAsked,
    output: &str,
    system: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let all = reads(text)?;
    let reads: Vec<Read> = all
        .iter()
        .map(|p| Read {
            path: p.clone(),
            system: system(p),
        })
        .collect();
    Some(render(asked, output, &reads))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(mode: DepMode, targets: Vec<DepTarget>, phony: bool) -> DepAsked {
        DepAsked {
            mode,
            file: None,
            targets,
            phony,
        }
    }

    // covers: B-55
    #[test]
    fn a_clang_depfile_is_read_with_escapes_and_without_phony_rules() {
        let text = "obj/a.o: \\\n  /r/a\\ b.cc inc/x.h \\\n  /sdk/usr/include/c\\#.h $$HOME.h\n\ninc/x.h:\n";
        assert_eq!(
            reads(text).unwrap(),
            ["/r/a b.cc", "inc/x.h", "/sdk/usr/include/c#.h", "$HOME.h"]
        );
        // GCC's layout: several on a line.
        assert_eq!(
            reads("a.o: a.c /usr/include/stdio.h \\\n /usr/include/x.h\n").unwrap(),
            ["a.c", "/usr/include/stdio.h", "/usr/include/x.h"]
        );
        assert!(reads("").is_none());
    }

    // covers: B-57
    #[test]
    fn a_depfile_is_written_as_the_compiler_would_and_reads_back() {
        let reads_in = vec![
            Read {
                path: "/r/a b.cc".into(),
                system: false,
            },
            Read {
                path: "inc/x.h".into(),
                system: false,
            },
            Read {
                path: "/sdk/usr/include/stdio.h".into(),
                system: true,
            },
        ];
        let text = render(
            &asked(
                DepMode::All,
                vec![DepTarget::Plain("obj/a.cc.o".into())],
                false,
            ),
            "obj/a.cc.o",
            &reads_in,
        );
        assert_eq!(
            text,
            "obj/a.cc.o: \\\n  /r/a\\ b.cc \\\n  inc/x.h \\\n  /sdk/usr/include/stdio.h\n"
        );
        assert_eq!(
            reads(&text).unwrap(),
            ["/r/a b.cc", "inc/x.h", "/sdk/usr/include/stdio.h"]
        );
        // -MMD leaves out system folders; -MP adds a rule per header; -MQ quotes.
        let text = render(
            &asked(
                DepMode::User,
                vec![DepTarget::Quoted("o $x.o".into())],
                true,
            ),
            "ignored.o",
            &reads_in,
        );
        assert_eq!(
            text,
            "o\\ $$x.o: \\\n  /r/a\\ b.cc \\\n  inc/x.h\n\ninc/x.h:\n"
        );
        // No target named: the output.
        assert!(render(&asked(DepMode::All, vec![], false), "a.o", &reads_in).starts_with("a.o:"));
    }
}
