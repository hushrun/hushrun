#!/usr/bin/env python3
"""An independent reference for report summaries (C-27), standard library only.

It reads test and coverage reports as hushrun-reports does, written apart from it, so that the
two can be checked against each other:

    python3 -I tools/refreports.py check testdata/reports/vectors.json

Each vector names report files, relative to the vectors file, and the summary hushrun records
for them. `check` computes each summary here and says which disagree. Python's XML parsers expand
a DOCTYPE's entities, so this reads XML with a small tokenizer of its own, which skips the
DOCTYPE whole and never expands an entity: only element names and numeric attributes are read.
"""

import json
import math
import os
import re
import stat
import sys
from datetime import datetime, timezone

MOST_FILES = 16
LONGEST_PATH = 160
MOST_BYTES = 4096
MOST_COUNT = 10**9
MOST_DURATION_MS = 10**11
FILE_BYTES = 64 << 20
TOTAL_BYTES = 256 << 20
MOST_REPORTS = 5000
SNIFF_BYTES = 64 << 10

TESTS_FORMATS = ("junit", "trx")
COVERAGE_ORDER = ("lcov", "cobertura", "jacoco", "clover", "opencover", "go", "coveragepy", "istanbul", "simplecov")


class NotAReport(Exception):
    pass


class Malformed(Exception):
    pass


# XML -------------------------------------------------------------------------------------------

NAME = r"[A-Za-z_:][-A-Za-z0-9_:.]*"
START = re.compile(r"<(" + NAME + r")((?:\s+" + NAME + r"\s*=\s*(?:\"[^\"]*\"|'[^']*'))*)\s*(/?)>")
END = re.compile(r"</(" + NAME + r")\s*>")
ATTR = re.compile(r"(" + NAME + r")\s*=\s*(?:\"([^\"]*)\"|'([^']*)')")


def local(name):
    return name.split(":")[-1]


def tags(text):
    """Each tag of `text`: ("start" | "empty", name, attributes) or ("end", name)."""
    i, n = 0, len(text)
    while True:
        j = text.find("<", i)
        if j < 0:
            return
        if text.startswith("<!--", j):
            k = text.find("-->", j + 4)
            if k < 0:
                raise Malformed("an unclosed comment")
            i = k + 3
        elif text.startswith("<![CDATA[", j):
            k = text.find("]]>", j + 9)
            if k < 0:
                raise Malformed("an unclosed CDATA section")
            i = k + 3
        elif text.startswith("<?", j):
            k = text.find("?>", j + 2)
            if k < 0:
                raise Malformed("an unclosed processing instruction")
            i = k + 2
        elif text.startswith("<!DOCTYPE", j):
            # To its `>`, past an internal subset in brackets and anything quoted.
            k, depth, quote = j + 9, 0, None
            while k < n:
                c = text[k]
                if quote:
                    if c == quote:
                        quote = None
                elif c in "\"'":
                    quote = c
                elif c == "[":
                    depth += 1
                elif c == "]":
                    depth -= 1
                elif c == ">" and depth <= 0:
                    break
                k += 1
            if k >= n:
                raise Malformed("an unclosed DOCTYPE")
            i = k + 1
        elif text.startswith("</", j):
            m = END.match(text, j)
            if not m:
                raise Malformed("a broken end tag")
            yield ("end", local(m.group(1)))
            i = m.end()
        else:
            m = START.match(text, j)
            if not m:
                raise Malformed("a broken tag")
            attrs = {}
            for a in ATTR.finditer(m.group(2)):
                attrs.setdefault(local(a.group(1)), a.group(2) if a.group(2) is not None else a.group(3))
            yield ("empty" if m.group(3) else "start", local(m.group(1)), attrs)
            i = m.end()


class Doc:
    """A document's root, then each tag below it with its depth (the root's children at 2)."""

    def __init__(self, text):
        self.stack = []
        self.tags = tags(text)
        self.root = None
        for tag in self.tags:
            if tag[0] in ("start", "empty"):
                self.root = tag
                if tag[0] == "start":
                    self.stack.append(tag[1])
                break
            raise Malformed("an end tag before the root")
        if self.root is None:
            raise NotAReport()

    def below(self):
        """(kind, name, attrs, depth): opens and closes, until the root closes."""
        if self.root[0] == "empty":
            return
        for tag in self.tags:
            if tag[0] == "start":
                self.stack.append(tag[1])
                yield ("open", tag[1], tag[2], len(self.stack), True)
            elif tag[0] == "empty":
                yield ("open", tag[1], tag[2], len(self.stack) + 1, False)
            else:
                if not self.stack or self.stack[-1] != tag[1]:
                    raise Malformed("an end tag that doesn't match")
                depth = len(self.stack)
                self.stack.pop()
                yield ("close", tag[1], None, depth, None)
                if not self.stack:
                    return
        raise Malformed("it ends before its root closes")


def number(attrs, name):
    value = attrs.get(name)
    if value is None:
        return None
    value = value.strip()
    if not value.isdigit():
        return None
    return int(value)


def needed(attrs, name, element):
    value = number(attrs, name)
    if value is None:
        raise Malformed("%s has no count in %s" % (element, name))
    return value


def seconds(attrs, name):
    value = attrs.get(name)
    if value is None:
        return None
    value = value.strip()
    value = value.replace(",", "") if "." in value else value.replace(",", ".")
    try:
        s = float(value)
    except ValueError:
        return None
    ms = s * 1000.0
    # Half away from zero, as Rust rounds.
    ms = math.floor(ms + 0.5) if ms >= 0 else -math.floor(-ms + 0.5)
    if not math.isfinite(ms) or not (0 <= ms <= 1e11):
        return None
    return int(ms)


def ratio(covered, total):
    return None if total <= 0 else (min(covered, total), total)


def junit(doc):
    t = {"total": 0, "failed": 0, "errors": 0, "skipped": 0}
    suites, without_time = None, False
    case = None

    def count(c):
        t["total"] += 1
        if c["failure"]:
            t["failed"] += 1
        elif c["error"]:
            t["errors"] += 1
        elif c["skipped"]:
            t["skipped"] += 1

    for kind, name, attrs, depth, start in doc.below():
        if kind == "open":
            if name == "testsuite" and depth == 2:
                ms = seconds(attrs, "time")
                if ms is None:
                    without_time = True
                else:
                    suites = (suites or 0) + ms
            elif name == "testcase" and case is None:
                this = {"depth": depth, "failure": False, "error": False, "skipped": False}
                if start:
                    case = this
                else:
                    count(this)
            elif name in ("failure", "error", "skipped") and case is not None and case["depth"] + 1 == depth:
                case[{"failure": "failure", "error": "error", "skipped": "skipped"}[name]] = True
        elif case is not None and case["depth"] == depth:
            count(case)
            case = None
    ms = seconds(doc.root[2], "time")
    if ms is None and not without_time:
        ms = suites
    if ms is not None:
        t["durationMs"] = ms
    return t


def timestamp_ms(text):
    text = text.strip()
    m = re.match(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:\d{2})$", text)
    if not m:
        return None
    base = datetime.fromisoformat(m.group(1) + ("+00:00" if m.group(3) == "Z" else m.group(3)))
    epoch = datetime(1970, 1, 1, tzinfo=timezone.utc)
    whole = (base - epoch).days * 86400 + (base - epoch).seconds
    fraction = (m.group(2) or "0").ljust(9, "0")[:9]
    ns = whole * 10**9 + int(fraction)
    return ns // 10**6


def trx(doc):
    if doc.root[0] == "empty":
        raise Malformed("a TRX file without a ResultSummary")
    tests, times, in_summary = None, None, False
    for kind, name, attrs, depth, start in doc.below():
        if kind == "open":
            if name == "Times" and depth == 2 and "start" in attrs and "finish" in attrs:
                times = (attrs["start"], attrs["finish"])
            elif name == "ResultSummary" and depth == 2:
                in_summary = start
            elif name == "Counters" and depth == 3 and in_summary:
                some = lambda n: number(attrs, n) or 0
                tests = {
                    "total": needed(attrs, "total", "Counters"),
                    "failed": some("failed") + some("timeout") + some("aborted"),
                    "errors": some("error"),
                    "skipped": some("notExecuted"),
                }
        elif depth == 2:
            in_summary = False
    if tests is None:
        raise Malformed("a TRX file without Counters")
    if times:
        start, finish = timestamp_ms(times[0]), timestamp_ms(times[1])
        if start is not None and finish is not None and finish >= start:
            tests["durationMs"] = finish - start
    return tests


def condition(text):
    m = re.search(r"\(([^)]*)\)", text or "")
    if not m or "/" not in m.group(1):
        return None
    covered, total = m.group(1).split("/", 1)
    covered, total = covered.strip(), total.strip()
    if not (covered.isdigit() and total.isdigit()):
        return None
    return int(covered), int(total)


def cobertura(doc):
    seen, files, current = {}, {}, None
    for kind, name, attrs, depth, start in doc.below():
        if kind != "open":
            continue
        if name == "class":
            current = files.setdefault(attrs.get("filename", ""), len(files))
        elif name == "line" and current is not None and number(attrs, "number") is not None:
            key = (current, number(attrs, "number"))
            hits = number(attrs, "hits") or 0
            covered, total = condition(attrs.get("condition-coverage")) or (0, 0)
            old = seen.get(key, (0, 0, 0))
            seen[key] = (max(old[0], hits), max(old[1], covered), max(old[2], total))
    root = doc.root[2]

    def attrs(covered, valid):
        c, v = number(root, covered), number(root, valid)
        return (c, v) if c is not None and v is not None and v > 0 else None

    lines = attrs("lines-covered", "lines-valid") or (sum(1 for h, _, _ in seen.values() if h > 0), len(seen))
    branches = attrs("branches-covered", "branches-valid") or (
        sum(c for _, c, _ in seen.values()), sum(t for _, _, t in seen.values()))
    return {"lines": ratio(*lines), "branches": ratio(*branches)}


def jacoco(doc):
    counts, found = {}, False
    for kind, name, attrs, depth, start in doc.below():
        if kind == "open" and depth == 2 and name == "counter":
            kind_of = attrs.get("type")
            if kind_of in ("LINE", "BRANCH"):
                missed = needed(attrs, "missed", "counter")
                covered = needed(attrs, "covered", "counter")
                counts["lines" if kind_of == "LINE" else "branches"] = ratio(covered, covered + missed)
                found = found or kind_of == "LINE"
    if not found:
        raise NotAReport()
    return counts


def clover(doc):
    counts, in_project = None, False
    for kind, name, attrs, depth, start in doc.below():
        if kind == "open":
            if name == "project" and depth == 2:
                in_project = start
            elif name == "metrics" and depth == 3 and in_project:
                statements = needed(attrs, "statements", "metrics")
                covered = needed(attrs, "coveredstatements", "metrics")
                total, hit = number(attrs, "conditionals"), number(attrs, "coveredconditionals")
                counts = {"lines": ratio(covered, statements),
                          "branches": ratio(hit, total) if total is not None and hit is not None else None}
        elif depth == 2:
            in_project = False
    if counts is None:
        raise NotAReport()
    return counts


def opencover(doc):
    counts = None
    for kind, name, attrs, depth, start in doc.below():
        if kind == "open" and depth == 2 and name == "Summary":
            points = needed(attrs, "numSequencePoints", "Summary")
            visited = needed(attrs, "visitedSequencePoints", "Summary")
            total, hit = number(attrs, "numBranchPoints"), number(attrs, "visitedBranchPoints")
            counts = {"lines": ratio(visited, points),
                      "branches": ratio(hit, total) if total is not None and hit is not None else None}
    if counts is None:
        raise NotAReport()
    return counts


def xml_format(root):
    name, attrs = root[1], root[2]
    if name in ("testsuites", "testsuite"):
        return "junit"
    if name == "TestRun":
        return "trx"
    if name == "coverage":
        return "cobertura" if "line-rate" in attrs else "clover"
    if name == "report":
        return "jacoco"
    if name == "CoverageSession":
        return "opencover"
    return None


def read_xml(text):
    doc = Doc(text)
    fmt = xml_format(doc.root)
    if fmt is None:
        raise NotAReport()
    return fmt, {"junit": junit, "trx": trx, "cobertura": cobertura, "jacoco": jacoco,
                 "clover": clover, "opencover": opencover}[fmt](doc)


# Text ------------------------------------------------------------------------------------------

def text_format(text):
    for line in text.split("\n"):
        line = line.strip()
        if line:
            if line in ("mode: set", "mode: count", "mode: atomic"):
                return "go"
            if line.startswith("TN:") or line.startswith("SF:"):
                return "lcov"
            return None
    return None


def whole(text):
    text = text.strip()
    if not text.isdigit():
        raise Malformed("not a count: %r" % text)
    return int(text)


def lcov(text):
    lines, branches, record = None, None, None

    def close(r):
        nonlocal lines, branches
        if r["lf"] is not None and r["lh"] is not None:
            l = ratio(r["lh"], r["lf"])
        else:
            l = ratio(sum(1 for h in r["da"].values() if h), len(r["da"]))
        if r["brf"] is not None and r["brh"] is not None:
            b = ratio(r["brh"], r["brf"])
        else:
            b = ratio(sum(1 for t in r["brda"].values() if t), len(r["brda"]))
        lines, branches = add(lines, l), add(branches, b)

    for line in text.split("\n"):
        line = line.strip()
        if line == "end_of_record":
            if record is not None:
                close(record)
                record = None
            continue
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        if key == "SF":
            if record is not None:
                close(record)
            record = {"lf": None, "lh": None, "brf": None, "brh": None, "da": {}, "brda": {}}
            continue
        if record is None:
            continue
        if key in ("LF", "LH", "BRF", "BRH"):
            record[key.lower()] = whole(value)
        elif key == "DA":
            parts = value.split(",")
            at = whole(parts[0])
            hits = False
            if len(parts) > 1:
                try:
                    hits = float(parts[1].strip()) > 0
                except ValueError:
                    raise Malformed("DA's count isn't a number")
            record["da"][at] = record["da"].get(at, False) or hits
        elif key == "BRDA":
            parts = [p.strip() for p in value.split(",")]
            if len(parts) != 4:
                raise Malformed("a BRDA line needs four parts")
            at = whole(parts[0])
            try:
                taken = parts[3] != "-" and float(parts[3]) > 0
            except ValueError:
                taken = False
            key3 = (at, parts[1], parts[2])
            record["brda"][key3] = record["brda"].get(key3, False) or taken
    if record is not None:
        close(record)
    return {"lines": lines, "branches": branches}


def go(text):
    blocks = {}
    for line in text.split("\n"):
        line = line.strip()
        if not line or line.startswith("mode:"):
            continue
        parts = line.rsplit(" ", 2)
        if len(parts) != 3 or ":" not in parts[0] or "," not in parts[0]:
            raise Malformed("not a coverprofile block")
        block, statements, count = parts[0], whole(parts[1]), whole(parts[2])
        old = blocks.get(block, (statements, 0))
        blocks[block] = (max(old[0], statements), max(old[1], count))
    total = sum(s for s, _ in blocks.values())
    covered = sum(s for s, c in blocks.values() if c > 0)
    return {"statements": ratio(covered, total)}


def read_text(text):
    fmt = text_format(text)
    if fmt is None:
        raise NotAReport()
    return fmt, (go if fmt == "go" else lcov)(text)


# JSON ------------------------------------------------------------------------------------------

def is_number(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def pointer(value, *keys):
    for key in keys:
        if not isinstance(value, dict) or key not in value:
            return None
        value = value[key]
    return value


def count_at(value, *keys):
    v = pointer(value, *keys)
    if not isinstance(v, int) or isinstance(v, bool) or v < 0:
        raise Malformed("no count at /" + "/".join(keys))
    return v


def optional_count(value, *keys):
    v = pointer(value, *keys)
    return v if isinstance(v, int) and not isinstance(v, bool) and v >= 0 else None


def json_format(text):
    if '"total"' in text and '"lines"' in text:
        return "istanbul"
    if '"show_contexts"' in text or ('"totals"' in text and '"num_statements"' in text):
        return "coveragepy"
    if '"coverage"' in text:
        return "simplecov"
    return None


def read_json(data):
    try:
        value = json.loads(data.decode("utf-8"))
    except (ValueError, UnicodeDecodeError) as e:
        raise Malformed("not JSON: %s" % e)
    if is_number(pointer(value, "total", "lines", "total")):
        covered, total = optional_count(value, "total", "branches", "covered"), optional_count(value, "total", "branches", "total")
        return "istanbul", {"lines": ratio(count_at(value, "total", "lines", "covered"), count_at(value, "total", "lines", "total")),
                            "branches": ratio(covered, total) if covered is not None and total is not None else None}
    if is_number(pointer(value, "totals", "num_statements")):
        covered, total = optional_count(value, "totals", "covered_branches"), optional_count(value, "totals", "num_branches")
        return "coveragepy", {"lines": ratio(count_at(value, "totals", "covered_lines"), count_at(value, "totals", "num_statements")),
                              "branches": ratio(covered, total) if covered is not None and total is not None else None}
    if isinstance(value, dict) and value and all(isinstance(e, dict) and isinstance(e.get("coverage"), dict) for e in value.values()):
        files = {}
        for entry in value.values():
            for path, cov in entry["coverage"].items():
                lines, branches = files.setdefault(path, ({}, {}))
                if isinstance(cov, list):
                    these = cov
                elif isinstance(cov, dict):
                    these = cov.get("lines") if isinstance(cov.get("lines"), list) else []
                else:
                    raise Malformed("a file's coverage is neither lines nor an object")
                for at, hits in enumerate(these):
                    if is_number(hits):
                        lines[at] = lines.get(at, False) or hits > 0
                conditions = cov.get("branches") if isinstance(cov, dict) else None
                if isinstance(conditions, dict):
                    for cond, arms in conditions.items():
                        if isinstance(arms, dict):
                            for arm, hits in arms.items():
                                key = (cond, arm)
                                branches[key] = branches.get(key, False) or (is_number(hits) and hits > 0)
        all_lines = [c for lines, _ in files.values() for c in lines.values()]
        all_branches = [c for _, branches in files.values() for c in branches.values()]
        return "simplecov", {"lines": ratio(sum(all_lines), len(all_lines)),
                             "branches": ratio(sum(all_branches), len(all_branches))}
    raise NotAReport()


# Summaries -------------------------------------------------------------------------------------

def strip(data):
    return data[3:] if data.startswith(b"\xef\xbb\xbf") else data


def first_byte(data):
    for b in data:
        if b not in b" \t\r\n\x0b\x0c":
            return b
    return None


def utf8(data):
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError:
        raise Malformed("not UTF-8")


def read(data):
    data = strip(data)
    b = first_byte(data)
    if b is None:
        raise NotAReport()
    if b == ord("<"):
        return read_xml(utf8(data))
    if b == ord("{"):
        return read_json(data)
    return read_text(utf8(data))


def sniff(prefix):
    prefix = strip(prefix)
    if first_byte(prefix) is None:
        return None
    try:
        text = prefix.decode("utf-8")
    except UnicodeDecodeError as e:
        text = prefix[: e.start].decode("utf-8")
    text = text.lstrip(" \t\r\n\x0b\x0c")
    if text.startswith("<"):
        try:
            return xml_format(Doc(text).root)
        except (Malformed, NotAReport):
            return None
    if text.startswith("{"):
        return json_format(text)
    return text_format(text)


def add(a, b):
    if a is None:
        return b
    if b is None:
        return a
    return (a[0] + b[0], a[1] + b[1])


def summarize(found):
    """found: [(path, rel)] -> the summary dict, or None."""
    seen, unique = set(), []
    for path, rel in found:
        if path not in seen:
            seen.add(path)
            unique.append((path, rel))
    if len(unique) > MOST_REPORTS:
        return None
    sniffed, too_big = [], {"tests": False, "coverage": False}
    for path, rel in unique:
        try:
            st = os.lstat(path)
        except OSError:
            continue
        if not stat.S_ISREG(st.st_mode):
            continue
        with open(path, "rb") as f:
            fmt = sniff(f.read(SNIFF_BYTES))
        if fmt is None:
            continue
        kind = "tests" if fmt in TESTS_FORMATS else "coverage"
        if st.st_size > FILE_BYTES:
            too_big[kind] = True
            continue
        sniffed.append((path, rel, st.st_size, kind))
    read_files = []
    for kind in ("tests", "coverage"):
        if too_big[kind]:
            continue
        ours = [s for s in sniffed if s[3] == kind]
        if sum(s[2] for s in ours) > TOTAL_BYTES:
            continue
        for path, rel, _, _ in ours:
            with open(path, "rb") as f:
                data = f.read()
            try:
                fmt, said = read(data)
            except (NotAReport, Malformed):
                continue
            read_files.append((rel, fmt, said))
    return reports_of(read_files)


def reports_of(read_files):
    tests, durations = None, True
    for _, fmt, said in read_files:
        if fmt in TESTS_FORMATS:
            durations = durations and "durationMs" in said
            if tests is None:
                tests = {"total": 0, "failed": 0, "errors": 0, "skipped": 0, "durationMs": 0}
            for key in ("total", "failed", "errors", "skipped"):
                tests[key] += said[key]
            tests["durationMs"] += said.get("durationMs", 0)
    if tests is not None and (not durations or tests["durationMs"] > MOST_DURATION_MS):
        del tests["durationMs"]
    # Counts past what a record holds are no summary, never a partial one.
    if tests is not None and any(tests[k] > MOST_COUNT for k in ("total", "failed", "errors", "skipped")):
        tests = None
    coverage = None
    for wanted in COVERAGE_ORDER:
        counts = [said for _, fmt, said in read_files if fmt == wanted]
        if not counts:
            continue
        summed = {}
        for c in counts:
            for key in ("lines", "statements", "branches"):
                summed[key] = add(summed.get(key), c.get(key))
        if any(summed.get(k) is not None for k in ("lines", "statements", "branches")):
            coverage = {"format": wanted}
            for key in ("lines", "statements", "branches"):
                if summed.get(key) is not None:
                    coverage[key] = {"covered": summed[key][0], "total": summed[key][1]}
            if any(v["total"] > MOST_COUNT for k, v in coverage.items() if k != "format"):
                coverage = None
            break
    if tests is None and coverage is None:
        return None
    named = sorted({rel: fmt for rel, fmt, _ in read_files}.items())
    reports = {"files": [], "filesLeftOut": 0}
    for rel, fmt in named:
        entry = {"path": rel, "kind": "tests" if fmt in TESTS_FORMATS else "coverage", "format": fmt}
        if len(reports["files"]) < MOST_FILES and len(rel.encode("utf-8")) <= LONGEST_PATH:
            reports["files"].append(entry)
        else:
            reports["filesLeftOut"] += 1
    if tests is not None:
        reports["tests"] = tests
    if coverage is not None:
        reports["coverage"] = coverage

    def size(r):
        shown = {k: v for k, v in r.items() if not (k == "files" and not v) and not (k == "filesLeftOut" and v == 0)}
        return len(json.dumps(shown, separators=(",", ":"), ensure_ascii=False).encode("utf-8"))

    while size(reports) > MOST_BYTES and reports["files"]:
        reports["files"].pop()
        reports["filesLeftOut"] += 1
    if not reports["files"]:
        del reports["files"]
    if reports["filesLeftOut"] == 0:
        del reports["filesLeftOut"]
    return reports


def check(vectors_path):
    base = os.path.dirname(os.path.abspath(vectors_path))
    with open(vectors_path, encoding="utf-8") as f:
        doc = json.load(f)
    wrong = 0
    for vector in doc["vectors"]:
        found = [(os.path.join(base, rel), rel) for rel in vector["files"]]
        got = summarize(found)
        if got != vector["reports"]:
            wrong += 1
            print("disagrees: %s\n  expected %s\n  got      %s" % (
                vector["name"], json.dumps(vector["reports"]), json.dumps(got)))
    print("%d of %d vectors agree" % (len(doc["vectors"]) - wrong, len(doc["vectors"])))
    return 1 if wrong else 0


def main(argv):
    if len(argv) == 3 and argv[1] == "check":
        return check(argv[2])
    print("usage: refreports.py check testdata/reports/vectors.json", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
