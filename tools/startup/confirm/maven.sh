#!/bin/sh
# Confirms the `maven` part's reader (docs/spec/11-baked-packages.md, K-10): a local repository
# filled online is read as a read-only tail through MAVEN_OPTS, by an empty repository, offline.
# Needs mvn 3.9 or later and a route to Maven Central the first time. Prints one line per check.
set -eu
work=$(mktemp -d)
trap 'chmod -R u+w "$work"; rm -rf "$work"' EXIT
mkdir -p "$work/app/src/test/java/t"
cat > "$work/app/pom.xml" <<'POM'
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>t</groupId><artifactId>app</artifactId><version>1</version>
  <properties><maven.compiler.release>17</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding></properties>
  <dependencies>
    <dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.13.2</version>
      <scope>test</scope></dependency>
  </dependencies>
</project>
POM
cat > "$work/app/src/test/java/t/AppTest.java" <<'JAVA'
package t;
public class AppTest { @org.junit.Test public void ok() { org.junit.Assert.assertTrue(true); } }
JAVA
cd "$work/app"
# 1. The bake's fetch: the build online, its repository at a folder of its own.
MAVEN_OPTS="-Dmaven.repo.local=$work/part/repository" mvn -q -B test >/dev/null
echo "fetch: $(find "$work/part/repository" -type f | wc -l) files"
# 2. The reader: the part read-only, an empty repository, offline, the tail through MAVEN_OPTS.
chmod -R a-w "$work/part"
rm -rf target
MAVEN_OPTS="-Dmaven.repo.local=$work/empty -Dmaven.repo.local.tail=$work/part/repository" \
  mvn -o -q -B test >/dev/null && echo "tail through MAVEN_OPTS, offline, read-only: passes"
echo "the empty repository gained $(find "$work/empty" -type f 2>/dev/null | wc -l) files"
# 3. Without the tail the same offline build fails: the tail is what it read.
rm -rf "$work/empty2" target
if MAVEN_OPTS="-Dmaven.repo.local=$work/empty2" mvn -o -q -B test >/dev/null 2>&1; then
  echo "without the tail: passes (the check proves nothing)"; exit 1
fi
echo "without the tail, offline: fails, as it should"
# 4. With ./mvnw, when the wrapper can be generated here.
if mvn -q -B wrapper:wrapper -Dmaven.repo.local="$work/wrap" >/dev/null 2>&1; then
  rm -rf "$work/empty3" target
  MAVEN_OPTS="-Dmaven.repo.local=$work/empty3 -Dmaven.repo.local.tail=$work/part/repository" \
    ./mvnw -o -q -B test >/dev/null && echo "./mvnw reads the tail through MAVEN_OPTS: passes"
else
  echo "./mvnw: not confirmed (the wrapper couldn't be generated here)"
fi
# 5. One byte changed in a jar of the tail: does Maven notice?
jar=$(find "$work/part/repository/junit" -name '*.jar' | head -1)
chmod u+w "$jar" "$(dirname "$jar")"; printf 'X' | dd of="$jar" bs=1 seek=100 conv=notrunc 2>/dev/null
rm -rf "$work/empty4" target
if MAVEN_OPTS="-Dmaven.repo.local=$work/empty4 -Dmaven.repo.local.tail=$work/part/repository" \
  mvn -o -q -B test >/dev/null 2>&1; then
  echo "one byte changed: not caught by Maven (the part is verified by signature)"
else
  echo "one byte changed: the build failed reading the broken jar; Maven checked no checksum of it"
fi
