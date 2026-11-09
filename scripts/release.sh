#!/usr/bin/env bash
# The CLI's next release (ADR 0064). From the repository's root.
#
#   scripts/release.sh due                           the version due, on stdout, or nothing
#   scripts/release.sh prepare <version> [--skip-ci] the version commit and its tag, here only
#
# A release is due when commits since the last tag touch what ships, and one of them says so in its
# Conventional Commits type: `fix` is a patch; `feat`, or `!` for a breaking change, a minor (the
# CLI is before 1.0). Other types release nothing, and go out with the next release that does.
#
# A maintainer pushes the commit and the tag together, and the tag starts release.yml, which
# starts publish.yml and npm.yml. `--skip-ci` puts `[skip ci]` in the commit's subject, so that
# neither push starts a workflow: for a release a maintainer built on their own machine.
set -euo pipefail

# What ships in a release: the binary, its packages and its container image.
SHIPPED=(crates Cargo.lock npm deploy)

type_re='^[a-z]+(\([^)]*\))?'

# Prints `minor`, `patch`, or nothing, for the commit subjects on stdin.
bump_of() {
  local subject bump=""
  while IFS= read -r subject; do
    if [[ "$subject" =~ $type_re!: ]] || [[ "$subject" =~ ^feat(\([^\)]*\))?: ]]; then
      bump="minor"
    elif [[ "$subject" =~ ^fix(\([^\)]*\))?: ]] && [ -z "$bump" ]; then
      bump="patch"
    fi
  done
  echo "$bump"
}

last_tag() { git tag --merged HEAD --list 'v[0-9]*' --sort=-v:refname | sed -n 1p; }

subjects_since() { git log --format=%s "$1..HEAD" -- "${SHIPPED[@]}"; }

# The version the commits since the last tag call for, or nothing.
due() {
  local last bump major minor patch
  last="$(last_tag)"
  [ -n "$last" ] || { echo "release.sh: this history has no version tag" >&2; return 1; }
  bump="$(subjects_since "$last" | bump_of)"
  [ -n "$bump" ] || return 0
  IFS=. read -r major minor patch <<<"${last#v}"
  case "$bump" in
    minor) echo "$major.$((minor + 1)).0" ;;
    patch) echo "$major.$minor.$((patch + 1))" ;;
  esac
}

# Moves [Unreleased]'s notes under the version, or writes them from the commits when there are none.
changelog() { # <version> <last tag>
  local notes
  notes="$(mktemp)"
  subjects_since "$2" >"$notes"
  python3 -I - "$1" "$(date -u +%F)" "$notes" <<'PY'
import re, sys

version, date, notes = sys.argv[1:4]
text = open("CHANGELOG.md").read()
head = "## [Unreleased]\n"
start = text.index(head) + len(head)
end = text.index("\n## [", start) + 1
body = text[start:end].strip()
if not body:
    sections = {"feat": [], "fix": []}
    for subject in open(notes).read().splitlines():
        match = re.match(r"(feat|fix)(\([^)]*\))?!?: (.+)", subject)
        if match:
            sections[match.group(1)].append("- " + match.group(3)[0].upper() + match.group(3)[1:] + ".")
    parts = []
    if sections["feat"]:
        parts.append("### Added\n\n" + "\n".join(sections["feat"]))
    if sections["fix"]:
        parts.append("### Fixed\n\n" + "\n".join(sections["fix"]))
    body = "\n\n".join(parts)
open("CHANGELOG.md", "w").write(
    text[:start] + f"\n## [{version}] - {date}\n\n{body}\n\n" + text[end:]
)
PY
  rm -f "$notes"
}

# The version commit and its tag.
prepare() { # <version> [--skip-ci]
  local version="${1:?a version}" suffix="" last
  [ "${2:-}" = --skip-ci ] && suffix=" [skip ci]"
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "release.sh: not a version: $version" >&2; return 1; }
  [ -z "$(git status --porcelain)" ] || { echo "release.sh: the work tree has changes" >&2; return 1; }
  last="$(last_tag)"
  # [workspace.package]'s version. hushrun-core, -attest, -ledger and -tlog keep their own, which
  # the hosted service builds with: a release changes nothing for it.
  sed -i.bak -E '/^\[workspace\.package\]$/,/^\[/ s/^version = ".*"$/version = "'"$version"'"/' Cargo.toml
  rm -f Cargo.toml.bak
  cargo metadata --format-version 1 >/dev/null
  (cd fuzz && cargo metadata --format-version 1 >/dev/null)
  changelog "$version" "$last"
  # The golden files of `hushrun adopt` name the version.
  HUSHRUN_BLESS=1 cargo test --locked -p hushrun --test adopt
  git add Cargo.toml Cargo.lock fuzz/Cargo.lock CHANGELOG.md testdata/adopt
  git commit -q -m "chore(release): $version$suffix"
  git tag "v$version"
  echo "prepared v$version: push the commit and the tag together"
}

case "${1:-}" in
  due) due ;;
  prepare) shift; prepare "$@" ;;
  *) echo "usage: $0 due | prepare <version> [--skip-ci]" >&2; exit 2 ;;
esac
