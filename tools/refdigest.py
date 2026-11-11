#!/usr/bin/env python3
"""Independent reference implementation of hushrun's source digest v1 (D-1 ... D-8), of the
unit key (C-31), and of hints (C-38): their key, tokens, address, content id and reader.

Written from docs/spec/04-source-digest.md, not from the Rust code, so the two can check each
other on the golden vectors. Standard library only; Python 3.9 or later.

Usage:
  refdigest.py git  [--repo DIR] [--rev REV] [SCOPE OPTIONS] [--manifest-out FILE]
  refdigest.py dir  PATH                     [SCOPE OPTIONS] [--manifest-out FILE]
  refdigest.py vectors FILE [--update]   check (or rewrite) the expected digests of every vector
  refdigest.py globs FILE                check every case of a glob vector file
  refdigest.py unit-keys FILE            check every unit key vector (C-31)
  refdigest.py unit-hints FILE [--corpus DIR]
                                         check every hint vector (C-38); with --corpus, also write
                                         each vector's bytes and content there, for the fuzzer

Scope options (one scope; the default includes everything):
  --include PATTERN    repeatable; default "**"
  --exclude PATTERN    repeatable
  --global-exclude PATTERN   repeatable (digest.exclude)

The digest is printed as sha256:<hex>. Errors print their code (E_EMPTY_SCOPE, ...) and exit 1.
"""

import argparse
import base64
import hashlib
import hmac
import json
import os
import subprocess
import sys
import tempfile
import unicodedata
import zlib

CANONICALIZATION = "hushrun-source-v1"
VCS_METADATA = {".git", ".hg", ".svn", ".bzr", "_darcs"}


class DigestError(Exception):
    def __init__(self, code, message):
        super().__init__("%s: %s" % (code, message))
        self.code = code


# --- Paths (D-2) -------------------------------------------------------------------------

def normalize_path(raw):
    """Raw path bytes -> normalized path, or None for VCS metadata (D-2)."""
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        raise DigestError("E_PATH_ENCODING", "path is not UTF-8: " + raw.hex())
    path = unicodedata.normalize("NFC", text)
    segments = path.split("/")
    for segment in segments:
        if segment in ("", ".", "..") or "\0" in segment:
            raise DigestError("E_PATH_INVALID", "invalid path %r" % path)
    if any(segment in VCS_METADATA for segment in segments):
        return None
    return path


# --- Patterns (D-7) ----------------------------------------------------------------------

class PatternError(Exception):
    def __init__(self, kind, column):
        super().__init__("%s at character %d" % (kind, column))
        self.kind = kind
        self.column = column


RECURSIVE = "**"


def compile_pattern(pattern):
    """Validates a pattern; returns its segments: RECURSIVE or a list of tokens.

    Tokens: ("lit", c), ("star",), ("any",), ("class", negated, [(lo, hi), ...]).
    """
    if pattern == "":
        raise PatternError("Empty", 1)
    chars = list(pattern)
    segments = []
    tokens = []
    recursive = False
    start = 0
    i = 0
    while i < len(chars):
        c = chars[i]
        if c == "/":
            if i == start:
                raise PatternError("EmptySegment", i + 1)
            segments.append(RECURSIVE if recursive else tokens)
            tokens, recursive, start = [], False, i + 1
        elif c in "{}":
            raise PatternError("Brace", i + 1)
        elif c == "*":
            run = 0
            while i + run < len(chars) and chars[i + run] == "*":
                run += 1
            if run == 1:
                tokens.append(("star",))
            else:
                end = i
                while end < len(chars) and chars[end] != "/":
                    end += 1
                if run != 2 or i != start or i + 2 != end:
                    raise PatternError("RecursiveWithOther", i + 1)
                recursive = True
            i += run
            continue
        elif c == "?":
            tokens.append(("any",))
        elif c == "[":
            token, close = parse_class(chars, i)
            tokens.append(token)
            i = close + 1
            continue
        else:
            tokens.append(("lit", c))
        i += 1
    if start == len(chars):
        raise PatternError("EmptySegment", len(chars))
    segments.append(RECURSIVE if recursive else tokens)
    return segments


def parse_class(chars, open_at):
    j = open_at + 1
    negated = j < len(chars) and chars[j] == "!"
    if negated:
        j += 1
    elif j < len(chars) and chars[j] == "^":
        raise PatternError("CaretClass", j + 1)
    ranges = []
    first = True
    pending_range = False   # a "-" follows a single character: the next one ends a range
    after_range = False     # the last item is a completed range
    while True:
        if j >= len(chars) or chars[j] == "/":
            raise PatternError("UnclosedClass", open_at + 1)
        c = chars[j]
        if c in "{}":
            raise PatternError("Brace", j + 1)
        if c == "]" and not first:
            break
        if c == "-" and first:
            ranges.append(["-", "-"])
        elif c == "-" and pending_range:
            close_range(ranges, "-", j)
            pending_range, after_range = False, True
        elif c == "-":
            if j + 1 < len(chars) and chars[j + 1] == "]":
                ranges.append(["-", "-"])  # last character of the class: a literal
            elif after_range:
                raise PatternError("AmbiguousRange", j + 1)
            else:
                pending_range = True
        elif pending_range:
            close_range(ranges, c, j)
            pending_range, after_range = False, True
        else:
            ranges.append([c, c])
            after_range = False
        first = False
        j += 1
    return ("class", negated, [tuple(r) for r in ranges]), j


def close_range(ranges, end, j):
    if end < ranges[-1][0]:
        raise PatternError("InvalidRange", j + 1)
    ranges[-1][1] = end


def segment_matches(tokens, text):
    """Whether one segment's tokens match one path segment (no '/')."""
    # reachable[k]: tokens[:t] can match text[:k]
    reachable = [True] + [False] * len(text)
    for token in tokens:
        nxt = [False] * (len(text) + 1)
        kind = token[0]
        if kind == "star":
            seen = False
            for k in range(len(text) + 1):
                seen = seen or reachable[k]
                nxt[k] = seen
        else:
            for k in range(len(text)):
                if reachable[k] and char_matches(token, text[k]):
                    nxt[k + 1] = True
        reachable = nxt
    return reachable[len(text)]


def char_matches(token, c):
    kind = token[0]
    if kind == "lit":
        return token[1] == c
    if kind == "any":
        return c != "/"
    _, negated, ranges = token
    inside = any(lo <= c <= hi for lo, hi in ranges)
    return c != "/" and (inside != negated)


def pattern_matches(segments, path):
    parts = path.split("/")
    memo = {}

    def match(i, j):
        key = (i, j)
        if key in memo:
            return memo[key]
        if i == len(segments):
            result = j == len(parts)
        elif segments[i] == RECURSIVE:
            if i == len(segments) - 1:
                result = len(parts) - j >= 1          # last: one or more whole segments
            else:
                result = any(match(i + 1, k) for k in range(j, len(parts) + 1))
        else:
            result = j < len(parts) and segment_matches(segments[i], parts[j]) and match(i + 1, j + 1)
        memo[key] = result
        return result

    return match(0, 0)


class Scope:
    def __init__(self, include=None, exclude=None, global_exclude=None):
        self.include = [compile_pattern(p) for p in (include or ["**"])]
        self.exclude = [compile_pattern(p) for p in (exclude or [])]
        self.global_exclude = [compile_pattern(p) for p in (global_exclude or [])]

    def contains(self, path):
        return (any(pattern_matches(p, path) for p in self.include)
                and not any(pattern_matches(p, path) for p in self.exclude)
                and not any(pattern_matches(p, path) for p in self.global_exclude))


# --- Manifest and digest (D-3 ... D-6) -------------------------------------------------------

def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


def manifest(entries, scope):
    """entries: (path, kind, content bytes). Returns the manifest bytes of the scope."""
    chosen = []
    seen = set()
    for path, kind, content in entries:
        if path in seen:
            raise DigestError("E_PATH_COLLISION", "duplicate path %r" % path)
        seen.add(path)
        if scope.contains(path):
            chosen.append((path.encode("utf-8"), kind, content))
    if not chosen:
        raise DigestError("E_EMPTY_SCOPE", "the scope contains no files")
    chosen.sort(key=lambda e: e[0])
    out = bytearray(CANONICALIZATION.encode() + b"\n")
    for path, kind, content in chosen:
        out += kind.encode() + b" " + sha256_hex(content).encode() + b" " + path + b"\0"
    return bytes(out)


def digest(manifest_bytes):
    return "sha256:" + sha256_hex(manifest_bytes)


# --- Sources (D-9, D-11) -----------------------------------------------------------------------

def git(repo, *args, stdin=None):
    return subprocess.run(["git", "-C", repo] + list(args), input=stdin,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout


def git_entries(repo, rev):
    """Entries of a git revision, read from the object database (D-9)."""
    object_format = git(repo, "rev-parse", "--show-object-format").decode().strip()
    tree = git(repo, "rev-parse", "--verify", rev + "^{tree}").decode().strip()
    records = []
    for record in git(repo, "ls-tree", "-r", "-z", "--full-tree", tree).split(b"\0"):
        if not record:
            continue
        meta, raw_path = record.split(b"\t", 1)
        mode, _type, oid = meta.split(b" ")
        records.append((int(mode, 8), oid.decode(), raw_path))
    blobs = [oid for mode, oid, _ in records if (mode & 0o170000) != 0o160000]
    contents = read_blobs(repo, blobs)
    entries = []
    for mode, oid, raw_path in records:
        path = normalize_path(raw_path)
        if path is None:
            continue
        kind_bits = mode & 0o170000
        if kind_bits == 0o100000:
            # git canonicalizes blob modes on checkout: only the owner execute bit counts (D-3).
            kind = "x" if mode & 0o100 else "f"
            content = contents[oid]
        elif kind_bits == 0o120000:
            kind, content = "l", contents[oid]
        elif kind_bits == 0o160000:
            kind, content = "g", ("%s:%s" % (object_format, oid)).encode()
        else:
            raise DigestError("E_UNSUPPORTED_FILE", "mode %o at %r" % (mode, path))
        entries.append((path, kind, content))
    return entries


def read_blobs(repo, oids):
    unique = sorted(set(oids))
    if not unique:
        return {}
    env = dict(os.environ, GIT_NO_LAZY_FETCH="1")
    out = subprocess.run(["git", "-C", repo, "cat-file", "--batch"], env=env,
                         input="".join(o + "\n" for o in unique).encode(),
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout
    contents = {}
    pos = 0
    for oid in unique:
        end = out.index(b"\n", pos)
        header = out[pos:end].split(b" ")
        if header[1] == b"missing":
            raise DigestError("E_MISSING_OBJECTS", "object %s is missing" % oid)
        size = int(header[2])
        contents[oid] = out[end + 1:end + 1 + size]
        pos = end + 1 + size + 1
    return contents


def dir_entries(root):
    """Entries of a plain directory (D-11): regular files and symbolic links, never followed."""
    entries = []
    pending = [""]
    while pending:
        relative_dir = pending.pop()
        with os.scandir(os.path.join(root, relative_dir) if relative_dir else root) as it:
            for item in it:
                relative = relative_dir + "/" + item.name if relative_dir else item.name
                if item.name in VCS_METADATA:
                    continue
                if item.is_symlink():
                    target = os.readlink(item.path)
                    if os.name == "nt":
                        target = target.replace("\\", "/")
                    content = os.fsencode(target)
                    kind = "l"
                elif item.is_dir(follow_symlinks=False):
                    pending.append(relative)
                    continue
                elif item.is_file(follow_symlinks=False):
                    mode = os.lstat(item.path).st_mode
                    kind = "x" if os.name != "nt" and mode & 0o111 else "f"
                    with open(item.path, "rb") as f:
                        content = f.read()
                else:
                    raise DigestError("E_UNSUPPORTED_FILE", "%r is not a regular file or link" % relative)
                raw = os.fsencode(relative.replace(os.sep, "/") if os.name == "nt" else relative)
                path = normalize_path(raw)
                if path is not None:
                    entries.append((path, kind, content))
    return entries


# --- Golden vectors ------------------------------------------------------------------------

def generated_bytes(spec):
    """Deterministic content: `size` bytes of a SHA-256 counter stream seeded with `seed`."""
    size, seed = spec["size"], spec.get("seed", 0)
    out = bytearray()
    counter = 0
    while len(out) < size:
        out += hashlib.sha256(b"hushrun-vector-%d-%d" % (seed, counter)).digest()
        counter += 1
    return bytes(out[:size])


def vector_content(entry):
    if "text" in entry:
        return entry["text"].encode("utf-8")
    if "base64" in entry:
        return base64.b64decode(entry["base64"])
    if "generate" in entry:
        return generated_bytes(entry["generate"])
    if "target" in entry:
        return entry["target"].encode("utf-8")
    if "commit" in entry:
        return ("%s:%s" % (entry.get("objectFormat", "sha1"), entry["commit"])).encode()
    return b""


def vector_raw_path(entry):
    if "pathHex" in entry:
        return bytes.fromhex(entry["pathHex"])
    return entry["path"].encode("utf-8")


def vector_entries(vector):
    entries = []
    for entry in vector["entries"]:
        path = normalize_path(vector_raw_path(entry))
        if path is None:
            continue
        entries.append((path, entry["kind"], vector_content(entry)))
    # A vector-authoring check, not a digest rule: no path can be both a file and a directory.
    paths = {p for p, _, _ in entries}
    for path in paths:
        parts = path.split("/")
        for i in range(1, len(parts)):
            if "/".join(parts[:i]) in paths:
                raise ValueError("vector %r: %r is both a file and a directory" % (vector["name"], "/".join(parts[:i])))
    return entries


def vector_scopes(vector):
    config = vector.get("config") or {}
    global_exclude = config.get("digestExclude", [])
    scopes = config.get("scopes") or {"default": {"include": ["**"]}}
    return {name: Scope(s.get("include", ["**"]), s.get("exclude", []), global_exclude)
            for name, s in scopes.items()}


def evaluate_vector(vector):
    """{scope: {digest, files}} or {"error": code}."""
    try:
        entries = vector_entries(vector)
        scopes = vector_scopes(vector)
        result = {}
        for name in sorted(scopes):
            m = manifest(entries, scopes[name])
            result[name] = {"digest": digest(m), "files": m.count(b"\0")}
        return result
    except DigestError as err:
        return {"error": err.code}
    except PatternError as err:
        return {"error": "E_PATTERN_" + err.kind}


def materialize_git(vector, directory):
    """A git repository whose HEAD holds exactly the vector's entries, built with plumbing so
    raw paths (NFD, non-UTF-8) and modes are exact."""
    object_format = vector.get("objectFormat", "sha1")
    git(directory, "init", "-q", "--object-format=" + object_format, ".")
    lines = []
    for entry in vector["entries"]:
        kind = entry["kind"]
        if kind == "g":
            mode, oid = "160000", entry["commit"]
        else:
            mode = {"f": "100644", "x": "100755", "l": "120000"}[kind]
            if "mode" in entry:
                mode = entry["mode"]
            oid = git(directory, "hash-object", "-w", "--stdin", stdin=vector_content(entry)).decode().strip()
        lines.append(mode.encode() + b" " + oid.encode() + b"\t" + vector_raw_path(entry) + b"\0")
    git(directory, "update-index", "-z", "--index-info", stdin=b"".join(lines))
    tree = git(directory, "write-tree").decode().strip()
    env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t", GIT_COMMITTER_NAME="t",
               GIT_COMMITTER_EMAIL="t@t", GIT_AUTHOR_DATE="2026-01-01T00:00:00Z",
               GIT_COMMITTER_DATE="2026-01-01T00:00:00Z")
    commit = subprocess.run(["git", "-C", directory, "commit-tree", tree, "-m", "vector"], env=env,
                            stdout=subprocess.PIPE, check=True).stdout.decode().strip()
    git(directory, "update-ref", "HEAD", commit)


def check_vectors(path, update):
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    failures = 0
    for vector in data["vectors"]:
        got = evaluate_vector(vector)
        # The same content through this tool's own git source.
        if not vector.get("noGit") and "error" not in got:
            with tempfile.TemporaryDirectory() as tmp:
                materialize_git(vector, tmp)
                scopes = vector_scopes(vector)
                entries = git_entries(tmp, "HEAD")
                via_git = {n: {"digest": digest(manifest(entries, scopes[n])),
                               "files": manifest(entries, scopes[n]).count(b"\0")} for n in sorted(scopes)}
                if via_git != got:
                    print("FAIL %s: git source gives %s, entries give %s" % (vector["name"], via_git, got))
                    failures += 1
        if update:
            vector["expect"] = got
            continue
        if got != vector.get("expect"):
            print("FAIL %s: got %s, expected %s" % (vector["name"], got, vector.get("expect")))
            failures += 1
        else:
            print("ok   %s" % vector["name"])
    if update:
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2, ensure_ascii=False)
            f.write("\n")
        print("updated %d vectors" % len(data["vectors"]))
    return failures


def check_globs(path):
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    failures = 0
    for case in data["cases"]:
        pattern = case["pattern"]
        try:
            segments = compile_pattern(pattern)
            error = None
        except PatternError as err:
            segments, error = None, err.kind
        if case.get("error") != error:
            print("FAIL %r: error %r, expected %r" % (pattern, error, case.get("error")))
            failures += 1
            continue
        for path in case.get("matches", []):
            if not pattern_matches(segments, path):
                print("FAIL %r should match %r" % (pattern, path))
                failures += 1
        for path in case.get("rejects", []):
            if pattern_matches(segments, path):
                print("FAIL %r should not match %r" % (pattern, path))
                failures += 1
    print("globs: %d cases, %d failures" % (len(data["cases"]), failures))
    return failures


# --- Unit keys (C-31) -------------------------------------------------------------------------

UNIT_KEY_SCHEMA = "hushrun.unit-key/v0"


def files_digest(paths, files):
    """Source digest of those of paths that files ({path: (kind, content bytes)}) holds. Unlike a
    scope's, it may be of no file: an empty set digests the header line alone (C-31)."""
    chosen = sorted((p.encode("utf-8"), files[p][0], files[p][1]) for p in set(paths) if p in files)
    out = bytearray(CANONICALIZATION.encode() + b"\n")
    for path, kind, content in chosen:
        out += kind.encode() + b" " + sha256_hex(content).encode() + b" " + path + b"\0"
    return digest(bytes(out))


def shape_digest(paths):
    """SHA-256 of the sources set's paths: each path's UTF-8 bytes and a NUL, sorted by bytes."""
    data = b"".join(p + b"\0" for p in sorted({p.encode("utf-8") for p in paths}))
    return "sha256:" + sha256_hex(data)


def jcs(value):
    """RFC 8785 for what a unit key holds: strings, lists and objects, no numbers."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def evaluate_unit_key(vector):
    files = {f["path"]: (f["kind"], f["text"].encode("utf-8")) for f in vector["files"]}
    keyed = [p for p in vector["reads"] if p in set(vector["units"]) | set(vector["sources"])]
    reads = files_digest(keyed, files)
    rest = files_digest(vector["rest"], files)
    shape = shape_digest(vector["sources"])
    base = vector["base"]
    key = {"schema": UNIT_KEY_SCHEMA, "check": base["check"], "command": base["command"],
           "cwd": base["cwd"], "scope": base["scope"], "unit": vector["unit"],
           "reads": reads, "rest": rest, "shape": shape}
    for optional in ("external", "counts_from"):
        if optional in base:
            key[optional] = base[optional]
    return {"reads": reads, "rest": rest, "shape": shape, "key": "sha256:" + sha256_hex(jcs(key))}


def check_unit_keys(path):
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    failures = 0
    for vector in data["vectors"]:
        got = evaluate_unit_key(vector)
        if got != vector["expect"]:
            print("FAIL %s: got %s, expected %s" % (vector["name"], got, vector["expect"]))
            failures += 1
        else:
            print("ok   %s" % vector["name"])
    return failures


# --- Hints (C-38) -----------------------------------------------------------------------------

HINT_SCHEMA = b"hushrun.unit-hints/v0\n"
HINT_KEY_DOMAIN = b"hushrun.unit-hints-key/v0"
HINT_ADDRESS_SCHEMA = "hushrun.unit-hints-address/v0"
HINT_MOST_BYTES = 2 * 1024 * 1024
HINT_MOST_CONTENT = 16 * 1024 * 1024
HINT_MOST_UNITS = 5000
HINT_MOST_TOKENS = 1000000
HINT_MOST_ENTRIES = 2000000


class Unread(Exception):
    pass


def shape_bytes(paths):
    """What shape hashes: each source path's UTF-8 bytes and a NUL, sorted by bytes, each once."""
    return b"".join(p + b"\0" for p in sorted({p.encode("utf-8") for p in paths}))


def hint_key(sources):
    """HMAC-SHA-256 under the key domain of the bytes shape hashes, not of the shape digest."""
    return hmac.new(HINT_KEY_DOMAIN, shape_bytes(sources), hashlib.sha256).digest()


def hint_token(key, path):
    return hmac.new(key, path.encode("utf-8"), hashlib.sha256).digest()[:16]


def hint_address(check_id, shape):
    """SHA-256 of the JCS form of {schema, check, shape}, both written sha256:<hex>."""
    return "sha256:" + sha256_hex(jcs({"schema": HINT_ADDRESS_SCHEMA, "check": check_id, "shape": shape}))


def hint_content(hint, key):
    """The canonical content of a hint ({adapter, units: {path: {reads, dirs}}, unitFiles})."""
    def tokens_of(paths):
        return {hint_token(key, p) for p in paths}

    # Each unit by its own token; of two with one token, the first by path stays.
    kept = {}
    for own in sorted(hint["units"], key=lambda p: p.encode("utf-8")):
        unit = hint["units"][own]
        kept.setdefault(hint_token(key, own), (tokens_of(unit["reads"]), tokens_of(unit["dirs"])))
    unit_files = tokens_of(hint["unitFiles"])
    table = set(unit_files)
    for own, (reads, dirs) in kept.items():
        table |= {own} | reads | dirs
    tokens = sorted(table)
    index = {t: i for i, t in enumerate(tokens)}

    def indices(items):
        return sorted(index[t] for t in items)

    units = {index[own]: (indices(reads), indices(dirs)) for own, (reads, dirs) in kept.items()}

    def u32(n):
        return n.to_bytes(4, "big")

    def lst(items):
        return u32(len(items)) + b"".join(u32(i) for i in items)

    word = hint["adapter"].encode("ascii")
    out = bytearray(HINT_SCHEMA + bytes([len(word)]) + word + u32(len(tokens)) + b"".join(tokens))
    out += lst(indices(unit_files))
    out += u32(len(units))
    for own in sorted(units):
        reads, dirs = units[own]
        out += u32(own) + lst(reads) + lst(dirs)
    return bytes(out)


def hint_inflate(data):
    """One zlib stream, nothing after it, inflating to at most HINT_MOST_CONTENT bytes."""
    if len(data) > HINT_MOST_BYTES:
        raise Unread("too large")
    if not data:
        raise Unread("empty")
    inflater = zlib.decompressobj(15)
    try:
        out = inflater.decompress(data, HINT_MOST_CONTENT + 1)
    except zlib.error as err:
        raise Unread("not a zlib stream: %s" % err)
    if len(out) > HINT_MOST_CONTENT:
        raise Unread("inflates too large")
    if not inflater.eof:
        raise Unread("truncated")
    if inflater.unused_data or inflater.unconsumed_tail:
        raise Unread("trailing bytes")
    return out


def hint_parse(content):
    """The content's structure: (adapter, tokens, unit files, [(own, reads, dirs)])."""
    if len(content) > HINT_MOST_CONTENT:
        raise Unread("inflates too large")
    pos = [0]

    def take(n):
        if pos[0] + n > len(content):
            raise Unread("truncated")
        chunk = content[pos[0]:pos[0] + n]
        pos[0] += n
        return chunk

    def u32():
        return int.from_bytes(take(4), "big")

    def count(most, each):
        n = u32()
        if n > most:
            raise Unread("too many")
        if n * each > len(content) - pos[0]:
            raise Unread("truncated")
        return n

    if content[:len(HINT_SCHEMA)] != HINT_SCHEMA:
        raise Unread("schema")
    take(len(HINT_SCHEMA))
    word = take(take(1)[0])
    if not 1 <= len(word) <= 32 or any(chr(b) not in "abcdefghijklmnopqrstuvwxyz0123456789-" for b in word):
        raise Unread("adapter")
    tokens = [take(16) for _ in range(count(HINT_MOST_TOKENS, 16))]
    if any(a >= b for a, b in zip(tokens, tokens[1:])):
        raise Unread("tokens out of order")
    used = set()
    entries = [0]

    def index():
        i = u32()
        if i >= len(tokens):
            raise Unread("index past the tokens")
        used.add(i)
        return i

    def lst(most):
        n = count(most, 4)
        entries[0] += n
        if entries[0] > HINT_MOST_ENTRIES:
            raise Unread("too many entries")
        items = [index() for _ in range(n)]
        if any(a >= b for a, b in zip(items, items[1:])):
            raise Unread("indices out of order")
        return items

    unit_files = lst(HINT_MOST_UNITS)
    units = []
    for _ in range(count(HINT_MOST_UNITS, 12)):
        own = index()
        if units and units[-1][0] >= own:
            raise Unread("units out of order")
        units.append((own, lst(HINT_MOST_TOKENS), lst(HINT_MOST_TOKENS)))
    if pos[0] != len(content):
        raise Unread("bytes after its end")
    if len(used) != len(tokens):
        raise Unread("a token no index names")
    return word.decode("ascii"), tokens, unit_files, units


def hint_read(data, key, scope, inflated=False):
    """The hint data holds, tokens matched against scope's files and the directories holding them
    ("" and "." the root); a unit any token of which matches nothing, or two paths, is dropped."""
    content = data if inflated else hint_inflate(data)
    adapter, tokens, unit_files, units = hint_parse(content)
    candidates = [(p, "file") for p in scope]
    dirs = {"", "."}
    for p in scope:
        raw = p.encode("utf-8")
        dirs.update(raw[:i].decode("utf-8") for i, b in enumerate(raw) if b == ord("/"))
    candidates += [(d, "dir") for d in dirs]
    found = {}
    position = {t: i for i, t in enumerate(tokens)}
    for path, kind in candidates:
        i = position.get(hint_token(key, path))
        if i is None:
            continue
        if found.get(i, (path, kind)) != (path, kind):
            found[i] = (None, "two")
        else:
            found[i] = (path, kind)

    def match(i, kinds):
        path, kind = found.get(i, (None, None))
        return path if kind in kinds else None

    read_units = {}
    for own, reads, dirs_ in units:
        own_path = match(own, ("file", "dir"))
        read_paths = [match(i, ("file",)) for i in reads]
        dir_paths = [match(i, ("dir",)) for i in dirs_]
        if own_path is None or None in read_paths or None in dir_paths:
            continue
        read_units[own_path] = {"reads": sorted(read_paths), "dirs": sorted(dir_paths)}
    files = sorted(p for p in (match(i, ("file",)) for i in unit_files) if p is not None)
    return {"hint": {"adapter": adapter, "units": read_units, "unitFiles": files},
            "contentId": "sha256:" + sha256_hex(content), "dropped": len(units) - len(read_units)}


def normalized_hint(hint):
    return {"adapter": hint["adapter"],
            "units": {p: {"reads": sorted(u["reads"]), "dirs": sorted(u["dirs"])} for p, u in hint["units"].items()},
            "unitFiles": sorted(hint["unitFiles"])}


def check_unit_hints(path, corpus=None):
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    failures = 0
    if corpus:
        os.makedirs(corpus, exist_ok=True)
    for vector in data["vectors"]:
        name, expect = vector["name"], vector["expect"]
        key = hint_key(vector["sources"])
        shape = shape_digest(vector["sources"])
        content = hint_content(vector["hint"], key)
        raw = base64.b64decode(vector["bytes"])
        got = {
            "key": key.hex(),
            "shape": shape,
            "address": hint_address(vector["checkId"], shape),
            "tokens": {p: hint_token(key, p).hex() for p in expect["tokens"]},
            "content": content.hex(),
            "contentId": "sha256:" + sha256_hex(content),
        }
        want = {k: expect[k] for k in got}
        try:
            read = hint_read(raw, key, vector["scope"])
            got["read"], got["dropped"] = read["hint"], read["dropped"]
            got["readContentId"] = read["contentId"]
            got["inflated"] = hint_inflate(raw) == content
        except Unread as err:
            got["read"] = "unread: %s" % err
        want.update(read=normalized_hint(expect["read"]), dropped=expect["dropped"],
                    readContentId=expect["contentId"], inflated=True)
        if got != want:
            print("FAIL %s: got %s, expected %s" % (name, got, want))
            failures += 1
        else:
            print("ok   %s" % name)
        if corpus:
            with open(os.path.join(corpus, "vector-%s.zlib" % name), "wb") as f:
                f.write(raw)
            with open(os.path.join(corpus, "vector-%s.content" % name), "wb") as f:
                f.write(content)
    for vector in data["unread"]:
        name = vector["name"]
        raw = base64.b64decode(vector["bytes"])
        try:
            hint_read(raw, hint_key(vector["sources"]), vector["scope"])
            print("FAIL %s: read, expected unread" % name)
            failures += 1
        except Unread as err:
            print("ok   %s (unread: %s)" % (name, err))
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("git", "dir"):
        p = sub.add_parser(name)
        if name == "git":
            p.add_argument("--repo", default=".")
            p.add_argument("--rev", default="HEAD")
        else:
            p.add_argument("path")
        p.add_argument("--include", action="append")
        p.add_argument("--exclude", action="append")
        p.add_argument("--global-exclude", action="append")
        p.add_argument("--manifest-out")
    p = sub.add_parser("vectors")
    p.add_argument("file")
    p.add_argument("--update", action="store_true")
    p = sub.add_parser("globs")
    p.add_argument("file")
    p = sub.add_parser("unit-keys")
    p.add_argument("file")
    p = sub.add_parser("unit-hints")
    p.add_argument("file")
    p.add_argument("--corpus")
    args = parser.parse_args()

    if args.command == "vectors":
        return 1 if check_vectors(args.file, args.update) else 0
    if args.command == "globs":
        return 1 if check_globs(args.file) else 0
    if args.command == "unit-keys":
        return 1 if check_unit_keys(args.file) else 0
    if args.command == "unit-hints":
        return 1 if check_unit_hints(args.file, args.corpus) else 0
    try:
        scope = Scope(args.include, args.exclude, args.global_exclude)
        entries = git_entries(args.repo, args.rev) if args.command == "git" else dir_entries(args.path)
        m = manifest(entries, scope)
    except (DigestError, PatternError) as err:
        print("error: %s" % err, file=sys.stderr)
        return 1
    if args.manifest_out:
        with open(args.manifest_out, "wb") as f:
            f.write(m)
    print(digest(m))
    return 0


if __name__ == "__main__":
    sys.exit(main())
