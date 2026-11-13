# 4. Spec: source digest v1

> Normative. Copied from the implementation plan (2026-10-13) in M0; this file is now the source of truth.
> MUST, SHOULD and MAY follow RFC 2119. Rules tagged `[Mn]` belong to milestone n.

**D-1 Entries.** A VCS adapter (§11.1) yields a set of entries `(path, kind, content)` for one revision or snapshot. Scope filtering (D-7) happens before hashing.

**D-2 Paths.** A path:

- is relative and `/`-separated, with no leading or trailing `/` and no empty, `.` or `..` segments;
- is valid UTF-8 (otherwise the adapter fails with `E_PATH_ENCODING`, printing the path in hex) and is normalized to Unicode NFC;
- is unique after normalization (otherwise `E_PATH_COLLISION`);
- never has a segment equal to `.git`, `.hg`, `.svn`, `.bzr` or `_darcs`, because adapters skip VCS metadata.

A backslash inside a path is an ordinary character. Only the `dir` adapter on Windows converts the OS separator to `/`.

**D-3 Kinds and content hashes.** `content_hash` is the lowercase hex SHA-256 of the content column:

| Kind | Entry | Content |
|---|---|---|
| `f` | Regular file without execute bits | The file bytes as stored in the VCS: no line-ending, keyword or filter conversion |
| `x` | Regular file with any execute bit | Same as `f` |
| `l` | Symbolic link | The link target bytes as stored; links are never followed |
| `g` | Git submodule (gitlink) | The ASCII string `<object format>:<commit id hex>`, such as `sha1:3f9c…`; git adapter only |

Empty directories are not represented. FIFOs, sockets and devices fail with `E_UNSUPPORTED_FILE`. Git LFS files are hashed as their pointer files, which embed the object's SHA-256.

In a git tree, a blob's mode is first canonicalized as git does when it checks the file out: only the owner execute bit (`0o100`) makes it executable, so a legacy mode such as `100664` is `f` and `100755` is `x`. A checkout then has the same kinds as the tree (D-13). (ADR-0004.)

**D-4 Manifest.**

```
manifest = "hushrun-source-v1\n" || entry_1 || … || entry_n
entry    = kind || " " || content_hash || " " || path || "\0"
```

Entries are sorted by path, comparing UTF-8 bytes (memcmp order). The encoding is injective, because `kind` and `content_hash` have fixed lengths and paths contain no NUL.

**D-5 Digest.** The digest is `SHA-256(manifest)`, written `sha256:<64 lowercase hex>`. Running `sha256sum` on the manifest bytes (from `hushrun digest --manifest-out`) gives the same value, so anyone can check a digest with standard tools.

**D-6 Empty scopes.** A scope with no entries fails with `E_EMPTY_SCOPE`, which almost always means a misconfigured glob.

**D-7 Scope filtering.** An entry belongs to a scope when all three hold:

- its path matches at least one of the scope's `include` patterns;
- it matches none of the scope's `exclude` patterns;
- it matches no global `digest.exclude` pattern.

The default `include` is `["**"]`. A scope MUST cover everything that determines its artifact *and its deployment*: build files, the Dockerfile, the deploy workflow. The default scope covers everything, which is always safe.

**Pattern grammar.** This is the complete grammar, the same on every OS. A pattern is matched case-sensitively against the whole normalized path. It is a `/`-separated list of segments, with no leading or trailing `/`.

| Pattern element | Meaning |
|---|---|
| `**` as a whole segment | First or middle segment: zero or more whole segments. Last segment: one or more whole segments, so `a/**` matches `a/b` and `a/b/c`, but not a file named `a`. |
| `*` | Any run of characters within one segment, possibly empty |
| `?` | Exactly one character within one segment |
| `[abc]`, `[a-z]` | One character from the class, never `/` |
| `[!abc]` | One character not in the class, never `/` |
| `]` first in a class | A literal `]` |
| `-` first or last in a class | A literal `-` |
| any other character | Itself, including `\`, which is never an escape |

These are validation errors (C-8):

- `**` combined with other characters in one segment (`a**`, `**b`);
- `{` or `}` anywhere (no brace alternation);
- an empty segment;
- an unclosed class, or a class starting with `^` (use `!`);
- a range whose start is after its end, such as `[z-a]`;
- a `-` right after a completed range that isn't the last character of the class, such as `[a-c-e]`, which other glob implementations read in different ways. (ADR-0003.)

Examples:

- `services/api/**` matches `services/api/main.go` and `services/api/x/y.go`, but not `services/apix/a.go`.
- `**/*.md` matches `README.md` and `docs/a/b.md`.
- `*.go` matches `main.go`, but not `cmd/main.go`.

The implementation validates a pattern as above, translates it to an anchored regular expression itself, and compiles that with `regex-automata` in Unicode mode, matching the normalized path string, so that `?` and classes match one character and `\` is ordinary on every OS. Every `[!…]` class also excludes `/`. (ADR-0003 explains why this replaced `globset`, whose byte-mode classes and Windows separator rewriting differ from this grammar.) `tools/refdigest.py` implements the same grammar by hand. Glob golden vectors run on all three OSes (VALIDATION.md §4.1).

**D-8 Versioning.** `hushrun-source-v1` is frozen once M1 ships. Any change becomes a new canonicalization (`hushrun-source-v2`) that events record (§5), and the planner never matches digests across canonicalizations.

Rationale (ADR-0001):

- Git tree hashes are git-specific and depend on the object format.
- in-toto's `dirHash` (Go's `h1`) ignores file modes and symlinks.
- A flat sorted manifest handles arbitrary glob scopes and takes about ten lines in any language. `tools/refdigest.py` is the independent reference implementation.

### 4.1 Adapter rules

**D-9 git, revision mode** (`--rev`, default `HEAD`). Entries come from the revision's tree in the object database, and blob bytes are used as stored; the worktree is never read. Missing objects (blob-less or tree-less partial clones) fail with `E_MISSING_OBJECTS` and a hint to use a full or shallow clone.

There are two object readers, and both MUST pass the same vectors:

- **`gix`** is the default. A read it can't do goes to the `git` CLI, and only what that can't read either fails: a `git gc` running in the background moves objects into a pack, and for that moment `gix` can miss an object that's there. Asking for `gix` by name turns this off.
- **The `git` CLI** (`git ls-tree -r -z --full-tree <rev>` plus `git cat-file --batch`) is used when the repository uses the `sha256` object format, or when `gix` can't open the repository. gitoxide's SHA-256 support was still incomplete when this plan was written; check its status at the start of M1.

**D-10 git, worktree mode** (`--worktree`). The digest equals the digest of the commit that `git add -A && git commit` would create now:

1. Find the index with `git rev-parse --git-path index`. This works in linked worktrees and submodules, where `.git` is a file.
2. Copy it to a temporary file. If the repository uses a split index (`core.splitIndex`), create the temporary index with `git read-tree HEAD` instead (slower: every file is re-hashed).
3. Run `git add -A` and `git write-tree` with `GIT_INDEX_FILE` pointing at the temporary index.
4. Hash the resulting tree as in D-9.

The real index is never modified. `git add -A` writes loose objects into the repository's object database; this is harmless, git's garbage collection removes them, and the docs mention it.

**D-11 dir mode** (`--dir <path>`). Every regular file and symlink under the root is an entry, except VCS metadata (D-2). No ignore files apply, because the directory is assumed to be a clean export, and symlinks are never followed. On Windows there are no execute bits (every file is `f`), and `\` in symlink targets becomes `/`.

**D-12 exec mode [M11].** A configured command exports the revision into an empty temporary directory, which is then hashed as in D-11. Recipes for Mercurial, Subversion and Perforce MUST turn off archive metadata, keyword expansion and line-ending conversion. Examples: `hg archive --config ui.archivemeta=false`, and `svn export --ignore-keywords --native-eol LF`. Where a VCS can't turn one off, the recipe documents the difference.

**D-13 Equivalence.** For identical content, these MUST produce the same digest: git revision mode, git worktree mode, and, on POSIX systems, dir mode on a clean export. On Windows, dir mode can't see execute bits (D-11), so it matches only content without executable files. Symlinks match only where the checkout created real ones. Golden vectors enforce this (VALIDATION.md §4.1).
