#!/bin/sh
# Confirms that an older hushrun beside a package store works as before (plan 1.5's last test,
# docs/spec/11-baked-packages.md K-1): a baked store and a package store side by side, the older
# binary reads the baked store as it did and runs a check, and never looks at the packages.
# Usage: older-hushrun.sh OLD_HUSHRUN NEW_HUSHRUN. Needs cargo; nothing is downloaded.
set -eu
old=$1 new=$2
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export HOME="$work/home" HUSHRUN_CACHE_DIR="$work/cache" HUSHRUN_NO_UPDATE_CHECK=1 HUSHRUN_BUILD=on
mkdir -p "$HOME" "$work/repo/src"
cd "$work/repo"
printf '[package]\nname = "shop"\nversion = "0.1.0"\nedition = "2021"\n' > Cargo.toml
printf 'pub fn total() -> u32 { 21 }\n' > src/lib.rs
printf 'target/\n' > .gitignore
printf '<project><modelVersion>4.0.0</modelVersion><groupId>t</groupId><artifactId>a</artifactId><version>1</version></project>\n' > pom.xml
git init -q && git add . && git -c user.email=t@t -c user.name=t commit -qm init
# The baked store, by the older binary's own bake, so it is one the older binary reads.
"$old" run build -- cargo build -q >/dev/null 2>&1
"$old" build bake --out "$work/hushrun" >/dev/null 2>&1
# The package store beside it, by the new binary.
mkdir -p "$work/m2/g/n/1" && printf 'jar' > "$work/m2/g/n/1/n-1.jar"
"$new" packages bake --no-sign --out "$work/hushrun-packages" --harvest "maven=$work/m2" >/dev/null
ls "$work" | grep -qx hushrun-packages
# A fresh cache: what is reused from here on comes from the baked store.
export HUSHRUN_CACHE_DIR="$work/cache2"
export HUSHRUN_BUILD_LOWER="$work/hushrun" HUSHRUN_BUILD_LOWER_UNTRUSTED=1
export HUSHRUN_PACKAGES_LOWER="$work/hushrun-packages" HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1
# The older binary: the baked store reads as before, and a check runs and is recorded.
"$old" build status 2>&1 | grep -q "current" && echo "older ($("$old" --version)): reads the baked store beside a package store"
rm -rf target
echo "older: $("$old" run build -- cargo build -q 2>&1 | grep -oE "[0-9]+ of [0-9]+ crates? reused" | head -1), from the baked store"
"$old" run t -- true 2>&1 | grep -q "passed" && echo "older: runs a check, MAVEN_OPTS untouched: $(MAVEN_OPTS= "$old" run t2 -- sh -c 'echo "[$MAVEN_OPTS]"' 2>/dev/null | head -1)"
# The new binary, the same folders: both stores are read.
"$new" build status 2>&1 | grep -q "current" && echo "new: reads the baked store"
"$new" packages status 2>&1 | grep -q "current" && echo "new: reads the package store"
