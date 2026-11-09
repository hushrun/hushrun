#!/usr/bin/env python3
"""Independent transparency-log implementation, to cross-verify hushrun-tlog (spec L-11, L-12).

RFC 6962's Merkle tree hash, audit paths and consistency proofs, written as the RFC's recursive
definitions (section 2.1); C2SP signed notes and checkpoints; hushrun's log entries and listing
statements. [M19] The prefix map, its proofs and range proofs of key trees, from their
definitions (L-12). Ed25519 comes from refdsse.py (RFC 8032 section 6). Standard library only;
Python 3.9 or later.

Usage:
  reftlog.py selftest     RFC 6962 test vectors, and notes Go's golang.org/x/mod/sumdb/note signed
  reftlog.py vectors      print testdata/tlog/vectors.json
  reftlog.py check FILE   exit 1 unless FILE holds exactly the vectors this computes
"""

import base64
import hashlib
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from refdsse import ed25519_sign, ed25519_verify, secret_to_public  # noqa: E402

# --- RFC 6962 section 2.1 ----------------------------------------------------------------------


def sha256(data):
    return hashlib.sha256(data).digest()


def leaf_hash(entry):
    return sha256(b"\x00" + entry)


def node_hash(left, right):
    return sha256(b"\x01" + left + right)


def split(n):
    """The largest power of two smaller than n."""
    k = 1
    while k * 2 < n:
        k *= 2
    return k


def mth(leaves):
    """MTH(D[n]) over leaf hashes."""
    if not leaves:
        return sha256(b"")
    if len(leaves) == 1:
        return leaves[0]
    k = split(len(leaves))
    return node_hash(mth(leaves[:k]), mth(leaves[k:]))


def path(m, leaves):
    """PATH(m, D[n])."""
    n = len(leaves)
    if n <= 1:
        return []
    k = split(n)
    if m < k:
        return path(m, leaves[:k]) + [mth(leaves[k:])]
    return path(m - k, leaves[k:]) + [mth(leaves[:k])]


def subproof(m, leaves, whole):
    """SUBPROOF(m, D[n], b)."""
    n = len(leaves)
    if m == n:
        return [] if whole else [mth(leaves)]
    k = split(n)
    if m <= k:
        return subproof(m, leaves[:k], whole) + [mth(leaves[k:])]
    return subproof(m - k, leaves[k:], False) + [mth(leaves[:k])]


def proof(m, leaves):
    """PROOF(m, D[n]); empty for m = 0 and m = n."""
    if m == 0 or m == len(leaves):
        return []
    return subproof(m, leaves, True)


# --- C2SP signed notes and checkpoints ---------------------------------------------------------


def key_hash(name, public):
    return sha256(name.encode() + b"\n" + b"\x01" + public)[:4]


def vkey(name, public):
    return "%s+%s+%s" % (name, key_hash(name, public).hex(), base64.b64encode(b"\x01" + public).decode())


def skey(name, seed):
    public = secret_to_public(seed)
    return "PRIVATE+KEY+%s+%s+%s" % (name, key_hash(name, public).hex(), base64.b64encode(b"\x01" + seed).decode())


def sign_note(text, name, seed):
    public = secret_to_public(seed)
    signature = ed25519_sign(seed, text.encode())
    blob = key_hash(name, public) + signature
    return text + "\n" + "— " + name + " " + base64.b64encode(blob).decode() + "\n"


def open_note(note, name, public):
    text, _, lines = note.rpartition("\n\n")
    text += "\n"
    for line in lines.rstrip("\n").split("\n"):
        prefix = "— " + name + " "
        if not line.startswith(prefix):
            continue
        blob = base64.b64decode(line[len(prefix):], validate=True)
        if blob[:4] == key_hash(name, public) and ed25519_verify(public, text.encode(), blob[4:]):
            return text
    raise ValueError("no signature by " + name)


def checkpoint_text(origin, size, root):
    return "%s\n%d\n%s\n" % (origin, size, base64.b64encode(root).decode())


# --- hushrun's entries and listing statements (L-11) -------------------------------------------


def entry(kind, name, data_sha256, gc=None):
    if kind == "map":
        return ("hushrun-tlog-entry/v1\nmap\n%s\n" % data_sha256.hex()).encode()
    text = "hushrun-tlog-entry/v1\n%s\n%s\n%s\n" % (kind, name, data_sha256.hex())
    if kind == "tombstone":
        text += "gc %s\n" % (gc.hex() if gc else "none")
    return text.encode()


def keys_hash(keys):
    return sha256("".join(k + "\n" for k in keys).encode())


def listing_text(origin, size, root, prefix, after, nxt, keys):
    return (
        "hushrun-listing/v1\norigin %s\nsize %d\nroot %s\nprefix %s\nafter %s\nnext %s\nkeys %d %s\n"
        % (origin, size, base64.b64encode(root).decode(), prefix, after or "", nxt or "", len(keys), keys_hash(keys).hex())
    )


# --- [M19] Range proofs of RFC 6962 trees (L-12) -----------------------------------------------


def range_nodes(begin, end):
    """The aligned complete subtrees that make up the leaves [begin, end), left to right."""
    nodes = []
    at = begin
    while at < end:
        level = 0
        while at % (1 << (level + 1)) == 0 and at + (1 << (level + 1)) <= end:
            level += 1
        nodes.append((at, at + (1 << level)))
        at += 1 << level
    return nodes


def range_proof(leaves, begin, end):
    """The hashes of the complete subtrees left of [begin, end) and right of it."""
    left = [mth(leaves[b:e]) for b, e in range_nodes(0, begin)]
    right = [mth(leaves[b:e]) for b, e in range_nodes(end, len(leaves))]
    return left, right


def root_from_range(size, begin, range_leaves, left, right):
    """MTH(D[size]) from the RFC's recursion, where every subtree is one the proof gives."""
    end = begin + len(range_leaves)
    known = {}
    for (b, e), h in zip(range_nodes(0, begin), left):
        known[(b, e)] = h
    for i, h in enumerate(range_leaves):
        known[(begin + i, begin + i + 1)] = h
    for (b, e), h in zip(range_nodes(end, size), right):
        known[(b, e)] = h
    if len(left) != len(range_nodes(0, begin)) or len(right) != len(range_nodes(end, size)):
        raise ValueError("a proof of the wrong length")

    def subtree(lo, hi):
        if (lo, hi) in known:
            return known[(lo, hi)]
        if hi - lo <= 1:
            raise ValueError("a leaf the proof doesn't give")
        k = split(hi - lo)
        return node_hash(subtree(lo, lo + k), subtree(lo + k, hi))

    return sha256(b"") if size == 0 else subtree(0, size)


# --- [M19] The prefix map (L-12) ---------------------------------------------------------------

MAP_EMPTY = bytes(32)


def map_prefix(key):
    if not (key.startswith("v1/events/") or key.startswith("v1/deployed/")):
        return None
    parts = key.split("/")
    if len(parts) < 5 or any(p == "" for p in parts[:4]) or "/".join(parts[4:]) == "":
        return None
    return "/".join(parts[:4]) + "/"


def key_leaf(key):
    return leaf_hash(key.encode())


def map_entry_text(prefix, keys):
    return "hushrun-map-entry/v1\n%s\n%d\n%s\n" % (prefix, len(keys), mth([key_leaf(k) for k in keys]).hex())


def map_bits(prefix):
    digest = sha256(prefix.encode())
    return [(digest[i // 8] >> (7 - i % 8)) & 1 for i in range(256)]


def map_root(entries, depth=0):
    """entries: {prefix: sorted keys}. The compacted sparse Merkle tree's hash, by definition."""
    if not entries:
        return MAP_EMPTY
    if len(entries) == 1:
        (prefix, keys), = entries.items()
        return leaf_hash(map_entry_text(prefix, keys).encode())
    left = {p: k for p, k in entries.items() if map_bits(p)[depth] == 0}
    right = {p: k for p, k in entries.items() if map_bits(p)[depth] == 1}
    return node_hash(map_root(left, depth + 1), map_root(right, depth + 1))


def map_proof(entries, prefix):
    """(path deepest first, the entry where it ends or None)."""
    bits = map_bits(prefix)
    path = []
    depth = 0
    current = entries
    while len(current) > 1:
        same = {p: k for p, k in current.items() if map_bits(p)[depth] == bits[depth]}
        other = {p: k for p, k in current.items() if map_bits(p)[depth] != bits[depth]}
        path.append(map_root(other, depth + 1))
        current = same
        depth += 1
    path.reverse()
    leaf = None
    if len(current) == 1:
        (p, keys), = current.items()
        leaf = {"prefix": p, "count": len(keys), "root": mth([key_leaf(k) for k in keys]).hex()}
    return path, leaf


def verify_map_proof(prefix, path, leaf, root):
    bits = map_bits(prefix)
    if leaf is None:
        h = MAP_EMPTY
    else:
        assert map_bits(leaf["prefix"])[: len(path)] == bits[: len(path)]
        text = "hushrun-map-entry/v1\n%s\n%d\n%s\n" % (leaf["prefix"], leaf["count"], leaf["root"])
        h = leaf_hash(text.encode())
    d = len(path)
    for i, sibling in enumerate(path):
        h = node_hash(h, sibling) if bits[d - 1 - i] == 0 else node_hash(sibling, h)
    return h == root


# --- Vectors -----------------------------------------------------------------------------------

# RFC 6962's test leaves, as Certificate Transparency's implementations use them.
CT_LEAVES = ["", "00", "10", "2021", "3031", "40414243", "5051525354555657", "606162636465666768696a6b6c6d6e6f"]
CT_ROOTS = [
    "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
    "fac54203e7cc696cf0dfcb42c92a1d9dbaf70ad9e621f4bd8d98662f00e3c125",
    "aeb6bcfe274b70a14fb067a5e5578264db0fa9b51af5e0ba159158f329e06e77",
    "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7",
    "4e3bbb1f7b478dcfe71fb631631519a3bca12c9aefca1612bfce4c13a86264d4",
    "76e67dadbcdf1e10e1b74ddc608abd2f98dfb16fbce75277b5232a127f2087ef",
    "ddb89be403809e325750d3d263cd78929c2942b7942a34b77e122c9594a74c8c",
    "5dc9da79a70659a9ad559cb701ded9a2ab9d823aad2f4960cfe370eff4604328",
]
# Proofs as Certificate Transparency's tests list them: (index, size, path) and (old, new, proof).
CT_INCLUSION = [
    (0, 8, ["96a296d224f285c67bee93c30f8a309157f0daa35dc5b87e410b78630a09cfc7",
            "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
            "6b47aaf29ee3c2af9af889bc1fb9254dabd31177f16232dd6aab035ca39bf6e4"]),
    (5, 8, ["bc1a0643b12e4d2d7c77918f44e0f4f79a838b6cf9ec5b5c283e1f4d88599e6b",
            "ca854ea128ed050b41b35ffc1b87b8eb2bde461e9e3b5596ece6b9d5975a0ae0",
            "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7"]),
    (2, 3, ["fac54203e7cc696cf0dfcb42c92a1d9dbaf70ad9e621f4bd8d98662f00e3c125"]),
    (1, 5, ["6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
            "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
            "bc1a0643b12e4d2d7c77918f44e0f4f79a838b6cf9ec5b5c283e1f4d88599e6b"]),
]
CT_CONSISTENCY = [
    (1, 1, []),
    (1, 8, ["96a296d224f285c67bee93c30f8a309157f0daa35dc5b87e410b78630a09cfc7",
            "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
            "6b47aaf29ee3c2af9af889bc1fb9254dabd31177f16232dd6aab035ca39bf6e4"]),
    (6, 8, ["0ebc5d3437fbe2db158b9f126a1d118e308181031d0a949f8dededebc558ef6a",
            "ca854ea128ed050b41b35ffc1b87b8eb2bde461e9e3b5596ece6b9d5975a0ae0",
            "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7"]),
    (2, 5, ["5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
            "bc1a0643b12e4d2d7c77918f44e0f4f79a838b6cf9ec5b5c283e1f4d88599e6b"]),
]

# The example of Go's golang.org/x/mod/sumdb/note.
PETER_SKEY = "PRIVATE+KEY+PeterNeumann+c74f20a3+AYEKFALVFGyNhPJEMzD1QIDr+Y7hfZx09iUvxdXHKDFz"
PETER_VKEY = "PeterNeumann+c74f20a3+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW"
PETER_TEXT = "If you think cryptography is the answer to your problem,\nthen you don't know what your problem is.\n"
PETER_NOTE = PETER_TEXT + "\n— PeterNeumann x08go/ZJkuBS9UG/SffcvIAQxVBtiFupLLr8pAcElZInNIuGUgYN1FFYC2pZSNXgKvqfqdngotpRZb6KE6RyyBwJnAM=\n"

ORIGIN = "hushrun.example/l/acme/back"
LOG_SEED = bytes(range(32))
# What Go's note.Sign wrote for the checkpoint of the eight CT leaves, with LOG_SEED as the key.
GO_CHECKPOINT_8 = (
    "hushrun.example/l/acme/back\n8\nXcnaeacGWamtVZy3Ad7ZoqudgjqtL0lgz+Nw7/RgQyg=\n\n"
    "— hushrun.example/l/acme/back Izr53CsW6ohjtGUay+jQtt4s9Czq1Y2/1wqKjG3imBNHaSBt5lv251xoTU6o0H2Cr62c+5u+F7jXN1qN5rwf2PM7jwc=\n"
)
GO_LOG_VKEY = "hushrun.example/l/acme/back+233af9dc+AQOhB7/zzhC+HXDdGOdLwJln5NYwm6UNXx3chmQSVTG4"



def map_fixture():
    """Prefixes with keys, as a ledger's map holds them."""
    entries = {}
    for i, c in enumerate("ab05f"):
        prefix = "v1/events/api/%s/" % (c * 64)
        entries[prefix] = ["%s01JA%022d.check._.json" % (prefix, n) for n in range(i + 1)]
    entries["v1/deployed/api/production/"] = [
        "v1/deployed/api/production/%013d.01JA%022d.json" % (9999999999999 - n, n) for n in range(5)
    ]
    entries["v1/deployed/api/production/"].sort()
    return entries


def ct_leaf_hashes():
    return [leaf_hash(bytes.fromhex(x)) for x in CT_LEAVES]


def selftest():
    leaves = ct_leaf_hashes()
    for n in range(1, 9):
        assert mth(leaves[:n]).hex() == CT_ROOTS[n - 1], n
    for index, size, expected in CT_INCLUSION:
        assert [h.hex() for h in path(index, leaves[:size])] == expected, (index, size)
    for old, new, expected in CT_CONSISTENCY:
        assert [h.hex() for h in proof(old, leaves[:new])] == expected, (old, new)
    seed = base64.b64decode(PETER_SKEY.split("+", 4)[4])[1:]  # the base64 may hold a +
    assert skey("PeterNeumann", seed) == PETER_SKEY
    public = secret_to_public(seed)
    assert vkey("PeterNeumann", public) == PETER_VKEY
    assert sign_note(PETER_TEXT, "PeterNeumann", seed) == PETER_NOTE
    assert open_note(PETER_NOTE, "PeterNeumann", public) == PETER_TEXT
    log_public = secret_to_public(LOG_SEED)
    assert vkey(ORIGIN, log_public) == GO_LOG_VKEY
    text = checkpoint_text(ORIGIN, 8, mth(leaves))
    assert sign_note(text, ORIGIN, LOG_SEED) == GO_CHECKPOINT_8
    # Range proofs agree with the tree they come from, and with nothing else.
    for size in range(0, 9):
        for begin in range(0, size + 1):
            for end in range(begin, size + 1):
                left, right = range_proof(leaves[:size], begin, end)
                assert root_from_range(size, begin, leaves[begin:end], left, right) == mth(leaves[:size])
    # The map's proofs, from the definitions.
    entries = map_fixture()
    root = map_root(entries)
    for prefix in list(entries) + ["v1/events/api/%s/" % (c * 64) for c in "0123456789"]:
        siblings, leaf = map_proof(entries, prefix)
        assert verify_map_proof(prefix, siblings, leaf, root)
        assert (leaf is not None and leaf["prefix"] == prefix) == (prefix in entries)
    print("ok: RFC 6962 roots and proofs, range proofs, the prefix map, Go's notes and checkpoint")


def vectors():
    leaves = ct_leaf_hashes()
    inclusion = []
    consistency = []
    for size in range(1, 9):
        for index in range(size):
            inclusion.append({"index": index, "size": size, "proof": [h.hex() for h in path(index, leaves[:size])]})
        for old in range(0, size + 1):
            consistency.append({"old": old, "new": size, "proof": [h.hex() for h in proof(old, leaves[:size])]})
    log_public = secret_to_public(LOG_SEED)
    root8 = mth(leaves)
    keys = [
        "v1/events/api/%s/01JA0000000000000000000001.started.staging.json" % ("ab" * 32),
        "v1/events/api/%s/01JA0000000000000000000002.failed.staging.json" % ("ab" * 32),
    ]
    statement = listing_text(ORIGIN, 8, root8, "v1/events/api/%s/" % ("ab" * 32), None, None, keys)
    paged = listing_text(ORIGIN, 8, root8, "v1/deployed/api/production/", "v1/deployed/api/production/a", "v1/deployed/api/production/b", ["v1/deployed/api/production/b"])
    entries = []
    for kind, name, data, gc in [
        ("object", keys[0], b"started", None),
        ("config", "trust.dsse.json", b"{}", None),
        ("tombstone", keys[1], b"failed", sha256(b"a GC statement")),
        ("tombstone", keys[1], b"failed", None),
    ]:
        encoded = entry(kind, name, sha256(data), gc)
        entries.append({
            "kind": kind, "name": name, "sha256": sha256(data).hex(),
            "gc": gc.hex() if gc else None,
            "encoded": encoded.decode(), "leaf": leaf_hash(encoded).hex(),
        })
    entries.append({
        "kind": "map", "name": "", "sha256": sha256(b"a map").hex(), "gc": None,
        "encoded": entry("map", "", sha256(b"a map")).decode(),
        "leaf": leaf_hash(entry("map", "", sha256(b"a map"))).hex(),
    })
    fixture = map_fixture()
    root = map_root(fixture)
    proofs = []
    for prefix in sorted(fixture) + ["v1/events/api/%s/" % ("9" * 64), "v1/deployed/web/staging/"]:
        siblings, leaf = map_proof(fixture, prefix)
        proofs.append({"prefix": prefix, "path": [h.hex() for h in siblings], "leaf": leaf})
    deployed = fixture["v1/deployed/api/production/"]
    deployed_leaves = [key_leaf(k) for k in deployed]
    pages = []
    for begin, end in [(0, 5), (0, 2), (1, 4), (3, 5), (4, 4), (5, 5)]:
        left, right = range_proof(deployed_leaves, begin, end)
        pages.append({"begin": begin, "end": end, "left": [h.hex() for h in left], "right": [h.hex() for h in right]})
    return {
        "comment": "Generated by tools/reftlog.py vectors; do not edit by hand.",
        "rfc6962": {
            "leaves": CT_LEAVES,
            "leaf_hashes": [h.hex() for h in leaves],
            "roots": [mth(leaves[:n]).hex() for n in range(0, 9)],
            "inclusion": inclusion,
            "consistency": consistency,
        },
        "notes": {
            "peter": {"skey": PETER_SKEY, "vkey": PETER_VKEY, "text": PETER_TEXT, "note": PETER_NOTE},
            "log": {
                "origin": ORIGIN,
                "seed": LOG_SEED.hex(),
                "public": base64.b64encode(log_public).decode(),
                "vkey": vkey(ORIGIN, log_public),
                "skey": skey(ORIGIN, LOG_SEED),
                "checkpoint": sign_note(checkpoint_text(ORIGIN, 8, root8), ORIGIN, LOG_SEED),
                "listing": sign_note(statement, ORIGIN, LOG_SEED),
                "listing_keys": keys,
                "paged_listing": sign_note(paged, ORIGIN, LOG_SEED),
            },
        },
        "entries": entries,
        "map": {
            "entries": [
                {"prefix": p, "keys": k, "text": map_entry_text(p, k), "leaf": leaf_hash(map_entry_text(p, k).encode()).hex()}
                for p, k in sorted(fixture.items())
            ],
            "root": root.hex(),
            "proofs": proofs,
            "key_tree": {
                "prefix": "v1/deployed/api/production/",
                "keys": deployed,
                "root": mth(deployed_leaves).hex(),
                "pages": pages,
            },
        },
    }


def main(argv):
    if argv[1:] == ["selftest"]:
        selftest()
    elif argv[1:] == ["vectors"]:
        print(json.dumps(vectors(), indent=2, ensure_ascii=False))
    elif len(argv) == 3 and argv[1] == "check":
        with open(argv[2], encoding="utf-8") as f:
            stored = json.load(f)
        if stored != vectors():
            print("%s differs from what reftlog.py computes; regenerate it with `vectors`" % argv[2])
            return 1
        print("ok: %s" % argv[2])
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
