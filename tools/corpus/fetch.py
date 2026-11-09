"""Fetch the corpus: a partial, sparse checkout of each repository at its pinned commit.

Usage:
  python3 -I fetch.py fetch --corpus corpus.json --dir DIR [--only OWNER/NAME,...] [--jobs 8] [--refresh]
  python3 -I fetch.py resolve --survey repos.csv --out corpus.json [--jobs 8]
  python3 -I fetch.py repin --corpus corpus.json [--only OWNER/NAME,...] [--jobs 8]

`fetch` makes DIR/<owner>__<name>@<sha12>/ for each repository: `git init`, the remote, then
`git fetch --depth 1 --filter=blob:none origin <commit>`, sparse-checkout patterns from
select_paths.py over `git ls-tree -r --name-only <commit>`, and a checkout of the commit on the
repository's default branch, as a clone would leave it. A checkout that finished is kept, unless
--refresh. The full file list is kept in `.git/corpus-files.txt`, outside the working tree.

`resolve` turns the survey's 12-character commits into full ones, once: the survey kept only
those. `repin` moves every commit to its default branch's head, on purpose: it invalidates the
baseline.

Nothing here runs repository code: git only reads objects, and the repository brings no hooks or
config. Checkouts are untrusted data, so the scripts that read them take their paths as arguments.
"""
import argparse
import concurrent.futures
import csv
import datetime
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
SELECT = HERE / "select_paths.py"
# Each repository's fetch gets this long: the largest (LLVM, Chromium-sized trees) take a minute.
TIMEOUT = 900
DONE = "corpus-done"
FILES = "corpus-files.txt"


def git(*args, cwd=None, timeout=TIMEOUT, input_text=None):
    """git with no configuration of the machine's or the repository's, and no prompts."""
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": os.environ.get("HOME", "/nonexistent"),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        "LC_ALL": "C",
    }
    return subprocess.run(
        ["git", "-c", "advice.detachedHead=false", "-c", "core.hooksPath=/dev/null", *args],
        cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout, input=input_text,
        check=False)


def key(repo: str, commit: str) -> str:
    return f"{repo.replace('/', '__')}@{commit[:12]}"


def url(repo: str) -> str:
    return f"https://github.com/{repo}.git"


def default_branch(repo: str):
    """The default branch and its head, from `git ls-remote --symref`."""
    out = git("ls-remote", "--symref", url(repo), "HEAD", timeout=120)
    if out.returncode != 0:
        raise RuntimeError(f"ls-remote: {out.stderr.strip()[-300:]}")
    branch, head = None, None
    for line in out.stdout.splitlines():
        if line.startswith("ref: ") and line.endswith("\tHEAD"):
            branch = line[len("ref: "):-len("\tHEAD")].removeprefix("refs/heads/")
        elif line.endswith("\tHEAD"):
            head = line.split("\t")[0]
    if not branch or not head:
        raise RuntimeError(f"ls-remote named no default branch: {out.stdout[:200]}")
    return branch, head


def resolve_one(row: dict) -> dict:
    """The survey's row with its full commit and default branch."""
    repo, prefix = row["repo"], row["commit"]
    branch, head = default_branch(repo)
    if head.startswith(prefix):
        return {"repo": repo, "commit": head, "branch": branch, "language": row["language_group"]}
    since = datetime.date.fromisoformat(row["commit_date"][:10]) - datetime.timedelta(days=7)
    with tempfile.TemporaryDirectory(prefix="corpus-resolve-") as tmp:
        git("init", "-q", tmp)
        # Commits only, no trees or blobs: enough to find a prefix among them. A commit date can
        # be older than its place in history (git/git's are), so a depth backs the date up.
        for depth in (f"--shallow-since={since.isoformat()}", "--depth=500", "--depth=5000"):
            out = git("fetch", "-q", "--filter=tree:0", depth, url(repo),
                      f"refs/heads/{branch}", cwd=tmp)
            if out.returncode != 0:
                raise RuntimeError(f"fetch for {prefix}: {out.stderr.strip()[-300:]}")
            found = git("rev-parse", "--verify", "--quiet", f"{prefix}^{{commit}}", cwd=tmp)
            if found.returncode == 0:
                return {"repo": repo, "commit": found.stdout.strip(), "branch": branch,
                        "language": row["language_group"]}
        raise RuntimeError(f"{prefix} isn't among {branch}'s last 5,000 commits")


def fetch_one(entry: dict, base: Path, refresh: bool) -> str:
    repo, commit, branch = entry["repo"], entry["commit"], entry["branch"]
    dest = base / key(repo, commit)
    if (dest / ".git" / DONE).exists() and not refresh:
        return f"kept {repo}"
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    steps = [
        ("init", "-q", "-b", branch),
        ("remote", "add", "origin", url(repo)),
        ("fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", commit),
    ]
    for step in steps:
        out = git(*step, cwd=dest)
        if out.returncode != 0:
            raise RuntimeError(f"git {step[0]}: {out.stderr.strip()[-300:]}")
    listed = git("ls-tree", "-r", "--name-only", "-z", commit, cwd=dest)
    if listed.returncode != 0:
        raise RuntimeError(f"ls-tree: {listed.stderr.strip()[-300:]}")
    files = [p for p in listed.stdout.split("\0") if p]
    (dest / ".git" / FILES).write_text("\n".join(files) + "\n", encoding="utf-8")
    picked = subprocess.run([sys.executable, "-I", str(SELECT), str(dest / ".git" / FILES)],
                            capture_output=True, text=True, check=True).stdout
    for step in [
        ("config", "core.sparseCheckout", "true"),
        ("sparse-checkout", "set", "--no-cone", "--stdin"),
    ]:
        out = git(*step, cwd=dest, input_text=picked if step[0] == "sparse-checkout" else None)
        if out.returncode != 0:
            raise RuntimeError(f"git {' '.join(step[:2])}: {out.stderr.strip()[-300:]}")
    # As a clone leaves it: the default branch checked out, and origin's refs beside it.
    for step in [
        ("update-ref", f"refs/remotes/origin/{branch}", commit),
        ("symbolic-ref", "refs/remotes/origin/HEAD", f"refs/remotes/origin/{branch}"),
        ("reset", "-q", "--hard", commit),
        ("branch", "-q", f"--set-upstream-to=origin/{branch}"),
    ]:
        out = git(*step, cwd=dest)
        if out.returncode != 0:
            raise RuntimeError(f"git {step[0]}: {out.stderr.strip()[-300:]}")
    (dest / ".git" / DONE).write_text(commit + "\n")
    got = sum(1 for p in dest.rglob("*") if p.is_file() and ".git" not in p.relative_to(dest).parts)
    return f"fetched {repo} ({got} of {len(files)} files)"


def load(path: Path, only):
    entries = json.loads(path.read_text())["repositories"]
    if only:
        wanted = set(only.split(","))
        missing = wanted - {e["repo"] for e in entries}
        if missing:
            sys.exit(f"not in the corpus: {', '.join(sorted(missing))}")
        entries = [e for e in entries if e["repo"] in wanted]
    return entries


def write_corpus(path: Path, entries):
    doc = {
        "about": "The survey's 100 repositories at pinned commits: python3 -I fetch.py repin moves them on purpose, and invalidates baseline.json.",
        "repositories": entries,
    }
    path.write_text(json.dumps(doc, indent=1) + "\n")


def parallel(fn, items, jobs):
    results, failed = {}, []
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = {pool.submit(fn, item): item for item in items}
        for future in concurrent.futures.as_completed(futures):
            item = futures[future]
            name = item["repo"]
            try:
                results[name] = future.result()
                if isinstance(results[name], str):
                    print(results[name], flush=True)
            except Exception as err:  # noqa: BLE001
                failed.append(name)
                print(f"FAILED {name}: {err}", file=sys.stderr, flush=True)
    return results, failed


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fetch")
    f.add_argument("--corpus", type=Path, required=True)
    f.add_argument("--dir", type=Path, required=True)
    f.add_argument("--only")
    f.add_argument("--jobs", type=int, default=8)
    f.add_argument("--refresh", action="store_true")
    r = sub.add_parser("resolve")
    r.add_argument("--survey", type=Path, required=True)
    r.add_argument("--out", type=Path, required=True)
    r.add_argument("--jobs", type=int, default=8)
    p = sub.add_parser("repin")
    p.add_argument("--corpus", type=Path, required=True)
    p.add_argument("--only")
    p.add_argument("--jobs", type=int, default=8)
    args = parser.parse_args()

    if args.cmd == "resolve":
        rows = list(csv.DictReader(args.survey.open(encoding="utf-8")))
        results, failed = parallel(resolve_one, rows, args.jobs)
        if failed:
            sys.exit(f"{len(failed)} repositories didn't resolve: {', '.join(sorted(failed))}")
        write_corpus(args.out, [results[row["repo"]] for row in rows])
        print(f"wrote {args.out}: {len(rows)} repositories")
    elif args.cmd == "repin":
        entries = json.loads(args.corpus.read_text())["repositories"]
        wanted = set(args.only.split(",")) if args.only else {e["repo"] for e in entries}

        def repin(entry):
            branch, head = default_branch(entry["repo"])
            return {**entry, "commit": head, "branch": branch}

        results, failed = parallel(repin, [e for e in entries if e["repo"] in wanted], args.jobs)
        if failed:
            sys.exit(f"{len(failed)} repositories didn't repin: {', '.join(sorted(failed))}")
        write_corpus(args.corpus, [results.get(e["repo"], e) for e in entries])
        print(f"repinned {len(results)}: baseline.json no longer applies; run with --update-baseline")
    else:
        args.dir.mkdir(parents=True, exist_ok=True)
        entries = load(args.corpus, args.only)
        _, failed = parallel(lambda e: fetch_one(e, args.dir, args.refresh), entries, args.jobs)
        if failed:
            sys.exit(f"{len(failed)} of {len(entries)} repositories failed to fetch")
        print(f"{len(entries)} repositories in {args.dir}")


if __name__ == "__main__":
    main()
