#!/bin/sh
# verify.sh: the secret scanners on an export of this repository (ADR 0064), such as
# `git archive HEAD | tar -x -C DIR`: never a working tree, whose build output and untracked files
# aren't what is pushed.
#
#   sh scripts/verify.sh DIR
#
# gitleaks must find nothing beyond gitleaks-baseline.json, the test fixtures reviewed on
# 2026-11-09 (fake tokens, key stubs, testdata/keys, the C2SP test vector key). trufflehog must
# verify nothing. A finding exits 1; review it, then fix the source or, for a new fixture, update
# the baseline from inside DIR with `gitleaks dir . --redact -f json -r <this repository>/scripts/gitleaks-baseline.json`.
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: sh scripts/verify.sh DIR" >&2
  exit 2
fi
dir=$1
here=$(cd "$(dirname "$0")" && pwd)
for tool in gitleaks trufflehog; do
  command -v "$tool" >/dev/null || { echo "verify.sh: $tool isn't installed (brew install $tool)" >&2; exit 2; }
done

status=0
# From inside DIR, so paths match the baseline's, which are relative to the tree's root.
if ! (cd "$dir" && gitleaks dir . --no-banner --redact --baseline-path "$here/gitleaks-baseline.json" \
  --log-level warn); then
  echo "verify.sh: gitleaks found something new in $dir" >&2
  status=1
fi
found=$(trufflehog filesystem "$dir" --no-update --json --results=verified 2>/dev/null | grep -c '"SourceMetadata"' || true)
if [ "$found" != 0 ]; then
  echo "verify.sh: trufflehog verified $found live secret(s) in $dir: run it without --json to see them" >&2
  status=1
fi
[ "$status" = 0 ] && echo "verify.sh: no secrets beyond the reviewed test fixtures"
exit "$status"
