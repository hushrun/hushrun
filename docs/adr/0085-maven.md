# 0085. Maven's test classes, and two jars built here

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** C-47; changes C-32 and C-39; relies on C-30, C-31, C-33, C-34, C-38, ADR 0068, 0069,
  0070, 0071, 0075, 0077, RFC 0004

## Context

RFC 0004 adds the test as a unit beside the test file, and an adapter for each runner it names.
This decision is Maven's: Surefire and Failsafe, for Java, Kotlin, Groovy and Scala, with a test
class as the test file and a method as the test. RFC 0004 §4.6 planned it on an excludes file named
in `MAVEN_ARGS`, the reports Surefire writes, and "every source" as what a class read. A spike on
Maven 3.8.9, 3.9.16, 3.10.0 and 4.0.0-rc-7, with Surefire from 2.12.4 to 3.6.1, found that none of
the three holds as planned:

- `MAVEN_ARGS` doesn't reach Maven 3.8 nor the Maven wrapper's jar launcher; `MAVEN_OPTS` reaches
  all six launchers, as system properties that plugins read as their user properties.
- Surefire's excludes file can't leave out one method soundly: a method line fails the build on
  Surefire 2.22 and 3.0.0-M1 to M5, costs seconds per thousand tests (12 s at 5,000 lines), and any
  excludes file drops Surefire's default exclusion of nested classes, so that classes nobody ran
  before start running, and with forks a nested class overwrites its outer class's report. A POM's
  own excludes file, or `-Dtest` on the command line, makes Surefire ignore hushrun's entirely.
- Reading POMs for the module graph needs parent resolution, interpolation and profiles: not
  sound. Maven's own model is, and a core extension sees it before any plugin runs.
- Reports alone can't tell a class left out from one that never ran, a stale report from a fresh
  one after a crash, or a TestNG test skipped because its class's setup failed from one disabled.

So this adapter loads through `MAVEN_OPTS`, leaves tests out from inside the test JVM, and reads
the reactor from Maven itself. That needs code that runs in a JVM, which this repository didn't
have.

## Decision

1. **Two jars, built here and checked in.** hushrun embeds two small jars, each written to a
   directory of its own, `<cache dir>/adapters/jvm-<first 16 hex of its SHA-256>/`, when a run
   needs them:
   - **the test-JVM hook** (`crates/hushrun/jvm/test-jvm-hook/`, about 20 KB): a JUnit Platform
     filter after discovery and a TestNG method interceptor that leave out exactly the classes and
     methods a list names, and a JUnit Platform listener that writes each test's events, flushed a
     line at a time. It has nothing of Maven, so Gradle's adapter can load it too. It finds its list
     and its events directory by a system property, else a variable, else below `HUSHRUN_UNITS`
     (C-47 says which);
   - **the Maven core extension** (`crates/hushrun/jvm/maven-extension/`, about 25 KB): it writes
     the reactor as Maven built it, and turns `hushrun.json` into the hook's list.

   They are a new kind of thing here: built artifacts in the repository. They are our own code
   only. `cargo xtask jars` compiles their sources with javac 21.0.11 and `--release 8`, so they
   load in any JVM from Java 8 on, against API jars fetched from Maven Central by exact version and
   checked against SHA-256s pinned in `xtask/src/jars.rs` (JUnit Platform 1.9.3, TestNG 7.5.1,
   Maven 3.6.3, Plexus Utils 3.2.1, `javax.inject` 1). Those API jars are compile-time only: nothing
   of them goes into a jar, and nothing but the jars' own bytes goes into the binary, so no crate
   and no notice changes. The jar is a stored zip that the xtask writes itself, its entries in byte
   order after the manifest, every timestamp 1980-01-01 00:00, so its bytes depend only on the
   sources and the compiler. Anyone verifies it the way CI does: `cargo xtask jars --check` with
   JDK 21.0.11 builds both again and fails when a checked-in jar isn't byte for byte what its
   sources build. CI's `jars` job runs it with Temurin 21.0.11.
2. **Loading, with COMMAND as written.** `MAVEN_OPTS` gains `-Dmaven.ext.class.path` and
   `-Dmaven.test.additionalClasspath`, last, so that they win as a property given twice does, each
   joined to a value `MAVEN_OPTS` or `.mvn/jvm.config` already gave it. A command line's, a
   `.mvn/maven.config`'s or a POM's value wins over ours: then the hook or the extension isn't
   there, and the run says what that costs (everything runs, or nothing is recorded). `MAVEN_OPTS`
   is split at spaces by the `mvn` scripts, so a cache path with a space loads nothing, said once.
3. **A unit is a test class** (C-30), named by its source file when exactly one file below its
   module's test roots is `pkg/Outer.<kind>`, else by its module's directory and its binary name
   (`app::pkg.Outer`): Kotlin names files freely and puts several classes in one. A nested class is
   its outer class's. **A test is a method** (C-39), by name, after its nested class's path: the
   hook leaves out by class and name, so overloads and a parametrized method's invocations are one
   test, as they are one entry. Ids are the same from events and from reports, which never give the
   parameters in one form.
4. **What a class read** is every file below its module's source roots and those of each module of
   the reactor its module depends on (test roots too through a test-jar dependency), from Maven's
   own model: kept as directories, so a new file there changes the key. The sources are the files
   of the four kinds below a directory some unit read; everything else, POMs and resources
   included, is the rest, and a change to it runs everything. Below a module's roots there is no
   finer list: `reads: test` runs as `reads: file`, and a change to one test class runs every class
   of its module. With `-pl` and no `-am`, the module's tests read its dependencies' jars from the
   local repository, which no file here describes: `not observed`.
5. **Outcomes come from the hook's events where it loaded,** and from the reports this run wrote
   elsewhere, under rules that each stop a false pass (C-47): only reports Maven wrote after the
   extension ran and that changed since, only directly in a reports directory, by `classname`,
   never by counts, a class failure fails the class and finishes none of its tests, a report that
   doesn't read voids its directory, a skipped test passes only in a class where nothing failed, a
   started test that never finished finishes nothing of its class. Absence never means a pass.
6. **Where the hook doesn't load, nothing is left out,** and every test runs: Surefire before
   3.0.0-M4, JUnit 4 and TestNG on Surefire's own providers before 3.6 (TestNG's interceptor still
   leaves tests out inside TestNG's provider, and says which), a POM that sets its own additional
   classpath. Leaving classes out there through an excludes file was not built: it can't be made
   exact (2.12.4 has no such parameter, a POM's own file or `-Dtest` silently replace it, and
   re-adding the nested-class default depends on settings the extension would have to model). A run
   whose plan listed classes and where the hook loaded nowhere says so once.
7. **Which COMMANDs.** `mvn` and `mvnw` with any goals and options. Not offered, said once: `mvnd`,
   whose daemon keeps its own JVM and options; `-DskipTests` and `-Dmaven.test.skip`; a command line
   that sets `maven.ext.class.path`. `-Dtest` works: the hook still applies, and the key holds
   COMMAND. A module that sets `failIfNoTests` keeps one listed class running, so that leaving the
   rest out never fails its build.
8. **The plan reads files only:** the map's units and directories, else the hints', addressed by
   every file of the four kinds, as Vitest's are (ADR 0075), so `hushrun ready` can make it. A first
   run here, with no hint, reuses nothing.

## Amendment 1: a skip earns no pass where something failed (2026-11-12)

Item 5 let a skipped test pass in a class where nothing failed. A reviewer showed TestNG, after
another class's `@BeforeSuite` failed, skipping every test of the suite: the reports held the
failure under that class only, the skipped classes were recorded as passed, and the next run left
out a test that fails. The decision: each skipped test, and its class, is told `skipped` (ADR 0077,
Amendment 2), a class Surefire's report says was skipped whole too, so a run whose COMMAND failed
records nothing of them; and a skipped test finishes only when nothing of the run failed, else its
class didn't run whole either.

## Amendment 2: what a class read, and the hook's list, matched exactly (2026-11-12)

Item 4 followed Maven's reactor graph, which holds no edge for the annotation processors a
compiler names in `annotationProcessorPaths`: a change to a processor module with tests of its own
changed no key, and the tests of the code it generates were reused. The decision: a module's
upstream also holds the reactor projects its plugins depend on and those its compiler names as
processors (on the plugin or in any execution), and theirs, with dependencies of every scope and
its parent; one this build doesn't build is `upstream-not-built`, as before.

Item 3 said a test's id is its method's name up to its first `[`, and the hook compared names so:
Spock features and parametrized display names can hold one. The decision: a test's id is its
method's whole name, but a JUnit Vintage test's, whose Parameterized invocation is `name[…]`, and
a report's (which gives the parameters in no one form). The hook matches whole names too, `name[…]`
only under the Vintage engine, and tells each test it leaves out with the entry that listed it.
TestNG's interceptor matches the class a method runs as a test of, its instance's: a method
inherited from a concrete base class is a test of each subclass, which the base class's entry no
longer leaves out, and a subclass's entry does. The adapter holds the events to the list of each
module: a test left out that no entry names exactly, or an entry neither left out nor seen run in
a class that ran, makes the run start COMMAND once more with nothing left out (ADR 0077,
Amendment 4).

## Amendment 3: starts, and forks that die (2026-11-12)

The extension's file names its start, `start` (ADR 0077, Amendment 1): every fork and module of one
Maven build is one start. Two files, a script that starts Maven twice, still record nothing, said
once. Within the start a test finished only when every JVM that reported it finished it, and a
class or test one of them left open is open. A rerun that passed after a failure
(`rerunFailingTestsCount`) is a failure, from events and reports alike: a flaky test is a failed
one. A fork that dies in a class's teardown writes no report of that class, and its events leave
it open: the class records nothing, which a test checks.

Known, and left as it is: Maven names a test `<method>` under a unit that is its class, and Gradle
`<class>.<method>` under its source file, so the same test has another id under each adapter (ADR
0084, Amendment 5).

## Amendment 4: a JVM file no module owns is the rest (2026-11-12)

The rule Gradle's adapter gained (ADR 0084, Amendment 6) holds here already, and tests now show
it: a module is read as its source roots, from Maven's model, so a file of a JVM kind in none of
them (a module the reactor leaves out, one in a profile that isn't active, a tool beside a module's
sources) is below no directory a unit read, and is the rest; a change to one runs everything. Only
the build knows which files it compiles, and a file it doesn't may reach a test by a road hushrun
doesn't see, such as a jar in the local repository. With `-pl app -am`, the modules app depends on
keep their sources on app's classes' lists.

## Amendment 5: test-jars handed on, and modules a plugin names (2026-11-12)

Item 4 added an upstream module's test roots only when the module itself declared that module's
test-jar. A module that takes another's test-jar at compile scope hands those test classes on to
every module that depends on it, and a test there was reused after a test class it reads changed.
The decision: the test-jar dependencies (type `test-jar` or classifier `tests`, of any scope) of
the module and of every module it depends on, to the end, add those modules' test roots.

## Consequences

- A Maven repository reuses test classes, and with `unit: test` single tests, on Maven 3.8 to 4.0
  and Surefire 3.0.0-M4 and later for JUnit 5, 3.6 and later for JUnit 4.
- The set of sources depends on which modules' tests ran: the first run after that set changes
  (a module's tests running for the first time, or not at all) runs everything once.
- A change below a module's roots runs every class of that module and of the modules that depend
  on it: Maven gives no finer graph than modules.
- The repository now holds two built jars and their sources; a change to the sources needs a
  rebuild with JDK 21.0.11 in the same commit, which CI enforces.

## Alternatives considered

- **Surefire's excludes file for methods:** fails or is ignored on too many versions, costs seconds
  at scale, and changes which nested classes run.
- **`-Dtest` from `MAVEN_OPTS`:** replaces the project's own includes and excludes, and runs every
  class.
- **Reading POMs:** needs Maven's model builder to be right.
- **Appending a goal that prints the graph:** changes COMMAND's work, and doesn't reach Maven 3.8.
- **An agent or a bytecode change:** more than a filter and a listener need, and RFC 0004 §6's
  `reads: test` for the JVM would still need what each test ran.
