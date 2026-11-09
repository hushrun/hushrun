"""Analyze the CI of each fetched repository: per-job facts, read with the harness's own reader.

Usage: python3 -I analyze.py --corpus corpus.json --repos DIR --out facts.json

Ported from the survey's method/analyze.py, with its scratch paths made parameters and its
classification left as it was, so that the harness's counts can be checked against the survey's.
Beside the survey's facts it keeps, per test line, the local composite action it came from, and,
per job, the variables set from a secret (A4). extra.py's repository facts (duplicate triggers,
merge queues, path filters) are folded in.

Reads only YAML/JSON/TOML/text with safe loaders; never executes repository content. It never
imports hushrun's code: it is only useful where it can disagree with it.
"""
import argparse
import json
import re
import sys
from pathlib import Path

import yaml

# The harness never writes into the source tree.
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import patterns as P  # noqa: E402

REPOS = None
CATEGORY = {}
FETCHED = {}


class Loader(yaml.SafeLoader):
    pass


# keep "on" as a string key instead of YAML 1.1's boolean
for ch in "OoYyNn":
    Loader.yaml_implicit_resolvers[ch] = [
        (tag, rx) for tag, rx in Loader.yaml_implicit_resolvers.get(ch, [])
        if tag != "tag:yaml.org,2002:bool"]
Loader.add_implicit_resolver("tag:yaml.org,2002:bool", re.compile(r"^(?:true|True|TRUE|false|False|FALSE)$"), list("tTfF"))


def load_yaml(path: Path):
    try:
        return yaml.load(path.read_text(encoding="utf-8", errors="replace"), Loader=Loader)
    except Exception as e:  # noqa: BLE001
        return {"__error__": str(e)[:200]}


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except Exception:  # noqa: BLE001
        return ""


def logical_lines(script: str):
    """Shell lines with backslash continuations joined; comments dropped."""
    out, buf = [], ""
    for raw in script.splitlines():
        s = raw.rstrip()
        if s.endswith("\\"):
            buf += s[:-1] + " "
            continue
        buf += s
        t = buf.strip()
        if t and not t.startswith("#"):
            out.append(t)
        buf = ""
    if buf.strip():
        out.append(buf.strip())
    return out


def as_list(v):
    if v is None:
        return []
    return v if isinstance(v, list) else [v]


# ------------------------------------------------------------------ package.json scripts
def pkg_scripts(repo: Path, rel_dir: str = ""):
    p = repo / rel_dir / "package.json" if rel_dir else repo / "package.json"
    try:
        data = json.loads(read(p) or "{}")
    except Exception:  # noqa: BLE001
        return {}, {}
    return (data.get("scripts") or {}) if isinstance(data, dict) else {}, data if isinstance(data, dict) else {}


PM_RUN = re.compile(r"(?<![\w-])(npm|pnpm|yarn|bun)\s+((?:-[\w-]+(?:[= ]\S+)?\s+)*)(?:run(?:-script)?\s+)?([\w:.\-]+)")


def expand_scripts(line: str, scripts: dict, depth=0) -> str:
    """Text of the package.json scripts a line runs, recursively."""
    if depth > 3 or not scripts:
        return ""
    found = []
    for m in PM_RUN.finditer(line):
        name = m.group(3)
        if name in ("install", "ci", "i", "add", "exec", "dlx", "x", "workspaces", "workspace"):
            continue
        if name == "t":
            name = "test"
        body = scripts.get(name)
        if isinstance(body, str):
            found.append(body)
            found.append(expand_scripts(body, scripts, depth + 1))
    return "\n".join(found)


# ------------------------------------------------------------------ config-level report writers
def config_flags(repo: Path):
    flags = {"pytest_addopts_cov": False, "pytest_addopts_junit": False, "jest_collect_cov": False,
             "vitest_cov_enabled": False, "nextest_junit": False, "jvm_xml_reports": False,
             "jacoco": False, "kover": False, "tox_cov": False, "nox_cov": False,
             "simplecov": False, "phpunit_cov": False}
    for name in ("pyproject.toml", "setup.cfg", "tox.ini", "pytest.ini"):
        t = read(repo / name)
        m = re.search(r"addopts\s*=\s*(\[[^\]]*\]|\"\"\"[\s\S]*?\"\"\"|'''[\s\S]*?'''|[^\n]*(\n[ \t]+[^\n]+)*)", t)
        if m:
            if "--cov" in m.group(0):
                flags["pytest_addopts_cov"] = True
            if "junit" in m.group(0):
                flags["pytest_addopts_junit"] = True
        if name == "tox.ini" and re.search(r"--cov|coverage run|pytest-cov", t):
            flags["tox_cov"] = True
    if re.search(r"--cov|coverage|session\.run\([^)]*cov", read(repo / "noxfile.py")):
        flags["nox_cov"] = True
    for f in repo.glob("jest.config.*"):
        if re.search(r"collectCoverage\s*:\s*true", read(f)):
            flags["jest_collect_cov"] = True
    _, pj = pkg_scripts(repo)
    if isinstance(pj.get("jest"), dict) and pj["jest"].get("collectCoverage"):
        flags["jest_collect_cov"] = True
    for f in list(repo.glob("vitest.config.*")) + list(repo.glob("vite.config.*")) + list(repo.glob("vitest.workspace.*")):
        t = read(f)
        if re.search(r"coverage\s*:\s*\{[^}]*enabled\s*:\s*(true|!?process|isCI|!!process)", t, re.S):
            flags["vitest_cov_enabled"] = True
    for f in [repo / ".config" / "nextest.toml", repo / "nextest.toml"]:
        if re.search(r"junit", read(f)):
            flags["nextest_junit"] = True
    jvm = [repo / n for n in ("build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts", "pom.xml")]
    if any(p.exists() for p in jvm):
        flags["jvm_xml_reports"] = True  # Gradle and Maven test tasks write JUnit XML by default
        t = "\n".join(read(p) for p in jvm)
        if re.search(r"jacoco", t, re.I):
            flags["jacoco"] = True
        if re.search(r"kover", t, re.I):
            flags["kover"] = True
    if re.search(r"simplecov", read(repo / "Gemfile"), re.I) or (repo / ".simplecov").exists():
        flags["simplecov"] = True
    for n in ("phpunit.xml", "phpunit.xml.dist", "phpunit.dist.xml"):
        if re.search(r"<coverage|<report>", read(repo / n)):
            flags["phpunit_cov"] = True
    return flags


def codecov_config(repo: Path):
    for rel in ("codecov.yml", ".codecov.yml", "codecov.yaml", ".codecov.yaml", ".github/codecov.yml",
                ".github/.codecov.yml", ".github/codecov.yaml", "dev/codecov.yml", "ci/codecov.yml"):
        p = repo / rel
        if p.exists():
            d = load_yaml(p)
            if not isinstance(d, dict):
                d = {}
            cov = d.get("coverage") or {}
            status = cov.get("status") if isinstance(cov, dict) else None
            flags = d.get("flags") or {}
            fm = d.get("flag_management") or {}
            cf = False
            if isinstance(flags, dict):
                cf = any(isinstance(v, dict) and v.get("carryforward") for v in flags.values())
            if isinstance(fm, dict):
                dr = fm.get("default_rules") or {}
                if isinstance(dr, dict) and dr.get("carryforward"):
                    cf = True
                for ind in fm.get("individual_flags") or []:
                    if isinstance(ind, dict) and ind.get("carryforward"):
                        cf = True
            cc = d.get("codecov") or {}
            notify = cc.get("notify") if isinstance(cc, dict) else None
            comment = d.get("comment", "default")
            anb = None
            if isinstance(notify, dict) and "after_n_builds" in notify:
                anb = notify["after_n_builds"]
            c_anb = comment.get("after_n_builds") if isinstance(comment, dict) else None
            return {
                "path": rel,
                "comment": ("off" if comment is False else ("default" if comment == "default" else "on")),
                "comment_require_changes": bool(isinstance(comment, dict) and comment.get("require_changes")),
                "after_n_builds": anb, "comment_after_n_builds": c_anb,
                "wait_for_ci": notify.get("wait_for_ci") if isinstance(notify, dict) else None,
                "require_ci_to_pass": cc.get("require_ci_to_pass") if isinstance(cc, dict) else None,
                "carryforward": cf,
                "n_flags": len(flags) if isinstance(flags, dict) else len(fm.get("individual_flags") or []) if isinstance(fm, dict) else 0,
                "status": ("off" if status is False else
                           "custom" if isinstance(status, dict) else "default"),
                "status_informational": bool(re.search(r"informational:\s*true", read(p))),
                "parse_error": d.get("__error__") if isinstance(d, dict) else None,
            }
    return None


# ------------------------------------------------------------------ GitHub Actions
def triggers_of(wf: dict):
    on = wf.get("on", wf.get(True))
    trig, paths = {}, False
    if isinstance(on, str):
        trig[on] = {}
    elif isinstance(on, list):
        for t in on:
            trig[str(t)] = {}
    elif isinstance(on, dict):
        for k, v in on.items():
            trig[str(k)] = v if isinstance(v, dict) else {}
            if isinstance(v, dict) and any(x in v for x in ("paths", "paths-ignore")):
                if k in ("push", "pull_request", "pull_request_target", "merge_group"):
                    paths = True
    return trig, paths


def matrix_variants(strategy):
    """(variants, countable). variants is an int estimate; countable False when expressions decide."""
    if not isinstance(strategy, dict):
        return 1, True
    m = strategy.get("matrix")
    if m is None:
        return 1, True
    if isinstance(m, str):
        return None, False
    if not isinstance(m, dict):
        return None, False
    prod, keys, countable = 1, 0, True
    for k, v in m.items():
        if k in ("include", "exclude"):
            continue
        keys += 1
        if isinstance(v, list):
            prod *= max(len(v), 1)
            if any(isinstance(x, str) and "${{" in x for x in v):
                countable = False
        else:
            countable = False
    inc = m.get("include")
    exc = m.get("exclude")
    if isinstance(inc, str) or isinstance(exc, str):
        countable = False
    n_inc = len(inc) if isinstance(inc, list) else 0
    n_exc = len(exc) if isinstance(exc, list) else 0
    if keys == 0:
        variants = n_inc
    else:
        variants = max(prod - n_exc, 1) + 0  # include items mostly extend existing combos
        if n_inc and prod == 1:
            variants = max(n_inc, 1)
    return variants, countable


def runner_kind(runs_on, matrix):
    labels = []
    if isinstance(runs_on, str):
        labels = [runs_on]
    elif isinstance(runs_on, list):
        labels = [str(x) for x in runs_on]
    elif isinstance(runs_on, dict):
        labels = [str(x) for x in as_list(runs_on.get("labels"))] + ["group:" + str(runs_on.get("group"))]
    kinds = set()
    for lab in labels:
        if "${{" in lab:
            mm = re.search(r"matrix\.([\w-]+)", lab)
            vals = []
            if mm and isinstance(matrix, dict):
                vals = [v for v in as_list(matrix.get(mm.group(1))) if isinstance(v, str)]
                for inc in as_list(matrix.get("include")):
                    if isinstance(inc, dict) and isinstance(inc.get(mm.group(1)), str):
                        vals.append(inc[mm.group(1)])
            if vals:
                for v in vals:
                    kinds.add(runner_kind(v, None))
            else:
                kinds.add("expression")
        elif lab == "self-hosted" or lab.startswith("group:"):
            kinds.add("self-hosted")
        elif P.GH_HOSTED.match(lab):
            kinds.add("github-hosted")
        elif P.THIRD_PARTY_RUNNER.search(lab):
            kinds.add("third-party")
        else:
            kinds.add("custom-label")
    if not kinds:
        return "none"
    for k in ("self-hosted", "third-party", "custom-label", "expression", "github-hosted"):
        if k in kinds:
            return k if len(kinds) == 1 else "mixed:" + "+".join(sorted(kinds))
    return "+".join(sorted(kinds))


def load_composite(repo: Path, uses: str, depth=0):
    """Steps of a local composite action, inlined."""
    if depth > 3:
        return []
    rel = uses[2:] if uses.startswith("./") else uses
    for name in ("action.yml", "action.yaml"):
        p = repo / rel / name
        if p.exists():
            d = load_yaml(p)
            if isinstance(d, dict) and isinstance(d.get("runs"), dict):
                steps = d["runs"].get("steps") or []
                out = []
                for s in steps if isinstance(steps, list) else []:
                    if isinstance(s, dict):
                        out.append(s)
                        u = s.get("uses")
                        if isinstance(u, str) and u.startswith("./"):
                            out.extend(load_composite(repo, u, depth + 1))
                return out
    return []


def classify_test_line(line: str):
    fams = [fam for fam, rx in P.TEST_CMDS if rx.search(line)]
    if not fams:
        return None
    # prefer specific tool over generic script/pm wrappers
    order = ["pytest", "tox", "nox", "python-unittest", "go-test", "cargo-test", "jest", "vitest",
             "mocha", "playwright", "cypress", "bun-test", "deno-test", "node-test", "gradle", "maven",
             "sbt", "bazel-test", "ctest", "ruby", "php", "dotnet", "swift", "elixir", "dart", "zig",
             "turbo", "nx", "lerna", "js-pm-script", "make-test", "just-task", "script"]
    for f in order:
        if f in fams:
            return f
    return fams[0]


NOT_TEST = re.compile(
    r"^(echo|printf|cat|ls|cd|mkdir|cp|mv|rm|export|set|if|then|fi|else|for|do|done|git |sudo apt|apt|brew install|"
    r"pip install|python -m pip|npm (ci|install)|pnpm install|yarn install|choco|curl|wget|docker (pull|build|push)|"
    r"#)|--help|--version|install-deps|test -[fdzne] |\[\[? -|^test\s|setup|playwright install|lint|typecheck|"
    r"tsc\b|eslint|prettier|clippy|fmt\b|format|rustfmt|build-only|--no-run|-DskipTests(?!=false)|"
    r"go test -run=XXX|bench|--list|xvfb-run$|bundle install|composer install|cargo build|mvn -B dependency", re.I)


def is_test_line(line: str):
    fam = classify_test_line(line)
    if not fam:
        return None
    low = line.lower()
    if NOT_TEST.search(line) and fam in ("script", "make-test", "js-pm-script", "just-task", "gradle", "maven", "dotnet"):
        # common false positives: echo "...test...", build-only, lint
        if not re.search(r"\b(test|tests|check|spec)\b", low) or re.match(r"^(echo|printf|cat|ls|cd|export|if |#)", line):
            return None
    if re.match(r"^(echo|printf|cat|ls|export|#|::)", line):
        return None
    if re.match(r"^(!\s*)?(test|\[\[?)\s+(!\s+)?([\"'$-]|\S+\s+(=|==|!=|-eq|-ne|-gt|-lt|-ge|-le)\s)", line):
        return None  # the shell's test builtin
    if fam == "cargo-test" and re.search(r"--no-run|cargo\s+test\s+--doc\s*$", line):
        return None
    if fam == "js-pm-script":
        m = re.search(r"(?<![\w-])(?:npm|pnpm|yarn|bun)\s+(?:\S+\s+)*?(?:run(?:-script)?\s+)?((?:test|t|tests|ci|check|unit|e2e|coverage|cov|vitest|jest|spec)[\w:.-]*)", line)
        name = m.group(1) if m else ""
        if re.search(r"publish|release|deploy|build|docs?$|lint|format|prettier|typecheck|type-check|tsc|changeset|version|bundle|size", name, re.I) and not re.search(r"test|spec|e2e|unit", name, re.I):
            return None
    if fam == "gradle":
        m = re.search(r"(?<![\w/.:-])(\./)?gradlew?(\.bat)?(?=\s)(.*)$", line)
        toks = (m.group(3) if m else "").split()
        tasks, skip_next = [], False
        for t in toks:
            if skip_next:
                skip_next = False
                continue
            if t in ("-x", "--exclude-task"):
                skip_next = True
                continue
            if t.startswith("-") or "=" in t or t in ("&&", "||", "|", ";"):
                if t in ("&&", "||", "|", ";"):
                    break
                continue
            tasks.append(t)
        if not any(re.search(r"(^|:)(test|check|build)$|[a-z0-9]Tests?$|^allTests$|integrationTest|^ci\w*$", t) for t in tasks):
            return None
    if fam == "maven":
        if re.search(r"-DskipTests(?!=false)\b|-Dmaven\.test\.skip=true|-DskipTests=true", line):
            return None
        m = re.search(r"(?<![\w/.:-])(\./)?(mvnw?|build/mvn|[\w./-]*mvn_\w+\.(ps1|sh))(\.cmd)?(?=\s)(.*)$", line)
        toks = (m.group(5) if m else "").split()
        if not any(t in ("test", "verify", "install", "package", "integration-test", "deploy") or t.endswith(":test") for t in toks):
            return None
    return fam


def make_recipe(text: str, target: str, depth=0):
    """Recipe lines of a Makefile/justfile target, with one level of prerequisites."""
    if depth > 2 or not text:
        return ""
    m = re.search(r"(?m)^" + re.escape(target) + r"(\s[^:=\n]*)?\s*:(?!=)([^\n]*)\n((?:(?:\t| {2,})[^\n]*\n?|\s*\n)*)", text)
    if not m:
        return ""
    body = m.group(3)
    pre = [p for p in re.split(r"\s+", m.group(2).split("#")[0].strip()) if p and not p.startswith(("$", "|"))]
    out = body
    for p in pre[:6]:
        out += "\n" + make_recipe(text, p, depth + 1)
    return out


SCRIPT_TOKEN = re.compile(r"""(?:^|\s|=|["'])((?:\./|\.\./)?[\w.${}/-]+\.(?:sh|bash|py|ps1|js|mjs|cjs|ts|rb|pl|cmd|bat))\b""")


def follow(line: str, wd, rel: dict, depth=0):
    """Text of repo-local scripts and make/just targets a line starts (only files we fetched)."""
    if depth > 1:
        return ""
    out = []
    base = (wd.strip("./") + "/") if isinstance(wd, str) and wd and "${{" not in wd else ""
    for m in SCRIPT_TOKEN.finditer(line):
        tok = m.group(1).lstrip("./") if not m.group(1).startswith("../") else m.group(1)
        for cand in (base + tok, tok):
            p = rel.get(cand)
            if p is not None:
                t = read(p)[:200000]
                out.append(t)
                for l2 in logical_lines(t)[:400]:
                    if SCRIPT_TOKEN.search(l2):
                        out.append(follow(l2, None, rel, depth + 1))
                break
    mm = re.search(r"(?<![\w/-])(make|gmake|just)\s+((?:-\S+\s+(?:\S+\s+)?|\w+=\S+\s+)*)([\w-]+)", line)
    if mm:
        tool, opts, target = mm.group(1), mm.group(2), mm.group(3)
        cdir = re.search(r"-C\s+(\S+)", opts or "")
        d0 = (cdir.group(1).strip("./") + "/") if cdir else base
        names = ("justfile", "Justfile", ".justfile") if tool == "just" else ("Makefile", "makefile", "GNUmakefile")
        for n in names:
            p = rel.get(d0 + n) or rel.get(n)
            if p is not None:
                out.append(make_recipe(read(p), target))
                break
    return "\n".join(x for x in out if x)


def scan_report_flags(text: str):
    cov = sorted({name for name, rx in P.COV_GEN if rx.search(text)})
    junit = bool(P.JUNIT_GEN.search(text))
    return cov, junit


def analyze_repo(rname: str):
    d = REPOS / FETCHED[rname]["key"]
    files = [p for p in d.rglob("*") if p.is_file() and ".git" not in p.parts]
    rel = {str(p.relative_to(d)): p for p in files}
    res = {"repo": rname, "category": CATEGORY.get(rname), **FETCHED.get(rname, {})}
    providers = set()
    wf_files = sorted([p for k, p in rel.items() if re.match(r"\.github/workflows/[^/]+\.ya?ml$", k)])
    if wf_files:
        providers.add("github-actions")
    all_paths = [l.rstrip("\n") for l in open(d / ".git" / "corpus-files.txt", encoding="utf-8", errors="replace")]
    other_ci = {
        "circleci": [k for k in all_paths if k.startswith(".circleci/") and k.endswith((".yml", ".yaml"))],
        "buildkite": [k for k in all_paths if k.startswith(".buildkite/")],
        "azure-pipelines": [k for k in all_paths if re.match(r"(azure-pipelines[^/]*\.ya?ml$|\.azure-pipelines/|\.azure/pipelines/|eng/pipelines/|build/azure-pipelines/|\.pipelines/|\.vsts-ci/)", k)],
        "gitlab-ci": [k for k in all_paths if k == ".gitlab-ci.yml" or k.startswith(".gitlab/ci")],
        "travis": [k for k in all_paths if k == ".travis.yml"],
        "cirrus": [k for k in all_paths if k in (".cirrus.yml", ".cirrus.star")],
        "appveyor": [k for k in all_paths if k in ("appveyor.yml", ".appveyor.yml")],
        "jenkins": [k for k in all_paths if re.match(r"(Jenkinsfile|\.jenkins/)", k)],
        "luci": [k for k in all_paths if k == ".ci.yaml"],
        "teamcity": [k for k in all_paths if k.startswith(".teamcity/")],
    }
    for k, v in other_ci.items():
        if v:
            providers.add(k)
    res["other_ci_files"] = {k: len(v) for k, v in other_ci.items() if v}

    scripts_root, _ = pkg_scripts(d)
    cfg = config_flags(d)
    res["config_flags"] = {k: v for k, v in cfg.items() if v}
    res["codecov_yml"] = codecov_config(d)
    res["merge_bots"] = sorted({v for k, v in P.MERGE_QUEUE_FILES.items() if k in rel})
    res["build_systems"] = sorted({n for n, f in (("bazel", "MODULE.bazel"), ("bazel", "WORKSPACE"), ("bazel", ".bazelversion"),
                                                  ("nx", "nx.json"), ("turbo", "turbo.json"), ("lerna", "lerna.json"),
                                                  ("rush", "rush.json"), ("moon", "moon.yml"), ("pnpm-workspace", "pnpm-workspace.yaml"))
                                   if f in rel})

    # ---- parse workflows
    wfs = {}
    for p in wf_files:
        k = str(p.relative_to(d))
        y = load_yaml(p)
        wfs[k] = y if isinstance(y, dict) else {"__error__": "not a mapping"}
    called_local = set()
    for k, y in wfs.items():
        for jid, job in (y.get("jobs") or {}).items() if isinstance(y.get("jobs"), dict) else []:
            if isinstance(job, dict) and isinstance(job.get("uses"), str) and re.match(r"^(\./|\$/)\.github/workflows/", job["uses"]):
                called_local.add(job["uses"][2:].split("@")[0])

    trig_counts = {}
    agg = {"merge_group": False, "pull_request": False, "push": False, "pull_request_target": False,
           "workflow_run": False, "schedule": False, "paths_filter_trigger": False, "concurrency_cancel": False,
           "id_token_write": False, "step_summary_any": False, "reusable_local_calls": len(called_local),
           "reusable_remote_calls": 0, "parse_errors": 0}
    test_steps = []
    jobs_out = []
    uses_all = []  # (workflow, job, uses, with)
    run_text_all = []
    jobs_total = 0
    runners = {}
    for k, y in wfs.items():
        if "__error__" in y:
            agg["parse_errors"] += 1
            continue
        trig, pf = triggers_of(y)
        for t in trig:
            trig_counts[t] = trig_counts.get(t, 0) + 1
            if t in agg:
                agg[t] = True
        if pf:
            agg["paths_filter_trigger"] = True
        is_reusable = "workflow_call" in trig
        per_change = any(t in trig for t in ("push", "pull_request", "merge_group", "pull_request_target")) or (is_reusable and k in called_local)
        conc = y.get("concurrency")
        if isinstance(conc, dict) and conc.get("cancel-in-progress") not in (None, False, "false"):
            agg["concurrency_cancel"] = True
        perms = y.get("permissions")
        if isinstance(perms, dict) and str(perms.get("id-token")) == "write":
            agg["id_token_write"] = True
        wenv = y.get("env") or {}
        jobs = y.get("jobs") if isinstance(y.get("jobs"), dict) else {}
        for jid, job in jobs.items():
            if not isinstance(job, dict):
                continue
            jobs_total += 1
            if isinstance(job.get("uses"), str):
                if not job["uses"].startswith(("./", "$/")):
                    agg["reusable_remote_calls"] += 1
                uses_all.append((k, jid, job["uses"], job.get("with") or {}))
                continue
            jc = job.get("concurrency")
            if isinstance(jc, dict) and jc.get("cancel-in-progress") not in (None, False, "false"):
                agg["concurrency_cancel"] = True
            jp = job.get("permissions")
            if isinstance(jp, dict) and str(jp.get("id-token")) == "write":
                agg["id_token_write"] = True
            strategy = job.get("strategy")
            variants, countable = matrix_variants(strategy)
            matrix = strategy.get("matrix") if isinstance(strategy, dict) else None
            rk = runner_kind(job.get("runs-on"), matrix if isinstance(matrix, dict) else None)
            runners[rk] = runners.get(rk, 0) + 1
            jenv = {**(wenv if isinstance(wenv, dict) else {}), **(job.get("env") or {} if isinstance(job.get("env"), dict) else {})}
            env_text = " ".join(f"{a}={b}" for a, b in jenv.items())
            steps = []
            for s in job.get("steps") or [] if isinstance(job.get("steps"), list) else []:
                if not isinstance(s, dict):
                    continue
                steps.append(s)
                u = s.get("uses")
                if isinstance(u, str) and u.startswith("./"):
                    # Inlined, as the survey did; each remembers the action it came from.
                    steps.extend({**inner, "__action__": u} for inner in load_composite(d, u))
            secret_vars = sorted(
                str(name) for scope in [wenv, job.get("env")] + [s.get("env") for s in steps]
                if isinstance(scope, dict)
                for name, value in scope.items()
                if re.search(r"secrets\.|github\.token", str(value)))
            job_text_parts = []
            for s in steps:
                u = s.get("uses")
                w = s.get("with") or {}
                if isinstance(u, str):
                    uses_all.append((k, jid, u, w if isinstance(w, dict) else {}))
                    job_text_parts.append("uses: " + u + " " + json.dumps(w, default=str))
                run = s.get("run")
                if isinstance(run, str):
                    run_text_all.append(run)
                    job_text_parts.append(run)
                    if P.STEP_SUMMARY.search(run):
                        agg["step_summary_any"] = True
                if isinstance(w, dict) and isinstance(w.get("script"), str) and P.STEP_SUMMARY.search(w["script"]):
                    agg["step_summary_any"] = True
            job_text = "\n".join(job_text_parts) + "\n" + env_text
            for si, s in enumerate(steps):
                run = s.get("run")
                senv = s.get("env") if isinstance(s.get("env"), dict) else {}
                wd = s.get("working-directory") or ((job.get("defaults") or {}).get("run") or {}).get("working-directory") if isinstance(job.get("defaults"), dict) else s.get("working-directory")
                cands = []
                if isinstance(run, str):
                    cands = logical_lines(run)
                u = s.get("uses")
                if isinstance(u, str) and re.match(r"cypress-io/github-action", u, re.I):
                    cands.append("cypress run")
                for line in cands:
                    fam = is_test_line(line)
                    followed = follow(line, wd, rel)
                    inner_fam = None
                    if followed:
                        for l2 in logical_lines(followed)[:2000]:
                            f2 = is_test_line(l2)
                            if f2 and f2 not in ("script",):
                                inner_fam = f2
                                break
                    if not fam:
                        if inner_fam and re.search(r"test|check|ci|spec|verify", line, re.I):
                            fam = "script"
                        else:
                            continue
                    sc = scripts_root
                    if isinstance(wd, str) and wd and "${{" not in wd:
                        sub, _ = pkg_scripts(d, wd.strip("./"))
                        sc = sub or scripts_root
                    expanded = expand_scripts(line, sc)
                    step_env = " ".join(f"{a}={b}" for a, b in senv.items())
                    cov_direct, junit_direct = scan_report_flags(line + "\n" + expanded + "\n" + step_env)
                    cov_followed, junit_followed = scan_report_flags(followed) if followed else ([], False)
                    cov_cfg = []
                    if fam in ("pytest",) and cfg["pytest_addopts_cov"]:
                        cov_cfg.append("pytest addopts")
                    if fam == "tox" and cfg["tox_cov"]:
                        cov_cfg.append("tox.ini")
                    if fam == "nox" and cfg["nox_cov"]:
                        cov_cfg.append("noxfile")
                    if fam in ("jest", "js-pm-script") and cfg["jest_collect_cov"]:
                        cov_cfg.append("jest collectCoverage")
                    if fam in ("vitest", "js-pm-script") and cfg["vitest_cov_enabled"]:
                        cov_cfg.append("vitest coverage.enabled")
                    if fam in ("gradle", "maven") and (cfg["jacoco"] or cfg["kover"]):
                        cov_cfg.append("jacoco/kover plugin")
                    if fam == "ruby" and cfg["simplecov"]:
                        cov_cfg.append("simplecov")
                    junit_cfg = []
                    if fam in ("gradle", "maven", "sbt"):
                        junit_cfg.append("build tool writes XML by default")
                    if fam == "cargo-test" and cfg["nextest_junit"] and "nextest" in line:
                        junit_cfg.append("nextest junit profile")
                    if fam == "pytest" and cfg["pytest_addopts_junit"]:
                        junit_cfg.append("pytest addopts")
                    if fam == "bazel-test":
                        junit_cfg.append("bazel writes test.xml")
                    test_steps.append({
                        "repo": rname, "workflow": k, "job": jid, "step": str(s.get("name") or "")[:80],
                        "in_action": s.get("__action__"),
                        "family": fam, "line": line[:300], "expanded": expanded[:300],
                        "variants": variants, "matrix_countable": countable,
                        "matrix_multi": (variants is None) or (variants >= 2) or (not countable),
                        "in_reusable": is_reusable, "per_change": per_change,
                        "triggers": sorted(trig.keys()), "runs_on": rk,
                        "services": bool(job.get("services")), "container": bool(job.get("container")),
                        "cov_direct": cov_direct, "cov_cfg": cov_cfg, "cov_followed": cov_followed,
                        "junit_direct": junit_direct, "junit_cfg": junit_cfg, "junit_followed": junit_followed,
                        "inner_family": inner_fam, "followed_chars": len(followed),
                        "job_uploads_cov": None, "job_publishes_tests": None,
                        "job_text_cov": [n for n, rx in P.COV_GEN if rx.search(job_text)],
                    })
            # what the job does with reports, after its test steps
            job_uses = [u for (wk, jj, u, w) in uses_all if wk == k and jj == jid]
            jcov = sorted({name for name, rx in P.COV_UPLOAD_ACTIONS for u in job_uses if rx.search(u)} |
                          {name for name, rx in P.COV_UPLOAD_CLI if rx.search(job_text)})
            if any(re.match(r"^codecov/codecov-action", u, re.I) for u in job_uses):
                pass
            jtests = sorted({name for name, rx in P.TEST_REPORT_ACTIONS for u in job_uses if rx.search(u)} |
                            {name for name, rx in P.TEST_REPORT_CLI if rx.search(job_text)})
            jobs_out.append({
                "repo": rname, "workflow": k, "job": jid, "name": str(job.get("name") or "")[:80],
                "triggers": sorted(trig.keys()), "per_change": per_change, "in_reusable": is_reusable,
                "variants": variants, "matrix_countable": countable,
                "matrix_multi": (variants is None) or (variants >= 2) or (not countable),
                "runs_on": rk, "services": bool(job.get("services")), "container": bool(job.get("container")),
                "if": str(job.get("if"))[:120] if job.get("if") is not None else None,
                "uploads_cov": jcov, "publishes_tests": jtests,
                "step_summary": bool(P.STEP_SUMMARY.search(job_text)),
                "artifact_reports": bool(re.search(
                    r"uses: actions/upload-artifact[^\n]*(junit|test-results?|test_results|surefire|reports?|coverage|lcov|\.xml|trx|testlogs|cov)", job_text, re.I)),
                "cov_comment_action": any(P.COV_COMMENT_ACTIONS.search(u) for u in job_uses),
                "pr_comment_action": any(P.PR_COMMENT_ACTIONS.search(u) for u in job_uses),
                "test_lines": sum(1 for ts in test_steps if ts["workflow"] == k and ts["job"] == jid),
                "cov_gen_text": [n for n, rx in P.COV_GEN if rx.search(job_text)],
                "junit_text": bool(P.JUNIT_GEN.search(job_text)),
                "id_token_job": isinstance(jp, dict) and str(jp.get("id-token")) == "write",
                "secret_vars": secret_vars,
                "local_actions": sorted({str(s.get("uses")) for s in steps if str(s.get("uses") or "").startswith("./") and "__action__" not in s}),
            })
            for ts in test_steps:
                if ts["workflow"] == k and ts["job"] == jid and ts["job_uploads_cov"] is None:
                    ts["job_uploads_cov"] = jcov
                    ts["job_publishes_tests"] = jtests
                    ts["job_step_summary"] = bool(P.STEP_SUMMARY.search(job_text))
                    ts["job_uploads_artifact_reports"] = bool(re.search(
                        r"uses: actions/upload-artifact[^\n]*(junit|test-results?|test_results|surefire|reports?|coverage|lcov|\.xml|trx|testlogs|cov)", job_text, re.I))

    res["gha"] = {
        "workflows": len(wf_files), "jobs": jobs_total, "triggers": trig_counts, **agg,
        "runners": runners,
    }
    # ---- repo-wide action and CLI usage (workflows + composite actions)
    all_uses = [u for (_, _, u, _) in uses_all]
    res["actions_used_count"] = len(set(all_uses))
    run_blob = "\n".join(run_text_all)
    # other CI files and CI scripts (text)
    other_texts = []
    for k, p in rel.items():
        if k.startswith(".github/workflows/"):
            continue
        if (any(k in v for v in other_ci.values()) or re.search(r"(^|/)(\.?ci|ci_scripts|ci-scripts|cicd|\.circleci|\.buildkite|pipelines)/", k)
                or k.startswith((".github/actions/", ".github/scripts/", ".github/ci", ".github/workflows-src", "eng/", "build/azure-pipelines", ".ci/"))
                or k in ("Makefile", "justfile", "Justfile", "Taskfile.yml", "noxfile.py", "tox.ini", "Rakefile", "x.py", ".ci.yaml")):
            other_texts.append((k, read(p)))
    other_blob = "\n".join(t for _, t in other_texts)
    pkg_blob = "\n".join(read(p) for k, p in rel.items() if k.endswith("package.json"))
    other_blob += "\n" + "\n".join(read(p) for k, p in rel.items() if re.match(r"(settings\.gradle(\.kts)?|build\.gradle(\.kts)?|gradle\.properties|\.mvn/extensions\.xml|pom\.xml)$", k))

    def hits(rxlist, *blobs):
        return sorted({name for name, rx in rxlist if any(rx.search(b) for b in blobs)})

    res["cov_upload"] = {
        "workflow_actions": sorted({name for name, rx in P.COV_UPLOAD_ACTIONS for u in all_uses if rx.search(u)}),
        "workflow_cli": hits(P.COV_UPLOAD_CLI, run_blob),
        "other_ci_or_scripts": hits(P.COV_UPLOAD_CLI, other_blob),
        "codecov_action_versions": sorted({u.split("@")[-1][:12] for u in all_uses if re.match(r"codecov/codecov-action", u, re.I)}),
        "codecov_with_flags": any(isinstance(w, dict) and w.get("flags") for (_, _, u, w) in uses_all if re.match(r"codecov/codecov-action", u, re.I)),
        "codecov_test_results": any((re.match(r"codecov/test-results-action", u, re.I)) or (isinstance(w, dict) and str(w.get("report_type", "")).startswith("test_results")) for (_, _, u, w) in uses_all) or bool(re.search(r"report[_-]type[:= ]+['\"]?test_results", run_blob + other_blob)),
    }
    res["cov_comment_actions"] = sorted({u.split("@")[0] for u in all_uses if P.COV_COMMENT_ACTIONS.search(u)})
    res["cov_generation_anywhere"] = hits(P.COV_GEN, run_blob, other_blob, pkg_blob)
    res["test_report_actions"] = sorted({name for name, rx in P.TEST_REPORT_ACTIONS for u in all_uses if rx.search(u)})
    res["test_report_actions_raw"] = sorted({u.split("@")[0] for u in all_uses if any(rx.search(u) for _, rx in P.TEST_REPORT_ACTIONS)})
    res["test_report_cli"] = hits(P.TEST_REPORT_CLI, run_blob, other_blob)
    res["junit_anywhere"] = bool(P.JUNIT_GEN.search(run_blob + other_blob + pkg_blob))
    res["upload_artifact_reports"] = any(
        re.match(r"actions/upload-artifact", u, re.I) and isinstance(w, dict) and re.search(
            r"junit|test-results?|test_results|surefire|reports?|coverage|lcov|\.xml|\.trx|testlogs|cov", json.dumps(w, default=str), re.I)
        for (_, _, u, w) in uses_all)
    res["pr_comment_actions"] = sorted({u.split("@")[0] for u in all_uses if P.PR_COMMENT_ACTIONS.search(u)})
    res["build_cache"] = sorted(set(hits(P.BUILD_CACHE, run_blob, other_blob)) | {name for name, rx in P.BUILD_CACHE for u in all_uses if rx.search(u)})
    res["change_detection"] = sorted({name for name, rx in P.CHANGE_DETECT[:1] for u in all_uses if rx.search(u)} |
                                     ({"git diff script"} if P.CHANGE_DETECT[1][1].search(run_blob) else set()) |
                                     ({"on.paths filters"} if agg["paths_filter_trigger"] else set()))
    res["other_ci_signals"] = {
        "circleci_store_test_results": bool(re.search(r"store_test_results", other_blob)),
        "azure_publish_test_results": bool(re.search(r"PublishTestResults|publishTestResults", other_blob)),
        "azure_publish_coverage": bool(re.search(r"PublishCodeCoverageResults", other_blob)),
        "buildkite_junit_annotate": bool(re.search(r"junit-annotate|test-collector|BUILDKITE_ANALYTICS_TOKEN", other_blob)),
        "buildkite_artifact_paths": bool(re.search(r"artifact_paths", other_blob)),
    }
    res["providers"] = sorted(providers)
    res["test_steps_n"] = len(test_steps)
    res["_jobs"] = jobs_out
    return res, test_steps


TESTY_JOB = None


def trig(y):
    on = y.get("on", y.get(True))
    if isinstance(on, str):
        return {on: {}}
    if isinstance(on, list):
        return {str(t): {} for t in on}
    if isinstance(on, dict):
        return {str(k): (v if isinstance(v, dict) else {}) for k, v in on.items()}
    return {}


def extra_facts(rname, jobs):
    """extra.py's repository facts: test workflows on push and pull_request, on merge_group, with
    path filters, and jobs that use a paths-filter action."""
    d = REPOS / FETCHED[rname]["key"]
    testjob = {(j["workflow"], j["job"]) for j in jobs if j["repo"] == rname and j["test_lines"] > 0}
    out = {"push_and_pr_test_workflows": [], "merge_group_test_workflows": [],
           "paths_filtered_test_workflows": [], "paths_filter_action_jobs": []}
    wfd = d / ".github" / "workflows"
    for wf in sorted(wfd.glob("*.y*ml")) if wfd.exists() else []:
        k = str(wf.relative_to(d))
        y = load_yaml(wf)
        if not isinstance(y, dict) or "__error__" in y:
            continue
        tr = trig(y)
        wjobs = y.get("jobs") if isinstance(y.get("jobs"), dict) else {}
        has_test = any((k, jid) in testjob for jid in wjobs)
        if has_test and "pull_request" in tr and "push" in tr:
            br = tr["push"].get("branches") if isinstance(tr["push"], dict) else None
            if br is None or br:
                out["push_and_pr_test_workflows"].append(k)
        if has_test and "merge_group" in tr:
            out["merge_group_test_workflows"].append(k)
        if has_test and any(isinstance(tr.get(x), dict) and ("paths" in tr[x] or "paths-ignore" in tr[x])
                            for x in ("push", "pull_request", "merge_group")):
            out["paths_filtered_test_workflows"].append(k)
        for jid, job in wjobs.items():
            if not isinstance(job, dict):
                continue
            steps = [s for s in job.get("steps") or [] if isinstance(s, dict)]
            if any(re.match(r"(dorny/paths-filter|tj-actions/changed-files|step-security/changed-files|getsentry/paths-filter|fkirc/skip-duplicate-actions|Ana06/get-changed-files)", str(s.get("uses") or ""), re.I) for s in steps):
                out["paths_filter_action_jobs"].append(f"{k}:{jid}")
    return out


def main():
    global REPOS
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--repos", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--only")
    args = parser.parse_args()
    REPOS = args.repos
    entries = json.loads(args.corpus.read_text())["repositories"]
    if args.only:
        wanted = set(args.only.split(","))
        entries = [e for e in entries if e["repo"] in wanted]
    for e in entries:
        CATEGORY[e["repo"]] = e["language"]
        FETCHED[e["repo"]] = {"branch": e["branch"], "sha": e["commit"],
                              "key": f"{e['repo'].replace('/', '__')}@{e['commit'][:12]}"}
    allres, allsteps, alljobs = [], [], []
    for rname in FETCHED:
        try:
            r, steps = analyze_repo(rname)
        except Exception as e:  # noqa: BLE001
            import traceback
            traceback.print_exc()
            r, steps = {"repo": rname, "error": str(e)}, []
        jobs = r.pop("_jobs", [])
        r["extra"] = extra_facts(rname, jobs) if "error" not in r else {}
        alljobs.extend(jobs)
        allres.append(r)
        allsteps.extend(steps)
    args.out.write_text(json.dumps({"repos": allres, "jobs": alljobs, "steps": allsteps},
                                   indent=1, default=str) + "\n")
    errors = [r["repo"] for r in allres if "error" in r]
    print(f"{len(allres)} repositories, {len(alljobs)} jobs, {len(allsteps)} test lines"
          + (f"; FAILED to read {', '.join(errors)}" if errors else ""))
    if errors:
        sys.exit(1)


if __name__ == "__main__":
    main()
