"""Score the corpus: join the analyzer's facts with init's plans, give each test job one outcome,
check A1 to A10, and write the scoreboard.

Usage: python3 -I score.py --corpus corpus.json --repos DIR --facts facts.json --init DIR
                           --out DIR [--again DIR] [--baseline baseline.json] [--known known.json]
                           [--only OWNER/NAME,...] [--update-baseline]

--init holds run_init.py's records of a first run; --again those of a run on the checkouts with
the first run's edits applied (A6). Writes OUT/scoreboard.json and OUT/scoreboard.md, and, with
--update-baseline, the baseline. Exits 1 when an assertion fails, or a job loses its skip against
the baseline, unless known.json lists the case with a reason.

A plan is init's JSON, `hushrun.init-plan/v1`. A hushrun from before it prints text, which this
reads as the survey did (its aggregate.py), so that the survey's numbers can be checked: such a run
has walls, the survey's view, and outcomes only as far as text can tell them.
"""
import argparse
import collections
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import yaml

# The harness never writes into the source tree.
sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import analyze  # noqa: E402  (its YAML loader, which keeps `on` a key)

# What the survey counted as a test command or check: its aggregate.py and export.py.
TESTY = re.compile(r"test|spec|pytest|nextest|ctest|check-|unittest|rspec|phpunit|e2e", re.I)
JS = {"js-pm-script", "jest", "vitest", "mocha", "playwright", "cypress", "turbo", "nx", "lerna",
      "node-test", "bun-test", "deno-test"}
PER_CHANGE = ("push", "pull_request", "merge_group", "pull_request_target")

# The survey's walls for hushrun 0.6.0, the reference the harness was checked against.
SURVEY = {"skippable": 96, "matrix": 263, "unrecognized": 219, "reusable": 113, "js": 107, "left": 25}
WALLS = {
    "matrix": "matrix job: no skip in CI",
    "reusable": "reusable workflow job: no skip in CI",
    "skippable": "wrapped by init: can skip in CI",
    "left": "init leaves its command",
    "js": "JS script or tool init does not wrap",
    "unrecognized": "command init does not recognize",
}
OUTCOMES = ["skips", "runs:reports", "runs:variants", "runs:reusable", "runs:cant-tell",
            "stepped-aside", "left:*", "js-not-wrapped", "unseen"]
ASSERTIONS = {
    "A1": "reports: a wrapped command beside a reader runs, and the two catalogs agree",
    "A2": "variants: no job with several variants predicts a skip without identity",
    "A3": "reusable: no job of a called workflow predicts a skip without identity",
    "A4": "secrets: no wrapped command holds a secret",
    "A5": "valid edits: the edited files load, and keep every job, step and key",
    "A6": "idempotent: init run again on its own edits proposes none",
    "A7": "no crash: exit 0 within 60 s, no panic",
    "A8": "no regression: no job loses its skip against the baseline",
    "A9": "prediction equals decision",
    "A10": "workspace graph: each package's scope holds its workspace dependencies",
}


# ---------------------------------------------------------------- init's text (0.6.0 and before)

def parse_init_text(text: str):
    """The survey's parse_init (aggregate.py), unchanged."""
    out = {"wrapped_scripts": [], "script_jobs": [], "wrapped_ci": [], "left_ci": [], "left_scripts": [],
           "summary": None, "one_scope_reason": None, "scopes": 0}
    lines = text.splitlines()
    sec, cur_file = None, None
    for i, l in enumerate(lines):
        if l.startswith("  workflows   ") or (sec == "hdr_wf" and l.startswith("              ")):
            sec = "hdr_wf"
            m = re.match(r"\s+(?:workflows\s+)?(\.github/workflows/\S+|\.gitlab-ci\.yml) runs (.+) \(jobs? (.+)\)$", l)
            if m:
                for jb in re.split(r", | and ", m.group(3)):
                    out["script_jobs"].append({"workflow": m.group(1), "checks": m.group(2), "job": jb.strip()})
            continue
        if l.startswith("There is no package.json") or l.startswith("No script in package.json"):
            out["summary"] = l.split(",")[0] if "init knows" not in l else "no package.json and no CI check init knows"
            if "can't wrap the checks" in l:
                out["summary"] = "no package.json; CI checks known but none wrappable"
            if l.startswith("No script"):
                out["summary"] = "no package.json script wrappable"
        if l.startswith("One scope for the whole repository"):
            out["one_scope_reason"] = l
        if re.match(r"^  scope \w", l) or "scope " in l and l.strip().startswith("scope "):
            out["scopes"] += 1
        if l == "hushrun init will change:":
            sec = "change"
            continue
        if l.startswith("It leaves these commands in CI's files"):
            sec = "left_ci"
            continue
        if l.startswith("It leaves these scripts"):
            sec = "left_scripts"
            continue
        if l.startswith("Nothing changed") or l.startswith("One scope"):
            sec = None
            continue
        if sec == "change":
            if re.match(r"^  \S", l):
                cur_file = l.strip()
                continue
            if cur_file == "package.json":
                m = re.match(r'^\s+"([^"]+)": "(.*)" → "(.*)"', l)
                if m:
                    out["wrapped_scripts"].append({"script": m.group(1), "old": m.group(2)})
            elif cur_file and (cur_file.startswith(".github/workflows/") or cur_file == ".gitlab-ci.yml"):
                m = re.match(r"^\s+job ([^:]+): (.+) → hushrun run (?:--scope \S+ )?(\S+) -- ", l)
                if m:
                    out["wrapped_ci"].append({"workflow": cur_file, "job": m.group(1), "cmd": m.group(2), "check": m.group(3)})
        elif sec == "left_ci":
            m = re.match(r"^\s+(\S+), job ([^:]+): (.*)$", l)
            if m:
                rest = m.group(3)
                reasons = ["its job has several variants", "the shell makes its words what they are",
                           "its answer changes with an advisory database", "it writes a report", "it sets ",
                           "it uses ||", "it uses a subshell", "it redirects", "it pipes", "a `cd` before it",
                           "it runs commands one after another", "it watches", "it uses",
                           "it passes a secret", "an earlier step sets", "a command fills in its words"]
                idx = -1
                for rs in reasons:
                    k = rest.find(": " + rs)
                    if k != -1 and (idx == -1 or k < idx):
                        idx = k
                cmd, reason = (rest[:idx], rest[idx + 2:]) if idx != -1 else (rest, "?")
                out["left_ci"].append({"workflow": m.group(1), "job": m.group(2), "cmd": cmd, "reason": reason})
        elif sec == "left_scripts":
            m = re.match(r'^\s+"([^"]+)": (.*)$', l)
            if m:
                out["left_scripts"].append({"script": m.group(1), "reason": m.group(2)})
    return out


# ---------------------------------------------------------------- init's plan (JSON)

def decisions_of_plan(plan: dict):
    """What init does to each job, in the same shape as the text's, with its predictions. A left
    command or script whose `ciWhy` is `stepped-aside` runs through a tool whose remote cache
    keeps its results (Phase 4)."""
    out = {"wrapped_ci": [], "script_jobs": [], "left_ci": [], "plan_jobs": {},
           "aside_scripts": {s["script"] for s in plan["scripts"]["left"]
                             if s.get("ciWhy") == "stepped-aside"}}
    for job in plan["jobs"]:
        key = (job["file"], job["job"])
        out["plan_jobs"][key] = job
        for c in job["commands"]:
            if c["status"] == "wrapped":
                out["wrapped_ci"].append({"workflow": job["file"], "job": job["job"], "cmd": c["before"],
                                          "check": c["check"], "ci": c["ci"], "ciWhy": c["ciWhy"],
                                          "ciText": c["ciText"], "after": c["after"]})
            elif c["status"] == "left":
                out["left_ci"].append({"workflow": job["file"], "job": job["job"], "cmd": c["before"],
                                       "reason": c["why"], "ciWhy": c.get("ciWhy")})
        for s in job["scripts"]:
            out["script_jobs"].append({"workflow": job["file"], "job": job["job"], "checks": s["check"],
                                       "ci": s["ci"], "ciWhy": s["ciWhy"], "ciText": s["ciText"]})
    return out


# A package manager running a package.json script: `npm test`, `pnpm run lint`, `bun run test`.
PM_SCRIPT = re.compile(r"^(?:\w+=\S+\s+)*(?:(?:npm|pnpm|yarn)\s+(?:run(?:-script)?\s+)?|bun\s+run\s+)([\w:.-]+)")


def script_of(line: str):
    """The package.json script a CI line runs through a package manager, if it runs one."""
    m = PM_SCRIPT.match(line.strip())
    if not m:
        return None
    return "test" if m.group(1) == "t" else m.group(1)


def reason_slug(reason: str) -> str:
    """A left command's reason, as `left:<slug>` names it."""
    for prefix, slug in [
        ("its job has several variants", "variants"), ("its job has several instances", "variants"),
        ("the shell makes its words", "dollar-words"), ("it writes a report", "writes-report"),
        ("it sets ", "var-prefix"), ("its answer changes with an advisory", "advisory"),
        ("a `cd`", "cd"), ("it watches", "watches"),
        ("it uses", "shell-operator"), ("it redirects", "shell-operator"), ("it pipes", "shell-operator"),
        ("it runs commands one after another", "shell-operator"),
        # [ADR 0050] What may put a secret in a result's record.
        ("it passes a secret", "secret"), ("an earlier step sets", "unknown-variable"),
        ("a command fills in its words", "substitution"),
        # [ADR 0055 item 6] A RUSTFLAGS or RUSTDOCFLAGS CI sets that init can't carry into the line.
        ("its job sets ", "flags"), ("an earlier step may set ", "flags"),
    ]:
        if reason.startswith(prefix):
            return slug
    return "other"


# ---------------------------------------------------------------- outcomes

def wrapped_test_jobs(dec: dict):
    """The jobs where a wrapped test command or test script runs, as the survey found them."""
    found = collections.defaultdict(list)
    for w in dec["wrapped_ci"]:
        if TESTY.search(w["check"] or "") or TESTY.search(w["cmd"] or ""):
            found[(w["workflow"], w["job"])].append(w)
    for sj in dec["script_jobs"]:
        checks = [c.strip() for c in sj["checks"].split(",")]
        if any(TESTY.search(c) for c in checks):
            found[(sj["workflow"], sj["job"])].append(sj)
    return found


def wall_of(job, wrapped, left, families):
    """The survey's category (export.py's waterfall_cat)."""
    if job["matrix_multi"]:
        return "matrix"
    if job["in_reusable"]:
        return "reusable"
    if wrapped:
        return "skippable"
    if left:
        return "left"
    if families & JS:
        return "js"
    return "unrecognized"


def outcome_of(job, wrapped, left, families, mode, aside_script=False):
    """The plan's one outcome for a test job. `aside_script` says the job runs a package.json
    script init leaves to its tool's remote cache."""
    if wrapped:
        if mode == "text":
            # Text predicts nothing: the survey's reading of what 0.6.0 left to the run.
            if job["matrix_multi"]:
                return "runs:variants"
            if job["in_reusable"]:
                return "runs:reusable"
            return "skips"
        runs = [w for w in wrapped if w.get("ci") == "runs"]
        if not runs:
            return "skips"
        why = runs[0]["ciWhy"]
        return "stepped-aside" if why == "stepped-aside" else f"runs:{why}"
    if left:
        if left[0].get("ciWhy") == "stepped-aside":
            return "stepped-aside"
        return f"left:{reason_slug(left[0]['reason'])}"
    if aside_script:
        return "stepped-aside"
    if families & JS:
        return "js-not-wrapped"
    return "unseen"


def shape(family: str, line: str) -> str:
    words = line.split()
    while words and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", words[0]):
        words.pop(0)
    words = [re.sub(r"\$\{\{[^}]*\}\}", "${{…}}", w) for w in words]
    return f"{family}: {' '.join(words[:2])}"


# ---------------------------------------------------------------- A5: edits keep the files whole

SETUP = re.compile(r"^hushrun/hushrun/action/setup@")
# [ADR 0055 item 6] A `RUSTFLAGS` or `RUSTDOCFLAGS` CI sets, carried into a cargo line through
# `env`, as one shell word: bare, or between `'`s or `"`s.
CARRIED = r"""(?:'RUST(?:DOC)?FLAGS=[^']*'|"RUST(?:DOC)?FLAGS=(?:[^"\\]|\\.)*"|RUST(?:DOC)?FLAGS=[^\s'"]*) """
# The line's own `NAME=value` words, up to its cargo.
OWN_THEN_CARGO = r"""(?=(?:[A-Za-z_]\w*=(?:'[^']*'|"[^"]*"|[^\s'"])* )*cargo\b)"""
# [ADR 0050] with the `env` init puts in front of a line's own `NAME=value` words, and
# [ADR 0055 item 6] the flags it carries in front of a cargo line.
WRAP = re.compile(
    r"hushrun run (?:--scope \S+ )?[\w.:${}\-\[\]]+ -- "
    rf"(?:env (?:(?:{CARRIED})+{OWN_THEN_CARGO}|(?=[A-Za-z_]\w*=)))?"
)


def is_setup(item) -> bool:
    if isinstance(item, dict):
        return bool(SETUP.match(str(item.get("uses") or "")))
    return isinstance(item, str) and "install-hushrun.sh" in item


def init_env(path, key, value):
    """Whether `key: value`, added to the `env` (GitHub) or `variables` (GitLab) at `path`, is one
    init writes: what tells a job's variants and calls apart (ADR 0049), with the value init
    writes, or GitLab's HUSHRUN_REPORTS_READ (ADR 0047)."""
    file = path.split(".jobs.")[0]
    words = re.sub(r"\s", "", str(value)).lower()
    if path.endswith(".env"):
        return (key, words) in (("HUSHRUN_VARIANT", "${{tojson(matrix)}}"),
                                ("HUSHRUN_INPUTS", "${{tojson(inputs)}}")) \
            or (key == "HUSHRUN_WORKFLOW" and value == file)
    return path.endswith(".variables") and key == "HUSHRUN_REPORTS_READ"


# [ADR 0051] What init lists in turbo.json's globalPassThroughEnv, so that the scripts Turborepo's
# strict mode runs get hushrun's variables and CI's.
TURBO_PASSED = {"ACTIONS_ID_TOKEN_REQUEST_*", "GITHUB_*", "GITLAB_CI", "CI_*", "HUSHRUN_*"}


def jsonc_parts(text: str):
    """JSON with comments, as Turborepo reads it: its value, and its comments in order."""
    out, comments, i, n = [], [], 0, len(text)
    while i < n:
        c = text[i]
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            out.append(text[i:j + 1])
            i = j + 1
        elif text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            comments.append(text[i:j].rstrip("\r"))
            i = j
        elif text.startswith("/*", i):
            j = text.find("*/", i + 2)
            j = n if j < 0 else j + 2
            comments.append(text[i:j])
            out.append(" ")
            i = j
        else:
            out.append(c)
            i += 1
    value = json.loads(re.sub(r",(\s*[}\]])", r"\1", "".join(out)))
    return value, comments


def check_turbo(path: str, before: str, after: str, problems):
    """A5 for turbo.json: only globalPassThroughEnv grows, by hushrun's variables, after the
    entries it had, and every comment stays."""
    try:
        edited, comments = jsonc_parts(after)
    except ValueError as err:
        problems.append(f"{path}: no longer JSON: {err}")
        return
    original, old_comments = jsonc_parts(before) if before else ({}, [])
    for k in set(original) | set(edited):
        if k != "globalPassThroughEnv" and original.get(k) != edited.get(k):
            problems.append(f"{path}: {k} changed")
    old = original.get("globalPassThroughEnv") or []
    new = edited.get("globalPassThroughEnv") or []
    if new[:len(old)] != old or not set(new[len(old):]) <= TURBO_PASSED:
        problems.append(f"{path}: globalPassThroughEnv changed beyond hushrun's variables")
    if comments != old_comments:
        problems.append(f"{path}: its comments changed")


def compare(orig, edit, path, problems):
    """`edit` keeps everything of `orig`: it only adds env keys, setup steps and permissions, and
    puts hushrun run in front of run: lines."""
    if isinstance(orig, dict):
        if not isinstance(edit, dict):
            problems.append(f"{path}: no longer a mapping")
            return
        for k, v in orig.items():
            if k not in edit:
                problems.append(f"{path}.{k}: removed")
            elif k == "permissions":
                compare_permissions(v, edit[k], f"{path}.{k}", problems)
            else:
                compare(v, edit[k], f"{path}.{k}", problems)
        for k in edit:
            if k not in orig:
                if k == "permissions":
                    compare_permissions({}, edit[k], f"{path}.{k}", problems)
                elif k in ("env", "variables") and isinstance(edit[k], dict):
                    for name, value in edit[k].items():
                        if not init_env(f"{path}.{k}", name, value):
                            problems.append(f"{path}.{k}.{name}: added")
                elif not init_env(path, k, edit[k]):
                    problems.append(f"{path}.{k}: added")
    elif isinstance(orig, list):
        if not isinstance(edit, list):
            problems.append(f"{path}: no longer a list")
            return
        rest = [e for e in edit if not is_setup(e) or e in orig]
        if len(rest) != len(orig):
            problems.append(f"{path}: {len(orig)} items became {len(rest)}, setup steps aside")
            return
        for i, (a, b) in enumerate(zip(orig, rest)):
            compare(a, b, f"{path}[{i}]", problems)
    elif isinstance(orig, str) and isinstance(edit, str):
        if orig != edit and WRAP.sub("", edit) != orig:
            problems.append(f"{path}: changed beyond hushrun run")
    elif orig != edit:
        problems.append(f"{path}: changed")


def compare_permissions(orig, edit, path, problems):
    """init may add `id-token: write`, and `contents: read` beside it in a block of its own."""
    if orig == edit:
        return
    if not isinstance(edit, dict):
        if edit not in ("write-all",):
            problems.append(f"{path}: became {edit!r}")
        return
    orig = orig if isinstance(orig, dict) else {}
    for k, v in edit.items():
        if k in orig and orig[k] == v:
            continue
        if (k, v) not in (("id-token", "write"), ("contents", "read")):
            problems.append(f"{path}.{k}: {orig.get(k)!r} became {v!r}")
    for k in orig:
        if k not in edit:
            problems.append(f"{path}.{k}: removed")


def load_yaml_text(text: str):
    return yaml.load(text, Loader=analyze.Loader)


def check_edits(checkout: Path, plan: dict):
    """A5 for one repository: each edit applies, and each file still reads and keeps its parts."""
    problems = []
    with tempfile.TemporaryDirectory(prefix="corpus-edits-") as tmp:
        tmp = Path(tmp)
        for edit in plan["edits"]:
            path = edit["path"]
            target = tmp / path
            target.parent.mkdir(parents=True, exist_ok=True)
            source = checkout / path
            if source.exists():
                shutil.copyfile(source, target)
            done = subprocess.run(["git", "apply", "--unsafe-paths", "-"], cwd=tmp, input=edit["diff"],
                                  text=True, capture_output=True, check=False)
            if done.returncode != 0:
                problems.append(f"{path}: doesn't apply: {done.stderr.strip()[-200:]}")
                continue
            after = target.read_text(encoding="utf-8")
            before = source.read_text(encoding="utf-8") if source.exists() else None
            if path.endswith((".yml", ".yaml")):
                try:
                    edited = load_yaml_text(after)
                except yaml.YAMLError as err:
                    problems.append(f"{path}: no longer loads: {str(err)[:160]}")
                    continue
                # [ADR 0050] A composite action of the repository's is compared as a workflow is.
                action = path.rsplit("/", 1)[-1] in ("action.yml", "action.yaml")
                if before is not None and (path.startswith(".github/workflows/") or path == ".gitlab-ci.yml"
                                           or action):
                    try:
                        original = load_yaml_text(before)
                    except yaml.YAMLError:
                        continue  # init wraps nothing in a file that doesn't load
                    compare(original, edited, path, problems)
            elif path in ("turbo.json", "turbo.jsonc"):
                check_turbo(path, before, after, problems)
            elif path.endswith("package.json"):
                try:
                    edited = json.loads(after)
                except ValueError as err:
                    problems.append(f"{path}: no longer JSON: {err}")
                    continue
                original = json.loads(before) if before else {}
                for k in set(original) | set(edited):
                    if k in ("scripts", "hushrun", "devDependencies"):
                        continue
                    if original.get(k) != edited.get(k):
                        problems.append(f"{path}: {k} changed")
                for k, v in (original.get("devDependencies") or {}).items():
                    if (edited.get("devDependencies") or {}).get(k) != v:
                        problems.append(f"{path}: devDependencies.{k} changed")
                for k, v in (original.get("scripts") or {}).items():
                    new = (edited.get("scripts") or {}).get(k)
                    if new == v:
                        continue
                    # hushrun run in front, a bare vitest made `vitest run` (C-15), or the hook's
                    # install after a script that was there (C-24).
                    # [ADR 0051] A workspace package's script names its scope and no check.
                    unwrapped = WRAP.sub("", re.sub(r"hushrun run (?:--scope \S+ )?-- ", "", new or ""))
                    if new is None or (unwrapped != v and re.sub(r"\bvitest run\b", "vitest", unwrapped) != v
                                       and not new.startswith(v + " && (")):
                        problems.append(f"{path}: scripts.{k} changed beyond hushrun run")
    return problems


# ---------------------------------------------------------------- A10, the workspace graph

# [ADR 0051] What pnpm leaves out of a workspace, whatever its patterns say; npm, Yarn and Bun only
# node_modules.
PNPM_IGNORES = ("**/node_modules/**", "**/bower_components/**", "**/test/**", "**/tests/**")
DEPENDENCY_FIELDS = ("dependencies", "devDependencies", "peerDependencies", "optionalDependencies")


def glob_re(pattern: str):
    """A pattern as the workspace tools and hushrun's scopes read it: `*` and `?` within a
    segment, `**` across segments."""
    out = []
    parts = pattern.split("/")
    for i, part in enumerate(parts):
        last = i == len(parts) - 1
        if part == "**":
            out.append(".+" if last else "(?:.+/)?")
            continue
        segment = "".join("[^/]*" if c == "*" else "[^/]" if c == "?" else re.escape(c) for c in part)
        out.append(segment if last else segment + "/")
    return re.compile("".join(out) + r"\Z")


def braces(pattern: str):
    """`pattern` with each `{a,b}` written out."""
    m = re.search(r"\{([^{}]*)\}", pattern)
    if not m:
        return [pattern]
    return [x for choice in m.group(1).split(",")
            for x in braces(pattern[:m.start()] + choice + pattern[m.end():])]


def workspace_graph(checkout: Path):
    """The harness's own reading of the root's JavaScript workspace, for A10: each package's
    directory, and the directories of the workspace packages it depends on, transitively, by name
    in the four dependency fields. None when the root heads no workspace this reading can read."""
    try:
        root = json.loads((checkout / "package.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    pnpm = (checkout / "pnpm-workspace.yaml").exists()
    if pnpm:
        try:
            doc = yaml.safe_load((checkout / "pnpm-workspace.yaml").read_text(encoding="utf-8"))
        except yaml.YAMLError:
            return None
        patterns = doc.get("packages") if isinstance(doc, dict) else None
    elif isinstance(root.get("workspaces"), list):
        patterns = root["workspaces"]
    elif isinstance(root.get("workspaces"), dict):
        patterns = root["workspaces"].get("packages")
    elif (checkout / "lerna.json").exists():
        try:
            patterns = json.loads((checkout / "lerna.json").read_text(encoding="utf-8")).get(
                "packages", ["packages/*"])
        except ValueError:
            return None
    else:
        return None
    if not isinstance(patterns, list) or not all(isinstance(p, str) for p in patterns):
        return None
    include, exclude = [], []
    for written in patterns:
        negated = written.startswith("!")
        for pattern in braces(written[1:] if negated else written):
            pattern = pattern.strip()
            while pattern.startswith("./"):
                pattern = pattern[2:]
            pattern = pattern.rstrip("/").removesuffix("/package.json")
            if pattern in ("", "."):
                continue
            (exclude if negated else include).append(glob_re(pattern + "/package.json"))
    exclude += [glob_re(ignore) for ignore in (PNPM_IGNORES if pnpm else PNPM_IGNORES[:1])]
    files = (checkout / ".git" / "corpus-files.txt").read_text(encoding="utf-8").splitlines()
    members = [f.removesuffix("/package.json") for f in files
               if f.endswith("/package.json") and any(r.match(f) for r in include)
               and not any(r.match(f) for r in exclude)]
    names, named = {}, {}
    for member in list(members):
        try:
            manifest = json.loads((checkout / member / "package.json").read_text(encoding="utf-8"))
        except FileNotFoundError:
            # Past the checkout's 400 package.json files: init reads no package there either.
            members.remove(member)
            continue
        except (OSError, ValueError):
            return None
        if isinstance(manifest.get("name"), str):
            names[manifest["name"]] = member
        named[member] = [name for field in DEPENDENCY_FIELDS
                         for name in (manifest.get(field) or {} if isinstance(manifest.get(field), dict) else {})]
    direct = {m: {names[n] for n in deps if n in names and names[n] != m} for m, deps in named.items()}
    closure = {}
    for member in direct:
        seen, todo = set(), [member]
        while todo:
            here = todo.pop()
            if here not in seen:
                seen.add(here)
                todo.extend(direct.get(here, ()))
        closure[member] = seen - {member}
    return closure


def scope_holds(scope: dict, path: str) -> bool:
    """Whether a scope of init's plan holds `path`, as hushrun reads its patterns."""
    included = any(glob_re(p).match(path) for p in scope.get("include") or [])
    return included and not any(glob_re(p).match(path) for p in scope.get("exclude") or [])


# ---------------------------------------------------------------- the score

def load_records(directory: Path):
    records = {}
    for path in sorted(directory.glob("*.json")):
        record = json.loads(path.read_text())
        records[record["repo"]] = record
    return records


def score(args):
    corpus = json.loads(args.corpus.read_text())["repositories"]
    corpus_hash = hashlib.sha256(args.corpus.read_bytes()).hexdigest()
    if args.only:
        wanted = set(args.only.split(","))
        corpus = [e for e in corpus if e["repo"] in wanted]
    language = {e["repo"]: e["language"] for e in corpus}
    keys = {e["repo"]: f"{e['repo'].replace('/', '__')}@{e['commit'][:12]}" for e in corpus}
    facts = json.loads(args.facts.read_text())
    in_scope = {e["repo"] for e in corpus}
    records = {repo: r for repo, r in load_records(args.init).items() if repo in in_scope}
    again = {repo: r for repo, r in (load_records(args.again) if args.again else {}).items()
             if repo in in_scope}
    known = json.loads(args.known.read_text()) if args.known and args.known.exists() else {}
    baseline = json.loads(args.baseline.read_text()) if args.baseline and args.baseline.exists() else None

    steps_by_job = collections.defaultdict(list)
    for s in facts["steps"]:
        steps_by_job[(s["repo"], s["workflow"], s["job"])].append(s)
    job_facts = {(j["repo"], j["workflow"], j["job"]): j for j in facts["jobs"]}

    modes = collections.Counter()
    tools = collections.Counter()
    decisions = {}
    for repo, record in records.items():
        if record.get("plan"):
            modes["json"] += 1
            tools[record["plan"]["tool"]] += 1
            decisions[repo] = ("json", decisions_of_plan(record["plan"]))
        else:
            modes["text"] += 1
            dec = parse_init_text(record.get("stdout") or "")
            decisions[repo] = ("text", dec)

    jobs_out = []
    unseen_shapes = collections.Counter()
    for j in facts["jobs"]:
        if not (j["test_lines"] > 0 and j["per_change"]) or j["repo"] not in in_scope:
            continue
        repo = j["repo"]
        mode, dec = decisions.get(repo, ("missing", {"wrapped_ci": [], "script_jobs": [], "left_ci": []}))
        key = (j["workflow"], j["job"])
        wrapped = wrapped_test_jobs(dec).get(key, [])
        job_left = [x for x in dec["left_ci"] if (x["workflow"], x["job"]) == key]
        # A test command left decides; else one left to its tool's remote cache, such as
        # `./gradlew build`, whose words say no test.
        left = [x for x in job_left if TESTY.search(x["cmd"] or "")] or \
            [x for x in job_left if x.get("ciWhy") == "stepped-aside"]
        steps = steps_by_job[(repo, j["workflow"], j["job"])]
        families = {s["family"] for s in steps}
        aside_scripts = dec.get("aside_scripts", set())
        aside_script = any(script_of(s["line"]) in aside_scripts for s in steps)
        wall = wall_of(j, wrapped, left, families)
        outcome = outcome_of(j, wrapped, left, families, mode, aside_script)
        if outcome == "unseen":
            for found in sorted({shape(s["family"], s["line"]) for s in steps}):
                unseen_shapes[found] += 1
        jobs_out.append({
            "repo": repo, "workflow": j["workflow"], "job": j["job"], "language": language.get(repo),
            "outcome": outcome, "wall": wall, "matrix": j["matrix_multi"], "reusable": j["in_reusable"],
            "readers": sorted(set(j["uploads_cov"]) | set(j["publishes_tests"])
                              | ({"artifact"} if j["artifact_reports"] else set())),
            "inAction": sorted({s["in_action"] for s in steps if s.get("in_action")}),
            "first": steps[0]["line"][:160] if steps else "",
        })

    def bucket(outcome):
        return "left:*" if outcome.startswith("left:") else outcome

    totals = collections.Counter(j["outcome"] for j in jobs_out)
    grouped = collections.Counter(bucket(j["outcome"]) for j in jobs_out)
    walls = collections.Counter(j["wall"] for j in jobs_out)
    by_language = collections.defaultdict(collections.Counter)
    by_repo = collections.defaultdict(collections.Counter)
    for j in jobs_out:
        by_language[j["language"]][bucket(j["outcome"])] += 1
        by_repo[j["repo"]][bucket(j["outcome"])] += 1

    # ------------------------------------------------ assertions
    results = {name: {"what": what, "status": "pass", "failures": []} for name, what in ASSERTIONS.items()}

    def fail(name, case):
        results[name]["failures"].append(case)

    json_plans = {repo: r["plan"] for repo, r in records.items() if r.get("plan")}
    has_readers = bool(json_plans) and all("readers" in job for p in json_plans.values() for job in p["jobs"])
    if not has_readers:
        results["A1"]["status"] = "not measured"
        results["A1"]["why"] = "init's plan names no readers yet (Phase 1)"
    else:
        # A1: the CLI's readers and the harness's agree on every test job init's plan lists, and
        # a wrapped test command beside a reader that isn't failure-only doesn't predict a skip.
        for j in jobs_out:
            plan = json_plans.get(j["repo"])
            job = next((x for x in (plan or {}).get("jobs", [])
                        if x["file"] == j["workflow"] and x["job"] == j["job"]), None)
            if job is None:
                continue
            cli = sorted({r["name"] for r in job["readers"]})
            j["cliReaders"] = cli
            case = {"repo": j["repo"], "workflow": j["workflow"], "job": j["job"]}
            if bool(cli) != bool(j["readers"]):
                fail("A1", {**case, "why": f"the harness's readers: {', '.join(j['readers']) or 'none'}; "
                                           f"the CLI's: {', '.join(cli) or 'none'}"})
            live = [r for r in job["readers"] if not r["onlyOnFailure"]]
            if not live:
                continue
            for c in [c for c in job["commands"] if c["status"] == "wrapped"] + job["scripts"]:
                testy = TESTY.search(c.get("check") or "") or TESTY.search(c.get("before") or "")
                if testy and c.get("ci") == "skips":
                    fail("A1", {**case, "why": f"{c.get('check')} predicts a skip beside {live[0]['name']}"})
    if not json_plans:
        for name in ("A2", "A3", "A4", "A5", "A6"):
            results[name]["status"] = "not measured"
            results[name]["why"] = "init printed text, not a plan"
    for repo, plan in json_plans.items():
        for job in plan["jobs"]:
            facts_job = job_facts.get((repo, job["file"], job["job"]), {})
            wrapped = [c for c in job["commands"] if c["status"] == "wrapped"] + job["scripts"]
            skips = [c for c in wrapped if c.get("ci") == "skips"]
            if skips and job["variants"] != "one" and job["variantIdentity"] == "none":
                fail("A2", {"repo": repo, "workflow": job["file"], "job": job["job"],
                            "why": f"{len(skips)} predicted to skip, {job['variants']} variants, no identity"})
            triggers = set(facts_job.get("triggers", []))
            called_only = facts_job.get("in_reusable") and not triggers & set(PER_CHANGE)
            if skips and called_only and job["variantIdentity"] != "env":
                fail("A3", {"repo": repo, "workflow": job["file"], "job": job["job"],
                            "why": "a called workflow's job predicted to skip without HUSHRUN_WORKFLOW"})
            secret_vars = facts_job.get("secret_vars", [])
            for c in [c for c in job["commands"] if c["status"] == "wrapped"]:
                line = c["after"] or ""
                named = [v for v in secret_vars
                         if re.search(r"\$\{?" + re.escape(v) + r"\b|env\." + re.escape(v) + r"\b", line)]
                if "secrets." in line or "github.token" in line or named:
                    fail("A4", {"repo": repo, "workflow": job["file"], "job": job["job"], "why": line[:160]})
        for script in plan["scripts"]["wrapped"]:
            if "secrets." in script["after"] or "github.token" in script["after"]:
                fail("A4", {"repo": repo, "workflow": "package.json", "job": script["script"],
                            "why": script["after"][:160]})
        for problem in check_edits(args.repos / keys[repo], plan):
            fail("A5", {"repo": repo, "why": problem})
    if args.again:
        for repo, record in again.items():
            plan = record.get("plan")
            for miss in record.get("editsNotApplied", []):
                fail("A6", {"repo": repo, "why": f"the first run's edit of {miss['path']} didn't apply: {miss['error']}"})
            if plan and plan["edits"]:
                fail("A6", {"repo": repo, "why": "proposes " + ", ".join(e["path"] for e in plan["edits"])})
            if not plan and record.get("exit") == 0:
                fail("A6", {"repo": repo, "why": "the second run printed no plan"})
    elif json_plans:
        results["A6"]["status"] = "not measured"
        results["A6"]["why"] = "no second run (--again)"
    for repo, record in records.items():
        if record.get("timedOut") or record.get("exit") != 0:
            fail("A7", {"repo": repo, "why": f"exit {record.get('exit')}{' (timed out)' if record.get('timedOut') else ''}"})
        elif record.get("seconds", 0) > 60:
            fail("A7", {"repo": repo, "why": f"took {record['seconds']} s"})
        if "panicked" in (record.get("stderr") or ""):
            fail("A7", {"repo": repo, "why": "panicked"})
    missing = sorted(set(keys) - set(records))
    for repo in missing:
        fail("A7", {"repo": repo, "why": "no run"})
    regressions, changes, to_reports, to_aside, to_flags = [], [], [], [], []
    if baseline:
        if baseline.get("corpus") != corpus_hash:
            results["A8"]["status"] = "not measured"
            results["A8"]["why"] = "the baseline is for another corpus.json: run with --update-baseline"
        else:
            before = baseline["jobs"]
            for j in jobs_out:
                key = f"{j['repo']} {j['workflow']} {j['job']}"
                old = before.get(key)
                if old is not None and old != j["outcome"]:
                    changes.append({"job": key, "from": old, "to": j["outcome"]})
                    # Phase 1: a job whose reports a later step reads stops skipping in CI, as it
                    # should. Each is listed.
                    if old == "skips" and j["outcome"] == "runs:reports":
                        to_reports.append(key)
                    # Phase 4 (D7): a job whose tool reuses its own results across machines is
                    # left to that tool. Each is listed.
                    elif old == "skips" and j["outcome"] == "stepped-aside":
                        to_aside.append(key)
                    # [ADR 0055 item 6] A cargo line whose RUSTFLAGS or RUSTDOCFLAGS init can't
                    # carry into it stays as it is: a pass without CI's flags never skips CI's.
                    # Each is listed.
                    elif old == "skips" and j["outcome"] == "left:flags":
                        to_flags.append(key)
                    elif old == "skips":
                        case = {"repo": j["repo"], "workflow": j["workflow"], "job": j["job"],
                                "why": f"skips → {j['outcome']}"}
                        regressions.append(case)
                        fail("A8", case)
    else:
        results["A8"]["status"] = "not measured"
        results["A8"]["why"] = "no baseline yet"
    results["A9"]["status"] = "elsewhere"
    results["A9"]["why"] = "checked by crates/hushrun/tests/init_fixtures.rs on every fixture"
    # [ADR 0051] Every package scope holds the workspace packages the harness finds it depends on.
    checked = 0
    for repo, plan in json_plans.items():
        graph = workspace_graph(args.repos / keys[repo])
        if not graph:
            continue
        for scope in plan.get("scopes") or []:
            dir = scope.get("dir")
            if dir not in graph:
                continue
            checked += 1
            for needed in sorted(graph[dir]):
                if not scope_holds(scope, f"{needed}/package.json"):
                    fail("A10", {"repo": repo, "why": f"scope {scope['name']} ({dir}) leaves out "
                                                     f"{needed}, a workspace package it depends on"})
    if checked:
        results["A10"]["why"] = f"{checked} package scopes"
    else:
        results["A10"]["status"] = "not measured"
        results["A10"]["why"] = "no plan gives a workspace package a scope"

    failed = False
    for name, result in results.items():
        kept, listed = [], []
        for case in result["failures"]:
            match = any(
                all(case.get(key) == k.get(key) for key in ("repo", "workflow", "job") if key in k)
                for k in known.get(name, []))
            (listed if match else kept).append(case)
        result["failures"], result["known"] = kept, listed
        if kept and result["status"] == "pass":
            result["status"] = "fail"
            failed = True
        elif listed and result["status"] == "pass":
            result["status"] = "pass (known)"

    delta = None
    if baseline and baseline.get("corpus") == corpus_hash:
        # Of the repositories measured, so that --only compares like with like.
        before = collections.Counter(
            bucket(outcome) for key, outcome in baseline["jobs"].items()
            if key.split(" ", 1)[0] in in_scope)
        delta = {k: grouped.get(k, 0) - before.get(k, 0) for k in OUTCOMES}

    board = {
        "tool": ", ".join(sorted(tools)) or "text",
        "modes": dict(modes),
        "corpus": corpus_hash,
        "repositories": len(corpus),
        "testJobs": len(jobs_out),
        "outcomes": dict(sorted(totals.items())),
        "grouped": {k: grouped.get(k, 0) for k in OUTCOMES},
        "walls": {k: walls.get(k, 0) for k in WALLS},
        "survey": SURVEY,
        "byLanguage": {k: dict(v) for k, v in sorted(by_language.items())},
        "byRepository": {k: dict(v) for k, v in sorted(by_repo.items())},
        "unseenShapes": unseen_shapes.most_common(30),
        "assertions": results,
        "delta": delta,
        "changes": changes,
        "toReports": to_reports,
        "toSteppedAside": to_aside,
        "toFlags": to_flags,
        "jobs": jobs_out,
    }
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "scoreboard.json").write_text(json.dumps(board, indent=1) + "\n")
    (args.out / "scoreboard.md").write_text(markdown(board, baseline))
    if args.update_baseline:
        args.baseline.write_text(json.dumps({
            "about": "The scoreboard of the last accepted corpus run: score.py compares every run with it (A8). cargo xtask corpus --update-baseline writes it.",
            "tool": board["tool"], "corpus": corpus_hash, "testJobs": len(jobs_out),
            "grouped": board["grouped"], "walls": board["walls"],
            "jobs": {f"{j['repo']} {j['workflow']} {j['job']}": j["outcome"] for j in jobs_out},
        }, indent=1, sort_keys=False) + "\n")
    print(markdown(board, baseline, short=True))
    return 1 if failed else 0


def markdown(board, baseline, short=False):
    n = board["testJobs"]
    lines = [f"# Corpus scoreboard: {board['tool']}", "",
             f"{board['repositories']} repositories, {n} test jobs that run on pushes, pull requests or merge queues.", ""]
    lines += ["## Outcomes", "", "| Outcome | Jobs | Share | Against the baseline |", "|---|---:|---:|---:|"]
    for k, v in board["grouped"].items():
        d = board["delta"].get(k, 0) if board["delta"] else None
        lines.append(f"| `{k}` | {v} | {100 * v / max(n, 1):.0f}% | {'' if d is None else f'{d:+d}'} |")
    lefts = {k: v for k, v in board["outcomes"].items() if k.startswith("left:")}
    if lefts:
        lines += ["", "`left:*` by reason: " + ", ".join(f"`{k}` {v}" for k, v in sorted(lefts.items(), key=lambda x: -x[1]))]
    lines += ["", "## The survey's walls", "", "| Wall | Jobs | Survey (0.6.0) |", "|---|---:|---:|"]
    for k, label in WALLS.items():
        lines.append(f"| {label} | {board['walls'][k]} | {board['survey'][k]} |")
    lines += ["", "## Assertions", "", "| | What | Result |", "|---|---|---|"]
    for name, r in board["assertions"].items():
        result = r["status"].removesuffix(" (known)")
        if r["failures"]:
            result += f": {len(r['failures'])} failing"
        if r.get("known"):
            count = len(r["known"])
            result += f", and {count} known" if r["failures"] else \
                f", but for {count} known case{'' if count == 1 else 's'} (known.json)"
        if r.get("why"):
            result += f": {r['why']}"
        lines.append(f"| {name} | {r['what']} | {result} |")
    for name, r in board["assertions"].items():
        if r["failures"]:
            lines += ["", f"### {name} failures", ""]
            lines += [f"- {c.get('repo')} {c.get('workflow', '')} {c.get('job', '')}: {c['why']}".replace("  ", " ")
                      for c in r["failures"][:40]]
    if short:
        return "\n".join(lines) + "\n"
    lines += ["", "## Skips by language", "", "| Language | Test jobs | `skips` |", "|---|---:|---:|"]
    for lang, counts in board["byLanguage"].items():
        total = sum(counts.values())
        lines.append(f"| {lang} | {total} | {counts.get('skips', 0)} ({100 * counts.get('skips', 0) / max(total, 1):.0f}%) |")
    lines += ["", "## The 30 most common shapes of `unseen` commands", "", "| Shape | Jobs |", "|---|---:|"]
    lines += [f"| `{s}` | {c} |" for s, c in board["unseenShapes"]]
    if board.get("toReports"):
        lines += ["", f"## Stopped skipping in CI, since a later step reads their reports ({len(board['toReports'])})",
                  "", "A8 lets these go: a skip would leave their readers nothing.", ""]
        lines += [f"- {job}" for job in board["toReports"]]
    if board.get("toSteppedAside"):
        lines += ["", f"## Left to a remote build cache ({len(board['toSteppedAside'])})",
                  "", "A8 lets these go: their tool reuses its own results across machines.", ""]
        lines += [f"- {job}" for job in board["toSteppedAside"]]
    if board.get("toFlags"):
        lines += ["", f"## Left, since init can't carry CI's Rust flags into the line ({len(board['toFlags'])})",
                  "", "A8 lets these go: a pass without CI's RUSTFLAGS or RUSTDOCFLAGS never skips CI's (ADR 0055).", ""]
        lines += [f"- {job}" for job in board["toFlags"]]
    if board["changes"]:
        lines += ["", f"## Changed against the baseline ({len(board['changes'])})", ""]
        lines += [f"- {c['job']}: `{c['from']}` → `{c['to']}`" for c in board["changes"][:200]]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--repos", type=Path, required=True)
    parser.add_argument("--facts", type=Path, required=True)
    parser.add_argument("--init", type=Path, required=True)
    parser.add_argument("--again", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--known", type=Path)
    parser.add_argument("--only")
    parser.add_argument("--update-baseline", action="store_true")
    args = parser.parse_args()
    if args.update_baseline and not args.baseline:
        sys.exit("--update-baseline needs --baseline")
    if args.update_baseline and args.only:
        sys.exit("--update-baseline measures every repository: leave out --only")
    sys.exit(score(args))


if __name__ == "__main__":
    main()
