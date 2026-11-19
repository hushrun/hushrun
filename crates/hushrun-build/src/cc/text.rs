//! What a read file says about the compile beyond its bytes (B-54): the names it asks
//! `__has_include` about, whether it uses a macro that holds the time, and whether it reads a
//! file no depfile names (an assembler's `.incbin` or `.include`, `__has_embed`).

use memchr::memmem;

/// What one read file holds that the second key cares about.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Seen {
    /// Each literal name passed to `__has_include` or `__has_include_next`, in order, once each:
    /// `"x.h"` as `"x.h"`, `<x.h>` as `<x.h>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<String>,
    /// A probe whose argument is not a literal (`computed-probe`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub computed: bool,
    /// `__TIME__` or `__DATE__` appears.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub time: bool,
    /// `__TIMESTAMP__` appears: a time no `SOURCE_DATE_EPOCH` fixes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timestamp: bool,
    /// An assembler directive that reads a file (`.incbin`, `.include "…"`), which no compiler's
    /// depfile names (`incbin`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incbin: bool,
    /// A call of `__has_embed`, whose answer depends on whether a file is there and empty
    /// (`has-embed`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_embed: bool,
}

fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `needle` appears in `text` as a whole identifier.
fn has_word(text: &[u8], needle: &[u8]) -> bool {
    memmem::find_iter(text, needle).any(|at| {
        let before = at.checked_sub(1).map(|i| text[i]);
        let after = text.get(at + needle.len()).copied();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

/// Reads one file's bytes for [`Seen`].
pub fn scan(text: &[u8]) -> Seen {
    let mut seen = Seen {
        time: has_word(text, b"__TIME__") || has_word(text, b"__DATE__"),
        timestamp: has_word(text, b"__TIMESTAMP__"),
        incbin: assembler_reads(text),
        has_embed: calls(text, b"__has_embed"),
        ..Seen::default()
    };
    let pat = b"__has_include";
    for at in memmem::find_iter(text, pat) {
        if at > 0 && ident(text[at - 1]) {
            continue;
        }
        let mut j = at + pat.len();
        if text[j..].starts_with(b"_next") {
            j += 5;
        }
        // `__has_include__` and other longer names are not the operator.
        if text.get(j).copied().is_some_and(ident) {
            continue;
        }
        while text.get(j).is_some_and(|b| *b == b' ' || *b == b'\t') {
            j += 1;
        }
        // `#ifdef __has_include`, `defined(__has_include)`: no call, no probe.
        if text.get(j) != Some(&b'(') {
            continue;
        }
        j += 1;
        while text.get(j).is_some_and(|b| *b == b' ' || *b == b'\t') {
            j += 1;
        }
        match text.get(j) {
            Some(open @ (b'"' | b'<')) => {
                let close = if *open == b'"' { b'"' } else { b'>' };
                let rest = &text[j + 1..];
                match rest.iter().position(|c| *c == close || *c == b'\n') {
                    Some(k) if rest[k] == close => {
                        let name = String::from_utf8_lossy(&rest[..k]);
                        let probe = if close == b'"' {
                            format!("\"{name}\"")
                        } else {
                            format!("<{name}>")
                        };
                        if !seen.probes.contains(&probe) {
                            seen.probes.push(probe);
                        }
                    }
                    _ => seen.computed = true,
                }
            }
            // `#define __has_include(x) 0`: the operator's own definition for compilers without
            // it, not a probe.
            _ if defines_it(text, at) => {}
            _ => seen.computed = true,
        }
    }
    seen
}

/// Whether `name` appears as a whole identifier followed by `(`, spaces allowed: a call, not a
/// test of whether it is defined.
fn calls(text: &[u8], name: &[u8]) -> bool {
    memmem::find_iter(text, name).any(|at| {
        if at > 0 && ident(text[at - 1]) {
            return false;
        }
        let mut j = at + name.len();
        if text.get(j).copied().is_some_and(ident) {
            return false;
        }
        while text.get(j).is_some_and(|b| *b == b' ' || *b == b'\t') {
            j += 1;
        }
        text.get(j) == Some(&b'(')
    })
}

/// Whether the text holds an assembler directive that reads a file: `.incbin` as a word (inline
/// assembly is a string, so the byte before it is anything), or `.include` followed by a quoted
/// name (written `\"` inside a C string), which a member called `include` never is. ccache
/// refuses the same.
fn assembler_reads(text: &[u8]) -> bool {
    let word_ends = |at: usize| !text.get(at).copied().is_some_and(ident);
    if memmem::find_iter(text, b".incbin").any(|at| word_ends(at + 7)) {
        return true;
    }
    memmem::find_iter(text, b".include").any(|at| {
        let mut j = at + 8;
        if !text.get(j).is_some_and(|b| *b == b' ' || *b == b'\t') {
            return false;
        }
        while text.get(j).is_some_and(|b| *b == b' ' || *b == b'\t') {
            j += 1;
        }
        matches!(text.get(j), Some(b'"' | b'\\'))
    })
}

/// Whether the name at `at` is the macro a `#define` on its line defines.
fn defines_it(text: &[u8], at: usize) -> bool {
    let start = text[..at]
        .iter()
        .rposition(|c| *c == b'\n')
        .map_or(0, |i| i + 1);
    let line: Vec<u8> = text[start..at]
        .iter()
        .copied()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    line == b"#define"
}

/// The name a probe asks about, without its quotes, and whether it was written `"…"`.
pub fn probe_name(probe: &str) -> (&str, bool) {
    if let Some(name) = probe.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
        (name, true)
    } else {
        let name = probe
            .strip_prefix('<')
            .and_then(|p| p.strip_suffix('>'))
            .unwrap_or(probe);
        (name, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-54
    #[test]
    fn literal_probes_are_named_and_computed_ones_seen() {
        let text = b"#if __has_include(\"opt.h\")\n#endif\n#if __has_include_next( <sys/x.h> )\n#endif\n\
            #ifdef __has_include\n#if defined(__has_include) && __has_include(<opt.h>)\n#endif\n#endif\n";
        let seen = scan(text);
        assert_eq!(seen.probes, ["\"opt.h\"", "<sys/x.h>", "<opt.h>"]);
        assert!(!seen.computed && !seen.time && !seen.timestamp);
        assert!(scan(b"#define HAVE(x) __has_include(x)\n").computed);
        // The SDK's own fallback definition is no probe.
        let fallback = scan(b"#ifndef __has_include\n#define __has_include(x) 0\n#endif\n");
        assert!(!fallback.computed && fallback.probes.is_empty());
        assert!(scan(b"#  define __has_include(__x) 0\n").probes.is_empty());
        assert!(!scan(b"MY__has_include(x)").computed);
    }

    // covers: B-54
    #[test]
    fn the_time_macros_are_seen_as_words() {
        assert!(scan(b"const char *t = __TIME__;").time);
        assert!(scan(b"x = __DATE__").time);
        assert!(!scan(b"x = __DATE__").timestamp);
        assert!(scan(b"x = __TIMESTAMP__").timestamp);
        assert!(!scan(b"ABSL__TIME__X").time);
    }

    // covers: B-54
    #[test]
    fn a_read_no_depfile_names_is_seen() {
        assert!(scan(b"__asm__(\".incbin \\\"data.bin\\\"\");").incbin);
        assert!(scan(b"__asm__(\"x:\\n.incbin \\\"d\\\"\");").incbin);
        assert!(scan(b"asm(\".include \\\"m.s\\\"\");").incbin);
        assert!(!scan(b"config.include(path);").incbin);
        assert!(!scan(b"int incbin_size;").incbin);
        assert!(scan(b"#if __has_embed(\"x.bin\")\n#endif\n").has_embed);
        assert!(scan(b"#if __has_embed (<x.bin>)\n#endif\n").has_embed);
        assert!(!scan(b"#if defined(__has_embed)\n#endif\n").has_embed);
        assert!(!scan(b"#ifdef __has_embed\n#endif\n").has_embed);
    }
}
