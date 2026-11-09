//! The scope pattern grammar (D-7).
//!
//! One grammar on every OS: a pattern is a `/`-separated list of segments, matched
//! case-sensitively against the whole normalized path. A pattern is validated, translated to an
//! anchored regular expression, and compiled in Unicode mode, so `?` and classes match one
//! character and `\` is always an ordinary character (ADR-0003).

use std::fmt;

use regex_automata::meta::Regex;
use regex_automata::util::syntax;

/// A validated, compiled scope pattern.
#[derive(Clone)]
pub struct Pattern {
    source: String,
    regex: Regex,
}

/// Why a pattern is invalid (C-8). `column` counts characters from 1.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid pattern {pattern:?} at character {column}: {kind}")]
pub struct PatternError {
    pub pattern: String,
    pub column: usize,
    pub kind: PatternErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatternErrorKind {
    #[error("the pattern is empty")]
    Empty,
    #[error("empty segment: a pattern has no leading, trailing or doubled `/`")]
    EmptySegment,
    #[error("`**` must be a whole segment")]
    RecursiveWithOther,
    #[error("`{{` and `}}` are not allowed: there is no brace alternation")]
    Brace,
    #[error("unclosed character class")]
    UnclosedClass,
    #[error("a class can't start with `^`; negate it with `!`")]
    CaretClass,
    #[error("invalid range {0:?}-{1:?}: the start is after the end")]
    InvalidRange(char, char),
    #[error("ambiguous `-` after a range: put a literal `-` first or last in the class")]
    AmbiguousRange,
    #[error("{0}")]
    Regex(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// `**` as a whole segment.
    Recursive,
    Parts(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(char),
    /// `*`: any run of characters within one segment.
    Star,
    /// `?`: exactly one character within one segment.
    Any,
    /// `[…]` or `[!…]`: one character from (or not from) the ranges, never `/`.
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

impl Pattern {
    /// Validates and compiles `source`.
    pub fn new(source: &str) -> Result<Self, PatternError> {
        let segments = parse(source)?;
        let regex = Regex::builder()
            .syntax(syntax::Config::new().dot_matches_new_line(true))
            .build(&to_regex(&segments))
            .map_err(|err| PatternError {
                pattern: source.to_string(),
                column: 1,
                kind: PatternErrorKind::Regex(err.to_string()),
            })?;
        Ok(Self {
            source: source.to_string(),
            regex,
        })
    }

    /// The pattern as written.
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Whether the pattern matches the whole of `path`, a normalized path (D-2).
    pub fn is_match(&self, path: &str) -> bool {
        self.regex.is_match(path)
    }
}

impl fmt::Debug for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Pattern").field(&self.source).finish()
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for Pattern {}

/// Validates `source` without compiling it.
pub fn validate(source: &str) -> Result<(), PatternError> {
    parse(source).map(|_| ())
}

fn parse(source: &str) -> Result<Vec<Segment>, PatternError> {
    let chars: Vec<char> = source.chars().collect();
    let error = |index: usize, kind| PatternError {
        pattern: source.to_string(),
        column: index + 1,
        kind,
    };
    if chars.is_empty() {
        return Err(error(0, PatternErrorKind::Empty));
    }

    let mut segments = Vec::new();
    let mut tokens = Vec::new();
    let mut recursive = false;
    let mut segment_start = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '/' => {
                if i == segment_start {
                    return Err(error(i, PatternErrorKind::EmptySegment));
                }
                segments.push(finish_segment(&mut tokens, &mut recursive));
                segment_start = i + 1;
            }
            '{' | '}' => return Err(error(i, PatternErrorKind::Brace)),
            '*' => {
                let run = chars[i..].iter().take_while(|c| **c == '*').count();
                if run == 1 {
                    tokens.push(Token::Star);
                } else {
                    let segment_end = chars[i..]
                        .iter()
                        .position(|c| *c == '/')
                        .map_or(chars.len(), |p| i + p);
                    if run != 2 || i != segment_start || i + 2 != segment_end {
                        return Err(error(i, PatternErrorKind::RecursiveWithOther));
                    }
                    recursive = true;
                }
                i += run;
                continue;
            }
            '?' => tokens.push(Token::Any),
            '[' => {
                let (class, end) = parse_class(&chars, i).map_err(|(at, kind)| error(at, kind))?;
                tokens.push(class);
                i = end + 1;
                continue;
            }
            c => tokens.push(Token::Literal(c)),
        }
        i += 1;
    }
    if segment_start == chars.len() {
        return Err(error(chars.len() - 1, PatternErrorKind::EmptySegment));
    }
    segments.push(finish_segment(&mut tokens, &mut recursive));
    Ok(segments)
}

fn finish_segment(tokens: &mut Vec<Token>, recursive: &mut bool) -> Segment {
    if std::mem::take(recursive) {
        Segment::Recursive
    } else {
        Segment::Parts(std::mem::take(tokens))
    }
}

/// Parses the class that opens at `chars[open]`; returns it and the index of its closing `]`.
fn parse_class(chars: &[char], open: usize) -> Result<(Token, usize), (usize, PatternErrorKind)> {
    let mut j = open + 1;
    let negated = chars.get(j) == Some(&'!');
    if negated {
        j += 1;
    } else if chars.get(j) == Some(&'^') {
        return Err((j, PatternErrorKind::CaretClass));
    }
    let mut ranges: Vec<(char, char)> = Vec::new();
    let mut first = true;
    // A `-` was read after a single character: the next character ends a range.
    let mut in_range = false;
    // The last item is a completed range, so a following `-` would be ambiguous.
    let mut after_range = false;
    loop {
        let Some(&c) = chars.get(j) else {
            return Err((open, PatternErrorKind::UnclosedClass));
        };
        match c {
            '/' => return Err((open, PatternErrorKind::UnclosedClass)),
            '{' | '}' => return Err((j, PatternErrorKind::Brace)),
            ']' if !first => break,
            '-' if first => ranges.push(('-', '-')),
            '-' if in_range => {
                close_range(&mut ranges, '-').map_err(|kind| (j, kind))?;
                in_range = false;
                after_range = true;
            }
            '-' => {
                // A `-` that is the last character of the class is a literal.
                if chars.get(j + 1) == Some(&']') {
                    ranges.push(('-', '-'));
                } else if after_range {
                    return Err((j, PatternErrorKind::AmbiguousRange));
                } else {
                    in_range = true;
                }
            }
            c if in_range => {
                close_range(&mut ranges, c).map_err(|kind| (j, kind))?;
                in_range = false;
                after_range = true;
            }
            c => {
                ranges.push((c, c));
                after_range = false;
            }
        }
        first = false;
        j += 1;
    }
    Ok((Token::Class { negated, ranges }, j))
}

fn close_range(ranges: &mut [(char, char)], end: char) -> Result<(), PatternErrorKind> {
    let last = ranges
        .last_mut()
        .expect("a range always follows a character");
    if end < last.0 {
        return Err(PatternErrorKind::InvalidRange(last.0, end));
    }
    last.1 = end;
    Ok(())
}

fn to_regex(segments: &[Segment]) -> String {
    let mut re = String::from(r"\A(?:");
    for (i, segment) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match segment {
            // Last: one or more whole segments (or, alone, any path).
            Segment::Recursive if last => re.push_str(".+"),
            // First or middle: zero or more whole segments, each followed by `/`.
            Segment::Recursive => re.push_str("(?:.+/)?"),
            Segment::Parts(tokens) => {
                for token in tokens {
                    push_token(&mut re, token);
                }
                if !last {
                    re.push('/');
                }
            }
        }
    }
    re.push_str(r")\z");
    re
}

fn push_token(re: &mut String, token: &Token) {
    match token {
        Token::Literal(c) => {
            if r"\.+*?()|[]{}^$#&-~".contains(*c) {
                re.push('\\');
            }
            re.push(*c);
        }
        Token::Star => re.push_str("[^/]*"),
        Token::Any => re.push_str("[^/]"),
        Token::Class { negated, ranges } => {
            re.push('[');
            if *negated {
                re.push('^');
            }
            for (start, end) in ranges {
                push_class_char(re, *start);
                if start != end {
                    re.push('-');
                    push_class_char(re, *end);
                }
            }
            if *negated {
                push_class_char(re, '/');
            }
            re.push(']');
        }
    }
}

fn push_class_char(re: &mut String, c: char) {
    use std::fmt::Write;
    let _ = write!(re, r"\x{{{:X}}}", u32::from(c));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        Pattern::new(pattern).unwrap().is_match(path)
    }

    fn error(pattern: &str) -> PatternErrorKind {
        Pattern::new(pattern).unwrap_err().kind
    }

    // covers: D-7
    #[test]
    fn spec_examples() {
        assert!(matches("services/api/**", "services/api/main.go"));
        assert!(matches("services/api/**", "services/api/x/y.go"));
        assert!(!matches("services/api/**", "services/apix/a.go"));
        assert!(matches("**/*.md", "README.md"));
        assert!(matches("**/*.md", "docs/a/b.md"));
        assert!(matches("*.go", "main.go"));
        assert!(!matches("*.go", "cmd/main.go"));
    }

    // covers: D-7
    #[test]
    fn recursive_segments() {
        assert!(matches("a/**", "a/b"));
        assert!(matches("a/**", "a/b/c"));
        assert!(!matches("a/**", "a"));
        assert!(matches("**", "a"));
        assert!(matches("**", "a/b/c"));
        assert!(matches("a/**/b", "a/b"));
        assert!(matches("a/**/b", "a/x/y/b"));
        assert!(!matches("a/**/b", "a/xb"));
        assert!(matches("**/b", "b"));
        assert!(matches("**/b", "x/b"));
        assert!(!matches("**/b", "xb"));
        assert!(matches("**", "line\nbreak"));
    }

    // covers: D-7
    #[test]
    fn single_segment_wildcards_match_characters() {
        assert!(matches("?.txt", "é.txt"));
        assert!(!matches("?.txt", "ab.txt"));
        assert!(!matches("a?b", "a/b"));
        assert!(matches("a*", "a"));
        assert!(!matches("a*", "a/b"));
        assert!(matches("[é]", "é"));
        assert!(matches("[à-é]", "è"));
    }

    // covers: D-7
    #[test]
    fn classes() {
        assert!(matches("[abc]x", "bx"));
        assert!(!matches("[abc]x", "dx"));
        assert!(matches("[a-z]", "q"));
        assert!(matches("[!a-z]", "Q"));
        assert!(!matches("a[!b]c", "a/c"));
        assert!(matches("[]]", "]"));
        assert!(matches("[!]]", "x"));
        assert!(!matches("[!]]", "]"));
        assert!(matches("[-a]", "-"));
        assert!(matches("[a-]", "-"));
        assert!(matches("[!a-]", "b"));
        assert!(!matches("[!a-]", "-"));
        assert!(matches("[!-]", "a"));
        assert_eq!(error("[a--]"), PatternErrorKind::InvalidRange('a', '-'));
        assert!(matches("[--z]", "q"));
    }

    // covers: D-2, D-7
    #[test]
    fn backslash_is_ordinary() {
        assert!(matches(r"a\b", r"a\b"));
        assert!(!matches(r"a\b", "a/b"));
        assert!(matches(r"a\*", r"a\xyz"));
        assert!(matches(r"[\]", r"\"));
    }

    #[test]
    fn literal_regex_metacharacters() {
        assert!(matches("a+b(c)|$^.#&~", "a+b(c)|$^.#&~"));
        assert!(!matches("a.b", "axb"));
    }

    // covers: C-8, D-7
    #[test]
    fn validation_errors() {
        use PatternErrorKind::*;
        assert_eq!(error(""), Empty);
        assert_eq!(error("/a"), EmptySegment);
        assert_eq!(error("a/"), EmptySegment);
        assert_eq!(error("a//b"), EmptySegment);
        assert_eq!(error("a**"), RecursiveWithOther);
        assert_eq!(error("**b"), RecursiveWithOther);
        assert_eq!(error("a/***"), RecursiveWithOther);
        assert_eq!(error("{a,b}"), Brace);
        assert_eq!(error("a}"), Brace);
        assert_eq!(error("[ab"), UnclosedClass);
        assert_eq!(error("[a/b]"), UnclosedClass);
        assert_eq!(error("[^a]"), CaretClass);
        assert_eq!(error("[z-a]"), InvalidRange('z', 'a'));
        assert_eq!(error("[a-c-e]"), AmbiguousRange);
        let err = Pattern::new("src/{a,b}").unwrap_err();
        assert_eq!(err.column, 5);
    }
}
