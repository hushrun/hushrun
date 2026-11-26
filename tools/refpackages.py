#!/usr/bin/env python3
"""Independent reference implementation of a package store's three digests (K-2): a part's
identity, a store's identity and a part's inputs.

Written from docs/spec/11-baked-packages.md, not from the Rust code, so the two can check each
other on testdata/packages/vectors.json. Standard library only; Python 3.9 or later.

Usage:
  refpackages.py check FILE      check every vector's expected digests; exit 1 on a difference
  refpackages.py update FILE     rewrite every vector's expected digests
"""

import hashlib
import json
import sys


def digest(fields):
    """The SHA-256 of each field's UTF-8 bytes followed by one NUL byte, as 64 hex."""
    h = hashlib.sha256()
    for field in fields:
        h.update(field.encode("utf-8"))
        h.update(b"\0")
    return h.hexdigest()


def part_identity(part):
    fields = ["hushrun.packages-part/v1", part["part"], part["class"], part.get("runtime") or ""]
    for row in sorted(part["rows"], key=lambda r: r["path"].encode("utf-8")):
        fields += [row["path"], row["kind"], row["sha256"]]
    return digest(fields)


def store_identity(parts):
    fields = ["hushrun.baked-packages-id/v1"]
    for name, identity in sorted(parts, key=lambda p: p[0].encode("utf-8")):
        fields += [name, identity]
    return digest(fields)


def inputs(case):
    fields = ["hushrun.packages-inputs/v1", case["part"], case["tool"], case["platform"]]
    for m in sorted(case["manifests"], key=lambda m: m["path"].encode("utf-8")):
        fields += [m["path"], m["sha256"]]
    return digest(fields)


def compute(vector):
    out = {}
    ids = []
    for part in vector["parts"]:
        out["part:" + part["part"]] = part_identity(part)
        ids.append((part["part"], out["part:" + part["part"]]))
    out["store"] = store_identity(ids)
    for case in vector["inputs"]:
        out["inputs:" + case["part"]] = inputs(case)
    return out


def main(argv):
    if len(argv) != 3 or argv[1] not in ("check", "update"):
        print(__doc__, file=sys.stderr)
        return 2
    with open(argv[2], encoding="utf-8") as f:
        doc = json.load(f)
    bad = 0
    for vector in doc["vectors"]:
        got = compute(vector)
        if argv[1] == "update":
            vector["expected"] = got
        elif got != vector["expected"]:
            print(f"{vector['name']}: expected {vector['expected']}, computed {got}")
            bad += 1
    if argv[1] == "update":
        with open(argv[2], "w", encoding="utf-8") as f:
            json.dump(doc, f, indent=2, ensure_ascii=False)
            f.write("\n")
        return 0
    print(f"{len(doc['vectors'])} vectors, {bad} differ")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
