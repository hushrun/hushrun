"""check-tree.py: refuses a tree that could leak a secret, or anything private (ADR 0064).

    python3 scripts/check-tree.py [--private] DIR
    python3 scripts/check-tree.py [--private] --log RANGE

This repository is public, so whatever is pushed to it is public at once. With DIR, it checks the
files git tracks there (every file, when DIR isn't a git work tree): no file over 2 MB, nothing
shaped like a secret outside the test fixtures, and every relative link in Markdown resolving
inside DIR. With --log, it checks the commits git log lists for RANGE (such as origin/main..HEAD,
or "main --not --remotes=origin" as one argument): each author's
and committer's email is a hushrun.com or GitHub noreply address, and no message holds a secret.

--private adds the maintainers' private denylist, a file of terms that must never appear here,
which lives outside this repository: HUSHRUN_PUBLIC_DENYLIST names it. CI runs without it; the
maintainers' pre-push hook (scripts/hooks/pre-push) and release script run with it.

Exits 1 with every finding, 0 when clean.
"""

import base64
import os
import re
import subprocess
import sys
from pathlib import Path

MAX_BYTES = 2 * 1024 * 1024

# Private keys and tokens that may appear, because they are test fixtures: path prefix -> reason.
ALLOWED_KEYS = {
    "testdata/keys/": "test keys, labelled as such in its README",
}

SECRETS = [
    ("GitHub token", re.compile(rb"\b(ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36}\b")),
    ("GitHub fine-grained token", re.compile(rb"\bgithub_pat_[A-Za-z0-9_]{60,}")),
    ("npm token", re.compile(rb"\bnpm_[A-Za-z0-9]{36}\b")),
    ("AWS access key", re.compile(rb"\bAKIA[0-9A-Z]{16}\b")),
    ("Google API key", re.compile(rb"\bAIza[0-9A-Za-z_-]{35}\b")),
    ("Slack token", re.compile(rb"\bxox[abprs]-[0-9A-Za-z-]{10,}")),
    ("Stripe live key", re.compile(rb"\b[sr]k_live_[0-9A-Za-z]{10,}")),
    ("Google service account", re.compile(rb'"type":\s*"service_account"')),
    # A real private key's body runs to hundreds of characters; the code's stubs are short.
    ("private key", re.compile(rb"-----BEGIN [A-Z ]*PRIVATE KEY-----(?:\\n|\s)+[A-Za-z0-9+/=\\\s]{200,}")),
]

LINK = re.compile(r"\[[^\]\n]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")

# The addresses a public commit may carry: the project's own, and GitHub's noreply ones.
EMAIL_OK = re.compile(r"(@hushrun\.com|@users\.noreply\.github\.com|^noreply@github\.com)$", re.IGNORECASE)


def denylist():
    name = os.environ.get("HUSHRUN_PUBLIC_DENYLIST")
    if not name:
        sys.exit("check-tree.py: --private needs HUSHRUN_PUBLIC_DENYLIST, the denylist's path")
    path = Path(name).expanduser()
    if not path.is_file():
        sys.exit(f"check-tree.py: no denylist at {path}: refusing to check without it")
    rules = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            rules.append((line, re.compile(line.encode(), re.IGNORECASE)))
    if not rules:
        sys.exit(f"check-tree.py: {path} has no rules")
    return rules


def blobs(data):
    """The base64 blobs in data, decoded: a name can hide in a certificate or a token."""
    for m in re.finditer(rb"[A-Za-z0-9+/]{120,}={0,2}", data):
        try:
            yield base64.b64decode(m.group(0) + b"=" * (-len(m.group(0)) % 4))
        except ValueError:
            pass


def tracked(out):
    """The files to check: those git tracks in out, or every file when out isn't a work tree's root."""
    try:
        top = subprocess.run(
            ["git", "-C", str(out), "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        if Path(top).resolve() == out:
            listed = subprocess.run(
                ["git", "-C", str(out), "ls-files", "-z"], capture_output=True, check=True
            ).stdout.decode("utf-8")
            return sorted(out / p for p in listed.split("\0") if p and (out / p).is_file())
    except (OSError, subprocess.CalledProcessError):
        pass
    return sorted(p for p in out.rglob("*") if p.is_file() and ".git" not in p.relative_to(out).parts)


def check_tree(out, rules):
    findings = []
    files = tracked(out)
    for p in files:
        rel = p.relative_to(out).as_posix()
        data = p.read_bytes()
        if len(data) > MAX_BYTES:
            findings.append(f"{rel}: {len(data)} bytes, over {MAX_BYTES}")
        for text, rule in rules:
            if rule.search(rel.encode()):
                findings.append(f"{rel}: its path matches the denylist ({text})")
            m = rule.search(data)
            if m:
                line = data.count(b"\n", 0, m.start()) + 1
                findings.append(f"{rel}:{line}: matches the denylist ({text})")
            elif any(rule.search(b) for b in blobs(data)):
                findings.append(f"{rel}: a base64 blob matches the denylist ({text})")
        if not any(rel.startswith(prefix) for prefix in ALLOWED_KEYS):
            for name, pattern in SECRETS:
                m = pattern.search(data)
                if m:
                    line = data.count(b"\n", 0, m.start()) + 1
                    findings.append(f"{rel}:{line}: looks like a {name}")
        # The notices quote other projects' license texts, links to their own files included.
        if rel.endswith(".md") and rel != "THIRD-PARTY-NOTICES.md":
            text = data.decode("utf-8", "replace")
            for m in LINK.finditer(text):
                target = m.group(1)
                if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("#"):
                    continue
                target = target.split("#", 1)[0]
                if not target:
                    continue
                dest = (p.parent / target).resolve()
                line = text.count("\n", 0, m.start()) + 1
                if out not in dest.parents and dest != out:
                    findings.append(f"{rel}:{line}: link {target} leaves the repository")
                elif not dest.exists():
                    findings.append(f"{rel}:{line}: link {target} goes nowhere")
    return findings, f"{len(files)} files"


def check_log(spec, rules):
    raw = subprocess.run(
        ["git", "log", "--format=%H%x1f%ae%x1f%ce%x1f%B%x1e", *spec.split()],
        capture_output=True, text=True, check=True,
    ).stdout
    findings = []
    commits = [c for c in raw.split("\x1e") if c.strip()]
    for entry in commits:
        sha, author, committer, message = entry.strip("\n").split("\x1f", 3)
        short = sha[:12]
        for role, email in (("author", author), ("committer", committer)):
            if not EMAIL_OK.search(email):
                findings.append(f"{short}: its {role}'s email isn't a hushrun.com or GitHub noreply address")
        data = message.encode()
        for text, rule in rules:
            if rule.search(data) or any(rule.search(b) for b in blobs(data)):
                findings.append(f"{short}: its message matches the denylist ({text})")
        for name, pattern in SECRETS:
            if pattern.search(data):
                findings.append(f"{short}: its message looks like it holds a {name}")
    return findings, f"{len(commits)} commits"


def main():
    args = sys.argv[1:]
    private = "--private" in args
    args = [a for a in args if a != "--private"]
    if len(args) == 2 and args[0] == "--log":
        check, target, where = check_log, args[1], args[1]
    elif len(args) == 1 and not args[0].startswith("-"):
        check, target = check_tree, Path(args[0]).resolve()
        where = str(target)
    else:
        sys.exit("usage: python3 scripts/check-tree.py [--private] DIR | [--private] --log RANGE")
    findings, what = check(target, denylist() if private else [])
    if findings:
        print(f"check-tree.py: {len(findings)} finding(s) in {where}:", file=sys.stderr)
        for f in findings:
            print(f"  {f}", file=sys.stderr)
        sys.exit(1)
    extra = ", nothing on the denylist" if private else ""
    last = "every address allowed" if check is check_log else "every link resolves"
    print(f"check-tree.py: {what}, no secrets{extra}, {last}")


if __name__ == "__main__":
    main()
