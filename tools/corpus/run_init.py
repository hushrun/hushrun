"""Run `hushrun init --dry-run --no-browser --format json` on a copy of each checkout.

Usage: python3 -I run_init.py --hushrun PATH --corpus corpus.json --repos DIR --out DIR
                               [--only OWNER/NAME,...] [--jobs 8] [--timeout 180]
                               [--edits FILE.json]

Each run gets a fresh copy of the checkout, an empty HOME and XDG directories, no hushrun
credentials, no CI variables and no update check: a dry run signs in to nothing, writes nothing,
installs nothing and sends nothing (C-15, C-16). OUT/<owner>__<name>.json keeps the exit code,
stdout, stderr and how long it took; `plan` holds stdout as JSON when it is a
`hushrun.init-plan/v1`. A hushrun from before that plan prints its text whatever --format says,
and score.py reads that text instead.

With --edits, each copy first gets the edits a previous run's plans hold (A6: init run again on
its own edits proposes none). Their placeholder backend becomes a valid URL first, since a signed
out dry run names no org.
"""
import argparse
import concurrent.futures
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PLACEHOLDER = "<your org's URL, once you sign in>"
STAND_IN = "https://api.hushrun.com/o/corpus"


def key(repo: str, commit: str) -> str:
    return f"{repo.replace('/', '__')}@{commit[:12]}"


def clean_env(home: Path) -> dict:
    """Only what a run needs: PATH, a locale and an empty home. Nothing of the machine's CI,
    hushrun or git configuration reaches it."""
    for sub in (".config", ".cache", ".local/share"):
        (home / sub).mkdir(parents=True, exist_ok=True)
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "XDG_DATA_HOME": str(home / ".local/share"),
        "HUSHRUN_NO_UPDATE_CHECK": "1",
        "HUSHRUN_CREDENTIALS": str(home / "no-credentials.json"),
        "HUSHRUN_CACHE_DIR": str(home / ".cache/hushrun"),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        # A checkout is blobless: git never fetches a missing blob from GitHub during a run, so
        # a run reads only what was fetched, offline, every time.
        "GIT_NO_LAZY_FETCH": "1",
        "LC_ALL": "C",
        "TERM": "dumb",
    }


def apply_edits(repo: Path, plan: dict) -> list:
    """Applies a plan's edits with `git apply`: the paths that didn't apply."""
    failed = []
    for edit in plan.get("edits", []):
        diff = edit["diff"].replace(PLACEHOLDER, STAND_IN)
        out = subprocess.run(["git", "apply", "--whitespace=nowarn", "-"], cwd=repo, input=diff,
                             text=True, capture_output=True, check=False,
                             env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                                  "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull})
        if out.returncode != 0:
            failed.append({"path": edit["path"], "error": out.stderr.strip()[-300:]})
    return failed


def run_one(hushrun: Path, entry: dict, repos: Path, out_dir: Path, timeout: int, edits) -> str:
    repo, commit = entry["repo"], entry["commit"]
    src = repos / key(repo, commit)
    if not (src / ".git" / "corpus-done").exists():
        raise RuntimeError(f"{src} isn't fetched: run fetch.py first")
    with tempfile.TemporaryDirectory(prefix="corpus-init-") as tmp:
        tmp = Path(tmp)
        work = tmp / "repo"
        shutil.copytree(src, work, symlinks=True)
        applied = None
        if edits is not None:
            applied = apply_edits(work, edits.get(repo) or {})
        env = clean_env(tmp / "home")
        started = time.monotonic()
        timed_out = False
        try:
            done = subprocess.run(
                [str(hushrun), "init", "--dry-run", "--no-browser", "--format", "json"],
                cwd=work, env=env, stdin=subprocess.DEVNULL, capture_output=True,
                timeout=timeout, check=False)
            code, stdout, stderr = done.returncode, done.stdout, done.stderr
        except subprocess.TimeoutExpired as expired:
            timed_out = True
            code, stdout, stderr = None, expired.stdout or b"", expired.stderr or b""
        seconds = round(time.monotonic() - started, 3)
    stdout = stdout.decode("utf-8", "replace")
    stderr = stderr.decode("utf-8", "replace")
    plan = None
    try:
        doc = json.loads(stdout)
        if isinstance(doc, dict) and doc.get("schema") == "hushrun.init-plan/v1":
            plan = doc
    except ValueError:
        pass
    record = {"repo": repo, "commit": commit, "exit": code, "timedOut": timed_out,
              "seconds": seconds, "plan": plan, "stdout": None if plan else stdout,
              "stderr": stderr.replace(str(tmp), "<tmp>")}
    if applied is not None:
        record["editsNotApplied"] = applied
    (out_dir / f"{repo.replace('/', '__')}.json").write_text(json.dumps(record, indent=1) + "\n")
    return f"{repo}: exit {code} in {seconds:.1f}s{' (JSON)' if plan else ''}"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--hushrun", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--repos", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--only")
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--edits", type=Path,
                        help="a directory of earlier runs whose plans' edits each copy gets first")
    args = parser.parse_args()
    hushrun = args.hushrun.resolve()
    entries = json.loads(args.corpus.read_text())["repositories"]
    if args.only:
        wanted = set(args.only.split(","))
        entries = [e for e in entries if e["repo"] in wanted]
    edits = None
    if args.edits:
        edits = {}
        for path in args.edits.glob("*.json"):
            record = json.loads(path.read_text())
            if record.get("plan"):
                edits[record["repo"]] = record["plan"]
    args.out.mkdir(parents=True, exist_ok=True)
    failed = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = {pool.submit(run_one, hushrun, e, args.repos, args.out, args.timeout, edits): e
                   for e in entries}
        for future in concurrent.futures.as_completed(futures):
            try:
                print(future.result(), flush=True)
            except Exception as err:  # noqa: BLE001
                failed += 1
                print(f"FAILED {futures[future]['repo']}: {err}", file=sys.stderr, flush=True)
    if failed:
        sys.exit(f"{failed} runs failed")


if __name__ == "__main__":
    main()
