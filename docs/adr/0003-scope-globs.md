# 0003. Scope globs: translate the grammar ourselves instead of compiling with globset

- **Status:** accepted
- **Date:** 2026-10-14
- **Rules:** D-7 (`docs/spec/04-source-digest.md`)

## Context

D-7 defines the complete pattern grammar for scopes, "the same on every OS": `?` is exactly one
character, a class matches one character, and `\` is always an ordinary character. The plan
said to compile patterns with `globset` (`literal_separator(true)`, `backslash_escape(false)`)
after hushrun's own validation.

Reading globset 0.4.20 shows three places where it differs from the grammar:

1. It compiles every glob in byte mode (`(?-u)`). `?` becomes `[^/]`, which matches one *byte*,
   so `?.txt` doesn't match `é.txt`, and a class such as `[é]` or `[à-é]` becomes a class of
   UTF-8 code units that can never match the character.
2. On Windows, `GlobMatcher::is_match` rewrites `\` to `/` in the path before matching, so a git
   path containing a backslash matches differently on Windows than on Linux.
3. Its negated classes can match `/` (the plan already worked around this one).

Each of these changes which files a scope contains, and so the digest, for some repositories
on some OS. `tools/refdigest.py` implements the grammar as written, so the two
implementations would disagree on the same vectors.

## Decision

hushrun validates a pattern (D-7's error list), then translates it to a regular expression
itself and compiles it with `regex-automata` (the engine globset uses) in Unicode mode, with `.`
matching newlines. Matching runs on the normalized path string, never on an OS path:

| Grammar | Regex |
|---|---|
| `**` alone | `.+` |
| `**` as the first or a middle segment | `(?:.+/)?` (zero or more whole segments, each followed by `/`) |
| `**` as the last segment | `.+` after the preceding `/` (one or more whole segments) |
| `*` | `[^/]*` |
| `?` | `[^/]` |
| `[abc]`, `[a-z]` | the same class, with every member written as `\x{…}` |
| `[!abc]` | `[^abc/]` |
| any other character | itself, escaped |

The whole expression is anchored at both ends. D-7's implementation note now describes this.

### Ambiguous classes are errors

The grammar leaves two class spellings open. Both are now validation errors, which runs more
checks rather than guessing (plan §0.1):

- a range whose start is after its end (`[z-a]`);
- a `-` right after a completed range that isn't the last character of the class (`[a-c-e]`):
  globset reads it as `a-e`, POSIX leaves it undefined, and a reader could expect `a-c`, `-`,
  `e`.

## Consequences

- One grammar on every OS, character-based, matching `refdigest.py` exactly; the glob golden
  vectors run on Linux, macOS and Windows (VALIDATION.md §4.1).
- `globset` is not a dependency; `regex-automata` is (it was already in the tree through
  `ignore`).
- The translation is about 150 lines with its validation, all of it covered by the vectors.

## Alternatives considered

- **globset as planned.** Rejected for the three differences above.
- **globset with a pre-pass that rewrites `?` and classes.** Byte-mode classes can't express a
  multi-byte character, and the Windows path rewrite happens inside the matcher.
- **A hand-written matcher with no regex engine.** Simple, but backtracking on `*` makes the
  worst case quadratic per pattern, and `regex-automata` gives linear-time matching.
