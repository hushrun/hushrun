#!/bin/sh
# Confirms the `gradle` part's reader (docs/spec/11-baked-packages.md, K-10): caches/modules-2 of
# a Gradle user home filled online is read through GRADLE_RO_DEP_CACHE, read-only, by an empty
# user home, offline. Needs gradle 6.2 or later and a route to Maven Central the first time.
set -eu
work=$(mktemp -d)
trap 'chmod -R u+w "$work"; rm -rf "$work"' EXIT
mkdir -p "$work/app/src/test/java/t"
cat > "$work/app/settings.gradle" <<'G'
rootProject.name = 'app'
G
cat > "$work/app/build.gradle" <<'G'
plugins { id 'java' }
repositories { mavenCentral() }
dependencies { testImplementation 'junit:junit:4.13.2' }
G
cat > "$work/app/src/test/java/t/AppTest.java" <<'JAVA'
package t;
public class AppTest { @org.junit.Test public void ok() { org.junit.Assert.assertTrue(true); } }
JAVA
cd "$work/app"
# 1. The bake's fetch: the build online with a user home of its own; the part is modules-2.
GRADLE_USER_HOME="$work/home" gradle -q --no-daemon test >/dev/null
mkdir -p "$work/part"
cp -R "$work/home/caches/modules-2" "$work/part/modules-2"
find "$work/part" \( -name '*.lock' -o -name gc.properties \) -delete
echo "fetch: $(find "$work/part" -type f | wc -l) files"
chmod -R a-w "$work/part"
# 2. The reader: an empty user home, offline, the part read-only through GRADLE_RO_DEP_CACHE.
rm -rf build .gradle
GRADLE_USER_HOME="$work/empty" GRADLE_RO_DEP_CACHE="$work/part" \
  gradle -q --no-daemon --offline test >/dev/null && echo "GRADLE_RO_DEP_CACHE, offline, read-only: passes"
echo "the empty home's modules-2 gained $(find "$work/empty/caches/modules-2/files-2.1" -type f 2>/dev/null | wc -l) files"
# 3. Without it the same offline build fails.
rm -rf build .gradle
if GRADLE_USER_HOME="$work/empty2" gradle -q --no-daemon --offline test >/dev/null 2>&1; then
  echo "without the variable: passes (the check proves nothing)"; exit 1
fi
echo "without the variable, offline: fails, as it should"
# 4. A dependency the part lacks, online: downloaded into the writable home; the part untouched.
before=$(find "$work/part" -type f | sort | xargs sha256sum | sha256sum)
echo "dependencies { testImplementation 'org.hamcrest:hamcrest:2.2' }" >> build.gradle
rm -rf build .gradle
GRADLE_USER_HOME="$work/empty3" GRADLE_RO_DEP_CACHE="$work/part" gradle -q --no-daemon test >/dev/null
after=$(find "$work/part" -type f | sort | xargs sha256sum | sha256sum)
[ "$before" = "$after" ] && echo "one more dependency, online: passes, the part unchanged"
echo "the writable home gained $(find "$work/empty3/caches/modules-2/files-2.1" -type f | wc -l) files"
